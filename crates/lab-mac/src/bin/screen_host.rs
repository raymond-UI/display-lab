//! `displaylab-screen-host`: owns exactly one virtual screen.
//!
//! The lab starts one of these per screen it plugs in. It creates a
//! `CGVirtualDisplay`, prints `ready <display id>` on stdout, and then keeps
//! the screen alive until stdin closes or the process is killed. When the
//! process exits, for any reason, the window server removes the screen.
//!
//! Arguments, all required unless marked:
//!   --name TEXT --vendor N --product N --serial N
//!   --width PX --height PX --refresh HZ
//!   --hidpi            (optional flag)
//!   --delay-ms MS      (optional: wait this long before the screen appears,
//!                       a projector that is slow to wake)
//!
//! On failure it prints `error <reason>` and exits 2.

#[cfg(not(target_os = "macos"))]
fn main() {
    println!("error displaylab-screen-host only runs on macOS");
    std::process::exit(2);
}

#[cfg(target_os = "macos")]
fn main() {
    match host::run() {
        Ok(never) => match never {},
        Err(reason) => {
            println!("error {reason}");
            std::process::exit(2);
        }
    }
}

#[cfg(target_os = "macos")]
mod host {
    use objc2::rc::{Allocated, Retained};
    use objc2::runtime::{AnyClass, AnyObject, Bool, Sel};
    use objc2::{msg_send, sel};
    use objc2_foundation::{NSArray, NSSize, NSString};
    use std::ffi::CStr;
    use std::io::{Read, Write};
    use std::time::Duration;

    pub enum Never {}

    struct Args {
        name: String,
        vendor: u32,
        product: u32,
        serial: u32,
        width: u32,
        height: u32,
        refresh: f64,
        hidpi: bool,
        delay_ms: u64,
    }

    fn parse() -> Result<Args, String> {
        let mut it = std::env::args().skip(1);
        let get = |key: &str, it: &mut dyn Iterator<Item = String>| {
            it.next().ok_or_else(|| format!("{key} needs a value"))
        };
        let (mut name, mut vendor, mut product, mut serial) = (None, None, None, None);
        let (mut width, mut height, mut refresh) = (None, None, None);
        let (mut hidpi, mut delay_ms) = (false, 0u64);
        let num = |k: &str, v: String| {
            v.parse::<u64>()
                .map_err(|_| format!("{k}: {v:?} is not a number"))
        };
        while let Some(key) = it.next() {
            match key.as_str() {
                "--name" => name = Some(get(&key, &mut it)?),
                "--vendor" => vendor = Some(num(&key, get(&key, &mut it)?)? as u32),
                "--product" => product = Some(num(&key, get(&key, &mut it)?)? as u32),
                "--serial" => serial = Some(num(&key, get(&key, &mut it)?)? as u32),
                "--width" => width = Some(num(&key, get(&key, &mut it)?)? as u32),
                "--height" => height = Some(num(&key, get(&key, &mut it)?)? as u32),
                "--refresh" => {
                    let v = get(&key, &mut it)?;
                    refresh = Some(
                        v.parse::<f64>()
                            .map_err(|_| format!("--refresh: {v:?} is not a number"))?,
                    );
                }
                "--hidpi" => hidpi = true,
                "--delay-ms" => delay_ms = num(&key, get(&key, &mut it)?)?,
                other => return Err(format!("unknown argument {other}")),
            }
        }
        let need = |k: &str| format!("missing {k}");
        Ok(Args {
            name: name.ok_or_else(|| need("--name"))?,
            vendor: vendor.ok_or_else(|| need("--vendor"))?,
            product: product.ok_or_else(|| need("--product"))?,
            serial: serial.ok_or_else(|| need("--serial"))?,
            width: width.ok_or_else(|| need("--width"))?,
            height: height.ok_or_else(|| need("--height"))?,
            refresh: refresh.ok_or_else(|| need("--refresh"))?,
            hidpi,
            delay_ms,
        })
    }

