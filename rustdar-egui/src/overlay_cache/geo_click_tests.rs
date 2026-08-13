use super::*;
use rustdar_overlays::types::HatchPattern;

/// A feature holding the given polygons, with fields that play no part in
/// hit-testing left at throwaway values.
fn feature(polygons: Vec<Vec<Vec<(f64, f64)>>>) -> OverlayFeature {
    OverlayFeature::new(
        polygons,
        [0, 0, 0, 0],
        [0, 0, 0, 0],
        String::new(),
        String::new(),
        HatchPattern::None,
    )
}

/// A square ring in (lat, lon) order, closed like GeoJSON rings arrive.
fn square(min_lat: f64, max_lat: f64, min_lon: f64, max_lon: f64) -> Vec<(f64, f64)> {
    vec![
        (min_lat, min_lon),
        (min_lat, max_lon),
        (max_lat, max_lon),
        (max_lat, min_lon),
        (min_lat, min_lon),
    ]
}

/// A donut: `polygon[0]` is the exterior, `polygon[1]` the hole — the ring
/// layout `GeoPolygonRing` documents and GeoJSON delivers.
fn donut() -> Vec<Vec<(f64, f64)>> {
    vec![
        square(30.0, 40.0, -100.0, -90.0),
        square(33.0, 37.0, -97.0, -93.0),
    ]
}

/// The live consequence of testing only `polygon.first()`: a click inside
/// an SPC/NWS polygon's hole reported "inside" and opened the surrounding
/// feature's popup. Even-odd says a hole hit is an exit, not an entry.
#[test]
fn a_point_in_a_polygons_hole_is_outside_the_feature() {
    let f = feature(vec![donut()]);
    assert!(
        !geo_point_in_feature(35.0, -95.0, &f),
        "the hole's centre was reported inside — interior rings are being ignored"
    );
    // The counterweight, either side of the hole boundary: the ring
    // between exterior and hole is still inside, so this cannot pass by
    // rejecting the exterior wholesale.
    assert!(
        geo_point_in_feature(31.0, -95.0, &f),
        "the solid ring south of the hole"
    );
    assert!(
        geo_point_in_feature(35.0, -98.5, &f),
        "the solid ring west of the hole"
    );
    assert!(
        !geo_point_in_feature(45.0, -95.0, &f),
        "north of the exterior entirely"
    );
}

/// A hole belongs to *its* polygon only. A second polygon of the same
/// MultiPolygon sitting inside the first one's hole (an island) must still
/// take the hit — rejecting on any hole in the feature would lose it.
#[test]
fn a_hole_in_one_polygon_does_not_mask_another_polygon_of_the_feature() {
    let island = vec![square(34.0, 36.0, -96.0, -94.0)];
    let f = feature(vec![donut(), island]);
    assert!(
        geo_point_in_feature(35.0, -95.0, &f),
        "the island inside the donut's hole was masked by the hole"
    );
}

// ── Renderer / hit-test agreement ────────────────────────────────────────

