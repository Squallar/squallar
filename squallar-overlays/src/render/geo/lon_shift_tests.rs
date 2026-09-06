//! [`lon_shift`]'s ceiling, at the widths real datums reach.
//!
//! The rule was "a datum spanning a half-turn or more gets no shift", and while
//! the viewport could not exceed the world nothing legitimate was wider.
//! `bc25271f` wrapped the map: the viewport spans up to a turn at the zoom
//! floor, a seam-cut zone pools to a turn wide, a ring spanning exactly a
//! half-turn is real input. The ceiling is a whole turn plus [`TURN_SLACK`]
//! now, and these are the widths it is decided at.
//!
//! No caller hands `lon_shift` the *viewport* as a datum — every datum in the
//! tree is a point, a ring's extent or a feature's pooled extent, and the
//! viewport is always the target. The floor is read here because it is the one
//! place the arithmetic is known to land either side of 360 by rounding alone,
//! which is what sizes the slack.

use super::{TURN_SLACK, box_lon_shift, lon_into_bounds, lon_shift};
use squallar_geo::GeoBounds;

/// `walkers::mercator::unproject_at_scale`, the longitude half, verbatim:
/// linear in x, no fold.
fn unproject_lon(px: f64, total: f64) -> f64 {
    (((px / total) * 2.0 - 1.0) * std::f64::consts::PI).to_degrees()
}

/// `walkers::mercator::project_at_scale`, the x half, verbatim.
fn project_x(lon: f64, total: f64) -> f64 {
    (1.0 + (lon.to_radians() / std::f64::consts::PI)) / 2.0 * total
}

/// The two longitudes a pane `w` px wide sees at `zoom`, centred on `centre` —
/// what `viewport_geo_bounds` reads off `Projector::unproject` at the pane's
/// two corners.
fn pane_edges(w: f64, zoom: f64, centre: f64) -> (f64, f64) {
    let total = 2f64.powf(zoom) * 256.0;
    let cx = project_x(centre, total);
    (
        unproject_lon(cx - w / 2.0, total),
        unproject_lon(cx + w / 2.0, total),
    )
}

/// The zoom floor for a pane `w` px wide: the world exactly as wide as the
/// pane. `walkers` solves it with `log2`; the edges are read back through
/// `powf`, and the two are not exact inverses.
fn floor_zoom(w: f64) -> f64 {
    (w / 256.0).log2()
}

/// **At the floor the viewport measures a turn to within rounding, on both
/// sides of 360, and the ceiling admits every arm.** Two pane widths that have
/// been measured — 1920, the walkers lane's; 2878, the seam probe's — at the
/// floor and one ulp of zoom either side, over three centres. A datum that
/// wide, seen from a box a turn east, must be carried by a whole turn.
#[test]
fn the_floor_viewport_is_a_turn_wide_to_within_the_slack_on_either_side_of_360() {
    let ulp_of_360 = 360f64.next_up() - 360.0;
    let (mut over, mut under, mut widest) = (0usize, 0usize, 0.0f64);
    for w in [1920.0, 2878.0] {
        let z = floor_zoom(w);
        for zoom in [z, z.next_up(), z.next_down()] {
            for centre in [180.0, 0.0, -86.783_624_466_817_76] {
                let (a, b) = pane_edges(w, zoom, centre);
                let span = b - a;
                let excess = span - 360.0;
                println!(
                    "pane {w} zoom {zoom:?} centre {centre}: edges ({a:?}, {b:?}) \
                     span {span:?} excess {excess:+.3e} ({:+.1} ulps of 360)",
                    excess / ulp_of_360
                );
                assert!(
                    excess.abs() <= TURN_SLACK,
                    "a {w} px pane at zoom {zoom:?} spans {span:?}, {excess:+.3e} from a \
                     turn, outside the slack of {TURN_SLACK:e}"
                );
                widest = widest.max(excess);
                if excess > 0.0 {
                    over += 1;
                } else {
                    under += 1;
                }
                // Carried: from a box a turn east, this datum moves a whole turn.
                let (t0, t1) = (centre + 350.0, centre + 370.0);
                assert_eq!(
                    lon_shift(a, b, t0, t1),
                    360.0,
                    "a datum {span:?} wide must be carried toward a box a turn east"
                );
                // And from a box over it, it stays.
                assert_eq!(lon_shift(a, b, centre - 10.0, centre + 10.0), 0.0);
            }
        }
    }
    assert!(
        over >= 1 && under >= 1,
        "the excess must change sign across the arms or the slack's two-sidedness is \
         untested: {over} arms over 360, {under} under"
    );
    assert!(
        widest > 0.0,
        "no arm landed over 360, so a bare `> 360.0` ceiling would pass here and the slack \
         is unproven"
    );
    assert!(
        widest <= TURN_SLACK / 2.0,
        "the largest excess is {widest:.3e} deg ({:.1} ulps of 360); the slack must clear \
         it by a margin, not a hair",
        widest / ulp_of_360
    );
}

/// A ring covering the eastern hemisphere, `0..180`, exactly a half-turn: the
/// old ceiling refused it. Seen from a pane panned west past the seam to
/// `-200..-170`, its eastern edge belongs at `-180` and it is carried there.
#[test]
fn a_datum_spanning_exactly_a_half_turn_is_carried() {
    assert_eq!(lon_shift(0.0, 180.0, -200.0, -170.0), -360.0);
    assert_eq!(
        lon_shift(0.0, 180.0, 80.0, 100.0),
        0.0,
        "and from a pane looking straight at it, it stays"
    );
}

