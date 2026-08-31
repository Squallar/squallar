//! The dateline, for the callers that are not GLM.
//!
//! The defect only exists where a folded datum meets an unfolded viewport, so
//! a fixture that never goes near the antimeridian cannot see it and would
//! pass identically on broken and fixed code. Every fixture here is measured
//! from the live product rather than invented:
//!
//!  * [`AKZ791`] and [`AKZ787`] are the largest ring of two *adjacent* NWS
//!    forecast zones, decimated to 41 vertices from
//!    `api.weather.gov/zones/forecast/...` on 2026-08-14. Shemya and Attu sits
//!    at 178.62..179.46 **east**; Atka and Adak at -175.30..-174.01 **west**.
//!    They are neighbours on the ground and a turn apart in the coordinates.
//!  * The station positions are from `api.weather.gov/radar/stations`, the
//!    same 208-row catalogue the app itself fetches: `PGUA` 13.4558/144.8111,
//!    `PAEC` 64.5114/-165.2950, `PABC` 60.7919/-161.8764.
//!
//! `walkers`' `unproject` is linear in pixel x and folds nothing, and
//! `OverlayTexturePlan::coverage` deliberately does not clamp longitude, so a
//! view panned across the seam arrives with an out-of-range edge — `-195..-165`
//! panning west, `140..200` panning east. Both appear below, and a view of the
//! *same ground* written both ways is what
//! [`a_zone_paints_the_same_pixels_whichever_way_the_viewport_is_written`]
//! stands on.

use super::*;
use crate::types::HatchPattern;

/// `AKZ791` "Shemya and Attu Islands", largest ring, 41 vertices,
/// lon 178.6194..179.4558 — **east** of the antimeridian.
#[rustfmt::skip]
const AKZ791: [(f64, f64); 41] = [
    (51.6423, 178.7016), (51.6455, 178.7612), (51.6273, 178.8125), (51.6192, 178.8808),
    (51.5786, 178.9522), (51.5483, 179.0074), (51.4788, 179.1261), (51.4482, 179.1925),
    (51.4255, 179.2171), (51.4086, 179.2604), (51.4081, 179.2937), (51.4039, 179.3309),
    (51.4030, 179.3782), (51.3868, 179.4071), (51.3816, 179.4447), (51.3693, 179.4558),
    (51.3622, 179.4244), (51.3624, 179.3877), (51.3773, 179.3904), (51.3632, 179.3288),
    (51.3579, 179.2682), (51.3635, 179.2152), (51.3866, 179.2254), (51.4005, 179.1701),
    (51.4191, 179.1421), (51.4356, 179.1039), (51.4532, 179.0683), (51.4728, 179.0408),
    (51.4990, 179.0147), (51.5231, 178.9676), (51.5477, 178.9254), (51.5578, 178.8961),
    (51.5636, 178.8602), (51.5753, 178.8257), (51.5710, 178.7869), (51.5889, 178.7642),
    (51.5887, 178.7282), (51.6008, 178.6983), (51.6179, 178.6625), (51.6370, 178.6194),
    (51.6595, 178.6684),
];

/// `AKZ787` "Atka and Adak", largest ring, 41 vertices,
/// lon -175.3027..-174.0131 — **west** of the antimeridian, and the control:
/// it is already in frame for the viewports below.
#[rustfmt::skip]
const AKZ787: [(f64, f64); 41] = [
    (52.4160, -174.1431), (52.3452, -174.0131), (52.2711, -174.0260), (52.2119, -174.2004),
    (52.1762, -174.1469), (52.1171, -174.1144), (52.0943, -174.2184), (52.1226, -174.3272),
    (52.0931, -174.3680), (52.0629, -174.3923), (52.0699, -174.4707), (52.0432, -174.5015),
    (52.0353, -174.5559), (52.0349, -174.6270), (52.0520, -174.6906), (52.0109, -174.7380),
    (52.0331, -174.8410), (52.0242, -174.9717), (52.0029, -175.1000), (52.0115, -175.3027),
    (52.0356, -175.2279), (52.0294, -175.1259), (52.0523, -175.0409), (52.0824, -174.9653),
    (52.0953, -174.8983), (52.0939, -174.8241), (52.1084, -174.7415), (52.1105, -174.6530),
    (52.1057, -174.5792), (52.1334, -174.4882), (52.1771, -174.5091), (52.2002, -174.4488),
    (52.1930, -174.3434), (52.2262, -174.2938), (52.2775, -174.2410), (52.3175, -174.3311),
    (52.2890, -174.3279), (52.2899, -174.4083), (52.3142, -174.3586), (52.3933, -174.2690),
    (52.4181, -174.1449),
];

const TEX: u32 = 512;

fn feature(ring: &[(f64, f64)]) -> OverlayFeature {
    OverlayFeature::new(
        vec![vec![ring.to_vec()]],
        [255, 0, 0, 255],
        [255, 255, 255, 255],
        String::new(),
        String::new(),
        HatchPattern::None,
    )
}

