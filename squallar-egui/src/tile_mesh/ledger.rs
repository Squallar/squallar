//! What the ground phase has actually placed, and what it handed to the GPU.
//!
//! **Product telemetry, not a campaign instrument**, on the terms of
//! [`crate::floor_ledger`] and [`crate::overlay_cache::ledger`]: always on, no
//! feature gate, every write one `fetch_add` with [`Relaxed`] ordering on a
//! `static`, and one write per tile per counter rather than one per shape. The
//! sentence that reports these numbers is written by `squallar-app`.
//!
//! # The denominators — four, and no two of them are added
//!
//! * [`Totals::mesh_vertices_placed`] — **vertices of tessellated fills that
//!   `ShapeOrText::placed` copied on the frame thread**. The figure this whole
//!   mechanism exists to take to zero on a plan-view frame. It is *not* zero
//!   on a floor-strip pass: the 3D floor keeps the CPU path deliberately (the
//!   mirror swaps callbacks for empty meshes, so a callback tile would not
//!   reach the floor at all).
//! * [`Totals::path_points_placed`] — **points of stroked lines that
//!   `placed` copied on the frame thread**. The stroke half of the first
//!   figure, and it goes to zero on the same frames, for the same reason:
//!   since 2026-09-01 strokes are pre-tessellated too. Reported beside the
//!   fills rather than folded into them, because a single "ground vertices"
//!   figure would read as the whole of the ground phase and it is about half
//!   of it. A tile flattened at another `pixels_per_point` puts its paths
//!   back here until the re-flatten lands, so this is the figure a display
//!   change moves.
//! * [`Totals::label_anchors_placed`] — **label anchors the ground phase
//!   deferred**. The non-triviality conjunct under the first figure: a zero
//!   there means the fills went to the GPU only if this is still positive; if
//!   both are zero the tile pass did not run.
//! * [`Totals::label_solves`] — **times the label phase actually ran**, one
//!   per pane per frame whose names or glyph raster had moved since that
//!   pane last drew them. Its denominator is FRAMES-AND-PANES and the figure
//!   above it is ANCHORS, so the two are never divided into one another; what
//!   they say together is how much of the deferred work the memo removed. A
//!   figure equal to the pane-frames the leg drew is a memo that never
//!   answers; see `squallar_egui::label_cache`.
//! * [`Totals::mesh_draws`] — **fill runs handed to the renderer**, one per
//!   run per tile per frame. The floor under the first figure's zero. Its
//!   denominator is RUNS, and since 2026-09-10 a run is no longer a callback:
//!   see [`Totals::ground_callbacks`].
//! * [`Totals::ground_callbacks`] — **paint callbacks the ground phase handed
//!   the painter for those runs**. Its denominator is LAYER PASSES AND
//!   FRAMES, one per hand-over of a `crate::tile_mesh::GroundBatch`, so it is
//!   never divided into or added to the two run figures. Measured on a
//!   1920x1080 pane at zoom 6 over 45 vector cells: **45 before the batch, 1
//!   after**, with the run figures unmoved at 90 fills and 45 strokes.
//! * [`Totals::ground_shapes`] — **shapes the ground phase handed the
//!   painter**, and so what reaches `Context::tessellate` from the ground.
//!   It does **not** count the batch: a callback held for
//!   `crate::tile_mesh::GroundBatch` leaves the tile's own list, so a tile
//!   that places nothing else reports zero here and its geometry is in
//!   [`Totals::ground_callbacks`] instead.
//!   The parent the two figures below are cuts of. Its denominator is TILES
//!   AND FRAMES; it is never divided into a vertex figure.
//! * [`Totals::ground_shape_slots`] — **slots the walk reserved to hold
//!   them**, over the same tiles and frames, so the two divide into one
//!   another and the quotient is what a frame asked the allocator for against
//!   what it used. It is the figure the walk's `Vec::with_capacity` sizing
//!   moves and `ground_shapes` is the one it must not.
//! * [`Totals::stroke_run_meshes`] and [`Totals::stroke_mesh_vertices`] —
//!   **stroke runs a painterless pass drew from the buffers they were already
//!   tessellated into**, and the vertices those meshes carried. A floor strip
//!   is the pass that ships. These are what `path_points_placed` fell to, and
//!   the two are never added: one counts path points, the other tessellated
//!   vertices.
//! * [`Totals::stroke_draws`] — **paint callbacks pushed for stroke runs**,
//!   likewise one per run per tile per frame, and the floor under the
//!   *second* figure's zero. A stroke run covers a span of consecutive paths,
//!   so this is far below the shape count and is not comparable to it.
//!
//! * [`Totals::raster_quads`] and [`Totals::raster_quad_meshes`] — **raster
//!   tile cells the grid walk placed**, and the shapes it handed the painter
//!   to draw them. Its denominator is CELLS AND FRAMES; it is never added to
//!   `ground_shapes`, which counts a vector tile's shapes. The two here
//!   divide into one another, and the quotient is how many cells one atlas
//!   page held in a row: one shape per consecutive run of one texture is what
//!   `crate::tile_mesh::RasterQuads` moved the second figure to, and the
//!   first is what it must not move.
//!
//! A fifth pair, [`Totals::mesh_uploads`] and [`Totals::mesh_upload_bytes`],
//! is written by the **renderer**, not by this crate, and counts buffer
//! writes rather than frames: the upload-once claim is the two of them against
//! [`Totals::mesh_draws`]. [`Totals::mesh_resident_bytes`] is a level, not a
//! total — what the store is holding right now.

