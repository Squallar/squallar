//! The slippy-map transforms, against implementations that are not ours.

use super::*;
use squallar_geo::{
    MERCATOR_LAT_LIMIT_DEG, lat_to_tile_y, lon_to_tile_x, tile_to_lat, tile_to_lon,
};

/// `(lat, lon, zoom, mercantile's x, mercantile's y)`.
const MERCANTILE_TILE: &[(f64, f64, u8, u32, u32)] = &[
    (35.3331, -97.2778, 3, 1, 3), // KTLX Oklahoma City OK
    (35.3331, -97.2778, 8, 58, 101),
    (35.3331, -97.2778, 12, 941, 1617),
    (35.3331, -97.2778, 16, 15059, 25884),
    (35.3331, -97.2778, 18, 60236, 103537),
    (44.8489, -93.5656, 3, 1, 2), // KMPX Minneapolis MN
    (44.8489, -93.5656, 8, 61, 92),
    (44.8489, -93.5656, 12, 983, 1475),
    (44.8489, -93.5656, 16, 15734, 23613),
    (44.8489, -93.5656, 18, 62939, 94455),
    (18.1156, -66.0781, 3, 2, 3), // TJUA Puerto Rico
    (18.1156, -66.0781, 8, 81, 114),
    (18.1156, -66.0781, 12, 1296, 1838),
    (18.1156, -66.0781, 16, 20738, 29413),
    (18.1156, -66.0781, 18, 82955, 117655),
    (20.1254, -155.778, 3, 0, 3), // PHKM Kohala HI
    (20.1254, -155.778, 8, 17, 113),
    (20.1254, -155.778, 12, 275, 1814),
    (20.1254, -155.778, 16, 4409, 29026),
    (20.1254, -155.778, 18, 17637, 116106),
    (60.7919, -161.8763, 3, 0, 2), // PABC Bethel AK
    (60.7919, -161.8763, 8, 12, 73),
    (60.7919, -161.8763, 12, 206, 1171),
    (60.7919, -161.8763, 16, 3299, 18739),
    (60.7919, -161.8763, 18, 13197, 74959),
    (65.0351, -147.5014, 3, 0, 2), // PAPD Fairbanks AK — highest-latitude WSR-88D
    (65.0351, -147.5014, 8, 23, 66),
    (65.0351, -147.5014, 12, 369, 1064),
    (65.0351, -147.5014, 16, 5916, 17039),
    (65.0351, -147.5014, 18, 23664, 68159),
    (56.8528, -135.5292, 3, 0, 2), // PACG Sitka AK
    (56.8528, -135.5292, 8, 31, 78),
    (56.8528, -135.5292, 12, 505, 1257),
    (56.8528, -135.5292, 16, 8095, 20126),
    (56.8528, -135.5292, 18, 32382, 80506),
    (36.956, 127.021, 3, 6, 3), // RKSG Camp Humphreys KR — east of the meridian
    (36.956, 127.021, 8, 218, 99),
    (36.956, 127.021, 12, 3493, 1594),
    (36.956, 127.021, 16, 55891, 25518),
    (36.956, 127.021, 18, 223565, 102074),
    (29.7039, -98.0286, 3, 1, 3), // KEWX Austin TX — HOLDOUT
    (29.7039, -98.0286, 8, 58, 105),
    (29.7039, -98.0286, 12, 932, 1693),
    (29.7039, -98.0286, 16, 14922, 27100),
    (29.7039, -98.0286, 18, 59689, 108402),
    (0.0, 0.0, 0, 0, 0),
    (89.999999, 0.0, 0, 0, 0),
    (-89.999999, 0.0, 0, 0, 0),
    (0.0, 180.0, 0, 0, 0),
    (0.0, -180.0, 0, 0, 0),
    (85.0511287798066, 0.0, 0, 0, 0),
    (-85.0511287798066, 0.0, 0, 0, 0),
    (89.999999, 0.0, 5, 16, 0),
    (89.999999, 0.0, 14, 8192, 0),
    (-89.999999, 0.0, 5, 16, 31),
    (-89.999999, 0.0, 14, 8192, 16383),
    (85.0511287798066, 0.0, 5, 16, 0),
    (85.0511287798066, 0.0, 14, 8192, 0),
    (-85.0511287798066, 0.0, 5, 16, 31),
    (-85.0511287798066, 0.0, 14, 8192, 16383),
    (85.05112878080661, 0.0, 5, 16, 0), // one ulp-ish past, north
    (85.05112878080661, 0.0, 14, 8192, 0),
    (-85.05112878080661, 0.0, 5, 16, 31), // one ulp-ish past, south
    (-85.05112878080661, 0.0, 14, 8192, 16383),
    (85.0511287788066, 0.0, 5, 16, 0), // just inside
    (85.0511287788066, 0.0, 14, 8192, 0),
    (85.05, 0.0, 5, 16, 0), // the truncated figure three modules used to carry
    (85.05, 0.0, 14, 8192, 0),
    (-85.05, 0.0, 5, 16, 31),
    (-85.05, 0.0, 14, 8192, 16383),
    (0.0, 180.0, 5, 31, 16),
    (0.0, 180.0, 14, 16383, 8192),
    (0.0, -180.0, 5, 0, 16),
    (0.0, -180.0, 14, 0, 8192),
    (0.0, 179.999999, 5, 31, 16),
    (0.0, 179.999999, 14, 16383, 8192),
    (0.0, -179.999999, 5, 0, 16),
    (0.0, -179.999999, 14, 0, 8192),
    (65.0, 180.0, 5, 31, 8), // and at a latitude a radar could be at
    (65.0, 180.0, 14, 16383, 4263),
    (65.0, -180.0, 5, 0, 8),
    (65.0, -180.0, 14, 0, 4263),
    (0.0, 180.000001, 5, 31, 16), // past it, which unproject can produce
    (0.0, 180.000001, 14, 16383, 8192),
    (0.0, -180.000001, 5, 0, 16),
    (0.0, -180.000001, 14, 0, 8192),
];

/// `(x, y, zoom, mercantile's ul().lat, mercantile's ul().lng)`.
const MERCANTILE_UL: &[(u32, u32, u8, f64, f64)] = &[
    (1, 3, 3, 40.97989806962013, -135.0),
    (58, 101, 8, 35.4606699514953, -98.4375),
    (941, 1617, 12, 35.389049966911664, -97.294921875),
    (15059, 25884, 16, 35.33529320309328, -97.2784423828125),
    (60236, 103537, 18, 35.334172889944156, -97.2784423828125),
    (1, 2, 3, 66.51326044311186, -135.0),
    (61, 92, 8, 45.089035564831015, -94.21875),
    (983, 1475, 12, 44.90257799628886, -93.603515625),
    (15734, 23613, 16, 44.85197466334986, -93.570556640625),
    (62939, 94455, 18, 44.84905388253941, -93.56643676757812),
    (2, 3, 3, 40.97989806962013, -90.0),
    (81, 114, 8, 19.31114335506464, -66.09375),
    (1296, 1838, 12, 18.145851771694467, -66.09375),
    (20738, 29413, 16, 18.119749966946426, -66.082763671875),
    (82955, 117655, 18, 18.11583436045722, -66.07864379882812),
    (0, 3, 3, 40.97989806962013, -180.0),
    (17, 113, 8, 20.632784250388017, -156.09375),
    (275, 1814, 12, 20.138470312451147, -155.830078125),
    (4409, 29026, 16, 20.128155311797176, -155.7806396484375),
    (17637, 116106, 18, 20.12557645527057, -155.77926635742188),
    (0, 2, 3, 66.51326044311186, -180.0),
    (12, 73, 8, 60.930432202923335, -163.125),
    (206, 1171, 12, 60.80206374467982, -161.89453125),
    (3299, 18739, 16, 60.79402357411144, -161.8780517578125),
    (13197, 74959, 18, 60.79201321604703, -161.87667846679688),
    (23, 66, 8, 65.36683689226321, -147.65625),
    (369, 1064, 12, 65.07213008560696, -147.568359375),
    (5916, 17039, 16, 65.03737880040536, -147.50244140625),
    (23664, 68159, 18, 65.03564004643361, -147.50244140625),
    (31, 78, 8, 57.32652122521708, -136.40625),
    (505, 1257, 12, 56.897003921272606, -135.615234375),
    (8095, 20126, 16, 56.8549793476547, -135.5328369140625),
    (32382, 80506, 18, 56.85347759622393, -135.53009033203125),
    (6, 3, 3, 40.97989806962013, 90.0),
    (218, 99, 8, 37.718590325588146, 126.5625),
    (3493, 1594, 12, 37.020098201368114, 127.001953125),
    (55891, 25518, 16, 36.958671131530316, 127.0184326171875),
    (223565, 102074, 18, 36.95647639022987, 127.01980590820312),
    (58, 105, 8, 30.751277776257812, -98.4375), // KEWX, holdout
    (932, 1693, 12, 29.764377375163114, -98.0859375),
    (14922, 27100, 16, 29.707139348134152, -98.031005859375),
    (59689, 108402, 18, 29.704753721672635, -98.02963256835938),
    (0, 0, 0, 85.0511287798066, -180.0),
    (16, 16, 5, 0.0, 0.0),
    (8192, 8192, 14, 0.0, 0.0),
    (16, 0, 5, 85.0511287798066, 0.0),
    (8192, 0, 14, 85.0511287798066, 0.0),
    (16, 31, 5, -83.97925949886205, 0.0),
    (8192, 16383, 14, -85.04923290826918, 0.0),
    (31, 16, 5, 0.0, 168.75),
    (16383, 8192, 14, 0.0, 179.97802734375),
    (0, 16, 5, 0.0, -180.0),
    (0, 8192, 14, 0.0, -180.0),
    (31, 8, 5, 66.51326044311186, 168.75),
    (16383, 4263, 14, 65.00722434895742, 179.97802734375),
    (0, 8, 5, 66.51326044311186, -180.0),
    (0, 4263, 14, 65.00722434895742, -180.0),
];

