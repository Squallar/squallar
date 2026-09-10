//! **A busy pool may not veto a quiet pool's trim.**
//!
//! `grid_pool_trim` gives a staging pool's parked block up after
//! [`TRIM_AFTER`] consecutive readings with no decode. It read
//! `squallar_overlays::staging::decodes_served()` to decide that — the **sum**
//! over both shipped pools — and a sum is the right figure for "did anything
//! decode" and the wrong one for "may THIS block go".
//!
//! The two pools have cadences two orders of magnitude apart: MRMS re-polls
//! every 120 s and stages a granule per loop frame, GMGSI polls hourly. So on
//! every scene where MRMS was live the sum moved on essentially every tick,
//! every reading was `Reading::Busy`, and GMGSI's 15,000,000 B block was never
//! given up at all. Measured on a 420 s six-pane HEAVY6 leg: the `overlay
//! grids` census family's `parked` term sat at 15,221,000 B across every
//! sample of the leg and never once fell.
//!
//! `squallar_overlays::staging::release_all_retained` identifies this exact
//! hazard in its own comment — "a short-circuit would leave GMGSI's block
//! parked on every event where MRMS happened to have one" — and closes it in
//! the release. This is the same hazard one level up, in the policy that
//! decides whether the release is ever reached.
//!
//! Its own binary and one test, for the reason
//! `tests/grid_pool_trim_slots.rs` gives: the shipped slots are process-global
//! statics, so a second test in the same binary could not tell its own parked
//! block from one another test left behind.

use squallar_app::grid_pool_trim::{Reading, TRIM_AFTER, observe_reading};

/// One MRMS decode, through the pool's own doors: `take` moves that pool's
/// decode count and `give` parks the buffer again, which is what a layer
/// staging a loop frame does every time round.
fn one_mrms_decode() {
    let mrms = squallar_overlays::mrms::staging::global();
    let buffer = mrms
        .take(squallar_overlays::mrms::staging::STAGING_POINTS)
        .expect("a staging band fits on a test host");
    mrms.give(buffer);
}

#[test]
fn a_busy_mrms_does_not_hold_gmgsi_s_block() {
    let mrms = squallar_overlays::mrms::staging::global();
    let gmgsi = squallar_overlays::gmgsi::staging::global();

    let Some(threshold) = TRIM_AFTER else {
        // The wasm32 arm holds its blocks for the life of the page on purpose
        // — the bytes do not come back from a linear memory. `grid_pool_trim`'s
        // own suite covers that arm as a function.
        return;
    };

    // Park a block in each, through the pools' own doors, at the shape they
    // retain.
    mrms.give(
        mrms.take(squallar_overlays::mrms::staging::STAGING_POINTS)
            .expect("a staging band fits on a test host"),
    );
    gmgsi.give(
        gmgsi
            .take(squallar_overlays::gmgsi::staging::STAGING_POINTS)
            .expect("a mosaic buffer fits on a test host"),
    );

    // **Both premises asserted, not assumed.** An empty pool reads zero
    // retained bytes before the trim and zero after it, so a test that only
    // checked the end state would be green on a policy that did nothing at
    // all — which is the shape of the defect this exists to catch.
    assert_eq!(
        mrms.retained_bytes(),
        squallar_overlays::mrms::CONUS_BAND_BYTES,
        "premise: the MRMS slot is holding its band",
    );
    assert_eq!(
        gmgsi.retained_bytes(),
        squallar_overlays::gmgsi::GRID_POINTS,
        "premise: the GMGSI slot is holding a mosaic (one byte a point), so \
         there is something for the trim below to be observed taking",
    );

    // **MRMS decodes on every tick and GMGSI decodes on none of them** — the
    // shape of every scene with a playing MRMS loop, which is the shape of
    // every heavy scene this campaign measures. Twice the threshold, so the
    // quiet run has room to complete and then settle.
    for _ in 0..(threshold * 2) {
        one_mrms_decode();
        observe_reading();
    }

    // **The claim.** GMGSI decoded nothing across that whole run, so its block
    // is the campaign's cleanest kind of byte: parked, read by nothing, and
    // released without trading anything a pane is drawing.
    assert_eq!(
        gmgsi.retained_bytes(),
        0,
        "GMGSI decoded nothing across {} readings and must have given its \
         block up; a non-zero here is MRMS's cadence deciding GMGSI's fate, \
         which is 15,000,000 B held on every scene with a live MRMS loop",
        threshold * 2,
    );

    // **And the other direction, which is what keeps this from being a policy
    // that simply trims harder.** MRMS decoded on every one of those readings.
    // Its block must still be parked, because the next decode is about to want
    // it — a policy that took it would be paying an allocation per loop frame,
    // which is the churn the pools exist to prevent.
    assert_eq!(
        mrms.retained_bytes(),
        squallar_overlays::mrms::CONUS_BAND_BYTES,
        "MRMS decoded on every reading, so its block is still earning its \
         keep and must NOT have been taken",
    );
    assert!(
        mrms.is_retaining(),
        "and MRMS is still parking, so the decode after this one is served \
         out of the slot",
    );

    // **The fires counter, both directions.** A policy that fired and gave
    // nothing back reads exactly like one nothing called, so the counter has
    // to show GMGSI's trim firing AND MRMS's not firing.
    let (gmgsi_trims, gmgsi_blocks, gmgsi_bytes, _, gmgsi_quiet) =
        squallar_app::grid_pool_trim::trim_totals(squallar_overlays::staging::Pool::Gmgsi);
    assert!(
        gmgsi_trims >= 1 && gmgsi_blocks >= 1,
        "the counter must record the trim that fired and the block it took: \
         {gmgsi_trims} trims, {gmgsi_blocks} blocks",
    );
    assert_eq!(
        gmgsi_bytes,
        squallar_overlays::gmgsi::GRID_POINTS as u64,
        "and price it at what the slot was actually holding",
    );
    assert!(
        gmgsi_quiet >= threshold as u64,
        "GMGSI's quiet readings are the denominator that separates 'never \
         fired' from 'never went quiet': {gmgsi_quiet}",
    );

    let (mrms_trims, _, mrms_bytes, mrms_busy, _) =
        squallar_app::grid_pool_trim::trim_totals(squallar_overlays::staging::Pool::Mrms);
    assert_eq!(
        (mrms_trims, mrms_bytes),
        (0, 0),
        "MRMS never went quiet, so nothing of its may have been given up",
    );
    assert!(
        mrms_busy >= threshold as u64,
        "and its readings were busy ones: {mrms_busy}",
    );

    // **Non-triviality, both ways.** The policy is still a policy and not an
    // unconditional release: a tick with no decode behind it reads quiet even
    // for MRMS, and a tick with one reads busy again. A policy hard-wired to
    // either answer fails one of these two rows.
    assert_eq!(
        observe_reading(),
        Reading::Settled,
        "no pool decoded between the loop's last reading and this one, so \
         nothing here is busy",
    );
    one_mrms_decode();
    assert_eq!(
        observe_reading(),
        Reading::Busy,
        "and a decode re-arms it: MRMS moved its own count, so the session \
         reads busy again",
    );
    assert!(
        mrms.is_retaining(),
        "the busy reading turns MRMS's parking back on",
    );
}
