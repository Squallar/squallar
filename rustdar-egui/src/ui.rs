use crate::actions::{GuiAction, RadarConfig};
use chrono::Timelike;
use egui::Context;
use walkers::{HttpTiles, MapMemory};
use rustdar_radar::render::{RadarProduct, ScanInfo};
use rustdar_radar::sites::RADARS;

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
    tiles: Option<HttpTiles>,
    // Radar rendering state
    selected_elevation: usize,
    selected_product: RadarProduct,
    radar_image: Option<(egui::TextureHandle, f64, f64, f32, Vec<f32>)>, // texture, lat, lon, max_range_km, value_data
    // Hover state for showing radar values
    hover_value: Option<String>,
    // Site display settings
    show_radar_sites: bool,
    // Track which site is currently loading
    loading_site: Option<String>,
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
            tiles: None,
            selected_elevation: 0,
            selected_product: RadarProduct::Reflectivity,
            radar_image: None,
            hover_value: None,
            show_radar_sites: false,
            loading_site: None,
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

        // Poll for new scans every 60 seconds
        if self.auto_poll_enabled
            && !self.fetching
            && let Some(last_fetch) = self.last_fetch_time
            && last_fetch.elapsed().as_secs() >= 60
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

        // Menu bar
        let mut action = None;
        self.render_menu_bar(ctx, &mut action);
        if let Some(a) = action {
            actions.push(a);
        }

        // Bottom status bar - render first so it spans the full width
        self.render_status_bar(ctx);

        // Left panel for radar controls
        let mut action = None;
        self.render_radar_panel(ctx, &mut action);
        if let Some(a) = action {
            actions.push(a);
        }

        // Right panel for radar display controls
        self.render_radar_display_panel(ctx);

        // Map in central panel
        let map_actions = self.render_map(ctx);
        actions.extend(map_actions);

        actions
    }

    /// Update the scan info (called from the app when scan is loaded)
    pub fn set_scan_info(&mut self, info: ScanInfo) {
        self.scan_info = Some(info);
        self.fetching = false;

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
    }

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
                    ui.checkbox(&mut self.auto_poll_enabled, "Auto-poll for new scans");
                    ui.checkbox(&mut self.show_radar_sites, "Show radar sites");
                });
            });
        });
    }

    fn render_radar_panel(&mut self, ctx: &Context, action: &mut Option<GuiAction>) {
        egui::SidePanel::left("radar_config_panel")
            .default_width(300.0)
            .show(ctx, |ui| {
                ui.heading("Radar Configuration");
                ui.separator();

                ui.horizontal(|ui| {
                    ui.label("Site Code:");
                    ui.text_edit_singleline(&mut self.radar_config.site);
                });

                ui.add_space(10.0);

                ui.label("Timestamp (local):");
                ui.horizontal(|ui| {
                    ui.label("Date:");
                    if ui.text_edit_singleline(&mut self.date_string).changed() {
                        self.update_timestamp_from_strings();
                    }
                });

                ui.horizontal(|ui| {
                    ui.label("Time:");
                    if ui.text_edit_singleline(&mut self.time_string).changed() {
                        self.update_timestamp_from_strings();
                    }
                });

                ui.add_space(10.0);

                if ui.button("Use Current Time").clicked() {
                    self.radar_config.timestamp = chrono::Local::now().naive_local();
                    self.date_string = self.radar_config.timestamp.format("%Y-%m-%d").to_string();
                    self.time_string = self.radar_config.timestamp.format("%H:%M:%S").to_string();
                }

                ui.add_space(10.0);
                ui.separator();

                let fetch_button = ui.add_enabled(
                    !self.fetching,
                    egui::Button::new(if self.fetching {
                        "Fetching..."
                    } else {
                        "Fetch Radar Scan"
                    }),
                );

                if fetch_button.clicked() {
                    self.fetching = true;
                    *action = Some(GuiAction::FetchRadarScan(self.radar_config.clone()));
                }
            });
    }

    fn update_timestamp_from_strings(&mut self) {
        // Try to parse the date and time strings
        let datetime_str = format!("{} {}", self.date_string, self.time_string);
        if let Ok(timestamp) =
            chrono::NaiveDateTime::parse_from_str(&datetime_str, "%Y-%m-%d %H:%M:%S")
        {
            self.radar_config.timestamp = timestamp;
        }
    }

    fn render_radar_display_panel(&mut self, ctx: &Context) {
        egui::SidePanel::right("radar_display_panel")
            .default_width(200.0)
            .show(ctx, |ui| {
                ui.heading("Display");
                ui.separator();

                if let Some(scan_info) = &self.scan_info {
                    // Product selector
                    ui.label("Product:");

                    let prev_product = self.selected_product;

                    egui::ComboBox::from_id_salt("product_selector")
                        .selected_text(self.selected_product.name())
                        .show_ui(ui, |ui| {
                            for product in &scan_info.available_products {
                                ui.selectable_value(
                                    &mut self.selected_product,
                                    *product,
                                    product.name(),
                                );
                            }
                        });

                    // Reset elevation to 0 if product changed
                    if prev_product != self.selected_product {
                        self.selected_elevation = 0;
                    }

                    ui.add_space(10.0);

                    // Elevation selector - shows elevations for the selected product
                    ui.label("Elevation:");

                    if let Some(elevations) =
                        scan_info.product_elevations.get(&self.selected_product)
                    {
                        if !elevations.is_empty() {
                            // Clamp selected_elevation to valid range
                            if self.selected_elevation >= elevations.len() {
                                self.selected_elevation = 0;
                            }

                            let selected_angle = elevations
                                .get(self.selected_elevation)
                                .copied()
                                .unwrap_or(0.0);

                            egui::ComboBox::from_id_salt("elevation_selector")
                                .selected_text(format!("{:.1}°", selected_angle))
                                .show_ui(ui, |ui| {
                                    for (i, angle) in elevations.iter().enumerate() {
                                        ui.selectable_value(
                                            &mut self.selected_elevation,
                                            i,
                                            format!("{:.1}°", angle),
                                        );
                                    }
                                });

                            ui.add_space(10.0);
                            ui.separator();
                            ui.label(format!("Showing: {}", self.selected_product.name()));
                            ui.label(format!("Elevation: {:.1}°", selected_angle));
                            ui.label(format!("{} tilts available", elevations.len()));
                        } else {
                            ui.label("No elevations available for this product");
                        }
                    } else {
                        ui.label("Product not available in scan");
                    }
                } else {
                    ui.label("No scan loaded");
                    ui.separator();
                    ui.label("Load a radar scan to view products.");
                }
            });
    }

    fn render_status_bar(&mut self, ctx: &Context) {
        egui::TopBottomPanel::bottom("status_bar")
            .show_separator_line(true)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 8.0;

                    // Status indicator
                    if self.fetching {
                        ui.label("🔄");
                        ui.label("Downloading");
                        ui.spinner();
                    } else if self.auto_poll_enabled {
                        // Show time until next poll
                        if let Some(last_fetch) = self.last_fetch_time {
                            let elapsed = last_fetch.elapsed().as_secs();
                            let remaining = 60_u64.saturating_sub(elapsed);
                            ui.label("🔁");
                            ui.label(format!("Auto-polling (next in {}s)", remaining));
                        } else {
                            ui.label("🔁");
                            ui.label("Auto-polling");
                        }
                    } else {
                        ui.label("⏸");
                        ui.label("Idle");
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
                        if let Some(error_msg) = self.error_message.clone() {
                            if ui.button("✕").clicked() {
                                self.error_message = None;
                            }
                            ui.label(&error_msg);
                            ui.label("❌");
                        }
                    });
                });
            });
    }

    fn render_map(&mut self, ctx: &Context) -> Vec<GuiAction> {
        use walkers::{Map, Position, sources::OpenStreetMap};

        let mut actions = Vec::new();

        // Initialize tiles on first use
        if self.tiles.is_none() {
            self.tiles = Some(HttpTiles::new(OpenStreetMap, ctx.to_owned()));
        }

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

                // Reset hover value for this frame
                self.hover_value = None;

                // Create and render the map
                if let Some(tiles) = &mut self.tiles {
                    // MapMemory automatically preserves user's zoom and pan between frames
                    // The center parameter is just used for initialization if no memory exists
                    Map::new(None, &mut self.map_memory, center)
                        .with_layer(tiles, 1.0)
                        .zoom_with_ctrl(false) // Allow scroll to zoom without holding Ctrl
                        .panning(false) // Disable scroll panning
                        .drag_pan_buttons(egui::DragPanButtons::PRIMARY) // Left-click drag to pan
                        .show(ui, |ui, projector, memory| {
                            if self.show_radar_sites {
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

                                    // Draw site name text (use black for readability on the current map)
                                    let text_color = egui::Color32::BLACK;
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
                                        let tooltip_text = format!("{}\nLat: {:.3}°, Lon: {:.3}°\nElev: {} ft", 
                                            radar_site.name, radar_site.lat, radar_site.lon, radar_site.elev);
                                        response.on_hover_text(tooltip_text);
                                    }
                                }
                            }

                            // Overlay radar data if available
                            if let Some((ref texture, lat, lon, max_range_km, ref value_data)) =
                                radar_image
                            {
                                // Detect hover position and look up radar value
                                if let Some(hover_pos) = ui.ctx().pointer_hover_pos() {
                                    // Convert screen position to map position
                                    let screen_vec = egui::vec2(hover_pos.x, hover_pos.y);
                                    let map_pos = projector.unproject(screen_vec);
                                    let hover_lat = map_pos.y();
                                    let hover_lon = map_pos.x();

                                    // Calculate bearing and distance from radar to hover point
                                    let lat1 = lat.to_radians();
                                    let lon1 = lon.to_radians();
                                    let lat2 = hover_lat.to_radians();
                                    let lon2 = hover_lon.to_radians();

                                    // Haversine formula for distance
                                    let dlat = lat2 - lat1;
                                    let dlon = lon2 - lon1;
                                    let a = (dlat / 2.0).sin().powi(2)
                                        + lat1.cos() * lat2.cos() * (dlon / 2.0).sin().powi(2);
                                    let c = 2.0 * a.sqrt().atan2((1.0 - a).sqrt());
                                    let distance_km = 6371.0 * c; // Earth radius in km

                                    // Calculate bearing (azimuth)
                                    let y = dlon.sin() * lat2.cos();
                                    let x = lat1.cos() * lat2.sin()
                                        - lat1.sin() * lat2.cos() * dlon.cos();
                                    let bearing_rad = y.atan2(x);
                                    let bearing_deg = bearing_rad.to_degrees();
                                    let azimuth = (bearing_deg + 360.0) % 360.0;

                                    // Convert to radar image coordinates (1800x1800 pixels)
                                    const IMAGE_SIZE: f64 = 1800.0;
                                    const MAX_RANGE_KM: f64 = 230.0;
                                    let pixels_per_km = IMAGE_SIZE / (2.0 * MAX_RANGE_KM);

                                    // Convert polar to cartesian (0° = North, clockwise)
                                    let x_offset = distance_km * azimuth.to_radians().sin();
                                    let y_offset = -distance_km * azimuth.to_radians().cos();

                                    // Convert to pixel coordinates
                                    let px = ((IMAGE_SIZE / 2.0) + x_offset * pixels_per_km) as i32;
                                    let py = ((IMAGE_SIZE / 2.0) + y_offset * pixels_per_km) as i32;

                                    // Look up value if within bounds
                                    let mut value_str = String::new();
                                    if px >= 0
                                        && px < IMAGE_SIZE as i32
                                        && py >= 0
                                        && py < IMAGE_SIZE as i32
                                    {
                                        let pixel_idx =
                                            (py as usize * IMAGE_SIZE as usize) + px as usize;
                                        if pixel_idx < value_data.len() {
                                            let value = value_data[pixel_idx];
                                            if !value.is_nan() {
                                                // Format value based on product type
                                                let product = self.selected_product;
                                                value_str = match product {
                                                    RadarProduct::Velocity => {
                                                        let mph = value * 2.23694; // m/s to mph
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
                                        "Lat: {:.4}°, Lon: {:.4}° | Range: {:.1}km, Az: {:.1}° | Px: {},{} {}",
                                        hover_lat, hover_lon, distance_km, azimuth, px, py, value_str
                                    ));
                                }
                                // Project radar center to screen coordinates
                                let radar_center = walkers::lat_lon(lat, lon);
                                let center_screen = projector.project(radar_center).to_pos2();

                                // Calculate size using map zoom level
                                // At zoom level z, there are 2^z tiles across the world (256px each)
                                // The world width in pixels at this zoom is: 256 * 2^z
                                // Earth's circumference at equator: ~40,075 km
                                // So pixels per km = (256 * 2^z) / 40075
                                let zoom = memory.zoom() as f32;
                                let tiles_across = 2.0_f32.powf(zoom);
                                let world_width_pixels = 256.0 * tiles_across;
                                let earth_circumference_km = 40075.0;
                                let pixels_per_km = world_width_pixels / earth_circumference_km;

                                // Adjust for latitude (map is Mercator projection)
                                // Scale factor varies with latitude
                                let lat_rad = (lat as f32).to_radians();
                                let scale_factor = 1.0 / lat_rad.cos();

                                let image_size_pixels =
                                    max_range_km * 2.0 * pixels_per_km * scale_factor;

                                // Draw the radar image centered on the radar site
                                let rect = egui::Rect::from_center_size(
                                    center_screen,
                                    egui::vec2(image_size_pixels, image_size_pixels),
                                );

                                ui.painter().image(
                                    texture.id(),
                                    rect,
                                    egui::Rect::from_min_max(
                                        egui::pos2(0.0, 0.0),
                                        egui::pos2(1.0, 1.0),
                                    ),
                                    egui::Color32::WHITE,
                                );

                                // Draw a light grey circle showing the actual radar range
                                let range_radius_pixels = max_range_km * pixels_per_km * scale_factor;
                                ui.painter().circle_stroke(
                                    center_screen,
                                    range_radius_pixels,
                                    egui::Stroke::new(1.0, egui::Color32::from_rgba_unmultiplied(150, 150, 150, 80)),
                                );
                            }
                        });
                }
            });

        actions
    }

    /// Get the selected product and elevation for rendering
    pub fn get_rendering_params(&self) -> Option<(RadarProduct, f32)> {
        self.scan_info.as_ref().and_then(|scan_info| {
            scan_info
                .product_elevations
                .get(&self.selected_product)
                .and_then(|elevations| elevations.get(self.selected_elevation).copied())
                .map(|elev_angle| (self.selected_product, elev_angle))
        })
    }

    /// Get the current radar config
    pub fn get_radar_config(&self) -> &RadarConfig {
        &self.radar_config
    }

    /// Set the radar config
    pub fn set_radar_config(&mut self, config: RadarConfig) {
        self.radar_config = config.clone();
        // Update the date/time strings to match the new config
        self.date_string = config.timestamp.format("%Y-%m-%d").to_string();
        self.time_string = config.timestamp.format("%H:%M:%S").to_string();
    }

    /// Set which site is currently loading
    pub fn set_loading_site(&mut self, site: Option<String>) {
        self.loading_site = site;
    }

    /// Get the current scan info
    pub fn get_scan_info(&self) -> Option<&ScanInfo> {
        self.scan_info.as_ref()
    }

    /// Get the current radar image
    pub fn get_radar_image(&self) -> &Option<(egui::TextureHandle, f64, f64, f32, Vec<f32>)> {
        &self.radar_image
    }

    /// Take the radar image, removing it from the GUI
    pub fn take_radar_image(&mut self) -> Option<(egui::TextureHandle, f64, f64, f32, Vec<f32>)> {
        self.radar_image.take()
    }

    /// Set the radar image to display on the map
    pub fn set_radar_image(
        &mut self,
        texture: egui::TextureHandle,
        lat: f64,
        lon: f64,
        max_range_km: f32,
        value_data: Vec<f32>,
    ) {
        self.radar_image = Some((texture, lat, lon, max_range_km, value_data));
    }

    /// Clear the radar image
    pub fn clear_radar_image(&mut self) {
        self.radar_image = None;
    }

    pub fn clear_graphics_state(&mut self) {
        self.radar_image = None;
        self.tiles = None;
        self.loading_site = None;
    }
}
