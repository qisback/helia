// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! The GUI's settings file: what the user has already been told, and the
//! colour they last chose for each filter.
//!
//! A plain `key=value` text file so it can be read and edited by hand:
//!
//! ```text
//! tips_dismissed=true
//! colour.ha=cak,false,0.80
//! backend=auto
//! export_format=tiff
//! last_open_dir=/home/me/captures
//! ```
//!
//! `colour.<filter>=<preset or mono>,<invert>,<strength>`. Besides the tips
//! and colours it remembers the backend, the export format, outlier rejection,
//! full-size preview and the folders last opened from and exported to. Keep
//! count and alignment-point size are not remembered: they come from each
//! capture's recommendation. The file is looked
//! for at `$HELIA_GUI_CONFIG`, else `%APPDATA%\helia\gui.conf`, else
//! `$XDG_CONFIG_HOME/helia/gui.conf`, else `~/.config/helia/gui.conf`. A
//! missing or damaged file is not an error: the defaults apply, and nothing is
//! written until something changes.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use helia_core::stack::RobustKind;
use helia_core::{ColourParams, Preset};
use helia_gpu::BackendChoice;

use crate::app::Settings;
use crate::model::ExportFormat;

/// How many recent captures are remembered.
pub const MAX_RECENT: usize = 8;

/// The settings file format this build writes. Older files (no `schema` line) are
/// format 1; a file with a higher number is read-only.
pub const SCHEMA: u32 = 1;

/// Persisted choices.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Prefs {
    /// The first-run tips strip was dismissed.
    pub tips_dismissed: bool,
    /// Last colour chosen per filter id (`ha`, `cak`, ...).
    pub colours: BTreeMap<String, ColourParams>,
    /// Address of the feedback relay, when not the default
    /// (`HELIA_FEEDBACK_URL` overrides it).
    pub feedback_url: Option<String>,
    /// Folder the last capture was opened from.
    pub last_open_dir: Option<PathBuf>,
    /// Folder the last stack was exported to.
    pub last_export_dir: Option<PathBuf>,
    /// Remembered backend choice.
    pub backend: Option<BackendChoice>,
    /// Remembered export format.
    pub export_format: Option<ExportFormat>,
    /// Remembered outlier rejection: `off`, `sigma`, `huber` or `winsor`.
    pub robust: Option<String>,
    /// Remembered full-size preview switch.
    pub full_preview: Option<bool>,
    /// Captures opened lately, newest first (at most [`MAX_RECENT`]).
    pub recent: Vec<PathBuf>,
    /// Inner size of the window when it was last closed, in points.
    pub window: Option<[f32; 2]>,
    /// The file was written by a newer Helia (its `schema` is above [`SCHEMA`]). Its
    /// settings are read as far as they are understood and the file is never
    /// rewritten, so going back to an older Helia cannot lose newer settings.
    pub newer_schema: Option<u32>,
    path: Option<PathBuf>,
}

fn robust_id(r: Option<RobustKind>) -> &'static str {
    match r {
        None => "off",
        Some(RobustKind::Sigma) => "sigma",
        Some(RobustKind::Huber) => "huber",
        Some(RobustKind::Winsor) => "winsor",
    }
}

/// The choice for a known id (`None` is off); `Err` for anything else.
fn robust_from_id(v: &str) -> Result<Option<RobustKind>, ()> {
    match v {
        "off" => Ok(None),
        "sigma" => Ok(Some(RobustKind::Sigma)),
        "huber" => Ok(Some(RobustKind::Huber)),
        "winsor" => Ok(Some(RobustKind::Winsor)),
        _ => Err(()),
    }
}

/// A window size worth restoring: not tiny, not absurd.
fn valid_window(s: [f32; 2]) -> bool {
    (640.0..=16384.0).contains(&s[0]) && (480.0..=16384.0).contains(&s[1])
}

fn parse_window(v: &str) -> Option<[f32; 2]> {
    let (w, h) = v.split_once('x')?;
    let s = [w.trim().parse().ok()?, h.trim().parse().ok()?];
    valid_window(s).then_some(s)
}

