# Documentation coverage

Every user-visible feature of Helia should have three things:

1. a **tutorial**: step by step, on a real capture, with screenshots, the exact
   settings, what to expect and what to do when it goes wrong;
2. a complete **reference**: every command line flag, API field and app control,
   with ranges and defaults;
3. a **changelog entry that says how**: the method, the references it follows and
   the measured effect, with a link to the tutorial.

This page records, feature by feature, where Helia stands against that rule. It is
kept honest on purpose: a feature is **Done** only when all three columns are, and
a gap is named so that it can be closed. The status is judged by a person, but two
things are checked by `cargo xtask docs --check` (and `cargo xtask coverage`): every
subcommand and long flag in the command line reference is mentioned in the guide
(User guide, Step by step or How-to recipes), and every page linked from the table
below exists.

The command line and API references are generated from the code, so they are
complete by construction; the Reference column is only Partial where an app
control (a slider range, a chip) is described in prose and not in a table.

How to read the cells: a link is the page that does the job. *Partial* says what is
missing. *Owner* is the person or agent working on the gap: **H-A** is this
documentation pass, **H-B** is the pass that is writing the tutorials listed
against it, and an issue number means the feature itself is still being changed
there.

| Feature | Tutorial | Reference | Changelog-how | Status | Owner |
|---|---|---|---|---|---|
| Open a capture, read its header, Bayer detection and overrides (`helia info`, Open step, `POST /captures/inspect`) | Done: [Open a capture](../walkthrough/01-open.md), [Fixing a Bayer grid](../recipes/fixing-a-bayer-grid.md) | Done: [CLI](cli.md), [API](api.md), [desktop app](../user-guide/gui.md) | Done: 0.2.0 gives the detection test, the 5% threshold and how red is told from blue; #124 has its entry | Done |  |
| SER and AVI readers, SharpCap sidecars, byte-order check | Partial: [Capturing for Helia](../user-guide/capturing.md) and [Troubleshooting](../user-guide/troubleshooting.md) explain it, with no worked example | Done: [Output formats](output-formats.md), [CLI](cli.md) | Partial: the SER byte-order check is explained; the AVI reader entry is one line | Partial | H-A |
| FITS frames as a capture: a folder or pattern of subs, a cube, image extensions (`helia stack`, `helia info`) | Done: [Stack FITS frames: Seestar subs and cubes](../tutorials/stack-fits-frames.md), [Capturing for Helia](../user-guide/capturing.md) | Done: [CLI](cli.md), [Output formats](output-formats.md) | Done | Done | |
| MP4, MOV, MKV and other compressed video through ffmpeg | Partial: [MP4 from a phone](../recipes/mp4-from-a-phone.md) has no screenshot and no real run | Done: [CLI](cli.md), [Configuration](configuration.md) | Partial: method given, no measured effect | Partial | H-A |
| Demosaicing (bilinear, Malvar-He-Cutler) and colour plane choice | Done: [Fixing a Bayer grid](../recipes/fixing-a-bayer-grid.md), [Colourising](../user-guide/colourising.md) | Done: [CLI](cli.md) | Done: 0.1.0 names the method and the reference (Malvar, He and Cutler) and what the test requires; no figure for the effect exists | Done |  |
| Clipped colour plane: Helia stacks the next plane (#125) | Partial: [The red channel is clipped](../user-guide/troubleshooting.md#the-red-channel-is-clipped-or-the-disc-is-a-flat-white-plate) and the [FAQ](../user-guide/faq.md) explain it with the measured numbers; the app does it too, with a real-capture screenshot in the [time-lapse tutorial](../tutorials/timelapse.md) | Partial: `--channel` help does not mention the fallback | Done: Unreleased entry for #119 with the method and the contrast measured | Partial | H-A |
| Frame ranking, sharpness metrics, keep share and quality weights | Done: [Analyse](../walkthrough/03-analyse.md), [How stacking works](../user-guide/how-stacking-works.md) | Done: [CLI](cli.md), [API](api.md) | Done: 0.1.0 gives both metrics, the blur, the noise floor and the weights | Done |  |
| Global alignment: phase correlation, limb fit and its confidence | Done: [Align](../walkthrough/04-align.md), [How stacking works](../user-guide/how-stacking-works.md) | Done: [CLI](cli.md), [API](api.md) | Done: phase correlation (0.1.0) and the limb fit (0.2.0) both give the method and the measured errors | Done |  |
| Alignment points (multi-point alignment, per-point frame selection) | Done: [Align](../walkthrough/04-align.md) | Done: [CLI](cli.md), [API](api.md) | Done: 0.1.0 gives the method; the measured effect is in the 0.5.0 granulation entry | Done |  |
| Automatic alignment points (`--ap-auto`) | Partial: the Auto chip is in [Align](../walkthrough/04-align.md); the numbers are in [How stacking works](../user-guide/how-stacking-works.md) | Done: [CLI](cli.md), [API](api.md) | Done | Partial | H-A |
| Two-pass alignment point refinement, shift smoothing, residual (`--ap-passes`, `--ap-smooth`, `--ap-residual`) | Partial: a section of [How stacking works](../user-guide/how-stacking-works.md), no step-by-step | Done: [CLI](cli.md), [API](api.md) | Done | Partial | H-A |
| Keeping alignment off burned-in text and empty sky (`--exclude`, `--prominence-band`) | Partial: [Troubleshooting](../user-guide/troubleshooting.md) and [How stacking works](../user-guide/how-stacking-works.md), no worked example | Done: [CLI](cli.md), [API](api.md) | Done: 0.1.0 gives the method and #118 the measured fix | Partial | H-A |
| Stacking: weighted mean, sigma clip | Done: [Stack](../walkthrough/05-stack.md) | Done: [CLI](cli.md), [API](api.md) | Done: 0.1.0 gives the weights, the two passes and the memory bound | Done |  |
| Robust stacking (`--robust huber`, `winsor`) | Done: [Robust stacking against satellites, hot pixels and clouds](../tutorials/robust-stacking.md), [How stacking works](../user-guide/how-stacking-works.md) | Done: [CLI](cli.md), [API](api.md) | Done | Done | |
| Crop to the disc (`--autocrop`) | Done: [Crop the stack to the disc](../tutorials/autocrop.md), the chip in [Stack](../walkthrough/05-stack.md) | Done: [CLI](cli.md), [API](api.md) | Done | Done | |
| Dark, flat and bad-pixel calibration | Done: [Calibrate: darks, flats and bad pixels](../tutorials/calibrate.md), [Calibrate](../walkthrough/calibrate.md), [Calibration](../user-guide/calibration.md) | Done: [CLI](cli.md), [API](api.md) | Done | Done | |
| Self-calibrated flats from displaced-disc frames | Partial: a section of [Calibration](../user-guide/calibration.md) and a troubleshooting entry in the [calibrate tutorial](../tutorials/calibrate.md); no real capture has worked yet | Done: [CLI](cli.md), [API](api.md) | Done | Partial | H-A |
| True colour from a raw colour camera (`--colour true`) | Done: [True colour from a raw colour camera](../tutorials/true-colour.md), [Colourising](../user-guide/colourising.md) | Done: [CLI](cli.md), [API](api.md) | Done | Done | |
| Limb-darkening fit and flatten (`--limb-law`, `--flatten`) | Done: [Flatten the limb darkening](../tutorials/limb-darkening.md), [Sharpening](../user-guide/sharpening.md) | Done: [CLI](cli.md), [API](api.md) | Done | Done | |
| Sharpening: wavelets, unsharp mask, Richardson-Lucy, denoise | Done: [Sharpening like RegiStax](../recipes/sharpening-like-registax.md) (the wavelet layers and their settings), [Sharpen](../walkthrough/06-sharpen.md), [Sharpening](../user-guide/sharpening.md) | Partial: CLI and API are generated; the slider ranges of the app are described in prose | Done: 0.2.0 gives the transform, the regularisation (Dey et al., 2006), the guard and the measured figures | Partial | H-A |
| Richardson-Lucy deconvolution with a measured blur, iteration scrub and noise warning (`--sharpen rl`, `--rl-psf`, `--rl-iterations`, `--rl-guard`) | Done: [Deconvolution](../recipes/deconvolution.md) | Done: [CLI](cli.md), [API](api.md) | Done | Done | |
| Colour grading: hue, saturation, gamma, black and white points and curves on top of a band look (`--grade`) | Done: [Colour grading your band](../recipes/colour-grading.md) | Done: [CLI](cli.md), [API](api.md) | Done | Done | |
| Field rotation correction for altazimuth mounts (`--field-rotation`) | Done: [Tutorial: correcting field rotation](../tutorials/field-rotation.md), [Field rotation](../user-guide/field-rotation.md) | Done: [CLI](cli.md), [API](api.md) | Done | Done | |
| Adjustments: levels, tone curve, crop, rotate, flip, resize, saved presets, `helia edit` | Partial: [Adjust](../walkthrough/07-adjust.md) is one page with one picture; [Sharpening](../user-guide/sharpening.md) has the rest | Done: [CLI](cli.md), [API](api.md) | Partial: 0.2.0 lists operations, no method or effect | Partial | H-A |
| Colour presets, filter recognition, invert and strength (`--preset`, `--colour`, `helia presets`) | Done: [Colour](../walkthrough/08-colour.md), [Colourising](../user-guide/colourising.md) | Done: [Colour presets](presets-table.md), [CLI](cli.md), [API](api.md) | Done: 0.2.0 gives the recognition rule and what a look is made of | Done |  |
| Choosing the channel and band for a filter | Done: [Which channel, or band, should I use for my filter?](../user-guide/faq.md) and [Colourising](../user-guide/colourising.md#how-the-filter-is-recognised) | Done: [Colour presets](presets-table.md) | Done | Done |  |
| Colour table export (`helia lut`, `GET /presets/{id}/lut`) | Partial: a section of [Colourising](../user-guide/colourising.md#exporting-a-colour-table), no screenshot | Done: [CLI](cli.md), [API](api.md) | Done | Partial | H-A |
| Prominence and disc compositing (`--prominence`) | Done: [Tutorial: bringing out prominences](../tutorials/prominences.md), [Prominences](../user-guide/prominences.md) | Done: [CLI](cli.md), [API](api.md) | Done | Done | |
| Sunspots, groups and the Wolf number (`helia sunspots`) | Done: [Tutorial: counting sunspots](../tutorials/sunspots.md), [Sunspots](../user-guide/sunspots.md) | Done: [CLI](cli.md), [API](api.md) | Done | Done | |
| Science outputs: frame quality, alignment point, image science and solar plots in a report (`helia stack --report`, `helia plot`, `helia timelapse --report`, `report` in the API, the Science panel) | Done: [Reading the frame quality graph](../tutorials/reading-the-frame-quality-graph.md), [Reading the alignment point plots](../tutorials/reading-the-alignment-point-plots.md), [Reading the image science plots](../tutorials/reading-the-image-science-plots.md), [Reading the solar plots](../tutorials/reading-the-solar-plots.md) | Done: [Science outputs](../user-guide/science-outputs.md), [CLI](cli.md), [API](api.md) | Done: the Unreleased entries give the method and the measured effect | Done |  |
| Time-lapse: stack, register, even out the brightness, frames and a movie (`helia timelapse`, `POST /timelapse`, Time-lapse step) | Done: [A time-lapse of two H-alpha captures](../tutorials/timelapse.md), [Time-lapse](../user-guide/timelapse.md) | Done: [CLI](cli.md), [API](api.md), [desktop app](../user-guide/gui.md#10-time-lapse) | Done | Done | #26 |
| Solar differential-rotation derotation (`helia solar-derotate`, `helia timelapse --derotate`, `POST /solar-derotation`) | Done: [Derotating H-alpha stacks to one time](../tutorials/derotation.md), [Solar derotation](../user-guide/derotation.md); the real captures are minutes apart, so the effect shown is small (no real series spans hours yet) | Done: [CLI](cli.md), [API](api.md), [desktop app](../user-guide/gui.md#10-time-lapse) | Done | Done | #26 |
| Mosaics from panel stacks: matching, placement, brightness, seams, feathering, derotation (`helia mosaic`, `POST /mosaic`, Mosaic step) | Done: [Joining panels into a full-disc mosaic](../tutorials/mosaic.md), [Mosaics](../user-guide/mosaic.md); there is no real mosaic capture, so the tutorial cuts panels from a real stack | Done: [CLI](cli.md), [API](api.md), [desktop app](../user-guide/gui.md#11-mosaic) | Done | Done | #26 |
| Granulation and seeing report (`helia measure`, `GRAN*` cards) | Done: [Measure a stack: granulation and seeing](../tutorials/measure-and-seeing.md), [The granulation and seeing report](../user-guide/quality-report.md) | Done: [CLI](cli.md), [API](api.md) | Done | Done | |
| Seeing estimation from the capture (`--seeing`) | Done: [Measure a stack: granulation and seeing](../tutorials/measure-and-seeing.md), [Seeing estimation](../user-guide/seeing.md) | Done: [CLI](cli.md), [API](api.md) | Done: listed under 0.6.0, the release that carries it | Done | |
| Export: TIFF, PNG, FITS with SOLARNET keywords | Done: [Export](../walkthrough/09-export.md) | Done: [Output formats](output-formats.md) | Done | Done | |
| Desktop app: Quick stack card, hints, notices, remembered settings, Recent list, log and bug report | Done: [Using the desktop app](../user-guide/gui.md), [Quick start](../user-guide/quick-start.md) | Partial: [Configuration](configuration.md) lists the settings; control ranges are in prose | Done | Partial | H-A |
| In-app feedback and `helia feedback` | Done: [Send feedback](../walkthrough/feedback.md) | Done: [Feedback protocol](feedback-protocol.md), [CLI](cli.md) | Done | Done | |
| About page, version block, authorship | Done: [About](../walkthrough/10-about.md) | Done: [CLI](cli.md) | Done | Done | |
| Install on Windows and Linux: setup program, `.deb`, `.rpm`, AppImage, Open with | Partial: [Installation](../user-guide/installation.md) and [First launch](../getting-started/first-launch.md) now have troubleshooting for every route, checked against the packaging scripts; there are no installer screenshots and no fresh install was run for this check | Done: the installer switches and exit codes are in [Installation](../user-guide/installation.md#windows) | Done | Partial | H-A |
| GPU backend and timings (`--backend`, `--timings`, `HELIA_GPU_TRACE`, `HELIA_GPU_RESIDENT_MB`) | Partial: [GPU acceleration](../user-guide/gpu.md) is explanation, no screenshot | Done: [CLI](cli.md), [Configuration](configuration.md) | Done | Partial | H-A |
| Batch processing | Done: [Batch processing](../recipes/batch-processing.md) | Done: [CLI](cli.md) | n/a | Done | |
| Troubleshooting: soft image, clipped red channel | Done: [The stack looks soft](../user-guide/troubleshooting.md#the-stack-looks-soft), [The red channel is clipped](../user-guide/troubleshooting.md#the-red-channel-is-clipped-or-the-disc-is-a-flat-white-plate), [FAQ](../user-guide/faq.md) | n/a | n/a | Done |  |
| API server and scripting (`helia serve`) | Done: [Drive Helia from your own code with the API](../tutorials/api-server.md), [The API](../user-guide/api.md), the [recipe](../recipes/scripting-with-the-api.md) | Done: [API](api.md), [CLI](cli.md) | Done | Done | |
| Plugins: stack elsewhere, pipeline steps and analysis from separate programs (`helia plugins`, `--plugin`, `--plugin-step`, `--stack-backend`, `plugin_steps` and `stack_backend` in the API) | Done: [Using plugins](../tutorials/plugins.md), [Writing a plugin](../tutorials/writing-plugins.md), [Plugins](../user-guide/plugins.md) | Done: [Plugins reference](plugins.md), [specification](../developer/plugins.md), [CLI](cli.md), [API](api.md) | Done: the Unreleased entries say how, with the standards cited | Done |  |

## The science outputs

The graphs and tables a stack can write. Each has a tutorial that reads it on a real capture
and is listed with its files on [Science outputs](../user-guide/science-outputs.md). Every
one is a PNG with its data as CSV and JSON, drawn by `helia stack --report DIR` (or
`helia timelapse --report DIR`), redrawn by `helia plot KIND DIR`, served by the API as
`GET /jobs/{id}/report/{file}` and shown in the Science panel of the app. The kind in
brackets is the argument of `helia plot`.

**Frame quality** ([tutorial](../tutorials/reading-the-frame-quality-graph.md)):

- the quality graph sorted, with the keep threshold (`quality-sorted`)
- the quality graph in capture order, kept and rejected, with the trend (`quality-order`)
- the histogram of the quality scores (`quality-histogram`)
- the global shift (x, y) of each frame, the drift and the path (`shift-drift`)
- the differential image motion, the seeing, over the capture (`seeing-time`)

**Alignment points** ([tutorial](../tutorials/reading-the-alignment-point-plots.md)):

- the point grid on the reference frame (`ap-grid`)
- the heatmap of the sharpness each point stacks (`ap-quality`)
- the heatmap of how much each point moves (`ap-shift`)
- the vector plot of the mean displacement of each point (`ap-field`)
- the vector plot of the residual against the finished stack, with `--ap-residual` (`ap-residual`)

**Image science** ([tutorial](../tutorials/reading-the-image-science-plots.md)):

- the power spectrum of stack, best frame and sharpened image, with the noise floors and the
  granulation and diffraction cut-offs (`power-spectrum`)
- the MTF-like transfer curves between them (`mtf`)
- the limb-darkening profile against the fitted law, with residuals (`limb-profile`)
- the limb edge, the fitted blur and the point spread function it implies (`limb-psf`)
- the histograms per channel with the clipping marked (`histogram`)
- the noise and the signal to noise (`noise-snr`)

**Solar specifics** ([tutorial](../tutorials/reading-the-solar-plots.md)):

- the Stonyhurst heliographic grid with P, B0, L0 and solar north and east (`grid-stonyhurst`)
- the Carrington heliographic grid (`grid-carrington`)
- the sunspot overlay and group table, with `--sunspots` (`sunspots.png`; the method is in
  [counting sunspots](../tutorials/sunspots.md))
- the registration, brightness and derotation diagnostics of a time-lapse
  (`timelapse-registration`, `timelapse-brightness`, `timelapse-derotation`)
