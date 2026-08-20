//! The source-handler contract: the trait every layer implements, and the
//! vocabulary its methods speak.
//!
//! Moved here from `rustdar-overlays` at WO-M9 so that a source lives in its
//! own crate rather than inside the overlay crate: `rustdar-radar` implements
//! this trait for the radar layer, and `rustdar-overlays` for the other
//! eleven. Neither can see the other, and neither needs to.

use std::any::Any;
use std::fmt::Debug;
use std::future::Future;
use std::marker::PhantomData;
use std::pin::Pin;
use std::sync::Arc;

use rustdar_units::UserPreferences;

use crate::controls::{
    ControlEffect, ControlItem, ControlUpdate, PaneControlContext, PaneControlContextMut,
};
use crate::draw::{DrawPointContext, HoverContext, MapPoint, PointPainter};
use crate::feature::{OverlayFeature, OverlayLabel};
use crate::fetch_policy::{
    Assembled, DataCompleteness, FetchError, FetchFailure, FetchRetry, FetchRound, RoundShape,
    Whole,
};
use crate::id::LayerId;
use crate::job::{DescribedJob, JobCodec};

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
    /// use rustdar_source::fetch_policy::Assembled;
    /// use rustdar_source::handler::OverlayState;
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
    /// use rustdar_source::fetch_policy::{Assembled, DataCompleteness};
    /// use rustdar_source::handler::OverlayState;
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
    /// use rustdar_source::fetch_policy::{Assembled, FetchRound, Whole};
    /// use rustdar_source::handler::{FetchPayload, OverlayState};
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
    /// use rustdar_source::fetch_policy::{Assembled, FetchRound};
    /// use rustdar_source::handler::{FetchPayload, OverlayState};
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
    // `SourceHandler::auto_fetch_delay`.
}

/// How a layer gets onto the screen — declared by its handler
/// ([`SourceHandler::render_mode`]) and dispatched on by the draw loop, which
/// therefore never needs to know *which* layer it is holding.
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
/// Declared by the handler ([`SourceHandler::surface`]) rather than decided
/// layer-by-layer in the UI crate: a new layer does not compile until its
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
    /// Which layer this item came from — the same [`known`](crate::id::known) const its
    /// handler's [`SourceHandler::id`] answers. A selection list mixes
    /// items from every layer, so routing a popup action back to the
    /// handler that owns it (`OverlayRegistry::handle_popup_action`) and
    /// filtering a handler's own selections out of it both ask this.
    fn layer_id(&self) -> LayerId;

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
/// click pay for geometry — see [`SourceHandler::map_labels`].
pub struct ClickableItem<'a> {
    pub features: &'a [OverlayFeature],
    pub item: Arc<dyn OverlayItem>,
}

// ── Signed presentation ───────────────────────────────────────────────────

/// Something a handler hands the UI, with a number that changes when it does.
///
/// The signature exists so a caller can decide **not to look**. Presentation
/// hooks are asked once per pane per frame; their answers are built from state
/// that changes at fetch rate, which is four to six orders of magnitude slower.
/// A caller holding `signature` from last frame and seeing the same one this
/// frame knows the whole answer is unchanged without reading a byte of it —
/// which is what lets it keep a baked texture, a laid-out string list, or any
/// other derived thing instead of rebuilding it.
///
/// # The contract, stated as a trait property
///
/// **Every present and future presentation hook on [`SourceHandler`] returns
/// `Signed<…>`.** Not "returns `Signed` where it seemed worth it": a hook that
/// answers bare data is a hook whose callers have no way to skip it, and the
/// only remedy then is for each caller to hash the answer — which means
/// building the answer first, i.e. paying the cost the signature exists to
/// avoid. The rule is what makes the skip available by default rather than by
/// negotiation.
///
/// # What a signature must satisfy
///
/// Equal signature ⇒ equal `items`. The converse is not required: a handler
/// may move its signature without changing the answer, and the caller merely
/// rebuilds something identical. Handlers spell it as whatever already tracks
/// their content — a fetch generation, a selection discriminant — mixed
/// together; there is deliberately no derived-hash helper here, because the
/// number that is *cheap* to compute is the handler's own state counter and
/// hashing the payload would be the cost being avoided.
pub struct Signed<T> {
    /// Changes whenever `items` would.
    pub signature: u64,
    pub items: T,
}

// ── Source handler trait ──────────────────────────────────────────────────

/// Adding a layer means: implement this, give it a [`known`](crate::id::known)
/// const, append that spelling to
/// [`LAYER_ID_LEDGER`](crate::id::LAYER_ID_LEDGER), and register it in the
/// `sources()` function of the crate that owns it —
/// `rustdar_overlays::sources` for a layer built out of overlay data,
/// `rustdar_radar::sources` for one built out of radar data.
///
/// **Every registration list the app composes is named there and nowhere
/// else** (WO-M9): `rustdar_egui::sources::all` is the one composition, and it
/// is the whole of what a new layer has to be added to. The claim this
/// sentence replaced — that nothing in `rustdar-egui` or the `rustdar` crate
/// changes — was never true: the crate that composes the registry is the crate
/// a new layer is registered in (audit Finding 10).
pub trait SourceHandler: Send {
    // ── Identity & metadata ───────────────────────────────────────────

