// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! `cargo xtask`: repository automation.
//!
//! `cargo xtask docs` regenerates the reference pages of the book from the
//! code: the CLI reference from the clap definition in `helia-cli`, and the
//! API reference from the `OpenAPI` document that `helia-server` builds, and
//! the colour preset table from `helia-core`.
//! `cargo xtask docs --check` fails when the committed files differ, so the
//! references cannot go stale.
//!
//! `cargo xtask coverage` fails when a subcommand or long flag is not mentioned in
//! the hand-written chapters, or the documentation coverage table is inconsistent;
//! `docs --check` runs it too, so one CI step covers both.
//!
//! `cargo xtask branding` rebuilds the icons derived from the vector logo.
//!
//! `cargo xtask reuse-check` fails when a file lacks licensing information.

mod apidoc;
mod branding;
mod clidoc;
mod coverage;
mod presetdoc;
mod reuse;

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "xtask", about = "Helia repository automation")]
struct Xtask {
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Regenerate the generated pages of the book.
    Docs {
        /// Do not write; fail if a committed generated file is out of date.
        #[arg(long)]
        check: bool,
    },
    /// Fail if a flag is undocumented or the coverage table is inconsistent.
    Coverage,
    /// Fail if a file lacks an SPDX header or a REUSE.toml annotation.
    ReuseCheck,
    /// Rebuild the images derived from assets/branding/helia-logo.svg.
    Branding {
        /// Also write a contact sheet of the icon sizes here, to check by eye.
        #[arg(long)]
        contact_sheet: Option<PathBuf>,
    },
    /// Write a synthetic SER capture of a sun, to try Helia without a telescope.
    Sample {
        /// File to write (a `.ser` extension is conventional).
        #[arg(default_value = "synthetic-sun.ser")]
        output: PathBuf,
        /// Number of frames.
        #[arg(long, default_value_t = 400)]
        frames: usize,
    },
}

/// Directory of the book sources, relative to the repository root.
const REFERENCE_DIR: &str = "docs/book/src/reference";

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map_or_else(|| PathBuf::from("."), Path::to_path_buf)
}

/// Every generated file: repository-relative path and exact content.
fn generated() -> Result<Vec<(String, String)>> {
    let openapi = helia_server::openapi::document().context("cannot build the OpenAPI document")?;
    let spec: serde_json::Value = serde_json::from_str(&openapi)?;
    Ok(vec![
        (format!("{REFERENCE_DIR}/cli.md"), clidoc::render()),
        (format!("{REFERENCE_DIR}/api.md"), apidoc::render(&spec)?),
        (format!("{REFERENCE_DIR}/openapi.json"), openapi + "\n"),
        (
            format!("{REFERENCE_DIR}/presets-table.md"),
            presetdoc::render(),
        ),
    ])
}

/// Line endings differ between checkouts on Windows; content must not.
fn normalise(s: &str) -> String {
    s.replace("\r\n", "\n")
}

/// Render the book's measured-result charts from their data files, or check that they are
/// current and quote only numbers their page carries.
fn charts(root: &Path, check: bool) -> Result<()> {
    let script = root.join("tools/docs-graphs/make_graphs.py");
    let mut failure = String::new();
    for py in ["python3", "python"] {
        let mut cmd = std::process::Command::new(py);
        cmd.arg(&script).arg("--root").arg(root);
        if check {
            cmd.arg("--check");
        }
        match cmd.status() {
            Ok(s) if s.success() => return Ok(()),
            Ok(s) => failure = format!("`{py}` exited with {s}"),
            Err(e) => failure = format!("cannot run `{py}`: {e}"),
        }
    }
    bail!(
        "the chart generator failed ({failure}); run `python3 tools/docs-graphs/make_graphs.py` and commit the result"
    )
}

fn docs(check: bool) -> Result<()> {
    let root = repo_root();
    let mut stale = Vec::new();
    for (rel, content) in generated()? {
        let path = root.join(&rel);
        if check {
            let on_disk = std::fs::read_to_string(&path).unwrap_or_default();
            if normalise(&on_disk) != content {
                stale.push(rel);
            }
        } else {
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            std::fs::write(&path, &content)
                .with_context(|| format!("cannot write {}", path.display()))?;
            println!("wrote {rel}");
        }
    }
    if !stale.is_empty() {
        bail!(
            "generated documentation is out of date: {}\nrun `cargo xtask docs` and commit the result",
            stale.join(", ")
        );
    }
    charts(&root, check)?;
    if check {
        println!("generated documentation is up to date");
        coverage::check(&root)?;
    }
    Ok(())
}

/// A reproducible synthetic capture: a known sun with varying blur, shifts and
/// seeing-like distortion. The ground truth is not saved; this is for trying
/// the tools, and the documentation's examples use it.
fn sample(output: &Path, frames: usize) -> Result<()> {
    use helia_core::testutil::{
        CaptureConfig, SyntheticCapture, SyntheticConfig, SyntheticSun, ser_bytes,
    };
    if frames == 0 {
        bail!("--frames must be at least 1");
    }
    let sun = SyntheticSun::new(&SyntheticConfig {
        width: 320,
        height: 288,
        ..SyntheticConfig::default()
    });
    let cap = SyntheticCapture::generate(
        &sun,
        &CaptureConfig {
            frames,
            noise: 0.02,
            warp_amp: 1.5,
            warp_free: 5,
            ..CaptureConfig::default()
        },
    );
    std::fs::write(output, ser_bytes(&cap.frames))
        .with_context(|| format!("cannot write {}", output.display()))?;
    println!(
        "wrote {} ({frames} frames, 320 x 288, 16-bit mono)",
        output.display()
    );
    Ok(())
}

fn main() {
    let result = match Xtask::parse().command {
        Cmd::Docs { check } => docs(check),
        Cmd::Branding { contact_sheet } => branding::run(
            &repo_root().join("assets/branding"),
            contact_sheet.as_deref(),
        ),
        Cmd::Coverage => coverage::check(&repo_root()),
        Cmd::ReuseCheck => reuse::check(&repo_root()),
        Cmd::Sample { output, frames } => sample(&output, frames),
    };
    if let Err(e) = result {
        eprintln!("xtask: {e:#}");
        std::process::exit(1);
    }
}
