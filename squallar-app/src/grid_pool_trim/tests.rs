//! The policy as a function of its inputs. Its effect on the shipped pools is
//! `tests/grid_pool_trim_slots.rs`, which needs a binary of its own.

use super::*;

/// A count that moved is Busy whatever the quiet run behind it was — including
/// out of [`SETTLED`], which is what re-arms a session that had already
/// trimmed.
#[test]
fn a_decode_in_the_interval_is_busy_from_every_state() {
    for quiet in [0, 1, 3, TRIM_AFTER.unwrap_or(4) - 1, SETTLED] {
        assert_eq!(
            decide(8, 7, quiet, Some(4)),
            Reading::Busy,
            "one more decode than the last reading is Busy at quiet={quiet}",
        );
    }
}

/// Four consecutive quiet readings and the fourth is the one that trims; the
/// fifth and every one after it is Settled until a decode moves the count.
///
/// **Non-vacuity**: the first three readings are asserted to be `Quiet(n)` with
/// the n they carry, so a `decide` that returned `Trim` immediately — or that
/// never returned it — fails on a named row rather than passing a `<=`.
#[test]
fn the_fourth_quiet_reading_trims_and_the_rest_are_settled() {
    let served = 12;
    assert_eq!(decide(served, served, 0, Some(4)), Reading::Quiet(1));
    assert_eq!(decide(served, served, 1, Some(4)), Reading::Quiet(2));
    assert_eq!(decide(served, served, 2, Some(4)), Reading::Quiet(3));
    assert_eq!(decide(served, served, 3, Some(4)), Reading::Trim);
    assert_eq!(decide(served, served, SETTLED, Some(4)), Reading::Settled);
}

/// **The wasm32 arm, exercised from a native test.** `TRIM_AFTER` is `None`
/// there and the policy must never reach `Trim` — the blocks are held for the
/// life of the page, which is the behaviour that shipped before this module.
///
/// Reachable only because `decide` takes the threshold as an argument; a
/// `#[cfg]` inside the body would leave this arm untested on every target that
/// is not the one it governs.
#[test]
fn a_target_with_no_trim_never_reaches_trim() {
    for quiet in [0, 1, 2, 3, 4, 99, SETTLED] {
        assert_eq!(
            decide(5, 5, quiet, None),
            Reading::Settled,
            "no threshold means nothing is ever given up (quiet={quiet})",
        );
    }
    assert_eq!(
        decide(6, 5, 0, None),
        Reading::Busy,
        "and a decode still reads Busy"
    );
}
