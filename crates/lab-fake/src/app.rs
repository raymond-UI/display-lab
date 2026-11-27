//! The fake presentation app, and its adapter.
//!
//! It has two outputs. `audience` is configured, at launch, to the first
//! screen that is not the host; `stage` to the second, if there is one. An
//! output follows its screen by identity (vendor, product, serial) and never
//! by name or position, so a different projector on the same port is not
//! adopted. While the audience output's screen is away and the host is the
//! only screen left, the slides stand in on the host instead of the console.

use crate::render::{render_console, render_slide};
use crate::world::{with_world, World};
use lab_adapter::{Adapter, AdapterError, AppInfo, Binding, Capabilities, ScreenRef};
use lab_core::{Frame, Identity, ScreenId, ScreenInfo};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Ways the fake app can be told to misbehave, so the lab can prove that a
/// scenario fails when the app is wrong.
#[derive(Debug, Clone, Copy, Default)]
pub struct FakeBugs {
    /// The app only reads the screen list at launch and never notices a
    /// screen coming or going.
    pub ignore_hotplug: bool,
    /// How long between an input and the picture changing.
    pub input_latency_ms: u64,
    /// Bind outputs by screen name, the wrong rule: a different projector
    /// with the same name is adopted.
    pub bind_by_name: bool,
}

struct Output {
    name: &'static str,
    configured: Option<Identity>,
}

#[derive(Default)]
pub(crate) struct AppState {
    running: bool,
    bugs: FakeBugs,
    outputs: Vec<Output>,
    /// The slide the app is on.
    slide: u32,
    blank: bool,
    /// The slide and blank state currently drawn, and an input not yet drawn.
    shown: (u32, bool),
    pending: Option<((u32, bool), Instant)>,
    /// The screens the app knows about.
    known: Vec<ScreenInfo>,
    dirty: bool,
}

impl AppState {
    fn identity_matches(&self, configured: &Identity, screen: &ScreenInfo) -> bool {
        if self.bugs.bind_by_name {
            configured.name == screen.identity.name
        } else {
            configured.vendor == screen.identity.vendor
                && configured.product == screen.identity.product
                && configured.serial == screen.identity.serial
        }
    }

    fn bound_screen(&self, output: &Output) -> Option<&ScreenInfo> {
        let configured = output.configured.as_ref()?;
        self.known
            .iter()
            .find(|s| !s.host && self.identity_matches(configured, s))
    }

    /// Bring the app up to date. Returns what every screen should show and
    /// the moment that picture became current, if anything changed.
    ///
    /// The moment matters for latency. The fake world only advances when
    /// someone looks at it, so "now" can be well after the app would have
    /// drawn; an input's picture is stamped with when it was due instead.
    pub(crate) fn update(
        &mut self,
        now: Instant,
        reported: &[ScreenInfo],
    ) -> Option<(BTreeMap<ScreenId, Frame>, Instant)> {
        if !self.running {
            return None;
        }
        let mut current_at = now;
        if !self.bugs.ignore_hotplug && self.known != reported {
            self.known = reported.to_vec();
            self.dirty = true;
        }
        if let Some((target, due)) = self.pending {
            if due <= now {
                self.shown = target;
                self.pending = None;
                self.dirty = true;
                current_at = due;
            }
        }
        if !self.dirty {
            return None;
        }
        self.dirty = false;

        let (slide, blank) = self.shown;
        let content = |w: u32, h: u32| {
            if blank {
                Frame::solid(w, h, [0, 0, 0, 255])
            } else {
                render_slide(slide, w, h)
            }
        };

        let mut draws = BTreeMap::new();
        let mut audience_bound = false;
        for output in &self.outputs {
            if let Some(screen) = self.bound_screen(output) {
                audience_bound |= output.name == "audience";
                draws.insert(screen.id, content(screen.mode.width, screen.mode.height));
            }
        }
        let only_host = reported.len() == 1 && reported.iter().all(|s| s.host);
        for screen in reported {
            if draws.contains_key(&screen.id) {
                continue;
            }
            let frame = if screen.host {
                if !audience_bound && only_host && self.has_audience_config() {
                    content(screen.mode.width, screen.mode.height)
                } else {
                    render_console(screen.mode.width, screen.mode.height)
                }
            } else {
                // A screen with no output on it: the app leaves it black.
                Frame::solid(screen.mode.width, screen.mode.height, [0, 0, 0, 255])
            };
            draws.insert(screen.id, frame);
        }
        Some((draws, current_at))
    }