fn backend_id(b: BackendChoice) -> &'static str {
    match b {
        BackendChoice::Auto => "auto",
        BackendChoice::Cpu => "cpu",
        BackendChoice::Gpu => "gpu",
    }
}

fn format_id(f: ExportFormat) -> &'static str {
    match f {
        ExportFormat::Tiff => "tiff",
        ExportFormat::Png => "png",
        ExportFormat::Fits => "fits",
    }
}

fn parse_format(s: &str) -> Option<ExportFormat> {
    [ExportFormat::Tiff, ExportFormat::Png, ExportFormat::Fits]
        .into_iter()
        .find(|f| format_id(*f) == s)
}

fn absolute_dir(v: &str) -> Option<PathBuf> {
    Some(PathBuf::from(v)).filter(|d| !v.is_empty() && d.is_absolute())
}

fn parse_colour(value: &str) -> Option<ColourParams> {
    let mut it = value.split(',').map(str::trim);
    let preset = match it.next()? {
        "mono" => None,
        id => Some(id.parse::<Preset>().ok()?),
    };
    let invert = it.next()?.parse::<bool>().ok()?;
    let strength = it.next()?.parse::<f32>().ok()?;
    let grade = match it.next() {
        Some(g) => helia_core::grade::Grade::parse(&g.replace(';', ",")).ok()?,
        None => helia_core::grade::Grade::default(),
    };
    (0.0..=1.0).contains(&strength).then_some(ColourParams {
        preset,
        invert,
        strength,
        grade,
    })
}

impl Prefs {
    /// Where the file lives, if a place can be worked out.
    #[must_use]
    pub fn default_path() -> Option<PathBuf> {
        let var = |k: &str| {
            std::env::var_os(k)
                .filter(|v| !v.is_empty())
                .map(PathBuf::from)
        };
        if let Some(p) = var("HELIA_GUI_CONFIG") {
            return Some(p);
        }
        let dir = var("APPDATA")
            .or_else(|| var("XDG_CONFIG_HOME"))
            .or_else(|| var("HOME").map(|h| h.join(".config")))?;
        Some(dir.join("helia").join("gui.conf"))
    }

    /// Read the default file (or start empty).
    #[must_use]
    pub fn load_default() -> Self {
        Self::default_path().map_or_else(Self::default, |p| Self::load(&p))
    }

    /// Read `path`; a missing or unreadable file gives the defaults, still
    /// remembering `path` for saving.
    #[must_use]
    pub fn load(path: &Path) -> Self {
        // A settings file is a few lines; refuse to read anything large.
        let text = std::fs::metadata(path)
            .ok()
            .filter(|m| m.len() <= 64 * 1024)
            .and_then(|_| std::fs::read_to_string(path).ok())
            .unwrap_or_default();
        let mut p = Self::parse(&text);
        p.path = Some(path.to_owned());
        p
    }

    /// Parse file text; unknown or malformed lines are skipped.
    #[must_use]
    pub fn parse(text: &str) -> Self {
        let mut p = Self::default();
        for line in text.lines().map(str::trim) {
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let Some((k, v)) = line.split_once('=') else {
                continue;
            };
            let (k, v) = (k.trim(), v.trim());
            if k == "schema" {
                p.newer_schema = v.parse::<u32>().ok().filter(|n| *n > SCHEMA);
            } else if k == "tips_dismissed" {
                p.tips_dismissed = v == "true";
            } else if k == "last_open_dir" {
                p.last_open_dir = absolute_dir(v);
            } else if k == "last_export_dir" {
                p.last_export_dir = absolute_dir(v);
            } else if k == "backend" {
                p.backend = v.parse().ok();
            } else if k == "export_format" {
                p.export_format = parse_format(v);
            } else if k == "robust" {
                p.robust = Some(v.to_owned()).filter(|v| robust_from_id(v).is_ok());
            } else if k == "sigma_clip" && p.robust.is_none() {
                // Files from before the robust modes: the sigma switch.
                p.robust = v
                    .parse::<bool>()
                    .ok()
                    .map(|on| (if on { "sigma" } else { "off" }).to_owned());
            } else if k == "full_preview" {
                p.full_preview = v.parse().ok();
            } else if k == "recent" {
                if let Some(f) = absolute_dir(v)
                    && p.recent.len() < MAX_RECENT
                    && !p.recent.contains(&f)
                {
                    p.recent.push(f);
                }
            } else if k == "window" {
                p.window = parse_window(v);
            } else if k == "feedback_url" {
                p.feedback_url = Some(v.to_owned()).filter(|s| !s.is_empty());
            } else if let Some(filter) = k.strip_prefix("colour.")
                && Preset::from_id(filter).is_some()
                && let Some(c) = parse_colour(v)
            {
                p.colours.insert(filter.to_owned(), c);
            }
        }
        p
    }

