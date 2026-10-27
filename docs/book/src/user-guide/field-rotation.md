# Field rotation (altazimuth mounts)

On an altazimuth mount the camera keeps its orientation to the horizon while the
sky turns about the optical axis, so the picture of the Sun rotates during a
capture (#24). The rate is
`15 degrees per hour * cos(latitude) * |cos(azimuth)| / cos(altitude)`: at 51.5
degrees north with the Sun on the meridian near the equinox it is 0.2 degrees a
minute, 12 degrees an hour. On a disc 450 pixels in radius that is 1.6 pixels a
minute at the limb. A ten second capture does not notice; a ten minute capture
turns by two degrees and smears the limb by 16 pixels, more than the alignment
points can follow; and a time-lapse of stacks taken hours apart shows the whole
surface turning in the frame.

Helia undoes it from the capture's own timestamps. An equatorial mount has no
field rotation: leave this off.

## Using it

* **A long capture.** `--field-rotation` in `helia stack`, with the observing site
  (`--site LAT,LON,ALT`, or the `HELIA_SITE` environment variable):

  ```sh
  helia stack session.ser -o sun.fits --preset white --field-rotation --site 51.5,-0.1,30
  # for example, for a ten minute capture near noon at the equinox:
  rotation: the parallactic angle runs 2.31 degrees over the capture (0.231 degrees per minute, Sun at 38 degrees altitude); frames are turned by up to 1.16 degrees to the orientation at 2026-03-20T12:05:00.000
  rotation: at the limb that is up to 9.1 pixels
  ```

  Every frame is turned to the orientation of the middle of the capture
  (`--field-rotation-reference first|last` chooses another) before it is scored,
  aligned or stacked. The capture needs timestamps, which SER files carry and AVI and
  video files do not. `--field-rotation-mirror` is for a mirror image (an odd number
  of reflections, a star diagonal for one), where the turn is the other way. It
  works with `--colour true`: a raw mosaic cannot be turned, so each of red, green and
  blue is demosaiced and turned alike before it is stacked.
* **Subs of a star field (a Seestar).** FITS subs whose header holds `RA`, `DEC`,
  `SITELAT`, `SITELONG`, `EQMODE = 0` (altazimuth) and `DATE-OBS` need no flag:
  `helia stack` turns them by the parallactic angle of the target in the header, about
  the site in the header, and prints `rotation:` lines. `--no-field-rotation` stops
  it; `--site` overrides the header's site; `--field-rotation` asks for it on subs
  that lack `EQMODE`. See [Stack FITS frames](../tutorials/stack-fits-frames.md) for the
  worked example.
* **A time-lapse.** `helia derotate` turns stacked images of one session to one
  orientation, from the time each was taken:

  ```sh
  helia derotate sun-1100.fits sun-1130.fits sun-1200.fits -o aligned --site 51.5,-0.1,30
  helia derotate a.tif b.tif c.tif -o aligned --site 51.5,-0.1 \
      --times 2026-03-20T11:00:00,2026-03-20T11:30:00,2026-03-20T12:00:00
  ```

  The time is a FITS file's `DATE-AVG` (which `helia stack` writes) or `DATE-OBS`,
  or `--times` for each image in order. The images come out as `NAME-derot.EXT` in
  the folder, turned to the middle image's orientation (`--reference first`, `last`
  or an image number choose another); a FITS file keeps its time, the site and a
  `PRSTEP` for the turn. The images are turned about their centre and keep their
  size; the disc may have moved a little in the frame and the corners are filled with
  the edge level, so stack or crop afterwards. `helia derotate` refuses to replace a file that exists
  unless you give `--overwrite`.
* **API:** `"field_rotation": {}` in a job's `config` (with `site`); `mirror` and
  `reference` are optional. `summary.field_rotation` says what it did. See
  [The API](api.md).
* **App:** Advanced settings, Field rotation: Alt-az, the site and Mirror image.
  The line under the controls says how far the capture rotates.

## Subs of a star field

The same angle serves a target that is not the Sun: a fixed point of the sky, its
right ascension and declination taken from the header. A sequence of subs over half an
hour on an altazimuth mount turns by several degrees, which trails every star but
those at the centre (7.5 degrees over 29 minutes for NGC 7009 from 36 degrees north).

