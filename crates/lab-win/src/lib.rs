//! The Windows backend. **Not implemented yet**, on purpose.
//!
//! The plan (Display Lab doc, "Platform map" and "Stack research"):
//!
//! - **Screens:** the open-source, signed Virtual Display Driver (IddCx).
//!   It loads custom EDID profiles, supports fractional refresh rates, and
//!   is controlled over a named pipe.
//! - **Capture:** the `windows-capture` crate, Windows Graphics Capture
//!   first, DXGI Desktop Duplication as the fallback. Duplication becomes
//!   invalid on every mode change and has to be recreated.
//!
//! The open question that decides the shape of this crate, and the first
//! thing to test on a Windows machine: can the driver give **each** screen
//! its own EDID and plug **one** screen at a time? Its documentation shows a
//! single EDID profile and a display-count setting applied with a driver
//! reload. If that is all it can do, every screen shares one identity and
//! every plug or unplug blinks all of them, which breaks the identity and
//! hotplug scenarios. Then the driver gets forked to report monitor arrival
//! and departure per screen through IddCx.
//!
//! Writing the pipe protocol before that is answered would be guessing, so
//! the backend refuses clearly instead. The rest of the lab (scenarios,
//! runner, adapters, compare, the fake backend) already runs on Windows.

use lab_core::{Backend, BackendError};

pub fn backend() -> Result<Backend, BackendError> {
    Err(BackendError::Unsupported(
        "create virtual screens on Windows yet: the Virtual Display Driver backend is not written \
         (see crates/lab-win/src/lib.rs for what has to be checked first); use --backend fake",
    ))
}
