// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Embeds the Windows `VersionInfo` resource (product, copyright, version) in
//! `helia.exe`. Does nothing on other targets.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    #[cfg(windows)]
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set("ProductName", "Helia")
            .set("FileDescription", "Helia command-line stacker")
            .set("CompanyName", "Copyright (C) 2026 Laurence Horrocks-Barlow")
            .set(
                "LegalCopyright",
                "Copyright (C) 2026 Laurence Horrocks-Barlow",
            )
            .set("OriginalFilename", "helia.exe")
            .set("InternalName", "helia");
        res.compile()
            .expect("cannot embed the Windows `VersionInfo` resource");
    }
}
