//! A vector tile's tessellated fills, flattened once and drawn from the GPU.
//!
//! A styled MVT tile is a `Vec<ShapeOrText>` in **extent units over the whole
//! tile**, and the map draws it by placing every shape onto the tile's screen
//! rect. For the fills that placement is the whole cost: measured on the
//! committed Monaco fixture's z14 tile, release build,
//! [`walkers::ShapeOrText::placed`] over its two coalesced meshes (18,018
//! vertices, 40,812 indices) is 12.63 µs of the tile's 26.61 µs — per tile, per
//! frame, against a viewport that holds up to 84 of them. Placement is only the
//! first of the passes those vertices take: epaint's tessellator walks them
//! again for the bounds cull and copies them into the frame's mesh, and
//! `update_buffers` copies that into the staging belt.
//!
//! The geometry does not change between frames; only where it goes does. So it
//! is flattened once, when the tile arrives or is restyled ([`TileMeshes`], off
//! the frame thread on native), uploaded once per tile lifetime, and drawn with
//! the placement as a **uniform**. What the CPU does per frame is one paint
//! callback per mesh run.
//!
//! **Strokes take the same route**, since 2026-09-01. A style's `line-width`
//! is in screen points while the geometry beside it is in extent units, which
//! is what kept them here — but epaint computes a stroke vertex as
//! `point + normal * radius` with the normal read off the path's own points,
//! and a normalised direction is invariant under a scale-and-translate. So the
//! *offset* is a screen-point quantity that does not change with the tile's
//! side: epaint's tessellation runs once at tile build, in extent space, and
//! the offset rides along as a second vertex attribute the shader adds after
//! the placement. See [`stroke`], which owns that port and the list of paths
//! it refuses.
//!
//! # What stays on the CPU, and why
//!
//! * **Labels.** They need egui's font atlas and its glyph layout, they are
//!   few, and they are laid out once per pane rather than once per tile.
//! * **The background rectangle.** One shape per tile.
//! * **Any stroke [`stroke::is_thick_open_stroke`] refuses** — a path whose
//!   coordinates are not integers in `i16`, or one thinner than a pixel, whose
//!   topology is a different branch of epaint's tessellator.
//!
//! # The GPU half
//!
//! Lives in `squallar_gpu::tile_mesh`, reached through [`TileMeshPainter`] —
//! the same shape as the 3D view's [`VolumePainter`](crate::volume_view::VolumePainter):
//! this crate hands over a description and gets back the opaque
//! `Arc<dyn Any>` `egui_wgpu` downcasts, and never names wgpu.

use std::any::Any;
use std::sync::Arc;

use walkers::ShapeOrText;

/// What the [`ledger`] counts, and the denominators it counts against.
pub mod ledger;

/// epaint's thick-open stroke tessellation, run once in extent space.
pub mod stroke;

/// One vertex of a tile's flattened fills.
///
/// **Positions are in MVT extent units**, not screen points: the placement is
/// a uniform the shader applies, which is the whole reason this type exists.
/// `color` is egui's own packed byte quadruple, moved across unchanged — the
/// renderer unpacks it exactly as egui's shader does.
///
/// `uv` is not carried. `mvt::render` emits every fill vertex at
/// [`epaint::WHITE_UV`](egui::epaint::WHITE_UV) with the default texture id,
/// where egui's sampler reads the atlas's reserved opaque-white texel, so the
/// texture factor is exactly one and the renderer's shader omits it.
/// [`flatten`] checks that rather than assuming it, and refuses a run that
/// says otherwise.
///
/// This is the **layout description**, not the storage: [`TileMeshes`] holds
/// the vertices as bytes in exactly this shape, so the upload is a handover of
/// a slice rather than a second pass over the geometry — and neither this
/// crate nor the renderer needs a pointer cast to get there.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TileVertex {
    pub pos: [f32; 2],
    pub color: u32,
}

/// Bytes one [`TileVertex`] occupies in the vertex buffer: two `f32`s and a
/// `u32`, native-endian, no padding.
pub const TILE_VERTEX_BYTES: u64 = 12;

