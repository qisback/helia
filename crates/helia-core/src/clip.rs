// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Clipped colour planes of a raw colour capture.
//!
//! A narrow-band filter lights one Bayer plane far more than the others: with
//! H-alpha the red pixels carry the disc and the green ones only a leak. An
//! exposure set to show the whole of that leak, or the prominences, clips the
//! bright plane: the disc becomes a flat plateau at the sensor limit and
//! holds no detail at all. Stacking that plane gives a white disc, while the
//! plane next to it, short of the limit, carries the chromosphere (the real
//! H-alpha captures of 2022-07-17 are all like this: red 98 to 100% flat,
//! green 40 to 60% of scale with plage, filaments and fibrils).
//!
//! [`choose_channel`] looks at a few frames as read, finds the planes that
//! are mostly flat at their own maximum, and moves the stack to the best plane
//! that is not. A plateau is recognised by its flatness, not by a fixed
//! level, because a recorder may scale the sensor range (a 12-bit sensor in
//! a 16-bit file clips at 0.0625 of 16-bit full scale).

use crate::debayer::Channel;
use crate::frame::Frame;

/// A plane is clipped when at least this share of its lit pixels sit at its
/// maximum.
pub const CLIPPED_SHARE: f32 = 0.20;

/// A plane counts as holding signal when its disc level is at least this
/// share of the brightest plane's level.
const MIN_RELATIVE_LEVEL: f32 = 0.08;

/// How far below its ceiling a pixel may sit and still count as on the
/// plateau: rounding of the scaled integer samples only.
const PLATEAU_TOLERANCE: f32 = 1e-5;

/// Plateau tolerance for decoded video: lossy compression leaves a clipped
/// plane within a few steps of 8-bit full scale rather than exactly at one value.
const VIDEO_TOLERANCE: f32 = 4.0 / 255.0;

/// Frames read to look for clipping.
pub const PROBE_FRAMES: usize = 6;

/// What the planes of a raw colour capture look like.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PlaneClip {
    /// Per plane (R, G, B), the share of the lit pixels at the plane maximum.
    pub clipped: [f32; 3],
    /// Per plane, the mean level of the lit pixels (0 to 1 of full scale).
    pub level: [f32; 3],
}

/// The plane to stack and what was done about the one that was asked for.
#[derive(Clone, Debug, PartialEq)]
pub struct ChannelChoice {
    /// Plane to stack.
    pub channel: Channel,
    /// A sentence for the user, when the choice changed or no plane is usable.
    pub note: Option<String>,
}

fn name(c: Channel) -> &'static str {
    match c {
        Channel::Red => "red",
        Channel::Green => "green",
        Channel::Blue => "blue",
        Channel::Luminance => "luminance",
    }
}

fn plane(i: usize) -> Channel {
    [Channel::Red, Channel::Green, Channel::Blue][i]
}

/// The share of lit pixels on the plateau and the mean lit level of one plane
/// of samples. `None` when the plane is empty.
fn plane_stats(vals: &[f32], tolerance: f32) -> Option<(f32, f32)> {
    if vals.is_empty() {
        return None;
    }
    let mut tail = vals.to_vec();
    let k = ((tail.len() as f32 * 0.995) as usize).min(tail.len() - 1);
    let (_, &mut ceiling, _) = tail.select_nth_unstable_by(k, f32::total_cmp);
    if ceiling <= 0.0 {
        return None;
    }
    // Never wider than 1.5% of the ceiling, or a dim plane with a smooth
    // profile would pass for flat.
    let tolerance = tolerance.min(0.015 * ceiling);
    let (mut lit, mut flat, mut sum) = (0usize, 0usize, 0.0f64);
    for &v in vals.iter().filter(|&&v| v >= 0.25 * ceiling) {
        lit += 1;
        sum += f64::from(v);
        if v >= ceiling - tolerance {
            flat += 1;
        }
    }
    (lit > 0).then(|| (flat as f32 / lit as f32, (sum / lit as f64) as f32))
}

/// Mean of per-sample statistics into a [`PlaneClip`].
fn average(samples: &[[Option<(f32, f32)>; 3]]) -> Option<PlaneClip> {
    let mut clipped = [0.0f32; 3];
    let mut level = [0.0f32; 3];
    let mut used = 0usize;
    for s in samples.iter().filter(|s| s.iter().any(Option::is_some)) {
        for (c, st) in s.iter().enumerate() {
            if let Some((cl, lv)) = st {
                clipped[c] += cl;
                level[c] += lv;
            }
        }
        used += 1;
    }
    (used > 0).then(|| PlaneClip {
        clipped: clipped.map(|v| v / used as f32),
        level: level.map(|v| v / used as f32),
    })
}

