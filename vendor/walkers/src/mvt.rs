//! Renderer for Mapbox Vector Tiles.

use std::collections::HashMap;

use egui::{
    Color32, Mesh, Rect, Shape, Stroke,
    emath::TSTransform,
    epaint::{Vertex, WHITE_UV},
    pos2, vec2,
};
pub use geo_types::{Coord, Geometry, Line};
use log::warn;
use lyon_path::{
    Path, Polygon,
    geom::{Point, point},
};
use lyon_tessellation::{
    BuffersBuilder, FillOptions, FillTessellator, FillVertex, TessellationError, VertexBuffers,
};
use mvt_reader::{Reader, feature::Value};
use serde_json::{Number, Value as JsonValue};

use crate::{
    expression::{Context, Properties},
    style::{Filter, Layer, Layout, Paint, Style},
    text::Text,
};

#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error("Decoding MVT failed: {0}.")]
    Mvt(String),
    #[error("Layer not found: {0}. Available layers: {1:?}")]
    LayerNotFound(String, Vec<String>),
    #[error("Unsupported layer extent: {0}")]
    UnsupportedLayerExtent(String),
    #[error("Unsupported kind: {0:?}")]
    UnsupportedFeatureKind(HashMap<String, Value>),
    #[error("Missing kind in properties: {0:?}")]
    FeatureWithoutKind(HashMap<String, Value>),
    #[error("Missing properties in feature")]
    FeatureWithoutProperties,
    #[error(transparent)]
    Tessellation(#[from] TessellationError),
}

/// Custom conversion because mvt_reader::error::Error is not Send.
impl From<mvt_reader::error::ParserError> for Error {
    fn from(err: mvt_reader::error::ParserError) -> Self {
        Error::Mvt(err.to_string())
    }
}

/// Currently this is the only supported extent.
const ONLY_SUPPORTED_EXTENT: u32 = 4096;

#[derive(Debug, Clone)]
pub enum ShapeOrText {
    Shape(Shape),
    Text(Text),
}

impl From<Shape> for ShapeOrText {
    fn from(shape: Shape) -> Self {
        ShapeOrText::Shape(shape)
    }
}

impl From<Mesh> for ShapeOrText {
    fn from(mesh: Mesh) -> Self {
        ShapeOrText::Shape(Shape::Mesh(mesh.into()))
    }
}

impl ShapeOrText {
    /// This shape placed on screen by `transform`, as a new value.
    ///
    /// **Geometry is transformed; stroke widths and font sizes are not.** A
    /// style's `line-width` and `text-size` are in *screen points* — MapLibre
    /// defines them that way, and a style's own zoom stops are what scale them
    /// — while the geometry these shapes carry is in MVT extent units. So the
    /// placement is an affine map on positions only, and a road is the width
    /// the style asked for whatever side the tile is drawn at. See
    /// [`render_line`].
    ///
    /// `Text` already worked this way upstream, which is where the rule comes
    /// from: it scaled `position` and left `font_size` alone.
    ///
    /// **This replaces an in-place `transform(&mut self)`.** That spelling
    /// forced the caller to own a copy first, and for a `Shape::Mesh` — which
    /// holds an `Arc<Mesh>` the tile cache is still referencing — the mutation
    /// then went through `Arc::make_mut` and copied the mesh a second time.
    /// Building the placed value directly is one pass over the points instead
    /// of a copy followed by a walk.
    pub fn placed(&self, transform: TSTransform) -> ShapeOrText {
        let place = |p: egui::Pos2| transform.scaling * p + transform.translation;

        match self {
            // The hot arm: a dense city tile is mostly tessellated fills.
            ShapeOrText::Shape(Shape::Mesh(mesh)) => ShapeOrText::Shape(Shape::Mesh(
                Mesh {
                    indices: mesh.indices.clone(),
                    vertices: mesh
                        .vertices
                        .iter()
                        .map(|vertex| Vertex {
                            pos: place(vertex.pos),
                            ..*vertex
                        })
                        .collect(),
                    texture_id: mesh.texture_id,
                }
                .into(),
            )),
            ShapeOrText::Shape(Shape::Path(path)) => {
                let mut placed = path.clone();
                for point in &mut placed.points {
                    *point = place(*point);
                }
                ShapeOrText::Shape(Shape::Path(placed))
            }
            // `render` emits only `Mesh`, `Path`, `Rect` and `Text`, so this is
            // the background rectangle and nothing else. `Shape::transform`
            // would scale a stroke width, and the background carries none.
            ShapeOrText::Shape(shape) => {
                let mut placed = shape.clone();
                placed.transform(transform);
                ShapeOrText::Shape(placed)
            }
            ShapeOrText::Text(text) => {
                let mut placed = text.clone();
                placed.position = place(placed.position);
                ShapeOrText::Text(placed)
            }
        }
    }
}

