use crate::actions::{GuiAction, RadarConfig};
use crate::layers::{LayerKind, LayerManager};
use crate::overlay_cache::{
    CachedFeature, OverlayLayerCache, ViewportKey,
    build_cached_features, draw_cached_features,
};
use crate::pane::{PaneId, PaneLayout, PaneState, MAX_PANES_DESKTOP, MAX_PANES_MOBILE};
use chrono::Timelike;
use egui::Context;
use std::collections::HashMap;
use std::sync::Arc;
use walkers::{HttpTiles, Texture, Tiles, sources::{TileSource, Attribution}, TileId};
use rustdar_radar::render::{ImageBounds, RadarProduct, ScanInfo, IMAGE_SIZE, MAX_RANGE_KM};
use rustdar_radar::sites::RADARS;
use rustdar_overlays::spc::outlook::{OutlookDay, OutlookProduct, SpcOutlook};
use rustdar_overlays::spc::discussion::SpcDiscussion;
use rustdar_overlays::spc::colors::{md_fill_color, md_stroke_color};
use rustdar_overlays::nws::alert::NwsAlert;
use rustdar_overlays::types::HatchPattern;

// ---------------------------------------------------------------------------
// Double-tap-drag zoom gesture detector (Android touch devices)
// ---------------------------------------------------------------------------

/// Detects a "double-tap and drag" gesture commonly used on touch devices
/// for one-handed zooming. The gesture flow is:
/// 1. Tap (short press-release)
/// 2. Within 400 ms, press down again and hold
/// 3. Drag vertically: up = zoom in, down = zoom out
#[cfg(target_os = "android")]
#[derive(Clone)]
struct DoubleTapDragDetector {
    /// Time of the last completed tap (short press-release)
    last_tap_time: Option<f64>,
    /// Screen position of the last completed tap
    last_tap_pos: Option<egui::Pos2>,
    /// Whether we are currently in a zoom-drag gesture
    zooming: bool,
    /// Starting Y position when zoom-drag began
    drag_start_y: f32,
    /// Map zoom level when zoom-drag began
    initial_zoom: f64,
    /// Time when the current/last primary press started
    press_time: f64,
    /// Position where the current/last primary press started
    press_pos: egui::Pos2,
}

#[cfg(target_os = "android")]
impl Default for DoubleTapDragDetector {
    fn default() -> Self {
        Self {
            last_tap_time: None,
            last_tap_pos: None,
            zooming: false,
            drag_start_y: 0.0,
            initial_zoom: 4.0,
            press_time: 0.0,
            press_pos: egui::Pos2::ZERO,
        }
    }
}

#[cfg(target_os = "android")]
impl DoubleTapDragDetector {
    /// Process this frame's input and update the map zoom if a
    /// double-tap-drag gesture is active.
    fn update(&mut self, ctx: &egui::Context, map_memory: &mut MapMemory) {
        let (pressed, released, down, pos, time) = ctx.input(|i| {
            (
                i.pointer.primary_pressed(),
                i.pointer.primary_released(),
                i.pointer.primary_down(),
                i.pointer.interact_pos(),
                i.time,
            )
        });
        let pos = pos.unwrap_or(egui::Pos2::ZERO);

        if self.zooming {
            if !down {
                self.zooming = false;
            } else {
                // Drag up (negative dy) = zoom in, drag down = zoom out
                let dy = pos.y - self.drag_start_y;
                let zoom_delta = dy as f64 / 150.0;
                let new_zoom = (self.initial_zoom + zoom_delta).clamp(1.0, 19.0);
                let _ = map_memory.set_zoom(new_zoom);
            }
            return;
        }

        if pressed {
            // Check if this is the second tap of a double-tap
            if let (Some(last_time), Some(last_pos)) = (self.last_tap_time, self.last_tap_pos) {
                let dt = time - last_time;
                let dist = (pos - last_pos).length();
                if dt < 0.4 && dist < 50.0 {
                    // Double-tap detected — enter zoom-drag mode
                    self.zooming = true;
                    self.drag_start_y = pos.y;
                    self.initial_zoom = map_memory.zoom();
                    self.last_tap_time = None;
                    self.last_tap_pos = None;
                    return;
                }
            }
            self.press_time = time;
            self.press_pos = pos;
        }

        if released {
            // Classify as a "tap" only if the press was short and didn't move far
            let duration = time - self.press_time;
            let distance = (pos - self.press_pos).length();
            if duration < 0.3 && distance < 20.0 {
                self.last_tap_time = Some(time);
                self.last_tap_pos = Some(pos);
            } else {
                // Long press or drag — not a tap
                self.last_tap_time = None;
                self.last_tap_pos = None;
            }
        }
    }

    /// Whether a zoom-drag gesture is currently active.
    fn is_zooming(&self) -> bool {
        self.zooming
    }
}

/// CartoDB tile source variants.
/// Base maps use `nolabels` so city/road names are not obscured by the radar
/// overlay. A separate `labels-only` layer is drawn on top of the radar.
#[derive(Clone)]
pub enum CartoDbStyle {
    LightNoLabels,
    DarkNoLabels,
    LightLabelsOnly,
    DarkLabelsOnly,
}

#[derive(Clone)]
pub struct CartoDb {
    style: CartoDbStyle,
}

impl CartoDb {
    pub fn light() -> Self {
        Self { style: CartoDbStyle::LightNoLabels }
    }

    pub fn dark() -> Self {
        Self { style: CartoDbStyle::DarkNoLabels }
    }

    pub fn light_labels() -> Self {
        Self { style: CartoDbStyle::LightLabelsOnly }
    }

    pub fn dark_labels() -> Self {
        Self { style: CartoDbStyle::DarkLabelsOnly }
    }
}

impl TileSource for CartoDb {
    fn tile_url(&self, tile_id: TileId) -> String {
        let style_name = match self.style {
            CartoDbStyle::LightNoLabels => "light_nolabels",
            CartoDbStyle::DarkNoLabels => "dark_nolabels",
            CartoDbStyle::LightLabelsOnly => "light_only_labels",
            CartoDbStyle::DarkLabelsOnly => "dark_only_labels",
        };
        
        // Use one of the available subdomains (a, b, c, d)
        let subdomain = match tile_id.x % 4 {
            0 => "a",
            1 => "b", 
            2 => "c",
            _ => "d",
        };
        
        format!(
            "https://cartodb-basemaps-{}.global.ssl.fastly.net/{}/{}/{}/{}.png",
            subdomain, style_name, tile_id.zoom, tile_id.x, tile_id.y
        )
    }

    fn attribution(&self) -> Attribution {
        Attribution {
            text: "© OpenStreetMap © CartoDB",
            url: "https://www.openstreetmap.org/copyright",
            logo_light: None,
            logo_dark: None,
        }
    }
}

pub struct Gui {
    radar_config: RadarConfig,
    scan_info: Option<ScanInfo>,
    fetching: bool,
    error_message: Option<String>,
    // Temporary strings for editing date/time components
    date_string: String,
    time_string: String,
    // Auto-polling state
    last_fetch_time: Option<std::time::Instant>,
    auto_poll_enabled: bool,
    initial_fetch_done: bool,
    initial_zoom_set: bool, // Track if we've already set the initial zoom
    // Map tile state (shared across all panes)
    tiles_light: Option<HttpTiles>,
    tiles_dark: Option<HttpTiles>,
    current_theme_is_dark: bool, // Track current theme
    // City / road label overlay (shared)
    label_tiles_light: Option<HttpTiles>,
    label_tiles_dark: Option<HttpTiles>,
    // Track which site is currently loading
    loading_site: Option<String>,
    // Time dialog state
    show_time_dialog: bool,
    // Auto-poll interval in seconds (increases on failure, resets on success)
    poll_interval_secs: u64,
    // User's GPS location for blue dot indicator (lat, lon)
    user_location: Option<(f64, f64)>,
    // SPC convective outlook data cache (shared — same weather data for all panes)
    spc_outlooks: HashMap<(OutlookDay, OutlookProduct), SpcOutlook>,
    // Track when each SPC product was last fetched
    spc_fetch_times: HashMap<(OutlookDay, OutlookProduct), std::time::Instant>,
    // Whether an SPC fetch is currently in flight
    spc_fetching: bool,
    // NWS weather alerts state (shared)
    nws_alerts: Vec<NwsAlert>,
    nws_fetch_time: Option<std::time::Instant>,
    nws_fetching: bool,
    /// Index of the currently selected alert for detail popup.
    selected_alert: Option<usize>,
    // SPC Mesoscale Discussions state (shared)
    spc_discussions: Vec<SpcDiscussion>,
    spc_md_fetch_time: Option<std::time::Instant>,
    spc_md_fetching: bool,
    /// Index of the currently selected MD for detail popup.
    selected_md: Option<usize>,
    /// Data generation counters — bumped when source data is replaced (shared).
    spc_data_generation: HashMap<(OutlookDay, OutlookProduct), u64>,
    nws_data_generation: u64,
    spc_md_data_generation: u64,
    // Multi-pane state
    panes: Vec<PaneState>,
    active_pane: PaneId,
    pane_layout: PaneLayout,
    viewport_sync: bool,
    sync_layers: bool,
    // Whether the mobile slide-out menu is open (Android only)
    #[cfg(target_os = "android")]
    show_mobile_menu: bool,
    // Double-tap-drag zoom gesture detector (Android only)
    #[cfg(target_os = "android")]
    double_tap_detector: DoubleTapDragDetector,
    // Safe area insets in logical pixels (top, bottom, left, right)
    // Used on Android to avoid drawing under system bars.
    #[cfg(target_os = "android")]
    safe_area_insets: (f32, f32, f32, f32),
}

impl Default for Gui {
    fn default() -> Self {
        Self::new()
    }
}

impl Gui {
    pub fn new() -> Self {
        let radar_config = RadarConfig::default();
        let date_string = radar_config.timestamp.format("%Y-%m-%d").to_string();
        let time_string = radar_config.timestamp.format("%H:%M:%S").to_string();

        Self {
            radar_config,
            scan_info: None,
            fetching: false,
            error_message: None,
            date_string,
            time_string,
            last_fetch_time: None,
            auto_poll_enabled: true,
            initial_fetch_done: false,
            initial_zoom_set: false,
            tiles_light: None,
            tiles_dark: None,
            current_theme_is_dark: true, // Default to dark theme
            label_tiles_light: None,
            label_tiles_dark: None,
            loading_site: None,
            show_time_dialog: false,
            poll_interval_secs: 60,
            user_location: None,
            spc_outlooks: HashMap::new(),
            spc_fetch_times: HashMap::new(),
            spc_fetching: false,
            nws_alerts: Vec::new(),
            nws_fetch_time: None,
            nws_fetching: false,
            selected_alert: None,
            spc_discussions: Vec::new(),
            spc_md_fetch_time: None,
            spc_md_fetching: false,
            selected_md: None,
            spc_data_generation: HashMap::new(),
            nws_data_generation: 0,
            spc_md_data_generation: 0,
            panes: vec![PaneState::new()],
            active_pane: 0,
            pane_layout: PaneLayout::default(),
            viewport_sync: true,
            sync_layers: true,
            #[cfg(target_os = "android")]
            show_mobile_menu: false,
            #[cfg(target_os = "android")]
            double_tap_detector: DoubleTapDragDetector::default(),
            #[cfg(target_os = "android")]
            safe_area_insets: (0.0, 0.0, 0.0, 0.0),
        }
    }