/// Measure the planes of `frames` (raw mosaics, as read). `None` when the
/// frames are not a mosaic or show no lit pixels.
///
/// A plane's ceiling is its 99.5th percentile, not its maximum: a few hot
/// pixels sit above a plateau (the 2022 red planes clip at 203 of 255 with hot
/// pixels at 255). The plateau is every lit pixel with that same value.
#[must_use]
pub fn probe(frames: &[Frame]) -> Option<PlaneClip> {
    let mut samples = Vec::new();
    for f in frames {
        let pattern = f.bayer?;
        let mut planes: [Vec<f32>; 3] = [Vec::new(), Vec::new(), Vec::new()];
        for y in 0..f.height {
            for (x, &v) in f.row(y).iter().enumerate() {
                planes[pattern.channel_at(x, y)].push(v);
            }
        }
        samples.push(planes.each_ref().map(|p| plane_stats(p, PLATEAU_TOLERANCE)));
    }
    average(&samples)
}

/// As [`probe`], for a colour video: one decoded frame per plane (red, green,
/// blue), each a full mono picture.
#[must_use]
pub fn probe_planes(planes: [&Frame; 3]) -> Option<PlaneClip> {
    average(&[planes.map(|f| plane_stats(&f.data, VIDEO_TOLERANCE))])
}

/// The sentence that ends a plane-change note for the command line.
pub const CLI_OVERRIDE_HINT: &str = "Name a channel with --channel to override.";

/// A plane-change note worded for the app: the override is a channel chip, not a flag.
#[must_use]
pub fn app_note(note: &str) -> String {
    note.replace(
        CLI_OVERRIDE_HINT,
        "Pick a channel under Advanced settings to override.",
    )
}

/// The plane to stack, given the one `asked` for (a preset, or the user) and
/// the measured planes.
///
/// A single plane, or the luminance, that is mostly clipped is replaced by the
/// brightest plane that is not, so long as it holds real signal. Nothing is
/// replaced when no plane qualifies (the exposure is too long for any of
/// them); the note says so.
#[must_use]
pub fn choose(asked: Channel, p: &PlaneClip) -> ChannelChoice {
    let top = p.level.iter().copied().fold(0.0f32, f32::max);
    // An empty plane (blue behind a red filter) is all noise at one level, which
    // looks flat; it is empty, not clipped.
    let holds_signal = |i: usize| p.level[i] >= MIN_RELATIVE_LEVEL * top;
    let bad = |i: usize| p.clipped[i] >= CLIPPED_SHARE && holds_signal(i);
    let usable = |i: usize| !bad(i) && holds_signal(i);
    let asked_plane = match asked {
        Channel::Red => Some(0),
        Channel::Green => Some(1),
        Channel::Blue => Some(2),
        Channel::Luminance => None,
    };
    let first_bad = (0..3).find(|&i| bad(i));
    let culprit = match asked_plane {
        Some(i) => bad(i).then_some(i),
        None => first_bad,
    };
    let Some(culprit) = culprit else {
        return ChannelChoice {
            channel: asked,
            note: None,
        };
    };
    let clipped_names: Vec<String> = (0..3)
        .filter(|&i| bad(i))
        .map(|i| format!("{} {:.0}%", name(plane(i)), (p.clipped[i] * 100.0).round()))
        .collect();
    let best = (0..3)
        .filter(|&i| usable(i))
        .max_by(|&a, &b| p.level[a].total_cmp(&p.level[b]));
    match best {
        Some(i) => ChannelChoice {
            channel: plane(i),
            note: Some(format!(
                "the {} plane is clipped ({} of its lit pixels sit at the sensor limit, so the disc is flat there); stacking the {} plane instead. A shorter exposure or a lower gain keeps {} usable. {CLI_OVERRIDE_HINT}",
                name(plane(culprit)),
                clipped_names.join(", "),
                name(plane(i)),
                name(plane(culprit)),
            )),
        },
        None => ChannelChoice {
            channel: asked,
            note: Some(format!(
                "every usable colour plane is clipped ({}): the exposure is too long for them to hold detail. Shorten the exposure or lower the gain.",
                clipped_names.join(", ")
            )),
        },
    }
}

