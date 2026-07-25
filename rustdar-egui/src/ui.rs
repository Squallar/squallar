use crate::actions::{GuiAction, RadarConfig};
use rustdar_overlays::render::controls::{ControlEffect, ControlItem, ControlUpdate, ControlValue, PaneControlContext, PaneControlContextMut};

const DEFAULT_INITIAL_ZOOM: f64 = 7.0;

use rustdar_overlays::render::overlay_state::{OverlayRegistry, OverlayKind};
use crate::pane::{
    ColorScaleOrientation, PaneId, PaneLayout, PaneState, MAX_PANES_DESKTOP, MAX_PANES_MOBILE,
};
use crate::tiles::MapTileState;
use chrono::Timelike;
use egui::Context;
use rustdar_radar::types::{RadarProduct, ScanInfo};
use rustdar_units::UserPreferences;


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
#[path = "ui_settings.rs"]
mod settings;

#[cfg(target_os = "android")]
use crate::ui_input::TouchGestures;

/// Android-only UI state: slide-out menu visibility and gesture detection.
#[cfg(target_os = "android")]
#[derive(Default)]
pub(super) struct MobileState {
    pub show_menu: bool,
    /// Touch gesture detectors (double-tap-drag zoom, long press).
    /// Platform-independent — see [`crate::ui_input`].
    pub gestures: TouchGestures,
}

/// Radar fetch lifecycle state.
pub(super) struct RadarState {
    pub config: RadarConfig,
    pub fetching: bool,
    pub error_message: Option<String>,
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
    #[cfg(not(target_os = "android"))]
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
    // User's GPS fix (full data from GPS receiver or Android LocationManager)
    user_fix: Option<rustdar_gps::GpsFix>,
    // Compass heading in degrees (0–360), from device compass sensor
    user_heading: Option<f32>,
    // Overlay data (SPC outlooks, NWS alerts, SPC discussions)
    pub overlays: OverlayRegistry,
    // Multi-pane state
    panes: Vec<PaneState>,
    active_pane: PaneId,
    pane_layout: PaneLayout,
    /// Remembered color-scale bar orientation for the map panel (hysteresis, so
    /// a resize near the boundary cannot make the bars hop).
    color_scale_orientation: ColorScaleOrientation,
    /// The map panel rect the last frame laid its pane grid out in. Only read
    /// by tests, which need the same rects `render_map` used.
    #[cfg(test)]
    last_map_panel_rect: egui::Rect,
    viewport_sync: bool,
    sync_layers: bool,
    // --- Radar loop settings ---
    /// How far back (in seconds) to fetch historical scans for the loop.
    pub loop_lookback_secs: u64,
    /// Animation speed in frames per second.
    pub loop_speed_fps: f32,
    #[cfg(target_os = "android")]
    mobile: MobileState,
    // Safe area insets in logical pixels (top, bottom, left, right)
    // Used on Android to avoid drawing under system bars.
    safe_area_insets: (f32, f32, f32, f32),
    /// User unit and timezone preferences.
    pub preferences: UserPreferences,
    /// Whether the settings panel is open.
    pub show_settings: bool,
    /// GPS configuration (port, baud, heading source).
    pub gps_config: rustdar_gps::GpsConfig,
}

impl Default for Gui {
    fn default() -> Self {
        Self::new()
    }
}

