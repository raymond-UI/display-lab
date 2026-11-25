//! Scenarios: a display setup, and a timeline of things that happen to it.
//!
//! A scenario is app-neutral. It says "the projector is pulled for 20 s" and
//! "the audience output is back on the projector within 3 s"; how the app is
//! driven and what "audience output" means is the adapter's business.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeSet;
use std::path::PathBuf;

/// How long an expectation waits for the screens to settle, unless the step
/// says otherwise.
pub const DEFAULT_WITHIN_MS: u64 = 3000;

#[derive(Debug, thiserror::Error)]
pub enum ScenarioError {
    #[error("parsing {path}: {source}")]
    Parse {
        path: String,
        source: Box<toml::de::Error>,
    },
    #[error("{path}: {message}")]
    Invalid { path: String, message: String },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Scenario {
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub screens: Vec<ScreenDecl>,
    pub steps: Vec<Step>,
}

/// A screen the scenario uses, by a name local to the scenario.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScreenDecl {
    pub id: String,
    pub profile: String,
    /// `WIDTHxHEIGHT` or `WIDTHxHEIGHT@HZ`; the profile's first mode if unset.
    #[serde(default)]
    pub mode: Option<String>,
    /// Plugged in before the app launches. Set false for a screen that
    /// arrives later in the timeline.
    #[serde(default = "yes")]
    pub plugged: bool,
    /// The machine's own screen. Never unplugged by the lab.
    #[serde(default)]
    pub host: bool,
    /// Overrides the profile's `timings.report_delay_ms` for this screen:
    /// the same projector, slow to wake this time.
    #[serde(default)]
    pub report_delay_ms: Option<u64>,
}

fn yes() -> bool {
    true
}

/// Per-step override of the compare thresholds. Unset fields keep the
/// adapter's (or the lab's) defaults.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompareSpec {
    pub min_ssim: Option<f64>,
    pub max_delta_e: Option<f64>,
    pub max_bad_fraction: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "step", rename_all = "snake_case", deny_unknown_fields)]
pub enum Step {
    /// Start the app under test.
    Launch,
    /// The adapter's one-time setup for an unattended run.
    Setup,
    /// Send the app an input. `action` and `args` mean whatever the adapter
    /// says they mean. `mark` names the moment for a later latency check.
    Input {
        action: String,
        #[serde(default)]
        args: Value,
        #[serde(default)]
        mark: Option<String>,
    },
    Plug {
        screen: String,
        #[serde(default)]
        mode: Option<String>,
    },
    Unplug {
        screen: String,
    },
    Wait {
        ms: u64,
    },

    /// Something other than black is on the screen.
    ExpectContent {
        screen: String,
        #[serde(default)]
        within_ms: Option<u64>,
    },
    /// The screen is present and shows nothing but black.
    ExpectBlank {
        screen: String,
        #[serde(default)]
        within_ms: Option<u64>,
    },
    /// The screen's picture changes after `since` (a mark), or after the most
    /// recent input. Records the latency; fails if it exceeds `max_latency_ms`.
    ExpectChanged {
        screen: String,
        #[serde(default)]
        since: Option<String>,
        #[serde(default)]
        within_ms: Option<u64>,
        #[serde(default)]
        max_latency_ms: Option<u64>,
    },
    /// The screen shows what the app says `output` should show. Needs the
    /// adapter's `expected_frame` capability.
    ExpectMatches {
        screen: String,
        output: String,
        #[serde(default)]
        within_ms: Option<u64>,
        #[serde(default)]
        compare: Option<CompareSpec>,
    },
    /// The screen matches an image on disk.
    ExpectReference {
        screen: String,
        image: PathBuf,
        #[serde(default)]
        within_ms: Option<u64>,
        #[serde(default)]
        compare: Option<CompareSpec>,
    },
    /// The app reports `output` drawn on `screen`. Needs `bindings`.
    ExpectBound {
        output: String,
        screen: String,
        #[serde(default)]
        within_ms: Option<u64>,
    },
    /// The app reports `output` drawn on no screen. Needs `bindings`.
    ExpectUnbound {
        output: String,
        #[serde(default)]
        within_ms: Option<u64>,
    },
    /// A value in the app's state, by JSON pointer, equals `equals`. Needs
    /// `state`.
    ExpectState {
        pointer: String,
        equals: Value,
        #[serde(default)]
        within_ms: Option<u64>,
    },
}

