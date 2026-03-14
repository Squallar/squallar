#![cfg(target_os = "android")]

use crate::actions::GuiAction;
use crate::pane::PaneState;

/// Maximum time (seconds) between first tap release and second press
/// for it to count as a double-tap.
const DOUBLE_TAP_TIMEOUT_S: f64 = 0.4;
/// Maximum distance (pixels) between first and second tap positions
/// for it to count as a double-tap.
const DOUBLE_TAP_DISTANCE_PX: f32 = 50.0;
/// Maximum duration (seconds) for a press-release to classify as a "tap".
const TAP_DURATION_MAX_S: f64 = 0.3;
/// Maximum movement (pixels) for a press-release to classify as a "tap".
const TAP_DISTANCE_MAX_PX: f32 = 20.0;
/// Pixels of vertical drag per 1.0 zoom level change.
const ZOOM_DRAG_SENSITIVITY: f32 = 150.0;

/// Detects a "double-tap and drag" gesture commonly used on touch devices
/// for one-handed zooming. The gesture flow is:
/// 1. Tap (short press-release)
/// 2. Within [`DOUBLE_TAP_TIMEOUT_S`], press down again and hold
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
                let zoom_delta = dy as f64 / ZOOM_DRAG_SENSITIVITY as f64;
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
                if dt < DOUBLE_TAP_TIMEOUT_S && dist < DOUBLE_TAP_DISTANCE_PX {
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
            if duration < TAP_DURATION_MAX_S && distance < TAP_DISTANCE_MAX_PX {
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
                    } else if let Some(scan_info) = &self.radar.scan_info {
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
    fn render_mobile_layers_panel(&mut self, ctx: &egui::Context) -> Vec<GuiAction> {
        let mut actions = Vec::new();
        if !self.show_mobile_menu {
            return actions;
        }

        let top_inset = self.safe_area_insets.0;
        let bottom_inset = self.safe_area_insets.1;

        let mut pane = std::mem::take(&mut self.panes[self.active_pane]);

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
                    self.render_pane_selector(ui, &mut pane);

                    self.render_layer_controls(ui, &mut pane, 180.0, "m_", &mut actions);

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
                        self.show_mobile_menu = false; // close menu so dialog is visible
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
