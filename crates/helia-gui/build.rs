// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Embeds the Windows icon and the `VersionInfo` resource (product, copyright,
//! version) in `helia-gui.exe`. Does nothing on other targets.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=../../assets/branding/helia.ico");
    #[cfg(windows)]
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        // The icon is not GPL: see LOGO-LICENCE.md.
        res.set_icon("../../assets/branding/helia.ico");
        res.set("ProductName", "Helia")
            .set("FileDescription", "Helia graphical stacker")
            .set("CompanyName", "Copyright (C) 2026 Laurence Horrocks-Barlow")
            .set(
                "LegalCopyright",
                "Copyright (C) 2026 Laurence Horrocks-Barlow",
            )
            .set("OriginalFilename", "helia-gui.exe")
            .set("InternalName", "helia-gui");
        res.compile()
            .expect("cannot embed the Windows `VersionInfo` resource");
    }
}
