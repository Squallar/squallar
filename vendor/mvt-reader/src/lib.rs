//! # mvt-reader
//!
//! `mvt-reader` is a Rust library for decoding and reading Mapbox vector tiles.
//!
//! It provides the `Reader` struct, which allows you to read vector tiles and access their layers and features.
//!
//! # Usage
//!
//! To use the `mvt-reader` library in your Rust project, add the following to your `Cargo.toml` file:
//!
//! ```toml
//! [dependencies]
//! mvt-reader = "2.4.0"
//! ```
//!
//! Then, you can import and use the library in your code:
//!
//! ```rust
//! use mvt_reader::{Reader, error::{ParserError}};
//!
//! fn main() -> Result<(), ParserError> {
//!   // Read a vector tile from file or data
//!   let data = vec![/* Vector tile data */];
//!   let reader = Reader::new(data)?;
//!
//!   // Get layer names
//!   let layer_names = reader.get_layer_names()?;
//!   for name in layer_names {
//!     println!("Layer: {}", name);
//!   }
//!
//!   // Get features for a specific layer
//!   let layer_index = 0;
//!   let features = reader.get_features(layer_index)?;
//!   for feature in features {
//!     todo!()
//!   }
//!
//!   Ok(())
//! }
//! ```
//!
//! # Features
//!
//! The `mvt-reader` library provides the following features:
//!
//! - `wasm`: Enables the compilation of the library as a WebAssembly module, allowing usage in JavaScript/TypeScript projects.
//! - `protoc`: Enables the use of `prost-build` to compile the protobuf definition sources from `vector_tile.proto`. This is useful for development and testing purposes, but it is not required for using the library in production. If the `protoc` feature is not enabled, the library will use pre-generated Rust code for the protobuf definitions.
//!
//! To enable the `wasm` feature, add the following to your `Cargo.toml` file:
//!
//! ```toml
//! [dependencies]
//! mvt-reader = { version = "2.4.0", features = ["wasm"] }
//! ```
//!
//! To enable the `protoc` feature, add the following to your `Cargo.toml` file:
//!
//! ```toml
//! [dependencies]
//! mvt-reader = { version = "2.4.0", features = ["protoc"] }
//! ```
//!
//! # License
//!
//! This project is licensed under the [MIT License](https://github.com/codeart1st/mvt-reader/blob/main/LICENSE).

pub mod error;
pub mod feature;
pub mod layer;

mod vector_tile;

/// LOCAL CHANGE: the peak-allocation pin this vendored copy exists for.
/// Upstream ships no test target at all, so there was nothing to inherit.
#[cfg(test)]
mod peak_allocation_tests;

use feature::{Feature, Value};
use geo_types::{
    Coord, CoordNum, Geometry, LineString, MultiLineString, MultiPoint, MultiPolygon, Point,
    Polygon,
};
use layer::Layer;
use num_traits::NumCast;
use prost::{Message, bytes::Bytes};
use vector_tile::{Tile, tile::GeomType};

/// The dimension used for the vector tile.
const DIMENSION: u32 = 2;

/// Reader for decoding and accessing vector tile data.
pub struct Reader {
    tile: Tile,
}

impl Reader {
    /// Creates a new `Reader` instance with the provided vector tile data.
    ///
    /// # Arguments
    ///
    /// * `data` - The vector tile data as a byte vector.
    ///
    /// # Returns
    ///
    /// A result containing the `Reader` instance if successful, or a `DecodeError` if decoding the vector tile data fails.
    ///
    /// # Examples
    ///
    /// ```
    /// use mvt_reader::Reader;
    ///
    /// let data = vec![/* Vector tile data */];
    /// let reader = Reader::new(data);
    /// ```
    pub fn new(data: Vec<u8>) -> Result<Self, error::ParserError> {
        let tile = Tile::decode(Bytes::from(data))?;
        Ok(Self { tile })
    }

