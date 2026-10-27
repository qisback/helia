// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! SOLARNET metadata for FITS output.
//!
//! The SOLARNET Metadata Recommendations (Haugan and Fredvik, arXiv:2011.12139)
//! say what a solar observation file should carry so that tools such as
//! `sunpy` and `astropy` can place it in time, space and wavelength. This module
//! gathers what Helia knows about a stack (the capture's own header and
//! timestamps, the camera sidecar, the filter preset, the limb fit and the
//! processing that ran) and turns it into header cards.
//!
//! Nothing is invented. A value that cannot be known is left out, and the
//! file says so with `SOLARNET = 0.5` (partially compliant) and `COMMENT`
//! cards. In particular the world coordinate system is *approximate*: the
//! scale comes from the fitted limb radius and a solar radius computed from the
//! date, and the orientation of the picture on the sky is not known.
//!
//! The keywords are listed in the book, under *Output formats*.
//!
//! # References
//!
//! - Haugan, S. V. H., Fredvik, T. et al., SOLARNET metadata recommendations for solar observations,
//!   arXiv:2011.12139.
//! - Pence, W. D. et al. (2010), Definition of the Flexible Image Transport System (FITS), version 3.0,
//!   Astron. Astrophys. 524, A42, doi:10.1051/0004-6361/201015362.
//! - Greisen, E. W. and Calabretta, M. R. (2002), Representations of world coordinates in FITS, Astron.
//!   Astrophys. 395, 1061-1075, doi:10.1051/0004-6361:20021326; Calabretta, M. R. and Greisen, E. W.
//!   (2002), Representations of celestial coordinates in FITS, Astron. Astrophys. 395, 1077-1122,
//!   doi:10.1051/0004-6361:20021327.
//! - Thompson, W. T. (2006), Coordinate systems for solar image data, Astron. Astrophys. 449, 791-803,
//!   doi:10.1051/0004-6361:20054262 (the helioprojective `HPLN-TAN` and `HPLT-TAN` axes).

#![allow(
    clippy::many_single_char_names,
    reason = "x, y, z, r, d, m, s are the natural names in coordinate and calendar maths"
)]

use std::fs::File;
use std::io::BufReader;
use std::path::Path;
use std::str::FromStr;

use crate::align::method::AlignReport;
use crate::colour::Preset;
use crate::debayer::Debayer;
use crate::error::Error;
use crate::io::fits::HeaderCard;
use crate::io::ser::SerReader;
use crate::io::sidecar::CameraSettings;
use crate::mask::{self, Disc};
use crate::pipeline::{StackConfig, StackOutput};
use crate::stack::StackMode;

/// Nominal solar radius, metres (IAU 2015 resolution B3).
pub const RSUN_REF_M: f64 = 695_700_000.0;
/// Astronomical unit, metres.
pub const AU_M: f64 = 149_597_870_700.0;
/// Ticks of 100 ns from 0001-01-01 to 1970-01-01, the SER timestamp epoch.
const SER_UNIX_EPOCH_TICKS: i64 = 621_355_968_000_000_000;

/// Where the telescope stands.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Site {
    /// Geodetic latitude, degrees north (-90 to 90).
    pub lat_deg: f64,
    /// Longitude, degrees east (-180 to 180).
    pub lon_deg: f64,
    /// Height above the WGS 84 ellipsoid, metres.
    pub alt_m: f64,
}

impl FromStr for Site {
    type Err = Error;

    /// `lat,lon,alt` or `lat,lon` (altitude 0): degrees north, degrees east,
    /// metres.
    fn from_str(s: &str) -> Result<Self, Error> {
        let bad = || {
            Error::Invalid(format!(
                "invalid site {s:?}; use LAT,LON,ALT: degrees north, degrees east, metres (e.g. 51.5,-0.1,30)"
            ))
        };
        let parts: Vec<f64> = s
            .split(',')
            .map(|p| p.trim().parse::<f64>().map_err(|_| bad()))
            .collect::<Result<_, _>>()?;
        let (lat, lon, alt) = match parts[..] {
            [lat, lon] => (lat, lon, 0.0),
            [lat, lon, alt] => (lat, lon, alt),
            _ => return Err(bad()),
        };
        if !(-90.0..=90.0).contains(&lat)
            || !(-180.0..=180.0).contains(&lon)
            || !(-1000.0..=10_000.0).contains(&alt)
        {
            return Err(bad());
        }
        Ok(Self {
            lat_deg: lat,
            lon_deg: lon,
            alt_m: alt,
        })
    }
}

impl Site {
    /// The site named by the `HELIA_SITE` environment variable (`lat,lon,alt`),
    /// if it is set and valid.
    #[must_use]
    pub fn from_env() -> Option<Self> {
        std::env::var("HELIA_SITE").ok()?.parse().ok()
    }

    /// Earth-fixed Cartesian coordinates (ITRS, WGS 84), metres.
    #[must_use]
    pub fn itrs(&self) -> [f64; 3] {
        const A: f64 = 6_378_137.0;
        const F: f64 = 1.0 / 298.257_223_563;
        let e2 = F * (2.0 - F);
        let (lat, lon) = (self.lat_deg.to_radians(), self.lon_deg.to_radians());
        let n = A / (1.0 - e2 * lat.sin().powi(2)).sqrt();
        [
            (n + self.alt_m) * lat.cos() * lon.cos(),
            (n + self.alt_m) * lat.cos() * lon.sin(),
            (n * (1.0 - e2) + self.alt_m) * lat.sin(),
        ]
    }
}

