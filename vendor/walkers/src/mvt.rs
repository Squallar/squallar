//! Renderer for Mapbox Vector Tiles.
//!
//! The pipeline is two halves with a cacheable value between them:
//! [`parse`] decodes the MVT bytes into a [`ParsedTile`] — zoom- and
//! style-independent — and [`styled`] evaluates a [`Style`] over it at a zoom,
//! producing the [`ShapeOrText`]s a tile draws as. [`render`] is the two run
//! back to back. A consumer that keeps the `ParsedTile` (in an `Arc`; both
//! halves of it are shared, not copied) can re-style a tile on a theme flip or
//! a layer toggle without touching the bytes again.

use std::collections::HashMap;
use std::sync::Arc;

use egui::{
    Color32, Mesh, Rect, Shape, Stroke,
    emath::TSTransform,
    epaint::{Vertex, WHITE_UV},
    pos2, vec2,
};
pub use geo_types::{Coord, Geometry, Line};
use log::{trace, warn};
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

/// The zoom- and style-independent decode of one MVT tile: every source
/// layer's features, geometry in extent units, property bags as they arrive
/// from `mvt-reader`.
///
/// This is the value worth caching. [`styled`] evaluates any [`Style`] at any
/// zoom over it without re-reading the bytes, and the per-feature property
/// bags are `Arc`-shared into every [`Context`] built over them, so styling a
/// parsed tile allocates contexts and shapes but never a second copy of the
/// decode.
pub struct ParsedTile {
    layers: Vec<ParsedLayer>,
}

struct ParsedLayer {
    name: String,
    /// Whether the layer's extent is [`ONLY_SUPPORTED_EXTENT`]. A layer at any
    /// other extent decodes no features and is skipped by [`styled`], which is
    /// what the reader-backed path did: `find_layer` refused it and the
    /// caller's `if let Ok` fell through to the same trace as a missing layer.
    extent_supported: bool,
    features: Vec<ParsedFeature>,
}

struct ParsedFeature {
    geometry: Geometry<f32>,
    /// Shared, not owned: one bag is read by every style layer that visits the
    /// source layer, across every styling of this tile.
    properties: Arc<HashMap<String, Value>>,
}

impl ParsedTile {
    /// The heap this tile holds, counted at **capacity**, because capacity is
    /// what is resident while the tile is cached.
    ///
    /// Exact on the `Vec`, `String` and geometry terms. The `HashMap` term is
    /// an estimate from below: hashbrown keeps one `(K, V)` slot and one
    /// control byte per bucket, and this counts `capacity()` of each — the
    /// usable seven-eighths of the buckets — because the bucket count itself
    /// is not observable. Consumers sizing a cache against this should treat
    /// it the way `MEASURED_VECTOR_TILE_BYTES`' derivation treats its figure:
    /// re-measure by forcing the deriving test to fail, never infer.
    pub fn heap_bytes(&self) -> usize {
        self.layers.capacity() * std::mem::size_of::<ParsedLayer>()
            + self
                .layers
                .iter()
                .map(|layer| {
                    layer.name.capacity()
                        + layer.features.capacity() * std::mem::size_of::<ParsedFeature>()
                        + layer
                            .features
                            .iter()
                            .map(|feature| {
                                geometry_heap_bytes(&feature.geometry)
                                    // The Arc allocation: two refcounts and the
                                    // map header itself.
                                    + 2 * std::mem::size_of::<usize>()
                                    + std::mem::size_of::<HashMap<String, Value>>()
                                    + properties_heap_bytes(&feature.properties)
                            })
                            .sum::<usize>()
                })
                .sum::<usize>()
    }
}

/// The heap behind one property bag: the table, the key strings, and the
/// string values — the only `mvt_reader::feature::Value` arm that owns heap.
fn properties_heap_bytes(properties: &HashMap<String, Value>) -> usize {
    properties.capacity() * (std::mem::size_of::<(String, Value)>() + 1)
        + properties
            .iter()
            .map(|(key, value)| {
                key.capacity()
                    + match value {
                        Value::String(string) => string.capacity(),
                        _ => 0,
                    }
            })
            .sum::<usize>()
}

/// The heap behind one geometry, at capacity. A `Coord<f32>` is inline; every
/// heap byte is a `Vec` somewhere in the variant.
fn geometry_heap_bytes(geometry: &Geometry<f32>) -> usize {
    const COORD: usize = std::mem::size_of::<Coord<f32>>();

    fn line_string(ls: &geo_types::LineString<f32>) -> usize {
        ls.0.capacity() * COORD
    }

    fn polygon(polygon: &geo_types::Polygon<f32>) -> usize {
        line_string(polygon.exterior())
            + polygon
                .interiors()
                .iter()
                .map(|interior| {
                    std::mem::size_of::<geo_types::LineString<f32>>() + line_string(interior)
                })
                .sum::<usize>()
    }

    match geometry {
        Geometry::Point(_) | Geometry::Line(_) | Geometry::Rect(_) | Geometry::Triangle(_) => 0,
        Geometry::MultiPoint(points) => points.0.capacity() * COORD,
        Geometry::LineString(ls) => line_string(ls),
        Geometry::MultiLineString(mls) => {
            mls.0.capacity() * std::mem::size_of::<geo_types::LineString<f32>>()
                + mls.0.iter().map(line_string).sum::<usize>()
        }
        Geometry::Polygon(p) => polygon(p),
        Geometry::MultiPolygon(mp) => {
            mp.0.capacity() * std::mem::size_of::<geo_types::Polygon<f32>>()
                + mp.0.iter().map(polygon).sum::<usize>()
        }
        Geometry::GeometryCollection(gc) => {
            gc.0.capacity() * std::mem::size_of::<Geometry<f32>>()
                + gc.0.iter().map(geometry_heap_bytes).sum::<usize>()
        }
    }
}

