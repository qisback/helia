# Science outputs

Helia can write the graphs and tables that astrophotographers review when they
judge a stack: how good each frame was and where the keep threshold fell, how the
image drifted, how the seeing changed over the capture. They are one layer with one
shape, whichever way you ask for them:

* **A directory per stack** (`helia stack --report DIR`, `"report": true` in an API
  job, **Save report** in the app) holding every figure as a **PNG**, the numbers
  behind it as **CSV** and **JSON**, and **`report.html`**: one self-contained page
  with the figures inline, the tables, the settings the stack ran with and, for a
  FITS output, the header cards.
* **One figure at a time** (`helia plot KIND DIR`), redrawn from the report's JSON at
  any size, without stacking again.
* **The Science panel** of the Stack step, which shows the figures in the viewer.

The figures are drawn by Helia itself (the pure-Rust `plotters` crate, MIT licensed,
with the IBM Plex Sans face the app already carries), so a report needs nothing
installed and looks the same on every machine. The data files are the contract: a
figure is drawn from its data, and you can redraw it in Python from the CSV.

For a worked example on a real capture, read
[Tutorial: reading the frame quality graph](../tutorials/reading-the-frame-quality-graph.md).

## What each figure is

| File | Group | What it shows |
|---|---|---|
| `quality-sorted.png` | Frame quality | Every frame's quality score, best first, with the keep threshold and the kept band marked. The AutoStakkert quality graph. |
| `quality-order.png` | Frame quality | The same scores in capture order: kept and rejected points, a running median, the threshold, and the scores an artefact had raised. |
| `quality-histogram.png` | Frame quality | The distribution of the scores, the bins above the threshold in green. |
| `shift-drift.png` | Frame quality | The global shift (x and y) of each registered frame against the reference, the straight-line drift and the jitter about it, and the path of the image in the plane. |
| `seeing-time.png` | Frame quality | The differential image motion (the seeing index) of each sampled frame over the capture, a window average and the whole-capture index. |
| `ap-grid.png` | Alignment points | The reference frame with every alignment point window and centre, the skipped windows, and the fitted disc. |
| `ap-quality.png` | Alignment points | Heatmap of the mean sharpness of the frames each point stacks. |
| `ap-shift.png` | Alignment points | Heatmap of how much each point moves from frame to frame once the global shift is taken out (RMS pixels). |
| `ap-field.png` | Alignment points | Vector plot of each point's mean displacement from the global shift. |
| `ap-residual.png` | Alignment points | Vector plot of the residual of the shifts against the finished stack (needs `--ap-residual`). |
| `power-spectrum.png` | Image science | Radially averaged power spectrum of the disc centre for the stack, the best single frame and the sharpened image, with their noise floors, the granulation peak and roll-off, the sampling limit and the diffraction cut-off. |
| `mtf.png` | Image science | MTF-like transfer curves: stack over best frame and sharpened over stack (amplitude ratios of the spectra), against the diffraction-limited MTF of the aperture. |
| `limb-profile.png` | Image science | The limb-darkening profile (median of each ring against mu) with the fitted law and the residuals. |
| `limb-psf.png` | Image science | The limb edge across all angles, the Gaussian-blurred step fitted to it and the point spread function it implies. |
| `histogram.png` | Image science | Histograms per channel of the written image and of the raw capture, with the share at the top and at zero and the clipping plateau marked. |
| `noise-snr.png` | Image science | Signal to noise of the best frame, the stack and the sharpened image, and the gain against the square root of the frames. |
| `grid-stonyhurst.png` | Solar specifics | The Stonyhurst heliographic grid (15 degree parallels and meridians from the central meridian) on the stack, with P, B0 and L0 and solar north and east marked. Needs the time of the capture. |
| `grid-carrington.png` | Solar specifics | The same grid labelled in Carrington longitude. |
| `sunspots.png` | Solar specifics | With `--sunspots`: the flattened disc with each spot outlined and the groups numbered, and a table of the groups in the report. |
| `timelapse-registration.png` | Solar specifics | Time-lapse (`helia timelapse --report`): the shift applied to each frame, the limb fit confidence and the phase correlation PSR. |
| `timelapse-brightness.png` | Solar specifics | Time-lapse: the brightness of each frame before and after `--deflicker`. |
| `timelapse-derotation.png` | Solar specifics | Time-lapse with `--derotate`: the largest displacement the Sun's differential rotation caused in each frame. |

## On the command line

