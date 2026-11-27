//! The parts of the Windows backend that are plain logic: talking to the
//! driver, finding a lab screen among the displays Windows reports, reading
//! a monitor's registry key, turning captured pixels into RGBA. Kept apart
//! from the Win32 calls so it is tested on every platform.

use lab_core::Identity;

/// The driver's control pipe.
pub const PIPE: &str = r"\\.\pipe\DisplayLabIdd";

/// How many screens the driver can hold at once (its `MaxSlots`).
pub const SLOTS: u8 = 16;

/// A display Windows has on the desktop right now.
#[derive(Debug, Clone, PartialEq)]
pub struct Display {
    /// `\\.\DISPLAY3`: what DXGI calls the output.
    pub gdi_name: String,
    /// The monitor's device interface path, which also names its registry key.
    pub device_path: String,
    /// From the monitor's EDID, when Windows has one.
    pub identity: Option<Identity>,
    pub origin: (i32, i32),
    pub width: u32,
    pub height: u32,
    pub refresh_hz: f64,
}

/// How the backend recognises one of its screens among the displays.
#[derive(Debug, Clone, PartialEq)]
pub enum Locator {
    /// A screen the lab plugged: its EDID identity, which is unique per
    /// profile. The name is left out; an EDID holds only 13 characters of it.
    Plugged {
        vendor: u32,
        product: u32,
        serial: u32,
    },
    /// The machine's own screen, by its device path.
    Host { device_path: String },
}

impl Locator {
    pub fn for_identity(id: &Identity) -> Locator {
        Locator::Plugged {
            vendor: id.vendor,
            product: id.product,
            serial: id.serial,
        }
    }

    pub fn find<'a>(&self, displays: &'a [Display]) -> Option<&'a Display> {
        displays.iter().find(|d| match self {
            Locator::Plugged {
                vendor,
                product,
                serial,
            } => d.identity.as_ref().is_some_and(|i| {
                i.vendor == *vendor && i.product == *product && i.serial == *serial
            }),
            Locator::Host { device_path } => d.device_path == *device_path,
        })
    }
}

/// The machine's own screen: the one at the desktop origin, else the first.
/// Only meaningful while the lab has nothing plugged in.
pub fn host_display(displays: &[Display]) -> Option<&Display> {
    displays
        .iter()
        .find(|d| d.origin == (0, 0))
        .or_else(|| displays.first())
}

/// Another display showing the same picture as `d`. Windows mirrors by
/// driving two monitors from one desktop source, so both carry one GDI name.
pub fn mirror_of<'a>(displays: &'a [Display], d: &Display) -> Option<&'a Display> {
    displays
        .iter()
        .find(|o| o.gdi_name == d.gdi_name && o.device_path != d.device_path)
}

/// The common name of the certificate release drivers are signed with.
pub const CERT_NAME: &str = "Display Lab driver";

/// Whether a certificate named `name` signs a Display Lab driver: the
/// release certificate, or one named after it for CI or testing
/// (`Display Lab driver (CI)`).
pub fn is_driver_certificate(name: &str) -> bool {
    name == CERT_NAME
        || name
            .strip_prefix(CERT_NAME)
            .is_some_and(|rest| rest.starts_with(" ("))
}

/// Bytes as upper-case hex, the way Windows shows a thumbprint.
pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02X}")).collect()
}

/// The first slot not in `used`.
pub fn free_slot(used: impl IntoIterator<Item = u8>) -> Option<u8> {
    let used: Vec<u8> = used.into_iter().collect();
    (0..SLOTS).find(|s| !used.contains(s))
}