    /// Create the UI using egui.
    pub fn ui(&mut self, ctx: &egui::Context) -> Vec<GuiAction> {
        let mut actions = Vec::new();

        // Auto-fetch on first load
        if !self.initial_fetch_done && !self.fetching {
            self.fetching = true;
            self.initial_fetch_done = true;
            self.last_fetch_time = Some(std::time::Instant::now());
            actions.push(GuiAction::FetchRadarScan(self.radar_config.clone()));
        }

        // Poll for new scans at the current poll interval
        if self.auto_poll_enabled
            && !self.fetching
            && let Some(last_fetch) = self.last_fetch_time
            && last_fetch.elapsed().as_secs() >= self.poll_interval_secs
        {
            // Check for new files without downloading
            let now = chrono::Local::now().naive_local();
            let current_scan_time = now
                .with_second(0)
                .and_then(|t| t.with_nanosecond(0))
                .unwrap_or(now);

            // Use current time for the check request
            let mut config = self.radar_config.clone();
            config.timestamp = current_scan_time;
            actions.push(GuiAction::CheckForNewScans(config));

            // Reset timer to avoid spamming checks
            self.last_fetch_time = Some(std::time::Instant::now());
        }

        // Auto-refresh NWS alerts every 2 minutes when any pane has an NWS layer enabled
        if self.panes.iter().any(|p| p.layers.any_nws_enabled())
            && !self.nws_fetching
            && self
                .nws_fetch_time
                .map_or(true, |t| t.elapsed().as_secs() >= 120)
        {
            actions.push(GuiAction::FetchNwsAlerts);
        }

        // Auto-refresh SPC Mesoscale Discussions every 2 minutes when any pane has it enabled
        if self.panes.iter().any(|p| p.layers.is_enabled(LayerKind::SpcMesoscaleDiscussions))
            && !self.spc_md_fetching
            && self
                .spc_md_fetch_time
                .map_or(true, |t| t.elapsed().as_secs() >= 120)
        {
            actions.push(GuiAction::FetchSpcDiscussions);
        }

        #[cfg(target_os = "android")]
        self.render_mobile_ui(ctx, &mut actions);
        #[cfg(not(target_os = "android"))]
        self.render_desktop_ui(ctx, &mut actions);

        actions
    }

    /// Update the scan info (called from the app when scan is loaded)
    pub fn set_scan_info(&mut self, info: ScanInfo) {
        self.scan_info = Some(info);
        self.fetching = false;
        // Reset poll interval on success
        self.poll_interval_secs = 60;

        // Only zoom to radar on the first scan load to avoid disrupting user navigation
        if !self.initial_zoom_set {
            for pane in &mut self.panes {
                let _ = pane.map_memory.set_zoom(7.0);
            }
            self.initial_zoom_set = true;
        }
    }

    /// Set fetching status
    pub fn set_fetching(&mut self, fetching: bool) {
        self.fetching = fetching;
    }

    /// Set an error message
    pub fn set_error(&mut self, error: String) {
        self.error_message = Some(error);
        self.fetching = false;
        // Exponential backoff: double poll interval on failure, cap at 5 minutes
        self.poll_interval_secs = (self.poll_interval_secs * 2).min(300);
    }

    #[cfg(not(target_os = "android"))]
    fn render_menu_bar(&mut self, ctx: &Context, action: &mut Option<GuiAction>) {
        egui::TopBottomPanel::top("menubar_container").show(ctx, |ui| {
            egui::MenuBar::new().ui(ui, |ui| {
                ui.menu_button("File", |ui| {
                    if ui.button("Exit").clicked() {
                        *action = Some(GuiAction::Exit);
                        ui.close_kind(egui::UiKind::Menu);
                    }
                });

                ui.menu_button("View", |ui| {
                    ui.checkbox(self.panes[self.active_pane].layers.enabled_mut(LayerKind::RadarSites), "Show radar sites");
                    ui.checkbox(self.panes[self.active_pane].layers.enabled_mut(LayerKind::CityLabels), "Show city labels");
                    ui.separator();
                    if ui.button("Time...").clicked() {
                        self.show_time_dialog = true;
                        ui.close_kind(egui::UiKind::Menu);
                    }
                });
            });
        });
    }

    fn render_time_dialog(&mut self, ctx: &Context) -> Option<GuiAction> {
        let mut action = None;
        
        if self.show_time_dialog {
            egui::Window::new("Set Time")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
                .show(ctx, |ui| {
                    ui.vertical_centered(|ui| {
                        ui.heading("Select Time");
                        ui.add_space(10.0);
                        
                        ui.label("Date:");
                        ui.text_edit_singleline(&mut self.date_string);
                        
                        ui.add_space(5.0);
                        
                        ui.label("Time:");
                        ui.text_edit_singleline(&mut self.time_string);
                        
                        ui.add_space(10.0);
                        
                        if ui.button("Use Current Time").clicked() {
                            self.radar_config.timestamp = chrono::Local::now().naive_local();
                            self.date_string = self.radar_config.timestamp.format("%Y-%m-%d").to_string();
                            self.time_string = self.radar_config.timestamp.format("%H:%M:%S").to_string();
                        }
                        
                        ui.add_space(15.0);
                        
                        ui.horizontal(|ui| {
                            if ui.button("OK").clicked() {
                                // Try to parse the date and time strings
                                let datetime_str = format!("{} {}", self.date_string, self.time_string);
                                if let Ok(timestamp) = chrono::NaiveDateTime::parse_from_str(&datetime_str, "%Y-%m-%d %H:%M:%S") {
                                    self.radar_config.timestamp = timestamp;
                                    action = Some(GuiAction::FetchRadarScan(self.radar_config.clone()));
                                }
                                self.show_time_dialog = false;
                            }
                            
                            if ui.button("Cancel").clicked() {
                                // Restore the original strings from the current config
                                self.date_string = self.radar_config.timestamp.format("%Y-%m-%d").to_string();
                                self.time_string = self.radar_config.timestamp.format("%H:%M:%S").to_string();
                                self.show_time_dialog = false;
                            }
                        });
                    });
                });
        }
        
        action
    }

