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
//! **One clock, many timelines.** A pane has one clock and, since `5ef52be5`,
//! one timeline per frame-series layer it animates — so a single settled
//! instant can be a hole in several layers at once and has to be asked of
//! every one of them. This module read `transport_state` alone, which on a
//! radar-plus-satellite pane refilled the transport and left the secondaries
//! holding frames stamped *after* the clock; `overlay_texture_on_screen`
//! correctly refuses to draw those, and nothing ever widened their window, so
//! they went blank and stayed blank. The walk is
//! `PaneState::animating_layers`, each layer carrying its own `span_secs` and
//! answering `SourceHandler::residency_for` for itself.
//!
//! **What this deliberately does not do.** It does not re-fetch a frame that
//! was fetched and dropped. Eviction is anchored on the pane's own clock
//! (`app_fetch::append_polled_frame`), so the instant a scrubbed pane is parked
//! on is the anchor of its own retention window and cannot age out from under
//! it; asking the network again for bytes the app chose to drop would be
//! paying twice for one decision.

use chrono::NaiveDateTime;
use squallar_egui::pane::{PaneState, TimeMode};
use squallar_overlays::render::overlay_state::OverlayRegistry;
use squallar_source::id::LayerId;
use squallar_source::time::Residency;

/// How long a pane's clock must name one unserved instant before that instant
/// is asked for.
///
/// The same 100 ms the overlay rasteriser treats as "the gesture is over" —
/// `squallar_egui::overlay_cache::SETTLE_REPAINT_DELAY`, reused rather than
/// re-chosen, so the app has one idea of when a hand has stopped moving. It is
/// what keeps a drag that passes through many instants from asking for each of
/// them, without putting a single frame of latency in the rail: the check is a
/// binary search over one pane's frame list and the ask itself is a spawned
/// task.
pub(crate) const REFILL_SETTLE: std::time::Duration =
    squallar_egui::overlay_cache::SETTLE_REPAINT_DELAY;

/// One pane's ask: the layer to list, and the window to list over.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RefillAsk {
    pub pane_idx: usize,
    pub layer: LayerId,
    pub range: (NaiveDateTime, NaiveDateTime),
}

/// **The instant this pane depicts that at least one layer it is animating has
/// no frame for**, or `None` when every one of them answers the clock it is
/// on.
///
/// A pane has one clock, so there is one instant to name however many
/// timelines are running; [`unserved_layers`] is which of them the instant is
/// a hole in. Reading the transport alone — what this did until the walk
/// landed — answered `None` for a pane whose satellite had gone blank under a
/// radar transport that was perfectly well served.
///
/// Four things make an instant *not* unserved for a layer, and each is a
/// different reason:
///
/// * the loop is not settled — `FetchingScanList` and `Rendering` are a loop
///   already being supplied, and asking again would be asking twice;
/// * the loop holds no frames — see above, that is a loop mid-build;
/// * the clock is `Live` — "the newest there is" always names a frame when
///   there is one;
/// * a frame qualifies — the ordinary case, and the one that must stay
///   silent, because a refill on every clock move is a refetch on every scrub.
pub(crate) fn unserved_instant(pane: &PaneState) -> Option<NaiveDateTime> {
    let TimeMode::AsOf(instant) = pane.time.mode else {
        return None;
    };
    (!unserved_layers(pane).is_empty()).then_some(instant)
}

