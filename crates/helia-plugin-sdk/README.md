# helia-plugin-sdk

Write a [Helia](https://github.com/qisback/helia) plugin in Rust. The crate implements the plugin
side of the plugin protocol (API version 1): JSON-RPC 2.0 lines on standard input and output,
progress, cancellation, shared pixel files and plain-English errors. The protocol is specified in
the Helia book under *Developer guide, Plugins: design and protocol*; a plugin does not need this
crate, it only saves you the plumbing.

Licence: MIT OR Apache-2.0, so a plugin that uses it can have any licence, including a closed one.
This crate depends on no Helia crate. See `LICENSE-MIT` and `LICENSE-APACHE`.

```rust
use helia_plugin_sdk::{run, Ctx, Info, Plugin, PluginError, ProcessRequest, ProcessResult};

struct Invert;

impl Plugin for Invert {
    fn info(&self) -> Info {
        Info::new("invert", "0.1.0", &["image-op"]).with_steps(&["invert"])
    }
    fn image_process(&mut self, ctx: &Ctx, req: ProcessRequest) -> Result<ProcessResult, PluginError> {
        let mut img = ctx.read_image(&req.input)?;
        let (lo, hi) = img.range();
        for v in &mut img.data { *v = hi + lo - *v; }
        ctx.progress(1.0, "Inverted");
        Ok(ProcessResult::new(ctx.write_image(&req.output, &img)?))
    }
}

fn main() { std::process::exit(run(Invert)); }
```
