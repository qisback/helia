// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Raw frames kept on the device between the stages of one run.
//!
//! The stages (score, register, alignment points, stack) each read the same
//! frames. Without this, every stage reads them from the file and uploads them
//! again. With it, a frame is uploaded the first time any stage needs it and
//! copied from device memory by every later stage, so the file is read and the
//! bus crossed once.
//!
//! What is kept are the stored bytes, not decoded frames: the decode kernel
//! runs on them every time, so the result is bit for bit what the uploading
//! path gives, and a frame costs a quarter of its decoded size for 8-bit data.
//!
//! The store is bounded by a byte budget and is a cache, not a requirement:
//! a frame that does not fit is read from the file as before. Which frames are
//! kept follows the stage in progress: frames the current stage will use are
//! never evicted for ones it will not, so after scoring has filled the store
//! first come first served, the later stages swap in the frames they need.
//!
//! The store hangs off the source ([`DeviceCache`]), so it lives and dies with
//! the capture it holds frames of.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use helia_core::io::raw::SharedFrames;
use wgpu::{Buffer, BufferUsages, DeviceType, ErrorFilter};

use crate::context::GpuContext;

/// Target size of one device buffer of the store, bytes.
const CHUNK_BYTES: u64 = 128 << 20;

/// Most a discrete adapter's store may take unless asked for more, bytes. It
/// holds the largest capture the benchmarks use (5.4 GiB) on a card of 8 GB
/// or more.
const DISCRETE_BUDGET: u64 = 6 << 30;

/// Device memory left free when the device runs out while the store grows, so
/// the stages' own buffers still fit, bytes.
const HEADROOM: u64 = 1536 << 20;

/// The environment variable that overrides the budget, in MiB (0 turns the
/// store off).
pub const BUDGET_ENV: &str = "HELIA_GPU_RESIDENT_MB";

/// Raw frames of one source resident on one device.
pub(crate) struct Resident {
    /// Keeps the device alive for as long as the buffers are.
    ctx: Arc<GpuContext>,
    /// Bytes per stored frame (a multiple of four).
    pitch: usize,
    /// Frames per full chunk buffer.
    per_chunk: usize,
    /// Memory to leave free when the device runs out, bytes.
    headroom: u64,
    /// Pretend the device holds no more than this many bytes of the store
    /// (tests of the out-of-memory path).
    simulated_limit: Option<u64>,
    state: Mutex<State>,
    hits: AtomicU64,
    misses: AtomicU64,
}

#[derive(Default)]
struct State {
    chunks: Vec<Buffer>,
    /// Slot of every frame resident or being made resident.
    slot_of: HashMap<usize, u32>,
    /// Frame in each allocated slot.
    owner: Vec<Option<usize>>,
    free: Vec<u32>,
    /// Slots whose frame the running stage does not use, newest last.
    victims: Vec<u32>,
    wanted: HashSet<usize>,
    /// Admissions of the batch in flight, not yet committed.
    pending: Vec<(usize, u32)>,
    /// Most slots the budget allows (lowered when the device runs out).
    max_slots: usize,
    /// Device refused more memory (or the budget is reached).
    exhausted: bool,
}

impl Resident {
    /// A store for frames of `pitch` bytes on `ctx`, or `None` when residency
    /// is off or a single frame cannot fit a buffer.
    pub(crate) fn new(ctx: &Arc<GpuContext>, frame_bytes: usize, budget: u64) -> Option<Self> {
        Self::with_sizes(ctx, frame_bytes, budget, CHUNK_BYTES, HEADROOM)
    }

    /// As [`Self::new`] with the chunk size and the headroom chosen.
    fn with_sizes(
        ctx: &Arc<GpuContext>,
        frame_bytes: usize,
        budget: u64,
        chunk_bytes: u64,
        headroom: u64,
    ) -> Option<Self> {
        let pitch = frame_bytes.next_multiple_of(4);
        if pitch == 0 || budget < pitch as u64 || pitch as u64 > ctx.device.limits().max_buffer_size
        {
            return None;
        }
        let max_chunk = chunk_bytes.min(ctx.device.limits().max_buffer_size);
        let per_chunk = ((max_chunk / pitch as u64).max(1)) as usize;
        Some(Self {
            ctx: Arc::clone(ctx),
            pitch,
            per_chunk,
            headroom,
            simulated_limit: None,
            state: Mutex::new(State {
                max_slots: (budget / pitch as u64) as usize,
                ..State::default()
            }),
            hits: AtomicU64::new(0),
            misses: AtomicU64::new(0),
        })
    }