    /// Retrieves the names of the layers in the vector tile.
    ///
    /// # Returns
    ///
    /// A result containing a vector of layer names if successful, or a `ParserError` if there is an error parsing the tile.
    ///
    /// # Examples
    ///
    /// ```
    /// use mvt_reader::Reader;
    ///
    /// let data = vec![/* Vector tile data */];
    /// let reader = Reader::new(data).unwrap();
    ///
    /// match reader.get_layer_names() {
    ///   Ok(layer_names) => {
    ///     for name in layer_names {
    ///       println!("Layer: {}", name);
    ///     }
    ///   }
    ///   Err(error) => {
    ///     todo!();
    ///   }
    /// }
    /// ```
    pub fn get_layer_names(&self) -> Result<Vec<String>, error::ParserError> {
        process_layers(&self.tile.layers, |layer, _| layer.name.clone())
    }

    /// Retrieves metadata about the layers in the vector tile.
    ///
    /// # Returns
    ///
    /// A result containing a vector of `Layer` structs if successful, or a `ParserError` if there is an error parsing the tile.
    ///
    /// # Examples
    ///
    /// ```
    /// use mvt_reader::Reader;
    ///
    /// let data = vec![/* Vector tile data */];
    /// let reader = Reader::new(data).unwrap();
    ///
    /// match reader.get_layer_metadata() {
    ///   Ok(layers) => {
    ///     for layer in layers {
    ///       println!("Layer: {}", layer.name);
    ///       println!("Extent: {}", layer.extent);
    ///     }
    ///   }
    ///   Err(error) => {
    ///     todo!();
    ///   }
    /// }
    /// ```
    pub fn get_layer_metadata(&self) -> Result<Vec<Layer>, error::ParserError> {
        process_layers(&self.tile.layers, |layer, index| Layer {
            layer_index: index,
            version: layer.version,
            name: layer.name.clone(),
            feature_count: layer.features.len(),
            extent: layer.extent.unwrap_or(4096),
        })
    }

    /// Retrieves the features of a specific layer in the vector tile.
    ///
    /// # Arguments
    ///
    /// * `layer_index` - The index of the layer.
    ///
    /// # Returns
    ///
    /// A result containing a vector of features if successful, or a `ParserError` if there is an error parsing the tile or accessing the layer.
    ///
    /// # Examples
    ///
    /// ```
    /// use mvt_reader::Reader;
    ///
    /// let data = vec![/* Vector tile data */];
    /// let reader = Reader::new(data).unwrap();
    ///
    /// match reader.get_features(0) {
    ///   Ok(features) => {
    ///     for feature in features {
    ///       todo!();
    ///     }
    ///   }
    ///   Err(error) => {
    ///     todo!();
    ///   }
    /// }
    /// ```
    pub fn get_features(&self, layer_index: usize) -> Result<Vec<Feature>, error::ParserError> {
        self.get_features_as::<f32>(layer_index)
    }

    /// Retrieves the features of a specific layer with geometry coordinates in the specified numeric type.
    ///
    /// This is a generic version of [`get_features`](Reader::get_features) that allows you to choose
    /// the coordinate type for the geometry. Supported types include `f32` (default), `i32`, and `i16`.
    ///
    /// # Arguments
    ///
    /// * `layer_index` - The index of the layer.
    ///
    /// # Type Parameters
    ///
    /// * `T` - The numeric type for geometry coordinates (e.g. `f32`, `i32`, `i16`).
    ///
    /// # Returns
    ///
    /// A result containing a vector of features if successful, or a `ParserError` if there is an error parsing the tile or accessing the layer.
    ///
    /// # Examples
    ///
    /// ```
    /// use mvt_reader::Reader;
    ///
    /// let data = vec![/* Vector tile data */];
    /// let reader = Reader::new(data).unwrap();
    ///
    /// // Get features with i32 coordinates
    /// let features = reader.get_features_as::<i32>(0);
    ///
    /// // Get features with i16 coordinates
    /// let features = reader.get_features_as::<i16>(0);
    /// ```
    pub fn get_features_as<T: CoordNum>(
        &self,
        layer_index: usize,
    ) -> Result<Vec<Feature<T>>, error::ParserError> {
        let layer = self.tile.layers.get(layer_index);
        match layer {
            Some(layer) => {
                let mut features = Vec::with_capacity(layer.features.len());
                for feature in layer.features.iter() {
                    if let Some(geom_type) = feature.r#type {
                        let geom_type = GeomType::try_from(geom_type)
                            .map_err(|_| error::ParserError::InvalidGeometry)?;
                        let parsed_geometry = parse_geometry::<T>(&feature.geometry, geom_type)?;
                        let parsed_tags = parse_tags(&feature.tags, &layer.keys, &layer.values)?;

                        features.push(Feature {
                            geometry: parsed_geometry,
                            id: feature.id,
                            properties: Some(parsed_tags),
                        });
                    }
                }
                Ok(features)
            }
            None => Ok(vec![]),
        }
    }
}