    #[cfg(not(target_os = "android"))]
    fn render_status_bar(&mut self, ctx: &Context) -> Option<GuiAction> {
        let mut action = None;
        
        egui::TopBottomPanel::bottom("status_bar")
            .show_separator_line(true)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 8.0;

                    // Refresh button
                    let refresh_button = ui.add_enabled(
                        !self.fetching,
                        egui::Button::new("🔄").frame(false)
                    );
                    if refresh_button.clicked() {
                        action = Some(GuiAction::FetchRadarScan(self.radar_config.clone()));
                    }
                    refresh_button.on_hover_text("Refresh radar data");

                    ui.separator();

                    // Unified auto-poll checkbox and status
                    if self.fetching {
                        ui.label("🔄");
                        ui.label("Downloading");
                        ui.spinner();
                    } else if self.auto_poll_enabled {
                        // Show time until next poll with checkbox
                        if let Some(last_fetch) = self.last_fetch_time {
                            let elapsed = last_fetch.elapsed().as_secs();
                            let remaining = self.poll_interval_secs.saturating_sub(elapsed);
                            ui.checkbox(&mut self.auto_poll_enabled, &format!("Auto-poll (next in {}s)", remaining));
                        } else {
                            ui.checkbox(&mut self.auto_poll_enabled, "Auto-poll");
                        }
                    } else {
                        ui.checkbox(&mut self.auto_poll_enabled, "Auto-poll");
                    }

                    ui.separator();

                    // Scan information
                    if let Some(scan_info) = &self.scan_info {
                        ui.label(format!(
                            "Scan: {} @ {} UTC ({} products)",
                            scan_info.site.name,
                            scan_info.timestamp.format("%Y-%m-%d %H:%M:%S"),
                            scan_info.available_products.len()
                        ));
                    } else {
                        ui.label("No scan loaded");
                    }

                    ui.separator();

                    // Hover information - show from whichever pane has data
                    let hover_info = self.panes.iter()
                        .find_map(|p| p.hover_value.as_ref());
                    if let Some(hover_info) = hover_info {
                        ui.label("📍");
                        ui.label(hover_info);
                    } else {
                        // Add empty space when no hover info
                        ui.label("");
                    }

                    // Add flexible space to push error to the right
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        // Error message (if any)
                        let mut dismiss_error = false;
                        if let Some(error_msg) = &self.error_message {
                            if ui.button("✕").clicked() {
                                dismiss_error = true;
                            }
                            ui.label(error_msg.as_str());
                            ui.label("❌");
                        }
                        if dismiss_error { self.error_message = None; }
                    });
                });
            });
        
        action
    }

    fn render_map(&mut self, ctx: &Context) -> Vec<GuiAction> {
        use walkers::{Map, Position};

        let mut actions = Vec::new();

        // Detect current theme from egui context
        let is_dark_theme = ctx.style().visuals.dark_mode;
        self.current_theme_is_dark = is_dark_theme;

        // Lazily initialize tiles for each theme variant.
        // Keeping both avoids discarding the tile cache on every theme toggle.
        if is_dark_theme {
            if self.tiles_dark.is_none() {
                self.tiles_dark = Some(HttpTiles::new(CartoDb::dark(), ctx.to_owned()));
            }
        } else if self.tiles_light.is_none() {
            self.tiles_light = Some(HttpTiles::new(CartoDb::light(), ctx.to_owned()));
        }

        // Lazily initialize label-only tiles if any pane uses city labels.
        let any_city_labels = self.panes.iter().any(|p| p.layers.is_enabled(LayerKind::CityLabels));
        if any_city_labels {
            if is_dark_theme && self.label_tiles_dark.is_none() {
                self.label_tiles_dark = Some(HttpTiles::new(CartoDb::dark_labels(), ctx.to_owned()));
            } else if !is_dark_theme && self.label_tiles_light.is_none() {
                self.label_tiles_light = Some(HttpTiles::new(CartoDb::light_labels(), ctx.to_owned()));
            }
        }

        // Take tiles out of self so they can be reborrowed per-pane in the loop.
        let mut tiles_owned = if is_dark_theme {
            self.tiles_dark.take()
        } else {
            self.tiles_light.take()
        };

        let took_dark_labels = any_city_labels && is_dark_theme;
        let took_light_labels = any_city_labels && !is_dark_theme;
        let mut label_tiles: Option<HttpTiles> = if took_dark_labels {
            self.label_tiles_dark.take()
        } else if took_light_labels {
            self.label_tiles_light.take()
        } else {
            None
        };

        let pane_count = self.pane_layout.pane_count;

        egui::CentralPanel::default()
            .frame(egui::Frame::NONE)
            .show(ctx, |ui| {
                let panel_rect = ui.max_rect();

                // Activate pane on primary press (before rendering so drag/pan
                // happens on the newly-active pane in the same frame).
                if pane_count > 1 {
                    if let Some(pos) = ui.ctx().input(|i| {
                        if i.pointer.primary_pressed() {
                            i.pointer.interact_pos()
                        } else {
                            None
                        }
                    }) {
                        for idx in 0..pane_count {
                            let rect = self.pane_layout.pane_rect(idx, panel_rect);
                            if rect.contains(pos) && idx != self.active_pane {
                                self.active_pane = idx;
                                break;
                            }
                        }
                    }
                }

                // Snapshot viewport state before rendering for sync detection
                let (pre_zooms, pre_positions): (Vec<f64>, Vec<Option<Position>>) =
                    if self.viewport_sync && pane_count > 1 {
                        self.panes.iter().take(pane_count)
                            .map(|p| (p.map_memory.zoom(), p.map_memory.detached()))
                            .unzip()
                    } else {
                        (vec![], vec![])
                    };

                // Dismiss overlay popups when clicking outside them (once, not per-pane)
                let pointer_available = self.selected_alert.is_none()
                    && self.selected_md.is_none();
                if !pointer_available {
                    let click_pos = ctx.input(|i| {
                        if i.pointer.any_click() {
                            i.pointer.interact_pos()
                        } else {
                            None
                        }
                    });
                    if let Some(pos) = click_pos {
                        let on_popup = ctx.layer_id_at(pos)
                            .is_some_and(|l| l.order > egui::Order::Background);
                        if !on_popup {
                            self.selected_alert = None;
                            self.selected_md = None;
                        }
                    }
                }

                for pane_idx in 0..pane_count {
                    let pane_rect = self.pane_layout.pane_rect(pane_idx, panel_rect);
                    let is_active = pane_idx == self.active_pane;

                    let mut pane = std::mem::take(&mut self.panes[pane_idx]);

                    // Determine the map center
                    let center = if let Some(scan_info) = &self.scan_info {
                        Position::new(scan_info.site.lon, scan_info.site.lat)
                    } else {
                        Position::new(-98.5795, 39.8283) // Geographic center of contiguous USA
                    };

                    // Clone radar image data for use in closure
                    let radar_image = pane.radar_image.clone();

                    // Clone user location for use in closure
                    let user_location = self.user_location;

                    let show_city_labels = pane.layers.is_enabled(LayerKind::CityLabels);

                    // On Android, process double-tap-drag zoom only for the active pane
                    #[cfg(target_os = "android")]
                    if is_active {
                        self.double_tap_detector.update(ctx, &mut pane.map_memory);
                    }

                    #[cfg(target_os = "android")]
                    let is_zoom_dragging = if is_active {
                        self.double_tap_detector.is_zooming()
                    } else {
                        false
                    };
                    #[cfg(not(target_os = "android"))]
                    let is_zoom_dragging = false;

                    // Create a child UI constrained to this pane's rect
                    let mut child_ui = ui.new_child(
                        egui::UiBuilder::new()
                            .max_rect(pane_rect)
                            .id_salt(("pane_map", pane_idx)),
                    );
                    child_ui.set_clip_rect(pane_rect);

                    if let Some(tiles) = tiles_owned.as_mut() {
                        Map::new(None, &mut pane.map_memory, center)
                        .with_layer(tiles, 1.0)
                        .zoom_with_ctrl(false)
                        .panning(false)
                        .drag_pan_buttons(if is_zoom_dragging {
                            egui::DragPanButtons::empty()
                        } else {
                            egui::DragPanButtons::PRIMARY
                        })
                        .show(&mut child_ui, |ui, projector, memory| {
                            let zoom = memory.zoom();

                            // Draw SPC outlook polygons (below radar)
                            draw_spc_overlays(
                                ui,
                                projector,
                                zoom,
                                &pane.layers,
                                &self.spc_outlooks,
                                self.current_theme_is_dark,
                                &mut pane.spc_overlay_caches,
                                &self.spc_data_generation,
                            );

                            // Overlay radar data if available
                            if pane.layers.is_enabled(LayerKind::Radar) {
                            if let Some((ref texture, lat, lon, _max_range_km, ref value_data)) =
                                radar_image
                            {
                                let bounds = pane.cached_image_bounds
                                    .unwrap_or_else(|| ImageBounds::from_radar_site(lat, lon));

                                let nw = projector.project(walkers::lat_lon(bounds.max_lat, bounds.min_lon)).to_pos2();
                                let se = projector.project(walkers::lat_lon(bounds.min_lat, bounds.max_lon)).to_pos2();
                                let rect = egui::Rect::from_two_pos(nw, se);

                                // Hover: only compute for the pane the cursor is in
                                if let Some(hover_pos) = ui.ctx().pointer_hover_pos() {
                                    if pane_rect.contains(hover_pos) {
                                    let pos_changed = pane.last_hover_pos
                                        .map(|last| (last - hover_pos).length() > 0.5)
                                        .unwrap_or(true);
                                    pane.last_hover_pos = Some(hover_pos);

                                    if pos_changed {
                                        let screen_vec = egui::vec2(hover_pos.x, hover_pos.y);
                                        let map_pos = projector.unproject(screen_vec);
                                        let hover_lat = map_pos.y();
                                        let hover_lon = map_pos.x();

                                        let lat1 = lat.to_radians();
                                        let lon1 = lon.to_radians();
                                        let lat2 = hover_lat.to_radians();
                                        let lon2 = hover_lon.to_radians();
                                        let dlat = lat2 - lat1;
                                        let dlon = lon2 - lon1;
                                        let a = (dlat / 2.0).sin().powi(2)
                                            + lat1.cos() * lat2.cos() * (dlon / 2.0).sin().powi(2);
                                        let c = 2.0 * a.sqrt().atan2((1.0 - a).sqrt());
                                        let distance_km = 6371.0 * c;

                                        let y = dlon.sin() * lat2.cos();
                                        let x = lat1.cos() * lat2.sin()
                                            - lat1.sin() * lat2.cos() * dlon.cos();
                                        let azimuth = (y.atan2(x).to_degrees() + 360.0) % 360.0;

                                        let mut value_str = String::new();
                                        let frac_x = (hover_pos.x - rect.left()) / rect.width();
                                        let frac_y = (hover_pos.y - rect.top()) / rect.height();
                                        let px = (frac_x * IMAGE_SIZE as f32) as i32;
                                        let py = (frac_y * IMAGE_SIZE as f32) as i32;

                                        if px >= 0
                                            && px < IMAGE_SIZE as i32
                                            && py >= 0
                                            && py < IMAGE_SIZE as i32
                                        {
                                            let pixel_idx = py as usize * IMAGE_SIZE + px as usize;
                                            if pixel_idx < value_data.len() {
                                                let value = value_data[pixel_idx];
                                                if !value.is_nan() {
                                                    let product = pane.selected_product;
                                                    value_str = match product {
                                                        RadarProduct::Reflectivity => {
                                                            format!(" | Reflectivity: {:.1} dBZ", value)
                                                        }
                                                        RadarProduct::Velocity
                                                        | RadarProduct::StormRelativeVelocity => {
                                                            let mph = value * 2.23694;
                                                            format!(" | {}: {:.1} mph", product.name(), mph)
                                                        }
                                                        RadarProduct::SpectrumWidth => {
                                                            let mph = value * 2.23694;
                                                            format!(" | Spectrum Width: {:.1} mph", mph)
                                                        }
                                                        RadarProduct::DifferentialReflectivity => {
                                                            format!(" | Diff. Reflectivity: {:.2} dB", value)
                                                        }
                                                        RadarProduct::CorrelationCoefficient => {
                                                            format!(" | Corr. Coefficient: {:.4}", value)
                                                        }
                                                        RadarProduct::DifferentialPhase => {
                                                            format!(" | Diff. Phase: {:.1}°", value)
                                                        }
                                                        RadarProduct::SpecificDifferentialPhase => {
                                                            format!(" | KDP: {:.2} °/km", value)
                                                        }
                                                        RadarProduct::EchoTops => {
                                                            format!(" | Echo Tops: {:.1} kft", value)
                                                        }
                                                        RadarProduct::VerticallyIntegratedLiquid => {
                                                            format!(" | VIL: {:.1} kg/m²", value)
                                                        }
                                                        RadarProduct::HydrometeorClassification => {
                                                            let class = match value as u16 {
                                                                0..=9 => "No Data",
                                                                10..=19 => "Biological",
                                                                20..=29 => "Clutter/AP",
                                                                30..=39 => "Ice Crystals",
                                                                40..=49 => "Dry Snow",
                                                                50..=59 => "Wet Snow",
                                                                60..=69 => "Rain",
                                                                70..=79 => "Heavy Rain",
                                                                80..=89 => "Big Drops",
                                                                90..=99 => "Graupel",
                                                                100..=109 => "Hail+Rain",
                                                                110..=119 => "Large Hail",
                                                                120..=139 => "Giant Hail",
                                                                140..=149 => "Unknown",
                                                                150.. => "Range Folded",
                                                            };
                                                            format!(" | HHC: {class}")
                                                        }
                                                        RadarProduct::PrecipitationRate => {
                                                            format!(" | Precip Rate: {:.2} in/hr", value)
                                                        }
                                                        RadarProduct::NormalizedRotation => {
                                                            format!(" | NROT: {:.2}", value)
                                                        }
                                                    };
                                                }
                                            }
                                        }

                                        pane.hover_value = Some(format!(
                                            "Lat: {:.4}°, Lon: {:.4}° | Range: {:.1}km, Az: {:.1}° {}",
                                            hover_lat, hover_lon, distance_km, azimuth, value_str
                                        ));
                                    }
                                    } else {
                                        // Cursor not in this pane
                                        pane.last_hover_pos = None;
                                        pane.hover_value = None;
                                    }
                                } else {
                                    pane.last_hover_pos = None;
                                    pane.hover_value = None;
                                }

                                // Draw the radar image overlay
                                ui.painter().image(
                                    texture.id(),
                                    rect,
                                    egui::Rect::from_min_max(
                                        egui::pos2(0.0, 0.0),
                                        egui::pos2(1.0, 1.0),
                                    ),
                                    egui::Color32::WHITE,
                                );

                                // Draw a light grey circle showing the radar range
                                let radar_center = projector.project(walkers::lat_lon(lat, lon)).to_pos2();
                                let north_edge = projector.project(
                                    walkers::lat_lon(lat + MAX_RANGE_KM / 111.32, lon)
                                ).to_pos2();
                                let range_radius_pixels = (radar_center.y - north_edge.y).abs();
                                ui.painter().circle_stroke(
                                    radar_center,
                                    range_radius_pixels,
                                    egui::Stroke::new(1.0, egui::Color32::from_rgba_unmultiplied(150, 150, 150, 80)),
                                );
                            }
                            } // end if radar layer enabled

                            // Draw SPC Mesoscale Discussion polygons
                            let clicked_md = draw_spc_discussions(
                                ui,
                                projector,
                                zoom,
                                &pane.layers,
                                &self.spc_discussions,
                                &mut pane.spc_md_overlay_cache,
                                self.spc_md_data_generation,
                                pointer_available,
                            );
                            if let Some(idx) = clicked_md {
                                self.selected_md = Some(idx);
                            }

                            // Draw NWS alert polygons
                            let clicked_alert = draw_nws_alerts(
                                ui,
                                projector,
                                zoom,
                                &pane.layers,
                                &self.nws_alerts,
                                &mut pane.nws_overlay_cache,
                                self.nws_data_generation,
                                pointer_available,
                            );
                            if let Some(idx) = clicked_alert {
                                self.selected_alert = Some(idx);
                            }

                            // Draw label-only tiles on top of the radar overlay
                            if show_city_labels {
                                if let Some(ref mut ltiles) = label_tiles {
                                    draw_label_tiles_overlay(ui, projector, memory.zoom(), ltiles);
                                }
                            }

                            // Draw radar site icons
                            if pane.layers.is_enabled(LayerKind::RadarSites) {
                                for radar_site in &RADARS {
                                    let site_position = walkers::lat_lon(radar_site.lat, radar_site.lon);
                                    let site_screen = projector.project(site_position).to_pos2();

                                    let screen_rect = ui.max_rect();
                                    if !screen_rect.expand(100.0).contains(site_screen) {
                                        continue;
                                    }

                                    let zoom = memory.zoom() as f32;
                                    let icon_size = (10.0 + zoom * 2.0).clamp(8.0, 24.0);

                                    let is_current_site = self.scan_info.as_ref()
                                        .map(|info| info.site.name == radar_site.name)
                                        .unwrap_or(false);

                                    let is_loading = self.loading_site.as_ref()
                                        .map(|loading| loading == radar_site.name)
                                        .unwrap_or(false);

                                    let icon_color = if is_loading {
                                        egui::Color32::from_rgb(160, 32, 240)
                                    } else if is_current_site {
                                        egui::Color32::from_rgb(255, 100, 100)
                                    } else {
                                        egui::Color32::from_rgb(100, 150, 255)
                                    };

                                    let icon_rect = egui::Rect::from_center_size(
                                        site_screen,
                                        egui::vec2(icon_size, icon_size)
                                    );

                                    let response = ui.allocate_rect(icon_rect, egui::Sense::click());

                                    if response.clicked() {
                                        self.loading_site = Some(radar_site.name.to_string());
                                        actions.push(GuiAction::SwitchRadarSite(radar_site.name.to_string()));
                                    }

                                    ui.painter().circle_filled(site_screen, icon_size / 2.0, icon_color);

                                    ui.painter().circle_stroke(
                                        site_screen,
                                        icon_size / 2.0,
                                        egui::Stroke::new(1.5, egui::Color32::WHITE)
                                    );

                                    let text_color = if is_dark_theme {
                                        egui::Color32::WHITE
                                    } else {
                                        egui::Color32::BLACK
                                    };
                                    let font_size = (icon_size * 0.6).clamp(8.0, 12.0);

                                    let text_pos = egui::pos2(
                                        site_screen.x,
                                        site_screen.y + icon_size / 2.0 + 3.0,
                                    );

                                    ui.painter().text(
                                        text_pos,
                                        egui::Align2::CENTER_TOP,
                                        radar_site.name,
                                        egui::FontId::monospace(font_size),
                                        text_color,
                                    );

                                    if response.hovered() {
                                        let tooltip_text = if radar_site.elev == -99999 {
                                            format!("{}\nLat: {:.3}°, Lon: {:.3}°\nElev: N/A",
                                                radar_site.name, radar_site.lat, radar_site.lon)
                                        } else {
                                            format!("{}\nLat: {:.3}°, Lon: {:.3}°\nElev: {} ft",
                                                radar_site.name, radar_site.lat, radar_site.lon, radar_site.elev)
                                        };
                                        response.on_hover_text(tooltip_text);
                                    }
                                }
                            }

                            // Draw user location indicator (blue dot)
                            if let Some((user_lat, user_lon)) = user_location {
                                let user_screen = projector
                                    .project(walkers::lat_lon(user_lat, user_lon))
                                    .to_pos2();

                                let screen_rect = ui.max_rect();
                                if screen_rect.expand(50.0).contains(user_screen) {
                                    ui.painter().circle_filled(
                                        user_screen,
                                        14.0,
                                        egui::Color32::from_rgba_unmultiplied(30, 130, 255, 40),
                                    );
                                    ui.painter().circle_stroke(
                                        user_screen,
                                        7.0,
                                        egui::Stroke::new(2.5, egui::Color32::WHITE),
                                    );
                                    ui.painter().circle_filled(
                                        user_screen,
                                        7.0,
                                        egui::Color32::from_rgb(30, 130, 255),
                                    );
                                }
                            }
                        });
                    }

                    // Restore pane
                    self.panes[pane_idx] = pane;

                    // Draw pane border when multi-pane
                    if pane_count > 1 {
                        let border_color = if is_active {
                            egui::Color32::from_rgb(60, 140, 255)
                        } else {
                            egui::Color32::from_rgba_unmultiplied(128, 128, 128, 100)
                        };
                        let stroke_width = if is_active { 2.0 } else { 1.0 };
                        ui.painter().rect_stroke(
                            pane_rect,
                            0.0,
                            egui::Stroke::new(stroke_width, border_color),
                            egui::StrokeKind::Outside,
                        );
                    }
                } // end pane loop

                // Handle divider dragging on a foreground layer so they
                // take priority over map panning in the overlap zone.
                if pane_count > 1 {
                    let divider_layer = egui::LayerId::new(
                        egui::Order::Foreground,
                        egui::Id::new("pane_dividers"),
                    );
                    let mut divider_ui = ui.new_child(
                        egui::UiBuilder::new()
                            .max_rect(panel_rect)
                            .layer_id(divider_layer),
                    );
                    self.pane_layout.handle_dividers(&mut divider_ui, panel_rect);
                }

                // Sync viewports: propagate the interacted pane's viewport to all others
                if self.viewport_sync && pane_count > 1 {
                    // Detect which pane changed this frame
                    let mut source_idx = None;
                    for idx in 0..pane_count {
                        if idx < pre_zooms.len() {
                            let zoom_diff = (self.panes[idx].map_memory.zoom() - pre_zooms[idx]).abs();
                            if zoom_diff > 0.0001 {
                                source_idx = Some(idx);
                                break;
                            }
                            let prev_pos = &pre_positions[idx];
                            let curr_pos = self.panes[idx].map_memory.detached();
                            let pos_changed = match (prev_pos, &curr_pos) {
                                (Some(p1), Some(p2)) => {
                                    (p1.x() - p2.x()).abs() > 0.00001
                                        || (p1.y() - p2.y()).abs() > 0.00001
                                }
                                (None, Some(_)) | (Some(_), None) => true,
                                _ => false,
                            };
                            if pos_changed {
                                source_idx = Some(idx);
                                break;
                            }
                        }
                    }
                    let src = source_idx.unwrap_or(self.active_pane);
                    let zoom = self.panes[src].map_memory.zoom();
                    let pos = self.panes[src].map_memory.detached();
                    for idx in 0..pane_count {
                        if idx != src {
                            let _ = self.panes[idx].map_memory.set_zoom(zoom);
                            if let Some(p) = pos {
                                self.panes[idx].map_memory.center_at(p);
                            }
                        }
                    }
                }
            });

        // Restore tiles and label tiles
        if is_dark_theme {
            self.tiles_dark = tiles_owned;
        } else {
            self.tiles_light = tiles_owned;
        }

        if took_dark_labels {
            self.label_tiles_dark = label_tiles;
        } else if took_light_labels {
            self.label_tiles_light = label_tiles;
        }

        actions
    }

    /// Render the layers panel on the left side (desktop).
    #[cfg(not(target_os = "android"))]
    fn render_layers_panel(&mut self, ctx: &Context) -> Vec<GuiAction> {
        let mut actions = Vec::new();
        let mut pane = std::mem::take(&mut self.panes[self.active_pane]);
        let day = pane.layers.spc_day;

        egui::SidePanel::left("layers_panel")
            .default_width(170.0)
            .resizable(false)
            .show(ctx, |ui| {
                ui.heading("Layers");
                ui.separator();

                // --- Pane selector (only when multi-pane) ---
                if self.pane_layout.pane_count > 1 {
                    ui.horizontal(|ui| {
                        ui.label("Pane:");
                        for i in 0..self.pane_layout.pane_count {
                            let label = format!("{}", i + 1);
                            if ui.selectable_label(self.active_pane == i, &label).clicked()
                                && self.active_pane != i
                            {
                                // Restore current pane before switching
                                self.panes[self.active_pane] = std::mem::take(&mut pane);
                                self.active_pane = i;
                                pane = std::mem::take(&mut self.panes[i]);
                            }
                        }
                    });
                    ui.separator();
                }

                // --- Pane count selector ---
                {
                    let max_panes = if cfg!(target_os = "android") {
                        MAX_PANES_MOBILE
                    } else {
                        MAX_PANES_DESKTOP
                    };
                    ui.horizontal(|ui| {
                        ui.label("Panes:");
                        for count in 1..=max_panes {
                            if ui.selectable_label(
                                self.pane_layout.pane_count == count,
                                format!("{count}"),
                            ).clicked() && self.pane_layout.pane_count != count {
                                // Restore current pane first
                                self.panes[self.active_pane] = std::mem::take(&mut pane);
                                // Resize panes vec
                                while self.panes.len() < count {
                                    self.panes.push(PaneState::new());
                                }
                                self.pane_layout = PaneLayout::for_count(count);
                                if self.active_pane >= count {
                                    self.active_pane = 0;
                                }
                                pane = std::mem::take(&mut self.panes[self.active_pane]);
                            }
                        }
                    });
                    ui.separator();
                }

                // --- Radar layer ---
                ui.checkbox(pane.layers.enabled_mut(LayerKind::Radar), "🛰  Radar");

                if pane.layers.is_enabled(LayerKind::Radar) {
                    ui.indent("radar_controls", |ui| {
                        if let Some(scan_info) = &self.scan_info {
                            // Product selector
                            let prev_product = pane.selected_product;
                            egui::ComboBox::from_id_salt("layer_product_selector")
                                .selected_text(pane.selected_product.name())
                                .width(120.0)
                                .show_ui(ui, |ui| {
                                    for product in &scan_info.available_products {
                                        ui.selectable_value(
                                            &mut pane.selected_product,
                                            *product,
                                            product.name(),
                                        );
                                    }
                                });
                            if prev_product != pane.selected_product {
                                pane.selected_elevation = 0.0;
                            }

                            // Elevation selector
                            if let Some(elevations) =
                                scan_info.product_elevations.get(&pane.selected_product)
                            {
                                if !elevations.is_empty() {
                                    let selected_angle = elevations
                                        .iter()
                                        .min_by(|a, b| {
                                            ((**a - pane.selected_elevation).abs())
                                                .partial_cmp(
                                                    &((**b - pane.selected_elevation).abs()),
                                                )
                                                .unwrap()
                                        })
                                        .copied()
                                        .unwrap_or(0.0);

                                    egui::ComboBox::from_id_salt("layer_elevation_selector")
                                        .selected_text(format!("{:.1}°", selected_angle))
                                        .width(120.0)
                                        .show_ui(ui, |ui| {
                                            for angle in elevations.iter() {
                                                ui.selectable_value(
                                                    &mut pane.selected_elevation,
                                                    *angle,
                                                    format!("{:.1}°", angle),
                                                );
                                            }
                                        });
                                }
                            }
                        } else {
                            ui.label("No scan loaded");
                        }
                    });
                }

                ui.add_space(6.0);
                ui.separator();

                // --- SPC Outlooks section ---
                ui.label("⛈  SPC Outlooks");

                // Day selector
                ui.horizontal_wrapped(|ui| {
                    ui.label("Day:");
                    let mut changed = false;
                    let mut new_day = pane.layers.spc_day;
                    for &d in OutlookDay::all() {
                        if ui.selectable_label(new_day == d, d.label()).clicked() {
                            new_day = d;
                            changed = true;
                        }
                    }
                    if changed {
                        pane.layers.spc_day = new_day;
                        // Fetch data for the new day if any SPC layers are enabled
                        let products = pane.layers.enabled_spc_products();
                        if !products.is_empty() {
                            actions.push(GuiAction::FetchSpcOutlook {
                                day: new_day,
                                products,
                            });
                        }
                    }
                });

                // Product toggles (depends on selected day)
                let spc_layers = pane.layers.spc_layers_for_day();
                for layer in &spc_layers {
                    let was_enabled = pane.layers.is_enabled(*layer);
                    ui.checkbox(pane.layers.enabled_mut(*layer), layer.display_name());
                    let is_enabled = pane.layers.is_enabled(*layer);

                    // If just toggled on and we don't have data, fetch it
                    if is_enabled && !was_enabled {
                        if let Some(product) = layer.to_outlook_product() {
                            if !self.spc_outlooks.contains_key(&(day, product)) {
                                actions.push(GuiAction::FetchSpcOutlook {
                                    day,
                                    products: vec![product],
                                });
                            }
                        }
                    }
                }

                // Refresh button
                if pane.layers.any_spc_enabled() {
                    ui.horizontal(|ui| {
                        let refresh_enabled = !self.spc_fetching;
                        if ui.add_enabled(refresh_enabled, egui::Button::new("🔄 Refresh")).clicked() {
                            actions.push(GuiAction::RefreshSpcOutlooks);
                        }
                        if self.spc_fetching {
                            ui.spinner();
                        }
                    });
                }

                ui.add_space(6.0);
                ui.separator();

                // --- SPC Mesoscale Discussions section ---
                {
                    let was_enabled = pane.layers.is_enabled(LayerKind::SpcMesoscaleDiscussions);
                    let label = if self.spc_discussions.is_empty() {
                        "📋  Mesoscale Disc.".to_string()
                    } else {
                        format!("📋  Mesoscale Disc. ({})", self.spc_discussions.len())
                    };
                    ui.checkbox(
                        pane.layers.enabled_mut(LayerKind::SpcMesoscaleDiscussions),
                        label,
                    );
                    let is_enabled = pane.layers.is_enabled(LayerKind::SpcMesoscaleDiscussions);

                    // If just toggled on and we have no data, fetch
                    if is_enabled && !was_enabled && self.spc_discussions.is_empty() && !self.spc_md_fetching {
                        actions.push(GuiAction::FetchSpcDiscussions);
                    }
                }

                if pane.layers.is_enabled(LayerKind::SpcMesoscaleDiscussions) {
                    ui.horizontal(|ui| {
                        let refresh_enabled = !self.spc_md_fetching;
                        if ui.add_enabled(refresh_enabled, egui::Button::new("🔄 Refresh")).clicked() {
                            actions.push(GuiAction::RefreshSpcDiscussions);
                        }
                        if self.spc_md_fetching {
                            ui.spinner();
                        }
                    });
                    if let Some(t) = self.spc_md_fetch_time {
                        let secs_ago = t.elapsed().as_secs();
                        let label = if secs_ago < 60 {
                            format!("Updated {}s ago", secs_ago)
                        } else {
                            format!("Updated {}m ago", secs_ago / 60)
                        };
                        ui.label(egui::RichText::new(label).small().weak());
                    }
                }

                ui.add_space(6.0);
                ui.separator();

                // --- NWS Alerts section ---
                ui.label("⚠  NWS Alerts");

                let nws_layers = [LayerKind::NwsWarnings, LayerKind::NwsWatches, LayerKind::NwsAdvisories];
                for layer in &nws_layers {
                    let was_enabled = pane.layers.is_enabled(*layer);
                    ui.checkbox(pane.layers.enabled_mut(*layer), layer.display_name());
                    let is_enabled = pane.layers.is_enabled(*layer);

                    // If just toggled on and we have no alerts cached, fetch them
                    if is_enabled && !was_enabled && self.nws_alerts.is_empty() && !self.nws_fetching {
                        actions.push(GuiAction::FetchNwsAlerts);
                    }
                }

                if pane.layers.any_nws_enabled() {
                    ui.horizontal(|ui| {
                        let refresh_enabled = !self.nws_fetching;
                        if ui.add_enabled(refresh_enabled, egui::Button::new("🔄 Refresh")).clicked() {
                            actions.push(GuiAction::RefreshNwsAlerts);
                        }
                        if self.nws_fetching {
                            ui.spinner();
                        }
                    });
                    // Show alert count and last-updated time
                    if !self.nws_alerts.is_empty() {
                        let categories = pane.layers.enabled_nws_categories();
                        let visible_count = self.nws_alerts.iter()
                            .filter(|a| categories.contains(&a.category))
                            .count();
                        ui.label(format!("{} alerts shown", visible_count));
                    }
                    if let Some(t) = self.nws_fetch_time {
                        let secs_ago = t.elapsed().as_secs();
                        let label = if secs_ago < 60 {
                            format!("Updated {}s ago", secs_ago)
                        } else {
                            format!("Updated {}m ago", secs_ago / 60)
                        };
                        ui.label(egui::RichText::new(label).small().weak());
                    }
                }

                ui.add_space(6.0);
                ui.separator();

                // --- Viewport sync ---
                if self.pane_layout.pane_count > 1 {
                    ui.checkbox(&mut self.viewport_sync, "🔗  Sync Viewports");
                    ui.checkbox(&mut self.sync_layers, "🔗  Sync Layers");
                    ui.separator();
                }

                // --- Other overlays ---
                ui.checkbox(pane.layers.enabled_mut(LayerKind::CityLabels), "🏷  City Labels");
                ui.checkbox(pane.layers.enabled_mut(LayerKind::RadarSites), "📡  Radar Sites");
            });

        self.panes[self.active_pane] = pane;

        // Propagate layer settings to all other panes when sync is enabled
        if self.sync_layers && self.pane_layout.pane_count > 1 {
            let src = &self.panes[self.active_pane].layers;
            let spc_day = src.spc_day;
            let snapshot: Vec<(LayerKind, bool)> = [
                LayerKind::Radar,
                LayerKind::SpcCategorical,
                LayerKind::SpcTornado,
                LayerKind::SpcWind,
                LayerKind::SpcHail,
                LayerKind::SpcProbabilistic,
                LayerKind::SpcMesoscaleDiscussions,
                LayerKind::NwsWarnings,
                LayerKind::NwsWatches,
                LayerKind::NwsAdvisories,
                LayerKind::CityLabels,
                LayerKind::RadarSites,
            ]
            .iter()
            .map(|&k| (k, src.is_enabled(k)))
            .collect();

            for (idx, p) in self.panes.iter_mut().enumerate() {
                if idx == self.active_pane {
                    continue;
                }
                for &(kind, enabled) in &snapshot {
                    p.layers.set_enabled(kind, enabled);
                }
                p.layers.spc_day = spc_day;
            }
        }

        actions
    }

    /// Get the selected product and elevation for rendering (active pane).
    pub fn get_rendering_params(&self) -> Option<(RadarProduct, f32)> {
        self.panes[self.active_pane].get_rendering_params(self.scan_info.as_ref())
    }

    /// Get the rendering params for a specific pane.
    pub fn get_rendering_params_for_pane(&self, pane_idx: PaneId) -> Option<(RadarProduct, f32)> {
        self.panes.get(pane_idx)
            .and_then(|p| p.get_rendering_params(self.scan_info.as_ref()))
    }

    /// Number of active panes.
    pub fn pane_count(&self) -> usize {
        self.pane_layout.pane_count
    }

    /// Get the current radar config
    pub fn get_radar_config(&self) -> &RadarConfig {
        &self.radar_config
    }

    /// Set the radar config
    pub fn set_radar_config(&mut self, config: RadarConfig) {
        // Read timestamps before moving config
        let date = config.timestamp.format("%Y-%m-%d").to_string();
        let time = config.timestamp.format("%H:%M:%S").to_string();
        self.radar_config = config;
        self.date_string = date;
        self.time_string = time;
    }

    /// Set which site is currently loading
    pub fn set_loading_site(&mut self, site: Option<String>) {
        self.loading_site = site;
    }

    /// Set the user's GPS location for the blue dot indicator
    /// Set safe area insets in logical pixels (top, bottom, left, right).
    /// On Android, this compensates for the status bar and navigation bar.
    #[cfg(target_os = "android")]
    pub fn set_safe_area_insets(&mut self, top: f32, bottom: f32, left: f32, right: f32) {
        self.safe_area_insets = (top, bottom, left, right);
    }

    pub fn set_user_location(&mut self, lat: f64, lon: f64) {
        self.user_location = Some((lat, lon));
    }

    /// Store a fetched SPC outlook in the cache.
    pub fn set_spc_outlook(&mut self, day: OutlookDay, product: OutlookProduct, outlook: SpcOutlook) {
        self.spc_outlooks.insert((day, product), outlook);
        self.spc_fetch_times.insert((day, product), std::time::Instant::now());
        // Bump data generation to invalidate cached overlay geometry
        let generation = self.spc_data_generation.entry((day, product)).or_insert(0);
        *generation = generation.wrapping_add(1);
    }

    /// Set whether an SPC fetch is currently in progress.
    pub fn set_spc_fetching(&mut self, fetching: bool) {
        self.spc_fetching = fetching;
    }

    /// Store fetched NWS alerts, replacing the previous set.
    pub fn set_nws_alerts(&mut self, alerts: Vec<NwsAlert>) {
        self.nws_alerts = alerts;
        self.nws_fetch_time = Some(std::time::Instant::now());
        // Invalidate cached overlay geometry
        self.nws_data_generation = self.nws_data_generation.wrapping_add(1);
    }

    /// Set whether an NWS alerts fetch is currently in progress.
    pub fn set_nws_fetching(&mut self, fetching: bool) {
        self.nws_fetching = fetching;
    }

    /// Store fetched SPC Mesoscale Discussions, replacing the previous set.
    pub fn set_spc_discussions(&mut self, discussions: Vec<SpcDiscussion>) {
        self.spc_discussions = discussions;
        self.spc_md_fetch_time = Some(std::time::Instant::now());
        // Invalidate cached overlay geometry
        self.spc_md_data_generation = self.spc_md_data_generation.wrapping_add(1);
    }

    /// Set whether an SPC MD fetch is currently in progress.
    pub fn set_spc_md_fetching(&mut self, fetching: bool) {
        self.spc_md_fetching = fetching;
    }

    /// Get the active pane's layer manager (immutable).
    pub fn layers(&self) -> &LayerManager {
        &self.panes[self.active_pane].layers
    }

    /// Get the active pane's layer manager (mutable).
    pub fn layers_mut(&mut self) -> &mut LayerManager {
        &mut self.panes[self.active_pane].layers
    }

    /// Get a specific pane's layer manager (immutable).
    pub fn layers_for_pane(&self, pane_idx: PaneId) -> Option<&LayerManager> {
        self.panes.get(pane_idx).map(|p| &p.layers)
    }

    /// Get the current scan info
    pub fn get_scan_info(&self) -> Option<&ScanInfo> {
        self.scan_info.as_ref()
    }

    /// Get the active pane's radar image.
    pub fn get_radar_image(&self) -> &Option<(egui::TextureHandle, f64, f64, f64, Arc<Vec<f32>>)> {
        &self.panes[self.active_pane].radar_image
    }

    /// Take the radar image from the active pane.
    pub fn take_radar_image(&mut self) -> Option<(egui::TextureHandle, f64, f64, f64, Arc<Vec<f32>>)> {
        self.panes[self.active_pane].take_radar_image()
    }

    /// Take the radar image from a specific pane.
    pub fn take_radar_image_for_pane(&mut self, pane_idx: PaneId) -> Option<(egui::TextureHandle, f64, f64, f64, Arc<Vec<f32>>)> {
        self.panes.get_mut(pane_idx).and_then(|p| p.take_radar_image())
    }

    /// Set the radar image for the active pane.
    pub fn set_radar_image(
        &mut self,
        texture: egui::TextureHandle,
        lat: f64,
        lon: f64,
        max_range_km: f64,
        value_data: Vec<f32>,
    ) {
        self.panes[self.active_pane].set_radar_image(texture, lat, lon, max_range_km, value_data);
    }

    /// Set the radar image for a specific pane.
    pub fn set_radar_image_for_pane(
        &mut self,
        pane_idx: PaneId,
        texture: egui::TextureHandle,
        lat: f64,
        lon: f64,
        max_range_km: f64,
        value_data: Vec<f32>,
    ) {
        if let Some(pane) = self.panes.get_mut(pane_idx) {
            pane.set_radar_image(texture, lat, lon, max_range_km, value_data);
        }
    }

    /// Clear the radar image on the active pane.
    pub fn clear_radar_image(&mut self) {
        self.panes[self.active_pane].clear_radar_image();
    }

    /// Clear the radar image on a specific pane.
    pub fn clear_radar_image_for_pane(&mut self, pane_idx: PaneId) {
        if pane_idx < self.panes.len() {
            self.panes[pane_idx].clear_radar_image();
        }
    }

    /// Clear the radar image on all panes.
    pub fn clear_all_radar_images(&mut self) {
        for pane in &mut self.panes {
            pane.clear_radar_image();
        }
    }

    /// Whether auto-poll is active and the event loop should keep waking
    pub fn is_auto_poll_active(&self) -> bool {
        (self.auto_poll_enabled && self.initial_fetch_done)
            || self.panes.iter().any(|p| p.layers.any_nws_enabled())
    }

    pub fn clear_graphics_state(&mut self) {
        for pane in &mut self.panes {
            pane.clear_radar_image();
        }
        self.tiles_light = None;
        self.tiles_dark = None;
        self.label_tiles_light = None;
        self.label_tiles_dark = None;
        self.loading_site = None;
        // Reset theme tracking to force tile recreation on next render
        self.current_theme_is_dark = !self.current_theme_is_dark;
    }

    /// Render the NWS alert detail popup when an alert is selected.
    fn render_alert_popup(&mut self, ctx: &egui::Context) {
        let Some(idx) = self.selected_alert else {
            return;
        };
        let Some(alert) = self.nws_alerts.get(idx) else {
            self.selected_alert = None;
            return;
        };

        // Clone data needed for the popup to avoid borrowing issues
        let event = alert.event.clone();
        let headline = alert.headline.clone();
        let area_desc = alert.area_desc.clone();
        let sender_name = alert.sender_name.clone();
        let effective = alert.effective.clone();
        let expires = alert.expires.clone();
        let description = alert.description.clone();
        let instruction = alert.instruction.clone();
        let [r, g, b, _] = alert.features.first()
            .map(|f| f.stroke_rgba)
            .unwrap_or([200, 200, 200, 255]);
        let accent = egui::Color32::from_rgb(r, g, b);

        let mut open = true;
        let screen = ctx.input(|i| i.viewport_rect());
        let is_mobile = cfg!(target_os = "android");
        let popup_width = if is_mobile {
            (screen.width() - 32.0).max(200.0)
        } else {
            380.0
        };
        let popup_max_height = if is_mobile {
            (screen.height() - 80.0).max(200.0)
        } else {
            500.0
        };

        egui::Window::new(egui::RichText::new(&event).color(accent).strong())
            .id(egui::Id::new("nws_alert_popup"))
            .open(&mut open)
            .collapsible(false)
            .resizable(!is_mobile)
            .default_width(popup_width)
            .max_width(popup_width)
            .max_height(popup_max_height)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                // Headline
                if let Some(headline) = &headline {
                    ui.label(egui::RichText::new(headline).strong().size(if is_mobile { 13.0 } else { 14.0 }));
                    ui.add_space(4.0);
                }

                // Metadata grid — wrap long text for mobile
                egui::Grid::new("alert_meta").num_columns(2).show(ui, |ui| {
                    ui.label(egui::RichText::new("Areas:").strong());
                    ui.add(egui::Label::new(&area_desc).wrap());
                    ui.end_row();

                    ui.label(egui::RichText::new("Issued by:").strong());
                    ui.add(egui::Label::new(&sender_name).wrap());
                    ui.end_row();

                    ui.label(egui::RichText::new("Effective:").strong());
                    ui.label(format_iso_time(&effective));
                    ui.end_row();

                    ui.label(egui::RichText::new("Expires:").strong());
                    ui.label(format_iso_time(&expires));
                    ui.end_row();
                });

                ui.separator();

                // Description (scrollable)
                egui::ScrollArea::vertical()
                    .max_height(250.0)
                    .show(ui, |ui| {
                        ui.label(&description);
                    });

                // Instruction (emphasized)
                if let Some(instruction) = &instruction {
                    ui.add_space(4.0);
                    ui.separator();
                    ui.label(
                        egui::RichText::new(instruction)
                            .strong()
                            .color(accent),
                    );
                }
            });

        if !open {
            self.selected_alert = None;
        }
    }

    /// Render the SPC Mesoscale Discussion detail popup when an MD is selected.
    fn render_md_popup(&mut self, ctx: &egui::Context) {
        let Some(idx) = self.selected_md else {
            return;
        };
        let Some(md) = self.spc_discussions.get(idx) else {
            self.selected_md = None;
            return;
        };

        // Clone data to avoid borrow issues
        let number = md.number;
        let md_type = md.md_type;
        let concerning = md.concerning.clone();
        let text = md.text.clone();
        let link = md.link.clone();
        let stroke_rgba = md_stroke_color(&md_type);
        let [r, g, b, _] = stroke_rgba;
        let accent = egui::Color32::from_rgb(r, g, b);

        let mut open = true;
        let screen = ctx.input(|i| i.viewport_rect());
        let is_mobile = cfg!(target_os = "android");
        let popup_width = if is_mobile {
            (screen.width() - 32.0).max(200.0)
        } else {
            420.0
        };
        let popup_max_height = if is_mobile {
            (screen.height() - 80.0).max(200.0)
        } else {
            500.0
        };

        let title = format!("Mesoscale Discussion #{:04}", number);
        egui::Window::new(egui::RichText::new(&title).color(accent).strong())
            .id(egui::Id::new("spc_md_popup"))
            .open(&mut open)
            .collapsible(false)
            .resizable(!is_mobile)
            .default_width(popup_width)
            .max_width(popup_width)
            .max_height(popup_max_height)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                // Type badge
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new(format!("Type: {}", md_type)).strong().color(accent));
                });

                // Concerning line
                if let Some(ref concerning) = concerning {
                    ui.add_space(2.0);
                    ui.label(egui::RichText::new(format!("Concerning: {}", concerning)).strong());
                }

                ui.add_space(4.0);
                ui.separator();

                // Full discussion text (scrollable)
                egui::ScrollArea::vertical()
                    .max_height(if is_mobile { 300.0 } else { 350.0 })
                    .show(ui, |ui| {
                        ui.label(
                            egui::RichText::new(&text)
                                .font(egui::FontId::monospace(if is_mobile { 11.0 } else { 12.0 }))
                        );
                    });

                ui.add_space(4.0);
                ui.separator();

                // Link to SPC
                if !link.is_empty() {
                    ui.hyperlink_to("Open on SPC website", &link);
                }
            });

        if !open {
            self.selected_md = None;
        }
    }

    #[cfg(not(target_os = "android"))]
    fn render_desktop_ui(&mut self, ctx: &egui::Context, actions: &mut Vec<GuiAction>) {
        // Menu bar
        let mut action = None;
        self.render_menu_bar(ctx, &mut action);
        if let Some(a) = action {
            actions.push(a);
        }

        // Time dialog
        if let Some(a) = self.render_time_dialog(ctx) {
            actions.push(a);
        }

        // Bottom status bar - render first so it spans the full width
        if let Some(a) = self.render_status_bar(ctx) {
            actions.push(a);
        }

        // Left panel for layer controls (replaces old right radar display panel)
        let layer_actions = self.render_layers_panel(ctx);
        actions.extend(layer_actions);

        // Map in central panel
        let map_actions = self.render_map(ctx);
        actions.extend(map_actions);

        // NWS alert detail popup (rendered after map so it floats on top)
        self.render_alert_popup(ctx);

        // SPC Mesoscale Discussion detail popup
        self.render_md_popup(ctx);
    }

    #[cfg(target_os = "android")]
    fn render_mobile_ui(&mut self, ctx: &egui::Context, actions: &mut Vec<GuiAction>) {
        // Time dialog (shared between platforms)
        if let Some(a) = self.render_time_dialog(ctx) {
            actions.push(a);
        }

        // Mobile status bar (simplified)
        if let Some(a) = self.render_mobile_status_bar(ctx) {
            actions.push(a);
        }

        // Collapsible layers panel
        let layer_actions = self.render_mobile_layers_panel(ctx);
        actions.extend(layer_actions);

        // Map in central panel
        let map_actions = self.render_map(ctx);
        actions.extend(map_actions);

        // NWS alert detail popup (rendered after map so it floats on top)
        self.render_alert_popup(ctx);

        // SPC Mesoscale Discussion detail popup
        self.render_md_popup(ctx);

        // Floating hamburger button to open the menu (drawn last so it's on top)
        if !self.show_mobile_menu {
            let top_inset = self.safe_area_insets.0;
            let btn_rect = egui::Rect::from_min_size(
                egui::pos2(12.0, 48.0 + top_inset),
                egui::vec2(48.0, 48.0),
            );
            let response = ctx.input(|i| i.pointer.any_click()
                && i.pointer.interact_pos().map_or(false, |p| btn_rect.contains(p)));
            let painter = ctx.layer_painter(egui::LayerId::new(
                egui::Order::Foreground,
                egui::Id::new("mobile_hamburger"),
            ));
            let bg_color = if ctx.style().visuals.dark_mode {
                egui::Color32::from_rgba_unmultiplied(40, 40, 40, 220)
            } else {
                egui::Color32::from_rgba_unmultiplied(240, 240, 240, 230)
            };
            painter.rect_filled(btn_rect, 8.0, bg_color);
            painter.text(
                btn_rect.center(),
                egui::Align2::CENTER_CENTER,
                "☰",
                egui::FontId::proportional(26.0),
                ctx.style().visuals.text_color(),
            );
            if response {
                self.show_mobile_menu = true;
            }
        }
    }

    #[cfg(target_os = "android")]
    fn render_mobile_status_bar(&mut self, ctx: &egui::Context) -> Option<GuiAction> {
        let mut action = None;
        let top_inset = self.safe_area_insets.0;
        
        egui::TopBottomPanel::top("mobile_status_bar")
            .min_height(32.0 + top_inset)
            .show_separator_line(true)
            .show(ctx, |ui| {
                // Add spacing to push content below the status bar cutout
                if top_inset > 0.0 {
                    ui.add_space(top_inset);
                }
                ui.horizontal_centered(|ui| {
                    // Refresh button
                    let refresh_button = ui.add_enabled(
                        !self.fetching,
                        egui::Button::new("🔄").min_size(egui::vec2(44.0, 32.0))
                    );
                    if refresh_button.clicked() {
                        action = Some(GuiAction::FetchRadarScan(self.radar_config.clone()));
                    }
                    refresh_button.on_hover_text("Refresh radar data");

                    ui.separator();

                    // Status indicator
                    if self.fetching {
                        ui.label("🔄 Loading...");
                        ui.spinner();
                    } else if let Some(scan_info) = &self.scan_info {
                        ui.label(format!("{} @ {}", 
                            scan_info.site.name,
                            scan_info.timestamp.format("%H:%M")
                        ));
                    } else {
                        ui.label("No scan loaded");
                    }

                    ui.separator();

                    // Error message (if any)
                    let mut dismiss_error = false;
                    if let Some(error_msg) = &self.error_message {
                        if ui.button("✕").clicked() {
                            dismiss_error = true;
                        }
                        ui.label(error_msg.as_str());
                    }
                    if dismiss_error { self.error_message = None; }
                });
            });
        
        action
    }

    /// Collapsible layers/controls panel for mobile (replaces bottom toolbar).
    #[cfg(target_os = "android")]
    fn render_mobile_layers_panel(&mut self, ctx: &egui::Context) -> Vec<GuiAction> {
        let mut actions = Vec::new();
        if !self.show_mobile_menu {
            return actions;
        }

        let top_inset = self.safe_area_insets.0;
        let bottom_inset = self.safe_area_insets.1;

        let mut pane = std::mem::take(&mut self.panes[self.active_pane]);
        let day = pane.layers.spc_day;

        egui::SidePanel::left("mobile_layers_panel")
            .default_width(260.0)
            .resizable(false)
            .show(ctx, |ui| {
                // Safe-area top padding
                if top_inset > 0.0 {
                    ui.add_space(top_inset);
                }

                // Header with close button
                ui.horizontal(|ui| {
                    ui.heading("Layers");
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("✕").clicked() {
                            self.show_mobile_menu = false;
                        }
                    });
                });
                ui.separator();

                egui::ScrollArea::vertical().show(ui, |ui| {
                    // ── Pane count selector (mobile: 1-4) ──
                    {
                        ui.horizontal(|ui| {
                            ui.label("Panes:");
                            for count in 1..=MAX_PANES_MOBILE {
                                if ui.selectable_label(
                                    self.pane_layout.pane_count == count,
                                    format!("{count}"),
                                ).clicked() && self.pane_layout.pane_count != count {
                                    self.panes[self.active_pane] = std::mem::take(&mut pane);
                                    while self.panes.len() < count {
                                        self.panes.push(PaneState::new());
                                    }
                                    self.pane_layout = PaneLayout::for_count(count);
                                    if self.active_pane >= count {
                                        self.active_pane = 0;
                                    }
                                    pane = std::mem::take(&mut self.panes[self.active_pane]);
                                }
                            }
                        });
                        if self.pane_layout.pane_count > 1 {
                            ui.horizontal(|ui| {
                                ui.label("Pane:");
                                for i in 0..self.pane_layout.pane_count {
                                    if ui.selectable_label(self.active_pane == i, format!("{}", i + 1)).clicked()
                                        && self.active_pane != i
                                    {
                                        self.panes[self.active_pane] = std::mem::take(&mut pane);
                                        self.active_pane = i;
                                        pane = std::mem::take(&mut self.panes[i]);
                                    }
                                }
                            });
                        }
                        ui.separator();
                    }

                    // ── Radar ──
                    ui.checkbox(pane.layers.enabled_mut(LayerKind::Radar), "🛰  Radar");

                    if pane.layers.is_enabled(LayerKind::Radar) {
                        ui.indent("m_radar_controls", |ui| {
                            if let Some(scan_info) = &self.scan_info {
                                let prev_product = pane.selected_product;
                                egui::ComboBox::from_id_salt("m_product_sel")
                                    .selected_text(pane.selected_product.name())
                                    .width(180.0)
                                    .show_ui(ui, |ui| {
                                        for product in &scan_info.available_products {
                                            ui.selectable_value(
                                                &mut pane.selected_product,
                                                *product,
                                                product.name(),
                                            );
                                        }
                                    });
                                if prev_product != pane.selected_product {
                                    pane.selected_elevation = 0.0;
                                }

                                if let Some(elevations) =
                                    scan_info.product_elevations.get(&pane.selected_product)
                                {
                                    if !elevations.is_empty() {
                                        let selected_angle = elevations
                                            .iter()
                                            .min_by(|a, b| {
                                                ((**a - pane.selected_elevation).abs())
                                                    .partial_cmp(
                                                        &((**b - pane.selected_elevation).abs()),
                                                    )
                                                    .unwrap()
                                            })
                                            .copied()
                                            .unwrap_or(0.0);

                                        egui::ComboBox::from_id_salt("m_elev_sel")
                                            .selected_text(format!("{:.1}°", selected_angle))
                                            .width(180.0)
                                            .show_ui(ui, |ui| {
                                                for angle in elevations.iter() {
                                                    ui.selectable_value(
                                                        &mut pane.selected_elevation,
                                                        *angle,
                                                        format!("{:.1}°", angle),
                                                    );
                                                }
                                            });
                                    }
                                }
                            } else {
                                ui.label("No scan loaded");
                            }
                        });
                    }

                    ui.add_space(6.0);
                    ui.separator();

                    // ── SPC Outlooks ──
                    ui.label("⛈  SPC Outlooks");

                    ui.horizontal_wrapped(|ui| {
                        ui.label("Day:");
                        let mut changed = false;
                        let mut new_day = pane.layers.spc_day;
                        for &d in OutlookDay::all() {
                            if ui.selectable_label(new_day == d, d.label()).clicked() {
                                new_day = d;
                                changed = true;
                            }
                        }
                        if changed {
                            pane.layers.spc_day = new_day;
                            let products = pane.layers.enabled_spc_products();
                            if !products.is_empty() {
                                actions.push(GuiAction::FetchSpcOutlook {
                                    day: new_day,
                                    products,
                                });
                            }
                        }
                    });

                    let spc_layers = pane.layers.spc_layers_for_day();
                    for layer in &spc_layers {
                        let was_enabled = pane.layers.is_enabled(*layer);
                        ui.checkbox(pane.layers.enabled_mut(*layer), layer.display_name());
                        let is_enabled = pane.layers.is_enabled(*layer);
                        if is_enabled && !was_enabled {
                            if let Some(product) = layer.to_outlook_product() {
                                if !self.spc_outlooks.contains_key(&(day, product)) {
                                    actions.push(GuiAction::FetchSpcOutlook {
                                        day,
                                        products: vec![product],
                                    });
                                }
                            }
                        }
                    }

                    if pane.layers.any_spc_enabled() {
                        ui.horizontal(|ui| {
                            if ui
                                .add_enabled(!self.spc_fetching, egui::Button::new("🔄 Refresh"))
                                .clicked()
                            {
                                actions.push(GuiAction::RefreshSpcOutlooks);
                            }
                            if self.spc_fetching {
                                ui.spinner();
                            }
                        });
                    }

                    ui.add_space(6.0);
                    ui.separator();

                    // ── SPC Mesoscale Discussions ──
                    {
                        let was_enabled = pane.layers.is_enabled(LayerKind::SpcMesoscaleDiscussions);
                        let label = if self.spc_discussions.is_empty() {
                            "📋  Mesoscale Disc.".to_string()
                        } else {
                            format!("📋  Mesoscale Disc. ({})", self.spc_discussions.len())
                        };
                        ui.checkbox(
                            pane.layers.enabled_mut(LayerKind::SpcMesoscaleDiscussions),
                            label,
                        );
                        let is_enabled = pane.layers.is_enabled(LayerKind::SpcMesoscaleDiscussions);
                        if is_enabled && !was_enabled && self.spc_discussions.is_empty() && !self.spc_md_fetching {
                            actions.push(GuiAction::FetchSpcDiscussions);
                        }
                    }

                    if pane.layers.is_enabled(LayerKind::SpcMesoscaleDiscussions) {
                        ui.horizontal(|ui| {
                            if ui
                                .add_enabled(!self.spc_md_fetching, egui::Button::new("🔄 Refresh"))
                                .clicked()
                            {
                                actions.push(GuiAction::RefreshSpcDiscussions);
                            }
                            if self.spc_md_fetching {
                                ui.spinner();
                            }
                        });
                        if let Some(t) = self.spc_md_fetch_time {
                            let secs_ago = t.elapsed().as_secs();
                            let label = if secs_ago < 60 {
                                format!("Updated {}s ago", secs_ago)
                            } else {
                                format!("Updated {}m ago", secs_ago / 60)
                            };
                            ui.label(egui::RichText::new(label).small().weak());
                        }
                    }

                    ui.add_space(6.0);
                    ui.separator();

                    // ── NWS Alerts ──
                    ui.label("⚠  NWS Alerts");

                    let nws_layers = [LayerKind::NwsWarnings, LayerKind::NwsWatches, LayerKind::NwsAdvisories];
                    for layer in &nws_layers {
                        let was_enabled = pane.layers.is_enabled(*layer);
                        ui.checkbox(pane.layers.enabled_mut(*layer), layer.display_name());
                        let is_enabled = pane.layers.is_enabled(*layer);
                        if is_enabled && !was_enabled && self.nws_alerts.is_empty() && !self.nws_fetching {
                            actions.push(GuiAction::FetchNwsAlerts);
                        }
                    }

                    if pane.layers.any_nws_enabled() {
                        ui.horizontal(|ui| {
                            if ui
                                .add_enabled(!self.nws_fetching, egui::Button::new("🔄 Refresh"))
                                .clicked()
                            {
                                actions.push(GuiAction::RefreshNwsAlerts);
                            }
                            if self.nws_fetching {
                                ui.spinner();
                            }
                        });
                        if !self.nws_alerts.is_empty() {
                            let categories = pane.layers.enabled_nws_categories();
                            let visible_count = self.nws_alerts.iter()
                                .filter(|a| categories.contains(&a.category))
                                .count();
                            ui.label(format!("{} alerts shown", visible_count));
                        }
                    }

                    ui.add_space(6.0);
                    ui.separator();

                    // ── Other overlays ──
                    ui.checkbox(pane.layers.enabled_mut(LayerKind::CityLabels), "🏷  City Labels");
                    ui.checkbox(pane.layers.enabled_mut(LayerKind::RadarSites), "📡  Radar Sites");

                    // ── Viewport sync toggle ──
                    if self.pane_layout.pane_count > 1 {
                        ui.separator();
                        ui.checkbox(&mut self.viewport_sync, "🔗  Sync Viewports");
                        ui.checkbox(&mut self.sync_layers, "🔗  Sync Layers");
                    }

                    ui.add_space(10.0);
                    ui.separator();

                    // ── Controls ──
                    ui.label("⚙  Controls");
                    ui.add_space(4.0);

                    // Refresh
                    if ui
                        .add_enabled(!self.fetching, egui::Button::new("🔄  Refresh Radar"))
                        .clicked()
                    {
                        actions.push(GuiAction::FetchRadarScan(self.radar_config.clone()));
                    }

                    // Time
                    if ui.button("🕐  Set Time...").clicked() {
                        self.show_time_dialog = true;
                        self.show_mobile_menu = false; // close menu so dialog is visible
                    }

                    // Auto-poll
                    ui.checkbox(&mut self.auto_poll_enabled, "⏰  Auto-poll");

                    if self.fetching {
                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.label("Loading...");
                        });
                    }
                });

                // Safe-area bottom padding
                if bottom_inset > 0.0 {
                    ui.add_space(bottom_inset);
                }
            });

        self.panes[self.active_pane] = pane;

        // Propagate layer settings to all other panes when sync is enabled
        if self.sync_layers && self.pane_layout.pane_count > 1 {
            let src = &self.panes[self.active_pane].layers;
            let spc_day = src.spc_day;
            let snapshot: Vec<(LayerKind, bool)> = [
                LayerKind::Radar,
                LayerKind::SpcCategorical,
                LayerKind::SpcTornado,
                LayerKind::SpcWind,
                LayerKind::SpcHail,
                LayerKind::SpcProbabilistic,
                LayerKind::SpcMesoscaleDiscussions,
                LayerKind::NwsWarnings,
                LayerKind::NwsWatches,
                LayerKind::NwsAdvisories,
                LayerKind::CityLabels,
                LayerKind::RadarSites,
            ]
            .iter()
            .map(|&k| (k, src.is_enabled(k)))
            .collect();

            for (idx, p) in self.panes.iter_mut().enumerate() {
                if idx == self.active_pane {
                    continue;
                }
                for &(kind, enabled) in &snapshot {
                    p.layers.set_enabled(kind, enabled);
                }
                p.layers.spc_day = spc_day;
            }
        }

        actions
    }
}

