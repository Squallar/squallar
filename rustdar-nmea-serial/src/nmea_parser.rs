use crate::config::MIN_SPEED_FOR_BEARING_MPS;
use nmea::sentences::FixType;

/// The GGA fix-quality indicator, in this crate's own vocabulary.
///
/// Exactly the nine codes NMEA can put on the wire — nothing else, because a
/// parser can only report what a sentence said. What each one means to the
/// *app* (which of them may relocate a map, which are noise) is the fix
/// model's business, and the fix model lives above this crate:
/// `rustdar_location`'s `serial` module owns that translation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParsedQuality {
    None,
    Gps,
    Dgps,
    Pps,
    Rtk,
    FloatRtk,
    Estimated,
    Manual,
    Simulation,
}

/// One position-bearing sentence's accumulated result, in this crate's own
/// vocabulary.
///
/// Deliberately not the app's fix type: this crate parses NMEA and knows
/// nothing about what an application does with a position. There is no
/// accuracy field because NMEA has none — GGA and GSA give
/// [`hdop`](Self::hdop), a dimensionless geometry factor, and turning that
/// into metres needs the receiver's UERE, which it does not report.
#[derive(Debug, Clone)]
pub struct ParsedFix {
    /// Latitude in decimal degrees, positive = North.
    pub lat: f64,
    /// Longitude in decimal degrees, positive = East.
    pub lon: f64,
    /// Altitude above mean sea level in meters (from GGA).
    pub altitude_m: Option<f64>,
    /// Ground speed in meters per second (from RMC/VTG).
    pub speed_mps: Option<f64>,
    /// True course heading in degrees (0–360, from RMC/VTG). Suppressed below
    /// `MIN_SPEED_FOR_BEARING_MPS` — course-over-ground from a near-stationary
    /// receiver is noise. That is a statement about NMEA receivers, so the
    /// parser is where it is decided.
    pub heading_deg: Option<f64>,
    /// Number of satellites in use (from GGA).
    pub satellites: Option<u8>,
    /// Fix quality indicator (from GGA).
    pub quality: ParsedQuality,
    /// Horizontal dilution of precision (from GSA).
    pub hdop: Option<f32>,
    /// UTC timestamp from the receiver.
    pub timestamp: Option<chrono::NaiveDateTime>,
}

/// A fix is spread across several sentence types (GGA, RMC, GSA, VTG), so
/// fields accumulate here until a position-bearing one completes.
pub struct NmeaState {
    parser: nmea::Nmea,
}

impl Default for NmeaState {
    fn default() -> Self {
        Self::new()
    }
}

impl NmeaState {
    pub fn new() -> Self {
        Self {
            parser: nmea::Nmea::default(),
        }
    }

    /// `sentence` must keep its `$` prefix and `*XX` checksum.
    pub fn feed_sentence(&mut self, sentence: &str) -> Option<ParsedFix> {
        if self.parser.parse(sentence).is_err() {
            return None;
        }

        let lat = self.parser.latitude?;
        let lon = self.parser.longitude?;

        let quality = match self.parser.fix_type {
            Some(FixType::Invalid) | None => ParsedQuality::None,
            Some(FixType::Gps) => ParsedQuality::Gps,
            Some(FixType::DGps) => ParsedQuality::Dgps,
            Some(FixType::Pps) => ParsedQuality::Pps,
            Some(FixType::Rtk) => ParsedQuality::Rtk,
            Some(FixType::FloatRtk) => ParsedQuality::FloatRtk,
            Some(FixType::Estimated) => ParsedQuality::Estimated,
            Some(FixType::Manual) => ParsedQuality::Manual,
            Some(FixType::Simulation) => ParsedQuality::Simulation,
        };

        let altitude_m = self.parser.altitude.map(|a| a as f64);
        let speed_mps = self
            .parser
            .speed_over_ground
            .map(|knots| knots as f64 * 0.514444);
        let heading_deg = self
            .parser
            .true_course
            .map(|c| c as f64)
            .filter(|_| speed_mps.is_some_and(|s| s > MIN_SPEED_FOR_BEARING_MPS));
        let satellites = self.parser.num_of_fix_satellites.map(|n| n as u8);
        let hdop = self.parser.hdop;

        let timestamp = self.parser.fix_time.map(|t| {
            let date = self
                .parser
                .fix_date
                .unwrap_or_else(|| chrono::Utc::now().date_naive());
            date.and_time(t)
        });

        Some(ParsedFix {
            lat,
            lon,
            altitude_m,
            speed_mps,
            heading_deg,
            satellites,
            quality,
            hdop,
            timestamp,
        })
    }
}
