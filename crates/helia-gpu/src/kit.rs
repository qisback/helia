// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Small helpers shared by the GPU stages: compiled kernels, bind groups,
//! staged read-back and a threaded reader of raw frame batches.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::Instant;

use helia_core::io::raw::SharedFrames;
use helia_core::profile::{Category, Profile};
use helia_core::{Error, Result};
use rayon::prelude::*;
use wgpu::{
    BindGroup, BindGroupDescriptor, BindGroupEntry, Buffer, BufferDescriptor, BufferUsages,
    CommandEncoder, ComputePipeline, ComputePipelineDescriptor, MapMode, PollType,
    ShaderModuleDescriptor, ShaderSource,
};

use crate::context::{GpuContext, GpuError};
use crate::resident::Resident;

/// One batch of frames as the readers hand them to the stages: the stored
/// bytes of each frame, or for a frame already on the device the slot that
/// holds it (its byte buffer is then empty).
pub(crate) struct RawBatch {
    /// Stored bytes, padded to a multiple of four; empty for resident frames.
    pub bufs: Vec<Vec<u8>>,
    /// File index of each frame.
    pub indices: Vec<usize>,
    /// Resident slot of each frame, when it has one.
    pub slots: Vec<Option<u32>>,
}

impl RawBatch {
    /// Frames in the batch.
    pub(crate) fn len(&self) -> usize {
        self.indices.len()
    }
}

/// One compute entry point with an automatic bind group layout.
pub(crate) struct Kernel {
    pipeline: ComputePipeline,
    label: String,
}

/// What a binding points at.
pub(crate) enum Res<'a> {
    /// The whole buffer.
    All(&'a Buffer),
}

impl Kernel {
    /// Compile `wgsl` and select `entry`.
    pub(crate) fn new(ctx: &GpuContext, label: &str, wgsl: &str, entry: &str) -> Self {
        let module = ctx.device.create_shader_module(ShaderModuleDescriptor {
            label: Some(label),
            source: ShaderSource::Wgsl(wgsl.into()),
        });
        let pipeline = ctx
            .device
            .create_compute_pipeline(&ComputePipelineDescriptor {
                label: Some(label),
                layout: None,
                module: &module,
                entry_point: Some(entry),
                // Every kernel writes its workgroup arrays before reading them.
                // Zero-filling a 16 KB array made the WARP adapter compile the
                // FFT kernels for 40 s each (and costs every workgroup start).
                compilation_options: wgpu::PipelineCompilationOptions {
                    zero_initialize_workgroup_memory: false,
                    ..wgpu::PipelineCompilationOptions::default()
                },
                cache: None,
            });
        Self {
            pipeline,
            label: label.to_string(),
        }
    }

    /// Bind `resources[i]` to binding `i`.
    pub(crate) fn bind(&self, ctx: &GpuContext, resources: &[Res<'_>]) -> BindGroup {
        let entries: Vec<BindGroupEntry<'_>> = resources
            .iter()
            .enumerate()
            .map(|(i, r)| BindGroupEntry {
                binding: i as u32,
                resource: match r {
                    Res::All(b) => b.as_entire_binding(),
                },
            })
            .collect();
        ctx.device.create_bind_group(&BindGroupDescriptor {
            label: None,
            layout: &self.pipeline.get_bind_group_layout(0),
            entries: &entries,
        })
    }

    /// Record one dispatch of `groups` workgroups in its own pass, so the
    /// previous pass's writes are visible to it.
    pub(crate) fn dispatch(
        &self,
        enc: &mut CommandEncoder,
        bind: &BindGroup,
        groups: (u32, u32, u32),
    ) {
        gpu_trace::pass(enc, &self.label, |mut pass| {
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, bind, &[]);
            pass.dispatch_workgroups(groups.0, groups.1, groups.2);
        });
    }
}

/// Per-kernel GPU time for `HELIA_GPU_TRACE=1`, from timestamp queries around
/// every pass.
pub(crate) mod gpu_trace {
    use std::collections::BTreeMap;
    use std::sync::{Mutex, OnceLock};

    use wgpu::{
        BufferDescriptor, BufferUsages, CommandEncoder, ComputePass, ComputePassDescriptor,
        ComputePassTimestampWrites, Device, MapMode, PollType, QuerySet, QuerySetDescriptor,
        QueryType, Queue,
    };

    const CAPACITY: u32 = 4096;

    struct State {
        device: Device,
        queue: Queue,
        queries: QuerySet,
        used: u32,
        labels: Vec<String>,
    }

