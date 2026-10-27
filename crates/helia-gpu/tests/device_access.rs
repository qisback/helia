// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! The device and queue are public, so a program outside this crate can run its
//! own kernel on them.
//!
//! `HELIA_GPU_TESTS` selects the adapter as in `parity.rs`.

use helia_gpu::wgpu::{self, util::DeviceExt};
use helia_gpu::{AdapterRequest, GpuContext};

fn request() -> Option<AdapterRequest> {
    match std::env::var("HELIA_GPU_TESTS")
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "" | "none" => {
            eprintln!("HELIA_GPU_TESTS is unset or 'none': skipping GPU tests");
            None
        }
        "software" => Some(AdapterRequest::Software),
        "hardware" => Some(AdapterRequest::Hardware),
        other => panic!("HELIA_GPU_TESTS={other:?}: expected software, hardware or none"),
    }
}

const DOUBLE: &str = "
@group(0) @binding(0) var<storage, read_write> data: array<f32>;
@compute @workgroup_size(64)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    if (id.x < arrayLength(&data)) { data[id.x] = data[id.x] * 2.0; }
}
";

#[test]
fn a_caller_can_run_its_own_kernel_on_the_device() {
    let Some(request) = request() else { return };
    let ctx = GpuContext::new(request).unwrap_or_else(|e| panic!("no adapter: {e}"));
    let (device, queue) = (ctx.device(), ctx.queue());
    let input: Vec<f32> = (0..1000).map(|i| i as f32).collect();
    let buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("data"),
        contents: bytemuck::cast_slice(&input),
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
    });
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("double"),
        source: wgpu::ShaderSource::Wgsl(DOUBLE.into()),
    });
    let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some("double"),
        layout: None,
        module: &module,
        entry_point: Some("main"),
        compilation_options: wgpu::PipelineCompilationOptions::default(),
        cache: None,
    });
    let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: None,
        layout: &pipeline.get_bind_group_layout(0),
        entries: &[wgpu::BindGroupEntry {
            binding: 0,
            resource: buffer.as_entire_binding(),
        }],
    });
    let staging = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("staging"),
        size: 4000,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
    {
        let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor::default());
        pass.set_pipeline(&pipeline);
        pass.set_bind_group(0, &group, &[]);
        pass.dispatch_workgroups(16, 1, 1);
    }
    enc.copy_buffer_to_buffer(&buffer, 0, &staging, 0, 4000);
    queue.submit(Some(enc.finish()));
    let (tx, rx) = std::sync::mpsc::channel();
    staging.slice(..).map_async(wgpu::MapMode::Read, move |r| {
        let _ = tx.send(r);
    });
    device
        .poll(wgpu::PollType::Wait {
            submission_index: None,
            timeout: None,
        })
        .expect("poll");
    rx.recv().expect("map").expect("mapped");
    let bytes = staging.slice(..).get_mapped_range().expect("mapped range");
    let out: Vec<f32> = bytemuck::pod_collect_to_vec(&bytes);
    assert_eq!(out.len(), 1000);
    assert!(
        out.iter()
            .enumerate()
            .all(|(i, &v)| (v - 2.0 * i as f32).abs() < 1e-6)
    );
}
