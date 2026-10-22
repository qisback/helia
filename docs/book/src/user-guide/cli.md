---
description: "The Helia command line: stack, sharpen, colour, calibrate, measure and batch-process solar captures from scripts."
---

# The command line

`helia` has these subcommands:

| Command | What it does |
|---|---|
| `helia stack` | Stack a capture into one image. |
| `helia info` | Print what a capture file contains, without stacking. |
| `helia measure` | Measure the granulation contrast of a finished image. |
| `helia presets` | List the filter presets and what each does. |
| `helia serve` | Run the [HTTP API](api.md). |
| `helia feedback` | Send a bug report, feature request or question, to the Helia issue tracker. See [Sending feedback](sending-feedback.md). |

Every option is listed in the [CLI reference](../reference/cli.md), which is
generated from the program itself. This page explains how to use them.
`helia --help` and `helia <command> --help` print the same information.

## `helia info`

```sh
helia info capture.ser
```

The output has up to four parts. This is `Sun_Test.ser`, an ASI482MC colour
camera behind an H-alpha filter, recorded by SharpCap as a file that says it is
mono:

```text
file:               Sun_Test.ser
format:             SER

Header, as written in the file
  FileID:            LUCAM-RECORDER
  LuID:              0
  ColorID:           0 (MONO)
  LittleEndian:      0 (big-endian)
  Width:             1920 px
  Height:            1080 px
  PixelDepth:        8 bit
  FrameCount:        631
  Observer:          (empty)
  Instrument:        (empty)
  Telescope:         (empty)
  DateTime (local):  not set
  DateTimeUTC:       not set
  Timestamps:        absent

Helia will use
  Bayer pattern:     RGGB           (detection)
  Bit depth:         8 bit          (header)
  Byte order:        not used (8-bit) (header)
  Row order:         top to bottom  (header)

Mosaic check (2x2 pixel positions, share of full scale)
  means:             79.6%  77.6%  77.4%  0.3%
  spread:            135.0% over 5 frame(s)

warning:  The SER header says mono (ColorID 0), but the frames hold a Bayer mosaic ...
```

1. **Header, as written in the file.** Every field of a SER header, decoded:
   FileID, LuID, ColorID with its meaning, the LittleEndian flag, size, pixel
   depth, frame count, observer, instrument, telescope, DateTime and DateTimeUTC
   as dates (`not set` when zero), and whether per-frame timestamps are present,
   with the first and the last. For an AVI it prints the codec, pixel format,
   size, row order and frame count, then what the SharpCap `CameraSettings.txt`
   next to it says (its path, the camera, the colour space and the Debayer
   Preview).
2. **Helia will use.** The Bayer pattern, bit depth, byte order and row order the
   capture will be decoded with, each with where it came from: `header`,
   `sidecar`, `detection` (measured from the pixels) or `override` (an option you
   gave).
