// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! End-to-end tests of the `helia` binary on a small synthetic capture.

use std::path::{Path, PathBuf};

use assert_cmd::Command;
use helia_core::io::{fits, load_image};
use helia_core::testutil::{
    CaptureConfig, SyntheticCapture, SyntheticConfig, SyntheticSun, ser_bytes,
};
use predicates::prelude::*;

/// A fresh temporary directory whose name cannot read as a filter.
///
/// Helia guesses the filter from the folder a capture is in (and the one above), and the
/// random names `tempfile` makes sometimes spell one ("Ha", "Na", "656"), which changes the
/// recommended look, the Bayer channel and the output. The name here is letters that are in
/// no filter name, unique to the process and the call.
fn scratch() -> tempfile::TempDir {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static N: AtomicUsize = AtomicUsize::new(0);
    const LETTERS: &[u8; 10] = b"qxzkvwjyfp";
    let n = N.fetch_add(1, Ordering::Relaxed);
    let code = |mut v: usize| {
        let mut s = String::new();
        loop {
            s.push(char::from(LETTERS[v % 10]));
            v /= 10;
            if v == 0 {
                return s;
            }
        }
    };
    tempfile::Builder::new()
        .prefix(&format!(
            "zz{}z{}",
            code(std::process::id() as usize),
            code(n)
        ))
        .rand_bytes(0)
        .tempdir()
        .expect("tempdir")
}

/// The longest a `helia` child may run in these tests. The slowest takes well under a
/// minute on a loaded runner, so this is generous; a child past it is hung
/// (issue #185). It is killed, and the test that spawned it fails with the child's
/// stderr in the report (`assert()`) or from [`finished`] (`output()`) rather than the job
/// waiting for its own timeout.
const CHILD_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);

fn helia() -> Command {
    let mut cmd = Command::cargo_bin("helia").expect("helia binary");
    cmd.timeout(CHILD_TIMEOUT);
    cmd
}

/// The output of a finished child. A child killed by the timeout fails the test, naming
/// it and printing the child's stderr, as `assert()` does.
fn finished(out: std::io::Result<std::process::Output>) -> std::process::Output {
    let out = out.expect("run helia");
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt as _;
        assert!(
            out.status.signal() != Some(9),
            "helia hung and was killed after {CHILD_TIMEOUT:?} in {}\nstderr:\n{}",
            std::thread::current().name().unwrap_or("?"),
            String::from_utf8_lossy(&out.stderr)
        );
    }
    out
}

fn capture(dir: &Path) -> PathBuf {
    let sun = SyntheticSun::new(&SyntheticConfig {
        width: 128,
        height: 112,
        ..SyntheticConfig::default()
    });
    let cap = SyntheticCapture::generate(
        &sun,
        &CaptureConfig {
            frames: 20,
            noise: 0.02,
            ..CaptureConfig::default()
        },
    );
    let path = dir.join("sun.ser");
    std::fs::write(&path, ser_bytes(&cap.frames)).expect("write ser");
    path
}

#[test]
fn version_flag_prints_name_and_version() {
    helia()
        .arg("--version")
        .assert()
        .success()
        .stdout(predicate::str::starts_with("helia 0.8.2"));
}

#[test]
fn long_version_has_copyright_licence_and_warranty() {
    helia()
        .arg("--version")
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Copyright (C) 2026 Laurence Horrocks-Barlow",
        ))
        .stdout(predicate::str::contains("GPL-3.0-or-later"))
        .stdout(predicate::str::contains("WITHOUT ANY WARRANTY"));
}

#[test]
fn short_version_is_one_line() {
    helia()
        .arg("-V")
        .assert()
        .success()
        .stdout(predicate::eq("helia 0.8.2\n"));
}

#[test]
fn info_describes_the_capture() {
    let dir = scratch();
    let ser = capture(dir.path());
    helia()
        .arg("info")
        .arg(&ser)
        .assert()
        .success()
        .stdout(predicate::str::contains("128 px"))
        .stdout(predicate::str::contains("112 px"))
        .stdout(predicate::str::is_match(r"FrameCount:\s+20").expect("regex"))
        .stdout(predicate::str::contains("16 bit"));
}

#[test]
fn stack_writes_a_16_bit_tiff() {
    let dir = scratch();
    let ser = capture(dir.path());
    let out = dir.path().join("out.tif");
    helia()
        .args(["stack"])
        .arg(&ser)
        .arg("-o")
        .arg(&out)
        .args(["--keep", "30%", "--ap-size", "32", "--quiet"])
        .assert()
        .success();
    let bytes = std::fs::read(&out).expect("output exists");
    assert_eq!(&bytes[..4], b"II*\0", "little-endian TIFF signature");
    let mut dec = tiff::decoder::Decoder::new(std::io::Cursor::new(bytes)).expect("decode");
    assert_eq!(dec.dimensions().expect("dims"), (128, 112));
    assert!(matches!(
        dec.read_image().expect("pixels"),
        tiff::decoder::DecodingResult::U16(_)
    ));
}

#[test]
fn stack_writes_fits_with_global_only_options() {
    let dir = scratch();
    let ser = capture(dir.path());
    let out = dir.path().join("out.fits");
    helia()
        .arg("stack")
        .arg(&ser)
        .arg("-o")
        .arg(&out)
        .args([
            "--keep-frames",
            "5",
            "--no-multipoint",
            "--backend",
            "cpu",
            "--sigma-clip",
            "2.5",
            "-q",
        ])
        .assert()
        .success();
    let f = fits::read_fits(std::io::BufReader::new(
        std::fs::File::open(&out).expect("open"),
    ))
    .expect("fits");
    assert_eq!((f.width, f.height), (128, 112));
    assert!(f.min_max().1 > 0.5, "normalised output has a bright disc");
    let _ = load_image(&out).expect("load_image reads FITS");
}

#[test]
fn stack_accepts_every_robust_mode_and_rejects_bad_combinations() {
    let dir = tempfile::tempdir().expect("tempdir");
    let ser = capture(dir.path());
    for (robust, k) in [
        ("sigma", None),
        ("huber", Some("1.5")),
        ("winsor", Some("2")),
    ] {
        let mut cmd = helia();
        cmd.arg("stack")
            .arg(&ser)
            .arg("-o")
            .arg(dir.path().join(format!("{robust}.fits")))
            .args(["--keep-frames", "5", "--no-multipoint", "--backend", "cpu"])
            .args(["--robust", robust, "-q"]);
        if let Some(k) = k {
            cmd.args(["--robust-k", k]);
        }
        cmd.assert().success();
    }
    for bad in [
        vec!["--robust-k", "2"],
        vec!["--robust", "huber", "--robust-k", "-1"],
        vec!["--robust", "huber", "--sigma-clip", "2.5"],
    ] {
        helia()
            .arg("stack")
            .arg(&ser)
            .arg("-o")
            .arg(dir.path().join("bad.fits"))
            .args(&bad)
            .assert()
            .failure();
    }
}

#[test]
fn robust_stacking_says_what_it_does_not_remove() {
    let dir = tempfile::tempdir().expect("tempdir");
    let ser = capture(dir.path());
    for (robust, noted) in [("sigma", true), ("none", false)] {
        let out = helia()
            .arg("stack")
            .arg(&ser)
            .arg("-o")
            .arg(dir.path().join(format!("{robust}.fits")))
            .args(["--keep-frames", "5", "--no-multipoint", "--backend", "cpu"])
            .args(["--robust", robust])
            .output();
        let out = finished(out);
        assert!(out.status.success());
        let err = String::from_utf8_lossy(&out.stderr);
        assert_eq!(
            err.contains("fixed on the sensor"),
            noted,
            "{robust}: {err}"
        );
    }
}

#[test]
fn stack_crops_to_the_disc_and_chooses_alignment_points() {
    let dir = scratch();
    let ser = capture(dir.path());
    let whole = dir.path().join("whole.fits");
    let cropped = dir.path().join("cropped.fits");
    let run = |out: &Path, extra: &[&str]| {
        helia()
            .arg("stack")
            .arg(&ser)
            .arg("-o")
            .arg(out)
            .args(["--keep", "30%", "--backend", "cpu"])
            .args(extra)
            .assert()
            .success()
            .get_output()
            .stderr
            .clone()
    };
    run(&whole, &[]);
    let err = String::from_utf8(run(&cropped, &["--autocrop", "--ap-auto"])).expect("utf8");
    assert!(err.contains("auto-ap: alignment points"), "{err}");
    assert!(err.contains("crop:    "), "{err}");
    let dims = |p: &Path| {
        let f = load_image(p).expect("image");
        (f.width, f.height)
    };
    let (a, b) = (dims(&whole), dims(&cropped));
    assert!(b.0 < a.0 && b.1 <= a.1, "{b:?} is not smaller than {a:?}");
    // The margin is a percentage and changes the size.
    let wide = dir.path().join("wide.fits");
    run(&wide, &["--autocrop", "--autocrop-margin", "40"]);
    assert!(dims(&wide).0 > b.0);
}

#[test]
fn autocrop_and_ap_auto_options_are_checked() {
    let dir = scratch();
    let ser = capture(dir.path());
    for bad in [
        vec!["--ap-auto", "--ap-size", "48"],
        vec!["--ap-auto", "--no-multipoint"],
        vec!["--autocrop-margin", "10"],
        vec!["--autocrop", "--autocrop-margin", "250"],
        vec!["--autocrop", "--autocrop-margin", "-5"],
    ] {
        helia()
            .arg("stack")
            .arg(&ser)
            .arg("-o")
            .arg(dir.path().join("bad.fits"))
            .args(&bad)
            .assert()
            .failure();
    }
}

/// A raw colour (RGGB) capture of the synthetic Sun, red at half and blue at
/// 0.7 of the green's brightness, red and blue displaced a little.
fn colour_capture(dir: &Path) -> PathBuf {
    use helia_core::BayerPattern;
    use helia_core::Shift;
    use helia_core::io::ser::{ColorId, SerHeader};
    use helia_core::testutil::{colour_mosaic, ser_bytes_with};
    let sun = SyntheticSun::new(&SyntheticConfig {
        width: 128,
        height: 112,
        ..SyntheticConfig::default()
    });
    let cap = SyntheticCapture::generate(
        &sun,
        &CaptureConfig {
            frames: 20,
            noise: 0.01,
            ..CaptureConfig::default()
        },
    );
    let frames = colour_mosaic(
        &cap.frames,
        [0.5, 1.0, 0.7],
        [
            Shift::new(-0.5, -0.6),
            Shift::default(),
            Shift::new(0.4, 0.5),
        ],
        BayerPattern::Rggb,
    );
    let header = SerHeader::new(128, 112, ColorId::Bayer(BayerPattern::Rggb), 16);
    let path = dir.join("colour.ser");
    std::fs::write(&path, ser_bytes_with(&frames, header)).expect("write ser");
    path
}

