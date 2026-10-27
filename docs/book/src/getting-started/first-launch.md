# First launch

You have downloaded, checked and installed or unpacked Helia (see
[Download and verify](download-and-verify.md) and
[Installation](../user-guide/installation.md)). This page is what happens next.

## Check the programs run

Helia is two programs, `helia` (the command line tool) and `helia-gui` (the
app). After a Debian, Ubuntu or Fedora package they are in `/usr/bin`, so in any
new terminal:

```sh
helia --version
helia-gui --version
```

From the Windows installer, `helia` is only on the PATH if you ticked **Add the
helia command to PATH** (and opened a new terminal afterwards). Otherwise run it by
its folder, by default `%LOCALAPPDATA%\Programs\Helia`:
`& "$env:LOCALAPPDATA\Programs\Helia\helia.exe" --version`. From the zip or a
tarball, run `.\helia.exe --version` or `./helia --version` in the folder you
unpacked. From an AppImage, `./Helia-<version>-x86_64.AppImage --cli --version`
runs the command line tool and the plain file runs the app.

Each program prints its version, when and how it was built and the licence. Neither
opens a window; this is the quick test that the program starts on your machine. On
Windows `helia-gui.exe` is a window program with no console of its own, so in
PowerShell its text can arrive after the next prompt has appeared; `helia.exe` is
the better tool for this test. A line such as `wgpu: 30.0.1` is the graphics
library Helia was built with, not a requirement on you.

## Start the desktop app

Choose **Helia** in the Start menu (Windows installer) or the applications menu
(`.deb` and `.rpm`), double-click `helia-gui.exe` or the AppImage, or run
`helia-gui` in a terminal. The first start after installing from Windows can show
the SmartScreen warning described in
[Installation](../user-guide/installation.md#windows). A window opens
with the nine steps (and two optional ones, Time-lapse and Mosaic) down the left and a dark, empty viewer. The top bar offers
**About**, **Open capture** and **Export**.

If Helia finds a graphics card it says so in the top bar, for example
`Vulkan` and the adapter's name. If it finds none, it says `CPU` and works anyway; the answers
are the same, only slower for sharpening previews. See
[GPU acceleration](../user-guide/gpu.md).

## The first-run strip

The first time, a **Quick start** strip lists the three steps: open a capture,
press Stack with recommended settings, pick a colour and export. Press **Got it**
and it never returns. Helia remembers that, and the look you choose for each
filter, in a small settings file (see
[Settings file](../user-guide/gui.md#settings-file)). Nothing else is saved, and
nothing is sent over the network.

## Open your first capture

Press Ctrl+O, drop a file on the window, pick one under **Recent** in the top
bar, or right-click a `.ser`, `.avi` or `.fits` file and choose **Open with**,
**Helia** (or run `helia-gui capture.ser`). Helia remembers the size of its window
and the last eight captures. If you have no capture to hand, any
SER or AVI of the Sun will do, or a short phone MP4 if you have installed ffmpeg.
From here, follow the [step-by-step walkthrough](../walkthrough/index.md): it goes
from the Open step to a finished picture with a screenshot at every step.

If you prefer a terminal, the [Quick start](../user-guide/quick-start.md) shows
the same journey on the command line, and
[the recipes](../recipes/h-alpha-full-disc.md) are short, complete jobs for the
common captures.
