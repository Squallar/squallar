#![cfg(target_os = "android")]

use crate::actions::GuiAction;
use crate::ui::{PaneState, UserPreferences};
use crate::pane::{RadarImageData};
use rustdar_radar::types::ImageBounds;

// Gesture detection (double-tap-drag zoom, long press) lives in
// `crate::ui_input` — it is pure egui pointer/time logic with no Android APIs,
// so it stays compilable and testable on every target.

/// Vertical offset (pixels) from the touch point to the tooltip center.
pub(crate) const TOOLTIP_OFFSET_Y: f32 = 60.0;
/// Default width of the layers panel on mobile.
const LAYERS_PANEL_WIDTH: f32 = 260.0;
/// Width of combo boxes in the layers panel on mobile.
const COMBO_BOX_WIDTH: f32 = 180.0;

/// Draw a floating tooltip above the finger during a long-press on mobile,
/// showing the radar value at the touched position.
#[cfg(target_os = "android")]
pub fn draw_long_press_tooltip(
    ui: &egui::Ui,
    projector: &walkers::Projector,
    img: &RadarImageData,
    touch_pos: egui::Pos2,
    pane: &PaneState,
    prefs: &UserPreferences,
) {
    draw_long_press_tooltip_raw(ui, projector, &img.value_data, img.lat, img.lon, touch_pos, pane, prefs);
}

pub fn draw_long_press_tooltip_raw(
    ui: &egui::Ui,
    projector: &walkers::Projector,
    value_data: &[f32],
    lat: f64,
    lon: f64,
    touch_pos: egui::Pos2,
    pane: &PaneState,
    prefs: &UserPreferences,
) {
    use rustdar_radar::types::IMAGE_SIZE;

    let bounds = ImageBounds::from_radar_site(lat, lon);

    let nw = projector
        .project(walkers::lat_lon(bounds.max_lat, bounds.min_lon))
        .to_pos2();
    let se = projector
        .project(walkers::lat_lon(bounds.min_lat, bounds.max_lon))
        .to_pos2();
    let image_rect = egui::Rect::from_two_pos(nw, se);

    // Compute pixel coordinates inside the radar image
    let frac_x = (touch_pos.x - image_rect.left()) / image_rect.width();
    let frac_y = (touch_pos.y - image_rect.top()) / image_rect.height();
    let px = (frac_x * IMAGE_SIZE as f32) as i32;
    let py = (frac_y * IMAGE_SIZE as f32) as i32;

    let mut text = String::new();
    if px >= 0 && px < IMAGE_SIZE as i32 && py >= 0 && py < IMAGE_SIZE as i32 {
        let pixel_idx = py as usize * IMAGE_SIZE + px as usize;
        if pixel_idx < value_data.len() {
            let value = value_data[pixel_idx];
            if !value.is_nan() {
                text = pane.selected_product.format_value(value, prefs);
            }
        }
    }

    if text.is_empty() {
        text = "No data".into();
    }

    // Position tooltip above the finger
    let tooltip_pos = egui::pos2(touch_pos.x, touch_pos.y - super::mobile::TOOLTIP_OFFSET_Y);

    let painter = ui.painter();
    let font = egui::FontId::proportional(14.0);
    let galley = painter.layout_no_wrap(text, font, egui::Color32::WHITE);
    let text_size = galley.size();
    let padding = egui::vec2(8.0, 4.0);
    let bg_rect = egui::Rect::from_center_size(
        tooltip_pos,
        text_size + padding * 2.0,
    );

    painter.rect_filled(bg_rect, 4.0, egui::Color32::from_black_alpha(200));
    painter.galley(bg_rect.min + padding, galley, egui::Color32::WHITE);
}


impl super::Gui {
    pub(super) fn render_mobile_ui(&mut self, ui: &mut egui::Ui, actions: &mut Vec<GuiAction>) {
        let ctx = ui.ctx().clone();

        // Time dialog (shared between platforms)
        if let Some(a) = self.render_time_dialog(&ctx) {
            actions.push(a);
        }

        // Mobile status bar (simplified)
        if let Some(a) = self.render_mobile_status_bar(ui) {
            actions.push(a);
        }

        // Collapsible layers panel
        let layer_actions = self.render_mobile_layers_panel(ui);
        actions.extend(layer_actions);

        // Map in central panel
        let map_actions = self.render_map(ui);
        actions.extend(map_actions);

        // Floating hamburger button (uses Area so it participates in focus ordering
        // and popup windows can layer above it)
        if !self.mobile.show_menu {
            let top_inset = self.safe_area_insets.0;
            egui::Area::new(egui::Id::new("mobile_hamburger"))
                .order(egui::Order::Middle)
                .fixed_pos(egui::pos2(12.0, 48.0 + top_inset))
                .interactable(true)
                .show(&ctx, |ui| {
                    let (rect, response) = ui.allocate_exact_size(
                        egui::vec2(48.0, 48.0),
                        egui::Sense::click(),
                    );
                    let bg_color = if ui.style().visuals.dark_mode {
                        egui::Color32::from_rgba_unmultiplied(40, 40, 40, 220)
                    } else {
                        egui::Color32::from_rgba_unmultiplied(240, 240, 240, 230)
                    };
                    ui.painter().rect_filled(rect, 8.0, bg_color);
                    ui.painter().text(
                        rect.center(),
                        egui::Align2::CENTER_CENTER,
                        "\u{2630}",
                        egui::FontId::proportional(26.0),
                        ui.style().visuals.text_color(),
                    );
                    if response.clicked() {
                        self.mobile.show_menu = true;
                    }
                });
        }

        // Overlay detail pager popup (rendered after hamburger so it floats on top)
        self.render_overlay_popup(&ctx);
    }

