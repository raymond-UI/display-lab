//! Installing Display Lab's display driver: what `devcon install` does,
//! without needing devcon.
//!
//! 1. Trust the certificate the driver package is signed with, for code
//!    signing on this machine (the Root and TrustedPublisher stores).
//! 2. Create the root-enumerated device the driver binds to.
//! 3. Install the driver on it, then wait for the driver to answer.
//!
//! All of it needs an administrator terminal.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use windows::core::{HSTRING, PCWSTR};
use windows::Win32::Devices::DeviceAndDriverInstallation::{
    DiUninstallDevice, SetupDiCallClassInstaller, SetupDiCreateDeviceInfoList,
    SetupDiCreateDeviceInfoW, SetupDiDestroyDeviceInfoList, SetupDiEnumDeviceInfo,
    SetupDiGetClassDevsW, SetupDiGetDeviceRegistryPropertyW, SetupDiSetDeviceRegistryPropertyW,
    UpdateDriverForPlugAndPlayDevicesW, DICD_GENERATE_ID, DIF_REGISTERDEVICE,
    GUID_DEVCLASS_DISPLAY, HDEVINFO, INSTALLFLAG_FORCE, SETUP_DI_GET_CLASS_DEVS_FLAGS,
    SPDRP_HARDWAREID, SP_DEVINFO_DATA,
};
use windows::Win32::Foundation::{ERROR_ACCESS_DENIED, HWND};
use windows::Win32::Security::Cryptography::{
    CertCloseStore, CertDeleteCertificateFromStore, CertDuplicateCertificateContext,
    CertEnumCertificatesInStore, CertGetCertificateContextProperty, CertGetNameStringW,
    CertOpenStore, CERT_CONTEXT, CERT_NAME_SIMPLE_DISPLAY_TYPE, CERT_OPEN_STORE_FLAGS,
    CERT_QUERY_ENCODING_TYPE, CERT_SHA1_HASH_PROP_ID, CERT_STORE_OPEN_EXISTING_FLAG,
    CERT_STORE_PROV_SYSTEM_W, CERT_SYSTEM_STORE_LOCAL_MACHINE, HCERTSTORE,
};

use crate::display::{hex, is_driver_certificate, wide};

/// The hardware id in DisplayLabIdd.inf.
const HARDWARE_ID: &str = r"Root\DisplayLabIdd";

#[derive(Debug, thiserror::Error)]
pub enum InstallError {
    #[error("{0}")]
    Package(String),
    #[error(
        "installing a driver needs an administrator terminal: right-click Terminal (or \
         PowerShell), choose \"Run as administrator\", and run this again"
    )]
    NotAdmin,
    #[error("{what}: {why}")]
    Failed { what: String, why: String },
}

fn failed(what: &str, why: impl std::fmt::Display) -> InstallError {
    InstallError::Failed {
        what: what.into(),
        why: why.to_string(),
    }
}

fn from_win(what: &str, e: windows::core::Error) -> InstallError {
    if e.code() == ERROR_ACCESS_DENIED.to_hresult() {
        InstallError::NotAdmin
    } else {
        failed(what, e)
    }
}

/// The files a driver package must have.
struct Package {
    inf: PathBuf,
    cer: PathBuf,
}

fn package(dir: &Path) -> Result<Package, InstallError> {
    let find = |ext: &str| -> Option<PathBuf> {
        std::fs::read_dir(dir)
            .ok()?
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .find(|p| {
                p.extension()
                    .is_some_and(|x| x.to_string_lossy().eq_ignore_ascii_case(ext))
            })
    };
    let missing = |what: &str| {
        InstallError::Package(format!(
            "{} has no {what}; it should hold the driver package \
             (DisplayLabIdd.inf, .dll, .cat and the .cer it is signed with)",
            dir.display()
        ))
    };
    for ext in ["dll", "cat"] {
        find(ext).ok_or_else(|| missing(&format!(".{ext}")))?;
    }
    Ok(Package {
        inf: find("inf").ok_or_else(|| missing(".inf"))?,
        cer: find("cer").ok_or_else(|| missing(".cer certificate"))?,
    })
}

