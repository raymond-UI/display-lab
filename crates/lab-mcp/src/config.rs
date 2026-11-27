//! The TOML file that maps lab requests to an app's MCP tools.
//!
//! Every section except `[server]` and `[actions]` is optional, and the
//! adapter offers exactly the capabilities whose sections are present.

use lab_core::CompareSpec;
use serde::Deserialize;
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    // Not a `source`: the message already carries it, and a chained source
    // would print it twice.
    #[error("{path}: {err}")]
    Io { path: String, err: std::io::Error },
    #[error("{path}: {message}")]
    Parse { path: String, message: String },
    #[error("{path}: {message}")]
    Invalid { path: String, message: String },
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default)]
    pub app: AppSection,
    pub server: ServerSection,
    #[serde(default)]
    pub setup: Option<Call>,
    #[serde(default)]
    pub actions: BTreeMap<String, Call>,
    #[serde(default)]
    pub state: Option<StateSection>,
    #[serde(default)]
    pub outputs: Option<OutputsSection>,
    #[serde(default)]
    pub bindings: Option<BindingsSection>,
    #[serde(default)]
    pub expected_frame: Option<Call>,
    #[serde(default)]
    pub compare: Option<CompareSpec>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppSection {
    /// Shown in reports.
    #[serde(default)]
    pub name: Option<String>,
    /// The app's macOS bundle id: the lab then captures only its windows.
    #[serde(default)]
    pub bundle_id: Option<String>,
    /// The command that starts the app. Without it, the app must already be
    /// running (or be the stdio server itself).
    #[serde(default)]
    pub launch: Vec<String>,
    /// Called until it succeeds after the app starts; the app counts as up
    /// once it does. Without it, a completed MCP handshake is enough.
    #[serde(default)]
    pub ready: Option<Call>,
    /// How long the app gets to come up.
    #[serde(default = "default_ready_timeout")]
    pub ready_timeout_s: u64,
    /// Called once the app is up, for example to open a test document.
    #[serde(default)]
    pub start: Option<Call>,
    /// Called before the lab stops the app.
    #[serde(default)]
    pub stop: Option<Call>,
}

fn default_ready_timeout() -> u64 {
    30
}

