# Installing external tools: adoption guide

Some features need a program or a data pack that the app must not ship. Helia's
video support needs ffmpeg. Valerian will need the Gaia DR3 data pack. This page
describes `helia-external` (`crates/helia-external`), the shared crate that lets an app
offer to fetch such an item, safely and only when the user agrees, and how another app
adopts it. The design issue is
#365.

Everything it relies on for trust comes from `helia-update` (the minisign verifier, the
pinned release keys, the address checks and the HTTP client). `helia-external` adds the
list of items, the licence acceptance and the install.

## The rules the crate enforces

1. **Opt-in.** Nothing is downloaded unless the app calls `Client::install`, and
   `install` returns `Error::NotAccepted` **before making any request** unless the user's
   acceptance of exactly this licence text is on record.
2. **The licence comes first.** The signed manifest carries the full licence text, the
   provider and a source link. The app must show all of it and ask. Only then does it
   call `Acceptances::accept` and `Acceptances::save`.
3. **Re-ask when the text changes.** The record keeps the sha256 of the text that was
   shown. A new text, even one character different, does not match, so the app asks again.
4. **Nothing is redistributed.** The app never contains the item. The files come from
   the provider's own server, named in the signed manifest.
5. **Verified.** https only, hosts on the app's own allow-list (a signed manifest cannot
   widen it), sha256 and size checked, resumable, unpacked by file name only into a
   fresh folder in the user's tools folder.
6. **Local records.** Acceptances are kept on the user's computer only (mode 0600 where
   the system has file modes) and are never sent anywhere.
7. **No new cryptography.** The manifest is signed with the release key by the same
   tool as the update manifests and verified by `helia_update::verify`.

## The manifest

One JSON document per app, `external.json`, and its detached signature
`external.json.minisig`, served from the app's update host. The limits are 512 KiB and
nesting depth 8. Unknown fields are ignored; a download for an unknown platform or archive
kind is skipped, so a newer manifest still works in an older app.

| Field | Meaning |
|---|---|
| `schema` | Always `1`. A larger number is refused with "update the app by hand". |
| `app` | The app the manifest is for (`helia`). A manifest for another app is refused. |
| `date`, `expires` | `YYYY-MM-DDTHH:MM:SSZ`. After `expires` the manifest is refused (freeze attack). |
| `sequence` | Rises with every manifest. The client remembers the highest and refuses a lower one (replay). |
| `licences[]` | `id`, `name`, `spdx`, `text` (the full text, shown unchanged), `source_url`, `attribution`, `show_attribution`. Up to 128 KiB of text each. |
| `items[]` | `id`, `name`, `version`, `provider`, `provider_url`, `description` (plain words, shown in the dialog), `required` (else optional), `licence` (the default licence id). |
| `items[].downloads[]` | `platform`, `url` (https), `sha256`, `size`, `archive` (`none`, `zip` or `tar.xz`), `members[]`, optional `licence`. |
| `members[]` | `find` (path inside the archive, matched as a whole-component suffix, for example `bin/ffmpeg`), `save_as` (the file name used on disk), `provides` (the logical name the app looks it up by), `executable`. |
| `items[].packages[]` | `platform`, `manager`, `argv` (a package manager command, never run through a shell), optional `licence`. |

Platforms are `windows-x86_64`, `linux-x86_64`, `linux-aarch64`, `macos-aarch64` and
`macos-x86_64`. The parts of one item on one platform must share a licence, so one
acceptance covers them. A package command must start with `sudo` followed by a known
package manager (`apt`, `apt-get`, `dnf`, `pacman`, `zypper`) or with `brew`, and every word
is limited to letters, digits and `- _ . + : = @`.

### Building and signing it

Helia keeps the list of items in `packaging/external/helia-items.json` and the licence
texts beside it. The pins (URL, sha256, size) are taken from files actually downloaded
and checked against the provider's own published checksums where there are any (BtbN
publishes `checksums.sha256`; evermeet.cx signs each file with a GPG key whose fingerprint is
on its page). To build and sign:

```sh
python3 packaging/external/build_manifest.py --items packaging/external/helia-items.json \
    --sequence 12 --out external.json
helia-manifest sign --manifest external.json          # writes external.json.minisig
```

