//! The source-handler contract: the trait every layer implements, and the
//! vocabulary its methods speak.

use std::any::Any;
use std::fmt::Debug;
use std::future::Future;
use std::marker::PhantomData;
use std::pin::Pin;
use std::sync::Arc;

use rustdar_units::UserPreferences;

use crate::controls::{ControlEffect, ControlItem, ControlUpdate};
use crate::draw::{DrawPointContext, HoverContext, MapPoint, PointPainter};
use crate::feature::{OverlayFeature, OverlayLabel};
use crate::fetch_policy::{
    Assembled, DataCompleteness, FetchError, FetchFailure, FetchRetry, FetchRound, RoundShape,
    Whole,
};
use crate::id::LayerId;
use crate::job::{DescribedJob, JobCodec};
use crate::product::{FieldId, ProductSpec};
use crate::time::{FrameListing, FrameStamp, TimeAxis};

/// Not `rustdar_radar::LegendScale`: duplicated here to avoid the dependency.
pub struct OverlayLegend {
    /// Colour stops, **sorted ascending by value**.
    pub thresholds: Vec<(f32, [u8; 3])>,
    pub is_gradient: bool,
    pub min_value: f32,
    pub max_value: f32,
    pub unit_label: &'static str,
}

/// Fetch-cache-generation lifecycle shared by every overlay type. `S` decides
/// which data-installing method this state has: `set_data` for a [`Whole`]
/// round, `set_data_with_coverage` and only that for an [`Assembled`] one.
pub struct OverlayState<T, S: RoundShape> {
    pub data: T,
    /// Stamped on a **good answer only**; see [`crate::fetch_policy`].
    pub fetch_time: Option<web_time::Instant>,
    pub fetching: bool,
    pub data_generation: u64,
    pub retry: FetchRetry,
    /// `fn() -> S` so the marker cannot make this state less `Send`/`Sync`.
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
    /// Bumps `data_generation`, ends the fetch, clears the retry ladder, and
    /// declares the answer **whole**.
    pub fn set_data(&mut self, data: T) {
        self.install(data, DataCompleteness::default());
    }
}

impl<T> OverlayState<T, Assembled> {
    /// The **only** way data reaches the map of a layer whose round can deliver
    /// less than it was asked for. The clock still stamps and the ladder still
    /// resets: a half-delivered round is a good answer missing pieces.
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
    /// assert!(!state.retry.is_unhealthy());
    /// assert!(state.retry.is_incomplete());
    /// ```
    pub fn set_data_with_coverage(&mut self, data: T, coverage: DataCompleteness) {
        self.install(data, coverage);
    }

    /// Coverage on its own, for the assembled layer that **stamps its own map**
    /// rather than replacing it (SPC outlooks: one product per payload).
    pub fn record_coverage(&mut self, coverage: DataCompleteness) {
        self.retry.record_coverage(coverage);
    }
}

impl<T, S: RoundShape> OverlayState<T, S> {
    fn install(&mut self, data: T, coverage: DataCompleteness) {
        self.data = data;
        self.fetch_time = Some(web_time::Instant::now());
        self.data_generation = self.data_generation.wrapping_add(1);
        self.fetching = false;
        self.retry.record_success();
        self.retry.record_coverage(coverage);
    }

    /// This layer's own round, out of the payload the host handed back.
    /// `R::Shape` unifies with this state's `S`, so a [`Whole`] handler cannot
    /// take delivery of a round that declared itself [`Assembled`].
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
    pub fn downcast_round<R>(&self, payload: FetchPayload) -> Option<R>
    where
        R: FetchRound<Shape = S>,
    {
        payload.downcast::<R>().ok().map(|round| *round)
    }

    /// End a fetch that did not produce data, filing it against the ladder.
    pub fn record_failure(&mut self, error: &FetchError) {
        self.fetching = false;
        self.retry.record_failure(error);
        // An `Absent` answer is an answer: it stamps the clock, so the layer
        if error.failure == FetchFailure::Absent {
            self.fetch_time = Some(web_time::Instant::now());
        }
    }

    /// Whether switching this layer on should re-ask the origin: nothing drawn,
    /// **or** what is drawn is stale, **or** it is missing pieces.
    pub fn enable_should_refetch(&self, has_data: bool) -> bool {
        !self.fetching && (!has_data || self.retry.is_unhealthy() || self.retry.is_incomplete())
    }
}

/// How a layer gets onto the screen; dispatched on by the draw loop.
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

/// Which of a pane's two content surfaces a layer draws onto: ground is drawn
/// **at** a latitude and longitude and mirrors onto a 3D pane's floor; glass is
/// chrome against the pane's own **edges**.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Surface {
    Ground,
    Glass,
}

pub trait OverlayItem: Send + Sync + Debug {
    /// Which layer this item came from — its [`SourceHandler::id`].
    fn layer_id(&self) -> LayerId;

