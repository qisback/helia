# Plugins: design and protocol

Helia can be extended by **plugins**: separate programs that Helia starts, talks to in JSON, and
stops. A plugin can replace the stacking step with its own (for example one that does the work on
another computer), add a processing step to the pipeline (denoise, deconvolve, super-resolution,
a mask), or analyse an image and report numbers and overlays. Helia knows nothing about what a
plugin does inside; it only speaks the protocol on this page.

This page is the specification. The tutorials are [Using plugins](../tutorials/plugins.md) and
[Writing a plugin](../tutorials/writing-plugins.md); the reference for the manifest, command line and
screens is [Plugins reference](../reference/plugins.md).

Status: **API version 1**. The text below is the contract. Anything marked *reserved* is not
used by version 1 and must be ignored.

## Design in ten lines

1. A plugin is an executable plus a manifest, `plugin.toml`, in a folder.
2. It runs as a **child process** of Helia and is crash-isolated: if it dies, hangs or is
   cancelled, Helia reports a plain-English error and carries on.
3. Control messages are **JSON-RPC 2.0**, one message per line (newline-delimited JSON) on the
   plugin's standard input and output. Standard error is the plugin's log.
4. Pixels never travel in the messages. They go through **shared files** in a private working folder
   (raw 32-bit float, a 24-byte header, optionally memory-mapped), named in the message.
5. Plugins are language-agnostic: anything that can read a line, parse JSON and write a file works.
6. A plugin declares **capabilities**: `stack-backend`, `frame-op`, `image-op`, `analyse`.
7. **Nothing loads silently.** A plugin is listed after discovery but runs only after the user
   enables it; enabling records the SHA-256 of the executable (and of the manifest) the user approved.
   A changed hash disables the plugin until it is approved again.
8. **Plugins run with your rights**, not in a sandbox. Enable only plugins you trust.
9. The host-side parsers (manifest, JSON-RPC lines, buffer headers, paths) are fuzzed and have a
   malicious-input test suite.
10. The protocol is versioned by one integer, `api_version`; changes within a version are additive only.

## Discovery, approval and trust

### Where plugins are found

* The per-user plugins folder: `<settings>/plugins/<name>/plugin.toml`, where `<settings>` is
  `$HELIA_CONFIG_DIR` if set, else `%APPDATA%\helia` on Windows, else `$XDG_CONFIG_HOME/helia`, else
  `~/.config/helia`.
* Any folder or manifest named with the repeatable command line flag `--plugin <dir-or-manifest>`
  (on `helia` and `helia-gui`), for plugins you are developing or keep elsewhere.

Finding a plugin only **lists** it. Helia reads and validates the manifest, never runs the
executable, until the plugin is enabled.

### Approval (`plugins.toml`)

`<settings>/plugins.toml` records what you approved (mode 0600 on Unix):

```toml
[[plugin]]
name = "invert"
manifest = "/home/me/.config/helia/plugins/invert/plugin.toml"
sha256 = "3f…"            # SHA-256 of the executable you approved
manifest_sha256 = "9a…"   # SHA-256 of the manifest and its schema files
enabled = true
approved = "2026-10-22T18:04:00Z"

[plugin.settings]          # values of the plugin's settings (may hold secrets)
token = "…"
```

Rules, all enforced by the host before a process is started:

1. A plugin whose name is not in `plugins.toml`, or whose `enabled` is false, is never started.
2. At every start Helia hashes the executable and the manifest. If either differs from the recorded
   hash, the plugin is **disabled** and shown as "changed, approve again". It does not start.
3. The manifest also states `sha256` of the executable. If it does not match the file, the plugin
   is refused as damaged (this catches a corrupt download before you are asked to approve it).
4. Approving (GUI: **Enable**; CLI: `helia plugins enable NAME`) shows the source path, the
   hash, the capabilities, the resource hints and the plain statement that the plugin runs with your
   rights, and asks you to confirm. The CLI asks `Enable this plugin? [y/N]`; `--yes` skips the
   question for scripts and still records the hash.
