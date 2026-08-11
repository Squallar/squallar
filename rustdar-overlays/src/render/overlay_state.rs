use std::any::Any;
use std::fmt::Debug;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use rustdar_units::UserPreferences;

use crate::render::controls::{
    ControlEffect, ControlItem, ControlUpdate, PaneControlContext, PaneControlContextMut,
};
use crate::render::draw::{DrawPointContext, HoverContext, MapPoint, PointPainter};
use crate::render::rasterize::RasterizeOutput;
use crate::types::{GeoBounds, OverlayFeature, OverlayLabel};

pub type RasterizeFn = Box<dyn FnOnce(&GeoBounds, u32, u32) -> RasterizeOutput + Send>;

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
pub struct OverlayState<T> {
    pub data: T,
    pub fetch_time: Option<web_time::Instant>,
    pub fetching: bool,
    pub data_generation: u64,
}

impl<T: Default> Default for OverlayState<T> {
    fn default() -> Self {
        Self {
            data: T::default(),
            fetch_time: None,
            fetching: false,
            data_generation: 0,
        }
    }
}

impl<T: Default> OverlayState<T> {
    pub fn new() -> Self {
        Self::default()
    }
}

impl<T> OverlayState<T> {
    /// Bumps `data_generation`, which is what invalidates cached textures.
    pub fn set_data(&mut self, data: T) {
        self.data = data;
        self.fetch_time = Some(web_time::Instant::now());
        self.data_generation = self.data_generation.wrapping_add(1);
    }

    /// True when nothing has been fetched yet, **or** `interval_secs` has
    /// elapsed since the last fetch. The second branch is how every
    /// auto-polling overlay refreshes.
    pub fn needs_refresh(&self, interval_secs: u64) -> bool {
        self.fetch_time
            .is_none_or(|t| t.elapsed().as_secs() >= interval_secs)
    }
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
/// `rustdar-platform` changes.
pub trait OverlayHandler: Send {
    // ── Identity & metadata ───────────────────────────────────────────

    fn kind(&self) -> OverlayKind;

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
    /// re-render only when the picture changes (the 3D floor's composite)
    /// compare it across frames. Unlike [`data_generation`], a refetch that
    /// returns the same content should keep it stable where the handler can
    /// tell — NWS alerts poll every two minutes and mostly return the same
    /// warning set, and a floor recomposed on every poll is a floor
    /// recomposed for nothing. The default is `data_generation`, correct for
    /// every handler that cannot tell (it may only over-refresh, never
    /// under-refresh). Called every frame, so it must not clone data.
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

    /// `None` when there is nothing to render.
    fn prepare_rasterize(&self, ctx: &RasterizeContext) -> Option<RasterizeFn> {
        let _ = ctx;
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
    pub sources: rustdar_radar::sources::DataSources,
    /// `None` before the first frame is rendered. METAR must scope to this —
    /// the whole-country IEM form is 54 MB ungzipped; see
    /// [`crate::metar::networks`] for the no-viewport fallback.
    pub viewport: Option<crate::types::GeoBounds>,
}

pub struct RasterizeContext {
    pub is_dark: bool,
    pub zoom: f64,
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
// **Do not relax the renderer's bounds to match.** `rustdar-frontend`'s render
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
    pub kind: OverlayKind,
    pub future: TaskFuture,
}

// ── Overlay registry ─────────────────────────────────────────────────────

pub struct OverlayRegistry {
    handlers: Vec<Box<dyn OverlayHandler>>,
    /// Populated by map clicks; paged through in the popup.
    pub selected_overlays: Vec<Arc<dyn OverlayItem>>,
    pub selected_overlay_page: usize,
}

impl Default for OverlayRegistry {
    fn default() -> Self {
        Self {
            handlers: super::handlers::create_handlers(),
            selected_overlays: Vec::new(),
            selected_overlay_page: 0,
        }
    }
}

impl OverlayRegistry {
    fn handler(&self, kind: OverlayKind) -> Option<&dyn OverlayHandler> {
        self.handlers
            .iter()
            .find(|h| h.kind() == kind)
            .map(|h| &**h)
    }

