# Glossary

**Alignment point (AP).** A small square window on the disc. Helia measures how each
frame has drifted at each point and shifts that part of the frame back, so the
distortion the atmosphere adds across the Sun is corrected locally. See
[Align](../walkthrough/04-align.md).

**Artefact.** A feature in a picture that is not in the Sun: a halo beside a sharp
edge, ringing round a sunspot, noise turned into texture, a grid from a mosaic.

**Bayer pattern.** The repeating 2 x 2 arrangement of red, green and blue filters on
a colour camera's sensor (RGGB, GRBG, GBRG or BGGR). Raw captures hold one colour per
pixel until they are *debayered*. See
[Fixing a Bayer grid](../recipes/fixing-a-bayer-grid.md).

**Bit depth.** Bits per pixel in the capture, 8 or 16. More bits keep fainter
contrast.

**Channel.** One colour plane of a colour camera: red, green, blue, or the combined
luminance (`lum`). Narrow-band solar data is one colour of light, so Helia stacks one
channel; the recommended one depends on the filter.

**Point spread function (PSF).** The blur that the telescope, the air
and the camera put on a point of light. Deconvolution tries to undo it.

**Debayer, demosaic.** Turning a one-colour-per-pixel mosaic into a full-colour
picture. Helia offers bilinear and Malvar-He-Cutler (the default).

**Deconvolution.** Sharpening by undoing a known blur. Helia uses Richardson-Lucy with
a limb and sunspot guard.

**FITS.** The astronomy image format. Helia writes 32-bit floating-point mono FITS with
solar metadata (SOLARNET keywords) in the header.

**Frame.** One picture in the capture video.

**Gain (sensor).** The camera's amplification. In Helia's sharpening, the *gain* of a
wavelet layer is its multiplier.

**Granulation.** The fine cell pattern of the Sun's visible surface, a few
per cent in contrast, seen in white light.

**Limb.** The edge of the solar disc. Helia can fit it to align whole frames.

**Lucky imaging.** Recording many short frames, keeping the sharpest and averaging
them, so the best moments of the seeing win. What Helia does.

**Noise cut.** In wavelet sharpening, detail weaker than this many noise sigmas in a
layer is treated as noise and removed.

**Phase correlation.** A way of finding the shift between two pictures by comparing
their frequency content. Helia uses it to align whole frames when the limb fit is not
confident.

**Pre-release.** A version before 1.0.

**Quality.** A frame's sharpness score, scaled so that the best frame in a capture is
1 and the worst is 0. **Rank** is the position when the frames are sorted by it; rank 1
is the sharpest.

**SER.** A simple uncompressed video format made for astronomy, which stores the colour
layout and optional timestamps. Preferred over AVI and MP4.

**Seeing.** The blurring and shifting of the image by turbulence in the atmosphere. It
changes many times a second, which is why frames differ.

**SharpCap sidecar.** The `CameraSettings.txt` file SharpCap writes beside an AVI,
which Helia reads to learn the camera, the Bayer pattern and sometimes the filter.

**Sigma clipping.** Throwing out pixels that are far from the average of the frames
before averaging them.

**SOLARNET.** The metadata recommendations for solar image files, which Helia follows
(partially) in FITS output.

**Stack.** The single picture made by combining the best frames. Also the verb.

**TIFF, PNG.** The picture formats Helia writes: 16-bit TIFF and 8-bit PNG, greyscale
or colour.

**Unsharp mask.** Sharpening by adding back a share of the difference between the
picture and a blurred copy.

**Wavelet layers.** A way of splitting a picture by the size of detail. Layer 1 is the
finest detail (and most of the noise); higher layers are larger structure. Each layer
has an amount, a sharpen and a denoise (a gain and a noise cut).

**Weighted mean.** An average in which each frame counts in proportion to its quality.
