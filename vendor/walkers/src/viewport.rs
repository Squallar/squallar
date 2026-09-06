//! Keeping the viewport covered by map.
//!
//! Web Mercator's world is a square `total_pixels(zoom)` points on a side —
//! `2^zoom · 256` — spanning ±180° of longitude and ±85.0511° of latitude, and
//! it is the whole of what there is to draw. A viewport larger than that square
//! on either axis therefore shows points no geography covers, however the map
//! is panned. On the horizontal axis it shows more than one turn of longitude
//! at once as well, which is what puts the same continent on screen twice.
//!
//! Two things follow, and this module is both of them: a **zoom floor**, below
//! which the square is smaller than the viewport, and a **band the centre may
//! be panned within** once the zoom is at or above it.
//!
//! **The band is vertical only, deliberately.** The world wraps east-west and
//! the answer there is to draw the wrap, not to stop the pan; a horizontal
//! clamp would foreclose that. Nothing here bounds the centre's longitude, and
//! `center.rs`'s `horizontal_panning_is_never_clamped` pins that it does not.

use crate::mercator::zoom_for_total_pixels;
use egui::Rect;

/// How far out of the world the centre may sit before it is pulled back, in
/// points.
///
/// Not a tolerance on the geometry — it is what stops the clamp firing on its
/// own arithmetic. The centre is stored as a geographical position, so testing
/// it costs a `project`, and re-reading it next frame costs an `unproject` that
/// does not return the identical `f64`. That round trip is worth ~1e-10 points
/// here, and without a deadband a centre resting exactly on the bound would
/// register as out of bounds every frame, move by a ten-billionth of a point,
/// and `request_repaint` forever — a permanently non-idle map.
///
/// A 64th of a point is four orders of magnitude above that round trip and
/// three below anything a display can resolve, so the void it tolerates is not
/// a void.
///
/// **The other deadband in this workspace** is
/// `squallar-egui/src/overlay_cache.rs`'s `COVERAGE_DEADBAND_TEXELS`, which
/// exists for the same reason — a comparison that would otherwise re-fire on
/// its own rounding forever — and is spelled quite differently, so a reader
/// who has met one should be able to find the other. That one is a deadband in
/// *texels*, converted to ground before the comparison it guards, and a texel
/// is only as fine as the picture it belongs to; it therefore carries a second
/// constant, `COVERAGE_DEADBAND_VIEWPORT_CEILING`, bounding the converted
/// figure to a thousandth of a viewport.
///
/// **This one needs no such ceiling**, and that is the whole of the
/// difference: it is already in the unit the comparison is made in — projected
/// points, the units `center.rs` tests the clamp in — and it is a fixed
/// constant rather than something derived per picture, so there is no
/// conversion for a viewport to make coarse. A 64th of a point is a 64th of a
/// point on every map this widget draws.
pub(crate) const CENTER_SLACK_POINTS: f64 = 1.0 / 64.0;

/// The shallowest zoom at which the world is no smaller than `rect`.
///
/// `log2(max(width, height) / 256)`. The **larger** side is what decides,
/// because the world is square: clamping on the width alone leaves void above
/// and below any viewport taller than it is wide.
///
/// Deliberately unclamped to any zoom range, and total. A viewport under one
/// tile across gives a negative answer and an empty one gives `-inf`; both are
/// the honest "the world is already big enough", and leaving them alone is what
/// lets [`clamp_zoom`] be a plain comparison instead of a special case. A
/// viewport with a `NaN` side gives `NaN`, which [`clamp_zoom`] refuses to act
/// on.
pub fn min_zoom(rect: Rect) -> f64 {
    zoom_for_total_pixels(f64::from(rect.width().max(rect.height())))
}

/// `zoom`, raised to [`min_zoom`] if it is below it.
///
/// Only ever raises. Nothing here bounds how far in a map may be zoomed.
pub fn clamp_zoom(zoom: f64, rect: Rect) -> f64 {
    let floor = min_zoom(rect);
    if zoom.is_nan() || floor.is_nan() || zoom >= floor {
        zoom
    } else {
        floor
    }
}