/// Seconds since 1970-01-01T00:00:00 UTC for a SER timestamp, or `None` when
/// the stamp is unset or implausible (before 1990 or after 2200).
#[must_use]
pub fn ser_ticks_to_unix(ticks: i64) -> Option<f64> {
    let unix = ticks.checked_sub(SER_UNIX_EPOCH_TICKS)? as f64 / 1e7;
    (631_152_000.0..7_258_118_400.0)
        .contains(&unix)
        .then_some(unix)
}

/// ISO 8601 UTC with millisecond precision, `YYYY-MM-DDThh:mm:ss.fff`.
#[must_use]
pub fn iso_from_unix(unix: f64) -> String {
    let ms_total = (unix * 1000.0).round() as i64;
    let (secs, ms) = (ms_total.div_euclid(1000), ms_total.rem_euclid(1000));
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // Days to civil date (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.{ms:03}",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

/// Unix seconds UTC for `YYYY-MM-DD`, `YYYY-MM-DDThh:mm[:ss[.fff]]` or the
/// same with a space (a trailing `Z` is allowed); `None` when it is not one.
#[must_use]
pub fn unix_from_iso(text: &str) -> Option<f64> {
    let t = text.trim().trim_end_matches('Z');
    let (date, time) = t
        .split_once(['T', ' '])
        .map_or((t, ""), |(d, tm)| (d, tm.trim()));
    let mut d = date.split('-');
    let (y, m, day): (i64, i64, i64) = (
        d.next()?.parse().ok()?,
        d.next()?.parse().ok()?,
        d.next()?.parse().ok()?,
    );
    if d.next().is_some() || !(1..=12).contains(&m) || !(1..=31).contains(&day) {
        return None;
    }
    let mut secs = 0.0;
    if !time.is_empty() {
        let mut parts = time.split(':');
        let hh: f64 = parts.next()?.parse().ok()?;
        let mm: f64 = parts.next()?.parse().ok()?;
        let ss: f64 = parts.next().map_or(Some(0.0), |p| p.parse().ok())?;
        if parts.next().is_some() || hh >= 24.0 || mm >= 60.0 || ss >= 61.0 {
            return None;
        }
        secs = hh * 3600.0 + mm * 60.0 + ss;
    }
    // Civil date to days since 1970-01-01 (Howard Hinnant's algorithm).
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(days as f64 * 86_400.0 + secs)
}

/// The time a FITS file says it was taken (`DATE-AVG`, else `DATE-OBS`),
/// unix seconds UTC.
#[must_use]
pub fn fits_time(path: &Path) -> Option<f64> {
    let f = File::open(path).ok()?;
    let (_, map) = crate::io::fits::read_fits_planes(BufReader::new(f)).ok()?;
    ["DATE-AVG", "DATE-OBS", "DATE-BEG"]
        .iter()
        .find_map(|k| map.get(*k).and_then(|v| unix_from_iso(v)))
}

impl CaptureInfo {
    /// Middle of the capture: the mean of the frame stamps, else the header's
    /// start time.
    #[must_use]
    pub fn mid_unix(&self) -> Option<f64> {
        let t: Vec<f64> = self.frame_unix.iter().flatten().copied().collect();
        if t.is_empty() {
            self.start_unix
        } else {
            Some(t.iter().sum::<f64>() / t.len() as f64)
        }
    }
}

/// Earth to Sun distance in astronomical units at `unix` seconds (the low
/// precision series of the Astronomical Almanac, good to about 1e-4 AU).
#[must_use]
pub fn sun_distance_au(unix: f64) -> f64 {
    let jd = unix / 86_400.0 + 2_440_587.5;
    let g = (357.528 + 0.985_600_28 * (jd - 2_451_545.0)).to_radians();
    1.000_14 - 0.016_71 * g.cos() - 0.000_14 * (2.0 * g).cos()
}

/// Apparent solar radius in arcseconds seen from `distance_au`.
#[must_use]
pub fn solar_radius_arcsec(distance_au: f64) -> f64 {
    (RSUN_REF_M / (distance_au * AU_M)).atan().to_degrees() * 3600.0
}

/// Exposure in seconds from a `SharpCap` value such as `10.4480ms`.
fn parse_exposure(text: &str) -> Option<f64> {
    let t = text.trim().to_ascii_lowercase();
    let split = t.find(|c: char| !(c.is_ascii_digit() || c == '.' || c == '-'))?;
    let (num, unit) = t.split_at(split);
    let v: f64 = num.parse().ok()?;
    let scale = match unit.trim() {
        "ms" => 1e-3,
        "us" | "\u{b5}s" => 1e-6,
        "s" | "sec" => 1.0,
        _ => return None,
    };
    let s = v * scale;
    (s.is_finite() && s > 0.0).then_some(s)
}

/// What the capture file and its neighbours say about the observation.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CaptureInfo {
    /// SER telescope string.
    pub telescope: Option<String>,
    /// SER instrument string, else the camera named in the sidecar.
    pub instrument: Option<String>,
    /// SER observer string.
    pub observer: Option<String>,
    /// UTC time of the first frame from the SER header, unix seconds.
    pub start_unix: Option<f64>,
    /// UTC time of every frame from the SER trailer, unix seconds (`None` for
    /// a frame whose stamp is unset or implausible).
    pub frame_unix: Vec<Option<f64>>,
    /// Exposure of one frame in seconds, from the `SharpCap` sidecar.
    pub exposure_s: Option<f64>,
    /// The filter preset the stack was made for.
    pub filter: Option<Preset>,
}

