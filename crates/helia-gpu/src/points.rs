// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Alignment point measurement stage on the device.
//!
//! Per batch of frames: decode, smooth and background-subtract (separable
//! Gaussians), blur and tabulate the gradient energy for window scoring, and
//! correlate every active point's template over the search area. The device
//! also finds the best offset of each point, the window sums there and the
//! frame's noise floor, so a batch reads back fourteen floats per point and one
//! per frame, not the correlation surfaces and the floor samples. The sub-pixel
//! peak fit, the neighbour interpolation of unreliable points and the scoring
//! formula run on the host with the same code as the CPU path, from the 3x3
//! neighbourhood of the peak. Windows of interpolated points are only known
//! after the fit, so their sums take a second, small dispatch.

#![allow(
    clippy::many_single_char_names,
    reason = "x, y, w, h and the sums a, b are the natural names in image maths"
)]

use std::time::Instant;

use std::sync::Arc;

use helia_core::align::multipoint::{
    ApMeasure, FillPlan, LocalTable, MultipointConfig, measurement_from_patch, plan_fill,
};
use helia_core::filter::gaussian_taps;
use helia_core::profile::Category;
use helia_core::quality::combine_sums;
use helia_core::{MeasureRequest, Metric, Rect, Result, Shift};
use rayon::prelude::*;
use wgpu::{BindGroup, Buffer, BufferUsages};

use crate::context::GpuContext;
use crate::decode::{BatchPlan, DecodeSet, Decoder};
use crate::kit::{Kernel, RawBatch, Res, for_each_batch_overlapped, groups, trace};
use crate::resident::Resident;

/// Taps a Gaussian pass can carry (the uniform holds 32 vec4).
const MAX_TAPS: usize = 128;
/// Largest search radius the surface kernel's shared memory holds.
const MAX_RADIUS: usize = 7;
/// Zero columns either side of every row of the correlated frame.
const CORR_PAD: usize = 2;
/// Floats the surface kernel returns per point and frame.
const RECORD: usize = 14;

/// Words of one frame's record in the floor selection: 256 histogram bins and
/// [prefix, rank, count, step].
const FLOOR_RECORD: usize = 260;
/// Workgroups that share the histogram of one frame.
const FLOOR_SLICES: u32 = 32;

/// The noise-floor selection: a histogram kernel and a kernel that picks the
/// byte holding the wanted rank, run four times (see `floor_select.wgsl`).
pub(crate) struct FloorKernel {
    hist: Kernel,
    pick: Kernel,
}

/// Bindings of [`FloorKernel`] for one set of buffers.
pub(crate) struct FloorBind {
    hist: BindGroup,
    pick: BindGroup,
}

impl FloorKernel {
    pub(crate) fn new(ctx: &GpuContext) -> Self {
        let wgsl = include_str!("shaders/floor_select.wgsl");
        Self {
            hist: Kernel::new(ctx, "floor_hist", wgsl, "hist"),
            pick: Kernel::new(ctx, "floor_pick", wgsl, "pick"),
        }
    }

    /// `params`, the frames' samples and the selection records (zeroed, see
    /// [`FLOOR_RECORD`]), after which the floors are written.
    pub(crate) fn bind(
        &self,
        ctx: &GpuContext,
        params: &Buffer,
        mono: &Buffer,
        sel: &Buffer,
    ) -> FloorBind {
        FloorBind {
            hist: self
                .hist
                .bind(ctx, &[Res::All(params), Res::All(sel), Res::All(mono)]),
            pick: self.pick.bind(ctx, &[Res::All(params), Res::All(sel)]),
        }
    }

    /// Record the selection of the floors of `count` frames.
    pub(crate) fn encode(&self, enc: &mut wgpu::CommandEncoder, bind: &FloorBind, count: usize) {
        for _ in 0..4 {
            self.hist
                .dispatch(enc, &bind.hist, (FLOOR_SLICES, count as u32, 1));
            self.pick.dispatch(enc, &bind.pick, (count as u32, 1, 1));
        }
    }
}