    fn popup_content(&self, prefs: &UserPreferences) -> PopupContent;

    /// Logical identity (same alert ID), not pointer equality:
    /// `retain_selections()` uses it to carry selections across a refetch.
    fn matches(&self, other: &dyn OverlayItem) -> bool;

    fn as_any(&self) -> &dyn Any;
}

/// Lets the UI crate hit-test without knowing overlay-specific types: a **view**
/// of the handler's geometry, not a copy. Labels are in `map_labels`.
pub struct ClickableItem<'a> {
    pub features: &'a [OverlayFeature],
    pub item: Arc<dyn OverlayItem>,
}

/// Something a handler hands the UI, with a number that changes when it does,
/// so a caller asking once per frame can decide **not to look**. Equal
/// signature ⇒ equal `items`.
pub struct Signed<T> {
    pub signature: u64,
    pub items: T,
}

/// The `null` a [`PaneRef`] with nothing saved points at. A `static` rather
/// than a fresh value per call, so [`PaneRef::bare`] borrows instead of
/// allocating.
static NULL_CONFIG: serde_json::Value = serde_json::Value::Null;

/// **One pane, as a handler sees it** — the pane-shaped half of every
/// [`SourceHandler`] method that can differ between two panes showing the same
/// layer.
///
/// A handler holds no per-pane fields: the pane owns them, hands them over as
/// `state` for the duration of one call, and takes them back. `config` is the
/// same facts as JSON, for a handler that has not been given a state type
/// (and for the load that builds one).
pub struct PaneRef<'a> {
    /// Which pane. Still meaningful to a handler that keeps no state — the
    /// fetch bookkeeping is keyed by it.
    pub pane_idx: usize,
    /// This layer's saved configuration in this pane; [`serde_json::Value::Null`]
    /// when the pane has nothing saved for it.
    pub config: &'a serde_json::Value,
    /// This layer's live per-pane state, when the handler defined
    /// [`SourceHandler::create_pane_state`]. Downcast with [`Self::state_as`].
    pub state: Option<&'a dyn Any>,
    /// The **sibling** slots' configs — the same pane's other layers, so a
    /// layer can read a fact another one owns (the site picker reads the radar
    /// slot's `"site"`). Look one up with [`Self::sibling`].
    ///
    /// The id is **borrowed**, not owned: an id read from a config file is a
    /// `Cow::Owned`, and this table is rebuilt once per pane per frame, so
    /// owning it would allocate a string per layer per pane per frame for a
    /// list almost every handler ignores.
    pub slots: &'a [(&'a LayerId, &'a serde_json::Value)],
    /// **Radar-transitional.** The site this pane is currently loading, which
    /// today lives on `Gui` rather than in any slot. WO-E8 dissolves the field
    /// it is read from; this member goes with it, and nothing new may read it.
    pub loading_site: Option<&'a str>,
    /// **The same layer's state in the OTHER panes**, in pane order, for the
    /// panes that have one.
    ///
    /// A handler answers about one pane through [`Self::state`]. It reads this
    /// only where the question is about the **layer** rather than about a
    /// pane — a fetch round every pane's selection contributes to, a cache
    /// every pane draws from — and then the answer is the **union**:
    /// [`Self::all_as`] walks this pane and its peers together, and what any
    /// pane still needs is kept. Dropping what one pane selects to satisfy
    /// another is the failure mode this exists to prevent, and the union is
    /// what every other eviction path in this workspace already does
    /// (`derive::retain_volumes` keeps what any live volume names;
    /// `VolumeStore::retain_set` drops an entry when the last pane lets go).
    pub peers: &'a [&'a dyn Any],
}

impl<'a> PaneRef<'a> {
    /// A pane with nothing saved, no state and no siblings — what a caller
    /// that is asking a handler a pane-independent question passes, and what
    /// a handler with no per-pane state is given.
    pub fn bare(pane_idx: usize) -> Self {
        Self {
            pane_idx,
            config: &NULL_CONFIG,
            state: None,
            slots: &[],
            loading_site: None,
            peers: &[],
        }
    }

