//! What a reading of the raster ledger is allowed to mean.
//!
//! **All but the last of these are about the arithmetic, not about the
//! counters.** What they check is the part with no dependence on any run:
//! given a reading, which conclusions it licenses. The claim that the *real*
//! path moves the real counters is `every_arrival_is_either_a_picture_or_a_drop`,
//! in `squallar-app`, and the per-browser figures come off the Tier-2 rig,
//! which is a fresh process per leg.
//!
//! The last one is about the counters themselves: that one test's writes stay
//! out of another's figures, which is what a test build's per-thread sink buys
//! and what a reading of absolute values in a concurrent binary rests on.

use super::ledger::{Totals, has_ink};
use super::{RerenderReason, ledger};

/// **A picture that paints nothing is not a picture that painted.**
///
/// The hole this closes, measured 2026-08-31: `note_picture` counted the RGBA
/// buffer whatever was in it, so a layer emitting a fully transparent pixmap
/// satisfied every conjunct the rig has — `dispatched > 0`, `arrived > 0`,
/// `pictures > 0`, `picture_bytes > 0`, `shown + promoted > 0` and the arrival
/// balance — over a map drawing nothing. The reading below is the one that
/// comes apart, and it is the only one that does.
#[test]
fn a_blank_picture_reads_apart_from_a_painted_one() {
    // 64 px of premultiplied RGBA, every byte zero: what a layer that
    // rasterized and drew nothing hands over.
    let blank = vec![0u8; 64 * 4];
    // The same buffer with one pixel of ink, in the LAST position — the shape
    // the cheap wrong answers (peek at the head, sample a stride) get wrong.
    let mut one_pixel = blank.clone();
    *one_pixel.last_mut().expect("the buffer is not empty") = 1;

    assert!(
        !has_ink(&blank),
        "a fully transparent picture reported ink, so the new conjunct passes \
         on the exact case it was added for",
    );
    assert!(
        has_ink(&one_pixel),
        "a picture with one non-transparent pixel reported no ink, so the \
         conjunct would red-gate every honest run",
    );
    assert_eq!(
        blank.len(),
        one_pixel.len(),
        "the two buffers differ in size, so `picture_bytes` would separate \
         them by itself and this test is not about what it says it is about",
    );

    // What the two look like on the line, at the same byte figure. The first
    // five conjuncts cannot tell them apart; that is the hole, stated.
    let painted = Totals {
        dispatched: 6,
        arrived: 6,
        pictures: 6,
        picture_bytes: 6 * 256,
        inked: 6,
        shown: 6,
        ..Totals::default()
    };
    let blank_run = Totals {
        inked: 0,
        ..painted
    };
    for reading in [&painted, &blank_run] {
        assert!(reading.ran());
        assert!(reading.arrivals_balance());
        assert!(reading.pictures > 0 && reading.picture_bytes > 0);
        assert!(reading.on_screen() > 0);
    }
    assert_ne!(
        painted.inked, blank_run.inked,
        "the two readings agree on every figure including the ink, so nothing \
         on this line can see a layer that stopped drawing",
    );
    assert!(
        blank_run.inked <= blank_run.pictures && painted.inked <= painted.pictures,
        "the ink count exceeded its own denominator",
    );
}

/// **A zero that means "never ran" is distinguishable from a zero that means
/// "moved nothing", and that is the whole reason this ledger has a floor.**
///
/// Four checks caught in this campaign could not have failed. The shape of
/// each was the same: the checker and the checked came from one belief, so the
/// absent case and the working-but-empty case read identically. A byte counter
/// alone has exactly that defect — `0 B uploaded` is what a browser that never
/// enabled a texture layer reports and also what one whose uploads had stopped
/// reports. [`Totals::ran`] is the conjunct that separates them, and this is
/// the test that it does.
#[test]
fn the_ledger_separates_a_path_that_never_ran_from_one_that_moved_nothing() {
    // Nothing ever asked for a raster.
    let never = Totals::default();
    assert!(
        !never.ran(),
        "a ledger with no dispatch reported that the path ran",
    );
    assert_eq!(never.picture_bytes, 0);
    assert_eq!(never.on_screen(), 0);

    // The path ran and moved nothing — which is a *fault*, and has to be
    // readable as one rather than as silence.
    let elided = Totals {
        dispatched: 12,
        ..Totals::default()
    };
    assert!(
        elided.ran(),
        "a ledger with twelve dispatches reported that the path never ran, so a \
         pipeline that had stopped moving bytes would be indistinguishable from \
         one nobody switched on",
    );
    assert_eq!(
        elided.picture_bytes, never.picture_bytes,
        "the two cases have to agree on the byte figure, or this test is not \
         about what it says it is about",
    );
    assert_ne!(
        elided.ran(),
        never.ran(),
        "the byte figures agree and the floor does not, which is the property",
    );
}

