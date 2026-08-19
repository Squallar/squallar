//! The location domain's common vocabulary — the fix model, the OS
//! permission, heading choice; providers (nmea-serial, the OS bridges, the
//! browser) depend on it, it depends on no provider.

mod fix;
mod heading;
mod permission;

pub use fix::{Fix, FixQuality, prefer_fix};
pub use heading::HeadingSource;
pub use permission::{LocationPermission, decode_permission, encode_permission};