impl Step {
    /// The scenario screen this step names, if any.
    pub fn screen(&self) -> Option<&str> {
        match self {
            Step::Plug { screen, .. }
            | Step::Unplug { screen }
            | Step::ExpectContent { screen, .. }
            | Step::ExpectBlank { screen, .. }
            | Step::ExpectChanged { screen, .. }
            | Step::ExpectMatches { screen, .. }
            | Step::ExpectReference { screen, .. }
            | Step::ExpectBound { screen, .. } => Some(screen),
            Step::Launch
            | Step::Setup
            | Step::Input { .. }
            | Step::Wait { .. }
            | Step::ExpectUnbound { .. }
            | Step::ExpectState { .. } => None,
        }
    }

    /// The step's name as it is written in a scenario file.
    pub fn kind(&self) -> &'static str {
        match self {
            Step::Launch => "launch",
            Step::Setup => "setup",
            Step::Input { .. } => "input",
            Step::Plug { .. } => "plug",
            Step::Unplug { .. } => "unplug",
            Step::Wait { .. } => "wait",
            Step::ExpectContent { .. } => "expect_content",
            Step::ExpectBlank { .. } => "expect_blank",
            Step::ExpectChanged { .. } => "expect_changed",
            Step::ExpectMatches { .. } => "expect_matches",
            Step::ExpectReference { .. } => "expect_reference",
            Step::ExpectBound { .. } => "expect_bound",
            Step::ExpectUnbound { .. } => "expect_unbound",
            Step::ExpectState { .. } => "expect_state",
        }
    }

    pub fn is_expectation(&self) -> bool {
        self.kind().starts_with("expect_")
    }

    pub fn within_ms(&self) -> u64 {
        let within = match self {
            Step::ExpectContent { within_ms, .. }
            | Step::ExpectBlank { within_ms, .. }
            | Step::ExpectChanged { within_ms, .. }
            | Step::ExpectMatches { within_ms, .. }
            | Step::ExpectReference { within_ms, .. }
            | Step::ExpectBound { within_ms, .. }
            | Step::ExpectUnbound { within_ms, .. }
            | Step::ExpectState { within_ms, .. } => *within_ms,
            _ => None,
        };
        within.unwrap_or(DEFAULT_WITHIN_MS)
    }
}

impl Scenario {
    pub fn from_toml(text: &str, path: &str) -> Result<Scenario, ScenarioError> {
        let scenario: Scenario = toml::from_str(text).map_err(|e| ScenarioError::Parse {
            path: path.to_string(),
            source: Box::new(e),
        })?;
        scenario.validate(path)?;
        Ok(scenario)
    }

