//! The typed Gui↔App seam: [`FrameInputs`] for snapshot-shaped facts
//! the App owns and re-states every frame, [`GuiEvent`] for event-shaped
//! pushes applied at the call site's existing control-flow position. The
//! out-direction, `GuiAction`, already had this shape and lives in
//! [`crate::actions`]. The re-verbed `GuiAction` lands here at E5.

use squallar_device_profile::hist::Hist;
use squallar_radar::types::ScanInfo;

/// The frame instrument's histograms, borrowed for one frame — what the
/// diagnostics overlay windows. Cumulative-from-boot recorders; the overlay
/// itself takes the trailing-window diff, so nothing here is ever displayed
/// raw. References rather than copies: composing this costs pointer packing,
/// whether or not the overlay is showing.
#[derive(Clone, Copy)]
pub struct FrameDiagnostics<'a> {
    /// Service of presented frames whose input carried a pointer/touch/
    /// wheel event. Service is the redraw minus the swapchain acquire.
    pub service_interact: &'a Hist,
    /// Service of presented frames whose input carried none.
    pub service_idle: &'a Hist,
    /// Where an interact frame's service went:
    /// `[pre, pump, ui, prepare, finish, post]`.
    pub segments: [&'a Hist; 6],
    /// The swapchain-acquire span of interact frames — the vsync wait,
    /// excluded from service and reported beside it.
    pub acquire: &'a Hist,
    /// Redraw-to-redraw interval of presented frames, both input families.
    /// Never added to service; the two share no denominator.
    pub cadence: &'a Hist,
    /// A GPU pass-timing line, verbatim, once a probe can supply one.
    /// `None` prints as the overlay's absence text — absence, never
    /// extrapolation.
    pub gpu_passes: Option<&'a str>,
    /// The `budget state:` sentence, verbatim — the bracket and rung the
    /// budgets were resolved at and the host signals beside them. `None`
    /// until the telemetry tick has composed one; the overlay prints its
    /// absence text there.
    pub budget_state: Option<&'a str>,
}

/// Where [`crate::Gui::ui_phased`] crossed its own phase boundaries, handed
/// back so the App's frame ledger can cut its `ui` segment with them.
///
/// **Instants, not durations, and that is the point.** The ledger's `ui`
/// segment is bracketed by the App's own two stamps around the call; cutting
/// it with instants taken *inside* the call makes the cuts contiguous with
/// those brackets, so the pieces telescope to the segment exactly instead of
/// summing to something near it. A struct of durations could not do that — it
/// would leave the prologue and the return outside every figure, which is the
/// hole the renderer's own four-phase pass ledger had.
///
/// Five stamps, six cuts: `ui_start → polled → laid_out → shell → panes →
/// applied → ui_end`. The two outer boundaries are the ledger's, not this
/// type's, which is why they are absent here.
#[derive(Clone, Copy, Debug)]
pub struct UiPhaseStamps {
    /// After the frame's polls: the site-table republish, the auto-poll
    /// check and the offline-download settle. These run before anything is
    /// laid out and emit most of the frame's fetch actions.
    pub polled: web_time::Instant,
    /// After `LayoutCtx::resolve`, the pane-grid reflow, the site-query
    /// expiry and the fade invariants — the root `Ui` is built and the
    /// frame's geometry is settled.
    pub laid_out: web_time::Instant,
    /// After `render_shell`: the topbar, the layer stack and the drawer.
    /// **The frame's eye click is read here** and acted on in the next cut.
    pub shell: web_time::Instant,
    /// After the time dialog and `render_panes` — the map surfaces.
    pub panes: web_time::Instant,
    /// After the pending appliers (pane view, section line, region, section
    /// edit) and the fade toggle.
    pub applied: web_time::Instant,
}