/// Bytes one index occupies in the index buffer.
pub const TILE_INDEX_BYTES: u64 = 4;

/// Which buffer pair a run draws out of, and so which pipeline draws it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunKind {
    /// One `Shape::Mesh`, out of the fill buffers. `TileVertex`, `u32`
    /// indices, one shape per run.
    Fill,
    /// A span of consecutive `Shape::Path`s, out of the stroke buffers.
    /// [`stroke::StrokeVertex`], `u16` indices rebased onto
    /// [`MeshRun::first_vertex`].
    Stroke,
}

/// One run of the styled tile, as a range of the flat buffers.
///
/// `shape_index` is its position in the tile's `Vec<ShapeOrText>`, which is
/// what keeps the draw order the style asked for: a run draws where it sat
/// among the shapes, not before or after all of them.
///
/// A fill run is one mesh, so `shape_span` is 1. A stroke run is a *span* of
/// consecutive paths — anything that would draw between two of them closes
/// the run — and `shape_span` is how far the span reaches, so the shape walk
/// knows which later paths this run has already drawn. Labels are inside a
/// span rather than closing it: the ground phase defers every `Text` to
/// [`crate::ui_map_overlays::paint_labels`], so no label ever draws between
/// two of a span's paths.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MeshRun {
    pub shape_index: u32,
    /// Shapes from `shape_index` this run covers. Always 1 for a fill.
    pub shape_span: u32,
    /// The run's first vertex, for a stroke run's vertex-buffer offset.
    /// Always 0 for a fill, whose indices are rebased into the whole buffer.
    pub first_vertex: u32,
    pub first_index: u32,
    pub index_count: u32,
    pub kind: RunKind,
}

/// One tile's fills, flattened into a single vertex/index buffer pair.
///
/// Built once per (tile, style epoch) — the same seam the styling itself runs
/// at, so it is off the frame thread on native and inside the pump's decode
/// budget on wasm — and held beside the styled shapes in the tile cache. The
/// renderer's residency is keyed on [`Self::id`] and released when this value
/// is dropped, which is exactly when the tile leaves the LRU or a restyle
/// replaces it.
#[derive(Debug)]
pub struct TileMeshes {
    id: u64,
    /// [`TileVertex`]-shaped bytes, ready for the renderer's buffer write.
    vertices: Vec<u8>,
    /// `u32` indices, rebased into [`Self::vertices`], likewise as bytes.
    indices: Vec<u8>,
    vertex_count: u32,
    index_count: u32,
    /// [`stroke::StrokeVertex`]-shaped bytes.
    stroke_vertices: Vec<u8>,
    /// `u16` indices, rebased onto each run's own [`MeshRun::first_vertex`].
    stroke_indices: Vec<u8>,
    stroke_vertex_count: u32,
    stroke_index_count: u32,
    /// The tessellator feathering, in points, the stroke offsets were
    /// computed at — `feathering_size_in_pixels / pixels_per_point`.
    ///
    /// **A flatten input, because feathering is one.** It sets the two radii,
    /// the end extrude and, at hairline widths, which topology branch epaint
    /// takes. Drawing these offsets under a different `pixels_per_point` would
    /// paint wrong-width roads, so the ground phase compares this against the
    /// frame's and declines the run rather than drawing it; see
    /// [`crate::tiles::MapTileState::ensure_base_tiles`] for what re-flattens
    /// the tile.
    feathering: f32,
    runs: Vec<MeshRun>,
}

/// Identities are minted, never derived from a tile id: one `TileId` is a
/// different mesh under a different style epoch, and a stale GPU buffer drawn
/// under a re-used key is a wrong picture rather than a missing one.
static NEXT_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

impl TileMeshes {
    /// This tile's renderer-side identity. Unique for the process.
    pub fn id(&self) -> u64 {
        self.id
    }

    /// The vertex buffer's contents, in [`TileVertex`]'s layout.
    pub fn vertex_bytes(&self) -> &[u8] {
        &self.vertices
    }

