use serde::{Deserialize, Serialize};

/// Quality of the GPS fix, from the NMEA GGA quality indicator. Every variant
/// but [`Device`](Self::Device) is a GGA quality code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum FixQuality {
    #[default]
    None,
    Gps,
    Dgps,
    Pps,
    Rtk,
    FloatRtk,
    Estimated,
    Manual,
    Simulation,
    /// A platform location service that fused satellites, Wi-Fi and cell towers
    /// and declined to say which won. `Gps` would claim a satellite fix and
    /// `Estimated` means *dead reckoning* in NMEA, so neither fits. Carries no
    /// accuracy of its own; see [`Fix::accuracy_m`].
    Device,
}

impl FixQuality {
    pub fn label(self) -> &'static str {
        match self {
            FixQuality::None => "No fix",
            FixQuality::Gps => "GPS",
            FixQuality::Dgps => "DGPS",
            FixQuality::Pps => "PPS",
            FixQuality::Rtk => "RTK",
            FixQuality::FloatRtk => "Float RTK",
            FixQuality::Estimated => "Estimated",
            FixQuality::Manual => "Manual",
            FixQuality::Simulation => "Simulation",
            FixQuality::Device => "Device",
        }
    }

    /// Whether a fix of this quality may move the user's radar site.
    ///
    /// Not "does it carry coordinates": [`Manual`](Self::Manual) is GGA quality
    /// 7, a position somebody typed in, and [`Simulation`](Self::Simulation) is
    /// quality 8, a replayed track — both carry well-formed coordinates and say
    /// nothing about where the user is. [`Estimated`](Self::Estimated) is
    /// admitted, being dead reckoning from a real fix.
    pub fn can_relocate(self) -> bool {
        !matches!(
            self,
            FixQuality::None | FixQuality::Manual | FixQuality::Simulation
        )
    }
}

/// A GPS position fix. The `Option` fields come from different NMEA sentences
/// and depend on the receiver and fix state.
#[derive(Debug, Clone)]
pub struct Fix {
    /// Latitude positive = North, longitude positive = East, decimal degrees.
    pub point: rustdar_geo::GeoPoint,
    /// Altitude above mean sea level in meters (from GGA).
    pub altitude_m: Option<f64>,
    /// Ground speed in meters per second (from RMC/VTG).
    pub speed_mps: Option<f64>,
    /// True course heading in degrees (0–360, from RMC/VTG). Valid when moving.
    pub heading_deg: Option<f64>,
    /// Number of satellites in use (from GGA).
    pub satellites: Option<u8>,
    pub fix_quality: FixQuality,
    /// Horizontal dilution of precision (from GSA).
    pub hdop: Option<f32>,
    /// Radius of the 68% horizontal confidence circle, in metres.
    ///
    /// Reported by every platform location service and by no NMEA sentence,
    /// which give [`hdop`](Self::hdop) instead. `None` means "this source does
    /// not say", not "perfect", and every consumer must treat it as passing.
    /// The one consumer today is `App::upgrade_provisional_site`.
    pub accuracy_m: Option<f64>,
    /// UTC timestamp from the GPS receiver.
    pub timestamp: Option<chrono::NaiveDateTime>,
}

// `rustdar_geo::GeoPoint` does not derive `Default` — a (0, 0) point is a place,
// not an absence.
impl Default for Fix {
    fn default() -> Self {
        Self {
            point: rustdar_geo::GeoPoint { lat: 0.0, lon: 0.0 },
            altitude_m: None,
            speed_mps: None,
            heading_deg: None,
            satellites: None,
            fix_quality: FixQuality::default(),
            hdop: None,
            accuracy_m: None,
            timestamp: None,
        }
    }
}

impl Fix {
    pub fn from_lat_lon(latitude: f64, longitude: f64) -> Self {
        Self {
            point: rustdar_geo::GeoPoint {
                lat: latitude,
                lon: longitude,
            },
            fix_quality: FixQuality::Gps,
            ..Default::default()
        }
    }

    /// A position from a platform location service, whose source is fused and
    /// unnamed. Separate from [`from_lat_lon`](Self::from_lat_lon), whose `Gps`
    /// quality is load-bearing for the browser and the tests.
    pub fn from_device_position(latitude: f64, longitude: f64) -> Self {
        Self {
            point: rustdar_geo::GeoPoint {
                lat: latitude,
                lon: longitude,
            },
            fix_quality: FixQuality::Device,
            ..Default::default()
        }
    }
}

