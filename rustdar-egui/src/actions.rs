use rustdar_radar::types::ScanInfo;
use rustdar_overlays::spc::outlook::{OutlookDay, OutlookProduct};
use chrono::NaiveDateTime;

/// Configuration for radar site and time selection
#[derive(Debug, Clone)]
pub struct RadarConfig {
    /// The radar site code (e.g., "KTLX" for Oklahoma City)
    pub site: String,
    /// The timestamp for the radar scan (defaults to current time)
    pub timestamp: NaiveDateTime,
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
    SwitchRadarSite(String), // Switch to a different radar site
    /// Fetch SPC outlook product(s) for the given day.
    FetchSpcOutlook {
        day: OutlookDay,
        products: Vec<OutlookProduct>,
    },
    /// Re-fetch all currently enabled SPC outlook layers.
    RefreshSpcOutlooks,
    /// Fetch all active NWS weather alerts.
    FetchNwsAlerts,
    /// Re-fetch NWS alerts (manual refresh).
    RefreshNwsAlerts,
    /// Fetch all active SPC Mesoscale Discussions.
    FetchSpcDiscussions,
    /// Re-fetch SPC Mesoscale Discussions (manual refresh).
    RefreshSpcDiscussions,
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
            GuiAction::SwitchRadarSite(site) => {
                write!(f, "Switch to radar site {}", site)
            }
            GuiAction::FetchSpcOutlook { day, products } => {
                write!(f, "Fetch SPC {} outlook ({} products)", day, products.len())
            }
            GuiAction::RefreshSpcOutlooks => {
                write!(f, "Refresh SPC outlooks")
            }
            GuiAction::FetchNwsAlerts => {
                write!(f, "Fetch NWS alerts")
            }
            GuiAction::RefreshNwsAlerts => {
                write!(f, "Refresh NWS alerts")
            }
            GuiAction::FetchSpcDiscussions => {
                write!(f, "Fetch SPC Mesoscale Discussions")
            }
            GuiAction::RefreshSpcDiscussions => {
                write!(f, "Refresh SPC Mesoscale Discussions")
            }
        }
    }
}
