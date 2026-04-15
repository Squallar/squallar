use rustdar_overlays::render::overlay_state::{PopupContent, PopupSection};

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
        .order(egui::Order::Foreground)
        .show(ctx, |ui| body(ui));

    !open
}

/// Render popup sections generically. Returns indices of triggered actions.
fn render_popup_sections(
    ui: &mut egui::Ui,
    content: &PopupContent,
) -> Vec<usize> {
    let mut triggered = Vec::new();

    for section in &content.sections {
        match section {
            PopupSection::Heading(text) => {
                ui.label(
                    egui::RichText::new(text)
                        .strong()
                        .size(if IS_MOBILE { 13.0 } else { 14.0 }),
                );
                ui.add_space(4.0);
            }
            PopupSection::Text(text) => {
                ui.label(text);
            }
            PopupSection::ColoredText { text, rgb, bold } => {
                let color = egui::Color32::from_rgb(rgb[0], rgb[1], rgb[2]);
                let mut rt = egui::RichText::new(text).color(color);
                if *bold {
                    rt = rt.strong();
                }
                ui.label(rt);
            }
            PopupSection::KeyValueGrid(rows) => {
                egui::Grid::new("popup_kv_grid").num_columns(2).show(ui, |ui| {
                    for (key, value) in rows {
                        ui.label(egui::RichText::new(format!("{}:", key)).strong());
                        ui.add(egui::Label::new(value).wrap());
                        ui.end_row();
                    }
                });
            }
            PopupSection::ScrollableText { text, monospace, max_height } => {
                egui::ScrollArea::vertical()
                    .max_height(*max_height)
                    .show(ui, |ui| {
                        let rt = if *monospace {
                            egui::RichText::new(text)
                                .font(egui::FontId::monospace(if IS_MOBILE { 11.0 } else { 12.0 }))
                        } else {
                            egui::RichText::new(text)
                        };
                        ui.label(rt);
                    });
            }
            PopupSection::Separator => {
                ui.separator();
            }
            PopupSection::Link { label, url } => {
                ui.hyperlink_to(label, url);
            }
        }
    }

    // Action buttons
    if !content.actions.is_empty() {
        ui.add_space(6.0);
        ui.separator();
        for (i, action) in content.actions.iter().enumerate() {
            if ui.button(&action.label).clicked() {
                triggered.push(i);
            }
        }
    }

    triggered
}

impl super::Gui {
    /// Render the overlay detail pager popup.
    ///
    /// Shows the currently selected overlay item with prev/next navigation
    /// when multiple overlays are stacked. Fully generic — uses PopupContent
    /// descriptors from the overlay crate.
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

        // Build popup content from overlay data
        let content = self.overlays.popup_content(&*current, &self.preferences);

        let accent = egui::Color32::from_rgb(
            content.accent_rgb[0],
            content.accent_rgb[1],
            content.accent_rgb[2],
        );

        let mut triggered_actions: Vec<usize> = Vec::new();
        let closed = show_detail_popup(
            ctx,
            "overlay_pager_popup",
            egui::RichText::new(&content.title).color(accent).strong(),
            content.width,
            |ui| {
                if count > 1 {
                    render_pager_nav(ui, page, count, &mut self.overlays.selected_overlay_page);
                    ui.separator();
                }
                triggered_actions = render_popup_sections(ui, &content);
            },
        );

        // Process any triggered actions
        for &action_idx in &triggered_actions {
            if let Some(action) = content.actions.get(action_idx) {
                let should_remove = self.overlays.handle_popup_action(action);
                if should_remove {
                    self.overlays.selected_overlays.remove(page);
                    if self.overlays.selected_overlays.is_empty() {
                        self.overlays.selected_overlay_page = 0;
                    } else if self.overlays.selected_overlay_page >= self.overlays.selected_overlays.len() {
                        self.overlays.selected_overlay_page = self.overlays.selected_overlays.len() - 1;
                    }
                }
            }
        }

        if closed {
            self.overlays.selected_overlays.clear();
            self.overlays.selected_overlay_page = 0;
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