5. The `signature` manifest field is *reserved* in version 1. Helia shows "unsigned" and does not
   verify it. Signature checking, when added, will only ever make approval stricter.

### What a plugin can do

A plugin is a program. It runs **with your rights**: it can read and write your files, use the
network, and run other programs. Helia passes it only what the call needs, and checks what it sends
back, but it does not contain it. This is the same trust as running any program you download.
Enable plugins from people you trust, and check the hash against the author's published one.

## The manifest, `plugin.toml`

```toml
name = "invert"                 # [a-z0-9][a-z0-9_-]{0,31}, unique
version = "0.1.0"               # the plugin's own version
api_version = 1                 # the protocol major version it speaks
title = "Invert"                # shown in lists
description = "Inverts the brightness of an image."
author = "Laurence Horrocks-Barlow"
licence = "MIT"                 # SPDX expression, informational
homepage = "https://example.org/invert"   # informational
capabilities = ["image-op"]
executable = "invert"           # relative to this folder: no "..", no absolute path
interpreter = "python3"         # optional: a program on the PATH that runs the executable (a script)
args = []                       # fixed arguments, optional
files = ["helper.py"]           # optional: further files the program needs; covered by the approval
sha256 = "…64 hex digits…"      # optional but recommended: SHA-256 of the executable; if given it must match
signature = ""                  # reserved
settings_schema = "settings.schema.json"  # optional, relative; JSON Schema for the settings form

[resources]                     # hints only: Helia shows them and uses them to warn
gpu = false                     # the plugin wants a GPU
memory_mb = 256                 # typical peak resident memory

[limits]                        # optional, host defaults shown
timeout_s = 120                 # a call fails after this long with no progress
max_total_s = 86400             # absolute limit for one call

[update]                        # optional: where newer versions of this plugin are published
manifest_url = "https://example.org/invert/update.json"   # https only, no user name
signing_key = "…64 hex digits…"  # Ed25519 public key the update manifest must be signed with
channel = "stable"              # optional release channel

[[steps]]                       # one per processing step the plugin offers
id = "invert"                   # [a-z0-9][a-z0-9_-]{0,31}, unique in the plugin
capability = "image-op"         # frame-op, image-op or analyse
title = "Invert"
description = "Replaces each value v by (max + min) - v."
params_schema = "invert.params.json"      # optional JSON Schema for this step's parameters
```

* `capabilities` lists what the plugin offers: `stack-backend`, `frame-op`, `image-op`, `analyse`.
  A `frame-op`, `image-op` or `analyse` plugin lists its steps in `[[steps]]`. A `stack-backend`
  plugin needs no step.
* `[update]` only **declares** where updates come from. Helia validates the three fields when it
  reads the manifest (an `https://` address with no user name, a 64-digit hex key, a short channel
  word) and makes them available to the automatic-update mechanism; this page does not define how an
  update is fetched or applied. An updated plugin has a new hash, so it is disabled until you approve
  it again, exactly like any other change.
* Strings are limited to 4 KiB, the manifest to 256 KiB and each schema file to 256 KiB. Unknown keys
  are ignored (so newer manifests load) and shown as a note.
* The manifest's own hash covers `plugin.toml`, the schema files it names and the extra `files`.
* **Scripts**: set `executable` to the script (it is hashed and approved like a program) and
  `interpreter` to a bare program name such as `python3`. Helia runs `interpreter executable args…`,
  looking the interpreter up on the `PATH`. The interpreter itself is not hashed; the approval screen
  names it so you can see what will run.
* `sha256` in the manifest is a convenience for the author: Helia refuses a program that does not
  match it (a damaged download). The approval records the hash Helia computed, whether or not the
  manifest states one. `helia plugins info NAME` prints the hash.

### Settings schema

`settings_schema` is a JSON Schema (draft 2020-12 subset) describing the plugin's **settings**: values
that belong to the plugin as a whole, such as a server address or a token. Helia draws the form from
it and validates the values before a call. Supported keywords:

| Keyword | Meaning |
|---|---|
| `type` | `object` at the top, then `string`, `number`, `integer`, `boolean` for properties |
| `properties`, `required` | the fields; each property is one control |
| `title`, `description`, `default` | label, help line, initial value |
| `enum` | a choice box |
| `minimum`, `maximum` | numeric range (a slider when both are given) |
| `minLength`, `maxLength`, `pattern` | string limits; `pattern` is checked by the plugin only |
| `format` | `password` hides the value; `path` adds a Browse button; `uri` is a hint |
| `x-secret` | `true`: the value is kept in `plugins.toml` and never printed in logs |

A schema is optional: with none, any settings or parameters the user gives are passed through
unchecked (as an object). Property order is by `x-order` (an integer), then by name. Other keywords
are ignored by the form and passed to nobody. A step's `params_schema` has the same
shape and describes the **parameters** of that step, which belong to a pipeline entry and can differ
from one use to the next (a strength, a number of iterations).

## Transport

* The host starts `executable` with `args`, working folder = the manifest's folder, standard input
  and output piped, standard error piped to the log. The environment is the user's, with
  `HELIA_PLUGIN_API=1` added.
* **Framing**: each message is one JSON value encoded as UTF-8 on **one line**, ended by `\n`
  (`\r\n` is accepted). JSON text that contains no raw newline (all compact JSON) satisfies this.
  There are no batches (a JSON array is an invalid request).
* **Limits the host enforces**: a line is at most **4 MiB** (a longer line is a protocol error and
  the plugin is stopped); nesting depth at most **32**; a string at most 4 MiB. Pixels do not
  go in messages, so these are generous.
* **Standard output carries protocol lines only.** Anything else is a protocol error. Plugins that
  print diagnostics use standard error. The host keeps the last 64 KiB of standard error as the
  plugin's log, shown in the GUI and printed by `helia plugins test NAME`.
* **One request at a time**: the host sends the next request only after the response to the last.
  Notifications (`progress`, `log`, `cancel`) may arrive at any time.
* Standard JSON-RPC 2.0 rules apply: `"jsonrpc":"2.0"`, requests have `id` (an integer chosen by the
  host), responses carry `result` or `error`, notifications have no `id`.

## Session

```
host                                   plugin
 |  spawn                               |
 |--- initialize ---------------------->|
 |<-- result: name, version, caps ------|
 |--- (test | stack.run | frame.process | image.process | analyse.run) -->|
 |<-- progress / log notifications -----|
 |<-- result or error ------------------|
 |--- shutdown ------------------------>|
 |<-- result null ----------------------|
 |--- exit (notification) ------------->|   plugin exits; host kills it after 2 s
```

### `initialize`

Params:

```json
{"api_version": 1,
 "host": {"name": "helia", "version": "0.8.0"},
 "workdir": "/tmp/helia-plugin-1234-abcd",
 "settings": {"server": "https://example"},
 "limits": {"max_buffer_bytes": 4294967296},
 "locale": "en-GB"}
```

Result:

```json
{"api_version": 1, "name": "invert", "version": "0.1.0",
 "capabilities": ["image-op"], "steps": ["invert"]}
```

* `api_version` is the major version the plugin will speak. It must equal the host's; otherwise the
  host stops the plugin and reports the mismatch. The host and manifest `api_version` must also agree.
* `capabilities` is what the plugin implements **now**. The host uses the intersection with the
  manifest. A capability in the manifest that the plugin does not confirm is reported and unusable;
  a capability the plugin adds beyond its manifest is ignored.
* `workdir` is a private folder (mode 0700) created by the host for this session and removed when the
  session ends. All pixel files live here.
* `settings` are the user's values, already validated against the schema.

### `shutdown` and `exit`

`shutdown` (no params) asks the plugin to finish and answer `null`. Then the host sends the
notification `exit`; the plugin exits. If it has not exited 2 seconds later, the host kills it.

