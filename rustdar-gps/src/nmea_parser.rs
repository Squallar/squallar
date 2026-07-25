use crate::config::MIN_SPEED_FOR_BEARING_MPS;
use crate::types::{FixQuality, GpsFix};
use nmea::sentences::FixType;

/// A fix is spread across several sentence types (GGA, RMC, GSA, VTG), so
/// fields accumulate here until a position-bearing one completes.
pub(crate) struct NmeaState {
    parser: nmea::Nmea,
}

impl NmeaState {
    pub fn new() -> Self {
        Self {
            parser: nmea::Nmea::default(),
        }
    }

    /// `sentence` must keep its `$` prefix and `*XX` checksum.
    pub fn feed_sentence(&mut self, sentence: &str) -> Option<GpsFix> {
        if self.parser.parse(sentence).is_err() {
            return None;
        }

        let lat = self.parser.latitude?;
        let lon = self.parser.longitude?;

        let fix_quality = match self.parser.fix_type {
            Some(FixType::Invalid) | None => FixQuality::None,
            Some(FixType::Gps) => FixQuality::Gps,
            Some(FixType::DGps) => FixQuality::Dgps,
            Some(FixType::Pps) => FixQuality::Pps,
            Some(FixType::Rtk) => FixQuality::Rtk,
            Some(FixType::FloatRtk) => FixQuality::FloatRtk,
            Some(FixType::Estimated) => FixQuality::Estimated,
            Some(FixType::Manual) => FixQuality::Manual,
            Some(FixType::Simulation) => FixQuality::Simulation,
        };

        let altitude_m = self.parser.altitude.map(|a| a as f64);
        let speed_mps = self.parser.speed_over_ground.map(|knots| knots as f64 * 0.514444);
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

        Some(GpsFix {
            latitude: lat,
            longitude: lon,
            altitude_m,
            speed_mps,
            heading_deg,
            satellites,
            fix_quality,
            hdop,
            timestamp,
        })
    }
}
