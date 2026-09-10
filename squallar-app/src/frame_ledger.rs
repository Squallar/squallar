//! What each frame cost this thread, recorded where the frame happens.
//!
//! `handle_redraw` and the two frame calls it makes stamp a handful of
//! instants per frame; `finalize` folds them into fixed-shape histograms
//! ([`squallar_device_profile::hist::Hist`]) once the frame's outcome is
//! known. **Product telemetry, not a campaign instrument**: always on, no
//! feature gate, and the per-frame cost is **sixty-three clock reads on a
//! one-pane plan-view frame, seventy-one integer bin searches** and two
//! `u32` comparisons.
//!
//! The clock reads, counted where they are taken: the ledger's own **eight**
//! stamps (its six `mark_*`/`finalize` reads plus the pair the acquire
//! closure hands back), the **six** `handle_redraw` takes across its head,
//! the **seven** `setup_egui_frame` takes across `pump`, the
//! **twenty-four** `Gui::ui` takes on its way through (six in `ui_phased`,
//! two inside `render_shell_phased`, six more inside
//! `render_stack_and_inspector`, ten inside `render_panes`), the
//! **five** the egui pass takes, the **six** `handle_redraw`'s tail takes
//! (five in the tail, one inside `process_gui_actions`) and the **seven**
//! `present_frame` takes after the acquire returns.
//!
//! **Sixty-three is the frame that reaches the layer panel; a frame that does
//! not costs fifty-nine.** `render_stack_and_inspector` has three early
//! returns — a compact width, a faded chrome, both slide factors at zero —
//! and every one of them fills its remaining five stamps from ONE clock read
//! (`shell_api::StackStamps::skipped_after`). So the [`StackHists`] split is
//! six reads at most and two at least, and never a read per skipped region.
//!
//! **Ten of the twenty-four are `render_panes`', and they are the one count
//! in this block that moves with the SCENE.** [`PanesHists`]' seven cuts are
//! nanosecond sums across a pane loop, so that call takes three reads before
//! the loop, one after it, and **six per plan-view pane** — a pane that is
//! not a plan view runs no `walkers::Map` widget of its own and takes three.
//! Sixty-three is therefore the ONE-pane plan-view frame: a second plan-view
//! pane makes it sixty-nine, and a one-pane volume frame costs sixty. Count
//! the panes before quoting the figure.
//!
//! **Two neighbouring instruments are NOT in the fifty-three, and naming them
//! is what stops the next recount oscillating.** `EguiRenderer` takes a sixth
//! unconditional read for `PassCosts::note` (and two more when a pane mirror
//! is requested), and `handle_redraw`'s tail takes up to two more building
//! `auto_poll_at` and `egui_repaint_at` — deadlines, not stamps, and
//! conditional. None of the five feeds a figure in this file. The count above
//! is **this ledger's** reads on the unconditional path.
//!
//! The bin searches, on a presented interact frame: ten outside the splits
//! (one service, one service-less-present, six segments, one acquire, one
//! cadence) and sixty-one in them — **seven** the `pre` split ([`PreHists`]), **eight** the `pump`
//! split ([`PumpHists`]), **nine** the `ui` split ([`UiHists`]), **seven**
//! the `stack` split ([`StackHists`], one level below the `ui` nine and never
//! added to them), **eight** the `panes` split ([`PanesHists`], the `stack`
//! split's sibling one level below a different `ui` cut, and never added to
//! those nine either), **six** the
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
//! itself was six of the then forty-seven and seven of the then fifty-five;
//! the other fifteen and eleven were drift. Recount here rather than adjust,
//! or the next reader inherits the same arithmetic. **Every figure in this
//! block is a recount off the current source** — the `stack` split's six
//! reads and seven searches were added by recounting all seven contributors,
//! not by adding six and seven to what stood here.
//!
//! **`service less present` is one bin search of the ten and nothing else.**
//! It is `service` with the `finish` split's eighth cut taken back out —
//! arithmetic on two figures this file already has — and it records on
//! exactly one of its two families per frame. So it left the clock reads
//! unchanged and took the bin searches from fifty-five to fifty-six. See
//! [`service_less_present_micros`].
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
//!   **`service less present` is this denominator, not a fourth one**: the
//!   same figure with the `finish` split's eighth cut taken back out, filed
//!   on the same two families from the same frames. It is a different
//!   FIGURE over the same frames, and the responsiveness bar is still
//!   stated in `service` — see [`service_less_present_micros`].
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
///
/// # What each cut scales with, and why the shares are not a constant
///
/// **The share of a cut is a property of the SCENE, not of this segment.**
/// Every campaign frame figure through 2026-09-07 was read on a ONE-PANE
/// scene, and the shape the campaign carried from them — "`tessellate` is
/// `prepare`'s biggest named cut" — does not survive a second pane. Measured
/// on two 420 s gesture legs that differ in `pane_count` and in nothing else
/// (same eighteen layers, same sites, same binary, same display; interact
/// frames, `sum=` over `n`):
///
/// ```text
///                 1 pane            6 panes
///   n              4267               4126
///   prepare      847 us/fr        3201 us/fr     (x3.78)
///   plan           0.1%   1.09      0.0%    1.30 (x1.19)  per frame
///   end-pass       6.7%  56.33      1.7%   54.17 (x0.96)  per shape + frame
///   tessellate    25.8% 218.64     10.0%  318.90 (x1.46)  per shape/vertex
///   upload        56.6% 479.00     85.2% 2728.83 (x5.70)  per byte
///   mirror         0.0%   0.02      0.0%    0.14          per 3D pane
///   buffers       10.5%  89.10      3.0%   95.10 (x1.07)  per vertex/index
///   residual      0.29%   2.41     0.08%    2.62 (x1.09)  per frame
/// ```
///
/// Two things in that table are load-bearing for anyone reading a share here.
///
/// **The residual is a fixed number of MICROSECONDS, not a fixed share.** It
/// is six truncating [`micros`] calls against one truncating parent and
/// nothing else, so its expectation is `6(0.5) - 0.5 = 2.5` us per frame
/// whatever the scene costs — measured 2.41 and 2.62 across a 3.78x change in
/// the parent. A residual that ever GREW with pane count would mean real work
/// no cut names; this one does not.
///
/// **Panes are not a multiplier.** Six panes share one window, so each pane is
/// smaller and draws less: the shape population went 873 to 1925 per pass
/// (x2.2), not x6, and `tessellate` followed it sub-linearly. What did not
/// follow is `upload`, which grew x5.70 on only 5% more bytes — a routing
/// change, not a volume change. See
/// [`squallar_gpu::egui_renderer::texture_upload`]: a delta crosses whole on
/// the frame thread when it is *small enough*, so six smaller panes push their
/// rasters under the threshold and onto the blocking route that one large pane
/// stayed above. Measured over the same two legs: 2.5 MB of 59.2 GB blocking
/// on one pane, 35.8 GB of 62.3 GB on six.
///
/// The threshold those figures were read against was 8 MiB, and the frame's
/// whole allowance was 16 MiB — `band_cap` times a count of staging buffers,
/// which is **7.6 ms** of blocking at the 2.1 GB/s that module measures, against
/// a [`squallar_device_profile::constants::TARGET_FRAME_SERVICE`] of 4 ms. Both
/// are now one figure derived from that frame and that bandwidth
/// (`texture_upload::whole_budget`), so the six-pane `upload` reading above is
/// a reading of the OLD routing and is kept as the before half of the pair.
///
/// **The table is one box's arms, and `upload`'s share does not port.** Both
/// columns are Linux legs; on Metal the same segment is
/// **tessellate-dominated**. Scene C, Firefox: `prepare`'s `tessellate` is
/// **1,513 us — 51 %**, and `upload` is **33 us — 1.1 %**, against the
/// 85.2 % above. That is not a small difference in a share, it is two orders
/// of magnitude in the cut's absolute cost, and it inverts which of the six
/// is worth opening. A reader prioritising `prepare` off the six-pane column
/// alone would optimise the wrong cut on Apple hardware. Read the arm your
/// target runs on; these are never merged.
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
    /// sub-microsecond cost of finding that out — **plus the
    /// `staged_geometry` walk**, which is inside this span and not the next
    /// one. That placement is deliberate and stated where it is made
    /// (`EguiRenderer::end_pass_and_upload`): the walk is the instrument for
    /// the `buffers` phase, so counting it there would inflate the very
    /// microseconds its byte total is divided by. It is named here so a
    /// reader who finds this cut non-zero on a scene with no 3D pane looks
    /// for a per-primitive walk rather than for a mirror that is not there.
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
/// equality is the whole design. The **nine** are contiguous cuts of the one
/// span, so they telescope to it (`the_ui_phases_telescope_to_ui`), which
/// makes the residual arithmetic rather than inference: any `ui` time these
/// nine do not name is a bug in this decomposition, not a mystery.
///
/// The count read "six" from before the split widened, twice. Recounted off
/// the fields rather than adjusted, on this module's own rule.
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
    /// `render_stack_and_inspector` — the remainder of the shell, and the
    /// owner of this segment's tail: p50 421–500 µs against a max of
    /// 8,000–9,514 µs on the measured legs, 16–20x its own median. Opened up
    /// one level further by [`StackHists`], whose seven cuts telescope to
    /// exactly this one.
    pub(crate) stack: Hist,
    /// The time dialog, between the shell and the panes. Its own cut because
    /// a dialog that is not open should not be charged to the map surfaces.
    pub(crate) dialog: Hist,
    /// `render_panes` — every map surface, and on a toggle frame the pane
    /// that acts on the click `shell` just read.
    ///
    /// **The time dialog is NOT in this cut.** It was, until `dialog` was cut
    /// out above; this doc went on saying otherwise for two landings, which
    /// is what `ui_phase_micros`' sixth and seventh entries settle —
    /// `dialog` closes on `phases.dialog` and this one opens on it.
    ///
    /// Opened up one level further by [`PanesHists`], whose seven named cuts
    /// and residual telescope to exactly this one.
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

/// Where the `ui` split's `stack` cut went, one level below [`UiHists`].
///
/// # Denominator
///
/// **Exactly [`UiHists::stack`]'s** — the frames on which that cut records,
/// which is presented interact frames that left `ui_phases` — and that
/// equality is the design, not a coincidence: the seven `record` calls sit in
/// the very block the ninth `ui` cut's do, so `stack.snap.total()` and
/// `ui.stack.total()` cannot differ
/// (`the_stack_family_records_on_exactly_the_frames_its_parent_does`).
///
/// A family with a NARROWER `n` than its parent would still telescope on the
/// frames it holds, and every share read off it would be arithmetic over the
/// wrong denominator. [`DispatchHists`] is allowed that only because its
/// parent cut is itself near-zero on the frames it skips; `stack` is the
/// opposite — the frames it would skip are the closed-panel ones, where the
/// whole cut is one gate.
///
/// **Never added to `frame ui (stack)`.** These are not a tenth `ui` cut
/// beside it; they *are* it, opened up. The reporting prefix is deliberately
/// `frame stack` — a third spelling, so that no reader pattern-matching
/// `frame ui` can add two levels of the same span together.
///
/// # What this split was cut to answer
///
/// `ui` is ~50 % of the frame, and `panes` + `stack` is 81–82 % of `ui`.
/// `stack` owns the tail on every Firefox leg measured (max 8,000/8,000/9,514
/// µs against `panes`' 5,657/3,364/4,000) and it is a SPIKE, not a level: p50
/// 421–500 µs, so the maximum is 16–20x the median and one frame is more than
/// twice the whole 4 ms bar. Every spike at or above 4,900 µs fell in the
/// first ~13 s and then stopped, and the same loop phase 20 s later read 959
/// µs — the shape of a cache filling rather than of per-click work. A
/// percentile over one undivided cut cannot say which of seven things that
/// is.
///
/// # Which arms those figures are from, and which they are not from
///
/// **Every number above was read on ONE arm**: a Mac at 3440x1440 and 175 Hz
/// on hardware WebGPU, scene D's ui-sweep, 40 s windows, n=108/107 interact
/// frames — **three Firefox 155 legs and one Chromium 152**. The
/// "16–20x its median" spread is the three Firefox legs; Chromium is a single
/// leg and is not what that range describes.
///
/// **Do not carry a figure across an engine.** The `overlay rasters` ledger,
/// an always-on counter in this same family, reads 20 inked of 908 pictures
/// on Chromium against 582 of 1,336 on Firefox for the same scene — 2 % and
/// 44 %. A counter can be sound on both arms and still be answering a
/// different question on each, and nothing here is licensed to describe an
/// engine it was not read on.
///
/// **No native, Safari, Android or iOS arm has read this family at all**, for
/// the plain reason that it did not exist until this landing. Those are
/// absences, not zeros. Note what that makes the four legs above: Firefox and
/// Chromium with hardware WebGPU are **browser** legs, so the Mac is the HOST
/// and every figure here is a figure about the **web target**. There is no
/// native reading of this family to compare them against.
///
/// # Attributing a frame between same-frame segments needs evidence
///
/// `stack` shares its frame with eight sibling `ui` cuts and five sibling
/// segments, and a spike in one is not evidence about any other. The
/// first-~13-s clustering above reads as a cache filling — but nothing in
/// THIS family separates that from a same-frame segment with a warm-up of its
/// own. So hold `stack` against the other cuts on the SAME leg before
/// attributing a frame to either, and in both directions: this family can no
/// more exonerate a neighbour than convict one.
///
/// **And check the leg produces the frames.** Every cut here is interact-only,
/// so a leg that takes no input records `n=0` and the seven telescope
/// perfectly over nothing — indistinguishable, on the artifact, from a
/// correct instrument. The rig's `wide` leg reads `frame service (interact)`
/// at `n=0` for exactly that reason. Read `n` before reading a share.
///
/// # There is no `residual`, and that is deliberate
///
/// `settle` closes on the parent's own right boundary, so no `stack` time can
/// hide in an unnamed tail — [`PostHists::close`]'s reasoning verbatim. What
/// a subtraction would leave is [`micros`]' truncation dust, bounded at
/// `n - 1 = 6` µs and never over; printing six microseconds of arithmetic
/// under a heading called "residual" invites exactly the misreading this
/// family exists to prevent. **This split's residual is `settle`, a named
/// span, not a subtraction.** [`DispatchHists`] carries one only because it
/// accumulates in nanoseconds across a loop and cannot bracket its own span.
#[derive(Default)]
pub(crate) struct StackHists {
    /// The two selection snaps — a pane that draws no map layers, and a layer
    /// the active pane does not hold. `O(1)`: it scales with nothing, which
    /// is what makes a non-zero reading here a finding rather than a size.
    pub(crate) snap: Hist,
    /// The compact-width gate, `chrome_fade` and the two slide animations.
    /// Scales with nothing either — **but it holds the WHOLE cut on a frame
    /// that draws no panel**, because all three of the early returns are
    /// inside it. A `stack` that is large and all in `gate` is a frame that
    /// spent its time deciding not to draw.
    pub(crate) gate: Hist,
    /// `PaneState::hydrate_layer_states`. Scales with the active pane's slot
    /// count.
    pub(crate) hydrate: Hist,
    /// `stack_row_statuses` — one status line per row. Scales with the layer
    /// count, and with the alert-set size through
    /// `NwsAlertHandler::status_line`.
    pub(crate) statuses: Hist,
    /// `render_stack`. Scales with layer count times widgets per row.
    pub(crate) render: Hist,
    /// `render_inspector`. Scales with the selected layer's control surface,
    /// so it moves with WHAT is selected rather than with how much there is.
    pub(crate) inspector: Hist,
    /// The pane restore, `propagate_pane_sync`, the `ShellPhased`
    /// construction and the return out of `render_shell_phased`. Scales with
    /// **pane count**, which makes it structurally weak on a one-pane scene —
    /// and it is this family's residual, closing on the parent's own right
    /// boundary so that nothing can hide behind it.
    pub(crate) settle: Hist,
}

