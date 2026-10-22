// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Solar specifics: the Stonyhurst and Carrington heliographic grid laid over
//! the stack with solar north and east marked, and the sunspot overlay and
//! table of the stack.

use std::fmt::Write as _;

use helia_core::colour::RgbImage;
use helia_core::ephemeris;
use helia_core::pipeline::StackOutput;
use helia_core::sunspots;
use plotters::prelude::*;
use serde::{Deserialize, Serialize};

use crate::ap::DiscData;
use crate::bundle::{Bundle, Figure, Section, Table};
use crate::canvas::{self, FONT, Size, palette, plot_err, tick};
use crate::image_plot::{GreyImage, PictureChart, paint_backdrop, picture_axes, picture_chart};
use crate::{Error, PlotKind, Result};

/// What the caller knows about the capture that the stack does not keep.
#[derive(Clone, Debug, Default)]
pub struct SolarInputs {
    /// When the stack was taken, unix seconds UTC (the mid-time of the capture).
    pub when_unix: Option<f64>,
    /// The image has celestial north up (an equatorial mount): solar north is
    /// then turned by the angle P.
    pub celestial_north_up: bool,
    /// Extra turn of the picture in degrees, counter-clockwise, before the
    /// above (a camera rotated, a flipped diagonal).
    pub rotate_deg: f32,
    /// The picture is a mirror image.
    pub mirror: bool,
    /// The sunspot detection of the stack, when it was asked for.
    pub sunspots: Option<(sunspots::Report, RgbImage)>,
}

/// The orientation of the Sun in the picture.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Orientation {
    /// Position angle of the axis, degrees east of celestial north.
    pub p_deg: f64,
    /// Heliographic latitude of the disc centre.
    pub b0_deg: f64,
    /// Carrington longitude of the disc centre.
    pub l0_deg: f64,
    /// Angle by which the picture is turned to put solar north up, degrees.
    pub north_angle_deg: f64,
    /// Whether the picture is a mirror image.
    pub mirror: bool,
    /// Whether celestial north is up in the picture.
    pub celestial_north_up: bool,
}

/// A sunspot group for the table.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct GroupRow {
    /// Group number in the picture.
    pub id: usize,
    /// Spots.
    pub spots: usize,
    /// Latitude.
    pub lat_deg: f32,
    /// Longitude from the central meridian.
    pub cmd_deg: f32,
    /// Carrington longitude when known.
    pub lon_deg: Option<f32>,
    /// Corrected area, millionths of the hemisphere.
    pub area_msh: f32,
}

/// Everything the solar plots are drawn from.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SolarData {
    /// Layout version of this file.
    pub version: u32,
    /// Width of the picture in pixels.
    pub width: usize,
    /// Height of the picture in pixels.
    pub height: usize,
    /// The fitted disc.
    pub disc: DiscData,
    /// Time of the picture, ISO 8601, when known.
    pub when: Option<String>,
    /// P, B0 and L0 and the orientation, when the time is known.
    pub orientation: Option<Orientation>,
    /// Sunspot groups, when the detection ran.
    pub groups: Vec<GroupRow>,
    /// Wolf number, when the detection ran.
    pub wolf: Option<f32>,
    /// The picture, shrunk (written as `solar-reference.png`, not in the JSON).
    #[serde(skip)]
    pub backdrop: Option<GreyImage>,
    /// The sunspot overlay as a PNG, the table as CSV and the report as JSON.
    #[serde(skip)]
    pub spots: Option<(Vec<u8>, String, String)>,
}

/// One sample of a graticule line.
struct Line {
    system: &'static str,
    label: String,
    value: f64,
    /// Longitude from the central meridian, for a meridian; `None` for a parallel.
    cmd: Option<f64>,
    points: Vec<(f64, f64)>,
    major: bool,
}

impl SolarData {
    /// File name of the JSON.
    pub const JSON: &'static str = "solar.json";
    /// File name of the graticule table.
    pub const CSV: &'static str = "heliographic-grid.csv";
    /// File name of the backdrop.
    pub const BACKDROP: &'static str = "solar-reference.png";