Publish both files next to the update manifests (`<base>/helia/external.json`). A
provider that moves a file only needs a new manifest, not a new app. The release
workflow wiring for this step is tracked on the design issue.

## Using it from an app

```rust
use helia_external::{Acceptances, Client, ExternalSource, Route, installed_program};
use helia_update::{SystemClock, StdFs, Platform};
use helia_update::ureq_http::UreqHttp;

// 1. Configure: the manifest address, the pinned keys, the hosts you will talk to.
let source = ExternalSource::new("valerian", "https://example.org/valerian/external.json", &keys)
    .with_asset_hosts(&["data.example.org"]);
let dirs = helia_external::Dirs::from_env("valerian", "VALERIAN_EXTERNAL_DIR").expect("a home folder");
let (http, clock) = (UreqHttp::new(false), SystemClock);
let client = Client { http: &http, fs: &StdFs, clock: &clock, source: &source, dirs: &dirs };

// 2. Fetch the verified manifest and pick the way to get the item on this system.
let manifest = client.fetch_manifest()?;
let offers = manifest.offers("gaia-dr3", Platform::current())?;
let offer = offers.iter().find(|o| matches!(o.route, Route::Download(_))).unwrap();

// 3. Show offer.licence.text in full, with offer.item.provider and offer.licence.source_url.
//    Only on an explicit yes:
let mut accepted = Acceptances::load(&StdFs, &dirs.accepted)?;
if !accepted.is_accepted(offer) {
    // ... ask the user ...
    accepted.accept(offer, clock.now_unix(), "gui");
    accepted.save(&StdFs, &dirs.accepted)?;
}

// 4. Install (progress and cancel via install_with), then use it.
let record = client.install(offer, &accepted)?;
let path = installed_program(&dirs, "gaia-dr3", "gaia-pack");
```

What the app is responsible for:

- Show the **whole** licence text, the provider and the source link before asking, with
  nothing pre-ticked. A "Decline" leaves the feature unavailable with a clear message and
  the rest of the app working.
- After install, show `Installed::attribution` where `show_attribution` is true (About
  page, tools status).
- For a `Route::Package` offer, show `PackageCommand::display()` and run it with
  `Client::run_package` only if the user agrees. `SystemRunner` hands the terminal to the
  command so `sudo` asks for the password itself. In a window with no terminal, show the
  command to copy instead.
- Use `Client::list`, `Client::check` and `Client::remove` for a status screen and an
  uninstall. `remove` deletes the item's folder; for a package manager install it only
  forgets Helia's record and says so.
- Keep an environment variable override (Helia: `HELIA_FFMPEG`, `HELIA_FFPROBE`) that wins
  over the installed copy.

### Testing an adopting app

`helia-update` provides `mem::{FixedClock, MemFs, MemHttp}` and, with its `sign` feature,
`sign::KeyPair` for throwaway keys. The crate's own `tests/net.rs` is the pattern for a
loopback server: it serves a signed manifest and an archive on `127.0.0.1` and runs the
whole flow with `ExternalSource::with_loopback_http(true)`.

## Adopting it in Valerian and Annora

Both depend on Helia's crates through a git tag, as they do for `helia-core`: add
`helia-external` (and `helia-update`) from the same tag. Each app has its own manifest
(`app` = `valerian`), its own `ExternalSource` with its own allow-listed hosts and its own
`Dirs::from_env("valerian", ...)`; the pinned release keys are the update keys.

Valerian's Gaia DR3 data pack, as a sketch: an item `gaia-dr3` with `archive: "none"` (or a
tarball) and one `members` entry that saves `gaia-dr3.pack` with `provides: "gaia-pack"`; a
licence entry for CC BY-NC 3.0 IGO whose `text` is the full licence deed and whose
`attribution` is ESA's required credit, with `show_attribution: true`. The pack is then
fetched from its host after acceptance and never sits inside a paid installer. Items up to
4 GiB are supported; `size` and `sha256` are required.

## What is not covered

- Installing into a system-wide location. Downloads go to the user's tools folder;
  only a package manager route changes the system, and only with the user's consent.
- Updating an installed item. Run the install again for a new version; the old folder is
  replaced as a whole. Automatic refresh is not offered, on purpose.
- Anything that is not a plain file set: no installers are run and no scripts execute.
