// SPDX-FileCopyrightText: 2026 Laurence Horrocks-Barlow
// SPDX-License-Identifier: GPL-3.0-or-later
// See ADDITIONAL-TERMS.md for terms added under GPLv3 section 7.

//! Plugin steps in the pipeline.
//!
//! A [`StepSpec`] names a plugin step and its parameters. A [`Runner`] starts the plugins the
//! steps need (once each, kept for the whole run) and applies them: `frame-op` steps to every
//! frame as it is read ([`PluginFrames`]), `image-op` steps to the stack, `analyse` steps to the
//! result, and `stack-backend` plugins in place of the local stacking.

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use helia_core::error::{Error, Result as CoreResult};
use helia_core::frame::Frame;
use helia_core::io::source::FrameSource;
use serde_json::{Value, json};

use crate::error::{HostError, Kind};
use crate::registry::Registry;
use crate::session::{Analysis, Progress, Session, Stacked};

/// Where an `image-op` step runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StepStage {
    /// On the stack, before Helia's own sharpening and edits.
    PreEdit,
    /// After the edits, before colour (the default).
    PostEdit,
}

impl StepStage {
    /// The word used in messages and on the command line.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::PreEdit => "pre-edit",
            Self::PostEdit => "post-edit",
        }
    }
}

/// One use of a plugin step.
#[derive(Clone, Debug, PartialEq)]
pub struct StepSpec {
    /// Plugin name.
    pub plugin: String,
    /// Step id.
    pub step: String,
    /// Parameters.
    pub params: Value,
    /// Where an `image-op` runs.
    pub stage: StepStage,
}

impl StepSpec {
    /// Parse `plugin/step`, `plugin/step:{"json":"params"}` or `plugin/step:key=value,key=value`,
    /// optionally prefixed `pre:` for an image step that runs before Helia's edits.
    ///
    /// # Errors
    /// A sentence saying what is wrong with the text.
    pub fn parse(text: &str) -> Result<Self, String> {
        let (stage, rest) = match text.strip_prefix("pre:") {
            Some(r) => (StepStage::PreEdit, r),
            None => (StepStage::PostEdit, text),
        };
        let (id, params) = match rest.split_once(':') {
            Some((a, b)) => (a, Some(b)),
            None => (rest, None),
        };
        let (plugin, step) = id
            .split_once('/')
            .filter(|(p, s)| !p.is_empty() && !s.is_empty() && !s.contains('/'))
            .ok_or_else(|| {
                format!("{text:?} should look like plugin/step, for example invert/invert")
            })?;
        let params = match params {
            None | Some("") => json!({}),
            Some(p) if p.trim_start().starts_with('{') => {
                if !helia_plugin_sdk::wire::depth_ok(
                    p.as_bytes(),
                    helia_plugin_sdk::wire::MAX_DEPTH,
                ) {
                    return Err("the parameters nest too deeply".into());
                }
                serde_json::from_str(p)
                    .map_err(|e| format!("the parameters are not valid JSON ({e})"))?
            }
            Some(p) => {
                let mut m = serde_json::Map::new();
                for kv in p.split(',') {
                    let (k, v) = kv
                        .split_once('=')
                        .ok_or_else(|| format!("{kv:?} should look like key=value"))?;
                    let v = v.trim();
                    let val = if let Ok(b) = v.parse::<bool>() {
                        Value::Bool(b)
                    } else if let Ok(n) = v.parse::<i64>() {
                        json!(n)
                    } else if let Some(n) = v.parse::<f64>().ok().filter(|n| n.is_finite()) {
                        json!(n)
                    } else {
                        Value::String(v.to_owned())
                    };
                    m.insert(k.trim().to_owned(), val);
                }
                Value::Object(m)
            }
        };
        Ok(Self {
            plugin: plugin.to_owned(),
            step: step.to_owned(),
            params,
            stage,
        })
    }
}

/// The plugins in use for one run.
pub struct Runner {
    /// The plugins found and their approvals.
    pub registry: Registry,
    sessions: HashMap<String, Session>,
}

impl Runner {
    /// A runner over `registry`.
    #[must_use]
    pub fn new(registry: Registry) -> Self {
        Self {
            registry,
            sessions: HashMap::new(),
        }
    }

    /// The running session for `name`, starting it on first use (and again if it died).
    ///
    /// # Errors
    /// [`HostError`] from starting.
    pub fn session(&mut self, name: &str) -> Result<&mut Session, HostError> {
        let dead = self.sessions.get_mut(name).is_some_and(Session::is_dead);
        if dead {
            self.sessions.remove(name);
        }
        if !self.sessions.contains_key(name) {
            let s = Session::start(&self.registry, name)?;
            self.sessions.insert(name.to_owned(), s);
        }
        self.sessions
            .get_mut(name)
            .ok_or_else(|| HostError::invalid(name, "could not be started"))
    }