    /// Gather what the stack and the capture say about the Sun; `None` when
    /// the stack has no fitted disc.
    #[must_use]
    pub fn from_output(out: &StackOutput, inputs: &SolarInputs) -> Option<Self> {
        let disc = out.disc?;
        let (backdrop, _) = GreyImage::from_frame(&out.image, 900);
        let orientation = inputs.when_unix.map(|t| {
            let e = ephemeris::solar_ephemeris(t);
            let north = if inputs.celestial_north_up {
                f64::from(inputs.rotate_deg) + e.p_deg
            } else {
                f64::from(inputs.rotate_deg)
            };
            Orientation {
                p_deg: e.p_deg,
                b0_deg: e.b0_deg,
                l0_deg: e.l0_deg,
                north_angle_deg: north,
                mirror: inputs.mirror,
                celestial_north_up: inputs.celestial_north_up,
            }
        });
        let (groups, wolf, spots) = match &inputs.sunspots {
            Some((r, overlay)) => {
                let mut png = Vec::new();
                helia_core::io::png::write_png8_rgb(
                    &mut png,
                    overlay,
                    Some("Helia sunspot overlay"),
                )
                .ok();
                (
                    r.groups
                        .iter()
                        .map(|g| GroupRow {
                            id: g.id,
                            spots: g.spots,
                            lat_deg: g.lat_deg,
                            cmd_deg: g.cmd_deg,
                            lon_deg: g.lon_deg,
                            area_msh: g.area_msh,
                        })
                        .collect(),
                    Some(r.wolf),
                    Some((png, r.csv(), r.json())),
                )
            }
            None => (Vec::new(), None, None),
        };
        Some(Self {
            version: 1,
            width: out.image.width,
            height: out.image.height,
            disc: DiscData {
                cx: disc.cx,
                cy: disc.cy,
                r: disc.r,
            },
            when: inputs
                .when_unix
                .map(helia_core::io::solarnet::iso_from_unix),
            orientation,
            groups,
            wolf,
            backdrop: Some(backdrop),
            spots,
        })
    }

    /// Pixel position and visibility of a heliographic point: latitude and
    /// longitude from the central meridian, degrees.
    fn project(&self, o: &Orientation, lat_deg: f64, cmd_deg: f64) -> Option<(f64, f64)> {
        let (phi, lam) = (lat_deg.to_radians(), cmd_deg.to_radians());
        let (sb, cb) = (o.b0_deg.to_radians().sin(), o.b0_deg.to_radians().cos());
        let xs = phi.cos() * lam.sin();
        let ys = phi.sin() * cb - phi.cos() * lam.cos() * sb;
        let z = phi.sin() * sb + phi.cos() * lam.cos() * cb;
        if z < 0.0 {
            return None;
        }
        Some(self.to_pixel(o, xs, ys))
    }

    /// Pixel of the disc-plane point `(xs, ys)` in solar radii (west, north).
    fn to_pixel(&self, o: &Orientation, xs: f64, ys: f64) -> (f64, f64) {
        let a = o.north_angle_deg.to_radians();
        let r = f64::from(self.disc.r);
        let mut dx = r * (xs * a.cos() - ys * a.sin());
        let dy = r * (xs * a.sin() + ys * a.cos());
        if o.mirror {
            dx = -dx;
        }
        (f64::from(self.disc.cx) + dx, f64::from(self.disc.cy) - dy)
    }

