//! **When a looping pane's clock names an instant its own frames cannot
//! answer for, ask the layer for that instant.**
//!
//! WI-3 stopped the fabrication: a `FrameSeries` layer whose clock sits before
//! every frame it holds now draws *nothing*, instead of presenting the oldest
//! frame it happened to have — a picture valid **after** the moment asked
//! about — as the answer. That is the correct refusal, and it left a hole: a
//! loop's frames come from the one listing captured when the loop was enabled,
//! and nothing ever widened that window, so "draws nothing" was permanent
//! rather than the pause before an answer.
//!
//! This module is the missing half. It decides *that* an instant is unserved
//! and *which window to ask for*; the shell's
//! `App::refill_unserved_loop_windows` turns that decision into the same
//! `create_frame_list_task` dispatch a freshly enabled loop makes, and the
//! existing listing-acceptance path builds the frames. Nothing here fetches,
//! decodes or draws.
//!
//! **The one case, and it is exactly one.** `LayerTimeState::qualifying_frame_at`
//! answers `None` for a non-empty frame list if and only if no frame is stamped
//! at or before the clock — that is, the clock is *earlier than the oldest
//! frame held*. Later than the newest is not a hole: the latest frame at or
//! before the clock is still the newest one. So the unserved instant is always
//! before the loaded window, and the window asked for is always disjoint from
//! the one already held.
//!
//! **What this deliberately does not do.** It does not re-fetch a frame that
//! was fetched and dropped. Eviction is anchored on the pane's own clock
//! (`app_fetch::append_polled_frame`), so the instant a scrubbed pane is parked
//! on is the anchor of its own retention window and cannot age out from under
//! it; asking the network again for bytes the app chose to drop would be
//! paying twice for one decision.

use chrono::NaiveDateTime;
use rustdar_egui::pane::{PaneState, TimeMode};
use rustdar_source::id::LayerId;

/// How long a pane's clock must name one unserved instant before that instant
/// is asked for.
///
/// The same 100 ms the overlay rasteriser treats as "the gesture is over" —
/// `rustdar_egui::overlay_cache::SETTLE_REPAINT_DELAY`, reused rather than
/// re-chosen, so the app has one idea of when a hand has stopped moving. It is
/// what keeps a drag that passes through many instants from asking for each of
/// them, without putting a single frame of latency in the rail: the check is a
/// binary search over one pane's frame list and the ask itself is a spawned
/// task.
pub(crate) const REFILL_SETTLE: std::time::Duration =
    rustdar_egui::overlay_cache::SETTLE_REPAINT_DELAY;

/// One pane's ask: the layer to list, and the window to list over.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RefillAsk {
    pub pane_idx: usize,
    pub layer: LayerId,
    pub range: (NaiveDateTime, NaiveDateTime),
}

/// **The instant this pane depicts that its transport layer has no frame
/// for**, or `None` when the pane is answering the clock it is on.
///
/// Read through `transport_state`, so the question is asked of whichever layer
/// the pane's transport addresses rather than of radar by name.
///
/// Four things make an instant *not* unserved, and each is a different reason:
///
/// * the loop is not settled — `FetchingScanList` and `Rendering` are a loop
///   already being supplied, and asking again would be asking twice;
/// * the loop holds no frames — see above, that is a loop mid-build;
/// * the clock is `Live` — "the newest there is" always names a frame when
///   there is one;
/// * a frame qualifies — the ordinary case, and the one that must stay
///   silent, because a refill on every clock move is a refetch on every scrub.
pub(crate) fn unserved_instant(pane: &PaneState) -> Option<NaiveDateTime> {
    let ls = pane.transport_state();
    if !ls.is_render_ready() || ls.frames.is_empty() {
        return None;
    }
    let mode = pane.time.mode;
    let TimeMode::AsOf(instant) = mode else {
        return None;
    };
    if ls.qualifying_frame_at(mode).is_some() {
        return None;
    }
    Some(instant)
}

