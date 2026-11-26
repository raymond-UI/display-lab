//! The Win32 half of the backend: the driver pipe, the displays Windows
//! reports (`QueryDisplayConfig` and each monitor's EDID from the registry),
//! and capture with DXGI Desktop Duplication.

use crate::display::{
    bgra_to_rgba, edid_key, free_slot, host_display, parse_reply, parse_slots, qpc_to_lab_ns, wide,
    Display, Locator, PIPE,
};
use crate::edid;
use lab_core::{
    now_ns, Backend, BackendError, Capture, Displays, Frame, Identity, Mode, Profile, ScreenId,
    ScreenInfo,
};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use windows::core::{Interface, HSTRING, PCWSTR};
use windows::Win32::Devices::Display::{
    DisplayConfigGetDeviceInfo, GetDisplayConfigBufferSizes, QueryDisplayConfig,
    DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME, DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME,
    DISPLAYCONFIG_DEVICE_INFO_HEADER, DISPLAYCONFIG_MODE_INFO, DISPLAYCONFIG_MODE_INFO_TYPE_SOURCE,
    DISPLAYCONFIG_PATH_INFO, DISPLAYCONFIG_SOURCE_DEVICE_NAME, DISPLAYCONFIG_TARGET_DEVICE_NAME,
    QDC_ONLY_ACTIVE_PATHS,
};
use windows::Win32::Foundation::{ERROR_INSUFFICIENT_BUFFER, ERROR_SUCCESS, HMODULE};
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_UNKNOWN;
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D, D3D11_CPU_ACCESS_READ,
    D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_MAPPED_SUBRESOURCE, D3D11_MAP_READ, D3D11_SDK_VERSION,
    D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, IDXGIAdapter1, IDXGIFactory1, IDXGIOutput1, IDXGIOutputDuplication,
    IDXGIResource, DXGI_ERROR_ACCESS_LOST, DXGI_ERROR_WAIT_TIMEOUT, DXGI_OUTDUPL_FRAME_INFO,
};
use windows::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};
use windows::Win32::System::Registry::{RegGetValueW, HKEY_LOCAL_MACHINE, RRF_RT_REG_BINARY};
use windows::Win32::UI::HiDpi::{
    SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};

const NOT_INSTALLED: &str = "Display Lab's display driver is not installed, so there are no \
     virtual screens on this machine. Install it with `displaylab driver install` in an \
     administrator terminal, or use --backend fake";

fn platform(what: &str, e: impl std::fmt::Display) -> BackendError {
    BackendError::Platform(format!("{what}: {e}"))
}

fn debug() -> bool {
    std::env::var_os("DISPLAYLAB_DEBUG").is_some()
}

// ---- the driver -----------------------------------------------------------

/// One request to the driver: open the pipe, send a line, read the answer.
fn request(line: &str) -> Result<String, BackendError> {
    let deadline = Instant::now() + Duration::from_secs(5);
    let not_found_deadline = Instant::now() + Duration::from_millis(1500);
    let pipe = loop {
        match std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(PIPE)
        {
            Ok(pipe) => break pipe,
            // Briefly absent between two connections; absent for good when
            // the driver is not installed or not running.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                if Instant::now() >= not_found_deadline {
                    return Err(BackendError::Platform(NOT_INSTALLED.into()));
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            // ERROR_PIPE_BUSY: another request is being answered.
            Err(e) if e.raw_os_error() == Some(231) && Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(e) => return Err(platform("opening the driver's pipe", e)),
        }
    };
    let mut writer = &pipe;
    writer
        .write_all(format!("{line}\n").as_bytes())
        .map_err(|e| platform("writing to the driver", e))?;
    let mut answer = String::new();
    BufReader::new(&pipe)
        .read_line(&mut answer)
        .map_err(|e| platform("reading the driver's answer", e))?;
    parse_reply(&answer).map_err(|why| {
        let verb = line.split(' ').next().unwrap_or(line);
        BackendError::Platform(format!("the display driver refused {verb}: {why}"))
    })
}