/// Every case in the table lands in the tile `mercantile` lands it in.
#[test]
fn a_lat_lon_lands_in_the_tile_mercantile_lands_it_in() {
    let mut wrong = Vec::new();
    for &(lat, lon, zoom, want_x, want_y) in MERCANTILE_TILE {
        let got = (lon_to_tile_x(lon, zoom), lat_to_tile_y(lat, zoom));
        if got != (want_x, want_y) {
            wrong.push(format!(
                "  lat {lat}, lon {lon}, z{zoom}: ours {got:?}, mercantile ({want_x}, {want_y})"
            ));
        }
    }
    assert!(
        wrong.is_empty(),
        "{} of {} cases disagree with mercantile 1.2.1:\n{}",
        wrong.len(),
        MERCANTILE_TILE.len(),
        wrong.join("\n")
    );
}

/// A tile's north-west corner is the one `mercantile.ul` reports.
#[test]
fn a_tile_corner_is_the_one_mercantile_reports() {
    const TOL_DEG: f64 = 1e-12;
    let mut worst = (0.0f64, String::new());
    for &(x, y, zoom, want_lat, want_lon) in MERCANTILE_UL {
        let (got_lat, got_lon) = (tile_to_lat(y, zoom), tile_to_lon(x, zoom));
        for (got, want, axis) in [(got_lat, want_lat, "lat"), (got_lon, want_lon, "lon")] {
            let err = (got - want).abs();
            if err > worst.0 {
                worst = (
                    err,
                    format!("tile ({x},{y},z{zoom}) {axis}: {got} vs {want}"),
                );
            }
        }
    }
    assert!(
        worst.0 <= TOL_DEG,
        "worst disagreement with mercantile.ul is {} deg (bar {TOL_DEG}): {}",
        worst.0,
        worst.1
    );
}

/// Nothing this returns is off the grid, at any latitude, longitude or zoom.
#[test]
fn no_input_produces_an_index_off_the_grid() {
    let lats = [
        90.0,
        89.999_999,
        MERCATOR_LAT_LIMIT_DEG + 1e-9,
        MERCATOR_LAT_LIMIT_DEG,
        85.05,
        60.0,
        0.0,
        -60.0,
        -85.05,
        -MERCATOR_LAT_LIMIT_DEG,
        -MERCATOR_LAT_LIMIT_DEG - 1e-9,
        -89.999_999,
        -90.0,
        -89.998_9,
        89.998_9,
    ];
    let lons = [
        -360.0,
        -180.000_001,
        -180.0,
        -179.999_999,
        -97.2778,
        0.0,
        127.021,
        179.999_999,
        180.0,
        180.000_001,
        360.0,
    ];
    for zoom in 0..=22u8 {
        let last = 2u32.saturating_pow(u32::from(zoom)) - 1;
        for &lat in &lats {
            let y = lat_to_tile_y(lat, zoom);
            assert!(
                y <= last,
                "lat {lat} at z{zoom} gave tile y {y}, past the last row {last}"
            );
        }
        for &lon in &lons {
            let x = lon_to_tile_x(lon, zoom);
            assert!(
                x <= last,
                "lon {lon} at z{zoom} gave tile x {x}, past the last column {last}"
            );
        }
    }
}

/// The poles land on the grid rather than saturating a `u32`.
#[test]
fn the_poles_land_on_the_grid_rather_than_saturating_a_u32() {
    for zoom in 0..=22u8 {
        let last = 2u32.saturating_pow(u32::from(zoom)) - 1;
        assert_eq!(
            lat_to_tile_y(90.0, zoom),
            0,
            "the north pole belongs in the top row at z{zoom}"
        );
        assert_eq!(
            lat_to_tile_y(-90.0, zoom),
            last,
            "the south pole belongs in the bottom row at z{zoom}"
        );
        assert_ne!(lat_to_tile_y(-90.0, zoom), u32::MAX);
    }
}

/// Mercator's `y` is computed in the spelling that survives the far south.
#[test]
fn the_transform_is_as_exact_in_the_south_as_in_the_north() {
    for zoom in [5u8, 10, 14, 18, 20] {
        let n = 2u32.pow(u32::from(zoom));
        for &lat in &[
            1.0, 30.0, 60.0, 85.0, 89.0, 89.9, 89.99, 89.999, 89.9999, 89.99999,
        ] {
            let north = lat_to_tile_y(lat, zoom);
            let south = lat_to_tile_y(-lat, zoom);
            assert_eq!(
                north,
                n - 1 - south,
                "z{zoom}: +{lat} is row {north} from the top but -{lat} is row {} from the bottom",
                n - 1 - south
            );
        }
    }
}

/// The tile corners this module computes are where `walkers` draws them.
#[test]
fn a_tile_corner_projects_where_walkers_draws_it() {
    const TOL_POINTS: f32 = 0.1;

    let clip = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1920.0, 1080.0));
    let mut worst = (0.0f32, String::new());

    for &(lat, lon, zoom, _, _) in MERCANTILE_TILE {
        if zoom > 18 {
            continue;
        }
        let mut memory = walkers::MapMemory::default();
        if memory.set_zoom(f64::from(zoom)).is_err() {
            continue;
        }
        let centre = walkers::lat_lon(lat, lon);
        let projector = walkers::Projector::new(clip, &memory, centre);

        let x = lon_to_tile_x(lon, zoom);
        let y = lat_to_tile_y(lat, zoom);
        let last = 2u32.pow(u32::from(zoom)) - 1;

        let here = projector.project(walkers::lat_lon(tile_to_lat(y, zoom), tile_to_lon(x, zoom)));

        if x < last {
            let east = projector.project(walkers::lat_lon(
                tile_to_lat(y, zoom),
                tile_to_lon(x + 1, zoom),
            ));
            let err = ((east.x - here.x) - TILE_SIDE_POINTS).abs();
            if err > worst.0 {
                worst = (
                    err,
                    format!(
                        "east of tile ({x},{y},z{zoom}) is {} pt away",
                        east.x - here.x
                    ),
                );
            }
            assert!(
                (east.y - here.y).abs() <= TOL_POINTS,
                "the east neighbour of ({x},{y},z{zoom}) is not on the same row"
            );
        }
        if y < last {
            let south = projector.project(walkers::lat_lon(
                tile_to_lat(y + 1, zoom),
                tile_to_lon(x, zoom),
            ));
            let err = ((south.y - here.y) - TILE_SIDE_POINTS).abs();
            if err > worst.0 {
                worst = (
                    err,
                    format!(
                        "south of tile ({x},{y},z{zoom}) is {} pt away",
                        south.y - here.y
                    ),
                );
            }
            assert!(
                (south.x - here.x).abs() <= TOL_POINTS,
                "the south neighbour of ({x},{y},z{zoom}) is not in the same column"
            );
        }
    }

    assert!(
        worst.0 <= TOL_POINTS,
        "our tile corners and walkers' projection disagree by {} points \
         (a tile is {TILE_SIDE_POINTS}): {}",
        worst.0,
        worst.1
    );
}

/// The named clamp latitude is the one whose Mercator `y` is exactly `π`.
#[test]
fn the_clamp_latitude_is_the_one_whose_mercator_y_is_pi() {
    let y = MERCATOR_LAT_LIMIT_DEG.to_radians().tan().asinh();
    assert!(
        (y - std::f64::consts::PI).abs() < 1e-12,
        "MERCATOR_LAT_LIMIT_DEG projects to {y}, not pi"
    );
    let truncated = 85.05f64.to_radians().tan().asinh();
    let short_m = (MERCATOR_LAT_LIMIT_DEG - 85.05) * squallar_geo::KM_PER_DEGREE_LAT * 1000.0;
    assert!(
        (truncated - std::f64::consts::PI).abs() > 1e-6,
        "85.05 was expected to be measurably short of the limit"
    );
    assert!(
        (125.0..126.0).contains(&short_m),
        "the truncation was measured at 125.51 m of meridian, got {short_m}"
    );
}

// ── The tile span: what the viewport needs, and nothing else. ──

/// The canvas the cache tiers in `tile_source` are sized against.
const CANVAS: egui::Vec2 = egui::vec2(1920.0, 1080.0);

/// Half a point. A gap this sweep is hunting is a whole tile column — 256
/// points at a whole zoom and 181 at the worst half-step — so the bar is
/// ~360x smaller than the smallest defect it must catch, and only wide enough
/// to absorb `Projector`'s `f32` return.
const TOL_POINTS: f32 = 0.5;

/// Where each sweep case centres the viewport. Mid-latitude, the far north
/// where Mercator's `y` bends hardest, the origin, the south, and hard against
/// each of the four grid edges where `tile_index`'s clamp bites.
const SPAN_ANCHORS: &[(f64, f64)] = &[
    (35.3331, -97.2778),  // KTLX Oklahoma City OK
    (65.0351, -147.5014), // PAPD Fairbanks AK — highest-latitude WSR-88D
    (0.0, 0.0),
    (-44.0, 170.0),
    (84.9, 0.0),   // hard against the north Mercator limit
    (-84.9, 0.0),  // and the south
    (0.0, 179.9),  // hard against the antimeridian, east
    (0.0, -179.9), // and west
];