impl CaptureInfo {
    /// Read what `capture` carries. Unreadable parts are simply absent.
    /// `filter` is the preset chosen or detected for the run.
    #[must_use]
    pub fn read(capture: &Path, filter: Option<Preset>) -> Self {
        let mut info = Self {
            filter,
            ..Self::default()
        };
        let text = |s: &str| {
            let t = s.trim();
            (!t.is_empty()).then(|| t.to_owned())
        };
        if let Some(s) = CameraSettings::for_capture(capture) {
            info.exposure_s = s.get("Exposure").and_then(parse_exposure);
            info.instrument = s.sections.first().and_then(|n| text(n));
        }
        if capture
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("ser"))
            && let Ok(f) = File::open(capture)
            && let Ok(mut r) = SerReader::new_lenient(BufReader::new(f))
        {
            let h = r.header().clone();
            info.telescope = text(&h.telescope);
            info.observer = text(&h.observer);
            if let Some(i) = text(&h.instrument) {
                info.instrument = Some(i);
            }
            info.start_unix = ser_ticks_to_unix(h.date_time_utc);
            if let Ok(Some(ticks)) = r.timestamps() {
                info.frame_unix = ticks.into_iter().map(ser_ticks_to_unix).collect();
            }
        }
        if crate::io::fitsseq::is_fits_input(capture)
            && let Ok(scan) = crate::io::fitsseq::Scan::read(capture)
        {
            info.frame_unix.clone_from(&scan.times);
            info.start_unix = scan.times.iter().flatten().copied().reduce(f64::min);
            info.exposure_s = info.exposure_s.or(scan.exposure_s);
        }
        info
    }
}

/// One processing step for the `PRSTEPn` family.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProvStep {
    /// What kind of step, upper case with dashes (`PRSTEPn`).
    pub step: String,
    /// What did it (`PRPROCn`).
    pub process: String,
    /// Settings, short (`PRPARAn`).
    pub params: String,
}

impl ProvStep {
    pub(crate) fn new(step: &str, process: &str, params: impl Into<String>) -> Self {
        Self {
            step: step.into(),
            process: process.into(),
            params: params.into(),
        }
    }
}

/// The processing steps a run with `cfg` performs, in order. `bayer` says the
/// frames were raw colour; `masked` counts the rectangles that were flattened.
#[must_use]
pub fn pipeline_steps(
    cfg: &StackConfig,
    bayer: bool,
    frames: usize,
    kept: usize,
    masked: usize,
    disc: bool,
    align: &AlignReport,
) -> Vec<ProvStep> {
    let mut v = cfg
        .calibration
        .as_ref()
        .map_or_else(Vec::new, |c| c.provenance());
    v.push(ProvStep::new(
        "FRAME-SELECTION",
        "helia-core quality",
        format!("metric={:?}; kept {kept} of {frames}", cfg.metric),
    ));
    if bayer {
        v.push(ProvStep::new(
            "DEBAYER",
            "helia-core debayer",
            debayer_params(cfg.debayer),
        ));
    }
    if masked > 0 || disc {
        v.push(ProvStep::new(
            "MASKING",
            "helia-core mask",
            format!(
                "{masked} flattened region(s); limb fit {}",
                if disc { "on" } else { "off" }
            ),
        ));
    }
    let confidence = align
        .confidence_mean
        .map_or(String::new(), |c| format!("; confidence {c:.2}"));
    v.push(ProvStep::new(
        "GLOBAL-ALIGNMENT",
        &format!("helia-core align {}", align.method.name()),
        format!(
            "reference of {} best frame(s){confidence}",
            cfg.reference_frames
        ),
    ));
    if let Some(mp) = &cfg.multipoint {
        v.push(ProvStep::new(
            "LOCAL-ALIGNMENT",
            "helia-core align multipoint",
            format!("alignment points of {} px", mp.ap_size),
        ));
    }
    let mode = match cfg.mode {
        StackMode::Mean => "weighted mean".to_owned(),
        StackMode::SigmaClip { kappa } => format!("sigma clip {kappa}"),
        StackMode::Huber { k } => format!("Huber {k}"),
        StackMode::Winsor { k } => format!("winsorised mean {k}"),
    };
    v.push(ProvStep::new(
        "STACKING",
        "helia-core stack",
        format!("{mode}; kernel={:?}", cfg.kernel),
    ));
    if cfg.normalise {
        v.push(ProvStep::new(
            "NORMALISATION",
            "helia-core pipeline",
            "99.99th percentile scaled to 1",
        ));
    }
    v
}

fn debayer_params(d: Debayer) -> String {
    format!("method={:?}; channel={:?}", d.method, d.channel)
}

/// Wavelength of a preset in nanometres (air) with a note, or `None` for a
/// broad-band preset.
fn preset_wavelength(p: Preset) -> Option<(f64, &'static str)> {
    Some(match p {
        Preset::Ha => (656.28, "H-alpha"),
        Preset::CaK => (393.37, "Ca II K"),
        Preset::CaH => (396.85, "Ca II H"),
        Preset::Mg => (517.27, "Mg I b2"),
        Preset::Na => (589.3, "mean of Na I D1 and D2"),
        Preset::HeD3 => (587.56, "He I D3"),
        Preset::Hbeta => (486.13, "H-beta"),
        Preset::White => return None,
    })
}

