// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! The feedback page: a screenshot of the window to draw on, a kind, a title
//! and a description, and a button that files an issue through the
//! feedback relay. It replaces the viewer while open (Ctrl+Shift+F, or the
//! Feedback button in the top bar), at any step.
//!
//! Nothing is sent until Send is pressed. When the relay cannot be reached the
//! report can be saved as a `.zip` or opened as a pre-filled new-issue page in
//! the browser. The protocol and the client live in `helia-feedback`.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::{Duration, Instant};

use eframe::egui::{
    self, Color32, ColorImage, Context, Frame, Margin, Rect, RichText, ScrollArea, Sense, Stroke,
    StrokeKind, TextureHandle, TextureOptions, Ui, pos2, vec2,
};
use helia_core::about;
use helia_feedback::client::{FeedbackError, Phase, ProofOfWork, Relay, submit};
use helia_feedback::fallback;
use helia_feedback::protocol::{
    AppInfo, Created, FORGE_URL, Highlight, Kind as FbKind, MAX_DESCRIPTION, MAX_HIGHLIGHTS,
    MAX_TITLE, Report, SENT_WARNING,
};
use helia_feedback::screenshot::Screenshot;
use helia_feedback::sysinfo;

use crate::activity::{TaskKind, ToastKind};
use crate::app::App;
use crate::theme::{RADIUS_LARGE, mono, palette, semibold};
use crate::widgets::{self, Kind};

/// Room kept in the description for the system-details block.
const USER_DESCRIPTION_LIMIT: usize = MAX_DESCRIPTION - 1500;
/// How long to wait for the window screenshot before going on without one.
const CAPTURE_TIMEOUT: Duration = Duration::from_secs(3);
/// The shown image is at most this wide, in points.
const IMAGE_MAX_WIDTH: f32 = 820.0;

/// What the sender may choose to include about their setup.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SystemDetails {
    /// The build.
    pub app: AppInfo,
    /// The operating system.
    pub os: String,
    /// The open capture, as its file name and header summary; never the path
    /// and never any image data.
    pub capture: Option<String>,
}

/// Where the page is.
#[derive(Clone, Debug)]
pub enum Stage {
    /// Closed.
    Idle,
    /// Waiting for the window screenshot (the page is not shown yet, so it is
    /// not in the picture).
    Capturing(Instant),
    /// Writing the report.
    Editing,
    /// A send is under way.
    Sending(Phase),
    /// The relay filed the issue.
    Sent(Created),
    /// The send failed.
    Failed {
        /// In words.
        message: String,
        /// Offer saving and the browser.
        fallbacks: bool,
    },
}

/// What a press on the page asks the app to do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    /// Send the report.
    Send,
    /// Stop a send in progress.
    Cancel,
    /// Leave the page.
    Close,
    /// Save the report as a `.zip`.
    Save,
    /// Open the pre-filled new-issue page in the browser.
    OpenBrowser,
    /// Back to editing after a failure.
    Edit,
}

/// Messages from the worker threads.
enum Msg {
    Phase(u64, Phase),
    Done(u64, Result<Created, FeedbackError>),
    Saved(Result<PathBuf, String>),
}

/// State of the feedback page.
pub struct Feedback {
    /// The page is showing.
    pub open: bool,
    /// Progress through the flow.
    pub stage: Stage,
    shot: Option<Screenshot>,
    tex: Option<TextureHandle>,
    /// Rectangles drawn over the screenshot, in its pixels.
    pub highlights: Vec<Highlight>,
    drag: Option<(f32, f32)>,
    /// Bug, feature request, question or other.
    pub kind: FbKind,
    /// One line.
    pub title: String,
    /// The user's words.
    pub description: String,
    /// Attach the build, OS and capture summary.
    pub include_system: bool,
    /// Attach the screenshot.
    pub include_shot: bool,
    /// Show exactly what will be sent.
    pub preview: bool,
    /// Gathered when the page opens.
    pub details: Option<SystemDetails>,
    /// Relay address from the settings file (the environment wins).
    pub relay_setting: Option<String>,
    /// Message about the last save.
    pub note: Option<String>,
    tx: Sender<Msg>,
    rx: Receiver<Msg>,
    job: u64,
    cancel: Arc<AtomicBool>,
}

