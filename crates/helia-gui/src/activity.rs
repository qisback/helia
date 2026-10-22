// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! What the application is doing, and what it has just done.
//!
//! [`Tasks`] tracks the long operations (opening, scoring, stacking,
//! colouring, writing) so the window can always say what it is busy with:
//! the stage in plain words, a progress bar when the fraction is known, a time
//! estimate, and whether it can be cancelled. [`Toasts`] holds the short
//! messages that report an outcome: successes fade, errors stay until
//! dismissed. Neither touches egui, so both are unit tested; the drawing lives
//! in `notices.rs`.

use std::time::{Duration, Instant};

use helia_core::pipeline::Stage;

/// Work shorter than this is not announced, so quick operations do not flash.
pub const NOTICE_DELAY: Duration = Duration::from_millis(150);

/// A long operation the user should be told about.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TaskKind {
    /// Opening a capture and indexing its frames.
    Opening,
    /// Converting a video to a capture with ffmpeg.
    Transcoding,
    /// Decoding a frame for the viewer.
    Decoding,
    /// Scoring every frame.
    Scoring,
    /// The stacking pipeline, in the given stage.
    Stack(Stage),
    /// Sharpening, adjusting and colouring the downsized preview.
    Previewing,
    /// The same at full size (the preview is set to full resolution).
    PreviewingFull,
    /// Writing the result to disk.
    Exporting,
    /// Fetching the feedback challenge and solving the proof of work.
    FeedbackPreparing,
    /// Uploading a feedback report.
    FeedbackSending,
}

impl TaskKind {
    /// Tasks of the same key replace each other; the stages of one stack share
    /// a key so the notice follows the run through its stages.
    fn key(self) -> u8 {
        match self {
            Self::Opening => 0,
            Self::Transcoding => 1,
            Self::Decoding => 2,
            Self::Scoring => 3,
            Self::Stack(_) => 4,
            Self::Previewing => 5,
            Self::Exporting => 6,
            Self::PreviewingFull => 7,
            Self::FeedbackPreparing | Self::FeedbackSending => 8,
        }
    }

    /// The stage in plain words. `frames` is the capture's frame count.
    #[must_use]
    pub fn label(self, frames: usize) -> String {
        match self {
            Self::Opening => "Opening the file and indexing its frames\u{2026}".into(),
            Self::Transcoding => "Converting the video with ffmpeg\u{2026}".into(),
            Self::Decoding => "Decoding the frame\u{2026}".into(),
            Self::Scoring if frames > 0 => format!("Scoring {} frames\u{2026}", group(frames)),
            Self::Scoring => "Scoring the frames\u{2026}".into(),
            Self::Stack(Stage::Quality) if frames > 0 => {
                format!("Scoring {} frames\u{2026}", group(frames))
            }
            Self::Stack(Stage::Quality) => "Scoring the frames\u{2026}".into(),
            Self::Stack(Stage::Reference) => "Building the reference image\u{2026}".into(),
            Self::Stack(Stage::GlobalAlign) => "Aligning the frames\u{2026}".into(),
            Self::Stack(Stage::LocalAlign) => "Measuring the alignment points\u{2026}".into(),
            Self::Stack(Stage::Stack) => "Stacking the best frames\u{2026}".into(),
            Self::Stack(Stage::Finish) => "Finishing the image\u{2026}".into(),
            Self::Previewing => "Sharpening and colouring the preview\u{2026}".into(),
            Self::PreviewingFull => "Sharpening and colouring the full-size image\u{2026}".into(),
            Self::Exporting => "Applying the edits and writing the file\u{2026}".into(),
            Self::FeedbackPreparing => "Preparing your feedback\u{2026}".into(),
            Self::FeedbackSending => "Sending your feedback\u{2026}".into(),
        }
    }
}

/// One running operation.
#[derive(Clone, Debug)]
pub struct Task {
    /// What it is.
    pub kind: TaskKind,
    /// When it began.
    pub started: Instant,
    /// Progress in `0..=1` when known.
    pub fraction: Option<f32>,
    /// A Cancel button makes sense.
    pub cancellable: bool,
}

