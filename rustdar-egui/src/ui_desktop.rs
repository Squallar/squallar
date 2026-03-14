#![cfg(not(target_os = "android"))]

use crate::actions::GuiAction;
use rustdar_overlays::render::layers::LayerKind;
use crate::pane::PaneState;

use egui::Context;
use rustdar_radar::types::ScanInfo;

fn render_auto_poll_status(
    ui: &mut egui::Ui,
    fetching: bool,
    auto_poll: &mut super::AutoPollState,
) {
    if fetching {
        ui.label("🔄");
        ui.label("Downloading");
        ui.spinner();
    } else if auto_poll.enabled {
        if let Some(remaining) = auto_poll.time_until_next() {
            ui.checkbox(&mut auto_poll.enabled, &format!("Auto-poll (next in {}s)", remaining));
        } else {
            ui.checkbox(&mut auto_poll.enabled, "Auto-poll");
        }
    } else {
        ui.checkbox(&mut auto_poll.enabled, "Auto-poll");
    }
}

fn render_scan_info(ui: &mut egui::Ui, scan_info: Option<&ScanInfo>) {
    if let Some(scan_info) = scan_info {
        ui.label(format!(
            "Scan: {} @ {} UTC ({} products)",
            scan_info.site.name,
            scan_info.timestamp.format("%Y-%m-%d %H:%M:%S"),
            scan_info.available_products.len()
        ));
    } else {
        ui.label("No scan loaded");
    }
}

fn render_hover_info(ui: &mut egui::Ui, panes: &[PaneState]) {
    let hover_info = panes.iter().find_map(|p| p.hover_value.as_ref());
    if let Some(hover_info) = hover_info {
        ui.label("📍");
        ui.label(hover_info);
    } else {
        ui.label("");
    }
}

fn render_error_display(ui: &mut egui::Ui, error_message: &mut Option<String>) {
    let mut dismiss = false;
    if let Some(msg) = error_message.as_deref() {
        if ui.button("✕").clicked() {
            dismiss = true;
        }
        ui.label(msg);
        ui.label("❌");
    }
    if dismiss {
        *error_message = None;
    }
}

impl super::Gui {
    pub(super) fn render_menu_bar(&mut self, ctx: &Context, action: &mut Option<GuiAction>) {
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
                        self.time_dialog.show = true;
                        ui.close_kind(egui::UiKind::Menu);
                    }
                });
            });
        });
    }

    pub(super) fn render_status_bar(&mut self, ctx: &Context) -> Option<GuiAction> {
        let mut action = None;
        
        egui::TopBottomPanel::bottom("status_bar")
            .show_separator_line(true)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 8.0;

                    // Refresh button
                    let refresh_button = ui.add_enabled(
                        !self.radar.fetching,
                        egui::Button::new("🔄").frame(false)
                    );
                    if refresh_button.clicked() {
                        action = Some(GuiAction::FetchRadarScan(self.radar.config.clone()));
                    }
                    refresh_button.on_hover_text("Refresh radar data");

                    ui.separator();

                    render_auto_poll_status(ui, self.radar.fetching, &mut self.auto_poll);

                    ui.separator();

                    render_scan_info(ui, self.radar.scan_info.as_ref());

                    ui.separator();

                    render_hover_info(ui, &self.panes);

                    // Add flexible space to push error to the right
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        render_error_display(ui, &mut self.radar.error_message);
                    });
                });
            });
        
        action
    }

    /// Render the layers panel on the left side (desktop).
    pub(super) fn render_layers_panel(&mut self, ctx: &Context) -> Vec<GuiAction> {
        let mut actions = Vec::new();
        let mut pane = std::mem::take(&mut self.panes[self.active_pane]);

        egui::SidePanel::left("layers_panel")
            .default_width(170.0)
            .resizable(false)
            .show(ctx, |ui| {
                ui.heading("Layers");
                ui.separator();

                self.render_pane_selector(ui, &mut pane);

                self.render_layer_controls(ui, &mut pane, 120.0, "d_", &mut actions);
            });

        self.panes[self.active_pane] = pane;
        self.propagate_layer_sync();

        actions
    }

    pub(super) fn render_desktop_ui(&mut self, ctx: &egui::Context, actions: &mut Vec<GuiAction>) {
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

        // Overlay detail pager popup (rendered after map so it floats on top)
        self.render_overlay_popup(ctx);
    }
}