/// Mean red, green and blue of the middle of a colour image.
fn middle(img: &helia_core::RgbImage) -> [f32; 3] {
    let (cx, cy) = (img.width / 2, img.height / 2);
    let mut sum = [0.0f64; 3];
    let mut n = 0.0;
    for y in cy - 8..cy + 8 {
        for x in cx - 8..cx + 8 {
            let p = img.pixel(x, y);
            for c in 0..3 {
                sum[c] += f64::from(p[c]);
            }
            n += 1.0;
        }
    }
    sum.map(|v| (v / n) as f32)
}

#[test]
fn true_colour_writes_a_balanced_rgb_image_in_every_format() {
    use helia_core::io::any::{Loaded, read_any};
    let dir = scratch();
    let ser = colour_capture(dir.path());
    let run = |name: &str, extra: &[&str]| {
        let out = dir.path().join(name);
        let err = helia()
            .arg("stack")
            .arg(&ser)
            .arg("-o")
            .arg(&out)
            .args(["--keep", "40%", "--backend", "cpu", "--colour", "true"])
            .args(extra)
            .assert()
            .success()
            .get_output()
            .stderr
            .clone();
        (out, String::from_utf8(err).expect("utf8"))
    };
    let read = |p: &Path| match read_any(p).expect("read").0 {
        Loaded::Rgb(i) => i,
        Loaded::Mono(_) => panic!("{} is mono", p.display()),
    };
    let (tif, err) = run("auto.tif", &[]);
    assert!(err.contains("colour:  true colour, gains red"), "{err}");
    assert!(err.contains("Channel alignment: red moved"), "{err}");
    let m = middle(&read(&tif));
    assert!(
        (m[0] / m[1] - 1.0).abs() < 0.05 && (m[2] / m[1] - 1.0).abs() < 0.05,
        "{m:?}"
    );
    // The sensor's own balance keeps red at half and blue at 0.7.
    let (off, _) = run("off.tif", &["--white-balance", "off"]);
    let m = middle(&read(&off));
    assert!(
        (m[0] / m[1] - 0.5).abs() < 0.05 && (m[2] / m[1] - 0.7).abs() < 0.05,
        "{m:?}"
    );
    // Given gains.
    let (manual, _) = run("manual.tif", &["--white-balance", "2,1,1.4286"]);
    let m = middle(&read(&manual));
    assert!((m[0] / m[1] - 1.0).abs() < 0.06, "{m:?}");
    // PNG and a three-plane FITS cube.
    let (png, _) = run("c.png", &[]);
    assert_eq!(read(&png).width, 128);
    let (fits, err) = run("c.fits", &["--no-channel-align"]);
    assert!(err.contains("three-plane"), "{err}");
    assert!(!err.contains("Channel alignment"), "{err}");
    let cube = read(&fits);
    assert_eq!((cube.width, cube.height), (128, 112));
    let m = middle(&cube);
    assert!((m[0] / m[1] - 1.0).abs() < 0.05, "{m:?}");
    // Edits go to each channel and the colour stays.
    let (cropped, _) = run("crop.tif", &["--crop", "10,10,60,50", "--autocrop"]);
    let c = read(&cropped);
    assert_eq!((c.width, c.height), (60, 50));
}

#[test]
fn true_colour_options_are_checked() {
    let dir = scratch();
    let colour = colour_capture(dir.path());
    let mono = capture(dir.path());
    let stack = |input: &Path, extra: &[&str]| {
        helia()
            .arg("stack")
            .arg(input)
            .arg("-o")
            .arg(dir.path().join("x.tif"))
            .args(["--backend", "cpu"])
            .args(extra)
            .assert()
    };
    // A mono capture has no colours to stack.
    stack(&mono, &["--colour", "true"])
        .failure()
        .stderr(predicate::str::contains("raw colour capture"));
    // The options belong to true colour.
    stack(&colour, &["--white-balance", "off"])
        .failure()
        .stderr(predicate::str::contains("belong to --colour true"));
    stack(&colour, &["--no-channel-align"]).failure();
    stack(&colour, &["--colour", "true", "--white-balance", "1,2"]).failure();
    stack(&colour, &["--colour", "true", "--channel", "g"])
        .failure()
        .stderr(predicate::str::contains("do not apply"));
    stack(&colour, &["--colour", "true", "--invert"]).failure();
}

#[test]
fn stack_prints_progress_summary_unless_quiet() {
    let dir = scratch();
    let ser = capture(dir.path());
    helia()
        .arg("stack")
        .arg(&ser)
        .arg("-o")
        .arg(dir.path().join("o.tif"))
        .args(["--keep", "25%", "--ap-size", "32"])
        .assert()
        .success()
        .stderr(predicate::str::contains("kept"))
        .stderr(predicate::str::contains("detail:"));
}

#[test]
fn bad_input_fails_with_a_message_not_a_panic() {
    let dir = scratch();
    helia()
        .args(["stack", "missing.ser", "-o"])
        .arg(dir.path().join("o.tif"))
        .assert()
        .failure()
        .stderr(predicate::str::contains("cannot open"));

    let junk = dir.path().join("junk.ser");
    std::fs::write(&junk, vec![0u8; 400]).expect("write");
    helia()
        .arg("info")
        .arg(&junk)
        .assert()
        .failure()
        .stderr(predicate::str::contains("not a SER file"));

    let ser = capture(dir.path());
    helia()
        .arg("stack")
        .arg(&ser)
        .arg("-o")
        .arg(dir.path().join("o.jpg"))
        .assert()
        .failure()
        .stderr(predicate::str::contains("cannot choose an output format"));
    helia()
        .arg("stack")
        .arg(&ser)
        .arg("-o")
        .arg(dir.path().join("o.tif"))
        .args(["--keep", "25%", "--keep-frames", "4"])
        .assert()
        .failure();
    helia()
        .arg("stack")
        .arg(&ser)
        .arg("-o")
        .arg(dir.path().join("o.tif"))
        .args(["--keep", "150%"])
        .assert()
        .failure();
}

#[test]
fn timings_flag_prints_stage_and_busy_tables() {
    let dir = scratch();
    let ser = capture(dir.path());
    let out = dir.path().join("out.tif");
    helia()
        .args(["stack"])
        .arg(&ser)
        .arg("-o")
        .arg(&out)
        .args(["--keep", "30%", "--ap-size", "32", "--quiet", "--timings"])
        .assert()
        .success()
        .stderr(predicate::str::contains("wall seconds per stage"))
        .stderr(predicate::str::contains("write"))
        .stderr(predicate::str::contains("busy seconds"))
        .stderr(predicate::str::contains("AP align"));
}

/// A capture whose file name carries the filter, in a folder with a fixed name.
fn named_capture(dir: &Path, name: &str) -> PathBuf {
    let from = capture(dir);
    let session = dir.join("session");
    std::fs::create_dir_all(&session).expect("session dir");
    let to = session.join(name);
    std::fs::rename(from, &to).expect("rename");
    to
}

fn decode_tiff(path: &Path) -> (tiff::ColorType, Vec<u16>, Option<String>) {
    let mut dec =
        tiff::decoder::Decoder::new(std::io::Cursor::new(std::fs::read(path).expect("read")))
            .expect("decode");
    let colour = dec.colortype().expect("colour type");
    let note = dec
        .get_tag_ascii_string(tiff::tags::Tag::ImageDescription)
        .ok();
    let tiff::decoder::DecodingResult::U16(px) = dec.read_image().expect("pixels") else {
        panic!("expected 16-bit samples")
    };
    (colour, px, note)
}

#[test]
fn presets_lists_every_filter_with_its_wavelength() {
    let out = helia().arg("presets").assert().success();
    let text = String::from_utf8_lossy(&out.get_output().stdout).into_owned();
    for id in ["ha", "cak", "cah", "mg", "na", "he-d3", "hbeta", "white"] {
        assert!(
            text.lines().any(|l| l.starts_with(id)),
            "{id} missing:\n{text}"
        );
    }
    assert!(text.contains("656.3 nm") && text.contains("393.4 nm"));
}

#[test]
fn preset_writes_a_colour_tiff_that_says_how() {
    let dir = scratch();
    let ser = capture(dir.path());
    let out = dir.path().join("ha.tif");
    helia()
        .arg("stack")
        .arg(&ser)
        .arg("-o")
        .arg(&out)
        .args(["--preset", "ha", "--keep", "30%", "--ap-size", "32"])
        .assert()
        .success()
        .stderr(predicate::str::contains("H-alpha (chosen)"))
        .stderr(predicate::str::contains("scoring frames"))
        .stderr(predicate::str::contains("colourising"))
        .stderr(predicate::str::contains("writing"));
    let (colour, px, desc) = decode_tiff(&out);
    assert_eq!(colour, tiff::ColorType::RGB(16));
    assert_eq!(px.len(), 128 * 112 * 3);
    let desc = desc.expect("description tag");
    assert!(
        desc.contains("preset ha") && desc.contains("presentation"),
        "{desc}"
    );
    // H-alpha is orange: red outweighs blue over the picture.
    let sum = |c: usize| {
        px.iter()
            .skip(c)
            .step_by(3)
            .map(|&v| u64::from(v))
            .sum::<u64>()
    };
    assert!(sum(0) > sum(2), "red {} blue {}", sum(0), sum(2));
}

#[test]
fn the_filter_is_read_from_the_file_name_and_colour_is_opt_in() {
    let dir = scratch();
    let ser = named_capture(dir.path(), "2026-10-19_Sun_CaK_001.ser");
    let mono = dir.path().join("mono.tif");
    helia()
        .arg("stack")
        .arg(&ser)
        .arg("-o")
        .arg(&mono)
        .args(["--ap-size", "32"])
        .assert()
        .success()
        .stderr(predicate::str::contains(
            "Ca II K (from \"2026-10-19_Sun_CaK_001\")",
        ))
        .stderr(predicate::str::contains("--colour auto"));
    assert_eq!(decode_tiff(&mono).0, tiff::ColorType::Gray(16));

    let png = dir.path().join("cak.png");
    helia()
        .arg("stack")
        .arg(&ser)
        .arg("-o")
        .arg(&png)
        .args(["--colour", "auto", "--ap-size", "32", "--quiet"])
        .assert()
        .success();
    let mut reader = png::Decoder::new(std::io::Cursor::new(std::fs::read(&png).expect("read")))
        .read_info()
        .expect("png header");
    let mut buf = vec![0u8; reader.output_buffer_size().expect("size")];
    let info = reader.next_frame(&mut buf).expect("png pixels");
    assert_eq!(info.color_type, png::ColorType::Rgb);
    assert_eq!(info.bit_depth, png::BitDepth::Eight);
    // Violet: blue outweighs red.
    let sum = |c: usize| {
        buf.iter()
            .skip(c)
            .step_by(3)
            .map(|&v| u64::from(v))
            .sum::<u64>()
    };
    assert!(sum(2) > sum(0), "blue {} red {}", sum(2), sum(0));
}

