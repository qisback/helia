# Architecture

Helia turns a capture of the Sun (thousands of seeing-blurred frames) into one
sharp image. This page is the map: which crate does what, how data flows, and
the rules that keep the CPU and GPU paths honest.

## Crate map

| Crate | Kind | Role |
|---|---|---|
| `helia-core` | library | All image maths and file formats. No GPU code, `#![forbid(unsafe_code)]`. |
| `helia-gpu` | library | wgpu compute implementation of the `Backend` trait that lives in `helia-core`. |
| `helia-server` | library | axum HTTP/JSON API over the pipeline (inspect, previews, jobs, WebSocket events, OpenAPI). Embedded by the CLI; the GUI can embed it too. See the API chapter of the book. |
| `helia-report` | library | The science outputs: frame quality, alignment point, image science and solar plots drawn with `plotters` (bitmap backend, bundled IBM Plex Sans, no system libraries), the CSV and JSON behind each, and the self-contained HTML report. Built from a `StackOutput` plus `Extras` read from the capture; `helia plot` redraws a figure from its JSON. Used by the CLI, the server and the app. See Science outputs in the book. |
| `helia-feedback` | library | The feedback relay protocol (wire types, proof of work), screenshot highlights and burn-in, the relay client with its authentication interface, and the save and browser fallbacks. The relay reuses the protocol types with `default-features = false`. |
| `helia-cli` | binary `helia` | `helia stack`, `helia calibrate`, `helia info`, `helia presets` and `helia serve`, with stage lines and a progress bar. |
| `helia-gui` | binary `helia-gui` | egui front end: pipeline rail, viewer, quality curve, inspector, activity notices and toasts, Quick stack and Colour steps. |
| `fuzz` | cargo-fuzz crate, not a workspace member | Feeds arbitrary bytes to the SER, FITS and AVI readers, and the ffprobe JSON and sidecar parsers of the video path. |

`helia-core` modules:

* `io`: `ser` (SER v3 reader and writer), `avi` (uncompressed AVI, including
  OpenDML), `fits` (reader and writer, with `HISTORY` cards), `tiff` (16-bit
  grey and RGB writer), `png` (8-bit grey and RGB writer), `sidecar`
  (SharpCap `CameraSettings.txt` hints), `solarnet` (the SOLARNET FITS
  keywords: capture times, exposure, site, filter wavelength, processing
  steps and the helioprojective coordinate system from the limb fit), `ffmpeg`
  (compressed video through an optional external `ffmpeg`, below), `source`
  (the `FrameSource` trait that hides which container a capture came from, and
  `OpenOptions`, the user's overrides), `detect` (finds a Bayer mosaic in
  frames a SER header calls mono, from the 2 x 2 pixel-position means on the
  disc) and `capture` (opens a capture with the overrides and returns a
  `CaptureReport`: the header as written, the values used with their source,
  the detection and the notices, which `helia info`, the app's Open step and
  the API all print).
* `calibrate`: dark, flat and bad-pixel calibration. `master` builds a master
  dark, bias or flat from calibration captures (SER, AVI, video, FITS stills or a
  folder of them) by median or sigma-clipped mean, scaling and normalising a flat
  per colour of the mosaic; `badpix` finds hot, cold and stuck pixels and replaces
  them with the median of their same-colour neighbours; `diag` reads a flat for
  Newton rings, etalon fringes, vignetting and dust; `spec` turns "this dark, this
  flat, this map" (master files or the captures to make them from) into a
  `Calibration`, which `Calibration::plan` checks against the frames and compiles
  to per-pixel tables; `source::CalibratedSource` wraps a capture so that every
  frame it serves is calibrated, in `read` and in the raw bytes the CPU and GPU
  decoders share (as `f32`, before debayering, scoring or alignment).
* `debayer`: bilinear and Malvar-He-Cutler demosaicing to a mono image, with a
  choice of red, green, blue or mean channel.
* `quality`: sharpness metrics, ranking, `Keep` (a percentage or a count) and
  the soft weighting curve.
* `quality_extra`: the CPU-only ranking metrics (Helmli-Scherer, MFGS, DCT energy
  ratio, band-limited Fourier power, local contrast). `Metric::on_gpu` says which
  metrics a device implements; a backend declines the others and the CPU scores them.
