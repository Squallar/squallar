//! The docked top bar: the one piece of chrome that claims space at the top, at
//! every width.

use super::ui_menu;
use crate::actions::GuiAction;

/// The app-menu button's glyph — the whole menu lives behind it.
const MENU_BUTTON_LABEL: &str = "\u{2630}";
/// The layers-panel toggle. Selected-state styled while the panel is open.
const LAYERS_TOGGLE_LABEL: &str = "\u{25a3} Layers";
/// The cross-section arm toggle.
const SECTION_TOGGLE_LABEL: &str = "\u{2215} X-sec";
/// The 3D region arm toggle, [`SECTION_TOGGLE_LABEL`]'s sibling and styled
/// identically: the two are the bar's armed-drag pair and a user has to be able to
/// see at a glance which of them is lit.
const REGION_TOGGLE_LABEL: &str = "\u{26f6} Region";
/// The offline-download arm toggle, the third of the bar's armed-drag set.
///
/// **Icon-only at every width, unlike its two siblings**, and that is a
/// measurement rather than a taste: the bar's roomy/tight decision is made
/// from what the right-hand run leaves, and a fourth worded toggle takes
/// Medium's floor (600 pt) past the point where the pane segments keep even
/// their tight captions — which is `the_bar_never_overlaps_at_mediums_
/// narrowest_width`'s exact finding. The word lives in the hover text and in
/// the menu leaf, both of which the parity walk keeps reachable at every
/// width.
const OFFLINE_TOGGLE_GLYPH: &str = "\u{2b8b}";
/// The word [`OFFLINE_TOGGLE_GLYPH`] stands in for. "Offline" and not
/// "Download": what the user gets is the map working without a network, and
/// the transfer is the means.
const OFFLINE_TOGGLE_HINT: &str = "Download an area for offline use";
/// The inspector toggle. Selected-state styled while the inspector is open — the
/// mirror of [`LAYERS_TOGGLE_LABEL`] for the right-hand panel.
const INSPECTOR_TOGGLE_LABEL: &str = "\u{2699} Inspector";

/// The pane-count segments' caption in the roomy form.
const PANES_LABEL: &str = "Panes:";
/// The active-pane segments' caption, likewise.
const PANE_LABEL: &str = "Pane:";
/// The tight form's abbreviated captions — small text, no colon.
const PANES_TIGHT_LABEL: &str = "Panes";
const PANE_TIGHT_LABEL: &str = "Pane";

/// **The split-orientation segments, as text rather than pictograms.** The
/// obvious glyphs for this — ▤ U+25A4 and ▥ U+25A5 — are in neither family
/// egui bundles (checked against `Fonts::has_glyph`, which is what
/// `ui_glyphs`' coverage test walks), and a tofu box is worse than a word.
/// Roomy and tight spellings, the same pairing the captions above use.
const SPLIT_LABELS: [(crate::pane::SplitOrientation, &str, &str, &str); 3] = [
    (
        crate::pane::SplitOrientation::Auto,
        "Auto",
        "A",
        "Split the panes to suit the window's width",
    ),
    (
        crate::pane::SplitOrientation::Rows,
        "Rows",
        "R",
        "Stack the panes in rows, at every width",
    ),
    (
        crate::pane::SplitOrientation::Columns,
        "Cols",
        "C",
        "Sit the panes side by side, at every width",
    ),
];

/// Item spacing in the roomy form — and the unit [`roomy_run_width`] charges per
/// element, so the measure and the layout cannot drift apart.
const ROOMY_ITEM_SPACING: f32 = 8.0;
/// Item spacing when the bar tightens.
const TIGHT_ITEM_SPACING: f32 = 4.0;
/// Horizontal button padding when the bar tightens (egui's stock is 4).
const TIGHT_BUTTON_PADDING: f32 = 2.0;
/// What a `Separator` claims along a horizontal run: `Separator::default()`'s own
/// `spacing`.
const SEPARATOR_WIDTH: f32 = 6.0;

