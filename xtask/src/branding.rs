// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! `cargo xtask branding`: rebuild every image derived from the vector logo.
//!
//! The only input is `assets/branding/helia-logo.svg`, which is never modified.
//! Outputs (all covered by `LOGO-LICENCE.md`, not by the GPL):
//!
//! * `helia-logo-transparent.png`: the whole logo without its background.
//! * `icons/helia-<size>.png`: square transparent icons (Sun, frame stack and
//!   rings, no wordmark). Sizes up to `SIMPLE_MAX` use a simplified drawing from
//!   the same SVG (Sun, rings, one frame) because the stack turns to mush there.
//! * `helia.ico` (Windows) and `helia.icns` (macOS), both PNG-in-container.
//! * optionally a contact sheet to check the sizes by eye.

use std::fs;
use std::path::Path;

use anyhow::{Context, Result, anyhow};
use image::imageops::{self, FilterType};
use image::{Rgba, RgbaImage};
use resvg::{tiny_skia, usvg};

/// Icon sizes written as PNG files.
pub const SIZES: [u32; 9] = [16, 24, 32, 48, 64, 128, 256, 512, 1024];
/// Sizes in the Windows icon.
const ICO_SIZES: [u32; 7] = [16, 24, 32, 48, 64, 128, 256];
/// Sizes in the macOS icon with their `OSType`.
const ICNS_SIZES: [(&[u8; 4], u32); 4] = [
    (b"ic07", 128),
    (b"ic08", 256),
    (b"ic09", 512),
    (b"ic10", 1024),
];
/// Largest size that uses the simplified drawing.
const SIMPLE_MAX: u32 = 48;

/// Crop in SVG user units: left, top, side. Full mark, no wordmark (starts at y 915).
const FULL: (f32, f32, f32) = (100.0, -60.0, 960.0);
/// Crop for the simplified icon: the Sun, its rings and the front frame.
const SIMPLE: (f32, f32, f32) = (230.0, 20.0, 800.0);

/// The opaque background rectangle.
const BACKGROUND: &str = r##"<rect width="1200" height="1200" fill="#050608"/>"##;
/// Lines dropped from the simplified icon: the five back frames and the thin ring.
const SIMPLE_DROP: [&str; 6] = [
    r##"fill="#171a20""##,
    r##"fill="#1a1e24""##,
    r##"fill="#242a31""##,
    r##"fill="#161a20""##,
    r##"fill="#0b0e12""##,
    r##"stroke="#4c535d""##,
];

fn without_background(svg: &str) -> String {
    svg.replace(BACKGROUND, "")
}

