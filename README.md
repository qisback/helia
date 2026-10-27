<p align="center">
  <img src="assets/branding/helia-logo.png" width="320" alt="Helia logo">
</p>

# Helia

[![CI](https://github.com/qisback/helia/actions/workflows/ci.yml/badge.svg)](https://github.com/qisback/helia/actions/workflows/ci.yml)
[![Documentation](https://img.shields.io/badge/docs-qisback.github.io%2Fhelia-blue)](https://qisback.github.io/helia/)
[![Get Helia](https://img.shields.io/badge/get-the%20store%20or%20build%20from%20source-blue)](https://qisback.github.io/helia/getting-started/download/)
[![Licence: GPL-3.0-or-later](https://img.shields.io/badge/licence-GPL--3.0--or--later-blue)](LICENSE)

<a href="https://ko-fi.com/qisback"><img src="docs/book/src/images/kofi.png" alt="Buy me a coffee at ko-fi.com" height="36"></a>

A modern, open-source solar lucky-imaging stacker for every computer.

Helia takes the video you capture of the Sun (SER, AVI, or MP4 and other
compressed video through the optional ffmpeg) and turns thousands
of seeing-blurred frames into one sharp image: it ranks frame quality, aligns
the disc globally and with a multi-point grid, stacks the best frames and
writes 16-bit TIFF, 8-bit PNG or FITS. It recognises the common solar filters from
the file name, picks sensible settings for them, sharpens the stack (wavelets,
unsharp mask or deconvolution, with a limb guard against ringing) and edits it
non-destructively, and can colour the result for sharing (the stack and FITS stay
mono, and FITS is the unsharpened stack unless you ask). It does the job of tools like AutoStakkert!, with
a modern interface, on Linux, Windows and macOS, on x86_64 and ARM.

## Support

Helia is free software. If it saves you time, you can buy me a coffee at [ko-fi.com/qisback](https://ko-fi.com/qisback).

<a href="https://ko-fi.com/qisback"><img src="docs/book/src/images/kofi.png" alt="Buy me a coffee at ko-fi.com" height="36"></a>

## Dedication

Helia is dedicated to my daughter, the light of my life. Her middle name is
Helia, because she has an energetic spark. She has not stayed still since the
day she was born, which suits software whose whole job is catching the sharp
moments in something that never stops moving.

## Acceleration

Helia runs its heavy maths on whatever GPU you have, through
[wgpu](https://wgpu.rs):

| Platform | GPU API | Hardware |
|---|---|---|
| Linux | Vulkan, GLES fallback | AMD, NVIDIA, Intel, Raspberry Pi 4/5 |
| Windows | DX12, Vulkan | AMD, NVIDIA, Intel, Qualcomm |
| macOS | Metal | Apple silicon, Intel Macs |

With no usable GPU it falls back to a multi-threaded SIMD CPU path that gives
the same results. Optional machine-learning features (planned) run through
ONNX Runtime on CUDA, ROCm, DirectML, OpenVINO or CoreML, with experimental
Coral Edge TPU support.

## Speed

Seconds for `helia stack` (best of several runs on an otherwise idle machine, release build, capture read from the file cache). Rows are machines, columns are captures; a dash means that capture was not run on that machine.

| Machine | 1080p SER, 631 frames | 5.6 GB capture, 2713 frames | 1080p AVI, 630 frames |
|---|---|---|---|
| NVIDIA A100 80 GB (GPU) | 2.90 | 8.46 | - |
| x86, AMD EPYC-Milan, 8 vCPUs (CPU) | 12.58 | 62.21 | - |
| Arm, Ampere Altra, 8 cores (CPU) | 14.68 | - | - |
| Apple M1, Mac mini (CPU) | - | - | 8.8 |
| Apple M1, Mac mini (CPU and Metal, automatic) | - | - | 5.9 |

<img src="docs/book/src/images/perf/platforms.svg" alt="Bar chart of the seconds helia stack takes on six captures, on an NVIDIA A100, an x86 server and an Arm server" width="720">

The full tables, graphs for stages, throughput, cores, frames and memory, the machines and how to reproduce every number are in the
[Performance and benchmarks](https://qisback.github.io/helia/user-guide/benchmarks/) page.

## Status

Early development, versions below 1.0.0 are pre-releases. The first milestone is
the stacking pipeline; see
the [roadmap](https://qisback.github.io/helia/roadmap/) for the plan.

**Feedback is welcome.** The app has a Feedback button (Ctrl+Shift+F) where you can
draw on a screenshot of the window and send a bug, feature request or question to
the Helia issue tracker, and `helia feedback` does the same from a terminal. Nothing is sent
until you press Send, and Helia has no telemetry. See the book's
[Sending feedback](docs/book/src/user-guide/sending-feedback.md) page.

## Documentation

The Helia book is at **<https://qisback.github.io/helia/>**: a user guide
(installation, quick start, capturing, the desktop app, the command line, the API, how
stacking works, GPU acceleration, sending feedback, troubleshooting), step-by-step
tutorials, reference pages and the roadmap. The source is in [`docs/book`](docs/book)
and the site is built with [Zensical](https://zensical.org/):

```sh
python3 -m venv .venv && .venv/bin/pip install -r docs/requirements.txt
.venv/bin/zensical serve
```

The CLI and API references are generated: run `cargo xtask docs` after changing
either.

## Get Helia

Helia is free software under the GPLv3 and the complete source is here. Two ways to get it:

* **Installers** for Windows, Debian and Ubuntu, Fedora and other Linux systems are sold on
  [12hobbies.studio](https://12hobbies.studio/projects/helia/). That page is coming soon while the store
  is set up.
* **Build from source** on Windows, macOS or Linux (including Raspberry Pi). The full guide is the
  [Build from source](https://qisback.github.io/helia/getting-started/build-from-source/) page of the
  documentation, and the short version is:

```sh
git clone https://github.com/qisback/helia.git && cd helia
cargo build --release --locked -p helia-cli -p helia-gui
target/release/helia --help
```

It needs a current stable Rust (see `rust-version` in `Cargo.toml`) and, on Linux, a few development
packages that the guide lists. This repository's releases carry the source and the release notes only.

## Contributing

I develop Helia on an internal repository with an extensive internal CI system, and everything is built and tested there before it reaches GitHub, which is a published mirror. For that reason I am not accepting contributions at the moment: a pull request would have to be pulled back internally, tested, then rebased and pushed out again. Bug reports and feature requests are welcome as GitHub issues. See [CONTRIBUTING.md](CONTRIBUTING.md).

## Supporting Helia

Helia is free software. If it helps your imaging, you can
support its development through the links in [FUNDING.yml](.github/FUNDING.yml).

## Copyright and licence

Copyright (C) 2026 Laurence Horrocks-Barlow.

Helia is free software: you can redistribute it and/or modify it under the
terms of the GNU General Public License as published by the Free Software
Foundation, either version 3 of the License, or (at your option) any later
version.

Helia is distributed in the hope that it will be useful, but WITHOUT ANY
WARRANTY; without even the implied warranty of MERCHANTABILITY or FITNESS FOR A
PARTICULAR PURPOSE. See the GNU General Public License for more details. The
licence text is in [LICENSE](LICENSE), unmodified.

Additional terms under section 7 of the GPL (7(b) and 7(c)) apply to Helia and
to every work based on it: the copyright line and the dedication above must be
preserved, and modified versions must be marked as changed and must not be
presented as the original or as made or endorsed by Laurence Horrocks-Barlow.
They are in [ADDITIONAL-TERMS.md](ADDITIONAL-TERMS.md), and are shown by the
About page and by `--version`. Every source file carries an SPDX header
and `REUSE.toml` covers the rest, following the
[REUSE specification](https://reuse.software/spec/); the IBM Plex fonts keep
their own copyright and the SIL Open Font License.

The methods Helia implements are cited in the manual's [References](docs/book/src/reference/references.md),
and other products are named only to say what they are, with no affiliation: see
[Credits, licences and trademarks](docs/book/src/reference/credits-and-trademarks.md). The licence and
provenance audit is in [docs/LEGAL-AUDIT.md](docs/LEGAL-AUDIT.md).

Helia is made by Laurence Horrocks-Barlow. If you are interested in anything else I make, visit [12hobbies.studio](https://12hobbies.studio).
