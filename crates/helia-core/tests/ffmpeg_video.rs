// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Compressed video through the optional `ffmpeg`, on synthetic suns.
//!
//! Each test makes a small MP4, MOV, MKV, M4V or AVI with `ffmpeg` from the
//! synthetic sun generator, then opens it with Helia. Without `ffmpeg` on the
//! `PATH` (or in `HELIA_FFMPEG`) every test prints a note and passes; set
//! `HELIA_REQUIRE_FFMPEG=1` (as CI does after installing it) to make a missing
//! `ffmpeg` fail instead.
#![cfg(feature = "testutil")]

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use helia_core::Frame;
use helia_core::io::ffmpeg::{self, FfmpegError, Tools, VideoOptions};
use helia_core::io::source::{FrameSource, OpenOptions, open_capture, open_capture_with};
use helia_core::pipeline::{Pipeline, StackConfig};
use helia_core::resample::shifted;
use helia_core::testutil::rmse;
use helia_core::testutil::{CaptureConfig, SyntheticCapture, SyntheticConfig, SyntheticSun};
use helia_core::{Error, Keep, Kernel, Metric, filter, quality};

const FRAMES: usize = 40;
const WIDTH: usize = 192;
const HEIGHT: usize = 144;

fn tools() -> Option<Tools> {
    match Tools::locate() {
        Ok(t) => Some(t),
        Err(e) => {
            assert!(
                std::env::var("HELIA_REQUIRE_FFMPEG").as_deref() != Ok("1"),
                "HELIA_REQUIRE_FFMPEG=1 but ffmpeg is not usable: {e}"
            );
            eprintln!("ffmpeg not available, skipping: {e}");
            None
        }
    }
}

