//! The MRMS instance of the pool — its invariants over buffers rather than
//! over granules, **at the CONUS shape and at a shape the CONUS constant does
//! not describe**.
//!
//! The *decode* gates live in `tests/mrms_staging_blocks.rs`, where a counting
//! global allocator watches the real shipped path. What is checked here is the
//! half that decides whether a retained buffer can ever hand a grid the wrong
//! bytes, and it is checked at the sizes a shipped mosaic uses — 24.5 M `u16`
//! is 49 MB, so these allocate for real rather than at a toy width.
//!
//! **Why a second shape is here at all.** Every assertion in this file used to
//! be at [`STAGING_POINTS`], which is also the committed fixture's shape and
//! also the constant the slot was keyed on — three spellings of one number, so
//! nothing here could tell a pool that reuses from a pool keyed on a figure the
//! product has stopped publishing. That is exactly how GMGSI shipped a slot
//! that reused nothing on every real granule with its whole suite green.

use super::*;

/// **The CONUS mosaic NOAA publishes**, and what this build is sized for:
/// 7000 x 3500 = 24,500,000 points. Read off section 3 of one granule per day
/// across 17 dates from 2020-10-14 to 2026-09-04 on both shipped products; it
/// has never been anything else.
const CONUS_POINTS: usize = 7000 * 3500;

/// **One tile-row band of that mosaic** — 16 x 7000 = 112,000 points, which is
/// what the slot holds since `decode::tile_png_codes` stopped building a plane
/// for the tiler to read. [`STAGING_POINTS`] is this; [`CONUS_POINTS`] is the
/// grid it is 1/219th of, and the two being different numbers is what the file
/// header above asks for.
const CONUS_BAND_POINTS: usize = crate::render::gridded::TILE * 7000;

/// **A shape MRMS publishes that this build's constant does not describe** —
/// the Caribbean domain's 3000 x 1500 = 4,500,000 points, read off
/// `CARIB/MergedReflectivityQCComposite_00.50/20260904/` on 2026-09-04. Same
/// product name, same packing (template 3.0, DRT 5.41, 16-bit, no bitmap), a
/// fifth of the points.
///
/// It stands in for the event this module is now proof against and which CONUS
/// has not had: NOAA moving the grid under a build. GMGSI had it on
/// 2026-09-03.
const OFF_NOMINAL_POINTS: usize = 3000 * 1500;

/// A capacity-exact mosaic buffer, empty — in the pool's own width.
fn mosaic_buffer() -> Vec<u16> {
    buffer_of(STAGING_POINTS)
}

/// The same at any point count, so a test can drive a shape the constant does
/// not name.
fn buffer_of(points: usize) -> Vec<u16> {
    let mut v: Vec<u16> = Vec::new();
    v.try_reserve_exact(points)
        .expect("a staging buffer fits on a test host");
    v
}

/// **The nominal figure prices the budgets and does not key the slot.**
#[test]
fn the_nominal_shape_prices_the_budgets_and_does_not_key_the_slot() {
    assert_eq!(STAGING_POINTS, CONUS_BAND_POINTS);
    assert_ne!(
        STAGING_POINTS, CONUS_POINTS,
        "the slot holds a BAND, not a mosaic: the decode tiles out of the row \
         walk and never builds the plane this used to be",
    );
    assert_eq!(StagingPool::new().nominal_points(), STAGING_POINTS);
    assert_eq!(
        STAGING_POINTS * size_of::<StagedCode>(),
        crate::mrms::CONUS_BAND_BYTES,
        "one staged band, which is what the slot is",
    );
    assert_ne!(
        OFF_NOMINAL_POINTS, STAGING_POINTS,
        "premise: the off-nominal shape below really is off-nominal",
    );
}

