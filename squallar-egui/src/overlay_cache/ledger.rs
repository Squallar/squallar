//! What the whole-picture overlay pipeline has actually spent.
//!
//! **Product telemetry, not a campaign instrument.** Always on, no feature
//! gate, no debug arm. Every write is one `fetch_add` with [`Relaxed`]
//! ordering on a `static`; nothing here allocates, formats, locks or takes a
//! clock. The sentence that reports these numbers is written elsewhere — see
//! [`Totals`] — so no formatting happens on any path that increments.
//!
//! # The denominator
//!
//! **The overlay texture dispatch, and nothing else**: the ten layer kinds
//! `App::spawn_overlay_render` rasterizes. Radar declares
//! [`RenderMode::Texture`](squallar_source::handler::RenderMode) and keeps a
//! cache in the same map, but its raster comes off its own pipeline and is
//! refused by name at the overlay dispatch, so it is in none of these figures.
//! Neither are the loop frames, which never enter a pane's overlay cache.
//! The device's *total* upload cost, radar and font atlas included, is a
//! different instrument with a different denominator:
//! `squallar_gpu::egui_renderer::texture_upload::UploadTotals`. **The two are
//! never added together.**
//!
//! # Why a zero here is readable
//!
//! A counter that reads zero because the path never ran has to be
//! distinguishable from one that reads zero because the work was elided, or
//! the reading proves nothing. Three properties do that here, and each can
//! fail on its own:
//!
//! * [`Totals::dispatched`] is the floor. Zero means nothing ever asked for an
//!   overlay raster — no enabled texture layer, or no data behind one — and
//!   every figure below is then trivially zero. It is the conjunct that stops
//!   a byte reading passing vacuously, the way `out_moved > 0` does for the
//!   worker transport in `squallar_web`'s `worker_port`.
//! * [`Totals::arrived`] `==` [`Totals::pictures`] `+` [`Totals::dropped`] is
//!   an identity over the arrival path, not a hope: every response either
//!   reaches `Context::load_texture` or is thrown away before it, and the two
//!   are counted on the two sides of that branch. A count that stops adding up
//!   is a path that grew a third exit.
//! * [`Totals::picture_bytes`] is zero with [`Totals::pictures`] positive when
//!   every arrival was a **blank** — see [`Totals::inked`]. That is a reading
//!   and not a fault: a blank raster is charged no bytes because no buffer is
//!   built for it. Read with [`Totals::inked`] beside it, the pair separates
//!   "every layer drew nothing" from "pictures arrived and cost nothing",
//!   which cannot both be true.
//! * [`Totals::inked`] is zero with [`Totals::pictures`] positive when every
//!   picture that arrived **painted nothing**. That is a fourth distinct
//!   fault, and until 2026-08-31 nothing here could see it: a layer emitting a
//!   fully transparent pixmap satisfied every conjunct above, because
//!   [`note_picture`] counted the RGBA buffer whatever was in it. Measured on
//!   a deliberately emptied raster: 6 dispatched, 6 arrived, 6 pictures, and
//!   a map drawing nothing.
//!
//! # What a blank costs, since 2026-09-04
//!
//! `pictures - inked` is the count of arrivals that painted nothing, and each
//! of them now costs **no picture at all**: `has_ink` decides in the run
//! funnel's output stage, and where it answers `false` the buffer is given up
//! there — so on the web it never crosses the worker wire (6 bytes of reply
//! against the whole picture, pinned by `wire_identity::WIRE_REPLY_ROWS`), and
//! on every target the transparent `ColorImage` is never built, never handed
//! to `Context::load_texture` and never uploaded. What
//! reaches the pane is a `Blank` carrying the size the picture would have had,
//! and the pane clears on it exactly as it cleared on the transparent texture
//! — `OverlayTextureCache::show_blank`. **A blank is a clear, not a skip**: it
//! is what replaces the ink of a layer whose data has gone away, so a blank
//! that did not reach the pane would leave that ink on the glass while every
//! figure on this line improved.
//!
//! The share this removes, four Tier-2 legs on 2026-09-04 — **two targets,
//! never added and never averaged**: Firefox 368/1928 (19.1%) and 266/1856
//! (14.3%) blank at 8.92 MB a picture; Chromium 72/664 (10.8%) and 77/681
//! (11.3%) at 8.26 MB.
//!
//! Pinned by `the_ledger_separates_a_path_that_never_ran_from_one_that_moved_nothing`
//! and `every_arrival_is_either_a_picture_or_a_drop`.
//!
//! # What "ink" is, and why it is a count rather than a coverage
//!
//! [`has_ink`] is the whole definition, and it is exact rather than a
//! heuristic: egui's `Color32` is **premultiplied**, so a pixel that would
//! change nothing under `dst = src + dst·(1−src.a)` is zero in all four bytes.
//! "Some byte of this buffer is non-zero" is therefore precisely "some pixel
//! of this picture would alter the frame it is drawn on".
//!
//! A *coverage* figure — inked pixels over total pixels — was the other
//! candidate and was not taken. It cannot short-circuit, so it walks every
//! picture in full whatever is in it, where the predicate stops at the first
//! non-zero byte and only pays the whole pass for the blank picture it exists
//! to catch. And its denominator is not one this line already carries:
//! [`Totals::picture_bytes`] is a sum over pictures of different sizes, so a
//! ratio against it is an aggregate over an unstated mix rather than "how much
//! of a picture is ink". [`Totals::inked`] has exactly one denominator,
//! [`Totals::pictures`], and `inked <= pictures` always.