impl Default for Feedback {
    fn default() -> Self {
        let (tx, rx) = channel();
        Self {
            open: false,
            stage: Stage::Idle,
            shot: None,
            tex: None,
            highlights: Vec::new(),
            drag: None,
            kind: FbKind::Bug,
            title: String::new(),
            description: String::new(),
            include_system: true,
            include_shot: true,
            preview: false,
            details: None,
            relay_setting: None,
            note: None,
            tx,
            rx,
            job: 0,
            cancel: Arc::new(AtomicBool::new(false)),
        }
    }
}

/// The image-pixel position under `pos` when the image fills `rect`.
#[must_use]
pub fn to_image_px(pos: egui::Pos2, rect: Rect, size: [usize; 2]) -> (f32, f32) {
    let fx = (pos.x - rect.left()) / rect.width().max(1.0);
    let fy = (pos.y - rect.top()) / rect.height().max(1.0);
    (fx * size[0] as f32, fy * size[1] as f32)
}

/// The inverse: where image pixel `(x, y)` lies in `rect`.
#[must_use]
pub fn from_image_px(x: f32, y: f32, rect: Rect, size: [usize; 2]) -> egui::Pos2 {
    pos2(
        rect.left() + x / size[0] as f32 * rect.width(),
        rect.top() + y / size[1] as f32 * rect.height(),
    )
}

impl Feedback {
    /// Start waiting for the window screenshot.
    pub fn begin_capture(&mut self, now: Instant) {
        if matches!(self.stage, Stage::Sending(_)) || self.open {
            return;
        }
        self.stage = Stage::Capturing(now);
    }

    /// Show the page with `shot` (or without a screenshot) and `details`.
    pub fn open_with(&mut self, shot: Option<Screenshot>, details: SystemDetails) {
        self.include_shot = shot.is_some();
        self.shot = shot;
        self.tex = None;
        self.highlights.clear();
        self.drag = None;
        self.details = Some(details);
        self.preview = false;
        self.note = None;
        self.stage = Stage::Editing;
        self.open = true;
    }

    /// The report as it would be sent (without the proof of work).
    #[must_use]
    pub fn report(&self) -> Report {
        let sys = self
            .include_system
            .then_some(self.details.as_ref())
            .flatten();
        let mut description = self.description.trim().to_owned();
        if let Some(capture) = sys.and_then(|d| d.capture.as_deref()) {
            if !description.is_empty() {
                description.push_str("\n\n");
            }
            description.push_str("Open capture: ");
            description.push_str(capture);
        }
        Report {
            kind: self.kind,
            title: self.title.trim().to_owned(),
            description,
            app: sys.map(|d| d.app.clone()),
            os: sys.map(|d| d.os.clone()),
            highlights: if self.include_shot && self.shot.is_some() {
                self.highlights.clone()
            } else {
                Vec::new()
            },
            pow: None,
        }
    }

    /// The screenshot that would be sent.
    #[must_use]
    pub fn screenshot(&self) -> Option<&Screenshot> {
        self.shot.as_ref().filter(|_| self.include_shot)
    }

    /// Can Send be pressed?
    #[must_use]
    pub fn can_send(&self) -> bool {
        matches!(self.stage, Stage::Editing) && self.report().validate().is_ok()
    }

    /// Remove the last rectangle.
    pub fn undo(&mut self) {
        self.highlights.pop();
    }

    /// Remove every rectangle.
    pub fn clear(&mut self) {
        self.highlights.clear();
    }

    fn texture(&mut self, ctx: &Context) -> Option<TextureHandle> {
        if self.tex.is_none() {
            let s = self.shot.as_ref()?;
            let img =
                ColorImage::from_rgba_unmultiplied([s.width as usize, s.height as usize], &s.rgba);
            self.tex = Some(ctx.load_texture("feedback-shot", img, TextureOptions::LINEAR));
        }
        self.tex.clone()
    }