fn simplified(svg: &str) -> String {
    without_background(svg)
        .lines()
        .filter(|l| !SIMPLE_DROP.iter().any(|d| l.contains(d)))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Render `svg` into a square of `size` pixels showing the `(left, top, side)` window.
fn render(svg: &str, (left, top, span): (f32, f32, f32), size: u32) -> Result<RgbaImage> {
    let tree = usvg::Tree::from_str(svg, &usvg::Options::default())
        .map_err(|e| anyhow!("cannot parse the logo SVG: {e}"))?;
    let mut pixmap = tiny_skia::Pixmap::new(size, size).ok_or_else(|| anyhow!("empty pixmap"))?;
    let scale = size as f32 / span;
    let ts = tiny_skia::Transform::from_scale(scale, scale).pre_translate(-left, -top);
    resvg::render(&tree, ts, &mut pixmap.as_mut());
    let mut out = RgbaImage::new(size, size);
    for (o, p) in out.pixels_mut().zip(pixmap.pixels()) {
        let c = p.demultiply();
        *o = Rgba([c.red(), c.green(), c.blue(), c.alpha()]);
    }
    Ok(out)
}

fn icon(svg: &str, size: u32, simple: bool) -> Result<RgbaImage> {
    if simple {
        render(&simplified(svg), SIMPLE, size)
    } else {
        render(&without_background(svg), FULL, size)
    }
}

fn png_bytes(img: &RgbaImage) -> Result<Vec<u8>> {
    let mut buf = std::io::Cursor::new(Vec::new());
    img.write_to(&mut buf, image::ImageFormat::Png)?;
    Ok(buf.into_inner())
}

/// ICO container with one PNG per size (supported since Windows Vista).
fn ico(images: &[(u32, Vec<u8>)]) -> Vec<u8> {
    let mut out = vec![0, 0, 1, 0];
    out.extend_from_slice(&u16::try_from(images.len()).unwrap_or(0).to_le_bytes());
    let mut offset = 6 + 16 * images.len();
    for (size, png) in images {
        let dim = if *size >= 256 { 0 } else { *size as u8 };
        out.extend_from_slice(&[dim, dim, 0, 0, 1, 0, 32, 0]);
        out.extend_from_slice(&u32::try_from(png.len()).unwrap_or(0).to_le_bytes());
        out.extend_from_slice(&u32::try_from(offset).unwrap_or(0).to_le_bytes());
        offset += png.len();
    }
    for (_, png) in images {
        out.extend_from_slice(png);
    }
    out
}

/// ICNS container with PNG payloads (`ic07` to `ic10`).
fn icns(images: &[(&[u8; 4], Vec<u8>)]) -> Vec<u8> {
    let total = 8 + images.iter().map(|(_, p)| 8 + p.len()).sum::<usize>();
    let mut out = b"icns".to_vec();
    out.extend_from_slice(&u32::try_from(total).unwrap_or(0).to_be_bytes());
    for (ty, png) in images {
        out.extend_from_slice(*ty);
        out.extend_from_slice(&u32::try_from(8 + png.len()).unwrap_or(0).to_be_bytes());
        out.extend_from_slice(png);
    }
    out
}

/// Alpha-composite `img` onto `sheet` at `(x0, y0)`.
fn put(sheet: &mut RgbaImage, img: &RgbaImage, x0: u32, y0: u32) {
    for (x, y, p) in img.enumerate_pixels() {
        let a = f32::from(p.0[3]) / 255.0;
        let dst = sheet.get_pixel_mut(x0 + x, y0 + y);
        for i in 0..3 {
            dst.0[i] = (f32::from(p.0[i]) * a + f32::from(dst.0[i]) * (1.0 - a)).round() as u8;
        }
    }
}

/// Contact sheet on a dark and a light half. Per half: the shipped sizes at 1x,
/// the small sizes enlarged 4x as shipped, then the same sizes with the other
/// drawing (full where simplified is shipped, and vice versa), then the 256 icon.
fn contact_sheet(svg: &str) -> Result<RgbaImage> {
    let pad = 16u32;
    let mut sheet = RgbaImage::new(1500, 1100);
    for (x, _, p) in sheet.enumerate_pixels_mut() {
        *p = if x < 750 {
            Rgba([32, 32, 36, 255])
        } else {
            Rgba([240, 240, 240, 255])
        };
    }
    for half in [0, 750u32] {
        let mut y = pad;
        let mut x = half + pad;
        for s in [16, 24, 32, 48, 64, 128, 256] {
            put(&mut sheet, &icon(svg, s, s <= SIMPLE_MAX)?, x, y);
            x += s + pad;
        }
        y += 256 + pad;
        for other in [false, true] {
            let mut x = half + pad;
            for s in [16, 24, 32, 48] {
                let big = imageops::resize(
                    &icon(svg, s, (s <= SIMPLE_MAX) != other)?,
                    s * 4,
                    s * 4,
                    FilterType::Nearest,
                );
                put(&mut sheet, &big, x, y);
                x += s * 4 + pad;
            }
            y += 192 + pad;
        }
        put(&mut sheet, &icon(svg, 256, false)?, half + pad, y);
    }
    Ok(sheet)
}

/// Write every derived asset under `dir`; optionally the contact sheet to `sheet`.
pub fn run(dir: &Path, sheet: Option<&Path>) -> Result<()> {
    let source = dir.join("helia-logo.svg");
    let svg = fs::read_to_string(&source)
        .with_context(|| format!("cannot read {}", source.display()))?
        .replace("\r\n", "\n");

    render(&without_background(&svg), (0.0, 0.0, 1200.0), 1200)?
        .save(dir.join("helia-logo-transparent.png"))?;

    let icons = dir.join("icons");
    fs::create_dir_all(&icons)?;
    let mut pngs = std::collections::BTreeMap::new();
    for size in SIZES {
        let bytes = png_bytes(&icon(&svg, size, size <= SIMPLE_MAX)?)?;
        fs::write(icons.join(format!("helia-{size}.png")), &bytes)?;
        pngs.insert(size, bytes);
    }
    let ico_images: Vec<(u32, Vec<u8>)> = ICO_SIZES.iter().map(|s| (*s, pngs[s].clone())).collect();
    fs::write(dir.join("helia.ico"), ico(&ico_images))?;
    let icns_images: Vec<(&[u8; 4], Vec<u8>)> = ICNS_SIZES
        .iter()
        .map(|(t, s)| (*t, pngs[s].clone()))
        .collect();
    fs::write(dir.join("helia.icns"), icns(&icns_images))?;

    // Copies the book needs inside its own source tree.
    let root = dir
        .parent()
        .and_then(Path::parent)
        .context("bad branding dir")?;
    fs::write(root.join("docs/book/src/images/favicon.png"), &pngs[&48])?;
    fs::copy(
        dir.join("helia-logo.png"),
        root.join("docs/book/src/images/helia-logo.png"),
    )?;

    if let Some(path) = sheet {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        contact_sheet(&svg)?.save(path)?;
    }
    Ok(())
}
