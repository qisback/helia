<!--
SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
SPDX-License-Identifier: GPL-3.0-or-later
-->

# Helia logo licence

Copyright (C) 2026 Laurence Horrocks-Barlow. **All rights reserved.**

The Helia source code is free software under the GPL-3.0-or-later with the
additional terms in [ADDITIONAL-TERMS.md](ADDITIONAL-TERMS.md). **The Helia logo
is not part of that.** The logo, and every image derived from it, is not covered
by the GPL. In the REUSE sense it is `LicenseRef-Helia-Logo` (the text is in
[LICENSES/LicenseRef-Helia-Logo.txt](LICENSES/LicenseRef-Helia-Logo.txt)).

## What is covered

Everything under `assets/branding/`, and the copies of it elsewhere in the
repository (`docs/book/src/images/helia-logo.png`, `docs/book/src/images/favicon-header.png`, `docs/book/src/images/favicon.png`):

| File | What it is |
|---|---|
| `helia-logo.png` | The original artwork, unmodified. |
| `helia-logo.svg` | The vector artwork, unmodified. |
| `helia-logo-transparent.png` | The vector logo without its background. |
| `icons/helia-<size>.png` | Square icons, 16 to 1024 pixels. |
| `helia.ico`, `helia.icns` | The Windows and macOS icons. |

`cargo xtask branding` rebuilds everything except the two originals from
`helia-logo.svg`. The originals are identified by their SHA-256:

```text
33accd205d49891ee85f39f7c401ed0e76fbfcf9ef2777cc8a7985f7437b2812  helia-logo.png
cc3f7be591846a5e882144ec816ee575783bc18be82fb70f6360ff738982c721  helia-logo.svg
```

## What you may do

* Use the logo, **unmodified**, in unmodified builds of Helia, such as the
  releases published by the author.
* Use the unmodified logo to refer to the Helia project: an article, a review,
  a link to the project page.

Anything else needs the author's written permission.

## Forks and modified versions

If you distribute a modified version of Helia, or a fork, you must **replace the
logo and every image derived from it** with your own, so that your version is not
mistaken for the original. This goes with section 2 of
[ADDITIONAL-TERMS.md](ADDITIONAL-TERMS.md) (GPLv3 section 7(c)): a modified
version must not be presented as the original work or as made or endorsed by
Laurence Horrocks-Barlow. The places to change are the README, the book, the
About page and the window icon in `crates/helia-gui`, the Windows icon in
`crates/helia-gui/build.rs` and the Linux icons and desktop file in `packaging/`.
