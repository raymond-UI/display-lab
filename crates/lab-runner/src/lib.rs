//! Running a scenario.
//!
//! The runner owns nothing platform- or app-specific: it is handed a display
//! backend, a capture backend and an adapter, plays the scenario's timeline
//! against them, and writes a [`Report`]. Every wait is a poll against a
//! deadline, never a fixed sleep, because every real platform reports screen
//! changes asynchronously.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

mod report;

pub use report::{Latency, Outcome, Report, StepReport};

use lab_adapter::{Adapter, AdapterError, ScreenRef};
use lab_compare::{compare, diff_image, differs, is_blank, load_png, save_png, Thresholds};
use lab_core::{
    now_ns, BackendError, Capture, CompareSpec, Displays, Frame, Mode, Profile, ProfileSet,
    Scenario, ScreenId, ScreenInfo, Step, WatchId,
};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// How often a wait looks again. Around one frame at 60 Hz: finer is wasted
/// work, coarser blurs the latency numbers.
pub const POLL: Duration = Duration::from_millis(16);

/// How long the initial setup may take to settle (all `plugged` screens
/// reported) before the run gives up.
pub const SETTLE: Duration = Duration::from_secs(10);

/// How long an unplugged screen may take to disappear. macOS needs up to two
/// seconds for six screens.
pub const REMOVAL: Duration = Duration::from_secs(5);

pub struct Runner<'a> {
    pub displays: &'a mut dyn Displays,
    pub capture: &'a mut dyn Capture,
    pub adapter: &'a mut dyn Adapter,
    pub profiles: &'a ProfileSet,
    /// Named in the report.
    pub backend: String,
    /// Where the frame each check judged is written. `None` writes nothing.
    pub artifacts: Option<PathBuf>,
    /// Where relative reference-image paths in the scenario are resolved
    /// from: normally the scenario file's directory.
    pub scenario_dir: PathBuf,
}

/// A step's result before it is put in the report.
struct StepResult {
    outcome: Outcome,
    detail: Option<String>,
    artifacts: Vec<String>,
}

impl StepResult {
    fn done() -> StepResult {
        StepResult {
            outcome: Outcome::Done,
            detail: None,
            artifacts: vec![],
        }
    }
    fn pass(detail: Option<String>) -> StepResult {
        StepResult {
            outcome: Outcome::Pass,
            detail,
            artifacts: vec![],
        }
    }
    fn fail(detail: impl Into<String>) -> StepResult {
        StepResult {
            outcome: Outcome::Fail,
            detail: Some(detail.into()),
            artifacts: vec![],
        }
    }
}

/// What a poll saw this time round.
enum Probe<T> {
    Done(T),
    /// Not yet; the string says what was seen, for the failure message.
    NotYet(String),
}

/// A screen declaration with its profile and mode resolved, and the
/// scenario's timing overrides applied to the profile.
struct Resolved {
    profile: Profile,
    mode: Mode,
}

