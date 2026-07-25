use serde::{Deserialize, Serialize};

/// Minimum ground speed (m/s) for GPS bearing to be considered meaningful.
/// Below this, heading data is too noisy from a near-stationary receiver.
pub(crate) const MIN_SPEED_FOR_BEARING_MPS: f64 = 0.5;

/// Ground speed (m/s) above which the device is considered "moving" (~5 mph).
/// Used by [`HeadingSource::Auto`] to switch from compass to GPS bearing.
pub(crate) const MOVING_SPEED_THRESHOLD_MPS: f64 = 2.2;

/// How the effective heading (for the directional wedge) is determined
/// when both compass and GPS bearing data are available.
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

    /// Compute the effective heading from compass and GPS bearing inputs.
    ///
    /// - `compass_heading`: degrees from device compass sensor (0–360).
    /// - `gps_bearing`: degrees course-over-ground from GPS (0–360).
    /// - `speed_mps`: current ground speed in m/s from GPS.
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

/// Configuration for GPS serial port connection.
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