/// The bar's vertical inner margin, each side.
const VERTICAL_MARGIN: i8 = 7;
/// The interact height the bar's widgets lay out at — a comfortable button height
/// in place of egui's 18 pt default, for the same finding.
const INTERACT_HEIGHT: f32 = 26.0;
/// What the two constants above promise together: the bar can never be thinner than
/// the margins plus one interact row.
#[cfg(test)]
pub(crate) const MIN_BAR_HEIGHT: f32 = 2.0 * VERTICAL_MARGIN as f32 + INTERACT_HEIGHT;

impl super::Gui {
    /// Draw the top bar. Runs before anything else claims space, and before any
    /// `mem::take` window opens — which is what lets the menu model read the live
    /// active pane and the segments write state directly.
    pub(super) fn render_top_bar(&mut self, ui: &mut egui::Ui, actions: &mut Vec<GuiAction>) {
        let compact = self.layout.width == crate::ui_layout::WidthClass::Compact;
        let model = (!compact).then(|| self.menu_model());
        let mut menu_frame = ui_menu::MenuFrame::default();

        #[cfg(test)]
        let mut probe = super::TopBarProbe::default();

        let frame =
            egui::Frame::side_top_panel(&ui.ctx().global_style()).inner_margin(egui::Margin {
                left: 8,
                right: 8,
                top: VERTICAL_MARGIN,
                bottom: VERTICAL_MARGIN,
            });
        let panel = egui::Panel::top("top_bar").frame(frame).show(ui, |ui| {
            ui.spacing_mut().interact_size.y = INTERACT_HEIGHT;
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = ROOMY_ITEM_SPACING;

                if let Some(model) = &model {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let insp_open = self.insp_open;
                        let inspector = ui.selectable_label(insp_open, INSPECTOR_TOGGLE_LABEL);
                        #[cfg(test)]
                        {
                            probe.inspector_toggle = (inspector.rect, insp_open);
                        }
                        // A UiSweep target: the sweep opens the inspector here
                        // and closes it by its own × button.
                        crate::gesture_player::click_registry::register(
                            crate::gesture_player::ui_sweep::INSPECTOR_TOGGLE,
                            inspector.rect,
                        );
                        if inspector.clicked() {
                            self.insp_open = !insp_open;
                        }

                        let armed = self.section_draw_armed();
                        let section = ui.selectable_label(armed, SECTION_TOGGLE_LABEL);
                        #[cfg(test)]
                        {
                            probe.section_arm = (section.rect, armed);
                        }
                        if section.clicked() {
                            self.set_section_draw_armed(!armed);
                        }

                        let region_armed = self.region_pick_armed();
                        let region = ui.selectable_label(region_armed, REGION_TOGGLE_LABEL);
                        #[cfg(test)]
                        {
                            probe.region_arm = (region.rect, region_armed);
                        }
                        if region.clicked() {
                            self.set_region_pick_armed(!region_armed);
                        }

                        let offline_armed = self.download_pick_armed();
                        let offline = ui
                            .selectable_label(offline_armed, OFFLINE_TOGGLE_GLYPH)
                            .on_hover_text(OFFLINE_TOGGLE_HINT);
                        #[cfg(test)]
                        {
                            probe.offline_arm = (offline.rect, offline_armed);
                        }
                        if offline.clicked() {
                            self.set_download_pick_armed(!offline_armed);
                        }

                        ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                            self.render_top_bar_run(
                                ui,
                                model,
                                &mut menu_frame,
                                #[cfg(test)]
                                &mut probe,
                            );
                        });
                    });
                } else {
                    self.render_phone_top_bar_run(
                        ui,
                        #[cfg(test)]
                        &mut probe,
                    );
                }
            });
        });

        self.clear_fade_on_top_bar_press(ui.ctx(), panel.response.rect);

        #[cfg(test)]
        {
            probe.rect = panel.response.rect;
            self.probes.last_top_bar = probe;
            self.probes
                .last_menu_leaves
                .extend(menu_frame.drawn.iter().copied());
        }

        for event in menu_frame.events {
            self.apply_menu_event(event, actions);
        }
    }

    /// The phone bar's run (plan §1.2): wordmark · ⏴ collapse · live scan summary
    /// chip · (spacer) · icon-only ∕ and ⛶ arms.
    fn render_phone_top_bar_run(
        &mut self,
        ui: &mut egui::Ui,
        #[cfg(test)] probe: &mut super::TopBarProbe,
    ) {
        self.menu_popup_open = false;
        #[cfg(test)]
        {
            probe.pane_count_max = self.layout.width.max_panes();
        }

        let collapsed = self.statusbar_collapsed;
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if !collapsed {
                let armed = self.section_draw_armed();
                let section = ui
                    .selectable_label(armed, "\u{2215}")
                    .on_hover_text("Draw cross-section");
                #[cfg(test)]
                {
                    probe.section_arm = (section.rect, armed);
                }
                if section.clicked() {
                    self.set_section_draw_armed(!armed);
                    if !armed && self.top_sheet_page().is_some() {
                        self.clear_sheet_pages();
                    }
                }

                let region_armed = self.region_pick_armed();
                let region = ui
                    .selectable_label(region_armed, "\u{26f6}")
                    .on_hover_text("Pick 3D region");
                #[cfg(test)]
                {
                    probe.region_arm = (region.rect, region_armed);
                }
                if region.clicked() {
                    self.set_region_pick_armed(!region_armed);
                    if !region_armed && self.top_sheet_page().is_some() {
                        self.clear_sheet_pages();
                    }
                }

                let offline_armed = self.download_pick_armed();
                let offline = ui
                    .selectable_label(offline_armed, OFFLINE_TOGGLE_GLYPH)
                    .on_hover_text(OFFLINE_TOGGLE_HINT);
                #[cfg(test)]
                {
                    probe.offline_arm = (offline.rect, offline_armed);
                }
                if offline.clicked() {
                    self.set_download_pick_armed(!offline_armed);
                    if !offline_armed && self.top_sheet_page().is_some() {
                        self.clear_sheet_pages();
                    }
                }
            }

            ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                render_wordmark(ui);

                let collapse = ui
                    .button(if collapsed {
                        super::statusbar::RESTORE_LABEL
                    } else {
                        super::statusbar::COLLAPSE_LABEL
                    })
                    .on_hover_text(if collapsed {
                        "Restore the top bar"
                    } else {
                        "Collapse the top bar"
                    });
                #[cfg(test)]
                {
                    probe.collapse = collapse.rect;
                }
                if collapse.clicked() {
                    self.statusbar_collapsed = !collapsed;
                }
                if collapsed {
                    return;
                }

                let scan_text = self.phone_scan_summary();
                ui.add(egui::Label::new(scan_text.as_str()).truncate());
                #[cfg(test)]
                {
                    probe.scan_text = scan_text;
                }
                #[cfg(not(test))]
                let _ = scan_text;
            });
        });
    }

    /// The scan chip's text: site, time and a ⏺ live / ⏮ archive posture glyph —
    /// the short form the compact status bar carried before the phone shell, with
    /// the posture glyph in place of the room it does not have.
    fn phone_scan_summary(&self) -> String {
        let pane = self.active_pane();
        let posture = if pane.viewing_live {
            "\u{23fa}"
        } else {
            "\u{23ee}"
        };
        match &pane.scan_info {
            Some(info) => format!(
                "{} - {} {posture}",
                info.site.name,
                self.preferences
                    .timezone
                    .format_naive_utc(info.timestamp, "%H:%M"),
            ),
            None => "No scan loaded".to_owned(),
        }
    }

    /// The left-hand run: wordmark, ☰ dropdown, Layers toggle and the pane
    /// segments, inside the unconditional scroll wrapper.
    fn render_top_bar_run(
        &mut self,
        ui: &mut egui::Ui,
        model: &[ui_menu::MenuNode],
        menu_frame: &mut ui_menu::MenuFrame,
        #[cfg(test)] probe: &mut super::TopBarProbe,
    ) {
        let roomy = ui.available_width() >= roomy_run_width(ui, self.pane_layout.pane_count);

        egui::ScrollArea::horizontal()
            .scroll_source(super::shell::panel_scroll_source())
            .id_salt("top_bar_scroll")
            .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    if !roomy {
                        ui.spacing_mut().item_spacing.x = TIGHT_ITEM_SPACING;
                        ui.spacing_mut().button_padding.x = TIGHT_BUTTON_PADDING;
                    }

                    render_wordmark(ui);

                    let menu_button = ui.button(MENU_BUTTON_LABEL);
                    #[cfg(test)]
                    {
                        probe.menu_button = menu_button.rect;
                    }
                    if std::mem::take(&mut self.menu_popup_close_requested) {
                        egui::Popup::close_id(
                            ui.ctx(),
                            egui::Popup::default_response_id(&menu_button),
                        );
                    }
                    egui::Popup::menu(&menu_button)
                        .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
                        .show(|ui| {
                            *menu_frame = ui_menu::render_menu_popup(ui, model);
                            let armed = menu_frame.events.iter().any(|event| {
                                matches!(
                                    event,
                                    ui_menu::MenuEvent::Toggled(
                                        ui_menu::MenuToggle::DrawCrossSection
                                            | ui_menu::MenuToggle::PickRegion,
                                        true,
                                    )
                                )
                            });
                            if armed {
                                ui.close_kind(egui::UiKind::Menu);
                            }
                        });
                    self.menu_popup_open = egui::Popup::is_id_open(
                        ui.ctx(),
                        egui::Popup::default_response_id(&menu_button),
                    );

                    ui.separator();

                    let layers_open = self.layers_panel_visible();
                    let layers = ui.selectable_label(layers_open, LAYERS_TOGGLE_LABEL);
                    #[cfg(test)]
                    {
                        probe.layers_toggle = (layers.rect, layers_open);
                        self.probes
                            .widget_id_probes
                            .push(("layers_toggle", layers.id));
                    }
                    // A UiSweep target: the sweep closes and reopens the panel.
                    crate::gesture_player::click_registry::register(
                        crate::gesture_player::ui_sweep::LAYERS_TOGGLE,
                        layers.rect,
                    );
                    if layers.clicked() {
                        if self.layout.width.has_persistent_sidebar() {
                            self.stack_open = Some(!layers_open);
                        } else {
                            self.drawer_open = !layers_open;
                        }
                    }

                    ui.separator();
                    #[cfg(test)]
                    {
                        probe.pane_count_max = self.layout.width.max_panes();
                    }
                    self.render_pane_segments(ui, roomy);
                });
            });
    }

    /// The pane-count and active-pane segments.
    pub(super) fn render_pane_segments(&mut self, ui: &mut egui::Ui, roomy: bool) {
        let offered = self.layout.width.max_panes();

        if roomy {
            ui.label(PANES_LABEL);
        } else {
            ui.label(egui::RichText::new(PANES_TIGHT_LABEL).small().weak());
        }
        for count in 1..=crate::ui_layout::WidthClass::max_panes_absolute() {
            let selected = self.pane_layout.pane_count == count;
            let enabled = count <= offered;
            let button = ui.add_enabled(
                enabled,
                egui::Button::selectable(selected, format!("{count}")),
            );
            #[cfg(test)]
            self.probes.last_pane_options.push(super::PaneOptionProbe {
                count,
                selected,
                enabled,
                rect: button.rect,
            });
            if button.clicked() && !selected {
                let _ = self.set_pane_count(count);
            }
        }

        // **Only with a split to orient.** One pane has no divider, so the
        // three buttons would be an option the window cannot express.
        if self.pane_layout.pane_count > 1 {
            for (orientation, roomy_label, tight_label, hover) in SPLIT_LABELS {
                let selected = self.split_orientation == orientation;
                let label = if roomy { roomy_label } else { tight_label };
                let button = ui
                    .add(egui::Button::selectable(selected, label))
                    .on_hover_text(hover);
                #[cfg(test)]
                self.probes
                    .last_split_options
                    .push(super::SplitOptionProbe {
                        orientation,
                        selected,
                        rect: button.rect,
                    });
                if button.clicked() && !selected {
                    self.set_split_orientation(orientation);
                }
            }
        }

        if self.pane_layout.pane_count > 1 {
            if roomy {
                ui.label(PANE_LABEL);
            } else {
                ui.label(egui::RichText::new(PANE_TIGHT_LABEL).small().weak());
            }
            for i in 0..self.pane_layout.pane_count {
                let selected = self.active_pane == i;
                if ui
                    .selectable_label(selected, format!("{}", i + 1))
                    .clicked()
                    && !selected
                {
                    self.active_pane = i;
                }
            }
        }
    }
}

