// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! The SOLARNET FITS header of a real run on a synthetic SER capture: keywords
//! checked here, then (when Python has astropy and sunpy) read back by
//! `solarnet_check.py`. Set `HELIA_REQUIRE_SOLARNET=1` to make a missing
//! astropy or sunpy a failure, and `HELIA_PYTHON` to pick the interpreter.
#![cfg(feature = "testutil")]

use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::AtomicBool;

use helia_core::Keep;
use helia_core::Preset;
use helia_core::io::ser::{ColorId, SerHeader, SerWriter};
use helia_core::io::solarnet::{CaptureInfo, FitsMeta, Site};
use helia_core::io::source::{OpenOptions, open_capture};
use helia_core::io::{ImageNotes, save_image_with};
use helia_core::pipeline::{Pipeline, StackConfig};
use helia_core::stack::StackMode;
use helia_core::testutil::{CaptureConfig, SyntheticCapture, SyntheticConfig, SyntheticSun};

/// 2022-07-17T14:42:57 UTC in SER ticks (100 ns since 0001-01-01).
const START_TICKS: i64 = 621_355_968_000_000_000 + 1_658_068_977 * 10_000_000;
const FRAME_STEP_TICKS: i64 = 200_000; // 20 ms

fn tick(i: usize) -> i64 {
    START_TICKS + i64::try_from(i).unwrap_or(0) * FRAME_STEP_TICKS
}

fn work_dir() -> PathBuf {
    let d = std::env::temp_dir().join(format!("helia-solarnet-{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap_or_else(|e| panic!("{e}"));
    d
}

/// Header cards of a FITS file as (keyword, text after "= " or the comment).
fn cards(bytes: &[u8]) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for c in bytes.chunks(80) {
        let text = String::from_utf8_lossy(c).into_owned();
        let key = text[..8].trim().to_owned();
        if key == "END" {
            break;
        }
        out.push((key, text[8..].trim_start_matches("= ").trim().to_owned()));
    }
    out
}

fn value<'a>(cards: &'a [(String, String)], key: &str) -> Option<&'a str> {
    cards
        .iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.split(" / ").next().unwrap_or("").trim())
}

fn python() -> Option<String> {
    if let Ok(p) = std::env::var("HELIA_PYTHON") {
        return Some(p);
    }
    ["python3", "python"].iter().find_map(|p| {
        Command::new(p)
            .arg("--version")
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|_| (*p).to_owned())
    })
}

fn make_capture(dir: &Path) -> PathBuf {
    let sun = SyntheticSun::new(&SyntheticConfig {
        width: 256,
        height: 200,
        radius_frac: 0.4,
        ..SyntheticConfig::default()
    });
    let cap = SyntheticCapture::generate(
        &sun,
        &CaptureConfig {
            frames: 30,
            max_shift: 3.0,
            ..CaptureConfig::default()
        },
    );
    let mut hdr = SerHeader::new(256, 200, ColorId::Mono, 16);
    hdr.telescope = "Test refractor 80/600".into();
    hdr.instrument = "ZWO ASI482MC".into();
    hdr.observer = "A. Observer".into();
    hdr.date_time_utc = START_TICKS;
    let mut w = SerWriter::new(Cursor::new(Vec::new()), hdr).unwrap_or_else(|e| panic!("{e}"));
    for (i, f) in cap.frames.iter().enumerate() {
        w.write_frame(f, Some(tick(i)))
            .unwrap_or_else(|e| panic!("{e}"));
    }
    let bytes = w.finish().unwrap_or_else(|e| panic!("{e}")).into_inner();
    let ser = dir.join("sun_Ha.ser");
    std::fs::write(&ser, bytes).unwrap_or_else(|e| panic!("{e}"));
    std::fs::write(
        dir.join("sun_Ha.CameraSettings.txt"),
        "[ZWO ASI482MC]\nExposure=10.4480ms\nGain=17\n",
    )
    .unwrap_or_else(|e| panic!("{e}"));
    ser
}

fn expected_json(meta: &FitsMeta, out: &helia_core::pipeline::StackOutput, site: Site) -> String {
    let disc = meta.disc.unwrap_or_else(|| panic!("no disc"));
    let scale = meta.arcsec_per_pixel().unwrap_or(0.0);
    let times: Vec<f64> = out.kept.iter().map(|&i| tick(i) as f64).collect();
    let to_iso = |ticks: f64| {
        helia_core::io::solarnet::iso_from_unix(
            helia_core::io::solarnet::ser_ticks_to_unix(ticks as i64).unwrap_or(0.0),
        )
    };
    let lo = times.iter().copied().fold(f64::INFINITY, f64::min);
    let hi = times.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let avg = times.iter().sum::<f64>() / times.len() as f64;
    let rsun_arc = scale * f64::from(disc.r);
    format!(
        r#"{{"nsumexp": 10, "xposure": 0.10448, "texposur": 0.010448,
        "date_beg": "{}", "date_end": "{}", "date_avg": "{}",
        "telescop": "Test refractor 80/600", "instrume": "ZWO ASI482MC", "observer": "A. Observer",
        "wavelnth": 656.28, "obsgeo": [{:?}, {:?}, {:?}],
        "cdelt": {:?}, "crpix1": {:?}, "crpix2": {:?}, "rsun_arc": {:?},
        "disc_x": {:?}, "disc_y_fits": {:?}, "disc_r": {:?}, "shape": [{}, {}]}}"#,
        to_iso(lo),
        // Last contributing frame plus its exposure.
        helia_core::io::solarnet::iso_from_unix(
            helia_core::io::solarnet::ser_ticks_to_unix(hi as i64).unwrap_or(0.0) + 0.010_448
        ),
        to_iso(avg),
        site.itrs()[0],
        site.itrs()[1],
        site.itrs()[2],
        scale,
        f64::from(disc.cx) + 1.0,
        out.image.height as f64 - f64::from(disc.cy),
        rsun_arc,
        f64::from(disc.cx),
        out.image.height as f64 - 1.0 - f64::from(disc.cy),
        f64::from(disc.r),
        out.image.height,
        out.image.width,
    )
}