/// Where the `ui` split's `panes` cut went, one level below [`UiHists`].
///
/// # Denominator
///
/// **Exactly [`UiHists::panes`]'s** — presented interact frames that left
/// `ui_phases` — and that equality is the design, not a coincidence: the
/// eight `record` calls sit in the very block the seventh `ui` cut's does, so
/// `panes.residual.total()` and `ui.panes.total()` cannot differ
/// (`the_panes_family_records_on_exactly_the_frames_its_parent_does`).
/// [`StackHists`]' denominator note applies here word for word: a family with
/// a NARROWER `n` than its parent would still telescope on the frames it
/// holds, and every share read off it would be arithmetic over the wrong
/// denominator.
///
/// **Never added to `frame ui (panes)`.** These are not a tenth `ui` cut
/// beside it; they *are* it, opened up. The reporting prefix is deliberately
/// `frame panes` — a third spelling, so that no reader pattern-matching
/// `frame ui` can add two levels of the same span together. [`StackHists`]'
/// choice exactly.
///
/// # Nanoseconds in, microseconds out — and why that forces a residual
///
/// [`StackHists`] has no `residual` because its seven cuts are bracketed by
/// instants and the last closes on the parent's own right boundary. This one
/// cannot do that: `render_panes` runs a **pane loop**, and four of the seven
/// named cuts are sums across it. So [`squallar_egui::shell_api::PanesCuts`]
/// accumulates in nanoseconds and converts once, here —
/// [`DispatchCuts`]' pattern for [`DispatchCuts`]' reason. Truncating per
/// pane instead would round every sub-microsecond piece to zero and hand the
/// difference to the residual, which is the one figure that must not absorb
/// an artifact.
///
/// The residual is then `panes` minus the seven — **arithmetic, not
/// inference**. It holds three things and nothing else: up to seven
/// microseconds of truncation dust per frame (one per named cut), the return
/// out of `render_panes` after its last charge, and any span this
/// decomposition genuinely failed to name. A residual of a few microseconds
/// per frame is the first of those; a large one is the third, and is a bug in
/// this split rather than a finding about the frame.
///
/// # What this split was cut to answer
///
/// `panes` is the largest cut of the largest segment. Measured on the Tier-2
/// browser rig against main `7dad44888`, scene `pan-zoom-2d`, 1 pane KTLX, 18
/// layers, 2878x1566 canvas, on two **software** arms that are never merged —
/// Firefox on Mesa llvmpipe and Chromium on SwiftShader — `ui` was the
/// largest segment mean and the largest segment p99 on all six legs, and
/// `ui_panes` held **81.6 %** of `ui` on Firefox (leading 76 of 80 latched
/// worst frames) and **40.1 %** on Chromium (55 of 62). One undivided cut
/// cannot say which of seven things that is.
///
/// **Those figures are from software rasterisers on a contended box** (1-min
/// loadavg 14.4–25.5), so the absolutes are inflated; the shares and the
/// leader-counts are what they support.
///
/// # The hardware prediction, and how it scored
///
/// This paragraph used to end by saying that on hardware `finish` shrinks —
/// it is 95.6 % `finish:submit` on llvmpipe — so `ui`'s share would RISE, and
/// that this was a mechanism rather than a measurement with no number behind
/// it. It has since been measured on **two independent hardware arms**, and it
/// scored.
///
/// **Three measurement arms, never merged and never averaged**: the browser
/// *software* pair the paragraph above is read on, a browser *hardware* pair,
/// and a *native* adapter pair. Each has its own denominator and its own box.
/// (Unrelated to the three *pane kinds* the `widget` section below counts.)
///
/// **Mac M2, real EDID sink 1080p60, Metal through `BrowserWebGpu`, canvas
/// 1248x714, settled window, Firefox 155.0.1.** `finish` fell from
/// 28.7–35.5 % of the frame to **5.1–5.6 %**. Over the same denominator the
/// llvmpipe figure uses — `finish:submit` against `finish:whole` — it fell
/// from 95.6 % to **16.7–20.0 %**, and on Chromium from 88 % to
/// **9.9–12.5 %**. `finish` never exceeded **439 us** in any latched worst
/// frame. `ui` rose from 42.6–52.3 % to **58.5 %** on Firefox at one pane;
/// Chromium's 49.2–50.1 % sits *inside* its own software range, so on this
/// arm the rise is clear **on the governing browser only**.
///
/// **Linux, RTX 3090, adapter-isolated native pair** — the same binary, the
/// same display, the only difference the Vulkan ICD, so llvmpipe was
/// unreachable on one leg and nothing else could account for the gap.
/// `finish` 86.0 % → **8.1 %**; `ui` 5.1 % → **48.9 %**. **8.3x on the
/// adapter alone.**
///
/// **The correction that matters, and it is a narrowing.** Read against the
/// *browser software* arms, hardware `ui` at 48.9–58.5 % OVERLAPS Firefox's
/// software 42.6–52.3 %: those two do not separate, and a reader quoting the
/// rise off the table above would be quoting an overlap. The rise is measured
/// against the **native llvmpipe control** — 5.1 % → 48.9 % on one binary,
/// one display and one ICD swap — and that is the pair to cite it from.
///
/// # `widget` is structurally absent, not small, on two of the three arms
///
/// Only a plan-view pane runs a `walkers::Map` widget. A cross-section or
/// volume pane leaves `widget` at exactly zero and files its whole arm under
/// `content`, so `widget / content` is a ratio about the SCENE before it is
/// one about the code. Read `n` and the scene's pane kinds before reading
/// that pair — [`StackHists`]' "check the leg produces the frames" note, one
/// level down.
#[derive(Default)]
pub(crate) struct PanesHists {
    /// See [`squallar_egui::shell_api::PanesCuts::setup_ns`] — the prologue
    /// before the central panel opens, tile-slot decisions included.
    pub(crate) setup: Hist,
    /// See [`squallar_egui::shell_api::PanesCuts::panel_ns`] — the panel's
    /// head, before the first pane.
    pub(crate) panel: Hist,
    /// See [`squallar_egui::shell_api::PanesCuts::resolve_ns`] — per pane,
    /// everything before its render-view arm. Scales with pane count.
    pub(crate) resolve: Hist,
    /// See [`squallar_egui::shell_api::PanesCuts::widget_ns`] — per pane, the
    /// `walkers::Map` widget's own frame around this crate's draw closure.
    /// **Zero on a pane that is not a plan view.**
    pub(crate) widget: Hist,
    /// See [`squallar_egui::shell_api::PanesCuts::content_ns`] — per pane,
    /// the draw context this crate builds and the enabled-layer walk it hands
    /// over to. The only cut here that scales with layer data.
    pub(crate) content: Hist,
    /// See [`squallar_egui::shell_api::PanesCuts::tools_ns`] — per pane, the
    /// furniture around the content. Inert unless a draw tool is armed.
    pub(crate) tools: Hist,
    /// See [`squallar_egui::shell_api::PanesCuts::credit_ns`] — the
    /// epilogue: dividers, credit, viewport sync and the tile restores.
    pub(crate) credit: Hist,
    /// `panes` minus the seven above — arithmetic, not inference. See this
    /// type's "Nanoseconds in, microseconds out" note for the three things it
    /// can hold.
    pub(crate) residual: Hist,
}

/// Where the `post` segment's time went, cut at the seams `handle_redraw`'s
/// tail has.
///
/// # Denominator
///
/// **Exactly [`SegmentHists::post`]'s** — presented interact frames — and that
/// equality is the whole design. The **seven** are contiguous cuts of the one
/// span, so they telescope to it (`the_post_phases_telescope_to_post`), which
/// makes the residual arithmetic rather than inference: any `post` time these
/// seven do not name is a bug in this decomposition, not a mystery.
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
    /// The seven `stack` cuts of THIS frame, in [`StackHists`]' order:
    /// `[snap, gate, hydrate, statuses, render, inspector, settle]` — the
    /// fifth of [`WorstFrame::ui_cuts`], opened up. They telescope to
    /// `ui_cuts[4]` **to within the six microseconds seven truncating
    /// [`micros`] calls can lose, and never over it**.
    ///
    /// **Here for [`WorstFrame::ui_cuts`]' reason exactly, and it is the
    /// reason this field is not optional.** [`StackHists`] records inside
    /// `finalize`'s `if interacted` arm, and half the `ui` spikes measured on
    /// scene D — on the **browser** legs [`StackHists`] names, which are the
    /// only arms this family has been read on — fall on IDLE frames: 8,339,
    /// 6,600, 8,219 and 7,879 µs, because the frame that PAYS for a click
    /// carries no pointer event. A
    /// split that existed only in the interact histograms would be blind to
    /// half the frames the p99 verdict is about.
    ///
    /// Zeroed on a frame that left no `ui_phases`, on `ui_cuts`' terms.
    /// **Zero new clock reads**: the six stamps are already taken on every
    /// presented frame that reaches the pass; only seven `u32` subtractions
    /// moved out of the arm.
    ///
    /// # These seven are a LOWER BOUND on the cut, never its maximum
    ///
    /// **The latch key is `service`, not any cut.** [`FrameLedger::finalize`]
    /// replaces this record only when a frame's `service` exceeds the stored
    /// one, so these seven describe whichever frame was slowest OVERALL — and
    /// a frame whose `stack` was large while its `service` was not is never
    /// latched and never printed. Read "at least this much", never "at most".
    ///
    /// And the `boot:` copy is **not** a session maximum of anything but
    /// `service`. It is monotone in that one field, so its `stack_cuts` move
    /// in BOTH directions across a session: on the native scene-D leg of
    /// 2026-09-08 run 3 printed `ui_stack=1353 us` on its first tick and
    /// `132 us` on its last, a later present stall having displaced the
    /// record with a frame that barely touched the UI. It reads like a
    /// high-water mark and is not one; the first lane to point this family at
    /// a leg misread it exactly that way before checking the key.
    ///
    /// Combined with [`Hist`] carrying no maximum, that leaves the maximum of
    /// `ui.stack` **unreported by this tree**. Closing it means a latch keyed
    /// on the cut, which is a new comparison per frame and was not worth it
    /// on the evidence available when this landed — recorded here so the next
    /// reader meets the gap rather than inferring a figure from these seven.
    pub(crate) stack_cuts: [u32; 7],
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
    /// The seven `post` cuts of THIS frame, in [`PostHists`]' order:
    /// `[handle, dispatch, back, wake, poll, repaint, close]`. They telescope
    /// to `segments[5]` within [`micros`]' truncation — at most six
    /// microseconds for seven cuts — so the worst frame's `post` is
    /// decomposed by arithmetic on ONE frame.
    ///
    /// **Here for [`WorstFrame::ui_cuts`]' reason exactly, and this is the
    /// segment where the gap was widest.** [`PostHists`] records inside
    /// `finalize`'s `if interacted` arm, and `post` is not a per-frame cost
    /// at all — it is one occasional event on an otherwise 46 µs span. Every
    /// such event measured so far has landed on an IDLE frame: the Mac
    /// scene A leg of 2026-09-10 latched `post=15,409 µs` on a
    /// `family=idle` frame, a 335x outlier, and NOTHING in this tree could
    /// name which of the seven cuts it was — the interact histograms never
    /// saw the frame and this line carried no `post_*` column.
    ///
    /// Zeroed on a frame that left no `post_phases`. **Zero new clock
    /// reads**: the six stamps are already taken on every frame that reaches
    /// the tail; only the subtraction moved out of the arm.
    pub(crate) post_cuts: [u32; 7],
    /// The seven `dispatch` cuts of THIS frame, in [`DispatchHists`]' order:
    /// `[dedupe, marks, hydrate, prepare, hitmap, offload, residual]` — the
    /// second of [`WorstFrame::post_cuts`], opened up. They telescope to
    /// `post_cuts[1]` by construction, the residual being the parent minus
    /// the six.
    ///
    /// **One level further down for [`WorstFrame::stack_cuts`]' reason.**
    /// `dispatch` held 84 % of `post` on the Firefox scene D leg
    /// [`DispatchHists`] names, so a `post` spike is a `dispatch` spike until
    /// shown otherwise — and the six named cuts are the difference between
    /// "the tail dispatched" and "the tail built a paint input on the frame
    /// thread".
    ///
    /// Zeroed on a frame whose tail dispatched nothing, which is most of
    /// them; that is the same absence [`FrameLedger::record_dispatch_cuts`]
    /// files, spelled as zeros here because this record is one frame's
    /// anatomy and not a distribution.
    pub(crate) dispatch_cuts: [u32; 7],
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
    /// `service_interact` with the `finish` split's `present` cut taken back
    /// out — see [`service_less_present_micros`]. Same frames as
    /// `service_interact`, one cut lighter, and never added to it.
    service_less_present_interact: Hist,
    /// `service_idle`'s frames, the same cut lighter.
    service_less_present_idle: Hist,
    /// See [`SegmentHists`] — interact frames only.
    segments: SegmentHists,
    /// See [`PreHists`] — `segments.pre`, opened up, same frames.
    pre: PreHists,
    /// See [`PrepareHists`] — `segments.prepare`, opened up, same frames.
    prepare: PrepareHists,
    /// See [`UiHists`] — `segments.ui`, opened up, same frames.
    ui: UiHists,
    /// See [`StackHists`] — `ui.stack`, opened up, same frames.
    stack: StackHists,
    /// See [`PanesHists`] — `ui.panes`, opened up, same frames.
    panes: PanesHists,
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

