//! What each frame cost this thread, recorded where the frame happens.
//!
//! `handle_redraw` and the two frame calls it makes stamp a handful of
//! instants per frame; `finalize` folds them into fixed-shape histograms
//! ([`squallar_device_profile::hist::Hist`]) once the frame's outcome is
//! known. **Product telemetry, not a campaign instrument**: always on, no
//! feature gate, and the per-frame cost is eight clock reads and a dozen
//! integer bin searches. **No figure recorded here ever gates CI.**
//!
//! # The three denominators, stated once
//!
//! * **Service** is the frame thread's own work: the whole `handle_redraw`
//!   span **minus the swapchain acquire** (the vsync block — the display's
//!   time, not ours). Only a frame that presented is a service sample; a
//!   skipped or lost surface leaves no acquire to subtract.
//! * **Interact vs idle** splits every service sample by whether the frame's
//!   egui raw input carried at least one pointer/touch/wheel/zoom event
//!   (`EguiRenderer::frame_had_interaction`). Interact frames are the ones
//!   the responsiveness bar is about; idle frames are the floor under them.
//! * **Cadence** is redraw-to-redraw: the interval between consecutive
//!   *presented* frames' starts. It is the co-criterion for a GPU-bound leg
//!   — innocent CPU service with a limping cadence is still a limp — and it
//!   is **never added to service**; the two share no denominator.

use squallar_device_profile::hist::Hist;
use web_time::Instant;

/// Whether service is assembled as one whole-frame pair minus the acquire
/// interval, rather than as the sum of the six non-acquire segments. The two
/// spellings agree exactly on contiguous stamps (the sum telescopes —
/// `the_two_service_spellings_agree` holds that), but browser clocks are
/// coarsened (iOS Safari to 1 ms), and a figure assembled from the fewest
/// stamps is the one whose quantization error is bounded by the clock's own
/// grain. A selected value, so `finalize` has no target fork in its body.
#[cfg(target_arch = "wasm32")]
const SERVICE_FROM_WHOLE_FRAME: bool = true;
#[cfg(not(target_arch = "wasm32"))]
const SERVICE_FROM_WHOLE_FRAME: bool = false;

/// One frame's stamps, taken as the frame passes its boundaries. Reset by
/// [`FrameLedger::mark_frame_start`]; a frame that early-returns simply
/// leaves holes, and `finalize` records nothing from holes.
#[derive(Default)]
struct Marks {
    /// `handle_redraw` entry.
    start: Option<Instant>,
    /// `setup_egui_frame` entry.
    setup: Option<Instant>,
    /// Immediately before `Gui::ui`.
    ui_start: Option<Instant>,
    /// Immediately after `Gui::ui`.
    ui_end: Option<Instant>,
    /// The `get_current_texture` span, stamped inside the acquire closure.
    acquire: Option<(Instant, Instant)>,
    /// `present_frame` return.
    present_return: Option<Instant>,
    /// The pass ended without a real present (a skipped or lost surface).
    skipped: bool,
}

/// The per-segment histograms of interact frames, and only interact frames:
/// the segments exist to say where an interact frame's service went, and
/// mixing idle frames in would average the answer away.
#[derive(Default)]
pub(crate) struct SegmentHists {
    /// `handle_redraw` entry to `setup_egui_frame` entry — the pollers,
    /// eviction, deferred drops and the autosave check.
    pub(crate) pre: Hist,
    /// `setup_egui_frame` entry to `Gui::ui` — theme, restore, the raster
    /// promote and the three pump phases.
    pub(crate) pump: Hist,
    /// The `Gui::ui` call itself: layout and the paint list.
    pub(crate) ui: Hist,
    /// `Gui::ui` return to the acquire: mirror planning, tessellation, the
    /// texture-delta uploads and egui's buffer staging.
    pub(crate) prepare: Hist,
    /// Acquire return to `present_frame` return: draw, submit, present.
    pub(crate) finish: Hist,
    /// `present_frame` return to `finalize`: action processing and the
    /// repaint scheduling tail of `handle_redraw`.
    pub(crate) post: Hist,
}

/// The frame recorder the `App` owns. Single-writer, on the frame thread.
#[derive(Default)]
pub(crate) struct FrameLedger {
    cur: Marks,
    /// Service of presented frames whose input carried interaction.
    service_interact: Hist,
    /// Service of presented frames whose input carried none.
    service_idle: Hist,
    /// See [`SegmentHists`] — interact frames only.
    segments: SegmentHists,
    /// The acquire span itself, interact frames only. Reported beside the
    /// segments and never inside service: it is the vsync block.
    acquire: Hist,
    /// Redraw-to-redraw of presented frames, both families.
    cadence: Hist,
    /// The last presented frame's start, cadence's left stamp.
    last_presented_start: Option<Instant>,
}

/// Whole microseconds from `a` to `b`, saturating into the histogram's `u32`.
fn micros(a: Instant, b: Instant) -> u32 {
    b.duration_since(a).as_micros().min(u128::from(u32::MAX)) as u32
}

