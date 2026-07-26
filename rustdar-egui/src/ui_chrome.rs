//! The one UI chrome: menu bar, status bar, layers panel and hamburger.
//!
//! This replaces `ui_desktop.rs` and `ui_mobile.rs`, which were selected by
//! `cfg(target_os = "android")` and could therefore never both exist in one
//! binary — which is exactly what the wasm build needs, since a single wasm
//! artifact serves a phone browser and a desktop browser.
//!
//! # Panel order is load-bearing
//!
//! egui panels claim space in call order, and whatever is left becomes the
//! map's `CentralPanel`. That rect feeds pane hit-testing, `excluded_rects`
//! and overlay texture sizing, so the order below is not cosmetic:
//!
//! 1. menu bar (top)
//! 2. status bar (bottom)
//! 3. layers panel (left)
//! 4. hamburger — a floating `Area`, claims no space
//!
//! # Ids do not depend on the breakpoint
//!
//! Every panel, and the combo-box id prefix, uses one constant id regardless of
//! which presentation is on screen. egui keys widget memory — combo state,
//! scroll offsets, panel sizes — on those ids, so keying any of them on the
//! layout would silently reset the user's UI state every time the window
//! crossed a breakpoint. The two old files had exactly that hazard latent in
//! them: `"d_"`/`"m_"` control prefixes and `layers_panel`/`mobile_layers_panel`
//! could never collide only because the two files were never compiled together.

use crate::actions::GuiAction;
use crate::ui_layout::{PointerModality, WidthClass};
use super::ui_menu;
use rustdar_radar::types::ScanInfo;
use rustdar_units::UserPreferences;

use super::PaneState;

/// Width of the layers panel, in both its persistent and drawer forms.
///
/// One value, not two, because the panel keeps one egui id: `default_size`
/// only applies the first time an id is shown, so a second width would be
/// silently ignored anyway — and a *resizable* panel would remember the first.
const LAYERS_PANEL_WIDTH: f32 = 240.0;

/// Width of combo boxes inside the layers panel.
const COMBO_BOX_WIDTH: f32 = 150.0;

/// Id prefix for every widget in the layers panel.
///
/// Deliberately one constant and not a per-layout string: see the module note.
const LAYER_CONTROL_ID_PREFIX: &str = "layers_";

/// Size of the floating hamburger button.
const HAMBURGER_SIZE: f32 = 48.0;
/// Where the hamburger sits inside the content rect.
const HAMBURGER_INSET: egui::Vec2 = egui::vec2(12.0, 12.0);

/// What the chrome produced this frame.
pub(super) struct ChromeOutput {
    pub actions: Vec<GuiAction>,
    /// Screen rects of floating chrome drawn *over* the map, which map click
    /// handling must not treat as map clicks.
    ///
    /// This is an **output** of the chrome rather than something the map
    /// reconstructs. `ui_map.rs` used to rebuild the hamburger's rect from a
    /// copy of its position and size constants, so the two could disagree
    /// silently — moving the button would have left a dead zone at the old
    /// place and a live one under the new. Only the code that draws the button
    /// knows where it is.
    pub excluded_rects: Vec<egui::Rect>,
}

impl super::Gui {
    /// Draw all the chrome around the map, in the order the panels must claim
    /// their space.
    pub(super) fn render_chrome(&mut self, ui: &mut egui::Ui) -> ChromeOutput {
        let mut actions = Vec::new();
        let width = self.layout.width;

        if width.has_menu_bar() {
            self.render_menu_bar_panel(ui, &mut actions);
        }

        self.render_status_bar(ui, &mut actions);

        // The layers panel is either always there or reached through the
        // hamburger. `has_persistent_sidebar` and `has_hamburger` are exact
        // complements, so there is always exactly one way in.
        let show_panel = width.has_persistent_sidebar() || self.drawer_open;
        if show_panel {
            self.render_layers_panel(ui, &mut actions);
        }

        let mut excluded_rects = Vec::new();
        if width.has_hamburger() && !self.drawer_open {
            excluded_rects.push(self.render_hamburger(ui.ctx()));
        }

        ChromeOutput {
            actions,
            excluded_rects,
        }
    }

    fn render_menu_bar_panel(&mut self, ui: &mut egui::Ui, actions: &mut Vec<GuiAction>) {
        let model = self.menu_model();
        let mut frame = ui_menu::MenuFrame::default();
        egui::Panel::top("menubar_container").show(ui, |ui| {
            frame = ui_menu::render_menu_bar(ui, &model);
        });

        #[cfg(test)]
        self.last_menu_leaves.extend(frame.drawn.iter().copied());

        for event in frame.events {
            self.apply_menu_event(event, actions);
        }
    }

