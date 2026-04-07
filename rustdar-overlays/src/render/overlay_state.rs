use std::any::Any;
use std::fmt::Debug;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use rustdar_units::UserPreferences;

use crate::render::controls::{ControlEffect, ControlItem, ControlUpdate, PaneControlContext, PaneControlContextMut};
use crate::render::draw::{DrawPointContext, HoverContext, MapPoint, PointPainter};
use crate::render::rasterize::RasterizeOutput;
use crate::types::{GeoBounds, OverlayFeature, OverlayLabel};

/// Generic wrapper for overlay data that follows the fetch-cache-generation pattern.
///
/// Each overlay type (SPC outlooks, NWS alerts, SPC discussions) has the same
/// lifecycle: data is fetched asynchronously, cached locally, and invalidated
/// via a generation counter when new data arrives.  This struct captures that
/// shared pattern, reducing scattered fields on `Gui`.
pub struct OverlayState<T> {
    pub data: T,
    pub fetch_time: Option<std::time::Instant>,
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
    /// Replace the data and bump the generation counter.
    pub fn set_data(&mut self, data: T) {
        self.data = data;
        self.fetch_time = Some(std::time::Instant::now());
        self.data_generation = self.data_generation.wrapping_add(1);
    }

    /// Whether a refresh is due (no data yet, or `interval` has elapsed since last fetch).
    pub fn needs_refresh(&self, interval_secs: u64) -> bool {
        self.fetch_time
            .map_or(true, |t| t.elapsed().as_secs() >= interval_secs)
    }
}

/// How an overlay is rendered on the map.
///
/// Handlers declare their render mode; the draw loop dispatches generically
/// based on this rather than matching on `OverlayKind` variants.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderMode {
    /// Pre-rasterized to an RGBA texture on a background thread (SPC, NWS, Radar, etc.).
    Texture,
    /// Drawn each frame using the abstract [`PointPainter`] trait (METAR station models).
    PerFramePoint,
    /// Drawn each frame by the handler via framework-agnostic primitives (UserLocation).
    PerFrameDirect,
    /// Streaming tile layer managed by the map widget (BaseMap, CityLabels).
    Tile,
}

/// A clickable overlay data item that can appear in the popup pager.
///
/// Replaces the old fixed `SelectedOverlay` enum. Handlers store their data as
/// `Vec<Arc<T>>` where `T: OverlayItem`; when the user clicks, the `Arc` is
/// cloned into the selection list as `Arc<dyn OverlayItem>`.
pub trait OverlayItem: Send + Sync + Debug {
    /// Which overlay kind this item belongs to.
    fn kind(&self) -> OverlayKind;

    /// Build the popup content for this item's detail view.
    fn popup_content(&self, prefs: &UserPreferences) -> PopupContent;

    /// Identity match: returns `true` if `other` represents the same logical
    /// item (e.g. same alert ID, same MD number). Used by `retain_selections()`
    /// to map old selections to refreshed data.
    fn matches(&self, other: &dyn OverlayItem) -> bool;

    /// Downcast to `&dyn Any` for concrete type comparisons in `matches()`.
    fn as_any(&self) -> &dyn Any;
}

/// An overlay item that can be clicked and optionally labelled on the map.
///
/// Returned by [`OverlayHandler::clickable_items()`] so that the UI crate can
/// perform hit-testing and label drawing without knowing overlay-specific types.
pub struct ClickableItem {
    /// Renderable polygon features for hit-testing.
    pub features: Vec<OverlayFeature>,
    /// Optional map label to draw at a geographic position.
    pub label: Option<OverlayLabel>,
    /// The data item to store when the user clicks this item.
    pub item: Arc<dyn OverlayItem>,
}

// ── Overlay handler trait ─────────────────────────────────────────────────

/// Trait for overlay type handlers. Each overlay type implements this to
/// encapsulate its data, fetch logic, render logic, UI controls, and popup
/// content.
///
/// Adding a new overlay only requires:
/// 1. Implementing this trait in a new handler struct
/// 2. Registering it in `create_handlers()`
/// 3. Adding an `OverlayKind` variant
///
/// No changes to `rustdar-egui` or `rustdar-platform` are required.
pub trait OverlayHandler: Send {
    // ── Identity & metadata ───────────────────────────────────────────