// ---------------------------------------------------------------------------
// Slippy-map tile coordinate helpers (standard OSM / Web Mercator formulas)
// ---------------------------------------------------------------------------

/// Convert longitude to tile X index at the given zoom level.
fn lon_to_tile_x(lon: f64, zoom: u8) -> u32 {
    let n = 2f64.powi(zoom as i32);
    ((lon + 180.0) / 360.0 * n).floor().max(0.0) as u32
}

/// Convert latitude to tile Y index at the given zoom level.
fn lat_to_tile_y(lat: f64, zoom: u8) -> u32 {
    let n = 2f64.powi(zoom as i32);
    let lat_rad = lat.to_radians();
    ((1.0 - (lat_rad.tan() + 1.0 / lat_rad.cos()).ln() / std::f64::consts::PI) / 2.0 * n)
        .floor()
        .max(0.0) as u32
}

/// Convert tile X index back to the western longitude of the tile.
fn tile_to_lon(x: u32, zoom: u8) -> f64 {
    let n = 2f64.powi(zoom as i32);
    x as f64 / n * 360.0 - 180.0
}

/// Convert tile Y index back to the northern latitude of the tile.
fn tile_to_lat(y: u32, zoom: u8) -> f64 {
    let n = 2f64.powi(zoom as i32);
    (std::f64::consts::PI * (1.0 - 2.0 * y as f64 / n))
        .sinh()
        .atan()
        .to_degrees()
}

