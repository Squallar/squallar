//! The `building` source layer, read straight out of MVT bytes.
//!
//! # Why this is a direct parse and not a reach into the tile cache
//!
//! The obvious cheaper route is to take the footprints out of the
//! [`ParsedTile`] the map already holds, since the bytes have been decoded
//! once already. **Neither half of that is available.**
//!
//! * `vendor/walkers`' `ParsedTile::layers` is a private field and
//!   `ParsedLayer` / `ParsedFeature` are private structs. The public surface is
//!   `parse`, `styled`, `render` and `heap_bytes` — there is no accessor for a
//!   feature's geometry or its property bag, so the extraction cannot be
//!   written through today's API. Adding one means editing a vendored crate
//!   whose inline expression tests are a deliberate behaviour pin, to serve a
//!   caller that could not link it anyway: walkers is an egui widget, and this
//!   crate's charter forbids egui in its resolved graph.
//! * The parsed-tile cache is page-side (`squallar_egui::tile_source`'s
//!   `SharedParsedTiles`), and this code runs on a worker with no view of the
//!   page's heap at all. On the browser arm it is a different address space.
//!
//! So the MVT **bytes** ride to the worker and are parsed again there, which
//! is the second payload class [`crate::jobs`] costs rather than assumes. What
//! that buys is that the re-parse is on the worker with the tessellation,
//! rather than the tessellation being on the frame thread with the cache.
//!
//! # The property names, and how they were confirmed
//!
//! Read out of `squallar-egui/testdata/monaco.pmtiles`, the committed
//! planetiler v0.10.2 OpenMapTiles build, two ways that agree. Its metadata
//! declares the `building` layer's fields as exactly `colour` (String),
//! `render_height` (Number) and `render_min_height` (Number); walking every
//! feature at every zoom finds the same three and nothing else — 126 features
//! at z14 carrying `render_height` and `render_min_height`, 22 of them also
//! carrying `colour`, and four at z13 carrying no properties at all.
//!
//! **[`HIDE_3D`] is not in that archive**, neither declared in the layer's
//! field list nor present on any feature. It is read here because it is in the
//! OpenMapTiles schema this tree's styles are written against and a build that
//! carries it must not extrude the parts it marks; but nothing this workspace
//! ships exercises it, so its pin
//! `the_hide_3d_key_is_honoured_though_no_shipped_archive_carries_it`
//! is over a synthetic tile rather than a real one, and says so.
//!
//! [`ParsedTile`]: https://docs.rs/walkers

use std::collections::HashMap;

use geo_types::{Geometry, LineString, Polygon};
use mvt_reader::Reader;
use mvt_reader::feature::Value;

use crate::tile::{BoxFrame, TileId};

/// The source layer 3D buildings are read from.
pub const SOURCE_LAYER: &str = "building";

/// Metres from the ground to the top of the building.
pub const RENDER_HEIGHT: &str = "render_height";

/// Metres from the ground to the bottom of the building, for the parts that do
/// not start on it. Absent means zero, which is what all but seven of the
/// confirmation archive's features carry.
pub const RENDER_MIN_HEIGHT: &str = "render_min_height";

/// Set on the parts an OpenMapTiles build wants left out of a 3D view.
pub const HIDE_3D: &str = "hide_3d";

/// The most extent-unit vertices one tile's `building` layer may contribute.
///
/// **A refusal ceiling and not a measured budget.** These bytes arrive off a
/// message port, and the vertex budget in [`crate::budget`] bounds what comes
/// *out* of the tessellator rather than what goes in. The busiest tile in the
/// confirmation archive carries 43 features across 118 KB; this is four orders
/// of magnitude above that, and exists so a doctored tile answers an error
/// instead of exhausting the worker.
pub const MAX_RING_VERTICES_PER_TILE: usize = 1 << 22;