    /// The index buffer's contents: `u32`s rebased into the vertex buffer, so
    /// a run draws over its own index range with a zero base vertex.
    pub fn index_bytes(&self) -> &[u8] {
        &self.indices
    }

    pub fn vertex_count(&self) -> u32 {
        self.vertex_count
    }

    pub fn index_count(&self) -> u32 {
        self.index_count
    }

    pub fn runs(&self) -> &[MeshRun] {
        &self.runs
    }

    /// The stroke vertex buffer's contents, in [`stroke::StrokeVertex`]'s
    /// layout.
    pub fn stroke_vertex_bytes(&self) -> &[u8] {
        &self.stroke_vertices
    }

    /// The stroke index buffer's contents: `u16`s rebased onto each run's own
    /// [`MeshRun::first_vertex`], which the run binds the vertex buffer at.
    pub fn stroke_index_bytes(&self) -> &[u8] {
        &self.stroke_indices
    }

    pub fn stroke_vertex_count(&self) -> u32 {
        self.stroke_vertex_count
    }

    pub fn stroke_index_count(&self) -> u32 {
        self.stroke_index_count
    }

    /// The feathering these stroke offsets were computed at, in points. See
    /// the field.
    pub fn feathering(&self) -> f32 {
        self.feathering
    }

    /// One stroke vertex, decoded back out of the bytes. Tests only, as
    /// [`Self::vertex`].
    pub fn stroke_vertex(&self, index: usize) -> Option<stroke::StrokeVertex> {
        let at = index * stroke::STROKE_VERTEX_BYTES as usize;
        let bytes: &[u8; 16] = self.stroke_vertices.get(at..at + 16)?.try_into().ok()?;
        Some(stroke::StrokeVertex {
            pos: [
                i16::from_ne_bytes(bytes[0..2].try_into().ok()?),
                i16::from_ne_bytes(bytes[2..4].try_into().ok()?),
            ],
            offset: [
                f32::from_ne_bytes(bytes[4..8].try_into().ok()?),
                f32::from_ne_bytes(bytes[8..12].try_into().ok()?),
            ],
            color: u32::from_ne_bytes(bytes[12..16].try_into().ok()?),
        })
    }

    /// One stroke index, decoded back out of the bytes. Tests only.
    pub fn stroke_index(&self, index: usize) -> Option<u16> {
        let at = index * stroke::STROKE_INDEX_BYTES as usize;
        Some(u16::from_ne_bytes(
            self.stroke_indices.get(at..at + 2)?.try_into().ok()?,
        ))
    }

    /// One vertex, decoded back out of the bytes. For the tests that check
    /// what was flattened; nothing shipped reads a vertex individually.
    pub fn vertex(&self, index: usize) -> Option<TileVertex> {
        let at = index * TILE_VERTEX_BYTES as usize;
        let bytes: &[u8; 12] = self.vertices.get(at..at + 12)?.try_into().ok()?;
        Some(TileVertex {
            pos: [
                f32::from_ne_bytes(bytes[0..4].try_into().ok()?),
                f32::from_ne_bytes(bytes[4..8].try_into().ok()?),
            ],
            color: u32::from_ne_bytes(bytes[8..12].try_into().ok()?),
        })
    }

    /// One index, decoded back out of the bytes. Tests only, as [`Self::vertex`].
    pub fn index(&self, index: usize) -> Option<u32> {
        let at = index * TILE_INDEX_BYTES as usize;
        Some(u32::from_ne_bytes(
            self.indices.get(at..at + 4)?.try_into().ok()?,
        ))
    }

    /// What one residency costs the GPU, counted the way the renderer budgets
    /// it: the two buffers' contents, nothing else.
    pub fn bytes(&self) -> u64 {
        self.vertices.len() as u64
            + self.indices.len() as u64
            + self.stroke_vertices.len() as u64
            + self.stroke_indices.len() as u64
    }

    /// Whether there is anything here to draw. A raster tile, or a styled
    /// vector tile whose style produced no fills at this zoom, flattens to
    /// nothing and keeps the CPU path by simply having no runs.
    pub fn is_empty(&self) -> bool {
        self.runs.is_empty()
    }
}

