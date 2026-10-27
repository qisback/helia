// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Global registration on the device: the phase correlation of
//! [`helia_core::align::global::PhaseCorrelator`] for a batch of frames at a
//! time.
//!
//! The frame is windowed and transformed along rows (real input, the
//! non-negative half of the spectrum kept), then along columns; the
//! cross-power spectrum with the reference's is whitened and band-passed, and
//! transformed back (columns, then rows) to the correlation surface. The
//! integer peak and the sub-pixel refinement (direct evaluation of the
//! spectrum around the estimate) also run on the device, so only the final
//! shifts come back. Sizes, windows and filters come from the CPU correlator,
//! so the two sides use the same grid.

#![allow(
    clippy::many_single_char_names,
    reason = "x, y, w, h and n are the natural names in image and transform maths"
)]

use std::time::Instant;

use std::sync::Arc;

use helia_core::align::Shift;
use helia_core::align::global::PhaseCorrelator;
use helia_core::profile::Category;
use helia_core::{RegisterRequest, Result};
use wgpu::{BindGroup, Buffer, BufferUsages};

use crate::context::GpuContext;
use crate::decode::{BatchPlan, DecodeSet, Decoder};
use crate::kit::{Kernel, RawBatch, Res, for_each_raw_batch, trace};
use crate::resident::Resident;

/// Longest transform the workgroup memory holds.
const MAX_LEN: usize = 2048;
/// Workgroups per frame for the partial sums of the mean.
const MEAN_GROUPS: usize = 128;
const FFT: &str = include_str!("shaders/fft_common.wgsl");

/// Compiled registration kernels.
pub(crate) struct RegisterKernels {
    mean: Kernel,
    mean_final: Kernel,
    rows_fwd: Kernel,
    cols: Kernel,
    cross: Kernel,
    whiten: Kernel,
    rows_inv: Kernel,
    peak: Kernel,
    refine: Kernel,
    refine_update: Kernel,
}

/// The mixed-radix plan of a transform length as the shader's `Plan` words,
/// or `None` when it needs more stages than the shader holds or does not fit
/// workgroup memory.
fn plan_words(n: usize) -> Option<[u32; 32]> {
    if n == 0 || n > MAX_LEN {
        return None;
    }
    let mut rest = n;
    let mut factors: Vec<u32> = Vec::new();
    for p in [5usize, 3] {
        while rest.is_multiple_of(p) {
            factors.push(p as u32);
            rest /= p;
        }
    }
    let mut twos = 0;
    while rest.is_multiple_of(2) {
        twos += 1;
        rest /= 2;
    }
    if rest != 1 {
        return None; // not 5-smooth
    }
    factors.extend(std::iter::repeat_n(4, twos / 2));
    if twos % 2 == 1 {
        factors.push(2);
    }
    if factors.len() > 12 {
        return None;
    }
    let mut w = [0u32; 32];
    w[0] = n as u32;
    w[1] = factors.len() as u32;
    let mut span = 1u32;
    w[16] = 1;
    for (i, &r) in factors.iter().enumerate() {
        w[4 + i] = r;
        span *= r;
        w[16 + i + 1] = span;
    }
    Some(w)
}

/// Device state of one registration run.
struct Work {
    set: DecodeSet,
    /// Words to rewrite per batch: (uniform, word index).
    frame_words: Vec<(Buffer, usize)>,
    binds: Binds,
    est: Buffer,
    staging: Buffer,
    ny: usize,
    nxh: usize,
    cross_groups: usize,
    refinements: usize,
}

struct Binds {
    mean: BindGroup,
    mean_final: BindGroup,
    rows_fwd: BindGroup,
    cols_fwd: BindGroup,
    cross: BindGroup,
    floors: BindGroup,
    whiten: BindGroup,
    cols_inv: BindGroup,
    rows_inv: BindGroup,
    peak: BindGroup,
    refine: BindGroup,
    refine_update: BindGroup,
}

