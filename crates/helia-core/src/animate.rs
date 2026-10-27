// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Moving pictures from a sequence of frames: an MP4 through `ffmpeg` when the
//! program is there, otherwise an animated PNG written here with no help.

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::error::{Error, Result};
use crate::io::any::Loaded;
use crate::io::ffmpeg::Tools;
use crate::picture::{self, Picture};

/// What kind of movie to make.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MovieChoice {
    /// MP4 when `ffmpeg` is installed, else an animated PNG.
    #[default]
    Auto,
    /// MP4 only; an error without `ffmpeg`.
    Mp4,
    /// Animated PNG, which needs nothing else.
    Apng,
    /// No movie.
    None,
}

impl std::str::FromStr for MovieChoice {
    type Err = Error;
    fn from_str(s: &str) -> Result<Self> {
        match s.to_ascii_lowercase().as_str() {
            "auto" => Ok(Self::Auto),
            "mp4" => Ok(Self::Mp4),
            "apng" | "png" => Ok(Self::Apng),
            "none" | "off" => Ok(Self::None),
            other => Err(Error::Invalid(format!(
                "unknown movie kind {other:?}; use auto, mp4, apng or none"
            ))),
        }
    }
}

/// What was written.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Movie {
    /// The file.
    pub path: PathBuf,
    /// `"mp4"` or `"apng"`.
    pub kind: &'static str,
    /// Why this kind was used, when it was not what the caller preferred.
    pub note: Option<String>,
}

/// Frame size and the colour type the frames share, checked.
fn shape(frames: &[Picture]) -> Result<(usize, usize, bool)> {
    let first = frames
        .first()
        .ok_or_else(|| Error::Invalid("a movie needs at least one frame".into()))?;
    let (w, h) = first.size();
    if frames.iter().any(|f| f.size() != (w, h)) {
        return Err(Error::Invalid("the frames differ in size".into()));
    }
    let colour = frames.iter().any(|f| matches!(f, Loaded::Rgb(_)));
    Ok((w, h, colour))
}

fn png_err(e: impl std::fmt::Display) -> Error {
    Error::Invalid(format!("PNG: {e}"))
}

/// Delay of one frame as a fraction of a second for the PNG chunk.
fn delay(fps: f32) -> (u16, u16) {
    // Hundredths keep the fraction exact for the usual rates.
    let hundredths = (100.0 / fps.max(0.01)).round().clamp(1.0, 65535.0) as u16;
    (hundredths, 100)
}

/// Write `frames` as an animated PNG (APNG) that plays forever at `fps` frames
/// a second.
///
/// # Errors
/// No frames, frames of unequal size, or I/O and encoder failures.
pub fn write_apng(path: &Path, frames: &[Picture], fps: f32) -> Result<()> {
    let (w, h, colour) = shape(frames)?;
    let to_u32 = |v: usize| u32::try_from(v).map_err(|_| Error::Invalid("image too large".into()));
    let n = u32::try_from(frames.len()).map_err(|_| Error::Invalid("too many frames".into()))?;
    let file = BufWriter::new(File::create(path)?);
    let mut enc = ::png::Encoder::new(file, to_u32(w)?, to_u32(h)?);
    enc.set_color(if colour {
        ::png::ColorType::Rgb
    } else {
        ::png::ColorType::Grayscale
    });
    enc.set_depth(::png::BitDepth::Eight);
    enc.set_animated(n, 0).map_err(png_err)?;
    enc.add_text_chunk("Software".into(), "Helia".into())
        .map_err(png_err)?;
    let mut writer = enc.write_header().map_err(png_err)?;
    let (num, den) = delay(fps);
    for f in frames {
        writer.set_frame_delay(num, den).map_err(png_err)?;
        let bytes = if colour {
            picture::to_rgb8(f)
        } else {
            picture::to_rgb8(f)
                .as_chunks::<3>()
                .0
                .iter()
                .map(|p| p[0])
                .collect()
        };
        writer.write_image_data(&bytes).map_err(png_err)?;
    }
    writer.finish().map_err(png_err)?;
    Ok(())
}

