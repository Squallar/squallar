//! **The radar layer's own glue, in the presentation crate.**
//!
//! What the radar layer needs the presentation to hold for it that no other
//! layer has, and that `rustdar-radar` does not yet own. Everything here is
//! reached by name — a caller that wants radar geometry asks for
//! [`LoopGeometry`] — so the generic layer vocabulary next door never grows a
//! radar field.

use rustdar_radar::sites::RadarSite;

/// **Where one radar layer's frames are projected from**, captured when the
/// loop was built.
///
/// The site code is the loop's *geometry* site, not the pane's live
/// selection: the two are deliberately decoupled so that a pane whose site
/// moves mid-loop keeps routing arriving frames to the loop that asked for
/// them, and keeps projecting them about the coordinates they were rendered
/// for. `lat`/`lon` are render **inputs** — they are mapped straight into the
/// renderer's parameters — and the raster's placement is derived *from* them,
/// never the other way round.
#[derive(Clone, Debug, PartialEq)]
pub struct LoopGeometry {
    /// NEXRAD site code supplying the projection geometry, e.g. `"KTLX"`.
    pub site: String,
    pub lat: f64,
    pub lon: f64,
}

impl LoopGeometry {
    /// The geometry of `site`, taken whole so that the code and the
    /// coordinates it projects with cannot come from two different sites.
    pub fn of(site: &RadarSite) -> Self {
        Self {
            site: site.name.to_string(),
            lat: site.lat,
            lon: site.lon,
        }
    }
}

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
