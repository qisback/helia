---
description: "Check an installer from 12hobbies.studio against its checksum before you install it."
---

# Download and verify

**Not sure how to get Helia?** The [Get Helia](download.md) page lists the ways, including
[building from source](build-from-source.md). This page is for checking an installer you downloaded.

This page takes you from nothing to a program you have checked is the one the
project published. It is the first of three: this one, then
[Installation](../user-guide/installation.md) for your system, then
[First launch](first-launch.md).

## A pre-release

Helia is at version 0.x, which the project calls a **pre-release**: the stacking,
sharpening, colour and export described in this manual all work and are tested,
but some things the roadmap promises are not there yet (drizzle is the biggest)
and the interface and file notes can still change
between releases. Nothing is stored or sent
anywhere unless you ask: Helia has no telemetry. See the
[roadmap](../roadmap.md) for what is planned.

## Get the files

Installers are sold on [12hobbies.studio](https://12hobbies.studio/projects/helia/) (coming soon).
Each version comes with the files below. Download the one for your system and its checksum.

| You have | Download | Checksum |
|---|---|---|
| Windows 10 or 11, 64-bit | `helia-<version>-x86_64-pc-windows-msvc-setup.exe` (installer), or the `.zip` | `SHA256SUMS`, or the `.sha256` file beside it |
| Debian, Ubuntu, Raspberry Pi OS | `helia_<version>_amd64.deb` or `helia_<version>_arm64.deb` | `SHA256SUMS` |
| Fedora and other RPM systems | `helia-<version>-1.x86_64.rpm` or `...aarch64.rpm` | `SHA256SUMS` |
| Any Linux | `Helia-<version>-x86_64.AppImage` or `...-aarch64.AppImage`, or the `.tar.gz` | `SHA256SUMS` |
| macOS | coming: `Helia-<version>-macos.dmg` | `SHA256SUMS` |
| Windows on ARM | not published | build from source: [Installation](../user-guide/installation.md#build-from-source) |

Every file, the Windows ones included, has its line in
`SHA256SUMS`. A version also comes with two `*.cdx.json` files, the
lists of libraries inside each program (a software bill of materials). You do not
need them to run Helia.

## Check the download

A checksum is a short fingerprint of a file. If your copy gives the same
fingerprint as the one the project published, the file arrived complete and
unaltered. Do this before you run the program.

On **Windows**, in PowerShell, in the folder you downloaded to (the same works for the
`-setup.exe`):

```powershell
(Get-FileHash helia-0.2.0-x86_64-pc-windows-msvc.zip -Algorithm SHA256).Hash.ToLower()
Get-Content helia-0.2.0-x86_64-pc-windows-msvc.zip.sha256
```

The two lines must match. For version 0.2.0 both read:

```text
27930fae6620307520ebdc4af51e472756934ba7f7d50986f4528bf2f2ac498b
```

(the second line also names the file after the hash). Use the version you
downloaded, not this one.

On **Linux**, in the folder holding the file and `SHA256SUMS` (a `.deb`, `.rpm`, AppImage or
archive):

```sh
sha256sum -c SHA256SUMS --ignore-missing
```

```text
helia-0.2.0-x86_64-unknown-linux-gnu.tar.gz: OK
```

`--ignore-missing` skips the lines for archives you did not download. If you see
`FAILED`, download the file again; if it fails a second time, do not run it, and
tell the project (see [Sending feedback](../walkthrough/feedback.md)).

> The programs are not code-signed yet. Windows SmartScreen shows "Windows
> protected your PC" the first time you run the installer; choose **More info**,
> then **Run anyway**. The checksum above is how you know it is the file the
> project published. See [Installation](../user-guide/installation.md#windows).

## Next

* [Installation](../user-guide/installation.md): the Windows installer, the `.deb`,
  `.rpm` and AppImage, uninstalling, Raspberry Pi notes, and the optional ffmpeg
  for MP4 files.
* [First launch](first-launch.md): what you will see when you start it.