use super::RerenderReason;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

/// Every counter on this line, in one object.
///
/// **Fields rather than loose statics so that there can be more than one set
/// of them**, which is the whole of the isolation described on `sink`.
/// Production keeps exactly one, in a `static` of this type, so each `note_*`
/// below is the same single relaxed `fetch_add` on the same address as the
/// loose static it replaced.
struct Counters {
    /// Overlay rasters asked for. See [`note_dispatched`].
    dispatched: AtomicU64,
    /// [`Self::dispatched`] split by [`RerenderReason`], indexed by
    /// [`RerenderReason::index`]. Written by the same call that writes
    /// [`Self::dispatched`], so the two cannot drift — see
    /// [`Totals::reasons_balance`].
    reasons: [AtomicU64; RerenderReason::COUNT],
    /// The blank arrivals split by [`BlankReason`], indexed by
    /// [`BlankReason::index`]. Written by [`note_blank`], the same call that
    /// counts the blank as a picture, so the two cannot drift — see
    /// [`Totals::blank_reasons_balance`].
    blank_reasons: [AtomicU64; BlankReason::COUNT],
    /// Rasterized responses received. See [`note_arrived`].
    arrived: AtomicU64,
    /// Responses thrown away before their pixels were handed over.
    dropped: AtomicU64,
    /// Pictures handed to egui.
    pictures: AtomicU64,
    /// Bytes of those pictures.
    picture_bytes: AtomicU64,
    /// Of those pictures, the ones that had any ink in them. See [`has_ink`].
    inked: AtomicU64,
    /// Pictures put straight on screen.
    shown: AtomicU64,
    /// Pictures that reached the screen after their last band landed.
    promoted: AtomicU64,
    /// Uploads discarded mid-flight by a newer picture for the same
    /// destination.
    superseded: AtomicU64,
    /// Dispatches withdrawn at the supersede seam before their answer was
    /// used.
    cancelled: AtomicU64,
    /// The last [`Totals::progress`] a caller was handed by
    /// [`totals_if_moved`].
    reported: AtomicU64,
}

impl Counters {
    const fn new() -> Self {
        Self {
            dispatched: AtomicU64::new(0),
            reasons: [const { AtomicU64::new(0) }; RerenderReason::COUNT],
            blank_reasons: [const { AtomicU64::new(0) }; BlankReason::COUNT],
            arrived: AtomicU64::new(0),
            dropped: AtomicU64::new(0),
            pictures: AtomicU64::new(0),
            picture_bytes: AtomicU64::new(0),
            inked: AtomicU64::new(0),
            shown: AtomicU64::new(0),
            promoted: AtomicU64::new(0),
            superseded: AtomicU64::new(0),
            cancelled: AtomicU64::new(0),
            reported: AtomicU64::new(0),
        }
    }
}