    /// The file text.
    #[must_use]
    pub fn to_text(&self) -> String {
        let mut out = String::from("# Helia GUI settings\n");
        let _ = writeln!(out, "schema={SCHEMA}");
        let _ = writeln!(out, "tips_dismissed={}", self.tips_dismissed);
        if let Some(u) = &self.feedback_url {
            let _ = writeln!(out, "feedback_url={u}");
        }
        let mut line = |k: &str, v: Option<String>| {
            if let Some(v) = v {
                let _ = writeln!(out, "{k}={v}");
            }
        };
        line("backend", self.backend.map(|b| backend_id(b).to_owned()));
        line(
            "export_format",
            self.export_format.map(|f| format_id(f).to_owned()),
        );
        line("robust", self.robust.clone());
        line("full_preview", self.full_preview.map(|b| b.to_string()));
        line(
            "last_open_dir",
            self.last_open_dir.as_ref().map(|d| d.display().to_string()),
        );
        line(
            "last_export_dir",
            self.last_export_dir
                .as_ref()
                .map(|d| d.display().to_string()),
        );
        line(
            "window",
            self.window
                .map(|[w, h]| format!("{}x{}", w.round(), h.round())),
        );
        for f in &self.recent {
            let _ = writeln!(out, "recent={}", f.display());
        }
        for (filter, c) in &self.colours {
            let _ = writeln!(
                out,
                "colour.{filter}={},{},{:.2},{}",
                c.preset.map_or("mono", Preset::id),
                c.invert,
                c.strength,
                c.grade.to_text().replace(',', ";")
            );
        }
        out
    }

    /// Write the file if this is a file-backed instance. Failure is ignored:
    /// losing a remembered choice is not worth interrupting the user.
    pub fn save(&self) {
        let Some(path) = &self.path else { return };
        if self.newer_schema.is_some() {
            return;
        }
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(path, self.to_text());
    }

    /// Remember `colour` for `filter` and save.
    pub fn remember_colour(&mut self, filter: Preset, colour: ColourParams) {
        if self.colours.get(filter.id()) != Some(&colour) {
            self.colours.insert(filter.id().to_owned(), colour);
            self.save();
        }
    }

    /// Put the remembered choices into `s` (at start-up).
    pub fn apply_to(&self, s: &mut Settings) {
        if let Some(b) = self.backend {
            s.backend = b;
        }
        if let Some(f) = self.export_format {
            s.export_format = f;
        }
        if let Some(v) = self.robust.as_deref().and_then(|v| robust_from_id(v).ok()) {
            s.robust = v;
        }
        if let Some(v) = self.full_preview {
            s.full_preview = v;
        }
    }

    /// Remember the choices in `s`, saving when any changed.
    pub fn remember_settings(&mut self, s: &Settings) {
        let now = (
            Some(s.backend),
            Some(s.export_format),
            Some(robust_id(s.robust).to_owned()),
            Some(s.full_preview),
        );
        let before = (
            self.backend,
            self.export_format,
            self.robust.clone(),
            self.full_preview,
        );
        if now != before {
            (
                self.backend,
                self.export_format,
                self.robust,
                self.full_preview,
            ) = now;
            self.save();
        }
    }

    /// Remember the folder of an opened capture and save.
    pub fn remember_open_dir(&mut self, file: &Path) {
        let dir = file
            .parent()
            .filter(|d| d.is_absolute())
            .map(Path::to_owned);
        if dir.is_some() && dir != self.last_open_dir {
            self.last_open_dir = dir;
            self.save();
        }
    }

