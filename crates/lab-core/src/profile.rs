//! Display profiles: what a fake screen claims to be, and how it behaves.
//!
//! A profile is shared by every app the lab tests. Identities should be a
//! small, fixed set: on macOS every distinct identity the lab ever presents
//! leaves a colour profile behind in the system for good, so a profile per
//! test run would litter the machine.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt;
use std::path::Path;

#[derive(Debug, thiserror::Error)]
pub enum ProfileError {
    #[error("reading {path}: {source}")]
    Io {
        path: String,
        source: std::io::Error,
    },
    #[error("parsing {path}: {source}")]
    Parse {
        path: String,
        source: Box<toml::de::Error>,
    },
    #[error("{path}: {message}")]
    Invalid { path: String, message: String },
    #[error("two profiles are named {0:?}")]
    Duplicate(String),
    #[error("no profile named {0:?}")]
    Unknown(String),
    #[error("mode {mode:?} is not one of profile {profile:?}'s modes ({available})")]
    NoSuchMode {
        profile: String,
        mode: String,
        available: String,
    },
    #[error("{0:?} is not a mode; expected WIDTHxHEIGHT or WIDTHxHEIGHT@HZ, e.g. 1024x768@60")]
    BadMode(String),
}

/// What the screen tells the OS about itself. On a real display this comes
/// from its EDID; apps that remember screens key on it.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Identity {
    pub vendor: u32,
    pub product: u32,
    pub serial: u32,
    pub name: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Mode {
    pub width: u32,
    pub height: u32,
    #[serde(default = "default_refresh")]
    pub refresh_hz: f64,
    #[serde(default)]
    pub hidpi: bool,
}

fn default_refresh() -> f64 {
    60.0
}

impl fmt::Display for Mode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}x{}@{}", self.width, self.height, self.refresh_hz)?;
        if self.hidpi {
            write!(f, " HiDPI")?;
        }
        Ok(())
    }
}

/// A mode as a scenario names it: size, and optionally the refresh rate.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ModeSpec {
    pub width: u32,
    pub height: u32,
    pub refresh_hz: Option<f64>,
}

impl ModeSpec {
    pub fn parse(text: &str) -> Result<ModeSpec, ProfileError> {
        let bad = || ProfileError::BadMode(text.to_string());
        let (size, refresh) = match text.split_once('@') {
            Some((size, hz)) => (size, Some(hz.trim().parse::<f64>().map_err(|_| bad())?)),
            None => (text, None),
        };
        let (w, h) = size.trim().split_once('x').ok_or_else(bad)?;
        let width = w.trim().parse().map_err(|_| bad())?;
        let height = h.trim().parse().map_err(|_| bad())?;
        if width == 0 || height == 0 {
            return Err(bad());
        }
        Ok(ModeSpec {
            width,
            height,
            refresh_hz: refresh,
        })
    }

    fn matches(&self, mode: &Mode) -> bool {
        self.width == mode.width
            && self.height == mode.height
            && self
                .refresh_hz
                .is_none_or(|hz| (hz - mode.refresh_hz).abs() < 0.01)
    }
}

/// How the device behaves over time, beyond what it claims to be.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Timings {
    /// How long after the screen is plugged in the OS reports it. Models a
    /// projector that takes its time to wake, or a slow HDMI handshake.
    #[serde(default)]
    pub report_delay_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Profile {
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub identity: Identity,
    /// The first mode is the one a screen comes up in unless a scenario says
    /// otherwise.
    pub modes: Vec<Mode>,
    #[serde(default)]
    pub timings: Timings,
}

impl Profile {
    pub fn from_toml(text: &str, path: &str) -> Result<Profile, ProfileError> {
        let profile: Profile = toml::from_str(text).map_err(|e| ProfileError::Parse {
            path: path.to_string(),
            source: Box::new(e),
        })?;
        profile.validate(path)?;
        Ok(profile)
    }

    fn validate(&self, path: &str) -> Result<(), ProfileError> {
        let invalid = |message: &str| ProfileError::Invalid {
            path: path.to_string(),
            message: message.to_string(),
        };
        if self.name.trim().is_empty() {
            return Err(invalid("name is empty"));
        }
        if self.modes.is_empty() {
            return Err(invalid("a profile needs at least one mode"));
        }
        for mode in &self.modes {
            if mode.width == 0 || mode.height == 0 {
                return Err(invalid("a mode has a zero width or height"));
            }
            if !(mode.refresh_hz > 0.0 && mode.refresh_hz.is_finite()) {
                return Err(invalid("a mode's refresh_hz must be a positive number"));
            }
        }
        Ok(())
    }

