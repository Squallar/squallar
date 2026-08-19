use std::any::Any;
use std::fmt::Debug;
use std::future::Future;
use std::marker::PhantomData;
use std::pin::Pin;
use std::sync::Arc;

use rustdar_source::id::{LayerId, known};
use rustdar_source::job::{DescribedJob, JobCodec};
use rustdar_units::UserPreferences;

use crate::fetch_policy::{
    Assembled, DataCompleteness, FetchError, FetchFailure, FetchHealth, FetchRetry, FetchRound,
    RoundShape, Whole,
};
use crate::render::controls::{
    ControlEffect, ControlItem, ControlUpdate, PaneControlContext, PaneControlContextMut,
};
use crate::render::draw::{DrawPointContext, HoverContext, MapPoint, PointPainter};
use crate::types::{OverlayFeature, OverlayLabel};

/// What opens a layer-stack status line that is reporting a fault rather than a
/// count — see [`OverlayRegistry::status_line`].
///
/// A `const` and not a literal because the host **reads** it: the stack row
/// renders its status line `.small().weak()`, which is the same dim grey an
/// ordinary `3 shown - W/Wa` sits in, so a warning rendered that way is a
/// warning in the typeface of a footnote. `rustdar-egui` tests this prefix to
/// colour the line instead, and a mark the two crates spelled differently would
/// be a mark that silently stopped being legible.
pub const STATUS_MARK: &str = "!";

/// Not `rustdar_radar::LegendScale`: duplicated here to avoid the dependency.
pub struct OverlayLegend {
    /// Colour stops, **sorted ascending by value**.
    pub thresholds: Vec<(f32, [u8; 3])>,
    pub is_gradient: bool,
    pub min_value: f32,
    pub max_value: f32,
    /// e.g. "J/kg".
    pub unit_label: &'static str,
}

/// Fetch-cache-generation lifecycle shared by every overlay type.
///
/// `S` is the layer's declared [`RoundShape`], and it decides which
/// data-installing method this state has at all — [`set_data`] for a [`Whole`]
/// round, [`set_data_with_coverage`] and only that for an [`Assembled`] one.
/// There is deliberately **no default** for it: a default is a way of not
/// saying, and not saying is precisely how five layers came to declare a
/// half-delivered round whole.
///
/// Nothing about it costs a `Whole` layer anything beyond the word: no field
/// with a runtime size, no branch, no call it did not already make.
///
/// [`set_data`]: OverlayState::set_data
/// [`set_data_with_coverage`]: OverlayState::set_data_with_coverage
pub struct OverlayState<T, S: RoundShape> {
    pub data: T,
    /// Stamped on a **good answer only**. Was the sole input to "is a fetch
    /// due?", which is what made a failing layer due on every frame — see
    /// [`crate::fetch_policy`]. `retry` is the other half of that decision now.
    pub fetch_time: Option<web_time::Instant>,
    pub fetching: bool,
    pub data_generation: u64,
    /// What the last fetch did, and what the next automatic one may do.
    pub retry: FetchRetry,
    /// `fn() -> S` rather than `S`, so the marker can never make this state
    /// less `Send` or less `Sync` than the data it holds.
    shape: PhantomData<fn() -> S>,
}

impl<T: Default, S: RoundShape> Default for OverlayState<T, S> {
    fn default() -> Self {
        Self {
            data: T::default(),
            fetch_time: None,
            fetching: false,
            data_generation: 0,
            retry: FetchRetry::new(),
            shape: PhantomData,
        }
    }
}

impl<T: Default, S: RoundShape> OverlayState<T, S> {
    pub fn new() -> Self {
        Self::default()
    }
}

impl<T> OverlayState<T, Whole> {
    /// Bumps `data_generation`, which is what invalidates cached textures.
    ///
    /// Also ends the fetch and clears the retry ladder: this **is** the good
    /// answer, and a handler should not have to remember to say so three times.
    ///
    /// And declares the answer **whole** — which is now a thing it is entitled
    /// to say, rather than a thing it says by default. This method exists only
    /// on a state whose layer declared [`Whole`], and a layer only gets to
    /// declare that by taking delivery of a round type that declared it too;
    /// see [`FetchRound`]. A round assembled from several requests reaches
    /// [`set_data_with_coverage`](OverlayState::set_data_with_coverage) or it
    /// reaches nothing.
    pub fn set_data(&mut self, data: T) {
        self.install(data, DataCompleteness::default());
    }
}

impl<T> OverlayState<T, Assembled> {
    /// The **only** way data reaches the map of a layer whose round can deliver
    /// less than it was asked for. It carries the report of how much less
    /// because there is nothing else to carry it with: this state has no
    /// `set_data`.
    ///
    /// One call rather than two, so the order cannot be got wrong: an ordinary
    /// `set_data` declares its answer whole, so a handler that recorded
    /// coverage first and data second would erase its own report and be back to
    /// the silence this exists to end.
    ///
    /// The data still lands, the clock still stamps, the ladder still resets. A
    /// half-delivered round is a **good answer missing pieces**, not a failure:
    /// what arrived is real and has to be drawn, and filing it as a failure
    /// would back the layer off from the very retry that could complete it.
    ///
    /// A recovered round passes [`DataCompleteness::default`] through here and
    /// the mark clears, so an assembled layer that is whole again does not have
    /// to remember to say so either.
    ///
    /// # The METAR defect, and why it no longer compiles
    ///
    /// One state network refuses, that state blanks, and the round is still
    /// `Ok` because the rest of the country's observations are real. Written
    /// the way it was written the first time — take the observations, hand them
    /// to `set_data`, say nothing about the network that refused:
    ///
    /// ```compile_fail
    /// use rustdar_overlays::fetch_policy::Assembled;
    /// use rustdar_overlays::render::overlay_state::OverlayState;
    ///
    /// struct MetarRound {
    ///     observations: Vec<u8>,
    ///     failed_networks: Vec<&'static str>,
    /// }
    ///
    /// let mut state: OverlayState<Vec<u8>, Assembled> = OverlayState::new();
    /// let round = MetarRound {
    ///     observations: vec![1, 2, 3],
    ///     failed_networks: vec!["KS"],
    /// };
    /// state.set_data(round.observations);
    /// ```
    ///
    /// `error[E0599]: no method named set_data found for struct
    /// OverlayState<Vec<u8>, Assembled> in the current scope`, with rustc
    /// adding `the method was found for OverlayState<T, Whole>`.
    ///
    /// The **pair** is what makes that mean anything, not the `compile_fail`
    /// alone: a `compile_fail` block passes on any error at all, a typo
    /// included, and rustdoc's `,E0599` annotation is checked on nightly only,
    /// so on a stable toolchain it is decoration. So here is the same round,
    /// the same state, the same data and the same clock, differing in one
    /// thing — that the report of what is not on the map comes with it — and it
    /// compiles and runs:
    ///
    /// ```
    /// use rustdar_overlays::fetch_policy::{Assembled, DataCompleteness};
    /// use rustdar_overlays::render::overlay_state::OverlayState;
    ///
    /// struct MetarRound {
    ///     observations: Vec<u8>,
    ///     failed_networks: Vec<&'static str>,
    /// }
    ///
    /// let mut state: OverlayState<Vec<u8>, Assembled> = OverlayState::new();
    /// let round = MetarRound {
    ///     observations: vec![1, 2, 3],
    ///     failed_networks: vec!["KS"],
    /// };
    /// let coverage = DataCompleteness {
    ///     expected: 6,
    ///     missing: round.failed_networks.len(),
    ///     unit: "state networks",
    ///     ..DataCompleteness::default()
    /// };
    /// state.set_data_with_coverage(round.observations, coverage);
    ///
    /// // A good answer: fresh clock, clear ladder — and marked for the state
    /// // that is not drawn, which is the half no check in this crate could
    /// // express before `DataCompleteness` and no type could require before
    /// // this method was the only one here.
    /// assert!(!state.retry.is_unhealthy());
    /// assert!(state.retry.is_incomplete());
    /// ```
    pub fn set_data_with_coverage(&mut self, data: T, coverage: DataCompleteness) {
        self.install(data, coverage);
    }

    /// Coverage on its own, for the assembled layer that **stamps its own map**
    /// rather than replacing it.
    ///
    /// SPC outlooks are keyed by `(day, product)` and arrive one product per
    /// payload, so there is no moment at which the layer holds a finished
    /// answer to hand to `set_data_with_coverage`; its round is finished when
    /// the last of several tasks lands, and that is where it writes its ledger.
    /// This is on the assembled impl and not the shared one so it stays what it
    /// is — a second way in for a layer that already declared it can
    /// under-deliver — rather than a way for any layer at all to start moving
    /// the coverage axis.
    pub fn record_coverage(&mut self, coverage: DataCompleteness) {
        self.retry.record_coverage(coverage);
    }
}

impl<T, S: RoundShape> OverlayState<T, S> {
    /// What both of the above do, which is the same thing: the difference
    /// between them is what each is *allowed to pass*, never what happens next.
    fn install(&mut self, data: T, coverage: DataCompleteness) {
        self.data = data;
        self.fetch_time = Some(web_time::Instant::now());
        self.data_generation = self.data_generation.wrapping_add(1);
        self.fetching = false;
        self.retry.record_success();
        self.retry.record_coverage(coverage);
    }

    /// This layer's own round, out of the payload the host handed back — and
    /// the seam where the round type's declaration meets the layer's.
    ///
    /// `R::Shape` unifies with this state's `S`, which is what turns one
    /// declaration into the whole guarantee. A handler holding a [`Whole`]
    /// state cannot take delivery of a round that declared itself
    /// [`Assembled`]: it does not compile, and the only fix is to say
    /// `Assembled` in the state's own type — at which point `set_data` is gone
    /// and the sole route onto the map is
    /// [`set_data_with_coverage`](OverlayState::set_data_with_coverage), which
    /// cannot be called without the report.
    ///
    /// Every handler takes delivery here rather than reaching for `downcast`
    /// itself, because reaching for `downcast` is how a handler would step
    /// around the unification and get its `Whole` state back;
    /// `no_handler_takes_delivery_of_its_round_by_hand` fails if one starts.
    ///
    /// `None` is "this payload is not mine", which every caller logs and
    /// returns on, exactly as it did when it wrote the downcast itself.
    ///
    /// # The storm-reports defect, at the earlier of its two seams
    ///
    /// Three CSVs, one per report kind, refused as a round only when all three
    /// fail — so a 503 on the tornado CSV arrives here as `Ok` with every
    /// tornado report in the country missing from it. A handler that files that
    /// round into a `Whole` state does not get as far as `set_data`:
    ///
    /// ```compile_fail
    /// use rustdar_overlays::fetch_policy::{Assembled, FetchRound, Whole};
    /// use rustdar_overlays::render::overlay_state::{FetchPayload, OverlayState};
    ///
    /// struct StormReportsFetchResult {
    ///     reports: Vec<u8>,
    ///     failed_kinds: Vec<&'static str>,
    /// }
    /// impl FetchRound for StormReportsFetchResult {
    ///     type Shape = Assembled;
    /// }
    ///
    /// let state: OverlayState<Vec<u8>, Whole> = OverlayState::new();
    /// let payload: FetchPayload = Box::new(StormReportsFetchResult {
    ///     reports: vec![1, 2, 3],
    ///     failed_kinds: vec!["tornado"],
    /// });
    /// let _round = state.downcast_round::<StormReportsFetchResult>(payload);
    /// ```
    ///
    /// `error[E0271]: type mismatch resolving <StormReportsFetchResult as
    /// FetchRound>::Shape == Whole`.
    ///
    /// Its pair, which is the identical program with the layer declaring the
    /// shape its round declared, and which compiles and hands the round over:
    ///
    /// ```
    /// use rustdar_overlays::fetch_policy::{Assembled, FetchRound};
    /// use rustdar_overlays::render::overlay_state::{FetchPayload, OverlayState};
    ///
    /// struct StormReportsFetchResult {
    ///     reports: Vec<u8>,
    ///     failed_kinds: Vec<&'static str>,
    /// }
    /// impl FetchRound for StormReportsFetchResult {
    ///     type Shape = Assembled;
    /// }
    ///
    /// let state: OverlayState<Vec<u8>, Assembled> = OverlayState::new();
    /// let payload: FetchPayload = Box::new(StormReportsFetchResult {
    ///     reports: vec![1, 2, 3],
    ///     failed_kinds: vec!["tornado"],
    /// });
    /// let round = state
    ///     .downcast_round::<StormReportsFetchResult>(payload)
    ///     .expect("the payload is this layer's own");
    /// assert_eq!(round.failed_kinds, ["tornado"]);
    /// ```
    ///
    /// Declaring the same round [`Whole`] is the one step left that a person
    /// can still take alone, and it is a step taken beside `failed_kinds` with
    /// nothing else in the file to blame it on.
    pub fn downcast_round<R>(&self, payload: FetchPayload) -> Option<R>
    where
        R: FetchRound<Shape = S>,
    {
        payload.downcast::<R>().ok().map(|round| *round)
    }

