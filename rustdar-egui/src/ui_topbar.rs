//! The docked top bar: the one piece of chrome that claims space at the top,
//! at every width.
//!
//! The Synthesis design's full-bleed rule is that chrome floats over the radar
//! inside its bounds, with exactly one exception — this bar, which docks and
//! pushes the panes down. It is one `Panel` under one id at every
//! [`WidthClass`](crate::ui_layout::WidthClass): being unconditional is what
//! lets crossing a breakpoint re-key nothing (see the id note in
//! `ui_shell.rs`). What fills it splits once, at the Compact breakpoint.
//!
//! The wide form, left to right: wordmark · ☰ app menu · Layers toggle ·
//! pane-count and active-pane segments · (spacer) · the two armed-drag
//! toggles · the ⚙ Inspector toggle. No Set Time button — that belongs to
//! the timeline's timestamp. The ☰ dropdown is the whole menu on both wide
//! widths, and the Layers toggle is the one way the layers panel opens and
//! closes there.
//!
//! The phone form (plan §1.2) is minimal: wordmark · ⏴ collapse · scan chip ·
//! (spacer) · icon-only ⛶ and ∕ arms. The menu, Layers, Pane and App routes
//! live in the bottom bar (`ui_sheet.rs`), and the pane segments in the
//! sheet's Layers page header — this bar keeps only what has nowhere else
//! honest to be: the identity, the scan at a glance, and the two modes whose
//! whole point is being visible while armed.
//!
//! # The bar never overlaps itself
//!
//! Two mechanisms, layered. The armed-drag toggles are laid out **first**, in
//! a right-to-left run, so they own the right edge before anything else asks
//! for room — however long the left-hand run grows, nothing can land under
//! them. The left-hand run then lives inside an unconditionally-present
//! horizontal `ScrollArea` (one id on both wide widths, per the module note
//! in `ui_shell.rs`), which clips rather than collides when even its tight
//! form cannot fit — the graceful floor at Medium's 600 pt edge; below that
//! the phone form has nothing left to overlap.
//!
//! Above that floor the run adapts instead of scrolling: when the space the
//! toggles left is less than what the roomy form measures, the segment labels
//! go and the paddings tighten. The decision keys on `ui.available_width()`
//! against the real galleys at the real style — never on the `WidthClass`,
//! which would re-introduce a breakpoint into a bar whose whole point is not
//! to have one. Everything the choice adds or removes is a plain label or a
//! padding: stateless, so no widget memory rides on which form drew.

use super::ui_menu;
use crate::actions::GuiAction;

/// The app-menu button's glyph — the whole menu lives behind it.
const MENU_BUTTON_LABEL: &str = "\u{2630}";
/// The layers-panel toggle. Selected-state styled while the panel is open.
/// `▣`, not the demo's `▤`: every icon char here is drawn from the inventory
/// `ui_glyphs.rs` verifies against egui's bundled fonts, and `▤` has no glyph
/// in them — it shipped as a tofu box.
const LAYERS_TOGGLE_LABEL: &str = "\u{25a3} Layers";
/// The 3D-region arm toggle. The label names the subject; the menu entry of
/// the same mode ([`ui_menu::REGION_ARM_LABEL`]) carries the longer teaching
/// phrase — a bar has room for a word, a menu for a sentence. `⛶` (a carried
/// selection-corners glyph) rather than the demo's uncarried `⬚`, and the
/// same glyph heads the inspector's 3D block: the drag this arms is how that
/// view is aimed.
const REGION_TOGGLE_LABEL: &str = "\u{26f6} Region";
/// The cross-section arm toggle, on the same terms as the region one. `∕`
/// (division slash — a carried diagonal) rather than the uncarried `╱`.
const SECTION_TOGGLE_LABEL: &str = "\u{2215} X-sec";
/// The inspector toggle. Selected-state styled while the inspector is open —
/// the mirror of [`LAYERS_TOGGLE_LABEL`] for the right-hand panel.
const INSPECTOR_TOGGLE_LABEL: &str = "\u{2699} Inspector";