    /// What kind of step `spec` is (`frame-op`, `image-op` or `analyse`), or why it cannot be used.
    ///
    /// # Errors
    /// [`HostError`]: unknown plugin or step, plugin not enabled, parameters wrong.
    pub fn check(&self, spec: &StepSpec) -> Result<String, HostError> {
        let l = self.registry.runnable(&spec.plugin)?;
        let s = l.step(&spec.step).ok_or_else(|| {
            HostError::refused(
                &spec.plugin,
                format!(
                    "The plugin {} has no step called {}.",
                    spec.plugin, spec.step
                ),
                format!(
                    "Its steps are: {}.",
                    l.steps
                        .iter()
                        .map(|s| s.step.id.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
                "Check the name with `helia plugins info`.",
            )
        })?;
        s.params.validate(&spec.params).map_err(|p| {
            HostError::refused(
                &spec.plugin,
                format!(
                    "The parameters of {}/{} are not right.",
                    spec.plugin, spec.step
                ),
                p.join(" "),
                "Correct them and try again.",
            )
        })?;
        Ok(s.step.capability.clone())
    }

    /// Check every step before a long run starts.
    ///
    /// # Errors
    /// The first problem found.
    pub fn check_all(&self, specs: &[StepSpec]) -> Result<(), HostError> {
        specs.iter().try_for_each(|s| self.check(s).map(|_| ()))
    }

    /// Apply the `image-op` steps of `specs` that run at `stage`, in order.
    ///
    /// # Errors
    /// [`HostError`] from the first step that fails, or cancellation.
    pub fn image_steps(
        &mut self,
        specs: &[StepSpec],
        stage: StepStage,
        mut frame: Frame,
        cancel: &AtomicBool,
        on_progress: &mut dyn FnMut(&StepSpec, Progress),
    ) -> Result<Frame, HostError> {
        for spec in specs.iter().filter(|s| s.stage == stage) {
            if self.check(spec)? != "image-op" {
                continue;
            }
            let session = self.session(&spec.plugin)?;
            let mut forward = |p: Progress| on_progress(spec, p);
            frame = session.image_op(
                &spec.step,
                &spec.params,
                &frame,
                stage.as_str(),
                cancel,
                &mut forward,
            )?;
        }
        Ok(frame)
    }

    /// Run the `analyse` steps of `specs` on `frame`.
    ///
    /// # Errors
    /// [`HostError`] from the first step that fails.
    pub fn analyse_steps(
        &mut self,
        specs: &[StepSpec],
        frame: &Frame,
        cancel: &AtomicBool,
        on_progress: &mut dyn FnMut(&StepSpec, Progress),
    ) -> Result<Vec<(StepSpec, Analysis)>, HostError> {
        let mut out = Vec::new();
        for spec in specs {
            if self.check(spec)? != "analyse" {
                continue;
            }
            let session = self.session(&spec.plugin)?;
            let mut forward = |p: Progress| on_progress(spec, p);
            let a = session.analyse(&spec.step, &spec.params, frame, cancel, &mut forward)?;
            out.push((spec.clone(), a));
        }
        Ok(out)
    }

    /// Have the `stack-backend` plugin `name` stack `input`.
    ///
    /// # Errors
    /// [`HostError`].
    pub fn stack(
        &mut self,
        name: &str,
        input: &Path,
        job: &Value,
        cancel: &AtomicBool,
        on_progress: &mut dyn FnMut(Progress),
    ) -> Result<Stacked, HostError> {
        self.session(name)?.stack(input, job, cancel, on_progress)
    }

    /// Run the plugin's own check.
    ///
    /// # Errors
    /// [`HostError`].
    pub fn test(
        &mut self,
        name: &str,
        cancel: &AtomicBool,
    ) -> Result<helia_plugin_sdk::TestResult, HostError> {
        self.session(name)?.test(cancel)
    }

    /// The `frame-op` steps among `specs`, checked.
    ///
    /// # Errors
    /// [`HostError`] for a step that cannot be used.
    pub fn frame_steps(&self, specs: &[StepSpec]) -> Result<Vec<StepSpec>, HostError> {
        let mut out = Vec::new();
        for s in specs {
            if self.check(s)? == "frame-op" {
                out.push(s.clone());
            }
        }
        Ok(out)
    }

    /// Lines for the history of a saved file about every plugin that took part in the run, in
    /// plugin-name order: see [`Session::provenance`].
    #[must_use]
    pub fn provenance(&self) -> Vec<String> {
        let mut names: Vec<&String> = self.sessions.keys().collect();
        names.sort();
        names
            .into_iter()
            .filter_map(|n| self.sessions.get(n))
            .flat_map(Session::provenance)
            .collect()
    }

    /// FITS cards for every plugin that took part: one pair of `HIERARCH HELIA PLUGIN` cards
    /// naming them all, then each plugin's allow-listed `HELIA ML` cards.
    #[must_use]
    pub fn fits_cards(&self) -> Vec<helia_core::io::fits::HeaderCard> {
        let mut names: Vec<&String> = self.sessions.keys().collect();
        names.sort();
        let sessions: Vec<&Session> = names
            .into_iter()
            .filter_map(|n| self.sessions.get(n))
            .filter(|s| !s.provenance().is_empty())
            .collect();
        let who: Vec<(String, String)> = sessions
            .iter()
            .map(|s| {
                let m = &s.loaded().manifest;
                (m.name.clone(), m.version.clone())
            })
            .collect();
        let mut cards = crate::cards::plugin_cards(&who);
        for s in sessions {
            cards.extend(s.fits_cards().into_iter().skip(2));
        }
        cards
    }

    /// The log of a running plugin.
    #[must_use]
    pub fn log(&self, name: &str) -> String {
        self.sessions
            .get(name)
            .map(Session::log)
            .unwrap_or_default()
    }

    /// Stop every plugin.
    pub fn close_all(&mut self) {
        for (_, mut s) in self.sessions.drain() {
            s.close();
        }
    }
}

impl Drop for Runner {
    fn drop(&mut self) {
        self.close_all();
    }
}

/// A frame source that passes every frame through `frame-op` steps as it is read.
pub struct PluginFrames {
    inner: Box<dyn FrameSource>,
    runner: Arc<Mutex<Runner>>,
    steps: Vec<StepSpec>,
    cancel: Arc<AtomicBool>,
    failure: Arc<Mutex<Option<HostError>>>,
}

impl PluginFrames {
    /// Wrap `inner`. `steps` must all be `frame-op` steps (see [`Runner::frame_steps`]).
    #[must_use]
    pub fn new(
        inner: Box<dyn FrameSource>,
        runner: Arc<Mutex<Runner>>,
        steps: Vec<StepSpec>,
        cancel: Arc<AtomicBool>,
    ) -> Self {
        Self {
            inner,
            runner,
            steps,
            cancel,
            failure: Arc::new(Mutex::new(None)),
        }
    }

    /// A handle to read the plugin's own error after the pipeline reports a failure.
    #[must_use]
    pub fn failure(&self) -> Arc<Mutex<Option<HostError>>> {
        Arc::clone(&self.failure)
    }
}

impl FrameSource for PluginFrames {
    fn dimensions(&self) -> (usize, usize) {
        self.inner.dimensions()
    }

    fn frame_count(&self) -> usize {
        self.inner.frame_count()
    }

    fn read(&mut self, index: usize) -> CoreResult<Frame> {
        let mut frame = self.inner.read(index)?;
        let count = self.inner.frame_count();
        let Ok(mut runner) = self.runner.lock() else {
            return Err(Error::Backend(
                "A plugin step stopped working earlier in this run.".into(),
            ));
        };
        for spec in &self.steps {
            let r = runner.session(&spec.plugin).and_then(|s| {
                s.frame_op(&spec.step, &spec.params, &frame, index, count, &self.cancel)
            });
            match r {
                Ok(f) => frame = f,
                Err(e) => {
                    let core: Error = e.clone().into();
                    if e.kind != Kind::Cancelled
                        && let Ok(mut slot) = self.failure.lock()
                    {
                        slot.get_or_insert(e);
                    }
                    return Err(core);
                }
            }
        }
        Ok(frame)
    }

    fn describe(&self) -> String {
        let names: Vec<String> = self
            .steps
            .iter()
            .map(|s| format!("{}/{}", s.plugin, s.step))
            .collect();
        format!("{} + {}", self.inner.describe(), names.join(", "))
    }

    fn quality_note(&self) -> Option<String> {
        self.inner.quality_note()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn parse_steps() {
        let s = StepSpec::parse("invert/invert").unwrap();
        assert_eq!(
            (s.plugin.as_str(), s.step.as_str(), s.stage),
            ("invert", "invert", StepStage::PostEdit)
        );
        let s = StepSpec::parse("pre:dn/denoise:strength=0.5,fast=true,n=3,mode=a").unwrap();
        assert_eq!(s.stage, StepStage::PreEdit);
        assert_eq!(
            s.params,
            json!({"strength": 0.5, "fast": true, "n": 3, "mode": "a"})
        );
        let s = StepSpec::parse(r#"dn/denoise:{"strength":2}"#).unwrap();
        assert_eq!(s.params, json!({"strength": 2}));
        for bad in [
            "",
            "x",
            "a/b/c",
            "/b",
            "a/",
            "a/b:k",
            "a/b:{",
            "a/b:{\"k\":",
        ] {
            assert!(StepSpec::parse(bad).is_err(), "{bad:?}");
        }
        let deep = format!("a/b:{}{}", "[".repeat(50), "]".repeat(50));
        assert!(StepSpec::parse(&format!("{{{deep}")).is_err());
    }
}
