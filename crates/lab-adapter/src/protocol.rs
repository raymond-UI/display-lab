//! Wire types. `docs/PROTOCOL.md` is the prose version of this file; keep
//! them in step.

use base64::Engine;
use lab_core::{CompareSpec, Frame, ScreenInfo};
use serde::{Deserialize, Serialize};

/// The protocol version this lab speaks. An adapter that answers `initialize`
/// with a different major version is refused.
pub const PROTOCOL_VERSION: &str = "1.0";

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Capabilities {
    #[serde(default)]
    pub setup: bool,
    #[serde(default)]
    pub state: bool,
    #[serde(default)]
    pub bindings: bool,
    #[serde(default)]
    pub expected_frame: bool,
    /// The app's own compare thresholds, used instead of the lab's defaults.
    #[serde(default)]
    pub compare: Option<CompareSpec>,
}

/// The answer to `initialize`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AppInfo {
    pub name: String,
    #[serde(default)]
    pub version: String,
    /// The app's macOS bundle id. When set, the macOS backend captures only
    /// this app's windows, so the desktop, other apps and the OS's own marks
    /// never count as the app's picture.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bundle_id: Option<String>,
    pub protocol: String,
    #[serde(default)]
    pub capabilities: Capabilities,
}

/// Which screen an output is on, as the app knows it. Apps describe screens
/// differently (an EDID hash, a name, a position), so every field is
/// optional and the lab matches on the ones that are set. At least one must
/// be set.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ScreenRef {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vendor: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub product: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub serial: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<(i32, i32)>,
}

impl ScreenRef {
    pub fn is_empty(&self) -> bool {
        self.name.is_none()
            && self.vendor.is_none()
            && self.product.is_none()
            && self.serial.is_none()
            && self.origin.is_none()
    }

    /// Every field that is set agrees with `screen`. An empty reference
    /// matches nothing.
    pub fn matches(&self, screen: &ScreenInfo) -> bool {
        if self.is_empty() {
            return false;
        }
        let id = &screen.identity;
        self.name.as_ref().is_none_or(|n| *n == id.name)
            && self.vendor.is_none_or(|v| v == id.vendor)
            && self.product.is_none_or(|p| p == id.product)
            && self.serial.is_none_or(|s| s == id.serial)
            && self.origin.is_none_or(|o| o == screen.origin)
    }

    pub fn of(screen: &ScreenInfo) -> ScreenRef {
        ScreenRef {
            name: Some(screen.identity.name.clone()),
            vendor: Some(screen.identity.vendor),
            product: Some(screen.identity.product),
            serial: Some(screen.identity.serial),
            origin: Some(screen.origin),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Binding {
    pub output: String,
    /// `None`: the output is configured but drawn on no screen.
    #[serde(default)]
    pub screen: Option<ScreenRef>,
}

/// A frame on the wire: RGBA8, row-major, base64.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WireFrame {
    pub width: u32,
    pub height: u32,
    pub rgba_base64: String,
}

impl WireFrame {
    pub fn encode(frame: &Frame) -> WireFrame {
        WireFrame {
            width: frame.width,
            height: frame.height,
            rgba_base64: base64::engine::general_purpose::STANDARD.encode(&frame.rgba),
        }
    }

    pub fn decode(&self) -> Result<Frame, String> {
        let rgba = base64::engine::general_purpose::STANDARD
            .decode(&self.rgba_base64)
            .map_err(|e| format!("rgba_base64: {e}"))?;
        let frame = Frame {
            width: self.width,
            height: self.height,
            rgba,
            present_ns: 0,
        };
        if frame.width == 0 || frame.height == 0 || !frame.is_well_formed() {
            return Err(format!(
                "a {}x{} frame needs {} bytes, got {}",
                frame.width,
                frame.height,
                frame.pixel_count() * 4,
                frame.rgba.len()
            ));
        }
        Ok(frame)
    }
}

/// Major versions must agree.
pub fn compatible(version: &str) -> bool {
    let major = |v: &str| v.split('.').next().map(str::to_string);
    major(version) == major(PROTOCOL_VERSION)
}

#[cfg(test)]
mod tests {
    use super::*;
    use lab_core::{Identity, Mode, ScreenId};

    fn screen() -> ScreenInfo {
        ScreenInfo {
            id: ScreenId(1),
            identity: Identity {
                vendor: 7,
                product: 8,
                serial: 9,
                name: "Projector".into(),
            },
            mode: Mode {
                width: 1024,
                height: 768,
                refresh_hz: 60.0,
                hidpi: false,
            },
            origin: (1920, 0),
            host: false,
        }
    }

    #[test]
    fn a_ref_matches_on_the_fields_it_sets() {
        let s = screen();
        assert!(ScreenRef {
            name: Some("Projector".into()),
            ..Default::default()
        }
        .matches(&s));
        assert!(ScreenRef {
            serial: Some(9),
            origin: Some((1920, 0)),
            ..Default::default()
        }
        .matches(&s));
        assert!(!ScreenRef {
            name: Some("Projector".into()),
            serial: Some(10),
            ..Default::default()
        }
        .matches(&s));
        assert!(
            !ScreenRef::default().matches(&s),
            "an empty ref matches nothing"
        );
        assert!(ScreenRef::of(&s).matches(&s));
    }

    #[test]
    fn frames_round_trip() {
        let f = Frame::solid(3, 2, [1, 2, 3, 4]);
        let back = WireFrame::encode(&f).decode().unwrap();
        assert!(back.same_pixels(&f));
    }

    #[test]
    fn a_short_frame_is_refused() {
        let mut w = WireFrame::encode(&Frame::solid(3, 2, [1, 2, 3, 4]));
        w.height = 3;
        assert!(w.decode().is_err());
    }

    #[test]
    fn versions() {
        assert!(compatible("1.0"));
        assert!(compatible("1.4"));
        assert!(!compatible("2.0"));
    }
}
