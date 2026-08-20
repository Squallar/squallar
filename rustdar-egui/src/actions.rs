use chrono::NaiveDateTime;
use rustdar_geo::GeoBounds;
use rustdar_source::id::LayerId;

#[derive(Debug, Clone)]
pub struct RadarConfig {
    pub site: String,
    pub timestamp: NaiveDateTime,
}

impl Default for RadarConfig {
    fn default() -> Self {
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
    SwitchRadarSite {
        site: String,
        pane_idx: usize,
    }, // Switch to a different radar site
    FetchOverlay {
        kind: LayerId,
        pane_idx: usize,
    },
    RefreshOverlay {
        kind: LayerId,
        pane_idx: usize,
    },
    RenderOverlay {
        pane_idx: usize,
        overlay_kind: LayerId,
        /// The *unexpanded* viewport. The renderer grows it by
        /// `texture.overdraw` — never by `OVERDRAW_FRACTION`, which the adapter
        /// may not have been able to honour.
        geo_bounds: GeoBounds,
        texture: crate::overlay_cache::OverlayTexturePlan,
        /// The cache token this raster is being asked for — see
        /// `ui_map_pane::overlay_cache_token`.
        data_generation: u64,
        zoom: i32,
    },
    EnableLoop {
        pane_idx: usize,
        lookback_secs: u64,
    },
    DisableLoop {
        pane_idx: usize,
    },
    ToggleLoopPlayback {
        pane_idx: usize,
    },
    StepLoopFrame {
        pane_idx: usize,
        forward: bool,
    },
    SeekLoopFrame {
        pane_idx: usize,
        frame_index: usize,
    },
    NavigateTime {
        pane_idx: usize,
        step_secs: i64,
    },
    NavigateOneScan {
        pane_idx: usize,
        forward: bool,
    },
    JumpToLive {
        pane_idx: usize,
    },
    StartGps {
        config: rustdar_nmea_serial::SerialConfig,
    },
    StopGps,
    RequestLocation,
    StopLocation,
    OpenLocationSettings,
    /// Build the voxel grid a 3D pane needs, if it is not already in hand.
    /// Level-triggered: re-emitted every frame until the grid lands, so the
    /// handler must dedupe on the target before it builds (150–200 ms at the
    /// desktop shape, measured) — safe only while that handler is synchronous.
    PrepareVolume {
        pane_idx: usize,
        target: crate::pane::VolumeTarget,
    },
    ReleaseVolume {
        pane_idx: usize,
    },
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
            GuiAction::FetchOverlay { kind, pane_idx } => {
                write!(f, "Fetch overlay {:?} for pane {}", kind, pane_idx)
            }
            GuiAction::RefreshOverlay { kind, pane_idx } => {
                write!(f, "Refresh overlay {:?} for pane {}", kind, pane_idx)
            }
            GuiAction::RenderOverlay {
                pane_idx,
                overlay_kind,
                ..
            } => {
                write!(f, "Render overlay {:?} for pane {}", overlay_kind, pane_idx)
            }
            GuiAction::EnableLoop {
                pane_idx,
                lookback_secs,
            } => {
                write!(
                    f,
                    "Enable loop for pane {} ({}s lookback)",
                    pane_idx, lookback_secs
                )
            }
            GuiAction::DisableLoop { pane_idx } => {
                write!(f, "Disable loop for pane {}", pane_idx)
            }
            GuiAction::ToggleLoopPlayback { pane_idx } => {
                write!(f, "Toggle loop playback for pane {}", pane_idx)
            }
            GuiAction::StepLoopFrame { pane_idx, forward } => {
                write!(
                    f,
                    "Step loop frame for pane {} (forward={})",
                    pane_idx, forward
                )
            }
            GuiAction::SeekLoopFrame {
                pane_idx,
                frame_index,
            } => {
                write!(
                    f,
                    "Seek loop to frame {} for pane {}",
                    frame_index, pane_idx
                )
            }
            GuiAction::NavigateTime {
                pane_idx,
                step_secs,
            } => {
                write!(
                    f,
                    "Navigate time by {} seconds for pane {}",
                    step_secs, pane_idx
                )
            }
            GuiAction::NavigateOneScan { pane_idx, forward } => {
                write!(
                    f,
                    "Navigate one scan (forward={}) for pane {}",
                    forward, pane_idx
                )
            }
            GuiAction::JumpToLive { pane_idx } => {
                write!(f, "Jump to live for pane {}", pane_idx)
            }
            GuiAction::StartGps { .. } => {
                write!(f, "Start GPS")
            }
            GuiAction::StopGps => {
                write!(f, "Stop GPS")
            }
            GuiAction::RequestLocation => {
                write!(f, "Use the platform location service")
            }
            GuiAction::StopLocation => {
                write!(f, "Stop the platform location service")
            }
            GuiAction::OpenLocationSettings => {
                write!(f, "Open the system location settings")
            }
            GuiAction::PrepareVolume { pane_idx, target } => {
                write!(
                    f,
                    "Prepare {} volume for pane {} from {} at {}",
                    target.product.code(),
                    pane_idx,
                    target.volume.site,
                    target.volume.collected,
                )
            }
            GuiAction::ReleaseVolume { pane_idx } => {
                write!(f, "Release the volume pane {} was holding", pane_idx)
            }
        }
    }
}