/// Whole zooms and fractional ones on both sides of every `round()` step,
/// including the half where a tile is at its smallest on the glass.
const SPAN_ZOOMS: &[f64] = &[
    0.0, 1.5, 3.0, 3.25, 4.499, 4.5, 4.75, 5.0, 6.5, 8.0, 10.33, 12.0, 14.5, 16.0, 18.0,
];

/// How far into a tile the viewport's centre is placed, in tiles, on each axis.
const SUB_TILE_OFFSETS: &[f64] = &[0.0, 0.25, 0.5, 0.75];

/// The fractional tile coordinate of a position — `lon_to_tile_x` and
/// `lat_to_tile_y` before they floor.
fn fractional_tile(lat: f64, lon: f64, zoom: u8) -> (f64, f64) {
    let n = 2f64.powi(i32::from(zoom));
    let x = (lon + 180.0) / 360.0 * n;
    let y = (1.0 - lat.to_radians().tan().asinh() / std::f64::consts::PI) / 2.0 * n;
    (x, y)
}

/// A projector over `CANVAS` at `zoom`, centred in `anchor`'s tile cell but
/// `offset` of a tile in from that cell's north-west corner.
fn projector_for(
    zoom: f64,
    anchor: (f64, f64),
    offset: (f64, f64),
) -> Option<(walkers::Projector, u8)> {
    let tile_zoom = zoom.round() as u8;
    let n = 2f64.powi(i32::from(tile_zoom));
    let (fx, fy) = fractional_tile(anchor.0, anchor.1, tile_zoom);
    let (cx, cy) = (fx.floor() + offset.0, fy.floor() + offset.1);

    let lon = cx / n * 360.0 - 180.0;
    let lat = squallar_geo::mercator_y_to_lat_rad(std::f64::consts::PI * (1.0 - 2.0 * cy / n))
        .to_degrees();

    let mut memory = walkers::MapMemory::default();
    memory.set_zoom(zoom).ok()?;
    Some((
        walkers::Projector::new(canvas(), &memory, walkers::lat_lon(lat, lon)),
        tile_zoom,
    ))
}

/// The viewport every sweep case draws into.
fn canvas() -> egui::Rect {
    egui::Rect::from_min_size(egui::Pos2::ZERO, CANVAS)
}

/// The screen rect one tile is painted into, the long way round: the tile's two
/// corners as geography, projected back.
///
/// This is what `draw_tile_layer` used to hand `geo_corner_rect`, and it is the
/// reference `the_affine_tile_rect_agrees_with_the_geographic_round_trip`
/// measures [`walkers::Projector::tile_rect`] against. Every other test in this
/// file keeps using it, so the reference is not the thing under test.
fn geographic_tile_rect(projector: &walkers::Projector, x: u32, y: u32, zoom: u8) -> egui::Rect {
    crate::overlay_cache::geo_corner_rect(
        projector,
        (tile_to_lat(y, zoom), tile_to_lon(x, zoom)),
        (tile_to_lat(y + 1, zoom), tile_to_lon(x + 1, zoom)),
    )
}

/// The screen rect the whole span covers — the grid is regular, so the union
/// of its tiles is a rect.
fn span_rect(projector: &walkers::Projector, span: TileSpan, zoom: u8) -> egui::Rect {
    geographic_tile_rect(projector, span.west, span.north, zoom)
        .union(geographic_tile_rect(projector, span.east, span.south, zoom))
}

/// The screen rect the whole slippy grid covers. Past its edges there is no
/// tile to draw, so this is what bounds what coverage can even mean.
fn world_rect(projector: &walkers::Projector) -> egui::Rect {
    crate::overlay_cache::geo_corner_rect(
        projector,
        (MERCATOR_LAT_LIMIT_DEG, -180.0),
        (-MERCATOR_LAT_LIMIT_DEG, 180.0),
    )
}

/// The tile zoom offsets the parity sweep visits, relative to `round(zoom)`.
///
/// **Zero on its own would prove nothing.** At `tile_zoom == round(zoom)` and a
/// whole map zoom, a wrong-sign exponent gives the same 256-point side as the
/// right one, so a sweep that never leaves that row cannot fail for the one
/// mistake this arithmetic invites. Every other entry here separates them, and
/// each is a real configuration: `draw_tile_layer` picks
/// `tile_zoom = round(zoom) + zoom_bias`, and walkers' own
/// `interpolate_from_lower_zoom` stretches an ancestor from a shallower level
/// over a gap.
const TILE_ZOOM_BIASES: &[i32] = &[-2, -1, 0, 1, 2];

/// **The affine tile rect is the geographic round trip, to within `f32`.**
///
/// `Projector::tile_rect` replaces four `sinh`/`atan` → `tan`/`asinh` corner
/// pairs per tile with two multiplies and an add. The two must place and size
/// every tile alike; this measures the worst disagreement over the sweep rather
/// than asserting a belief about it, and prints it whether it passes or fails.
///
/// **Denominators.** The sweep is `SPAN_ZOOMS` (15) x `SPAN_ANCHORS` (8) x
/// `SUB_TILE_OFFSETS` squared (16) x `TILE_ZOOM_BIASES` (5) = 9,600 viewports,
/// minus the ones whose zoom or tile zoom is out of range, and every tile of
/// `tile_span` inside each. The tile total is the figure that matters and is
/// counted, not derived. Ranges: zoom 0 to 18 including four fractional steps
/// and both sides of a `round()` boundary; tile zoom `round(zoom) - 2` to
/// `+ 2`; the viewport centred at each of four sub-tile phases on each axis;
/// anchors at mid-latitude, at 65 N, at the origin, in the southern
/// hemisphere, and hard against all four grid edges, where `tile_index`'s clamp
/// bites and the span degenerates to a single column or row.
#[test]
fn the_affine_tile_rect_agrees_with_the_geographic_round_trip() {
    /// Two hundredths of a point, against a tile that is 181 points across at
    /// its smallest. The disagreement is the `f64` round trip through four
    /// transcendentals plus `Projector`'s `f32` return, and the measured worst
    /// is printed below — if it ever approaches this, the arithmetic moved.
    const TOL: f32 = 0.02;

    let mut viewports = 0usize;
    let mut tiles = 0usize;
    let mut worst = (0.0f32, String::new());

    for (zoom, anchor, offset) in span_sweep() {
        let Some((projector, round_zoom)) = projector_for(zoom, anchor, offset) else {
            continue;
        };

        for &bias in TILE_ZOOM_BIASES {
            let Ok(tile_zoom) = u8::try_from(i32::from(round_zoom) + bias) else {
                continue;
            };
            if tile_zoom > 20 {
                continue;
            }
            viewports += 1;

            let span = tile_span(&projector, canvas(), tile_zoom);
            for y in span.north..=span.south {
                for x in span.west..=span.east {
                    tiles += 1;

                    let want = geographic_tile_rect(&projector, x, y, tile_zoom);
                    let got = projector.tile_rect(walkers::TileId {
                        x,
                        y,
                        zoom: tile_zoom,
                    });

                    for (corner, err) in [
                        ("min.x", (got.min.x - want.min.x).abs()),
                        ("min.y", (got.min.y - want.min.y).abs()),
                        ("max.x", (got.max.x - want.max.x).abs()),
                        ("max.y", (got.max.y - want.max.y).abs()),
                    ] {
                        if err > worst.0 {
                            worst = (
                                err,
                                format!(
                                    "{corner} of tile ({x},{y},z{tile_zoom}) at map zoom \
                                     {zoom} (bias {bias}), anchor {anchor:?} offset \
                                     {offset:?}: affine {got:?} vs geographic {want:?}"
                                ),
                            );
                        }
                    }
                }
            }
        }
    }

    // Non-vacuity. The floors are a little under what the sweep above actually
    // reaches, so a narrowed `SPAN_*` list or a `tile_span` that stopped
    // naming tiles reddens here instead of passing on an empty sweep.
    assert!(
        viewports >= 6_000,
        "the sweep only built {viewports} viewports; it is not sweeping what this test claims"
    );
    assert!(
        tiles >= 500_000,
        "the sweep only compared {tiles} tiles; it is not sweeping what this test claims"
    );

    assert!(
        worst.0 <= TOL,
        "the affine tile rect and the geographic round trip disagree by {} points \
         over {tiles} tiles in {viewports} viewports: {}",
        worst.0,
        worst.1
    );

    println!(
        "tile_rect parity: {tiles} tiles, {viewports} viewports, worst {} pt",
        worst.0
    );
}

/// Every `(zoom, anchor, offset)` the three span sweeps visit.
fn span_sweep() -> impl Iterator<Item = (f64, (f64, f64), (f64, f64))> {
    SPAN_ZOOMS.iter().flat_map(|&zoom| {
        SPAN_ANCHORS.iter().flat_map(move |&anchor| {
            SUB_TILE_OFFSETS.iter().flat_map(move |&ox| {
                SUB_TILE_OFFSETS
                    .iter()
                    .map(move |&oy| (zoom, anchor, (ox, oy)))
            })
        })
    })
}