    /// The graticule of one system: parallels every 15 degrees and meridians
    /// every 15 degrees (from the central meridian, or in Carrington longitude).
    fn graticule(&self, o: &Orientation, carrington: bool) -> Vec<Line> {
        let mut lines = Vec::new();
        for lat in (-75..=75).step_by(15) {
            let lat = f64::from(lat);
            let mut pts = Vec::new();
            for k in -90..=90 {
                if let Some(p) = self.project(o, lat, f64::from(k)) {
                    pts.push(p);
                }
            }
            lines.push(Line {
                system: if carrington {
                    "carrington"
                } else {
                    "stonyhurst"
                },
                label: if lat == 0.0 {
                    "EQ".to_string()
                } else {
                    format!("{}{}", lat.abs(), if lat > 0.0 { "N" } else { "S" })
                },
                value: lat,
                cmd: None,
                points: pts,
                major: lat == 0.0,
            });
        }
        // Meridians: Stonyhurst offsets from the central meridian are multiples
        // of 15; Carrington ones are multiples of 15 of the longitude itself.
        let l0 = o.l0_deg;
        let first = if carrington {
            ((l0 - 100.0) / 15.0).ceil() as i32
        } else {
            -6
        };
        let last = if carrington {
            ((l0 + 100.0) / 15.0).floor() as i32
        } else {
            6
        };
        for m in first..=last {
            let (value, cmd) = if carrington {
                let lon = f64::from(m) * 15.0;
                (
                    lon.rem_euclid(360.0),
                    (lon - l0 + 180.0).rem_euclid(360.0) - 180.0,
                )
            } else {
                (f64::from(m) * 15.0, f64::from(m) * 15.0)
            };
            if cmd.abs() > 95.0 {
                continue;
            }
            let pts: Vec<(f64, f64)> = (-90..=90)
                .filter_map(|k| self.project(o, f64::from(k), cmd))
                .collect();
            lines.push(Line {
                system: if carrington {
                    "carrington"
                } else {
                    "stonyhurst"
                },
                label: if carrington {
                    format!("{value:.0}")
                } else if value == 0.0 {
                    "CM".to_string()
                } else {
                    format!("{}{}", value.abs(), if value > 0.0 { "W" } else { "E" })
                },
                value,
                cmd: Some(cmd),
                points: pts,
                major: cmd.abs() < 7.5,
            });
        }
        lines
    }

    /// Draw the Stonyhurst or Carrington grid over the stack.
    pub fn plot(&self, kind: PlotKind, size: Size) -> Result<Vec<u8>> {
        let carrington = match kind {
            PlotKind::GridStonyhurst => false,
            PlotKind::GridCarrington => true,
            _ => {
                return Err(Error::NoData(format!(
                    "{} is not a heliographic grid",
                    kind.name()
                )));
            }
        };
        let o = self.orientation.as_ref().ok_or_else(|| {
            Error::NoData(
                "the time of the capture is unknown, so P, B0 and L0 cannot be found (a SER with timestamps, or a FITS with DATE-OBS, gives it)".into(),
            )
        })?;
        let img = self.backdrop.as_ref().ok_or_else(|| {
            Error::NoData(format!("{} is missing from the report", Self::BACKDROP))
        })?;
        let lines = self.graticule(o, carrington);
        let title = format!(
            "{} grid: P {:+.2} deg, B0 {:+.2} deg, L0 {:.2} deg{}",
            if carrington {
                "Carrington"
            } else {
                "Stonyhurst"
            },
            o.p_deg,
            o.b0_deg,
            o.l0_deg,
            self.when
                .as_ref()
                .map_or(String::new(), |w| format!(", {w}"))
        );
        canvas::render(size, |root| {
            let (mut chart, layout) =
                picture_chart(root, size, (self.width, self.height), 0, &title)?;
            paint_backdrop(root, layout, img, 0.9)?;
            picture_axes(&mut chart, "x (px)", "y (px)")?;
            for l in &lines {
                if l.points.len() < 2 {
                    continue;
                }
                let colour = if l.major {
                    palette::ORANGE
                } else {
                    RGBColor(255, 255, 255)
                };
                chart
                    .draw_series(std::iter::once(PathElement::new(
                        l.points.clone(),
                        colour
                            .mix(if l.major { 0.95 } else { 0.6 })
                            .stroke_width(if l.major { 2 } else { 1 }),
                    )))
                    .map_err(plot_err)?;
            }
            self.label_grid(&mut chart, o, &lines)?;
            self.compass(&mut chart, o)?;
            // Disc limb and centre.
            let d = self.disc;
            chart
                .draw_series(std::iter::once(PathElement::new(
                    (0..=180)
                        .map(|i| {
                            let a = f64::from(i) * std::f64::consts::PI / 90.0;
                            (
                                f64::from(d.cx) + f64::from(d.r) * a.cos(),
                                f64::from(d.cy) + f64::from(d.r) * a.sin(),
                            )
                        })
                        .collect::<Vec<_>>(),
                    palette::SKY.mix(0.9).stroke_width(1),
                )))
                .map_err(plot_err)?;
            Ok(())
        })
    }

