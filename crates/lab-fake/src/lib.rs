//! A fake world: screens that exist only in memory, and a fake presentation
//! app that draws on them.
//!
//! This is how the lab tests itself. The runner, the scenario format and the
//! checks can all be exercised end to end on a machine with no display at
//! all, including CI. The fake app follows the same binding rules as
//! a real presentation app (an output follows its screen's identity, never falls back to
//! name or position; a disconnected audience output stands in on the only
//! screen), and it can be told to misbehave, so a scenario can be shown to
//! fail when it should.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

mod app;
mod render;
mod world;

pub use app::{FakeApp, FakeBugs};
pub use render::render_slide;
pub use world::{FakeCapture, FakeDisplays, World};

use std::sync::{Arc, Mutex};

/// A fresh world and its three handles: displays, capture, and the app.
pub fn fake(bugs: FakeBugs) -> (FakeDisplays, FakeCapture, FakeApp) {
    let world = Arc::new(Mutex::new(World::default()));
    (
        FakeDisplays::new(world.clone()),
        FakeCapture::new(world.clone()),
        FakeApp::new(world, bugs),
    )
}