/// **Every arrival ends as a picture or as a drop, and the ledger says so in
/// one equation.**
///
/// The arrival path has two exits — the pixels are handed to egui, or they are
/// not — and a third exit growing without a counter is exactly how a byte
/// figure quietly starts describing a subset. That is not a hypothetical: the
/// live path already shares its receiver with the loop frames, which take an
/// earlier arm and are deliberately in none of these numbers.
#[test]
fn an_unaccounted_exit_shows_up_in_the_balance() {
    let honest = Totals {
        dispatched: 9,
        arrived: 9,
        pictures: 6,
        dropped: 3,
        ..Totals::default()
    };
    assert!(honest.arrivals_balance());

    let leaked = Totals {
        dropped: 2,
        ..honest
    };
    assert!(
        !leaked.arrivals_balance(),
        "an arrival that was neither uploaded nor counted as dropped left the \
         balance intact, so a third exit could be added without the ledger \
         noticing",
    );
}

/// A picture that reached the screen did so by exactly one of the two routes,
/// and neither route alone is the answer.
#[test]
fn a_picture_reaches_the_screen_by_either_route() {
    let first_picture = Totals {
        shown: 4,
        ..Totals::default()
    };
    let after_a_hold = Totals {
        promoted: 4,
        ..Totals::default()
    };
    assert_eq!(first_picture.on_screen(), 4);
    assert_eq!(after_a_hold.on_screen(), 4);
    assert_eq!(Totals::default().on_screen(), 0);
}

/// **A sibling thread's writes cannot reach this thread's figures**, which is
/// a property of the build rather than of anyone remembering a lock.
///
/// What it pins, observed 2026-09-06 under `cargo test --workspace` and never
/// under a filtered run:
/// `rebuild_reason_tests::the_dispatch_reason_separates_a_pan_from_a_data_arrival`
/// read one `PanCoverage` dispatch across a phase in which its map never
/// moved, because a sibling test dispatched inside its bracket. The bracket
/// was taken under a crate-wide lock — but fifteen files wrote these counters
/// and only eight read them, and it was the readers that took it. See
/// `ledger::sink`.
///
/// **Both directions, because only one of them is about isolation.** The
/// sibling reads its own figures back, so a build in which its writes went
/// nowhere at all cannot pass this by counting nothing; and the same call made
/// on this thread still has to move this thread's own count.
#[test]
fn a_sibling_threads_writes_stay_out_of_this_threads_figures() {
    const SIBLING_DISPATCHES: u64 = 64;

    ledger::reset_for_test();
    let before = ledger::totals();

    let sibling = std::thread::spawn(|| {
        ledger::reset_for_test();
        for _ in 0..SIBLING_DISPATCHES {
            ledger::note_dispatched(RerenderReason::PanCoverage);
            ledger::note_arrived();
        }
        ledger::totals()
    })
    .join()
    .expect("the sibling thread ran to completion");

    assert_eq!(
        sibling.dispatched, SIBLING_DISPATCHES,
        "the sibling's own dispatches did not reach the sibling's own figures, \
         so what is asserted below is isolation from a thread that counted \
         nothing",
    );
    assert_eq!(
        ledger::totals(),
        before,
        "{SIBLING_DISPATCHES} dispatches on another thread moved this thread's \
         figures, so the counters are shared again and every bracketed reading \
         in the workspace is its neighbours' spending as much as its own",
    );

    ledger::note_dispatched(RerenderReason::PanCoverage);
    assert_eq!(
        ledger::totals().dispatched,
        before.dispatched + 1,
        "a dispatch on this thread did not move this thread's own count, so \
         the equality above says 'nothing counts anywhere' rather than \
         'a sibling cannot reach me'",
    );
}