/// A scratch directory removed on drop.
struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("helia-ffmpeg-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch dir");
        Self(dir)
    }
    fn file(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn capture() -> SyntheticCapture {
    let sun = SyntheticSun::new(&SyntheticConfig {
        width: WIDTH,
        height: HEIGHT,
        ..SyntheticConfig::default()
    });
    SyntheticCapture::generate(
        &sun,
        &CaptureConfig {
            frames: FRAMES,
            noise: 0.02,
            ..CaptureConfig::default()
        },
    )
}

/// The 8-bit values the encoder is given.
fn quantise(f: &Frame) -> Vec<u8> {
    f.data
        .iter()
        .map(|v| (v.clamp(0.0, 1.0) * 255.0).round() as u8)
        .collect()
}

/// Encode `frames` to `out` with the given `ffmpeg` output arguments. `None`
/// when this `ffmpeg` lacks the encoder (a note is printed).
fn encode(tools: &Tools, frames: &[Frame], out: &Path, args: &[&str]) -> Option<()> {
    let mut child = Command::new(&tools.ffmpeg)
        .args(["-y", "-v", "error", "-f", "rawvideo", "-pix_fmt", "gray"])
        .args(["-s", &format!("{WIDTH}x{HEIGHT}"), "-r", "25", "-i", "-"])
        .args(args)
        .arg(out)
        .stdin(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn ffmpeg");
    let mut stdin = child.stdin.take().expect("stdin");
    let bytes: Vec<Vec<u8>> = frames.iter().map(quantise).collect();
    let writer = std::thread::spawn(move || {
        for b in bytes {
            if stdin.write_all(&b).is_err() {
                break;
            }
        }
    });
    let output = child.wait_with_output().expect("wait");
    let _ = writer.join();
    if output.status.success() {
        Some(())
    } else {
        eprintln!(
            "skipping {}: this ffmpeg cannot encode it: {}",
            out.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        );
        None
    }
}

const X264: &[&str] = &[
    "-c:v",
    "libx264",
    "-preset",
    "ultrafast",
    "-crf",
    "12",
    "-pix_fmt",
    "yuv420p",
];

fn mean_abs_error(a: &Frame, expect: &[u8]) -> f32 {
    let sum: f32 = a
        .data
        .iter()
        .zip(expect)
        .map(|(v, &e)| (v - f32::from(e) / 255.0).abs())
        .sum();
    sum / expect.len() as f32
}

#[test]
fn each_container_opens_with_the_right_shape() {
    let Some(tools) = tools() else { return };
    let cap = capture();
    let dir = Scratch::new("containers");
    let cache = Scratch::new("containers-cache");
    // (file name, ffmpeg arguments, lossy)
    let cases: [(&str, &[&str], bool); 5] = [
        ("sun.mp4", X264, true),
        ("sun.mov", X264, true),
        ("sun.m4v", X264, true),
        ("sun.mkv", &["-c:v", "ffv1", "-pix_fmt", "gray"], false),
        (
            "sun.avi",
            &["-c:v", "mjpeg", "-q:v", "2", "-pix_fmt", "yuvj420p"],
            true,
        ),
    ];
    for (name, args, lossy) in cases {
        let path = dir.file(name);
        if encode(&tools, &cap.frames, &path, args).is_none() {
            continue;
        }
        let video = VideoOptions {
            cache_dir: Some(cache.0.clone()),
            ..VideoOptions::default()
        };
        let mut src = open_capture_with(&path, &OpenOptions::default(), &video)
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(src.frame_count(), FRAMES, "{name}: frame count");
        assert_eq!(src.dimensions(), (WIDTH, HEIGHT), "{name}: size");
        assert!(src.describe().contains("decoded with ffmpeg"), "{name}");
        assert_eq!(
            src.quality_note().is_some(),
            lossy,
            "{name}: lossy note {:?}",
            src.quality_note()
        );
        if lossy {
            let note = src.quality_note().expect("note");
            assert!(note.contains("8-bit") && note.contains("raw captures keep more detail"));
        }
        let err = mean_abs_error(&src.read(7).expect("read"), &quantise(&cap.frames[7]));
        if lossy {
            assert!(err < 0.03, "{name}: mean error {err}");
        } else {
            assert_eq!(err, 0.0, "{name}: lossless video must decode exactly");
        }
        // Random access in any order, and the concurrent decoder used by the GPU.
        for i in [FRAMES - 1, 0, 19] {
            src.read(i)
                .unwrap_or_else(|e| panic!("{name} frame {i}: {e}"));
        }
        assert!(src.read(FRAMES).is_err());
        assert!(src.shared().is_some(), "{name}: shared decoder");
    }
}

#[test]
fn a_stack_beats_the_best_single_frame() {
    let Some(tools) = tools() else { return };
    let sun = SyntheticSun::new(&SyntheticConfig {
        width: WIDTH,
        height: HEIGHT,
        ..SyntheticConfig::default()
    });
    let cap = SyntheticCapture::generate(
        &sun,
        &CaptureConfig {
            frames: FRAMES,
            noise: 0.03,
            ..CaptureConfig::default()
        },
    );
    let dir = Scratch::new("stack");
    let cache = Scratch::new("stack-cache");
    let path = dir.file("sun.mp4");
    if encode(&tools, &cap.frames, &path, X264).is_none() {
        return;
    }
    let video = VideoOptions {
        cache_dir: Some(cache.0.clone()),
        ..VideoOptions::default()
    };
    let mut src = open_capture_with(&path, &OpenOptions::default(), &video).expect("open");
    let decoded: Vec<Frame> = (0..FRAMES).map(|i| src.read(i).expect("read")).collect();
    let out = Pipeline::new(StackConfig {
        keep: Keep::Fraction(0.15),
        normalise: false,
        multipoint: None,
        ..StackConfig::default()
    })
    .run(&mut src, &|_, _| {}, &AtomicBool::new(false))
    .expect("stack");
    // Against the known sun, with the best single frame perfectly registered so
    // that position cannot count against it.
    let best = cap.sharpest();
    let truth = sun.render_clean(cap.shifts[best], None);
    let stack_err = rmse(&out.image, &truth, 14);
    let aligned: Vec<Frame> = decoded
        .iter()
        .zip(&cap.shifts)
        .map(|(f, s)| shifted(f, *s - cap.shifts[best], Kernel::Lanczos3))
        .collect();
    let best_single_err = aligned
        .iter()
        .map(|f| rmse(f, &truth, 14))
        .fold(f32::INFINITY, f32::min);
    // Light blur first, so noise (which stacking removes) does not read as detail.
    let detail =
        |f: &Frame| quality::score(&filter::gaussian_blur(f, 1.5), Metric::GradientEnergy, None);
    let best_single_detail = aligned.iter().map(detail).fold(0.0, f32::max);
    eprintln!(
        "video stack: rmse {stack_err:.5} vs best single {best_single_err:.5}; detail {:.5} vs {best_single_detail:.5}",
        detail(&out.image)
    );
    assert!(
        stack_err < 0.85 * best_single_err,
        "stack {stack_err} vs best single {best_single_err}"
    );
    assert!(detail(&out.image) > best_single_detail);
}

#[test]
fn a_colour_channel_is_extracted_by_ffmpeg() {
    let Some(tools) = tools() else { return };
    let cap = capture();
    let dir = Scratch::new("channel");
    let cache = Scratch::new("channel-cache");
    let path = dir.file("sun.mkv");
    // Lossless gray, so every colour plane of the RGB view is the same picture.
    if encode(
        &tools,
        &cap.frames,
        &path,
        &["-c:v", "ffv1", "-pix_fmt", "gray"],
    )
    .is_none()
    {
        return;
    }
    for channel in [
        helia_core::debayer::Channel::Luminance,
        helia_core::debayer::Channel::Red,
        helia_core::debayer::Channel::Green,
        helia_core::debayer::Channel::Blue,
    ] {
        let video = VideoOptions {
            cache_dir: Some(cache.0.clone()),
            ..VideoOptions::default()
        };
        let opts = OpenOptions {
            channel,
            ..OpenOptions::default()
        };
        let mut src = open_capture_with(&path, &opts, &video).expect("open");
        assert_eq!(src.dimensions(), (WIDTH, HEIGHT), "{channel:?}");
        let err = mean_abs_error(&src.read(3).expect("read"), &quantise(&cap.frames[3]));
        assert!(err < 0.01, "{channel:?}: mean error {err}");
    }
}

#[test]
fn rotation_metadata_is_applied() {
    let Some(tools) = tools() else { return };
    let cap = capture();
    let dir = Scratch::new("rotation");
    let cache = Scratch::new("rotation-cache");
    let plain = dir.file("plain.mp4");
    if encode(&tools, &cap.frames, &plain, X264).is_none() {
        return;
    }
    let rotated = dir.file("rotated.mp4");
    let remux = Command::new(&tools.ffmpeg)
        .args(["-y", "-v", "error", "-display_rotation", "90", "-i"])
        .arg(&plain)
        .args(["-c", "copy"])
        .arg(&rotated)
        .output()
        .expect("run ffmpeg");
    if !remux.status.success() {
        eprintln!("skipping: this ffmpeg has no -display_rotation");
        return;
    }
    let video = VideoOptions {
        cache_dir: Some(cache.0.clone()),
        ..VideoOptions::default()
    };
    let src = open_capture_with(&rotated, &OpenOptions::default(), &video).expect("open");
    assert_eq!(
        src.dimensions(),
        (HEIGHT, WIDTH),
        "a quarter turn swaps width and height"
    );
    let info = ffmpeg::inspect(&rotated).expect("inspect");
    assert_eq!(info.rotation % 180, 90);
}

#[test]
fn inspect_reports_codec_count_and_rate_without_decoding() {
    let Some(tools) = tools() else { return };
    let cap = capture();
    let dir = Scratch::new("inspect");
    let path = dir.file("sun.mp4");
    if encode(&tools, &cap.frames, &path, X264).is_none() {
        return;
    }
    let i = ffmpeg::inspect(&path).expect("inspect");
    assert_eq!(i.codec, "h264");
    assert_eq!((i.width, i.height), (WIDTH, HEIGHT));
    assert_eq!(i.frames, FRAMES as u64);
    assert!(i.frames_exact);
    assert_eq!(i.frame_rate, Some(25.0));
    assert_eq!(i.chroma(), Some("4:2:0"));
}

#[test]
fn a_cut_file_is_reported_as_truncated() {
    let Some(tools) = tools() else { return };
    let cap = capture();
    let dir = Scratch::new("cut");
    let whole = dir.file("whole.mp4");
    if encode(&tools, &cap.frames, &whole, X264).is_none() {
        return;
    }
    let bytes = std::fs::read(&whole).expect("read");
    let cut = dir.file("cut.mp4");
    std::fs::write(&cut, &bytes[..bytes.len() / 2]).expect("write");
    let Err(err) = open_capture(&cut, &OpenOptions::default()) else {
        panic!("a half file must not open")
    };
    assert!(
        matches!(
            err,
            Error::Ffmpeg(FfmpegError::Truncated(_) | FfmpegError::Probe(_))
        ),
        "{err}"
    );
    // A file that is not a video at all.
    let junk = dir.file("junk.mp4");
    std::fs::write(&junk, b"this is not a video").expect("write");
    assert!(open_capture(&junk, &OpenOptions::default()).is_err());
}

#[test]
fn cancelling_stops_ffmpeg_and_leaves_no_cache_file() {
    let Some(tools) = tools() else { return };
    let cap = capture();
    let dir = Scratch::new("cancel");
    let cache = Scratch::new("cancel-cache");
    let path = dir.file("sun.mp4");
    if encode(&tools, &cap.frames, &path, X264).is_none() {
        return;
    }
    let cancel = Arc::new(AtomicBool::new(true));
    let video = VideoOptions {
        cache_dir: Some(cache.0.clone()),
        cancel: Some(Arc::clone(&cancel)),
        ..VideoOptions::default()
    };
    let Err(err) = open_capture_with(&path, &OpenOptions::default(), &video) else {
        panic!("a cancelled open must fail")
    };
    assert!(matches!(err, Error::Cancelled), "{err}");
    assert!(cancel.load(Ordering::Relaxed));
    assert_eq!(
        std::fs::read_dir(&cache.0).expect("dir").count(),
        0,
        "a cancelled decode leaves nothing behind"
    );
}

#[test]
fn the_decoded_file_is_shared_while_open_and_removed_after() {
    let Some(tools) = tools() else { return };
    let cap = capture();
    let dir = Scratch::new("cache");
    let cache = Scratch::new("cache-dir");
    let path = dir.file("sun.mp4");
    if encode(&tools, &cap.frames, &path, X264).is_none() {
        return;
    }
    let video = VideoOptions {
        cache_dir: Some(cache.0.clone()),
        ..VideoOptions::default()
    };
    let files = || std::fs::read_dir(&cache.0).expect("dir").count();
    let first = open_capture_with(&path, &OpenOptions::default(), &video).expect("open");
    assert_eq!(files(), 1);
    // A second open of the same video reuses the decoded file.
    let second = open_capture_with(&path, &OpenOptions::default(), &video).expect("reopen");
    assert_eq!(files(), 1, "decoded once");
    let keep = first.keep_alive().expect("keep-alive handle");
    drop(first);
    drop(second);
    assert_eq!(files(), 1, "the handle keeps the file");
    drop(keep);
    assert_eq!(files(), 0, "the file goes with the last handle");
}

#[test]
fn an_unusable_cache_directory_is_an_error() {
    let Some(tools) = tools() else { return };
    let cap = capture();
    let dir = Scratch::new("space");
    let path = dir.file("sun.mp4");
    if encode(&tools, &cap.frames, &path, X264).is_none() {
        return;
    }
    // Ask for more than any disk has by lying about nothing: the check itself
    // is exercised by a cache directory that cannot be created.
    let blocker = dir.file("not-a-dir");
    std::fs::write(&blocker, b"x").expect("write");
    let video = VideoOptions {
        cache_dir: Some(blocker.join("cache")),
        ..VideoOptions::default()
    };
    assert!(open_capture_with(&path, &OpenOptions::default(), &video).is_err());
}

#[test]
fn single_frames_come_without_decoding_the_whole_video() {
    let Some(tools) = tools() else { return };
    let cap = capture();
    let dir = Scratch::new("frame");
    let path = dir.file("sun.mkv");
    if encode(
        &tools,
        &cap.frames,
        &path,
        &["-c:v", "ffv1", "-pix_fmt", "gray"],
    )
    .is_none()
    {
        return;
    }
    let f = ffmpeg::read_frame(&path, 11, helia_core::debayer::Channel::Luminance).expect("frame");
    assert_eq!((f.width, f.height), (WIDTH, HEIGHT));
    assert_eq!(mean_abs_error(&f, &quantise(&cap.frames[11])), 0.0);
    assert!(ffmpeg::read_frame(&path, FRAMES, helia_core::debayer::Channel::Luminance).is_err());
}