/// Render MVT data into a list of [`epaint::Shape`]s.
pub fn render(data: &[u8], style: &Style, zoom: u8) -> Result<Vec<ShapeOrText>, Error> {
    let data = mvt_reader::Reader::new(data.to_vec())?;
    let mut shapes = Vec::new();

    for layer in &style.layers {
        match layer {
            Layer::Background { paint } => {
                let context = Context::new("None".to_string(), HashMap::new(), zoom);

                let bg_color = if let Some(color) = &paint.background_color {
                    color.evaluate(&context)
                } else {
                    Color32::WHITE
                };

                let rect = Rect::from_min_size(
                    pos2(0.0, 0.0),
                    vec2(ONLY_SUPPORTED_EXTENT as f32, ONLY_SUPPORTED_EXTENT as f32),
                );
                shapes.push(Shape::rect_filled(rect, 0.0, bg_color).into());
            }
            Layer::Fill {
                source_layer,
                filter,
                paint,
            } => {
                for (geometry, context) in
                    get_layer_features(&data, zoom, source_layer, filter.as_ref())?
                {
                    if let Err(err) = render_polygon(&geometry, &context, &mut shapes, paint) {
                        warn!("{err}");
                    }
                }
            }
            Layer::Line {
                source_layer,
                filter,
                paint,
            } => {
                for (geometry, context) in
                    get_layer_features(&data, zoom, source_layer, filter.as_ref())?
                {
                    if let Err(err) = render_line(&geometry, &context, &mut shapes, paint) {
                        warn!("{err}");
                    }
                }
            }
            Layer::Symbol {
                source_layer,
                filter,
                layout,
                paint,
            } => {
                for (geometry, context) in
                    get_layer_features(&data, zoom, source_layer, filter.as_ref())?
                {
                    if let Err(err) = render_symbol(&geometry, &context, &mut shapes, layout, paint)
                    {
                        warn!("{err}");
                    }
                }
            }
            layer => {
                log::warn!("Unsupported layer type in style: {layer:?}");
                continue;
            }
        }
    }

    let shapes = coalesce_adjacent_meshes(shapes);

    log::trace!("Rendered {} shapes", shapes.len());
    Ok(shapes)
}

/// Fold each run of neighbouring [`Shape::Mesh`]es into one mesh.
///
/// **Order-preserving, and that is the whole of the safety argument.** Only
/// *adjacent* meshes are folded, so every shape still draws in the position the
/// style put it in: a fill layer's polygons are consecutive, a line layer
/// between two fill layers breaks the run, and the triangles, their order and
/// their colours are untouched. `Mesh::append` is `egui`'s own concatenation
/// and takes the index rebasing with it; every mesh here carries
/// `TextureId::default()`, which is what makes them appendable at all.
///
/// It is worth doing because a tile is *thousands* of tiny fills. Measured on
/// the committed Monaco fixture's z14 tile: 2,257 meshes holding 18,018
/// vertices — eight vertices each. Every one of them was a separate `Arc<Mesh>`
/// with two heap allocations behind it, held for as long as the tile is cached
/// and rebuilt by the consumer on every frame it is drawn.
fn coalesce_adjacent_meshes(shapes: Vec<ShapeOrText>) -> Vec<ShapeOrText> {
    let mut folded: Vec<ShapeOrText> = Vec::with_capacity(shapes.len());

    for shape in shapes {
        let ShapeOrText::Shape(Shape::Mesh(mesh)) = shape else {
            folded.push(shape);
            continue;
        };

        if let Some(ShapeOrText::Shape(Shape::Mesh(previous))) = folded.last_mut()
            && previous.texture_id == mesh.texture_id
        {
            // `previous` is the only owner: nothing has been handed out yet.
            std::sync::Arc::make_mut(previous).append_ref(&mesh);
            continue;
        }

        folded.push(ShapeOrText::Shape(Shape::Mesh(mesh)));
    }

    // The appends above grow by doubling, so the folded meshes carry slack of
    // their own. A tile is cached, not transient, so it is worth one pass to
    // give it back.
    for shape in &mut folded {
        if let ShapeOrText::Shape(Shape::Mesh(mesh)) = shape {
            let mesh = std::sync::Arc::make_mut(mesh);
            mesh.vertices.shrink_to_fit();
            mesh.indices.shrink_to_fit();
        }
    }

    folded.shrink_to_fit();
    folded
}

/// The transform that puts a tile's shapes on `rect`.
///
/// Exposed so a consumer can place shape by shape — culling, or building its
/// own output vector — rather than materialising a second `Vec<ShapeOrText>`
/// first. See [`ShapeOrText::placed`] and [`ShapeOrText::placed_bounds`].
pub fn placement(rect: egui::Rect) -> TSTransform {
    TSTransform {
        scaling: rect.width() / ONLY_SUPPORTED_EXTENT as f32,
        translation: rect.min.to_vec2(),
    }
}

/// Transform shapes from MVT space to screen space.
pub fn transformed(shapes: &[ShapeOrText], rect: egui::Rect) -> Vec<ShapeOrText> {
    let transform = placement(rect);
    shapes.iter().map(|shape| shape.placed(transform)).collect()
}

