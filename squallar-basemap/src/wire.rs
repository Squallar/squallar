//! The codec for a styled vector tile: `Vec<walkers::ShapeOrText>` across a
//! message port, so the tessellation can happen where the thread is.
//!
//! # Four tags, and it refuses the fifth
//!
//! `walkers::mvt::styled` emits exactly four things — a coalesced fill
//! [`Mesh`], a stroke [`PathShape`], the one background [`RectShape`] and a
//! [`Text`] anchor. That is not an assumption this module makes; it is a
//! property the vendored `mvt.rs` pins by value in its own `GOLDEN` debug
//! string, and every encoder below **panics** on anything else rather than
//! skipping it. A silent fallback is how an unsupported shape becomes a blank
//! road, and a blank road is indistinguishable from a road that is genuinely
//! not there.
//!
//! Three invariants come with those tags, and each is CHECKED rather than
//! assumed, because each is what lets a field be dropped from the wire:
//!
//! * every mesh vertex is at [`epaint::WHITE_UV`](egui::epaint::WHITE_UV) and
//!   every mesh is at [`TextureId::default()`], so `uv` and `texture_id` are
//!   reconstructed as constants and a vertex costs 12 bytes instead of 20;
//! * a path's stroke is [`ColorMode::Solid`] at [`StrokeKind::Middle`], which
//!   is exactly what [`PathStroke::new`] builds, so a stroke is a width and a
//!   colour;
//! * the background rect is exactly `Shape::rect_filled(rect, 0.0, fill)`, so
//!   its eight other fields are reconstructed by calling that same
//!   constructor. The encoder rebuilds it and compares, so "exactly" is a
//!   test the encoder runs on every tile rather than a claim in this comment.
//!
//! # Head and tails
//!
//! Modelled on `squallar_buildings::jobs` line for line: a small descriptor
//! stream in the reply HEAD, and the bulk arrays as separately nominated
//! TAILS. `squallar_web::worker` pushes head and tails into one `buffers` list
//! and lends them identically, so nominating a tail buys no different
//! transport -- what it buys is not building the concatenation at all.
//!
//! Four tails, in this order, concatenated across every shape of every tile in
//! head order: mesh vertices (12 B each), mesh indices (4 B each), path points
//! (8 B each), label UTF-8. The decoder walks the head and advances one cursor
//! per tail, and every tail must end **exactly** where the head says: a length
//! that disagrees is another build's layout, not a tile to salvage.

use egui::epaint::{Mesh, PathShape, PathStroke, Vertex};
use egui::{Color32, Pos2, Rect, Shape, TextureId};
use squallar_source::wire::Reader;
use walkers::{ShapeOrText, Text};

/// A coalesced fill mesh.
const TAG_MESH: u8 = 0;
/// One stroke.
const TAG_PATH: u8 = 1;
/// The tile's background rectangle.
const TAG_RECT: u8 = 2;
/// One label anchor.
const TAG_TEXT: u8 = 3;

/// Bytes one mesh vertex costs: `pos.x`, `pos.y`, packed colour. `uv` and the
/// texture id are invariants, checked at encode and reconstructed at decode.
const VERTEX_BYTES: usize = 12;
/// Bytes one mesh index costs.
const INDEX_BYTES: usize = 4;
/// Bytes one path point costs.
const POINT_BYTES: usize = 8;

/// The most shapes one tile may declare.
///
/// A refusal ceiling and not a measured budget: the committed Monaco z14
/// city-core tile — the densest in that archive by a wide margin — styles to
/// **738** shapes against the unfiltered dark style. This is two orders of
/// magnitude past it, and exists so a doctored count answers `None` instead of
/// reserving its way into an abort on a 1 GiB wasm ceiling.
pub const MAX_SHAPES_PER_TILE: usize = 1 << 17;