Which way the picture turns depends on how the rows of the file run, and the header does
not always say so truthfully: a Seestar's rows are stored top-down with no `ROWORDER`
(Helia now knows this and reads them as stored), and other programs' files may differ.
Helia therefore reads the sense from the stars. The first and last sub (the two the field has turned between
the most) are turned each way to the common orientation and phase-correlated; the
sense in which they lock sharply is the right one. It is believed when the lock (the
peak-to-sidelobe ratio) is at least 5 and at least half as much again as the other
sense; otherwise nothing is turned and the `rotation:` line says so. A field that turns by under
half a degree is left alone too, because the stars cannot tell the sense apart then. To
settle it yourself, give `--field-rotation` (and `--field-rotation-mirror` if the picture
is a mirror image); a named sense is not measured.

Reference for the correlation: Reddy and Chatterji (1996), IEEE Transactions on Image
Processing 5(8), 1266-1271.

## How it works

The angle is the **parallactic angle** `q`, the angle at the Sun between the
direction to the celestial north pole and the direction to the zenith:

```text
H = LST - RA                                       (hour angle)
tan q = sin H / (tan(latitude) cos(dec) - sin(dec) cos H)
```

(Meeus, *Astronomical Algorithms*, equation 14.1; sidereal time from equation 12.4,
checked against the book's worked example.) The Sun's right ascension and
declination are the apparent ones of the series used for the sunspot positions
([Sunspots](sunspots.md)). `q` is positive after the Sun has crossed the meridian,
and then celestial north is **clockwise** of the zenith direction in a picture that
is not a mirror image. A frame taken at `q` is brought to the orientation of a frame
taken at `q_ref` by turning it **counter-clockwise** by `q - q_ref` (clockwise in a
mirror image). The turn is the same whichever way up the camera is, because a
camera that is turned a fixed angle adds the same angle to every frame.

The turn is about the centre of the frame, which is where the mount points, and uses
Lanczos interpolation. The disc is not exactly on that axis, so it moves a little
(a turn of 1 degree moves a disc 100 pixels off the centre by 1.7 pixels); the
whole-frame alignment that follows takes that out. A frame with no timestamp of its
own takes the nearest one. The angle is followed through the jump at 180 degrees
that a Sun near the pole of the sky makes.

## What was measured

**The maths.** The angle matches the textbook cases (zero on the meridian, 45
degrees at six hours west for a latitude of 45 degrees, 135 in the southern
hemisphere), the sidereal time matches Meeus's example 12.a to 0.0001 degree, and
the rate at noon near the equinox matches the formula above to 0.01 degree a minute.

**A synthetic Sun.** An hour of a synthetic capture on an altazimuth mount (25
frames, 2.5 minutes apart, turned by up to 6 degrees, no noise), stacked on global
alignment only: the error against the true image falls from 0.0207 to 0.0008 (26
times), and the stack is as sharp as the truth.

**A real capture.** `15_20_57.ser` (100 frames at 10 per second, real timestamps):
over its own 9.5 seconds the angle runs 0.023 degrees, 0.09 pixels at a limb of 445
pixels, which is nothing to correct. To see what the correction does on real
frames, the same 100 frames were given the timestamps of an hour (one every 36 s,
from a site at 51.5 degrees north, the Sun at 47 degrees altitude: 6.3 degrees, 0.107
degrees a minute, turns of up to 3.2 degrees, 25 pixels at the limb) and turned by
the field rotation of those times, and stacked, keeping the best half. The
granulation band (1 to 4 pixels) at the disc centre, in per cent of the mean:

| | Frames as they were | Rotating capture | Corrected |
|---|---|---|---|
| Global alignment | 1.286 | 0.706 | 1.296 |
| Alignment points | 1.311 | 0.915 | 1.321 |

![Grouped bars of the granulation band (1 to 4 px) at the disc centre in per cent of the mean. Global alignment: frames as they were 1.286, rotating capture 0.706, corrected 1.296. Alignment points: 1.311, 0.915 and 1.321.](../images/charts/user-guide-field-rotation-granulation.svg)

*Granulation band at the disc centre for 100 frames of `15_20_57.ser` given the timestamps of an hour, from the table above (higher is better).*

The rotating capture loses 45% of the granulation on global alignment and 30% with
the alignment points, which follow some of the turn; the correction gives it all
back.

## Limits

* It needs a timestamp for every frame (SER, or FITS with `DATE-OBS`) and the site, and the Sun's position
  is the astronomical one: no refraction, and a time that is only as good as the
  computer's clock that wrote it.
* The turn is by Lanczos interpolation, which costs a little detail (the corrected
  granulation above is within 1% of the original) and some time: frames are turned
  one at a time, so a corrected stack of a long capture takes longer than one that is
  not (the capture is no longer decoded in parallel).
* The pivot is the frame centre. A mount that does not hold the Sun on the axis
  leaves a drift the alignment must take out.
* `helia derotate` cannot tell the time of a TIFF or PNG: give `--times`.
