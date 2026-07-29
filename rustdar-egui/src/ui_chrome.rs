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
//!
//! ## …but the status bar's *positional* id does, and that is fine
//!
//! Crossing 600pt makes the menu-bar panel appear or vanish, which advances the
//! root `Ui`'s auto-id counter one step more or less before the status bar is
//! shown. egui's `Ui::new_child` computes `unique_id = stable_id.with(parent's
//! next_auto_id_salt)` (`egui-0.35.0/src/ui.rs:255`), so that counter folds into
//! every child scope's registered id **regardless of salting** — `Panel` builds
//! its `Ui` with `id_salt`, which moves only `stable_id`. So the status-bar
//! panel, and the widgets whose auto-ids run off its counter, come back under
//! new ids on the far side of the breakpoint, and egui's debug check reports two
//! rects in the bar as `changed id between passes`.
//!
//! **Decision: leave it.** It costs no widget state, and there is no fix here
//! that is not worse:
//!
//! * `unique_id` is documented by egui as deliberately non-stable — "it can
//!   change if new widgets are added or removed prior to this one… should
//!   therefore only be used for transient interactions (clicks etc), not for
//!   storing state over time" (`ui.rs:346`). Everything that *does* persist —
//!   `ScrollArea`, `ComboBox`, panel sizes — keys on `make_persistent_id`, i.e.
//!   `Ui::id()`, i.e. `stable_id`, which does not move. That is what
//!   `crossing_a_breakpoint_does_not_move_any_widget_id` checks, and it is green
//!   for real reasons, not because the check is shadowed.
//! * Nothing in this bar stores anything under an auto-id anyway: the refresh
//!   button and the separators are stateless, and the auto-poll checkbox writes
//!   to `AutoPollState`. The worst observable cost is a tooltip or a half-made
//!   click on the refresh button being dropped on the single frame of a resize —
//!   which needs the pointer to be holding that button while the window is
//!   being resized.
//! * Making it stable would mean keeping the counter identical either side,
//!   i.e. allocating a menu-bar scope that draws nothing below 600pt. That is
//!   precisely the always-allocated empty child `Ui` removed from
//!   `render_status_bar` below, and the pattern egui's own check flags. `Panel`
//!   offers no explicit-id form (`UiBuilder::id` exists, `Panel` does not use
//!   it), so the alternative is patching egui.
//!
//! `crossing_the_menu_bar_breakpoint_re_keys_only_the_status_bar` holds the
//! *extent* of this: the shift must stay inside the status bar, where nothing is
//! stored, and the ids that key stored state must not move.

use super::ui_menu;
use crate::actions::GuiAction;
use crate::ui_layout::{PointerModality, WidthClass};
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

        let panel = egui::Panel::bottom("status_bar")
            .show_separator_line(true)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 8.0;

                    let refresh_button = ui.add_enabled(
                        !self.radar.fetching,
                        egui::Button::new("\u{1f504}").frame(false),
                    );
                    #[cfg(test)]
                    {
                        probe.refresh = refresh_button.rect;
                    }
                    if refresh_button.clicked() {
                        // The active pane's site, not `radar.config`'s global
                        // one — see `active_pane_fetch_config`.
                        actions.push(GuiAction::FetchRadarScan(self.active_pane_fetch_config()));
                    }
                    refresh_button.on_hover_text("Refresh radar data");

                    ui.separator();

                    if roomy {
                        let drawn = render_auto_poll_status(
                            ui,
                            self.radar.fetching,
                            &mut self.auto_poll,
                            &self.chunk_status,
                        );
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

                    // The Level II scan time above says nothing about a Level
                    // III product's age — they come from different objects,
                    // and the Level III one can be a day older. Drawn only
                    // when there is one, so a Level II pane keeps the bar it
                    // always had.
                    let age_text = render_product_age(
                        ui,
                        self.panes.get(self.active_pane),
                        &self.preferences,
                        roomy,
                    );
                    #[cfg(test)]
                    {
                        probe.product_age_text = age_text;
                    }
                    #[cfg(not(test))]
                    let _ = age_text;

                    if has_hover {
                        ui.separator();
                        render_hover_info(ui, self.panes());
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
                    //
                    // Skipping the allocation fixes the *empty* case only. When
                    // there really is an error the same slot is still welded to
                    // the right edge while everything to its left comes and
                    // goes — the auto-poll block, and now the Level III age —
                    // so its rect stays put while its id moves, and its three
                    // widgets go with it (their auto-ids run off this scope's
                    // `unique_id`). `UiBuilder::id` is the one form that takes
                    // `IdSource::Explicit`, which makes `unique_id ==
                    // stable_id` and takes the parent's counter out of it
                    // entirely. Salting cannot do this.
                    if self.radar.error_message.is_some() {
                        ui.scope_builder(
                            egui::UiBuilder::new()
                                .id(ui.id().with("status_error"))
                                .layout(egui::Layout::right_to_left(egui::Align::Center)),
                            |ui| {
                                render_error_display(ui, &mut self.radar.error_message);
                            },
                        );
                    }
                });
            });

        #[cfg(test)]
        {
            probe.rect = panel.response.rect;
            self.last_status_bar = probe;
        }
        #[cfg(not(test))]
        let _ = panel;
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
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.button("\u{2715}").clicked() {
                                self.drawer_open = false;
                            }
                        });
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
        self.last_menu_leaves
            .extend(menu_frame.drawn.iter().copied());

        for event in menu_frame.events {
            self.apply_menu_event(event, actions);
        }
    }
}