    /// Which overlay kind this handler manages.
    fn kind(&self) -> OverlayKind;

    /// Human-readable display name (e.g. "Radar", "NWS Alerts").
    fn display_name(&self) -> &str;

    /// How this overlay is rendered on the map.
    fn render_mode(&self) -> RenderMode;

    /// Whether this overlay is enabled by default in a new pane.
    fn default_enabled(&self) -> bool { false }

    // ── Data lifecycle ────────────────────────────────────────────────

    /// Current data generation counter for cache invalidation.
    fn data_generation(&self) -> u64;

    /// Whether any data has been fetched.
    fn has_data(&self) -> bool;

    /// Whether a fetch is currently in flight.
    fn is_fetching(&self) -> bool;

    /// Set the fetching flag.
    fn set_fetching(&mut self, fetching: bool);

    /// Timestamp of the last completed fetch.
    fn fetch_time(&self) -> Option<std::time::Instant>;

    /// Auto-poll interval in seconds, or `None` if this overlay doesn't auto-poll.
    fn auto_poll_interval(&self) -> Option<u64> { None }

    /// Number of loaded data items.
    fn item_count(&self) -> usize { 0 }

    /// Whether this overlay is currently enabled (considering handler internal toggles).
    ///
    /// Replaces the old `OverlayKind::is_enabled(layers)` — each handler owns
    /// its own toggle state and checks it here.
    fn is_enabled(&self) -> bool { true }

    /// Set the enabled state directly. Only meaningful for simple toggle handlers.
    fn set_enabled(&mut self, _enabled: bool) {}

    // ── Fetching ──────────────────────────────────────────────────────

    /// Create async fetch tasks for this overlay's data.
    fn create_fetch_tasks(&self, ctx: &FetchConfig) -> Vec<FetchTask> {
        let _ = ctx;
        Vec::new()
    }

    /// Apply a type-erased fetch result. The handler downcasts to its expected type.
    fn apply_fetch_result(&mut self, result: Box<dyn Any + Send>);

    // ── Rendering (texture mode) ──────────────────────────────────────

    /// Prepare a rasterization closure for background rendering.
    /// Returns `None` if there's nothing to render.
    fn prepare_rasterize(
        &self,
        ctx: &RasterizeContext,
    ) -> Option<Box<dyn FnOnce(&GeoBounds, u32, u32) -> RasterizeOutput + Send>> {
        let _ = ctx;
        None
    }

    // ── Click & selection ─────────────────────────────────────────────

    /// Build clickable/labelled items for hit-testing on the map.
    fn clickable_items(&self) -> Vec<ClickableItem> { Vec::new() }

    /// Handle a popup action button. Returns `true` if the action was handled.
    fn handle_popup_action(&mut self, _action: &PopupAction) -> bool { false }

    /// Remove stale selections that no longer exist in the handler's data.
    /// Keeps selections whose `matches()` still finds a corresponding item.
    fn retain_selections(&self, selections: &mut Vec<Arc<dyn OverlayItem>>);

    // ── Per-frame point rendering (opt-in for PerFramePoint mode) ─────

    /// Return geographic points to be drawn per-frame by the UI crate.
    fn per_frame_points(&self) -> &[MapPoint] { &[] }

    /// Draw a single point using abstract drawing primitives.
    fn draw_point(&self, _id: u32, _painter: &mut dyn PointPainter, _ctx: &DrawPointContext) {}

    /// Clickable radius in screen pixels for hit-testing around each point.
    fn point_hit_radius(&self, _zoom: f32) -> f32 { 0.0 }

