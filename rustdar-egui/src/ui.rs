use crate::actions::{GuiAction, RadarConfig};
use crate::layers::{LayerKind, LayerManager};
use crate::overlay_state::OverlayData;
use crate::pane::{PaneId, PaneLayout, PaneState, RadarImageData, MAX_PANES_DESKTOP, MAX_PANES_MOBILE};
use crate::tiles::MapTileState;
use chrono::Timelike;
use egui::Context;
use rustdar_radar::types::{RadarProduct, ScanInfo};
use rustdar_overlays::spc::outlook::OutlookDay;


#[path = "ui_popups.rs"]
mod popups;
#[path = "ui_config.rs"]
mod config;
#[path = "ui_mobile.rs"]
mod mobile;
#[path = "ui_map_overlays.rs"]
mod map_overlays;
#[path = "ui_desktop.rs"]
mod desktop;
#[path = "ui_map.rs"]
mod map;

#[cfg(target_os = "android")]
use mobile::DoubleTapDragDetector;

/// Android-only UI state: slide-out menu visibility and gesture detection.
#[cfg(target_os = "android")]
pub(super) struct MobileState {
    pub show_menu: bool,
    pub double_tap_detector: DoubleTapDragDetector,
}

#[cfg(target_os = "android")]
impl Default for MobileState {
    fn default() -> Self {
        Self {
            show_menu: false,
            double_tap_detector: DoubleTapDragDetector::default(),
        }
    }
}

/// Radar fetch lifecycle state.
pub(super) struct RadarState {
    pub config: RadarConfig,
    pub scan_info: Option<ScanInfo>,
    pub fetching: bool,
    pub error_message: Option<String>,
    pub loading_site: Option<String>,
}

/// Auto-polling timer state.
pub(super) struct AutoPollState {
    last_fetch_time: Option<std::time::Instant>,
    pub enabled: bool,
    initial_fetch_done: bool,
    interval_secs: u64,
}

impl AutoPollState {
    /// Record that a fetch was just dispatched.
    pub fn record_fetch(&mut self) {
        self.last_fetch_time = Some(std::time::Instant::now());
    }

    /// Call when a scan loads successfully — resets backoff to the base interval.
    pub fn on_success(&mut self) {
        self.interval_secs = 60;
    }

    /// Call on fetch failure — exponential backoff capped at 5 minutes.
    pub fn on_error(&mut self) {
        self.interval_secs = (self.interval_secs * 2).min(300);
    }

    /// Whether the poll timer has elapsed and a new check should fire.
    pub fn should_poll(&self) -> bool {
        self.enabled
            && self.last_fetch_time
                .is_some_and(|t| t.elapsed().as_secs() >= self.interval_secs)
    }

    /// Seconds remaining until the next poll, if a timer is running.
    pub fn time_until_next(&self) -> Option<u64> {
        self.last_fetch_time.map(|t| {
            self.interval_secs.saturating_sub(t.elapsed().as_secs())
        })
    }

    /// Whether auto-poll has started (initial fetch done) and is enabled.
    pub fn is_active(&self) -> bool {
        self.enabled && self.initial_fetch_done
    }
}

/// Time editing dialog state.
pub(super) struct TimeDialogState {
    pub date_string: String,
    pub time_string: String,
    pub show: bool,
}

pub struct Gui {
    radar: RadarState,
    auto_poll: AutoPollState,
    time_dialog: TimeDialogState,
    initial_zoom_set: bool,
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
    #[cfg(target_os = "android")]
    mobile: MobileState,
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
            radar: RadarState {
                config: radar_config,
                scan_info: None,
                fetching: false,
                error_message: None,
                loading_site: None,
            },
            auto_poll: AutoPollState {
                last_fetch_time: None,
                enabled: true,
                initial_fetch_done: false,
                interval_secs: 60,
            },
            time_dialog: TimeDialogState {
                date_string,
                time_string,
                show: false,
            },
            initial_zoom_set: false,
            map_tiles: MapTileState::default(),
            user_location: None,
            overlays: OverlayData::default(),
            panes: vec![PaneState::new()],
            active_pane: 0,
            pane_layout: PaneLayout::default(),
            viewport_sync: true,
            sync_layers: true,
            #[cfg(target_os = "android")]
            mobile: MobileState::default(),
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
        if !self.auto_poll.initial_fetch_done && !self.radar.fetching {
            self.radar.fetching = true;
            self.auto_poll.initial_fetch_done = true;
            self.auto_poll.record_fetch();
            actions.push(GuiAction::FetchRadarScan(self.radar.config.clone()));
        }

