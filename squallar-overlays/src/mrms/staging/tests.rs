//! The pool's own invariants, over buffers rather than over granules.
//!
//! The *decode* gates live in `tests/mrms_staging.rs`, where a counting global
//! allocator watches the real shipped path. What is checked here is the half
//! that decides whether a retained buffer can ever hand a grid the wrong bytes,
//! and it is checked at the sizes a shipped mosaic uses — 24.5 M `f32` is
//! 98 MB, so these allocate for real rather than at a toy width.

use super::*;

/// A capacity-exact mosaic buffer, empty.
fn mosaic_buffer() -> Vec<f32> {
    let mut v: Vec<f32> = Vec::new();
    v.try_reserve_exact(STAGING_POINTS)
        .expect("a mosaic buffer fits on a test host");
    v
}

/// **The whole point, at the pool's own level**: hand a buffer back and the
/// next mosaic-sized decode is given that buffer instead of a new one.
#[test]
fn a_returned_mosaic_buffer_is_the_next_mosaic_decodes_buffer() {
    let pool = StagingPool::new();

    let first = pool.take(STAGING_POINTS).expect("a cold pool allocates");
    let address = first.as_ptr() as usize;
    assert_eq!(
        pool.totals(),
        StagingTotals {
            allocated: 1,
            reused: 0,
            declined: 0
        },
        "a cold pool has nothing to hand out and must say so",
    );

    pool.give(first);
    let second = pool.take(STAGING_POINTS).expect("the slot is full");
    assert_eq!(
        second.as_ptr() as usize,
        address,
        "the second mosaic must be decoded into the FIRST one's block. A pool \
         that answered with a fresh allocation of the same size would satisfy \
         every capacity assertion in this file and leave the fragmentation \
         this module exists to remove exactly as it was",
    );
    assert_eq!(
        pool.totals(),
        StagingTotals {
            allocated: 1,
            reused: 1,
            declined: 0
        },
    );
    assert!(
        second.is_empty(),
        "a buffer comes out of the slot with nothing in it, whatever it held",
    );
    assert_eq!(
        second.capacity(),
        STAGING_POINTS,
        "and at the full mosaic capacity, so the decode's reserve is a no-op",
    );
}

/// **A grid that is not mosaic-shaped never touches the retained buffer.**
///
/// The invariant that stops a pooled buffer from being a memory bug in the
/// other direction: `MrmsGrid::resident_bytes` — the figure both byte budgets
/// are spent against — is `len * 4`, so a 400-byte grid handed the 98 MB block
/// would report 400 bytes while holding 98 MB, and the cache would go on
/// filling until the tab died. Matching capacity *exactly* rather than "≥" is
/// what forbids it.
#[test]
fn a_grid_of_another_shape_is_never_given_the_mosaic_buffer() {
    let pool = StagingPool::new();
    pool.give(mosaic_buffer());

    let small = pool.take(1024).expect("a small grid always allocates");
    assert_eq!(
        small.capacity(),
        1024,
        "a 1024-point grid must be given a 1024-point buffer, not the mosaic's",
    );
    assert_eq!(
        pool.totals(),
        StagingTotals {
            allocated: 1,
            reused: 0,
            declined: 0
        },
        "and the mosaic buffer must still be in the slot, untouched",
    );

    let mosaic = pool.take(STAGING_POINTS).expect("the slot still holds one");
    assert_eq!(pool.totals().reused, 1, "which the next mosaic then takes");
    drop((small, mosaic));
}

/// And the same rule on the way in: a buffer of any other capacity is refused
/// rather than kept, so the slot can only ever hold something a mosaic decode
/// is allowed to be handed.
#[test]
fn a_buffer_of_another_capacity_is_refused_by_the_slot() {
    let pool = StagingPool::new();
    let mut odd: Vec<f32> = Vec::new();
    odd.try_reserve_exact(STAGING_POINTS - 1).expect("fits");
    pool.give(odd);
    assert_eq!(
        pool.totals(),
        StagingTotals {
            allocated: 0,
            reused: 0,
            declined: 1
        },
    );

    let fresh = pool.take(STAGING_POINTS).expect("so the slot was empty");
    assert_eq!(pool.totals().allocated, 1, "and the next mosaic allocates");
    drop(fresh);
}

