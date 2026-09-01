//! What a reading of the raster ledger is allowed to mean.
//!
//! **These are about the arithmetic, not about the statics.** The counters are
//! process-global, so a unit-test binary shares them across every test it runs
//! and an assertion on their absolute values would be an assertion about the
//! order the harness happened to pick. What is checked here is the part that
//! has no such dependence: given a reading, which conclusions it licenses. The
//! claim that the *real* path moves the real counters is
//! `every_arrival_is_either_a_picture_or_a_drop`, in `squallar-app`, and the
//! per-browser figures come off the Tier-2 rig, which is a fresh process per
//! leg.

use super::ledger::{Totals, has_ink};

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
