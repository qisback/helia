# Changelog

What is new in each release, newest first.

## [0.8.2]

- The store sign-in, licence keys and paid downloads are no longer part of the public source. They live in a separate closed add-on that only the installers from the store include, so the public source tree contains no store code and builds without it.
- Nothing changes in an installer from the store: the same About > Updates settings and the same `helia licence` command.
- A copy built from source now says when a newer version exists and links to the Helia page. Before, it could not even tell you, because the signed update information was refused as a whole.
- The About page and `helia --version` name the update backend when an installer includes one.
- Fixed: attaching the macOS installer to a release no longer stops after the upload.

## [0.8.1]

- Helia can now install ffmpeg for you. ffmpeg is what opens MP4 and other video captures. Open a video without it and Helia offers an Install ffmpeg page that shows the provider, the download size and the whole licence first; `helia tools install ffmpeg` does the same from the terminal. Nothing is downloaded until you accept.
- The About page now lists the optional tools Helia has installed, with their licences.
- Fixed: the prominence layer no longer flattens bright solar prominences into plates.
- Fixed on Windows: `helia update apply --yes` now really installs the new version. Before, it reported success and installed nothing.

## [0.8.0]

- Plugins: Helia can be extended with extra processing steps. Use them from the command line and the API, or from the new Plugins page in the desktop app, which lists what Helia found and lets you switch each plugin on or off.
- Updates: Helia can check once a day for a new version and tell you about it (the default), or install it for you when you ask. Updates are signed and checked before they install. An optional store sign-in gives subscribers automatic updates.
- Seestar and FITS: `helia stack` and `helia info` now read FITS frames, including a folder of Seestar sub-frames. Seestar S50 frames come out the right way up (`--row-order` lets you choose), and a stack of subs from an alt-azimuth mount no longer shows trailed stars.
- Solar fixes: no more dark ring just outside the limb, flat-edged slabs or a comb of teeth in the prominence layer, and the burned-in text mask no longer paints a bright patch on a capture with no text.
- `helia edit` with no options now converts an image to the format of its output name, and `helia stack` tells you when a capture does not look like the Sun.
- The GitHub repository carries the source; installers are sold at 12hobbies.studio. A new Download page opens the manual, and a Build from source page explains how to build Helia yourself.

## Earlier releases

- Helia is open-source software for stacking, aligning and sharpening solar captures. It reads SER, AVI, FITS and (through ffmpeg) MP4 and other video, and comes as a command-line tool, an HTTP API and a desktop app.
- Alignment and stacking: limb-fit global alignment, automatic alignment points, robust stacking that removes artefacts, dark, flat and bad-pixel calibration, correction for the Sun's rotation, and time-lapse.
- Output and measurement: sharpening and adjustments, a prominence layer, limb-darkening fit and flatten, SOLARNET-compliant FITS output, and quality and science plots, with optional GPU acceleration.
- It runs on Windows, Linux and macOS. The manual covers installation, every feature, tutorials on real captures and a performance guide.
- You can send feedback from inside the app, and the manual includes a Why I made Helia page.
