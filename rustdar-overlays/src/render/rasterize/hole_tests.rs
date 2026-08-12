//! Interior rings — holes — in `draw_feature`.
//!
//! The bug these pin: `draw_feature` filled `polygon.first()` and dropped
//! `polygon[1..]`, so a donut painted as a solid blob while
//! `geo_point_in_feature` (which does read the interior rings) called the
//! cut-out empty. Clicking a hole hit nothing, and nothing on screen said why.

use super::*;
use crate::types::{GeoPolygon, HatchPattern};

/// Real NWS zone geometry, in rustdar's own cache form: `(lat, lon)` pairs,
/// already RDP-simplified. See the `_source` key in the file.
const ZONE_FIXTURE: &str = include_str!("../../../testdata/nws_zone_polygons.json");

const FILL: [u8; 4] = [220, 40, 40, 255];
const STROKE: [u8; 4] = [40, 40, 220, 255];
const W: u32 = 256;
const H: u32 = 256;

fn fixture() -> serde_json::Value {
    serde_json::from_str(ZONE_FIXTURE).expect("zone fixture must parse")
}

fn polygons_at(v: &serde_json::Value) -> Vec<GeoPolygon> {
    v.as_array()
        .expect("polygons must be an array")
        .iter()
        .map(|poly| {
            poly.as_array()
                .expect("polygon must be an array of rings")
                .iter()
                .map(|ring| {
                    ring.as_array()
                        .expect("ring must be an array of points")
                        .iter()
                        .map(|pt| {
                            let pair = pt.as_array().expect("point must be a pair");
                            (
                                pair[0].as_f64().expect("lat"),
                                pair[1].as_f64().expect("lon"),
                            )
                        })
                        .collect()
                })
                .collect()
        })
        .collect()
}

/// Sussex County, Virginia: one exterior ring, one interior ring. Production
/// NWS zone geometry, not a construction.
fn donut_polygons() -> Vec<GeoPolygon> {
    polygons_at(&fixture()["donut"]["polygons"])
}

/// Nine real zones, none of which has an interior ring — the shapes this
/// change must leave untouched.
fn hole_free_zones() -> Vec<(String, Vec<GeoPolygon>)> {
    fixture()["hole_free"]
        .as_array()
        .expect("hole_free must be an array")
        .iter()
        .map(|z| {
            (
                z["id"].as_str().expect("id").to_string(),
                polygons_at(&z["polygons"]),
            )
        })
        .collect()
}

fn feature(polygons: Vec<GeoPolygon>, stroke: [u8; 4]) -> OverlayFeature {
    OverlayFeature::new(
        polygons,
        FILL,
        stroke,
        String::new(),
        String::new(),
        HatchPattern::None,
    )
}

/// A feature drawn alone, plus the projection it was drawn under, so a test
/// can ask about a geographic point rather than a pixel index.
struct Rendered {
    pixmap: Pixmap,
    mb: MercatorBounds,
}

impl Rendered {
    /// The texture spans the feature's own bounds with 5% padding, so the
    /// outline is never clipped by the texture edge.
    fn of(feature: &OverlayFeature) -> Self {
        let b = feature
            .geo_bounds
            .expect("fixture feature must have bounds");
        let pad_lat = (b.max_lat - b.min_lat) * 0.05;
        let pad_lon = (b.max_lon - b.min_lon) * 0.05;
        let bounds = GeoBounds {
            min_lat: b.min_lat - pad_lat,
            max_lat: b.max_lat + pad_lat,
            min_lon: b.min_lon - pad_lon,
            max_lon: b.max_lon + pad_lon,
        };
        let mb = MercatorBounds::from_geo(&bounds);
        let mut pixmap = Pixmap::new(W, H).expect("pixmap");
        draw_feature(&mut pixmap, feature, &mb, W as f32, H as f32);
        Self { pixmap, mb }
    }

    /// Premultiplied RGBA, as tiny-skia leaves it: `draw_feature` is called
    /// directly, without the `premultiplied_to_straight` the public entry
    /// points apply afterwards.
    fn rgba_at_geo(&self, lat: f64, lon: f64) -> [u8; 4] {
        let (px, py) = self.mb.project(lat, lon, W as f32, H as f32);
        let idx = ((py as u32) * W + px as u32) as usize * 4;
        let d = self.pixmap.data();
        [d[idx], d[idx + 1], d[idx + 2], d[idx + 3]]
    }
}