    fn state() -> &'static Mutex<Option<State>> {
        static S: OnceLock<Mutex<Option<State>>> = OnceLock::new();
        S.get_or_init(|| Mutex::new(None))
    }

    /// Give the tracer the device whose passes it should time.
    pub(crate) fn attach(device: &Device, queue: &Queue) {
        if !super::trace::enabled() || !device.features().contains(wgpu::Features::TIMESTAMP_QUERY)
        {
            return;
        }
        if let Ok(mut s) = state().lock() {
            *s = Some(State {
                device: device.clone(),
                queue: queue.clone(),
                queries: device.create_query_set(&QuerySetDescriptor {
                    label: Some("trace"),
                    ty: QueryType::Timestamp,
                    count: CAPACITY,
                }),
                used: 0,
                labels: Vec::new(),
            });
        }
    }

    /// Record a compute pass, timed when tracing is attached.
    pub(crate) fn pass(
        enc: &mut CommandEncoder,
        label: &str,
        record: impl FnOnce(ComputePass<'_>),
    ) {
        let guard = state().lock().ok();
        let slot = guard.as_ref().and_then(|g| g.as_ref());
        let Some(st) = slot.filter(|st| st.used + 2 <= CAPACITY) else {
            drop(guard);
            record(enc.begin_compute_pass(&ComputePassDescriptor::default()));
            return;
        };
        let first = st.used;
        let writes = ComputePassTimestampWrites {
            query_set: &st.queries,
            beginning_of_pass_write_index: Some(first),
            end_of_pass_write_index: Some(first + 1),
        };
        record(enc.begin_compute_pass(&ComputePassDescriptor {
            label: None,
            timestamp_writes: Some(writes),
        }));
        drop(guard);
        if let Ok(mut g) = state().lock()
            && let Some(st) = g.as_mut()
        {
            st.used += 2;
            st.labels.push(label.to_string());
        }
    }

    /// Print the summed time per kernel since the last dump and clear it.
    pub(crate) fn dump(title: &str) {
        let Ok(mut g) = state().lock() else { return };
        let Some(st) = g.as_mut().filter(|st| st.used > 0) else {
            return;
        };
        let bytes = u64::from(st.used) * 8;
        let resolve = st.device.create_buffer(&BufferDescriptor {
            label: None,
            size: bytes,
            usage: BufferUsages::QUERY_RESOLVE | BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        let staging = st.device.create_buffer(&BufferDescriptor {
            label: None,
            size: bytes,
            usage: BufferUsages::MAP_READ | BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let mut enc = st
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        enc.resolve_query_set(&st.queries, 0..st.used, &resolve, 0);
        enc.copy_buffer_to_buffer(&resolve, 0, &staging, 0, bytes);
        st.queue.submit(Some(enc.finish()));
        let (tx, rx) = std::sync::mpsc::channel();
        staging.slice(..).map_async(MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        let _ = st.device.poll(PollType::Wait {
            submission_index: None,
            timeout: None,
        });
        if rx.recv().is_ok_and(|r| r.is_ok()) {
            let data = staging.slice(..).get_mapped_range();
            if let Ok(data) = data {
                let ticks: Vec<u64> = data
                    .as_chunks::<8>()
                    .0
                    .iter()
                    .map(|c| u64::from_le_bytes(*c))
                    .collect();
                let period = f64::from(st.queue.get_timestamp_period());
                let mut per: BTreeMap<&str, (f64, u32)> = BTreeMap::new();
                for (i, label) in st.labels.iter().enumerate() {
                    let (a, b) = (ticks[2 * i], ticks[2 * i + 1]);
                    let e = per.entry(label).or_insert((0.0, 0));
                    e.0 += b.saturating_sub(a) as f64 * period * 1e-9;
                    e.1 += 1;
                }
                let line: Vec<String> = per
                    .iter()
                    .map(|(k, (secs, n))| format!("{k} {secs:.3}s/{n}"))
                    .collect();
                eprintln!("gpu kernels {title}: {}", line.join(", "));
            }
            staging.unmap();
        }
        st.used = 0;
        st.labels.clear();
    }
}

/// Wall-time accounting for `HELIA_GPU_TRACE=1`: seconds per named phase.
pub(crate) mod trace {
    use std::collections::BTreeMap;
    use std::sync::{Mutex, OnceLock};
    use std::time::Instant;

    pub(crate) fn enabled() -> bool {
        static ON: OnceLock<bool> = OnceLock::new();
        *ON.get_or_init(|| std::env::var_os("HELIA_GPU_TRACE").is_some())
    }

    fn table() -> &'static Mutex<BTreeMap<&'static str, f64>> {
        static T: OnceLock<Mutex<BTreeMap<&'static str, f64>>> = OnceLock::new();
        T.get_or_init(|| Mutex::new(BTreeMap::new()))
    }

    /// Time `f` under `name` when tracing is on.
    pub(crate) fn time<T>(name: &'static str, f: impl FnOnce() -> T) -> T {
        if !enabled() {
            return f();
        }
        let t = Instant::now();
        let v = f();
        if let Ok(mut m) = table().lock() {
            *m.entry(name).or_insert(0.0) += t.elapsed().as_secs_f64();
        }
        v
    }

    /// Print and clear the table, headed by `title`, with the per-kernel GPU
    /// times gathered since the last dump.
    pub(crate) fn dump(title: &str) {
        if !enabled() {
            return;
        }
        super::gpu_trace::dump(title);
        if let Ok(mut m) = table().lock() {
            let line: Vec<String> = m.iter().map(|(k, v)| format!("{k} {v:.2}s")).collect();
            eprintln!("trace {title}: {}", line.join(", "));
            m.clear();
        }
    }
}

/// `n / d` rounded up, as a dispatch size.
pub(crate) fn groups(n: usize, d: usize) -> u32 {
    n.div_ceil(d) as u32
}

impl GpuContext {
    /// Uniform buffer holding `words`.
    pub(crate) fn uniform(&self, words: &[u32]) -> Buffer {
        let buf = self.device.create_buffer(&BufferDescriptor {
            label: Some("params"),
            size: (words.len() * 4).max(16) as u64,
            usage: BufferUsages::UNIFORM | BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        self.queue
            .write_buffer(&buf, 0, bytemuck::cast_slice(words));
        buf
    }

    /// Buffer for the GPU to write and the host to read back.
    pub(crate) fn staging(&self, bytes: u64) -> Buffer {
        self.device.create_buffer(&BufferDescriptor {
            label: Some("staging"),
            size: bytes.max(4),
            usage: BufferUsages::MAP_READ | BufferUsages::COPY_DST,
            mapped_at_creation: false,
        })
    }

    /// Map `staging` (after the submission that fills it) and copy out `len` values.
    pub(crate) fn map_f32(
        &self,
        staging: &Buffer,
        len: usize,
    ) -> std::result::Result<Vec<f32>, GpuError> {
        self.map_f32_after(staging, len, None)
    }

    /// As [`Self::map_f32`], waiting only for `submission` (and so not for
    /// work queued after it) when it is given.
    pub(crate) fn map_f32_after(
        &self,
        staging: &Buffer,
        len: usize,
        submission: Option<wgpu::SubmissionIndex>,
    ) -> std::result::Result<Vec<f32>, GpuError> {
        let bytes = (len * 4) as u64;
        let (tx, rx) = mpsc::channel();
        staging.slice(..bytes).map_async(MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        self.device
            .poll(PollType::Wait {
                submission_index: submission,
                timeout: None,
            })
            .map_err(|e| GpuError::Readback(e.to_string()))?;
        rx.recv()
            .map_err(|e| GpuError::Readback(e.to_string()))?
            .map_err(|e| GpuError::Readback(e.to_string()))?;
        let data = staging
            .slice(..bytes)
            .get_mapped_range()
            .map_err(|e| GpuError::Readback(e.to_string()))?;
        let out = bytemuck::cast_slice::<u8, f32>(&data).to_vec();
        drop(data);
        staging.unmap();
        Ok(out)
    }

    /// Largest storage binding, bytes.
    pub(crate) fn max_binding(&self) -> u64 {
        self.device.limits().max_storage_buffer_binding_size
    }
}

/// Like [`for_each_raw_batch`] with two batches in flight: `submit` queues
/// work for a batch in `slot` (0 or 1, alternating) and returns what `finish`
/// needs to collect it; `finish(slot, start, count, pending)` for batch `i` runs
/// after `submit` of batch `i + 1`, so the device works on one batch while the
/// host finishes the previous one. Each slot must own its device buffers.
#[allow(
    clippy::too_many_arguments,
    reason = "the reader's parameters plus the two stage callbacks"
)]
pub(crate) fn for_each_batch_overlapped<P>(
    frames: &dyn SharedFrames,
    indices: &[usize],
    batch: usize,
    profile: &Profile,
    cancel: &AtomicBool,
    resident: Option<&Resident>,
    mut submit: impl FnMut(usize, usize, &RawBatch) -> Result<P>,
    mut finish: impl FnMut(usize, usize, usize, P) -> Result<()>,
) -> Result<()> {
    let mut pending: Option<(usize, usize, usize, P)> = None;
    let mut n = 0usize;
    for_each_raw_batch(
        frames,
        indices,
        batch,
        profile,
        cancel,
        resident,
        |start, raw| {
            let slot = n % 2;
            n += 1;
            let p = submit(slot, start, raw)?;
            if let Some((s, st, c, prev)) = pending.replace((slot, start, raw.len(), p)) {
                finish(s, st, c, prev)?;
            }
            Ok(())
        },
    )?;
    if let Some((s, st, c, prev)) = pending.take() {
        finish(s, st, c, prev)?;
    }
    Ok(())
}

/// Reads the stored bytes of `indices` in batches of up to `batch` frames on
/// a helper thread (frames of a batch in parallel) while `consume` works on
/// the previous batch, so file reads overlap GPU work. Buffers are recycled.
///
/// With a `resident` store, frames it holds are not read from the file (the
/// batch names their slot instead) and frames it lacks are read and then
/// offered to it by [`crate::decode::Decoder::upload_and_encode`].
///
/// `consume(start, batch)` receives the batch beginning at `indices[start]`.
pub(crate) fn for_each_raw_batch(
    frames: &dyn SharedFrames,
    indices: &[usize],
    batch: usize,
    profile: &Profile,
    cancel: &AtomicBool,
    resident: Option<&Resident>,
    mut consume: impl FnMut(usize, &RawBatch) -> Result<()>,
) -> Result<()> {
    type Msg = Result<(usize, RawBatch)>;
    let (tx, rx) = mpsc::sync_channel::<Msg>(2);
    let (back_tx, back_rx) = mpsc::channel::<Vec<Vec<u8>>>();
    if let Some(r) = resident {
        r.begin_stage(indices);
    }
    std::thread::scope(|scope| {
        scope.spawn(move || {
            for (ci, chunk) in indices.chunks(batch).enumerate() {
                if cancel.load(Ordering::Relaxed) {
                    let _ = tx.send(Err(Error::Cancelled));
                    return;
                }
                let mut bufs = back_rx.try_recv().unwrap_or_default();
                bufs.resize_with(chunk.len(), Vec::new);
                let slots: Vec<Option<u32>> = chunk
                    .iter()
                    .map(|&i| resident.and_then(|r| r.lookup(i)))
                    .collect();
                let t = Instant::now();
                let read: Result<()> =
                    bufs.par_iter_mut()
                        .zip(chunk)
                        .zip(&slots)
                        .try_for_each(|((b, &i), slot)| {
                            if slot.is_some() {
                                b.clear();
                                return Ok(());
                            }
                            frames.read_raw(i, b)?;
                            // Buffer writes must be a multiple of four bytes.
                            b.resize(b.len().next_multiple_of(4), 0);
                            Ok(())
                        });
                profile.add(Category::Decode, t.elapsed());
                let msg = read.map(|()| {
                    (
                        ci * batch,
                        RawBatch {
                            bufs,
                            indices: chunk.to_vec(),
                            slots,
                        },
                    )
                });
                let failed = msg.is_err();
                if tx.send(msg).is_err() || failed {
                    return;
                }
            }
        });
        let mut result = Ok(());
        // Time spent receiving is time the device side waited for the file.
        while let Ok(msg) = trace::time("wait for reader", || rx.recv()) {
            match msg.and_then(|(start, raw)| {
                let done = trace::time("consume", || consume(start, &raw));
                if let Some(r) = resident {
                    if done.is_ok() {
                        r.commit();
                    } else {
                        r.rollback();
                    }
                }
                done?;
                let _ = back_tx.send(raw.bufs);
                Ok(())
            }) {
                Ok(()) => {}
                Err(e) => {
                    result = Err(e);
                    break;
                }
            }
        }
        if let Some(r) = resident.filter(|_| trace::enabled()) {
            let (hit, miss) = r.take_counts();
            let (frames, bytes) = r.usage();
            eprintln!(
                "resident: {hit} frames from the device, {miss} from the file; holds {frames} frames, {} MiB",
                bytes >> 20
            );
        }
        // Dropping the receiver ends the reader if it is still sending.
        result
    })
}
