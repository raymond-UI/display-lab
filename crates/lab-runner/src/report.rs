//! What a run produces: one report per scenario, as JSON for tools and as
//! Markdown for people.

use serde::{Deserialize, Serialize};
use std::fmt::Write as _;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    /// An action that ran.
    Done,
    /// An expectation that held.
    Pass,
    Fail,
    /// Not run, because an earlier step failed.
    Skipped,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepReport {
    /// 1-based, as people count scenario steps.
    pub index: usize,
    pub kind: String,
    /// The step in a few words, e.g. `unplug projector`.
    pub label: String,
    pub outcome: Outcome,
    pub elapsed_ms: u64,
    /// Why it failed, or what it measured.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// Files written for this step (captured, expected and diff frames),
    /// relative to the report.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifacts: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Latency {
    pub step: usize,
    pub screen: String,
    /// The mark the latency was measured from, or `last input`.
    pub since: String,
    pub ms: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Report {
    pub scenario: String,
    pub app: String,
    pub backend: String,
    pub passed: bool,
    /// A failure before or around the steps: a missing profile, a capability
    /// the app lacks, a backend that would not plug a screen.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Facts about how the run looked at the screens, for whoever reads the
    /// verdict: for example, that it captured only the app's windows.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
    pub steps: Vec<StepReport>,
    pub latencies: Vec<Latency>,
    pub duration_ms: u64,
}

impl Report {
    pub fn markdown(&self) -> String {
        let mut out = String::new();
        let verdict = if self.passed { "PASS" } else { "FAIL" };
        let _ = writeln!(out, "# {verdict}: {}\n", self.scenario);
        let _ = writeln!(
            out,
            "App `{}` on the `{}` backend, {} ms.\n",
            self.app, self.backend, self.duration_ms
        );
        for note in &self.notes {
            let _ = writeln!(out, "{note}\n");
        }
        if let Some(error) = &self.error {
            let _ = writeln!(out, "**Error:** {error}\n");
        }
        if !self.steps.is_empty() {
            let _ = writeln!(out, "| # | Step | Result | ms | Detail |");
            let _ = writeln!(out, "| --- | --- | --- | --- | --- |");
            for s in &self.steps {
                let result = match s.outcome {
                    Outcome::Done => "done",
                    Outcome::Pass => "pass",
                    Outcome::Fail => "**FAIL**",
                    Outcome::Skipped => "skipped",
                };
                let mut detail = s.detail.clone().unwrap_or_default().replace('|', "\\|");
                for a in &s.artifacts {
                    let _ = write!(detail, " [{a}]({a})");
                }
                let _ = writeln!(
                    out,
                    "| {} | {} | {result} | {} | {} |",
                    s.index,
                    s.label.replace('|', "\\|"),
                    s.elapsed_ms,
                    detail.trim()
                );
            }
            out.push('\n');
        }
        if !self.latencies.is_empty() {
            let _ = writeln!(out, "## Latency\n");
            let _ = writeln!(out, "| Step | Screen | Since | ms |");
            let _ = writeln!(out, "| --- | --- | --- | --- |");
            for l in &self.latencies {
                let _ = writeln!(
                    out,
                    "| {} | {} | {} | {:.1} |",
                    l.step, l.screen, l.since, l.ms
                );
            }
        }
        out
    }
}