/// The counters this thread writes to and reads from.
///
/// # Production: one set, shared, exactly as before
///
/// A single `static`, so every figure on the line is the process's — which is
/// what the reported sentence means. Nothing about the increments changed.
///
/// # A test build: one set per thread, and why
///
/// `cfg(test)`, and the `test-support` feature `squallar-app` turns on through
/// its dev-dependency (the same edge it reaches `squallar-radar`'s
/// state-forcing hooks on), replace that one shared set with a **per-thread**
/// one.
///
/// A test binary runs its tests concurrently in one process, so a counter
/// every test shares is a counter every test writes. Counted on this tree on
/// 2026-09-06, per test file in `squallar-app/src`, as *sites that took the
/// crate-wide lock this replaced* : *sites that dispatch or paint*:
/// `gmgsi_loop_tests` 0:26, `satellite_loop_draw_tests` 0:23,
/// `loop_overlay_render_tests` 0:19, `frame_build_order_tests` 0:11,
/// `app_render/tests.rs` 0:10, `frame_thread_conversion_tests` 0:9,
/// `raster_hold_tests` 0:6, and nine more — fifteen files writing these
/// counters under no lock at all, against eight reading them under one.
/// **A lock only readers take protects nothing**: production dispatch and
/// paint are what write, so a sibling merely painting landed in a reader's
/// figure. It surfaced as
/// `rebuild_reason_tests::the_dispatch_reason_separates_a_pan_from_a_data_arrival`
/// charging one of its fifteen dispatches to the coverage arm across a phase
/// in which its map never moved — under `cargo test --workspace`, never under
/// the filtered run.
///
/// Per-thread counters end that by deleting the object the two tests shared:
/// there is no path from one thread's `note_*` to another thread's figures.
/// libtest gives each test its own thread, so a reader is isolated whether or
/// not its author knew to arrange anything — which is why the crate-wide
/// `overlay_ledger_lock` this replaced is **deleted** rather than kept beside
/// it. Keeping it would teach the next author that the lock is the mechanism.
///
/// # The premise, written down because the design rests on it
///
/// Thread-locality is reader-locality **only because every write below happens
/// on the frame thread**, which in these fixtures is the test's own thread.
/// All eleven call sites, workspace-wide on 2026-09-06:
///
/// * `squallar-egui/src/overlay_cache.rs:834` — [`note_dispatched`], in
///   `RendersInFlight::record`, whose one production caller is
///   `App::spawn_overlay_render` (`squallar-app/src/app_fetch.rs:1216`),
///   reached from `App::dispatch_overlay_renders` (`app.rs:1977`, under
///   `process_gui_actions`) and `App::dispatch_overlay_loop_renders`
///   (`app_render.rs:5856`).
/// * `squallar-app/src/app_fetch.rs:1391` — [`note_cancelled`], in that same
///   `App::spawn_overlay_render`, at the supersede seam.
/// * `squallar-app/src/app_render.rs` 2705, 2736, 2744, 2768, 2778, 2818,
///   2835, 2843 — the eight arrival counters, all in
///   `App::poll_overlay_render_results`, whose caller
///   `pump_poll_overlay_render_results` (`app_render.rs:8036`) is an Apply row
///   run from `setup_egui_frame`.
/// * `squallar-egui/src/pane.rs:3354` — [`note_promoted`], in
///   `PaneState::promote_held_overlays`, reached from
///   `Gui::promote_held_rasters` (`squallar-egui/src/ui.rs:2383`), called by
///   `App::promote_uploaded_rasters` (`app_render.rs:1876`) and
///   `App::deliver_held_rasters` (`app_render.rs:2393`).
///
/// **If a future `note_*` call is added on a worker, a rayon thread or a
/// task, a test build silently stops counting it.** The reader's figure comes
/// back short with nothing at the call site to say why, so that is the one
/// thing to check when a counter is added here. Nothing in this module can
/// detect it.
///
/// # What this weakens
///
/// An assertion of the shape "and no more dispatches happened" can only ever
/// observe **this thread's** writes, so it can no longer catch a production
/// path that dispatches from another thread. Under the premise above there is
/// no such path — which is exactly why the premise is stated rather than
/// assumed.
#[cfg(not(any(test, feature = "test-support")))]
fn sink() -> &'static Counters {
    static SHARED: Counters = Counters::new();
    &SHARED
}

