//! The macOS backend proper. See the crate docs for the facts it rests on.

use crate::cg;
use lab_core::{
    now_ns, Backend, BackendError, Capture, Displays, Frame, Identity, Mode, Profile, ScreenId,
    ScreenInfo,
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
}

type Shared = Arc<Mutex<Registry>>;

fn lock(r: &Shared) -> MutexGuard<'_, Registry> {
    r.lock().unwrap_or_else(|p| p.into_inner())
}

pub fn backend() -> Result<Backend, BackendError> {
    let host_exe = std::env::current_exe()
        .map_err(|e| BackendError::Platform(format!("locating displaylab: {e}")))?
        .with_file_name("displaylab-screen-host");
    if !host_exe.is_file() {
        return Err(BackendError::Platform(format!(
            "{} is missing; build the whole workspace (cargo build --workspace)",
            host_exe.display()
        )));
    }
    let registry: Shared = Arc::default();
    Ok((
        Box::new(MacDisplays {
            registry: registry.clone(),
            host_exe,
        }),
        Box::new(MacCapture {
            registry,
            streams: HashMap::new(),
        }),
    ))
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
        Ok(())
    }
}

impl Drop for MacDisplays {
    fn drop(&mut self) {
        let _ = self.reset();
    }
}

/// The newest complete frame from one ScreenCaptureKit stream.
struct Sink {
    latest: Arc<Mutex<Option<Frame>>>,
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
    streams: HashMap<ScreenId, Stream>,
}

impl MacCapture {
    /// A running stream on `cg_id`, or `None` if ScreenCaptureKit does not
    /// list the screen yet (it lags the window server by a moment after a
    /// plug).
    fn start(&self, cg_id: cg::DisplayId) -> Result<Option<Stream>, BackendError> {
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
        let filter = SCContentFilter::create()
            .with_display(&display)
            .with_excluding_windows(&[])
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

impl Capture for MacCapture {
    fn frame(&mut self, id: ScreenId) -> Result<Option<Frame>, BackendError> {
        let cg_id = {
            let r = lock(&self.registry);
            r.entries.iter().find(|e| e.lab == id).and_then(|e| e.cg)
        };
        let active = cg::active_displays();
        let Some(cg_id) = cg_id.filter(|c| active.contains(c)) else {
            // Gone, or not reported yet. Drop any stream on it.
            if let Some(old) = self.streams.remove(&id) {
                let _ = old.stream.stop_capture();
            }
            return Err(BackendError::NoSuchScreen(id));
        };
        if self.streams.get(&id).is_none_or(|s| s.cg != cg_id) {
            match self.start(cg_id)? {
                Some(stream) => {
                    self.streams.insert(id, stream);
                }
                None => return Ok(None),
            }
        }
        let Some(stream) = self.streams.get(&id) else {
            return Ok(None);
        };
        Ok(stream
            .latest
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone())
    }
}

impl Drop for MacCapture {
    fn drop(&mut self) {
        for (_, s) in self.streams.drain() {
            let _ = s.stream.stop_capture();
        }
    }
}