/// Compiled kernels of the stage.
pub(crate) struct PointKernels {
    gauss_h: Kernel,
    gauss_v: Kernel,
    blur: Kernel,
    energy: Kernel,
    floor: FloorKernel,
    surface: Kernel,
    windows: Kernel,
}

/// One Gaussian: its uniform and the bindings of both passes.
struct Gauss {
    params: Buffer,
    h_bind: BindGroup,
    v_bind: BindGroup,
}

/// Device buffers and bindings for one run of the stage.
struct Work {
    set: DecodeSet,
    smooth: Option<Gauss>,
    high: Gauss,
    blur_params: Buffer,
    energy_params: Buffer,
    surface_params: Buffer,
    window_params: Buffer,
    blur_bind: BindGroup,
    energy_bind: BindGroup,
    floor_bind: FloorBind,
    surface_bind: BindGroup,
    windows_bind: BindGroup,
    guess: Buffer,
    surf: Buffer,
    floor_sel: Buffer,
    window_list: Buffer,
    window_sums: Buffer,
    staging_surf: Buffer,
    staging_floors: Buffer,
    staging_sums: Buffer,
    side: usize,
    stride: usize,
    n_active: usize,
    floor_params: Buffer,
    /// Word of `floor_sel` where the floors start.
    floor_base: usize,
}

/// What the host keeps of one frame while a batch is finished.
struct FrameResult {
    row: Vec<Option<ApMeasure>>,
    plans: Vec<FillPlan>,
    floor: f32,
}

/// Uniform words for a Gaussian of `sigma`, or for the identity when it is zero.
fn words_of_taps(sigma: f32) -> Option<Vec<u32>> {
    let taps = if sigma > 0.0 {
        gaussian_taps(sigma)
    } else {
        vec![0.0, 1.0, 0.0]
    };
    if taps.len() > MAX_TAPS {
        return None;
    }
    let mut words = vec![0u32; 8 + MAX_TAPS];
    words[3] = (taps.len() / 2) as u32;
    for (i, t) in taps.iter().enumerate() {
        words[8 + i] = t.to_bits();
    }
    Some(words)
}

impl PointKernels {
    pub(crate) fn new(ctx: &GpuContext) -> Self {
        let make = |label: &str, wgsl: &str| Kernel::new(ctx, label, wgsl, "main");
        Self {
            gauss_h: make("gauss_h", include_str!("shaders/gauss_h.wgsl")),
            gauss_v: make("gauss_v", include_str!("shaders/gauss_v.wgsl")),
            blur: make("score_blur", include_str!("shaders/score_blur.wgsl")),
            energy: make("energy", include_str!("shaders/energy.wgsl")),
            floor: FloorKernel::new(ctx),
            surface: make("ap_surface", include_str!("shaders/ap_surface.wgsl")),
            windows: make("window_sums", include_str!("shaders/window_sums.wgsl")),
        }
    }