fn get_layer_features(
    reader: &Reader,
    zoom: u8,
    name: &str,
    filter: Option<&Filter>,
) -> Result<impl Iterator<Item = (Geometry<f32>, Context)>, Error> {
    let features = if let Ok(layer_index) = find_layer(reader, name) {
        reader.get_features(layer_index)?
    } else {
        warn!("Source layer '{name}' not found. Skipping.");
        Vec::new()
    }
    .into_iter()
    .filter_map(move |feature| {
        // The property bag is *moved* into the context, not rebuilt in it.
        // Converting it to JSON up front cost a `HashMap` allocation and a
        // `String` clone per string-valued property for every feature the
        // source layer holds -- including the ones the filter is about to
        // reject, which read no property at all. `Properties::Mvt` converts a
        // value when a lookup asks for it instead.
        let context = Context::with_properties(
            geometry_type_to_str(&feature.geometry).to_string(),
            Properties::Mvt(feature.properties.unwrap_or_default()),
            zoom,
        );

        filter
            .is_none_or(|filter| filter.matches(&context))
            .then_some((feature.geometry, context))
    });

    Ok(features)
}

pub(crate) fn mvt_value_to_json_value(value: &Value) -> JsonValue {
    match value {
        Value::String(s) => JsonValue::String(s.clone()),
        Value::Int(x) | Value::SInt(x) => JsonValue::Number((*x).into()),
        Value::Double(x) => Number::from_f64(*x)
            .map(JsonValue::Number)
            .unwrap_or_else(|| {
                warn!("Invalid f64 value: {x}");
                JsonValue::Null
            }),
        Value::Bool(b) => JsonValue::Bool(*b),
        Value::Null => JsonValue::Null,
        _ => {
            warn!("Unsupported MVT value type: {value:?}");
            JsonValue::Null
        }
    }
}

fn geometry_type_to_str(geometry: &Geometry<f32>) -> &'static str {
    match geometry {
        Geometry::Point(_) | Geometry::MultiPoint(_) => "Point",
        Geometry::Line(_) => "Line",
        Geometry::LineString(_) | Geometry::MultiLineString(_) => "LineString",
        Geometry::Polygon(_) | Geometry::MultiPolygon(_) => "Polygon",
        Geometry::GeometryCollection(_) => "GeometryCollection",
        Geometry::Rect(_) => "Rect",
        Geometry::Triangle(_) => "Triangle",
    }
}

pub fn render_line(
    geometry: &Geometry<f32>,
    context: &Context,
    shapes: &mut Vec<ShapeOrText>,
    paint: &Paint,
) -> Result<(), Error> {
    let width = if let Some(width) = &paint.line_width {
        // **In screen points, and carried through placement untouched.** A
        // style's `line-width` is a screen quantity by MapLibre's definition;
        // the geometry beside it is in MVT extent units.
        // [`ShapeOrText::placed`] transforms positions only, so this number
        // arrives on screen as itself and no pre-multiplier is correct here.
        //
        // A pre-multiplier is what upstream had (`4.0`), and it can only ever
        // be right at one tile side: the placement scales by
        // `rect.width() / ONLY_SUPPORTED_EXTENT`, and `rect.width()` is not a
        // constant. squallar draws a tile 256 points across at a whole zoom and
        // zoom bias 0, 181 at the half step, 362 at the other half, and 128
        // when `MirrorPlan::tile_zoom_bias` asks a 3D pane's floor strip for one
        // level deeper. Measured against the committed styles, whose 56 `line`
        // layers all ask for width 8, the factor `4096/256 = 16` delivered
        // 8.00 / 5.66 / 11.31 / 4.00 at those four sides -- a road breathing
        // +-41% through a continuous zoom sweep and drawn quarter-width on
        // every floor strip.
        //
        // An ancestor stretched over a gap is the same rule and reads better
        // for it: its roads are the width the style asked for rather than
        // `2^(zoom - tile_zoom)` times it, so a loading tile is a coarse map
        // and not a slab.
        width.evaluate(context)
    } else {
        // Untouched, and inconsistent with the line above: the MapLibre default
        // for `line-width` is 1. It is left as upstream wrote it because no
        // layer of either committed style reaches it -- all 56 `line` layers in
        // `www/styles/{dark,light}.json` set `line-width` (counted 2026-08-28)
        // -- and changing a branch nothing exercises would be an unverifiable
        // edit inside a vendored file. Its *unit* moved with the line above:
        // this is now 2 screen points rather than 2 extent units.
        2.0
    };

    let opacity = if let Some(opacity) = &paint.line_opacity {
        opacity.evaluate(context)
    } else {
        1.0
    };

    let color = if let Some(color) = &paint.line_color {
        color.evaluate(context).gamma_multiply(opacity)
    } else {
        Color32::WHITE
    };

    match geometry {
        Geometry::LineString(line_string) => {
            let stroke = Stroke::new(width, color);
            let points = line_string
                .0
                .iter()
                .map(|p| pos2(p.x, p.y))
                .collect::<Vec<_>>();
            shapes.push(Shape::line(points, stroke).into());
        }
        Geometry::MultiLineString(multi_line_string) => {
            let stroke = Stroke::new(width, color);
            for line_string in multi_line_string {
                let points = line_string
                    .0
                    .iter()
                    .map(|p| pos2(p.x, p.y))
                    .collect::<Vec<_>>();
                shapes.push(Shape::line(points, stroke).into());
            }
        }
        _ => (),
    }
    Ok(())
}

