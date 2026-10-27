# Credits, licences and trademarks

## Helia's own licence

Helia is free software under the GNU General Public License, version 3 or (at
your option) any later version, with the additional terms in
`ADDITIONAL-TERMS.md` (GPLv3 section 7: the author attribution and the dedication
are kept, and modified versions are marked as changed). The copyright is held by
Laurence Horrocks-Barlow. The logo is **not** under the GPL: it is all rights
reserved, see `LOGO-LICENCE.md`.

## What is built into Helia

* **Rust libraries.** Every library in the program is under a permissive licence
  (MIT, Apache-2.0, BSD, ISC, Zlib, Unicode-3.0, Boost-1.0 or similar). A few are
  offered under a choice of licences that includes a copyleft one; Helia uses
  them under the permissive option. The full list, with each licence text, is in
  `THIRD-PARTY-LICENSES.txt` in every release archive and package. The graphics
  and window layers are [wgpu](https://wgpu.rs/), [egui](https://www.egui.rs/) and
  [winit](https://github.com/rust-windowing/winit).
* **Fonts.** IBM Plex Sans and IBM Plex Mono, copyright 2017 IBM Corp. with
  Reserved Font Name "Plex", under the SIL Open Font License 1.1 (the licence
  text is in the source tree, `crates/helia-gui/assets/fonts/OFL-IBM-Plex.txt`).
  egui's own default fonts are not included.
* **Data.** Helia ships no star catalogue, deep-sky list or ephemeris file. The
  position of the Sun comes from published formulas (see
  [References](references.md)), and the limb-darkening coefficients are the
  published ones.
* **Test captures.** The pictures and captures in the manual and the tests were
  made by the author with his own telescope and cameras.

## Programs Helia runs but does not contain

* **ffmpeg** reads MP4, MOV and compressed AVI files when it is installed. Helia
  starts it as a separate program and does not include, link or distribute it. It
  has its own licence, which depends on how it was built.
* **Vulkan, Metal and DirectX drivers** come with your system. On Linux the
  optional Mesa software driver (lavapipe) is used only if you install it.

## Credit

Helia stands on published work. [References](references.md) lists the papers and
books for every method, and the doc comment of each module repeats them. The
RegiStax-style wavelet controls follow the documentation and tutorials by Cor
Berrevoets and others listed in [Coming from RegiStax](../user-guide/coming-from-registax.md).

## Trademarks

Other products are named in this manual only to say what they are or what Helia
can read or do alongside them (for example "coming from RegiStax", "reads
SharpCap files"). **Helia is not affiliated with, endorsed by or sponsored by**
any of them. All product and company names are trademarks or registered
trademarks of their owners, including:

AutoStakkert!, RegiStax, ImPPG, Siril, PixInsight, WinJUPOS, PIPP, SharpCap,
FireCapture, AstroSurface, SER Player, PlanetarySystemStacker, SolHAT, JSol'Ex,
ZWO and ASI, Seestar and ZWO ASIAIR, Celestron, Windows, DirectX, macOS, Metal,
Apple, Linux, NVIDIA, AMD, Intel, Vulkan, FITS, Raspberry Pi.

Statements about other programs (their features, licence or platform) come from
their public documentation at the time of the review in
[Research](../developer/research.md), are marked there with how far they were
checked, and may be out of date. If a statement about your program is wrong,
please say so through [Feedback](../user-guide/sending-feedback.md) and it will
be corrected.
