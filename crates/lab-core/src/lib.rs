//! Display Lab's shared model.
//!
//! Nothing in this crate knows about any operating system or any app under
//! test. Platform crates implement [`Displays`] and [`Capture`]; the adapter
//! crate talks to apps; the runner joins them through the types here.

pub mod backend;
pub mod clock;
pub mod frame;
pub mod profile;
pub mod scenario;

pub use backend::{Backend, BackendError, Capture, Displays, ScreenId, ScreenInfo};
pub use clock::now_ns;
pub use frame::Frame;
pub use profile::{Identity, Mode, ModeSpec, Profile, ProfileError, ProfileSet, Timings};
pub use scenario::{CompareSpec, Scenario, ScenarioError, ScreenDecl, Step, DEFAULT_WITHIN_MS};
