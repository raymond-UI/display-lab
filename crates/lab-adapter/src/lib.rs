//! The one per-app piece of Display Lab.
//!
//! An adapter launches the app, sends it input, and optionally reports what it
//! is doing: its state, which output it draws on which screen, and the frame
//! it expects an output to show. Everything else in the lab is app-neutral.
//!
//! Adapters can be written in any language. The lab talks to them over
//! JSON-RPC 2.0 on stdio, one message per line (`docs/PROTOCOL.md`). A Rust
//! adapter can implement [`Adapter`] and hand it to [`serve`].
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

mod client;
mod protocol;
mod server;

pub use client::RpcAdapter;
pub use protocol::{AppInfo, Binding, Capabilities, ScreenRef, WireFrame};
pub use server::serve;

use lab_core::{Frame, ScreenInfo};
use serde_json::Value;

#[derive(Debug, thiserror::Error)]
pub enum AdapterError {
    /// The app does not offer this; the scenario step that needs it cannot run.
    #[error("the adapter does not offer {0}")]
    Unsupported(&'static str),
    /// The adapter answered with an error.
    #[error("adapter error {code}: {message}")]
    Remote { code: i64, message: String },
    #[error("adapter did not answer {method} within {seconds} s")]
    Timeout { method: String, seconds: u64 },
    #[error("adapter process: {0}")]
    Transport(String),
    #[error("adapter sent something that is not the protocol: {0}")]
    Protocol(String),
}

/// What the lab asks of an app.
///
/// Required: [`launch`](Adapter::launch), [`input`](Adapter::input),
/// [`stop`](Adapter::stop). The rest are optional and advertised in
/// [`Capabilities`]; the runner refuses a step whose capability is missing
/// before it touches any screen, rather than failing halfway through a run.
pub trait Adapter: Send {
    fn info(&self) -> &AppInfo;

    /// Start the app. `screens` is what the OS reports at that moment, for
    /// adapters that configure the app's outputs at launch.
    fn launch(&mut self, screens: &[ScreenInfo]) -> Result<(), AdapterError>;

    /// One-time setup for an unattended run (for example, approving remote
    /// control in the app's own UI).
    fn setup(&mut self) -> Result<(), AdapterError> {
        Err(AdapterError::Unsupported("setup"))
    }

    fn input(&mut self, action: &str, args: &Value) -> Result<(), AdapterError>;

    fn state(&mut self) -> Result<Value, AdapterError> {
        Err(AdapterError::Unsupported("state"))
    }

    fn bindings(&mut self) -> Result<Vec<Binding>, AdapterError> {
        Err(AdapterError::Unsupported("bindings"))
    }

    /// What `output` should look like at `width` x `height`.
    fn expected_frame(
        &mut self,
        output: &str,
        width: u32,
        height: u32,
    ) -> Result<Frame, AdapterError> {
        let _ = (output, width, height);
        Err(AdapterError::Unsupported("expected_frame"))
    }

    fn stop(&mut self) -> Result<(), AdapterError>;
}
