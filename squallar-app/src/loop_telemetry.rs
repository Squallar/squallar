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
pub(crate) fn loop_state_line(s: &LoopState) -> String {
    format!(
        "loop state: {} panes, {} layers animating, {} frames listed, \
         {} resident, {} in flight, {} failed; allowed plan={} section={} \
         volume={} overlay={}, cap {}, held {}; share {} B, pool {} B, \
         floor {} B, ceiling {} B; advance {} us; shared {}",
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
    )
}

#[cfg(test)]
#[path = "loop_telemetry/tests.rs"]
mod tests;