fn plug_slot(slot: u8, profile: &Profile, mode: &Mode) -> Result<(), BackendError> {
    let edid = edid::build(&profile.identity, mode).map_err(|e| platform(&profile.name, e))?;
    request(&format!(
        "PLUG {slot} {} {}",
        edid::hex(&edid),
        edid::mode_arg(mode)
    ))
    .map(drop)
}

// ---- what Windows shows ---------------------------------------------------

/// Every display on the desktop now.
fn displays() -> Result<Vec<Display>, BackendError> {
    let (paths, modes) = query_config()?;
    let mut out = vec![];
    for path in &paths {
        // SAFETY: without QDC_VIRTUAL_MODE_AWARE the union holds modeInfoIdx.
        let idx = unsafe { path.sourceInfo.Anonymous.modeInfoIdx } as usize;
        let Some(mode) = modes.get(idx) else { continue };
        if mode.infoType != DISPLAYCONFIG_MODE_INFO_TYPE_SOURCE {
            continue;
        }
        // SAFETY: infoType says the union holds a source mode.
        let source = unsafe { mode.Anonymous.sourceMode };

        let mut name = DISPLAYCONFIG_SOURCE_DEVICE_NAME {
            header: header(
                DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME,
                size_of::<DISPLAYCONFIG_SOURCE_DEVICE_NAME>(),
                path.sourceInfo.adapterId,
                path.sourceInfo.id,
            ),
            ..Default::default()
        };
        let mut target = DISPLAYCONFIG_TARGET_DEVICE_NAME {
            header: header(
                DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME,
                size_of::<DISPLAYCONFIG_TARGET_DEVICE_NAME>(),
                path.targetInfo.adapterId,
                path.targetInfo.id,
            ),
            ..Default::default()
        };
        // SAFETY: each packet's header names its own type and size.
        let ok = unsafe {
            DisplayConfigGetDeviceInfo(&mut name.header) == 0
                && DisplayConfigGetDeviceInfo(&mut target.header) == 0
        };
        if !ok {
            continue;
        }
        let device_path = wide(&target.monitorDevicePath);
        let rate = path.targetInfo.refreshRate;
        out.push(Display {
            gdi_name: wide(&name.viewGdiDeviceName),
            identity: read_edid(&device_path).and_then(|e| edid::parse(&e).ok()),
            device_path,
            origin: (source.position.x, source.position.y),
            width: source.width,
            height: source.height,
            refresh_hz: if rate.Denominator == 0 {
                0.0
            } else {
                rate.Numerator as f64 / rate.Denominator as f64
            },
        });
    }
    Ok(out)
}

fn header(
    kind: windows::Win32::Devices::Display::DISPLAYCONFIG_DEVICE_INFO_TYPE,
    size: usize,
    adapter: windows::Win32::Foundation::LUID,
    id: u32,
) -> DISPLAYCONFIG_DEVICE_INFO_HEADER {
    DISPLAYCONFIG_DEVICE_INFO_HEADER {
        r#type: kind,
        size: size as u32,
        adapterId: adapter,
        id,
    }
}

fn query_config(
) -> Result<(Vec<DISPLAYCONFIG_PATH_INFO>, Vec<DISPLAYCONFIG_MODE_INFO>), BackendError> {
    // The configuration can change between asking its size and reading it;
    // then ask again.
    for _ in 0..5 {
        let (mut np, mut nm) = (0u32, 0u32);
        // SAFETY: two valid out-pointers.
        let err = unsafe { GetDisplayConfigBufferSizes(QDC_ONLY_ACTIVE_PATHS, &mut np, &mut nm) };
        if err != ERROR_SUCCESS {
            return Err(platform("GetDisplayConfigBufferSizes", format!("{err:?}")));
        }
        let mut paths = vec![DISPLAYCONFIG_PATH_INFO::default(); np as usize];
        let mut modes = vec![DISPLAYCONFIG_MODE_INFO::default(); nm as usize];
        // SAFETY: the buffers hold the counts passed in.
        let err = unsafe {
            QueryDisplayConfig(
                QDC_ONLY_ACTIVE_PATHS,
                &mut np,
                paths.as_mut_ptr(),
                &mut nm,
                modes.as_mut_ptr(),
                None,
            )
        };
        if err == ERROR_INSUFFICIENT_BUFFER {
            continue;
        }
        if err != ERROR_SUCCESS {
            return Err(platform("QueryDisplayConfig", format!("{err:?}")));
        }
        paths.truncate(np as usize);
        modes.truncate(nm as usize);
        return Ok((paths, modes));
    }
    Err(BackendError::Platform(
        "the display configuration kept changing while it was read".into(),
    ))
}