    /// Tooltip text shown on hover. Return `None` to suppress the tooltip.
    fn hover_text(&self, _id: u32, _ctx: &HoverContext<'_>) -> Option<String> { None }

    // ── Declarative UI controls ───────────────────────────────────────

    /// Describe this overlay's UI controls declaratively.
    /// The egui crate renders these generically.
    fn controls(&self, _ctx: &PaneControlContext<'_>) -> Vec<ControlItem> { Vec::new() }

    /// Apply a control update from the UI. The handler interprets the
    /// control `id` and `value` to update its internal state.
    /// Returns a [`ControlEffect`] signalling any side-effects the caller
    /// should perform (e.g. triggering a data fetch).
    fn apply_control(&mut self, _update: &ControlUpdate, _ctx: &mut PaneControlContextMut<'_>) -> ControlEffect { ControlEffect::None }

    // ── Per-pane state ────────────────────────────────────────────────

    /// Create initial per-pane handler state (e.g. selected product, loop state).
    /// Returns `None` if this handler has no per-pane state.
    fn create_pane_state(&self) -> Option<Box<dyn Any + Send>> { None }

    // ── Config persistence ────────────────────────────────────────────

    /// Serialize this handler's global state for config persistence.
    fn serialize_state(&self) -> serde_json::Value { serde_json::Value::Null }

    /// Restore global state from a previously serialized value.
    fn deserialize_state(&mut self, _value: serde_json::Value) {}

    /// Serialize per-pane handler state.
    fn serialize_pane_state(&self, _state: &dyn Any) -> serde_json::Value {
        serde_json::Value::Null
    }

    /// Restore per-pane handler state from a serialized value.
    fn deserialize_pane_state(&self, _value: serde_json::Value) -> Option<Box<dyn Any + Send>> {
        None
    }

    // ── Loop support (opt-in) ─────────────────────────────────────────

    /// Whether this overlay supports time-series loop animation.
    fn supports_loop(&self) -> bool { false }

    /// Create an async task that lists available frames for a time range.
    /// Returns timestamps of available frames.
    fn create_loop_list_task(
        &self,
        _ctx: &FetchConfig,
        _start: chrono::NaiveDateTime,
        _end: chrono::NaiveDateTime,
    ) -> Option<FetchTask> {
        None
    }

    /// Create an async task to download a single loop frame's data.
    fn create_loop_frame_task(
        &self,
        _ctx: &FetchConfig,
        _timestamp: chrono::NaiveDateTime,
    ) -> Option<FetchTask> {
        None
    }
}

/// Context for creating overlay fetch tasks.
pub struct FetchConfig {
    pub client: reqwest::Client,
    pub zone_cache_dir: Option<std::path::PathBuf>,
}

/// Context for preparing overlay rasterization closures.
pub struct RasterizeContext {
    /// Whether the app is in dark theme.
    pub is_dark: bool,
    /// Current map zoom level.
    pub zoom: f64,
}

/// An async fetch task produced by a handler.
pub struct FetchTask {
    pub kind: OverlayKind,
    pub future: Pin<Box<dyn Future<Output = Box<dyn Any + Send>> + Send>>,
}

// ── Overlay registry ─────────────────────────────────────────────────────

/// Central overlay state, replacing the old per-type fields.
/// Contains registered overlay handlers and shared popup pager state.
pub struct OverlayRegistry {
    handlers: Vec<Box<dyn OverlayHandler>>,
    /// Overlay items selected for the popup pager (from map clicks).
    pub selected_overlays: Vec<Arc<dyn OverlayItem>>,
    /// Current page in the popup pager.
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
        self.handlers.iter().find(|h| h.kind() == kind).map(|h| &**h)
    }

    fn handler_mut(&mut self, kind: OverlayKind) -> Option<&mut dyn OverlayHandler> {
        for handler in &mut self.handlers {
            if handler.kind() == kind {
                return Some(&mut **handler);
            }
        }
        None
    }

    /// Iterate over all registered handlers.
    pub fn handlers(&self) -> impl Iterator<Item = &dyn OverlayHandler> {
        self.handlers.iter().map(|h| &**h)
    }

    /// Get the handler for a specific overlay kind.
    pub fn get_handler(&self, kind: OverlayKind) -> Option<&dyn OverlayHandler> {
        self.handler(kind)
    }

    /// Get a mutable handler for a specific overlay kind.
    pub fn get_handler_mut(&mut self, kind: OverlayKind) -> Option<&mut dyn OverlayHandler> {
        self.handler_mut(kind)
    }

    pub fn data_generation(&self, kind: OverlayKind) -> u64 {
        self.handler(kind).map_or(0, |h| h.data_generation())
    }

    pub fn has_data(&self, kind: OverlayKind) -> bool {
        self.handler(kind).map_or(false, |h| h.has_data())
    }

    pub fn is_fetching(&self, kind: OverlayKind) -> bool {
        self.handler(kind).map_or(false, |h| h.is_fetching())
    }

    pub fn set_fetching(&mut self, kind: OverlayKind, fetching: bool) {
        if let Some(h) = self.handler_mut(kind) {
            h.set_fetching(fetching);
        }
    }

    pub fn fetch_time(&self, kind: OverlayKind) -> Option<std::time::Instant> {
        self.handler(kind).and_then(|h| h.fetch_time())
    }

    pub fn auto_poll_interval(&self, kind: OverlayKind) -> Option<u64> {
        self.handler(kind).and_then(|h| h.auto_poll_interval())
    }

    pub fn item_count(&self, kind: OverlayKind) -> usize {
        self.handler(kind).map_or(0, |h| h.item_count())
    }

    pub fn is_enabled(&self, kind: OverlayKind) -> bool {
        self.handler(kind).map_or(false, |h| h.is_enabled())
    }

    pub fn set_enabled(&mut self, kind: OverlayKind, enabled: bool) {
        if let Some(h) = self.handler_mut(kind) {
            h.set_enabled(enabled);
        }
    }

    pub fn clickable_items(&self, kind: OverlayKind) -> Vec<ClickableItem> {
        self.handler(kind).map_or_else(Vec::new, |h| h.clickable_items())
    }

    /// Build popup content for a selected overlay item.
    pub fn popup_content(&self, selected: &dyn OverlayItem, prefs: &UserPreferences) -> PopupContent {
        selected.popup_content(prefs)
    }

    /// Execute a popup action by routing to the owning handler.
    pub fn handle_popup_action(&mut self, action: &PopupAction) -> bool {
        let kind = action.target.kind();
        self.handler_mut(kind).is_some_and(|h| h.handle_popup_action(action))
    }

    /// Apply a type-erased fetch result from the unified overlay channel.
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

    /// Prepare a rasterize closure for background overlay rendering.
    pub fn prepare_rasterize(
        &self,
        kind: OverlayKind,
        ctx: &RasterizeContext,
    ) -> Option<Box<dyn FnOnce(&GeoBounds, u32, u32) -> RasterizeOutput + Send>> {
        self.handler(kind).and_then(|h| h.prepare_rasterize(ctx))
    }

    /// Create async fetch tasks for the given overlay kind.
    pub fn create_fetch_tasks(&self, kind: OverlayKind, ctx: &FetchConfig) -> Vec<FetchTask> {
        self.handler(kind).map_or_else(Vec::new, |h| h.create_fetch_tasks(ctx))
    }

    /// Describe UI controls for the given overlay kind.
    pub fn controls(&self, kind: OverlayKind, ctx: &PaneControlContext<'_>) -> Vec<ControlItem> {
        self.handler(kind).map_or_else(Vec::new, |h| h.controls(ctx))
    }

    /// Apply a control update to the owner handler.
    pub fn apply_control(&mut self, kind: OverlayKind, update: &ControlUpdate, ctx: &mut PaneControlContextMut<'_>) -> ControlEffect {
        if let Some(h) = self.handler_mut(kind) {
            h.apply_control(update, ctx)
        } else {
            ControlEffect::None
        }
    }

    /// Get the render mode for the given overlay kind.
    pub fn render_mode(&self, kind: OverlayKind) -> Option<RenderMode> {
        self.handler(kind).map(|h| h.render_mode())
    }

    /// Get the display name for the given overlay kind.
    pub fn display_name(&self, kind: OverlayKind) -> &str {
        self.handler(kind).map_or("Unknown", |h| h.display_name())
    }

    /// Whether the given overlay kind is enabled.
    pub fn default_enabled(&self, kind: OverlayKind) -> bool {
        self.handler(kind).is_some_and(|h| h.default_enabled())
    }

    // ── Per-frame point rendering delegates ───────────────────────────

    /// Geographic points for per-frame rendering of the given overlay kind.
    pub fn per_frame_points(&self, kind: OverlayKind) -> &[MapPoint] {
        self.handler(kind).map_or(&[], |h| h.per_frame_points())
    }

    /// Draw a single point for the given overlay kind.
    pub fn draw_point(&self, kind: OverlayKind, id: u32, painter: &mut dyn PointPainter, ctx: &DrawPointContext) {
        if let Some(h) = self.handler(kind) {
            h.draw_point(id, painter, ctx);
        }
    }

    /// Clickable radius for the given overlay kind at the current zoom.
    pub fn point_hit_radius(&self, kind: OverlayKind, zoom: f32) -> f32 {
        self.handler(kind).map_or(0.0, |h| h.point_hit_radius(zoom))
    }

    /// Tooltip text for a point in the given overlay kind.
    pub fn hover_text(&self, kind: OverlayKind, id: u32, ctx: &HoverContext<'_>) -> Option<String> {
        self.handler(kind).and_then(|h| h.hover_text(id, ctx))
    }

    // ── Config persistence ────────────────────────────────────────────

    /// Serialize all handler states for config persistence.
    /// Returns a map of overlay kind name → serialized state.
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

    /// Restore handler states from previously serialized data.
    pub fn deserialize_handler_states(&mut self, states: &serde_json::Map<String, serde_json::Value>) {
        for h in &mut self.handlers {
            let key = format!("{:?}", h.kind());
            if let Some(val) = states.get(&key) {
                h.deserialize_state(val.clone());
            }
        }
    }
}

// ── Generic overlay kind ─────────────────────────────────────────────────

/// Identifies each map layer that participates in the per-pane draw order.
///
/// Texture-overlay variants (SpcOutlook, SpcDiscussions, NwsAlerts) are
/// rasterized to textures on background threads. Non-texture variants
/// (Radar, CityLabels, RadarSites, UserLocation) are drawn directly.
///
/// Used as a HashMap key for per-pane texture caches, in render requests,
/// and in the per-pane `draw_order` vec. Adding a new layer type only
/// requires adding a variant here and implementing the match arms below.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum OverlayKind {
    SpcOutlook,
    SpcDiscussions,
    NwsAlerts,
    StormReports,
    Metar,
    Radar,
    CityLabels,
    RadarSites,
    UserLocation,
}

impl OverlayKind {
    /// All registered layer kinds in default draw order.
    pub const fn all() -> &'static [OverlayKind] {
        &[
            OverlayKind::SpcOutlook,
            OverlayKind::Radar,
            OverlayKind::SpcDiscussions,
            OverlayKind::NwsAlerts,
            OverlayKind::StormReports,
            OverlayKind::Metar,
            OverlayKind::CityLabels,
            OverlayKind::RadarSites,
            OverlayKind::UserLocation,
        ]
    }

