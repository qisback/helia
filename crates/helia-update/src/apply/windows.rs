// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Windows: run the new installer silently once the app has exited.
//!
//! The installer (`packaging/windows/helia.nsi`) takes `/S` (silent) and
//! `/D=<folder>` (install folder, last, unquoted), installs per user without
//! administrator rights, and leaves settings (`%APPDATA%\helia`) alone, so the
//! update runs it with exactly those two switches on the folder the app is in.
//! A copy installed for all users needs administrator rights, so that case only
//! shows where the installer is.
//!
//! A running program cannot be overwritten, so the work is done by a hidden
//! PowerShell helper started just before the app exits. It waits for the app's
//! process to end, runs the installer, and if the installer fails puts back the
//! programs [`apply`] copied aside. It then starts the app again. The script is
//! passed as `-EncodedCommand` (base64 of UTF-16LE), so no path needs shell quoting.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use super::{ApplyOutcome, Ctx, Rollback, verify_staged};
use crate::download::Staged;
use crate::error::{Error, Result};
use crate::state::BootHealth;

/// The programs the installer replaces and the update keeps a copy of.
pub const PROGRAMS: [&str; 2] = ["helia-gui.exe", "helia.exe"];

const PS: &str = "powershell.exe";

/// A PowerShell single-quoted string literal.
fn ps_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

/// Base64 of the UTF-16LE bytes, the form `-EncodedCommand` takes.
#[must_use]
pub fn encode_command(script: &str) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let bytes: Vec<u8> = script.encode_utf16().flat_map(u16::to_le_bytes).collect();
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = (u32::from(chunk[0]) << 16)
            | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8)
            | u32::from(*chunk.get(2).unwrap_or(&0));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(char::from(ALPHABET[((n >> (18 - 6 * i)) & 63) as usize]));
            } else {
                out.push('=');
            }
        }
    }
    out
}

fn helper_args(script: &str) -> Vec<OsString> {
    [
        "-NoProfile",
        "-NonInteractive",
        "-WindowStyle",
        "Hidden",
        "-EncodedCommand",
    ]
    .iter()
    .map(OsString::from)
    .chain(std::iter::once(OsString::from(encode_command(script))))
    .collect()
}

fn restore_line(dir: &str, backup: &str) -> String {
    let programs = PROGRAMS.map(ps_quote).join(",");
    format!(
        "foreach ($f in {programs}) {{ $b = Join-Path {backup} $f; if (Test-Path -LiteralPath $b) {{ Copy-Item -LiteralPath $b -Destination (Join-Path {dir} $f) -Force }} }}\n"
    )
}

/// The helper script for an update.
#[must_use]
pub fn install_script(
    pid: u32,
    installer: &Path,
    dir: &Path,
    backup: &Path,
    relaunch: bool,
) -> String {
    let installer = ps_quote(&installer.to_string_lossy());
    let dir_q = ps_quote(&dir.to_string_lossy());
    let backup = ps_quote(&backup.to_string_lossy());
    let switch = ps_quote(&format!("/D={}", dir.to_string_lossy()));
    let mut lines = vec![
        "$ErrorActionPreference = 'Continue'".to_string(),
        format!("try {{ Wait-Process -Id {pid} -Timeout 3600 }} catch {{ }}"),
        "$ok = $false".to_string(),
        format!(
            "try {{ $p = Start-Process -FilePath {installer} -ArgumentList '/S',{switch} -Wait -PassThru; $ok = ($p.ExitCode -eq 0) }} catch {{ }}"
        ),
        format!("if (-not $ok) {{ {} }}", restore_line(&dir_q, &backup)),
    ];
    if relaunch {
        lines.push(format!(
            "Start-Process -FilePath (Join-Path {dir_q} 'helia-gui.exe')"
        ));
    }
    lines.join(
        "
",
    ) + "
"
}

/// The helper script for a rollback: put the kept programs back and, when
/// `relaunch`, start the app.
#[must_use]
pub fn rollback_script(pid: u32, dir: &Path, backup: &Path, relaunch: bool) -> String {
    let dir_q = ps_quote(&dir.to_string_lossy());
    let backup = ps_quote(&backup.to_string_lossy());
    let mut lines = vec![
        "$ErrorActionPreference = 'Continue'".to_string(),
        format!("try {{ Wait-Process -Id {pid} -Timeout 3600 }} catch {{ }}"),
        restore_line(&dir_q, &backup),
    ];
    if relaunch {
        lines.push(format!(
            "Start-Process -FilePath (Join-Path {dir_q} 'helia-gui.exe')"
        ));
    }
    lines.join(
        "
",
    ) + "
"
}

fn backup_for(ctx: &Ctx<'_>, version: &str) -> PathBuf {
    let safe: String = version
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '+'))
        .collect();
    ctx.backup_dir.join(safe)
}