/// Draw SPC convective outlook polygons on the map.
///
/// Uses cached projected + triangulated geometry to avoid per-frame O(n²)
/// ear-clip triangulation. The cache is invalidated on viewport changes
/// (pan/zoom) or when new SPC data arrives.
///
/// This is a free function (not a method on `Gui`) to avoid capturing the
/// entire `self` struct inside the map closure, which would conflict with
/// disjoint field borrows required by Rust's borrow checker.
fn draw_spc_overlays(
    ui: &egui::Ui,
    projector: &walkers::Projector,
    zoom: f64,
    layers: &crate::layers::LayerManager,
    spc_outlooks: &HashMap<(OutlookDay, OutlookProduct), SpcOutlook>,
    current_theme_is_dark: bool,
    caches: &mut HashMap<(OutlookDay, OutlookProduct), OverlayLayerCache>,
    data_generations: &HashMap<(OutlookDay, OutlookProduct), u64>,
) {
    let screen_rect = ui.max_rect();
    let key = ViewportKey::from_projector_and_rect(projector, zoom, screen_rect);
    let day = layers.spc_day;

    let hatch_color = if current_theme_is_dark {
        egui::Color32::from_rgba_unmultiplied(200, 200, 200, 180)
    } else {
        egui::Color32::from_rgba_unmultiplied(60, 60, 60, 180)
    };

    // Iterate through enabled SPC layers
    for layer_kind in layers.spc_layers_for_day() {
        if !layers.is_enabled(layer_kind) {
            continue;
        }
        let Some(product) = layer_kind.to_outlook_product() else {
            continue;
        };
        let Some(outlook) = spc_outlooks.get(&(day, product)) else {
            continue;
        };

        let data_gen = data_generations.get(&(day, product)).copied().unwrap_or(0);
        let cache = caches.entry((day, product)).or_insert_with(OverlayLayerCache::new);

        // Rebuild on any viewport or data change. This is cheap because
        // O(n²) triangulation is pre-computed at fetch time — only O(n)
        // vertex projection runs here.
        if !cache.is_valid(&key, data_gen) {
            cache.features = build_cached_features(
                &outlook.features,
                projector,
                screen_rect,
                true,
                current_theme_is_dark,
            );
            cache.viewport_key = key;
            cache.data_generation = data_gen;
        }

        // Draw from cache — all geometry is batched into minimal painter calls
        draw_cached_features(
            ui.painter(),
            &cache.features,
            &outlook.features,
            screen_rect,
            hatch_color,
        );
    }
}

