// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Plugin steps in the pipeline: the card in the Stack step where steps and a stack backend are
//! chosen, the plan a run takes to the worker, and the numbers and shapes an `analyse` step
//! reports.
//!
//! In the desktop app every `image-op` step runs on the stack right after stacking, before the
//! sharpening and edits of the later steps (the command line's `pre:` position). The command line
//! can also run steps after the edits.

use std::sync::{Arc, Mutex};

use eframe::egui::{self, Painter, Pos2, Rect, RichText, Stroke, Ui, vec2};
use helia_plugin::{Analysis, Runner, StepSpec, StepStage};
use serde_json::Value;

use crate::app::App;
use crate::theme::{mono, palette, semibold};
use crate::widgets::{self, Kind};

/// One plugin step chosen for the pipeline.
#[derive(Clone, Debug, PartialEq)]
pub struct UiStep {
    /// Plugin name.
    pub plugin: String,
    /// Step id.
    pub step: String,
    /// The values of its parameter form.
    pub params: Value,
}

/// The plugin choices of the pipeline and what the last run reported.
#[derive(Default)]
pub struct PluginPipeline {
    /// Steps, in the order they run.
    pub steps: Vec<UiStep>,
    /// A `stack-backend` plugin to stack with, instead of this computer.
    pub backend: Option<String>,
    /// What the `analyse` steps of the last run reported.
    pub results: Vec<(StepSpec, Analysis)>,
    /// Draw the shapes the steps reported on the stack.
    pub draw: bool,
    /// What a plugin said while a run was going ("Uploading 40%").
    pub status: Option<String>,
    /// Lines for the history of an exported file about the plugins that made the stack.
    pub provenance: Vec<String>,
    /// FITS cards for the plugins that made the stack.
    pub cards: Vec<helia_core::io::fits::HeaderCard>,
}

/// What a stack run takes to the worker thread.
pub struct PluginPlan {
    /// The plugins, started once and kept for the run.
    pub runner: Arc<Mutex<Runner>>,
    /// The steps in order.
    pub specs: Vec<StepSpec>,
    /// The stack-backend plugin, if one was chosen.
    pub backend: Option<String>,
    /// The filter id, for the backend's job.
    pub filter: Option<String>,
}

/// A plugin that is on: its name, its steps (id, capability, title) and whether it can stack.
type EnabledPlugin = (String, Vec<(String, String, String)>, bool);

/// What kind of step a capability is, in words.
fn kind_words(cap: &str) -> &'static str {
    match cap {
        "frame-op" => "on every frame",
        "image-op" => "on the stack",
        _ => "measures the stack",
    }
}

impl App {
    /// The steps and the backend as a plan for the worker, checked first.
    ///
    /// # Errors
    /// A sentence in the What / Why / What-to-do form when a plugin is off, changed or out of range, or a
    /// setting cannot be used with plugins.
    pub fn plugin_plan(&self) -> Result<Option<PluginPlan>, String> {
        let pp = &self.plugin_pipeline;
        if pp.steps.is_empty() && pp.backend.is_none() {
            return Ok(None);
        }
        let specs: Vec<StepSpec> = pp
            .steps
            .iter()
            .map(|s| StepSpec {
                plugin: s.plugin.clone(),
                step: s.step.clone(),
                params: s.params.clone(),
                stage: StepStage::PreEdit,
            })
            .collect();
        let runner = Runner::new(self.plugins.registry.clone());
        runner.check_all(&specs).map_err(|e| e.to_string())?;
        if let Some(b) = &pp.backend {
            let l = runner.registry.runnable(b).map_err(|e| e.to_string())?;
            if !l.has("stack-backend") {
                return Err(format!(
                    "The plugin {b} cannot stack. It does not offer the stack-backend capability. \
                     What to do: choose this computer, or another plugin."
                ));
            }
        }
        let one_channel_only = specs.iter().any(|s| {
            runner
                .check(s)
                .is_ok_and(|c| c == "image-op" || c == "analyse")
        });
        if self.settings.true_colour && one_channel_only {
            return Err(
                "A true-colour stack cannot go through a plugin image step. Those steps work on \
                 one-channel pictures. What to do: turn true colour off, or remove the plugin step."
                    .into(),
            );
        }
        Ok(Some(PluginPlan {
            runner: Arc::new(Mutex::new(runner)),
            specs,
            backend: pp.backend.clone(),
            filter: self.settings.preset.map(|p| p.id().to_owned()),
        }))
    }