/// **The bound**, and the only one there is: a refill asks for exactly one
/// span, ending at the instant that was asked about.
///
/// Not "everything between the loaded window and here". Scrubbing an hour back
/// and scrubbing to 1997 cost the same single listing over the same
/// `span_secs`; the distance travelled never appears in the size of the
/// question. That is what stops a wild clock — a group-synced pane, a restored
/// config, a fat-fingered date — from spawning a backfill proportional to how
/// wrong it is.
///
/// Ending *at* the instant rather than centred on it is the contract's own
/// shape: `FrameSeries` presents the latest frame at or before the clock, so
/// the frames that answer the question are the ones behind it.
pub(crate) fn refill_range(
    span_secs: u64,
    instant: NaiveDateTime,
) -> (NaiveDateTime, NaiveDateTime) {
    (
        instant - chrono::Duration::seconds(span_secs as i64),
        instant,
    )
}

/// What one pane's clock has been doing, so that a hand still moving is not
/// mistaken for a question.
#[derive(Clone, Debug)]
struct Watch {
    /// The unserved instant this pane's clock is naming.
    target: NaiveDateTime,
    /// When it started naming it.
    since: web_time::Instant,
    /// Whether a listing has already been asked for over `target`. The
    /// idempotence: an instant with genuinely no data at the source stays
    /// unserved forever, and must still be asked about exactly once.
    asked: bool,
}

/// **The debounce and the dedupe**, per pane.
///
/// Held by `App` and walked once a frame. It is deliberately not a timer or a
/// queue: the pane's clock is the only input, and "has it been still for
/// [`REFILL_SETTLE`]" is the whole rule.
#[derive(Default)]
pub(crate) struct LoopRefillWatch {
    panes: Vec<Option<Watch>>,
}

impl LoopRefillWatch {
    /// **The asks whose instants have settled**, marking each one asked.
    ///
    /// Called once per frame with the pane vector and the frame's clock. At
    /// most one ask per pane per settled instant: a pane whose clock is still
    /// travelling re-arms the timer instead of asking, and a pane parked on an
    /// instant that is still unserved after the ask went out is silent from
    /// then on.
    pub(crate) fn settled_asks(
        &mut self,
        panes: &[PaneState],
        now: web_time::Instant,
    ) -> Vec<RefillAsk> {
        if self.panes.len() < panes.len() {
            self.panes.resize(panes.len(), None);
        }
        let mut asks = Vec::new();
        for (pane_idx, pane) in panes.iter().enumerate() {
            let slot = &mut self.panes[pane_idx];
            let Some(target) = unserved_instant(pane) else {
                // The clock answers again — through a refill, a scrub back, or
                // the loop being switched off. Forget it, so returning to the
                // same hole later asks again.
                *slot = None;
                continue;
            };
            match slot {
                // Still travelling: a new instant restarts the settle.
                Some(watch) if watch.target != target => {
                    *slot = Some(Watch {
                        target,
                        since: now,
                        asked: false,
                    });
                }
                Some(watch) => {
                    if !watch.asked && now.duration_since(watch.since) >= REFILL_SETTLE {
                        watch.asked = true;
                        asks.push(RefillAsk {
                            pane_idx,
                            layer: pane.transport_layer().clone(),
                            range: refill_range(pane.transport_state().span_secs, target),
                        });
                    }
                }
                None => {
                    *slot = Some(Watch {
                        target,
                        since: now,
                        asked: false,
                    });
                }
            }
        }
        asks
    }

    /// Forget what pane `pane_idx` was waiting on — the ask could not be built
    /// or was refused, so the next settle may try again rather than the pane
    /// being marked asked for something that never went out.
    pub(crate) fn forget(&mut self, pane_idx: usize) {
        if let Some(slot) = self.panes.get_mut(pane_idx) {
            *slot = None;
        }
    }
}

#[cfg(test)]
#[path = "loop_refill/tests.rs"]
mod tests;