/// Draw SPC Mesoscale Discussion polygons on the map.
///
/// Uses cached projected geometry. On cache miss, temporary `OverlayFeature`
/// wrappers are built for each MD's polygon so `build_cached_features` can
/// pre-triangulate them.
///
/// Returns `Some(discussion_index)` if the user clicked on an MD polygon.
fn draw_spc_discussions(
    ui: &egui::Ui,
    projector: &walkers::Projector,
    zoom: f64,
    layers: &crate::layers::LayerManager,
    discussions: &[SpcDiscussion],
    cache: &mut OverlayLayerCache,
    data_gen: u64,
    pointer_available: bool,
) -> Option<usize> {
    if !layers.is_enabled(LayerKind::SpcMesoscaleDiscussions) || discussions.is_empty() {
        return None;
    }

    let screen_rect = ui.max_rect();
    let key = ViewportKey::from_projector_and_rect(projector, zoom, screen_rect);

    // Rebuild on any viewport or data change
    if !cache.is_valid(&key, data_gen) {
        let temp_features: Vec<rustdar_overlays::types::OverlayFeature> = discussions
            .iter()
            .map(|md| {
                let fill = md_fill_color(&md.md_type);
                let stroke = md_stroke_color(&md.md_type);
                let polygons: Vec<Vec<Vec<(f64, f64)>>> = md
                    .polygon
                    .iter()
                    .map(|ring| vec![ring.clone()])
                    .collect();
                rustdar_overlays::types::OverlayFeature::new(
                    polygons,
                    fill,
                    stroke,
                    String::new(),
                    String::new(),
                    HatchPattern::None,
                )
            })
            .collect();

        cache.features = build_cached_features(
            &temp_features,
            projector,
            screen_rect,
            false,
            false,
        );
        cache.viewport_key = key;
        cache.data_generation = data_gen;
    }

    let mut clicked_index: Option<usize> = None;
    let painter = ui.painter();
    let mut fill_mesh = egui::Mesh::default();
    let mut strokes: Vec<egui::Shape> = Vec::new();

    for (md_idx, (md, cached_feat)) in discussions.iter().zip(cache.features.iter()).enumerate() {
        let [r, g, b, a] = md_fill_color(&md.md_type);
        let fill = egui::Color32::from_rgba_unmultiplied(r, g, b, a);
        let [sr, sg, sb, sa] = md_stroke_color(&md.md_type);
        let stroke_color = egui::Color32::from_rgba_unmultiplied(sr, sg, sb, sa);

        for cached_poly in &cached_feat.polygons {
            if !screen_rect.intersects(cached_poly.poly_rect) {
                continue;
            }

            // Fill triangles
            if !cached_poly.tri_indices.is_empty() {
                let base = fill_mesh.vertices.len() as u32;
                for pt in &cached_poly.screen_pts {
                    fill_mesh.vertices.push(egui::epaint::Vertex {
                        pos: *pt,
                        uv: egui::epaint::WHITE_UV,
                        color: fill,
                    });
                }
                for &idx in &cached_poly.tri_indices {
                    fill_mesh.indices.push(base + idx);
                }
            }

            // Stroke outline
            if sa > 0 {
                let stroke = egui::Stroke::new(2.0, stroke_color);
                let pts = &cached_poly.screen_pts;
                for i in 0..pts.len() {
                    let j = (i + 1) % pts.len();
                    strokes.push(egui::Shape::line_segment([pts[i], pts[j]], stroke));
                }
            }

            // MD number label at polygon centroid
            if !cached_poly.screen_pts.is_empty() {
                let cx = cached_poly.screen_pts.iter().map(|p| p.x).sum::<f32>()
                    / cached_poly.screen_pts.len() as f32;
                let cy = cached_poly.screen_pts.iter().map(|p| p.y).sum::<f32>()
                    / cached_poly.screen_pts.len() as f32;
                painter.text(
                    egui::pos2(cx, cy),
                    egui::Align2::CENTER_CENTER,
                    format!("MD {}", md.number),
                    egui::FontId::proportional(11.0),
                    stroke_color,
                );
            }

            // Click detection
            if pointer_available && clicked_index.is_none() {
                let clicked = ui.ctx().input(|i| {
                    i.pointer.any_click()
                        && i.pointer.interact_pos().is_some_and(|p| {
                            cached_poly.poly_rect.contains(p)
                                && point_in_polygon(p, &cached_poly.screen_pts)
                        })
                });
                if clicked {
                    clicked_index = Some(md_idx);
                }
            }
        }
    }

    if !fill_mesh.vertices.is_empty() {
        painter.add(egui::Shape::mesh(fill_mesh));
    }
    painter.extend(strokes);

    clicked_index
}