/// **The defect GMGSI shipped, at a shape MRMS itself publishes.**
///
/// A pool keyed on [`STAGING_POINTS`] and handed a grid at any other shape
/// reuses nothing and accepts nothing back: `take` compared the request against
/// the constant and `give` compared the offered capacity against it, so every
/// decode allocated a fresh block and every one was freed again — the exact
/// churn this module exists to remove, with the module in place, at full speed,
/// with no error anywhere.
///
/// Observed red before the fix, this test: `declined` **1** where 0 is
/// expected, then `reused` **0** where 1 is, `retained_bytes` **0**, and
/// `health` `Inert`.
#[test]
fn a_grid_at_a_shape_the_conus_constant_does_not_describe_is_pooled() {
    let pool = StagingPool::new();
    assert_eq!(pool.health(), StagingHealth::Cold);

    let first = pool
        .take(OFF_NOMINAL_POINTS)
        .expect("a cold pool allocates");
    let address = first.as_ptr() as usize;
    assert_eq!(first.capacity(), OFF_NOMINAL_POINTS);
    assert_eq!(
        pool.totals().allocated,
        1,
        "premise: the first one is fresh"
    );

    // That very buffer back through `recycle`, the door an eviction uses, so
    // what the next grid is handed can be compared block for block.
    pool.recycle(grid_around(first));
    assert_eq!(
        pool.totals().declined,
        0,
        "a grid of the shape the product is publishing must not be refused by \
         the slot because a constant in this build says 7000 x 3500",
    );
    assert_eq!(
        pool.retained_points(),
        OFF_NOMINAL_POINTS,
        "and the slot must describe the block it is actually holding",
    );
    assert_eq!(
        pool.retained_bytes(),
        OFF_NOMINAL_POINTS * size_of::<StagedCode>(),
        "which is 9,000,000 B and not the nominal 49,000,000",
    );

    let second = pool
        .take(OFF_NOMINAL_POINTS)
        .expect("the slot holds that shape");
    assert_eq!(
        pool.totals().reused,
        1,
        "the next grid of the same product must be handed that block",
    );
    assert_eq!(
        pool.health(),
        StagingHealth::Reusing,
        "and the pool must read as working rather than as merely untouched",
    );
    assert_eq!(
        second.as_ptr() as usize,
        address,
        "and it must be the FIRST one's block, not a fresh allocation of the \
         same size: a pool that reallocated would satisfy every count above and \
         leave the fragmentation it exists to remove exactly as it was",
    );
    assert!(second.is_empty(), "and arrive with nothing in it");
    drop(second);

    assert_eq!(
        pool.nominal_points(),
        STAGING_POINTS,
        "with the budget figure untouched: it prices the caches, not the slot",
    );
}

/// **A pool that reuses nothing reads as inert, not as cold** — the reading the
/// shipping GMGSI defect had, and the one three raw counters could not give.
///
/// `reused: 0` is the reading of a healthy pool nobody has touched *and* of a
/// permanently broken one; only the company it keeps separates them.
#[test]
fn a_pool_that_reuses_nothing_reads_as_inert_and_not_as_cold() {
    let pool = StagingPool::new();
    assert_eq!(
        pool.health(),
        StagingHealth::Cold,
        "premise: nothing has asked this pool for anything",
    );
    assert_eq!(
        pool.totals().reused,
        0,
        "and `reused` is 0 while it is cold"
    );

    // Two decodes that never get a block back — what a pool keyed on a stale
    // constant does on every granule for the life of the process.
    let a = pool.take(OFF_NOMINAL_POINTS).expect("allocates");
    pool.give(buffer_of(OFF_NOMINAL_POINTS));
    let b = pool.take(OFF_NOMINAL_POINTS + 1).expect("allocates");
    assert_eq!(
        pool.totals().reused,
        0,
        "premise: `reused` still reads 0, exactly as it did while cold",
    );
    assert_eq!(
        pool.health(),
        StagingHealth::Inert,
        "and that is the difference a verdict makes: the same 0, now saying \
         the pool is removing nothing rather than that nobody has used it",
    );
    drop((a, b));
}

/// **The product's shape moving mid-process costs one block, not the pool.**
///
/// The slot follows the granule: the buffer for a shape nobody is publishing
/// any more is dropped, the arriving shape becomes the retained one, and the
/// change is counted where an operator can read it. Holding the old block
/// instead is what left GMGSI's shipped pool inert.
#[test]
fn a_shape_change_hands_the_slot_over_and_says_it_did() {
    let pool = StagingPool::new();
    pool.give(mosaic_buffer());
    assert_eq!(
        pool.retained_points(),
        STAGING_POINTS,
        "premise: one CONUS mosaic is parked",
    );

    let fresh = pool.take(OFF_NOMINAL_POINTS).expect("allocates its own");
    assert_eq!(fresh.capacity(), OFF_NOMINAL_POINTS);
    assert_eq!(
        pool.resizes(),
        1,
        "the shape change is one counted event, not a silent decline",
    );
    assert_eq!(pool.retained_points(), 0, "and the old block is let go");
    drop(fresh);

    pool.give(buffer_of(OFF_NOMINAL_POINTS));
    let staged = pool
        .take(OFF_NOMINAL_POINTS)
        .expect("the new shape is pooled");
    assert_eq!(
        pool.totals().reused,
        1,
        "so the cost of the change is one block, once, and not one per granule \
         for the life of the process",
    );
    assert_eq!(pool.resizes(), 1, "and it is not counted again");
    drop(staged);
}

