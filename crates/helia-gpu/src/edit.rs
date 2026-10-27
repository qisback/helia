// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Edit operations on the device: levels, curves, unsharp mask, a-trous
//! wavelets (and so denoise) and Richardson-Lucy deconvolution.
//!
//! `helia_core::edit` is the specification and every kernel here repeats its
//! arithmetic, so the two agree to float contraction (see the parity tests).
//! Geometry edits (crop, rotate, flip, resize) are not worth a round trip to
//! the device and run on the CPU between device operations.
//!
//! A run of device operations keeps the image on the device; the image goes
//! back to the host only for a geometry edit, for the one step of
//! deconvolution that builds the ringing guard (it needs the whole observed
//! image), and at the end.

#![allow(
    clippy::many_single_char_names,
    reason = "image maths uses w, h, n and the like"
)]

use helia_core::edit::deconv::{self, PsfKernel, RATIO_EPS, TV_EPS2, TV_FLOOR};
use helia_core::edit::flatten::State as FlattenState;
use helia_core::edit::wavelet::{self, Layer, MAD_TO_SIGMA, Wavelet};
use helia_core::edit::{Curve, Deconvolve, EditList, Levels, Op, Unsharp, apply_op};
use helia_core::limbdark::FlattenPlan;
use helia_core::{Error, Frame, Result};
use wgpu::{Buffer, BufferUsages, CommandEncoder, CommandEncoderDescriptor};

use crate::context::GpuContext;
use crate::kit::{Kernel, Res};

/// Threads per workgroup of the one-dimensional kernels.
const GROUP: usize = 256;
/// Workgroups per dispatch row; 2-D dispatch keeps both below the 65 535 limit.
const ROW_GROUPS: usize = 4096;
/// Edge of the square workgroups of the image kernels.
const TILE: usize = 16;

/// The compiled kernels.
pub(crate) struct EditKernels {
    levels: Kernel,
    flatten: Kernel,
    curve: Kernel,
    conv_h: Kernel,
    conv_v: Kernel,
    conv2d: Kernel,
    unsharp: Kernel,
    wavelet: Kernel,
    gather: Kernel,
    rl_ratio: Kernel,
    rl_update: Kernel,
    guard: Kernel,
}

/// An image in a device buffer.
struct Dev {
    buf: Buffer,
    w: usize,
    h: usize,
}

/// Where the image being edited is.
enum Cur {
    Host(Frame),
    Dev(Dev),
}

fn bits(v: f32) -> u32 {
    v.to_bits()
}

fn gpu_err(e: impl std::fmt::Display) -> Error {
    Error::Backend(e.to_string())
}

impl EditKernels {
    pub(crate) fn new(ctx: &GpuContext) -> Self {
        let k = |name: &str, src: &str| Kernel::new(ctx, name, src, "main");
        Self {
            levels: k("edit-levels", include_str!("shaders/edit_levels.wgsl")),
            flatten: k("edit-flatten", include_str!("shaders/edit_flatten.wgsl")),
            curve: k("edit-curve", include_str!("shaders/edit_curve.wgsl")),
            conv_h: k("edit-conv-h", include_str!("shaders/edit_conv_h.wgsl")),
            conv_v: k("edit-conv-v", include_str!("shaders/edit_conv_v.wgsl")),
            conv2d: k("edit-conv2d", include_str!("shaders/edit_conv2d.wgsl")),
            unsharp: k("edit-unsharp", include_str!("shaders/edit_unsharp.wgsl")),
            wavelet: k("edit-wavelet", include_str!("shaders/edit_wavelet.wgsl")),
            gather: k("edit-gather", include_str!("shaders/edit_gather.wgsl")),
            rl_ratio: k("edit-rl-ratio", include_str!("shaders/edit_rl_ratio.wgsl")),
            rl_update: k(
                "edit-rl-update",
                include_str!("shaders/edit_rl_update.wgsl"),
            ),
            guard: k("edit-guard", include_str!("shaders/edit_guard.wgsl")),
        }
    }

