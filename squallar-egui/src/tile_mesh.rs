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
//! # What stays on the CPU, and why
//!
//! * **Strokes.** A style's `line-width` is in *screen points* and the
//!   geometry beside it is in extent units, so a road is the width the style
//!   asked for whatever side the tile is drawn at (see `mvt::render_line`).
//!   Pre-tessellating a stroke in extent space would scale that width with the
//!   tile; keeping the offsets in points means reproducing epaint's own
//!   feathered stroke tessellation, byte for byte, in a shader. Neither is
//!   worth it here. The same Monaco tile's 708 `Shape::Path`s (4,390 points)
//!   measure 13.51 µs of placement, so this leaves **roughly half** the tile's
//!   per-frame placement on the CPU — a measurement, not an estimate, and the
//!   [`ledger`] reports both halves separately so it stays one.
//! * **Labels.** They need egui's font atlas and its glyph layout, they are
//!   few, and they are laid out once per pane rather than once per tile.
//! * **The background rectangle.** One shape per tile.
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

/// One `Shape::Mesh` of the styled tile, as a range of the flat buffers.
///
/// `shape_index` is its position in the tile's `Vec<ShapeOrText>`, which is
/// what keeps the draw order the style asked for: a fill run draws where it
/// sat among the strokes, not before or after all of them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MeshRun {
    pub shape_index: u32,
    pub first_index: u32,
    pub index_count: u32,
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
        self.vertices.len() as u64 + self.indices.len() as u64
    }

    /// Whether there is anything here to draw. A raster tile, or a styled
    /// vector tile whose style produced no fills at this zoom, flattens to
    /// nothing and keeps the CPU path by simply having no runs.
    pub fn is_empty(&self) -> bool {
        self.runs.is_empty()
    }
}

/// Flatten a styled tile's `Shape::Mesh`es into one buffer pair.
///
/// Runs are emitted in shape order and indices are rebased into the shared
/// vertex buffer, so a run draws with `base_vertex` zero over its own index
/// range.
///
/// A mesh carrying a texture other than egui's font atlas, or a vertex whose
/// `uv` is not [`WHITE_UV`](egui::epaint::WHITE_UV), is **skipped**: the
/// renderer's shader has no texture to sample and the caller's CPU path draws
/// it correctly. `mvt::render` emits neither today; this is the branch that
/// keeps that a fact rather than an assumption.
pub fn flatten(shapes: &[ShapeOrText]) -> TileMeshes {
    flatten_meshes(
        shapes
            .iter()
            .enumerate()
            .filter_map(|(index, shape)| match shape {
                ShapeOrText::Shape(egui::Shape::Mesh(mesh)) => Some((index as u32, &**mesh)),
                _ => None,
            }),
    )
}

/// [`flatten`]'s inner loop, over `(shape index, mesh)` pairs.
///
/// Separate from [`flatten`] so a caller that has meshes but no styled tile —
/// the renderer's own parity gate, which lives in a crate that must not
/// depend on `walkers` — builds its fixture through the **same** flattener
/// the map builds real tiles through, rather than through a second copy of it
/// that could agree with the shader while disagreeing with the map.
pub fn flatten_meshes<'a>(
    meshes: impl Iterator<Item = (u32, &'a egui::epaint::Mesh)>,
) -> TileMeshes {
    let mut vertices: Vec<u8> = Vec::new();
    let mut indices: Vec<u8> = Vec::new();
    let mut vertex_count = 0u32;
    let mut index_count = 0u32;
    let mut runs: Vec<MeshRun> = Vec::new();

    for (shape_index, mesh) in meshes {
        // A run with no triangles in it draws nothing and would ask the
        // renderer for a zero-length buffer, which is a validation error
        // rather than an empty draw. There is nothing to keep for the CPU
        // path either, so it is simply not a run.
        if mesh.vertices.is_empty() || mesh.indices.is_empty() {
            continue;
        }
        if mesh.texture_id != egui::TextureId::default()
            || mesh
                .vertices
                .iter()
                .any(|vertex| vertex.uv != egui::epaint::WHITE_UV)
        {
            continue;
        }

        let base = vertex_count;
        let first_index = index_count;
        vertices.reserve(mesh.vertices.len() * TILE_VERTEX_BYTES as usize);
        for vertex in &mesh.vertices {
            vertices.extend_from_slice(&vertex.pos.x.to_ne_bytes());
            vertices.extend_from_slice(&vertex.pos.y.to_ne_bytes());
            // `Color32` is already premultiplied sRGB bytes in RGBA order,
            // which is the order egui's own vertex attribute packs them in.
            vertices.extend_from_slice(&vertex.color.to_array());
        }
        indices.reserve(mesh.indices.len() * TILE_INDEX_BYTES as usize);
        for index in &mesh.indices {
            indices.extend_from_slice(&(index + base).to_ne_bytes());
        }
        vertex_count += mesh.vertices.len() as u32;
        index_count += mesh.indices.len() as u32;
        runs.push(MeshRun {
            shape_index,
            first_index,
            index_count: mesh.indices.len() as u32,
        });
    }

    vertices.shrink_to_fit();
    indices.shrink_to_fit();
    runs.shrink_to_fit();

    TileMeshes {
        id: NEXT_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
        vertices,
        indices,
        vertex_count,
        index_count,
        runs,
    }
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
