// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! The Helia logo and icons, embedded in the executable.
//!
//! The images are not GPL: see `LOGO-LICENCE.md`. They are rebuilt from the
//! vector logo by `cargo xtask branding`.

use eframe::egui::{self, ColorImage, TextureHandle, TextureOptions};

/// Window and taskbar icon.
const WINDOW_ICON: &[u8] = include_bytes!("../../../assets/branding/icons/helia-256.png");
/// Small mark for the top bar (simplified drawing, see the xtask).
const SMALL_ICON: &[u8] = include_bytes!("../../../assets/branding/icons/helia-48.png");
/// The artwork shown on the About page.
const LOGO: &[u8] = include_bytes!("../../../assets/branding/helia-logo.png");

/// Which embedded image.
#[derive(Clone, Copy)]
pub enum Image {
    /// The small mark.
    Small,
    /// The full logo with the wordmark.
    Logo,
}

/// Decode an embedded PNG to straight RGBA8. The bytes are ours and fixed, so a
/// failure is a build error and not a runtime condition.
fn decode(bytes: &[u8]) -> Option<([usize; 2], Vec<u8>)> {
    let mut decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    decoder.set_transformations(png::Transformations::EXPAND);
    let mut reader = decoder.read_info().ok()?;
    let mut buf = vec![0; reader.output_buffer_size()?];
    let info = reader.next_frame(&mut buf).ok()?;
    let (w, h) = (info.width as usize, info.height as usize);
    let rgba = match info.color_type {
        png::ColorType::Rgba => buf[..info.buffer_size()].to_vec(),
        png::ColorType::Rgb => buf[..info.buffer_size()]
            .as_chunks::<3>()
            .0
            .iter()
            .flat_map(|p| [p[0], p[1], p[2], 255])
            .collect(),
        _ => return None,
    };
    Some(([w, h], rgba))
}

/// The window and taskbar icon.
pub fn window_icon() -> Option<egui::IconData> {
    let ([width, height], rgba) = decode(WINDOW_ICON)?;
    Some(egui::IconData {
        rgba,
        width: u32::try_from(width).ok()?,
        height: u32::try_from(height).ok()?,
    })
}

/// A texture of an embedded image, uploaded once per context.
pub fn texture(ctx: &egui::Context, which: Image) -> Option<TextureHandle> {
    let (name, bytes) = match which {
        Image::Small => ("helia-icon", SMALL_ICON),
        Image::Logo => ("helia-logo", LOGO),
    };
    let id = egui::Id::new(name);
    if let Some(t) = ctx.data(|d| d.get_temp::<TextureHandle>(id)) {
        return Some(t);
    }
    let (size, rgba) = decode(bytes)?;
    let opts = TextureOptions {
        mipmap_mode: Some(egui::TextureFilter::Linear),
        ..TextureOptions::LINEAR
    };
    let t = ctx.load_texture(name, ColorImage::from_rgba_unmultiplied(size, &rgba), opts);
    ctx.data_mut(|d| d.insert_temp(id, t.clone()));
    Some(t)
}
