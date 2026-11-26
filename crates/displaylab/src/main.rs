//! `displaylab`: run display scenarios against an app.
//!
//! Exit codes: 0 every scenario passed, 1 at least one failed, 2 the lab
//! itself could not do its job (bad file, missing backend, adapter would not
//! start). A CI script can tell "the app regressed" from "the lab is broken".

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use lab_adapter::{Adapter, RpcAdapter};
use lab_core::{Capture, Displays, ProfileSet, Scenario};
use lab_fake::FakeBugs;
use lab_runner::{Report, Runner};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

const EXIT_FAIL: u8 = 1;
const EXIT_TOOL_ERROR: u8 = 2;

#[derive(Parser)]
#[command(
    name = "displaylab",
    version,
    about = "Test what an app puts on the screens."
)]
struct Cli {
    /// Directory of display profiles.
    #[arg(long, global = true, default_value = "profiles")]
    profiles: PathBuf,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run scenarios and write a report for each.
    Run {
        /// Scenario files.
        #[arg(required = true)]
        scenarios: Vec<PathBuf>,
        /// Which screens to use. `native` is this machine's own backend.
        #[arg(long, value_enum, default_value_t = BackendChoice::Native)]
        backend: BackendChoice,
        /// The app's adapter: a command (`--adapter "node adapter.js"`), or
        /// `mcp:<file>` to drive an app's own MCP server from a config file
        /// (`--adapter mcp:my-app.mcp.toml`). Not used with the fake backend,
        /// which brings its own app.
        #[arg(long)]
        adapter: Option<String>,
        /// Make the fake app misbehave: ignore-hotplug, bind-by-name,
        /// latency=MS. Repeatable.
        #[arg(long = "fake-bug")]
        fake_bugs: Vec<String>,
        /// Where reports and failing frames go, one folder per scenario.
        #[arg(long, default_value = "out")]
        out: PathBuf,
    },
    /// Check scenario files and profiles without running anything.
    Validate {
        #[arg(required = true)]
        scenarios: Vec<PathBuf>,
    },
    /// List the display profiles.
    Profiles,
}

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
enum BackendChoice {
    Native,
    Fake,
    Mac,
    Windows,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match dispatch(cli) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("displaylab: {e:#}");
            ExitCode::from(EXIT_TOOL_ERROR)
        }
    }
}

fn dispatch(cli: Cli) -> Result<ExitCode> {
    let profiles = ProfileSet::load_dir(&cli.profiles)
        .with_context(|| format!("loading profiles from {}", cli.profiles.display()))?;
    match cli.command {
        Command::Profiles => {
            for p in profiles.iter() {
                let modes: Vec<String> = p.modes.iter().map(|m| m.to_string()).collect();
                println!(
                    "{:<24} {:<28} {}",
                    p.name,
                    p.identity.name,
                    modes.join(", ")
                );
                if !p.description.is_empty() {
                    println!("{:<24} {}", "", p.description);
                }
            }
            Ok(ExitCode::SUCCESS)
        }
        Command::Validate { scenarios } => {
            for path in &scenarios {
                let scenario = load_scenario(path)?;
                check_profiles(&scenario, &profiles, path)?;
                println!(
                    "ok  {}  ({} screens, {} steps)",
                    path.display(),
                    scenario.screens.len(),
                    scenario.steps.len()
                );
            }
            Ok(ExitCode::SUCCESS)
        }
        Command::Run {
            scenarios,
            backend,
            adapter,
            fake_bugs,
            out,
        } => {
            let scenarios: Vec<(PathBuf, Scenario)> = scenarios
                .into_iter()
                .map(|p| load_scenario(&p).map(|s| (p, s)))
                .collect::<Result<_>>()?;
            let backend = resolve(backend);
            let bugs = parse_bugs(&fake_bugs)?;
            if backend != BackendChoice::Fake && !fake_bugs.is_empty() {
                bail!("--fake-bug only applies to the fake backend");
            }

            let mut all_passed = true;
            for (path, scenario) in &scenarios {
                let dir = out.join(&scenario.name);
                std::fs::create_dir_all(&dir)
                    .with_context(|| format!("creating {}", dir.display()))?;
                let report = run_one(
                    backend,
                    adapter.as_deref(),
                    bugs,
                    &profiles,
                    path,
                    scenario,
                    &dir,
                )?;
                write_report(&report, &dir)?;
                let verdict = if report.passed { "PASS" } else { "FAIL" };
                println!(
                    "{verdict}  {}  ({} ms)  {}",
                    scenario.name,
                    report.duration_ms,
                    dir.join("report.md").display()
                );
                if let Some(e) = &report.error {
                    println!("      {e}");
                }
                for s in report
                    .steps
                    .iter()
                    .filter(|s| s.outcome == lab_runner::Outcome::Fail)
                {
                    println!(
                        "      step {}: {}: {}",
                        s.index,
                        s.label,
                        s.detail.as_deref().unwrap_or("")
                    );
                }
                all_passed &= report.passed;
            }
            Ok(if all_passed {
                ExitCode::SUCCESS
            } else {
                ExitCode::from(EXIT_FAIL)
            })
        }
    }
}

fn resolve(choice: BackendChoice) -> BackendChoice {
    match choice {
        BackendChoice::Native if cfg!(target_os = "macos") => BackendChoice::Mac,
        BackendChoice::Native if cfg!(target_os = "windows") => BackendChoice::Windows,
        // No native backend here; say so when it is used, not now, so
        // `validate` and the fake backend still work on Linux.
        other => other,
    }
}

