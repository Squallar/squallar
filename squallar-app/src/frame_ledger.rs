//! What each frame cost this thread, recorded where the frame happens.
//!
//! `handle_redraw` and the two frame calls it makes stamp a handful of
//! instants per frame; `finalize` folds them into fixed-shape histograms
//! ([`squallar_device_profile::hist::Hist`]) once the frame's outcome is
//! known. **Product telemetry, not a campaign instrument**: always on, no
//! feature gate, and the per-frame cost is **forty-seven clock reads,
//! fifty-five integer bin searches** and two `u32` comparisons.
//!
//! The clock reads, counted where they are taken: the ledger's own **eight**
//! stamps (its six `mark_*`/`finalize` reads plus the pair the acquire
//! closure hands back), the **six** `handle_redraw` takes across its head,
//! the **seven** `setup_egui_frame` takes across `pump`, the **eight**
//! `Gui::ui` takes on its way through (six in `ui_phased`, two inside
//! `render_shell_phased`), the **five** the egui pass takes, the **six**
//! `handle_redraw`'s tail takes (five in the tail, one inside
//! `process_gui_actions`) and the **seven** `present_frame` takes after the
//! acquire returns.
//!
//! The bin searches, on a presented interact frame: nine outside the splits
//! (one service, six segments, one acquire, one cadence) and forty-six in
//! them — **seven** the `pre` split ([`PreHists`]), **eight** the `pump`
//! split ([`PumpHists`]), **nine** the `ui` split ([`UiHists`]), **six** the
//! `prepare` split ([`PrepareHists`]), **seven** the `post` split
//! ([`PostHists`]) and **nine** the `finish` split ([`FinishHists`]). All but
//! the last record only on the frames their own segment does; the `finish`
//! nine record on EVERY presented frame, which is the one denominator
//! difference in this file and is the reason that split exists — see
//! [`FinishHists`]. The two comparisons are [`WorstFrame`]'s latch and its
//! session maximum, and like the `finish` nine they are offered EVERY
//! presented frame.
//!
//! **Both figures were stale before the `pre` split, and by more than the
//! split adds.** They read twenty-six and "about thirty-seven" while the `ui`
//! split had grown from six cuts to nine, the `post` split from six to seven,
//! and the eight `pump` stamps had never been counted at all. The `pre` split
//! itself is six of the forty-seven and seven of the fifty-five; the other
//! fifteen and eleven were drift. Recount here rather than adjust, or the
//! next reader inherits the same arithmetic.
//!
//! **The unnecessary-frame verdict adds no clock read and no bin search**, and
//! that is the pin: [`crate::frame_need`] rides on this call and costs one
//! relaxed `swap` of a `u32` register plus between three and six `u64`
//! increments per presented frame. The three counts above are unchanged by it.
//!
//! **A dispatching frame pays more, and only a dispatching frame.** The
//! `dispatch` split ([`DispatchHists`]) adds two clock reads per
//! `dispatch_overlay_renders` call, eight more per request that survives its
//! dedupe, and seven bin searches — on the frames whose tail dispatches an
//! overlay raster at all, which measured 19 of 176 on the scene the split was
//! cut for, and nothing on the rest. **No figure recorded here ever gates
//! CI.**
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
    /// Where `handle_redraw`'s head crossed its own phase boundaries, taken
    /// by that call on its way through. `None` on a frame that returned
    /// through one of the head's three early exits — minimized, zero-area or
    /// no renderer — which is the same frame that leaves no `setup`.
    pre_phases: Option<PrePhaseStamps>,
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
    /// Where `setup_egui_frame` crossed its own phase boundaries, taken by
    /// that call on its way through. `None` on a frame that early-returned
    /// before the pass, which is the same frame that leaves no `ui_start`.
    pump_phases: Option<PumpPhaseStamps>,
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
    /// Where `present_frame`'s tail crossed its own phase boundaries, off the
    /// stamps it took on its way through. `None` on a frame that returned
    /// through the skipped/lost arm, which is the same frame `finalize`
    /// discards outright.
    finish_phases: Option<FinishPhaseStamps>,
    /// What this frame's `dispatch` cut spent, by sub-cut. `None` on a frame
    /// whose tail never dispatched, which is most of them — and an absence
    /// here is not a zero: a frame that dispatched nothing has no dispatch to
    /// decompose, while a frame that dispatched cheaply has six small figures.
    dispatch: Option<DispatchCuts>,
    /// The pass ended without a real present (a skipped or lost surface).
    skipped: bool,
}

/// Where `handle_redraw`'s head crossed the six boundaries between the seven
/// things it does before `setup_egui_frame`. Taken by `handle_redraw` itself
/// — like [`PostPhaseStamps`] and [`PumpPhaseStamps`], and unlike the
/// `prepare` and `ui` splits, this segment has no single callee to carry them
/// back.
///
/// All six are stamped before the call that opens `pump`, so a frame that
/// takes one of the head's early exits leaves them behind unfiled: `finalize`
/// discards that frame anyway, and the stamps it did pay for are three or
/// four clock reads on a path that draws nothing.
#[derive(Clone, Copy)]
pub(crate) struct PrePhaseStamps {
    /// After `clear_frame_state` and `poll_platform_state`: the theme poll,
    /// the location permission step (which asks the platform for its KV
    /// store), the fix poll and the heading poll.
    pub(crate) polled: Instant,
    /// `poll_data_channels`' return — the `Ingest` pump walk.
    pub(crate) ingested: Instant,
    /// `evict_unshown_scans`' return.
    pub(crate) evicted: Instant,
    /// `drain_deferred_drops`' return.
    pub(crate) dropped: Instant,
    /// `autosave_config`'s return.
    pub(crate) saved: Instant,
    /// After the minimized and zero-area window queries — the two platform
    /// questions whose answers can abandon the frame.
    pub(crate) gated: Instant,
}

/// Where the `pre` segment's time went, cut at the seams `handle_redraw`'s
/// head has.
///
/// # Denominator
///
/// **Exactly [`SegmentHists::pre`]'s** — presented interact frames — and that
/// equality is the whole design. The seven are contiguous cuts of the one
/// span, so they telescope to it (`the_pre_phases_telescope_to_pre`) to
/// within [`micros`]' truncation, which makes the residual arithmetic rather
/// than inference.
///
/// **Never added to `frame segment (pre)`.** These are not a seventh segment
/// beside it; they *are* it, opened up — [`PostHists`]' own relationship to
/// `frame segment (post)`. The reporting prefix is `frame pre`.
///
/// `frame pre (drops)` and `frame post (*)` are different spans under
/// different parents and are never added; so are `frame pre (ingest)` and
/// `frame pump (apply)`, which are two different pump walks.
///
/// # What this split was cut to answer
///
/// `pre` was the last segment with no split, and a top-three tail owner
/// while it had none. Measured on scene D, hardware Vulkan on an RTX 3090,
/// n=1120 interact frames per leg over three legs: **mean 325.6 µs, p99
/// 1,682 µs, and 10,357 µs on one latched frame — 88–92 % of that whole
/// frame.** Every other segment could be opened up and this one could not,
/// so nothing in the tree could say which of the seven things below the
/// 10 ms was.
#[derive(Default)]
pub(crate) struct PreHists {
    /// `InputHandler::clear_frame_state` (two bool writes, folded in rather
    /// than given a name it could not fill) and `poll_platform_state`: the
    /// theme poll, the location step, the fix poll and the heading poll.
    /// Reaches the platform, and on a browser that is a JS crossing per ask.
    pub(crate) platform: Hist,
    /// `poll_data_channels` — the `Ingest` pump walk, where every arrival
    /// channel is drained and its payload applied. **The data path onto the
    /// frame thread**, and the cut that scales with what arrived.
    pub(crate) ingest: Hist,
    /// `evict_unshown_scans`: the pane walk, `retain_still` and the
    /// `discard_each` that queues what it evicted. Scales with pane count and
    /// with how many volumes fell out of retention.
    pub(crate) evict: Hist,
    /// `drain_deferred_drops`: the budgeted free of what eviction and
    /// supersession queued. Bounded by `DEFERRED_DROP_BUDGET_PER_FRAME` by
    /// design, so a figure meaningfully over that budget is the budget
    /// failing on its own boundary rather than a busy frame.
    pub(crate) drops: Hist,
    /// `autosave_config(false)`: one clock read and an interval compare on
    /// almost every frame, and on the one frame per period that fires, the
    /// whole UI config serialized to JSON and handed to the KV store. Its own
    /// cut because that shape — cheap on all but one frame — is exactly the
    /// shape a percentile over `pre` cannot attribute.
    pub(crate) autosave: Hist,
    /// The minimized query and the zero-area `inner_size` query: two window
    /// calls, and on the frames either answers yes the frame is abandoned
    /// rather than drawn. Named because they are platform calls on the frame
    /// thread, and a windowing system is free to make them expensive.
    pub(crate) gate: Hist,
    /// `ensure_rendering_state` and the renderer/window test after it:
    /// nothing on a steady-state frame, and the whole device and surface
    /// bring-up on the first. The tail of the head, named rather than folded
    /// so that a `pre` residual cannot hide in an unnamed remainder.
    pub(crate) ensure: Hist,
}

/// Where `setup_egui_frame` crossed the seven boundaries between the eight
/// things it does before `Gui::ui`. Taken by that call itself — like
/// [`PostPhaseStamps`] and unlike the `prepare` and `ui` splits, this segment
/// has no single callee to carry them back.
#[derive(Clone, Copy)]
pub(crate) struct PumpPhaseStamps {
    /// The context clone's return: egui's pass is open, the theme is applied
    /// and the per-pane vectors are sized.
    pub(crate) began: Instant,
    /// `restore_cached_render`'s return, or the instant the `restore_pending`
    /// test answered no.
    pub(crate) restored: Instant,
    /// `promote_uploaded_rasters`' return.
    pub(crate) promoted: Instant,
    /// `report_raster_telemetry`'s return.
    pub(crate) rastered: Instant,
    /// The `Apply` pump walk's return.
    pub(crate) applied: Instant,
    /// The `Advance` pump walk's return.
    pub(crate) advanced: Instant,
    /// The `Dispatch` pump walk's return.
    pub(crate) dispatched: Instant,
}