impl<'a> Runner<'a> {
    pub fn run(&mut self, scenario: &Scenario) -> Report {
        let started = Instant::now();
        let mut report = Report {
            scenario: scenario.name.clone(),
            app: self.adapter.info().name.clone(),
            backend: self.backend.clone(),
            passed: false,
            error: None,
            notes: vec![],
            steps: vec![],
            latencies: vec![],
            duration_ms: 0,
        };

        // Everything that can be known to be wrong before a screen is
        // touched is checked here, so a run never half-happens because of a
        // typo in a profile name or a capability the app does not have.
        let resolved = match self.preflight(scenario) {
            Ok(r) => r,
            Err(e) => {
                report.error = Some(e);
                report.duration_ms = started.elapsed().as_millis() as u64;
                return report;
            }
        };
        match self.capture_app() {
            Ok(Some(note)) => report.notes.push(note),
            Ok(None) => {}
            Err(e) => {
                report.error = Some(e);
                report.duration_ms = started.elapsed().as_millis() as u64;
                return report;
            }
        }

        let mut run = Run {
            runner: self,
            scenario,
            resolved,
            plugged: BTreeMap::new(),
            marks: BTreeMap::new(),
            last_input: None,
            launched: false,
        };
        let setup = run.set_up();
        if let Err(e) = setup {
            report.error = Some(e);
        } else {
            let mut failed = false;
            for (i, step) in scenario.steps.iter().enumerate() {
                let index = i + 1;
                let label = label(step);
                if failed {
                    report.steps.push(StepReport {
                        index,
                        kind: step.kind().into(),
                        label,
                        outcome: Outcome::Skipped,
                        elapsed_ms: 0,
                        detail: None,
                        artifacts: vec![],
                    });
                    continue;
                }
                let t = Instant::now();
                let mut result = run.step(index, step, &mut report.latencies);
                failed = result.outcome == Outcome::Fail;
                let elapsed_ms = t.elapsed().as_millis() as u64;
                if failed {
                    run.save_the_rest(index, step.screen(), &mut result);
                }
                report.steps.push(StepReport {
                    index,
                    kind: step.kind().into(),
                    label,
                    outcome: result.outcome,
                    elapsed_ms,
                    detail: result.detail,
                    artifacts: result.artifacts,
                });
            }
        }

        // Always tidy up, pass or fail, so the next run starts clean.
        let watches: Vec<WatchId> = run
            .marks
            .values()
            .chain(run.last_input.as_ref())
            .flat_map(|m| m.watches.values().copied())
            .collect();
        for w in watches {
            run.runner.capture.unwatch(w);
        }
        let mut teardown = vec![];
        if run.launched {
            if let Err(e) = run.runner.adapter.stop() {
                teardown.push(format!("stopping the app: {e}"));
            }
        }
        if let Err(e) = run.runner.displays.reset() {
            teardown.push(format!("removing screens: {e}"));
        }
        if !teardown.is_empty() {
            let text = teardown.join("; ");
            report.error = Some(match report.error.take() {
                Some(e) => format!("{e}; then {text}"),
                None => format!("teardown: {text}"),
            });
        }

        report.passed =
            report.error.is_none() && report.steps.iter().all(|s| s.outcome != Outcome::Fail);
        report.duration_ms = started.elapsed().as_millis() as u64;
        report
    }

    /// Point capture at the app's own windows, when the app names itself.
    /// Returns a note for the report saying how the screens were seen.
    fn capture_app(&mut self) -> Result<Option<String>, String> {
        let Some(bundle_id) = self.adapter.info().bundle_id.clone() else {
            return Ok(None);
        };
        match self.capture.only_app(&bundle_id) {
            Ok(()) => Ok(Some(format!(
                "Captured only the windows of `{bundle_id}`: the desktop, other apps \
                 and the OS's own marks are left out."
            ))),
            Err(BackendError::Unsupported(_)) => Ok(Some(format!(
                "The `{}` backend captures whole screens, so the desktop and other apps \
                 count as the app's picture (`{bundle_id}` is not used).",
                self.backend
            ))),
            Err(e) => Err(format!("capturing only {bundle_id:?}: {e}")),
        }
    }

    fn preflight(&self, scenario: &Scenario) -> Result<BTreeMap<String, Resolved>, String> {
        let mut resolved = BTreeMap::new();
        for decl in &scenario.screens {
            let mut profile = self
                .profiles
                .get(&decl.profile)
                .map_err(|e| format!("screen {:?}: {e}", decl.id))?
                .clone();
            if let Some(delay) = decl.report_delay_ms {
                profile.timings.report_delay_ms = delay;
            }
            let mode = profile
                .mode(decl.mode.as_deref())
                .map_err(|e| format!("screen {:?}: {e}", decl.id))?;
            resolved.insert(decl.id.clone(), Resolved { profile, mode });
        }

        let caps = &self.adapter.info().capabilities;
        for (i, step) in scenario.steps.iter().enumerate() {
            let at = |m: String| format!("step {} ({}): {m}", i + 1, step.kind());
            let needs = match step {
                Step::Setup => Some(("setup", caps.setup)),
                Step::ExpectMatches { .. } => Some(("expected_frame", caps.expected_frame)),
                Step::ExpectBound { .. } | Step::ExpectUnbound { .. } => {
                    Some(("bindings", caps.bindings))
                }
                Step::ExpectState { .. } => Some(("state", caps.state)),
                _ => None,
            };
            if let Some((name, offered)) = needs {
                if !offered {
                    return Err(at(format!(
                        "needs the adapter capability {name:?}, which {} does not offer",
                        self.adapter.info().name
                    )));
                }
            }
            if let Step::Plug {
                screen,
                mode: Some(mode),
            } = step
            {
                if let Some(r) = resolved.get(screen) {
                    r.profile.mode(Some(mode)).map_err(|e| at(e.to_string()))?;
                }
            }
            if let Step::ExpectReference { image, .. } = step {
                let path = self.scenario_dir.join(image);
                if !path.is_file() {
                    return Err(at(format!("no reference image at {}", path.display())));
                }
            }
        }
        Ok(resolved)
    }
}