    // `record_success()` used to live here — stamp the clock, end the fetch,
    // clear the ladder — documented as "a good answer that replaced no data,
    // for the outlook handler". It had **no callers at all**: the outlook
    // handler writes `state.retry.record_success()` itself, from
    // `file_round_verdict`, because its ledger is written once per round and
    // not once per payload.
    //
    // Deleted rather than left as a correct-looking helper, for the same
    // reason `needs_refresh` below it was: on an `Assembled` state it was the
    // whole silence in two lines. `state.data = whatever; state.record_success()`
    // installs data, stamps a fresh clock, resets the ladder and leaves the
    // previous coverage report standing — everything `set_data` used to do
    // wrong, reachable on exactly the layers that must not be able to do it.

    /// End a fetch that did not produce data, filing it against the ladder.
    ///
    /// The counterpart to [`set_data`](Self::set_data), and the reason a
    /// handler's error branch can no longer leave the layer due on the next
    /// frame: this is the only way to clear `fetching` after a failure, and it
    /// records the failure in the same move.
    ///
    /// [`set_data`]: Self::set_data
    pub fn record_failure(&mut self, error: &FetchError) {
        self.fetching = false;
        self.retry.record_failure(error);
        // An `Absent` answer is an answer: it stamps the clock, so the layer
        // goes back to its ordinary interval instead of being permanently due.
        if error.failure == FetchFailure::Absent {
            self.fetch_time = Some(web_time::Instant::now());
        }
    }

    /// Whether the user switching this layer on should re-ask the origin, given
    /// the handler's own answer to "do I have anything to draw?".
    ///
    /// Three reasons to ask, and the last two are the ones that were missing:
    /// there is nothing drawn, **or** what is drawn is stale
    /// ([`FetchRetry::is_unhealthy`]), **or** what is drawn is missing pieces
    /// ([`FetchRetry::is_incomplete`]). The condition used to be `!has_data`
    /// alone, and every handler spells `has_data` as "the vector is not empty" —
    /// so a layer that fetched successfully, then started failing, was holding
    /// data and did not re-ask. Toggling it off and on did nothing, in the one
    /// case where a user is most likely to try it, on layers that carry
    /// warnings. See `Gui::set_pane_overlay_with_fetch` for the same rule at the
    /// pane seam.
    ///
    /// The incompleteness clause is the same argument one axis over: a layer
    /// drawing 85 of 297 warnings is the strongest case there is for a user
    /// toggling it and expecting the missing ones to appear, and a re-ask is
    /// exactly what could deliver them — the zone boundaries that failed are not
    /// cached, so the next round retries precisely them.
    ///
    /// `has_data` is a parameter rather than read from `self` because only the
    /// handler knows what having data means for its own payload — a `Vec`, an
    /// `Option<Arc<Grid>>`, a map keyed by product.
    pub fn enable_should_refetch(&self, has_data: bool) -> bool {
        !self.fetching && (!has_data || self.retry.is_unhealthy() || self.retry.is_incomplete())
    }

    // `needs_refresh(interval)` used to live here — `fetch_time.is_none_or(|t|
    // t.elapsed() >= interval)`, documented as "how every auto-polling overlay
    // refreshes". It had no callers, and that rule is exactly the storm: a
    // failure never stamps `fetch_time`, so it answers true on every frame
    // forever. Removed rather than left as a correct-looking helper for the
    // next person to reach for; the rule that replaced it is
    // `OverlayHandler::auto_fetch_delay`.
}

/// The draw loop dispatches on this rather than matching `OverlayKind`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderMode {
    /// Rasterized to RGBA on a background thread (SPC, NWS, Radar).
    Texture,
    /// Per-frame via [`PointPainter`] (METAR station models).
    PerFramePoint,
    /// Per-frame by the handler itself (UserLocation).
    PerFrameDirect,
    /// Streaming tiles owned by the map widget (BaseMap, CityLabels).
    Tile,
}

/// Which of a pane's two content surfaces a layer draws onto.
///
/// A pane's content divides in two, and the line between them is geography:
///
/// * [`Ground`](Surface::Ground) — everything drawn **at** a latitude and
///   longitude, through the projector. The basemap and city-label tiles, the
///   radar raster and its range ring, SPC outlooks and mesoscale discussions,
///   NWS alerts, storm reports, lightning, METARs, model data, the radar-site
///   icons and their names, the location dot. All of it is a picture of the
///   world, and all of it is still true when it is laid flat on the world.
/// * [`Glass`](Surface::Glass) — chrome, positioned against the pane's own
///   **edges** rather than against the map underneath: the colour-scale
///   legends and the stale-image notice. Neither has a latitude, and neither
///   survives being laid flat — on a 3D pane's floor a legend is painted into
///   the ground in perspective, shrinking with distance and swinging round
///   with the camera.
///
/// For a plan-view pane the distinction is invisible, because its ground *is*
/// its glass: one rect carries both. It becomes real for a 3D pane, whose
/// ground goes into the off-screen strip the raymarcher mirrors onto the floor
/// (`Gui::draw_floor_strip`) while its glass stays on the pane rect the volume
/// occupies.
///
/// Declared by the handler ([`OverlayHandler::surface`]) rather than matched
/// over `OverlayKind` in the UI crate: a new layer does not compile until its
/// author has said whether it is a picture of the world or chrome over one.
/// That is what makes the split a stated rule rather than an `if` somebody
/// happened to write in one arm — the previous spelling of it was no spelling
/// at all, which is how the colour scale ended up painted onto the ground the
/// day the ground arrived.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Surface {
    /// Geography. Mirrors onto a 3D pane's floor.
    Ground,
    /// Chrome over geography. Never mirrors.
    Glass,
}

/// Handlers store `Vec<Arc<T>>`; a click clones the `Arc` into the selection
/// list as `Arc<dyn OverlayItem>`.
pub trait OverlayItem: Send + Sync + Debug {
    fn kind(&self) -> OverlayKind;

    fn popup_content(&self, prefs: &UserPreferences) -> PopupContent;

    /// Logical identity (same alert ID, same MD number), not pointer equality:
    /// `retain_selections()` uses it to carry selections across a refetch.
    fn matches(&self, other: &dyn OverlayItem) -> bool;

    /// For concrete type comparisons inside `matches()`.
    fn as_any(&self) -> &dyn Any;
}

/// Lets the UI crate hit-test without knowing overlay-specific types.
///
/// A **view** of the handler's geometry, not a copy of it. It used to own a
/// `Vec<OverlayFeature>` — rings, plus the precomputed triangulation index
/// buffers — and the draw loop built one per alert per pane per frame, so a day
/// with national warning coverage deep-cloned every polygon in the country sixty
/// times a second. Nothing needs the geometry to outlive the handler borrow: the
/// hit test runs inside it and clones only the `Arc` of what it hit.
///
/// Labels are **not** here. They are the one thing the draw loop wants on every
/// frame, and asking for them through this type is what made a frame with no
/// click pay for geometry — see [`OverlayHandler::map_labels`].
pub struct ClickableItem<'a> {
    pub features: &'a [OverlayFeature],
    pub item: Arc<dyn OverlayItem>,
}

// ── Overlay handler trait ─────────────────────────────────────────────────

/// Adding an overlay means: implement this, register it in `create_handlers()`,
/// add an `OverlayKind` variant. Nothing in `rustdar-egui` or
/// the `rustdar` crate changes.
pub trait OverlayHandler: Send {
    // ── Identity & metadata ───────────────────────────────────────────

    fn kind(&self) -> OverlayKind;

    /// This layer's open-string identity — one of the [`known`] consts,
    /// spelled as a **literal** in each impl rather than derived through
    /// [`kind`](Self::kind): the enum bridge closes at M8b-b3 and this
    /// method is what survives it. The registry's identity tests pin
    /// uniqueness and ledger membership across all twelve, and the
    /// state-key tripwire holds each handler's id to its `kind`'s spelling
    /// while both exist.
    fn id(&self) -> LayerId;

    /// Which pane surface this layer draws onto. See [`Surface`].
    fn surface(&self) -> Surface;

    /// This layer's position in the default draw order, **bottom to top** —
    /// a lower weight draws first and is occluded by higher ones.
    ///
    /// The weights encode `OverlayKind::all()`'s order — the REAL default
    /// draw order, which is neither the enum's declaration order nor
    /// `create_handlers()`'s vec order (SpcOutlook sits BELOW Radar here;
    /// the vec registers Radar second). The literal-list pin in
    /// `registry_identity_tests` is what holds the weights to it. Spaced by
    /// 10 so a future layer can sit between two without renumbering.
    fn draw_order_weight(&self) -> u32;

    fn display_name(&self) -> &str;

    fn render_mode(&self) -> RenderMode;

    /// Applies to a *new* pane only.
    fn default_enabled(&self) -> bool {
        false
    }

    // ── Data lifecycle ────────────────────────────────────────────────

    /// Bumped on every data replacement; drives texture cache invalidation.
    fn data_generation(&self) -> u64;

    /// A cheap token for **what this handler would draw**: consumers that
    /// re-render only when the picture changes compare it across frames.
    /// Unlike [`data_generation`], a refetch that returns the same content
    /// should keep it stable where the handler can tell — NWS alerts poll
    /// every two minutes and mostly return the same warning set, and a raster
    /// rebuilt on every poll is a raster rebuilt for nothing. The default is
    /// `data_generation`, correct for every handler that cannot tell (it may
    /// only over-refresh, never under-refresh). Called every frame, so it must
    /// not clone data.
    ///
    /// `rustdar_egui`'s `overlay_cache_token` is what asks — it is the token
    /// every [`RenderMode::Texture`] overlay's cached raster is keyed by, so a
    /// handler that answers this well is one whose overlay is not rasterized
    /// twice a minute for a byte-identical picture.
    ///
    /// [`data_generation`]: OverlayHandler::data_generation
    fn content_signature(&self) -> u64 {
        self.data_generation()
    }

    fn has_data(&self) -> bool;

    fn is_fetching(&self) -> bool;

    fn set_fetching(&mut self, fetching: bool);

    fn fetch_time(&self) -> Option<web_time::Instant>;

    /// Seconds. `None` means this overlay never auto-polls.
    fn auto_poll_interval(&self) -> Option<u64> {
        None
    }

    /// This layer's retry ledger; `None` for a handler that never fetches.
    ///
    /// Every fetching handler returns `Some(&self.state.retry)`. A handler that
    /// answers `None` while declaring an [`auto_poll_interval`] gets the old
    /// behaviour back — which is why
    /// `every_auto_polling_handler_backs_off_after_a_failure` fails if a new
    /// one forgets: its message is "auto-polls every {interval}s but keeps no
    /// retry ledger".
    ///
    /// [`auto_poll_interval`]: OverlayHandler::auto_poll_interval
    fn retry(&self) -> Option<&FetchRetry> {
        None
    }

    fn retry_mut(&mut self) -> Option<&mut FetchRetry> {
        None
    }

    /// How long until an **automatic** fetch of this layer may start; `None`
    /// when one never may.
    ///
    /// The single expression of the poll gate. `Gui::check_auto_polls` fires
    /// when this reads zero and `Gui::overlay_poll_delay` sleeps on it, so the
    /// schedule and the firing cannot disagree — they used to be two separate
    /// readings of `fetch_time`, one in whole seconds and one in durations.
    ///
    /// Two terms, whichever is later:
    ///
    /// - the **poll clock**, `fetch_time + interval`, which is what a healthy
    ///   layer runs on; and
    /// - the **backoff**, from [`crate::fetch_policy`], which is zero unless
    ///   something has failed.
    ///
    /// At the ladder's ceiling the two coincide by construction, because the
    /// ceiling *is* the interval.
    ///
    /// `None` for exactly two reasons: a layer that does not auto-poll, and one
    /// whose fetch is already in flight (what ends that wait is the result
    /// landing, which asks for its own frame).
    ///
    /// A [`broken`](FetchRetry::is_broken) layer used to be a third, and it was
    /// the wrong shape. `None` there is a state nothing can leave: no automatic
    /// fetch runs, so no success is ever recorded, so nothing ever clears the
    /// verdict — absorbing by construction, on a first 403, for a layer that
    /// carries tornado warnings. Broken is now just a very long rung of the
    /// backoff ([`FetchRetry::backoff`]), so it is the same two terms as every
    /// other state and there is one expression of the schedule rather than two.
    fn auto_fetch_delay(&self) -> Option<std::time::Duration> {
        let interval = std::time::Duration::from_secs(self.auto_poll_interval()?);
        if self.is_fetching() {
            return None;
        }
        let retry = self.retry();
        // Never fetched and nothing on record: due now.
        let by_clock = self.fetch_time().map_or(std::time::Duration::ZERO, |t| {
            interval.saturating_sub(t.elapsed())
        });
        let by_backoff = retry.map_or(std::time::Duration::ZERO, |r| r.backoff_remaining(interval));
        Some(by_clock.max(by_backoff))
    }