    #[allow(clippy::too_many_lines)] // one flat list of buffer and binding set-up
    fn work(&self, ctx: &GpuContext, dec: &Decoder, req: &MeasureRequest<'_>) -> Option<Work> {
        let cfg = req.config;
        if cfg.search_radius > MAX_RADIUS || cfg.search_radius == 0 {
            return None;
        }
        let layout = dec.layout();
        let (w, h) = (layout.width, layout.height);
        let active: Vec<usize> = (0..req.templates.len())
            .filter(|&a| req.templates[a].is_some())
            .collect();
        let n_active = active.len();
        let side = 2 * cfg.search_radius + 1;
        let stride = RECORD;
        let sample_cols = w.div_ceil(3);
        let samples_per_frame = sample_cols * h.div_ceil(3);
        // Smoothed, background-subtracted, blurred, energy and scratch frames. The
        // surfaces and floor samples this stage once held are counted still: the
        // batch size sets how the frames are grouped in the sums of the stack,
        // so it stays what it was.
        let extra =
            5 * w * h * 4 + (n_active * (side * side + 4) + samples_per_frame) * 4 + n_active * 40;
        let plan = BatchPlan::new(ctx, layout, extra, 2);
        let set = Decoder::set(ctx, plan);
        let frame_buf = |label: &str| {
            ctx.storage(
                label,
                (plan.cap * plan.pixels * 4) as u64,
                BufferUsages::empty(),
            )
        };
        let tmp = frame_buf("tmp");
        let blurred = frame_buf("blurred");
        let energy = frame_buf("energy");
        let scored_own = (cfg.smooth > 0.0).then(|| frame_buf("scored"));
        // The correlated frame has two zero columns either side of each row.
        let corr_stride = w + 2 * CORR_PAD;
        let correlate = ctx.storage(
            "correlate",
            (plan.cap * corr_stride * h * 4) as u64,
            BufferUsages::empty(),
        );
        let scored = scored_own.as_ref().unwrap_or(&set.mono);

        // A Gaussian of `sigma` from `src` into `dst`, optionally subtracted from
        // `minus` and written in the padded layout.
        let gauss = |sigma: f32,
                     src: &Buffer,
                     dst: &Buffer,
                     minus: Option<&Buffer>,
                     padded: bool|
         -> Option<Gauss> {
            let mut words = words_of_taps(sigma)?;
            words[0] = w as u32;
            words[1] = h as u32;
            words[4] = u32::from(minus.is_some());
            words[5] = if padded { corr_stride as u32 } else { w as u32 };
            words[6] = if padded { CORR_PAD as u32 } else { 0 };
            let params = ctx.uniform(&words);
            let h_bind = self
                .gauss_h
                .bind(ctx, &[Res::All(&params), Res::All(src), Res::All(&tmp)]);
            let v_bind = self.gauss_v.bind(
                ctx,
                &[
                    Res::All(&params),
                    Res::All(&tmp),
                    Res::All(dst),
                    Res::All(minus.unwrap_or(&tmp)),
                ],
            );
            Some(Gauss {
                params,
                h_bind,
                v_bind,
            })
        };
        let smooth = match &scored_own {
            Some(buf) => Some(gauss(cfg.smooth, &set.mono, buf, None, false)?),
            None => None,
        };
        // Background removal, or with none a pass-through blur of radius zero
        // that only moves the scored frame into the padded layout.
        let high = if cfg.highpass > 0.0 {
            gauss(cfg.highpass, &set.mono, &correlate, Some(scored), true)?
        } else {
            gauss(0.0, scored, &correlate, None, true)?
        };

        // Templates and point descriptions.
        let mut tmpl: Vec<f32> = Vec::new();
        let mut points: Vec<u32> = Vec::with_capacity(n_active * 8);
        for &a in &active {
            let t = req.templates[a].as_ref()?;
            let r = t.rect();
            points.extend([
                r.x0 as u32,
                r.y0 as u32,
                r.width() as u32,
                r.height() as u32,
                tmpl.len() as u32,
                t.norm().to_bits(),
                0,
                0,
            ]);
            tmpl.extend_from_slice(t.data());
        }
        let upload = |label: &str, bytes: &[u8]| {
            let buf = ctx.storage(label, bytes.len().max(16) as u64, BufferUsages::COPY_DST);
            ctx.queue.write_buffer(&buf, 0, bytes);
            buf
        };
        let tmpl_buf = upload("templates", bytemuck::cast_slice(&tmpl));
        let points_buf = upload("points", bytemuck::cast_slice(&points));
        let guess = ctx.storage("guess", (plan.cap * 8) as u64, BufferUsages::COPY_DST);

        let surf = ctx.storage(
            "surfaces",
            (plan.cap * n_active.max(1) * stride * 4) as u64,
            BufferUsages::COPY_SRC,
        );
        let max_windows = plan.cap * n_active.max(1);
        let window_list = ctx.storage("windows", (max_windows * 32) as u64, BufferUsages::COPY_DST);
        let window_sums = ctx.storage(
            "window sums",
            (max_windows * 8) as u64,
            BufferUsages::COPY_SRC,
        );

        let blur_params = ctx.uniform(&[0u32; 12]);
        let energy_params = ctx.uniform(&[0u32; 4]);
        let surface_params = ctx.uniform(&[0u32; 12]);
        // Row bands per offset triple, as many as fit the 128 threads.
        let triples = side.div_ceil(3);
        let parts = (128 / (side * triples)).clamp(1, 8);
        let rows_per_part = req
            .templates
            .iter()
            .flatten()
            .map(|t| t.rect().height())
            .max()
            .unwrap_or(1)
            .div_ceil(parts);
        let window_params = ctx.uniform(&[0u32; 4]);
        ctx.queue.write_buffer(
            &surface_params,
            0,
            bytemuck::cast_slice(&[
                w as u32,
                h as u32,
                0,
                cfg.search_radius as u32,
                side as u32,
                n_active as u32,
                stride as u32,
                parts as u32,
                rows_per_part as u32,
                corr_stride as u32,
                cfg.min_correlation.to_bits(),
                0,
            ]),
        );
        let blur_bind = self.blur.bind(
            ctx,
            &[Res::All(&blur_params), Res::All(scored), Res::All(&blurred)],
        );
        let energy_bind = self.energy.bind(
            ctx,
            &[
                Res::All(&energy_params),
                Res::All(&blurred),
                Res::All(&energy),
            ],
        );
        let floor_sel = ctx.storage(
            "floor selection",
            (plan.cap * (FLOOR_RECORD + 1) * 4) as u64,
            BufferUsages::COPY_SRC,
        );
        let floor_params = ctx.uniform(&[
            w as u32,
            h as u32,
            0,
            samples_per_frame as u32,
            sample_cols as u32,
            FLOOR_SLICES,
            (plan.cap * FLOOR_RECORD) as u32,
            0,
        ]);
        let floor_bind = self.floor.bind(ctx, &floor_params, scored, &floor_sel);
        let surface_bind = self.surface.bind(
            ctx,
            &[
                Res::All(&surface_params),
                Res::All(&correlate),
                Res::All(&blurred),
                Res::All(&energy),
                Res::All(&tmpl_buf),
                Res::All(&points_buf),
                Res::All(&guess),
                Res::All(&surf),
            ],
        );
        let windows_bind = self.windows.bind(
            ctx,
            &[
                Res::All(&window_params),
                Res::All(&blurred),
                Res::All(&energy),
                Res::All(&window_list),
                Res::All(&window_sums),
            ],
        );
        Some(Work {
            staging_surf: ctx.staging((plan.cap * n_active.max(1) * stride * 4) as u64),
            staging_floors: ctx.staging((plan.cap * 4) as u64),
            staging_sums: ctx.staging((max_windows * 8) as u64),
            set,
            smooth,
            high,
            blur_params,
            energy_params,
            surface_params,
            window_params,
            blur_bind,
            energy_bind,
            floor_bind,
            floor_params,
            floor_base: plan.cap * FLOOR_RECORD,
            surface_bind,
            windows_bind,
            guess,
            surf,
            floor_sel,
            window_list,
            window_sums,
            side,
            stride,
            n_active,
        })
    }