### `test`

`test` is optional: Params `{"settings":{…}}`. Result `{"ok":true,"message":"Connected to hex (A100)"}`.
Used by the Test button and `helia plugins test`. A plugin without a test answers method-not-found,
and Helia treats it as "no test offered".

## Pixel buffers

Pixels are passed as files in `workdir`. A **buffer reference** in a message is:

```json
{"path": "in-0001.hlpx", "format": "hlpx", "width": 1920, "height": 1080, "channels": 1}
```

* `path` is **relative to `workdir`**, a single file name or a sub-path with no `..`, no absolute
  path, no drive letter or colon, no NUL, no backslash, at most 128 bytes, and it must resolve inside
  `workdir` (the host also refuses links that leave it). A path that breaks this is a protocol error.
* `format` is `"hlpx"` in every version-1 call. A call may allow `"fits"` where it says so
  (`stack.run` results).
* `width`, `height`, `channels` must match the file header and are the host's check that nothing lied.

### The `hlpx` file layout

All integers and floats are **little-endian**.

| Offset | Size | Field |
|---|---|---|
| 0 | 4 | magic `HLPX` |
| 4 | 4 | `u32` layout version, 1 |
| 8 | 4 | `u32` width, 1 to 65536 |
| 12 | 4 | `u32` height, 1 to 65536 |
| 16 | 4 | `u32` channels, 1 to 4 (1 = mono, 3 = RGB, 4 = RGBA) |
| 20 | 4 | `u32` reserved, 0 |
| 24 | `4 * width * height * channels` | `f32` samples, row-major, channels interleaved |

The file size must be exactly `24 + 4 * width * height * channels`. The host checks it **before**
mapping or reading (with an overflow-safe multiplication) and refuses a file that is short, long, or
larger than `limits.max_buffer_bytes`. Samples are the same scale Helia uses internally (a stack is
about 0 to 1; values may be outside it). NaN and infinity are replaced by 0 when the host reads a
result. A reader may read the file normally or memory-map it; the host never writes to a file the
plugin owns, and the plugin must not modify an input file (the host may keep it read-only).

Direction: the host writes **input** files and names them; for a result the host names the
**output** path it wants (`output.path`) and the plugin writes there and returns the reference
(with the true dimensions, since a plugin may change the size). A plugin must write the file
completely and only then answer; write to a temporary name and rename it so a crash never leaves a
half file named as a result.

## Capabilities and methods

### `stack-backend`: `stack.run`

Replaces the local stacking step. Params:

```json
{"settings": {…},
 "input": "/home/me/captures/sun.ser",
 "job": {"keep": "25%", "metric": "gradient", "...": "…"},
 "output": {"path": "stack-000001.hlpx", "format": "hlpx", "accept": ["hlpx", "fits"]}}
```

* `input` is the **absolute path of the capture**, readable by the plugin (it runs with the user's
  rights). Large captures are not copied.
* `job` is a stacking job in the vocabulary of the [API](../user-guide/api.md): the `config` of
  `POST /api/v1/jobs`. The host refuses settings a backend cannot honour (true colour, calibration,
  field rotation) before the call and says why.
* The plugin writes the unedited mono stack and answers
  `{"output": {"path": "stack-000001.hlpx", "format": "hlpx", "width": 1920, "height": 1080,
  "channels": 1}, "note": "upload 12 s, job 3 s, download 2 s"}`. `note` is shown in the summary.
  The usual way is an `hlpx` file at `output.path`. Because `accept` lists `fits`, a plugin that
  already holds a 32-bit float FITS file may instead write it anywhere in `workdir` and answer with
  `"format": "fits"` and that path. The host reads either, with the same size limit.
* Sharpening, colour and export stay in Helia and run on the result.

### `frame-op`: `frame.process`

A step applied to **every frame before stacking** (for example a per-frame denoise). Params:

```json
{"step": "denoise", "settings": {…}, "params": {"strength": 0.5},
 "input": {…buffer…}, "output": {"path": "out-0001.hlpx", "format": "hlpx"},
 "meta": {"frame": 17, "frames": 631, "bayer": "rggb"}}
```

Result `{"output": {…buffer…}}`. The output must have the **same width, height and channels** as
the input (a frame operation does not resize: stacking needs frames of one size). `meta.bayer` is
null for mono or the pattern name when the frame is an undemosaiced mosaic; a step that cannot handle
a mosaic answers an `unsupported_input` error and Helia says so. Frame steps run once per frame, so
they are slow for a 600-frame capture: the host reuses one process for the whole run, and the plugin
should keep its state between calls.

### `image-op`: `image.process`

A step applied to an **image**: the stack, either before Helia's own sharpening and edits
(`stage: "pre-edit"`) or after them and before colour (`stage: "post-edit"`, the default). Params as
`frame.process` with `meta: {"stage": "post-edit"}`. The output may have a **different size**
(super-resolution): it is returned with its true `width` and `height`. Mono images only in version 1.

### `analyse`: `analyse.run`

Reads an image and reports; it changes nothing. Params as `image.process` but without `output`.
Result:

```json
{"metrics": {"mean": 31200.5, "sharpness": 0.82},
 "overlays": [
   {"kind": "circle", "x": 960, "y": 540, "r": 410, "label": "disc"},
   {"kind": "rect", "x": 10, "y": 20, "w": 100, "h": 80},
   {"kind": "line", "x1": 0, "y1": 0, "x2": 100, "y2": 50},
   {"kind": "points", "points": [[10, 20], [30, 40]]},
   {"kind": "text", "x": 5, "y": 5, "text": "note"}],
 "notes": ["Two sunspot groups"]}
```

Coordinates are pixels of the input image, origin top-left. At most 1000 metrics, 10000 overlay
items and 100 notes are kept; the rest is dropped and noted.

### `meta` and what the host records

Any result of `stack.run`, `frame.process` or `image.process` may carry an optional `meta` object next to
`output` (and `note`): a free-form record of how the result was made, for example
`{"provenance": {"model": "denoise-v3", "sha256": "…", "tile": 512, "strength": 0.5}}`. It is optional and
ignored by a host that does not know it.

Whatever the plugin returns, Helia **records that a plugin took part**. For every plugin that ran in a
stacking run the saved file's history (the `HISTORY` cards of a FITS file, the description of a TIFF or PNG)
gets a line

```
Derived by plugin NAME VERSION, program SHA-256 HEX: STEP, STEP
```

naming the plugin, its version, the checksum of the program that ran and the steps it did. A FITS file also
gets cards (see below). Then, for each step
that returned `meta`, a line `Plugin NAME STEP: {…}` with the record as compact JSON (at most 900
characters). A neural step cannot be missed in the saved product, and a plugin that gives no `meta` is still
named. When a `stack.run` answer says `"format": "fits"`, the host reads the FITS file and checks the
`width` and `height` of the answer against its header, as it does for an `hlpx` file.

#### FITS cards

For a FITS file Helia also writes, for every plugin that took part, `HIERARCH HELIA PLUGIN` (the names) and
`HIERARCH HELIA PLUGIN VERSION` (name and version), set by the host whatever the plugin says. From
`meta.provenance` the host copies **only** these ten keys, each checked, and ignores everything else:

| Key | Type and check |
|---|---|
| `HELIA ML` | boolean `true`; without it no `HELIA ML` card is written at all |
| `HELIA ML TASK` | `denoise` or `sharpen` |
| `HELIA ML MODEL` | up to 64 characters of letters, digits and `. _ : / + - space` |
| `HELIA ML VERSION` | the same, up to 32 characters |
| `HELIA ML SHA1`, `HELIA ML SHA2` | exactly 32 hexadecimal digits (the first and last half of the model's SHA-256) |
| `HELIA ML PROVIDER` | the same characters, up to 30 (`cpu`, `cuda`, `directml`, `remote:NAME`) |
| `HELIA ML STRENGTH` | number from 0 to 1 |
| `HELIA ML TILE` | whole number from 1 to 65536 |
| `HELIA ML NOTE` | exactly `NOT MEASUREMENT GRADE` |

A value that fails its check is dropped on its own. `helia edit` to FITS records only the history lines.

### `cancel` (notification, host to plugin)

`{"jsonrpc":"2.0","method":"cancel","params":{"id":7}}` asks the plugin to abandon request 7 as soon
as it can. The plugin answers request 7 with error code `-32800` (cancelled) and stays running. If it
has not answered within **5 seconds** the host kills it. The host sends `cancel` when the user
cancels, and before it enforces a timeout.

### `progress` (notification, plugin to host)

`{"jsonrpc":"2.0","method":"progress","params":{"id":7,"fraction":0.4,"status":"Uploading 40%",
"stage":"stack"}}`. `fraction` is 0 to 1 (optional), `status` is a short line for the status area
(optional), `stage` is one of Helia's stage names (`quality`, `reference`, `global_align`,
`local_align`, `stack`, `finish`) when the plugin does the same work (optional). Sending progress
also resets the call's inactivity timeout. The host throttles what it shows, so a plugin may send
often, but should not send more than about 20 a second.

### `log` (notification, plugin to host)

`{"jsonrpc":"2.0","method":"log","params":{"level":"info","message":"…"}}`; `level` is `debug`, `info`,
`warn` or `error`. Added to the plugin's log.

## Errors

A failed call answers with a JSON-RPC `error`. Helia's standard for errors is **What happened, Why,
What to do**, so a plugin should fill `data`:

```json
{"jsonrpc":"2.0","id":7,"error":{
  "code": -32010,
  "message": "The server did not answer.",
  "data": {"what": "I could not reach the stacking server.",
           "why": "The connection to 127.0.0.1:7871 was refused.",
           "todo": "Check that the SSH tunnel is open, then try again."}}}
```

`message` is a one-line fallback. `what`, `why`, `todo` are plain English, one or two sentences each,
no jargon, no code names. Missing parts are filled in by Helia (see below).

| Code | Name | Meaning |
|---|---|---|
| -32700 | parse error | the other side sent invalid JSON (host-originated errors only) |
| -32600 | invalid request | not a valid JSON-RPC message |
| -32601 | method not found | the plugin does not implement the method |
| -32602 | invalid params | the parameters are wrong |
| -32603 | internal error | the plugin's own bug |
| -32800 | cancelled | the call was cancelled |
| -32010 | unavailable | something the plugin needs is not reachable (a server, a licence) |
| -32011 | unauthorised | credentials are wrong or missing |
| -32012 | unsupported input | the data or settings cannot be handled (a mosaic frame, true colour) |
| -32013 | resource | out of memory, out of GPU memory, disk full |
| -32014 | invalid settings | a setting is wrong |
| -32000 | failed | any other failure |

Helia reports, in this order, the plugin's own `what`/`why`/`todo` where given and otherwise:

| Situation | What | Why | What to do |
|---|---|---|---|
| plugin not enabled | "The plugin NAME is not turned on." | "Plugins only run after you approve them." | "Open the Plugins page and enable it, or run `helia plugins enable NAME`." |
| hash changed | "The plugin NAME has changed since you approved it." | "Its program or manifest no longer matches the checksum you approved." | "If you updated it on purpose, review it and enable it again." |
| hash differs from manifest | "The plugin NAME looks damaged." | "The program's checksum does not match its manifest." | "Download it again from the author." |
| plugin exited | "The plugin NAME stopped unexpectedly." | "It exited with code N." plus the last log line | "See the plugin's log on the Plugins page. Your capture is untouched." |
| timeout | "The plugin NAME stopped answering." | "It sent nothing for N seconds." | "Try again; if it keeps happening, raise `timeout_s` in its manifest or ask its author." |
| protocol error | "The plugin NAME sent something Helia cannot read." | the specific rule broken | "Update the plugin. Report it to its author." |
| bad result | "The plugin NAME returned a picture that does not fit." | "The file was N bytes, the header says M." | as above |
| cancelled | "Cancelled." | | |