/// **Which of the layers this pane is animating have no frame for the instant
/// it depicts**, bottom to top, and empty when none do.
///
/// `animating_layers` and not `transport_state`. The transport is still in
/// this set whenever it is the hole — `is_render_ready` is one of
/// `Ready`/`Playing`/`Paused`, none of which is `Inactive`, so a timeline that
/// qualified under the single-layer reading is animating by construction — and
/// every secondary timeline beside it is now asked the same question, which is
/// the whole of what changed.
pub(crate) fn unserved_layers(pane: &PaneState) -> Vec<LayerId> {
    let mode = pane.time.mode;
    if !matches!(mode, TimeMode::AsOf(_)) {
        return Vec::new();
    }
    pane.animating_layers()
        .filter(|slot| {
            let ls = &slot.time;
            ls.is_render_ready() && !ls.frames.is_empty() && ls.qualifying_frame_at(mode).is_none()
        })
        .map(|slot| slot.id.clone())
        .collect()
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
///
/// **`hold` is the layer's own answer, and it can only widen this.** A window
/// one span wide names its own first stop, and the picture that stop is drawn
/// from is the frame at or before it — earlier than the window itself whenever
/// the layer's steps are coarser than where the scrub landed. That is the same
/// leading-partial-step fact a listing clipped at its own start gets wrong. So
/// the start is the earlier of the span's edge and what
/// [`Residency::extent`] reaches back to, and never the later: a layer that
/// knows nothing back there answers empty and the window is exactly the span
/// it always was.
pub(crate) fn refill_range(
    hold: &Residency,
    span_secs: u64,
    instant: NaiveDateTime,
) -> (NaiveDateTime, NaiveDateTime) {
    let span_start = instant - chrono::Duration::seconds(span_secs as i64);
    let start = hold
        .extent()
        .map_or(span_start, |(held, _)| held.min(span_start));
    (start, instant)
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
    /// Called once per frame with the pane vector, the registry and the
    /// frame's clock. At most one ask **per unserved layer** per pane per
    /// settled instant: a pane whose clock is still travelling re-arms the
    /// timer instead of asking, and a pane parked on an instant that is still
    /// unserved after the asks went out is silent from then on.
    ///
    /// The mark is per pane and not per layer because the *clock* is per pane:
    /// the layers that are holes at one settled instant became holes together
    /// and are asked together, in one pass.
    ///
    /// `panes` is taken mutably for [`PaneState::hydrate_layer_states`] alone
    /// — no handler is asked anything about a pane whose slots have no live
    /// state — and only for a pane that is about to ask.
    pub(crate) fn settled_asks(
        &mut self,
        panes: &mut [PaneState],
        overlays: &OverlayRegistry,
        now: web_time::Instant,
    ) -> Vec<RefillAsk> {
        if self.panes.len() < panes.len() {
            self.panes.resize(panes.len(), None);
        }
        let mut asks = Vec::new();
        for pane_idx in 0..panes.len() {
            let slot = &mut self.panes[pane_idx];
            let Some(target) = unserved_instant(&panes[pane_idx]) else {
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
                        asks.extend(layer_asks(panes, overlays, pane_idx, target));
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

/// **One pane's asks at one settled instant** — one per layer the instant is a
/// hole in, each over that layer's own window.
///
/// Every layer brings its own `span_secs`, the width its own listing was asked
/// over rather than the transport's: a satellite loop listed over twelve hours
/// beside a radar loop listed over one has to be refilled over twelve, and
/// handing it the transport's hour would list a window its own frames are an
/// hour apart inside.
fn layer_asks(
    panes: &mut [PaneState],
    overlays: &OverlayRegistry,
    pane_idx: usize,
    target: NaiveDateTime,
) -> Vec<RefillAsk> {
    let layers = unserved_layers(&panes[pane_idx]);
    if layers.is_empty() {
        return Vec::new();
    }
    // Hydrated once for the pane, before any handler is asked anything about
    // it — the order `App::with_layer_pane` takes, which cannot be called here
    // because the caller already holds the panes and the registry together.
    panes[pane_idx].hydrate_layer_states(overlays, pane_idx);
    let view = panes[pane_idx].view(pane_idx);
    layers
        .into_iter()
        .map(|layer| {
            let span_secs = panes[pane_idx].time_state(&layer).span_secs;
            // The window the span alone would reach over, offered to the layer
            // as the two stops it has to answer for. A layer that knows a
            // frame before the earlier stop says so, and the ask widens back
            // to reach it.
            let stops = [target - chrono::Duration::seconds(span_secs as i64), target];
            let hold = overlays.residency_for(&layer, &view.layer(&layer), &stops);
            RefillAsk {
                pane_idx,
                layer,
                range: refill_range(&hold, span_secs, target),
            }
        })
        .collect()
}

#[cfg(test)]
#[path = "loop_refill/tests.rs"]
mod tests;