    /// Apply `list` to `frame`; `None` when an image would not fit one
    /// storage binding, in which case the caller uses the CPU.
    pub(crate) fn run(
        &self,
        ctx: &GpuContext,
        frame: &Frame,
        list: &EditList,
    ) -> Result<Option<Frame>> {
        list.validate(frame.width, frame.height)?;
        let list = list.resolve(frame);
        let mut dims = (frame.width, frame.height);
        let mut biggest = dims.0 * dims.1;
        for op in &list.ops {
            dims = op.output_size(dims.0, dims.1)?;
            biggest = biggest.max(dims.0 * dims.1);
        }
        if (biggest * 4) as u64 > ctx.max_binding() {
            return Ok(None);
        }
        let mut cur = Cur::Host(frame.clone());
        for op in &list.ops {
            if op.is_identity() {
                continue;
            }
            cur = match op {
                Op::Crop(_) | Op::Rotate(_) | Op::Flip(_) | Op::Resize(_) | Op::Prominence(_) => {
                    Cur::Host(apply_op(&into_host(ctx, cur)?, op)?)
                }
                Op::Deconvolve(d) => {
                    let host = into_host(ctx, cur)?;
                    Cur::Dev(self.deconvolve(ctx, &host, d)?)
                }
                other => {
                    let dev = into_dev(ctx, cur);
                    Cur::Dev(self.device_op(ctx, dev, other)?)
                }
            };
        }
        Ok(Some(into_host(ctx, cur)?))
    }

    fn device_op(&self, ctx: &GpuContext, d: Dev, op: &Op) -> Result<Dev> {
        match op {
            Op::Levels(l) => {
                self.levels(ctx, &d, l);
                Ok(d)
            }
            Op::Curve(c) => {
                self.curve(ctx, &d, c)?;
                Ok(d)
            }
            Op::Flatten(f) => {
                if let FlattenState::Ready(plan) = &f.state {
                    self.flatten(ctx, &d, plan);
                }
                Ok(d)
            }
            Op::Unsharp(u) => Ok(self.unsharp(ctx, &d, u)),
            Op::Wavelet(w) => self.wavelet(ctx, d, w),
            Op::Denoise(n) => self.wavelet(ctx, d, &Wavelet::from_layers(n.layers_for())),
            other => Err(Error::Unsupported(format!(
                "{} has no device kernel",
                other.name()
            ))),
        }
    }

    // ----- dispatch helpers -----

    /// One thread per sample. `extra` follows the leading `n` and row stride
    /// in the uniform block; `bufs` bind from 1.
    fn pixels(
        ctx: &GpuContext,
        kernel: &Kernel,
        enc: &mut CommandEncoder,
        n: usize,
        extra: &[u32],
        bufs: &[&Buffer],
    ) {
        let groups = n.div_ceil(GROUP);
        let gx = groups.clamp(1, ROW_GROUPS);
        let gy = groups.div_ceil(gx).max(1);
        let mut words = vec![n as u32, (gx * GROUP) as u32];
        words.extend_from_slice(extra);
        let params = ctx.uniform(&words);
        let mut res = vec![Res::All(&params)];
        res.extend(bufs.iter().map(|b| Res::All(b)));
        let bind = kernel.bind(ctx, &res);
        kernel.dispatch(enc, &bind, (gx as u32, gy as u32, 1));
    }

    /// One thread per pixel in square tiles; `words` is the whole uniform block.
    fn image(
        ctx: &GpuContext,
        kernel: &Kernel,
        enc: &mut CommandEncoder,
        w: usize,
        h: usize,
        words: &[u32],
        bufs: &[&Buffer],
    ) {
        let params = ctx.uniform(words);
        let mut res = vec![Res::All(&params)];
        res.extend(bufs.iter().map(|b| Res::All(b)));
        let bind = kernel.bind(ctx, &res);
        kernel.dispatch(
            enc,
            &bind,
            (w.div_ceil(TILE) as u32, h.div_ceil(TILE) as u32, 1),
        );
    }

    fn encoder(ctx: &GpuContext) -> CommandEncoder {
        ctx.device
            .create_command_encoder(&CommandEncoderDescriptor::default())
    }

