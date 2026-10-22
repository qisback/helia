// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Adapter and device selection.

use std::sync::mpsc;
use std::sync::{Condvar, Mutex, PoisonError};

use wgpu::{
    Adapter, AdapterInfo, Backends, Buffer, BufferDescriptor, BufferUsages, Device, DeviceType,
    Instance, InstanceDescriptor, InstanceFlags, MapMode, PollType, PowerPreference, Queue,
    RequestAdapterOptions,
};

/// GPU set-up failures.
#[derive(Debug, thiserror::Error)]
pub enum GpuError {
    /// No adapter of the requested kind.
    #[error("no suitable GPU adapter found ({0})")]
    NoAdapter(String),
    /// The adapter refused to create a device.
    #[error("could not open GPU device: {0}")]
    Device(String),
    /// A buffer read-back failed.
    #[error("GPU read-back failed: {0}")]
    Readback(String),
}

impl From<GpuError> for helia_core::Error {
    fn from(e: GpuError) -> Self {
        Self::Backend(e.to_string())
    }
}

/// Which adapter to ask wgpu for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AdapterRequest {
    /// Best real adapter (high-performance preference) on Vulkan, DX12, Metal or GL.
    Hardware,
    /// wgpu's fallback adapter: WARP on Windows, lavapipe on Linux.
    Software,
}

/// Serialises Vulkan instance and device creation and teardown across the
/// whole process. The NVIDIA driver spins or faults when one thread tears a
/// device down while another creates one, which is what the parallel test
/// binaries did (issue #31).
///
/// A flag and a condition variable rather than a `MutexGuard`, because the
/// teardown token has to live inside [`GpuContext`] and stay `Send`.
static LIFECYCLE: (Mutex<bool>, Condvar) = (Mutex::new(false), Condvar::new());

/// Held while a device is created or torn down.
struct Lifecycle;

impl Lifecycle {
    fn acquire() -> Self {
        let (busy, cv) = &LIFECYCLE;
        let mut b = busy.lock().unwrap_or_else(PoisonError::into_inner);
        while *b {
            b = cv.wait(b).unwrap_or_else(PoisonError::into_inner);
        }
        *b = true;
        Self
    }
}

impl Drop for Lifecycle {
    fn drop(&mut self) {
        let (busy, cv) = &LIFECYCLE;
        *busy.lock().unwrap_or_else(PoisonError::into_inner) = false;
        cv.notify_one();
    }
}

/// True when `HELIA_GPU_DEBUG` asks for wgpu's debug names and validation.
fn debug_requested() -> bool {
    std::env::var_os("HELIA_GPU_DEBUG").is_some_and(|v| !v.is_empty() && v != "0")
}

/// The Vulkan (or other) instance for `backends`, created once and kept for the
/// life of the process.
///
/// Destroying the last instance makes the loader unload the driver libraries,
/// while a thread that ever called into a driver still carries thread-local
/// destructors that point into it; when that thread exits, it jumps into
/// unmapped memory and the process dies with SIGSEGV. That is the crash the
/// A100 showed at test exit and, in the API tests, as a thread dying mid-run
/// (issue #31). With the instance kept, the drivers stay loaded and
/// devices can come and go.
fn instance_for(backends: Backends) -> Instance {
    static INSTANCES: Mutex<Vec<(Backends, Instance)>> = Mutex::new(Vec::new());
    let mut all = INSTANCES.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some((_, i)) = all.iter().find(|(b, _)| *b == backends) {
        return i.clone();
    }
    let mut desc = InstanceDescriptor::new_without_display_handle();
    desc.backends = backends;
    // Debug builds default to VK_EXT_debug_utils, and the Vulkan loader's
    // vkSetDebugUtilsObjectNameEXT faults when several threads name buffers
    // at once (issue #173: SIGSEGV in libvulkan during parallel
    // tests). Object names and validation are for debugging the kernels, so
    // they are opt-in.
    if !debug_requested() {
        desc.flags
            .remove(InstanceFlags::DEBUG | InstanceFlags::VALIDATION);
    }
    let instance = Instance::new(desc);
    all.push((backends, instance.clone()));
    instance
}

/// An open device with its queue.
///
/// Dropping it waits for the device to go idle and then releases the device and
/// the adapter while holding the process-wide lifecycle lock, so a device is
/// never destroyed while another is being created. The instance it came from
/// stays alive (see `instance_for`).
pub struct GpuContext {
    pub(crate) device: Device,
    pub(crate) queue: Queue,
    info: AdapterInfo,
    // Declared after the device so they drop after it.
    _adapter: Adapter,
    _instance: Instance,
    // Last field: released only after everything above has been destroyed.
    #[allow(
        dead_code,
        reason = "held for its Drop, which releases the lifecycle lock"
    )]
    teardown: Option<Lifecycle>,
}

impl Drop for GpuContext {
    fn drop(&mut self) {
        self.teardown = Some(Lifecycle::acquire());
        // Let queued work finish so the driver is idle when it is destroyed.
        let _ = self.device.poll(PollType::Wait {
            submission_index: None,
            timeout: None,
        });
    }
}

