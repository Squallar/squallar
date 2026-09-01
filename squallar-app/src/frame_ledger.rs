//! What each frame cost this thread, recorded where the frame happens.
//!
//! `handle_redraw` and the two frame calls it makes stamp a handful of
//! instants per frame; `finalize` folds them into fixed-shape histograms
//! ([`squallar_device_profile::hist::Hist`]) once the frame's outcome is
//! known. **Product telemetry, not a campaign instrument**: always on, no
//! feature gate, and the per-frame cost is nine clock reads and about fifteen
//! integer bin searches — the ledger's own eight stamps plus the one the egui
//! pass takes on entry, and six of the bin searches are the `prepare` split
//! ([`PrepareHists`]), which records only on the frames `prepare` itself does.
//! **No figure recorded here ever gates CI.**
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
    /// Where the egui pass crossed its own phase boundaries, off the
    /// `PreparedFrame` the pass returned. `None` on a frame that never
    /// finished a pass, which is the same frame that leaves no acquire.
    prepare_phases: Option<squallar_gpu::egui_renderer::pass_costs::PassPhaseStamps>,
    /// `present_frame` return.
    present_return: Option<Instant>,
    /// The pass ended without a real present (a skipped or lost surface).
    skipped: bool,
}

/// Where the `prepare` segment's time went, cut at the seams the code has.
///
/// # Denominator
///
/// **Exactly [`SegmentHists::prepare`]'s** — presented interact frames — and
/// that equality is the whole design. The six are contiguous cuts of the one
/// span, so they telescope to it (`the_prepare_phases_telescope_to_prepare`),
/// which makes the residual arithmetic rather than inference: any prepare time
/// these six do not name is a bug in this decomposition, not a mystery.
///
/// **Never added to [`squallar_gpu::egui_renderer::pass_costs::PassCosts`]**,
/// which counts every pass ended — idle frames and non-presenting frames
/// included — and therefore has more samples in it than this has frames.
/// The two measure overlapping work over different frame sets; only this one
/// shares a denominator with `prepare`.
#[derive(Default)]
pub(crate) struct PrepareHists {
    /// `Gui::ui` return to the egui pass's close: the app's own prologue —
    /// the command encoder, the mirror source rects, the floor demand, the
    /// mirror rung plan and any mirror-texture realloc.
    pub(crate) plan: Hist,
    /// `Context::end_pass` and the platform-output handoff. Was invisible to
    /// every figure before this split: the renderer's first clock read used to
    /// be taken after it.
    pub(crate) end_pass: Hist,
    /// `Context::tessellate` — shapes to triangles, on this thread.
    pub(crate) tessellate: Hist,
    /// Filing and draining this frame's texture deltas: the memcpys into
    /// staging slots and any blocking `write_texture`.
    pub(crate) upload: Hist,
    /// The pane-mirror pass, and on a frame with no mirror request the
    /// sub-microsecond cost of finding that out.
    pub(crate) mirror: Hist,
    /// egui's `update_buffers` — which also dispatches every paint callback's
    /// `prepare`, the 3D raymarch's CPU-side encode included — plus the return
    /// to the swapchain acquire, which is a handful of instructions and is
    /// folded in here rather than given a seventh name it could not fill.
    pub(crate) buffers: Hist,
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
    /// texture-delta uploads and egui's buffer staging. Opened up by
    /// [`PrepareHists`], whose six cuts telescope to exactly this.
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
    /// See [`PrepareHists`] — `segments.prepare`, opened up, same frames.
    prepare: PrepareHists,
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

/// The six contiguous cuts of the `prepare` segment, in pass order:
/// `[plan, end_pass, tessellate, upload, mirror, buffers]` — see
/// [`PrepareHists`], whose fields these are.
///
/// Contiguous by construction: each cut ends where the next begins, and the
/// pair at the ends are `prepare`'s own boundaries, so the six sum to
/// `micros(ui_end, acquire_start)` exactly. A free function so the telescoping
/// is testable without a frame.
fn prepare_phase_micros(
    ui_end: Instant,
    phases: &squallar_gpu::egui_renderer::pass_costs::PassPhaseStamps,
    acquire_start: Instant,
) -> [u32; 6] {
    [
        micros(ui_end, phases.entry),
        micros(phases.entry, phases.tessellate),
        micros(phases.tessellate, phases.upload),
        micros(phases.upload, phases.upload_done),
        micros(phases.upload_done, phases.buffers),
        micros(phases.buffers, acquire_start),
    ]
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

    /// The phase stamps the egui pass took on its way through, carried off the
    /// `PreparedFrame` it returned. Recorded unconditionally; `finalize`
    /// decides whether this frame is a sample.
    pub(crate) fn record_prepare_phases(
        &mut self,
        stamps: squallar_gpu::egui_renderer::pass_costs::PassPhaseStamps,
    ) {
        self.cur.prepare_phases = Some(stamps);
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
            // Inside the interact arm, and only here: these six are cuts of
            // the `prepare` recorded two lines up, and a sample recorded on a
            // frame that segment did not take would break the one property
            // that makes the split arithmetic.
            if let Some(phases) = m.prepare_phases.as_ref() {
                let [plan, end_pass, tessellate, upload, mirror, buffers] =
                    prepare_phase_micros(ui_end, phases, acquire_start);
                self.prepare.plan.record(plan);
                self.prepare.end_pass.record(end_pass);
                self.prepare.tessellate.record(tessellate);
                self.prepare.upload.record(upload);
                self.prepare.mirror.record(mirror);
                self.prepare.buffers.record(buffers);
            }
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

    pub(crate) fn prepare_phases(&self) -> &PrepareHists {
        &self.prepare
    }

    pub(crate) fn acquire(&self) -> &Hist {
        &self.acquire
    }

    pub(crate) fn cadence(&self) -> &Hist {
        &self.cadence
    }

    /// Every histogram this ledger keeps, borrowed as the diagnostics
    /// overlay's frame input. `gpu_passes` is `None` here — the GPU pass
    /// line is not this ledger's to compose, and `push_frame_inputs` overlays
    /// it from the probe's report where one is installed.
    pub(crate) fn diagnostics(&self) -> squallar_egui::shell_api::FrameDiagnostics<'_> {
        squallar_egui::shell_api::FrameDiagnostics {
            service_interact: &self.service_interact,
            service_idle: &self.service_idle,
            segments: [
                &self.segments.pre,
                &self.segments.pump,
                &self.segments.ui,
                &self.segments.prepare,
                &self.segments.finish,
                &self.segments.post,
            ],
            acquire: &self.acquire,
            cadence: &self.cadence,
            gpu_passes: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Instant, micros, prepare_phase_micros, service_micros};
    use squallar_gpu::egui_renderer::pass_costs::PassPhaseStamps;

    /// A pass whose phases land at the given microsecond offsets from
    /// `ui_end`, so a test can state its stamps as arithmetic.
    fn phases_at(ui_end: Instant, offsets: [u64; 5]) -> PassPhaseStamps {
        let at = |us: u64| ui_end + std::time::Duration::from_micros(us);
        PassPhaseStamps {
            entry: at(offsets[0]),
            tessellate: at(offsets[1]),
            upload: at(offsets[2]),
            upload_done: at(offsets[3]),
            buffers: at(offsets[4]),
        }
    }

    /// **The six cuts are a decomposition of `prepare`, not a sample of it.**
    ///
    /// The sum telescopes to `micros(ui_end, acquire_start)` — the very span
    /// `SegmentHists::prepare` records — so "what is in prepare" is answered by
    /// subtraction rather than by inference, and any phase the split fails to
    /// name shows up as a gap instead of hiding inside a neighbour.
    #[test]
    fn the_prepare_phases_telescope_to_prepare() {
        let ui_end = Instant::now();
        let phases = phases_at(ui_end, [400, 1_500, 9_100, 12_000, 12_050]);
        let acquire_start = ui_end + std::time::Duration::from_micros(18_300);

        let cuts = prepare_phase_micros(ui_end, &phases, acquire_start);
        assert_eq!(
            cuts,
            [400, 1_100, 7_600, 2_900, 50, 6_250],
            "a cut moved: the six no longer bracket the phases they are named \
             for",
        );
        assert_eq!(
            cuts.iter().sum::<u32>(),
            micros(ui_end, acquire_start),
            "the six cuts do not sum to the prepare span they decompose, so \
             the residual this instrument reports is not a residual of prepare",
        );
        assert_eq!(cuts.iter().sum::<u32>(), 18_300);
    }

    /// **The non-vacuity floor under the two cuts this split adds.**
    ///
    /// The renderer's own `PassCosts` ledger already timed four phases, and
    /// its first clock read was taken at tessellation — so the app's prologue
    /// (`plan`) and `Context::end_pass` itself were outside every figure the
    /// instrument had. This holds that the four old phases really do leave a
    /// hole, and that the hole is exactly the two new cuts: without it, a
    /// split that renamed the existing four and measured nothing more would
    /// pass every other test here.
    #[test]
    fn the_phases_the_pass_ledger_already_timed_do_not_cover_prepare() {
        let ui_end = Instant::now();
        let phases = phases_at(ui_end, [400, 1_500, 9_100, 12_000, 12_050]);
        let acquire_start = ui_end + std::time::Duration::from_micros(18_300);

        let cuts = prepare_phase_micros(ui_end, &phases, acquire_start);
        let already_timed: u32 = cuts[2..].iter().sum();
        let newly_named: u32 = cuts[..2].iter().sum();
        assert_ne!(
            newly_named, 0,
            "the two cuts this split adds are empty, so it opened nothing up",
        );
        assert_eq!(
            already_timed + newly_named,
            micros(ui_end, acquire_start),
            "the old four plus the new two are not prepare, so one of the two \
             groups is measuring something else",
        );
        assert_eq!((already_timed, newly_named), (16_800, 1_500));
    }

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