/// What the left-hand run's roomy form needs, measured from the real galleys at the
/// real style — no width constant to drift from the fonts, and nothing for a theme
/// change to silently invalidate.
/// One measured run width and the signature it was measured under.
#[derive(Clone)]
struct RoomyWidth {
    signature: u64,
    width: f32,
}

fn roomy_run_width(ui: &egui::Ui, pane_count: usize) -> f32 {
    let body = egui::TextStyle::Body.resolve(ui.style());
    let button_font = egui::TextStyle::Button.resolve(ui.style());
    // Signature-memoised on the house pattern (see `legend_ramp::ramp`),
    // because the run below shapes a galley per label -- the wordmark, two
    // buttons, every pane-count digit, and at more than one pane every split
    // label and pane index, fifteen to twenty of them -- to answer one
    // comparison against the available width. The answer moves only when the
    // two fonts, the button padding or the pane count move, all of which are
    // rare -- the SCALE is deliberately not in it, because a galley's
    // `size().x` is points and does not move with `pixels_per_point`, and it was being recomputed every frame: `ui:topbar`
    // measured 285 us mean on a 4757 us median frame (Mac 60 Hz, Firefox,
    // scene D, n=107). A font DEFINITION swapped underneath an unchanged
    // `FontId` is not in the signature and would be missed; nothing in this
    // app does that outside boot, and the alternative is hashing the font
    // atlas every frame to save fifteen galleys.
    let signature = {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        pane_count.hash(&mut h);
        body.size.to_bits().hash(&mut h);
        body.family.hash(&mut h);
        button_font.size.to_bits().hash(&mut h);
        button_font.family.hash(&mut h);
        ui.spacing().button_padding.x.to_bits().hash(&mut h);
        h.finish()
    };
    let slot = egui::Id::new("top_bar_roomy_run_width");
    if let Some(memo) = ui.ctx().data(|d| d.get_temp::<RoomyWidth>(slot))
        && memo.signature == signature
    {
        return memo.width;
    }
    let text = |font: &egui::FontId, s: &str| -> f32 {
        ui.painter()
            .layout_no_wrap(s.to_owned(), font.clone(), egui::Color32::PLACEHOLDER)
            .size()
            .x
    };
    let button_pad = 2.0 * ui.spacing().button_padding.x;

    let mut widths = vec![
        text(&body, "SQUALLAR"),
        text(&button_font, MENU_BUTTON_LABEL) + button_pad,
        SEPARATOR_WIDTH,
        text(&button_font, LAYERS_TOGGLE_LABEL) + button_pad,
        SEPARATOR_WIDTH,
        text(&body, PANES_LABEL),
    ];
    for count in 1..=crate::ui_layout::WidthClass::max_panes_absolute() {
        widths.push(text(&button_font, &format!("{count}")) + button_pad);
    }
    if pane_count > 1 {
        for (_, roomy_label, _, _) in SPLIT_LABELS {
            widths.push(text(&button_font, roomy_label) + button_pad);
        }
        widths.push(text(&body, PANE_LABEL));
        for i in 0..pane_count {
            widths.push(text(&button_font, &format!("{}", i + 1)) + button_pad);
        }
    }
    let width = widths.iter().sum::<f32>() + ROOMY_ITEM_SPACING * widths.len() as f32;
    ui.ctx()
        .data_mut(|d| d.insert_temp(slot, RoomyWidth { signature, width }));
    width
}

