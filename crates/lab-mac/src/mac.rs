//! The macOS backend proper. See the crate docs for the facts it rests on.

use crate::{cg, permission};
use lab_core::{
    now_ns, Backend, BackendError, Capture, Displays, Frame, FrameTest, Identity, Mode, Profile,
    ScreenId, ScreenInfo, WatchId,
};
use screencapturekit::cv::CVPixelBufferLockFlags;
use screencapturekit::prelude::*;
use screencapturekit::SCFrameStatus;
use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

/// How long a host process may take to say `ready`, beyond any delay the
/// profile asked for.
const HOST_START: Duration = Duration::from_secs(10);

/// How long an app may take to show up in ScreenCaptureKit's list after the
/// lab asks to capture only its windows (a just-launched app lags).
const APP_APPEARS: Duration = Duration::from_secs(10);

struct Entry {
    lab: ScreenId,
    /// The CoreGraphics id, once the screen exists.
    cg: Option<cg::DisplayId>,
    identity: Identity,
    refresh_hz: f64,
    host: bool,
    /// The host process; `None` for the machine's own screen.
    child: Option<Child>,
    /// Set if the host process reported an error instead of `ready`.
    failed: Option<String>,
}

#[derive(Default)]
struct Registry {
    next: u64,
    entries: Vec<Entry>,
    /// Capture streams, by screen. Kept here rather than in the capture
    /// half so unplugging a screen stops its stream first: a stream left
    /// running on a removed display can make the window server refuse the
    /// next display with the same identity.
    streams: HashMap<ScreenId, Stream>,
    /// Capture only this app's windows, by bundle id; `None` captures the
    /// whole screen.
    app: Option<String>,
    /// Per screen, the frame tests the stream runs on every new frame.
    /// They outlive a stream, so rebuilding one keeps its watches.
    watches: HashMap<ScreenId, Watches>,
    next_watch: u64,
}

struct Watch {
    id: WatchId,
    test: FrameTest,
    /// The first frame that passed `test`.
    caught: Option<Frame>,
}

type Watches = Arc<Mutex<Vec<Watch>>>;

fn lock_watches(w: &Watches) -> MutexGuard<'_, Vec<Watch>> {
    w.lock().unwrap_or_else(|p| p.into_inner())
}

type Shared = Arc<Mutex<Registry>>;

fn lock(r: &Shared) -> MutexGuard<'_, Registry> {
    r.lock().unwrap_or_else(|p| p.into_inner())
}

pub fn backend() -> Result<Backend, BackendError> {
    // Each screen host is this same executable, run with the host argument.
    let host_exe = std::env::current_exe()
        .map_err(|e| BackendError::Platform(format!("locating displaylab: {e}")))?;
    if !cg::screen_capture_allowed() {
        return Err(BackendError::Platform(screen_recording_help()));
    }
    let registry: Shared = Arc::default();
    Ok((
        Box::new(MacDisplays {
            registry: registry.clone(),
            host_exe,
        }),
        Box::new(MacCapture { registry }),
    ))
}

/// What to do when Screen Recording is not allowed, naming the app that
/// holds the permission when it can be told.
fn screen_recording_help() -> String {
    let app = permission::app_from_ancestors(ancestor_paths()).or_else(|| {
        permission::app_from_term_program(std::env::var("TERM_PROGRAM").ok().as_deref())
    });
    permission::help(app.as_deref())
}

/// The executables of this process and each of its parents, up to launchd.
fn ancestor_paths() -> Vec<std::path::PathBuf> {
    let mut paths = Vec::new();
    let mut pid = std::process::id() as i32;
    // A bound, in case a parent chain is ever cyclic or absurdly deep.
    for _ in 0..64 {
        if let Some(path) = cg::process_path(pid) {
            paths.push(path);
        }
        match cg::parent_pid(pid) {
            Some(parent) if parent > 1 && parent != pid => pid = parent,
            _ => break,
        }
    }
    paths
}

