//! The docked top bar: the one piece of chrome that claims space at the top,
//! at every width.
//!
//! The Synthesis design's full-bleed rule is that chrome floats over the radar
//! inside its bounds, with exactly one exception — this bar, which docks and
//! pushes the panes down. It is the same bar at every
//! [`WidthClass`](crate::ui_layout::WidthClass): being unconditional is what
//! lets crossing a breakpoint re-key nothing (see the id note in
//! `ui_shell.rs`), and it is where the routes that used to depend on the
//! width now live — the ☰ dropdown is the whole menu everywhere, and the
//! Layers toggle is the one way the layers panel opens and closes.
//!
//! Left to right: wordmark · ☰ app menu · Layers toggle · pane-count and
//! active-pane segments · (spacer) · the two armed-drag toggles · the ⚙
//! Inspector toggle. No Set Time button — that belongs to the timeline's
//! timestamp.
//!
//! # The bar never overlaps itself
//!
//! Two mechanisms, layered. The armed-drag toggles are laid out **first**, in
//! a right-to-left run, so they own the right edge before anything else asks
//! for room — however long the left-hand run grows, nothing can land under
//! them. The left-hand run then lives inside an unconditionally-present
//! horizontal `ScrollArea` (one id at every width, per the module note in
//! `ui_shell.rs`), which clips rather than collides when even its tight form
//! cannot fit — the graceful floor for widths below Compact's breakpoint,
//! until M6's minimal phone bar arrives.
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
const LAYERS_TOGGLE_LABEL: &str = "\u{25a4} Layers";
/// The 3D-region arm toggle. The label names the subject; the menu entry of
/// the same mode ([`ui_menu::REGION_ARM_LABEL`]) carries the longer teaching
/// phrase — a bar has room for a word, a menu for a sentence.
const REGION_TOGGLE_LABEL: &str = "\u{2b1a} Region";
/// The cross-section arm toggle, on the same terms as the region one.
const SECTION_TOGGLE_LABEL: &str = "\u{2571} X-sec";
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

impl super::Gui {
    /// Draw the top bar. Runs before anything else claims space, and before
    /// any `mem::take` window opens — which is what lets the menu model read
    /// the live active pane and the segments write state directly.
    pub(super) fn render_top_bar(&mut self, ui: &mut egui::Ui, actions: &mut Vec<GuiAction>) {
        let model = self.menu_model();
        let mut menu_frame = ui_menu::MenuFrame::default();

        #[cfg(test)]
        let mut probe = super::TopBarProbe::default();

        let panel = egui::Panel::top("top_bar").show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = ROOMY_ITEM_SPACING;

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
                        self.render_top_bar_run(ui, &model, &mut menu_frame, #[cfg(test)] &mut probe);
                    });
                });
            });
        });

        #[cfg(test)]
        {
            probe.rect = panel.response.rect;
            self.last_top_bar = probe;
            self.last_menu_leaves.extend(menu_frame.drawn.iter().copied());
        }
        #[cfg(not(test))]
        let _ = panel;

        for event in menu_frame.events {
            self.apply_menu_event(event, actions);
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
    fn render_pane_segments(&mut self, ui: &mut egui::Ui, roomy: bool) {
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