    fn handler_mut(&mut self, kind: OverlayKind) -> Option<&mut dyn OverlayHandler> {
        for handler in &mut self.handlers {
            if handler.kind() == kind {
                return Some(&mut **handler);
            }
        }
        None
    }

    pub fn handlers(&self) -> impl Iterator<Item = &dyn OverlayHandler> {
        self.handlers.iter().map(|h| &**h)
    }

    pub fn get_handler(&self, kind: OverlayKind) -> Option<&dyn OverlayHandler> {
        self.handler(kind)
    }

    pub fn get_handler_mut(&mut self, kind: OverlayKind) -> Option<&mut dyn OverlayHandler> {
        self.handler_mut(kind)
    }

    pub fn data_generation(&self, kind: OverlayKind) -> u64 {
        self.handler(kind).map_or(0, |h| h.data_generation())
    }

    /// [`OverlayHandler::content_signature`] for `kind`; `0` for a kind with
    /// no handler.
    pub fn content_signature(&self, kind: OverlayKind) -> u64 {
        self.handler(kind).map_or(0, |h| h.content_signature())
    }

    /// The NWS alert fetch payload for a known alert list, exactly as the
    /// network fetch would deliver it to [`apply_fetch_result`]. Public so a
    /// host (or its tests) can feed a chosen warning set through the one
    /// production ingest path instead of growing a parallel setter.
    ///
    /// [`apply_fetch_result`]: OverlayRegistry::apply_fetch_result
    #[doc(hidden)]
    pub fn nws_alerts_payload(alerts: Vec<crate::nws::alert::NwsAlert>) -> FetchPayload {
        Box::new(super::handlers::alert::NwsAlertFetchResult(Ok(alerts)))
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

    pub fn has_data(&self, kind: OverlayKind) -> bool {
        self.handler(kind).is_some_and(|h| h.has_data())
    }

    pub fn is_fetching(&self, kind: OverlayKind) -> bool {
        self.handler(kind).is_some_and(|h| h.is_fetching())
    }

    pub fn set_fetching(&mut self, kind: OverlayKind, fetching: bool) {
        if let Some(h) = self.handler_mut(kind) {
            h.set_fetching(fetching);
        }
    }

    pub fn fetch_time(&self, kind: OverlayKind) -> Option<web_time::Instant> {
        self.handler(kind).and_then(|h| h.fetch_time())
    }

    pub fn auto_poll_interval(&self, kind: OverlayKind) -> Option<u64> {
        self.handler(kind).and_then(|h| h.auto_poll_interval())
    }

    pub fn item_count(&self, kind: OverlayKind) -> usize {
        self.handler(kind).map_or(0, |h| h.item_count())
    }

    pub fn is_enabled(&self, kind: OverlayKind) -> bool {
        self.handler(kind).is_some_and(|h| h.is_enabled())
    }

    pub fn set_enabled(&mut self, kind: OverlayKind, enabled: bool) {
        if let Some(h) = self.handler_mut(kind) {
            h.set_enabled(enabled);
        }
    }

    /// [`OverlayHandler::status_line`] for `kind`; `None` for a kind with no
    /// handler.
    pub fn status_line(&self, kind: OverlayKind) -> Option<String> {
        self.handler(kind).and_then(|h| h.status_line())
    }

    pub fn clickable_items(&self, kind: OverlayKind) -> Vec<ClickableItem<'_>> {
        self.handler(kind)
            .map_or_else(Vec::new, |h| h.clickable_items())
    }

    /// [`OverlayHandler::map_labels`] for `kind`; empty for a kind with no
    /// handler.
    pub fn map_labels(&self, kind: OverlayKind) -> &[OverlayLabel] {
        self.handler(kind).map_or(&[], |h| h.map_labels())
    }

    pub fn hover_value_at(&self, kind: OverlayKind, lat: f64, lon: f64) -> Option<String> {
        self.handler(kind).and_then(|h| h.hover_value_at(lat, lon))
    }

    pub fn legend(&self, kind: OverlayKind) -> Option<OverlayLegend> {
        self.handler(kind).and_then(|h| h.legend())
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
        self.handler_mut(kind)
            .is_some_and(|h| h.handle_popup_action(action))
    }