impl From<&Fix> for (f64, f64) {
    fn from(fix: &Fix) -> (f64, f64) {
        (fix.point.lat, fix.point.lon)
    }
}

/// Choose between a fix from the serial reader and one from the OS.
///
/// **"Serial with a positional quality wins", not "serial wins".** A receiver
/// with no sky view goes on emitting GGA at quality 0 — the last coordinates it
/// had, fix flag cleared — at 1 Hz forever, so the plain rule would discard a
/// good OS fix on every frame. When neither is positional the serial one still
/// wins: it carries satellite counts and HDOP the OS never reports.
pub fn prefer_fix(serial: Option<Fix>, os: Option<Fix>) -> Option<Fix> {
    match (serial, os) {
        (Some(serial), Some(os)) => Some(if serial.fix_quality == FixQuality::None {
            os
        } else {
            serial
        }),
        (serial, os) => serial.or(os),
    }
}

/// How coarse a fix may be and still be allowed to spend the provisional site.
///
/// **Deliberately enormous, and measured rather than guessed.** What it replaces
/// is the IANA timezone guess, whose population-weighted mean error is 605 km
/// and which opens 61% of sampled US metro population on a radar that cannot see
/// their weather. A portal IP lookup — the coarsest source rustdar reads —
/// measures 25 km, and displacing every sample point by that much changed the
/// chosen site in only 5.5% of probes. WSR-88D sites sit ~200 km apart.
pub const MAX_RELOCATION_ACCURACY_M: f64 = 150_000.0;

/// Whether a fix reporting this accuracy may choose the opening site. `None`
/// passes: every NMEA source reports no accuracy at all, and treating absence as
/// failure would disable the serial dongle's own upgrade.
pub fn fix_is_accurate_enough_to_relocate(accuracy_m: Option<f64>) -> bool {
    // `is_none_or`, so a NaN accuracy is rejected rather than admitted.
    accuracy_m.is_none_or(|m| m <= MAX_RELOCATION_ACCURACY_M)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn serial_fix(quality: FixQuality) -> Fix {
        Fix {
            fix_quality: quality,
            ..Fix::from_lat_lon(35.25, -97.5)
        }
    }

    fn os_fix() -> Fix {
        Fix {
            accuracy_m: Some(25_000.0),
            ..Fix::from_device_position(39.74, -104.99)
        }
    }

    /// A receiver that has a fix is the better source by three orders of
    /// magnitude, and it stays the better source.
    #[test]
    fn a_serial_fix_outranks_the_operating_systems() {
        let chosen = prefer_fix(Some(serial_fix(FixQuality::Gps)), Some(os_fix()))
            .expect("both were present");
        assert_eq!(chosen.fix_quality, FixQuality::Gps);
    }

    /// The regression the qualifier exists for: a dongle indoors emits quality 0
    /// with real-looking coordinates at 1 Hz forever.
    #[test]
    fn a_dongle_with_no_sky_view_does_not_shadow_a_real_fix() {
        let chosen = prefer_fix(Some(serial_fix(FixQuality::None)), Some(os_fix()))
            .expect("both were present");
        assert_eq!(
            chosen.fix_quality,
            FixQuality::Device,
            "a serial reader reporting no fix suppressed the one source that \
             had one"
        );
    }

    /// Today's behaviour on a machine with no OS provider.
    #[test]
    fn a_lone_source_is_used_whatever_it_says() {
        assert_eq!(
            prefer_fix(Some(serial_fix(FixQuality::None)), None)
                .expect("the serial reading")
                .fix_quality,
            FixQuality::None,
        );
        assert_eq!(
            prefer_fix(None, Some(os_fix()))
                .expect("the OS reading")
                .fix_quality,
            FixQuality::Device,
        );
        assert!(prefer_fix(None, None).is_none());
    }
    /// The measured portal number, pinned: displacing a sample point by 25 km
    /// changed the chosen site in 5.5% of probes.
    #[test]
    fn the_accuracy_gate_admits_a_coarse_but_usable_fix() {
        assert!(fix_is_accurate_enough_to_relocate(Some(25_000.0)));
        assert!(
            fix_is_accurate_enough_to_relocate(None),
            "the serial path reports no accuracy at all and has always been \
                 trusted"
        );
        assert!(!fix_is_accurate_enough_to_relocate(Some(1_000_000.0)));
        assert!(
            !fix_is_accurate_enough_to_relocate(Some(f64::NAN)),
            "a NaN accuracy compares false against everything, so it has to be \
                 rejected explicitly or it slips through as 'good enough'"
        );
    }
}
