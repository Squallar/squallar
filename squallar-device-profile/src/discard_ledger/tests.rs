//! The ledger's two halves, each asserted on **both** arms: a holder that
//! really is empty must read zero, and one holding a payload must read its
//! bytes.
//!
//! The queue half is thread-local and each test owns its thread, so it is
//! asserted absolutely. The lane half is one process-wide static and these
//! tests share a process, so every test that touches it takes [`LANE`] first —
//! without that they would be reading each other's payloads and the figures
//! would be nobody's.

use super::*;
use std::sync::Mutex;

/// Held by every test that files onto the lane, so each sees only its own.
static LANE: Mutex<()> = Mutex::new(());

/// The green arm, and the one worth naming: a holder that really is empty
/// reads zero. A family whose zero is never checked against a genuinely empty
/// holder cannot tell "nothing in flight" from "nothing counted".
#[test]
fn an_empty_ledger_reads_zero_on_both_halves() {
    let _lane = LANE.lock().unwrap_or_else(|e| e.into_inner());
    assert_eq!(queued_bytes(), 0, "this thread has filed nothing");
    assert_eq!(
        lane_bytes(),
        0,
        "no test holds the lane while this one does"
    );
    assert_eq!(in_flight_bytes(), 0, "so the sum is zero too");
}

/// Filed, then freed, and the total is back where it started — the property
/// that makes zero mean "settled" rather than "never counted".
#[test]
fn the_queue_half_prices_at_the_filing_and_deprices_at_the_drop() {
    assert_eq!(queued_bytes(), 0);
    queue_filed(47 << 20);
    assert_eq!(queued_bytes(), 47 << 20, "a filed payload reads its bytes");
    queue_filed(1 << 20);
    assert_eq!(queued_bytes(), (47 << 20) + (1 << 20));
    queue_freed(47 << 20);
    assert_eq!(queued_bytes(), 1 << 20);
    queue_freed(1 << 20);
    assert_eq!(queued_bytes(), 0, "an emptied queue reads zero again");
}

/// The same round trip on the lane half.
#[test]
fn the_lane_half_prices_at_the_handover_and_deprices_at_the_drop() {
    let _lane = LANE.lock().unwrap_or_else(|e| e.into_inner());
    assert_eq!(lane_bytes(), 0);
    lane_filed(64 << 20);
    assert_eq!(
        lane_bytes(),
        64 << 20,
        "a payload handed to the lane reads its bytes at the seam"
    );
    lane_freed(64 << 20);
    assert_eq!(lane_bytes(), 0, "and leaves the total when it is freed");
}

/// A decrement larger than the total saturates. A lost increment must read as
/// a small overcount; wrapping would publish `u64::MAX` bytes in flight, which
/// swamps every other family on the census line and reads as catastrophe.
#[test]
fn a_lane_decrement_past_zero_saturates_rather_than_wrapping() {
    let _lane = LANE.lock().unwrap_or_else(|e| e.into_inner());
    assert_eq!(lane_bytes(), 0);
    lane_freed(u64::MAX);
    assert_eq!(lane_bytes(), 0, "an over-large decrement must not wrap");
    // And the counter still works afterwards.
    lane_filed(8);
    assert_eq!(lane_bytes(), 8);
    lane_freed(8);
    assert_eq!(lane_bytes(), 0);
}

/// **The halves are counted apart and summed.** This is what lets one family
/// be true on both targets: each target's unused route is structurally zero,
/// and the sum reports whichever one is carrying the payload.
#[test]
fn the_two_holders_are_counted_apart_and_summed() {
    let _lane = LANE.lock().unwrap_or_else(|e| e.into_inner());
    assert_eq!(in_flight_bytes(), 0);

    queue_filed(8 << 20);
    assert_eq!(
        in_flight_bytes(),
        8 << 20,
        "the queue's bytes reach the sum"
    );
    assert_eq!(lane_bytes(), 0, "filing the queue is not filing the lane");

    lane_filed(4 << 20);
    assert_eq!(in_flight_bytes(), (8 << 20) + (4 << 20));
    assert_eq!(queued_bytes(), 8 << 20, "filing the lane is not the queue");

    queue_freed(8 << 20);
    assert_eq!(in_flight_bytes(), 4 << 20, "the lane's half outlives it");
    lane_freed(4 << 20);
    assert_eq!(in_flight_bytes(), 0);
}