/// What went wrong, said out loud.
///
/// A refused parse and a genuine absence of buildings are the same observable
/// at the glass, which is the confusion this enum spends its variants to
/// avoid. Worse here than in most places: `squallar_web::worker` runs a job
/// unguarded under `panic = "abort"`, so a failure that is not returned is a
/// dead worker rather than an empty pane.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BuildingsError {
    /// `mvt-reader` refused the bytes. Carried as a string because
    /// `mvt_reader::error::ParserError` is not `Send`, and this rides a
    /// channel.
    Parse(String),
    /// The tile decoded, and has no `building` layer. Not an error at the
    /// glass — most tiles on earth have no buildings — but the caller is the
    /// one that decides that.
    NoBuildingLayer,
    /// A layer declaring an extent of zero, which no arithmetic recovers from.
    ZeroExtent,
    /// An address outside the tile grid at its own zoom.
    NotAddressable(TileId),
    /// One tile's rings passed [`MAX_RING_VERTICES_PER_TILE`].
    TooManyVertices { vertices: usize },
}

impl std::fmt::Display for BuildingsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Parse(e) => write!(f, "the tile did not decode: {e}"),
            Self::NoBuildingLayer => write!(f, "the tile carries no `building` layer"),
            Self::ZeroExtent => write!(f, "the `building` layer declares an extent of zero"),
            Self::NotAddressable(t) => {
                write!(f, "z{}/{}/{} is not a tile that can exist", t.z, t.x, t.y)
            }
            Self::TooManyVertices { vertices } => write!(
                f,
                "one tile's rings carry {vertices} vertices, past the {} ceiling",
                MAX_RING_VERTICES_PER_TILE
            ),
        }
    }
}

/// One closed ring of a footprint, in box kilometres.
///
/// **Canonically wound**: exterior rings run counter-clockwise in the
/// east/north frame and interior rings run clockwise, whatever the tile said.
/// Two things depend on it and they would fail differently. The tessellator's
/// non-zero fill rule needs exterior and interior to disagree or a courtyard
/// fills in; the wall builder needs to know which side is out, and an inverted
/// ring lights every wall of that building from inside.
///
/// **Open**: the repeated closing vertex the wire carries is dropped, so
/// `points.len()` is the edge count.
#[derive(Clone, Debug, PartialEq)]
pub struct Ring {
    /// East and north kilometres from the box origin.
    pub points: Vec<[f64; 2]>,
    /// Whether this ring bounds material or a hole in it.
    pub exterior: bool,
}

impl Ring {
    /// Twice the signed area, positive counter-clockwise. The shoelace sum,
    /// left undivided because only its sign and its being non-zero are ever
    /// read.
    fn double_signed_area(points: &[[f64; 2]]) -> f64 {
        let n = points.len();
        let mut sum = 0.0;
        for i in 0..n {
            let a = points[i];
            let b = points[(i + 1) % n];
            sum += a[0] * b[1] - b[0] * a[1];
        }
        sum
    }
}

/// One building: its rings, and the two heights the walls run between.
#[derive(Clone, Debug, PartialEq)]
pub struct BuildingFootprint {
    /// Every ring of every polygon this feature carries, exteriors and holes
    /// together.
    ///
    /// A multi-polygon feature stays **one** footprint rather than becoming
    /// several, because the budget sheds whole buildings and half a building
    /// is a hole in the skyline rather than a smaller building.
    pub rings: Vec<Ring>,
    /// [`RENDER_MIN_HEIGHT`], metres above the ground.
    pub base_m: f64,
    /// [`RENDER_HEIGHT`], metres above the ground.
    pub height_m: f64,
    /// `[x0, y0, x1, y1]` over every ring, box kilometres.
    pub bbox: [f64; 4],
}

impl BuildingFootprint {
    /// How many ring vertices this footprint carries, which is what the
    /// mesh's size is proportional to.
    pub fn ring_vertices(&self) -> usize {
        self.rings.iter().map(|ring| ring.points.len()).sum()
    }
}