/// One set of counters per thread — the production arm above carries the whole
/// account of why.
#[cfg(any(test, feature = "test-support"))]
fn sink() -> &'static Counters {
    thread_local! {
        /// Leaked rather than borrowed, so that this arm hands back the same
        /// `&'static Counters` the production arm does and every `note_*`
        /// body below stays one spelling. One `Counters` per thread that
        /// touches this ledger at all; the process exit frees them.
        static OWN: &'static Counters = Box::leak(Box::new(Counters::new()));
    }
    OWN.with(|counters| *counters)
}

/// A reading of the counters below, taken together.
///
/// **Reported, never gated on, by this crate.** `squallar-egui` is the UI layer
/// and has no frame of its own to report on; the running-total line is written
/// by `squallar-app` once a frame, from [`totals`], on the same terms
/// `squallar_volumetric::degrade` already uses for the surface-loss count. That
/// keeps every increment here free of a `log::` call and therefore free of
/// formatting.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Totals {
    /// Rasters asked for — one per [`RendersInFlight::record`], which is the
    /// mark the dispatch sets and the admission bound counts.
    ///
    /// [`RendersInFlight::record`]: super::RendersInFlight::record
    pub dispatched: u64,
    /// Rasterized responses that came back. `dispatched > 0` with this at zero
    /// is a dispatch path whose answers never arrive.
    pub arrived: u64,
    /// Of [`Self::arrived`], those thrown away before `Context::load_texture`.
    /// **Bytes that were rasterized and never reached the GPU.**
    ///
    /// **A zero here is the ordinary reading, not a dead path**, and the doc
    /// used to leave that unsaid — it reads zero on every Tier-2 browser leg.
    /// What raises it is an event, and each one is something a scene has to
    /// *do*:
    ///
    /// * a pane closed while a raster was in the air — `Gui::close_pane`
    ///   abandons every mark from that index up, so the answer arrives stale;
    /// * the layer was switched off while its raster flew
    ///   (`PaneState::overlay_texture_is_releasable`);
    /// * the egui context died and the caches were released with rasters out;
    /// * the render produced no picture at all — a worker that died, a reply
    ///   of the wrong length, a hit map that does not fit its dispatch, or a
    ///   job withdrawn at the WO-8 cancel seam.
    ///
    /// **What cannot raise it is a pane that merely moved.** The doc used to
    /// name that first, and both dispatch doors refuse it: `ui_map_pane`'s
    /// draw-loop ask and `App::arrived_overlay_asks` each gate on
    /// [`RendersInFlight::admits`], which declines a second dispatch for a
    /// destination that already has a raster out — so a live pane never has
    /// two whole-picture rasters in the air to supersede one another, and
    /// [`Self::cancelled`] is zero for the same reason. A leg that pans for a
    /// minute and reads zero here is reading correctly.
    ///
    /// [`RendersInFlight::admits`]: super::RendersInFlight::admits
    pub dropped: u64,
    /// Of [`Self::arrived`], those whose pixels were handed to egui. One per
    /// response, not one per pane: a grouped dispatch is one upload shared by
    /// every pane that asked, and counting it per pane would inflate the byte
    /// figure by the pane count.
    pub pictures: u64,
    /// Bytes of [`Self::pictures`] — `width × height × 4`, the picture's own
    /// size, taken at the call that hands the pixels over.
    ///
    /// **This is the size of the buffer, never a statement about what is in
    /// it** — and a blank arrival has no buffer, so it adds nothing here. That
    /// zero is exact rather than a convention: nothing was allocated, nothing
    /// crossed to `Context::load_texture` and nothing was uploaded. Before
    /// 2026-09-04 a picture of 8 MB of fully transparent pixels reported 8 MB
    /// here, which is why [`Self::inked`] exists beside it and how the waste
    /// was found.
    pub picture_bytes: u64,
    /// Of [`Self::pictures`], those with at least one non-zero byte — pictures
    /// that would actually change the frame they were drawn on. `inked <
    /// pictures` is layers rasterizing blank; `inked == 0` with `pictures > 0`
    /// is **every** overlay painting nothing, which every other figure on this
    /// line reports identically to a healthy run.
    ///
    /// Decided by [`has_ink`] in the run funnel's output stage, off the frame
    /// thread on both targets,
    /// never on the frame thread: see `App::overlay_job_deliver`, and
    /// `no_poller_unmultiplies_on_the_frame_thread` for the rule. **That same
    /// answer is what decides whether a picture is built at all**, so
    /// `pictures - inked` counts arrivals that cost no buffer, no texture and
    /// no upload — see the module note.
    pub inked: u64,
    /// Pictures put straight on screen, because the pane was drawing nothing
    /// for that layer yet.
    pub shown: u64,
    /// Pictures that were held until every band had landed and then shown.
    pub promoted: u64,
    /// Pictures handed to a cache that was **already** holding one, so an
    /// upload that had started was thrown away before it could be drawn. This
    /// is the quantity the `PAN_REBUILD_THRESHOLD` sweep calls a discarded
    /// hold, and the one a world-anchored tile grid is meant to make rare.
    ///
    /// **Reading this beside `OverlayTextureCache::hold_superseded`.** They
    /// are not two denominators over one population, which is the natural
    /// guess and is wrong. The two rise on the *same* condition at the *same*
    /// instant: the overlay arrival asks `is_holding()` and calls
    /// [`note_superseded`] immediately before the `hold()` whose `|=` sets the
    /// flag. What differs is coverage, and this figure is the **smaller** one
    /// — radar's own arrival holds through `PaneState::place_radar_raster`
    /// without coming past this counter, so the flag can be set where this
    /// never increments.
    ///
    /// So a native leg reading this in the hundreds beside a fixture reading
    /// **zero** is not a denominator mismatch to reconcile: it is a
    /// **synchronous fixture**. One whose delivery empties the hold before the
    /// next arrival can never reproduce the condition, and a conclusion drawn
    /// from it about an asynchronous pipeline does not hold. That mistake has
    /// been made on this counter once already.
    pub superseded: u64,
    /// [`Self::dispatched`] split by the arm that asked for it, indexed by
    /// [`RerenderReason::index`]. Read it through [`Self::reason`].
    ///
    /// **Its denominator is [`Self::dispatched`] and nothing else**, and
    /// [`Self::reasons_balance`] is that identity rather than an expectation:
    /// the same call writes both. An ask that `RendersInFlight::admits`
    /// refused appears in neither — it spent no raster — so this is a
    /// breakdown of rasters *spent*, never of rasters *wanted*.
    pub reasons: [u64; RerenderReason::COUNT],
    /// **The blanks split by why they were blank**, indexed by
    /// [`BlankReason::index`]. Read it through [`Self::blank_reason`].
    ///
    /// Its denominator is `pictures - inked` and nothing else — the arrivals
    /// that painted nothing — and [`Self::blank_reasons_balance`] is that
    /// identity rather than an expectation, because [`note_blank`] writes both
    /// sides.
    ///
    /// **This is the line that separates a saving from a loss.** A blank is a
    /// clear: the pane stops drawing the layer. `pictures - inked` on its own
    /// says only how often that happened, which reads as waste avoided; what
    /// it cannot say is whether the layer still covered the view when it
    /// happened, and that is the difference between a correct clear and a user
    /// watching loaded data disappear. See [`Self::blanks_over_covered_ground`].
    pub blank_reasons: [u64; BlankReason::COUNT],
    /// Of [`Self::dispatched`], those withdrawn at the supersede seam (WO-8)
    /// before their answer was used: a newer dispatch replaced every
    /// destination the raster was for, so the job was cancelled at the
    /// offload registry — unrun where it had not started, its answer refused
    /// where it had. Each one still **arrives** (the withdrawal delivers
    /// "nothing", which the arrival path drops), so
    /// [`Self::arrivals_balance`] holds unchanged; what this figure names is
    /// raster work the pipeline declined to spend, not a fourth arrival exit.
    pub cancelled: u64,
}

