// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Mean stacking stage on the device: the whole-frame sums and every
//! alignment point tile accumulate from batches of decoded frames without
//! leaving the device until the end.

#![allow(
    clippy::many_single_char_names,
    reason = "x, y, w, h are the natural names in image maths"
)]

use std::time::Instant;

use std::sync::Arc;

use helia_core::profile::Category;
use helia_core::resample::{Taps, split};
use helia_core::{
    Accumulated, FrameDeposit, Kernel as ResampleKernel, Result, StackRequest, StackResult,
};
use rayon::prelude::*;
use wgpu::{BindGroup, Buffer, BufferUsages};

use crate::context::GpuContext;
use crate::decode::{BatchPlan, DecodeSet, Decoder};
use crate::kit::{Kernel, Res, for_each_raw_batch, groups, trace};
use crate::resident::Resident;

/// u32 words of one deposit record, see the shaders.
const DEPOSIT_WORDS: usize = 24;
/// Batches the host may run ahead of the device.
const IN_FLIGHT: usize = 2;

/// Compiled stacking kernels.
pub(crate) struct StackKernels {
    frame: Kernel,
    tiles: Kernel,
}

/// Device state of one stacking run.
struct Work {
    set: DecodeSet,
    frame_params: Buffer,
    tile_params: Buffer,
    frame_deposits: Buffer,
    tile_deposits: Buffer,
    frame_bind: BindGroup,
    tile_bind: BindGroup,
    acc_sum: Buffer,
    acc_weight: Buffer,
    tile_sum: Buffer,
    tile_weight: Buffer,
    /// Start of each tile in the tile accumulators.
    offsets: Vec<usize>,
    total_tile_px: usize,
    max_tile_px: usize,
}

/// Accumulators and tile tables on the device.
struct Accumulators {
    frame_sum: Buffer,
    frame_weight: Buffer,
    tile_sum: Buffer,
    tile_weight: Buffer,
    /// Reference over every tile, concatenated.
    centres: Buffer,
    /// Tile rectangles and offsets, see the shader.
    tiles: Buffer,
    offsets: Vec<usize>,
    total: usize,
    max_px: usize,
}

impl Accumulators {
    /// `None` when the accumulators would not fit one binding.
    fn new(ctx: &GpuContext, req: &StackRequest<'_>) -> Option<Self> {
        let (w, h) = (req.width, req.height);
        let mut offsets = Vec::with_capacity(req.tiles.len());
        let mut total = 0usize;
        for r in req.tiles {
            offsets.push(total);
            total += r.area();
        }
        let max_px = req
            .tiles
            .iter()
            .map(helia_core::Rect::area)
            .max()
            .unwrap_or(0);
        if (total.max(w * h) * 4) as u64 > ctx.max_binding() {
            return None;
        }
        let rw = BufferUsages::COPY_SRC | BufferUsages::COPY_DST;
        let centres = ctx.storage("centres", (total.max(1) * 4) as u64, BufferUsages::COPY_DST);
        let centre_data: Vec<f32> = req.centres.iter().flatten().copied().collect();
        ctx.queue
            .write_buffer(&centres, 0, bytemuck::cast_slice(&centre_data));
        let info: Vec<u32> = req
            .tiles
            .iter()
            .zip(&offsets)
            .flat_map(|(r, &off)| {
                [
                    r.x0 as u32,
                    r.y0 as u32,
                    r.width() as u32,
                    r.height() as u32,
                    off as u32,
                    0,
                    0,
                    0,
                ]
            })
            .collect();
        let tiles = ctx.storage(
            "tiles",
            (info.len().max(8) * 4) as u64,
            BufferUsages::COPY_DST,
        );
        ctx.queue
            .write_buffer(&tiles, 0, bytemuck::cast_slice(&info));
        Some(Self {
            frame_sum: ctx.storage("frame sum", (w * h * 4) as u64, rw),
            frame_weight: ctx.storage("frame weight", (w * h * 4) as u64, rw),
            tile_sum: ctx.storage("tile sum", (total.max(1) * 4) as u64, rw),
            tile_weight: ctx.storage("tile weight", (total.max(1) * 4) as u64, rw),
            centres,
            tiles,
            offsets,
            total,
            max_px,
        })
    }
}