/// Where the `pump` segment's time went, cut at the seams `setup_egui_frame`
/// has.
///
/// # Denominator
///
/// **Exactly [`SegmentHists::pump`]'s** — presented interact frames — and
/// that equality is the whole design. The eight are contiguous cuts of the
/// one span, so they telescope to it (`the_pump_phases_telescope_to_pump`),
/// which makes the residual arithmetic rather than inference.
///
/// **Never added to `frame segment (pump)`.** These are not a seventh segment
/// beside it; they *are* it, opened up — [`PostHists`]' own relationship to
/// `frame segment (post)`. The reporting prefix is `frame pump`.
///
/// `frame pump (dispatch)` and `frame post (dispatch)` are **different spans
/// under different parents** and are never added: this one is the `Dispatch`
/// pump walk before the paint list is built, that one is
/// `dispatch_overlay_renders` after the present.
///
/// # What this split was cut to answer
///
/// Measured on the Mac 60 Hz arm, scene D, windowed worst interact frame,
/// app 995eecc0: `pump` was **11,800 µs on Firefox and 280 µs on Chromium**,
/// a factor of 42 on the same scene with only a 2.25x difference in picture
/// bytes. Nothing in the tree could say which of the eight things
/// `setup_egui_frame` does that was.
#[derive(Default)]
pub(crate) struct PumpHists {
    /// egui's `begin_frame`, the gesture player's event batch, the theme
    /// apply, `ensure_pane_count`, `release_hidden_pane_volumes` and the
    /// context clone. Scales with pane count, not with data.
    pub(crate) begin: Hist,
    /// `restore_cached_render` — an upload, and one-shot per session. Its own
    /// cut so a boot-frame cost cannot be read as a steady-state one.
    pub(crate) restore: Hist,
    /// `promote_uploaded_rasters`: the band-complete sweep that puts a
    /// finished raster into this frame's paint list. Scales with pictures.
    pub(crate) promote: Hist,
    /// `report_raster_telemetry`: one clock read on all but one frame per
    /// telemetry period, and on that one frame up to a dozen formatted
    /// `console.log` calls. Its own cut because a browser console is not a
    /// free sink and this is the only writer inside the segment.
    pub(crate) raster: Hist,
    /// The `Apply` pump walk — every arrival channel drained and applied.
    pub(crate) apply: Hist,
    /// The `Advance` pump walk — loop playback.
    pub(crate) advance: Hist,
    /// The `Dispatch` pump walk — the refill and the four render
    /// dispatchers. **Not** `frame post (dispatch)`; see the type doc.
    pub(crate) dispatch: Hist,
    /// The tail: the volume-store budget enforcement, `update_loop_readiness`
    /// and `push_frame_inputs`. Named rather than folded so a `pump` residual
    /// cannot hide in an unnamed remainder.
    pub(crate) settle: Hist,
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

/// Where `present_frame` crossed the seven boundaries between the eight things
/// it does after the swapchain acquire returns. Taken by that call itself —
/// like [`PostPhaseStamps`] and [`PumpPhaseStamps`], and unlike the `prepare`
/// and `ui` splits, this segment has no single callee to carry them back.
#[derive(Clone, Copy)]
pub(crate) struct FinishPhaseStamps {
    /// The ledger filing (`record_prepare_phases`, `record_acquire`), the
    /// deferred-mirror repaint-delay choice and the surface-status match —
    /// everything between the acquire's return and the first GPU call.
    pub(crate) filed: Instant,
    /// `create_view` on the acquired surface texture's return.
    pub(crate) viewed: Instant,
    /// `EguiRenderer::draw`'s return: the main render pass, **encoded**.
    pub(crate) drawn: Instant,
    /// `probe_end_frame`'s return — the GPU probe's claimed brackets resolved
    /// into the encoder. A no-op on an arm with no timestamp probe installed,
    /// and that zero is the reading.
    pub(crate) resolved: Instant,
    /// `PreparedFrame::submit`'s return: `encoder.finish()` plus the one
    /// `queue.submit` that carries every command buffer this frame recorded.
    pub(crate) submitted: Instant,
    /// `probe_collect`'s return — the probe's ring harvested. Documented as
    /// never blocking; this cut is what makes that claim checkable.
    pub(crate) collected: Instant,
    /// `free_textures`' return.
    pub(crate) freed: Instant,
}

/// Where the `finish` segment's time went, cut at the seams `present_frame`
/// has.
///
/// # Denominator, and it is NOT the other splits'
///
/// **Every presented frame, interact and idle alike** — deliberately wider
/// than [`PrepareHists`], [`UiHists`], [`PumpHists`] and [`PostHists`], all
/// of which record inside `finalize`'s `if interacted` arm. That choice is
/// the whole reason this family is useful: the frames `finish` is a p99
/// contributor on are **idle** ones. Measured on this box, Firefox, scene D,
/// 3440x1440@174.96, n=107 interact frames: of the eight worst frames of a
/// gesture window every one was family=idle, and `finish` was the largest
/// segment on two of them, one at 14,179 us. A split recorded the way its
/// four siblings are would have read EMPTY on exactly those frames.
///
/// So `frame finish (*)` is **never added to `frame segment (finish)`** and
/// is not even a decomposition of it: it decomposes the same span over a
/// strictly larger frame set. [`FinishHists::whole`] is that parent, recorded
/// here on this family's own denominator, so every share this family supports
/// is computable without borrowing another family's `n`.
///
/// The eight named cuts are contiguous, so they telescope to `whole`
/// (`the_finish_phases_telescope_to_finish`).
#[derive(Default)]
pub(crate) struct FinishHists {
    /// See [`FinishPhaseStamps::filed`].
    pub(crate) file: Hist,
    /// See [`FinishPhaseStamps::viewed`].
    pub(crate) view: Hist,
    /// See [`FinishPhaseStamps::drawn`].
    pub(crate) draw: Hist,
    /// See [`FinishPhaseStamps::resolved`].
    pub(crate) resolve: Hist,
    /// See [`FinishPhaseStamps::submitted`].
    pub(crate) submit: Hist,
    /// See [`FinishPhaseStamps::collected`].
    pub(crate) collect: Hist,
    /// See [`FinishPhaseStamps::freed`].
    pub(crate) free: Hist,
    /// `SurfaceTexture::present`'s return. **The wait candidate**: on a real
    /// compositor this is where the frame thread meets the display's own
    /// back-pressure, and a cost here is not one any code of ours can be made
    /// faster to avoid.
    pub(crate) present: Hist,
    /// The `finish` segment itself on THIS family's denominator — every
    /// presented frame, not just the interact ones `SegmentHists::finish`
    /// counts. Carried so the eight cuts above have a parent that shares
    /// their `n`; the two `finish` figures are the same span over different
    /// frame sets and are never added.
    pub(crate) whole: Hist,
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
    /// `render_top_bar`.
    pub(crate) topbar: Hist,
    /// `render_status_bar`.
    pub(crate) statusbar: Hist,
    /// `render_stack_and_inspector` — the remainder of the shell.
    pub(crate) stack: Hist,
    /// The time dialog, between the shell and the panes. Its own cut because
    /// a dialog that is not open should not be charged to the map surfaces.
    pub(crate) dialog: Hist,
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

/// What one `dispatch_overlay_renders` call spent, by sub-cut, accumulated in
/// **nanoseconds**.
///
/// Nanoseconds and not microseconds because this one accumulates: `post`'s six
/// siblings are each a single span between two instants, while `dispatch` runs
/// a loop and these are sums across it. Truncating to whole microseconds once
/// per surviving request would round every sub-microsecond piece to zero and
/// hand the difference to the residual, which is the one figure that must not
/// absorb an artifact — it is read as "time these six do not name".
///
/// Filled field-wise by `spawn_overlay_render`, once per surviving request;
/// `dedupe_ns` is the dispatcher's own and is added once per call.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub(crate) struct DispatchCuts {
    /// `deduplicate_overlay_renders`: the grouping map and the collect out of
    /// it. Scales with the number of requests the frame emitted.
    pub(crate) dedupe_ns: u64,
    /// The per-pane in-flight marks and the dispatch record written beside
    /// them. Scales with **pane count**, which is what makes it a structurally
    /// weak suspect on a one-pane scene.
    pub(crate) marks_ns: u64,
    /// `hydrate_layer_states`, and the unconditional radar-selection publish
    /// it opens with. Scales with the pane's slot count.
    pub(crate) hydrate_ns: u64,
    /// `prepare_job` — for a polygon layer, a whole `paint_input` built here,
    /// on the frame thread. Scales with **layer data**.
    pub(crate) prepare_ns: u64,
    /// `hit_items`: the page-side half of a hit map, one `Arc` clone per item.
    /// Answered by two handlers only. Scales with **layer data**.
    pub(crate) hitmap_ns: u64,
    /// `offload_job` and the supersede/cancel seam after it. Scales with pane
    /// count and with what supersession left orphaned.
    pub(crate) offload_ns: u64,
}

impl DispatchCuts {
    /// Add `other` field-wise, saturating. One call per surviving request.
    pub(crate) fn add(&mut self, other: DispatchCuts) {
        self.dedupe_ns = self.dedupe_ns.saturating_add(other.dedupe_ns);
        self.marks_ns = self.marks_ns.saturating_add(other.marks_ns);
        self.hydrate_ns = self.hydrate_ns.saturating_add(other.hydrate_ns);
        self.prepare_ns = self.prepare_ns.saturating_add(other.prepare_ns);
        self.hitmap_ns = self.hitmap_ns.saturating_add(other.hitmap_ns);
        self.offload_ns = self.offload_ns.saturating_add(other.offload_ns);
    }

