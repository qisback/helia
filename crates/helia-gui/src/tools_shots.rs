// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Snapshots of the ffmpeg install page and of the About page's Optional tools block, for the
//! book (`docs/book/src/images/ui/`). The state is injected, so nothing is fetched.
//!
//! Regenerate the pictures with
//! `UPDATE_SNAPSHOTS=1 HELIA_GPU_TESTS=software cargo test -p helia-gui tools_shots`.

use eframe::egui::vec2;
use egui_kittest::Harness;

use crate::app::App;
use crate::snapshots::{SnapshotSettled, data, enabled, load, options, serial, shared_wgpu};
use crate::tools_ui::tests::offer;
use crate::tools_ui::{InstalledLine, ToolsState, ToolsUi};

fn harness() -> Harness<'static, App> {
    Harness::builder()
        .with_size(vec2(1440.0, 1120.0))
        .with_options(options())
        .wgpu_setup(shared_wgpu())
        .build_eframe(|cc| App::with_ctx(&cc.egui_ctx, None, false))
}

fn page(name: &str, state: ToolsState, reason: Option<&str>) {
    if !enabled() {
        return;
    }
    let _serial = serial();
    let mut h = harness();
    h.state_mut().tools = ToolsUi::showing(state);
    h.state_mut().tools.reason = reason.map(str::to_owned);
    h.run_steps(3);
    h.snapshot_settled(name);
}

const VIDEO_REASON: &str = "The file you opened is a video (MP4, MOV, MKV or WebM), and Helia reads those through the free ffmpeg program, which is not installed on this computer.";

/// A video needed ffmpeg: what happened, why, what to do, the provider and the whole licence.
#[test]
fn tools_offer() {
    page(
        "tools-offer",
        ToolsState::Offer(Box::new(offer())),
        Some(VIDEO_REASON),
    );
}

/// The same licence, already accepted before and unchanged.
#[test]
fn tools_offer_accepted_before() {
    let mut o = offer();
    o.accepted_on = Some("2026-10-26T15:42:22Z".into());
    o.package_command = None;
    page("tools-offer-accepted", ToolsState::Offer(Box::new(o)), None);
}

/// The download is running.
#[test]
fn tools_installing() {
    page(
        "tools-installing",
        ToolsState::Installing {
            label: "Downloading".into(),
            done: 62_000_000,
            total: 137_933_780,
        },
        Some(VIDEO_REASON),
    );
}

/// Installed, with the attribution the licence asks for.
#[test]
fn tools_done() {
    page(
        "tools-done",
        ToolsState::Done {
            message: "FFmpeg 9.0.2 is installed. Helia uses it automatically.".into(),
            attribution: Some("FFmpeg is a trademark of Fabrice Bellard, originator of the FFmpeg project. This build of FFmpeg is made by the BtbN FFmpeg-Builds project and is used under the GNU Lesser General Public License, version 3 or later. Helia is not affiliated with FFmpeg or BtbN.".into()),
        },
        None,
    );
}

/// The signed list could not be fetched.
#[test]
fn tools_failed() {
    page(
        "tools-failed",
        ToolsState::Failed(
            "could not reach the server: connection refused. Check your network and try again; nothing was downloaded."
                .into(),
        ),
        Some(VIDEO_REASON),
    );
}

/// The About page with ffmpeg installed.
#[test]
fn tools_about_installed() {
    if !enabled() {
        return;
    }
    let _serial = serial();
    let d = data();
    let mut h = Harness::builder()
        .with_size(vec2(1440.0, 1900.0))
        .with_options(options())
        .wgpu_setup(shared_wgpu())
        .build_eframe(|cc| App::with_ctx(&cc.egui_ctx, None, false));
    let ctx = h.ctx.clone();
    load(h.state_mut(), &ctx, &d);
    h.state_mut().version_override = Some(
        [
            "helia-gui 0.8.0",
            "release: pre-release",
            "commit: 1a2b3c4d",
            "built: 2026-10-25 09:30 UTC",
            "profile: release",
            "target: x86_64-unknown-linux-gnu",
        ]
        .join("\n"),
    );
    let mut t = ToolsUi::showing(ToolsState::Idle);
    t.open = false;
    t.installed = vec![InstalledLine {
        name: "FFmpeg".into(),
        version: "9.0.2".into(),
        route: "download".into(),
        provider: "BtbN FFmpeg-Builds".into(),
        licence_name: "GNU Lesser General Public License, version 3 or later".into(),
        attribution: Some("FFmpeg is a trademark of Fabrice Bellard, originator of the FFmpeg project. Helia is not affiliated with FFmpeg or BtbN.".into()),
        problems: Vec::new(),
    }];
    t.found = Some("<tools>/ffmpeg/ffmpeg (installed with Helia)".into());
    h.state_mut().tools = t;
    h.state_mut().show_about = true;
    h.run_steps(3);
    h.snapshot_settled("tools-about-installed");
}
