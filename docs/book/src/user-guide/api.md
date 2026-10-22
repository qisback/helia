# The API

`helia serve` exposes the stacker as a JSON API, so scripts, notebooks and
other programs can drive Helia without touching the command line or the GUI.
It is the same pipeline as `helia stack`, with the same options.

The server is the `helia-server` crate. The CLI embeds it, and so can the GUI
or your own Rust program (`Server::builder()`).

This page is the guide: how to run the server, how to call it, and the security
model. The complete list of operations, parameters and types is the
[API reference](../reference/api.md), generated from the server's own OpenAPI
document, so it always matches the code. A running server also serves that
document at `/api/v1/openapi.json` and a small browsable page at
`/api/v1/docs`.

## Running it

```sh
helia serve --data-root ~/captures
# listening on http://127.0.0.1:7870
```

| Option | Default | Meaning |
|---|---|---|
| `--bind ADDR` | `127.0.0.1:7870` | Listen address. A non-loopback address is refused unless a token is set. |
| `--data-root DIR` | current directory | Every path a client sends must stay inside this directory. |
| `--token T` | none | Bearer token. Visible in the process list; prefer the next two. |
| `--token-file FILE` | none | Read the token from a file (whitespace trimmed). |
| `HELIA_TOKEN` | none | Environment variable, used when neither option is given. |
| `--workers N` | 1 | Jobs that run at once. The GPU is one device, so leave this at 1 unless you stack on the CPU. |
| `--max-queue N` | 16 | Queued plus running jobs allowed; more get `429`. |
| `--cors-origin ORIGIN` | none | Allow a browser page on this origin (repeatable). |
| `--allow-host HOST` | none | Accept this `Host` header on a loopback bind, for a reverse proxy (repeatable). |

Port `0` picks a free port; the first line printed is
`listening on http://127.0.0.1:PORT`. Stop with Ctrl-C: running jobs are
cancelled and open event streams are closed.

All routes are under `/api/v1`. Errors are always

```json
{"error": {"code": "path_outside_root", "message": "path resolves outside the data root"}}
```

where `code` is stable and safe to match on. Request bodies are JSON
(`Content-Type: application/json`) and limited to 64 KiB.

## Paths

Paths in requests are relative to the data root (absolute paths are accepted
only if they point inside it). Responses show paths relative to the root with
forward slashes. Outputs are written inside the root too; missing parent
directories are created.

## Authentication

With a token set, every request except `GET /api/v1/health` and the static
`GET /api/v1/docs` page needs

```
Authorization: Bearer <token>
```

and gets `401 unauthorized` otherwise. The WebSocket endpoint takes the same
header; browsers cannot set it on a WebSocket, so use a non-browser client or
a proxy that adds it.

```sh
export H='Authorization: Bearer my-token'
```

## Endpoints

### `GET /health`

```sh
curl http://127.0.0.1:7870/api/v1/health
# {"status":"ok"}
```

### `GET /version`

Server version, how it was built (commit, date, profile, target, Rust and wgpu
versions), copyright and licence, backend availability (the GPU is probed once,
on first use), limits and whether a token is required.

```sh
curl -H "$H" http://127.0.0.1:7870/api/v1/version
```

```json
{"name":"helia-server","version":"0.5.0","api":"v1",
 "build":{"commit":"1a2b3c4d","dirty":false,"built":"2026-10-19 09:30 UTC","profile":"release",
          "target":"x86_64-unknown-linux-gnu","rust":"rustc 1.95.0 (...)","wgpu":"30.0.1"},
 "licence":{"copyright":"Copyright (C) 2026 Laurence Horrocks-Barlow",
            "licence":"GPL-3.0-or-later","source":"https://github.com/qisback/helia"},
 "backends":{"cpu":{"available":true},
             "gpu":{"available":true,"adapter":"NVIDIA ... (Vulkan, DiscreteGpu)","software":false}},
 "limits":{"workers":1,"max_queue":16,"max_body_bytes":65536,"active_jobs":0},
 "auth_required":true}
```

### `GET /presets`

The filter presets: for each one its id, name, wavelength, the Bayer channel and
share of frames it recommends, why, and the colour parameters. Colour is
presentation, not measurement; see [Colourising](colourising.md).

```sh
curl -H "$H" http://127.0.0.1:7870/api/v1/presets
```