/// **One slot, not a free list.** The second buffer offered while the first is
/// still waiting is dropped, because the budget this pool implements is
/// `FRAME_STAGING_BYTES` — *one* mosaic — and a pool that grew without bound
/// would be a memory leak sold as a fix.
#[test]
fn the_slot_holds_one_mosaic_and_refuses_the_second() {
    let pool = StagingPool::new();
    pool.give(mosaic_buffer());
    pool.give(mosaic_buffer());
    assert_eq!(
        pool.totals(),
        StagingTotals {
            allocated: 0,
            reused: 0,
            declined: 1
        },
        "the second offer is declined, and says so rather than growing the pool",
    );
}

/// **A grid something else is still reading is not taken apart.**
///
/// `recycle` reclaims through `Arc::into_inner`, so a granule whose raster job
/// still holds a refcount drops normally and is counted as declined. Prising
/// the values out from under a live job would be a use-after-free wearing a
/// pool's clothes.
#[test]
fn a_grid_another_reference_still_holds_is_declined_not_reclaimed() {
    // The reclaimed case first, so the assertion below is a *difference* and
    // not a claim that `recycle` never works.
    let pool = StagingPool::new();
    pool.recycle(mosaic_grid());
    assert_eq!(
        pool.totals(),
        StagingTotals {
            allocated: 0,
            reused: 0,
            declined: 0
        },
        "premise: the sole reference to a mosaic-sized grid is reclaimed",
    );
    assert_eq!(
        pool.take(STAGING_POINTS)
            .expect("the slot took it")
            .capacity(),
        STAGING_POINTS,
    );
    assert_eq!(pool.totals().reused, 1);

    let pool = StagingPool::new();
    let grid = mosaic_grid();
    let still_reading = std::sync::Arc::clone(&grid.grid);
    pool.recycle(grid);
    assert_eq!(
        pool.totals(),
        StagingTotals {
            allocated: 0,
            reused: 0,
            declined: 1
        },
        "a grid a raster job is still reading is left alone, and the pool says \
         so instead of pretending it recycled",
    );
    assert_eq!(
        still_reading.values.capacity(),
        STAGING_POINTS,
        "and the reference that kept it alive still has its buffer",
    );
    assert_eq!(
        pool.take(STAGING_POINTS).expect("allocates").capacity(),
        STAGING_POINTS,
    );
    assert_eq!(
        pool.totals().allocated,
        1,
        "the slot was left empty, so the next mosaic allocated",
    );
}

/// A one-reference [`MrmsGrid`] whose values are a full mosaic buffer — what
/// the frame cache hands `recycle` on every eviction.
fn mosaic_grid() -> crate::mrms::MrmsGrid {
    let product = crate::mrms::MrmsProduct::ReflectivityComposite;
    crate::mrms::MrmsGrid {
        product,
        grid: std::sync::Arc::new(crate::render::gridded::ResidentGrid {
            field: crate::mrms::fields::spec(product).id.clone(),
            ni: 2,
            nj: 2,
            coords: crate::hrrr::GridCoords::Regular {
                lat0: 0.0,
                lon0: 0.0,
                dlat: 1.0,
                dlon: 1.0,
                ni: 2,
                nj: 2,
                scan_mode: 0,
            },
            values: mosaic_buffer(),
        }),
        bounds: squallar_geo::GeoBounds::from_points([(0.0, 0.0), (1.0, 1.0)])
            .expect("two points make a box"),
        valid: chrono::NaiveDate::from_ymd_opt(2026, 8, 31)
            .expect("a real date")
            .and_hms_opt(0, 0, 0)
            .expect("a real time"),
        visible_points: 0,
        value_range: None,
    }
}
