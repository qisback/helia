// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Frame scoring stage: decode, blur, reduce and sample on the device, finish
//! on the host.

#![allow(
    clippy::many_single_char_names,
    reason = "x, y, w, h and the sums a, b are the natural names in image maths"
)]

use std::time::Instant;

use std::sync::Arc;

use helia_core::profile::Category;
use helia_core::quality::combine_sums;
use helia_core::{Metric, Rect, Result, ScoreRequest};
use rayon::prelude::*;
use wgpu::{BindGroup, Buffer, BufferUsages};

use crate::context::GpuContext;
use crate::decode::{BatchPlan, DecodeSet, Decoder};
use crate::kit::{Kernel, RawBatch, Res, for_each_batch_overlapped, groups, trace};
use crate::resident::Resident;

/// Compiled scoring kernels.
pub(crate) struct ScoreKernels {
    blur: Kernel,
    reduce: Kernel,
    samples: Kernel,
}

/// Device buffers and bindings for scoring batches of one frame geometry.
struct Work {
    set: DecodeSet,
    params: Buffer,
    partials: Buffer,
    samples: Buffer,
    staging_partials: Buffer,
    staging_samples: Buffer,
    blur_bind: BindGroup,
    reduce_bind: BindGroup,
    samples_bind: BindGroup,
    /// Workgroups over the scored region.
    gx: usize,
    gy: usize,
    /// Floats of partial sums per frame.
    partial_floats: usize,
    samples_per_frame: usize,
    sample_cols: usize,
}

impl ScoreKernels {
    pub(crate) fn new(ctx: &GpuContext) -> Self {
        let make = |label: &str, wgsl: &str| Kernel::new(ctx, label, wgsl, "main");
        Self {
            blur: make("score_blur", include_str!("shaders/score_blur.wgsl")),
            reduce: make("score_reduce", include_str!("shaders/score_reduce.wgsl")),
            samples: make("score_samples", include_str!("shaders/score_samples.wgsl")),
        }
    }

    fn work(&self, ctx: &GpuContext, dec: &Decoder, region: Rect) -> Work {
        let layout = dec.layout();
        let (w, h) = (layout.width, layout.height);
        let gx = groups(region.width(), 16) as usize;
        let gy = groups(region.height(), 16) as usize;
        let sample_cols = w.div_ceil(3);
        let samples_per_frame = sample_cols * h.div_ceil(3);
        let partial_floats = gx * gy * 3;
        let extra = w * h * 4 + (partial_floats + samples_per_frame) * 4;
        let plan = BatchPlan::new(ctx, layout, extra, 2);
        let set = Decoder::set(ctx, plan);
        let blurred = ctx.storage(
            "blurred",
            (plan.cap * plan.pixels * 4) as u64,
            BufferUsages::empty(),
        );
        let partials = ctx.storage(
            "partials",
            (plan.cap * partial_floats * 4) as u64,
            BufferUsages::COPY_SRC,
        );
        let samples = ctx.storage(
            "floor samples",
            (plan.cap * samples_per_frame * 4) as u64,
            BufferUsages::COPY_SRC,
        );
        let params = ctx.uniform(&[0u32; 12]);
        Work {
            blur_bind: self.blur.bind(
                ctx,
                &[Res::All(&params), Res::All(&set.mono), Res::All(&blurred)],
            ),
            reduce_bind: self.reduce.bind(
                ctx,
                &[Res::All(&params), Res::All(&blurred), Res::All(&partials)],
            ),
            samples_bind: self.samples.bind(
                ctx,
                &[Res::All(&params), Res::All(&set.mono), Res::All(&samples)],
            ),
            staging_partials: ctx.staging((plan.cap * partial_floats * 4) as u64),
            staging_samples: ctx.staging((plan.cap * samples_per_frame * 4) as u64),
            set,
            params,
            partials,
            samples,
            gx,
            gy,
            partial_floats,
            samples_per_frame,
            sample_cols,
        }
    }