use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

static MESH_VERTICES_PLACED: AtomicU64 = AtomicU64::new(0);
static PATH_POINTS_PLACED: AtomicU64 = AtomicU64::new(0);
static LABEL_ANCHORS_PLACED: AtomicU64 = AtomicU64::new(0);
static LABEL_SOLVES: AtomicU64 = AtomicU64::new(0);
static MESH_DRAWS: AtomicU64 = AtomicU64::new(0);
static STROKE_DRAWS: AtomicU64 = AtomicU64::new(0);
static STROKE_RUN_MESHES: AtomicU64 = AtomicU64::new(0);
static STROKE_MESH_VERTICES: AtomicU64 = AtomicU64::new(0);
static GROUND_CALLBACKS: AtomicU64 = AtomicU64::new(0);
static GROUND_SHAPES: AtomicU64 = AtomicU64::new(0);
static GROUND_SHAPE_SLOTS: AtomicU64 = AtomicU64::new(0);
static RASTER_QUADS: AtomicU64 = AtomicU64::new(0);
static RASTER_QUAD_MESHES: AtomicU64 = AtomicU64::new(0);
static MESH_UPLOADS: AtomicU64 = AtomicU64::new(0);
static MESH_UPLOAD_BYTES: AtomicU64 = AtomicU64::new(0);
static MESH_EVICTIONS: AtomicU64 = AtomicU64::new(0);
static MESH_RESIDENT_BYTES: AtomicU64 = AtomicU64::new(0);
static MESH_STORE_MISSING: AtomicU64 = AtomicU64::new(0);

/// A reading of every counter, taken together.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Totals {
    pub mesh_vertices_placed: u64,
    pub path_points_placed: u64,
    pub label_anchors_placed: u64,
    /// Label phases run: one per pane per frame that could not be answered
    /// from the kept solve. A **count of phases**, never of labels.
    pub label_solves: u64,
    pub mesh_draws: u64,
    pub stroke_draws: u64,
    /// **Paint callbacks the ground phase handed the painter for run spans.**
    /// Its denominator is LAYER PASSES AND FRAMES -- one per hand-over of a
    /// `crate::tile_mesh::GroundBatch`, however many tiles' spans that batch
    /// held -- so it is never divided into, added to or subtracted from
    /// [`Self::mesh_draws`] and [`Self::stroke_draws`], whose denominator is
    /// RUNS. What the three say together is how many boundaries a frame
    /// records for the runs it draws: the run figures are what a cut here
    /// must not move, and this is the one it does.
    pub ground_callbacks: u64,
    /// Stroke runs a pass with no painter drew from the buffers they were
    /// already tessellated into, rather than by putting their paths back
    /// through epaint. One per run per tile per frame; see
    /// [`Totals::stroke_mesh_vertices`] for what they cost.
    pub stroke_run_meshes: u64,
    /// **Vertices those meshes carry.** The relocation half of the pair: the
    /// figure [`Totals::path_points_placed`] must not simply have moved into.
    /// The two are different quantities with different denominators -- path
    /// POINTS against tessellated VERTICES, four or three of the second per
    /// one of the first on epaint's two branches -- so they are never
    /// subtracted from one another. What they say together is that the
    /// per-frame work fell from tessellating the points to copying the
    /// vertices.
    pub stroke_mesh_vertices: u64,
    /// **Shapes the ground phase handed the painter**, over every tile of
    /// every pane: the parent total the two figures above are cuts of, and
    /// what actually reaches `Context::tessellate` from the ground. Counted
    /// once per tile off the length of the list, so a callback, a mesh and a
    /// path each count one. A cut that moved work rather than removing it
    /// leaves this unchanged.
    pub ground_shapes: u64,
    /// **Slots the ground phase reserved for those shapes**, over the same
    /// tiles and frames [`Self::ground_shapes`] counts, so the two divide into
    /// one another and the quotient is what the frame asked the allocator for
    /// against what it used.
    ///
    /// The vector is `Vec::with_capacity`-ed once per drawn tile and handed to
    /// `Painter::extend`, which consumes it, so this is a buffer minted and
    /// released every frame. Sized from the tile's shape list it was
    /// `size_of::<egui::Shape>()` times a dense tile's shape count; sized from
    /// the plan (`crate::tile_mesh::TilePlan::shape_slots`) it is what the walk
    /// will place. Against a `ground_shapes` that must not move, this is the
    /// figure the sizing changes.
    pub ground_shape_slots: u64,
    /// **Raster tile cells the grid walk placed**, over every tile layer of
    /// every pane. The raster half of [`Self::ground_shapes`]'s question, and
    /// a separate counter rather than a term in it: that one counts a vector
    /// tile's shapes and these are textured quads, so the two are never
    /// added.
    pub raster_quads: u64,
    /// **Shapes the raster arm handed the painter to draw them**, over the
    /// same cells and frames, so the two divide into one another. One per
    /// consecutive run of one atlas page, so the quotient is how many tiles a
    /// run held -- and the figure `crate::tile_mesh::RasterQuads` moves while
    /// [`Self::raster_quads`] must not. Equal counts are a viewport whose
    /// cells never share a page two in a row.
    pub raster_quad_meshes: u64,
    pub mesh_uploads: u64,
    pub mesh_upload_bytes: u64,
    pub mesh_evictions: u64,
    /// Bytes the renderer is holding for tiles right now — a level, not a
    /// running total, and the only figure here that can go down.
    pub mesh_resident_bytes: u64,
    /// Ground draws that reached a renderer with no store installed and drew
    /// nothing. **Zero on every correct install**, and the counter that makes
    /// a map with no fills in it a number rather than a puzzle: the renderer
    /// crate declares no `log`, so the fault is counted where it happens and
    /// said where a logger exists.
    pub mesh_store_missing: u64,
}

