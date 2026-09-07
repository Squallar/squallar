//! **The O(items) walks the dispatch path performs, counted.**
//!
//! `spawn_overlay_render` asks a handler for two things per dispatch, back to
//! back: the paint input (`prepare_job`) and the page-side hit list
//! (`hit_items`). Both are, in the shape a handler reaches for first, one full
//! iteration of that layer's item list. The `frame dispatch (prepare)` and
//! `frame dispatch (hitmap)` cuts time those two calls, and a time is not a
//! count: a cut that reads high says nothing about whether the list was walked
//! once, twice, or not at all.
//!
//! So this counts them. Three running totals, incremented where the walk
//! actually happens rather than where a reader of the source would expect it
//! to:
//!
//! * [`note_dispatch`] — one per `OverlayRegistry::prepare_job`, which is the
//!   denominator every ratio here is quoted against.
//! * [`note_paint_walk`] — one per paint-input build that actually ran, taken
//!   inside the memo, so a memo HIT increments nothing. This is the figure
//!   that separates "the input was rebuilt" from "the input was handed back".
//! * [`note_hit_walk`] — one per hit list materialised row-by-row. A
//!   [`HitItems::Slab`](crate::hit::HitItems::Slab) increments nothing,
//!   because it walks nothing.
//!
//! # Scope: per thread, and that is the whole count
//!
//! The counters are thread-local. Every call site is on the frame thread —
//! `prepare_job` and `hit_items` are reached only from the dispatch tail, which
//! runs there and nowhere else — so a per-thread count is the whole count for
//! the thread that has one, and zero everywhere else. Stated rather than
//! assumed: if a walk is ever moved off the frame thread, this reads zero for
//! it, and a zero here would then be a gap in the instrument and not a saving.
//!
//! Thread-local also makes the gate test exact. `cargo test` runs each test on
//! its own thread, so a delta taken around a loop counts that loop's walks and
//! cannot pick up a concurrent test's.

use std::cell::Cell;

thread_local! {
    static DISPATCHES: Cell<u64> = const { Cell::new(0) };
    static PAINT_WALKS: Cell<u64> = const { Cell::new(0) };
    static HIT_WALKS: Cell<u64> = const { Cell::new(0) };
}

/// The three running totals, read together so a ratio is taken across one
/// instant rather than three.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WalkTotals {
    /// `OverlayRegistry::prepare_job` calls — the denominator.
    pub dispatches: u64,
    /// Paint-input builds that ran (memo misses).
    pub paint_walks: u64,
    /// Hit lists materialised one row per item.
    pub hit_walks: u64,
}

impl WalkTotals {
    /// What happened between `earlier` and `self` — the only honest way to
    /// read a running total, since these are cumulative from process start
    /// and a bare reading carries every dispatch since boot.
    pub fn since(self, earlier: Self) -> Self {
        Self {
            dispatches: self.dispatches.saturating_sub(earlier.dispatches),
            paint_walks: self.paint_walks.saturating_sub(earlier.paint_walks),
            hit_walks: self.hit_walks.saturating_sub(earlier.hit_walks),
        }
    }

    /// Every item-list walk in the dispatch path, both halves.
    pub fn walks(self) -> u64 {
        self.paint_walks.saturating_add(self.hit_walks)
    }
}

fn bump(cell: &'static std::thread::LocalKey<Cell<u64>>) {
    cell.with(|c| c.set(c.get().saturating_add(1)));
}

/// One dispatch asked for. Called by `OverlayRegistry::prepare_job`, which the
/// dispatch tail calls exactly once per layer it dispatches.
pub fn note_dispatch() {
    bump(&DISPATCHES);
}

/// Which half of the dispatch a walk belongs to. A memo does not know this
/// about itself — it is a fact about what the memo holds — so it is set where
/// the memo is constructed and travels with it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WalkKind {
    /// The rows `prepare_job` describes.
    Paint,
    /// The item list `hit_items` answers.
    Hit,
}

/// One paint input actually built. Called from inside the built-input memo, on
/// the miss path only.
pub fn note_paint_walk() {
    bump(&PAINT_WALKS);
}

/// One walk of whichever half `kind` names.
pub fn note_walk(kind: WalkKind) {
    match kind {
        WalkKind::Paint => note_paint_walk(),
        WalkKind::Hit => note_hit_walk(),
    }
}

/// One hit list built one row per item.
pub fn note_hit_walk() {
    bump(&HIT_WALKS);
}

/// The totals for this thread, right now.
pub fn totals() -> WalkTotals {
    WalkTotals {
        dispatches: DISPATCHES.with(Cell::get),
        paint_walks: PAINT_WALKS.with(Cell::get),
        hit_walks: HIT_WALKS.with(Cell::get),
    }
}

#[cfg(test)]
mod tests {
    use super::{WalkTotals, note_dispatch, note_hit_walk, note_paint_walk, totals};

    /// A delta over a window is what every reading here is, and it must be the
    /// window's own count rather than the process's.
    #[test]
    fn a_delta_counts_the_window_and_not_the_history() {
        note_dispatch();
        note_paint_walk();
        let before = totals();

        note_dispatch();
        note_dispatch();
        note_hit_walk();

        let window = totals().since(before);
        assert_eq!(window.dispatches, 2);
        assert_eq!(window.paint_walks, 0, "the build was before the window");
        assert_eq!(window.hit_walks, 1);
        assert_eq!(window.walks(), 1);
    }

    /// `since` never wraps: a reader that hands the arguments over the wrong
    /// way round gets zero, not `u64::MAX` dressed as a finding.
    #[test]
    fn a_reversed_delta_saturates_rather_than_wrapping() {
        let low = WalkTotals {
            dispatches: 1,
            paint_walks: 1,
            hit_walks: 1,
        };
        let high = WalkTotals {
            dispatches: 9,
            paint_walks: 9,
            hit_walks: 9,
        };
        assert_eq!(low.since(high), WalkTotals::default());
    }
}