/// How stale a tilt is, in words a status bar has room for.
///
/// Seconds while the number is small enough to mean "just now", then minutes —
/// which is also where the archive path permanently lives, so the two transports
/// read on the same scale.
fn describe_age(secs: u64) -> String {
    match secs {
        0..=9 => "just now".to_owned(),
        s if s < 90 => format!("{s}s old"),
        s => format!("{}m old", (s + 30) / 60),
    }
}

/// Returns the checkbox's rect when one was drawn — while a fetch is running
/// there is a spinner instead.
///
/// The label is three-valued because the two transports differ by two orders of
/// magnitude in latency and the user cannot otherwise tell which one they are
/// on. A feed that has silently retired takes a site from seconds behind the
/// radar to minutes behind it, which is exactly the kind of downgrade a severe
/// weather display should say out loud rather than absorb.
fn render_auto_poll_status(
    ui: &mut egui::Ui,
    fetching: bool,
    auto_poll: &mut super::AutoPollState,
    chunks: &super::ChunkFeedStatus,
) -> Option<egui::Rect> {
    if fetching {
        ui.label("\u{1f504}");
        ui.label("Downloading");
        ui.spinner();
        return None;
    }

    let archive = match auto_poll.time_until_next() {
        Some(remaining) if auto_poll.enabled => format!("archive {remaining}s"),
        _ => "archive off".to_owned(),
    };

    let label = if chunks.feeding {
        // About the tilt on screen, not the feed's progress through the volume.
        // A cut count answers the wrong question — a volume can be nearly
        // assembled while the user's own tilt is still minutes old — and it is
        // operator jargon besides. The archive countdown is left out because
        // that poll is suppressed while a feed runs, so showing it would be a
        // countdown to something that will not fire.
        match chunks.tilt {
            Some(tilt) => format!(
                "\u{26a1} Live \u{2014} {:.1}\u{b0} {}",
                tilt.elevation,
                describe_age(tilt.data_age_secs)
            ),
            None => "\u{26a1} Live \u{2014} waiting for this tilt".to_owned(),
        }
    } else if chunks.retired {
        format!("\u{26a0} Live \u{2014} real-time unavailable, {archive}")
    } else {
        format!("Auto-poll ({archive})")
    };

    let response = ui.checkbox(&mut auto_poll.enabled, label);
    let response = if chunks.feeding {
        response.on_hover_text(format!(
            "Assembled from the real-time chunk feed, checked every {}s. The age \
             is how long ago the radar collected this tilt; it climbs until the \
             beam comes back round. The archive is polled only if the feed stops.",
            chunks.interval_secs
        ))
    } else if chunks.retired {
        response.on_hover_text(
            "The real-time feed stopped responding for this site; falling back \
             to completed archive volumes, which are several minutes old.",
        )
    } else {
        response
    };
    Some(response.rect)
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

/// How old a Level III product is, in words.
///
/// Whole minutes below an hour and `Nh Mm` above it — a volume takes four to
/// six minutes, so minutes are the unit that tells "this volume" from "the one
/// before", and hours are the unit that tells a live field from the previous
/// UTC day's that `level3::latest_key` falls back to.
///
/// A key stamped in the future is not clamped to zero: `ProductStamp::age`
/// deliberately keeps the sign so "impossible" stays distinguishable from
/// "fresh", and a bar that rounded it away would report a clock skew as a
/// current product.
pub(super) fn format_product_age(age: chrono::Duration) -> String {
    if age < chrono::Duration::zero() {
        return "stamped ahead".to_owned();
    }
    let minutes = age.num_minutes();
    if minutes < 60 {
        format!("{minutes} min old")
    } else {
        format!("{}h {}m old", minutes / 60, minutes % 60)
    }
}

/// The Level III product line: when the object behind the pane's radar image
/// was written, and how long ago that was. Returns the text it drew, or `None`
/// when there was nothing to draw.
///
/// Suppressed during loop playback: the frame on screen then is one of
/// [`PaneState::loop_state`]'s, chosen by the animation, and `level3_time`
/// describes the *static* render it replaced.
fn render_product_age(
    ui: &mut egui::Ui,
    pane: Option<&PaneState>,
    prefs: &UserPreferences,
    roomy: bool,
) -> Option<String> {
    let pane = pane?;
    if pane.loop_state.is_active() {
        return None;
    }
    let written = pane.level3_time?;
    let age = format_product_age(chrono::Utc::now().naive_utc() - written);
    let text = if roomy {
        format!(
            "Level III: {} ({age})",
            prefs
                .timezone
                .format_naive_utc(written, "%Y-%m-%d %H:%M:%S")
        )
    } else {
        format!(
            "L3 {} ({age})",
            prefs.timezone.format_naive_utc(written, "%H:%M")
        )
    };
    ui.separator();
    ui.label(&text);
    Some(text)
}

/// The pointer readout: the first pane with a hover value.
///
/// Handed `Gui::panes()` — the visible slice — never the raw vector. A hidden
/// pane is not rendered, so nothing ever clears its `hover_value` again, and
/// scanning the full vector would surface that stale readout forever.
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

#[cfg(test)]
mod age_format {
    use super::format_product_age;
    use chrono::Duration;

    // The negative branch is only reachable through a clock skew, so no UI test
    // arrives at it. Without this, "-5 min old" renders and reads as fresh.
    #[test]
    fn a_stamp_from_the_future_is_not_reported_as_an_age() {
        assert_eq!(format_product_age(Duration::minutes(-5)), "stamped ahead");
        assert_eq!(format_product_age(Duration::seconds(-1)), "stamped ahead");
    }

    #[test]
    fn minutes_below_an_hour_then_hours_above_it() {
        assert_eq!(format_product_age(Duration::zero()), "0 min old");
        assert_eq!(format_product_age(Duration::minutes(59)), "59 min old");
        assert_eq!(format_product_age(Duration::minutes(60)), "1h 0m old");
        assert_eq!(format_product_age(Duration::minutes(1565)), "26h 5m old");
    }
}

#[cfg(test)]
mod age_wording_tests {
    use super::describe_age;

    /// Very fresh data reads as "just now" rather than as a jittering
    /// single-digit counter — the poll is every 5s, so the number would never
    /// settle.
    #[test]
    fn seconds_old_data_reads_as_just_now() {
        assert_eq!(describe_age(0), "just now");
        assert_eq!(describe_age(4), "just now");
        assert_eq!(describe_age(9), "just now");
    }

    /// Through the middle range the exact second is useful: it is how a user
    /// sees the beam coming back round.
    #[test]
    fn the_middle_range_reads_in_seconds() {
        assert_eq!(describe_age(10), "10s old");
        assert_eq!(describe_age(89), "89s old");
    }

    /// Past ninety seconds it switches to minutes, which is the scale the
    /// archive path permanently lives on — so the two transports read on one
    /// scale and the difference between them is obvious.
    #[test]
    fn older_data_reads_in_rounded_minutes() {
        assert_eq!(describe_age(90), "2m old");
        assert_eq!(describe_age(120), "2m old");
        assert_eq!(describe_age(330), "6m old");
    }
}