    fn item_count(&self) -> usize {
        0
    }

    /// Each handler owns its own toggle state; there is no central layer table.
    fn is_enabled(&self) -> bool {
        true
    }

    /// Meaningful only for simple toggle handlers.
    fn set_enabled(&mut self, _enabled: bool) {}

    /// One line of live status for the layer stack's row — `"3 shown · W/Wa"`,
    /// `"Day 1 · Categorical"` — or `None` for a handler with nothing worth
    /// saying, which renders as a row with no line under it rather than as a
    /// placeholder.
    ///
    /// Read-only and cheap: derived from state the handler already holds,
    /// called every frame the stack is on screen, never a reason to fetch or
    /// to clone data. A disabled handler returns `None` — its row is already
    /// dimmed, and a status line describing what a hidden layer would show
    /// reads as the layer being on.
    fn status_line(&self) -> Option<String> {
        None
    }

    // ── Fetching ──────────────────────────────────────────────────────

    fn create_fetch_tasks(&self, ctx: &FetchConfig) -> Vec<FetchTask> {
        let _ = ctx;
        Vec::new()
    }

    /// The handler downcasts the payload to its own type.
    fn apply_fetch_result(&mut self, result: FetchPayload);

    // ── Rendering (texture mode) ──────────────────────────────────────

    /// This handler's raster as a described job, or `None` when there is
    /// nothing to render — which is also the default, for the kinds whose
    /// pixels come from somewhere else entirely.
    ///
    /// The answer is a [`DescribedJob`] over the kind's own rasterizer input
    /// struct ([`crate::render::rasterize::AlertsInput`] and its siblings) —
    /// substrate-typed, so this crate never names the frontend — and the
    /// codec row that carries it is the one [`job_codec`] answers.
    ///
    /// This is the **only** way a handler's raster is dispatched. The closure
    /// twin it used to agree with (`prepare_rasterize`, a boxed `FnOnce` the
    /// wasm target could only run inline on the browser's one thread) is
    /// deleted, so a texture kind without a described input is a texture kind
    /// that cannot draw — `has_data()` must answer `false` exactly when this
    /// answers `None`
    /// (`handlers::texture_tests::every_texture_handler_agrees_with_its_own_rasterizer`),
    /// or `ui_map_pane`'s settle machinery asks for a render nothing can
    /// satisfy, every 100 ms, forever. The dispatch site (`rustdar_app`'s
    /// `spawn_overlay_render`) routes by an explicit match on kind, not by
    /// probing this method, so a kind moved between paths is a decision made
    /// there and tested in
    /// `handlers::texture_tests::every_texture_kind_rasterizes_as_a_described_job`.
    ///
    /// [`job_codec`]: OverlayHandler::job_codec
    fn prepare_job(&self, ctx: &RasterizeContext) -> Option<DescribedJob> {
        let _ = ctx;
        None
    }

    /// The codec row that encodes, decodes and runs this handler's described
    /// job — one of [`crate::render::jobs::JOB_CODECS`] — or `None` for a
    /// kind with no row.
    ///
    /// The row is this handler's whole statement of *how* its raster crosses
    /// a message port; [`prepare_job`] states *what* crosses. The pairing is
    /// bidirectional and pinned
    /// (`handlers::texture_tests::every_texture_handler_owns_exactly_one_codec_row`):
    /// every texture handler except `Radar` answers exactly one row, no row
    /// is claimed twice, and no row goes unclaimed. `RadarSites` answers its
    /// row here while its `prepare_job` stays `None` — the sites raster is
    /// still described at the dispatch, which can see the pane facts the
    /// handler cannot until per-pane handler state exists (M10).
    ///
    /// [`prepare_job`]: OverlayHandler::prepare_job
    fn job_codec(&self) -> Option<&'static JobCodec> {
        None
    }

    /// The `Arc<dyn OverlayItem>`s a hit-map kind's clicks resolve to,
    /// **index-aligned with the rows [`prepare_job`] describes** — `None` for
    /// a kind whose clicks resolve some other way, which is the default and
    /// every kind but storm reports and lightning.
    ///
    /// This is the half of a hit map that never crosses a message port. The
    /// dispatch captures it beside the described input and zips it with the
    /// returned cells at delivery
    /// ([`HitMap::from_cells`](crate::render::rasterize::HitMap::from_cells),
    /// where the order invariant is stated). A handler that implements this
    /// must build it and the input's rows from **one iteration of one list**
    /// — the shipped two build both from `state.data`, in order, out of one
    /// pair of methods sitting side by side — because an item at the wrong
    /// index is a hover that names the wrong report, which no guard downstream
    /// can see.
    ///
    /// Same `Some`-ness as [`prepare_job`] for the kinds that answer at all:
    /// an input with rows and no items would zip every hit to nothing.
    ///
    /// [`prepare_job`]: OverlayHandler::prepare_job
    fn hit_items(&self) -> Option<Vec<Arc<dyn OverlayItem>>> {
        None
    }

    // ── Click & selection ─────────────────────────────────────────────

    /// The features a click is tested against, borrowed from this handler.
    ///
    /// Called **only on a frame that has a click to resolve**, and only for a
    /// layer whose rasterizer produced no hit buffer — never as part of
    /// ordinary drawing. Building a `Vec` here is therefore fine; cloning
    /// geometry into it is still not, which is why [`ClickableItem`] borrows.
    fn clickable_items(&self) -> Vec<ClickableItem<'_>> {
        Vec::new()
    }

    /// The map labels this layer paints, in the pane's own draw pass.
    ///
    /// Split out of [`clickable_items`] because the two have opposite
    /// schedules: labels are wanted on every frame, geometry only on a frame
    /// with a click. Handed out as a borrow, not a `Vec`, so a per-pane
    /// per-frame call allocates nothing — a handler with labels precomputes
    /// them when its data changes.
    ///
    /// [`clickable_items`]: OverlayHandler::clickable_items
    fn map_labels(&self) -> &[OverlayLabel] {
        &[]
    }

    /// `true` if this handler owned the action.
    fn handle_popup_action(&mut self, _action: &PopupAction) -> bool {
        false
    }

    /// Drops selections whose `matches()` finds nothing in the refreshed data.
    fn retain_selections(&self, selections: &mut Vec<Arc<dyn OverlayItem>>);

    // ── Per-frame point rendering (opt-in for PerFramePoint mode) ─────

    fn per_frame_points(&self) -> &[MapPoint] {
        &[]
    }

    fn draw_point(&self, _id: u32, _painter: &mut dyn PointPainter, _ctx: &DrawPointContext) {}

    /// Screen pixels.
    fn point_hit_radius(&self, _zoom: f32) -> f32 {
        0.0
    }

    /// `None` suppresses the tooltip entirely.
    fn hover_text(&self, _id: u32, _ctx: &HoverContext<'_>) -> Option<String> {
        None
    }

    /// For gridded overlays only.
    fn hover_value_at(&self, _lat: f64, _lon: f64) -> Option<String> {
        None
    }

    fn legend(&self) -> Option<OverlayLegend> {
        None
    }

    // ── Declarative UI controls ───────────────────────────────────────

    /// Declarative: the egui crate renders these without overlay-specific code.
    fn controls(&self, _ctx: &PaneControlContext<'_>) -> Vec<ControlItem> {
        Vec::new()
    }

    /// The returned [`ControlEffect`] is how a handler asks the caller for a
    /// side-effect such as a refetch.
    fn apply_control(
        &mut self,
        _update: &ControlUpdate,
        _ctx: &mut PaneControlContextMut<'_>,
    ) -> ControlEffect {
        ControlEffect::None
    }

    // ── Per-pane state ────────────────────────────────────────────────

    /// e.g. selected product, loop state. `None` if there is no per-pane state.
    fn create_pane_state(&self) -> Option<FetchPayload> {
        None
    }

    // ── Config persistence ────────────────────────────────────────────

    fn serialize_state(&self) -> serde_json::Value {
        serde_json::Value::Null
    }

    fn deserialize_state(&mut self, _value: serde_json::Value) {}

    fn serialize_pane_state(&self, _state: &dyn Any) -> serde_json::Value {
        serde_json::Value::Null
    }

    fn deserialize_pane_state(&self, _value: serde_json::Value) -> Option<FetchPayload> {
        None
    }

    // ── Loop support (opt-in) ─────────────────────────────────────────

    fn supports_loop(&self) -> bool {
        false
    }

    /// Yields the timestamps of available frames, not the frames themselves.
    fn create_loop_list_task(
        &self,
        _ctx: &FetchConfig,
        _start: chrono::NaiveDateTime,
        _end: chrono::NaiveDateTime,
    ) -> Option<FetchTask> {
        None
    }

    fn create_loop_frame_task(
        &self,
        _ctx: &FetchConfig,
        _timestamp: chrono::NaiveDateTime,
    ) -> Option<FetchTask> {
        None
    }
}

pub struct FetchConfig {
    pub client: reqwest::Client,
    pub zone_cache_dir: Option<std::path::PathBuf>,
    /// Every origin a fetch may reach is declared here, not inline in URLs.
    pub sources: rustdar_source::origins::DataSources,
    /// `None` before the first frame is rendered. METAR must scope to this —
    /// the whole-country IEM form is 54 MB ungzipped; see
    /// [`crate::metar::networks`] for the no-viewport fallback.
    pub viewport: Option<rustdar_geo::GeoBounds>,
}

/// `Copy` so a rasterizer can be handed the whole thing rather than three loose
/// scalars: `zoom`, `is_dark` and `device_scale` are read together by every
/// symbol a texture overlay draws, and splitting them across an argument list
/// is how one of them comes to be forgotten at a call site.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RasterizeContext {
    pub is_dark: bool,
    pub zoom: f64,
    /// Texels per logical point the texture being rasterized was sized at —
    /// `rustdar_egui::overlay_cache::OverlayTexturePlan::pixels_per_point`.
    ///
    /// Every marker radius, label pill and stroke width in
    /// [`crate::render::rasterize`] is a length in **texels**, chosen from the
    /// map zoom so that it comes out at a sensible size on screen. That
    /// reasoning silently assumed one texel per point, which held for as long
    /// as the overlay textures were sized in points. They are sized in physical
    /// pixels now, so on a display at two of them per point an unscaled radius
    /// would draw at half the size it is meant to — and the site label pill,
    /// whose glyphs egui draws over it per frame at a fixed *point* size, would
    /// be half the width of the text it is a background for.
    ///
    /// `1.0` is one texel per point, which is every display that is not scaled
    /// and every reading this field had before it existed.
    pub device_scale: f32,
    /// The page's clock at dispatch, UTC.
    ///
    /// The one time-dependent rasterizer input in this crate: GLM's flash-age
    /// fade reads it and nothing else does. It lives here rather than being
    /// read inside the handler so that the capture site is the **dispatch** —
    /// the same moment every other page-side fact in this context is captured
    /// — and travels with the described job from there. A worker (or a
    /// handler) re-reading its own clock instead would render a different
    /// picture than the direct call; see
    /// [`GlmStrikesInput::now`](crate::render::rasterize::GlmStrikesInput::now)
    /// for the parity pin that keeps it on the wire.
    pub now: chrono::NaiveDateTime,
}

// ── Fetch-path thread bounds ─────────────────────────────────────────────
//
// `Send` on native because `tokio::spawn` requires `Send + 'static`; not on
// web, where reqwest's futures hold `Rc<RefCell<..>>` and are `!Send` by
// construction. Both bounds are load-bearing, not portability decoration.
//
// Two bounds, not one: the future, and the type-erased payload it sends back
// over an `mpsc::Sender`. Relaxing only the future still fails to compile.
//
// **Do not relax the renderer's bounds to match.** `rustdar-app`'s render
// dispatch spawns real OS threads and needs `Send` on every target; matching
// it to this compiles for web while silently breaking desktop threading.

