//! EDIDs for the lab's virtual screens.
//!
//! On Windows a screen's identity is its EDID: the manufacturer, product
//! code, serial and name every app reads come from these 128 bytes. The
//! Display Lab driver takes the EDID with each plug, so it is built here from
//! the profile. Modes travel separately in the plug request; the EDID's one
//! detailed timing only has to be valid and preferred.
//!
//! Platform-independent, so it is tested everywhere.

use lab_core::{Identity, Mode};

/// The longest name an EDID holds (one display-name descriptor).
pub const NAME_LEN: usize = 13;

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum EdidError {
    #[error("vendor {0:#06x} does not fit an EDID manufacturer id (16 bits, top bit clear)")]
    Vendor(u32),
    #[error("product {0:#x} does not fit an EDID product code (16 bits)")]
    Product(u32),
    #[error("{0}x{1} @ {2} Hz is too fast for an EDID detailed timing (655 MHz pixel clock)")]
    TooFast(u32, u32, f64),
    #[error("not an EDID: {0}")]
    Invalid(&'static str),
}

const HEADER: [u8; 8] = [0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x00];

/// A 128-byte EDID 1.4 for `identity`, preferring `mode`.
pub fn build(identity: &Identity, mode: &Mode) -> Result<[u8; 128], EdidError> {
    if identity.vendor > 0x7FFF {
        return Err(EdidError::Vendor(identity.vendor));
    }
    let product =
        u16::try_from(identity.product).map_err(|_| EdidError::Product(identity.product))?;

    let mut e = [0u8; 128];
    e[..8].copy_from_slice(&HEADER);
    e[8..10].copy_from_slice(&(identity.vendor as u16).to_be_bytes());
    e[10..12].copy_from_slice(&product.to_le_bytes());
    e[12..16].copy_from_slice(&identity.serial.to_le_bytes());
    e[16] = 1; // week of manufacture
    e[17] = 36; // 2026 - 1990
    e[18] = 1; // EDID 1.4
    e[19] = 4;
    e[20] = 0xA2; // digital, 8 bits per colour, HDMI-a

    // The physical size of a 96 DPI screen. Windows picks a scale factor
    // from size and resolution, and scales big screens up as if they were
    // TVs; at 96 DPI it stays at 100%, so a capture is pixel for pixel what
    // the profile says.
    let (w_cm, h_cm) = physical_cm(mode);
    e[21] = w_cm;
    e[22] = h_cm;
    e[23] = 120; // gamma 2.2
    e[24] = 0x06; // RGB 4:4:4, sRGB default, preferred timing is native
    e[25..35].copy_from_slice(&[0xEE, 0x91, 0xA3, 0x54, 0x4C, 0x99, 0x26, 0x0F, 0x50, 0x54]);
    // 35..38: no established timings. 38..54: standard timings unused.
    for i in (38..54).step_by(2) {
        e[i] = 0x01;
        e[i + 1] = 0x01;
    }

    e[54..72].copy_from_slice(&detailed_timing(mode, w_cm, h_cm)?);
    e[72..90].copy_from_slice(&text_descriptor(0xFF, &identity.serial.to_string()));
    e[90..108].copy_from_slice(&text_descriptor(0xFC, &identity.name));
    e[108..126].copy_from_slice(&range_limits());
    e[126] = 0; // no extension blocks
    e[127] = checksum(&e);
    Ok(e)
}

/// What an EDID says about identity, as Windows and macOS read it: the
/// header's manufacturer, product and serial, and the display-name text.
pub fn parse(e: &[u8]) -> Result<Identity, EdidError> {
    if e.len() < 128 {
        return Err(EdidError::Invalid("shorter than 128 bytes"));
    }
    if e[..8] != HEADER {
        return Err(EdidError::Invalid("bad header"));
    }
    if checksum(&e[..128]) != e[127] {
        return Err(EdidError::Invalid("bad checksum"));
    }
    let name = (0..4)
        .map(|i| &e[54 + i * 18..72 + i * 18])
        .find(|d| d[0] == 0 && d[1] == 0 && d[3] == 0xFC)
        .map(|d| {
            let text: String = d[5..18]
                .iter()
                .take_while(|&&b| b != 0x0A)
                .map(|&b| b as char)
                .collect();
            text.trim_end().to_string()
        })
        .unwrap_or_default();
    Ok(Identity {
        vendor: u16::from_be_bytes([e[8], e[9]]) as u32,
        product: u16::from_le_bytes([e[10], e[11]]) as u32,
        serial: u32::from_le_bytes([e[12], e[13], e[14], e[15]]),
        name,
    })
}

/// `name` as an EDID can hold it: printable ASCII, at most [`NAME_LEN`]
/// characters. A screen's name on Windows is this, not the full profile name.
pub fn edid_name(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_graphic() || c == ' ' {
                c
            } else {
                '?'
            }
        })
        .take(NAME_LEN)
        .collect::<String>()
        .trim_end()
        .to_string()
}

