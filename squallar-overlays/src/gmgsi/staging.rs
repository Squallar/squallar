//! **The one staging buffer a GMGSI mosaic is decoded into, retained between
//! granules.**
//!
//! [`crate::staging`] is the pool; this is its GMGSI instance and the two doors
//! a granule leaves the layer by. The account of *why* — a wasm32 heap that
//! only grows, dlmalloc that cannot coalesce across a live block, a 98 MB
//! request failing with 192 MB free — is in [`crate::mrms::staging`], and it
//! applies here unchanged: a GMGSI mosaic is 3000 x 5000 points = 15,000,000 B
//! at the byte a point its values are (60,000,000 B while it was held as
//! `f32`), GMGSI is a loop layer that decodes one granule per frame, and until
//! this module every granule built a fresh raster and freed the last one.
//!
//! Measured on the committed granule before the pool (debug build,
//! `tests/gmgsi_staging_blocks.rs`'s own counter, 2026-09-04): **91 blocks of
//! exactly 60,000,000 B per decode** — the raster, `hdf5_pure`'s storage-width
//! copy and its assembled stored bytes, plus **88** from the two 2-D
//! coordinate variables, whose single 60 MB chunk was re-inflated for each of
//! the 44 row windows the axis walk read. After it, same build and granule,
//! in the steady state: **1** per decode, the stored bytes `hdf5_pure`
//! assembles for `data`, transient; the raster comes out of the slot and the
//! coordinate arrays are not read at all ([`super::decode::AxisCache`]). That
//! one read is what `hdf5_pure` 0.44's public API leaves — see
//! [`squallar_netcdf::Granule::read_unpacked_f32_into`].

use std::sync::Arc;

use crate::render::gridded::{GridValues, ResidentGrid};

/// **The mosaic width this build's budgets were sized for**, in `f32` —
/// [`super::GRID_POINTS`].
///
/// **Not the slot's capacity.** It was, and that was the defect: the product
/// now publishes `[1, 3000, 4999]` and a slot keyed on 15,000,000 reused
/// nothing and accepted nothing back. [`crate::staging`] takes its one capacity
/// from the granule that hands a buffer back, and this figure is what it
/// reports as [`crate::staging::StagingPool::nominal_points`] — the reference a
/// [`crate::staging::StagingPool::retained_points`] reading is compared against.
pub const STAGING_POINTS: usize = super::GRID_POINTS;

/// The pool over one GMGSI mosaic, by element.
///
/// **`u8`, because a GMGSI value is a byte.** The slot holds the raster the
/// decode fills and `render::gridded::GridValues::Bytes` keeps, so the block
/// this module parks is 15,000,000 B where it was 60,000,000 B — the same one
/// block, a quarter of it. `STAGING_POINTS` is a count of *points* and is
/// unchanged by that; what changed is the width one point occupies.
pub type StagingPool = crate::staging::StagingPool<u8>;

pub use crate::staging::{StagingHealth, StagingTotals};

/// The process-wide staging area — what every shipped decode uses.
///
/// One slot for the whole application, not one per handler or one per thread,
/// for the reason MRMS gives: the live fetch and the loop's frame fetch are
/// exactly the two callers that must share the one slot the budget names.
static GLOBAL: StagingPool = StagingPool::new(STAGING_POINTS);

/// See [`GLOBAL`].
pub fn global() -> &'static StagingPool {
    &GLOBAL
}

/// Take a decoded raster's values back into `pool`.
///
/// The raster is owned outright here — the decode hands its [`ResidentGrid`]
/// over by value — so there is nothing to contend with.
pub fn recycle(pool: &StagingPool, grid: ResidentGrid) {
    match grid.values {
        GridValues::Bytes(codes) => pool.give(codes.into_codes()),
        // A granule the decode could not narrow — one value that is not a byte
        // code, or more absent points than the store carries — holds an `f32`
        // raster that is not this pool's element at all, and a `Scaled` grid
        // never comes from here. Counted rather than silently dropped, for the
        // same reason `give` reports a wrong capacity: a slot that stops being
        // refilled must say so rather than read like a slot nobody used.
        GridValues::F32(_) | GridValues::Scaled(_) => pool.decline(),
    }
}

/// [`recycle`] for a raster the layer holds behind an `Arc`, if this is the
/// last reference to it.
///
/// `Arc::into_inner` rather than a clone-and-drop: a grid whose raster job is
/// still in flight is genuinely still in use, and prising the values out from
/// under it would be a use-after-free by another name. That case is counted as
/// declined and the grid drops normally.
pub fn recycle_shared(pool: &StagingPool, grid: Arc<ResidentGrid>) {
    match Arc::into_inner(grid) {
        Some(grid) => recycle(pool, grid),
        None => pool.decline(),
    }
}

#[cfg(test)]
mod tests;
