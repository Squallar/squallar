//! The seam between the platform bridges and whatever location service the OS
//! offers. Each target gets a private module here exposing one type,
//! `OsLocationReader`, implementing one trait, [`OsLocationProvider`].
//!
//! **This module is the entire `cfg` surface.** Landing a provider touches this
//! file, the provider's own file, and that target's `os-providers`-fenced
//! dependency block in `Cargo.toml`. Nothing else.

mod backend;
#[cfg(target_os = "linux")]
mod linux;
mod unsupported;

pub use backend::OsBackend;

/// CoreLocation, for macOS and iOS. Declared unconditionally, unlike the arm
/// that selects it: the half of that file which decodes statuses and sentinels
/// names no Objective-C type, and `cargo test` runs on Linux.
mod apple;

// The arm table. Written on `target_os` and never on `unix` or `target_family`:
// Android *is* `unix` and would take the Linux arm despite reaching a different
// API over JNI, and iOS is `unix` but not `macos` while sharing CoreLocation.
// wasm32 lands on the `not(...)` arm, where `unsupported` is the right answer:
// a browser tab has no OS location service, the page's Geolocation API being
// the *web bridge's* business.

/// `org.freedesktop.portal.Location` over `ashpd`. Never GeoClue directly.
#[cfg(target_os = "linux")]
use linux as provider;

/// `AppCapability` for the state, `Geolocator` to prompt, an MTA worker to keep
/// `RequestAccessAsync` off the frame thread. Compiled under `test` as well as
/// on Windows, because CI has no Windows runner and the pure mapping half is
/// exercised on the Linux host.
#[cfg(any(target_os = "windows", test))]
mod windows;

/// `self::`, because a bare `windows` is ambiguous with the crate of that name.
#[cfg(target_os = "windows")]
use self::windows as provider;

/// One `apple.rs` with two `#[cfg]` islands: macOS constructs its bridge after
/// `NSApplication` exists and may not be in a bundle, iOS constructs it before
/// `UIApplicationMain` has run.
#[cfg(any(target_os = "macos", target_os = "ios"))]
use apple as provider;

/// Everything else, wasm32 included.
#[cfg(not(any(
    target_os = "linux",
    target_os = "windows",
    target_os = "macos",
    target_os = "ios"
)))]
use unsupported as provider;

pub(crate) use provider::OsLocationReader;

/// Asks the event loop for a frame, so something a provider pushed while the
/// loop was parked is actually seen. `Arc<dyn …>` because two providers clone it
/// into every session thread or `StartDelivery` command they start.
pub(crate) type RedrawWake = crate::provider::Wake;

pub(crate) type ReportPermission =
    std::sync::Arc<dyn Fn(crate::LocationPermission) + Send + Sync + 'static>;

/// The three ways a provider talks back to the app, and the only three. One
/// struct because they travel together through every layer and are never used
/// apart; `Clone` because a provider that stops and starts again hands a fresh
/// copy to each session.
#[derive(Clone)]
pub(crate) struct OsLocationSink {
    /// Where fixes go; see [`crate::prefer_fix`].
    pub fixes: std::sync::mpsc::Sender<crate::Fix>,
    /// See [`RedrawWake`].
    pub wake: RedrawWake,
    /// See [`ReportPermission`] and the note on [`OsLocationProvider::start`].
    pub report: ReportPermission,
}

/// One shape for every arm of the table above.
///
/// The parameter is an [`OsLocationSink`] and not a serial config: a portal
/// session, a WinRT `Geolocator` and a `CLLocationManager` have no port name or
/// baud rate, only somewhere to put a fix, a way to ask for a frame, and a way
/// to say the permission changed.
///
/// Two phases. [`start`](Self::start) brings the provider up and **must not
/// prompt and must not deliver**; [`request`](Self::request) is the user-visible
/// act. Windows' and Apple's permission watchers must be live *before* anything
/// is asked and stay live *after* delivery stops; Linux's `Start()` can sit on
/// an agent dialog, so it cannot be part of bringing the provider up.
///
/// The permission lives in the bridge: providers **push** through
/// [`OsLocationSink::report`], because the gate answers `Denied` by calling
/// `stop_location` and state kept inside the value being stopped evaporates
/// exactly when it matters.
pub(crate) trait OsLocationProvider: Sized {
    /// Bring the provider up, prompting nobody and delivering nothing. `None`
    /// means no location service to subscribe to, which the bridge renders as
    /// [`Unavailable`](crate::LocationPermission::Unavailable).
    fn start(sink: OsLocationSink) -> Option<Self>;

    /// Prompt if the platform needs prompting, and start delivering. Nothing
    /// durable may hang off the `bool`: two of the three platforms cannot tell
    /// whether the ask reached a human.
    fn request(&mut self) -> bool;

    /// Stop delivering. Never revokes, and never tears down the permission
    /// watcher — the thing that notices a change made while delivery is off.
    fn stop(&mut self);

    /// Whether fixes are being delivered right now. Not "granted".
    fn active(&self) -> bool;

    /// Whether this platform has a location settings page worth offering. An
    /// associated function because `App::new` asks it once, before any provider
    /// has been constructed; a `&self` answer would be `false` on Windows forever.
    fn settings_available() -> bool {
        false
    }

    /// Open the system location settings. Fire and forget; must not block.
    fn open_settings(&mut self) {}
}
