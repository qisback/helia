# Tutorial: writing a plugin

**Goal:** write a Helia plugin from nothing, first in plain Python (no SDK, nothing to install), then in
Rust with the SDK, run it by hand to see the protocol, and run it inside Helia.

**You need:** Python 3 for the first half; a Rust toolchain for the second. The finished plugins are in
`examples/plugins` in the Helia source (`invert-py`, `stats-py` and `invert`), under the MIT OR Apache-2.0
licence, so copy them freely and give your plugin any licence you like, including a closed one. Read
[Using plugins](plugins.md) first if you have not enabled a plugin yet, and keep the
[specification](../developer/plugins.md) open; this page is the walk-through, that one is the contract.

Everything below was run on the build at the head of the plugin pull requests.

## The idea in one minute

A plugin is a folder with two things: a **manifest** (`plugin.toml`) that says what the plugin is and what
it offers, and a **program** that Helia starts. Helia and the program talk by sending one JSON message per
line to each other (on the program's standard input and output), and pictures go through files in a folder
Helia makes for the call. The program can be written in any language that can read a line, parse JSON and
write a file.

This plugin will offer one **step**: `invert`, an `image-op` that turns each pixel value `v` into
`max + min - v`.

## Part 1: Python, no SDK

### 1. The manifest

Make a folder `invert-py` with this `plugin.toml`:

```toml
name = "invert-py"
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
gpu = false
memory_mb = 256

[[steps]]
id = "invert"
capability = "image-op"
title = "Invert"
description = "Replaces each value v by (max + min) - v."
```

* `name` is how the plugin is called on the command line (`invert-py/invert`); lower-case letters, digits,
  `-` and `_`.
* `api_version = 1` is the protocol version. A Helia that does not speak it lists the plugin as unusable
  rather than running it.
* `capabilities` says what the plugin can do; `[[steps]]` lists each step, with its own capability.
* `executable` is relative to this folder. `interpreter` is for scripts: Helia runs
  `python3 invert.py`. A compiled plugin leaves it out. (On Windows the Python launcher is usually
  `python`; set `interpreter` to the name you have.)
* `[resources]` are hints shown when you approve the plugin; they limit nothing.

[Every manifest field](../reference/plugins.md#the-manifest-plugintoml) has its range and default in the
reference.

### 2. Say hello: `initialize`

Helia's first message is `initialize`. The plugin answers with who it is. Start `invert.py`:

```python
API_VERSION = 1
NAME = "invert-py"
VERSION = "0.1.0"

def initialize(self, params):
    if params.get("api_version") != API_VERSION:
        raise Failure(-32602, "This plugin speaks API version 1.")
    self.workdir = params["workdir"]
    return {"api_version": API_VERSION, "name": NAME, "version": VERSION,
            "capabilities": ["image-op"], "steps": ["invert"]}
```

`workdir` is the private folder where every picture of this session lives; keep it. The answer must agree
with the manifest: the name, the version of the protocol, and the capabilities the plugin really has.

### 3. Read the line, write the line

The loop is the whole protocol transport. In `invert.py` a thread reads standard input so that a `cancel`
message is noticed while the plugin is busy, and the main loop answers requests one at a time:

```python
def send(obj):
    # One JSON value on one line. Standard output carries protocol lines only.
    sys.stdout.write(json.dumps(obj, separators=(",", ":")) + "\n")
    sys.stdout.flush()
```

Two rules matter: **standard output carries protocol lines only** (print diagnostics to standard error,
which Helia keeps as the plugin's log), and **flush after every line**.

### 4. Do the work: `image.process`

Helia names an input picture and says where the result must go:

```json
{"jsonrpc":"2.0","id":2,"method":"image.process","params":{"step":"invert","params":{},
 "input":{"path":"in.hlpx","format":"hlpx","width":3,"height":2,"channels":1},
 "output":{"path":"out.hlpx","format":"hlpx"}}}
```

Pictures are `hlpx` files: a 24-byte header (`HLPX`, a layout version, width, height, channels and a
reserved word, all little-endian 32-bit) followed by the samples as little-endian 32-bit floats, row by row.
Reading and writing them in Python:

```python
HEADER = struct.Struct("<4sIIIII")  # magic, layout version, width, height, channels, reserved

def read_image(workdir, ref):
    with open(inside(workdir, ref["path"]), "rb") as f:
        raw = f.read()
    magic, version, width, height, channels, _ = HEADER.unpack_from(raw)
    if magic != b"HLPX" or version != 1:
        raise Failure(-32602, "Not an hlpx file.")
    if len(raw) != HEADER.size + 4 * width * height * channels:
        raise Failure(-32602, "The file size does not match its header.")
    data = array.array("f")
    data.frombytes(raw[HEADER.size:])
    return width, height, channels, data
```

The `inside` helper refuses a path that leaves the working folder. Helia checks its own paths the same way;
do it too, because a plugin is also a program that reads files it was told to.

Write the result to a temporary name and rename it, so that a crash never leaves a half file under the real
name:

```python
tmp = path + ".part"
with open(tmp, "wb") as f:
    f.write(HEADER.pack(b"HLPX", 1, width, height, channels, 0))
    f.write(data.tobytes())
os.replace(tmp, path)
```

The answer names the file, with the **true** size (a super-resolution plugin returns a bigger picture):

```json
{"jsonrpc":"2.0","id":2,"result":{"output":{"path":"out.hlpx","format":"hlpx","width":3,"height":2,"channels":1}}}
```

The values are on Helia's own scale, which for a stack is about 0 to 1 (they can go outside it). `NaN` and
infinity are replaced by 0 when Helia reads your result.

### 5. Report progress and honour cancel

A long step should tell the user it is alive and stop when asked. Send `progress` notifications (no `id`):

```json
{"jsonrpc":"2.0","method":"progress","params":{"id":2,"fraction":0.5,"status":"Inverting"}}
```

Any message from the plugin also restarts Helia's inactivity timer (120 seconds unless the manifest says
otherwise), so a plugin that is busy but silent for longer is treated as hung. When the user cancels, Helia
sends `{"method":"cancel","params":{"id":2}}`; stop soon and answer request 2 with error `-32800`. If you
have not answered within 5 seconds, Helia kills the plugin. `invert.py` checks a flag between slices of the
picture.

### 6. Say it plainly when it goes wrong

Helia's standard for errors is **what happened, why, what to do**, and a plugin fills `data` the same way:

```json
{"jsonrpc":"2.0","id":2,"error":{"code":-32010,"message":"The server did not answer.",
 "data":{"what":"I could not reach the stacking server.",
         "why":"The connection to 127.0.0.1:7871 was refused.",
         "todo":"Check that the SSH tunnel is open, then try again."}}}
```

Pick the closest code from the [error table](../developer/plugins.md#errors) (unavailable `-32010`,
unauthorised `-32011`, unsupported input `-32012`, resource `-32013`, invalid settings `-32014`, failed
`-32000`). Helia shows your sentences as they are and fills in any part you leave out.

### 7. Talk to it by hand

You do not need Helia to test a plugin. Make a 3 by 2 picture in a folder and pipe four messages to the
program:

```console
$ printf '%s\n' \
  '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"api_version":1,"workdir":"/tmp/w","settings":{}}}' \
  '{"jsonrpc":"2.0","id":2,"method":"image.process","params":{"step":"invert","params":{},"input":{"path":"in.hlpx","format":"hlpx","width":3,"height":2,"channels":1},"output":{"path":"out.hlpx","format":"hlpx"}}}' \
  '{"jsonrpc":"2.0","id":3,"method":"shutdown"}' \
  '{"jsonrpc":"2.0","method":"exit"}' | python3 invert.py
{"jsonrpc":"2.0","id":1,"result":{"api_version":1,"name":"invert-py","version":"0.1.0","capabilities":["image-op"],"steps":["invert"]}}
{"jsonrpc":"2.0","method":"progress","params":{"id":2,"fraction":0.16666666666666666,"status":"Inverting"}}
...
{"jsonrpc":"2.0","method":"progress","params":{"id":2,"fraction":1.0,"status":"Inverting"}}
{"jsonrpc":"2.0","id":2,"result":{"output":{"path":"out.hlpx","format":"hlpx","width":3,"height":2,"channels":1}}}
{"jsonrpc":"2.0","id":3,"result":null}
```

With `in.hlpx` holding the values 0, 1, 2, 3, 4, 5, the file `out.hlpx` holds 5, 4, 3, 2, 1, 0.

### 8. Run it in Helia

```console
$ helia --plugin invert-py plugins info invert-py        # look at the checksum
$ helia --plugin invert-py plugins enable invert-py       # approve it
$ helia --plugin invert-py plugins test invert-py          # start it, run its check, show its log
$ helia --plugin invert-py stack capture.ser -o out.tif --colour mono --plugin-step invert-py/invert
```

`plugins test` answers "no check offered" for this plugin because it does not implement `test` (it answers
`-32601`, which Helia treats as "none"). A plugin that needs a server implements `test` to say whether it
can reach it, and the **Test** button and `helia plugins test` show its answer.

Every time you change `invert.py` the checksum changes and Helia switches the plugin off until you enable it
again; that is the safety working, not a bug. While developing, `helia plugins enable NAME --yes` is the
quick way.

### An analyse plugin

`stats-py` is the other example. An `analyse` step reads a picture and returns numbers and shapes instead
of a picture:

```json
{"jsonrpc":"2.0","id":2,"result":{
  "metrics":{"mean":0.244,"max":1.003},
  "overlays":[{"kind":"rect","x":496,"y":172,"w":728,"h":744,"label":"bright part"}],
  "notes":["The bright part is 728 by 744 pixels."]}}
```

and a parameter form in its manifest (`params_schema = "stats.params.json"`), a JSON Schema that gives the
`threshold` its title, range and default; Helia draws the form from it and refuses a value outside the range
before calling the plugin. The shapes are `circle`, `rect`, `line`, `points` and `text`, in pixels of the
picture, origin top-left.

## Part 2: Rust with the SDK

The Rust SDK, `helia-plugin-sdk`, does the plumbing: the JSON-RPC loop, a thread that watches for `cancel`,
progress, the picture files and the error type. It is licensed MIT OR Apache-2.0 and depends on no Helia
crate, so your plugin can have any licence.

### 1. The crate

```toml
[package]
name = "my-invert"
version = "0.1.0"
edition = "2024"
license = "MIT OR Apache-2.0"

[dependencies]
helia-plugin-sdk = { path = "path/to/helia/crates/helia-plugin-sdk" }
```

(Inside the Helia repository the example, `examples/plugins/invert`, is a member of the workspace, so
`cargo build -p helia-example-invert` builds it with everything else.)

### 2. The program

```rust
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
```

* `info` is the answer to `initialize`; the SDK checks the protocol version for you.
* `read_image` checks the path, the header and the size before it reads, and replaces `NaN` and infinity;
  `write_image` writes to a temporary name and renames, and returns the reference to send back.
* `check_cancelled` returns the cancelled error if Helia asked; the `?` ends the call cleanly.
* A method you do not implement is answered "method not found" for you. The other methods you can implement
  are `stack` (a `stack-backend`), `frame_process`, `analyse` and `test`.
* A panic in your code is caught and sent to Helia as an internal error, so the plugin stays alive.

### 3. The manifest and running it

The manifest is the one from part 1 with `executable = "invert"` and no `interpreter`. Build, copy the
program next to the manifest and enable it:

```console
$ cargo build --release -p helia-example-invert
$ cp target/release/helia-example-invert examples/plugins/invert/invert
$ helia --plugin examples/plugins/invert plugins enable invert
$ helia --plugin examples/plugins/invert stack capture.ser -o out.tif --colour mono --plugin-step invert/invert
```

For a compiled plugin, publish the program's SHA-256 next to the download and put it in the manifest as
`sha256 = "..."`: Helia then refuses a program that does not match, which catches a damaged download
before you are even asked to approve it.

### 4. Test it end to end

The example has a test (`examples/plugins/invert/tests/protocol.rs`) that starts the compiled program,
sends `initialize`, `image.process` and `shutdown` the way Helia does, writes a picture with the SDK's
`Image::encode`, and checks the file that comes back. The SDK's `run_with` takes any reader and writer, so
you can also test your `Plugin` without starting a process.

## Where to go next

* Offer settings (a server address, a token): add `settings_schema` to the manifest. Helia draws the form,
  checks the values, keeps secrets out of logs and hands the values to `initialize`.
* A `frame-op` step runs once per frame, so keep your process warm between calls (it is one process per
  run) and load models in `initialize`.
* A `stack-backend` plugin receives the capture's absolute path and the job in the vocabulary of the
  [API](../user-guide/api.md); it writes the stack and answers with a note for the summary.
* Say how you made a result. A step that uses a model or a service can return `meta` next to `output`:
  `{"output": {...}, "meta": {"provenance": {"model": "denoise-v3", "sha256": "...", "strength": 0.5}}}`
  (in Rust, `ProcessResult::new(out).with_meta(json!({...}))`). Helia writes it into the saved file's history,
  after a line that names your plugin, its version and the checksum of the program that ran, so a user can tell
  later that a picture went through your step. Keep it short: at most 900 characters are kept.
* Offer updates: an `[update]` section in the manifest names where newer versions are announced and the key
  they are signed with. Helia reads and validates it; the automatic-update mechanism does the rest.
* Read the [specification](../developer/plugins.md) for the rules this page skipped: limits on message size,
  timeouts, the full list of error codes and how the protocol changes between versions.
