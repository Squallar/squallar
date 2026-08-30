use std::collections::BTreeSet;

use super::*;
use crate::tile::{BoxFrame, TileId};

/// One real `building` layer out of the archive this workspace ships.
///
/// z14/8529/5974 of `squallar-egui/testdata/monaco.pmtiles`, the committed
/// planetiler v0.10.2 OpenMapTiles build, with the other fourteen source
/// layers dropped and the `building` layer re-wrapped as a whole tile:
/// 20,742 bytes against the 185,182 the tile carries entire. Chosen out of the
/// five z14 tiles that carry buildings because it is **the only one whose
/// `render_min_height` is not zero on every feature** — its values are
/// 0, 3, 18, 21 and 43, so a reader that ignored the key altogether would
/// still fail here.
///
/// **Do not re-encode it.** It is upstream's bytes, and what makes it worth
/// committing is that nothing in this repository chose them.
pub(crate) const REAL_BUILDING_TILE: &[u8] =
    include_bytes!("../../testdata/monaco-building-z14-8529-5974.mvt");

/// The address [`REAL_BUILDING_TILE`] came from.
pub(crate) const REAL_TILE_ID: TileId = TileId {
    z: 14,
    x: 8529,
    y: 5974,
};

/// A box centred on [`REAL_TILE_ID`]'s own centre, wide enough to hold the
/// whole tile (a z14 tile is 1.77 km across at this latitude).
///
/// **The two extents differ deliberately**, on the reasoning
/// `squallar_worker`'s height fixture records: a square box makes a
/// transposition of the two axes invisible, in the projection here as much as
/// in a wire layout.
pub(crate) fn a_frame() -> BoxFrame {
    BoxFrame {
        site: (43.731_414_013_768_99, 7.415_771_484_375),
        x_km: (-3.0, 3.0),
        y_km: (-2.0, 4.0),
    }
}

// ── A vector tile, encoded by hand ──────────────────────────────────────────
//
// `mvt-reader` only decodes and its protobuf module is private, so a tile with
// a chosen property on a chosen ring has to be written in the wire format.
// Field numbers are the vector-tile spec's:
//
//   Tile    { layers = 3 }
//   Layer   { name = 1, features = 2, keys = 3, values = 4, extent = 5,
//             version = 15 }
//   Feature { id = 1, tags = 2, type = 3, geometry = 4 }
//   Value   { string = 1, float = 2, double = 3, int = 4, uint = 5,
//             sint = 6, bool = 7 }
//
// Shaped after `vendor/walkers`' own test encoder, which exists for the same
// reason. It is re-spelled rather than reached for because that one is behind
// `#[cfg(test)]` in a crate this one may not link.
pub(crate) mod fixture {
    /// The extent every fixture layer declares unless it says otherwise, and
    /// the one every real OpenMapTiles build uses.
    pub(crate) const EXTENT: u32 = 4096;

    #[derive(Clone)]
    pub(crate) enum Prop {
        Str(&'static str),
        Float(f32),
        Double(f64),
        Int(i64),
        UInt(u64),
        SInt(i64),
        Bool(bool),
    }

    pub(crate) struct FeatureSpec {
        pub(crate) geom_type: u32,
        pub(crate) properties: Vec<(&'static str, Prop)>,
        pub(crate) geometry: Vec<u32>,
    }

    pub(crate) const GEOM_POINT: u32 = 1;
    pub(crate) const GEOM_LINESTRING: u32 = 2;
    pub(crate) const GEOM_POLYGON: u32 = 3;

    fn varint(mut value: u64, out: &mut Vec<u8>) {
        loop {
            let byte = (value & 0x7f) as u8;
            value >>= 7;
            if value == 0 {
                out.push(byte);
                break;
            }
            out.push(byte | 0x80);
        }
    }

    fn tag(field: u32, wire_type: u32, out: &mut Vec<u8>) {
        varint(u64::from((field << 3) | wire_type), out);
    }

    fn varint_field(field: u32, value: u64, out: &mut Vec<u8>) {
        tag(field, 0, out);
        varint(value, out);
    }

    fn bytes_field(field: u32, payload: &[u8], out: &mut Vec<u8>) {
        tag(field, 2, out);
        varint(payload.len() as u64, out);
        out.extend_from_slice(payload);
    }