impl Totals {
    /// Whether the overlay dispatch has run at all. **The non-vacuity floor**:
    /// every other figure is zero when this is false, so a zero read without
    /// this one cannot be told from a path that never executed.
    pub fn ran(&self) -> bool {
        self.dispatched > 0
    }

    /// Whether every arrival is accounted for on exactly one side of the
    /// upload branch. False means the arrival path grew an exit neither
    /// [`Self::pictures`] nor [`Self::dropped`] counts.
    pub fn arrivals_balance(&self) -> bool {
        self.arrived == self.pictures + self.dropped
    }

    /// Whether every dispatch is accounted for by exactly one reason. False
    /// means a `record` reached the ledger without passing through
    /// [`note_dispatched`], which is the only way the two can disagree.
    ///
    /// **What this cannot detect: whether the reasons are the right ones.** It
    /// is a conservation law, so it proves nothing is lost and nothing about
    /// what anything is called. Measured: a tamper that swapped both
    /// `PanCoverage` arming sites to `ContentOneShot` made a pan phase spend 45
    /// rasters charged 45-to-content and 0-to-pan, and this returned `true`
    /// throughout with zero unattributed. Only an assertion naming the arm it
    /// expects, in both directions, catches a misattribution. "The balance line
    /// is green" is not a health check.
    pub fn reasons_balance(&self) -> bool {
        self.reasons.iter().sum::<u64>() == self.dispatched
    }

