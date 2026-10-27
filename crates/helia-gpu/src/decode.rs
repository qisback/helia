// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Stored bytes to mono frames on the device.

use std::sync::Arc;

use helia_core::debayer::Debayer;
use helia_core::io::raw::{Planes, RowLayout, Sample, SharedFrames};
use helia_core::{BayerPattern, Frame};
use wgpu::{Buffer, BufferUsages, CommandEncoder};

use crate::context::GpuContext;
use crate::kit::{Kernel, RawBatch, Res, groups};
use crate::resident::Resident;

/// Largest batch buffer the stages allocate, bytes.
const BATCH_BYTES: u64 = 256 << 20;

/// The decode kernel and the format it was configured for.
pub(crate) struct Decoder {
    kernel: Kernel,
    layout: RowLayout,
    bayer: Option<BayerPattern>,
    debayer: Debayer,
    resident: Option<Arc<Resident>>,
}

/// Frame batch geometry shared by the stages.
#[derive(Clone, Copy, Debug)]
pub(crate) struct BatchPlan {
    /// Frames per batch.
    pub cap: usize,
    /// Bytes between stored frames in the raw buffer (a multiple of four).
    pub pitch: usize,
    /// Pixels per frame.
    pub pixels: usize,
}

impl BatchPlan {
    /// A plan for `layout` with `per_frame_extra` further bytes of device
    /// memory used per frame by the caller, within the binding limit.
    ///
    /// `slots` sets of such buffers are alive at once (two when batches overlap),
    /// and share the device-memory budget.
    pub(crate) fn new(
        ctx: &GpuContext,
        layout: &RowLayout,
        per_frame_extra: usize,
        slots: usize,
    ) -> Self {
        let pitch = layout.frame_bytes().next_multiple_of(4);
        let pixels = layout.width * layout.height;
        let per_frame = (pitch + pixels * 4 + per_frame_extra).max(1) as u64;
        let by_budget = (BATCH_BYTES * 3 / (per_frame * slots.max(1) as u64)).max(1);
        // No single buffer may exceed the binding limit.
        let largest = (pitch.max(pixels * 4)) as u64;
        let by_limit = (ctx.max_binding() / largest.max(1)).max(1);
        Self {
            cap: by_budget.min(by_limit).min(16) as usize,
            pitch,
            pixels,
        }
    }
}

impl Decoder {
    pub(crate) fn new(
        ctx: &GpuContext,
        frames: &dyn SharedFrames,
        debayer: Debayer,
        resident: Option<Arc<Resident>>,
    ) -> Option<Self> {
        let layout = frames.layout();
        let bayer = frames.bayer();
        // The stencil needs at least a 5x5 frame; smaller ones stay on the CPU.
        if layout.width < 5 || layout.height < 5 {
            return None;
        }
        Some(Self {
            kernel: Kernel::new(ctx, "decode", include_str!("shaders/decode.wgsl"), "main"),
            layout,
            bayer,
            debayer,
            resident,
        })
    }

    /// The store of frames kept on the device between stages, if any.
    pub(crate) fn resident(&self) -> Option<&Resident> {
        self.resident.as_deref()
    }

    pub(crate) fn layout(&self) -> &RowLayout {
        &self.layout
    }

    /// Uniform words for decoding `count` frames into slots from `out_base`.
    fn params(&self, plan: &BatchPlan, count: usize, out_base: usize) -> Vec<u32> {
        let l = &self.layout;
        let sample = match l.sample {
            Sample::U8 => 0,
            Sample::U16Le => 1,
            Sample::U16Be => 2,
            Sample::F32 => 3,
        };
        let planes = match l.planes {
            Planes::One => 0,
            Planes::MeanNormalised => 1,
            Planes::MeanInteger => 2,
        };
        let mut words = vec![
            l.width as u32,
            l.height as u32,
            (plan.pitch / 4) as u32,
            l.stride as u32,
            sample,
            planes,
            u32::from(l.bottom_up),
            u32::from(self.bayer.is_some()),
            count as u32,
            out_base as u32,
            0,
            0,
        ];
        let table = self
            .debayer
            .stencil_table(self.bayer.unwrap_or(BayerPattern::Rggb));
        words.extend(table.iter().flatten().map(|c| c.to_bits()));
        words
    }