    fn packed_u32_field(field: u32, values: &[u32], out: &mut Vec<u8>) {
        let mut payload = Vec::new();
        for value in values {
            varint(u64::from(*value), &mut payload);
        }
        bytes_field(field, &payload, out);
    }

    fn encode_value(prop: &Prop) -> Vec<u8> {
        let mut out = Vec::new();
        match prop {
            Prop::Str(s) => bytes_field(1, s.as_bytes(), &mut out),
            Prop::Float(f) => {
                tag(2, 5, &mut out);
                out.extend_from_slice(&f.to_le_bytes());
            }
            Prop::Double(d) => {
                tag(3, 1, &mut out);
                out.extend_from_slice(&d.to_le_bytes());
            }
            Prop::Int(i) => varint_field(4, *i as u64, &mut out),
            Prop::UInt(u) => varint_field(5, *u, &mut out),
            Prop::SInt(s) => varint_field(6, ((s << 1) ^ (s >> 63)) as u64, &mut out),
            Prop::Bool(b) => varint_field(7, u64::from(*b), &mut out),
        }
        out
    }

    /// `CommandInteger`: the command id in the low three bits, the repeat
    /// count above them.
    fn command(id: u32, count: u32) -> u32 {
        (id & 0x7) | (count << 3)
    }

    /// `ParameterInteger`: zig-zag encoded, and relative to the cursor.
    fn param(value: i32) -> u32 {
        ((value << 1) ^ (value >> 31)) as u32
    }

    /// One or more rings as a polygon geometry, with the cursor carried from
    /// each ring's last vertex into the next ring's `MoveTo`.
    ///
    /// The ring points are given **open**; the spec's `ClosePath` is what
    /// repeats the first vertex.
    pub(crate) fn polygon(rings: &[&[(i32, i32)]]) -> Vec<u32> {
        let mut out = Vec::new();
        let mut cursor = (0, 0);
        for ring in rings {
            assert!(ring.len() >= 3, "a ring needs three vertices");
            out.push(command(1, 1));
            out.push(param(ring[0].0 - cursor.0));
            out.push(param(ring[0].1 - cursor.1));
            cursor = ring[0];
            out.push(command(2, (ring.len() - 1) as u32));
            for point in &ring[1..] {
                out.push(param(point.0 - cursor.0));
                out.push(param(point.1 - cursor.1));
                cursor = *point;
            }
            out.push(command(7, 1));
        }
        out
    }

    /// A single point geometry, for the "not a polygon" arm.
    pub(crate) fn point(at: (i32, i32)) -> Vec<u32> {
        vec![command(1, 1), param(at.0), param(at.1)]
    }

    fn encode_layer(name: &str, extent: u32, features: &[FeatureSpec]) -> Vec<u8> {
        let mut keys: Vec<&str> = Vec::new();
        let mut values: Vec<Vec<u8>> = Vec::new();
        let mut blobs: Vec<Vec<u8>> = Vec::new();

        for (index, feature) in features.iter().enumerate() {
            let mut tags = Vec::new();
            for (key, value) in &feature.properties {
                let key_index = match keys.iter().position(|k| k == key) {
                    Some(index) => index,
                    None => {
                        keys.push(key);
                        keys.len() - 1
                    }
                };
                let encoded = encode_value(value);
                let value_index = match values.iter().position(|v| *v == encoded) {
                    Some(index) => index,
                    None => {
                        values.push(encoded);
                        values.len() - 1
                    }
                };
                tags.push(key_index as u32);
                tags.push(value_index as u32);
            }

            let mut blob = Vec::new();
            varint_field(1, index as u64 + 1, &mut blob);
            packed_u32_field(2, &tags, &mut blob);
            varint_field(3, u64::from(feature.geom_type), &mut blob);
            packed_u32_field(4, &feature.geometry, &mut blob);
            blobs.push(blob);
        }

        let mut layer = Vec::new();
        bytes_field(1, name.as_bytes(), &mut layer);
        for blob in &blobs {
            bytes_field(2, blob, &mut layer);
        }
        for key in &keys {
            bytes_field(3, key.as_bytes(), &mut layer);
        }
        for value in &values {
            bytes_field(4, value, &mut layer);
        }
        varint_field(5, u64::from(extent), &mut layer);
        varint_field(15, 2, &mut layer);
        layer
    }

