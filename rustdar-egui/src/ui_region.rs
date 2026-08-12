//! A 3D pane's zoom gesture, which moves the **eye** and nothing else.
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