```json
{"presets":[{"id":"ha","name":"H-alpha","wavelength":"656.3 nm","hue":"orange-red",
  "summary":"...","reason":"...","channel":"r","keep_percent":25.0,
  "colour":{"gamma":0.85,"gains":[1.0,0.85,0.6],"exponents":[0.7,1.5,2.6],
            "mid_grey_rgb":[169,90,33]}}, "..."]}
```

The ids are `ha`, `cak`, `cah`, `mg`, `na`, `he-d3`, `hbeta` and `white`.

### `GET /presets/{id}/lut`

A filter's colour table as a file for other programs: `format` (`cube`, the
default, `csv`, `imagej` or `png`), `size` (2 to 4096, default 256), `invert` and
`strength`. `404` for an unknown preset, `422 invalid_config` for a bad value. See
[Colourising](colourising.md#exporting-a-colour-table).

```bash
curl -H "$H" -o ha.cube "http://127.0.0.1:7870/api/v1/presets/ha/lut?format=cube"
```

### `POST /captures/inspect`

Header information for a `.ser`, `.avi` or video (`.mp4`, `.mov`, `.mkv`,
`.webm`, `.m4v`) capture: the header as written, what Helia will use and where
each value came from, and what to tell the user. Video needs the optional
`ffmpeg` on the server; only `ffprobe` runs for an inspect, so it is quick. For a
SER file that says it is mono, a few frames are read to check the pixels for a
Bayer mosaic. The body may also carry the overrides `bayer`, `bit_depth`,
`endian` and `flip_rows` (as in a job's `config`, below) to see what they would
change.

```sh
curl -H "$H" -H 'Content-Type: application/json' \
  -d '{"path":"2026-10-01/sun.ser"}' http://127.0.0.1:7870/api/v1/captures/inspect
```

```json
{"path":"2026-10-01/sun.ser","format":"ser","width":1936,"height":1096,
 "bit_depth":16,"colour":"mono","bayer":null,"frames":4000,
 "observer":null,"instrument":"ZWO ASI174MM","telescope":null,"timestamps":true,
 "fourcc":null,"sidecar":null,
 "recommended":{"filter":"ha","filter_source":"2026-10-01_Ha","channel":"lum",
                "keep_percent":25.0,"ap_size":72},
 "header":{"file_id":"LUCAM-RECORDER","lu_id":0,"colour_id":0,
           "colour_meaning":"MONO","little_endian":true,"width":1936,
           "height":1096,"pixel_depth":16,"frame_count":4000,"observer":"",
           "instrument":"ZWO ASI174MM","telescope":"","date_time":null,
           "date_time_utc":"2026-10-01 09:12:03.250",
           "timestamps":{"count":4000,"first":"2026-10-01 09:12:03.250",
                         "last":"2026-10-01 09:12:35.240"}},
 "effective":{"bayer":{"value":"none","source":"header"},
              "bit_depth":{"value":16,"source":"header"},
              "byte_order":{"value":"little","source":"header"},
              "flip_rows":{"value":false,"source":"header"}},
 "detection":{"frames_used":5,"region":[724,420,488,256],
              "position_means":[0.51,0.51,0.51,0.51],"spread":0.003,
              "verdict":"mono","pattern":null,"red_blue_basis":null,"reason":null},
 "overrides":[],"notices":[]}
```

`colour` is `mono`, `bayer`, `rgb`, `bgr` or `video`, and `bayer` and `bit_depth` are the
values Helia will decode with, so for a SER file that says mono but holds a
mosaic `colour` is `bayer` and `bayer` is the detected pattern. The new fields
say how Helia got there:

* `header`: a SER header exactly as written, decoded (`null` for an AVI).
  `colour_meaning` is the meaning of `colour_id` (`MONO`, `BAYER_RGGB`, ...),
  `date_time` and `date_time_utc` are decoded dates or `null` when unset, and
  `timestamps` gives the count and the first and last of the per-frame timestamps
  (UTC), or `null` when the file has none.
* `effective`: the `bayer` pattern (or `"none"`), `bit_depth`, `byte_order`
  (`little` or `big`) and `flip_rows` in force, each with a `source` of `header`,
  `sidecar`, `detection` or `override`.
* `detection`: the mosaic check, present when it ran (a SER file that says mono,
  with `bayer` not overridden): the means of the four pixel positions of a 2 x 2
  cell, their `spread`, and a `verdict` of `mosaic`, `mono` or `inconclusive`
  (with the `pattern` and the `red_blue_basis` for a mosaic, or the `reason`).
* `overrides`: what the request overrode, as command-line flags.
* `notices`: what to tell the user, each with a `level` (`info` or `warning`) and
  its `text`. A SER file that says mono but holds a mosaic gives a `warning`
  that names the pattern and the measurement.

For a video, `format` is its extension (`mp4`, `mov`, `mkv`, `webm`, `m4v`, or
`avi` for a compressed AVI), `codec` and `frame_rate` are set, `width` and
`height` are after any rotation, and `quality_note` carries the lossy-input
caveat when there is one, for example "lossy 8-bit 4:2:0 video (h264): fine for
quick results, SER/AVI raw captures keep more detail" (it is `null` for
everything else). For AVI files, `sidecar` carries what a SharpCap `CameraSettings.txt` next to the
capture says (colour space, Bayer hint, frame count, camera section names) and
`fourcc` the codec tag.

`recommended` is what a job would use if given no settings: the filter Helia
detected (`filter`, an id from `GET /presets`, or `null`) and the text it found
it in (`filter_source`), the Bayer `channel` it would stack (`lum` for a capture
that is not raw colour), the share of frames to keep and the alignment point size.

### Videos

A job whose input is a video decodes it first (the stage is reported as
`quality`, at 0 per cent, until the stacking passes begin), into the directory
given by `helia serve --cache-dir` (or `HELIA_CACHE_DIR`), and removes the
decoded file when the job ends. Cancelling the job kills `ffmpeg`. A frame
preview of a video decodes only up to that frame. The job's `channel` setting
chooses the colour plane `ffmpeg` extracts. Video errors have their own codes:
`ffmpeg_unavailable` (503, not installed or too old), `truncated_video` (422),
`insufficient_storage` (507, not enough room to decode); other unreadable files
are `invalid_capture` (422). The path sandbox applies as for any file, and no
text from a request is ever passed to `ffmpeg` as an option.

### `GET /captures/frame`

An 8-bit greyscale PNG of one frame, auto-stretched (0.1 to 99.9 percentiles).

| Query | Default | |
|---|---|---|
| `path` | required | capture |
| `index` | `0` | frame number |
| `channel` | `lum` | `r`, `g`, `b` or `lum`, for Bayer captures and video |
| `max_width` | none | downscale to at most this width, 16 to 8192 |
| `bayer`, `bit_depth`, `endian`, `flip_rows` | as for a job | how to read the capture, as in a job's `config` |

```sh
curl -H "$H" -o frame.png \
  'http://127.0.0.1:7870/api/v1/captures/frame?path=sun.ser&index=120&max_width=800'
```

### `POST /jobs`

Queue a stack. Returns `202` with the job.

```sh
curl -H "$H" -H 'Content-Type: application/json' -d '{
  "input": "sun.ser",
  "output": "stacks/sun.tif",
  "config": {"preset": "ha", "backend": "auto"}
}' http://127.0.0.1:7870/api/v1/jobs
```

`config` takes the same knobs as `helia stack`; every field is optional:

| Field | Default | Same as |
|---|---|---|
| `keep` | `"25%"` | `--keep` (`"25%"` or a count such as `"500"`) |
| `keep_frames` | none | `--keep-frames` (excludes `keep`) |
| `ap_size` | from the image size | `--ap-size` (8 to 1024) |
| `ap_auto` | `false` | `--ap-auto`; excludes `ap_size`, needs `multipoint`; what it chose is in `summary.auto_ap` |
| `ap_passes` | 1 | `--ap-passes` (1 or 2; needs `multipoint`) |
| `ap_smooth` | `false` | `--ap-smooth on`; needs `multipoint` |
| `aperture_mm` | none | `--aperture-mm`; adds the diffraction cut-off to `summary.granulation` |
| `field_rotation` | none | `--field-rotation`; an object (empty for the defaults) with `mirror` and `reference` (`middle`, `first`, `last`); needs `site` (or the server's `HELIA_SITE`) and a SER capture with timestamps; what it did is in `summary.field_rotation`; see [Field rotation](field-rotation.md) |
| `sunspots` | none | `--sunspots`; an object (empty for the defaults) with `umbra`, `penumbra`, `min_area`, `max_area`, `group_gap_deg`, `wolf_k`, `north_angle_deg`, `celestial_north_up`, `mirror`; the result is in `summary.sunspots`, `GET /jobs/{id}/sunspots.csv` and `sunspots.png`, and the `SPOT*` FITS cards; see [Sunspots](sunspots.md) |
| `seeing` | `false` | `--seeing`; the estimate is in `summary.seeing`, `r0` needs `aperture_mm` |
| `autocrop` | `false` | `--autocrop`; the region kept is in `summary.crop` |
| `autocrop_margin` | 10 | `--autocrop-margin` (per cent of the radius, 0 to 200; needs `autocrop`) |
| `colour` | none | `--colour` (`auto`, `mono`, a preset id or `true`); `true` is true colour, see below |
| `white_balance` | `"auto"` | `--white-balance` (`auto`, `off`, `"R,G,B"`); only with `colour: "true"` |
| `align_channels` | `true` | `false` is `--no-channel-align`; only with `colour: "true"` |
| `limb_law` | `"quadratic"` | `--limb-law` (`linear`, `quadratic`, `neckel`); the fit is in `summary.limb_darkening` |
| `flatten` | off | `--flatten` (`model`, `profile`); runs before `sharpen` and `edits` |
| `multipoint` | `true` | `false` is `--no-multipoint` |
| `backend` | `"auto"` | `--backend` (`auto`, `cpu`, `gpu`) |
| `sigma_clip` | off | `--sigma-clip` |
| `robust` | `"none"` | `--robust` (`none`, `sigma`, `huber`, `winsor`; excludes `sigma_clip`) |
| `robust_k` | 2.5, 1.345 or 2 by kind | `--robust-k` (needs `robust`) |
| `bayer` | `"auto"` | `--bayer` (`auto`, `none`, `rggb`, `grbg`, `gbrg`, `bggr`) |
| `bit_depth` | the SER header's | `--bit-depth` (1 to 16; SER only) |
| `endian` | the header flag, checked | `--endian` (`little`, `big`; SER only) |
| `flip_rows` | `false` | `--flip-rows` (SER only) |
| `channel` | the preset's, else `"lum"` | `--channel` (`r`, `g`, `b`, `lum`) |
| `debayer` | `"mhc"` | `--debayer` (`bilinear`, `mhc`) |
| `metric` | `"gradient"` | `--metric` (`gradient`, `laplacian`, `helmli`, `mfgs`, `dct`, `fourier`, `contrast`) |
| `exclude` | `[]` | `--exclude`, as a list of `{"x":0,"y":0,"w":200,"h":20}` (pixels from the top-left corner; `w` and `h` at least 1) |
| `prominence_band` | `0` | `--prominence-band` (pixels outside the limb where points are still allowed) |
| `disc_mask` | `true` | `false` is `--no-disc-mask` |
| `detect_overlay` | `true` | `false` is `--no-overlay-detect` |
| `preset` | none | `--preset` (an id from `GET /presets`) |
| `colour` | none | `--colour` (`"auto"`, `"mono"` or an id) |
| `invert` | `false` | `--invert` |
| `colour_strength` | `1.0` | `--colour-strength` (0 to 1) |
| `sharpen` | none | `--sharpen` (`wavelet`, `unsharp`, `rl`, `none`), with the filter's recommended values |
| `edits` | `[]` | further operations, applied in order after `sharpen` (see below) |
| `sharpen_fits` | `false` | `--sharpen-fits`: apply `sharpen` and `edits` to FITS output too |
| `calibration` | none | `--dark`, `--flat`, `--bad-pixels` and friends, as an object (see below) |

Unknown fields are rejected. The output extension picks the format (`.tif`,
`.tiff`, `.png`, `.fits`, `.fit`, `.fts`). An existing output is refused
(`409 output_exists`) unless the request sets `"overwrite": true`; two active
jobs cannot share an output (`409 output_in_use`). The input is opened before
queueing, so an unreadable capture fails immediately with `422`.

### Calibration

`config.calibration` applies dark, flat and bad-pixel calibration to every frame
before it is demosaiced, scored or aligned. Every path is relative to the data root
and names a master file (FITS from `helia calibrate`) or raw calibration frames to
build one from (a SER, AVI or FITS file, or a folder of them); at least one of
`dark`, `flat` and `bad_pixels` is needed.

```sh
curl -H "$H" -H 'Content-Type: application/json' -d '{
  "input": "sun.ser", "output": "stacks/sun.fits",
  "config": {"calibration": {"dark": "cal/master-dark.fits",
                             "flat": "cal/master-flat.fits"}}
}' http://127.0.0.1:7870/api/v1/jobs
```

| Field | Default | Same as |
|---|---|---|
| `dark` | none | `--dark` |
| `flat` | none | `--flat` |
| `displaced` | none | `--displaced`: captures of the Sun at many places on the sensor, to solve the flat from; excludes `flat` |
| `flat_dark` | none | `--flat-dark`: darks at the exposure of the flats, when `flat` is captures |
| `bias` | none | `--bias` |
| `bad_pixels` | none | `--bad-pixels` (FITS map or `x y` text list) |
| `cosmetic` | `true` | `false` is `--no-cosmetic` |
| `scale_dark` | `false` | `--scale-dark` |
| `combine` | `"sigma"` | `--calibration-combine` (`median`, `sigma`, `sigma:K`) |

A missing path is `404 not_found`, a bad `combine` `422 invalid_config`. A master of
the wrong size or mosaic fails the job with `invalid_calibration` or `stack_failed`
and a message that says why. The job's `summary.calibration` lists the masters
applied, one line each, and any warning about them (a flat with rings, a dark that
could not be scaled); it is empty for a job without `calibration`. The result's
header or description records them too.

### Colour and presets

Settings left out of `config` come from the filter and the image: the filter is
`preset` if given, otherwise detected from the file name, folder, sidecar or
header (the same rules as the command line; see
[Colourising](colourising.md#how-the-filter-is-recognised)). It supplies the
Bayer `channel` and the share of frames to `keep`, and the image size supplies
`ap_size`; a value you give always wins. `POST /captures/inspect` shows what
would be chosen.

A job colours its output when `preset` or `colour` says so. `preset` colours a
`.tif` (16-bit RGB) or `.png` (8-bit RGB) output with that filter's look.
`colour` chooses only the look: `"auto"` is the preset in force (a job fails with
`no_filter` if there is none), `"mono"` is plain grey and an id names a look.
`invert` and `colour_strength` adjust it. A `.fits` output stays mono and
unaltered, with the look noted in its header, along with SOLARNET metadata
(times, exposure, filter, processing, sky coordinates; see
[Output formats](../reference/output-formats.md#fits)). `site` (`"lat,lon,alt"`)
adds the observing site as `OBSGEO-X/Y/Z`. When the job has finished,
`summary.bayer` and `summary.bayer_source` say which Bayer pattern the frames were
decoded with (`null` for mono) and where it came from, and `summary.notices`
carries the notices of the inspection (for example a Bayer mosaic found in a file
whose header says mono); how the capture was read is also written into the
output file. `summary.filter` and `summary.filter_source` say which filter was in force
(`"chosen"` when `preset` gave it) and `summary.colour` describes the look
written, in the words the file carries. An unknown `preset` or `colour` is a
`422 invalid_config` that lists the valid ids.

`colour: "true"` stacks the red, green and blue planes of a raw colour capture on
one alignment (see [True colour](colourising.md#true-colour)); `white_balance` and
`align_channels` belong to it. `summary.true_colour` then holds `gains` (red,
green, blue, the common scale included), `channels` (for red and blue: `shift`
in pixels as `[x, y]`, `confidence` 0 to 1 and whether it was `applied`) and
`notes`, the lines the command line prints; it is `null` for any other job. The
output is an RGB TIFF or PNG, or a three-plane FITS cube. See [the true colour
tutorial](../tutorials/true-colour.md) for a run.

`align` chooses how whole frames are aligned: `"auto"` (the default: fit the
solar limb when the whole disc is in the frame and the fit is confident, else
phase correlation), `"phase"` or `"limb"`. A finished job's `summary.alignment`
says what happened: `requested`, `method` (`"limb"` or `"phase"`), `reason`, the
mean `confidence` and `confidence_min` of the frames (0 to 1; absent when not
measured), `fallbacks` (frames that wanted the limb but used phase correlation),
`sampled` (the confidence comes from a sample of frames, as after a GPU run) and
`limb`, the reference's fitted `[x, y, radius]` in pixels. See
[How stacking works](how-stacking-works.md#3-aligning-the-whole-frame).

### Sharpening and edits

`sharpen` picks a method with the recommended values of the filter in force, and
`edits` adds operations after it: a list of objects, each with an `op` of
`unsharp`, `wavelet`, `deconvolve`, `denoise`, `prominence`, `levels`, `curve`,
`crop`, `rotate`, `flip` or `resize` and that operation's parameters. Anything left out
takes its default. The operations are described in
[Sharpening and adjustments](sharpening.md#saved-presets).

```sh
curl -H "$H" -H 'Content-Type: application/json' -d '{
  "input": "sun_Ha.ser",
  "output": "stacks/sun.tif",
  "config": {
    "sharpen": "wavelet",
    "edits": [
      {"op": "levels", "gamma": 1.2},
      {"op": "crop", "x": 100, "y": 80, "width": 1600, "height": 900}
    ]
  }
}' http://127.0.0.1:7870/api/v1/jobs
```

`{"op": "prominence"}` composites a stretched off-limb prominence layer with the
disc (fields `strength`, `stretch`, `level`, `feather`, `reach`, `gate`; see
[Prominences](prominences.md)); put it after the sharpening and before `levels`.

The edits run on the mono stack in the order given, before any colour. A request
with an unknown operation or an out-of-range value is a `422 invalid_config`; a
crop that does not fit this capture fails the job (`failed`, code
`invalid_config`) once the capture is open. The job reports an `edit` stage
between `finish` and `colourise`. `summary.edits` lists the lines recorded in
the file, `summary.edited` says whether they were applied to the pixels, and
`summary.width` and `summary.height` are the size of the written image. FITS
output is the unedited stack unless `sharpen_fits` is true, and its `summary.edits`
then says so. `GET /presets` includes each filter's recommended sharpening under
`sharpen`.

Jobs run on a bounded pool (`--workers`, default 1) in submission order.
When `--max-queue` jobs are queued or running, new ones get `429 queue_full`.

### `GET /jobs`, `GET /jobs/{id}`

```json
{"id":3,"state":"running","stage":"stack","stage_progress":0.42,"progress":0.78,
 "queue_position":null,"input":"sun.ser","output":"stacks/sun.tif",
 "config":{"keep":"25%","...":"..."},
 "created_at":1790000000000,"started_at":1790000000100,"finished_at":null,
 "run_seconds":12.3,"timings":[],"backend":"GPU (...)","summary":null,"error":null}
```

* `state`: `queued`, `running`, `succeeded`, `failed` or `cancelled`.
* `stage`: `opening` (the capture is being opened and indexed), `quality`,
  `reference`, `global_align`, `local_align`, `stack`, `finish`, then
  `colourise` (only when the output is coloured) and `write`. `stage_progress`
  is 0 to 1 within the stage; `progress` is an approximate overall figure (stage
  weights are fixed, not measured). The stage names stream on the events
  WebSocket too. `opening`, `colourise` and `write` do not move `progress`: it
  is 0 while the capture opens and already 1 when the pipeline ends, so the
  stage name is what tells you the job is still colouring or writing.
* `queue_position`: 0 is next to run.
* Once finished: `timings` per stage, and `summary` with frame counts,
  alignment points and output size. On failure `error` is `{code, message}`.

`GET /jobs` lists remembered jobs, oldest first. The newest 100 finished jobs
are kept.

### `DELETE /jobs/{id}`

* Queued job: cancelled at once (`202`, state `cancelled`).
* Running job: the pipeline's cancel flag is set (`202`); poll until the state
  is `cancelled`. No output is written.
* Finished job: the record is forgotten (`204`). The output file stays.

### `GET /jobs/{id}/result`

The written TIFF, PNG or FITS file (`image/tiff`, `image/png` or
`application/fits`), streamed.
`409 not_ready` unless the job succeeded.

```sh
curl -H "$H" -o sun.tif http://127.0.0.1:7870/api/v1/jobs/3/result
```

### `GET /jobs/{id}/preview.png`

An 8-bit PNG of the result (square-root stretch of the normalised stack), at
most 2048 pixels wide. `?max_width=` shrinks it further. `409 not_ready`
unless the job succeeded.

### `GET /jobs/{id}/sunspots.csv` and `sunspots.png`

For a job that had `sunspots` in its config: the spot table as CSV, and the overlay
(the flattened disc with penumbra, umbra, limb and group numbers) as a PNG. `404
sunspots_missing` when the job did not ask. The counts, the Wolf number, the areas,
`P`, `B0`, `L0` and both tables are also in `summary.sunspots`. See
[Sunspots](sunspots.md).

```bash
curl -H "$H" -o spots.csv http://127.0.0.1:7870/api/v1/jobs/3/sunspots.csv
curl -H "$H" -o spots.png http://127.0.0.1:7870/api/v1/jobs/3/sunspots.png
```

### `POST /timelapse`

Registers stacked images to each other and writes a time-lapse: numbered frames, a
registration table and a movie. Stack each capture first with an ordinary job (the same
`config` for all), then send the stacks it wrote:

```sh
curl -H "$H" -H 'Content-Type: application/json' http://127.0.0.1:7870/api/v1/timelapse -d '{
  "inputs": ["stacks/a.tif", "stacks/b.tif", "stacks/c.tif"],
  "times": ["2026-10-20T14:32:09", "2026-10-20T14:37:19", "2026-10-20T14:42:30"],
  "output": "tl", "movie": "auto", "fps": 10, "deflicker": "match", "crop": true }'
```

It answers when the series is written (a `200` with a row for each frame: its file, time,
the shift that was applied, the method, the limb fit's confidence, the correlation's
peak-to-sidelobe ratio and the brightness gain, and the paths of the movie and the
table). The files are in the data root. Between 2 and 200 inputs; they are put in time
order when every one has a time (from `times`, a FITS header or a file name with a full
date and time). Errors: `unsupported_input` (an input that is not a `.tif`, `.tiff`, `.png`
or `.fits` file), `invalid_config` (a setting out of range, or `times` of the wrong
length), `output_exists` (`409`: `output` already holds a time-lapse and `overwrite` is
false), `not_found` and the path errors as everywhere. The fields, their ranges and
defaults are in the [API reference](../reference/api.md#post-timelapse); the method is in
[Time-lapse](timelapse.md).

### `POST /mosaic`

Joins stacked panels of the Sun in the data root into a full-disc mosaic (see
[Mosaics](mosaic.md)) and writes it, with `NAME-panels.csv` and `NAME-pairs.csv` beside it:

```sh
curl -H "$H" -H 'Content-Type: application/json' http://127.0.0.1:7870/api/v1/mosaic -d '{
  "inputs": ["stacks/p1.tif", "stacks/p2.tif", "stacks/p3.tif", "stacks/p4.tif"],
  "output": "out/sun.tif", "max_rotation_deg": 3, "feather_px": 12 }'
```

The answer has the size of the mosaic, a row for each panel (where it went, its turn, the
gain and offset, and the derotation), a row for each matched pair (correlation, overlap,
residual, whether it was used), the seam and overlap RMS, the disc, and the paths of the two
tables. Between 2 and 64 inputs; the first is the reference. `gain_match` and `seams` (both
default true) turn parts off; `derotate` takes the object of `POST /solar-derotation` with
`to` as `first`, `middle` (default), `last` or a UTC time, and needs a time for every panel
(`times`, a FITS header or a file name with a full date and time). Errors: `unsupported_input`,
`invalid_config`, `unsupported_output_format`, `output_exists` and `output_is_input` (`409`),
`mosaic_failed` (panels that do not match, no time for a derotation) and the path errors as
everywhere. The fields are in the [API reference](../reference/api.md#post-mosaic).

### `POST /solar-derotation`

Brings images of the Sun in the data root to one time by its differential rotation
(see [Solar derotation](derotation.md)) and writes `NAME-derotated.EXT` to `output`:

```sh
curl -H "$H" -H 'Content-Type: application/json' http://127.0.0.1:7870/api/v1/solar-derotation -d '{
  "inputs": ["stacks/a.tif", "stacks/b.tif"],
  "times": ["2026-10-20T14:32:09", "2026-10-20T14:37:19"],
  "output": "dd", "to": "2026-10-20T19:37:19", "law": "howard" }'
```

`to` is `first`, `middle` (default), `last` or a UTC time; `law` is `howard` (default),
`snodgrass` or `A,B,C`; `north_angle_deg`, `celestial_north_up`, `mirror` and
`raw_brightness` are as on the command line. The answer has, for each image, the file,
the time, the seconds moved, the most the surface moved and how much at the disc centre
(in pixels) and the disc that was fitted. Errors: `no_time` (an image
with no time and none in `times`), `no_disc`, `unsupported_input`, `invalid_config`,
`output_exists` (`409`) and the path errors as everywhere. The fields are in the
[API reference](../reference/api.md#post-solar-derotation). A time-lapse takes the same
settings as its `derotate` object.

### `GET /jobs/{id}/events` (WebSocket)

Text frames, one JSON object each:

```json
{"type":"snapshot","job":{"id":3,"state":"queued","...":"..."}}
{"type":"state","job":3,"state":"running","error":null}
{"type":"progress","job":3,"stage":"quality","stage_progress":0.5,"progress":0.1}
{"type":"state","job":3,"state":"succeeded","error":null}
```

The first message is always a `snapshot`. Progress messages are thinned to
about one per half a percent. The server closes the stream after the final
`state` message; a job that has already finished yields the snapshot and then
closes.

```sh
websocat -H "$H" ws://127.0.0.1:7870/api/v1/jobs/3/events
```

### `GET /openapi.json`, `GET /docs`

`openapi.json` is generated from the code (utoipa) and always matches the
server. `/docs` is a small page of our own that lists the operations from that
document; it is static and public, and has a box for the token.

## Python

```python
import time
import requests

BASE = "http://127.0.0.1:7870/api/v1"
s = requests.Session()
s.headers["Authorization"] = "Bearer my-token"   # omit if the server has no token

def api(method, path, **kw):
    r = s.request(method, BASE + path, **kw)
    if not r.ok:
        raise RuntimeError(r.json()["error"])
    return r

print(api("GET", "/version").json()["backends"])

info = api("POST", "/captures/inspect", json={"path": "sun.ser"}).json()
print(info["frames"], "frames,", info["width"], "x", info["height"])

job = api("POST", "/jobs", json={
    "input": "sun.ser",
    "output": "stacks/sun.tif",
    "config": {"keep": "20%", "ap_size": 48},
    "overwrite": True,
}).json()

while job["state"] in ("queued", "running"):
    time.sleep(1)
    job = api("GET", f"/jobs/{job['id']}").json()
    print(f"{job['state']:9} {job.get('stage') or '':13} {job['progress']:.0%}")

if job["state"] != "succeeded":
    raise SystemExit(job["error"] or job["state"])

with open("sun.tif", "wb") as f:
    f.write(api("GET", f"/jobs/{job['id']}/result").content)
with open("sun.png", "wb") as f:
    f.write(api("GET", f"/jobs/{job['id']}/preview.png").content)
```

Polling is enough for scripts; use the WebSocket (for example with the
`websockets` package, passing the `Authorization` header) when you want live
progress without a delay.

## Security model

The server reads and writes files on behalf of whoever can reach it, so it is
conservative by default.

* **Loopback only.** The default bind is `127.0.0.1`. A non-loopback address
  (`0.0.0.0`, a LAN address) is refused at start-up unless a token is set.
  There is no TLS: put a reverse proxy in front for anything beyond a trusted
  network, and give it `--allow-host` for the public name.
* **Bearer token.** Compared in constant time. Only `/health` and the static
  `/docs` page are open. Use a long random token and keep it in a file
  (`--token-file`, mode 600) rather than on the command line.
* **Data root confinement.** Every input, output, capture and preview path is
  resolved against `--data-root` and refused with `403` when it:
  * contains a `..` component (`path_traversal`), even if it would land inside;
  * is absolute and outside the root, or resolves outside it through a
    symbolic link, junction or dangling link (`path_outside_root`).

  For outputs that do not exist yet, the nearest existing parent is resolved
  and checked, so a link in the parent chain cannot redirect a write. Checks
  happen when the request arrives; do not let untrusted local users write to
  the data root, since a link swapped in between check and use is not defended
  against. A SharpCap sidecar is only read when it also resolves inside the
  root.
* **Host check.** On a loopback bind, requests whose `Host` is not
  `localhost` or a loopback address are refused (`403 bad_host`). This stops a
  web page from reaching the server through DNS rebinding.
* **No CORS by default.** A browser page on another origin cannot read
  responses. `--cors-origin` opts one origin in; combine it with a token.
* **Resource limits.** 64 KiB request bodies, a bounded job queue (`429`),
  at most 100 remembered finished jobs, previews capped at 2048 pixels (frame
  previews at 8192), existing files never overwritten without `overwrite`.
* **Path errors** do not echo absolute server paths; job records
  show paths relative to the root.

Without a token any process on the machine (and any local user) can read
captures under the data root and write image files there. The server prints a
warning in that case; use a token on shared machines.