    /// Structural checks that need no profiles and no machine. Profile and
    /// mode names are checked by the runner against a real profile set.
    pub fn validate(&self, path: &str) -> Result<(), ScenarioError> {
        let invalid = |message: String| ScenarioError::Invalid {
            path: path.to_string(),
            message,
        };

        if self.name.trim().is_empty() {
            return Err(invalid("name is empty".into()));
        }

        let mut ids = BTreeSet::new();
        for screen in &self.screens {
            if !ids.insert(screen.id.as_str()) {
                return Err(invalid(format!("two screens are called {:?}", screen.id)));
            }
            if screen.host && !screen.plugged {
                return Err(invalid(format!(
                    "screen {:?} is the host screen, which is always plugged in",
                    screen.id
                )));
            }
        }
        if self.screens.iter().filter(|s| s.host).count() > 1 {
            return Err(invalid("more than one host screen".into()));
        }

        let mut launched = false;
        let mut marks = BTreeSet::new();
        for (i, step) in self.steps.iter().enumerate() {
            let at =
                |message: String| invalid(format!("step {} ({}): {message}", i + 1, step.kind()));
            if let Some(screen) = step.screen() {
                if !ids.contains(screen) {
                    return Err(at(format!("no screen called {screen:?}")));
                }
                let is_host = self.screens.iter().any(|s| s.id == screen && s.host);
                if is_host && matches!(step, Step::Plug { .. } | Step::Unplug { .. }) {
                    return Err(at(format!(
                        "{screen:?} is the host screen and cannot be plugged or unplugged"
                    )));
                }
            }
            match step {
                Step::Launch => {
                    if launched {
                        return Err(at("the app is already launched".into()));
                    }
                    launched = true;
                }
                Step::Input { mark, .. } => {
                    if !launched {
                        return Err(at("input before launch".into()));
                    }
                    if let Some(mark) = mark {
                        if !marks.insert(mark.as_str()) {
                            return Err(at(format!("mark {mark:?} is used twice")));
                        }
                    }
                }
                Step::ExpectChanged {
                    since: Some(since), ..
                } if !marks.contains(since.as_str()) => {
                    return Err(at(format!("no earlier input is marked {since:?}")));
                }
                Step::ExpectState { pointer, .. }
                    if !(pointer.is_empty() || pointer.starts_with('/')) =>
                {
                    return Err(at(format!(
                        "{pointer:?} is not a JSON pointer (it must start with '/')"
                    )));
                }
                _ => {}
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CABLE_PULL: &str = r#"
        name = "cable-pull"
        description = "Projector pulled mid-presentation"

        [[screens]]
        id = "laptop"
        profile = "laptop-1080p"
        host = true

        [[screens]]
        id = "projector"
        profile = "xga-projector"

        [[steps]]
        step = "launch"

        [[steps]]
        step = "input"
        action = "next"
        mark = "first"

        [[steps]]
        step = "expect_changed"
        screen = "projector"
        since = "first"
        max_latency_ms = 250

        [[steps]]
        step = "unplug"
        screen = "projector"

        [[steps]]
        step = "wait"
        ms = 200

        [[steps]]
        step = "plug"
        screen = "projector"

        [[steps]]
        step = "expect_bound"
        output = "audience"
        screen = "projector"
        within_ms = 5000
    "#;

    #[test]
    fn parses_a_scenario() -> Result<(), ScenarioError> {
        let s = Scenario::from_toml(CABLE_PULL, "cable.toml")?;
        assert_eq!(s.screens.len(), 2);
        assert!(s.screens[1].plugged);
        assert_eq!(s.steps.len(), 7);
        assert_eq!(s.steps[6].within_ms(), 5000);
        assert_eq!(s.steps[2].within_ms(), DEFAULT_WITHIN_MS);
        assert!(s.steps[2].is_expectation());
        Ok(())
    }

    fn invalid(text: &str) -> bool {
        matches!(
            Scenario::from_toml(text, "x.toml"),
            Err(ScenarioError::Invalid { .. })
        )
    }

    #[test]
    fn rejects_unknown_screens() {
        assert!(invalid(&CABLE_PULL.replace(
            r#"screen = "projector"
        since"#,
            r#"screen = "nope"
        since"#
        )));
    }

    #[test]
    fn rejects_input_before_launch() {
        assert!(invalid(&CABLE_PULL.replacen(
            r#"step = "launch""#,
            r#"step = "setup""#,
            1
        )));
    }

    #[test]
    fn rejects_an_unknown_mark() {
        assert!(invalid(
            &CABLE_PULL.replace(r#"since = "first""#, r#"since = "second""#)
        ));
    }

    #[test]
    fn rejects_unplugging_the_host() {
        assert!(invalid(&CABLE_PULL.replace(
            r#"step = "unplug"
        screen = "projector""#,
            r#"step = "unplug"
        screen = "laptop""#
        )));
    }

    #[test]
    fn rejects_unknown_fields() {
        let text = CABLE_PULL.replace("ms = 200", "ms = 200\n        seconds = 1");
        assert!(matches!(
            Scenario::from_toml(&text, "x.toml"),
            Err(ScenarioError::Parse { .. })
        ));
    }
}