/// The pane-count segments' caption, drawn only in the roomy form.
const PANES_LABEL: &str = "Panes:";
/// The active-pane segments' caption, likewise roomy-only.
const PANE_LABEL: &str = "Pane:";

/// Item spacing in the roomy form — and the unit [`roomy_run_width`] charges
/// per element, so the measure and the layout cannot drift apart.
const ROOMY_ITEM_SPACING: f32 = 8.0;
/// Item spacing when the bar tightens.
const TIGHT_ITEM_SPACING: f32 = 4.0;
/// Horizontal button padding when the bar tightens (egui's stock is 4).
const TIGHT_BUTTON_PADDING: f32 = 2.0;
/// What a `Separator` claims along a horizontal run: `Separator::default()`'s
/// own `spacing`.
const SEPARATOR_WIDTH: f32 = 6.0;

/// The bar's vertical inner margin, each side. The stock panel frame's 2 pt
/// left the bar a cramped strip (the first-run finding); this is layout, not
/// theme — the frame keeps the stock panel fill and stroke.
const VERTICAL_MARGIN: i8 = 7;
/// The interact height the bar's widgets lay out at — a comfortable button
/// height in place of egui's 18 pt default, for the same finding. Width is
/// egui's own default; only the height changes.
const INTERACT_HEIGHT: f32 = 26.0;
/// What the two constants above promise together: the bar can never be
/// thinner than the margins plus one interact row. The height pin asserts
/// this floor at every width, so the breathing room cannot regress.
#[cfg(test)]
pub(crate) const MIN_BAR_HEIGHT: f32 = 2.0 * VERTICAL_MARGIN as f32 + INTERACT_HEIGHT;

impl super::Gui {
    /// Draw the top bar. Runs before anything else claims space, and before
    /// any `mem::take` window opens — which is what lets the menu model read
    /// the live active pane and the segments write state directly.
    ///
    /// One `Panel` under one id at every width; what fills it splits at the
    /// Compact breakpoint. The wider widths get the full run below; Compact
    /// gets the minimal phone bar ([`Self::render_phone_top_bar_run`]) —
    /// wordmark, scan chip, the two arm icons — because everything else the
    /// bar carries lives in the bottom bar down there (plan §1.2).
    pub(super) fn render_top_bar(&mut self, ui: &mut egui::Ui, actions: &mut Vec<GuiAction>) {
        let compact = self.layout.width == crate::ui_layout::WidthClass::Compact;
        let model = (!compact).then(|| self.menu_model());
        let mut menu_frame = ui_menu::MenuFrame::default();

        #[cfg(test)]
        let mut probe = super::TopBarProbe::default();

        // The stock panel frame with a real vertical margin, and a taller
        // interact height for everything in the bar — the first-run fix for a
        // bar that read as a cramped strip. Layout only: fill, stroke and
        // fonts are the stock theme's.
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
                    // Right-to-left, and **before** everything else, so the
                    // right-hand cluster claims its edge first: however long the
                    // left-hand run grows, it cannot lay a segment under it. The
                    // first widget added is the rightmost — Inspector at the
                    // edge, then X-sec, then Region, which reads left-to-right as
                    // Region · X-sec · Inspector.
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let insp_open = self.insp_open;
                        let inspector = ui.selectable_label(insp_open, INSPECTOR_TOGGLE_LABEL);
                        #[cfg(test)]
                        {
                            probe.inspector_toggle = (inspector.rect, insp_open);
                        }
                        if inspector.clicked() {
                            // A plain flip: whatever the inspector was last about
                            // is what it reopens on — the ⟩ collapse keeps the
                            // selection for the same reason.
                            self.insp_open = !insp_open;
                        }

                        let armed = self.section_draw_armed();
                        let section = ui.selectable_label(armed, SECTION_TOGGLE_LABEL);
                        #[cfg(test)]
                        {
                            probe.section_arm = (section.rect, armed);
                        }
                        if section.clicked() {
                            // Through the setters both ways: arming un-arms the
                            // other drag, disarming drops a half-made gesture.
                            self.set_section_draw_armed(!armed);
                        }

                        let armed = self.region_arm;
                        let region = ui.selectable_label(armed, REGION_TOGGLE_LABEL);
                        #[cfg(test)]
                        {
                            probe.region_arm = (region.rect, armed);
                        }
                        if region.clicked() {
                            self.set_region_arm(!armed);
                        }

                        // Everything else takes what the toggles left, reading
                        // left-to-right again.
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

        // The unfade-before-acting choke point (§3.6, `ui_fade.rs`): a press
        // on the bar while faded clears the fade before the shell draws the
        // floating chrome, so whatever the press performs — this frame's
        // handlers above have already run it — lands in a visible UI. After
        // the panel so the rect is this frame's true one; before the menu
        // dispatch below, which is the last of the bar's acting.
        self.clear_fade_on_top_bar_press(ui.ctx(), panel.response.rect);

        #[cfg(test)]
        {
            probe.rect = panel.response.rect;
            self.last_top_bar = probe;
            self.last_menu_leaves
                .extend(menu_frame.drawn.iter().copied());
        }

        for event in menu_frame.events {
            self.apply_menu_event(event, actions);
        }
    }