/// Draw NWS weather alert polygons on the map.
///
/// Uses cached projected geometry. The cache stores a flat list of
/// `CachedFeature`s built from all alerts' features in order, so the
/// drawing loop can reconstruct the alert→feature mapping cheaply.
///
/// Returns `Some(alert_index)` if the user clicked on an alert polygon,
/// allowing the caller to open a detail popup.
///
/// This is a free function (not a method on `Gui`) for the same borrow-checker
/// reasons as `draw_spc_overlays`.
fn draw_nws_alerts(
    ui: &egui::Ui,
    projector: &walkers::Projector,
    zoom: f64,
    layers: &crate::layers::LayerManager,
    nws_alerts: &[NwsAlert],
    cache: &mut OverlayLayerCache,
    data_gen: u64,
    pointer_available: bool,
) -> Option<usize> {
    if !layers.any_nws_enabled() || nws_alerts.is_empty() {
        return None;
    }

    let screen_rect = ui.max_rect();
    let key = ViewportKey::from_projector_and_rect(projector, zoom, screen_rect);

    // Rebuild on any viewport or data change.
    // Cache is built for ALL alerts (regardless of enabled categories) so that
    // layer toggles don't require an expensive cache rebuild.
    if !cache.is_valid(&key, data_gen) {
        let mut all_cached: Vec<CachedFeature> = Vec::new();
        for alert in nws_alerts.iter() {
            let cached = build_cached_features(
                &alert.features,
                projector,
                screen_rect,
                false,
                false,
            );
            all_cached.extend(cached);
        }
        cache.features = all_cached;
        cache.viewport_key = key;
        cache.data_generation = data_gen;
    }

    let enabled_categories = layers.enabled_nws_categories();
    let mut clicked_index: Option<usize> = None;
    let painter = ui.painter();
    let mut fill_mesh = egui::Mesh::default();
    let mut strokes: Vec<egui::Shape> = Vec::new();

    // Walk the flat cache in the same alert→feature order it was built.
    let mut flat_idx = 0;
    for (alert_idx, alert) in nws_alerts.iter().enumerate() {
        let skip = !enabled_categories.contains(&alert.category);
        for src_feature in &alert.features {
            if flat_idx >= cache.features.len() {
                break;
            }
            let cached_feat = &cache.features[flat_idx];
            flat_idx += 1;

            if skip {
                continue;
            }

            let [r, g, b, a] = src_feature.fill_rgba;
            let fill = egui::Color32::from_rgba_unmultiplied(r, g, b, a);
            let [sr, sg, sb, sa] = src_feature.stroke_rgba;
            let stroke_color = egui::Color32::from_rgba_unmultiplied(sr, sg, sb, sa);

            for cached_poly in &cached_feat.polygons {
                if !screen_rect.intersects(cached_poly.poly_rect) {
                    continue;
                }

                // Fill triangles
                if !cached_poly.tri_indices.is_empty() {
                    let base = fill_mesh.vertices.len() as u32;
                    for pt in &cached_poly.screen_pts {
                        fill_mesh.vertices.push(egui::epaint::Vertex {
                            pos: *pt,
                            uv: egui::epaint::WHITE_UV,
                            color: fill,
                        });
                    }
                    for &idx in &cached_poly.tri_indices {
                        fill_mesh.indices.push(base + idx);
                    }
                }

                // Stroke outline
                if sa > 0 {
                    let stroke = egui::Stroke::new(2.0, stroke_color);
                    let pts = &cached_poly.screen_pts;
                    for i in 0..pts.len() {
                        let j = (i + 1) % pts.len();
                        strokes.push(egui::Shape::line_segment([pts[i], pts[j]], stroke));
                    }
                }

                // Click detection
                if pointer_available && clicked_index.is_none() {
                    let clicked = ui.ctx().input(|i| {
                        i.pointer.any_click()
                            && i.pointer.interact_pos().is_some_and(|p| {
                                cached_poly.poly_rect.contains(p)
                                    && point_in_polygon(p, &cached_poly.screen_pts)
                            })
                    });
                    if clicked {
                        clicked_index = Some(alert_idx);
                    }
                }
            }
        }
    }

    if !fill_mesh.vertices.is_empty() {
        painter.add(egui::Shape::mesh(fill_mesh));
    }
    painter.extend(strokes);

    clicked_index
}

