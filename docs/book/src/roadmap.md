# Roadmap

This is the plan, in the order the project intends to build it. It follows the
milestones proposed in the
[October 2026 state of the art review](developer/research/state-of-the-art-2026.md),
and each item links to its issue, where the discussion and the current status
live. Nothing below is available until its issue is closed and this page says
so; dates are deliberately not given. Issue titles are as of this release.

The overall plan and every decision so far are in
the project plan. This
documentation is tracked in
the follow-up plan.

## M1: the stacking pipeline (built)

SER, AVI and FITS input (and MP4, MOV, MKV and WebM through the optional
ffmpeg), bilinear and Malvar-He-Cutler demosaicing, frame and
per-point quality, phase-correlation global alignment, an alignment point grid,
quality-weighted stacking with optional sigma clipping, TIFF and FITS output,
the command line, the desktop app and the API. Everything in the User guide
describes this milestone. Since the first release it also has guided steps, work
notices and colour presets for the solar lines (the colour part of the prominence and disc compositing work; see
[Colourising](user-guide/colourising.md)).

## M2: trustworthy stacking inputs and outputs

Fixes the largest known solar failure modes and makes the files carry the
metadata they should.

| Issue | Title |
|---|---|
| #3 | Dark, flat and hot-pixel calibration (built) |
| #4 | Two-pass alignment-point refinement and shift-field smoothing |
| #16 | Real-data bugs: APs on timestamp and sky, quality vs rank mismatch (built) |
| #18 | Limb-fit global alignment with confidence (built) |
| #19 | SOLARNET-compliant FITS output (built) |

## M3: look and measure

A complete workflow, and the evidence for choosing the default quality metric.

| Issue | Title |
|---|---|
| #5 | Sharpening and adjustments: unsharp mask, a-trous wavelets, Richardson-Lucy, levels, curves, crop, rotate, resize (built: see [Sharpening and adjustments](user-guide/sharpening.md)) |
| #20 | Radial limb-darkening flattening |
| #21 | Quality-metric bake-off and default by seeing |
| #22 | Spike: wgsl-fft for GPU phase correlation |

## M4: solar presentation

Features that users of other solar tools already expect.

| Issue | Title |
|---|---|
| #23 | Prominence/disc HDR compositing and colour LUT export (the colour LUT part is built: see [Colourising](user-guide/colourising.md); HDR compositing is not) |
| #24 | Alt-az field-rotation correction from SER timestamps |
| #25 | Autocrop and automatic AP parameters |

## M5: geometry and sampling

| Issue | Title |
|---|---|
| #6 | Drizzle via the splat accumulator |
| #26 | Mosaics, solar derotation and time-lapse |

## M6: research track

All experimental, and labelled that way when it arrives.

| Issue | Title |
|---|---|
| #7 | Experimental MFBD restoration |
| #8 | ML frame-quality scorer (ONNX Runtime; Coral experimental) |
