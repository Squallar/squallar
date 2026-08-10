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