    fn label_grid(
        &self,
        chart: &mut PictureChart<'_, '_>,
        o: &Orientation,
        lines: &[Line],
    ) -> Result<()> {
        let st = (FONT, 13).into_font().color(&WHITE);
        for l in lines {
            // A parallel is labelled at its west end, a meridian where it
            // crosses the equator.
            let at = match l.cmd {
                None => l.points.last().map(|&(x, y)| (x + 5.0, y - 8.0)),
                // Meridians near the limb are too foreshortened to label.
                Some(cmd) if cmd.abs() <= 76.0 => {
                    self.project(o, 0.0, cmd).map(|(x, y)| (x - 10.0, y + 6.0))
                }
                Some(_) => None,
            };
            if let Some(at) = at {
                chart
                    .draw_series(std::iter::once(Text::new(l.label.clone(), at, st.clone())))
                    .map_err(plot_err)?;
            }
        }
        Ok(())
    }

    /// Solar north and east arrows beside the disc.
    fn compass(&self, chart: &mut PictureChart<'_, '_>, o: &Orientation) -> Result<()> {
        let d = self.disc;
        let r = f64::from(d.r);
        // The top-left corner, which is sky unless the disc fills the frame.
        let base = (r * 0.18).max(20.0);
        let origin = (base * 1.7 + 40.0, base * 1.5 + 30.0);
        for (label, xs, ys, colour) in [
            ("N", 0.0, 1.0, palette::ORANGE),
            ("E", -1.0, 0.0, palette::SKY),
        ] {
            let (px, py) = self.to_pixel(o, xs, ys);
            let (ox, oy) = self.to_pixel(o, 0.0, 0.0);
            let (vx, vy) = (px - ox, py - oy);
            let n = vx.hypot(vy).max(1e-9);
            let (ux, uy) = (vx / n, vy / n);
            let tip = (origin.0 + ux * base, origin.1 + uy * base);
            chart
                .draw_series(vec![
                    PathElement::new(vec![origin, tip], colour.stroke_width(3)),
                    PathElement::new(
                        vec![
                            tip,
                            (
                                tip.0 - 0.35 * base * (ux * 0.9 - uy * 0.45),
                                tip.1 - 0.35 * base * (uy * 0.9 + ux * 0.45),
                            ),
                        ],
                        colour.stroke_width(3),
                    ),
                    PathElement::new(
                        vec![
                            tip,
                            (
                                tip.0 - 0.35 * base * (ux * 0.9 + uy * 0.45),
                                tip.1 - 0.35 * base * (uy * 0.9 - ux * 0.45),
                            ),
                        ],
                        colour.stroke_width(3),
                    ),
                ])
                .map_err(plot_err)?;
            chart
                .draw_series(std::iter::once(Text::new(
                    format!("solar {label}"),
                    (
                        tip.0 + ux * 10.0
                            - if ux < -0.3 {
                                62.0
                            } else if ux.abs() <= 0.3 {
                                26.0
                            } else {
                                0.0
                            },
                        tip.1 + uy * 10.0 - 8.0,
                    ),
                    (FONT, 15).into_font().color(&colour),
                )))
                .map_err(plot_err)?;
        }
        Ok(())
    }

    /// The graticule as CSV: one row per sample point.
    #[must_use]
    pub fn csv(&self) -> String {
        let mut s = String::from("system,line,value_deg,x_px,y_px\n");
        if let Some(o) = &self.orientation {
            for carr in [false, true] {
                for l in self.graticule(o, carr) {
                    for (x, y) in &l.points {
                        let _ = writeln!(s, "{},{},{},{x:.2},{y:.2}", l.system, l.label, l.value);
                    }
                }
            }
        }
        s
    }