/// One run in progress.
struct Run<'r, 'a> {
    runner: &'r mut Runner<'a>,
    scenario: &'r Scenario,
    resolved: BTreeMap<String, Resolved>,
    /// Scenario screen name to the backend's id, while plugged in.
    plugged: BTreeMap<String, ScreenId>,
    /// Input marks: the lab-clock time of the input, and every plugged
    /// screen's picture just before it.
    marks: BTreeMap<String, Mark>,
    last_input: Option<Mark>,
    launched: bool,
}

#[derive(Clone)]
struct Mark {
    at_ns: u64,
    before: BTreeMap<String, Option<Frame>>,
    /// Per screen, the backend's watch for the first frame that differs
    /// from `before`, where the backend can keep one.
    watches: BTreeMap<String, WatchId>,
}

impl Run<'_, '_> {
    fn set_up(&mut self) -> Result<(), String> {
        let decls = self.scenario.screens.clone();
        for decl in decls.iter().filter(|d| d.plugged) {
            let r = &self.resolved[&decl.id];
            let id = if decl.host {
                self.runner.displays.host_screen(&r.profile, &r.mode)
            } else {
                self.runner.displays.plug(&r.profile, &r.mode)
            }
            .map_err(|e| format!("setting up screen {:?}: {e}", decl.id))?;
            self.plugged.insert(decl.id.clone(), id);
        }
        // Wait until the OS reports every screen the scenario starts with.
        let wanted: Vec<ScreenId> = self.plugged.values().copied().collect();
        let deadline = Instant::now() + SETTLE;
        loop {
            let reported = self.runner.displays.screens().map_err(|e| e.to_string())?;
            let missing = wanted
                .iter()
                .filter(|id| !reported.iter().any(|s| s.id == **id))
                .count();
            if missing == 0 {
                self.warm();
                return Ok(());
            }
            if Instant::now() >= deadline {
                let seen: Vec<String> = reported
                    .iter()
                    .map(|s| format!("{:?} {:?} {}", s.id, s.identity.name, s.mode))
                    .collect();
                return Err(format!(
                    "{missing} screen(s) were plugged in but not reported within {} s \
                     (waiting for {wanted:?}; the backend reports [{}])",
                    SETTLE.as_secs(),
                    seen.join(", ")
                ));
            }
            std::thread::sleep(POLL);
        }
    }