    /// Put `file` at the head of the recent list and save.
    pub fn remember_recent(&mut self, file: &Path) {
        if !file.is_absolute() || self.recent.first().is_some_and(|f| f == file) {
            return;
        }
        self.recent.retain(|f| f != file);
        self.recent.insert(0, file.to_owned());
        self.recent.truncate(MAX_RECENT);
        self.save();
    }

    /// Forget one recent file (it no longer exists) and save.
    pub fn forget_recent(&mut self, file: &Path) {
        let before = self.recent.len();
        self.recent.retain(|f| f != file);
        if self.recent.len() != before {
            self.save();
        }
    }

    /// Remember the window size when it differs from the saved one.
    pub fn remember_window(&mut self, size: [f32; 2]) {
        let size = [size[0].round(), size[1].round()];
        if valid_window(size) && self.window != Some(size) {
            self.window = Some(size);
            self.save();
        }
    }

    /// Remember the folder of an exported file and save.
    pub fn remember_export_dir(&mut self, file: &Path) {
        let dir = file
            .parent()
            .filter(|d| d.is_absolute())
            .map(Path::to_owned);
        if dir.is_some() && dir != self.last_export_dir {
            self.last_export_dir = dir;
            self.save();
        }
    }

    /// The settings as text for a bug report: everything but folder names,
    /// which can carry a user name.
    #[must_use]
    pub fn report_text(&self) -> String {
        let mut p = self.clone();
        let said = |d: &Option<PathBuf>| if d.is_some() { "set" } else { "unset" };
        let folders = format!(
            "# folders (names left out): open {}, export {}\n",
            said(&p.last_open_dir),
            said(&p.last_export_dir)
        );
        p.last_open_dir = None;
        p.last_export_dir = None;
        p.recent.clear();
        format!("{folders}{}", p.to_text())
    }