/// Raster cells one tile pass placed, and the shapes it handed the painter
/// for them. One call per tile layer per pane per frame.
pub fn note_raster_quads(quads: u64, meshes: u64) {
    RASTER_QUADS.fetch_add(quads, Relaxed);
    RASTER_QUAD_MESHES.fetch_add(meshes, Relaxed);
}

/// Fill vertices this tile placed on the CPU. One call per tile.
pub fn note_mesh_vertices_placed(n: u64) {
    MESH_VERTICES_PLACED.fetch_add(n, Relaxed);
}

/// Stroke points this tile placed on the CPU. One call per tile.
pub fn note_path_points_placed(n: u64) {
    PATH_POINTS_PLACED.fetch_add(n, Relaxed);
}

/// Label anchors this tile deferred to the label phase. One call per tile.
pub fn note_label_anchors_placed(n: u64) {
    LABEL_ANCHORS_PLACED.fetch_add(n, Relaxed);
}

/// One pane's label phase ran rather than answering from its kept solve. One
/// call per pane per frame, and only on a frame that solved.
pub fn note_label_solve() {
    LABEL_SOLVES.fetch_add(1, Relaxed);
}

/// Paint callbacks one layer pass handed the painter for its run spans. One
/// call per hand-over, whatever the batch held.
pub fn note_ground_callback() {
    GROUND_CALLBACKS.fetch_add(1, Relaxed);
}

/// Fill runs this tile handed to the renderer. One call per tile.
pub fn note_mesh_draws(n: u64) {
    MESH_DRAWS.fetch_add(n, Relaxed);
}

/// Stroke runs this tile handed to the renderer. One call per tile.
pub fn note_stroke_draws(n: u64) {
    STROKE_DRAWS.fetch_add(n, Relaxed);
}

/// Stroke runs this tile drew from its pre-tessellated buffers, and the
/// vertices they carried. One call per tile.
pub fn note_stroke_run_meshes(runs: u64, vertices: u64) {
    STROKE_RUN_MESHES.fetch_add(runs, Relaxed);
    STROKE_MESH_VERTICES.fetch_add(vertices, Relaxed);
}

/// Shapes this tile handed the painter, and the slots it reserved for them.
/// One call per tile.
pub fn note_ground_shapes(n: u64, slots: u64) {
    GROUND_SHAPES.fetch_add(n, Relaxed);
    GROUND_SHAPE_SLOTS.fetch_add(slots, Relaxed);
}

/// One tile's buffers crossed to the GPU. Called by the renderer.
pub fn note_mesh_upload(bytes: u64) {
    MESH_UPLOADS.fetch_add(1, Relaxed);
    MESH_UPLOAD_BYTES.fetch_add(bytes, Relaxed);
}