    fn step(&mut self, index: usize, step: &Step, latencies: &mut Vec<Latency>) -> StepResult {
        let within = Duration::from_millis(step.within_ms());
        let result = match step {
            Step::Launch => self.launch(),
            Step::Setup => self
                .runner
                .adapter
                .setup()
                .map(|_| StepResult::done())
                .map_err(app),
            Step::Input { action, args, mark } => self.input(action, args, mark.as_deref()),
            Step::Plug { screen, mode } => self.plug(screen, mode.as_deref()),
            Step::Unplug { screen } => self.unplug(screen),
            Step::Wait { ms } => {
                std::thread::sleep(Duration::from_millis(*ms));
                Ok(StepResult::done())
            }
            Step::ExpectContent { screen, .. } => {
                self.expect_frame(index, screen, within, "content", |f| {
                    if is_blank(f) {
                        Probe::NotYet("the screen is blank".into())
                    } else {
                        Probe::Done(())
                    }
                })
            }
            Step::ExpectBlank { screen, .. } => {
                self.expect_frame(index, screen, within, "blank", |f| {
                    if is_blank(f) {
                        Probe::Done(())
                    } else {
                        Probe::NotYet("the screen shows content".into())
                    }
                })
            }
            Step::ExpectChanged {
                screen,
                since,
                max_latency_ms,
                ..
            } => self.expect_changed(
                index,
                screen,
                since.as_deref(),
                within,
                *max_latency_ms,
                latencies,
            ),
            Step::ExpectMatches {
                screen,
                output,
                compare,
                ..
            } => self.expect_matches(index, screen, output, within, *compare),
            Step::ExpectReference {
                screen,
                image,
                compare,
                ..
            } => self.expect_reference(index, screen, image, within, *compare),
            Step::ExpectBound { output, screen, .. } => {
                self.expect_bound(output, Some(screen), within)
            }
            Step::ExpectUnbound { output, .. } => self.expect_bound(output, None, within),
            Step::ExpectState {
                pointer, equals, ..
            } => self.expect_state(pointer, equals, within),
        };
        result.unwrap_or_else(StepResult::fail)
    }

    fn launch(&mut self) -> Result<StepResult, String> {
        let screens = self.runner.displays.screens().map_err(|e| e.to_string())?;
        self.runner.adapter.launch(&screens).map_err(app)?;
        self.launched = true;
        // A started app is a new process: capture has to find it again.
        self.runner.capture_app()?;
        self.warm();
        Ok(StepResult::done())
    }

    /// Start capturing every plugged screen now, so a check's time is spent
    /// on the app, not on the backend's first frame (most of a second for a
    /// large screen on macOS). A screen not reported yet is started by the
    /// first check that looks at it. Errors surface in that check too.
    fn warm(&mut self) {
        let names: Vec<String> = self.plugged.keys().cloned().collect();
        for name in names {
            let _ = self.frame(&name);
        }
    }

    /// After a failed check, keep what every other screen showed at that
    /// moment: a picture that went to the wrong screen shows up there.
    fn save_the_rest(&mut self, index: usize, checked: Option<&str>, out: &mut StepResult) {
        if self.runner.artifacts.is_none() {
            return;
        }
        let names: Vec<String> = self.plugged.keys().cloned().collect();
        // The checked screen's own frame is already saved with the verdict.
        for name in names.into_iter().filter(|n| Some(n.as_str()) != checked) {
            if let Ok(Some(frame)) = self.frame(&name) {
                self.save(index, &format!("{name}-meanwhile"), Some(&frame), out);
            }
        }
    }

    fn input(
        &mut self,
        action: &str,
        args: &Value,
        mark: Option<&str>,
    ) -> Result<StepResult, String> {
        // The "before" pictures are taken first, so a fast app cannot change
        // the screen between the snapshot and the timestamp.
        let names: Vec<String> = self.plugged.keys().cloned().collect();
        let mut before = BTreeMap::new();
        let mut watches = BTreeMap::new();
        for name in names {
            let frame = self.frame(&name)?;
            // Ask the backend to keep the first frame that differs, so a
            // change that is over before the input call returns (a fade
            // that runs while the app answers) is timed from its start.
            if let (Some(b), Some(&id)) = (frame.clone(), self.plugged.get(&name)) {
                match self
                    .runner
                    .capture
                    .watch(id, Box::new(move |f: &Frame| differs(&b, f)))
                {
                    Ok(Some(w)) => {
                        watches.insert(name.clone(), w);
                    }
                    Ok(None) | Err(BackendError::NoSuchScreen(_)) => {}
                    Err(e) => return Err(format!("watching {name:?}: {e}")),
                }
            }
            before.insert(name, frame);
        }
        let at_ns = now_ns();
        let m = Mark {
            at_ns,
            before,
            watches,
        };
        let sent = self.runner.adapter.input(action, args).map_err(app);
        let replaced = match mark {
            Some(name) => self.marks.insert(name.to_string(), m.clone()),
            None => None,
        };
        let previous = self.last_input.replace(m);
        for old in [replaced, previous].into_iter().flatten() {
            self.retire(old);
        }
        sent?;
        Ok(StepResult::done())
    }