/// Keep copies of the running programs, then start the helper that installs.
///
/// # Errors
/// [`Error::Apply`] (nothing changed) when the file cannot be used or the helper cannot start.
pub fn apply(
    ctx: &Ctx<'_>,
    dir: &Path,
    all_users: bool,
    staged: &Staged,
    from_version: &str,
    health: &mut BootHealth,
) -> Result<ApplyOutcome> {
    verify_staged(ctx.fs, staged, "nsis")?;
    if all_users {
        return Ok(ApplyOutcome::Manual {
            message: format!(
                "Helia is installed for all users, which needs administrator rights to change. The new installer is downloaded to {}. Run it as an administrator.",
                staged.path.display()
            ),
            command: None,
            file: Some(staged.path.clone()),
        });
    }
    let backup = backup_for(ctx, from_version);
    ctx.fs.create_dir_all(&backup).map_err(|e| {
        Error::Apply(format!(
            "could not make room to keep the old version ({e}). Nothing was changed."
        ))
    })?;
    let mut kept = 0;
    for p in PROGRAMS {
        let from = dir.join(p);
        if ctx.fs.len(&from)?.is_some() {
            ctx.fs.copy_file(&from, &backup.join(p)).map_err(|e| {
                Error::Apply(format!(
                    "could not keep a copy of the old {p} ({e}). Nothing was changed."
                ))
            })?;
            kept += 1;
        }
    }
    let script = install_script(
        ctx.process.pid(),
        &staged.path,
        dir,
        &backup,
        ctx.relaunch_gui,
    );
    ctx.process
        .spawn_detached(Path::new(PS), &helper_args(&script))
        .map_err(|e| {
            Error::Apply(format!(
                "could not start the installer helper ({e}). Nothing was changed; you can run {} yourself.",
                staged.path.display()
            ))
        })?;
    health.record_applied(from_version, (kept > 0).then_some(backup.as_path()));
    Ok(ApplyOutcome::Scheduled {
        message: format!(
            "Helia {} installs as soon as Helia closes{}. Your settings are kept.",
            staged.version,
            if ctx.relaunch_gui {
                ", then Helia opens again"
            } else {
                ""
            }
        ),
    })
}

/// Put the kept programs back (after Helia has exited).
///
/// # Errors
/// [`Error::Apply`] when no copy is kept or the helper cannot start.
pub fn rollback(ctx: &Ctx<'_>, dir: &Path, health: &mut BootHealth) -> Result<Rollback> {
    let version = health.previous.clone().unwrap_or_default();
    let Some(backup) = health.previous_path.clone() else {
        return Err(Error::Apply(
            "the old version was not kept, so Helia cannot go back to it. Download the version you want from the Helia website.".into(),
        ));
    };
    if ctx.fs.len(&backup.join(PROGRAMS[0]))?.is_none() {
        return Err(Error::Apply(format!(
            "the old version is not kept at {} any more, so Helia cannot go back to it. Download the version you want from the Helia website.",
            backup.display()
        )));
    }
    ctx.process
        .spawn_detached(
            Path::new(PS),
            &helper_args(&rollback_script(
                ctx.process.pid(),
                dir,
                &backup,
                ctx.relaunch_gui,
            )),
        )
        .map_err(|e| {
            Error::Apply(format!(
                "could not start the helper that puts the old version back ({e})."
            ))
        })?;
    let failed = health.running.clone();
    health.rolled_back();
    let then = if ctx.relaunch_gui {
        format!("Helia closes now and opens again on {version}.")
    } else {
        format!("{version} is put back as soon as this command ends.")
    };
    let message = match failed {
        Some(f) => format!(
            "Helia {f} did not start properly twice, so I am putting the old version {version} back. {then} Nothing of yours was changed."
        ),
        None => format!(
            "Helia is going back to version {version}. {then} Nothing of yours was changed."
        ),
    };
    Ok(Rollback {
        to_version: version,
        message,
        relaunch: None,
        exit_now: true,
    })
}

#[cfg(test)]
#[allow(clippy::many_single_char_names, reason = "tests")]
mod tests {
    use super::*;
    use crate::apply::fake::{FakeProcess, TestFs};
    use crate::verify::sha256_hex;

    const DIR: &str = "C:/Users/me/AppData/Local/Programs/Helia";

    fn ctx<'a>(fs: &'a TestFs, p: &'a FakeProcess, gui: bool) -> Ctx<'a> {
        Ctx {
            fs,
            process: p,
            backup_dir: Path::new("C:/data/helia/previous"),
            relaunch_gui: gui,
        }
    }

    fn setup() -> (TestFs, Staged) {
        let fs = TestFs::default();
        fs.mem
            .put(&Path::new(DIR).join("helia-gui.exe"), b"gui-old");
        fs.mem.put(&Path::new(DIR).join("helia.exe"), b"cli-old");
        let body = b"MZ installer";
        let path = PathBuf::from("C:/data/helia/updates/ab12-Helia-0.8.0-setup.exe");
        fs.mem.put(&path, body);
        let s = Staged {
            version: "0.8.0".into(),
            kind: "nsis".into(),
            path,
            sha256: sha256_hex(body),
            size: body.len() as u64,
        };
        (fs, s)
    }