/// **No gap at any edge.** The tiles the span names cover every point of the
/// viewport that the slippy grid has a tile for.
#[test]
fn the_span_covers_the_whole_viewport() {
    let mut cases = 0usize;
    let mut short = Vec::new();

    for (zoom, anchor, offset) in span_sweep() {
        let Some((projector, tile_zoom)) = projector_for(zoom, anchor, offset) else {
            continue;
        };
        cases += 1;

        let clip = canvas();
        let span = tile_span(&projector, clip, tile_zoom);
        let drawn = span_rect(&projector, span, tile_zoom);
        // Off the grid's own edges there is nothing to cover.
        let need = clip.intersect(world_rect(&projector));
        if need.width() <= 0.0 || need.height() <= 0.0 {
            continue;
        }

        for (side, missing) in [
            ("west", drawn.left() - need.left()),
            ("north", drawn.top() - need.top()),
            ("east", need.right() - drawn.right()),
            ("south", need.bottom() - drawn.bottom()),
        ] {
            if missing > TOL_POINTS {
                short.push(format!(
                    "  z{zoom} anchor {anchor:?} offset {offset:?}: {missing} points of \
                     the viewport uncovered on the {side} (span {span:?})"
                ));
            }
        }
    }

    assert!(
        cases >= 1000,
        "the sweep must not go vacuous: only {cases} cases ran"
    );
    assert!(
        short.is_empty(),
        "{} of {cases} sweep cases leave a gap at the viewport edge:\n{}",
        short.len(),
        short.join("\n")
    );
}

/// **And nothing beyond it.** No tile in the span lies wholly off the
/// viewport. This is the assertion a `+ 1` on the far end fails: it appends a
/// full column east of the clip rect and a full row south of it, each tile of
/// which is fetched, cached and painted for nothing.
#[test]
fn the_span_names_no_tile_that_is_wholly_off_the_viewport() {
    let mut cases = 0usize;
    let mut wasted = Vec::new();
    let mut worst = 0.0f32;

    for (zoom, anchor, offset) in span_sweep() {
        let Some((projector, tile_zoom)) = projector_for(zoom, anchor, offset) else {
            continue;
        };
        cases += 1;

        let clip = canvas();
        let span = tile_span(&projector, clip, tile_zoom);

        for y in span.north..=span.south {
            for x in span.west..=span.east {
                let rect = geographic_tile_rect(&projector, x, y, tile_zoom);
                let overlap_x = rect.right().min(clip.right()) - rect.left().max(clip.left());
                let overlap_y = rect.bottom().min(clip.bottom()) - rect.top().max(clip.top());
                let off = (-overlap_x).max(-overlap_y);
                if off > TOL_POINTS {
                    worst = worst.max(off);
                    wasted.push(format!(
                        "  z{zoom} anchor {anchor:?} offset {offset:?}: tile \
                         ({x},{y},z{tile_zoom}) is {off} points clear of the viewport"
                    ));
                }
            }
        }
    }

    assert!(
        cases >= 1000,
        "the sweep must not go vacuous: only {cases} cases ran"
    );
    assert!(
        wasted.is_empty(),
        "{} tiles across {cases} sweep cases are drawn wholly off the viewport \
         (worst is {worst} points clear); first 12:\n{}",
        wasted.len(),
        wasted
            .iter()
            .take(12)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    );
}

/// The most tiles the span ever names over this sweep, at a whole zoom.
/// `tile_source`'s cache-sizing docs quote this figure.
const WHOLE_ZOOM_WORST: usize = 54;
/// And between two whole zooms, where a tile is drawn smaller than
/// `TILE_SIDE_POINTS` and more of them fit. See the test below.
const BETWEEN_ZOOMS_WORST: usize = 84;

/// What the loop asks for and what the sizing arithmetic reports are the same
/// number — at a whole zoom, which is the only claim the whole-zoom figure
/// makes.
#[test]
fn the_span_and_the_cache_sizing_agree_at_a_whole_zoom() {
    let budget = tiles_resident_at_whole_zoom(canvas(), 0, 1);

    let mut whole = 0usize;
    let mut between = (0usize, String::new());

    for (zoom, anchor, offset) in span_sweep() {
        let Some((projector, tile_zoom)) = projector_for(zoom, anchor, offset) else {
            continue;
        };
        let drawn = tile_span(&projector, canvas(), tile_zoom).tiles();
        if (zoom - f64::from(tile_zoom)).abs() < 1e-9 {
            whole = whole.max(drawn);
        } else if drawn > between.0 {
            between = (
                drawn,
                format!("z{zoom} anchor {anchor:?} offset {offset:?}"),
            );
        }
    }

    assert_eq!(
        whole, budget,
        "at a whole zoom the loop's worst case and `tiles_resident_for` must be \
         the same number, not merely close"
    );
    assert_eq!(
        whole, WHOLE_ZOOM_WORST,
        "the whole-zoom worst case over this sweep is a measured figure, and \
         `tile_source`'s cache-sizing docs quote it"
    );

    // Between two whole zooms a tile is drawn `256 * 2^(zoom - round(zoom))`
    // points across — 181 at the half step — so more of them fit the same
    // window. That is the larger figure, it is the one a cache must be sized
    // against, and it is the one `tiles_resident_for` reports; the whole-zoom
    // number above is never a cache size.
    assert_eq!(
        between.0, BETWEEN_ZOOMS_WORST,
        "the between-zooms worst case is a measured figure and moved; the worst \
         case this sweep found is at {}",
        between.1
    );
    assert_eq!(
        tiles_resident_for(canvas(), 0, 1),
        between.0,
        "`tiles_resident_for` must carry the between-zooms worst case the sweep \
         measures at {}, and `tiles_resident_at_whole_zoom` the {budget} at a \
         whole zoom — one function per question",
        between.1
    );
    assert!(
        between.0 > budget,
        "the between-zooms case must be the larger of the two, or the scale \
         term points the wrong way"
    );
}

/// **The cache holds the working set at every zoom, not only the whole ones —
/// by its floor, not by its budget.** The sweep's worst case over the whole
/// zoom range, plus the ancestor net's bound, is what a pass reports as its
/// working set; a `ByteLru` floored there holds every one of those entries
/// at a budget of one byte and evicts only what lies beyond. An LRU that
/// evicted inside the working set would evict a tile still on the glass, and
/// the next frame would re-enter `request_once` for it: a network fetch and a
/// decode against the per-source, per-pass wasm decode budget, for a tile the
/// user never stopped looking at. This used to gate a count constant against
/// the sweep; the constant is gone and the floor is measured per pass, so
/// what is gated is the mechanism the floor rests on.
#[test]
fn the_cache_holds_the_working_set_at_every_zoom() {
    use crate::tile_source::byte_lru::{ByteLru, MARKER_BYTES};

    let mut cases = 0usize;
    let mut worst = (0usize, String::new());

    for (zoom, anchor, offset) in span_sweep() {
        let Some((projector, tile_zoom)) = projector_for(zoom, anchor, offset) else {
            continue;
        };
        cases += 1;
        let drawn = tile_span(&projector, canvas(), tile_zoom).tiles();
        if drawn > worst.0 {
            worst = (
                drawn,
                format!("z{zoom} anchor {anchor:?} offset {offset:?}"),
            );
        }
    }

    assert!(
        cases >= 1000,
        "the sweep must not go vacuous: only {cases} cases ran"
    );
    assert_eq!(
        worst.0, BETWEEN_ZOOMS_WORST,
        "the worst case over the whole zoom range is a measured figure and \
         moved; the worst this sweep found is at {}",
        worst.1
    );

    // The floor a pass over this canvas reports: the drawn worst case plus
    // the net's bound, as `draw_tile_layer` asks for both.
    let floor = tiles_resident_with_warm_net(canvas(), 0, 1);
    assert!(
        worst.0 <= floor,
        "the sweep's worst case {} exceeds the working-set bound {floor} the pass would report",
        worst.0
    );
    let mut cache: ByteLru<usize, ()> = ByteLru::new(1);
    cache.set_floor_entries(floor);
    let mut evicted = Vec::new();
    for entry in 0..floor {
        cache.put(entry, (), MARKER_BYTES, &mut evicted);
    }
    assert!(
        evicted.is_empty(),
        "a {CANVAS:?}-point canvas keeps {floor} tiles resident per source at {} and the \
         net, and the floor let {} of them go at a budget of one byte: the cache evicts \
         tiles that are still on the glass",
        worst.1,
        evicted.len()
    );
    assert_eq!(cache.len(), floor);
    // History beyond the floor is the budget's to reclaim.
    cache.put(floor, (), MARKER_BYTES, &mut evicted);
    assert_eq!(
        evicted.len(),
        1,
        "the entry beyond the working set is history"
    );
}

/// `tiles_resident_for` reports the worst case over the **whole** zoom range,
/// which is the question every caller sizing a cache against it is asking.
///
/// It is an upper bound on every case the sweep measures, and it is attained —
/// a bound nothing reaches would size the cache for a viewport that does not
/// exist.
#[test]
fn tiles_resident_for_reports_the_worst_case_over_the_zoom_range() {
    let budget = tiles_resident_for(canvas(), 0, 1);
    let mut cases = 0usize;
    let mut over = Vec::new();
    let mut worst = 0usize;

    for (zoom, anchor, offset) in span_sweep() {
        let Some((projector, tile_zoom)) = projector_for(zoom, anchor, offset) else {
            continue;
        };
        cases += 1;
        let drawn = tile_span(&projector, canvas(), tile_zoom).tiles();
        worst = worst.max(drawn);
        if drawn > budget {
            over.push(format!(
                "  z{zoom} anchor {anchor:?} offset {offset:?}: {drawn} tiles \
                 against a reported {budget}"
            ));
        }
    }

    assert!(
        cases >= 1000,
        "the sweep must not go vacuous: only {cases} cases ran"
    );
    assert!(
        over.is_empty(),
        "{} of {cases} sweep cases ask for more tiles than `tiles_resident_for` \
         reports, so anything sized against it is undersized; first 12:\n{}",
        over.len(),
        over.iter().take(12).cloned().collect::<Vec<_>>().join("\n")
    );
    assert_eq!(
        budget, worst,
        "`tiles_resident_for` must report the worst case the sweep measures, \
         not merely bound it"
    );
}