/// **A blank is charged to the layer that produced it, and the two splits of
/// the blank total agree.**
///
/// The per-layer array is written by the same `note_blank` as the per-reason
/// one, so `blank_layers_balance` is an identity — but an identity is only
/// evidence if the writes it balances actually landed somewhere distinguishable.
/// So this asserts the *attribution* too: two layers blanking for the same
/// reason must not pool, which is what a counter keyed by the reason alone
/// looks like and is the whole defect this dimension exists to end.
///
/// The reset is checked here as well, on the new array specifically: the
/// per-reason array was added beside `reasons` and missed by the loop that
/// zeroes them (see the test below), so a second array added the same way gets
/// its own check the day it lands rather than the day someone notices.
#[test]
fn a_blank_is_charged_to_its_own_layer_and_the_two_splits_agree() {
    use ledger::BlankReason;
    use squallar_source::id::{LayerId, known};

    ledger::reset_for_test();
    for _ in 0..5 {
        ledger::note_blank(BlankReason::OutsideView, &known::LIGHTNING);
    }
    ledger::note_blank(BlankReason::OutsideView, &known::METAR);
    ledger::note_blank(BlankReason::DrewNoInk, &known::LIGHTNING);
    // An id no build registers: it must be counted, not dropped. A blank
    // charged to nobody is how an attribution instrument reads green while
    // silently missing its subject.
    ledger::note_blank(
        BlankReason::OutsideView,
        &LayerId::new("AConfigFileSpelling"),
    );

    let t = ledger::totals();
    assert_eq!(t.blanks(), 8, "the blanks did not all reach the ledger");
    assert!(
        t.blank_layers_balance(),
        "the per-layer split does not account for the per-reason one",
    );
    // The attribution, and not merely the arithmetic: Lightning and Metar
    // blanked for the SAME reason, and a counter that pooled them would pass
    // every balance above.
    assert_eq!(
        t.blank_layer_reason(&known::LIGHTNING, BlankReason::OutsideView),
        5
    );
    assert_eq!(
        t.blank_layer_reason(&known::METAR, BlankReason::OutsideView),
        1
    );
    assert_eq!(t.blank_layer(&known::LIGHTNING), 6);
    assert_eq!(t.blank_layer(&known::METAR), 1);
    assert_eq!(
        t.blank_layer(&LayerId::new("SomeOtherUnregisteredSpelling")),
        1,
        "off-ledger ids share one slot, so this reads the same row",
    );
    // A layer that never blanked reads zero rather than being unrepresentable.
    assert_eq!(t.blank_layer(&known::MRMS), 0);
    assert_eq!(t.blank_layers_seen(), 3);
    assert_eq!(
        t.blank_layer_rows().map(|(_, total, _)| total).sum::<u64>(),
        t.blanks(),
        "the printed rows do not partition the blank total, so the line built \
         from them would under-report and look complete",
    );

    ledger::reset_for_test();
    let cleared = ledger::totals();
    assert_eq!(cleared.blank_layer(&known::LIGHTNING), 0);
    assert_eq!(cleared.blank_layers_seen(), 0);
    assert!(cleared.blank_layers_balance());
}

/// **`reset_for_test` puts every counter back, the blank breakdown included.**
///
/// It did not, from the day the breakdown landed until 2026-09-09: the array
/// was added beside `reasons` and not to the loop that zeroes them. Under
/// `--test-threads=1`, where libtest runs every test on one thread and one set
/// of counters spans the whole run, a test asserting an absolute per-reason
/// count would have read its predecessors' blanks on top of its own.
///
/// **Written as a walk over `BlankReason::ALL` rather than as a list**, so a
/// variant appended to the enum is covered the day it exists — which is how
/// the hole got in.
///
/// Both directions: the writes have to land before the reset is asked to
/// remove them, or this passes on a build where `note_blank` counts nothing.
#[test]
fn the_reset_clears_the_blank_breakdown_and_not_only_the_dispatch_reasons() {
    use ledger::BlankReason;

    ledger::reset_for_test();
    for reason in BlankReason::ALL {
        ledger::note_blank(reason, &squallar_source::id::known::LIGHTNING);
    }

    let posted = ledger::totals();
    for reason in BlankReason::ALL {
        assert_eq!(
            posted.blank_reason(reason),
            1,
            "{reason:?} did not reach its own counter, so the reset below \
             would be asked to clear a figure nothing wrote",
        );
    }
    assert_eq!(
        posted.blanks(),
        BlankReason::ALL.len() as u64,
        "the blank count and its breakdown disagree before the reset",
    );

    ledger::reset_for_test();
    let cleared = ledger::totals();
    for reason in BlankReason::ALL {
        assert_eq!(
            cleared.blank_reason(reason),
            0,
            "{reason:?} survived `reset_for_test`, so a test asserting an \
             absolute blank figure under `--test-threads=1` reads its \
             predecessors' blanks as its own",
        );
    }
    assert!(
        cleared.blank_reasons_balance(),
        "the reset left the blank count and its breakdown out of step, which \
         is a hole in the wiring rather than a stale figure",
    );
}
