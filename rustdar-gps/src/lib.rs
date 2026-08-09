mod config;
// Gated with the reader it feeds: otherwise it is dead code, and a warning, on
// every build that turns `serial` off (wasm, iOS).
#[cfg(feature = "serial")]
mod nmea_parser;
// Ungated, unlike everything else here: a permission is what a *platform*
// location service needs, and the targets with no serial port (web, iOS,
// Android) are precisely the ones that have one.
mod permission;
mod types;

#[cfg(feature = "serial")]
mod serial;

pub use config::{GpsConfig, HeadingSource};
pub use permission::LocationPermission;
pub use types::{FixQuality, GpsFix};

#[cfg(feature = "serial")]
pub use serial::{GpsPortInfo, SerialGpsReader, detect_gps_ports};