    /// Stop the watches of a mark nothing refers to any more.
    fn retire(&mut self, old: Mark) {
        for w in old.watches.values() {
            let in_use = self
                .marks
                .values()
                .chain(self.last_input.as_ref())
                .any(|m| m.watches.values().any(|x| x == w));
            if !in_use {
                self.runner.capture.unwatch(*w);
            }
        }
    }

    fn plug(&mut self, screen: &str, mode: Option<&str>) -> Result<StepResult, String> {
        if self.plugged.contains_key(screen) {
            return Err(format!("{screen:?} is already plugged in"));
        }
        let r = &self.resolved[screen];
        let mode = match mode {
            Some(m) => r.profile.mode(Some(m)).map_err(|e| e.to_string())?,
            None => r.mode,
        };
        let id = self
            .runner
            .displays
            .plug(&r.profile, &mode)
            .map_err(|e| e.to_string())?;
        let detail = format!("{} in {mode}", r.profile.name);
        self.plugged.insert(screen.to_string(), id);
        // Start capture now if the OS already reports it; see `warm`.
        let _ = self.frame(screen);
        // Deliberately no wait for the OS to report it: a scenario about a
        // projector that wakes late wants to look at the world before then.
        Ok(StepResult {
            outcome: Outcome::Done,
            detail: Some(detail),
            artifacts: vec![],
        })
    }

    fn unplug(&mut self, screen: &str) -> Result<StepResult, String> {
        let id = self
            .plugged
            .remove(screen)
            .ok_or_else(|| format!("{screen:?} is not plugged in"))?;
        self.runner.displays.unplug(id).map_err(|e| e.to_string())?;
        // Removal is asynchronous on real platforms. The next step should
        // see a world where the screen is gone.
        let deadline = Instant::now() + REMOVAL;
        loop {
            let reported = self.runner.displays.screens().map_err(|e| e.to_string())?;
            if !reported.iter().any(|s| s.id == id) {
                return Ok(StepResult::done());
            }
            if Instant::now() >= deadline {
                return Err(format!(
                    "{screen:?} was unplugged but still reported after {} s",
                    REMOVAL.as_secs()
                ));
            }
            std::thread::sleep(POLL);
        }
    }

    /// The newest frame on a scenario screen. `None` when the screen is
    /// plugged in but not reported yet, or nothing is drawn on it.
    fn frame(&mut self, screen: &str) -> Result<Option<Frame>, String> {
        let Some(&id) = self.plugged.get(screen) else {
            return Err(format!("{screen:?} is not plugged in"));
        };
        match self.runner.capture.frame(id) {
            Ok(frame) => Ok(frame),
            Err(BackendError::NoSuchScreen(_)) => Ok(None),
            Err(e) => Err(format!("capturing {screen:?}: {e}")),
        }
    }

    fn screen_info(&mut self, screen: &str) -> Result<Option<ScreenInfo>, String> {
        let Some(&id) = self.plugged.get(screen) else {
            return Ok(None);
        };
        let reported = self.runner.displays.screens().map_err(|e| e.to_string())?;
        Ok(reported.into_iter().find(|s| s.id == id))
    }

    /// Poll `probe` until it is done or `within` runs out.
    fn poll<T>(
        &mut self,
        within: Duration,
        mut probe: impl FnMut(&mut Self) -> Result<Probe<T>, String>,
    ) -> Result<Result<T, String>, String> {
        let deadline = Instant::now() + within;
        loop {
            match probe(self)? {
                Probe::Done(t) => return Ok(Ok(t)),
                Probe::NotYet(seen) => {
                    if Instant::now() >= deadline {
                        return Ok(Err(format!("not within {} ms: {seen}", within.as_millis())));
                    }
                }
            }
            std::thread::sleep(POLL);
        }
    }

