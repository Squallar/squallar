//! **The radar layer's own glue, in the presentation crate.**
//!
//! What the radar layer needs the presentation to hold for it that no other
//! layer has. The geometry itself is radar's own type and lives in
//! `squallar-radar`; what stays here is the reading of it out of a
//! [`LayerTimeState`], which is a presentation type. Everything here is
//! reached by name — a caller that wants radar geometry asks for
//! [`LoopGeometry`] — so the generic layer vocabulary next door never grows a
//! radar field.

use squallar_overlays::render::overlay_state::OverlayRegistry;
use squallar_radar::loop_geometry::LoopGeometry;
use squallar_radar::sites::RadarSite;
use squallar_source::controls::ControlItem;
use squallar_source::id::{LayerId, known};
use squallar_source::liveness::SourceLiveness;

use crate::pane::LayerTimeState;

/// The geometry `time`'s frames are projected from, when it is a radar
/// layer's timeline and a loop has been built on it.
pub fn geometry(time: &LayerTimeState) -> Option<&LoopGeometry> {
    time.anchor_as::<LoopGeometry>()
}

/// The site a radar layer's frames are keyed to, or `""` for a timeline that
/// has no geometry yet — the same empty string the state used to be born
/// holding, and the same answer the arrival filter used to compare against.
pub fn site(time: &LayerTimeState) -> &str {
    geometry(time).map_or("", |geo| geo.site.as_str())
}

/// The coordinates a radar layer's frames are projected about, or `(0.0, 0.0)`
/// for a timeline with no geometry — the pair the state used to be born
/// holding.
pub fn coords(time: &LayerTimeState) -> (f64, f64) {
    geometry(time).map_or((0.0, 0.0), |geo| (geo.lat, geo.lon))
}

/// The timeline a radar loop starts with: listing requested, covering
/// `span_secs`, anchored at `site`'s geometry.
pub fn begin_loop(
    span_secs: u64,
    site: &RadarSite,
    view: squallar_radar::types::RenderView,
) -> LayerTimeState {
    LayerTimeState::begin(span_secs, view, Box::new(LoopGeometry::of(site)))
}

// ── The archive poll (WO-E8a) ────────────────────────────────────────────
//
// The radar layer polls the archive on the same terms as every other
// auto-polling layer, through `SourceHandler::auto_fetch_delay`. What stays
// here is the *reading* of that answer: the presentation asks these three
// questions and never names a radar field to do it.

/// The layer whose poll the status bar's chip, the ☰ menu's leaf and the
/// settings row are all about.
pub const POLL_LAYER: LayerId = known::RADAR;

/// Whether the archive poll is switched on. A layer that declares no interval
/// is a layer that will not poll, and radar declares none exactly when the
/// user has turned this off — so this is the switch, read through the
/// contract rather than copied beside it.
pub fn auto_poll_enabled(overlays: &OverlayRegistry) -> bool {
    overlays.auto_poll_interval(&POLL_LAYER).is_some()
}

/// Whether a round has ever been asked for — what tells "counting down"
/// from "no timer running yet", which is the difference between the chip
/// printing a number and printing `archive off`.
pub fn archive_poll_started(overlays: &OverlayRegistry) -> bool {
    overlays.fetch_time(&POLL_LAYER).is_some()
}

/// How long until the archive poll's next round may start, or `None` while
/// the poll is off or a tracked round is already in flight.
pub fn archive_poll_delay(overlays: &OverlayRegistry) -> Option<std::time::Duration> {
    overlays.auto_fetch_delay(&POLL_LAYER)
}

/// **Whether a radar round someone is waiting on is in flight** — the
/// spinner's answer, and the layer's own.
///
/// The shell used to keep a second copy of this beside the layer's, raised
/// and dropped at the same four moments; WO-E8d deleted the copy. The two
/// could not disagree at any observable point — every path that dropped the
/// shell's flag ended the layer's round in the same statement pair — which is
/// what made one of them redundant rather than load-bearing.
pub fn archive_fetching(overlays: &OverlayRegistry) -> bool {
    overlays.is_fetching(&POLL_LAYER)
}

/// Switch the archive poll on or off, through the layer's own control surface
/// — the one write both the ☰ menu leaf and the settings row make, so the two
/// cannot drift.
pub fn auto_poll_update(on: bool) -> squallar_source::controls::ControlUpdate {
    squallar_source::controls::ControlUpdate {
        id: squallar_radar::source::AUTO_POLL_CONTROL,
        value: squallar_source::controls::ControlValue::Bool(on),
    }
}