    fn has_audience_config(&self) -> bool {
        self.outputs
            .iter()
            .any(|o| o.name == "audience" && o.configured.is_some())
    }

    fn go(&mut self, slide: u32, blank: bool) {
        self.slide = slide;
        self.blank = blank;
        let due = Instant::now() + Duration::from_millis(self.bugs.input_latency_ms);
        self.pending = Some(((slide, blank), due));
    }
}

pub struct FakeApp {
    world: Arc<Mutex<World>>,
    info: AppInfo,
    bugs: FakeBugs,
}

impl FakeApp {
    pub fn new(world: Arc<Mutex<World>>, bugs: FakeBugs) -> FakeApp {
        FakeApp {
            world,
            info: AppInfo {
                name: "fake-presenter".into(),
                version: env!("CARGO_PKG_VERSION").into(),
                bundle_id: None,
                protocol: "1.0".into(),
                capabilities: Capabilities {
                    setup: true,
                    state: true,
                    bindings: true,
                    expected_frame: true,
                    compare: None,
                },
            },
            bugs,
        }
    }

    fn running<T>(
        &self,
        f: impl FnOnce(&mut AppState) -> Result<T, AdapterError>,
    ) -> Result<T, AdapterError> {
        with_world(&self.world, |w| {
            if !w.app.running {
                return Err(AdapterError::Remote {
                    code: 1,
                    message: "the app is not running".into(),
                });
            }
            f(&mut w.app)
        })
    }
}

impl Adapter for FakeApp {
    fn info(&self) -> &AppInfo {
        &self.info
    }

    fn launch(&mut self, _screens: &[ScreenInfo]) -> Result<(), AdapterError> {
        let bugs = self.bugs;
        with_world(&self.world, |w| {
            let reported = w.reported(Instant::now());
            let mut externals = reported.iter().filter(|s| !s.host);
            let audience = externals.next().map(|s| s.identity.clone());
            let stage = externals.next().map(|s| s.identity.clone());
            w.app = AppState {
                running: true,
                bugs,
                outputs: vec![
                    Output {
                        name: "audience",
                        configured: audience,
                    },
                    Output {
                        name: "stage",
                        configured: stage,
                    },
                ],
                slide: 1,
                blank: false,
                shown: (1, false),
                pending: None,
                known: reported,
                dirty: true,
            };
        });
        Ok(())
    }

    fn setup(&mut self) -> Result<(), AdapterError> {
        // Nothing to approve in the fake app; offered so scenarios that run
        // setup can run here too.
        Ok(())
    }

    fn input(&mut self, action: &str, args: &Value) -> Result<(), AdapterError> {
        self.running(|app| {
            let bad = |message: String| AdapterError::Remote { code: 2, message };
            match action {
                "next" => {
                    let n = app.slide + 1;
                    app.go(n, false);
                }
                "previous" => {
                    let n = app.slide.saturating_sub(1).max(1);
                    app.go(n, false);
                }
                "go_to" => {
                    let n = args
                        .get("slide")
                        .and_then(Value::as_u64)
                        .ok_or_else(|| bad("go_to needs {\"slide\": N}".into()))?;
                    app.go(n as u32, false);
                }
                "blank" => {
                    let n = app.slide;
                    app.go(n, true);
                }
                "unblank" => {
                    let n = app.slide;
                    app.go(n, false);
                }
                other => return Err(bad(format!("unknown action {other:?}"))),
            }
            Ok(())
        })
    }

    fn state(&mut self) -> Result<Value, AdapterError> {
        self.running(|app| Ok(json!({ "slide": app.slide, "blank": app.blank })))
    }

    fn bindings(&mut self) -> Result<Vec<Binding>, AdapterError> {
        self.running(|app| {
            Ok(app
                .outputs
                .iter()
                .filter(|o| o.configured.is_some())
                .map(|o| Binding {
                    output: o.name.to_string(),
                    screen: app.bound_screen(o).map(ScreenRef::of),
                })
                .collect())
        })
    }

    fn expected_frame(
        &mut self,
        output: &str,
        width: u32,
        height: u32,
    ) -> Result<Frame, AdapterError> {
        self.running(|app| {
            if !app.outputs.iter().any(|o| o.name == output) {
                return Err(AdapterError::Remote {
                    code: 3,
                    message: format!("no output {output:?}"),
                });
            }
            Ok(if app.blank {
                Frame::solid(width, height, [0, 0, 0, 255])
            } else {
                render_slide(app.slide, width, height)
            })
        })
    }

