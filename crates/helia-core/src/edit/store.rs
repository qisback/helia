// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Saved edit presets: a named [`EditList`] in a small JSON file.
//!
//! Presets live one per file in a folder, found at `$HELIA_EDIT_PRESETS`,
//! else `%APPDATA%\helia\edit-presets`, else
//! `$XDG_CONFIG_HOME/helia/edit-presets`, else `~/.config/helia/edit-presets`.
//! The file is readable and editable by hand:
//!
//! ```json
//! { "name": "H-alpha gentle", "filter": "ha", "edits": [ {"op": "unsharp", "amount": 0.5} ] }
//! ```

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::EditList;
use crate::error::{Error, Result};

/// Largest preset file read.
const MAX_BYTES: u64 = 256 * 1024;

/// A named, saved edit list.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct EditPreset {
    /// Name shown in lists.
    pub name: String,
    /// Filter id (`ha`, `cak`, ...) the preset was made for, if any.
    pub filter: Option<String>,
    /// The edits.
    pub edits: EditList,
}

/// File stem for a preset name: lower-case letters, digits and hyphens.
///
/// # Errors
/// [`Error::Invalid`] when nothing usable is left.
pub fn file_stem(name: &str) -> Result<String> {
    let mut stem = String::new();
    for c in name.trim().chars() {
        if c.is_ascii_alphanumeric() {
            stem.push(c.to_ascii_lowercase());
        } else if (c == ' ' || c == '-' || c == '_') && !stem.ends_with('-') && !stem.is_empty() {
            stem.push('-');
        }
    }
    let stem = stem.trim_end_matches('-').to_string();
    if stem.is_empty() || stem.len() > 64 {
        return Err(Error::Invalid(
            "a preset name needs 1 to 64 letters, digits or spaces".into(),
        ));
    }
    Ok(stem)
}

/// The folder presets are kept in, if a place can be worked out.
#[must_use]
pub fn presets_dir() -> Option<PathBuf> {
    let var = |k: &str| {
        std::env::var_os(k)
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
    };
    if let Some(p) = var("HELIA_EDIT_PRESETS") {
        return Some(p);
    }
    let dir = var("APPDATA")
        .or_else(|| var("XDG_CONFIG_HOME"))
        .or_else(|| var("HOME").map(|h| h.join(".config")))?;
    Some(dir.join("helia").join("edit-presets"))
}

/// Read one preset file.
///
/// # Errors
/// I/O failure, an oversized file or a file that is not a preset.
pub fn load(path: &Path) -> Result<EditPreset> {
    let meta = std::fs::metadata(path)?;
    if meta.len() > MAX_BYTES {
        return Err(Error::Invalid(format!(
            "{} is too large for a preset",
            path.display()
        )));
    }
    let text = std::fs::read_to_string(path)?;
    let stem = path
        .file_stem()
        .map_or_else(String::new, |s| s.to_string_lossy().into_owned());
    if text.trim_start().starts_with('[') {
        // A bare edit list is also accepted.
        return Ok(EditPreset {
            name: stem,
            filter: None,
            edits: EditList::from_json(&text)?,
        });
    }
    let mut preset: EditPreset = serde_json::from_str(&text)
        .map_err(|e| Error::Invalid(format!("{}: {e}", path.display())))?;
    if preset.name.is_empty() {
        preset.name = stem;
    }
    if preset.edits.ops.len() > super::MAX_OPS {
        return Err(Error::Invalid(format!(
            "{}: at most {} edits",
            path.display(),
            super::MAX_OPS
        )));
    }
    Ok(preset)
}

/// Write `preset` into `dir` (created if needed); returns the file written.
///
/// # Errors
/// An unusable name or an I/O failure.
pub fn save(dir: &Path, preset: &EditPreset) -> Result<PathBuf> {
    let stem = file_stem(&preset.name)?;
    std::fs::create_dir_all(dir)?;
    let path = dir.join(format!("{stem}.json"));
    let text =
        serde_json::to_string_pretty(preset).map_err(|e| Error::Invalid(format!("preset: {e}")))?;
    // Write beside, then rename, so a crash never leaves half a preset.
    let tmp = dir.join(format!(".{stem}.json.tmp"));
    std::fs::write(&tmp, text + "\n")?;
    std::fs::rename(&tmp, &path)?;
    Ok(path)
}

