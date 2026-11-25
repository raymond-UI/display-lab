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
pub fn facts(id: DisplayId) -> Option<Facts> {
    // SAFETY: each call takes any display id; the mode is released below.
    unsafe {
        let mode = CGDisplayCopyDisplayMode(id);
        if mode.is_null() {
            return None;
        }
        let pixels = CGDisplayModeGetPixelWidth(mode);
        let points = CGDisplayModeGetWidth(mode);
        let facts = Facts {
            vendor: CGDisplayVendorNumber(id),
            product: CGDisplayModelNumber(id),
            serial: CGDisplaySerialNumber(id),
            origin: {
                let b = CGDisplayBounds(id);
                (b.origin.x as i32, b.origin.y as i32)
            },
            width: pixels as u32,
            height: CGDisplayModeGetPixelHeight(mode) as u32,
            // Virtual displays often report 0 Hz; the caller fills it in.
            refresh_hz: CGDisplayModeGetRefreshRate(mode),
            hidpi: points > 0 && pixels > points,
        };
        CGDisplayModeRelease(mode);
        Some(facts)
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
