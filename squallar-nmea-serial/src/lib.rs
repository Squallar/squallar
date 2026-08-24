//! NMEA parsing and the serial-port transport, in this crate's own parsed
//! vocabulary — a parser that does not know the app's fix model.
//!
//! The translation from [`ParsedFix`] to the app's fix type lives ABOVE this
//! crate, in `squallar_location`'s `serial` module.

mod config;
// Ungated: the parser needs no serial port — only the transport below does.
mod nmea_parser;

#[cfg(feature = "serial")]
mod serial;

pub use config::SerialConfig;
pub use nmea_parser::{NmeaState, ParsedFix, ParsedQuality};

#[cfg(feature = "serial")]
pub use serial::{GpsPortInfo, SerialGpsReader, detect_gps_ports};
