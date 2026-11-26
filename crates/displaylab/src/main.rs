//! `displaylab`: run display scenarios against an app.
//!
//! Exit codes: 0 every scenario passed, 1 at least one failed, 2 the lab
//! itself could not do its job (bad file, missing backend, adapter would not
//! start). A CI script can tell "the app regressed" from "the lab is broken".
//!
//! The shipped profiles and scenarios are built in, so an installed
//! `displaylab` is a single file that works from any folder.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use lab_adapter::{Adapter, AdapterError, AppInfo, Capabilities, RpcAdapter};
use lab_core::{Capture, Displays, Profile, ProfileSet, Scenario, ScreenInfo};
use lab_fake::FakeBugs;
use lab_runner::{Report, Runner};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

mod builtin {
    include!(concat!(env!("OUT_DIR"), "/builtin.rs"));
}

const EXIT_FAIL: u8 = 1;
const EXIT_TOOL_ERROR: u8 = 2;

#[derive(Parser)]
#[command(
    name = "displaylab",
    version,
    about = "Test what an app puts on the screens."
)]
struct Cli {
    /// A folder of your own display profiles, added to the built-in ones
    /// (a profile with a built-in's name replaces it).
    #[arg(long, global = true)]
    profiles: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run scenarios and write a report for each.
    Run {
        /// Scenario files or folders, or built-in scenarios by name:
        /// `catalogue` runs the whole group, `cable-pull` or
        /// `catalogue/cable-pull` runs one. `displaylab scenarios` lists them.
        #[arg(required = true)]
        scenarios: Vec<String>,
        /// Which screens to use. `native` is this machine's own backend.
        #[arg(long, value_enum, default_value_t = BackendChoice::Native)]
        backend: BackendChoice,
        /// The app's adapter: a command (`--adapter "node adapter.js"`), or
        /// `mcp:<file>` to drive an app's own MCP server from a config file
        /// (`--adapter mcp:my-app.mcp.toml`). Without one, a real backend
        /// runs with no app: enough to check screens work (`run smoke`). Not
        /// used with the fake backend, which brings its own app.
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
        scenarios: Vec<String>,
    },
    /// List the display profiles.
    Profiles,
    /// List the built-in scenarios.
    Scenarios,
    /// Windows: install, check or remove the display driver that gives the
    /// lab its virtual screens.
    Driver {
        #[command(subcommand)]
        action: DriverAction,
    },
}

#[derive(Subcommand)]
enum DriverAction {
    /// Install it (administrator terminal). Trusts the certificate the
    /// driver is signed with, for code signing on this machine.
    Install {
        /// The driver package; by default the `driver` folder next to
        /// displaylab.exe, as the release zip has it.
        #[arg(long)]
        from: Option<PathBuf>,
    },
    /// Whether it is installed and answering.
    Status,
    /// Remove it, its device and its certificate (administrator terminal).
    Uninstall,
}

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
enum BackendChoice {
    Native,
    Fake,
    Mac,
    Windows,
}

