//! How the runner uses a backend's app-only capture and frame watches.
//!
//! The fake world has neither, so these tests wrap it: a capture that can
//! (or cannot) filter to one app and keep the first changed frame, and an
//! app that names its bundle id.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use lab_adapter::{Adapter, AdapterError, AppInfo, Binding};
use lab_core::{
    now_ns, BackendError, Capture, Frame, FrameTest, Identity, Mode, Profile, ProfileSet, Scenario,
    ScreenId, ScreenInfo, Timings, WatchId,
};
use lab_fake::{fake, FakeApp, FakeBugs, FakeCapture};
use lab_runner::{Report, Runner};
use serde_json::Value;
use std::path::PathBuf;

const BUNDLE: &str = "org.example.fake";

fn profile(name: &str, serial: u32, w: u32, h: u32) -> Profile {
    Profile {
        name: name.into(),
        description: String::new(),
        identity: Identity {
            vendor: 0x1234,
            product: 1,
            serial,
            name: name.into(),
        },
        modes: vec![Mode {
            width: w,
            height: h,
            refresh_hz: 60.0,
            hidpi: false,
        }],
        timings: Timings { report_delay_ms: 0 },
    }
}

fn profiles() -> ProfileSet {
    let mut set = ProfileSet::default();
    set.insert(profile("laptop", 0, 320, 200)).unwrap();
    set.insert(profile("projector", 7, 256, 192)).unwrap();
    set
}

const NEXT: &str = r#"
    name = "t"
    [[screens]]
    id = "laptop"
    profile = "laptop"
    host = true
    [[screens]]
    id = "projector"
    profile = "projector"
    [[steps]]
    step = "launch"
    [[steps]]
    step = "expect_content"
    screen = "projector"
    [[steps]]
    step = "input"
    action = "next"
    [[steps]]
    step = "expect_changed"
    screen = "projector"
    max_latency_ms = 150
"#;

/// What the wrapped capture can do.
#[derive(Clone, Copy, PartialEq)]
enum Filter {
    /// `only_app` is unsupported, as on a whole-screen backend.
    None,
    Works,
    Fails,
}

struct Wrapped {
    inner: FakeCapture,
    filter: Filter,
    only_app_calls: Vec<String>,
    /// Watches as (id, screen, test, caught).
    watches: Vec<(WatchId, ScreenId, FrameTest, Option<Frame>)>,
    next: u64,
    /// When set, a watch reports a frame shown this long after it was set,
    /// standing in for a change the lab saw start long before it settled.
    early_ms: Option<u64>,
    watch_started: Vec<(WatchId, u64)>,
}

impl Wrapped {
    fn new(inner: FakeCapture, filter: Filter, early_ms: Option<u64>) -> Self {
        Wrapped {
            inner,
            filter,
            only_app_calls: vec![],
            watches: vec![],
            next: 0,
            early_ms,
            watch_started: vec![],
        }
    }
}

impl Capture for Wrapped {
    fn frame(&mut self, id: ScreenId) -> Result<Option<Frame>, BackendError> {
        let frame = self.inner.frame(id)?;
        if let Some(f) = &frame {
            for (watch, screen, test, caught) in &mut self.watches {
                if *screen == id && caught.is_none() && test(f) {
                    let mut first = f.clone();
                    if let Some(early) = self.early_ms {
                        let (_, at) = self.watch_started.iter().find(|(w, _)| w == watch).unwrap();
                        first.present_ns = at + early * 1_000_000;
                    }
                    *caught = Some(first);
                }
            }
        }
        Ok(frame)
    }

    fn only_app(&mut self, bundle_id: &str) -> Result<(), BackendError> {
        self.only_app_calls.push(bundle_id.to_string());
        match self.filter {
            Filter::None => Err(BackendError::Unsupported("capture one app's windows")),
            Filter::Works => Ok(()),
            Filter::Fails => Err(BackendError::Platform(format!(
                "{bundle_id} is not running"
            ))),
        }
    }

    fn watch(&mut self, id: ScreenId, test: FrameTest) -> Result<Option<WatchId>, BackendError> {
        self.next += 1;
        let watch = WatchId(self.next);
        self.watches.push((watch, id, test, None));
        self.watch_started.push((watch, now_ns()));
        Ok(Some(watch))
    }

