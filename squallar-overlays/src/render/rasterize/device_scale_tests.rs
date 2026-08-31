//! Lines keep their weight on screen when the texture under them gets denser.
//!
//! A coverage ring has two lengths in it and they are not the same kind of
//! thing. Its **radius** is ground — 230 km, which is however many texels the
//! projection says and needs no telling. Its **stroke width** is a hairline on
//! the display, chosen so a continental view of the whole network is a map
//! rather than a mesh, and that reasoning assumed one texel per point silently
//! for as long as the overlay textures were sized in points. They are sized in
//! physical pixels now (`squallar_egui::overlay_cache::plan_overlay_texture`),
//! so on a display at two of them per point the line would draw at half its
//! intended weight unless it is told the density.
//!
//! That split is what the ratio below measures. A denser texture doubles the
//! radius on its own; only the stroke needs `device_scale`, and a rasterizer
//! that ignored it would land on 2 instead of 4.

use super::{RadarSiteInfo, SitesInput, rasterize_radar_sites};
use squallar_geo::GeoBounds;

/// Ten degrees square, so a 230 km ring lands well inside a 256-texel texture
/// with room on every side; the station sits at its centre.
const BOUNDS: GeoBounds = GeoBounds {
    min_lat: 30.0,
    max_lat: 40.0,
    min_lon: -103.0,
    max_lon: -93.0,
};

fn one_site(device_scale: f32) -> SitesInput {
    SitesInput {
        sites: vec![RadarSiteInfo {
            name: "KTLX".to_string(),
            lat: 35.0,
            lon: -98.0,
            is_current: false,
            is_loading: false,
        }],
        zoom: 8.0,
        is_dark: false,
        device_scale,
    }
}

/// How many pixels the rasterizer actually painted.
fn painted(rgba: &[u8]) -> usize {
    rgba.chunks_exact(4).filter(|px| px[3] > 0).count()
}

/// A coverage ring on a 2x texture covers four times the texels: twice the
/// circumference, because the ring is ground and the texture is denser, and
/// twice the line weight, because the line is a length on the display.
#[test]
fn a_denser_texture_draws_its_rings_at_the_same_weight_on_screen() {
    const W: u32 = 256;
    const H: u32 = 256;

    let at_1x = rasterize_radar_sites(&one_site(1.0), &BOUNDS, W, H);
    let at_2x = rasterize_radar_sites(&one_site(2.0), &BOUNDS, W * 2, H * 2);

    // The control, measured rather than reasoned about: the same dense texture
    // rasterized by a caller that did *not* pass the density on. Only the
    // circumference doubles there, so it is what "the stroke ignored the scale"
    // actually costs, and the assertion below is against that number and not
    // against an argued-for one.
    let at_2x_density_ignored = rasterize_radar_sites(&one_site(1.0), &BOUNDS, W * 2, H * 2);

    let (one, two, ignored) = (
        painted(&at_1x.rgba),
        painted(&at_2x.rgba),
        painted(&at_2x_density_ignored.rgba),
    );
    assert!(one > 0, "the fixture must actually paint a ring");

    // Below the geometric 4: the anti-aliased fringe either side of the line is
    // about a texel wide at *either* density, so it is a bigger share of a thin
    // line than of a thick one and drags the count sub-quadratic.
    let ratio = two as f64 / one as f64;
    assert!(
        (2.5..=4.3).contains(&ratio),
        "a 2x texture painted {two} texels against {one} at 1x, a ratio of \
         {ratio:.2}; circumference and weight each double, so it must be well \
         clear of the {:.2} that ignoring the density produces",
        ignored as f64 / one as f64,
    );
    assert!(
        two as f64 >= ignored as f64 * 1.2,
        "a 2x texture painted {two} texels with the density and {ignored} \
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

    let plain = rasterize_radar_sites(&one_site(1.0), &BOUNDS, W, H);
    // Every value that is not a description of a display reads as unscaled,
    // rather than reaching a `Rect::from_xywh` that returns `None` or a radius
    // of zero — either of which is an overlay that silently stops painting.
    for (scale, why) in [
        (0.0, "a zero scale"),
        (-2.0, "a negative scale"),
        (0.5, "a scale under one texel per point"),
        (f32::NAN, "a scale that is not a number"),
    ] {
        let got = rasterize_radar_sites(&one_site(scale), &BOUNDS, W, H);
        assert_eq!(got.rgba, plain.rgba, "{why} must rasterize as unscaled");
    }
}