#[cfg(not(target_arch = "wasm32"))]
pub type FetchPayload = Box<dyn Any + Send>;
#[cfg(target_arch = "wasm32")]
pub type FetchPayload = Box<dyn Any>;

#[cfg(not(target_arch = "wasm32"))]
pub type TaskFuture = Pin<Box<dyn Future<Output = FetchPayload> + Send>>;
#[cfg(target_arch = "wasm32")]
pub type TaskFuture = Pin<Box<dyn Future<Output = FetchPayload>>>;

pub struct FetchTask {
    pub kind: LayerId,
    pub future: TaskFuture,
}

// ── Overlay registry ─────────────────────────────────────────────────────

pub struct OverlayRegistry {
    handlers: Vec<Box<dyn OverlayHandler>>,
    /// Populated by map clicks; paged through in the popup.
    pub selected_overlays: Vec<Arc<dyn OverlayItem>>,
    pub selected_overlay_page: usize,
    /// The config value each handler was last loaded from, for the handlers
    /// whose state has not moved since — the dirty half of
    /// [`load_pane_configs`], which every pane calls on every frame.
    ///
    /// An entry means "this handler holds exactly what deserializing that
    /// value would give it", so the load is a no-op and can be skipped. Every
    /// route that can move a handler's state removes its entry
    /// ([`forget_loaded_config`]), so the skip is only ever taken where
    /// re-deserializing would have changed nothing — the reload discipline
    /// (`Gui::write_pane_overlay`: a change that never reached the config is
    /// undone next frame) is preserved exactly, because a change that *did*
    /// happen always cleared its entry first.
    ///
    /// [`load_pane_configs`]: OverlayRegistry::load_pane_configs
    /// [`forget_loaded_config`]: OverlayRegistry::forget_loaded_config
    loaded_configs: std::collections::HashMap<LayerId, serde_json::Value>,
}

impl Default for OverlayRegistry {
    fn default() -> Self {
        Self {
            handlers: super::handlers::create_handlers(),
            selected_overlays: Vec::new(),
            selected_overlay_page: 0,
            loaded_configs: std::collections::HashMap::new(),
        }
    }
}

impl OverlayRegistry {
    fn handler(&self, id: &LayerId) -> Option<&dyn OverlayHandler> {
        self.handlers.iter().find(|h| &h.id() == id).map(|h| &**h)
    }

    fn handler_mut(&mut self, id: &LayerId) -> Option<&mut dyn OverlayHandler> {
        self.forget_loaded_config(id);
        for handler in &mut self.handlers {
            if &handler.id() == id {
                return Some(&mut **handler);
            }
        }
        None
    }

    /// Drop `id`'s "already loaded" note, so the next
    /// [`load_pane_configs`](OverlayRegistry::load_pane_configs) re-applies
    /// its config rather than skipping it.
    ///
    /// Called from the one place every mutable handler borrow comes through
    /// ([`handler_mut`](OverlayRegistry::handler_mut)), so it cannot be
    /// forgotten by a new mutator: whatever a caller does with the borrow, the
    /// note is already gone.
    fn forget_loaded_config(&mut self, id: &LayerId) {
        self.loaded_configs.remove(id);
    }

    pub fn handlers(&self) -> impl Iterator<Item = &dyn OverlayHandler> {
        self.handlers.iter().map(|h| &**h)
    }

    pub fn get_handler(&self, kind: OverlayKind) -> Option<&dyn OverlayHandler> {
        self.handler(&kind.id())
    }

    pub fn get_handler_mut(&mut self, kind: OverlayKind) -> Option<&mut dyn OverlayHandler> {
        self.handler_mut(&kind.id())
    }

    /// The handler registered under `id`, if any — the open-string primary
    /// the M8b draw loop asks; an id no handler owns answers `None` (unknown
    /// ids are retained by callers and skipped at draw, never resolved here).
    pub fn handler_by_id(&self, id: &LayerId) -> Option<&dyn OverlayHandler> {
        self.handler(id)
    }

    /// The mutable half of [`handler_by_id`](Self::handler_by_id); routes
    /// through [`handler_mut`](Self::handler_mut), so the loaded-config note
    /// is dropped exactly as for every other mutable borrow.
    pub fn handler_by_id_mut(&mut self, id: &LayerId) -> Option<&mut dyn OverlayHandler> {
        self.handler_mut(id)
    }

    pub fn data_generation(&self, kind: OverlayKind) -> u64 {
        self.handler(&kind.id()).map_or(0, |h| h.data_generation())
    }

    /// [`OverlayHandler::content_signature`] for `kind`; `0` for a kind with
    /// no handler.
    pub fn content_signature(&self, kind: OverlayKind) -> u64 {
        self.handler(&kind.id())
            .map_or(0, |h| h.content_signature())
    }

    /// The NWS alert fetch payload for a known alert list, exactly as the
    /// network fetch would deliver it to [`apply_fetch_result`]. Public so a
    /// host (or its tests) can feed a chosen warning set through the one
    /// production ingest path instead of growing a parallel setter.
    ///
    /// [`apply_fetch_result`]: OverlayRegistry::apply_fetch_result
    #[doc(hidden)]
    pub fn nws_alerts_payload(alerts: Vec<crate::nws::alert::NwsAlert>) -> FetchPayload {
        Box::new(super::handlers::alert::NwsAlertFetchResult(Ok(
            crate::nws::fetch::ActiveAlerts::whole(alerts),
        )))
    }

    /// The NWS alert payload for a round whose **zone resolution came up
    /// short** — the alerts that did resolve, beside the report of what did
    /// not, exactly as `nws::fetch` delivers one.
    ///
    /// The counterpart to [`nws_alerts_payload`], for the same reason
    /// [`spc_discussions_failure_payload`] is the counterpart to its own: the
    /// half-delivered round is a production state, and a test that cannot build
    /// one has to reach past the ingest path and poke the ledger, which is
    /// exactly how a verdict stops reaching the UI without anything going red.
    ///
    /// [`nws_alerts_payload`]: OverlayRegistry::nws_alerts_payload
    /// [`spc_discussions_failure_payload`]: OverlayRegistry::spc_discussions_failure_payload
    #[doc(hidden)]
    pub fn nws_alerts_partial_payload(
        alerts: Vec<crate::nws::alert::NwsAlert>,
        zones: crate::nws::zones::ZoneResolution,
    ) -> FetchPayload {
        Box::new(super::handlers::alert::NwsAlertFetchResult(Ok(
            crate::nws::fetch::ActiveAlerts { alerts, zones },
        )))
    }

    /// The SPC Mesoscale Discussion fetch payload for a known MD list — the
    /// same seam as [`nws_alerts_payload`], for the same reason.
    ///
    /// [`nws_alerts_payload`]: OverlayRegistry::nws_alerts_payload
    #[doc(hidden)]
    pub fn spc_discussions_payload(
        discussions: Vec<crate::spc::discussion::SpcDiscussion>,
    ) -> FetchPayload {
        Box::new(super::handlers::discussion::SpcDiscussionFetchResult(Ok(
            discussions,
        )))
    }

    /// The SPC MD payload for a fetch that **failed**, exactly as the network
    /// path would deliver it. The counterpart to [`spc_discussions_payload`],
    /// and what lets a test drive real failing frames through the real ingest
    /// path rather than poking the ledger directly.
    ///
    /// [`spc_discussions_payload`]: OverlayRegistry::spc_discussions_payload
    #[doc(hidden)]
    pub fn spc_discussions_failure_payload(error: FetchError) -> FetchPayload {
        Box::new(super::handlers::discussion::SpcDiscussionFetchResult(Err(
            error,
        )))
    }

    /// Age `kind`'s retry ledger — see [`FetchRetry::rewind`].
    #[doc(hidden)]
    pub fn rewind_retry(&mut self, kind: OverlayKind, by: std::time::Duration) {
        if let Some(r) = self.handler_mut(&kind.id()).and_then(|h| h.retry_mut()) {
            r.rewind(by);
        }
    }

    pub fn has_data(&self, kind: OverlayKind) -> bool {
        self.handler(&kind.id()).is_some_and(|h| h.has_data())
    }

    pub fn is_fetching(&self, kind: OverlayKind) -> bool {
        self.handler(&kind.id()).is_some_and(|h| h.is_fetching())
    }

    pub fn set_fetching(&mut self, kind: OverlayKind, fetching: bool) {
        if let Some(h) = self.handler_mut(&kind.id()) {
            h.set_fetching(fetching);
        }
    }

    pub fn fetch_time(&self, kind: OverlayKind) -> Option<web_time::Instant> {
        self.handler(&kind.id()).and_then(|h| h.fetch_time())
    }

    pub fn auto_poll_interval(&self, kind: OverlayKind) -> Option<u64> {
        self.handler(&kind.id())
            .and_then(|h| h.auto_poll_interval())
    }

    /// [`OverlayHandler::auto_fetch_delay`] for `kind` — the one gate the
    /// automatic poll consults, and the only caller that may.
    pub fn auto_fetch_delay(&self, kind: OverlayKind) -> Option<std::time::Duration> {
        self.handler(&kind.id()).and_then(|h| h.auto_fetch_delay())
    }

    /// Wipe `kind`'s retry ledger because the **user** asked for a fetch.
    ///
    /// Called from `push_user_overlay_fetch` and nowhere else, so that "a user
    /// action is never made to wait out a backoff" holds by construction.
    pub fn clear_retry(&mut self, kind: OverlayKind) {
        if let Some(r) = self.handler_mut(&kind.id()).and_then(|h| h.retry_mut()) {
            r.clear();
        }
    }

    /// File a failure against `kind`'s ladder from outside the handler.
    ///
    /// The host uses this for failures that never reach `apply_fetch_result`
    /// because no task was ever built — see
    /// [`OverlayHandler::create_fetch_tasks`] returning empty.
    pub fn record_fetch_failure(&mut self, kind: OverlayKind, error: &FetchError) {
        if let Some(h) = self.handler_mut(&kind.id()) {
            h.set_fetching(false);
            if let Some(r) = h.retry_mut() {
                r.record_failure(error);
            }
        }
    }

    /// What `kind`'s last fetch said.
    ///
    /// Read by `Gui::set_pane_overlay_with_fetch` to decide whether switching a
    /// layer on should re-ask the origin; the *rendering* of it goes through
    /// [`Self::controls`], which prepends
    /// [`FetchRetry::status_note`](crate::fetch_policy::FetchRetry::status_note)
    /// for every layer rather than trusting handlers to remember.
    pub fn fetch_health(&self, kind: OverlayKind) -> Option<&FetchHealth> {
        self.handler(&kind.id())
            .and_then(|h| h.retry())
            .map(|r| r.health())
    }

    pub fn item_count(&self, kind: OverlayKind) -> usize {
        self.handler(&kind.id()).map_or(0, |h| h.item_count())
    }

    pub fn is_enabled(&self, kind: OverlayKind) -> bool {
        self.handler(&kind.id()).is_some_and(|h| h.is_enabled())
    }

    pub fn set_enabled(&mut self, kind: OverlayKind, enabled: bool) {
        if let Some(h) = self.handler_mut(&kind.id()) {
            h.set_enabled(enabled);
        }
    }

    /// [`OverlayHandler::status_line`] for `kind`, marked when the layer is not
    /// updating; `None` for a kind with no handler.
    ///
    /// The companion to [`Self::controls`], and the reason both live here rather
    /// than in the handlers. `controls` carries the full sentence, but it is in
    /// the layer's **options panel** — a user has to select the layer to read
    /// it, and nobody selects a layer that looks fine. The stack row is the
    /// surface that is always on screen, so it is where "these warnings stopped
    /// updating" has to appear if it is to be seen at all. A frozen alert set
    /// and a current one are identical on the map; this is the only difference
    /// visible without a click.
    ///
    /// Short by design: the row is one line beside a name, and the sentence that
    /// explains it is one click away.
    ///
    /// Only for a layer that is **on** — a hidden layer draws nothing, so
    /// nothing it holds can be misread, and its row is already dimmed. Free
    /// while healthy: both tests are discriminant tests and the `format!` runs
    /// only when there is something to say, which matters because this is asked
    /// of every layer in the stack every frame.
    ///
    /// # Two marks, because there are two ways to be wrong
    ///
    /// `not updating` is the time axis and `incomplete` is the coverage axis,
    /// and a layer can carry both at once: `! not updating, incomplete`. They
    /// are not interchangeable and must not be collapsed into one word — stale
    /// means wait or refresh, incomplete means look at what is missing and why,
    /// and a mark that cannot tell a user which one they are looking at is a
    /// mark they cannot act on.
    ///
    /// `incomplete` is a verdict rather than a count on purpose. The counts are
    /// one click away in the layer's options
    /// ([`DataCompleteness::status_note`]), and the handler's own line is
    /// already standing right beside this saying how much of it drew —
    /// `! incomplete - 85 of 297 shown - W/Wa/Adv/Oth`.
    pub fn status_line(&self, kind: OverlayKind) -> Option<String> {
        let handler = self.handler(&kind.id())?;
        let line = handler.status_line();
        if !handler.is_enabled() {
            return line;
        }
        let retry = handler.retry();
        let stale = retry.is_some_and(FetchRetry::is_unhealthy);
        let incomplete = retry.and_then(|r| r.coverage().status_mark());
        let mark = match (stale, incomplete) {
            (false, None) => return line,
            (true, None) => format!("{STATUS_MARK} not updating"),
            (false, Some(mark)) => format!("{STATUS_MARK} {mark}"),
            (true, Some(mark)) => format!("{STATUS_MARK} not updating, {mark}"),
        };
        Some(match line {
            Some(line) => format!("{mark} - {line}"),
            None => mark,
        })
    }