/// The four bulk buffers a batch's shapes are written into.
#[derive(Debug, Default)]
pub struct Tails {
    /// Mesh vertices, [`VERTEX_BYTES`] each.
    pub vertices: Vec<u8>,
    /// Mesh indices, [`INDEX_BYTES`] each, **mesh-local and not rebased** —
    /// the round trip is an identity, so an index means what it meant.
    pub indices: Vec<u8>,
    /// Path points, [`POINT_BYTES`] each.
    pub points: Vec<u8>,
    /// Label UTF-8.
    pub text: Vec<u8>,
}

impl Tails {
    /// The four buffers in the order [`TailCursors`] reads them back.
    pub fn into_vec(self) -> Vec<Vec<u8>> {
        vec![self.vertices, self.indices, self.points, self.text]
    }
}

/// The read side of [`Tails`]: four independent cursors, advanced by
/// [`decode_shapes`] as the head describes each shape.
pub struct TailCursors<'a> {
    vertices: Reader<'a>,
    indices: Reader<'a>,
    points: Reader<'a>,
    text: Reader<'a>,
}

impl<'a> TailCursors<'a> {
    /// Refuses a tail count this module did not write.
    pub fn new(tails: &'a [Vec<u8>]) -> Option<Self> {
        let [vertices, indices, points, text] = tails else {
            return None;
        };
        Some(Self {
            vertices: Reader::new(vertices),
            indices: Reader::new(indices),
            points: Reader::new(points),
            text: Reader::new(text),
        })
    }

    /// Whether every tail was consumed exactly. A tail with bytes left over is
    /// a head that described less than was sent.
    pub fn all_consumed(&self) -> bool {
        self.vertices.at_end()
            && self.indices.at_end()
            && self.points.at_end()
            && self.text.at_end()
    }
}

/// A packed colour as the four premultiplied sRGBA bytes egui itself stores.
fn color_bits(color: Color32) -> u32 {
    u32::from_le_bytes(color.to_array())
}

/// [`color_bits`] undone.
fn color_of(bits: u32) -> Color32 {
    let [r, g, b, a] = bits.to_le_bytes();
    Color32::from_rgba_premultiplied(r, g, b, a)
}

