mod config;
// Gated with the reader it feeds: otherwise it is dead code, and a warning, on
// every build that turns `serial` off (wasm, iOS).
#[cfg(feature = "serial")]
mod nmea_parser;
mod types;

#[cfg(feature = "serial")]
mod serial;

pub use config::{GpsConfig, HeadingSource};
pub use types::{FixQuality, GpsFix};

#[cfg(feature = "serial")]
pub use serial::{detect_gps_ports, GpsPortInfo, SerialGpsReader};
