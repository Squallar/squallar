//! The typed Gui↔App seam: [`FrameInputs`] for snapshot-shaped facts
//! the App owns and re-states every frame, [`GuiEvent`] for event-shaped
//! pushes applied at the call site's existing control-flow position. The
//! out-direction, `GuiAction`, already had this shape and lives in
//! [`crate::actions`]. The re-verbed `GuiAction` lands here at E5.

use squallar_device_profile::fit::{NeedTerms, PaneTerms};
use squallar_device_profile::hist::Hist;
use squallar_device_profile::scene::CapacitySource;
use squallar_radar::types::ScanInfo;
use squallar_source::id::LayerId;

/// **What the scene costs and what the machine holds, as the App last priced
/// them** — the budget system's readout, composed in `squallar-app` and
/// crossing here as one field of [`FrameInputs`]. The UI reads it and paints
/// it; it prices nothing, because pricing is `fit::need` over a `Scene` the
/// App alone can describe (the loop aliasing, the tile ledger, the planned
/// picture sizes).
///
/// **Composed on its consumer's cadence, not the frame's.** The App builds
/// it inside `App::report_frame_telemetry`, the 2 s tick that prints
/// `budget state:` — every input it reads is a *level* (bytes resident now,
/// the heap as last sampled), so there is no scene-change trigger that would
/// make it fresh, and a level printed every two seconds does not need a
/// figure taken 120 times a second to say it. The frame path composes
/// nothing. See [`Self::generation`] for what a consumer compares.
///
/// Two families of figure, never added: the **priced** terms
/// (`fit::need_terms_for_pane` per pane, `fit::need_terms` for the whole)
/// are what the scene *costs* at the budgets in force — a loop's decoded
/// volumes at their measured size where resident and at the reserve where
/// still to come; the **held** figures (`shared` / `own`) are what a pane's
/// stores are *holding* now, read off the refcounted stores and the loop
/// pool's grants. Every byte figure is bytes; the telemetry line divides.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BudgetReadout {
    /// **Which composition this is** — the App bumps it once per rebuild and
    /// never otherwise, so a reader that holds a copy asks "has this moved?"
    /// with one integer compare instead of walking the pane vector and the
    /// grid list. `0` is the readout a fresh application carries before its
    /// first composition; the first real one is `1`.
    ///
    /// It is part of `PartialEq` on purpose: two readouts with the same
    /// figures composed at different times are not interchangeable to a
    /// consumer that caches by generation.
    pub generation: u64,
    /// One entry per visible pane, in pane order.
    pub panes: Vec<PaneBudget>,
    /// **One row per layer of each pane that costs the model anything**, in
    /// pane order and outer-indexed exactly like [`Self::panes`] — the layers
    /// menu's own read-out. Empty inner vectors are normal: a pane whose
    /// layers are all point-drawn charges no term any row could carry.
    pub pane_layers: Vec<Vec<LayerBudget>>,
    /// The whole scene's terms — the panes folded plus the scene-level
    /// terms: mirror, tiles, the arrival, the overlay grids.
    pub terms: NeedTerms,
    /// The GPU pool.
    pub gpu: PoolReadout,
    /// The host pool, where the session has a host figure at all — a
    /// browser's declared heap, a measured RAM reading. `None` on a native
    /// arm no reader has answered for, where nothing is ever over.
    pub host: Option<PoolReadout>,
    /// **Every gridded overlay layer enabled on any pane**, with the host
    /// budget it is priced at — the per-layer read-out of
    /// `NeedTerms::overlay_grids_host`, keyed for the layers menu.
    pub overlay_grids: Vec<(LayerId, u64)>,
    /// **The texture-size setting's own effective-beside-requested pair** —
    /// what the user allows a radar raster's side, and what one may actually
    /// reach on this device.
    pub raster: RasterSideReadout,
}

/// **What the user allows any radar raster's side, and the largest side one
/// may actually take here** — [`PoolReadout::effective_percent`]'s argument
/// for a ceiling in pixels rather than a share of a pool.
///
/// The pair exists for one reason: the setting is a ceiling the hardware may
/// clamp below, and a control that shows only what was asked for cannot say
/// so. A user who lowered this months ago and forgot, and a user whose
/// adapter will not reach the size they picked, are told the same way — the
/// figure they asked for, beside the figure in force.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RasterSideReadout {
    /// **The ceiling the user asked for**, verbatim.
    /// `TextureCeiling::NONE` — the default — is no restriction at all, and
    /// is what a session that never touched the control carries.
    pub requested: squallar_device_profile::budget::TextureCeiling,
    /// **The largest side a plan-view raster may actually take this
    /// session**: `Budgets::raster_side_for_adapter` at this device's
    /// `max_texture_dimension_2d`, which is the figure the render dispatcher
    /// is held to and therefore the one whose effect the user sees.
    ///
    /// It can never exceed [`Self::requested`], because the ceiling is
    /// applied to every raster side in `Budgets` before that function reads
    /// them; it is *below* it whenever the device class, the fit's ladder or
    /// the adapter's own limit reaches lower.
    ///
    /// **`None` is absence, not zero and not a guess**: before a device has
    /// answered there is no adapter figure to hold anything to, and
    /// `memory_share_caption`'s ruling applies — a session with no figure is
    /// told so rather than shown an invented one.
    pub effective_side_px: Option<usize>,
}