impl Task {
    /// Expected time left, once enough is done to say. Within the first few
    /// percent or the first second the estimate would be noise.
    #[must_use]
    pub fn eta(&self, now: Instant) -> Option<Duration> {
        let f = self.fraction?;
        let elapsed = now.saturating_duration_since(self.started);
        if !(0.05..1.0).contains(&f) || elapsed < Duration::from_secs(1) {
            return None;
        }
        Some(elapsed.mul_f32((1.0 - f) / f))
    }

    /// The line under the notice: percentage, count and time left.
    #[must_use]
    pub fn detail(&self, now: Instant, frames: usize) -> String {
        let Some(f) = self.fraction else {
            return "Working\u{2026}".into();
        };
        let mut parts = vec![format!("{:.0}%", f * 100.0)];
        if matches!(self.kind, TaskKind::Scoring) && frames > 0 {
            parts.push(format!(
                "{} of {} frames",
                group((f * frames as f32).round() as usize),
                group(frames)
            ));
        }
        if let Some(eta) = self.eta(now) {
            parts.push(format!("about {} left", coarse(eta)));
        }
        parts.join(" \u{b7} ")
    }

    /// Text for screen readers: stage, progress and time left in one sentence.
    #[must_use]
    pub fn spoken(&self, now: Instant, frames: usize) -> String {
        format!(
            "{} {}",
            self.kind.label(frames).trim_end_matches('\u{2026}'),
            self.detail(now, frames)
        )
    }
}

/// The set of running operations.
#[derive(Clone, Debug, Default)]
pub struct Tasks {
    list: Vec<Task>,
}

impl Tasks {
    /// Start `kind`, or restart the running task with the same key.
    pub fn begin(&mut self, kind: TaskKind, now: Instant, cancellable: bool) {
        self.list.retain(|t| t.kind.key() != kind.key());
        self.list.push(Task {
            kind,
            started: now,
            fraction: None,
            cancellable,
        });
    }

    /// Report progress. A task that was not begun is begun now, so a worker
    /// that only sends progress still gets a notice. `kind` may name a new
    /// stage of the same task.
    pub fn update(&mut self, kind: TaskKind, fraction: Option<f32>, now: Instant) {
        if let Some(t) = self.list.iter_mut().find(|t| t.kind.key() == kind.key()) {
            t.kind = kind;
            t.fraction = fraction.map(|f| f.clamp(0.0, 1.0));
        } else {
            self.begin(kind, now, false);
            self.update(kind, fraction, now);
        }
    }

    /// Stop the task of `kind`'s key.
    pub fn end(&mut self, kind: TaskKind) {
        self.list.retain(|t| t.kind.key() != kind.key());
    }

    /// The running task of `kind`'s key.
    #[must_use]
    pub fn get(&self, kind: TaskKind) -> Option<&Task> {
        self.list.iter().find(|t| t.kind.key() == kind.key())
    }

    /// Tasks old enough to announce, newest first.
    #[must_use]
    pub fn visible(&self, now: Instant) -> Vec<&Task> {
        let mut v: Vec<&Task> = self
            .list
            .iter()
            .filter(|t| now.saturating_duration_since(t.started) >= NOTICE_DELAY)
            .collect();
        v.sort_by_key(|t| std::cmp::Reverse(t.started));
        v
    }

    /// Anything running at all, announced or not.
    #[must_use]
    pub fn any(&self) -> bool {
        !self.list.is_empty()
    }
}

/// How a toast looks and how long it lives.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToastKind {
    /// Something finished; fades away.
    Success,
    /// Information; fades away.
    Info,
    /// Something failed; stays until dismissed.
    Error,
}

/// How long a success or info toast shows before it starts to fade.
pub const TOAST_LIFETIME: Duration = Duration::from_secs(6);
/// How long the fade takes.
pub const TOAST_FADE: Duration = Duration::from_millis(900);
/// Toasts kept at once; the oldest fading one goes first.
const TOAST_LIMIT: usize = 4;

