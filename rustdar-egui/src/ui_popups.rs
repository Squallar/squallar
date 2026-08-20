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

    for (idx, section) in content.sections.iter().enumerate() {
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
                egui::Grid::new(ui.id().with(("popup_kv", idx)))
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
                    .scroll_source(super::shell::panel_scroll_source())
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
    /// Render the overlay detail pager popup — the floating window the two
    /// wide widths get. On Compact the sheet's Feature page hosts the same
    /// body ([`Self::render_feature_page_body`]); the phone never draws this
    /// window (plan §1.9).
    pub(super) fn render_overlay_popup(&mut self, ctx: &egui::Context) {
        if self.overlays.selected_overlays.is_empty() || self.layout.width == WidthClass::Compact {
            return;
        }

        let count = self.overlays.selected_overlays.len();
        if self.overlays.selected_overlay_page >= count {
            self.overlays.selected_overlay_page = count - 1;
        }

        let page = self.overlays.selected_overlay_page;
        let current = self.overlays.selected_overlays[page].clone();

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

        self.handle_triggered_popup_actions(&content, &triggered_actions, page);

        if closed {
            self.overlays.selected_overlays.clear();
            self.overlays.selected_overlay_page = 0;
        }
    }

    /// The current feature page's title and accent, for the sheet's title
    /// row — the same values the window above puts in its own title bar.
    pub(super) fn feature_page_heading(&self) -> Option<(String, egui::Color32)> {
        let count = self.overlays.selected_overlays.len();
        if count == 0 {
            return None;
        }
        let page = self.overlays.selected_overlay_page.min(count - 1);
        let current = &self.overlays.selected_overlays[page];
        let content = self.overlays.popup_content(&**current, &self.preferences);
        Some((
            content.title,
            egui::Color32::from_rgb(
                content.accent_rgb[0],
                content.accent_rgb[1],
                content.accent_rgb[2],
            ),
        ))
    }

    /// The feature dialog's content, host-free, for the sheet's Feature page.
    pub(super) fn render_feature_page_body(&mut self, ui: &mut egui::Ui) {
        let count = self.overlays.selected_overlays.len();
        if count == 0 {
            return;
        }
        if self.overlays.selected_overlay_page >= count {
            self.overlays.selected_overlay_page = count - 1;
        }
        let page = self.overlays.selected_overlay_page;
        let current = self.overlays.selected_overlays[page].clone();
        let content = self.overlays.popup_content(&*current, &self.preferences);
        let layout = self.layout;

        if count > 1 {
            render_pager_nav(ui, page, count, &mut self.overlays.selected_overlay_page);
            ui.separator();
        }
        let triggered = render_popup_sections(ui, &layout, &content);
        self.handle_triggered_popup_actions(&content, &triggered, page);
    }

    /// Apply this frame's triggered action buttons — the **first one only**.
    fn handle_triggered_popup_actions(
        &mut self,
        content: &PopupContent,
        triggered: &[usize],
        page: usize,
    ) {
        #[cfg(test)]
        {
            self.probes.last_popup_triggered = triggered.to_vec();
        }
        if let Some(&action_idx) = triggered.first()
            && let Some(action) = content.actions.get(action_idx)
        {
            #[cfg(test)]
            self.probes.last_popup_handled.push(action_idx);
            let should_remove = self.overlays.handle_popup_action(action);
            if should_remove {
                self.overlays.selected_overlays.remove(page);
                if self.overlays.selected_overlays.is_empty() {
                    self.overlays.selected_overlay_page = 0;
                } else if self.overlays.selected_overlay_page
                    >= self.overlays.selected_overlays.len()
                {
                    self.overlays.selected_overlay_page = self.overlays.selected_overlays.len() - 1;
                }
            }
        }
    }
}

/// Render prev/next pager navigation controls.
fn render_pager_nav(ui: &mut egui::Ui, page: usize, count: usize, current_page: &mut usize) {
    ui.horizontal(|ui| {
        if ui
            .add_enabled(page > 0, egui::Button::new("\u{23f4}"))
            .clicked()
        {
            *current_page = page.saturating_sub(1);
        }
        ui.label(format!("{} / {}", page + 1, count));
        if ui
            .add_enabled(page + 1 < count, egui::Button::new("\u{23f5}"))
            .clicked()
        {
            *current_page = page + 1;
        }
    });
}