    /// Separable convolution `src -> tmp -> dst` with `taps` `step` apart.
    #[allow(clippy::too_many_arguments, reason = "one dispatch description")]
    fn separable(
        &self,
        ctx: &GpuContext,
        enc: &mut CommandEncoder,
        taps: &Buffer,
        ntaps: usize,
        step: usize,
        (w, h): (usize, usize),
        (src, tmp, dst): (&Buffer, &Buffer, &Buffer),
    ) {
        let words = [w as u32, h as u32, ntaps as u32, step as u32];
        Self::image(ctx, &self.conv_h, enc, w, h, &words, &[taps, src, tmp]);
        Self::image(ctx, &self.conv_v, enc, w, h, &words, &[taps, tmp, dst]);
    }

    // ----- pointwise -----

    fn levels(&self, ctx: &GpuContext, d: &Dev, l: &Levels) {
        let mut enc = Self::encoder(ctx);
        let use_pow = (l.gamma - 1.0).abs() >= f32::EPSILON;
        Self::pixels(
            ctx,
            &self.levels,
            &mut enc,
            d.w * d.h,
            &[
                bits(l.black),
                bits(l.white - l.black),
                bits(1.0 / l.gamma),
                bits(l.out_black),
                bits(l.out_white - l.out_black),
                u32::from(use_pow),
            ],
            &[&d.buf],
        );
        ctx.queue.submit(Some(enc.finish()));
    }

    fn flatten(&self, ctx: &GpuContext, d: &Dev, plan: &FlattenPlan) {
        let gain = ctx.storage(
            "edit-flatten-gain",
            (plan.gain.len() * 4) as u64,
            BufferUsages::COPY_DST,
        );
        ctx.queue
            .write_buffer(&gain, 0, bytemuck::cast_slice(&plan.gain));
        let mut enc = Self::encoder(ctx);
        Self::pixels(
            ctx,
            &self.flatten,
            &mut enc,
            d.w * d.h,
            &[
                d.w as u32,
                (plan.gain.len() - 1) as u32,
                bits(plan.cx),
                bits(plan.cy),
                bits(1.0 / plan.radius),
                bits(plan.background),
            ],
            &[&gain, &d.buf],
        );
        ctx.queue.submit(Some(enc.finish()));
    }

    fn curve(&self, ctx: &GpuContext, d: &Dev, c: &Curve) -> Result<()> {
        let k = c.knots()?;
        let count = k.x.len();
        let table: Vec<f32> = k.x.iter().chain(&k.y).chain(&k.m).copied().collect();
        let knots = ctx.storage(
            "edit-knots",
            (table.len() * 4) as u64,
            BufferUsages::COPY_DST,
        );
        ctx.queue
            .write_buffer(&knots, 0, bytemuck::cast_slice(&table));
        let mut enc = Self::encoder(ctx);
        Self::pixels(
            ctx,
            &self.curve,
            &mut enc,
            d.w * d.h,
            &[count as u32, 0],
            &[&knots, &d.buf],
        );
        ctx.queue.submit(Some(enc.finish()));
        Ok(())
    }

    // ----- unsharp and wavelets -----

    fn taps(ctx: &GpuContext, taps: &[f32]) -> Buffer {
        let buf = ctx.storage("edit-taps", (taps.len() * 4) as u64, BufferUsages::COPY_DST);
        ctx.queue.write_buffer(&buf, 0, bytemuck::cast_slice(taps));
        buf
    }

    fn unsharp(&self, ctx: &GpuContext, d: &Dev, u: &Unsharp) -> Dev {
        let n = d.w * d.h;
        let taps = u.taps();
        let taps_buf = Self::taps(ctx, &taps);
        let tmp = image_buffer(ctx, "edit-tmp", n);
        let blur = image_buffer(ctx, "edit-blur", n);
        let out = image_buffer(ctx, "edit-unsharp", n);
        let mut enc = Self::encoder(ctx);
        self.separable(
            ctx,
            &mut enc,
            &taps_buf,
            taps.len(),
            1,
            (d.w, d.h),
            (&d.buf, &tmp, &blur),
        );
        Self::pixels(
            ctx,
            &self.unsharp,
            &mut enc,
            n,
            &[bits(u.amount), bits(u.threshold)],
            &[&d.buf, &blur, &out],
        );
        ctx.queue.submit(Some(enc.finish()));
        Dev {
            buf: out,
            w: d.w,
            h: d.h,
        }
    }