    /// The phone bar's run (plan §1.2): wordmark · ⏴ collapse · live scan
    /// summary chip · (spacer) · icon-only ∕ and ⛶ arms. No pane segments,
    /// no Layers or Inspector toggles, no ☰ — the bottom bar owns those
    /// routes down here, and the sheet's Layers page carries the segments.
    ///
    /// The collapse shares [`Gui::statusbar_collapsed`](super::Gui) with the status
    /// bar the wider widths draw: the phone has no separate status bar, so
    /// the one collapse state applies to the bar that carries the scan text
    /// (§1.6, contract 75). Collapsed, only the wordmark and the restore
    /// button remain.
    ///
    /// The hover readout hosts here when a mouse is driving — contract 25's
    /// rule: the readout follows the modality, never the width, and this bar
    /// is the only chrome the phone shell keeps at the top to host it.
    fn render_phone_top_bar_run(
        &mut self,
        ui: &mut egui::Ui,
        #[cfg(test)] probe: &mut super::TopBarProbe,
    ) {
        // The ☰ dropdown is desktop and tablet chrome: no button anchors it
        // here, so its open-state mirror must not stay latched across a
        // resize — `dismiss_top_layer` would consume a press against a popup
        // that is not on screen.
        self.menu_popup_open = false;
        #[cfg(test)]
        {
            probe.pane_count_max = self.layout.width.max_panes();
        }

        let collapsed = self.statusbar_collapsed;
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if !collapsed {
                // First added is rightmost: ∕ at the edge, then ⛶, reading
                // left-to-right as ⛶ · ∕ — the wide bar's own order.
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
                    // Arming needs the map: an open sheet page is in the
                    // drag's way, so it closes — the Menu page's own rule
                    // for its two arm entries (`render_sheet_menu`), applied
                    // to the bar's route. Disarming keeps whatever is up,
                    // as the dropdown does.
                    if !armed && self.top_sheet_page().is_some() {
                        self.clear_sheet_pages();
                    }
                }

                let armed = self.region_arm;
                let region = ui
                    .selectable_label(armed, "\u{26f6}")
                    .on_hover_text("Pick 3D region");
                #[cfg(test)]
                {
                    probe.region_arm = (region.rect, armed);
                }
                if region.clicked() {
                    self.set_region_arm(!armed);
                    // Same terms as the ╱ above.
                    if !armed && self.top_sheet_page().is_some() {
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

                if self.layout.modality == crate::ui_layout::PointerModality::Mouse {
                    ui.separator();
                    // The readout is the one unbounded string on this bar,
                    // and a `Label` in a horizontal run extends rather than
                    // wraps — across the ⬚/╱ toggles laid out before it.
                    // The module note's overlap rule applies here as it
                    // does to the wide run, in its truncation form: cap
                    // the readout at the width the arm toggles left, so
                    // however long the value grows the arms stay
                    // unobscured and clickable.
                    ui.scope(|ui| {
                        ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Truncate);
                        super::statusbar::render_hover_info(ui, self.panes());
                    });
                    #[cfg(test)]
                    {
                        probe.hover = true;
                    }
                }
            });
        });
    }

    /// The scan chip's text: site, time and a ⏺ live / ⏮ archive posture
    /// glyph — the short form the compact status bar carried before the
    /// phone shell, with the posture glyph in place of the room it does not
    /// have. `⏺` is the timeline's own live glyph; `⏮` is its
    /// previous-frame glyph, borrowed because it reads as "past" where a
    /// bare `⏸` read as "app paused" (the M8.1 finding — pause keeps its
    /// transport and auto-poll uses, where the context carries it). The
    /// demo's `⚡`/`⏪` have no glyph in egui's bundled fonts. The time is
    /// the user's own timezone preference, exactly as the status bar prints
    /// it — no hardcoded `Z` suffix claiming UTC at a setting that may not
    /// be.
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
        // The adaptation decision, from the space the toggles actually left
        // against what the roomy form actually measures — see the module
        // note for why it must not key on the `WidthClass`.
        let roomy = ui.available_width() >= roomy_run_width(ui, self.pane_layout.pane_count);

        // Unconditional — same id path at every width, like every other
        // piece of this bar — and bar-less: it exists to *clip* an overrun
        // into something scrollable rather than to advertise scrolling.
        egui::ScrollArea::horizontal()
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
                    // A dismiss was consumed against the open dropdown since
                    // the last frame — Android's back, or an Escape the
                    // frontend resolved beside egui's own handling. Closing
                    // through the popup's memory before `show` is what makes
                    // the two routes one: an Escape egui also saw closes it
                    // twice over, idempotently, and a back press with no key
                    // event in egui's queue still closes it here.
                    if std::mem::take(&mut self.menu_popup_close_requested) {
                        egui::Popup::close_id(
                            ui.ctx(),
                            egui::Popup::default_response_id(&menu_button),
                        );
                    }
                    egui::Popup::menu(&menu_button)
                        // Not the menu default (close on any click): the toggles
                        // are the bulk of the entries, and a menu that shut on
                        // every tick would make flipping two of them three opens.
                        .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
                        .show(|ui| {
                            *menu_frame = ui_menu::render_menu_popup(ui, model);
                            // Arming a modal drag closes the dropdown, as the
                            // dispatcher closes the layers drawer: the next thing
                            // the user does is a drag on the map, and an open menu
                            // is in its way. Commands close themselves — the
                            // renderer runs with `in_menu` set — and disarming
                            // deliberately keeps the menu where the user is.
                            let armed = menu_frame.events.iter().any(|event| {
                                matches!(
                                    event,
                                    ui_menu::MenuEvent::Toggled(
                                        ui_menu::MenuToggle::RegionArm
                                            | ui_menu::MenuToggle::DrawCrossSection,
                                        true,
                                    )
                                )
                            });
                            if armed {
                                ui.close_kind(egui::UiKind::Menu);
                            }
                        });
                    // What `dismiss_top_layer` reads next press: whether the
                    // dropdown is open *now*, after the click that may have
                    // toggled it and the Escape that may have closed it.
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
                        // The toggle's real egui id, so the keyboard tests can
                        // focus the widget egui keyed rather than a guess.
                        self.widget_id_probes.push(("layers_toggle", layers.id));
                    }
                    if layers.clicked() {
                        // On Expanded the toggle writes the explicit choice over
                        // the shell default; elsewhere the panel *is* the drawer
                        // and the toggle is its opener. See `layers_panel_visible`.
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
    ///
    /// Every count up to the absolute maximum is drawn and the ones past this
    /// width's offer are disabled rather than absent — the picker narrows on a
    /// phone (see [`WidthClass::max_panes`](crate::ui_layout::WidthClass::max_panes)),
    /// and a disabled button says so where a missing one would just be a
    /// shorter row. The config clamp deliberately does not narrow with it; see
    /// [`WidthClass::max_panes_absolute`](crate::ui_layout::WidthClass::max_panes_absolute).
    ///
    /// `roomy` gates only the two captions, which is what keeps the tight form
    /// id-neutral: a label allocates no widget memory.
    ///
    /// `pub(super)` because the phone sheet's Layers page hosts the same
    /// segments (plan §1.3): the phone top bar has none, and copying the
    /// renderer rather than the rules is how the hidden-while-one-pane rule
    /// stays one rule.
    pub(super) fn render_pane_segments(&mut self, ui: &mut egui::Ui, roomy: bool) {
        let offered = self.layout.width.max_panes();

        if roomy {
            ui.label(PANES_LABEL);
        }
        for count in 1..=crate::ui_layout::WidthClass::max_panes_absolute() {
            let selected = self.pane_layout.pane_count == count;
            let enabled = count <= offered;
            let button = ui.add_enabled(
                enabled,
                egui::Button::selectable(selected, format!("{count}")),
            );
            // The button that was drawn: which count, whether it read as
            // selected and enabled, and where it landed so a test can click
            // it. A probe built from `offered` instead would be a restatement
            // of the line above and could not see the loop at all.
            #[cfg(test)]
            self.last_pane_options.push(super::PaneOptionProbe {
                count,
                selected,
                enabled,
                rect: button.rect,
            });
            if button.clicked() && !selected {
                // The answer is ignored here and only here: a disabled button
                // never clicks, so every reachable count is within what
                // `PaneLayout::for_count` allows and the clamp can never bite.
                let _ = self.set_pane_count(count);
            }
        }

        // Hidden while there is one pane — a pre-M1 rule this bar inherited,
        // and one M6's phone sheet header must copy deliberately (plan §1.2).
        if self.pane_layout.pane_count > 1 {
            if roomy {
                ui.label(PANE_LABEL);
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

/// What the left-hand run's roomy form needs, measured from the real galleys
/// at the real style — no width constant to drift from the fonts, and nothing
/// for a theme change to silently invalidate.
///
/// Charged one [`ROOMY_ITEM_SPACING`] per element, which over-counts by one
/// gap: deliberate slack, on the side that flips to the tight form a few
/// points early rather than the side that overlaps.
fn roomy_run_width(ui: &egui::Ui, pane_count: usize) -> f32 {
    let body = egui::TextStyle::Body.resolve(ui.style());
    let button_font = egui::TextStyle::Button.resolve(ui.style());
    let text = |font: &egui::FontId, s: &str| -> f32 {
        ui.painter()
            .layout_no_wrap(s.to_owned(), font.clone(), egui::Color32::PLACEHOLDER)
            .size()
            .x
    };
    let button_pad = 2.0 * ui.spacing().button_padding.x;

    let mut widths = vec![
        text(&body, "RUSTDAR"),
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
        widths.push(text(&body, PANE_LABEL));
        for i in 0..pane_count {
            widths.push(text(&button_font, &format!("{}", i + 1)) + button_pad);
        }
    }
    widths.iter().sum::<f32>() + ROOMY_ITEM_SPACING * widths.len() as f32
}

/// The wordmark: RUSTDAR with the accent on "DAR".
///
/// Stock theme only — the accent is the theme's own hyperlink colour, not a
/// new palette entry, so light and dark mode both keep their contract.
fn render_wordmark(ui: &mut egui::Ui) {
    let mut job = egui::text::LayoutJob::default();
    let font_id = egui::TextStyle::Body.resolve(ui.style());
    job.append(
        "RUST",
        0.0,
        egui::TextFormat {
            font_id: font_id.clone(),
            color: ui.visuals().strong_text_color(),
            ..Default::default()
        },
    );
    job.append(
        "DAR",
        0.0,
        egui::TextFormat {
            font_id,
            color: ui.visuals().hyperlink_color,
            ..Default::default()
        },
    );
    ui.label(job);
}