/// Everything the FITS header is built from.
#[derive(Clone, Debug, PartialEq)]
pub struct FitsMeta {
    /// Telescope.
    pub telescope: Option<String>,
    /// Instrument or camera.
    pub instrument: Option<String>,
    /// Observer.
    pub observer: Option<String>,
    /// Filter preset.
    pub filter: Option<Preset>,
    /// First contributing exposure began, unix seconds UTC.
    pub date_beg: Option<f64>,
    /// Last contributing exposure ended (or began, when the exposure is
    /// unknown), unix seconds UTC.
    pub date_end: Option<f64>,
    /// Mean time of the contributing exposures, unix seconds UTC.
    pub date_avg: Option<f64>,
    /// File written, unix seconds UTC.
    pub written: Option<f64>,
    /// Exposure of one frame, seconds.
    pub exposure_s: Option<f64>,
    /// Frames that contributed.
    pub frames_summed: usize,
    /// Observing site.
    pub site: Option<Site>,
    /// Fitted limb: centre and radius in image pixels, top-left origin.
    pub disc: Option<Disc>,
    /// Size of the image the disc was fitted in.
    pub size: (usize, usize),
    /// Processing steps, in order.
    pub steps: Vec<ProvStep>,
    /// Granulation report of the stack, for a photospheric filter.
    pub granulation: Option<crate::granulation::Granulation>,
    /// Seeing estimated from the capture.
    pub seeing: Option<crate::seeing::SeeingReport>,
    /// Sunspot count, Wolf number and area (`SPOT*` cards).
    pub sunspots: Option<crate::sunspots::Report>,
    /// Helia version.
    pub version: String,
    /// Cards a plugin's provenance adds (`HIERARCH HELIA ...`), already checked by the host.
    pub plugin_cards: Vec<HeaderCard>,
}

impl FitsMeta {
    /// Add the provenance cards of the plugins that took part.
    #[must_use]
    pub fn with_plugin_cards(mut self, cards: Vec<HeaderCard>) -> Self {
        self.plugin_cards = cards;
        self
    }

    /// Add the sunspot report (`SPOT*` cards).
    #[must_use]
    pub fn with_sunspots(mut self, s: Option<crate::sunspots::Report>) -> Self {
        self.sunspots = s;
        self
    }

    /// Add the seeing estimate (`SEE*` cards).
    #[must_use]
    pub fn with_seeing(mut self, s: Option<crate::seeing::SeeingReport>) -> Self {
        self.seeing = s;
        self
    }

    /// Add the granulation report of the stack (`GRAN*` cards).
    #[must_use]
    pub fn with_granulation(mut self, g: Option<crate::granulation::Granulation>) -> Self {
        self.granulation = g;
        self
    }

    /// Metadata for an image that `helia derotate` turned: its time, the site
    /// and the one processing step. There is no limb, so no WCS.
    #[must_use]
    pub fn derotated(time: f64, site: Site, turn_deg: f64, reference_unix: f64) -> Self {
        Self {
            telescope: None,
            instrument: None,
            observer: None,
            filter: None,
            date_beg: None,
            date_end: None,
            date_avg: Some(time),
            written: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .ok()
                .map(|d| d.as_secs_f64()),
            exposure_s: None,
            frames_summed: 0,
            site: Some(site),
            disc: None,
            size: (0, 0),
            plugin_cards: Vec::new(),
            steps: vec![ProvStep::new(
                "FIELD-ROTATION",
                "helia derotate",
                format!(
                    "turned {turn_deg:+.3} deg counter-clockwise to the orientation at {}",
                    iso_from_unix(reference_unix)
                ),
            )],
            granulation: None,
            seeing: None,
            sunspots: None,
            version: env!("CARGO_PKG_VERSION").to_owned(),
        }
    }

    /// Metadata for the stack `out`, from what `info` says about the capture
    /// and the user-given `site`. The limb comes from the run's own fit, else
    /// from a fit to the stacked image.
    #[must_use]
    pub fn from_stack(info: &CaptureInfo, out: &StackOutput, site: Option<Site>) -> Self {
        let times: Vec<f64> = out
            .kept
            .iter()
            .filter_map(|&i| info.frame_unix.get(i).copied().flatten())
            .collect();
        let exp = info.exposure_s.unwrap_or(0.0);
        let (date_beg, date_end, date_avg) = if times.is_empty() {
            (info.start_unix, None, info.start_unix)
        } else {
            let lo = times.iter().copied().fold(f64::INFINITY, f64::min);
            let hi = times.iter().copied().fold(f64::NEG_INFINITY, f64::max);
            (
                Some(lo),
                Some(hi + exp),
                Some(times.iter().sum::<f64>() / times.len() as f64),
            )
        };
        let written = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()
            .map(|d| d.as_secs_f64());
        Self {
            telescope: info.telescope.clone(),
            instrument: info.instrument.clone(),
            observer: info.observer.clone(),
            filter: info.filter,
            date_beg,
            date_end,
            date_avg,
            written,
            exposure_s: info.exposure_s,
            frames_summed: out.kept.len(),
            site,
            disc: out.disc.or_else(|| mask::fit_disc(&out.image)),
            size: (out.image.width, out.image.height),
            plugin_cards: Vec::new(),
            steps: out.provenance.clone(),
            granulation: None,
            seeing: None,
            sunspots: None,
            version: env!("CARGO_PKG_VERSION").to_owned(),
        }
    }

    /// Plate scale of the image in arcseconds per pixel, when a limb is known.
    #[must_use]
    pub fn arcsec_per_pixel(&self) -> Option<f64> {
        let d = self.disc?;
        let when = self.date_avg.or(self.date_beg);
        let dist = when.map_or(1.0, sun_distance_au);
        Some(solar_radius_arcsec(dist) / f64::from(d.r))
    }