#[test]
fn fits_stays_mono_and_unaltered_with_a_history_note() {
    let dir = scratch();
    let ser = capture(dir.path());
    let (plain, noted) = (dir.path().join("a.fits"), dir.path().join("b.fits"));
    for (out, extra) in [(&plain, &[][..]), (&noted, &["--preset", "mg"][..])] {
        helia()
            .arg("stack")
            .arg(&ser)
            .arg("-o")
            .arg(out)
            .args(["--keep", "30%", "--ap-size", "32", "--quiet"])
            .args(extra)
            .assert()
            .success();
    }
    let header = String::from_utf8_lossy(&std::fs::read(&noted).expect("read")).into_owned();
    assert!(
        header.contains("HISTORY Colour is presentation"),
        "no history card"
    );
    assert!(header.contains("preset mg"));
    let (a, b) = (
        load_image(&plain).expect("a"),
        load_image(&noted).expect("b"),
    );
    assert_eq!(a.data, b.data, "the note must not change a pixel");
}

#[test]
fn bad_colour_options_are_explained() {
    let dir = scratch();
    let ser = capture(dir.path());
    let out = dir.path().join("o.tif");
    helia()
        .arg("stack")
        .arg(&ser)
        .arg("-o")
        .arg(&out)
        .args(["--colour", "teal"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "auto, mono, true or one of ha, cak",
        ));
    helia()
        .arg("stack")
        .arg(&ser)
        .arg("-o")
        .arg(&out)
        .args(["--preset", "ha", "--colour-strength", "2"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("between 0 and 1"));
}

#[test]
fn fits_carries_solarnet_keywords_and_the_site() {
    let dir = scratch();
    let ser = capture(dir.path());
    let with_site = dir.path().join("site.fits");
    helia()
        .arg("stack")
        .arg(&ser)
        .arg("-o")
        .arg(&with_site)
        .args([
            "--keep-frames",
            "5",
            "--preset",
            "ha",
            "--site",
            "51.5,-0.1,30",
            "-q",
        ])
        .assert()
        .success();
    let text = String::from_utf8_lossy(&std::fs::read(&with_site).expect("read")).into_owned();
    for key in [
        "EXTNAME = 'STACKED-IMAGE'",
        "SOLARNET=",
        "OBS_HDU =",
        "NSUMEXP =",
        "OBSGEO-X=",
        "OBSGEO-Z=",
        "WAVELNTH=",
        "PRSTEP1 = 'FRAME-SELECTION'",
        "CTYPE1  = 'HPLN-TAN'",
    ] {
        assert!(text.contains(key), "missing {key}");
    }
    // No timestamps and no sidecar: those keywords are left out, not invented.
    assert!(!text.contains("DATE-BEG="));
    assert!(!text.contains("XPOSURE ="));

    let without = dir.path().join("nosite.fits");
    helia()
        .env_remove("HELIA_SITE")
        .arg("stack")
        .arg(&ser)
        .arg("-o")
        .arg(&without)
        .args(["--keep-frames", "5", "--no-multipoint"])
        .assert()
        .success()
        .stderr(predicate::str::contains("no --site given"));
    let text = String::from_utf8_lossy(&std::fs::read(&without).expect("read")).into_owned();
    assert!(!text.contains("OBSGEO-X="));
    helia()
        .arg("stack")
        .arg(&ser)
        .arg("-o")
        .arg(dir.path().join("bad.fits"))
        .args(["--site", "95,0,0", "-q"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("LAT,LON,ALT"));
}

#[test]
fn align_option_chooses_the_method_and_the_summary_says_which() {
    let dir = scratch();
    let ser = capture(dir.path());
    let run = |extra: &[&str]| {
        helia()
            .arg("stack")
            .arg(&ser)
            .arg("-o")
            .arg(dir.path().join("out.tif"))
            .args(["--keep-frames", "5", "--no-multipoint", "--backend", "cpu"])
            .args(extra)
            .assert()
    };
    run(&["--align", "limb"])
        .success()
        .stderr(predicate::str::contains("align:   limb fit (chosen"))
        .stderr(predicate::str::contains("confidence mean"));
    run(&["--align", "phase"])
        .success()
        .stderr(predicate::str::contains(
            "align:   phase correlation (chosen)",
        ));
    // Auto says what it chose and why, whichever it is.
    run(&[]).success().stderr(
        predicate::str::is_match(r"align:   (limb fit|phase correlation) \(.+\)").expect("regex"),
    );
    run(&["--align", "fft"])
        .failure()
        .stderr(predicate::str::contains("auto, phase or limb"));
}

/// A SER file that says mono although its frames are an RGGB mosaic.
fn mislabelled(dir: &Path) -> PathBuf {
    let frames: Vec<_> = (0..16)
        .map(|i| {
            helia_core::testutil::mosaic_disc(
                160,
                128,
                Some(helia_core::BayerPattern::Rggb),
                [0.94, 1.0, 0.01],
                9 + i,
            )
        })
        .collect();
    let mut h =
        helia_core::io::ser::SerHeader::new(160, 128, helia_core::io::ser::ColorId::Mono, 8);
    h.instrument = "Test camera".into();
    let path = dir.join("mono-label.ser");
    std::fs::write(&path, helia_core::testutil::ser_bytes_with(&frames, h)).expect("write ser");
    path
}

#[test]
fn info_prints_the_header_and_what_will_be_used() {
    let dir = scratch();
    let ser = mislabelled(dir.path());
    helia()
        .arg("info")
        .arg(&ser)
        .assert()
        .success()
        .stdout(predicate::str::contains("FileID:"))
        .stdout(predicate::str::contains("LUCAM-RECORDER"))
        .stdout(predicate::str::contains("0 (MONO)"))
        .stdout(predicate::str::contains("Instrument:"))
        .stdout(predicate::str::contains("Test camera"))
        .stdout(predicate::str::contains("DateTimeUTC:"))
        .stdout(predicate::str::contains("Timestamps:"))
        .stdout(predicate::str::is_match(r"Bayer pattern:\s+RGGB\s+\(detection\)").expect("regex"))
        .stdout(predicate::str::contains("Mosaic check"))
        .stdout(predicate::str::contains("warning:"))
        .stdout(predicate::str::contains("header says mono"));
    // The user's word wins and is labelled as such.
    helia()
        .args(["info", "--bayer", "none"])
        .arg(&ser)
        .assert()
        .success()
        .stdout(
            predicate::str::is_match(r"Bayer pattern:\s+none \(mono\)\s+\(override\)")
                .expect("regex"),
        )
        .stdout(predicate::str::contains("Mosaic check").not());
}

#[test]
fn stack_debayers_a_mislabelled_capture_says_so_and_records_it() {
    let dir = scratch();
    let ser = mislabelled(dir.path());
    let out = dir.path().join("out.tif");
    helia()
        .arg("stack")
        .arg(&ser)
        .arg("-o")
        .arg(&out)
        .args(["--keep", "50%", "--no-multipoint"])
        .assert()
        .success()
        .stderr(predicate::str::contains("warning:"))
        .stderr(predicate::str::contains("Bayer mosaic"));
    let bytes = std::fs::read(&out).expect("output exists");
    let text = String::from_utf8_lossy(&bytes);
    assert!(
        text.contains("Capture read as: Bayer RGGB (detection)"),
        "the TIFF records how the capture was read"
    );
    // Overrides are recorded too, and a mono FITS carries the note as HISTORY.
    let fits_out = dir.path().join("out.fits");
    helia()
        .arg("stack")
        .arg(&ser)
        .arg("-o")
        .arg(&fits_out)
        .args([
            "--keep",
            "50%",
            "--no-multipoint",
            "--bayer",
            "none",
            "--endian",
            "little",
            "--quiet",
        ])
        .assert()
        .success();
    let bytes = std::fs::read(&fits_out).expect("output exists");
    let text = String::from_utf8_lossy(&bytes);
    assert!(
        text.contains("HISTORY Capture read as: Bayer none (mono) (override)"),
        "{text}"
    );
    assert!(text.contains("Overrides: --bayer none, --endian little"));
}

#[test]
fn reading_options_are_checked() {
    let dir = scratch();
    let ser = capture(dir.path());
    for bad in [
        ["--bit-depth", "0"],
        ["--bit-depth", "17"],
        ["--endian", "sideways"],
        ["--bayer", "xyz"],
    ] {
        helia().arg("info").arg(&ser).args(bad).assert().failure();
    }
}

fn stack_args(ser: &Path, out: &Path) -> Command {
    let mut c = helia();
    c.arg("stack").arg(ser).arg("-o").arg(out).args([
        "--keep",
        "30%",
        "--ap-size",
        "32",
        "--quiet",
    ]);
    c
}

#[test]
fn sharpen_runs_before_colour_and_is_recorded_in_the_tiff() {
    let dir = tempfile::tempdir().expect("tempdir");
    let ser = capture(dir.path());
    let out = dir.path().join("sharp.tif");
    stack_args(&ser, &out)
        .args([
            "--preset",
            "ha",
            "--sharpen",
            "wavelet",
            "--wavelet-gains",
            "2,1.5",
        ])
        .assert()
        .success();
    let (colour, _, desc) = decode_tiff(&out);
    assert_eq!(colour, tiff::ColorType::RGB(16));
    let desc = desc.expect("description tag");
    assert!(desc.contains("wavelet sharpen"), "{desc}");
    assert!(desc.contains("100/0.100/"), "{desc}");
    assert!(desc.contains("sharpening runs on the mono stack before any colour"));
    // The edit list is there in full, so the edit can be rerun.
    assert!(
        desc.contains("Helia edit list (JSON): [{\"op\":\"wavelet\""),
        "{desc}"
    );
    assert!(desc.contains("preset ha"));
}

#[test]
fn fits_is_the_unsharpened_stack_unless_asked() {
    let dir = tempfile::tempdir().expect("tempdir");
    let ser = capture(dir.path());
    let (plain, sharp, asked) = (
        dir.path().join("plain.fits"),
        dir.path().join("sharp.fits"),
        dir.path().join("asked.fits"),
    );
    stack_args(&ser, &plain).assert().success();
    stack_args(&ser, &sharp)
        .args(["--sharpen", "unsharp"])
        .assert()
        .success();
    stack_args(&ser, &asked)
        .args(["--sharpen", "unsharp", "--sharpen-fits"])
        .assert()
        .success();
    let (a, b, c) = (
        load_image(&plain).expect("plain"),
        load_image(&sharp).expect("sharp"),
        load_image(&asked).expect("asked"),
    );
    assert_eq!(a.data, b.data, "FITS must be the unsharpened stack");
    assert_ne!(a.data, c.data, "--sharpen-fits must sharpen it");
    let header = String::from_utf8_lossy(&std::fs::read(&sharp).expect("read")).into_owned();
    assert!(header.contains("unedited stack"), "{header}");
    let header = String::from_utf8_lossy(&std::fs::read(&asked).expect("read")).into_owned();
    assert!(header.contains("HISTORY 1. unsharp mask"), "{header}");
}

#[test]
fn rl_and_none_are_accepted_and_bad_combinations_are_explained() {
    let dir = tempfile::tempdir().expect("tempdir");
    let ser = capture(dir.path());
    let out = dir.path().join("o.tif");
    stack_args(&ser, &out)
        .args([
            "--sharpen",
            "rl",
            "--rl-iterations",
            "5",
            "--rl-psf",
            "gaussian:1.4",
        ])
        .assert()
        .success();
    let (_, _, desc) = decode_tiff(&out);
    assert!(
        desc.expect("desc").contains("5 iterations"),
        "iterations were not recorded"
    );
    stack_args(&ser, &out)
        .args(["--sharpen", "none"])
        .assert()
        .success();
    stack_args(&ser, &out)
        .args(["--unsharp-amount", "1"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("--sharpen unsharp"));
    stack_args(&ser, &out)
        .args(["--sharpen", "wobble"])
        .assert()
        .failure();
    stack_args(&ser, &out)
        .args(["--crop", "100,100,100,100"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("does not fit"));
}

#[test]
fn edit_applies_an_operation_list_to_an_existing_image() {
    let dir = tempfile::tempdir().expect("tempdir");
    let ser = capture(dir.path());
    let stacked = dir.path().join("stack.tif");
    stack_args(&ser, &stacked).assert().success();
    let out = dir.path().join("edited.tif");
    helia()
        .arg("edit")
        .arg(&stacked)
        .arg("-o")
        .arg(&out)
        .args([
            "--sharpen",
            "unsharp",
            "--gamma",
            "1.4",
            "--crop",
            "10,8,100,80",
            "--rotate",
            "90",
            "--flip-h",
            "--resize",
            "50%",
            "--quiet",
        ])
        .assert()
        .success();
    let (colour, px, desc) = decode_tiff(&out);
    assert_eq!(colour, tiff::ColorType::Gray(16));
    // 100 x 80 rotated a quarter turn is 80 x 100; halved is 40 x 50.
    assert_eq!(px.len(), 40 * 50);
    let desc = desc.expect("description");
    for what in [
        "unsharp mask",
        "levels",
        "crop 100x80 at (10,8)",
        "rotate 90.00",
        "flip horizontally",
        "resize by 0.5",
        "Edited from stack.tif",
    ] {
        assert!(desc.contains(what), "{what} missing from {desc}");
    }
}

#[test]
fn edit_keeps_colour_and_can_run_a_saved_preset() {
    let dir = tempfile::tempdir().expect("tempdir");
    let ser = capture(dir.path());
    let coloured = dir.path().join("c.tif");
    stack_args(&ser, &coloured)
        .args(["--preset", "ha"])
        .assert()
        .success();
    let presets = dir.path().join("presets");
    // Save a preset while editing a mono copy, then reuse it on the colour image.
    let mono = dir.path().join("m.tif");
    stack_args(&ser, &mono).assert().success();
    helia()
        .env("HELIA_EDIT_PRESETS", &presets)
        .arg("edit")
        .arg(&mono)
        .arg("-o")
        .arg(dir.path().join("m2.tif"))
        .args([
            "--sharpen",
            "wavelet",
            "--save-edits",
            "My Ha gentle",
            "--quiet",
        ])
        .assert()
        .success();
    assert!(presets.join("my-ha-gentle.json").is_file());
    let out = dir.path().join("c2.tif");
    helia()
        .env("HELIA_EDIT_PRESETS", &presets)
        .arg("edit")
        .arg(&coloured)
        .arg("-o")
        .arg(&out)
        .args(["--edits", "my ha gentle", "--quiet"])
        .assert()
        .success();
    let (colour, px, desc) = decode_tiff(&out);
    assert_eq!(colour, tiff::ColorType::RGB(16));
    assert_eq!(px.len(), 128 * 112 * 3);
    assert!(desc.expect("description").contains("wavelet sharpen"));
}

#[test]
fn edit_without_options_converts_the_format_and_changes_nothing() {
    let dir = tempfile::tempdir().expect("tempdir");
    let ser = capture(dir.path());
    let img = dir.path().join("i.tif");
    stack_args(&ser, &img).assert().success();
    let before = helia_core::io::any::read_any(&img).expect("read").0;
    for name in ["o.png", "o.fits", "o.tif"] {
        let out = dir.path().join(name);
        helia()
            .arg("edit")
            .arg(&img)
            .arg("-o")
            .arg(&out)
            .assert()
            .success()
            .stderr(predicate::str::contains("only converted"));
        let after = helia_core::io::any::read_any(&out).expect("read").0;
        let (helia_core::io::any::Loaded::Mono(a), helia_core::io::any::Loaded::Mono(b)) =
            (&before, &after)
        else {
            panic!("a mono stack stays mono");
        };
        assert_eq!((a.width, a.height), (b.width, b.height), "{name}");
        // A PNG is 8-bit (half a step is 0.002); TIFF and FITS keep far more.
        let allowed = if out
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("png"))
        {
            0.0025
        } else {
            2.0e-5
        };
        let worst = a
            .data
            .iter()
            .zip(&b.data)
            .map(|(x, y)| (x - y).abs())
            .fold(0.0f32, f32::max);
        assert!(worst < allowed, "{name}: worst difference {worst}");
    }
}

#[test]
fn edit_with_a_missing_input_or_bad_edit_fails_plainly() {
    let dir = tempfile::tempdir().expect("tempdir");
    let ser = capture(dir.path());
    let img = dir.path().join("i.tif");
    stack_args(&ser, &img).assert().success();
    helia()
        .arg("edit")
        .arg(dir.path().join("missing.tif"))
        .arg("-o")
        .arg(dir.path().join("o.tif"))
        .args(["--sharpen", "wavelet"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("cannot read"));
}

#[test]
fn feedback_dry_run_shows_the_report_and_sends_nothing() {
    helia()
        .args([
            "feedback",
            "--kind",
            "feature",
            "--title",
            "Dark mode",
            "--description",
            "Please.",
            "--system-info",
            "--dry-run",
        ])
        .env("HELIA_FEEDBACK_URL", "http://127.0.0.1:1")
        .assert()
        .success()
        .stdout(predicate::str::contains("\"kind\": \"feature\""))
        .stdout(predicate::str::contains("\"title\": \"Dark mode\""))
        .stdout(predicate::str::contains("fetched from the relay"))
        .stdout(predicate::str::contains("Nothing was sent"))
        .stderr(predicate::str::contains(
            "sent to the Helia developers and filed in the Helia issue tracker",
        ));
}

#[test]
fn feedback_without_a_reachable_service_prints_the_fallbacks_and_fails() {
    let dir = tempfile::tempdir().expect("tempdir");
    let zip = dir.path().join("report.zip");
    helia()
        .args(["feedback", "--title", "It broke", "--save"])
        .arg(&zip)
        // Nothing listens on port 1.
        .env("HELIA_FEEDBACK_URL", "http://127.0.0.1:1")
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "could not reach the feedback service",
        ))
        .stderr(predicate::str::contains("Saved the report"))
        .stderr(predicate::str::contains(
            "/issues/new?title=%5BBug%5D%20It%20broke",
        ));
    assert!(zip.is_file(), "the report was saved");
}

#[test]
fn feedback_refuses_an_invalid_report_before_any_network() {
    helia()
        .args(["feedback", "--title", "  "])
        .assert()
        .failure()
        .stderr(predicate::str::contains("the title is empty"));
    helia()
        .args(["feedback", "--title", "x", "--highlight", "1,2,3,4"])
        .assert()
        .failure();
}

#[test]
fn long_version_says_pre_release_while_the_version_is_zero_point_x() {
    let out = finished(helia().arg("--version").output());
    let text = String::from_utf8_lossy(&out.stdout);
    assert_eq!(
        text.contains("release: pre-release"),
        helia_core::about::PRE_RELEASE
    );
    assert_eq!(
        helia_core::about::PRE_RELEASE,
        env!("CARGO_PKG_VERSION").starts_with("0."),
        "pre-release exactly while the version is 0.x"
    );
}

/// A capture through a sensor with dust, vignetting and hot pixels, with the
/// dark and flat captures of that sensor next to it.
fn defective_rig(dir: &Path) -> (PathBuf, PathBuf, PathBuf) {
    use helia_core::testutil::{DefectConfig, SensorDefects};
    let sun = SyntheticSun::new(&SyntheticConfig {
        width: 128,
        height: 112,
        ..SyntheticConfig::default()
    });
    let cap = SyntheticCapture::generate(
        &sun,
        &CaptureConfig {
            frames: 20,
            noise: 0.01,
            ..CaptureConfig::default()
        },
    );
    let sensor = SensorDefects::new(128, 112, 6, &DefectConfig::default());
    let lights: Vec<helia_core::Frame> = cap
        .frames
        .iter()
        .map(|f| {
            let mut d = f.clone();
            d.data.iter_mut().for_each(|v| *v *= 0.75);
            sensor.apply(&mut d);
            d
        })
        .collect();
    let (sun_p, dark_p, flat_p) = (
        dir.join("sun.ser"),
        dir.join("darks.ser"),
        dir.join("flats.ser"),
    );
    std::fs::write(&sun_p, ser_bytes(&lights)).expect("write");
    std::fs::write(&dark_p, ser_bytes(&sensor.dark_frames(16, 0.004, 1))).expect("write");
    std::fs::write(&flat_p, ser_bytes(&sensor.flat_frames(16, 0.6, 0.004, 2))).expect("write");
    (sun_p, dark_p, flat_p)
}

#[test]
fn calibrate_writes_masters_and_stack_applies_and_records_them() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (sun, darks, flats) = defective_rig(dir.path());
    let out = dir.path().join("masters");
    helia()
        .arg("calibrate")
        .arg("--darks")
        .arg(&darks)
        .arg("--flats")
        .arg(&flats)
        .arg("--flat-darks")
        .arg(&darks)
        .arg("-o")
        .arg(&out)
        .arg("--quiet")
        .assert()
        .success()
        .stdout(predicate::str::contains("master-dark.fits"))
        .stdout(predicate::str::contains("master-flat.fits"))
        .stdout(predicate::str::contains("bad-pixels.fits"))
        .stdout(predicate::str::contains("next:     helia stack"));
    for f in ["master-dark.fits", "master-flat.fits", "bad-pixels.fits"] {
        assert!(out.join(f).is_file(), "{f}");
    }
    // The three outputs of one stack: FITS, and a TIFF with the notes.
    let fits_out = dir.path().join("cal.fits");
    let tif_out = dir.path().join("cal.tif");
    for o in [&fits_out, &tif_out] {
        stack_args(&sun, o)
            .arg("--dark")
            .arg(out.join("master-dark.fits"))
            .arg("--flat")
            .arg(out.join("master-flat.fits"))
            .arg("--bad-pixels")
            .arg(out.join("bad-pixels.fits"))
            .assert()
            .success();
    }
    let header = String::from_utf8_lossy(&std::fs::read(&fits_out).expect("read")).into_owned();
    for want in [
        "DARK-SUBTRACTION",
        "FLAT-FIELDING",
        "BAD-PIXEL-CORRECTION",
        "HISTORY Calibration: dark subtracted",
    ] {
        assert!(header.contains(want), "{want} missing from the FITS header");
    }
    let (_, _, description) = decode_tiff(&tif_out);
    let d = description.expect("a description");
    assert!(d.contains("Calibration: flat-fielded"), "{d}");
    assert!(d.contains("Calibration: bad pixels corrected"), "{d}");
}

#[test]
fn stack_builds_masters_from_capture_files_and_calibration_changes_the_result() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (sun, darks, flats) = defective_rig(dir.path());
    let (plain, cal) = (dir.path().join("plain.fits"), dir.path().join("cal.fits"));
    stack_args(&sun, &plain).assert().success();
    stack_args(&sun, &cal)
        .arg("--dark")
        .arg(&darks)
        .arg("--flat")
        .arg(&flats)
        .arg("--flat-dark")
        .arg(&darks)
        .assert()
        .success();
    let (a, b) = (load_image(&plain).expect("a"), load_image(&cal).expect("b"));
    assert_ne!(a.data, b.data);
}

