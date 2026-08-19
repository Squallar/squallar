//! The location facade: everything between "where am I" and the operating
//! system (user-ruled, seam ruling 6 — WO-RL-3/RL-4).
//!
//! The app holds ONE value, [`LocationFacade`] — the permission gate composed
//! with whichever provider arm the platform shell constructed — and feeds its
//! frame inputs from it. Default features give the vocabulary and the facade
//! machinery at exactly the lean dependency set the charter test pins; every
//! ARM is fenced behind a feature, each pulling only its own target-gated
//! dependencies:
//!
//! - `os-providers` — the desktop/mobile OS arms (`os_location`): the Linux
//!   location portal (`ashpd`), Windows `Geolocator`/`AppCapability`
//!   (`windows`), Apple `CLLocationManager` (`objc2` family).
//! - `serial` — the NMEA serial reader (`serial`), wrapping
//!   rustdar-nmea-serial (which parses in its own vocabulary and does not know
//!   [`Fix`]; the translation lives here). Rides inside the desktop arm.
//! - `android-provider` — the JNI arm (`android`): permission tri-state,
//!   LocationHelper polling, initialised once via `android::init`.
//! - `web-provider` — the browser arm (`web`): Geolocation + Permissions
//!   APIs (the `web-sys`/`js-sys` family).
//!
//! Consumers that only speak the vocabulary (rustdar-egui, rustdar-app)
//! declare `default-features = false` and no features; the `rustdar` shell
//! and rustdar-web turn their target's arm on and hand the facade to the app.

/// The Android JNI arm, feature-fenced (+ host-typecheckable via
/// `jni-typecheck`, mirroring the shell's own android module story).
#[cfg(any(
    all(target_os = "android", feature = "android-provider"),
    feature = "jni-typecheck"
))]
#[cfg_attr(not(target_os = "android"), allow(dead_code))]
pub mod android;
pub(crate) mod bridge;
mod facade;
mod fix;
mod gate;
mod heading;
mod hint;
/// The OS location provider arms, feature-fenced. Since WO-RL-4 the shell's
/// wiring lives here too (`os_location::OsBackend`); the provider seam
/// types are crate-internal.
#[cfg(feature = "os-providers")]
pub mod os_location;
mod permission;
mod provider;
#[cfg(feature = "serial")]
pub mod serial;
/// The browser arm, feature-fenced; its decision half is plain host-testable
/// functions, its `web_sys` half exists only on wasm32 (the split the file
/// was born with in rustdar-web).
#[cfg(feature = "web-provider")]
pub mod web;

pub use facade::{KvSource, LocationFacade};
pub use fix::{
    Fix, FixQuality, MAX_RELOCATION_ACCURACY_M, fix_is_accurate_enough_to_relocate, prefer_fix,
};
pub use gate::{LOCATION_MEMO_KEY, LocationGate, LocationStep};
pub use heading::HeadingSource;
pub use hint::{ZONE_ANCHORS, ZoneAnchor, coordinate_for_timezone};
pub use permission::{LocationPermission, decode_permission, encode_permission};
pub use provider::{LocationProvider, UnavailableProvider, Wake};
