//! What a platform has to provide: screens it can plug in and pull out, and
//! a way to see what is on them.

use crate::{Frame, Identity, Mode, Profile};
use serde::{Deserialize, Serialize};

/// A screen as one backend knows it. Only meaningful to the backend that
/// issued it, and only while that screen is plugged in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ScreenId(pub u64);

/// A screen the OS currently reports.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScreenInfo {
    pub id: ScreenId,
    pub identity: Identity,
    pub mode: Mode,
    /// Top-left corner in the desktop arrangement.
    pub origin: (i32, i32),
    /// The machine's own screen (a laptop panel), not one the lab plugged in.
    /// The lab never unplugs it.
    pub host: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum BackendError {
    #[error("this backend cannot {0} on this machine")]
    Unsupported(&'static str),
    #[error("no screen {0:?}")]
    NoSuchScreen(ScreenId),
    #[error("{0}")]
    Platform(String),
}

/// Plugging screens in and out.
///
/// `plug` returns as soon as the screen exists from the backend's side. The
/// OS may report it later (see [`crate::Timings::report_delay_ms`]); until it
/// does, `screens` does not list it. The runner waits on `screens`, never on
/// a fixed sleep: on macOS even removal is asynchronous.
pub trait Displays: Send {
    /// The machine's own screens, which exist before any scenario starts.
    /// The fake backend creates one from `profile`; a real backend ignores
    /// `profile` and reports what is attached.
    fn host_screen(&mut self, profile: &Profile, mode: &Mode) -> Result<ScreenId, BackendError>;

    fn plug(&mut self, profile: &Profile, mode: &Mode) -> Result<ScreenId, BackendError>;

    fn unplug(&mut self, id: ScreenId) -> Result<(), BackendError>;

    /// Screens the OS currently reports, host screens included.
    fn screens(&mut self) -> Result<Vec<ScreenInfo>, BackendError>;

    /// Unplug everything this backend plugged in. Called at the end of every
    /// run, pass or fail, so a crashed scenario does not leave screens behind.
    fn reset(&mut self) -> Result<(), BackendError>;
}

/// A test a watch puts each new frame to. See [`Capture::watch`].
pub type FrameTest = Box<dyn Fn(&Frame) -> bool + Send>;

/// One watch on one screen, issued by [`Capture::watch`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct WatchId(pub u64);

/// Seeing what is on a screen.
pub trait Capture: Send {
    /// The newest frame presented on `id`, or `None` if nothing has been
    /// presented on it yet.
    fn frame(&mut self, id: ScreenId) -> Result<Option<Frame>, BackendError>;

    /// Capture only what the app with this bundle id draws, leaving out the
    /// desktop, other apps and the OS's own marks, so a screen the app does
    /// not draw on reads as black. Called again after the app is
    /// (re)started. A backend that can only capture whole screens says
    /// [`BackendError::Unsupported`].
    fn only_app(&mut self, bundle_id: &str) -> Result<(), BackendError> {
        let _ = bundle_id;
        Err(BackendError::Unsupported("capture one app's windows"))
    }

    /// Keep the first frame presented on `id` from now on that passes
    /// `test`, so a change that is soon replaced (the start of a fade) is
    /// still seen, with its own presentation time. A backend that only
    /// keeps the newest frame returns `None`, and the lab looks at that
    /// instead.
    fn watch(&mut self, id: ScreenId, test: FrameTest) -> Result<Option<WatchId>, BackendError> {
        let _ = (id, test);
        Ok(None)
    }

    /// The frame a watch caught, once it has caught one.
    fn caught(&mut self, watch: WatchId) -> Result<Option<Frame>, BackendError> {
        let _ = watch;
        Ok(None)
    }

    /// Stop a watch. Unknown ids are ignored.
    fn unwatch(&mut self, watch: WatchId) {
        let _ = watch;
    }
}

/// A platform backend: its display half and its capture half. They usually
/// share state (capture has to find the screens displays plugged in), so a
/// platform crate hands out both together.
pub type Backend = (Box<dyn Displays>, Box<dyn Capture>);
