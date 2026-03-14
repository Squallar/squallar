use rustdar_overlays::spc::colors::md_stroke_color;
use rustdar_overlays::nws::alert::NwsAlert;
use rustdar_overlays::spc::discussion::SpcDiscussion;
use rustdar_overlays::render::overlay_state::SelectedOverlay;

/// Format an ISO 8601 timestamp into a shorter human-readable form.
/// Falls back to displaying the raw string on parse errors.
pub(super) fn format_iso_time(iso: &str) -> String {
    chrono::DateTime::parse_from_rfc3339(iso)
        .map(|dt| dt.format("%b %d %Y %H:%M %Z").to_string())
        .unwrap_or_else(|_| iso.to_string())
}

const IS_MOBILE: bool = cfg!(target_os = "android");

/// Show a centered detail popup window with platform-appropriate sizing.
///
/// Returns `true` if the user closed the popup (via the X button).
fn show_detail_popup(
    ctx: &egui::Context,
    id: &str,
    title: egui::RichText,
    desktop_width: f32,
    body: impl FnOnce(&mut egui::Ui),
) -> bool {
    let screen = ctx.input(|i| i.viewport_rect());
    let popup_width = if IS_MOBILE {
        (screen.width() - 32.0).max(200.0)
    } else {
        desktop_width
    };
    let popup_max_height = if IS_MOBILE {
        (screen.height() - 80.0).max(200.0)
    } else {
        500.0
    };

    let mut open = true;
    egui::Window::new(title)
        .id(egui::Id::new(id))
        .open(&mut open)
        .collapsible(false)
        .resizable(!IS_MOBILE)
        .default_width(popup_width)
        .max_width(popup_width)
        .max_height(popup_max_height)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| body(ui));

    !open
}

/// Render the content of an NWS alert detail popup.
/// Returns `true` if the "Hide from map" button was clicked.
fn alert_popup_content(ui: &mut egui::Ui, alert: &NwsAlert, accent: egui::Color32) -> bool {
    // Headline
    if let Some(headline) = &alert.headline {
        ui.label(egui::RichText::new(headline).strong().size(if IS_MOBILE { 13.0 } else { 14.0 }));
        ui.add_space(4.0);
    }

    // Metadata grid
    egui::Grid::new("alert_meta").num_columns(2).show(ui, |ui| {
        ui.label(egui::RichText::new("Areas:").strong());
        ui.add(egui::Label::new(&alert.area_desc).wrap());
        ui.end_row();

        ui.label(egui::RichText::new("Issued by:").strong());
        ui.add(egui::Label::new(&alert.sender_name).wrap());
        ui.end_row();

        ui.label(egui::RichText::new("Effective:").strong());
        ui.label(format_iso_time(&alert.effective));
        ui.end_row();

        ui.label(egui::RichText::new("Expires:").strong());
        ui.label(format_iso_time(&alert.expires));
        ui.end_row();
    });

    ui.separator();

    // Description (scrollable)
    egui::ScrollArea::vertical()
        .max_height(250.0)
        .show(ui, |ui| {
            ui.label(&alert.description);
        });

    // Instruction (emphasized)
    if let Some(instruction) = &alert.instruction {
        ui.add_space(4.0);
        ui.separator();
        ui.label(
            egui::RichText::new(instruction)
                .strong()
                .color(accent),
        );
    }

    ui.add_space(6.0);
    ui.separator();
    ui.button("\u{1f6ab}  Hide from map").clicked()
}

/// Render the content of an SPC Mesoscale Discussion popup.
fn md_popup_content(ui: &mut egui::Ui, md: &SpcDiscussion, accent: egui::Color32) {
    // Type badge
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(format!("Type: {}", md.md_type)).strong().color(accent));
    });

    // Concerning line
    if let Some(ref concerning) = md.concerning {
        ui.add_space(2.0);
        ui.label(egui::RichText::new(format!("Concerning: {}", concerning)).strong());
    }

    ui.add_space(4.0);
    ui.separator();

    // Full discussion text (scrollable)
    egui::ScrollArea::vertical()
        .max_height(if IS_MOBILE { 300.0 } else { 350.0 })
        .show(ui, |ui| {
            ui.label(
                egui::RichText::new(&md.text)
                    .font(egui::FontId::monospace(if IS_MOBILE { 11.0 } else { 12.0 }))
            );
        });

    ui.add_space(4.0);
    ui.separator();

    // Link to SPC
    if !md.link.is_empty() {
        ui.hyperlink_to("Open on SPC website", &md.link);
    }
}

