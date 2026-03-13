#![cfg(target_os = "android")]

use crate::actions::GuiAction;
use crate::layers::LayerKind;
use crate::pane::{PaneLayout, PaneState, MAX_PANES_MOBILE};
use rustdar_overlays::spc::outlook::OutlookDay;

/// Detects a "double-tap and drag" gesture commonly used on touch devices
/// for one-handed zooming. The gesture flow is:
/// 1. Tap (short press-release)
/// 2. Within 400 ms, press down again and hold
/// 3. Drag vertically: up = zoom in, down = zoom out
#[derive(Clone)]
pub(super) struct DoubleTapDragDetector {
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

impl DoubleTapDragDetector {
    /// Process this frame's input and update the map zoom if a
    /// double-tap-drag gesture is active.
    pub(super) fn update(&mut self, ctx: &egui::Context, map_memory: &mut walkers::MapMemory) {
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
    pub(super) fn is_zooming(&self) -> bool {
        self.zooming
    }
}

impl super::Gui {
    pub(super) fn render_mobile_ui(&mut self, ctx: &egui::Context, actions: &mut Vec<GuiAction>) {
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
                "\u{2630}",
                egui::FontId::proportional(26.0),
                ctx.style().visuals.text_color(),
            );
            if response {
                self.show_mobile_menu = true;
            }
        }
    }

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
                        egui::Button::new("\u{1f504}").min_size(egui::vec2(44.0, 32.0))
                    );
                    if refresh_button.clicked() {
                        action = Some(GuiAction::FetchRadarScan(self.radar_config.clone()));
                    }
                    refresh_button.on_hover_text("Refresh radar data");

                    ui.separator();

                    // Status indicator
                    if self.fetching {
                        ui.label("\u{1f504} Loading...");
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
                        if ui.button("\u{2715}").clicked() {
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
                        if ui.button("\u{2715}").clicked() {
                            self.show_mobile_menu = false;
                        }
                    });
                });
                ui.separator();

                egui::ScrollArea::vertical().show(ui, |ui| {
                    // -- Pane count selector (mobile: 1-4) --
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

                    // -- Radar --
                    ui.checkbox(pane.layers.enabled_mut(LayerKind::Radar), "\u{1f6f0}  Radar");

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
                                            .selected_text(format!("{:.1}\u{b0}", selected_angle))
                                            .width(180.0)
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

                    // -- SPC Outlooks --
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
                                .add_enabled(!self.spc_fetching, egui::Button::new("\u{1f504} Refresh"))
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

                    // -- SPC Mesoscale Discussions --
                    {
                        let was_enabled = pane.layers.is_enabled(LayerKind::SpcMesoscaleDiscussions);
                        let label = if self.spc_discussions.is_empty() {
                            "\u{1f4cb}  Mesoscale Disc.".to_string()
                        } else {
                            format!("\u{1f4cb}  Mesoscale Disc. ({})", self.spc_discussions.len())
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
                                .add_enabled(!self.spc_md_fetching, egui::Button::new("\u{1f504} Refresh"))
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

                    // -- NWS Alerts --
                    ui.label("\u{26a0}  NWS Alerts");

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
                                .add_enabled(!self.nws_fetching, egui::Button::new("\u{1f504} Refresh"))
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

                    // -- Other overlays --
                    ui.checkbox(pane.layers.enabled_mut(LayerKind::CityLabels), "\u{1f3f7}  City Labels");
                    ui.checkbox(pane.layers.enabled_mut(LayerKind::RadarSites), "\u{1f4e1}  Radar Sites");

                    // -- Viewport sync toggle --
                    if self.pane_layout.pane_count > 1 {
                        ui.separator();
                        ui.checkbox(&mut self.viewport_sync, "\u{1f517}  Sync Viewports");
                        ui.checkbox(&mut self.sync_layers, "\u{1f517}  Sync Layers");
                    }

                    ui.add_space(10.0);
                    ui.separator();

                    // -- Controls --
                    ui.label("\u{2699}  Controls");
                    ui.add_space(4.0);

                    // Refresh
                    if ui
                        .add_enabled(!self.fetching, egui::Button::new("\u{1f504}  Refresh Radar"))
                        .clicked()
                    {
                        actions.push(GuiAction::FetchRadarScan(self.radar_config.clone()));
                    }

                    // Time
                    if ui.button("\u{1f550}  Set Time...").clicked() {
                        self.show_time_dialog = true;
                        self.show_mobile_menu = false; // close menu so dialog is visible
                    }

                    // Auto-poll
                    ui.checkbox(&mut self.auto_poll_enabled, "\u{23f0}  Auto-poll");

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
