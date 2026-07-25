mod config;
// NMEA sentence parsing exists solely to feed the serial reader, so it shares
// that feature's gate. Without this it is dead code on every target that turns
// `serial` off -- wasm and iOS -- and warns on every build there.
#[cfg(feature = "serial")]
mod nmea_parser;
mod types;

#[cfg(feature = "serial")]
mod serial;

pub use config::{GpsConfig, HeadingSource};
pub use types::{FixQuality, GpsFix};

#[cfg(feature = "serial")]
pub use serial::{detect_gps_ports, GpsPortInfo, SerialGpsReader};