    /// The header cards, in order.
    #[must_use]
    #[allow(clippy::too_many_lines, reason = "one flat list of header cards")]
    pub fn cards(&self) -> Vec<HeaderCard> {
        use HeaderCard as C;
        let mut v = Vec::new();
        let iso = |t: f64| iso_from_unix(t);
        v.push(C::string("EXTNAME", "STACKED-IMAGE", "Name of HDU"));
        v.push(C::float(
            "SOLARNET",
            0.5,
            "Partially SOLARNET-compliant (see COMMENT)",
        ));
        v.push(C::int("OBS_HDU", 1, "This HDU contains observational data"));
        if let Some(t) = self.written {
            v.push(C::string("DATE", iso(t), "[UTC] File creation date"));
        }
        if let Some(t) = self.date_beg {
            v.push(C::string("DATE-BEG", iso(t), "[UTC] Start of observation"));
            v.push(C::string("DATE-OBS", iso(t), "[UTC] Same as DATE-BEG"));
        }
        if let Some(t) = self.date_avg {
            v.push(C::string(
                "DATE-AVG",
                iso(t),
                "[UTC] Mean time of the frames",
            ));
        }
        if let Some(t) = self.date_end {
            v.push(C::string("DATE-END", iso(t), "[UTC] End of observation"));
        }
        if self.date_beg.is_some() {
            v.push(C::string("TIMESYS", "UTC", "Time scale"));
        }
        if let Some(s) = &self.telescope {
            v.push(C::string("TELESCOP", s.clone(), "Telescope (SER header)"));
        }
        if let Some(s) = &self.instrument {
            v.push(C::string("INSTRUME", s.clone(), "Instrument or camera"));
        }
        if let Some(s) = &self.observer {
            v.push(C::string("OBSERVER", s.clone(), "Observer (SER header)"));
        }
        if let Some(p) = self.filter {
            v.push(C::string("FILTER", p.name(), "Filter preset"));
            if let Some((nm, note)) = preset_wavelength(p) {
                v.push(C::float("WAVELNTH", nm, note));
                v.push(C::int("WAVEUNIT", -9, "WAVELNTH is in 10^WAVEUNIT m (nm)"));
                v.push(C::string("WAVEREF", "air", "Wavelengths are in air"));
            }
        }
        if let Some(e) = self.exposure_s {
            v.push(C::float("TEXPOSUR", e, "[s] Single-exposure time"));
            v.push(C::float(
                "XPOSURE",
                e * self.frames_summed as f64,
                "[s] Accumulated exposure time",
            ));
        }
        if self.frames_summed > 0 {
            v.push(C::int(
                "NSUMEXP",
                i64::try_from(self.frames_summed).unwrap_or(i64::MAX),
                "Number of summed exposures",
            ));
        }
        if let Some(site) = &self.site {
            let [x, y, z] = site.itrs();
            v.push(C::float("OBSGEO-X", x, "[m] Observer ITRS X"));
            v.push(C::float("OBSGEO-Y", y, "[m] Observer ITRS Y"));
            v.push(C::float("OBSGEO-Z", z, "[m] Observer ITRS Z"));
        }
        self.wcs_cards(&mut v);
        if let Some(g) = &self.granulation {
            Self::granulation_cards(g, &mut v);
        }
        if let Some(r) = &self.seeing {
            Self::seeing_cards(r, &mut v);
        }
        if let Some(r) = &self.sunspots {
            Self::sunspot_cards(r, &mut v);
        }
        for (i, s) in self.steps.iter().enumerate() {
            let n = i + 1;
            v.push(C::string(
                &format!("PRSTEP{n}"),
                s.step.clone(),
                "Processing step",
            ));
            v.push(C::string(
                &format!("PRPROC{n}"),
                s.process.clone(),
                "Procedure",
            ));
            v.push(C::string(
                &format!("PRPVER{n}"),
                format!("helia {}", self.version),
                "Version of procedure",
            ));
            if !s.params.is_empty() {
                v.push(C::string(
                    &format!("PRPARA{n}"),
                    s.params.clone(),
                    "Parameters",
                ));
            }
        }
        v.extend(self.plugin_cards.iter().cloned());
        v.extend(self.notes().iter().map(|t| C::commentary(t)));
        v
    }

    fn sunspot_cards(r: &crate::sunspots::Report, v: &mut Vec<HeaderCard>) {
        use HeaderCard as C;
        let int = |n: usize| i64::try_from(n).unwrap_or(i64::MAX);
        v.push(C::int("SPOTNGRP", int(r.group_count), "Sunspot groups"));
        v.push(C::int(
            "SPOTNSPT",
            int(r.spot_count),
            "Individual sunspots (umbrae and pores)",
        ));
        v.push(C::float(
            "SPOTWOLF",
            f64::from(r.wolf),
            "Relative Wolf number k(10g+s)",
        ));
        v.push(C::float(
            "SPOTK",
            f64::from(r.params.k),
            "Observer factor k of SPOTWOLF",
        ));
        v.push(C::float(
            "SPOTAREA",
            f64::from(r.area_msh),
            "[1e-6 hemisphere] Spot area, foreshortening corrected",
        ));
        v.push(C::float(
            "SPOTPROJ",
            f64::from(r.projected_msh),
            "[1e-6 hemisphere] Spot area as seen",
        ));
        v.push(C::float(
            "SPOTUMBR",
            f64::from(r.umbra_msh),
            "[1e-6 hemisphere] Umbral area, corrected",
        ));
        v.push(C::float(
            "SPOTTHRU",
            f64::from(r.params.umbra),
            "Umbra threshold, share of the quiet disc",
        ));
        v.push(C::float(
            "SPOTTHRP",
            f64::from(r.params.penumbra),
            "Penumbra threshold, share of the quiet disc",
        ));
        if let Some(e) = &r.ephemeris {
            v.push(C::float(
                "SPOTP",
                f64::from(e.p_deg),
                "[deg] Position angle of the solar axis",
            ));
            v.push(C::float(
                "CRLT_OBS",
                f64::from(e.b0_deg),
                "[deg] Carrington latitude of the observer (B0)",
            ));
            v.push(C::float(
                "CRLN_OBS",
                f64::from(e.l0_deg),
                "[deg] Carrington longitude of the observer (L0)",
            ));
        }
    }