    /// Whether anything was accumulated at all — a dispatch that ran but
    /// spent under a nanosecond in every cut is not distinguishable from one
    /// that never ran, and this reports the second as an absence.
    fn any(self) -> bool {
        self.dedupe_ns
            | self.marks_ns
            | self.hydrate_ns
            | self.prepare_ns
            | self.hitmap_ns
            | self.offload_ns
            != 0
    }
}

/// Where the `post` segment's `dispatch` cut went — the split that names
/// which of the six things `dispatch_overlay_renders` inlines is the one that
/// costs.
///
/// # Denominator
///
/// **Exactly [`PostHists::dispatch`]'s** — presented interact frames on which
/// the tail dispatched at all. Six named cuts plus a residual, so they
/// telescope to `dispatch` by construction
/// (`the_dispatch_cuts_telescope_to_dispatch`).
///
/// **Never added to `frame post (dispatch)`.** These are not a seventh post
/// cut beside it; they *are* it, opened up — [`PostHists`]' own relationship
/// to `frame segment (post)`, one level down. The reporting prefix is
/// `frame dispatch` for that reason.
///
/// # What this split was cut to answer
///
/// Measured on Firefox scene D, quiet box: `dispatch` held **27,728 of the
/// 33,043 µs** of `post` — 84% — carried by 19 of 176 frames. `post`'s split
/// could say *that* and could not say *which*, because the call inlines a
/// dedupe, a per-pane mark loop, a state hydrate, a `prepare_job`, a
/// `hit_items` and an offload, and a percentile over the whole span names
/// none of them.
///
/// # The residual reads high, and the six read low
///
/// Each cut converts its own nanosecond sum to whole microseconds and
/// truncates down; the residual is what the parent span has left after all
/// six. So up to six microseconds of truncation per dispatch land in the
/// residual rather than in the cut that earned them. Stated because the
/// residual is the figure that would otherwise be read as an unnamed cost:
/// a residual of a few microseconds on a frame with six live cuts is
/// arithmetic, not a finding.
#[derive(Default)]
pub(crate) struct DispatchHists {
    /// See [`DispatchCuts::dedupe_ns`].
    pub(crate) dedupe: Hist,
    /// See [`DispatchCuts::marks_ns`].
    pub(crate) marks: Hist,
    /// See [`DispatchCuts::hydrate_ns`].
    pub(crate) hydrate: Hist,
    /// See [`DispatchCuts::prepare_ns`].
    pub(crate) prepare: Hist,
    /// See [`DispatchCuts::hitmap_ns`].
    pub(crate) hitmap: Hist,
    /// See [`DispatchCuts::offload_ns`].
    pub(crate) offload: Hist,
    /// `dispatch` minus the six above — arithmetic, not inference. Any
    /// dispatch time the six do not name is a gap in this decomposition, and
    /// this is where it shows.
    pub(crate) residual: Hist,
}

/// The per-segment histograms of interact frames, and only interact frames:
/// the segments exist to say where an interact frame's service went, and
/// mixing idle frames in would average the answer away.
#[derive(Default)]
pub(crate) struct SegmentHists {
    /// `handle_redraw` entry to `setup_egui_frame` entry — the pollers,
    /// eviction, deferred drops and the autosave check. Opened up by
    /// [`PreHists`], whose seven cuts telescope to this within [`micros`]'
    /// truncation.
    pub(crate) pre: Hist,
    /// `setup_egui_frame` entry to `Gui::ui` — theme, restore, the raster
    /// promote and the three pump phases.
    pub(crate) pump: Hist,
    /// The `Gui::ui` call itself: layout and the paint list. Opened up by
    /// [`UiHists`], whose nine cuts telescope to this within [`micros`]'
    /// truncation.
    pub(crate) ui: Hist,
    /// `Gui::ui` return to the acquire: mirror planning, tessellation, the
    /// texture-delta uploads and egui's buffer staging. Opened up by
    /// [`PrepareHists`], whose six cuts telescope to this within [`micros`]'
    /// truncation.
    pub(crate) prepare: Hist,
    /// Acquire return to `present_frame` return: draw, submit, present.
    pub(crate) finish: Hist,
    /// `present_frame` return to `finalize`: action processing and the
    /// repaint scheduling tail of `handle_redraw`. Opened up by
    /// [`PostHists`], whose seven cuts telescope to this within [`micros`]'
    /// truncation.
    pub(crate) post: Hist,
}

/// The anatomy of the single most expensive presented frame since the last
/// report — the one reading scene D's figure of merit asks for and that no
/// histogram can give.
///
/// # Why a latch and not a percentile
///
/// A `Hist` can say "a frame in the 13.5–16 ms bin happened"; it cannot say
/// which of the six segments that frame spent its time in, because the six
/// are recorded into six independent histograms and nothing ties one frame's
/// samples together again afterwards. Scene D's verdict is `p99` **AND**
/// `max` — "a single stalled click frame is the defect" — so the campaign's
/// own figure of merit is one this instrument could describe only by
/// inference. That inference has already been made and been wrong: CARD-D
/// attributed a 53.8 ms scene-D max to the `ui` segment, and the `ui` split
/// then measured `ui` at 3% of service.
///
/// # The denominator, and it is NOT the segments'
///
/// **Every presented frame**, interact and idle alike — which is the whole
/// point. The six segment histograms and all nineteen cuts record interact
/// frames only, and a click's *consequences* (the raster dispatch it causes,
/// the texture the answer uploads, the source it releases) are paid on the
/// frames after the one that carried the pointer event, every one of which
/// this ledger files as idle. Measured on this box, scene D, 1920x1080 on a
/// 3440x1440@174.96 display, main@3d5e1559: a two-loop window's worst
/// **interact** frame lands in the 2.83–3.36 ms bin while its worst **idle**
/// frame lands in 5.66–6.73 ms, and no split in the tree can open the latter.
/// So `interact` was never where scene D's worst frame lived.
///
/// # Windowed, because a maximum cannot be differenced
///
/// Taken and cleared by each report, so the figure is "the worst frame of the
/// last telemetry period" and a bracket's answer is the MAX over its ticks.
/// Every other family here is cumulative-from-boot and windowed by
/// subtraction; a running maximum cannot be subtracted, so this one is
/// windowed at the source instead. `frame worst` is deliberately a different
/// prefix from both `frame segments` and `frame segment` — its six figures
/// are one frame's microseconds, not a percentile over many, and adding it to
/// either would be adding one frame to a distribution that already contains
/// it.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub(crate) struct WorstFrame {
    /// This frame's service, on `SERVICE_FROM_WHOLE_FRAME`'s spelling — the
    /// figure the latch is ordered by.
    pub(crate) service: u32,
    /// `[pre, pump, ui, prepare, finish, post]`, this one frame's.
    pub(crate) segments: [u32; 6],
    /// The nine `ui` cuts of THIS frame, in [`UiHists`]' order:
    /// `[poll, layout, topbar, statusbar, stack, dialog, panes, apply,
    /// chrome]`. They telescope to `segments[2]` **to within the eight
    /// microseconds nine truncating [`micros`] calls can lose, and never over
    /// it** — 1–6 µs short on the 588 real frames measured, mean 3.39, exact
    /// on none — so the worst frame's `ui` is decomposed by arithmetic on ONE
    /// frame rather than by pairing two cumulative distributions that share no
    /// frame. `the_worst_frames_ui_cuts_telescope_to_its_ui` derives that
    /// bound off the cut count and holds it.
    ///
    /// **Why it is here and not left to [`UiHists`].** Those nine record
    /// inside `finalize`'s `if interacted` arm, so a frame that carried no
    /// pointer event — every frame that PAYS for a click, and half the spikes
    /// measured on scene D — contributes to none of them. Until this field
    /// the only way to attribute a worst frame's `ui` to a cut was to observe
    /// that two cumulative maxima landed in adjacent bins, which is an
    /// inference across two aggregates and not a fact about one frame.
    ///
    /// Zeroed on a frame that left no `ui_phases` — an early return before
    /// the pass, which is the same frame that leaves no acquire and is not a
    /// sample of anything. **Zero new clock reads**: the stamps already exist
    /// on every presented frame; only the subtraction moved out of the arm.
    pub(crate) ui_cuts: [u32; 9],
    /// The seven `pre` cuts of THIS frame, in [`PreHists`]' order:
    /// `[platform, ingest, evict, drops, autosave, gate, ensure]`. They
    /// telescope to `segments[0]` within [`micros`]' truncation — at most six
    /// microseconds for seven cuts — so the worst frame's `pre` is decomposed
    /// by arithmetic on ONE frame.
    ///
    /// **Here for [`WorstFrame::ui_cuts`]' reason exactly.** [`PreHists`]
    /// records inside `finalize`'s `if interacted` arm, and the frame this
    /// split was cut for is a latched one: `pre` read 10,357 µs on it, 88–92 %
    /// of the whole frame. Half the expensive frames on scene D carry no
    /// pointer event and are filed idle, so the interact-only histograms
    /// cannot open the very frames the `max` verdict is about.
    ///
    /// Zeroed on a frame that left no `pre_phases` — one of the head's three
    /// early exits, which is the same frame that leaves no acquire and is not
    /// a sample of anything. **Zero new clock reads**: the six stamps are
    /// already taken on every frame that reaches the pass; only the
    /// subtraction moved out of the arm.
    pub(crate) pre_cuts: [u32; 7],
    /// Whether this frame's raw input carried interaction. Reported rather
    /// than filtered on: a scene whose worst frame is always idle is saying
    /// something, and a family column is how it says it.
    pub(crate) interact: bool,
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
    /// See [`PreHists`] — `segments.pre`, opened up, same frames.
    pre: PreHists,
    /// See [`PrepareHists`] — `segments.prepare`, opened up, same frames.
    prepare: PrepareHists,
    /// See [`UiHists`] — `segments.ui`, opened up, same frames.
    ui: UiHists,
    /// See [`PumpHists`] — `segments.pump`, opened up, same frames.
    pump: PumpHists,
    /// See [`PostHists`] — `segments.post`, opened up, same frames.
    post: PostHists,
    /// See [`FinishHists`] — the `finish` span, opened up, over EVERY
    /// presented frame rather than the interact ones alone.
    finish: FinishHists,
    /// See [`DispatchHists`] — `post.dispatch`, opened up, same frames.
    dispatch: DispatchHists,
    /// The dispatch accumulator the current `dispatch_overlay_renders` call is
    /// filling. Cleared and read by that call alone; a frame's value reaches
    /// `cur.dispatch` through `record_dispatch_cuts`, so an arrival-path
    /// dispatch — which runs upstream of the `post` tail — cannot leave a
    /// figure behind that `finalize` would file under `dispatch`.
    dispatch_scratch: DispatchCuts,
    /// The acquire span itself, interact frames only. Reported beside the
    /// segments and never inside service: it is the vsync block.
    acquire: Hist,
    /// Redraw-to-redraw of presented frames, both families.
    cadence: Hist,
    /// The worst presented frame since the last report — see [`WorstFrame`].
    /// `None` between a `take_worst` and the next presented frame, which is
    /// what makes the figure windowed rather than cumulative.
    worst: Option<WorstFrame>,
    /// The largest service any presented frame has cost this session, never
    /// cleared. Rides on the same line as the windowed figure so that ONE
    /// surviving line still names the worst frame of the whole run: a browser
    /// console ring holds 1200 entries and the rig scrapes the last 60, so a
    /// windowed maximum whose tick has scrolled out is indistinguishable from
    /// a run that never had a bad frame — and "absent" reading as "it never
    /// happened" is the failure this campaign keeps finding. A running total
    /// that is re-said every tick cannot be evicted into a false negative.
    ///
    /// The WHOLE frame, not its service: the p99 verdict needs the anatomy of
    /// the frame that set the maximum, and a `u32` here was the instrument
    /// throwing away the one frame it existed to describe.
    worst_since_boot: Option<WorstFrame>,
    /// The last presented frame's start, cadence's left stamp.
    last_presented_start: Option<Instant>,
    /// **Whether this frame should have been drawn at all** — the one family
    /// here that is not about what a frame cost. Its denominator is every
    /// presented frame, [`WorstFrame`]'s and not the segments'; see
    /// [`crate::frame_need`].
    need: crate::frame_need::NeedLedger,
}

/// Whole microseconds from `a` to `b`, saturating into the histogram's `u32`.
///
/// # Truncating down, and every split in this file pays for it
///
/// A segment is ONE call of this; the `n` cuts that decompose it are `n` more.
/// Each cut throws away its own fraction of a microsecond while the parent
/// throws away only the fraction of the sum, so
///
/// ```text
/// parent - sum(cuts) == floor(sum of the cuts' fractional parts),  in 0 ..= n-1
/// ```
///
/// A split therefore **telescopes to its parent to within `n - 1` µs and can
/// never exceed it** — not "exactly", however contiguous its stamps are.
/// Measured on the nine `ui` cuts of 588 real frames: 1–6 µs short of the
/// frame's own `ui`, mean 3.39 µs, and exact on **none** of them.
///
/// Per frame that is dust. Per WINDOW it is up to `n - 1` µs times the frame
/// count, which is why a family's share is read as its own `sum` over its
/// parent's `sum` rather than as a difference against it.
/// `the_worst_frames_ui_cuts_telescope_to_its_ui` derives the bound above and
/// holds it against instants a clock actually produced; [`DispatchHists`] is
/// the one split that already stated it, because its residual is where its own
/// truncation lands.
fn micros(a: Instant, b: Instant) -> u32 {
    b.duration_since(a).as_micros().min(u128::from(u32::MAX)) as u32
}

