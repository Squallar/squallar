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
//!   `placed` copied on the frame thread**. These stay on the CPU by design;
//!   see the module doc of [`super`] for why. Reported beside the fills rather
//!   than folded into them, because a single "ground vertices" figure would
//!   read as the whole of the ground phase and it is about half of it.
//! * [`Totals::label_anchors_placed`] — **label anchors the ground phase
//!   deferred**. The non-triviality conjunct under the first figure: a zero
//!   there means the fills went to the GPU only if this is still positive; if
//!   both are zero the tile pass did not run.
//! * [`Totals::mesh_draws`] — **paint callbacks pushed for fill runs**, one
//!   per run per tile per frame. The floor under the first figure's zero.
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
static MESH_DRAWS: AtomicU64 = AtomicU64::new(0);
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
    pub mesh_draws: u64,
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

/// Fill runs this tile handed to the renderer. One call per tile.
pub fn note_mesh_draws(n: u64) {
    MESH_DRAWS.fetch_add(n, Relaxed);
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
            .wrapping_add(self.mesh_draws)
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
        mesh_draws: MESH_DRAWS.load(Relaxed),
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
        &MESH_DRAWS,
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
