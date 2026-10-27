// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: MIT OR Apache-2.0

//! The smallest useful Helia plugin, in Rust with the SDK: one `image-op` step that inverts the
//! brightness of an image (each value `v` becomes `max + min - v`).
//!
//! It shows the four things every plugin does: say what it offers (`info`), read the input
//! picture, report progress and honour cancel, and write the result where the host asked.

use helia_plugin_sdk::{Ctx, Info, Plugin, PluginError, ProcessRequest, ProcessResult, run};

struct Invert;

impl Plugin for Invert {
    fn info(&self) -> Info {
        Info::new("invert", env!("CARGO_PKG_VERSION"), &["image-op"]).with_steps(&["invert"])
    }

    fn image_process(
        &mut self,
        ctx: &Ctx,
        req: ProcessRequest,
    ) -> Result<ProcessResult, PluginError> {
        let mut img = ctx.read_image(&req.input)?;
        let (lo, hi) = img.range();
        let total = img.data.len().max(1);
        for (i, v) in img.data.iter_mut().enumerate() {
            *v = hi + lo - *v;
            // Look for a cancel and tell the host how far on we are, about 20 times a run.
            if i % (total / 20 + 1) == 0 {
                ctx.check_cancelled()?;
                ctx.progress(i as f64 / total as f64, "Inverting");
            }
        }
        ctx.progress(1.0, "Inverted");
        Ok(ProcessResult::new(ctx.write_image(&req.output, &img)?))
    }
}

fn main() {
    std::process::exit(run(Invert));
}