* `align`: `global` (FFT phase correlation), `limb` (global shift from a robust
  circle fit to the solar limb, with a confidence), `method` (the choice between
  the two, per-frame confidence and the report shown to users) and `multipoint`
  (alignment point grid, local shifts, blending).
* `resample`, `stack`: sub-pixel interpolation and the accumulators.
* `backend`: the `Backend` and `Accumulator` traits and the CPU implementation.
* `mask`: what the alignment must not look at. `detect_overlay` finds burned-in
  text, `fit_disc` fits the limb, and `MaskedSource` wraps a capture so that
  masked rectangles read as one flat level, in `read` and in the raw bytes the
  GPU decodes.
* `pipeline`: the end-to-end driver.
* `about`: the author, copyright and licence notices, and the build record
  (commit, date, profile, target, compiler) that `build.rs` captures. Shared by
  `--version`, the About page and `GET /api/v1/version`.
* `colour`: the solar filter presets (a look, a recommended Bayer channel and
  frame share each), the colour mapping sampled into a 4096-entry `ColourLut`,
  and the CPU reference `apply_lut`; also detection of the filter from text and
  the alignment point size for an image scale. `Backend::colourise` defaults to
  the CPU reference and the GPU backend overrides it with a kernel.
* `recommend`: the "just press Stack" settings for a capture, from the filter
  (chosen, or read from the file name, folder, sidecar and SER header) and the
  image size, including the default sharpening of the filter.
* `edit`: sharpening and everyday edits as an ordered, recorded `EditList` of
  `Op`s (serde, so JSON in presets, the API and file metadata): `wavelet` (the
  a-trous transform, unsharp mask, denoise), `deconv` (Richardson-Lucy with total
  variation, the PSFs and the limb and sunspot ringing guard), `tone` (levels and
  a monotone curve), `geometry` (crop, rotate, flip, resize), `defaults` (the
  per-filter values, which live beside the colour presets in `colour`), `options`
  (named options such as the command line's to a list), `parse`, `store` (saved
  presets) and `conv` (the convolutions). `Backend::apply_edits` defaults to the
  CPU reference, `edit::apply_cpu`, and the GPU backend overrides it.
* `output`: the last step, shared by the command line, the app and the API:
  edits, then colour, then the file with the edits recorded in it; FITS is the
  unedited stack unless asked. `io::any` reads TIFF, PNG and FITS back for
  `helia edit`.
* `picture`, `captime`, `timelapse` and `animate`: the series tools. `picture`
  moves, warps, scales, crops and saves a mono or colour image alike; `captime`
  finds when a capture was taken (SER stamps, a FITS header, or the file name);
  `timelapse` registers stacked images to a reference (the limb fit, then a phase
  correlation of the disc centre) and evens out their brightness; `animate`
  writes an MP4 through `ffmpeg` (raw frames on its standard input) or an
  animated PNG itself. The command line, the API (`POST /timelapse`) and the app
  all call `timelapse::run` or `timelapse::register`.
* `derotate`: the Sun's differential rotation. A pixel of the result is traced back through
  the sphere (latitude and longitude from the central meridian, the Carrington frame, the
  rotation law, `B0`, `P` and `L0` from `ephemeris`) to where its feature was at the
  image's own time, and sampled with a Lanczos kernel; `timelapse` calls it between the limb
  shift and the phase correlation so the correlation follows the pointing. `derotate::synth`
  draws a rotating spotted sphere for tests.
* `mosaic`: panels of the Sun into one picture. `pair` matches two panels on their overlap (masked
  normalised cross-correlation over trial rotations on shrunk detail pictures, then a Lucas-Kanade
  refinement on a pyramid), `solve` places all the panels by robust weighted least squares (rotations
  first, then shifts), and `blend` resamples them onto a canvas, matches gain and offset by one least
  squares fit over all the overlaps, finds seams by dynamic programming and feathers them. With
  derotation each panel is derotated to one time with the disc fitted to a first joining.
* `testutil` (feature `testutil`): synthetic suns with known ground truth.

## Compressed video

MP4, MOV, M4V, MKV, WebM and AVI files with a compressed codec go through the
`ffmpeg` and `ffprobe` programs, found on the `PATH` or in `HELIA_FFMPEG` and
`HELIA_FFPROBE`. Helia never links or bundles them; they are an optional external
tool, run as child processes from an argument vector (never a shell), with the
input as an absolute path after `-i`, `-protocol_whitelist file`, and no user
text as an option.

