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
//! * **The background rectangle.** One shape per tile — and one *primitive*
//!   per pane rather than one per tile: the ground walk draws every tile's
//!   ahead of every tile's geometry, cut to its piece instead of clipped to
//!   it ([`background_within`]), so epaint merges them into one mesh.
//! * **Any stroke [`stroke::is_open_stroke`] refuses** — a closed path, a
//!   filled one, a `ColorMode::UV` one, or a path whose coordinates are not
//!   integers in `i16`. None of those can come out of `mvt::render_line` over
//!   an MVT tile, so on the shipped path this list is empty; it is the branch
//!   that keeps that a checked fact rather than a belief. **Both** of
//!   epaint's feathered branches are carried, the thick one and the hairline
//!   one — a line thinner than a pixel is not a refusal.
//!
//! # The GPU half
//!
//! Lives in `squallar_gpu::tile_mesh`, reached through [`TileMeshPainter`] —
//! the same shape as the 3D view's [`VolumePainter`](crate::volume_view::VolumePainter):
//! this crate hands over a description and gets back the opaque
//! `Arc<dyn Any>` `egui_wgpu` downcasts, and never names wgpu.

use std::any::Any;
use std::sync::Arc;

use egui::emath::GuiRounding as _;
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
/// One step of a tile's per-frame ground draw, in submission order.
///
/// **This is the shape walk, precomputed.** Which index opens a run, which
/// span a run covers, and which shapes the CPU still has to place are all a
/// pure function of `(tile, style epoch)` — the same pair [`TileMeshes`] is
/// built for, off the frame thread. Only the affine placement is per-frame, so
/// the frame has no reason to rediscover the structure by walking every shape.
///
/// It matters because the walk it replaces had almost nothing left to find.
/// Measured on the committed Monaco fixture's z14 city-core tile
/// (`tile_source.rs`): **738 shapes — 708 paths, two coalesced meshes and 27
/// labels.** Once fills moved to the GPU (`af3af305`) and strokes followed,
/// 708 of those 738 became runs the walk skips one at a time. The loop was not
/// slow; its reason for existing had been removed by two changes that shipped
/// without revisiting it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanStep {
    /// Issue `runs[first..first + count]` as **one** paint callback.
    ///
    /// Runs are coalesced here rather than at the frame, and only when
    /// nothing the ground phase draws sits between them, so the batch draws
    /// its runs in the order the style asked for and in the place the first
    /// of them held. See [`build_plan`] for why that is every run of a tile
    /// in practice.
    ///
    /// A run the renderer declines — no store, or a feathering this frame
    /// does not draw at — breaks the batch at exactly that run: the runs
    /// before it go out as one callback, its own span is placed on the CPU,
    /// and the runs after it open a fresh batch. That keeps the declined
    /// geometry at its own place among the shapes, which is what the
    /// un-planned walk did when `take_at` returned `None`.
    Runs { first: u32, count: u32 },
    /// Place `shapes[i]` on the CPU.
    Place(u32),
}

/// A tile's precomputed [`PlanStep`] list, with the shape count it was built
/// for.
///
/// **The count is a guard, not bookkeeping.** A plan is only valid for the
/// exact shape list it was derived from, and `TileMeshes` can also be built by
/// [`flatten_meshes`] / [`flatten_paths`], which never see a shape list. A
/// caller checks `matches` before trusting the steps, so a mismatched pair
/// falls back to the full walk rather than drawing the wrong tile.
#[derive(Debug)]
pub struct TilePlan {
    steps: Vec<PlanStep>,
    shape_count: u32,
    shape_slots: u32,
}

impl TilePlan {
    pub fn steps(&self) -> &[PlanStep] {
        &self.steps
    }

    /// Whether this plan was built for a shape list of exactly this length.
    pub fn matches(&self, shape_count: usize) -> bool {
        self.shape_count as usize == shape_count
    }

