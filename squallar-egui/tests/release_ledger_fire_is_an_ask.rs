//! **A fire is an ask too** — `release_ledger`'s one invariant, asserted as an
//! exact delta.
//!
//! # Why this is its own binary
//!
//! The counters behind [`squallar_egui::release_ledger`] are process-global
//! `static`s, and their production writer,
//! `Gui::release_data_of_layers_no_pane_draws`, runs on **every frame**. In the
//! `squallar-egui` lib-test binary that writer is not hypothetical: the harness
//! in `input_harness/tests.rs` alone drives frames from well over two hundred
//! call sites, and libtest runs those threads concurrently with everything
//! else. Any one of them scheduled between this test's two reads adds its own
//! asks to the difference.
//!
//! Reading deltas rather than levels does not fix that. **A delta is immune to
//! what happened before the window and defenceless against what happens
//! inside it** — which is exactly what a concurrent frame is. The window here
//! is the three lines between `before` and `after`, and the only way to hold
//! it closed is to put the test in a process where nothing else drives a
//! frame. That is this file.
//!
//! So the assertion stays **exact**. Relaxing it to `>=` would buy a green
//! board by giving up the property: a ledger that lost a fire, or counted an
//! ask it never made, would still satisfy `>=`, and the one thing this test
//! exists to catch would pass.

use squallar_egui::release_ledger::{note_ask, note_fire, totals};

/// **A fire is an ask too**, so `fires <= asks` holds without the caller having
/// to remember two calls — the shape that would otherwise let a leg report more
/// releases than the pass made.
///
/// The byte figure rides along on the same call, so a fire that forgot to carry
/// its bytes fails here rather than reading as a silent zero.
#[test]
fn a_fire_counts_as_an_ask_and_carries_its_bytes() {
    let before = totals();
    note_ask();
    note_fire(11_109_496);
    let after = totals();
    assert_eq!(after.asks - before.asks, 2, "one plain ask and one fire");
    assert_eq!(after.fires - before.fires, 1);
    assert_eq!(after.bytes - before.bytes, 11_109_496);
}
