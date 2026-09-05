//! The source-handler contract: the trait every layer implements, and the
//! vocabulary its methods speak.

use std::any::Any;
use std::fmt::Debug;
use std::future::Future;
use std::marker::PhantomData;
use std::pin::Pin;
use std::sync::Arc;

use squallar_units::UserPreferences;

use crate::controls::{ControlEffect, ControlItem, ControlUpdate};
use crate::draw::{DrawPointContext, HoverContext, MapPoint, PointPainter};
use crate::feature::{OverlayFeature, OverlayLabel};
use crate::fetch_policy::{
    Assembled, DataCompleteness, FetchError, FetchFailure, FetchRetry, FetchRound, RoundShape,
    Whole,
};
use crate::footprint::ItemFootprint;
use crate::id::LayerId;
use crate::job::{DescribedJob, JobCodec};
use crate::product::{FieldId, ProductSpec};
use crate::time::{FrameListing, FrameSource, FrameStamp, Residency, TimeAxis};

/// Not `squallar_radar::LegendScale`: duplicated here to avoid the dependency.
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
    /// **What [`Self::data`] owns on the heap**, priced where it was
    /// installed and read back as a load — see [`crate::footprint`]. Private,
    /// and moved only through the doors that can change it, so this figure
    /// and the level it feeds cannot disagree.
    data_bytes: u64,
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
            data_bytes: 0,
            shape: PhantomData,
        }
    }
}

/// **The level follows a state's whole life, not only its installs.**
///
/// A handler that goes away takes its data with it, and a level that counted
/// only installs would climb across a process and read as resident memory
/// nobody is holding. `data` is never moved out of an `OverlayState` anywhere
/// in this workspace, so no caller loses a partial move to this impl.
impl<T, S: RoundShape> Drop for OverlayState<T, S> {
    fn drop(&mut self) {
        crate::footprint::move_installed(self.data_bytes, 0);
    }
}

impl<T: Default, S: RoundShape> OverlayState<T, S> {
    pub fn new() -> Self {
        Self::default()
    }
}

impl<T: ItemFootprint> OverlayState<T, Whole> {
    /// Bumps `data_generation`, ends the fetch, clears the retry ladder, and
    /// declares the answer **whole**.
    pub fn set_data(&mut self, data: T) {
        self.install(data, DataCompleteness::default());
    }
}

impl<T: ItemFootprint> OverlayState<T, Assembled> {
    /// The **only** way data reaches the map of a layer whose round can deliver
    /// less than it was asked for. The clock still stamps and the ladder still
    /// resets: a half-delivered round is a good answer missing pieces.
    ///
    /// ```compile_fail
    /// use squallar_source::fetch_policy::Assembled;
    /// use squallar_source::handler::OverlayState;
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
    /// use squallar_source::fetch_policy::{Assembled, DataCompleteness};
    /// use squallar_source::handler::OverlayState;
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

impl<T: ItemFootprint, S: RoundShape> OverlayState<T, S> {
    /// **The one write to [`Self::data`]**, and therefore the one place the
    /// heap level it feeds moves.
    ///
    /// The pricing walk is O(items) and it runs here rather than on any frame
    /// path: an install already builds the list it is handed, and the census
    /// read is a load of what this stores.
    fn install(&mut self, data: T, coverage: DataCompleteness) {
        let bytes = data.owned_bytes();
        crate::footprint::move_installed(self.data_bytes, bytes);
        self.data_bytes = bytes;
        self.data = data;
        self.fetch_time = Some(web_time::Instant::now());
        self.data_generation = self.data_generation.wrapping_add(1);
        self.fetching = false;
        self.retry.record_success();
        self.retry.record_coverage(coverage);
    }

    /// **Re-price [`Self::data`] after a write that did not go through
    /// [`Self::install`].**
    ///
    /// [`Self::data`] is public, and two layers use that: the SPC outlook and
    /// fire-weather handlers **stamp their own map** one product per payload
    /// rather than replacing it wholesale (see [`Self::record_coverage`]), so
    /// their bytes move without an install. A direct write that does not call
    /// this leaves the level reading the previous generation's figure, which
    /// is a wrong census line rather than a wrong picture.
    pub fn reprice(&mut self) {
        let bytes = self.data.owned_bytes();
        crate::footprint::move_installed(self.data_bytes, bytes);
        self.data_bytes = bytes;
    }