/// One pane's priced cost and held bytes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PaneBudget {
    /// What the pane costs, term by term, at the budgets in force —
    /// `fit::need_terms_for_pane`, so a pane on a shared loop prices at its
    /// own cost and no more.
    pub terms: PaneTerms,
    /// **Bytes this pane holds that another pane holds too**: a 3D pane's
    /// grids in the volume store held by more than one pane, and a 2D loop's
    /// grant where the pane is an alias of another pane's loop or has one.
    /// Displayed, never attributed — ruling 8's `shared N`.
    pub shared_bytes: u64,
    /// **Bytes this pane holds alone**: the rest of the same two figures. The
    /// loop frame store's picture sharing beyond the aliasing (two unlinked
    /// panes whose windows overlap) is counted on `loop state:`'s `shared`
    /// and not folded in here — a different denominator.
    pub own_bytes: u64,
    /// **Frames this pane's loop ASKED for**: its own lookback at its
    /// cadence, which nothing of the budget model's shortens (ruling 13).
    /// `0` for a pane that is not looping.
    pub loop_frames_requested: usize,
    /// **Frames it gets**: [`Self::loop_frames_requested`] held to what this
    /// session's capacity was measured to reach
    /// (`squallar_device_profile::budget::Budgets::loop_frames_reachable`).
    ///
    /// Below the request only where the machine cannot reach the span, and
    /// then the pair is what makes the clamp **visible** — ruling 13's *"a
    /// looping pane or layer that does not fit at full span is refused at
    /// admission, visibly"*. The admission door itself is not built here; a
    /// readout that names the two figures is what stands in for it.
    pub loop_frames_effective: usize,
    /// **What this pane costs the pool that binds it, and the room it has
    /// there** — the frame overlay's whole line. See [`Charge`].
    pub charge: Charge,
}

impl PaneBudget {
    /// Whether this pane's loop was held below what its span asked for.
    pub fn loop_span_clamped(&self) -> bool {
        self.loop_frames_effective < self.loop_frames_requested
    }
}

/// **One layer of one pane, as the model prices it** — the layers menu's row
/// figure.
///
/// The pair is the *priced* family, split by **who is charged for it**, which
/// is the split ruling 8 asks to be displayed rather than attributed:
/// [`Self::shared_bytes`] is what the application pays once however many panes
/// show the layer, [`Self::own_bytes`] what this pane pays on top. It is not
/// the held family [`PaneBudget::shared_bytes`] carries — that one is bytes
/// resident in the two refcounted stores at the instant of the read — and the
/// two are never added.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LayerBudget {
    /// Which layer's row this is.
    pub layer: LayerId,
    /// **Bytes the application pays once for this layer, whatever pane shows
    /// it**: a gridded overlay's decoded source grid (one handler instance
    /// app-wide, so shared by construction), a tile role's working set, and a
    /// loop the plan aliased onto another pane's.
    pub shared_bytes: u64,
    /// **Bytes this pane pays on top**: its own whole-picture raster and the
    /// copy of it the upload queue holds, its own loop's frames where the plan
    /// did not alias them, and radar's own rasters and decoded volumes.
    ///
    /// **Never zero for a shown texture overlay**: the picture is per pane by
    /// construction.
    pub own_bytes: u64,
    /// The pool [`Self::shared_bytes`] and [`Self::own_bytes`] are in, and the
    /// room this layer has in it. The pool is the one binding the layer's own
    /// **pane**, so one menu carries one denominator — and the pane's frame
    /// overlay is where that denominator is named.
    pub charge: Charge,
}

/// **What one pane or one layer is charged, in one named pool, against the
/// room it has there.**
///
/// # Why the allowance is a difference and not a share
///
/// The user's ruling is that *"a 1 pane window should have the same overall
/// pane budget as a 6 pane window … more panes open just slice that whole pane
/// budget slimmer. and honestly, no reason for it to be equal slices either.
/// let panes have as much ram as they need"*
/// (`docs/cross-platform-resource-limits.md` §9.7). So there is no per-pane
/// allowance to divide out, and an equal share would be a figure the model
/// does not hold. What a pane or a layer *is* allowed is what the rest of the
/// scene leaves it: the pool's allowance less every other thing's cost. That
/// falls as siblings arrive, exactly as the ruling says, and it is a
/// difference of figures `fit` already produces rather than a second rule.
///
/// A corollary worth stating rather than leaving to be discovered: because the
/// allowance is the pool's less everything else, `cost > allowed` holds for
/// one thing exactly when the scene's whole need is over that pool's
/// allowance. The readout is per pane; the condition it reports is the
/// scene's, and the binder beside it is what says who to ask about it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Charge {
    /// **Which memory this is.** `Joint` on a unified adapter, where the two
    /// shares are one memory and `fit::over` asks one question of it; on a
    /// split capacity, whichever of the two pools this thing fills the larger
    /// fraction of — the one that binds. Naming it is what gives the two
    /// figures below a denominator.
    pub pool: squallar_device_profile::admit::Pool,
    /// What the thing costs that pool at the budgets in force.
    pub cost_bytes: u64,
    /// **The room it has**: the pool's allowance less what the rest of the
    /// scene costs of it. `None` where the pool itself is unknown — a native
    /// arm no host reader has answered for — where nothing is ever over.
    pub allowed_bytes: Option<u64>,
}

