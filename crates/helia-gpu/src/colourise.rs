// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Colour a mono image through a sampled colour table on the device.

use helia_core::{ColourLut, Error, Frame, Result, RgbImage};
use wgpu::{BufferUsages, CommandEncoderDescriptor};

use crate::context::GpuContext;
use crate::kit::{Kernel, Res};

/// Threads per workgroup in `colourise.wgsl`.
const GROUP: usize = 256;
/// Workgroups per dispatch row; 2-D dispatch keeps both below the 65 535 limit.
const ROW_GROUPS: usize = 4096;

/// The compiled kernel.
pub(crate) struct ColouriseKernel {
    kernel: Kernel,
}

impl ColouriseKernel {
    pub(crate) fn new(ctx: &GpuContext) -> Self {
        Self {
            kernel: Kernel::new(
                ctx,
                "colourise",
                include_str!("shaders/colourise.wgsl"),
                "main",
            ),
        }
    }

    /// Colour `frame` through `lut`; `None` when the buffers would not fit
    /// one storage binding, in which case the caller uses the CPU.
    pub(crate) fn run(
        &self,
        ctx: &GpuContext,
        frame: &Frame,
        lut: &ColourLut,
    ) -> Result<Option<RgbImage>> {
        let pixels = frame.data.len();
        let out_bytes = (pixels * 3 * 4) as u64;
        if pixels == 0 || out_bytes > ctx.max_binding() || lut.entries.len() < 2 {
            return Ok(None);
        }
        let host: Vec<f32> = frame
            .data
            .iter()
            .map(|&v| if v.is_finite() { v } else { 0.0 })
            .collect();
        let table: Vec<f32> = lut.entries.iter().flatten().copied().collect();
        let mono = ctx.storage("colour-mono", (pixels * 4) as u64, BufferUsages::COPY_DST);
        let lut_buf = ctx.storage(
            "colour-lut",
            (table.len() * 4) as u64,
            BufferUsages::COPY_DST,
        );
        let rgb = ctx.storage("colour-rgb", out_bytes, BufferUsages::COPY_SRC);
        ctx.queue
            .write_buffer(&mono, 0, bytemuck::cast_slice(&host));
        ctx.queue
            .write_buffer(&lut_buf, 0, bytemuck::cast_slice(&table));
        let groups = pixels.div_ceil(GROUP);
        let gx = groups.min(ROW_GROUPS);
        let gy = groups.div_ceil(gx);
        let params = ctx.uniform(&[
            pixels as u32,
            lut.entries.len() as u32,
            (gx * GROUP) as u32,
            0,
        ]);
        let bind = self.kernel.bind(
            ctx,
            &[
                Res::All(&params),
                Res::All(&mono),
                Res::All(&lut_buf),
                Res::All(&rgb),
            ],
        );
        let staging = ctx.staging(out_bytes);
        let mut enc = ctx
            .device
            .create_command_encoder(&CommandEncoderDescriptor::default());
        self.kernel
            .dispatch(&mut enc, &bind, (gx as u32, gy as u32, 1));
        enc.copy_buffer_to_buffer(&rgb, 0, &staging, 0, out_bytes);
        ctx.queue.submit(Some(enc.finish()));
        let data = ctx
            .map_f32(&staging, pixels * 3)
            .map_err(|e| Error::Backend(e.to_string()))?;
        Ok(Some(RgbImage {
            width: frame.width,
            height: frame.height,
            data,
        }))
    }
}
