use rustdar_overlays::render::overlay_state::{PopupContent, PopupSection};

use crate::ui_layout::{LayoutCtx, WidthClass};

/// Body text sizes, picked from the width class rather than the target OS: a
/// narrow window on a desktop wants the tighter type just as much as a phone
/// does, and a tablet does not.
fn heading_size(layout: &LayoutCtx) -> f32 {
    if layout.width == WidthClass::Compact {
        13.0
    } else {
        14.0
    }
}

fn monospace_size(layout: &LayoutCtx) -> f32 {
    if layout.width == WidthClass::Compact {
        11.0
    } else {
        12.0
    }
}

/// Show a centered detail popup window sized for the current layout.
///
/// Returns `true` if the user closed the popup (via the X button).
fn show_detail_popup(
    ctx: &egui::Context,
    layout: &LayoutCtx,
    id: &str,
    title: egui::RichText,
    roomy_width: f32,
    body: impl FnOnce(&mut egui::Ui),
) -> bool {
    let popup_width = layout.dialog_width(roomy_width);
    let mut open = true;
    egui::Window::new(title)
        .id(egui::Id::new(id))
        .open(&mut open)
        .collapsible(false)
        .resizable(true)
        // Since egui 0.35 (#7725, "rework `Window` margins") this is the window's
        // OUTER width — it used to size the content. Content is now narrower by
        // 2 x (`spacing.window_margin` + `visuals.window_stroke`), 14px at the
        // stock theme. Deliberately not compensated: when compact `popup_width`
        // is `content - 32`, which reads as "a 16px gutter each side", and only
        // the new meaning actually delivers that. Adding 14 back would restore
        // the old content width but hardcode a theme-derived constant that rots
        // the moment the style changes.
        .default_width(popup_width)
        .pivot(egui::Align2::CENTER_CENTER)
        .default_pos(layout.dialog_center())
        .order(egui::Order::Foreground)
        .show(ctx, |ui| body(ui));

    !open
}

/// Render popup sections generically. Returns indices of triggered actions.
fn render_popup_sections(
    ui: &mut egui::Ui,
    layout: &LayoutCtx,
    content: &PopupContent,
) -> Vec<usize> {
    let mut triggered = Vec::new();

    for section in &content.sections {
        match section {
            PopupSection::Heading(text) => {
                ui.label(
                    egui::RichText::new(text)
                        .strong()
                        .size(heading_size(layout)),
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
                egui::Grid::new("popup_kv_grid")
                    .num_columns(2)
                    .show(ui, |ui| {
                        for (key, value) in rows {
                            ui.label(egui::RichText::new(format!("{}:", key)).strong());
                            ui.add(egui::Label::new(value).wrap());
                            ui.end_row();
                        }
                    });
            }
            PopupSection::ScrollableText {
                text,
                monospace,
                max_height,
            } => {
                egui::ScrollArea::vertical()
                    .max_height(*max_height)
                    .show(ui, |ui| {
                        let rt = if *monospace {
                            egui::RichText::new(text)
                                .font(egui::FontId::monospace(monospace_size(layout)))
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
        let layout = self.layout;
        let closed = show_detail_popup(
            ctx,
            &layout,
            "overlay_pager_popup",
            egui::RichText::new(&content.title).color(accent).strong(),
            content.width,
            |ui| {
                if count > 1 {
                    render_pager_nav(ui, page, count, &mut self.overlays.selected_overlay_page);
                    ui.separator();
                }
                triggered_actions = render_popup_sections(ui, &layout, &content);
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
                    } else if self.overlays.selected_overlay_page
                        >= self.overlays.selected_overlays.len()
                    {
                        self.overlays.selected_overlay_page =
                            self.overlays.selected_overlays.len() - 1;
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
        if ui
            .add_enabled(page > 0, egui::Button::new("\u{25c0}"))
            .clicked()
        {
            *current_page = page.saturating_sub(1);
        }
        ui.label(format!("{} / {}", page + 1, count));
        if ui
            .add_enabled(page + 1 < count, egui::Button::new("\u{25b6}"))
            .clicked()
        {
            *current_page = page + 1;
        }
    });
}