`helia stack ... --report DIR` writes the report into `DIR` (created) as the last step.
It switches the seeing probe on, which reads about 120 more frames. Add
`--aperture-mm` (and `--wavelength-nm` for a filter Helia does not know) for `r0` and
a grade in the seeing figure's title. Nothing else about the stack changes: the image
is the same with and without `--report`.

`helia plot KIND INPUT` draws one figure:

* `KIND` is `quality-sorted`, `quality-order`, `quality-histogram`, `shift-drift`,
  `seeing-time`, `ap-grid`, `ap-quality`, `ap-shift`, `ap-field`, `ap-residual`,
  `power-spectrum`, `mtf`, `limb-profile`, `limb-psf`, `histogram`, `noise-snr`,
  `grid-stonyhurst`, `grid-carrington`, `timelapse-registration`,
  `timelapse-brightness`, `timelapse-derotation` or `all`.
* `INPUT` is a report directory (it reads `frame-quality.json`, `ap-data.json`,
  `ap-reference.png`, `image-science.json`, `solar.json` and `solar-reference.png`, or
  for a time-lapse report `timelapse.json`), or a capture, which
  is stacked first with the `helia stack` options you give after `--`:
  `helia plot shift-drift sun.ser -- --keep 10% --preset ha`.
* `-o FILE` is the PNG to write (default: `KIND.png` in the current directory); for
  `all` it is a directory (default: the current one).
* `--size WIDTHxHEIGHT` is the picture size in pixels, 200 to 8000 wide and 150 to
  8000 high (default `960x540`).

A figure with no data is skipped with the reason (`skipped: seeing-time: ...`) when
you ask for `all`, and is an error when you ask for it by name.

## The files

All names are fixed. CSV is UTF-8 with a header row and `.` for the decimal point;
a blank cell means "not measured".

### `frame-quality.csv`

One row per frame of the capture, in capture order.

| Column | Meaning |
|---|---|
| `frame` | Frame index, from 0. |
| `score` | Quality score as ranked: with robust ranking, an artefact's raised score is replaced by its neighbours' level. Units depend on the metric; only the order and ratios matter. |
| `rank` | 0 for the best frame. |
| `kept` | 1 when the frame went into the stack. |
| `measured_score` | The score the metric measured, only for a frame whose score was replaced. |
| `dx_px`, `dy_px` | Global shift of the frame against the reference, in pixels (positive: the content moved right or down). Only the candidate frames (twice the frames kept when alignment points are on) are registered; the rest are blank. |

### `frame-quality.json`

The same numbers with their context; `helia plot` reads it. Fields: `version` (1),
`metric` (the ranking metric's name), `frames`, `keep`, `threshold` (the score of the
lowest kept frame), `scores` and `rank` (arrays in capture order), `kept` (frame
indices, best first), `outliers` (`frame`, `measured`), `shifts` (`frame`, `dx`, `dy`),
`drift` (`dx_per_1000`, `dy_per_1000` in pixels per 1000 frames, the line's
intercepts `dx0`, `dy0`, and the jitter `dx_jitter`, `dy_jitter`, the RMS about the
line), `score_summary` and `kept_summary` (`n`, `min`, `p05`, `median`, `p95`, `max`,
`mean`, `std`) and `seeing` (below), or `null`. A score that was not finite is
written as 0.

### `seeing-time.csv` and `seeing` in the JSON

One row per frame the seeing probe followed (up to 120, spread evenly over the
capture): `frame`, `rms_px` (the differential motion of that frame, pixels per axis,
on the scale of the index the text report prints) and `window_rms_px` (the RMS of
the nine samples centred on it). The JSON's `seeing` object adds `half_window` (4),
`overall_rms_px` (the whole-capture index), `arcsec_per_px`, `r0_500_cm` and `grade`
when they were measured. How the index is made is in
[Seeing estimation](seeing.md).

### `ap-table.csv`

One row per alignment point of the grid (row-major), `ap-data.json` the same with the
grid's settings, and `ap-reference.png` the reference frame the plots are drawn over
(an 8-bit grey picture, stretched, at most 640 pixels on a side).