#[cfg(test)]
mod tests {
    use crate::input_harness::InputHarness;
    use rustdar_overlays::render::overlay_state::{
        OverlayItem, PopupAction, PopupActionKind, PopupContent, PopupSection,
    };
    use rustdar_source::id::{LayerId, known};
    use std::sync::Arc;

    /// An overlay item whose popup is whatever the test says it is. The
    /// concrete items are `pub(crate)` to `rustdar-overlays`; the trait is not.
    #[derive(Debug)]
    struct StubItem(fn() -> PopupContent);

    impl OverlayItem for StubItem {
        fn layer_id(&self) -> LayerId {
            known::NWS_ALERTS
        }
        fn popup_content(&self, _prefs: &rustdar_units::UserPreferences) -> PopupContent {
            (self.0)()
        }
        fn matches(&self, _other: &dyn OverlayItem) -> bool {
            false
        }
        fn as_any(&self) -> &dyn std::any::Any {
            self
        }
    }

    fn empty_content() -> PopupContent {
        PopupContent {
            title: "Empty".to_owned(),
            accent_rgb: [200, 60, 60],
            width: 320.0,
            sections: Vec::new(),
            actions: Vec::new(),
        }
    }

    /// Two key-value grids whose key columns differ wildly on purpose: the
    /// second grid's value lands close to its one-letter key only if the two
    /// grids lay their columns out independently.
    fn two_grids() -> PopupContent {
        PopupContent {
            title: "Two grids".to_owned(),
            accent_rgb: [200, 60, 60],
            width: 320.0,
            sections: vec![
                PopupSection::KeyValueGrid(vec![(
                    "A key much longer than the other grid has".to_owned(),
                    "first-grid-value".to_owned(),
                )]),
                PopupSection::KeyValueGrid(vec![("K".to_owned(), "second-grid-value".to_owned())]),
            ],
            actions: Vec::new(),
        }
    }

    /// Two grids in one popup keep their own column widths.
    #[test]
    fn each_kv_grid_in_a_popup_lays_out_its_own_columns() {
        let mut h = InputHarness::new();
        h.gui_mut().overlays.selected_overlays = vec![Arc::new(StubItem(two_grids))];
        h.warm_up();

        let rects = h.painted_text_rects();
        let value_left = |needle: &str| {
            rects
                .iter()
                .find(|(_, text)| text == needle)
                .unwrap_or_else(|| panic!("the popup never painted {needle:?}"))
                .0
                .left()
        };
        let first = value_left("first-grid-value");
        let second = value_left("second-grid-value");
        assert!(
            second < first,
            "the second grid's value column starts at x={second}, the first's \
             at x={first}: the one-letter-key grid inherited the long-key \
             grid's column widths, so the two grids are sharing one egui id"
        );
    }

    fn two_actions() -> PopupContent {
        let target: Arc<dyn OverlayItem> = Arc::new(StubItem(empty_content));
        PopupContent {
            title: "Two actions".to_owned(),
            accent_rgb: [200, 60, 60],
            width: 320.0,
            sections: vec![PopupSection::Text("body".to_owned())],
            actions: vec![
                PopupAction {
                    label: "First action".to_owned(),
                    target: target.clone(),
                    kind: PopupActionKind::HideFromMap,
                },
                PopupAction {
                    label: "Second action".to_owned(),
                    target,
                    kind: PopupActionKind::HideFromMap,
                },
            ],
        }
    }

    /// One frame handles at most one popup action, however many buttons the
    /// renderer reported as clicked.
    #[test]
    fn one_frame_handles_at_most_one_popup_action() {
        let mut gui = crate::Gui::new();
        gui.overlays.selected_overlays = vec![
            Arc::new(StubItem(two_actions)),
            Arc::new(StubItem(empty_content)),
        ];
        let content = two_actions();

        gui.handle_triggered_popup_actions(&content, &[0, 1], 0);

        let (triggered, handled) = gui.popup_actions_for_test();
        assert_eq!(triggered, vec![0, 1], "the probe lost the frame's report");
        assert_eq!(
            handled,
            vec![0],
            "one frame handled more than the first triggered action; a second \
             `remove(page)` would act on a vector the first already shortened"
        );
        assert_eq!(
            gui.overlays.selected_overlays.len(),
            2,
            "a stub action removes nothing — the registry has no real item to \
             hide — so any removal here means an action was double-applied"
        );
    }
}