    fn expect_frame(
        &mut self,
        index: usize,
        screen: &str,
        within: Duration,
        what: &str,
        check: impl Fn(&Frame) -> Probe<()>,
    ) -> Result<StepResult, String> {
        let mut last = None;
        let verdict = self.poll(within, |run| {
            let Some(frame) = run.frame(screen)? else {
                return Ok(Probe::NotYet("nothing on the screen yet".into()));
            };
            let probe = check(&frame);
            last = Some(frame);
            Ok(probe)
        })?;
        Ok(match verdict {
            Ok(()) => {
                let mut out = StepResult::pass(None);
                self.save(index, &format!("{screen}-{what}"), last.as_ref(), &mut out);
                out
            }
            Err(why) => {
                let mut out = StepResult::fail(why);
                self.save(
                    index,
                    &format!("{screen}-not-{what}"),
                    last.as_ref(),
                    &mut out,
                );
                out
            }
        })
    }

    fn expect_changed(
        &mut self,
        index: usize,
        screen: &str,
        since: Option<&str>,
        within: Duration,
        max_latency_ms: Option<u64>,
        latencies: &mut Vec<Latency>,
    ) -> Result<StepResult, String> {
        let mark = match since {
            Some(name) => self.marks.get(name).cloned(),
            None => self.last_input.clone(),
        }
        .ok_or("no input has been sent yet")?;
        let before = mark.before.get(screen).cloned().flatten();
        let watch = mark.watches.get(screen).copied();
        let mut last = None;
        let verdict = self.poll(within, |run| {
            // The first changed frame, if the backend kept it: it is earlier
            // than the newest whenever the picture went on changing.
            if let Some(w) = watch {
                let caught = run
                    .runner
                    .capture
                    .caught(w)
                    .map_err(|e| format!("capturing {screen:?}: {e}"))?;
                if let Some(frame) = caught {
                    last = Some(frame.clone());
                    return Ok(Probe::Done(frame));
                }
            }
            let Some(frame) = run.frame(screen)? else {
                return Ok(Probe::NotYet("nothing on the screen".into()));
            };
            let changed = before.as_ref().is_none_or(|b| differs(b, &frame));
            if changed {
                // The backend tests a frame before publishing it, so a
                // watch may hold an earlier change than this one.
                if let Some(w) = watch {
                    if let Ok(Some(first)) = run.runner.capture.caught(w) {
                        last = Some(first.clone());
                        return Ok(Probe::Done(first));
                    }
                }
            }
            last = Some(frame.clone());
            Ok(if changed {
                Probe::Done(frame)
            } else {
                Probe::NotYet("the picture has not changed".into())
            })
        })?;
        match verdict {
            Ok(frame) => {
                // The frame's own presentation time if the backend knows it;
                // otherwise when the lab saw it, which can only be later.
                let seen = if frame.present_ns > 0 {
                    frame.present_ns
                } else {
                    now_ns()
                };
                let ms = seen.saturating_sub(mark.at_ns) as f64 / 1e6;
                latencies.push(Latency {
                    step: index,
                    screen: screen.to_string(),
                    since: since.unwrap_or("last input").to_string(),
                    ms,
                });
                if let Some(max) = max_latency_ms {
                    if ms > max as f64 {
                        let mut out =
                            StepResult::fail(format!("changed after {ms:.1} ms, budget {max} ms"));
                        self.save(index, &format!("{screen}-late"), Some(&frame), &mut out);
                        return Ok(out);
                    }
                }
                let mut out = StepResult::pass(Some(format!("changed after {ms:.1} ms")));
                self.save(index, &format!("{screen}-changed"), Some(&frame), &mut out);
                Ok(out)
            }
            Err(why) => {
                let mut out = StepResult::fail(why);
                self.save(
                    index,
                    &format!("{screen}-unchanged"),
                    last.as_ref(),
                    &mut out,
                );
                Ok(out)
            }
        }
    }