// ── The chunk feed's switches (WO-E8b) ───────────────────────────────────
//
// Three facts that used to be `Gui` root fields and are now the radar
// handler's own: the live-chunk switch, the push-notification switch and the
// notifier endpoint. `squallar-egui` cannot see a `RadarSource` field —
// `OverlayRegistry` hands out `&dyn SourceHandler` and `as_any` is refused
// (ruling (23)(iii)) — so the presentation reads them back off the layer's
// **declared control surface**, which is the door the inspector, the ☰ menu
// and the settings rows were already sharing for the archive poll.

/// The `enabled` flag of the [`ControlItem::Toggle`] `id` names in `controls`,
/// or `None` when the layer declares no such toggle.
fn control_toggle(controls: &[ControlItem], id: &str) -> Option<bool> {
    controls.iter().find_map(|item| match item {
        ControlItem::Toggle {
            id: this, enabled, ..
        } if *this == id => Some(*enabled),
        _ => None,
    })
}

/// The text of the [`ControlItem::TextField`] `id` names in `controls`.
fn control_text<'a>(controls: &'a [ControlItem], id: &str) -> Option<&'a str> {
    controls.iter().find_map(|item| match item {
        ControlItem::TextField {
            id: this, value, ..
        } if *this == id => Some(value.as_str()),
        _ => None,
    })
}

/// **Whether live panes should be fed from the real-time chunk bucket.**
///
/// The active pane's radar slot answers and the layer's global is the
/// fallback — the precedence WO-E6b established, now read through the control
/// the layer offers for the active pane rather than off a field beside it.
pub fn live_chunks_enabled(gui: &crate::Gui) -> bool {
    control_toggle(
        &gui.layer_controls(&POLL_LAYER),
        squallar_radar::source::LIVE_CHUNKS_CONTROL,
    )
    .unwrap_or(true)
}

/// **The live-chunk switch's global answer** — the layer's own, with no
/// pane's copy in front of it. The value a pane that has never been told
/// takes, which is what the saved slot config falls back to.
pub fn live_chunks_default(gui: &crate::Gui) -> bool {
    control_toggle(
        &gui.layer_default_controls(&POLL_LAYER),
        squallar_radar::source::LIVE_CHUNKS_CONTROL,
    )
    .unwrap_or(true)
}

/// Whether to subscribe to the push-notification service.
pub fn chunk_notifications_enabled(gui: &crate::Gui) -> bool {
    control_toggle(
        &gui.layer_controls(&POLL_LAYER),
        squallar_radar::source::CHUNK_NOTIFICATIONS_CONTROL,
    )
    .unwrap_or(true)
}

/// **Where the notifier service lives**, exactly as typed — empty and all.
/// [`notifier_endpoint`] is the resolved answer; this is the box's contents,
/// which is what an editor has to show.
pub fn notifier_endpoint_raw(gui: &crate::Gui) -> String {
    control_text(
        &gui.layer_controls(&POLL_LAYER),
        squallar_radar::source::NOTIFIER_ENDPOINT_CONTROL,
    )
    .unwrap_or_default()
    .to_string()
}

/// **Where the notifier service lives**, with an empty box resolved to the
/// built-in default. The resolution itself is the layer's own — see
/// `RadarSource::notifier_endpoint` — and this is the same rule spelled over
/// the control surface, because that is the only door out of the registry.
pub fn notifier_endpoint(gui: &crate::Gui) -> String {
    let raw = notifier_endpoint_raw(gui);
    if raw.trim().is_empty() {
        squallar_radar::source::DEFAULT_NOTIFIER_ENDPOINT.to_string()
    } else {
        raw.trim().to_string()
    }
}

/// The update that writes the live-chunk switch.
pub fn live_chunks_update(on: bool) -> squallar_source::controls::ControlUpdate {
    squallar_source::controls::ControlUpdate {
        id: squallar_radar::source::LIVE_CHUNKS_CONTROL,
        value: squallar_source::controls::ControlValue::Bool(on),
    }
}

/// The update that writes the push-notification switch.
pub fn chunk_notifications_update(on: bool) -> squallar_source::controls::ControlUpdate {
    squallar_source::controls::ControlUpdate {
        id: squallar_radar::source::CHUNK_NOTIFICATIONS_CONTROL,
        value: squallar_source::controls::ControlValue::Bool(on),
    }
}

/// The update that writes the notifier endpoint.
pub fn notifier_endpoint_update(
    text: impl Into<String>,
) -> squallar_source::controls::ControlUpdate {
    squallar_source::controls::ControlUpdate {
        id: squallar_radar::source::NOTIFIER_ENDPOINT_CONTROL,
        value: squallar_source::controls::ControlValue::String(text.into()),
    }
}

