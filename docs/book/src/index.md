---
description: "Helia is a free, open-source solar lucky-imaging stacker: it ranks the sharpest frames of a Sun video, aligns and stacks them, then sharpens and colours the result on your GPU."
---

# Introduction

<img src="images/helia-logo.png" width="320" alt="Helia logo">

<a href="https://ko-fi.com/qisback"><img src="images/kofi.png" alt="Buy me a coffee at ko-fi.com" width="143" height="36"></a>

Helia is a free, open-source stacker for pictures of the Sun. You point a
telescope and a camera at the Sun and record a video, a few thousand frames
long. Each frame is blurred a little differently by the air above you, because
the atmosphere is never still. Helia ranks the frames by sharpness, lines them
up (the whole disc, and many small patches of it, because the air bends the
image differently across the Sun), and averages the best ones into one sharp
picture, written as a 16-bit TIFF, an 8-bit PNG or a FITS file. It knows the common
solar filters (H-alpha, calcium K and H, magnesium, sodium, helium D3, H-beta and
white light), picks sensible settings for them, and can colour the result for
sharing.

This technique is called *lucky imaging*. It does the job of tools such as
AutoStakkert!, in a single program that runs on Linux, Windows and macOS, on
x86_64 and ARM, and does its heavy maths on whatever GPU you have.

Helia is written in Rust. It comes as three front ends over the same engine:

* **`helia-gui`**, a desktop app that walks you through the steps. See
  [Using the desktop app](user-guide/gui.md).
* **`helia`**, a command line tool for scripts and batch work. See
  [The command line](user-guide/cli.md).
* **`helia serve`**, an HTTP and WebSocket API for notebooks and other
  programs. See [The API](user-guide/api.md).

## Where to start

* **Wondering why it exists?** [Why I made Helia](why-i-made-helia.md) is the short story:
  an HPC habit of automating, queuing, reviewing and resubmitting, turned into a program.
* **How fast is it?** [Performance and benchmarks](user-guide/benchmarks.md) has graphs and tables
  for the CPU, the GPU and several platforms, and the script that makes them.
* **New to Helia?** [Get Helia](getting-started/download.md) (installers, or build from source),
  then [Download and verify](getting-started/download-and-verify.md),
  [Installation](user-guide/installation.md) and [First launch](getting-started/first-launch.md).
* **Want to see every step?** The [step-by-step walkthrough](walkthrough/index.md)
  goes from a raw capture to a finished picture with a screenshot at each step.
* **Have a particular capture?** The [recipes](recipes/h-alpha-full-disc.md) cover
  H-alpha, white light, calcium K, phone video, a Bayer grid, batches and scripting.
* **Something wrong?** [Troubleshooting](user-guide/troubleshooting.md) and the
  [FAQ](user-guide/faq.md).

## Copyright and licence

Copyright (C) 2026 Laurence Horrocks-Barlow. Helia is free software under the
GNU General Public License, version 3 or (at your option) any later version,
with no warranty, and with the additional terms in `ADDITIONAL-TERMS.md` that
keep the author's name and this dedication with the program; the About page of
the desktop app, `helia --version` and the
[contributing notes](https://github.com/qisback/helia/blob/main/CONTRIBUTING.md) have the details. Helia is made by Laurence Horrocks-Barlow. If you are interested in anything else I make, visit [12hobbies.studio](https://12hobbies.studio).

## Dedication

Helia is dedicated to my daughter, the light of my life. Her middle name is
Helia, because she has an energetic spark. She has not stayed still since the
day she was born, which suits software whose whole job is catching the sharp
moments in something that never stops moving.

## Status

Helia is in early development. The stacking pipeline, the command line, the
desktop app and the API work today, with sharpening, deconvolution and everyday
adjustments, and it applies dark and flat frames and a bad-pixel map before it analyses anything. Drizzle and the other planned features are listed in the [Roadmap](roadmap.md), and each
page of this guide says plainly when something is not available yet. The plan
and every decision taken so far live in
the project plan.

Installers for Windows and Linux are sold on [12hobbies.studio](https://12hobbies.studio/projects/helia/)
(coming soon); on every system, including macOS, you can
[build Helia from source](getting-started/build-from-source.md). See
[Installation](user-guide/installation.md).

## Supported platforms and GPUs

Helia runs its heavy maths through [wgpu](https://wgpu.rs), which talks to
whatever graphics API your system has:

| Platform | GPU API | Hardware |
|---|---|---|
| Linux | Vulkan, GLES fallback | AMD, NVIDIA, Intel, Raspberry Pi 4/5 |
| Windows | DX12, Vulkan | AMD, NVIDIA, Intel, Qualcomm |
| macOS | Metal | Apple silicon, Intel Macs |

With no usable GPU, Helia falls back to a multi-threaded CPU path that gives
the same results. [GPU acceleration](user-guide/gpu.md) explains how the choice
is made and what to do when your GPU is not found.

Optional machine-learning features are planned (they would run through ONNX
Runtime on CUDA, ROCm, DirectML, OpenVINO or CoreML, with experimental Coral
Edge TPU support). None of that exists yet.

## How this book is organised

* The **User guide** takes you from installing Helia to a finished image, and
  explains what each setting does.
* The **Reference** pages list every option. The command line and API
  references are generated from the code, so they cannot drift from it.
* The **Developer guide** covers the architecture, how to build and test, and
  how to contribute.
* The **Roadmap** lists what is planned, with links to the issues.

Helia is licensed under GPL-3.0-or-later. If it helps your imaging, you can
support its development through the links in `.github/FUNDING.yml` in the
repository.
