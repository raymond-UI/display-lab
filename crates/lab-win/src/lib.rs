//! The Windows backend.
//!
//! - **Screens:** Display Lab's own indirect display driver
//!   (`driver/windows`). It starts with no monitors; the lab plugs each one
//!   over a named pipe with an EDID built from the profile ([`edid`]), and
//!   unplugs one at a time, so the other screens never blink. A ready-made
//!   virtual display driver was the first plan, but it gives every screen
//!   the same EDID and reloads all of them to change the count, which breaks
//!   the identity and hotplug scenarios.
//! - **Capture:** DXGI Desktop Duplication per output, with frame times
//!   from the same performance counter `Instant` uses.
//!
//! The driver has to be installed first (`driver/windows/README.md`);
//! without it the backend says so and stops.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

pub mod display;
pub mod edid;
#[cfg(windows)]
mod win;

use lab_core::{Backend, BackendError};

/// The display and capture halves of the Windows backend.
pub fn backend() -> Result<Backend, BackendError> {
    #[cfg(windows)]
    {
        win::backend()
    }
    #[cfg(not(windows))]
    {
        Err(BackendError::Unsupported(
            "create virtual screens (the Windows backend only runs on Windows)",
        ))
    }
}
