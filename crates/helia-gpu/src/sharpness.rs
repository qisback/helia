// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Sharpness metrics on the GPU.

use std::sync::Mutex;

use helia_core::quality::{combine_sums, noise_floor};
use helia_core::{Error, Frame, Metric, Rect, Result};
use wgpu::{
    BindGroupDescriptor, BindGroupEntry, BindGroupLayout, BindGroupLayoutDescriptor,
    BindGroupLayoutEntry, BindingType, Buffer, BufferBindingType, BufferUsages,
    ComputePassDescriptor, ComputePipeline, ComputePipelineDescriptor, PipelineLayoutDescriptor,
    ShaderModuleDescriptor, ShaderSource, ShaderStages,
};

use crate::context::GpuContext;

const WORKGROUP: u32 = 16;
const PARAM_WORDS: usize = 8;

/// Buffers sized for one frame geometry, reused across calls.
struct Scratch {
    width: usize,
    height: usize,
    frame: Buffer,
    blurred: Buffer,
    params: Buffer,
    partials: Option<(usize, Buffer)>,
}

/// Compiled pipelines plus cached scratch buffers.
pub struct Sharpness {
    layout: BindGroupLayout,
    blur: ComputePipeline,
    reduce: ComputePipeline,
    scratch: Mutex<Option<Scratch>>,
}

