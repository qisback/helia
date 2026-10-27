// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! The plugin host: everything Helia needs to find, approve, start and talk to plugins.
//!
//! The protocol is specified in the book (Developer guide, Plugins: design and protocol). In
//! short: a plugin is a program with a `plugin.toml`; it runs as a child process that speaks
//! JSON-RPC 2.0 on one line per message and passes pixels as raw `f32` files in a private folder;
//! nothing runs until the user approves its checksum.
//!
//! | Module | Role |
//! |---|---|
//! | [`manifest`] | read and validate `plugin.toml`, compute the checksums |
//! | [`schema`] | the JSON Schema subset behind the settings form and step parameters |
//! | [`registry`] | discovery, the approvals file `plugins.toml`, enabled or changed |
//! | [`session`] | one running plugin: handshake, calls, progress, cancel, timeouts, crashes |
//! | [`cards`] | the FITS cards a plugin's provenance may add (a fixed allow-list) |
//! | [`job`] | the stacking job a `stack-backend` plugin receives |
//! | [`ops`] | steps in the pipeline: frame, image and analyse steps, the stack backend |
//! | [`error`] | errors as What / Why / What to do |

pub mod cards;
pub mod error;
pub mod job;
pub mod manifest;
pub mod ops;
pub mod registry;
pub mod schema;
pub mod session;

pub use error::{HostError, Kind};
pub use manifest::{Digests, Loaded, ManifestError};
pub use ops::{PluginFrames, Runner, StepSpec, StepStage};
pub use registry::{Approvals, Registry, Status};
pub use session::{Analysis, Progress, Session, Stacked};