/// Whole nanoseconds from `a` to `b`, saturating into the dispatch
/// accumulator's `u64`. The nanosecond twin of [`micros`], and the one
/// spelling both `app.rs` and `app_fetch.rs` use to fill [`DispatchCuts`] —
/// see that type for why this split counts in nanoseconds where every other
/// one counts in microseconds.
pub(crate) fn nanos(a: Instant, b: Instant) -> u64 {
    b.duration_since(a).as_nanos().min(u128::from(u64::MAX)) as u64
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

/// The seven cuts of the `dispatch` cut, in call order:
/// `[dedupe, marks, hydrate, prepare, hitmap, offload, residual]` — see
/// [`DispatchHists`], whose fields these are.
///
/// `dispatch` is the parent span in whole microseconds, as
/// [`post_phase_micros`] computed it; `cuts` is the nanosecond accumulation
/// the call itself made. The residual is the parent minus the six, saturating
/// at zero — a free function so the telescoping is testable without a frame,
/// on [`ui_phase_micros`]' terms.
///
/// **Saturating and not asserting.** The six are measured inside the span the
/// parent brackets, so they cannot legitimately exceed it; but the parent is
/// two clock reads and the six are twelve, and a clock that steps backwards
/// between them (a coarse or non-monotonic web clock) would otherwise panic
/// on the frame thread. A zero residual is the honest report of that case.
fn dispatch_cut_micros(cuts: DispatchCuts, dispatch: u32) -> [u32; 7] {
    let us = |ns: u64| -> u32 { (ns / 1_000).min(u64::from(u32::MAX)) as u32 };
    let named = [
        us(cuts.dedupe_ns),
        us(cuts.marks_ns),
        us(cuts.hydrate_ns),
        us(cuts.prepare_ns),
        us(cuts.hitmap_ns),
        us(cuts.offload_ns),
    ];
    let claimed = named.iter().fold(0u32, |sum, &cut| sum.saturating_add(cut));
    let [dedupe, marks, hydrate, prepare, hitmap, offload] = named;
    [
        dedupe,
        marks,
        hydrate,
        prepare,
        hitmap,
        offload,
        dispatch.saturating_sub(claimed),
    ]
}

/// The standing worst frame after `candidate` has been offered to it.
///
/// A free function for the reason `prepare_phase_micros` is one: the property
/// that matters — **which frames are eligible** — is then testable without
/// driving a real frame through `finalize`, and a `FrameLedger` cannot be
/// handed synthetic instants because every `mark_*` reads the clock itself.
///
/// A candidate must be **strictly greater** to take the slot, so the FIRST
/// frame to reach a given service keeps it. Ties are common at microsecond
/// resolution, and the earlier frame is the one a reader can still find in
/// the log above the line that reports it.
fn latch_worst(standing: Option<WorstFrame>, candidate: WorstFrame) -> WorstFrame {
    match standing {
        Some(worst) if worst.service >= candidate.service => worst,
        _ => candidate,
    }
}

/// The seven contiguous cuts of the `pre` segment, in call order:
/// `[platform, ingest, evict, drops, autosave, gate, ensure]` — see
/// [`PreHists`], whose fields these are.
///
/// Contiguous by construction: each cut ends where the next begins, and the
/// pair at the ends are `pre`'s own boundaries, so the seven sum to
/// `micros(start, setup)` to within the six microseconds seven truncating
/// [`micros`] calls can lose — see that function. A free function so the
/// telescoping is testable without a frame.
fn pre_phase_micros(start: Instant, phases: &PrePhaseStamps, setup: Instant) -> [u32; 7] {
    [
        micros(start, phases.polled),
        micros(phases.polled, phases.ingested),
        micros(phases.ingested, phases.evicted),
        micros(phases.evicted, phases.dropped),
        micros(phases.dropped, phases.saved),
        micros(phases.saved, phases.gated),
        micros(phases.gated, setup),
    ]
}

/// The six contiguous cuts of the `prepare` segment, in pass order:
/// `[plan, end_pass, tessellate, upload, mirror, buffers]` — see
/// [`PrepareHists`], whose fields these are.
///
/// Contiguous by construction: each cut ends where the next begins, and the
/// pair at the ends are `prepare`'s own boundaries, so the six sum to
/// `micros(ui_end, acquire_start)` to within the five microseconds six
/// truncating [`micros`] calls can lose — see that function. A free function
/// so the telescoping is testable without a frame.
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

/// The nine contiguous cuts of the `ui` segment, in call order:
/// `[poll, layout, topbar, statusbar, stack, dialog, panes, apply, chrome]` —
/// see [`UiHists`], whose fields these are.
///
/// Nine where it was six. `shell` was ~1 ms of every interact frame as ONE cut
/// covering the top bar, the status bar and the layer stack, so nothing said
/// which of the three it was; and the time dialog was charged to `panes`, so a
/// dialog that is not open was counted against the map surfaces.
///
/// Contiguous by construction: each cut ends where the next begins, and the
/// pair at the ends are `ui`'s own boundaries, so the **nine** sum to
/// `micros(ui_start, ui_end)` to within the eight microseconds nine
/// truncating [`micros`] calls can lose — measured 1–6 on real frames, mean
/// 3.39, exact on none of 588. A free function so the telescoping is testable
/// without a frame.
fn ui_phase_micros(
    ui_start: Instant,
    phases: &squallar_egui::shell_api::UiPhaseStamps,
    ui_end: Instant,
) -> [u32; 9] {
    [
        micros(ui_start, phases.polled),
        micros(phases.polled, phases.laid_out),
        micros(phases.laid_out, phases.topbar),
        micros(phases.topbar, phases.statusbar),
        micros(phases.statusbar, phases.shell),
        micros(phases.shell, phases.dialog),
        micros(phases.dialog, phases.panes),
        micros(phases.panes, phases.applied),
        micros(phases.applied, ui_end),
    ]
}

/// The eight contiguous cuts of the `pump` segment, in call order:
/// `[begin, restore, promote, raster, apply, advance, dispatch, settle]` —
/// see [`PumpHists`], whose fields these are.
///
/// Contiguous by construction: each cut ends where the next begins, and the
/// pair at the ends are `pump`'s own boundaries, so the eight sum to
/// `micros(setup, ui_start)` to within the seven microseconds eight
/// truncating [`micros`] calls can lose — see that function. A free function
/// so the telescoping is testable without a frame.
fn pump_phase_micros(setup: Instant, phases: &PumpPhaseStamps, ui_start: Instant) -> [u32; 8] {
    [
        micros(setup, phases.began),
        micros(phases.began, phases.restored),
        micros(phases.restored, phases.promoted),
        micros(phases.promoted, phases.rastered),
        micros(phases.rastered, phases.applied),
        micros(phases.applied, phases.advanced),
        micros(phases.advanced, phases.dispatched),
        micros(phases.dispatched, ui_start),
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
/// `micros(present_return, closed)` to within the six microseconds seven
/// truncating [`micros`] calls can lose — see that function. A free function
/// so the telescoping is testable without a frame.
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

/// The eight contiguous cuts of the `finish` segment, in call order:
/// `[file, view, draw, resolve, submit, collect, free, present]` — see
/// [`FinishHists`], whose fields these are.
///
/// Contiguous by construction: each cut ends where the next begins, and the
/// pair at the ends are `finish`'s own boundaries, so the eight sum to
/// `micros(acquire_end, present_return)` to within the seven microseconds
/// eight truncating [`micros`] calls can lose — see that function. A free
/// function so the telescoping is testable without a frame.
fn finish_phase_micros(
    acquire_end: Instant,
    phases: &FinishPhaseStamps,
    present_return: Instant,
) -> [u32; 8] {
    [
        micros(acquire_end, phases.filed),
        micros(phases.filed, phases.viewed),
        micros(phases.viewed, phases.drawn),
        micros(phases.drawn, phases.resolved),
        micros(phases.resolved, phases.submitted),
        micros(phases.submitted, phases.collected),
        micros(phases.collected, phases.freed),
        micros(phases.freed, present_return),
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

    /// The phase stamps `handle_redraw`'s head took on its way through.
    /// Recorded unconditionally, immediately before the call that opens
    /// `pump`; `finalize` decides whether this frame is a sample.
    pub(crate) fn record_pre_phases(&mut self, stamps: PrePhaseStamps) {
        self.cur.pre_phases = Some(stamps);
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
    /// Add what one surviving request spent to the dispatch call in progress.
    ///
    /// Called by `spawn_overlay_render`, whose signature is not the place to
    /// carry this: `app.rs` is pinned to exactly one textual call of it and
    /// its ~30 test call sites all pass four arguments. The accumulator lives
    /// here instead, and the call that owns the span clears and takes it.
    pub(crate) fn add_dispatch_cuts(&mut self, cuts: DispatchCuts) {
        self.dispatch_scratch.add(cuts);
    }

    /// Clear the accumulator and return what it held.
    ///
    /// Called twice by `dispatch_overlay_renders` — once on entry to start
    /// from zero, once on return to read the call's own total. The
    /// clear-on-entry is what keeps an arrival-path dispatch, which runs
    /// upstream of the `post` tail in the same frame, out of the tail's
    /// figure: that caller takes its total and drops it.
    pub(crate) fn take_dispatch_cuts(&mut self) -> DispatchCuts {
        std::mem::take(&mut self.dispatch_scratch)
    }

    /// File the `post` tail's dispatch decomposition for this frame.
    ///
    /// `None` from a frame whose tail dispatched nothing, which is most of
    /// them — and that absence is recorded as an absence rather than as seven
    /// zeros, because a frame with no dispatch has no `dispatch` cut to
    /// decompose and would otherwise pull every percentile here to the floor.
    pub(crate) fn record_dispatch_cuts(&mut self, cuts: DispatchCuts) {
        self.cur.dispatch = cuts.any().then_some(cuts);
    }

    /// The phase stamps `setup_egui_frame` took on its way through. Recorded
    /// unconditionally; `finalize` decides whether this frame is a sample.
    pub(crate) fn record_pump_phases(&mut self, stamps: PumpPhaseStamps) {
        self.cur.pump_phases = Some(stamps);
    }

    pub(crate) fn record_post_phases(&mut self, stamps: PostPhaseStamps) {
        self.cur.post_phases = Some(stamps);
    }

    /// The phase stamps `present_frame` took after the acquire returned.
    /// Recorded unconditionally; `finalize` decides whether this frame is a
    /// sample — and unlike every other split, its answer is yes for idle
    /// frames too. See [`FinishHists`] for why.
    pub(crate) fn record_finish_phases(&mut self, stamps: FinishPhaseStamps) {
        self.cur.finish_phases = Some(stamps);
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

        // **The first thing past the last early return, on purpose.** This is
        // the point at which the frame is known to have presented, and the
        // cause register must be cleared exactly once per presented frame:
        // clear it earlier and a frame that early-returned would throw away
        // causes nothing has drawn yet, so the frame that finally draws them
        // would be reported as unnecessary. One relaxed `swap` and a handful
        // of integer increments; no clock read, which is why the counts this
        // module's doc pins are unchanged.
        self.need.record(squallar_egui::frame_need::take());

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

        // **Above the arm on purpose, and the only ARITHMETIC that is.** These
        // nine ride on `WorstFrame`, whose denominator is every presented
        // frame, so they have to be computed on the idle ones too -- the
        // frames that pay for a click, where half of scene D's spikes live and
        // where `UiHists` records nothing. The nine `self.ui.*.record` calls
        // stay inside the arm below, so no histogram's denominator moves; only
        // the subtraction left it. Zero new clock reads: `m.ui_phases` is
        // already stamped on every presented frame.
        let ui_cuts = m.ui_phases.as_ref().map_or([0u32; 9], |phases| {
            ui_phase_micros(ui_start, phases, ui_end)
        });

        // The same, and for the same reason: `pre` is the segment that read
        // 10,357 us on ONE latched frame, and a latched frame is as often
        // idle as not. Above the arm, and the statement may not read
        // `interacted` -- see
        // `the_worst_frames_pre_cuts_are_computed_outside_the_interact_arm`.
        let pre_cuts = m
            .pre_phases
            .as_ref()
            .map_or([0u32; 7], |phases| pre_phase_micros(start, phases, setup));

        // EVERY split below is interact-only, and that is a limit worth
        // stating rather than rediscovering. A frame the renderer did not call
        // interacted -- boot among them -- contributes to `service_idle`, to
        // the worst-frame latch and to `finish`, and to nothing else: no
        // segment, no `prepare`, `post`, `pump` or `dispatch` cut, and no `ui`
        // histogram. So a boot frame's anatomy is visible only on the
        // `frame worst:` line -- which since `ui_cuts` landed carries that
        // frame's own nine `ui` cuts, so `ui` is the one segment a search
        // CAN open up behind such a frame; every other family is still empty.
        // Measured instance, 2026-09-04: a 13 ms `pump` on a Mac Firefox boot
        // frame, against a 203 us interact mean, attributable to no cut in the
        // tree.
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
            // Inside the interact arm, and only here: these seven are cuts of
            // the `pre` recorded above, and the left boundary is the very
            // `start` stamp `pre` measures from. The seven values are
            // computed above the arm because `WorstFrame::pre_cuts` needs
            // them on idle frames too; the RECORD calls stay here, and still
            // only on a frame that actually left `pre_phases`, so this
            // family's denominator is `segments.pre`'s exactly. A frame with
            // no phases must contribute no sample -- seven zeros would be
            // seven false readings, not an absence.
            if m.pre_phases.is_some() {
                let [platform, ingest, evict, drops, autosave, gate, ensure] = pre_cuts;
                self.pre.platform.record(platform);
                self.pre.ingest.record(ingest);
                self.pre.evict.record(evict);
                self.pre.drops.record(drops);
                self.pre.autosave.record(autosave);
                self.pre.gate.record(gate);
                self.pre.ensure.record(ensure);
            }
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
            // The same, for the `pump` segment recorded above — the eight
            // cuts of `setup_egui_frame`, whose left boundary is the very
            // `setup` stamp `pump` measures from.
            if let Some(phases) = m.pump_phases.as_ref() {
                let [
                    begin,
                    restore,
                    promote,
                    raster,
                    apply,
                    advance,
                    dispatch,
                    settle,
                ] = pump_phase_micros(setup, phases, ui_start);
                self.pump.begin.record(begin);
                self.pump.restore.record(restore);
                self.pump.promote.record(promote);
                self.pump.raster.record(raster);
                self.pump.apply.record(apply);
                self.pump.advance.record(advance);
                self.pump.dispatch.record(dispatch);
                self.pump.settle.record(settle);
            }
            // The same, for the `ui` segment recorded above. Independent of
            // the prepare block: a different segment, a different set of
            // stamps, the same denominator rule.
            //
            // The nine values are computed above the arm because
            // `WorstFrame::ui_cuts` needs them on idle frames too; the RECORD
            // calls stay here, and still only on a frame that actually left
            // `ui_phases`, so this family's denominator is exactly what it was
            // before that move. A frame with no phases must contribute no
            // sample -- nine zeros would be nine false readings, not an
            // absence.
            if m.ui_phases.is_some() {
                let [
                    poll,
                    layout,
                    topbar,
                    statusbar,
                    stack,
                    dialog,
                    panes,
                    apply,
                    chrome,
                ] = ui_cuts;
                self.ui.poll.record(poll);
                self.ui.layout.record(layout);
                self.ui.topbar.record(topbar);
                self.ui.statusbar.record(statusbar);
                self.ui.stack.record(stack);
                self.ui.dialog.record(dialog);
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
                // One level further down, and only on the frames whose tail
                // actually dispatched: `dispatch` is the cut recorded two
                // lines above `back`, and these seven telescope to exactly
                // it. A frame that dispatched nothing leaves `cur.dispatch`
                // empty and contributes no sample here — see
                // `record_dispatch_cuts`.
                if let Some(cuts) = m.dispatch {
                    let [dedupe, marks, hydrate, prepare, hitmap, offload, residual] =
                        dispatch_cut_micros(cuts, dispatch);
                    self.dispatch.dedupe.record(dedupe);
                    self.dispatch.marks.record(marks);
                    self.dispatch.hydrate.record(hydrate);
                    self.dispatch.prepare.record(prepare);
                    self.dispatch.hitmap.record(hitmap);
                    self.dispatch.offload.record(offload);
                    self.dispatch.residual.record(residual);
                }
            }
        } else {
            self.service_idle.record(service);
        }

        // **Outside the `interacted` arm on purpose, and the only split that
        // is.** `finish`'s expensive frames are idle ones — see
        // [`FinishHists`] for the measurement that says so — so this family
        // records on every presented frame and carries its own parent
        // (`whole`) rather than borrowing `segments.finish`'s narrower one.
        if let Some(phases) = m.finish_phases.as_ref() {
            let [file, view, draw, resolve, submit, collect, free, present] =
                finish_phase_micros(acquire_end, phases, present_return);
            self.finish.file.record(file);
            self.finish.view.record(view);
            self.finish.draw.record(draw);
            self.finish.resolve.record(resolve);
            self.finish.submit.record(submit);
            self.finish.collect.record(collect);
            self.finish.free.record(free);
            self.finish.present.record(present);
            self.finish.whole.record(segments[4]);
        }

        // **Every presented frame, both families** — see [`WorstFrame`]. The
        // comparison is on service, which is the figure the bar is stated in,
        // and it is deliberately outside the `interacted` block above: the
        // frame that pays for a click carries no pointer event and is filed
        // idle, so a latch inside that block would be blind to exactly the
        // frame scene D's `max` verdict is about.
        self.worst = Some(latch_worst(
            self.worst,
            WorstFrame {
                service,
                segments,
                ui_cuts,
                pre_cuts,
                interact: interacted,
            },
        ));
        if self.worst_since_boot.is_none_or(|w| service > w.service) {
            self.worst_since_boot = Some(WorstFrame {
                service,
                segments,
                ui_cuts,
                pre_cuts,
                interact: interacted,
            });
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

    /// See [`PreHists`] — `segments.pre`, opened up.
    pub(crate) fn pre_phases(&self) -> &PreHists {
        &self.pre
    }

    pub(crate) fn prepare_phases(&self) -> &PrepareHists {
        &self.prepare
    }

    pub(crate) fn ui_phases(&self) -> &UiHists {
        &self.ui
    }

    /// See [`DispatchHists`] — `post.dispatch`, opened up.
    pub(crate) fn dispatch_cuts(&self) -> &DispatchHists {
        &self.dispatch
    }

    /// See [`PumpHists`] — `segments.pump`, opened up.
    pub(crate) fn pump_phases(&self) -> &PumpHists {
        &self.pump
    }

    pub(crate) fn post_phases(&self) -> &PostHists {
        &self.post
    }

    /// See [`FinishHists`] — the `finish` span opened up, over every
    /// presented frame rather than the interact ones alone.
    pub(crate) fn finish_phases(&self) -> &FinishHists {
        &self.finish
    }

    pub(crate) fn acquire(&self) -> &Hist {
        &self.acquire
    }

    pub(crate) fn cadence(&self) -> &Hist {
        &self.cadence
    }

    /// File what `handle_redraw`'s tail claimed, for the frame after this one.
    /// See [`crate::frame_need::NeedLedger::record_wake_claim`].
    pub(crate) fn record_wake_claim(&mut self, claim: crate::frame_need::WakeClaim) {
        self.need.record_wake_claim(claim);
    }

    /// The unnecessary-frame verdict's running totals. See
    /// [`crate::frame_need`].
    pub(crate) fn need(&self) -> crate::frame_need::Reading {
        self.need.reading()
    }

    /// The worst presented frame since the last call, and clear the latch.
    ///
    /// Takes rather than borrows, which is what makes the figure windowed:
    /// see [`WorstFrame`]'s note on why a maximum cannot be differenced the
    /// way every other family here is. `None` means no frame presented in
    /// this period — an absence, and reported as one.
    pub(crate) fn take_worst(&mut self) -> Option<WorstFrame> {
        self.worst.take()
    }

    /// The largest-service frame any presented frame has cost this session —
    /// its family and six segments — or `None` before the first presented frame.
    /// Never cleared, so it survives a console ring that has dropped the tick
    /// the frame happened in; see the field.
    pub(crate) fn worst_frame_since_boot(&self) -> Option<WorstFrame> {
        self.worst_since_boot
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
            budget_state: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        DispatchCuts, FinishPhaseStamps, Instant, PostPhaseStamps, PrePhaseStamps, PumpPhaseStamps,
        WorstFrame, dispatch_cut_micros, finish_phase_micros, latch_worst, micros,
        post_phase_micros, pre_phase_micros, prepare_phase_micros, pump_phase_micros,
        service_micros, ui_phase_micros,
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
    fn ui_phases_at(ui_start: Instant, offsets: [u64; 8]) -> UiPhaseStamps {
        let at = |us: u64| ui_start + std::time::Duration::from_micros(us);
        UiPhaseStamps {
            polled: at(offsets[0]),
            laid_out: at(offsets[1]),
            topbar: at(offsets[2]),
            statusbar: at(offsets[3]),
            shell: at(offsets[4]),
            dialog: at(offsets[5]),
            panes: at(offsets[6]),
            applied: at(offsets[7]),
        }
    }

    /// **The nine cuts are a decomposition of `ui`, not a sample of it.**
    ///
    /// The sum telescopes to `micros(ui_start, ui_end)` — the very span
    /// [`super::SegmentHists::ui`] records — so "what is in ui" is answered by
    /// subtraction rather than by inference, and any phase the split fails to
    /// name shows up as a gap instead of hiding inside a neighbour.
    #[test]
    fn the_ui_phases_telescope_to_ui() {
        let ui_start = Instant::now();
        let phases = ui_phases_at(
            ui_start,
            [300, 1_900, 9_000, 12_000, 24_100, 24_600, 39_400, 39_450],
        );
        let ui_end = ui_start + std::time::Duration::from_micros(41_000);

        let cuts = ui_phase_micros(ui_start, &phases, ui_end);
        assert_eq!(
            cuts,
            [300, 1_600, 7_100, 3_000, 12_100, 500, 14_800, 50, 1_550],
            "a cut moved: the nine no longer bracket the phases they are named \
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
        let base_offsets = [500u64, 2_500, 9_000, 14_000, 25_000, 30_000, 38_000, 39_000];
        let ui_end = ui_start + std::time::Duration::from_micros(41_000);
        let base = ui_phase_micros(ui_start, &ui_phases_at(ui_start, base_offsets), ui_end);

        for stamp in 0..8 {
            let mut moved = base_offsets;
            moved[stamp] -= 100;
            let cuts = ui_phase_micros(ui_start, &ui_phases_at(ui_start, moved), ui_end);
            let changed: Vec<usize> = (0..9).filter(|&i| cuts[i] != base[i]).collect();
            assert_eq!(
                changed,
                vec![stamp, stamp + 1],
                "moving stamp {stamp} did not move exactly the two cuts it \
                 bounds, so one of them is not reading it and the split is \
                 narrower than its nine names claim",
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
    /// nine names covered `ui` but where eight were pinned at zero would pass
    /// the telescoping test and report a single opaque number under nine
    /// headings — which is the instrument this replaces, renamed.
    #[test]
    fn no_ui_cut_is_structurally_pinned_to_zero() {
        let ui_start = Instant::now();
        let phases = ui_phases_at(
            ui_start,
            [300, 1_900, 9_000, 12_000, 24_100, 24_600, 39_400, 39_450],
        );
        let ui_end = ui_start + std::time::Duration::from_micros(41_000);
        let cuts = ui_phase_micros(ui_start, &phases, ui_end);
        assert!(
            cuts.iter().all(|&c| c > 0),
            "a cut is zero on stamps chosen to make all nine non-zero, so it \
             cannot be reading the span it is named for: {cuts:?}",
        );
    }

    /// A `handle_redraw` head whose boundaries land at the given microsecond
    /// offsets from the `start` stamp, so a test can state its stamps as
    /// arithmetic. The `pre` sibling of `pump_phases_at`.
    fn pre_phases_at(start: Instant, offsets: [u64; 6]) -> PrePhaseStamps {
        let at = |us: u64| start + std::time::Duration::from_micros(us);
        PrePhaseStamps {
            polled: at(offsets[0]),
            ingested: at(offsets[1]),
            evicted: at(offsets[2]),
            dropped: at(offsets[3]),
            saved: at(offsets[4]),
            gated: at(offsets[5]),
        }
    }

    /// **The seven cuts are a decomposition of `pre`, not a sample of it.**
    ///
    /// The sum telescopes to `micros(start, setup)` — the very span
    /// [`super::SegmentHists::pre`] records — so "what is in pre" is answered
    /// by subtraction rather than by inference. `pre` was the LAST segment
    /// with no split and a top-three tail owner while it had none: scene D,
    /// hardware Vulkan, n=1120 interact frames per leg over three legs, mean
    /// 325.6 us, p99 1,682 us, and 10,357 us on one latched frame — 88–92 %
    /// of that whole frame, attributable to nothing.
    ///
    /// **Truncation-aware from the first line it was written on.** Seven cuts
    /// are seven truncating [`micros`] calls where the parent is one, so the
    /// claim is exact only when every stamp lands on a whole microsecond, and
    /// is `assert_telescopes_within_truncation`'s derived bound otherwise —
    /// see that helper and `the_worst_frames_ui_cuts_telescope_to_its_ui`,
    /// whose gate asserted an exactness that was false on 588 of 588 real
    /// frames because its fixture could not produce a fraction.
    #[test]
    fn the_pre_phases_telescope_to_pre() {
        // ── Arm 1: whole-microsecond stamps, where the exactness IS true ──
        let start = Instant::now();
        let phases = pre_phases_at(start, [4, 94, 124, 184, 186, 195]);
        let setup = start + std::time::Duration::from_micros(207);

        let cuts = pre_phase_micros(start, &phases, setup);
        assert_eq!(
            assert_telescopes_within_truncation(&cuts, micros(start, setup), "whole-us stamps"),
            0,
            "six stamps with nothing below a microsecond on them still lost \
             time, so the seven are not contiguous cuts of one span",
        );
        assert_eq!(
            cuts,
            [4, 90, 30, 60, 2, 9, 12],
            "a cut moved: the seven no longer bracket the phases they are \
             named for",
        );
        assert_eq!(cuts.iter().sum::<u32>(), 207);

        // ── Arm 2: the same seven spans with half a microsecond of dust on
        // each, which is what a clock hands the constructor ──
        //
        // Seven fractions of 0.5 sum to 3.5, so the seven cuts fall exactly
        // 3 us short of a parent that is itself truncated. Deterministic, and
        // the arm that makes the bound below non-vacuous.
        let dusty_start = Instant::now();
        let mut at = 0u64;
        let mut ns = [0u64; 6];
        for (slot, len) in [4_500u64, 90_500, 30_500, 60_500, 2_500, 9_500]
            .into_iter()
            .enumerate()
        {
            at += len;
            ns[slot] = at;
        }
        let dusty = PrePhaseStamps {
            polled: dusty_start + std::time::Duration::from_nanos(ns[0]),
            ingested: dusty_start + std::time::Duration::from_nanos(ns[1]),
            evicted: dusty_start + std::time::Duration::from_nanos(ns[2]),
            dropped: dusty_start + std::time::Duration::from_nanos(ns[3]),
            saved: dusty_start + std::time::Duration::from_nanos(ns[4]),
            gated: dusty_start + std::time::Duration::from_nanos(ns[5]),
        };
        let dusty_setup = dusty_start + std::time::Duration::from_nanos(at + 12_500);
        let dusty_cuts = pre_phase_micros(dusty_start, &dusty, dusty_setup);
        assert_eq!(
            assert_telescopes_within_truncation(
                &dusty_cuts,
                micros(dusty_start, dusty_setup),
                "sub-microsecond stamps",
            ),
            3,
            "seven cuts each half a microsecond long in their fraction did \
             not lose the 3 us that truncating seven of them must lose, so \
             this arm is not exercising the truncation it exists for: \
             {dusty_cuts:?}",
        );
        assert_eq!(
            dusty_cuts,
            [4, 90, 30, 60, 2, 9, 12],
            "the same seven spans as arm 1, each truncated",
        );

        // ── Arm 3: instants a clock actually produced ──
        //
        // Six bare `Instant::now()` reads in the order `handle_redraw` takes
        // them, bracketed by the two the ledger takes. The bound is asserted
        // and a non-zero gap deliberately is not — see
        // `the_worst_frames_ui_cuts_telescope_to_its_ui`'s third arm for why
        // demanding truncation from a live clock would be asserting the
        // clock.
        let mut samples = 0;
        for _ in 0..256 {
            let live_start = Instant::now();
            let live = PrePhaseStamps {
                polled: Instant::now(),
                ingested: Instant::now(),
                evicted: Instant::now(),
                dropped: Instant::now(),
                saved: Instant::now(),
                gated: Instant::now(),
            };
            let live_setup = Instant::now();
            assert_telescopes_within_truncation(
                &pre_phase_micros(live_start, &live, live_setup),
                micros(live_start, live_setup),
                "instants from the clock",
            );
            samples += 1;
        }
        assert_eq!(
            samples, 256,
            "the real-clock arm did not take the samples it claims to have \
             taken, so its greenness is an absence and not a reading",
        );
    }

    /// **The non-vacuity floor under the `pre` split: a cut may not trivially
    /// cover the segment.**
    ///
    /// Telescoping alone is satisfied by a degenerate split — one cut holding
    /// the whole span and six zeros telescopes perfectly and decomposes
    /// nothing. So the floor is stated on the boundaries: **every one of the
    /// six stamps must be able to move the answer**, which is only true if
    /// each is read by two different cuts. Held by perturbation, on
    /// `every_ui_stamp_is_load_bearing_in_two_cuts`' terms.
    #[test]
    fn every_pre_stamp_is_load_bearing_in_two_cuts() {
        let start = Instant::now();
        // Its own fixture, not the telescoping test's: every cut here is
        // wider than the 100 us nudge, so a stamp that fails to move a cut
        // fails this test rather than underflowing it.
        let base_offsets = [500u64, 2_500, 9_000, 14_000, 15_200, 16_100];
        let setup = start + std::time::Duration::from_micros(20_100);
        let base = pre_phase_micros(start, &pre_phases_at(start, base_offsets), setup);

        for stamp in 0..6 {
            let mut moved = base_offsets;
            moved[stamp] -= 100;
            let cuts = pre_phase_micros(start, &pre_phases_at(start, moved), setup);
            let changed: Vec<usize> = (0..7).filter(|&i| cuts[i] != base[i]).collect();
            assert_eq!(
                changed,
                vec![stamp, stamp + 1],
                "moving stamp {stamp} did not move exactly the two cuts it \
                 bounds, so one of them is not reading it and the split is \
                 narrower than its seven names claim",
            );
            assert_eq!(
                (cuts[stamp], cuts[stamp + 1]),
                (base[stamp] - 100, base[stamp + 1] + 100),
                "the two cuts around stamp {stamp} did not trade the 100 us \
                 exactly, so the boundary between them is not the stamp",
            );
            assert_eq!(cuts.iter().sum::<u32>(), 20_100);
        }
    }

    /// **The floor's other half: no `pre` cut may be structurally empty.**
    ///
    /// A split whose seven names covered `pre` but where six were pinned at
    /// zero would pass the telescoping test and report a single opaque number
    /// under seven headings — which is the instrument this replaces, renamed.
    #[test]
    fn no_pre_cut_is_structurally_pinned_to_zero() {
        let start = Instant::now();
        let phases = pre_phases_at(start, [4, 94, 124, 184, 186, 195]);
        let setup = start + std::time::Duration::from_micros(207);
        let cuts = pre_phase_micros(start, &phases, setup);
        assert!(
            cuts.iter().all(|&c| c > 0),
            "a cut is zero on stamps chosen to make all seven non-zero, so it \
             cannot be reading the span it is named for: {cuts:?}",
        );
    }

    /// A `setup_egui_frame` whose boundaries land at the given microsecond
    /// offsets from the `setup` stamp, so a test can state its stamps as
    /// arithmetic. The `pump` sibling of `post_phases_at`.
    fn pump_phases_at(setup: Instant, offsets: [u64; 7]) -> PumpPhaseStamps {
        let at = |us: u64| setup + std::time::Duration::from_micros(us);
        PumpPhaseStamps {
            began: at(offsets[0]),
            restored: at(offsets[1]),
            promoted: at(offsets[2]),
            rastered: at(offsets[3]),
            applied: at(offsets[4]),
            advanced: at(offsets[5]),
            dispatched: at(offsets[6]),
        }
    }

    /// **The eight cuts are a decomposition of `pump`, not a sample of it.**
    ///
    /// The sum telescopes to `micros(setup, ui_start)` — the very span
    /// [`super::SegmentHists::pump`] records — so "what is in pump" is
    /// answered by subtraction rather than by inference. The sibling of
    /// `the_post_phases_telescope_to_post`, and the reason this split exists:
    /// `pump` was 11.8 ms on Firefox against 280 us on Chromium and nothing
    /// could say which of the eight it was.
    #[test]
    fn the_pump_phases_telescope_to_pump() {
        let setup = Instant::now();
        let phases = pump_phases_at(setup, [120, 130, 4_130, 9_130, 11_130, 11_180, 11_600]);
        let ui_start = setup + std::time::Duration::from_micros(11_700);

        let cuts = pump_phase_micros(setup, &phases, ui_start);
        assert_eq!(
            cuts,
            [120, 10, 4_000, 5_000, 2_000, 50, 420, 100],
            "a cut moved: the eight no longer bracket the boundaries they are \
             named for",
        );
        assert_eq!(
            cuts.iter().sum::<u32>(),
            micros(setup, ui_start),
            "the eight cuts do not sum to the pump span they decompose, so a \
             reading off this split is not a reading of pump",
        );
        assert_eq!(cuts.iter().sum::<u32>(), 11_700);
    }

    /// **The non-vacuity floor: a cut may not trivially cover `pump`.**
    ///
    /// `every_post_stamp_is_load_bearing_in_two_cuts`' sibling, and needed for
    /// the same reason: six of the eight cuts are near-zero on a healthy leg,
    /// so a split whose stamps had been folded together would look exactly
    /// like the reading everyone expects and would pass the telescoping test.
    #[test]
    fn every_pump_stamp_is_load_bearing_in_two_cuts() {
        let setup = Instant::now();
        let base_offsets = [500u64, 1_000, 1_500, 2_000, 2_500, 3_000, 3_500];
        let ui_start = setup + std::time::Duration::from_micros(4_000);
        let base = pump_phase_micros(setup, &pump_phases_at(setup, base_offsets), ui_start);

        for moved in 0..7 {
            let mut offsets = base_offsets;
            offsets[moved] += 100;
            let cuts = pump_phase_micros(setup, &pump_phases_at(setup, offsets), ui_start);
            let changed: Vec<usize> = (0..8).filter(|&i| cuts[i] != base[i]).collect();
            assert_eq!(
                changed,
                vec![moved, moved + 1],
                "moving stamp {moved} did not move exactly the two cuts it \
                 bounds, so a boundary is folded away and the split reports \
                 fewer spans than it names",
            );
            assert_eq!(
                cuts.iter().sum::<u32>(),
                base.iter().sum::<u32>(),
                "moving stamp {moved} changed the total, so the eight no \
                 longer telescope to pump",
            );
        }
    }

    /// A `handle_redraw` tail whose boundaries land at the given microsecond
    /// offsets from `present_return`, so a test can state its stamps as
    /// arithmetic. Named apart from the other two splits' fixtures: the three
    /// decompositions share this module and answer with different stamp types.
    fn finish_phases_at(acquire_end: Instant, offsets: [u64; 7]) -> FinishPhaseStamps {
        let at = |us: u64| acquire_end + std::time::Duration::from_micros(us);
        FinishPhaseStamps {
            filed: at(offsets[0]),
            viewed: at(offsets[1]),
            drawn: at(offsets[2]),
            resolved: at(offsets[3]),
            submitted: at(offsets[4]),
            collected: at(offsets[5]),
            freed: at(offsets[6]),
        }
    }

    /// **The eight cuts are a decomposition of `finish`, not a sample of it.**
    ///
    /// The sum telescopes to `micros(acquire_end, present_return)` — the very
    /// span [`super::SegmentHists::finish`] records and the very span
    /// [`super::FinishHists::whole`] records beside these eight — so a share
    /// computed against `whole` is a share of the whole segment.
    #[test]
    fn the_finish_phases_telescope_to_finish() {
        let acquire_end = Instant::now();
        let phases = finish_phases_at(acquire_end, [20, 60, 210, 215, 900, 905, 910]);
        let present_return = acquire_end + std::time::Duration::from_micros(1_400);

        let cuts = finish_phase_micros(acquire_end, &phases, present_return);
        assert_eq!(
            cuts,
            [20, 40, 150, 5, 685, 5, 5, 490],
            "a cut moved: the eight no longer bracket the boundaries they are \
             named for",
        );
        assert_eq!(
            cuts.iter().sum::<u32>(),
            micros(acquire_end, present_return),
            "the eight cuts do not sum to the finish span they decompose, so \
             a share of `whole` computed from them is not a share of finish",
        );
        assert_eq!(cuts.iter().sum::<u32>(), 1_400);
    }

    /// **The non-vacuity floor: a cut may not trivially cover `finish`.**
    ///
    /// The sibling of `every_post_stamp_is_load_bearing_in_two_cuts`, and the
    /// hazard is the same one: on an arm with no GPU probe installed
    /// `resolve` and `collect` are genuinely near-zero, so a split whose
    /// stamps had been folded together would look exactly like a real reading
    /// and pass the telescoping test above. Held by perturbation instead.
    #[test]
    fn every_finish_stamp_is_load_bearing_in_two_cuts() {
        let acquire_end = Instant::now();
        let base_offsets = [4_500u64, 5_000, 5_500, 6_000, 6_500, 7_000, 7_500];
        let present_return = acquire_end + std::time::Duration::from_micros(8_000);
        let base = finish_phase_micros(
            acquire_end,
            &finish_phases_at(acquire_end, base_offsets),
            present_return,
        );

        for moved in 0..7 {
            let mut offsets = base_offsets;
            offsets[moved] += 100;
            let cuts = finish_phase_micros(
                acquire_end,
                &finish_phases_at(acquire_end, offsets),
                present_return,
            );
            let changed: Vec<usize> = (0..8).filter(|&i| cuts[i] != base[i]).collect();
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

    /// A candidate frame whose six segments sum to `service` and whose nine
    /// `ui` cuts sum to `segments[2]`, so a test can state a frame as one
    /// number and still have both decompositions telescope.
    ///
    /// The nine are spread rather than parked on one cut on purpose: a fixture
    /// of `[0, 0, 0, 0, ui, 0, 0, 0, 0]` telescopes under a formatter that
    /// printed `segments[2]` in the `ui_stack=` slot, so it could not tell the
    /// two apart. `the_worst_frames_ui_cuts_telescope_to_its_ui` keeps that
    /// degenerate shape as its own explicit green arm instead.
    fn frame(service: u32, interact: bool) -> WorstFrame {
        let sixth = service / 6;
        let mut segments = [sixth; 6];
        segments[5] = service - sixth * 5;
        let ninth = segments[2] / 9;
        let mut ui_cuts = [ninth; 9];
        ui_cuts[8] = segments[2] - ninth * 8;
        let seventh = segments[0] / 7;
        let mut pre_cuts = [seventh; 7];
        pre_cuts[6] = segments[0] - seventh * 6;
        WorstFrame {
            service,
            segments,
            ui_cuts,
            pre_cuts,
            interact,
        }
    }

    /// The latch keeps the largest service, whatever order the frames arrive
    /// in — a maximum, not a last-writer-wins.
    #[test]
    fn the_worst_frame_latch_keeps_the_largest_service() {
        let a = frame(1_200, true);
        // Not 6_400: `geodesy_one_definition` reads a bare 6_400 in this
        // tree as an earth radius in kilometres. 5_657 us is a real bin edge
        // here and cannot be mistaken for one.
        let b = frame(5_657, false);
        let c = frame(900, true);
        let ascending = [a, b, c]
            .into_iter()
            .fold(None, |standing, next| Some(latch_worst(standing, next)));
        let descending = [c, b, a]
            .into_iter()
            .fold(None, |standing, next| Some(latch_worst(standing, next)));
        assert_eq!(ascending, Some(b));
        assert_eq!(descending, Some(b));
    }

    /// **An idle frame can be the worst frame, and this is the whole point.**
    ///
    /// Every segment histogram and all nineteen cuts in this file record
    /// interact frames only, because they exist to say where an *interact*
    /// frame's service went. A click's consequences — the raster dispatch it
    /// causes, the texture its answer uploads, the source it releases — are
    /// paid on the frames after the one carrying the pointer event, and this
    /// ledger files every one of those as idle. Measured on scene D
    /// (main@3d5e1559, 1920x1080 on 3440x1440@174.96): a two-loop window's
    /// worst interact frame is in the 2.83-3.36 ms bin and its worst idle
    /// frame is in 5.66-6.73 ms. A latch that inherited the segments'
    /// interact-only rule would report the smaller of those two as the
    /// window's worst frame and read green while the user waited.
    ///
    /// So this is the non-vacuity gate: it is RED against the degenerate
    /// implementation — the latch moved inside `finalize`'s `if interacted`
    /// arm — which is exactly the shape this instrument would have taken if
    /// written to match its neighbours.
    #[test]
    fn the_worst_frame_latch_admits_an_idle_frame() {
        let clicked = frame(3_364, true);
        let paid_for_the_click = frame(6_728, false);
        let worst = latch_worst(Some(clicked), paid_for_the_click);
        assert_eq!(
            worst, paid_for_the_click,
            "the latch refused an idle frame that cost twice the interact \
             frame beside it, which is where scene D's worst frame lives",
        );
        assert!(
            !worst.interact,
            "the family column must say idle, or a reader cannot tell that \
             the worst frame was not the one that carried the click",
        );
    }

    /// The first frame to reach a service keeps the slot: ties do not churn
    /// the reading, and the frame the line describes is the earlier one in
    /// the log.
    #[test]
    fn the_worst_frame_latch_holds_the_first_of_a_tie() {
        let first = frame(4_000, true);
        let second = WorstFrame {
            interact: false,
            ..frame(4_000, false)
        };
        assert_eq!(latch_worst(Some(first), second), first);
    }

    /// **The session maximum is not cleared by the take.**
    ///
    /// The windowed figure beside it exists to be bracketed; this one exists
    /// to survive a scrape that lost the bracket. A browser console ring
    /// holds 1200 entries and the rig reads the last 60, so the tick a rare
    /// bad frame was reported in can be evicted before anything reads it —
    /// and an evicted line is indistinguishable from a run in which the bad
    /// frame never happened. Clearing this field in `take_worst` would
    /// reintroduce exactly that false negative, so the take is held to
    /// touching only the windowed slot.
    #[test]
    fn the_session_maximum_survives_a_take() {
        let body = include_str!("frame_ledger.rs")
            .split_once("pub(crate) fn take_worst(")
            .expect("take_worst is no longer a method here")
            .1
            .split_once("\n    }")
            .expect("take_worst has no recognisable body")
            .0;
        assert!(
            !body.contains("worst_since_boot"),
            "take_worst touches the session maximum, so a console ring that \
             dropped the bad tick would read as a run with no bad frame",
        );
    }

    /// The latched frame's six segments telescope to its own service, so
    /// `frame worst`'s figures are a decomposition of the frame it names
    /// rather than six numbers standing beside a seventh.
    #[test]
    fn the_worst_frames_segments_telescope_to_its_service() {
        let w = frame(6_728, false);
        assert_eq!(
            service_micros(false, 0, w.segments, 0),
            w.service,
            "the worst frame's segments do not sum to the service it was \
             latched on, so the line would decompose a different frame",
        );
    }

    /// The gap a decomposition of truncated cuts leaves against its own
    /// truncated parent, and the bound the number of cuts puts on it.
    ///
    /// Every cut is a [`micros`] call and so is the parent, and `micros`
    /// **truncates down** to whole microseconds. Write each cut's true length
    /// as `floor + frac`: the cuts throw away `sum(frac)` between them, and
    /// the parent throws away `frac(sum(frac))`, so what is left over is
    ///
    /// ```text
    /// parent - sum(cuts) == floor(sum of the cuts' fractional parts)
    /// ```
    ///
    /// Each `frac` is in `[0, 1)`, so `n` of them sum to strictly less than
    /// `n` and the gap is in `0 ..= n - 1`: never negative — the cuts can
    /// never claim more than the parent — and never `n`. **The bound is a
    /// contradiction off the number of cuts, not a tolerance chosen to fit an
    /// observation**: a tenth cut would raise it by exactly one, and a split
    /// that widened without widening this bound would be caught by it.
    fn assert_telescopes_within_truncation(cuts: &[u32], parent: u32, what: &str) -> u32 {
        let sum = cuts.iter().fold(0u32, |sum, &cut| sum.saturating_add(cut));
        assert!(
            sum <= parent,
            "{what}: the cuts sum to {sum} us against a parent of {parent} us, \
             so the decomposition claims time the span it decomposes did not \
             have. Truncation can only make cuts SMALLER than their parent; a \
             sum above it is an overlap or a cut taken from the wrong pair of \
             stamps: {cuts:?}",
        );
        let gap = parent - sum;
        assert!(
            gap < cuts.len() as u32,
            "{what}: the {} cuts fall {gap} us short of their {parent} us \
             parent, and {} truncating micros() calls can lose at most {} us \
             between them. A gap at or above the cut count is a cut that is \
             missing, zeroed or measuring the wrong span: {cuts:?}",
            cuts.len(),
            cuts.len(),
            cuts.len() - 1,
        );
        gap
    }

    /// **The latched frame's nine `ui` cuts telescope to its own `ui`
    /// segment**, so `frame worst`'s `ui_*` figures decompose the very frame
    /// the line names rather than standing beside it.
    ///
    /// This is the property the whole field exists for. Before it, the only
    /// way to say which `ui` cut owned a spike was to observe that
    /// `frame ui (stack)`'s cumulative maximum and `frame segment (ui)`'s
    /// cumulative maximum landed in adjacent bins — two aggregates that share
    /// no frame, over a denominator that excludes every idle frame. The
    /// assertions below are arithmetic on one frame instead.
    ///
    /// # Telescoping is not equality, and the fixture used to hide that
    ///
    /// [`ui_phase_micros`] makes **nine independent truncating `micros`
    /// calls** where the parent makes one, so on a real frame the nine sum to
    /// **1–6 µs short** of the `ui` the same frame reports — mean 3.39 µs,
    /// and exact on **0 of 588** frames measured. The gate that stood here
    /// asserted exact equality and passed anyway, because its hand-built
    /// fixture placed every stamp at a whole-microsecond offset and so had no
    /// fractions to lose. **A test that builds its input by hand never
    /// exercises the constructor**: with the fixture below rounding to
    /// nearest instead of truncating down — a real defect in the sub-
    /// microsecond behaviour of `ui_phase_micros` — all 981 tests of this
    /// crate's lib suite stayed green.
    ///
    /// So the claim is stated where each half of it is true: **exact** on
    /// whole-microsecond stamps, **within [`assert_telescopes_within_truncation`]'s
    /// derived bound** on stamps a clock actually produces, and **never over
    /// the parent** on either.
    #[test]
    fn the_worst_frames_ui_cuts_telescope_to_its_ui() {
        // ── Arm 1: whole-microsecond stamps, where the exactness IS true ──
        //
        // One frame's stamps, stated as offsets from `ui_start`, and its `ui`
        // segment taken from the SAME pair of instants the ledger takes it
        // from — so the two figures being compared are one frame's, which is
        // the whole claim. The exact array is what catches a cut computed
        // from the wrong pair of stamps, which no sum can see.
        let ui_start = Instant::now();
        let phases = ui_phases_at(
            ui_start,
            [300, 1_900, 9_000, 12_000, 24_100, 24_600, 39_400, 39_450],
        );
        let ui_end = ui_start + std::time::Duration::from_micros(41_000);
        let w = WorstFrame {
            service: 60_000,
            segments: [1_000, 2_000, micros(ui_start, ui_end), 8_000, 4_000, 4_000],
            ui_cuts: ui_phase_micros(ui_start, &phases, ui_end),
            pre_cuts: [100, 300, 200, 250, 50, 50, 50],
            interact: false,
        };
        assert_eq!(
            assert_telescopes_within_truncation(&w.ui_cuts, w.segments[2], "whole-us stamps"),
            0,
            "nine stamps with nothing below a microsecond on them still lost \
             time, so the nine are not contiguous cuts of one span",
        );
        assert_eq!(
            w.ui_cuts,
            [300, 1_600, 7_100, 3_000, 12_100, 500, 14_800, 50, 1_550],
            "a cut moved: the nine no longer bracket the phases they are named \
             for, so `frame worst`'s ui_* columns name the wrong spans",
        );
        assert_eq!(w.ui_cuts.iter().sum::<u32>(), 41_000);

        // ── Arm 2: the same nine spans with half a microsecond of dust on
        // each, which is what a clock hands the constructor ──
        //
        // Deterministic, and chosen so the loss is provably NOT zero: nine
        // fractions of 0.5 sum to 4.5, so the nine cuts fall exactly 4 us
        // short of a parent that is itself truncated. That 4 is inside the
        // 1–6 measured in the field and is what the old fixture could not
        // produce.
        let dusty_start = Instant::now();
        let mut at = 0u64;
        let mut ns = [0u64; 8];
        for (slot, len) in [
            300_500u64, 1_600_500, 7_100_500, 3_000_500, 12_100_500, 500_500, 14_800_500, 50_500,
        ]
        .into_iter()
        .enumerate()
        {
            at += len;
            ns[slot] = at;
        }
        let dusty = UiPhaseStamps {
            polled: dusty_start + std::time::Duration::from_nanos(ns[0]),
            laid_out: dusty_start + std::time::Duration::from_nanos(ns[1]),
            topbar: dusty_start + std::time::Duration::from_nanos(ns[2]),
            statusbar: dusty_start + std::time::Duration::from_nanos(ns[3]),
            shell: dusty_start + std::time::Duration::from_nanos(ns[4]),
            dialog: dusty_start + std::time::Duration::from_nanos(ns[5]),
            panes: dusty_start + std::time::Duration::from_nanos(ns[6]),
            applied: dusty_start + std::time::Duration::from_nanos(ns[7]),
        };
        let dusty_end = dusty_start + std::time::Duration::from_nanos(at + 1_549_500);
        let dusty_cuts = ui_phase_micros(dusty_start, &dusty, dusty_end);
        let dusty_gap = assert_telescopes_within_truncation(
            &dusty_cuts,
            micros(dusty_start, dusty_end),
            "sub-microsecond stamps",
        );
        assert_eq!(
            dusty_gap, 4,
            "nine cuts each half a microsecond long in their fraction did not \
             lose the 4 us that truncating nine of them must lose, so this arm \
             is not exercising the truncation it exists for: {dusty_cuts:?}",
        );
        assert_eq!(
            dusty_cuts,
            [300, 1_600, 7_100, 3_000, 12_100, 500, 14_800, 50, 1_549],
            "the same nine spans as arm 1, each truncated: a cut that did not \
             lose its own fraction is not the arithmetic the field sees",
        );

        // ── Arm 3: instants a clock actually produced, no offsets at all ──
        //
        // The stamps are nine bare `Instant::now()` reads in the order the
        // struct declares them, which is the order `Gui::ui` takes them in and
        // the only input shape the field ever hands this function. Their
        // spacing is whatever the machine gives — tens to hundreds of
        // nanoseconds — so the fractions are arbitrary rather than chosen.
        //
        // **The bound is asserted; a non-zero gap is not.** On a fast box
        // every cut can truncate to zero and the gap with it, and a test that
        // demanded truncation from a real clock would be asserting the clock
        // rather than the property. Arm 2 carries the non-vacuity; this arm
        // carries the input.
        let mut samples = 0;
        for _ in 0..256 {
            let live_start = Instant::now();
            let live = UiPhaseStamps {
                polled: Instant::now(),
                laid_out: Instant::now(),
                topbar: Instant::now(),
                statusbar: Instant::now(),
                shell: Instant::now(),
                dialog: Instant::now(),
                panes: Instant::now(),
                applied: Instant::now(),
            };
            let live_end = Instant::now();
            assert_telescopes_within_truncation(
                &ui_phase_micros(live_start, &live, live_end),
                micros(live_start, live_end),
                "instants from the clock",
            );
            samples += 1;
        }
        assert_eq!(
            samples, 256,
            "the real-clock arm did not take the samples it claims to have \
             taken, so its greenness is an absence and not a reading",
        );

        // **The green arm, beside the red one.** A frame whose `ui` really is
        // all in one cut — every other cut genuinely zero — is a healthy input
        // that RESEMBLES the degenerate failure, and the gate must not fire on
        // it. Over-firing here would block every real single-cut frame.
        let single = WorstFrame {
            ui_cuts: [0, 0, 0, 0, 41_000, 0, 0, 0, 0],
            ..w
        };
        assert_eq!(
            assert_telescopes_within_truncation(
                &single.ui_cuts,
                single.segments[2],
                "a genuinely single-cut frame",
            ),
            0,
            "a frame whose ui was genuinely spent in one cut fails the \
             telescoping gate, so the gate over-fires on healthy input",
        );
    }

    /// **The latched frame's seven `pre` cuts telescope to its own `pre`
    /// segment**, so `frame worst`'s `pre_*` figures decompose the very frame
    /// the line names rather than standing beside it.
    ///
    /// The property the field exists for, and it is not the same property
    /// `frame pre (*)` has: those seven histograms record inside `finalize`'s
    /// `if interacted` arm, and the frame this split was cut for is a latched
    /// one — `pre` read **10,357 µs on it, 88–92 % of the whole frame**.
    /// Half of scene D's expensive frames carry no pointer event and are filed
    /// idle, so an interact-only histogram cannot open the very frames the
    /// `max` verdict is about.
    ///
    /// Written on `the_worst_frames_ui_cuts_telescope_to_its_ui`'s corrected
    /// pattern from the start: exact where the stamps are whole microseconds,
    /// within the derived bound where they are not, never over the parent on
    /// either, and a green arm beside the red ones.
    #[test]
    fn the_worst_frames_pre_cuts_telescope_to_its_pre() {
        let start = Instant::now();
        let phases = pre_phases_at(start, [4, 94, 124, 184, 186, 195]);
        let setup = start + std::time::Duration::from_micros(207);
        let w = WorstFrame {
            service: 11_248,
            segments: [micros(start, setup), 2_000, 4_000, 3_000, 1_000, 1_041],
            ui_cuts: [0, 0, 0, 0, 4_000, 0, 0, 0, 0],
            pre_cuts: pre_phase_micros(start, &phases, setup),
            interact: false,
        };
        assert_eq!(
            assert_telescopes_within_truncation(&w.pre_cuts, w.segments[0], "whole-us stamps"),
            0,
            "the worst frame's seven pre cuts do not sum to its own pre \
             segment, so the line's pre_* figures decompose some other frame \
             and the attribution they exist to make is an inference again",
        );
        assert_eq!(
            w.pre_cuts,
            [4, 90, 30, 60, 2, 9, 12],
            "a cut moved: the seven no longer bracket the phases they are \
             named for, so `frame worst`'s pre_* columns name the wrong spans",
        );

        // The same frame with sub-microsecond stamps, which is the only shape
        // the field produces: the seven fall inside the derived bound and
        // never over the parent.
        let dusty_start = Instant::now();
        let dusty = PrePhaseStamps {
            polled: dusty_start + std::time::Duration::from_nanos(4_500),
            ingested: dusty_start + std::time::Duration::from_nanos(95_000),
            evicted: dusty_start + std::time::Duration::from_nanos(125_500),
            dropped: dusty_start + std::time::Duration::from_nanos(186_000),
            saved: dusty_start + std::time::Duration::from_nanos(188_500),
            gated: dusty_start + std::time::Duration::from_nanos(198_000),
        };
        let dusty_setup = dusty_start + std::time::Duration::from_nanos(210_500);
        let dusty_frame = WorstFrame {
            segments: [
                micros(dusty_start, dusty_setup),
                2_000,
                4_000,
                3_000,
                1_000,
                1_041,
            ],
            pre_cuts: pre_phase_micros(dusty_start, &dusty, dusty_setup),
            ..w
        };
        assert_eq!(
            assert_telescopes_within_truncation(
                &dusty_frame.pre_cuts,
                dusty_frame.segments[0],
                "sub-microsecond stamps",
            ),
            3,
            "the seven cuts of a frame whose stamps carry fractions did not \
             lose the 3 us seven truncating micros() calls must lose, so this \
             arm is not exercising the truncation it exists for",
        );

        // **The green arm, beside the red one.** A frame whose `pre` really
        // is all in one cut — an autosave that fired, every other cut
        // genuinely zero — is a healthy input that RESEMBLES the degenerate
        // failure, and the gate must not fire on it.
        let single = WorstFrame {
            segments: [8_400, 2_000, 4_000, 3_000, 1_000, 1_041],
            pre_cuts: [0, 0, 0, 0, 8_400, 0, 0],
            ..w
        };
        assert_eq!(
            assert_telescopes_within_truncation(
                &single.pre_cuts,
                single.segments[0],
                "a genuinely single-cut frame",
            ),
            0,
            "a frame whose pre was genuinely spent in one cut fails the \
             telescoping gate, so the gate over-fires on healthy input",
        );
    }

    /// **The seven cuts are computed for EVERY presented frame, not only the
    /// interact ones.** Held against `finalize`'s own source, on
    /// [`the_worst_frames_ui_cuts_are_computed_outside_the_interact_arm`]'s
    /// terms: the binding must appear before the `if interacted {` that opens
    /// the arm, and the statement itself may not read the flag.
    ///
    /// The degenerate this is red against is the natural one — leaving the
    /// `pre_phase_micros` call where its seven `record` calls are. That shape
    /// compiles, telescopes on the frames it does fill, and reports seven
    /// zeros on exactly the frames the field was added to describe: the
    /// latched ones, where `pre` was measured at 88–92 % of a whole frame.
    #[test]
    fn the_worst_frames_pre_cuts_are_computed_outside_the_interact_arm() {
        let body = include_str!("frame_ledger.rs")
            .split_once("pub(crate) fn finalize(")
            .expect("finalize is no longer a method here")
            .1;
        let bound = body
            .find("let pre_cuts = ")
            .expect("finalize no longer binds the worst frame's seven pre cuts");
        let interact_arm = body
            .find("if interacted {")
            .expect("finalize no longer splits on the interact flag");
        assert!(
            bound < interact_arm,
            "the seven pre cuts are computed inside finalize's interact arm, \
             so every frame that PAYS for a click -- all of which are filed \
             idle -- would carry seven zeros on the one line that reports it",
        );
        let statement = body[bound..]
            .split_once("\n\n")
            .expect("the pre_cuts binding is no longer a statement of its own")
            .0;
        assert!(
            !statement.contains("interacted"),
            "the pre cuts binding reads the interact flag, so an idle frame \
             would carry seven zeros however early the binding sits: \
             {statement:?}",
        );
    }

    /// **The nine cuts are computed for EVERY presented frame, not only the
    /// interact ones.** Held against `finalize`'s own source, on
    /// [`the_worst_frame_latch_is_outside_the_interact_arm`]'s terms: the
    /// binding must appear before the `if interacted {` that opens the arm.
    ///
    /// The degenerate this is red against is the natural one — leaving the
    /// `ui_phase_micros` call where its nine `record` calls are and reading
    /// zeros on every idle frame. That shape compiles, telescopes on the
    /// frames it does fill, and reports nine zeros on exactly the frames this
    /// field was added to describe: the ones that pay for a click.
    #[test]
    fn the_worst_frames_ui_cuts_are_computed_outside_the_interact_arm() {
        let body = include_str!("frame_ledger.rs")
            .split_once("pub(crate) fn finalize(")
            .expect("finalize is no longer a method here")
            .1;
        let bound = body
            .find("let ui_cuts = ")
            .expect("finalize no longer binds the worst frame's nine ui cuts");
        let interact_arm = body
            .find("if interacted {")
            .expect("finalize no longer splits on the interact flag");
        assert!(
            bound < interact_arm,
            "the nine ui cuts are computed inside finalize's interact arm, so \
             every frame that PAYS for a click -- all of which are filed idle, \
             and where half of scene D's spikes live -- would carry nine zeros \
             on the one line that reports it",
        );
        // **The position alone is not enough**, and this is the half that
        // catches the degenerate a position check cannot see: a binding
        // spelled `let ui_cuts = if interacted { … } else { [0; 9] };` sits
        // before the arm, compiles, telescopes on every frame it fills, and
        // reports nine zeros on exactly the frames the field was added for.
        // So the statement itself may not read the flag.
        let statement = body[bound..]
            .split_once("\n\n")
            .expect("the ui_cuts binding is no longer a statement of its own")
            .0;
        assert!(
            !statement.contains("interacted"),
            "the ui cuts binding reads the interact flag, so an idle frame \
             would carry nine zeros however early the binding sits: \
             {statement:?}",
        );
    }

    /// **The latch is offered every presented frame, not only interact
    /// ones.** `latch_worst` cannot see where it is called from, so the
    /// eligibility rule is held here, against `finalize`'s own source: the
    /// call must come after the `} else {` that closes the interact arm.
    #[test]
    fn the_worst_frame_latch_is_outside_the_interact_arm() {
        let body = include_str!("frame_ledger.rs")
            .split_once("pub(crate) fn finalize(")
            .expect("finalize is no longer a method here")
            .1;
        let interact_arm = body
            .find("if interacted {")
            .expect("finalize no longer splits on the interact flag");
        let idle_arm = body[interact_arm..]
            .find("} else {")
            .map(|at| interact_arm + at)
            .expect("finalize no longer has an idle arm");
        let arm_end = body[idle_arm..]
            .find("\n        }")
            .map(|at| idle_arm + at)
            .expect("the interact/idle block has no recognisable end");
        let latch = body
            .find("latch_worst(")
            .expect("finalize no longer latches a worst frame");
        assert!(
            latch > arm_end,
            "the worst-frame latch sits inside finalize's interact/idle \
             block, so the frames that pay for a click -- every one of which \
             is filed idle -- cannot be the frame it reports",
        );
    }

    /// A dispatch accumulation stated in nanoseconds, one field at a time.
    fn cuts(
        dedupe: u64,
        marks: u64,
        hydrate: u64,
        prepare: u64,
        hitmap: u64,
        offload: u64,
    ) -> DispatchCuts {
        DispatchCuts {
            dedupe_ns: dedupe,
            marks_ns: marks,
            hydrate_ns: hydrate,
            prepare_ns: prepare,
            hitmap_ns: hitmap,
            offload_ns: offload,
        }
    }

    /// **The seven telescope to `dispatch`.** The residual is defined as the
    /// parent minus the six named, so this cannot be an approximate equality
    /// and no reading of the split may need one: any dispatch time the six do
    /// not name is *in* the seventh figure, not missing from the report.
    #[test]
    fn the_dispatch_cuts_telescope_to_dispatch() {
        let dispatch = 9_000;
        let seven = dispatch_cut_micros(
            cuts(1_500_000, 40_000, 300_000, 5_200_000, 900_000, 60_000),
            dispatch,
        );
        assert_eq!(
            seven.iter().copied().fold(0u32, u32::wrapping_add),
            dispatch,
            "the seven cuts of `dispatch` do not sum to it: {seven:?}",
        );
        // And the residual is the one absorbing the difference, not a cut.
        assert_eq!(seven[..6], [1_500, 40, 300, 5_200, 900, 60]);
        assert_eq!(seven[6], 1_000);
    }

    /// **Sub-microsecond work is not rounded away.** The six accumulate in
    /// nanoseconds precisely so that many cheap requests add up to a figure
    /// instead of to nothing: per-request truncation to whole microseconds
    /// would have reported eight 900 ns visits as 0 µs and handed the whole
    /// 7.2 µs to the residual, which is read as "time the six do not name".
    #[test]
    fn many_sub_microsecond_visits_survive_into_the_cut_that_earned_them() {
        let mut accumulated = DispatchCuts::default();
        for _ in 0..8 {
            accumulated.add(cuts(0, 0, 0, 0, 900, 0));
        }
        let seven = dispatch_cut_micros(accumulated, 20);
        assert_eq!(seven[4], 7, "hitmap lost its sub-microsecond visits");
        assert_eq!(seven[6], 13, "the residual absorbed them instead");
    }

    /// **A span the cuts overrun reports no residual, and does not panic.**
    /// The parent is two clock reads and the six are twelve; a coarse or
    /// backward-stepping web clock can order them wrongly, and a frame-thread
    /// panic in an always-on instrument is a worse outcome than a zero.
    #[test]
    fn cuts_that_overrun_their_span_report_a_zero_residual() {
        let seven = dispatch_cut_micros(cuts(0, 0, 0, 8_000_000, 0, 0), 3_000);
        assert_eq!(seven[3], 8_000);
        assert_eq!(
            seven[6], 0,
            "the residual went negative rather than to zero"
        );
    }

    /// **An empty accumulation is an absence, not seven zeros.** Most frames
    /// dispatch nothing at all; recording zeros for them would put thousands
    /// of floor samples in seven histograms whose `n` is meant to *be* the
    /// count of dispatching frames.
    #[test]
    fn a_frame_that_dispatched_nothing_offers_no_dispatch_sample() {
        let mut ledger = super::FrameLedger::default();
        ledger.record_dispatch_cuts(DispatchCuts::default());
        assert!(
            ledger.cur.dispatch.is_none(),
            "an empty dispatch was filed as a sample",
        );
        ledger.record_dispatch_cuts(cuts(0, 0, 0, 1, 0, 0));
        assert!(
            ledger.cur.dispatch.is_some(),
            "a dispatch that spent a nanosecond was filed as an absence",
        );
    }

    /// **The accumulator is cleared by the take, not merely read.** That is
    /// what keeps an arrival-path dispatch — which reaches the same function
    /// earlier in the frame — out of the `post` tail's figure.
    #[test]
    fn taking_the_accumulator_clears_it() {
        let mut ledger = super::FrameLedger::default();
        ledger.add_dispatch_cuts(cuts(0, 0, 0, 4_000, 0, 0));
        assert_eq!(ledger.take_dispatch_cuts(), cuts(0, 0, 0, 4_000, 0, 0));
        assert_eq!(
            ledger.take_dispatch_cuts(),
            DispatchCuts::default(),
            "a second take saw the first take's figure again",
        );
    }
}