/// The wordmark: SQUALLAR with the accent on "AR".
fn render_wordmark(ui: &mut egui::Ui) {
    let mut job = egui::text::LayoutJob::default();
    let font_id = egui::TextStyle::Body.resolve(ui.style());
    job.append(
        "SQUALL",
        0.0,
        egui::TextFormat {
            font_id: font_id.clone(),
            color: ui.visuals().strong_text_color(),
            ..Default::default()
        },
    );
    job.append(
        "AR",
        0.0,
        egui::TextFormat {
            font_id,
            color: ui.visuals().hyperlink_color,
            ..Default::default()
        },
    );
    ui.label(job);
}

#[cfg(test)]
mod roomy_width_memo_tests {
    /// A `Ui` on a live context, the shape `ui_map_overlays`'s tests use.
    fn ui_on(ctx: &egui::Context) -> egui::Ui {
        egui::Ui::new(
            ctx.clone(),
            egui::Id::new("roomy_width_memo_test"),
            egui::UiBuilder::new()
                .layer_id(egui::LayerId::background())
                .max_rect(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(2000.0, 40.0),
                )),
        )
    }

    /// The memo must not outlive what it was measured under. Pane count is the
    /// input that moves in normal use -- every pane adds a numbered button to
    /// the run -- so a stale answer here would misjudge roomy against compact
    /// for the rest of the session. Two counts, two widths, and the second is
    /// the wider because it carries more buttons.
    #[test]
    fn the_memo_is_rebuilt_when_the_pane_count_moves() {
        let ctx = egui::Context::default();
        ctx.begin_pass(egui::RawInput::default());
        let ui = ui_on(&ctx);
        let one = super::roomy_run_width(&ui, 1);
        let four = super::roomy_run_width(&ui, 4);
        let one_again = super::roomy_run_width(&ui, 1);
        let _ = ctx.end_pass();
        assert!(
            four > one,
            "four panes carry more buttons than one, so the run is wider: \
             one={one} four={four}"
        );
        assert_eq!(
            one, one_again,
            "the same pane count must give the same width, memo or not"
        );
    }

    /// The other half: a signature input that is NOT the pane count. Text size
    /// is the one a user can move at runtime, and every measured galley moves
    /// with it, so a memo keyed only on pane count would answer for the old
    /// font for the rest of the session.
    ///
    /// The SCALE is deliberately absent from both the signature and this test:
    /// a galley's `size().x` is in points, so `pixels_per_point` does not move
    /// this width, and a signature input that cannot change the answer is an
    /// invalidation nobody can justify. Checked, not assumed -- an earlier
    /// version of this test asserted the opposite and went red.
    #[test]
    fn the_memo_is_rebuilt_when_the_text_size_moves() {
        let ctx = egui::Context::default();
        ctx.begin_pass(egui::RawInput::default());
        let ui = ui_on(&ctx);
        let small = super::roomy_run_width(&ui, 2);
        let _ = ctx.end_pass();

        ctx.all_styles_mut(|style| {
            for font in style.text_styles.values_mut() {
                font.size *= 2.0;
            }
        });
        ctx.begin_pass(egui::RawInput::default());
        let ui = ui_on(&ctx);
        let large = super::roomy_run_width(&ui, 2);
        let _ = ctx.end_pass();
        assert!(
            large > small,
            "doubled text is a wider run, and a memo that missed it would \
             answer for the old font: small={small} large={large}"
        );
    }
}
