#![cfg(not(target_os = "android"))]

use crate::actions::GuiAction;
use crate::layers::LayerKind;
use crate::pane::{PaneLayout, PaneState, MAX_PANES_DESKTOP, MAX_PANES_MOBILE};
use egui::Context;

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

                    // Unified auto-poll checkbox and status
                    if self.radar.fetching {
                        ui.label("🔄");
                        ui.label("Downloading");
                        ui.spinner();
                    } else if self.auto_poll.enabled {
                        // Show time until next poll with checkbox
                        if let Some(remaining) = self.auto_poll.time_until_next() {
                            ui.checkbox(&mut self.auto_poll.enabled, &format!("Auto-poll (next in {}s)", remaining));
                        } else {
                            ui.checkbox(&mut self.auto_poll.enabled, "Auto-poll");
                        }
                    } else {
                        ui.checkbox(&mut self.auto_poll.enabled, "Auto-poll");
                    }

                    ui.separator();

                    // Scan information
                    if let Some(scan_info) = &self.radar.scan_info {
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
                        if let Some(error_msg) = &self.radar.error_message {
                            if ui.button("✕").clicked() {
                                dismiss_error = true;
                            }
                            ui.label(error_msg.as_str());
                            ui.label("❌");
                        }
                        if dismiss_error { self.radar.error_message = None; }
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

        // NWS alert detail popup (rendered after map so it floats on top)
        self.render_alert_popup(ctx);

        // SPC Mesoscale Discussion detail popup
        self.render_md_popup(ctx);
    }
}