/// Give back the decode's slack, ring by ring.
///
/// `mvt-reader` opens a feature's first ring with
/// `Vec::with_capacity(geometry_data.len())` — the **whole feature's**
/// command-integer count. Measured on the committed Monaco fixture's z14
/// city-core tile, 2026-08-29: 28,128,896 bytes of geometry capacity where the
/// shrunk content is 1,113,224 — a 25× slack. A parsed tile is cached, not
/// transient, so one shrink pass per feature is the same trade
/// [`tessellate_polygon`] makes for the same reason.
///
/// **That reservation used to be repeated at full size for every ring, and
/// this pass could never have fixed it.** Every ring of a feature is alive at
/// once inside `parse_geometry`, so the *peak* was `rings × commands` while
/// this runs afterwards, on what survived. On wasm32 the peak is an infallible
/// allocation against a 1 GiB module ceiling: it aborted the web build on an
/// ordinary low-zoom tile, and nothing unwinds through a wasm trap. Fixed in
/// `vendor/mvt-reader` on 2026-08-31 — see its `VENDORED.md`. The 25× figure
/// above is unaffected and still measured on the same fixture: it is about the
/// slack that survives the decode, which is what this function is for.
///
/// The `interiors` and `MultiLineString` outer vectors grow by ordinary
/// doubling (at most 2×) and `interiors_mut` exposes only a slice, so the
/// rings' coordinate vectors — where the measured slack lives — are what is
/// shrunk.
fn shrink_geometry(geometry: &mut Geometry<f32>) {
    fn shrink_ls(ls: &mut geo_types::LineString<f32>) {
        ls.0.shrink_to_fit();
    }
    fn shrink_poly(polygon: &mut geo_types::Polygon<f32>) {
        polygon.exterior_mut(|exterior| exterior.0.shrink_to_fit());
        polygon.interiors_mut(|interiors| {
            for interior in interiors {
                shrink_ls(interior);
            }
        });
    }
    match geometry {
        Geometry::Point(_) | Geometry::Line(_) | Geometry::Rect(_) | Geometry::Triangle(_) => {}
        Geometry::MultiPoint(points) => points.0.shrink_to_fit(),
        Geometry::LineString(ls) => shrink_ls(ls),
        Geometry::MultiLineString(mls) => {
            mls.0.shrink_to_fit();
            for ls in &mut mls.0 {
                shrink_ls(ls);
            }
        }
        Geometry::Polygon(polygon) => shrink_poly(polygon),
        Geometry::MultiPolygon(mp) => {
            mp.0.shrink_to_fit();
            for polygon in &mut mp.0 {
                shrink_poly(polygon);
            }
        }
        Geometry::GeometryCollection(gc) => {
            gc.0.shrink_to_fit();
            for geometry in &mut gc.0 {
                shrink_geometry(geometry);
            }
        }
    }
}

/// Decode MVT bytes into a [`ParsedTile`].
///
/// Every layer the tile carries is decoded, whether or not any style will
/// visit it — the parse cannot know the styles it will serve. The one
/// behavioural consequence against the fused path: a layer whose features will
/// not decode fails the whole parse even if no style layer names it, where
/// `render` only reached it on a style's request. A tile like that is a broken
/// tile; failing it loudly is the honest arm.
pub fn parse(data: &[u8]) -> Result<ParsedTile, Error> {
    let reader = Reader::new(data.to_vec())?;
    let metadata = reader.get_layer_metadata()?;

    let mut layers = Vec::with_capacity(metadata.len());
    for layer in metadata {
        let extent_supported = layer.extent == ONLY_SUPPORTED_EXTENT;
        let features = if extent_supported {
            reader
                .get_features(layer.layer_index)?
                .into_iter()
                .map(|mut feature| {
                    shrink_geometry(&mut feature.geometry);
                    ParsedFeature {
                        geometry: feature.geometry,
                        properties: Arc::new(feature.properties.unwrap_or_default()),
                    }
                })
                .collect()
        } else {
            Vec::new()
        };
        layers.push(ParsedLayer {
            name: layer.name,
            extent_supported,
            features,
        });
    }

    Ok(ParsedTile { layers })
}

/// Render MVT data into a list of [`epaint::Shape`]s: [`parse`] then
/// [`styled`], for a caller with nothing to keep between them.
pub fn render(data: &[u8], style: &Style, zoom: u8) -> Result<Vec<ShapeOrText>, Error> {
    Ok(styled(&parse(data)?, style, zoom))
}

