use super::{PassCostLedger, PassCosts};

/// Each phase accumulates into its own field and every note counts one pass —
/// with a distinct value in every position, so a transposed field cannot read
/// as correct arithmetic.
#[test]
fn each_phase_accumulates_into_its_own_field() {
    let mut ledger = PassCostLedger::default();
    ledger.note(11, 23, 37, 41);
    ledger.note(100, 200, 300, 400);
    assert_eq!(
        ledger.totals(),
        PassCosts {
            passes: 2,
            tessellate_us: 111,
            upload_apply_us: 223,
            mirror_us: 337,
            buffers_and_callbacks_us: 441,
        },
    );
    assert_eq!(ledger.totals().total_us(), 111 + 223 + 337 + 441);
}

/// A pass that cost nothing measurable is still a pass: the non-vacuity floor
/// moves even when every phase reads zero.
#[test]
fn a_free_pass_still_counts_toward_the_floor() {
    let mut ledger = PassCostLedger::default();
    ledger.note(0, 0, 0, 0);
    assert_eq!(
        ledger.totals().passes,
        1,
        "a zero-cost pass vanished; a stopped clock would now be \
         indistinguishable from an idle renderer",
    );
    assert_eq!(ledger.totals().total_us(), 0);
}

/// `totals_if_moved` answers once per pass and stays quiet until the next
/// one — the read-and-mark contract the upload totals already keep.
#[test]
fn if_moved_answers_once_per_pass() {
    let mut ledger = PassCostLedger::default();
    assert_eq!(
        ledger.totals_if_moved(),
        None,
        "a renderer that ended no pass reported one",
    );
    ledger.note(5, 6, 7, 8);
    assert!(ledger.totals_if_moved().is_some());
    assert_eq!(
        ledger.totals_if_moved(),
        None,
        "the same pass was reported twice",
    );
    ledger.note(1, 2, 3, 4);
    let again = ledger
        .totals_if_moved()
        .expect("a new pass went unreported");
    assert_eq!(again.passes, 2, "the second answer lost the running total");
}