/// One frame's facts, composed by the App from state it already owns, applied
/// by `Gui::apply_frame_inputs` once per frame immediately before `Gui::ui`.
pub struct FrameInputs<'a> {
    /// Safe-area insets in logical pixels (top, bottom, left, right).
    pub safe_area_insets: (f32, f32, f32, f32),
    /// Whether this platform can quit; `false` drops Exit from the menu.
    pub supports_exit: bool,
    /// This build's loop frame cap, for the timeline's row-2 caption.
    pub loop_frame_budget: usize,
    /// **How many overlay rasters one pane and layer may have crossing at
    /// once** — the device's `Budgets::concurrent_renders`, which is the same
    /// figure every other background render on this device is spent against
    /// and is read off the resolved budgets rather than a `cfg`.
    ///
    /// Composed here rather than read from `squallar_device_profile` inside this
    /// crate for the reason [`Self::loop_frame_budget`] is: the resolved value
    /// is the App's, and a browser's is not a `cfg` — the same wasm build gets
    /// a different number on a blocklisted driver than on a workstation GPU.
    pub concurrent_renders: usize,
    /// **What the map tile caches may hold** — the device's
    /// `Budgets::tile_cache()`, in bytes per population (styled, parsed,
    /// terrain), composed here for the reason [`Self::concurrent_renders`]
    /// is: the resolved value is the App's, and on the measured arm it is a
    /// share of what the card left over once the scene was paid for, which no
    /// `cfg` can say. Applied to every tile source, live or parked, before the
    /// pane loop.
    pub tile_cache: squallar_device_profile::budget::TileCacheBudget,
    /// Whether this platform has a location settings page to offer.
    pub location_settings_available: bool,
    /// What the platform location service is doing: (permission, active).
    pub location: (squallar_location::LocationPermission, bool),
    /// Fix + when the app heard it. The instant travels WITH the fix because
    /// `user_fix_at` is "when did we last hear anything", stamped at arrival —
    /// re-stamping per frame would break the settings pane's staleness
    /// question (see `user_fix_at` on the `Gui` state).
    pub gps: Option<(squallar_location::Fix, web_time::Instant)>,
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
    pub liveness: &'a [squallar_source::liveness::SourceLiveness],
    /// How much extra tile detail the 3D floor can actually show. Pushed from
    /// `present_frame` after this frame's `Gui::ui` under the setter regime,
    /// so the UI always read it a frame late; composed at the top of the next
    /// frame it has the identical observable timing.
    pub floor_tile_zoom_bias: u8,
    /// Moves when the shell's mirror plan changed on a frame whose strips
    /// were **held** (the clean skip): the realloc the new plan needs would
    /// destroy the picture the floors are sampling, so the shell defers it
    /// and this stamp makes the Gui repaint every strip first. A rung flip
    /// mid-orbit reaches the strips through here.
    pub mirror_plan_stamp: u64,
    /// The frame instrument's histograms, when the shell has them — the
    /// diagnostics overlay's input. `None` from a caller with no ledger (the
    /// test harness); the overlay then shows itself still collecting.
    pub frame_diagnostics: Option<FrameDiagnostics<'a>>,
}