/// The two counts at real canvases, pinned. The bracket arguments in
/// `squallar-device-profile` quote these figures.
///
/// Sizes are in **points**: a 4K panel at the 2x scaling it is nearly always
/// run at presents the 1920x1080 row, not the 3840x2160 one.
///
/// **The user's own 2878x1651 window is the last row, and it is the row the
/// arithmetic overstates.** The grid arithmetic says 104 tiles at a whole
/// zoom and 187 between zooms for a rect that size; the rig measured 86 and
/// 174 on the real window (2026-09-02, zoom 14.0 and 13.5, the working-set
/// floor in place — the ~106 first read at 13.5 was the count cap's ceiling
/// on what could be seen distinct, not the working set), because the map
/// pane is 0.8-0.9 of the canvas — the chrome takes the rest — and this
/// function prices the rect it is given. The two measured figures are held
/// under the two predictions here so the overstatement stays a known
/// direction and never a surprise.
#[test]
fn the_resident_counts_are_the_ones_the_tier_table_quotes() {
    let canvas = |w: f32, h: f32| egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(w, h));

    for (w, h, whole, worst) in [
        (1920.0, 1080.0, 54usize, 84usize),
        (1920.0, 1200.0, 54, 96),
        (2560.0, 1440.0, 77, 144),
        (3840.0, 2160.0, 160, 299),
        (2878.0, 1651.0, 104, 187),
    ] {
        assert_eq!(
            tiles_resident_at_whole_zoom(canvas(w, h), 0, 1),
            whole,
            "the whole-zoom count for {w}x{h} points moved"
        );
        assert_eq!(
            tiles_resident_for(canvas(w, h), 0, 1),
            worst,
            "the worst-case count for {w}x{h} points moved"
        );
        assert!(worst > whole, "the scale term must only ever add tiles");
    }

    // The user's window, as measured on the rig against what the rect
    // arithmetic predicts for the whole canvas.
    use super::measured::{HALF_STEP_TILES, WHOLE_ZOOM_TILES};
    assert!(
        WHOLE_ZOOM_TILES <= tiles_resident_at_whole_zoom(canvas(2878.0, 1651.0), 0, 1)
            && HALF_STEP_TILES <= tiles_resident_with_warm_net(canvas(2878.0, 1651.0), 0, 1),
        "the rig measured more tiles on the user's window than the whole-canvas arithmetic \
         predicts: the map pane is now larger than the canvas"
    );

    // One zoom of bias halves the tile on both axes, so it costs exactly what
    // doubling the canvas on both axes costs.
    assert_eq!(
        tiles_resident_for(canvas(1920.0, 1080.0), 1, 1),
        tiles_resident_for(canvas(3840.0, 2160.0), 0, 1),
    );
    assert_eq!(tiles_resident_for(canvas(1920.0, 1080.0), 1, 1), 299);
    // And `layers` is a flat multiplier: one cache per layer's own source.
    assert_eq!(tiles_resident_for(canvas(1920.0, 1080.0), 0, 2), 168);

    // The degenerate inputs still answer zero rather than panicking.
    assert_eq!(tiles_resident_for(canvas(f32::NAN, 1080.0), 0, 1), 0);
    assert_eq!(tiles_resident_for(canvas(1920.0, f32::INFINITY), 0, 1), 0);
    assert_eq!(tiles_resident_for(canvas(1920.0, 1080.0), 255, 1), 0);
}

/// **The snapped level is the floor and the sharp level the round**, the bias
/// after and the source's ceiling last — the one place the rule is spelled,
/// held at the half step where the two rules part and in the lower half where
/// they agree. And what the rule buys on the glass, on the projector the
/// other tests use: at the half step the sharp span is past the whole-zoom
/// bound and the snapped span inside it.
#[test]
fn the_snapped_level_is_the_floor_and_the_sharp_level_the_round() {
    for zoom in [13.0, 13.25, 13.49] {
        assert_eq!(tile_zoom_for(zoom, false, 0, 22), 13, "sharp at {zoom}");
        assert_eq!(
            tile_zoom_for(zoom, true, 0, 22),
            13,
            "snapped at {zoom}: the lower half of a zoom agrees with round"
        );
    }
    for zoom in [13.5, 13.75, 13.99] {
        assert_eq!(tile_zoom_for(zoom, false, 0, 22), 14, "sharp at {zoom}");
        assert_eq!(
            tile_zoom_for(zoom, true, 0, 22),
            13,
            "snapped at {zoom}: one level up"
        );
    }
    // The bias applies after the rule and the ceiling last, for both.
    assert_eq!(tile_zoom_for(13.5, false, 1, 22), 15);
    assert_eq!(tile_zoom_for(13.5, true, 1, 22), 14);
    assert_eq!(tile_zoom_for(13.5, false, 0, 13), 13);
    assert_eq!(tile_zoom_for(13.5, true, 0, 12), 12);
    assert_eq!(
        tile_zoom_for(13.5, true, 255, 22),
        22,
        "the bias saturates before the ceiling clamps"
    );

    let (projector, rounded) =
        projector_for(13.5, (35.33, -97.28), (0.5, 0.5)).expect("zoom 13.5 is in range");
    assert_eq!(
        rounded, 14,
        "fixture: the helper rounds as the sharp rule does"
    );
    let sharp = tile_span(&projector, canvas(), tile_zoom_for(13.5, false, 0, 22)).tiles();
    let snapped = tile_span(&projector, canvas(), tile_zoom_for(13.5, true, 0, 22)).tiles();
    let whole = tiles_resident_at_whole_zoom(canvas(), 0, 1);
    assert!(
        sharp > whole,
        "the sharp span ({sharp}) at the half step is not past the whole-zoom bound ({whole})"
    );
    assert!(
        snapped <= whole,
        "the snapped span ({snapped}) is past the whole-zoom bound ({whole}) the rung promises"
    );
}

/// **Each bracket's styled floor holds what its argument says, in bytes, and
/// the worst case exceeds the wasm floor — which is what the working-set
/// floor and the snapping rung are for.** The old count test said the wasm
/// arm "overruns at 1440p by design"; the byte arm's statement is different
/// in kind. At the typical entry cost every floor holds the user's 2878x1651
/// window between zooms (193 entries with the net) many times over. At the
/// measured city-core tail the wasm floor holds 34 entries against the 174
/// the window measured at the half step: the floor in entries keeps those
/// 174 resident as overrun, and the tile-sharpness rung snaps to the whole
/// zoom (at most 86) when a dwell of overrun says so. The desktop floor holds
/// the user's whole-zoom set at the tail and the half-step set at its
/// measured cost twice over, its step holds 2560x1440 between zooms and its
/// ceiling holds 3840x2160, so a workstation with a real driver is never in
/// the floor's care at all. The arithmetic is in every message.
#[test]
fn each_tier_holds_the_canvas_its_docs_claim() {
    use super::measured::{HALF_STEP_BYTES, HALF_STEP_TILES, WHOLE_ZOOM_BYTES, WHOLE_ZOOM_TILES};
    use crate::tile_source::{
        MEASURED_STYLED_ENTRY_BYTES, TYPICAL_STYLED_ENTRY_BYTES, worst_case_entries,
    };
    use squallar_device_profile::budget::BudgetLimits;

    let canvas = |w: f32, h: f32| egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(w, h));
    let user_window = tiles_resident_with_warm_net(canvas(2878.0, 1651.0), 0, 1);
    const MEASURED_ON_THE_USERS_WINDOW: usize = HALF_STEP_TILES;
    assert!(
        user_window >= MEASURED_ON_THE_USERS_WINDOW,
        "fixture: the arithmetic ({user_window}) must bound the measured {MEASURED_ON_THE_USERS_WINDOW}"
    );

    for limits in BudgetLimits::SHIPPED {
        let floor = limits.tile_styled_bytes.floor;
        let typical = floor / TYPICAL_STYLED_ENTRY_BYTES;
        assert!(
            typical >= 8 * user_window,
            "the {} styled floor ({} MiB) holds {typical} typical entries, under eight of the \
             user's {user_window}-tile window",
            limits.name,
            floor >> 20,
        );
    }

    // The wasm floor at the tail: short of the user's window, by design and
    // by name.
    let wasm_floor = BudgetLimits::WASM.tile_styled_bytes.floor as u64;
    let at_tail = worst_case_entries(wasm_floor);
    assert!(
        at_tail < MEASURED_ON_THE_USERS_WINDOW,
        "the wasm styled floor ({} MiB) holds {at_tail} entries at the {MEASURED_STYLED_ENTRY_BYTES} B \
         tail, at least the {MEASURED_ON_THE_USERS_WINDOW} the user's window measured: the worst \
         case now fits the floor and the snapping rung's premise is gone",
        wasm_floor >> 20,
    );
    assert!(
        at_tail >= 30,
        "the wasm styled floor holds only {at_tail} city-core entries; a phone-sized viewport \
         is 20-35 tiles and the floor should hold most of one"
    );
    // And the set the rung snaps the user's window to fits that floor, at
    // its measured cost: the rung has somewhere to go.
    assert!(
        WHOLE_ZOOM_BYTES <= wasm_floor,
        "the user's whole-zoom set ({WHOLE_ZOOM_BYTES} B) no longer fits the wasm styled floor \
         ({wasm_floor} B): snapping cannot bring the window under budget"
    );

    // Desktop: the floor holds the user's whole-zoom set at the tail and the
    // half-step set at its measured cost twice over -- at the tail the
    // half-step set (254 MB) would overrun it and snap, to a set it holds --
    // the step holds 2560x1440 between zooms, the ceiling holds 4K.
    let desktop = BudgetLimits::DESKTOP.tile_styled_bytes;
    let qhd = tiles_resident_for(canvas(2560.0, 1440.0), 0, 1);
    let uhd = tiles_resident_for(canvas(3840.0, 2160.0), 0, 1);
    assert!(
        worst_case_entries(desktop.floor as u64) >= WHOLE_ZOOM_TILES,
        "the desktop styled floor holds {} tail entries, under the {WHOLE_ZOOM_TILES} the user's \
         window measured at a whole zoom: the set the rung snaps to no longer fits the floor",
        worst_case_entries(desktop.floor as u64)
    );
    assert!(
        desktop.floor as u64 >= 2 * HALF_STEP_BYTES,
        "the desktop styled floor ({} MiB) no longer holds the user's measured half-step set \
         ({HALF_STEP_BYTES} B) twice over: the desktop arm would snap on the user's own window",
        desktop.floor >> 20
    );
    assert!(
        worst_case_entries(desktop.step as u64) >= qhd,
        "the desktop styled step holds {} tail entries, under the {qhd} a 2560x1440 canvas \
         keeps between zooms",
        worst_case_entries(desktop.step as u64)
    );
    assert!(
        worst_case_entries(desktop.ceiling as u64) >= uhd,
        "the desktop styled ceiling holds {} tail entries, under the {uhd} a 3840x2160 canvas \
         keeps between zooms",
        worst_case_entries(desktop.ceiling as u64)
    );
}