    pub fn clickable_items(&self, kind: OverlayKind) -> Vec<ClickableItem<'_>> {
        self.handler(&kind.id())
            .map_or_else(Vec::new, |h| h.clickable_items())
    }

    /// [`OverlayHandler::map_labels`] for `kind`; empty for a kind with no
    /// handler.
    pub fn map_labels(&self, kind: OverlayKind) -> &[OverlayLabel] {
        self.handler(&kind.id()).map_or(&[], |h| h.map_labels())
    }

    pub fn hover_value_at(&self, kind: OverlayKind, lat: f64, lon: f64) -> Option<String> {
        self.handler(&kind.id())
            .and_then(|h| h.hover_value_at(lat, lon))
    }

    pub fn legend(&self, kind: OverlayKind) -> Option<OverlayLegend> {
        self.handler(&kind.id()).and_then(|h| h.legend())
    }

    pub fn popup_content(
        &self,
        selected: &dyn OverlayItem,
        prefs: &UserPreferences,
    ) -> PopupContent {
        selected.popup_content(prefs)
    }

    /// Routes to the handler that owns `action.target`.
    pub fn handle_popup_action(&mut self, action: &PopupAction) -> bool {
        let kind = action.target.kind();
        self.handler_mut(&kind.id())
            .is_some_and(|h| h.handle_popup_action(action))
    }

    /// Re-runs `retain_selections` afterwards, since the data just changed.
    pub fn apply_fetch_result(&mut self, result: OverlayFetchResult) {
        let id = result.kind;
        // The one mutation route that reaches a handler without going through
        // `handler_mut` — it indexes, so that `retain_selections` can borrow
        // `selected_overlays` beside it. No shipped handler's
        // `apply_fetch_result` moves what `serialize_state` reports, so this
        // is belt-and-braces rather than a fix for a live bug; it is here so
        // "a handler's state moved ⇒ its note is gone" holds by construction
        // instead of by auditing twelve `apply_fetch_result` bodies.
        self.forget_loaded_config(&id);
        if let Some(idx) = self.handlers.iter().position(|h| h.id() == id) {
            self.handlers[idx].apply_fetch_result(result.data);
            self.handlers[idx].retain_selections(&mut self.selected_overlays);
        }
        if self.selected_overlay_page >= self.selected_overlays.len().max(1) {
            self.selected_overlay_page = 0;
        }
    }

    /// [`OverlayHandler::prepare_job`] through the registry — the only way a
    /// handler's raster is reached; the closure twin this sat beside is
    /// deleted.
    pub fn prepare_job(&self, kind: OverlayKind, ctx: &RasterizeContext) -> Option<DescribedJob> {
        self.handler(&kind.id()).and_then(|h| h.prepare_job(ctx))
    }

    /// [`OverlayHandler::job_codec`] through the registry — the codec row the
    /// dispatch frames and labels `kind`'s described job with.
    pub fn job_codec(&self, kind: OverlayKind) -> Option<&'static JobCodec> {
        self.handler(&kind.id()).and_then(|h| h.job_codec())
    }

    /// [`OverlayHandler::hit_items`] through the registry — the page-side
    /// half of a hit-map kind's described render, captured at the dispatch
    /// beside [`prepare_job`](Self::prepare_job).
    pub fn hit_items(&self, kind: OverlayKind) -> Option<Vec<Arc<dyn OverlayItem>>> {
        self.handler(&kind.id()).and_then(|h| h.hit_items())
    }

    pub fn create_fetch_tasks(&self, kind: OverlayKind, ctx: &FetchConfig) -> Vec<FetchTask> {
        self.handler(&kind.id())
            .map_or_else(Vec::new, |h| h.create_fetch_tasks(ctx))
    }

    /// The handler's own options, with its fetch health prepended.
    ///
    /// The note is added **here** rather than in each handler because handlers
    /// forget. Exactly one of the six fetching handlers rendered
    /// [`FetchRetry::status_note`] — SPC discussions — so NWS alerts, storm
    /// reports, METAR and lightning could each be frozen on data hours old with
    /// nothing on screen but an "Updated 47m ago" line that reads as a fact
    /// about the weather rather than about the app. A handler cannot forget
    /// something it does not write.
    ///
    /// **First**, not last. It changes what everything under it means: an empty
    /// alerts layer is a quiet afternoon or an unreachable origin, and a full
    /// one is current warnings or a frozen copy of last hour's. A caveat below
    /// the thing it qualifies is a caveat most people do not read.
    ///
    /// Two notes, not one merged sentence, and a layer can carry both. Staleness
    /// leads because it is the older and broader claim; incompleteness follows
    /// it and above everything else, because `212 of 297 alerts missing` also
    /// changes what `85 alerts shown` beneath it means.
    pub fn controls(&self, kind: OverlayKind, ctx: &PaneControlContext<'_>) -> Vec<ControlItem> {
        let Some(handler) = self.handler(&kind.id()) else {
            return Vec::new();
        };
        let mut items = handler.controls(ctx);
        // Inserted in reverse, so each lands above the one before it.
        if let Some(note) = handler.retry().and_then(|r| r.coverage().status_note()) {
            items.insert(0, ControlItem::InfoText { text: note });
        }
        if let Some(note) = handler.retry().and_then(FetchRetry::status_note) {
            items.insert(0, ControlItem::InfoText { text: note });
        }
        items
    }

    pub fn apply_control(
        &mut self,
        kind: OverlayKind,
        update: &ControlUpdate,
        ctx: &mut PaneControlContextMut<'_>,
    ) -> ControlEffect {
        if let Some(h) = self.handler_mut(&kind.id()) {
            h.apply_control(update, ctx)
        } else {
            ControlEffect::None
        }
    }

    pub fn render_mode(&self, kind: OverlayKind) -> Option<RenderMode> {
        self.handler(&kind.id()).map(|h| h.render_mode())
    }

    pub fn display_name(&self, kind: OverlayKind) -> &str {
        self.handler(&kind.id())
            .map_or("Unknown", |h| h.display_name())
    }

    pub fn default_enabled(&self, kind: OverlayKind) -> bool {
        self.handler(&kind.id())
            .is_some_and(|h| h.default_enabled())
    }

    /// Seeds a new pane's `enabled_overlays`; call after config deserialization.
    pub fn build_enabled_map(&self) -> std::collections::HashMap<OverlayKind, bool> {
        self.handlers
            .iter()
            .map(|h| (h.kind(), h.is_enabled()))
            .collect()
    }

    pub fn save_pane_configs(&self) -> std::collections::HashMap<OverlayKind, serde_json::Value> {
        self.handlers
            .iter()
            .map(|h| (h.kind(), h.serialize_state()))
            .collect()
    }

    /// Handlers absent from `configs` keep their current state.
    ///
    /// Every map pane calls this on every frame, for all twelve handlers, so
    /// the body has to be free when nothing changed: it used to deep-clone a
    /// `serde_json::Value` per handler and hand it to `deserialize_state`,
    /// which for two of them cloned again and rebuilt a `HashSet` through
    /// `serde_json::from_value` — config-changed-only work running at frame
    /// rate. A handler still holding what a value would give it is skipped by
    /// comparing against [`loaded_configs`], which allocates nothing.
    ///
    /// [`loaded_configs`]: OverlayRegistry::loaded_configs
    pub fn load_pane_configs(
        &mut self,
        configs: &std::collections::HashMap<OverlayKind, serde_json::Value>,
    ) {
        let Self {
            handlers,
            loaded_configs,
            ..
        } = self;
        for h in handlers {
            let Some(val) = configs.get(&h.kind()) else {
                continue;
            };
            let id = h.id();
            if loaded_configs.get(&id).is_some_and(|seen| seen == val) {
                continue;
            }
            h.deserialize_state(val.clone());
            loaded_configs.insert(id, val.clone());
        }
    }

    pub fn save_enabled_map(&self) -> std::collections::HashMap<OverlayKind, bool> {
        self.handlers
            .iter()
            .map(|h| (h.kind(), h.is_enabled()))
            .collect()
    }

    // ── Per-frame point rendering delegates ───────────────────────────

    pub fn per_frame_points(&self, kind: OverlayKind) -> &[MapPoint] {
        self.handler(&kind.id())
            .map_or(&[], |h| h.per_frame_points())
    }

    pub fn draw_point(
        &self,
        kind: OverlayKind,
        id: u32,
        painter: &mut dyn PointPainter,
        ctx: &DrawPointContext,
    ) {
        if let Some(h) = self.handler(&kind.id()) {
            h.draw_point(id, painter, ctx);
        }
    }

    pub fn point_hit_radius(&self, kind: OverlayKind, zoom: f32) -> f32 {
        self.handler(&kind.id())
            .map_or(0.0, |h| h.point_hit_radius(zoom))
    }

    pub fn hover_text(&self, kind: OverlayKind, id: u32, ctx: &HoverContext<'_>) -> Option<String> {
        self.handler(&kind.id()).and_then(|h| h.hover_text(id, ctx))
    }

    // ── Config persistence ────────────────────────────────────────────

    /// Keyed by the layer id **string** ([`LayerId::as_str`]) — byte-identical
    /// to the historical `Debug` spelling of `OverlayKind` these maps have
    /// always been keyed by (the M8a spelling pin holds the two equal), so
    /// every existing config file keeps matching. Renaming an id orphans its
    /// saved state; the ledger is append-only for exactly that reason. Null
    /// states are omitted.
    pub fn serialize_handler_states(&self) -> serde_json::Map<String, serde_json::Value> {
        let mut map = serde_json::Map::new();
        for h in &self.handlers {
            let val = h.serialize_state();
            if !val.is_null() {
                map.insert(h.id().as_str().to_string(), val);
            }
        }
        map
    }

    pub fn deserialize_handler_states(
        &mut self,
        states: &serde_json::Map<String, serde_json::Value>,
    ) {
        // A second source of handler state: whatever a pane config last put
        // there is no longer what the handlers hold.
        self.loaded_configs.clear();
        for h in &mut self.handlers {
            if let Some(val) = states.get(h.id().as_str()) {
                h.deserialize_state(val.clone());
            }
        }
    }
}

// ── Generic overlay kind ─────────────────────────────────────────────────

/// Each map layer in the per-pane draw order. Also a `HashMap` key for the
/// per-pane texture caches, and serialized by its `Debug` spelling.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum OverlayKind {
    ModelData,
    SpcOutlook,
    SpcDiscussions,
    NwsAlerts,
    StormReports,
    Lightning,
    Metar,
    Radar,
    CityLabels,
    RadarSites,
    UserLocation,
    ColorScale,
}