    fn stop(&mut self) -> Result<(), AdapterError> {
        with_world(&self.world, |w| w.app.running = false);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake;
    use lab_core::{Capture, Displays, Identity, Mode, Profile, Timings};

    fn profile(name: &str, serial: u32, w: u32, h: u32) -> Profile {
        Profile {
            name: name.into(),
            description: String::new(),
            identity: Identity {
                vendor: 1,
                product: 2,
                serial,
                name: name.into(),
            },
            modes: vec![Mode {
                width: w,
                height: h,
                refresh_hz: 60.0,
                hidpi: false,
            }],
            timings: Timings::default(),
        }
    }

    #[test]
    fn slides_follow_the_projector_by_identity() {
        let (mut displays, mut capture, mut app) = fake(FakeBugs::default());
        let laptop = profile("laptop", 0, 64, 40);
        let projector = profile("projector", 7, 48, 36);
        let host = displays.host_screen(&laptop, &laptop.modes[0]).unwrap();
        let pj = displays.plug(&projector, &projector.modes[0]).unwrap();

        app.launch(&[]).unwrap();
        let shown = capture.frame(pj).unwrap().unwrap();
        assert!(shown.same_pixels(&render_slide(1, 48, 36)));
        assert!(capture
            .frame(host)
            .unwrap()
            .unwrap()
            .same_pixels(&render_console(64, 40)));

        // Pulled: the slides stand in on the laptop.
        displays.unplug(pj).unwrap();
        assert!(capture
            .frame(host)
            .unwrap()
            .unwrap()
            .same_pixels(&render_slide(1, 64, 40)));
        assert_eq!(app.bindings().unwrap()[0].screen, None);

        // Back: a new screen id, same identity, adopted again.
        let pj2 = displays.plug(&projector, &projector.modes[0]).unwrap();
        assert!(capture
            .frame(pj2)
            .unwrap()
            .unwrap()
            .same_pixels(&render_slide(1, 48, 36)));
        assert!(app.bindings().unwrap()[0].screen.is_some());
    }

    #[test]
    fn a_different_projector_is_not_adopted() {
        let (mut displays, mut capture, mut app) = fake(FakeBugs::default());
        let laptop = profile("laptop", 0, 64, 40);
        let ours = profile("projector", 7, 48, 36);
        let mut other = profile("projector", 8, 48, 36);
        other.name = "other".into();
        displays.host_screen(&laptop, &laptop.modes[0]).unwrap();
        let pj = displays.plug(&ours, &ours.modes[0]).unwrap();
        app.launch(&[]).unwrap();
        displays.unplug(pj).unwrap();
        let swapped = displays.plug(&other, &other.modes[0]).unwrap();
        let f = capture.frame(swapped).unwrap().unwrap();
        assert!(f.same_pixels(&Frame::solid(48, 36, [0, 0, 0, 255])));
    }

    #[test]
    fn the_name_bug_adopts_the_wrong_projector() {
        let bugs = FakeBugs {
            bind_by_name: true,
            ..Default::default()
        };
        let (mut displays, mut capture, mut app) = fake(bugs);
        let laptop = profile("laptop", 0, 64, 40);
        let ours = profile("projector", 7, 48, 36);
        let other = profile("projector", 8, 48, 36);
        displays.host_screen(&laptop, &laptop.modes[0]).unwrap();
        let pj = displays.plug(&ours, &ours.modes[0]).unwrap();
        app.launch(&[]).unwrap();
        displays.unplug(pj).unwrap();
        let swapped = displays.plug(&other, &other.modes[0]).unwrap();
        let f = capture.frame(swapped).unwrap().unwrap();
        assert!(f.same_pixels(&render_slide(1, 48, 36)));
    }

    #[test]
    fn input_latency_delays_the_picture() {
        let bugs = FakeBugs {
            input_latency_ms: 80,
            ..Default::default()
        };
        let (mut displays, mut capture, mut app) = fake(bugs);
        let projector = profile("projector", 7, 48, 36);
        let pj = displays.plug(&projector, &projector.modes[0]).unwrap();
        app.launch(&[]).unwrap();
        app.input("next", &Value::Null).unwrap();
        assert!(capture
            .frame(pj)
            .unwrap()
            .unwrap()
            .same_pixels(&render_slide(1, 48, 36)));
        std::thread::sleep(Duration::from_millis(100));
        assert!(capture
            .frame(pj)
            .unwrap()
            .unwrap()
            .same_pixels(&render_slide(2, 48, 36)));
    }
}
