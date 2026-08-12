//! The patch of ground a 3D pane resamples, measured off the pane's own
//! viewport.
//!
//! # The region *is* the viewport
//!
//! There is no separate thing to aim. A 3D pane resamples the largest square
//! that fits inside the ground its own map is showing, so its box and its floor
//! are two measurements of one rectangle — and the gesture that aims it is the
//! gesture that aims every other pane in the application: scroll, or pinch.
//! [`zoom_viewport`] is that gesture's 3D arm, and it writes the same
//! `MapMemory` a plan view's `walkers::Map` writes, by the same arithmetic.
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
    let (centre, measured) = measure_viewport(rect, map_memory, center)?;

    // Down to the step on each axis, and **before** the region's own clamp, so
    // the whole pipeline is a function of two whole kilometres: `clamped`'s
    // corner scaling is continuous in its input, and quantising after it would
    // hand a wide-open pane a fresh cache key on every frame of `f32` rect
    // jitter — the rebuild-for-ever this step exists to stop, reintroduced by
    // an ordering.
    let quantised = rustdar_radar::voxel::HalfExtentKm {
        east_km: quantise(measured.east_km),
        north_km: quantise(measured.north_km),
    };
    // A NaN reaches `VolumeRegion::new`, where `is_finite` refuses it outright.
    // Refusing it here as well would be the same answer by a longer route;
    // laundering it past both would be a key that never equals itself.
    let region = VolumeRegion::new(centre, quantised)?;

    // Refused rather than clamped up if the resampler's floor grew the box past
    // the ground that measured it: a clamped-up box is larger than the viewport
    // it came from, and the pane's own caption would describe a box the floor
    // does not cover.
    //
    // Stated as "did the stored box come back bigger than what was measured"
    // rather than as a second `< MIN_HALF_WIDTH_KM` test, because that *is* the
    // property — and because `HalfExtentKm::clamped` has two ways to grow an
    // axis, only one of which a pre-construction test would catch. The other is
    // its own documented corner: an extent past 47:1 is scaled to the diagonal
    // bound and lands back under the floor, which a viewport cannot reach today
    // and which this covers anyway for free.
    if region.half_east_km() > quantised.east_km || region.half_north_km() > quantised.north_km {
        return None;
    }
    Some(region)
}

/// One extent, rounded **down** to a whole [`HALF_WIDTH_STEP_KM`].
fn quantise(km: f64) -> f64 {
    (km / HALF_WIDTH_STEP_KM).floor() * HALF_WIDTH_STEP_KM
}

/// The centre of `rect`'s ground and the **unquantised** half-extent of the
/// largest square inscribed in it, kilometres.
///
/// The measurement half of [`region_for_viewport`], split out because
/// [`zoom_viewport`] needs the raw numbers: the quantisation exists to keep a
/// *cache key* still, and a zoom bound computed off a number that has already
/// been rounded down by up to a kilometre would stop the gesture up to a
/// kilometre early — visibly, at the tight end where the box is only ten across.
///
/// The answer is a [`rustdar_radar::voxel::HalfExtentKm`] and today both its
/// lanes hold the same number — see the loop below. The type is the one the
/// resampler, the renderer and [`VolumeRegion`] all speak, so carrying it from
/// the measurement outwards is what leaves the flip to a rectangle a change in
/// this function alone.
///
/// `None` for every reason `region_for_viewport` answers `None` except the
/// resampler's floor, which is the caller's decision rather than the
/// measurement's.
fn measure_viewport(
    rect: egui::Rect,
    map_memory: &walkers::MapMemory,
    center: walkers::Position,
) -> Option<(GeoPoint, rustdar_radar::voxel::HalfExtentKm)> {
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
    // edge; the box's half-width is the smallest of all four, so the square it
    // describes is inside the viewport on all four sides even though Mercator
    // makes the four distances differ.
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
        let (_, range_km) = rustdar_radar::beam::site_bearing_range_km(
            centre.lat, centre.lon, point.lat, point.lon,
        );
        if !range_km.is_finite() {
            return None;
        }
        half_width_km = half_width_km.min(range_km);
    }
    // One minimum over all four edges, written into both lanes: the box a
    // rectangular type is carrying is still the inscribed square, and this line
    // is the only reason it is.
    Some((
        centre,
        rustdar_radar::voxel::HalfExtentKm::square(half_width_km),
    ))
}