/// Render a single declarative [`ControlItem`] into the UI, collecting any
/// resulting [`ControlUpdate`]s into `updates`.
fn render_control_item(
    ui: &mut egui::Ui,
    kind: OverlayKind,
    item: &ControlItem,
    updates: &mut Vec<(OverlayKind, ControlUpdate)>,
) {
    match item {
        ControlItem::Toggle { id, label, enabled } => {
            let mut value = *enabled;
            if ui.checkbox(&mut value, label.as_str()).changed() {
                updates.push((kind, ControlUpdate { id, value: ControlValue::Bool(value) }));
            }
        }
        ControlItem::Heading { text } => {
            ui.label(text.as_str());
        }
        ControlItem::InfoText { text } => {
            ui.label(egui::RichText::new(text.as_str()).small().weak());
        }
        ControlItem::ButtonRow { buttons } => {
            let any_highlighted = buttons.iter().any(|b| b.highlight);
            ui.horizontal_wrapped(|ui| {
                for btn in buttons {
                    let clicked = if any_highlighted {
                        ui.add_enabled(
                            btn.enabled,
                            egui::Button::new(btn.label.as_str()).selected(btn.highlight),
                        ).clicked()
                    } else {
                        ui.add_enabled(
                            btn.enabled,
                            egui::Button::new(btn.label.as_str()),
                        ).clicked()
                    };
                    if clicked {
                        updates.push((kind, ControlUpdate { id: btn.id, value: ControlValue::Action }));
                    }
                }
            });
        }
        ControlItem::Separator => {
            ui.separator();
        }
        ControlItem::Dropdown { id, label, options, selected } => {
            let mut sel = selected.clone();
            let original = sel.clone();
            ui.horizontal(|ui| {
                ui.label(label.as_str());
                egui::ComboBox::from_id_salt(format!("{kind:?}_{id}"))
                    .selected_text(sel.as_str())
                    .show_ui(ui, |ui| {
                        for (value, display) in options {
                            ui.selectable_value(&mut sel, value.clone(), display.as_str());
                        }
                    });
            });
            if sel != original {
                updates.push((kind, ControlUpdate { id, value: ControlValue::String(sel) }));
            }
        }
        ControlItem::Slider { id, label, min, max, value, logarithmic, .. } => {
            let mut val = *value;
            let original = val;
            ui.horizontal(|ui| {
                ui.label(label.as_str());
                let mut slider = egui::Slider::new(&mut val, *min..=*max);
                if *logarithmic {
                    slider = slider.logarithmic(true);
                }
                ui.add(slider);
            });
            if (val - original).abs() > f64::EPSILON {
                updates.push((kind, ControlUpdate { id, value: ControlValue::Float(val) }));
            }
        }
        ControlItem::Section { label, collapsible, expanded, items } => {
            if *collapsible {
                egui::CollapsingHeader::new(label.as_str())
                    .default_open(*expanded)
                    .show(ui, |ui| {
                        for child in items {
                            render_control_item(ui, kind, child, updates);
                        }
                    });
            } else {
                ui.group(|ui| {
                    ui.label(egui::RichText::new(label.as_str()).strong());
                    for child in items {
                        render_control_item(ui, kind, child, updates);
                    }
                });
            }
        }
    }
}