    /// How many rasters `reason` asked for.
    pub fn reason(&self, reason: RerenderReason) -> u64 {
        self.reasons[reason.index()]
    }

    /// How many blank arrivals were blank for `reason`.
    pub fn blank_reason(&self, reason: BlankReason) -> u64 {
        self.blank_reasons[reason.index()]
    }

    /// The blanks account for every arrival that painted nothing.
    ///
    /// An identity and not an expectation: [`note_blank`] is the only writer
    /// of either side. It reading false is a picture counted by one path and
    /// not the other, which is a hole in this wiring.
    pub fn blank_reasons_balance(&self) -> bool {
        self.blank_reasons.iter().sum::<u64>() == self.blanks()
    }

    /// Arrivals that painted nothing: [`Self::pictures`] less [`Self::inked`].
    ///
    /// `saturating_sub` because this type is public and a hand-built `Totals`
    /// — the telemetry line tests build one whose fields are deliberately
    /// implausible, so a transposition cannot read as correct — may state more
    /// ink than pictures. A debug overflow there would be this accessor
    /// asserting a fixture's realism, which is not its job.
    pub fn blanks(&self) -> u64 {
        self.pictures.saturating_sub(self.inked)
    }

    /// How many distinct blank reasons were observed at all.
    ///
    /// **The anti-vacuity conjunct for this breakdown**, on the same terms as
    /// [`Self::distinct_reasons`]: a counter that only ever answered one
    /// variant satisfies every other check here and says nothing.
    pub fn distinct_blank_reasons(&self) -> usize {
        self.blank_reasons.iter().filter(|n| **n > 0).count()
    }

    /// **Blanks not shown to be correct** — every reason but
    /// [`BlankReason::OutsideCoverage`], which is the only one proven right by
    /// an observation rather than a claim. Conservative on purpose; see
    /// [`BlankReason::clears_covered_ground`] for why a handler's own
    /// `paints_in` refusal is counted here despite looking correct.
    ///
    /// This is the figure a correctness reading of the blank rate wants, and
    /// it did not exist before 2026-09-06. `pictures - inked` was measured at
    /// 37–63 % of dispatches on two browser legs and filed as *waste*; read as
    /// "a picture disappeared from under the user whenever the previous one
    /// had ink", the same number is a *correctness* figure. Which of the two
    /// readings a given run deserves is exactly what this splits out, and
    /// nothing could answer it while the reason went unrecorded.
    pub fn blanks_over_covered_ground(&self) -> u64 {
        BlankReason::ALL
            .iter()
            .filter(|r| r.clears_covered_ground())
            .map(|r| self.blank_reasons[r.index()])
            .sum()
    }

    /// How many distinct reasons were observed at all.
    ///
    /// **The anti-vacuity conjunct for this breakdown.** A reason counter that
    /// always answered the same variant satisfies every other check here —
    /// `dispatched` is positive, the sum balances, each figure is readable —
    /// and says nothing. A scene that really does move the view *and* deliver
    /// data must read at least two.
    pub fn distinct_reasons(&self) -> usize {
        self.reasons.iter().filter(|n| **n > 0).count()
    }

