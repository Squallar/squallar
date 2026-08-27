#[cfg(feature = "mvt")]
use crate::mvt::{self, ShapeOrText};
#[cfg(feature = "mvt")]
use crate::text::Text;
#[cfg(feature = "mvt")]
use crate::text::{OccupiedAreas, OrientedRect};

use egui::{Color32, Context, Mesh, Rect, Vec2, pos2};
use egui::{ColorImage, TextureHandle};
#[cfg(feature = "mvt")]
use egui::{FontId, Shape};
use image::{ImageError, ImageReader};
use std::collections::HashSet;
use thiserror::Error;

use crate::mercator::{tile_id, total_tiles};
use crate::position::Pixels;
use crate::projector::Projector;
use crate::sources::Attribution;
use crate::style::Style;
use crate::zoom::Zoom;
use crate::{Position, position::PixelsExt as _};

#[derive(Error, Debug)]
pub enum TileError {
    #[error(transparent)]
    Io(#[from] std::io::Error),

    #[error(transparent)]
    Image(#[from] ImageError),

    #[cfg(feature = "mvt")]
    #[error(transparent)]
    Mvt(#[from] mvt::Error),

    #[error("Tile data is empty.")]
    Empty,

    #[error("Unrecognized image format.")]
    UnrecognizedFormat,
}

/// Identifies the tile in the tile grid.
#[derive(Debug, PartialEq, Eq, Hash, Clone, Copy)]
#[cfg_attr(feature = "serde", derive(::serde::Serialize, ::serde::Deserialize))]
pub struct TileId {
    /// X number of the tile.
    pub x: u32,

    /// Y number of the tile.
    pub y: u32,

    /// Zoom level, where 0 means no zoom.
    /// See: <https://wiki.openstreetmap.org/wiki/Zoom_levels>
    pub zoom: u8,
}

impl TileId {
    /// Tile position (in pixels) on the "World bitmap".
    pub fn project(&self, tile_size: f64) -> Pixels {
        Pixels::new(self.x as f64 * tile_size, self.y as f64 * tile_size)
    }

    /// The tile to the east, or `None` at the eastern edge of the grid -- or at a
    /// zoom the grid cannot be counted at, see [`total_tiles`].
    pub fn east(&self) -> Option<TileId> {
        (self.x < total_tiles(self.zoom)? - 1).then_some(TileId {
            x: self.x + 1,
            y: self.y,
            zoom: self.zoom,
        })
    }

    pub fn west(&self) -> Option<TileId> {
        Some(TileId {
            x: self.x.checked_sub(1)?,
            y: self.y,
            zoom: self.zoom,
        })
    }

    pub fn north(&self) -> Option<TileId> {
        Some(TileId {
            x: self.x,
            y: self.y.checked_sub(1)?,
            zoom: self.zoom,
        })
    }

    /// The tile to the south, or `None` at the southern edge of the grid -- or at a
    /// zoom the grid cannot be counted at, see [`total_tiles`].
    pub fn south(&self) -> Option<TileId> {
        (self.y < total_tiles(self.zoom)? - 1).then_some(TileId {
            x: self.x,
            y: self.y + 1,
            zoom: self.zoom,
        })
    }

    /// Is this tile inside the grid for its own zoom level? False at a zoom the
    /// grid cannot be counted at, see [`total_tiles`].
    pub fn valid(&self) -> bool {
        match total_tiles(self.zoom) {
            Some(side) => self.x < side && self.y < side,
            None => false,
        }
    }
}

/// Source of tiles to be put together to render the map.
pub trait Tiles {
    fn at(&mut self, tile_id: TileId) -> Option<TilePiece>;
    fn attribution(&self) -> Attribution;
    fn tile_size(&self) -> u32;
}

#[derive(Clone)]
pub enum Tile {
    Raster(TextureHandle),
    #[cfg(feature = "mvt")]
    Vector(Vec<ShapeOrText>),
}

impl Tile {
    /// Create a tile from raw image data. The data can be either raster image (PNG, JPEG, etc.)
    /// or vector tile (MVT) if the `mvt` feature is enabled.
    pub fn new(image: &[u8], style: &Style, zoom: u8, ctx: &Context) -> Result<Self, TileError> {
        #[cfg(not(feature = "mvt"))]
        let _ = (style, zoom);

        if image.is_empty() {
            return Err(TileError::Empty);
        }

        let reader = ImageReader::new(std::io::Cursor::new(image)).with_guessed_format()?;
        if reader.format().is_some() {
            log::debug!("Decoding tile as raster image.");
            let image = reader.decode()?.to_rgba8();
            let pixels = image.as_flat_samples();
            let image = ColorImage::from_rgba_unmultiplied(
                [image.width() as _, image.height() as _],
                pixels.as_slice(),
            );

            Ok(Self::from_color_image(image, ctx))
        } else {
            #[cfg(feature = "mvt")]
            {
                log::debug!("Trying to decode tile as MVT vector tile.");
                Ok(Self::from_mvt(image, style, zoom)?)
            }
            #[cfg(not(feature = "mvt"))]
            {
                Err(TileError::UnrecognizedFormat)
            }
        }
    }

    #[cfg(feature = "mvt")]
    pub fn from_mvt(data: &[u8], style: &Style, zoom: u8) -> Result<Self, TileError> {
        Ok(Self::Vector(mvt::render(data, style, zoom)?))
    }

    /// Load the texture from egui's [`ColorImage`].
    fn from_color_image(color_image: ColorImage, ctx: &Context) -> Self {
        Self::Raster(ctx.load_texture("image", color_image, Default::default()))
    }

    /// Draw the tile on the given `rect`. The `uv` parameter defines which part of the tile
    /// should be drawn on the `rect`.
    fn draw(&self, painter: &egui::Painter, rect: Rect, uv: Rect, transparency: f32) {
        match self {
            Tile::Raster(texture_handle) => {
                let mut mesh = Mesh::with_texture(texture_handle.id());
                mesh.add_rect_with_uv(rect, uv, Color32::WHITE.gamma_multiply(transparency));
                painter.add(egui::Shape::mesh(mesh));
            }
            #[cfg(feature = "mvt")]
            Tile::Vector(shapes) => {
                // Renderer needs to work on the full tile, before it was clipped with `uv`...
                let full_rect = full_rect_of_clipped_tile(rect, uv);

                // ...and then it can be clipped to the `rect`.
                let painter = painter.with_clip_rect(rect);

                let mut occupied_text_areas = OccupiedAreas::new();

                // Need to collect it to avoid deadlock caused by `Painter::extend` and `fonts_mut`.
                let shapes: Vec<_> = mvt::transformed(shapes, full_rect)
                    .into_iter()
                    .map(|shape_or_text| match shape_or_text {
                        ShapeOrText::Shape(shape) => shape,
                        ShapeOrText::Text(text) => {
                            self.draw_text(text, painter.ctx(), &mut occupied_text_areas)
                        }
                    })
                    .collect();

                painter.extend(shapes);
            }
        }
    }

    #[cfg(feature = "mvt")]
    fn draw_text(
        &self,
        text: Text,
        ctx: &Context,
        occupied_text_areas: &mut OccupiedAreas,
    ) -> Shape {
        use egui::epaint::TextShape;

        let mut layout_job = egui::text::LayoutJob::default();

        layout_job.append(
            &text.text,
            0.0,
            egui::TextFormat {
                font_id: FontId::proportional(text.font_size),
                color: text.text_color,
                background: text.background_color,
                ..Default::default()
            },
        );

        let galley = ctx.fonts_mut(|fonts| fonts.layout_job(layout_job));

        let area = OrientedRect::new(text.position, text.angle, galley.size());
        let top_left = area.top_left();

        if occupied_text_areas.try_occupy(area) {
            TextShape::new(top_left, galley, text.text_color)
                .with_angle(text.angle)
                .into()
        } else {
            Shape::Noop
        }
    }
}

/// Clipped piece of a tile.
pub struct TilePiece {
    pub tile: Tile,
    pub uv: Rect,
}

impl TilePiece {
    pub fn new(tile: Tile, uv: Rect) -> Self {
        Self { tile, uv }
    }
}

/// Draw every tile of one layer that the painter's clip rect can see.
///
/// **`projector` places, `painter` culls, and the two rects are not the same rect.**
/// `Map::show` builds the projector from the widget rect it allocated and clips the painter
/// to that same rect — but [`egui::Painter::with_clip_rect`] *intersects* rather than sets,
/// so a parent that clips the widget (a scroll area, a panel smaller than the map) leaves the
/// painter with a strictly narrower rect. Placement must follow the widget rect, which is
/// what every other consumer of the projector places against; culling must follow the true
/// clip, which is what will actually be shown. So the invariant here is containment, not
/// equality, and it holds by construction from `intersect`.
///
/// This is a behaviour change in the narrowed case, and a fix: the flood fill used to offset
/// tiles from `painter.clip_rect().center()`, so under a narrowing parent the tiles slid away
/// from the markers and overlays the plugins drew through the projector.
///
/// **This whole path is unreachable in this application** — both `Map::new` sites in
/// `squallar-egui::ui_map` pass `None` for tiles and neither adds a layer, so a green board
/// says nothing about it. `map::tests::a_tile_layer_is_placed_by_the_projector_and_culled_by_the_painter`
/// is the only thing that executes it.
pub(crate) fn draw_tiles(
    painter: &egui::Painter,
    projector: &Projector,
    map_center: Position,
    zoom: Zoom,
    tiles: &mut dyn Tiles,
    transparency: f32,
) {
    debug_assert!(
        projector.clip_rect().contains_rect(painter.clip_rect())
            || !painter.clip_rect().is_positive(),
        "the painter's clip {:?} is not inside the projector's viewport {:?}, so a tile could \
         be culled against one map and drawn onto another",
        painter.clip_rect(),
        projector.clip_rect(),
    );

    let mut meshes = Default::default();
    flood_fill_tiles(
        painter,
        projector,
        tile_id(map_center, zoom.round(), tiles.tile_size()),
        tiles,
        transparency,
        &mut meshes,
    );
}

/// Use simple [flood fill algorithm](https://en.wikipedia.org/wiki/Flood_fill) to draw tiles on the map.
fn flood_fill_tiles(
    painter: &egui::Painter,
    projector: &Projector,
    tile_id: TileId,
    tiles: &mut dyn Tiles,
    transparency: f32,
    meshes: &mut HashSet<TileId>,
) {
    // `Projector::tile_rect` already carries both corrections this used to spell out here:
    // the difference between integer and floating point zoom levels, and the source's tile
    // size, which `mercator::tile_id` folded into `tile_id.zoom` on the way in.
    let tile_rect = projector.tile_rect(tile_id);

    if painter.clip_rect().intersects(tile_rect) && meshes.insert(tile_id) {
        if let Some(tile) = tiles.at(tile_id) {
            tile.tile.draw(painter, tile_rect, tile.uv, transparency)
        }

        for next_tile_id in [
            tile_id.north(),
            tile_id.east(),
            tile_id.south(),
            tile_id.west(),
        ]
        .iter()
        .flatten()
        {
            flood_fill_tiles(
                painter,
                projector,
                *next_tile_id,
                tiles,
                transparency,
                meshes,
            );
        }
    }
}

/// Take a piece of a tile with lower zoom level and use it as a required tile.
///
/// Returns the ancestor at `available_zoom` and the sub-rectangle of it that
/// `tile_id` covers, in texture (`uv`) coordinates. `None` if `available_zoom` is
/// deeper than the tile's own zoom -- there is no such ancestor -- or if the two
/// are far enough apart that the ratio does not fit a `u32`.
pub fn interpolate_from_lower_zoom(tile_id: TileId, available_zoom: u8) -> Option<(TileId, Rect)> {
    let dzoom = 2u32.checked_pow(tile_id.zoom.checked_sub(available_zoom)? as u32)?;

    let x = (tile_id.x / dzoom, tile_id.x % dzoom);
    let y = (tile_id.y / dzoom, tile_id.y % dzoom);

    let zoomed_tile_id = TileId {
        x: x.0,
        y: y.0,
        zoom: available_zoom,
    };

    let z = (dzoom as f32).recip();

    let uv = Rect::from_min_max(
        pos2(x.1 as f32 * z, y.1 as f32 * z),
        pos2(x.1 as f32 * z + z, y.1 as f32 * z + z),
    );

    Some((zoomed_tile_id, uv))
}

#[cfg(any(feature = "mvt", test))]
/// Get the original rect which was clipped using the `uv`.
fn full_rect_of_clipped_tile(rect: Rect, uv: Rect) -> Rect {
    let uv_width = uv.max.x - uv.min.x;
    let uv_height = uv.max.y - uv.min.y;

    let full_width = rect.width() / uv_width;
    let full_height = rect.height() / uv_height;

    let full_min_x = rect.min.x - (full_width * uv.min.x);
    let full_min_y = rect.min.y - (full_height * uv.min.y);

    Rect::from_min_max(
        pos2(full_min_x, full_min_y),
        pos2(full_min_x + full_width, full_min_y + full_height),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_full_rect_of_clipped_tile() {
        let rect = Rect::from_min_max(pos2(0.0, 0.0), pos2(50.0, 50.0));
        let uv = Rect::from_min_max(pos2(0.0, 0.0), pos2(0.5, 0.5));

        let full_rect = full_rect_of_clipped_tile(rect, uv);

        assert_eq!(full_rect.min, pos2(0.0, 0.0));
        assert_eq!(full_rect.max, pos2(100.0, 100.0));
    }

    /// `TileId::zoom` is a `u8`, so a tile id can name a zoom the `u32` tile grid
    /// cannot count. The public neighbour accessors reach that arithmetic.
    #[test]
    fn tile_id_past_the_u32_grid_is_not_valid() {
        let tile_id = TileId {
            x: 0,
            y: 0,
            zoom: 32,
        };

        assert!(!tile_id.valid());
        assert_eq!(tile_id.east(), None);
        assert_eq!(tile_id.south(), None);
    }

    /// A tile cannot be cut out of an ancestor that is deeper than it is.
    #[test]
    fn interpolating_from_a_deeper_zoom_is_none() {
        let tile_id = TileId {
            x: 1,
            y: 1,
            zoom: 2,
        };

        assert_eq!(interpolate_from_lower_zoom(tile_id, 3), None);

        // Its own zoom is the whole tile.
        let (ancestor, uv) = interpolate_from_lower_zoom(tile_id, 2).expect("same zoom resolves");
        assert_eq!(ancestor, tile_id);
        assert_eq!(uv, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)));
    }

    #[test]
    fn tile_id_cannot_go_beyond_limits() {
        // There is only one tile at zoom 0.
        let tile_id = TileId {
            x: 0,
            y: 0,
            zoom: 0,
        };

        assert_eq!(tile_id.west(), None);
        assert_eq!(tile_id.north(), None);
        assert_eq!(tile_id.south(), None);
        assert_eq!(tile_id.east(), None);

        // There are 2 tiles at zoom 1.
        let tile_id = TileId {
            x: 0,
            y: 0,
            zoom: 1,
        };

        assert_eq!(tile_id.west(), None);
        assert_eq!(tile_id.north(), None);

        assert_eq!(
            tile_id.south(),
            Some(TileId {
                x: 0,
                y: 1,
                zoom: 1
            })
        );

        assert_eq!(
            tile_id.east(),
            Some(TileId {
                x: 1,
                y: 0,
                zoom: 1
            })
        );
    }
}