impl Default for Charge {
    /// What a fresh application carries before its first composition: nothing
    /// charged, and no pool known well enough for anything to be over.
    fn default() -> Self {
        Self {
            pool: squallar_device_profile::admit::Pool::Gpu,
            cost_bytes: 0,
            allowed_bytes: None,
        }
    }
}

impl Charge {
    /// Whether this thing costs more than the room it has. `false` wherever
    /// the pool is unknown: a readout with no figure refuses nothing, the way
    /// the admission doors do.
    pub fn over(&self) -> bool {
        self.allowed_bytes
            .is_some_and(|allowed| self.cost_bytes > allowed)
    }

    /// **The pool's name for a reader** — the label the Settings screen shows
    /// for the share that moves it (`ui_settings`'s `"GPU memory"` and
    /// `"System memory"`), so a figure and the control that moves it cannot
    /// drift into two names for one memory. `memory` for a unified adapter,
    /// where one of the two shares is not a thing the reader has.
    pub fn pool_word(&self) -> &'static str {
        use squallar_device_profile::admit::Pool;
        match self.pool {
            Pool::Gpu => "GPU memory",
            Pool::Host => "system memory",
            Pool::Joint => "memory",
        }
    }

    /// [`Self::pool_word`] with the word `memory` dropped, for a line that
    /// already has a byte figure on it and no room to say so twice — the
    /// layers menu's rows, where the panel leaves about 178 pt and the pair
    /// plus the allowance takes 148 of it.
    pub fn pool_tag(&self) -> &'static str {
        use squallar_device_profile::admit::Pool;
        match self.pool {
            Pool::Gpu => "GPU",
            Pool::Host => "system",
            Pool::Joint => "memory",
        }
    }
}

/// One pool — GPU or host — as the session sees it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PoolReadout {
    /// The capacity in force this session, in bytes: the figure `fit` is
    /// held to, after pressure has had its say.
    pub capacity_bytes: u64,
    /// How that figure was learned. The host pool has no probe: a probed
    /// session's host figure is the bracket's presumption and says so.
    pub source: CapacitySource,
    /// What the scene's need may occupy of it.
    pub allowance_bytes: u64,
    /// What the scene needs of it now, at the budgets in force.
    pub need_bytes: u64,
    /// **What is left for one more pane or layer**: the allowance less the
    /// need, and on a page heap never more than the heap's own room
    /// (`max - byteLength`, as last read). The live-bytes bound the plan
    /// names (`act_line - live_bytes`) has no producer yet and is not in the
    /// minimum until it does; `None` only where the pool itself is unknown.
    pub spare_bytes: Option<u64>,
    /// **The percentage of this pool the user asked for** — their own
    /// setting, `squallar_device_profile::scene::PoolPercents`, verbatim.
    /// `None` only from a caller that prices no scene (the test harness).
    pub requested_percent: Option<u8>,
    /// **The percentage actually in force**, after the hardware, the user's
    /// own setting and the governor have each had their say: the figure in
    /// force over the figure the machine reported, floored. It is what makes
    /// rung-shedding visible, and the mitigation for a user who set 20 %
    /// months ago and forgot. `None` where the pool itself is unknown.
    pub effective_percent: Option<u8>,
    /// **Which of the three terms is the one holding this pool down.** Three
    /// terms can lower a pool and [`Self::effective_percent`] alone cannot say
    /// which did.
    pub binder: squallar_device_profile::scene::PoolBinder,
    /// **Whether the governor's ceiling is on its way back up.** Read only
    /// when [`Self::binder`] is `Governor`, and true on either pool — both
    /// have a governor that can lift now (`Modulation`'s two ceilings) —
    /// though the two pools mean it by different evidence.
    ///
    /// On the **host** pool it means successive qualifying readings are
    /// banked toward the next promotion
    /// (`squallar_app::recovery::HostRecovery::held`): a squeezed host pool
    /// with nothing banked is not recovering, because the margin is
    /// *observed*.
    ///
    /// On the **GPU** pool it means a step is held at all
    /// (`squallar_app::recovery::GpuRecovery::is_squeezed`). Nothing observes
    /// a card's memory coming back, so that governor restores a step after a
    /// stretch of wall-clock quiet rather than on a margin — there is no bank
    /// that could be empty, and every squeezed GPU pool is on its way back
    /// unless a new event lands. The mechanism is stated on the application's
    /// `budget state:` line (`gpu ... dwell 4x 120 s`) so that this one bool
    /// is not asked to carry the difference.
    pub recovering: bool,
}

impl PoolReadout {
    /// **Which of the three terms is holding this pool down, in words** — the
    /// four the settings caption already uses, so the pane's own line and the
    /// screen that carries the control cannot come to say different things
    /// about one pool.
    ///
    /// `ui_settings`'s `memory_share_caption` spells the same four; that copy
    /// is not yet routed through here because another lane holds the file.
    pub fn binder_words(&self) -> &'static str {
        use squallar_device_profile::scene::PoolBinder;
        match self.binder {
            PoolBinder::Hardware => "all this machine reports",
            PoolBinder::UserPercent => "your setting",
            PoolBinder::Governor if self.recovering => "memory pressure, recovering",
            PoolBinder::Governor => "memory pressure",
        }
    }
}

impl Default for PoolReadout {
    /// A pool nothing has priced: zero capacity, presumed. What a fresh
    /// application carries before its first loop walk.
    fn default() -> Self {
        Self {
            capacity_bytes: 0,
            source: CapacitySource::Presumed,
            allowance_bytes: 0,
            need_bytes: 0,
            spare_bytes: None,
            requested_percent: None,
            effective_percent: None,
            binder: squallar_device_profile::scene::PoolBinder::Hardware,
            recovering: false,
        }
    }
}