impl OverlayKind {
    /// **This order is the default draw order, bottom to top** — it is not the
    /// declaration order above and reordering it changes what occludes what.
    pub const fn all() -> &'static [OverlayKind] {
        &[
            OverlayKind::ModelData,
            OverlayKind::SpcOutlook,
            OverlayKind::Radar,
            OverlayKind::SpcDiscussions,
            OverlayKind::NwsAlerts,
            OverlayKind::StormReports,
            OverlayKind::Lightning,
            OverlayKind::Metar,
            OverlayKind::CityLabels,
            OverlayKind::RadarSites,
            OverlayKind::UserLocation,
            OverlayKind::ColorScale,
        ]
    }

    pub fn default_draw_order() -> Vec<OverlayKind> {
        Self::all().to_vec()
    }

    /// The M8a bridge: this variant's open-string identity. Each arm hands
    /// out the [`known`] const whose spelling equals the variant's `Debug`
    /// spelling — the equality the spelling-pin test below holds, and the
    /// reason M8's enum-to-string flip needs no config migration.
    pub fn id(self) -> LayerId {
        match self {
            Self::ModelData => known::MODEL_DATA,
            Self::SpcOutlook => known::SPC_OUTLOOK,
            Self::SpcDiscussions => known::SPC_DISCUSSIONS,
            Self::NwsAlerts => known::NWS_ALERTS,
            Self::StormReports => known::STORM_REPORTS,
            Self::Lightning => known::LIGHTNING,
            Self::Metar => known::METAR,
            Self::Radar => known::RADAR,
            Self::CityLabels => known::CITY_LABELS,
            Self::RadarSites => known::RADAR_SITES,
            Self::UserLocation => known::USER_LOCATION,
            Self::ColorScale => known::COLOR_SCALE,
        }
    }

    /// The bridge's other direction: the variant whose [`Self::id`] equals
    /// `id`, or `None` for an id no variant owns (open strings admit ids the
    /// enum never will — callers keep unknowns, they don't invent variants).
    /// Scans [`Self::all`] rather than matching a second literal spelling
    /// table: `id()` stays the one enum-to-string spelling site.
    pub fn from_id(id: &LayerId) -> Option<Self> {
        Self::all().iter().copied().find(|kind| &kind.id() == id)
    }
}

// ── Unified overlay fetch result ──────────────────────────────────────────

pub struct OverlayFetchResult {
    pub kind: LayerId,
    pub data: FetchPayload,
}

// ── Popup content descriptors ─────────────────────────────────────────────

/// Built here, drawn by the UI crate, which never learns which overlay type
/// produced it.
pub struct PopupContent {
    pub title: String,
    pub accent_rgb: [u8; 3],
    /// Desktop only; mobile auto-sizes to the screen.
    pub width: f32,
    /// Rendered in order.
    pub sections: Vec<PopupSection>,
    /// Buttons at the bottom.
    pub actions: Vec<PopupAction>,
}

pub enum PopupSection {
    Heading(String),
    Text(String),
    ColoredText {
        text: String,
        rgb: [u8; 3],
        bold: bool,
    },
    KeyValueGrid(Vec<(String, String)>),
    ScrollableText {
        text: String,
        monospace: bool,
        max_height: f32,
    },
    Separator,
    Link {
        label: String,
        url: String,
    },
}

/// The UI crate renders it; this crate defines what it means.
pub struct PopupAction {
    pub label: String,
    pub target: Arc<dyn OverlayItem>,
    pub kind: PopupActionKind,
}

pub enum PopupActionKind {
    /// NWS alerts only.
    HideFromMap,
}

#[cfg(test)]
mod pane_config_tests {
    use super::*;

    /// The pane config for "MDs off, everything else as built".
    fn mds_off(registry: &mut OverlayRegistry) -> std::collections::HashMap<OverlayKind, Value> {
        registry.set_enabled(OverlayKind::SpcDiscussions, false);
        let configs = registry.save_pane_configs();
        registry.set_enabled(OverlayKind::SpcDiscussions, true);
        configs
    }

    use serde_json::Value;

    /// A load applies, and a second load of the same map leaves the same
    /// answer — the skip is not allowed to be visible.
    #[test]
    fn loading_a_config_twice_lands_where_loading_it_once_did() {
        let mut registry = OverlayRegistry::default();
        let configs = mds_off(&mut registry);
        assert!(
            registry.is_enabled(OverlayKind::SpcDiscussions),
            "fixture: the handler is on, so the config has something to do",
        );

        registry.load_pane_configs(&configs);
        assert!(
            !registry.is_enabled(OverlayKind::SpcDiscussions),
            "the first load must apply the config",
        );

        // The frame-rate case: the same map, again, with nothing having
        // happened in between.
        registry.load_pane_configs(&configs);
        assert!(
            !registry.is_enabled(OverlayKind::SpcDiscussions),
            "a repeat load changed the answer",
        );
    }

    /// The one mutation route that reaches a handler without `handler_mut`
    /// still forgets what that handler was loaded from.
    ///
    /// Nothing observable depends on this today — no shipped
    /// `apply_fetch_result` writes a field its `deserialize_state` reads, so
    /// deleting the invalidation breaks no behaviour any test can see. That is
    /// exactly why the invariant is asserted directly: the skip at
    /// [`OverlayRegistry::load_pane_configs`] is built on "every mutation route
    /// forgets", and a route that quietly stops forgetting would only surface
    /// the first time a handler grew a field on both sides.
    #[test]
    fn a_fetch_result_forgets_the_config_the_handler_was_loaded_from() {
        let mut registry = OverlayRegistry::default();
        let configs = mds_off(&mut registry);

        registry.load_pane_configs(&configs);
        assert!(
            registry
                .loaded_configs
                .contains_key(&OverlayKind::SpcDiscussions.id()),
            "fixture: a load has to record what it read for the skip to exist",
        );

        registry.apply_fetch_result(OverlayFetchResult {
            kind: known::SPC_DISCUSSIONS,
            data: OverlayRegistry::spc_discussions_payload(Vec::new()),
        });

        assert!(
            !registry
                .loaded_configs
                .contains_key(&OverlayKind::SpcDiscussions.id()),
            "a fetch may move what `serialize_state` reports, so the next load \
             has to run rather than be skipped",
        );
    }

    /// The reload discipline survives the skip: a handler change that never
    /// reached the config is still undone by the next load.
    ///
    /// This is the one thing the "already loaded" note could have broken —
    /// skip a load whose handler has since moved and the change sticks
    /// forever, which is precisely the bug `Gui::write_pane_overlay`'s
    /// both-halves rule exists to prevent. Every mutable handler borrow drops
    /// the note, so there is no route to a stale skip; the two below are the
    /// routes the app actually takes.
    #[test]
    fn a_handler_change_outside_the_config_is_still_undone_by_the_next_load() {
        let mut registry = OverlayRegistry::default();
        let configs = mds_off(&mut registry);
        registry.load_pane_configs(&configs);
        assert!(!registry.is_enabled(OverlayKind::SpcDiscussions));

        // Route 1: the registry's own setter — the layer-stack eye's half
        // that forgot to write the config.
        registry.set_enabled(OverlayKind::SpcDiscussions, true);
        registry.load_pane_configs(&configs);
        assert!(
            !registry.is_enabled(OverlayKind::SpcDiscussions),
            "a `set_enabled` that never reached the config survived the \
             reload — the skip went stale",
        );

        // Route 2: a raw mutable handler borrow, which anything may take and
        // do anything with.
        registry
            .get_handler_mut(OverlayKind::SpcDiscussions)
            .expect("the MD handler is registered")
            .set_enabled(true);
        registry.load_pane_configs(&configs);
        assert!(
            !registry.is_enabled(OverlayKind::SpcDiscussions),
            "a change made through `get_handler_mut` survived the reload",
        );
    }

    /// Two panes with different configs both get theirs, in either order —
    /// the note is per handler and per value, not "the last map I saw".
    #[test]
    fn alternating_two_panes_configs_gives_each_pane_its_own() {
        let mut registry = OverlayRegistry::default();
        let off = mds_off(&mut registry);
        let on = registry.save_pane_configs();
        assert!(
            registry.is_enabled(OverlayKind::SpcDiscussions),
            "fixture: the two configs differ",
        );

        for _ in 0..3 {
            registry.load_pane_configs(&off);
            assert!(
                !registry.is_enabled(OverlayKind::SpcDiscussions),
                "the off pane did not get its config",
            );
            registry.load_pane_configs(&on);
            assert!(
                registry.is_enabled(OverlayKind::SpcDiscussions),
                "the on pane did not get its config",
            );
        }
    }
}

#[cfg(test)]
mod controls_parity_tests {
    use super::*;
    use crate::render::controls::{ControlItem, PaneControlContext};

    /// A control's identity, stripped of its live values. The *set of
    /// options offered* is what must not depend on state; a toggle's
    /// checked-ness, a dropdown's selection and a slider's value
    /// legitimately do.
    fn shape(item: &ControlItem) -> String {
        match item {
            ControlItem::Toggle { id, label, .. } => format!("toggle:{id}:{label}"),
            ControlItem::Dropdown { id, label, .. } => format!("dropdown:{id}:{label}"),
            ControlItem::Slider { id, label, .. } => format!("slider:{id}:{label}"),
            ControlItem::ButtonRow { buttons } => {
                let ids: Vec<&str> = buttons.iter().map(|b| b.id).collect();
                format!("buttons:{}", ids.join(","))
            }
            ControlItem::InfoText { text } => format!("info:{text}"),
            ControlItem::Heading { text } => format!("heading:{text}"),
            ControlItem::Section { label, items, .. } => {
                let children: Vec<String> = items.iter().map(shape).collect();
                format!("section:{label}[{}]", children.join(";"))
            }
            ControlItem::Separator => "separator".into(),
        }
    }

    /// Every handler offers the identical control tree hidden and shown —
    /// the every-option rule: the stack row's eye hides *pixels*, never
    /// options. A handler whose disabled tree shrank stranded its
    /// sub-options exactly when a user goes looking for why a layer is off
    /// or what it will show once on (the M9.1 user report), so each of the
    /// twelve is pinned by name.
    #[test]
    fn every_handlers_control_tree_is_identical_hidden_and_shown() {
        let mut registry = OverlayRegistry::default();
        let kinds: Vec<OverlayKind> = registry.handlers().map(|h| h.kind()).collect();
        assert_eq!(
            kinds.len(),
            12,
            "the registry carries all twelve handlers - the walk below \
             must cover every one"
        );
        let ctx = PaneControlContext {
            pane_idx: 0,
            pane_state: None,
        };
        for kind in kinds {
            registry.set_enabled(kind, true);
            let shown: Vec<String> = registry.controls(kind, &ctx).iter().map(shape).collect();
            registry.set_enabled(kind, false);
            let hidden: Vec<String> = registry.controls(kind, &ctx).iter().map(shape).collect();
            assert_eq!(
                shown, hidden,
                "{kind:?} offers a different option set hidden than shown - \
                 the eye must change pixels, never the options"
            );
        }
    }
}

#[cfg(test)]
mod retry_ledger_tests {
    use super::*;
    use crate::fetch_policy::FetchError;
    use crate::render::handlers::create_handlers;

    /// **The copy-paste guard.** Every handler that auto-polls must keep a
    /// retry ledger, and a transient failure must actually push its next
    /// automatic attempt into the future.
    ///
    /// Written over `create_handlers()` rather than over a list of names, so a
    /// new overlay is covered the day it is registered. All six auto-polling
    /// handlers had the identical defect — log the error, clear `fetching`,
    /// leave `fetch_time` alone — because the shape was copied from whichever
    /// one came first, and nothing stopped the seventh from copying it too.
    /// (`SpcOutlook` writes the same error branch but declares no interval, so
    /// it never reached the poll gate; it is fixed alongside them rather than
    /// left as the one copy of the old shape.)
    ///
    /// A handler that keeps no ledger gets the old behaviour exactly: the poll
    /// gate falls back to `fetch_time`, which a failure never stamps, and the
    /// layer is due again on the next frame.
    #[test]
    fn every_auto_polling_handler_backs_off_after_a_failure() {
        let mut checked = 0;
        for handler in create_handlers().iter_mut() {
            let Some(interval) = handler.auto_poll_interval() else {
                continue;
            };
            checked += 1;
            let kind = handler.kind();

            assert!(
                handler.retry().is_some(),
                "{kind:?} auto-polls every {interval}s but keeps no retry \
                 ledger, so a failed fetch leaves it due on every frame",
            );

            assert_eq!(
                handler.auto_fetch_delay(),
                Some(std::time::Duration::ZERO),
                "{kind:?} has never been fetched, so it is due now",
            );

            handler
                .retry_mut()
                .expect("just asserted present")
                .record_failure(&FetchError::transient("network down"));

            let delay = handler
                .auto_fetch_delay()
                .expect("a transient failure is still owed an eventual retry");
            assert!(
                !delay.is_zero(),
                "{kind:?} is due again immediately after a failed fetch — this \
                 is the per-frame retry storm",
            );
            assert!(
                delay <= std::time::Duration::from_secs(interval),
                "{kind:?} backs off past its own {interval}s poll interval, so \
                 a failure recovers slower than an ordinary refresh: {delay:?}",
            );
        }
        assert_eq!(
            checked, 6,
            "the six auto-polling handlers that shared the defect must all \
             still be covered; a new one is not exempt, and a removed one \
             should be removed from this count deliberately",
        );
    }