/// `walkers::Map::zoom_speed`'s default, which the plan view leaves alone.
///
/// Named here because this module has to apply the *same* number: "zoom" means
/// one thing in both render modes, and a second constant is how the two come to
/// disagree by a factor nobody notices until they compare two panes side by
/// side. If the plan view's arm ever calls `zoom_speed`, this follows it.
const ZOOM_SPEED: f64 = 2.0;

/// This frame's geography zoom, in zoom **levels**, from whatever device
/// produced one.
///
/// A restatement of `walkers::Map::zoom_delta` — deliberately, and it is the
/// only honest way to get "the same gesture means the same thing": a 3D pane's
/// map is drawn off screen, so `Map::handle_gestures` skips its zoom outright
/// (`ui.ui_contains_pointer()` is false down there) and no amount of
/// configuration will make walkers do this for us. So the arithmetic is
/// restated, once, beside the measurement it feeds — and pinned by
/// `a_scroll_moves_a_3d_pane_the_same_distance_it_moves_a_plan_view`, which
/// drives the two arms through the real UI and compares the answers rather than
/// comparing this function against a copy of itself.
///
/// The two branches are walkers' own, for the `zoom_with_ctrl(false)` the plan
/// view selects: `zoom_delta` carries a pinch or a ctrl-scroll, and a frame
/// with neither — which reports exactly 1.0 — falls back to the raw scroll,
/// scaled by the frame time so a wheel notch is worth the same at any frame
/// rate.
fn zoom_step(input: &egui::InputState) -> f64 {
    let mut delta = f64::from(input.zoom_delta());
    if delta == 1.0 {
        delta = 1.0
            + f64::from(
                input.smooth_scroll_delta.y
                    * input
                        .stable_dt
                        .clamp(input.predicted_dt * 0.5, input.predicted_dt * 2.0),
            ) / 4.0;
    }
    (delta - 1.0) * ZOOM_SPEED
}

