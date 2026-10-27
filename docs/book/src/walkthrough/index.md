---
description: "A step-by-step walkthrough of Helia from a raw capture to a finished picture, with a screenshot at every step."
---

# Step by step

This part of the manual follows one job from start to finish in the desktop app,
a screenshot at every step. It goes from a raw video of the Sun to a finished,
coloured, sharpened picture, and says at each step what the controls do and what
to look for. Read it once in order; afterwards the steps are reference.

1. [Open a capture](01-open.md): the header, what Helia will use, the overrides
2. [Quick stack](02-quick-stack.md): the one-button route
   * [Calibrate](calibrate.md) (optional): dark, flat and bad pixels, before anything else
3. [Analyse](03-analyse.md): which frames are sharp
4. [Align](04-align.md): alignment points, exclusions, the limb fit
5. [Stack](05-stack.md): combining the best frames
6. [Sharpen](06-sharpen.md): wavelets, unsharp mask, deconvolution
7. [Adjust](07-adjust.md): denoise, levels, curve, crop
8. [Colour](08-colour.md): the filter looks
9. [Export](09-export.md): TIFF, PNG and FITS
10. [About](10-about.md)
11. [Send feedback](feedback.md)

## The captures used

Three real captures of the Sun, taken on 17 July 2022 with a ZWO ASI482MC colour
camera, are used throughout. They are copies, renamed so that the pictures show
no folder names.

| Name in the pictures | What it is | Used for |
|---|---|---|
| `disc-sample.ser` | A full disc, 1920 x 1080, 16-bit, 100 frames | The main walkthrough: analyse, align, stack, sharpen, adjust, colour, export |
| `ha-sample.avi` | An H-alpha capture at 720 x 540, 8-bit, 1000 frames, with the SharpCap `CameraSettings.txt` beside it | The Open step with a filter Helia recognises from the file name |
| `bayer-sample.ser` | 1920 x 1080, 8-bit, 631 frames, a SER whose header says mono although the pixels are a colour mosaic | The header view and the Bayer notice |

You can follow the same steps with your own captures; the numbers on your screen
will differ.

## How the screenshots were made

They are real: the application itself, running on those captures with its real
worker threads, drawn to an image by the test harness and saved without being
touched up. The graphics adapter named in the top bar is the one of the machine
that made them (an NVIDIA A100 on a Linux build server), not a recommendation.
Where a picture of the finished image is shown (the before and after pairs, the
colour gallery) it is the image Helia writes, cropped or reduced in size, not a
window.

The set is made by one test, `real_capture_shots` in
`crates/helia-gui/src/real_shots.rs`, which does nothing unless it is given a
capture. See [the source repository](https://github.com/qisback/helia). The
other pictures in the book, under `images/ui`, are the synthetic snapshots that
the ordinary test run checks against the interface.

The app has its own scripted mode, `helia-gui --open FILE --shots DIR`, which
writes screenshots of a live window as it opens and stacks a capture. It needs a
window that is being painted and stops at the Export step, so the book uses the
off-screen test, which also reaches Sharpen, Adjust, Colour and About.