| Column | Meaning |
|---|---|
| `index`, `cx`, `cy` | Point number and window centre in pixels of the reference. |
| `active` | 1 when the window held enough detail to be measured. |
| `contrast` | Contrast of the reference window. |
| `measured`, `lock_fraction` | Frames in which the point was measured, and their share of the registered frames. |
| `quality_mean` | Mean sharpness of the window over the frames it was measured in. |
| `quality_kept` | Mean sharpness over the frames this point stacks (its own best ones). |
| `warp_dx_px`, `warp_dy_px` | Mean displacement of the point from the frame's global shift: the static part. |
| `warp_rms_px` | RMS scatter of that displacement about its mean: the part that changes from frame to frame. |
| `residual_dx_px`, `residual_dy_px`, `residual_rms_px` | Mean and RMS difference between the shifts used for the stack and the shifts measured against the finished stack; blank without `--ap-residual`. |

The JSON adds `window`, `step` (the distance between neighbouring centres), `frames`
(registered), `keep`, `passes`, `smoothed`, `residual_rms_px`, `moved_rms_px` and the
fitted `disc`. Points are measured on the registered (candidate) frames only.

### Image science files

`image-science.json` holds everything the image science plots are drawn from, and the
CSVs the same as tables:

| File | Columns |
|---|---|
| `spectrum.csv` | `image` (`stack`, `best frame`, `sharpened`), `freq_cycles_per_px`, `power`: the radially averaged power of the disc-centre square after it is divided by its own blur, so images are comparable. |
| `limb-profile.csv` | `mu`, `measured` (median of the ring relative to the disc centre, sky taken off), `model` (the fitted law), `residual`. |
| `limb-psf.csv` | `offset_px` from the fitted edge (negative on the disc), `measured`, `model`. |
| `histogram.csv` | `image` (`stack`, `sharpened image`, `colour stack` or `raw capture`), `channel`, `bin_low`, `bin_high`, `count`. |
| `noise.csv` | `image`, `signal` (median level), `noise_sigma`, `snr`. |

In the JSON, `spectrum` adds the granulation peak and roll-off, the diffraction cut-off
(when the aperture, the wavelength and the image scale are known), the scale in arcseconds
per pixel and the square used; `psf` gives sigma and FWHM in pixels and arcseconds and
the misfit; each histogram gives the share at the top level (`high_clip`, the lit pixels on
the plateau at the 99.5th percentile, counted as the clipped-plane check of
[the troubleshooting page](troubleshooting.md) counts them) and at zero (`low_clip`).
A plane whose top is below 30% of the range holds no signal and is not reported as clipped.

What is compared, and how it is measured:

* The **best frame** is read from the capture as the pipeline reads it (the colour plane
  that was stacked, burned-in text flattened) and not aligned; spectra do not care. The
  **sharpened** image is the image after `--sharpen` and the other edits, when the output
  is a TIFF or PNG that was edited (or a FITS with `--sharpen-fits`); otherwise it is not
  drawn. The app has the stack and the best frame only.
* The spectrum is that of [the granulation report](quality-report.md): the 240 by 240
  pixel square at the disc centre, divided by its own blur (an eighth of its side),
  Hann-windowed, averaged in rings. The diffraction cut-off `D / lambda` needs
  `--aperture-mm`, a wavelength (the filter's, or `--wavelength-nm`) and the image scale
  from the fitted limb.
* The **transfer curves** are `sqrt(P_a / P_b)` of two spectra. They are not the
  telescope's MTF: they compare two images of the same capture.
* The **noise** is Immerkaer's estimate (the median absolute deviation of the response to
  a 3 by 3 high-pass mask, divided by 6) over the same square.
* The **limb profile and PSF** are those of `helia measure --limb` and `--psf`.

### Solar specifics files

| File | What |
|---|---|
| `heliographic-grid.csv` | One row per sample point of every graticule line: `system` (`stonyhurst` or `carrington`), `line` (its label: `30N`, `CM`, `15W`, `345`), `value_deg`, `x_px`, `y_px` in pixels of the stack. |
| `solar.json` | The picture's `width` and `height`, the fitted `disc` (`cx`, `cy`, `r`), the capture time `when` (ISO 8601 UTC), the `orientation` (`p_deg`, `b0_deg`, `l0_deg`, `north_angle_deg`, `mirror`, `celestial_north_up`) and the sunspot `groups` (`id`, `spots`, `lat_deg`, `cmd_deg`, `lon_deg`, `area_msh`) and `wolf` with `--sunspots`. |
| `solar-reference.png` | The stack the grids are drawn on (8-bit grey, stretched, at most 900 pixels on a side). |
| `sunspots.csv`, `sunspots.json`, `sunspots.png` | With `--sunspots`: the spot table, the whole sunspot report and the overlay, as `helia stack --sunspots` writes them beside the output. |
| `timelapse-frames.csv`, `timelapse.json` | The registration table of a time-lapse: `frame`, `time_utc`, `shift_x_px`, `shift_y_px`, `method`, `limb_confidence`, `phase_psr`, `level`, `gain`, `derotated_px`. |

