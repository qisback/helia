---
description: "How Helia updates itself: the three modes, what each install type does, how to turn it off, what is sent, and how to go back to the previous version."
---

# Updates

Helia can tell you when a newer version exists, and for some installs can install it
for you. Nothing is installed without your say-so unless you choose **automatic**
mode, and you can turn the whole thing off.

## The three modes

| Mode | What Helia does | Default |
|---|---|---|
| **Notify** | Checks once a day (a few seconds after start-up). If there is a newer version it says so: a button in the top bar and a notice. It downloads nothing until you click. | yes |
| **Automatic** | Checks once a day, downloads the update in the background, and installs it when you restart Helia (Windows installer: when you close it). Only for installs Helia can replace itself, see below. For the others it behaves like notify. | |
| **Off** | Never contacts the network on its own. You can still press **Check for updates** or run `helia update check` yourself. | |

Choose the mode on the **About** page, under **Updates**, or from a terminal:

```sh
helia update config                   # show the mode and channel
helia update config --mode off        # never check on my own
helia update config --mode automatic
helia update config --mode notify     # the default
helia update config --channel prerelease   # include pre-release versions
```

The choice is kept in `update.conf` in the Helia settings folder
(`%APPDATA%\helia` on Windows, `~/.config/helia` on Linux), shared by the app and
the command line. It holds three lines you can edit by hand:

```text
schema=1
mode=notify
channel=stable
```

The **stable** channel carries released versions. **Pre-release** carries those too,
plus pre-release builds. If a settings file was written by a newer Helia than the one
you are running, Helia reads what it understands, says so, and does not rewrite the
file, so going back to an older Helia never loses newer settings.

## Checking now

On the **About** page press **Check for updates**. If there is one, the page names the
version, links to what changed, and offers **Download and install** (or **Download**,
for installs Helia does not replace itself).

From a terminal:

```sh
helia update check            # exit code 0: up to date, 10: update available
helia update check --json     # the same answer as JSON
helia update apply            # shows what will happen and asks first
helia update apply --yes      # no question
helia update status           # mode, channel, install type, last check
helia update rollback         # go back to the version the update replaced
```

Each of these is a subcommand of `helia update`; the documentation coverage check, which
lists subcommands by their last word, knows them as `helia check`, `helia apply`,
`helia status`, `helia rollback` and `helia config`. The command line never checks by itself; it contacts the update server only when you
run `check` or `apply`. Exit codes: `0` success (for `check`: up to date), `1` failed
(the message says what, why and what to do), `3` `apply` or `rollback` was not
confirmed, `10` `check` found an update, `12` the update details need a newer Helia
to read, so update by hand.

## What happens for each kind of install

| How Helia is installed | What an update does |
|---|---|
| **Windows installer** (per user) | Helia downloads the new installer, checks it, and when Helia closes a small hidden helper runs it silently in the same folder. Your settings are not touched. Helia opens again afterwards. If the installer fails, the helper puts the old programs back. |
| **Windows installer** (all users) | Changing `Program Files` needs administrator rights, so Helia downloads the installer and tells you where it is. Run it as an administrator. |
| **AppImage** | The new file is written next to the old one, flushed to disk, marked runnable, and then renamed over the old one in one step. The old one stays beside it as `<name>.previous`. The new version starts the next time you open Helia. |
| **deb or rpm** (installed with `apt`, `dnf`, `zypper`) | Helia never touches files that belong to your package manager. It downloads the package and shows the command to run, for example `sudo apt install /home/me/.local/share/helia/updates/....deb` or `sudo dnf upgrade .../....rpm`. |
| **Flatpak** | Use `flatpak update casa.discworld.helia`. |
| **macOS app, tar.gz or zip** | Helia tells you a newer version exists and downloads it on request. You replace the app or unpack the archive yourself. |

## Going back

If a new version does not start properly twice in a row, the third start puts the
previous version back (AppImage: the `.previous` file; Windows installer: the
programs kept before the update), starts it, and records what happened in the log and
as a notice (shown by the restored version when it has this feature). That version is not offered again by the daily check, so there is no loop.

You can also go back yourself with `helia update rollback`. This only works for an
update that Helia installed itself, and only while the old version is still kept.

"Starts properly" means the window has been up for a few seconds. A version that
crashes before it can record that cannot roll itself back; for that, copy the
`.previous` file over the AppImage by hand, or reinstall the older installer.

## Privacy: what is sent

Helia has no telemetry. The update check makes these requests and no others:

1. `GET` of the signed manifest, a small JSON file, from the update server named on
   the **About** page (the build's configured address; by default
   `12hobbies.studio`, with the files themselves from GitHub release downloads);
2. `GET` of its signature (`manifest.json.minisig`) from the same place;
3. only when you ask (or in automatic mode): `GET` of the update file itself.

Every request carries only the standard HTTP framing and this identification, with
your Helia's version number in place of `0.7.1`:

```text
User-Agent: helia-update/0.7.1
```

No user name, machine name, address you chose, capture name, licence, or any
identifier is sent, and no cookies. (An installer bought from the store has its own settings under Updates, and says there what they send.) Redirects are followed only between the servers
named in the build (the manifest server for the manifest, the download servers for
files), and every address must be https. The server sees your IP address, as any web
server does; Helia does not send it anything else. To stop all of it, choose **Never
check on my own**.

## How an update is checked

Before anything is used, Helia checks that the manifest is signed by the Helia release
key built into the program (an Ed25519 signature in the minisign format, up to two
keys so the key can be changed), that it is for Helia and the right channel, that it
has not expired, that it is not an older copy of one already seen, and that every
address is on the allowed servers. The downloaded file must then match the size and
SHA-256 in the signed manifest, and is checked again just before it is used. A
tampered manifest, a wrong signature, an old manifest replayed, a truncated or
altered download and an address on another server are all refused, and nothing on
your computer changes. The design and the manifest format are in
[Automatic updates: design](../developer/updates.md).

## If something goes wrong

* **"could not reach the update server".** You are offline or the server is down. Helia
  carries on as it was; try again later. A failed daily check says nothing; a failed
  click on **Check for updates** shows the message.
* **"not signed by a key this app trusts".** The update details were altered or are
  from another source. Nothing was installed. Download Helia again from the Helia
  website.
* **"could not install the update ... Nothing was changed".** The folder is not
  writable or the disk is full. The text names the folder. Fix that and try again; the
  old version is still in place.
* **A version that you do not want.** Choose **Off** and download the version you want
  from the Helia website.
