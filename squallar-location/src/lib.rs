//! The location facade: everything between "where am I" and the operating
//! system. The app holds ONE value, [`LocationFacade`].
//!
//! Default features give the vocabulary and the facade machinery; every arm is
//! fenced behind a feature, each pulling only its own target-gated dependencies:
//!
//! - `os-providers` — the desktop/mobile OS arms (`os_location`): the Linux
//!   location portal (`ashpd`), Windows `Geolocator`/`AppCapability`, Apple
//!   `CLLocationManager`.
//! - `serial` — the NMEA serial reader, wrapping squallar-nmea-serial. Rides
//!   inside the desktop arm.
//! - `android-provider` — the JNI arm (`android`).
//! - `web-provider` — the browser arm (`web`).

/// The Android JNI arm, feature-fenced (+ host-typecheckable via
/// `jni-typecheck`).
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
/// The OS location provider arms, feature-fenced.
#[cfg(feature = "os-providers")]
pub mod os_location;
mod permission;
mod provider;
#[cfg(feature = "serial")]
pub mod serial;
/// The browser arm, feature-fenced; its `web_sys` half exists only on wasm32.
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