// The two probe points, chosen offline against this exact fixture for maximum
// clearance from every ring: the hole's is 11 px clear of both rings at this
// texture size, the solid ring's 47 px, so neither can be decided by
// anti-aliasing.
const IN_HOLE: (f64, f64) = (36.695138, -77.538071);
const IN_SOLID_RING: (f64, f64) = (36.617478, -77.613143);

/// The bug itself, on real data.
#[test]
fn a_real_zone_donuts_hole_is_left_unpainted() {
    let r = Rendered::of(&feature(donut_polygons(), [0, 0, 0, 0]));

    assert_eq!(
        r.rgba_at_geo(IN_HOLE.0, IN_HOLE.1)[3],
        0,
        "the interior ring was painted over: `draw_feature` is filling only \
         `polygon.first()` again, so this donut renders solid while \
         `geo_point_in_feature` still calls the hole empty"
    );
    assert_eq!(
        r.rgba_at_geo(IN_SOLID_RING.0, IN_SOLID_RING.1),
        FILL,
        "the solid ring between exterior and hole must still be filled — the \
         hole cannot be cut by dropping the polygon wholesale"
    );
}

/// Why the fill rule is even-odd and not non-zero.
///
/// Re-wound so the interior ring turns the same way as its exterior, which
/// GeoJSON's right-hand rule forbids and two rings in a real 7,015-zone cache
/// do anyway. `FillRule::Winding` renders this solid; even-odd does not care
/// which way either ring turns.
#[test]
fn a_hole_wound_like_its_exterior_is_still_cut() {
    let mut polygons = donut_polygons();
    let exterior_ccw = ring_is_ccw(&polygons[0][0]);
    if ring_is_ccw(&polygons[0][1]) != exterior_ccw {
        polygons[0][1].reverse();
    }
    assert_eq!(
        ring_is_ccw(&polygons[0][1]),
        exterior_ccw,
        "fixture setup: the hole must wind with its exterior for this test to \
         mean anything"
    );

    let r = Rendered::of(&feature(polygons, [0, 0, 0, 0]));
    assert_eq!(
        r.rgba_at_geo(IN_HOLE.0, IN_HOLE.1)[3],
        0,
        "a hole wound like its exterior was filled in — the fill rule has \
         gone back to consulting orientation, which this data does not \
         reliably carry"
    );
}

/// Shoelace sign in `(lon, lat)`, matching how the rings project.
fn ring_is_ccw(ring: &[(f64, f64)]) -> bool {
    let ring = strip_closing_dup(ring);
    let n = ring.len();
    let mut twice = 0.0;
    for i in 0..n {
        let (y1, x1) = ring[i];
        let (y2, x2) = ring[(i + 1) % n];
        twice += x1 * y2 - x2 * y1;
    }
    twice > 0.0
}

/// The hole's edge is a boundary of the feature, so the outline follows it.
#[test]
fn the_holes_rim_is_stroked() {
    let r = Rendered::of(&feature(donut_polygons(), STROKE));

    // Midpoint of the hole's longest edge, an 18 px run at this texture size.
    let rim = r.rgba_at_geo(36.688812, -77.559497);
    assert!(
        rim[3] > 0,
        "the hole's rim was left unstroked: the interior rings are in the \
         path for the fill but not for the outline"
    );
    assert!(
        rim[2] > rim[0],
        "the rim pixel is {rim:?}, which is the fill colour, not the stroke — \
         the outline is not following the hole"
    );
    assert_eq!(
        r.rgba_at_geo(IN_HOLE.0, IN_HOLE.1)[3],
        0,
        "stroking the rim must not fill the hole it encloses"
    );
}

/// An interior ring that encloses nothing must not become a scratch.
///
/// RDP simplification collapses small closed rings to retracing out-and-back
/// slivers — 2,515 of the 4,579 interior rings in a full zone cache have
/// exactly zero area, and every one of them is a three-point ring that walks
/// out and comes straight back. Even-odd already ignores them for the fill, but
/// each is still a subpath, and the stroke would draw every one of them.
/// Pinning this as byte equality, because "nearly invisible" is what a scratch
/// looks like until there are two thousand of them.
#[test]
fn a_zero_area_interior_ring_changes_no_pixel() {
    let (id, polygons) = hole_free_zones()
        .into_iter()
        .next()
        .expect("fixture must carry hole-free zones");

    let plain = Rendered::of(&feature(polygons.clone(), STROKE));

    let mut with_sliver = polygons;
    // `a → b → a`: the corpus's own shape, and the only one that is exactly
    // zero at every texture size. Three *collinear* points would not do — they
    // are collinear in lat/lon but not in Mercator, so their projected area is
    // merely small, and small scales with the square of the texture width.
    let a = with_sliver[0][0][0];
    let b = with_sliver[0][0][1];
    with_sliver[0].push(vec![a, b, a]);
    let slivered = Rendered::of(&feature(with_sliver, STROKE));

    assert_eq!(
        plain.pixmap.data(),
        slivered.pixmap.data(),
        "zone {id} rendered differently once a zero-area interior ring was \
         added — `MIN_HOLE_AREA_PX` is no longer dropping degenerate rings, \
         and every simplification sliver in the zone cache is now a stroke"
    );
}

