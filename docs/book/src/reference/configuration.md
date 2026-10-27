# Configuration options

Helia has no configuration file for stacking. (Updates have a small one of their own, `update.conf`: see [Updates](../user-guide/updates.md).) Every setting is chosen per run, in
the same way in all three front ends. (The desktop app keeps a small settings
file of its own for things it should not ask twice; see
[Using the desktop app](../user-guide/gui.md#settings-file).) This table shows each setting under its name in the
command line, the API job `config` object, and the desktop app.

| Setting | Command line | API `config` field | Desktop app | Default |
|---|---|---|---|---|
| Share of frames to keep | `--keep 25%` | `keep` (`"25%"` or `"500"`) | Keep best frames, 5 to 100 per cent in steps of 5 | the preset's (25% or 30%), else 25% |
| Number of frames to keep | `--keep-frames N` | `keep_frames` | not available (use the share) | none |
| Sharpness metric | `--metric gradient`, `laplacian`, `helmli`, `mfgs`, `dct`, `fourier` or `contrast` | `metric` | Sharpness metric (the desktop app offers gradient energy and Laplacian variance) | `gradient` |
| Alignment point size (pixels) | `--ap-size N` | `ap_size` (8 to 1024) | Point size: 32 to 96 in steps of 8 | chosen from the image size |
| Automatic alignment points | `--ap-auto` | `ap_auto` (excludes `ap_size`) | Align step: Auto chip | off |
| Alignment point passes | `--ap-passes 1` or `2` | `ap_passes` | not available | 1 |
| Smooth outlier alignment point shifts | `--ap-smooth on` or `off` | `ap_smooth` | not available | off |
| Telescope aperture (mm) | `--aperture-mm MM` | `aperture_mm` | Advanced settings: Aperture (mm), 0 to 2000 (seeing line only) | none |
| Field rotation (altazimuth) | `--field-rotation`, `--field-rotation-reference`, `--field-rotation-mirror`, `--site`; `helia derotate` for stacked images | `field_rotation` (`mirror`, `reference`) with `site` | Advanced settings: Field rotation, site, Mirror image | off |
| Find sunspots | `--sunspots` (and `helia sunspots IMAGE`); `--sunspot-umbra`, `--sunspot-penumbra`, `--sunspot-min-area`, `--sunspot-max-area`, `--sunspot-group-gap`, `--wolf-k`, `--north-angle`, `--celestial-north-up`, `--mirror` | `sunspots` (an object with `umbra`, `penumbra`, `min_area`, `max_area`, `group_gap_deg`, `wolf_k`, `north_angle_deg`, `celestial_north_up`, `mirror`) | Stack step: Sunspots chips, observer factor, celestial north | off |
| Time-lapse: reference frame | `helia timelapse --reference first`, `middle` or `N` | `POST /timelapse` `reference` | Time-lapse step: Register to | `middle` |
| Time-lapse: refine by phase correlation | `--no-refine`, `--max-refine PX` | `refine`, `max_refine_px` | Time-lapse step: Refine by phase correlation | on, 3 px |
| Time-lapse: even out the brightness | `--deflicker off`, `match` or `smooth`, `--deflicker-window N` | `deflicker`, `deflicker_window` | Time-lapse step: Brightness, Window | off, 5 |
| Mosaic: join panels | `helia mosaic PANELS -o FILE` | `POST /mosaic` (`inputs`, `output`, `times`, `overwrite`) | Mosaic step: Add panels, Make mosaic, Save mosaic | not applicable |
| Mosaic: matching | `--max-rotation DEG`, `--min-overlap PERCENT`, `--min-correlation R` | `max_rotation_deg`, `min_overlap_percent`, `min_correlation` | Mosaic step: Largest turn | 4, 5, 0.45 |
| Mosaic: blend | `--feather PX`, `--no-gain-match`, `--no-seams` | `feather_px`, `gain_match`, `seams` | Mosaic step: Feather, Match brightness, Seams of least difference | 12, on, on |
| Mosaic: derotate the panels | `--derotate`, `--derotate-to first`, `middle`, `last` or a UTC time, with the options of `helia solar-derotate` | `derotate` (as for `POST /solar-derotation`, `to` may be `first`, `middle` or `last`) | Mosaic step: Derotation | off |
| Solar derotation | `helia solar-derotate IMAGES -o DIR`, `--to first`, `middle`, `last` or a UTC time; `helia timelapse --derotate`, `--derotate-to TIME` | `POST /solar-derotation`; `derotate` in `POST /timelapse` (an object with `to`, `law`, `north_angle_deg`, `celestial_north_up`, `mirror`, `raw_brightness`) | Time-lapse step: Derotation Off, To reference | off |
| Solar derotation: rotation law | `--rotation-law howard`, `snodgrass` or `A,B,C` | `law` | Time-lapse step: Law | `howard` |
| Solar derotation: orientation | `--north-angle DEG`, `--celestial-north-up`, `--mirror` | `north_angle_deg`, `celestial_north_up`, `mirror` | Time-lapse step: Celestial north is up | north up |
| Solar derotation: brightness | `--raw-brightness` | `raw_brightness` | not available | contrast is carried |
| Time-lapse: crop to the common area | `--crop` | `crop` | Time-lapse step: Crop to the common area | off |
| Time-lapse: times of the captures | `--date YYYY-MM-DD`, `--utc-offset HOURS`, `--no-sort` | `times` | Time-lapse step: Times | from the files |
| Time-lapse: output | `-o DIR`, `--format png` or `tiff`, `--movie auto`, `mp4`, `apng` or `none`, `--fps FPS`, `--reuse-stacks` | `output`, `format`, `movie`, `fps`, `overwrite` | Time-lapse step: Movie, Frames, FPS, Save time-lapse | PNG, auto, 10 |
| Estimate the seeing | `--seeing` | `seeing` | Stack step: the seeing line (always on; aperture under Advanced settings) | off |
| Crop to the disc | `--autocrop` | `autocrop` | Stack step: Crop, Disc chip | off |
| Crop margin (per cent of the radius) | `--autocrop-margin P` | `autocrop_margin` (0 to 200) | Stack step: Margin slider, 0 to 50 | 10 |
| Prominence layer | `--prominence`, `--prominence-strength`, `--prominence-stretch`, `--prominence-level`, `--prominence-feather`, `--prominence-reach`, `--prominence-gate` | an `edits` entry `{"op": "prominence", ...}` | Adjust step: Prominence layer and its sliders | off |
| Colour table export | `helia lut PRESET -o FILE` (`--format`, `--size`, `--invert`, `--strength`) | `GET /presets/{id}/lut` | Colour step: Colour table | not applicable |
| Limb-darkening law fitted | `--limb-law linear`, `quadratic` or `neckel` | `limb_law` | Sharpen step: law chips under Limb darkening | `quadratic` |
| Flatten the limb darkening | `--flatten model` or `profile` | `flatten` (or a `flatten` edit) | Sharpen step: Limb darkening Off, Model, Profile | off |
| Global alignment method | `--align auto`, `phase` or `limb` | `align` | chosen automatically; shown after a stack | `auto` |
| Multi-point alignment | `--no-multipoint` turns it off | `multipoint` (`true` or `false`) | always on | on |
| Sigma clipping | `--sigma-clip K` | `sigma_clip` (a number) | Outliers: Sigma chip (fixed at 2.5) | off |
| Robust combination | `--robust none`, `sigma`, `huber` or `winsor` | `robust` | Outliers: Off, Sigma, Huber, Winsor | `none` (weighted mean) |
| Robust threshold (standard deviations) | `--robust-k K` | `robust_k` | not available (the default for the kind) | 2.5 `sigma`, 1.345 `huber`, 2 `winsor` |
| Compute backend | `--backend auto`, `cpu` or `gpu` | `backend` | Backend: Auto, CPU, GPU | `auto` |
| Bayer pattern | `--bayer auto`, `none`, `rggb`, `grbg`, `gbrg` or `bggr` | `bayer` | Bayer pattern in the Open inspector | `auto` (the header, with a pixel check of a SER file that says mono) |
| Bit depth of a SER file | `--bit-depth N` (1 to 16) | `bit_depth` | Bit depth in the Open inspector: Auto, 8-bit, 16-bit | the SER header's |
| Byte order of a SER file | `--endian little` or `big` | `endian` | Byte order in the Open inspector | the header flag, checked against the data |
| Row order of a SER file | `--flip-rows` | `flip_rows` | Row order in the Open inspector | top to bottom |
| Colour channel | `--channel r`, `g`, `b` or `lum` | `channel` | Advanced settings: Channel | the preset's, else `lum` |
| Demosaicing method | `--debayer bilinear` or `mhc` | `debayer` | Debayer: method | `mhc` |
| Filter preset | `--preset ha` (see `helia presets`) | `preset` | Quick stack card: the filter buttons | detected from the file name, folder, sidecar or header |
| Colour | `--colour auto`, `mono` or a preset id | `colour` | Colour step: the look cards | none (mono); the preset's look in the app |
| Invert before colouring | `--invert` | `invert` | Colour step: Invert | off |
| Colour strength | `--colour-strength S` (0 to 1) | `colour_strength` | Colour step: Strength | 1 |
| Output format | extension of `-o` | extension of `output` | Export: TIFF 16-bit, PNG 8-bit or FITS | TIFF in the app; by extension elsewhere |
| Observing site (FITS `OBSGEO-X/Y/Z`) | `--site LAT,LON,ALT` | `site` (`"lat,lon,alt"`) | the `HELIA_SITE` environment variable | none; `HELIA_SITE` if set |
| Hide progress and summary | `-q`, `--quiet` | not applicable | not applicable | shown |

The settings that are not exposed (the search radius, correlation thresholds,
smoothing and similar internals of the alignment) are fixed to the defaults in
`MultipointConfig` and `PhaseOptions` in `helia-core`. They can be changed from
Rust code through the library, but not from the programs.

## Server settings

These apply to `helia serve` only. See [The API](../user-guide/api.md).

| Setting | Option | Default |
|---|---|---|
| Listen address | `--bind` | `127.0.0.1:7870` |
| Directory requests must stay inside | `--data-root` | current directory |
| Bearer token | `--token`, `--token-file`, or the `HELIA_TOKEN` environment variable | none |
| Concurrent jobs | `--workers` | 1 |
| Queued plus running jobs allowed | `--max-queue` | 16 |
| Browser origins allowed | `--cors-origin` (repeatable) | none |
| Extra accepted `Host` headers | `--allow-host` (repeatable) | none |

## Environment variables

| Variable | Used by | Effect |
|---|---|---|
| `HELIA_TOKEN` | `helia serve` | Bearer token, used when neither `--token` nor `--token-file` is given. |
| `HELIA_SITE` | `helia stack`, `helia serve`, `helia-gui` | Observing site `LAT,LON,ALT` for a FITS output, used when `--site` or the job's `site` is not given. |
| `HELIA_GPU_DEVICE` | `helia stack`, `helia serve`, `helia-gui`, tests | Index N (from 0, in the driver's order) of the real GPU adapter to use, instead of the first. For machines with several GPUs where another job holds the first; unset by default. Software adapters are not counted. See the [Performance guide](../user-guide/performance.md#hardware). |
| `HELIA_GPU_DEBUG` | `helia stack`, `helia serve`, `helia-gui`, tests | Any value other than empty or `0`: turn on wgpu's debug object names and validation layers, which are off by default (debug builds used to turn them on). For debugging the GPU kernels only: the Vulkan loader can fault when several threads name objects at once. |
| `HELIA_GPU_TRACE` | `helia stack`, `helia serve` | Any value: print where each GPU stage spends wall time and the GPU time of each kernel, and for each stage how many frames came from the device's frame store and how many from the file, to standard error. |
| `HELIA_GPU_RESIDENT_MB` | `helia stack`, `helia serve`, `helia-gui`, tests | Video memory, in MiB, that the GPU path may use to keep a capture's raw frames between stages (scoring, alignment, alignment points, stacking), so each frame is read from the file and uploaded once. Whole number; `0` turns it off. Default: 6144 on a discrete GPU, 0 on a software or integrated one. The store stops growing when the device reports it is out of memory, and frames that do not fit are read from the file as before; the result is bit for bit the same at any value. See [Frames kept on the device](../user-guide/performance.md#frames-kept-on-the-device-between-stages). |
| `HELIA_GPU_TESTS` | test suites | `software`, `hardware` or `none`; see [the source repository](https://github.com/qisback/helia). |
| `HELIA_FEEDBACK_URL` | `helia feedback`, `helia-gui` | Address of the feedback service, instead of `https://feedback.discworld.casa`. HTTPS only (HTTP for `localhost`). Overrides `feedback_url` in the app's settings file; `helia feedback --url` overrides this. See [Sending feedback](../user-guide/sending-feedback.md). |
| `HELIA_REAL_DATA` | test suites | Directory of real captures for the integration tests. |
| `UPDATE_SNAPSHOTS` | GUI snapshot tests | Set to `1` to regenerate the screenshots. |
| `HELIA_LOG_DIR` | `helia`, `helia serve`, `helia-gui` | Folder for the log file `helia.log`, instead of the OS default. See [Log file and bug report](../user-guide/gui.md#log-file-and-bug-report). |
| `HELIA_GUI_CONFIG` | `helia-gui` | Path of the app's settings file; see [Using the desktop app](../user-guide/gui.md#settings-file). |
