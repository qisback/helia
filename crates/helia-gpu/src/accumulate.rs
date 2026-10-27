// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Shift-and-add accumulation on the GPU.

use std::sync::Arc;

use helia_core::resample::{Taps, split};
use helia_core::{Accumulated, Accumulator, Error, Frame, Kernel, Result, Shift};
use wgpu::{
    BindGroup, BindGroupDescriptor, BindGroupEntry, Buffer, BufferUsages, ComputePassDescriptor,
    ComputePipeline, ComputePipelineDescriptor, ShaderModuleDescriptor, ShaderSource,
};

use crate::context::GpuContext;

const WORKGROUP: u32 = 16;
/// Uniform block: 3 vec4 of scalars then 4 vec4 of tap weights.
const PARAM_WORDS: usize = 28;

/// Compiled accumulate pipeline.
pub struct Kernels {
    pipeline: Arc<ComputePipeline>,
}

impl Kernels {
    pub fn new(ctx: &GpuContext) -> Self {
        let module = ctx.device.create_shader_module(ShaderModuleDescriptor {
            label: Some("accumulate"),
            source: ShaderSource::Wgsl(include_str!("shaders/accumulate.wgsl").into()),
        });
        let pipeline = ctx
            .device
            .create_compute_pipeline(&ComputePipelineDescriptor {
                label: Some("accumulate"),
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
        width: usize,
        height: usize,
        kernel: Kernel,
    ) -> Result<GpuAccumulator> {
        let n = width * height;
        let limit = ctx.device.limits().max_storage_buffer_binding_size;
        if (n * 4) as u64 > limit {
            return Err(Error::Backend(format!(
                "{width}x{height} frame exceeds the GPU storage buffer limit"
            )));
        }
        let usage = BufferUsages::COPY_DST | BufferUsages::COPY_SRC;
        let frame_buf = ctx.storage("frame", (n * 4) as u64, BufferUsages::COPY_DST);
        let sum = ctx.storage("sum", (n * 4) as u64, usage);
        let weight = ctx.storage("weight", (n * 4) as u64, usage);
        let params = ctx.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("params"),
            size: (PARAM_WORDS * 4) as u64,
            usage: BufferUsages::UNIFORM | BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        // Buffers start zeroed in wgpu, so the sums need no clearing.
        let bind = ctx.device.create_bind_group(&BindGroupDescriptor {
            label: Some("accumulate"),
            layout: &self.pipeline.get_bind_group_layout(0),
            entries: &[
                BindGroupEntry {
                    binding: 0,
                    resource: params.as_entire_binding(),
                },
                BindGroupEntry {
                    binding: 1,
                    resource: frame_buf.as_entire_binding(),
                },
                BindGroupEntry {
                    binding: 2,
                    resource: sum.as_entire_binding(),
                },
                BindGroupEntry {
                    binding: 3,
                    resource: weight.as_entire_binding(),
                },
            ],
        });
        Ok(GpuAccumulator {
            ctx: Arc::clone(ctx),
            pipeline: Arc::clone(&self.pipeline),
            width,
            height,
            kernel,
            frame_buf,
            sum,
            weight,
            params,
            bind,
        })
    }
}

/// Running sum held in GPU memory.
pub struct GpuAccumulator {
    ctx: Arc<GpuContext>,
    pipeline: Arc<ComputePipeline>,
    width: usize,
    height: usize,
    kernel: Kernel,
    frame_buf: Buffer,
    sum: Buffer,
    weight: Buffer,
    params: Buffer,
    bind: BindGroup,
}

fn pack_params(
    w: usize,
    h: usize,
    shift: Shift,
    weight: f32,
    kernel: Kernel,
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
    for i in 0..6 {
        p[12 + i] = tx.w[i].to_bits();
        p[20 + i] = ty.w[i].to_bits();
    }
    p
}

impl Accumulator for GpuAccumulator {
    fn add(&mut self, frame: &Frame, shift: Shift, weight: f32) -> Result<()> {
        if frame.width != self.width || frame.height != self.height {
            return Err(Error::Invalid(
                "accumulated frame has a different size".into(),
            ));
        }
        let q = &self.ctx.queue;
        q.write_buffer(&self.frame_buf, 0, bytemuck::cast_slice(&frame.data));
        let params = pack_params(self.width, self.height, shift, weight, self.kernel);
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

    fn finish(self: Box<Self>) -> Result<Accumulated> {
        let n = self.width * self.height;
        let sum = self.ctx.read_f32(&self.sum, n)?;
        let weight = self.ctx.read_f32(&self.weight, n)?;
        Ok(Accumulated {
            width: self.width,
            height: self.height,
            sum,
            weight,
        })
    }
}