/// Read every extrudable building out of one tile.
///
/// Returns them in the order the tile lists them; [`crate::budget`] is what
/// puts them in height order, because the sort has to see every tile's
/// features at once and this function sees one tile.
///
/// Four kinds of feature are dropped here rather than downstream, each because
/// there is no prism to build:
///
/// * anything marked [`HIDE_3D`];
/// * anything with no [`RENDER_HEIGHT`] — a building whose height is unknown
///   would have to have one invented for it, and inventing it is worse than
///   leaving the footprint flat on a basemap that already draws it;
/// * anything whose top is not above its base, which is the degenerate prism
///   with no walls and two coincident caps (the confirmation archive carries
///   one such feature, at `render_height = 0`);
/// * anything outside `frame`, by bounding box. The caller fetches whole
///   tiles and a tile overhangs the drawn footprint, so this is where that
///   overhang stops costing vertex budget.
pub fn read_footprints(
    tile: TileId,
    mvt: &[u8],
    frame: &BoxFrame,
) -> Result<Vec<BuildingFootprint>, BuildingsError> {
    if !tile.is_addressable() {
        return Err(BuildingsError::NotAddressable(tile));
    }
    let reader = Reader::new(mvt.to_vec()).map_err(|e| BuildingsError::Parse(e.to_string()))?;
    let layers = reader
        .get_layer_metadata()
        .map_err(|e| BuildingsError::Parse(e.to_string()))?;
    let layer = layers
        .iter()
        .find(|layer| layer.name == SOURCE_LAYER)
        .ok_or(BuildingsError::NoBuildingLayer)?;
    if layer.extent == 0 {
        return Err(BuildingsError::ZeroExtent);
    }
    let features = reader
        .get_features_as::<f64>(layer.layer_index)
        .map_err(|e| BuildingsError::Parse(e.to_string()))?;

    let mut out = Vec::new();
    let mut vertices = 0usize;
    for feature in features {
        let properties = feature.properties.unwrap_or_default();
        if hidden_in_3d(&properties) {
            continue;
        }
        let Some(height_m) = number(&properties, RENDER_HEIGHT) else {
            continue;
        };
        let base_m = number(&properties, RENDER_MIN_HEIGHT).unwrap_or(0.0);
        if !(height_m.is_finite() && base_m.is_finite() && height_m > base_m) {
            continue;
        }

        let mut rings = Vec::new();
        collect_rings(&feature.geometry, tile, layer.extent, frame, &mut rings);
        if rings.is_empty() {
            continue;
        }
        vertices += rings.iter().map(|ring| ring.points.len()).sum::<usize>();
        if vertices > MAX_RING_VERTICES_PER_TILE {
            return Err(BuildingsError::TooManyVertices { vertices });
        }

        let bbox = bounding_box(&rings);
        if !frame.overlaps(bbox) {
            continue;
        }
        out.push(BuildingFootprint {
            rings,
            base_m,
            height_m,
            bbox,
        });
    }
    Ok(out)
}

/// Whether [`HIDE_3D`] is set.
///
/// A boolean is what OpenMapTiles emits. An integer is accepted beside it
/// because MVT has six numeric value arms and a producer is free to encode a
/// flag in any of them; a string is not, because "false" would then be truthy
/// and that is the wrong way for this to fail.
fn hidden_in_3d(properties: &HashMap<String, Value>) -> bool {
    match properties.get(HIDE_3D) {
        Some(Value::Bool(set)) => *set,
        Some(Value::Int(n) | Value::SInt(n)) => *n != 0,
        Some(Value::UInt(n)) => *n != 0,
        _ => false,
    }
}

