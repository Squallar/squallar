//! **The radar layer's own glue, in the presentation crate.**
//!
//! What the radar layer needs the presentation to hold for it that no other
//! layer has. The geometry itself is radar's own type and lives in
//! `rustdar-radar`; what stays here is the reading of it out of a
//! [`LayerTimeState`], which is a presentation type. Everything here is
//! reached by name — a caller that wants radar geometry asks for
//! [`LoopGeometry`] — so the generic layer vocabulary next door never grows a
//! radar field.

use rustdar_overlays::render::overlay_state::OverlayRegistry;
use rustdar_radar::loop_geometry::LoopGeometry;
use rustdar_radar::sites::RadarSite;
use rustdar_source::id::{LayerId, known};

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
    view: rustdar_radar::types::RenderView,
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

/// Switch the archive poll on or off, through the layer's own control surface
/// — the one write both the ☰ menu leaf and the settings row make, so the two
/// cannot drift.
pub fn auto_poll_update(on: bool) -> rustdar_source::controls::ControlUpdate {
    rustdar_source::controls::ControlUpdate {
        id: rustdar_radar::source::AUTO_POLL_CONTROL,
        value: rustdar_source::controls::ControlValue::Bool(on),
    }
}