fn certutil(args: &[&str]) -> Result<String, InstallError> {
    let out = Command::new("certutil")
        .args(args)
        .output()
        .map_err(|e| failed("running certutil", e))?;
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    if out.status.success() {
        Ok(text)
    } else if text.contains("0x80070005") || text.to_lowercase().contains("access is denied") {
        Err(InstallError::NotAdmin)
    } else {
        Err(failed(&format!("certutil {}", args.join(" ")), text.trim()))
    }
}

/// Install the driver package in `dir`. Returns what it did, for the user.
pub fn install(dir: &Path) -> Result<String, InstallError> {
    let pkg = package(dir)?;
    let cer = pkg.cer.display().to_string();
    let mut done = vec![];

    certutil(&["-f", "-addstore", "Root", &cer])?;
    certutil(&["-f", "-addstore", "TrustedPublisher", &cer])?;
    done.push(format!(
        "trusted {} for code signing on this machine",
        pkg.cer
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or(cer)
    ));

    if devices()?.is_empty() {
        create_device()?;
        done.push(format!("created the {HARDWARE_ID} device"));
    }

    let inf = HSTRING::from(pkg.inf.as_path());
    let id = HSTRING::from(HARDWARE_ID);
    let mut reboot = windows::core::BOOL::default();
    // SAFETY: both strings outlive the call; the out-pointer is a local.
    unsafe {
        UpdateDriverForPlugAndPlayDevicesW(
            None,
            PCWSTR(id.as_ptr()),
            PCWSTR(inf.as_ptr()),
            INSTALLFLAG_FORCE,
            Some(&mut reboot),
        )
    }
    .map_err(|e| from_win("installing the driver", e))?;
    done.push("installed the driver".into());
    if reboot.as_bool() {
        done.push("Windows asks for a restart before the driver starts".into());
        return Ok(done.join("\n"));
    }

    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        match ping() {
            Ok(version) => {
                done.push(format!("the driver is running ({version})"));
                return Ok(done.join("\n"));
            }
            Err(_) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(250)),
            Err(e) => return Err(failed("the driver was installed but does not answer", e)),
        }
    }
}

/// What is installed, in a line.
pub fn status() -> Result<String, InstallError> {
    let devices = devices()?.len();
    Ok(match (devices, ping()) {
        (_, Ok(version)) => format!("installed and running ({version})"),
        (0, Err(_)) => "not installed".into(),
        (_, Err(e)) => format!("installed, but the driver does not answer: {e}"),
    })
}

/// Remove the device, the driver package and the certificate.
pub fn uninstall() -> Result<String, InstallError> {
    let mut done = vec![];
    let removed = remove_devices()?;
    if removed > 0 {
        done.push(format!("removed {removed} {HARDWARE_ID} device(s)"));
    }
    for published in published_infs()? {
        let out = Command::new("pnputil")
            .args(["/delete-driver", &published, "/uninstall", "/force"])
            .output()
            .map_err(|e| failed("running pnputil", e))?;
        if out.status.success() {
            done.push(format!("deleted the driver package {published}"));
        }
    }
    for store in ["TrustedPublisher", "Root"] {
        for (name, thumbprint) in remove_certificates(store)? {
            done.push(format!(
                "removed the \"{name}\" certificate ({thumbprint}) from {store}"
            ));
        }
    }
    if done.is_empty() {
        done.push("nothing to remove: the driver was not installed".into());
    }
    Ok(done.join("\n"))
}

/// A machine certificate store, closed when dropped.
struct Store(HCERTSTORE);

impl Drop for Store {
    fn drop(&mut self) {
        // SAFETY: the store came from CertOpenStore and is closed once.
        let _ = unsafe { CertCloseStore(Some(self.0), 0) };
    }
}