    /// This layer's own round, out of the payload the host handed back.
    /// `R::Shape` unifies with this state's `S`, so a [`Whole`] handler cannot
    /// take delivery of a round that declared itself [`Assembled`].
    ///
    /// ```compile_fail
    /// use squallar_source::fetch_policy::{Assembled, FetchRound, Whole};
    /// use squallar_source::handler::{FetchPayload, OverlayState};
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
    /// use squallar_source::fetch_policy::{Assembled, FetchRound};
    /// use squallar_source::handler::{FetchPayload, OverlayState};
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

    /// **What [`Self::data`] owns on the heap**, as last priced. A load, not
    /// a walk.
    pub fn data_bytes(&self) -> u64 {
        self.data_bytes
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
    /// **Both**: a rasterized picture for the geometry, plus a per-frame point
    /// pass for the text.
    ///
    /// Not a convenience — it is the shape of any layer that plots a symbol
    /// AND labels it. The geometry belongs in a picture: a station model is
    /// tens of stroked shapes per item, and stroked paths are the
    /// tessellator's expensive case, so at several hundred items it is the
    /// frame's whole geometry budget. The text cannot follow it, because the
    /// pictures are rasterized by `tiny_skia`, which has no fonts, and nothing
    /// in the worker can lay out a galley.
    ///
    /// So the text stays on the frame thread and goes through the same
    /// `walkers::GalleyCache` the map's place labels use — one text path for
    /// everything the map labels, rather than a second one in the worker that
    /// would have to match it.
    ///
    /// A layer in this mode must NOT draw its geometry twice: the point pass
    /// asks the registry whether the layer has a picture and suppresses the
    /// shapes when it does. See `EguiPointPainter::text_only`.
    TextureAndPoint,
    /// Per-frame by the handler itself (UserLocation).
    PerFrameDirect,
    /// Streaming tiles owned by the map widget (BaseMap, CityLabels).
    Tile,
}

impl RenderMode {
    /// Whether this layer is backed by a rasterized picture.
    ///
    /// The question almost every texture invariant actually wants — asked
    /// instead of `== Texture` so a mode that gained a picture is covered by
    /// them rather than silently skipped.
    pub fn has_texture(self) -> bool {
        matches!(self, Self::Texture | Self::TextureAndPoint)
    }

    /// Whether the frame's point walk visits this layer.
    pub fn draws_points(self) -> bool {
        matches!(self, Self::PerFramePoint | Self::TextureAndPoint)
    }
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

/// Where a layer that is not presented as a peer layer keeps its one
/// user-facing switch — see [`SourceHandler::surfaced_through`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SurfacedControl {
    /// The layer whose inspector hosts the toggle. The host's absence is the
    /// declared edge: with the host removed from a pane's stack, the toggle
    /// is unreachable there, and the surfaced layer simply follows its own
    /// persisted per-pane state.
    pub host: LayerId,
    /// The toggle's label in the host's inspector.
    pub label: &'static str,
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

