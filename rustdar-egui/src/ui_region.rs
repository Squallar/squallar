//! The patch of ground a 3D pane resamples, measured off the pane's own
//! viewport.
//!
//! # The region *is* the viewport
//!
//! There is no separate thing to aim, and no gesture that aims it. A 3D pane
//! resamples the largest square that fits inside the ground its own map is
//! showing, so its box and its floor are two measurements of one rectangle.
//!
//! That is not a simplification of the drag this replaced — it is the fix for
//! the class of bug the drag caused. The box's size and the floor's extent used
//! to be set independently: the box came from a square dragged out on some
//! *other* map pane, and the floor came from whatever viewport that pane
//! happened to be showing at the time. Whenever the second was smaller than the
//! first the floor simply stopped, leaving the volume standing on transparency
//! past its edge, and a 3D view opened from a tab — which has no source map at
//! all — got no floor whatsoever. Deriving one from the other makes coverage a
//! property rather than a coincidence.
//!
//! The drag's real purpose survives, because the grid has a fixed cell count:
//! a tighter box spends the same cells over less ground. Zooming the pane in is
//! now how that is asked for, which is also how every other pane in the
//! application is aimed.
//!
//! # Why the ground is measured rather than computed from the zoom
//!
//! Web Mercator's scale varies with latitude, so the ground under the top edge
//! of a tall viewport is *narrower* than the ground under its middle. Sizing
//! the box from the centre's scale alone would let it overhang the floor at the
//! poleward edge by around 3% on a 460 km box at 40°N — a thin transparent
//! strip along one side, which is precisely the symptom this module exists to
//! remove. So all four edges are unprojected through the pane's own projector
//! and measured with [`rustdar_radar::beam::site_bearing_range_km`], the
//! codebase's real geodesy, and the box takes the **smallest** of the four.
//! Containment then holds on every side, at every latitude.

use crate::pane::{GeoPoint, VolumeRegion};

/// The step the derived half-width is quantised to, kilometres.
///
/// **Not cosmetic — it is what stops the pane rebuilding for ever.** The region
/// is part of `VolumeTarget`, so any change to it asks for a fresh 8 MiB
/// resample. Measured continuously off a viewport, the half-width would differ
/// by a few metres between frames from nothing more than `f32` rect arithmetic
/// and walkers' zoom animation, and every one of those differences would be a
/// new key, a new build, and a permanently hot CPU whose only symptom is a fan.
///
/// A whole kilometre is far below anything a reader can see in a 460 km box
/// (it is well under one cell at every zoom) and far above the jitter.
///
/// Rounded **down**, always. The box must stay inside the ground the floor
/// covers, and rounding up would push it out by up to a kilometre — the exact
/// failure this module exists to prevent, reintroduced by a rounding mode.
const HALF_WIDTH_STEP_KM: f64 = 1.0;

/// The largest square box inscribed in `rect`'s ground, for a map centred by
/// `map_memory` on `center`.
///
/// `None` when the viewport has no area, when the projector cannot place its
/// centre on Earth, or when the ground it covers is too small for the resampler
/// to honour — all of which mean "there is no measurable viewport here" rather
/// than "use a smaller box". The caller falls back to
/// [`crate::pane::DEFAULT_HALF_WIDTH_KM`], which crops nothing.
///
/// A [`walkers::Projector`] is built here rather than borrowed from inside
/// `Map::show`, because the region is needed on frames where no map is drawn —
/// a pane with its floor turned off still resamples a box. `Projector::new`
/// takes exactly the three things the pane already has, and going through it
/// rather than restating `256 · 2^zoom / 360` is what keeps this measuring the
/// same projection the floor is drawn in: a tile size or zoom convention that
/// moves in `walkers` moves both together.
pub(crate) fn region_for_viewport(
    rect: egui::Rect,
    map_memory: &walkers::MapMemory,
    center: walkers::Position,
) -> Option<VolumeRegion> {
    if !(rect.width() > 0.0 && rect.height() > 0.0) {
        return None;
    }
    let projector = walkers::Projector::new(rect, map_memory, center);
    let to_geo = |pos: egui::Pos2| {
        let position = projector.unproject(pos.to_vec2());
        GeoPoint {
            lat: position.y(),
            lon: position.x(),
        }
    };

    let centre = to_geo(rect.center());
    if !centre.is_on_earth() {
        return None;
    }

    // The four edge midpoints. Each gives the ground from the centre to one
    // edge; the box's half-width is the smallest, so the square it describes is
    // inside the viewport on all four sides even though Mercator makes the four
    // distances differ.
    let edges = [
        egui::pos2(rect.center().x, rect.top()),
        egui::pos2(rect.center().x, rect.bottom()),
        egui::pos2(rect.left(), rect.center().y),
        egui::pos2(rect.right(), rect.center().y),
    ];
    let mut half_width_km = f64::INFINITY;
    for edge in edges {
        let point = to_geo(edge);
        if !point.is_on_earth() {
            return None;
        }
        let (_, range_km) =
            rustdar_radar::beam::site_bearing_range_km(centre.lat, centre.lon, point.lat, point.lon);
        if !range_km.is_finite() {
            return None;
        }
        half_width_km = half_width_km.min(range_km);
    }

    // Down to the step, then refused rather than clamped up if what is left is
    // below what `build_voxels` will honour: a clamped-up box would be larger
    // than the viewport that measured it, and the pane's own caption would
    // describe a box the floor does not cover.
    let quantised = (half_width_km / HALF_WIDTH_STEP_KM).floor() * HALF_WIDTH_STEP_KM;
    if !(quantised >= rustdar_radar::voxel::MIN_HALF_WIDTH_KM) {
        return None;
    }
    VolumeRegion::new(centre, quantised)
}

#[cfg(test)]
mod tests;