impl Gui {
    pub fn new() -> Self {
        let radar_config = RadarConfig::default();
        let date_string = radar_config.timestamp.format("%Y-%m-%d").to_string();
        let time_string = radar_config.timestamp.format("%H:%M:%S").to_string();

        let mut gui = Self {
            radar: RadarState {
                config: radar_config,
                fetching: false,
                error_message: None,
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
            user_fix: None,
            user_heading: None,
            overlays: OverlayRegistry::default(),
            panes: vec![PaneState::new()],
            active_pane: 0,
            pane_layout: PaneLayout::default(),
            color_scale_orientation: ColorScaleOrientation::default(),
            #[cfg(test)]
            last_map_panel_rect: egui::Rect::ZERO,
            viewport_sync: true,
            sync_layers: true,
            loop_lookback_secs: 3600, // default 1 hour
            loop_speed_fps: 5.0,      // default 5 fps
            #[cfg(target_os = "android")]
            mobile: MobileState::default(),
            safe_area_insets: (0.0, 0.0, 0.0, 0.0),
            preferences: UserPreferences::default(),
            show_settings: false,
            gps_config: rustdar_gps::GpsConfig::default(),
        };
        gui.initialize_pane_enabled();
        gui
    }

    /// Create the UI using egui.
    pub fn ui(&mut self, ctx: &egui::Context) -> Vec<GuiAction> {
        let mut actions = Vec::new();

        self.check_auto_polls(&mut actions);

        // Create a root Ui to host the panels. Since egui 0.35 the Context-taking
        // `Panel::show` is gone and panels are Ui-scoped only, so this root Ui is
        // the only way in.
        let mut root_ui = egui::Ui::new(
            ctx.clone(),
            egui::Id::new("rustdar_root"),
            egui::UiBuilder::new()
                .layer_id(egui::LayerId::background())
                .max_rect(ctx.content_rect()),
        );

        #[cfg(target_os = "android")]
        self.render_mobile_ui(&mut root_ui, &mut actions);
        #[cfg(not(target_os = "android"))]
        self.render_desktop_ui(&mut root_ui, &mut actions);

        self.render_settings(ctx, &mut actions);

        // Ensure the handler state reflects the active pane's config at frame
        // end, so any deferred actions (FetchOverlay, etc.) processed after the
        // frame use the correct per-pane state.
        let active = &self.panes[self.active_pane];
        if !active.overlay_configs.is_empty() {
            let configs = active.overlay_configs.clone();
            self.overlays.load_pane_configs(&configs);
        }

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
            let mut config = self.radar.config.clone();
            config.site = self.active_pane().site.clone();
            actions.push(GuiAction::FetchRadarScan(config));
        }

        // Poll for new scans at the current poll interval (only when any pane is viewing live)
        if self.is_any_pane_live() && self.auto_poll.should_poll() && !self.radar.fetching {
            // Check for new files without downloading — emit one check per unique live site
            let now = chrono::Local::now().naive_local();
            let current_scan_time = now
                .with_second(0)
                .and_then(|t| t.with_nanosecond(0))
                .unwrap_or(now);

            let mut seen_sites: Vec<&str> = Vec::with_capacity(self.pane_layout.pane_count);
            for pane in self.panes.iter().take(self.pane_layout.pane_count) {
                if pane.viewing_live && !seen_sites.contains(&pane.site.as_str()) {
                    seen_sites.push(&pane.site);
                    let config = RadarConfig {
                        site: pane.site.clone(),
                        timestamp: current_scan_time,
                    };
                    actions.push(GuiAction::CheckForNewScans(config));
                }
            }

            // Reset timer to avoid spamming checks
            self.auto_poll.record_fetch();
        }

        // Auto-refresh overlay data when layers are enabled and refresh interval elapsed
        for &kind in OverlayKind::all() {
            if let Some(interval) = self.overlays.auto_poll_interval(kind)
                && let Some(pane_idx) = self.first_pane_with_overlay_enabled(kind)
                    && !self.overlays.is_fetching(kind)
                    && self.overlays.fetch_time(kind)
                        .is_none_or(|t| t.elapsed().as_secs() >= interval)
                {
                    actions.push(GuiAction::FetchOverlay { kind, pane_idx });
                }
        }
    }

    /// Update the scan info for all panes viewing the given site.
    pub fn set_scan_info_for_site(&mut self, site: &str, info: ScanInfo) {
        for pane in &mut self.panes {
            if pane.site == site {
                pane.scan_info = Some(info.clone());
            }
        }
        self.radar.fetching = false;
        self.auto_poll.on_success();

        // Only zoom to radar on the first scan load to avoid disrupting user navigation
        if !self.initial_zoom_set {
            for pane in &mut self.panes {
                let _ = pane.map_memory.set_zoom(DEFAULT_INITIAL_ZOOM);
            }
            self.initial_zoom_set = true;
        }
    }