/// The two halves of "is this point in this feature" — the pixels the
/// rasterizer paints and the answer `geo_point_in_feature` gives — must agree.
///
/// They did not. `draw_feature` filled only `polygon.first()`, so every hole
/// in this file's fixtures painted solid while the tests above called it
/// empty: a donut you could see but not click. This walks a grid over the
/// whole feature and fails on the first point where paint and hit test
/// disagree, so neither side can drift without the other.
#[test]
fn every_painted_pixel_agrees_with_the_hit_test() {
    // A donut with an island in its hole: exterior fill, hole, and a second
    // polygon inside the hole, so the sampler crosses all three answers.
    let island = vec![square(34.0, 36.0, -96.0, -94.0)];
    let f = feature(vec![donut(), island]);

    const W: u32 = 400;
    const H: u32 = 400;
    // Padded past the rings so nothing is decided by the texture edge.
    let bounds = GeoBounds {
        min_lat: 29.0,
        max_lat: 41.0,
        min_lon: -101.0,
        max_lon: -89.0,
    };
    // Opaque fill, no stroke: a painted pixel is then unambiguous, and no
    // outline can bleed a hole's rim into the count.
    let rendered = rustdar_overlays::render::rasterize::rasterize_spc_outlooks(
        &[OverlayFeature::new(
            f.polygons.clone(),
            [255, 0, 0, 255],
            [0, 0, 0, 0],
            String::new(),
            String::new(),
            HatchPattern::None,
        )],
        &bounds,
        W,
        H,
        [0, 0, 0, 0],
        1.0,
    );

    // The projection `MercatorBounds` applies, restated: lon is linear, lat is
    // linear in Mercator Y, and the texture's y axis runs north to south.
    let merc_min = lat_rad_to_mercator_y(bounds.min_lat.to_radians());
    let merc_max = lat_rad_to_mercator_y(bounds.max_lat.to_radians());
    let project = |lat: f64, lon: f64| -> (f64, f64) {
        let x = (lon - bounds.min_lon) / (bounds.max_lon - bounds.min_lon) * W as f64;
        let m = lat_rad_to_mercator_y(lat.to_radians());
        let y = (1.0 - (m - merc_min) / (merc_max - merc_min)) * H as f64;
        (x, y)
    };

    // Anti-aliasing and the pixel grid make the answer genuinely ambiguous
    // within a pixel or so of any ring, and the hit test's own boundary
    // behaviour is documented as unspecified. Sample only where neither side
    // has an excuse.
    let clearance_px = |lat: f64, lon: f64| -> f64 {
        let (px, py) = project(lat, lon);
        let mut best = f64::MAX;
        for ring in f.polygons.iter().flatten() {
            for pair in ring.windows(2) {
                let (ax, ay) = project(pair[0].0, pair[0].1);
                let (bx, by) = project(pair[1].0, pair[1].1);
                let (dx, dy) = (bx - ax, by - ay);
                let len_sq = dx * dx + dy * dy;
                let t = if len_sq < 1e-12 {
                    0.0
                } else {
                    (((px - ax) * dx + (py - ay) * dy) / len_sq).clamp(0.0, 1.0)
                };
                let (cx, cy) = (ax + t * dx, ay + t * dy);
                best = best.min(((px - cx).powi(2) + (py - cy).powi(2)).sqrt());
            }
        }
        best
    };

    let mut checked = 0;
    let mut in_hole_checked = 0;
    for iy in 0..80 {
        for ix in 0..80 {
            let lat = bounds.min_lat + (iy as f64 + 0.5) / 80.0 * (bounds.max_lat - bounds.min_lat);
            let lon = bounds.min_lon + (ix as f64 + 0.5) / 80.0 * (bounds.max_lon - bounds.min_lon);
            if clearance_px(lat, lon) < 2.0 {
                continue;
            }
            let (px, py) = project(lat, lon);
            let idx = ((py as u32) * W + px as u32) as usize * 4;
            let painted = rendered[idx + 3] > 0;
            let hit = geo_point_in_feature(lat, lon, &f);
            assert_eq!(
                painted, hit,
                "({lat:.3}, {lon:.3}) is painted={painted} but hit={hit} — the \
                 rasterizer and the hit test disagree about the shape"
            );
            checked += 1;
            // Inside the donut's hole but outside the island: the region the
            // whole disagreement was about.
            if (33.0..37.0).contains(&lat)
                && (-97.0..-93.0).contains(&lon)
                && !((34.0..36.0).contains(&lat) && (-96.0..-94.0).contains(&lon))
            {
                in_hole_checked += 1;
                assert!(
                    !painted,
                    "({lat:.3}, {lon:.3}) is in the hole and was painted"
                );
            }
        }
    }
    assert!(
        checked > 3_000,
        "only {checked} points cleared every ring by 2 px; the sampler is not \
         covering the feature and would pass on a blank texture"
    );
    assert!(
        in_hole_checked > 50,
        "only {in_hole_checked} sample points landed in the hole; this test \
         is no longer exercising the case it exists for"
    );
}
