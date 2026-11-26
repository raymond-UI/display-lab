//! Naming the app that needs the Screen Recording permission.
//!
//! macOS grants Screen Recording to the app bundle a process belongs to, not
//! to the `displaylab` binary: the terminal, the editor, or whatever agent
//! launched it. The lab names that app by walking up its parent processes to
//! the first one inside a `.app` bundle. `TERM_PROGRAM` is the fallback, for
//! when no ancestor is visible (tmux, ssh).
//!
//! Kept free of platform calls so it is tested on every OS.

use std::path::Path;

/// The app a process belongs to, from its executable's path: the outermost
/// `.app` bundle, so a helper inside an editor names the editor.
pub fn app_from_path(path: &Path) -> Option<String> {
    path.components().find_map(|c| {
        let name = c.as_os_str().to_str()?;
        let app = name.strip_suffix(".app")?;
        (!app.is_empty()).then(|| app.to_string())
    })
}

/// The first app along a chain of executable paths, starting at this
/// process and going up through its parents.
pub fn app_from_ancestors<I, P>(paths: I) -> Option<String>
where
    I: IntoIterator<Item = P>,
    P: AsRef<Path>,
{
    paths.into_iter().find_map(|p| app_from_path(p.as_ref()))
}

/// A readable name for the terminal `TERM_PROGRAM` names.
pub fn app_from_term_program(term: Option<&str>) -> Option<String> {
    let name = match term? {
        "" => return None,
        "Apple_Terminal" => "Terminal",
        "iTerm.app" => "iTerm",
        "ghostty" => "Ghostty",
        "vscode" => "Visual Studio Code (or Cursor)",
        "WarpTerminal" => "Warp",
        other => other,
    };
    Some(name.to_string())
}

/// What to do when Screen Recording is not allowed.
pub fn help(app: Option<&str>) -> String {
    let app = app.unwrap_or("the app you are running displaylab from");
    format!(
        "Display Lab needs the Screen Recording permission to watch the screens.\n  \
         Open System Settings > Privacy & Security > Screen & System Audio Recording,\n  \
         turn on {app}, then quit and reopen it and run this again."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_app_is_named_by_its_outermost_bundle() {
        let p = Path::new("/Applications/Terminal.app/Contents/MacOS/Terminal");
        assert_eq!(app_from_path(p).as_deref(), Some("Terminal"));
        let helper = Path::new(
            "/Applications/Visual Studio Code.app/Contents/Frameworks/\
             Code Helper.app/Contents/MacOS/Code Helper",
        );
        assert_eq!(app_from_path(helper).as_deref(), Some("Visual Studio Code"));
        let spaced = Path::new("/Applications/T3 Code (Nightly).app/Contents/MacOS/T3 Code");
        assert_eq!(app_from_path(spaced).as_deref(), Some("T3 Code (Nightly)"));
    }

    #[test]
    fn a_plain_binary_names_no_app() {
        assert_eq!(app_from_path(Path::new("/bin/zsh")), None);
        assert_eq!(app_from_path(Path::new("/opt/x/.app/bin")), None);
        assert_eq!(app_from_path(Path::new("/opt/x/apps/bin")), None);
    }

    #[test]
    fn the_nearest_ancestor_in_an_app_wins() {
        let chain = [
            "/usr/local/bin/displaylab",
            "/bin/zsh",
            "/usr/local/bin/agent",
            "/Applications/Ghostty.app/Contents/MacOS/ghostty",
            "/Applications/Other.app/Contents/MacOS/Other",
        ];
        assert_eq!(app_from_ancestors(chain).as_deref(), Some("Ghostty"));
        assert_eq!(app_from_ancestors(["/bin/zsh", "/sbin/launchd"]), None);
    }

    #[test]
    fn term_program_is_a_fallback_name() {
        assert_eq!(
            app_from_term_program(Some("Apple_Terminal")).as_deref(),
            Some("Terminal")
        );
        assert_eq!(app_from_term_program(Some("tmux")).as_deref(), Some("tmux"));
        assert_eq!(app_from_term_program(Some("")), None);
        assert_eq!(app_from_term_program(None), None);
    }

    #[test]
    fn help_names_the_app_or_says_which_one_generically() {
        assert!(help(Some("Warp")).contains("turn on Warp, then"));
        assert!(help(None).contains("turn on the app you are running displaylab from"));
    }
}
