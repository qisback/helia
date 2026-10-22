// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Console output from a GUI-subsystem program.
//!
//! On Windows `helia-gui.exe` is built for the `windows` subsystem so a double
//! click opens no console window. Such a program starts with no standard
//! handles when it is run from a terminal, so `--version` would print nowhere.
//! [`attach`] joins the parent's console for those few commands that print and
//! exit. Elsewhere it does nothing.

/// Make `println!` reach the terminal that started this program, if there is
/// one. Does nothing when stdout is already redirected, or off Windows.
#[cfg(not(windows))]
pub fn attach() {}

/// See the non-Windows variant.
#[cfg(windows)]
#[allow(unsafe_code, reason = "two Win32 console calls, no memory is shared")]
pub fn attach() {
    use std::os::windows::io::IntoRawHandle;

    use windows_sys::Win32::System::Console::{
        ATTACH_PARENT_PROCESS, AttachConsole, GetStdHandle, STD_ERROR_HANDLE, STD_OUTPUT_HANDLE,
        SetStdHandle,
    };

    // SAFETY: GetStdHandle only reads the process's handle table.
    let out = unsafe { GetStdHandle(STD_OUTPUT_HANDLE) };
    if !out.is_null() && out as isize != -1 {
        return; // redirected to a file or a pipe: it already works
    }
    // SAFETY: AttachConsole takes a constant and fails harmlessly with no parent console.
    if unsafe { AttachConsole(ATTACH_PARENT_PROCESS) } == 0 {
        return;
    }
    if let Ok(f) = std::fs::OpenOptions::new().write(true).open("CONOUT$") {
        let h = f.into_raw_handle();
        // SAFETY: the handle is valid and now owned by the process for its whole life.
        unsafe {
            SetStdHandle(STD_OUTPUT_HANDLE, h);
            SetStdHandle(STD_ERROR_HANDLE, h);
        }
    }
}