/// A monitor's EDID, as Windows stored it when the monitor arrived.
fn read_edid(device_path: &str) -> Option<Vec<u8>> {
    let key = HSTRING::from(edid_key(device_path)?);
    let value = HSTRING::from("EDID");
    let mut buf = vec![0u8; 512];
    let mut size = buf.len() as u32;
    // SAFETY: the buffer holds `size` bytes; both strings outlive the call.
    let err = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            PCWSTR(key.as_ptr()),
            PCWSTR(value.as_ptr()),
            RRF_RT_REG_BINARY,
            None,
            Some(buf.as_mut_ptr().cast()),
            Some(&mut size),
        )
    };
    (err == ERROR_SUCCESS).then(|| {
        buf.truncate(size as usize);
        buf
    })
}

// ---- the backend ----------------------------------------------------------

struct Entry {
    lab: ScreenId,
    /// The driver slot, for a screen the lab plugged.
    slot: Option<u8>,
    locator: Locator,
    /// What the lab reports: the profile's identity, full name included.
    identity: Identity,
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
    // Real pixels everywhere: without this, sizes and captures of a scaled
    // screen come back scaled. Fails harmlessly if already set.
    // SAFETY: no pointers; sets a process-wide flag.
    let _ = unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };

    let version = request("PING")?;
    if debug() {
        eprintln!("[lab-win] driver: {version}");
    }
    // Screens a crashed run left behind would confuse this one.
    for slot in parse_slots(&request("LIST")?) {
        let _ = request(&format!("UNPLUG {slot}"));
    }

    let registry: Shared = Arc::default();
    Ok((
        Box::new(WinDisplays {
            registry: registry.clone(),
        }),
        Box::new(WinCapture {
            registry,
            outputs: HashMap::new(),
        }),
    ))
}

struct WinDisplays {
    registry: Shared,
}

impl Displays for WinDisplays {
    fn host_screen(&mut self, profile: &Profile, _mode: &Mode) -> Result<ScreenId, BackendError> {
        let all = displays()?;
        let host = host_display(&all)
            .ok_or_else(|| BackendError::Platform("Windows reports no screen at all".into()))?;
        // The real panel's identity, when Windows has its EDID; the profile
        // only names it.
        let found = host.identity.clone().unwrap_or(Identity {
            vendor: 0,
            product: 0,
            serial: 0,
            name: String::new(),
        });
        let mut r = lock(&self.registry);
        r.next += 1;
        let lab = ScreenId(r.next);
        r.entries.push(Entry {
            lab,
            slot: None,
            locator: Locator::Host {
                device_path: host.device_path.clone(),
            },
            identity: Identity {
                name: profile.identity.name.clone(),
                ..found
            },
            failed: None,
        });
        Ok(lab)
    }