fn main() -> ExitCode {
    // The macOS backend runs one copy of this executable per virtual screen.
    let mut args = std::env::args().skip(1);
    if args.next().as_deref() == Some(lab_mac::screen_host::ARG) {
        lab_mac::screen_host::run(args.collect());
    }
    // `__edid <profile> [mode]`: the driver plug arguments for a profile, for
    // the Windows driver's CI probe.
    let mut args = std::env::args().skip(1);
    if args.next().as_deref() == Some("__edid") {
        let rest: Vec<String> = args.collect();
        return match edid_args(&rest) {
            Ok(line) => {
                println!("{line}");
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("displaylab: {e:#}");
                ExitCode::from(EXIT_TOOL_ERROR)
            }
        };
    }
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
    if let Command::Driver { action } = &cli.command {
        let done = match action {
            DriverAction::Install { from } => {
                let dir = match from {
                    Some(dir) => dir.clone(),
                    None => std::env::current_exe()
                        .context("locating displaylab")?
                        .with_file_name("driver"),
                };
                lab_win::driver::install(&dir)?
            }
            DriverAction::Status => lab_win::driver::status()?,
            DriverAction::Uninstall => lab_win::driver::uninstall()?,
        };
        println!("{done}");
        return Ok(ExitCode::SUCCESS);
    }
    let profiles = load_profiles(cli.profiles.as_deref())?;
    match cli.command {
        Command::Driver { .. } => unreachable!("handled above"),
        Command::Scenarios => {
            let mut group = "";
            for (g, stem, text) in builtin::SCENARIOS {
                if *g != group {
                    group = g;
                    println!("{g}");
                }
                let scenario = Scenario::from_toml(text, stem)?;
                let flat = scenario
                    .description
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ");
                let summary = flat.split_inclusive(". ").next().unwrap_or("").trim();
                println!("  {stem:<24} {summary}");
            }
            println!();
            println!("Run one with `displaylab run <name>`, or a whole group with `displaylab run <group>`.");
            Ok(ExitCode::SUCCESS)
        }
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
            for (path, scenario) in &find_scenarios(&scenarios)? {
                check_profiles(scenario, &profiles, path)?;
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
            let scenarios = find_scenarios(&scenarios)?;
            for (path, scenario) in &scenarios {
                check_profiles(scenario, &profiles, path)?;
            }
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
    let scenario_dir = if path.is_file() {
        path.parent().map(Path::to_path_buf).unwrap_or_default()
    } else {
        PathBuf::new() // a built-in: no files of its own
    };
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
            // Load the adapter's config before touching any screen, so a
            // bad file costs nothing.
            let mcp = match adapter.and_then(|spec| spec.strip_prefix("mcp:")) {
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
            let app: Box<dyn Adapter> = match (mcp, adapter) {
                (Some(mcp), _) => Box::new(mcp),
                (None, None) => Box::new(NoApp::new()),
                (None, Some(spec)) => {
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

fn edid_args(args: &[String]) -> Result<String> {
    let Some(name) = args.first() else {
        bail!("usage: displaylab __edid <profile> [WxH@HZ]");
    };
    let profiles = load_profiles(None)?;
    let profile = profiles.get(name)?;
    let mode = profile.mode(args.get(1).map(String::as_str))?;
    let edid = lab_win::edid::build(&profile.identity, &mode)?;
    Ok(format!(
        "{} {}",
        lab_win::edid::hex(&edid),
        lab_win::edid::mode_arg(&mode)
    ))
}

/// The built-in profiles, then the ones in `dir`, which win on a name clash.
fn load_profiles(dir: Option<&Path>) -> Result<ProfileSet> {
    let mut set = ProfileSet::default();
    for (name, text) in builtin::PROFILES {
        set.insert(Profile::from_toml(text, &format!("built-in {name}"))?)?;
    }
    if let Some(dir) = dir {
        let mine = ProfileSet::load_dir(dir)
            .with_context(|| format!("loading profiles from {}", dir.display()))?;
        for p in mine.iter() {
            set.replace(p.clone());
        }
    }
    Ok(set)
}

/// Each argument as scenarios: a file, a folder of `*.toml`, a built-in
/// group (`catalogue`), or one built-in (`cable-pull`, `catalogue/cable-pull`).
/// A file or folder on disk wins over a built-in of the same name.
fn find_scenarios(args: &[String]) -> Result<Vec<(PathBuf, Scenario)>> {
    let mut out = vec![];
    for arg in args {
        let path = Path::new(arg);
        if path.is_file() {
            out.push((path.to_path_buf(), load_scenario(path)?));
            continue;
        }
        if path.is_dir() {
            let mut files: Vec<PathBuf> = std::fs::read_dir(path)
                .with_context(|| format!("reading {}", path.display()))?
                .filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| p.extension().is_some_and(|x| x == "toml"))
                .collect();
            files.sort();
            if files.is_empty() {
                bail!("{} has no scenario files (*.toml)", path.display());
            }
            for f in files {
                let s = load_scenario(&f)?;
                out.push((f, s));
            }
            continue;
        }
        let picked: Vec<_> = builtin::SCENARIOS
            .iter()
            .filter(|(group, stem, _)| {
                arg == group || arg == stem || *arg == format!("{group}/{stem}")
            })
            .collect();
        let groups: std::collections::BTreeSet<_> = picked.iter().map(|(g, _, _)| *g).collect();
        if picked.is_empty() {
            bail!(
                "no scenario file, folder or built-in scenario named {arg:?}; \
                 `displaylab scenarios` lists the built-ins"
            );
        }
        if groups.len() > 1 && !builtin::SCENARIOS.iter().any(|(g, _, _)| g == arg) {
            let names: Vec<String> = picked.iter().map(|(g, s, _)| format!("{g}/{s}")).collect();
            bail!(
                "{arg:?} is in more than one group; pick one: {}",
                names.join(", ")
            );
        }
        for (group, stem, text) in picked {
            let label = format!("built-in {group}/{stem}");
            // Built-ins refer to no files, so their folder is the current one.
            out.push((PathBuf::from(&label), Scenario::from_toml(text, &label)?));
        }
    }
    Ok(out)
}

/// The app for a run with no `--adapter`: launches nothing and refuses input.
/// Enough for scenarios that only look at the screens.
struct NoApp {
    info: AppInfo,
}

impl NoApp {
    fn new() -> NoApp {
        NoApp {
            info: AppInfo {
                name: "no app".into(),
                version: String::new(),
                protocol: "1.0".into(),
                capabilities: Capabilities::default(),
            },
        }
    }
}

impl Adapter for NoApp {
    fn info(&self) -> &AppInfo {
        &self.info
    }

    fn launch(&mut self, _screens: &[ScreenInfo]) -> Result<(), AdapterError> {
        Ok(())
    }

    fn input(&mut self, action: &str, _args: &serde_json::Value) -> Result<(), AdapterError> {
        Err(AdapterError::Remote {
            code: -32001,
            message: format!(
                "this run drives no app, so it cannot send {action:?}; \
                 pass --adapter to test an app"
            ),
        })
    }

    fn stop(&mut self) -> Result<(), AdapterError> {
        Ok(())
    }
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
    fn every_built_in_loads_and_names_real_profiles() {
        let profiles = load_profiles(None).unwrap();
        assert!(profiles.iter().count() >= 5);
        let all =
            find_scenarios(&["catalogue".into(), "smoke".into(), "fake-presenter".into()]).unwrap();
        assert!(all.len() >= 10, "{} built-in scenarios", all.len());
        for (path, s) in &all {
            check_profiles(s, &profiles, path).unwrap();
        }
    }

    #[test]
    fn scenarios_are_found_by_name() {
        let one = |arg: &str| find_scenarios(&[arg.into()]).map(|v| v.len());
        assert_eq!(one("cable-pull").ok(), Some(1));
        assert_eq!(one("catalogue/cable-pull").ok(), Some(1));
        assert!(one("catalogue").is_ok_and(|n| n >= 5));
        let e = one("no-such-thing").unwrap_err().to_string();
        assert!(e.contains("displaylab scenarios"), "{e}");
    }

    #[test]
    fn parses_bugs() {
        let b = parse_bugs(&["ignore-hotplug".into(), "latency=50".into()]).ok();
        assert!(b.is_some_and(|b| b.ignore_hotplug && b.input_latency_ms == 50));
        assert!(parse_bugs(&["nope".into()]).is_err());
    }
}