/// Read a few frames of `src` and choose the plane to stack, as [`choose`].
/// Frames that are not raw colour leave `asked` alone.
#[must_use]
pub fn choose_channel(
    asked: Channel,
    src: &mut dyn crate::io::source::FrameSource,
) -> ChannelChoice {
    let n = src.frame_count();
    let frames: Vec<Frame> = crate::mask::sample_indices(n, PROBE_FRAMES.min(n))
        .into_iter()
        .filter_map(|i| src.read(i).ok())
        .collect();
    match probe(&frames) {
        Some(p) => choose(asked, &p),
        None => ChannelChoice {
            channel: asked,
            note: None,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::BayerPattern;

    /// A mosaic disc: `levels` per plane (R, G, B), clipped at `limit`.
    fn disc(levels: [f32; 3], limit: f32) -> Frame {
        let (w, h) = (96usize, 96usize);
        let mut f = Frame::zeros(w, h).with_bayer(Some(BayerPattern::Rggb));
        for y in 0..h {
            for x in 0..w {
                let (dx, dy) = (x as f32 - 48.0, y as f32 - 48.0);
                if dx * dx + dy * dy < 40.0 * 40.0 {
                    let c = BayerPattern::Rggb.channel_at(x, y);
                    // A little texture, so an unclipped plane has no plateau.
                    let tex = 0.2 * (((x * 7 + y * 13) % 97) as f32 / 97.0 - 0.5);
                    f.data[y * w + x] = (levels[c] * (1.0 + tex)).min(limit);
                }
            }
        }
        f
    }

    #[test]
    fn a_clipped_red_plane_moves_ha_to_green() {
        let frames = vec![disc([1.6, 0.5, 0.0], 1.0), disc([1.6, 0.5, 0.0], 1.0)];
        let p = probe(&frames).expect("mosaic");
        assert!(p.clipped[0] > 0.9, "{p:?}");
        assert!(p.clipped[1] < 0.1, "{p:?}");
        let c = choose(Channel::Red, &p);
        assert_eq!(c.channel, Channel::Green);
        assert!(
            c.note
                .as_deref()
                .is_some_and(|n| n.contains("red plane is clipped"))
        );
    }

    #[test]
    fn the_plateau_need_not_be_at_full_scale() {
        // A 12-bit sensor in a 16-bit file clips at 0.0625 of full scale.
        let f = disc([0.2, 0.03, 0.0], 0.0625);
        let p = probe(&[f]).expect("mosaic");
        assert!(p.clipped[0] > 0.9, "{p:?}");
        assert_eq!(choose(Channel::Red, &p).channel, Channel::Green);
    }

    #[test]
    fn an_unclipped_plane_is_left_alone() {
        let p = probe(&[disc([0.8, 0.4, 0.0], 1.0)]).expect("mosaic");
        assert!(p.clipped.iter().all(|&s| s < 0.1), "{p:?}");
        let c = choose(Channel::Red, &p);
        assert_eq!((c.channel, c.note), (Channel::Red, None));
    }

    #[test]
    fn luminance_drops_to_the_unclipped_plane_when_others_clip() {
        let p = probe(&[disc([1.5, 0.7, 1.4], 1.0)]).expect("mosaic");
        let c = choose(Channel::Luminance, &p);
        assert_eq!(c.channel, Channel::Green);
        assert!(c.note.is_some());
    }

    #[test]
    fn an_empty_plane_is_not_chosen() {
        // Red clipped, green nearly empty, blue empty: nothing to fall back on.
        let p = probe(&[disc([1.6, 0.05, 0.0], 1.0)]).expect("mosaic");
        let c = choose(Channel::Red, &p);
        assert_eq!(c.channel, Channel::Red);
        assert!(
            c.note
                .as_deref()
                .is_some_and(|n| n.contains("every usable"))
        );
    }

    #[test]
    fn a_video_with_a_clipped_red_plane_uses_green() {
        // Decoded planes of a Seestar-style frame: red flat at 254, green
        // textured, blue empty.
        let plane = |level: f32, limit: f32| {
            let mut f = Frame::zeros(64, 64);
            for y in 0..64 {
                for x in 0..64 {
                    let (dx, dy) = (x as f32 - 32.0, y as f32 - 32.0);
                    if dx * dx + dy * dy < 28.0 * 28.0 {
                        let tex = 0.2 * (((x * 7 + y * 13) % 97) as f32 / 97.0 - 0.5);
                        f.data[y * 64 + x] = (level * (1.0 + tex)).min(limit);
                    }
                }
            }
            f
        };
        let (red, green, blue) = (plane(1.4, 0.99), plane(0.34, 1.0), plane(0.006, 1.0));
        let p = probe_planes([&red, &green, &blue]).expect("planes");
        let c = choose(Channel::Luminance, &p);
        assert_eq!(c.channel, Channel::Green, "{p:?}");
    }

    #[test]
    fn mono_frames_are_ignored() {
        let f = Frame::zeros(8, 8);
        assert_eq!(probe(&[f]), None);
    }
}