/// Evaluate `style` at `zoom` over a parsed tile.
///
/// The style half of [`render`]: filter evaluation, paint and layout
/// expressions, and tessellation. Infallible where `render` is not, because
/// everything that can fail — the byte decode — happened in [`parse`];
/// a polygon the tessellator rejects is logged and skipped, exactly as the
/// fused path logged and skipped it.
pub fn styled(tile: &ParsedTile, style: &Style, zoom: u8) -> Vec<ShapeOrText> {
    let mut shapes = Vec::new();

    for layer in &style.layers {
        // **The zoom gate, before anything reads a feature.** A style layer
        // declares the zooms it draws at; outside them it can produce no shape,
        // so visiting its source layer is pure waste. Skipping here rather than
        // inside the arms is what makes it a skip of the *scan* and not just of
        // the tessellation: measured on the committed dark style over Monaco's
        // z14 tile, the walk made 36,921 feature scans at every zoom from 0 to
        // 16 -- the same number at zoom 0, where 14 of the 95 layers are live,
        // as at zoom 16, where 78 are.
        if !layer.visible_at(zoom) {
            continue;
        }

        match layer {
            Layer::Background { paint, .. } => {
                let context = Context::new("None", HashMap::new(), zoom);

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
                ..
            } => {
                for (geometry, context) in layer_features(tile, zoom, source_layer, filter.as_ref())
                {
                    if let Err(err) = render_polygon(geometry, &context, &mut shapes, paint) {
                        warn!("{err}");
                    }
                }
            }
            Layer::Line {
                source_layer,
                filter,
                paint,
                ..
            } => {
                for (geometry, context) in layer_features(tile, zoom, source_layer, filter.as_ref())
                {
                    if let Err(err) = render_line(geometry, &context, &mut shapes, paint) {
                        warn!("{err}");
                    }
                }
            }
            Layer::Symbol {
                source_layer,
                filter,
                layout,
                paint,
                ..
            } => {
                for (geometry, context) in layer_features(tile, zoom, source_layer, filter.as_ref())
                {
                    if let Err(err) = render_symbol(geometry, &context, &mut shapes, layout, paint)
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
    shapes
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

/// Feature scans, counted for the tests that gate the shape of the style walk.
///
/// One **scan** is one feature *considered* by one style layer: a [`Context`]
/// built over it and, if the layer has one, a filter evaluated against it. It
/// is the unit the walk spends its style time in, and the unit both the zoom
/// gate and the source-layer grouping exist to reduce, so it is what those
/// gates assert on rather than a wall clock.
///
/// Thread-local, so that a filtered `cargo test` run measuring one walk is not
/// perturbed by another test rendering on a sibling thread.
#[cfg(test)]
pub(crate) mod scans {
    use std::cell::Cell;

    thread_local! {
        static SCANS: Cell<usize> = const { Cell::new(0) };
    }

    pub(crate) fn bump() {
        SCANS.with(|scans| scans.set(scans.get() + 1));
    }

    /// Run `body` and report what it returned alongside the scans it made.
    pub(crate) fn counted<T>(body: impl FnOnce() -> T) -> (T, usize) {
        SCANS.with(|scans| scans.set(0));
        let value = body();
        (value, SCANS.with(Cell::get))
    }
}

fn layer_features<'a>(
    tile: &'a ParsedTile,
    zoom: u8,
    name: &str,
    filter: Option<&'a Filter>,
) -> impl Iterator<Item = (&'a Geometry<f32>, Context)> + 'a {
    let features = match tile.layers.iter().find(|layer| layer.name == name) {
        Some(layer) if layer.extent_supported => layer.features.as_slice(),
        _ => {
            // **`trace!`, not `warn!`, because a tile without a source layer is
            // ordinary data.** A style names 94 source layers and no tile carries
            // all of them -- open country has no `building`, most tiles have no
            // `poi` -- so at `warn` this fired once per style layer per tile,
            // constantly, for nothing the reader could act on. Measured over a
            // 45-tile Oklahoma viewport at zooms 6/7/8: 903 warn lines, **every
            // single warn line the renderer emitted**, `transportation` alone 245.
            //
            // The case that really is broken -- a style naming a source layer no
            // tile anywhere carries -- is caught before it ships, by
            // `squallar-egui/tests/committed_styles_parse.rs`'s check that every
            // `source-layer` is one of the sixteen OpenMapTiles names. That test
            // fails the build; this line could only ever have whispered about it
            // underneath thousands of false ones.
            //
            // Kept rather than deleted because it is genuinely the answer to "why
            // is this one layer not drawing on this one tile", which is a real
            // question with no other instrument. At `trace` it costs nothing until
            // somebody asks.
            //
            // An unsupported extent lands here too, exactly as it did when
            // `find_layer` refused it into the same fallback.
            trace!("Source layer '{name}' not found. Skipping.");
            &[]
        }
    };

    features.iter().filter_map(move |feature| {
        #[cfg(test)]
        scans::bump();

        // The property bag is *shared* into the context, not rebuilt in it.
        // Converting it to JSON up front cost a `HashMap` allocation and a
        // `String` clone per string-valued property for every feature the
        // source layer holds -- including the ones the filter is about to
        // reject, which read no property at all. `Properties::Mvt` converts a
        // value when a lookup asks for it instead, and the `Arc` is what lets
        // one parse serve every styling without copying a bag.
        let context = Context::with_properties(
            geometry_type_to_str(&feature.geometry),
            Properties::Mvt(Arc::clone(&feature.properties)),
            zoom,
        );

        filter
            .is_none_or(|filter| filter.matches(&context))
            .then_some((&feature.geometry, context))
    })
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

/// A symbol layer's text size, in screen points.
///
/// Hoisted out of the two `render_symbol` arms, which carried it letter for
/// letter twice.
fn symbol_text_size(context: &Context, layout: &Layout) -> f32 {
    layout
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
        .unwrap_or(12.0)
}

/// A symbol layer's glyph colour.
///
/// `text-halo-color` and `text-halo-width` are deliberately not read here.
/// Nothing draws a halo -- see [`crate::text::Text::shape`] for why -- and
/// carrying them onto every [`Text`] so that no consumer could use them was
/// paying for the field on every label to no effect.
fn symbol_text_color(context: &Context, paint: &Option<Paint>) -> Color32 {
    // Default from the MapLibre spec.
    paint
        .as_ref()
        .and_then(|paint| paint.text_color.as_ref())
        .map_or(Color32::BLACK, |color| color.evaluate(context))
}

/// A symbol layer's wrapping, in ems: `(text-max-width, text-line-height)`.
///
/// `text-max-width` defaults to **10**, which is MapLibre's default and is
/// applied here rather than left to the text layer, because "the style said
/// nothing" and "the style said do not wrap" are different instructions and
/// only the second one should produce an unwrapped run. Every symbol layer in
/// the committed styles that carries a long name sets it explicitly anyway; the
/// default is what protects the ones that do not.
fn symbol_wrapping(context: &Context, layout: &Layout) -> (Option<f32>, Option<f32>) {
    const DEFAULT_TEXT_MAX_WIDTH_EMS: f32 = 10.0;

    let max_width = layout
        .text_max_width
        .as_ref()
        .map_or(DEFAULT_TEXT_MAX_WIDTH_EMS, |value| value.evaluate(context));

    // A non-positive width is not a wrap instruction, and dividing a label into
    // one word per row is worse than not wrapping it.
    let max_width = (max_width > 0.0).then_some(max_width);

    let line_height = layout
        .text_line_height
        .as_ref()
        .map(|value| value.evaluate(context))
        .filter(|height| *height > 0.0);

    (max_width, line_height)
}

fn render_symbol(
    geometry: &Geometry<f32>,
    context: &Context,
    shapes: &mut Vec<ShapeOrText>,
    layout: &Layout,
    paint: &Option<Paint>,
) -> Result<(), Error> {
    // Read once per feature rather than once per arm. `layout.text` in
    // particular evaluates an expression and allocates a `String`.
    let Some(text) = layout.text(context) else {
        return Ok(());
    };

    let text_size = symbol_text_size(context, layout);
    let text_color = symbol_text_color(context, paint);
    let (max_width_ems, line_height_ems) = symbol_wrapping(context, layout);

    let label = |position, angle, wrap: bool| {
        ShapeOrText::Text(
            Text::new(position, text.clone(), text_size, text_color, angle)
                .with_wrapping(wrap.then_some(max_width_ems).flatten(), line_height_ems),
        )
    };

    match geometry {
        // Point placement wraps.
        Geometry::MultiPoint(multi_point) => {
            shapes.extend(
                multi_point
                    .0
                    .iter()
                    .map(|p| label(pos2(p.x(), p.y()), 0.0, true)),
            );
        }
        // **Line placement does not wrap, and that is MapLibre's rule rather
        // than a simplification.** A label following a watercourse is laid out
        // along the line, so there is no column to break into; MapLibre spells
        // this as `placement === 'point' ? text-max-width * ONE_EM : 0` when it
        // shapes a symbol. Wrapping here would stack "North Canadian River"
        // into three short rows sitting across the river rather than along it,
        // which is worse than the run it replaced.
        Geometry::MultiLineString(multi_line_string) => {
            for line_string in multi_line_string {
                if let Some((position, angle)) = anchor_along(line_string) {
                    shapes.push(label(position, angle, false));
                }
            }
        }
        _ => (),
    }
    Ok(())
}

/// Where a line label goes on `line_string`, and which way it points.
///
/// **A point on the line and the local tangent, replacing the chord midpoint
/// and the chord slope.** The old spelling took the single longest *segment* of
/// the geometry and used the midpoint of its two endpoints with
/// `slope().atan()`. Both halves are wrong on a watercourse: the anchor is the
/// midpoint of a straight line between two points on a meander, which is off
/// the water, and the angle is that same straight line's bearing, which on
/// adjacent OSM fragments of one river differs by tens of degrees even where
/// the river itself is smooth. That is what put "Rio Grande" on screen six
/// times at six different rotations.
///
/// The anchor is the point half way along by **arc length**, so it is on the
/// geometry by construction. The angle is measured over a short window centred
/// on it -- a local chord rather than the whole fragment's -- which follows a
/// curve while staying immune to a single tiny segment's noise. `atan2` is
/// folded back into a half turn so the text never draws upside down, which is
/// what `slope().atan()` did for free and what `text-keep-upright: true` in the
/// committed styles asks for.
fn anchor_along(line_string: &geo_types::LineString<f32>) -> Option<(egui::Pos2, f32)> {
    /// The tangent window, as a fraction of the fragment's length either side
    /// of the anchor. Small enough to track a bend, wide enough that a
    /// millimetre-long segment does not set the angle by itself.
    const TANGENT_WINDOW: f32 = 0.05;

    let lines: Vec<Line<f32>> = line_string.lines().collect();
    let total: f32 = lines.iter().map(length).sum();

    if lines.is_empty() || !total.is_finite() || total <= 0.0 {
        return None;
    }

    let middle = total / 2.0;
    let window = total * TANGENT_WINDOW;

    let position = point_at(&lines, middle)?;
    let before = point_at(&lines, middle - window)?;
    let after = point_at(&lines, middle + window)?;

    Some((position, upright_angle(after - before)))
}

/// The point `distance` along the polyline `lines`, clamped to its ends.
///
/// Clamping rather than returning `None` past the end is what lets
/// [`anchor_along`] ask for the anchor plus and minus a window without checking
/// first: on a fragment shorter than the window the two probes collapse towards
/// the endpoints, which still gives the fragment's own direction.
fn point_at(lines: &[Line<f32>], distance: f32) -> Option<egui::Pos2> {
    let last = lines.len().checked_sub(1)?;
    let mut remaining = distance.max(0.0);

    for (index, line) in lines.iter().enumerate() {
        let len = length(line);

        if remaining <= len || index == last {
            // `len` is zero for a repeated coordinate, and then the fraction
            // along it is meaningless -- both endpoints are the same point.
            let t = if len > 0.0 {
                (remaining / len).clamp(0.0, 1.0)
            } else {
                0.0
            };
            return Some(pos2(
                line.start.x + line.dx() * t,
                line.start.y + line.dy() * t,
            ));
        }

        remaining -= len;
    }

    None
}

/// The direction of `delta`, folded into `[-pi/2, pi/2]` so text drawn at this
/// angle is never upside down.
fn upright_angle(delta: egui::Vec2) -> f32 {
    use std::f32::consts::{FRAC_PI_2, PI};

    let angle = delta.y.atan2(delta.x);

    if angle > FRAC_PI_2 {
        angle - PI
    } else if angle < -FRAC_PI_2 {
        angle + PI
    } else {
        angle
    }
}

fn length(line: &Line<f32>) -> f32 {
    (line.dx() * line.dx() + line.dy() * line.dy()).sqrt()
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
    ///
    /// **Re-recorded twice on 2026-08-28**, and the second recording undid part
    /// of the first. Against the recording that predates both, every surviving
    /// difference is one of these three and nothing else:
    ///
    /// 1. `background_color` is gone, along with the halo fields that briefly
    ///    replaced it. Nothing draws a halo; see [`crate::text::Text::shape`]
    ///    for the two approximations that were tried and rejected on the glass.
    /// 2. `max_width_ems: Some(10.0)` on the three *point* labels, 10 being
    ///    MapLibre's default `text-max-width`. This is the behaviour change:
    ///    point labels wrap now.
    /// 3. `max_width_ems: None` on the two *line* labels, for the same reason
    ///    from the other side: MapLibre wraps point placements only, and a
    ///    label laid along a river has no column to break into. That the same
    ///    recording carries both values is what makes it a pin on the
    ///    distinction rather than on wrapping being on or off everywhere.
    ///    (`line_height_ems: None` throughout; the fixture sets no
    ///    `text-line-height`.)
    ///
    /// The second recording removed ` halo_color: …, halo_width: …` from the
    /// five label entries **and changed nothing else** -- 215 characters, all of
    /// them inside those two fields. That is the whole diff, which is why
    /// removing the halo needed no judgement about whether anything moved.
    ///
    /// **No label position moved, and that is the load-bearing part.** The
    /// fixture's two roads are straight two-point lines, on which the arc-length
    /// anchor and the old chord midpoint agree exactly -- so this recording
    /// pins that [`anchor_along`] is a no-op on straight geometry and only
    /// changes what it was written to change. The one angle that moved,
    /// `-0.0` to `0.0` on the westward road, is the same number: `slope().atan()`
    /// gave negative zero for a westward run and the `atan2` fold gives
    /// positive zero.
    const GOLDEN: &str = r##"[Shape(Rect(RectShape { rect: [[0.0 0.0] - [4096.0 4096.0]], corner_radius: CornerRadius { nw: 0, ne: 0, sw: 0, se: 0 }, fill: #10_20_30_FF, stroke: Stroke { width: 0.0, color: #00_00_00_00 }, stroke_kind: Outside, round_to_pixels: None, blur_width: 0.0, brush: None, angle: 0.0 })), Shape(Mesh(Mesh { indices: [1, 0, 2, 1, 2, 3, 5, 4, 6, 5, 6, 7], vertices: [Vertex { pos: [0.0 0.0], uv: [0.0 0.0], color: #00_80_00_80 }, Vertex { pos: [100.0 0.0], uv: [0.0 0.0], color: #00_80_00_80 }, Vertex { pos: [0.0 100.0], uv: [0.0 0.0], color: #00_80_00_80 }, Vertex { pos: [100.0 100.0], uv: [0.0 0.0], color: #00_80_00_80 }, Vertex { pos: [200.0 200.0], uv: [0.0 0.0], color: #00_80_00_FF }, Vertex { pos: [350.0 200.0], uv: [0.0 0.0], color: #00_80_00_FF }, Vertex { pos: [200.0 350.0], uv: [0.0 0.0], color: #00_80_00_FF }, Vertex { pos: [350.0 350.0], uv: [0.0 0.0], color: #00_80_00_FF }], texture_id: Managed(0) })), Shape(Path(PathShape { points: [[10.0 20.0], [100.0 20.0], [100.0 100.0]], closed: false, fill: #00_00_00_00, stroke: PathStroke { width: 4.0, color: Solid(#CC_00_00_CC), kind: Middle } })), Shape(Path(PathShape { points: [[200.0 200.0], [300.0 300.0]], closed: false, fill: #00_00_00_00, stroke: PathStroke { width: 2.0, color: Solid(#00_00_FF_FF), kind: Middle } })), Shape(Path(PathShape { points: [[10.0 10.0], [60.0 10.0]], closed: false, fill: #00_00_00_00, stroke: PathStroke { width: 2.0, color: Solid(#FF_FF_00_FF), kind: Middle } })), Shape(Path(PathShape { points: [[60.0 60.0], [10.0 60.0]], closed: false, fill: #00_00_00_00, stroke: PathStroke { width: 2.0, color: Solid(#FF_FF_00_FF), kind: Middle } })), Text(Text { text: "Warsaw", position: [500.0 600.0], font_size: 12.0, text_color: #FF_FF_FF_FF, angle: 0.0, max_width_ems: Some(10.0), line_height_ems: None }), Text(Text { text: "Warsaw", position: [530.0 560.0], font_size: 12.0, text_color: #FF_FF_FF_FF, angle: 0.0, max_width_ems: Some(10.0), line_height_ems: None }), Text(Text { text: "Krakow", position: [1200.0 1300.0], font_size: 12.0, text_color: #FF_FF_FF_FF, angle: 0.0, max_width_ems: Some(10.0), line_height_ems: None }), Text(Text { text: "Back Alley", position: [35.0 10.0], font_size: 14.0, text_color: #CC_CC_CC_FF, angle: 0.0, max_width_ems: None, line_height_ems: None }), Text(Text { text: "Back Alley", position: [35.0 60.0], font_size: 14.0, text_color: #CC_CC_CC_FF, angle: 0.0, max_width_ems: None, line_height_ems: None })]"##;

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

    /// **One parse serves every styling, and each styling equals the fused
    /// path exactly.**
    ///
    /// The split this pins: [`parse`] is the zoom- and style-independent
    /// half, [`styled`] the style half, and a consumer caching the
    /// `ParsedTile` re-styles without re-decoding. Equality is against
    /// [`render`]'s own output *per style*, compared as `Debug` (exact for
    /// `f32`, as [`GOLDEN`] argues), and the two styles' outputs must differ
    /// from each other or the sharing was never exercised on anything a style
    /// could disagree about.
    #[test]
    fn one_parse_styled_under_two_styles_matches_the_fused_path_for_both() {
        let bytes = fixture();
        let parsed = parse(&bytes).expect("the fixture parses");

        let full = Style::from_json(STYLE).expect("fixture style parses");
        let lines_only = Style::from_json(&roads_line_style(r#"["==", "kind", "primary"]"#))
            .expect("style parses");

        let from_parse_full = format!("{:?}", styled(&parsed, &full, ZOOM));
        let from_parse_lines = format!("{:?}", styled(&parsed, &lines_only, ZOOM));

        assert_eq!(
            from_parse_full,
            format!("{:?}", render(&bytes, &full, ZOOM).expect("renders")),
            "styling a cached parse diverged from parse-and-style in one pass"
        );
        assert_eq!(
            from_parse_lines,
            format!("{:?}", render(&bytes, &lines_only, ZOOM).expect("renders")),
            "the second styling of the same parse diverged from the fused path"
        );
        assert_ne!(
            from_parse_full, from_parse_lines,
            "non-vacuity: the two styles must render differently, or the \
             equalities above would hold for a styling that ignored the style"
        );
    }

    /// The parsed representation is measurable, and the measure follows the
    /// content — the property a consumer sizes its cache with.
    #[test]
    fn a_parsed_tiles_heap_grows_with_its_content() {
        let empty = parse(&encode_tile(&[])).expect("an empty tile parses");
        let full = parse(&fixture()).expect("the fixture parses");

        assert!(
            full.heap_bytes() > empty.heap_bytes(),
            "the fixture's parse ({} B) must out-weigh an empty tile's ({} B)",
            full.heap_bytes(),
            empty.heap_bytes()
        );
        // The floor: seven features across three layers carry geometry and
        // property strings; a heap count that misses them would sit near zero.
        assert!(
            full.heap_bytes() > 500,
            "the fixture's parse measured only {} B, so the count is not \
             reaching the geometry and property heap",
            full.heap_bytes()
        );
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

    // ── Line-label placement ────────────────────────────────────────────────

    fn line_string(points: &[(f32, f32)]) -> geo_types::LineString<f32> {
        points.iter().copied().collect::<Vec<(f32, f32)>>().into()
    }

    /// The rule [`anchor_along`] replaced: the midpoint and slope of whichever
    /// single segment happens to be longest.
    ///
    /// Spelled out here rather than described, so the comparisons below are
    /// against the code that actually shipped.
    fn longest_segment_anchor(ls: &geo_types::LineString<f32>) -> (egui::Pos2, f32) {
        let line = ls
            .lines()
            .max_by_key(|line| length(line) as u32)
            .expect("a line string with at least one segment");
        (
            pos2(
                (line.start.x + line.end.x) / 2.0,
                (line.start.y + line.end.y) / 2.0,
            ),
            line.slope().atan(),
        )
    }

    /// Distance from `point` to the nearest point of the polyline.
    fn distance_to(ls: &geo_types::LineString<f32>, point: egui::Pos2) -> f32 {
        ls.lines()
            .map(|line| {
                let start = pos2(line.start.x, line.start.y);
                let seg = egui::vec2(line.dx(), line.dy());
                let len2 = seg.length_sq();
                let t = if len2 > 0.0 {
                    ((point - start).dot(seg) / len2).clamp(0.0, 1.0)
                } else {
                    0.0
                };
                (point - (start + seg * t)).length()
            })
            .fold(f32::INFINITY, f32::min)
    }

    /// The anchor is half way along by arc length, and on the geometry.
    #[test]
    fn a_line_label_sits_half_way_along_the_line() {
        // An L: two 100-unit legs, so the halfway point is exactly the corner.
        let ls = line_string(&[(0.0, 0.0), (100.0, 0.0), (100.0, 100.0)]);
        let (position, _) = anchor_along(&ls).expect("the L has an anchor");

        assert!(
            (position - pos2(100.0, 0.0)).length() < 0.01,
            "the halfway point of two equal legs is the corner, got {position:?}"
        );
        assert!(
            distance_to(&ls, position) < 0.01,
            "the anchor left the line"
        );
    }

    /// **The angle is the tangent where the label sits, not some other
    /// segment's.**
    ///
    /// A `V` whose two legs are the same length: the old rule took the longest
    /// segment, which for a tie is the last one, and reported its 45-degree
    /// descent. The label is drawn at the apex, where the line is turning
    /// through horizontal, so the direction across it is flat.
    #[test]
    fn a_line_label_takes_the_tangent_at_its_own_anchor() {
        let ls = line_string(&[(0.0, 0.0), (100.0, 100.0), (200.0, 0.0)]);

        let (position, angle) = anchor_along(&ls).expect("the V has an anchor");
        assert!(
            (position - pos2(100.0, 100.0)).length() < 0.01,
            "the anchor is the apex, got {position:?}"
        );
        assert!(
            angle.abs() < 0.05,
            "the tangent across the apex is flat, got {angle} rad"
        );

        // NON-VACUITY: the rule this replaced answers a quarter turn away on
        // the very same geometry, so the assertion above is not something any
        // placement would satisfy.
        let (_, was) = longest_segment_anchor(&ls);
        assert!(
            (was.abs() - std::f32::consts::FRAC_PI_4).abs() < 0.01,
            "the old rule is supposed to read 45 degrees here, got {was}"
        );
    }

    /// **The anchor barely moves when the same river is simplified, and that is
    /// the fix for labels that jump while zooming.**
    ///
    /// A vector tile carries a river generalised differently at every zoom.
    /// "The midpoint of the longest segment" is decided by which segment
    /// survives simplification, so it can leap from one end of a fragment to
    /// the other between two zoom levels -- the label pops out here and back in
    /// over there, at a new rotation. "Half way along" is a property of the
    /// river, not of the sampling, so it stays put.
    #[test]
    fn simplifying_a_river_barely_moves_its_label() {
        // One S-bend, sampled finely and coarsely. Same watercourse.
        let dense = line_string(&[
            (0.0, 0.0),
            (10.0, 6.0),
            (20.0, 13.0),
            (30.0, 15.0),
            (40.0, 11.0),
            (50.0, 0.0),
            (60.0, -11.0),
            (70.0, -15.0),
            (80.0, -13.0),
            (90.0, -6.0),
            (100.0, 0.0),
        ]);
        let coarse = line_string(&[
            (0.0, 0.0),
            (30.0, 15.0),
            (50.0, 0.0),
            (70.0, -15.0),
            (100.0, 0.0),
        ]);

        let (now_dense, _) = anchor_along(&dense).expect("dense anchor");
        let (now_coarse, _) = anchor_along(&coarse).expect("coarse anchor");
        let moved = (now_dense - now_coarse).length();

        let (was_dense, _) = longest_segment_anchor(&dense);
        let (was_coarse, _) = longest_segment_anchor(&coarse);
        let moved_before = (was_dense - was_coarse).length();

        assert!(
            moved < 5.0,
            "the anchor moved {moved} units between two samplings of one river"
        );

        // NON-VACUITY, and the size of the defect: the rule this replaced moves
        // an order of magnitude further on the same pair. Without this the
        // assertion above would also pass on a placement that ignored the
        // geometry entirely.
        assert!(
            moved_before > 4.0 * moved,
            "the old rule moved {moved_before} and the new one {moved}; this test \
             is only evidence if the two genuinely differ"
        );
    }

    /// Degenerate geometry has no anchor rather than a panicking one.
    #[test]
    fn a_degenerate_line_has_no_anchor() {
        assert!(anchor_along(&line_string(&[])).is_none());
        assert!(anchor_along(&line_string(&[(5.0, 5.0)])).is_none());
        // Every coordinate identical: zero total length, so no direction.
        assert!(anchor_along(&line_string(&[(5.0, 5.0), (5.0, 5.0), (5.0, 5.0)])).is_none());
    }

    /// A label never draws upside down, whichever way the line runs.
    #[test]
    fn a_westward_line_label_is_still_upright() {
        use std::f32::consts::FRAC_PI_2;

        for points in [
            [(0.0, 0.0), (100.0, 0.0)],   // east
            [(100.0, 0.0), (0.0, 0.0)],   // west
            [(0.0, 0.0), (0.0, 100.0)],   // south
            [(0.0, 100.0), (0.0, 0.0)],   // north
            [(0.0, 0.0), (-100.0, 50.0)], // north-west
        ] {
            let (_, angle) = anchor_along(&line_string(&points)).expect("an anchor");
            assert!(
                angle.abs() <= FRAC_PI_2 + 0.001,
                "{points:?} gave {angle} rad, which reads upside down"
            );
        }
    }

    /// `text-max-width` and `text-line-height` reach the emitted label, and the
    /// halo properties beside them reach nothing.
    ///
    /// The halo half is a *negative* pin and is the point of the test: the style
    /// asks loudly for `text-halo-color` and `text-halo-width: 2`, and the label
    /// carries neither, because nothing draws a halo. Two approximations were
    /// tried on the glass and both looked worse than plain glyphs; see
    /// [`crate::text::Text::shape`]. If a real one is ever built, this is the
    /// test that should go red.
    #[test]
    fn a_symbol_layer_carries_its_wrapping_and_no_halo_to_the_label() {
        let style = Style::from_json(
            r##"{"layers":[{"type":"symbol","source-layer":"places",
                 "layout":{"text-field":["get","name"],"text-size":12,
                           "text-max-width":6,"text-line-height":1.4},
                 "paint":{"text-color":"#ffffff","text-halo-color":"#000000",
                          "text-halo-width":2}}]}"##,
        )
        .expect("the fixture style parses");

        // The style DOES parse them -- `style` models the MapLibre spec, not
        // this renderer's subset -- which is what makes the absence below a
        // rendering decision rather than a parser gap.
        let Layer::Symbol { paint, .. } = &style.layers[0] else {
            panic!("fixture: the layer is a symbol layer");
        };
        let paint = paint.as_ref().expect("fixture: the layer has paint");
        assert!(
            paint.text_halo_color.is_some(),
            "fixture: halo colour asked"
        );
        assert!(paint.text_halo_width.is_some(), "fixture: halo width asked");

        let labels: Vec<Text> = render(&fixture(), &style, ZOOM)
            .expect("renders")
            .into_iter()
            .filter_map(|s| match s {
                ShapeOrText::Text(text) => Some(text),
                ShapeOrText::Shape(_) => None,
            })
            .collect();

        assert!(!labels.is_empty(), "the fixture has place labels to carry");
        for label in &labels {
            assert_eq!(label.max_width_ems, Some(6.0));
            assert_eq!(label.line_height_ems, Some(1.4));
        }
    }

    /// A style that says nothing about wrapping still wraps, at MapLibre's
    /// default of 10 ems.
    #[test]
    fn the_maplibre_defaults_are_what_an_unspecified_layer_gets() {
        let style = Style::from_json(
            r##"{"layers":[{"type":"symbol","source-layer":"places",
                 "layout":{"text-field":["get","name"],"text-size":12},
                 "paint":{"text-color":"#ffffff"}}]}"##,
        )
        .expect("the fixture style parses");

        let label = render(&fixture(), &style, ZOOM)
            .expect("renders")
            .into_iter()
            .find_map(|s| match s {
                ShapeOrText::Text(text) => Some(text),
                ShapeOrText::Shape(_) => None,
            })
            .expect("a place label");

        assert_eq!(label.max_width_ems, Some(10.0), "text-max-width default");
        // The control: a style that says nothing gets the default, and a style
        // that says `0` gets no wrapping at all, so the line above is reading
        // the property rather than reporting a constant.
        let off = Style::from_json(
            r##"{"layers":[{"type":"symbol","source-layer":"places",
                 "layout":{"text-field":["get","name"],"text-size":12,"text-max-width":0},
                 "paint":{"text-color":"#ffffff"}}]}"##,
        )
        .expect("the fixture style parses");
        let unwrapped = render(&fixture(), &off, ZOOM)
            .expect("renders")
            .into_iter()
            .find_map(|s| match s {
                ShapeOrText::Text(text) => Some(text),
                ShapeOrText::Shape(_) => None,
            })
            .expect("a place label");
        assert_eq!(
            unwrapped.max_width_ems, None,
            "text-max-width: 0 is 'never wrap'"
        );
    }

    /// **A line label does not wrap, however narrow `text-max-width` is.**
    ///
    /// MapLibre shapes a symbol with
    /// `placement === 'point' ? text-max-width * ONE_EM : 0`, so wrapping is a
    /// point-placement rule. A label following a river is laid out along the
    /// line and has no column to break into; wrapping it would stack short rows
    /// across the water instead of along it.
    #[test]
    fn a_line_label_is_never_wrapped() {
        // The same absurdly narrow cap applied to both layers, so the only
        // thing separating the two answers is the geometry.
        let style = Style::from_json(
            r##"{"layers":[
                 {"type":"symbol","source-layer":"places",
                  "layout":{"text-field":["get","name"],"text-size":12,"text-max-width":2},
                  "paint":{"text-color":"#ffffff"}},
                 {"type":"symbol","source-layer":"roads",
                  "layout":{"text-field":["get","name"],"text-size":12,"text-max-width":2},
                  "paint":{"text-color":"#cccccc"}}]}"##,
        )
        .expect("the fixture style parses");

        let labels: Vec<Text> = render(&fixture(), &style, ZOOM)
            .expect("renders")
            .into_iter()
            .filter_map(|s| match s {
                ShapeOrText::Text(text) => Some(text),
                ShapeOrText::Shape(_) => None,
            })
            .collect();

        // `places` is points, `roads` is lines; the fixture carries both.
        let (points, lines): (Vec<&Text>, Vec<&Text>) = labels
            .iter()
            .partition(|t| t.angle == 0.0 && t.font_size == 12.0 && t.text != "Back Alley");

        assert!(!points.is_empty(), "fixture: the tile has point labels");
        assert!(!lines.is_empty(), "fixture: the tile has line labels");

        for label in &lines {
            assert_eq!(
                label.max_width_ems, None,
                "the line label {:?} carries a wrap width",
                label.text
            );
        }
        // The control: the very same cap does reach a point label, so `None`
        // above is the placement rule and not the property being dropped.
        for label in &points {
            assert_eq!(
                label.max_width_ems,
                Some(2.0),
                "the point label {:?} lost its wrap width",
                label.text
            );
        }
    }

    /// The fixture style as JSON, with `patch` applied to every layer whose
    /// `type` is in `types` — or those layers removed outright when `patch` is
    /// `None`. The two modes are what make the zoom gate assertable as an
    /// identity: "these layers are out of range" and "these layers are not
    /// here" have to produce the same picture.
    fn fixture_style_with(types: &[&str], patch: Option<(&str, f64)>) -> Style {
        let mut style: JsonValue = serde_json::from_str(STYLE).expect("the fixture style is JSON");
        let layers = style["layers"].as_array_mut().expect("a layers array");
        match patch {
            Some((key, value)) => {
                for layer in layers.iter_mut() {
                    if types.contains(&layer["type"].as_str().unwrap_or_default()) {
                        layer[key] = serde_json::json!(value);
                    }
                }
            }
            None => {
                layers.retain(|layer| !types.contains(&layer["type"].as_str().unwrap_or_default()))
            }
        }
        Style::from_json(&style.to_string()).expect("the patched fixture style parses")
    }

    /// Render the fixture under `style`, reporting the shapes and the feature
    /// scans it took to reach them.
    fn drawn_and_scanned(style: &Style, zoom: u8) -> (String, usize) {
        let (shapes, scans) =
            scans::counted(|| render(&fixture(), style, zoom).expect("the fixture tile renders"));
        (format!("{shapes:?}"), scans)
    }

    /// **A layer outside its own zoom range does not read a single feature.**
    ///
    /// The waste this closes is not tessellation — a layer out of range draws
    /// nothing either way — it is the *scan*: building a [`Context`] over every
    /// feature of the layer's source layer and evaluating a filter against it,
    /// for a layer that cannot produce a shape at this zoom. Measured on the
    /// committed dark style over Monaco's z14 tile before this gate existed,
    /// the walk made **36,921 feature scans at every zoom from 0 to 16** — the
    /// same number at zoom 0, where 14 of the style's 95 layers are live, as at
    /// zoom 16, where 78 are.
    ///
    /// Asserted as an equality on both axes, because a one-sided "fewer scans"
    /// is satisfied by drawing nothing at all: the scan count drops by exactly
    /// the three `line` layers' share, and the picture is exactly the picture
    /// of the style with those three layers deleted.
    #[test]
    fn a_layer_outside_its_zoom_range_is_never_scanned() {
        let (all_drawn, all_scans) = drawn_and_scanned(
            &Style::from_json(STYLE).expect("fixture style parses"),
            ZOOM,
        );

        // The floor. Three `line` layers over `roads`' three features is nine
        // of the fixture's eighteen scans, and they are what the two variants
        // below remove — one by putting the layers out of range, one by
        // deleting them. A fixture that stopped scanning would make both trivially
        // equal, so the count is pinned here rather than only differenced.
        assert_eq!(
            all_scans, 18,
            "the fixture style scans `landuse` twice over two features (4), \
             `roads` four times over three (12) and `places` once over two (2)"
        );

        let out_of_range = fixture_style_with(&["line"], Some(("minzoom", f64::from(ZOOM) + 1.0)));
        let (gated_drawn, gated_scans) = drawn_and_scanned(&out_of_range, ZOOM);

        let deleted = fixture_style_with(&["line"], None);
        let (deleted_drawn, deleted_scans) = drawn_and_scanned(&deleted, ZOOM);

        assert_eq!(
            gated_scans, 9,
            "a `line` layer whose minzoom is above the tile zoom still read \
             features: the zoom gate is not skipping the scan"
        );
        assert_eq!(
            gated_scans, deleted_scans,
            "an out-of-range layer must cost exactly what an absent one costs"
        );
        assert_eq!(
            gated_drawn, deleted_drawn,
            "an out-of-range layer must draw exactly what an absent one draws"
        );

        // Non-vacuity: the very layers just gated do draw, and do scan, when
        // their range admits the zoom. Without this the two equalities above
        // are also satisfied by a style whose `line` layers never drew.
        assert!(
            all_scans > gated_scans && all_drawn != gated_drawn,
            "the control does not draw or scan more, so the gate proves nothing"
        );
    }

    /// **`minzoom` is inclusive, `maxzoom` is exclusive** — the specification's
    /// asymmetry, which is invisible except as a map that draws one zoom level
    /// too much or too little.
    ///
    /// <https://maplibre.org/maplibre-style-spec/layers/>: "at zoom levels less
    /// than the minzoom, the layer will be hidden" against "at zoom levels
    /// equal to or greater than the maxzoom, the layer will be hidden".
    #[test]
    fn the_zoom_range_honours_minzoom_inclusively_and_maxzoom_exclusively() {
        let ranged = {
            let mut style: JsonValue = serde_json::from_str(STYLE).expect("JSON");
            for layer in style["layers"].as_array_mut().expect("layers") {
                if layer["type"] == serde_json::json!("line") {
                    layer["minzoom"] = serde_json::json!(5);
                    layer["maxzoom"] = serde_json::json!(7);
                }
            }
            Style::from_json(&style.to_string()).expect("parses")
        };
        let absent = fixture_style_with(&["line"], None);

        for zoom in [4u8, 7, 8] {
            assert_eq!(
                drawn_and_scanned(&ranged, zoom),
                drawn_and_scanned(&absent, zoom),
                "zoom {zoom} is outside [5, 7) and the layers must be absent"
            );
        }
        for zoom in [5u8, 6] {
            assert_ne!(
                drawn_and_scanned(&ranged, zoom).0,
                drawn_and_scanned(&absent, zoom).0,
                "zoom {zoom} is inside [5, 7) and the layers must draw"
            );
        }
    }
}
