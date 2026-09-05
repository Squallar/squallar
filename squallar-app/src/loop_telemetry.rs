//! **What the loops on screen are actually holding**, as one telemetry
//! sentence.
//!
//! Every other `frame *` line prices the frame thread. This one prices the
//! thing that makes a frame expensive when loops are running, and it exists
//! because the campaign's scene E asked four questions no counter in the tree
//! could answer:
//!
//! * **How many layers are really looping** — a scene seeded to loop
//!   everything and a scene that armed one layer produce the same screenshot
//!   at a glance and completely different frame costs.
//! * **Frames RESIDENT against frames LISTED.** A loop that lists fourteen
//!   frames and holds three draws a three-frame animation while every phase
//!   reads healthy — the repo's documented "silent partial success" shape.
//!   The two counts are on the same line precisely so the gap cannot be
//!   reported as an absence.
//! * **The pool, against the bracket it lives in.** `LoopPool::back_off`
//!   halves the application's whole loop allowance after a lost surface and
//!   says so at `warn`; a reading that carries the pool beside its floor and
//!   ceiling makes the demotion a figure on every row instead of a log line
//!   somebody has to have kept.
//! * **The share, against the pool.** `LoopPoolState` holds an allocation
//!   across a `LOOP_POOL_DWELL_FRAMES` dwell and a `LOOP_POOL_HYSTERESIS`
//!   dead band, so the division in force is not a function of what is on
//!   screen right now. `share` beside `pool` is what shows it.
//!
//! **Denominators, stated once, never added.** `listed` counts frame SLOTS
//! across every animating layer of every pane — what the listings became
//! after sampling to the cap. `resident` is the subset of those holding a
//! picture; `in flight` the subset with a render out; `failed` the subset a
//! render already refused. `shared` is a different count again: PICTURES in
//! the loop frame store that more than one pane holds — two slots on two
//! panes drawing one shared picture are two `resident` and one `shared`, so
//! it is never added to `resident` and never subtracted from it.
//!
//! (`resident`, `in flight` and `failed` are the three subsets of `listed`; a
//! render already refused. Those three are disjoint subsets of `listed` and
//! their sum is not it (a slot may be none of the three: listed, never
//! dispatched). The `allowed` group is the pool's per-view answer — a
//! ceiling on frames TEXTURED, not on slots — and `cap`/`held` are the
//! device tier's own `loop_render_budget` and `loop_frames_held`. Bytes are
//! bytes. `advance` is the playback interval the transport steps on, in
//! microseconds; it is the loop-speed setting expressed the way the loop
//! itself consumes it, and an integer because the rig's probe reads these
//! sentences with `(\d+)` groups.
//!
//! **The one EVENT count on the line is `ticks skipped`**, and it is here
//! because every figure above it is a level. `resident` against `listed` is
//! the documented gap detector, and a level cannot see this particular gap:
//! the playback tick scans forward for the first frame holding a picture and
//! stamps its clock on whatever it finds, so a loop whose frames are arriving
//! slower than it plays them runs at full wall-clock speed over half its
//! frames and reads `resident` healthy on every tick either side of the skip.
//! [`SkippedTicks`] counts those ticks as they happen — see its own note for
//! the denominator, which is ticks and never frames.
//!
//! **Product telemetry, not a campaign instrument**: it rides
//! `report_frame_telemetry`'s existing 2 s tick, walks panes only on the tick
//! that is due, and no figure it prints ever gates CI.

use squallar_egui::pane::PaneState;

/// One reading of what the application's loops hold.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct LoopState {
    /// Panes animating at least one layer.
    pub(crate) panes: usize,
    /// Animating layer slots, summed over those panes — the "how many layers
    /// are looping" figure, and never a pane count.
    pub(crate) layers: usize,
    /// Frame slots those layers hold.
    pub(crate) listed: usize,
    /// Of [`Self::listed`], the ones holding a picture.
    pub(crate) resident: usize,
    /// Of [`Self::listed`], the ones with a render out.
    pub(crate) in_flight: usize,
    /// Of [`Self::listed`], the ones a render already refused.
    pub(crate) failed: usize,
    /// The allocation in force, per view.
    pub(crate) allowed_plan: usize,
    pub(crate) allowed_section: usize,
    pub(crate) allowed_volume: usize,
    pub(crate) allowed_overlay: usize,
    /// One loop's slice of the pool, in bytes.
    pub(crate) share_bytes: usize,
    /// This tier's `loop_render_budget` — the dispatcher's own ceiling.
    pub(crate) cap: usize,
    /// This tier's `loop_frames_held`.
    pub(crate) held: usize,
    /// The application's whole loop allowance, and the bracket it lives in.
    pub(crate) pool_bytes: usize,
    pub(crate) floor_bytes: usize,
    pub(crate) ceiling_bytes: usize,
    /// The playback interval, in microseconds.
    pub(crate) advance_us: u64,
    /// Pictures in the loop frame store held by more than one pane — see
    /// the module note for the denominator.
    pub(crate) shared: usize,
}

impl LoopState {
    /// Fold one pane's loops into the counts.
    ///
    /// **Called from `App::loop_demand`'s walk**, not from a walk of its own:
    /// `app_render.rs` sits on a permanent `self.gui.` coupling ceiling that
    /// may only fall, so a new reading does not get to buy itself new reaches
    /// — it rides the pane walk the frame already makes. That is also why this
    /// takes one pane rather than an iterator.
    pub(crate) fn count_pane(&mut self, pane: &PaneState) {
        let mut animating = false;
        for slot in pane.animating_layers() {
            animating = true;
            self.layers += 1;
            for frame in &slot.time.frames {
                self.listed += 1;
                if frame.image.is_some() {
                    self.resident += 1;
                }
                if frame.render_in_flight {
                    self.in_flight += 1;
                }
                if frame.render_failed {
                    self.failed += 1;
                }
            }
        }
        if animating {
            self.panes += 1;
        }
    }
}

