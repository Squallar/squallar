use serde::{Deserialize, Serialize};

/// Quality of the GPS fix, derived from NMEA GGA fix quality indicator.
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
        }
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
}

impl From<&GpsFix> for (f64, f64) {
    fn from(fix: &GpsFix) -> (f64, f64) {
        (fix.latitude, fix.longitude)
    }
}