fn process_layers<T, F>(
    layers: &[vector_tile::tile::Layer],
    mut processor: F,
) -> Result<Vec<T>, error::ParserError>
where
    F: FnMut(&vector_tile::tile::Layer, usize) -> T,
{
    let mut results = Vec::with_capacity(layers.len());
    for (index, layer) in layers.iter().enumerate() {
        match layer.version {
            1 | 2 => results.push(processor(layer, index)),
            _ => {
                return Err(error::ParserError::UnsupportedVersion {
                    layer_name: layer.name.clone(),
                    version: layer.version,
                });
            }
        }
    }
    Ok(results)
}

fn parse_tags(
    tags: &[u32],
    keys: &[String],
    values: &[vector_tile::tile::Value],
) -> Result<std::collections::HashMap<String, Value>, error::ParserError> {
    let mut result = std::collections::HashMap::new();
    for item in tags.chunks(2) {
        if item.len() != 2 || item[0] as usize >= keys.len() || item[1] as usize >= values.len() {
            return Err(error::ParserError::InvalidTags);
        }
        result.insert(
            keys[item[0] as usize].clone(),
            map_value(values[item[1] as usize].clone()),
        );
    }
    Ok(result)
}

fn map_value(value: vector_tile::tile::Value) -> Value {
    if let Some(s) = value.string_value {
        return Value::String(s);
    }
    if let Some(f) = value.float_value {
        return Value::Float(f);
    }
    if let Some(d) = value.double_value {
        return Value::Double(d);
    }
    if let Some(i) = value.int_value {
        return Value::Int(i);
    }
    if let Some(u) = value.uint_value {
        return Value::UInt(u);
    }
    if let Some(s) = value.sint_value {
        return Value::SInt(s);
    }
    if let Some(b) = value.bool_value {
        return Value::Bool(b);
    }
    Value::Null
}

fn shoelace_formula<T: CoordNum>(points: &[Point<T>]) -> f32 {
    let mut area: f32 = 0.0;
    let n = points.len();
    let mut v1 = points[n - 1];
    for v2 in points.iter().take(n) {
        let v2y: f32 = NumCast::from(v2.y()).unwrap_or(0.0);
        let v1y: f32 = NumCast::from(v1.y()).unwrap_or(0.0);
        let v2x: f32 = NumCast::from(v2.x()).unwrap_or(0.0);
        let v1x: f32 = NumCast::from(v1.x()).unwrap_or(0.0);
        area += (v2y - v1y) * (v2x + v1x);
        v1 = *v2;
    }
    area * 0.5
}

