//! **What `SourceHandler::release_data` has actually given back** — the fires
//! counter behind the idle-layer release pass.
//!
//! Product telemetry, not a campaign instrument, on the terms of
//! [`crate::floor_ledger`]: always on, no feature gate, every write one
//! `fetch_add` with [`Relaxed`] ordering on a `static`. The sentence that
//! reports these numbers is written by `squallar-app` beside the raster lines,
//! so no formatting happens on any path that increments.
//!
//! # Why this exists at all
//!
//! [`crate::ui::Gui::release_data_of_layers_no_pane_draws`] runs every frame
//! over every layer no pane draws, and until this ledger there was **no
//! reading anywhere of whether it had ever released anything**. That is the
//! shape of a cut that is banked and never executes: the pass is unconditional
//! and cheap, so it looks live on every leg, while the layers it is supposed to
//! empty may have been enabled for the whole session and handed it nothing. A
//! byte figure from the census cannot separate those two — a layer that was
//! never on and a layer that was released both read zero — and a count from a
//! unit test says only that the code path compiles.
//!
//! # The denominators — three, and they are never added
//!
//! * [`Totals::asks`] counts **layers asked**: one per layer per frame the
//!   pass reached a handler at all. This is the pass's own rate, and on an
//!   ordinary session it is large and means nothing on its own — a first
//!   launch has four gridded layers off and asks each of them sixty times a
//!   second.
//! * [`Totals::fires`] counts **releases that gave something back**: one per
//!   ask whose `release_data` answered `true`. This is the figure a cut is
//!   claimed on. A leg whose `fires` is zero released nothing, whatever its
//!   byte figures say and whatever the code does.
//! * [`Totals::bytes`] sums, over those fires, the drop in the handler's own
//!   [`squallar_source::handler::SourceHandler::resident_source_bytes`] across
//!   the call. It is the **decode-level** figure — a count of bytes the
//!   handler says it stopped holding — so it is load-immune in the way a
//!   process high-water mark is not.
//!
//! `asks` is the floor under `fires` and `fires` is the floor under a nonzero
//! `bytes`; adding any two of them means nothing.
//!
//! # What `bytes` is not
//!
//! It is not RSS and it is not a promise the allocator returned anything to
//! the operating system. It is what the layer stopped counting itself as
//! holding, measured through the same accessor the `overlay grids` census
//! family is summed from, so the two move together by construction.
//!
//! A handler whose `resident_source_bytes` does not fall — every feature layer
//! that releases a `Vec` the trait's default figure never counted — fires here
//! with a zero byte delta, which is the honest reading rather than a silence.
//!
//! # Testing these counters
//!
//! They are process-global `static`s and
//! [`crate::ui::Gui::release_data_of_layers_no_pane_draws`] writes to them on
//! **every frame**, so any test in a binary that drives a frame shares them
//! with a live writer running on another thread.
//!
//! **Reading a delta instead of a level does not make that safe.** A delta is
//! immune to whatever the counters held *before* the window and completely
//! defenceless against anything written *inside* it, which is precisely what a
//! concurrently scheduled frame is. A test that needs an exact difference
//! needs a process with no frame driver in it — put it in its own file under
//! `tests/`, as `tests/release_ledger_fire_is_an_ask.rs` does. Widening its
//! assertion to `>=` instead would only hide the collision, and would stop the
//! test detecting the lost fire it exists for.

use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

/// Layers the idle pass asked. See the module doc's denominator.
static ASKS: AtomicU64 = AtomicU64::new(0);
/// Asks that released something. See the module doc's denominator.
static FIRES: AtomicU64 = AtomicU64::new(0);
/// Bytes those fires gave back, at the handler's own accounting.
static BYTES: AtomicU64 = AtomicU64::new(0);

/// A reading of all three counters, taken together.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Totals {
    /// Layers asked by the idle release pass.
    pub asks: u64,
    /// Asks whose `release_data` answered `true`.
    pub fires: u64,
    /// Bytes given back across those fires, at the handler's own accounting.
    pub bytes: u64,
}

impl Totals {
    /// How far along this ledger is, as one number, so a caller can tell
    /// "nothing has happened since I last looked" in a single compare.
    ///
    /// **`fires` and `bytes`, never `asks`.** The pass asks on every frame of
    /// every session, so folding `asks` in would make this line speak on every
    /// telemetry tick forever and say nothing — the exact noise the
    /// `totals_if_moved` shape exists to remove.
    fn progress(&self) -> u64 {
        self.fires.wrapping_add(self.bytes)
    }
}

/// Record one ask that released nothing.
pub fn note_ask() {
    ASKS.fetch_add(1, Relaxed);
}

/// Record one ask that released something, and what it gave back.
///
/// `bytes` is the fall in the handler's own `resident_source_bytes` across the
/// call; zero is a legitimate reading for a layer whose data that figure never
/// counted, and it still counts as a fire.
pub fn note_fire(bytes: u64) {
    ASKS.fetch_add(1, Relaxed);
    FIRES.fetch_add(1, Relaxed);
    BYTES.fetch_add(bytes, Relaxed);
}

/// Read every counter. Three [`Relaxed`] loads, not an atomic snapshot, on the
/// same terms as [`crate::overlay_cache::ledger::totals`].
pub fn totals() -> Totals {
    Totals {
        asks: ASKS.load(Relaxed),
        fires: FIRES.load(Relaxed),
        bytes: BYTES.load(Relaxed),
    }
}

/// The last [`Totals::progress`] a caller was handed by [`totals_if_moved`].
static REPORTED: AtomicU64 = AtomicU64::new(0);

/// [`totals`], but only when a release has happened since the last time this
/// was asked — the telemetry writer's read, so a session in which no layer is
/// ever switched off writes no line at all.
pub fn totals_if_moved() -> Option<Totals> {
    let totals = totals();
    let progress = totals.progress();
    if REPORTED.swap(progress, Relaxed) == progress {
        return None;
    }
    Some(totals)
}

#[cfg(test)]
mod tests {
    use super::*;

    // `a_fire_counts_as_an_ask_and_carries_its_bytes` is deliberately NOT here.
    // It reads the counters twice and asserts an exact difference, and this
    // binary drives frames — so the production writer runs inside its window.
    // It lives in `tests/release_ledger_fire_is_an_ask.rs`, whose process
    // drives none. See this module's doc.

    /// **`asks` is deliberately outside `progress`.** A pass that asked and
    /// released nothing must not make the telemetry writer emit a line — see
    /// [`Totals::progress`].
    #[test]
    fn an_ask_that_released_nothing_is_not_progress() {
        let quiet = Totals {
            asks: 9_000,
            fires: 3,
            bytes: 12,
        };
        let same_releases = Totals {
            asks: 90_000,
            fires: 3,
            bytes: 12,
        };
        assert_eq!(quiet.progress(), same_releases.progress());
    }
}
