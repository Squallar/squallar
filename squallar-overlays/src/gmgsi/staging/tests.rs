//! The GMGSI instance of the pool, at the shipped width **and at the width the
//! product actually publishes**.
//!
//! `crate::staging::tests` holds the pool's invariants at a toy width; what is
//! checked here is what only the instance can be wrong about — the two doors a
//! raster leaves the layer by reach the slot, and a granule of the shape NOAA
//! is publishing today is pooled rather than refused. 15,000,000 `u8` is
//! 15 MB, so these allocate for real.
//!
//! **Why a second width is here at all.** The committed fixture is 5000 wide
//! and so was every assertion in this file, so a slot keyed on `3000 * 5000`
//! passed everything while reusing nothing on a real granule. A suite whose
//! fixture is the constant cannot notice the constant has stopped describing
//! the product.

use super::*;
use crate::hrrr::GridCoords;
use squallar_source::product::FieldId;

/// The mosaic width **every GMGSI granule dated 2026-09-03 carries**, on all
/// four channels — 24 granules read plus two re-fetched independently. The
/// 2025-06 and 2026-07 granules are 5000 wide; the product moved.
const PUBLISHED_NX: usize = 4999;
const PUBLISHED_NY: usize = 3000;
/// 14,997,000 points, 14,997,000 B — 3,000 B under [`STAGING_POINTS`], and the
/// whole reason the pool reused nothing.
const PUBLISHED_POINTS: usize = PUBLISHED_NY * PUBLISHED_NX;

/// A raster of `ny` x `nx` whose buffer is capacity-exact — what the decode
/// hands over.
fn grid_of(ny: usize, nx: usize) -> ResidentGrid {
    let mut values: Vec<u8> = Vec::new();
    values
        .try_reserve_exact(ny * nx)
        .expect("a mosaic buffer fits on a test host");
    grid_from(values, ny, nx)
}

/// The same, around a buffer the caller already has — so a test can follow one
/// block out of the slot, through a raster, and back in.
fn grid_from(mut values: Vec<u8>, ny: usize, nx: usize) -> ResidentGrid {
    values.clear();
    values.resize(ny * nx, 82);
    ResidentGrid {
        field: FieldId::from_static("GmgsiLongwaveIr"),
        ni: nx,
        nj: ny,
        coords: GridCoords::Separable {
            lat_axis: vec![0.0; ny],
            lon_axis: vec![0.0; nx],
        },
        values: GridValues::Bytes(
            crate::render::gridded::ByteCodes::new(values, Vec::new())
                .expect("a raster with no absent points is a byte store"),
        ),
    }
}

/// A mosaic-shaped raster at the width this build's budgets are sized for.
fn mosaic_grid() -> ResidentGrid {
    grid_of(3000, 5000)
}

/// The nominal figure is what the handler's budgets are priced at, and it is
/// **not** what the slot is keyed on.
#[test]
fn the_nominal_shape_prices_the_budgets_and_does_not_key_the_slot() {
    assert_eq!(STAGING_POINTS, 3000 * 5000);
    assert_eq!(global().nominal_points(), STAGING_POINTS);
    // **What the slot now holds**: one byte a point, because a GMGSI value is
    // a byte reading. This is the figure every byte the layer parks is spent
    // against, and `retained_bytes` derives from it.
    assert_eq!(STAGING_POINTS * size_of::<u8>(), 15_000_000);
    // **What the handler still prices it at**, unchanged by the narrowing:
    // `GLOBAL_GRID_BYTES` is `GRID_POINTS * size_of::<f32>()` and is spelled in
    // `render::handlers::gmgsi`, whose budgets are not this module's to move.
    // The two differing by 4x is the budget over-provisioning, never
    // under-counting — `GridValues::resident_bytes` reports the real 15 MB, so
    // the caches evict against what the heap is carrying and not against this.
    assert_eq!(STAGING_POINTS * size_of::<f32>(), 60_000_000);
    // And the figure the nominal shape over-provisions by against the width
    // the product publishes, stated so a later reader does not have to
    // re-derive whether it matters: 0.02 % of one grid, at either width.
    assert_eq!(STAGING_POINTS - PUBLISHED_POINTS, 3000);
    assert_eq!((STAGING_POINTS - PUBLISHED_POINTS) * size_of::<u8>(), 3_000);
}

