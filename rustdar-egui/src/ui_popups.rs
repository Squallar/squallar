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
                // Section-indexed, not a fixed salt: two grids in one popup
                // used to share `"popup_kv_grid"`, so egui keyed both grids'
                // column-width state on one id and the second grid laid its
                // columns out to the first grid's widths.
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
    /// Render the overlay detail pager popup — the floating window the two
    /// wide widths get. On Compact the sheet's Feature page hosts the same
    /// body ([`Self::render_feature_page_body`]); the phone never draws this
    /// window (plan §1.9).
    ///
    /// Shows the currently selected overlay item with prev/next navigation
    /// when multiple overlays are stacked. Fully generic — uses PopupContent
    /// descriptors from the overlay crate.
    pub(super) fn render_overlay_popup(&mut self, ctx: &egui::Context) {
        if self.overlays.selected_overlays.is_empty() || self.layout.width == WidthClass::Compact {
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

        self.handle_triggered_popup_actions(&content, &triggered_actions, page);

        if closed {
            self.overlays.selected_overlays.clear();
            self.overlays.selected_overlay_page = 0;
        }
    }

    /// The current feature page's title and accent, for the sheet's title
    /// row — the same values the window above puts in its own title bar.
    /// `None` only when nothing is selected, in which case there is no
    /// Feature page to be titling.
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

    /// The feature dialog's content, host-free, for the sheet's Feature page:
    /// the overlap pager, the sections and the action buttons — the same
    /// pieces the window assembles, over the same handling, so the two
    /// presentations cannot mean different things. The page flag is
    /// `selected_overlays` itself; the sheet's ✕ and the dismissal chain
    /// clear it, so no close affordance is drawn here.
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
    ///
    /// The render loop above trusts `clicked()` per button, so nothing shapes
    /// `triggered` to one entry. egui 0.35's pointer path happens to: the
    /// interaction snapshot holds a single clicked id, and a focused button's
    /// Enter-click is re-read after a same-frame pointer click has surrendered
    /// its focus. But an AccessKit click request fires `clicked()` with no
    /// pointer involved, and none of that is this code's contract to lean on.
    /// Two entries used to both run: each removal indexes `page` into a vector
    /// the previous removal already shortened, so the second removed — or
    /// panicked on — a page nobody pressed a button for.
    fn handle_triggered_popup_actions(
        &mut self,
        content: &PopupContent,
        triggered: &[usize],
        page: usize,
    ) {
        #[cfg(test)]
        {
            self.last_popup_triggered = triggered.to_vec();
        }
        if let Some(&action_idx) = triggered.first()
            && let Some(action) = content.actions.get(action_idx)
        {
            #[cfg(test)]
            self.last_popup_handled.push(action_idx);
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
        OverlayItem, OverlayKind, PopupAction, PopupActionKind, PopupContent, PopupSection,
    };
    use std::sync::Arc;

    /// An overlay item whose popup is whatever the test says it is. The
    /// concrete items are `pub(crate)` to `rustdar-overlays`; the trait is not.
    #[derive(Debug)]
    struct StubItem(fn() -> PopupContent);

    impl OverlayItem for StubItem {
        fn kind(&self) -> OverlayKind {
            OverlayKind::NwsAlerts
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
    ///
    /// They used to share the fixed egui id `"popup_kv_grid"`, so both grids'
    /// column-width state lived under one key and the second grid indented its
    /// values to the first grid's much wider key column.
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
    ///
    /// The two-trigger frame is driven into the shipped handler directly
    /// rather than through synthetic input, because egui 0.35's pointer path
    /// cannot produce it (verified against the source: the interaction
    /// snapshot holds one clicked id, and a focused button's Enter-click is
    /// re-read after a same-frame pointer click has surrendered its focus) —
    /// while an AccessKit click request still can, and nothing makes the
    /// renderer's per-button `clicked()` loop one-entry by contract. The
    /// handler is what the guard lives in, so the handler is what is held.
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
