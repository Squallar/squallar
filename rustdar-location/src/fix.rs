use serde::{Deserialize, Serialize};

/// Quality of the GPS fix, derived from NMEA GGA fix quality indicator.
///
/// Every variant but [`Device`](Self::Device) is a GGA quality code. `Device`
/// is the one that has no NMEA number because no NMEA receiver can produce it.
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
    /// and declined to say which won.
    ///
    /// Windows `Geolocator`, macOS/iOS `CLLocationManager`, Linux's location portal
    /// and
    /// Android's fused provider all answer this way. Neither existing variant
    /// fits: `Gps` claims a satellite fix, which is a lie the moment the
    /// position came from an IP lookup, and `Estimated` means *dead reckoning*
    /// in NMEA — a receiver extrapolating from its last real fix — which says
    /// something quite different about how much to trust the coordinates.
    ///
    /// It carries no accuracy of its own; see [`Fix::accuracy_m`], which is
    /// the field this variant exists alongside.
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
    /// # Why this is not "does it carry coordinates"
    ///
    /// Those are different questions, and conflating them is a real bug rather
    /// than a stylistic one. [`Manual`](Self::Manual) is GGA quality 7 — a
    /// position somebody typed into the receiver — and
    /// [`Simulation`](Self::Simulation) is quality 8, a receiver replaying a
    /// canned track. Both carry perfectly well-formed coordinates, both are
    /// live on the serial path this crate reads, and neither says anything at
    /// all about where the user is. A predicate named for the coordinates would
    /// admit both, and the first person to reuse it for the site upgrade would
    /// hand a GPS *simulator* the ability to silently relocate the map.
    ///
    /// [`None`](Self::None) is excluded for the ordinary reason: the fix flag
    /// is clear, so whatever latitude and longitude came with it are stale or
    /// meaningless.
    ///
    /// Everything else is admitted, including [`Estimated`](Self::Estimated):
    /// dead reckoning from a real fix is still a statement about where the
    /// receiver is, and Android has been emitting it for every non-satellite
    /// provider since long before this predicate existed.
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
    /// Where the receiver says it is: latitude positive = North, longitude
    /// positive = East, both in decimal degrees.
    pub point: rustdar_geo::GeoPoint,
    /// Altitude above mean sea level in meters (from GGA).
    pub altitude_m: Option<f64>,
    /// Ground speed in meters per second (from RMC/VTG).
    pub speed_mps: Option<f64>,
    /// True course heading in degrees (0–360, from RMC/VTG). Only valid when moving.
    pub heading_deg: Option<f64>,
    /// Number of satellites in use (from GGA).
    pub satellites: Option<u8>,
    /// Fix quality indicator (from GGA).
    pub fix_quality: FixQuality,
    /// Horizontal dilution of precision (from GSA).
    pub hdop: Option<f32>,
    /// Radius of the 68% horizontal confidence circle, in metres.
    ///
    /// Reported by every platform location service and by none of the NMEA
    /// sentences, which give [`hdop`](Self::hdop) — a dimensionless geometry
    /// factor — instead. `None` therefore means "this source does not say", not
    /// "perfect", and every consumer must treat it as passing rather than
    /// failing: the serial path has always been trusted and reports nothing
    /// here.
    ///
    /// The one consumer today is the provisional-site upgrade, which uses it
    /// only to reject the absurd. See `App::upgrade_provisional_site` for why
    /// the threshold there is set so loosely.
    pub accuracy_m: Option<f64>,
    /// UTC timestamp from the GPS receiver.
    pub timestamp: Option<chrono::NaiveDateTime>,
}

// Written out because `rustdar_geo::GeoPoint` does not derive `Default` — a
// (0, 0) point is a place, not an absence, and the geo floor has no business
// pretending otherwise. Here it is only the base the two constructors
// overwrite before anyone reads it.
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
    /// unnamed.
    ///
    /// Separate from [`from_lat_lon`](Self::from_lat_lon) rather than a
    /// parameter on it: that one is the browser's and the tests' constructor
    /// and its `Gps` quality is load-bearing for both, so widening it would
    /// have meant touching every existing call site to say "still `Gps`".
    ///
    /// Accuracy is left `None` for the caller to fill in — the OS providers all
    /// report one, and none of them report it the same way.
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
/// **"Serial with a positional quality wins", not "serial wins".** The plain
/// rule looks obviously right — a dongle with a sky view beats an IP lookup by
/// three orders of magnitude — and it has a failure mode that is silent and
/// permanent: a receiver with *no* sky view goes on emitting GGA at quality 0,
/// with the last coordinates it had and a cleared fix flag, at 1 Hz forever. A
/// user with a USB GPS in a drawer and a working platform location service
/// would have the good fix discarded on every single frame in favour of a fix
/// the receiver itself is saying not to trust.
///
/// So the serial reader wins only while it is actually reporting a fix. When it
/// is not, and the OS is, the OS's fix is what there is. When neither is
/// positional the serial one is still preferred: it carries satellite counts
/// and HDOP the OS never reports, and preserving it is what keeps today's
/// behaviour unchanged on a machine with no OS provider at all.
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
/// **Deliberately enormous, and the number is measured rather than guessed.**
/// The instinct is to demand a tight fix here, and it is exactly backwards: the
/// thing this replaces is the IANA timezone guess, whose population-weighted
/// mean error is **605 km** and which opens 61% of sampled US metro population
/// on a radar that physically cannot see their weather. A portal IP lookup —
/// the coarsest source rustdar will ever read — measures **25 km**, and
/// displacing every sample point by that much changed the chosen site in only
/// **5.5%** of probes, by a median of 17 km. WSR-88D sites sit ~200 km apart;
/// this job simply does not need precision.
///
/// So the gate exists to reject the absurd, not to hold a standard. 150 km is
/// roughly where a fix stops beating the hint it would replace. Set it tight
/// and the single largest win in the feature is silently switched off.
pub const MAX_RELOCATION_ACCURACY_M: f64 = 150_000.0;

/// Whether a fix reporting this accuracy may choose the opening site.
///
/// `None` passes. Every NMEA source reports no accuracy at all — the sentences
/// carry HDOP, a dimensionless geometry factor, and no way to turn it into
/// metres — and the serial path has been trusted since before this field
/// existed. Treating absence as failure would disable the serial dongle's own
/// upgrade, which is the one source here that is *more* accurate than the
/// threshold, not less.
pub fn fix_is_accurate_enough_to_relocate(accuracy_m: Option<f64>) -> bool {
    // `is_none_or`, so a NaN accuracy — which no producer should emit and which
    // compares false against everything — is rejected rather than admitted.
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

    /// The regression the qualifier exists for: a dongle indoors emits quality
    /// 0 with real-looking coordinates at 1 Hz forever, and plain "serial wins"
    /// discards a good OS fix on every frame in favour of it.
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

    /// With nothing else on offer the serial reading still stands, quality and
    /// all. This is today's behaviour on a machine with no OS provider, and it
    /// must not change.
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
    /// The measured portal number, pinned. It is an order of magnitude coarser
    /// than a satellite fix and an order of magnitude better than it needs to
    /// be: displacing a sample point by 25 km changed the chosen site in 5.5%
    /// of probes. A threshold that rejected it would switch off the largest
    /// single improvement this feature has.
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