    /// Queue everything up to the point records of one batch; they and the
    /// noise floors reach the staging buffers when the submission completes.
    fn submit_surfaces(
        &self,
        ctx: &GpuContext,
        dec: &Decoder,
        work: &Work,
        req: &MeasureRequest<'_>,
        start: usize,
        batch: &RawBatch,
    ) -> wgpu::SubmissionIndex {
        let layout = dec.layout();
        let (w, h) = (layout.width, layout.height);
        let count = batch.len();
        let write = |buf: &Buffer, word: usize, v: u32| {
            ctx.queue
                .write_buffer(buf, (word * 4) as u64, bytemuck::bytes_of(&v));
        };
        for g in work.smooth.iter().chain([&work.high]) {
            write(&g.params, 2, count as u32);
        }
        write(&work.blur_params, 0, w as u32);
        write(&work.blur_params, 1, h as u32);
        write(&work.blur_params, 7, count as u32);
        write(&work.floor_params, 2, count as u32);
        write(&work.energy_params, 0, w as u32);
        write(&work.energy_params, 1, h as u32);
        write(&work.energy_params, 2, count as u32);
        write(&work.surface_params, 2, count as u32);
        let guess: Vec<i32> = req.shifts[start..start + count]
            .iter()
            .flat_map(|s| [s.dx.round() as i32, s.dy.round() as i32])
            .collect();
        ctx.queue
            .write_buffer(&work.guess, 0, bytemuck::cast_slice(&guess));

        let full = (groups(w, 16), groups(h, 16), count as u32);
        let mut enc = ctx
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        trace::time("upload", || {
            dec.upload_and_encode(ctx, &mut enc, &work.set, batch);
        });
        for g in work.smooth.iter().chain([&work.high]) {
            self.gauss_h.dispatch(&mut enc, &g.h_bind, full);
            self.gauss_v.dispatch(&mut enc, &g.v_bind, full);
        }
        self.blur.dispatch(&mut enc, &work.blur_bind, full);
        self.energy.dispatch(&mut enc, &work.energy_bind, full);
        self.floor.encode(&mut enc, &work.floor_bind, count);
        if work.n_active > 0 {
            self.surface.dispatch(
                &mut enc,
                &work.surface_bind,
                (work.n_active as u32, count as u32, 1),
            );
        }
        let surf_floats = count * work.n_active * work.stride;
        if surf_floats > 0 {
            enc.copy_buffer_to_buffer(
                &work.surf,
                0,
                &work.staging_surf,
                0,
                (surf_floats * 4) as u64,
            );
        }
        enc.copy_buffer_to_buffer(
            &work.floor_sel,
            (work.floor_base * 4) as u64,
            &work.staging_floors,
            0,
            (count * 4) as u64,
        );
        trace::time("submit", || ctx.queue.submit(Some(enc.finish())))
    }