    /// The a-trous transform of `wavelet::transform_with`.
    fn wavelet(&self, ctx: &GpuContext, d: Dev, wv: &Wavelet) -> Result<Dev> {
        let (w, h) = (d.w, d.h);
        let n = w * h;
        let bytes = (n * 4) as u64;
        let filter = wv.filter.taps();
        let taps = Self::taps(ctx, &filter);
        let spacing = wv.spacings();
        let layers = &wv.layers;
        let acc = image_buffer(ctx, "edit-acc", n);
        let mut prev = d.buf;
        let mut cur = image_buffer(ctx, "edit-cur", n);
        let tmp = image_buffer(ctx, "edit-tmp", n);
        let mut enc = Self::encoder(ctx);
        enc.copy_buffer_to_buffer(&prev, 0, &acc, 0, bytes);
        // Linked: the running result is both the input and the output of a
        // layer, so it lives in `acc` and `prev` is its copy for the pass.
        for (j, layer) in layers.iter().enumerate() {
            let identity = layer.is_identity();
            if identity && (wv.linked || layers[j + 1..].iter().all(Layer::is_identity)) {
                if wv.linked {
                    continue;
                }
                break;
            }
            if wv.linked {
                enc.copy_buffer_to_buffer(&acc, 0, &prev, 0, bytes);
            }
            self.separable(
                ctx,
                &mut enc,
                &taps,
                filter.len(),
                spacing[j],
                (w, h),
                (&prev, &tmp, &cur),
            );
            if !identity {
                let mut t = 0.0f32;
                if layer.threshold() > 0.0 {
                    let stride = wavelet::mad_stride(w, h).max(1);
                    let nx = w.div_ceil(stride);
                    let count = nx * h.div_ceil(stride);
                    let samples =
                        ctx.storage("edit-samples", (count * 4) as u64, BufferUsages::COPY_SRC);
                    let params = ctx.uniform(&[w as u32, nx as u32, count as u32, stride as u32]);
                    let bind = self.gather.bind(
                        ctx,
                        &[
                            Res::All(&params),
                            Res::All(&prev),
                            Res::All(&cur),
                            Res::All(&samples),
                        ],
                    );
                    self.gather
                        .dispatch(&mut enc, &bind, (count.div_ceil(GROUP) as u32, 1, 1));
                    ctx.queue.submit(Some(enc.finish()));
                    enc = Self::encoder(ctx);
                    let mut v = ctx.read_f32(&samples, count).map_err(gpu_err)?;
                    t = layer.threshold() * (wavelet::median_in_place(&mut v) * MAD_TO_SIGMA);
                }
                Self::pixels(
                    ctx,
                    &self.wavelet,
                    &mut enc,
                    n,
                    &[bits(layer.gain()), bits(t)],
                    &[&prev, &cur, &acc],
                );
            }
            if !wv.linked {
                std::mem::swap(&mut prev, &mut cur);
            }
        }
        ctx.queue.submit(Some(enc.finish()));
        Ok(Dev { buf: acc, w, h })
    }

    // ----- deconvolution -----