    /// **No single pane — the layer as every pane holds it.**
    ///
    /// `state` is `None` and [`Self::peers`] carries every pane's, so
    /// [`Self::all_as`] is the union over the whole layer and there is no
    /// "this pane" for a handler to mistake for the answer. This is what the
    /// **arrival path** passes: a fetch result carries a layer id and no
    /// pane, and what a handler needs of it — is this product still asked
    /// for, which parameter must the cache not evict — is a question about
    /// every pane at once.
    pub fn across(peers: &'a [&'a dyn Any]) -> Self {
        Self {
            pane_idx: 0,
            config: &NULL_CONFIG,
            state: None,
            slots: &[],
            loading_site: None,
            peers,
        }
    }

    /// This pane's state as `T`, or `None` when the pane has no state for this
    /// layer (or it is not a `T`).
    pub fn state_as<T: 'static>(&self) -> Option<&'a T> {
        self.state?.downcast_ref::<T>()
    }

    /// **Every pane's state for this layer as `T`** — this one first, then the
    /// peers, skipping any that is absent or not a `T`.
    ///
    /// The union door. A handler folds over this when the question is about
    /// the layer rather than about one pane; folding over [`Self::state_as`]
    /// alone would answer for one pane and act for all of them.
    pub fn all_as<T: 'static>(&self) -> impl Iterator<Item = &'a T> + '_ {
        self.state_as::<T>()
            .into_iter()
            .chain(self.peers.iter().filter_map(|p| p.downcast_ref::<T>()))
    }

    /// Another layer's saved config **in this same pane**.
    pub fn sibling(&self, id: &LayerId) -> Option<&'a serde_json::Value> {
        self.slots
            .iter()
            .find(|(slot_id, _)| *slot_id == id)
            .map(|(_, config)| *config)
    }
}

/// [`PaneRef`]'s mutable half — what [`SourceHandler::apply_control`] writes
/// through. A control edit lands in the pane's own state, so the same edit in
/// two panes produces two answers.
pub struct PaneMut<'a> {
    pub pane_idx: usize,
    pub state: Option<&'a mut dyn Any>,
    /// The same layer's state in the **other** panes, read-only — see
    /// [`PaneRef::peers`]. A control edit that changes what the layer as a
    /// whole is asking for (the outlook's day and product set) has to weigh
    /// its own pane's new selection against the ones it is not editing, or it
    /// takes the layer off a ledger another pane's selection is still on.
    pub peers: &'a [&'a dyn Any],
}

impl PaneMut<'_> {
    /// A pane with no state — what a caller asking a handler that keeps none
    /// passes.
    pub fn bare(pane_idx: usize) -> Self {
        Self {
            pane_idx,
            state: None,
            peers: &[],
        }
    }

    /// This pane's state as `T`, mutably.
    pub fn state_as<T: 'static>(&mut self) -> Option<&mut T> {
        self.state.as_deref_mut()?.downcast_mut::<T>()
    }

    /// The **read** view of the same pane, for the handler methods a control
    /// edit has to consult mid-edit (`is_enabled`, `has_data`). It carries the
    /// state, which is the half those answers are computed from; it carries no
    /// config or siblings, because a mutable pane is being written through,
    /// not loaded from.
    pub fn as_ref(&self) -> PaneRef<'_> {
        PaneRef {
            pane_idx: self.pane_idx,
            config: &NULL_CONFIG,
            state: self.state.as_deref(),
            slots: &[],
            loading_site: None,
            peers: self.peers,
        }
    }
}

/// **The whole per-pane state of a layer whose only per-pane fact is whether
/// this pane draws it** — seven of the twelve handlers, and the exact shape the
/// `"enabled"` member of a slot config has always had, so a converted handler's
/// saved bytes are the bytes it already wrote.
pub struct PaneToggle {
    pub enabled: bool,
}

impl PaneToggle {
    /// A pane that has saved nothing: the layer's own default, **not** whatever
    /// some other pane last left in the handler.
    pub fn create(default_on: bool) -> Option<FetchPayload> {
        Some(Box::new(PaneToggle {
            enabled: default_on,
        }))
    }

    /// A pane's saved config, decoded. An absent or non-boolean member falls
    /// back to the layer's default — the reading that used to come out as
    /// "leave the handler as it is", which was one pane inheriting another's.
    pub fn restore(value: &serde_json::Value, default_on: bool) -> Option<FetchPayload> {
        Some(Box::new(PaneToggle {
            enabled: value
                .get("enabled")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(default_on),
        }))
    }

    /// Back to JSON — byte-identical to the `serialize_state` these handlers
    /// have always written.
    pub fn save(state: &dyn Any) -> serde_json::Value {
        match state.downcast_ref::<PaneToggle>() {
            Some(toggle) => serde_json::json!({ "enabled": toggle.enabled }),
            None => serde_json::Value::Null,
        }
    }

    /// This pane's answer, or `fallback` for a caller that supplied no pane —
    /// which during WO-M10b is still the registry's own copy, kept by the swap.
    pub fn is_on(pane: &PaneRef<'_>, fallback: bool) -> bool {
        pane.state_as::<PaneToggle>()
            .map_or(fallback, |toggle| toggle.enabled)
    }

