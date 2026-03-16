use rustdar_overlays::spc::outlook::{OutlookDay, OutlookProduct};
use rustdar_overlays::types::GeoBounds;
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
    SwitchRadarSite { site: String, pane_idx: usize }, // Switch to a different radar site
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
    /// Request a background overlay rasterization for a pane.
    RenderOverlay {
        pane_idx: usize,
        overlay_kind: OverlayRenderKind,
        geo_bounds: GeoBounds,
        width: u32,
        height: u32,
        data_generation: u64,
        zoom: i32,
    },
    /// Enable radar loop for a pane — triggers historical scan listing + fetch.
    EnableLoop {
        pane_idx: usize,
        lookback_secs: u64,
    },
    /// Disable radar loop for a pane and drop cached frames.
    DisableLoop {
        pane_idx: usize,
    },
    /// Toggle play/pause of the loop animation for a pane.
    ToggleLoopPlayback {
        pane_idx: usize,
    },
    /// Step the loop one frame forward or backward.
    StepLoopFrame {
        pane_idx: usize,
        forward: bool,
    },
    /// Seek to a specific frame index in the loop.
    SeekLoopFrame {
        pane_idx: usize,
        frame_index: usize,
    },
    /// Navigate forward or backward by a time step (seconds).
    NavigateTime {
        pane_idx: usize,
        step_secs: i64,
    },
    /// Step to the next or previous adjacent scan.
    NavigateOneScan {
        pane_idx: usize,
        forward: bool,
    },
    /// Jump back to live mode (display latest available scan).
    JumpToLive {
        pane_idx: usize,
    },
}

/// Which overlay type to rasterize.
#[derive(Debug, Clone)]
pub enum OverlayRenderKind {
    SpcOutlook,
    SpcDiscussions,
    NwsAlerts,
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
            GuiAction::SwitchRadarSite { site, pane_idx } => {
                write!(f, "Switch pane {} to radar site {}", pane_idx, site)
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
            GuiAction::RenderOverlay { pane_idx, overlay_kind, .. } => {
                write!(f, "Render overlay {:?} for pane {}", overlay_kind, pane_idx)
            }
            GuiAction::EnableLoop { pane_idx, lookback_secs } => {
                write!(f, "Enable loop for pane {} ({}s lookback)", pane_idx, lookback_secs)
            }
            GuiAction::DisableLoop { pane_idx } => {
                write!(f, "Disable loop for pane {}", pane_idx)
            }
            GuiAction::ToggleLoopPlayback { pane_idx } => {
                write!(f, "Toggle loop playback for pane {}", pane_idx)
            }
            GuiAction::StepLoopFrame { pane_idx, forward } => {
                write!(f, "Step loop frame for pane {} (forward={})", pane_idx, forward)
            }
            GuiAction::SeekLoopFrame { pane_idx, frame_index } => {
                write!(f, "Seek loop to frame {} for pane {}", frame_index, pane_idx)
            }
            GuiAction::NavigateTime { pane_idx, step_secs } => {
                write!(f, "Navigate time by {} seconds for pane {}", step_secs, pane_idx)
            }
            GuiAction::NavigateOneScan { pane_idx, forward } => {
                write!(f, "Navigate one scan (forward={}) for pane {}", forward, pane_idx)
            }
            GuiAction::JumpToLive { pane_idx } => {
                write!(f, "Jump to live for pane {}", pane_idx)
            }
        }
    }
}