fn open_store(name: &str) -> Result<Store, InstallError> {
    let wide_name = HSTRING::from(name);
    // SAFETY: the name outlives the call.
    unsafe {
        CertOpenStore(
            CERT_STORE_PROV_SYSTEM_W,
            CERT_QUERY_ENCODING_TYPE(0),
            None,
            CERT_OPEN_STORE_FLAGS(
                CERT_SYSTEM_STORE_LOCAL_MACHINE | CERT_STORE_OPEN_EXISTING_FLAG.0,
            ),
            Some(wide_name.as_ptr().cast()),
        )
    }
    .map(Store)
    .map_err(|e| from_win(&format!("opening the {name} certificate store"), e))
}

/// A certificate's simple name (its common name, usually).
///
/// # Safety
/// `cert` is a live certificate context.
unsafe fn certificate_name(cert: *const CERT_CONTEXT) -> String {
    let mut buf = [0u16; 256];
    // SAFETY: the buffer is passed with its length.
    let n =
        unsafe { CertGetNameStringW(cert, CERT_NAME_SIMPLE_DISPLAY_TYPE, 0, None, Some(&mut buf)) };
    wide(&buf[..(n as usize).min(buf.len())])
}

/// A certificate's SHA-1 thumbprint, as Windows shows it.
///
/// # Safety
/// `cert` is a live certificate context.
unsafe fn thumbprint(cert: *const CERT_CONTEXT) -> String {
    let mut hash = [0u8; 20];
    let mut size = hash.len() as u32;
    // SAFETY: the buffer holds `size` bytes.
    let got = unsafe {
        CertGetCertificateContextProperty(
            cert,
            CERT_SHA1_HASH_PROP_ID,
            Some(hash.as_mut_ptr().cast()),
            &mut size,
        )
    };
    match got {
        Ok(()) => hex(&hash[..(size as usize).min(hash.len())]),
        Err(_) => "no thumbprint".into(),
    }
}

/// Delete every Display Lab driver certificate from the machine's `store`.
/// Returns each one deleted, as (name, thumbprint); nothing is reported that
/// Windows did not delete.
fn remove_certificates(store: &str) -> Result<Vec<(String, String)>, InstallError> {
    let opened = open_store(store)?;
    let mut ours = vec![];
    // SAFETY: enumeration frees the context passed back in, so the ones
    // kept are duplicated; each duplicate is freed by the delete below.
    unsafe {
        let mut cert = CertEnumCertificatesInStore(opened.0, None);
        while !cert.is_null() {
            let name = certificate_name(cert);
            if is_driver_certificate(&name) {
                ours.push((
                    CertDuplicateCertificateContext(Some(cert)),
                    name,
                    thumbprint(cert),
                ));
            }
            cert = CertEnumCertificatesInStore(opened.0, Some(cert));
        }
    }
    let mut removed = vec![];
    let mut first_error = None;
    for (cert, name, thumbprint) in ours {
        // SAFETY: a duplicate this function owns; the delete always frees
        // it, even when it fails.
        match unsafe { CertDeleteCertificateFromStore(cert) } {
            Ok(()) => removed.push((name, thumbprint)),
            Err(e) => {
                first_error.get_or_insert_with(|| {
                    from_win(
                        &format!("removing the \"{name}\" certificate from {store}"),
                        e,
                    )
                });
            }
        }
    }
    match first_error {
        Some(e) => Err(e),
        None => Ok(removed),
    }
}

fn ping() -> Result<String, String> {
    crate::win::request("PING").map_err(|e| e.to_string())
}

/// Device-info entries for every DisplayLabIdd device, present or not.
struct DeviceSet(HDEVINFO);

impl Drop for DeviceSet {
    fn drop(&mut self) {
        // SAFETY: the set came from SetupDiGetClassDevsW and is freed once.
        let _ = unsafe { SetupDiDestroyDeviceInfoList(self.0) };
    }
}

fn hardware_ids(set: HDEVINFO, data: &SP_DEVINFO_DATA) -> Vec<String> {
    let mut buf = [0u8; 1024];
    // SAFETY: the buffer is passed with its length.
    let ok = unsafe {
        SetupDiGetDeviceRegistryPropertyW(set, data, SPDRP_HARDWAREID, None, Some(&mut buf), None)
    };
    if ok.is_err() {
        return vec![];
    }
    let wide: Vec<u16> = buf
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect();
    wide.split(|&c| c == 0)
        .filter(|s| !s.is_empty())
        .map(String::from_utf16_lossy)
        .collect()
}