/// One tile's buffers were released. Called by the renderer.
pub fn note_mesh_eviction(n: u64) {
    MESH_EVICTIONS.fetch_add(n, Relaxed);
}

/// What the renderer is holding for tiles, after this frame's sweep and
/// uploads. Called by the renderer; a level, so it is set rather than added.
pub fn set_mesh_resident_bytes(bytes: u64) {
    MESH_RESIDENT_BYTES.store(bytes, Relaxed);
}

/// One ground draw found no store to draw through. Called by the renderer.
pub fn note_mesh_store_missing() {
    MESH_STORE_MISSING.fetch_add(1, Relaxed);
}

impl Totals {
    /// How far along this ledger is, as one number, so a caller can tell
    /// "nothing has happened since I last looked" in a single compare. The
    /// resident level is deliberately out of it: it is not monotonic, and a
    /// sweep that only released bytes is not news the reporter has to wake
    /// for.
    fn progress(&self) -> u64 {
        self.mesh_vertices_placed
            .wrapping_add(self.path_points_placed)
            .wrapping_add(self.label_anchors_placed)
            .wrapping_add(self.label_solves)
            .wrapping_add(self.mesh_draws)
            .wrapping_add(self.stroke_draws)
            .wrapping_add(self.stroke_run_meshes)
            .wrapping_add(self.ground_shapes)
            .wrapping_add(self.ground_shape_slots)
            .wrapping_add(self.raster_quads)
            .wrapping_add(self.raster_quad_meshes)
            .wrapping_add(self.mesh_uploads)
            .wrapping_add(self.mesh_evictions)
            .wrapping_add(self.mesh_store_missing)
    }
}

/// The last [`Totals::progress`] a caller was handed by [`totals_if_moved`].
static REPORTED: AtomicU64 = AtomicU64::new(0);

/// [`totals`], but only when something has happened since the last time this
/// was asked — the telemetry writer's read, so an idle app writes no line.
pub fn totals_if_moved() -> Option<Totals> {
    let totals = totals();
    let progress = totals.progress();
    if REPORTED.swap(progress, Relaxed) == progress {
        return None;
    }
    Some(totals)
}

/// Read every counter.
pub fn totals() -> Totals {
    Totals {
        mesh_vertices_placed: MESH_VERTICES_PLACED.load(Relaxed),
        path_points_placed: PATH_POINTS_PLACED.load(Relaxed),
        label_anchors_placed: LABEL_ANCHORS_PLACED.load(Relaxed),
        label_solves: LABEL_SOLVES.load(Relaxed),
        mesh_draws: MESH_DRAWS.load(Relaxed),
        stroke_draws: STROKE_DRAWS.load(Relaxed),
        stroke_run_meshes: STROKE_RUN_MESHES.load(Relaxed),
        stroke_mesh_vertices: STROKE_MESH_VERTICES.load(Relaxed),
        ground_callbacks: GROUND_CALLBACKS.load(Relaxed),
        ground_shapes: GROUND_SHAPES.load(Relaxed),
        ground_shape_slots: GROUND_SHAPE_SLOTS.load(Relaxed),
        raster_quads: RASTER_QUADS.load(Relaxed),
        raster_quad_meshes: RASTER_QUAD_MESHES.load(Relaxed),
        mesh_uploads: MESH_UPLOADS.load(Relaxed),
        mesh_upload_bytes: MESH_UPLOAD_BYTES.load(Relaxed),
        mesh_evictions: MESH_EVICTIONS.load(Relaxed),
        mesh_resident_bytes: MESH_RESIDENT_BYTES.load(Relaxed),
        mesh_store_missing: MESH_STORE_MISSING.load(Relaxed),
    }
}

/// Put every counter back to zero.
///
/// For tests that read a window rather than a running total. Nothing shipped
/// calls it: the reported line is cumulative from boot, and a windowed reading
/// is the difference of two.
#[cfg(test)]
pub(crate) fn reset() {
    for counter in [
        &MESH_VERTICES_PLACED,
        &PATH_POINTS_PLACED,
        &LABEL_ANCHORS_PLACED,
        &LABEL_SOLVES,
        &MESH_DRAWS,
        &STROKE_DRAWS,
        &GROUND_CALLBACKS,
        &STROKE_RUN_MESHES,
        &STROKE_MESH_VERTICES,
        &GROUND_SHAPES,
        &GROUND_SHAPE_SLOTS,
        &RASTER_QUADS,
        &RASTER_QUAD_MESHES,
        &MESH_UPLOADS,
        &MESH_UPLOAD_BYTES,
        &MESH_EVICTIONS,
        &MESH_RESIDENT_BYTES,
        &MESH_STORE_MISSING,
        &REPORTED,
    ] {
        counter.store(0, Relaxed);
    }
}