    #[allow(clippy::too_many_lines, reason = "one iteration loop with its set-up")]
    fn deconvolve(&self, ctx: &GpuContext, frame: &Frame, p: &Deconvolve) -> Result<Dev> {
        p.validate()?;
        let (w, h) = (frame.width, frame.height);
        let n = w * h;
        let kernel = PsfKernel::new(p.psf)?;
        let obs_host = deconv::observed(frame);
        let upload = |label: &str, data: &[f32]| {
            let b = image_buffer(ctx, label, data.len());
            ctx.queue.write_buffer(&b, 0, bytemuck::cast_slice(data));
            b
        };
        let obs = upload("edit-obs", &obs_host);
        let mut est = upload("edit-est", &obs_host);
        let mut next = image_buffer(ctx, "edit-next", n);
        let (blur, ratio, corr, tmp) = (
            image_buffer(ctx, "edit-rl-blur", n),
            image_buffer(ctx, "edit-rl-ratio", n),
            image_buffer(ctx, "edit-rl-corr", n),
            image_buffer(ctx, "edit-rl-tmp", n),
        );
        let (kernel_buf, ntaps) = match &kernel.separable {
            Some(t) => (Self::taps(ctx, t), t.len()),
            None => (
                {
                    let b = ctx.storage(
                        "edit-psf",
                        (kernel.full.len() * 4) as u64,
                        BufferUsages::COPY_DST,
                    );
                    ctx.queue
                        .write_buffer(&b, 0, bytemuck::cast_slice(&kernel.full));
                    b
                },
                0,
            ),
        };
        let separable = kernel.separable.is_some();
        let convolve = |enc: &mut CommandEncoder, src: &Buffer, dst: &Buffer| {
            if separable {
                self.separable(ctx, enc, &kernel_buf, ntaps, 1, (w, h), (src, &tmp, dst));
            } else {
                Self::image(
                    ctx,
                    &self.conv2d,
                    enc,
                    w,
                    h,
                    &[w as u32, h as u32, kernel.radius as u32, 0],
                    &[&kernel_buf, src, dst],
                );
            }
        };
        let mut enc = Self::encoder(ctx);
        for _ in 0..p.iterations {
            convolve(&mut enc, &est, &blur);
            Self::pixels(
                ctx,
                &self.rl_ratio,
                &mut enc,
                n,
                &[bits(RATIO_EPS), 0],
                &[&obs, &blur, &ratio],
            );
            convolve(&mut enc, &ratio, &corr);
            Self::image(
                ctx,
                &self.rl_update,
                &mut enc,
                w,
                h,
                &[
                    w as u32,
                    h as u32,
                    bits(p.tv),
                    bits(TV_EPS2),
                    bits(TV_FLOOR),
                    0,
                    0,
                    0,
                ],
                &[&est, &corr, &next],
            );
            std::mem::swap(&mut est, &mut next);
        }
        if p.protect > 0.0 {
            let sigma = deconv::guard_sigma(p.psf)?;
            let guard = deconv::protection(&obs_host, w, h, sigma, p.protect);
            let (gw, glo, ghi) = (
                upload("edit-guard-weight", &guard.weight),
                upload("edit-guard-lo", &guard.lo),
                upload("edit-guard-hi", &guard.hi),
            );
            Self::pixels(
                ctx,
                &self.guard,
                &mut enc,
                n,
                &[0, 0],
                &[&gw, &glo, &ghi, &est],
            );
        }
        ctx.queue.submit(Some(enc.finish()));
        Ok(Dev { buf: est, w, h })
    }
}

/// Put the image on the device (non-finite samples become 0).
fn into_dev(ctx: &GpuContext, cur: Cur) -> Dev {
    match cur {
        Cur::Dev(d) => d,
        Cur::Host(f) => {
            let host: Vec<f32> = f
                .data
                .iter()
                .map(|&v| if v.is_finite() { v } else { 0.0 })
                .collect();
            let buf = image_buffer(ctx, "edit-image", f.data.len());
            ctx.queue.write_buffer(&buf, 0, bytemuck::cast_slice(&host));
            Dev {
                buf,
                w: f.width,
                h: f.height,
            }
        }
    }
}

/// Bring the image back to the host.
fn into_host(ctx: &GpuContext, cur: Cur) -> Result<Frame> {
    match cur {
        Cur::Host(f) => Ok(f),
        Cur::Dev(d) => {
            let data = ctx.read_f32(&d.buf, d.w * d.h).map_err(gpu_err)?;
            Ok(Frame {
                width: d.w,
                height: d.h,
                data,
                bayer: None,
            })
        }
    }
}

/// A storage buffer for `n` samples that can be uploaded to and read back.
fn image_buffer(ctx: &GpuContext, label: &str, n: usize) -> Buffer {
    ctx.storage(
        label,
        (n * 4) as u64,
        BufferUsages::COPY_SRC | BufferUsages::COPY_DST,
    )
}