    /// A failure must not leave the layer stuck "Fetching...", which is the
    /// other way to make the ledger moot: `is_fetching` suppresses the poll, so
    /// a handler that never clears it never polls again.
    #[test]
    fn recording_a_failure_ends_the_fetch() {
        let mut state: OverlayState<Vec<u8>, Whole> = OverlayState::new();
        state.fetching = true;
        state.record_failure(&FetchError::transient("network down"));
        assert!(!state.fetching);
        assert_eq!(state.fetch_time, None, "a failure must not stamp the clock");

        state.fetching = true;
        state.set_data(vec![1]);
        assert!(!state.fetching);
        assert!(state.fetch_time.is_some());
    }

    /// **The silence guard.** Every layer that can fail must *say* it is
    /// failing, in its own options, without its handler having written a line
    /// of code to do it.
    ///
    /// Exactly one of the six — SPC discussions — used to push
    /// `status_note()` itself. NWS alerts, storm reports, METAR and lightning
    /// pushed nothing, so a frozen warning set looked identical to a current
    /// one and the only thing on screen was an "Updated 47m ago" line that
    /// reads as a fact about the weather. Written over `create_handlers()`, so
    /// a seventh overlay is covered the day it is registered rather than the
    /// day someone remembers.
    #[test]
    fn every_fetching_layer_says_so_when_it_is_failing() {
        use crate::render::controls::{ControlItem, PaneControlContext};

        let ctx = PaneControlContext {
            pane_idx: 0,
            pane_state: None,
        };
        let mut registry = OverlayRegistry::default();
        let kinds: Vec<OverlayKind> = registry
            .handlers()
            .filter(|h| h.retry().is_some())
            .map(|h| h.kind())
            .collect();
        assert_eq!(
            kinds.len(),
            7,
            "the seven fetching handlers must all be covered",
        );

        for kind in kinds {
            let quiet = registry.controls(kind, &ctx).len();

            registry.record_fetch_failure(kind, &FetchError::transient("connection refused"));
            let note = registry
                .fetch_health(kind)
                .and_then(|_| {
                    registry
                        .controls(kind, &ctx)
                        .into_iter()
                        .find_map(|item| match item {
                            ControlItem::InfoText { text }
                                if text.contains("connection refused") =>
                            {
                                Some(text)
                            }
                            _ => None,
                        })
                })
                .unwrap_or_else(|| {
                    panic!(
                        "{kind:?} is failing and its options say nothing about it — \
                         a stale layer that looks current is the whole bug",
                    )
                });
            assert!(
                note.contains("stale"),
                "{kind:?} reports the error but not what it means for what is \
                 drawn: {note}",
            );
            assert_eq!(
                registry.controls(kind, &ctx).len(),
                quiet + 1,
                "{kind:?} gained more than the one health line",
            );

            // First, above everything it qualifies.
            assert!(
                matches!(
                    registry.controls(kind, &ctx).first(),
                    Some(ControlItem::InfoText { text }) if text.contains("connection refused"),
                ),
                "{kind:?} buried its health note below the options it changes the \
                 meaning of",
            );

            registry.clear_retry(kind);
            assert_eq!(
                registry.controls(kind, &ctx).len(),
                quiet,
                "{kind:?} kept a health line after recovering",
            );
        }
    }

    /// The enable-fetch rule, in the four states it has to tell apart.
    ///
    /// The third row is the fix: a layer *holding* data that has since gone
    /// stale must re-ask when the user switches it on. The rule was
    /// `!has_data` alone, so toggling a frozen alerts layer off and on did
    /// nothing whatsoever — in the one case where a user is most likely to try
    /// it, on the layer where being wrong matters most.
    #[test]
    fn switching_a_layer_on_re_asks_when_there_is_nothing_worth_trusting() {
        let cases = [
            (false, None, true, "nothing drawn: ask"),
            (
                true,
                None,
                false,
                "fresh data drawn: do not spend a request",
            ),
            (true, Some(false), true, "data drawn but failing: ask"),
            (true, Some(true), true, "data drawn but broken: ask"),
        ];
        for (has_data, unhealthy, expected, why) in cases {
            let mut state: OverlayState<Vec<u8>, Whole> = OverlayState::new();
            if has_data {
                state.set_data(vec![1]);
            }
            match unhealthy {
                None => {}
                Some(broken) => {
                    let n = if broken {
                        crate::fetch_policy::REFUSALS_BEFORE_BROKEN
                    } else {
                        1
                    };
                    for _ in 0..n {
                        state
                            .retry
                            .record_failure(&FetchError::permanent("HTTP 400"));
                    }
                    assert_eq!(state.retry.is_broken(), broken, "premise: {why}");
                }
            }
            assert_eq!(state.enable_should_refetch(has_data), expected, "{why}");
        }
    }

    /// **The always-visible half.** The full note is in the layer's options
    /// panel, which a user has to select the layer to reach — and nobody
    /// selects a layer that looks fine. The stack row is on screen the whole
    /// time, so a layer that stopped updating has to say so *there* or it is
    /// only discoverable by someone already suspicious.
    #[test]
    fn a_failing_layer_is_marked_on_the_always_visible_stack_row() {
        let mut registry = OverlayRegistry::default();
        let kinds: Vec<OverlayKind> = registry
            .handlers()
            .filter(|h| h.retry().is_some())
            .map(|h| h.kind())
            .collect();

        for kind in kinds {
            registry.set_enabled(kind, true);
            let healthy = registry.status_line(kind);
            assert!(
                !healthy
                    .as_deref()
                    .is_some_and(|l| l.contains("not updating")),
                "{kind:?} claims to be failing before anything has failed: {healthy:?}",
            );

            registry.record_fetch_failure(kind, &FetchError::transient("connection refused"));
            let marked = registry
                .status_line(kind)
                .unwrap_or_else(|| panic!("{kind:?} says nothing on the row while failing"));
            assert!(
                marked.starts_with("! not updating"),
                "{kind:?}: the mark must lead the row, not trail whatever the \
                 layer was already saying: {marked}",
            );
            if let Some(healthy) = healthy.as_deref() {
                assert!(
                    marked.contains(healthy),
                    "{kind:?} lost its own status line to the mark: {marked}",
                );
            }

            registry.clear_retry(kind);
            assert_eq!(
                registry.status_line(kind),
                healthy,
                "{kind:?} kept the mark after recovering",
            );
        }
    }

    /// The alerts of one poll: `placed` of them carrying the outlines a
    /// zone-based alert only has once `resolve_zone_geometries` has fetched
    /// them, the rest carrying none — which is exactly what the handler is
    /// handed when zone boundaries fail.
    fn alerts_where_only_some_resolved(
        total: usize,
        placed: usize,
    ) -> Vec<crate::nws::alert::NwsAlert> {
        use crate::nws::alert::{AlertCategory, NwsAlert};
        use crate::types::{HatchPattern, OverlayFeature};
        (0..total)
            .map(|i| NwsAlert {
                id: format!("urn:oid:2.49.0.1.840.0.{i}"),
                event: "Tornado Warning".to_string(),
                category: AlertCategory::Warning,
                severity: "Severe".parse().unwrap(),
                urgency: "Immediate".parse().unwrap(),
                certainty: "Observed".parse().unwrap(),
                headline: None,
                description: String::new(),
                instruction: None,
                area_desc: String::new(),
                sender_name: String::new(),
                effective: String::new(),
                expires: String::new(),
                onset: None,
                ends: None,
                affected_zones: vec!["https://api.weather.gov/zones/county/OKC001".to_string()],
                features: std::sync::Arc::new(if i < placed {
                    vec![OverlayFeature::new(
                        vec![vec![vec![(35.0, -97.0), (36.0, -97.0), (36.0, -96.0)]]],
                        [0, 0, 0, 0],
                        [0, 0, 0, 0],
                        "Tornado Warning".to_string(),
                        String::new(),
                        HatchPattern::None,
                    )]
                } else {
                    Vec::new()
                }),
            })
            .collect()
    }

    /// **The bug, end to end, in the numbers it was observed in.**
    ///
    /// 297 warnings arrive, 212 of them referencing zone boundaries that would
    /// not resolve, so 85 are on the map. Every check in this crate passed: the
    /// alert fetch genuinely succeeded, so the ladder is clear, the clock is
    /// fresh and the health is `Ok`. The row said `297 shown - W/Wa/Adv` and the
    /// options said `Updated 0s ago`, and both were *true statements* about the
    /// fetch and lies about the map.
    ///
    /// Driven through [`OverlayRegistry::apply_fetch_result`] — the one path
    /// production takes — rather than by writing to the ledger, because every
    /// link is load-bearing and any of them going quiet reproduces the bug:
    /// the resolver counting, the fetch carrying, the handler filing through
    /// `set_data_with_coverage`, and the registry rendering. Delete any one and
    /// this fails.
    #[test]
    fn a_layer_that_under_drew_says_so_on_its_row_and_in_its_options() {
        use crate::nws::zones::{ZoneFailure, ZoneResolution};

        let ctx = PaneControlContext {
            pane_idx: 0,
            pane_state: None,
        };
        let kind = OverlayKind::NwsAlerts;
        let mut registry = OverlayRegistry::default();

        // Healthy first, so the difference below is this poll's and not the
        // fixture's.
        registry.apply_fetch_result(OverlayFetchResult {
            kind: kind.id(),
            data: OverlayRegistry::nws_alerts_payload(alerts_where_only_some_resolved(297, 297)),
        });
        assert_eq!(
            registry.status_line(kind).as_deref(),
            Some("297 shown - W/Wa/Adv/Oth"),
            "a whole round must read as a plain count",
        );
        let quiet = registry.controls(kind, &ctx).len();

        registry.apply_fetch_result(OverlayFetchResult {
            kind: kind.id(),
            data: OverlayRegistry::nws_alerts_partial_payload(
                alerts_where_only_some_resolved(297, 85),
                ZoneResolution {
                    alerts_expected: 297,
                    alerts_complete: 85,
                    alerts_partial: 0,
                    alerts_missing: 212,
                    zones_requested: 1200,
                    zones_resolved: 995,
                    failures: vec![(ZoneFailure::Http(503), 198), (ZoneFailure::NoBoundary, 7)],
                },
            ),
        });

        // The always-visible half. Both halves of it: the mark, and a count
        // that no longer claims 297 warnings are on a map holding 85.
        assert_eq!(
            registry.status_line(kind).as_deref(),
            Some("! incomplete - 85 of 297 shown - W/Wa/Adv/Oth"),
            "a layer drawing 85 of 297 warnings must not read as healthy",
        );

        // The one-click half: what is missing, why, and that it is not the
        // other fault.
        let note = registry
            .controls(kind, &ctx)
            .into_iter()
            .find_map(|item| match item {
                ControlItem::InfoText { text } if text.starts_with("Incomplete") => Some(text),
                _ => None,
            })
            .expect("the options must say what the row is marking");
        for expected in [
            "missing 212 of 297 alerts",
            "995 of 1200 zone boundaries resolved",
            "198 HTTP 503",
            "7 no usable boundary",
            "Not the same as stale data",
        ] {
            assert!(
                note.contains(expected),
                "the note must be countable and say why - missing {expected:?}: {note}",
            );
        }
        assert_eq!(
            registry.controls(kind, &ctx).len(),
            quiet + 1,
            "exactly one line was added, and the layer's own options are intact",
        );
        assert!(
            matches!(
                registry.controls(kind, &ctx).first(),
                Some(ControlItem::InfoText { text }) if text.starts_with("Incomplete"),
            ),
            "the note must lead the options it changes the meaning of",
        );

        // Incomplete is **not** stale, and the ledger must not have confused
        // them: this round succeeded, so the clock is fresh and the ladder clear.
        assert_eq!(
            registry.fetch_health(kind),
            Some(&FetchHealth::Ok),
            "a round that delivered 85 real warnings is a good answer, and \
             filing it as a failure would back the layer off from the retry \
             that could complete it",
        );
        let since = registry
            .fetch_time(kind)
            .expect("a round that delivered data stamps the clock")
            .elapsed();
        assert!(
            since < std::time::Duration::from_secs(1),
            "the partial round must stamp its own clock: {since:?}",
        );

        // A recovered poll clears the mark without the handler saying so.
        registry.apply_fetch_result(OverlayFetchResult {
            kind: kind.id(),
            data: OverlayRegistry::nws_alerts_payload(alerts_where_only_some_resolved(297, 297)),
        });
        assert_eq!(
            registry.status_line(kind).as_deref(),
            Some("297 shown - W/Wa/Adv/Oth"),
            "the mark outlived the round it was about",
        );
    }