    /// Write this pane's answer. **`false` means the caller supplied no pane**
    /// and must fall back to the handler's own field, so a missed pane is a
    /// visible branch rather than a silently dropped edit.
    #[must_use]
    pub fn set(pane: &mut PaneMut<'_>, on: bool) -> bool {
        match pane.state_as::<PaneToggle>() {
            Some(toggle) => {
                toggle.enabled = on;
                true
            }
            None => false,
        }
    }
}

/// Adding a layer means: implement this, give it a [`known`](crate::id::known)
/// const, append that spelling to
/// [`LAYER_ID_LEDGER`](crate::id::LAYER_ID_LEDGER), and register it in the
/// `sources()` of the crate that owns it.
pub trait SourceHandler: Send {
    /// This layer's open-string identity — one of the
    /// [`known`](crate::id::known) consts, spelled as a **literal** in each
    /// impl. Every per-layer map is keyed by it, so it is bytes in the config.
    fn id(&self) -> LayerId;

    fn surface(&self) -> Surface;

    /// Position in the default draw order, **bottom to top** — not the
    /// registration order. Spaced by 10 so a layer can sit between two.
    fn draw_order_weight(&self) -> u32;

    fn display_name(&self) -> &str;

    fn render_mode(&self) -> RenderMode;

    fn default_enabled(&self) -> bool {
        false
    }

    /// Bumped on every data replacement; drives texture cache invalidation.
    fn data_generation(&self) -> u64;

    /// A cheap token for **what this handler would draw in this pane**: a
    /// refetch returning the same content should keep it stable. Called every
    /// frame, so it must not clone.
    ///
    /// Pane-aware because it is a **cache token**: two panes showing the same
    /// layer with different filters draw different pictures, and one token for
    /// both is one texture for both.
    fn content_signature(&self, pane: &PaneRef<'_>) -> u64 {
        let _ = pane;
        self.data_generation()
    }

    /// Whether this handler's **cached raster** would come out differently in
    /// the other theme. A caching property, not a rendering one: a
    /// [`RenderMode::PerFramePoint`] layer holds no cached raster, so it answers
    /// `false` however theme-dependent it looks.
    fn theme_sensitive(&self) -> bool {
        false
    }

