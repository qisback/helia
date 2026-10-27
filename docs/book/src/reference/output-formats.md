# Output formats

Helia reads captures and writes one stacked image. This page says exactly what
goes into and out of those files today.

## What Helia reads

| Format | Extensions | Notes |
|---|---|---|
| SER (version 3) | `.ser` | Mono, raw Bayer (RGGB, GRBG, GBRG, BGGR), RGB and BGR; 8 and 16 bit; per-frame timestamps are read when present. |
| AVI, uncompressed | `.avi` | `BI_RGB`, `Y800`/`GREY`, `Y16`, and 24-bit BGR; AVI 1.0 and OpenDML (over 1 GB); top-down and bottom-up. |

A SER header is read, not believed blindly: Helia checks a file that says it is
mono for a hidden Bayer mosaic and the byte-order flag against the data, and
`--bayer`, `--bit-depth`, `--endian` and `--flip-rows` override it (see
[Troubleshooting](../user-guide/troubleshooting.md)). What was decided is
recorded in every output file (below).

Samples are scaled to the range 0 to 1 by the size of their container (255 for
one byte, 65535 for two), not by the bit depth in the header, because cameras
differ on whether 12-bit data is stored at the low or high end of 16 bits.
Colour captures are reduced to one plane for stacking; see `--channel` and
`--debayer` in the [CLI reference](cli.md).

Helia also reads FITS images (`BITPIX` 8, 16 and -32, with `BZERO` and `BSCALE`,
and the `BAYERPAT` and `ROWORDER` keywords; Seestar S50 frames, which are stored
top row first without a `ROWORDER` card, are recognised from their header, and
`--row-order` overrides all of it). `helia stack` takes FITS frames as a
capture: a folder or pattern of single-frame files such as a Seestar's
`Light_*.fit`, a cube with one frame per plane, or a file with one image per
extension; see [Stack FITS frames](../tutorials/stack-fits-frames.md). `helia
edit` reads a TIFF (8, 16 or 32-bit float grey, 8 or 16-bit RGB), a PNG (8 or
16-bit grey or RGB) or a FITS file as the image to edit.

## What Helia writes

The output extension chooses the format, in the command line, the app and the
API alike.

| Extension | Format |
|---|---|
| `.tif`, `.tiff` | 16-bit TIFF, greyscale or RGB |
| `.png` | 8-bit PNG, greyscale or RGB |
| `.fits`, `.fit`, `.fts` | 32-bit floating-point FITS: mono, or a three-plane cube (red, green, blue) for a true-colour stack |

Any other extension is rejected before stacking starts. The case of the
extension does not matter.

The stacked image is normalised so that its 99.99th percentile is 1. Sharpening
and the other edits (see [Sharpening and adjustments](../user-guide/sharpening.md))
are applied to it next, and colour last; the result is then written.

A colour look is applied only to TIFF and PNG, and only when one is chosen
(`--preset` or `--colour`, `preset` or `colour` in an API job, or the Colour step
of the app). See [Colourising](../user-guide/colourising.md). A FITS file is
never coloured by a look and its pixels are never changed by one.