    /// The two faults are independent, and a layer with both says both.
    ///
    /// `! incomplete` and `! not updating` answer different questions — what is
    /// missing from the map, and whether what is on it is current — and a user
    /// looking at a warning layer needs to know which one they have. Collapsing
    /// them into one word would make the mark unactionable, and picking one
    /// would hide the other exactly when both are true: a layer that under-drew
    /// and has since stopped fetching is the worst state there is.
    #[test]
    fn a_layer_that_is_both_stale_and_incomplete_says_both() {
        use crate::nws::zones::{ZoneFailure, ZoneResolution};

        let kind = OverlayKind::NwsAlerts;
        let mut registry = OverlayRegistry::default();
        registry.apply_fetch_result(OverlayFetchResult {
            kind: kind.id(),
            data: OverlayRegistry::nws_alerts_partial_payload(
                alerts_where_only_some_resolved(297, 85),
                ZoneResolution {
                    alerts_expected: 297,
                    alerts_complete: 85,
                    alerts_missing: 212,
                    zones_requested: 1200,
                    zones_resolved: 995,
                    failures: vec![(ZoneFailure::Http(503), 205)],
                    ..ZoneResolution::default()
                },
            ),
        });
        assert_eq!(
            registry.status_line(kind).as_deref(),
            Some("! incomplete - 85 of 297 shown - W/Wa/Adv/Oth"),
        );

        // The origin then goes away entirely. What is drawn is now both a
        // subset and out of date.
        registry.record_fetch_failure(kind, &FetchError::transient("connection refused"));
        assert_eq!(
            registry.status_line(kind).as_deref(),
            Some("! not updating, incomplete - 85 of 297 shown - W/Wa/Adv/Oth"),
            "a failure must not overwrite the coverage verdict, or the reverse",
        );

        let ctx = PaneControlContext {
            pane_idx: 0,
            pane_state: None,
        };
        let notes: Vec<String> = registry
            .controls(kind, &ctx)
            .into_iter()
            .filter_map(|item| match item {
                ControlItem::InfoText { text } => Some(text),
                _ => None,
            })
            .collect();
        assert!(
            notes.iter().any(|n| n.contains("may be stale"))
                && notes.iter().any(|n| n.starts_with("Incomplete")),
            "both faults must be spelled out, not merged into one: {notes:?}",
        );

        // A user pressing Refresh has not yet been given the 212 zones that
        // failed. Clearing the ladder must not claim they arrived.
        registry.clear_retry(kind);
        assert_eq!(
            registry.status_line(kind).as_deref(),
            Some("! incomplete - 85 of 297 shown - W/Wa/Adv/Oth"),
            "clearing the retry ladder marked the layer whole before the answer \
             that would make it whole had landed",
        );
    }

    /// Switching a layer on when it is drawing 85 of 297 warnings must re-ask.
    ///
    /// The same argument as the staleness clause one axis over, and the same
    /// user: someone who can see the layer is wrong and toggles it, which is the
    /// first thing anyone tries. The zones that failed are not cached, so a
    /// re-ask retries precisely them.
    #[test]
    fn switching_on_a_layer_that_under_drew_re_asks() {
        use crate::fetch_policy::DataCompleteness;

        let mut state: OverlayState<Vec<u8>, Assembled> = OverlayState::new();
        state.set_data_with_coverage(
            vec![1],
            DataCompleteness {
                expected: 297,
                missing: 212,
                ..DataCompleteness::default()
            },
        );
        assert!(!state.retry.is_unhealthy(), "premise: the round succeeded");
        assert!(state.enable_should_refetch(true));

        // The recovered round, spelled the way an assembled layer has to spell
        // it: there is no `set_data` on this state, and the whole report is
        // what clears the mark.
        state.set_data_with_coverage(vec![1], DataCompleteness::default());
        assert!(
            !state.enable_should_refetch(true),
            "a whole round must not spend a request on being switched on",
        );
    }

    /// A hidden layer draws nothing, so nothing it holds can be misread, and
    /// its row is already dimmed. Marking it would put a warning on every layer
    /// a user has deliberately switched off.
    #[test]
    fn a_hidden_layer_is_not_marked_on_the_stack_row() {
        let mut registry = OverlayRegistry::default();
        let kind = OverlayKind::NwsAlerts;
        registry.record_fetch_failure(kind, &FetchError::transient("connection refused"));
        registry.set_enabled(kind, false);
        assert!(
            !registry
                .status_line(kind)
                .is_some_and(|l| l.contains("not updating")),
            "a layer that is switched off must not carry a staleness mark",
        );
    }

    /// A fetch already in flight is never doubled by switching the layer on,
    /// however unhealthy the ledger looks — the result that is coming is the
    /// answer.
    #[test]
    fn switching_a_layer_on_does_not_double_a_fetch_in_flight() {
        let mut state: OverlayState<Vec<u8>, Whole> = OverlayState::new();
        state
            .retry
            .record_failure(&FetchError::transient("timeout"));
        state.fetching = true;
        assert!(!state.enable_should_refetch(false));
    }
}

#[cfg(test)]
mod state_key_tests {
    use crate::render::handlers::create_handlers;

    /// Every name saved handler state has ever been filed under, as a
    /// **literal** list — the self-verifying-inventory discipline: the live
    /// set is checked against it below, so neither side can rot alone.
    const STATE_KEYS: [&str; 12] = [
        "ModelData",
        "SpcOutlook",
        "SpcDiscussions",
        "NwsAlerts",
        "StormReports",
        "Lightning",
        "Metar",
        "Radar",
        "CityLabels",
        "RadarSites",
        "UserLocation",
        "ColorScale",
    ];

    /// **The tripwire for the day `OverlayKind` stops being a plain enum.**
    ///
    /// `serialize_handler_states` and `deserialize_handler_states` key the
    /// saved handler state by `h.id().as_str()` since M8b-b1 (the two
    /// `format!("{:?}")` sites this test was written against are gone — m1).
    /// That is safe only because every handler's `id()` spells exactly what
    /// the `{:?}` key always spelled, which is the name already sitting in
    /// every user's config file. A handler whose id drifts from its
    /// `kind()`'s `Debug`/serde spelling — a typo, or two handlers' ids
    /// swapped — silently stops matching the file, and that user's saved
    /// handler state is orphaned without a single error. This test is what
    /// fails instead.
    ///
    /// New persistence code must key by [`LayerId::as_str`], never by a new
    /// `{:?}` site (`LayerId`'s derived `Debug` prints `LayerId("…")` —
    /// visibly wrong on purpose).
    ///
    /// [`LayerId::as_str`]: rustdar_source::id::LayerId::as_str
    #[test]
    fn handler_state_keys_are_the_twelve_debug_spellings_and_serde_agrees() {
        let handlers = create_handlers();
        assert_eq!(
            handlers.len(),
            STATE_KEYS.len(),
            "a handler was registered or retired without updating the literal \
             key list; saved state for it has no pinned spelling",
        );
        for h in &handlers {
            let kind = h.kind();
            let debug_spelling = format!("{kind:?}");
            let serde_spelling = serde_json::to_value(kind)
                .expect("a fieldless enum serializes to its variant name");
            let serde_spelling = serde_spelling
                .as_str()
                .expect("a unit variant is a JSON string on the wire");
            assert_eq!(
                debug_spelling, serde_spelling,
                "the Debug and serde spellings of this kind disagree — the \
                 historical key and the on-disk spelling just diverged",
            );
            assert_eq!(
                h.id().as_str(),
                debug_spelling,
                "this handler's id() does not spell its kind()'s Debug name — \
                 the id-keyed handler-state maps just orphaned its saved \
                 state (a swap of two handlers' ids also fails here)",
            );
            assert!(
                STATE_KEYS.contains(&debug_spelling.as_str()),
                "{debug_spelling} is not one of the twelve names saved configs \
                 file handler state under — a renamed variant orphans the \
                 user's saved state for it",
            );
        }
    }
}

#[cfg(test)]
mod registry_identity_tests {
    use rustdar_source::id::LAYER_ID_LEDGER;

    use crate::render::handlers::create_handlers;

    /// b1 pin: no two handlers answer the same id. The open string has no
    /// compiler to refuse a duplicate the way the enum's match arms did, so
    /// the registry pins uniqueness instead — the replacement rigor the M8c
    /// enum deletion depends on.
    #[test]
    fn no_two_handlers_share_an_id() {
        let handlers = create_handlers();
        assert_eq!(handlers.len(), 12, "the walk below must cover all twelve");
        let mut seen = std::collections::HashSet::new();
        for h in &handlers {
            assert!(
                seen.insert(h.id()),
                "two handlers both register {:?} — the second shadows the \
                 first at every registry lookup",
                h.id(),
            );
        }
    }

    /// b1 pin: every handler's id sits in the append-only ledger — a handler
    /// cannot register a spelling `LAYER_ID_LEDGER` does not carry.
    #[test]
    fn every_handlers_id_sits_in_the_ledger() {
        for h in &create_handlers() {
            assert!(
                LAYER_ID_LEDGER.contains(&h.id().as_str()),
                "{}'s id is missing from LAYER_ID_LEDGER — ledger rows are \
                 append-only and this one was never appended",
                h.display_name(),
            );
        }
    }

    /// **The draw-weight order pin.** Sorting the registered handlers by
    /// `draw_order_weight` yields EXACTLY the historical default draw order,
    /// bottom to top, spelled out as literals.
    ///
    /// Three orders exist in this crate and only this one is the draw order:
    /// the enum's declaration order differs (SpcDiscussions before Radar),
    /// and `create_handlers()`'s vec order differs (Radar second). The
    /// weights encode `OverlayKind::all()`'s order — SpcOutlook BELOW Radar —
    /// and this literal list is the pin that keeps a weight edit from
    /// silently reordering what occludes what on every user's map.
    #[test]
    fn draw_order_weights_encode_the_default_draw_order() {
        let mut handlers = create_handlers();
        let mut weights: Vec<u32> = handlers.iter().map(|h| h.draw_order_weight()).collect();
        weights.sort_unstable();
        weights.dedup();
        assert_eq!(
            weights.len(),
            handlers.len(),
            "two handlers share a draw-order weight — their relative order \
             would be an accident of registration order",
        );
        handlers.sort_by_key(|h| h.draw_order_weight());
        let ids: Vec<String> = handlers
            .iter()
            .map(|h| h.id().as_str().to_string())
            .collect();
        assert_eq!(
            ids,
            [
                "ModelData",
                "SpcOutlook",
                "Radar",
                "SpcDiscussions",
                "NwsAlerts",
                "StormReports",
                "Lightning",
                "Metar",
                "CityLabels",
                "RadarSites",
                "UserLocation",
                "ColorScale",
            ],
            "the weight order drifted from the historical default draw order",
        );
    }
}

#[cfg(test)]
mod layer_id_bridge_tests {
    use rustdar_source::id::LAYER_ID_LEDGER;

    use super::OverlayKind;

    /// Test group 1 (spelling pin): every variant's `LayerId` is its own
    /// `Debug` spelling, byte for byte — **THE zero-config-migration proof**.
    /// Configs key on the Debug spellings today (the E0a corpus pins them);
    /// M8b re-keys on `id().as_str()`; this equality is why nothing in any
    /// user's file moves. A red here means a `known` const drifted from its
    /// variant — fix the const, never the enum.
    #[test]
    fn every_kinds_id_is_its_own_debug_spelling() {
        assert_eq!(OverlayKind::all().len(), 12);
        for &kind in OverlayKind::all() {
            assert_eq!(
                kind.id().as_str(),
                format!("{kind:?}"),
                "the known const for {kind:?} does not spell the Debug name — \
                 M8b's re-key would orphan this layer's saved state",
            );
        }
    }

    /// Test group 2 (round-trip): the bridge inverts — `from_id(id())`
    /// answers the variant it came from, for all twelve.
    #[test]
    fn the_bridge_round_trips_every_variant() {
        for &kind in OverlayKind::all() {
            assert_eq!(
                OverlayKind::from_id(&kind.id()),
                Some(kind),
                "from_id(id()) failed to invert for {kind:?}",
            );
        }
    }

    /// Test group 3's bridge half: every variant's id appears in the
    /// append-only ledger — the enum cannot register a spelling the ledger
    /// does not carry.
    #[test]
    fn every_variants_id_sits_in_the_ledger() {
        for &kind in OverlayKind::all() {
            assert!(
                LAYER_ID_LEDGER.contains(&kind.id().as_str()),
                "{kind:?}'s id is missing from LAYER_ID_LEDGER — ledger rows \
                 are append-only and this one was never appended",
            );
        }
    }
}