    /// Re-runs `retain_selections` afterwards, since the data just changed.
    pub fn apply_fetch_result(&mut self, result: OverlayFetchResult) {
        let kind = result.kind;
        if let Some(idx) = self.handlers.iter().position(|h| h.kind() == kind) {
            self.handlers[idx].apply_fetch_result(result.data);
            self.handlers[idx].retain_selections(&mut self.selected_overlays);
        }
        if self.selected_overlay_page >= self.selected_overlays.len().max(1) {
            self.selected_overlay_page = 0;
        }
    }

    pub fn prepare_rasterize(
        &self,
        kind: OverlayKind,
        ctx: &RasterizeContext,
    ) -> Option<RasterizeFn> {
        self.handler(kind).and_then(|h| h.prepare_rasterize(ctx))
    }

    pub fn create_fetch_tasks(&self, kind: OverlayKind, ctx: &FetchConfig) -> Vec<FetchTask> {
        self.handler(kind)
            .map_or_else(Vec::new, |h| h.create_fetch_tasks(ctx))
    }

    pub fn controls(&self, kind: OverlayKind, ctx: &PaneControlContext<'_>) -> Vec<ControlItem> {
        self.handler(kind)
            .map_or_else(Vec::new, |h| h.controls(ctx))
    }

    pub fn apply_control(
        &mut self,
        kind: OverlayKind,
        update: &ControlUpdate,
        ctx: &mut PaneControlContextMut<'_>,
    ) -> ControlEffect {
        if let Some(h) = self.handler_mut(kind) {
            h.apply_control(update, ctx)
        } else {
            ControlEffect::None
        }
    }

    pub fn render_mode(&self, kind: OverlayKind) -> Option<RenderMode> {
        self.handler(kind).map(|h| h.render_mode())
    }

    pub fn display_name(&self, kind: OverlayKind) -> &str {
        self.handler(kind).map_or("Unknown", |h| h.display_name())
    }

    pub fn default_enabled(&self, kind: OverlayKind) -> bool {
        self.handler(kind).is_some_and(|h| h.default_enabled())
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
    pub fn load_pane_configs(
        &mut self,
        configs: &std::collections::HashMap<OverlayKind, serde_json::Value>,
    ) {
        for h in &mut self.handlers {
            if let Some(val) = configs.get(&h.kind()) {
                h.deserialize_state(val.clone());
            }
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
        self.handler(kind).map_or(&[], |h| h.per_frame_points())
    }

    pub fn draw_point(
        &self,
        kind: OverlayKind,
        id: u32,
        painter: &mut dyn PointPainter,
        ctx: &DrawPointContext,
    ) {
        if let Some(h) = self.handler(kind) {
            h.draw_point(id, painter, ctx);
        }
    }

    pub fn point_hit_radius(&self, kind: OverlayKind, zoom: f32) -> f32 {
        self.handler(kind).map_or(0.0, |h| h.point_hit_radius(zoom))
    }

    pub fn hover_text(&self, kind: OverlayKind, id: u32, ctx: &HoverContext<'_>) -> Option<String> {
        self.handler(kind).and_then(|h| h.hover_text(id, ctx))
    }

    // ── Config persistence ────────────────────────────────────────────

    /// Keyed by the `Debug` spelling of `OverlayKind`, so renaming a variant
    /// orphans its saved state. Null states are omitted.
    pub fn serialize_handler_states(&self) -> serde_json::Map<String, serde_json::Value> {
        let mut map = serde_json::Map::new();
        for h in &self.handlers {
            let val = h.serialize_state();
            if !val.is_null() {
                map.insert(format!("{:?}", h.kind()), val);
            }
        }
        map
    }

    pub fn deserialize_handler_states(
        &mut self,
        states: &serde_json::Map<String, serde_json::Value>,
    ) {
        for h in &mut self.handlers {
            let key = format!("{:?}", h.kind());
            if let Some(val) = states.get(&key) {
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
}

// ── Unified overlay fetch result ──────────────────────────────────────────

pub struct OverlayFetchResult {
    pub kind: OverlayKind,
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