/// The frame instrument's histograms, borrowed for one frame — what the
/// diagnostics overlay windows. Cumulative-from-boot recorders; the overlay
/// itself takes the trailing-window diff, so nothing here is ever displayed
/// raw. References rather than copies: composing this costs pointer packing,
/// whether or not the overlay is showing.
#[derive(Clone, Copy)]
pub struct FrameDiagnostics<'a> {
    /// Service of presented frames whose input carried a pointer/touch/
    /// wheel event. Service is the redraw minus the swapchain acquire.
    pub service_interact: &'a Hist,
    /// Service of presented frames whose input carried none.
    pub service_idle: &'a Hist,
    /// Where an interact frame's service went:
    /// `[pre, pump, ui, prepare, finish, post]`.
    pub segments: [&'a Hist; 6],
    /// The swapchain-acquire span of interact frames — the vsync wait,
    /// excluded from service and reported beside it.
    pub acquire: &'a Hist,
    /// Redraw-to-redraw interval of presented frames, both input families.
    /// Never added to service; the two share no denominator.
    pub cadence: &'a Hist,
    /// A GPU pass-timing line, verbatim, once a probe can supply one.
    /// `None` prints as the overlay's absence text — absence, never
    /// extrapolation.
    pub gpu_passes: Option<&'a str>,
    /// The `budget state:` sentence, verbatim — the bracket and rung the
    /// budgets were resolved at and the host signals beside them. `None`
    /// until the telemetry tick has composed one; the overlay prints its
    /// absence text there.
    pub budget_state: Option<&'a str>,
}

/// Where [`crate::Gui::ui_phased`] crossed its own phase boundaries, handed
/// back so the App's frame ledger can cut its `ui` segment with them.
///
/// **Instants, not durations, and that is the point.** The ledger's `ui`
/// segment is bracketed by the App's own two stamps around the call; cutting
/// it with instants taken *inside* the call makes the cuts contiguous with
/// those brackets, so the pieces telescope to the segment exactly instead of
/// summing to something near it. A struct of durations could not do that — it
/// would leave the prologue and the return outside every figure, which is the
/// hole the renderer's own four-phase pass ledger had.
///
/// **Eight stamps, nine cuts**: `ui_start → polled → laid_out → topbar →
/// statusbar → shell → dialog → panes → applied → ui_end`. The two outer
/// boundaries are the ledger's, not this type's, which is why they are absent
/// here.
///
/// The count read "five stamps, six cuts" on an eight-field struct returning
/// nine cuts, from the two widenings that landed after it was written. A lane
/// that sizes its arrays off a stated count rather than off the fields writes
/// against the wrong shape, so the count is restated here whenever a stamp is
/// added — [`StackStamps`] below is the ninth through the fourteenth, and it
/// deliberately does NOT change the nine: those six bound cuts of `stack`,
/// which is one cut of this family, not a tenth beside them. [`PanesCuts`] is
/// not a stamp at all — it is nanosecond sums over `render_panes`' pane loop
/// — and for the same reason does not change the nine either.
#[derive(Clone, Copy, Debug)]
pub struct UiPhaseStamps {
    /// After the frame's polls: the site-table republish, the auto-poll
    /// check and the offline-download settle. These run before anything is
    /// laid out and emit most of the frame's fetch actions.
    pub polled: web_time::Instant,
    /// After `LayoutCtx::resolve`, the pane-grid reflow, the site-query
    /// expiry and the fade invariants — the root `Ui` is built and the
    /// frame's geometry is settled.
    pub laid_out: web_time::Instant,
    /// After `render_top_bar`.
    pub topbar: web_time::Instant,
    /// After `render_status_bar`.
    pub statusbar: web_time::Instant,
    /// After `render_shell`: the topbar, the layer stack and the drawer.
    /// **The frame's eye click is read here** and acted on in the next cut.
    ///
    /// The two stamps above open this span up: `topbar`, then `statusbar`,
    /// then the stack and inspector as the remainder. The shell was ~1 ms of
    /// every interact frame as one undivided cut.
    pub shell: web_time::Instant,
    /// After the time dialog, which sits between the shell and the panes and
    /// was previously counted inside `panes` — a dialog that is not open at
    /// all should not be charged to the map surfaces.
    pub dialog: web_time::Instant,
    /// After the time dialog and `render_panes` — the map surfaces.
    pub panes: web_time::Instant,
    /// After the pending appliers (pane view, section line, region, section
    /// edit) and the fade toggle.
    pub applied: web_time::Instant,
    /// Where `render_stack_and_inspector` crossed its own six interior
    /// boundaries — [`UiPhaseStamps::statusbar`] to [`UiPhaseStamps::shell`],
    /// opened up. See [`StackStamps`].
    ///
    /// **Carried inside this struct rather than returned beside it**, so the
    /// App's call site keeps its tuple arity and the App layer gains no new
    /// reach into the Gui: the six ride the stamps the ledger already takes.
    pub stack: StackStamps,
    /// Where `render_panes` spent the seventh cut — [`UiPhaseStamps::dialog`]
    /// to [`UiPhaseStamps::panes`], opened up. See [`PanesCuts`].
    ///
    /// **Nanosecond sums, not instants**, because that cut runs a pane loop
    /// and four of its seven pieces are sums across it; [`StackStamps`]'
    /// six bound one straight-line span and can be instants. Carried inside
    /// this struct for [`UiPhaseStamps::stack`]'s reason exactly: the App's
    /// call site keeps its arity and the App layer gains no new reach into
    /// the Gui.
    pub panes_cuts: PanesCuts,
}