    #[test]
    fn base64_matches_known_values() {
        // "hi" in UTF-16LE is 68 00 69 00.
        assert_eq!(encode_command("hi"), "aABpAA==");
        assert_eq!(encode_command("a"), "YQA=");
        assert_eq!(encode_command(""), "");
        assert_eq!(encode_command("abc"), "YQBiAGMA");
    }

    #[test]
    fn apply_keeps_the_programs_and_starts_one_helper() {
        let (fs, s) = setup();
        let p = FakeProcess::default();
        let mut h = BootHealth::default();
        let out = apply(
            &ctx(&fs, &p, true),
            Path::new(DIR),
            false,
            &s,
            "0.7.1",
            &mut h,
        )
        .unwrap();
        assert!(matches!(out, ApplyOutcome::Scheduled { .. }), "{out:?}");
        let b = Path::new("C:/data/helia/previous/0.7.1");
        assert_eq!(fs.mem.get(&b.join("helia-gui.exe")).unwrap(), b"gui-old");
        assert_eq!(fs.mem.get(&b.join("helia.exe")).unwrap(), b"cli-old");
        assert_eq!(h.previous.as_deref(), Some("0.7.1"));
        assert_eq!(h.previous_path.as_deref(), Some(b));
        let spawned = p.spawned.lock().unwrap();
        assert_eq!(spawned.len(), 1);
        assert_eq!(spawned[0].0, Path::new("powershell.exe"));
        let a: Vec<String> = spawned[0]
            .1
            .iter()
            .map(|x| x.to_string_lossy().into_owned())
            .collect();
        assert!(a.contains(&"-EncodedCommand".to_string()), "{a:?}");
        // The installer is still in place for the helper.
        assert!(fs.mem.get(&s.path).is_some());
    }

    #[test]
    fn the_script_waits_installs_silently_restores_and_relaunches() {
        let script = install_script(
            4242,
            Path::new("C:/data/updates/Helia 0.8.0-setup.exe"),
            Path::new("C:/Users/O'Brien/Programs/Helia"),
            Path::new("C:/data/previous/0.7.1"),
            true,
        );
        assert!(script.contains("Wait-Process -Id 4242"));
        assert!(script.contains("-ArgumentList '/S','/D=C:/Users/O''Brien/Programs/Helia'"));
        assert!(script.contains("-FilePath 'C:/data/updates/Helia 0.8.0-setup.exe'"));
        assert!(script.contains("if (-not $ok)"));
        assert!(script.contains("Copy-Item"));
        assert!(script.trim_end().ends_with("'helia-gui.exe')"));
        let cli = install_script(1, Path::new("i.exe"), Path::new("d"), Path::new("b"), false);
        assert!(!cli.contains("Start-Process -FilePath (Join-Path 'd' 'helia-gui.exe')"));
    }

    #[test]
    fn all_users_install_is_manual() {
        let (fs, s) = setup();
        let p = FakeProcess::default();
        let mut h = BootHealth::default();
        let out = apply(
            &ctx(&fs, &p, true),
            Path::new(DIR),
            true,
            &s,
            "0.7.1",
            &mut h,
        )
        .unwrap();
        assert!(matches!(out, ApplyOutcome::Manual { file: Some(_), .. }));
        assert!(p.spawned.lock().unwrap().is_empty());
        assert_eq!(h, BootHealth::default());
    }

    #[test]
    fn a_tampered_installer_is_refused() {
        let (fs, s) = setup();
        fs.mem.put(&s.path, b"MZ different!");
        let p = FakeProcess::default();
        let mut h = BootHealth::default();
        assert!(
            apply(
                &ctx(&fs, &p, true),
                Path::new(DIR),
                false,
                &s,
                "0.7.1",
                &mut h
            )
            .is_err()
        );
        assert!(p.spawned.lock().unwrap().is_empty());
    }

    #[test]
    fn rollback_starts_the_helper_and_forgets_the_failure() {
        let (fs, s) = setup();
        let p = FakeProcess::default();
        let mut h = BootHealth::default();
        apply(
            &ctx(&fs, &p, true),
            Path::new(DIR),
            false,
            &s,
            "0.7.1",
            &mut h,
        )
        .unwrap();
        for _ in 0..3 {
            h.begin_start("0.8.0");
        }
        let r = rollback(&ctx(&fs, &p, true), Path::new(DIR), &mut h).unwrap();
        assert!(r.exit_now);
        assert_eq!(r.to_version, "0.7.1");
        assert_eq!(p.spawned.lock().unwrap().len(), 2);
        assert_eq!(h.rollback_target(), None);
        // Nothing kept: refuse.
        let e = rollback(
            &ctx(&fs, &p, true),
            Path::new(DIR),
            &mut BootHealth::default(),
        );
        assert!(e.is_err());
    }
}
