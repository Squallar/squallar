//! What a 3D pane's stored region implies: the viewport its floor is drawn
//! through, and the zoom gesture that must not touch either.
//!
//! Both halves run **from** the region rather than towards it, and that is the
//! whole of what this module is now. [`viewport_for_region`] frames the floor
//! strip on the box; [`zoom_camera`] spends the wheel on the eye. Nothing here
//! writes a region.
//!
//! # The region is stored, and this module does not write it
//!
//! A 3D pane resamples a stored patch of ground: the volume's own data reach,
//! circumscribed — the whole ring — or a smaller region the user picked. It
//! changes when the site, the product, the reach or the selection changes, and
//! at no other time. Not on a zoom, not on a pan, not on a divider drag, not on
//! a window resize. See [`crate::pane::VolumeRegion`], which is where it now
//! lives, and [`rustdar_radar::voxel::box_half_width_km`], which answers the
//! unselected case from the reach.
//!
//! It used to be **derived**, every frame, from the pane's own viewport, and
//! that is the defect this module was rebuilt to remove. Deriving it made the
//! gesture that frames the picture also re-cut the data under it, so the ground
//! the pane covered shrank as the user zoomed in — reported three times:
//!
//! > the goddamn 3d viewer still covers less and less 3d geometry
//!
//! > The 3d viewer's region should CAP at either the size of the data in the
//! > radar scan, or the region selected if the user did that. That region (the
//! > selector OR the radar's ring) must never change. Zooming should keep the
//! > rest of the region around and merely zoom into what's already there.
//!
//! On the reported session the pane's own caption stated the loss: `802 × 490
//! km box` as opened, `668 × 408 km box` after a zoom. That is
//! Chattanooga-to-Jacksonville reduced to Dalton-to-Wilmington. The box was not
//! *cropped* by the zoom — which would at least have been a visible edge — it
//! was rebuilt smaller, so the storms outside the new box stopped being
//! resampled at all and the picture lost them silently.
//!
//! # What the gesture does instead
//!
//! It divides [`OrbitCamera::eye_distance`](crate::pane::OrbitCamera::eye_distance).
//! That value is a multiple of the box's framing radius, so halving it halves
//! the ground the pane is *looking at* while the box, the grid inside it and
//! the floor under it all stand exactly still. The rest of the region stays
//! around, off the edges of the frame, which is the plain reading of the ask.
//!
//! Nothing else in the pane is touched. In particular the pane's own
//! `walkers::MapMemory` is left alone, where the old arm wrote it on every
//! gesture frame: it is shared with the pane's plan view, so writing it here
//! also meant that flipping a pane to 3D, scrolling, and flipping back moved
//! the map the user had aimed.
//!
//! # Why the bound is the camera's and not the box's
//!
//! The old arm clamped the gesture against
//! [`rustdar_radar::voxel::MIN_HALF_WIDTH_KM`] and
//! [`rustdar_radar::voxel::MAX_HALF_DIAGONAL_KM`], because the gesture was
//! resizing the thing those constants bound. It no longer is, so the only
//! bound left is the one the camera has always carried:
//! [`crate::pane::MIN_EYE_DISTANCE`]`..=`[`crate::pane::MAX_EYE_DISTANCE`],
//! 0.05 to 8.0, applied by
//! [`OrbitCamera::nudge`](crate::pane::OrbitCamera::nudge). That is a range of
//! 160×, or **7.32 zoom levels**, from inside the box to well outside it — and
//! it is the honest bound, because what the gesture now runs out of is somewhere
//! to put the eye rather than ground to resample.
//!
//! It also removes the re-measure probe the old arm needed. That probe existed
//! because the box's floor was reached by a logarithm that is exact in a flat
//! projection and out by a fraction of a percent in Mercator, and at the tight
//! end that fraction decided whether a box was legal at all. A clamp on a
//! camera ratio has no projection in it and nothing to verify.

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
/// restated, once, beside the gesture it feeds — and pinned by
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