    /// True when this store is on `ctx` and holds frames of `frame_bytes`.
    pub(crate) fn fits(&self, ctx: &Arc<GpuContext>, frame_bytes: usize) -> bool {
        Arc::ptr_eq(&self.ctx, ctx) && self.pitch == frame_bytes.next_multiple_of(4)
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// A stage is starting that will use `indices`: frames outside them may
    /// be evicted to make room for frames inside them.
    pub(crate) fn begin_stage(&self, indices: &[usize]) {
        let mut st = self.lock();
        st.wanted = indices.iter().copied().collect();
        st.pending.clear();
        let State {
            owner,
            wanted,
            victims,
            ..
        } = &mut *st;
        victims.clear();
        victims.extend(
            owner
                .iter()
                .enumerate()
                .filter(|(_, o)| o.is_some_and(|f| !wanted.contains(&f)))
                .map(|(s, _)| s as u32),
        );
        // Memory other work held when the last stage ran may be free now.
        st.exhausted = false;
    }

    /// Slot holding frame `index`, if it is resident.
    pub(crate) fn lookup(&self, index: usize) -> Option<u32> {
        let slot = self.lock().slot_of.get(&index).copied();
        if slot.is_some() {
            self.hits.fetch_add(1, Ordering::Relaxed);
        } else {
            self.misses.fetch_add(1, Ordering::Relaxed);
        }
        slot
    }

    /// The buffer and byte offset of `slot`.
    pub(crate) fn place(&self, slot: u32) -> (Buffer, u64) {
        let chunk = slot as usize / self.per_chunk;
        let within = slot as usize % self.per_chunk;
        let buf = self.lock().chunks[chunk].clone();
        (buf, (within * self.pitch) as u64)
    }

    /// Reserve a slot for frame `index`, which the caller is about to copy
    /// there. `None` when it is already resident or reserved, or no room can
    /// be made. The reservation holds once [`Self::commit`] confirms the copy
    /// was submitted, and is undone by [`Self::rollback`].
    pub(crate) fn admit(&self, index: usize) -> Option<u32> {
        let mut st = self.lock();
        if st.slot_of.contains_key(&index) {
            return None;
        }
        let slot = self.take_slot(&mut st)?;
        st.owner[slot as usize] = Some(index);
        st.slot_of.insert(index, slot);
        st.pending.push((index, slot));
        Some(slot)
    }

    fn take_slot(&self, st: &mut State) -> Option<u32> {
        if let Some(s) = st.free.pop() {
            return Some(s);
        }
        if !st.exhausted && st.owner.len() < st.max_slots {
            // Chunks are whole except the last, which the budget cuts short.
            let take = (st.max_slots - st.owner.len()).min(self.per_chunk);
            if self.grow(st, take) {
                return st.free.pop();
            }
            st.exhausted = true;
            self.give_back_headroom(st);
            // Keep what is held: the next stage must not grow into the room
            // just given back.
            st.max_slots = st.owner.len();
            if let Some(s) = st.free.pop() {
                return Some(s);
            }
        }
        // Evict a frame the running stage does not use.
        while let Some(s) = st.victims.pop() {
            if let Some(old) = st.owner[s as usize]
                && !st.wanted.contains(&old)
            {
                st.slot_of.remove(&old);
                st.owner[s as usize] = None;
                return Some(s);
            }
        }
        None
    }

    /// Add a chunk buffer of `take` slots. False when the device is out of
    /// memory.
    fn grow(&self, st: &mut State, take: usize) -> bool {
        let size = (take * self.pitch) as u64;
        let held: u64 = st.chunks.iter().map(Buffer::size).sum();
        if self.simulated_limit.is_some_and(|l| held + size > l) {
            return false;
        }
        let guard = self.ctx.device.push_error_scope(ErrorFilter::OutOfMemory);
        let buf = self.ctx.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("resident frames"),
            size,
            usage: BufferUsages::COPY_SRC | BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        if pollster::block_on(guard.pop()).is_some() {
            return false;
        }
        let first = st.owner.len() as u32;
        st.chunks.push(buf);
        st.owner.extend(std::iter::repeat_n(None, take));
        st.free.extend((first..first + take as u32).rev());
        true
    }

    /// After the device ran out of memory, drop trailing chunks until
    /// the headroom is free again, forgetting the frames in them.
    fn give_back_headroom(&self, st: &mut State) {
        let mut freed = 0u64;
        while freed < self.headroom && st.chunks.len() > 1 {
            let Some(chunk) = st.chunks.pop() else { break };
            freed += chunk.size();
            // Dropped, not destroyed: a command recorded this batch may still copy into it.
            drop(chunk);
        }
        let keep = st
            .chunks
            .iter()
            .map(|c| c.size() / self.pitch as u64)
            .sum::<u64>() as usize;
        st.owner.truncate(keep);
        st.free.retain(|&s| (s as usize) < keep);
        st.victims.retain(|&s| (s as usize) < keep);
        st.pending.retain(|&(_, s)| (s as usize) < keep);
        st.slot_of.retain(|_, &mut s| (s as usize) < keep);
    }

    /// The batch's copies were submitted: the reservations stand.
    pub(crate) fn commit(&self) {
        self.lock().pending.clear();
    }

    /// The batch failed before its copies were submitted: release its slots.
    pub(crate) fn rollback(&self) {
        let mut st = self.lock();
        let pending = std::mem::take(&mut st.pending);
        for (index, slot) in pending {
            st.slot_of.remove(&index);
            st.owner[slot as usize] = None;
            st.free.push(slot);
        }
    }

    /// Frames resident now and bytes of device memory the store holds.
    pub(crate) fn usage(&self) -> (usize, u64) {
        let st = self.lock();
        let bytes = st.chunks.iter().map(Buffer::size).sum();
        (st.slot_of.len(), bytes)
    }

    /// `(hits, misses)` of lookups since the last call.
    pub(crate) fn take_counts(&self) -> (u64, u64) {
        (
            self.hits.swap(0, Ordering::Relaxed),
            self.misses.swap(0, Ordering::Relaxed),
        )
    }
}

/// Bytes of device memory the store may take on `ctx`: `HELIA_GPU_RESIDENT_MB`
/// if set, else 6 GiB on a discrete adapter and nothing elsewhere.
///
/// wgpu does not report how much memory an adapter has, and the limits it
/// does report do not follow it (`max_buffer_size` is 4 GiB on an 80 GB A100),
/// so the default is a fixed share for discrete adapters. The real guard is the
/// device: the store stops growing the moment it reports it is out of memory,
/// gives back [`HEADROOM`] for the stages' buffers, and the frames that did
/// not fit are read from the file as before.
pub(crate) fn default_budget(ctx: &GpuContext) -> u64 {
    if let Some(mb) = std::env::var(BUDGET_ENV)
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
    {
        return mb << 20;
    }
    let info = ctx.adapter_info();
    match info.device_type {
        DeviceType::DiscreteGpu => DISCRETE_BUDGET,
        // Memory shared with the machine: the "upload" is a memory copy, so a
        // second copy of the frames saves nothing (measured on lavapipe: no
        // change in time, 0.3 to 0.9 GB more resident).
        _ => 0,
    }
}

/// The store of `frames` on `ctx`, creating it on first use. `None` when the
/// source has no cache, the budget is zero, or the cache belongs to another
/// device.
pub(crate) fn for_source(
    ctx: &Arc<GpuContext>,
    frames: &dyn SharedFrames,
    budget: Option<u64>,
) -> Option<Arc<Resident>> {
    let cache = frames.device_cache()?;
    let frame_bytes = frames.layout().frame_bytes();
    let budget = budget.unwrap_or_else(|| default_budget(ctx));
    cache
        .get_or_insert_with(|| Resident::new(ctx, frame_bytes, budget))
        .filter(|r| r.fits(ctx, frame_bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::AdapterRequest;

    /// A software device, or `None` (the test then skips) where there is none
    /// and no run of the GPU tests was asked for. Where a hardware run was
    /// asked for and the platform has no software adapter (Metal has none),
    /// the hardware device stands in: these tests drive the store with a
    /// simulated limit and do not depend on which adapter holds it.
    fn device() -> Option<Arc<GpuContext>> {
        let mode = std::env::var("HELIA_GPU_TESTS").ok();
        let hardware = mode
            .as_deref()
            .is_some_and(|m| m.eq_ignore_ascii_case("hardware"));
        match GpuContext::new(AdapterRequest::Software) {
            Ok(c) => Some(Arc::new(c)),
            Err(_) if hardware => match GpuContext::new(AdapterRequest::Hardware) {
                Ok(c) => Some(Arc::new(c)),
                Err(e) => panic!("HELIA_GPU_TESTS=hardware but no adapter opened: {e}"),
            },
            Err(e) => {
                assert!(
                    mode.is_none(),
                    "HELIA_GPU_TESTS is set but there is no software adapter: {e}"
                );
                None
            }
        }
    }

    const FRAME: usize = 64 << 10;
    const CHUNK: u64 = 1 << 20;

    fn store(ctx: &Arc<GpuContext>, slots: u64, limit: Option<u64>) -> Resident {
        let mut r =
            Resident::with_sizes(ctx, FRAME, slots * FRAME as u64, CHUNK, CHUNK).expect("store");
        r.simulated_limit = limit;
        r
    }

    fn admit_all(r: &Resident, frames: std::ops::Range<usize>) -> usize {
        let got = frames.filter_map(|i| r.admit(i)).count();
        r.commit();
        got
    }

    #[test]
    fn fills_to_the_budget_then_refuses() {
        let Some(ctx) = device() else { return };
        let r = store(&ctx, 40, None);
        r.begin_stage(&(0..100).collect::<Vec<_>>());
        assert_eq!(admit_all(&r, 0..100), 40);
        assert!(r.lookup(0).is_some() && r.lookup(39).is_some());
        assert!(r.lookup(40).is_none());
        assert_eq!(r.usage().0, 40);
    }

    #[test]
    fn a_stage_swaps_in_the_frames_it_needs_and_keeps_those_it_shares() {
        let Some(ctx) = device() else { return };
        let r = store(&ctx, 20, None);
        r.begin_stage(&(0..20).collect::<Vec<_>>());
        assert_eq!(admit_all(&r, 0..20), 20);
        // The next stage wants 10..30: frames 0..10 go, 10..20 stay.
        r.begin_stage(&(10..30).collect::<Vec<_>>());
        assert!((10..20).all(|i| r.lookup(i).is_some()));
        assert_eq!(admit_all(&r, 20..30), 10);
        assert!((10..30).all(|i| r.lookup(i).is_some()));
        assert!((0..10).all(|i| r.lookup(i).is_none()));
        // Frames the stage wants are never evicted for one it also wants.
        r.begin_stage(&(10..40).collect::<Vec<_>>());
        assert_eq!(admit_all(&r, 30..40), 0);
    }

    #[test]
    fn a_failed_batch_gives_its_slots_back() {
        let Some(ctx) = device() else { return };
        let r = store(&ctx, 8, None);
        r.begin_stage(&(0..8).collect::<Vec<_>>());
        assert_eq!(r.admit(0).map(|_| ()), Some(()));
        r.rollback();
        assert!(r.lookup(0).is_none());
        assert_eq!(admit_all(&r, 0..8), 8);
    }

    /// Asks a real GPU for far more than it has: `cargo test -p helia-gpu --lib
    /// -- --ignored` with `HELIA_GPU_DEVICE` naming a card nobody is using.
    #[test]
    #[ignore = "fills the memory of a real GPU for a moment"]
    fn a_real_device_that_runs_out_keeps_the_rest() {
        let ctx = Arc::new(GpuContext::new(AdapterRequest::Hardware).expect("a hardware GPU"));
        if ctx.is_software() {
            return;
        }
        // 256 MiB frames in 1 GiB chunks, a budget of 4 TiB.
        let frame = 256 << 20;
        let r = Resident::with_sizes(&ctx, frame, 4 << 40, 1 << 30, 1536 << 20).expect("store");
        r.begin_stage(&(0..100_000).collect::<Vec<_>>());
        let mut asked = 0;
        for i in 0..100_000 {
            asked += 1;
            if r.admit(i).is_none() {
                break;
            }
        }
        r.commit();
        let (frames, bytes) = r.usage();
        eprintln!(
            "the device refused after {asked} frames; {frames} frames, {} MiB kept",
            bytes >> 20
        );
        assert!(frames > 0 && frames < 100_000);
        // It still works: a further buffer fits in what was given back.
        let spare = ctx.storage("spare", 1 << 30, BufferUsages::empty());
        drop(spare);
    }

    #[test]
    fn a_device_that_runs_out_keeps_less_and_gives_headroom_back() {
        let Some(ctx) = device() else { return };
        // 16 frames per chunk; the device holds three chunks; one chunk of
        // headroom is returned when it refuses the fourth.
        let r = store(&ctx, 100, Some(3 * CHUNK));
        r.begin_stage(&(0..100).collect::<Vec<_>>());
        // The third chunk's 16 admissions were copied but are forgotten with it.
        assert_eq!(admit_all(&r, 0..100), 48);
        assert_eq!(r.usage(), (32, 2 * CHUNK), "two chunks stay");
        assert!((0..32).all(|i| r.lookup(i).is_some()));
        assert!((32..100).all(|i| r.lookup(i).is_none()));
        // Later stages do not try to grow into the room given back.
        r.begin_stage(&(0..100).collect::<Vec<_>>());
        assert_eq!(admit_all(&r, 32..100), 0);
        assert_eq!(r.usage().1, 2 * CHUNK);
    }
}