    /// The mode a scenario asked for, or the profile's default.
    pub fn mode(&self, spec: Option<&str>) -> Result<Mode, ProfileError> {
        let Some(text) = spec else {
            // validate() guarantees at least one mode.
            return Ok(self.modes[0]);
        };
        let wanted = ModeSpec::parse(text)?;
        self.modes
            .iter()
            .copied()
            .find(|m| wanted.matches(m))
            .ok_or_else(|| ProfileError::NoSuchMode {
                profile: self.name.clone(),
                mode: text.to_string(),
                available: self
                    .modes
                    .iter()
                    .map(|m| m.to_string())
                    .collect::<Vec<_>>()
                    .join(", "),
            })
    }
}

/// Every profile the lab knows, by name.
#[derive(Debug, Clone, Default)]
pub struct ProfileSet {
    profiles: BTreeMap<String, Profile>,
}

impl ProfileSet {
    pub fn insert(&mut self, profile: Profile) -> Result<(), ProfileError> {
        if self.profiles.contains_key(&profile.name) {
            return Err(ProfileError::Duplicate(profile.name));
        }
        self.profiles.insert(profile.name.clone(), profile);
        Ok(())
    }

    /// Add `profile`, replacing any profile of the same name. How a
    /// project's own profiles override the built-in ones.
    pub fn replace(&mut self, profile: Profile) {
        self.profiles.insert(profile.name.clone(), profile);
    }

    /// Every `*.toml` in `dir`, not recursive.
    pub fn load_dir(dir: &Path) -> Result<ProfileSet, ProfileError> {
        let io = |source| ProfileError::Io {
            path: dir.display().to_string(),
            source,
        };
        let mut paths: Vec<_> = std::fs::read_dir(dir)
            .map_err(io)?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|x| x == "toml"))
            .collect();
        paths.sort();

        let mut set = ProfileSet::default();
        for path in paths {
            let shown = path.display().to_string();
            let text = std::fs::read_to_string(&path).map_err(|source| ProfileError::Io {
                path: shown.clone(),
                source,
            })?;
            set.insert(Profile::from_toml(&text, &shown)?)?;
        }
        Ok(set)
    }

    pub fn get(&self, name: &str) -> Result<&Profile, ProfileError> {
        self.profiles
            .get(name)
            .ok_or_else(|| ProfileError::Unknown(name.to_string()))
    }

    pub fn iter(&self) -> impl Iterator<Item = &Profile> {
        self.profiles.values()
    }

    pub fn len(&self) -> usize {
        self.profiles.len()
    }

    pub fn is_empty(&self) -> bool {
        self.profiles.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const XGA: &str = r#"
        name = "xga-projector"
        [identity]
        vendor = 0x4C2D
        product = 0x0F00
        serial = 1
        name = "Test Projector"
        [[modes]]
        width = 1024
        height = 768
        [[modes]]
        width = 800
        height = 600
        refresh_hz = 59.94
        [timings]
        report_delay_ms = 250
    "#;

    #[test]
    fn parses_a_profile() -> Result<(), ProfileError> {
        let p = Profile::from_toml(XGA, "xga.toml")?;
        assert_eq!(p.identity.vendor, 0x4C2D);
        assert_eq!(p.modes[0].refresh_hz, 60.0);
        assert_eq!(p.timings.report_delay_ms, 250);
        Ok(())
    }

    #[test]
    fn picks_modes() -> Result<(), ProfileError> {
        let p = Profile::from_toml(XGA, "xga.toml")?;
        assert_eq!(p.mode(None)?.width, 1024);
        assert_eq!(p.mode(Some("800x600"))?.refresh_hz, 59.94);
        assert_eq!(p.mode(Some("800x600@59.94"))?.width, 800);
        assert!(matches!(
            p.mode(Some("1920x1080")),
            Err(ProfileError::NoSuchMode { .. })
        ));
        assert!(matches!(p.mode(Some("big")), Err(ProfileError::BadMode(_))));
        Ok(())
    }

    #[test]
    fn rejects_a_profile_without_modes() {
        let text = r#"
            name = "none"
            modes = []
            [identity]
            vendor = 1
            product = 1
            serial = 1
            name = "x"
        "#;
        assert!(matches!(
            Profile::from_toml(text, "none.toml"),
            Err(ProfileError::Invalid { .. })
        ));
    }

    #[test]
    fn refuses_duplicate_names() -> Result<(), ProfileError> {
        let mut set = ProfileSet::default();
        set.insert(Profile::from_toml(XGA, "a.toml")?)?;
        assert!(matches!(
            set.insert(Profile::from_toml(XGA, "b.toml")?),
            Err(ProfileError::Duplicate(_))
        ));
        Ok(())
    }
}
