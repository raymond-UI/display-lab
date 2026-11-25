//! The fake world's screens, and the backend handles onto them.

use crate::app::AppState;
use lab_core::clock::ns_at;
use lab_core::{BackendError, Capture, Displays, Frame, Mode, Profile, ScreenId, ScreenInfo};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

struct FakeScreen {
    info: ScreenInfo,
    /// When the "OS" starts reporting this screen.
    reported_at: Instant,
    surface: Option<Frame>,
}

/// Everything in the fake world, behind one lock: screens, what is drawn on
/// them, and the app. One lock keeps the app's reaction to a topology change
/// atomic with the change, the way a real app reacts inside the OS
/// notification.
#[derive(Default)]
pub struct World {
    next_id: u64,
    screens: BTreeMap<ScreenId, FakeScreen>,
    pub(crate) app: AppState,
}

impl World {
    fn add(&mut self, profile: &Profile, mode: &Mode, host: bool) -> ScreenId {
        self.next_id += 1;
        let id = ScreenId(self.next_id);
        // New screens go to the right of everything already arranged, the
        // way both macOS and Windows place a newly attached display.
        let x = self
            .screens
            .values()
            .map(|s| s.info.origin.0 + s.info.mode.width as i32)
            .max()
            .unwrap_or(0);
        let delay = if host {
            Duration::ZERO
        } else {
            Duration::from_millis(profile.timings.report_delay_ms)
        };
        self.screens.insert(
            id,
            FakeScreen {
                info: ScreenInfo {
                    id,
                    identity: profile.identity.clone(),
                    mode: *mode,
                    origin: (x, 0),
                    host,
                },
                reported_at: Instant::now() + delay,
                surface: None,
            },
        );
        id
    }

    /// Screens the OS reports right now, in id order.
    pub(crate) fn reported(&self, now: Instant) -> Vec<ScreenInfo> {
        self.screens
            .values()
            .filter(|s| s.reported_at <= now)
            .map(|s| s.info.clone())
            .collect()
    }

    /// Advance the world to now: screens due to be reported appear, the app
    /// reacts to the topology and to pending input, and surfaces are redrawn.
    pub(crate) fn tick(&mut self) {
        let now = Instant::now();
        let reported = self.reported(now);
        if let Some((draws, current_at)) = self.app.update(now, &reported) {
            let stamp = ns_at(current_at);
            for screen in self.screens.values_mut() {
                let next = draws.get(&screen.info.id).cloned();
                let changed = match (&screen.surface, &next) {
                    (Some(a), Some(b)) => !a.same_pixels(b),
                    (None, None) => false,
                    _ => true,
                };
                if changed {
                    screen.surface = next.map(|mut f| {
                        f.present_ns = stamp;
                        f
                    });
                }
            }
        }
    }
}

fn lock(world: &Arc<Mutex<World>>) -> MutexGuard<'_, World> {
    // A panic elsewhere while holding the lock poisons it; the world's data
    // is still coherent (every mutation completes before unlocking), so keep
    // going rather than cascade the panic into the runner.
    world.lock().unwrap_or_else(|p| p.into_inner())
}

pub(crate) fn with_world<T>(world: &Arc<Mutex<World>>, f: impl FnOnce(&mut World) -> T) -> T {
    let mut w = lock(world);
    w.tick();
    let out = f(&mut w);
    w.tick();
    out
}

pub struct FakeDisplays {
    world: Arc<Mutex<World>>,
}

impl FakeDisplays {
    pub fn new(world: Arc<Mutex<World>>) -> FakeDisplays {
        FakeDisplays { world }
    }
}

impl Displays for FakeDisplays {
    fn host_screen(&mut self, profile: &Profile, mode: &Mode) -> Result<ScreenId, BackendError> {
        Ok(with_world(&self.world, |w| w.add(profile, mode, true)))
    }

    fn plug(&mut self, profile: &Profile, mode: &Mode) -> Result<ScreenId, BackendError> {
        Ok(with_world(&self.world, |w| w.add(profile, mode, false)))
    }

    fn unplug(&mut self, id: ScreenId) -> Result<(), BackendError> {
        with_world(&self.world, |w| match w.screens.get(&id) {
            None => Err(BackendError::NoSuchScreen(id)),
            Some(s) if s.info.host => Err(BackendError::Platform(
                "the host screen cannot be unplugged".into(),
            )),
            Some(_) => {
                w.screens.remove(&id);
                Ok(())
            }
        })
    }

    fn screens(&mut self) -> Result<Vec<ScreenInfo>, BackendError> {
        Ok(with_world(&self.world, |w| w.reported(Instant::now())))
    }

    fn reset(&mut self) -> Result<(), BackendError> {
        with_world(&self.world, |w| w.screens.retain(|_, s| s.info.host));
        Ok(())
    }
}

pub struct FakeCapture {
    world: Arc<Mutex<World>>,
}

impl FakeCapture {
    pub fn new(world: Arc<Mutex<World>>) -> FakeCapture {
        FakeCapture { world }
    }
}

impl Capture for FakeCapture {
    fn frame(&mut self, id: ScreenId) -> Result<Option<Frame>, BackendError> {
        with_world(&self.world, |w| match w.screens.get(&id) {
            Some(s) if s.reported_at <= Instant::now() => Ok(s.surface.clone()),
            _ => Err(BackendError::NoSuchScreen(id)),
        })
    }
}
