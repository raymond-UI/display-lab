//! The macOS backend.
//!
//! Screens are `CGVirtualDisplay`s, a private CoreGraphics API: macOS has no
//! public way to create a display short of a DriverKit extension, which
//! needs an entitlement Apple grants case by case. Every virtual-display app
//! uses these same four classes. Facts this design rests on, measured on
//! macOS 26.6 / Apple Silicon:
//!
//! - The window server removes a virtual display when the process that made
//!   it exits, even on a crash. So each screen lives in its own small host
//!   process (`displaylab` run as a [`screen_host`]), and unplugging is
//!   killing it.
//! - Changing a virtual display's mode makes it un-removable until its
//!   process exits. So the lab never changes a mode in place: a mode change
//!   is an unplug and a plug in the new mode.
//! - macOS remembers the last mode per identity and keeps a colour profile
//!   for every identity it has seen, forever. Keep profiles to a small fixed
//!   set of identities.
//! - Removal is asynchronous (0.7 s for four small screens, 1.9 s for six at
//!   1080p). The runner waits for screens to leave the active list.
//! - No window-server session, no display: the lab must run inside a
//!   logged-in GUI session, never from a launch daemon.
//!
//! Capture is ScreenCaptureKit, one stream per screen, started on first use.
//! It needs the Screen Recording permission for the process that runs the
//! lab (the terminal, or the CI runner).

use lab_core::{Backend, BackendError};

#[cfg(target_os = "macos")]
mod cg;
#[cfg(target_os = "macos")]
mod mac;
#[cfg(any(target_os = "macos", test))]
mod permission;
pub mod screen_host;

/// The display and capture halves of the macOS backend. They share one
/// table of screens, so capture can find what displays plugged in.
pub fn backend() -> Result<Backend, BackendError> {
    #[cfg(target_os = "macos")]
    {
        mac::backend()
    }
    #[cfg(not(target_os = "macos"))]
    {
        Err(BackendError::Unsupported(
            "create virtual screens (the macOS backend only runs on macOS)",
        ))
    }
}