fn run_one(
    backend: BackendChoice,
    adapter: Option<&str>,
    bugs: FakeBugs,
    profiles: &ProfileSet,
    path: &Path,
    scenario: &Scenario,
    artifacts: &Path,
) -> Result<Report> {
    let scenario_dir = path.parent().map(Path::to_path_buf).unwrap_or_default();
    let backend_name = match backend {
        BackendChoice::Fake => "fake",
        BackendChoice::Mac => "mac",
        BackendChoice::Windows => "windows",
        BackendChoice::Native => "native",
    };

    let (mut displays, mut capture, mut app): (
        Box<dyn Displays>,
        Box<dyn Capture>,
        Box<dyn Adapter>,
    ) = match backend {
        BackendChoice::Fake => {
            if adapter.is_some() {
                bail!(
                    "the fake backend runs its own fake app; drop --adapter or pick a real backend"
                );
            }
            let (d, c, a) = lab_fake::fake(bugs);
            (Box::new(d), Box::new(c), Box::new(a))
        }
        real => {
            let Some(spec) = adapter else {
                bail!("a real backend needs the app's adapter: --adapter \"<command>\" or --adapter mcp:<file>");
            };
            // Load the adapter's config before touching any screen, so a
            // bad file costs nothing.
            let mcp = match spec.strip_prefix("mcp:") {
                Some(file) => Some(
                    lab_mcp::McpAdapter::load(Path::new(file))
                        .with_context(|| format!("loading MCP adapter config {file:?}"))?,
                ),
                None => None,
            };
            let (d, c) = match real {
                BackendChoice::Mac => lab_mac::backend(),
                BackendChoice::Windows => lab_win::backend(),
                _ => bail!("this machine has no native display backend; use --backend fake"),
            }
            .context("starting the display backend")?;
            let app: Box<dyn Adapter> = match mcp {
                Some(mcp) => Box::new(mcp),
                None => {
                    let words = split_command(spec)?;
                    Box::new(
                        RpcAdapter::spawn(&words)
                            .with_context(|| format!("starting adapter {spec:?}"))?,
                    )
                }
            };
            (d, c, app)
        }
    };

    let mut runner = Runner {
        displays: displays.as_mut(),
        capture: capture.as_mut(),
        adapter: app.as_mut(),
        profiles,
        backend: backend_name.into(),
        artifacts: Some(artifacts.to_path_buf()),
        scenario_dir,
    };
    Ok(runner.run(scenario))
}

fn load_scenario(path: &Path) -> Result<Scenario> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    Ok(Scenario::from_toml(&text, &path.display().to_string())?)
}

fn check_profiles(scenario: &Scenario, profiles: &ProfileSet, path: &Path) -> Result<()> {
    for decl in &scenario.screens {
        let profile = profiles
            .get(&decl.profile)
            .with_context(|| format!("{}: screen {:?}", path.display(), decl.id))?;
        profile
            .mode(decl.mode.as_deref())
            .with_context(|| format!("{}: screen {:?}", path.display(), decl.id))?;
    }
    Ok(())
}

fn write_report(report: &Report, dir: &Path) -> Result<()> {
    let json = serde_json::to_string_pretty(report)?;
    std::fs::write(dir.join("report.json"), json)?;
    std::fs::write(dir.join("report.md"), report.markdown())?;
    Ok(())
}

fn parse_bugs(flags: &[String]) -> Result<FakeBugs> {
    let mut bugs = FakeBugs::default();
    for flag in flags {
        match flag.split_once('=') {
            None if flag == "ignore-hotplug" => bugs.ignore_hotplug = true,
            None if flag == "bind-by-name" => bugs.bind_by_name = true,
            Some(("latency", ms)) => {
                bugs.input_latency_ms = ms.parse().with_context(|| format!("--fake-bug {flag}"))?
            }
            _ => bail!(
                "unknown --fake-bug {flag:?}; expected ignore-hotplug, bind-by-name or latency=MS"
            ),
        }
    }
    Ok(bugs)
}

/// Split an adapter command into words. Double quotes group words; there is
/// no other escaping. Enough for `node "my adapter.js" --port 9`; anything
/// fancier belongs in a script.
fn split_command(command: &str) -> Result<Vec<String>> {
    let mut words = vec![];
    let mut word = String::new();
    let mut quoted = false;
    let mut started = false;
    for c in command.chars() {
        match c {
            '"' => {
                quoted = !quoted;
                started = true;
            }
            c if c.is_whitespace() && !quoted => {
                if started {
                    words.push(std::mem::take(&mut word));
                    started = false;
                }
            }
            c => {
                word.push(c);
                started = true;
            }
        }
    }
    if quoted {
        bail!("unbalanced quote in adapter command {command:?}");
    }
    if started {
        words.push(word);
    }
    if words.is_empty() {
        bail!("empty adapter command");
    }
    Ok(words)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_commands() {
        assert_eq!(
            split_command(r#"node "my adapter.js"  --x 1"#).ok(),
            Some(vec![
                "node".to_string(),
                "my adapter.js".into(),
                "--x".into(),
                "1".into()
            ])
        );
        assert!(split_command(r#"node "open"#).is_err());
        assert!(split_command("   ").is_err());
    }

    #[test]
    fn parses_bugs() {
        let b = parse_bugs(&["ignore-hotplug".into(), "latency=50".into()]).ok();
        assert!(b.is_some_and(|b| b.ignore_hotplug && b.input_latency_ms == 50));
        assert!(parse_bugs(&["nope".into()]).is_err());
    }
}
