// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Reading the seeing off a burst of frames: tiles of the disc are followed
//! through a sample of the capture, giving the shifts for the motion index and
//! the aligned tile spectra for the spectral ratio.

use rayon::prelude::*;

use crate::align::Shift;
use crate::align::global::{PhaseCorrelator, PhaseOptions};
use crate::align::multipoint::{ApGrid, ApMask, ApTemplate, MultipointConfig, Prepared, measure};
use crate::error::Result;
use crate::frame::Frame;
use crate::seeing::motion::{MotionIndex, motion_index, motion_series};
use crate::seeing::spectral::{Ring, TILE, TileSpectrum, noise_sigma, pool};

/// Most tiles followed.
pub const MAX_TILES: usize = 16;
/// Search radius around the global shift, pixels.
const SEARCH: usize = 6;
/// Lowest peak correlation for a tile's shift to count.
const MIN_CORRELATION: f32 = 0.5;

/// Tiles followed through a burst. The non-overlapping tiles with the most
/// detail are chosen, which on a regular half-tile grid sit at least a tile apart.
pub struct BurstProbe {
    grid: ApGrid,
    templates: Vec<Option<ApTemplate>>,
    /// Indices into the grid of the tiles followed.
    chosen: Vec<usize>,
    corr: PhaseCorrelator,
    cfg: MultipointConfig,
    spectra: Vec<TileSpectrum>,
    rows: Vec<Vec<Option<Shift>>>,
    centres: Vec<(f32, f32)>,
}

/// What the probe measured.
pub struct ProbeResult {
    /// The motion index.
    pub motion: Option<MotionIndex>,
    /// The pooled rings of the tile spectra.
    pub rings: Vec<Ring>,
    /// The rings of each tile, to see how much they differ.
    pub per_tile: Vec<Vec<Ring>>,
    /// Differential motion of each frame added, in pixels per axis (see
    /// [`motion_series`]), in the order they were added; `None` where no pair
    /// of tiles locked.
    pub series: Vec<Option<f32>>,
    /// Frames that were added.
    pub frames: usize,
    /// Tiles followed.
    pub tiles: usize,
}

impl BurstProbe {
    /// Choose up to [`MAX_TILES`] tiles of the reference: the highest-contrast
    /// non-overlapping `TILE` x `TILE` windows that lie wholly inside the
    /// disc (when one is known). `None` when there are fewer than two.
    ///
    /// # Errors
    /// Phase correlator set-up failures.
    pub fn new(reference: &Frame, mask: &ApMask, phase: PhaseOptions) -> Result<Option<Self>> {
        let cfg = MultipointConfig {
            ap_size: TILE,
            step: TILE / 2,
            search_radius: SEARCH,
            min_correlation: MIN_CORRELATION,
            ..MultipointConfig::default()
        };
        let grid = ApGrid::build_masked(reference, &cfg, mask)?;
        let inside = |ap: &crate::align::multipoint::Ap| {
            let r = ap.rect;
            mask.disc.is_none_or(|d| {
                [
                    (r.x0, r.y0),
                    (r.x1 - 1, r.y0),
                    (r.x0, r.y1 - 1),
                    (r.x1 - 1, r.y1 - 1),
                ]
                .iter()
                .all(|&(x, y)| d.contains(x, y, -0.04 * d.r))
            })
        };
        let mut candidates: Vec<usize> = grid
            .aps
            .iter()
            .enumerate()
            .filter(|(_, a)| {
                a.active && a.rect.width() == TILE && a.rect.height() == TILE && inside(a)
            })
            .map(|(i, _)| i)
            .collect();
        candidates.sort_by(|&a, &b| grid.aps[b].contrast.total_cmp(&grid.aps[a].contrast));
        // Greedily, the best first, keeping tiles from overlapping.
        let mut picked: Vec<usize> = Vec::new();
        for &c in &candidates {
            let (cx, cy) = (grid.aps[c].cx, grid.aps[c].cy);
            if picked
                .iter()
                .all(|&p| grid.aps[p].cx.abs_diff(cx).max(grid.aps[p].cy.abs_diff(cy)) >= TILE)
            {
                picked.push(c);
            }
            if picked.len() == MAX_TILES {
                break;
            }
        }
        let mut candidates = picked;
        candidates.sort_unstable();
        if candidates.len() < 2 {
            return Ok(None);
        }
        let templates = grid.templates(reference);
        let centres = candidates
            .iter()
            .map(|&i| (grid.aps[i].cx as f32, grid.aps[i].cy as f32))
            .collect();
        Ok(Some(Self {
            corr: PhaseCorrelator::new(reference, phase)?,
            spectra: candidates.iter().map(|_| TileSpectrum::new(TILE)).collect(),
            grid,
            templates,
            chosen: candidates,
            cfg,
            rows: Vec::new(),
            centres,
        }))
    }

