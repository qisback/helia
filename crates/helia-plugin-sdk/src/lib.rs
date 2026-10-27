// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: MIT OR Apache-2.0

//! SDK for writing [Helia](https://github.com/qisback/helia) plugins in Rust.
//!
//! A plugin is a program Helia starts and talks to in JSON-RPC 2.0, one message per line on
//! standard input and output, with pixels passed through raw `f32` files (`hlpx`) in a private
//! working folder. The protocol is specified in the Helia book (Developer guide, Plugins: design
//! and protocol); this crate is the plugin side of it, and is also used by Helia's own host for the
//! shared definitions (framing, buffers, errors) so both sides agree.
//!
//! * [`Plugin`] and [`run`]: implement the trait for the capabilities you offer, call `run`.
//! * [`Ctx`]: progress, cancellation and image files for the call in progress.
//! * [`Image`], [`BufferRef`]: the pixel buffer format.
//! * [`PluginError`]: an error in Helia's What / Why / What-to-do form.
//! * [`wire`]: framing and message parsing (used by hosts and tests).
//!
//! Licence: MIT OR Apache-2.0. This crate depends on no Helia crate.

pub mod hlpx;
mod runtime;
mod types;
pub mod wire;

pub use hlpx::{BufferRef, Image, OutputSpec};
pub use runtime::{Ctx, Plugin, run, run_with};
pub use types::{
    API_VERSION, AnalyseRequest, AnalyseResult, ErrorKind, Info, InitializeParams, PluginError,
    ProcessRequest, ProcessResult, StackRequest, StackResult, TestResult, clip, code,
};
