//! **The tail of a frame's GUI actions that did not fit in it** — the queue
//! behind [`squallar_device_profile::constants::ACTION_BUDGET_PER_FRAME`], and
//! the fires counter that says how often it actually bit.
//!
//! # Why this exists
//!
//! `App::process_gui_actions` was a `for action in actions` with no bound at
//! all, three lines from an arrival drain that stops at
//! `INGEST_BUDGET_PER_FRAME` and a teardown drain that stops at
//! `DEFERRED_DROP_BUDGET_PER_FRAME`. The inconsistency was measurable: on
//! native scene A (`pan-zoom-2d`, one pane KTLX, all eighteen layers,
//! 1920x1080) the `frame worst:` latch caught one frame spending **8,172 µs of
//! its 8,436 µs in `post_handle`** — the frame on which every enabled layer's
//! first fetch was built and spawned back to back.
//!
//! # Deferred, never dropped, never reordered
//!
//! What the budget stops is queued here in emission order and drains **ahead
//! of** the next frame's own actions, so the stream one `handle_gui_action`
//! sees is the stream it saw before, later. That matters because the actions
//! are not all idempotent: `Exit`, `ToggleLoopPlayback` and `StepLoopFrame`
//! are one-shot edges the UI emits once and never again, and a budget that
//! dropped its tail and trusted re-emission would silently eat them.
//!
//! # The one thing that IS collapsed, and why it has to be
//!
//! A layer's fetch ask stands until the fetch is spawned:
//! `Gui::check_auto_polls` re-emits `FetchOverlay` on every frame a layer is
//! due, and what clears "due" is `App::fetch_overlay` marking the layer
//! fetching — which is the very call the budget deferred. So a deferred ask is
//! re-emitted on the frame after it, and a queue that appended both would
//! spawn the same download twice. [`ActionQueue::would_duplicate`] drops the
//! re-emission and keeps the queued one, which is the older ask; the count of
//! those is [`Totals::coalesced`], and it is a floor under nothing else here.
//!
//! Collapsed on the **whole ask** — same variant, same layer, same pane index
//! — and never on the layer alone. Two panes owed a round of the same layer
//! ask for two different selections (`Gui::panes_owed_a_round`), and merging
//! them would leave one pane drawing a product it did not pick.
//!
//! # `RenderOverlay` is never deferred and never budgeted
//!
//! It is the one action the loop does not *handle*: `process_gui_actions`
//! intercepts it into a vector and the cost is in the dispatch that follows,
//! which is its own `post` cut with its own decomposition
//! (`frame_ledger::DispatchCuts`). Charging a raster ask to this budget would
//! price it twice and delay a picture the pane is already showing a stale
//! version of, for no saving at all.
//!
//! # The denominators — four, and they are never added
//!
//! * [`Totals::handled`] is **actions run on the frame thread**: one per
//!   action `handle_gui_action` was called for. The pass's own rate.
//! * [`Totals::bites`] is **frames on which the budget stopped the loop**.
//!   This is the fires counter. A leg whose `bites` is zero paid nothing for
//!   this mechanism and saved nothing by it, whatever its frame figures say.
//! * [`Totals::deferred`] is **actions carried to a later frame**, summed over
//!   those bites. Always at least `bites` when `bites` is nonzero.
//! * [`Totals::coalesced`] is **re-emitted asks dropped** against a queue that
//!   already held the identical one. It is a saving in downloads, not in
//!   frame time, and it exists only because the queue exists.
//!
//! # Testing these counters
//!
//! `squallar_egui::release_ledger`'s note applies verbatim: they are
//! process-global `static`s written by a live frame driver, so a test wanting
//! an exact difference needs a process with no frame driver in it.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

use squallar_egui::actions::GuiAction;

/// Actions `handle_gui_action` was called for. See the module doc.
static HANDLED: AtomicU64 = AtomicU64::new(0);
/// Frames the budget stopped the loop on — the fires counter.
static BITES: AtomicU64 = AtomicU64::new(0);
/// Actions carried to a later frame across those bites.
static DEFERRED: AtomicU64 = AtomicU64::new(0);
/// Re-emitted asks dropped against an identical queued one.
static COALESCED: AtomicU64 = AtomicU64::new(0);
/// The deepest the queue has ever been, so a reader can tell a budget that
/// bites once and recovers from one that is permanently behind.
static DEEPEST: AtomicU64 = AtomicU64::new(0);

/// A reading of every counter, taken together.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Totals {
    /// Actions run on the frame thread.
    pub(crate) handled: u64,
    /// Frames on which the budget stopped the loop.
    pub(crate) bites: u64,
    /// Actions carried to a later frame.
    pub(crate) deferred: u64,
    /// Re-emitted asks dropped against an identical queued one.
    pub(crate) coalesced: u64,
    /// The deepest the queue has been.
    pub(crate) deepest: u64,
}