impl Default for AppSection {
    fn default() -> AppSection {
        AppSection {
            name: None,
            bundle_id: None,
            launch: vec![],
            ready: None,
            ready_timeout_s: default_ready_timeout(),
            start: None,
            stop: None,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerSection {
    /// Streamable HTTP endpoint, e.g. `http://127.0.0.1:8000/mcp`.
    #[serde(default)]
    pub url: Option<String>,
    /// A server to spawn and talk to over stdio.
    #[serde(default)]
    pub command: Vec<String>,
    /// The MCP revision to ask for in `initialize`.
    #[serde(default = "default_protocol")]
    pub protocol: String,
    /// Per-call limit. Apps that ask a person to approve a change can take a
    /// while to answer.
    #[serde(default = "default_timeout")]
    pub timeout_s: u64,
}

pub const DEFAULT_PROTOCOL: &str = "2025-06-18";

fn default_protocol() -> String {
    DEFAULT_PROTOCOL.into()
}

fn default_timeout() -> u64 {
    60
}

/// One tool call. `args` is a template: see [`crate::template`].
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Call {
    pub tool: String,
    #[serde(default = "empty_object")]
    pub args: Value,
}

fn empty_object() -> Value {
    Value::Object(Default::default())
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StateSection {
    pub tool: String,
    #[serde(default = "empty_object")]
    pub args: Value,
    /// A JSON pointer into the result; the whole result if absent.
    #[serde(default)]
    pub pointer: Option<String>,
}

/// How to find the app's outputs and tell which is which.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OutputsSection {
    pub tool: String,
    #[serde(default = "empty_object")]
    pub args: Value,
    /// Pointer to the array of outputs in the result.
    pub list: String,
    /// Pointer, inside one output, to the field that says which output it is.
    pub key: String,
    /// Lab output name to the app's value at `key`. Without an entry, the
    /// value itself is the lab name.
    #[serde(default)]
    pub names: BTreeMap<String, Value>,
}

/// Pointers, inside one output, to the screen it draws on.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BindingsSection {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub vendor: Option<String>,
    #[serde(default)]
    pub product: Option<String>,
    #[serde(default)]
    pub serial: Option<String>,
    #[serde(default)]
    pub x: Option<String>,
    #[serde(default)]
    pub y: Option<String>,
    /// A boolean; when it is false the output is unbound whatever else is
    /// set.
    #[serde(default)]
    pub connected: Option<String>,
    /// For apps whose outputs name the screen they were configured for,
    /// connected or not: a second list of the screens that are connected now.
    /// An output is bound only when its screen is in that list, and the
    /// screen fields above are then read from the matching live screen.
    #[serde(default)]
    pub screens: Option<ScreensJoin>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScreensJoin {
    pub tool: String,
    #[serde(default = "empty_object")]
    pub args: Value,
    /// Pointer to the array of connected screens in the result.
    pub list: String,
    /// Pointer, inside one live screen, to the value that identifies it.
    pub key: String,
    /// Pointer, inside one output, to the same value.
    pub output_key: String,
}

impl BindingsSection {
    fn pointers(&self) -> impl Iterator<Item = &String> {
        [
            &self.name,
            &self.vendor,
            &self.product,
            &self.serial,
            &self.x,
            &self.y,
            &self.connected,
        ]
        .into_iter()
        .flatten()
    }
}

impl Config {
    pub fn load(path: &Path) -> Result<Config, ConfigError> {
        let shown = path.display().to_string();
        let text = std::fs::read_to_string(path).map_err(|err| ConfigError::Io {
            path: shown.clone(),
            err,
        })?;
        Config::from_toml(&text, &shown)
    }

    pub fn from_toml(text: &str, label: &str) -> Result<Config, ConfigError> {
        let config: Config = toml::from_str(text).map_err(|e| ConfigError::Parse {
            path: label.into(),
            message: e.to_string(),
        })?;
        config.validate().map_err(|message| ConfigError::Invalid {
            path: label.into(),
            message,
        })?;
        Ok(config)
    }

    fn validate(&self) -> Result<(), String> {
        match (&self.server.url, self.server.command.is_empty()) {
            (Some(_), false) => return Err("[server] takes url or command, not both".into()),
            (None, true) => return Err("[server] needs url or command".into()),
            (Some(url), true) if !url.starts_with("http://") && !url.starts_with("https://") => {
                return Err(format!("[server] url {url:?} must be http:// or https://"))
            }
            _ => {}
        }
        if self.actions.is_empty() {
            return Err("[actions] must map at least one lab action to a tool".into());
        }
        if self.bindings.is_some() && self.outputs.is_none() {
            return Err("[bindings] reads the [outputs] list, so it needs [outputs]".into());
        }
        if let Some(b) = &self.bindings {
            if b.pointers().next().is_none() {
                return Err("[bindings] needs at least one screen field".into());
            }
        }
        let mut pointers: Vec<&String> = vec![];
        if let Some(o) = &self.outputs {
            pointers.extend([&o.list, &o.key]);
        }
        if let Some(b) = &self.bindings {
            pointers.extend(b.pointers());
            if let Some(j) = &b.screens {
                pointers.extend([&j.list, &j.key, &j.output_key]);
            }
        }
        if let Some(p) = self.state.as_ref().and_then(|s| s.pointer.as_ref()) {
            pointers.push(p);
        }
        for p in pointers {
            if !(p.is_empty() || p.starts_with('/')) {
                return Err(format!(
                    "{p:?} is not a JSON pointer; pointers start with / (\"\" is the whole value)"
                ));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINIMAL: &str = r#"
[server]
url = "http://127.0.0.1:9/mcp"
[actions]
next = { tool = "next" }
"#;

    #[test]
    fn minimal_config_loads_with_defaults() {
        let c = Config::from_toml(MINIMAL, "t").unwrap();
        assert_eq!(c.server.protocol, DEFAULT_PROTOCOL);
        assert_eq!(c.server.timeout_s, 60);
        assert_eq!(c.app.ready_timeout_s, 30);
        assert_eq!(c.actions["next"].args, serde_json::json!({}));
    }

    #[test]
    fn the_app_can_name_its_bundle_id() {
        assert_eq!(Config::from_toml(MINIMAL, "t").unwrap().app.bundle_id, None);
        let text = format!("{MINIMAL}\n[app]\nbundle_id = \"org.example.app\"");
        let c = Config::from_toml(&text, "t").unwrap();
        assert_eq!(c.app.bundle_id.as_deref(), Some("org.example.app"));
    }

    #[test]
    fn mistakes_are_named() {
        let bad = |text: &str, needle: &str| {
            let e = Config::from_toml(text, "t").unwrap_err().to_string();
            assert!(e.contains(needle), "{e:?} should mention {needle:?}");
        };
        bad("[actions]\nnext = { tool = \"n\" }", "server");
        bad("[server]\nurl = \"http://x/mcp\"", "[actions]");
        bad(
            "[server]\nurl = \"ftp://x\"\n[actions]\nn = { tool = \"n\" }",
            "http://",
        );
        bad(
            &format!("{MINIMAL}\n[bindings]\nname = \"/screen/name\""),
            "[outputs]",
        );
        bad(
            &format!("{MINIMAL}\n[outputs]\ntool = \"l\"\nlist = \"outputs\"\nkey = \"/kind\""),
            "JSON pointer",
        );
        bad(&format!("{MINIMAL}\n[surprise]\nx = 1"), "surprise");
    }
}
