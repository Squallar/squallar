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
    /// It carries no accuracy of its own; see [`GpsFix::accuracy_m`], which is
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
#[derive(Debug, Clone, Default)]
pub struct GpsFix {
    /// Latitude in decimal degrees (positive = North).
    pub latitude: f64,
    /// Longitude in decimal degrees (positive = East).
    pub longitude: f64,
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

impl GpsFix {
    pub fn from_lat_lon(latitude: f64, longitude: f64) -> Self {
        Self {
            latitude,
            longitude,
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
            latitude,
            longitude,
            fix_quality: FixQuality::Device,
            ..Default::default()
        }
    }
}

impl From<&GpsFix> for (f64, f64) {
    fn from(fix: &GpsFix) -> (f64, f64) {
        (fix.latitude, fix.longitude)
    }
}
