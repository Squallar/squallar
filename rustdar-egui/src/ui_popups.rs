use rustdar_overlays::spc::colors::md_stroke_color;

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

impl super::Gui {
    /// Render the NWS alert detail popup when an alert is selected.
    pub(super) fn render_alert_popup(&mut self, ctx: &egui::Context) {
        let Some(idx) = self.overlays.selected_alert else {
            return;
        };
        let Some(alert) = self.overlays.nws_alerts.data.get(idx) else {
            self.overlays.selected_alert = None;
            return;
        };

        // Clone data needed for the popup to avoid borrowing issues
        let alert_id = alert.id.clone();
        let event = alert.event.clone();
        let headline = alert.headline.clone();
        let area_desc = alert.area_desc.clone();
        let sender_name = alert.sender_name.clone();
        let effective = alert.effective.clone();
        let expires = alert.expires.clone();
        let description = alert.description.clone();
        let instruction = alert.instruction.clone();
        let [r, g, b, _] = alert.features.first()
            .map(|f| f.stroke_rgba)
            .unwrap_or([200, 200, 200, 255]);
        let accent = egui::Color32::from_rgb(r, g, b);

        let mut hide_clicked = false;
        let closed = show_detail_popup(
            ctx,
            "nws_alert_popup",
            egui::RichText::new(&event).color(accent).strong(),
            380.0,
            |ui| {
                // Headline
                if let Some(headline) = &headline {
                    ui.label(egui::RichText::new(headline).strong().size(if IS_MOBILE { 13.0 } else { 14.0 }));
                    ui.add_space(4.0);
                }

                // Metadata grid
                egui::Grid::new("alert_meta").num_columns(2).show(ui, |ui| {
                    ui.label(egui::RichText::new("Areas:").strong());
                    ui.add(egui::Label::new(&area_desc).wrap());
                    ui.end_row();

                    ui.label(egui::RichText::new("Issued by:").strong());
                    ui.add(egui::Label::new(&sender_name).wrap());
                    ui.end_row();

                    ui.label(egui::RichText::new("Effective:").strong());
                    ui.label(format_iso_time(&effective));
                    ui.end_row();

                    ui.label(egui::RichText::new("Expires:").strong());
                    ui.label(format_iso_time(&expires));
                    ui.end_row();
                });

                ui.separator();

                // Description (scrollable)
                egui::ScrollArea::vertical()
                    .max_height(250.0)
                    .show(ui, |ui| {
                        ui.label(&description);
                    });

                // Instruction (emphasized)
                if let Some(instruction) = &instruction {
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
                if ui.button("\u{1f6ab}  Hide from map").clicked() {
                    hide_clicked = true;
                }
            },
        );

        if hide_clicked {
            self.overlays.hidden_alerts.insert(alert_id);
            self.overlays.nws_alerts.data_generation = self.overlays.nws_alerts.data_generation.wrapping_add(1);
            self.overlays.selected_alert = None;
        }
        if closed {
            self.overlays.selected_alert = None;
        }
    }

    /// Render the SPC Mesoscale Discussion detail popup when an MD is selected.
    pub(super) fn render_md_popup(&mut self, ctx: &egui::Context) {
        let Some(idx) = self.overlays.selected_md else {
            return;
        };
        let Some(md) = self.overlays.spc_discussions.data.get(idx) else {
            self.overlays.selected_md = None;
            return;
        };

        // Clone data to avoid borrow issues
        let number = md.number;
        let md_type = md.md_type;
        let concerning = md.concerning.clone();
        let text = md.text.clone();
        let link = md.link.clone();
        let stroke_rgba = md_stroke_color(&md_type);
        let [r, g, b, _] = stroke_rgba;
        let accent = egui::Color32::from_rgb(r, g, b);

        let title = format!("Mesoscale Discussion #{:04}", number);
        let closed = show_detail_popup(
            ctx,
            "spc_md_popup",
            egui::RichText::new(&title).color(accent).strong(),
            420.0,
            |ui| {
                // Type badge
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new(format!("Type: {}", md_type)).strong().color(accent));
                });

                // Concerning line
                if let Some(ref concerning) = concerning {
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
                            egui::RichText::new(&text)
                                .font(egui::FontId::monospace(if IS_MOBILE { 11.0 } else { 12.0 }))
                        );
                    });

                ui.add_space(4.0);
                ui.separator();

                // Link to SPC
                if !link.is_empty() {
                    ui.hyperlink_to("Open on SPC website", &link);
                }
            },
        );

        if closed {
            self.overlays.selected_md = None;
        }
    }
}
