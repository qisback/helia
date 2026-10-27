// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Snapshots of the Plugins page and of the plugin steps in the Stack step, for the book
//! (`docs/book/src/images/ui/`), and checks of what the page does. The plugins are small files made
//! here with fixed contents, so the checksums in the pictures never change and nothing depends on
//! the machine; the folder they are in is shown as `<plugins>`.
//!
//! Regenerate the pictures with
//! `UPDATE_SNAPSHOTS=1 HELIA_GPU_TESTS=software cargo test -p helia-gui plugin_shots`.

#![allow(clippy::many_single_char_names)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use eframe::egui::vec2;
use egui_kittest::Harness;
use helia_plugin::registry::{Approvals, Registry};
use helia_plugin::{Analysis, StepSpec, StepStage};
use serde_json::json;

use crate::app::App;
use crate::model::Step;
use crate::plugin_steps::UiStep;
use crate::plugins_page::{PluginsUi, TestState};
use crate::snapshots::{
    SnapshotSettled, data, enabled, give_result, load, options, serial, shared_wgpu,
};

/// A plugin folder with fixed contents.
fn plugin(root: &Path, name: &str, manifest: &str, files: &[(&str, &str)]) {
    let d = root.join(name);
    std::fs::create_dir_all(&d).expect("plugin folder");
    std::fs::write(d.join("plugin.toml"), manifest).expect("manifest");
    for (f, text) in files {
        std::fs::write(d.join(f), text).expect("file");
    }
}

/// Four plugins: two on, one off and one that changed after it was approved.
fn fixtures() -> (PathBuf, Registry) {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static N: AtomicUsize = AtomicUsize::new(0);
    // Canonical from the start: on Windows the registry spells the plugin file as given and the
    // program canonicalised (8.3 short name against the verbatim form), so the mask only
    // matches both when the root already is the canonical path.
    let tmp = std::env::temp_dir();
    let root = tmp.canonicalize().unwrap_or(tmp).join(format!(
        "helia-plugin-shots-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&root);
    plugin(
        &root,
        "invert-py",
        r#"name = "invert-py"
version = "0.1.0"
api_version = 1
title = "Invert (Python)"
description = "Inverts the brightness of an image, written in plain Python with no SDK."
author = "Laurence Horrocks-Barlow"
licence = "MIT OR Apache-2.0"
capabilities = ["image-op"]
executable = "invert.py"
interpreter = "python3"
[resources]
memory_mb = 256
[[steps]]
id = "invert"
capability = "image-op"
title = "Invert"
description = "Replaces each value v by (max + min) - v."
"#,
        &[("invert.py", "# invert: fixed text for the snapshot\n")],
    );
    plugin(
        &root,
        "stats-py",
        r#"name = "stats-py"
version = "0.1.0"
api_version = 1
title = "Image statistics (Python)"
description = "Reports the mean, range and spread of an image and outlines its bright part."
author = "Laurence Horrocks-Barlow"
licence = "MIT OR Apache-2.0"
capabilities = ["analyse"]
executable = "stats.py"
interpreter = "python3"
[resources]
memory_mb = 256
[[steps]]
id = "stats"
capability = "analyse"
title = "Statistics"
description = "Mean, minimum, maximum and standard deviation, and a box round the bright part."
params_schema = "stats.params.json"
"#,
        &[
            ("stats.py", "# stats: fixed text for the snapshot\n"),
            (
                "stats.params.json",
                r#"{"type":"object","properties":{"threshold":{"type":"number","title":"Bright part threshold","description":"Pixels above this share of the maximum count as bright.","minimum":0,"maximum":1,"default":0.5}}}"#,
            ),
        ],
    );
    plugin(
        &root,
        "denoise-gpu",
        r#"name = "denoise-gpu"
version = "1.2.0"
api_version = 1
title = "GPU denoise"
description = "Denoises the stack on a GPU server and can also do the stacking there."
author = "A. Plugin Author"
licence = "MIT"
capabilities = ["stack-backend", "image-op"]
executable = "denoise"
settings_schema = "settings.json"
[resources]
gpu = true
memory_mb = 4096
[limits]
timeout_s = 300
[update]
manifest_url = "https://plugins.example.org/denoise-gpu/update.json"
signing_key = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
channel = "stable"
[[steps]]
id = "denoise"
capability = "image-op"
title = "Denoise"
description = "Learned denoise of the stack."
params_schema = "denoise.params.json"
"#,
        &[
            (
                "denoise",
                "#!/bin/sh\n# denoise: fixed text for the snapshot\n",
            ),
            (
                "settings.json",
                r#"{"type":"object","required":["server"],"properties":{
"server":{"type":"string","title":"Server","description":"Address of the GPU server.","default":"https://gpu.example.org:7871","x-order":1},
"token":{"type":"string","title":"Access token","format":"password","description":"Kept in plugins.toml and never logged.","x-order":2},
"port":{"type":"integer","title":"Port","minimum":1,"maximum":65535,"default":7871,"x-order":3},
"mode":{"type":"string","title":"Quality","enum":["fast","balanced","best"],"default":"balanced","x-order":4},
"verify":{"type":"boolean","title":"Check the server's certificate","default":true,"x-order":5}}}"#,
            ),
            (
                "denoise.params.json",
                r#"{"type":"object","properties":{"strength":{"type":"number","title":"Strength","description":"0 keeps the noise, 1 removes all it can.","minimum":0,"maximum":1,"default":0.5}}}"#,
            ),
        ],
    );
    plugin(
        &root,
        "old-tool",
        r#"name = "old-tool"
version = "0.3.0"
api_version = 1
title = "Old tool"
description = "A plugin that was updated after it was approved."
author = "Someone Else"
capabilities = ["analyse"]
executable = "tool.py"
interpreter = "python3"
[[steps]]
id = "look"
capability = "analyse"
"#,
        &[("tool.py", "# tool: first version\n")],
    );
    let mut reg = Registry::discover(Some(&root), &[], Approvals::in_memory());
    for n in ["invert-py", "denoise-gpu", "old-tool"] {
        let l = reg.get(n).expect("found").clone();
        reg.approvals.approve(&l).expect("approve");
    }
    // The program changes after it was approved.
    std::fs::write(root.join("old-tool/tool.py"), "# tool: second version\n").expect("change");
    (root, reg)
}

