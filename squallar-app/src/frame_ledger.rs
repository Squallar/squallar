//! What each frame cost this thread, recorded where the frame happens.
//!
//! `handle_redraw` and the two frame calls it makes stamp a handful of
//! instants per frame; `finalize` folds them into fixed-shape histograms
//! ([`squallar_device_profile::hist::Hist`]) once the frame's outcome is
//! known. **Product telemetry, not a campaign instrument**: always on, no
//! feature gate, and the per-frame cost is nineteen clock reads and about
//! twenty-seven integer bin searches — the ledger's own eight stamps, the one
//! the egui pass takes on entry, the five `Gui::ui` takes on its way through
//! and the five `handle_redraw` takes across its tail. Six of the bin searches
//! are the `prepare` split ([`PrepareHists`]), six more the `ui` split
//! ([`UiHists`]) and six more the `post` split ([`PostHists`]); each records
//! only on the frames its own segment does. **No figure recorded here ever
//! gates CI.**
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
    /// Where `Gui::ui` crossed its own phase boundaries, off the stamps it
    /// returned. `None` on a frame that never called it, which is the same
    /// frame that leaves no `ui_start`.
    ui_phases: Option<squallar_egui::shell_api::UiPhaseStamps>,
    /// `present_frame` return.
    present_return: Option<Instant>,
    /// Where `handle_redraw`'s tail crossed its own phase boundaries, off the
    /// stamps it took on its way through. `None` on a frame that returned
    /// before the tail, which is the same frame that leaves no
    /// `present_return`.
    post_phases: Option<PostPhaseStamps>,
    /// The pass ended without a real present (a skipped or lost surface).
    skipped: bool,
}