    /// Whether this pane has something to draw. Pane-aware: what "data" means
    /// is a function of the pane's own selection — the model layer's resident
    /// grid is *this pane's* parameter's grid.
    fn has_data(&self, pane: &PaneRef<'_>) -> bool;

    fn is_fetching(&self) -> bool;

    /// Takes the pane read-only: a layer whose round is one task per selected
    /// product sizes the round from the pane's selection.
    fn set_fetching(&mut self, fetching: bool, pane: &PaneRef<'_>);

    fn fetch_time(&self) -> Option<web_time::Instant>;

    fn auto_poll_interval(&self) -> Option<u64> {
        None
    }

    /// This layer's retry ledger; `None` for a handler that never fetches.
    fn retry(&self) -> Option<&FetchRetry> {
        None
    }

    fn retry_mut(&mut self) -> Option<&mut FetchRetry> {
        None
    }

    /// Age this layer's **poll clock**, as though `by` had passed since its
    /// last round — the [`fetch_time`](Self::fetch_time) twin of
    /// [`FetchRetry::rewind`], and there for the same reason: a test that
    /// needs a timer part-way through its interval cannot fabricate a
    /// [`web_time::Instant`] in the past through any other door.
    ///
    /// Default: nothing. A layer whose clock is stamped only on delivery is
    /// exercised by delivering to it, which needs no seam.
    #[doc(hidden)]
    fn rewind_fetch_time(&mut self, by: std::time::Duration) {
        let _ = by;
    }

    /// How long until an **automatic** fetch may start; `None` for a layer that
    /// does not auto-poll, or one already in flight. Two terms, whichever is
    /// later: the poll clock `fetch_time + interval`, and the backoff from
    /// [`crate::fetch_policy`].
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

    fn item_count(&self, pane: &PaneRef<'_>) -> usize {
        let _ = pane;
        0
    }

    /// **Is this layer on, in this pane?** Computed from the pane's own state,
    /// never mirrored into a bool beside it: for a layer whose "enabled" is
    /// really a set (the alert categories, the outlook's products) the set is
    /// the fact and this is how it is read, so the two cannot disagree.
    fn is_enabled(&self, pane: &PaneRef<'_>) -> bool {
        let _ = pane;
        true
    }

    fn set_enabled(&mut self, _enabled: bool, _pane: &mut PaneMut<'_>) {}

    /// One line of live status for the layer stack's row — `"3 shown · W/Wa"`.
    /// Called every frame; a disabled handler returns `None`.
    fn status_line(&self, pane: &PaneRef<'_>) -> Option<String> {
        let _ = pane;
        None
    }

    fn create_fetch_tasks(&self, ctx: &FetchConfig, pane: &PaneRef<'_>) -> Vec<FetchTask> {
        let _ = (ctx, pane);
        Vec::new()
    }

    /// Take delivery of one fetch round.
    ///
    /// `pane` is a [`PaneRef::across`]: an arrival carries a layer id and no
    /// pane, so `state` is `None` and every pane's is in
    /// [`peers`](PaneRef::peers). A handler whose bookkeeping depends on what
    /// is being asked for — the outlook's day-and-product scope, the model
    /// cache's un-evictable parameter — reads it through
    /// [`PaneRef::all_as`] and takes the **union**: what any pane still wants
    /// survives.
    fn apply_fetch_result(&mut self, result: FetchPayload, pane: &PaneRef<'_>);

    /// This handler's raster as a described job, or `None` when there is nothing
    /// to render. `has_data()` must answer `false` exactly when this answers
    /// `None`, or the settle machinery asks for a render nothing can satisfy.
    fn prepare_job(&self, ctx: &RasterizeContext, pane: &PaneRef<'_>) -> Option<DescribedJob> {
        let _ = (ctx, pane);
        None
    }

    /// The codec row that encodes, decodes and runs this handler's described job.
    /// Every texture handler except `Radar` answers exactly one row.
    fn job_codec(&self) -> Option<&'static JobCodec> {
        None
    }

    /// The `Arc<dyn OverlayItem>`s a hit-map kind's clicks resolve to,
    /// **index-aligned with the rows `prepare_job` describes**: build both from
    /// one iteration of one list, or a hover names the wrong report.
    fn hit_items(&self) -> Option<Vec<Arc<dyn OverlayItem>>> {
        None
    }

    /// The features a click is tested against, borrowed from this handler.
    /// Called **only on a frame that has a click to resolve**.
    fn clickable_items<'a>(&'a self, pane: &PaneRef<'_>) -> Vec<ClickableItem<'a>> {
        let _ = pane;
        Vec::new()
    }

    /// The map labels this layer paints, in the pane's own draw pass. Split out
    /// of `clickable_items`: labels are wanted every frame, geometry only on a
    /// click. A borrow, so a per-pane per-frame call allocates nothing.
    fn map_labels(&self) -> &[OverlayLabel] {
        &[]
    }

    fn handle_popup_action(&mut self, _action: &PopupAction) -> bool {
        false
    }

    /// Drops selections whose `matches()` finds nothing in the refreshed data
    /// — or which this pane's own filters no longer draw.
    fn retain_selections(&self, selections: &mut Vec<Arc<dyn OverlayItem>>, pane: &PaneRef<'_>);

    fn per_frame_points(&self) -> &[MapPoint] {
        &[]
    }

    fn draw_point(&self, _id: u32, _painter: &mut dyn PointPainter, _ctx: &DrawPointContext) {}

    fn point_hit_radius(&self, _zoom: f32) -> f32 {
        0.0
    }

    fn hover_text(&self, _id: u32, _ctx: &HoverContext<'_>) -> Option<String> {
        None
    }

    fn hover_value_at(&self, _lat: f64, _lon: f64, _pane: &PaneRef<'_>) -> Option<String> {
        None
    }

    /// This layer's colour bar, [`Signed`] so a caller can keep what it baked:
    /// the bar is sampled once per pixel of its length, at frame rate.
    fn legend(&self, pane: &PaneRef<'_>) -> Option<Signed<OverlayLegend>> {
        let _ = pane;
        None
    }

    /// The renderable fields this layer offers, as data.
    ///
    /// A layer with one picture and nothing to choose between returns the empty
    /// default. A layer that publishes several quantities — radar's moments,
    /// the model's parameters — returns one [`ProductSpec`] each, and the UI
    /// builds its pickers, legends and catalogue tiles from those rows without
    /// naming a single one of them.
    fn products(&self) -> &'static [ProductSpec] {
        &[]
    }

    /// **Which of this layer's [`products`](Self::products) `pane` is showing
    /// right now**, or `None` from a layer that offers no choice.
    ///
    /// **Answered from this layer's OWN per-pane state** — the slot config it
    /// already reads everything else out of, or the control named by
    /// [`field_control_id`](Self::field_control_id). Never by reaching up into
    /// the pane's own selection field: a handler cannot see one, and a layer
    /// that could would be answering for its neighbours as well as itself.
    ///
    /// The caller is a pane deciding *which layer to ask for a picture* — the
    /// 3D walk asks it of every volume-capable slot in the stack, top down —
    /// so an answer that is stale by a frame selects the wrong field, not
    /// merely a late one. A layer answering from its slot config therefore
    /// depends on that config being kept current, which is what
    /// `publish_radar_selection` exists to do for the one layer whose
    /// selection the pane owns.
    ///
    /// Defaulted to `None`, which is also the honest answer from a layer with
    /// one picture and nothing to choose between.
    fn current_field(&self, pane: &PaneRef<'_>) -> Option<FieldId> {
        let _ = pane;
        None
    }

    /// **This layer's 3D half, if it has one.**
    ///
    /// `Some` from a layer that can build a
    /// [`VolumeGrid`](crate::volume::VolumeGrid); `None` — the default, and
    /// eleven of this build's twelve layers take it — from a flat one.
    ///
    /// A pane in Volume mode walks its stack for the first enabled slot that
    /// answers `Some` here and whose [`current_field`](Self::current_field)
    /// that layer can build, rather than naming a layer. That is the whole
    /// seam: adding a second 3D source is an implementation on its own
    /// handler and no arm anywhere above it.
    fn volume(&self) -> Option<&dyn crate::volume::VolumeCapable> {
        None
    }

    /// The id of the control that selects which of this layer's
    /// [`products`](Self::products) a pane is showing, if the choice is made
    /// through a control at all.
    ///
    /// **This is the layer stating its own field-selection route**, so a
    /// catalogue tile can be applied without anything above knowing which
    /// layer it belongs to. `Some(id)` means "send a
    /// [`ControlUpdate`] with this id and the field's own id as the value, and
    /// my `apply_control` will do the rest" — the model's parameter dropdown.
    /// `None` means the selection is not a control of mine: the pane owns it,
    /// as it owns radar's product and elevation.
    ///
    /// Defaulted to `None` because most layers have no fields at all, and a
    /// layer with one picture has nothing to select between.
    fn field_control_id(&self) -> Option<&'static str> {
        None
    }

    /// The fields this layer offers as catalogue tiles.
    ///
    /// Defaults to [`products`](Self::products): a layer that publishes fields
    /// normally wants all of them on offer. A layer whose catalogue presence is
    /// narrower than its field list overrides this.
    fn catalog(&self) -> &'static [ProductSpec] {
        self.products()
    }

    fn controls(&self, _pane: &PaneRef<'_>) -> Vec<ControlItem> {
        Vec::new()
    }

    /// The returned [`ControlEffect`] asks the caller for a side-effect.
    fn apply_control(&mut self, _update: &ControlUpdate, _pane: &mut PaneMut<'_>) -> ControlEffect {
        ControlEffect::None
    }

    /// e.g. selected product, loop state. `None` if there is no per-pane state.
    ///
    /// A handler that answers `Some` here has **moved** those fields out of
    /// itself: the pane owns them, and every method that reads them takes a
    /// [`PaneRef`] and downcasts [`PaneRef::state`].
    ///
    /// `enabled` is the pane's own slot flag — what the file said about this
    /// pane before any handler was asked. A fresh state starts from it, so a
    /// pane that has saved a flag but no config comes back the way it was
    /// left rather than at the layer's default.
    fn create_pane_state(&self, enabled: bool) -> Option<FetchPayload> {
        let _ = enabled;
        None
    }

    fn serialize_state(&self) -> serde_json::Value {
        serde_json::Value::Null
    }

    fn deserialize_state(&mut self, _value: serde_json::Value) {}

    fn serialize_pane_state(&self, _state: &dyn Any) -> serde_json::Value {
        serde_json::Value::Null
    }

    /// `enabled` is the pane's slot flag, the fallback for anything the saved
    /// config does not name — the config wins wherever it has an opinion,
    /// which is the precedence the swap has always produced.
    fn deserialize_pane_state(
        &self,
        _value: serde_json::Value,
        _enabled: bool,
    ) -> Option<FetchPayload> {
        None
    }

    // ── Time ──────────────────────────────────────────────────────────────
    //
    // How this layer relates to the clock. `time_axis` is the whole
    // declaration: every presentation rule is derived from the arm, written
    // on [`TimeAxis`] itself. The rest of this block is the frame supply a
    // [`TimeAxis::FrameSeries`] layer answers through; the other two arms
    // take the defaults and never see a frame.

    /// This layer's relationship to the clock — see [`TimeAxis`] for the
    /// rules each arm derives.
    fn time_axis(&self) -> TimeAxis {
        TimeAxis::Live
    }

    /// **What frames this layer could show over `range`, as it already knows
    /// it.** A synchronous query over handler-owned state — it **never**
    /// performs I/O; [`Self::create_frame_list_task`] is the fetch that fills
    /// that state in.
    ///
    /// Pane-scoped because frames are per-slot: two panes on two radar sites
    /// hold two frame sets, and a [`FrameStamp`] carries no site, so an
    /// unscoped answer would pool them into one bogus list.
    fn list_frames(
        &self,
        ctx: &FetchConfig,
        pane: &PaneRef<'_>,
        range: (chrono::NaiveDateTime, chrono::NaiveDateTime),
    ) -> FrameListing {
        let _ = (ctx, pane);
        FrameListing::empty(range)
    }

    /// **The async supply for [`Self::list_frames`]** — the listing fetch that
    /// teaches this handler what frames exist. Lands as
    /// [`SourceEvent::Frames`]; `None` from a layer with no listing to fetch.
    ///
    /// Build the task through [`FrameListingResult::task`]: the scope this
    /// listing will be filed under is **captured here, at dispatch**, and
    /// travels with the round trip rather than being read back off a pane
    /// that may have moved by the time it lands.
    fn create_frame_list_task(
        &self,
        ctx: &FetchConfig,
        pane: &PaneRef<'_>,
        range: (chrono::NaiveDateTime, chrono::NaiveDateTime),
    ) -> Option<FetchTask> {
        let _ = (ctx, pane, range);
        None
    }

    /// **The async supply for one frame's data.** Lands as
    /// [`SourceEvent::FrameReady`]; `None` when this handler cannot fetch that
    /// stamp (or already holds it).
    fn fetch_frame(
        &self,
        ctx: &FetchConfig,
        pane: &PaneRef<'_>,
        stamp: &FrameStamp,
    ) -> Option<FetchTask> {
        let _ = (ctx, pane, stamp);
        None
    }

    /// The stamps this handler is **holding data for** in this pane, ready to
    /// draw without a fetch. A subset of what [`Self::list_frames`] names.
    ///
    /// The frame cache is the **handler's own**: nothing above keeps a parallel
    /// map of frames, so nothing above can disagree with this answer.
    ///
    /// **Not yet true of the radar layer**, which is the only implementor of
    /// this surface today: the decoded volumes and the paired Level III
    /// objects its frames are made of are held ABOVE it, so it takes the
    /// default here and answers with none while the frames exist. It becomes
    /// true when those two caches move behind the handler — WO-M12d — and the
    /// claim above is a statement about the contract, not about the tree,
    /// until then.
    fn frames_resident(&self, pane: &PaneRef<'_>) -> Vec<FrameStamp> {
        let _ = pane;
        Vec::new()
    }

    /// Drop every resident frame **not** in `keep`. The one eviction door, so
    /// the budget that decides what to keep lives above and the storage stays
    /// below — with the same caveat [`Self::frames_resident`] carries: radar's
    /// storage is still above, so radar still evicts its own.
    fn retain_frames(&mut self, pane: &PaneRef<'_>, keep: &[FrameStamp]) {
        let _ = (pane, keep);
    }

    /// **Cache-key quantum for [`TimeAxis::EventLifetime`] as-of
    /// rasterization** — how coarsely the depicted instant is rounded before
    /// it enters a texture cache key, so a scrubbing pane re-uses rasters
    /// instead of minting one per clock tick.
    ///
    /// Consumed at WO-E7c, and **only under a scrubbed posture**: a live pane
    /// keys on nothing time-shaped at all, which is why a one-second quantum
    /// here does not re-raster a live layer every second.
    fn as_of_quantum(&self) -> std::time::Duration {
        std::time::Duration::from_secs(60)
    }

    /// **Take delivery of a frame listing this handler asked for**, with the
    /// `scope` its own [`Self::create_frame_list_task`] captured at dispatch.
    ///
    /// The scope is this handler's own type and nothing above reads it. It is
    /// how the listing is *filed*: `listing` names no site, so a handler two
    /// panes on two sites both ask of would otherwise pool two sites' stamps
    /// into one list.
    fn apply_frame_listing(
        &mut self,
        listing: FrameListing,
        scope: FetchPayload,
        pane: &PaneRef<'_>,
    ) {
        let _ = (listing, scope, pane);
    }

    /// Take delivery of one frame's data.
    fn apply_frame(&mut self, stamp: FrameStamp, data: FetchPayload, pane: &PaneRef<'_>) {
        let _ = (stamp, data, pane);
    }
}

pub struct FetchConfig {
    pub client: reqwest::Client,
    pub zone_cache_dir: Option<std::path::PathBuf>,
    /// Every origin a fetch may reach is declared here, not inline in URLs.
    pub sources: crate::origins::DataSources,
    /// `None` before the first frame is rendered. METAR must scope to this —
    /// the whole-country IEM form is 54 MB ungzipped.
    pub viewport: Option<rustdar_geo::GeoBounds>,
}

/// `Copy` so a rasterizer takes the whole thing rather than three loose scalars.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RasterizeContext {
    pub is_dark: bool,
    pub zoom: f64,
    /// Texels per logical point the texture was sized at. Rasterizer lengths are
    /// in **texels** chosen from the map zoom, but textures are sized in
    /// physical pixels, so a 2× display would halve an unscaled radius.
    pub device_scale: f32,
    /// The page's **wall clock** at dispatch, UTC. Captured at the dispatch so
    /// a worker matches the direct call.
    pub now: chrono::NaiveDateTime,
    /// The instant the picture **DEPICTS**, UTC. Equal to [`Self::now`] on a
    /// live pane; a scrubbed pane (WO-E7 and later) writes the scrub instant.
    ///
    /// A [`TimeAxis::EventLifetime`] layer filters on this and never on
    /// `now`; a [`TimeAxis::Live`] layer ignores it.
    pub as_of: chrono::NaiveDateTime,
}