/// This frame's scroll or pinch as a multiplicative dolly for
/// [`OrbitDelta::zoom_factor`](crate::pane::OrbitDelta::zoom_factor), or `1.0`
/// — "the eye did not move" — for a frame with no gesture for this pane.
///
/// # Why the factor is two to the power of the step
///
/// [`zoom_step`] answers in Web Mercator **zoom levels**, and one level is a
/// factor of two of ground per point by the projection's own definition. The
/// ground a perspective camera sees at its pivot plane is `2 · d · tan(fov/2)`
/// — linear in the standoff `d` at a fixed field of view — so one level is one
/// halving of `eye_distance`, exactly. That is what makes a wheel notch on a 3D
/// pane cover the same ground it covers on the plan view beside it, and it is
/// derived from the two definitions rather than tuned to feel similar.
///
/// The neutral value is 1.0 rather than 0.0 because this is a *ratio*: it is
/// what `nudge` divides the standoff by, and `2^0 = 1` is the identity a frame
/// with no gesture must produce.
///
/// # Why the gate is correctness and not politeness
///
/// `Input::zoom_delta` and `smooth_scroll_delta` are **global**: they report the
/// frame's gesture wherever on screen it happened. Without `hovered() ||
/// dragged()` a pinch over a map pane would dolly every 3D pane on screen at
/// once. The topmost-layer check is the same rule `filter_dialog_blocked`
/// applies to clicks — a wheel over the timeline must work the timeline, not
/// the map under it — and its own ground is the `dragged()` arm, which keeps
/// the response resolving after the pointer has wandered onto the chrome.
///
/// # Why a non-finite step is refused rather than clamped
///
/// [`OrbitCamera::nudge`](crate::pane::OrbitCamera::nudge) refuses a whole
/// delta whose factor is not finite and positive, which is the right answer but
/// discards the frame's orbit and pan with it. Answering 1.0 here keeps those
/// two verbs working on a frame whose scroll arrived as a NaN — and a `2^step`
/// that overflowed to infinity is the one arithmetic step between `zoom_step`'s
/// own finiteness and the camera's.
pub(crate) fn zoom_camera(ctx: &egui::Context, response: &egui::Response) -> f32 {
    if !(response.hovered() || response.dragged()) || !pointer_on_map_layer(ctx) {
        return 1.0;
    }
    dolly_for_step(ctx.input(zoom_step))
}

/// [`zoom_camera`]'s arithmetic, without the gate: zoom levels in, a standoff
/// divisor out.
///
/// Split from the gate because the two fail in different ways and only one of
/// them can be tested without a live `egui::Context` — this half is the one
/// that has to answer 1.0 for every input a gesture can produce that the camera
/// would refuse, and it is exhaustively checkable as a function.
fn dolly_for_step(step: f64) -> f32 {
    if !step.is_finite() || step == 0.0 {
        return 1.0;
    }
    let factor = step.exp2() as f32;
    if factor.is_finite() && factor > 0.0 {
        factor
    } else {
        1.0
    }
}

/// How many passes [`viewport_for_region`] gets to settle.
///
/// The east–west lane converges in **one**: walkers' points per degree of
/// longitude is exactly `tile_size · 2^zoom / 360`, so the ratio of what is
/// covered to what is wanted is exactly a power of two away and the logarithm
/// lands on it. The north–south lane does not, because Web Mercator's scale
/// varies with latitude and the latitudes of the rect's own top and bottom edges
/// move as the zoom does — so the second pass measures a projection the first
/// pass changed.
///
/// Measured on the worst shape this application can make — a 920 km box in a
/// 450 × 900 point strip at 64.8°N, where Mercator bends most between a rect's
/// centre and its poleward edge — the shortfall runs **1.00098, 1.0000019,
/// 1.0000000038**. So pass two is already inside [`COVERAGE_MARGIN`] and the
/// loop stops there; four is that with two passes to spare, for a latitude or a
/// pane shape nobody has thought of.
const MAX_FRAMING_PASSES: usize = 4;

