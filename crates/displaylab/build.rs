//! On macOS, point the binary at the system Swift runtime.
//!
//! ScreenCaptureKit is reached through a Swift bridge, which needs
//! `libswift_Concurrency.dylib` and friends from `/usr/lib/swift`. The
//! `screencapturekit` crate sets that rpath in its own build script, but Cargo
//! only applies a dependency's link arguments to the dependency's own
//! targets, so every binary that links it has to set it again.
fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        println!("cargo:rustc-link-arg=-Wl,-rpath,/usr/lib/swift");
    }
}