    /// **How many shapes this plan can hand the painter**, which is not how
    /// many steps it has.
    ///
    /// A `Runs` batch is one `Shape::Callback`. A `Place` is one shape unless
    /// the shape is a label -- a label is pushed onto the deferred list for
    /// [`crate::ui_map_overlays::paint_labels`] and puts nothing in the
    /// painter's list at all -- and on a real tile the labels are nearly all
    /// of it. So this is the ground walk's own `Vec::with_capacity`, settled
    /// where the plan is, off the frame thread.
    ///
    /// It is an exact count for the pass the renderer accepts, and a floor
    /// for the one it declines: a refused run puts its span's geometry back on
    /// the CPU (`ui_map_overlays::place_run_on_cpu`), which pushes more shapes
    /// than the one callback would have. The vector grows there, as it did
    /// before any of this.
    pub fn shape_slots(&self) -> usize {
        self.shape_slots as usize
    }
}

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
    /// **The fill buffers, held only until the one upload takes them.**
    ///
    /// [`TileVertex`]-shaped bytes and the `u32` indices rebased into them,
    /// ready for the renderer's buffer write — and read by **exactly one
    /// caller once per tile lifetime**, `squallar_gpu`'s
    /// `TileMeshStore::ensure`, which is keyed on [`Self::id`] and returns
    /// early for a tile it has already made resident. Nothing on the frame
    /// thread reads them: a fill run is a `Shape::Mesh` in the styled shape
    /// list, so the painterless route places that shape rather than these
    /// bytes ([`crate::ui_map_overlays`]'s `place_run_as_mesh` takes stroke
    /// runs and returns `false` for fills, "there is nothing here to win").
    ///
    /// So they were a **second host copy of a buffer already on the device**,
    /// held for the life of every cached tile. Measured on this box, one
    /// pane, fresh config, the shipped binary: the styled cache held 68.5 MB
    /// and `tile meshes` — this pair plus the stroke pair — was 38.6 MB of
    /// it, on a `live_peak` of 265-371 MB whose target is 250.
    ///
    /// [`Self::take_fill_bytes`] is the upload's take. Once taken, a store
    /// built *later* (a surface lost and rebuilt, which is
    /// `App::ensure_rendering_state` on a mobile resume) finds nothing to
    /// upload and simply does not make the tile's fills resident; the draw
    /// then places the tile's own `Shape::Mesh` shapes, which is what a build
    /// with no wgpu renderer has always done. Correct, and slower for those
    /// tiles until the LRU turns them over or a restyle re-flattens them.
    fills: std::sync::Mutex<Option<FillBytes>>,
    /// **Which painter's store took [`Self::fills`]**, as
    /// [`painter_epoch`] read it — zero while they are still here.
    ///
    /// The take is one-way and the buffers it hands over live in a *store*,
    /// so a store that is built after the take has nothing to make this tile's
    /// fills resident with and its draw would skip the run: a hole, not a
    /// fallback. That store is a real thing — a surface lost and rebuilt drops
    /// it and installs a fresh painter over a tile cache that survived — so
    /// the frame side asks [`Self::fill_runs_drawable`] before it hands a fill
    /// run to a callback at all, and places the tile's own `Shape::Mesh`
    /// instead when the answer is no.
    fills_epoch: std::sync::atomic::AtomicU64,
    /// What [`Self::fills`] held when it was flattened, kept after the take.
    ///
    /// [`Self::bytes`] is the **renderer's** residency figure — what the
    /// device holds while this value is alive — and the device keeps holding
    /// it after the host copy is gone, so the price may not fall with the
    /// take. See [`Self::host_bytes`] for the reading that does.
    fill_bytes: u64,
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
    /// The precomputed shape walk, when this was built from a shape list.
    /// `None` from [`flatten_meshes`] / [`flatten_paths`], which have none.
    plan: Option<TilePlan>,
}

/// One tile's fill buffers on their way to the device, and nowhere else.
///
/// A named pair rather than a tuple because the two are only ever handed over
/// together and a swapped pair is a wrong picture, not a compile error.
#[derive(Debug)]
pub struct FillBytes {
    /// [`TileVertex`]-shaped bytes.
    pub vertices: Vec<u8>,
    /// `u32` indices, rebased into [`Self::vertices`].
    pub indices: Vec<u8>,
}

/// Identities are minted, never derived from a tile id: one `TileId` is a
/// different mesh under a different style epoch, and a stale GPU buffer drawn
/// under a re-used key is a wrong picture rather than a missing one.
static NEXT_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

/// **How many times a [`TileMeshPainter`] has been installed**, starting at
/// one so that no tile's stamp can read as "never taken".
///
/// A painter arrives with the store behind it (`App::install_volume_bridge`
/// makes the `TileMeshStore` and publishes the painter in the same block), so
/// this counts stores as well, and a tile whose fill bytes an earlier store
/// took is exactly a tile whose stamp is behind this. See
/// [`TileMeshes::fills_epoch`].
static PAINTER_EPOCH: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

/// The epoch a take stamps and a draw compares against.
pub fn painter_epoch() -> u64 {
    PAINTER_EPOCH.load(std::sync::atomic::Ordering::Relaxed)
}

/// **A painter was installed**: every tile whose fills an earlier store took
/// stops offering its fill runs to a callback from here.
///
/// Called from the one place a painter is installed,
/// `Gui::apply(GuiEvent::TileMeshPainter)`. Bumped for a `None` too — there is
/// no store then either, and the CPU path is the right answer for both.
pub fn note_painter_installed() {
    PAINTER_EPOCH.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
}

impl TileMeshes {
    /// This tile's renderer-side identity. Unique for the process.
    pub fn id(&self) -> u64 {
        self.id
    }