True colour (`--colour true`, `colour: "true"`) is different: the stack itself
is colour, made from the red, green and blue planes of a raw colour capture. It
goes to a 16-bit RGB TIFF, an 8-bit RGB PNG or a FITS cube of three planes (see
[True colour](../user-guide/colourising.md#true-colour)).

### FITS cube

A true-colour FITS file has `NAXIS` 3 and `NAXIS3` 3: plane 1 is red, plane 2
green and plane 3 blue, each 32-bit float, with `CTYPE3 = 'RGB'` and a comment
saying so. The header carries the same SOLARNET keywords as a mono file (the
limb is in the pixels of the cube) and a `PRSTEP` card each for the white
balance and the channel alignment. `helia edit` reads such a file as colour.

### TIFF

* One uncompressed image, 16 bits per sample: greyscale, or three samples per
  pixel (RGB) when coloured.
* Samples are clamped to 0 to 1 and scaled to 0 to 65535. Values that are not
  finite are written as 0.
* The `ImageDescription` tag always says how the capture was read: the Bayer
  pattern, bit depth and byte order with their sources, the SER header's own
  claim, the mosaic measurement when one was detected, and any overrides, in
  lines joined with `; ` (see
  [What the files say](../user-guide/colourising.md#what-the-files-say)). When
  edits were applied it then lists them (a heading, one line per operation, then
  the list as JSON), and when a preset or colour was chosen it ends with that (for
  example `Colour is presentation, not measurement: preset ha (H-alpha 656.3 nm,
  orange-red), invert off, strength 1.00`). The other tags are the structural
  ones that describe the image.

### PNG

* One 8-bit image: greyscale, or RGB when coloured. Samples are clamped to 0 to
  1 and scaled to 0 to 255, so it is for sharing; use TIFF to keep 16 bits.
* A `Description` text chunk carries the same lines as the TIFF
  `ImageDescription` (how the capture was read, the edits when applied, and the
  colour look when one was chosen), and a `Software` chunk says `Helia`.

### FITS

* A single image HDU, `BITPIX = -32` (IEEE single precision), two axes.
* Values are written as computed, without clamping, so a pixel can exceed 1.0 if
  the brightest few hundredths of a per cent of the stack do.
* Rows are written bottom row first, as the FITS convention requires, so the
  image appears the right way up in FITS viewers.
* The header carries the SOLARNET observation keywords described below, so
  that `astropy`, `sunpy` and other solar tools can read the time, place,
  exposure, filter, processing and sky coordinates.

#### Structure keywords

| Keyword | Value |
|---|---|
| `SIMPLE` | `T` |
| `BITPIX` | `-32` |
| `NAXIS` | `2` |
| `NAXIS1` | width in pixels |
| `NAXIS2` | height in pixels |
| `CREATOR` | `'Helia'` |
| `HISTORY` | one or more cards of up to 72 characters: how the capture was read, the edits (when they were applied to the pixels, or a note that the file is the unedited stack), then the colour look, in words |
| `END` | |

The FITS pixels are the unsharpened stack by default; sharpening and the other
edits are applied to them only when asked for (`--sharpen-fits`, `sharpen_fits`
in a job, or **Edit the FITS too** in the app), and the header then says so.

#### SOLARNET keywords

Helia follows the
[SOLARNET Metadata Recommendations](https://solarnet-metadata.readthedocs.io/en/stable/)
(Haugan and Fredvik, arXiv:2011.12139). A keyword whose value Helia cannot
know is left out, never guessed, and a `COMMENT` card in the header says which
ones are missing and why.

| Keyword | Value | Written when |
|---|---|---|
| `EXTNAME` | `'STACKED-IMAGE'` | always |
| `SOLARNET` | `0.5`: partially compliant. Helia does not claim full compliance, which needs more keywords than a stacker can know. | always |
| `OBS_HDU` | `1` | always |
| `DATE` | UTC time the file was written | always |
| `DATE-BEG` | UTC time the first contributing frame began | the capture is a SER file with timestamps (else the SER header start time if it is set) |
| `DATE-OBS` | the same as `DATE-BEG`, for readers that only know the older keyword | with `DATE-BEG` |
| `DATE-AVG` | mean UTC time of the frames that were kept | with `DATE-BEG` |
| `DATE-END` | UTC time the last kept frame ended (its start plus its exposure) | per-frame SER timestamps |
| `TIMESYS` | `'UTC'` | with `DATE-BEG` |
| `TELESCOP` | telescope name | the SER header has one |
| `INSTRUME` | instrument name from the SER header, else the camera model in the SharpCap sidecar | either has one |
| `OBSERVER` | observer name | the SER header has one |
| `FILTER` | the filter preset's name (for example `H-alpha`) | a preset was chosen or detected |
| `WAVELNTH` | line centre in nanometres, in air (H-alpha 656.28, Ca II K 393.37, Ca II H 396.85, Mg I b2 517.27, Na I D 589.3 as the mean of D1 and D2, He I D3 587.56, H-beta 486.13) | a line preset, not white light |
| `WAVEUNIT` | `-9`: `WAVELNTH` is in units of 10^-9 m | with `WAVELNTH` |
| `WAVEREF` | `'air'` | with `WAVELNTH` |
| `TEXPOSUR` | exposure of one frame, seconds | the SharpCap sidecar (`Exposure=`) is present |
| `XPOSURE` | accumulated exposure: `TEXPOSUR` times `NSUMEXP` | with `TEXPOSUR` |
| `NSUMEXP` | number of frames that were kept and summed | always |
| `OBSGEO-X`, `OBSGEO-Y`, `OBSGEO-Z` | observer position in metres, ITRS (WGS 84), from the site | a site was given (see below) |

The processing is recorded as one numbered group per step, in the order they
ran: `PRSTEPn` (the kind of step), `PRPROCn` (what did it), `PRPVERn` (the Helia
version, for example `'helia 0.5.0'`) and `PRPARAn` (the settings, shortened to
fit a card). The steps Helia writes are

| `PRSTEPn` | Written when | `PRPARAn` says |
|---|---|---|
| `DARK-SUBTRACTION` | a master dark was applied (first of the steps, before `FRAME-SELECTION`) | the master: frames, how they were combined, exposure, source and a fingerprint of its pixels |
| `FLAT-FIELDING` | a master flat was applied | the same, for the flat |
| `BAD-PIXEL-CORRECTION` | bad pixels were replaced | how many, and how many of them hot, cold or stuck |
| `FRAME-SELECTION` | always | the sharpness metric and how many of how many frames were kept |
| `DEBAYER` | the capture is raw colour | demosaicing method and colour channel |
| `MASKING` | text or exclusions were flattened, or the limb was fitted | how many regions, and whether the limb fit ran |
| `GLOBAL-ALIGNMENT` | always | how many frames built the reference and the mean confidence of the frames' alignment; `PRPROCn` names the method that ran (`limb fit` or `phase correlation`, see [How stacking works](../user-guide/how-stacking-works.md#3-aligning-the-whole-frame)) |
| `LOCAL-ALIGNMENT` | multi-point alignment ran | the alignment point size |
| `STACKING` | always | the combining mode and resampling kernel |
| `NORMALISATION` | the stack was normalised (the default) | `99.99th percentile scaled to 1` |

The three calibration steps are Helia's own words for them: the SOLARNET
recommendation lists `PRSTEPn` values only as examples, and these have not been
checked against a reader that insists on a fixed vocabulary.

A `HISTORY` card also names the Helia version, and carries how the capture was
read (Bayer pattern, bit depth and byte order with their sources, the SER
header's own claim, the mosaic measurement and any overrides) and the colour
look when one was chosen (see [Colourising](../user-guide/colourising.md)).

#### World coordinates

When a limb was fitted (see [How stacking works](../user-guide/how-stacking-works.md))
the header carries a helioprojective coordinate system in the `TAN` projection
(Thompson 2006), so that `sunpy.map.Map` builds a helioprojective map from the
file:

| Keyword | Value |
|---|---|
| `WCSNAME` | `'Helioprojective (limb fit, approximate)'` |
| `CTYPE1`, `CTYPE2` | `'HPLN-TAN'`, `'HPLT-TAN'` |
| `CUNIT1`, `CUNIT2` | `'arcsec'` |
| `CRPIX1`, `CRPIX2` | disc centre from the limb fit, in FITS pixels (counted from 1, the second axis from the bottom row) |
| `CRVAL1`, `CRVAL2` | `0`, `0`: the disc centre is taken as the Sun's centre |
| `CDELT1`, `CDELT2` | arcseconds per pixel: the apparent solar radius divided by the fitted limb radius in pixels |
| `CROTA2` | `0`. **The orientation of the picture on the sky is not known**, so this does not mean that solar north is up. |
| `LONPOLE` | `180` |
| `RSUN_REF` | `695700000.0`: the nominal solar radius in metres |
| `RSUN_ARC`, `RSUN_OBS` | the apparent solar radius in arcseconds that `CDELT` was computed from (`RSUN_OBS` is the same value, for `sunpy`) |

The coordinate system is **approximate**. The apparent solar radius comes from
the date (the mean frame time) and a low-precision formula for the Earth to Sun
distance, which agrees with the `sunpy` ephemeris to a few hundredths of an
arcsecond; with no date it assumes 1 AU, which can be wrong by up to 1.7 per
cent, and the header says so. No observer position (`DSUN_OBS`, `HGLN_OBS`,
`HGLT_OBS`) is written, so a reader derives it from the date and assumes an
Earth-based observer. If no limb is found there
is no world coordinate system and no `RSUN_*` keywords.

#### Giving the observing site

`OBSGEO-X`, `OBSGEO-Y` and `OBSGEO-Z` need the telescope's position, which no
capture file holds. Give it as `LAT,LON,ALT`: degrees north, degrees east, and
metres above the WGS 84 ellipsoid (`ALT` may be left out for 0).

| Front end | How |
|---|---|
| Command line | `--site 51.5,-0.1,30` |
| API | `"site": "51.5,-0.1,30"` in the job's `config` |
| Desktop app | the `HELIA_SITE` environment variable |
| Any of them | `HELIA_SITE=51.5,-0.1,30` is used when nothing more specific is given |

Without a site the three keywords are left out, a `COMMENT` card says so, and
the command line prints a note.

#### Reading the file

```python
import sunpy.map
m = sunpy.map.Map("sun.fits")      # a helioprojective GenericMap
print(m.scale, m.rsun_obs, m.reference_date, m.exposure_time)
```

The test suite does exactly this on a stack of a synthetic capture and asserts
the header values: `crates/helia-core/tests/solarnet_fits.rs` and
`solarnet_check.py`. The `solarnet` CI job installs `astropy` and `sunpy` for it.

#### Commentary

The `HISTORY` cards are text of up to 72 characters each (longer lines are
split, and anything that is not printable ASCII is written as `?`). They record
how the capture was read, and the look that a coloured copy of the same stack
would use, so the data and the colour file can be matched later; the pixels are
exactly those written without them.

The FITS writer in `helia-core` can also write unsigned 16-bit data
(`BITPIX = 16` with `BZERO = 32768` and `BSCALE = 1`), but nothing in the
command line, the app or the API selects it today.

Not written yet: `BUNIT` (the stack is normalised, not calibrated),
`WAVEMIN`/`WAVEMAX` (the filter bandwidth is not known), `DSUN_OBS` and the
heliographic observer keywords, and any rotation of the picture on the sky.
