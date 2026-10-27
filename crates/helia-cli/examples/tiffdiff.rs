// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Compare two 16-bit stacked TIFFs: `cargo run --example tiffdiff -- a.tif b.tif`.
//!
//! Prints the RMS and largest difference (as a share of the full range) and
//! the blurred detail score of each image, the same figure `helia stack`
//! reports, so a change in the pipeline can be judged on real output.
#![allow(clippy::many_single_char_names, reason = "image maths")]

use std::fs::File;
use std::io::BufReader;

use helia_core::{Frame, Metric, filter, quality};
use tiff::decoder::{Decoder, DecodingResult};

fn load(path: &str) -> Result<Frame, Box<dyn std::error::Error>> {
    let mut dec = Decoder::new(BufReader::new(File::open(path)?))?;
    let (w, h) = dec.dimensions()?;
    let DecodingResult::U16(px) = dec.read_image()? else {
        return Err("not a 16-bit TIFF".into());
    };
    let data = px.iter().map(|&v| f32::from(v) / 65535.0).collect();
    Ok(Frame::new(w as usize, h as usize, data)?)
}

fn detail(f: &Frame) -> f32 {
    quality::score(&filter::gaussian_blur(f, 2.0), Metric::GradientEnergy, None)
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [a, b] = args.as_slice() else {
        return Err("usage: tiffdiff a.tif b.tif".into());
    };
    let (fa, fb) = (load(a)?, load(b)?);
    if (fa.width, fa.height) != (fb.width, fb.height) {
        return Err("image sizes differ".into());
    }
    let (mut s2, mut worst) = (0.0f64, 0.0f32);
    for (x, y) in fa.data.iter().zip(&fb.data) {
        let d = (x - y).abs();
        s2 += f64::from(d) * f64::from(d);
        worst = worst.max(d);
    }
    let rms = (s2 / fa.data.len() as f64).sqrt();
    println!(
        "rms diff {rms:.5}  max diff {worst:.5}  detail a {:.6} b {:.6} (b/a {:.4})",
        detail(&fa),
        detail(&fb),
        detail(&fb) / detail(&fa).max(1e-12)
    );
    Ok(())
}
