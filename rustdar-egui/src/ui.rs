use crate::actions::{GuiAction, RadarConfig};
use crate::layers::{LayerKind, LayerManager};
use crate::overlay_state::OverlayData;
use crate::pane::{PaneId, PaneLayout, PaneState, RadarImageData, MAX_PANES_DESKTOP, MAX_PANES_MOBILE};
use crate::tiles::MapTileState;
use chrono::Timelike;
use egui::Context;
use rustdar_radar::types::{ImageBounds, RadarProduct, ScanInfo, IMAGE_SIZE, MAX_RANGE_KM};
use rustdar_radar::sites::RADARS;
use rustdar_overlays::spc::outlook::OutlookDay;


#[path = "ui_popups.rs"]
mod popups;
#[path = "ui_config.rs"]
mod config;
#[path = "ui_mobile.rs"]
mod mobile;
#[path = "ui_map_overlays.rs"]
mod map_overlays;

#[cfg(target_os = "android")]
use mobile::DoubleTapDragDetector;

use map_overlays::{draw_spc_overlays, draw_spc_discussions, draw_nws_alerts, draw_label_tiles_overlay};

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
    // Track which site is currently loading
    loading_site: Option<String>,
    // Time dialog state
    show_time_dialog: bool,
    // Auto-poll interval in seconds (increases on failure, resets on success)
    poll_interval_secs: u64,
    // --- Map tiles (shared across panes) ---
    map_tiles: MapTileState,
    // User's GPS location for blue dot indicator (lat, lon)
    user_location: Option<(f64, f64)>,
    // Overlay data (SPC outlooks, NWS alerts, SPC discussions)
    pub overlays: OverlayData,
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
            map_tiles: MapTileState::default(),
            loading_site: None,
            show_time_dialog: false,
            poll_interval_secs: 60,
            user_location: None,
            overlays: OverlayData::default(),
            panes: vec![PaneState::new()],
            active_pane: 0,
            pane_layout: PaneLayout::default(),
            viewport_sync: true,
            sync_layers: true,
            #[cfg(target_os = "android")]
            show_mobile_menu: false,
            #[cfg(target_os = "android")]
            double_tap_detector: DoubleTapDragDetector::default(),
            safe_area_insets: (0.0, 0.0, 0.0, 0.0),
        }
    }

    /// Create the UI using egui.
    pub fn ui(&mut self, ctx: &egui::Context) -> Vec<GuiAction> {
        let mut actions = Vec::new();

        self.check_auto_polls(&mut actions);

        #[cfg(target_os = "android")]
        self.render_mobile_ui(ctx, &mut actions);
        #[cfg(not(target_os = "android"))]
        self.render_desktop_ui(ctx, &mut actions);

        actions
    }

    /// Check timers and emit fetch actions for auto-polling radar scans,
    /// NWS alerts, and SPC discussions.
    fn check_auto_polls(&mut self, actions: &mut Vec<GuiAction>) {
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
            && !self.overlays.nws_alerts.fetching
            && self.overlays.nws_alerts.needs_refresh(120)
        {
            actions.push(GuiAction::FetchNwsAlerts);
        }

        // Auto-refresh SPC Mesoscale Discussions every 2 minutes when any pane has it enabled
        if self.panes.iter().any(|p| p.layers.is_enabled(LayerKind::SpcMesoscaleDiscussions))
            && !self.overlays.spc_discussions.fetching
            && self.overlays.spc_discussions.needs_refresh(120)
        {
            actions.push(GuiAction::FetchSpcDiscussions);
        }
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

        // Initialize tiles via MapTileState
        self.map_tiles.ensure_base_tiles(is_dark_theme, ctx);
        let any_city_labels = MapTileState::any_city_labels(&self.panes);
        if any_city_labels {
            self.map_tiles.ensure_label_tiles(is_dark_theme, ctx);
        }

        // Take tiles out of self so they can be reborrowed per-pane in the loop.
        let mut tiles_owned = self.map_tiles.take_base_tiles();
        let mut label_tiles = if any_city_labels {
            self.map_tiles.take_label_tiles()
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
                let pointer_available = self.overlays.selected_alert.is_none()
                    && self.overlays.selected_md.is_none();
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
                            self.overlays.selected_alert = None;
                            self.overlays.selected_md = None;
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
                                &self.overlays.spc_outlooks.data,
                                self.map_tiles.current_theme_is_dark,
                                &mut pane.spc_overlay_caches,
                                &self.overlays.spc_data_generation,
                            );

                            // Overlay radar data if available
                            if pane.layers.is_enabled(LayerKind::Radar) {
                            if let Some(ref img) =
                                radar_image
                            {
                                let bounds = pane.cached_image_bounds
                                    .unwrap_or_else(|| ImageBounds::from_radar_site(img.lat, img.lon));

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

                                        pane.hover_value = Some(compute_hover_info(
                                            img,
                                            hover_lat,
                                            hover_lon,
                                            hover_pos,
                                            rect,
                                            pane.selected_product,
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
                                    img.texture.id(),
                                    rect,
                                    egui::Rect::from_min_max(
                                        egui::pos2(0.0, 0.0),
                                        egui::pos2(1.0, 1.0),
                                    ),
                                    egui::Color32::WHITE,
                                );

                                // Draw a light grey circle showing the radar range
                                let radar_center = projector.project(walkers::lat_lon(img.lat, img.lon)).to_pos2();
                                let north_edge = projector.project(
                                    walkers::lat_lon(img.lat + MAX_RANGE_KM / 111.32, img.lon)
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
                                &self.overlays.spc_discussions.data,
                                &mut pane.spc_md_overlay_cache,
                                self.overlays.spc_discussions.data_generation,
                                pointer_available,
                            );
                            if let Some(idx) = clicked_md {
                                self.overlays.selected_md = Some(idx);
                            }

                            // Draw NWS alert polygons
                            let clicked_alert = draw_nws_alerts(
                                ui,
                                projector,
                                zoom,
                                &pane.layers,
                                &self.overlays.nws_alerts.data,
                                &self.overlays.hidden_alerts,
                                &mut pane.nws_overlay_cache,
                                self.overlays.nws_alerts.data_generation,
                                pointer_available,
                            );
                            if let Some(idx) = clicked_alert {
                                self.overlays.selected_alert = Some(idx);
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
                self.sync_viewports(pane_count, &pre_zooms, &pre_positions);
            });

        // Restore tiles and label tiles
        self.map_tiles.restore_base_tiles(tiles_owned);
        if any_city_labels {
            self.map_tiles.restore_label_tiles(label_tiles);
        }

        actions
    }

    /// Render the layer controls shared by desktop and mobile panels.
    ///
    /// Covers: radar product/elevation, SPC outlooks, SPC discussions,
    /// NWS alerts, city labels, radar sites, and viewport sync toggles.
    fn render_layer_controls(
        &mut self,
        ui: &mut egui::Ui,
        pane: &mut PaneState,
        combo_width: f32,
        id_prefix: &str,
        actions: &mut Vec<GuiAction>,
    ) {
        let day = pane.layers.spc_day;

        // --- Radar layer ---
        ui.checkbox(pane.layers.enabled_mut(LayerKind::Radar), "\u{1f6f0}  Radar");

        if pane.layers.is_enabled(LayerKind::Radar) {
            ui.indent(format!("{id_prefix}radar_controls"), |ui| {
                if let Some(scan_info) = &self.scan_info {
                    let prev_product = pane.selected_product;
                    egui::ComboBox::from_id_salt(format!("{id_prefix}product_sel"))
                        .selected_text(pane.selected_product.name())
                        .width(combo_width)
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

                            egui::ComboBox::from_id_salt(format!("{id_prefix}elev_sel"))
                                .selected_text(format!("{:.1}\u{b0}", selected_angle))
                                .width(combo_width)
                                .show_ui(ui, |ui| {
                                    for angle in elevations.iter() {
                                        ui.selectable_value(
                                            &mut pane.selected_elevation,
                                            *angle,
                                            format!("{:.1}\u{b0}", angle),
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

        // --- SPC Outlooks ---
        ui.label("\u{26c8}  SPC Outlooks");

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
                    if !self.overlays.spc_outlooks.data.contains_key(&(day, product)) {
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
                    .add_enabled(!self.overlays.spc_outlooks.fetching, egui::Button::new("\u{1f504} Refresh"))
                    .clicked()
                {
                    actions.push(GuiAction::RefreshSpcOutlooks);
                }
                if self.overlays.spc_outlooks.fetching {
                    ui.spinner();
                }
            });
        }

        ui.add_space(6.0);
        ui.separator();

        // --- SPC Mesoscale Discussions ---
        {
            let was_enabled = pane.layers.is_enabled(LayerKind::SpcMesoscaleDiscussions);
            let label = if self.overlays.spc_discussions.data.is_empty() {
                "\u{1f4cb}  Mesoscale Disc.".to_string()
            } else {
                format!("\u{1f4cb}  Mesoscale Disc. ({})", self.overlays.spc_discussions.data.len())
            };
            ui.checkbox(
                pane.layers.enabled_mut(LayerKind::SpcMesoscaleDiscussions),
                label,
            );
            let is_enabled = pane.layers.is_enabled(LayerKind::SpcMesoscaleDiscussions);
            if is_enabled && !was_enabled && self.overlays.spc_discussions.data.is_empty() && !self.overlays.spc_discussions.fetching {
                actions.push(GuiAction::FetchSpcDiscussions);
            }
        }

        if pane.layers.is_enabled(LayerKind::SpcMesoscaleDiscussions) {
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(!self.overlays.spc_discussions.fetching, egui::Button::new("\u{1f504} Refresh"))
                    .clicked()
                {
                    actions.push(GuiAction::RefreshSpcDiscussions);
                }
                if self.overlays.spc_discussions.fetching {
                    ui.spinner();
                }
            });
            if let Some(t) = self.overlays.spc_discussions.fetch_time {
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

        // --- NWS Alerts ---
        ui.label("\u{26a0}  NWS Alerts");

        let nws_layers = [LayerKind::NwsWarnings, LayerKind::NwsWatches, LayerKind::NwsAdvisories];
        for layer in &nws_layers {
            let was_enabled = pane.layers.is_enabled(*layer);
            ui.checkbox(pane.layers.enabled_mut(*layer), layer.display_name());
            let is_enabled = pane.layers.is_enabled(*layer);
            if is_enabled && !was_enabled && self.overlays.nws_alerts.data.is_empty() && !self.overlays.nws_alerts.fetching {
                actions.push(GuiAction::FetchNwsAlerts);
            }
        }

        if pane.layers.any_nws_enabled() {
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(!self.overlays.nws_alerts.fetching, egui::Button::new("\u{1f504} Refresh"))
                    .clicked()
                {
                    actions.push(GuiAction::RefreshNwsAlerts);
                }
                if self.overlays.nws_alerts.fetching {
                    ui.spinner();
                }
            });
            if !self.overlays.nws_alerts.data.is_empty() {
                let categories = pane.layers.enabled_nws_categories();
                let visible_count = self.overlays.nws_alerts.data.iter()
                    .filter(|a| categories.contains(&a.category))
                    .count();
                ui.label(format!("{} alerts shown", visible_count));
            }
            if let Some(t) = self.overlays.nws_alerts.fetch_time {
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
            ui.checkbox(&mut self.viewport_sync, "\u{1f517}  Sync Viewports");
            ui.checkbox(&mut self.sync_layers, "\u{1f517}  Sync Layers");
            ui.separator();
        }

        // --- Other overlays ---
        ui.checkbox(pane.layers.enabled_mut(LayerKind::CityLabels), "\u{1f3f7}  City Labels");
        ui.checkbox(pane.layers.enabled_mut(LayerKind::RadarSites), "\u{1f4e1}  Radar Sites");
    }

    /// Propagate layer settings from the active pane to all others (when sync is enabled).
    fn propagate_layer_sync(&mut self) {
        if !self.sync_layers || self.pane_layout.pane_count <= 1 {
            return;
        }
        let src = &self.panes[self.active_pane].layers;
        let spc_day = src.spc_day;
        let snapshot: Vec<(LayerKind, bool)> = LayerKind::all()
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

    /// Render the layers panel on the left side (desktop).
    #[cfg(not(target_os = "android"))]
    fn render_layers_panel(&mut self, ctx: &Context) -> Vec<GuiAction> {
        let mut actions = Vec::new();
        let mut pane = std::mem::take(&mut self.panes[self.active_pane]);

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
                    ui.separator();
                }

                self.render_layer_controls(ui, &mut pane, 120.0, "d_", &mut actions);
            });

        self.panes[self.active_pane] = pane;
        self.propagate_layer_sync();

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
    pub fn set_safe_area_insets(&mut self, top: f32, bottom: f32, left: f32, right: f32) {
        self.safe_area_insets = (top, bottom, left, right);
    }

    pub fn set_user_location(&mut self, lat: f64, lon: f64) {
        self.user_location = Some((lat, lon));
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
    pub fn get_radar_image(&self) -> &Option<RadarImageData> {
        &self.panes[self.active_pane].radar_image
    }

    /// Take the radar image from the active pane.
    pub fn take_radar_image(&mut self) -> Option<RadarImageData> {
        self.panes[self.active_pane].take_radar_image()
    }

    /// Take the radar image from a specific pane.
    pub fn take_radar_image_for_pane(&mut self, pane_idx: PaneId) -> Option<RadarImageData> {
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
        self.map_tiles.clear();
        self.loading_site = None;
    }

    /// Propagate the interacted pane's viewport (zoom + position) to all other panes.
    fn sync_viewports(
        &mut self,
        pane_count: usize,
        pre_zooms: &[f64],
        pre_positions: &[Option<walkers::Position>],
    ) {
        if !self.viewport_sync || pane_count <= 1 {
            return;
        }
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
}

/// Format a radar product value for display in the hover tooltip.
fn format_radar_value(product: RadarProduct, value: f32) -> String {
    match product {
        RadarProduct::Reflectivity => format!(" | Reflectivity: {:.1} dBZ", value),
        RadarProduct::Velocity | RadarProduct::StormRelativeVelocity => {
            let mph = value * 2.23694;
            format!(" | {}: {:.1} mph", product.name(), mph)
        }
        RadarProduct::SpectrumWidth => {
            let mph = value * 2.23694;
            format!(" | Spectrum Width: {:.1} mph", mph)
        }
        RadarProduct::DifferentialReflectivity => format!(" | Diff. Reflectivity: {:.2} dB", value),
        RadarProduct::CorrelationCoefficient => format!(" | Corr. Coefficient: {:.4}", value),
        RadarProduct::DifferentialPhase => format!(" | Diff. Phase: {:.1}°", value),
        RadarProduct::SpecificDifferentialPhase => format!(" | KDP: {:.2} °/km", value),
        RadarProduct::EchoTops => format!(" | Echo Tops: {:.1} kft", value),
        RadarProduct::VerticallyIntegratedLiquid => format!(" | VIL: {:.1} kg/m²", value),
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
        RadarProduct::PrecipitationRate => format!(" | Precip Rate: {:.2} in/hr", value),
        RadarProduct::NormalizedRotation => format!(" | NROT: {:.2}", value),
    }
}

/// Compute the hover information string for a cursor position over the radar image.
fn compute_hover_info(
    img: &RadarImageData,
    hover_lat: f64,
    hover_lon: f64,
    hover_pos: egui::Pos2,
    rect: egui::Rect,
    product: RadarProduct,
) -> String {
    let lat1 = img.lat.to_radians();
    let lon1 = img.lon.to_radians();
    let lat2 = hover_lat.to_radians();
    let lon2 = hover_lon.to_radians();
    let dlat = lat2 - lat1;
    let dlon = lon2 - lon1;
    let a = (dlat / 2.0).sin().powi(2) + lat1.cos() * lat2.cos() * (dlon / 2.0).sin().powi(2);
    let c = 2.0 * a.sqrt().atan2((1.0 - a).sqrt());
    let distance_km = 6371.0 * c;

    let y = dlon.sin() * lat2.cos();
    let x = lat1.cos() * lat2.sin() - lat1.sin() * lat2.cos() * dlon.cos();
    let azimuth = (y.atan2(x).to_degrees() + 360.0) % 360.0;

    let mut value_str = String::new();
    let frac_x = (hover_pos.x - rect.left()) / rect.width();
    let frac_y = (hover_pos.y - rect.top()) / rect.height();
    let px = (frac_x * IMAGE_SIZE as f32) as i32;
    let py = (frac_y * IMAGE_SIZE as f32) as i32;

    if px >= 0 && px < IMAGE_SIZE as i32 && py >= 0 && py < IMAGE_SIZE as i32 {
        let pixel_idx = py as usize * IMAGE_SIZE + px as usize;
        if pixel_idx < img.value_data.len() {
            let value = img.value_data[pixel_idx];
            if !value.is_nan() {
                value_str = format_radar_value(product, value);
            }
        }
    }

    format!(
        "Lat: {:.4}°, Lon: {:.4}° | Range: {:.1}km, Az: {:.1}° {}",
        hover_lat, hover_lon, distance_km, azimuth, value_str
    )
}