    fn plug(&mut self, profile: &Profile, mode: &Mode) -> Result<ScreenId, BackendError> {
        let (lab, slot) = {
            let mut r = lock(&self.registry);
            let slot = free_slot(r.entries.iter().filter_map(|e| e.slot)).ok_or_else(|| {
                BackendError::Platform("the display driver has no free slot (16 screens)".into())
            })?;
            r.next += 1;
            let lab = ScreenId(r.next);
            r.entries.push(Entry {
                lab,
                slot: Some(slot),
                locator: Locator::for_identity(&profile.identity),
                identity: profile.identity.clone(),
                failed: None,
            });
            (lab, slot)
        };

        let delay = Duration::from_millis(profile.timings.report_delay_ms);
        if delay.is_zero() {
            if let Err(e) = plug_slot(slot, profile, mode) {
                lock(&self.registry).entries.retain(|e| e.lab != lab);
                return Err(e);
            }
            return Ok(lab);
        }
        // A projector that is slow to wake: the monitor arrives later, and
        // plug returns now so the runner can watch the world meanwhile.
        let (registry, profile, mode) = (self.registry.clone(), profile.clone(), *mode);
        std::thread::spawn(move || {
            std::thread::sleep(delay);
            // Unplugged while waking: nothing to plug.
            if !lock(&registry).entries.iter().any(|e| e.lab == lab) {
                return;
            }
            if let Err(e) = plug_slot(slot, &profile, &mode) {
                if let Some(entry) = lock(&registry).entries.iter_mut().find(|e| e.lab == lab) {
                    entry.failed = Some(e.to_string());
                }
            }
        });
        Ok(lab)
    }

    fn unplug(&mut self, id: ScreenId) -> Result<(), BackendError> {
        let entry = {
            let mut r = lock(&self.registry);
            let i = r
                .entries
                .iter()
                .position(|e| e.lab == id)
                .ok_or(BackendError::NoSuchScreen(id))?;
            if r.entries[i].slot.is_none() {
                return Err(BackendError::Platform(
                    "the host screen cannot be unplugged".into(),
                ));
            }
            r.entries.remove(i)
        };
        let Some(slot) = entry.slot else {
            return Ok(());
        };
        // A delayed plug that has not happened yet has nothing to unplug:
        // removing the entry already stopped it.
        if !in_use(slot)? {
            return Ok(());
        }
        request(&format!("UNPLUG {slot}")).map(drop)
    }

    fn screens(&mut self) -> Result<Vec<ScreenInfo>, BackendError> {
        let all = displays()?;
        let r = lock(&self.registry);
        if debug() {
            for d in &all {
                eprintln!("[lab-win] display {d:?}");
            }
            for e in &r.entries {
                eprintln!(
                    "[lab-win] screen {:?}: slot={:?} found={} failed={:?}",
                    e.lab,
                    e.slot,
                    e.locator.find(&all).is_some(),
                    e.failed
                );
            }
        }
        Ok(r.entries
            .iter()
            .filter_map(|e| {
                let d = e.locator.find(&all)?;
                Some(ScreenInfo {
                    id: e.lab,
                    identity: e.identity.clone(),
                    mode: Mode {
                        width: d.width,
                        height: d.height,
                        refresh_hz: d.refresh_hz,
                        hidpi: false,
                    },
                    origin: d.origin,
                    host: e.slot.is_none(),
                })
            })
            .collect())
    }

    fn reset(&mut self) -> Result<(), BackendError> {
        let slots: Vec<u8> = {
            let mut r = lock(&self.registry);
            let slots = r.entries.iter().filter_map(|e| e.slot).collect();
            r.entries.clear();
            slots
        };
        for slot in slots {
            if in_use(slot)? {
                request(&format!("UNPLUG {slot}"))?;
            }
        }
        Ok(())
    }
}

fn in_use(slot: u8) -> Result<bool, BackendError> {
    Ok(parse_slots(&request("LIST")?).contains(&slot))
}

impl Drop for WinDisplays {
    fn drop(&mut self) {
        let _ = self.reset();
    }
}

// ---- capture --------------------------------------------------------------

/// Desktop Duplication on one output.
struct Output {
    gdi_name: String,
    context: ID3D11DeviceContext,
    device: ID3D11Device,
    duplication: IDXGIOutputDuplication,
    staging: Option<(ID3D11Texture2D, u32, u32)>,
    latest: Option<Frame>,
}

enum Grab {
    /// The duplication no longer works (a mode change, a desktop switch).
    Lost,
    Failed(BackendError),
}

struct WinCapture {
    registry: Shared,
    outputs: HashMap<ScreenId, Output>,
}