/// Where `Gui::render_stack_and_inspector` crossed its own boundaries — the
/// six interior stamps that cut `ui.stack`, the fifth of [`UiPhaseStamps`]'
/// nine.
///
/// # Why this exists
///
/// `ui.stack` is one undivided cut and it owns the `ui` tail: measured on
/// hardware WebGPU at 3440x1440, p50 421–500 µs against a max of 8,000–9,514
/// µs — 16–20x its own median, and a single frame more than twice the whole
/// 4 ms bar. Nothing in the tree could say which part of the span that was.
///
/// **Instants, not durations**, for [`UiPhaseStamps`]' reason exactly: the
/// parent's own two boundaries already exist (`statusbar` and `shell`), so
/// six interior instants make seven contiguous cuts that telescope to the
/// parent rather than summing to something near it.
///
/// # No `residual` field, and that is deliberate
///
/// The seventh cut (`settle`) closes on the parent's own right boundary, so
/// no `stack` time can hide in an unnamed tail — `PostHists::close`'s
/// reasoning. What a subtraction would leave is `frame_ledger::micros`'
/// truncation dust, bounded at six microseconds for seven cuts and never
/// over; printing that under a heading called "residual" invites exactly the
/// misreading this family exists to prevent. The residual here is a NAMED
/// span, not a subtraction. `DispatchCuts` needs one only because it
/// accumulates across a loop and cannot bracket its own span.
#[derive(Clone, Copy, Debug)]
pub struct StackStamps {
    /// After the two selection snaps — a pane that draws no map layers, and a
    /// layer the active pane does not hold. `O(1)`, and taken before the
    /// width gate so that the cut it opens exists on every path.
    pub snapped: web_time::Instant,
    /// After the compact-width gate, the chrome fade and the two slide
    /// animations: everything that can decide this frame draws no panel at
    /// all. **Holds the whole span when the panel is closed** — the three
    /// early returns are inside it.
    pub gated: web_time::Instant,
    /// After `PaneState::hydrate_layer_states`. Scales with the active pane's
    /// slot count.
    pub hydrated: web_time::Instant,
    /// After `stack_row_statuses` — one status line per row. Scales with the
    /// layer count, and with the alert-set size through
    /// `NwsAlertHandler::status_line`.
    pub statused: web_time::Instant,
    /// After `render_stack`. Scales with layer count times widgets per row.
    pub rendered: web_time::Instant,
    /// After `render_inspector`. Scales with the selected layer's control
    /// surface.
    pub inspected: web_time::Instant,
}

impl StackStamps {
    /// Every boundary the frame did not reach, at **one** clock read.
    ///
    /// The three early returns (`Compact` width, a faded chrome, both slides
    /// at zero) skip four of the seven regions. Filling their stamps from a
    /// single instant is what makes those four cuts read exactly zero and
    /// leaves `gate` holding the remainder; a fresh `now()` per slot would
    /// scatter the clock's own dust across cuts that ran no code, which is a
    /// figure about the instrument rather than about the frame.
    ///
    /// **Recording is never skipped on these frames.** A narrower `n` than
    /// the parent cut's would break the one property that makes the split
    /// arithmetic — see `frame_ledger::StackHists`' denominator note.
    #[must_use]
    pub fn skipped_after(snapped: web_time::Instant) -> Self {
        let now = web_time::Instant::now();
        Self {
            snapped,
            gated: now,
            hydrated: now,
            statused: now,
            rendered: now,
            inspected: now,
        }
    }
}