    /// Queue the decode, blur and reduction of one batch; the results land in
    /// the work's staging buffers once the returned submission completes.
    fn submit_batch(
        &self,
        ctx: &GpuContext,
        dec: &Decoder,
        work: &Work,
        req: &ScoreRequest<'_>,
        region: Rect,
        batch: &RawBatch,
    ) -> wgpu::SubmissionIndex {
        let layout = dec.layout();
        let (w, h) = (layout.width, layout.height);
        let count = batch.len();
        let words: [u32; 12] = [
            w as u32,
            h as u32,
            region.x0 as u32,
            region.y0 as u32,
            region.x1 as u32,
            region.y1 as u32,
            u32::from(req.metric == Metric::LaplacianVariance),
            count as u32,
            work.gx as u32,
            work.gy as u32,
            work.samples_per_frame as u32,
            work.sample_cols as u32,
        ];
        ctx.queue
            .write_buffer(&work.params, 0, bytemuck::cast_slice(&words));
        let mut enc = ctx
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        trace::time("upload", || {
            dec.upload_and_encode(ctx, &mut enc, &work.set, batch);
        });
        self.blur.dispatch(
            &mut enc,
            &work.blur_bind,
            (groups(w, 16), groups(h, 16), count as u32),
        );
        self.reduce.dispatch(
            &mut enc,
            &work.reduce_bind,
            (work.gx as u32, work.gy as u32, count as u32),
        );
        self.samples.dispatch(
            &mut enc,
            &work.samples_bind,
            (groups(work.samples_per_frame, 256), 1, count as u32),
        );
        let (pb, sb) = (
            (count * work.partial_floats * 4) as u64,
            (count * work.samples_per_frame * 4) as u64,
        );
        enc.copy_buffer_to_buffer(&work.partials, 0, &work.staging_partials, 0, pb);
        enc.copy_buffer_to_buffer(&work.samples, 0, &work.staging_samples, 0, sb);
        trace::time("submit", || ctx.queue.submit(Some(enc.finish())))
    }

    /// Score the requested frames; `None` when the frame geometry is out of
    /// reach for the kernels.
    pub(crate) fn run(
        &self,
        ctx: &GpuContext,
        req: &ScoreRequest<'_>,
        resident: Option<Arc<Resident>>,
    ) -> Result<Option<Vec<f32>>> {
        if !req.metric.on_gpu() {
            return Ok(None);
        }
        let Some(dec) = Decoder::new(ctx, req.frames, req.debayer, resident) else {
            return Ok(None);
        };
        let (w, h) = (dec.layout().width, dec.layout().height);
        let region = req.roi.unwrap_or_else(|| Rect::new(0, 0, w, h)).clamp(w, h);
        if region.is_empty() {
            return Ok(Some(vec![0.0; req.indices.len()]));
        }
        // Two sets of buffers: the device works on one batch while the host
        // finishes the previous one.
        let works = [self.work(ctx, &dec, region), self.work(ctx, &dec, region)];
        let metric = req.metric;
        let n_region = region.area() as f64;
        let mut scores = vec![0.0f32; req.indices.len()];
        let mut done = 0usize;
        for_each_batch_overlapped(
            req.frames,
            req.indices,
            works[0].set.plan.cap,
            req.profile,
            req.cancel,
            dec.resident(),
            |slot, _, batch| Ok(self.submit_batch(ctx, &dec, &works[slot], req, region, batch)),
            |slot, start, count, submission| {
                let t = Instant::now();
                let work = &works[slot];
                let part = trace::time("wait partials", || {
                    ctx.map_f32_after(
                        &work.staging_partials,
                        count * work.partial_floats,
                        Some(submission.clone()),
                    )
                })?;
                let floor_samples = trace::time("wait samples", || {
                    ctx.map_f32_after(
                        &work.staging_samples,
                        count * work.samples_per_frame,
                        Some(submission.clone()),
                    )
                })?;
                let out: Vec<f32> = trace::time("host finish", || {
                    part.par_chunks_exact(work.partial_floats)
                        .zip(floor_samples.par_chunks_exact(work.samples_per_frame))
                        .map(|(sums, samp)| {
                            let (mut mean, mut a, mut b) = (0.0f64, 0.0f64, 0.0f64);
                            for g in sums.as_chunks::<3>().0 {
                                mean += f64::from(g[0]);
                                a += f64::from(g[1]);
                                b += f64::from(g[2]);
                            }
                            combine_sums(metric, n_region, mean, a, b, floor_of(samp))
                        })
                        .collect()
                });
                scores[start..start + count].copy_from_slice(&out);
                done += count;
                req.profile.add(Category::Quality, t.elapsed());
                (req.progress)(done as f32 / req.indices.len() as f32);
                Ok(())
            },
        )?;
        trace::dump("score");
        Ok(Some(scores))
    }
}

/// The 1st percentile of the sampled pixels, as [`helia_core::quality::noise_floor`].
pub(crate) fn floor_of(samples: &[f32]) -> f32 {
    let mut v: Vec<f32> = samples.iter().copied().filter(|x| x.is_finite()).collect();
    if v.is_empty() {
        return 0.0;
    }
    let k = v.len() / 100;
    *v.select_nth_unstable_by(k, f32::total_cmp).1
}
