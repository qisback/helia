# Package-manager channels: what is ready, and how to submit

Nothing here has been submitted. Putting Helia in a store or a package index is an outward
action and needs the owner's yes. Everything below is prepared from the real 0.7.1 release by
`python3 packaging/package_managers.py` (run it again after each release; it only writes
files, and `python3 -m unittest discover -s packaging` checks it).

| Channel | System | Files | State | Owner decision |
|---|---|---|---|---|
| winget | Windows 10 and 11 | `packaging/winget/manifests/l/LaurenceHorrocksBarlow/Helia/<version>/` | ready to validate and submit | submit it? |
| Flathub | Linux | `packaging/flatpak/casa.discworld.helia.yml` | refreshed to 0.7.1, **untested** (no builder on the CI hosts); needs changes first | the app id, and binary repackaging or a source build |
| Homebrew cask | macOS | `packaging/homebrew/helia.rb` (written only when a release has a `.dmg`) | waits for a macOS build | official cask or own tap, once a signed `.dmg` exists |

## winget

What it gives: `winget install LaurenceHorrocksBarlow.Helia` on Windows, and updates through
`winget upgrade`. The manifest points at the installer on the GitHub release, with its SHA-256
(from `SHA256SUMS`). The installer is silent with `/S` and installs for the current user.

To submit, on a Windows machine with winget:

1. `winget validate --manifest packaging\winget\manifests\l\LaurenceHorrocksBarlow\Helia\0.7.1`
2. `winget install --manifest <same folder>` to try it (the installer is not code-signed, so
   SmartScreen may need "Run anyway" even here).
3. Fork `microsoft/winget-pkgs`, copy the `manifests\l\LaurenceHorrocksBarlow` folder in, and open a
   pull request; or `wingetcreate submit <folder>`.

Later releases: `wingetcreate update LaurenceHorrocksBarlow.Helia --version <v> --urls <installer url> --submit`.
Risk: reviewers run the installer through automated scans; an unsigned installer from a new
publisher can be slow to approve. The package identifier is permanent once accepted.

## Flathub

The existing manifest repackages the released `.tar.gz` for x86_64 and aarch64 (the sources
block is generated). It has not been built: there is no Flatpak builder on the runners
(`flatpak-builder --user --install --force-clean build packaging/flatpak/casa.discworld.helia.yml`
tests it on any Linux machine with Flatpak). Before sending it to Flathub:

- **App id.** Flathub requires an id on a domain the project owns or the `io.github.<user>`
  form. `casa.discworld.helia` is a private domain. Use `io.github.qisback.Helia`, which means
  renaming the id in the manifest, `packaging/linux/casa.discworld.helia.metainfo.xml.in` and the
  desktop file and icon names.
- **Build from source.** Flathub prefers that open-source apps are built from source; reviewers
  accept repackaged binaries only case by case. A source build needs `cargo-sources.json`
  (from `flatpak-cargo-generator`) and an offline `cargo build --release` in the manifest.
- Runtime `org.freedesktop.Platform` 24.08 should be bumped to the current stable runtime.
- The metainfo needs screenshots, an OARS content rating and a release entry per version.
- The finish-args ask for the whole home folder (captures live anywhere); expect a review
  question about that and be ready to justify it or use the file chooser portal.

Submission: open a pull request against `flathub/flathub` (branch `new-pr`) with the manifest;
Flathub then creates the `flathub/<app-id>` repository.

## Homebrew cask (macOS)

There is no macOS build in the releases yet, so no cask is written. When a release has a `.dmg`,
`package_managers.py` writes `packaging/homebrew/helia.rb` (Apple silicon only, as the app is).
Two routes:

- **Own tap** (recommended first): create `qisback/homebrew-helia`, put `Casks/helia.rb` in it;
  people run `brew install --cask qisback/helia/helia`. No review, no gatekeeping.
- **Official cask**: pull request to `Homebrew/homebrew-cask`. Its rules expect a signed and
  notarised app and a stable download URL, which Helia does not have yet.

## Decisions for the owner

1. Submit the winget manifest (and let `wingetcreate` update it each release)?
2. Flathub: rename the app id to `io.github.qisback.Helia`, and accept the extra work of a
   source build, or skip Flathub and rely on the AppImage, `.deb` and `.rpm`?
3. Homebrew: wait for a macOS build; then own tap first, official cask later?