    /// Collect the point records and noise floors of a submitted batch.
    fn read_surfaces(
        ctx: &GpuContext,
        work: &Work,
        count: usize,
        submission: &wgpu::SubmissionIndex,
    ) -> Result<(Vec<f32>, Vec<f32>)> {
        let surf_floats = count * work.n_active * work.stride;
        let surf = if surf_floats > 0 {
            trace::time("wait surfaces", || {
                ctx.map_f32_after(&work.staging_surf, surf_floats, Some(submission.clone()))
            })?
        } else {
            Vec::new()
        };
        Ok((
            surf,
            trace::time("wait floors", || {
                ctx.map_f32_after(&work.staging_floors, count, Some(submission.clone()))
            })?,
        ))
    }

    /// Queue the sums over extra windows (`frame, rect`), after the peak fits.
    /// Does not wait; [`Self::read_window_sums`] collects them.
    fn submit_window_sums(
        &self,
        ctx: &GpuContext,
        work: &Work,
        w: usize,
        h: usize,
        windows: &[(usize, Rect)],
    ) -> Option<wgpu::SubmissionIndex> {
        if windows.is_empty() {
            return None;
        }
        let words: Vec<u32> = windows
            .iter()
            .flat_map(|(f, r)| {
                [
                    *f as u32,
                    r.x0 as u32,
                    r.y0 as u32,
                    r.width() as u32,
                    r.height() as u32,
                    0,
                    0,
                    0,
                ]
            })
            .collect();
        ctx.queue
            .write_buffer(&work.window_list, 0, bytemuck::cast_slice(&words));
        ctx.queue.write_buffer(
            &work.window_params,
            0,
            bytemuck::cast_slice(&[w as u32, h as u32, windows.len() as u32, 0]),
        );
        let mut enc = ctx
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        self.windows
            .dispatch(&mut enc, &work.windows_bind, (windows.len() as u32, 1, 1));
        enc.copy_buffer_to_buffer(
            &work.window_sums,
            0,
            &work.staging_sums,
            0,
            (windows.len() * 8) as u64,
        );
        Some(ctx.queue.submit(Some(enc.finish())))
    }