3. **Mosaic check.** Only for a SER file that says mono: the mean level of the
   four pixel positions of a 2 x 2 cell on the solar disc and their spread (see
   [Troubleshooting](troubleshooting.md#a-grid-or-chessboard-in-the-stack-a-bayer-mosaic-that-was-not-removed)).
4. **Notices.** `warning:` when the file disagrees with itself or with you and
   Helia has chosen, `note:` for anything else worth knowing. `helia stack`
   prints the same notices.

`helia info` accepts the same `--bayer`, `--bit-depth`, `--endian` and
`--flip-rows` as `helia stack`, to show what an override would change before you
stack.

For an MP4, MOV, M4V, MKV or WebM file, or an AVI with a compressed codec, it
asks `ffprobe` (part of the optional [ffmpeg](installation.md#optional-ffmpeg-for-mp4-and-other-video))
and prints the container, codec and profile, the frame size (after any rotation
the file asks for), the pixel format and bit depth, the frame count, frame rate
and duration, and, for lossy video, a `note:` line such as "lossy 8-bit 4:2:0
video (h264): fine for quick results, SER/AVI raw captures keep more detail".
Nothing is decoded, so it is quick even for a large file.

Run it first on a new kind of capture. Most problems with colour data show up
here. When the **byte order** under *Helia will use* differs from the
`LittleEndian` flag under the header, Helia has corrected a mislabelled file (see
[Troubleshooting](troubleshooting.md)).

## `helia measure`

```sh
helia measure sun.tif
```

Prints the band-pass contrast (standard deviation over mean) of a 240 pixel crop
at the centre of the disc, in three bands: fine (0 to 1 px), granulation (1 to
4 px) and meso (4 to 12 px). It reads a TIFF, PNG or FITS file; a colour image is
averaged to grey. `--crop PX` changes the crop (default 240). `--centre X,Y` centres it on a pixel instead of on the centroid of the bright disc; the centroid moves with prominences, so to compare several stacks of one capture give each the fitted disc centre from its `disc:` line (see [Choosing a quality metric](choosing-a-quality-metric.md)). Use it to compare settings on one
capture; see [White-light granulation](../recipes/white-light-granulation.md#measuring-granulation).

`helia measure sun.fits --limb` fits the three limb-darkening laws (linear,
quadratic, Neckel and Labs) to the disc instead, and prints the coefficients, the
limb level and the measured profile beside each law. See
[Limb darkening](sharpening.md#limb-darkening).
It then prints the granulation report of
[the granulation and seeing report](quality-report.md): RMS contrast, cell size
and the power spectrum's roll-off. `--scale ARCSEC` gives sizes in arcseconds;
`--aperture MM` (with `--wavelength NM` and `--scale`) adds the diffraction
cut-off. `helia stack` prints the same report for a white-light or sodium
stack, and `--aperture-mm MM` adds the cut-off.

## `helia calibrate`

Builds master calibration files from calibration captures, for
`helia stack --dark --flat --bad-pixels`. Each input is a SER, AVI or video file, a
FITS still, or a folder of them. See [Calibration](calibration.md) for what to
capture and why.

```sh
helia calibrate --darks darks.ser --flats flats.ser --flat-darks flat-darks.ser \
                --preset ha -o masters/
# wrote:    masters/master-dark.fits  (master dark of 120 frame(s), sigma-clip 3, ...)
# wrote:    masters/master-flat.fits  (master flat of 40 frame(s), ...)
# wrote:    masters/bad-pixels.fits  (37 pixel(s) (31 hot, 6 cold), from the dark and flat)
# next:     helia stack <capture> -o out.tif --dark masters/master-dark.fits ...
```

* `--darks`, `--flats`, `--flat-darks`, `--bias`: the inputs (each takes several
  paths). `--flat-darks` are darks at the exposure of the flats; without them,
  or `--bias`, the flat keeps its offset and the command warns.
* `--output-dir DIR` (`-o`): where the files go, `calibration` by default:
  `master-dark.fits`, `master-flat.fits`, `master-bias.fits`,
  `master-flat-dark.fits` and `bad-pixels.fits`, whichever were asked for.
* `--combine median|sigma|sigma:K`: how frames are combined, per pixel. The
  default `sigma` rejects values more than 3 standard deviations out and
  averages the rest. `--max-frames N` limits the frames used; by default as many
  as fit in 3 GiB.
* `--lights CAPTURE`: also look for stuck pixels in a light capture: pixels that
  read the same wrong value in every frame while the Sun moves. Useful when there
  are no darks. `--no-bad-pixels` skips the search, `--hot-sigma K` changes how far
  from its neighbours a hot pixel of the dark must be (default 6).
* `--displaced PATH...`: captures of the Sun at many places on the sensor (the
  mount off tracking, or nudged), to solve the flat from instead of `--flats`
  (they conflict). Needs 6 frames or more and 30 pixels or more between the
  furthest; uses up to 60 spread over the input (`--max-frames`). The darks are
  taken off. By default only the structure finer than a quarter of the
  displacements is kept (dust, fringes, the column pattern); `--displaced-keep-smooth`
  keeps every scale. The `selfcal:` line gives the misfit, how much of the sensor
  the Sun covered and the scale above which the gain is not recovered well. See
  [A flat from the Sun itself](calibration.md#a-flat-from-the-sun-itself-displaced-disc-frames).
* `--preset ha`: adds the advice for that filter to the warnings.
* The same `--bayer`, `--bit-depth`, `--endian` and `--flip-rows` as `helia stack`.

The flat is checked as it is made: the command prints how far the edge of the
field falls below the centre, the size of any rings and how much of the field
dust shadows, and warns about strong vignetting (an etalon's sweet spot), rings
(Newton rings and etalon fringes) and dust. It exits with `0` when it wrote the
files, whatever it warned about.

## `helia stack`

The minimum is an input and an output:

```sh
helia stack capture.ser -o stack.tif
```

The output extension chooses the format: `.tif` or `.tiff` for a 16-bit TIFF,
`.png` for an 8-bit PNG, `.fits` or `.fit` for a 32-bit floating-point FITS.
Anything else is refused before any work starts. An existing output file is overwritten without asking.
See [Output formats](../reference/output-formats.md).

### What Helia decides for you

With nothing but an input and an output, `helia stack` already does the right
thing for most captures. It reads the filter from the file name, the folder, the
SharpCap sidecar or the SER header (see
[Colourising](colourising.md#how-the-filter-is-recognised)) and prints what it
chose on the `settings:` line:

* the **colour channel** of a raw colour camera (red for H-alpha, blue for
  calcium, green for magnesium, all three for sodium, helium and white light);
* the **share of frames** to keep (25 per cent, or 30 for the faint lines);
* the **alignment point size**, about a sixteenth of the short side of the
  image, in steps of 8 from 32 to 96 pixels.

Any option you give wins over the recommendation. If the filter is not
recognised, the defaults are all channels combined and 25 per cent.

`--preset ha|cak|cah|mg|na|he-d3|hbeta|white` names the filter yourself: it
applies the same recommendations and also colours a `.tif` or `.png` output.
`helia presets` lists them. The colour options are in
[Colourising](colourising.md#on-the-command-line).

### Choosing the frames

* `--keep 25%` (the default) keeps a share of the frames, ranked by quality.
  A bare number such as `--keep 500` is a count.
* `--keep-frames 500` is the same as a count and cannot be combined with
  `--keep`.
* `--metric gradient` (the default), `laplacian`, `helmli`, `mfgs`, `dct`,
  `fourier` or `contrast` chooses how sharpness is measured. All ignore overall
  brightness. Only the first two run on the GPU; the rest score on the CPU.
  On the real captures tried, gradient was as good as any for H-alpha and Ca K;
  see [Choosing a quality metric](choosing-a-quality-metric.md).

Fewer frames give a sharper but noisier stack. Start at 25 per cent and look at
the *Quality, sorted* curve in the [desktop app](gui.md) to see where your
capture drops off.

### Alignment

* `--ap-size 64` is the size, in pixels, of each alignment point. The default is
  chosen from the image size (see above).
  Smaller points follow finer detail but need more contrast to lock on; the
  smallest allowed size is 8 (the API also caps it at 1024).
* `--ap-auto` chooses the alignment point size, spacing and search range
  instead of a fixed size (it excludes `--ap-size`). Helia fits the solar limb
  of the reference for the image scale, then registers a dozen of the frames it
  is about to stack and measures how far each point strays from its frame's
  median shift (the local warp of the seeing). The window follows the detail in
  arcseconds (2.5 granules for white light and sodium, 90 arcsec otherwise),
  clamped to 32 to 96 pixels, with the usual half-window spacing; the search
  radius covers three times the warp (4 to 12 pixels). With no limb the size is
  the one recommended for the frame. An `auto-ap:` line says what was chosen:
  `alignment points 32 px every 16 px, search 5 px (2.16 arcsec per pixel; local
  warp 0.88 px (1.89 arcsec))`. Off by default, so existing runs do not change.
* `--ap-passes 2` stacks once and measures the alignment points again against
  that stack, which has less noise than the reference; the default 1 measures
  them once. `--ap-smooth on` replaces shifts that disagree with their
  neighbours by the neighbours' median. `--ap-residual` measures the points a
  last time against the finished stack and prints how far the shifts used are
  from that (`refine:` line). All need multipoint and are off by default; see
  [Refining the shifts](how-stacking-works.md#refining-the-shifts).
* `--seeing` estimates the seeing from the capture after the stack: the Fried
  parameter `r0` from the spectral ratio of tiles of the disc (it needs
  `--aperture-mm`, and `--wavelength-nm` when the filter is not known) and a
  relative motion index from how much neighbouring alignment points move against
  each other, over 120 frames spread across the capture. See
  [Seeing estimation](seeing.md).
* `--field-rotation` undoes the field rotation of an altazimuth mount: every frame
  is turned to the orientation of the middle of the capture from the parallactic
  angle of the Sun at its SER timestamp, for the `--site` (or `HELIA_SITE`), before it
  is scored, aligned or stacked. `--field-rotation-reference first|last` and
  `--field-rotation-mirror` adjust it. `helia derotate` does the same for a time-lapse
  of stacked images. See [Field rotation](field-rotation.md).
* `--sunspots` finds the sunspots on the stack (white light, whole disc): groups,
  the Wolf number, areas corrected for foreshortening and positions. It prints the
  report after the stack, writes `NAME-sunspots.csv`, `.json` and `.png` beside the
  output and adds `SPOT*` cards to a FITS output. `--sunspot-umbra`,
  `--sunspot-penumbra`, `--sunspot-min-area`, `--sunspot-max-area`,
  `--sunspot-group-gap` and `--wolf-k` tune it; `--north-angle`,
  `--celestial-north-up` and `--mirror` say how the image sits on the Sun. The same
  works on any stacked image with `helia sunspots`. See
  [Sunspots](sunspots.md).
* `--align auto|phase|limb` chooses how whole frames are aligned. `auto`, the
  default, fits the solar limb when the whole disc is in the frame and the fit is
  confident, and uses phase correlation otherwise (a cropped region, a disc cut
  by the frame, no disc). `phase` and `limb` force one; with `limb`, a frame
  whose limb cannot be fitted still falls back to phase correlation, and a
  capture with no disc at all is an error. The end of a run prints an `align:`
  line with the choice, the reason and the confidence. See
  [How stacking works](how-stacking-works.md#3-aligning-the-whole-frame).
* `--no-multipoint` aligns whole frames only. It is faster, and a useful check:
  if the multi-point result looks worse than the global one, the points are
  locking on the wrong thing.

### Prominences

`--prominence` (with `helia stack` and `helia edit`) composites a stretched
off-limb layer with the disc after the sharpening: the glow outside the limb is
taken off, what is left is stretched and joined to the disc through a feathered
edge from the limb fit. `--prominence-strength`, `--prominence-stretch`,
`--prominence-level`, `--prominence-feather`, `--prominence-reach` and
`--prominence-gate` tune it. See [Prominences and the disc
together](prominences.md).

### Colour tables

`helia lut <preset> -o FILE` writes a filter's colour look as a `.cube`, `.csv`,
ImageJ `.lut` or PNG strip, for other programs; `--format`, `--size`, `--invert`
and `--strength` choose how. See [Colourising](colourising.md#exporting-a-colour-table).

### Limb darkening

Every run fits the limb darkening of the stack and prints it, for example
`limb:    quadratic: u1 = 0.472, u2 = 0.046; limb at 48% of the centre; rings differ
from the law by 0.66% rms`. `--limb-law linear|quadratic|neckel` chooses the law
(default quadratic). `--flatten model|profile` also divides the disc by the fitted
law or by the measured profile, before the sharpening, in every output but FITS
(add `--sharpen-fits`). `helia edit` takes the same flags. See
[Limb darkening](sharpening.md#limb-darkening).

### Cropping to the disc

* `--autocrop` cuts the result to a square around the fitted solar disc, in
  every output format (a FITS file records the step and keeps the limb in its
  own pixels). `--autocrop-margin 10` is the room left beyond the limb as a
  percentage of the radius (default 10, 0 to 200); prominences seldom reach 5%.
  A disc near the frame edge moves the square inwards; a disc larger than the
  frame keeps the whole of that side. With no clear disc (a cropped region, a
  prominence frame) the image is left whole and a `crop:` line says why.

### Keeping alignment off text and sky

Helia finds a burned-in timestamp by itself, and places alignment points only
where the centre of the point is inside the fitted limb of the sun. These
options adjust that; the background is in
[How stacking works](how-stacking-works.md#keeping-alignment-off-text-and-sky).

* `--exclude x,y,w,h` keeps alignment and scoring off a rectangle, in pixels
  from the top-left corner. Repeat it for several. The area is flat in the
  result. Use it for anything the automatic detection does not cover.
* `--prominence-band 20` allows points up to 20 pixels outside the limb, so that
  they can follow prominences. The default is 0.
* `--no-disc-mask` and `--no-overlay-detect` switch the limb fit and the text
  detection off. The summary prints `masked:` and `disc:` lines for what was
  found.

### Calibration

* `--dark PATH` subtracts a master dark: a FITS file from `helia calibrate` (or any
  FITS or TIFF), or dark captures to build one from on the spot (a SER, AVI or FITS
  file, or a folder).
* `--flat PATH` divides by a master flat, or builds one from flat captures. With
  captures, `--flat-dark PATH` gives the darks at the flats' exposure.
  `--displaced PATH` solves the flat from captures of the Sun at many places on
  the sensor instead (it conflicts with `--flat`; up to 60 frames are used).
* `--bad-pixels FILE` replaces the pixels of a map (`bad-pixels.fits`, or a text file
  with an `x y` pair per line, from 0 at the top left) with the median of their
  neighbours. Without it, when there is a dark or a flat, the hot pixels of the dark
  and the dead ones of the flat are found and replaced; `--no-cosmetic` stops that.
* `--scale-dark` scales the dark by the ratio of the exposures (the Sun's comes from
  the SharpCap file beside the capture). `--bias PATH` gives it a master bias to scale
  around. `--calibration-combine` is `--combine` of `helia calibrate`, for masters
  built on the spot.

The masters are checked against the capture before any work starts: a master
from another camera or region (a different size or mosaic) is refused with a
message. The calibration is applied to every frame as it is read, before it is
demosaiced, scored or aligned, so the whole run sees calibrated frames. The
`calibrate:` lines at the start of the run name each master, and the output records
them (`PRSTEP` cards and `HISTORY` in FITS, the description in TIFF and PNG).
See [Calibration](calibration.md).

### Stacking

* The default is a quality-weighted mean.
* `--robust sigma|huber|winsor` guards against outlier samples (hot pixels,
  satellite and plane trails, a cloud dimming a few frames): `sigma` drops
  samples further than K standard deviations from the mean, `huber` gives them
  less weight the further they are, `winsor` clamps them to K standard
  deviations. `--robust-k K` sets K (defaults 2.5, 1.345 and 2). All three read
  the capture twice and are off by default (`--robust none`). They remove what
  differs between frames; a defect fixed on the sensor is not an outlier and
  needs `--dark` or `--bad-pixels` (the command prints a note saying so). The measured
  behaviour is in [How stacking works](how-stacking-works.md#5-stacking).
* `--sigma-clip 2.5` is the older spelling of `--robust sigma --robust-k 2.5`.

### Video files

`helia stack` takes an MP4, MOV, M4V, MKV or WebM file, or a compressed AVI, in
place of a SER file, if the optional `ffmpeg` is installed:

```sh
helia stack sun.mp4 -o stack.tif
```

The video is decoded once into an uncompressed SER file in a cache directory,
and the stack runs from that, so it is as fast as a SER capture after the
decode. The file is deleted when the run ends. `--cache-dir DIR` (or the
`HELIA_CACHE_DIR` environment variable) picks the directory; the default is the
operating system's cache folder. Helia checks there is room first: a 4K clip
of 7,500 frames needs about 60 GB. `--channel r|g|b` makes `ffmpeg` extract that
colour plane while decoding (`lum`, the default, is the luminance). The summary
gains a `note:` line when the video is lossy. `HELIA_FFMPEG` and `HELIA_FFPROBE`
name the programs if they are not on your `PATH`.

### Colour cameras

* `--bayer auto|none|rggb|bggr|grbg|gbrg` says how to treat raw colour data.
  `auto` (the default) trusts the file's header, or the SharpCap sidecar for an
  AVI; but for a SER file that says it is mono it also checks the pixels for a
  hidden Bayer mosaic and debayers when it finds one, saying so. `none` forces
  mono whatever the header says; a pattern name forces that pattern. Both skip
  the check of the pixels. For `rggb` and the others, name the pattern of the
  picture as it will be stacked (after `--flip-rows`, if you give it).

* `--colour true` stacks the red, green and blue planes of a raw colour capture
  on one alignment and writes a colour picture (RGB TIFF or PNG, or a three-plane
  FITS cube): true colour for white light. `--white-balance auto|off|R,G,B`
  (default `auto`, the disc centre made neutral) and `--no-channel-align` (do not
  register red and blue to green) belong to it, and `--channel`, `--invert` and
  `--colour-strength` do not apply. It is refused for a capture that is not a
  Bayer mosaic. See [True colour](colourising.md#true-colour).

### Reading a file whose header is wrong

For a SER file, to say what the header gets wrong (see
[Capturing for Helia](capturing.md#why-a-ser-header-can-be-wrong)):

* `--bit-depth N` uses N bits per sample instead of the header's. 1 to 8 read
  one byte per pixel, 9 to 16 two; samples are scaled to the range of their
  container, as for the header's own depth.
* `--endian little|big` sets the byte order of 16-bit samples instead of the
  header flag and Helia's check of it.
* `--flip-rows` reads the rows bottom to top.

An AVI describes its own samples, so these three are refused for an AVI.
Whatever you give is printed as a `note:` and recorded in the output file, as the
`Overrides:` line (see [What the files say](colourising.md#what-the-files-say)).
* `--debayer mhc` (the default) or `bilinear` chooses the demosaicing method.
* `--channel lum|r|g|b` chooses which plane is analysed and stacked. The
  default is the preset's channel (see above), or `lum`, the mean of the three,
  when there is no preset. For H-alpha that is `r`, as explained in
  [Troubleshooting](troubleshooting.md).

### Backend

`--backend auto|cpu|gpu`; see [GPU acceleration](gpu.md).

### Colour

* `--preset <filter>` colours the output (see above).
* `--colour auto|mono|<filter>` chooses only the colour: `auto` is the preset
  you gave or the filter Helia detected, `mono` is grey.
* `--invert` makes a negative before colouring; `--colour-strength 0.6` mixes
  60 per cent colour with grey.

Colour goes to `.tif` (16-bit RGB) and `.png` (8-bit RGB). A `.fits` output is
never coloured. Every file notes the look used.

A `.fits` output carries the SOLARNET observation keywords: capture times,
exposure, telescope and observer from the SER header, the filter and its
wavelength, the processing steps, and a helioprojective coordinate system from
the limb fit. `--site LAT,LON,ALT` (or the `HELIA_SITE` environment variable)
adds the observing site as `OBSGEO-X/Y/Z`. Every keyword is listed in
[Output formats](../reference/output-formats.md#fits).

### Sharpening and adjustments

* `--sharpen wavelet|unsharp|rl|none` sharpens the stack with the recommended
  values of the filter. The default is `none`. It runs on the mono stack, before
  any colour.
* `--wavelets registax:l1=AMOUNT/SHARPEN/DENOISE,...` (the layers, scheme, filter and
  linked mode as in RegiStax 6; see
  [Coming from RegiStax](coming-from-registax.md)), `--wavelet-gains`,
  `--wavelet-noise` (lists of the gain and noise cut, finest layer first),
  `--unsharp-radius`, `--unsharp-amount`, `--unsharp-threshold`,
  `--rl-iterations`, `--rl-tv`, `--rl-psf` (`auto`, `gaussian:SIGMA`,
  `airy:RADIUS` or `airy-scope:NM,MM,ARCSEC_PER_PX`) and `--rl-guard` override
  individual values; each needs its method.
* `--denoise [STRENGTH]`, `--denoise-layers`, `--black`, `--white`, `--gamma`,
  `--curve IN:OUT,...`, `--crop X,Y,W,H`, `--rotate DEG`, `--flip-h`, `--flip-v`
  and `--resize 50%` (or `1200x800`) are the everyday adjustments.
* `--edits NAME|FILE` starts from a saved preset; `--save-edits NAME` keeps the
  list as one.
* `--sharpen-fits` applies the edits to a FITS output too. By default a FITS file
  is the unsharpened stack.

The edits run in this order: those of `--edits`, denoise, the sharpening method,
levels, the curve, crop, rotate, flip, resize. The summary says how much finer
detail they gave the stack and what they did to the noise. See
[Sharpening and adjustments](sharpening.md) for what each does and the risks.

### Output and exit status

The result goes to the file named by `-o` (long form `--output`); the extension
chooses the format: `.tif` or `.tiff`, `.png`, or `.fits` or `.fts`.

While it works, `helia stack` prints the settings it chose, a line as each stage
starts with the time since the start (`[   0.3s] stacking`), a progress bar
and a summary. All of it goes to standard error, so the output file is the only
thing on disk and you can redirect the messages. `-q` or `--quiet` hides all of
it.
The exit status is 0 on success; on any error `helia` prints `helia: <message>`
on standard error and exits with status 1.

## `helia timelapse`

Stacks several captures the same way, registers the stacks to each other and writes a
numbered PNG or TIFF sequence and a movie (an MP4 when `ffmpeg` is installed, else an
animated PNG):

```sh
helia timelapse 15_32_09.avi 15_37_19.avi -o tl --date 2026-10-20 --utc-offset 1 \
    --crop --deflicker match -- --preset ha
```

Everything after `--` is given to `helia stack` for every capture. `-o DIR` is a
directory, which holds `frame-0001.png` and so on, `timelapse.mp4` (or `timelapse.png`),
`timelapse.csv` and the stacks in `stacks/`. The registration (`--reference`,
`--no-refine`, `--max-refine`), the brightness (`--deflicker`, `--deflicker-window`), the
edges (`--crop`), the times (`--date`, `--utc-offset`, `--no-sort`) and the output
(`--format`, `--movie`, `--fps`, `--reuse-stacks`) are explained, with the method and a
table of every option, in [Time-lapse](timelapse.md). A worked example is the
[tutorial](../tutorials/timelapse.md).

## `helia mosaic`

Joins panels of the Sun into a full-disc mosaic: matches every pair of panels on its
overlap (shift and up to a few degrees of turn), places them all at once by least squares,
matches their brightness, joins them along seams of least difference and feathers the seams.
The panels are captures (stacked as for `helia timelapse`, with the settings after `--`) or
stacked images:

```sh
helia mosaic p1.tif p2.tif p3.tif p4.tif -o mosaic.tif
```

`NAME-panels.csv` and `NAME-pairs.csv` are written beside the output. `--max-rotation`,
`--min-overlap`, `--min-correlation` and `--feather` tune the matching and the blend,
`--no-gain-match` and `--no-seams` turn parts off, `--derotate` (with `--derotate-to`,
`--rotation-law`, `--north-angle`, `--celestial-north-up`, `--mirror` and `--raw-brightness`)
brings the panels to one time first, and `--date`, `--utc-offset` and `--reuse-stacks` are as
for the time-lapse. The method, every option, the tables and the checks that were made are in
[Mosaics](mosaic.md); a worked example is the [tutorial](../tutorials/mosaic.md).

## `helia solar-derotate`

Brings stacked images of the Sun to one time by its differential rotation, so that
features line up between pictures taken minutes or hours apart (not field rotation).
Each image is derotated on its own and not shifted:

```sh
helia solar-derotate 0001-15_32_09.tif 0002-15_37_19.tif -o dd \
    --date 2026-10-20 --utc-offset 1 --to 2026-10-20T19:37:19
```

`--to` is `first`, `middle` (the default), `last` or a UTC time; `--rotation-law` is
`howard` (default), `snodgrass` or `A,B,C`; `--north-angle`, `--celestial-north-up` and
`--mirror` say how the picture sits against the Sun, and `--raw-brightness` moves the
pixel values as they are. The time-lapse takes the same options with `--derotate`. The
method, every option and a worked example are in [Solar derotation](derotation.md) and the
[tutorial](../tutorials/derotation.md).

## `helia edit`

Sharpen and adjust an image that already exists: a TIFF, PNG or FITS, mono or
colour.

```sh
helia edit stack.fits -o sharp.tif --sharpen wavelet --preset ha
helia edit sun.tif -o small.png --crop 200,150,1200,900 --resize 50%
helia edit sun.tif -o sun2.tif --edits "H-alpha gentle" --gamma 1.1
```

It takes the same sharpening and adjustment options as `helia stack`, plus
`--preset <filter>` for the recommended values (the filter is otherwise read
from the file name or folder). A colour image is edited channel by channel and
stays colour. The output records what was done, with where it came from. With
nothing to do it says so and exits with status 1.

## `helia serve`

Starts the API server. The options are in the [CLI reference](../reference/cli.md)
and the behaviour is in [The API](api.md). The first line it prints is
`listening on http://ADDRESS`, which scripts can read to learn the port when
they ask for port 0.

## `helia feedback`

Files a bug, a feature request or a question as an issue on the project's forge,
through the feedback service, for people who work without the desktop app:

```sh
helia feedback --kind bug --title "Stack fails on my AVI" --description "..." \
    --screenshot problem.png --system-info
```

`--dry-run` prints what would be sent and sends nothing; if the service cannot be
reached, `--save report.zip` keeps the report and the command prints the
pre-filled new-issue address. Your report is filed in the Helia issue tracker, and Helia sends
nothing you did not ask it to. [Sending feedback](sending-feedback.md) explains the
options and the privacy rules.

## Batch processing

Stack every SER in a folder, with a different output name for each. In a POSIX
shell:

```sh
for f in captures/*.ser; do
  helia stack "$f" -o "stacks/$(basename "$f" .ser).tif" --quiet || echo "failed: $f"
done
```

In PowerShell:

```powershell
Get-ChildItem captures\*.ser | ForEach-Object {
  helia stack $_.FullName -o "stacks\$($_.BaseName).tif" --quiet
  if ($LASTEXITCODE -ne 0) { Write-Warning "failed: $($_.Name)" }
}
```

Both stop a failing capture from hiding the others. For more control, queue
jobs through [the API](api.md) from a script.