/// What one `Gui::render_panes` call spent, by sub-cut, accumulated in
/// **nanoseconds**.
///
/// # Why nanoseconds, and why this is not a `Stamps` type
///
/// [`StackStamps`]' six siblings each bracket a single span between two
/// instants, so instants telescope for them. `render_panes` runs a **loop**,
/// and four of the seven cuts below are sums across it. Truncating each to
/// whole microseconds once per pane would round every sub-microsecond piece
/// to zero and hand the difference to the residual — the one figure that must
/// not absorb an artifact, because it is read as "time these seven do not
/// name". `frame_ledger::DispatchCuts` is the same shape for the same reason.
///
/// # The seven partition the call, and nothing overlaps
///
/// `setup` and `panel` are the frame's two prologues, `resolve`/`widget`/
/// `content`/`tools` are per-pane sums over the pane loop, and `credit` is
/// the epilogue that closes on the function's own return. Together they are
/// contiguous over the whole call, so `frame_ledger::PanesHists`' residual is
/// the parent cut minus these — arithmetic, never inference.
///
/// **A pane that is not a plan view leaves `widget` at zero and files its
/// whole arm under `content`.** Only the plan-view arm runs a `walkers::Map`
/// widget, so `widget` is structurally absent on a cross-section or volume
/// pane rather than small — read it beside `content`, never alone.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct PanesCuts {
    /// The prologue before the central panel opens: the refusal-plate text,
    /// the per-pane cost vector, the visible-pane count, and the basemap and
    /// terrain slot decisions (`ensure_base_tiles` / `release_base_tiles` and
    /// their terrain twins) with the two takes that follow them. Scales with
    /// what the tile slots have to do, which on a steady frame is nothing.
    pub setup_ns: u64,
    /// The panel's own head, before the first pane: the colour-scale
    /// orientation, `detect_active_pane_click`, the multi-pane viewport
    /// snapshot, `dismiss_overlay_popups`, the floor set and
    /// `floor_strip_plan`. Scales with pane count and with nothing else.
    pub panel_ns: u64,
    /// Per pane, everything before its render-view arm: the pane rect, the
    /// colour-scale floor (which measures through the painter), the pane's
    /// take out of the slice, the centre, the pointer and gesture resolve and
    /// the child `Ui`. Scales with pane count.
    pub resolve_ns: u64,
    /// Per pane, the `walkers::Map` widget's own frame **around** this
    /// crate's draw closure — the gesture handling and the projector on the
    /// way in, the response on the way out. Zero on a pane that is not a plan
    /// view. Scales with pane count, not with layer data.
    pub widget_ns: u64,
    /// Per pane, what the pane actually draws.
    ///
    /// On a plan view: the zoom read, the armed gesture's tracking, the
    /// `PaneRenderCtx` this crate builds to hand over (the excluded-rect and
    /// user-fix clones are in here, not in `resolve`), and
    /// `render_pane_map_content` — the whole enabled-layer walk. On a
    /// cross-section or volume pane: the entire arm.
    ///
    /// Scales with **layer data**, and is the only cut here that does.
    pub content_ns: u64,
    /// Per pane, the furniture around the content: the section-edit tracker,
    /// the section tracks and the region boxes inside the map closure, and
    /// after the arm the armed-hint chip, the pane's restore into the slice
    /// and its border. Scales with pane count; all three draw tools are inert
    /// unless one is armed.
    pub tools_ns: u64,
    /// The epilogue, and this split's right boundary: the pane dividers, the
    /// basemap and terrain credit, `sync_viewports`, and after the panel
    /// closes the floor-strip pass end and the two tile restores. Drawn once
    /// per panel however many panes there are.
    pub credit_ns: u64,
}

impl PanesCuts {
    /// Charge `ns` to `slot`, saturating.
    ///
    /// **The accumulation is nanoseconds and converts once**, in
    /// `frame_ledger::panes_cut_micros`. Truncating to whole microseconds
    /// here — once per pane — would round every sub-microsecond piece to zero
    /// and hand the difference to the residual, which is the one figure that
    /// must not absorb an artifact. `frame_ledger::DispatchCuts` carries the
    /// same rule for the same reason.
    pub fn charge_ns(slot: &mut u64, ns: u64) {
        *slot = slot.saturating_add(ns);
    }

    /// Charge the span from `from` to now against `slot`, and return the
    /// instant that closed it so the caller can carry it as its cursor.
    ///
    /// **One clock read.** The cuts are contiguous, so a seam's instant both
    /// closes the cut behind it and opens the one in front; taking two would
    /// leave the gap between them in no cut at all and quietly inflate the
    /// residual.
    #[must_use]
    pub(crate) fn charge(slot: &mut u64, from: web_time::Instant) -> web_time::Instant {
        let now = web_time::Instant::now();
        Self::charge_ns(
            slot,
            now.duration_since(from)
                .as_nanos()
                .min(u128::from(u64::MAX)) as u64,
        );
        now
    }

    /// The last charge of a call, where no cut opens after it. Spelled apart
    /// from [`PanesCuts::charge`] so that one can stay `#[must_use]`: a
    /// dropped cursor mid-call would leave the next cut measuring from the
    /// wrong instant, and that is a mistake worth a compile error.
    pub(crate) fn close(slot: &mut u64, from: web_time::Instant) {
        let _ = Self::charge(slot, from);
    }
}