    /// The sums queued by [`Self::submit_window_sums`] for `n` windows.
    fn read_window_sums(
        ctx: &GpuContext,
        work: &Work,
        n: usize,
        submission: Option<wgpu::SubmissionIndex>,
    ) -> Result<Vec<f32>> {
        if n == 0 {
            return Ok(Vec::new());
        }
        // Wait for these sums only, not for the batch queued behind them.
        Ok(ctx.map_f32_after(&work.staging_sums, n * 2, submission)?)
    }

    /// Store a batch's rows once its window sums have arrived.
    fn finish_batch(
        ctx: &GpuContext,
        table: &mut LocalTable,
        work: &Work,
        batch: Deferred,
    ) -> Result<()> {
        let Deferred {
            start,
            mut results,
            n_windows,
            submission,
        } = batch;
        let sums = Self::read_window_sums(ctx, work, n_windows, submission)?;
        let mut k = 0;
        for r in &mut results {
            for p in &r.plans {
                let area = p.window.area() as f64;
                r.row[p.ap] = Some(ApMeasure {
                    shift: p.shift,
                    quality: combine_sums(
                        Metric::GradientEnergy,
                        area,
                        f64::from(sums[2 * k]),
                        f64::from(sums[2 * k + 1]),
                        0.0,
                        r.floor,
                    ),
                    correlation: 0.0,
                    interpolated: true,
                });
                k += 1;
            }
        }
        for (j, r) in results.iter().enumerate() {
            table.set_row(start + j, &r.row);
        }
        Ok(())
    }

    /// Measure every active point of the requested frames.
    #[allow(clippy::too_many_lines, reason = "one flat batch loop")]
    pub(crate) fn run(
        &self,
        ctx: &GpuContext,
        req: &MeasureRequest<'_>,
        resident: Option<Arc<Resident>>,
    ) -> Result<Option<LocalTable>> {
        let Some(dec) = Decoder::new(ctx, req.frames, req.debayer, resident) else {
            return Ok(None);
        };
        // Two sets of buffers: the device works on one batch while the host
        // fits the previous one.
        let (Some(first), Some(second)) = (self.work(ctx, &dec, req), self.work(ctx, &dec, req))
        else {
            return Ok(None);
        };
        let works = [first, second];
        let work = &works[0];
        let (w, h) = (dec.layout().width, dec.layout().height);
        let n_aps = req.grid.aps.len();
        let mut table = LocalTable::new(req.indices.len(), n_aps);
        let active: Vec<usize> = (0..n_aps).filter(|&a| req.templates[a].is_some()).collect();
        let (grid, templates, cfg) = (req.grid, req.templates, req.config);
        let shifts = req.shifts;
        let mut done = 0usize;
        let trace = std::env::var_os("HELIA_GPU_TRACE").is_some();
        let (mut t_device, mut t_fit, mut t_windows) = (0.0f64, 0.0f64, 0.0f64);
        let wall = Instant::now();
        let mut deferred: Option<(usize, Deferred)> = None;
        for_each_batch_overlapped(
            req.frames,
            req.indices,
            work.set.plan.cap,
            req.profile,
            req.cancel,
            dec.resident(),
            |slot, start, batch| {
                Ok(self.submit_surfaces(ctx, &dec, &works[slot], req, start, batch))
            },
            |slot, start, count, submission| {
                let work = &works[slot];
                let t = Instant::now();
                let (surf, floors) = Self::read_surfaces(ctx, work, count, &submission)?;
                t_device += t.elapsed().as_secs_f64();
                let t_fit0 = Instant::now();
                // Fit every point of every frame on the host.
                let results: Vec<FrameResult> = (0..count)
                    .into_par_iter()
                    .map(|j| {
                        let guess = shifts[start + j];
                        let floor = floors[j];
                        let row = fit_frame(
                            &surf[j * work.n_active * work.stride..][..work.n_active * work.stride],
                            &active,
                            templates,
                            guess,
                            cfg,
                            work.side,
                            floor,
                            n_aps,
                        );
                        let plans = plan_fill(grid, templates, &row, guess, w, h);
                        FrameResult { row, plans, floor }
                    })
                    .collect();
                // Windows of interpolated points, scored in one more dispatch.
                let windows: Vec<(usize, Rect)> = results
                    .iter()
                    .enumerate()
                    .flat_map(|(j, r)| r.plans.iter().map(move |p| (j, p.window)))
                    .collect();
                t_fit += t_fit0.elapsed().as_secs_f64();
                let t_w = Instant::now();
                let submission = self.submit_window_sums(ctx, work, w, h, &windows);
                // The previous batch's sums were queued before this batch's
                // surfaces, so they are ready: collect them without stalling
                // the device behind the batch queued after this one.
                if let Some((pslot, prev)) = deferred.take() {
                    Self::finish_batch(ctx, &mut table, &works[pslot], prev)?;
                }
                deferred = Some((
                    slot,
                    Deferred {
                        start,
                        results,
                        n_windows: windows.len(),
                        submission,
                    },
                ));
                t_windows += t_w.elapsed().as_secs_f64();
                done += count;
                req.profile.add(Category::ApAlign, t.elapsed());
                (req.progress)(done as f32 / req.indices.len() as f32);
                Ok(())
            },
        )?;
        if let Some((pslot, prev)) = deferred.take() {
            Self::finish_batch(ctx, &mut table, &works[pslot], prev)?;
        }
        if trace {
            eprintln!(
                "points: {:.2}s total; device (upload, kernels, read-back) {t_device:.2}s, host fit {t_fit:.2}s, window sums {t_windows:.2}s",
                wall.elapsed().as_secs_f64()
            );
            trace::dump("points");
        }
        Ok(Some(table))
    }
}

