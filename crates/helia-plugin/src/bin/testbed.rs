// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! A plugin the host's tests run end to end. It is a test fixture and is not shipped.
//!
//! Each `image-op` step misbehaves in one way, so a test can check that the host copes: `invert`
//! works, `slow` waits until cancelled, `hang` ignores cancel, `crash` exits, `garbage` prints
//! non-protocol text, `bigline` prints a line over the limit, `badsize` returns a file whose
//! header lies, `traverse` names a path outside the working folder, `fail` returns a plain-English
//! error, `grow` doubles the size.

use std::io::Write;
use std::time::Duration;

use helia_plugin_sdk::{
    AnalyseRequest, AnalyseResult, Ctx, Image, Info, Plugin, PluginError, ProcessRequest,
    ProcessResult, StackRequest, StackResult, TestResult, run,
};
use serde_json::Value;

struct Testbed;

fn ms(p: &Value, default: u64) -> u64 {
    p.get("ms").and_then(Value::as_u64).unwrap_or(default)
}

impl Plugin for Testbed {
    fn info(&self) -> Info {
        Info::new(
            "testbed",
            "0.1.0",
            &["stack-backend", "frame-op", "image-op", "analyse"],
        )
        .with_steps(&["invert", "addn", "stats"])
    }

    fn test(&mut self, _ctx: &Ctx, settings: &Value) -> Result<TestResult, PluginError> {
        let who = settings
            .get("who")
            .and_then(Value::as_str)
            .unwrap_or("nobody");
        Ok(TestResult {
            ok: true,
            message: format!("Hello {who}"),
        })
    }

    fn stack(&mut self, ctx: &Ctx, req: StackRequest) -> Result<StackResult, PluginError> {
        for i in 0..4 {
            ctx.check_cancelled()?;
            ctx.progress(f64::from(i) / 4.0, &format!("Stacking {}", i * 25));
        }
        let data: Vec<f32> = (0..64).map(|i| i as f32).collect();
        let img = Image::new(8, 8, 1, data).map_err(|e| PluginError::failed(e.to_string()))?;
        let out = ctx.write_image(&req.output, &img)?;
        Ok(StackResult {
            output: out,
            note: Some(format!("stacked {}", req.input)),
            meta: Some(serde_json::json!({"provenance": {"model": "testbed"}})),
        })
    }

    fn frame_process(
        &mut self,
        ctx: &Ctx,
        req: ProcessRequest,
    ) -> Result<ProcessResult, PluginError> {
        let add = req.params.get("add").and_then(Value::as_f64).unwrap_or(1.0) as f32;
        let mut img = ctx.read_image(&req.input)?;
        for v in &mut img.data {
            *v += add;
        }
        Ok(ProcessResult::new(ctx.write_image(&req.output, &img)?))
    }

    fn image_process(
        &mut self,
        ctx: &Ctx,
        req: ProcessRequest,
    ) -> Result<ProcessResult, PluginError> {
        let img = ctx.read_image(&req.input)?;
        match req.step.as_str() {
            "invert" => {
                let mut img = img;
                let (lo, hi) = img.range();
                let n = 10;
                for i in 0..n {
                    ctx.check_cancelled()?;
                    ctx.progress(f64::from(i) / f64::from(n), "Inverting");
                }
                for v in &mut img.data {
                    *v = hi + lo - *v;
                }
                Ok(ProcessResult::new(ctx.write_image(&req.output, &img)?))
            }
            "grow" => {
                let (w, h) = (img.width * 2, img.height * 2);
                let mut data = Vec::with_capacity(w * h);
                for y in 0..h {
                    for x in 0..w {
                        data.push(img.data[(y / 2) * img.width + x / 2]);
                    }
                }
                let big =
                    Image::new(w, h, 1, data).map_err(|e| PluginError::failed(e.to_string()))?;
                Ok(ProcessResult::new(ctx.write_image(&req.output, &big)?))
            }
            "slow" => {
                let end =
                    std::time::Instant::now() + Duration::from_millis(ms(&req.params, 30_000));
                while std::time::Instant::now() < end {
                    ctx.check_cancelled()?;
                    std::thread::sleep(Duration::from_millis(10));
                }
                Ok(ProcessResult::new(ctx.write_image(&req.output, &img)?))
            }
            "quiet" => {
                std::thread::sleep(Duration::from_millis(ms(&req.params, 3000)));
                Ok(ProcessResult::new(ctx.write_image(&req.output, &img)?))
            }
            "hang" => {
                std::thread::sleep(Duration::from_secs(3600));
                Ok(ProcessResult::new(ctx.write_image(&req.output, &img)?))
            }
            "crash" => {
                eprintln!("boom: the testbed is crashing on purpose");
                std::process::exit(3);
            }
            "garbage" => {
                println!("this is not JSON");
                let _ = std::io::stdout().flush();
                std::thread::sleep(Duration::from_secs(5));
                Err(PluginError::failed("unreachable"))
            }
            "bigline" => {
                let mut out = std::io::stdout().lock();
                let _ = out.write_all(&vec![b'x'; 5 << 20]);
                let _ = out.write_all(b"\n");
                let _ = out.flush();
                std::thread::sleep(Duration::from_secs(5));
                Err(PluginError::failed("unreachable"))
            }
            "badsize" => {
                // A header that claims the right size on a file that is one float short.
                let mut bytes = img.encode();
                bytes.truncate(bytes.len() - 4);
                let p = ctx.workdir().join(&req.output.path);
                std::fs::write(&p, bytes)?;
                Ok(ProcessResult::new(helia_plugin_sdk::BufferRef {
                    path: req.output.path.clone(),
                    format: "hlpx".into(),
                    width: img.width as u32,
                    height: img.height as u32,
                    channels: 1,
                }))
            }
            "traverse" => Ok(ProcessResult::new(helia_plugin_sdk::BufferRef {
                path: "../evil.hlpx".into(),
                format: "hlpx".into(),
                width: 1,
                height: 1,
                channels: 1,
            })),
            "fail" => Err(
                PluginError::new(helia_plugin_sdk::code::UNAVAILABLE, "no server").explain(
                    "I could not reach the stacking server.",
                    "The connection to the server was refused.",
                    "Check that the tunnel is open, then try again.",
                ),
            ),
            other => Err(PluginError::invalid_params(format!("no such step {other}"))),
        }
    }

    fn analyse(&mut self, ctx: &Ctx, req: AnalyseRequest) -> Result<AnalyseResult, PluginError> {
        let img = ctx.read_image(&req.input)?;
        let (lo, hi) = img.range();
        let mean = img.data.iter().map(|&v| f64::from(v)).sum::<f64>() / img.data.len() as f64;
        let mut r = AnalyseResult::default();
        r.metrics.insert("min".into(), f64::from(lo));
        r.metrics.insert("max".into(), f64::from(hi));
        r.metrics.insert("mean".into(), mean);
        r.overlays.push(serde_json::json!({"kind": "circle", "x": img.width / 2, "y": img.height / 2, "r": 3, "label": "centre"}));
        r.notes.push("testbed".into());
        Ok(r)
    }
}

fn main() {
    std::process::exit(run(Testbed));
}
