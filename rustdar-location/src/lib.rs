//! The location domain's common vocabulary — the fix model, the OS
//! permission, heading choice; providers (nmea-serial, the OS bridges, the
//! browser) depend on it, it depends on no provider.

mod bridge;
mod fix;
mod gate;
mod heading;
mod hint;
mod permission;

pub use bridge::LocationBridge;
pub use fix::{
    Fix, FixQuality, MAX_RELOCATION_ACCURACY_M, fix_is_accurate_enough_to_relocate, prefer_fix,
};
pub use gate::{LOCATION_MEMO_KEY, LocationGate, LocationStep};
pub use heading::HeadingSource;
pub use hint::{ZONE_ANCHORS, ZoneAnchor, coordinate_for_timezone};
pub use permission::{LocationPermission, decode_permission, encode_permission};