impl GpuContext {
    /// Open a device.
    ///
    /// # Errors
    /// [`GpuError`] when no matching adapter exists or the device cannot be created.
    pub fn new(request: AdapterRequest) -> Result<Self, GpuError> {
        // Starting every backend costs the better part of a second (DXGI and
        // GL enumeration), so look for a real GPU on the native API first. The
        // software adapters (WARP on DX12, lavapipe on Vulkan) are primary.
        let searches: &[Backends] = match request {
            AdapterRequest::Hardware => &[
                Backends::VULKAN | Backends::METAL,
                Backends::PRIMARY | Backends::SECONDARY,
            ],
            AdapterRequest::Software => &[Backends::PRIMARY],
        };
        let _lock = Lifecycle::acquire();
        let mut last = GpuError::NoAdapter(format!("{request:?}: no backend tried"));
        for &backends in searches {
            let instance = instance_for(backends);
            if request == AdapterRequest::Hardware
                && let Some(n) = std::env::var("HELIA_GPU_DEVICE")
                    .ok()
                    .and_then(|v| v.trim().parse::<usize>().ok())
            {
                // The Nth real adapter, in the driver's order, for machines
                // with several GPUs where another job holds the first.
                let adapters = pollster::block_on(instance.enumerate_adapters(backends));
                let mut real = adapters
                    .into_iter()
                    .filter(|a| a.get_info().device_type != DeviceType::Cpu);
                if let Some(adapter) = real.nth(n) {
                    return Self::open_device(instance, adapter);
                }
                last = GpuError::NoAdapter(format!("HELIA_GPU_DEVICE={n}: no such adapter"));
                continue;
            }
            match pollster::block_on(instance.request_adapter(&RequestAdapterOptions {
                power_preference: PowerPreference::HighPerformance,
                force_fallback_adapter: request == AdapterRequest::Software,
                compatible_surface: None,
                apply_limit_buckets: false,
            })) {
                Ok(adapter) => return Self::open_device(instance, adapter),
                Err(e) => last = GpuError::NoAdapter(format!("{request:?}: {e}")),
            }
        }
        Err(last)
    }

    fn open_device(instance: Instance, adapter: Adapter) -> Result<Self, GpuError> {
        let info = adapter.get_info();
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("helia"),
            // Per-kernel timing for HELIA_GPU_TRACE, where the adapter has it.
            required_features: adapter.features() & wgpu::Features::TIMESTAMP_QUERY,
            required_limits: adapter.limits(),
            ..wgpu::DeviceDescriptor::default()
        }))
        .map_err(|e| GpuError::Device(e.to_string()))?;
        Ok(Self {
            device,
            queue,
            info,
            _adapter: adapter,
            _instance: instance,
            teardown: None,
        })
    }

    /// The adapter's identity: name, backend and device type.
    pub(crate) fn adapter_info(&self) -> &AdapterInfo {
        &self.info
    }

    /// `name (backend, device type)`.
    #[must_use]
    pub fn summary(&self) -> String {
        format!(
            "{} ({:?}, {:?})",
            self.info.name, self.info.backend, self.info.device_type
        )
    }

    /// True for CPU-hosted adapters.
    #[must_use]
    pub fn is_software(&self) -> bool {
        self.info.device_type == DeviceType::Cpu
    }

    /// The wgpu device, for kernels written outside this crate (Valerian's).
    ///
    /// Keep the context alive as long as anything made from the device is in
    /// use: dropping it waits for the device to go idle and then releases it.
    #[must_use]
    pub fn device(&self) -> &Device {
        &self.device
    }

    /// The device's queue, for the same callers as [`GpuContext::device`].
    #[must_use]
    pub fn queue(&self) -> &Queue {
        &self.queue
    }

    /// Create a storage buffer.
    pub(crate) fn storage(&self, label: &str, size: u64, extra: BufferUsages) -> Buffer {
        self.device.create_buffer(&BufferDescriptor {
            label: Some(label),
            size: size.max(4),
            usage: BufferUsages::STORAGE | extra,
            mapped_at_creation: false,
        })
    }

    /// Copy `src` (which needs `COPY_SRC`) to the host as f32s.
    pub(crate) fn read_f32(&self, src: &Buffer, len: usize) -> Result<Vec<f32>, GpuError> {
        let bytes = (len * 4) as u64;
        let staging = self.device.create_buffer(&BufferDescriptor {
            label: Some("readback"),
            size: bytes.max(4),
            usage: BufferUsages::MAP_READ | BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let mut enc = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        enc.copy_buffer_to_buffer(src, 0, &staging, 0, bytes);
        self.queue.submit(Some(enc.finish()));
        let (tx, rx) = mpsc::channel();
        staging.slice(..bytes).map_async(MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        self.device
            .poll(PollType::Wait {
                submission_index: None,
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
}