    fn thresholds(&self, step: Option<CompareSpec>) -> Thresholds {
        let mut t = Thresholds::default();
        for spec in [self.runner.adapter.info().capabilities.compare, step]
            .into_iter()
            .flatten()
        {
            if let Some(v) = spec.min_ssim {
                t.min_ssim = v;
            }
            if let Some(v) = spec.max_delta_e {
                t.max_delta_e = v;
            }
            if let Some(v) = spec.max_bad_fraction {
                t.max_bad_fraction = v;
            }
        }
        t
    }

    fn expect_matches(
        &mut self,
        index: usize,
        screen: &str,
        output: &str,
        within: Duration,
        spec: Option<CompareSpec>,
    ) -> Result<StepResult, String> {
        let thresholds = self.thresholds(spec);
        let mut last: Option<(Frame, Frame, String)> = None;
        let verdict = self.poll(within, |run| {
            let Some(actual) = run.frame(screen)? else {
                return Ok(Probe::NotYet("nothing on the screen yet".into()));
            };
            let expected = run
                .runner
                .adapter
                .expected_frame(output, actual.width, actual.height)
                .map_err(app)?;
            let result = compare(&actual, &expected, thresholds).map_err(|e| e.to_string())?;
            let summary = result.summary(output);
            let passed = result.passed();
            last = Some((actual, expected, summary.clone()));
            Ok(if passed {
                Probe::Done(summary)
            } else {
                Probe::NotYet(summary)
            })
        })?;
        Ok(match verdict {
            Ok(summary) => {
                let mut out = StepResult::pass(Some(summary));
                if let Some((actual, _, _)) = &last {
                    self.save(index, &format!("{screen}-actual"), Some(actual), &mut out);
                }
                out
            }
            Err(why) => {
                let mut out = StepResult::fail(why);
                if let Some((actual, expected, _)) = &last {
                    self.save(index, &format!("{screen}-actual"), Some(actual), &mut out);
                    self.save(
                        index,
                        &format!("{screen}-expected"),
                        Some(expected),
                        &mut out,
                    );
                    let diff = diff_image(actual, expected, thresholds.max_delta_e);
                    self.save(index, &format!("{screen}-diff"), Some(&diff), &mut out);
                }
                out
            }
        })
    }

    fn expect_reference(
        &mut self,
        index: usize,
        screen: &str,
        image: &Path,
        within: Duration,
        spec: Option<CompareSpec>,
    ) -> Result<StepResult, String> {
        let thresholds = self.thresholds(spec);
        let path = self.runner.scenario_dir.join(image);
        let expected = load_png(&path).map_err(|e| e.to_string())?;
        let mut last = None;
        let verdict = self.poll(within, |run| {
            let Some(actual) = run.frame(screen)? else {
                return Ok(Probe::NotYet("nothing on the screen yet".into()));
            };
            let probe = match compare(&actual, &expected, thresholds) {
                Ok(r) if r.passed() => Probe::Done(r.summary("reference")),
                Ok(r) => Probe::NotYet(r.summary("reference")),
                Err(e) => Probe::NotYet(e.to_string()),
            };
            last = Some(actual);
            Ok(probe)
        })?;
        Ok(match verdict {
            Ok(summary) => {
                let mut out = StepResult::pass(Some(summary));
                self.save(index, &format!("{screen}-actual"), last.as_ref(), &mut out);
                out
            }
            Err(why) => {
                let mut out = StepResult::fail(why);
                self.save(index, &format!("{screen}-actual"), last.as_ref(), &mut out);
                out
            }
        })
    }