The pipeline reads a capture more than once and out of order, and the GUI
scrubs it, but video codecs only seek to key frames. So `io::ffmpeg` decodes a
video **once** into an uncompressed SER file in a cache directory (the OS cache
folder, `--cache-dir` or `HELIA_CACHE_DIR`), with a free-space check first, and
the capture is then an ordinary SER source: random access, the concurrent
decoder, GPU decode. 8-bit video stays 8-bit (`gray`) and anything deeper is
`gray16le`; a colour channel chosen with `--channel` is extracted by `ffmpeg`
(`extractplanes`), so the cache holds one plane. `ffmpeg` applies the file's
rotation. The file is deleted when the last handle to it drops (a `VideoSource`
or the `keep_alive` handle the GUI holds), cancelling kills the child, and an
older leftover is swept on the next decode.

Streaming each pass straight from `ffmpeg` was the alternative, and measured
slower. On a 3840 by 2160 8-bit H.264 clip (898 frames, 25 fps, 96 Mbit/s), on
an AMD EPYC 7543 with 24 threads and an A100 (the project's GPU build server), at
this change: decoding into the cache took 12.7 s (about 70 frames per second,
including writing 7.5 GB), and the rest of the stack, from the cache, 16.3 s.
`ffmpeg` alone decodes about 100 to 125 frames per second there, so a streamed
pass over the same 898 frames costs 7 to 9 s, and the pipeline makes at least
four (score, global alignment, alignment points, stacking), plus the GUI's
random access. The cache costs disk space instead: a frame is `width * height`
bytes (8.3 MB at 4K), which is why the free-space check exists. Numbers for other
hardware will differ; the ratio is what carries over.

## Data flow

```
capture file --FrameSource--> raw frame --debayer--> mono Frame (f32, 0..1)
  0a. calibrate (optional): (raw - dark) / flat, bad pixels replaced, applied to
      the stored plane of every frame as it is read, before the debayer
  0. mask burned-in text and user exclusions (flat in every frame read below)
  1. score every frame            (quality, noise floor subtracted)
  2. rank; keep best N            (--keep 25% or --keep-frames N)
  3. reference = aligned mean of the best few frames
  4. register candidates globally: limb fit when the full disc is in view and
     the fit is confident, else phase correlation (sub-pixel), each with a
     confidence
  5. multipoint: per alignment point and frame, measure the local shift
     by correlation, score the window, and let each point pick its own
     best frames
  6. stack: weighted mean of each point's frames (optionally sigma clip, Huber
     or winsorised: two passes, the second using the first's mean and spread)
  7. blend the points with raised-cosine weights; the global stack fills
     featureless areas
  8. normalise and write 16-bit TIFF or FITS (the FITS header carries SOLARNET
     metadata built from the capture, the limb fit and the list of steps run)
  9. optionally sharpen and adjust the mono stack (`edit`), then colour it
     through a preset's table and write an RGB TIFF or PNG (FITS stays mono and
     is the unedited stack unless asked; the edits and the look are recorded in
     the file)
```

Frames are streamed in batches of about two per thread and are never all held
in memory. The capture is read once to score, once to register, once to
measure alignment points and once or twice to stack (twice only with a
robust mode: sigma clip, Huber or winsorised mean). Progress and cancellation are reported through a callback and an
`AtomicBool`.

## Where the time goes

`helia stack --timings` prints wall time per stage and busy time per kind of
work summed over threads (`helia_core::profile`). Wall time says what to wait
for; busy time says what to optimise, because stages overlap decoding with
maths on every thread.

How the hot paths are built:

* Decode. `io::raw::RowLayout` turns stored bytes into normalised rows;
  `FileFrames` reads with positional I/O (safe `std`, no memory map, so
  `helia-core` stays free of `unsafe`) and implements `SharedFrames`, which the
  pipeline calls from many threads at once. For a Bayer capture the rows go
  straight into `Debayer::apply_rows`, a fixed 5x5 stencil per Bayer phase, so
  no raw `f32` frame is ever materialised. Sources that cannot be read
  concurrently use the sequential `FrameSource::read` path.
* Quality. Row-based 3x3 blur and gradient with no per-pixel closures;
  `WindowScorer` reuses one blurred frame for every alignment-point window.
* Limb fit. 720 rays from the reference disc sample the lightly blurred frame
  every half pixel; the edge on a ray is the centroid of the fall in brightness
  where it exceeds a quarter of its peak, which averages noise over the whole
  edge and is unbiased for a symmetric blur. A Tukey-weighted Gauss-Newton fit
  gives the circle; a second pass uses a window sized from the first pass's edge
  width. The centre error is the larger of the model estimate and a
  delete-one-eighth jackknife, which is honest about noise shared by
  neighbouring rays. A limb cut by the frame holds the reference radius. The
  fit always runs on the CPU, in parallel over frames; frames below the
  confidence floor fall back to the phase correlator.
* Global alignment. Phase correlation works on the half spectrum of the real
  frame (`realfft` along rows, complex transforms down the columns, stored
  transposed). Plans, filters, the reference spectrum and the working buffers
  live in the `PhaseCorrelator` and are shared by all threads.
* Alignment points. Correlation sums are running sums plus one vectorised dot
  product per offset.
* Stacking. `resample_region` filters each source row horizontally once and
  combines rows vertically.

Conventions worth knowing:

* A shift `s` means `frame(x) = reference(x - s)`; aligning samples the frame
  at `x + s`.
* Selection is global and per point. The best `keep` frames by global quality
  form the kept set. With multipoint on, `keep * 2` frames are registered and
  each alignment point stacks its own best `keep` of them by local sharpness.
* Weights are a smooth function of quality over the selected frames, not a
  cutoff, so marginal frames still contribute a little.
* Masking happens at the source. `Pipeline` samples about 16 frames, flags
  pixels that flip between white and dark (a timestamp), adds the user's
  rectangles and wraps the capture in `MaskedSource`, so scoring, registration,
  alignment points and the stack all see the same flat patch on the CPU and
  the GPU. The limb is fitted on the reference (a robust circle through the
  steepest edge of each scan line) and an alignment point is placed only when
  its centre is inside it, plus an optional prominence band, and its window
  overlaps no masked rectangle.
* Alignment points are correlated on a smoothed, background-subtracted copy of
  the frame (a Gaussian band-pass), because on low-contrast H-alpha detail the
  slow limb-darkening gradient otherwise flattens the correlation peak.
* A point with a weak or ridge-like correlation (a straight stretch of limb
  constrains the shift across the edge only) takes the inverse-distance
  average of its neighbours' shifts instead of its own noisy estimate.
* Everything that deposits a frame onto the output goes through the
  `Accumulator` interface (or `TileStack` for tiles). Drizzle will be another
  `Deposit` variant; today it returns `Unsupported`.
* The robust modes (`StackMode::SigmaClip`, `Huber`, `Winsor`) add a second
  streaming pass: pass one gathers the weighted mean and spread per pixel, pass
  two sums with weights from `RobustKind::apply`. For whole frames this is the
  `RobustAccumulator` interface (CPU in `backend.rs`, GPU in
  `robust_accumulate.rs`); for alignment point tiles it is `TileStack`. Memory
  stays O(pixels).

## API server

`helia-server` is a thin layer over `Pipeline`; it adds no image maths.

* `Server::builder()` validates settings (a non-loopback bind needs a token)
  and canonicalises the data root. `Server::router()` gives an in-process
  `axum::Router` (used by the tests); `Server::bind()` takes the socket.
* `paths::Sandbox` is the only way a client string becomes a `PathBuf`: `..`
  is rejected, then the path is canonicalised through its deepest existing
  ancestor (so symlinks are followed) and must stay under the root.
* `jobs::JobManager` holds the registry and a FIFO worker pool (a tokio
  semaphore sized by `--workers`; the GPU is one device, so the default is 1).
  Each job runs `Pipeline::run` in `spawn_blocking` with its own `AtomicBool`;
  the progress callback updates the job record and feeds a broadcast channel
  that the WebSocket handler forwards. Lock order: registry, then job state.
* A middleware does the loopback `Host` check (DNS-rebinding defence) and
  bearer authentication; CORS is a separate outer layer, off by default.
* The OpenAPI document is generated from `#[utoipa::path]` annotations, so it
  cannot drift from the handlers' signatures.
* A job that was not given `channel`, `keep` or `ap_size` takes them from
  `helia_core::recommend` once the capture is open, then reports the stages
  `opening`, the pipeline's, and (when the output is coloured) `colourise` and
  `write`. `GET /presets` is rendered from `helia_core::colour`.
* `ap_auto` and `autocrop` set `StackConfig::auto_ap` and `autocrop`. The
  choice of alignment points (`helia_core::autoap`) happens in
  `Pipeline::run` after the reference exists: a limb fit gives the scale, a probe
  of twelve candidates measures the local warp, `autoap::choose` returns the
  size, spacing and search radius, and the provenance records what ran, not what
  was asked for. The crop (`helia_core::autocrop`) runs last, after
  normalisation, so levels do not depend on it; `StackOutput::disc` is in the
  cropped image's pixels while `grid`, `masked` and `reference` stay in the
  capture's. The jobs report both in `summary.auto_ap` and `summary.crop`.
* `helia_core::limbdark` fits a law to the azimuthal median profile of the
  stack (`LimbFit`, linear in the coefficients, solved by Gaussian elimination)
  and builds a `FlattenPlan`: a gain table of 513 nodes over the radius. The
  `flatten` edit (`Op::Flatten`) must be first in a list, because it measures the
  stack itself; `EditList::resolve` fits it (`State::Ready` or `State::NoDisc`)
  and the CPU (`FlattenPlan::apply`) and the GPU (`edit_flatten.wgsl`) read the
  same table the same way, so parity is by construction. The pipeline fits the
  configured law on every stack (`StackOutput::limb`) without changing it.
* The prominence layer (`Op::Prominence`, `helia_core::edit::prominence`) is a
  CPU-only edit, run on the host like the geometry edits (the device path hands it
  to `apply_op`). It fits the disc on the frame it is given, models the glow as a
  table of ring medians in 24 angular sectors (windows of three, interpolated),
  blurs and gates the excess, stretches it with `asinh` and joins it to the disc
  with a feathered smoothstep. Inside the limb the output is the input, bit for
  bit. `helia_core::lut_export` turns a `ColourLut` into `.cube`, CSV, ImageJ
  `.lut` or PNG; the CLI (`helia lut`), the API (`/presets/{id}/lut`) and the
  Colour step all call it.
* Field rotation (`helia_core::field_rotation`, `StackConfig::field_rotation`):
  `FieldRotation::plan` turns the frame stamps into a parallactic angle per frame
  (the Sun's apparent position from `ephemeris`, Meeus 14.1) and a turn for each
  relative to the reference frame. `run_impl` (and `score`) wrap the source, after
  calibration and masking, in a `DerotatedSource`, which demosaics a raw colour
  frame to the configured plane, turns it about the frame centre with Lanczos
  and has no `shared()` decoder, so every later stage sees one orientation.
  `StackOutput::field_rotation` carries the plan and the provenance gets a
  `FIELD-ROTATION` step. `helia derotate` applies `rotate_ccw` to finished images.
* True colour (`helia_core::truecolour`, `StackConfig::true_colour`): `run`
  runs the usual stages on the luminance (`run` switches the channel before it
  calls `run_impl`), then `stack_plane` three times with `with_channel` red,
  green and blue over the one `StackStage` (candidates, shifts, scores, reference,
  point set and table), the stack stage's progress scaled to thirds. `compose`
  registers red and blue to green, balances and scales, `StackOutput::colour`
  holds the picture and `image` its luminance. The backend stacks each plane as
  it would a mono stack, so GPU parity is the existing stack parity plus the
  `true_colour_stack_matches_the_cpu_reference` test. `OutputPlan::rgb` carries
  the picture to `prepare` (edits per channel by `edit_rgb`) and `finish`
  (`save_rgb`: TIFF, PNG or a FITS cube from `write_fits_planes`).
* `JobConfig` mirrors `helia stack`'s options. The CLI keeps its own clap
  types; a change to one needs the same change in the other.

## The CPU is the specification

`CpuBackend` defines the correct result. Every GPU kernel has a CPU reference
and a parity test (`crates/helia-gpu/tests/parity.rs`) that compares the two:

* accumulated sums within 1e-4 per unit weight, validity maps exactly equal;
* two-pass robust stacks (sigma, Huber, winsor) within 2e-4 per pixel;
* sharpness within 2e-3 relative;
* colouring within 1e-5 per sample (the same table, the same interpolation);
* every edit operation within 1e-4 (the filters) or 2e-3 (iterated
  deconvolution), observed about 1e-6 or better.

Observed differences are several orders tighter (about 1e-6 and 1e-7) because
the GPU kernels use the CPU's tap weights and the same operation order. The
trait lives in `helia-core`, so the core builds and tests without any GPU
dependency, and tolerances are stated next to the tests rather than hidden.

## GPU backend selection

`helia-gpu` asks wgpu for a high-performance adapter across Vulkan, DX12,
Metal and GL.

* `--backend auto` (default): take the adapter wgpu ranks best (discrete,
  integrated, other, virtual), otherwise the CPU path. A CPU-hosted software
  adapter (WARP, lavapipe, llvmpipe) is never chosen, because it is slower
  than the multi-threaded CPU path (`GpuBackend::is_software`, #28).
* `--backend cpu`: always the CPU.
* `--backend gpu`: a real GPU, falling back to wgpu's software adapter (WARP
  on Windows, lavapipe on Linux); an error if neither exists.

A process has one device per adapter request: `GpuBackend::shared` hands every
caller the same `Arc`, and the device is torn down when the last holder drops
it. Creating and destroying devices is serialised process-wide (several at once
hung the NVIDIA Vulkan driver), and a device waits for its queue to go idle
before it is released. The wgpu instance is created once per backend set and
never dropped: destroying the last instance makes the Vulkan loader unload the
driver libraries, and any thread that has called into a driver then jumps into
unmapped memory when it exits, because its thread-local destructors point into
the unloaded library (#31). A static device that is never dropped had the
mirror-image problem at process exit.

A backend takes over whole pipeline stages (`Backend::score_frames`,
`register_frames`, `measure_points`, `stack_frames`). Each receives the
capture as `SharedFrames` (stored bytes, read by the backend itself) and
returns `Ok(None)` to decline, in which case the pipeline runs the CPU code for
that stage. The GPU backend uploads the stored bytes of a batch of frames,
converts and demosaics them in a compute shader, and keeps them on the device
for the stage; only small results (sums, shifts, per-point records)
come back. A helper thread reads the next batch from the file while the
device works on the current one, and the scoring and alignment point stages
keep two batches in flight (two sets of device buffers) so the host's finishing
work overlaps the device's.

Between stages the stored bytes of a frame stay on the device. A frame is
uploaded by the first stage that needs it and also copied, device to device,
into a store of chunk buffers (`crates/helia-gpu/src/resident.rs`); every later
stage finds it there (`RawBatch.slots`) and copies it from the store into the
batch's raw buffer instead of reading the file and uploading it. What is kept
is the stored bytes, not the decoded frames, so the decode kernel runs on the
same input as before and the result is the same to the bit. The store hangs off
the source (`SharedFrames::device_cache`, a `DeviceCache`), so it is freed with
the capture and never serves one capture's frames for another's; the pipeline
also asks the backend to release it when a run or a scoring pass ends
(`Backend::release_frames`), so a source the application keeps open does not
hold video memory. It is bounded by a byte budget (`HELIA_GPU_RESIDENT_MB`, else
6 GiB on a discrete adapter and 2 GiB on one that shares the machine's memory).
wgpu does not report an adapter's memory, so the guard is the device itself:
when it reports out-of-memory the store stops growing, gives back 1.5 GiB for
the stages' buffers and keeps what it holds; frames that do not fit are read
from the file. A stage names the frames it will use (`Resident::begin_stage`);
frames outside that set are evicted to make room for frames inside it, and
nothing inside it ever is.

What runs where today: frame scoring (gradient energy and Laplacian variance),
global registration, alignment point measurement and mean stacking (tiles and
whole frame) all run on the GPU from the stored bytes. Sigma-clipped stacking
stays on the CPU (it needs two passes), as does registration of frames whose
transform lengths exceed 2048. `Backend::supports` tells the pipeline which
stages a backend implements, so the CPU keeps its single pass over the frames
for registration and alignment points while a device that takes the points
gets the global shifts first.

Registration on the device follows the CPU correlator step for step: windowed
rows transform to the non-negative half spectrum (stored transposed), columns
follow, the cross-power spectrum with the reference is whitened and
band-passed, and two inverse transforms give the correlation surface. The
transforms are in-place decimation-in-time FFTs of one line in workgroup memory
(radices 2, 3, 4 and 5, so the same 5-smooth lengths as the CPU, up to 2048).
The peak and the refinement iterations are further kernels; only the shifts
are read back.

The alignment point stage keeps the correlation surfaces and the noise-floor
samples on the device. The surface kernel finds the best offset of each point,
checks it against the correlation threshold and the border of the search area,
and returns a 14-float record (offset, 3x3 neighbourhood of the peak, window
sums); a radix-selection kernel (`floor_select.wgsl`) returns the noise floor
of each frame, the value of rank n / 100 among the finite samples, bit for bit
as `floor_of` on the host. The sub-pixel fit (`fit_patch`), the sharpness
formula (`combine_sums`) and the interpolation of unreliable points
(`plan_fill`) are the CPU's own functions, so the two sides cannot disagree on
decisions, only on float rounding. The fit stays on the host because a fit on
the device (tried) differs from it in the last digits: the compiler's fused
multiply-adds and the device's `log` are not the host's. Device
kernels follow three rules learned from the WARP adapter: every thread of a
workgroup reaches each barrier (no early return before one), per-batch
parameters live in buffers rewritten before the submission that reads them,
and workgroup memory is not zero-filled (every kernel writes it before it
reads it; the zero-fill made large arrays compile for tens of seconds).
`HELIA_GPU_TRACE=1` prints wall time and per-kernel GPU time per stage.

Tests choose their adapter with `HELIA_GPU_TESTS`: `software` requests the
fallback adapter, `hardware` the normal one, unset or `none` skips GPU tests
with a printed note. With `software` or `hardware` set, a missing adapter is a
failure so CI cannot skip silently.

## The desktop app

`helia-gui` is a thin shell: all image work stays in `helia-core` and
`helia-gpu`. Its modules:

* `theme`: colour tokens (`Palette`), the embedded IBM Plex fonts (SIL OFL 1.1)
  and the egui style. A light palette would be a second `Palette` constant.
* `model`: the view model, with no egui widgets and no I/O. Step statuses,
  overall progress, the sorted quality curve and its geometry, keep and
  threshold maths, zoom and pan, alignment point rectangles, label formatting.
  This is where the unit tests are.
* `worker`: every slow operation on a background thread. One long-lived thread
  decodes preview frames (the newest request wins, so scrubbing never queues)
  and uploads textures; other threads open captures, score, run the pipeline
  with progress and a cancel flag, probe GPU adapters and run file dialogs.
  They talk to the UI through one `Event` channel and call
  `request_repaint`; the UI thread never blocks.
* `app`: state, event handling, shortcuts and the responsive rule (panels
  collapse below 1100 px). Decoded frames are cached as textures, at most 40,
  evicting the frame furthest from the current one.
* `capture_view`: the Open step's notices, header and "Helia will use" lists and
  override chips, drawn from the `CaptureReport` the worker returns.
* `edits`: the Sharpen and Adjust settings and how they become an edit list, the
  preview size (a power of two within 1280 pixels) and the geometry-only list for
  the "before" half of a split view. Unit tested.
* `topbar`, `centre`, `inspector`, `editors`, `curve`, `widgets`: drawing. Custom
  widgets report AccessKit labels and roles. A preview is one background job at a
  time (`Workers::render`): the edits on a shrunk copy of the stack, then the
  colour, uploaded as a texture; a change while one runs is folded into the next.
  The backend is kept between previews so a GPU device is opened once.
* `demo`: `--open <capture> --shots <dir>` runs a scripted stack and saves
  screenshots, for checking real captures by eye.

The window is snapshot tested with `egui_kittest` (`snapshots` module): a
synthetic capture from `helia_core::testutil`, injected state at each step, and
a software wgpu adapter. The PNGs live in `docs/book/src/images/ui/` and are
embedded in the user guide. The test runs only when `HELIA_GPU_TESTS=software`
and fails on more than 8 pixels differing at the default per-pixel tolerance;
`UPDATE_SNAPSHOTS=1` regenerates the images.

## Real captures

`crates/helia-core/tests/real_data.rs` runs against real SharpCap and SER
captures when `HELIA_REAL_DATA` points at them. The files are large and are
never stored in the repository (`*.ser` and `*.avi` are ignored). They run every
night on the GPU runner (`real-data-nightly`), where the four captures are staged
and hash-checked.
