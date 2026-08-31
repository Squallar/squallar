//! Lines keep their weight on screen when the texture under them gets denser.
//!
//! The coverage wash has two lengths in it and they are not the same kind of
//! thing. The **discs** are ground — 230 km, which is however many texels the
//! projection says and needs no telling. The **edge** drawn round the merged
//! region is a hairline on the display, and that reasoning assumed one texel per
//! point silently for as long as the overlay textures were sized in points. They
//! are sized in physical pixels now
//! (`squallar_egui::overlay_cache::plan_overlay_texture`), so on a display at two
//! of them per point the line would draw at half its intended weight unless it
//! is told the density.
//!
//! **The measurement is of the edge alone, and it has to be.** Counting every
//! painted texel — which is what this module did while it rasterized bare rings
//! — cannot see the density any more: the fill quadruples with the texture on
//! its own, swamps the stroke, and lands on the same ratio whether or not
//! `device_scale` was passed. That is precisely the shape of a test that reads
//! green over a broken stroke, so the fill and the edge are separated by the
//! alpha they are drawn at and only the edge is counted.

use super::{CoverageInput, CoverageSite, rasterize_radar_coverage};
use squallar_geo::GeoBounds;

/// Ten degrees square, so a 230 km disc lands well inside a 256-texel texture
/// with room on every side; the station sits at its centre.
const BOUNDS: GeoBounds = GeoBounds {
    min_lat: 30.0,
    max_lat: 40.0,
    min_lon: -103.0,
    max_lon: -93.0,
};

fn one_site(device_scale: f32) -> CoverageInput {
    CoverageInput {
        sites: vec![CoverageSite {
            lat: 35.0,
            lon: -98.0,
        }],
        device_scale,
    }
}

/// How many texels the rasterizer painted at all.
fn painted(rgba: &[u8]) -> usize {
    rgba.chunks_exact(4).filter(|px| px[3] > 0).count()
}

/// How many texels are **edge** rather than wash.
///
/// The wash is laid down at alpha 38 and the edge at 160 over it, which
/// composites to about 175; nothing else in this raster reaches the eighties. A
/// threshold rather than an equality because the edge is anti-aliased and its
/// fringe lands anywhere between the two.
fn edge_texels(rgba: &[u8]) -> usize {
    rgba.chunks_exact(4).filter(|px| px[3] > 80).count()
}

/// A denser texture draws the coverage edge at four times the texels: twice the
/// perimeter, because the region is ground and the texture is denser, and twice
/// the line weight, because the line is a length on the display.
#[test]
fn a_denser_texture_draws_its_coverage_edge_at_the_same_weight_on_screen() {
    const W: u32 = 256;
    const H: u32 = 256;

    let at_1x = rasterize_radar_coverage(&one_site(1.0), &BOUNDS, W, H);
    let at_2x = rasterize_radar_coverage(&one_site(2.0), &BOUNDS, W * 2, H * 2);

    // The control, measured rather than reasoned about: the same dense texture
    // rasterized by a caller that did *not* pass the density on. Only the
    // perimeter doubles there, so it is what "the stroke ignored the scale"
    // actually costs, and the assertion below is against that number and not
    // against an argued-for one.
    let at_2x_density_ignored = rasterize_radar_coverage(&one_site(1.0), &BOUNDS, W * 2, H * 2);

    let (one, two, ignored) = (
        edge_texels(&at_1x.rgba),
        edge_texels(&at_2x.rgba),
        edge_texels(&at_2x_density_ignored.rgba),
    );
    assert!(one > 0, "the fixture must actually paint an edge");
    assert!(
        painted(&at_1x.rgba) > one,
        "the fixture must paint a wash as well as an edge, or `edge_texels` is \
         counting the whole raster and this test cannot separate them"
    );

    // Below the geometric 4: the anti-aliased fringe either side of the line is
    // about a texel wide at *either* density, so it is a bigger share of a thin
    // line than of a thick one and drags the count sub-quadratic.
    let ratio = two as f64 / one as f64;
    assert!(
        (2.5..=4.3).contains(&ratio),
        "a 2x texture painted {two} edge texels against {one} at 1x, a ratio of \
         {ratio:.2}; perimeter and weight each double, so it must be well clear \
         of the {:.2} that ignoring the density produces",
        ignored as f64 / one as f64,
    );
    assert!(
        two as f64 >= ignored as f64 * 1.2,
        "a 2x texture painted {two} edge texels with the density and {ignored} \
         without it -- too close to tell apart, so this test cannot see a \
         stroke that dropped `device_scale` on the floor",
    );
}

/// One texel per point is byte-identical to what this module produced before
/// it could be told otherwise.
#[test]
fn an_unscaled_display_rasterizes_exactly_as_it_did_before() {
    const W: u32 = 256;
    const H: u32 = 256;

    let plain = rasterize_radar_coverage(&one_site(1.0), &BOUNDS, W, H);
    // Every value that is not a description of a display reads as unscaled,
    // rather than reaching a `Rect::from_xywh` that returns `None` or a radius
    // of zero — either of which is an overlay that silently stops painting.
    for (scale, why) in [
        (0.0, "a zero scale"),
        (-2.0, "a negative scale"),
        (0.5, "a scale under one texel per point"),
        (f32::NAN, "a scale that is not a number"),
    ] {
        let got = rasterize_radar_coverage(&one_site(scale), &BOUNDS, W, H);
        assert_eq!(got.rgba, plain.rgba, "{why} must rasterize as unscaled");
    }
}

/// **Overlapping coverage is one region, not two rings.**
///
/// The whole reason this raster fills under the non-zero winding rule: two
/// stations closer together than 460 km share ground, and a reader asking "is
/// this inside anybody's coverage" needs one answer over the overlap. Under
/// even-odd the overlap would cancel to a hole — coverage drawn as its own
/// absence — and under per-station strokes it was the mesh of intersecting
/// outlines that made the old sites layer illegible.
#[test]
fn two_overlapping_stations_leave_no_hole_between_them() {
    const W: u32 = 256;
    const H: u32 = 256;

    // 1.5 degrees apart at 35N: about 137 km, well inside one coverage radius,
    // so the discs overlap and the midpoint is inside both.
    let pair = CoverageInput {
        sites: vec![
            CoverageSite {
                lat: 35.0,
                lon: -98.75,
            },
            CoverageSite {
                lat: 35.0,
                lon: -97.25,
            },
        ],
        device_scale: 1.0,
    };
    let out = rasterize_radar_coverage(&pair, &BOUNDS, W, H);

    // The midpoint of the two stations, in texels. Inside both discs, so it is
    // exactly the texel an even-odd fill would punch out.
    let mid = ((W / 2) as usize, (H / 2) as usize);
    let px = &out.rgba[(mid.1 * W as usize + mid.0) * 4..][..4];
    assert!(
        px[3] > 0,
        "the ground both stations cover came back transparent -- an even-odd \
         fill draws doubled coverage as a hole"
    );
}