// ---------------------------------------------------------------------------
// A style change re-styles the live source in place.
// ---------------------------------------------------------------------------

/// **A theme flip re-styles the live source; it does not rebuild it.**
///
/// The rebuild was the v1 shape, and it refetched every visible tile; since
/// `HttpsTiles::set_style` the flip is served out of the source's parsed
/// cache with zero fetches (`tile_source::tests::archive` pins the zero).
/// What this pins is the state machine: the slot survives, `base_builds`
/// stays where the first frame put it, and `base_restyles` is what moves.
#[test]
fn a_theme_flip_restyles_the_live_source_instead_of_rebuilding() {
    let ctx = egui::Context::default();
    let mut state = MapTileState::default();

    state.ensure_base_tiles(true, &Default::default(), &ctx);
    assert!(
        state.tiles.is_some() && state.current_theme_is_dark,
        "fixture: a dark frame must make the dark source"
    );
    assert_eq!(state.base_builds, 1, "fixture: the first frame builds");
    assert_eq!(state.base_restyles, 0, "fixture: nothing has flipped yet");

    state.ensure_base_tiles(false, &Default::default(), &ctx);
    assert!(
        state.tiles.is_some() && !state.current_theme_is_dark,
        "the flip must keep a live source in the slot"
    );
    assert_eq!(
        (state.base_builds, state.base_restyles),
        (1, 1),
        "a theme flip is a restyle of the live source, not a rebuild"
    );

    state.ensure_base_tiles(false, &Default::default(), &ctx);
    assert_eq!(
        (state.base_builds, state.base_restyles),
        (1, 1),
        "an unchanged frame must neither rebuild nor restyle"
    );

    state.ensure_base_tiles(true, &Default::default(), &ctx);
    assert_eq!(
        (state.base_builds, state.base_restyles),
        (1, 2),
        "flipping back is another restyle of the same source"
    );
}

/// **The flip keeps the source**, rather than the single slot making the
/// claim true by construction: the frame after the flip, before anything is
/// drawn, the slot still holds the very source whose parsed cache the
/// restyle is draining into -- emptying it here would forfeit the cache the
/// zero-fetch contract stands on.
#[test]
fn the_flip_keeps_the_source_whose_caches_serve_the_restyle() {
    let ctx = egui::Context::default();
    let mut state = MapTileState::default();

    state.ensure_base_tiles(true, &Default::default(), &ctx);
    assert!(state.tiles.is_some(), "fixture: the dark source exists");

    state.ensure_base_tiles(false, &Default::default(), &ctx);
    assert!(
        !state.current_theme_is_dark,
        "the state must have adopted the light theme"
    );

    // The take/restore round-trip the per-pane draw makes must find the
    // restyled source there, not a hole.
    let taken = state.take_base_tiles();
    assert!(
        taken.is_some(),
        "the flip emptied the slot: the parsed cache went with it and the \
         restyle has nothing to draw from"
    );
    state.restore_base_tiles(taken);
    assert_eq!(state.base_builds, 1, "no rebuild happened across the flip");
}

/// **A changed source-layer set re-styles the base source; an unchanged one
/// does not churn it; and the release still keeps no source in the slot.** The
/// comparison in `ensure_base_tiles` is the whole mechanism a toggle flip
/// rides -- nothing signals it -- so an `ensure_` that stopped comparing
/// would leave every flip invisible until the next theme change.
#[test]
fn a_changed_source_layer_set_restyles_the_base_source() {
    let ctx = egui::Context::default();
    let mut state = MapTileState::default();
    let empty = std::collections::BTreeSet::new();

    state.ensure_base_tiles(true, &empty, &ctx);
    assert_eq!(state.base_builds, 1, "fixture: the first frame builds");

    state.ensure_base_tiles(true, &empty, &ctx);
    assert_eq!(
        (state.base_builds, state.base_restyles),
        (1, 0),
        "an unchanged set must not churn the source"
    );

    let disabled: std::collections::BTreeSet<String> = ["water".to_owned()].into();
    state.ensure_base_tiles(true, &disabled, &ctx);
    assert_eq!(
        (state.base_builds, state.base_restyles),
        (1, 1),
        "a changed set is the toggle flip, and it must restyle in place"
    );
    assert!(
        state.tiles.is_some(),
        "the restyle left a source in the slot"
    );

    state.ensure_base_tiles(true, &disabled, &ctx);
    assert_eq!(
        (state.base_builds, state.base_restyles),
        (1, 1),
        "the new set is now the styled set"
    );

    // The release parks the source: the slot empties, so nothing draws it and
    // it asks the network for nothing, but the next ensure moves it back
    // rather than paying a build. This assertion used to read `2` -- "coming
    // back is a fresh build" -- which was the v1 layer-off contract; see
    // `MapTileState::parked_base` for the measured reason it changed.
    state.release_base_tiles();
    assert!(state.tiles.is_none(), "the release must empty the slot");
    state.ensure_base_tiles(true, &disabled, &ctx);
    assert_eq!(
        state.base_builds, 1,
        "coming back built a second source, so the release dropped the first \
         one -- which joins its IO thread on the frame thread and throws away \
         the parsed cache the layer is about to want back"
    );
    assert_eq!(
        state.base_restyles, 1,
        "coming back with the set it left under must not restyle: nothing \
         about the style changed while the layer was off"
    );
}

/// **A flip that happens while the layer is OFF must land on the source that
/// comes back.**
///
/// The hazard is an ordering one and it is invisible to every count taken with
/// the slot full. `ensure_base_tiles` decides whether to restyle by comparing
/// the theme and the detail set against what the slot was last styled with,
/// and it can only call `set_style` on a source that is *in* the slot. Unpark
/// after that comparison and the comparison sees an empty slot: nothing gets
/// restyled, the remembered fields are updated anyway to say it did, and the
/// source is then moved back in still styled for the theme the user has since
/// left. The map comes back in the wrong colours and no counter is wrong.
///
/// **Measured as a real hole, not a hypothetical**: with the unpark moved below
/// the restyle block, the whole `squallar-egui` suite -- 1470 tests -- passed.
/// The pre-existing restyle assertions could not see it because they never
/// release, and the release assertions could not see it because they never
/// change the style across the release. This test changes both halves at once,
/// which is the only arrangement that discriminates.
#[test]
fn a_flip_while_the_layer_is_off_restyles_the_source_that_comes_back() {
    let ctx = egui::Context::default();
    let mut state = MapTileState::default();
    let empty = std::collections::BTreeSet::new();

    state.ensure_base_tiles(true, &empty, &ctx);
    state
        .tiles
        .as_mut()
        .expect("the first ensure builds a source")
        .pump();
    let marked = state
        .tiles
        .as_ref()
        .expect("the source is still in the slot")
        .pumps();
    assert_eq!(
        (marked, state.base_builds, state.base_restyles),
        (1, 1, 0),
        "fixture: one source, one pump on its ledger, nothing flipped yet"
    );

    // The layer goes off, and only THEN do the theme and the detail set change
    // -- the user flipping to light mode, or turning a map-detail row off,
    // while the basemap layer is switched off.
    state.release_base_tiles();
    let disabled: std::collections::BTreeSet<String> = ["water".to_owned()].into();
    state.ensure_base_tiles(false, &disabled, &ctx);

    assert_eq!(
        state
            .tiles
            .as_ref()
            .expect("the layer came back with no source")
            .pumps(),
        marked,
        "the layer came back as a different object, so this is a rebuild and \
         not a park"
    );
    assert_eq!(
        state.base_builds, 1,
        "coming back paid for a second source construction"
    );
    assert_eq!(
        state.base_restyles, 1,
        "the source came back WITHOUT being restyled, so it is still styled \
         for the theme and the detail set the user left. The restyle \
         comparison ran while the slot was empty, found nothing to call \
         `set_style` on, and updated the remembered style anyway -- so every \
         later frame agrees the source is correctly styled and it is not"
    );
    assert!(
        !state.current_theme_is_dark,
        "the state did not adopt the theme it was handed"
    );
}