/// The refresh rate the driver's plug request carries, in millihertz.
pub fn millihertz(mode: &Mode) -> u32 {
    (mode.refresh_hz * 1000.0).round() as u32
}

/// `mode` in the driver's plug syntax, `WxH@MILLIHZ`.
pub fn mode_arg(mode: &Mode) -> String {
    format!("{}x{}@{}", mode.width, mode.height, millihertz(mode))
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02X}")).collect()
}

fn checksum(e: &[u8]) -> u8 {
    let sum: u32 = e[..127].iter().map(|&b| b as u32).sum();
    ((256 - sum % 256) % 256) as u8
}

fn physical_cm(mode: &Mode) -> (u8, u8) {
    let cm = |px: u32| ((px as f64 / 96.0 * 2.54).round() as u32).clamp(1, 255) as u8;
    (cm(mode.width), cm(mode.height))
}

/// A CVT reduced-blanking timing for `mode`.
fn detailed_timing(mode: &Mode, w_cm: u8, h_cm: u8) -> Result<[u8; 18], EdidError> {
    let (w, h) = (mode.width, mode.height);
    let hz = mode.refresh_hz.max(1.0);
    let (h_blank, h_front, h_sync) = (160u32, 48u32, 32u32);
    // At least 460 us of vertical blanking, as CVT-RB asks.
    let line_time = 1.0 / (hz * (h as f64 + 30.0));
    let v_blank = ((460e-6 / line_time).ceil() as u32).max(10);
    let (v_front, v_sync) = (3u32, 5u32);
    let clock_10khz = ((w + h_blank) as f64 * (h + v_blank) as f64 * hz / 10_000.0).round();
    if clock_10khz > 65_535.0 || w > 4095 || h > 4095 {
        return Err(EdidError::TooFast(w, h, mode.refresh_hz));
    }
    let clock = clock_10khz as u16;
    let (w_mm, h_mm) = (w_cm as u32 * 10, h_cm as u32 * 10);

    let mut d = [0u8; 18];
    d[0..2].copy_from_slice(&clock.to_le_bytes());
    d[2] = w as u8;
    d[3] = h_blank as u8;
    d[4] = (((w >> 8) & 0xF) << 4 | ((h_blank >> 8) & 0xF)) as u8;
    d[5] = h as u8;
    d[6] = v_blank as u8;
    d[7] = (((h >> 8) & 0xF) << 4 | ((v_blank >> 8) & 0xF)) as u8;
    d[8] = h_front as u8;
    d[9] = h_sync as u8;
    d[10] = ((v_front & 0xF) << 4 | (v_sync & 0xF)) as u8;
    d[11] = (((h_front >> 8) & 3) << 6
        | ((h_sync >> 8) & 3) << 4
        | ((v_front >> 4) & 3) << 2
        | ((v_sync >> 4) & 3)) as u8;
    d[12] = w_mm as u8;
    d[13] = h_mm as u8;
    d[14] = (((w_mm >> 8) & 0xF) << 4 | ((h_mm >> 8) & 0xF)) as u8;
    d[17] = 0x1E; // digital separate sync, both polarities positive
    Ok(d)
}

/// A display descriptor carrying text: 0xFF serial, 0xFC name.
fn text_descriptor(tag: u8, text: &str) -> [u8; 18] {
    let mut d = [0u8; 18];
    d[3] = tag;
    let text = edid_name(text);
    let bytes = text.as_bytes();
    d[5..5 + bytes.len()].copy_from_slice(bytes);
    if bytes.len() < NAME_LEN {
        d[5 + bytes.len()] = 0x0A;
        for b in &mut d[6 + bytes.len()..18] {
            *b = 0x20;
        }
    }
    d
}