## Timeouts and resource hints

* Each call has an **inactivity timeout** (`limits.timeout_s`, default 120 s; `initialize` 30 s;
  `test` 60 s; `shutdown` 5 s). Any message from the plugin (progress or log included) restarts it.
  A call also has an absolute limit, `limits.max_total_s` (default 86400).
* On timeout the host sends `cancel`, waits 5 seconds, then kills the process.
* `resources.gpu` and `resources.memory_mb` are **hints**. Helia shows them before approval and on
  the plugin's page, and warns if the machine has less memory. They do not limit the plugin.

## Versioning

* `api_version` is one integer, the **major** version. Version 1 is described here.
* Within a version, changes are **additive only**: new optional fields, new methods, new
  capabilities, new error codes. Receivers **ignore unknown fields** and answer unknown methods with
  `-32601`. A plugin must treat an unknown capability name as "not mine".
* A removed or changed field, a changed meaning or a new required field is a new major version. A
  host supports the versions it lists in the release notes (version N and N-1 for one release after
  a change). A plugin with an `api_version` the host does not support is shown as "needs a newer
  Helia" or "needs an update", never run.
* Plugin version (`version`) is free-form and informational.

## Security of the host

The parsers are the attack surface a plugin (or a manifest downloaded with one) can reach:

* **Manifest**: size limited, TOML parsed with the `toml` crate, every string length-limited, `name`,
  step ids and `executable` validated (no traversal, must exist and be a regular file inside the
  manifest's folder after resolving links), hash comparison in constant time.
* **JSON-RPC lines**: read with a hard line limit *while reading* (an unterminated gigabyte is not
  buffered), depth-checked before parsing, strict UTF-8, invalid frames reported without panic.
* **Buffers**: path rules above, header and size checked before reading, overflow-safe arithmetic,
  allocation limit, NaN scrub.
* **Processes**: the host owns `workdir`, deletes it on exit, never passes the plugin a path it did
  not choose except the capture path the user picked, and kills the process tree on cancel on
  platforms where that is possible.
* **Tests**: a malicious-input unit suite (huge frames, deep JSON, truncated lines, bad UTF-8,
  path traversal in buffer paths, size lies in headers, hostile manifests) and a `cargo-fuzz` target,
  `plugin_protocol`, run by the nightly fuzz job and the release security audit.

## The SDK

`helia-plugin-sdk` (crate in `crates/helia-plugin-sdk`) implements the plugin side for Rust: the
JSON-RPC loop, progress, cancellation flag, buffer reading and writing, and the error type. It is
licensed **MIT OR Apache-2.0**, not GPL, so that a plugin that uses it can have any licence, including
a closed one. This is consistent with the rest of Helia because the SDK is the project owner's own
code, which the owner may license differently, and because it **depends on no Helia crate** (checked
by `cargo deny`, which would fail on a GPL dependency in its tree). The protocol itself is
this document; a plugin does not need the SDK, and the Python example `examples/plugins/invert-py`
shows the same in about 150 lines of standard library.

## Examples in the repository

| Example | Language | Capability | Shows |
|---|---|---|---|
| `examples/plugins/invert` | Rust, SDK | image-op | the smallest useful plugin, progress, cancel |
| `examples/plugins/invert-py` | Python, standard library only | image-op | the protocol without an SDK |
| `examples/plugins/stats-py` | Python, standard library only | analyse | metrics and overlays |

## Out of scope for version 1

Plugins that open ports, load libraries into Helia (in-process), colour (RGB) steps, a plugin store,
signature verification, concurrent requests. They can be added without breaking version 1.