#[test]
fn calibration_that_does_not_fit_is_refused_before_stacking() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (sun, _, _) = defective_rig(dir.path());
    let other = dir.path().join("other.ser");
    std::fs::write(&other, {
        let f = helia_core::Frame::new(64, 48, vec![0.1; 64 * 48]).expect("frame");
        ser_bytes(&[f.clone(), f])
    })
    .expect("write");
    stack_args(&sun, &dir.path().join("x.fits"))
        .arg("--dark")
        .arg(&other)
        .assert()
        .failure()
        .stderr(predicate::str::contains("same camera"));
    helia()
        .arg("calibrate")
        .assert()
        .failure()
        .stderr(predicate::str::contains("nothing to do"));
    stack_args(&sun, &dir.path().join("y.fits"))
        .arg("--bad-pixels")
        .arg(dir.path().join("missing.txt"))
        .assert()
        .failure();
}

#[test]
fn calibrate_warns_about_a_flat_without_a_dark_flat_and_about_ha_etalons() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (_, _, flats) = defective_rig(dir.path());
    helia()
        .arg("calibrate")
        .arg("--flats")
        .arg(&flats)
        .arg("--preset")
        .arg("ha")
        .arg("-o")
        .arg(dir.path().join("m"))
        .arg("--quiet")
        .assert()
        .success()
        .stderr(predicate::str::contains("no dark flat"))
        .stderr(predicate::str::contains("sweet spot"));
}