/// **Playback ticks that wanted a frame and did not get one**, since launch,
/// attributed to the pane whose transport ticked.
///
/// `App::advance_loop_playback` moves a playing pane's clock by scanning
/// forward from the frame it is on for the first frame holding a picture, and
/// stamps `last_advance` unconditionally — there is no branch that waits. So a
/// tick whose next frame has no picture yet does not slow down and does not
/// log: it lands on a later frame, or on nothing at all, and the loop plays a
/// thinned set at full speed. That is decimation, which the frame-density
/// ruling forbids, and before this counter existed **nothing in the process
/// counted it**.
///
/// **The denominator is TICKS, not frames.** One tick that skipped nine
/// frames counts 1, the same as one that skipped one. The question this
/// answers is "how often did playback not show the frame it was due to show",
/// and a frame count would answer a different one — and would be dominated by
/// the empty-timeline case, where every tick skips the whole list. It is
/// never added to `listed`, `resident` or `shared`, which count slots and
/// pictures on one tick rather than events over a run.
///
/// **A RUNNING TOTAL, unlike every other figure on the line**, which is a
/// level. It only rises, so two readings bracket an interval and the rig's
/// last reading in a window is the count since launch rather than the count
/// in that window.
///
/// **Attributed by pane index, not by site.** The tick walks each pane's
/// *transport* layer, which is whatever the ∞ toggle armed — radar on one
/// pane, satellite on the next — so a site is the wrong key and would be the
/// empty string for every non-radar transport. The pane is what has one
/// transport and one clock.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct SkippedTicks {
    /// Skipped ticks per pane index, grown on demand. A `Vec` and not a map
    /// because pane indices are small, dense and few, and the line walks it
    /// in index order.
    by_pane: Vec<u64>,
}

impl SkippedTicks {
    /// Record that `pane`'s tick did not land on the frame it asked for.
    pub(crate) fn note(&mut self, pane: usize) {
        if self.by_pane.len() <= pane {
            self.by_pane.resize(pane + 1, 0);
        }
        self.by_pane[pane] = self.by_pane[pane].saturating_add(1);
    }

    /// Every pane that has ever skipped, and how often — index order, and
    /// panes that never skipped are not in it.
    pub(crate) fn attributed(&self) -> impl Iterator<Item = (usize, u64)> + '_ {
        self.by_pane
            .iter()
            .enumerate()
            .filter(|&(_, n)| *n > 0)
            .map(|(idx, n)| (idx, *n))
    }

    /// Skipped ticks over every pane. The figure the line's tail carries.
    pub(crate) fn total(&self) -> u64 {
        self.by_pane
            .iter()
            .fold(0u64, |sum, n| sum.saturating_add(*n))
    }
}

/// The `loop state:` line.
///
/// **A free function returning a `String`, for the reason every other
/// telemetry sentence in this tree is one**: `.github/browser-rig/drive.py`
/// scrapes it with a regex written in another language in another directory,
/// where an extra space is not a compile error and turns the rig's whole
/// reading into `null` — which reads as "nothing was looping".
/// `the_rig_reads_the_loop_line_the_app_actually_writes` is what stops that,
/// and it can only exist because this is a value rather than an argument to
/// `log::info!`.
///
/// **`skips` rides the TAIL, past every column `loop_state_re` reads.** That
/// regex is unanchored and ends at `shared`, so appending here cannot shift a
/// group it captures and cannot turn an existing reading into `null`. The
/// per-pane attribution is variable-arity and so could never be a `(\d+)`
/// column; the scalar the rig reads comes **after** it, under its own
/// `loop_skipped_re` probe, so a run with four panes skipping and a run with
/// none put that figure behind the same label.
pub(crate) fn loop_state_line(s: &LoopState, skips: &SkippedTicks) -> String {
    // `none` rather than an empty run of pairs, so the sentence never has two
    // separators with nothing between them and the healthy case is a positive
    // statement rather than an absence a reader has to notice.
    let attributed: String = {
        let mut panes = skips.attributed().peekable();
        if panes.peek().is_none() {
            "none".to_string()
        } else {
            panes
                .map(|(idx, n)| format!("{idx}={n}"))
                .collect::<Vec<_>>()
                .join(" ")
        }
    };
    format!(
        "loop state: {} panes, {} layers animating, {} frames listed, \
         {} resident, {} in flight, {} failed; allowed plan={} section={} \
         volume={} overlay={}, cap {}, held {}; share {} B, pool {} B, \
         floor {} B, ceiling {} B; advance {} us; shared {}; \
         skipped by pane {attributed}; ticks skipped {}",
        s.panes,
        s.layers,
        s.listed,
        s.resident,
        s.in_flight,
        s.failed,
        s.allowed_plan,
        s.allowed_section,
        s.allowed_volume,
        s.allowed_overlay,
        s.cap,
        s.held,
        s.share_bytes,
        s.pool_bytes,
        s.floor_bytes,
        s.ceiling_bytes,
        s.advance_us,
        s.shared,
        skips.total(),
    )
}

#[cfg(test)]
#[path = "loop_telemetry/tests.rs"]
mod tests;