struct MacDisplays {
    registry: Shared,
    host_exe: std::path::PathBuf,
}

impl Displays for MacDisplays {
    fn host_screen(&mut self, profile: &Profile, _mode: &Mode) -> Result<ScreenId, BackendError> {
        // The real built-in panel, whatever the profile says about it; the
        // profile only names it in reports.
        let id = cg::active_displays()
            .into_iter()
            .find(|d| cg::is_builtin(*d))
            .unwrap_or_else(cg::main_display);
        let mut r = lock(&self.registry);
        r.next += 1;
        let lab = ScreenId(r.next);
        let facts = cg::facts(id)
            .ok_or_else(|| BackendError::Platform("the host screen has no mode".into()))?;
        r.entries.push(Entry {
            lab,
            cg: Some(id),
            identity: Identity {
                vendor: facts.vendor,
                product: facts.product,
                serial: facts.serial,
                name: profile.identity.name.clone(),
            },
            refresh_hz: facts.refresh_hz,
            host: true,
            child: None,
            failed: None,
        });
        Ok(lab)
    }

    fn plug(&mut self, profile: &Profile, mode: &Mode) -> Result<ScreenId, BackendError> {
        let id = &profile.identity;
        let mut command = Command::new(&self.host_exe);
        command
            .arg(crate::screen_host::ARG)
            .args(["--name", &id.name])
            .args(["--vendor", &id.vendor.to_string()])
            .args(["--product", &id.product.to_string()])
            .args(["--serial", &id.serial.to_string()])
            .args(["--width", &mode.width.to_string()])
            .args(["--height", &mode.height.to_string()])
            .args(["--refresh", &mode.refresh_hz.to_string()])
            .args(["--delay-ms", &profile.timings.report_delay_ms.to_string()])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        if mode.hidpi {
            command.arg("--hidpi");
        }
        let mut child = command
            .spawn()
            .map_err(|e| BackendError::Platform(format!("starting the screen host: {e}")))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| BackendError::Platform("screen host has no stdout".into()))?;

        let lab = {
            let mut r = lock(&self.registry);
            r.next += 1;
            let lab = ScreenId(r.next);
            r.entries.push(Entry {
                lab,
                cg: None,
                identity: id.clone(),
                refresh_hz: mode.refresh_hz,
                host: false,
                child: Some(child),
                failed: None,
            });
            lab
        };

        // The host answers once the screen exists, which with a profile
        // delay is later than now. Read the answer in the background so
        // plug returns at once and the runner can look at the world before
        // the screen arrives.
        let registry = self.registry.clone();
        std::thread::spawn(move || {
            let mut line = String::new();
            let answer = BufReader::new(stdout).read_line(&mut line);
            let mut r = lock(&registry);
            let Some(entry) = r.entries.iter_mut().find(|e| e.lab == lab) else {
                return;
            };
            match (answer, line.trim().split_once(' ')) {
                (Ok(_), Some(("ready", id))) => match id.parse() {
                    Ok(cg) => entry.cg = Some(cg),
                    Err(_) => entry.failed = Some(format!("screen host said {line:?}")),
                },
                (Ok(_), Some(("error", why))) => entry.failed = Some(why.to_string()),
                (Ok(_), _) => entry.failed = Some(format!("screen host said {line:?}")),
                (Err(e), _) => entry.failed = Some(format!("reading the screen host: {e}")),
            }
        });