    /// Follow the tiles through `frames` and add them to the spectra and the
    /// shift table.
    ///
    /// # Errors
    /// Phase correlation failures.
    pub fn add(&mut self, frames: &[Frame]) -> Result<()> {
        /// One frame's shifts and cut-out tiles.
        type Read = (Vec<Option<Shift>>, Vec<Option<Cut>>);
        struct Cut {
            crop: Vec<f32>,
            rx: f32,
            ry: f32,
            sigma: f32,
        }
        let measured: Vec<Read> = frames
            .par_iter()
            .map(|f| -> Result<_> {
                let g = self.corr.estimate(f)?.shift;
                let prep: Prepared = self.grid.prepare(f);
                let mut shifts = Vec::with_capacity(self.chosen.len());
                let mut cuts = Vec::with_capacity(self.chosen.len());
                for &i in &self.chosen {
                    let m = self.templates[i]
                        .as_ref()
                        .and_then(|t| measure(t, &prep, g, &self.cfg))
                        .filter(|m| m.correlation >= MIN_CORRELATION);
                    shifts.push(m.map(|m| m.shift));
                    cuts.push(m.and_then(|m| {
                        let r = self.grid.aps[i].rect;
                        let (ix, iy) = (m.shift.dx.round(), m.shift.dy.round());
                        let (x0, y0) = (r.x0 as i64 + ix as i64, r.y0 as i64 + iy as i64);
                        if x0 < 0
                            || y0 < 0
                            || x0 as usize + TILE > f.width
                            || y0 as usize + TILE > f.height
                        {
                            return None;
                        }
                        let (x0, y0) = (x0 as usize, y0 as usize);
                        let mut crop = Vec::with_capacity(TILE * TILE);
                        for y in y0..y0 + TILE {
                            crop.extend_from_slice(&f.row(y)[x0..x0 + TILE]);
                        }
                        let sigma = noise_sigma(&crop, TILE);
                        Some(Cut {
                            crop,
                            rx: m.shift.dx - ix,
                            ry: m.shift.dy - iy,
                            sigma,
                        })
                    }));
                }
                Ok((shifts, cuts))
            })
            .collect::<Result<_>>()?;
        let cuts: Vec<&Vec<Option<Cut>>> = measured.iter().map(|m| &m.1).collect();
        self.spectra
            .par_iter_mut()
            .enumerate()
            .for_each(|(t, spectrum)| {
                for c in &cuts {
                    if let Some(c) = &c[t] {
                        spectrum.add(&c.crop, c.rx, c.ry, c.sigma);
                    }
                }
            });
        self.rows.extend(measured.into_iter().map(|m| m.0));
        Ok(())
    }

    /// What was measured.
    #[must_use]
    pub fn finish(self) -> ProbeResult {
        let per_tile: Vec<Vec<Ring>> = self
            .spectra
            .iter()
            .filter(|s| s.frames() >= 16)
            .map(TileSpectrum::rings)
            .collect();
        ProbeResult {
            motion: motion_index(&self.centres, &self.rows),
            series: motion_series(&self.centres, &self.rows),
            rings: pool(&per_tile),
            tiles: per_tile.len(),
            per_tile,
            frames: self.rows.len(),
        }
    }
}