    /// A whole tile out of one or more `(name, extent, features)` layers.
    pub(crate) fn tile(layers: &[(&str, u32, Vec<FeatureSpec>)]) -> Vec<u8> {
        let mut out = Vec::new();
        for (name, extent, features) in layers {
            let layer = encode_layer(name, *extent, features);
            bytes_field(3, &layer, &mut out);
        }
        out
    }
}

use fixture::{EXTENT, FeatureSpec, GEOM_LINESTRING, GEOM_POINT, GEOM_POLYGON, Prop};

/// A square whose winding `mvt-reader` reads as an **exterior** ring.
///
/// Extent units run southward, so the screen-clockwise order below is the one
/// whose shoelace sum comes out positive under the reader's own rule — and it
/// is what becomes counter-clockwise once [`TileId::point_geo`] flips the
/// axis.
fn exterior_square(origin: (i32, i32), side: i32) -> Vec<(i32, i32)> {
    let (x, y) = origin;
    vec![(x, y), (x + side, y), (x + side, y + side), (x, y + side)]
}

/// The same square wound the other way, which is how a hole is spelled.
fn interior_square(origin: (i32, i32), side: i32) -> Vec<(i32, i32)> {
    let mut points = exterior_square(origin, side);
    points.reverse();
    points
}

fn building(properties: Vec<(&'static str, Prop)>, geometry: Vec<u32>) -> FeatureSpec {
    FeatureSpec {
        geom_type: GEOM_POLYGON,
        properties,
        geometry,
    }
}

/// A tile of one `building` layer at the standard extent.
fn buildings_tile(features: Vec<FeatureSpec>) -> Vec<u8> {
    fixture::tile(&[(SOURCE_LAYER, EXTENT, features)])
}

/// A plain building at the tile's middle: 100 extent units on a side, which is
/// about 43 m at this zoom and latitude.
fn a_plain_building(height: i64) -> FeatureSpec {
    building(
        vec![(RENDER_HEIGHT, Prop::Int(height))],
        fixture::polygon(&[&exterior_square((2000, 2000), 100)]),
    )
}

// ── The real archive ────────────────────────────────────────────────────────

/// **The property names, confirmed against a real tile.**
///
/// The whole of this unit's data contract is three key names, and the plan
/// carried them as unverified — an earlier claim that they had been checked
/// was not true of this repository. This is the check: every property key on
/// every feature of a real OpenMapTiles `building` layer, as a set.
#[test]
fn the_shipped_archives_building_layer_carries_exactly_three_property_names() {
    let reader = mvt_reader::Reader::new(REAL_BUILDING_TILE.to_vec()).expect("the fixture decodes");
    let layers = reader.get_layer_metadata().expect("it has layer metadata");
    let layer = layers
        .iter()
        .find(|layer| layer.name == SOURCE_LAYER)
        .expect("the fixture is a building layer");
    let features = reader
        .get_features(layer.layer_index)
        .expect("its features decode");

    let keys: BTreeSet<String> = features
        .iter()
        .flat_map(|feature| {
            feature
                .properties
                .iter()
                .flat_map(|bag| bag.keys().cloned())
        })
        .collect();
    assert_eq!(
        keys,
        ["colour", RENDER_HEIGHT, RENDER_MIN_HEIGHT]
            .into_iter()
            .map(str::to_owned)
            .collect::<BTreeSet<String>>(),
        "the real archive's building layer carries a different property set \
         than this crate reads",
    );
    assert!(
        !keys.contains(HIDE_3D),
        "the archive DOES carry `hide_3d` after all, and this crate's note \
         that no shipped build exercises that key is now false",
    );

    // The falsifiability half: a fixture with one feature, or with one value
    // repeated, would satisfy the set above while proving nothing about a
    // reader that ignores the keys.
    assert_eq!(
        features.len(),
        43,
        "the fixture tile is not the one recorded"
    );
    let heights: BTreeSet<u64> = features
        .iter()
        .filter_map(|f| number(f.properties.as_ref()?, RENDER_HEIGHT).map(f64::to_bits))
        .collect();
    assert!(
        heights.len() > 10,
        "the fixture carries {} distinct heights, too few for the \
         height-ordered shed to be meaningful over it",
        heights.len(),
    );

    // **The arm the real archive actually uses**, which is not the obvious
    // one: planetiler writes these as MVT `sint_value`, the zig-zag varint,
    // and not as `int_value`. A reader that matched only `Value::Int` would
    // find no heights at all here and extrude nothing, which is the failure
    // this line exists to catch -- an earlier draft of this very test did
    // exactly that.
    assert!(
        features.iter().any(|f| matches!(
            f.properties.as_ref().and_then(|bag| bag.get(RENDER_HEIGHT)),
            Some(mvt_reader::feature::Value::SInt(_))
        )),
        "the archive no longer writes `render_height` in MVT's sint arm; the \
         note beside `number` describing which arms are live is now stale",
    );
}

/// **The identity-fixture guard for the base-height lane.**
///
/// `render_min_height` is zero on 119 of the 126 building features in the
/// archive, so a fixture picked at random sits on the degenerate value and a
/// reader that returned zero unconditionally would pass everything. This
/// asserts the committed tile is one of the ones that does not.
#[test]
fn the_real_tile_carries_buildings_that_do_not_start_on_the_ground() {
    let footprints =
        read_footprints(REAL_TILE_ID, REAL_BUILDING_TILE, &a_frame()).expect("the tile reads");
    assert!(
        footprints.len() > 20,
        "only {} footprints came out of a 43-feature tile",
        footprints.len(),
    );
    let bases: BTreeSet<u64> = footprints.iter().map(|f| f.base_m.to_bits()).collect();
    let raised = footprints.iter().filter(|f| f.base_m > 0.0).count();
    assert!(
        bases.len() >= 4 && raised >= 3,
        "the fixture has {} distinct base heights and {raised} raised \
         buildings; a reader hard-wired to zero would pass over it",
        bases.len(),
    );
    assert!(
        footprints.iter().all(|f| f.height_m > f.base_m),
        "a footprint came through with its top at or below its base",
    );
}

/// Every footprint the real tile yields lands inside the box it was read
/// against, and inside the tile it came from.
///
/// The projection has four independent ways to be wrong — the mercator
/// inverse, the axis flip, the extent divisor and the bearing pair — and three
/// of them put a building somewhere plausible rather than somewhere absurd.
/// A tile is 1.77 km across here, so anything outside that is one of the four.
#[test]
fn the_real_tiles_footprints_land_inside_the_tile_they_came_from() {
    let footprints =
        read_footprints(REAL_TILE_ID, REAL_BUILDING_TILE, &a_frame()).expect("the tile reads");
    let mut bbox = [f64::MAX, f64::MAX, f64::MIN, f64::MIN];
    for footprint in &footprints {
        bbox[0] = bbox[0].min(footprint.bbox[0]);
        bbox[1] = bbox[1].min(footprint.bbox[1]);
        bbox[2] = bbox[2].max(footprint.bbox[2]);
        bbox[3] = bbox[3].max(footprint.bbox[3]);
    }
    let (width, height) = (bbox[2] - bbox[0], bbox[3] - bbox[1]);
    assert!(
        width > 0.3 && width < 1.9 && height > 0.3 && height < 1.9,
        "the tile's buildings span {width:.3} km by {height:.3} km; a z14 \
         tile at this latitude is 1.77 km across, so the projection put them \
         somewhere else",
    );
    // And the tile's centre really is near the box origin, which is where the
    // frame was anchored -- a swapped bearing pair would put it on the wrong
    // axis while keeping the span right.
    let centre = [(bbox[0] + bbox[2]) / 2.0, (bbox[1] + bbox[3]) / 2.0];
    assert!(
        centre[0].abs() < 0.9 && centre[1].abs() < 0.9,
        "the tile's buildings centre on {centre:?} km, not on the box origin \
         its own centre was made the site of",
    );
}

// ── The keys, one at a time ─────────────────────────────────────────────────

/// [`HIDE_3D`] is read even though no archive this workspace ships carries it,
/// so the synthetic tile below is the only place the key is exercised — and
/// saying that out loud is the point of the test's name.
#[test]
fn the_hide_3d_key_is_honoured_though_no_shipped_archive_carries_it() {
    let mvt = buildings_tile(vec![
        a_plain_building(30),
        building(
            vec![(RENDER_HEIGHT, Prop::Int(40)), (HIDE_3D, Prop::Bool(true))],
            fixture::polygon(&[&exterior_square((2200, 2000), 100)]),
        ),
        building(
            vec![(RENDER_HEIGHT, Prop::Int(50)), (HIDE_3D, Prop::Bool(false))],
            fixture::polygon(&[&exterior_square((2400, 2000), 100)]),
        ),
        building(
            vec![(RENDER_HEIGHT, Prop::Int(60)), (HIDE_3D, Prop::UInt(1))],
            fixture::polygon(&[&exterior_square((2600, 2000), 100)]),
        ),
    ]);
    let found = read_footprints(REAL_TILE_ID, &mvt, &a_frame()).expect("the tile reads");
    let heights: Vec<f64> = found.iter().map(|f| f.height_m).collect();
    assert_eq!(
        heights,
        vec![30.0, 50.0],
        "the hidden buildings are the 40 m one (`hide_3d` true) and the 60 m \
         one (`hide_3d` 1); `hide_3d` false must NOT hide, which is the half \
         a reader that treats the key's presence as the signal gets wrong",
    );
}

/// A building whose height nobody knows is left flat on the basemap rather
/// than given an invented one.
#[test]
fn a_feature_with_no_render_height_is_not_extruded() {
    let mvt = buildings_tile(vec![
        a_plain_building(30),
        building(
            vec![(RENDER_MIN_HEIGHT, Prop::Int(3))],
            fixture::polygon(&[&exterior_square((2200, 2000), 100)]),
        ),
    ]);
    let found = read_footprints(REAL_TILE_ID, &mvt, &a_frame()).expect("the tile reads");
    assert_eq!(
        found.len(),
        1,
        "the height-less feature was extruded anyway"
    );
    assert_eq!(found[0].height_m, 30.0);
}

/// A prism with no walls is not a prism. The real archive carries one such
/// feature, at `render_height = 0`.
#[test]
fn a_feature_whose_top_is_not_above_its_base_is_not_extruded() {
    let mvt = buildings_tile(vec![
        a_plain_building(0),
        building(
            vec![
                (RENDER_HEIGHT, Prop::Int(5)),
                (RENDER_MIN_HEIGHT, Prop::Int(5)),
            ],
            fixture::polygon(&[&exterior_square((2200, 2000), 100)]),
        ),
        building(
            vec![
                (RENDER_HEIGHT, Prop::Int(5)),
                (RENDER_MIN_HEIGHT, Prop::Int(9)),
            ],
            fixture::polygon(&[&exterior_square((2400, 2000), 100)]),
        ),
        a_plain_building(7),
    ]);
    let found = read_footprints(REAL_TILE_ID, &mvt, &a_frame()).expect("the tile reads");
    assert_eq!(
        found.iter().map(|f| f.height_m).collect::<Vec<_>>(),
        vec![7.0],
        "a zero-height, a flat and an inverted feature all have to go, and \
         the 7 m one has to stay -- without it this test would pass on a \
         reader that dropped everything",
    );
}

/// Every numeric arm MVT can spell, since a producer picks whichever it likes.
#[test]
fn a_height_is_read_out_of_whichever_numeric_arm_it_arrived_in() {
    let arms: [(&str, Prop, f64); 5] = [
        ("int", Prop::Int(30), 30.0),
        ("uint", Prop::UInt(31), 31.0),
        ("sint", Prop::SInt(32), 32.0),
        ("float", Prop::Float(33.5), 33.5),
        ("double", Prop::Double(34.25), 34.25),
    ];
    for (name, prop, expected) in arms {
        let mvt = buildings_tile(vec![building(
            vec![(RENDER_HEIGHT, prop)],
            fixture::polygon(&[&exterior_square((2000, 2000), 100)]),
        )]);
        let found = read_footprints(REAL_TILE_ID, &mvt, &a_frame()).expect("the tile reads");
        assert_eq!(
            found.iter().map(|f| f.height_m).collect::<Vec<_>>(),
            vec![expected],
            "the {name} arm did not read as {expected}",
        );
    }
    // The falsifiability half: a string height is not a height, and reading
    // one would be a producer's typo becoming a tower.
    let mvt = buildings_tile(vec![building(
        vec![(RENDER_HEIGHT, Prop::Str("30"))],
        fixture::polygon(&[&exterior_square((2000, 2000), 100)]),
    )]);
    assert!(
        read_footprints(REAL_TILE_ID, &mvt, &a_frame())
            .expect("the tile reads")
            .is_empty(),
        "a string `render_height` was read as a number",
    );
}

// ── Geometry ────────────────────────────────────────────────────────────────

/// The exterior comes back counter-clockwise and the hole clockwise, whatever
/// the tile wound them, and neither carries the repeated closing vertex.
#[test]
fn rings_arrive_open_and_wound_canonically() {
    let mvt = buildings_tile(vec![building(
        vec![(RENDER_HEIGHT, Prop::Int(30))],
        fixture::polygon(&[
            &exterior_square((2000, 2000), 400),
            &interior_square((2100, 2100), 100),
        ]),
    )]);
    let found = read_footprints(REAL_TILE_ID, &mvt, &a_frame()).expect("the tile reads");
    assert_eq!(found.len(), 1);
    let rings = &found[0].rings;
    assert_eq!(rings.len(), 2, "the hole did not come through as a ring");
    assert!(rings[0].exterior && !rings[1].exterior);
    for ring in rings {
        assert_eq!(
            ring.points.len(),
            4,
            "a four-vertex square came back with {} vertices, so the closing \
             repeat was kept",
            ring.points.len(),
        );
        assert_ne!(
            ring.points.first(),
            ring.points.last(),
            "the ring is closed, and `Ring` promises open",
        );
    }
    let areas: Vec<f64> = rings
        .iter()
        .map(|ring| Ring::double_signed_area(&ring.points))
        .collect();
    assert!(
        areas[0] > 0.0 && areas[1] < 0.0,
        "the rings are wound {areas:?}; an exterior must be positive and a \
         hole negative or the non-zero fill rule fills the courtyard in",
    );
}

/// The same square at twice the extent and twice the coordinates is the same
/// building.
///
/// **Not a formality.** `vendor/walkers` refuses any extent but 4096 outright,
/// which is a defensible thing for a renderer whose transform bakes the number
/// in; this reader divides by whatever the layer declared, and this is what
/// says so. A reader with 4096 hard-wired puts every building in an
/// 8192-extent tile at half scale in the tile's north-west quarter.
#[test]
fn the_layers_declared_extent_is_what_scales_the_geometry() {
    let at_4096 = fixture::tile(&[(SOURCE_LAYER, 4096, vec![a_plain_building(30)])]);
    let at_8192 = fixture::tile(&[(
        SOURCE_LAYER,
        8192,
        vec![building(
            vec![(RENDER_HEIGHT, Prop::Int(30))],
            fixture::polygon(&[&exterior_square((4000, 4000), 200)]),
        )],
    )]);
    let a = read_footprints(REAL_TILE_ID, &at_4096, &a_frame()).expect("reads");
    let b = read_footprints(REAL_TILE_ID, &at_8192, &a_frame()).expect("reads");
    assert_eq!(a.len(), 1);
    assert_eq!(b.len(), 1);
    for (left, right) in a[0].rings[0].points.iter().zip(&b[0].rings[0].points) {
        assert!(
            (left[0] - right[0]).abs() < 1e-9 && (left[1] - right[1]).abs() < 1e-9,
            "the same ground square read as {left:?} at extent 4096 and \
             {right:?} at extent 8192",
        );
    }
    // The falsifiability half: the two really are at a scale where getting it
    // wrong would show. Half of this square is 21 m, not a rounding.
    let side = a[0].bbox[2] - a[0].bbox[0];
    assert!(
        (0.03..0.06).contains(&side),
        "the fixture square is {side:.4} km on a side, too small or too large \
         for a doubled extent to be distinguishable",
    );
}

/// A multi-polygon feature is one building, because the budget sheds whole
/// buildings.
#[test]
fn a_multipolygon_feature_stays_one_footprint() {
    let mvt = buildings_tile(vec![building(
        vec![(RENDER_HEIGHT, Prop::Int(30))],
        fixture::polygon(&[
            &exterior_square((2000, 2000), 100),
            &exterior_square((2400, 2000), 100),
        ]),
    )]);
    let found = read_footprints(REAL_TILE_ID, &mvt, &a_frame()).expect("the tile reads");
    assert_eq!(
        found.len(),
        1,
        "a two-part building became two buildings, so the shed can now drop \
         half of one",
    );
    assert_eq!(found[0].rings.len(), 2);
    assert!(
        found[0].rings.iter().all(|ring| ring.exterior),
        "the second part came through as a hole in the first",
    );
}

/// Points and lines have no footprint to extrude.
#[test]
fn a_non_polygon_feature_contributes_nothing() {
    let mvt = buildings_tile(vec![
        FeatureSpec {
            geom_type: GEOM_POINT,
            properties: vec![(RENDER_HEIGHT, Prop::Int(30))],
            geometry: fixture::point((2000, 2000)),
        },
        FeatureSpec {
            geom_type: GEOM_LINESTRING,
            properties: vec![(RENDER_HEIGHT, Prop::Int(40))],
            geometry: fixture::polygon(&[&exterior_square((2200, 2000), 100)]),
        },
        a_plain_building(50),
    ]);
    let found = read_footprints(REAL_TILE_ID, &mvt, &a_frame()).expect("the tile reads");
    assert_eq!(
        found.iter().map(|f| f.height_m).collect::<Vec<_>>(),
        vec![50.0],
        "only the polygon is a building, and the polygon has to survive or \
         this test passes on a reader that drops everything",
    );
}

// ── The box ─────────────────────────────────────────────────────────────────

/// A tile overhangs the drawn footprint, and this is where the overhang stops
/// costing vertex budget — but a building on the seam is kept whole.
#[test]
fn a_footprint_outside_the_box_is_culled_and_one_on_the_edge_is_not() {
    let mvt = buildings_tile(vec![a_plain_building(30)]);
    let inside = read_footprints(REAL_TILE_ID, &mvt, &a_frame()).expect("reads");
    assert_eq!(
        inside.len(),
        1,
        "the control building is not inside the box"
    );

    let elsewhere = BoxFrame {
        site: (39.0, -106.0),
        x_km: (-3.0, 3.0),
        y_km: (-2.0, 4.0),
    };
    assert!(
        read_footprints(REAL_TILE_ID, &mvt, &elsewhere)
            .expect("reads")
            .is_empty(),
        "a Monaco building was kept for a Colorado box",
    );

    // The seam: a box whose east edge cuts the building in half.
    let bbox = inside[0].bbox;
    let straddling = BoxFrame {
        site: a_frame().site,
        x_km: (bbox[0] - 1.0, (bbox[0] + bbox[2]) / 2.0),
        y_km: a_frame().y_km,
    };
    assert_eq!(
        read_footprints(REAL_TILE_ID, &mvt, &straddling)
            .expect("reads")
            .len(),
        1,
        "a building straddling the edge was culled; the cull is deliberately \
         the permissive arm",
    );
}

// ── Refusals ────────────────────────────────────────────────────────────────

#[test]
fn a_tile_with_no_building_layer_says_which_it_is() {
    let mvt = fixture::tile(&[("water", EXTENT, vec![a_plain_building(30)])]);
    assert_eq!(
        read_footprints(REAL_TILE_ID, &mvt, &a_frame()),
        Err(BuildingsError::NoBuildingLayer),
    );
}

#[test]
fn a_layer_at_extent_zero_is_refused_rather_than_divided_by() {
    let mvt = fixture::tile(&[(SOURCE_LAYER, 0, vec![a_plain_building(30)])]);
    assert_eq!(
        read_footprints(REAL_TILE_ID, &mvt, &a_frame()),
        Err(BuildingsError::ZeroExtent),
    );
}

#[test]
fn an_address_off_its_own_zooms_grid_is_refused() {
    let mvt = buildings_tile(vec![a_plain_building(30)]);
    let off_grid = TileId { z: 2, x: 4, y: 0 };
    assert_eq!(
        read_footprints(off_grid, &mvt, &a_frame()),
        Err(BuildingsError::NotAddressable(off_grid)),
    );
    // The control: the same zoom with a column that does exist reads.
    assert!(read_footprints(TileId { z: 2, x: 3, y: 0 }, &mvt, &a_frame()).is_ok());
}

#[test]
fn bytes_that_are_not_a_tile_are_refused_with_the_decoders_own_words() {
    let err = read_footprints(REAL_TILE_ID, b"not a vector tile at all", &a_frame())
        .expect_err("garbage does not decode");
    assert!(
        matches!(err, BuildingsError::Parse(_)),
        "garbage decoded as {err:?}",
    );
    assert!(
        !err.to_string().is_empty(),
        "the refusal says nothing, which is the confusion this enum exists to \
         avoid",
    );
}