    /// The card in the Stack step: where to stack, the plugin steps and what they reported.
    pub(crate) fn plugin_card(&mut self, ui: &mut Ui) {
        let p = palette();
        let idle = self.run.is_none();
        let enabled: Vec<EnabledPlugin> = self
            .plugins
            .enabled()
            .iter()
            .map(|l| {
                (
                    l.manifest.name.clone(),
                    l.steps
                        .iter()
                        .map(|s| {
                            (
                                s.step.id.clone(),
                                s.step.capability.clone(),
                                s.step.title.clone(),
                            )
                        })
                        .collect(),
                    l.has("stack-backend"),
                )
            })
            .collect();
        widgets::overline(ui, "Plugins");
        if enabled.is_empty() && self.plugin_pipeline.steps.is_empty() {
            ui.add(
                egui::Label::new(
                    RichText::new(
                        "No plugin is switched on. Plugins add steps or stack somewhere else.",
                    )
                    .color(p.muted),
                )
                .wrap(),
            );
            if widgets::button(ui, "Plugins\u{2026}", Kind::Outline, true, 0.0)
                .on_hover_text("Open the Plugins page")
                .clicked()
            {
                self.open_plugins();
            }
            return;
        }
        let backends: Vec<&String> = enabled
            .iter()
            .filter(|(_, _, b)| *b)
            .map(|(n, _, _)| n)
            .collect();
        if !backends.is_empty() {
            ui.add(egui::Label::new(
                RichText::new("Stack on").font(semibold(13.0)).color(p.text),
            ));
            ui.horizontal_wrapped(|ui| {
                if widgets::chip(
                    ui,
                    "This computer",
                    self.plugin_pipeline.backend.is_none(),
                    idle,
                )
                .clicked()
                    && idle
                {
                    self.plugin_pipeline.backend = None;
                }
                for b in backends {
                    let on = self.plugin_pipeline.backend.as_deref() == Some(b.as_str());
                    if widgets::chip(ui, b, on, idle)
                        .on_hover_text("Stack with this plugin instead of on this computer")
                        .clicked()
                        && idle
                    {
                        self.plugin_pipeline.backend = Some(b.clone());
                    }
                }
            });
        }
        let mut steps = std::mem::take(&mut self.plugin_pipeline.steps);
        let mut remove = None;
        for (i, s) in steps.iter_mut().enumerate() {
            let loaded = self.plugins.registry.get(&s.plugin);
            let step = loaded.and_then(|l| l.step(&s.step));
            let cap = step.map_or("", |st| st.step.capability.as_str());
            ui.horizontal(|ui| {
                ui.add(egui::Label::new(
                    RichText::new(format!("{}/{}", s.plugin, s.step))
                        .font(mono(12.5))
                        .color(p.text),
                ));
                widgets::badge(ui, kind_words(cap), "Where this step runs");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if idle && widgets::button(ui, "Remove", Kind::Outline, true, 0.0).clicked() {
                        remove = Some(i);
                    }
                });
            });
            if let Some(st) = step
                && !st.params.fields.is_empty()
            {
                ui.add_enabled_ui(idle, |ui| {
                    crate::plugins_page::schema_form(
                        ui,
                        &format!("step-{i}-{}-{}", s.plugin, s.step),
                        &st.params,
                        &mut s.params,
                    );
                });
            }
        }
        if let Some(i) = remove {
            steps.remove(i);
        }
        self.plugin_pipeline.steps = steps;
        let catalog: Vec<(String, String, String, String)> = enabled
            .iter()
            .flat_map(|(n, st, _)| {
                st.iter()
                    .map(|(id, cap, title)| (n.clone(), id.clone(), cap.clone(), title.clone()))
            })
            .collect();
        if !catalog.is_empty() {
            ui.add_enabled_ui(idle, |ui| {
                ui.menu_button("Add a plugin step", |ui| {
                    for (plugin, id, cap, title) in &catalog {
                        let label = format!(
                            "{plugin}/{id}  ({})",
                            if title.is_empty() {
                                kind_words(cap)
                            } else {
                                title.as_str()
                            }
                        );
                        if ui.button(label).clicked() {
                            let params = self
                                .plugins
                                .registry
                                .get(plugin)
                                .and_then(|l| l.step(id))
                                .map_or(Value::Null, |s| s.params.defaults());
                            self.plugin_pipeline.steps.push(UiStep {
                                plugin: plugin.clone(),
                                step: id.clone(),
                                params,
                            });
                            ui.close();
                        }
                    }
                });
            });
        }
        if widgets::button(ui, "Plugins\u{2026}", Kind::Outline, true, 0.0)
            .on_hover_text("Open the Plugins page")
            .clicked()
        {
            self.open_plugins();
        }
        self.plugin_results_panel(ui);
    }

    fn plugin_results_panel(&mut self, ui: &mut Ui) {
        let p = palette();
        if self.plugin_pipeline.results.is_empty() || self.run.is_some() {
            return;
        }
        widgets::overline(ui, "Plugin results");
        for (spec, a) in &self.plugin_pipeline.results {
            ui.add(egui::Label::new(
                RichText::new(format!("{}/{}", spec.plugin, spec.step))
                    .font(semibold(13.0))
                    .color(p.text),
            ));
            for (k, v) in &a.metrics {
                ui.add(
                    egui::Label::new(
                        RichText::new(format!("{k}: {v:.6}"))
                            .font(mono(12.0))
                            .color(p.muted),
                    )
                    .selectable(true),
                );
            }
            for n in &a.notes {
                ui.add(egui::Label::new(RichText::new(n).font(mono(12.0)).color(p.muted)).wrap());
            }
        }
        if self
            .plugin_pipeline
            .results
            .iter()
            .any(|(_, a)| !a.overlays.is_empty())
        {
            ui.checkbox(
                &mut self.plugin_pipeline.draw,
                "Draw the shapes on the stack",
            );
        }
    }

    /// Open the Plugins page.
    pub(crate) fn open_plugins(&mut self) {
        self.show_plugins = true;
        self.show_about = false;
        self.feedback.open = false;
    }

    /// Draw the shapes the `analyse` steps reported over the stack. `image_rect` is where the
    /// picture is on screen and `clip` the viewer; the shapes are in pixels of the stack.
    pub(crate) fn paint_plugin_overlays(&self, painter: &Painter, image_rect: Rect, clip: Rect) {
        if !self.plugin_pipeline.draw {
            return;
        }
        let p = palette();
        let stroke = Stroke::new(1.5, p.accent);
        let z = self.view.zoom;
        let at = |x: f64, y: f64| -> Pos2 { image_rect.min + vec2(x as f32 * z, y as f32 * z) };
        let painter = painter.with_clip_rect(clip);
        for (_, a) in &self.plugin_pipeline.results {
            for o in &a.overlays {
                let num = |k: &str| o.get(k).and_then(Value::as_f64);
                let label = o.get("label").and_then(Value::as_str);
                let anchor = match o.get("kind").and_then(Value::as_str) {
                    Some("circle") => {
                        if let (Some(x), Some(y), Some(r)) = (num("x"), num("y"), num("r")) {
                            painter.circle_stroke(at(x, y), r as f32 * z, stroke);
                            Some(at(x, y - r))
                        } else {
                            None
                        }
                    }
                    Some("rect") => {
                        if let (Some(x), Some(y), Some(w), Some(h)) =
                            (num("x"), num("y"), num("w"), num("h"))
                        {
                            painter.rect_stroke(
                                Rect::from_min_max(at(x, y), at(x + w, y + h)),
                                0.0,
                                stroke,
                                egui::StrokeKind::Middle,
                            );
                            Some(at(x, y))
                        } else {
                            None
                        }
                    }
                    Some("line") => {
                        if let (Some(x1), Some(y1), Some(x2), Some(y2)) =
                            (num("x1"), num("y1"), num("x2"), num("y2"))
                        {
                            painter.line_segment([at(x1, y1), at(x2, y2)], stroke);
                            Some(at(x1, y1))
                        } else {
                            None
                        }
                    }
                    Some("points") => {
                        if let Some(list) = o.get("points").and_then(Value::as_array) {
                            for pt in list {
                                if let Some([x, y]) = pt.as_array().map(Vec::as_slice)
                                    && let (Some(x), Some(y)) = (x.as_f64(), y.as_f64())
                                {
                                    painter.circle_filled(at(x, y), 2.5, p.accent);
                                }
                            }
                        }
                        None
                    }
                    Some("text") => {
                        if let (Some(x), Some(y), Some(t)) =
                            (num("x"), num("y"), o.get("text").and_then(Value::as_str))
                        {
                            painter.text(at(x, y), egui::Align2::LEFT_TOP, t, mono(12.0), p.accent);
                        }
                        None
                    }
                    _ => None,
                };
                if let (Some(pos), Some(text)) = (anchor, label) {
                    painter.text(
                        pos + vec2(2.0, -2.0),
                        egui::Align2::LEFT_BOTTOM,
                        text,
                        mono(11.5),
                        p.accent,
                    );
                }
            }
        }
    }
}
