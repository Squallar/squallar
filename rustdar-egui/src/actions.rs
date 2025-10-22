use chrono::NaiveDateTime;
use rustdar_radar::sites::RadarSite;
use std::collections::HashMap;

/// Radar product types
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RadarProduct {
    Reflectivity,
    Velocity,
    SpectrumWidth,
    DifferentialPhase,
    CorrelationCoefficient,
    DifferentialReflectivity,
    ClutterFilterPower,
}

impl RadarProduct {
    pub fn code(&self) -> &'static str {
        match self {
            RadarProduct::Reflectivity => "ref",
            RadarProduct::Velocity => "vel",
            RadarProduct::SpectrumWidth => "sw",
            RadarProduct::DifferentialPhase => "phi",
            RadarProduct::CorrelationCoefficient => "rho",
            RadarProduct::DifferentialReflectivity => "zdr",
            RadarProduct::ClutterFilterPower => "cfp",
        }
    }

    pub fn name(&self) -> &'static str {
        match self {
            RadarProduct::Reflectivity => "Reflectivity",
            RadarProduct::Velocity => "Velocity",
            RadarProduct::SpectrumWidth => "Spectrum Width",
            RadarProduct::DifferentialPhase => "Differential Phase",
            RadarProduct::CorrelationCoefficient => "Correlation Coefficient",
            RadarProduct::DifferentialReflectivity => "Differential Reflectivity",
            RadarProduct::ClutterFilterPower => "Clutter Filter Power",
        }
    }

    pub fn all() -> &'static [RadarProduct] {
        &[
            RadarProduct::Reflectivity,
            RadarProduct::Velocity,
            RadarProduct::SpectrumWidth,
            RadarProduct::DifferentialPhase,
            RadarProduct::CorrelationCoefficient,
            RadarProduct::DifferentialReflectivity,
            RadarProduct::ClutterFilterPower,
        ]
    }
}

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
    pub site: RadarSite,
    /// The actual timestamp of the scan data
    pub timestamp: NaiveDateTime,
    /// Available products in this scan
    pub available_products: Vec<RadarProduct>,
    /// Map of product to available elevation angles (sorted)
    pub product_elevations: HashMap<RadarProduct, Vec<f32>>,
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
    CheckForNewScans(RadarConfig),
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
            GuiAction::CheckForNewScans(config) => write!(
                f,
                "Check for new scans from {} at {}",
                config.site, config.timestamp
            ),
            GuiAction::SetScanInfo(info) => {
                write!(f, "Set scan info: {} at {}", info.site.name, info.timestamp)
            }
        }
    }
}
