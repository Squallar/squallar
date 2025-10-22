use crate::actions::{GuiAction, RadarConfig, ScanInfo};
use egui::Context;
use walkers::{HttpTiles, MapMemory};

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
    // Map state
    map_memory: MapMemory,
    tiles: Option<HttpTiles>,
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
            map_memory,
            tiles: None,
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
        if self.auto_poll_enabled && !self.fetching {
            if let Some(last_fetch) = self.last_fetch_time {
                if last_fetch.elapsed().as_secs() >= 60 {
                    self.fetching = true;
                    self.last_fetch_time = Some(std::time::Instant::now());
                    actions.push(GuiAction::FetchRadarScan(self.radar_config.clone()));
                }
            }
        }

        // Menu bar
        let mut action = None;
        self.render_menu_bar(ctx, &mut action);
        if let Some(a) = action {
            actions.push(a);
        }

        // Left panel for radar controls
        let mut action = None;
        self.render_radar_panel(ctx, &mut action);
        if let Some(a) = action {
            actions.push(a);
        }

        // Map in central panel
        self.render_map(ctx);

        // Bottom status bar
        self.render_status_bar(ctx);

        actions
    }

    /// Update the scan info (called from the app when scan is loaded)
    pub fn set_scan_info(&mut self, info: ScanInfo) {
        self.scan_info = Some(info);
        self.fetching = false;

        // Zoom to a good level for viewing radar data when a scan loads
        let _ = self.map_memory.set_zoom(7.0);
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

    fn render_status_bar(&mut self, ctx: &Context) {
        egui::TopBottomPanel::bottom("status_bar")
            .show_separator_line(true)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
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
                            "Scan: {} @ {} UTC ({} elevations)",
                            scan_info.site.name,
                            scan_info.timestamp.format("%Y-%m-%d %H:%M:%S"),
                            scan_info.num_elevations
                        ));
                    } else {
                        ui.label("No scan loaded");
                    }

                    ui.separator();

                    // Error message (if any)
                    if let Some(error_msg) = &self.error_message {
                        ui.label("❌");
                        ui.label(error_msg);
                        if ui.button("✕").clicked() {
                            self.error_message = None;
                        }
                    }
                });
            });
    }

    fn render_map(&mut self, ctx: &Context) {
        use walkers::{Map, Position, sources::OpenStreetMap};

        // Initialize tiles on first use
        if self.tiles.is_none() {
            self.tiles = Some(HttpTiles::new(OpenStreetMap, ctx.to_owned()));
        }

        egui::CentralPanel::default().show(ctx, |ui| {
            // Center on radar site if we have scan info, otherwise default to center of USA
            let center = if let Some(scan_info) = &self.scan_info {
                Position::new(scan_info.site.lon, scan_info.site.lat)
            } else {
                Position::new(-98.5795, 39.8283) // Geographic center of contiguous USA
            };

            // Create and render the map
            if let Some(tiles) = &mut self.tiles {
                Map::new(None, &mut self.map_memory, center)
                    .with_layer(tiles, 1.0)
                    .zoom_with_ctrl(false) // Allow scroll to zoom without holding Ctrl
                    .panning(false) // Disable scroll panning
                    .drag_pan_buttons(egui::DragPanButtons::PRIMARY) // Left-click drag to pan
                    .show(ui, |_ui, _projector, _memory| {
                        // Future: overlay radar data here
                    });
            }
        });
    }
}
