//! The source-handler contract: the trait every layer implements, and the
//! vocabulary its methods speak.

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

    /// A cheap token for **what this handler would draw**: a refetch returning
    /// the same content should keep it stable. Called every frame, so it must
    /// not clone.
    fn content_signature(&self) -> u64 {
        self.data_generation()
    }

    /// Whether this handler's **cached raster** would come out differently in
    /// the other theme. A caching property, not a rendering one: a
    /// [`RenderMode::PerFramePoint`] layer holds no cached raster, so it answers
    /// `false` however theme-dependent it looks.
    fn theme_sensitive(&self) -> bool {
        false
    }

    fn has_data(&self) -> bool;

    fn is_fetching(&self) -> bool;

    fn set_fetching(&mut self, fetching: bool);

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

    fn item_count(&self) -> usize {
        0
    }

    fn is_enabled(&self) -> bool {
        true
    }

    fn set_enabled(&mut self, _enabled: bool) {}

    /// One line of live status for the layer stack's row — `"3 shown · W/Wa"`.
    /// Called every frame; a disabled handler returns `None`.
    fn status_line(&self) -> Option<String> {
        None
    }


    fn create_fetch_tasks(&self, ctx: &FetchConfig) -> Vec<FetchTask> {
        let _ = ctx;
        Vec::new()
    }

    fn apply_fetch_result(&mut self, result: FetchPayload);


    /// This handler's raster as a described job, or `None` when there is nothing
    /// to render. `has_data()` must answer `false` exactly when this answers
    /// `None`, or the settle machinery asks for a render nothing can satisfy.
    fn prepare_job(&self, ctx: &RasterizeContext) -> Option<DescribedJob> {
        let _ = ctx;
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
    fn clickable_items(&self) -> Vec<ClickableItem<'_>> {
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

    /// Drops selections whose `matches()` finds nothing in the refreshed data.
    fn retain_selections(&self, selections: &mut Vec<Arc<dyn OverlayItem>>);


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

    fn hover_value_at(&self, _lat: f64, _lon: f64) -> Option<String> {
        None
    }

    /// This layer's colour bar, [`Signed`] so a caller can keep what it baked:
    /// the bar is sampled once per pixel of its length, at frame rate.
    fn legend(&self) -> Option<Signed<OverlayLegend>> {
        None
    }


    fn controls(&self, _ctx: &PaneControlContext<'_>) -> Vec<ControlItem> {
        Vec::new()
    }

    /// The returned [`ControlEffect`] asks the caller for a side-effect.
    fn apply_control(
        &mut self,
        _update: &ControlUpdate,
        _ctx: &mut PaneControlContextMut<'_>,
    ) -> ControlEffect {
        ControlEffect::None
    }


    /// e.g. selected product, loop state. `None` if there is no per-pane state.
    fn create_pane_state(&self) -> Option<FetchPayload> {
        None
    }


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


    fn supports_loop(&self) -> bool {
        false
    }

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
    /// The page's clock at dispatch, UTC — GLM's flash-age fade is the only
    /// reader. Captured at the dispatch so a worker matches the direct call.
    pub now: chrono::NaiveDateTime,
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