/// **Two shapes alternating over one slot corrupt nothing.**
///
/// Not a shape any shipped path produces — one slot, one product family — but
/// the case a "≥" rule or an inherited-content bug would show up in first.
/// Every buffer handed out is capacity-exact for its own request, nothing is
/// inherited, and the thrash reads as `resizes` rather than as silence.
#[test]
fn two_shapes_alternating_never_hand_a_grid_the_wrong_capacity() {
    let pool = StagingPool::new();
    let shapes = [OFF_NOMINAL_POINTS, 4096, OFF_NOMINAL_POINTS, 4096];
    for (round, points) in shapes.into_iter().enumerate() {
        let mut buffer = pool.take(points).expect("a buffer for this shape");
        assert_eq!(
            buffer.capacity(),
            points,
            "round {round}: a grid is handed a block of its OWN capacity, \
             never merely one big enough — `resident_bytes` is `len * 2` and a \
             grid holding a larger block would under-report its footprint to \
             the byte budget that evicts it",
        );
        assert!(
            buffer.is_empty(),
            "round {round}: and with nothing in it, whatever it last held",
        );
        buffer.resize(points, round as u16 + 1);
        pool.give(buffer);
    }
    assert_eq!(
        pool.resizes(),
        3,
        "three of the four requests were a shape the slot was not holding",
    );
    assert_eq!(
        pool.totals().reused,
        0,
        "so nothing was reused, which is the honest reading of a thrash and \
         what a second slot — not a bigger one — would be the fix for",
    );
    assert_eq!(pool.health(), StagingHealth::Inert);
}

/// A one-reference [`MrmsGrid`](crate::mrms::MrmsGrid) around a caller's own
/// buffer, so a test can follow one block out of the slot and back in.
fn grid_around(values: Vec<u16>) -> crate::mrms::MrmsGrid {
    let points = values.capacity();
    let mut grid = mosaic_grid();
    let arc = std::sync::Arc::get_mut(&mut grid.grid).expect("sole reference");
    arc.values = crate::render::gridded::GridValues::Scaled(crate::render::gridded::ScaledU16 {
        codes: values,
        ref_val: -9990.0,
        two_pow: 1.0,
        dig_factor: 0.1,
        nan_codes: vec![0, 9000],
    });
    debug_assert_eq!(codes_capacity(&arc.values), points);
    grid
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

/// **A grid that is not the slot's shape never touches the retained buffer.**
///
/// The invariant that stops a pooled buffer from being a memory bug in the
/// other direction: `MrmsGrid::resident_bytes` — the figure both byte budgets
/// are spent against — is `len * 2` at the stored width, so a 400-byte grid
/// handed the 49 MB block would report 400 bytes while holding 49 MB, and the
/// cache would go on
/// filling until the tab died. Matching capacity *exactly* rather than "≥" is
/// what forbids it.
///
/// **Its second half is deliberately falsified and re-pinned rather than
/// loosened.** It used to assert the mosaic buffer stayed parked and was handed
/// to the next mosaic-sized request. That is precisely how a wrongly declared
/// pool stays inert for the life of a process: a block for a shape nobody is
/// asking for any more holds the slot, refuses every offer at the shape they
/// *are* asking for, and reuses nothing. The arriving shape wins the slot now,
/// and the safety half — the small grid never receives the big block — is
/// unchanged and is the first assertion below.
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
        "and it is a fresh block, not the mosaic's handed over",
    );
    assert_eq!(
        pool.resizes(),
        1,
        "the mosaic block is let go rather than parked for a shape nobody is \
         asking for — counted, so a product that moved is one readable event",
    );
    assert_eq!(pool.retained_points(), 0, "and the slot is empty behind it");

    let mosaic = pool
        .take(STAGING_POINTS)
        .expect("so the next one allocates");
    assert_eq!(mosaic.capacity(), STAGING_POINTS);
    assert_eq!(
        pool.totals().allocated,
        2,
        "which is the one-block cost of a shape change, paid once",
    );
    drop((small, mosaic));
}