/// Flatten a styled tile's fills and strokes into buffers the GPU draws from.
///
/// Runs are emitted in shape order, so a fill or a stroke span draws where the
/// style put it. `feathering` is the tessellator's, in points
/// (`feathering_size_in_pixels / pixels_per_point`); it is a stroke input and
/// is remembered on the result — see [`TileMeshes::feathering`].
///
/// A mesh carrying a texture other than egui's font atlas, or a vertex whose
/// `uv` is not [`WHITE_UV`](egui::epaint::WHITE_UV), is **skipped**: the
/// renderer's shader has no texture to sample and the caller's CPU path draws
/// it correctly. `mvt::render` emits neither today; this is the branch that
/// keeps that a fact rather than an assumption. A path [`stroke::append`]
/// refuses is skipped the same way, and closes the stroke run it interrupted.
pub fn flatten(shapes: &[ShapeOrText], feathering: f32) -> TileMeshes {
    let mut flat = Flattening::new(feathering);
    for (index, shape) in shapes.iter().enumerate() {
        let index = index as u32;
        match shape {
            ShapeOrText::Shape(egui::Shape::Mesh(mesh)) => {
                flat.close_stroke_run();
                flat.push_mesh(index, mesh);
            }
            ShapeOrText::Shape(egui::Shape::Path(path)) => flat.push_path(index, path),
            // Deferred to the label phase and never drawn here, so it neither
            // opens nor closes a run. See [`MeshRun`].
            ShapeOrText::Text(_) => {}
            // The background rectangle, which does draw.
            ShapeOrText::Shape(_) => flat.close_stroke_run(),
        }
    }
    flat.finish()
}

/// [`flatten`]'s fill half, over `(shape index, mesh)` pairs.
///
/// Separate from [`flatten`] so a caller that has meshes but no styled tile —
/// the renderer's own parity gate, which lives in a crate that must not
/// depend on `walkers` — builds its fixture through the **same** flattener
/// the map builds real tiles through, rather than through a second copy of it
/// that could agree with the shader while disagreeing with the map.
pub fn flatten_meshes<'a>(
    meshes: impl Iterator<Item = (u32, &'a egui::epaint::Mesh)>,
) -> TileMeshes {
    // No strokes, so no feathering is consulted; the value is what an
    // unfeathered tessellator would use and is never read.
    let mut flat = Flattening::new(0.0);
    for (shape_index, mesh) in meshes {
        flat.push_mesh(shape_index, mesh);
    }
    flat.finish()
}

/// [`flatten`]'s stroke half, over `(shape index, path)` pairs.
///
/// The companion to [`flatten_meshes`], and there for the same reason: the
/// renderer's own parity gate must not depend on `walkers`, and a fixture it
/// builds has to go through **this** flattener rather than a second copy that
/// could agree with the shader while disagreeing with the map.
///
/// Indices that are consecutive make one run, exactly as consecutive paths in
/// a styled tile do.
pub fn flatten_paths<'a>(
    paths: impl Iterator<Item = (u32, &'a egui::epaint::PathShape)>,
    feathering: f32,
) -> TileMeshes {
    let mut flat = Flattening::new(feathering);
    for (shape_index, path) in paths {
        flat.push_path(shape_index, path);
    }
    flat.finish()
}

/// One tile's flatten in progress: the four byte buffers, the run list, and
/// the stroke run that is currently open.
struct Flattening {
    feathering: f32,
    vertices: Vec<u8>,
    indices: Vec<u8>,
    vertex_count: u32,
    index_count: u32,
    stroke_vertices: Vec<u8>,
    stroke_indices: Vec<u8>,
    stroke_vertex_count: u32,
    stroke_index_count: u32,
    /// The run being appended to, if one is open.
    open: Option<MeshRun>,
    scratch: stroke::Scratch,
    runs: Vec<MeshRun>,
}

