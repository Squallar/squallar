//! The typed Gui↔App seam (WO-E2): [`FrameInputs`] for snapshot-shaped facts
//! the App owns and re-states every frame, [`GuiEvent`] for event-shaped
//! pushes applied at the call site's existing control-flow position. The
//! out-direction, `GuiAction`, already had this shape and lives in
//! [`crate::actions`]. The re-verbed `GuiAction` lands here at E5.
//!
//! The appliers — `Gui::apply` and `Gui::apply_frame_inputs` — live on
//! `impl Gui` in the `ui` module, beside the state they write; this module
//! holds the vocabulary both sides of the seam can name.
//!
//! # What deliberately did not become part of the seam
//!
//! Five `Gui` setters are not App-pushes and stay setters:
//!
//! * `set_live_chunks`, `set_chunk_notifications`, `set_notifier_endpoint` —
//!   written by the settings UI and the config load, both inside this crate;
//!   the frontend never calls them in production.
//! * `set_section_draw_armed`, `set_region_pick_armed` — Gui-internal
//!   interaction toggles whose mutual-exclusion bodies are the reason they
//!   are setters at all; every caller is inside this crate (`pub(crate)`
//!   since E2 Land 2).
//!
//! `Gui::set_initial_site` is also untouched: it is the config-load path,
//! called once at startup, and dissolves at E6.

use crate::actions::RadarConfig;
use crate::ui::{ChunkFeedStatus, CurrentVolumeStamp};
use rustdar_radar::types::ScanInfo;

/// One frame's facts, composed by the App from state it already owns, applied
/// by `Gui::apply_frame_inputs` once per frame immediately before `Gui::ui`.
///
/// `chunk_status`/`current_volumes` are typed fields DELIBERATELY: E8
/// replaces them with `sources: &[SourceLiveness]` when the radar root fields
/// dissolve (adversary m7).
pub struct FrameInputs<'a> {
    /// Safe-area insets in logical pixels (top, bottom, left, right).
    pub safe_area_insets: (f32, f32, f32, f32),
    /// Whether this platform can quit; `false` drops Exit from the menu.
    pub supports_exit: bool,
    /// This build's loop frame cap, for the timeline's row-2 caption.
    pub loop_frame_budget: usize,
    /// Whether this platform has a location settings page to offer.
    pub location_settings_available: bool,
    /// What the platform location service is doing: (permission, active).
    pub location: (rustdar_location::LocationPermission, bool),
    /// Fix + when the app heard it. The instant travels WITH the fix because
    /// `user_fix_at` is "when did we last hear anything", stamped at arrival —
    /// re-stamping per frame would break the settings pane's staleness
    /// question (see `user_fix_at` on the `Gui` state).
    pub gps: Option<(rustdar_location::Fix, web_time::Instant)>,
    /// Compass heading in degrees, once a platform has delivered one.
    pub user_heading: Option<f32>,
    /// Whether the site list is still short of the network.
    pub catalogue_pending: bool,
    /// What the real-time chunk feed is doing. `Copy` — by value.
    pub chunk_status: ChunkFeedStatus,
    /// Each site's current-volume stamp; cloned into the `Gui` on apply.
    pub current_volumes: &'a std::collections::HashMap<String, CurrentVolumeStamp>,
    /// How much extra tile detail the 3D floor can actually show. Pushed from
    /// `present_frame` after this frame's `Gui::ui` under the setter regime,
    /// so the UI always read it a frame late; composed at the top of the next
    /// frame it has the identical observable timing.
    pub floor_tile_zoom_bias: u8,
}

/// Event-shaped pushes applied at the call site's existing control-flow
/// position. Variants named after today's setters on purpose; they re-verb
/// at E5/E8.
pub enum GuiEvent {
    /// A complete volume's scan info, for all panes viewing the site.
    /// Replaces: ends the wait (spinner down, archive backoff reset) and
    /// spends the one-shot zoom latch if any pane took it.
    ScanInfoForSite { site: String, info: ScanInfo },
    /// MERGE semantics, NOT replace — former `apply_chunk_scan_info` doc
    /// (partial volumes union products/elevations; no spinner/backoff touch).
    ChunkScanInfo { site: String, info: ScanInfo },
    /// Scan info for one pane only.
    ScanInfoForPane { pane_idx: usize, info: ScanInfo },
    /// Whether a fetch someone is waiting on is in flight.
    Fetching(bool),
    /// A fetch failed: the message, spinner down, archive backoff advanced.
    Error(String),
    /// The radar config, with the Set Time dialog's strings kept in sync.
    RadarConfig(RadarConfig),
    /// Live/historic viewing mode for one pane.
    ViewingLiveForPane { pane_idx: usize, live: bool },
    /// Install what can draw 3D panes, or take it away.
    VolumePainter(Option<std::sync::Arc<dyn crate::volume_view::VolumePainter>>),
}

#[cfg(test)]
mod tests;
