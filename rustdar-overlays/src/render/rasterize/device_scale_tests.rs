//! Symbols keep their size on screen when the texture under them gets denser.
//!
//! Every marker radius, label pill and stroke width in this module is a length
//! in **texels**, chosen from the map zoom to look right on screen. That
//! reasoning assumed one texel per point, silently, for as long as the overlay
//! textures were sized in points. They are sized in physical pixels now
//! (`rustdar_egui::overlay_cache::plan_overlay_texture`), so on a display at
//! two of them per point every one of those lengths would draw at half its
//! intended size unless it is told the density.

use super::{RadarSiteInfo, SitesInput, rasterize_radar_sites};
use rustdar_geo::GeoBounds;

const BOUNDS: GeoBounds = GeoBounds {
    min_lat: 34.0,
    max_lat: 36.0,
    min_lon: -99.0,
    max_lon: -97.0,
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

/// A site marker on a 2x texture covers four times the texels, because it is
/// the same size on screen and the screen has four times as many.
#[test]
fn a_denser_texture_draws_its_markers_at_the_same_size_on_screen() {
    const W: u32 = 256;
    const H: u32 = 256;

    let at_1x = rasterize_radar_sites(&one_site(1.0), &BOUNDS, W, H);
    let at_2x = rasterize_radar_sites(&one_site(2.0), &BOUNDS, W * 2, H * 2);

    let (one, two) = (painted(&at_1x.rgba), painted(&at_2x.rgba));
    assert!(one > 0, "the fixture must actually paint a marker");

    let ratio = two as f64 / one as f64;
    assert!(
        (3.4..=4.3).contains(&ratio),
        "a 2x texture painted {two} texels against {one} at 1x, a ratio of \
         {ratio:.2}; at the same on-screen size it must be about 4, and a \
         ratio near 1 means the scale was ignored",
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