fn page_state(app: &mut App, root: &Path, reg: Registry) {
    app.plugins = PluginsUi::with_registry(reg, Vec::new());
    app.plugins.mask = Some(root.to_path_buf());
    app.show_plugins = true;
}

#[test]
fn plugins_page() {
    if !enabled() {
        return;
    }
    let _serial = serial();
    let (root, reg) = fixtures();
    let mut h = Harness::builder()
        .with_size(vec2(1440.0, 960.0))
        .with_options(options())
        .wgpu_setup(shared_wgpu())
        .build_eframe(|cc| App::with_ctx(&cc.egui_ctx, None, false));
    {
        let app = h.state_mut();
        page_state(app, &root, reg);
        app.plugins.selected = Some("denoise-gpu".into());
        app.plugins.tests.insert(
            "denoise-gpu".into(),
            TestState::Done {
                ok: true,
                message: "Connected to gpu.example.org (NVIDIA A100, 80 GB).".into(),
                log: String::new(),
            },
        );
        app.plugins.forms.insert(
            "denoise-gpu".into(),
            json!({"server": "https://gpu.example.org:7871", "port": 7871, "mode": "best",
                   "verify": true}),
        );
    }
    h.run_steps(3);
    h.snapshot_settled("plugins-page");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn enabling_asks_first_and_a_changed_plugin_stays_off() {
    if !enabled() {
        return;
    }
    let _serial = serial();
    let (root, reg) = fixtures();
    let mut h = Harness::builder()
        .with_size(vec2(1440.0, 900.0))
        .with_options(options())
        .wgpu_setup(shared_wgpu())
        .build_eframe(|cc| App::with_ctx(&cc.egui_ctx, None, false));
    {
        let app = h.state_mut();
        page_state(app, &root, reg);
        app.plugins.selected = Some("stats-py".into());
        app.plugins.confirming = Some("stats-py".into());
        // Found but never approved: it does not run.
        assert!(
            app.plugins
                .enabled()
                .iter()
                .all(|l| l.manifest.name != "stats-py")
        );
        // Changed since it was approved: not enabled either.
        let names: Vec<_> = app
            .plugins
            .enabled()
            .iter()
            .map(|l| l.manifest.name.clone())
            .collect();
        assert_eq!(names, ["denoise-gpu", "invert-py"]);
    }
    h.run_steps(3);
    h.snapshot_settled("plugins-enable");
    // Nothing was enabled by drawing the page; only the button does it.
    assert!(
        h.state()
            .plugins
            .enabled()
            .iter()
            .all(|l| l.manifest.name != "stats-py")
    );
    h.state_mut().plugins.enable("stats-py");
    assert!(
        h.state()
            .plugins
            .enabled()
            .iter()
            .any(|l| l.manifest.name == "stats-py")
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn plugin_steps_in_the_stack_step_with_what_an_analysis_found() {
    if !enabled() {
        return;
    }
    let _serial = serial();
    let d = data();
    let (root, mut reg) = fixtures();
    let l = reg.get("stats-py").expect("found").clone();
    reg.approvals.approve(&l).expect("approve");
    let mut h = Harness::builder()
        .with_size(vec2(1440.0, 1700.0))
        .with_options(options())
        .wgpu_setup(shared_wgpu())
        .build_eframe(|cc| App::with_ctx(&cc.egui_ctx, None, false));
    let ctx = h.ctx.clone();
    load(h.state_mut(), &ctx, &d);
    give_result(h.state_mut(), &ctx, &d);
    {
        let app = h.state_mut();
        app.plugins = PluginsUi::with_registry(reg, Vec::new());
        app.plugins.mask = Some(root.clone());
        app.select(Step::Stack);
        app.plugin_pipeline.backend = None;
        app.plugin_pipeline.steps = vec![
            UiStep {
                plugin: "denoise-gpu".into(),
                step: "denoise".into(),
                params: json!({"strength": 0.35}),
            },
            UiStep {
                plugin: "stats-py".into(),
                step: "stats".into(),
                params: json!({"threshold": 0.8}),
            },
        ];
        // The shapes an analysis reported, on the stack: the bright part and a mark at its centre.
        let (w, hgt) = (d.out.image.width as f64, d.out.image.height as f64);
        let (cx, cy, r) = (w / 2.0, hgt / 2.0, hgt * 0.38);
        app.plugin_pipeline.results = vec![(
            StepSpec {
                plugin: "stats-py".into(),
                step: "stats".into(),
                params: json!({"threshold": 0.8}),
                stage: StepStage::PreEdit,
            },
            Analysis {
                metrics: BTreeMap::from([
                    ("max".to_owned(), 1.002_884_5),
                    ("mean".to_owned(), 0.244_170_5),
                    ("std".to_owned(), 0.370_348),
                ]),
                overlays: vec![
                    json!({"kind": "rect", "x": cx - r, "y": cy - r, "w": 2.0 * r, "h": 2.0 * r,
                           "label": "bright part"}),
                    json!({"kind": "circle", "x": cx, "y": cy, "r": 6}),
                ],
                notes: vec!["The bright part is 219 by 219 pixels.".into()],
            },
        )];
        app.plugin_pipeline.draw = true;
    }
    h.run_steps(3);
    h.snapshot_settled("plugins-stack-step");
    // The plan a run takes to the worker holds both steps, checked first.
    let plan = h
        .state()
        .plugin_plan()
        .expect("a valid plan")
        .expect("a plan");
    assert_eq!(plan.specs.len(), 2);
    assert_eq!(plan.specs[0].step, "denoise");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_plan_with_a_plugin_that_is_off_is_refused_in_plain_words() {
    let (root, reg) = fixtures();
    let ctx = eframe::egui::Context::default();
    let mut app = App::with_ctx(&ctx, None, false);
    app.plugins = PluginsUi::with_registry(reg, Vec::new());
    app.plugin_pipeline.steps = vec![UiStep {
        plugin: "stats-py".into(),
        step: "stats".into(),
        params: json!({}),
    }];
    let e = app.plugin_plan().err().expect("refused");
    assert!(e.contains("not turned on"), "{e}");
    assert!(e.contains("What to do") || e.contains("enable"), "{e}");
    // A step of the plugin that changed is refused too.
    app.plugin_pipeline.steps = vec![UiStep {
        plugin: "old-tool".into(),
        step: "look".into(),
        params: json!({}),
    }];
    let e = app.plugin_plan().err().expect("refused");
    assert!(e.contains("changed"), "{e}");
    // True colour cannot use an image step.
    app.plugin_pipeline.steps = vec![UiStep {
        plugin: "denoise-gpu".into(),
        step: "denoise".into(),
        params: json!({"strength": 0.5}),
    }];
    app.settings.true_colour = true;
    assert!(app.plugin_plan().is_err());
    app.settings.true_colour = false;
    assert!(app.plugin_plan().expect("fine").is_some());
    let _ = std::fs::remove_dir_all(&root);
}

/// A plugin offering every capability, in a few lines of Python.
const KIT: &str = r#"
import json, struct, sys, os, array
H = struct.Struct("<4sIIIII")
def rd(w, r):
    raw = open(os.path.join(w, r["path"]), "rb").read()
    _, _, x, y, c, _ = H.unpack_from(raw)
    a = array.array("f"); a.frombytes(raw[24:]); return x, y, c, a
def wr(w, o, x, y, c, a):
    p = os.path.join(w, o["path"])
    open(p, "wb").write(H.pack(b"HLPX", 1, x, y, c, 0) + a.tobytes())
    return {"path": o["path"], "format": "hlpx", "width": x, "height": y, "channels": c}
wd = None
for line in sys.stdin:
    m = json.loads(line)
    if m.get("method") == "exit": break
    if "id" not in m: continue
    p = m.get("params") or {}
    meth = m["method"]
    if meth == "initialize":
        wd = p["workdir"]
        res = {"api_version": 1, "name": "kit", "version": "1",
               "capabilities": ["stack-backend", "frame-op", "image-op", "analyse"],
               "steps": ["double", "plus", "mean"]}
    elif meth == "stack.run":
        a = array.array("f", [float(i % 64) / 64 for i in range(64 * 64)])
        res = {"output": wr(wd, p["output"], 64, 64, 1, a), "note": "kit stacked it"}
    elif meth in ("frame.process", "image.process"):
        x, y, c, a = rd(wd, p["input"])
        k = float((p.get("params") or {}).get("k", 1))
        if p["step"] == "double": a = array.array("f", (v * 2 for v in a))
        else: a = array.array("f", (v + k for v in a))
        res = {"output": wr(wd, p["output"], x, y, c, a)}
    elif meth == "analyse.run":
        x, y, c, a = rd(wd, p["input"])
        res = {"metrics": {"mean": sum(a) / len(a)}, "overlays": [{"kind": "circle", "x": 3, "y": 4, "r": 5}], "notes": ["kit looked"]}
    elif meth == "shutdown":
        res = None
    else:
        sys.stdout.write(json.dumps({"jsonrpc": "2.0", "id": m["id"], "error": {"code": -32601, "message": "no"}}) + "\n"); sys.stdout.flush(); continue
    sys.stdout.write(json.dumps({"jsonrpc": "2.0", "id": m["id"], "result": res}) + "\n"); sys.stdout.flush()
"#;

const KIT_TOML: &str = r#"
name = "kit"
version = "1"
api_version = 1
capabilities = ["stack-backend", "frame-op", "image-op", "analyse"]
executable = "kit.py"
interpreter = "python3"

[[steps]]
id = "double"
capability = "frame-op"
[[steps]]
id = "plus"
capability = "image-op"
[[steps]]
id = "mean"
capability = "analyse"
"#;

fn have_python() -> bool {
    let ok = std::process::Command::new("python3")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success());
    assert!(
        ok || std::env::var_os("HELIA_REQUIRE_PLUGIN_EXAMPLES").is_none(),
        "python3 is required"
    );
    ok
}

/// A small synthetic capture on disk.
fn write_capture(root: &Path) -> PathBuf {
    use helia_core::testutil::{
        CaptureConfig, SyntheticCapture, SyntheticConfig, SyntheticSun, ser_bytes,
    };
    let sun = SyntheticSun::new(&SyntheticConfig {
        width: 96,
        height: 80,
        ..SyntheticConfig::default()
    });
    let cap = SyntheticCapture::generate(
        &sun,
        &CaptureConfig {
            frames: 16,
            noise: 0.02,
            ..CaptureConfig::default()
        },
    );
    let path = root.join("sun.ser");
    std::fs::write(&path, ser_bytes(&cap.frames)).expect("write the capture");
    path
}

/// Pump the worker's events until `done` holds.
fn until(app: &mut App, ctx: &eframe::egui::Context, what: &str, done: impl Fn(&App) -> bool) {
    let t0 = std::time::Instant::now();
    while !done(app) {
        app.pump(ctx);
        assert!(
            t0.elapsed() < std::time::Duration::from_secs(120),
            "timed out waiting for {what}"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

#[test]
fn a_stack_runs_through_plugins_in_the_worker() {
    if !have_python() {
        eprintln!("skipped: python3 is not installed");
        return;
    }
    let root = std::env::temp_dir().join(format!("helia-plugin-run-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let kit = root.join("plugins/kit");
    std::fs::create_dir_all(&kit).expect("folder");
    std::fs::write(kit.join("kit.py"), KIT).expect("program");
    std::fs::write(kit.join("plugin.toml"), KIT_TOML).expect("manifest");
    let mut reg = Registry::discover(Some(&root.join("plugins")), &[], Approvals::in_memory());
    let l = reg.get("kit").expect("found").clone();
    reg.approvals.approve(&l).expect("approve");

    let ctx = eframe::egui::Context::default();
    let mut app = App::with_ctx(&ctx, None, false);
    app.settings.backend = helia_gpu::BackendChoice::Cpu;
    app.plugins = PluginsUi::with_registry(reg, Vec::new());
    app.begin_open(write_capture(&root));
    until(&mut app, &ctx, "the capture to open and score", |a| {
        a.capture.is_some() && a.scoring == crate::model::Scoring::Done
    });

    // Frame, image and analyse steps around the local stack.
    app.plugin_pipeline.steps = vec![
        UiStep {
            plugin: "kit".into(),
            step: "double".into(),
            params: json!({}),
        },
        UiStep {
            plugin: "kit".into(),
            step: "plus".into(),
            params: json!({"k": 10}),
        },
        UiStep {
            plugin: "kit".into(),
            step: "mean".into(),
            params: json!({}),
        },
    ];
    app.start_stack();
    assert!(app.run.is_some(), "the run started");
    until(&mut app, &ctx, "the stack", |a| a.result.is_some());
    let out = app.result.clone().expect("a result");
    // Every frame doubled before stacking, then ten added: the mean is far above a plain stack's.
    assert!(
        out.image.data.iter().sum::<f32>() / out.image.data.len() as f32 > 10.0,
        "the image step ran"
    );
    assert_eq!(app.plugin_pipeline.results.len(), 1);
    let (spec, found) = &app.plugin_pipeline.results[0];
    assert_eq!((spec.plugin.as_str(), spec.step.as_str()), ("kit", "mean"));
    assert!(found.metrics["mean"] > 10.0);
    assert_eq!(found.notes, ["kit looked"]);
    assert!(
        app.plugin_pipeline.draw,
        "shapes were reported, so they are drawn"
    );

    // A stack-backend plugin instead of the local pipeline.
    app.plugin_pipeline.steps.clear();
    app.plugin_pipeline.backend = Some("kit".into());
    app.result = None;
    app.start_stack();
    until(&mut app, &ctx, "the backend's stack", |a| {
        a.result.is_some()
    });
    let out = app.result.clone().expect("a result");
    assert_eq!((out.image.width, out.image.height), (64, 64));
    assert_eq!(out.backend, "plugin kit");
    // The scores of the scoring pass are kept; the plugin made none.
    assert_ne!(app.scores.len(), 0);
    let _ = std::fs::remove_dir_all(&root);
}