/// Apply this frame's scroll or pinch to a 3D pane's own viewport, and say
/// whether it moved.
///
/// The 3D half of "scroll zooms the geography". The plan view gets this from
/// `walkers::Map` for free; a 3D pane cannot, because its map is drawn into an
/// off-screen strip where `ui_contains_pointer` is false — so the gesture is
/// read here, against the pane's *on-screen* rect, and written into the very
/// `MapMemory` the floor and the box are both about to be measured from.
///
/// # Why it must be called before the floor is drawn
///
/// The floor strip and [`region_for_viewport`] both read this `map_memory`, and
/// the whole point of the region being the viewport is that the two are one
/// measurement. Applying the zoom after either of them would put the box one
/// frame behind the floor — which does not look like a bug, it looks like input
/// lag, and it gets "fixed" by turning the sensitivity up.
///
/// # Why the gate is correctness and not politeness
///
/// `Input::zoom_delta` and `smooth_scroll_delta` are **global**: they report the
/// frame's gesture wherever on screen it happened. Without `hovered() ||
/// dragged()` a pinch over a map pane would zoom every 3D pane on screen at
/// once. The topmost-layer check is the same rule `filter_dialog_blocked`
/// applies to clicks — a wheel over the timeline must work the timeline, not
/// the map under it — and its own ground is the `dragged()` arm, which keeps
/// the response resolving after the pointer has wandered onto the chrome.
///
/// # Why the eye does not move
///
/// It has nothing to move for. `OrbitCamera::eye_distance` is in multiples of
/// the box's half-diagonal, so a box that shrinks carries the eye in with it,
/// continuously, at exactly the angle and standoff ratio the user left it at.
/// There is no reframe here and there must never be one: nothing may move the
/// camera but the user.
pub(crate) fn zoom_viewport(
    ui: &egui::Ui,
    response: &egui::Response,
    rect: egui::Rect,
    map_memory: &mut walkers::MapMemory,
    center: walkers::Position,
) -> bool {
    if !(response.hovered() || response.dragged()) || !pointer_on_map_layer(ui.ctx()) {
        return false;
    }
    let step = ui.input(zoom_step);
    if !step.is_finite() || step == 0.0 {
        return false;
    }
    let from = map_memory.zoom();
    let Some((_, half)) = measure_viewport(rect, map_memory, center) else {
        return false;
    };

    // The bound the user asked for, and the only one: the viewport may not be
    // driven past the ground the radar itself covers, in either direction.
    //
    // Ground per point halves with every zoom level, so the two zooms that put
    // the box exactly on the resampler's ceiling and floor are one logarithm
    // away — no search, and nothing that has to be walked back. Each limit is
    // widened to the current zoom (`min`/`max` against `from`) so a pane that
    // arrives already outside the window — restored from a config, or converted
    // from a plan view someone had zoomed to the street — can always move
    // *towards* it and never further out. Clamping it on arrival instead would
    // be the camera moving on its own, which is the one thing this must not do.
    //
    // **The two bounds read different quantities off the box, and each reads
    // the one its own limit is about.** Inward is the *narrow* axis, because
    // that is the one that reaches `MIN_HALF_WIDTH_KM` first and a box with one
    // axis clamped up off the floor would overhang its own floor on that axis.
    // Outward is the **corner** against `MAX_HALF_DIAGONAL_KM`, because the
    // ceiling has always been a bound on the corner — `MAX_HALF_WIDTH_KM` is
    // that same bound written for the one case where the two sides are equal,
    // and a rectangle has no single width for it to be about.
    let inward =
        from + (half.east_km.min(half.north_km) / rustdar_radar::voxel::MIN_HALF_WIDTH_KM).log2();
    let outward = from + (half.corner_km() / rustdar_radar::voxel::MAX_HALF_DIAGONAL_KM).log2();
    let target = (from + step).clamp(outward.min(from), inward.max(from));
    if target == from {
        return false;
    }

    // Measured rather than trusted. The logarithm above is exact for a flat
    // projection and out by a fraction of a percent in Mercator, where the
    // latitudes of the four edges move as the zoom does — and at the tight end
    // that fraction is the difference between a legal box and
    // `region_for_viewport` refusing one, which would drop the pane's box from
    // 10 km straight to the 230 km fallback. So an inward step that lands under
    // the floor is refused whole rather than applied and repaired.
    let mut probe = map_memory.clone();
    if probe.set_zoom(target).is_err() {
        return false;
    }
    let Some((_, after)) = measure_viewport(rect, &probe, center) else {
        return false;
    };
    if target > from && after.east_km.min(after.north_km) < rustdar_radar::voxel::MIN_HALF_WIDTH_KM
    {
        return false;
    }
    *map_memory = probe;
    true
}

/// Whether the pointer is over the map rather than over floating chrome.
///
/// The pane-rect gate alone stopped being enough at the full-bleed flip: pane
/// rects now run under the timeline, the status bar and the layers panel, so
/// "the pointer is over this pane" no longer implies "the pointer is over the
/// *map*". A position covered by any layer above `Background` belongs to that
/// layer, and a wheel there must work the chrome, not the geography under it.
///
/// No pointer at all answers `false`, which is the conservative half of the
/// pair: a frame that cannot say where the gesture happened cannot say it
/// happened over the map.
pub(crate) fn pointer_on_map_layer(ctx: &egui::Context) -> bool {
    ctx.pointer_latest_pos().is_some_and(|pos| {
        !ctx.layer_id_at(pos)
            .is_some_and(|l| l.order > egui::Order::Background)
    })
}

#[cfg(test)]
mod tests;
