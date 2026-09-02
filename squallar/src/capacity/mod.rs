//! What this machine can hold, read where an API exists.
//!
//! One RAM reader per OS, selected by which module is mounted: a `cfg` here
//! picks a module and never forks a body. Every reader answers `None` rather
//! than a guess when its API does not — a signal a platform cannot read is
//! absent, and the device profile treats absent as the majority arm rather
//! than as small.

#[cfg(any(target_os = "linux", target_os = "android"))]
mod linux;
#[cfg(any(target_os = "linux", target_os = "android"))]
pub use linux::system_ram_bytes;

#[cfg(any(target_os = "macos", target_os = "ios"))]
mod apple;
#[cfg(any(target_os = "macos", target_os = "ios"))]
pub use apple::system_ram_bytes;

#[cfg(target_os = "windows")]
mod windows;
#[cfg(target_os = "windows")]
pub use windows::system_ram_bytes;

/// An OS this crate has no reader for. Unknown, not zero.
#[cfg(not(any(
    target_os = "linux",
    target_os = "android",
    target_os = "macos",
    target_os = "ios",
    target_os = "windows",
)))]
mod unknown {
    pub fn system_ram_bytes() -> Option<u64> {
        None
    }
}
#[cfg(not(any(
    target_os = "linux",
    target_os = "android",
    target_os = "macos",
    target_os = "ios",
    target_os = "windows",
)))]
pub use unknown::system_ram_bytes;

use squallar_app::platform::{FormFactor, HostSignals};

/// Threads the OS says this process may run at once, or `None` when it will
/// not say. The machine's own figure — not the size of any pool built on it.
pub fn parallelism() -> Option<usize> {
    std::thread::available_parallelism()
        .ok()
        .map(std::num::NonZeroUsize::get)
}

/// Every host signal a native bridge hands over. `form_factor` is the one
/// term the bridge knows and this module does not: a build fact, spelled by
/// whichever bridge was compiled. Declared RAM is a browser's notion and is
/// `None` on every native target.
pub fn host_signals(form_factor: FormFactor) -> HostSignals {
    HostSignals {
        system_ram_bytes: system_ram_bytes(),
        declared_ram_bytes: None,
        parallelism: parallelism(),
        form_factor: Some(form_factor),
    }
}