/// How much more than the box the framing deliberately covers, as a fraction of
/// it.
///
/// **Not slack, and not a fudge — it is the direction the solve is wrong in.**
/// The single-logarithm step is exact for the east–west lane and asymptotic for
/// the north–south one, and measurement says it approaches from *above*: at
/// 64.8°N in a 450 × 900 point strip the shortfall runs 1.00098, 1.0000019,
/// 1.0000000038 — never crossing 1.0, so a solve aimed at exact coverage lands
/// fractionally **short** on every pass it will ever take. Short is the one
/// failure this function exists to prevent: `floor_colour` answers transparent
/// off the mirror, so a hairline short is a hairline of the volume standing on
/// nothing, right where the box's edge is.
///
/// So the target is the box plus this, and the loop converges onto a viewport
/// that covers it. 0.1% of a 920 km box is 920 m — under a third of one cell at
/// the shipped grid's 3.6 km — which is the price of never being short.
const COVERAGE_MARGIN: f64 = 0.001;

/// A map viewport that frames `rect` on the box of `half` about `centre` —
/// **the inverse of the measurement this module used to make**.
///
/// The floor strip is a real `walkers::Map` drawn off screen, and the volume
/// shader samples it as the ground under the box. So the strip has to be showing
/// the box: `floor_hit` clips the floor to the box's own bottom face, and
/// `floor_colour` clips it again to the mirror's `0..1`, transparent outside
/// rather than clamped. Those two rectangles used to be the same one because the
/// box *was* the viewport. Now the box is stored, so the viewport is derived —
/// the same coupling, with the causality the user asked for:
///
/// > the "stage"/"floor" should never get smaller (in real-world geography
/// > terms) / nor the data above it ofc
///
/// # Why the whole box, rather than the part of it on screen
///
/// **Because "the part on screen" is not expressible here.** `floor_colour` maps
/// the box's own unit square through `floor_geo` and `box_size_km` and samples
/// the mirror with the result; nothing in the uniform says where the camera is.
/// Sizing the mirror to the visible part would mean putting the camera in it and
/// re-rendering the strip on every frame the eye moved — a mirror re-render per
/// gesture frame, to save ground that costs nothing to carry.
///
/// And at the standoffs a pane is usually at there would be nothing to save. The
/// far edge of the frustum meets the ground this many box **half-widths** past
/// the box's centre — a ratio that depends only on the standoff and the pitch,
/// not on how big the box is:
///
/// | `eye_distance` | pitch 25° (default) | 45° | 89° (`MAX_PITCH_DEG`) |
/// |---|---|---|---|
/// | 0.05 (`MIN_EYE_DISTANCE`) | 0.28 | 0.06 | 0.03 |
/// | 1.94276 (a pane opens here) | 10.78 | 2.22 | 1.01 |
/// | 8.0 (`MAX_EYE_DISTANCE`) | 44.40 | 9.16 | 4.14 |
///
/// Below 20° — half the 40° vertical field of view — the ray never meets the
/// ground at all and the entry is unbounded.
///
/// So the box's edge *is* off screen at the tight end, and that is new: the zoom
/// gesture now moves the eye, where it used to shrink the box. It changes
/// nothing here. The mirror is sampled in box space whatever the camera is
/// doing, so a strip that covers less than the box is a strip with transparent
/// ground in it, waiting for the first orbit or dolly that brings that part of
/// the box into frame.
///
/// # Why it is measured rather than computed from the zoom
///
/// `walkers::Projector` is the projection the strip is actually drawn in, and
/// going through it rather than restating `tile_size · 2^zoom / 360` is what
/// keeps the two the same: a tile size or zoom convention that moves in
/// `walkers` moves both together. The passes are what that costs, and
/// [`MAX_FRAMING_PASSES`] measures it.
///
/// The result is deliberately *tight* — it stops as soon as the strip covers the
/// box rather than leaving it wide — because the mirror is a fixed number of
/// pixels, so every kilometre of ground outside the box is floor resolution
/// spent on ground the box will clip away.
///
/// `None` for a rect with no area, an extent that is not finite and positive, or
/// a zoom outside `walkers`' own range. The caller falls back to the pane's own
/// map memory, which is what the strip was always drawn through.
pub(crate) fn viewport_for_region(
    rect: egui::Rect,
    centre: walkers::Position,
    half: rustdar_radar::voxel::HalfExtentKm,
) -> Option<walkers::MapMemory> {
    if !(rect.width() > 0.0 && rect.height() > 0.0) {
        return None;
    }
    if !(half.is_finite() && half.east_km > 0.0 && half.north_km > 0.0) {
        return None;
    }

    // The box plus the margin: what the strip is actually solved onto. See
    // `COVERAGE_MARGIN` — the solve approaches from above, so aiming at the box
    // itself lands short of it every time.
    let want = rustdar_radar::voxel::HalfExtentKm {
        east_km: half.east_km * (1.0 + COVERAGE_MARGIN),
        north_km: half.north_km * (1.0 + COVERAGE_MARGIN),
    };

    let mut memory = walkers::MapMemory::default();
    for _ in 0..MAX_FRAMING_PASSES {
        let covered = ground_half_extent(rect, &memory, centre)?;
        // How much wider the target is than what the strip currently shows, on
        // whichever axis is the binding one. Above 1.0 the strip is short.
        let shortfall = (want.east_km / covered.east_km).max(want.north_km / covered.north_km);
        if !shortfall.is_finite() || shortfall <= 0.0 {
            return None;
        }
        // Settled once the strip covers the **box** — the margin is what is
        // being converged through, not what has to be reached exactly.
        if shortfall <= 1.0 {
            return Some(memory);
        }
        // Ground per point halves with every zoom level, so the zoom that
        // covers the target is one logarithm away rather than a search.
        memory.set_zoom(memory.zoom() - shortfall.log2()).ok()?;
    }
    // Out of passes, and by construction still inside the margin rather than
    // short of the box: every pass moves outward and the first one already
    // clears the box itself. Answered rather than refused, because the
    // alternative is the pane's own map memory, which is not framed on the box
    // at all.
    Some(memory)
}