impl Flattening {
    fn new(feathering: f32) -> Self {
        Self {
            feathering,
            vertices: Vec::new(),
            indices: Vec::new(),
            vertex_count: 0,
            index_count: 0,
            stroke_vertices: Vec::new(),
            stroke_indices: Vec::new(),
            stroke_vertex_count: 0,
            stroke_index_count: 0,
            open: None,
            scratch: stroke::Scratch::default(),
            runs: Vec::new(),
        }
    }

    fn push_mesh(&mut self, shape_index: u32, mesh: &egui::epaint::Mesh) {
        // A run with no triangles in it draws nothing and would ask the
        // renderer for a zero-length buffer, which is a validation error
        // rather than an empty draw. There is nothing to keep for the CPU
        // path either, so it is simply not a run.
        if mesh.vertices.is_empty() || mesh.indices.is_empty() {
            return;
        }
        if mesh.texture_id != egui::TextureId::default()
            || mesh
                .vertices
                .iter()
                .any(|vertex| vertex.uv != egui::epaint::WHITE_UV)
        {
            return;
        }

        let base = self.vertex_count;
        let first_index = self.index_count;
        self.vertices
            .reserve(mesh.vertices.len() * TILE_VERTEX_BYTES as usize);
        for vertex in &mesh.vertices {
            self.vertices.extend_from_slice(&vertex.pos.x.to_ne_bytes());
            self.vertices.extend_from_slice(&vertex.pos.y.to_ne_bytes());
            // `Color32` is already premultiplied sRGB bytes in RGBA order,
            // which is the order egui's own vertex attribute packs them in.
            self.vertices.extend_from_slice(&vertex.color.to_array());
        }
        self.indices
            .reserve(mesh.indices.len() * TILE_INDEX_BYTES as usize);
        for index in &mesh.indices {
            self.indices
                .extend_from_slice(&(index + base).to_ne_bytes());
        }
        self.vertex_count += mesh.vertices.len() as u32;
        self.index_count += mesh.indices.len() as u32;
        self.runs.push(MeshRun {
            shape_index,
            shape_span: 1,
            first_vertex: 0,
            first_index,
            index_count: mesh.indices.len() as u32,
            kind: RunKind::Fill,
        });
    }

    fn push_path(&mut self, shape_index: u32, path: &egui::epaint::PathShape) {
        let first_vertex = match &self.open {
            Some(run) => self.stroke_vertex_count - run.first_vertex,
            None => 0,
        };
        let before = self.stroke_vertices.len();
        let mut appended = stroke::append(
            path,
            self.feathering,
            &mut self.scratch,
            &mut self.stroke_vertices,
            &mut self.stroke_indices,
            first_vertex,
        );
        if appended == stroke::Appended::RunFull {
            // The open run has filled its `u16` index space. Close it and
            // offer the same path to a fresh one.
            self.close_stroke_run();
            appended = stroke::append(
                path,
                self.feathering,
                &mut self.scratch,
                &mut self.stroke_vertices,
                &mut self.stroke_indices,
                0,
            );
        }
        let stroke::Appended::Wrote(indices) = appended else {
            // Refused: it draws on the CPU, at its own place among the
            // shapes, so nothing may draw over it out of order.
            self.close_stroke_run();
            return;
        };

        let vertices =
            ((self.stroke_vertices.len() - before) as u64 / stroke::STROKE_VERTEX_BYTES) as u32;
        let run = self.open.get_or_insert(MeshRun {
            shape_index,
            shape_span: 0,
            first_vertex: self.stroke_vertex_count,
            first_index: self.stroke_index_count,
            index_count: 0,
            kind: RunKind::Stroke,
        });
        run.index_count += indices;
        run.shape_span = shape_index - run.shape_index + 1;
        self.stroke_vertex_count += vertices;
        self.stroke_index_count += indices;
    }

    /// Finish the open stroke run, if there is one.
    fn close_stroke_run(&mut self) {
        if let Some(run) = self.open.take() {
            self.runs.push(run);
        }
    }

