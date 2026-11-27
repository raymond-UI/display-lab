//! The runner end to end against the fake world.
//!
//! Every behaviour test comes in a pair: the scenario passes against an app
//! that behaves, and fails against one with the matching bug. A check that
//! only ever passes proves nothing.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use lab_core::{Identity, Mode, Profile, ProfileSet, Scenario, Timings};
use lab_fake::{fake, FakeBugs};
use lab_runner::{Outcome, Report, Runner};
use std::path::PathBuf;

fn profile(name: &str, serial: u32, w: u32, h: u32, delay_ms: u64) -> Profile {
    Profile {
        name: name.into(),
        description: String::new(),
        identity: Identity {
            vendor: 0x1234,
            product: 1,
            serial,
            name: "Projector".into(),
        },
        modes: vec![
            Mode {
                width: w,
                height: h,
                refresh_hz: 60.0,
                hidpi: false,
            },
            Mode {
                width: w / 2,
                height: h / 2,
                refresh_hz: 60.0,
                hidpi: false,
            },
        ],
        timings: Timings {
            report_delay_ms: delay_ms,
        },
    }
}

fn profiles() -> ProfileSet {
    let mut set = ProfileSet::default();
    let mut laptop = profile("laptop", 0, 320, 200, 0);
    laptop.identity.name = "Built-in".into();
    set.insert(laptop).unwrap();
    set.insert(profile("projector", 7, 256, 192, 0)).unwrap();
    // Same model, same name, different unit.
    set.insert(profile("other-projector", 8, 256, 192, 0))
        .unwrap();
    set.insert(profile("stage", 10, 320, 180, 0)).unwrap();
    set
}

fn run_with(bugs: FakeBugs, toml: &str, artifacts: Option<PathBuf>) -> Report {
    let scenario = Scenario::from_toml(toml, "test.toml").unwrap();
    let profiles = profiles();
    let (mut displays, mut capture, mut app) = fake(bugs);
    let mut runner = Runner {
        displays: &mut displays,
        capture: &mut capture,
        adapter: &mut app,
        profiles: &profiles,
        backend: "fake".into(),
        artifacts,
        scenario_dir: PathBuf::from("."),
    };
    runner.run(&scenario)
}

fn run(toml: &str) -> Report {
    run_with(FakeBugs::default(), toml, None)
}

fn failed_step(report: &Report) -> Option<usize> {
    report
        .steps
        .iter()
        .find(|s| s.outcome == Outcome::Fail)
        .map(|s| s.index)
}

const SCREENS: &str = r#"
    [[screens]]
    id = "laptop"
    profile = "laptop"
    host = true

    [[screens]]
    id = "projector"
    profile = "projector"
"#;

fn scenario(steps: &str) -> String {
    format!("name = \"t\"\n{SCREENS}\n{steps}")
}

const CABLE_PULL: &str = r#"
    [[steps]]
    step = "launch"
    [[steps]]
    step = "expect_matches"
    screen = "projector"
    output = "audience"
    [[steps]]
    step = "unplug"
    screen = "projector"
    [[steps]]
    step = "expect_unbound"
    output = "audience"
    [[steps]]
    step = "expect_matches"
    screen = "laptop"
    output = "audience"
    [[steps]]
    step = "plug"
    screen = "projector"
    [[steps]]
    step = "expect_bound"
    output = "audience"
    screen = "projector"
    within_ms = 1000
    [[steps]]
    step = "expect_matches"
    screen = "projector"
    output = "audience"
    within_ms = 1000
"#;

#[test]
fn cable_pull_passes_when_the_app_follows_the_projector() {
    let report = run(&scenario(CABLE_PULL));
    assert!(report.passed, "{}", report.markdown());
    assert_eq!(report.steps.len(), 8);
}

#[test]
fn cable_pull_fails_when_the_app_ignores_hotplug() {
    let bugs = FakeBugs {
        ignore_hotplug: true,
        ..Default::default()
    };
    let report = run_with(bugs, &scenario(CABLE_PULL), None);
    assert!(!report.passed);
    // The app never notices the projector leaving, so it still claims the
    // audience output is on it.
    assert_eq!(failed_step(&report), Some(4), "{}", report.markdown());
    assert!(report.steps[4..]
        .iter()
        .all(|s| s.outcome == Outcome::Skipped));
}

const SWAP: &str = r#"
    [[screens]]
    id = "other"
    profile = "other-projector"
    plugged = false

    [[steps]]
    step = "launch"
    [[steps]]
    step = "unplug"
    screen = "projector"
    [[steps]]
    step = "plug"
    screen = "other"
    [[steps]]
    step = "wait"
    ms = 100
    [[steps]]
    step = "expect_unbound"
    output = "audience"
    [[steps]]
    step = "expect_blank"
    screen = "other"
"#;

