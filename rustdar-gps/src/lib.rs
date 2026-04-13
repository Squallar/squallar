mod config;
mod nmea_parser;
mod types;

#[cfg(feature = "serial")]
mod serial;

pub use config::{GpsConfig, HeadingSource};
pub use types::{FixQuality, GpsFix};

#[cfg(feature = "serial")]
pub use serial::{detect_gps_ports, GpsPortInfo, SerialGpsReader};
