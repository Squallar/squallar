use chrono::NaiveDateTime;

/// Configuration for radar site and time selection
#[derive(Debug, Clone)]
pub struct RadarConfig {
    /// The radar site code (e.g., "KTLX" for Oklahoma City)
    pub site: String,
    /// The timestamp for the radar scan (defaults to current time)
    pub timestamp: NaiveDateTime,
}

/// Information about a loaded radar scan
#[derive(Debug, Clone)]
pub struct ScanInfo {
    /// The radar site code
    pub site: String,
    /// The actual timestamp of the scan data
    pub timestamp: NaiveDateTime,
    /// Number of elevation angles in the scan
    pub num_elevations: usize,
    /// Status message
    pub status: String,
}

impl Default for RadarConfig {
    fn default() -> Self {
        // Get current local time as naive datetime (without timezone info)
        // This represents the local wall clock time
        let now = chrono::Local::now();
        let timestamp = now.naive_local();

        Self {
            site: "KTLX".to_string(),
            timestamp,
        }
    }
}

pub enum GuiAction {
    Exit,
    FetchRadarScan(RadarConfig),
    SetScanInfo(ScanInfo),
}

impl std::fmt::Display for GuiAction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GuiAction::Exit => write!(f, "Exit application"),
            GuiAction::FetchRadarScan(config) => write!(
                f,
                "Fetch radar scan from {} at {}",
                config.site, config.timestamp
            ),
            GuiAction::SetScanInfo(info) => {
                write!(f, "Set scan info: {} at {}", info.site, info.timestamp)
            }
        }
    }
}
