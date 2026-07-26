#![warn(clippy::all)]
// `deny`, not `forbid`: `rustdar_ios_main` below is `#[unsafe(no_mangle)]` and
// carries a scoped `allow`, which `forbid` cannot be overridden by. The GPS and
// theme bridges will need the same. Everything else in the crate still errors.
#![deny(unsafe_code)]

//! Desktop, Android and iOS entry points: the event loop bootstrap and the
//! concrete [`platform::PlatformBridge`] implementations. The portable
//! application lives in `rustdar-frontend`.

pub mod config_store;
/// Test-only. See the module docs for why it lives in this crate.
pub mod network_security_config;
pub mod platform;
pub mod run;

pub use crate::run::run;

/// iOS entry point. `ios/Sources/main.m` calls this symbol out of the
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
