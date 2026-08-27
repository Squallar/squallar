#![doc = include_str!("../README.md")]
#![deny(clippy::unwrap_used, rustdoc::broken_intra_doc_links)]

mod center;
mod local_tiles;
mod map;
mod memory;
mod options;
mod plugin;
#[cfg(feature = "mvt")]
mod style;
#[cfg(feature = "mvt")]
mod text;

#[cfg(not(feature = "mvt"))]
mod style {
    /// Dummy style, used when `mtv` feature is not enabled.
    #[derive(Default)]
    pub struct Style;
}

// TODO: I don't want it to be public.
pub mod mercator;

#[cfg(feature = "mvt")]
mod expression;
#[cfg(feature = "mvt")]
mod mvt;
mod position;
mod projector;
pub mod sources;
mod tiles;
mod zoom;

pub use local_tiles::LocalTiles;
pub use map::Map;
pub use memory::MapMemory;
pub use options::Options;
pub use plugin::Plugin;
pub use position::{Position, lat_lon, lon_lat};
pub use projector::Projector;
pub use style::Style;
#[cfg(feature = "mvt")]
pub use style::{Color, Filter, Float, Layer, Paint, Value, json};
pub use tiles::{Tile, TileId, TilePiece, Tiles};
pub use zoom::InvalidZoom;

// TODO: In future, I'd like to expose full drawing API instead of this.
#[cfg(feature = "mvt")]
pub use expression::Context;
#[cfg(feature = "mvt")]
pub use mvt::{Geometry, ShapeOrText, render_line, tessellate_polygon};
