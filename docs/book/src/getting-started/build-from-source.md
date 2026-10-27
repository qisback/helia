---
description: "Build Helia from source on Windows, macOS, Debian, Ubuntu, Fedora, Arch, Raspberry Pi and any other Linux: toolchain, system packages, build, check, install, optional packages and troubleshooting."
---

# Build from source

Helia is free software under the GPLv3 and the whole source is public, so you can build it
yourself on any supported system. This page takes you from nothing to working `helia` and
`helia-gui` programs. If you would rather not build, ready-made installers are sold on
[12hobbies.studio](https://12hobbies.studio/projects/helia/) (that page is coming soon while the
store is set up); see [Get Helia](download.md).

You get two programs:

* `helia`, the command line tool (which also runs the API server);
* `helia-gui`, the desktop app.

## What you need

| Need | Detail |
|---|---|
| Rust | A current stable toolchain, at least the `rust-version` in the root `Cargo.toml` (1.95 for 0.7). Install it with [rustup](https://rustup.rs). |
| A C toolchain and linker | Provided by the system packages below. |
| The source | A git clone, or the source archive of a tag. |
| A GPU (optional) | Helia uses [wgpu](https://wgpu.rs): DX12 or Vulkan on Windows, Vulkan on Linux, Metal on macOS. Without a working driver it runs on the CPU. |
| Disk and time | A release build needs a few gigabytes of disk for `target/`. The first build compiles every dependency, so it is much slower than later ones. |

Get the source:

```sh
git clone https://github.com/qisback/helia.git
cd helia
```

To build a released version, check out its tag afterwards (`git tag` lists them), or download the
source archive of that tag from the repository's releases page.

## Windows 10 and 11

1. Install the **Visual Studio Build Tools** (2022 or newer) with the workload "Desktop development
   with C++". Rust needs the Microsoft linker and the Windows SDK. For a Windows on Arm machine also
   tick the "MSVC ARM64 build tools" component.
2. Install Rust with `rustup-init.exe` from [rustup.rs](https://rustup.rs). The default host is
   `x86_64-pc-windows-msvc`; on Windows on Arm choose `aarch64-pc-windows-msvc`. The project's own
   builds and tests are x86-64; the Arm build is expected to work but is not tested by the project.
3. Install [Git for Windows](https://git-scm.com/download/win) and clone the source as above.
4. In a terminal, in the checkout:

```powershell
cargo build --release --locked -p helia-cli -p helia-gui
```

The programs are `target\release\helia.exe` and `target\release\helia-gui.exe`. Your graphics driver
provides DX12 or Vulkan. If a corporate policy blocks programs built inside the checkout (Windows
Application Control can refuse build scripts), point `CARGO_TARGET_DIR` at a folder the policy allows.

## macOS (Apple silicon and Intel)

1. Install the Xcode command line tools: `xcode-select --install`.
2. Install Rust with rustup (`curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh`).
3. Build:

```sh
cargo build --release --locked -p helia-cli -p helia-gui
```

Helia uses Metal, which needs no extra setup. The programs are `target/release/helia` and
`target/release/helia-gui`. The test suite passes on an Apple M1 (see [Run the tests](#run-the-tests)).
Programs you build yourself are not signed or notarised; on your own machine that is not a problem,
and to share the app with other Macs see [Optional: make packages](#optional-make-packages).

## Debian, Ubuntu, Raspberry Pi OS and Linux Mint

```sh
sudo apt-get update
sudo apt-get install build-essential pkg-config git curl \
    libvulkan1 mesa-vulkan-drivers \
    libxkbcommon-dev libwayland-dev libx11-dev libxcursor-dev libxrandr-dev libxi-dev
```

`mesa-vulkan-drivers` covers AMD, Intel and the Raspberry Pi. NVIDIA owners use NVIDIA's own driver
instead. These are the packages the project's own builds install, on Debian 12 and Ubuntu 22.04 and
24.04. Then install Rust with rustup and build:

```sh
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
. "$HOME/.cargo/env"
cargo build --release --locked -p helia-cli -p helia-gui
```

On a **Raspberry Pi** use a 64-bit operating system (Raspberry Pi OS 64-bit or Ubuntu for Pi). The
project's automated tests run on 64-bit Arm Linux with a software GPU, not on Raspberry Pi hardware,
so report what you find. Compiling on a Pi is slow and needs plenty of memory; add swap, or
cross-compile from a PC.

## Fedora, RHEL and relatives

```sh
sudo dnf install gcc gcc-c++ make pkgconf-pkg-config git curl \
    vulkan-loader mesa-vulkan-drivers \
    libxkbcommon-devel wayland-devel libX11-devel libXcursor-devel libXrandr-devel libXi-devel
```

Then install Rust with rustup and build as above. These are the Fedora names of the Debian packages
listed above; the project's own builds run on Debian and Ubuntu, so tell me if a name differs on your
release.

## Arch Linux and relatives

```sh
sudo pacman -S --needed base-devel git rustup vulkan-icd-loader mesa \
    libxkbcommon wayland libx11 libxcursor libxrandr libxi
rustup default stable
```

Install the Vulkan driver for your card too (`vulkan-radeon`, `vulkan-intel` or `nvidia-utils`). Then
build as above. As for Fedora, the names are the Arch equivalents of the Debian list.

## Any other Linux

You need a C compiler and linker, `pkg-config`, git, and the development files for xkbcommon,
Wayland, X11 (libX11, libXcursor, libXrandr, libXi) and the Vulkan loader, plus a Vulkan driver for
your GPU. Find the equivalents of the Debian packages above in your distribution, then build with
rustup's Rust. The command line tool alone (`-p helia-cli`) needs no windowing libraries.

## Build

```sh
cargo build --release --locked -p helia-cli -p helia-gui   # the two programs
cargo build --release --locked                             # every crate in the workspace
```

`--locked` builds exactly the dependency versions in `Cargo.lock`. The first build takes a while and
later ones are quick. The workspace members are `crates/*`, `xtask` and an example plugin; the `fuzz`
crate is outside the workspace because it needs nightly Rust.

## Check it works

```sh
target/release/helia --version
target/release/helia info path/to/capture.ser
target/release/helia-gui
```

On Windows use `target\release\helia.exe`. To try Helia without a capture, write a synthetic one with
`cargo xtask sample`, described in the [Quick start](../user-guide/quick-start.md). The top bar of
the desktop app says `GPU` or `CPU`; if you expected a GPU, see [GPU acceleration](../user-guide/gpu.md).

## Install the programs

Copy the two programs somewhere on your `PATH`, or let cargo do it:

```sh
cargo install --locked --path crates/helia-cli
cargo install --locked --path crates/helia-gui
```

That puts them in `~/.cargo/bin` (`%USERPROFILE%\.cargo\bin` on Windows). To remove them later,
`cargo uninstall helia-cli helia-gui`. Settings are kept apart from the programs, in `~/.config/helia`
(`%APPDATA%\helia` on Windows); delete that folder if you want them gone.

## Run the tests

```sh
HELIA_GPU_TESTS=software cargo test --workspace --all-features
```

`HELIA_GPU_TESTS` chooses the graphics adapter the GPU tests use: `software` (the fallback adapter:
WARP on Windows, lavapipe on Linux; install `mesa-vulkan-drivers`), `hardware` (your GPU), or unset to
skip them. On macOS there is no software adapter, so use `hardware`. In PowerShell set the variable
first: `$env:HELIA_GPU_TESTS = "software"`.

## Optional: ffmpeg for MP4 and other video

Helia reads SER, FITS and uncompressed AVI by itself. For MP4, MOV, MKV and WebM files it uses the free
`ffmpeg` and `ffprobe` (version 4.4 or newer) if they are on your `PATH`. Helia never bundles or links
ffmpeg. See [Installation](../user-guide/installation.md#optional-ffmpeg-for-mp4-and-other-video).

## Optional: make packages

The scripts the project uses for its own installers are in `packaging/`, for people who want to
distribute their own build. They need the release binaries and the licence summary produced by
`cargo about` (see `packaging/about.toml`). Read each script's header for its arguments.

| Package | Script | Needs |
|---|---|---|
| Linux archive (`.tar.gz`) | `packaging/package-linux.sh` | a Linux shell, `tar`, `gzip` |
| `.deb`, `.rpm`, AppImage | `packaging/linux/build-deb.sh`, `build-rpm.sh`, `build-appimage.sh` | Debian or Ubuntu with `rpm`, `desktop-file-utils` and `appstream` for the others |
| Windows installer | `packaging/windows/build-installer.ps1` | PowerShell; fetches a pinned NSIS if none is installed |
| macOS app and disk image | `packaging/macos/build-app.sh` | a Mac (`hdiutil`, `plutil`) |

Whoever distributes a build must keep the copyright line and the dedication and mark modified
versions as changed: the additional terms are in [`ADDITIONAL-TERMS.md`](https://github.com/qisback/helia/blob/main/ADDITIONAL-TERMS.md).

## If the build fails

| What you see | What it means and what to do |
|---|---|
| `error: package requires rustc 1.95 or newer` | Your Rust is too old. Run `rustup update stable`. |
| `linker 'cc' not found` or `link.exe not found` | No C toolchain. Install `build-essential` (Linux), the Xcode tools (macOS) or the Visual Studio C++ Build Tools (Windows). |
| `The system library 'xkbcommon' required by crate ... was not found` (or `wayland-client`, `x11`) | A development package is missing. Install the `-dev` or `-devel` packages listed for your system. |
| `Could not find pkg-config` | Install `pkg-config` (`pkgconf-pkg-config` on Fedora). |
| `Blocked by Application Control` on Windows | Policy refuses build scripts in the checkout. Set `CARGO_TARGET_DIR` to an allowed folder. |
| The build is killed or the machine freezes | Out of memory while linking. Close other programs, add swap, or build with `CARGO_BUILD_JOBS=2`. |
| Tests report no GPU adapter | With `HELIA_GPU_TESTS=software` or `hardware` a missing adapter is a failure by design. Install a Vulkan driver or unset the variable. |
| The window opens but the top bar says `CPU` | No Vulkan driver was found. Install `libvulkan1` and a driver. See [GPU acceleration](../user-guide/gpu.md). |

If something here is wrong or missing for your system, open an issue on the
[GitHub repository](https://github.com/qisback/helia/issues).