/// A short message about an outcome.
#[derive(Clone, Debug)]
pub struct Toast {
    /// Identifies it for dismissal.
    pub id: u64,
    /// Look and lifetime.
    pub kind: ToastKind,
    /// The message.
    pub text: String,
    /// When it appeared.
    pub created: Instant,
}

/// The visible toasts.
#[derive(Clone, Debug, Default)]
pub struct Toasts {
    list: Vec<Toast>,
    next: u64,
}

impl Toasts {
    /// Show `text`; returns its id.
    pub fn push(&mut self, kind: ToastKind, text: impl Into<String>, now: Instant) -> u64 {
        // The same message again (a retry that fails the same way) refreshes
        // the one on screen instead of stacking up.
        let text = text.into();
        if let Some(t) = self
            .list
            .iter_mut()
            .find(|t| t.kind == kind && t.text == text)
        {
            t.created = now;
            return t.id;
        }
        self.next += 1;
        self.list.push(Toast {
            id: self.next,
            kind,
            text,
            created: now,
        });
        while self.list.len() > TOAST_LIMIT {
            let victim = self
                .list
                .iter()
                .position(|t| t.kind != ToastKind::Error)
                .unwrap_or(0);
            self.list.remove(victim);
        }
        self.next
    }

    /// Remove one.
    pub fn dismiss(&mut self, id: u64) {
        self.list.retain(|t| t.id != id);
    }

    /// Remove every toast.
    pub fn clear(&mut self) {
        self.list.clear();
    }

    /// Drop the successes whose fade has finished.
    pub fn expire(&mut self, now: Instant) {
        self.list.retain(|t| Self::opacity(t, now) > 0.0);
    }

    /// Opacity in `0..=1`: errors stay solid, the rest fade after their life.
    #[must_use]
    pub fn opacity(t: &Toast, now: Instant) -> f32 {
        if t.kind == ToastKind::Error {
            return 1.0;
        }
        let age = now.saturating_duration_since(t.created);
        if age <= TOAST_LIFETIME {
            1.0
        } else {
            1.0 - (age.saturating_sub(TOAST_LIFETIME).as_secs_f32() / TOAST_FADE.as_secs_f32())
                .min(1.0)
        }
    }

    /// The toasts, oldest first.
    #[must_use]
    pub fn list(&self) -> &[Toast] {
        &self.list
    }

    /// True while something is fading, so the window keeps repainting.
    #[must_use]
    pub fn animating(&self, now: Instant) -> bool {
        self.list
            .iter()
            .any(|t| t.kind != ToastKind::Error && Self::opacity(t, now) > 0.0)
    }
}