impl super::Gui {
    /// Render the overlay detail pager popup.
    ///
    /// Shows the currently selected overlay item (alert or MD) with
    /// prev/next navigation when multiple overlays are stacked.
    pub(super) fn render_overlay_popup(&mut self, ctx: &egui::Context) {
        if self.overlays.selected_overlays.is_empty() {
            return;
        }

        // Clamp page index
        let count = self.overlays.selected_overlays.len();
        if self.overlays.selected_overlay_page >= count {
            self.overlays.selected_overlay_page = count - 1;
        }

        let page = self.overlays.selected_overlay_page;
        let current = self.overlays.selected_overlays[page].clone();

        match current {
            SelectedOverlay::Alert(idx) => {
                let Some(alert) = self.overlays.nws_alerts.data.get(idx) else {
                    self.overlays.selected_overlays.clear();
                    return;
                };

                let alert_id = alert.id.clone();
                let [r, g, b, _] = alert.features.first()
                    .map(|f| f.stroke_rgba)
                    .unwrap_or([200, 200, 200, 255]);
                let accent = egui::Color32::from_rgb(r, g, b);

                let mut hide_clicked = false;
                let closed = show_detail_popup(
                    ctx,
                    "overlay_pager_popup",
                    egui::RichText::new(&alert.event).color(accent).strong(),
                    380.0,
                    |ui| {
                        if count > 1 {
                            render_pager_nav(ui, page, count, &mut self.overlays.selected_overlay_page);
                            ui.separator();
                        }
                        hide_clicked = alert_popup_content(ui, alert, accent);
                    },
                );

                if hide_clicked {
                    self.overlays.hidden_alerts.insert(alert_id);
                    self.overlays.nws_alerts.data_generation =
                        self.overlays.nws_alerts.data_generation.wrapping_add(1);
                    // Remove this alert from the pager
                    self.overlays.selected_overlays.remove(page);
                    if self.overlays.selected_overlays.is_empty() {
                        self.overlays.selected_overlay_page = 0;
                    } else if self.overlays.selected_overlay_page >= self.overlays.selected_overlays.len() {
                        self.overlays.selected_overlay_page = self.overlays.selected_overlays.len() - 1;
                    }
                }
                if closed {
                    self.overlays.selected_overlays.clear();
                    self.overlays.selected_overlay_page = 0;
                }
            }
            SelectedOverlay::Discussion(idx) => {
                let Some(md) = self.overlays.spc_discussions.data.get(idx) else {
                    self.overlays.selected_overlays.clear();
                    return;
                };

                let stroke_rgba = md_stroke_color(&md.md_type);
                let [r, g, b, _] = stroke_rgba;
                let accent = egui::Color32::from_rgb(r, g, b);

                let title = format!("Mesoscale Discussion #{:04}", md.number);
                let closed = show_detail_popup(
                    ctx,
                    "overlay_pager_popup",
                    egui::RichText::new(&title).color(accent).strong(),
                    420.0,
                    |ui| {
                        if count > 1 {
                            render_pager_nav(ui, page, count, &mut self.overlays.selected_overlay_page);
                            ui.separator();
                        }
                        md_popup_content(ui, md, accent);
                    },
                );

                if closed {
                    self.overlays.selected_overlays.clear();
                    self.overlays.selected_overlay_page = 0;
                }
            }
        }
    }
}

/// Render prev/next pager navigation controls.
fn render_pager_nav(ui: &mut egui::Ui, page: usize, count: usize, current_page: &mut usize) {
    ui.horizontal(|ui| {
        if ui.add_enabled(page > 0, egui::Button::new("\u{25c0}")).clicked() {
            *current_page = page.saturating_sub(1);
        }
        ui.label(format!("{} / {}", page + 1, count));
        if ui.add_enabled(page + 1 < count, egui::Button::new("\u{25b6}")).clicked() {
            *current_page = page + 1;
        }
    });
}