/// A property as a number, whichever of MVT's numeric arms it arrived in.
///
/// **All six arms and not just the obvious one.** MVT has six ways to spell a
/// number and a producer picks whichever encodes shortest, so which one is
/// live is a property of the build rather than of the schema: planetiler
/// writes `render_height` as `sint_value`, the zig-zag varint, and a reader
/// matching only `Value::Int` finds no heights in the shipped archive at all
/// and extrudes nothing. Pinned by
/// `the_shipped_archives_building_layer_carries_exactly_three_property_names`,
/// which asserts the sint arm is the live one rather than assuming it.
///
/// A string is deliberately **not** read. `"30"` in a numeric field is a
/// producer's defect, and turning it into a thirty-metre tower would make that
/// defect invisible.
fn number(properties: &HashMap<String, Value>, key: &str) -> Option<f64> {
    match properties.get(key)? {
        Value::Float(v) => Some(f64::from(*v)),
        Value::Double(v) => Some(*v),
        Value::Int(v) | Value::SInt(v) => Some(*v as f64),
        Value::UInt(v) => Some(*v as f64),
        Value::String(_) | Value::Bool(_) | Value::Null => None,
    }
}

/// Every polygon ring in a feature's geometry, projected and canonically
/// wound. Non-polygon geometry contributes nothing: a point or a line has no
/// footprint to extrude.
fn collect_rings(
    geometry: &Geometry<f64>,
    tile: TileId,
    extent: u32,
    frame: &BoxFrame,
    out: &mut Vec<Ring>,
) {
    match geometry {
        Geometry::Polygon(polygon) => push_polygon(polygon, tile, extent, frame, out),
        Geometry::MultiPolygon(multi) => {
            for polygon in &multi.0 {
                push_polygon(polygon, tile, extent, frame, out);
            }
        }
        Geometry::GeometryCollection(collection) => {
            for inner in &collection.0 {
                collect_rings(inner, tile, extent, frame, out);
            }
        }
        _ => {}
    }
}

fn push_polygon(
    polygon: &Polygon<f64>,
    tile: TileId,
    extent: u32,
    frame: &BoxFrame,
    out: &mut Vec<Ring>,
) {
    if let Some(ring) = ring_of(polygon.exterior(), tile, extent, frame, true) {
        out.push(ring);
        for interior in polygon.interiors() {
            if let Some(ring) = ring_of(interior, tile, extent, frame, false) {
                out.push(ring);
            }
        }
    }
}

/// One `geo-types` ring, projected into box kilometres and wound the way
/// [`Ring`] promises.
///
/// A ring of fewer than three distinct vertices, or one whose area is exactly
/// zero, is dropped: it has no cap to tessellate and its walls would be a
/// zero-width sliver seen edge-on from every direction.
fn ring_of(
    ring: &LineString<f64>,
    tile: TileId,
    extent: u32,
    frame: &BoxFrame,
    exterior: bool,
) -> Option<Ring> {
    let mut points: Vec<[f64; 2]> = Vec::with_capacity(ring.0.len());
    for coord in &ring.0 {
        let (lat, lon) = tile.point_geo(extent, coord.x, coord.y);
        let km = frame.geo_to_km(lat, lon);
        if !km[0].is_finite() || !km[1].is_finite() {
            return None;
        }
        // MVT closes a ring by repeating its first vertex; `Ring` is open, and
        // a duplicate would be a zero-length edge with no wall normal.
        if points.last() != Some(&km) {
            points.push(km);
        }
    }
    if points.len() > 2 && points.first() == points.last() {
        points.pop();
    }
    if points.len() < 3 {
        return None;
    }
    let area = Ring::double_signed_area(&points);
    if area == 0.0 || !area.is_finite() {
        return None;
    }
    // Counter-clockwise for an exterior, clockwise for a hole, whatever the
    // tile's own winding was.
    if (area > 0.0) != exterior {
        points.reverse();
    }
    Some(Ring { points, exterior })
}

/// `[x0, y0, x1, y1]` over every ring.
fn bounding_box(rings: &[Ring]) -> [f64; 4] {
    let mut bbox = [f64::MAX, f64::MAX, f64::MIN, f64::MIN];
    for ring in rings {
        for point in &ring.points {
            bbox[0] = bbox[0].min(point[0]);
            bbox[1] = bbox[1].min(point[1]);
            bbox[2] = bbox[2].max(point[0]);
            bbox[3] = bbox[3].max(point[1]);
        }
    }
    bbox
}

#[cfg(test)]
pub(crate) mod tests;
