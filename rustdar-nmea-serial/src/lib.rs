//! NMEA parsing and the serial-port transport — one provider of
//! [`rustdar_location::Fix`].

mod config;
// Gated with the reader it feeds: otherwise it is dead code, and a warning, on
// every build that turns `serial` off (wasm, iOS).
#[cfg(feature = "serial")]
mod nmea_parser;

#[cfg(feature = "serial")]
mod serial;

pub use config::SerialConfig;

#[cfg(feature = "serial")]
pub use serial::{GpsPortInfo, SerialGpsReader, detect_gps_ports};