    fn caught(&mut self, watch: WatchId) -> Result<Option<Frame>, BackendError> {
        Ok(self
            .watches
            .iter()
            .find(|(w, ..)| *w == watch)
            .and_then(|(.., c)| c.clone()))
    }

    fn unwatch(&mut self, watch: WatchId) {
        self.watches.retain(|(w, ..)| *w != watch);
    }
}

/// The fake app, naming a bundle id.
struct Named {
    inner: FakeApp,
    info: AppInfo,
}

impl Adapter for Named {
    fn info(&self) -> &AppInfo {
        &self.info
    }
    fn launch(&mut self, screens: &[ScreenInfo]) -> Result<(), AdapterError> {
        self.inner.launch(screens)
    }
    fn input(&mut self, action: &str, args: &Value) -> Result<(), AdapterError> {
        self.inner.input(action, args)
    }
    fn state(&mut self) -> Result<Value, AdapterError> {
        self.inner.state()
    }
    fn bindings(&mut self) -> Result<Vec<Binding>, AdapterError> {
        self.inner.bindings()
    }
    fn expected_frame(&mut self, output: &str, w: u32, h: u32) -> Result<Frame, AdapterError> {
        self.inner.expected_frame(output, w, h)
    }
    fn stop(&mut self) -> Result<(), AdapterError> {
        self.inner.stop()
    }
}

fn run(bugs: FakeBugs, filter: Filter, early_ms: Option<u64>) -> (Report, Wrapped) {
    let scenario = Scenario::from_toml(NEXT, "test.toml").unwrap();
    let profiles = profiles();
    let (mut displays, capture, app) = fake(bugs);
    let mut capture = Wrapped::new(capture, filter, early_ms);
    let mut info = app.info().clone();
    info.bundle_id = Some(BUNDLE.into());
    let mut app = Named { inner: app, info };
    let mut runner = Runner {
        displays: &mut displays,
        capture: &mut capture,
        adapter: &mut app,
        profiles: &profiles,
        backend: "fake".into(),
        artifacts: None,
        scenario_dir: PathBuf::from("."),
    };
    let report = runner.run(&scenario);
    (report, capture)
}

#[test]
fn the_app_is_captured_alone_before_and_after_launch() {
    let (report, capture) = run(FakeBugs::default(), Filter::Works, None);
    assert!(report.passed, "{}", report.markdown());
    assert_eq!(capture.only_app_calls, [BUNDLE, BUNDLE]);
    assert!(
        report.notes[0].contains("Captured only the windows"),
        "{:?}",
        report.notes
    );
}

#[test]
fn a_whole_screen_backend_says_so_and_still_runs() {
    let (report, _) = run(FakeBugs::default(), Filter::None, None);
    assert!(report.passed, "{}", report.markdown());
    assert!(
        report.notes[0].contains("captures whole screens"),
        "{:?}",
        report.notes
    );
}

#[test]
fn an_app_capture_cannot_find_stops_the_run() {
    let (report, _) = run(FakeBugs::default(), Filter::Fails, None);
    assert!(!report.passed);
    assert!(report.steps.is_empty(), "{}", report.markdown());
    let error = report.error.unwrap();
    assert!(error.contains("is not running"), "{error}");
}

#[test]
fn latency_is_timed_from_the_first_changed_frame() {
    // The picture settles 300 ms after the input, past the 150 ms budget,
    // but the backend saw it start changing 20 ms after the watch was set.
    let bugs = FakeBugs {
        input_latency_ms: 300,
        ..Default::default()
    };
    let (report, capture) = run(bugs, Filter::Works, Some(20));
    assert!(report.passed, "{}", report.markdown());
    let ms = report.latencies[0].ms;
    assert!(ms < 100.0, "latency {ms}");
    assert!(capture.watches.is_empty(), "watches left running");
}

#[test]
fn a_change_that_starts_late_still_breaks_the_budget() {
    let bugs = FakeBugs {
        input_latency_ms: 300,
        ..Default::default()
    };
    let (report, capture) = run(bugs, Filter::Works, None);
    assert!(!report.passed, "{}", report.markdown());
    assert!(report.latencies[0].ms >= 300.0);
    assert!(capture.watches.is_empty(), "watches left running");
}
