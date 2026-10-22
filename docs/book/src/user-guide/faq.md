# Frequently asked questions

Short answers, with a link to the page that has the whole story. Problems with a
particular message or picture are in [Troubleshooting](troubleshooting.md).

## Getting it

**Where do I download it?** The
[releases page](https://github.com/qisback/helia/releases). See
[Download and verify](../getting-started/download-and-verify.md).

**Is it free?** Yes: GNU General Public Licence version 3 or later, with additional
terms under section 7 that keep the copyright line and the dedication. See
[About](../walkthrough/10-about.md).

**Which systems?** Windows 10 and 11 (x86_64), Linux x86_64 and aarch64 (including
Raspberry Pi 4 and 5 on a 64-bit system) are published. macOS and Windows on ARM
are built from source. See [Installation](installation.md).

**Why does it say pre-release?** It is below version 1.0. The features in this manual
work and are tested; some planned ones (derotation, drizzle) are
not built yet. See the [roadmap](../roadmap.md).

## Using it

**Do I need a GPU?** No. It uses one when it finds one and otherwise runs on the CPU
with the same results. See [GPU acceleration](gpu.md).

**What do I record?** SER, 16-bit if possible, short exposures, many frames. See
[Capturing for Helia](capturing.md).

**Can I use a phone video?** Yes, with ffmpeg installed. See
[MP4 from a phone](../recipes/mp4-from-a-phone.md).

**How many frames should I keep?** The recommended share for the filter (25 or 30
per cent) is a good start. Fewer is sharper and noisier. See [Stack](../walkthrough/05-stack.md).

**Which channel, or band, should I use for my filter?** Let Helia choose: it reads
the filter from the file name, folder or sidecar and applies the preset below. The
choice follows which pixels of a colour sensor the light reaches. On a mono camera
none of this applies.

| Filter | Colour plane stacked | Frames kept | Why |
|---|---|---|---|
| H-alpha | red | 25% | 656 nm is deep red; green and blue pixels add noise, not detail |
| Ca II K, Ca II H | blue | 30% | violet light reaches the blue pixels; the line is faint |
| Mg I b | green | 30% | 517 nm is green; the line is dim |
| Na I D | green | 12% | the filter leaves the blue pixels empty, and averaging them in prints the mosaic over the granulation; green is two pixels in four |
| He I D3 | all three combined | 25% | 587.6 nm is shared between red and green |
| H-beta | blue | 30% | 486 nm reaches the blue pixels; faint |
| White light | all three combined | 12% | the continuum carries all three colours; shows granulation |

Name another plane with `--channel r|g|b|lum`. A plane that turns out to be clipped
is replaced by the next one (see
[The red channel is clipped](troubleshooting.md#the-red-channel-is-clipped-or-the-disc-is-a-flat-white-plate)),
unless you named it. The reasons, with the numbers, are in
[Colour presets](../reference/presets-table.md). One caution: the 2022 captures that
were first taken for sodium were later identified, from their plane statistics, as
H-alpha (red clipped, blue empty, prominences in red; the labels have since been corrected). The sodium choice of green therefore rests on those captures, not on a real
Na D filter, and no true sodium capture is in the test set yet. If your Na D result
looks wrong, try `--channel lum` and `--channel r` and compare.

**My image looks soft.** Check the seeing, then the share of frames kept, then the
alignment points, then sharpen. See
[The stack looks soft](troubleshooting.md#the-stack-looks-soft). The wavelet controls are described in
[Sharpening like RegiStax](../recipes/sharpening-like-registax.md); Helia does not promise
the same look as RegiStax.

**The red channel is clipped, or the disc is a flat white plate.** The exposure was
long enough to push the red pixels to the sensor limit. Helia stacks the next plane
by itself unless you named one. See
[The red channel is clipped](troubleshooting.md#the-red-channel-is-clipped-or-the-disc-is-a-flat-white-plate).

**Which sharpening method?** Wavelets, at the recommended values, almost always. See
[Sharpen](../walkthrough/06-sharpen.md).

**Does it change my captures?** No. It only reads them.

**Can I get the picture without colour?** Yes: pick **Mono** in the Colour step, or
export FITS, which is mono for a filter stack (and a red, green, blue cube for a true-colour one).

**What is the colour, really?** A convention. Narrow-band solar data is one colour of
light; orange for H-alpha, violet for calcium and so on are for sharing. See
[Colourising](colourising.md).

**Why is the FITS not sharpened?** It is for measurement and further processing. Turn
on **Edit the FITS too** (or `--sharpen-fits`) if you want it sharpened.

## Privacy and network

**Does Helia use the network?** Only when you send feedback. The stacker and the
desktop app otherwise make no network connections of their own; links open in your
browser only when you click them. The feedback client, in the app and as
`helia feedback`, connects to the feedback service when you press Send (or run the
command) and at no other time. `helia serve` listens where you tell it, on the
loopback interface unless you say otherwise. See [Send feedback](../walkthrough/feedback.md).

**Does it send usage data?** No. There is no telemetry.

## Problems

**A grid across the picture.** [Fixing a Bayer grid](../recipes/fixing-a-bayer-grid.md).

**It picked the wrong filter.** Press the right one on the Quick stack card, or add
`--preset`. See [Troubleshooting](troubleshooting.md#helia-picked-the-wrong-filter-or-none).

**The sharpening looks overdone.** Turn it down: it is. See
[Troubleshooting](troubleshooting.md#sharpening-left-halos-ringing-or-grain).

**`--robust` did not remove a hot pixel or dust spot.** Robust stacking removes
what is different in one frame and not in the others (a satellite, a cosmic ray,
a hot pixel the alignment moves). A defect fixed on the sensor is in the same
pixel of every frame, so it is not an outlier. Use a dark frame or a bad-pixel
map: [Calibration](calibration.md). See
[Robust stacking](../tutorials/robust-stacking.md).

**Where do I report a bug?** See [Send feedback](../walkthrough/feedback.md).