/// **The shipping defect, at the width the product publishes.**
///
/// A pool declared at `3000 * 5000` and handed a `3000 * 4999` granule reused
/// nothing and accepted nothing back: `take` compared the request against the
/// constant and `give` compared the offered capacity against it, so every
/// decode allocated a fresh 60 MB block and every one was freed again — the
/// exact churn the pool exists to remove, with the pool in place, on every
/// granule of the product since 2026-09-03.
///
/// Observed red before the fix, this assertion: `reused` 0, `declined` 1,
/// `retained_bytes` 0, `health` `Inert`.
#[test]
fn a_granule_at_the_width_the_product_publishes_is_pooled() {
    static POOL: StagingPool = StagingPool::new(STAGING_POINTS);
    assert_eq!(POOL.health(), StagingHealth::Cold);

    let first = POOL.take(PUBLISHED_POINTS).expect("a cold pool allocates");
    let address = first.as_ptr() as usize;
    assert_eq!(first.capacity(), PUBLISHED_POINTS);
    assert_eq!(
        POOL.totals().allocated,
        1,
        "premise: the first one is fresh"
    );

    // That very buffer back through the door an eviction uses, so what the
    // next granule is handed can be compared block for block.
    recycle(&POOL, grid_from(first, PUBLISHED_NY, PUBLISHED_NX));
    assert_eq!(
        POOL.totals().declined,
        0,
        "a granule of the shape NOAA publishes must not be refused by the slot \
         because a constant in this build says 5000",
    );
    assert_eq!(
        POOL.retained_points(),
        PUBLISHED_POINTS,
        "and the slot must describe the block it is actually holding",
    );
    assert_eq!(
        POOL.retained_bytes(),
        PUBLISHED_POINTS * size_of::<u8>(),
        "which is 14,997,000 B and not the nominal 15,000,000",
    );

    let second = POOL
        .take(PUBLISHED_POINTS)
        .expect("the slot holds the published shape");
    assert_eq!(
        POOL.totals().reused,
        1,
        "the next granule of the same product must be handed that block",
    );
    assert_eq!(
        POOL.health(),
        StagingHealth::Reusing,
        "and the pool must read as working rather than as merely untouched",
    );
    assert_eq!(
        second.as_ptr() as usize,
        address,
        "and it must be the FIRST one's block, not a fresh allocation of the \
         same size: a pool that reallocated would satisfy every count above \
         and leave the fragmentation it exists to remove exactly as it was",
    );
    assert_eq!(second.capacity(), PUBLISHED_POINTS);
    assert!(second.is_empty(), "and arrive with nothing in it");
    drop(second);

    assert_eq!(
        POOL.nominal_points(),
        STAGING_POINTS,
        "with the budget figure untouched: it prices the cache, not the slot",
    );
}

/// A granule at the shape this build was written for is still pooled, so the
/// test above is a *difference* and not a claim that the pool only works off
/// its nominal.
#[test]
fn a_granule_at_the_nominal_shape_is_pooled_too() {
    static POOL: StagingPool = StagingPool::new(STAGING_POINTS);
    recycle(&POOL, mosaic_grid());
    let staged = POOL.take(STAGING_POINTS).expect("the slot took it");
    assert_eq!(staged.capacity(), STAGING_POINTS);
    assert_eq!(POOL.totals().reused, 1);
}

/// **The product's width moving mid-process costs one block, not the pool.**
///
/// The slot follows the granule: the buffer for the width nobody is publishing
/// any more is dropped, the arriving width becomes the retained one, and the
/// change is counted where an operator can read it. Holding the old block
/// instead is what left the shipped pool inert.
#[test]
fn a_width_change_hands_the_slot_over_and_says_it_did() {
    static POOL: StagingPool = StagingPool::new(STAGING_POINTS);
    recycle(&POOL, mosaic_grid());
    assert_eq!(POOL.retained_points(), STAGING_POINTS, "premise: 5000 wide");

    // The first granule of the new width.
    let fresh = POOL.take(PUBLISHED_POINTS).expect("allocates its own");
    assert_eq!(fresh.capacity(), PUBLISHED_POINTS);
    assert_eq!(
        POOL.resizes(),
        1,
        "the width change is one counted event, not a silent decline",
    );
    assert_eq!(POOL.retained_points(), 0, "and the old block is let go");
    drop(fresh);

    recycle(&POOL, grid_of(PUBLISHED_NY, PUBLISHED_NX));
    let staged = POOL
        .take(PUBLISHED_POINTS)
        .expect("the new width is pooled");
    assert_eq!(
        POOL.totals().reused,
        1,
        "so the cost of the change is one block, once, and not one per granule \
         for the life of the process",
    );
    assert_eq!(POOL.resizes(), 1, "and it is not counted again");
    drop(staged);
}

/// An owned raster's values go straight into the slot.
#[test]
fn an_owned_raster_is_recycled() {
    static POOL: StagingPool = StagingPool::new(STAGING_POINTS);
    recycle(&POOL, mosaic_grid());
    assert_eq!(
        POOL.totals(),
        StagingTotals {
            allocated: 0,
            reused: 0,
            declined: 0
        },
    );
    let staged = POOL.take(STAGING_POINTS).expect("the slot took it");
    assert_eq!(staged.capacity(), STAGING_POINTS);
    assert!(staged.is_empty());
    assert_eq!(POOL.totals().reused, 1);
}

/// **A raster something else is still reading is not taken apart.**
///
/// `recycle_shared` reclaims through `Arc::into_inner`, so a granule whose
/// raster job still holds a refcount drops normally and is counted as
/// declined. Prising the values out from under a live job would be a
/// use-after-free wearing a pool's clothes.
#[test]
fn a_raster_another_reference_still_holds_is_declined_not_reclaimed() {
    // The reclaimed case first, so the assertion below is a *difference* and
    // not a claim that `recycle_shared` never works.
    static SOLE: StagingPool = StagingPool::new(STAGING_POINTS);
    recycle_shared(&SOLE, Arc::new(mosaic_grid()));
    assert_eq!(
        SOLE.totals().declined,
        0,
        "premise: a sole reference is reclaimed"
    );
    assert_eq!(SOLE.retained_bytes(), STAGING_POINTS * size_of::<u8>());

    static SHARED: StagingPool = StagingPool::new(STAGING_POINTS);
    let grid = Arc::new(mosaic_grid());
    let still_reading = Arc::clone(&grid);
    recycle_shared(&SHARED, grid);
    assert_eq!(
        SHARED.totals(),
        StagingTotals {
            allocated: 0,
            reused: 0,
            declined: 1
        },
        "a raster a job is still reading is left alone, and the pool says so \
         instead of pretending it recycled",
    );
    assert_eq!(SHARED.retained_bytes(), 0);
    let GridValues::Bytes(kept) = &still_reading.values else {
        panic!("the fixture's raster is a byte store");
    };
    assert_eq!(
        kept.codes().len(),
        STAGING_POINTS,
        "and the reference that kept it alive still has its buffer",
    );
}