/// Draw label-only map tiles on top of the radar overlay.
///
/// Uses the same slippy-map tile grid that walkers uses internally so the
/// labels align pixel-perfectly with the base map. Only tiles that intersect
/// the current viewport are fetched / drawn.
fn draw_label_tiles_overlay(
    ui: &egui::Ui,
    projector: &walkers::Projector,
    zoom: f64,
    tiles: &mut HttpTiles,
) {
    let tile_zoom = zoom.round() as u8;
    let n = 2u32.pow(tile_zoom as u32);
    if n == 0 {
        return;
    }

    let screen_rect = ui.max_rect();

    // Determine the visible geographic bounds by unprojecting screen corners.
    let nw = projector.unproject(egui::vec2(screen_rect.left(), screen_rect.top()));
    let se = projector.unproject(egui::vec2(screen_rect.right(), screen_rect.bottom()));

    // walkers Position: x = longitude, y = latitude
    let min_lon = nw.x().min(se.x());
    let max_lon = nw.x().max(se.x());
    let max_lat = nw.y().max(se.y());
    let min_lat = nw.y().min(se.y());

    let min_tx = lon_to_tile_x(min_lon, tile_zoom);
    let max_tx = (lon_to_tile_x(max_lon, tile_zoom) + 1).min(n - 1);
    let min_ty = lat_to_tile_y(max_lat, tile_zoom); // higher lat → smaller tile y
    let max_ty = (lat_to_tile_y(min_lat, tile_zoom) + 1).min(n - 1);

    for ty in min_ty..=max_ty {
        for tx in min_tx..=max_tx {
            let tile_id = TileId {
                x: tx,
                y: ty,
                zoom: tile_zoom,
            };

            if let Some(twuv) = tiles.at(tile_id) {
                // Tile geographic corners
                let nw_lon = tile_to_lon(tx, tile_zoom);
                let nw_lat = tile_to_lat(ty, tile_zoom);
                let se_lon = tile_to_lon(tx + 1, tile_zoom);
                let se_lat = tile_to_lat(ty + 1, tile_zoom);

                let nw_screen = projector
                    .project(walkers::lat_lon(nw_lat, nw_lon))
                    .to_pos2();
                let se_screen = projector
                    .project(walkers::lat_lon(se_lat, se_lon))
                    .to_pos2();
                let rect = egui::Rect::from_two_pos(nw_screen, se_screen);

                let Texture::Raster(ref tex) = twuv.texture;
                ui.painter().image(tex.id(), rect, twuv.uv, egui::Color32::WHITE);
            }
        }
    }
}

/// Format an ISO 8601 timestamp into a shorter human-readable form.
/// Falls back to displaying the raw string on parse errors.
fn format_iso_time(iso: &str) -> String {
    // NWS timestamps look like "2026-03-06T18:00:00-06:00"
    chrono::DateTime::parse_from_rfc3339(iso)
        .map(|dt| dt.format("%b %d %Y %H:%M %Z").to_string())
        .unwrap_or_else(|_| iso.to_string())
}

/// Ray-casting (even-odd rule) point-in-polygon test.
/// Returns `true` if `point` lies inside the polygon defined by `vertices`.
fn point_in_polygon(point: egui::Pos2, vertices: &[egui::Pos2]) -> bool {
    let n = vertices.len();
    if n < 3 {
        return false;
    }
    let mut inside = false;
    let px = point.x;
    let py = point.y;
    let mut j = n - 1;
    for i in 0..n {
        let vi = vertices[i];
        let vj = vertices[j];
        // Check if the ray from point going in +x direction crosses this edge
        if (vi.y > py) != (vj.y > py)
            && px < (vj.x - vi.x) * (py - vi.y) / (vj.y - vi.y) + vi.x
        {
            inside = !inside;
        }
        j = i;
    }
    inside
}