impl WinCapture {
    /// Start duplicating the output Windows calls `gdi_name`, or `None` if
    /// DXGI does not list it yet (it lags the display configuration by a
    /// moment after a plug).
    fn open(gdi_name: &str) -> Result<Option<Output>, BackendError> {
        // SAFETY: plain COM calls; every interface returned is owned.
        unsafe {
            let factory: IDXGIFactory1 =
                CreateDXGIFactory1().map_err(|e| platform("CreateDXGIFactory1", e))?;
            for a in 0.. {
                let Ok(adapter) = factory.EnumAdapters1(a) else {
                    break;
                };
                for o in 0.. {
                    let Ok(output) = adapter.EnumOutputs(o) else {
                        break;
                    };
                    let Ok(desc) = output.GetDesc() else { continue };
                    if wide(&desc.DeviceName) != gdi_name {
                        continue;
                    }
                    let output: IDXGIOutput1 =
                        output.cast().map_err(|e| platform("IDXGIOutput1", e))?;
                    return Self::duplicate(&adapter, &output, gdi_name).map(Some);
                }
            }
        }
        Ok(None)
    }

    fn duplicate(
        adapter: &IDXGIAdapter1,
        output: &IDXGIOutput1,
        gdi_name: &str,
    ) -> Result<Output, BackendError> {
        let mut device = None;
        let mut context = None;
        // SAFETY: out-pointers to locals; the adapter is a live COM object.
        unsafe {
            D3D11CreateDevice(
                adapter,
                D3D_DRIVER_TYPE_UNKNOWN,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                None,
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                Some(&mut context),
            )
            .map_err(|e| platform("D3D11CreateDevice", e))?;
        }
        let (Some(device), Some(context)) = (device, context) else {
            return Err(BackendError::Platform(
                "D3D11CreateDevice returned nothing".into(),
            ));
        };
        // SAFETY: the device lives as long as the duplication.
        let duplication = unsafe { output.DuplicateOutput(&device) }
            .map_err(|e| platform(&format!("duplicating {gdi_name}"), e))?;
        Ok(Output {
            gdi_name: gdi_name.to_string(),
            context,
            device,
            duplication,
            staging: None,
            latest: None,
        })
    }

    /// Take the newest frame from `out`, if there is one.
    fn grab(out: &mut Output) -> Result<(), Grab> {
        let mut info = DXGI_OUTDUPL_FRAME_INFO::default();
        let mut resource: Option<IDXGIResource> = None;
        // The first frame after opening can take a moment; after that,
        // nothing new means the last frame is still right.
        let wait = if out.latest.is_none() { 100 } else { 0 };
        // SAFETY: out-pointers to locals.
        let acquired = unsafe {
            out.duplication
                .AcquireNextFrame(wait, &mut info, &mut resource)
        };
        match acquired {
            Ok(()) => {}
            Err(e) if e.code() == DXGI_ERROR_WAIT_TIMEOUT => return Ok(()),
            Err(e) if e.code() == DXGI_ERROR_ACCESS_LOST => return Err(Grab::Lost),
            Err(e) => return Err(Grab::Failed(platform("AcquireNextFrame", e))),
        }
        let result = Self::copy(out, &info, resource);
        // SAFETY: a frame was acquired above.
        let _ = unsafe { out.duplication.ReleaseFrame() };
        result.map_err(Grab::Failed)
    }