fn render_polygon(
    geometry: &Geometry<f32>,
    context: &Context,
    shapes: &mut Vec<ShapeOrText>,
    paint: &Paint,
) -> Result<(), Error> {
    if let Geometry::MultiPolygon(multi_polygon) = geometry {
        let Some(fill_color) = &paint.fill_color else {
            warn!("Fill layer without fill color. Skipping.");
            return Ok(());
        };

        let fill_color = fill_color.evaluate(context);

        let fill_color = if let Some(fill_opacity) = &paint.fill_opacity {
            let fill_opacity = fill_opacity.evaluate(context);
            fill_color.gamma_multiply(fill_opacity)
        } else {
            fill_color
        };

        for polygon in multi_polygon.iter() {
            let exterior = lyon_points(&polygon.exterior().0);
            let interiors = polygon
                .interiors()
                .iter()
                .map(|hole| lyon_points(&hole.0))
                .collect::<Vec<_>>();
            shapes.push(tessellate_polygon(&exterior, &interiors, fill_color)?.into());
        }
    }
    Ok(())
}

fn render_symbol(
    geometry: &Geometry<f32>,
    context: &Context,
    shapes: &mut Vec<ShapeOrText>,
    layout: &Layout,
    paint: &Option<Paint>,
) -> Result<(), Error> {
    match geometry {
        Geometry::MultiPoint(multi_point) => {
            let text_size = layout
                .text_size
                .as_ref()
                .and_then(|text_size| {
                    let size = text_size.evaluate(context);

                    if size > 3.0 {
                        Some(size)
                    } else {
                        warn!(
                            "{} evaluated into {size}, which is too small for text size.",
                            text_size.0
                        );
                        None
                    }
                })
                .unwrap_or(12.0);

            let text_color = if let Some(paint) = paint
                && let Some(color) = &paint.text_color
            {
                color.evaluate(context)
            } else {
                // Default from MapLibre spec.
                Color32::BLACK
            };

            if let Some(text) = &layout.text(context) {
                shapes.extend(multi_point.0.iter().map(|p| {
                    ShapeOrText::Text(Text::new(
                        pos2(p.x(), p.y()),
                        text.clone(),
                        text_size,
                        text_color,
                        Color32::TRANSPARENT,
                        0.0,
                    ))
                }))
            }
        }
        Geometry::MultiLineString(multi_line_string) => {
            let text_size = layout
                .text_size
                .as_ref()
                .and_then(|text_size| {
                    let size = text_size.evaluate(context);

                    if size > 3.0 {
                        Some(size)
                    } else {
                        warn!(
                            "{} evaluated into {size}, which is too small for text size.",
                            text_size.0
                        );
                        None
                    }
                })
                .unwrap_or(12.0);

            let text_color = if let Some(paint) = paint
                && let Some(color) = &paint.text_color
            {
                color.evaluate(context)
            } else {
                Color32::BLACK
            };

            let text_halo_color = if let Some(paint) = paint
                && let Some(color) = &paint.text_halo_color
            {
                color.evaluate(context)
            } else {
                Color32::TRANSPARENT
            };

            for line_string in multi_line_string {
                let lines: Vec<_> = line_string.lines().collect();

                if let Some(text) = &layout.text(context)
                // Use the longest line to fit the label.
                && let Some(line) = lines.into_iter().max_by_key(|line| length(line) as u32)
                {
                    let mid_point = midpoint(&line.start_point(), &line.end_point());
                    let angle = line.slope().atan();

                    shapes.push(ShapeOrText::Text(Text::new(
                        pos2(mid_point.x(), mid_point.y()),
                        text.clone(),
                        text_size,
                        text_color,
                        // TODO: Implement real halo rendering.
                        text_halo_color.gamma_multiply(0.5),
                        angle,
                    )));
                }
            }
        }
        _ => (),
    }
    Ok(())
}

fn length(line: &Line<f32>) -> f32 {
    (line.dx() * line.dx() + line.dy() * line.dy()).sqrt()
}

fn midpoint(p1: &geo_types::Point<f32>, p2: &geo_types::Point<f32>) -> geo_types::Point<f32> {
    geo_types::Point::new((p1.x() + p2.x()) / 2.0, (p1.y() + p2.y()) / 2.0)
}