    /// Default draw order (bottom to top) for a new pane.
    pub fn default_draw_order() -> Vec<OverlayKind> {
        Self::all().to_vec()
    }
}

// ── Unified overlay fetch result ──────────────────────────────────────────

/// A type-erased fetch result from any overlay handler, sent through the
/// unified overlay fetch channel.
pub struct OverlayFetchResult {
    pub kind: OverlayKind,
    pub data: Box<dyn Any + Send>,
}

// ── Popup content descriptors ─────────────────────────────────────────────

/// Describes the full content of an overlay detail popup, to be rendered
/// generically by the UI crate. The overlay crate builds these; the UI crate
/// draws them without knowing what overlay type produced them.
pub struct PopupContent {
    /// Popup window title text.
    pub title: String,
    /// Accent color `[r, g, b]` for the title and highlights.
    pub accent_rgb: [u8; 3],
    /// Desktop popup width (mobile auto-sizes to screen).
    pub width: f32,
    /// Ordered content sections.
    pub sections: Vec<PopupSection>,
    /// Actions the popup can trigger (rendered as buttons at the bottom).
    pub actions: Vec<PopupAction>,
}

/// A single section of popup content.
pub enum PopupSection {
    /// Bold heading text.
    Heading(String),
    /// Normal text label.
    Text(String),
    /// Colored text.
    ColoredText { text: String, rgb: [u8; 3], bold: bool },
    /// Key-value metadata rows.
    KeyValueGrid(Vec<(String, String)>),
    /// Long text in a scroll area, optionally monospace.
    ScrollableText { text: String, monospace: bool, max_height: f32 },
    /// A horizontal rule separator.
    Separator,
    /// A clickable hyperlink.
    Link { label: String, url: String },
}

/// An action button in the popup. The UI crate renders it; the overlay crate
/// defines what it means.
pub struct PopupAction {
    /// Button label.
    pub label: String,
    /// Which overlay item this action targets.
    pub target: Arc<dyn OverlayItem>,
    /// The kind of action.
    pub kind: PopupActionKind,
}

/// What a popup action button does when clicked.
pub enum PopupActionKind {
    /// Hide this item from the map (NWS alerts).
    HideFromMap,
}