/// One deposit as the words the shaders read.
fn deposit_words(kernel: ResampleKernel, d: FrameDeposit) -> [u32; DEPOSIT_WORDS] {
    let mut w = [0u32; DEPOSIT_WORDS];
    let Some((shift, weight)) = d else {
        return w;
    };
    let (ox, fx) = split(shift.dx);
    let (oy, fy) = split(shift.dy);
    let (tx, ty) = (Taps::new(kernel, fx), Taps::new(kernel, fy));
    w[0] = shift.dx.to_bits();
    w[1] = shift.dy.to_bits();
    w[2] = weight.to_bits();
    w[3] = 1;
    w[4] = ox as i32 as u32;
    w[5] = oy as i32 as u32;
    w[6] = tx.n as u32;
    w[7] = tx.lo as i32 as u32;
    for i in 0..6 {
        w[8 + i] = tx.w[i].to_bits();
        w[16 + i] = ty.w[i].to_bits();
    }
    w
}

impl StackKernels {
    pub(crate) fn new(ctx: &GpuContext) -> Self {
        let make = |label: &str, wgsl: &str| Kernel::new(ctx, label, wgsl, "main");
        Self {
            frame: make("stack_frame", include_str!("shaders/stack_frame.wgsl")),
            tiles: make("stack_tiles", include_str!("shaders/stack_tiles.wgsl")),
        }
    }

    fn work(&self, ctx: &GpuContext, dec: &Decoder, req: &StackRequest<'_>) -> Option<Work> {
        let layout = dec.layout();
        let (w, h) = (layout.width, layout.height);
        if (req.width, req.height) != (w, h) {
            return None;
        }
        let acc = Accumulators::new(ctx, req)?;
        let n_tiles = req.tiles.len();
        let deposit_bytes = DEPOSIT_WORDS * 4;
        let plan = BatchPlan::new(ctx, layout, (n_tiles + 1) * deposit_bytes, 1);
        let set = Decoder::set(ctx, plan);
        let frame_params = ctx.uniform(&[w as u32, h as u32, 0, 0]);
        let tile_params = ctx.uniform(&[w as u32, h as u32, 0, n_tiles as u32]);
        let frame_deposits = ctx.storage(
            "frame deposits",
            (plan.cap * deposit_bytes) as u64,
            BufferUsages::COPY_DST,
        );
        let tile_deposits = ctx.storage(
            "tile deposits",
            (plan.cap * n_tiles.max(1) * deposit_bytes) as u64,
            BufferUsages::COPY_DST,
        );
        let frame_bind = self.frame.bind(
            ctx,
            &[
                Res::All(&frame_params),
                Res::All(&set.mono),
                Res::All(&frame_deposits),
                Res::All(&acc.frame_sum),
                Res::All(&acc.frame_weight),
            ],
        );
        let tile_bind = self.tiles.bind(
            ctx,
            &[
                Res::All(&tile_params),
                Res::All(&set.mono),
                Res::All(&tile_deposits),
                Res::All(&acc.tiles),
                Res::All(&acc.centres),
                Res::All(&acc.tile_sum),
                Res::All(&acc.tile_weight),
            ],
        );
        Some(Work {
            set,
            frame_params,
            tile_params,
            frame_deposits,
            tile_deposits,
            frame_bind,
            tile_bind,
            acc_sum: acc.frame_sum,
            acc_weight: acc.frame_weight,
            tile_sum: acc.tile_sum,
            tile_weight: acc.tile_weight,
            offsets: acc.offsets,
            total_tile_px: acc.total,
            max_tile_px: acc.max_px,
        })
    }

