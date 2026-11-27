//! The handful of public CoreGraphics calls the backend needs, declared by
//! hand rather than pulling in a bindings crate for ten functions.

use std::ffi::c_void;

pub type DisplayId = u32;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct CGPoint {
    pub x: f64,
    pub y: f64,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct CGSize {
    pub width: f64,
    pub height: f64,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct CGRect {
    pub origin: CGPoint,
    pub size: CGSize,
}

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGGetActiveDisplayList(max: u32, displays: *mut DisplayId, count: *mut u32) -> i32;
    fn CGMainDisplayID() -> DisplayId;
    fn CGDisplayIsBuiltin(display: DisplayId) -> u32;
    fn CGDisplayVendorNumber(display: DisplayId) -> u32;
    fn CGDisplayModelNumber(display: DisplayId) -> u32;
    fn CGDisplaySerialNumber(display: DisplayId) -> u32;
    fn CGDisplayBounds(display: DisplayId) -> CGRect;
    fn CGDisplayCopyDisplayMode(display: DisplayId) -> *mut c_void;
    fn CGDisplayModeGetPixelWidth(mode: *mut c_void) -> usize;
    fn CGDisplayModeGetPixelHeight(mode: *mut c_void) -> usize;
    fn CGDisplayModeGetWidth(mode: *mut c_void) -> usize;
    fn CGDisplayModeGetRefreshRate(mode: *mut c_void) -> f64;
    fn CGDisplayModeRelease(mode: *mut c_void);
    fn CGDisplayPixelsWide(display: DisplayId) -> usize;
    fn CGDisplayPixelsHigh(display: DisplayId) -> usize;
    fn CGDisplayIsActive(display: DisplayId) -> u32;
    fn CGPreflightScreenCaptureAccess() -> bool;
    fn CGRequestScreenCaptureAccess() -> bool;
}

/// Whether this process may capture the screen. The first time it is not,
/// macOS is asked to show its own prompt (and to list this app in Settings),
/// which it does once per app.
pub fn screen_capture_allowed() -> bool {
    // SAFETY: no arguments; both only read or request a TCC permission.
    unsafe { CGPreflightScreenCaptureAccess() || CGRequestScreenCaptureAccess() }
}

/// Every display the window server is drawing to right now.
pub fn active_displays() -> Vec<DisplayId> {
    let mut ids = [0u32; 64];
    let mut count = 0u32;
    // SAFETY: the buffer holds 64 ids and we pass 64 as its capacity.
    let err = unsafe { CGGetActiveDisplayList(ids.len() as u32, ids.as_mut_ptr(), &mut count) };
    if err != 0 {
        return vec![];
    }
    ids[..count as usize].to_vec()
}

pub fn main_display() -> DisplayId {
    // SAFETY: no arguments, no preconditions.
    unsafe { CGMainDisplayID() }
}

pub fn is_builtin(id: DisplayId) -> bool {
    // SAFETY: any id is accepted; an unknown one answers false.
    unsafe { CGDisplayIsBuiltin(id) != 0 }
}

pub struct Facts {
    pub vendor: u32,
    pub product: u32,
    pub serial: u32,
    pub origin: (i32, i32),
    pub width: u32,
    pub height: u32,
    pub refresh_hz: f64,
    pub hidpi: bool,
}

/// What CoreGraphics says about a display, or `None` if it has gone.
///
/// A `CGVirtualDisplay` on a hosted macOS 26 runner is active and has bounds
/// but answers `CGDisplayCopyDisplayMode` with nothing, so the mode object is
/// optional: without it, size comes from the pixel counts (or the bounds) and
/// the refresh rate is left for the caller to fill in.
pub fn facts(id: DisplayId) -> Option<Facts> {
    // SAFETY: each call takes any display id; a copied mode is released below.
    unsafe {
        if CGDisplayIsActive(id) == 0 {
            return None;
        }
        let bounds = CGDisplayBounds(id);
        let origin = (bounds.origin.x as i32, bounds.origin.y as i32);
        let (vendor, product, serial) = (
            CGDisplayVendorNumber(id),
            CGDisplayModelNumber(id),
            CGDisplaySerialNumber(id),
        );
        let mode = CGDisplayCopyDisplayMode(id);
        let (width, height, refresh_hz, hidpi) = if mode.is_null() {
            let (mut w, mut h) = (CGDisplayPixelsWide(id), CGDisplayPixelsHigh(id));
            if w == 0 || h == 0 {
                (w, h) = (bounds.size.width as usize, bounds.size.height as usize);
            }
            (w, h, 0.0, false)
        } else {
            let pixels = CGDisplayModeGetPixelWidth(mode);
            let points = CGDisplayModeGetWidth(mode);
            let facts = (
                pixels,
                CGDisplayModeGetPixelHeight(mode),
                // Virtual displays often report 0 Hz; the caller fills it in.
                CGDisplayModeGetRefreshRate(mode),
                points > 0 && pixels > points,
            );
            CGDisplayModeRelease(mode);
            facts
        };
        if width == 0 || height == 0 {
            return None;
        }
        Some(Facts {
            vendor,
            product,
            serial,
            origin,
            width: width as u32,
            height: height as u32,
            refresh_hz,
            hidpi,
        })
    }
}

unsafe extern "C" {
    fn clock_gettime_nsec_np(clock: u32) -> u64;
}

/// `CLOCK_UPTIME_RAW`, the clock ScreenCaptureKit's presentation timestamps
/// (host time) and Rust's `Instant` both run on.
const CLOCK_UPTIME_RAW: u32 = 8;

/// Host time now, in nanoseconds.
pub fn host_now_ns() -> u64 {
    // SAFETY: a valid clock id; the call has no other preconditions.
    unsafe { clock_gettime_nsec_np(CLOCK_UPTIME_RAW) }
}

unsafe extern "C" {
    fn proc_pidpath(pid: i32, buffer: *mut c_void, buffersize: u32) -> i32;
    fn proc_pidinfo(pid: i32, flavor: i32, arg: u64, buffer: *mut c_void, buffersize: i32) -> i32;
}

/// `PROC_PIDPATHINFO_MAXSIZE` in `<sys/proc_info.h>`.
const PROC_PIDPATHINFO_MAXSIZE: usize = 4 * 1024;
/// `PROC_PIDT_SHORTBSDINFO` in `<sys/proc_info.h>`.
const PROC_PIDT_SHORTBSDINFO: i32 = 13;

/// `struct proc_bsdshortinfo`. Only the parent id is read.
#[repr(C)]
#[derive(Default)]
struct ProcBsdShortInfo {
    _pid: u32,
    ppid: u32,
    _pgid: u32,
    _status: u32,
    _comm: [u8; 16],
    _flags: u32,
    _uid: u32,
    _gid: u32,
    _ruid: u32,
    _rgid: u32,
    _svuid: u32,
    _svgid: u32,
    _rfu: u32,
}

/// The executable a process runs, or `None` if it is gone or not ours to see.
pub fn process_path(pid: i32) -> Option<std::path::PathBuf> {
    let mut buf = vec![0u8; PROC_PIDPATHINFO_MAXSIZE];
    // SAFETY: the buffer holds PROC_PIDPATHINFO_MAXSIZE bytes, as passed.
    let len = unsafe { proc_pidpath(pid, buf.as_mut_ptr().cast(), buf.len() as u32) };
    if len <= 0 {
        return None;
    }
    buf.truncate(len as usize);
    use std::os::unix::ffi::OsStringExt;
    Some(std::ffi::OsString::from_vec(buf).into())
}

/// A process's parent, or `None` if it cannot be read.
pub fn parent_pid(pid: i32) -> Option<i32> {
    let mut info = ProcBsdShortInfo::default();
    let size = std::mem::size_of::<ProcBsdShortInfo>() as i32;
    // SAFETY: `info` is a `proc_bsdshortinfo` of exactly `size` bytes.
    let got = unsafe {
        proc_pidinfo(
            pid,
            PROC_PIDT_SHORTBSDINFO,
            0,
            (&mut info as *mut ProcBsdShortInfo).cast(),
            size,
        )
    };
    (got == size).then_some(info.ppid as i32)
}

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGGetOnlineDisplayList(max: u32, displays: *mut DisplayId, count: *mut u32) -> i32;
    fn CGDisplayIsInMirrorSet(display: DisplayId) -> u32;
    fn CGDisplayMirrorsDisplay(display: DisplayId) -> DisplayId;
    fn CGBeginDisplayConfiguration(config: *mut *mut c_void) -> i32;
    fn CGConfigureDisplayMirrorOfDisplay(
        config: *mut c_void,
        display: DisplayId,
        master: DisplayId,
    ) -> i32;
    fn CGCompleteDisplayConfiguration(config: *mut c_void, option: u32) -> i32;
    fn CGCancelDisplayConfiguration(config: *mut c_void) -> i32;
}