    fn seeing_cards(r: &crate::seeing::SeeingReport, v: &mut Vec<HeaderCard>) {
        use HeaderCard as C;
        if let Some(m) = &r.motion {
            v.push(C::float(
                "SEEMOTPX",
                f64::from(m.rms_px),
                "[pix] Differential motion of alignment points, RMS",
            ));
            if let Some(a) = r.arcsec_per_px {
                v.push(C::float(
                    "SEEMOTAS",
                    f64::from(m.rms_px * a),
                    "[arcsec] Differential motion, RMS (relative index)",
                ));
            }
        }
        if let Some(s) = &r.spectral {
            v.push(C::float(
                "SEEDR0",
                f64::from(s.d_over_r0),
                "D/r0 from the spectral ratio",
            ));
            v.push(C::float(
                "SEER0500",
                f64::from(s.r0_500_cm) / 100.0,
                "[m] Fried parameter at 500 nm",
            ));
        }
        if let Some(g) = r.grade {
            v.push(C::string("SEEGRADE", g.label(), "Seeing grade from r0"));
        }
    }

    fn granulation_cards(g: &crate::granulation::Granulation, v: &mut Vec<HeaderCard>) {
        use HeaderCard as C;
        v.push(C::float(
            "GRANRMS",
            f64::from(g.rms_contrast),
            "Granulation RMS contrast (std/mean, flattened)",
        ));
        v.push(C::float(
            "GRANPITC",
            f64::from(g.cell_pitch_px),
            "[pix] Mean granule spacing",
        ));
        v.push(C::float(
            "GRANDIAM",
            f64::from(g.cell_diameter_px),
            "[pix] Mean diameter of whole granules",
        ));
        if let Some(a) = g.arcsec(g.cell_pitch_px) {
            v.push(C::float(
                "GRANPARC",
                f64::from(a),
                "[arcsec] Mean granule spacing",
            ));
        }
        if let Some(px) = g.granule_px() {
            v.push(C::float(
                "GRANGPIX",
                f64::from(px),
                "[pix] A 1.5 arcsec granule at this scale",
            ));
        }
        v.push(C::float(
            "GRANROLL",
            f64::from(g.rolloff_freq),
            "[cycle/pix] Power spectrum down to 10% of peak",
        ));
        v.push(C::float(
            "GRANRESO",
            f64::from(g.resolved_freq),
            "[cycle/pix] Highest frequency above 1% of peak",
        ));
        if let Some(c) = g.cutoff_freq {
            v.push(C::float(
                "GRANCUT",
                f64::from(c),
                "[cycle/pix] Diffraction cut-off D/lambda",
            ));
        }
    }

    fn wcs_cards(&self, v: &mut Vec<HeaderCard>) {
        use HeaderCard as C;
        let (Some(d), Some(scale)) = (self.disc, self.arcsec_per_pixel()) else {
            return;
        };
        let when = self.date_avg.or(self.date_beg);
        let rsun_arc = when.map_or(solar_radius_arcsec(1.0), |t| {
            solar_radius_arcsec(sun_distance_au(t))
        });
        v.push(C::string(
            "WCSNAME",
            "Helioprojective (limb fit, approximate)",
            "World coordinate system",
        ));
        v.push(C::string("CTYPE1", "HPLN-TAN", "Helioprojective longitude"));
        v.push(C::string("CTYPE2", "HPLT-TAN", "Helioprojective latitude"));
        v.push(C::string("CUNIT1", "arcsec", ""));
        v.push(C::string("CUNIT2", "arcsec", ""));
        // FITS pixels are 1-based and the first row is the bottom of the image.
        v.push(C::float(
            "CRPIX1",
            f64::from(d.cx) + 1.0,
            "[pix] Disc centre, from the limb fit",
        ));
        v.push(C::float(
            "CRPIX2",
            self.size.1 as f64 - f64::from(d.cy),
            "[pix] Disc centre, from the limb fit",
        ));
        v.push(C::float(
            "CRVAL1",
            0.0,
            "[arcsec] Disc centre taken as Sun centre",
        ));
        v.push(C::float(
            "CRVAL2",
            0.0,
            "[arcsec] Disc centre taken as Sun centre",
        ));
        v.push(C::float(
            "CDELT1",
            scale,
            "[arcsec/pix] Solar radius / limb radius",
        ));
        v.push(C::float(
            "CDELT2",
            scale,
            "[arcsec/pix] Solar radius / limb radius",
        ));
        v.push(C::float(
            "CROTA2",
            0.0,
            "[deg] Orientation unknown, 0 assumed",
        ));
        v.push(C::float(
            "LONPOLE",
            180.0,
            "[deg] Helioprojective native pole",
        ));
        v.push(C::float("RSUN_REF", RSUN_REF_M, "[m] Nominal solar radius"));
        v.push(C::float(
            "RSUN_ARC",
            rsun_arc,
            "[arcsec] Apparent solar radius used",
        ));
        v.push(C::float(
            "RSUN_OBS",
            rsun_arc,
            "[arcsec] Same as RSUN_ARC, for sunpy",
        ));
    }