/// A ring can clear the area floor and still be nothing but a stroke.
///
/// The area test alone leaves the slivers `MIN_HOLE_WIDTH_PX` exists for: a
/// hole a fortieth of a pixel wide removes no visible alpha, but its rim is
/// still outlined at full opacity, which is a line drawn across a zone that has
/// no hole in it. Both numbers below are `forecast_PKZ785`'s worst interior
/// ring as `hole_tests` would project it, restated as the rectangle of the same
/// area and perimeter.
#[test]
fn a_sliver_hole_is_dropped_even_though_its_area_clears_the_floor() {
    let sliver = [
        (0.0, 0.0),
        (15.545, 0.0),
        (15.545, 0.025_06),
        (0.0, 0.025_06),
    ];
    assert!(
        ring_area_px(&sliver) >= MIN_HOLE_AREA_PX,
        "fixture setup: this ring must clear the area floor, or it proves \
         nothing about the width test"
    );
    assert!(
        !hole_is_drawable(&sliver),
        "a 0.39 px² hole spread over 31 px of rim is being kept: it cuts \
         nothing anyone can see and strokes a 15 px scratch"
    );

    // The real cut-out this must not touch: Sussex County's enclave, 588.6 px²
    // over 107.7 px of rim at the size `Rendered` draws it, restated the same
    // way.
    let enclave = [
        (0.0, 0.0),
        (38.62, 0.0),
        (38.62, 15.24),
        (0.0, 15.24),
    ];
    assert!(
        hole_is_drawable(&enclave),
        "a real enclave was dropped as a sliver — the width test is too tight \
         and holes the map used to show are being filled back in"
    );
}

/// FNV-1a, spelled out so the pinned digests below cannot drift with a
/// standard-library hasher's implementation.
fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        hash ^= b as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// The regression guard: hole-free polygons must rasterize to the *same bytes*
/// they did before holes were honoured.
///
/// These digests were taken from the rasterizer as it stood before this change
/// (`cb6f2246`), running this very fixture, and they have not moved since.
/// That is what makes them evidence rather than a restatement: hole handling
/// only ever adds subpaths, and a polygon with no interior rings gets the same
/// single-ring path filled under the same `FillRule::Winding` as always.
#[test]
fn hole_free_zones_rasterize_to_the_same_bytes_as_before_holes_were_honoured() {
    // (zone id, FNV-1a of the premultiplied RGBA buffer).
    const PINNED: &[(&str, u64)] = &[
        ("county_TXC113", 0x1037_4a55_89b3_9cb6),
        ("county_OKC109", 0x3d99_3381_bcd6_3e04),
        ("county_KSC173", 0xc6ec_c3df_abd2_b90d),
        ("forecast_ILZ023", 0x3166_e1dc_96d7_abbc),
        ("county_COC069", 0xf6b6_c98f_d8d6_9857),
        ("forecast_NYZ080", 0xfb72_8465_8cfe_79ae),
        ("county_MTC031", 0x5e7c_c8bf_37e9_c866),
        ("forecast_TXZ120", 0x52a4_57f5_2767_75a0),
        ("county_NEC055", 0x18a8_d871_b5ec_734e),
    ];

    let zones = hole_free_zones();
    assert_eq!(
        zones.len(),
        PINNED.len(),
        "the fixture gained or lost a zone; re-pin against the pre-change \
         rasterizer, do not re-pin against this one"
    );

    for ((id, polygons), (pinned_id, pinned)) in zones.iter().zip(PINNED) {
        assert_eq!(id, pinned_id, "fixture order changed");
        for ring in polygons.iter().flatten() {
            assert!(!ring.is_empty(), "{id} has an empty ring");
        }
        assert!(
            polygons.iter().all(|p| p.len() == 1),
            "{id} grew an interior ring; it is no longer a hole-free control"
        );
        let r = Rendered::of(&feature(polygons.clone(), STROKE));
        assert_eq!(
            fnv1a(r.pixmap.data()),
            *pinned,
            "zone {id} no longer rasterizes to the bytes it did before holes \
             were honoured"
        );
    }
}