    /// Update the scan info for a specific pane.
    pub fn set_scan_info_for_pane(&mut self, pane_idx: usize, info: ScanInfo) {
        if let Some(pane) = self.panes.get_mut(pane_idx) {
            pane.scan_info = Some(info);
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
            let screen = ctx.input(|i| i.viewport_rect());
            egui::Window::new("Set Time")
                .collapsible(false)
                .resizable(false)
                .pivot(egui::Align2::CENTER_CENTER)
                .default_pos(screen.center())
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
                                    if let Some(pane) = self.panes.get_mut(self.active_pane) {
                                        pane.viewing_live = false;
                                    }
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
                    let active_site = self.panes[self.active_pane].site.clone();
                    let active_scan_info = self.panes[self.active_pane].scan_info.clone();
                    while self.panes.len() < count {
                        let mut new_pane = PaneState::with_site(active_site.clone());
                        new_pane.scan_info = active_scan_info.clone();
                        self.panes.push(new_pane);
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
    /// Covers: radar product/elevation, radar loop, SPC outlooks, SPC discussions,
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

        // --- Time navigation (forward/back/live) ---
        self.render_time_navigation(ui, pane, actions);

        // --- Radar loop controls ---
        self.render_loop_controls(ui, pane, actions);

        ui.add_space(6.0);
        ui.separator();

        // --- Handler-backed overlay controls (generic) ---
        self.render_overlay_controls(ui, pane, actions);

        ui.add_space(6.0);
        ui.separator();

        // --- Viewport sync ---
        if self.pane_layout.pane_count > 1 {
            ui.checkbox(&mut self.viewport_sync, "\u{1f517}  Sync Viewports");
            ui.checkbox(&mut self.sync_layers, "\u{1f517}  Sync Layers");
            ui.separator();
        }

    }

    /// Render radar product/elevation combo boxes (shown when Radar is enabled).
    fn render_radar_controls(
        &self,
        ui: &mut egui::Ui,
        pane: &mut PaneState,
        combo_width: f32,
        id_prefix: &str,
    ) {
        if pane.is_overlay_enabled(OverlayKind::Radar) {
            ui.indent(format!("{id_prefix}radar_controls"), |ui| {
                if let Some(scan_info) = &pane.scan_info {
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
                        && !elevations.is_empty() {
                            let selected_angle = elevations
                                .iter()
                                .min_by(|a, b| {
                                    ((**a - pane.selected_elevation).abs())
                                        .total_cmp(
                                            &((**b - pane.selected_elevation).abs()),
                                        )
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
                } else {
                    ui.label("No scan loaded");
                }
            });
        }
    }

    /// Available time step options: (seconds, label). 0 = "one scan".
    const TIME_STEP_OPTIONS: &[(i64, &str)] = &[
        (0, "1 scan"),
        (600, "10 min"),
        (1800, "30 min"),
        (3600, "1 hr"),
        (7200, "2 hr"),
        (21600, "6 hr"),
        (43200, "12 hr"),
    ];

    /// Render forward / live / back navigation buttons with a time step dropdown.
    fn render_time_navigation(
        &mut self,
        ui: &mut egui::Ui,
        pane: &mut PaneState,
        actions: &mut Vec<GuiAction>,
    ) {
        ui.add_space(4.0);

        // Time step dropdown
        let step_label = Self::TIME_STEP_OPTIONS
            .iter()
            .find(|(s, _)| *s == pane.time_step_secs)
            .map(|(_, l)| *l)
            .unwrap_or("10 min");

        ui.horizontal(|ui| {
            ui.label("Step:");
            egui::ComboBox::from_id_salt("time_step_sel")
                .selected_text(step_label)
                .show_ui(ui, |ui| {
                    for &(secs, label) in Self::TIME_STEP_OPTIONS {
                        ui.selectable_value(&mut pane.time_step_secs, secs, label);
                    }
                });
        });

        // Navigation buttons
        let active_pane_idx = self.active_pane;
        ui.horizontal(|ui| {
            // Back button
            if ui.button("\u{25c0} Back").clicked() {
                pane.viewing_live = false;
                if pane.time_step_secs == 0 {
                    actions.push(GuiAction::NavigateOneScan { pane_idx: active_pane_idx, forward: false });
                } else {
                    actions.push(GuiAction::NavigateTime { pane_idx: active_pane_idx, step_secs: -pane.time_step_secs });
                }
            }

            // Live button — highlighted when NOT live to indicate "click to return"
            let live_button = if pane.viewing_live {
                egui::Button::new("\u{23fa} Live")
            } else {
                egui::Button::new(
                    egui::RichText::new("\u{23fa} Live").color(egui::Color32::WHITE)
                ).fill(egui::Color32::from_rgb(200, 50, 50))
            };
            if ui.add(live_button).clicked() && !pane.viewing_live {
                actions.push(GuiAction::JumpToLive { pane_idx: active_pane_idx });
            }

            // Forward button — disabled when live
            if ui.add_enabled(!pane.viewing_live, egui::Button::new("Forward \u{25b6}")).clicked() {
                if pane.time_step_secs == 0 {
                    actions.push(GuiAction::NavigateOneScan { pane_idx: active_pane_idx, forward: true });
                } else {
                    actions.push(GuiAction::NavigateTime { pane_idx: active_pane_idx, step_secs: pane.time_step_secs });
                }
            }
        });
    }

    /// Render radar loop controls: enable/disable, lookback slider, speed slider,
    /// transport buttons (play/pause, step, seek), and frame progress.
    fn render_loop_controls(
        &mut self,
        ui: &mut egui::Ui,
        pane: &mut PaneState,
        actions: &mut Vec<GuiAction>,
    ) {
        ui.add_space(4.0);
        let loop_active = pane.loop_state.is_active();

        // Enable/disable toggle
        let mut enabled = loop_active;
        if ui.checkbox(&mut enabled, "\u{1f501}  Radar Loop").changed() {
            if enabled {
                for pane_idx in self.loop_sync_targets() {
                    actions.push(GuiAction::EnableLoop {
                        pane_idx,
                        lookback_secs: self.loop_lookback_secs,
                    });
                }
            } else {
                for pane_idx in self.loop_sync_targets() {
                    actions.push(GuiAction::DisableLoop { pane_idx });
                }
            }
        }

        if loop_active {
            ui.indent("loop_controls", |ui| {
                // Lookback duration slider
                let mut lookback_mins = (self.loop_lookback_secs as f32 / 60.0).round();
                ui.horizontal(|ui| {
                    ui.label("Lookback:");
                    if ui.add(egui::Slider::new(&mut lookback_mins, 5.0..=1440.0)
                        .logarithmic(true)
                        .suffix(" min")
                        .clamping(egui::SliderClamping::Always)
                    ).drag_stopped() {
                        let new_secs = (lookback_mins * 60.0) as u64;
                        if new_secs != self.loop_lookback_secs {
                            self.loop_lookback_secs = new_secs;
                            for pane_idx in self.loop_sync_targets() {
                                actions.push(GuiAction::EnableLoop {
                                    pane_idx,
                                    lookback_secs: new_secs,
                                });
                            }
                        }
                    }
                });

                // Speed slider
                ui.horizontal(|ui| {
                    ui.label("Speed:");
                    ui.add(egui::Slider::new(&mut self.loop_speed_fps, 1.0..=30.0)
                        .suffix(" fps")
                        .clamping(egui::SliderClamping::Always)
                    );
                });

                {
                    let ls = &pane.loop_state;
                    // Frame status
                    let rendered = ls.frames.iter().filter(|f| f.texture.is_some()).count();
                    let total = ls.frames.len();
                    let rendering = total > 0 && !ls.is_render_ready();
                    if ls.is_fetching() {
                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.label("Loading scan list...");
                        });
                    } else if total == 0 {
                        ui.label("No frames found");
                    } else {
                        // Progress bar when rendering, plain text when done
                        if rendering {
                            ui.horizontal(|ui| {
                                ui.spinner();
                                ui.label(format!("Rendering {}/{}...", rendered, total));
                            });
                            ui.add(
                                egui::ProgressBar::new(rendered as f32 / total as f32)
                                    .show_percentage()
                            );
                        } else {
                            ui.label(format!("{}/{} frames rendered", rendered, total));
                        }

                        // Transport controls
                        ui.horizontal(|ui| {
                            // Step backward
                            if ui.button("\u{23ee}").on_hover_text("Previous frame").clicked() {
                                for pane_idx in self.loop_sync_targets() {
                                    actions.push(GuiAction::StepLoopFrame {
                                        pane_idx,
                                        forward: false,
                                    });
                                }
                            }

                            // Play/pause
                            let play_label = if ls.is_playing() { "\u{23f8}" } else { "\u{25b6}" };
                            let play_hover = if ls.is_playing() {
                                "Pause".to_string()
                            } else if rendering {
                                format!("Waiting for renders ({}/{})", rendered, total)
                            } else {
                                "Play".to_string()
                            };
                            let play_btn = egui::Button::new(play_label);
                            let resp = ui.add_enabled(!rendering || ls.is_playing(), play_btn)
                                .on_hover_text(play_hover);
                            if resp.clicked() {
                                for pane_idx in self.loop_sync_targets() {
                                    actions.push(GuiAction::ToggleLoopPlayback { pane_idx });
                                }
                            }

                            // Step forward
                            if ui.button("\u{23ed}").on_hover_text("Next frame").clicked() {
                                for pane_idx in self.loop_sync_targets() {
                                    actions.push(GuiAction::StepLoopFrame {
                                        pane_idx,
                                        forward: true,
                                    });
                                }
                            }
                        });

                        // Frame seek slider
                        let mut frame_idx = ls.current_frame;
                        if ui.add(egui::Slider::new(&mut frame_idx, 0..=(total - 1))
                            .show_value(false)
                        ).changed() {
                            for pane_idx in self.loop_sync_targets() {
                                actions.push(GuiAction::SeekLoopFrame {
                                    pane_idx,
                                    frame_index: frame_idx,
                                });
                            }
                        }

                        // Current frame timestamp
                        if let Some(frame) = ls.frames.get(ls.current_frame) {
                            ui.label(
                                egui::RichText::new(
                                    self.preferences.timezone.format_naive_utc(frame.timestamp, "%H:%M:%S")
                                ).small()
                            );
                        }
                    }
                }
            });
        }
    }

    /// Render controls for all handler-backed overlays generically.
    ///
    /// Loads the active pane's overlay config snapshot into the handlers,
    /// renders each handler's controls, applies updates, then saves the
    /// resulting config back to the pane. This makes every sub-control
    /// (categories, day, products, etc.) per-pane when Sync Layers is off.
    fn render_overlay_controls(
        &mut self,
        ui: &mut egui::Ui,
        pane: &mut PaneState,
        actions: &mut Vec<GuiAction>,
    ) {
        const ORDER: &[OverlayKind] = &[
            OverlayKind::Radar,
            OverlayKind::ModelData,
            OverlayKind::SpcOutlook,
            OverlayKind::SpcDiscussions,
            OverlayKind::NwsAlerts,
            OverlayKind::StormReports,
            OverlayKind::Lightning,
            OverlayKind::Metar,
            OverlayKind::CityLabels,
            OverlayKind::RadarSites,
            OverlayKind::UserLocation,
            OverlayKind::ColorScale,
        ];

        // Load this pane's config snapshot into the handlers.
        if !pane.overlay_configs.is_empty() {
            self.overlays.load_pane_configs(&pane.overlay_configs);
        }

        let ctx = PaneControlContext {
            pane_idx: self.active_pane,
            pane_state: None,
        };

        // Render controls and collect updates.
        let mut updates: Vec<(OverlayKind, ControlUpdate)> = Vec::new();

        for (i, &kind) in ORDER.iter().enumerate() {
            if i > 0 {
                ui.add_space(6.0);
                ui.separator();
            }
            let controls = self.overlays.controls(kind, &ctx);

            for item in &controls {
                render_control_item(ui, kind, item, &mut updates);
            }
        }

        // Apply updates and handle effects.
        let mut pane_ctx = PaneControlContextMut {
            pane_idx: self.active_pane,
            pane_state: None,
        };

        for (kind, update) in updates {
            let effect = self.overlays.apply_control(kind, &update, &mut pane_ctx);
            if matches!(effect, ControlEffect::Fetch) {
                actions.push(GuiAction::FetchOverlay { kind, pane_idx: self.active_pane });
            }
        }

        // Save the (possibly mutated) handler state back to the pane.
        pane.overlay_configs = self.overlays.save_pane_configs();
        pane.enabled_overlays = self.overlays.save_enabled_map();
    }

    /// Return the pane indices that loop actions should target.
    /// When `sync_layers` is on and there are multiple panes, returns all pane indices;
    /// otherwise returns only the active pane.
    fn loop_sync_targets(&self) -> Vec<usize> {
        if self.sync_layers && self.pane_layout.pane_count > 1 {
            (0..self.pane_layout.pane_count).collect()
        } else {
            vec![self.active_pane]
        }
    }

    /// Propagate layer settings from the active pane to all others (when sync is enabled).
    /// Also converges site and scan_info so all panes display the same radar site.
    fn propagate_layer_sync(&mut self) {
        if !self.sync_layers || self.pane_layout.pane_count <= 1 {
            return;
        }
        let src = &self.panes[self.active_pane];
        let active_site = src.site.clone();
        let active_scan_info = src.scan_info.clone();
        let active_viewing_live = src.viewing_live;
        let active_time_step_secs = src.time_step_secs;
        let active_draw_order = src.draw_order.clone();
        let active_enabled_overlays = src.enabled_overlays.clone();
        let active_overlay_configs = src.overlay_configs.clone();
        let active_selected_product = src.selected_product;
        let active_selected_elevation = src.selected_elevation;

        // Sync per-pane fields including enabled overlays, configs, and radar product/elevation.
        for (idx, p) in self.panes.iter_mut().enumerate() {
            if idx == self.active_pane {
                continue;
            }
            p.site = active_site.clone();
            p.scan_info = active_scan_info.clone();
            p.viewing_live = active_viewing_live;
            p.time_step_secs = active_time_step_secs;
            p.draw_order = active_draw_order.clone();
            p.enabled_overlays = active_enabled_overlays.clone();
            p.overlay_configs = active_overlay_configs.clone();
            p.selected_product = active_selected_product;
            p.selected_elevation = active_selected_elevation;
        }
    }

    /// Initialize per-pane `enabled_overlays` from the current handler states.
    ///
    /// Called after `new()` and after `load_ui_config()` to populate any panes
    /// whose `enabled_overlays` maps are empty (backward compatibility).
    pub fn initialize_pane_enabled(&mut self) {
        let defaults = self.overlays.build_enabled_map();
        let default_configs = self.overlays.save_pane_configs();
        for pane in &mut self.panes {
            for (&kind, &enabled) in &defaults {
                pane.enabled_overlays.entry(kind).or_insert(enabled);
            }
            // Seed overlay configs from handler defaults for panes with empty configs.
            if pane.overlay_configs.is_empty() {
                pane.overlay_configs = default_configs.clone();
            }
        }
    }

    /// Returns `true` if any pane has the given overlay kind enabled.
    ///
    /// Used for auto-poll decisions: we should fetch data for an overlay
    /// if at least one pane wants to display it.
    pub fn any_pane_has_overlay_enabled(&self, kind: OverlayKind) -> bool {
        self.panes.iter()
            .take(self.pane_layout.pane_count)
            .any(|p| p.is_overlay_enabled(kind))
    }

    /// Returns the index of the first pane that has the given overlay kind enabled,
    /// or `None` if no pane has it enabled.
    pub fn first_pane_with_overlay_enabled(&self, kind: OverlayKind) -> Option<usize> {
        self.panes.iter()
            .take(self.pane_layout.pane_count)
            .position(|p| p.is_overlay_enabled(kind))
    }

    /// Get the active pane (immutable).
    pub fn active_pane(&self) -> &PaneState {
        &self.panes[self.active_pane]
    }

    /// Get the active pane (mutable).
    pub fn active_pane_mut(&mut self) -> &mut PaneState {
        &mut self.panes[self.active_pane]
    }

    /// Get a specific pane by index (immutable), or `None` if out of bounds.
    pub fn pane(&self, idx: usize) -> Option<&PaneState> {
        self.panes.get(idx)
    }

    /// Get a specific pane by index (mutable), or `None` if out of bounds.
    pub fn pane_mut(&mut self, idx: usize) -> Option<&mut PaneState> {
        self.panes.get_mut(idx)
    }

    /// Get the rendering params for a specific pane.
    pub fn get_rendering_params_for_pane(&self, pane_idx: PaneId) -> Option<(RadarProduct, f32)> {
        self.panes.get(pane_idx)
            .and_then(|p| p.get_rendering_params())
    }

    /// Number of active panes.
    pub fn pane_count(&self) -> usize {
        self.pane_layout.pane_count
    }

    /// Split the map into `count` panes, as the settings UI's pane picker does.
    #[cfg(test)]
    pub(crate) fn set_pane_count_for_test(&mut self, count: usize) {
        while self.panes.len() < count {
            self.panes.push(PaneState::new());
        }
        self.pane_layout = PaneLayout::for_count(count);
        if self.active_pane >= count {
            self.active_pane = 0;
        }
    }

    /// The rect the pane grid was laid out in on the last frame.
    #[cfg(test)]
    pub(crate) fn map_panel_rect_for_test(&self) -> egui::Rect {
        self.last_map_panel_rect
    }

    /// The pane rects the layout produces inside the map panel, as
    /// `render_map` computes them.
    #[cfg(test)]
    pub(crate) fn pane_rects_for_test(&self) -> Vec<egui::Rect> {
        let panel = self.last_map_panel_rect;
        (0..self.pane_layout.pane_count)
            .map(|idx| self.pane_layout.pane_rect(idx, panel))
            .collect()
    }

    /// Whether viewport sync is enabled (all panes share the same map viewport).
    pub fn is_viewport_sync(&self) -> bool {
        self.viewport_sync
    }

    /// Whether layer sync is enabled (layer changes propagate to all panes).
    pub fn is_sync_layers(&self) -> bool {
        self.sync_layers
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

    /// Clear loading_site on all panes viewing the given site.
    pub fn clear_loading_site_for_site(&mut self, site: &str) {
        for pane in &mut self.panes {
            if pane.site == site {
                pane.loading_site = None;
                pane.radar_sites_render_gen = pane.radar_sites_render_gen.wrapping_add(1);
            }
        }
    }

    /// Bump the RadarSites texture generation on all panes (e.g. on theme change).
    pub fn bump_all_radar_sites_gen(&mut self) {
        for pane in &mut self.panes {
            pane.radar_sites_render_gen = pane.radar_sites_render_gen.wrapping_add(1);
        }
    }

    /// Set the user's GPS location for the blue dot indicator
    /// Set safe area insets in logical pixels (top, bottom, left, right).
    /// On Android, this compensates for the status bar and navigation bar.
    pub fn set_safe_area_insets(&mut self, top: f32, bottom: f32, left: f32, right: f32) {
        self.safe_area_insets = (top, bottom, left, right);
    }

    pub fn set_gps_fix(&mut self, fix: rustdar_gps::GpsFix) {
        self.user_fix = Some(fix);
    }

    pub fn set_user_heading(&mut self, heading: f32) {
        self.user_heading = Some(heading);
    }

    /// Whether the active pane is showing the most recent (live) scan.
    pub fn is_viewing_live(&self) -> bool {
        self.panes.get(self.active_pane).is_some_and(|p| p.viewing_live)
    }

    /// Whether any pane is viewing live (for auto-poll gating).
    pub fn is_any_pane_live(&self) -> bool {
        self.panes.iter().take(self.pane_layout.pane_count).any(|p| p.viewing_live)
    }

    /// Set live/historic viewing mode for a specific pane.
    pub fn set_viewing_live_for_pane(&mut self, pane_idx: usize, live: bool) {
        if let Some(pane) = self.panes.get_mut(pane_idx) {
            pane.viewing_live = live;
        }
    }

    /// Get the scan info for the active pane.
    pub fn get_scan_info(&self) -> Option<&ScanInfo> {
        self.panes.get(self.active_pane).and_then(|p| p.scan_info.as_ref())
    }

    /// Get the scan info for a specific pane.
    pub fn get_scan_info_for_pane(&self, pane_idx: usize) -> Option<&ScanInfo> {
        self.panes.get(pane_idx).and_then(|p| p.scan_info.as_ref())
    }

    /// Whether auto-poll is active and the event loop should keep waking
    pub fn is_auto_poll_active(&self) -> bool {
        self.auto_poll.is_active()
            || OverlayKind::all().iter().any(|&kind| {
                self.overlays.auto_poll_interval(kind).is_some()
                    && self.any_pane_has_overlay_enabled(kind)
            })
    }

    /// Whether any pane has a loop that is playing or has in-flight work.
    pub fn any_loop_active(&self) -> bool {
        self.panes.iter().any(|p| {
            let ls = &p.loop_state;
            ls.is_active() && (ls.is_playing() || ls.is_fetching() || ls.frames.iter().any(|f| f.render_in_flight))
        })
    }

    pub fn clear_graphics_state(&mut self) {
        for pane in &mut self.panes {
            pane.loading_site = None;
            pane.radar_sites_render_gen = pane.radar_sites_render_gen.wrapping_add(1);
            // Clear loop frame textures so they get re-rendered on resume.
            // The frame list and scan cache survive, so dispatch_loop_renders()
            // will re-upload textures automatically.
            for frame in &mut pane.loop_state.frames {
                frame.texture = None;
                frame.render_in_flight = false;
            }
            // Clear overlay texture caches — handles become invalid when the
            // egui context is destroyed. needs_rerender() will trigger fresh
            // background renders.
            for cache in pane.overlay_textures.values_mut() {
                cache.current = None;
                cache.render_in_flight = false;
            }
        }
        self.map_tiles.clear();
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