fn bounds(min_lat: f64, max_lat: f64, min_lon: f64, max_lon: f64) -> GeoBounds {
    GeoBounds {
        min_lat,
        max_lat,
        min_lon,
        max_lon,
    }
}

fn painted(rgba: &[u8]) -> usize {
    rgba.chunks_exact(4).filter(|p| p[3] != 0).count()
}

fn painted_bbox(rgba: &[u8], w: u32) -> Option<(u32, u32, u32, u32)> {
    let mut b: Option<(u32, u32, u32, u32)> = None;
    for (i, p) in rgba.chunks_exact(4).enumerate() {
        if p[3] == 0 {
            continue;
        }
        let (x, y) = (i as u32 % w, i as u32 / w);
        b = Some(match b {
            None => (x, y, x, y),
            Some((x0, y0, x1, y1)) => (x0.min(x), y0.min(y), x1.max(x), y1.max(y)),
        });
    }
    b
}

fn draw(features: &[OverlayFeature], b: &GeoBounds) -> Vec<u8> {
    rasterize_spc_outlooks(
        &OutlooksInput {
            features: features.to_vec(),
            hatch_color: [0, 0, 0, 0],
            device_scale: 1.0,
        },
        b,
        TEX,
        TEX,
    )
    .rgba
}

/// The defect, on the two real zones either side of the seam. A view of
/// 165E..165W arrives as `-195..-165`; `AKZ787` is already written in that frame
/// and is the control, while `AKZ791` is a turn away and, before the shift, its
/// feature AABB could not intersect the viewport box at all.
#[test]
fn both_sides_of_the_seam_draw_in_a_view_that_spans_it() {
    let view = bounds(50.0, 54.0, -195.0, -165.0);

    let west = painted(&draw(&[feature(&AKZ787)], &view));
    let east = painted(&draw(&[feature(&AKZ791)], &view));

    // The control. If this is zero the fixture is wrong, not the code.
    assert!(
        west > 0,
        "AKZ787 is already in the viewport's frame and must draw regardless; got {west} px"
    );
    assert!(
        east > 0,
        "AKZ791 sits a turn from the viewport's frame and is what the shift is for; got {east} px"
    );
}

/// Non-triviality: the painted area has to *be* the island. A shift that is not
/// rigid — a per-vertex `wrap_lon` — moves whichever vertices fall west of
/// `min_lon` to the far side and paints a band across the texture.
#[test]
fn the_shifted_zone_paints_its_own_extent_and_not_a_band() {
    let view = bounds(50.0, 54.0, -195.0, -165.0);
    let rgba = draw(&[feature(&AKZ791)], &view);
    let (x0, _, x1, _) = painted_bbox(&rgba, TEX).expect("AKZ791 must paint");

    // 178.6194..179.4558 is 0.8364 deg of a 30 deg viewport: 14.3 px of 512.
    // Allow the stroke its width either side.
    let width = x1 - x0 + 1;
    assert!(
        (8..=32).contains(&width),
        "AKZ791 spans 0.84 deg, so ~14 px of {TEX}; painted {width} px wide \
         (a full-width band means the shift was applied per vertex, not per polygon)"
    );
}

/// The anti-deformation result, stated as an invariance a wrap cannot satisfy:
/// `-195..-165` and `165..195` name the *same ground*, so a rigid shift must
/// paint identical pixels.
#[test]
fn a_zone_paints_the_same_pixels_whichever_way_the_viewport_is_written() {
    let west = bounds(50.0, 54.0, -195.0, -165.0);
    let east = bounds(50.0, 54.0, 165.0, 195.0);
    let west_written = draw(&[feature(&AKZ791)], &west);
    let east_written = draw(&[feature(&AKZ791)], &east);

    let n = painted(&east_written);
    assert!(n > 100, "the fixture must paint a real area; got {n} px");
    assert_eq!(
        painted(&west_written),
        n,
        "the same ground written two ways must paint the same count"
    );
    assert!(
        west_written == east_written,
        "the same ground written two ways must paint the same pixels"
    );

    // Non-triviality as growth rather than as a floor: a count from a real shape
    // scales with its sampling; a count that is really zero does not.
    let big = |b: &GeoBounds| {
        rasterize_spc_outlooks(
            &OutlooksInput {
                features: vec![feature(&AKZ791)],
                hatch_color: [0, 0, 0, 0],
                device_scale: 1.0,
            },
            b,
            TEX * 2,
            TEX * 2,
        )
        .rgba
    };
    let (be, bw) = (painted(&big(&east)), painted(&big(&west)));
    assert_eq!(
        be, bw,
        "growth must be identical either way the viewport is written"
    );
    assert!(
        be > n * 2,
        "quadrupling texture area must grow a real shape's pixel count well past {n}; got {be}"
    );
}

