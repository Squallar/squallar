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

/// The screen rect one tile is painted into — the same two corners
/// `draw_tile_layer` hands `geo_corner_rect`.
fn tile_rect(projector: &walkers::Projector, x: u32, y: u32, zoom: u8) -> egui::Rect {
    crate::overlay_cache::geo_corner_rect(
        projector,
        (tile_to_lat(y, zoom), tile_to_lon(x, zoom)),
        (tile_to_lat(y + 1, zoom), tile_to_lon(x + 1, zoom)),
    )
}

/// The screen rect the whole span covers — the grid is regular, so the union
/// of its tiles is a rect.
fn span_rect(projector: &walkers::Projector, span: TileSpan, zoom: u8) -> egui::Rect {
    tile_rect(projector, span.west, span.north, zoom)
        .union(tile_rect(projector, span.east, span.south, zoom))
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
                let rect = tile_rect(&projector, x, y, tile_zoom);
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

/// What the loop asks for and what the cache is sized to hold are the same
/// number — at a whole zoom, which is the only claim
/// [`crate::tile_source::TILE_CACHE_ENTRIES`]'s docs make.
#[test]
fn the_span_and_the_cache_sizing_agree_at_a_whole_zoom() {
    let budget = tiles_resident_for(canvas(), 0, 1);

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

    // A record of a gap `tiles_resident_for` has, not a gate on the span:
    // it measures a tile as TILE_SIDE_POINTS, but between two whole zooms a
    // tile is drawn `256 * 2^(zoom - round(zoom))` points across — 181 at the
    // half step — so more of them fit the same window and the figure
    // understates. The wasm arm is sized against the whole-zoom number only.
    assert_eq!(
        between.0, BETWEEN_ZOOMS_WORST,
        "the between-zooms worst case is a measured figure and moved; the worst \
         case this sweep found is at {}",
        between.1
    );
    assert!(
        between.0 > crate::tile_source::WASM_TILE_CACHE_ENTRIES.get(),
        "between-zooms worst case is {} tiles at {}, which no longer overruns \
         the {} the wasm arm allows",
        between.0,
        between.1,
        crate::tile_source::WASM_TILE_CACHE_ENTRIES.get()
    );
}