/// `kCGNullDirectDisplay`: mirror nothing.
const NULL_DISPLAY: DisplayId = 0;
/// `kCGConfigureForAppOnly`: the change is undone when this process exits.
const CONFIGURE_FOR_APP_ONLY: u32 = 0;

/// Whether `display` mirrors another display or is mirrored by one.
pub fn in_mirror_set(display: DisplayId) -> bool {
    // SAFETY: takes any display id.
    unsafe { CGDisplayIsInMirrorSet(display) != 0 }
}

/// Take `display` out of any mirror set, so it extends the desktop: it stops
/// mirroring anything, and every display that mirrors it stops too. The
/// change lasts only as long as this process.
pub fn stop_mirroring(display: DisplayId) -> Result<(), String> {
    let mut ids = [0u32; 64];
    let mut count = 0u32;
    // SAFETY: the buffer holds 64 ids and we pass 64 as its capacity.
    let err = unsafe { CGGetOnlineDisplayList(ids.len() as u32, ids.as_mut_ptr(), &mut count) };
    if err != 0 {
        return Err(format!("listing online displays failed ({err})"));
    }
    let online = &ids[..count as usize];
    // SAFETY: takes any display id.
    let mirrors = |d: DisplayId| unsafe { CGDisplayMirrorsDisplay(d) };
    let targets: Vec<DisplayId> = online
        .iter()
        .copied()
        .filter(|&d| (d == display && mirrors(d) != NULL_DISPLAY) || mirrors(d) == display)
        .collect();
    if targets.is_empty() {
        return Ok(());
    }
    let mut config: *mut c_void = std::ptr::null_mut();
    // SAFETY: `config` is written by Begin and only used until Complete or
    // Cancel, which both release it.
    unsafe {
        let err = CGBeginDisplayConfiguration(&mut config);
        if err != 0 || config.is_null() {
            return Err(format!("starting a display configuration failed ({err})"));
        }
        for d in targets {
            let err = CGConfigureDisplayMirrorOfDisplay(config, d, NULL_DISPLAY);
            if err != 0 {
                CGCancelDisplayConfiguration(config);
                return Err(format!("unmirroring display {d} failed ({err})"));
            }
        }
        let err = CGCompleteDisplayConfiguration(config, CONFIGURE_FOR_APP_ONLY);
        if err != 0 {
            return Err(format!("applying the display configuration failed ({err})"));
        }
    }
    Ok(())
}

/// Every online display with its active and mirror state, for debug logs.
pub fn describe_displays() -> String {
    let mut ids = [0u32; 64];
    let mut count = 0u32;
    // SAFETY: the buffer holds 64 ids and we pass 64 as its capacity.
    let err = unsafe { CGGetOnlineDisplayList(ids.len() as u32, ids.as_mut_ptr(), &mut count) };
    if err != 0 {
        return format!("(listing online displays failed: {err})");
    }
    ids[..count as usize]
        .iter()
        .map(|&d| {
            // SAFETY: each takes any display id.
            let (active, mirrored, mirrors) = unsafe {
                (
                    CGDisplayIsActive(d) != 0,
                    CGDisplayIsInMirrorSet(d) != 0,
                    CGDisplayMirrorsDisplay(d),
                )
            };
            let mut s = format!("{d}");
            s.push_str(if active { " active" } else { " inactive" });
            if mirrored {
                s.push_str(" in-mirror-set");
            }
            if mirrors != NULL_DISPLAY {
                s.push_str(&format!(" mirrors {mirrors}"));
            }
            s
        })
        .collect::<Vec<_>>()
        .join(", ")
}
