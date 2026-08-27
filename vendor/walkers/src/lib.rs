#![doc = include_str!("../README.md")]
#![cfg_attr(not(test), deny(clippy::unwrap_used))]
#![deny(rustdoc::broken_intra_doc_links)]

mod center;
mod local_tiles;
mod map;
mod memory;
mod options;
mod plugin;
// `style`, `text` and `mvt` are public so that a dependent crate can drive the
// vector pipeline itself -- call `mvt::render`, hold the returned
// `Vec<ShapeOrText>`, and do its own text layout and label collision -- rather
// than only being able to hand bytes to `Tile::new` and take back an opaque
// tile. That is what lets this workspace keep style loading and label placement
// in `squallar-egui` instead of growing them as a delta inside this directory.
#[cfg(feature = "mvt")]
pub mod style;
#[cfg(feature = "mvt")]
pub mod text;

#[cfg(not(feature = "mvt"))]
pub mod style {
    /// Dummy style, used when `mtv` feature is not enabled.
    #[derive(Default)]
    pub struct Style;
}

// TODO: I don't want it to be public.
pub mod mercator;

#[cfg(feature = "mvt")]
mod expression;
#[cfg(feature = "mvt")]
pub mod mvt;
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
pub use style::{Color, Filter, Float, Layer, Layout, Paint, Value, json};
#[cfg(feature = "mvt")]
pub use text::{OccupiedAreas, Text};
pub use tiles::{Tile, TileId, TilePiece, Tiles};
pub use zoom::InvalidZoom;

// TODO: In future, I'd like to expose full drawing API instead of this.
#[cfg(feature = "mvt")]
pub use expression::Context;
#[cfg(feature = "mvt")]
pub use mvt::{Geometry, ShapeOrText, render_line, tessellate_polygon};
