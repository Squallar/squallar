//! The GMGSI instance of the pool, at the shipped width.
//!
//! `crate::staging::tests` holds the pool's invariants at a toy width; what is
//! checked here is what only the instance can be wrong about — the slot is
//! sized to the product's grid, and the two doors a raster leaves the layer
//! by reach it. 15,000,000 `f32` is 60 MB, so these allocate for real.

use super::*;
use crate::hrrr::GridCoords;
use squallar_source::product::FieldId;

/// A mosaic-shaped raster whose buffer is capacity-exact — what the decode
/// hands over.
fn mosaic_grid() -> ResidentGrid {
    let mut values: Vec<f32> = Vec::new();
    values
        .try_reserve_exact(STAGING_POINTS)
        .expect("a mosaic buffer fits on a test host");
    values.resize(STAGING_POINTS, 82.0);
    ResidentGrid {
        field: FieldId::from_static("GmgsiLongwaveIr"),
        ni: 5000,
        nj: 3000,
        coords: GridCoords::Separable {
            lat_axis: vec![0.0; 3000],
            lon_axis: vec![0.0; 5000],
        },
        values: GridValues::F32(values),
    }
}

/// The slot is exactly one mosaic, derived from the shape the handler's
/// budgets are derived from.
#[test]
fn the_slot_is_one_mosaic() {
    assert_eq!(STAGING_POINTS, 3000 * 5000);
    assert_eq!(global().points(), STAGING_POINTS);
    // The handler's `GLOBAL_GRID_BYTES` is derived from the same constant and
    // pinned to this figure at compile time, so the slot and the frame
    // cache's budget cannot drift apart.
    assert_eq!(STAGING_POINTS * size_of::<f32>(), 60_000_000);
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
    assert_eq!(SOLE.retained_bytes(), STAGING_POINTS * size_of::<f32>());

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
    let GridValues::F32(kept) = &still_reading.values else {
        panic!("the fixture's raster is f32");
    };
    assert_eq!(
        kept.capacity(),
        STAGING_POINTS,
        "and the reference that kept it alive still has its buffer",
    );
}