// `Send` on native because `tokio::spawn` requires `Send + 'static`; not on
// web, where reqwest's futures are `!Send`. Two bounds, not one: the future,
// and the type-erased payload it sends back. **Do not relax the renderer's
// bounds to match** — its dispatch spawns real OS threads.

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

/// **What a frame-list task's future yields** — the two halves of
/// [`SourceEvent::Frames`], so the driver that spawns the task never has to
/// invent either one.
///
/// [`SourceHandler::create_frame_list_task`] returns an ordinary
/// [`FetchTask`], so build it through [`Self::task`] and read it back through
/// [`Self::event`]; a payload that did not come through `task` is a
/// programming error at the handler, and `event` says so by answering `None`
/// rather than fabricating an empty listing.
pub struct FrameListingResult {
    /// The generic half: what frames exist over the window that was asked
    /// about. Names no site.
    pub listing: FrameListing,
    /// The source's own half, captured at dispatch and handed straight back
    /// to [`SourceHandler::apply_frame_listing`].
    pub scope: FetchPayload,
}

impl FrameListingResult {
    /// Wrap a frame-list future as the [`FetchTask`] the contract carries.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn task(kind: LayerId, future: impl Future<Output = Self> + Send + 'static) -> FetchTask {
        FetchTask {
            kind,
            future: Box::pin(async move { Box::new(future.await) as FetchPayload }),
        }
    }

    /// Wrap a frame-list future as the [`FetchTask`] the contract carries.
    ///
    /// The web arm takes no `Send`: reqwest's futures are `!Send` there, and
    /// the cfg'd [`TaskFuture`] alias is what already carries that difference.
    #[cfg(target_arch = "wasm32")]
    pub fn task(kind: LayerId, future: impl Future<Output = Self> + 'static) -> FetchTask {
        FetchTask {
            kind,
            future: Box::pin(async move { Box::new(future.await) as FetchPayload }),
        }
    }

    /// The arrival `data` names, or `None` when it is not a frame-list
    /// payload at all.
    pub fn event(kind: LayerId, data: FetchPayload) -> Option<SourceEvent> {
        let result = data.downcast::<Self>().ok()?;
        Some(SourceEvent::Frames {
            id: kind,
            listing: result.listing,
            scope: result.scope,
        })
    }
}