/// One frame's facts, composed by the App from state it already owns, applied
/// by `Gui::apply_frame_inputs` once per frame immediately before `Gui::ui`.
pub struct FrameInputs<'a> {
    /// Safe-area insets in logical pixels (top, bottom, left, right).
    pub safe_area_insets: (f32, f32, f32, f32),
    /// Whether this platform can quit; `false` drops Exit from the menu.
    pub supports_exit: bool,
    /// This build's loop frame cap, for the timeline's row-2 caption.
    pub loop_frame_budget: usize,
    /// **How many overlay rasters one pane and layer may have crossing at
    /// once** — the device's `Budgets::concurrent_renders`, which is the same
    /// figure every other background render on this device is spent against
    /// and is read off the resolved budgets rather than a `cfg`.
    ///
    /// Composed here rather than read from `squallar_device_profile` inside this
    /// crate for the reason [`Self::loop_frame_budget`] is: the resolved value
    /// is the App's, and a browser's is not a `cfg` — the same wasm build gets
    /// a different number on a blocklisted driver than on a workstation GPU.
    pub concurrent_renders: usize,
    /// **What the map tile caches may hold** — the device's
    /// `Budgets::tile_cache()`, in bytes per population (styled, parsed,
    /// terrain), composed here for the reason [`Self::concurrent_renders`]
    /// is: the resolved value is the App's, and on the measured arm it is a
    /// share of what the card left over once the scene was paid for, which no
    /// `cfg` can say. Applied to every tile source, live or parked, before the
    /// pane loop.
    pub tile_cache: squallar_device_profile::budget::TileCacheBudget,
    /// **How much overdraw a whole-picture overlay raster asks for**, per
    /// side, as the fraction `crate::overlay_cache::plan_overlay_texture`
    /// takes — the device's `Budgets::overlay_oversample_percent` through
    /// `crate::overlay_cache::overdraw_for_oversample`, composed here for the
    /// reason [`Self::tile_cache`] is: the ladder's rung is the App's, decided
    /// by `fit` against this session's capacity, and a browser's page heap is
    /// what moves it. Held to `OVERDRAW_FRACTION` by the planner.
    pub overlay_overdraw: f32,
    /// Whether this platform has a location settings page to offer.
    pub location_settings_available: bool,
    /// What the platform location service is doing: (permission, active).
    pub location: (squallar_location::LocationPermission, bool),
    /// Fix + when the app heard it. The instant travels WITH the fix because
    /// `user_fix_at` is "when did we last hear anything", stamped at arrival —
    /// re-stamping per frame would break the settings pane's staleness
    /// question (see `user_fix_at` on the `Gui` state).
    pub gps: Option<(squallar_location::Fix, web_time::Instant)>,
    /// Compass heading in degrees, once a platform has delivered one.
    pub user_heading: Option<f32>,
    /// Whether the site list is still short of the network.
    pub catalogue_pending: bool,
    /// **What each layer says it is doing**, in the layer's own vocabulary
    /// behind an opaque payload (WO-E8c). This replaced two radar-shaped
    /// members — the chunk-feed status and the per-site volume stamps — and
    /// the reason it is opaque is that the second one was already the third
    /// radar field to want a home here.
    ///
    /// The shell rebuilds an entry when that layer's answer **changes**;
    /// every other frame re-states the same `Arc`s.
    pub liveness: &'a [squallar_source::liveness::SourceLiveness],
    /// How much extra tile detail the 3D floor can actually show. Pushed from
    /// `present_frame` after this frame's `Gui::ui` under the setter regime,
    /// so the UI always read it a frame late; composed at the top of the next
    /// frame it has the identical observable timing.
    pub floor_tile_zoom_bias: u8,
    /// Moves when the shell's mirror plan changed on a frame whose strips
    /// were **held** (the clean skip): the realloc the new plan needs would
    /// destroy the picture the floors are sampling, so the shell defers it
    /// and this stamp makes the Gui repaint every strip first. A rung flip
    /// mid-orbit reaches the strips through here.
    pub mirror_plan_stamp: u64,
    /// The frame instrument's histograms, when the shell has them — the
    /// diagnostics overlay's input. `None` from a caller with no ledger (the
    /// test harness); the overlay then shows itself still collecting.
    pub frame_diagnostics: Option<FrameDiagnostics<'a>>,
    /// **The budget system's readout** — per-pane priced terms and held
    /// bytes, the two pools' capacity, need and spare — as the App's last
    /// telemetry tick composed it ([`BudgetReadout`]). Borrowed for the
    /// frame; the Gui takes a copy only when [`BudgetReadout::generation`]
    /// moved, which on every frame but that one is a single `u64` compare.
    /// `None` from a caller that prices no scene (the test harness).
    pub budget_readout: Option<&'a BudgetReadout>,
    /// **The admission table** — what one more pane, layer or loop would cost
    /// and what the pools have left, as the App last priced it
    /// ([`crate::admission::AdmissionCosts`]).
    ///
    /// The other half of [`Self::budget_readout`] and composed on the same
    /// tick off the same scene: that one describes what is resident, this one
    /// prices what is *not yet*. Borrowed for the frame; the ledger takes a
    /// copy only when the generation moved, which on every other frame is a
    /// single `u64` compare.
    ///
    /// `None` from a caller that prices no scene (the test harness). A door
    /// with no table admits everything — an application that has not yet
    /// priced a scene has nothing to refuse against, and refusing on an
    /// absent figure is how an admission system turns into a wall.
    pub admission: Option<&'a crate::admission::AdmissionCosts>,
    /// **The one debited total both ledgers spend from**
    /// ([`crate::admission::SharedDebit`]).
    ///
    /// The table above says what the App published; this says what has been
    /// admitted against it *since*, and there is exactly one of it. There are
    /// two ledgers — the App keeps its own because its doors run inside its
    /// own borrow of the Gui and could not reach the UI's — and a copy of the
    /// debit each is how a burst mixing UI acts with loop arms inside one
    /// telemetry tick admits one tick's spare **twice**.
    ///
    /// A handle, not a figure, and re-stated every frame: the ledger joins it
    /// once and both then spend from it. `None` from a caller with no App
    /// behind it (the test harness), which keeps the total it already had.
    pub admission_debit: Option<&'a crate::admission::SharedDebit>,
    /// **A refusal raised on the App's side of the seam**, for the pane to
    /// paint.
    ///
    /// The loop door lives in `squallar-app` and keeps its own ledger, so its
    /// refusals cannot reach the glass the way the UI doors' do. They cross
    /// here instead, as the sentence itself - the Gui raises it locally when
    /// the text **changes**, so a notice already up is not re-stamped every
    /// frame and does age out.
    ///
    /// **A refusal nobody can see is a worse defect than the allocation it
    /// prevents**, which is why this is a field on the frame's inputs rather
    /// than a log line.
    pub admission_notice: Option<&'a str>,
}

