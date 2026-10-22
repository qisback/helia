// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Two-pass robust shift-and-add on the GPU, see
//! [`helia_core::RobustAccumulator`].

use std::sync::Arc;

use helia_core::resample::{Taps, split};
use helia_core::stack::RobustKind;
use helia_core::{Error, Frame, Kernel, Result, RobustAccumulator, Shift};
use wgpu::{
    BindGroup, BindGroupDescriptor, BindGroupEntry, Buffer, BufferUsages, ComputePassDescriptor,
    ComputePipeline, ComputePipelineDescriptor, ShaderModuleDescriptor, ShaderSource,
};

use crate::context::GpuContext;

const WORKGROUP: u32 = 16;
const PARAM_WORDS: usize = 28;

fn entry(binding: u32, buffer: &Buffer) -> BindGroupEntry<'_> {
    BindGroupEntry {
        binding,
        resource: buffer.as_entire_binding(),
    }
}

/// Compiled robust accumulate pipeline.
pub struct RobustKernels {
    pipeline: Arc<ComputePipeline>,
}

impl RobustKernels {
    pub fn new(ctx: &GpuContext) -> Self {
        let module = ctx.device.create_shader_module(ShaderModuleDescriptor {
            label: Some("robust_accumulate"),
            source: ShaderSource::Wgsl(include_str!("shaders/robust_accumulate.wgsl").into()),
        });
        let pipeline = ctx
            .device
            .create_compute_pipeline(&ComputePipelineDescriptor {
                label: Some("robust_accumulate"),
                layout: None,
                module: &module,
                entry_point: Some("main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                cache: None,
            });
        Self {
            pipeline: Arc::new(pipeline),
        }
    }

    pub fn start(
        &self,
        ctx: &Arc<GpuContext>,
        centre: &Frame,
        kernel: Kernel,
        kind: RobustKind,
        k: f32,
    ) -> Result<GpuRobustAccumulator> {
        let (width, height) = (centre.width, centre.height);
        let n = width * height;
        let limit = ctx.device.limits().max_storage_buffer_binding_size;
        if (n * 32) as u64 > limit {
            return Err(Error::Backend(format!(
                "{width}x{height} frame exceeds the GPU storage buffer limit"
            )));
        }
        let usage = BufferUsages::COPY_DST | BufferUsages::COPY_SRC;
        let frame_buf = ctx.storage("frame", (n * 4) as u64, BufferUsages::COPY_DST);
        let centre_buf = ctx.storage("centre", (n * 4) as u64, BufferUsages::COPY_DST);
        ctx.queue
            .write_buffer(&centre_buf, 0, bytemuck::cast_slice(&centre.data));
        // Buffers start zeroed in wgpu, so the sums need no clearing.
        let stats = ctx.storage("stats", (n * 32) as u64, usage);
        let robust = ctx.storage("robust", (n * 8) as u64, usage);
        let params = ctx.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("params"),
            size: (PARAM_WORDS * 4) as u64,
            usage: BufferUsages::UNIFORM | BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bind = ctx.device.create_bind_group(&BindGroupDescriptor {
            label: Some("robust_accumulate"),
            layout: &self.pipeline.get_bind_group_layout(0),
            entries: &[
                entry(0, &params),
                entry(1, &frame_buf),
                entry(2, &centre_buf),
                entry(3, &stats),
                entry(4, &robust),
            ],
        });
        Ok(GpuRobustAccumulator {
            ctx: Arc::clone(ctx),
            pipeline: Arc::clone(&self.pipeline),
            width,
            height,
            kernel,
            kind,
            k,
            second: false,
            centre: centre.data.clone(),
            frame_buf,
            stats,
            robust,
            params,
            bind,
        })
    }
}

/// Statistics and robust sums held in GPU memory.
pub struct GpuRobustAccumulator {
    ctx: Arc<GpuContext>,
    pipeline: Arc<ComputePipeline>,
    width: usize,
    height: usize,
    kernel: Kernel,
    kind: RobustKind,
    k: f32,
    second: bool,
    centre: Vec<f32>,
    frame_buf: Buffer,
    stats: Buffer,
    robust: Buffer,
    params: Buffer,
    bind: BindGroup,
}

/// Uniform block: the scalars of `accumulate.wgsl` with `mode`, `kind` and `k`
/// in the words that one leaves as padding.
fn pack_params(
    (w, h): (usize, usize),
    shift: Shift,
    weight: f32,
    kernel: Kernel,
    (mode, kind, k): (u32, u32, f32),
) -> [u32; PARAM_WORDS] {
    let (ox, fx) = split(shift.dx);
    let (oy, fy) = split(shift.dy);
    let tx = Taps::new(kernel, fx);
    let ty = Taps::new(kernel, fy);
    let mut p = [0u32; PARAM_WORDS];
    p[0] = w as u32;
    p[1] = h as u32;
    p[2] = ox as i32 as u32;
    p[3] = oy as i32 as u32;
    p[4] = shift.dx.to_bits();
    p[5] = shift.dy.to_bits();
    p[6] = weight.to_bits();
    p[7] = tx.n as u32;
    p[8] = tx.lo as i32 as u32;
    p[9] = mode;
    p[10] = kind;
    p[11] = k.to_bits();
    for i in 0..6 {
        p[12 + i] = tx.w[i].to_bits();
        p[20 + i] = ty.w[i].to_bits();
    }
    p
}

impl GpuRobustAccumulator {
    fn deposit(&self, frame: &Frame, shift: Shift, weight: f32, mode: u32) -> Result<()> {
        if frame.width != self.width || frame.height != self.height {
            return Err(Error::Invalid(
                "accumulated frame has a different size".into(),
            ));
        }
        let kind = match self.kind {
            RobustKind::Sigma => 0,
            RobustKind::Huber => 1,
            RobustKind::Winsor => 2,
        };
        let q = &self.ctx.queue;
        q.write_buffer(&self.frame_buf, 0, bytemuck::cast_slice(&frame.data));
        let params = pack_params(
            (self.width, self.height),
            shift,
            weight,
            self.kernel,
            (mode, kind, self.k),
        );
        q.write_buffer(&self.params, 0, bytemuck::cast_slice(&params));
        let mut enc = self
            .ctx
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        {
            let mut pass = enc.begin_compute_pass(&ComputePassDescriptor::default());
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &self.bind, &[]);
            pass.dispatch_workgroups(
                (self.width as u32).div_ceil(WORKGROUP),
                (self.height as u32).div_ceil(WORKGROUP),
                1,
            );
        }
        q.submit(Some(enc.finish()));
        Ok(())
    }
}

