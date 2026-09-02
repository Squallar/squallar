#![warn(clippy::all)]
// `deny`, not `forbid`: the entry symbols carry scoped `allow`s, which `forbid`
// cannot be overridden by. Everything else in the crate still errors.
#![deny(unsafe_code)]

//! The squallar app, one crate: desktop, Android and iOS entry points, the
//! event loop bootstrap and the concrete [`platform::PlatformBridge`]
//! implementations. The portable application lives in `squallar-app`.

// The `jni-typecheck` feature compiles the android modules (except
// `android::entry`) for the host, so the JNI bodies get type-checked without an
// NDK. See the feature's comment in Cargo.toml.
#[cfg(any(target_os = "android", feature = "jni-typecheck"))]
// Under that feature there is no `android_main`, so every helper it is the sole
// caller of looks dead.
#[cfg_attr(not(target_os = "android"), allow(dead_code))]
pub mod android;
/// The RAM and thread readers behind `PlatformBridge::host_signals`, one
/// module per OS.
pub mod capacity;
pub mod kv;
/// Test-only. See the module docs for why it lives in this crate.
pub mod network_security_config;
/// Test-only. Mounted here because this package's executable is the one the
/// desktop rows of `build.yaml` upload.
pub mod release_artifact_features;
// This crate turns squallar-location's `os-providers` feature on for exactly the
// targets that mount a bridge with one; Android reaches its own location
// service over JNI from the `android` module and never enables it.
pub mod platform;
pub mod run;

pub use crate::run::run;

/// iOS entry point. `packaging/ios/Sources/main.m` calls this symbol out of the
/// `staticlib`; it hands off to the shared winit loop, whose UIKit backend calls
/// `UIApplicationMain` and never returns.
#[cfg(target_os = "ios")]
#[unsafe(no_mangle)]
#[allow(unsafe_code, reason = "the C ABI symbol ObjC main() links against")]
pub extern "C" fn squallar_ios_main() -> core::ffi::c_int {
    match pollster::block_on(run()) {
        Ok(()) => 0,
        Err(e) => {
            log::error!("squallar exited with an error: {e}");
            1
        }
    }
}