    /// Draw the page; returns what was pressed.
    pub fn page(&mut self, ui: &mut Ui) -> Vec<Action> {
        let p = palette();
        let mut actions = Vec::new();
        let sending = matches!(self.stage, Stage::Sending(_));
        let editable = matches!(self.stage, Stage::Editing);
        ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.set_max_width(ui.available_width().min(IMAGE_MAX_WIDTH));
                ui.spacing_mut().item_spacing.y = 10.0;
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new("Send feedback")
                            .font(semibold(22.0))
                            .color(p.text),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if widgets::button(ui, "Close", Kind::Outline, !sending, 0.0)
                            .on_hover_text("Leave without sending (Esc)")
                            .clicked()
                        {
                            actions.push(Action::Close);
                        }
                    });
                });
                warning(ui, SENT_WARNING);

                match self.stage.clone() {
                    Stage::Sent(created) => {
                        Self::sent(ui, &created, &mut actions);
                        return;
                    }
                    Stage::Failed { message, fallbacks } => {
                        self.failed(ui, &message, fallbacks, &mut actions);
                    }
                    _ => {}
                }

                ui.add_enabled_ui(editable, |ui| self.editor(ui));

                if sending {
                    ui.horizontal(|ui| {
                        ui.add(egui::Spinner::new().size(16.0).color(p.accent));
                        let text = match self.stage {
                            Stage::Sending(Phase::Preparing) => "Preparing your feedback\u{2026}",
                            _ => "Sending your feedback\u{2026}",
                        };
                        ui.label(RichText::new(text).color(p.text));
                        if widgets::button(ui, "Cancel", Kind::Outline, true, 0.0).clicked() {
                            actions.push(Action::Cancel);
                        }
                    });
                } else if editable {
                    ui.horizontal(|ui| {
                        let ok = self.can_send();
                        if widgets::button(ui, "Send", Kind::Primary, ok, 120.0)
                            .on_hover_text("File this in the Helia issue tracker")
                            .clicked()
                        {
                            actions.push(Action::Send);
                        }
                        if !ok {
                            ui.label(RichText::new("Add a title to send.").color(p.muted));
                        }
                    });
                }
            });
        actions
    }

    fn editor(&mut self, ui: &mut Ui) {
        let p = palette();
        if self.shot.is_some() {
            ui.checkbox(&mut self.include_shot, "Include the screenshot");
        }
        if self.include_shot && self.shot.is_some() {
            self.screenshot_view(ui);
        }

        widgets::overline(ui, "What kind of feedback");
        ui.horizontal_wrapped(|ui| {
            for k in FbKind::ALL {
                if widgets::chip(ui, k.label(), self.kind == k, true).clicked() {
                    self.kind = k;
                }
            }
        });

        widgets::overline(ui, "Title");
        ui.add(
            egui::TextEdit::singleline(&mut self.title)
                .char_limit(MAX_TITLE)
                .hint_text("One line that says what it is")
                .desired_width(f32::INFINITY),
        );
        widgets::overline(ui, "Description");
        ui.add(
            egui::TextEdit::multiline(&mut self.description)
                .char_limit(USER_DESCRIPTION_LIMIT)
                .hint_text(match self.kind {
                    FbKind::Bug => "What you did, what you expected, what happened instead",
                    FbKind::Feature => "What you would like Helia to do, and why",
                    FbKind::Question => "What you are trying to do",
                    FbKind::Other => "Tell us",
                })
                .desired_rows(6)
                .desired_width(f32::INFINITY),
        );
        ui.label(
            RichText::new(format!(
                "{} / {USER_DESCRIPTION_LIMIT}",
                self.description.chars().count()
            ))
            .font(mono(11.5))
            .color(p.faint),
        );

        ui.checkbox(&mut self.include_system, "Include system details");
        ui.label(
            RichText::new(
                "The Helia version, commit, build date and target, the Rust and wgpu versions, \
                 your GPU adapter, your operating system, and the open capture's file name and \
                 header summary (size, bit depth, frame count). Never image data, and never a \
                 folder path.",
            )
            .color(p.muted),
        );

        if widgets::chip(ui, "Preview what will be sent", self.preview, true).clicked() {
            self.preview = !self.preview;
        }
        if self.preview {
            self.preview_view(ui);
        }
    }

    /// The screenshot with the highlight tool.
    fn screenshot_view(&mut self, ui: &mut Ui) {
        let p = palette();
        let Some(tex) = self.texture(ui.ctx()) else {
            return;
        };
        let size = tex.size();
        ui.label(
            RichText::new(
                "Drag on the picture to draw a rectangle around the problem. Draw as many as \
                 you need.",
            )
            .color(p.muted),
        );
        let width = ui.available_width().min(IMAGE_MAX_WIDTH);
        let height = width * size[1] as f32 / size[0] as f32;
        let (rect, resp) = ui.allocate_exact_size(vec2(width, height), Sense::click_and_drag());
        resp.widget_info(|| {
            egui::WidgetInfo::labeled(
                egui::WidgetType::Other,
                true,
                "Screenshot of the Helia window: drag to highlight the problem",
            )
        });
        ui.painter().image(
            tex.id(),
            rect,
            Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
            Color32::WHITE,
        );
        ui.painter().rect_stroke(
            rect,
            0.0,
            Stroke::new(1.0, p.border_strong),
            StrokeKind::Outside,
        );
        if resp.hovered() {
            ui.output_mut(|o| o.cursor_icon = egui::CursorIcon::Crosshair);
        }
        if resp.drag_started()
            && let Some(pos) = resp.interact_pointer_pos()
        {
            self.drag = Some(to_image_px(pos, rect, size));
        }
        let live = match (self.drag, resp.interact_pointer_pos()) {
            (Some(start), Some(pos)) if resp.dragged() => {
                Some((start, to_image_px(pos, rect, size)))
            }
            _ => None,
        };
        if resp.drag_stopped()
            && let (Some(start), Some(pos)) = (self.drag.take(), resp.interact_pointer_pos())
            && self.highlights.len() < MAX_HIGHLIGHTS
            && let Some(h) = Highlight::from_corners(
                start,
                to_image_px(pos, rect, size),
                size[0] as u32,
                size[1] as u32,
            )
        {
            self.highlights.push(h);
        }
        paint_highlights(ui, rect, size, &self.highlights, live);
        ui.horizontal(|ui| {
            if widgets::chip(ui, "Undo", false, !self.highlights.is_empty()).clicked() {
                self.undo();
            }
            if widgets::chip(ui, "Clear", false, !self.highlights.is_empty()).clicked() {
                self.clear();
            }
            ui.label(
                RichText::new(match self.highlights.len() {
                    0 => "Nothing highlighted yet".to_owned(),
                    1 => "1 area highlighted".to_owned(),
                    n => format!("{n} areas highlighted"),
                })
                .color(p.muted),
            );
        });
    }

    fn preview_view(&mut self, ui: &mut Ui) {
        let p = palette();
        let report = self.report();
        Frame::new()
            .fill(p.panel)
            .stroke(Stroke::new(1.0, p.border))
            .corner_radius(RADIUS_LARGE)
            .inner_margin(Margin::same(14))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                widgets::overline(ui, "Report (JSON)");
                ui.add(
                    egui::Label::new(
                        RichText::new(report.preview_json())
                            .font(mono(12.0))
                            .color(p.muted),
                    )
                    .selectable(true),
                );
                ui.add_space(6.0);
                widgets::overline(ui, "Screenshot");
                if let (Some(tex), Some(_)) = (self.texture(ui.ctx()), self.screenshot()) {
                    let size = tex.size();
                    let width = ui.available_width().min(560.0);
                    let (rect, _) = ui.allocate_exact_size(
                        vec2(width, width * size[1] as f32 / size[0] as f32),
                        Sense::hover(),
                    );
                    ui.painter().image(
                        tex.id(),
                        rect,
                        Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
                        Color32::WHITE,
                    );
                    paint_highlights(ui, rect, size, &self.highlights, None);
                    ui.label(
                        RichText::new(
                            "Sent as a clean PNG, with the rectangles as numbers above; the \
                             service draws them.",
                        )
                        .color(p.muted),
                    );
                } else {
                    ui.label(RichText::new("No screenshot will be sent.").color(p.muted));
                }
            });
    }

    fn sent(ui: &mut Ui, created: &Created, actions: &mut Vec<Action>) {
        let p = palette();
        Frame::new()
            .fill(p.panel)
            .stroke(Stroke::new(1.0, p.border))
            .corner_radius(RADIUS_LARGE)
            .inner_margin(Margin::same(18))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.label(
                    RichText::new(format!(
                        "Thank you. Your feedback is issue #{}.",
                        created.issue_number
                    ))
                    .font(semibold(17.0))
                    .color(p.text),
                );
                ui.hyperlink_to(&created.issue_url, &created.issue_url);
                ui.label(
                    RichText::new("If you have an account on github.com you can follow it and add to it there.")
                        .color(p.muted),
                );
            });
        if widgets::button(ui, "Done", Kind::Primary, true, 120.0).clicked() {
            actions.push(Action::Close);
        }
    }

    fn failed(&mut self, ui: &mut Ui, message: &str, fallbacks: bool, actions: &mut Vec<Action>) {
        let p = palette();
        Frame::new()
            .fill(p.error_fill)
            .stroke(Stroke::new(1.0, p.error_border))
            .corner_radius(RADIUS_LARGE)
            .inner_margin(Margin::same(14))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.label(RichText::new(message).color(p.error_text));
                if fallbacks {
                    ui.label(
                        RichText::new(
                            "Nothing was lost. You can save the report and attach it to an issue \
                             yourself, or open the issue page with your text filled in (Open in browser \
                             only works if you have an account on github.com).",
                        )
                        .color(p.text),
                    );
                }
                ui.horizontal_wrapped(|ui| {
                    if fallbacks {
                        if widgets::button(ui, "Save report", Kind::Outline, true, 0.0)
                            .on_hover_text("A .zip with report.json and the screenshot")
                            .clicked()
                        {
                            actions.push(Action::Save);
                        }
                        if widgets::button(ui, "Open in browser", Kind::Outline, true, 0.0)
                            .on_hover_text(format!("Needs an account on github.com: {FORGE_URL}/issues/new"))
                            .clicked()
                        {
                            actions.push(Action::OpenBrowser);
                        }
                    }
                    if widgets::button(ui, "Edit and try again", Kind::Primary, true, 0.0).clicked()
                    {
                        actions.push(Action::Edit);
                    }
                });
                if let Some(n) = &self.note {
                    ui.label(RichText::new(n).color(p.success));
                }
            });
    }
}

