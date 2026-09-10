use super::*;

/// The ledger is process-global and always on, and this crate's unit tests are
/// one binary, so two suites that both drop moments run in parallel threads
/// against one counter. Every test here reads a DELTA under this lock rather
/// than an absolute, which is what `render::codes`' refusal ledger does for
/// the same reason.
static LEDGER: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn hold() -> std::sync::MutexGuard<'static, ()> {
    LEDGER.lock().unwrap_or_else(|e| e.into_inner())
}

#[test]
fn a_dropped_moment_is_counted_once_in_bytes_and_in_both_its_blocks() {
    let _guard = hold();
    let before = (dropped(), blocks(), bytes());
    dropped_cfp(1832);
    assert_eq!(dropped() - before.0, 1, "one moment");
    assert_eq!(
        blocks() - before.1,
        2,
        "two blocks: the gate `Vec` and the `Arc` that would have shared it"
    );
    assert_eq!(
        bytes() - before.2,
        1832 + crate::scan_size::ALLOCATOR_BLOCK_OVERHEAD as u64
            + crate::scan_size::GATE_BUFFER_SHARE_BYTES as u64
            + crate::scan_size::ALLOCATOR_BLOCK_OVERHEAD as u64,
        "the gate bytes and the two blocks holding them"
    );
}

/// A radial with no CFP block at all had nothing to drop, and counting it
/// would make the ledger read as a saving on volumes that never carried the
/// moment — 0 of the 108-volume corpus, but the wire and the chunk feed can
/// both produce one.
#[test]
fn a_radial_that_carried_no_cfp_is_not_counted() {
    let _guard = hold();
    let before = (dropped(), bytes());
    dropped_cfp(0);
    assert_eq!(dropped(), before.0, "nothing was dropped");
    assert_eq!(bytes(), before.1, "and nothing was priced");
}

/// The ledger is a RUNNING TOTAL and never falls, which is what lets a leg
/// quote it as "since the process started" rather than as a level.
#[test]
fn the_ledger_only_rises() {
    let _guard = hold();
    let before = bytes();
    dropped_cfp(2384);
    let after = bytes();
    assert!(after > before, "{after} is not above {before}");
    dropped_cfp(0);
    assert_eq!(bytes(), after, "a no-op left the total alone");
}