impl RegisterKernels {
    pub(crate) fn new(ctx: &GpuContext) -> Self {
        let plain = |label: &str, wgsl: &str| Kernel::new(ctx, label, wgsl, "main");
        let fft =
            |label: &str, wgsl: &str| Kernel::new(ctx, label, &format!("{FFT}{wgsl}"), "main");
        Self {
            mean: plain("reg_mean", include_str!("shaders/reg_mean.wgsl")),
            mean_final: plain(
                "reg_mean_final",
                include_str!("shaders/reg_mean_final.wgsl"),
            ),
            rows_fwd: fft("reg_rows_fwd", include_str!("shaders/reg_rows_fwd.wgsl")),
            cols: fft("reg_cols", include_str!("shaders/reg_cols.wgsl")),
            cross: plain("reg_cross", include_str!("shaders/reg_cross.wgsl")),
            whiten: plain("reg_whiten", include_str!("shaders/reg_whiten.wgsl")),
            rows_inv: fft("reg_rows_inv", include_str!("shaders/reg_rows_inv.wgsl")),
            peak: plain("reg_peak", include_str!("shaders/reg_peak.wgsl")),
            refine: plain("reg_refine", include_str!("shaders/reg_refine.wgsl")),
            refine_update: plain(
                "reg_refine_update",
                include_str!("shaders/reg_refine_update.wgsl"),
            ),
        }
    }