/// The band the map centre's projected `y` may lie in for a viewport `height`
/// points tall to stay inside a world `world` points across.
///
/// The bounds meet, and then cross, as the world approaches the viewport's
/// height — and it does cross, at the floor itself: `2f64.powf(x.log2())` is
/// not exactly `x`, so at [`min_zoom`] the world comes out a few hundred
/// femtopoints *short* of the side it was solved for. `(1439.0, 1438.999…)` is
/// the real answer for the viewport this was built for, and a `f64::clamp`
/// handed those two panics. [`clamp_center_y`] is what resolves it.
pub(crate) fn center_y_bounds(height: f64, world: f64) -> (f64, f64) {
    let half = height / 2.0;
    (half, world - half)
}

/// `y`, moved into [`center_y_bounds`].
///
/// A world that cannot contain the viewport — which, past the floor, means one
/// short of it by the rounding described above — is centred rather than
/// clamped, because there is no position that satisfies both bounds and the
/// midpoint is the one that misses each by the same amount.
pub(crate) fn clamp_center_y(y: f64, height: f64, world: f64) -> f64 {
    let (lo, hi) = center_y_bounds(height, world);
    if lo.is_nan() || hi.is_nan() || hi < lo {
        return world / 2.0;
    }
    y.clamp(lo, hi)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mercator::total_pixels;

    /// The pane the map was frozen in when this was reported, in points.
    const REAL_VIEWPORT: (f32, f32) = (2878.0, 1651.0);

    /// How far under the viewport's larger side the world at the floor is
    /// allowed to land, relative.
    ///
    /// Not slack in the geometry — it is `2f64.powf(x.log2())` not being
    /// exactly `x`, which no spelling of a floor as a `log2` can avoid. The
    /// worst case over the 2008 viewports below is printed by
    /// [`at_the_floor_the_world_is_the_viewport`] and pinned by it: this bound
    /// exists to be a few ulps and not a free pass. **Measured 2026-09-06:
    /// 1.72 ulps**, so it is a bound with room in it rather than one fitted to
    /// the run.
    const FLOOR_ROUNDING: f64 = 4.0 * f64::EPSILON;

    /// `log2(2878 / 256)`, to the bit. The floor for [`REAL_VIEWPORT`], and the
    /// value the whole of this module is arithmetic around.
    const REAL_FLOOR: f64 = 3.4908508767402977;

    fn rect(width: f32, height: f32) -> Rect {
        Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(width, height))
    }

    /// A deterministic spread, so that the property tests below run over the
    /// same several thousand cases on every machine.
    fn xorshift(state: &mut u64) -> u64 {
        *state ^= *state << 13;
        *state ^= *state >> 7;
        *state ^= *state << 17;
        *state
    }

    fn viewports() -> Vec<Rect> {
        let mut out = vec![
            rect(REAL_VIEWPORT.0, REAL_VIEWPORT.1),
            rect(1.0, 1.0),
            rect(256.0, 256.0),
            rect(255.0, 257.0),
            rect(1920.0, 1080.0),
            rect(1080.0, 1920.0),
            rect(3840.0, 2160.0),
            rect(390.0, 844.0),
        ];
        let mut state = 0x5171_5150_1234_9abcu64;
        for _ in 0..2000 {
            let w = (xorshift(&mut state) % 8192 + 1) as f32;
            let h = (xorshift(&mut state) % 8192 + 1) as f32;
            out.push(rect(w, h));
        }
        out
    }

    /// **Gate 1.** The floor is `log2(max(w, h) / 256)`, and it is the *larger*
    /// side that binds.
    ///
    /// The pin is the real pane this was reported from, to the bit; the tall
    /// rect beside it is what separates `max` from `width`, which agree on
    /// every landscape viewport anybody would have tried by hand.
    #[test]
    fn the_floor_is_log2_of_the_larger_side_over_one_tile() {
        let real = rect(REAL_VIEWPORT.0, REAL_VIEWPORT.1);
        assert_eq!(min_zoom(real).to_bits(), REAL_FLOOR.to_bits());
        assert_eq!(
            min_zoom(real).to_bits(),
            (f64::from(REAL_VIEWPORT.0) / 256.0).log2().to_bits()
        );

        // Taller than it is wide: the height decides, and the width's own
        // answer is more than one and a half zoom levels too shallow.
        let tall = rect(900.0, 1600.0);
        assert_eq!(
            min_zoom(tall).to_bits(),
            (1600.0f64 / 256.0).log2().to_bits()
        );
        assert!(
            min_zoom(tall) - (900.0f64 / 256.0).log2() > 0.8,
            "the width's floor would be {}, the height's is {}",
            (900.0f64 / 256.0).log2(),
            min_zoom(tall)
        );

        // Both orientations of the same box give the same floor.
        for r in viewports() {
            let flipped = rect(r.height(), r.width());
            assert_eq!(min_zoom(r).to_bits(), min_zoom(flipped).to_bits());
        }
    }

    /// **Gate 2.** At the floor the world is the viewport's larger side, to
    /// within `f64::EPSILON` of it.
    ///
    /// It is not *exactly* it, and the direction matters: `2f64.powf(x.log2())`
    /// lands a hair **under** `x` for the pane this was built for, which is why
    /// [`clamp_center_y`] has a crossed-bounds branch at all. The deficit is
    /// pinned here so that a future change to either function has to say so.
    #[test]
    fn at_the_floor_the_world_is_the_viewport() {
        let real = rect(REAL_VIEWPORT.0, REAL_VIEWPORT.1);
        let world = total_pixels(min_zoom(real));
        assert_eq!(world.to_bits(), 2877.999_999_999_999_5f64.to_bits());
        assert!(
            world < f64::from(REAL_VIEWPORT.0),
            "the deficit's direction is what the crossed-bounds branch exists for"
        );

        let mut worst = 0.0f64;
        for r in viewports() {
            let side = f64::from(r.width().max(r.height()));
            let world = total_pixels(min_zoom(r));
            worst = worst.max((world - side).abs() / (side * f64::EPSILON));
            assert!(
                (world - side).abs() <= side * FLOOR_ROUNDING,
                "at the floor for {r:?} the world is {world}, not {side}"
            );
        }
        println!("worst floor rounding: {worst} ulps");
        assert!(
            worst > 0.0,
            "no case rounded at all, so the tolerance is measuring nothing"
        );
    }

    /// **Gate 3.** One ulp below the floor is raised to exactly the floor; one
    /// ulp above is returned untouched.
    ///
    /// The failure this exists for is a clamp that is right at 3.49 and wrong
    /// at 3.4909 — a tolerance bolted on to the comparison, or a floor rounded
    /// to a whole zoom level. Both are invisible to any test that only tries
    /// zooms somebody picked.
    #[test]
    fn the_boundary_is_exact_to_one_ulp() {
        for r in viewports() {
            let floor = min_zoom(r);
            if !floor.is_finite() {
                continue;
            }

            let below = floor.next_down();
            let above = floor.next_up();

            assert_eq!(
                clamp_zoom(floor, r).to_bits(),
                floor.to_bits(),
                "the floor itself moved, for {r:?}"
            );
            assert_eq!(
                clamp_zoom(below, r).to_bits(),
                floor.to_bits(),
                "one ulp below the floor was let through, for {r:?}"
            );
            assert_eq!(
                clamp_zoom(above, r).to_bits(),
                above.to_bits(),
                "one ulp above the floor was moved, for {r:?}"
            );
        }
    }

    /// **Gate 7.** Whatever the viewport and whatever the zoom, the world after
    /// clamping is at least as large as the viewport's larger side.
    ///
    /// "At least" carries the rounding of gate 2 with it: the comparison is
    /// against `side - side·EPSILON`, because the floor's own world is that
    /// much under `side` and no implementation spelled `log2` can do better.
    #[test]
    fn a_clamped_zoom_always_covers_the_viewport() {
        let mut state = 0x9e37_79b9_7f4a_7c15u64;
        let mut clamped_at_least_once = 0usize;

        for r in viewports() {
            let side = f64::from(r.width().max(r.height()));
            let mut zooms = vec![0.0, 26.0, min_zoom(r), 3.326_757_482_253_017];
            for _ in 0..12 {
                zooms.push((xorshift(&mut state) % 27_000) as f64 / 1000.0);
            }

            for zoom in zooms {
                if !(0.0..=26.0).contains(&zoom) {
                    continue;
                }
                let after = clamp_zoom(zoom, r);
                if after != zoom {
                    clamped_at_least_once += 1;
                }
                // A world of 256·2^26 points is 1.7e10, so the only viewports
                // whose floor is out of walkers' zoom range are ones that
                // cannot exist. Nothing here has to give up.
                assert!(
                    total_pixels(after) >= side - side * FLOOR_ROUNDING,
                    "at zoom {zoom} -> {after} the world is {} for a {side}-point viewport",
                    total_pixels(after)
                );
            }
        }

        // The control: a property that is never exercised is not a property.
        assert!(
            clamped_at_least_once > 100,
            "only {clamped_at_least_once} of the cases actually needed clamping"
        );
    }

    /// A viewport smaller than a single tile is already covered at zoom 0, and
    /// the floor says so by being negative rather than by being special-cased.
    #[test]
    fn a_viewport_under_one_tile_needs_no_floor() {
        let tiny = rect(120.0, 90.0);
        assert!(min_zoom(tiny) < 0.0);
        assert_eq!(clamp_zoom(0.0, tiny).to_bits(), 0.0f64.to_bits());

        // And an empty one, which is what a collapsed pane hands the widget.
        let empty = rect(0.0, 0.0);
        assert_eq!(min_zoom(empty), f64::NEG_INFINITY);
        assert_eq!(clamp_zoom(0.0, empty).to_bits(), 0.0f64.to_bits());
    }

    /// A `NaN` side cannot say anything about coverage, so it does not move the
    /// zoom. `f64::clamp` would have propagated it into the map's state.
    #[test]
    fn a_nan_viewport_moves_nothing() {
        let nan = rect(f32::NAN, f32::NAN);
        assert!(min_zoom(nan).is_nan());
        assert_eq!(clamp_zoom(7.5, nan).to_bits(), 7.5f64.to_bits());
    }

    /// The centre band keeps both viewport edges inside the world.
    #[test]
    fn the_centre_band_keeps_both_edges_inside_the_world() {
        let height = 1651.0;
        let world = total_pixels(6.0);
        let (lo, hi) = center_y_bounds(height, world);
        assert_eq!(lo, height / 2.0);
        assert_eq!(hi, world - height / 2.0);

        for y in [-5000.0, 0.0, lo, world / 2.0, hi, world, 99_999.0] {
            let y = clamp_center_y(y, height, world);
            assert!(
                y - height / 2.0 >= -f64::EPSILON,
                "top edge at {}",
                y - height / 2.0
            );
            assert!(
                y + height / 2.0 <= world + f64::EPSILON,
                "bottom edge at {}",
                y + height / 2.0
            );
        }
    }

    /// At the floor the bounds cross, by the femtopoint of gate 2. The centre
    /// is the answer, and `f64::clamp` is not — it panics on `min > max`.
    #[test]
    fn a_world_a_hair_short_of_the_viewport_is_centred_not_clamped() {
        let real = rect(REAL_VIEWPORT.0, REAL_VIEWPORT.1);
        let world = total_pixels(min_zoom(real));
        let height = f64::from(REAL_VIEWPORT.1);

        // The height is the short side here, so these do not cross...
        let (lo, hi) = center_y_bounds(height, world);
        assert!(lo < hi);

        // ...but a square pane at its own floor is exactly the crossing case.
        let square = f64::from(REAL_VIEWPORT.0);
        let world = total_pixels(min_zoom(rect(REAL_VIEWPORT.0, REAL_VIEWPORT.0)));
        let (lo, hi) = center_y_bounds(square, world);
        assert!(hi < lo, "expected crossed bounds, got {lo} and {hi}");
        assert_eq!(clamp_center_y(0.0, square, world), world / 2.0);
        assert_eq!(clamp_center_y(1e9, square, world), world / 2.0);
    }
}