        // Fail fast if the host could not make the screen at all, rather
        // than letting the runner time out waiting for it.
        let deadline =
            Instant::now() + HOST_START + Duration::from_millis(profile.timings.report_delay_ms);
        if profile.timings.report_delay_ms == 0 {
            while Instant::now() < deadline {
                {
                    let r = lock(&self.registry);
                    let entry = r.entries.iter().find(|e| e.lab == lab);
                    match entry {
                        Some(Entry { cg: Some(_), .. }) => break,
                        Some(Entry {
                            failed: Some(why), ..
                        }) => return Err(BackendError::Platform(why.clone())),
                        _ => {}
                    }
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        }
        Ok(lab)
    }

    fn unplug(&mut self, id: ScreenId) -> Result<(), BackendError> {
        let mut r = lock(&self.registry);
        let Some(i) = r.entries.iter().position(|e| e.lab == id) else {
            return Err(BackendError::NoSuchScreen(id));
        };
        if r.entries[i].host {
            return Err(BackendError::Platform(
                "the host screen cannot be unplugged".into(),
            ));
        }
        let mut entry = r.entries.remove(i);
        r.watches.remove(&id);
        if let Some(old) = r.streams.remove(&id) {
            let _ = old.stream.stop_capture();
        }
        if let Some(mut child) = entry.child.take() {
            // The window server removes the screen when the process exits.
            let _ = child.kill();
            let _ = child.wait();
        }
        Ok(())
    }

    fn screens(&mut self) -> Result<Vec<ScreenInfo>, BackendError> {
        let active = cg::active_displays();
        let r = lock(&self.registry);
        if std::env::var_os("DISPLAYLAB_DEBUG").is_some() {
            for e in &r.entries {
                eprintln!(
                    "[lab-mac] screen {:?}: cg={:?} host={} active={} has_mode={} failed={:?}",
                    e.lab,
                    e.cg,
                    e.host,
                    e.cg.is_some_and(|c| active.contains(&c)),
                    e.cg.and_then(cg::facts).is_some(),
                    e.failed
                );
            }
            eprintln!(
                "[lab-mac] active displays: {active:?}; online: {}",
                cg::describe_displays()
            );
        }
        Ok(r.entries
            .iter()
            .filter_map(|e| {
                let id = e.cg.filter(|id| active.contains(id))?;
                let facts = cg::facts(id)?;
                Some(ScreenInfo {
                    id: e.lab,
                    identity: e.identity.clone(),
                    mode: Mode {
                        width: facts.width,
                        height: facts.height,
                        refresh_hz: if facts.refresh_hz > 0.0 {
                            facts.refresh_hz
                        } else {
                            e.refresh_hz
                        },
                        hidpi: facts.hidpi,
                    },
                    origin: facts.origin,
                    host: e.host,
                })
            })
            .collect())
    }

    fn reset(&mut self) -> Result<(), BackendError> {
        let mut r = lock(&self.registry);
        for entry in r.entries.iter_mut() {
            if let Some(mut child) = entry.child.take() {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
        r.entries.clear();
        r.watches.clear();
        r.app = None;
        Ok(())
    }
}

impl Drop for MacDisplays {
    fn drop(&mut self) {
        let _ = self.reset();
    }
}

/// The newest complete frame from one ScreenCaptureKit stream, and the
/// first frame each watch on the screen was waiting for.
struct Sink {
    latest: Arc<Mutex<Option<Frame>>>,
    watches: Watches,
}

impl SCStreamOutputTrait for Sink {
    fn did_output_sample_buffer(&self, sample: CMSampleBuffer, kind: SCStreamOutputType) {
        if !matches!(kind, SCStreamOutputType::Screen) {
            return;
        }
        // Only a complete frame carries new pixels. Idle means "nothing
        // changed", which is exactly when the last frame is still right.
        if sample
            .frame_status()
            .is_some_and(|s| s != SCFrameStatus::Complete)
        {
            return;
        }
        let Some(buffer) = sample.pixel_buffer() else {
            return;
        };
        let Ok(guard) = buffer.lock(CVPixelBufferLockFlags::READ_ONLY) else {
            return;
        };
        let (w, h, stride) = (guard.width(), guard.height(), guard.bytes_per_row());
        // SAFETY: the buffer is locked for reading for the guard's lifetime.
        let Some(bgra) = (unsafe { guard.as_slice() }) else {
            return;
        };
        if bgra.len() < stride * h || stride < w * 4 {
            return;
        }
        let mut rgba = Vec::with_capacity(w * h * 4);
        for row in bgra.chunks_exact(stride).take(h) {
            for p in row[..w * 4].chunks_exact(4) {
                rgba.extend_from_slice(&[p[2], p[1], p[0], 255]);
            }
        }
        // Presentation time is host time; move it onto the lab clock.
        let lab_now = now_ns() as i128;
        let present_ns = sample
            .presentation_timestamp()
            .as_seconds()
            .map(|s| {
                let host_ns = (s * 1e9) as i128;
                let offset = cg::host_now_ns() as i128 - lab_now;
                (host_ns - offset).clamp(1, i128::from(u64::MAX)) as u64
            })
            .unwrap_or(lab_now as u64);
        let frame = Frame {
            width: w as u32,
            height: h as u32,
            rgba,
            present_ns,
        };
        // Watches first: a reader that sees the new frame as the latest
        // must also find it caught.
        for w in lock_watches(&self.watches).iter_mut() {
            if w.caught.is_none() && (w.test)(&frame) {
                w.caught = Some(frame.clone());
            }
        }
        *self.latest.lock().unwrap_or_else(|p| p.into_inner()) = Some(frame);
    }
}

struct Stream {
    cg: cg::DisplayId,
    stream: SCStream,
    latest: Arc<Mutex<Option<Frame>>>,
}

struct MacCapture {
    registry: Shared,
}

impl MacCapture {
    /// A running stream on `cg_id`, or `None` if ScreenCaptureKit does not
    /// list the screen yet (it lags the window server by a moment after a
    /// plug).
    fn start(
        &self,
        cg_id: cg::DisplayId,
        app: Option<&str>,
        watches: Watches,
    ) -> Result<Option<Stream>, BackendError> {
        let platform = |what: &str, e: SCError| BackendError::Platform(format!("{what}: {e}"));
        let facts = cg::facts(cg_id).ok_or(BackendError::Platform("screen has no mode".into()))?;
        // Shareable content is a snapshot; take a fresh one so a screen that
        // was just plugged in is in it.
        let content = SCShareableContent::get()
            .map_err(|e| platform("listing screens (Screen Recording permission?)", e))?;
        let Some(display) = content
            .displays()
            .into_iter()
            .find(|d| d.display_id() == cg_id)
        else {
            return Ok(None);
        };
        let filter = SCContentFilter::create().with_display(&display);
        let filter = match app {
            None => filter.with_excluding_windows(&[]),
            Some(bundle_id) => {
                let Some(app) = content
                    .applications()
                    .into_iter()
                    .find(|a| a.bundle_identifier() == bundle_id)
                else {
                    return Err(BackendError::Platform(format!(
                        "{bundle_id} is not running, so there are no windows of it to capture"
                    )));
                };
                filter.with_including_applications(&[&app], &[])
            }
        }
        .build()
        .map_err(|e| platform("building the capture filter", e))?;
        let config = SCStreamConfiguration::new()
            .with_width(facts.width)
            .with_height(facts.height)
            .with_pixel_format(PixelFormat::BGRA)
            .with_shows_cursor(false);
        let latest = Arc::new(Mutex::new(None));
        let mut stream = SCStream::new(&filter, &config)
            .map_err(|e| platform("creating the capture stream", e))?;
        stream
            .add_output_handler(
                Sink {
                    latest: latest.clone(),
                    watches,
                },
                SCStreamOutputType::Screen,
            )
            .map_err(|e| platform("attaching the frame handler", e))?;
        stream
            .start_capture()
            .map_err(|e| platform("starting capture", e))?;
        Ok(Some(Stream {
            cg: cg_id,
            stream,
            latest,
        }))
    }
}

/// Whether ScreenCaptureKit lists a running app with this bundle id.
fn app_listed(bundle_id: &str) -> Result<bool, BackendError> {
    let content = SCShareableContent::get().map_err(|e| {
        BackendError::Platform(format!("listing apps (Screen Recording permission?): {e}"))
    })?;
    Ok(content
        .applications()
        .iter()
        .any(|a| a.bundle_identifier() == bundle_id))
}

impl Capture for MacCapture {
    fn only_app(&mut self, bundle_id: &str) -> Result<(), BackendError> {
        // Wait for the app here, where a missing app is one clear error,
        // rather than in every frame afterwards.
        let deadline = Instant::now() + APP_APPEARS;
        while !app_listed(bundle_id)? {
            if Instant::now() >= deadline {
                return Err(BackendError::Platform(format!(
                    "{bundle_id} is not running (not listed by ScreenCaptureKit after {} s)",
                    APP_APPEARS.as_secs()
                )));
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        let mut r = lock(&self.registry);
        r.app = Some(bundle_id.to_string());
        // Rebuild every stream on next use: the filter names the app's
        // process, which may be a new one since the last call.
        for (_, s) in r.streams.drain() {
            let _ = s.stream.stop_capture();
        }
        Ok(())
    }

    fn watch(&mut self, id: ScreenId, test: FrameTest) -> Result<Option<WatchId>, BackendError> {
        let mut r = lock(&self.registry);
        if !r.entries.iter().any(|e| e.lab == id) {
            return Err(BackendError::NoSuchScreen(id));
        }
        r.next_watch += 1;
        let watch = WatchId(r.next_watch);
        lock_watches(r.watches.entry(id).or_default()).push(Watch {
            id: watch,
            test,
            caught: None,
        });
        Ok(Some(watch))
    }

    fn caught(&mut self, watch: WatchId) -> Result<Option<Frame>, BackendError> {
        let r = lock(&self.registry);
        for list in r.watches.values() {
            if let Some(w) = lock_watches(list).iter().find(|w| w.id == watch) {
                return Ok(w.caught.clone());
            }
        }
        Ok(None)
    }

    fn unwatch(&mut self, watch: WatchId) {
        let r = lock(&self.registry);
        for list in r.watches.values() {
            lock_watches(list).retain(|w| w.id != watch);
        }
    }

    fn frame(&mut self, id: ScreenId) -> Result<Option<Frame>, BackendError> {
        let (cg_id, has_stream, app, watches) = {
            let mut r = lock(&self.registry);
            let cg_id = r.entries.iter().find(|e| e.lab == id).and_then(|e| e.cg);
            let has_stream = r.streams.get(&id).is_some_and(|s| Some(s.cg) == cg_id);
            let app = r.app.clone();
            // Only a screen the lab knows gets a watch list.
            let watches = match cg_id {
                Some(_) => r.watches.entry(id).or_default().clone(),
                None => Watches::default(),
            };
            (cg_id, has_stream, app, watches)
        };
        let active = cg::active_displays();
        let Some(cg_id) = cg_id.filter(|c| active.contains(c)) else {
            // Gone, or not reported yet. Drop any stream on it.
            if let Some(old) = lock(&self.registry).streams.remove(&id) {
                let _ = old.stream.stop_capture();
            }
            return Err(BackendError::NoSuchScreen(id));
        };
        if !has_stream {
            // Started without the lock held: listing shareable content can
            // take a moment, and the screen hosts' readers need the lock.
            let Some(stream) = self.start(cg_id, app.as_deref(), watches)? else {
                return Ok(None);
            };
            let mut r = lock(&self.registry);
            if !r.entries.iter().any(|e| e.lab == id) {
                // Unplugged while the stream was starting.
                let _ = stream.stream.stop_capture();
                return Err(BackendError::NoSuchScreen(id));
            }
            if let Some(old) = r.streams.insert(id, stream) {
                let _ = old.stream.stop_capture();
            }
        }
        let r = lock(&self.registry);
        let Some(stream) = r.streams.get(&id) else {
            return Ok(None);
        };
        let frame = stream
            .latest
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone();
        Ok(frame)
    }
}

impl Drop for MacCapture {
    fn drop(&mut self) {
        for (_, s) in lock(&self.registry).streams.drain() {
            let _ = s.stream.stop_capture();
        }
    }
}