fn warning(ui: &mut Ui, text: &str) {
    let p = palette();
    Frame::new()
        .fill(p.selected_fill)
        .stroke(Stroke::new(1.0, p.selected_border))
        .corner_radius(RADIUS_LARGE)
        .inner_margin(Margin::symmetric(14, 10))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.add(
                egui::Label::new(RichText::new(text).font(semibold(14.0)).color(p.accent)).wrap(),
            );
        });
}

fn paint_highlights(
    ui: &Ui,
    rect: Rect,
    size: [usize; 2],
    highlights: &[Highlight],
    live: Option<((f32, f32), (f32, f32))>,
) {
    let red = Color32::from_rgb(255, 48, 48);
    let draw = |a: (f32, f32), b: (f32, f32), stroke: f32| {
        let r = Rect::from_two_pos(
            from_image_px(a.0, a.1, rect, size),
            from_image_px(b.0, b.1, rect, size),
        );
        ui.painter().rect(
            r,
            0.0,
            Color32::from_rgba_unmultiplied(255, 48, 48, 28),
            Stroke::new(stroke, red),
            StrokeKind::Inside,
        );
    };
    for h in highlights {
        draw(
            (h.x as f32, h.y as f32),
            ((h.x + h.w) as f32, (h.y + h.h) as f32),
            2.5,
        );
    }
    if let Some((a, b)) = live {
        draw(a, b, 1.5);
    }
}

