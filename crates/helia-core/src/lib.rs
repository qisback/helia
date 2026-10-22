// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Helia core: lucky-imaging maths with no GPU dependency.
//!
//! Data flow: [`io`] readers produce [`Frame`]s, [`quality`] ranks them,
//! [`align`] registers them (globally, then per alignment point), [`stack`]
//! combines them and [`pipeline`] drives the whole chain. The CPU
//! implementation of [`backend::Backend`] is the reference that any GPU
//! backend must match.
#![forbid(unsafe_code)]

pub mod about;
pub mod align;
pub mod animate;
pub mod applog;
pub mod autoap;
pub mod autocrop;
pub mod backend;
pub mod calibrate;
pub mod captime;
pub mod clip;
pub mod colour;
pub mod debayer;
pub mod derotate;
pub mod edit;
pub mod ephemeris;
pub mod error;
pub mod field_rotation;
pub mod filter;
pub mod frame;
pub mod grade;
pub mod granulation;
pub mod io;
mod lanes;
pub mod limbdark;
pub mod lut_export;
pub mod mask;
pub mod measure;
pub mod mosaic;
pub mod noise;
pub mod output;
pub mod picture;
pub mod pipeline;
pub mod profile;
pub mod quality;
pub mod quality_extra;
pub mod recommend;
pub mod resample;
pub mod seeing;
pub mod stack;
pub mod sunspots;
#[cfg(any(test, feature = "testutil"))]
pub mod testutil;
pub mod timelapse;
pub mod truecolour;

pub use align::Shift;
pub use backend::{
    Accumulated, Accumulator, Backend, CpuBackend, DeviceStage, FrameDeposit, MeasureRequest,
    RegisterRequest, RobustAccumulator, ScoreRequest, StackRequest, StackResult, StageProgress,
};
pub use colour::{ColourLut, ColourParams, Preset, RgbImage};
pub use edit::{EditList, Op, SharpenMethod};
pub use error::{Error, Result};
pub use frame::{BayerPattern, Frame, Rect};
pub use quality::{Keep, Metric};
pub use resample::Kernel;
