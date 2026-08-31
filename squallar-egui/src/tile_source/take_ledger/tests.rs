//! What a reading of the take ledger is allowed to mean, and the one claim
//! that is about the real path.
//!
//! **Mostly about the arithmetic, not about the statics**, on
//! [`crate::overlay_cache::ledger_tests`]' terms: the histograms are
//! process-global, so a unit-test binary shares them across every test it runs
//! and an assertion on their absolute values would be an assertion about the
//! order the harness happened to pick.
//!
//! The two exceptions are [`the_drain_charges_every_take_to_the_ledger`] and
//! [`a_takes_cost_reaches_the_ledger_and_is_not_zero`], and they are safe to
//! make **because of a property of this crate rather than of the harness**: on
//! the native arm every real take is a [`TakeKind::Put`]
//! (`HttpsTiles::drain_completed_fetches`'s closure returns nothing else),
//! `tile_source::tests`' own drain fixtures return `Put` and
//! [`TakeKind::Sniffed`], and `note_take` has no other caller. So nothing else
//! in this binary can move [`TakeKind::Restyle`] or [`TakeKind::Vector`].
//!
//! **The two tests take one family each, and that is load-bearing.** They both
//! used `Restyle` when first written and raced -- the harness runs them on
//! different threads over one `static`, and the count test's three takes
//! landed inside the cost test's window, which read 4 where it asserted 1. A
//! family per test is what makes a delta exclusive; sharing one makes it a
//! coin flip. The per-browser figures come off the browser rig, which is a
//! fresh process per leg.

use super::{FAMILIES, TakeKind, Totals, totals};
use squallar_device_profile::hist::Hist;

/// **A family is a family: a take lands in one of them and in no other.**
///
/// The non-triviality floor under every windowed figure this ledger produces.
/// Five histograms whose contents were interchangeable would answer every
/// question with the same number, and a `vector` reading that silently
/// included the hillshade's raster takes is precisely the mistake that makes
/// a "one tile take costs X" claim unquotable.
#[test]
fn every_family_is_its_own_histogram() {
    for kind in FAMILIES {
        let mut families = [Hist::new(); FAMILIES.len()];
        families[kind.index()].record(4_000);
        let totals = Totals { families };

        assert_eq!(
            totals.family(kind).total(),
            1,
            "a take recorded into {} did not land there",
            kind.label(),
        );
        assert_eq!(
            totals.takes(),
            1,
            "one take into {} was counted {} times across the families",
            kind.label(),
            totals.takes(),
        );
        for other in FAMILIES.into_iter().filter(|&other| other != kind) {
            assert_eq!(
                totals.family(other).total(),
                0,
                "a take into {} also showed up in {}",
                kind.label(),
                other.label(),
            );
        }
    }
}

/// **Five distinct slots and five distinct words.** The line names the family
/// and the rig matches on that word, so two families sharing either would
/// silently merge two denominators — the exact defect the module doc says
/// these figures exist to avoid.
#[test]
fn the_families_are_distinguishable_by_slot_and_by_name() {
    let mut slots: Vec<usize> = FAMILIES.iter().map(|kind| kind.index()).collect();
    slots.sort_unstable();
    slots.dedup();
    assert_eq!(
        slots.len(),
        FAMILIES.len(),
        "two families share a histogram slot, so their takes are being added",
    );
    assert_eq!(
        slots,
        (0..FAMILIES.len()).collect::<Vec<_>>(),
        "the family slots are not the histogram array's indices",
    );

    let mut labels: Vec<&str> = FAMILIES.iter().map(|kind| kind.label()).collect();
    labels.sort_unstable();
    labels.dedup();
    assert_eq!(
        labels.len(),
        FAMILIES.len(),
        "two families report under one word, so the rig cannot tell them apart",
    );
    for label in labels {
        assert!(
            label.chars().all(|c| c.is_ascii_lowercase() || c == '-'),
            "the family word {label:?} is outside the rig's [a-z0-9-] group, \
             so `drive.py` would not match the line that carries it",
        );
    }
}