impl Totals {
    /// How far along this ledger is, so a caller can tell "nothing has
    /// happened since I last looked" in one compare.
    ///
    /// **Never `handled`.** Every frame that emits an action moves it, so
    /// folding it in would make the sentence speak on every telemetry tick of
    /// every session and say nothing — `release_ledger::Totals::progress`'s
    /// argument verbatim.
    fn progress(self) -> u64 {
        self.bites
            .wrapping_add(self.deferred)
            .wrapping_add(self.coalesced)
    }
}

/// Read every counter. Five [`Relaxed`] loads, not an atomic snapshot.
pub(crate) fn totals() -> Totals {
    Totals {
        handled: HANDLED.load(Relaxed),
        bites: BITES.load(Relaxed),
        deferred: DEFERRED.load(Relaxed),
        coalesced: COALESCED.load(Relaxed),
        deepest: DEEPEST.load(Relaxed),
    }
}

/// The last [`Totals::progress`] a caller was handed by [`totals_if_moved`].
static REPORTED: AtomicU64 = AtomicU64::new(0);

/// [`totals`], but only when the budget has done something since the last
/// time this was asked. `None` is the reading of a session that never overran
/// a frame's action allowance, which is most of them.
pub(crate) fn totals_if_moved() -> Option<Totals> {
    let totals = totals();
    let progress = totals.progress();
    if REPORTED.swap(progress, Relaxed) == progress {
        return None;
    }
    Some(totals)
}

/// The actions a frame could not afford, in the order they were emitted.
///
/// Owned by the `App`. Empty on every frame that finished its list, which is
/// every frame of an ordinary session.
#[derive(Default)]
pub(crate) struct ActionQueue {
    queued: VecDeque<GuiAction>,
}

impl ActionQueue {
    /// Take the whole tail, leaving the queue empty — the start of a frame's
    /// work list, ahead of that frame's own actions.
    pub(crate) fn take(&mut self) -> VecDeque<GuiAction> {
        std::mem::take(&mut self.queued)
    }

    /// Carry one action to a later frame.
    pub(crate) fn defer(&mut self, action: GuiAction) {
        DEFERRED.fetch_add(1, Relaxed);
        self.queued.push_back(action);
        DEEPEST.fetch_max(self.queued.len() as u64, Relaxed);
    }

    /// How many actions are waiting. The assertion surface of the budget's
    /// tests: `defer` and `take` are the only writers and a count is what
    /// separates "deferred" from "dropped".
    #[cfg(test)]
    pub(crate) fn queued_len(&self) -> usize {
        self.queued.len()
    }

    /// The layer the ask at the head of the queue is for, or `None` when the
    /// queue is empty or its head is not a fetch ask. Reads order, which is
    /// the property the seam between two frames is about.
    #[cfg(test)]
    pub(crate) fn head_fetch_layer(&self) -> Option<squallar_source::id::LayerId> {
        fetch_ask(self.queued.front()?).map(|(_, kind, _)| kind.clone())
    }
}

/// Whether `action` is the re-emission of an ask `work` is already carrying.
///
/// Answered against the frame's **work list** rather than the queue, because
/// the queue has already been drained into it by the time this is asked.
/// Scans only for the two re-emitted variants and only over a non-empty list,
/// so an ordinary frame pays one `matches!` and no walk at all.
pub(crate) fn would_duplicate(work: &VecDeque<GuiAction>, action: &GuiAction) -> bool {
    let Some(ask) = fetch_ask(action) else {
        return false;
    };
    work.iter().any(|queued| fetch_ask(queued) == Some(ask))
}

/// The ask a re-emitted fetch action names, or `None` for every action the UI
/// emits once. Borrowed rather than cloned: `LayerId` is compared, not kept.
fn fetch_ask(action: &GuiAction) -> Option<(bool, &squallar_source::id::LayerId, usize)> {
    match action {
        GuiAction::FetchOverlay { kind, pane_idx } => Some((false, kind, *pane_idx)),
        GuiAction::RefreshOverlay { kind, pane_idx } => Some((true, kind, *pane_idx)),
        _ => None,
    }
}

/// Record one action run on the frame thread.
pub(crate) fn note_handled() {
    HANDLED.fetch_add(1, Relaxed);
}

/// Record one frame on which the budget stopped the loop — the fires counter.
/// Called once per frame, not once per deferred action.
pub(crate) fn note_bite() {
    BITES.fetch_add(1, Relaxed);
}

/// Record one re-emitted ask dropped against an identical queued one.
pub(crate) fn note_coalesced() {
    COALESCED.fetch_add(1, Relaxed);
}