    /// **Surfaced through another layer's controls instead of a stack row and
    /// catalog tile of its own.**
    ///
    /// `None` — the answer for almost every layer — means the layer is a
    /// Layers-panel citizen: a row in the stack, a tile in the catalog, its
    /// own inspector body. `Some` means the ONE user-facing switch is a
    /// toggle rendered inside the `host` layer's inspector under `label`;
    /// the stack and the catalog then present no row and no tile for it.
    ///
    /// Presentation only: a surfaced layer still registers, still occupies
    /// its draw-order weight, still draws its own pixels when its per-pane
    /// enabled state says so, and its saved state keeps its own slot — a
    /// config that enabled it through the old row reopens 1:1 with no
    /// migration. The toggle is the SAME per-pane enabled state relocated,
    /// not a second source of truth.
    fn surfaced_through(&self) -> Option<SurfacedControl> {
        None
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
    fn hit_items(&self) -> Option<crate::hit::HitItems> {
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
    // on [`TimeAxis`] itself. What is left here is what every arm answers —
    // the axis, the loop floor and the as-of quantum. The frame *supply* is
    // not: it moved to `FrameSource`, reached through `frames`/`frames_mut`,
    // where its nine methods have no default bodies and a `FrameSeries` layer
    // must therefore write every one of them.

    /// This layer's relationship to the clock — see [`TimeAxis`] for the
    /// rules each arm derives.
    ///
    /// **Required, and it used to default to [`TimeAxis::Live`].** That default
    /// was not a neutral one: `Live` is the arm that says "I have no history",
    /// and it is the arm under which `FetchConfig::as_of` is left at the wall
    /// clock by contract. So a source that simply never mentioned the clock was
    /// silently declared historyless, and every mechanism that depends on the
    /// declaration — the depicted instant reaching the fetch at all, the
    /// residency law, the loop — skipped it without anything saying so.
    ///
    /// That is not hypothetical. The SPC mesoscale-discussion layer inherited
    /// this default while every one of its items carried a `VALID` window, so a
    /// pane scrubbed to a storm ten years gone drew *today's* discussions over
    /// it; the archive branch written to fix that was unreachable, because the
    /// caller had already decided this layer did not want the instant. Removing
    /// the body is what makes the next such layer a compile error instead.
    ///
    /// A `Live` layer answers `TimeAxis::Live` in its own body. That is one
    /// line, and it is a claim someone made rather than one nobody noticed.
    fn time_axis(&self) -> TimeAxis;

    /// **The fewest frames a loop of this layer is worth building.**
    ///
    /// A *count*, because that is what makes a loop a loop: how many pictures
    /// it cycles. The seconds it costs are this layer's own
    /// [`TimeAxis::FrameSeries::typical_step`], and the conversion is
    /// [`Self::min_loop_span_secs`] — nothing above holds a window beside an
    /// id.
    ///
    /// **One global window cannot serve two cadences.** The timeline's
    /// Lookback slider names one number for the whole application; sixty
    /// minutes of it is a dozen radar volumes and *two* hourly satellite
    /// images. A layer whose frames are an hour apart declares here how wide
    /// its own window has to be before it is a loop at all.
    ///
    /// Zero — the default, and radar's answer — means the window is exactly
    /// the one the slider names, to the second.
    fn min_loop_frames(&self) -> usize {
        0
    }

    /// **The narrowest window this layer's loop may be listed over**, in
    /// seconds: [`Self::min_loop_frames`] frames of [`Self::time_axis`]'s own
    /// step, which is `n - 1` steps end to end.
    ///
    /// A *floor*, never the window itself — a caller takes the wider of this
    /// and whatever the user set, so widening the slider still widens every
    /// layer. Zero whenever the layer declares no minimum or has no step to
    /// multiply, and a layer answering zero is therefore untouched by the
    /// whole mechanism.
    fn min_loop_span_secs(&self) -> u64 {
        match (self.time_axis(), self.min_loop_frames()) {
            (TimeAxis::FrameSeries { typical_step, .. }, frames) if frames > 1 => {
                typical_step.as_secs().saturating_mul(frames as u64 - 1)
            }
            _ => 0,
        }
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

    /// **The identity of this layer's picture at `as_of`** — what the as-of
    /// half of the overlay cache token is really asking, for a layer that can
    /// answer it exactly.
    ///
    /// [`Self::as_of_quantum`] is a *proxy* for this question, and a loose
    /// one: it moves the token on every bucket the depicted instant crosses,
    /// whether or not a single item began or ended in that bucket. A pane
    /// clock sweeping at playback rate crosses buckets continuously — a tick
    /// worth ~5 min against a 60 s quantum — so the proxy mints a fresh
    /// whole-viewport raster per bucket for a picture that is, pixel for
    /// pixel, the one already on the glass.
    ///
    /// A layer that overrides this returns a value that is **equal whenever
    /// its picture at two instants is equal**, which is the contract
    /// `ui::map::pane_render::overlay_cache_token` states for the whole token
    /// and which the bucket proxy does not keep. `None` — the default — means
    /// "I cannot say", and the caller falls back to the quantum, so every
    /// layer that does not override this is unaffected byte for byte.
    ///
    /// **Cheap, and called on the frame thread**: once per pane per layer per
    /// frame on a scrubbed pane, and never at all on a live one (the caller
    /// returns before asking). An implementation walks its own items with
    /// comparisons it has already parsed; it must not allocate, hash a
    /// `String`, or take a lock.
    fn as_of_signature(&self, _pane: &PaneRef<'_>, _as_of: chrono::NaiveDateTime) -> Option<u64> {
        None
    }

    /// **What this layer would have to hold to draw `stops`** — the
    /// [`Residency`] it is asking for, coalesced.
    ///
    /// `stops` are every instant the pane's clock can come to rest on: a
    /// loop's frames, a parked scrub's one instant, or a live pane's wall
    /// clock. The layer answers in its own vocabulary — a
    /// [`TimeAxis::FrameSeries`] layer through
    /// [`crate::time::frame_residency`], a [`TimeAxis::EventLifetime`] layer
    /// with whatever slice of archive its picture at a stop is a function of.
    ///
    /// **This is the one time method that is not frame supply**, which is why
    /// it sits here rather than on [`FrameSource`]: the layer this method was
    /// written for is `EventLifetime` and has no frames at all. Lightning's
    /// picture at an instant is the flashes of the preceding window, and a
    /// caller reconstructing that from a *span* is what lit a twelve-hour
    /// loop on one frame — two authorities on one question, the loop armed
    /// over 43 200 s and the poll told 3 600 s.
    ///
    /// **The empty default is a real answer, not a degradation.** A
    /// [`TimeAxis::Live`] layer ignores the depicted instant entirely, so no
    /// set of stops obliges it to hold anything, and
    /// [`Residency::none`] says exactly that. It is wrong for every other
    /// arm, and a conformance walk asserts that every non-`Live` layer
    /// answers non-empty so it cannot be inherited where it matters.
    ///
    /// **A requirement, not a report.** Nothing here claims the layer *is*
    /// holding this; [`FrameSource::frames_resident`] is that question. A
    /// layer whose storage lives above its handler can still answer this one
    /// honestly, because what it would need is knowable without holding it.
    fn residency_for(&self, pane: &PaneRef<'_>, stops: &[chrono::NaiveDateTime]) -> Residency {
        let _ = (pane, stops);
        Residency::none()
    }

    /// **This layer's frame supply, if it has one.**
    ///
    /// `Some` from a [`TimeAxis::FrameSeries`] layer; `None` — the default,
    /// and eleven of this build's fifteen layers take it — from one whose
    /// picture is not a named frame.
    ///
    /// The same accessor shape as [`Self::volume`], and for the same reason.
    /// [`FrameSource`] carries nine methods with **no default bodies**, so a
    /// framed layer cannot declare half a supply; routing them through an
    /// accessor is what keeps the other eleven layers from having to write
    /// nine trivial ones. A `None` here is the layer saying it has no frames
    /// at all, which is a different statement from "I have frames and know
    /// nothing about them" — the statement the old defaults made on every
    /// layer's behalf, silently.
    ///
    /// [`Self::time_axis`] and this method are **paired**, and the walk that
    /// asserts the pairing lives above this crate: `FrameSeries` with no
    /// supply, and a supply on a layer that never declared the axis, are both
    /// defects.
    fn frames(&self) -> Option<&dyn FrameSource> {
        None
    }

    /// The `&mut` half of [`Self::frames`], for the three supply methods that
    /// take delivery ([`FrameSource::apply_frame_listing`],
    /// [`FrameSource::apply_frame`]) or evict
    /// ([`FrameSource::retain_frames`]).
    ///
    /// Two accessors rather than one because [`Self::volume`]'s read-only
    /// shape does not stretch: a frame supply is asked questions *and* handed
    /// arrivals, and a single `&mut` accessor would force every read through a
    /// mutable borrow of the whole handler.
    ///
    /// A layer answering `Some` from [`Self::frames`] answers `Some` here too
    /// — they are the same object seen through two borrows, never two
    /// different opinions about whether this layer comes in frames.
    fn frames_mut(&mut self) -> Option<&mut dyn FrameSource> {
        None
    }

    /// **Bytes of decoded SOURCE data this handler is holding on the host
    /// heap** — the grids and granules themselves, not the pictures
    /// rasterized from them and not the textures those pictures became.
    ///
    /// The denominator is the allocator's, on the instance the handler lives
    /// on: a `Vec<f32>` mosaic counts, a `TextureHandle`'s pixels after upload
    /// do not, and neither does anything a worker instance holds. A handler
    /// whose data is a few hundred parsed features answers the default `0`
    /// rather than pricing itself into a figure the caller is reading in
    /// megabytes; what this exists to find is the three gridded layers whose
    /// single granule is 60 to 98 MB.
    ///
    /// **Cheap, and read on the frame thread's telemetry tick**: an
    /// implementation reads a maintained total or walks a cache whose entry
    /// count is bounded by a byte budget of a handful of grids. It must not
    /// walk grid contents, allocate, or take a blocking lock.
    ///
    /// A report, never a requirement: nothing here says what the layer would
    /// need, only what it is holding right now.
    fn resident_source_bytes(&self) -> u64 {
        0
    }
}

pub struct FetchConfig {
    pub client: reqwest::Client,
    pub zone_cache_dir: Option<std::path::PathBuf>,
    /// Every origin a fetch may reach is declared here, not inline in URLs.
    pub sources: crate::origins::DataSources,
    /// `None` before the first frame is rendered. METAR must scope to this —
    /// the whole-country IEM form is 54 MB ungzipped.
    pub viewport: Option<squallar_geo::GeoBounds>,
    /// The instant the fetch is **for**, UTC — the wall clock on a live pane,
    /// the scrub instant on a scrubbed one. The same quantity
    /// [`RasterizeContext::as_of`] carries down the paint path, on the fetch
    /// path.
    ///
    /// A [`TimeAxis::EventLifetime`] source whose archive is addressable by
    /// time reads this to choose *which* archive objects to ask for, so a
    /// scrubbed pane fetches the past rather than the present. A `Live` source
    /// ignores it by contract, and a `FrameSeries` source names its frames
    /// through [`crate::time::FrameSource::create_frame_list_task`] instead — for both,
    /// the caller leaves this equal to the wall clock and no bytes move.
    ///
    /// [`TimeAxis`]: crate::time::TimeAxis
    pub as_of: chrono::NaiveDateTime,
    /// **How far back the pane can depict without another fetch, in seconds
    /// — `None` on a live pane.**
    ///
    /// The pane's timeline span (`PaneTimePosture::span_secs`), present
    /// exactly when [`Self::as_of`] was narrowed to a depicted instant: a
    /// parked scrub and a playing loop are the same posture, and under a loop
    /// `as_of` is one *sampled* instant of a clock that sweeps the whole span
    /// between polls. A [`TimeAxis::EventLifetime`] source whose archive is
    /// addressable by time reads this to fetch and retain the **window** the
    /// pane depicts rather than the instant one poll happened to sample —
    /// retention anchored on the sampled instant is what lit a two-hour GLM
    /// loop on a single frame. `None` leaves the fetch byte-for-byte what a
    /// live pane's always was.
    ///
    /// [`TimeAxis::EventLifetime`]: crate::time::TimeAxis::EventLifetime
    pub depicted_span_secs: Option<u64>,
    /// **The instants this pane can actually put on the glass** — the frames
    /// its transport layer holds, plus [`Self::as_of`] itself. Empty on a live
    /// pane, and empty on a pane whose loop has no frames yet.
    ///
    /// [`Self::depicted_span_secs`] says how *wide* the pane's timeline is;
    /// this says which slices of it are ever drawn, and the two stop being the
    /// same question the moment a loop's frames are further apart than an
    /// [`TimeAxis::EventLifetime`] layer's own window. A thirteen-frame
    /// satellite loop is **twelve hours** wide and depicts thirteen 300 s
    /// windows inside it — 65 minutes of archive, not 24 hours of it. A source
    /// given only the span must therefore either under-reach (the Lookback
    /// slider's one hour of that twelve, which is one frame lit and twelve
    /// blank) or ask the archive for the whole extent object by object.
    ///
    /// Ordering is not promised and duplicates are allowed: a reader takes the
    /// set. Empty means "no depicted frames to speak of", and a reader falls
    /// back to [`Self::depicted_span_secs`] — which is a parked scrub with no
    /// loop armed, where the span *is* the reach.
    ///
    /// [`TimeAxis::EventLifetime`]: crate::time::TimeAxis::EventLifetime
    pub depicted_frames: Vec<chrono::NaiveDateTime>,
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
    /// **The one frame this raster is being made for**, or `None` for the
    /// pane's own live picture.
    ///
    /// A [`TimeAxis::FrameSeries`] layer's picture is *one named frame*, and a
    /// loop wants several of them at once — so the dispatch that fills a loop
    /// says which frame each raster is, and [`SourceHandler::prepare_job`]
    /// reads it instead of the pane's current selection. `None` is the live
    /// dispatch and every handler must then behave exactly as it did before
    /// this field existed.
    ///
    /// **Not [`Self::as_of`], and the difference is not cosmetic.** `as_of` is
    /// a bare instant: it cannot name the run a forecast frame came from, and
    /// [`FrameStamp`] carries `run` precisely because two runs both publish a
    /// grid valid at the same instant. `as_of` is also the *pane's* depicted
    /// instant — on a live pane it is the wall clock — so a handler selecting
    /// its grid from it would change what a pane that is not looping at all
    /// draws.
    ///
    /// [`TimeAxis::FrameSeries`]: crate::time::TimeAxis::FrameSeries
    /// [`FrameStamp`]: crate::time::FrameStamp
    pub frame: Option<crate::time::FrameStamp>,
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
/// [`crate::time::FrameSource::create_frame_list_task`] returns an ordinary
/// [`FetchTask`], so build it through [`Self::task`] and read it back through
/// [`Self::event`]; a payload that did not come through `task` is a
/// programming error at the handler, and `event` says so by answering `None`
/// rather than fabricating an empty listing.
pub struct FrameListingResult {
    /// The generic half: what frames exist over the window that was asked
    /// about. Names no site.
    pub listing: FrameListing,
    /// The source's own half, captured at dispatch and handed straight back
    /// to [`crate::time::FrameSource::apply_frame_listing`].
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
///
/// # The eventing posture (written and verified at WO-M13b)
///
/// **This enum IS a source's arrival path — there is no other, and getting one
/// is not how a source is added.** The frontend holds exactly one channel of
/// `SourceEvent`, kind-agnostic: every layer's arrival travels on it and names
/// itself by [`LayerId`] in the payload rather than by the channel it came
/// down. That channel is drained in exactly one place, which
/// `every_hub_receiver_is_drained_by_exactly_one_row` holds from the frontend
/// side, and once per frame, which is measured rather than pinned (WO-M13b).
/// **A new source registers a handler; it never adds a channel** —
/// `the_channel_hub_only_ever_shrinks` refuses one that tries.
///
/// **[`SourceEvent::Data`] carries the raster-now obligation (WO-M13a).** It is
/// not only "here is this layer's new state": the pass that installs the data
/// is also the one that re-asks the panes already showing that layer, so the
/// pictures made from a `Data` arrival are redrawn on the frame it lands
/// rather than on whichever later frame a draw loop notices they went stale.
/// A handler that answers with `Data` should expect its rasterization to be
/// asked for immediately, not eventually.
///
/// **What the frontend's remaining channels are, so this posture is not read
/// as bigger than it is.** They are radar's own stage-plumbing — its
/// multi-stage ingest and its own raster replies — which survive because
/// amendment M-H scopes `RadarSource` out of [`SourceHandler::create_fetch_tasks`]:
/// the unified fetch seam cannot express a per-pane multi-stage ingest. They
/// only ever shrink, and the post-campaign per-pane fetch seam and the
/// LiveFeeds/chunk-transport fold are what shrink them. Per-type channels were
/// **kept deliberately**: one channel would order cross-type arrivals by
/// whichever background task finished first instead of by the frame pump's
/// pinned row order.
///
/// **The generic streaming-push verb stays DEFERRED.** Restated verbatim from
/// the plan's post-campaign register: *"Generic streaming-push verb (deferred
/// until a second real stream exists)."* This enum is a **pull**-shaped
/// vocabulary: three arms, each the answer to a question a handler was asked.
/// Radar's chunk feed is the one real stream in the tree, and a push verb
/// generalised from a single implementor is a guess — so the fence stands
/// until a second one exists.
pub enum SourceEvent {
    /// Today's payload, unchanged — a whole fetch round for a layer.
    Data(OverlayFetchResult),
    /// What frames a layer can show, in answer to
    /// [`crate::time::FrameSource::create_frame_list_task`], with the **scope** that
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
    /// One frame's data, in answer to [`crate::time::FrameSource::fetch_frame`].
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