fn put_u32(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn put_f32(out: &mut Vec<u8>, v: f32) {
    out.extend_from_slice(&v.to_le_bytes());
}

/// A length as a `u32`, saturating rather than truncating: a truncated length
/// is a plausible smaller number, and [`decode_shapes`] then runs off the end
/// of its tail and answers `None`, which is a refusal. Not reachable — the
/// ceilings above are far below `u32::MAX` — and `encode` has no error channel
/// to report it through, exactly as `squallar_buildings::jobs` records.
fn saturating_u32(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

/// An `Option<f32>` as a presence byte and a value. The value is written even
/// when absent (as zero) so the record is fixed-width and a corrupt presence
/// byte cannot desynchronise the cursor.
fn put_opt_f32(out: &mut Vec<u8>, v: Option<f32>) {
    out.push(u8::from(v.is_some()));
    put_f32(out, v.unwrap_or(0.0));
}

fn take_opt_f32(r: &mut Reader<'_>) -> Option<Option<f32>> {
    let present = r.u8()?;
    let value = r.f32()?;
    match present {
        0 => Some(None),
        1 => Some(Some(value)),
        // Not a bool this build wrote.
        _ => None,
    }
}

/// Append one tile's shapes to `head` and the four `tails`.
///
/// # Panics
///
/// On any shape `walkers::mvt::styled` cannot produce, and on any of the three
/// invariants in the module doc being false. Both are unreachable in a build
/// whose `walkers` is the vendored one — the spike encoded every tile of the
/// committed archive without reaching either — and both are a panic rather
/// than a skip on purpose: this runs in the worker, where a panic aborts, the
/// funnel fails the batch, and `squallar-egui`'s pump decodes those tiles
/// inline as it does today. That is a degraded path with a loud cause. A
/// silently skipped shape is a wrong map with no cause at all.
pub fn encode_shapes(shapes: &[ShapeOrText], head: &mut Vec<u8>, tails: &mut Tails) {
    for shape in shapes {
        match shape {
            ShapeOrText::Shape(Shape::Mesh(mesh)) => {
                assert!(
                    mesh.texture_id == TextureId::default(),
                    "a styled basemap tile carried a mesh on texture {:?}; the \
                     wire reconstructs the default texture id and every fill \
                     `mvt::styled` emits is on it",
                    mesh.texture_id,
                );
                head.push(TAG_MESH);
                put_u32(head, saturating_u32(mesh.vertices.len()));
                put_u32(head, saturating_u32(mesh.indices.len()));
                tails.vertices.reserve(mesh.vertices.len() * VERTEX_BYTES);
                for vertex in &mesh.vertices {
                    assert!(
                        vertex.uv == egui::epaint::WHITE_UV,
                        "a styled basemap tile carried a mesh vertex at uv {:?}; \
                         the wire reconstructs WHITE_UV and `mvt::styled` emits \
                         every fill vertex there",
                        vertex.uv,
                    );
                    put_f32(&mut tails.vertices, vertex.pos.x);
                    put_f32(&mut tails.vertices, vertex.pos.y);
                    put_u32(&mut tails.vertices, color_bits(vertex.color));
                }
                tails.indices.reserve(mesh.indices.len() * INDEX_BYTES);
                for index in &mesh.indices {
                    put_u32(&mut tails.indices, *index);
                }
            }
            ShapeOrText::Shape(Shape::Path(path)) => {
                let egui::epaint::ColorMode::Solid(stroke_color) = path.stroke.color else {
                    panic!(
                        "a styled basemap tile carried a UV-callback stroke; the \
                         wire carries a solid colour, and `mvt::render_line` \
                         builds every stroke through `PathStroke::new`"
                    );
                };
                assert!(
                    path.stroke.kind == egui::StrokeKind::Middle,
                    "a styled basemap tile carried a {:?} stroke; the wire \
                     reconstructs `PathStroke::new`, which is Middle",
                    path.stroke.kind,
                );
                head.push(TAG_PATH);
                put_u32(head, saturating_u32(path.points.len()));
                head.push(u8::from(path.closed));
                put_u32(head, color_bits(path.fill));
                put_f32(head, path.stroke.width);
                put_u32(head, color_bits(stroke_color));
                tails.points.reserve(path.points.len() * POINT_BYTES);
                for point in &path.points {
                    put_f32(&mut tails.points, point.x);
                    put_f32(&mut tails.points, point.y);
                }
            }
            ShapeOrText::Shape(Shape::Rect(rect)) => {
                // Rebuilt and compared rather than field-by-field encoded: the
                // background is the one `Shape::rect_filled(rect, 0.0, fill)`
                // in `mvt::styled`, and reconstructing it through that same
                // constructor is what makes the round trip an identity across
                // an epaint version that adds a field.
                let rebuilt = Shape::rect_filled(rect.rect, 0.0, rect.fill);
                assert!(
                    matches!(&rebuilt, Shape::Rect(r) if r == rect),
                    "a styled basemap tile carried a rect that is not \
                     `rect_filled(rect, 0.0, fill)`: {rect:?}. The wire carries \
                     its rect and its fill and reconstructs the rest."
                );
                head.push(TAG_RECT);
                put_f32(head, rect.rect.min.x);
                put_f32(head, rect.rect.min.y);
                put_f32(head, rect.rect.max.x);
                put_f32(head, rect.rect.max.y);
                put_u32(head, color_bits(rect.fill));
            }
            ShapeOrText::Text(text) => {
                head.push(TAG_TEXT);
                put_f32(head, text.position.x);
                put_f32(head, text.position.y);
                put_f32(head, text.font_size);
                put_u32(head, color_bits(text.text_color));
                put_f32(head, text.angle);
                put_opt_f32(head, text.max_width_ems);
                put_opt_f32(head, text.line_height_ems);
                put_u32(head, saturating_u32(text.text.len()));
                tails.text.extend_from_slice(text.text.as_bytes());
            }
            ShapeOrText::Shape(other) => panic!(
                "a styled basemap tile carried a shape this wire has no tag \
                 for: {other:?}. `mvt::styled` emits Mesh, Path, Rect and Text \
                 and its own GOLDEN pins that; a new emitter needs a tag here, \
                 not a fallback."
            ),
        }
    }
}

/// Read `count` shapes off `head`, consuming the four tail cursors.
///
/// `None` on anything this build did not write: an unknown tag, a count past
/// [`MAX_SHAPES_PER_TILE`], a length longer than the tail that must hold it,
/// a presence byte that is not a bool, or label bytes that are not UTF-8.
pub fn decode_shapes(
    count: usize,
    head: &mut Reader<'_>,
    tails: &mut TailCursors<'_>,
) -> Option<Vec<ShapeOrText>> {
    if count > MAX_SHAPES_PER_TILE {
        return None;
    }
    let mut shapes = Vec::with_capacity(count);
    for _ in 0..count {
        let shape = match head.u8()? {
            TAG_MESH => {
                let vertices = tails.vertices.bounded(head.u32()?, VERTEX_BYTES)?;
                let indices = tails.indices.bounded(head.u32()?, INDEX_BYTES)?;
                let mut mesh = Mesh {
                    indices: Vec::with_capacity(indices),
                    vertices: Vec::with_capacity(vertices),
                    texture_id: TextureId::default(),
                };
                for _ in 0..vertices {
                    mesh.vertices.push(Vertex {
                        pos: Pos2::new(tails.vertices.f32()?, tails.vertices.f32()?),
                        uv: egui::epaint::WHITE_UV,
                        color: color_of(tails.vertices.u32()?),
                    });
                }
                for _ in 0..indices {
                    mesh.indices.push(tails.indices.u32()?);
                }
                ShapeOrText::Shape(Shape::Mesh(mesh.into()))
            }
            TAG_PATH => {
                let points = tails.points.bounded(head.u32()?, POINT_BYTES)?;
                let closed = match head.u8()? {
                    0 => false,
                    1 => true,
                    _ => return None,
                };
                let fill = color_of(head.u32()?);
                let width = head.f32()?;
                let stroke_color = color_of(head.u32()?);
                let mut path_points = Vec::with_capacity(points);
                for _ in 0..points {
                    path_points.push(Pos2::new(tails.points.f32()?, tails.points.f32()?));
                }
                ShapeOrText::Shape(Shape::Path(PathShape {
                    points: path_points,
                    closed,
                    fill,
                    stroke: PathStroke::new(width, stroke_color),
                }))
            }
            TAG_RECT => {
                let rect = Rect::from_min_max(
                    Pos2::new(head.f32()?, head.f32()?),
                    Pos2::new(head.f32()?, head.f32()?),
                );
                // The same constructor `mvt::styled` calls — see `encode_shapes`.
                ShapeOrText::Shape(Shape::rect_filled(rect, 0.0, color_of(head.u32()?)))
            }
            TAG_TEXT => {
                let position = Pos2::new(head.f32()?, head.f32()?);
                let font_size = head.f32()?;
                let text_color = color_of(head.u32()?);
                let angle = head.f32()?;
                let max_width_ems = take_opt_f32(head)?;
                let line_height_ems = take_opt_f32(head)?;
                let len = usize::try_from(head.u32()?).ok()?;
                let body = std::str::from_utf8(tails.text.take(len)?).ok()?;
                ShapeOrText::Text(
                    Text::new(position, body.to_owned(), font_size, text_color, angle)
                        .with_wrapping(max_width_ems, line_height_ems),
                )
            }
            // A tag this build does not allocate.
            _ => return None,
        };
        shapes.push(shape);
    }
    Some(shapes)
}

#[cfg(test)]
mod tests;