impl RobustAccumulator for GpuRobustAccumulator {
    fn add_stats(&mut self, frame: &Frame, shift: Shift, weight: f32) -> Result<()> {
        self.deposit(frame, shift, weight, 0)
    }

    fn begin_reject(&mut self) -> Result<()> {
        self.second = true;
        Ok(())
    }

    fn add_robust(&mut self, frame: &Frame, shift: Shift, weight: f32) -> Result<()> {
        if !self.second {
            return Err(Error::Invalid("begin_reject was not called".into()));
        }
        self.deposit(frame, shift, weight, 1)
    }

    fn finish(self: Box<Self>) -> Result<Frame> {
        let n = self.width * self.height;
        let stats = self.ctx.read_f32(&self.stats, n * 8)?;
        let robust = self.ctx.read_f32(&self.robust, n * 2)?;
        let data = (0..n)
            .map(|i| {
                // As TileStack::finish: if the second pass kept nothing, the
                // plain mean stands.
                let (s, w) = if robust[2 * i + 1] > 0.0 {
                    (robust[2 * i], robust[2 * i + 1])
                } else {
                    (stats[8 * i], stats[8 * i + 2])
                };
                if w > 0.0 {
                    self.centre[i] + s / w
                } else {
                    self.centre[i]
                }
            })
            .collect();
        Ok(Frame {
            width: self.width,
            height: self.height,
            data,
            bayer: None,
        })
    }
}