/// Captures of one disc that is a little further along in each, named by their
/// start time the way `SharpCap` names them.
fn moving_captures(dir: &Path) -> Vec<PathBuf> {
    let base = SyntheticSun::new(&SyntheticConfig {
        width: 160,
        height: 128,
        radius_frac: 0.36,
        ..SyntheticConfig::default()
    });
    [("10_00_00", 0.0f32), ("10_05_00", 3.2), ("10_10_00", -2.4)]
        .iter()
        .map(|&(name, dx)| {
            let cap = SyntheticCapture::generate(
                &base,
                &CaptureConfig {
                    frames: 12,
                    noise: 0.02,
                    ..CaptureConfig::default()
                },
            );
            let frames: Vec<_> = cap
                .frames
                .iter()
                .map(|f| {
                    helia_core::resample::shifted(
                        f,
                        helia_core::Shift::new(-dx, 0.0),
                        helia_core::Kernel::Bilinear,
                    )
                })
                .collect();
            let path = dir.join(format!("{name}.ser"));
            std::fs::write(&path, ser_bytes(&frames)).expect("write ser");
            path
        })
        .collect()
}

#[test]
fn timelapse_stacks_registers_and_writes_a_numbered_sequence() {
    let dir = scratch();
    let caps = moving_captures(dir.path());
    let out = dir.path().join("tl");
    helia()
        .arg("timelapse")
        .args(&caps)
        .args(["-o"])
        .arg(&out)
        .args([
            "--date",
            "2026-10-20",
            "--movie",
            "apng",
            "--deflicker",
            "match",
            "--",
        ])
        .args(["--keep", "50%", "--no-multipoint"])
        .assert()
        .success()
        .stdout(predicate::str::contains("3 frames registered"))
        .stdout(predicate::str::contains("limb+phase"));
    for f in [
        "frame-0001.png",
        "frame-0002.png",
        "frame-0003.png",
        "timelapse.png",
        "timelapse.csv",
    ] {
        assert!(out.join(f).is_file(), "{f}");
    }
    assert!(out.join("stacks/0002-10_05_00.tif").is_file());
    let csv = std::fs::read_to_string(out.join("timelapse.csv")).expect("csv");
    assert!(csv.contains("2026-10-20T10:05:00"), "{csv}");
    // A second run can reuse the stacks.
    helia()
        .arg("timelapse")
        .args(&caps)
        .args(["-o"])
        .arg(&out)
        .args([
            "--date",
            "2026-10-20",
            "--movie",
            "none",
            "--reuse-stacks",
            "--format",
            "tiff",
            "--",
        ])
        .args(["--no-multipoint"])
        .assert()
        .success()
        .stderr(predicate::str::contains("using"));
    assert!(out.join("frame-0001.tif").is_file());
}