/// Every DisplayLabIdd device, as (set, entry) pairs.
fn devices() -> Result<Vec<(std::rc::Rc<DeviceSet>, SP_DEVINFO_DATA)>, InstallError> {
    // SAFETY: a class GUID and no enumerator: all display-class devices,
    // present or not (flags 0).
    let set = unsafe {
        SetupDiGetClassDevsW(
            Some(&GUID_DEVCLASS_DISPLAY),
            PCWSTR::null(),
            None,
            SETUP_DI_GET_CLASS_DEVS_FLAGS(0),
        )
    }
    .map_err(|e| from_win("listing display devices", e))?;
    let set = std::rc::Rc::new(DeviceSet(set));
    let mut out = vec![];
    for i in 0.. {
        let mut data = SP_DEVINFO_DATA {
            cbSize: size_of::<SP_DEVINFO_DATA>() as u32,
            ..Default::default()
        };
        // SAFETY: data.cbSize is set as the call requires.
        if unsafe { SetupDiEnumDeviceInfo(set.0, i, &mut data) }.is_err() {
            break;
        }
        if hardware_ids(set.0, &data)
            .iter()
            .any(|id| id.eq_ignore_ascii_case(HARDWARE_ID))
        {
            out.push((set.clone(), data));
        }
    }
    Ok(out)
}

fn create_device() -> Result<(), InstallError> {
    // SAFETY: a fresh, empty set for the display class; freed by DeviceSet.
    let set = unsafe { SetupDiCreateDeviceInfoList(Some(&GUID_DEVCLASS_DISPLAY), None) }
        .map_err(|e| from_win("creating a device list", e))?;
    let set = DeviceSet(set);
    let mut data = SP_DEVINFO_DATA {
        cbSize: size_of::<SP_DEVINFO_DATA>() as u32,
        ..Default::default()
    };
    let class = HSTRING::from("Display");
    // SAFETY: valid set, class GUID and out-pointer; the name outlives the call.
    unsafe {
        SetupDiCreateDeviceInfoW(
            set.0,
            PCWSTR(class.as_ptr()),
            &GUID_DEVCLASS_DISPLAY,
            PCWSTR::null(),
            None,
            DICD_GENERATE_ID,
            Some(&mut data),
        )
    }
    .map_err(|e| from_win("creating the device", e))?;
    // A REG_MULTI_SZ: the id, then two NULs.
    let ids: Vec<u8> = HARDWARE_ID
        .encode_utf16()
        .chain([0, 0])
        .flat_map(u16::to_le_bytes)
        .collect();
    // SAFETY: the buffer is passed with its length.
    unsafe { SetupDiSetDeviceRegistryPropertyW(set.0, &mut data, SPDRP_HARDWAREID, Some(&ids)) }
        .map_err(|e| from_win("naming the device", e))?;
    // SAFETY: the entry was created in this set above.
    unsafe { SetupDiCallClassInstaller(DIF_REGISTERDEVICE, set.0, Some(&data)) }
        .map_err(|e| from_win("registering the device", e))?;
    Ok(())
}

fn remove_devices() -> Result<usize, InstallError> {
    let devices = devices()?;
    for (set, data) in &devices {
        // SAFETY: the entry belongs to the set, which is alive.
        unsafe { DiUninstallDevice(HWND::default(), set.0, data, 0, None) }
            .map_err(|e| from_win("removing the device", e))?;
    }
    Ok(devices.len())
}

/// The driver store names (oemNN.inf) of every installed DisplayLabIdd
/// package, from `pnputil /enum-drivers`.
fn published_infs() -> Result<Vec<String>, InstallError> {
    let out = Command::new("pnputil")
        .arg("/enum-drivers")
        .output()
        .map_err(|e| failed("running pnputil", e))?;
    Ok(crate::display::published_infs(
        &String::from_utf8_lossy(&out.stdout),
        "displaylabidd.inf",
    ))
}
