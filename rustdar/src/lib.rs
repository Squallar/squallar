#![warn(clippy::all)]
// `deny`, not `forbid`: the entry symbols carry scoped `allow`s, which `forbid`
// cannot be overridden by -- `rustdar_ios_main` below and, since the android
// fold (what the old note here anticipated as "the GPS and theme bridges will
// need the same"), the android module's two `#[unsafe(no_mangle)]` symbols and
// `android_main`'s raw JavaVM/Activity pointer wraps, each with its reason at
// the site. Everything else in the crate still errors.
#![deny(unsafe_code)]

//! The rustdar app, one crate: desktop, Android and iOS entry points, the
//! event loop bootstrap and the concrete [`platform::PlatformBridge`]
//! implementations. The portable application lives in `rustdar-frontend`.

// The `jni-typecheck` feature compiles the android modules (except
// `android::entry`) for the host, so the JNI bodies get type-checked without
// an NDK. See the feature's comment in Cargo.toml.
#[cfg(any(target_os = "android", feature = "jni-typecheck"))]
// Under that feature there is no `android_main`, so every helper it is the
// sole caller of looks dead. They are not; they just have no host entry point.
#[cfg_attr(not(target_os = "android"), allow(dead_code))]
pub mod android;
pub mod kv;
/// Test-only. See the module docs for why it lives in this crate.
pub mod network_security_config;
// The OS location providers moved to `rustdar_location::os_location` at
// WO-RL-3 (seam ruling 6: every remote location arm lives in the facade).
// This crate turns the facade's `os-providers` feature on for exactly the
// targets that mount a bridge with one -- `platform::DesktopPlatform` or
// `platform::IosPlatform`; Android reaches its own location service over JNI
// from the `android` module and never enables it.
pub mod platform;
pub mod run;

pub use crate::run::run;

/// iOS entry point. `packaging/ios/Sources/main.m` calls this symbol out of the
/// `staticlib`; it hands off to the shared winit loop, whose UIKit backend calls
/// `UIApplicationMain` and never returns. The `int` is therefore unreachable in
/// practice and exists only to satisfy C's `main`.
#[cfg(target_os = "ios")]
#[unsafe(no_mangle)]
#[allow(unsafe_code, reason = "the C ABI symbol ObjC main() links against")]
pub extern "C" fn rustdar_ios_main() -> core::ffi::c_int {
    match pollster::block_on(run()) {
        Ok(()) => 0,
        Err(e) => {
            log::error!("rustdar exited with an error: {e}");
            1
        }
    }
}
