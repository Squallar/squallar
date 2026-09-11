use super::*;

/// The ledger is process-global and always on, and this crate's unit tests are
/// one binary, so two suites that both build payloads run in parallel threads
/// against one counter. Every test here reads a DELTA under this lock rather
/// than an absolute, the way `crate::moment_drop`'s suite does for the same
/// reason.
static LEDGER: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn hold() -> std::sync::MutexGuard<'static, ()> {
    LEDGER.lock().unwrap_or_else(|e| e.into_inner())
}

/// One block apiece and not two: the `Arc` is shared rather than avoided, so
/// [`crate::scan_size::GATE_BUFFER_SHARE_BYTES`] is deliberately absent from
/// the figure. Asserted so that adding it later has to argue with this test.
#[test]
fn an_adopted_buffer_is_charged_the_gate_bytes_and_one_block() {
    let _guard = hold();
    let before = (adopted_count(), adopted_bytes());
    adopted(1832);
    assert_eq!(adopted_count() - before.0, 1, "one payload");
    assert_eq!(
        adopted_bytes() - before.1,
        1832 + crate::scan_size::ALLOCATOR_BLOCK_OVERHEAD as u64,
        "the gate bytes and the one `Vec` block the copy would have needed"
    );
}

#[test]
fn a_returned_buffer_is_charged_on_the_same_convention() {
    let _guard = hold();
    let before = (returned_count(), returned_bytes());
    returned(2384);
    assert_eq!(returned_count() - before.0, 1, "one rebuild");
    assert_eq!(
        returned_bytes() - before.1,
        2384 + crate::scan_size::ALLOCATOR_BLOCK_OVERHEAD as u64,
        "the gate bytes and one block"
    );
}

/// The two directions are different populations — a payload is built once per
/// reachable (radial, moment) and rebuilt only on the arms that reassemble a
/// volume — so neither total may leak into the other. A single mixed figure is
/// exactly the "every figure names its denominator" defect.
#[test]
fn the_two_directions_do_not_feed_each_others_totals() {
    let _guard = hold();
    let before = (
        adopted_count(),
        adopted_bytes(),
        returned_count(),
        returned_bytes(),
    );
    adopted(64);
    assert_eq!(
        returned_count(),
        before.2,
        "the way in left the way out alone"
    );
    assert_eq!(returned_bytes(), before.3, "in bytes too");
    returned(64);
    assert_eq!(
        adopted_count() - before.0,
        1,
        "and the way out left the way in at its one"
    );
    assert_eq!(
        adopted_bytes() - before.1,
        64 + crate::scan_size::ALLOCATOR_BLOCK_OVERHEAD as u64
    );
}

/// A moment stripped by [`crate::skeleton`] carries an empty buffer, and
/// counting it would make the ledger read as a saving on volumes holding no
/// gates at all.
#[test]
fn an_empty_buffer_is_not_counted_in_either_direction() {
    let _guard = hold();
    let before = (
        adopted_count(),
        adopted_bytes(),
        returned_count(),
        returned_bytes(),
    );
    adopted(0);
    returned(0);
    assert_eq!(adopted_count(), before.0, "nothing was adopted");
    assert_eq!(adopted_bytes(), before.1, "and nothing was priced");
    assert_eq!(returned_count(), before.2, "nothing was returned");
    assert_eq!(returned_bytes(), before.3, "and nothing was priced");
}

/// A RUNNING TOTAL, never a level, which is what lets a leg quote it as "since
/// the process started".
#[test]
fn the_totals_only_rise() {
    let _guard = hold();
    let before = adopted_bytes();
    adopted(1000);
    let after = adopted_bytes();
    assert!(after > before, "{after} is not above {before}");
    adopted(0);
    assert_eq!(adopted_bytes(), after, "a no-op left the total alone");
}
