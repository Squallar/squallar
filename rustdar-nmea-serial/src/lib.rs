//! NMEA parsing and the serial-port transport, in this crate's own parsed
//! vocabulary — a parser that does not know the app's fix model.
//!
//! The translation from [`ParsedFix`] to the app's fix type lives ABOVE this
//! crate, in `rustdar_location`'s `serial` module (WO-RL-3 flipped the edge:
//! the facade depends on this provider, never the reverse).

mod config;
// Ungated: the parser needs no serial port — only the transport below does —
// and since WO-RL-3 it is public API in its own right.
mod nmea_parser;

#[cfg(feature = "serial")]
mod serial;

pub use config::SerialConfig;
pub use nmea_parser::{NmeaState, ParsedFix, ParsedQuality};

#[cfg(feature = "serial")]
pub use serial::{GpsPortInfo, SerialGpsReader, detect_gps_ports};
