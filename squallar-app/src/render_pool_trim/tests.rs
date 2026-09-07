//! The idle policy as a function of its two readings.
//!
//! Only [`super::decide`] is exercised here. The stateful half touches
//! `squallar_radar::render`'s process-global slots, which other tests in this
//! binary rasterize into; it is pinned end to end in
//! `squallar-app/tests/render_pool_idle_trim.rs`, which is its own process.

use super::{QUIET_READINGS, Reading, SETTLED, decide};

/// A render dispatched between two readings restarts the count, whatever it
/// stood at.
#[test]
fn a_render_between_readings_is_busy() {
    for quiet in [0, 1, QUIET_READINGS - 1, SETTLED] {
        assert_eq!(
            decide(8, 7, quiet, true),
            Reading::Busy,
            "the count moved 7 -> 8 and the reading at quiet={quiet} was not busy"
        );
    }
}

/// An outstanding reply is busy even with the render count still — the raster
/// is on its way to a pane and the pools are about to be asked for again.
#[test]
fn an_outstanding_reply_is_busy() {
    assert_eq!(decide(7, 7, QUIET_READINGS - 1, false), Reading::Busy);
}

/// Quiet readings accumulate one at a time, and the trim is the
/// [`QUIET_READINGS`]th — never earlier.
#[test]
fn the_trim_is_the_fourth_consecutive_quiet_reading() {
    assert_eq!(QUIET_READINGS, 4, "the walk below counts four");
    let mut quiet = 0;
    for reading in 1..QUIET_READINGS {
        match decide(7, 7, quiet, true) {
            Reading::Quiet(n) => {
                assert_eq!(n, reading, "reading {reading} counted itself as {n}");
                quiet = n;
            }
            other => panic!("reading {reading} of {QUIET_READINGS} answered {other:?}"),
        }
    }
    assert_eq!(
        decide(7, 7, quiet, true),
        Reading::Trim,
        "reading {QUIET_READINGS} did not give the buffers up"
    );
}

/// **The inverse pin.** A session rendering steadily never reaches a trim,
/// however long it runs: every reading sees the count move, so the pools are
/// kept across renders and the next render resizes rather than allocates.
#[test]
fn a_session_rendering_steadily_never_trims() {
    let mut begun = 0;
    let mut quiet = 0;
    for tick in 0..50 {
        begun += 1;
        let reading = decide(begun, begun - 1, quiet, true);
        assert_eq!(
            reading,
            Reading::Busy,
            "a steadily rendering session reached {reading:?} at tick {tick}"
        );
        quiet = 0;
    }
}

/// A session that renders in bursts with gaps under the window keeps its
/// buffers too: three quiet readings then a render is never a trim.
#[test]
fn a_gap_shorter_than_the_window_keeps_the_buffers() {
    let mut quiet = 0;
    for _ in 0..QUIET_READINGS - 1 {
        match decide(7, 7, quiet, true) {
            Reading::Quiet(n) => quiet = n,
            other => panic!("a sub-window gap answered {other:?}"),
        }
    }
    assert_eq!(
        decide(8, 7, quiet, true),
        Reading::Busy,
        "a render arriving inside the window did not restart the count"
    );
}

/// Once trimmed, further quiet readings are settled rather than trims: there
/// is nothing left to give up, and the free lane must not be handed an empty
/// payload every tick for the rest of the session.
#[test]
fn a_trimmed_session_settles_until_a_render_begins() {
    assert_eq!(decide(7, 7, SETTLED, true), Reading::Settled);
    assert_eq!(
        decide(8, 7, SETTLED, true),
        Reading::Busy,
        "a render after a trim must re-arm the policy"
    );
}