    /// Stack the frames of the request; `None` when the job does not fit the kernels.
    pub(crate) fn run(
        &self,
        ctx: &GpuContext,
        req: &StackRequest<'_>,
        resident: Option<Arc<Resident>>,
    ) -> Result<Option<StackResult>> {
        let Some(dec) = Decoder::new(ctx, req.frames, req.debayer, resident) else {
            return Ok(None);
        };
        let Some(work) = self.work(ctx, &dec, req) else {
            return Ok(None);
        };
        let (w, h) = (req.width, req.height);
        let n_tiles = req.tiles.len();
        let kernel = req.kernel;
        let (frame_deposit, tile_deposit) = (req.frame_deposit, req.tile_deposit);
        let mut in_flight: std::collections::VecDeque<wgpu::SubmissionIndex> =
            std::collections::VecDeque::new();
        let mut done = 0usize;
        for_each_raw_batch(
            req.frames,
            req.order,
            work.set.plan.cap,
            req.profile,
            req.cancel,
            dec.resident(),
            |start, batch| {
                let t = Instant::now();
                let count = batch.len();
                // Deposit records of this batch, built in parallel.
                let frame_words: Vec<u32> = (0..count)
                    .flat_map(|j| deposit_words(kernel, frame_deposit(start + j)))
                    .collect();
                let tile_words: Vec<u32> = (0..count)
                    .into_par_iter()
                    .flat_map_iter(|j| {
                        (0..n_tiles)
                            .flat_map(move |t| deposit_words(kernel, tile_deposit(start + j, t)))
                    })
                    .collect();
                let any_frame = frame_words.chunks(DEPOSIT_WORDS).any(|d| d[3] != 0);
                let any_tile = tile_words.chunks(DEPOSIT_WORDS).any(|d| d[3] != 0);
                let write = |buf: &Buffer, words: &[u32]| {
                    ctx.queue.write_buffer(buf, 0, bytemuck::cast_slice(words));
                };
                write(&work.frame_params, &[w as u32, h as u32, count as u32, 0]);
                write(
                    &work.tile_params,
                    &[w as u32, h as u32, count as u32, n_tiles as u32],
                );
                write(&work.frame_deposits, &frame_words);
                if n_tiles > 0 {
                    write(&work.tile_deposits, &tile_words);
                }
                let mut enc = ctx
                    .device
                    .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
                dec.upload_and_encode(ctx, &mut enc, &work.set, batch);
                if any_frame {
                    self.frame.dispatch(
                        &mut enc,
                        &work.frame_bind,
                        (groups(w, 16), groups(h, 16), 1),
                    );
                }
                if any_tile && n_tiles > 0 {
                    self.tiles.dispatch(
                        &mut enc,
                        &work.tile_bind,
                        (groups(work.max_tile_px, 256), n_tiles as u32, 1),
                    );
                }
                in_flight.push_back(ctx.queue.submit(Some(enc.finish())));
                if in_flight.len() > IN_FLIGHT
                    && let Some(old) = in_flight.pop_front()
                {
                    trace::time("throttle", || {
                        let _ = ctx.device.poll(wgpu::PollType::Wait {
                            submission_index: Some(old),
                            timeout: None,
                        });
                    });
                }
                done += count;
                req.profile.add(Category::Stack, t.elapsed());
                (req.progress)(done as f32 / req.order.len().max(1) as f32);
                Ok(())
            },
        )?;

        let result = trace::time("read back", || Self::read(ctx, &work, w, h, req.centres))?;
        trace::dump("stack");
        Ok(Some(result))
    }

    fn read(
        ctx: &GpuContext,
        work: &Work,
        w: usize,
        h: usize,
        centres: &[Vec<f32>],
    ) -> Result<StackResult> {
        let read = |buf: &Buffer, len: usize| -> Result<Vec<f32>> {
            let staging = ctx.staging((len.max(1) * 4) as u64);
            let mut enc = ctx
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
            enc.copy_buffer_to_buffer(buf, 0, &staging, 0, (len * 4) as u64);
            ctx.queue.submit(Some(enc.finish()));
            Ok(ctx.map_f32(&staging, len)?)
        };
        let sum = read(&work.acc_sum, w * h)?;
        let weight = read(&work.acc_weight, w * h)?;
        let (tile_sum, tile_weight) = if work.total_tile_px > 0 {
            (
                read(&work.tile_sum, work.total_tile_px)?,
                read(&work.tile_weight, work.total_tile_px)?,
            )
        } else {
            (Vec::new(), Vec::new())
        };
        // Finished tiles are the reference plus the weighted mean offset.
        let tiles = work
            .offsets
            .iter()
            .zip(centres)
            .map(|(&off, centre)| {
                centre
                    .iter()
                    .enumerate()
                    .map(|(i, &c)| {
                        let (s, wt) = (tile_sum[off + i], tile_weight[off + i]);
                        if wt > 0.0 { c + s / wt } else { f32::NAN }
                    })
                    .collect()
            })
            .collect();
        Ok(StackResult {
            tiles,
            frame: Accumulated {
                width: w,
                height: h,
                sum,
                weight,
            },
        })
    }
}