    /// **Take the fill buffers for the one upload that reads them**, leaving
    /// this value holding none. See [`Self::fills`].
    ///
    /// `None` for a tile that flattened no fills, and for one whose bytes a
    /// store has already taken — the caller must then leave the tile's fills
    /// off the device and let the shape list draw them.
    pub fn take_fill_bytes(&self) -> Option<FillBytes> {
        let taken = self.fills.lock().ok()?.take()?;
        self.fills_epoch
            .store(painter_epoch(), std::sync::atomic::Ordering::Relaxed);
        Some(taken)
    }

    /// **Whether this tile's fill runs can be drawn through a callback**, or
    /// whether the frame must place their `Shape::Mesh` shapes itself.
    ///
    /// True while the fill bytes are still here — the store this frame is
    /// drawing through will upload them — and true once they have been taken
    /// by *this* painter's store. False for a tile whose bytes an earlier
    /// store took, which is a rebuilt surface; see [`Self::fills_epoch`].
    pub fn fill_runs_drawable(&self) -> bool {
        if self.fills_epoch.load(std::sync::atomic::Ordering::Relaxed) == 0 {
            return true;
        }
        self.fills_epoch.load(std::sync::atomic::Ordering::Relaxed) == painter_epoch()
    }

    /// Read the fill buffers, if they are still here — `None` once
    /// [`Self::take_fill_bytes`] has taken them.
    pub fn with_fill_bytes<R>(&self, read: impl FnOnce(&[u8], &[u8]) -> R) -> Option<R> {
        let held = self.fills.lock().ok()?;
        let fills = held.as_ref()?;
        Some(read(&fills.vertices, &fills.indices))
    }

    /// What the fill buffers held when they were flattened, whether or not
    /// they are still here. The figure [`Self::bytes`] carries them at.
    pub fn fill_bytes_len(&self) -> u64 {
        self.fill_bytes
    }

