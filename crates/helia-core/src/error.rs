// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Crate-wide error type.

use crate::io::{avi::AviError, ffmpeg::FfmpegError, fits::FitsError, ser::SerError};

/// Any failure produced by `helia-core`.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Underlying I/O failure.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    /// SER container problem.
    #[error(transparent)]
    Ser(#[from] SerError),
    /// FITS container problem.
    #[error(transparent)]
    Fits(#[from] FitsError),
    /// AVI container problem.
    #[error(transparent)]
    Avi(#[from] AviError),
    /// Compressed video through ffmpeg.
    #[error(transparent)]
    Ffmpeg(#[from] FfmpegError),
    /// TIFF encoding problem.
    #[error("TIFF error: {0}")]
    Tiff(String),
    /// Caller supplied inconsistent or unusable data.
    #[error("invalid input: {0}")]
    Invalid(String),
    /// Feature recognised but not implemented.
    #[error("unsupported: {0}")]
    Unsupported(String),
    /// A GPU or other backend failed.
    #[error("backend error: {0}")]
    Backend(String),
    /// The operation was cancelled by the caller.
    #[error("operation cancelled")]
    Cancelled,
}

/// Convenience alias.
pub type Result<T> = std::result::Result<T, Error>;