/// Event-shaped pushes applied at the call site's existing control-flow
/// position. Variants named after today's setters on purpose; they re-verb
/// at E5/E8.
pub enum GuiEvent {
    /// A complete volume's scan info, for all panes viewing the site.
    ///
    /// **The site-wide fan-out, and the live feed's variant.** Every pane on
    /// the site takes it, which is right for a volume nobody asked for in
    /// particular: the real-time chunk feed's closed volumes, the archive
    /// auto-poll, and the refetch a retired feed falls back to. An archive
    /// volume a *pane* navigated to is [`GuiEvent::ScanInfoForTimeGroup`]
    /// instead — see there for why the two cannot be one event.
    ScanInfoForSite { site: String, info: ScanInfo },
    /// **A volume the real-time chunk feed completed**, for every pane on the
    /// site that is *following* live.
    ///
    /// The site-wide fan-out narrowed by the one thing that separates the feed
    /// from a wholesale replacement of the site's data: a pane parked in the
    /// archive is not watching for it. `UNLINK_NOTE`'s two clauses are both
    /// about that pane — it holds its moment when parked, and follows new
    /// scans when live — and this event is the producer of the second, so it
    /// must not be the one that breaks the first.
    LiveScanInfoForSite { site: String, info: ScanInfo },
    /// **The archive volume one pane asked for**, delivered to that pane and
    /// the panes that share its clock — never to a same-site pane parked at
    /// its own moment.
    ///
    /// `UNLINK_NOTE` promises exactly this: "Parked in the archive it holds
    /// its moment; still live, it still follows new scans." The two clauses
    /// are two different audiences, which is why the shell has two events and
    /// not one with a flag. `requester` is the pane index the fetch was
    /// spawned for, carried back through `ScanResponse`; the group is
    /// resolved here, at delivery, so a link toggled while the fetch was in
    /// flight is honoured.
    ///
    /// A pane in the group but on **another site** is skipped: the volume is
    /// this site's, and shared time never means shared data.
    ScanInfoForTimeGroup {
        site: String,
        requester: usize,
        info: ScanInfo,
    },
    /// MERGE semantics, NOT replace — former `apply_chunk_scan_info` doc
    /// (partial volumes union products/elevations; no spinner/backoff touch).
    ChunkScanInfo { site: String, info: ScanInfo },
    /// Scan info for one pane only.
    ScanInfoForPane { pane_idx: usize, info: ScanInfo },
    /// Whether a fetch someone is waiting on is in flight.
    Fetching(bool),
    /// A fetch failed: the message, spinner down, archive backoff advanced.
    Error(String),
    /// The time the shell has navigated to, and **the only thing about a
    /// scan the shell pushes**: a site belongs to a pane, and the shell sets
    /// it by writing that pane.
    ///
    /// It replaced `GuiEvent::RadarConfig`, which carried a site beside the
    /// timestamp for one writer — `SwitchRadarSite` — that already writes
    /// every moving pane's site itself. With no app-wide site left for the
    /// second half to land in, the two variants said the same thing and one
    /// of them went.
    ///
    /// Applying it re-renders the Set Time dialog's two strings from the time
    /// still selected, so a half-typed edit does not survive a navigation.
    SelectedTime(chrono::NaiveDateTime),
    /// Live/historic viewing mode for one pane.
    ViewingLiveForPane { pane_idx: usize, live: bool },
    /// **One pane's time selection moved to `instant`** — the whole gesture,
    /// as one event.
    ///
    /// Three things move together whenever a user names a moment on a pane:
    /// the pane's `viewing_live` posture, the pane's **clock**
    /// ([`crate::pane::PaneState::set_time_mode`], which settles every layer's
    /// playhead onto it), and the Set Time dialog's displayed selection.
    ///
    /// They were three separate pushes and one of them was simply missing from
    /// the step buttons: `handle_navigate_time` sent
    /// [`Self::ViewingLiveForPane`] and [`Self::SelectedTime`] and no clock
    /// move at all, while the scrubber wrote the clock itself in the UI before
    /// emitting the same action. So a step on a pane with no radar scan moved
    /// nothing a layer could read, which is WO-T3.10's defect. Making it one
    /// event is what stops a fourth caller omitting a half.
    ///
    /// `instant` is **UTC**; the dialog's own strings are local, and the
    /// conversion is this event's to make so the two cannot drift. Under
    /// `live` the clock goes back to [`crate::pane::TimeMode::Live`] and
    /// `instant` is only what the dialog shows.
    PaneTimeSelected {
        pane_idx: usize,
        instant: chrono::NaiveDateTime,
        live: bool,
    },
    /// Install what can draw 3D panes, or take it away.
    VolumePainter(Option<std::sync::Arc<dyn crate::volume_view::VolumePainter>>),
    /// Install what can draw a vector tile's fills from the GPU, or take it
    /// away. Absent, every fill takes the CPU placement path — see
    /// [`crate::tile_mesh`].
    TileMeshPainter(Option<std::sync::Arc<dyn crate::tile_mesh::TileMeshPainter>>),
    /// Install what can draw a radar sweep's code plane from the GPU, or take
    /// it away. Absent, a pane holding a polar surface draws no radar at all —
    /// there is no raster to fall back to — so the shell installs this before
    /// it lets anything produce one. See [`crate::radar_fan`].
    RadarFanPainter(Option<std::sync::Arc<dyn crate::radar_fan::RadarFanPainter>>),
}

#[cfg(test)]
mod tests;