/// Every readable preset in `dir`, by name. Damaged files are skipped.
#[must_use]
pub fn list(dir: &Path) -> Vec<EditPreset> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<EditPreset> = rd
        .filter_map(std::result::Result::ok)
        .map(|e| e.path())
        .filter(|p| {
            p.extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("json"))
        })
        .filter_map(|p| load(&p).ok())
        .collect();
    out.sort_by_key(|a| a.name.to_lowercase());
    out
}

/// Delete the preset called `name` from `dir`.
///
/// # Errors
/// An unusable name or an I/O failure (a missing file is not an error).
pub fn remove(dir: &Path, name: &str) -> Result<()> {
    let path = dir.join(format!("{}.json", file_stem(name)?));
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.into()),
    }
}

/// Find a preset by file path or by name: a path to an existing file is read
/// directly; anything else is looked up by name in `dir`.
///
/// # Errors
/// No such preset, or it cannot be read.
pub fn find(reference: &str, dir: Option<&Path>) -> Result<EditPreset> {
    let direct = Path::new(reference);
    if direct.is_file() {
        return load(direct);
    }
    if let Some(dir) = dir {
        if let Ok(stem) = file_stem(reference) {
            let p = dir.join(format!("{stem}.json"));
            if p.is_file() {
                return load(&p);
            }
        }
        if let Some(found) = list(dir)
            .into_iter()
            .find(|e| e.name.eq_ignore_ascii_case(reference))
        {
            return Ok(found);
        }
    }
    Err(Error::Invalid(format!(
        "no edit preset {reference:?}: give a .json file or the name of a saved preset"
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::edit::{Op, Unsharp};

    fn dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("helia-presets-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn names_become_safe_file_stems() {
        assert_eq!(
            file_stem("H-alpha  gentle!").ok().as_deref(),
            Some("h-alpha-gentle")
        );
        assert_eq!(file_stem("../../etc").ok().as_deref(), Some("etc"));
        assert!(file_stem("   ").is_err());
        assert!(file_stem("***").is_err());
    }

    #[test]
    fn a_preset_survives_save_list_find_and_remove() {
        let d = dir("rt");
        let p = EditPreset {
            name: "My Ha".into(),
            filter: Some("ha".into()),
            edits: EditList::new(vec![Op::Unsharp(Unsharp::default())]),
        };
        let path = save(&d, &p).unwrap_or_else(|e| panic!("{e}"));
        assert!(path.ends_with("my-ha.json"));
        assert_eq!(list(&d), vec![p.clone()]);
        assert_eq!(find("my ha", Some(&d)).ok(), Some(p.clone()));
        assert_eq!(find(&path.to_string_lossy(), None).ok(), Some(p));
        remove(&d, "My Ha").unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(list(&d).len(), 0);
        remove(&d, "My Ha").unwrap_or_else(|e| panic!("{e}"));
        assert!(find("my ha", Some(&d)).is_err());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_bare_edit_list_and_a_damaged_file_are_handled() {
        let d = dir("bare");
        std::fs::create_dir_all(&d).unwrap_or_else(|e| panic!("{e}"));
        std::fs::write(d.join("bare.json"), r#"[{"op":"flip","axis":"vertical"}]"#)
            .unwrap_or_else(|e| panic!("{e}"));
        std::fs::write(d.join("broken.json"), "{ nope").unwrap_or_else(|e| panic!("{e}"));
        let l = list(&d);
        assert_eq!(l.len(), 1);
        assert_eq!(l[0].name, "bare");
        let _ = std::fs::remove_dir_all(&d);
    }
}