/// Wide limits, so no mode the lab asks for is out of range.
fn range_limits() -> [u8; 18] {
    [
        0, 0, 0, 0xFD, 0, //
        23, 240, // vertical Hz
        15, 255,  // horizontal kHz
        66,   // max pixel clock, 10 MHz units
        0x01, // no timing formula
        0x0A, 0x20, 0x20, 0x20, 0x20, 0x20, 0x20,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity(name: &str) -> Identity {
        Identity {
            vendor: 0x1D10,
            product: 0x0500,
            serial: 2001,
            name: name.into(),
        }
    }

    fn mode(w: u32, h: u32, hz: f64) -> Mode {
        Mode {
            width: w,
            height: h,
            refresh_hz: hz,
            hidpi: false,
        }
    }

    #[test]
    fn identity_round_trips() {
        let e = build(&identity("Lab Projector"), &mode(1920, 1080, 60.0)).unwrap();
        assert_eq!(parse(&e).unwrap(), identity("Lab Projector"));
        let sum: u32 = e.iter().map(|&b| b as u32).sum();
        assert_eq!(sum % 256, 0, "checksum");
    }

    #[test]
    fn long_names_are_cut_to_what_an_edid_holds() {
        let e = build(&identity("Lab HD Projector"), &mode(1920, 1080, 60.0)).unwrap();
        assert_eq!(parse(&e).unwrap().name, "Lab HD Projec");
        assert_eq!(edid_name("Lab HD Projector"), "Lab HD Projec");
        assert_eq!(edid_name("Café"), "Caf?");
    }

    #[test]
    fn the_preferred_timing_is_the_mode() {
        for (w, h, hz) in [
            (1920, 1080, 60.0),
            (1024, 768, 60.0),
            (3840, 2160, 60.0),
            (1280, 720, 59.94),
        ] {
            let e = build(&identity("x"), &mode(w, h, hz)).unwrap();
            let d = &e[54..72];
            let active_w = d[2] as u32 | ((d[4] as u32 >> 4) << 8);
            let active_h = d[5] as u32 | ((d[7] as u32 >> 4) << 8);
            assert_eq!((active_w, active_h), (w, h));
            let clock = u16::from_le_bytes([d[0], d[1]]) as f64 * 10_000.0;
            let blank_w = d[3] as u32 | ((d[4] as u32 & 0xF) << 8);
            let blank_h = d[6] as u32 | ((d[7] as u32 & 0xF) << 8);
            let refresh = clock / ((w + blank_w) as f64 * (h + blank_h) as f64);
            assert!((refresh - hz).abs() < 0.1, "{w}x{h}: {refresh} Hz");
        }
    }

    #[test]
    fn known_mfg_letters() {
        // 0x10AC is DEL in the EDID's packed five-bit letters.
        let mut id = identity("x");
        id.vendor = 0x10AC;
        let e = build(&id, &mode(1920, 1080, 60.0)).unwrap();
        let v = u16::from_be_bytes([e[8], e[9]]);
        let letter = |shift: u16| (((v >> shift) & 31) as u8 + b'A' - 1) as char;
        assert_eq!(format!("{}{}{}", letter(10), letter(5), letter(0)), "DEL");
    }

    #[test]
    fn out_of_range_values_are_refused() {
        let mut id = identity("x");
        id.vendor = 0x1_0000;
        assert!(build(&id, &mode(1920, 1080, 60.0)).is_err());
        assert!(build(&identity("x"), &mode(7680, 4320, 60.0)).is_err());
        assert!(parse(&[0u8; 128]).is_err());
    }

    #[test]
    fn physical_size_is_96_dpi() {
        let e = build(&identity("x"), &mode(1920, 1080, 60.0)).unwrap();
        assert_eq!((e[21], e[22]), (51, 29));
    }

    #[test]
    fn plug_arguments() {
        assert_eq!(mode_arg(&mode(1920, 1080, 59.94)), "1920x1080@59940");
        assert_eq!(hex(&[0, 0xAB]), "00AB");
    }
}