/// **One switch, every pane** — the fan-out `Gui::set_live_chunks` used to do,
/// living where radar-shaped things live.
///
/// The live-chunk switch is **two facts**: the layer's global, which
/// `RadarSource::apply_control` writes for itself, and a copy in every pane's
/// radar slot **config**, which no contract door can reach — `PaneMut` carries
/// this pane's `state` and read-only `peers`, and a handler may not write
/// another pane's slot config. So the second half is done here, over the pane
/// vector, beside the write rather than inside it (orchestrator ruling (27),
/// route (b)).
///
/// It takes the whole [`ControlUpdate`] and matches the id itself, so the
/// generic door that calls it names no radar field — it hands the radar glue
/// every edit and the radar glue decides whether any of it was radar's.
pub fn fan_out_live_chunks<'a>(
    panes: impl IntoIterator<Item = &'a mut crate::pane::PaneState>,
    update: &squallar_source::controls::ControlUpdate,
) {
    if update.id != squallar_radar::source::LIVE_CHUNKS_CONTROL {
        return;
    }
    let squallar_source::controls::ControlValue::Bool(on) = update.value else {
        return;
    };
    for pane in panes {
        pane.set_radar_live_chunks(on);
    }
}

/// **Whether this edit was the user asking for a scan now.**
///
/// The refresh button cannot answer itself with [`ControlEffect::Fetch`]: that
/// routes to the generic overlay fetch path, and this layer's fetch is
/// dispatched by the shell with a site and a timestamp. So the button is
/// recognised here and the caller — which is the half that can name a site —
/// pushes the action.
///
/// [`ControlEffect::Fetch`]: squallar_source::controls::ControlEffect
pub fn refresh_requested(update: &squallar_source::controls::ControlUpdate) -> bool {
    update.id == squallar_radar::source::REFRESH_CONTROL
}

// ── The radar layer's liveness (WO-E8c) ──────────────────────────────────
//
// What the chunk feed and the current-volume merge are doing. It used to be
// two typed fields on `FrameInputs` and two on the `Gui`; it is now one
// opaque `SourceLiveness` payload filed under the radar id, and this module
// is the only place that knows what is inside it.

/// One site's current-volume stamp, as the App publishes it each frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CurrentVolumeStamp {
    /// Collection time of the newest data in the merged volume — the identity
    /// a 3D pane names its build by. Every sealed sweep advances it, which is
    /// what makes the 3D view rebuild in step with the map beside it.
    pub newest: chrono::NaiveDateTime,
    /// When the complete base volume under the merge began, where one
    /// contributes at all. `None` while the site's first volume is still
    /// filling: there is no complete volume yet and the caption says so.
    pub base_started: Option<chrono::NaiveDateTime>,
}

/// **The radar layer's whole live status**, in the layer's own vocabulary.
///
/// The payload of the [`SourceLiveness`] entry filed under [`POLL_LAYER`].
/// Built by the shell when either half **changes**, not per frame: it travels
/// behind an `Arc` and a frame that publishes it re-states the same one.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RadarLiveness {
    /// What the real-time chunk feed is doing.
    pub chunk_status: squallar_radar::chunk_feed::ChunkFeedStatus,
    /// Each site's current-volume stamp, advanced by every sealed sweep. A 3D
    /// pane names the volume it wants by [`CurrentVolumeStamp::newest`], which
    /// is what makes its rebuilds follow the live feed.
    pub current_volumes: std::collections::HashMap<String, CurrentVolumeStamp>,
}

/// The radar layer's entry in `liveness`, or `None` when the shell has not
/// published one yet. A payload of another shape answers `None` too, which is
/// the honest answer to "did radar say this?".
pub fn liveness(entries: &[SourceLiveness]) -> Option<&RadarLiveness> {
    SourceLiveness::find::<RadarLiveness>(entries, &POLL_LAYER)
}

/// What the real-time chunk feed is doing, or the resting answer when radar
/// has published nothing — the same default the field this replaced was born
/// holding.
pub fn chunk_status(entries: &[SourceLiveness]) -> squallar_radar::chunk_feed::ChunkFeedStatus {
    liveness(entries)
        .map(|live| live.chunk_status)
        .unwrap_or_default()
}

/// The stamp of `site`'s current volume, if this build holds one at all.
pub fn current_volume_for(entries: &[SourceLiveness], site: &str) -> Option<CurrentVolumeStamp> {
    liveness(entries)?.current_volumes.get(site).copied()
}

/// The `SourceLiveness` entry the shell publishes for this layer.
pub fn liveness_entry(live: RadarLiveness) -> SourceLiveness {
    SourceLiveness::new(POLL_LAYER, live)
}