impl App {
    /// The details a sender may attach, gathered now.
    pub(crate) fn system_details(&self) -> SystemDetails {
        let b = about::build_info();
        SystemDetails {
            app: AppInfo {
                name: "helia".into(),
                version: crate::about::ui_version().into(),
                commit: b.commit_label(),
                target: b.target.into(),
                gpu: Some(self.gpu_description()),
            },
            os: sysinfo::os_description(),
            capture: self.capture.as_ref().map(|c| c.meta.fields().join(", ")),
        }
    }

    /// Start a feedback report: ask the window for a screenshot (the page opens
    /// when it arrives, so it is not in the picture).
    pub fn begin_feedback(&mut self, ctx: &Context) {
        if self.feedback.open || matches!(self.feedback.stage, Stage::Capturing(_)) {
            return;
        }
        self.show_about = false;
        self.feedback.relay_setting = self.prefs.feedback_url.clone();
        let now = self.now();
        self.feedback.begin_capture(now);
        ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::default()));
        ctx.request_repaint();
    }

    /// Leave the page. Whatever was typed is kept for the next time unless it
    /// was sent.
    pub fn close_feedback(&mut self) {
        if matches!(self.feedback.stage, Stage::Sent(_)) {
            let fb = &mut self.feedback;
            fb.title.clear();
            fb.description.clear();
            fb.highlights.clear();
        }
        self.feedback.open = false;
        self.feedback.stage = Stage::Idle;
        self.feedback.tex = None;
        self.feedback.shot = None;
    }

    /// Per frame: take the screenshot when it arrives, handle worker messages.
    pub(crate) fn feedback_pump(&mut self, ctx: &Context) {
        if let Stage::Capturing(since) = self.feedback.stage {
            let image = ctx.input(|i| {
                i.events.iter().find_map(|e| match e {
                    egui::Event::Screenshot { image, .. } => Some(Arc::clone(image)),
                    _ => None,
                })
            });
            if let Some(image) = image {
                let rgba: Vec<u8> = image.pixels.iter().flat_map(Color32::to_array).collect();
                let shot = Screenshot::new(image.size[0] as u32, image.size[1] as u32, rgba).ok();
                let details = self.system_details();
                self.feedback.open_with(shot, details);
            } else if self.now().saturating_duration_since(since) > CAPTURE_TIMEOUT {
                let details = self.system_details();
                self.feedback.open_with(None, details);
                self.toast(
                    ToastKind::Info,
                    "This system could not capture the window, so the report has no screenshot.",
                );
            } else {
                ctx.request_repaint_after(Duration::from_millis(50));
            }
        }
        while let Ok(msg) = self.feedback.rx.try_recv() {
            match msg {
                Msg::Phase(job, phase) if job == self.feedback.job => {
                    if matches!(self.feedback.stage, Stage::Sending(_)) {
                        self.feedback.stage = Stage::Sending(phase);
                        let now = self.now();
                        let kind = match phase {
                            Phase::Preparing => TaskKind::FeedbackPreparing,
                            Phase::Sending => TaskKind::FeedbackSending,
                        };
                        self.tasks.begin(kind, now, true);
                    }
                }
                Msg::Done(job, res) if job == self.feedback.job => {
                    self.tasks.end(TaskKind::FeedbackPreparing);
                    if !matches!(self.feedback.stage, Stage::Sending(_)) {
                        continue;
                    }
                    match res {
                        Ok(created) => {
                            self.toast(
                                ToastKind::Success,
                                format!("Feedback sent as issue #{}.", created.issue_number),
                            );
                            self.feedback.stage = Stage::Sent(created);
                        }
                        Err(FeedbackError::Cancelled) => self.feedback.stage = Stage::Editing,
                        Err(e) => {
                            self.feedback.stage = Stage::Failed {
                                message: e.to_string(),
                                fallbacks: e.offers_fallback(),
                            };
                        }
                    }
                }
                Msg::Phase(..) | Msg::Done(..) => {}
                Msg::Saved(Ok(path)) => {
                    self.feedback.note = Some(format!(
                        "Saved {}. Attach screenshot.png to the issue you open.",
                        path.display()
                    ));
                }
                Msg::Saved(Err(e)) => {
                    self.feedback.note = None;
                    self.toast(ToastKind::Error, format!("Could not save the report: {e}"));
                }
            }
        }
    }

    /// Carry out what the page asked for.
    pub(crate) fn feedback_act(&mut self, ctx: &Context, actions: Vec<Action>) {
        for a in actions {
            match a {
                Action::Close => self.close_feedback(),
                Action::Edit => {
                    self.feedback.stage = Stage::Editing;
                    self.feedback.note = None;
                }
                Action::Cancel => self.cancel_feedback(),
                Action::Send => self.send_feedback(ctx),
                Action::Save => self.save_feedback(ctx),
                Action::OpenBrowser => {
                    let note = self.feedback.screenshot().map(|_| fallback::ATTACH_NOTE);
                    let url = fallback::issue_url(&self.feedback.report(), note);
                    ctx.open_url(egui::OpenUrl::new_tab(url));
                }
            }
        }
    }

    /// Stop a send in progress.
    pub fn cancel_feedback(&mut self) {
        self.feedback.cancel.store(true, Ordering::Relaxed);
        self.feedback.job += 1;
        self.tasks.end(TaskKind::FeedbackPreparing);
        if matches!(self.feedback.stage, Stage::Sending(_)) {
            self.feedback.stage = Stage::Editing;
        }
    }

    fn send_feedback(&mut self, ctx: &Context) {
        if !self.feedback.can_send() {
            return;
        }
        let relay = match Relay::configured(self.feedback.relay_setting.as_deref()) {
            Ok(r) => r,
            Err(e) => {
                self.feedback.stage = Stage::Failed {
                    message: e.to_string(),
                    fallbacks: true,
                };
                return;
            }
        };
        let report = self.feedback.report();
        let shot = self.feedback.screenshot().cloned();
        self.feedback.job += 1;
        let job = self.feedback.job;
        self.feedback.cancel = Arc::new(AtomicBool::new(false));
        let cancel = Arc::clone(&self.feedback.cancel);
        let tx = self.feedback.tx.clone();
        let ctx = ctx.clone();
        self.feedback.stage = Stage::Sending(Phase::Preparing);
        let now = self.now();
        self.tasks.begin(TaskKind::FeedbackPreparing, now, true);
        std::thread::spawn(move || {
            let res = submit(
                &relay,
                &ProofOfWork,
                report,
                shot.as_ref(),
                &cancel,
                &mut |phase| {
                    let _ = tx.send(Msg::Phase(job, phase));
                    ctx.request_repaint();
                },
            );
            let _ = tx.send(Msg::Done(job, res));
            ctx.request_repaint();
        });
    }

    fn save_feedback(&mut self, ctx: &Context) {
        let report = self.feedback.report();
        let shot = self.feedback.screenshot().cloned();
        let tx = self.feedback.tx.clone();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let secs = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_secs());
            let Some(path) = rfd::FileDialog::new()
                .set_file_name(fallback::zip_name(secs))
                .add_filter("Zip archive", &["zip"])
                .save_file()
            else {
                return;
            };
            let res = fallback::build_zip(&report, shot.as_ref())
                .map_err(|e| e.to_string())
                .and_then(|bytes| std::fs::write(&path, bytes).map_err(|e| e.to_string()))
                .map(|()| path);
            let _ = tx.send(Msg::Saved(res));
            ctx.request_repaint();
        });
    }

    /// The page, in the centre panel.
    pub(crate) fn feedback_page(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        let actions = self.feedback.page(ui);
        self.feedback_act(&ctx, actions);
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "tests")]
    use super::*;

    pub(crate) fn details() -> SystemDetails {
        SystemDetails {
            app: AppInfo {
                name: "helia".into(),
                version: "0.3.0".into(),
                commit: "abc12345".into(),
                target: "x86_64-unknown-linux-gnu".into(),
                gpu: Some("Vulkan: Test GPU".into()),
            },
            os: "Test Linux 1.0 (x86_64)".into(),
            capture: Some("sun_Ha_001.ser, 384x288, 8-bit, 60 frames".into()),
        }
    }

    /// One frame that pumps feedback events; texture uploads are dropped.
    fn frame(ctx: &Context, raw: egui::RawInput, app: &mut App) {
        let mut out = ctx.run_ui(raw, |ui| app.feedback_pump(ui.ctx()));
        out.textures_delta.clear();
    }

    fn shot() -> Screenshot {
        Screenshot::new(100, 50, [20, 20, 20, 255].repeat(5000)).unwrap()
    }

    #[test]
    fn pointer_and_image_pixels_convert_both_ways() {
        let rect = Rect::from_min_size(pos2(10.0, 20.0), vec2(200.0, 100.0));
        let size = [400, 200];
        assert_eq!(to_image_px(pos2(110.0, 70.0), rect, size), (200.0, 100.0));
        assert_eq!(to_image_px(rect.min, rect, size), (0.0, 0.0));
        let back = from_image_px(200.0, 100.0, rect, size);
        assert_eq!(back, pos2(110.0, 70.0));
    }

    #[test]
    fn the_report_follows_the_checkboxes() {
        let mut fb = Feedback::default();
        fb.open_with(Some(shot()), details());
        fb.title = "  Slider jumps ".into();
        fb.description = "It jumps.".into();
        fb.kind = FbKind::Bug;
        fb.highlights.push(Highlight {
            x: 1,
            y: 2,
            w: 30,
            h: 20,
        });
        let r = fb.report();
        assert_eq!(r.title, "Slider jumps");
        assert_eq!(r.app.as_ref().unwrap().version, "0.3.0");
        assert_eq!(r.os.as_deref(), Some("Test Linux 1.0 (x86_64)"));
        assert!(
            r.description
                .ends_with("Open capture: sun_Ha_001.ser, 384x288, 8-bit, 60 frames")
        );
        assert!(!r.description.contains('/'), "no folder path");
        assert_eq!(r.highlights.len(), 1);

        fb.include_system = false;
        fb.include_shot = false;
        let r = fb.report();
        assert_eq!((r.app.as_ref(), r.os.as_ref()), (None, None));
        assert_eq!(r.description, "It jumps.");
        assert!(r.highlights.is_empty(), "no screenshot, no rectangles");
        assert!(fb.screenshot().is_none());
        assert_eq!(r.validate(), Ok(()));
    }

    #[test]
    fn send_needs_a_title_and_a_report_that_fits() {
        let mut fb = Feedback::default();
        assert!(!fb.can_send(), "closed");
        fb.open_with(None, details());
        assert!(!fb.can_send(), "no title");
        fb.title = "x".into();
        assert!(fb.can_send());
        fb.stage = Stage::Sending(Phase::Sending);
        assert!(!fb.can_send(), "already sending");
    }

    #[test]
    fn undo_and_clear_remove_rectangles() {
        let mut fb = Feedback::default();
        for i in 0..3 {
            fb.highlights.push(Highlight {
                x: i,
                y: 0,
                w: 5,
                h: 5,
            });
        }
        fb.undo();
        assert_eq!(fb.highlights.len(), 2);
        assert_eq!(fb.highlights[1].x, 1);
        fb.clear();
        assert_eq!(fb.highlights.len(), 0);
        fb.undo();
    }

    #[test]
    fn a_report_with_the_largest_user_text_still_fits_the_limit() {
        let mut fb = Feedback::default();
        fb.open_with(None, details());
        fb.title = "t".into();
        fb.description = "é".repeat(USER_DESCRIPTION_LIMIT);
        assert_eq!(fb.report().validate(), Ok(()));
    }

    #[test]
    fn the_page_opens_when_the_window_screenshot_arrives() {
        let ctx = Context::default();
        let mut app = App::with_ctx(&ctx, None, false);
        app.feedback.begin_capture(app.now());
        frame(&ctx, egui::RawInput::default(), &mut app);
        assert!(
            !app.feedback.open,
            "the page waits, so it is not in the picture"
        );
        assert!(matches!(app.feedback.stage, Stage::Capturing(_)));

        let mut raw = egui::RawInput::default();
        raw.events.push(egui::Event::Screenshot {
            viewport_id: egui::ViewportId::ROOT,
            user_data: egui::UserData::default(),
            image: Arc::new(ColorImage::filled([8, 6], Color32::BLUE)),
        });
        frame(&ctx, raw, &mut app);
        assert!(app.feedback.open);
        assert!(matches!(app.feedback.stage, Stage::Editing));
        let shot = app.feedback.screenshot().expect("the screenshot was kept");
        assert_eq!((shot.width, shot.height), (8, 6));
        assert_eq!(&shot.rgba[..4], &[0, 0, 255, 255]);
        assert!(app.feedback.details.is_some());
    }

    #[test]
    fn without_a_screenshot_the_page_still_opens_after_a_wait() {
        let ctx = Context::default();
        let mut app = App::with_ctx(&ctx, None, false);
        let start = Instant::now();
        app.frozen_now = Some(start);
        app.feedback.begin_capture(start);
        app.frozen_now = Some(start + CAPTURE_TIMEOUT + Duration::from_millis(1));
        frame(&ctx, egui::RawInput::default(), &mut app);
        assert!(app.feedback.open);
        assert!(app.feedback.screenshot().is_none());
        assert!(!app.feedback.include_shot);
        assert_eq!(app.toasts.list().len(), 1, "told why there is no picture");
    }

    #[test]
    fn closing_after_a_send_forgets_the_text_but_closing_otherwise_keeps_it() {
        let ctx = Context::default();
        let mut app = App::with_ctx(&ctx, None, false);
        app.feedback.open_with(None, details());
        app.feedback.title = "keep me".into();
        app.close_feedback();
        assert!(!app.feedback.open);
        assert_eq!(app.feedback.title, "keep me");
        app.feedback.open_with(None, details());
        app.feedback.stage = Stage::Sent(Created {
            issue_number: 5,
            issue_url: "https://x/5".into(),
        });
        app.close_feedback();
        assert_eq!(app.feedback.title, "");
    }

    #[test]
    fn the_warning_names_the_public_issue_list() {
        assert!(SENT_WARNING.contains("sent to the Helia developers"));
        assert!(SENT_WARNING.contains("issue tracker"));
        assert!(SENT_WARNING.contains("nothing private"));
    }
}