/// The `ffmpeg` arguments that read raw RGB frames from the standard input.
#[must_use]
pub fn mp4_args(
    w: usize,
    h: usize,
    fps: f32,
    codec: &[&str],
    output: &Path,
) -> Vec<std::ffi::OsString> {
    let mut a: Vec<std::ffi::OsString> = [
        "-y",
        "-loglevel",
        "error",
        "-f",
        "rawvideo",
        "-pix_fmt",
        "rgb24",
        "-s",
    ]
    .map(Into::into)
    .to_vec();
    a.push(format!("{w}x{h}").into());
    a.extend([
        "-r".into(),
        format!("{fps}").into(),
        "-i".into(),
        "pipe:0".into(),
    ]);
    a.extend(codec.iter().map(|&s| s.into()));
    a.extend(["-pix_fmt".into(), "yuv420p".into()]);
    // The `file:` prefix keeps a name that looks like a URL or an option a file name.
    let mut target = std::ffi::OsString::from("file:");
    target.push(output);
    a.push(target);
    a
}

/// Encode with one codec; `Err` carries what `ffmpeg` said.
fn run_ffmpeg(
    tools: &Tools,
    frames: &[Picture],
    fps: f32,
    codec: &[&str],
    out: &Path,
) -> Result<()> {
    // yuv420p needs even sides: the last row and column are dropped when odd.
    let (w, h, _) = shape(frames)?;
    let (ew, eh) = (w & !1, h & !1);
    let mut child = Command::new(&tools.ffmpeg)
        .args(mp4_args(ew, eh, fps, codec, out))
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| Error::Invalid(format!("cannot start ffmpeg: {e}")))?;
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| Error::Invalid("ffmpeg has no input".into()))?;
    let mut failed = None;
    for f in frames {
        let rgb = picture::to_rgb8(f);
        let mut row_bytes = Vec::with_capacity(ew * eh * 3);
        for y in 0..eh {
            row_bytes.extend_from_slice(&rgb[y * w * 3..(y * w + ew) * 3]);
        }
        if let Err(e) = stdin.write_all(&row_bytes) {
            failed = Some(e);
            break;
        }
    }
    drop(stdin);
    let output = child
        .wait_with_output()
        .map_err(|e| Error::Invalid(format!("ffmpeg did not finish: {e}")))?;
    if !output.status.success() || failed.is_some() {
        let said = String::from_utf8_lossy(&output.stderr);
        let line = said.lines().last().unwrap_or("it gave no reason");
        return Err(Error::Invalid(format!("ffmpeg failed: {line}")));
    }
    Ok(())
}

/// Write `frames` as an MP4 (H.264, falling back to MPEG-4 part 2 when the
/// `ffmpeg` build has no H.264 encoder).
///
/// # Errors
/// No frames, frames of unequal size, or `ffmpeg` failing.
pub fn write_mp4(tools: &Tools, path: &Path, frames: &[Picture], fps: f32) -> Result<()> {
    let h264 = ["-c:v", "libx264", "-crf", "17", "-preset", "medium"];
    let mpeg4 = ["-c:v", "mpeg4", "-q:v", "2"];
    run_ffmpeg(tools, frames, fps, &h264, path)
        .or_else(|_| run_ffmpeg(tools, frames, fps, &mpeg4, path))
}