    fn render_mobile_status_bar(&mut self, ui: &mut egui::Ui) -> Option<GuiAction> {
        let mut action = None;
        let top_inset = self.safe_area_insets.0;

        egui::Panel::top("mobile_status_bar")
            .min_size(32.0 + top_inset)
            .show_separator_line(true)
            .show_inside(ui, |ui| {
                // Add spacing to push content below the status bar cutout
                if top_inset > 0.0 {
                    ui.add_space(top_inset);
                }
                ui.horizontal_centered(|ui| {
                    // Refresh button
                    let refresh_button = ui.add_enabled(
                        !self.radar.fetching,
                        egui::Button::new("\u{1f504}").min_size(egui::vec2(44.0, 32.0))
                    );
                    if refresh_button.clicked() {
                        action = Some(GuiAction::FetchRadarScan(self.radar.config.clone()));
                    }
                    refresh_button.on_hover_text("Refresh radar data");

                    ui.separator();

                    // Status indicator
                    if self.radar.fetching {
                        ui.label("\u{1f504} Loading...");
                        ui.spinner();
                    } else if let Some(scan_info) = self.panes.get(self.active_pane).and_then(|p| p.scan_info.as_ref()) {
                        ui.label(format!("{} @ {}",
                            scan_info.site.name,
                            self.preferences.timezone.format_naive_utc(scan_info.timestamp, "%H:%M")
                        ));
                    } else {
                        ui.label("No scan loaded");
                    }

                    ui.separator();

                    // Error message (if any)
                    let mut dismiss_error = false;
                    if let Some(error_msg) = &self.radar.error_message {
                        if ui.button("\u{2715}").clicked() {
                            dismiss_error = true;
                        }
                        ui.label(error_msg.as_str());
                    }
                    if dismiss_error { self.radar.error_message = None; }
                });
            });

        action
    }

    /// Collapsible layers/controls panel for mobile (replaces bottom toolbar).
    fn render_mobile_layers_panel(&mut self, ui: &mut egui::Ui) -> Vec<GuiAction> {
        let mut actions = Vec::new();
        if !self.mobile.show_menu {
            return actions;
        }

        let top_inset = self.safe_area_insets.0;
        let bottom_inset = self.safe_area_insets.1;

        let mut pane = std::mem::take(&mut self.panes[self.active_pane]);

        egui::Panel::left("mobile_layers_panel")
            .default_size(LAYERS_PANEL_WIDTH)
            .resizable(false)
            .show_inside(ui, |ui| {
                // Safe-area top padding
                if top_inset > 0.0 {
                    ui.add_space(top_inset);
                }

                // Header with close button
                ui.horizontal(|ui| {
                    ui.heading("Layers");
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("\u{2715}").clicked() {
                            self.mobile.show_menu = false;
                        }
                    });
                });
                ui.separator();

                egui::ScrollArea::vertical().show(ui, |ui| {
                    self.render_pane_selector(ui, &mut pane);

                    self.render_layer_controls(ui, &mut pane, COMBO_BOX_WIDTH, "m_", &mut actions);

                    ui.add_space(10.0);
                    ui.separator();

                    // -- Controls --
                    ui.label("\u{2699}  Controls");
                    ui.add_space(4.0);

                    // Refresh
                    if ui
                        .add_enabled(!self.radar.fetching, egui::Button::new("\u{1f504}  Refresh Radar"))
                        .clicked()
                    {
                        actions.push(GuiAction::FetchRadarScan(self.radar.config.clone()));
                    }

                    // Time
                    if ui.button("\u{1f550}  Set Time...").clicked() {
                        self.time_dialog.show = true;
                        self.mobile.show_menu = false; // close menu so dialog is visible
                    }

                    // Settings
                    if ui.button("\u{2699}\u{fe0f}  Settings...").clicked() {
                        self.show_settings = true;
                        self.mobile.show_menu = false;
                    }

                    // Auto-poll
                    ui.checkbox(&mut self.auto_poll.enabled, "\u{23f0}  Auto-poll");

                    if self.radar.fetching {
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
        self.propagate_layer_sync();

        actions
    }
}