fn parse_geometry<T: CoordNum>(
    geometry_data: &[u32],
    geom_type: GeomType,
) -> Result<Geometry<T>, error::ParserError> {
    if geom_type == GeomType::Unknown {
        return Err(error::ParserError::InvalidGeometry);
    }

    // worst case capacity to prevent reallocation. not needed to be exact.
    //
    // LOCAL CHANGE: this reservation is made ONCE, for the first ring. The two
    // `coordinates = Vec::with_capacity(geometry_data.len())` lines that used to
    // start each SUBSEQUENT ring at the same worst case are `Vec::new()` below.
    // See VENDORED.md: the rings of one feature are all held at once in
    // `linestrings`, un-shrunk, so reserving the whole feature's command count
    // for each of them made a feature's peak `rings x commands` rather than
    // `commands` -- quadratic in one feature, and on wasm32 an infallible
    // allocation against a 1 GiB module ceiling.
    let mut coordinates: Vec<Coord<T>> = Vec::with_capacity(geometry_data.len());
    let mut polygons: Vec<Polygon<T>> = Vec::new();
    let mut linestrings: Vec<LineString<T>> = Vec::new();

    let mut cursor: [i32; 2] = [0, 0];
    let mut parameter_count: u32 = 0;

    for value in geometry_data.iter() {
        if parameter_count == 0 {
            let command_integer = value;
            let id = (command_integer & 0x7) as u8;
            match id {
                1 => {
                    // MoveTo
                    parameter_count = (command_integer >> 3) * DIMENSION;
                    if geom_type == GeomType::Linestring && !coordinates.is_empty() {
                        linestrings.push(LineString::new(coordinates));
                        // start with a new linestring (LOCAL CHANGE: empty, not the whole
                        // feature's command count again -- see the note at the first
                        // reservation above)
                        coordinates = Vec::new();
                    }
                }
                2 => {
                    // LineTo
                    parameter_count = (command_integer >> 3) * DIMENSION;
                }
                7 => {
                    // ClosePath
                    let first_coordinate = match coordinates.first() {
                        Some(coord) => coord.to_owned(),
                        None => {
                            return Err(error::ParserError::InvalidGeometry);
                        }
                    };
                    coordinates.push(first_coordinate);

                    let ring = LineString::new(coordinates);

                    let area = shoelace_formula(&ring.clone().into_points());

                    if area > 0.0 {
                        // exterior ring
                        if !linestrings.is_empty() {
                            // finish previous geometry
                            polygons.push(Polygon::new(
                                linestrings[0].clone(),
                                linestrings[1..].into(),
                            ));
                            linestrings = Vec::new();
                        }
                    }

                    linestrings.push(ring);
                    // start a new sequence (LOCAL CHANGE: empty, not the whole feature's
                    // command count again -- see the note at the first reservation above)
                    coordinates = Vec::new();
                }
                _ => (),
            }
        } else {
            let parameter_integer = value;
            let integer_value =
                ((parameter_integer >> 1) as i32) ^ -((parameter_integer & 1) as i32);
            if parameter_count.is_multiple_of(DIMENSION) {
                // clip value (LOCAL CHANGE: upstream spells this as a `match`,
                // which `clippy::manual_unwrap_or` denies at this workspace's
                // `-D warnings`. Same value, same clip.)
                cursor[0] = cursor[0].checked_add(integer_value).unwrap_or(i32::MAX);
            } else {
                // clip value (LOCAL CHANGE: upstream spells this as a `match`,
                // which `clippy::manual_unwrap_or` denies at this workspace's
                // `-D warnings`. Same value, same clip.)
                cursor[1] = cursor[1].checked_add(integer_value).unwrap_or(i32::MAX);
                let x = NumCast::from(cursor[0])
                    .ok_or(error::ParserError::CoordinateOverflow { value: cursor[0] })?;
                let y = NumCast::from(cursor[1])
                    .ok_or(error::ParserError::CoordinateOverflow { value: cursor[1] })?;
                coordinates.push(Coord { x, y });
            }
            parameter_count -= 1;
        }
    }

    match geom_type {
        GeomType::Linestring => {
            // the last linestring is in coordinates vec
            if !linestrings.is_empty() {
                linestrings.push(LineString::new(coordinates));
                return Ok(MultiLineString::new(linestrings).into());
            }
            Ok(LineString::new(coordinates).into())
        }
        GeomType::Point => Ok(MultiPoint(
            coordinates
                .iter()
                .map(|coord| Point::new(coord.x, coord.y))
                .collect(),
        )
        .into()),
        GeomType::Polygon => {
            if !linestrings.is_empty() {
                // finish pending polygon
                polygons.push(Polygon::new(
                    linestrings[0].clone(),
                    linestrings[1..].into(),
                ));
                return Ok(MultiPolygon::new(polygons).into());
            }
            match polygons.first() {
                Some(polygon) => Ok(polygon.to_owned().into()),
                None => Err(error::ParserError::InvalidGeometry),
            }
        }
        GeomType::Unknown => Err(error::ParserError::InvalidGeometry),
    }
}

// LOCAL CHANGE: upstream's `#[cfg(feature = "wasm")] pub mod wasm` -- 208
// lines of geojson/serde-wasm-bindgen glue publishing a `Reader` to
// JavaScript -- is deleted with the `wasm` feature that gated it. This
// workspace calls this crate from Rust; the module is dead here and its
// dependencies were the lockfile cost the manifest note describes.
// See VENDORED.md.
