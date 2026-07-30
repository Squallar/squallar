use chrono::NaiveDateTime;
use rustdar_overlays::render::overlay_state::OverlayKind;
use rustdar_overlays::types::GeoBounds;

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
    SwitchRadarSite {
        site: String,
        pane_idx: usize,
    }, // Switch to a different radar site
    /// Fetch overlay data for the given kind (initial load when layer enabled).
    FetchOverlay {
        kind: OverlayKind,
        pane_idx: usize,
    },
    /// Re-fetch overlay data for the given kind (manual refresh).
    RefreshOverlay {
        kind: OverlayKind,
        pane_idx: usize,
    },
    /// Request a background overlay rasterization for a pane.
    RenderOverlay {
        pane_idx: usize,
        overlay_kind: OverlayKind,
        /// The *unexpanded* viewport. The renderer grows it by
        /// `texture.overdraw` — never by `OVERDRAW_FRACTION`, which the adapter
        /// may not have been able to honour.
        geo_bounds: GeoBounds,
        /// Pixel dimensions and the overdraw they were sized for, already
        /// reconciled with the adapter's `max_texture_dimension_2d`.
        texture: crate::overlay_cache::OverlayTexturePlan,
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
    /// Start the desktop serial GPS reader with the given config.
    StartGps {
        config: rustdar_gps::GpsConfig,
    },
    /// Stop the desktop serial GPS reader.
    StopGps,
    /// Build the voxel grid a 3D pane needs, if it is not already in hand.
    ///
    /// Emitted from inside the pane's own render arm, on every frame the pane
    /// does not yet have the grid it wants. That sounds like a request storm and
    /// is not: the handler is idempotent against the store it fills, and the
    /// pane stops asking the moment the grid lands. Making it edge-triggered
    /// instead would mean remembering across `reset_panes_for_site`,
    /// `SwitchRadarSite` and a surface loss, which is three places to forget.
    ///
    /// A whole-volume grid is expensive — **150–200 ms** at the desktop shape,
    /// measured — so the handler must dedupe on the target before it builds,
    /// not after.
    ///
    /// **Level-triggered is safe only while that handler is synchronous**, and a
    /// handler that posts the build to a worker has to add state of its own or
    /// this fires a fresh job every frame until the first returns. The handler
    /// carries the full note; it is repeated here because this variant's own
    /// contract is what makes the requirement.
    PrepareVolume {
        pane_idx: usize,
        target: crate::pane::VolumeTarget,
    },
    /// This pane no longer needs whatever volume it was holding.
    ///
    /// Refcounted **by target** on the other side, not by pane: two panes on one
    /// volume share one build and one GPU upload, so the grid goes when the last
    /// of them lets go. Emitted when a 3D pane stops being one, and when the
    /// volume it wants changes.
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
