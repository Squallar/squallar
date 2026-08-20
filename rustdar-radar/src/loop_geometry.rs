//! **Where a radar layer's frames are projected from.**

use crate::sites::RadarSite;

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