    /// Dismiss the tips for good and save.
    pub fn dismiss_tips(&mut self) {
        if !self.tips_dismissed {
            self.tips_dismissed = true;
            self.save();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_round_trips() {
        let mut p = Prefs {
            tips_dismissed: true,
            feedback_url: Some("https://relay.example".into()),
            ..Prefs::default()
        };
        p.colours.insert(
            "ha".into(),
            ColourParams {
                preset: Some(Preset::CaK),
                invert: true,
                strength: 0.8,
                grade: helia_core::grade::Grade::parse("hue=-8,sat=1.1").expect("grade"),
            },
        );
        p.colours.insert(
            "mg".into(),
            ColourParams {
                preset: None,
                invert: false,
                strength: 1.0,
                grade: helia_core::grade::Grade::default(),
            },
        );
        let back = Prefs::parse(&p.to_text());
        assert_eq!(back, p);
    }

    #[test]
    fn settings_and_folders_round_trip_and_junk_is_ignored() {
        let p = Prefs {
            backend: Some(BackendChoice::Cpu),
            export_format: Some(ExportFormat::Fits),
            robust: Some("huber".to_owned()),
            full_preview: Some(true),
            last_open_dir: Some(std::env::temp_dir()),
            ..Prefs::default()
        };
        assert_eq!(Prefs::parse(&p.to_text()), p);
        let bad = Prefs::parse(
            "backend=quantum\nexport_format=bmp\nrobust=median\nlast_open_dir=relative/dir\nsigma_clip=maybe\n",
        );
        assert_eq!(bad, Prefs::default());
        let mut s = Settings::default();
        Prefs {
            backend: Some(BackendChoice::Gpu),
            robust: Some("winsor".to_owned()),
            ..Prefs::default()
        }
        .apply_to(&mut s);
        assert_eq!(s.backend, BackendChoice::Gpu);
        assert_eq!(s.robust, Some(RobustKind::Winsor));
        assert_eq!(s.export_format, ExportFormat::Tiff, "unset stays default");
    }

    #[test]
    fn the_old_sigma_switch_is_read_as_a_robust_choice() {
        assert_eq!(
            Prefs::parse("sigma_clip=true\n").robust.as_deref(),
            Some("sigma")
        );
        assert_eq!(
            Prefs::parse("robust=winsor\nsigma_clip=true\n")
                .robust
                .as_deref(),
            Some("winsor")
        );
    }

    #[test]
    fn damaged_lines_are_skipped_not_fatal() {
        let p = Prefs::parse(
            "tips_dismissed=true\ncolour.ha=teal,false,1\ncolour.nope=ha,false,1\n\
             colour.cak=cak,maybe,1\ncolour.mg=mg,true,7\nrubbish\ncolour.na=na,true,0.50\n",
        );
        assert!(p.tips_dismissed);
        assert_eq!(p.colours.len(), 1, "{:?}", p.colours);
        assert_eq!(p.colours["na"].preset, Some(Preset::Na));
        assert!(p.colours["na"].invert);
    }

    #[test]
    fn the_file_is_written_only_when_something_changes() {
        let dir = std::env::temp_dir().join(format!("helia-prefs-{}", std::process::id()));
        let path = dir.join("nested").join("gui.conf");
        let mut p = Prefs::load(&path);
        assert!(
            !p.tips_dismissed && !path.exists(),
            "loading writes nothing"
        );
        p.dismiss_tips();
        assert!(Prefs::load(&path).tips_dismissed);
        p.remember_colour(Preset::Ha, ColourParams::preset(Preset::Mg));
        let again = Prefs::load(&path);
        assert_eq!(again.colours["ha"].preset, Some(Preset::Mg));
        assert!(again.tips_dismissed, "earlier choices survive");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_file_from_a_newer_helia_is_read_but_never_rewritten() {
        let dir = std::env::temp_dir().join(format!("helia-prefs-newer-{}", std::process::id()));
        let path = dir.join("gui.conf");
        std::fs::create_dir_all(&dir).expect("dir");
        let text = "schema=9\ntips_dismissed=true\nfuture_setting=1\n";
        std::fs::write(&path, text).expect("write");
        let mut p = Prefs::load(&path);
        assert_eq!(p.newer_schema, Some(9));
        assert!(p.tips_dismissed, "what is understood is still used");
        p.remember_recent(&std::env::temp_dir().join("x.ser"));
        assert_eq!(std::fs::read_to_string(&path).expect("read"), text);
        assert_eq!(Prefs::parse("schema=1\n").newer_schema, None);
        assert_eq!(Prefs::parse("").newer_schema, None);
        assert!(Prefs::default().to_text().contains("schema=1"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_in_memory_instance_never_touches_the_disk() {
        let mut p = Prefs::default();
        p.dismiss_tips();
        p.remember_colour(Preset::Ha, ColourParams::default());
        assert!(p.tips_dismissed);
    }
    #[test]
    fn recent_files_and_window_size_round_trip() {
        let a = std::env::temp_dir().join("a.ser");
        let b = std::env::temp_dir().join("b.ser");
        let mut p = Prefs::default();
        p.remember_recent(&a);
        p.remember_recent(&b);
        p.remember_recent(&a);
        p.remember_recent(Path::new("relative.ser"));
        assert_eq!(p.recent, vec![a.clone(), b.clone()]);
        p.remember_window([1500.4, 900.0]);
        p.remember_window([10.0, 10.0]);
        assert_eq!(p.window, Some([1500.0, 900.0]));
        let q = Prefs::parse(&p.to_text());
        assert_eq!(q.recent, vec![a.clone(), b]);
        assert_eq!(q.window, Some([1500.0, 900.0]));
        assert!(
            !p.report_text().contains("a.ser"),
            "file names stay out of bug reports"
        );
        p.forget_recent(&a);
        assert_eq!(p.recent.len(), 1);
    }

    #[test]
    fn recent_list_is_capped_and_junk_window_ignored() {
        let mut p = Prefs::default();
        for i in 0..20 {
            p.remember_recent(&std::env::temp_dir().join(format!("{i}.ser")));
        }
        assert_eq!(p.recent.len(), MAX_RECENT);
        assert_eq!(Prefs::parse("window=axb\nwindow=100x100\n").window, None);
    }
}