/// A batch whose rows await their interpolated points' window sums.
struct Deferred {
    start: usize,
    results: Vec<FrameResult>,
    n_windows: usize,
    submission: Option<wgpu::SubmissionIndex>,
}

/// Measurements of the active points of one frame from their point records.
#[allow(clippy::too_many_arguments)] // flat inputs of one pure function
#[allow(clippy::cast_possible_wrap, reason = "the search side is at most 15")]
fn fit_frame(
    records: &[f32],
    active: &[usize],
    templates: &[Option<helia_core::align::multipoint::ApTemplate>],
    guess: Shift,
    cfg: &MultipointConfig,
    side: usize,
    floor: f32,
    n_aps: usize,
) -> Vec<Option<ApMeasure>> {
    let (gx, gy) = (guess.dx.round() as i64, guess.dy.round() as i64);
    let r = (side / 2) as i64;
    let mut row: Vec<Option<ApMeasure>> = vec![None; n_aps];
    for (c, &a) in active.iter().enumerate() {
        let Some(t) = templates[a].as_ref() else {
            continue;
        };
        let rec = &records[c * RECORD..(c + 1) * RECORD];
        if rec[1] < 0.5 {
            continue;
        }
        let bi = rec[0] as i64;
        let offset = (bi % side as i64 - r, bi / side as i64 - r);
        let nb: [f32; 9] = std::array::from_fn(|k| rec[5 + k]);
        if let Some((mut m, win)) = measurement_from_patch(t, &nb, offset, (gx, gy), cfg) {
            m.quality = combine_sums(
                Metric::GradientEnergy,
                win.area() as f64,
                f64::from(rec[3]),
                f64::from(rec[4]),
                0.0,
                floor,
            );
            row[a] = Some(m);
        }
    }
    row
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::AdapterRequest;
    use crate::score::floor_of;

    /// The noise floor chosen on the device is the host's, bit for bit, for
    /// frames with ties, infinities, NaNs, negative zeros and too few samples.
    #[test]
    fn device_noise_floor_is_the_host_noise_floor() {
        let hardware =
            std::env::var("HELIA_GPU_TESTS").is_ok_and(|m| m.eq_ignore_ascii_case("hardware"));
        let requests: &[AdapterRequest] = if hardware {
            &[AdapterRequest::Software, AdapterRequest::Hardware]
        } else {
            &[AdapterRequest::Software]
        };
        for &request in requests {
            let ctx = match GpuContext::new(request) {
                Ok(c) => c,
                Err(e) => {
                    // A hardware run on a platform with no software adapter
                    // (Metal) still tests the hardware device below.
                    assert!(
                        std::env::var_os("HELIA_GPU_TESTS").is_none()
                            || (hardware && request == AdapterRequest::Software),
                        "HELIA_GPU_TESTS is set but {request:?} did not open: {e}"
                    );
                    continue;
                }
            };
            let kernel = FloorKernel::new(&ctx);
            let mut seed = 0x9e37_79b9_u32;
            let mut next = move || {
                seed ^= seed << 13;
                seed ^= seed >> 17;
                seed ^= seed << 5;
                seed
            };
            // (width, height, kind of content)
            for (w, h, kind) in [
                (301usize, 203usize, 0u32),
                (301, 203, 1),
                (64, 48, 2),
                (9, 7, 0),
                (600, 400, 3),
                (50, 40, 4),
            ] {
                let frames = 3usize;
                let mut data = vec![0f32; frames * w * h];
                for v in &mut data {
                    let r = next();
                    *v = match kind {
                        0 => (r >> 8) as f32 / 16_777_216.0 * 4.0 - 1.0,
                        // Heavy ties.
                        1 => f32::from((r >> 24) as u8 % 7) * 0.25,
                        // Specials among ordinary values.
                        2 => match r % 11 {
                            0 => f32::NAN,
                            1 => f32::INFINITY,
                            2 => f32::NEG_INFINITY,
                            3 => -0.0,
                            4 => 0.0,
                            _ => (r >> 8) as f32 / 16_777_216.0 - 0.5,
                        },
                        // Wide dynamic range, both signs.
                        3 => {
                            let m = (r >> 9) as f32 / 8_388_608.0;
                            let e = i32::try_from(next() % 60).expect("small") - 30;
                            let sign = if next() & 1 == 0 { 1.0 } else { -1.0 };
                            sign * m * 2f32.powi(e)
                        }
                        // Every sample not finite.
                        _ => f32::NAN,
                    };
                }
                let cols = w.div_ceil(3);
                let samples = cols * h.div_ceil(3);
                let mono = ctx.storage("mono", (data.len() * 4) as u64, BufferUsages::COPY_DST);
                ctx.queue
                    .write_buffer(&mono, 0, bytemuck::cast_slice(&data));
                let base = frames * FLOOR_RECORD;
                let sel = ctx.storage("sel", ((base + frames) * 4) as u64, BufferUsages::COPY_SRC);
                let params = ctx.uniform(&[
                    w as u32,
                    h as u32,
                    frames as u32,
                    samples as u32,
                    cols as u32,
                    FLOOR_SLICES,
                    base as u32,
                    0,
                ]);
                let bind = kernel.bind(&ctx, &params, &mono, &sel);
                // Twice, to show the records are clear for the next batch.
                for _ in 0..2 {
                    let mut enc = ctx
                        .device
                        .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
                    kernel.encode(&mut enc, &bind, frames);
                    ctx.queue.submit(Some(enc.finish()));
                }
                let got = ctx.read_f32(&sel, base + frames).expect("read")[base..].to_vec();
                for f in 0..frames {
                    let plane = &data[f * w * h..(f + 1) * w * h];
                    let picked: Vec<f32> = (0..samples)
                        .map(|i| plane[(i / cols) * 3 * w + (i % cols) * 3])
                        .collect();
                    let want = floor_of(&picked);
                    assert_eq!(
                        got[f].to_bits(),
                        want.to_bits(),
                        "{request:?} {w}x{h} kind {kind} frame {f}: device {} host {want}",
                        got[f]
                    );
                }
            }
        }
    }
}