/// `1234567` as `1,234,567`.
#[must_use]
pub fn group(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// A short duration: `0.4 s`, `2.1 s`, `45 s`, `3 min 5 s`.
#[must_use]
pub fn duration(d: Duration) -> String {
    let s = d.as_secs_f32();
    if s < 10.0 {
        format!("{s:.1} s")
    } else if s < 60.0 {
        format!("{s:.0} s")
    } else {
        let whole = s.round() as u64;
        format!("{} min {} s", whole / 60, whole % 60)
    }
}

/// A time left, to the second: `3 s`, `1 min 5 s`.
#[must_use]
pub fn coarse(d: Duration) -> String {
    let whole = d.as_secs_f32().round().max(1.0) as u64;
    if whole < 60 {
        format!("{whole} s")
    } else {
        format!("{} min {} s", whole / 60, whole % 60)
    }
}

/// The message after a finished stack.
#[must_use]
pub fn stacked_message(kept: usize, total: usize, took: Duration) -> String {
    format!(
        "Stacked {} of {} frames in {}",
        group(kept),
        group(total),
        duration(took)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t0() -> Instant {
        Instant::now()
    }

    #[test]
    fn numbers_and_times_read_naturally() {
        assert_eq!(group(0), "0");
        assert_eq!(group(999), "999");
        assert_eq!(group(1000), "1,000");
        assert_eq!(group(1_234_567), "1,234,567");
        assert_eq!(duration(Duration::from_millis(2100)), "2.1 s");
        assert_eq!(duration(Duration::from_secs(45)), "45 s");
        assert_eq!(duration(Duration::from_secs(185)), "3 min 5 s");
        assert_eq!(
            stacked_message(250, 1000, Duration::from_millis(2100)),
            "Stacked 250 of 1,000 frames in 2.1 s"
        );
    }

    #[test]
    fn every_stage_of_the_work_has_a_plain_label() {
        let kinds = [
            TaskKind::Opening,
            TaskKind::Transcoding,
            TaskKind::Decoding,
            TaskKind::Scoring,
            TaskKind::Stack(Stage::Quality),
            TaskKind::Stack(Stage::Reference),
            TaskKind::Stack(Stage::GlobalAlign),
            TaskKind::Stack(Stage::LocalAlign),
            TaskKind::Stack(Stage::Stack),
            TaskKind::Stack(Stage::Finish),
            TaskKind::Previewing,
            TaskKind::PreviewingFull,
            TaskKind::Exporting,
        ];
        for k in kinds {
            let l = k.label(1000);
            assert!(l.ends_with('\u{2026}') && l.len() > 8, "{k:?}: {l}");
        }
        assert_eq!(
            TaskKind::Scoring.label(1000),
            "Scoring 1,000 frames\u{2026}"
        );
        assert!(TaskKind::Transcoding.label(0).contains("ffmpeg"));
    }

    #[test]
    fn a_task_is_announced_only_after_the_delay() {
        let now = t0();
        let mut tasks = Tasks::default();
        tasks.begin(TaskKind::Opening, now, false);
        assert!(tasks.any());
        assert!(tasks.visible(now + Duration::from_millis(100)).is_empty());
        let shown = tasks.visible(now + Duration::from_millis(160));
        assert_eq!(shown.len(), 1);
        assert_eq!(shown[0].kind, TaskKind::Opening);
        assert_eq!(shown[0].fraction, None, "indeterminate until told");
    }

    #[test]
    fn every_long_operation_produces_a_notice_and_clears_it() {
        let now = t0();
        let later = now + Duration::from_millis(200);
        let ops = [
            TaskKind::Opening,
            TaskKind::Transcoding,
            TaskKind::Decoding,
            TaskKind::Scoring,
            TaskKind::Stack(Stage::LocalAlign),
            TaskKind::Previewing,
            TaskKind::PreviewingFull,
            TaskKind::Exporting,
        ];
        for k in ops {
            let mut tasks = Tasks::default();
            tasks.begin(k, now, false);
            assert_eq!(tasks.visible(later).len(), 1, "{k:?} shows a notice");
            tasks.end(k);
            assert!(
                !tasks.any() && tasks.visible(later).is_empty(),
                "{k:?} clears"
            );
        }
    }

    #[test]
    fn stages_of_one_run_share_a_notice() {
        let now = t0();
        let mut tasks = Tasks::default();
        tasks.begin(TaskKind::Stack(Stage::Quality), now, true);
        tasks.update(TaskKind::Stack(Stage::Quality), Some(0.5), now);
        tasks.update(TaskKind::Stack(Stage::GlobalAlign), Some(0.1), now);
        let v = tasks.visible(now + Duration::from_secs(1));
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].kind, TaskKind::Stack(Stage::GlobalAlign));
        assert_eq!(v[0].fraction, Some(0.1));
        assert!(v[0].cancellable);
        tasks.end(TaskKind::Stack(Stage::Finish));
        assert!(!tasks.any(), "ending any stage ends the run's notice");
    }

    #[test]
    fn progress_alone_starts_a_notice() {
        let now = t0();
        let mut tasks = Tasks::default();
        tasks.update(TaskKind::Transcoding, Some(0.3), now);
        assert_eq!(
            tasks.get(TaskKind::Transcoding).and_then(|t| t.fraction),
            Some(0.3)
        );
    }

    #[test]
    fn eta_needs_progress_and_time() {
        let now = t0();
        let mut tasks = Tasks::default();
        tasks.begin(TaskKind::Scoring, now, true);
        tasks.update(TaskKind::Scoring, Some(0.5), now);
        let task = tasks.get(TaskKind::Scoring).expect("task").clone();
        assert_eq!(task.eta(now + Duration::from_millis(500)), None, "too soon");
        let eta = task.eta(now + Duration::from_secs(4)).expect("eta");
        assert!((eta.as_secs_f32() - 4.0).abs() < 0.01, "{eta:?}");
        tasks.update(TaskKind::Scoring, Some(0.01), now);
        let few = tasks.get(TaskKind::Scoring).expect("task");
        assert_eq!(
            few.eta(now + Duration::from_secs(4)),
            None,
            "too little done"
        );
        tasks.update(TaskKind::Scoring, None, now);
        assert_eq!(tasks.get(TaskKind::Scoring).and_then(|t| t.eta(now)), None);
    }

    #[test]
    fn the_detail_line_gives_percent_count_and_time_left() {
        let now = t0();
        let mut tasks = Tasks::default();
        tasks.begin(TaskKind::Scoring, now, true);
        tasks.update(TaskKind::Scoring, Some(0.5), now);
        let t = tasks.get(TaskKind::Scoring).expect("task");
        let d = t.detail(now + Duration::from_secs(3), 1000);
        assert_eq!(d, "50% \u{b7} 500 of 1,000 frames \u{b7} about 3 s left");
        assert!(t.spoken(now, 1000).starts_with("Scoring 1,000 frames 50%"));
        tasks.update(TaskKind::Scoring, None, now);
        let t = tasks.get(TaskKind::Scoring).expect("task");
        assert_eq!(t.detail(now, 1000), "Working\u{2026}");
    }

    #[test]
    fn success_toasts_fade_and_errors_stay() {
        let now = t0();
        let mut toasts = Toasts::default();
        let ok = toasts.push(
            ToastKind::Success,
            "Stacked 250 of 1,000 frames in 2.1 s",
            now,
        );
        let bad = toasts.push(ToastKind::Error, "Could not open file: bad header", now);
        assert_eq!(toasts.list().len(), 2);
        let mid = now + TOAST_LIFETIME + TOAST_FADE / 2;
        let o = Toasts::opacity(&toasts.list()[0], mid);
        assert!(o > 0.4 && o < 0.6, "{o}");
        toasts.expire(mid);
        assert_eq!(toasts.list().len(), 2);
        toasts.expire(now + TOAST_LIFETIME + TOAST_FADE + Duration::from_millis(1));
        assert_eq!(toasts.list().len(), 1, "the success is gone");
        assert_eq!(toasts.list()[0].id, bad);
        toasts.expire(now + Duration::from_secs(3600));
        assert_eq!(toasts.list().len(), 1, "the error stays");
        toasts.dismiss(bad);
        assert!(toasts.list().is_empty());
        assert_ne!(ok, bad);
    }

    #[test]
    fn a_repeated_message_does_not_pile_up() {
        let now = t0();
        let mut toasts = Toasts::default();
        let a = toasts.push(ToastKind::Error, "Could not open file: x", now);
        let b = toasts.push(
            ToastKind::Error,
            "Could not open file: x",
            now + Duration::from_secs(1),
        );
        assert_eq!(a, b);
        assert_eq!(toasts.list().len(), 1);
        toasts.push(ToastKind::Info, "Could not open file: x", now);
        assert_eq!(
            toasts.list().len(),
            2,
            "a different kind is a different message"
        );
    }

    #[test]
    fn too_many_toasts_drop_the_oldest_success_first() {
        let now = t0();
        let mut toasts = Toasts::default();
        let err = toasts.push(ToastKind::Error, "first failure", now);
        for i in 0..6 {
            toasts.push(ToastKind::Info, format!("note {i}"), now);
        }
        assert_eq!(toasts.list().len(), 4);
        assert!(toasts.list().iter().any(|t| t.id == err), "errors are kept");
        assert_eq!(
            toasts.list().last().map(|t| t.text.as_str()),
            Some("note 5")
        );
    }
}