    /// The private class, after checking it answers every selector we send.
    /// A macOS update that renames one must fail here with a clear message,
    /// not crash the window server's client half-way through.
    fn class(name: &CStr, instance: &[Sel]) -> Result<&'static AnyClass, String> {
        let shown = name.to_string_lossy();
        let cls =
            AnyClass::get(name).ok_or_else(|| format!("{shown} does not exist on this macOS"))?;
        for sel in instance {
            if cls.instance_method(*sel).is_none() {
                return Err(format!(
                    "{shown} no longer answers {}",
                    sel.name().to_string_lossy()
                ));
            }
        }
        Ok(cls)
    }

    unsafe extern "C" {
        fn dispatch_main() -> !;
        static _dispatch_main_q: AnyObject;
    }

    pub fn run() -> Result<Never, String> {
        let a = parse()?;
        if a.delay_ms > 0 {
            std::thread::sleep(Duration::from_millis(a.delay_ms));
        }

        let descriptor_cls = class(
            c"CGVirtualDisplayDescriptor",
            &[
                sel!(setName:),
                sel!(setMaxPixelsWide:),
                sel!(setMaxPixelsHigh:),
                sel!(setSizeInMillimeters:),
                sel!(setVendorID:),
                sel!(setProductID:),
                sel!(setSerialNum:),
                sel!(setQueue:),
            ],
        )?;
        let display_cls = class(
            c"CGVirtualDisplay",
            &[
                sel!(initWithDescriptor:),
                sel!(displayID),
                sel!(applySettings:),
            ],
        )?;
        let settings_cls = class(
            c"CGVirtualDisplaySettings",
            &[sel!(setHiDPI:), sel!(setModes:)],
        )?;
        let mode_cls = class(
            c"CGVirtualDisplayMode",
            &[sel!(initWithWidth:height:refreshRate:)],
        )?;

        // A believable physical size: 96 dpi, so the OS's idea of the
        // screen's density is that of an ordinary monitor or projector.
        let mm = |px: u32| px as f64 * 25.4 / 96.0;

        // SAFETY: every class and selector was checked above; argument types
        // follow the classes' declared properties (unsigned int ids and
        // sizes, CGSize, dispatch_queue_t, NSString, NSArray).
        let display_id = unsafe {
            let descriptor: Option<Retained<AnyObject>> = msg_send![descriptor_cls, new];
            let descriptor = descriptor.ok_or("could not create a display descriptor")?;
            let name = NSString::from_str(&a.name);
            let _: () = msg_send![&*descriptor, setName: &*name];
            let _: () = msg_send![&*descriptor, setMaxPixelsWide: a.width];
            let _: () = msg_send![&*descriptor, setMaxPixelsHigh: a.height];
            let _: () = msg_send![&*descriptor, setSizeInMillimeters: NSSize::new(mm(a.width), mm(a.height))];
            let _: () = msg_send![&*descriptor, setVendorID: a.vendor];
            let _: () = msg_send![&*descriptor, setProductID: a.product];
            let _: () = msg_send![&*descriptor, setSerialNum: a.serial];
            let main_queue: *const AnyObject = &raw const _dispatch_main_q;
            let _: () = msg_send![&*descriptor, setQueue: main_queue];

            let allocated: Allocated<AnyObject> = msg_send![display_cls, alloc];
            let display: Option<Retained<AnyObject>> =
                msg_send![allocated, initWithDescriptor: &*descriptor];
            let display = display.ok_or("the window server refused the virtual display")?;

            let mode_alloc: Allocated<AnyObject> = msg_send![mode_cls, alloc];
            let mode: Option<Retained<AnyObject>> = msg_send![
                mode_alloc,
                initWithWidth: a.width as usize,
                height: a.height as usize,
                refreshRate: a.refresh
            ];
            let mode = mode.ok_or("could not create the display mode")?;
            let modes = NSArray::from_retained_slice(&[mode]);

            let settings: Option<Retained<AnyObject>> = msg_send![settings_cls, new];
            let settings = settings.ok_or("could not create display settings")?;
            let _: () = msg_send![&*settings, setHiDPI: u32::from(a.hidpi)];
            let _: () = msg_send![&*settings, setModes: &*modes];
            let applied: Bool = msg_send![&*display, applySettings: &*settings];
            if !applied.as_bool() {
                return Err("the window server refused the display mode".into());
            }

            let id: u32 = msg_send![&*display, displayID];
            // The display lives as long as this process. Leak the handle so
            // nothing releases it before then.
            std::mem::forget(display);
            id
        };

        println!("ready {display_id}");
        std::io::stdout().flush().map_err(|e| e.to_string())?;

        // Exit when the lab closes our stdin, which it also does by dying.
        std::thread::spawn(|| {
            let mut sink = [0u8; 64];
            let mut stdin = std::io::stdin();
            while matches!(stdin.read(&mut sink), Ok(n) if n > 0) {}
            std::process::exit(0);
        });

        // The descriptor's queue is the main queue; run it forever.
        // SAFETY: called once, from the main thread.
        unsafe { dispatch_main() }
    }
}