    /// Buffers, uniforms and bindings; `None` when the transform sizes do not
    /// fit the kernels.
    #[allow(clippy::too_many_lines)] // one flat list of buffer and binding set-up
    fn work(&self, ctx: &GpuContext, dec: &Decoder, corr: &PhaseCorrelator) -> Option<Work> {
        let g = corr.geometry();
        let (w, h) = (dec.layout().width, dec.layout().height);
        if (g.width, g.height) != (w, h) {
            return None;
        }
        let (nx, ny) = (g.nx, g.ny);
        let nxh = nx / 2 + 1;
        let (plan_x, plan_y) = (plan_words(nx)?, plan_words(ny)?);
        let spec_floats = nxh * ny * 2;
        let cross_groups = (nxh * ny).div_ceil(256);
        // Two spectra and a surface per frame, and the decoded frame itself.
        let extra = 2 * spec_floats * 4 + nx * ny * 4 + (nxh * 5 + 8) * 4 + cross_groups * 4;
        let plan = BatchPlan::new(ctx, dec.layout(), extra, 1);
        let cap = plan.cap;
        let set = Decoder::set(ctx, plan);
        let upload = |label: &str, data: &[f32]| {
            let buf = ctx.storage(
                label,
                (data.len().max(4) * 4) as u64,
                BufferUsages::COPY_DST,
            );
            ctx.queue.write_buffer(&buf, 0, bytemuck::cast_slice(data));
            buf
        };
        let win_x = upload("win_x", g.win_x);
        let win_y = upload("win_y", g.win_y);
        let low_x = upload("low_x", g.low_x);
        let low_y = upload("low_y", g.low_y);
        let stop_x = upload("stop_x", g.stop_x);
        let stop_y = upload("stop_y", g.stop_y);
        let reference = upload("reference spectrum", &corr.reference_spectrum());

        let work_buf = |label: &str, floats: usize, extra: BufferUsages| {
            ctx.storage(label, (floats.max(4) * 4) as u64, extra)
        };
        let none = BufferUsages::empty();
        let mean_partials = work_buf("mean partials", cap * MEAN_GROUPS, none);
        let means = work_buf("means", cap, none);
        let spec_a = work_buf("spectrum a", cap * spec_floats, none);
        let spec_b = work_buf("spectrum b", cap * spec_floats, none);
        let surface = work_buf("surface", cap * nx * ny, none);
        let cross_partials = work_buf("cross partials", cap * cross_groups, none);
        let floors = work_buf("floors", cap, none);
        let est = work_buf("estimates", cap * 2, BufferUsages::COPY_SRC);
        let refine_partials = work_buf("refine partials", cap * nxh * 5, none);

        let u = |words: &[u32]| ctx.uniform(words);
        let mean_p = u(&[w as u32, h as u32, 0, MEAN_GROUPS as u32]);
        let mean_final_p = u(&[MEAN_GROUPS as u32, 0, (1.0 / (w * h) as f32).to_bits(), 0]);
        // Both plans in one buffer: the x plan at word 0, the y plan at word 32.
        let plans: Vec<u32> = plan_x.iter().chain(&plan_y).copied().collect();
        let plans_buf = ctx.storage(
            "fft plans",
            (plans.len() * 4) as u64,
            BufferUsages::COPY_DST,
        );
        ctx.queue
            .write_buffer(&plans_buf, 0, bytemuck::cast_slice(&plans));
        let rows_fwd_p = u(&[
            w as u32, h as u32, nx as u32, ny as u32, nxh as u32, 0, 0, 0, 0, 0, 0, 0,
        ]);
        let cols_fwd_p = u(&[ny as u32, nxh as u32, 0, 0, 32, 0, 0, 0]);
        let cols_inv_p = u(&[ny as u32, nxh as u32, 0, 1, 32, 0, 0, 0]);
        let cross_p = u(&[
            nx as u32,
            ny as u32,
            nxh as u32,
            0,
            cross_groups as u32,
            0,
            0,
            0,
        ]);
        let floors_p = u(&[
            cross_groups as u32,
            0,
            (g.whiten_eps / (nx * ny) as f32).to_bits(),
            0,
        ]);
        let whiten_p = u(&[nx as u32, ny as u32, nxh as u32, 0]);
        let rows_inv_p = u(&[nx as u32, ny as u32, nxh as u32, 0, 0, 0, 0, 0]);
        let peak_p = u(&[nx as u32, ny as u32, 0, 0]);
        let refine_p = u(&[nx as u32, ny as u32, nxh as u32, 0]);
        let update_p = u(&[nxh as u32, 0, 0, 0]);

        let binds = Binds {
            mean: self.mean.bind(
                ctx,
                &[
                    Res::All(&mean_p),
                    Res::All(&set.mono),
                    Res::All(&mean_partials),
                ],
            ),
            mean_final: self.mean_final.bind(
                ctx,
                &[
                    Res::All(&mean_final_p),
                    Res::All(&mean_partials),
                    Res::All(&means),
                ],
            ),
            rows_fwd: self.rows_fwd.bind(
                ctx,
                &[
                    Res::All(&rows_fwd_p),
                    Res::All(&set.mono),
                    Res::All(&means),
                    Res::All(&win_x),
                    Res::All(&win_y),
                    Res::All(&spec_a),
                    Res::All(&plans_buf),
                ],
            ),
            cols_fwd: self.cols.bind(
                ctx,
                &[
                    Res::All(&cols_fwd_p),
                    Res::All(&spec_a),
                    Res::All(&spec_b),
                    Res::All(&plans_buf),
                ],
            ),
            cross: self.cross.bind(
                ctx,
                &[
                    Res::All(&cross_p),
                    Res::All(&spec_b),
                    Res::All(&reference),
                    Res::All(&cross_partials),
                ],
            ),
            floors: self.mean_final.bind(
                ctx,
                &[
                    Res::All(&floors_p),
                    Res::All(&cross_partials),
                    Res::All(&floors),
                ],
            ),
            whiten: self.whiten.bind(
                ctx,
                &[
                    Res::All(&whiten_p),
                    Res::All(&spec_b),
                    Res::All(&floors),
                    Res::All(&low_x),
                    Res::All(&low_y),
                    Res::All(&stop_x),
                    Res::All(&stop_y),
                ],
            ),
            cols_inv: self.cols.bind(
                ctx,
                &[
                    Res::All(&cols_inv_p),
                    Res::All(&spec_b),
                    Res::All(&spec_a),
                    Res::All(&plans_buf),
                ],
            ),
            rows_inv: self.rows_inv.bind(
                ctx,
                &[
                    Res::All(&rows_inv_p),
                    Res::All(&spec_a),
                    Res::All(&surface),
                    Res::All(&plans_buf),
                ],
            ),
            peak: self.peak.bind(
                ctx,
                &[Res::All(&peak_p), Res::All(&surface), Res::All(&est)],
            ),
            refine: self.refine.bind(
                ctx,
                &[
                    Res::All(&refine_p),
                    Res::All(&spec_b),
                    Res::All(&est),
                    Res::All(&refine_partials),
                ],
            ),
            refine_update: self.refine_update.bind(
                ctx,
                &[
                    Res::All(&update_p),
                    Res::All(&refine_partials),
                    Res::All(&est),
                ],
            ),
        };
        // The frame count sits at these words of these uniforms.
        let frame_words = vec![
            (mean_p, 2),
            (mean_final_p, 1),
            (rows_fwd_p, 5),
            (cols_fwd_p, 2),
            (cols_inv_p, 2),
            (cross_p, 3),
            (floors_p, 1),
            (whiten_p, 3),
            (rows_inv_p, 3),
            (peak_p, 2),
            (refine_p, 3),
            (update_p, 1),
        ];
        Some(Work {
            set,
            frame_words,
            binds,
            est,
            staging: ctx.staging((cap * 2 * 4) as u64),
            ny,
            nxh,
            cross_groups,
            refinements: g.refine_iterations.max(1) - 1,
        })
    }

