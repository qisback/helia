---
description: "Troubleshooting Helia: GPU not found, files that will not open, slow stacks and odd colours, with fixes."
---

# Troubleshooting and FAQ

When something looks wrong, run `helia info` on the capture first. It prints
every field of the file's header, what Helia will use instead where the two
differ, and why. It reads the header and, for a SER file that says it is mono, a
few frames to check the pixels. Most of the problems below show up there.

## Opening captures

### "file truncated: header needs N bytes, stream has M"

A SER file records its frame count in the header. If the recording stopped
badly (a crash, a full disk, a copy that did not finish), the file holds fewer
frames than the header promises, and Helia refuses it rather than guess. Copy
the file again. If it is genuinely cut short, this version of Helia cannot read
it.

### "ffmpeg was not found"

You opened an MP4, MOV, M4V, MKV or WebM file, or a compressed AVI, and Helia
could not find `ffmpeg`. Those formats are read through the optional ffmpeg
program. Install it (the message lists the command for each system, and
[Installation](installation.md#optional-ffmpeg-for-mp4-and-other-video) has
them too), open a new terminal so `PATH` is refreshed, and try again. If it is
installed somewhere unusual, set `HELIA_FFMPEG` and `HELIA_FFPROBE` to the full
paths of `ffmpeg` and `ffprobe`.

### "ffmpeg is too old"

Helia needs ffmpeg 4.4 or newer. Update it with your package manager, or use a
static build from [ffmpeg.org](https://ffmpeg.org/download.html) and point
`HELIA_FFMPEG` and `HELIA_FFPROBE` at it.

### "the video is truncated or damaged"

A recording that stopped badly often has no index (an MP4's `moov` box is
written at the end) or ends mid-frame. Helia reports it rather than stacking a
partial video without telling you. Try recovering the file with
`ffmpeg -i broken.mp4 -c copy fixed.mp4`, or record to SER. A few frames missing
at the very end of a video (up to 1 per cent) are tolerated.

### "decoding needs about N MiB of free space"

Compressed video is decoded once into an uncompressed file in a cache
directory, and that needs room for every frame at full size. Free some space,
point `--cache-dir` (or the `HELIA_CACHE_DIR` environment variable) at a bigger
disk, or open a shorter clip. The file is removed when Helia is done; if Helia
was killed, a leftover `helia-video-*.ser` file older than a day is deleted the
next time a video is decoded.

### "unsupported AVI video format"

Helia reads uncompressed AVI itself (`BI_RGB`, `Y800`/`GREY`, `Y16`, 24-bit
BGR). An AVI with another codec, for example MJPG, goes to ffmpeg like an MP4.
If you still see this message, the file is not a plain AVI at all: run
`helia info` on it. See [Capturing for Helia](capturing.md).

### My AVI is huge, or reported as having the wrong number of frames

Files over 1 GB are written as a chain of OpenDML sections. Helia supports
them: it finds the frames by walking the file's own chunks, not by trusting the
index at the end, because capture programs often write that index wrongly or
leave it out. Run `helia info` and compare the frame count with what your
capture program said. If `helia info` is wrong for a file that plays elsewhere,
please report it with the file's first few hundred megabytes if you can.

### SharpCap SER files and the byte-order flag

A 16-bit SER file has a header flag that says whether the pixel bytes are
little-endian or big-endian. SharpCap files have been seen with the flag
cleared (big-endian) while the data is really little-endian, which makes the
image look like harsh noise. Helia checks: when the flag says big-endian it
looks at the first frame, and decodes it the way that gives a smooth image.
`helia info` shows both sides, the flag as written under the header and the order
used under *Helia will use*:

```text
  LittleEndian:      0 (big-endian)
  Byte order:        little-endian  (detection)
```

If the two differ, Helia has corrected a mislabelled file and nothing needs
doing. Data that really is big-endian is left alone. If Helia gets it wrong
(the picture is harsh noise), say which with `--endian little` or `--endian big`.

### The header gets the bit depth wrong, or the picture is upside down

A recorder can also write the wrong bit depth into the header. The usual sign is
that `helia info` or the app refuses the file as truncated, or that the picture
is striped or looks like noise. Use `--bit-depth 8` or `--bit-depth 16` to say
what the pixels really are (any value from 1 to 8 reads one byte per pixel, 9 to
16 two). A file stored with the bottom row first (rare in SER) is read the right
way up with `--flip-rows`. All of these apply to SER files, and the same choices
are in the Open step of the app and in the API (`bit_depth`, `endian`,
`flip_rows`). See [Capturing for Helia](capturing.md#why-a-ser-header-can-be-wrong).

### A grid or chessboard in the stack (a Bayer mosaic that was not removed)

A raw colour camera records one colour per pixel in a repeating 2 x 2 pattern.
The picture has to be demosaiced (debayered) before it is stacked. If it is not,
the stack carries the pattern: a fine grid or chessboard, with neighbouring
pixels alternating between bright and dark. Behind a narrow-band filter such as
H-alpha it is stark, because the blue pixels receive almost no light.

The usual cause is a SER file whose header says *mono* (`ColorID` 0) although
the frames are raw Bayer data. SharpCap does this for some colour cameras (a
ZWO ASI482MC, for example). Helia no longer trusts such a header blindly. For a
SER file that says mono it measures the four pixel positions of a 2 x 2 cell on
the solar disc in a few frames; if they differ by 5 per cent or more in a way a
colour-filter array would produce, it reports a mosaic, names the pattern and
debayers it. It says so:

```text
warning:  The SER header says mono (ColorID 0), but the frames hold a Bayer mosaic
that looks like RGGB: the four pixel positions of a 2x2 cell ... Helia will
debayer them as RGGB. ...
```

The desktop app shows the same message above the Quick stack card in the Open
step, and the API returns it in `notices` (see [The API](api.md)). Nothing is
changed silently: `helia info` and the app's *Helia will use* list give the
Bayer pattern with its source (`detection`), and the choice is recorded in the
output file.

If a stack made before this check shows the grid, stack it again.

If the check is wrong, tell Helia what you know:

```sh
helia stack capture.ser -o out.tif --bayer none     # it really is mono
helia stack capture.ser -o out.tif --bayer bggr     # a different pattern
```

Two cases need your eye:

* **Red and blue swapped.** The geometry of the mosaic says which diagonal is
  green but not which of the other two pixels is red. Helia settles it from the
  layout a SharpCap sidecar states, the colour the filter passes (red for
  H-alpha, blue for calcium K), the usual layout of the camera model, and
  finally by taking the dimmer of the two as blue. The message says which. For
  a stack of a single channel (the Ha preset takes the red one) a wrong pick
  matters; if the result looks like noise, try the other pattern of the same
  family (`rggb` or `bggr`, `grbg` or `gbrg`).
* **A mosaic too faint to see.** If the colours are balanced so that all four
  pixel positions are within 2 per cent of each other, the data is called mono;
  between 2 and 5 per cent Helia cannot tell, and uses the sidecar's colour
  space if there is one. Give `--bayer` yourself.

### Colours are wrong, or a colour camera looks like a grey mosaic

Raw colour cameras store a Bayer mosaic. In a SER file the pattern is normally in
the header (and see the previous section when the header says mono). An AVI
cannot say whether 8-bit data is raw colour or plain mono, so for SharpCap AVI
files Helia reads the `CameraSettings.txt` beside the video (same name,
`.CameraSettings.txt` in place of `.avi`). `helia info` shows the sidecar it
found, its colour space and its Debayer Preview, or no sidecar rows.

If there is no sidecar or the guess is wrong, say what you have:

```sh
helia stack capture.avi -o out.tif --bayer rggb
```

The patterns are `rggb`, `grbg`, `gbrg` and `bggr`; `none` forces mono. For a
bottom-up AVI the pattern means the picture the right way up. If the picture
looks like a fine chessboard, or the colour planes are swapped, try the other
patterns. Many ZWO colour sensors are RGGB, and Helia assumes RGGB for a raw
colour sidecar. The pixel check described above runs for SER files only; an AVI
has no header field to doubt, so its sidecar and `--bayer` decide.

## H-alpha with a colour camera

The H-alpha line is red (656 nm). On a colour camera it reaches mainly the red
pixels of the sensor, so the green and blue pixels carry little more than noise.
Averaging the three planes mixes that noise in and softens the result. Helia
uses the red plane for H-alpha by itself when it knows the filter: put `Ha` in
the file name, or give `--preset ha`:

```sh
helia stack ha.ser -o ha.tif --preset ha
```

If Helia did not recognise the filter (the `settings:` line says `filter not
recognised`), the default is all three planes; name the plane yourself with
`--channel r`. In the app, the Quick stack card picks the filter and the channel
with it; **Advanced settings** at the foot of the inspector has the channel
buttons. For a white-light colour capture, `lum` or `g` is usually the better
choice. Mono cameras are unaffected by these options.

## Helia picked the wrong filter, or none

Helia reads the filter from the file name, then the folder, then the SharpCap
sidecar and the SER header (see
[Colourising](colourising.md#how-the-filter-is-recognised)). A whole word has to
match, so rename the file to include `Ha`, `CaK`, `Mg`, `Na` or similar, or name
the filter yourself: `--preset` on the command line, the filter buttons in the
Quick stack card. A filter you choose always wins over the one detected.

## The result

### The stack looks soft

Answer these in order. Each is cheaper to fix than the next, and the first two
decide how sharp the stack can be at all.

1. **How good were the best frames?** A stack cannot be sharper than the sharp
   moments it is built from. Look at the *Quality, sorted* curve in the Analyse
   step. If the best frames are only a little better than the middle of the
   capture, the air was the limit. `helia stack --seeing --aperture-mm 100` (give
   your aperture) prints the Fried parameter `r0` and `D / r0`, and a grade: 10 cm
   or more is excellent, 7 to 10 good, 4 to 7 fair, 2.5 to 4 poor, under 2.5 bad.
   See [Seeing estimation](seeing.md). Nothing later in the chain makes up for bad
   seeing.
2. **How many frames did you keep?** Keep fewer: `--keep 10%`. If the sorted curve
   falls steeply, a smaller share helps; if it is flat, it will not. A lower
   `D / r0` allows a larger share.
3. **Are the alignment points doing their job?** Open the Align step and check how
   many points are active. Photospheric targets (white light, Na D) need small
   points, a few granules across: `--ap-auto` picks the size, spacing and search
   range from the image scale and the seeing, and `--ap-passes 2` measures the
   shifts a second time against the first stack. Compare with `--no-multipoint` and
   try a different `--ap-size`. See [How stacking works](how-stacking-works.md).
4. **Is it only the stack that is soft?** A stack is smoother and cleaner than any
   single frame, but not as sharp as the telescope can be. Sharpen it: the Sharpen
   step, or `--sharpen wavelet` on the command line. Judge with **Before / after**
   and **Full size**, because the preview is a shrunk copy. The method, the
   sliders and the recommended values for each filter are in
   [Sharpening and adjustments](sharpening.md) and the step-by-step
   [Sharpen](../walkthrough/06-sharpen.md).

**About the wavelets.** Wavelet sharpening in Helia is an a-trous transform with a
gain and a noise cut for each of six layers. Users coming from RegiStax have
reported that they cannot reach the same sharpness with it, and suspect that the
gains Helia allows on the fine layers are much smaller than the 10 to 100 times
they use there. Making the wavelet controls behave like RegiStax 6, with
comparable ranges, is in progress under issue
#131 and is not finished.
Until it lands, do not expect the same look from the same numbers, and do not read
this page as a promise of how the result will look.

### Sharpening left halos, ringing or grain

* Sharpening can create artefacts. Use **Before / after** and ease off: lower the
  gains of the wavelet layers (layer 1 and 2 first), raise their noise cuts, lower
  the unsharp amount, or use fewer deconvolution iterations.
* Ringing at the limb or round sunspots after deconvolution: keep the **limb and
  sunspot guard** at 1, and check that the blur (PSF) is not larger than the real
  one: too big a blur over-corrects.
* Grain or speckle that looks like texture is noise that was boosted. Raise the
  noise cut of the finest layers, turn on **Denoise** in the Adjust step, or stop
  the deconvolution sooner.
* The preview is a shrunk copy; press **Full size** to judge fine detail.

### Concentric rings after flattening and sharpening

**What you see.** Faint rings about ten pixels apart, centred on the disc, that
appear when you flatten the limb darkening and sharpen (`--flatten model
--sharpen wavelet`), most of all on a Seestar or phone video. Without
`--flatten` they are hidden under the brightness gradient.

**Why.** An 8-bit capture has only about 80 to 250 grey levels across the disc.
Each ring is one grey level of the limb darkening, and a lossy video encoder
removes the noise that would otherwise dither the steps, so stacking many frames
does not average them away. `--flatten model` divides by a smooth law, so the
steps are left standing, and the wavelets then lift them.

**What to do.** Use `--flatten profile`, which divides by the measured profile
ring by ring and so takes the rings out too. Go easy on the finest wavelet layers
for an 8-bit video, and record SER at 12 or 16 bits where the camera allows.

### The red channel is clipped, or the disc is a flat white plate

This is issue
#119, found on the
H-alpha captures of 2022-07-17 and 2026-10-20 (a ZWO ASI482MC, AVI and SER
files). Behind a narrow-band filter one colour plane of the sensor carries nearly
all the light. H-alpha is red, so the red pixels do. An exposure long enough to
show the faint leak of the line, or the prominences, pushes those pixels to the top
of the sensor's range, and almost every lit pixel of the disc then sits at one
value: 96 to 100% of them, 255 in the AVIs and 203 in the SER files. A plane that
is flat holds no detail, so a stack of it is a white disc with prominences round it
and no filaments, plage or fibrils. The green plane, which carries the leak of the
line at 40 to 60% of full scale, has all of that. Measured on `15_37_19.avi`, the
granulation-band contrast of the stack is 0.14% from the red plane and 1.39% from
the green. A white-light capture exposed too long does the same, to a lesser degree
(`14_42_57.avi`: red 50% and blue 21% clipped).

What Helia does about it (the fix, pull request
#125, is on `main` and comes
in the release after 0.5.0): before stacking, `helia stack` and API jobs read six
frames, find the planes that are mostly flat at their own ceiling and stack the
brightest plane that is not. Empty planes (blue behind a red filter) are never
chosen. A note in the settings says what happened:

```text
the red plane is clipped (red 98% of its lit pixels sit at the sensor limit, so
the disc is flat there); stacking the green plane instead. ...
```

* **A channel you name is never changed.** `--channel r` stacks red, clipped or
  not. Leave `--channel` off to let the fallback work.
* **The desktop app does it too.** When a capture opens and you have not touched the
  Channel chip, the Stack step shows the same note ("Channel: the red plane is clipped
  ...") and the chip under Advanced settings moves to the plane that is used. A plane you
  pick on the chip is never changed: choose **G** yourself if you picked **R** and the
  result is a flat plate.
* **If every usable plane is clipped**, the note says so and nothing is replaced:
  the exposure is too long for any of them.
* **The cure is at the telescope**: a shorter exposure or a lower gain, so the
  histogram of the bright plane stays clear of the right-hand edge. See
  [Exposure, gain and bit depth](capturing.md#exposure-gain-and-bit-depth).

### A burned-in timestamp, or a label, shows in the result

Helia finds a burned-in timestamp (the date and time that SharpCap and other
programs draw into the picture) by itself, keeps alignment and quality scoring
off it, and replaces it with a flat patch of sky in the result. The command
line summary lists what it masked (`masked:`), and the points off the disc and
off the text are simply not used. See
[Keeping alignment off text and sky](how-stacking-works.md#keeping-alignment-off-text-and-sky).

If some other text or a mark is still in the way, give its rectangle with
`--exclude x,y,w,h` (pixels from the top-left corner; repeat for several), or
add it under *Exclude regions* in the Align step of the app. If Helia masks
something it should not (it only looks for small, bright, changing areas at
the edge of the frame), switch the automatic detection off with
`--no-overlay-detect`. If the disc fit goes wrong and alignment points are
missing on a real disc, use `--no-disc-mask`, or `--prominence-band` to let
points sit a little outside the limb.

### Nothing improves, or the alignment points are all inactive

The point count in the app (and `points:` in the command line summary) says how
many points were bright and detailed enough to measure. Very low-contrast
frames, or too small a point size, leave few active points. Try a larger
`--ap-size`. If almost none are active, check the capture: an exposure so short
that the frame is nearly black, or a sensor region with no disc, leaves nothing
to lock on.

## GPU

### Helia says `backend: CPU`, or cannot find my GPU

See the troubleshooting section of [GPU acceleration](gpu.md). The short
version is: check your driver, then try `--backend gpu` to get an error message
that says why.

## Reporting a problem

Open **About** and choose **Save bug report**, then attach the `.zip` to an issue
at github.com/qisback/helia. It has the version, your GPU, the open
capture's header and the end of Helia's log, all as text you can read first. The
log itself is `helia.log`; where it lives, and what it does and does not record,
is in [Log file and bug report](gui.md#log-file-and-bug-report).

## Questions

**Does Helia need a GPU?** No. It uses one when it finds one and otherwise runs
on the CPU with the same results.

**Does Helia use the network?** Only when you send feedback. The stacker and the
desktop app otherwise contain no code that makes network connections; the feedback
client connects to the feedback service when you press Send (or run
`helia feedback`) and at no other time. `helia serve` listens on the address you give it,
on the loopback interface unless you say otherwise; see
[The API](api.md) for its security model.

**Does it modify my captures?** No. It only reads them. Output files are
written where you say, and the command line overwrites an existing output file
without asking.

**Can it stack colour?** A colour camera capture is stacked as one plane
(`--channel`, which Helia chooses from the filter), because narrow-band solar
data is one colour of light. The stack is mono. Helia can then colour it for
sharing with a look for each filter (orange for H-alpha, violet for calcium and
so on), into a 16-bit RGB TIFF or an 8-bit PNG; FITS stays mono. See
[Colourising](colourising.md). A true three-colour stack of a white-light
capture is not built.

**Can I calibrate with darks and flats?** Yes: see [Calibration](calibration.md)
and [Capturing for Helia](capturing.md). Build masters with `helia calibrate`, or
name the captures in the Calibrate step of the app.

**Where do I report a bug?** Press **Feedback** in the desktop app (Ctrl+Shift+F),
or run `helia feedback`: it files it in the issue tracker for you, with a screenshot you can
draw on, even if you have no account on the forge. See
[Sending feedback](sending-feedback.md). Or use the project's issue tracker
directly, with your operating system, GPU and driver, the output of `helia --version`, and if you
can a short capture (a few hundred frames) that shows the problem.