/// **A layer toggle must not drop the tile source**, because dropping it is a
/// blocking join on the frame thread.
///
/// `HttpsTiles` owns a `runtime::Runtime` whose `Drop` joins the IO thread,
/// and that thread's tokio runtime waits for `spawn_blocking` tile
/// tessellations that have already started. Measured release 2026-08-31 on the
/// committed Monaco fixture: **up to 13.1 ms** of frame-thread block, against
/// 0.034 ms for a source with no IO thread. Scene D's figure of merit is a
/// max, and one stalled click frame is the whole defect.
///
/// **Identity, not a count, and not a clock.** The pump ledger is per-source
/// and starts at zero, so a source that comes back still carrying the pumps we
/// put into it is necessarily the same object -- there is no arrangement of
/// rebuilds that reproduces it. The wall-clock cost this stands in for is not
/// asserted here on purpose: a timing assertion would red-gate on a loaded box
/// and this workspace counts the operation instead.
///
/// This state is **online** -- `MapTileState::default()` has not been through
/// `go_offline_for_tests`, so `ensure_base_tiles` takes the real `base_source`
/// arm and the release really does drop a real IO thread. A version of this
/// test written against an inert source would pass on both sides of the fix,
/// because `runtime::inert()` holds no thread to join.
#[test]
fn a_layer_toggle_parks_the_base_source_rather_than_joining_its_io_thread() {
    let ctx = egui::Context::default();
    let mut state = MapTileState::default();
    let empty = std::collections::BTreeSet::new();

    state.ensure_base_tiles(true, &empty, &ctx);
    for _ in 0..3 {
        state
            .tiles
            .as_mut()
            .expect("the first ensure builds a source")
            .pump();
    }
    let marked = state
        .tiles
        .as_ref()
        .expect("the source is still in the slot")
        .pumps();
    assert_eq!(
        marked, 3,
        "fixture: the pump ledger did not count the pumps, so it cannot \
         identify a source below"
    );

    state.release_base_tiles();
    assert!(
        state.tiles.is_none(),
        "a switched-off layer must draw nothing, parked or not"
    );

    state.ensure_base_tiles(true, &empty, &ctx);
    assert_eq!(
        state
            .tiles
            .as_ref()
            .expect("the layer came back with no source at all")
            .pumps(),
        marked,
        "the source that came back has a fresh pump ledger, so it is a new \
         object and the toggle dropped the old one on the frame thread"
    );
    assert_eq!(
        state.base_builds, 1,
        "the toggle paid for a second source construction"
    );
}

/// [`a_layer_toggle_parks_the_base_source_rather_than_joining_its_io_thread`]
/// for the terrain slot, which owns the same kind of `Runtime` and ran the
/// same join. The terrain half had no unit test of its own before this.
#[test]
fn a_layer_toggle_parks_the_terrain_source_too() {
    let ctx = egui::Context::default();
    let mut state = MapTileState::default();

    state.ensure_terrain_tiles(&ctx);
    for _ in 0..2 {
        state
            .terrain
            .as_mut()
            .expect("the first ensure builds a terrain source")
            .pump();
    }
    let marked = state
        .terrain
        .as_ref()
        .expect("the terrain source is still in the slot")
        .pumps();
    assert_eq!(marked, 2, "fixture: the pump ledger did not count");

    state.release_terrain_tiles();
    assert!(
        state.terrain.is_none(),
        "a switched-off Terrain layer must draw nothing"
    );

    state.ensure_terrain_tiles(&ctx);
    assert_eq!(
        state
            .terrain
            .as_ref()
            .expect("the Terrain layer came back with no source")
            .pumps(),
        marked,
        "the terrain source that came back is a new object, so the toggle \
         dropped the old one and joined its IO thread on the frame thread"
    );
}

/// A suspend or a graphics reset really does let go, park included.
///
/// The park exists to survive a **toggle**. A source parked across a suspend
/// would hold its LRU, its parsed cache and its IO thread for as long as the
/// app sat in the background, which is the opposite of what `clear` is for.
#[test]
fn a_clear_lets_go_of_the_parked_sources_as_well() {
    let ctx = egui::Context::default();
    let mut state = MapTileState::default();
    let empty = std::collections::BTreeSet::new();

    state.ensure_base_tiles(true, &empty, &ctx);
    state.ensure_terrain_tiles(&ctx);
    state
        .tiles
        .as_mut()
        .expect("a base source was built")
        .pump();
    let marked = state.tiles.as_ref().expect("still there").pumps();
    assert_eq!(marked, 1, "fixture: the pump was not counted");

    state.release_base_tiles();
    state.release_terrain_tiles();
    state.clear();

    state.ensure_base_tiles(true, &empty, &ctx);
    assert_eq!(
        state.tiles.as_ref().expect("rebuilt after clear").pumps(),
        0,
        "the source from before the clear came back, so a suspended app is \
         still holding the caches and the IO thread the clear exists to release"
    );
}

/// Residency is bounded by the flips rather than grown by them.
///
/// **One source, not two -- and now also not a churn of ones.** Labels used
/// to be a second tile pyramid over the same ground, so this asserted two;
/// the vector basemap draws them out of the tile it already has, and a flip
/// re-styles that one source rather than replacing it.
#[test]
fn repeated_flips_never_hold_more_than_the_one_live_source() {
    let ctx = egui::Context::default();
    let mut state = MapTileState::default();

    for round in 0..6 {
        let is_dark = round % 2 == 0;
        state.ensure_base_tiles(is_dark, &Default::default(), &ctx);
        assert_eq!(
            state.current_theme_is_dark, is_dark,
            "round {round} did not adopt the theme it was handed"
        );
        assert!(
            state.tiles.is_some(),
            "round {round} (dark = {is_dark}) left the map with no source"
        );
    }

    assert_eq!(
        state.base_builds, 1,
        "six flips over one session must reuse the one source they started with"
    );
}

// ---------------------------------------------------------------------------
// The archive URL consts, enumerated from the source rather than hand-listed
// ---------------------------------------------------------------------------

/// `tiles.rs`'s own source, for [`declared_archive_url_consts`].
///
/// Read as text because the thing being ratcheted is *declaration*: a fifth
/// archive const that nothing lists is invisible to any check written in terms
/// of the four names somebody already remembered. The idiom is
/// `arch_ratchets.rs`'s.
const TILES_SOURCE: &str = include_str!("../tiles.rs");

/// Every `pub const <NAME>_ARCHIVE_URL: &str = "https://…"` in `TILES_SOURCE`.
///
/// The two halves of the match are both load-bearing. The **name** must end in
/// `_ARCHIVE_URL` exactly, which is what keeps `HEIGHT_ARCHIVE_URL_ENV` — the
/// name of an environment variable, not of an archive — out of the set. The
/// **value** must be an `https://` literal, which keeps a future const that
/// derives its URL rather than spelling one from being read as a literal that
/// happens to sit nearby.
///
/// A const that is declared some third way is not caught, and that is the
/// known edge: this ratchets the shape the four real ones are written in, and
/// a fifth written differently is the case the `len` comparison in the caller
/// still catches from the other side.
fn declared_archive_url_consts() -> std::collections::BTreeMap<String, String> {
    const DECL: &str = "pub const ";
    const TYPED: &str = ": &str =";

    let mut found = std::collections::BTreeMap::new();
    for (at, _) in TILES_SOURCE.match_indices(DECL) {
        let rest = &TILES_SOURCE[at + DECL.len()..];
        // One declaration, bounded at its own terminator so a scan can never
        // read a name from one item and a literal from the next.
        let Some(end) = rest.find(';') else { continue };
        let item = &rest[..end];

        let Some(name_end) = item.find(TYPED) else {
            continue;
        };
        let name = item[..name_end].trim();
        if !name.ends_with("_ARCHIVE_URL") {
            continue;
        }

        let value = &item[name_end + TYPED.len()..];
        let Some(open) = value.find('"') else {
            continue;
        };
        let after = &value[open + 1..];
        let Some(close) = after.find('"') else {
            continue;
        };
        let url = &after[..close];
        if url.starts_with("https://") {
            found.insert(name.to_owned(), url.to_owned());
        }
    }
    found
}

// ---------------------------------------------------------------------------
// The archive URL set, and the generations that follow from it
// ---------------------------------------------------------------------------

/// **The height archives have not been built, and this is what says so.**
///
/// Both height URLs carry [`HEIGHT_ARCHIVE_GENERATION_PLACEHOLDER`] instead of
/// a `<12-hex>-<YYYYMMDD>` generation, because inventing one that looks right
/// would compile, satisfy every other pin in the tree, and 404 in the field.
///
/// # This test is meant to go red
///
/// The day a real terrain-RGB archive is published and its generation is
/// pasted into `tiles.rs`, this test fails. That is its whole purpose: the
/// failure message below is the checklist of everything that has to move in
/// the same commit, and it cannot be skipped, because the URLs cannot be
/// changed without reddening it.
#[test]
fn the_height_archives_are_still_unpublished() {
    const CHECKLIST: &str = "\
A real generation has been configured for a height archive. Everything below \
moves in the SAME commit, or the build ships a URL that answers 404:\n\
  1. squallar-egui/src/tiles.rs -- HEIGHT_ARCHIVE_URL and CONUS_HEIGHT_ARCHIVE_URL\n\
  2. squallar-web/sw.js         -- ARCHIVE_URLS, pinned BOTH ways by \
squallar-web/tests/pwa_assets.rs\n\
  3. this test                  -- delete it, or re-point it at whatever is still \
unpublished\n\
Nothing else needs to move: the Android host allowlist and the block-cache live \
set are both derived from the consts in (1).";

    for (name, url) in [
        ("HEIGHT_ARCHIVE_URL", HEIGHT_ARCHIVE_URL),
        ("CONUS_HEIGHT_ARCHIVE_URL", CONUS_HEIGHT_ARCHIVE_URL),
    ] {
        assert!(
            url.contains(HEIGHT_ARCHIVE_GENERATION_PLACEHOLDER),
            "{name} no longer carries {HEIGHT_ARCHIVE_GENERATION_PLACEHOLDER}.\n{CHECKLIST}"
        );
    }

    // Non-triviality, and length alone is not enough. A placeholder emptied to
    // "" makes `contains` true of every string on earth; a placeholder
    // *retargeted at a real generation* makes it true of exactly the URL this
    // test exists to reject, while still being 20-odd characters long. So the
    // marker must also not be generation-SHAPED: `<12-hex>-<8-digit>` is what
    // the publish step writes, and the marker may never look like one.
    assert!(
        HEIGHT_ARCHIVE_GENERATION_PLACEHOLDER.len() > 8,
        "the placeholder marker is too short to be unmissable in a URL"
    );
    assert!(
        !is_generation_shaped(HEIGHT_ARCHIVE_GENERATION_PLACEHOLDER),
        "HEIGHT_ARCHIVE_GENERATION_PLACEHOLDER has been pointed at something \
         shaped like a real generation, which would make this whole test pass \
         over a live URL.\n{CHECKLIST}"
    );

    // And the shape test itself is not vacuous: it says yes to the real thing.
    assert!(
        is_generation_shaped("7c94bc6966ab-20260829"),
        "the shape check does not recognise a published generation, so it \
         refuses nothing"
    );
    assert!(
        !is_generation_shaped("7c94bc6966ab-2026082"),
        "the shape check accepts a short date"
    );
    assert!(
        !is_generation_shaped("7c94bc6966zz-20260829"),
        "the shape check accepts a non-hex prefix"
    );
}