fn find_layer(data: &Reader, name: &str) -> Result<usize, Error> {
    let layer = data
        .get_layer_metadata()?
        .into_iter()
        .find(|layer| layer.name == name);

    let Some(layer) = layer else {
        return Err(Error::LayerNotFound(
            name.to_string(),
            data.get_layer_names()?,
        ));
    };

    if layer.extent != ONLY_SUPPORTED_EXTENT {
        return Err(Error::UnsupportedLayerExtent(name.to_string()));
    }

    Ok(layer.layer_index)
}

/// Egui cannot tessellate complex polygons, so we use lyon for that.
pub fn tessellate_polygon(
    exterior: &[Point<f32>],
    interiors: &[Vec<Point<f32>>],
    fill_color: Color32,
) -> Result<Mesh, TessellationError> {
    let mut builder = Path::builder();

    builder.add_polygon(Polygon {
        points: exterior,
        closed: true,
    });

    for interior in interiors {
        builder.add_polygon(Polygon {
            points: interior,
            closed: true,
        });
    }

    let mut buffers: VertexBuffers<Vertex, u32> = VertexBuffers::new();

    FillTessellator::new().tessellate_path(
        &builder.build(),
        &FillOptions::default(),
        &mut BuffersBuilder::new(&mut buffers, |vertex: FillVertex| {
            let pos = vertex.position();
            Vertex {
                pos: pos2(pos.x, pos.y),
                uv: WHITE_UV,
                color: fill_color,
            }
        }),
    )?;

    // **`VertexBuffers::new()` is `with_capacity(512, 1024)`**
    // (`lyon_tessellation-1.0.20/src/geometry_builder.rs`), and those two
    // allocations are 10,240 + 4,096 bytes whatever the polygon turns out to
    // need. A vector tile is *thousands* of small polygons, each one a mesh
    // that then lives in the tile cache for as long as the tile does, so the
    // slack is not transient: measured on the committed Monaco fixture's z14
    // tile, 2,257 meshes held 18,018 vertices and 40,812 indices inside
    // 1,155,584 vertex slots and 2,311,168 index slots -- 32.35 MB of capacity
    // for 0.52 MB of content, and the whole tile resident at 32.77 MB instead
    // of 0.77 MB.
    //
    // Sizing the buffers up front is not available: the tessellator's output
    // count is not a function of the input the caller has. Shrinking after is,
    // and it costs one reallocation and one copy of the content per polygon.
    buffers.vertices.shrink_to_fit();
    buffers.indices.shrink_to_fit();

    Ok(Mesh {
        indices: buffers.indices,
        vertices: buffers.vertices,
        ..Default::default()
    })
}