/// And on the way in: a buffer of another capacity is **retained** — it becomes
/// the shape the slot holds — but is never handed to a grid of a different
/// shape.
///
/// **Both halves, because the first was the GMGSI defect.** This asserted the
/// offer was *refused*, which is the assertion that made every real 4999-wide
/// GMGSI granule a refusal and the pool inert. Refusing is safe and useless;
/// what has to stay true is the second half.
#[test]
fn a_buffer_of_another_capacity_is_retained_but_never_handed_to_another_shape() {
    let pool = StagingPool::new();
    pool.give(buffer_of(STAGING_POINTS - 1));
    assert_eq!(
        pool.totals(),
        StagingTotals {
            allocated: 0,
            reused: 0,
            declined: 0
        },
        "the offer is kept: a slot that refused it would allocate a block of \
         that shape on every granule for the life of the process",
    );
    assert_eq!(
        pool.retained_points(),
        STAGING_POINTS - 1,
        "and the slot says which shape it is holding",
    );

    let fresh = pool.take(STAGING_POINTS).expect("a mosaic-sized request");
    assert_eq!(
        fresh.capacity(),
        STAGING_POINTS,
        "a mosaic is never handed the one-short block: capacity is matched \
         EXACTLY, and `len` is what every byte budget is spent against",
    );
    assert_eq!(pool.totals().allocated, 1, "so this one is fresh");
    assert_eq!(pool.resizes(), 1);
    drop(fresh);

    pool.give(buffer_of(STAGING_POINTS - 1));
    let reused = pool
        .take(STAGING_POINTS - 1)
        .expect("and its own shape gets it back");
    assert_eq!(reused.capacity(), STAGING_POINTS - 1);
    assert_eq!(pool.totals().reused, 1);
    drop(reused);
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
        codes_capacity(&still_reading.values),
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
            values: crate::render::gridded::GridValues::Scaled(crate::render::gridded::ScaledU16 {
                codes: mosaic_buffer(),
                // The shipped composite's own packing; see
                // `mrms::decode::tests`.
                ref_val: -9990.0,
                two_pow: 1.0,
                dig_factor: 0.1,
                nan_codes: vec![0, 9000],
            }),
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

/// **The retained level is what the slot is holding right now**, not what it
/// has ever held — the figure `resident_source_bytes` adds to the MRMS
/// layer's two caches.
///
/// Both transitions, because only one of them is obvious. A block sitting in
/// the slot is 49 MB nothing else is naming, and a census that missed it would
/// under-report by a whole mosaic; a block that is *out* is already counted as
/// the grid it is being decoded into, and counting it here as well would
/// double it.
#[test]
fn the_retained_level_follows_the_slot_in_both_directions() {
    let pool = StagingPool::new();
    assert_eq!(pool.retained_bytes(), 0, "a cold pool is holding nothing");

    pool.give(mosaic_buffer());
    assert_eq!(
        pool.retained_bytes(),
        crate::mrms::CONUS_BAND_BYTES,
        "the capacity, not the length: the slot's buffer is always empty, so a \
         level off `len` would read zero over a live block",
    );

    let buffer = pool.take(STAGING_POINTS).expect("the slot is full");
    assert_eq!(
        pool.retained_bytes(),
        0,
        "the block left with the decode and is counted as the grid it becomes",
    );
    drop(buffer);

    pool.give(buffer_of(STAGING_POINTS - 1));
    assert_eq!(
        pool.retained_bytes(),
        (STAGING_POINTS - 1) * size_of::<StagedCode>(),
        "and it follows the block the slot is ACTUALLY holding, not the \
         nominal figure: this used to read 0 because a one-short buffer was \
         refused, and a level that reported 0 over a parked 49 MB block would \
         be the exact under-report the census exists to stop",
    );
    assert!(
        pool.release_retained(),
        "premise: there was a block to find"
    );
    assert_eq!(pool.retained_bytes(), 0);
}

/// The narrow arm's own capacity — what the slot's exact-capacity rule is
/// stated in, and the only arm a mosaic grid is ever built on.
fn codes_capacity(values: &crate::render::gridded::GridValues) -> usize {
    match values {
        crate::render::gridded::GridValues::Scaled(scaled) => scaled.codes.capacity(),
        crate::render::gridded::GridValues::F32(_)
        | crate::render::gridded::GridValues::Bytes(_)
        | crate::render::gridded::GridValues::Tiled(_) => {
            panic!("a mosaic grid is stored as 16-bit codes, not as f32, bytes or tiles")
        }
    }
}

/// **The same lever the generic pool carries, so a memory governor's handle set
/// covers this 49 MB too and not only GMGSI's 15 MB.**
///
/// It is now literally the same lever: MRMS keeps its own *name* — the handler
/// calls `recycle`/`recycle_shared` on this concrete type as inherent methods,
/// which a generic pool cannot carry — but the slot behind it is
/// `crate::staging::StagingPool`, so this forwards rather than reimplementing.
/// The test stays because what the handler calls is this name.
#[test]
fn releasing_the_retained_mosaic_empties_the_slot_and_the_pool_still_works() {
    let pool = StagingPool::new();
    assert!(!pool.release_retained(), "an empty slot releases nothing");

    pool.give(mosaic_buffer());
    assert_eq!(
        pool.retained_bytes(),
        STAGING_POINTS * size_of::<u16>(),
        "premise: one mosaic is parked",
    );
    assert!(pool.release_retained());
    assert_eq!(pool.retained_bytes(), 0);
    assert!(
        !pool.release_retained(),
        "and there is nothing left to find"
    );

    let after = pool
        .take(STAGING_POINTS)
        .expect("a decode after a release still runs");
    assert_eq!(after.capacity(), STAGING_POINTS);
    assert_eq!(pool.totals().allocated, 1);
    pool.give(after);
    let reused = pool.take(STAGING_POINTS).expect("and the slot refills");
    assert_eq!(pool.totals().reused, 1);
    drop(reused);
}