    /// **What this value holds on the host for the whole of its life** — the
    /// stroke pair, which no upload takes because the frame thread draws from
    /// it (`ui_map_overlays::place_run_as_mesh`, for a pass with no painter to
    /// hand a run to; the 3D floor strip is the one that ships).
    ///
    /// The figure the styled tile cache charges a slot, since the fill pair is
    /// the device's from the first draw and is named by the `tile meshes`
    /// census family. See [`Self::fills`].
    pub fn resident_host_bytes(&self) -> u64 {
        self.stroke_vertices.len() as u64 + self.stroke_indices.len() as u64
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

    /// The precomputed shape walk, if this tile has one.
    pub fn plan(&self) -> Option<&TilePlan> {
        self.plan.as_ref()
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

    /// **One stroke run's triangles, placed** — the mesh the GPU pipeline
    /// would have drawn, built on this thread instead.
    ///
    /// The arithmetic is the shader's own line, `scale * pos + translation +
    /// offset`, so what comes back is the run's vertices at the same screen
    /// positions the callback route would have put them, in the same order,
    /// with the same indices and the same colours.
    ///
    /// **For the pass that has no painter to hand the run to.** A floor strip
    /// is the one of those that ships: its primitives are copied into the
    /// mirror with every callback swapped for an empty mesh, so it cannot draw
    /// a run through one — see
    /// `crate::ui_map_pane::PaneRenderCtx::ground_mesh_painter`. Before this,
    /// such a pass put the run's `Shape::Path`s back through epaint's
    /// tessellator on every frame; this hands the tessellator one already
    /// tessellated mesh instead.
    ///
    /// The caller owns the feathering test. These offsets were computed at
    /// [`Self::feathering`] and are wrong-width roads under any other, exactly
    /// as they are for the renderer; the two routes decline on the same
    /// comparison rather than on two spellings of it.
    ///
    /// `None` for a fill run, an empty run, or a run whose range is not in
    /// these buffers.
    pub fn placed_stroke_mesh(&self, run: MeshRun, place: Placement) -> Option<egui::Mesh> {
        if run.kind != RunKind::Stroke || run.index_count == 0 {
            return None;
        }
        let index_bytes = stroke::STROKE_INDEX_BYTES as usize;
        let from = (run.first_index as usize).checked_mul(index_bytes)?;
        let to = from.checked_add((run.index_count as usize).checked_mul(index_bytes)?)?;
        let packed = self.stroke_indices.get(from..to)?;

        // The run's own highest index, which is how far into the vertex buffer
        // this run reaches: indices are rebased onto `first_vertex`, and the
        // run's vertex count is not carried on `MeshRun` because the renderer
        // never needs it -- it binds the buffer at the offset and draws.
        let mut indices: Vec<u32> = Vec::with_capacity(run.index_count as usize);
        let mut highest = 0u32;
        for pair in packed.chunks_exact(index_bytes) {
            let index = u32::from(u16::from_ne_bytes([pair[0], pair[1]]));
            highest = highest.max(index);
            indices.push(index);
        }

        let vertex_bytes = stroke::STROKE_VERTEX_BYTES as usize;
        let from = (run.first_vertex as usize).checked_mul(vertex_bytes)?;
        let to = from.checked_add((highest as usize + 1).checked_mul(vertex_bytes)?)?;
        let packed = self.stroke_vertices.get(from..to)?;

        let mut vertices = Vec::with_capacity(highest as usize + 1);
        for vertex in packed.chunks_exact(vertex_bytes) {
            let x = f32::from(i16::from_ne_bytes([vertex[0], vertex[1]]));
            let y = f32::from(i16::from_ne_bytes([vertex[2], vertex[3]]));
            let offset_x = f32::from_ne_bytes([vertex[4], vertex[5], vertex[6], vertex[7]]);
            let offset_y = f32::from_ne_bytes([vertex[8], vertex[9], vertex[10], vertex[11]]);
            vertices.push(egui::epaint::Vertex {
                pos: egui::pos2(
                    place.scale * x + place.translation[0] + offset_x,
                    place.scale * y + place.translation[1] + offset_y,
                ),
                // As the fills': `mvt::render` emits every stroke at the
                // atlas's reserved opaque-white texel, which is what lets the
                // renderer's shader omit the texture factor. See
                // [`TileVertex`].
                uv: egui::epaint::WHITE_UV,
                color: egui::Color32::from_rgba_premultiplied(
                    vertex[12], vertex[13], vertex[14], vertex[15],
                ),
            });
        }

        Some(egui::Mesh {
            indices,
            vertices,
            texture_id: egui::TextureId::default(),
        })
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
        let bytes: [u8; 12] = self.with_fill_bytes(|vertices, _| {
            vertices.get(at..at + 12).and_then(|b| b.try_into().ok())
        })??;
        let bytes = &bytes;
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
        Some(u32::from_ne_bytes(self.with_fill_bytes(
            |_, indices| indices.get(at..at + 4).and_then(|b| b.try_into().ok()),
        )??))
    }

    /// What one residency costs the GPU, counted the way the renderer budgets
    /// it: the two buffers' contents, nothing else.
    pub fn bytes(&self) -> u64 {
        self.fill_bytes + self.stroke_vertices.len() as u64 + self.stroke_indices.len() as u64
    }

    /// **What this value holds on the HOST right now** — the same sum as
    /// [`Self::bytes`] until the upload takes the fills, and the stroke pair
    /// alone after it.
    ///
    /// The two are different questions and neither answers the other: the
    /// device keeps its copy for as long as this value is alive, so the
    /// renderer's residency is [`Self::bytes`]; the host gives the fills back
    /// at the first draw, so what the allocator is holding is this.
    pub fn host_bytes(&self) -> u64 {
        self.fills
            .lock()
            .ok()
            .and_then(|held| {
                held.as_ref()
                    .map(|fills| (fills.vertices.len() + fills.indices.len()) as u64)
            })
            .unwrap_or(0)
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
    let mut flat = flat.finish();
    flat.plan = Some(build_plan(shapes, &flat.runs));
    flat
}

/// The precomputed shape walk for `shapes`, given the runs flattening produced.
///
/// **It reproduces the un-planned walk's decisions exactly, in its order.**
/// Every branch below is one the frame loop used to take per shape:
///
/// * a run's opening index becomes a [`PlanStep::Runs`] batch — the `take_at`
///   arm — extending the batch the step before it opened, if that step was
///   one;
/// * a **path** inside a run's span is dropped — the `covers` arm, which is
///   spelled for `Shape::Path` and nothing else;
/// * a **label anchored off the tile** is dropped — see
///   [`anchor_is_off_the_tile`], the one decision here that the un-planned
///   walk makes per frame instead;
/// * everything remaining becomes [`PlanStep::Place`] — the labels that can
///   draw, the background rectangle, and any path no run claimed.
///
/// The `Path` test is the subtle one and it is deliberate rather than
/// inherited: a `Text` whose anchor falls inside a stroke run's span is **not**
/// drawn by that run, so it must still be placed. Dropping the whole span
/// would silently lose labels in dense tiles, which is the one failure a
/// count-based gate would happily report as an improvement.
///
/// Runs arrive sorted by `shape_index` (`Flattening::finish`), which is what
/// lets this walk both lists once rather than searching.
fn build_plan(shapes: &[ShapeOrText], runs: &[MeshRun]) -> TilePlan {
    let mut covered = vec![false; shapes.len()];
    for run in runs {
        for at in run.shape_index..run.shape_index + run.shape_span {
            if let Some(slot) = covered.get_mut(at as usize) {
                *slot = true;
            }
        }
    }

    let mut steps: Vec<PlanStep> = Vec::new();
    let mut next_run = 0usize;
    for (index, shape) in shapes.iter().enumerate() {
        if next_run < runs.len() && runs[next_run].shape_index as usize == index {
            // Extend the batch the previous step opened, or open one. The
            // test is `steps.last()`, so anything the ground phase places
            // between two runs ends the batch by simply being the last step;
            // a `Text` cannot, because it is deferred to the label phase and
            // pushes nothing into the primitive list.
            match steps.last_mut() {
                Some(PlanStep::Runs { count, .. }) => *count += 1,
                _ => steps.push(PlanStep::Runs {
                    first: next_run as u32,
                    count: 1,
                }),
            }
            next_run += 1;
            continue;
        }
        if covered[index] && matches!(shape, ShapeOrText::Shape(egui::Shape::Path(_))) {
            continue;
        }
        if let ShapeOrText::Text(text) = shape
            && anchor_is_off_the_tile(text.position)
        {
            continue;
        }
        steps.push(PlanStep::Place(index as u32));
    }

    steps.shrink_to_fit();
    // See `TilePlan::shape_slots`: a step that defers a label draws nothing.
    let shape_slots = steps
        .iter()
        .filter(|step| match step {
            PlanStep::Runs { .. } => true,
            PlanStep::Place(index) => {
                !matches!(shapes.get(*index as usize), Some(ShapeOrText::Text(_)))
            }
        })
        .count() as u32;
    TilePlan {
        steps,
        shape_count: shapes.len() as u32,
        shape_slots,
    }
}

/// Whether a label anchored at `position` in extent units can be drawn by
/// **no** pass of this tile, whatever the frame does with it.
///
/// **This is the anchor test, answered once instead of every frame.** A tile
/// carries every label whose feature reaches it, buffer included, so most of
/// the names in a tile's shape list are anchored in a neighbour and belong to
/// that neighbour's pass; `ui_map_overlays::place_one` drops them on
/// `rect.contains(placed anchor)`. Measured on the native rig, scene A, one
/// 1920x1080 pane with real tiles, over two legs of two whole `pan-zoom-2d`
/// loops: of **18,909,323 text steps** the frame walked over 353,597 tile
/// pieces, 2,187,638 (11.6%) passed that test and **16,425,271 (86.9%) failed
/// it with an anchor outside the tile's own extent** — the population this
/// drops, and 98.2% of every failure there was. The other 296,414 (1.6%) are
/// anchors *on* the tile but outside a stretched ancestor's `uv` window, which
/// is the frame's to answer and stays the frame's.
///
/// The same two legs after: **zero** off-tile anchors reach the frame, and a
/// tile piece walks 7.1 text steps where it walked 53.3.
///
/// # Why the frame's answer does not depend on the frame
///
/// `place_one` tests `rect.contains(placement.scaling * position +
/// placement.translation)`, where `rect` is the tile's **piece** on screen and
/// `placement` is [`walkers::mvt::placement`] of the whole tile's rect,
/// recovered from the piece and its `uv` window. Two structural facts collapse
/// that to a test on `position` alone:
///
/// * a piece is `Projector::tile_rect_at`, which is `Vec2::splat(side)` —
///   **square**, always; and
/// * `uv` is `walkers::tiles::interpolate_from_lower_zoom`'s quadtree window,
///   so `0 ≤ uv.min` and `uv.max ≤ 1` — **inside the unit square**, always.
///
/// Substituting both, every `rect` term cancels and the test is exactly
/// `uv.contains(position / extent)`. Since `uv` is inside the unit square,
/// `unit.contains(position / extent)` is a *necessary* condition for it, for
/// every zoom, camera, pane rect, display scale and `uv` a cached plan can
/// ever be drawn under. Nothing else the frame knows enters it: the anchor is
/// `position`, and `position` changes only by restyling the tile, which builds
/// a new plan. The two facts are pinned by
/// `tests::the_two_facts_the_off_tile_cull_rests_on`.
///
/// # The margin
///
/// One extent unit, and it is the direction that matters: this may **keep** a
/// label the frame then drops (one more test, no pixel), and must never drop
/// one the frame would have kept. The frame's arithmetic is `f32` over screen
/// points, so its answer for an anchor sitting on the boundary can round
/// either way; one extent unit is four orders of magnitude more slack than the
/// worst rounding of that expression at any zoom the map has, and the labels
/// this exists for sit tens to hundreds of units outside.
fn anchor_is_off_the_tile(position: egui::Pos2) -> bool {
    // The tile placed on the unit square, which is `walkers::mvt::placement`'s
    // own expression at the one rect where the answer is the fraction of the
    // extent: `scaling` is `1 / extent`, exactly, and reading it here is what
    // keeps the extent walkers' constant rather than a copy of it.
    let unit = walkers::mvt::placement(egui::Rect::from_min_size(
        egui::Pos2::ZERO,
        egui::Vec2::splat(1.0),
    ));
    let at = unit.scaling * position;
    let margin = unit.scaling;
    at.x < -margin || at.x > 1.0 + margin || at.y < -margin || at.y > 1.0 + margin
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

        let fill_bytes = (self.vertices.len() + self.indices.len()) as u64;
        TileMeshes {
            plan: None,
            id: NEXT_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            fills: std::sync::Mutex::new(Some(FillBytes {
                vertices: self.vertices,
                indices: self.indices,
            })),
            fills_epoch: std::sync::atomic::AtomicU64::new(0),
            fill_bytes,
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

/// Whether `rect` is a background rectangle the ground walk may take out of
/// its tile: drawn ahead of every tile's geometry, under the pane's clip, in
/// one mesh with every other tile's, with the piece it belongs to folded into
/// its geometry by [`background_within`] instead of held by a clip rect.
///
/// **A plain axis-aligned fill is the one rectangle for which "clip to the
/// piece" and "cut to the piece" put the same bytes on screen.** Every field
/// tested here is a way a rectangle paints outside its own edges, or paints
/// them differently once they move: a stroke straddles them, a corner radius
/// or a rotation bends them, a blur widens the feather past the pixel a
/// scissor would have kept, a brush changes the texture the mesh is drawn with
/// and so which mesh it can share. `mvt::render`'s background is
/// `Shape::rect_filled(extent, 0, colour)`, which passes; this is the branch
/// that keeps that a checked fact rather than a belief, and a style that ever
/// put something else there keeps its background inside the tile, clipped, as
/// before.
pub fn is_hoistable_background(rect: &egui::epaint::RectShape) -> bool {
    rect.stroke.is_empty()
        && rect.corner_radius == egui::CornerRadius::ZERO
        && rect.blur_width == 0.0
        && rect.brush.is_none()
        && rect.angle == 0.0
}

/// `placed` — a tile's background after placement, one
/// [`is_hoistable_background`] said yes to — cut to `piece`, the rect the tile
/// occupies on screen, as the mesh that paints exactly the pixels the tile's
/// own clipped painter painted, ready to draw under the **pane's** clip.
///
/// # Why the cut is the clip
///
/// The tile's own painter clips to the piece and lets epaint tessellate the
/// rectangle: rounded to whole pixels first (`round_rects_to_pixels`, on by
/// default and never changed here), then feathered by half a pixel either
/// side of every edge, opaque inside and transparent outside
/// (`fill_closed_path`, at egui's default one-pixel feathering). On a
/// pixel-rounded edge that feather resolves to nothing: the pixel centre half
/// a pixel inside reads the fill exactly, the one half a pixel outside reads
/// alpha zero exactly. So a feathered, pixel-rounded rectangle paints exactly
/// the pixels of the *hard* rectangle at its rounded bounds — and the clip,
/// rounded by the same `round()` (`egui_wgpu`'s `ScissorRect::new`; both are
/// `round()`, neither `floor` nor `ceil`), keeps exactly the pixels of the
/// hard rectangle at the rounded intersection.
///
/// This returns that hard rectangle: four vertices at the rounded
/// intersection, two triangles, no feather. Inside, the fragments are the same
/// pixel centres in the same colour, so the same dither. Outside there is
/// nothing — where a feathered rectangle's alpha-zero outer band still passes
/// through the blend and the dither, one pixel into every neighbouring piece,
/// which is exactly what the tile's scissor used to discard; a feathered
/// rectangle under the pane's clip is *not* byte-identical, and that was
/// measured before this became a mesh (59 texels on a 256² canvas).
///
/// The rounding is spelled out although a rasterizer rounds a hard edge to
/// the nearest pixel centre by itself: on an edge sitting exactly on a
/// half-pixel the top-left rule and `round()` disagree, and the rounding is
/// what makes this mesh take the scissor's side of that tie. Byte parity with
/// the clipped arrangement is held on a real adapter by `squallar-gpu`'s
/// `tile_mesh_gpu` suite
/// (`the_hoisted_background_rectangles_put_the_same_bytes_on_screen_as_per_tile_clipping`
/// — `#[ignore]`d like every case there that needs an adapter; run with
/// `cargo test -p squallar-gpu --test tile_mesh_gpu -- --ignored`), with every
/// x edge on a half-pixel, every y edge elsewhere between pixels, and a
/// stretched ancestor whose placed rectangle reaches over three neighbouring
/// pieces.
///
/// `None` when the two do not overlap, or the rounding leaves nothing; the
/// tile's clipped walk then places the shape itself and draws nothing, which
/// is the same picture.
pub fn background_within(
    placed: &egui::epaint::RectShape,
    piece: egui::Rect,
    pixels_per_point: f32,
) -> Option<egui::Shape> {
    let (within, fill) = background_quad_within(placed, piece, pixels_per_point)?;
    let mut mesh = egui::epaint::Mesh::default();
    mesh.add_colored_rect(within, fill);
    Some(egui::Shape::Mesh(mesh.into()))
}

/// [`background_within`]'s answer before it is made a shape: the hard
/// rectangle and the colour to fill it with.
///
/// Split out so a pass drawing many tiles can put every quad in **one** mesh
/// rather than minting one `Mesh` and one `Arc` per tile — see
/// [`HoistedBackgrounds`]. The arithmetic is `background_within`'s, unmoved,
/// and that function is now this one plus the shape it wraps it in, so the
/// two cannot answer differently.
pub fn background_quad_within(
    placed: &egui::epaint::RectShape,
    piece: egui::Rect,
    pixels_per_point: f32,
) -> Option<(egui::Rect, egui::Color32)> {
    let within = placed
        .rect
        .intersect(piece)
        .round_to_pixels(pixels_per_point);
    if !within.is_positive() {
        return None;
    }
    Some((within, placed.fill))
}

/// Every hoisted background quad of one tile pass, in one mesh.
///
/// **The quads were always going to end up in one mesh.** They are flat,
/// untextured, submitted consecutively ahead of every tile's geometry and
/// under one clip, so epaint's tessellator already appended each one into the
/// primitive the one before it opened
/// (`Tessellator::tessellate_clipped_shape` starts a new mesh only on a clip
/// or texture change). What it could not do is un-spend what reaching it
/// cost: per tile, a `Mesh` with two `Vec`s, an `Arc` to put it in a
/// `Shape::Mesh`, and a `Context::write` for `Painter::add` to hand it over.
/// On a 1920x1080 pane that is 41 tiles a frame — 123 allocations and 41 lock
/// acquisitions to draw 41 rectangles of one colour.
///
/// This accumulates them instead and hands the painter one shape. The
/// vertices go in in the same order, so the stream the tessellator emits is
/// the one it emitted before, which
/// `tests::a_batched_background_run_tessellates_to_the_same_bytes_as_one_shape_per_tile`
/// holds byte for byte.
///
/// # The cull is epaint's, applied here
///
/// A batched mesh is bounded by the union of its quads, and epaint's coarse
/// culling drops a `Shape::Mesh` whose bounds miss the clip
/// (`coarse_tessellation_culling`, on by default). Batching would therefore
/// KEEP a quad epaint drops today — invisible either way, since the scissor
/// takes it, but it would put vertices in the stream that were not there. So
/// the same test is made here, against the same rectangle: a colour-quad
/// mesh's `calc_bounds` is exactly the rectangle it was built from.
#[derive(Default)]
pub struct HoistedBackgrounds {
    mesh: egui::epaint::Mesh,
}

impl HoistedBackgrounds {
    /// Take the quad `placed` contributes to `piece`, or say there is none.
    ///
    /// The answer is [`background_quad_within`]'s and nothing else decides it:
    /// a quad culled against `clip` is still a quad the caller has hoisted, so
    /// the tile's own walk skips its shape either way — exactly as it does
    /// today for a shape epaint goes on to cull.
    pub fn push(
        &mut self,
        placed: &egui::epaint::RectShape,
        piece: egui::Rect,
        pixels_per_point: f32,
        clip: egui::Rect,
    ) -> bool {
        let Some((within, fill)) = background_quad_within(placed, piece, pixels_per_point) else {
            return false;
        };
        if clip.intersects(within) {
            self.mesh.add_colored_rect(within, fill);
        }
        true
    }

    /// The one shape the pass hands the painter, or `None` when every quad
    /// was culled or there were none.
    pub fn finish(self) -> Option<egui::Shape> {
        if self.mesh.is_empty() {
            return None;
        }
        Some(egui::Shape::Mesh(self.mesh.into()))
    }
}

/// epaint's own coarse cull, applied where a batched quad is appended.
///
/// [`egui::Shape::image`] puts four vertices at the rect's corners and the
/// tessellator drops the mesh when `clip_rect.intersects(mesh.calc_bounds())`
/// is false (`coarse_tessellation_culling`, on by default). `calc_bounds`
/// walks the four vertices through `Rect::extend_with`, which keeps `min`
/// below `max`, so it is that rect with its corners sorted -- the identity
/// for the positive rects `Projector::tile_rect_at` produces, and the right
/// answer for anything else.
fn quad_bounds(rect: egui::Rect) -> egui::Rect {
    egui::Rect::from_min_max(
        egui::pos2(rect.min.x.min(rect.max.x), rect.min.y.min(rect.max.y)),
        egui::pos2(rect.min.x.max(rect.max.x), rect.min.y.max(rect.max.y)),
    )
}

/// One tile pass's raster quads, batched into one mesh per consecutive run of
/// one texture.
///
/// **The quads were already going to end up in one mesh per texture run.**
/// `Painter::image` is `Mesh::with_texture` plus one `add_rect_with_uv`
/// ([`egui::Shape::image`]), the grid draws every cell under ONE clip rect,
/// and `Tessellator::tessellate_clipped_shape` starts a new primitive only on
/// a clip change, a callback or a **texture change** -- so a run of tiles
/// sharing a page of [`crate::raster_atlas`] was already being appended into
/// the primitive the first of them opened. What that merge could not un-spend
/// is what reaching it cost, per tile: a `Mesh` with two `Vec`s, an `Arc` to
/// put it in a `Shape::Mesh`, and a `Context::write` for `Painter::add` to
/// hand it over.
///
/// This accumulates the run instead and hands the painter one shape for it.
/// The vertices go in in the same order with the same texture, so the stream
/// the tessellator emits is the one it emitted before, which
/// `tests::a_batched_raster_run_tessellates_to_the_same_bytes_as_one_image_per_tile`
/// holds byte for byte -- texture ids included.
///
/// # Two things end a run
///
/// * **A different texture id.** A viewport whose tiles do not all fit one
///   atlas page spills into a second, and two pages are two textures: they
///   were two primitives before this type and they still are. Batching across
///   them would put one texture's quads in the other's mesh.
/// * **Anything drawn between two quads.** The grid's second walk interleaves
///   raster and vector tiles, and a vector tile's geometry goes in where it
///   goes in. [`Self::take`] is what the caller hands over first.
///
/// # The cull is epaint's, applied here
///
/// A batched mesh is bounded by the union of its quads, so epaint's coarse
/// cull would KEEP a quad it drops today -- invisible either way, since the
/// scissor takes it, but it would put vertices in the stream that were not
/// there. So the same test is made here, per quad, against the same clip:
/// [`quad_bounds`].
#[derive(Default)]
pub struct RasterQuads {
    /// The run being accumulated: `None` before the first quad and after
    /// every hand-over. An empty `Mesh` carries `TextureId::default()`,
    /// which is the font atlas and a texture a tile could in principle be
    /// in, so emptiness is spelled here rather than read off the mesh.
    run: Option<egui::epaint::Mesh>,
}

impl RasterQuads {
    /// Add one tile's quad, handing back the run it ended, if any.
    ///
    /// The returned shape is the run that was open **before** this quad and
    /// must be given to the painter before this quad's own run is: the
    /// caller adds it immediately, and the new run is not handed over until
    /// a later `push`, [`Self::take`] or [`Self::finish`].
    ///
    /// A quad epaint would cull is dropped here and ends no run -- exactly as
    /// it reaches the renderer today, where the cull happens after the shape
    /// is submitted and so cannot separate two shapes either.
    pub fn push(
        &mut self,
        texture: egui::TextureId,
        rect: egui::Rect,
        uv: egui::Rect,
        tint: egui::Color32,
        clip: egui::Rect,
    ) -> Option<egui::Shape> {
        if !clip.intersects(quad_bounds(rect)) {
            return None;
        }
        let ended = match &self.run {
            Some(run) if run.texture_id == texture => None,
            Some(_) => self.take(),
            None => None,
        };
        self.run
            .get_or_insert_with(|| egui::epaint::Mesh::with_texture(texture))
            .add_rect_with_uv(rect, uv, tint);
        ended
    }

    /// Hand over the open run because something else is about to be drawn.
    pub fn take(&mut self) -> Option<egui::Shape> {
        self.run.take().map(|run| egui::Shape::Mesh(run.into()))
    }

    /// The last run of the pass, or `None` when every quad was culled or
    /// there were none.
    pub fn finish(mut self) -> Option<egui::Shape> {
        self.take()
    }
}

/// A consecutive span of one tile's runs, at one placement, on one frame.
///
/// **A span rather than a run** because every run of a tile shares this
/// placement, this clip and these buffers, so one callback can draw all of
/// them: a callback forces a primitive boundary unconditionally, and the
/// boundary is what the frame tail is paid for. See [`PlanStep::Runs`].
pub struct GroundDraw<'a> {
    /// Cloned by the renderer to keep the flattened buffers alive for the
    /// upload, and downgraded to the weak handle its residency is swept by.
    pub meshes: &'a Arc<TileMeshes>,
    /// Index into [`TileMeshes::runs`] of the first run to draw.
    pub first_run: usize,
    /// How many consecutive runs from [`Self::first_run`] this draw covers,
    /// in that order.
    pub run_count: usize,
    pub place: Placement,
    /// The painter's opacity for this frame, 0-1, which the callback must
    /// apply itself: a `Shape::Callback` is the one shape `Painter::add`
    /// cannot tint, so the layer walk's `set_opacity` reaches every
    /// CPU-placed shape beside these runs on its own and reaches the runs
    /// only through here.
    pub opacity: f32,
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
    /// This frame's payload for one span of runs, or `None` when the renderer
    /// cannot draw them and the caller must place the shapes itself.
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
