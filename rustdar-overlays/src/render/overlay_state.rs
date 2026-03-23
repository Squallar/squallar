use std::any::Any;
use std::future::Future;
use std::pin::Pin;

use rustdar_units::UserPreferences;

use crate::nws::alert::AlertCategory;
use crate::render::layers::LayerManager;
use crate::spc::outlook::{OutlookDay, OutlookProduct};
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

/// Identifies a clicked overlay item for the detail popup pager.
#[derive(Clone, Debug)]
pub enum SelectedOverlay {
    /// An NWS alert, identified by its stable API ID string.
    Alert(String),
    /// An SPC Mesoscale Discussion, identified by its stable MD number.
    Discussion(u32),
    /// An SPC convective outlook feature, identified by its short label.
    Outlook { label: String },
    /// An SPC storm report, identified by its index in the reports list.
    StormReport { index: usize },
}

/// An overlay item that can be clicked and optionally labelled on the map.
///
/// Returned by `OverlayKind::clickable_items()` so that the UI crate can
/// perform hit-testing and label drawing without knowing overlay-specific types.
pub struct ClickableItem<'a> {
    /// Renderable polygon features for hit-testing.
    pub features: Vec<&'a OverlayFeature>,
    /// Optional map label to draw at a geographic position.
    pub label: Option<OverlayLabel>,
    /// The stable identifier to store when the user clicks this item.
    pub id: SelectedOverlay,
}

// ── Overlay handler trait ─────────────────────────────────────────────────

/// Trait for overlay type handlers. Each fetchable overlay type implements this
/// to encapsulate its data, fetch logic, render logic, and popup content.
///
/// Adding a new overlay only requires implementing this trait in a new handler
/// struct and registering it in the `OverlayRegistry` constructor.
pub(crate) trait OverlayHandler {
    /// Which overlay kind this handler manages.
    fn kind(&self) -> OverlayKind;

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

    /// Build clickable/labelled items for hit-testing on the map.
    fn clickable_items(&self, layers: &LayerManager) -> Vec<ClickableItem<'_>>;

    /// Build popup content for a selected overlay item.
    /// Returns `None` if this handler doesn't own the given selection.
    fn popup_content(&self, selected: &SelectedOverlay, prefs: &UserPreferences) -> Option<PopupContent>;

    /// Handle a popup action button. Returns `true` if the item should be removed.
    fn handle_popup_action(&mut self, _action: &PopupAction) -> bool { false }

    /// Apply a type-erased fetch result. The handler downcasts to its expected type.
    fn apply_fetch_result(&mut self, result: Box<dyn Any + Send>);

    /// Remove stale selections that no longer exist in the handler's data.
    fn retain_selections(&self, selections: &mut Vec<SelectedOverlay>);

    /// Prepare a rasterization closure for background rendering.
    /// Returns `None` if there's nothing to render.
    fn prepare_rasterize(
        &self,
        ctx: &RasterizeContext,
    ) -> Option<Box<dyn FnOnce(&GeoBounds, u32, u32) -> RasterizeOutput + Send>>;

    /// Create async fetch tasks for this overlay's data.
    fn create_fetch_tasks(&self, ctx: &FetchConfig) -> Vec<FetchTask>;
}

/// Context for creating overlay fetch tasks.
pub struct FetchConfig {
    pub client: reqwest::Client,
    pub zone_cache_dir: Option<std::path::PathBuf>,
    /// Currently selected SPC outlook day.
    pub spc_day: OutlookDay,
    /// SPC outlook products whose layers are enabled.
    pub spc_products: Vec<OutlookProduct>,
}

/// Context for preparing overlay rasterization closures.
pub struct RasterizeContext {
    /// Whether the app is in dark theme.
    pub is_dark: bool,
    /// Current map zoom level.
    pub zoom: f64,
    /// Selected SPC outlook day.
    pub spc_day: OutlookDay,
    /// Enabled SPC outlook products for the current day.
    pub enabled_spc_products: Vec<OutlookProduct>,
    /// NWS alert categories currently enabled.
    pub enabled_nws_categories: Vec<AlertCategory>,
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
    pub selected_overlays: Vec<SelectedOverlay>,
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

    pub fn data_generation(&self, kind: OverlayKind) -> u64 {
        self.handler(kind).map_or(0, |h| h.data_generation())
    }

    pub fn has_data(&self, kind: OverlayKind) -> bool {
        self.handler(kind).map_or(
            matches!(kind, OverlayKind::Radar | OverlayKind::RadarSites),
            |h| h.has_data(),
        )
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

    pub fn clickable_items(&self, kind: OverlayKind, layers: &LayerManager) -> Vec<ClickableItem<'_>> {
        self.handler(kind).map_or_else(Vec::new, |h| h.clickable_items(layers))
    }

    /// Build popup content by asking each handler until one claims the selection.
    pub fn popup_content(&self, selected: &SelectedOverlay, prefs: &UserPreferences) -> Option<PopupContent> {
        self.handlers.iter().find_map(|h| h.popup_content(selected, prefs))
    }

    /// Execute a popup action by routing to each handler.
    pub fn handle_popup_action(&mut self, action: &PopupAction) -> bool {
        for handler in &mut self.handlers {
            if handler.handle_popup_action(action) {
                return true;
            }
        }
        false
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
            OverlayKind::CityLabels,
            OverlayKind::RadarSites,
            OverlayKind::UserLocation,
        ]
    }

    /// Only the overlay kinds that get rasterized to background textures.
    pub const fn texture_overlays() -> &'static [OverlayKind] {
        &[
            OverlayKind::SpcOutlook,
            OverlayKind::SpcDiscussions,
            OverlayKind::NwsAlerts,
            OverlayKind::StormReports,
            OverlayKind::RadarSites,
            OverlayKind::Radar,
        ]
    }

    /// Whether this kind is a background-rasterized texture overlay.
    pub fn is_texture_overlay(self) -> bool {
        matches!(self, OverlayKind::SpcOutlook | OverlayKind::SpcDiscussions | OverlayKind::NwsAlerts | OverlayKind::StormReports | OverlayKind::RadarSites | OverlayKind::Radar)
    }

    /// Default draw order (bottom to top) for a new pane.
    pub fn default_draw_order() -> Vec<OverlayKind> {
        Self::all().to_vec()
    }

    /// Whether the relevant layer(s) for this kind are enabled.
    pub fn is_enabled(self, layers: &super::layers::LayerManager) -> bool {
        use super::layers::LayerKind;
        match self {
            OverlayKind::SpcOutlook => layers
                .spc_layers_for_day()
                .iter()
                .any(|lk| layers.is_enabled(*lk)),
            OverlayKind::SpcDiscussions => {
                layers.is_enabled(LayerKind::SpcMesoscaleDiscussions)
            }
            OverlayKind::NwsAlerts => layers.any_nws_enabled(),
            OverlayKind::StormReports => layers.is_enabled(LayerKind::StormReports),
            OverlayKind::Radar => layers.is_enabled(LayerKind::Radar),
            OverlayKind::CityLabels => layers.is_enabled(LayerKind::CityLabels),
            OverlayKind::RadarSites => layers.is_enabled(LayerKind::RadarSites),
            OverlayKind::UserLocation => true,
        }
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
    pub target: SelectedOverlay,
    /// The kind of action.
    pub kind: PopupActionKind,
}

/// What a popup action button does when clicked.
pub enum PopupActionKind {
    /// Hide this item from the map (NWS alerts).
    HideFromMap,
}