#[test]
fn timelapse_takes_stacked_images_and_explains_bad_input() {
    let dir = scratch();
    let sun = SyntheticSun::new(&SyntheticConfig {
        width: 160,
        height: 128,
        radius_frac: 0.36,
        ..SyntheticConfig::default()
    });
    let mut files = Vec::new();
    for (i, dx) in [0.0f32, 2.5].iter().enumerate() {
        let p = dir.path().join(format!("s{i}.tif"));
        helia_core::io::save_image(
            &p,
            &sun.render_clean(helia_core::Shift::new(*dx, 0.0), None),
        )
        .expect("save");
        files.push(p);
    }
    let out = dir.path().join("o");
    helia()
        .arg("timelapse")
        .args(&files)
        .arg("-o")
        .arg(&out)
        .args(["--movie", "apng", "--crop"])
        .assert()
        .success()
        .stdout(predicate::str::contains("cropped to the common area"));
    helia()
        .arg("timelapse")
        .args(&files)
        .arg("-o")
        .arg(&out)
        .args(["--reference", "9"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("reference frame"));
    helia()
        .arg("timelapse")
        .args(&files)
        .arg("-o")
        .arg(&out)
        .args(["--deflicker", "smooth", "--deflicker-window", "4"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("odd"));
    helia()
        .arg("timelapse")
        .args(&files)
        .arg("-o")
        .arg(&out)
        .args(["--", "--nope"])
        .assert()
        .success(); // stacked images are used as they are, so no stack setting is read
}

/// Two views of a spotted Sun six hours apart, named by their times.
fn rotating_sun(dir: &Path) -> (PathBuf, PathBuf, helia_core::Frame) {
    use helia_core::derotate::{DiscGeometry, Orientation, RotationLaw, synth::render_sun};
    let disc = DiscGeometry {
        cx: 320.0,
        cy: 260.0,
        r: 240.0,
    };
    let feats = [
        (0.0, -5.0),
        (35.0, -12.0),
        (-35.0, -8.0),
        (20.0, -35.0),
        (-20.0, 10.0),
    ];
    let t0 = helia_core::io::solarnet::unix_from_iso("2026-10-20T09:00:00").expect("time");
    let at = |h: f64| {
        render_sun(
            640,
            520,
            disc,
            t0 + h * 3600.0,
            t0,
            RotationLaw::Howard,
            Orientation::default(),
            &feats,
        )
    };
    let (a, b) = (
        dir.join("sun_20261020_090000.tif"),
        dir.join("sun_20261020_150000.tif"),
    );
    helia_core::io::save_image(&a, &at(0.0)).expect("save");
    helia_core::io::save_image(&b, &at(6.0)).expect("save");
    (a, b, at(0.0))
}

#[test]
fn solar_derotate_brings_a_later_image_back_to_an_earlier_time() {
    let dir = scratch();
    let (a, b, truth) = rotating_sun(dir.path());
    let out = dir.path().join("d");
    helia()
        .arg("solar-derotate")
        .args([&a, &b])
        .arg("-o")
        .arg(&out)
        .args(["--to", "first"])
        .assert()
        .success()
        .stdout(predicate::str::contains("moved up to 13."))
        .stdout(predicate::str::contains("Howard rates"));
    let mono = |p: &Path| match helia_core::io::any::read_any(p).expect("read").0 {
        helia_core::io::any::Loaded::Mono(f) => f,
        helia_core::io::any::Loaded::Rgb(_) => panic!("mono"),
    };
    let got = mono(&out.join("sun_20261020_150000-derotated.tif"));
    let before = mono(&b);
    let err = |x: &helia_core::Frame| {
        x.data
            .iter()
            .zip(&truth.data)
            .map(|(p, q)| f64::from(p - q).powi(2))
            .sum::<f64>()
    };
    assert!(
        err(&got) < 0.05 * err(&before),
        "{} vs {}",
        err(&got),
        err(&before)
    );
    // The first image is already at its own time.
    assert!(out.join("sun_20261020_090000-derotated.tif").is_file());
}

#[test]
fn solar_derotate_explains_what_it_cannot_do() {
    let dir = scratch();
    let (a, b, _) = rotating_sun(dir.path());
    let out = dir.path().join("d");
    let plain = dir.path().join("15_32_09.tif");
    std::fs::copy(&a, &plain).expect("copy");
    helia()
        .arg("solar-derotate")
        .args([&a, &plain])
        .arg("-o")
        .arg(&out)
        .assert()
        .failure()
        .stderr(predicate::str::contains("no time for"));
    helia()
        .arg("solar-derotate")
        .args([&a, &b])
        .arg("-o")
        .arg(&out)
        .args(["--rotation-law", "fast"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("unknown rotation law"));
    helia()
        .arg("solar-derotate")
        .args([&a, &b])
        .arg("-o")
        .arg(&out)
        .args(["--to", "yesterday"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("cannot read the time"));
}

#[test]
fn a_timelapse_can_derotate_and_says_so() {
    let dir = scratch();
    let (a, b, _) = rotating_sun(dir.path());
    let out = dir.path().join("tl");
    helia()
        .arg("timelapse")
        .args([&a, &b])
        .arg("-o")
        .arg(&out)
        .args([
            "--movie",
            "none",
            "--derotate",
            "--rotation-law",
            "snodgrass",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("derotated to 2026-10-20 15:00:00"));
    let csv = std::fs::read_to_string(out.join("timelapse.csv")).expect("csv");
    assert!(
        csv.lines()
            .next()
            .is_some_and(|l| l.ends_with("derotated_px"))
    );
    // A series with no times cannot be derotated.
    let plain = dir.path().join("p1.tif");
    let plain2 = dir.path().join("p2.tif");
    std::fs::copy(&a, &plain).expect("copy");
    std::fs::copy(&b, &plain2).expect("copy");
    helia()
        .arg("timelapse")
        .args([&plain, &plain2])
        .arg("-o")
        .arg(dir.path().join("tl2"))
        .args(["--movie", "none", "--derotate"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("time of every frame"));
}

/// A capture whose frames are stamped `step` apart, from 2022-07-17 15:32 UTC.
fn stamped_capture(dir: &Path, step_seconds: i64) -> PathBuf {
    use helia_core::io::ser::{ColorId, SerHeader};
    use helia_core::testutil::ser_bytes_stamped;
    let sun = SyntheticSun::new(&SyntheticConfig {
        width: 128,
        height: 112,
        ..SyntheticConfig::default()
    });
    let cap = SyntheticCapture::generate(
        &sun,
        &CaptureConfig {
            frames: 20,
            noise: 0.02,
            ..CaptureConfig::default()
        },
    );
    let path = dir.join("stamped.ser");
    let header = SerHeader::new(128, 112, ColorId::Mono, 16);
    std::fs::write(
        &path,
        ser_bytes_stamped(
            &cap.frames,
            header,
            637_936_687_290_000_000,
            step_seconds * 10_000_000,
        ),
    )
    .expect("write ser");
    path
}

#[test]
fn field_rotation_turns_the_frames_and_says_so() {
    let dir = scratch();
    let ser = stamped_capture(dir.path(), 90);
    let out = dir.path().join("rot.fits");
    helia()
        .arg("stack")
        .arg(&ser)
        .arg("-o")
        .arg(&out)
        .args([
            "--field-rotation",
            "--site",
            "51.5,-0.1,30",
            "--no-multipoint",
            "--backend",
            "cpu",
            "--keep-frames",
            "8",
        ])
        .env_remove("HELIA_SITE")
        .assert()
        .success()
        .stderr(predicate::str::contains("rotation:"))
        .stderr(predicate::str::contains("parallactic angle runs"));
    let (_, header) = fits::read_fits_header(std::io::BufReader::new(
        std::fs::File::open(&out).expect("open"),
    ))
    .expect("fits");
    assert!(header.values().any(|v| v == "FIELD-ROTATION"), "{header:?}");
}

#[test]
fn field_rotation_options_are_checked() {
    let dir = scratch();
    let stamped = stamped_capture(dir.path(), 90);
    let plain = capture(dir.path());
    let out = dir.path().join("x.fits");
    // No site.
    helia()
        .arg("stack")
        .arg(&stamped)
        .arg("-o")
        .arg(&out)
        .arg("--field-rotation")
        .env_remove("HELIA_SITE")
        .assert()
        .failure()
        .stderr(predicate::str::contains("needs the observing site"));
    // No time stamps.
    helia()
        .arg("stack")
        .arg(&plain)
        .arg("-o")
        .arg(&out)
        .args(["--field-rotation", "--site", "51.5,-0.1"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("needs the time of each frame"));
    // The tuning belongs to the correction.
    helia()
        .arg("stack")
        .arg(&stamped)
        .arg("-o")
        .arg(&out)
        .arg("--field-rotation-mirror")
        .assert()
        .failure();
}

#[test]
fn derotate_turns_a_session_of_images_to_one_orientation() {
    let dir = scratch();
    let sun = SyntheticSun::new(&SyntheticConfig {
        width: 128,
        height: 112,
        ..SyntheticConfig::default()
    });
    let names = ["a.fits", "b.fits", "c.fits"];
    for n in names {
        let mut f =
            std::io::BufWriter::new(std::fs::File::create(dir.path().join(n)).expect("create"));
        fits::write_fits(&mut f, sun.truth(), fits::FitsFormat::Float32).expect("write");
    }
    let out = dir.path().join("turned");
    helia()
        .arg("derotate")
        .args(names.map(|n| dir.path().join(n)))
        .arg("-o")
        .arg(&out)
        .args([
            "--site",
            "51.5,-0.1,30",
            "--times",
            "2026-03-20T11:00:00,2026-03-20T12:00:00,2026-03-20T13:00:00",
        ])
        .env_remove("HELIA_SITE")
        .assert()
        .success()
        .stdout(predicate::str::contains("parallactic angle runs"))
        .stdout(predicate::str::contains("+0.000"));
    let read = |n: &str| {
        fits::read_fits(std::io::BufReader::new(
            std::fs::File::open(out.join(n)).expect("open"),
        ))
        .expect("fits")
    };
    let (first, middle, last) = (
        read("a-derot.fits"),
        read("b-derot.fits"),
        read("c-derot.fits"),
    );
    let rms = |a: &helia_core::Frame, b: &helia_core::Frame| {
        (a.data
            .iter()
            .zip(&b.data)
            .map(|(x, y)| (x - y).powi(2))
            .sum::<f32>()
            / a.data.len() as f32)
            .sqrt()
    };
    // The middle image is the reference: unchanged. The others are turned.
    assert!(rms(&middle, sun.truth()) < 1e-4);
    assert!(rms(&first, sun.truth()) > 0.01);
    assert!(rms(&last, sun.truth()) > 0.01);
    // A second run refuses to overwrite.
    helia()
        .arg("derotate")
        .arg(dir.path().join("a.fits"))
        .arg("-o")
        .arg(&out)
        .args(["--site", "51.5,-0.1", "--times", "2026-03-20T11:00:00"])
        .env_remove("HELIA_SITE")
        .assert()
        .failure()
        .stderr(predicate::str::contains("exists"));
    // An image with no time is named.
    helia()
        .arg("derotate")
        .arg(dir.path().join("a.fits"))
        .arg("-o")
        .arg(dir.path().join("elsewhere"))
        .args(["--site", "51.5,-0.1"])
        .env_remove("HELIA_SITE")
        .assert()
        .failure()
        .stderr(predicate::str::contains("has no time"));
}

/// Four overlapping panels of one synthetic Sun, a little turned and of different gains,
/// written as TIFF files; returns the paths.
#[allow(
    clippy::many_single_char_names,
    reason = "x, y, u, v, s, c are the natural names in the cutting of panels"
)]
fn mosaic_panels(dir: &Path) -> Vec<PathBuf> {
    use helia_core::resample::{Kernel, sample};
    let sun = SyntheticSun::new(&SyntheticConfig {
        width: 520,
        height: 440,
        radius_frac: 0.43,
        granulation: 0.15,
        spots: 5,
        ..SyntheticConfig::default()
    });
    let truth = sun.truth();
    let cuts = [
        ((8.0f64, 10.0f64), 0.0f64, (1.0f32, 0.0f32)),
        ((210.0, 14.0), 1.0, (0.93, 0.015)),
        ((12.0, 170.0), -0.8, (1.06, -0.01)),
        ((214.0, 176.0), 0.6, (0.97, 0.008)),
    ];
    cuts.iter()
        .enumerate()
        .map(|(i, &((x0, y0), deg, (g, o)))| {
            let (w, h) = (310usize, 270usize);
            let mut f = helia_core::Frame::zeros(w, h);
            let (s, c) = deg.to_radians().sin_cos();
            for y in 0..h {
                for x in 0..w {
                    let (u, v) = (x as f64 - 154.5, y as f64 - 134.5);
                    let (qx, qy) = (c * u - s * v + 154.5 + x0, s * u + c * v + 134.5 + y0);
                    f.data[y * w + x] =
                        sample(truth, qx as f32, qy as f32, Kernel::Lanczos3) * 0.55 * g + o;
                }
            }
            let p = dir.join(format!("panel{}.tif", i + 1));
            helia_core::io::save_image(&p, &f).expect("save");
            p
        })
        .collect()
}

#[test]
fn mosaic_joins_panels_and_writes_the_tables() {
    let dir = scratch();
    let panels = mosaic_panels(dir.path());
    let out = dir.path().join("m").join("sun.tif");
    std::fs::create_dir_all(out.parent().expect("parent")).expect("dir");
    helia()
        .arg("mosaic")
        .args(&panels)
        .arg("-o")
        .arg(&out)
        .assert()
        .success()
        .stdout(predicate::str::contains("4 panels joined into"))
        .stdout(predicate::str::contains("gain 1.07"));
    let (loaded, _) = helia_core::io::any::read_any(&out).expect("read");
    let (w, h) = loaded.size();
    assert!(
        (500..=520).contains(&w) && (430..=450).contains(&h),
        "{w} x {h}"
    );
    let panels_csv = std::fs::read_to_string(dir.path().join("m/sun-panels.csv")).expect("csv");
    assert_eq!(panels_csv.lines().count(), 5, "{panels_csv}");
    assert!(
        panels_csv
            .lines()
            .next()
            .is_some_and(|l| l.starts_with("panel,file,time_utc,x_px"))
    );
    let pairs = std::fs::read_to_string(dir.path().join("m/sun-pairs.csv")).expect("csv");
    assert!(pairs.lines().count() >= 4, "{pairs}");
    // Options that change the result still work together.
    helia()
        .arg("mosaic")
        .args(&panels)
        .arg("-o")
        .arg(dir.path().join("m/plain.png"))
        .args([
            "--no-gain-match",
            "--no-seams",
            "--max-rotation",
            "0",
            "--feather",
            "4",
        ])
        .assert()
        .success();
}

#[test]
fn mosaic_explains_what_it_cannot_do() {
    let dir = scratch();
    let panels = mosaic_panels(dir.path());
    let out = dir.path().join("o.tif");
    helia()
        .arg("mosaic")
        .arg(&panels[0])
        .arg("-o")
        .arg(&out)
        .assert()
        .failure()
        .stderr(predicate::str::contains("at least two"));
    helia()
        .arg("mosaic")
        .args(&panels)
        .arg("-o")
        .arg(&out)
        .args(["--max-rotation", "90"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("--max-rotation"));
    helia()
        .arg("mosaic")
        .args(&panels)
        .arg("-o")
        .arg(&out)
        .arg("--derotate")
        .assert()
        .failure()
        .stderr(predicate::str::contains("time of every panel"));
    // Two panels that share nothing.
    let a = dir.path().join("a.tif");
    let b = dir.path().join("b.tif");
    let mut rng = helia_core::testutil::Rng::new(4);
    for p in [&a, &b] {
        let mut f = helia_core::Frame::zeros(120, 100);
        f.data.iter_mut().for_each(|v| *v = 0.5 + 0.2 * rng.gauss());
        helia_core::io::save_image(p, &f).expect("save");
    }
    helia()
        .arg("mosaic")
        .args([&a, &b])
        .arg("-o")
        .arg(&out)
        .assert()
        .failure()
        .stderr(predicate::str::contains("overlap"));
}

#[test]
fn stack_report_writes_figures_data_and_a_self_contained_page() {
    let dir = scratch();
    let ser = capture(dir.path());
    let out = dir.path().join("out.fits");
    let report = dir.path().join("rep");
    helia()
        .args(["stack"])
        .arg(&ser)
        .arg("-o")
        .arg(&out)
        .args(["--keep", "30%", "--ap-size", "32", "--quiet", "--report"])
        .arg(&report)
        .assert()
        .success();
    for f in [
        "report.html",
        "frame-quality.csv",
        "frame-quality.json",
        "quality-sorted.png",
        "quality-order.png",
        "quality-histogram.png",
        "shift-drift.png",
        "fits-header.txt",
        "image-science.json",
        "histogram.png",
        "histogram.csv",
        "noise-snr.png",
        "noise.csv",
    ] {
        assert!(report.join(f).is_file(), "{f} is missing");
    }
    let png = std::fs::read(report.join("quality-sorted.png")).expect("png");
    assert_eq!(&png[1..4], b"PNG");
    let html = std::fs::read_to_string(report.join("report.html")).expect("html");
    assert!(html.contains("data:image/png;base64,") && html.contains("Settings used"));
    assert!(
        !html.contains("src=\"quality-sorted.png\""),
        "the figures are inline"
    );
    let csv = std::fs::read_to_string(report.join("frame-quality.csv")).expect("csv");
    assert_eq!(csv.lines().count(), 21, "a header and a row per frame");

    // `helia plot` redraws a figure from the data the report wrote.
    let again = dir.path().join("again.png");
    helia()
        .args(["plot", "quality-histogram"])
        .arg(&report)
        .arg("-o")
        .arg(&again)
        .args(["--size", "640x360"])
        .assert()
        .success();
    let img = std::fs::read(&again).expect("redrawn");
    assert_eq!(&img[1..4], b"PNG");
    assert_eq!(
        u32::from_be_bytes(img[16..20].try_into().expect("width")),
        640
    );
    helia()
        .args(["plot", "no-such-plot"])
        .arg(&report)
        .assert()
        .failure()
        .stderr(predicate::str::contains("unknown plot"));
}

#[test]
fn stack_report_draws_the_alignment_point_plots() {
    let dir = scratch();
    let ser = capture(dir.path());
    let out = dir.path().join("out.tif");
    let report = dir.path().join("rep");
    helia()
        .args(["stack"])
        .arg(&ser)
        .arg("-o")
        .arg(&out)
        .args([
            "--keep",
            "30%",
            "--ap-size",
            "32",
            "--ap-residual",
            "--quiet",
            "--report",
        ])
        .arg(&report)
        .assert()
        .success();
    for f in [
        "ap-grid.png",
        "ap-quality.png",
        "ap-shift.png",
        "ap-field.png",
        "ap-residual.png",
        "ap-table.csv",
        "ap-data.json",
        "ap-reference.png",
    ] {
        assert!(report.join(f).is_file(), "{f} is missing");
    }
    let csv = std::fs::read_to_string(report.join("ap-table.csv")).expect("csv");
    assert!(csv.starts_with("index,cx,cy,active,"), "{csv}");
    assert!(csv.lines().count() > 4);
    // Redrawn from the data and the reference picture the report wrote.
    let again = dir.path().join("grid.png");
    helia()
        .args(["plot", "ap-grid"])
        .arg(&report)
        .arg("-o")
        .arg(&again)
        .assert()
        .success();
    assert_eq!(&std::fs::read(&again).expect("png")[1..4], b"PNG");
    // Without --ap-residual there is no residual figure, and the report says why.
    let plain = dir.path().join("plain");
    helia()
        .args(["stack"])
        .arg(&ser)
        .arg("-o")
        .arg(&out)
        .args(["--keep", "30%", "--ap-size", "32", "--report"])
        .arg(&plain)
        .assert()
        .success()
        .stderr(predicate::str::contains(
            "ap-residual: the residual was not measured",
        ));
    assert!(!plain.join("ap-residual.png").exists());
}

#[test]
fn stack_report_draws_the_heliographic_grids_when_the_time_is_known() {
    let dir = scratch();
    let ser = stamped_capture(dir.path(), 1);
    let out = dir.path().join("out.fits");
    let report = dir.path().join("rep");
    helia()
        .args(["stack"])
        .arg(&ser)
        .arg("-o")
        .arg(&out)
        .args(["--keep", "30%", "--ap-size", "32", "--quiet", "--report"])
        .arg(&report)
        .assert()
        .success();
    for f in [
        "grid-stonyhurst.png",
        "grid-carrington.png",
        "heliographic-grid.csv",
        "solar.json",
        "solar-reference.png",
    ] {
        assert!(report.join(f).is_file(), "{f} is missing");
    }
    let csv = std::fs::read_to_string(report.join("heliographic-grid.csv")).expect("csv");
    assert!(csv.starts_with("system,line,value_deg,x_px,y_px"));
    assert!(csv.contains("stonyhurst,CM,0,") && csv.contains("carrington,"));
    let json = std::fs::read_to_string(report.join("solar.json")).expect("json");
    assert!(json.contains("\"l0_deg\"") && json.contains("\"p_deg\""));
    helia()
        .args(["plot", "grid-carrington"])
        .arg(&report)
        .arg("-o")
        .arg(dir.path().join("c.png"))
        .assert()
        .success();
    // A capture with no time cannot be gridded, and the report says so.
    let ser = capture(dir.path());
    let plain = dir.path().join("plain");
    helia()
        .args(["stack"])
        .arg(&ser)
        .arg("-o")
        .arg(dir.path().join("p.tif"))
        .args(["--keep", "30%", "--ap-size", "32", "--report"])
        .arg(&plain)
        .assert()
        .success()
        .stderr(predicate::str::contains(
            "grid-stonyhurst: the time of the capture is unknown",
        ));
    assert!(!plain.join("grid-stonyhurst.png").exists());
    // --date supplies the time.
    let dated = dir.path().join("dated");
    helia()
        .args(["stack"])
        .arg(&ser)
        .arg("-o")
        .arg(dir.path().join("d.tif"))
        .args([
            "--keep",
            "30%",
            "--ap-size",
            "32",
            "--quiet",
            "--date",
            "2026-10-20T11:30:00",
            "--report",
        ])
        .arg(&dated)
        .assert()
        .success();
    assert!(dated.join("grid-stonyhurst.png").is_file());
    let json = std::fs::read_to_string(dated.join("solar.json")).expect("json");
    assert!(json.contains("2026-10-20T11:30:00"));
}

#[test]
fn a_timelapse_report_has_the_registration_brightness_and_derotation_diagnostics() {
    let dir = scratch();
    let (a, b, _) = rotating_sun(dir.path());
    let out = dir.path().join("tl");
    let report = dir.path().join("diag");
    helia()
        .arg("timelapse")
        .args([&a, &b])
        .arg("-o")
        .arg(&out)
        .args([
            "--movie",
            "none",
            "--derotate",
            "--deflicker",
            "match",
            "--report",
        ])
        .arg(&report)
        .assert()
        .success();
    for f in [
        "timelapse-registration.png",
        "timelapse-brightness.png",
        "timelapse-derotation.png",
        "timelapse-frames.csv",
        "timelapse.json",
        "report.html",
    ] {
        assert!(report.join(f).is_file(), "{f} is missing");
    }
    let csv = std::fs::read_to_string(report.join("timelapse-frames.csv")).expect("csv");
    assert_eq!(csv.lines().count(), 3);
    helia()
        .args(["plot", "timelapse-derotation"])
        .arg(&report)
        .arg("-o")
        .arg(dir.path().join("d.png"))
        .assert()
        .success();
}

/// The frames `capture` writes as a SER file.
fn synthetic_frames() -> Vec<helia_core::Frame> {
    let sun = SyntheticSun::new(&SyntheticConfig {
        width: 128,
        height: 112,
        ..SyntheticConfig::default()
    });
    SyntheticCapture::generate(
        &sun,
        &CaptureConfig {
            frames: 20,
            noise: 0.02,
            ..CaptureConfig::default()
        },
    )
    .frames
}

/// One FITS file per frame, 16-bit with BZERO as a Seestar writes them, each with a time and
/// the observing site (which a stack must not copy into its output).
fn write_fits_subs(dir: &Path, frames: &[helia_core::Frame], extra: &[fits::HeaderCard]) {
    std::fs::create_dir_all(dir).expect("subs dir");
    for (i, f) in frames.iter().enumerate() {
        let mut cards = vec![
            fits::HeaderCard::string("DATE-OBS", format!("2026-03-21T10:00:{i:02}.500"), ""),
            fits::HeaderCard::float("SITELAT", 51.5498, ""),
            fits::HeaderCard::float("SITELONG", -1.79183, ""),
            fits::HeaderCard::float("EXPTIME", 0.01, ""),
        ];
        cards.extend_from_slice(extra);
        let mut v = Vec::new();
        fits::write_fits_cards(&mut v, f, fits::FitsFormat::Int16, &cards, &[]).expect("encode");
        std::fs::write(dir.join(format!("zq_{i:03}.fit")), v).expect("write sub");
    }
}

fn max_difference(a: &Path, b: &Path) -> f32 {
    let (a, b) = (load_image(a).expect("a"), load_image(b).expect("b"));
    assert_eq!((a.width, a.height), (b.width, b.height));
    a.data
        .iter()
        .zip(&b.data)
        .map(|(x, y)| (x - y).abs())
        .fold(0.0, f32::max)
}

#[test]
fn stack_reads_a_folder_a_pattern_and_a_cube_of_fits_frames_like_the_ser() {
    let dir = scratch();
    let frames = synthetic_frames();
    let ser = dir.path().join("sun.ser");
    std::fs::write(&ser, ser_bytes(&frames)).expect("write ser");
    let subs = dir.path().join("zq_fits");
    write_fits_subs(&subs, &frames, &[]);
    let refs: Vec<&helia_core::Frame> = frames.iter().collect();
    let mut cube = Vec::new();
    fits::write_fits_planes(&mut cube, &refs, fits::FitsFormat::Int16, &[], &[]).expect("cube");
    let cube_path = dir.path().join("zq_cube.fits");
    std::fs::write(&cube_path, cube).expect("write cube");

    let from_ser = dir.path().join("ser.fits");
    stack_args(&ser, &from_ser).assert().success();
    for (name, input) in [
        ("folder", subs.clone()),
        ("pattern", subs.join("zq_*.fit")),
        ("cube", cube_path),
    ] {
        let out = dir.path().join(format!("{name}.fits"));
        stack_args(&input, &out).assert().success();
        let d = max_difference(&from_ser, &out);
        assert!(d < 2e-4, "{name}: differs from the SER stack by {d}");
    }
}

#[test]
fn stack_of_fits_frames_keeps_the_time_and_drops_the_site() {
    let dir = scratch();
    let subs = dir.path().join("zq_fits");
    write_fits_subs(&subs, &synthetic_frames(), &[]);
    let out = dir.path().join("out.fits");
    stack_args(&subs, &out).assert().success();
    let (_, header) = fits::read_fits_header(std::io::BufReader::new(
        std::fs::File::open(&out).expect("open"),
    ))
    .expect("fits");
    assert!(
        header
            .get("DATE-OBS")
            .is_some_and(|d| d.starts_with("2026-03-21T10:00:")),
        "the stack should carry the frame time: {header:?}"
    );
    for key in ["SITELAT", "SITELONG"] {
        assert!(
            !header.contains_key(key),
            "{key} was copied into the output"
        );
    }
}

#[test]
fn info_describes_fits_frames_and_names_what_it_will_use() {
    let dir = scratch();
    let subs = dir.path().join("zq_fits");
    write_fits_subs(
        &subs,
        &synthetic_frames()[..4],
        &[fits::HeaderCard::string("BAYERPAT", "GRBG", "")],
    );
    helia()
        .arg("info")
        .arg(&subs)
        .assert()
        .success()
        .stdout(predicate::str::contains("format:"))
        .stdout(predicate::str::contains("FITS"))
        .stdout(predicate::str::is_match(r"Frames:\s+4").expect("regex"))
        .stdout(predicate::str::is_match(r"BAYERPAT:\s+GRBG").expect("regex"))
        .stdout(predicate::str::is_match(r"Frame order:\s+by DATE-OBS").expect("regex"))
        // 112 rows, bottom-up: the stored GRBG reads as BGGR.
        .stdout(predicate::str::is_match(r"Bayer pattern:\s+BGGR").expect("regex"))
        .stdout(predicate::str::contains("2026-03-21T10:00:00"));
}

#[test]
fn stack_of_bayer_fits_subs_matches_the_same_frames_in_a_bayer_ser() {
    use helia_core::frame::BayerPattern;
    use helia_core::io::ser::{ColorId, SerHeader};
    use helia_core::testutil::{mosaic_disc, ser_bytes_with};
    let dir = scratch();
    let frames: Vec<_> = (0..12)
        .map(|i| mosaic_disc(96, 80, Some(BayerPattern::Rggb), [0.75, 1.0, 0.55], 11 + i))
        .collect();
    let ser = dir.path().join("sun.ser");
    let hdr = SerHeader::new(96, 80, ColorId::Bayer(BayerPattern::Rggb), 16);
    std::fs::write(&ser, ser_bytes_with(&frames, hdr)).expect("write ser");
    // The writer puts the top row last, as FITS does, so the stored pattern of an RGGB
    // picture with an even height is GBRG.
    let subs = dir.path().join("zq_fits");
    write_fits_subs(
        &subs,
        &frames,
        &[fits::HeaderCard::string("BAYERPAT", "GBRG", "")],
    );
    let (a, b) = (dir.path().join("ser.fits"), dir.path().join("fits.fits"));
    stack_args(&ser, &a).assert().success();
    stack_args(&subs, &b).assert().success();
    let d = max_difference(&a, &b);
    assert!(
        d < 2e-4,
        "the Bayer FITS stack differs from the SER stack by {d}"
    );
}

#[test]
fn stack_refuses_fits_frames_of_different_sizes_in_plain_words() {
    let dir = scratch();
    let subs = dir.path().join("zq_fits");
    write_fits_subs(&subs, &synthetic_frames()[..3], &[]);
    let odd = helia_core::Frame {
        width: 64,
        height: 50,
        data: vec![0.5; 64 * 50],
        bayer: None,
    };
    let mut v = Vec::new();
    fits::write_fits(&mut v, &odd, fits::FitsFormat::Int16).expect("encode");
    std::fs::write(subs.join("zq_999.fit"), v).expect("write");
    helia()
        .arg("stack")
        .arg(&subs)
        .arg("-o")
        .arg(dir.path().join("o.tif"))
        .assert()
        .failure()
        .stderr(predicate::str::contains("not all the same size"))
        .stderr(predicate::str::contains("zq_999.fit"));
}

#[test]
fn stack_survives_a_damaged_fits_sub_with_a_message() {
    let dir = scratch();
    let subs = dir.path().join("zq_fits");
    write_fits_subs(&subs, &synthetic_frames()[..3], &[]);
    let good = std::fs::read(subs.join("zq_000.fit")).expect("read");
    std::fs::write(subs.join("zq_001.fit"), &good[..2000]).expect("cut");
    std::fs::write(subs.join("zq_002.fit"), vec![b'x'; 9000]).expect("junk");
    helia()
        .arg("stack")
        .arg(&subs)
        .arg("-o")
        .arg(dir.path().join("o.tif"))
        .assert()
        .failure()
        .stderr(predicate::str::contains("not a FITS file Helia can read"));
    // An empty folder says so.
    let empty = dir.path().join("zq_empty");
    std::fs::create_dir(&empty).expect("mkdir");
    helia()
        .arg("stack")
        .arg(&empty)
        .arg("-o")
        .arg(dir.path().join("o.tif"))
        .assert()
        .failure()
        .stderr(predicate::str::contains("no FITS frames"));
}

#[test]
fn stack_settings_line_shows_the_values_it_uses_not_the_preset() {
    // issue #269: the line printed the preset's 12 % when --keep 50 % was given.
    let dir = scratch();
    let ser = capture(dir.path());
    let run = |extra: &[&str]| {
        let out = helia()
            .arg("stack")
            .arg(&ser)
            .arg("-o")
            .arg(dir.path().join("o.tif"))
            .args(["--preset", "white"])
            .args(extra)
            .output();
        let out = finished(out);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stderr)
            .lines()
            .find(|l| l.starts_with("settings:"))
            .expect("a settings line")
            .to_string()
    };
    let preset = run(&["--ap-size", "40"]);
    assert!(preset.contains("keep 12%"), "{preset}");
    assert!(preset.contains("alignment points 40 px"), "{preset}");
    let given = run(&["--keep", "50%", "--ap-size", "32"]);
    assert!(
        given.contains("keep 50%") && !given.contains("keep 12%"),
        "{given}"
    );
    assert!(given.contains("alignment points 32 px"), "{given}");
    let frames = run(&["--keep-frames", "4"]);
    assert!(frames.contains("keep 4 frames"), "{frames}");
}

/// A small bright disc on black, as a planet looks: not the Sun.
fn planet_capture(dir: &Path) -> PathBuf {
    let frames: Vec<helia_core::Frame> = (0..12)
        .map(|k| {
            let (w, h) = (96usize, 80usize);
            let mut data = vec![0.02f32; w * h];
            for y in 0..h {
                for x in 0..w {
                    let r2 = (x as f32 - 48.0 - 0.1 * k as f32).powi(2) + (y as f32 - 40.0).powi(2);
                    data[y * w + x] += 0.7 * (-r2 / 50.0).exp();
                }
            }
            helia_core::Frame::new(w, h, data).expect("frame")
        })
        .collect();
    let path = dir.join("planet.ser");
    std::fs::write(&path, ser_bytes(&frames)).expect("write ser");
    path
}

#[test]
fn a_capture_that_is_not_the_sun_is_stacked_with_a_plain_warning() {
    let dir = tempfile::tempdir().expect("tempdir");
    let planet = planet_capture(dir.path());
    let out = dir.path().join("planet.tif");
    stack_args(&planet, &out)
        .assert()
        .success()
        .stderr(predicate::str::contains(
            "warning: this capture does not look like the Sun",
        ))
        .stderr(predicate::str::contains("What to do:"));
    assert!(out.exists(), "the stack is made anyway");
    // The Sun itself, and a named filter, stay quiet.
    let sun = capture(dir.path());
    stack_args(&sun, &dir.path().join("sun.tif"))
        .assert()
        .success()
        .stderr(predicate::str::contains("does not look like the Sun").not());
    stack_args(&planet, &dir.path().join("p2.tif"))
        .args(["--preset", "white"])
        .assert()
        .success()
        .stderr(predicate::str::contains("does not look like the Sun").not());
}