    fn notes(&self) -> Vec<String> {
        let mut n = vec![
            "SOLARNET 0.5: partially compliant. Helia writes the mandatory keywords it knows."
                .to_owned(),
        ];
        if self.date_beg.is_none() && self.date_avg.is_none() {
            n.push(
                "No capture time was available (no SER timestamps), so DATE-BEG is absent."
                    .to_owned(),
            );
        }
        if self.site.is_none() {
            n.push("No observing site was given (--site), so OBSGEO-X/Y/Z are absent.".to_owned());
        }
        if self.exposure_s.is_none() {
            n.push("Exposure time unknown (no SharpCap sidecar), so XPOSURE is absent.".to_owned());
        }
        if self.disc.is_some() {
            n.push(
                "WCS is approximate: scale = apparent solar radius / fitted limb radius, centre = fitted disc centre."
                    .to_owned(),
            );
            n.push(
                "Image orientation on the sky is unknown: CROTA2 = 0 does not mean north is up."
                    .to_owned(),
            );
            if self.date_beg.is_none() {
                n.push("No date: the solar radius assumes a distance of 1 AU.".to_owned());
            }
        } else {
            n.push("No limb was found, so there is no WCS.".to_owned());
        }
        n
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn site_parses_and_converts_to_itrs() {
        let s: Site = "51.5, -0.1, 30".parse().unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(s.alt_m, 30.0);
        // Greenwich Observatory region: radius about 6365 km.
        let [x, y, z] = s.itrs();
        let r = (x * x + y * y + z * z).sqrt();
        assert!((6.35e6..6.40e6).contains(&r), "{r}");
        assert!(x > 0.0 && y < 0.0 && z > 0.0);
        // Equator, prime meridian, sea level: the semi-major axis.
        let eq: Site = "0,0,0".parse().unwrap_or_else(|e| panic!("{e}"));
        assert!((eq.itrs()[0] - 6_378_137.0).abs() < 1e-6);
        let pole: Site = "90,0,0".parse().unwrap_or_else(|e| panic!("{e}"));
        assert!((pole.itrs()[2] - 6_356_752.314).abs() < 1.0);
        assert!("91,0,0".parse::<Site>().is_err());
        assert!("1".parse::<Site>().is_err());
        assert!("a,b,c".parse::<Site>().is_err());
        assert!("1,2".parse::<Site>().is_ok());
    }

    #[test]
    fn iso_text_reads_back_to_unix_time() {
        assert_eq!(unix_from_iso("2022-07-17T15:30:00"), Some(1_658_071_800.0));
        assert_eq!(
            unix_from_iso("2022-07-17 15:30:00.5Z"),
            Some(1_658_071_800.5)
        );
        assert_eq!(unix_from_iso("1970-01-02"), Some(86_400.0));
        assert_eq!(unix_from_iso("2022-13-17"), None);
        assert_eq!(unix_from_iso("yesterday"), None);
        let t = 1_792_411_245.0;
        assert_eq!(unix_from_iso(&iso_from_unix(t)), Some(t));
    }

    #[test]
    fn ser_ticks_become_iso_time() {
        // 2022-07-17T14:42:57.500 UTC.
        let unix = 1_658_068_977.5;
        let ticks = (unix * 1e7) as i64 + SER_UNIX_EPOCH_TICKS;
        assert_eq!(ser_ticks_to_unix(ticks), Some(unix));
        assert_eq!(iso_from_unix(unix), "2022-07-17T14:42:57.500");
        assert_eq!(iso_from_unix(0.0), "1970-01-01T00:00:00.000");
        assert_eq!(iso_from_unix(951_782_400.0), "2000-02-29T00:00:00.000");
        assert_eq!(ser_ticks_to_unix(0), None);
        assert_eq!(ser_ticks_to_unix(i64::MAX), None);
    }

    #[test]
    fn solar_radius_follows_the_season() {
        // Perihelion (early January) is near 983 arcsec, aphelion near 944.
        let jan = 1_641_000_000.0; // 2022-01-01
        let jul = 1_657_000_000.0; // 2022-07-05
        let (rj, rl) = (
            solar_radius_arcsec(sun_distance_au(jan)),
            solar_radius_arcsec(sun_distance_au(jul)),
        );
        assert!((rj - 975.0).abs() < 6.0, "{rj}");
        assert!((rl - 944.0).abs() < 4.0, "{rl}");
        assert!((solar_radius_arcsec(1.0) - 959.2).abs() < 0.1);
    }

    #[test]
    fn exposure_units() {
        assert_eq!(parse_exposure("10.4480ms"), Some(0.010_448));
        assert_eq!(parse_exposure("0.64 ms"), Some(0.000_64));
        assert_eq!(parse_exposure("2s"), Some(2.0));
        assert_eq!(parse_exposure("500us"), Some(0.0005));
        assert_eq!(parse_exposure("auto"), None);
        assert_eq!(parse_exposure(""), None);
    }

    fn meta() -> FitsMeta {
        FitsMeta {
            telescope: Some("Test 'scope".into()),
            instrument: Some("ZWO ASI482MC".into()),
            observer: None,
            filter: Some(Preset::Ha),
            date_beg: Some(1_658_068_977.5),
            date_end: Some(1_658_068_999.0),
            date_avg: Some(1_658_068_988.0),
            written: None,
            exposure_s: Some(0.01),
            frames_summed: 100,
            site: Some(Site {
                lat_deg: 51.5,
                lon_deg: -0.1,
                alt_m: 30.0,
            }),
            disc: Some(Disc {
                cx: 99.5,
                cy: 79.5,
                r: 60.0,
            }),
            size: (200, 160),
            plugin_cards: Vec::new(),
            steps: vec![ProvStep::new(
                "STACKING",
                "helia-core stack",
                "weighted mean",
            )],
            granulation: None,
            seeing: None,
            sunspots: None,
            version: "9.9.9".into(),
        }
    }

    fn get<'a>(cards: &'a [HeaderCard], key: &str) -> Option<&'a crate::io::fits::CardValue> {
        cards
            .iter()
            .find(|c| c.key == key)
            .and_then(|c| c.value.as_ref())
    }

    #[test]
    fn cards_carry_the_minimum_and_the_wcs() {
        use crate::io::fits::CardValue as V;
        let c = meta().cards();
        assert_eq!(get(&c, "SOLARNET"), Some(&V::Float(0.5)));
        assert_eq!(get(&c, "OBS_HDU"), Some(&V::Int(1)));
        assert_eq!(get(&c, "EXTNAME"), Some(&V::Str("STACKED-IMAGE".into())));
        assert_eq!(get(&c, "NSUMEXP"), Some(&V::Int(100)));
        assert_eq!(get(&c, "XPOSURE"), Some(&V::Float(1.0)));
        assert_eq!(get(&c, "WAVEUNIT"), Some(&V::Int(-9)));
        assert_eq!(get(&c, "CRPIX1"), Some(&V::Float(100.5)));
        // Row 79.5 from the top of 160 rows is 80.5 from the bottom.
        assert_eq!(get(&c, "CRPIX2"), Some(&V::Float(80.5)));
        assert_eq!(get(&c, "PRSTEP1"), Some(&V::Str("STACKING".into())));
        assert_eq!(get(&c, "PRPVER1"), Some(&V::Str("helia 9.9.9".into())));
        let Some(V::Float(s)) = get(&c, "CDELT1") else {
            panic!("no CDELT1")
        };
        // July: about 944 arcsec over 60 pixels.
        assert!((s - 15.74).abs() < 0.1, "{s}");
        assert!(get(&c, "OBSGEO-X").is_some() && get(&c, "OBSGEO-Z").is_some());
    }

    #[test]
    fn the_sunspot_report_becomes_valid_cards() {
        use crate::io::fits::CardValue as V;
        use crate::sunspots::{Ephemeris3, Geometry, Params, Report};
        let mut m = meta();
        m.sunspots = Some(Report {
            when_unix: Some(1_658_071_800.0),
            ephemeris: Some(Ephemeris3 {
                p_deg: 20.0,
                b0_deg: 4.0,
                l0_deg: 100.0,
            }),
            disc: Geometry {
                cx: 100.0,
                cy: 80.0,
                radius: 60.0,
            },
            quiet: 0.5,
            params: Params::default(),
            north_angle_deg: 0.0,
            group_count: 2,
            spot_count: 5,
            wolf: 25.0,
            projected_msh: 400.0,
            area_msh: 500.0,
            umbra_msh: 90.0,
            spots: Vec::new(),
            groups: Vec::new(),
            notes: Vec::new(),
        });
        let c = m.cards();
        assert_eq!(get(&c, "SPOTNGRP"), Some(&V::Int(2)));
        assert_eq!(get(&c, "SPOTNSPT"), Some(&V::Int(5)));
        assert_eq!(get(&c, "SPOTWOLF"), Some(&V::Float(25.0)));
        assert_eq!(get(&c, "SPOTAREA"), Some(&V::Float(500.0)));
        assert_eq!(get(&c, "CRLT_OBS"), Some(&V::Float(4.0)));
        assert_eq!(get(&c, "CRLN_OBS"), Some(&V::Float(100.0)));
        assert!(m.cards().iter().all(|k| k.key.len() <= 8));
    }

    #[test]
    fn the_seeing_report_becomes_valid_cards() {
        use crate::io::fits::CardValue as V;
        use crate::seeing::motion::MotionIndex;
        use crate::seeing::{Grade, SeeingReport, SpectralFit};
        let mut m = meta();
        m.seeing = Some(SeeingReport {
            frames: 120,
            tiles: 12,
            motion: Some(MotionIndex {
                rms_px: 0.25,
                separation_px: 64.0,
                pairs: 20,
                frames: 120,
            }),
            arcsec_per_px: Some(0.5),
            spectral: Some(SpectralFit {
                d_over_r0: 8.0,
                r0_cm: 10.0,
                r0_500_cm: 8.0,
                rings: 14,
                tiles: 12,
                spread: None,
                cutoff: 0.3,
            }),
            grade: Some(Grade::Good),
            notes: Vec::new(),
            series: Vec::new(),
        });
        let c = m.cards();
        assert_eq!(get(&c, "SEEMOTPX"), Some(&V::Float(0.25)));
        assert_eq!(get(&c, "SEEMOTAS"), Some(&V::Float(0.125)));
        assert_eq!(get(&c, "SEEDR0"), Some(&V::Float(8.0)));
        assert_eq!(get(&c, "SEER0500"), Some(&V::Float(0.08)));
        assert_eq!(get(&c, "SEEGRADE"), Some(&V::Str("good".into())));
        assert!(
            c.iter().all(|c| c.key.len() <= 8),
            "keyword over 8 characters"
        );
        // Without the report there are no such cards.
        assert!(get(&meta().cards(), "SEEDR0").is_none());
    }

    #[test]
    fn unknowns_are_left_out_not_invented() {
        let mut m = meta();
        m.site = None;
        m.date_beg = None;
        m.date_avg = None;
        m.date_end = None;
        m.exposure_s = None;
        m.disc = None;
        m.filter = None;
        let c = m.cards();
        for k in [
            "OBSGEO-X", "DATE-BEG", "DATE-AVG", "XPOSURE", "TEXPOSUR", "CTYPE1", "WAVELNTH",
        ] {
            assert!(get(&c, k).is_none(), "{k} should be absent");
        }
        assert!(get(&c, "NSUMEXP").is_some());
        assert!(get(&c, "SOLARNET").is_some());
    }
}