        // Poll for new scans at the current poll interval
        if self.auto_poll.should_poll() && !self.radar.fetching {
            // Check for new files without downloading
            let now = chrono::Local::now().naive_local();
            let current_scan_time = now
                .with_second(0)
                .and_then(|t| t.with_nanosecond(0))
                .unwrap_or(now);

            // Use current time for the check request
            let mut config = self.radar.config.clone();
            config.timestamp = current_scan_time;
            actions.push(GuiAction::CheckForNewScans(config));

            // Reset timer to avoid spamming checks
            self.auto_poll.record_fetch();
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
        self.radar.scan_info = Some(info);
        self.radar.fetching = false;
        self.auto_poll.on_success();

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
        self.radar.fetching = fetching;
    }

    /// Set an error message
    pub fn set_error(&mut self, error: String) {
        self.radar.error_message = Some(error);
        self.radar.fetching = false;
        self.auto_poll.on_error();
    }

    fn render_time_dialog(&mut self, ctx: &Context) -> Option<GuiAction> {
        let mut action = None;
        
        if self.time_dialog.show {
            egui::Window::new("Set Time")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
                .show(ctx, |ui| {
                    ui.vertical_centered(|ui| {
                        ui.heading("Select Time");
                        ui.add_space(10.0);
                        
                        ui.label("Date:");
                        ui.text_edit_singleline(&mut self.time_dialog.date_string);
                        
                        ui.add_space(5.0);
                        
                        ui.label("Time:");
                        ui.text_edit_singleline(&mut self.time_dialog.time_string);
                        
                        ui.add_space(10.0);
                        
                        if ui.button("Use Current Time").clicked() {
                            self.radar.config.timestamp = chrono::Local::now().naive_local();
                            self.time_dialog.date_string = self.radar.config.timestamp.format("%Y-%m-%d").to_string();
                            self.time_dialog.time_string = self.radar.config.timestamp.format("%H:%M:%S").to_string();
                        }
                        
                        ui.add_space(15.0);
                        
                        ui.horizontal(|ui| {
                            if ui.button("OK").clicked() {
                                // Try to parse the date and time strings
                                let datetime_str = format!("{} {}", self.time_dialog.date_string, self.time_dialog.time_string);
                                if let Ok(timestamp) = chrono::NaiveDateTime::parse_from_str(&datetime_str, "%Y-%m-%d %H:%M:%S") {
                                    self.radar.config.timestamp = timestamp;
                                    action = Some(GuiAction::FetchRadarScan(self.radar.config.clone()));
                                }
                                self.time_dialog.show = false;
                            }
                            
                            if ui.button("Cancel").clicked() {
                                // Restore the original strings from the current config
                                self.time_dialog.date_string = self.radar.config.timestamp.format("%Y-%m-%d").to_string();
                                self.time_dialog.time_string = self.radar.config.timestamp.format("%H:%M:%S").to_string();
                                self.time_dialog.show = false;
                            }
                        });
                    });
                });
        }
        
        action
    }

