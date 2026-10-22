// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! The sorted-quality curve, drawn with the painter.

use eframe::egui::{
    Align2, Color32, CornerRadius, Mesh, Pos2, Sense, Shape, Stroke, StrokeKind, Ui, WidgetInfo,
    WidgetType, pos2, vec2,
};

use crate::model::Curve;
use crate::theme::{RADIUS_LARGE, mono, palette};

/// Draw the curve. `kept` frames lie left of the dashed threshold; `current`
/// is the frame being viewed. Returns the frame under a click.
pub fn show(
    ui: &mut Ui,
    curve: &Curve,
    kept: usize,
    current: Option<usize>,
    height: f32,
) -> Option<usize> {
    let p = palette();
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::click());
    resp.widget_info(|| {
        WidgetInfo::labeled(
            WidgetType::Label,
            true,
            "Frame quality sorted best first. Click to jump to a frame.",
        )
    });
    let painter = ui.painter_at(rect);
    painter.rect(
        rect,
        CornerRadius::same(RADIUS_LARGE),
        p.raised,
        Stroke::new(1.0, p.border),
        StrokeKind::Inside,
    );
    let plot = rect.shrink2(vec2(14.0, 12.0));
    for k in 1..4 {
        let y = plot.bottom() - plot.height() * k as f32 / 4.0;
        painter.line_segment(
            [pos2(plot.left(), y), pos2(plot.right(), y)],
            Stroke::new(1.0, p.border),
        );
    }
    painter.line_segment(
        [plot.left_bottom(), plot.right_bottom()],
        Stroke::new(1.0, p.border_strong),
    );
    if curve.is_empty() {
        painter.text(
            rect.center(),
            Align2::CENTER_CENTER,
            "Scoring frames\u{2026}",
            mono(12.5),
            p.faint,
        );
        return None;
    }

    let pts = curve.points(plot, plot.width().max(2.0) as usize);
    let thr_x = plot.left() + curve.threshold_x(kept) * plot.width();

    // Faint fill under the kept part.
    let mut left: Vec<Pos2> = Vec::new();
    for w in pts.windows(2) {
        let (a, b) = (w[0], w[1]);
        if a.x > thr_x {
            break;
        }
        if left.is_empty() {
            left.push(a);
        }
        if b.x <= thr_x {
            left.push(b);
        } else {
            let t = (thr_x - a.x) / (b.x - a.x);
            left.push(pos2(thr_x, a.y + (b.y - a.y) * t));
            break;
        }
    }
    if left.len() == 1 {
        left.push(pos2(thr_x.max(left[0].x), left[0].y));
    }
    let fill = Color32::from_rgba_unmultiplied(0xF5, 0xA5, 0x24, 34);
    let mut mesh = Mesh::default();
    for w in left.windows(2) {
        let base = mesh.vertices.len() as u32;
        for (pt, y) in [
            (w[0], w[0].y),
            (w[1], w[1].y),
            (w[1], plot.bottom()),
            (w[0], plot.bottom()),
        ] {
            mesh.colored_vertex(pos2(pt.x, y), fill);
        }
        mesh.add_triangle(base, base + 1, base + 2);
        mesh.add_triangle(base, base + 2, base + 3);
    }
    painter.add(Shape::mesh(mesh));

    painter.add(Shape::line(pts, Stroke::new(2.0, p.accent)));
    painter.extend(Shape::dashed_line(
        &[pos2(thr_x, plot.top() - 4.0), pos2(thr_x, plot.bottom())],
        Stroke::new(1.2, p.accent_hover),
        4.0,
        3.0,
    ));

    if let Some(rank) = current.and_then(|f| curve.rank_of(f)) {
        let c = curve.point(plot, rank);
        painter.circle(c, 5.5, Color32::WHITE, Stroke::new(2.0, p.raised));
    }

    // Hover read-out and click to jump.
    let mut jump = None;
    if let Some(pos) = resp
        .hover_pos()
        .filter(|q| plot.expand2(vec2(14.0, 12.0)).contains(*q))
    {
        let t = (pos.x - plot.left()) / plot.width();
        if let Some(rank) = curve.rank_at(t) {
            let c = curve.point(plot, rank);
            painter.line_segment(
                [pos2(c.x, plot.top()), pos2(c.x, plot.bottom())],
                Stroke::new(1.0, p.border_strong),
            );
            painter.circle_filled(c, 3.5, p.accent_hover);
            if let Some(frame) = curve.frame_at_rank(rank) {
                let q = curve.relative(frame).unwrap_or(0.0);
                let tip = format!(
                    "Rank {} \u{b7} frame {} \u{b7} quality {q:.2}",
                    rank + 1,
                    frame + 1
                );
                let resp2 = resp.clone().on_hover_text_at_pointer(tip);
                if resp2.clicked() {
                    jump = Some(frame);
                }
            }
        }
    }
    jump
}
