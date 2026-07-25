use serde::{Deserialize, Serialize};

/// Below this ground speed (m/s), course-over-ground from a near-stationary
/// receiver is noise. Gated because only `nmea_parser` reads it.
#[cfg(feature = "serial")]
pub(crate) const MIN_SPEED_FOR_BEARING_MPS: f64 = 0.5;

/// Ground speed (m/s) above which the device counts as "moving" (~5 mph).
pub(crate) const MOVING_SPEED_THRESHOLD_MPS: f64 = 2.2;

/// How the directional wedge picks a heading when both compass and GPS bearing
/// are available.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum HeadingSource {
    /// Use GPS bearing when moving (>~5 mph), compass when stationary.
    #[default]
    Auto,
    /// Use the device compass sensor exclusively.
    CompassOnly,
    /// Use GPS course-over-ground bearing exclusively.
    GpsOnly,
}

impl HeadingSource {
    pub const ALL: &[HeadingSource] = &[
        HeadingSource::Auto,
        HeadingSource::CompassOnly,
        HeadingSource::GpsOnly,
    ];

    pub fn label(self) -> &'static str {
        match self {
            HeadingSource::Auto => "Auto",
            HeadingSource::CompassOnly => "Compass only",
            HeadingSource::GpsOnly => "GPS only",
        }
    }

    /// Headings in degrees (0–360), speed in m/s.
    pub fn effective_heading(
        self,
        compass_heading: Option<f32>,
        gps_bearing: Option<f64>,
        speed_mps: Option<f64>,
    ) -> Option<f32> {
        match self {
            HeadingSource::Auto => {
                let moving = speed_mps.is_some_and(|s| s > MOVING_SPEED_THRESHOLD_MPS);
                if moving
                    && let Some(bearing) = gps_bearing {
                        return Some(bearing as f32);
                    }
                compass_heading
            }
            HeadingSource::CompassOnly => compass_heading,
            HeadingSource::GpsOnly => gps_bearing.map(|b| b as f32),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
#[derive(Default)]
pub struct GpsConfig {
    /// Serial port path. `None` means auto-detect.
    pub port_path: Option<String>,
    /// Baud rate. 0 means auto-detect.
    pub baud_rate: u32,
    /// How the directional heading is determined.
    pub heading_source: HeadingSource,
}


impl GpsConfig {
    /// Whether baud rate should be auto-detected.
    pub fn auto_baud(&self) -> bool {
        self.baud_rate == 0
    }

    /// Whether the port should be auto-detected.
    pub fn auto_port(&self) -> bool {
        self.port_path.is_none()
    }
}