/// Make the movie `choice` asks for in `dir` under the name `stem`
/// (`stem.mp4` or `stem.png`). `Ok(None)` for [`MovieChoice::None`] and for a
/// single frame, which has nothing to animate.
///
/// # Errors
/// [`MovieChoice::Mp4`] without a working `ffmpeg`, or I/O failures.
pub fn make_movie(
    dir: &Path,
    stem: &str,
    frames: &[Picture],
    fps: f32,
    choice: MovieChoice,
) -> Result<Option<Movie>> {
    if choice == MovieChoice::None || frames.len() < 2 {
        return Ok(None);
    }
    let mp4 = dir.join(format!("{stem}.mp4"));
    let apng = dir.join(format!("{stem}.png"));
    let try_mp4 = |why_not: &mut Option<String>| -> Option<Movie> {
        match Tools::locate() {
            Ok(t) => match write_mp4(&t, &mp4, frames, fps) {
                Ok(()) => Some(Movie {
                    path: mp4.clone(),
                    kind: "mp4",
                    note: None,
                }),
                Err(e) => {
                    *why_not = Some(e.to_string());
                    None
                }
            },
            Err(e) => {
                *why_not = Some(e.to_string());
                None
            }
        }
    };
    let mut why = None;
    match choice {
        MovieChoice::Apng => {}
        MovieChoice::Mp4 => {
            return try_mp4(&mut why).map(Some).ok_or_else(|| {
                Error::Invalid(why.unwrap_or_else(|| "ffmpeg is not available".into()))
            });
        }
        _ => {
            if let Some(m) = try_mp4(&mut why) {
                return Ok(Some(m));
            }
        }
    }
    write_apng(&apng, frames, fps)?;
    Ok(Some(Movie {
        path: apng,
        kind: "apng",
        note: why.map(|w| {
            let first = w.split_once(". ").map_or(w.as_str(), |(a, _)| a);
            format!("no MP4 ({first}); wrote an animated PNG instead")
        }),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::Frame;

    fn frames(n: usize, w: usize, h: usize) -> Vec<Picture> {
        (0..n)
            .map(|i| {
                Picture::Mono(Frame {
                    width: w,
                    height: h,
                    data: (0..w * h)
                        .map(|p| ((p + i * 7) % 50) as f32 / 50.0)
                        .collect(),
                    bayer: None,
                })
            })
            .collect()
    }

    #[test]
    fn apng_has_every_frame_and_the_delay() {
        let dir = std::env::temp_dir().join(format!("helia-apng-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap_or_else(|e| panic!("{e}"));
        let path = dir.join("a.png");
        write_apng(&path, &frames(4, 9, 6), 5.0).unwrap_or_else(|e| panic!("{e}"));
        let dec = ::png::Decoder::new(std::io::BufReader::new(
            File::open(&path).unwrap_or_else(|e| panic!("{e}")),
        ));
        let mut r = dec.read_info().unwrap_or_else(|e| panic!("{e}"));
        let ctl = r.info().animation_control().copied();
        assert_eq!(ctl.map(|c| c.num_frames), Some(4));
        let mut buf = vec![0u8; r.output_buffer_size().unwrap_or(0)];
        let mut seen = 0;
        while r.next_frame(&mut buf).is_ok() {
            let fc = r.info().frame_control().copied();
            assert_eq!(fc.map(|f| (f.delay_num, f.delay_den)), Some((20, 100)));
            seen += 1;
            if seen == 4 {
                break;
            }
        }
        assert_eq!(seen, 4);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn uneven_frames_are_refused() {
        let mut f = frames(2, 8, 8);
        f.push(frames(1, 9, 8).remove(0));
        let p = std::env::temp_dir().join("helia-uneven.png");
        assert!(write_apng(&p, &f, 5.0).is_err());
    }

    #[test]
    fn mp4_arguments_read_raw_rgb_from_stdin() {
        let a = mp4_args(640, 480, 10.0, &["-c:v", "libx264"], Path::new("o.mp4"));
        let s: Vec<String> = a.iter().map(|x| x.to_string_lossy().into_owned()).collect();
        assert!(s.windows(2).any(|w| w == ["-s", "640x480"]));
        assert!(s.windows(2).any(|w| w == ["-i", "pipe:0"]));
        assert_eq!(s.last().map(String::as_str), Some("file:o.mp4"));
        let odd = mp4_args(2, 2, 1.0, &[], Path::new("-rtmp://x/y.mp4"));
        assert_eq!(
            odd.last()
                .map(|x| x.to_string_lossy().into_owned())
                .as_deref(),
            Some("file:-rtmp://x/y.mp4")
        );
    }

    #[test]
    fn a_single_frame_makes_no_movie() {
        let m = make_movie(
            &std::env::temp_dir(),
            "x",
            &frames(1, 8, 8),
            10.0,
            MovieChoice::Auto,
        )
        .unwrap_or_else(|e| panic!("{e}"));
        assert!(m.is_none());
    }
}
