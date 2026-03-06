use crate::actions::{GuiAction, RadarConfig};
use crate::hatch::draw_hatch;
use crate::layers::{LayerKind, LayerManager};
use chrono::Timelike;
use egui::Context;
use std::collections::HashMap;
use std::sync::Arc;
use walkers::{HttpTiles, MapMemory, Texture, Tiles, sources::{TileSource, Attribution}, TileId};
use rustdar_radar::render::{ImageBounds, RadarProduct, ScanInfo, IMAGE_SIZE, MAX_RANGE_KM};
use rustdar_radar::sites::RADARS;
use rustdar_overlays::spc::outlook::{OutlookDay, OutlookProduct, SpcOutlook};
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
    // Map state
    map_memory: MapMemory,
    tiles_light: Option<HttpTiles>,
    tiles_dark: Option<HttpTiles>,
    current_theme_is_dark: bool, // Track current theme
    // Radar rendering state
    selected_elevation: f32,
    selected_product: RadarProduct,
    radar_image: Option<(egui::TextureHandle, f64, f64, f64, Arc<Vec<f32>>)>, // texture, lat, lon, max_range_km, value_data
    // Cached geographic bounds for the current radar image (recomputed only on site/image change)
    cached_image_bounds: Option<ImageBounds>,
    // Hover state for showing radar values
    hover_value: Option<String>,
    // Cache the last hover screen position to skip recomputation when pointer hasn't moved
    last_hover_pos: Option<egui::Pos2>,
    // City / road label overlay
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
    // Layer management (replaces individual show_* booleans)
    layers: LayerManager,
    // SPC convective outlook data cache
    spc_outlooks: HashMap<(OutlookDay, OutlookProduct), SpcOutlook>,
    // Track when each SPC product was last fetched
    spc_fetch_times: HashMap<(OutlookDay, OutlookProduct), std::time::Instant>,
    // Whether an SPC fetch is currently in flight
    spc_fetching: bool,
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

        // Initialize map memory with a view of the contiguous US
        let mut map_memory = MapMemory::default();
        // Set zoom level to 4 to show the full contiguous US
        // (default is 16 which is very zoomed in)
        let _ = map_memory.set_zoom(4.0);

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
            map_memory,
            tiles_light: None,
            tiles_dark: None,
            current_theme_is_dark: true, // Default to dark theme
            selected_elevation: 0.0,
            selected_product: RadarProduct::Reflectivity,
            radar_image: None,
            cached_image_bounds: None,
            hover_value: None,
            last_hover_pos: None,
            label_tiles_light: None,
            label_tiles_dark: None,
            loading_site: None,
            show_time_dialog: false,
            poll_interval_secs: 60,
            user_location: None,
            layers: LayerManager::new(),
            spc_outlooks: HashMap::new(),
            spc_fetch_times: HashMap::new(),
            spc_fetching: false,
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
            let _ = self.map_memory.set_zoom(7.0);
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
                    ui.checkbox(self.layers.enabled_mut(LayerKind::RadarSites), "Show radar sites");
                    ui.checkbox(self.layers.enabled_mut(LayerKind::CityLabels), "Show city labels");
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

                    // Hover information - expand to fill available space
                    if let Some(hover_info) = &self.hover_value {
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

        // Lazily initialize label-only tiles for city/road name overlay.
        let show_city_labels = self.layers.is_enabled(LayerKind::CityLabels);
        if show_city_labels {
            if is_dark_theme && self.label_tiles_dark.is_none() {
                self.label_tiles_dark = Some(HttpTiles::new(CartoDb::dark_labels(), ctx.to_owned()));
            } else if !is_dark_theme && self.label_tiles_light.is_none() {
                self.label_tiles_light = Some(HttpTiles::new(CartoDb::light_labels(), ctx.to_owned()));
            }
        }

        // Take label tiles out of self to avoid borrow conflicts in the
        // map closure (they are put back after the closure returns).
        let took_dark_labels = show_city_labels && is_dark_theme;
        let took_light_labels = show_city_labels && !is_dark_theme;
        let mut label_tiles: Option<HttpTiles> = if took_dark_labels {
            self.label_tiles_dark.take()
        } else if took_light_labels {
            self.label_tiles_light.take()
        } else {
            None
        };

        // Get a mutable reference to the active tiles
        let tiles = if is_dark_theme {
            self.tiles_dark.as_mut()
        } else {
            self.tiles_light.as_mut()
        };

        egui::CentralPanel::default()
            .frame(egui::Frame::NONE)
            .show(ctx, |ui| {
                // Determine the map center - only set it initially, then MapMemory takes over
                let center = if let Some(scan_info) = &self.scan_info {
                    Position::new(scan_info.site.lon, scan_info.site.lat)
                } else {
                    Position::new(-98.5795, 39.8283) // Geographic center of contiguous USA
                };

                // Clone radar image data for use in closure
                let radar_image = self.radar_image.clone();

                // Clone user location for use in closure
                let user_location = self.user_location;

                // On Android, process double-tap-drag zoom gesture BEFORE building
                // the Map widget so we can suppress panning while zooming.
                #[cfg(target_os = "android")]
                self.double_tap_detector.update(ctx, &mut self.map_memory);

                #[cfg(target_os = "android")]
                let is_zoom_dragging = self.double_tap_detector.is_zooming();
                #[cfg(not(target_os = "android"))]
                let is_zoom_dragging = false;

                // Create and render the map
                if let Some(tiles) = tiles {
                    // MapMemory automatically preserves user's zoom and pan between frames
                    // The center parameter is just used for initialization if no memory exists
                    Map::new(None, &mut self.map_memory, center)
                        .with_layer(tiles, 1.0)
                        .zoom_with_ctrl(false) // Allow scroll to zoom without holding Ctrl
                        .panning(false) // Disable scroll panning
                        // Explicitly disable drag-to-pan during double-tap-drag zoom;
                        // omitting the call would leave the Map's default (PRIMARY) active.
                        .drag_pan_buttons(if is_zoom_dragging {
                            egui::DragPanButtons::empty()
                        } else {
                            egui::DragPanButtons::PRIMARY
                        })
                        .show(ui, |ui, projector, memory| {
                            // Draw SPC outlook polygons (below radar)
                            draw_spc_overlays(
                                ui,
                                projector,
                                &self.layers,
                                &self.spc_outlooks,
                                self.current_theme_is_dark,
                            );

                            // Overlay radar data if available
                            if self.layers.is_enabled(LayerKind::Radar) {
                            if let Some((ref texture, lat, lon, _max_range_km, ref value_data)) =
                                radar_image
                            {
                                // Compute image bounds and the overlay rect FIRST, so hover
                                // lookup can use the rect for pixel indexing.
                                let bounds = self.cached_image_bounds
                                    .unwrap_or_else(|| ImageBounds::from_radar_site(lat, lon));

                                // Project the image corners through the map projector.
                                let nw = projector.project(walkers::lat_lon(bounds.max_lat, bounds.min_lon)).to_pos2();
                                let se = projector.project(walkers::lat_lon(bounds.min_lat, bounds.max_lon)).to_pos2();
                                let rect = egui::Rect::from_two_pos(nw, se);

                                // Hover: look up the radar value under the cursor
                                if let Some(hover_pos) = ui.ctx().pointer_hover_pos() {
                                    // Skip recomputation if the pointer hasn't moved since last frame
                                    let pos_changed = self.last_hover_pos
                                        .map(|last| (last - hover_pos).length() > 0.5)
                                        .unwrap_or(true);
                                    self.last_hover_pos = Some(hover_pos);

                                    if pos_changed {
                                        // Unproject for lat/lon display and haversine
                                        let screen_vec = egui::vec2(hover_pos.x, hover_pos.y);
                                        let map_pos = projector.unproject(screen_vec);
                                        let hover_lat = map_pos.y();
                                        let hover_lon = map_pos.x();

                                        // Haversine distance
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

                                        // Bearing (azimuth)
                                        let y = dlon.sin() * lat2.cos();
                                        let x = lat1.cos() * lat2.sin()
                                            - lat1.sin() * lat2.cos() * dlon.cos();
                                        let azimuth = (y.atan2(x).to_degrees() + 360.0) % 360.0;

                                        // Pixel lookup: use the cursor's fractional position within
                                        // the drawn overlay rect. This matches exactly what the GPU
                                        // does when bilinear-sampling the texture, so the value we
                                        // read corresponds to the color the user sees.
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
                                                    let product = self.selected_product;
                                                    value_str = match product {
                                                        RadarProduct::Velocity => {
                                                            let mph = value * 2.23694;
                                                            format!(" | Velocity: {:.1} mph", mph)
                                                        }
                                                        RadarProduct::Reflectivity => {
                                                            format!(" | Reflectivity: {:.1} dBZ", value)
                                                        }
                                                        _ => format!(" | Value: {:.2}", value),
                                                    };
                                                }
                                            }
                                        }

                                        self.hover_value = Some(format!(
                                            "Lat: {:.4}°, Lon: {:.4}° | Range: {:.1}km, Az: {:.1}° {}",
                                            hover_lat, hover_lon, distance_km, azimuth, value_str
                                        ));
                                    }
                                    // else: pointer hasn't moved, keep existing self.hover_value
                                } else {
                                    self.last_hover_pos = None;
                                    self.hover_value = None;
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

                            // Draw label-only tiles on top of the radar overlay
                            if let Some(ref mut ltiles) = label_tiles {
                                draw_label_tiles_overlay(ui, projector, memory.zoom(), ltiles);
                            }

                            // Draw radar site icons on the topmost layer
                            if self.layers.is_enabled(LayerKind::RadarSites) {
                                for radar_site in &RADARS {
                                    let site_position = walkers::lat_lon(radar_site.lat, radar_site.lon);
                                    let site_screen = projector.project(site_position).to_pos2();
                                    
                                    // Skip sites that are outside the visible area to improve performance
                                    let screen_rect = ui.max_rect();
                                    if !screen_rect.expand(100.0).contains(site_screen) {
                                        continue;
                                    }

                                    // Determine icon size based on zoom level
                                    let zoom = memory.zoom() as f32;
                                    let icon_size = (10.0 + zoom * 2.0).clamp(8.0, 24.0);
                                    
                                    // Choose icon color based on site state
                                    let is_current_site = self.scan_info.as_ref()
                                        .map(|info| info.site.name == radar_site.name)
                                        .unwrap_or(false);
                                    
                                    let is_loading = self.loading_site.as_ref()
                                        .map(|loading| loading == radar_site.name)
                                        .unwrap_or(false);
                                    
                                    let icon_color = if is_loading {
                                        egui::Color32::from_rgb(160, 32, 240) // Purple for loading
                                    } else if is_current_site {
                                        egui::Color32::from_rgb(255, 100, 100) // Red for current site
                                    } else {
                                        egui::Color32::from_rgb(100, 150, 255) // Blue for other sites
                                    };

                                    // Draw the radar site icon
                                    let icon_rect = egui::Rect::from_center_size(
                                        site_screen,
                                        egui::vec2(icon_size, icon_size)
                                    );

                                    // Make it clickable
                                    let response = ui.allocate_rect(icon_rect, egui::Sense::click());
                                    
                                    if response.clicked() {
                                        // Immediately set this site as loading
                                        self.loading_site = Some(radar_site.name.to_string());
                                        // Switch to this radar site
                                        actions.push(GuiAction::SwitchRadarSite(radar_site.name.to_string()));
                                    }

                                    // Draw the icon circle
                                    ui.painter().circle_filled(site_screen, icon_size / 2.0, icon_color);
                                    
                                    // Add a border
                                    ui.painter().circle_stroke(
                                        site_screen, 
                                        icon_size / 2.0, 
                                        egui::Stroke::new(1.5, egui::Color32::WHITE)
                                    );

                                    // Draw site name text with theme-appropriate color
                                    let text_color = if is_dark_theme {
                                        egui::Color32::WHITE
                                    } else {
                                        egui::Color32::BLACK
                                    };
                                    let font_size = (icon_size * 0.6).clamp(8.0, 12.0);

                                    // Position text slightly below the icon
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

                                    // Show tooltip on hover
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

                                // Only draw if the position is near the visible area
                                let screen_rect = ui.max_rect();
                                if screen_rect.expand(50.0).contains(user_screen) {
                                    // Semi-transparent accuracy circle
                                    ui.painter().circle_filled(
                                        user_screen,
                                        14.0,
                                        egui::Color32::from_rgba_unmultiplied(30, 130, 255, 40),
                                    );
                                    // White ring
                                    ui.painter().circle_stroke(
                                        user_screen,
                                        7.0,
                                        egui::Stroke::new(2.5, egui::Color32::WHITE),
                                    );
                                    // Blue dot
                                    ui.painter().circle_filled(
                                        user_screen,
                                        7.0,
                                        egui::Color32::from_rgb(30, 130, 255),
                                    );
                                }
                            }
                        });
                }
            });

        // Restore label tiles back into self
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
        let day = self.layers.spc_day;

        egui::SidePanel::left("layers_panel")
            .default_width(170.0)
            .resizable(false)
            .show(ctx, |ui| {
                ui.heading("Layers");
                ui.separator();

                // --- Radar layer ---
                ui.checkbox(self.layers.enabled_mut(LayerKind::Radar), "🛰  Radar");

                if self.layers.is_enabled(LayerKind::Radar) {
                    ui.indent("radar_controls", |ui| {
                        if let Some(scan_info) = &self.scan_info {
                            // Product selector
                            let prev_product = self.selected_product;
                            egui::ComboBox::from_id_salt("layer_product_selector")
                                .selected_text(self.selected_product.name())
                                .width(120.0)
                                .show_ui(ui, |ui| {
                                    for product in &scan_info.available_products {
                                        ui.selectable_value(
                                            &mut self.selected_product,
                                            *product,
                                            product.name(),
                                        );
                                    }
                                });
                            if prev_product != self.selected_product {
                                self.selected_elevation = 0.0;
                            }

                            // Elevation selector
                            if let Some(elevations) =
                                scan_info.product_elevations.get(&self.selected_product)
                            {
                                if !elevations.is_empty() {
                                    let selected_angle = elevations
                                        .iter()
                                        .min_by(|a, b| {
                                            ((**a - self.selected_elevation).abs())
                                                .partial_cmp(
                                                    &((**b - self.selected_elevation).abs()),
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
                                                    &mut self.selected_elevation,
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
                ui.horizontal(|ui| {
                    ui.label("Day:");
                    let mut changed = false;
                    let mut new_day = self.layers.spc_day;
                    if ui.selectable_label(new_day == OutlookDay::Day1, "1").clicked() {
                        new_day = OutlookDay::Day1;
                        changed = true;
                    }
                    if ui.selectable_label(new_day == OutlookDay::Day2, "2").clicked() {
                        new_day = OutlookDay::Day2;
                        changed = true;
                    }
                    if ui.selectable_label(new_day == OutlookDay::Day3, "3").clicked() {
                        new_day = OutlookDay::Day3;
                        changed = true;
                    }
                    if changed {
                        self.layers.spc_day = new_day;
                        // Fetch data for the new day if any SPC layers are enabled
                        let products = self.layers.enabled_spc_products();
                        if !products.is_empty() {
                            actions.push(GuiAction::FetchSpcOutlook {
                                day: new_day,
                                products,
                            });
                        }
                    }
                });

                // Product toggles (depends on selected day)
                let spc_layers = self.layers.spc_layers_for_day();
                for layer in &spc_layers {
                    let was_enabled = self.layers.is_enabled(*layer);
                    ui.checkbox(self.layers.enabled_mut(*layer), layer.display_name());
                    let is_enabled = self.layers.is_enabled(*layer);

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
                if self.layers.any_spc_enabled() {
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

                // --- Other overlays ---
                ui.checkbox(self.layers.enabled_mut(LayerKind::CityLabels), "🏷  City Labels");
                ui.checkbox(self.layers.enabled_mut(LayerKind::RadarSites), "📡  Radar Sites");
            });

        actions
    }

    /// Get the selected product and elevation for rendering
    pub fn get_rendering_params(&self) -> Option<(RadarProduct, f32)> {
        self.scan_info.as_ref().and_then(|scan_info| {
            scan_info
                .product_elevations
                .get(&self.selected_product)
                .and_then(|elevations| {
                    // Find closest matching elevation angle
                    elevations.iter()
                        .min_by(|a, b| {
                            ((**a - self.selected_elevation).abs())
                                .partial_cmp(&((**b - self.selected_elevation).abs()))
                                .unwrap()
                        })
                        .copied()
                })
                .map(|elev_angle| (self.selected_product, elev_angle))
        })
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
    }

    /// Set whether an SPC fetch is currently in progress.
    pub fn set_spc_fetching(&mut self, fetching: bool) {
        self.spc_fetching = fetching;
    }

    /// Get the layer manager (immutable).
    pub fn layers(&self) -> &LayerManager {
        &self.layers
    }

    /// Get the layer manager (mutable).
    pub fn layers_mut(&mut self) -> &mut LayerManager {
        &mut self.layers
    }

    /// Get the current scan info
    pub fn get_scan_info(&self) -> Option<&ScanInfo> {
        self.scan_info.as_ref()
    }

    /// Get the current radar image
    pub fn get_radar_image(&self) -> &Option<(egui::TextureHandle, f64, f64, f64, Arc<Vec<f32>>)> {
        &self.radar_image
    }

    /// Take the radar image, removing it from the GUI
    pub fn take_radar_image(&mut self) -> Option<(egui::TextureHandle, f64, f64, f64, Arc<Vec<f32>>)> {
        self.radar_image.take()
    }

    /// Set the radar image to display on the map
    pub fn set_radar_image(
        &mut self,
        texture: egui::TextureHandle,
        lat: f64,
        lon: f64,
        max_range_km: f64,
        value_data: Vec<f32>,
    ) {
        self.radar_image = Some((texture, lat, lon, max_range_km, Arc::new(value_data)));
        self.cached_image_bounds = Some(ImageBounds::from_radar_site(lat, lon));
    }

    /// Clear the radar image
    pub fn clear_radar_image(&mut self) {
        self.radar_image = None;
        self.cached_image_bounds = None;
    }

    /// Whether auto-poll is active and the event loop should keep waking
    pub fn is_auto_poll_active(&self) -> bool {
        self.auto_poll_enabled && self.initial_fetch_done
    }

    pub fn clear_graphics_state(&mut self) {
        self.radar_image = None;
        self.tiles_light = None;
        self.tiles_dark = None;
        self.label_tiles_light = None;
        self.label_tiles_dark = None;
        self.loading_site = None;
        // Reset theme tracking to force tile recreation on next render
        self.current_theme_is_dark = !self.current_theme_is_dark;
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
        let day = self.layers.spc_day;

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
                    // ── Radar ──
                    ui.checkbox(self.layers.enabled_mut(LayerKind::Radar), "🛰  Radar");

                    if self.layers.is_enabled(LayerKind::Radar) {
                        ui.indent("m_radar_controls", |ui| {
                            if let Some(scan_info) = &self.scan_info {
                                let prev_product = self.selected_product;
                                egui::ComboBox::from_id_salt("m_product_sel")
                                    .selected_text(self.selected_product.name())
                                    .width(180.0)
                                    .show_ui(ui, |ui| {
                                        for product in &scan_info.available_products {
                                            ui.selectable_value(
                                                &mut self.selected_product,
                                                *product,
                                                product.name(),
                                            );
                                        }
                                    });
                                if prev_product != self.selected_product {
                                    self.selected_elevation = 0.0;
                                }

                                if let Some(elevations) =
                                    scan_info.product_elevations.get(&self.selected_product)
                                {
                                    if !elevations.is_empty() {
                                        let selected_angle = elevations
                                            .iter()
                                            .min_by(|a, b| {
                                                ((**a - self.selected_elevation).abs())
                                                    .partial_cmp(
                                                        &((**b - self.selected_elevation).abs()),
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
                                                        &mut self.selected_elevation,
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

                    ui.horizontal(|ui| {
                        ui.label("Day:");
                        let mut changed = false;
                        let mut new_day = self.layers.spc_day;
                        for (d, label) in [
                            (OutlookDay::Day1, "1"),
                            (OutlookDay::Day2, "2"),
                            (OutlookDay::Day3, "3"),
                        ] {
                            if ui.selectable_label(new_day == d, label).clicked() {
                                new_day = d;
                                changed = true;
                            }
                        }
                        if changed {
                            self.layers.spc_day = new_day;
                            let products = self.layers.enabled_spc_products();
                            if !products.is_empty() {
                                actions.push(GuiAction::FetchSpcOutlook {
                                    day: new_day,
                                    products,
                                });
                            }
                        }
                    });

                    let spc_layers = self.layers.spc_layers_for_day();
                    for layer in &spc_layers {
                        let was_enabled = self.layers.is_enabled(*layer);
                        ui.checkbox(self.layers.enabled_mut(*layer), layer.display_name());
                        let is_enabled = self.layers.is_enabled(*layer);
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

                    if self.layers.any_spc_enabled() {
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

                    // ── Other overlays ──
                    ui.checkbox(self.layers.enabled_mut(LayerKind::CityLabels), "🏷  City Labels");
                    ui.checkbox(self.layers.enabled_mut(LayerKind::RadarSites), "📡  Radar Sites");

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
/// This is a free function (not a method on `Gui`) to avoid capturing the
/// entire `self` struct inside the map closure, which would conflict with
/// disjoint field borrows required by Rust's borrow checker.
fn draw_spc_overlays(
    ui: &egui::Ui,
    projector: &walkers::Projector,
    layers: &crate::layers::LayerManager,
    spc_outlooks: &HashMap<(OutlookDay, OutlookProduct), SpcOutlook>,
    current_theme_is_dark: bool,
) {
    let screen_rect = ui.max_rect();
    let day = layers.spc_day;

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

        // Draw features from lowest risk to highest (features are ordered low→high in GeoJSON)
        for feature in &outlook.features {
            for polygon in &feature.polygons {
                if polygon.is_empty() {
                    continue;
                }
                // Use exterior ring (first ring)
                let exterior = &polygon[0];
                if exterior.len() < 3 {
                    continue;
                }

// Strip the GeoJSON closing duplicate (last == first)
                    // since PathShape { closed: true } already closes the ring.
                    // Keeping it causes degenerate edges that fold the polygon.
                    let ring = if exterior.len() > 3
                        && exterior.first() == exterior.last()
                    {
                        &exterior[..exterior.len() - 1]
                    } else {
                        exterior.as_slice()
                    };

                    // Project to screen coordinates
                    let screen_pts: Vec<egui::Pos2> = ring
                    .iter()
                    .map(|&(lat, lon)| {
                        projector
                            .project(walkers::lat_lon(lat, lon))
                            .to_pos2()
                    })
                    .collect();

                // Quick AABB visibility check
                let mut min_x = f32::MAX;
                let mut min_y = f32::MAX;
                let mut max_x = f32::MIN;
                let mut max_y = f32::MIN;
                for pt in &screen_pts {
                    min_x = min_x.min(pt.x);
                    min_y = min_y.min(pt.y);
                    max_x = max_x.max(pt.x);
                    max_y = max_y.max(pt.y);
                }
                let poly_rect = egui::Rect::from_min_max(
                    egui::pos2(min_x, min_y),
                    egui::pos2(max_x, max_y),
                );
                if !screen_rect.intersects(poly_rect) {
                    continue;
                }

                // Draw filled polygon using ear-clipping triangulation.
                // egui's PathShape uses a centroid triangle-fan which only works
                // for convex shapes; SPC polygons are deeply concave.
                let [r, g, b, a] = feature.fill_rgba;
                let fill = egui::Color32::from_rgba_unmultiplied(r, g, b, a);
                let [sr, sg, sb, sa] = feature.stroke_rgba;
                let stroke_color = egui::Color32::from_rgba_unmultiplied(sr, sg, sb, sa);

                // Flatten screen points into [x0, y0, x1, y1, ...] for earcutr
                let coords: Vec<f64> = screen_pts
                    .iter()
                    .flat_map(|p| [p.x as f64, p.y as f64])
                    .collect();
                let tri_indices = earcutr::earcut(&coords, &[], 2).unwrap_or_default();
                if !tri_indices.is_empty() {
                    let mut mesh = egui::Mesh::default();
                    for pt in &screen_pts {
                        mesh.vertices.push(egui::epaint::Vertex {
                            pos: *pt,
                            uv: egui::epaint::WHITE_UV,
                            color: fill,
                        });
                    }
                    for idx in tri_indices {
                        mesh.indices.push(idx as u32);
                    }
                    ui.painter().add(egui::Shape::mesh(mesh));
                }

                // Draw stroke outline separately (PathShape handles this fine)
                if sa > 0 {
                    let outline = egui::epaint::PathShape {
                        points: screen_pts.clone(),
                        closed: true,
                        fill: egui::Color32::TRANSPARENT,
                        stroke: egui::epaint::PathStroke::new(1.5, stroke_color),
                    };
                    ui.painter().add(egui::Shape::Path(outline));
                }

                // Draw CIG hatching if applicable
                if feature.hatch != HatchPattern::None {
                    draw_hatch(
                        ui.painter(),
                        &screen_pts,
                        feature.hatch,
                        screen_rect,
                        current_theme_is_dark,
                    );
                }
            }
        }
    }
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
