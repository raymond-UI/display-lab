//! Two jobs.
//!
//! 1. On macOS, point the binary at the system Swift runtime. ScreenCaptureKit
//!    is reached through a Swift bridge, which needs `libswift_Concurrency.dylib`
//!    and friends from `/usr/lib/swift`. The `screencapturekit` crate sets that
//!    rpath in its own build script, but Cargo only applies a dependency's link
//!    arguments to the dependency's own targets, so every binary that links it
//!    has to set it again.
//! 2. Embed the shipped profiles and scenarios, so an installed `displaylab`
//!    is one file that works from any folder.
use std::fmt::Write as _;
use std::path::Path;

/// Scenario groups that ship inside the binary.
const GROUPS: [&str; 3] = ["catalogue", "smoke", "fake-presenter"];

fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        println!("cargo:rustc-link-arg=-Wl,-rpath,/usr/lib/swift");
    }

    let manifest = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_default();
    let root = Path::new(&manifest).join("../..");
    let mut out = String::new();

    out.push_str("/// `(file name, contents)` of every built-in profile.\n");
    out.push_str("pub const PROFILES: &[(&str, &str)] = &[\n");
    for (name, path) in tomls(&root.join("profiles")) {
        let _ = writeln!(out, "    ({name:?}, include_str!({path:?})),");
    }
    out.push_str("];\n\n");

    out.push_str("/// `(group, file stem, contents)` of every built-in scenario.\n");
    out.push_str("pub const SCENARIOS: &[(&str, &str, &str)] = &[\n");
    for group in GROUPS {
        for (name, path) in tomls(&root.join("scenarios").join(group)) {
            let stem = name.trim_end_matches(".toml");
            let _ = writeln!(out, "    ({group:?}, {stem:?}, include_str!({path:?})),");
        }
    }
    out.push_str("];\n");

    let dest = Path::new(&std::env::var("OUT_DIR").unwrap_or_default()).join("builtin.rs");
    if let Err(e) = std::fs::write(&dest, out) {
        panic!("writing {}: {e}", dest.display());
    }
}

/// `(file name, absolute path)` of each `*.toml` in `dir`, sorted.
fn tomls(dir: &Path) -> Vec<(String, String)> {
    println!("cargo:rerun-if-changed={}", dir.display());
    let Ok(entries) = std::fs::read_dir(dir) else {
        panic!("{} is missing", dir.display());
    };
    let mut files: Vec<(String, String)> = entries
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "toml"))
        .filter_map(|p| {
            let name = p.file_name()?.to_string_lossy().to_string();
            let abs = std::fs::canonicalize(&p).ok()?;
            Some((name, abs.display().to_string()))
        })
        .collect();
    files.sort();
    files
}