    fn copy(
        out: &mut Output,
        info: &DXGI_OUTDUPL_FRAME_INFO,
        resource: Option<IDXGIResource>,
    ) -> Result<(), BackendError> {
        // Only the pointer moved: the picture did not change.
        if info.LastPresentTime == 0 && out.latest.is_some() {
            return Ok(());
        }
        let Some(resource) = resource else {
            return Ok(());
        };
        let texture: ID3D11Texture2D = resource.cast().map_err(|e| platform("frame texture", e))?;
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        // SAFETY: an out-pointer to a local.
        unsafe { texture.GetDesc(&mut desc) };
        if desc.Format != DXGI_FORMAT_B8G8R8A8_UNORM {
            return Err(BackendError::Platform(format!(
                "{} captures as {:?}, not 8-bit BGRA (is HDR on?)",
                out.gdi_name, desc.Format
            )));
        }
        if !matches!(&out.staging, Some((_, w, h)) if *w == desc.Width && *h == desc.Height) {
            let staging_desc = D3D11_TEXTURE2D_DESC {
                Width: desc.Width,
                Height: desc.Height,
                MipLevels: 1,
                ArraySize: 1,
                Format: desc.Format,
                SampleDesc: DXGI_SAMPLE_DESC {
                    Count: 1,
                    Quality: 0,
                },
                Usage: D3D11_USAGE_STAGING,
                BindFlags: 0,
                CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
                MiscFlags: 0,
            };
            let mut staging = None;
            // SAFETY: valid description and out-pointer.
            unsafe {
                out.device
                    .CreateTexture2D(&staging_desc, None, Some(&mut staging))
            }
            .map_err(|e| platform("CreateTexture2D", e))?;
            let staging = staging
                .ok_or_else(|| BackendError::Platform("CreateTexture2D returned nothing".into()))?;
            out.staging = Some((staging, desc.Width, desc.Height));
        }
        let Some((staging, w, h)) = out.staging.clone() else {
            return Ok(());
        };
        let staging = &staging;
        let (w, h) = (&w, &h);
        let mut mapped = D3D11_MAPPED_SUBRESOURCE {
            pData: std::ptr::null_mut(),
            RowPitch: 0,
            DepthPitch: 0,
        };
        // SAFETY: both textures are live and the same size and format; the
        // mapping is read and released before anything else touches it.
        let rgba = unsafe {
            out.context.CopyResource(staging, &texture);
            out.context
                .Map(staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))
                .map_err(|e| platform("Map", e))?;
            let pitch = mapped.RowPitch as usize;
            let bytes =
                std::slice::from_raw_parts(mapped.pData as *const u8, pitch * (*h as usize));
            let rgba = bgra_to_rgba(bytes, pitch, *w as usize, *h as usize);
            out.context.Unmap(staging, 0);
            rgba
        };
        let rgba =
            rgba.ok_or_else(|| BackendError::Platform("captured frame has a short row".into()))?;

        let (mut qpc_now, mut frequency) = (0i64, 0i64);
        // SAFETY: out-pointers to locals.
        unsafe {
            let _ = QueryPerformanceCounter(&mut qpc_now);
            let _ = QueryPerformanceFrequency(&mut frequency);
        }
        let present_ns = qpc_to_lab_ns(info.LastPresentTime, qpc_now, frequency, now_ns());
        out.latest = Some(Frame {
            width: *w,
            height: *h,
            rgba,
            present_ns,
        });
        Ok(())
    }
}

impl Capture for WinCapture {
    fn frame(&mut self, id: ScreenId) -> Result<Option<Frame>, BackendError> {
        let locator = {
            let r = lock(&self.registry);
            r.entries
                .iter()
                .find(|e| e.lab == id)
                .map(|e| e.locator.clone())
        };
        let all = displays()?;
        let Some(display) = locator.as_ref().and_then(|l| l.find(&all)) else {
            // Gone, or not on the desktop yet.
            self.outputs.remove(&id);
            return Ok(None);
        };
        // The output can change under a screen (a mode change is an unplug
        // and a plug, and Windows may renumber); follow it.
        if self
            .outputs
            .get(&id)
            .is_some_and(|o| o.gdi_name != display.gdi_name)
        {
            self.outputs.remove(&id);
        }
        if let std::collections::hash_map::Entry::Vacant(slot) = self.outputs.entry(id) {
            match Self::open(&display.gdi_name)? {
                Some(out) => {
                    slot.insert(out);
                }
                None => return Ok(None),
            }
        }
        let Some(out) = self.outputs.get_mut(&id) else {
            return Ok(None);
        };
        match Self::grab(out) {
            Ok(()) => Ok(out.latest.clone()),
            // A mode change or a desktop switch: start over next time.
            Err(Grab::Lost) => {
                let latest = out.latest.clone();
                self.outputs.remove(&id);
                Ok(latest)
            }
            Err(Grab::Failed(e)) => Err(e),
        }
    }
}