    /// Draw every plot that has data and add the files and the section to `bundle`.
    pub fn build(&self, bundle: &mut Bundle, size: Size) -> Result<()> {
        let mut json = serde_json::to_vec_pretty(self)?;
        json.push(b'\n');
        bundle.add(Self::JSON, json);
        bundle.add(Self::CSV, self.csv().into_bytes());
        if let Some(b) = &self.backdrop {
            bundle.add(Self::BACKDROP, b.to_png()?);
        }
        let mut figures = Vec::new();
        let mut notes = Vec::new();
        for kind in [PlotKind::GridStonyhurst, PlotKind::GridCarrington] {
            match self.plot(kind, size) {
                Ok(png) => {
                    let (title, caption) = describe(kind);
                    let name = format!("{}.png", kind.name());
                    bundle.add(&name, png);
                    figures.push(Figure {
                        png: name,
                        title: title.into(),
                        caption: caption.into(),
                        data: vec![Self::JSON.into(), Self::CSV.into()],
                    });
                }
                Err(Error::NoData(why)) => notes.push(format!("{}: {why}", kind.name())),
                Err(e) => return Err(e),
            }
        }
        let mut tables = Vec::new();
        if let Some((png, csv, json)) = &self.spots {
            bundle.add("sunspots.png", png.clone());
            bundle.add("sunspots.csv", csv.clone().into_bytes());
            bundle.add("sunspots.json", json.clone().into_bytes());
            figures.push(Figure {
                png: "sunspots.png".into(),
                title: "Sunspots".into(),
                caption: "The flattened disc with the penumbra and umbra of every spot outlined and the groups numbered. The table below lists each group: its latitude, its longitude from the central meridian and its corrected area in millionths of the hemisphere.".into(),
                data: vec!["sunspots.csv".into(), "sunspots.json".into()],
            });
            tables.push(Table {
                title: format!(
                    "Sunspot groups{}",
                    self.wolf
                        .map_or(String::new(), |w| format!(" (Wolf number {w:.1})"))
                ),
                header: [
                    "group",
                    "spots",
                    "latitude",
                    "from the central meridian",
                    "Carrington longitude",
                    "corrected area (MSH)",
                ]
                .map(String::from)
                .to_vec(),
                rows: self
                    .groups
                    .iter()
                    .map(|g| {
                        vec![
                            g.id.to_string(),
                            g.spots.to_string(),
                            format!("{:+.0}", g.lat_deg),
                            format!("{:+.0}", g.cmd_deg),
                            g.lon_deg.map_or("n/a".into(), |l| format!("{l:.0}")),
                            tick(f64::from(g.area_msh)),
                        ]
                    })
                    .collect(),
            });
        }
        let intro = match &self.orientation {
            Some(o) => format!(
                "At {} the Sun's axis was at P = {:+.2} deg east of celestial north, tilted B0 = {:+.2} deg towards us, and the Carrington longitude of the disc centre was L0 = {:.2} deg (Meeus, Astronomical Algorithms, chapter 29).",
                self.when.as_deref().unwrap_or("the time of the capture"),
                o.p_deg,
                o.b0_deg,
                o.l0_deg
            ),
            None => "The time of the capture is unknown, so the Sun's orientation cannot be drawn."
                .to_string(),
        };
        bundle.sections.push(Section {
            id: "solar".into(),
            title: "Solar specifics".into(),
            intro,
            figures,
            tables,
            notes,
        });
        Ok(())
    }
}