/// The ground `rect` covers either side of `centre`, kilometres on each
/// horizontal axis, through the projection the strip is drawn in.
///
/// **Each axis takes the nearer of its own two edges.** Mercator makes the
/// poleward edge the near one, so the north–south lane is governed by the top
/// edge in the northern hemisphere and the bottom in the southern — and the
/// near edge is the binding one for *coverage*, because a box that reaches past
/// it is a box with a transparent strip along that side.
///
/// [`rustdar_radar::beam::site_bearing_range_km`] rather than a flat
/// approximation, because it is the codebase's real geodesy and the same
/// function the resampler places the box's own corners with.
fn ground_half_extent(
    rect: egui::Rect,
    map_memory: &walkers::MapMemory,
    centre: walkers::Position,
) -> Option<rustdar_radar::voxel::HalfExtentKm> {
    let projector = walkers::Projector::new(rect, map_memory, centre);
    let ground_km = |pos: egui::Pos2| {
        let point = projector.unproject(pos.to_vec2());
        let (_, range_km) = rustdar_radar::beam::site_bearing_range_km(
            centre.y(),
            centre.x(),
            point.y(),
            point.x(),
        );
        (range_km.is_finite() && range_km > 0.0).then_some(range_km)
    };
    Some(rustdar_radar::voxel::HalfExtentKm {
        east_km: ground_km(egui::pos2(rect.left(), rect.center().y))?
            .min(ground_km(egui::pos2(rect.right(), rect.center().y))?),
        north_km: ground_km(egui::pos2(rect.center().x, rect.top()))?
            .min(ground_km(egui::pos2(rect.center().x, rect.bottom()))?),
    })
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