/// Event-shaped pushes applied at the call site's existing control-flow
/// position. Variants named after today's setters on purpose; they re-verb
/// at E5/E8.
pub enum GuiEvent {
    /// A complete volume's scan info, for all panes viewing the site.
    ///
    /// **The site-wide fan-out, and the live feed's variant.** Every pane on
    /// the site takes it, which is right for a volume nobody asked for in
    /// particular: the real-time chunk feed's closed volumes, the archive
    /// auto-poll, and the refetch a retired feed falls back to. An archive
    /// volume a *pane* navigated to is [`GuiEvent::ScanInfoForTimeGroup`]
    /// instead — see there for why the two cannot be one event.
    ScanInfoForSite { site: String, info: ScanInfo },
    /// **A volume the real-time chunk feed completed**, for every pane on the
    /// site that is *following* live.
    ///
    /// The site-wide fan-out narrowed by the one thing that separates the feed
    /// from a wholesale replacement of the site's data: a pane parked in the
    /// archive is not watching for it. `UNLINK_NOTE`'s two clauses are both
    /// about that pane — it holds its moment when parked, and follows new
    /// scans when live — and this event is the producer of the second, so it
    /// must not be the one that breaks the first.
    LiveScanInfoForSite { site: String, info: ScanInfo },
    /// **The archive volume one pane asked for**, delivered to that pane and
    /// the panes that share its clock — never to a same-site pane parked at
    /// its own moment.
    ///
    /// `UNLINK_NOTE` promises exactly this: "Parked in the archive it holds
    /// its moment; still live, it still follows new scans." The two clauses
    /// are two different audiences, which is why the shell has two events and
    /// not one with a flag. `requester` is the pane index the fetch was
    /// spawned for, carried back through `ScanResponse`; the group is
    /// resolved here, at delivery, so a link toggled while the fetch was in
    /// flight is honoured.
    ///
    /// A pane in the group but on **another site** is skipped: the volume is
    /// this site's, and shared time never means shared data.
    ScanInfoForTimeGroup {
        site: String,
        requester: usize,
        info: ScanInfo,
    },
    /// MERGE semantics, NOT replace — former `apply_chunk_scan_info` doc
    /// (partial volumes union products/elevations; no spinner/backoff touch).
    ChunkScanInfo { site: String, info: ScanInfo },
    /// Scan info for one pane only.
    ScanInfoForPane { pane_idx: usize, info: ScanInfo },
    /// Whether a fetch someone is waiting on is in flight.
    Fetching(bool),
    /// A fetch failed: the message, spinner down, archive backoff advanced.
    Error(String),
    /// The time the shell has navigated to, and **the only thing about a
    /// scan the shell pushes**: a site belongs to a pane, and the shell sets
    /// it by writing that pane.
    ///
    /// It replaced `GuiEvent::RadarConfig`, which carried a site beside the
    /// timestamp for one writer — `SwitchRadarSite` — that already writes
    /// every moving pane's site itself. With no app-wide site left for the
    /// second half to land in, the two variants said the same thing and one
    /// of them went.
    ///
    /// Applying it re-renders the Set Time dialog's two strings from the time
    /// still selected, so a half-typed edit does not survive a navigation.
    SelectedTime(chrono::NaiveDateTime),
    /// Live/historic viewing mode for one pane.
    ViewingLiveForPane { pane_idx: usize, live: bool },
    /// **One pane's time selection moved to `instant`** — the whole gesture,
    /// as one event.
    ///
    /// Three things move together whenever a user names a moment on a pane:
    /// the pane's `viewing_live` posture, the pane's **clock**
    /// ([`crate::pane::PaneState::set_time_mode`], which settles every layer's
    /// playhead onto it), and the Set Time dialog's displayed selection.
    ///
    /// They were three separate pushes and one of them was simply missing from
    /// the step buttons: `handle_navigate_time` sent
    /// [`Self::ViewingLiveForPane`] and [`Self::SelectedTime`] and no clock
    /// move at all, while the scrubber wrote the clock itself in the UI before
    /// emitting the same action. So a step on a pane with no radar scan moved
    /// nothing a layer could read, which is WO-T3.10's defect. Making it one
    /// event is what stops a fourth caller omitting a half.
    ///
    /// `instant` is **UTC**; the dialog's own strings are local, and the
    /// conversion is this event's to make so the two cannot drift. Under
    /// `live` the clock goes back to [`crate::pane::TimeMode::Live`] and
    /// `instant` is only what the dialog shows.
    PaneTimeSelected {
        pane_idx: usize,
        instant: chrono::NaiveDateTime,
        live: bool,
    },
    /// Install what can draw 3D panes, or take it away.
    VolumePainter(Option<std::sync::Arc<dyn crate::volume_view::VolumePainter>>),
    /// Install what can draw a vector tile's fills from the GPU, or take it
    /// away. Absent, every fill takes the CPU placement path — see
    /// [`crate::tile_mesh`].
    TileMeshPainter(Option<std::sync::Arc<dyn crate::tile_mesh::TileMeshPainter>>),
}

#[cfg(test)]
mod tests;