    /// `screen` `Some`: the output must be on that screen. `None`: on none.
    fn expect_bound(
        &mut self,
        output: &str,
        screen: Option<&str>,
        within: Duration,
    ) -> Result<StepResult, String> {
        let verdict = self.poll(within, |run| {
            let bindings = run.runner.adapter.bindings().map_err(app)?;
            let Some(binding) = bindings.into_iter().find(|b| b.output == output) else {
                return Ok(Probe::NotYet(format!(
                    "the app reports no output {output:?}"
                )));
            };
            let on = describe(binding.screen.as_ref());
            match screen {
                None => Ok(if binding.screen.is_none() {
                    Probe::Done(())
                } else {
                    Probe::NotYet(format!("{output:?} is on {on}"))
                }),
                Some(name) => {
                    let Some(info) = run.screen_info(name)? else {
                        return Ok(Probe::NotYet(format!(
                            "{name:?} is not reported by the OS; {output:?} is on {on}"
                        )));
                    };
                    let here = binding.screen.as_ref().is_some_and(|r| r.matches(&info));
                    Ok(if here {
                        Probe::Done(())
                    } else {
                        Probe::NotYet(format!("{output:?} is on {on}, not {name:?}"))
                    })
                }
            }
        })?;
        Ok(match verdict {
            Ok(()) => StepResult::pass(None),
            Err(why) => StepResult::fail(why),
        })
    }

    fn expect_state(
        &mut self,
        pointer: &str,
        equals: &Value,
        within: Duration,
    ) -> Result<StepResult, String> {
        let verdict = self.poll(within, |run| {
            let state = run.runner.adapter.state().map_err(app)?;
            Ok(match state.pointer(pointer) {
                Some(v) if v == equals => Probe::Done(()),
                Some(v) => Probe::NotYet(format!("{pointer} is {v}, expected {equals}")),
                None => Probe::NotYet(format!("the state has nothing at {pointer}")),
            })
        })?;
        Ok(match verdict {
            Ok(()) => StepResult::pass(None),
            Err(why) => StepResult::fail(why),
        })
    }

    /// Write `frame` as an artifact of step `index`, if there is somewhere to
    /// write it. A failure to write is noted, never fatal.
    fn save(&self, index: usize, name: &str, frame: Option<&Frame>, out: &mut StepResult) {
        let (Some(dir), Some(frame)) = (&self.runner.artifacts, frame) else {
            return;
        };
        let file = format!("{index:02}-{}.png", sanitize(name));
        match save_png(frame, &dir.join(&file)) {
            Ok(()) => out.artifacts.push(file),
            Err(e) => {
                let note = format!("(could not save {file}: {e})");
                out.detail = Some(match out.detail.take() {
                    Some(d) => format!("{d} {note}"),
                    None => note,
                });
            }
        }
    }
}

fn app(e: AdapterError) -> String {
    format!("app: {e}")
}

fn describe(screen: Option<&ScreenRef>) -> String {
    match screen {
        None => "no screen".into(),
        Some(r) => match (&r.name, r.serial) {
            (Some(name), Some(serial)) => format!("{name:?} (serial {serial})"),
            (Some(name), None) => format!("{name:?}"),
            _ => format!("{r:?}"),
        },
    }
}

fn sanitize(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// A step in a few words, for the report.
fn label(step: &Step) -> String {
    match step {
        Step::Launch => "launch the app".into(),
        Step::Setup => "adapter setup".into(),
        Step::Input { action, args, .. } if args.is_null() => format!("input {action}"),
        Step::Input { action, args, .. } => format!("input {action} {args}"),
        Step::Plug {
            screen,
            mode: Some(m),
        } => format!("plug {screen} in {m}"),
        Step::Plug { screen, .. } => format!("plug {screen}"),
        Step::Unplug { screen } => format!("unplug {screen}"),
        Step::Wait { ms } => format!("wait {ms} ms"),
        Step::ExpectContent { screen, .. } => format!("{screen} shows content"),
        Step::ExpectBlank { screen, .. } => format!("{screen} is blank"),
        Step::ExpectChanged { screen, .. } => format!("{screen} changes"),
        Step::ExpectMatches { screen, output, .. } => format!("{screen} shows {output}"),
        Step::ExpectReference { screen, image, .. } => {
            format!("{screen} matches {}", image.display())
        }
        Step::ExpectBound { output, screen, .. } => format!("{output} is on {screen}"),
        Step::ExpectUnbound { output, .. } => format!("{output} is on no screen"),
        Step::ExpectState {
            pointer, equals, ..
        } => format!("state {pointer} = {equals}"),
    }
}