    /// The floating button that opens the layers drawer.
    ///
    /// Returns its rect, which becomes an excluded rect so a tap on the button
    /// is never also a tap on the map underneath.
    fn render_hamburger(&mut self, ctx: &egui::Context) -> egui::Rect {
        // Positioned inside the *content* rect, so it clears the notch and the
        // status bar without any manual inset arithmetic.
        let pos = self.layout.content_rect.min + HAMBURGER_INSET;

        // The rect returned is the one `allocate_exact_size` actually handed
        // out, not a second guess at it from the same constants. Recomputing it
        // is the hazard this whole return value exists to remove.
        let drawn = egui::Area::new(egui::Id::new("layers_hamburger"))
            .order(egui::Order::Middle)
            .fixed_pos(pos)
            .interactable(true)
            .show(ctx, |ui| {
                let (rect, response) =
                    ui.allocate_exact_size(egui::Vec2::splat(HAMBURGER_SIZE), egui::Sense::click());
                let bg_color = if ui.style().visuals.dark_mode {
                    egui::Color32::from_rgba_unmultiplied(40, 40, 40, 220)
                } else {
                    egui::Color32::from_rgba_unmultiplied(240, 240, 240, 230)
                };
                ui.painter().rect_filled(rect, 8.0, bg_color);
                ui.painter().text(
                    rect.center(),
                    egui::Align2::CENTER_CENTER,
                    "\u{2630}",
                    egui::FontId::proportional(26.0),
                    ui.style().visuals.text_color(),
                );
                (rect, response.clicked())
            })
            .inner;

        let (rect, clicked) = drawn;
        if clicked {
            self.drawer_open = true;
        }
        rect
    }

    /// The status bar along the bottom.
    ///
    /// `roomy` is about horizontal space: the long scan summary and the
    /// auto-poll checkbox do not fit side by side on a phone.
    ///
    /// The hover readout is a different question and keys on the *modality*.
    /// There is no hover without a pointing device, so a touchscreen has
    /// nothing to show however wide it is, and a narrow desktop window has a
    /// mouse and should keep it.
    fn render_status_bar(&mut self, ui: &mut egui::Ui, actions: &mut Vec<GuiAction>) {
        let roomy = self.layout.width != WidthClass::Compact;
        let has_hover = self.layout.modality == PointerModality::Mouse;

        #[cfg(test)]
        let mut probe = super::StatusBarProbe::default();

        egui::Panel::bottom("status_bar")
            .show_separator_line(true)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 8.0;

                    let refresh_button = ui.add_enabled(
                        !self.radar.fetching,
                        egui::Button::new("\u{1f504}").frame(false),
                    );
                    if refresh_button.clicked() {
                        actions.push(GuiAction::FetchRadarScan(self.radar.config.clone()));
                    }
                    refresh_button.on_hover_text("Refresh radar data");

                    ui.separator();

                    if roomy {
                        let drawn =
                            render_auto_poll_status(ui, self.radar.fetching, &mut self.auto_poll);
                        #[cfg(test)]
                        {
                            probe.auto_poll = drawn;
                        }
                        #[cfg(not(test))]
                        let _ = drawn;
                        ui.separator();
                    } else if self.radar.fetching {
                        ui.spinner();
                    }

                    let scan_text = render_scan_info(
                        ui,
                        self.panes
                            .get(self.active_pane)
                            .and_then(|p| p.scan_info.as_ref()),
                        &self.preferences,
                        roomy,
                    );
                    #[cfg(test)]
                    {
                        probe.scan_text = scan_text;
                    }
                    #[cfg(not(test))]
                    let _ = scan_text;

                    if has_hover {
                        ui.separator();
                        render_hover_info(ui, &self.panes);
                        #[cfg(test)]
                        {
                            probe.hover = true;
                        }
                    }

                    // Flexible space pushes the error to the right — but only
                    // when there is an error to push.
                    //
                    // Allocated unconditionally this scope is empty most of the
                    // time, and an empty child `Ui` is a zero-area widget rect
                    // pinned to the row's right edge: a rect that never moves,
                    // under an id that does. `Ui::new_child` folds the parent's
                    // auto-id counter into every child scope's registered id —
                    // `id_salt` stabilises only the state id, not that one — so
                    // the auto-poll block above (three widgets mid-fetch, one
                    // otherwise) re-keyed this slot on the frame a scan landed,
                    // which egui reports as `changed id between passes`.
                    if self.radar.error_message.is_some() {
                        ui.with_layout(
                            egui::Layout::right_to_left(egui::Align::Center),
                            |ui| {
                                render_error_display(ui, &mut self.radar.error_message);
                            },
                        );
                    }
                });
            });

        #[cfg(test)]
        {
            self.last_status_bar = probe;
        }
    }

    /// The layers panel, in whichever of its two forms this width calls for.
    ///
    /// The body is identical either way; only the header differs, because the
    /// drawer needs a way to close itself and the persistent sidebar does not.
    fn render_layers_panel(&mut self, ui: &mut egui::Ui, actions: &mut Vec<GuiAction>) {
        let is_drawer = !self.layout.width.has_persistent_sidebar();
        let show_menu_in_panel = !self.layout.width.has_menu_bar();

        // Built before the pane is taken, as `render_menu_bar_panel` does:
        // `menu_model` reads `self.active_pane()`, and inside the closure that
        // is `mem::take`'s default, whose `enabled_overlays` is empty. Every
        // toggle would render unchecked and emit `Toggled(kind, true)`.
        let menu_model = if show_menu_in_panel {
            Some(self.menu_model())
        } else {
            None
        };

        let mut pane = std::mem::take(&mut self.panes[self.active_pane]);
        let mut menu_frame = ui_menu::MenuFrame::default();

        egui::Panel::left("layers_panel")
            .default_size(LAYERS_PANEL_WIDTH)
            .resizable(false)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.heading("Layers");
                    if is_drawer {
                        ui.with_layout(
                            egui::Layout::right_to_left(egui::Align::Center),
                            |ui| {
                                if ui.button("\u{2715}").clicked() {
                                    self.drawer_open = false;
                                }
                            },
                        );
                    }
                });
                ui.separator();

                // An explicit salt rather than egui's positional auto-id.
                //
                // This is defensive, not a fix for a live bug: the two header
                // forms happen to allocate the same number of ids today (the
                // drawer's close button is nested inside the `horizontal`, so
                // it does not advance this Ui's counter), and the breakpoint
                // test confirms the auto-id would currently be stable too. The
                // salt makes that independent of *how many widgets precede it*,
                // which is what an unrelated edit to the header would otherwise
                // silently change — costing the user their scroll position on
                // every resize, with nothing to point at.
                let scroll = egui::ScrollArea::vertical()
                    .id_salt("layers_scroll")
                    .show(ui, |ui| {
                    self.render_pane_selector(ui, &mut pane);
                    self.render_layer_controls(
                        ui,
                        &mut pane,
                        COMBO_BOX_WIDTH,
                        LAYER_CONTROL_ID_PREFIX,
                        actions,
                    );

                    // With no menu bar on screen, the menu lives here — the
                    // same model, rendered as a list. This is what used to be
                    // `ui_mobile.rs`'s hand-rolled "Controls" block.
                    if let Some(model) = menu_model.as_ref() {
                        ui.add_space(10.0);
                        ui.separator();
                        menu_frame = ui_menu::render_menu_drawer(ui, model);
                    }
                });

                // Report the id egui really used, rather than reconstructing
                // it: the test that pins id stability across a breakpoint has
                // to be reading the same id the scroll state is stored under,
                // or it proves nothing about that state surviving.
                #[cfg(test)]
                self.widget_id_probes.push(("layers_scroll", scroll.id));
                #[cfg(not(test))]
                let _ = scroll;
            });

        self.panes[self.active_pane] = pane;
        self.propagate_layer_sync();

        #[cfg(test)]
        self.last_menu_leaves.extend(menu_frame.drawn.iter().copied());

        for event in menu_frame.events {
            self.apply_menu_event(event, actions);
        }
    }
}