    fn finish(mut self) -> TileMeshes {
        self.close_stroke_run();
        // The shape walk steps through this list in step with the shapes and
        // never searches, so the order is load-bearing. Every close already
        // precedes the push that caused it, which makes this a guard rather
        // than work; `runs_are_in_shape_order` is what holds it to that.
        self.runs.sort_by_key(|run| run.shape_index);

        self.vertices.shrink_to_fit();
        self.indices.shrink_to_fit();
        self.stroke_vertices.shrink_to_fit();
        self.stroke_indices.shrink_to_fit();
        self.runs.shrink_to_fit();

        TileMeshes {
            id: NEXT_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            vertices: self.vertices,
            indices: self.indices,
            vertex_count: self.vertex_count,
            index_count: self.index_count,
            stroke_vertices: self.stroke_vertices,
            stroke_indices: self.stroke_indices,
            stroke_vertex_count: self.stroke_vertex_count,
            stroke_index_count: self.stroke_index_count,
            feathering: self.feathering,
            runs: self.runs,
        }
    }
}

/// The tessellator feathering `ctx` will paint this frame at, in points.
///
/// **egui's own expression, and it must stay egui's own**: `Tessellator::new`
/// computes `feathering_size_in_pixels / pixels_per_point`, or zero when
/// feathering is off. Both sides of the stroke path read it from here — the
/// tile flatten, which bakes it into the offsets, and the ground phase, which
/// refuses a tile flattened at another value — so the two can never disagree
/// about what the number is, only about which frame it belongs to.
pub fn feathering_of(ctx: &egui::Context) -> f32 {
    let pixels_per_point = ctx.pixels_per_point();
    ctx.tessellation_options(|options| {
        if options.feathering {
            options.feathering_size_in_pixels / pixels_per_point
        } else {
            0.0
        }
    })
}

/// Where one tile's extent units land on screen: `scale * p + translation`,
/// the affine [`walkers::mvt::placement`] answers, carried to the shader
/// instead of being applied to every vertex.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Placement {
    pub scale: f32,
    pub translation: [f32; 2],
}

impl Placement {
    /// The placement for a tile whose **whole** extent covers `rect` — the
    /// same derivation [`walkers::mvt::placement`] makes, read off the same
    /// rect the CPU path places against.
    pub fn of(rect: egui::Rect) -> Self {
        let transform = walkers::mvt::placement(rect);
        Self {
            scale: transform.scaling,
            translation: [transform.translation.x, transform.translation.y],
        }
    }
}

/// One run of one tile, at one placement, on one frame.
pub struct GroundDraw<'a> {
    /// Cloned by the renderer to keep the flattened buffers alive for the
    /// upload, and downgraded to the weak handle its residency is swept by.
    pub meshes: &'a Arc<TileMeshes>,
    /// Index into [`TileMeshes::runs`].
    pub run: usize,
    pub place: Placement,
    /// egui's cumulative pass number, so the renderer can tell one frame's
    /// draws from the next without a clock or a callback of its own.
    pub pass_nr: u64,
}

/// Something that can draw a tile's flattened fills from the GPU.
///
/// Installed by the shell through [`GuiEvent::TileMeshPainter`]; absent, every
/// fill takes the CPU placement path, which is what a build without a wgpu
/// renderer (and every unit test in this crate) gets.
///
/// [`GuiEvent::TileMeshPainter`]: crate::shell_api::GuiEvent::TileMeshPainter
pub trait TileMeshPainter: Send + Sync {
    /// This frame's payload for one run, or `None` when the renderer cannot
    /// draw it and the caller must place the shape itself.
    fn payload(&self, draw: GroundDraw<'_>) -> Option<Arc<dyn Any + Send + Sync>>;
}

#[cfg(test)]
#[path = "tile_mesh/tests.rs"]
mod tests;

// The fixture is a PMTiles archive read through `basemap_archive`, which needs
// `tokio` and the filesystem — neither of which the wasm32 test target has.
#[cfg(all(test, not(target_arch = "wasm32")))]
#[path = "tile_mesh/fixture_tests.rs"]
mod fixture_tests;