/// Convert list of `geo_types::Coord` to Lyon's `Point`s.
fn lyon_points(points: &[Coord<f32>]) -> Vec<Point<f32>> {
    points.iter().map(|p| point(p.x, p.y)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::style::Style;

    // ---------------------------------------------------------------------
    // A vector tile, encoded by hand.
    //
    // `mvt-reader` only decodes, its protobuf module is private, and no `.pbf`
    // is checked in anywhere in this workspace -- so the only way to drive
    // `render` end to end is to write the wire format here. Field numbers are
    // the vector tile spec's:
    //
    //   Tile    { layers = 3 }
    //   Layer   { name = 1, features = 2, keys = 3, values = 4, extent = 5,
    //             version = 15 }
    //   Feature { id = 1, tags = 2, type = 3, geometry = 4 }
    //   Value   { string = 1, float = 2, double = 3, int = 4, uint = 5,
    //             sint = 6, bool = 7 }
    // ---------------------------------------------------------------------

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

    /// Every `mvt_reader::feature::Value` arm, including the two that
    /// `mvt_value_to_json_value` does not support and the empty message that
    /// decodes as `Null`.
    enum Prop {
        Str(&'static str),
        Float(f32),
        Double(f64),
        Int(i64),
        UInt(u64),
        SInt(i64),
        Bool(bool),
        Null,
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
            // No field set at all is how the spec spells a null value.
            Prop::Null => (),
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

    const GEOM_POINT: u32 = 1;
    const GEOM_LINESTRING: u32 = 2;
    const GEOM_POLYGON: u32 = 3;

    struct FeatureSpec {
        geom_type: u32,
        properties: Vec<(&'static str, Prop)>,
        geometry: Vec<u32>,
    }

    fn encode_layer(name: &str, features: &[FeatureSpec]) -> Vec<u8> {
        let mut keys: Vec<&str> = Vec::new();
        let mut values: Vec<Vec<u8>> = Vec::new();
        let mut feature_blobs: Vec<Vec<u8>> = Vec::new();

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
            feature_blobs.push(blob);
        }

        let mut out = Vec::new();
        bytes_field(1, name.as_bytes(), &mut out);
        for blob in &feature_blobs {
            bytes_field(2, blob, &mut out);
        }
        for key in &keys {
            bytes_field(3, key.as_bytes(), &mut out);
        }
        for value in &values {
            bytes_field(4, value, &mut out);
        }
        varint_field(5, u64::from(ONLY_SUPPORTED_EXTENT), &mut out);
        varint_field(15, 2, &mut out);
        out
    }

    fn encode_tile(layers: &[(&str, Vec<FeatureSpec>)]) -> Vec<u8> {
        let mut out = Vec::new();
        for (name, features) in layers {
            bytes_field(3, &encode_layer(name, features), &mut out);
        }
        out
    }

    /// One tile, three source layers, seven features between them.
    ///
    /// `roads/r1` carries both `kind = "primary"` and `primary = "yes"`. That
    /// pair is the load-bearing part of the fixture: a filter of
    /// `["==", "kind", "primary"]` matches only if the right operand stays the
    /// literal it is. Resolve it as a property and it becomes `"yes"`, the
    /// filter stops matching, and `r1`'s stroke disappears from the output.
    fn fixture() -> Vec<u8> {
        encode_tile(&[
            (
                "roads",
                vec![
                    // r1: one LineString.
                    FeatureSpec {
                        geom_type: GEOM_LINESTRING,
                        properties: vec![
                            ("kind", Prop::Str("primary")),
                            ("primary", Prop::Str("yes")),
                            ("width", Prop::Int(4)),
                            ("name", Prop::Str("Main Street")),
                        ],
                        geometry: vec![
                            command(1, 1),
                            param(10),
                            param(20),
                            command(2, 2),
                            param(90),
                            param(0),
                            param(0),
                            param(80),
                        ],
                    },
                    // r2: two MoveTo runs, so a MultiLineString.
                    FeatureSpec {
                        geom_type: GEOM_LINESTRING,
                        properties: vec![
                            ("kind", Prop::Str("driveway")),
                            ("width", Prop::Int(2)),
                            ("elevation", Prop::SInt(-2)),
                            ("lit", Prop::Bool(true)),
                            ("name", Prop::Str("Back Alley")),
                        ],
                        geometry: vec![
                            command(1, 1),
                            param(10),
                            param(10),
                            command(2, 1),
                            param(50),
                            param(0),
                            command(1, 1),
                            param(0),
                            param(50),
                            command(2, 1),
                            param(-50),
                            param(0),
                        ],
                    },
                    // r3: no properties at all.
                    FeatureSpec {
                        geom_type: GEOM_LINESTRING,
                        properties: vec![],
                        geometry: vec![
                            command(1, 1),
                            param(200),
                            param(200),
                            command(2, 1),
                            param(100),
                            param(100),
                        ],
                    },
                ],
            ),
            (
                "landuse",
                vec![
                    FeatureSpec {
                        geom_type: GEOM_POLYGON,
                        properties: vec![
                            ("class", Prop::Str("park")),
                            ("opacity", Prop::Double(0.5)),
                        ],
                        geometry: vec![
                            command(1, 1),
                            param(0),
                            param(0),
                            command(2, 3),
                            param(100),
                            param(0),
                            param(0),
                            param(100),
                            param(-100),
                            param(0),
                            command(7, 1),
                        ],
                    },
                    FeatureSpec {
                        geom_type: GEOM_POLYGON,
                        properties: vec![
                            ("class", Prop::Str("forest")),
                            // Neither `Float` nor `UInt` is a supported MVT
                            // value here; both must arrive as JSON `null`.
                            ("ratio", Prop::Float(0.25)),
                            ("count", Prop::UInt(7)),
                            ("note", Prop::Null),
                        ],
                        geometry: vec![
                            command(1, 1),
                            param(200),
                            param(200),
                            command(2, 3),
                            param(150),
                            param(0),
                            param(0),
                            param(150),
                            param(-150),
                            param(0),
                            command(7, 1),
                        ],
                    },
                ],
            ),
            (
                "places",
                vec![
                    FeatureSpec {
                        geom_type: GEOM_POINT,
                        properties: vec![
                            ("name", Prop::Str("Warsaw")),
                            ("capital", Prop::Bool(true)),
                            ("rank", Prop::Int(1)),
                        ],
                        geometry: vec![
                            command(1, 2),
                            param(500),
                            param(600),
                            param(30),
                            param(-40),
                        ],
                    },
                    FeatureSpec {
                        geom_type: GEOM_POINT,
                        properties: vec![("name", Prop::Str("Krakow")), ("rank", Prop::Int(2))],
                        geometry: vec![command(1, 1), param(1200), param(1300)],
                    },
                ],
            ),
        ])
    }

    /// Reads every property arm the fixture carries, through both operand
    /// resolvers, in filters and in paint and layout expressions.
    const STYLE: &str = r##"{
      "layers": [
        { "type": "background",
          "paint": { "background-color": "#102030" } },

        { "type": "fill", "source-layer": "landuse",
          "filter": ["==", "class", "park"],
          "paint": { "fill-color": "#00ff00", "fill-opacity": ["get", "opacity"] } },

        { "type": "fill", "source-layer": "landuse",
          "filter": ["all", ["!in", "class", "park"], ["has", "note"], ["==", "count", null]],
          "paint": { "fill-color": "#008000" } },

        { "type": "line", "source-layer": "roads",
          "filter": ["==", "kind", "primary"],
          "paint": { "line-color": "#ff0000", "line-width": ["get", "width"], "line-opacity": 0.8 } },

        { "type": "line", "source-layer": "roads",
          "filter": ["!has", "kind"],
          "paint": { "line-color": "#0000ff" } },

        { "type": "line", "source-layer": "roads",
          "filter": ["all", ["has", "lit"], ["<", "elevation", 0], ["==", "$type", "LineString"]],
          "paint": { "line-color": "#ffff00", "line-width": ["get", "width"] } },

        { "type": "symbol", "source-layer": "places",
          "filter": ["has", "name"],
          "layout": { "text-field": ["get", "name"],
                      "text-size": ["interpolate", ["linear"], ["zoom"], 0, 8, 10, 16] },
          "paint": { "text-color": "#ffffff" } },

        { "type": "symbol", "source-layer": "roads",
          "filter": ["has", "name"],
          "layout": { "text-field": ["get", "name"], "text-size": 14 },
          "paint": { "text-color": "#cccccc", "text-halo-color": "#000000" } }
      ]
    }"##;

    const ZOOM: u8 = 5;

    fn rendered() -> Vec<ShapeOrText> {
        let style = Style::from_json(STYLE).expect("fixture style parses");
        render(&fixture(), &style, ZOOM).expect("fixture tile renders")
    }

    /// What `render` produced for [`fixture`] before the property bag stopped
    /// being rebuilt per feature, recorded verbatim.
    ///
    /// `ShapeOrText` is not `PartialEq` -- `Text` does not derive it -- so this
    /// compares the `Debug` rendering, which for `f32` is the shortest string
    /// that round-trips and so is exact. Every field of every mesh vertex,
    /// stroke and label is in here. This is a pin on the *values expressions
    /// evaluate to*, not on tessellation: if a lookup starts returning
    /// something other than what the eager conversion returned, a colour, a
    /// width or a label moves and this goes red.
    const GOLDEN: &str = r##"[Shape(Rect(RectShape { rect: [[0.0 0.0] - [4096.0 4096.0]], corner_radius: CornerRadius { nw: 0, ne: 0, sw: 0, se: 0 }, fill: #10_20_30_FF, stroke: Stroke { width: 0.0, color: #00_00_00_00 }, stroke_kind: Outside, round_to_pixels: None, blur_width: 0.0, brush: None, angle: 0.0 })), Shape(Mesh(Mesh { indices: [1, 0, 2, 1, 2, 3, 5, 4, 6, 5, 6, 7], vertices: [Vertex { pos: [0.0 0.0], uv: [0.0 0.0], color: #00_80_00_80 }, Vertex { pos: [100.0 0.0], uv: [0.0 0.0], color: #00_80_00_80 }, Vertex { pos: [0.0 100.0], uv: [0.0 0.0], color: #00_80_00_80 }, Vertex { pos: [100.0 100.0], uv: [0.0 0.0], color: #00_80_00_80 }, Vertex { pos: [200.0 200.0], uv: [0.0 0.0], color: #00_80_00_FF }, Vertex { pos: [350.0 200.0], uv: [0.0 0.0], color: #00_80_00_FF }, Vertex { pos: [200.0 350.0], uv: [0.0 0.0], color: #00_80_00_FF }, Vertex { pos: [350.0 350.0], uv: [0.0 0.0], color: #00_80_00_FF }], texture_id: Managed(0) })), Shape(Path(PathShape { points: [[10.0 20.0], [100.0 20.0], [100.0 100.0]], closed: false, fill: #00_00_00_00, stroke: PathStroke { width: 4.0, color: Solid(#CC_00_00_CC), kind: Middle } })), Shape(Path(PathShape { points: [[200.0 200.0], [300.0 300.0]], closed: false, fill: #00_00_00_00, stroke: PathStroke { width: 2.0, color: Solid(#00_00_FF_FF), kind: Middle } })), Shape(Path(PathShape { points: [[10.0 10.0], [60.0 10.0]], closed: false, fill: #00_00_00_00, stroke: PathStroke { width: 2.0, color: Solid(#FF_FF_00_FF), kind: Middle } })), Shape(Path(PathShape { points: [[60.0 60.0], [10.0 60.0]], closed: false, fill: #00_00_00_00, stroke: PathStroke { width: 2.0, color: Solid(#FF_FF_00_FF), kind: Middle } })), Text(Text { text: "Warsaw", position: [500.0 600.0], font_size: 12.0, text_color: #FF_FF_FF_FF, background_color: #00_00_00_00, angle: 0.0 }), Text(Text { text: "Warsaw", position: [530.0 560.0], font_size: 12.0, text_color: #FF_FF_FF_FF, background_color: #00_00_00_00, angle: 0.0 }), Text(Text { text: "Krakow", position: [1200.0 1300.0], font_size: 12.0, text_color: #FF_FF_FF_FF, background_color: #00_00_00_00, angle: 0.0 }), Text(Text { text: "Back Alley", position: [35.0 10.0], font_size: 14.0, text_color: #CC_CC_CC_FF, background_color: #00_00_00_80, angle: 0.0 }), Text(Text { text: "Back Alley", position: [35.0 60.0], font_size: 14.0, text_color: #CC_CC_CC_FF, background_color: #00_00_00_80, angle: -0.0 })]"##;

    #[test]
    fn rendering_the_fixture_reproduces_the_recorded_shapes_exactly() {
        let shapes = rendered();

        // A non-triviality floor: an empty or short render must not be able to
        // pass by matching a golden nobody looked at.
        assert_eq!(
            shapes.len(),
            11,
            "the fixture draws a background, one coalesced fill mesh, four \
             strokes and five labels"
        );

        assert_eq!(format!("{shapes:?}"), GOLDEN);
    }

    /// **The stroke width the style asked for arrives, at every tile side.**
    ///
    /// The blind spot this closes: every other test here places on a
    /// 256-point rect or does not place at all, and 256 is exactly the side at
    /// which the old `LINE_WIDTH_TO_EXTENT = 4096/256` pre-multiplier was
    /// right. A consumer drawing a tile at any other side -- 181 at the half
    /// step, 362 at the other, 128 when a zoom bias asks for one level deeper,
    /// or `256 * 2^n` for an ancestor stretched over a gap -- got the styled
    /// width times `rect.width() / 256`.
    #[test]
    fn a_styled_line_width_survives_every_tile_side() {
        let shapes = rendered();

        let widths = |rect: egui::Rect| -> Vec<f32> {
            transformed(&shapes, rect)
                .into_iter()
                .filter_map(|shape| match shape {
                    ShapeOrText::Shape(Shape::Path(path)) => Some(path.stroke.width),
                    _ => None,
                })
                .collect()
        };

        let at_256 = widths(egui::Rect::from_min_size(
            egui::pos2(0.0, 0.0),
            egui::vec2(256.0, 256.0),
        ));

        // The floor: a run over an empty shape list would agree with itself at
        // every side and prove nothing.
        assert_eq!(
            at_256,
            vec![4.0, 2.0, 2.0, 2.0],
            "the fixture's four strokes are the styled widths, in screen points"
        );

        for side in [1.0f32, 128.0, 181.019_34, 362.038_67, 4096.0, 65536.0] {
            let rect = egui::Rect::from_min_size(egui::pos2(7.0, 11.0), egui::vec2(side, side));
            assert_eq!(
                widths(rect),
                at_256,
                "a tile drawn {side} points across delivered different stroke \
                 widths than one drawn 256 across; `line-width` is a screen \
                 quantity and must not scale with the placement"
            );

            // The geometry, unlike the width, *must* scale -- otherwise this
            // would pass by transforming nothing at all.
            let placed = transformed(&shapes, rect);
            // [0] background, [1] the coalesced fill mesh, [2] the first stroke.
            let ShapeOrText::Shape(Shape::Path(path)) = &placed[2] else {
                panic!("the third shape is the first stroke");
            };
            let expected = 7.0 + 10.0 * side / ONLY_SUPPORTED_EXTENT as f32;
            assert!(
                (path.points[0].x - expected).abs() < 1e-3,
                "at side {side} the first stroke point landed at {} rather \
                 than {expected}: the placement did not scale the geometry",
                path.points[0].x
            );
        }
    }

    fn roads_line_style(filter: &str) -> String {
        format!(
            r##"{{ "layers": [ {{ "type": "line", "source-layer": "roads",
                   "filter": {filter},
                   "paint": {{ "line-color": "#ff0000" }} }} ] }}"##
        )
    }

    fn roads_lines(filter: &str) -> usize {
        let style = Style::from_json(&roads_line_style(filter)).expect("style parses");
        render(&fixture(), &style, ZOOM)
            .expect("fixture tile renders")
            .len()
    }

    /// The two-resolver split, pinned through `render` rather than through
    /// `Context` directly.
    ///
    /// `roads/r1` has `kind = "primary"` *and* `primary = "yes"`. The left
    /// operand of a comparison names a property; the right one is the value
    /// compared against and stays the literal it is. Resolve the right side as
    /// a property too and both assertions below invert.
    #[test]
    fn the_right_operand_of_a_comparison_stays_a_literal() {
        assert_eq!(
            roads_lines(r#"["==", "kind", "primary"]"#),
            1,
            "'primary' on the right is the literal, and it equals r1's kind"
        );
        assert_eq!(
            roads_lines(r#"["==", "kind", "yes"]"#),
            0,
            "'yes' is r1's *primary* property, and must not be reachable from the right"
        );
    }
}
