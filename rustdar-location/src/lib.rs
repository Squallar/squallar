//! The location facade: everything between "where am I" and the operating
//! system (user-ruled, seam ruling 6 — WO-RL-3/RL-4).
//!
//! Default features give the domain vocabulary only — the fix model, the OS
//! permission, heading choice, the permission gate, the timezone anchors —
//! and cost exactly the lean dependency set the charter test pins. The
//! providers are fenced behind features, each pulling only its own
//! target-gated dependencies:
//!
//! - `os-providers` — the desktop/mobile OS arms ([`os_location`]): the Linux
//!   location portal (`ashpd`), Windows `Geolocator`/`AppCapability`
//!   (`windows`), Apple `CLLocationManager` (`objc2` family).
//! - `serial` — the NMEA serial provider ([`serial`]), wrapping
//!   rustdar-nmea-serial (which parses in its own vocabulary and does not know
//!   [`Fix`]; the translation lives here).
//!
//! Consumers that only speak the vocabulary (rustdar-egui, rustdar-frontend,
//! rustdar-web) declare `default-features = false` and no features; the
//! `rustdar` shell turns the provider arms on per target.

mod bridge;
mod fix;
mod gate;
mod heading;
mod hint;
/// The OS location provider arms, feature-fenced. The seam types are `pub`
/// only because the shell's `platform.rs` wiring still drives them; WO-RL-4
/// collapses that wiring into this crate.
#[cfg(feature = "os-providers")]
pub mod os_location;
mod permission;
#[cfg(feature = "serial")]
pub mod serial;

pub use bridge::LocationBridge;
pub use fix::{
    Fix, FixQuality, MAX_RELOCATION_ACCURACY_M, fix_is_accurate_enough_to_relocate, prefer_fix,
};
pub use gate::{LOCATION_MEMO_KEY, LocationGate, LocationStep};
pub use heading::HeadingSource;
pub use hint::{ZONE_ANCHORS, ZoneAnchor, coordinate_for_timezone};
pub use permission::{LocationPermission, decode_permission, encode_permission};
