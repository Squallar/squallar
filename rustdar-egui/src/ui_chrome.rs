//! The one UI chrome: top bar, status bar and layers panel.
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
//! 1. top bar (top) — see `ui_topbar.rs`
//! 2. status bar (bottom)
//! 3. layers panel (left)
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
//! The panels' *positional* ids hold too, and that is no accident. egui's
//! `Ui::new_child` computes `unique_id = stable_id.with(parent's
//! next_auto_id_salt)` (`egui-0.35.0/src/ui.rs:255`), so the root `Ui`'s
//! auto-id counter folds into every panel's registered id **regardless of
//! salting** — which is why a panel that appears or vanishes with the width
//! would re-key everything shown after it. The menu-bar panel used to do
//! exactly that at 600pt, re-keying the status bar on every crossing; its
//! replacement, the top bar, is drawn at every width, so nothing above the
//! status bar is conditional and the counter is the same on both sides of
//! every breakpoint. `crossing_a_breakpoint_re_keys_nothing` pins the whole
//! claim, and `crossing_a_breakpoint_does_not_move_any_widget_id` pins the
//! stored-state half of it.

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

/// What the chrome produced this frame.
pub(super) struct ChromeOutput {
    pub actions: Vec<GuiAction>,
    /// Screen rects of floating chrome drawn *over* the map, which map click
    /// handling must not treat as map clicks.
    ///
    /// This is an **output** of the chrome rather than something the map
    /// reconstructs — only the code that draws a floating thing knows where it
    /// is. Empty in practice since the hamburger went: everything left either
    /// claims panel space or is an egui layer above `Background`, which the
    /// layer half of `is_pos_blocked` catches with no plumbing. The mechanism
    /// stays because painted-in-pane chrome has no layer to be caught by, and
    /// the next thing painted over a pane will need it again.
    pub excluded_rects: Vec<egui::Rect>,
}

impl super::Gui {
    /// Draw all the chrome around the map, in the order the panels must claim
    /// their space.
    pub(super) fn render_chrome(&mut self, ui: &mut egui::Ui) -> ChromeOutput {
        let mut actions = Vec::new();

        self.render_top_bar(ui, &mut actions);
        self.render_status_bar(ui, &mut actions);

        // Persistent-by-default sidebar on Expanded, drawer elsewhere; the top
        // bar's Layers toggle is the one way in and out on every width.
        if self.layers_panel_visible() {
            self.render_layers_panel(ui, &mut actions);
        }

        ChromeOutput {
            actions,
            excluded_rects: Vec::new(),
        }
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

                    // How old what is on screen is, for every product alike.
                    // The scan summary above answers a different question —
                    // which volume is loaded — and for a product fetched from
                    // the Level III bucket it can be a day out.
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
    /// drawer covers the map and wants a close button where the user already
    /// is — the sidebar's way out is the top bar's Layers toggle.
    fn render_layers_panel(&mut self, ui: &mut egui::Ui, actions: &mut Vec<GuiAction>) {
        let is_drawer = !self.layout.width.has_persistent_sidebar();

        let mut pane = std::mem::take(&mut self.panes[self.active_pane]);

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
                        self.render_layer_controls(
                            ui,
                            &mut pane,
                            COMBO_BOX_WIDTH,
                            LAYER_CONTROL_ID_PREFIX,
                            actions,
                        );
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
        // After the restore, so the source it copies from is the real pane rather
        // than the `mem::take` placeholder. It deliberately does **not** copy
        // `content`: a pane's kind is how this pane presents the shared subject,
        // not part of the subject, and propagating it would convert every sibling
        // the moment one pane became a 3D view — from a setting called "Sync
        // Layers". The reasoning is written out on `propagate_layer_sync` itself.
        self.propagate_layer_sync();
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
                "\u{26a1} Live - {:.1}\u{b0} {}",
                tilt.elevation,
                describe_age(tilt.data_age_secs)
            ),
            None => "\u{26a1} Live - waiting for this tilt".to_owned(),
        }
    } else if chunks.retired {
        format!("\u{26a0} Live - real-time unavailable, {archive}")
    } else {
        format!("Auto-poll ({archive})")
    };

    let response = ui.checkbox(&mut auto_poll.enabled, label);
    let response = if chunks.feeding {
        response.on_hover_text(format!(
            "Assembled from the real-time chunk feed{}. The age is how long ago \
             the radar collected this tilt; it climbs until the beam comes back \
             round. The archive is polled only if the feed stops.",
            if chunks.pushed {
                ", fetched as each chunk is published".to_owned()
            } else {
                format!(", checked every {}s", chunks.interval_secs)
            }
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

/// How old the data behind a pane's image is, in words.
///
/// Whole minutes below an hour and `Nh Mm` above it — a volume takes four to
/// six minutes, so minutes are the unit that tells "this volume" from "the one
/// before", and hours are the unit that tells a live field from the previous
/// UTC day's that `level3::latest_key` falls back to.
///
/// A time in the future is not clamped to zero: `ProductStamp::age` deliberately
/// keeps the sign so "impossible" stays distinguishable from "fresh", and a bar
/// that rounded it away would report a clock skew as current data.
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

/// The data line: when the data behind the pane's radar image was collected, and
/// how long ago that was. Returns the text it drew, or `None` when there was
/// nothing to draw.
///
/// **One line for every product.** It used to say `Level III:` and appear only for
/// a product fetched from the bucket, which made the datasource a thing the user
/// could read off the bar — and made its *absence* informative too, since a
/// Level II pane silently had no age at all. The age itself is worth keeping: a
/// site down since yesterday paints a field up to ~48 h old, and the scan summary
/// beside this describes the Level II volume, so it looks perfectly current.
///
/// Under an active loop this reports the playing frame's own time rather than
/// being suppressed — see [`PaneState::data_time_on_screen`].
///
/// The scan summary's time and this one coincide for a product read off the
/// volume, and that redundancy is deliberate: the two answer different questions
/// (which volume is loaded, versus how old what you are looking at is), and making
/// the second conditional on the first disagreeing is what produced the tell.
fn render_product_age(
    ui: &mut egui::Ui,
    pane: Option<&PaneState>,
    prefs: &UserPreferences,
    roomy: bool,
) -> Option<String> {
    let collected = pane?.data_time_on_screen()?;
    let age = format_product_age(chrono::Utc::now().naive_utc() - collected);
    let text = if roomy {
        format!(
            "Data: {} ({age})",
            prefs
                .timezone
                .format_naive_utc(collected, "%Y-%m-%d %H:%M:%S")
        )
    } else {
        format!(
            "Data {} ({age})",
            prefs.timezone.format_naive_utc(collected, "%H:%M")
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
