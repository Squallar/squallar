use serde::{Deserialize, Serialize};

/// Below this ground speed (m/s), course-over-ground from a near-stationary
/// receiver is noise.
pub(crate) const MIN_SPEED_FOR_BEARING_MPS: f64 = 0.5;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
#[derive(Default)]
pub struct SerialConfig {
    /// Serial port path. `None` means auto-detect.
    pub port_path: Option<String>,
    /// Baud rate. 0 means auto-detect.
    pub baud_rate: u32,
}

impl SerialConfig {
    /// Whether baud rate should be auto-detected.
    pub fn auto_baud(&self) -> bool {
        self.baud_rate == 0
    }

    /// Whether the port should be auto-detected.
    pub fn auto_port(&self) -> bool {
        self.port_path.is_none()
    }
}
