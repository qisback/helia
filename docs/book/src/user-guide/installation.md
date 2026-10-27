---
description: "Install Helia on Windows, macOS, Debian, Ubuntu, Fedora or any Linux: installers, packages and AppImages, with checksums and uninstall steps."
---

# Installation

**Not sure how to get Helia?** The [Get Helia](../getting-started/download.md) page lists the ways.

There are two ways to get Helia: install a ready-made installer or package (Windows, Debian and Ubuntu,
Fedora, any Linux through an AppImage), sold on [12hobbies.studio](https://12hobbies.studio/projects/helia/)
(coming soon), or [build it from source](../getting-started/build-from-source.md) (every platform, including
macOS). The rest of this page describes what an installer or package does on your system.

You get two programs:

* `helia`, the command line tool (which also runs the API server);
* `helia-gui`, the desktop app.

## The installers and packages

Each version comes with an installer or package for your system, the plain archives and a
`SHA256SUMS` file. Pick **one** of the rows that matches your system.

| Platform | Install with | File | Status |
|---|---|---|---|
| Windows 10 or 11, 64-bit | the installer | `helia-<version>-x86_64-pc-windows-msvc-setup.exe` | available |
| Windows | unzip and run | `helia-<version>-x86_64-pc-windows-msvc.zip` | available |
| Debian, Ubuntu, Raspberry Pi OS, Mint | `.deb` | `helia_<version>_amd64.deb`, `helia_<version>_arm64.deb` | available |
| Fedora, RHEL and relatives | `.rpm` | `helia-<version>-1.x86_64.rpm`, `helia-<version>-1.aarch64.rpm` | available |
| Any Linux with glibc 2.35 or newer | AppImage | `Helia-<version>-x86_64.AppImage`, `Helia-<version>-aarch64.AppImage` | available |
| Any Linux | unpack the archive | `helia-<version>-<arch>-unknown-linux-gnu.tar.gz` | available |
| macOS | disk image | `Helia-<version>-macos.dmg` | [coming](#macos) |
| Windows ARM64 | | | not published; build from source |

[Download and verify](../getting-started/download-and-verify.md)
shows how to check a download against its checksum before you run it.

### Windows

Run `helia-<version>-x86_64-pc-windows-msvc-setup.exe`.

**The first run shows a SmartScreen warning.** The installer is not code-signed
yet, so Windows
says "Windows protected your PC" and "Unknown publisher". After you have checked
the file's checksum, choose **More info**, then **Run anyway**. The warning goes
away once the project has a signing certificate.

The installer:

1. shows the GPL and the additional terms that go with it (you must read, not
   sign: Helia is free software and the pages are there so you know what you are
   getting);
2. asks **Who is this for?** The default, **Just for me**, installs to
   `%LOCALAPPDATA%\Programs\Helia` and **needs no administrator rights**. **All
   users** installs to `C:\Program Files\Helia`; if you are not an administrator
   the installer asks Windows for permission and starts again with the choice made,
   so the licence pages show a second time;
3. offers these parts, ticked as shown:

   | Part | Default | What it does |
   |---|---|---|
   | Helia | always | `helia-gui.exe` (the app), `helia.exe` (the command line tool), the licences |
   | Start menu shortcut | on | Helia in the Start menu |
   | Desktop shortcut | off | an icon on the desktop |
   | Add the helia command to PATH | off | lets you type `helia` in any new terminal |
   | Open with Helia | on | lists Helia under **Open with** for `.ser`, `.avi` and `.fits` files. It never becomes the default program for them |

Helia then appears in **Settings > Apps > Installed apps** with its version and
publisher, and uninstalls from there. Double-clicking `helia-gui.exe` opens the
window with no console behind it; `helia.exe` is the command line tool and stays
a console program.

To install for a whole lab without the pages, from a terminal:

```powershell
.\helia-0.7.1-x86_64-pc-windows-msvc-setup.exe /S /ADDPATH /DESKTOP
```

The switches are `/S` (silent), `/AllUsers` (needs an administrator), `/ADDPATH`,
`/DESKTOP`, `/NOSTART`, `/NOASSOC` and `/D=C:\Where\To\Install` (last, with no
quotes). Without `/AllUsers` the install is for the current user and needs no
administrator rights, even when silent. With `/AllUsers` from a prompt that is not
elevated the installer stops with exit code 740 and installs nothing; run it from
an administrator prompt. The start menu shortcut and **Open with** are on unless you
pass `/NOSTART` or `/NOASSOC`; the desktop shortcut and the PATH entry are off
unless you pass `/DESKTOP` or `/ADDPATH`.

**Uninstall:** Settings > Apps > Installed apps > Helia > Uninstall, or run
`uninstall.exe` in the install folder with `/S`. For an all-users install add
`/AllUsers` and run it from an administrator prompt (`uninstall.exe /AllUsers /S`);
the entry in Settings does that for you. It removes the programs, the
shortcuts, the **Open with** entries and the PATH entry. It leaves your settings
(`%APPDATA%\helia`) and your images alone; delete the folder if you want them
gone.

If you prefer no installer, unzip the `.zip` anywhere and run `helia-gui.exe`.

**If something goes wrong on Windows**

| What you see | What it means and what to do |
|---|---|
| "Windows protected your PC", "Unknown publisher" | SmartScreen, because the installer is not code-signed yet. Check the checksum, then **More info** > **Run anyway**. If the button is missing, **More info** has not been pressed yet. |
| "This app has been blocked by your organisation", or SmartScreen offers no way to run it | A policy on a managed PC. Ask your administrator, or use the `.zip`, which needs no installer. |
| "Installing for all users needs administrator rights" (exit code 740 when silent) | You passed `/AllUsers` without elevation. Run the installer from an administrator prompt, or leave out `/AllUsers`. |
| "Error opening file for writing: helia-gui.exe" | Helia is running, so Windows will not replace the file. Close it, then choose Retry. |
| `helia` is "not recognised" in a terminal | The PATH entry is off by default and a terminal that was open during the install keeps its old environment. Open a new terminal; if it still fails, run the installer again and tick **Add the helia command to PATH**, or call `helia.exe` by its full path. |
| Helia is missing from **Open with** | The **Open with Helia** part was unticked (or `/NOASSOC` was used). Run the installer again with it ticked. Helia never becomes the default program for the files: choose it under **Open with**, then **Choose another app** if the first list does not show it. |
| Helia is not under Installed apps | An all-users install is listed for every user, a current-user install only for the account that ran it. Look under the account that installed it. |
| The uninstaller left a folder | It removes only the files it installed. Your settings (`%APPDATA%\helia`) and any file you added to the install folder stay; delete the folder by hand. |

### Debian, Ubuntu and Raspberry Pi OS

```sh
sudo apt install ./helia_0.7.1_amd64.deb      # arm64 on a Raspberry Pi
```

(`apt install ./file.deb` also pulls in the packages Helia recommends, such as
the Vulkan loader and Mesa drivers; plain `dpkg -i` does not.) You get `helia`
and `helia-gui` in `/usr/bin`, a **Helia** entry in your applications menu with
its icon, and Helia in the **Open with** list for SER, AVI and FITS files.
Install `ffmpeg` too if you want to open MP4 files.

**Uninstall:** `sudo apt remove helia`. (`sudo apt purge helia` is the same; the
package keeps no configuration. Your settings are in `~/.config/helia`.)

**If something goes wrong on Debian and relatives**

* `dpkg: dependency problems` or `libc6 (>= 2.35) but ... is installed`: the system
  is too old (Ubuntu 20.04 and Debian 11 are). Build from source, or use a newer
  system.
* `Unable to locate package ./helia_...`, or `Unsupported file ... given on
  commandline`: `apt install` needs the path to start with `./` (or be absolute), and
  the file must be the one for your CPU (`amd64` on a PC, `arm64` on a Raspberry Pi
  with a 64-bit system; `dpkg --print-architecture` tells you).
* `helia: command not found` right after installing: open a new terminal. The programs
  are in `/usr/bin`, so a fresh shell finds them.
* No **Helia** entry in the applications menu, or it is not under **Open with**:
  log out and in, or run `sudo update-desktop-database` and `sudo update-mime-database
  /usr/share/mime`. The package installs the desktop file and a MIME type for SER
  files (`video/x-astro-ser`); AVI and FITS are known to the system already.
* The window opens but the top bar says `CPU`: no Vulkan driver was found. Install
  `libvulkan1` and `mesa-vulkan-drivers`, or NVIDIA's driver. See
  [GPU acceleration](gpu.md).

### Fedora and other RPM systems

```sh
sudo dnf install ./helia-0.7.1-1.x86_64.rpm   # aarch64 on ARM
```

It installs the same files in the same places, and the troubleshooting notes
for the `.deb` apply, with `sudo dnf install ./file.rpm` for `apt install ./file.deb`
and the architectures `x86_64` and `aarch64` (`uname -m` tells you which).
**Uninstall:** `sudo dnf remove helia`. The RPM is built for glibc 2.35 or newer, which every supported Fedora
has. It has not been installed on a Fedora machine by the project's own tests
(there is none), only unpacked and checked on Debian; report what you find.

### AppImage

One file, no installation, runs on most Linux desktops:

```sh
chmod +x Helia-0.7.1-x86_64.AppImage
./Helia-0.7.1-x86_64.AppImage                  # the app
./Helia-0.7.1-x86_64.AppImage --cli --version  # the command line tool
```

Pass a capture to open it: `./Helia-0.7.1-x86_64.AppImage capture.ser`. If it
says it cannot mount itself, your system lacks FUSE; run it with
`APPIMAGE_EXTRACT_AND_RUN=1` or install `libfuse2`. An AppImage does not add a
menu entry by itself; tools such as AppImageLauncher do that, or copy the
`.desktop` file from the [tarball](#the-plain-archives). **Uninstall:** delete the file.

**If something goes wrong with the AppImage**

* `Permission denied`: the file is not executable. `chmod +x` it; some browsers
  and file managers drop the permission.
* It starts and stops at once with a message about `fusermount` or `libfuse.so.2`:
  see the FUSE note above (`APPIMAGE_EXTRACT_AND_RUN=1` or `libfuse2`).
* It will not run from a folder: a few systems mount `/tmp` or the home folder with
  `noexec`, which stops AppImages and extracted copies. Put the file in a folder
  that is not mounted that way.
* The command line tool is the same file with `--cli` first:
  `./Helia-0.7.1-x86_64.AppImage --cli stack capture.ser -o out.tif`.
* Open with is not set up: an AppImage does not register file types. Pass the capture
  on the command line, or integrate the AppImage with a tool such as AppImageLauncher.

### macOS

**A macOS disk image**, `Helia-VERSION-macos.dmg`, is not available yet; build from
source as described [below](#build-from-source). The disk image holds `Helia.app`,
universal for Apple silicon and Intel, with `.ser`, `.avi` and `.fits` listed
under Open With.

The app is not signed or notarised until the project has an Apple
Developer ID, so macOS Gatekeeper will say Helia "cannot be opened because the
developer cannot be verified". To open it the first time: drag Helia to
Applications, **right-click (or Control-click) Helia and choose Open**, then
**Open** again in the dialog. Or open **System Settings > Privacy & Security**,
scroll to the message about Helia, and choose **Open Anyway**. You only do this
once. **Uninstall:** drag Helia to the Bin; settings are in
`~/.config/helia`.

### The plain archives

The `.tar.gz` and `.zip` archives hold the same programs without an installer, for
people who want to put them somewhere themselves:

```sh
# in the folder you downloaded the archive and SHA256SUMS to
sha256sum -c SHA256SUMS --ignore-missing
tar -xzf helia-<version>-x86_64-unknown-linux-gnu.tar.gz
cd helia-<version>-x86_64-unknown-linux-gnu
./helia --version
./helia-gui
```

Each archive holds `helia`, `helia-gui`, the licences, this project's README and
changelog, and `THIRD-PARTY-LICENSES.txt` for the libraries inside. The Linux one
also has `share/applications/helia.desktop` and the icons under `share/icons`;
copy `share/` into `~/.local` to get a menu entry.

### Things to know about the Linux downloads

* They need **glibc 2.35 or newer**: Ubuntu 22.04, Debian 12, Raspberry Pi OS
  (Bookworm), Fedora 36 or anything newer. On an older system, build from source.
* They do not bundle a graphics driver. Install the Vulkan loader and a driver
  (`libvulkan1` and `mesa-vulkan-drivers`, or NVIDIA's driver); without one
  Helia uses the CPU. The `.deb` and `.rpm` recommend them. The desktop app also
  needs the usual X11 or Wayland and `libxkbcommon` libraries, which a desktop
  installation already has.
* The aarch64 packages are built and tested natively on a 64-bit ARM server (see
  [Linux on 64-bit ARM](#linux-on-64-bit-arm)), not on Raspberry Pi hardware.
  Report what you find.
* Each release also carries two CycloneDX SBOM files (`*.cdx.json`) listing the
  libraries each program was built from. Every package carries the licences:
  `/usr/share/doc/helia` in the `.deb` and `.rpm`, inside the AppImage, in the
  install folder on Windows.

### Linux on 64-bit ARM

The `arm64` and `aarch64` downloads are built and tested natively on a 64-bit ARM
machine (an Ampere server, not an emulator), against the same glibc 2.35 baseline as
the x86-64 ones. They are meant for:

* Raspberry Pi 5 (and Pi 4) running a 64-bit Raspberry Pi OS or Ubuntu;
* Ampere and AWS Graviton servers, for headless `helia` runs;
* arm64 laptops and mini PCs running Linux (Snapdragon X, Apple silicon under Asahi).

The release checks run each binary from the `.deb`, `.rpm` and AppImage, and install the
`.deb` in a clean Debian 12 arm64 container. They use a software GPU (Mesa lavapipe), so
GPU speed on a Pi's own graphics chip is not measured: report what you find.

### Open a capture from your file manager

With the installer or a package you can right-click a `.ser`, `.avi` or `.fits`
file and choose **Open with > Helia**. The same works from a terminal:
`helia-gui capture.ser`. Helia remembers the size of its window and, under
**Recent** in the top bar, the last eight captures you opened.

## Optional: ffmpeg for MP4 and other video

Helia reads SER and uncompressed AVI by itself. To open **MP4, MOV, M4V, MKV and
WebM** files, and AVI files that use a compressed codec (MJPEG, H.264), it uses
the free **ffmpeg** program if it is installed. Nothing else changes if it is
not: Helia never bundles or links ffmpeg, and without it those files are refused
with a message that says how to install it.

You need `ffmpeg` and `ffprobe` (they come together) at **version 4.4 or newer**.

| System | Install |
|---|---|
| Windows | `winget install Gyan.FFmpeg` (or `choco install ffmpeg`), then open a new terminal |
| macOS | `brew install ffmpeg` |
| Debian, Ubuntu, Raspberry Pi OS | `sudo apt install ffmpeg` |
| Fedora | `sudo dnf install ffmpeg` (enable RPM Fusion first on Fedora) |
| Anything else | a static build from [ffmpeg.org/download.html](https://ffmpeg.org/download.html) |

Check it with `ffmpeg -version`. Helia finds the programs on your `PATH`. To use
others, set `HELIA_FFMPEG` and `HELIA_FFPROBE` to their full paths. The
`helia info` command on a video file is a quick test that it all works, and
[Troubleshooting](troubleshooting.md#ffmpeg-was-not-found) lists what the
errors mean.

Decoded frames are written to a cache directory (the operating system's cache
folder by default, `--cache-dir` or `HELIA_CACHE_DIR` to choose another), and
removed when Helia is done with them. See [Capturing](capturing.md#when-an-mp4-is-fine)
for why raw SER or AVI is still better.

## Build from source

The full guide, for Windows, macOS, Debian, Ubuntu, Raspberry Pi, Fedora, Arch and other Linux
systems, is on its own page: [Build from source](../getting-started/build-from-source.md). In short,
install a current stable Rust and the system packages listed there, then:

```sh
cargo build --release --locked -p helia-cli -p helia-gui
```