/// The service figure, from the spelling [`SERVICE_FROM_WHOLE_FRAME`]
/// selected. `segments` is `[pre, pump, ui, prepare, finish, post]`.
fn service_micros(
    from_whole_frame: bool,
    whole_frame: u32,
    segments: [u32; 6],
    acquire: u32,
) -> u32 {
    if from_whole_frame {
        whole_frame.saturating_sub(acquire)
    } else {
        segments
            .iter()
            .fold(0u32, |sum, &segment| sum.saturating_add(segment))
    }
}

impl FrameLedger {
    /// Open a frame: stamp its start and forget the previous frame's marks.
    pub(crate) fn mark_frame_start(&mut self) {
        self.cur = Marks {
            start: Some(Instant::now()),
            ..Marks::default()
        };
    }

    pub(crate) fn mark_setup_entry(&mut self) {
        self.cur.setup = Some(Instant::now());
    }

    pub(crate) fn mark_ui_start(&mut self) {
        self.cur.ui_start = Some(Instant::now());
    }

    pub(crate) fn mark_ui_end(&mut self) {
        self.cur.ui_end = Some(Instant::now());
    }

    /// The measured `get_current_texture` span, stamped by the acquire
    /// closure itself so the boundary cannot drift from the call.
    pub(crate) fn record_acquire(&mut self, start: Instant, end: Instant) {
        self.cur.acquire = Some((start, end));
    }

    pub(crate) fn mark_present_return(&mut self) {
        self.cur.present_return = Some(Instant::now());
    }

    /// The Skip/Lost path: the pass ended without a real present, so this
    /// frame is not a service or cadence sample.
    pub(crate) fn mark_skipped(&mut self) {
        self.cur.skipped = true;
    }

    /// Close the frame's sample. Called at the end of `handle_redraw`;
    /// `interacted` is the renderer's own answer for this frame's input.
    pub(crate) fn finalize(&mut self, interacted: bool) {
        let m = std::mem::take(&mut self.cur);
        let now = Instant::now();
        let (Some(start), Some(setup), Some(ui_start), Some(ui_end)) =
            (m.start, m.setup, m.ui_start, m.ui_end)
        else {
            // The frame early-returned before the pass; not a sample.
            return;
        };
        if m.skipped {
            return;
        }
        let (Some((acquire_start, acquire_end)), Some(present_return)) =
            (m.acquire, m.present_return)
        else {
            return;
        };

        let acquire = micros(acquire_start, acquire_end);
        let segments = [
            micros(start, setup),
            micros(setup, ui_start),
            micros(ui_start, ui_end),
            micros(ui_end, acquire_start),
            micros(acquire_end, present_return),
            micros(present_return, now),
        ];
        let service = service_micros(
            SERVICE_FROM_WHOLE_FRAME,
            micros(start, now),
            segments,
            acquire,
        );

        if interacted {
            self.service_interact.record(service);
            let [pre, pump, ui, prepare, finish, post] = segments;
            self.segments.pre.record(pre);
            self.segments.pump.record(pump);
            self.segments.ui.record(ui);
            self.segments.prepare.record(prepare);
            self.segments.finish.record(finish);
            self.segments.post.record(post);
            self.acquire.record(acquire);
        } else {
            self.service_idle.record(service);
        }

        if let Some(previous) = self.last_presented_start {
            self.cadence.record(micros(previous, start));
        }
        self.last_presented_start = Some(start);
    }

    pub(crate) fn service_interact(&self) -> &Hist {
        &self.service_interact
    }

    pub(crate) fn service_idle(&self) -> &Hist {
        &self.service_idle
    }

    pub(crate) fn segments(&self) -> &SegmentHists {
        &self.segments
    }

    pub(crate) fn acquire(&self) -> &Hist {
        &self.acquire
    }

    pub(crate) fn cadence(&self) -> &Hist {
        &self.cadence
    }
}

#[cfg(test)]
mod tests {
    use super::service_micros;

    /// The two service spellings agree exactly whenever the segments are the
    /// contiguous cuts of the whole frame minus the acquire — the sum
    /// telescopes. What makes the per-target selection a reporting choice
    /// (fewest stamps under a coarse clock) rather than two different
    /// figures.
    #[test]
    fn the_two_service_spellings_agree() {
        // Contiguous stamps, micros from frame start: the boundaries of the
        // six segments plus the acquire span [ui_end→acquire, acquire 1200
        // wide], all inside a 10 000 us frame.
        let segments = [150u32, 900, 2_400, 1_800, 700, 850];
        let acquire = 3_200;
        let whole_frame: u32 = segments.iter().sum::<u32>() + acquire;
        assert_eq!(
            service_micros(false, whole_frame, segments, acquire),
            service_micros(true, whole_frame, segments, acquire),
            "the segment sum and the whole-frame pair disagree on contiguous \
             stamps; one of the two spellings is not measuring service",
        );
        assert_eq!(
            service_micros(true, whole_frame, segments, acquire),
            whole_frame - acquire,
        );
    }
}