/// `service` with the `finish` split's eighth cut — the
/// `SurfaceTexture::present` call — taken back out.
///
/// # What it is, and what it is not
///
/// `service` already excludes ONE end of the swapchain handover: the acquire,
/// which is the vsync block and the display's time rather than ours. It
/// includes the other end. This figure excludes both. It is a different
/// FIGURE over exactly `service`'s frames, never a different denominator, and
/// **it is not the bar**: the responsiveness bar this instrument was built
/// for is p99 interact `service`, it is stated in `service`, and nothing here
/// restates it. A reader who quotes this one against that bar is quoting two
/// different quantities.
///
/// # Why it exists
///
/// On a display with no present path the WSI does not wait — it copies.
/// Measured on NVIDIA's Vulkan WSI under Xvfb, 2026-09-06: 6.73 ms at
/// 640x480, 13.46 at 1024x588, 45.26 at 1920x1080 and 87.8 at 2878x1651 — a
/// 6.73x time ratio across a 6.75x pixel ratio, which is a blit at 21.8 ns/px
/// and not a vblank wait. That readback is charged to `finish`, therefore to
/// `service`, so on every headless arm this campaign runs every `service`
/// percentile reads `over` and the frame thread is held to ~11 fps. The
/// excluded cut is the WSI's on either kind of display — a wait where there
/// is a compositor, a copy where there is not — and taking it out is what
/// makes a headless leg's figure comparable to a panel's at all.
///
/// # Truncation
///
/// Both terms are truncating [`micros`] results and the subtraction
/// re-truncates one boundary, so this is NOT the same number as the same six
/// spans measured with `finish` cut short at `freed`: it is that number or
/// one microsecond above it, never below and never two above. The bound is
/// the two-cut case of [`micros`]' own rule — `finish` decomposed into
/// (everything up to `freed`) and (`present`) loses `0 ..= 1` µs to
/// truncation — and it is derived off the pair, not chosen to fit a reading.
/// `the_service_less_present_figure_is_service_minus_the_present_cut` holds
/// exactly that.
///
/// # Saturating, not asserting
///
/// `present` is a cut of a segment of `service`, so it cannot legitimately
/// exceed it. But the two are four clock reads between them and a clock that
/// steps backwards (a coarse or non-monotonic web clock) would otherwise
/// underflow on the frame thread. A zero is the honest report of that case,
/// on [`dispatch_cut_micros`]' terms.
fn service_less_present_micros(service: u32, present: u32) -> u32 {
    service.saturating_sub(present)
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

/// The eight cuts of the `ui` split's `panes` cut, in call order:
/// `[setup, panel, resolve, widget, content, tools, credit, residual]` — see
/// [`PanesHists`], whose fields these are.
///
/// `panes` is the parent cut in whole microseconds, as [`ui_phase_micros`]
/// computed it; `cuts` is the nanosecond accumulation `render_panes` itself
/// made across its pane loop. The residual is the parent minus the seven,
/// saturating at zero — [`dispatch_cut_micros`]' shape and its reasoning
/// verbatim, and a free function so the telescoping is testable without a
/// frame.
///
/// **Saturating and not asserting**, for [`dispatch_cut_micros`]' reason: the
/// seven are measured inside the span the parent brackets and cannot
/// legitimately exceed it, but the parent is two clock reads and the seven
/// are ten on a one-pane plan-view frame, and a clock that steps backwards
/// between them would otherwise panic on the frame thread.
fn panes_cut_micros(cuts: squallar_egui::shell_api::PanesCuts, panes: u32) -> [u32; 8] {
    let us = |ns: u64| -> u32 { (ns / 1_000).min(u64::from(u32::MAX)) as u32 };
    let named = [
        us(cuts.setup_ns),
        us(cuts.panel_ns),
        us(cuts.resolve_ns),
        us(cuts.widget_ns),
        us(cuts.content_ns),
        us(cuts.tools_ns),
        us(cuts.credit_ns),
    ];
    let claimed = named.iter().fold(0u32, |sum, &cut| sum.saturating_add(cut));
    let [setup, panel, resolve, widget, content, tools, credit] = named;
    [
        setup,
        panel,
        resolve,
        widget,
        content,
        tools,
        credit,
        panes.saturating_sub(claimed),
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

/// The seven contiguous cuts of the `ui` split's `stack` cut, in call order:
/// `[snap, gate, hydrate, statuses, render, inspector, settle]` — see
/// [`StackHists`], whose fields these are.
///
/// Contiguous by construction: each cut ends where the next begins, and the
/// pair at the ends are the PARENT CUT's own boundaries — `statusbar` and
/// `shell`, the very two [`ui_phase_micros`]' fifth entry is taken from — so
/// the seven sum to `micros(statusbar, shell)` to within the six microseconds
/// seven truncating [`micros`] calls can lose, and never over it. A free
/// function so the telescoping is testable without a frame.
///
/// **Both ends are the parent's, neither is this split's own.** That is what
/// makes the last cut a named span rather than a subtraction: `settle` runs
/// to the parent's right boundary, so a `stack` residual has nowhere to hide.
fn stack_phase_micros(
    statusbar: Instant,
    stack: &squallar_egui::shell_api::StackStamps,
    shell: Instant,
) -> [u32; 7] {
    [
        micros(statusbar, stack.snapped),
        micros(stack.snapped, stack.gated),
        micros(stack.gated, stack.hydrated),
        micros(stack.hydrated, stack.statused),
        micros(stack.statused, stack.rendered),
        micros(stack.rendered, stack.inspected),
        micros(stack.inspected, shell),
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

/// Where `present` sits among [`finish_phase_micros`]' eight — the cut
/// [`service_less_present_micros`] takes back out.
///
/// Named rather than spelled `7` at the site that reads it: the eight are in
/// call order, a cut inserted or reordered moves this, and a stale index
/// would subtract the wrong span while every telescoping gate in the file
/// stayed green. `the_present_cut_index_names_the_present_call` holds it
/// against the very pair of stamps `FinishPhaseStamps` documents as the
/// present.
const PRESENT_CUT: usize = 7;

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

        // **The same, one level down, and above the arm for the same reason.**
        // These seven cut `ui_cuts[4]` -- the `stack` cut, which owns the `ui`
        // tail and whose maximum is 16-20x its own median -- and HALF the
        // measured spikes fall on idle frames, where `StackHists` records
        // nothing. Zero new clock reads: `m.ui_phases` already carries the six
        // stamps; only seven subtractions left the arm. The statement may not
        // read `interacted` -- see
        // `the_worst_frames_stack_cuts_are_computed_outside_the_interact_arm`.
        let stack_cuts = m.ui_phases.as_ref().map_or([0u32; 7], |phases| {
            stack_phase_micros(phases.statusbar, &phases.stack, phases.shell)
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

        // Above the arm for the two bindings above's reason and one more of
        // its own: the eight `finish` cuts are already recorded on EVERY
        // presented frame (see [`FinishHists`]), and the eighth of them is
        // the subtrahend `service less present` needs on BOTH families. The
        // eight `self.finish.*.record` calls stay in their own block below,
        // so no histogram's denominator moves; only the arithmetic left it.
        // Zero new clock reads: `m.finish_phases` is already stamped on every
        // presented frame.
        //
        // `None` rather than eight zeros, and the `if let` at each of the
        // three sites below rather than a `map_or(0, …)`: a frame that left
        // no `finish_phases` has no `present` to subtract, and subtracting a
        // zero would file its whole `service` under a name that promises the
        // present is out of it -- a false reading on a frame that presented,
        // where an absent sample is an absence. The only arm that leaves none
        // is the skipped/lost one, and `finalize` returned on it above; the
        // two families' `n` are printed side by side so a path that ever
        // makes them differ is readable off the line rather than inferred.
        let finish_cuts = m
            .finish_phases
            .as_ref()
            .map(|phases| finish_phase_micros(acquire_end, phases, present_return));

        // Above the arm, and for the sharpest instance of the three
        // bindings above's reason. `post` is the one segment whose cost is
        // NOT per-frame: a 46 us span that once read 15,409 us, and every
        // such event observed has landed on an idle frame, where `PostHists`
        // records nothing. Zero new clock reads: `m.post_phases` is stamped
        // on every frame that reaches the tail; only seven subtractions left
        // the arm. The statement may not read `interacted` -- see
        // `the_worst_frames_post_cuts_are_computed_outside_the_interact_arm`.
        let post_cuts = m.post_phases.as_ref().map_or([0u32; 7], |phases| {
            post_phase_micros(present_return, phases, now)
        });

        // The same, one level down: these seven cut `post_cuts[1]` -- the
        // `dispatch` cut, which held 84% of `post` on the leg `DispatchHists`
        // names. Zeros on a frame whose tail dispatched nothing, which is the
        // absence `record_dispatch_cuts` files.
        let dispatch_cuts = m
            .dispatch
            .map_or([0u32; 7], |cuts| dispatch_cut_micros(cuts, post_cuts[1]));

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
            // Beside `service` and never instead of it: the same frame, one
            // cut lighter. See `service_less_present_micros` -- and note that
            // the bar this instrument reports against is still the line
            // above.
            if let Some(cuts) = finish_cuts {
                self.service_less_present_interact
                    .record(service_less_present_micros(service, cuts[PRESENT_CUT]));
            }
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
                // One level further down, inside the very guard the ninth cut
                // above records under: these seven telescope to `stack`, the
                // cut recorded four lines up, and the two families' `n` are
                // equal BY CONSTRUCTION rather than by inspection. A frame
                // that drew no panel still contributes a sample -- `gate`
                // holds the whole cut on it, which is a reading and not an
                // absence -- so this guard is `ui_phases`, never a panel
                // test. See `StackHists` for why a narrower denominator here
                // would break every share read off it.
                let [snap, gate, hydrate, statuses, render, inspector, settle] = stack_cuts;
                self.stack.snap.record(snap);
                self.stack.gate.record(gate);
                self.stack.hydrate.record(hydrate);
                self.stack.statuses.record(statuses);
                self.stack.render.record(render);
                self.stack.inspector.record(inspector);
                self.stack.settle.record(settle);
                // And the seventh cut, opened up on the same terms: inside
                // the very guard `ui.panes` records under, so the two
                // families' `n` are equal BY CONSTRUCTION. Unlike the seven
                // above these arrive as nanosecond sums over a pane loop, so
                // the parent value -- `panes`, destructured five lines up --
                // is what the residual is taken from. Zero new clock reads
                // here: `render_panes` took them where the loop is.
                let [
                    panes_setup,
                    panes_panel,
                    panes_resolve,
                    panes_widget,
                    panes_content,
                    panes_tools,
                    panes_credit,
                    panes_residual,
                ] = panes_cut_micros(
                    m.ui_phases
                        .as_ref()
                        .map_or_else(Default::default, |phases| phases.panes_cuts),
                    panes,
                );
                self.panes.setup.record(panes_setup);
                self.panes.panel.record(panes_panel);
                self.panes.resolve.record(panes_resolve);
                self.panes.widget.record(panes_widget);
                self.panes.content.record(panes_content);
                self.panes.tools.record(panes_tools);
                self.panes.credit.record(panes_credit);
                self.panes.residual.record(panes_residual);
            }
            // And the same for `post`, whose right-hand boundary is `now` —
            // the very instant this function opened with, so the sixth cut
            // closes on the same stamp the segment above did.
            if m.post_phases.is_some() {
                let [handle, dispatch, back, wake, poll, repaint, close] = post_cuts;
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
                if m.dispatch.is_some() {
                    let [dedupe, marks, hydrate, prepare, hitmap, offload, residual] =
                        dispatch_cuts;
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
            if let Some(cuts) = finish_cuts {
                self.service_less_present_idle
                    .record(service_less_present_micros(service, cuts[PRESENT_CUT]));
            }
        }

        // **Outside the `interacted` arm on purpose, and the only split that
        // is.** `finish`'s expensive frames are idle ones — see
        // [`FinishHists`] for the measurement that says so — so this family
        // records on every presented frame and carries its own parent
        // (`whole`) rather than borrowing `segments.finish`'s narrower one.
        if let Some(cuts) = finish_cuts {
            let [file, view, draw, resolve, submit, collect, free, present] = cuts;
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
                stack_cuts,
                pre_cuts,
                post_cuts,
                dispatch_cuts,
                interact: interacted,
            },
        ));
        if self.worst_since_boot.is_none_or(|w| service > w.service) {
            self.worst_since_boot = Some(WorstFrame {
                service,
                segments,
                ui_cuts,
                stack_cuts,
                pre_cuts,
                post_cuts,
                dispatch_cuts,
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

    /// See [`service_less_present_micros`] — `service_interact`'s frames with
    /// the `finish` split's eighth cut taken back out. Never added to
    /// `service_interact` and never to `finish_phases().present`.
    pub(crate) fn service_less_present_interact(&self) -> &Hist {
        &self.service_less_present_interact
    }

    /// `service_idle`'s frames, the same cut lighter.
    pub(crate) fn service_less_present_idle(&self) -> &Hist {
        &self.service_less_present_idle
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

    /// See [`StackHists`] — `ui_phases().stack`, opened up, and never added
    /// to it.
    pub(crate) fn stack_phases(&self) -> &StackHists {
        &self.stack
    }

    /// See [`PanesHists`] — `ui_phases().panes`, opened up, and never added
    /// to it.
    pub(crate) fn panes_phases(&self) -> &PanesHists {
        &self.panes
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
        DispatchCuts, FinishPhaseStamps, Instant, PRESENT_CUT, PostPhaseStamps, PrePhaseStamps,
        PumpPhaseStamps, WorstFrame, dispatch_cut_micros, finish_phase_micros, latch_worst, micros,
        panes_cut_micros, post_phase_micros, pre_phase_micros, prepare_phase_micros,
        pump_phase_micros, service_less_present_micros, service_micros, stack_phase_micros,
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
    /// Stack stamps for a fixture that says NOTHING about the stack split:
    /// all six on the parent cut's own left boundary, so the seven cuts read
    /// `[0, 0, 0, 0, 0, 0, whole]` -- coherent and telescoping, and
    /// deliberately not a claim about where stack time goes. The stack split
    /// has its own fixture (`stack_stamps_at`); a ui-level test that borrowed
    /// this one's numbers would be reading a shape this helper invented.
    fn stack_stamps_flat(statusbar: Instant) -> squallar_egui::shell_api::StackStamps {
        squallar_egui::shell_api::StackStamps {
            snapped: statusbar,
            gated: statusbar,
            hydrated: statusbar,
            statused: statusbar,
            rendered: statusbar,
            inspected: statusbar,
        }
    }

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
            stack: stack_stamps_flat(at(offsets[3])),
            // The `panes` split's own seven are nanosecond sums this helper
            // has no frame to take, and every cut of the level ABOVE them
            // telescopes whatever they hold: an all-zero accumulation files
            // the whole parent cut under `residual`, which is a coherent
            // reading and not a claim about where pane time goes. The panes
            // split has its own fixture -- see `panes_cuts`.
            panes_cuts: squallar_egui::shell_api::PanesCuts::default(),
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
            "the nine cuts do not sum to the ui span they decompose, so the \
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

    /// A `render_stack_and_inspector` whose six interior boundaries land at
    /// the given microsecond offsets from the parent cut's LEFT boundary —
    /// the `statusbar` stamp — so a test can state its stamps as arithmetic.
    ///
    /// **Offsets from `statusbar`, not from `snapped`**, because the first
    /// cut is bounded on the left by the parent and a fixture that could not
    /// move that boundary could not exercise it.
    fn stack_stamps_at(
        statusbar: Instant,
        offsets: [u64; 6],
    ) -> squallar_egui::shell_api::StackStamps {
        let at = |us: u64| statusbar + std::time::Duration::from_micros(us);
        squallar_egui::shell_api::StackStamps {
            snapped: at(offsets[0]),
            gated: at(offsets[1]),
            hydrated: at(offsets[2]),
            statused: at(offsets[3]),
            rendered: at(offsets[4]),
            inspected: at(offsets[5]),
        }
    }

    /// **The seven cuts are a decomposition of `ui.stack`, not a sample of
    /// it.**
    ///
    /// The sum telescopes to `micros(statusbar, shell)` — the very span
    /// [`super::UiHists::stack`] records, and [`ui_phase_micros`]' fifth
    /// entry — so "what is in the layer stack" is answered by subtraction
    /// rather than by inference. `stack` owns the `ui` tail on every leg
    /// measured and was one undivided cut while it did.
    ///
    /// **Both ends are the PARENT cut's**, which is the property a re-pointed
    /// boundary breaks: reading cut 4's left edge off `gated` instead of
    /// `hydrated` leaves `hydrate`'s span in no cut at all and the sum falls
    /// short.
    #[test]
    fn the_stack_phases_telescope_to_stack() {
        let statusbar = Instant::now();
        let stack = stack_stamps_at(statusbar, [90, 410, 1_500, 4_900, 7_300, 8_950]);
        let shell = statusbar + std::time::Duration::from_micros(9_514);

        let cuts = stack_phase_micros(statusbar, &stack, shell);
        assert_eq!(
            cuts,
            [90, 320, 1_090, 3_400, 2_400, 1_650, 564],
            "a cut moved: the seven no longer bracket the regions they are \
             named for",
        );
        assert_eq!(
            cuts.iter().sum::<u32>(),
            micros(statusbar, shell),
            "the seven cuts do not sum to the stack span they decompose, so \
             the attribution this instrument reports is not an attribution of \
             ui.stack",
        );
        assert_eq!(cuts.iter().sum::<u32>(), 9_514);
        // And the parent really is the `ui` split's fifth cut, taken from the
        // same two instants -- not a span that merely resembles it.
        let ui_start = statusbar - std::time::Duration::from_micros(4_000);
        let ui_end = shell + std::time::Duration::from_micros(6_000);
        let ui = ui_phase_micros(
            ui_start,
            &UiPhaseStamps {
                polled: ui_start + std::time::Duration::from_micros(300),
                laid_out: ui_start + std::time::Duration::from_micros(1_900),
                topbar: ui_start + std::time::Duration::from_micros(2_500),
                statusbar,
                shell,
                dialog: shell + std::time::Duration::from_micros(500),
                panes: shell + std::time::Duration::from_micros(5_000),
                applied: shell + std::time::Duration::from_micros(5_500),
                stack,
                panes_cuts: squallar_egui::shell_api::PanesCuts::default(),
            },
            ui_end,
        );
        assert_eq!(
            ui[4],
            cuts.iter().sum::<u32>(),
            "the seven do not decompose the `ui` split's fifth cut, so this \
             family names a span the level above it does not have",
        );
    }

    /// **The non-vacuity floor: every stamp must be able to move the answer.**
    ///
    /// Telescoping alone is satisfied by a degenerate split — one cut holding
    /// the whole span and six zeros telescopes perfectly and decomposes
    /// nothing. `every_ui_stamp_is_load_bearing_in_two_cuts`' shape, one level
    /// down: nudge one stamp and exactly two cuts change, by equal and
    /// opposite amounts. A split that folded a boundary away — `inspected`
    /// spelled as `rendered`, say — would move one cut or none.
    #[test]
    fn every_stack_stamp_is_load_bearing_in_two_cuts() {
        let statusbar = Instant::now();
        // Its own fixture, not the telescoping test's: every cut here is
        // wider than the 100 us nudge, so a stamp that fails to move a cut
        // fails this test rather than underflowing it.
        let base_offsets = [500u64, 1_500, 3_000, 5_000, 7_000, 8_500];
        let shell = statusbar + std::time::Duration::from_micros(9_514);
        let base = stack_phase_micros(statusbar, &stack_stamps_at(statusbar, base_offsets), shell);

        for stamp in 0..6 {
            let mut moved = base_offsets;
            moved[stamp] -= 100;
            let cuts = stack_phase_micros(statusbar, &stack_stamps_at(statusbar, moved), shell);
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
            assert_eq!(cuts.iter().sum::<u32>(), 9_514);
        }
    }

    /// **The floor's other half: no cut may be structurally empty.**
    ///
    /// [`every_stack_stamp_is_load_bearing_in_two_cuts`] holds that the
    /// boundaries are real; this holds that the *regions* are. A split whose
    /// seven names covered `stack` but where six were pinned at zero would
    /// pass the telescoping test and report a single opaque number under
    /// seven headings — which is the instrument this replaces, renamed.
    ///
    /// `snap` is the cut this is sharpest about. It brackets two `O(1)`
    /// selection snaps and it would be the natural one to fold into the gate
    /// beside it; folded, `snapped` and `gated` become the same instant and
    /// cut 1 reads zero on every frame the app will ever draw.
    #[test]
    fn no_stack_cut_is_structurally_pinned_to_zero() {
        let statusbar = Instant::now();
        let stack = stack_stamps_at(statusbar, [90, 410, 1_500, 4_900, 7_300, 8_950]);
        let shell = statusbar + std::time::Duration::from_micros(9_514);
        let cuts = stack_phase_micros(statusbar, &stack, shell);
        assert!(
            cuts.iter().all(|&c| c > 0),
            "a cut is zero on stamps chosen to make all seven non-zero, so it \
             cannot be reading the span it is named for: {cuts:?}",
        );
    }

    /// **An early return zeroes only the regions it skipped, and the seven
    /// still telescope.**
    ///
    /// Three of the seven regions do not run when the panel is closed — a
    /// `Compact` width, a faded chrome, both slide factors at zero — and all
    /// three returns are inside cut 2. `StackStamps::skipped_after` fills
    /// every remaining stamp from ONE clock read, so:
    ///
    /// * `hydrate`, `statuses`, `render` and `inspector` read exactly zero —
    ///   they are the four regions no code ran in;
    /// * `gate` holds the remainder, which is the reading: a large `stack`
    ///   all in `gate` is a frame that spent its time deciding not to draw;
    /// * `settle` is NOT zeroed. It closes on the parent's own right
    ///   boundary, so the `ShellPhased` construction and the return out of
    ///   `render_shell_phased` stay inside a named span on this path too.
    ///
    /// The degenerate this is red against is a fresh `now()` per skipped
    /// slot: the four cuts then carry the clock's own dust, `gate` loses it,
    /// and four figures describe the instrument rather than the frame.
    #[test]
    fn an_early_return_zeroes_the_cuts_it_skipped_and_still_telescopes() {
        // ---- Arm 1: the REAL constructor, structure only ----
        //
        // Every stamp comes off the same clock in call order. No magnitude is
        // asserted here and that is deliberate: `skipped_after` reads the
        // clock itself, so a fixture cannot both use the real constructor and
        // dictate microsecond-scale gaps -- six back-to-back reads are tens of
        // nanoseconds and truncate to zero. Mixing a synthetic offset into a
        // real-clock constructor is what made the first version of this test
        // assert a 90 us cut against stamps the clock had already placed.
        let statusbar = Instant::now();
        let snapped = Instant::now();
        // The constructor the app calls, not a hand-built twin: a fixture
        // that assembled the six itself would never exercise the one-read
        // rule this gate exists for.
        let stack = squallar_egui::shell_api::StackStamps::skipped_after(snapped);
        let shell = Instant::now();

        // **The stamps themselves, before the cuts.** Six back-to-back clock
        // reads take tens of nanoseconds and every one of them truncates to
        // zero microseconds, so a filler that read the clock per slot would
        // produce the SAME four zeros below and this gate would be vacuous.
        // The design rule is one read, and stamp identity is what says so.
        assert!(
            stack.gated == stack.hydrated
                && stack.hydrated == stack.statused
                && stack.statused == stack.rendered
                && stack.rendered == stack.inspected,
            "the five stamps an early return fills are not one instant, so \
             the filler read the clock more than once and the four skipped \
             cuts carry the clock's own dust rather than a structural zero",
        );
        assert_ne!(
            stack.snapped, stack.gated,
            "the filler overwrote the snap boundary, so cut 1 -- the one \
             region that DID run on this path -- is folded into the gate",
        );

        let cuts = stack_phase_micros(statusbar, &stack, shell);
        assert_eq!(
            &cuts[2..6],
            &[0, 0, 0, 0],
            "a region no code ran in reported time, so the filler took more \
             than one clock read and four cuts carry the clock's dust: \
             {cuts:?}",
        );
        // **Telescoping here is a BOUND, not an equality, and the arm's own
        // premise is why.** This arm's stamps come off the real clock, and
        // the comment at the top of it says the reads are tens of nanoseconds
        // apart -- so every one of the seven cuts truncates to 0 us while the
        // parent `micros(statusbar, shell)` truncates to whatever the whole
        // span happens to land on. On a quiet box that parent is 0 and the
        // equality held by luck; under a loaded one the six reads cross a
        // microsecond boundary, the parent rounds up to 1, and the assertion
        // failed with `left: 0, right: 1` -- on a peer's `--workspace` board,
        // for a lane that had not touched this file.
        //
        // That was a WALL-CLOCK assertion carrying a structural message: the
        // property this gate exists for is that the seven cuts decompose the
        // span and nothing hides between them, and that property is
        // `sum <= parent` with a gap under the cut count. Seven truncating
        // `micros` calls can lose at most `7 - 1 = 6` us between them (see
        // `micros`), so the bound is exactly 6 and NOT ONE MICROSECOND MORE:
        // a wider tolerance would swallow a genuinely missing, zeroed or
        // mis-paired cut, which is the only thing this assertion is here to
        // catch. `assert_telescopes_within_truncation` states both halves.
        //
        // Arm 2 below keeps the exact equality, and should: its stamps are
        // synthetic offsets from one base, no clock runs between them, and
        // equality is a real property of that arithmetic rather than a
        // property of how fast the machine was.
        assert_telescopes_within_truncation(
            &cuts,
            micros(statusbar, shell),
            "the seven cuts of an early-returning frame, off real back-to-back \
             clock reads",
        );

        // ---- Arm 2: synthetic stamps, where the MAGNITUDES are checkable ----
        //
        // The filler instant is placed far from `snapped`, which is what a
        // real closed-panel frame produces once the gate has done real work.
        // Legitimate to build by hand because arm 1 above already exercises
        // the constructor: this arm is about the arithmetic, that one about
        // `skipped_after`.
        let base = Instant::now();
        let syn_snapped = base + std::time::Duration::from_micros(90);
        let filler = base + std::time::Duration::from_micros(1_600);
        let syn = squallar_egui::shell_api::StackStamps {
            snapped: syn_snapped,
            gated: filler,
            hydrated: filler,
            statused: filler,
            rendered: filler,
            inspected: filler,
        };
        let syn_shell = base + std::time::Duration::from_micros(1_650);
        let syn_cuts = stack_phase_micros(base, &syn, syn_shell);
        assert_eq!(
            syn_cuts,
            [90, 1_510, 0, 0, 0, 0, 50],
            "the closed-panel shape is wrong: cut 1 is the snap, cut 2 must \
             HOLD THE WHOLE remainder of the decision, cuts 3-6 must be \
             structurally zero, and cut 7 must still close on the parent's \
             own right boundary",
        );
        assert!(
            syn_cuts[1] > syn_cuts[0],
            "the gate cut does not hold the remainder on a closed panel, so \
             the time spent deciding not to draw is attributed to nothing",
        );
        assert_eq!(syn_cuts.iter().sum::<u32>(), micros(base, syn_shell));
    }

    /// **The load-sensitive shape that red-gated a peer's board, constructed
    /// rather than waited for.**
    ///
    /// `an_early_return_zeroes_the_cuts_it_skipped_and_still_telescopes`'s
    /// first arm takes its stamps off the real clock. Six back-to-back reads
    /// are tens of nanoseconds on a quiet box, so historically every cut
    /// truncated to 0 and so did the parent, and an exact-equality assertion
    /// passed. Under a loaded box the same six reads straddle a microsecond
    /// boundary: the cuts still truncate to 0 individually, the PARENT rounds
    /// up to 1, and the assertion failed `left: 0, right: 1`.
    ///
    /// Reproducing that by loading the machine is not a test. This builds the
    /// arithmetic directly: a span of 1,200 ns whose seven sub-spans are each
    /// under 1,000 ns. `sum` is 0, `parent` is 1, and the ONLY thing that
    /// separates a passing gate from a failing one is whether the assertion
    /// is an equality or the truncation bound.
    ///
    /// **This test is red against the equality it replaced** — that is what
    /// makes it a regression test rather than a restatement.
    #[test]
    fn a_sub_microsecond_stack_span_whose_parent_rounds_up_still_telescopes() {
        let base = Instant::now();
        // The early-return shape: one filler instant for the five stamps
        // `skipped_after` fills, placed 600 ns in -- inside the same
        // microsecond as `base`.
        let filler = base + std::time::Duration::from_nanos(600);
        let stack = squallar_egui::shell_api::StackStamps {
            snapped: filler,
            gated: filler,
            hydrated: filler,
            statused: filler,
            rendered: filler,
            inspected: filler,
        };
        // The parent closes at 1,200 ns, which is a DIFFERENT microsecond
        // from `base`. This is the whole defect in three instants.
        let shell = base + std::time::Duration::from_nanos(1_200);

        let cuts = stack_phase_micros(base, &stack, shell);
        let parent = micros(base, shell);
        assert_eq!(
            cuts, [0; 7],
            "every sub-span is under a microsecond, so every cut must \
             truncate to zero; if one did not, this fixture no longer builds \
             the shape it exists to build: {cuts:?}",
        );
        assert_eq!(
            parent, 1,
            "the parent span must round UP to 1 us while its parts round \
             DOWN to 0 -- that mismatch IS the defect under test, and a \
             parent of {parent} means the fixture has stopped reproducing it",
        );
        assert_eq!(
            cuts.iter().sum::<u32>(),
            0,
            "the sum of the truncated parts must be 0 against a parent of 1",
        );

        // The property, which holds: the cuts claim no time the parent did
        // not have, and fall short by less than the cut count.
        let gap = assert_telescopes_within_truncation(
            &cuts,
            parent,
            "a sub-microsecond early-return span",
        );
        assert_eq!(
            gap, 1,
            "the whole gap here is one microsecond of truncation dust, well \
             inside the 6 us seven truncating micros() calls can lose",
        );
    }

    /// **The family records on exactly the frames its parent cut does.**
    ///
    /// The denominator gate, and the one a split is most likely to ship
    /// without. `stack.snap`'s `n` and `ui.stack`'s `n` must be equal on
    /// every path through `finalize`, because every share this family
    /// supports is its own `sum` over the parent cut's `sum` — two figures
    /// over two different frame sets are not a share, they are a coincidence.
    ///
    /// Held against a ledger driven through both arms: a frame that reaches
    /// the panel and an early-returning one, each recorded, plus a frame that
    /// left no `ui_phases` at all, which must add a sample to neither.
    ///
    /// # The object is the trajectory, not the total
    ///
    /// A final `total()` is a COUNT, and a count discards which frames it
    /// counted. A family that skipped one frame and double-recorded another
    /// ends level with its parent while never once having shared its
    /// denominator — so the claim, which is about the frame SET, would be
    /// checked against a figure that cannot see frame sets. The equality is
    /// therefore asserted after EVERY frame, and each step names which frame
    /// it followed.
    ///
    /// **Two families agreeing on ZERO is not proof of anything**, and the
    /// per-step count assertions are the half that says so — the first of
    /// them runs before any frame, where the equality holds and means
    /// nothing. The vacuous shape is one a real leg takes:
    /// `frame service (interact)` was measured at `n=0` on the rig's `wide`
    /// leg because that leg takes no input at all, so every interact-only
    /// family on it telescoped perfectly over an empty sample and looked
    /// correct while measuring nothing. The same caution applies one level
    /// out, to whoever validates this family on a leg: **check the leg
    /// produces interact frames before reading a share off it.**
    #[test]
    fn the_stack_family_records_on_exactly_the_frames_its_parent_does() {
        let mut ledger = super::FrameLedger::default();

        // **Checked after EVERY frame, not once at the end**, and the
        // difference is the whole point of the gate. `total()` is a count, and
        // a count discards WHICH frames contributed: a family that recorded on
        // frame 1 and not frame 2, then twice on frame 3, ends level with its
        // parent and has never once shared its denominator. The claim here is
        // about the frame SET, so the object has to be the trajectory -- the
        // pair after each frame -- and not the pair at the end.
        let agree = |ledger: &super::FrameLedger, after: &str| {
            let parent = ledger.ui.stack.total();
            for (name, family) in [
                ("snap", &ledger.stack.snap),
                ("gate", &ledger.stack.gate),
                ("hydrate", &ledger.stack.hydrate),
                ("statuses", &ledger.stack.statuses),
                ("render", &ledger.stack.render),
                ("inspector", &ledger.stack.inspector),
                ("settle", &ledger.stack.settle),
            ] {
                assert_eq!(
                    family.total(),
                    parent,
                    "after {after}: `frame stack ({name})` stands at {} \
                     samples against its parent `frame ui (stack)`'s {}. \
                     Every share this family supports is its own sum over \
                     that one, so a step where the two disagree is a share \
                     computed between two different frame sets -- even if \
                     they end level",
                    family.total(),
                    parent,
                );
            }
            parent
        };
        agree(&ledger, "no frames at all");

        for (nth, closed) in [false, true].into_iter().enumerate() {
            // **Every stamp off the real clock, in the order a frame takes
            // them.** `StackStamps::skipped_after` reads the clock itself, so
            // a synthetic timeline could not carry the closed arm without the
            // two disagreeing about when "now" is -- and a fixture whose
            // stamps cannot be ordered is not a frame that could exist.
            let start = Instant::now();
            let statusbar = Instant::now();
            let stack = if closed {
                squallar_egui::shell_api::StackStamps::skipped_after(Instant::now())
            } else {
                squallar_egui::shell_api::StackStamps {
                    snapped: Instant::now(),
                    gated: Instant::now(),
                    hydrated: Instant::now(),
                    statused: Instant::now(),
                    rendered: Instant::now(),
                    inspected: Instant::now(),
                }
            };
            let shell = Instant::now();
            let ui_end = Instant::now();
            ledger.cur.start = Some(start);
            ledger.cur.setup = Some(start);
            ledger.cur.ui_start = Some(start);
            ledger.cur.ui_end = Some(ui_end);
            ledger.cur.acquire = Some((ui_end, ui_end));
            ledger.cur.present_return = Some(ui_end);
            ledger.cur.ui_phases = Some(UiPhaseStamps {
                polled: start,
                laid_out: start,
                topbar: start,
                statusbar,
                shell,
                dialog: shell,
                panes: shell,
                applied: shell,
                stack,
                panes_cuts: squallar_egui::shell_api::PanesCuts::default(),
            });
            ledger.finalize(true);
            let parent = agree(
                &ledger,
                if closed {
                    "a frame that drew no panel"
                } else {
                    "a frame that reached the panel"
                },
            );
            assert_eq!(
                parent,
                nth as u64 + 1,
                "the parent cut did not take a sample from a frame that left \
                 ui_phases, so the equality above is holding two families \
                 level at a standstill rather than through a frame",
            );
        }

        // **A frame with no `ui_phases` at all.** The parent cut takes no
        // sample and neither may this family: seven zeros would be seven
        // false readings, not an absence. This is the step that a family
        // recording outside the guard fails, and it fails at the step rather
        // than only in the total.
        let start = Instant::now();
        let end = Instant::now();
        ledger.cur.start = Some(start);
        ledger.cur.setup = Some(start);
        ledger.cur.ui_start = Some(start);
        ledger.cur.ui_end = Some(end);
        ledger.cur.acquire = Some((end, end));
        ledger.cur.present_return = Some(end);
        ledger.finalize(true);
        let parent = agree(&ledger, "a frame that left no ui_phases");

        // **Two families agreeing on ZERO is not proof of anything.** The
        // equality above is vacuously true on a ledger no frame ever reached,
        // which is a shape a real leg takes: the rig's `wide` leg reads
        // `frame service (interact)` at n=0 because it takes no input at all,
        // so every interact-only family on it telescopes perfectly over an
        // empty sample and looks correct while measuring nothing.
        assert_eq!(
            parent, 2,
            "the parent cut does not hold the two samples this test drove \
             through it, so the per-step equality above proved nothing",
        );
    }

    /// **`frame panes (*)`'s `n` is `frame ui (panes)`'s `n`, on every frame
    /// and not only in the total** —
    /// `the_stack_family_records_on_exactly_the_frames_its_parent_does`, one
    /// cut across.
    ///
    /// Every figure this split supports is its own `sum` over the parent
    /// cut's `sum`. A family with a narrower denominator would still
    /// telescope on the frames it held and every share read off it would be
    /// arithmetic over the wrong frame set, so the claim is about the frame
    /// SET and the object has to be the trajectory — the pair after each
    /// frame — not the pair at the end.
    ///
    /// **And a frame that accumulated nothing still contributes a sample.**
    /// That is the difference from `DispatchHists`, which files no sample on
    /// a frame that dispatched nothing: `render_panes` runs on every frame
    /// that leaves `ui_phases`, so an all-zero accumulation is a frame whose
    /// whole `panes` cut is `residual` — a reading, and never an absence.
    #[test]
    fn the_panes_family_records_on_exactly_the_frames_its_parent_does() {
        let mut ledger = super::FrameLedger::default();

        let agree = |ledger: &super::FrameLedger, after: &str| {
            let parent = ledger.ui.panes.total();
            for (name, family) in [
                ("setup", &ledger.panes.setup),
                ("panel", &ledger.panes.panel),
                ("resolve", &ledger.panes.resolve),
                ("widget", &ledger.panes.widget),
                ("content", &ledger.panes.content),
                ("tools", &ledger.panes.tools),
                ("credit", &ledger.panes.credit),
                ("residual", &ledger.panes.residual),
            ] {
                assert_eq!(
                    family.total(),
                    parent,
                    "after {after}: `frame panes ({name})` stands at {} \
                     samples against its parent `frame ui (panes)`'s {}. \
                     Every share this family supports is its own sum over \
                     that one, so a step where the two disagree is a share \
                     computed between two different frame sets -- even if \
                     they end level",
                    family.total(),
                    parent,
                );
            }
            parent
        };
        agree(&ledger, "no frames at all");

        // **One frame that accumulated and one that did not.** The second is
        // the step a family guarded on "did this frame charge anything"
        // would fail, and it is the common frame: a pane loop can run
        // entirely inside the clock's grain on a cheap scene.
        for (nth, charged) in [true, false].into_iter().enumerate() {
            let start = Instant::now();
            let statusbar = Instant::now();
            let shell = Instant::now();
            let ui_end = Instant::now();
            ledger.cur.start = Some(start);
            ledger.cur.setup = Some(start);
            ledger.cur.ui_start = Some(start);
            ledger.cur.ui_end = Some(ui_end);
            ledger.cur.acquire = Some((ui_end, ui_end));
            ledger.cur.present_return = Some(ui_end);
            ledger.cur.ui_phases = Some(UiPhaseStamps {
                polled: start,
                laid_out: start,
                topbar: start,
                statusbar,
                shell,
                dialog: shell,
                panes: shell,
                applied: shell,
                stack: stack_stamps_flat(statusbar),
                panes_cuts: if charged {
                    panes_cuts([1_000, 0, 0, 0, 2_000, 0, 0])
                } else {
                    squallar_egui::shell_api::PanesCuts::default()
                },
            });
            ledger.finalize(true);
            let parent = agree(
                &ledger,
                if charged {
                    "a frame whose pane loop charged something"
                } else {
                    "a frame whose pane loop charged nothing"
                },
            );
            assert_eq!(
                parent,
                nth as u64 + 1,
                "the parent cut did not take a sample from a frame that left \
                 ui_phases, so the equality above is holding two families \
                 level at a standstill rather than through a frame",
            );
        }

        // **A frame with no `ui_phases` at all.** The parent cut takes no
        // sample and neither may this family: eight zeros would be eight
        // false readings, not an absence.
        let start = Instant::now();
        let end = Instant::now();
        ledger.cur.start = Some(start);
        ledger.cur.setup = Some(start);
        ledger.cur.ui_start = Some(start);
        ledger.cur.ui_end = Some(end);
        ledger.cur.acquire = Some((end, end));
        ledger.cur.present_return = Some(end);
        ledger.finalize(true);
        let parent = agree(&ledger, "a frame that left no ui_phases");

        assert_eq!(
            parent, 2,
            "the parent cut does not hold the two samples this test drove \
             through it, so the per-step equality above proved nothing",
        );
    }

    /// **The latched frame's seven `stack` cuts telescope to its own
    /// `ui_cuts[4]`**, so `frame worst`'s `stack_*` columns decompose the very
    /// frame the line names rather than standing beside it.
    ///
    /// The property the field exists for, and it is not the property
    /// `frame stack (*)` has: those seven histograms record inside
    /// `finalize`'s `if interacted` arm, and half the `ui` spikes measured on
    /// scene D fall on IDLE frames — 8,339, 6,600, 8,219 and 7,879 µs.
    ///
    /// **Both assertions are load-bearing and neither implies the other.**
    /// The sum catches a cut taken from the wrong pair of stamps only when
    /// the error changes the total; the exact array catches the degenerate
    /// the sum cannot see — whole-`stack` parked in one slot with zeros
    /// around it, which telescopes perfectly and attributes the tail to a
    /// region that never ran.
    #[test]
    fn the_worst_frames_stack_cuts_telescope_to_its_stack() {
        // ── Arm 1: whole-microsecond stamps, where the exactness IS true ──
        let statusbar = Instant::now();
        let stack = stack_stamps_at(statusbar, [90, 410, 1_500, 4_900, 7_300, 8_950]);
        let shell = statusbar + std::time::Duration::from_micros(9_514);
        let ui_stack = micros(statusbar, shell);
        let w = WorstFrame {
            service: 13_455,
            segments: [64, 55, 11_000, 2_829, 700, 293],
            ui_cuts: [11, 402, 1_207, 96, ui_stack, 4, 812, 3, 77],
            stack_cuts: stack_phase_micros(statusbar, &stack, shell),
            pre_cuts: [3, 21, 9, 14, 2, 7, 8],
            post_cuts: [0u32; 7],
            dispatch_cuts: [0u32; 7],
            interact: true,
        };
        assert_eq!(
            assert_telescopes_within_truncation(&w.stack_cuts, w.ui_cuts[4], "whole-us stamps"),
            0,
            "six stamps with nothing below a microsecond on them still lost \
             time, so the seven are not contiguous cuts of one span",
        );
        assert_eq!(
            w.stack_cuts,
            [90, 320, 1_090, 3_400, 2_400, 1_650, 564],
            "a cut moved: the seven no longer bracket the regions they are \
             named for, so `frame worst`'s stack_* columns name the wrong \
             spans",
        );
        assert_eq!(w.stack_cuts.iter().sum::<u32>(), 9_514);

        // ── Arm 2: the same seven spans with half a microsecond of dust on
        // each, which is what a clock hands the constructor ──
        //
        // Deterministic, and chosen so the loss is provably NOT zero: seven
        // fractions of 0.5 sum to 3.5, so the seven cuts fall exactly 3 us
        // short of a parent that is itself truncated.
        let dusty_statusbar = Instant::now();
        let mut at = 0u64;
        let mut ns = [0u64; 6];
        for (slot, len) in [
            90_500u64, 320_500, 1_090_500, 3_400_500, 2_400_500, 1_650_500,
        ]
        .into_iter()
        .enumerate()
        {
            at += len;
            ns[slot] = at;
        }
        let dusty = squallar_egui::shell_api::StackStamps {
            snapped: dusty_statusbar + std::time::Duration::from_nanos(ns[0]),
            gated: dusty_statusbar + std::time::Duration::from_nanos(ns[1]),
            hydrated: dusty_statusbar + std::time::Duration::from_nanos(ns[2]),
            statused: dusty_statusbar + std::time::Duration::from_nanos(ns[3]),
            rendered: dusty_statusbar + std::time::Duration::from_nanos(ns[4]),
            inspected: dusty_statusbar + std::time::Duration::from_nanos(ns[5]),
        };
        let dusty_shell = dusty_statusbar + std::time::Duration::from_nanos(at + 564_500);
        let dusty_frame = WorstFrame {
            ui_cuts: [
                11,
                402,
                1_207,
                96,
                micros(dusty_statusbar, dusty_shell),
                4,
                812,
                3,
                77,
            ],
            stack_cuts: stack_phase_micros(dusty_statusbar, &dusty, dusty_shell),
            ..w
        };
        assert_eq!(
            assert_telescopes_within_truncation(
                &dusty_frame.stack_cuts,
                dusty_frame.ui_cuts[4],
                "sub-microsecond stamps",
            ),
            3,
            "the seven cuts of a frame whose stamps carry fractions did not \
             lose the 3 us seven truncating micros() calls must lose, so this \
             arm is not exercising the truncation it exists for",
        );

        // ── Arm 3: a real clock, which is the only one that produces the
        // fractions the field will actually carry ──
        let mut samples = 0u32;
        for _ in 0..256 {
            let live_statusbar = Instant::now();
            let live = squallar_egui::shell_api::StackStamps {
                snapped: Instant::now(),
                gated: Instant::now(),
                hydrated: Instant::now(),
                statused: Instant::now(),
                rendered: Instant::now(),
                inspected: Instant::now(),
            };
            let live_shell = Instant::now();
            let cuts = stack_phase_micros(live_statusbar, &live, live_shell);
            assert_telescopes_within_truncation(
                &cuts,
                micros(live_statusbar, live_shell),
                "a real clock",
            );
            samples += 1;
        }
        assert_eq!(
            samples, 256,
            "the real-clock arm did not take the samples it claims to have \
             taken, so its greenness is an absence and not a reading",
        );

        // **The green arm, beside the red ones.** A frame whose `stack` really
        // is all in one cut is a healthy input that RESEMBLES the degenerate
        // failure, and the gate must not fire on it.
        let single = WorstFrame {
            stack_cuts: [0, 0, 0, 0, 9_514, 0, 0],
            ..w
        };
        assert_eq!(
            assert_telescopes_within_truncation(
                &single.stack_cuts,
                single.ui_cuts[4],
                "a genuinely single-cut frame",
            ),
            0,
            "a frame whose stack was genuinely spent in one cut fails the \
             telescoping gate, so the gate over-fires on healthy input",
        );
    }

    /// **The seven cuts are computed for EVERY presented frame, not only the
    /// interact ones.** Held against `finalize`'s own source, on
    /// [`the_worst_frames_ui_cuts_are_computed_outside_the_interact_arm`]'s
    /// terms exactly: the binding must appear before the `if interacted {`
    /// that opens the arm, and the statement itself may not read the flag.
    ///
    /// The degenerate this is red against is the natural one — leaving the
    /// `stack_phase_micros` call where its seven `record` calls are. That
    /// shape compiles, telescopes on the frames it does fill, and reports
    /// seven zeros on exactly the frames the field was added to describe:
    /// half of scene D's `ui` spikes are on idle frames.
    #[test]
    fn the_worst_frames_stack_cuts_are_computed_outside_the_interact_arm() {
        let body = include_str!("frame_ledger.rs")
            .split_once("pub(crate) fn finalize(")
            .expect("finalize is no longer a method here")
            .1;
        let bound = body
            .find("let stack_cuts = ")
            .expect("finalize no longer binds the worst frame's seven stack cuts");
        let interact_arm = body
            .find("if interacted {")
            .expect("finalize no longer splits on the interact flag");
        assert!(
            bound < interact_arm,
            "the seven stack cuts are computed inside finalize's interact \
             arm, so every frame that PAYS for a click -- all of which are \
             filed idle, and where half the measured ui spikes live -- would \
             carry seven zeros on the one line that reports it",
        );
        let statement = body[bound..]
            .split_once("\n\n")
            .expect("the stack_cuts binding is no longer a statement of its own")
            .0;
        assert!(
            !statement.contains("interacted"),
            "the stack cuts binding reads the interact flag, so an idle frame \
             would carry seven zeros however early the binding sits: \
             {statement:?}",
        );
    }

    /// **Many sub-microsecond regions still telescope, and none of them is
    /// rounded into a neighbour.**
    ///
    /// [`DispatchHists`]' twin, ported because this family is always on and
    /// runs on a browser clock. Every one of the seven regions can be well
    /// under a microsecond on a closed or nearly-empty panel, and what must
    /// not happen is a cut absorbing a neighbour's fraction: each truncates
    /// its OWN span, so the seven fall short of the parent by the derived
    /// bound and never by more.
    #[test]
    fn many_sub_microsecond_stack_regions_survive_into_their_own_cuts() {
        let statusbar = Instant::now();
        let stack = squallar_egui::shell_api::StackStamps {
            snapped: statusbar + std::time::Duration::from_nanos(700),
            gated: statusbar + std::time::Duration::from_nanos(1_400),
            hydrated: statusbar + std::time::Duration::from_nanos(2_100),
            statused: statusbar + std::time::Duration::from_nanos(2_800),
            rendered: statusbar + std::time::Duration::from_nanos(3_500),
            inspected: statusbar + std::time::Duration::from_nanos(4_200),
        };
        let shell = statusbar + std::time::Duration::from_nanos(4_900);
        let cuts = stack_phase_micros(statusbar, &stack, shell);
        assert_eq!(
            cuts, [0; 7],
            "a 700 ns region reported a whole microsecond, so a cut is \
             rounding rather than truncating and the seven can exceed their \
             parent",
        );
        assert_eq!(
            assert_telescopes_within_truncation(&cuts, micros(statusbar, shell), "700 ns regions"),
            4,
            "seven 700 ns regions did not lose the 4 us of a 4,900 ns parent, \
             so this arm is not exercising the truncation it exists for",
        );
    }

    /// **A backward-stepping clock does not panic the frame thread.**
    ///
    /// [`cuts_that_overrun_their_span_report_a_zero_residual`]'s reason, and
    /// it is sharper here: this instrument is ALWAYS ON, it takes fourteen
    /// reads per frame on a browser, and `Instant::duration_since` panics on
    /// a negative interval on some platforms. A coarse or non-monotonic web
    /// clock can order two of these stamps wrongly, and a frame-thread panic
    /// in product telemetry is a worse outcome than a zero.
    #[test]
    fn stack_stamps_out_of_order_report_zeros_rather_than_panicking() {
        let statusbar = Instant::now();
        let later = statusbar + std::time::Duration::from_micros(5_000);
        // Every interior stamp BEFORE the parent's left boundary, which is
        // the worst ordering a clock can hand this function.
        let backwards = squallar_egui::shell_api::StackStamps {
            snapped: statusbar,
            gated: statusbar,
            hydrated: statusbar,
            statused: statusbar,
            rendered: statusbar,
            inspected: statusbar,
        };
        // Right boundary backwards TOO: with `later` on the right, the
        // seventh cut legitimately spans statusbar -> later and reads 5,000,
        // which is correct saturating behaviour and not the all-zero case.
        // The first version of this test asserted `[0; 7]` against that
        // spelling and was wrong about its own arithmetic.
        let cuts = stack_phase_micros(later, &backwards, statusbar);
        assert_eq!(
            cuts, [0; 7],
            "a stamp ordering the clock can produce did not report zeros",
        );

        // The MIXED ordering -- interior stamps behind the left boundary, the
        // right boundary ahead of them -- is the one a coarse clock actually
        // produces. It must not panic, and the one cut that can still be
        // non-zero is the seventh, which really does span those two instants.
        let mixed = stack_phase_micros(later, &backwards, later);
        assert_eq!(
            mixed,
            [0, 0, 0, 0, 0, 0, 5_000],
            "a mixed backwards ordering did not saturate to zeros with the \
             one genuine span intact",
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

    // ── `service less present`: the figure, its bound and its denominator ──

    /// **[`PRESENT_CUT`] indexes the cut that brackets the
    /// `SurfaceTexture::present` call**, and not the one beside it.
    ///
    /// The eight are in call order and the index is what
    /// [`service_less_present_micros`]' subtrahend is read through, so an
    /// index left stale by a reordering would subtract the wrong span while
    /// every telescoping gate in this file stayed green — the sum is
    /// unchanged by which cut you name. Held against the very pair of stamps
    /// [`FinishPhaseStamps::freed`] and the ledger's `present_return`
    /// document as the present, with a distinct value in every other slot so
    /// an off-by-one cannot land on an equal number.
    #[test]
    fn the_present_cut_index_names_the_present_call() {
        let acquire_end = Instant::now();
        let phases = finish_phases_at(acquire_end, [10, 30, 60, 100, 150, 210, 280]);
        let present_return = acquire_end + std::time::Duration::from_micros(400);
        let cuts = finish_phase_micros(acquire_end, &phases, present_return);
        assert_eq!(
            cuts,
            [10, 20, 30, 40, 50, 60, 70, 120],
            "a finish cut moved: the eight no longer bracket the calls they \
             are named for, and the index below names one of them",
        );
        assert_eq!(
            cuts[PRESENT_CUT],
            micros(phases.freed, present_return),
            "PRESENT_CUT does not index the `freed -> present_return` span, \
             so `service less present` subtracts a cut that is not the \
             present: {cuts:?}",
        );
    }

    /// **The figure is `service` minus the `finish` split's `present` cut, to
    /// within the one microsecond a two-cut subtraction of truncating
    /// [`micros`] results can gain.**
    ///
    /// The claim is stated against an INDEPENDENT spelling of the same
    /// quantity — the same six segments with `finish` measured to `freed`
    /// instead of to `present_return` — rather than against `service` minus
    /// `present`, which the constructor makes true by construction and which
    /// no defect in the choice of subtrahend could disturb.
    ///
    /// # Where the microsecond comes from, and why it is not a tolerance
    ///
    /// `finish` is one [`micros`] call; the pair (everything up to `freed`,
    /// the present) is two. By [`micros`]' own rule a decomposition of `n`
    /// cuts falls `0 ..= n - 1` µs short of its truncated parent, so with
    /// `n = 2` the parent carries `0` or `1` µs the two cuts do not — and
    /// that surplus stays behind in `service` when only the present is taken
    /// out. **A third cut would raise the bound by exactly one**;
    /// `assert_telescopes_within_truncation` derives it off the pair rather
    /// than being handed a constant, and the residual is then asserted to be
    /// that very gap, not merely inside it.
    #[test]
    fn the_service_less_present_figure_is_service_minus_the_present_cut() {
        // The five segments that are not `finish`. Identical in both
        // spellings, so nothing about them can absorb a truncation error.
        let others = [150u32, 900, 2_400, 1_800, 850];

        // A frame's `service less present`, its independent spelling and the
        // telescoping gap between them, for one set of finish stamps.
        let read = |acquire_end: Instant, phases: &FinishPhaseStamps, present_return: Instant| {
            let finish = micros(acquire_end, present_return);
            let up_to_present = micros(acquire_end, phases.freed);
            let cuts = finish_phase_micros(acquire_end, phases, present_return);
            let service = service_micros(
                false,
                0,
                [
                    others[0], others[1], others[2], others[3], finish, others[4],
                ],
                0,
            );
            let alt = others.iter().fold(up_to_present, |sum, &s| sum + s);
            (
                service,
                service_less_present_micros(service, cuts[PRESENT_CUT]),
                alt,
                assert_telescopes_within_truncation(
                    &[up_to_present, cuts[PRESENT_CUT]],
                    finish,
                    "finish into (up to the present) and (the present)",
                ),
            )
        };

        // ── Arm 1: whole-microsecond stamps, where the two spellings agree
        // exactly ──
        let acquire_end = Instant::now();
        let phases = finish_phases_at(acquire_end, [1, 2, 3, 4, 5, 6, 6]);
        let present_return = acquire_end + std::time::Duration::from_micros(10);
        let (service, figure, alt, gap) = read(acquire_end, &phases, present_return);
        assert_eq!(
            gap, 0,
            "seven stamps with nothing below a microsecond on them still lost \
             time, so `finish` is not the two cuts this subtraction assumes",
        );
        assert_eq!(
            figure, alt,
            "on whole-microsecond stamps the figure must equal the same six \
             spans with `finish` measured to `freed`; it read {figure} us \
             against {alt} us",
        );
        assert_eq!(service - figure, 4, "the present of arm 1's fixture");

        // ── Arm 2: half a microsecond of dust on each side of `freed`, which
        // is the case that makes the bound above non-vacuous ──
        //
        // 6.5 µs up to `freed` and 3.5 µs of present truncate to 6 and 3
        // against a parent of exactly 10, so the pair loses exactly the 1 µs
        // two truncating cuts can lose and the figure sits exactly 1 µs above
        // its independent spelling. Deterministic.
        let dusty_end = Instant::now();
        let ns = |n: u64| dusty_end + std::time::Duration::from_nanos(n);
        let dusty = FinishPhaseStamps {
            filed: ns(1_000),
            viewed: ns(2_000),
            drawn: ns(3_000),
            resolved: ns(4_000),
            submitted: ns(5_000),
            collected: ns(6_000),
            freed: ns(6_500),
        };
        let dusty_return = ns(10_000);
        let (_, dusty_figure, dusty_alt, dusty_gap) = read(dusty_end, &dusty, dusty_return);
        assert_eq!(
            dusty_gap, 1,
            "a 6.5 us head and a 3.5 us present did not lose the 1 us that \
             truncating two cuts of a 10 us parent must lose, so this arm is \
             not exercising the truncation it exists for",
        );
        assert_eq!(
            dusty_figure - dusty_alt,
            dusty_gap,
            "the microsecond the pair lost to truncation did not stay in the \
             figure, so the residual is not the one this gate's bound is \
             derived from",
        );

        // ── Arm 3: instants a clock actually produced ──
        //
        // Seven bare `Instant::now()` reads in the order `present_frame`
        // takes them. The bound is asserted and a non-zero gap deliberately
        // is not — demanding truncation from a live clock would be asserting
        // the clock, on `the_worst_frames_ui_cuts_telescope_to_its_ui`'s
        // terms.
        let mut samples = 0;
        for _ in 0..256 {
            let live_end = Instant::now();
            let live = FinishPhaseStamps {
                filed: Instant::now(),
                viewed: Instant::now(),
                drawn: Instant::now(),
                resolved: Instant::now(),
                submitted: Instant::now(),
                collected: Instant::now(),
                freed: Instant::now(),
            };
            let live_return = Instant::now();
            let (_, live_figure, live_alt, live_gap) = read(live_end, &live, live_return);
            assert_eq!(
                live_figure - live_alt,
                live_gap,
                "on clock instants the figure is not its independent spelling \
                 plus the pair's own truncation gap",
            );
            assert!(
                live_gap <= 1,
                "two truncating cuts of one parent cannot lose {live_gap} us",
            );
            samples += 1;
        }
        assert_eq!(
            samples, 256,
            "the real-clock arm did not take the samples it claims to have \
             taken, so its greenness is an absence and not a reading",
        );
    }

    /// **The figure is strictly below `service` whenever the present cost
    /// anything, and equal to it only when the present cost nothing.**
    ///
    /// The direction is the whole point of the family: a figure that could
    /// equal `service` on a frame the present ate would be `service` under a
    /// second name, which is the confusion the line's sentence exists to
    /// prevent. The equality arm is asserted too, because a figure that
    /// invented a difference on a frame with no present would be reporting
    /// its own arithmetic.
    #[test]
    fn the_figure_is_below_service_by_exactly_the_present_and_only_by_it() {
        let service = 90_900u32;
        for present in [1u32, 7, 1_000, 6_728, 87_800] {
            let figure = service_less_present_micros(service, present);
            assert!(
                figure < service,
                "a {present} us present left the figure at {figure} us \
                 against a service of {service} us, so the two families would \
                 report the same number on a frame the present ate",
            );
            assert_eq!(
                service - figure,
                present,
                "the figure is below service by something other than the \
                 present cut it is named for",
            );
        }
        assert_eq!(
            service_less_present_micros(service, 0),
            service,
            "a frame whose present did not reach a whole microsecond is the \
             one frame on which the two figures agree, and this one invented \
             a difference",
        );
    }

    /// **The positive control: on a frame the present ate, the figure is the
    /// work and `service` is the work plus the readback.**
    ///
    /// Every assertion above holds on a fixture where the present is dust,
    /// which is what a real compositor hands back — so none of them shows the
    /// figure doing the thing it exists for. This one states the case it was
    /// built for: NVIDIA's Vulkan WSI with no present path copies the
    /// swapchain image back at 21.8 ns/px, measured 87.8 ms at 2878x1651 on
    /// 2026-09-06, and that lands inside `finish` and therefore inside
    /// `service`. The figures are asserted against the fixture's own extent —
    /// the segments it is built from — and not against any threshold.
    #[test]
    fn a_frame_the_present_ate_reads_the_work_while_service_reads_the_readback() {
        // The measured readback at the campaign's own window size.
        let present = 87_800u32;
        // A healthy frame's six segments, `finish` carrying the readback.
        let work = [150u32, 900, 2_400, 1_800, 700, 850];
        let segments = [
            work[0],
            work[1],
            work[2],
            work[3],
            work[4] + present,
            work[5],
        ];
        let service = service_micros(false, 0, segments, 0);
        let figure = service_less_present_micros(service, present);
        let worked = work.iter().sum::<u32>();
        assert_eq!(
            service,
            worked + present,
            "the fixture's own service is not its work plus its readback",
        );
        assert_eq!(
            figure, worked,
            "the figure carried something other than the frame's work: it \
             read {figure} us against the {worked} us the six segments spend \
             outside the present",
        );
        assert!(
            present > service - present,
            "this fixture is not one the present ate, so it cannot be the \
             positive control it claims to be",
        );
    }

    /// A present larger than the service it sits inside cannot happen — it is
    /// a cut of a segment of that very span — but the two are four clock
    /// reads between them and a clock that steps backwards would underflow on
    /// the frame thread. Zero is the honest report, on
    /// `cuts_that_overrun_their_span_report_a_zero_residual`'s terms.
    #[test]
    fn a_present_larger_than_its_service_reports_zero_rather_than_underflowing() {
        assert_eq!(service_less_present_micros(5, 9), 0);
    }

    /// Drive one presented frame through the ledger, with or without the
    /// `finish` stamps `present_frame` takes after the acquire returns.
    ///
    /// Every `mark_*` reads the real clock, so the figures such a frame
    /// produces are the machine's; what is asserted off this helper is
    /// therefore only ever a SAMPLE COUNT, which is exactly the property the
    /// two tests below are about.
    fn present_one_frame(ledger: &mut super::FrameLedger, interacted: bool, with_finish: bool) {
        ledger.mark_frame_start();
        ledger.mark_setup_entry();
        ledger.mark_ui_start();
        ledger.mark_ui_end();
        let acquire_start = Instant::now();
        let acquire_end = Instant::now();
        ledger.record_acquire(acquire_start, acquire_end);
        if with_finish {
            ledger.record_finish_phases(finish_phases_at(acquire_end, [0, 0, 0, 0, 0, 0, 0]));
        }
        ledger.mark_present_return();
        ledger.finalize(interacted);
    }

    /// **`service` and `service less present` take the same frames.**
    ///
    /// The subtraction is only meaningful if the two figures come off one
    /// frame each, on one denominator: a family recorded on a wider or
    /// narrower set would be a difference between two aggregates that share
    /// no frame, which is the defect the `finish` split's own doc spends a
    /// paragraph on. Held on both families at once, with a different count in
    /// each so a mis-wired `record` cannot pass by symmetry.
    #[test]
    fn the_two_service_families_take_the_same_frames() {
        let mut ledger = super::FrameLedger::default();
        for _ in 0..3 {
            present_one_frame(&mut ledger, true, true);
        }
        present_one_frame(&mut ledger, false, true);
        assert_eq!(
            (
                ledger.service_interact().total(),
                ledger.service_less_present_interact().total(),
            ),
            (3, 3),
            "the interact families disagree on how many frames they saw",
        );
        assert_eq!(
            (
                ledger.service_idle().total(),
                ledger.service_less_present_idle().total(),
            ),
            (1, 1),
            "the idle families disagree on how many frames they saw",
        );
    }

    /// **A presented frame that left no `finish` stamps is an ABSENCE here,
    /// never a zero.**
    ///
    /// Such a frame has no present to subtract, so filing its whole `service`
    /// under a name that promises the present is out of it would be a false
    /// reading — "this frame's present cost nothing" — on a frame that
    /// presented. The app produces no such frame: the only arm of
    /// `present_frame` that skips `record_finish_phases` also calls
    /// `mark_skipped`, and `finalize` discards that frame outright. This gate
    /// is what keeps the two `n` figures equal there BY the absence rather
    /// than by luck, and what makes a divergence between them readable off
    /// the pair of lines instead of silently filed as a zero present.
    #[test]
    fn a_presented_frame_with_no_finish_stamps_offers_no_service_less_present_sample() {
        let mut ledger = super::FrameLedger::default();
        present_one_frame(&mut ledger, true, false);
        present_one_frame(&mut ledger, false, false);
        assert_eq!(
            (
                ledger.service_interact().total(),
                ledger.service_idle().total(),
            ),
            (1, 1),
            "the frames this gate is about did not reach `service` at all, so \
             it is not testing the case it names",
        );
        assert_eq!(
            (
                ledger.service_less_present_interact().total(),
                ledger.service_less_present_idle().total(),
            ),
            (0, 0),
            "a frame with no present to subtract was filed as one whose \
             present cost nothing, which reports this instrument's own \
             arithmetic as a measurement",
        );
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

    /// A candidate frame whose six segments sum to `service`, whose nine
    /// `ui` cuts sum to `segments[2]` and whose seven `stack` cuts sum to
    /// `ui_cuts[4]`, so a test can state a frame as one number and still have
    /// all three decompositions telescope.
    ///
    /// The nine are spread rather than parked on one cut on purpose: a fixture
    /// of `[0, 0, 0, 0, ui, 0, 0, 0, 0]` telescopes under a formatter that
    /// printed `segments[2]` in the `ui_stack=` slot, so it could not tell the
    /// two apart. `the_worst_frames_ui_cuts_telescope_to_its_ui` keeps that
    /// degenerate shape as its own explicit green arm instead. The seven are
    /// spread for the same reason, one level down.
    fn frame(service: u32, interact: bool) -> WorstFrame {
        let sixth = service / 6;
        let mut segments = [sixth; 6];
        segments[5] = service - sixth * 5;
        let ninth = segments[2] / 9;
        let mut ui_cuts = [ninth; 9];
        ui_cuts[8] = segments[2] - ninth * 8;
        let stack_seventh = ui_cuts[4] / 7;
        let mut stack_cuts = [stack_seventh; 7];
        stack_cuts[6] = ui_cuts[4] - stack_seventh * 6;
        let seventh = segments[0] / 7;
        let mut pre_cuts = [seventh; 7];
        pre_cuts[6] = segments[0] - seventh * 6;
        WorstFrame {
            service,
            segments,
            ui_cuts,
            stack_cuts,
            pre_cuts,
            post_cuts: [0u32; 7],
            dispatch_cuts: [0u32; 7],
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
            // Spread across all seven and summing to `ui_cuts[4]`, so the
            // fixture describes a frame that could exist at BOTH levels.
            stack_cuts: [40, 260, 500, 900, 9_100, 1_200, 100],
            pre_cuts: [100, 300, 200, 250, 50, 50, 50],
            post_cuts: [0u32; 7],
            dispatch_cuts: [0u32; 7],
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
            stack: stack_stamps_flat(dusty_start + std::time::Duration::from_nanos(ns[3])),
            panes_cuts: squallar_egui::shell_api::PanesCuts::default(),
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
            let polled = Instant::now();
            let laid_out = Instant::now();
            let topbar = Instant::now();
            let live_statusbar = Instant::now();
            let live = UiPhaseStamps {
                polled,
                laid_out,
                topbar,
                statusbar: live_statusbar,
                shell: Instant::now(),
                dialog: Instant::now(),
                panes: Instant::now(),
                applied: Instant::now(),
                stack: stack_stamps_flat(live_statusbar),
                panes_cuts: squallar_egui::shell_api::PanesCuts::default(),
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
            // Re-stated rather than inherited: `w`'s seven sum to its own
            // 12,100 us `stack`, and carrying them onto a frame whose
            // `ui_stack` is 41,000 would make this fixture describe a frame
            // that cannot exist one level down.
            stack_cuts: [1, 2, 3, 4, 40_985, 3, 2],
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
            stack_cuts: [10, 20, 30, 40, 3_860, 20, 20],
            pre_cuts: pre_phase_micros(start, &phases, setup),
            post_cuts: [0u32; 7],
            dispatch_cuts: [0u32; 7],
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

    /// **The seven `post` cuts and the seven `dispatch` cuts are computed for
    /// EVERY presented frame, not only the interact ones.** Held against
    /// `finalize`'s own source, on
    /// [`the_worst_frames_pre_cuts_are_computed_outside_the_interact_arm`]'s
    /// terms.
    ///
    /// **This is the segment where the degenerate had actually shipped.**
    /// Until these two bindings moved out, `post` was decomposed by
    /// [`PostHists`] alone — interact frames only — and `frame worst:`
    /// carried no `post_*` column at all. `post` is not a per-frame cost: it
    /// is a 46 µs span that latched **15,409 µs** on a Mac scene A frame of
    /// 2026-09-10, a 335x outlier, and that frame was `family=idle`. Nothing
    /// in the tree could name which of the seven cuts it was, so the
    /// campaign's strongest tail lead was unattributable by construction.
    #[test]
    fn the_worst_frames_post_cuts_are_computed_outside_the_interact_arm() {
        let body = include_str!("frame_ledger.rs")
            .split_once("pub(crate) fn finalize(")
            .expect("finalize is no longer a method here")
            .1;
        let interact_arm = body
            .find("if interacted {")
            .expect("finalize no longer splits on the interact flag");
        for (binding, cuts) in [
            ("let post_cuts = ", "seven post cuts"),
            ("let dispatch_cuts = ", "seven dispatch cuts"),
        ] {
            let bound = body
                .find(binding)
                .unwrap_or_else(|| panic!("finalize no longer binds the worst frame's {cuts}"));
            assert!(
                bound < interact_arm,
                "the {cuts} are computed inside finalize's interact arm, so \
                 every frame that PAYS for a click -- all of which are filed \
                 idle, and where every `post` spike measured so far has \
                 landed -- would carry seven zeros on the one line that \
                 reports it",
            );
            let statement = body[bound..]
                .split_once("\n\n")
                .unwrap_or_else(|| panic!("the {cuts} binding is no longer a statement of its own"))
                .0;
            assert!(
                !statement.contains("interacted"),
                "the {cuts} binding reads the interact flag, so an idle frame \
                 would carry seven zeros however early the binding sits: \
                 {statement:?}",
            );
        }
    }

    /// **The seven `post` cuts on the latched frame telescope to its `post`**,
    /// to within the six microseconds seven truncating [`micros`] calls can
    /// lose — the property that makes `frame worst:`'s `post_*` columns a
    /// decomposition of one frame rather than seven figures beside a total.
    #[test]
    fn the_worst_frames_post_cuts_telescope_to_its_post() {
        let base = Instant::now();
        let at = |us: u64| base + std::time::Duration::from_micros(us);
        // A tail whose seven spans are deliberately uneven and whose second
        // -- `dispatch` -- holds nearly all of it, which is the shape every
        // measured `post` spike has had.
        let phases = PostPhaseStamps {
            handled: at(37),
            actions: at(15_402),
            back: at(15_409),
            wake: at(15_431),
            poll: at(15_436),
            repaint: at(15_452),
        };
        let cuts = post_phase_micros(base, &phases, at(15_463));
        let post = micros(base, at(15_463));
        let summed: u32 = cuts.iter().sum();
        assert!(
            post >= summed && post - summed <= 6,
            "the seven post cuts sum to {summed} against a post of {post}; \
             seven truncating `micros` calls may lose at most six \
             microseconds and may never gain any",
        );
        assert_eq!(
            cuts[1], 15_365,
            "the second cut is not `dispatch`, so the `disp_*` columns \
             decompose the wrong parent",
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

    /// A `render_panes` accumulation stated in nanoseconds, one field at a
    /// time — [`cuts`]' twin one family across.
    fn panes_cuts(seven: [u64; 7]) -> squallar_egui::shell_api::PanesCuts {
        squallar_egui::shell_api::PanesCuts {
            setup_ns: seven[0],
            panel_ns: seven[1],
            resolve_ns: seven[2],
            widget_ns: seven[3],
            content_ns: seven[4],
            tools_ns: seven[5],
            credit_ns: seven[6],
        }
    }

    /// **The eight telescope to `panes`.** The residual is defined as the
    /// parent minus the seven named, so this cannot be an approximate
    /// equality and no reading of the split may need one: any `render_panes`
    /// time the seven do not name is *in* the eighth figure, not missing
    /// from the report. `the_post_phases_telescope_to_post`'s property, and
    /// `the_dispatch_cuts_telescope_to_dispatch`'s shape, since this family
    /// accumulates across a loop rather than bracketing instants.
    #[test]
    fn the_panes_cuts_telescope_to_panes() {
        let panes = 9_000;
        // **Not 6_400 for `content`**: `geodesy_one_definition` reads a bare
        // 6_400 in this tree as an earth radius in kilometres, and it read
        // this fixture's assertion as exactly that. The precedent is
        // `the_worst_frame_latch_keeps_the_largest_service` five hundred
        // lines up; the figure below is arbitrary and carries no geodesy.
        let eight = panes_cut_micros(
            panes_cuts([
                120_000, 40_000, 310_000, 900_000, 5_400_000, 30_000, 200_000,
            ]),
            panes,
        );
        assert_eq!(
            eight.iter().copied().fold(0u32, u32::wrapping_add),
            panes,
            "the eight cuts of `panes` do not sum to it: {eight:?}",
        );
        // And the residual is the one absorbing the difference, not a cut.
        assert_eq!(eight[..7], [120, 40, 310, 900, 5_400, 30, 200]);
        assert_eq!(eight[7], 2_000);
    }

    /// **Every one of the seven can move the answer, and moves exactly two
    /// figures.** Telescoping alone is satisfied by a degenerate split — one
    /// cut holding everything and six spelled as the same accumulator
    /// telescopes perfectly and decomposes nothing.
    /// `every_ui_stamp_is_load_bearing_in_two_cuts`' shape for an
    /// accumulator family: charge one field and its own cut rises by the
    /// charge while the residual falls by exactly that, and nothing else
    /// moves.
    #[test]
    fn every_panes_cut_is_load_bearing_against_the_residual() {
        let panes = 20_000;
        let flat = panes_cut_micros(panes_cuts([0; 7]), panes);
        assert_eq!(
            flat,
            [0, 0, 0, 0, 0, 0, 0, panes],
            "an unaccumulated frame does not file its whole parent under the \
             residual, so the residual is not the parent minus the named",
        );
        for slot in 0..7 {
            let mut ns = [0u64; 7];
            ns[slot] = 3_000_000;
            let moved = panes_cut_micros(panes_cuts(ns), panes);
            for other in 0..7 {
                let expect = if other == slot { 3_000 } else { 0 };
                assert_eq!(
                    moved[other], expect,
                    "charging cut {slot} moved cut {other}: {moved:?}. Two \
                     cuts reading one accumulator would telescope and \
                     decompose nothing",
                );
            }
            assert_eq!(
                moved[7],
                panes - 3_000,
                "cut {slot}'s charge did not come out of the residual, so \
                 the eight do not partition the parent",
            );
        }
    }

    /// **Sub-microsecond work is not rounded away**, and here that is a
    /// per-PANE claim rather than a per-request one:
    /// `many_sub_microsecond_visits_survive_into_the_cut_that_earned_them`
    /// one family across. Six panes each spending 700 ns resolving is 4.2 µs
    /// of a frame; converting per pane would have reported six zeros and
    /// handed the whole of it to the residual, which is read as "time the
    /// seven do not name".
    #[test]
    fn many_sub_microsecond_panes_survive_into_the_cut_that_earned_them() {
        let mut accumulated = squallar_egui::shell_api::PanesCuts::default();
        for _ in 0..6 {
            squallar_egui::shell_api::PanesCuts::charge_ns(&mut accumulated.resolve_ns, 700);
        }
        let eight = panes_cut_micros(accumulated, 20);
        assert_eq!(eight[2], 4, "resolve lost its sub-microsecond panes");
        assert_eq!(eight[7], 16, "the residual absorbed them instead");
    }

    /// **A span the cuts overrun reports no residual, and does not panic** —
    /// `cuts_that_overrun_their_span_report_a_zero_residual`, one family
    /// across. The parent is two clock reads and the seven are ten on a
    /// one-pane plan-view frame; a coarse or backward-stepping web clock can
    /// order them wrongly, and a frame-thread panic in an always-on
    /// instrument is a worse outcome than a zero.
    #[test]
    fn panes_cuts_that_overrun_their_span_report_a_zero_residual() {
        let eight = panes_cut_micros(panes_cuts([0, 0, 0, 0, 8_000_000, 0, 0]), 3_000);
        assert_eq!(eight[4], 8_000);
        assert_eq!(
            eight[7], 0,
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