fn describe(kind: PlotKind) -> (&'static str, &'static str) {
    match kind {
        PlotKind::GridStonyhurst => (
            "Stonyhurst heliographic grid",
            "Latitude and longitude lines every 15 degrees on the stack, measured from the central meridian (CM): W to the west of it, E to the east. The equator and the central meridian are orange. Solar north and solar east are marked at the lower left: north is along the axis, and east is to the left of the picture when north is up, whatever the picture's rotation. The grid follows P (the tilt of the axis against celestial north) and B0 (the tilt of the pole towards us) at the time of the capture, so a spot's coordinates can be read off the lines.",
        ),
        PlotKind::GridCarrington => (
            "Carrington heliographic grid",
            "The same grid labelled in Carrington longitude, which is fixed to the rotating Sun, so a feature keeps its number from one day to the next while the disc turns about 13 degrees a day under the central meridian (L0 is the number at the centre of the disc). Use it to find a spot's longitude, or to look up where an active region will be on another day.",
        ),
        _ => ("", ""),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn data() -> SolarData {
        SolarData {
            version: 1,
            width: 400,
            height: 300,
            disc: DiscData {
                cx: 200.0,
                cy: 150.0,
                r: 120.0,
            },
            when: Some("2026-10-20T11:30:00".into()),
            orientation: Some(Orientation {
                p_deg: 20.0,
                b0_deg: 5.0,
                l0_deg: 100.0,
                north_angle_deg: 20.0,
                mirror: false,
                celestial_north_up: true,
            }),
            groups: vec![GroupRow {
                id: 1,
                spots: 2,
                lat_deg: -12.0,
                cmd_deg: 20.0,
                lon_deg: Some(120.0),
                area_msh: 150.0,
            }],
            wolf: Some(12.0),
            backdrop: Some(GreyImage {
                width: 100,
                height: 75,
                data: vec![90; 7500],
            }),
            spots: None,
        }
    }

    #[test]
    fn the_centre_of_the_disc_projects_to_the_centre() {
        let d = data();
        let o = d.orientation.expect("orientation");
        // The point at the disc centre has latitude B0 and zero longitude from the CM.
        let p = d.project(&o, o.b0_deg, 0.0).expect("visible");
        assert!(
            (p.0 - 200.0).abs() < 1e-6 && (p.1 - 150.0).abs() < 1e-6,
            "{p:?}"
        );
    }

    #[test]
    fn north_is_turned_by_the_angle_and_the_far_side_is_hidden() {
        let d = data();
        let o = d.orientation.expect("orientation");
        // The north pole of the Sun with B0 = 5 is visible, and sits at the
        // top of the disc turned by 20 degrees (clockwise on the screen
        // because the y axis points down and the picture is turned anticlockwise).
        let (x, y) = d.project(&o, 90.0, 0.0).expect("pole visible");
        let (dx, dy) = (x - 200.0, 150.0 - y);
        let angle = dx.atan2(dy).to_degrees();
        assert!((angle + 20.0).abs() < 0.5, "pole direction {angle}");
        assert!(
            d.project(&o, 0.0, 120.0).is_none(),
            "120 degrees from the CM is on the far side"
        );
    }

    #[test]
    fn east_is_left_when_north_is_up() {
        let mut d = data();
        let mut o = d.orientation.expect("orientation");
        o.north_angle_deg = 0.0;
        d.orientation = Some(o);
        let west = d.to_pixel(&o, 1.0, 0.0);
        let east = d.to_pixel(&o, -1.0, 0.0);
        assert!(east.0 < 200.0 && west.0 > 200.0);
    }

    #[test]
    fn both_grids_draw_and_the_csv_lists_lines() {
        let d = data();
        for k in [PlotKind::GridStonyhurst, PlotKind::GridCarrington] {
            let png = d.plot(k, Size::REPORT).expect("plot");
            assert_eq!(&png[1..4], b"PNG");
            if let Some(dir) = std::env::var_os("HELIA_REPORT_DUMP") {
                std::fs::write(
                    std::path::Path::new(&dir).join(format!("{}.png", k.name())),
                    &png,
                )
                .expect("dump");
            }
        }
        let csv = d.csv();
        assert!(csv.contains("stonyhurst,CM,0,") && csv.contains("carrington,"));
        assert!(csv.lines().count() > 200);
    }

    #[test]
    fn without_a_time_the_grid_says_why() {
        let mut d = data();
        d.orientation = None;
        let e = d
            .plot(PlotKind::GridStonyhurst, Size::REPORT)
            .expect_err("no data");
        assert!(e.to_string().contains("time of the capture"), "{e}");
    }
}
