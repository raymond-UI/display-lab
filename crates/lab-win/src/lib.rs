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
//! The driver has to be installed first (`displaylab driver install`, in
//! an administrator terminal); without it the backend says so and stops.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

pub mod display;
pub mod edid;
#[cfg(windows)]
mod install;
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

/// Installing and removing Display Lab's display driver (`displaylab
/// driver ...`). Windows only; each needs an administrator terminal except
/// `status`.
pub mod driver {
    use std::path::Path;

    #[cfg(windows)]
    pub use crate::install::InstallError as Error;

    #[cfg(not(windows))]
    #[derive(Debug, thiserror::Error)]
    pub enum Error {
        #[error("the display driver is for Windows; this machine is not running it")]
        NotWindows,
    }

    /// Install the driver package in `dir`; returns what was done.
    pub fn install(dir: &Path) -> Result<String, Error> {
        #[cfg(windows)]
        {
            crate::install::install(dir)
        }
        #[cfg(not(windows))]
        {
            let _ = dir;
            Err(Error::NotWindows)
        }
    }

    pub fn uninstall() -> Result<String, Error> {
        #[cfg(windows)]
        {
            crate::install::uninstall()
        }
        #[cfg(not(windows))]
        {
            Err(Error::NotWindows)
        }
    }

    pub fn status() -> Result<String, Error> {
        #[cfg(windows)]
        {
            crate::install::status()
        }
        #[cfg(not(windows))]
        {
            Err(Error::NotWindows)
        }
    }
}