/// Returns the checkbox's rect when one was drawn — while a fetch is running
/// there is a spinner instead.
fn render_auto_poll_status(
    ui: &mut egui::Ui,
    fetching: bool,
    auto_poll: &mut super::AutoPollState,
) -> Option<egui::Rect> {
    if fetching {
        ui.label("\u{1f504}");
        ui.label("Downloading");
        ui.spinner();
        return None;
    }
    let label = match auto_poll.time_until_next() {
        Some(remaining) if auto_poll.enabled => format!("Auto-poll (next in {}s)", remaining),
        _ => "Auto-poll".to_owned(),
    };
    Some(ui.checkbox(&mut auto_poll.enabled, label).rect)
}

/// The scan summary. `roomy` picks the long form; a compact bar has room for
/// the site and the time and nothing else. Returns the text it drew.
fn render_scan_info(
    ui: &mut egui::Ui,
    scan_info: Option<&ScanInfo>,
    prefs: &UserPreferences,
    roomy: bool,
) -> String {
    let text = match scan_info {
        Some(scan_info) if roomy => format!(
            "Scan: {} @ {} ({} products)",
            scan_info.site.name,
            prefs
                .timezone
                .format_naive_utc(scan_info.timestamp, "%Y-%m-%d %H:%M:%S"),
            scan_info.available_products.len()
        ),
        Some(scan_info) => format!(
            "{} @ {}",
            scan_info.site.name,
            prefs
                .timezone
                .format_naive_utc(scan_info.timestamp, "%H:%M")
        ),
        None => "No scan loaded".to_owned(),
    };
    ui.label(&text);
    text
}

fn render_hover_info(ui: &mut egui::Ui, panes: &[PaneState]) {
    let hover_info = panes.iter().find_map(|p| p.hover_value.as_ref());
    let overlay_hover = panes.iter().find_map(|p| p.overlay_hover_value.as_ref());
    if hover_info.is_some() || overlay_hover.is_some() {
        ui.label("\u{1f4cd}");
        if let Some(info) = hover_info {
            ui.label(info);
        }
        if let Some(info) = overlay_hover {
            ui.label(info);
        }
    } else {
        ui.label("");
    }
}

fn render_error_display(ui: &mut egui::Ui, error_message: &mut Option<String>) {
    let mut dismiss = false;
    if let Some(msg) = error_message.as_deref() {
        if ui.button("\u{2715}").clicked() {
            dismiss = true;
        }
        ui.label(msg);
        ui.label("\u{274c}");
    }
    if dismiss {
        *error_message = None;
    }
}