/// One completed fetch round's payload, on its way back to the handler that
/// asked for it. The arrival names a **layer** and no pane: what a handler
/// needs of it is a question about every pane at once.
pub struct OverlayFetchResult {
    pub kind: LayerId,
    pub data: FetchPayload,
}

/// **Everything that arrives on a source's one return path.**
///
/// One channel, one drain, one `match` — so a new arrival shape is a compile
/// error at the drain rather than a second channel nobody remembers to poll.
///
/// Generic over nothing: the payloads are the cfg'd [`FetchPayload`] alias, so
/// the web arm (`Box<dyn Any>`, **not** `Send`) carries exactly what it can.
pub enum SourceEvent {
    /// Today's payload, unchanged — a whole fetch round for a layer.
    Data(OverlayFetchResult),
    /// What frames a layer can show, in answer to
    /// [`SourceHandler::create_frame_list_task`], with the **scope** that
    /// question was asked in.
    ///
    /// `listing` is the generic half every reader understands and it names no
    /// site, exactly as [`FrameStamp`] does not. `scope` is the source's own,
    /// opaque here and interpreted only by the handler that built the task:
    /// it is what makes a listing filable, because a [`FrameStamp`] alone is
    /// never enough to key a cache a second site also writes to.
    ///
    /// **Captured at dispatch**, not read back off the pane on arrival — a
    /// listing is an uncancellable round trip and the pane that asked can be
    /// rebuilt for another site while it is in the air.
    Frames {
        id: LayerId,
        listing: FrameListing,
        scope: FetchPayload,
    },
    /// One frame's data, in answer to [`SourceHandler::fetch_frame`].
    FrameReady {
        id: LayerId,
        stamp: FrameStamp,
        data: FetchPayload,
    },
}

pub struct PopupContent {
    pub title: String,
    pub accent_rgb: [u8; 3],
    pub width: f32,
    pub sections: Vec<PopupSection>,
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

pub struct PopupAction {
    pub label: String,
    pub target: Arc<dyn OverlayItem>,
    pub kind: PopupActionKind,
}

pub enum PopupActionKind {
    HideFromMap,
}
