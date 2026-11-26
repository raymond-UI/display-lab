//! The Windows backend. **Screens are not wired up yet.**
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
//! Until the backend lands it refuses clearly. The rest of the lab
//! (scenarios, runner, adapters, compare, the fake backend) already runs on
//! Windows.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

pub mod edid;

use lab_core::{Backend, BackendError};

pub fn backend() -> Result<Backend, BackendError> {
    Err(BackendError::Unsupported(
        "create virtual screens on Windows yet: the backend for Display Lab's display driver is \
         not written; use --backend fake",
    ))
}