#[test]
fn a_stack_writes_a_solarnet_header_that_astropy_and_sunpy_read() {
    let dir = work_dir();
    let ser = make_capture(&dir);
    let mut src = open_capture(&ser, &OpenOptions::default()).unwrap_or_else(|e| panic!("{e}"));
    let cfg = StackConfig {
        keep: Keep::Count(10),
        multipoint: None,
        mode: StackMode::Mean,
        ..StackConfig::default()
    };
    let out = Pipeline::new(cfg)
        .run(&mut src, &|_, _| {}, &AtomicBool::new(false))
        .unwrap_or_else(|e| panic!("{e}"));
    assert!(out.disc.is_some(), "the limb fit finds the synthetic disc");

    let site: Site = "51.5,-0.1,30".parse().unwrap_or_else(|e| panic!("{e}"));
    let info = CaptureInfo::read(&ser, Some(Preset::Ha));
    let meta = FitsMeta::from_stack(&info, &out, Some(site));
    let fits = dir.join("stack.fits");
    save_image_with(
        &fits,
        &out.image,
        &ImageNotes::default().with_fits(meta.clone()),
    )
    .unwrap_or_else(|e| panic!("{e}"));

    // The keywords, read straight from the header cards.
    let bytes = std::fs::read(&fits).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(bytes.len() % 2880, 0);
    let c = cards(&bytes);
    assert_eq!(value(&c, "NSUMEXP"), Some("10"));
    assert_eq!(value(&c, "TELESCOP"), Some("'Test refractor 80/600'"));
    assert_eq!(value(&c, "INSTRUME"), Some("'ZWO ASI482MC'"));
    assert_eq!(value(&c, "OBSERVER"), Some("'A. Observer'"));
    assert_eq!(value(&c, "CTYPE1"), Some("'HPLN-TAN'"));
    assert_eq!(value(&c, "WAVEUNIT").map(str::trim), Some("-9"));
    assert!(value(&c, "PRSTEP1").is_some_and(|v| v.contains("FRAME-SELECTION")));
    assert!(
        c.iter().any(|(k, _)| k == "HISTORY"),
        "version recorded as history"
    );
    let xposure: f64 = value(&c, "XPOSURE")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0.0);
    assert!((xposure - 0.104_48).abs() < 1e-9, "{xposure}");
    // Every card is 80 characters of printable ASCII.
    assert!(bytes[..2880].iter().all(|b| (0x20..0x7f).contains(b)));

    // And through astropy and sunpy.
    let expected = expected_json(&meta, &out, site);
    let json = dir.join("expected.json");
    std::fs::write(&json, expected).unwrap_or_else(|e| panic!("{e}"));

    let require = std::env::var("HELIA_REQUIRE_SOLARNET").is_ok_and(|v| v == "1");
    let Some(py) = python() else {
        assert!(
            !require,
            "no Python interpreter for the astropy/sunpy check"
        );
        eprintln!("note: no Python found; skipping the astropy and sunpy check");
        return;
    };
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/solarnet_check.py");
    let run = Command::new(py)
        .arg("-I")
        .arg(&script)
        .arg(&fits)
        .arg(&json)
        .output()
        .unwrap_or_else(|e| panic!("cannot run Python: {e}"));
    let text = String::from_utf8_lossy(&run.stdout);
    eprintln!("{text}{}", String::from_utf8_lossy(&run.stderr));
    match run.status.code() {
        Some(0) => {}
        Some(77) => {
            assert!(!require, "astropy or sunpy missing but required:\n{text}");
            eprintln!("note: astropy or sunpy not installed; skipping the validation");
        }
        _ => panic!("astropy/sunpy validation failed:\n{text}"),
    }
}

/// The real 2022-07-17 captures (`HELIA_REAL_DATA`): times from the SER
/// trailer and the exposure from the sidecar. Skips without the data.
#[test]
fn real_captures_give_a_date_and_an_exposure() {
    let Some(base) = std::env::var_os("HELIA_REAL_DATA").map(PathBuf::from) else {
        eprintln!("HELIA_REAL_DATA is not set: skipping");
        return;
    };
    let find = |name: &str| {
        [base.join(name), base.join("solar-2022-07-17").join(name)]
            .into_iter()
            .find(|p| p.exists())
    };
    if let Some(ser) = find("15_20_57.ser") {
        let info = CaptureInfo::read(&ser, None);
        let first = info.frame_unix.iter().flatten().next().copied();
        let iso = first.map(helia_core::io::solarnet::iso_from_unix);
        assert!(
            iso.as_deref().is_some_and(|t| t.starts_with("2022-07-17T")),
            "{iso:?}"
        );
        assert_eq!(info.frame_unix.len(), 100);
        assert_eq!(info.instrument.as_deref(), Some("ZWO ASI482MC"));
    }
    if let Some(avi) = find("15_32_09.avi") {
        let info = CaptureInfo::read(&avi, None);
        let e = info.exposure_s.unwrap_or(0.0);
        assert!((e - 0.010_448).abs() < 1e-9, "{e}");
        assert!(info.frame_unix.is_empty(), "AVI has no per-frame times");
    }
}