/// **The window is of the window** — counts *and* the exact mean, which is the
/// whole reason a running sum sits beside the bins.
///
/// The failure this pins is the one that made a per-segment figure
/// unobtainable from the shipping instrument: a percentile cannot be
/// subtracted, so a cumulative-from-boot p99 answers "since boot, boot frames
/// included" no matter what window you meant. Bins subtract, and so does the
/// sum.
#[test]
fn the_diff_of_two_readings_is_the_window_between_them() {
    let mut before = [Hist::new(); FAMILIES.len()];
    // Before the window: two cheap vector takes.
    before[TakeKind::Vector.index()].record(1_000);
    before[TakeKind::Vector.index()].record(1_000);
    let before = Totals { families: before };

    // During it: two expensive ones, and one raster take.
    let mut after = before;
    after.families[TakeKind::Vector.index()].record(9_000);
    after.families[TakeKind::Vector.index()].record(11_000);
    after.families[TakeKind::Raster.index()].record(500);

    let window = after.diff(&before);
    assert_eq!(
        window.takes(),
        3,
        "the window did not isolate its own takes"
    );
    assert_eq!(window.family(TakeKind::Vector).total(), 2);
    assert_eq!(
        window.family(TakeKind::Vector).mean_micros(),
        Some(10_000),
        "the windowed mean is not exact, so the cheap takes before the window \
         are still being averaged into it",
    );
    assert_eq!(
        window.family(TakeKind::Raster).mean_micros(),
        Some(500),
        "the raster family's window is wrong",
    );

    // The cumulative reading is a different, and much flatter, answer. This is
    // the contrast the instrument exists to make available.
    assert_eq!(after.family(TakeKind::Vector).total(), 4);
    assert_eq!(
        after.family(TakeKind::Vector).mean_micros(),
        Some(5_500),
        "the cumulative mean changed, so this test is not contrasting what it \
         says it is contrasting",
    );
    assert_ne!(
        after.family(TakeKind::Vector).mean_micros(),
        window.family(TakeKind::Vector).mean_micros(),
        "the windowed and cumulative means agree, so this test could not have \
         failed if diffing were broken",
    );
}

/// **Every take the drain performs is charged to the ledger.**
///
/// The claim that the real path records, and the only test here that touches
/// the statics — see the module doc for why [`TakeKind::Restyle`] is exclusive
/// to it on the native arm.
///
/// A count and an identity, never a duration: what is asserted is that the
/// ledger's sample count moved by exactly the number of takes
/// `drain_up_to` reports performing. Deleting the `note_take` call, or making
/// it a no-op, or moving the timing outside the loop, all fail it. The
/// `taken == 3` conjunct is the non-triviality floor: without it a drain that
/// took nothing at all would satisfy `0 == 0`.
#[test]
fn the_drain_charges_every_take_to_the_ledger() {
    let (mut tx, mut rx) = futures::channel::mpsc::channel::<u8>(8);
    for item in 0..3u8 {
        tx.try_send(item).expect("the test channel has room for 3");
    }

    let before = totals();
    let mut handled = 0usize;
    let mut reported = false;
    let taken = super::super::drain_up_to(
        &mut rx,
        8,
        // Far enough out that the deadline cannot end the drain early; what
        // is under test is the recording, not the governor.
        web_time::Instant::now() + std::time::Duration::from_secs(30),
        true,
        &mut reported,
        |_item: u8| {
            handled += 1;
            TakeKind::Restyle
        },
    );
    let window = totals().diff(&before);

    assert_eq!(
        taken, 3,
        "the drain did not take the three queued completions"
    );
    assert_eq!(handled, 3, "the handler did not see every take");
    assert_eq!(
        window.family(TakeKind::Restyle).total(),
        3,
        "the drain performed 3 takes and the ledger recorded {} — a take the \
         frame paid for that no figure can see is exactly the hole this \
         ledger exists to close",
        window.family(TakeKind::Restyle).total(),
    );
}

/// **A take's cost is recorded, not just its occurrence.**
///
/// The companion floor to the count gate above: a `note_take` that always
/// passed zero would satisfy every count assertion in this file while
/// measuring nothing. Asserted as an identity on the histogram's own sum
/// rather than as a wall-clock bound — the handler here sleeps, but nothing
/// asserts *how long* it took, only that a positive cost reached the ledger.
#[test]
fn a_takes_cost_reaches_the_ledger_and_is_not_zero() {
    let (mut tx, mut rx) = futures::channel::mpsc::channel::<u8>(2);
    tx.try_send(0).expect("the test channel has room for 1");

    let before = totals();
    let mut reported = false;
    let taken = super::super::drain_up_to(
        &mut rx,
        8,
        web_time::Instant::now() + std::time::Duration::from_secs(30),
        true,
        &mut reported,
        |_item: u8| {
            // Well clear of any clock's grain, and of the histogram's 62.5 us
            // floor, so "the sum is positive" is not a coin flip on a coarse
            // timer. No assertion is made about the value.
            std::thread::sleep(std::time::Duration::from_millis(5));
            TakeKind::Vector
        },
    );
    let window = totals().diff(&before);

    assert_eq!(taken, 1, "the drain did not take the queued completion");
    assert_eq!(window.family(TakeKind::Vector).total(), 1);
    assert!(
        window.family(TakeKind::Vector).sum_micros() > 0,
        "the take was counted with a cost of zero, so every duration this \
         ledger reports would be an empty figure that still reads as one",
    );
    assert!(
        window
            .family(TakeKind::Vector)
            .mean_micros()
            .is_some_and(|m| m > 0),
        "the mean of a recorded take is zero",
    );
}