    /// Buffers for `plan`.
    pub(crate) fn set(ctx: &GpuContext, plan: BatchPlan) -> DecodeSet {
        DecodeSet {
            plan,
            raw: ctx.storage(
                "raw",
                (plan.cap * plan.pitch) as u64,
                BufferUsages::COPY_DST | BufferUsages::COPY_SRC,
            ),
            mono: ctx.storage(
                "mono",
                (plan.cap * plan.pixels * 4) as u64,
                BufferUsages::COPY_SRC,
            ),
            params: ctx.uniform(&[0u32; 12 + 100]),
        }
    }

    /// Bring one batch of stored frames into the raw buffer and record the
    /// decode of all of them into mono slots `0..batch.len()`.
    ///
    /// Frames the `resident` store holds are copied from it on the device;
    /// the others are uploaded (padded to the plan's pitch) and, if the store
    /// has room, copied into it for the stages after this one.
    pub(crate) fn upload_and_encode(
        &self,
        ctx: &GpuContext,
        enc: &mut CommandEncoder,
        set: &DecodeSet,
        batch: &RawBatch,
    ) {
        let resident = self.resident();
        let pitch = set.plan.pitch as u64;
        for (j, bytes) in batch.bufs.iter().enumerate() {
            let at = j as u64 * pitch;
            match (resident, batch.slots[j]) {
                (Some(res), Some(slot)) => {
                    let (src, from) = res.place(slot);
                    enc.copy_buffer_to_buffer(&src, from, &set.raw, at, pitch);
                }
                (res, _) => {
                    ctx.queue.write_buffer(&set.raw, at, bytes);
                    if let Some(res) = res
                        && let Some(slot) = res.admit(batch.indices[j])
                    {
                        let (dst, to) = res.place(slot);
                        enc.copy_buffer_to_buffer(&set.raw, at, &dst, to, pitch);
                    }
                }
            }
        }
        let frames = &batch.bufs;
        let words = self.params(&set.plan, frames.len(), 0);
        ctx.queue
            .write_buffer(&set.params, 0, bytemuck::cast_slice(&words));
        let bind = self.kernel.bind(
            ctx,
            &[
                Res::All(&set.params),
                Res::All(&set.raw),
                Res::All(&set.mono),
            ],
        );
        self.kernel.dispatch(
            enc,
            &bind,
            (
                groups(self.layout.width, 16),
                groups(self.layout.height, 16),
                frames.len() as u32,
            ),
        );
    }
}

/// Device buffers one batch of frames passes through.
pub(crate) struct DecodeSet {
    pub plan: BatchPlan,
    /// Stored bytes, `plan.pitch` apart.
    pub raw: Buffer,
    /// Decoded mono frames, `plan.pixels` samples each.
    pub mono: Buffer,
    params: Buffer,
}

/// Decode frames to host memory. A diagnostic: the stages keep the frames on
/// the device, this is for tests and tools that want to look at them.
pub(crate) fn decode_to_host(
    ctx: &GpuContext,
    frames: &dyn SharedFrames,
    indices: &[usize],
    debayer: Debayer,
) -> helia_core::Result<Option<Vec<Frame>>> {
    let Some(dec) = Decoder::new(ctx, frames, debayer, None) else {
        return Ok(None);
    };
    let plan = BatchPlan::new(ctx, dec.layout(), 0, 1);
    let set = Decoder::set(ctx, plan);
    let staging = ctx.staging((plan.cap * plan.pixels * 4) as u64);
    let cancel = std::sync::atomic::AtomicBool::new(false);
    let profile = helia_core::profile::Profile::new();
    let mut out = Vec::with_capacity(indices.len());
    let (w, h) = (dec.layout().width, dec.layout().height);
    crate::kit::for_each_raw_batch(
        frames,
        indices,
        plan.cap,
        &profile,
        &cancel,
        None,
        |_, batch| {
            let mut enc = ctx
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
            dec.upload_and_encode(ctx, &mut enc, &set, batch);
            let n = batch.len() * plan.pixels;
            enc.copy_buffer_to_buffer(&set.mono, 0, &staging, 0, (n * 4) as u64);
            ctx.queue.submit(Some(enc.finish()));
            let data = ctx.map_f32(&staging, n)?;
            for chunk in data.chunks_exact(plan.pixels) {
                out.push(Frame {
                    width: w,
                    height: h,
                    data: chunk.to_vec(),
                    bayer: None,
                });
            }
            Ok(())
        },
    )?;
    Ok(Some(out))
}