    /// This layer's open-string identity — one of the
    /// [`known`](crate::id::known) consts,
    /// spelled as a **literal** in each impl. It is what every per-layer
    /// map is keyed by (draw order, enabled flags, pane configs, saved
    /// handler state) and therefore the bytes those maps put in the user's
    /// config file, so it is derived from nothing: the registry's identity
    /// tests pin uniqueness and ledger membership across all twelve, and
    /// the state-key tripwire holds the live set to the literal twelve
    /// names saved configs have always filed handler state under.
    fn id(&self) -> LayerId;

    /// Which pane surface this layer draws onto. See [`Surface`].
    fn surface(&self) -> Surface;

    /// This layer's position in the default draw order, **bottom to top** —
    /// a lower weight draws first and is occluded by higher ones.
    ///
    /// The weights are the ONE spelling of the default draw order, and it is
    /// not the registration order: SpcOutlook draws BELOW Radar here, while
    /// the composition chains radar on last. The literal-list pin in
    /// `rustdar_egui::sources::registry_identity_tests` is what holds the
    /// weights to the order users have always seen. Spaced by 10 so a future
    /// layer can sit between two without renumbering.
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
    /// [`data_generation`]: SourceHandler::data_generation
    fn content_signature(&self) -> u64 {
        self.data_generation()
    }

    /// Whether this handler's **cached raster** would come out differently in
    /// the other theme — the one declaration the cache token's theme term is
    /// read from.
    ///
    /// `true` costs a re-rasterize on every theme flip, and buys correctness:
    /// a handler that bakes `RasterizeContext::is_dark` into its pixels and
    /// answers `false` keeps compositing the old theme's colours until its
    /// content happens to change. `false` costs nothing and is right for every
    /// handler whose pixels do not depend on the theme.
    ///
    /// # This is a caching property, not a rendering one
    ///
    /// The two sets are not the same, and reading this method as "does this
    /// layer look different in dark mode" gets it wrong in one specific place.
    /// A [`RenderMode::PerFramePoint`] layer — METAR is the one — reads the
    /// theme *inside the frame it draws*
    /// (`render::station_model::draw_station_model` picks its text colour off
    /// `DrawPointContext::is_dark`) and holds no cached raster and no cache
    /// token at all. There is nothing for a flip to invalidate: the next frame
    /// already draws in the new theme. So METAR answers `false` — not because
    /// its appearance is theme-independent (it plainly is not), but because
    /// its *cache* is, there being none. Declaring it `true` would be inert,
    /// and this note is here so the `false` is not later read as an oversight
    /// against `station_model.rs`.
    ///
    /// The overrides that answer `true` are therefore exactly the
    /// [`RenderMode::Texture`] handlers whose rasterizer input carries
    /// `is_dark`.
    fn theme_sensitive(&self) -> bool {
        false
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
    /// [`auto_poll_interval`]: SourceHandler::auto_poll_interval
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
    /// struct (`rustdar_overlays::render::rasterize::AlertsInput` and its siblings) —
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
    /// [`job_codec`]: SourceHandler::job_codec
    fn prepare_job(&self, ctx: &RasterizeContext) -> Option<DescribedJob> {
        let _ = ctx;
        None
    }

    /// The codec row that encodes, decodes and runs this handler's described
    /// job — one of `rustdar_overlays::render::jobs::JOB_CODECS` — or `None` for a
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
    /// [`prepare_job`]: SourceHandler::prepare_job
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
    /// (`rustdar_overlays::render::rasterize::HitMap::from_cells`,
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
    /// [`prepare_job`]: SourceHandler::prepare_job
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
    /// [`clickable_items`]: SourceHandler::clickable_items
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

    /// This layer's colour bar, [`Signed`] so a caller can keep what it baked
    /// from the last one.
    ///
    /// The signature is the whole point of the wrapper here: the caller draws
    /// the bar by sampling the ramp once per pixel of its length, which is
    /// ~1000 interpolations down a threshold list per bar per frame for an
    /// answer that changes at fetch rate. See [`Signed`] for the contract every
    /// presentation hook keeps.
    fn legend(&self) -> Option<Signed<OverlayLegend>> {
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
    pub sources: crate::origins::DataSources,
    /// `None` before the first frame is rendered. METAR must scope to this —
    /// the whole-country IEM form is 54 MB ungzipped; see
    /// `rustdar_overlays::metar::networks` for the no-viewport fallback.
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
    /// `rustdar_overlays::render::rasterize` is a length in **texels**, chosen from the
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
    /// `rustdar_overlays::render::rasterize::GlmStrikesInput::now`
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