/// Where `handle_redraw`'s tail crossed the five boundaries between the six
/// things it does after the present. Taken by `handle_redraw` itself — unlike
/// the `prepare` and `ui` splits, whose stamps ride back on a call's return
/// value, this segment has no callee to carry them.
#[derive(Clone, Copy)]
pub(crate) struct PostPhaseStamps {
    /// The action loop's end, inside `process_gui_actions` and before the
    /// overlay dispatch it tails into. Carried back by that call rather than
    /// taken here, because it is the one boundary in this tail that is not
    /// visible from `handle_redraw`.
    pub(crate) handled: Instant,
    /// `process_gui_actions` return.
    pub(crate) actions: Instant,
    /// `push_back_claim` return.
    pub(crate) back: Instant,
    /// The wake condition and any redraw ask it made.
    pub(crate) wake: Instant,
    /// `auto_poll_delay` and the `auto_poll_at` it sets.
    pub(crate) poll: Instant,
    /// The `repaint_action` match and any redraw ask its arms made.
    pub(crate) repaint: Instant,
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

/// Where the `ui` segment's time went, cut at the seams `Gui::ui` has.
///
/// # Denominator
///
/// **Exactly [`SegmentHists::ui`]'s** — presented interact frames — and that
/// equality is the whole design. The six are contiguous cuts of the one span,
/// so they telescope to it (`the_ui_phases_telescope_to_ui`), which makes the
/// residual arithmetic rather than inference: any `ui` time these six do not
/// name is a bug in this decomposition, not a mystery.
///
/// **Never added to `frame segment (ui)`.** These are not a seventh segment
/// beside it; they *are* it, opened up. The reporting prefix is deliberately
/// `frame ui` rather than `frame segment` so a reader cannot make that
/// mistake by pattern-matching a line.
///
/// A sibling of [`PrepareHists`] and independent of it: that one decomposes
/// `prepare`, this one `ui`. The two share a ledger and nothing else, and
/// neither is ever added to the other.
#[derive(Default)]
pub(crate) struct UiHists {
    /// `Gui::ui` entry to the polls' end: the site-table republish, the
    /// auto-poll check and the offline-download settle. Emits most of the
    /// frame's fetch actions and draws nothing.
    pub(crate) poll: Hist,
    /// `LayoutCtx::resolve`, the pane-grid reflow, the site-query expiry, the
    /// fade invariants and the root `Ui`'s construction — the frame's
    /// geometry, settled before a widget is placed.
    pub(crate) layout: Hist,
    /// `render_shell`: topbar, layer stack, drawer. **The eye click the
    /// UiSweep scene drives is read here**, and acted on in `panes`.
    pub(crate) shell: Hist,
    /// The time dialog and `render_panes` — every map surface, and on a
    /// toggle frame the pane that acts on the click `shell` just read.
    pub(crate) panes: Hist,
    /// The four pending appliers (pane view, section line, region, section
    /// edit) and the fade toggle: state the surfaces above deferred out of
    /// their own borrows.
    pub(crate) apply: Hist,
    /// Everything after the appliers: pills, the phone bottom bar, the
    /// timeline, the error toast, the sheet, the download area, the overlay
    /// popup, the catalog, the diagnostics panel and the deferred pane close.
    /// One cut rather than ten because it was cheap on every scene measured;
    /// the day it is not, it splits.
    pub(crate) chrome: Hist,
}

/// Where the `post` segment's time went, cut at the seams `handle_redraw`'s
/// tail has.
///
/// # Denominator
///
/// **Exactly [`SegmentHists::post`]'s** — presented interact frames — and that
/// equality is the whole design. The six are contiguous cuts of the one span,
/// so they telescope to it (`the_post_phases_telescope_to_post`), which makes
/// the residual arithmetic rather than inference: any `post` time these six do
/// not name is a bug in this decomposition, not a mystery.
///
/// **Never added to `frame segment (post)`.** These are not a seventh segment
/// beside it; they *are* it, opened up. The reporting prefix is `frame post`
/// rather than `frame segment` for the same reason [`UiHists`]' is `frame ui`.
///
/// A sibling of [`PrepareHists`] and [`UiHists`] and independent of both.
///
/// # What this split was cut to answer
///
/// `post` is not a per-frame cost: on the scene A Safari leg of 2026-09-01 it
/// read under the 62.5 µs histogram floor on 79% of interact frames and 8 ms
/// at p99 over the same 475 frames, with a windowed mean of 341 µs. A
/// distribution that shape is one occasional event, not a segment that grew,
/// and a percentile cannot say which of the six things below the event is.
#[derive(Default)]
pub(crate) struct PostHists {
    /// The `GuiAction` loop of `process_gui_actions`: every action the frame
    /// emitted, handled one at a time through `App::handle_gui_action` —
    /// which is the fetch layer, and reaches the network. `RenderOverlay` is
    /// not among them; it is intercepted into the list `dispatch` then acts
    /// on.
    pub(crate) handle: Hist,
    /// `dispatch_overlay_renders`: the dedupe, the grouping and one
    /// `spawn_overlay_render` per surviving request — the call that offloads
    /// a whole-picture raster to the worker pool.
    pub(crate) dispatch: Hist,
    /// `push_back_claim`: one `back_would_dismiss` read, and on the frames
    /// where the answer moved, one platform call.
    pub(crate) back: Hist,
    /// The wake condition — eight in-flight questions asked of the render
    /// state, the GUI, the chunk feeds, the deferred drops and the gesture
    /// player — and the redraw ask it makes when any of them says yes.
    pub(crate) wake: Hist,
    /// `auto_poll_delay` and the instant it schedules into `auto_poll_at`:
    /// four delay reads, a minimum and one clock read.
    pub(crate) poll: Hist,
    /// The `repaint_action` match on egui's own repaint delay, and the redraw
    /// ask its `Now` arm makes.
    pub(crate) repaint: Hist,
    /// The frame's close: the renderer's `frame_had_interaction` read, which
    /// is what decides whether this frame is a sample at all, and the return
    /// into `finalize`. Structurally tiny, and named rather than folded into
    /// `repaint` so that a `post` residual cannot hide in an unnamed tail.
    pub(crate) close: Hist,
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
    /// The `Gui::ui` call itself: layout and the paint list. Opened up by
    /// [`UiHists`], whose six cuts telescope to exactly this.
    pub(crate) ui: Hist,
    /// `Gui::ui` return to the acquire: mirror planning, tessellation, the
    /// texture-delta uploads and egui's buffer staging. Opened up by
    /// [`PrepareHists`], whose six cuts telescope to exactly this.
    pub(crate) prepare: Hist,
    /// Acquire return to `present_frame` return: draw, submit, present.
    pub(crate) finish: Hist,
    /// `present_frame` return to `finalize`: action processing and the
    /// repaint scheduling tail of `handle_redraw`. Opened up by
    /// [`PostHists`], whose six cuts telescope to exactly this.
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
    /// See [`UiHists`] — `segments.ui`, opened up, same frames.
    ui: UiHists,
    /// See [`PostHists`] — `segments.post`, opened up, same frames.
    post: PostHists,
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

/// The six contiguous cuts of the `ui` segment, in call order:
/// `[poll, layout, shell, panes, apply, chrome]` — see [`UiHists`], whose
/// fields these are.
///
/// Contiguous by construction: each cut ends where the next begins, and the
/// pair at the ends are `ui`'s own boundaries, so the six sum to
/// `micros(ui_start, ui_end)` exactly. A free function so the telescoping is
/// testable without a frame.
fn ui_phase_micros(
    ui_start: Instant,
    phases: &squallar_egui::shell_api::UiPhaseStamps,
    ui_end: Instant,
) -> [u32; 6] {
    [
        micros(ui_start, phases.polled),
        micros(phases.polled, phases.laid_out),
        micros(phases.laid_out, phases.shell),
        micros(phases.shell, phases.panes),
        micros(phases.panes, phases.applied),
        micros(phases.applied, ui_end),
    ]
}

/// The seven contiguous cuts of the `post` segment, in call order:
/// `[handle, dispatch, back, wake, poll, repaint, close]` — see
/// [`PostHists`], whose fields these are.
///
/// Seven where the other two splits have six, because six was not enough: the
/// first cut of the six-way spelling held 95.6% of `post` on the Safari
/// scene A leg of 2026-09-01 (131 µs of 137 µs, n=3173 settled interact
/// frames), which located the cost in `process_gui_actions` and stopped
/// there. The seam inside that call is where the answer is.
///
/// Contiguous by construction: each cut ends where the next begins, and the
/// pair at the ends are `post`'s own boundaries, so the seven sum to
/// `micros(present_return, closed)` exactly. A free function so the
/// telescoping is testable without a frame.
fn post_phase_micros(
    present_return: Instant,
    phases: &PostPhaseStamps,
    closed: Instant,
) -> [u32; 7] {
    [
        micros(present_return, phases.handled),
        micros(phases.handled, phases.actions),
        micros(phases.actions, phases.back),
        micros(phases.back, phases.wake),
        micros(phases.wake, phases.poll),
        micros(phases.poll, phases.repaint),
        micros(phases.repaint, closed),
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

    /// The phase stamps `Gui::ui` took on its way through, carried off the
    /// call that returned them. Recorded unconditionally; `finalize` decides
    /// whether this frame is a sample.
    pub(crate) fn record_ui_phases(&mut self, stamps: squallar_egui::shell_api::UiPhaseStamps) {
        self.cur.ui_phases = Some(stamps);
    }

    /// The phase stamps `handle_redraw`'s tail took on its way through.
    /// Recorded unconditionally; `finalize` decides whether this frame is a
    /// sample.
    pub(crate) fn record_post_phases(&mut self, stamps: PostPhaseStamps) {
        self.cur.post_phases = Some(stamps);
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
            // The same, for the `ui` segment recorded above. Independent of
            // the prepare block: a different segment, a different set of
            // stamps, the same denominator rule.
            if let Some(phases) = m.ui_phases.as_ref() {
                let [poll, layout, shell, panes, apply, chrome] =
                    ui_phase_micros(ui_start, phases, ui_end);
                self.ui.poll.record(poll);
                self.ui.layout.record(layout);
                self.ui.shell.record(shell);
                self.ui.panes.record(panes);
                self.ui.apply.record(apply);
                self.ui.chrome.record(chrome);
            }
            // And the same for `post`, whose right-hand boundary is `now` —
            // the very instant this function opened with, so the sixth cut
            // closes on the same stamp the segment above did.
            if let Some(phases) = m.post_phases.as_ref() {
                let [handle, dispatch, back, wake, poll, repaint, close] =
                    post_phase_micros(present_return, phases, now);
                self.post.handle.record(handle);
                self.post.dispatch.record(dispatch);
                self.post.back.record(back);
                self.post.wake.record(wake);
                self.post.poll.record(poll);
                self.post.repaint.record(repaint);
                self.post.close.record(close);
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

    pub(crate) fn ui_phases(&self) -> &UiHists {
        &self.ui
    }

    pub(crate) fn post_phases(&self) -> &PostHists {
        &self.post
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
    use super::{
        Instant, PostPhaseStamps, micros, post_phase_micros, prepare_phase_micros, service_micros,
        ui_phase_micros,
    };
    use squallar_egui::shell_api::UiPhaseStamps;
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

    /// A `Gui::ui` whose phases land at the given microsecond offsets from
    /// `ui_start`, so a test can state its stamps as arithmetic. Named apart
    /// from the `prepare` split's `phases_at`: the two decompositions share
    /// this module and answer with different stamp types.
    fn ui_phases_at(ui_start: Instant, offsets: [u64; 5]) -> UiPhaseStamps {
        let at = |us: u64| ui_start + std::time::Duration::from_micros(us);
        UiPhaseStamps {
            polled: at(offsets[0]),
            laid_out: at(offsets[1]),
            shell: at(offsets[2]),
            panes: at(offsets[3]),
            applied: at(offsets[4]),
        }
    }

    /// **The six cuts are a decomposition of `ui`, not a sample of it.**
    ///
    /// The sum telescopes to `micros(ui_start, ui_end)` — the very span
    /// [`super::SegmentHists::ui`] records — so "what is in ui" is answered by
    /// subtraction rather than by inference, and any phase the split fails to
    /// name shows up as a gap instead of hiding inside a neighbour.
    #[test]
    fn the_ui_phases_telescope_to_ui() {
        let ui_start = Instant::now();
        let phases = ui_phases_at(ui_start, [300, 1_900, 24_100, 39_400, 39_450]);
        let ui_end = ui_start + std::time::Duration::from_micros(41_000);

        let cuts = ui_phase_micros(ui_start, &phases, ui_end);
        assert_eq!(
            cuts,
            [300, 1_600, 22_200, 15_300, 50, 1_550],
            "a cut moved: the six no longer bracket the phases they are named \
             for",
        );
        assert_eq!(
            cuts.iter().sum::<u32>(),
            micros(ui_start, ui_end),
            "the six cuts do not sum to the ui span they decompose, so the \
             residual this instrument reports is not a residual of ui",
        );
        assert_eq!(cuts.iter().sum::<u32>(), 41_000);
    }

    /// **The non-vacuity floor: a cut may not trivially cover `ui`.**
    ///
    /// Telescoping alone is satisfied by a degenerate split — one cut holding
    /// the whole span and five zeros telescopes perfectly and decomposes
    /// nothing. That is the shape a "split" written against a wrong guess
    /// takes, and it is exactly what this campaign has twice shipped. So the
    /// floor is stated on the boundaries rather than on the sums: **every one
    /// of the five stamps must be able to move the answer**, which is only
    /// true if each is read by two different cuts.
    ///
    /// Held by perturbation: nudge one stamp and exactly two cuts change, by
    /// equal and opposite amounts. A split that folded a boundary away — the
    /// degenerate case — would move one cut or none.
    #[test]
    fn every_ui_stamp_is_load_bearing_in_two_cuts() {
        let ui_start = Instant::now();
        // Its own fixture, not the telescoping test's: every cut here is
        // wider than the 100 us nudge, so a stamp that fails to move a cut
        // fails this test rather than underflowing it.
        let base_offsets = [500u64, 2_500, 25_000, 38_000, 39_000];
        let ui_end = ui_start + std::time::Duration::from_micros(41_000);
        let base = ui_phase_micros(ui_start, &ui_phases_at(ui_start, base_offsets), ui_end);

        for stamp in 0..5 {
            let mut moved = base_offsets;
            moved[stamp] -= 100;
            let cuts = ui_phase_micros(ui_start, &ui_phases_at(ui_start, moved), ui_end);
            let changed: Vec<usize> = (0..6).filter(|&i| cuts[i] != base[i]).collect();
            assert_eq!(
                changed,
                vec![stamp, stamp + 1],
                "moving stamp {stamp} did not move exactly the two cuts it \
                 bounds, so one of them is not reading it and the split is \
                 narrower than its six names claim",
            );
            assert_eq!(
                (cuts[stamp], cuts[stamp + 1]),
                (base[stamp] - 100, base[stamp + 1] + 100),
                "the two cuts around stamp {stamp} did not trade the 100 us \
                 exactly, so the boundary between them is not the stamp",
            );
            assert_eq!(cuts.iter().sum::<u32>(), 41_000);
        }
    }

    /// **The floor's other half: no cut may be structurally empty.**
    ///
    /// [`every_ui_stamp_is_load_bearing_in_two_cuts`] holds that the
    /// boundaries are real; this holds that the *regions* are. A split whose
    /// six names covered `ui` but where five were pinned at zero would pass
    /// the telescoping test and report a single opaque number under six
    /// headings — which is the instrument this replaces, renamed.
    #[test]
    fn no_ui_cut_is_structurally_pinned_to_zero() {
        let ui_start = Instant::now();
        let phases = ui_phases_at(ui_start, [300, 1_900, 24_100, 39_400, 39_450]);
        let ui_end = ui_start + std::time::Duration::from_micros(41_000);
        let cuts = ui_phase_micros(ui_start, &phases, ui_end);
        assert!(
            cuts.iter().all(|&c| c > 0),
            "a cut is zero on stamps chosen to make all six non-zero, so it \
             cannot be reading the span it is named for: {cuts:?}",
        );
    }

    /// A `handle_redraw` tail whose boundaries land at the given microsecond
    /// offsets from `present_return`, so a test can state its stamps as
    /// arithmetic. Named apart from the other two splits' fixtures: the three
    /// decompositions share this module and answer with different stamp types.
    fn post_phases_at(present_return: Instant, offsets: [u64; 6]) -> PostPhaseStamps {
        let at = |us: u64| present_return + std::time::Duration::from_micros(us);
        PostPhaseStamps {
            handled: at(offsets[0]),
            actions: at(offsets[1]),
            back: at(offsets[2]),
            wake: at(offsets[3]),
            poll: at(offsets[4]),
            repaint: at(offsets[5]),
        }
    }

    /// **The seven cuts are a decomposition of `post`, not a sample of it.**
    ///
    /// The sum telescopes to `micros(present_return, closed)` — the very span
    /// [`super::SegmentHists::post`] records — so "what is in post" is
    /// answered by subtraction rather than by inference, and any phase the
    /// split fails to name shows up as a gap instead of hiding inside a
    /// neighbour.
    #[test]
    fn the_post_phases_telescope_to_post() {
        let present_return = Instant::now();
        let phases = post_phases_at(present_return, [7_900, 8_200, 8_260, 8_400, 8_450, 8_600]);
        let closed = present_return + std::time::Duration::from_micros(8_640);

        let cuts = post_phase_micros(present_return, &phases, closed);
        assert_eq!(
            cuts,
            [7_900, 300, 60, 140, 50, 150, 40],
            "a cut moved: the seven no longer bracket the boundaries they are \
             named for",
        );
        assert_eq!(
            cuts.iter().sum::<u32>(),
            micros(present_return, closed),
            "the seven cuts do not sum to the post span they decompose, so \
             the residual this instrument reports is not a residual of post",
        );
        assert_eq!(cuts.iter().sum::<u32>(), 8_640);
    }

    /// **The non-vacuity floor: a cut may not trivially cover `post`.**
    ///
    /// The sibling of `every_ui_stamp_is_load_bearing_in_two_cuts`, and the
    /// hazard is sharper here: `post` reads under the histogram floor on
    /// nineteen frames in twenty, so six of the seven cuts really are
    /// near-zero on a real leg. A split whose stamps had been folded together would look exactly
    /// like that reading and pass a telescoping test. Held by perturbation
    /// instead: nudge one stamp and exactly two cuts change, by equal and
    /// opposite amounts.
    #[test]
    fn every_post_stamp_is_load_bearing_in_two_cuts() {
        let present_return = Instant::now();
        // Its own fixture: every cut is wider than the 100 us nudge, so a
        // stamp that fails to move a cut fails this test rather than
        // underflowing it.
        let base_offsets = [4_500u64, 5_000, 5_500, 6_000, 6_500, 7_000];
        let closed = present_return + std::time::Duration::from_micros(7_500);
        let base = post_phase_micros(
            present_return,
            &post_phases_at(present_return, base_offsets),
            closed,
        );

        for moved in 0..6 {
            let mut offsets = base_offsets;
            offsets[moved] += 100;
            let cuts = post_phase_micros(
                present_return,
                &post_phases_at(present_return, offsets),
                closed,
            );
            let changed: Vec<usize> = (0..7).filter(|&i| cuts[i] != base[i]).collect();
            assert_eq!(
                changed,
                vec![moved, moved + 1],
                "moving stamp {moved} did not move exactly the two cuts it \
                 bounds, so a boundary is folded away and the split reports \
                 fewer spans than it names",
            );
            assert_eq!(
                cuts[moved] - base[moved],
                base[moved + 1] - cuts[moved + 1],
                "the two cuts stamp {moved} bounds did not trade the same 100 \
                 us, so they are not contiguous across it",
            );
        }
    }

    /// **`post`'s right-hand boundary is `finalize`'s own `now`.**
    ///
    /// The sixth cut and [`super::SegmentHists::post`] must close on the same
    /// instant, or `frame post (*)` sums to something that is not
    /// `frame segment (post)` and the two lines may not be read together.
    /// Stated as a test because the two are computed in different places —
    /// `micros(present_return, now)` for the segment, `post_phase_micros` for
    /// the split — and nothing else holds them to the same right edge.
    #[test]
    fn the_post_split_closes_on_the_segments_own_right_edge() {
        let present_return = Instant::now();
        let phases = post_phases_at(present_return, [20, 40, 90, 300, 340, 900]);
        let now = present_return + std::time::Duration::from_micros(1_000);

        let segment = micros(present_return, now);
        let cuts = post_phase_micros(present_return, &phases, now);
        assert_eq!(
            cuts.iter().sum::<u32>(),
            segment,
            "the split and the segment do not close on the same instant, so \
             the seven cuts are not this segment's decomposition",
        );
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