    /// Record and run one batch; returns the shifts of its frames.
    fn run_batch(
        &self,
        ctx: &GpuContext,
        dec: &Decoder,
        work: &Work,
        batch: &RawBatch,
    ) -> Result<Vec<Shift>> {
        let count = batch.len();
        for (buf, word) in &work.frame_words {
            ctx.queue
                .write_buffer(buf, (*word * 4) as u64, bytemuck::bytes_of(&(count as u32)));
        }
        let n = count as u32;
        let b = &work.binds;
        let mut enc = ctx
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        dec.upload_and_encode(ctx, &mut enc, &work.set, batch);
        self.mean
            .dispatch(&mut enc, &b.mean, (MEAN_GROUPS as u32, n, 1));
        self.mean_final.dispatch(&mut enc, &b.mean_final, (n, 1, 1));
        self.rows_fwd
            .dispatch(&mut enc, &b.rows_fwd, (work.ny as u32, n, 1));
        self.cols
            .dispatch(&mut enc, &b.cols_fwd, (work.nxh as u32, n, 1));
        let cg = work.cross_groups as u32;
        self.cross.dispatch(&mut enc, &b.cross, (cg, n, 1));
        self.mean_final.dispatch(&mut enc, &b.floors, (n, 1, 1));
        self.whiten.dispatch(&mut enc, &b.whiten, (cg, n, 1));
        self.cols
            .dispatch(&mut enc, &b.cols_inv, (work.nxh as u32, n, 1));
        self.rows_inv
            .dispatch(&mut enc, &b.rows_inv, (work.ny as u32, n, 1));
        self.peak.dispatch(&mut enc, &b.peak, (n, 1, 1));
        for _ in 0..work.refinements {
            self.refine
                .dispatch(&mut enc, &b.refine, (work.nxh as u32, n, 1));
            self.refine_update
                .dispatch(&mut enc, &b.refine_update, (n, 1, 1));
        }
        enc.copy_buffer_to_buffer(&work.est, 0, &work.staging, 0, (count * 8) as u64);
        trace::time("submit", || ctx.queue.submit(Some(enc.finish())));
        let est = trace::time("wait", || ctx.map_f32(&work.staging, count * 2))?;
        Ok(est
            .as_chunks::<2>()
            .0
            .iter()
            .map(|e| Shift::new(e[0], e[1]))
            .collect())
    }

    /// Register the requested frames; `None` when the sizes do not fit.
    pub(crate) fn run(
        &self,
        ctx: &GpuContext,
        req: &RegisterRequest<'_>,
        resident: Option<Arc<Resident>>,
    ) -> Result<Option<Vec<Shift>>> {
        let Some(dec) = Decoder::new(ctx, req.frames, req.debayer, resident) else {
            return Ok(None);
        };
        let corr = PhaseCorrelator::new(req.reference, req.phase)?;
        let Some(work) = self.work(ctx, &dec, &corr) else {
            return Ok(None);
        };
        let mut shifts: Vec<Shift> = Vec::with_capacity(req.indices.len());
        for_each_raw_batch(
            req.frames,
            req.indices,
            work.set.plan.cap,
            req.profile,
            req.cancel,
            dec.resident(),
            |_, batch| {
                let t = Instant::now();
                shifts.extend(self.run_batch(ctx, &dec, &work, batch)?);
                req.profile.add(Category::GlobalAlign, t.elapsed());
                (req.progress)(shifts.len() as f32 / req.indices.len() as f32);
                Ok(())
            },
        )?;
        trace::dump("register");
        Ok(Some(shifts))
    }
}