    /// Render pane count buttons and active-pane selector.
    ///
    /// Shared by desktop and mobile layers panels. The caller must pass the
    /// currently-taken `pane` by mutable reference so this method can swap
    /// it back into `self.panes` when the active pane changes.
    fn render_pane_selector(
        &mut self,
        ui: &mut egui::Ui,
        pane: &mut PaneState,
    ) {
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
                    self.panes[self.active_pane] = std::mem::take(pane);
                    while self.panes.len() < count {
                        self.panes.push(PaneState::new());
                    }
                    self.pane_layout = PaneLayout::for_count(count);
                    if self.active_pane >= count {
                        self.active_pane = 0;
                    }
                    *pane = std::mem::take(&mut self.panes[self.active_pane]);
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
                        self.panes[self.active_pane] = std::mem::take(pane);
                        self.active_pane = i;
                        *pane = std::mem::take(&mut self.panes[i]);
                    }
                }
            });
        }
        ui.separator();
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
        self.render_radar_controls(ui, pane, combo_width, id_prefix);

        ui.add_space(6.0);
        ui.separator();

        self.render_spc_outlook_controls(ui, pane, actions);

        ui.add_space(6.0);
        ui.separator();

        self.render_spc_discussion_controls(ui, pane, actions);

        ui.add_space(6.0);
        ui.separator();

        self.render_nws_alert_controls(ui, pane, actions);

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

    /// Render radar layer toggle with product/elevation combo boxes.
    fn render_radar_controls(
        &self,
        ui: &mut egui::Ui,
        pane: &mut PaneState,
        combo_width: f32,
        id_prefix: &str,
    ) {
        ui.checkbox(pane.layers.enabled_mut(LayerKind::Radar), "\u{1f6f0}  Radar");

        if pane.layers.is_enabled(LayerKind::Radar) {
            ui.indent(format!("{id_prefix}radar_controls"), |ui| {
                if let Some(scan_info) = &self.radar.scan_info {
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
    }

    /// Render SPC outlook day selector, layer toggles, and refresh button.
    fn render_spc_outlook_controls(
        &mut self,
        ui: &mut egui::Ui,
        pane: &mut PaneState,
        actions: &mut Vec<GuiAction>,
    ) {
        let day = pane.layers.spc_day;

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
    }

    /// Render SPC Mesoscale Discussion toggle, refresh button, and fetch time.
    fn render_spc_discussion_controls(
        &mut self,
        ui: &mut egui::Ui,
        pane: &mut PaneState,
        actions: &mut Vec<GuiAction>,
    ) {
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
    }

    /// Render NWS alert category toggles, refresh button, alert count, and fetch time.
    fn render_nws_alert_controls(
        &mut self,
        ui: &mut egui::Ui,
        pane: &mut PaneState,
        actions: &mut Vec<GuiAction>,
    ) {
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

    /// Get the active pane (immutable).
    pub fn active_pane(&self) -> &PaneState {
        &self.panes[self.active_pane]
    }

    /// Get the active pane (mutable).
    pub fn active_pane_mut(&mut self) -> &mut PaneState {
        &mut self.panes[self.active_pane]
    }

    /// Get the rendering params for a specific pane.
    pub fn get_rendering_params_for_pane(&self, pane_idx: PaneId) -> Option<(RadarProduct, f32)> {
        self.panes.get(pane_idx)
            .and_then(|p| p.get_rendering_params(self.radar.scan_info.as_ref()))
    }

    /// Number of active panes.
    pub fn pane_count(&self) -> usize {
        self.pane_layout.pane_count
    }

    /// Get the current radar config
    pub fn get_radar_config(&self) -> &RadarConfig {
        &self.radar.config
    }

    /// Set the radar config
    pub fn set_radar_config(&mut self, config: RadarConfig) {
        let date = config.timestamp.format("%Y-%m-%d").to_string();
        let time = config.timestamp.format("%H:%M:%S").to_string();
        self.radar.config = config;
        self.time_dialog.date_string = date;
        self.time_dialog.time_string = time;
    }

    /// Set which site is currently loading
    pub fn set_loading_site(&mut self, site: Option<String>) {
        self.radar.loading_site = site;
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

    /// Get a specific pane's layer manager (immutable).
    pub fn layers_for_pane(&self, pane_idx: PaneId) -> Option<&LayerManager> {
        self.panes.get(pane_idx).map(|p| &p.layers)
    }

    /// Get the current scan info
    pub fn get_scan_info(&self) -> Option<&ScanInfo> {
        self.radar.scan_info.as_ref()
    }

    /// Take the radar image from a specific pane.
    pub fn take_radar_image_for_pane(&mut self, pane_idx: PaneId) -> Option<RadarImageData> {
        self.panes.get_mut(pane_idx).and_then(|p| p.take_radar_image())
    }

    /// Clear the radar image on a specific pane.
    pub fn clear_radar_image_for_pane(&mut self, pane_idx: PaneId) {
        if pane_idx < self.panes.len() {
            self.panes[pane_idx].clear_radar_image();
        }
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

    /// Whether auto-poll is active and the event loop should keep waking
    pub fn is_auto_poll_active(&self) -> bool {
        self.auto_poll.is_active()
            || self.panes.iter().any(|p| p.layers.any_nws_enabled())
    }

    pub fn clear_graphics_state(&mut self) {
        for pane in &mut self.panes {
            pane.clear_radar_image();
        }
        self.map_tiles.clear();
        self.radar.loading_site = None;
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
}
