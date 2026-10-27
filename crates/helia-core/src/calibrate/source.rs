// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! A capture whose frames are calibrated as they are read.
//!
//! Like [`crate::mask::MaskedSource`], this wraps a [`FrameSource`] and edits
//! what it serves in one place, so the CPU stages and the GPU's decode kernel
//! (which reads the raw bytes) cannot disagree. The calibrated frame is served
//! as single-plane `f32`: calibration is applied to the plane as decoded,
//! before any demosaicing, scoring or alignment.

use std::any::Any;
use std::sync::Arc;

use crate::error::{Error, Result};
use crate::frame::{BayerPattern, Frame};
use crate::io::raw::{Planes, RowLayout, Sample, SharedFrames};
use crate::io::source::FrameSource;

use super::Plan;

thread_local! {
    /// The stored bytes of the frame being calibrated, kept per thread.
    static STORED: std::cell::RefCell<Vec<u8>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// A capture with every frame calibrated.
pub struct CalibratedSource<'a> {
    inner: &'a mut dyn FrameSource,
    plan: Plan,
    cache: crate::io::raw::DeviceCache,
}

impl<'a> CalibratedSource<'a> {
    /// Wrap `inner`.
    ///
    /// # Errors
    /// [`Error::Invalid`] when the plan was made for another frame size.
    pub fn new(inner: &'a mut dyn FrameSource, plan: Plan) -> Result<Self> {
        if inner.dimensions() != plan.size() {
            return Err(Error::Invalid(format!(
                "calibration is for {}x{} frames but the capture is {}x{}",
                plan.size().0,
                plan.size().1,
                inner.dimensions().0,
                inner.dimensions().1
            )));
        }
        Ok(Self {
            inner,
            plan,
            cache: crate::io::raw::DeviceCache::default(),
        })
    }

    /// The plan in force.
    #[must_use]
    pub fn plan(&self) -> &Plan {
        &self.plan
    }

    fn shared_inner(&self) -> Result<&dyn SharedFrames> {
        self.inner
            .shared()
            .ok_or_else(|| Error::Unsupported("source cannot be read concurrently".into()))
    }
}

impl FrameSource for CalibratedSource<'_> {
    fn dimensions(&self) -> (usize, usize) {
        self.inner.dimensions()
    }
    fn frame_count(&self) -> usize {
        self.inner.frame_count()
    }
    fn read(&mut self, index: usize) -> Result<Frame> {
        let mut f = self.inner.read(index)?;
        self.plan.apply(&mut f.data);
        Ok(f)
    }
    fn describe(&self) -> String {
        format!("{} (calibrated)", self.inner.describe())
    }
    fn shared(&self) -> Option<&dyn SharedFrames> {
        self.inner.shared().map(|_| self as &dyn SharedFrames)
    }
    fn quality_note(&self) -> Option<String> {
        self.inner.quality_note()
    }
    fn keep_alive(&self) -> Option<Arc<dyn Any + Send + Sync>> {
        self.inner.keep_alive()
    }
}

impl SharedFrames for CalibratedSource<'_> {
    fn frame_count(&self) -> usize {
        self.inner.frame_count()
    }
    fn layout(&self) -> RowLayout {
        let (width, height) = self.inner.dimensions();
        RowLayout {
            width,
            height,
            sample: Sample::F32,
            planes: Planes::One,
            stride: width * 4,
            bottom_up: false,
        }
    }
    fn bayer(&self) -> Option<BayerPattern> {
        self.inner.shared().and_then(SharedFrames::bayer)
    }
    fn device_cache(&self) -> Option<&crate::io::raw::DeviceCache> {
        Some(&self.cache)
    }
    fn read_raw(&self, index: usize, buf: &mut Vec<u8>) -> Result<()> {
        let inner = self.shared_inner()?;
        STORED.with(|cell| {
            let mut stored = cell.borrow_mut();
            inner.read_raw(index, &mut stored)?;
            let mut plane = inner.layout().decode(&stored);
            self.plan.apply(&mut plane);
            buf.clear();
            buf.reserve(plane.len() * 4);
            buf.extend(plane.iter().flat_map(|v| v.to_le_bytes()));
            Ok(())
        })
    }
}