How the grid is made: the Sun's orientation at the capture time comes from the series of
Meeus (*Astronomical Algorithms*, chapter 29): `P` the position angle of the axis, `B0` the
heliographic latitude of the disc centre and `L0` its Carrington longitude. A point of
latitude `phi` and longitude `lambda` from the central meridian is on the visible
hemisphere when `z = sin phi sin B0 + cos phi cos lambda cos B0` is positive, and falls at
`x = cos phi sin lambda`, `y = sin phi cos B0 - cos phi cos lambda sin B0` (in solar radii,
west and north) which is turned by the angle of solar north in the picture (0 for solar
north up, `P` for celestial north up, plus `--north-angle`), mirrored for `--mirror`, and
scaled and centred on the fitted disc. Carrington meridians are the multiples of 15
degrees of `L0 + lambda`.

`--date TIME` (UTC, `YYYY-MM-DDThh:mm:ss`; `date` in an API job) gives the time of a
capture that has none of its own. The orientation options are the ones of `--sunspots`:
`--celestial-north-up`, `--north-angle` and `--mirror`. The app uses the capture's time
stamps and the *celestial north is up* tick of the Sunspots chips.

For a time-lapse, `helia timelapse --report DIR` (or `"report": true` in
`POST /timelapse`, which writes `OUTPUT/report/` and returns `report`, and **Save
diagnostics** in the Time-lapse step) writes the three diagnostics from the registration
the series made; the time axis is minutes since the first frame when every frame has a
time, else the frame number.

### `report.html`

One file, no network use. It carries the figures as inline PNG, a caption for each
saying how to read it, the summary tables, the settings the stack ran with (the keep
share, the metric, the alignment point settings, the global alignment method and
confidence, the stack mode, the colour plane and any calibration), and, when the
output is FITS, `fits-header.txt`: the cards of the primary header.

## In the app

Once a stack has finished, the Stack step shows a **Science** section under the result
lines.

| Control | What it does |
|---|---|
| **Show plots** | Draws the report's figures (the first time only, a fraction of a second) and shows one in the viewer in place of the stack. Press again to go back to the stack. |
| Figure chips | Choose the figure. The caption under them says what it shows and how to read it. |
| **Save report** | Asks for a folder and writes the whole report into a folder named `CAPTURE-science` inside it. |

The aperture under *Advanced settings* gives `r0` and a grade in the seeing figure.
**Residual pass**, above the buttons, adds the extra registration pass that the residual
figure needs on the next stack (off by default: it adds about 0.3 s to a 2.5 s stack of
the 1000-frame test capture).

## Through the API

The sharpened curves need a job with edits that apply to its output. Add `"report": true`
to the job's `config` (it also turns the seeing probe on), and
`"ap_residual": true` for the residual figure (needs `multipoint`). On a finished job:

* `summary.report.files` lists the file names, and `summary.report.not_drawn` says
  which figures were not drawn and why;
* `GET /api/v1/jobs/{id}/report` returns `report.html`;
* `GET /api/v1/jobs/{id}/report/{file}` returns one file with its content type
  (`image/png`, `text/csv`, `application/json`, `text/plain`).

A job that did not ask for a report answers `404 report_missing`; an unknown file
`404 report_file_missing`; a job that has not succeeded `409 not_ready`. The files are
kept in memory with the job, like the preview, and go when the job is forgotten:
fetch them in time, or write them out with the CLI.

## Redrawing in Python

```python
import json
import pandas as pd

q = pd.read_csv("report/frame-quality.csv")           # one row per frame
meta = json.load(open("report/frame-quality.json"))   # threshold, drift, seeing
threshold = meta["threshold"]
```

## Limits

* The quality score is relative to one capture: do not compare scores between
  captures, or between metrics.
* Shifts, and the alignment point figures, are measured for the candidate frames only,
  not for every frame.
* The seeing figure follows up to 120 frames spread over the capture, not every
  frame, and needs a disc with at least two tiles of 64 pixels that carry detail.
* A report is made after the stack, from what the stack measured; it adds no
  processing to the image.