    /// Rasters the **view moving** asked for — see
    /// [`RerenderReason::is_view_driven`], which excludes a density or pane
    /// resize on purpose.
    pub fn view_driven(&self) -> u64 {
        self.sum_where(RerenderReason::is_view_driven)
    }

    /// Rasters a **wider oversampling margin could have prevented** — see
    /// [`RerenderReason::is_margin_avoidable`]. This is the figure that prices
    /// the margin, and its denominator is [`Self::dispatched`].
    pub fn margin_avoidable(&self) -> u64 {
        self.sum_where(RerenderReason::is_margin_avoidable)
    }

    fn sum_where(&self, pred: fn(RerenderReason) -> bool) -> u64 {
        RerenderReason::ALL
            .iter()
            .filter(|r| pred(**r))
            .map(|r| self.reasons[r.index()])
            .sum()
    }

    /// Pictures that reached the screen, by either route.
    pub fn on_screen(&self) -> u64 {
        self.shown + self.promoted
    }

    /// How far along this ledger is, as one number, so a caller can tell
    /// "nothing has happened since I last looked" in a single compare.
    fn progress(&self) -> u64 {
        self.dispatched + self.arrived + self.on_screen() + self.superseded + self.cancelled
    }
}

/// Record that an overlay raster was asked for, and **why**.
///
/// Two relaxed `fetch_add`s and one `match` on a fieldless enum; nothing here
/// allocates, formats or takes a clock, so it stays as free on the frame
/// thread as the single counter it replaced.
pub fn note_dispatched(reason: RerenderReason) {
    let sink = sink();
    sink.dispatched.fetch_add(1, Relaxed);
    sink.reasons[reason.index()].fetch_add(1, Relaxed);
}

/// Record that a rasterized response arrived.
pub fn note_arrived() {
    sink().arrived.fetch_add(1, Relaxed);
}

/// Record that an arrival was thrown away before its pixels were handed over.
pub fn note_dropped() {
    sink().dropped.fetch_add(1, Relaxed);
}

/// Whether any pixel of a premultiplied RGBA buffer would change the frame it
/// is drawn on — **defined one crate down**, in
/// [`squallar_overlays::render::rasterize`], and re-exported here.
///
/// It moved there on 2026-09-04 and its prose stayed: this is the module that
/// reports the reading, and the module note above is where the reading is
/// explained. What moved is where the answer is *used*. The reply codec has to
/// know whether a picture is worth putting on the wire at all, and the codec
/// sits below this crate; a second spelling of the predicate down there could
/// answer differently from this one, which is a picture uploaded against a
/// pane that was told to clear.
///
/// Never called from a poller — see `no_poller_unmultiplies_on_the_frame_thread`.
/// The one production caller is the run funnel's output stage, off the frame
/// thread on both targets.
pub use squallar_overlays::render::rasterize::has_ink;

/// Re-exported for the same reason [`has_ink`] is: the answer is decided below
/// the wire, in the crate that rasterizes, and a second spelling of it here
/// could disagree with the one the reply carries.
pub use squallar_overlays::render::rasterize::BlankReason;

/// Record `bytes` of picture handed to egui, and whether [`has_ink`] found
/// anything in it.
///
/// **`bytes` is zero for a blank arrival**, which is not the same event as a
/// drop: the arrival reached a pane and cleared it. See the module note.
pub fn note_picture(bytes: u64, inked: bool) {
    let sink = sink();
    sink.pictures.fetch_add(1, Relaxed);
    sink.picture_bytes.fetch_add(bytes, Relaxed);
    if inked {
        sink.inked.fetch_add(1, Relaxed);
    }
}

/// Record a **blank** arrival: one that reached a pane, cleared it, and cost
/// no buffer, no texture and no upload — and **why** it was blank.
///
/// [`note_picture`] with `bytes` of zero and `inked` false, plus the reason,
/// written by this one call so the blank count and its breakdown cannot drift
/// (`Totals::blank_reasons_balance`). Three relaxed `fetch_add`s and a `match`
/// on a fieldless enum; nothing here allocates or takes a clock.
pub fn note_blank(reason: BlankReason) {
    let sink = sink();
    sink.pictures.fetch_add(1, Relaxed);
    sink.blank_reasons[reason.index()].fetch_add(1, Relaxed);
}

