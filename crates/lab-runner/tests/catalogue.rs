//! The scenarios this repository ships, run against the fake presenter.
//!
//! Every shipped scenario must parse, name real profiles, and pass against
//! an app that behaves. And the catalogue must catch a broken app: with
//! hotplug handling switched off, the hotplug scenarios have to fail.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use lab_core::{ProfileSet, Scenario};
use lab_fake::{fake, FakeBugs};
use lab_runner::{Report, Runner};
use std::path::{Path, PathBuf};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn scenarios() -> Vec<(PathBuf, Scenario)> {
    let mut out = vec![];
    for dir in ["scenarios/catalogue", "scenarios/fake-presenter"] {
        let dir = root().join(dir);
        let mut paths: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| p.extension().is_some_and(|x| x == "toml"))
            .collect();
        paths.sort();
        assert!(!paths.is_empty(), "no scenarios in {}", dir.display());
        for p in paths {
            let text = std::fs::read_to_string(&p).unwrap();
            let s = Scenario::from_toml(&text, &p.display().to_string()).unwrap();
            out.push((p, s));
        }
    }
    out
}

fn run(path: &Path, scenario: &Scenario, bugs: FakeBugs) -> Report {
    let profiles = ProfileSet::load_dir(&root().join("profiles")).unwrap();
    let (mut displays, mut capture, mut app) = fake(bugs);
    let mut runner = Runner {
        displays: &mut displays,
        capture: &mut capture,
        adapter: &mut app,
        profiles: &profiles,
        backend: "fake".into(),
        artifacts: None,
        scenario_dir: path.parent().unwrap().to_path_buf(),
    };
    runner.run(scenario)
}

#[test]
fn every_shipped_scenario_passes_against_a_good_app() {
    let all = scenarios();
    assert!(all.len() >= 10, "found {} scenarios", all.len());
    let failures: Vec<String> = all
        .iter()
        .map(|(p, s)| run(p, s, FakeBugs::default()))
        .filter(|r| !r.passed)
        .map(|r| r.markdown())
        .collect();
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn the_hotplug_scenarios_catch_an_app_that_ignores_hotplug() {
    let bugs = FakeBugs {
        ignore_hotplug: true,
        ..Default::default()
    };
    for name in [
        "cable-pull",
        "mode-change",
        "projector-wakes-late",
        "stage-monitor-drops",
        "stand-in",
    ] {
        let (path, scenario) = scenarios()
            .into_iter()
            .find(|(_, s)| s.name == name)
            .unwrap();
        let report = run(&path, &scenario, bugs);
        assert!(
            !report.passed,
            "{name} passed against an app that ignores hotplug"
        );
    }
}
