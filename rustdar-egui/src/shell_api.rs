//! The typed Gui↔App seam: [`FrameInputs`] for snapshot-shaped facts
//! the App owns and re-states every frame, [`GuiEvent`] for event-shaped
//! pushes applied at the call site's existing control-flow position. The
//! out-direction, `GuiAction`, already had this shape and lives in
//! [`crate::actions`]. The re-verbed `GuiAction` lands here at E5.

use crate::actions::RadarConfig;
use rustdar_radar::types::ScanInfo;

/// One frame's facts, composed by the App from state it already owns, applied
/// by `Gui::apply_frame_inputs` once per frame immediately before `Gui::ui`.
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
    /// **What each layer says it is doing**, in the layer's own vocabulary
    /// behind an opaque payload (WO-E8c). This replaced two radar-shaped
    /// members — the chunk-feed status and the per-site volume stamps — and
    /// the reason it is opaque is that the second one was already the third
    /// radar field to want a home here.
    ///
    /// The shell rebuilds an entry when that layer's answer **changes**;
    /// every other frame re-states the same `Arc`s.
    pub liveness: &'a [rustdar_source::liveness::SourceLiveness],
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
    /// The time the shell has navigated to, with no claim about the site.
    ///
    /// [`GuiEvent::RadarConfig`] carries both, and exactly one of its senders
    /// means both: `SwitchRadarSite`, which moves the app-wide site. The rest
    /// were reading the global site back out of the `Gui` and handing it
    /// straight back unchanged, so that the timestamp had something to travel
    /// in -- which reads like three more writers of a field that has one.
    SelectedTime(chrono::NaiveDateTime),
    /// Live/historic viewing mode for one pane.
    ViewingLiveForPane { pane_idx: usize, live: bool },
    /// Install what can draw 3D panes, or take it away.
    VolumePainter(Option<std::sync::Arc<dyn crate::volume_view::VolumePainter>>),
}

#[cfg(test)]
mod tests;