/// Whether `name` is shaped like a published archive generation:
/// twelve lowercase hex digits, a hyphen, then an eight-digit date.
///
/// The shape `tools/squallar-terrain` writes and the one both live archive
/// URLs already carry (`omt-20260828` is the basemap's older two-part form;
/// this is the terrain form, `7c94bc6966ab-20260829`).
fn is_generation_shaped(name: &str) -> bool {
    let Some((hash, date)) = name.split_once('-') else {
        return false;
    };
    hash.len() == 12
        && hash.bytes().all(|b| b.is_ascii_hexdigit())
        && date.len() == 8
        && date.bytes().all(|b| b.is_ascii_digit())
}

/// Every archive URL the build reads is in the one list the block cache's live
/// set is derived from.
///
/// **The failure mode this ratchets against is silent.**
/// `gc_stale_generations` `remove_dir_all`s every directory under the shared
/// cache root whose name is not in `live_generations`, from `ensure_open`
/// inside a `get_or_init`, so an archive missing from [`live_archive_urls`]
/// loses its whole cache at the first cache open of *every* launch. Nothing
/// errors; the map is just slow forever.
///
/// A fifth archive URL that is declared and not listed reddens here. That the
/// derived config really carries all four is asserted in the child half of
/// `height_tests`, which is the one process in the suite that installs a cache
/// directory — doing it here would install a process-wide root that every
/// other test in this binary would then start writing through.
///
/// Native only, because what it ratchets is: there is no filesystem behind the
/// web target, so [`live_archive_urls`] and `archive_block_cache` are both
/// `cfg(not(wasm32))`, and a test reaching them from this ungated module is a
/// red `--all-targets` wasm row under a green lib one.
#[cfg(not(target_arch = "wasm32"))]
#[test]
fn the_live_generation_set_covers_every_archive_url_the_build_reads() {
    use crate::basemap_archive::block_cache::generation_for_url;

    // The overrides would make `live_archive_urls` answer something other than
    // the consts, and this test compares the two. Asserted rather than
    // skipped: a developer running under an override deserves to be told why
    // this reddened, not to have it quietly pass.
    for name in [
        BASEMAP_ARCHIVE_URL_ENV,
        TERRAIN_ARCHIVE_URL_ENV,
        HEIGHT_ARCHIVE_URL_ENV,
    ] {
        assert!(
            std::env::var(name).is_err(),
            "{name} is set, so live_archive_urls answers the override rather \
             than the const this test enumerates"
        );
    }

    let declared = declared_archive_url_consts();
    let urls = live_archive_urls();

    // **The enumeration, and the direction that matters.** Every archive URL
    // const `tiles.rs` declares must be in the live set. A fifth const added
    // and forgotten is exactly the silent-wipe hazard, and it is caught by
    // reading this module's own source rather than by a hand-kept list that
    // would have to be updated by the same person who forgot.
    for (name, url) in &declared {
        assert!(
            urls.iter().any(|live| live == url),
            "{name} is declared in tiles.rs and is not in live_archive_urls, \
             so its block cache is deleted at the first cache open of every \
             launch — a slow map, never an error. Add it to that list."
        );
    }

    // And the other direction: the live set names nothing that is not a
    // declared archive.
    assert_eq!(
        urls.len(),
        declared.len(),
        "live_archive_urls names {} URLs but tiles.rs declares {} archive \
         consts: {:?}",
        urls.len(),
        declared.len(),
        declared.keys().collect::<Vec<_>>()
    );

    // Non-triviality floor: a scan that found nothing, or that matched the
    // wrong shape, would make the loop above vacuous. The four known consts
    // must be in it, by name and by value.
    for (name, url) in [
        ("BASEMAP_ARCHIVE_URL", BASEMAP_ARCHIVE_URL),
        ("TERRAIN_ARCHIVE_URL", TERRAIN_ARCHIVE_URL),
        ("HEIGHT_ARCHIVE_URL", HEIGHT_ARCHIVE_URL),
        ("CONUS_HEIGHT_ARCHIVE_URL", CONUS_HEIGHT_ARCHIVE_URL),
    ] {
        assert_eq!(
            declared.get(name).map(String::as_str),
            Some(url),
            "the source scan did not read {name} out of tiles.rs, so the \
             enumeration above is not enumerating"
        );
    }
    // And it must not have swept up an `_ENV` const, which names a variable
    // rather than an archive.
    assert!(
        !declared.contains_key("HEIGHT_ARCHIVE_URL_ENV"),
        "the scan matched an override variable name as an archive URL"
    );

    // Distinct generation directories, so the archives are distinct caches
    // rather than one directory several names collide in.
    let generations: std::collections::BTreeSet<String> =
        urls.iter().map(|url| generation_for_url(url)).collect();
    assert_eq!(
        generations.len(),
        urls.len(),
        "two archive URLs derive the same generation directory: {generations:?}"
    );
}

/// **[`height_range_source`] opens the height archive and not one of the other
/// three.**
///
/// The suite around this one proves the read *chain* by spelling its body out
/// with a loopback client, which leaves the production function itself
/// unexecuted — and the two arguments it passes are the client and the URL.
/// The client is the half the loopback copy deliberately differs on; **the URL
/// is the half nothing else checks**, and it is the one whose failure is
/// silent: pointed at the hillshade, every body still decodes, every base-256
/// triple still yields a plausible metre figure, and the ground is simply
/// wrong. No fault, no log.
///
/// `archive_identity` is the whole assertion and it needs no server:
/// `HttpRangeSource::new` only parses the URL, so `https_only` never engages
/// and nothing is fetched.
#[test]
fn the_height_range_source_opens_the_height_archive() {
    use crate::basemap_archive::RangeSource as _;

    let identity = height_range_source()
        .expect("the compiled-in height archive URL parses")
        .archive_identity();

    assert_eq!(
        identity.as_deref(),
        Some(HEIGHT_ARCHIVE_URL),
        "height_range_source is pointed at the wrong archive. Reading heights \
         out of the hillshade is silent: the bodies decode, every pixel unpacks \
         to a plausible elevation, and the ground is wrong with no fault raised."
    );

    // The three siblings, so "the right one" is a distinction rather than a
    // coincidence of there being one archive.
    for (name, other) in [
        ("BASEMAP_ARCHIVE_URL", BASEMAP_ARCHIVE_URL),
        ("TERRAIN_ARCHIVE_URL", TERRAIN_ARCHIVE_URL),
        ("CONUS_HEIGHT_ARCHIVE_URL", CONUS_HEIGHT_ARCHIVE_URL),
    ] {
        assert_ne!(
            identity.as_deref(),
            Some(other),
            "height_range_source opened {name}"
        );
    }
}

/// The generation the height reader records is the generation of the archive
/// it actually opens — the two derivations meeting at the source itself rather
/// than both being spelled from the same const.
#[test]
fn the_height_generation_names_the_archive_the_source_opens() {
    use crate::basemap_archive::RangeSource as _;
    use crate::basemap_archive::block_cache::generation_for_url;

    let identity = height_range_source()
        .expect("the compiled-in height archive URL parses")
        .archive_identity()
        .expect("an HttpRangeSource promises its URL");

    assert_eq!(
        height_generation(),
        generation_for_url(&identity),
        "the block cache would file this archive's blocks under a generation \
         no reader of it looks in"
    );
}

/// The height generation is derived from the URL the reader opens, so a record
/// and the block cache cannot disagree about which archive a byte came from.
#[test]
fn the_height_generation_is_the_one_the_reader_opens() {
    use crate::basemap_archive::block_cache::generation_for_url;

    assert_eq!(
        height_generation(),
        generation_for_url(&height_archive_url()),
        "height_generation and height_archive_url have drifted apart"
    );

    // A generation is one portable path component: it is a directory name
    // under the shared cache root.
    let generation = height_generation();
    assert!(
        !generation.is_empty() && !generation.contains('/'),
        "the height generation {generation:?} is not a single path component"
    );
    assert_ne!(
        generation,
        terrain_generation(),
        "the height archive and the hillshade must not share a cache directory"
    );
}