impl Sharpness {
    pub fn new(ctx: &GpuContext) -> Self {
        let module = ctx.device.create_shader_module(ShaderModuleDescriptor {
            label: Some("sharpness"),
            source: ShaderSource::Wgsl(include_str!("shaders/sharpness.wgsl").into()),
        });
        // Explicit layout: each entry point uses a different subset of the bindings,
        // which automatic layouts would otherwise make incompatible.
        let storage = |binding: u32, read_only: bool| BindGroupLayoutEntry {
            binding,
            visibility: ShaderStages::COMPUTE,
            ty: BindingType::Buffer {
                ty: BufferBindingType::Storage { read_only },
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        };
        let layout = ctx
            .device
            .create_bind_group_layout(&BindGroupLayoutDescriptor {
                label: Some("sharpness"),
                entries: &[
                    BindGroupLayoutEntry {
                        binding: 0,
                        visibility: ShaderStages::COMPUTE,
                        ty: BindingType::Buffer {
                            ty: BufferBindingType::Uniform,
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    },
                    storage(1, true),
                    storage(2, false),
                    storage(3, false),
                ],
            });
        let pipeline_layout = ctx
            .device
            .create_pipeline_layout(&PipelineLayoutDescriptor {
                label: Some("sharpness"),
                bind_group_layouts: &[Some(&layout)],
                immediate_size: 0,
            });
        let make = |entry: &str| {
            ctx.device
                .create_compute_pipeline(&ComputePipelineDescriptor {
                    label: Some(entry),
                    layout: Some(&pipeline_layout),
                    module: &module,
                    entry_point: Some(entry),
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                    cache: None,
                })
        };
        Self {
            blur: make("blur_pass"),
            layout,
            reduce: make("reduce_pass"),
            scratch: Mutex::new(None),
        }
    }

    pub fn score(
        &self,
        ctx: &GpuContext,
        frame: &Frame,
        metric: Metric,
        roi: Option<Rect>,
    ) -> Result<f32> {
        if !metric.on_gpu() {
            return Err(Error::Unsupported(format!(
                "metric {} on the GPU",
                metric.name()
            )));
        }
        let (width, height) = (frame.width, frame.height);
        let region = roi.unwrap_or_else(|| frame.rect()).clamp(width, height);
        if region.is_empty() {
            return Ok(0.0);
        }
        let floor = noise_floor(frame);
        let mut guard = self
            .scratch
            .lock()
            .map_err(|_| Error::Backend("GPU state poisoned".into()))?;
        if !guard
            .as_ref()
            .is_some_and(|s| s.width == width && s.height == height)
        {
            *guard = Some(Scratch::new(ctx, width, height)?);
        }
        let scratch = guard
            .as_mut()
            .ok_or_else(|| Error::Backend("missing GPU scratch".into()))?;
        let sums = self.run(ctx, scratch, frame, metric, region)?;
        let (mut sum_mean, mut sum_a, mut sum_b) = (0.0f64, 0.0f64, 0.0f64);
        for group in sums.as_chunks::<3>().0 {
            sum_mean += f64::from(group[0]);
            sum_a += f64::from(group[1]);
            sum_b += f64::from(group[2]);
        }
        Ok(combine_sums(
            metric,
            region.area() as f64,
            sum_mean,
            sum_a,
            sum_b,
            floor,
        ))
    }

    /// Upload, blur, reduce and read back the per-workgroup partial sums.
    fn run(
        &self,
        ctx: &GpuContext,
        scratch: &mut Scratch,
        frame: &Frame,
        metric: Metric,
        region: Rect,
    ) -> Result<Vec<f32>> {
        let (gx, gy) = (
            (region.width() as u32).div_ceil(WORKGROUP),
            (region.height() as u32).div_ceil(WORKGROUP),
        );
        let groups = (gx * gy) as usize;
        if scratch.partials.as_ref().map(|p| p.0) != Some(groups) {
            scratch.partials = Some((
                groups,
                ctx.storage("partials", (groups * 3 * 4) as u64, BufferUsages::COPY_SRC),
            ));
        }
        let partials = &scratch
            .partials
            .as_ref()
            .ok_or_else(|| Error::Backend("missing partials".into()))?
            .1;
        let params: [u32; PARAM_WORDS] = [
            frame.width as u32,
            frame.height as u32,
            region.x0 as u32,
            region.y0 as u32,
            region.x1 as u32,
            region.y1 as u32,
            u32::from(metric == Metric::LaplacianVariance),
            0,
        ];
        ctx.queue
            .write_buffer(&scratch.frame, 0, bytemuck::cast_slice(&frame.data));
        ctx.queue
            .write_buffer(&scratch.params, 0, bytemuck::cast_slice(&params));
        let bind = ctx.device.create_bind_group(&BindGroupDescriptor {
            label: Some("sharpness"),
            layout: &self.layout,
            entries: &[
                BindGroupEntry {
                    binding: 0,
                    resource: scratch.params.as_entire_binding(),
                },
                BindGroupEntry {
                    binding: 1,
                    resource: scratch.frame.as_entire_binding(),
                },
                BindGroupEntry {
                    binding: 2,
                    resource: scratch.blurred.as_entire_binding(),
                },
                BindGroupEntry {
                    binding: 3,
                    resource: partials.as_entire_binding(),
                },
            ],
        });
        let mut enc = ctx
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        {
            let mut pass = enc.begin_compute_pass(&ComputePassDescriptor::default());
            pass.set_pipeline(&self.blur);
            pass.set_bind_group(0, &bind, &[]);
            pass.dispatch_workgroups(
                (frame.width as u32).div_ceil(WORKGROUP),
                (frame.height as u32).div_ceil(WORKGROUP),
                1,
            );
        }
        {
            // Separate pass so the blur is complete before the reduction reads it.
            let mut pass = enc.begin_compute_pass(&ComputePassDescriptor::default());
            pass.set_pipeline(&self.reduce);
            pass.set_bind_group(0, &bind, &[]);
            pass.dispatch_workgroups(gx, gy, 1);
        }
        ctx.queue.submit(Some(enc.finish()));
        Ok(ctx.read_f32(partials, groups * 3)?)
    }
}

impl Scratch {
    fn new(ctx: &GpuContext, width: usize, height: usize) -> Result<Self> {
        let bytes = (width * height * 4) as u64;
        if bytes > ctx.device.limits().max_storage_buffer_binding_size {
            return Err(Error::Backend(format!(
                "{width}x{height} frame exceeds the GPU storage buffer limit"
            )));
        }
        Ok(Self {
            width,
            height,
            frame: ctx.storage("frame", bytes, BufferUsages::COPY_DST),
            blurred: ctx.storage("blurred", bytes, BufferUsages::empty()),
            params: ctx.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("params"),
                size: (PARAM_WORDS * 4) as u64,
                usage: BufferUsages::UNIFORM | BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }),
            partials: None,
        })
    }
}