#[test]
fn a_swapped_projector_is_not_adopted() {
    let report = run(&scenario(SWAP));
    assert!(report.passed, "{}", report.markdown());
}

#[test]
fn binding_by_name_adopts_the_wrong_projector() {
    let bugs = FakeBugs {
        bind_by_name: true,
        ..Default::default()
    };
    let report = run_with(bugs, &scenario(SWAP), None);
    assert_eq!(failed_step(&report), Some(5), "{}", report.markdown());
}

const NEXT: &str = r#"
    [[steps]]
    step = "launch"
    [[steps]]
    step = "expect_content"
    screen = "projector"
    [[steps]]
    step = "input"
    action = "next"
    mark = "next"
    [[steps]]
    step = "expect_changed"
    screen = "projector"
    since = "next"
    max_latency_ms = 150
    [[steps]]
    step = "expect_state"
    pointer = "/slide"
    equals = 2
"#;

#[test]
fn latency_is_measured_and_within_budget() {
    let bugs = FakeBugs {
        input_latency_ms: 40,
        ..Default::default()
    };
    let report = run_with(bugs, &scenario(NEXT), None);
    assert!(report.passed, "{}", report.markdown());
    let ms = report.latencies[0].ms;
    assert!((40.0..150.0).contains(&ms), "latency {ms}");
}

#[test]
fn a_slow_app_breaks_the_latency_budget() {
    let bugs = FakeBugs {
        input_latency_ms: 300,
        ..Default::default()
    };
    let report = run_with(bugs, &scenario(NEXT), None);
    assert_eq!(failed_step(&report), Some(4), "{}", report.markdown());
    assert!(report.latencies[0].ms >= 300.0);
}

#[test]
fn a_late_projector_is_waited_for_and_the_laptop_stands_in() {
    // The same projector (same identity) comes back, but takes 400 ms to be
    // reported. Until then the slides stand in on the laptop; then they move.
    let steps = r#"
        name = "late"
        [[screens]]
        id = "laptop"
        profile = "laptop"
        host = true
        [[screens]]
        id = "projector"
        profile = "projector"
        [[screens]]
        id = "projector-waking"
        profile = "projector"
        plugged = false
        report_delay_ms = 400

        [[steps]]
        step = "launch"
        [[steps]]
        step = "unplug"
        screen = "projector"
        [[steps]]
        step = "plug"
        screen = "projector-waking"
        [[steps]]
        step = "expect_matches"
        screen = "laptop"
        output = "audience"
        within_ms = 200
        [[steps]]
        step = "expect_bound"
        output = "audience"
        screen = "projector-waking"
        within_ms = 2000
        [[steps]]
        step = "expect_matches"
        screen = "projector-waking"
        output = "audience"
    "#;
    let report = run(steps);
    assert!(report.passed, "{}", report.markdown());
    // The binding could only have come once the OS reported the screen.
    assert!(report.steps[4].elapsed_ms >= 150, "{}", report.markdown());
}

#[test]
fn a_mode_change_keeps_the_binding_and_refits() {
    let steps = scenario(
        r#"
        [[steps]]
        step = "launch"
        [[steps]]
        step = "unplug"
        screen = "projector"
        [[steps]]
        step = "plug"
        screen = "projector"
        mode = "128x96"
        [[steps]]
        step = "expect_bound"
        output = "audience"
        screen = "projector"
        [[steps]]
        step = "expect_matches"
        screen = "projector"
        output = "audience"
    "#,
    );
    let report = run(&steps);
    assert!(report.passed, "{}", report.markdown());
}

#[test]
fn a_missing_capability_is_refused_before_any_screen_is_touched() {
    // The fake app offers everything, so ask for a profile that does not
    // exist instead and check nothing ran.
    let text = scenario("[[steps]]\nstep = \"launch\"")
        .replace("profile = \"projector\"", "profile = \"nope\"");
    let report = run(&text);
    assert!(!report.passed);
    assert!(report.error.as_deref().unwrap_or("").contains("nope"));
    assert!(report.steps.is_empty());
}

#[test]
fn failures_leave_frames_behind() {
    let dir = std::env::temp_dir().join(format!("lab-runner-artifacts-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let steps = scenario(
        r#"
        [[steps]]
        step = "launch"
        [[steps]]
        step = "expect_blank"
        screen = "projector"
        within_ms = 100
    "#,
    );
    let report = run_with(FakeBugs::default(), &steps, Some(dir.clone()));
    assert_eq!(failed_step(&report), Some(2));
    let artifacts = &report.steps[1].artifacts;
    for a in artifacts {
        assert!(dir.join(a).is_file(), "{a}");
    }
    // The checked screen, then every other screen at that moment.
    assert_eq!(
        artifacts,
        &["02-projector-not-blank.png", "02-laptop-meanwhile.png"]
    );
    let _ = std::fs::remove_dir_all(&dir);
}