/// A zone the source cut at the seam pools to a turn wide — `PKZ784` measures
/// `-179.9999..180.0`. From a pane panned east past the seam to `181..200` the
/// copy nearest the box starts at `180`, which is where its western piece
/// belongs; left in ±180, the pooled extent meets nothing and the feature is
/// culled whole.
#[test]
fn a_seam_cut_pool_a_whole_turn_wide_is_carried_to_the_box() {
    assert_eq!(lon_shift(-179.9999, 180.0, 181.0, 200.0), 360.0);
    assert_eq!(lon_shift(-180.0, 180.0, 181.0, 200.0), 360.0);

    let pooled = GeoBounds {
        min_lat: 50.0,
        max_lat: 54.0,
        min_lon: -180.0,
        max_lon: 180.0,
    };
    let past = GeoBounds {
        min_lat: 50.0,
        max_lat: 54.0,
        min_lon: 181.0,
        max_lon: 200.0,
    };
    assert!(
        !pooled.intersects(&past),
        "non-triviality: unshifted, the pool misses a box past the seam"
    );
    let shift = lon_shift(pooled.min_lon, pooled.max_lon, past.min_lon, past.max_lon);
    let carried = GeoBounds {
        min_lon: pooled.min_lon + shift,
        max_lon: pooled.max_lon + shift,
        ..pooled
    };
    assert!(carried.intersects(&past));
}

/// The ceiling is a turn plus the slack: a datum that wide is carried, one ulp
/// wider is not. The target sits a turn east so a carried datum answers `360`
/// and a refused one `0`.
#[test]
fn the_ceiling_is_a_turn_plus_the_slack_and_not_an_ulp_more() {
    let at = |span: f64| lon_shift(0.0, span, 370.0, 390.0);
    assert_eq!(at(0.0), 360.0, "a point");
    assert_eq!(at(180.0), 360.0, "a half-turn");
    assert_eq!(at(360.0), 360.0, "a whole turn");
    assert_eq!(at(360.0 + TURN_SLACK), 360.0, "a turn plus the slack");
    assert_eq!(
        at((360.0 + TURN_SLACK).next_up()),
        0.0,
        "one ulp past the slack is not a width"
    );
    assert_eq!(at(361.0), 0.0);
    assert_eq!(at(f64::INFINITY), 0.0);
    assert_eq!(at(f64::NAN), 0.0);
}

/// The hover cull's fold: a grid written past the seam at `185..195` and the
/// three pointers the readout has to answer. `-170` is the same ground as
/// `190`, written in ±180; `0` is half a world away in either spelling.
#[test]
fn a_pointer_is_carried_into_the_grids_own_frame() {
    let grid = GeoBounds {
        min_lat: 30.0,
        max_lat: 40.0,
        min_lon: 185.0,
        max_lon: 195.0,
    };
    let hits = |lon: f64| grid.contains_point(35.0, lon_into_bounds(lon, &grid));
    assert!(hits(190.0), "the pointer in the grid's own frame");
    assert!(hits(-170.0), "the same ground, the pointer in +/-180");
    assert!(!hits(0.0), "half a world away in either spelling");
    assert!(
        !grid.contains_point(35.0, -170.0),
        "non-triviality: the unfolded cull refuses -170"
    );

    // The whole mosaic written in ±180, and a pointer past the seam.
    let world = GeoBounds {
        min_lat: -72.7,
        max_lat: 72.7,
        min_lon: -180.0,
        max_lon: 180.0,
    };
    assert_eq!(lon_into_bounds(190.0, &world), -170.0);
    assert_eq!(lon_into_bounds(-170.0, &world), -170.0);
    assert_eq!(lon_into_bounds(-97.0, &world), -97.0, "in frame, untouched");
}

/// The projection window's fold: a **box** is carried to its grid's frame at
/// any width. MRMS's frame is `-129.995..-60.005`; the ground under it written
/// a turn up comes back a turn, in frame it stays, and the zoom floor grown by
/// the overdraw — a turn and a half — is carried where [`lon_shift`]'s
/// ceiling would leave it a turn away from the grid.
#[test]
fn a_box_is_carried_into_the_grids_frame_at_any_width() {
    let (west, east) = (-129.995, -60.005);
    assert_eq!(box_lon_shift(230.0, 300.0, west, east), -360.0);
    assert_eq!(
        box_lon_shift(-130.0, -60.0, west, east),
        0.0,
        "in frame: the identity"
    );
    assert_eq!(box_lon_shift(-98.0, -97.0, west, east), 0.0);
    assert_eq!(box_lon_shift(262.0, 263.0, west, east), -360.0);
    assert_eq!(
        box_lon_shift(-458.0, -457.0, west, east),
        360.0,
        "a turn the other way"
    );
    assert_eq!(
        box_lon_shift(90.0, 630.0, west, east),
        -360.0,
        "the floor, grown, past the seam"
    );
    assert_eq!(
        lon_shift(90.0, 630.0, west, east),
        0.0,
        "non-triviality: a datum that wide is past lon_shift's ceiling and is not carried"
    );
    assert_eq!(box_lon_shift(f64::NAN, 10.0, west, east), 0.0);
    assert_eq!(box_lon_shift(0.0, f64::INFINITY, west, east), 0.0);
    assert_eq!(box_lon_shift(0.0, 10.0, f64::NAN, east), 0.0);
}