/// Record a picture put straight on screen.
pub fn note_shown() {
    sink().shown.fetch_add(1, Relaxed);
}

/// Record a held picture that reached the screen.
///
/// **Also an arrival door for the unnecessary-frame verdict**, and one that
/// the channel drains cannot cover: a picture's last band arrives on frame N
/// and the band-complete sweep puts it on the glass at the head of frame N+1,
/// so the frame that actually shows it takes no message of its own. Without
/// this, the frame a picture appears on would be scored unnecessary. See
/// [`crate::frame_need`].
pub fn note_promoted() {
    sink().promoted.fetch_add(1, Relaxed);
    crate::frame_need::note(crate::frame_need::NeedCause::Arrival);
}

/// Record an upload thrown away mid-flight by a newer picture.
pub fn note_superseded() {
    sink().superseded.fetch_add(1, Relaxed);
}

/// Record a dispatch withdrawn before its answer was used.
pub fn note_cancelled() {
    sink().cancelled.fetch_add(1, Relaxed);
}

/// Read every counter.
///
/// Nine [`Relaxed`] loads, and they are **not** an atomic snapshot: a
/// concurrent increment can land between two of them. That is deliberate and
/// harmless — every one of these is written from the frame thread, and a
/// reader that has to lock to be exactly right would be paying for a
/// consistency the numbers do not need.
pub fn totals() -> Totals {
    let sink = sink();
    Totals {
        dispatched: sink.dispatched.load(Relaxed),
        arrived: sink.arrived.load(Relaxed),
        dropped: sink.dropped.load(Relaxed),
        pictures: sink.pictures.load(Relaxed),
        picture_bytes: sink.picture_bytes.load(Relaxed),
        inked: sink.inked.load(Relaxed),
        shown: sink.shown.load(Relaxed),
        promoted: sink.promoted.load(Relaxed),
        superseded: sink.superseded.load(Relaxed),
        cancelled: sink.cancelled.load(Relaxed),
        reasons: std::array::from_fn(|i| sink.reasons[i].load(Relaxed)),
        blank_reasons: std::array::from_fn(|i| sink.blank_reasons[i].load(Relaxed)),
    }
}

/// [`totals`], but only when something has happened since the last time this
/// was asked — so a caller can write the line on a frame where the pipeline
/// moved and stay silent on one where it did not.
///
/// An idle frame costs nine relaxed loads and one compare-exchange that
/// fails.
pub fn totals_if_moved() -> Option<Totals> {
    let totals = totals();
    let progress = totals.progress();
    if sink().reported.swap(progress, Relaxed) == progress {
        return None;
    }
    Some(totals)
}

/// Put this thread's counters back to zero.
///
/// **For tests only, and the reason this is not `#[cfg(test)]`**: the counters
/// live one crate below most of the tests that drive them, and a `cfg(test)`
/// is crate-local.
///
/// In a test build there is one set of counters per thread — see `sink` — so
/// this zeroes the calling test's own and no other's. It is still needed:
/// under `--test-threads=1` libtest runs every test on the main thread, so one
/// set of counters spans the whole run and a test asserting an absolute figure
/// has to start it from zero.
#[doc(hidden)]
pub fn reset_for_test() {
    let sink = sink();
    for counter in [
        &sink.dispatched,
        &sink.arrived,
        &sink.dropped,
        &sink.pictures,
        &sink.picture_bytes,
        &sink.inked,
        &sink.shown,
        &sink.promoted,
        &sink.superseded,
        &sink.cancelled,
        &sink.reported,
    ] {
        counter.store(0, Relaxed);
    }
    for counter in &sink.reasons {
        counter.store(0, Relaxed);
    }
    // **The blank breakdown too, and it was missed when it landed.** The array
    // was added beside `reasons` and not to this loop, so under
    // `--test-threads=1` — where libtest runs every test on one thread and one
    // set of counters spans the whole run — a test asserting an absolute
    // blank-reason figure read the previous test's blanks on top of its own.
    // It survived because `blank_reasons_balance` held either way: `pictures`
    // *was* reset, so a leaked reason count made the identity fail rather than
    // read wrong, and no suite had yet asserted an absolute per-reason count.
    for counter in &sink.blank_reasons {
        counter.store(0, Relaxed);
    }
}