/// A polygon straddling the viewport's **western edge** — which happens on any
/// pan, not only at the seam — must stay put.
///
/// `wrap_lon` lands in `[min_lon, min_lon + 360)`, so a vertex at 164.9 against
/// a `min_lon` of 165 comes back at 524.9 while its neighbour at 165.1 stays:
/// the ring is torn across the texture. `lon_shift` returns 0 for the polygon.
#[test]
fn a_polygon_across_the_western_edge_is_clipped_not_torn() {
    // The Shemya ring moved so that it sits astride min_lon.
    let straddling: Vec<(f64, f64)> = AKZ791
        .iter()
        .map(|&(lat, lon)| (lat, lon - 179.0 + 165.0))
        .collect();
    let view = bounds(50.0, 54.0, 165.0, 195.0);
    let rgba = draw(&[feature(&straddling)], &view);
    let (x0, _, x1, _) = painted_bbox(&rgba, TEX).expect("the visible half must paint");

    assert_eq!(
        x0, 0,
        "the ring crosses the western edge, so it paints column 0"
    );
    // The eastern half reaches 0.4558 deg past min_lon: 7.8 px of 512.
    assert!(
        x1 < TEX / 8,
        "only the eastern sliver is in view, so painting must stop near the \
         west edge; it reached column {x1} of {TEX} (a tear would cross the texture)"
    );
}

/// Real stations, in a real view that spans the seam. `140..200` is 140E..160W;
/// `PGUA` is the control, `PAEC` and `PABC` are written at -165.30 and -161.88.
#[test]
fn stations_either_side_of_the_seam_all_draw() {
    let view = bounds(5.0, 70.0, 140.0, 200.0);
    let one = |lat: f64, lon: f64| {
        let input = super::CoverageInput {
            sites: vec![CoverageSite { lat, lon }],
            device_scale: 1.0,
        };
        painted(&rasterize_radar_coverage(&input, &view, TEX, TEX).rgba)
    };

    let guam = one(13.4558, 144.8111);
    let nome = one(64.5114, -165.2950);
    let bethel = one(60.7919, -161.8764);

    assert!(
        guam > 0,
        "PGUA is already in frame and must draw regardless; got {guam} px"
    );
    assert!(
        nome > 0,
        "PAEC belongs at 194.71 in this view; got {nome} px"
    );
    assert!(
        bethel > 0,
        "PABC belongs at 198.12 in this view; got {bethel} px"
    );
}

/// A station a little *west* of the texture still contributes its coverage —
/// its 230 km disc reaches ground that is in frame, which `wrap_lon` would have
/// deleted by sending the station 359.5 deg east instead of leaving it there.
#[test]
fn a_station_just_west_of_the_texture_keeps_its_slack() {
    let view = bounds(30.0, 50.0, -100.0, -70.0);
    let n = painted(
        &rasterize_radar_coverage(
            &super::CoverageInput {
                sites: vec![CoverageSite {
                    lat: 40.0,
                    lon: -100.5,
                }],
                device_scale: 1.0,
            },
            &view,
            TEX,
            TEX,
        )
        .rgba,
    );
    assert!(
        n > 0,
        "a station 0.5 deg west of a 30 deg viewport covers ground inside it \
         and must still paint; got {n} px"
    );
}

#[test]
fn a_datum_already_in_frame_is_not_moved() {
    let mb = MercatorBounds::from_geo(&bounds(30.0, 50.0, -100.0, -70.0));
    for lon in [-100.0, -99.9, -85.0, -70.1, -70.0] {
        assert_eq!(mb.lon_shift(lon, lon), 0.0, "lon {lon} is in frame");
        assert_eq!(mb.nearest_lon(lon), lon);
    }
    for lon in [-110.0, -60.0] {
        assert_eq!(
            mb.nearest_lon(lon),
            lon,
            "lon {lon} is nearer than its turn away"
        );
    }
}

/// A datum wider than a half-turn has no unambiguous nearest representation,
/// so it is left alone rather than guessed at.
#[test]
fn a_datum_wider_than_a_half_turn_gets_no_shift() {
    let mb = MercatorBounds::from_geo(&bounds(50.0, 54.0, -195.0, -165.0));
    // A feature whose parts the source already cut at the seam pools into a
    // box this wide; `PKZ784` measures -179.9999..180.0.
    assert_eq!(mb.lon_shift(-179.9999, 180.0), 0.0);
    assert_eq!(mb.lon_shift(0.0, 180.0), 0.0);
    assert_ne!(mb.lon_shift(170.0, 179.0), 0.0);
}

#[test]
fn every_shift_is_a_whole_turn() {
    let mb = MercatorBounds::from_geo(&bounds(50.0, 54.0, -195.0, -165.0));
    for lon in [-179.0, -90.0, 0.0, 90.0, 144.81, 178.62, 179.99] {
        let s = mb.lon_shift(lon, lon);
        assert_eq!(
            s % 360.0,
            0.0,
            "shift {s} for lon {lon} is not a whole turn"
        );
    }
}
