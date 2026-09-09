//! **The idle trim, driven against the shipped staging slots.**
//!
//! Its own binary, and one test, for the reason
//! `squallar-overlays/tests/overlay_grid_residency_split.rs` gives: the shipped
//! slots are process-global statics, so a second test in the same binary could
//! not tell its own parked block from one another test left behind — and
//! `App::on_pressure` empties the same two slots.
//!
//! **The property the fixture has to carry**, and the reason each premise below
//! is asserted rather than assumed: *both slots must be holding a block when
//! the quiet run starts*. An empty pool reads zero retained bytes before the
//! trim and zero after it, so a test that only checked the end state would be
//! green on a `release` that did nothing at all — which is the shape of the
//! defect this exists to catch. The per-reading assertion inside the loop is
//! the other half: a policy that trimmed on the first quiet reading would pass
//! an end-state check and fails here on a named row.

use squallar_app::grid_pool_trim::{Reading, TRIM_AFTER, observe_served};

#[test]
fn a_quiet_session_gives_both_staging_blocks_up_and_the_next_decode_is_served() {
    let mrms = squallar_overlays::mrms::staging::global();
    let gmgsi = squallar_overlays::gmgsi::staging::global();

    // Park a block in each, through the pools' own doors, at the shape they
    // retain: `take` on an empty slot allocates and `give` parks what came out.
    mrms.give(
        mrms.take(squallar_overlays::mrms::staging::STAGING_POINTS)
            .expect("a staging band fits on a test host"),
    );
    gmgsi.give(
        gmgsi
            .take(squallar_overlays::gmgsi::staging::STAGING_POINTS)
            .expect("a mosaic buffer fits on a test host"),
    );
    assert_eq!(
        mrms.retained_bytes(),
        squallar_overlays::mrms::CONUS_BAND_BYTES,
        "premise: the MRMS slot is holding its band, so there is something for \
         the trim below to be observed taking",
    );
    assert_eq!(
        gmgsi.retained_bytes(),
        squallar_overlays::gmgsi::GRID_POINTS,
        "premise: the GMGSI slot is holding a mosaic (one byte a point)",
    );

    let Some(threshold) = TRIM_AFTER else {
        // The wasm32 arm holds its blocks for the life of the page on purpose;
        // `grid_pool_trim`'s own suite covers that arm as a function.
        return;
    };

    // A count that does not move across readings is the whole signal. Supplied
    // rather than read off the pools: the `take` calls above already moved the
    // real counter, and a test that raced it would have a schedule-dependent
    // colour.
    let quiet_count = 4_242;
    assert_eq!(
        observe_served(quiet_count),
        Reading::Busy,
        "the first reading arms the run — it has nothing to compare against",
    );
    for n in 1..threshold {
        assert_eq!(
            observe_served(quiet_count),
            Reading::Quiet(n),
            "reading {n} of the quiet run is not yet the trim",
        );
        assert_eq!(
            mrms.retained_bytes(),
            squallar_overlays::mrms::CONUS_BAND_BYTES,
            "and the block is still parked at quiet reading {n}: a policy that \
             trimmed early fails on this row rather than passing on the end \
             state",
        );
    }

    assert_eq!(
        observe_served(quiet_count),
        Reading::Trim,
        "the {threshold}th reading is the one that gives the blocks up",
    );
    assert_eq!(mrms.retained_bytes(), 0, "the MRMS slot is empty");
    assert_eq!(gmgsi.retained_bytes(), 0, "the GMGSI slot is empty");
    assert_eq!(
        observe_served(quiet_count),
        Reading::Settled,
        "and a session that has already trimmed does not file a payload on \
         every tick after it",
    );

    // **The way back.** The whole cost of the trim is one allocation on the
    // next decode; nothing is refetched and nothing is redrawn. A decode after
    // a trim is served out of the allocator at exactly the shape the slot would
    // have handed out.
    let fresh = mrms
        .take(squallar_overlays::mrms::staging::STAGING_POINTS)
        .expect("the next decode after a trim allocates rather than failing");
    assert_eq!(
        fresh.capacity(),
        squallar_overlays::mrms::staging::STAGING_POINTS,
        "at the same shape, so the grid it fills is the same grid",
    );

    // **The half that moves a peak rather than an average.** While the trim
    // holds, an offered buffer is refused: the slot refills the moment the next
    // granule displaces one, so a release that left parking on would give the
    // block back for eight seconds out of every two-minute poll and the
    // family's high-water mark would not move.
    //
    // The property that makes this able to fail is that `fresh` owns a **real
    // allocation** at the slot's own shape — `give` already refuses a
    // zero-capacity `Vec` whatever the flag says, so an empty offer would be
    // declined for the wrong reason and this assertion would pass over a flag
    // that does nothing.
    assert_eq!(
        fresh.capacity() * squallar_overlays::mrms::staging::StagingPool::ELEMENT_BYTES,
        squallar_overlays::mrms::CONUS_BAND_BYTES,
        "premise: the buffer offered below owns a whole staging band, so a \
         decline can only be the trim's doing",
    );
    assert!(!mrms.is_retaining(), "the trim turned parking off");
    mrms.give(fresh);
    assert_eq!(
        mrms.retained_bytes(),
        0,
        "an offer made while the trim holds is refused, so the still session \
         parks nothing until a decode cadence comes back",
    );

    // A decode moves the count, which re-arms a settled session and turns
    // parking back on -- one un-pooled decode at the start of a loop, and no
    // more.
    assert_eq!(
        observe_served(quiet_count + 1),
        Reading::Busy,
        "a decode in the interval re-arms the policy out of Settled",
    );
    assert!(
        mrms.is_retaining(),
        "and the busy reading turns parking back on",
    );
    let reused = mrms
        .take(squallar_overlays::mrms::staging::STAGING_POINTS)
        .expect("a mosaic buffer fits on a test host");
    mrms.give(reused);
    assert_eq!(
        mrms.retained_bytes(),
        squallar_overlays::mrms::CONUS_BAND_BYTES,
        "and the pool parks again, so nothing about the loop case changed",
    );
    assert_eq!(
        squallar_overlays::staging::release_all_retained(),
        squallar_overlays::mrms::CONUS_BAND_BYTES as u64,
        "left as this binary found it, and the pressure lever prices what it \
         took",
    );
}