/// The registry key holding a monitor's EDID, from its device path:
/// `\\?\DISPLAY#GHP0500#1&28a6823a&0&UID256#{e6f07b5f-...}` is
/// `SYSTEM\CurrentControlSet\Enum\DISPLAY\GHP0500\1&28a6823a&0&UID256\Device Parameters`.
pub fn edid_key(device_path: &str) -> Option<String> {
    let rest = device_path.strip_prefix(r"\\?\")?;
    let instance = rest.split("#{").next()?;
    let parts: Vec<&str> = instance.split('#').collect();
    if parts.len() != 3 || parts.iter().any(|p| p.is_empty()) {
        return None;
    }
    Some(format!(
        r"SYSTEM\CurrentControlSet\Enum\{}\{}\{}\Device Parameters",
        parts[0], parts[1], parts[2]
    ))
}

/// The driver's answer: `OK ...` is the text after `OK`, `ERR why` is an
/// error.
pub fn parse_reply(line: &str) -> Result<String, String> {
    let line = line.trim();
    if line == "OK" {
        return Ok(String::new());
    }
    if let Some(rest) = line.strip_prefix("OK ") {
        return Ok(rest.to_string());
    }
    Err(line
        .strip_prefix("ERR ")
        .map(str::to_string)
        .unwrap_or_else(|| format!("the driver said {line:?}")))
}

/// The slots `LIST` reports.
pub fn parse_slots(text: &str) -> Vec<u8> {
    text.split(',')
        .filter_map(|s| s.trim().parse().ok())
        .collect()
}

/// Rows of BGRA with `pitch` bytes each, as RGBA with full alpha.
pub fn bgra_to_rgba(bgra: &[u8], pitch: usize, width: usize, height: usize) -> Option<Vec<u8>> {
    if pitch < width * 4 || bgra.len() < pitch * height.saturating_sub(1) + width * 4 {
        return None;
    }
    let mut rgba = Vec::with_capacity(width * height * 4);
    for y in 0..height {
        let row = &bgra[y * pitch..y * pitch + width * 4];
        for p in row.chunks_exact(4) {
            rgba.extend_from_slice(&[p[2], p[1], p[0], 255]);
        }
    }
    Some(rgba)
}

/// A performance-counter time on the lab clock, given the counter and the lab
/// clock read together just now.
pub fn qpc_to_lab_ns(qpc: i64, qpc_now: i64, frequency: i64, lab_now_ns: u64) -> u64 {
    if qpc <= 0 || frequency <= 0 {
        return lab_now_ns;
    }
    let ago_ns = (qpc_now - qpc) as i128 * 1_000_000_000 / frequency as i128;
    (lab_now_ns as i128 - ago_ns).clamp(1, u64::MAX as i128) as u64
}

/// From `pnputil /enum-drivers`, the published names (`oemNN.inf`) of the
/// packages whose original INF is `original`.
pub fn published_infs(enum_drivers: &str, original: &str) -> Vec<String> {
    let mut out = vec![];
    let mut published: Option<String> = None;
    for line in enum_drivers.lines() {
        let Some((key, value)) = line.split_once(':') else {
            published = None;
            continue;
        };
        let (key, value) = (key.trim(), value.trim());
        if key.eq_ignore_ascii_case("Published Name") {
            published = Some(value.to_string());
        } else if key.eq_ignore_ascii_case("Original Name") && value.eq_ignore_ascii_case(original)
        {
            out.extend(published.take());
        }
    }
    out
}

/// UTF-16 up to the first NUL.
pub fn wide(text: &[u16]) -> String {
    let end = text.iter().position(|&c| c == 0).unwrap_or(text.len());
    String::from_utf16_lossy(&text[..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn display(path: &str, serial: Option<u32>, origin: (i32, i32)) -> Display {
        Display {
            gdi_name: r"\\.\DISPLAY1".into(),
            device_path: path.into(),
            identity: serial.map(|serial| Identity {
                vendor: 0x1D10,
                product: 0x0500,
                serial,
                name: "Lab HD Projec".into(),
            }),
            origin,
            width: 1920,
            height: 1080,
            refresh_hz: 60.0,
        }
    }

    #[test]
    fn screens_are_found_by_identity_not_name() {
        let displays = [
            display("host", None, (0, 0)),
            display("a", Some(2001), (1024, 0)),
            display("b", Some(2002), (2944, 0)),
        ];
        let want = Identity {
            vendor: 0x1D10,
            product: 0x0500,
            serial: 2002,
            name: "Lab HD Projector".into(),
        };
        assert_eq!(
            Locator::for_identity(&want)
                .find(&displays)
                .unwrap()
                .device_path,
            "b"
        );
        let host = Locator::Host {
            device_path: "host".into(),
        };
        assert_eq!(host.find(&displays).unwrap().origin, (0, 0));
        assert_eq!(host_display(&displays).unwrap().device_path, "host");
        assert!(Locator::Plugged {
            vendor: 1,
            product: 2,
            serial: 3
        }
        .find(&displays)
        .is_none());
    }

    #[test]
    fn a_mirrored_screen_shares_its_source() {
        let host = display("host", None, (0, 0));
        let mut lab = display("lab", Some(2001), (0, 0));
        let other = Display {
            gdi_name: r"\\.\DISPLAY6".into(),
            ..display("other", Some(4001), (1920, 0))
        };
        let all = [host.clone(), lab.clone(), other.clone()];
        assert_eq!(mirror_of(&all, &lab).unwrap().device_path, "host");
        assert!(mirror_of(&all, &other).is_none());

        lab.gdi_name = r"\\.\DISPLAY5".into();
        let all = [host, lab.clone(), other];
        assert!(mirror_of(&all, &lab).is_none());
    }

    #[test]
    fn driver_certificates_by_name() {
        assert!(is_driver_certificate("Display Lab driver"));
        assert!(is_driver_certificate("Display Lab driver (CI)"));
        assert!(!is_driver_certificate("Display Lab drivers"));
        assert!(!is_driver_certificate("Display Lab"));
        assert!(!is_driver_certificate("Some other driver"));
        assert_eq!(hex(&[0x6F, 0x1E, 0x03]), "6F1E03");
    }

    #[test]
    fn registry_keys_from_device_paths() {
        assert_eq!(
            edid_key(
                r"\\?\DISPLAY#GHP0500#1&28a6823a&0&UID256#{e6f07b5f-ee97-4a90-b076-33f57bf4eaa7}"
            )
            .as_deref(),
            Some(
                r"SYSTEM\CurrentControlSet\Enum\DISPLAY\GHP0500\1&28a6823a&0&UID256\Device Parameters"
            )
        );
        assert_eq!(edid_key("DISPLAY#X"), None);
        assert_eq!(edid_key(r"\\?\DISPLAY#X#{guid}"), None);
    }

    #[test]
    fn driver_replies() {
        assert_eq!(
            parse_reply("OK displaylab-idd 1\n"),
            Ok("displaylab-idd 1".into())
        );
        assert_eq!(parse_reply("OK"), Ok(String::new()));
        assert_eq!(parse_reply("ERR slot in use"), Err("slot in use".into()));
        assert!(parse_reply("huh").is_err());
        assert_eq!(parse_slots("0,2,15"), vec![0, 2, 15]);
        assert!(parse_slots("").is_empty());
        assert_eq!(free_slot([0, 1, 3]), Some(2));
        assert_eq!(free_slot(0..SLOTS), None);
    }

    #[test]
    fn driver_packages_from_pnputil() {
        let text = "Microsoft PnP Utility\r\n\r\nPublished Name:     oem3.inf\r\nOriginal Name:      other.inf\r\n\r\n\
                    Published Name:     oem12.inf\r\nOriginal Name:      DisplayLabIdd.inf\r\nProvider Name:      Display Lab\r\n";
        assert_eq!(
            published_infs(text, "displaylabidd.inf"),
            vec!["oem12.inf".to_string()]
        );
        assert!(published_infs("", "x.inf").is_empty());
    }

    #[test]
    fn pixels_and_clocks() {
        // Two pixels per row, a pitch of 12 bytes (4 bytes of padding).
        let bgra = [
            1, 2, 3, 0, 4, 5, 6, 0, 9, 9, 9, 9, 7, 8, 9, 0, 10, 11, 12, 0,
        ];
        let rgba = bgra_to_rgba(&bgra, 12, 2, 2).unwrap();
        assert_eq!(
            rgba,
            [3, 2, 1, 255, 6, 5, 4, 255, 9, 8, 7, 255, 12, 11, 10, 255]
        );
        assert!(bgra_to_rgba(&bgra, 4, 2, 2).is_none());

        // A frame presented 10 ms ago on a 10 MHz counter.
        assert_eq!(
            qpc_to_lab_ns(900_000, 1_000_000, 10_000_000, 50_000_000),
            40_000_000
        );
        assert_eq!(qpc_to_lab_ns(0, 1, 1, 7), 7);
        assert_eq!(wide(&[72, 105, 0, 65]), "Hi");
    }
}
