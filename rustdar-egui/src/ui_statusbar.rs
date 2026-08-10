//! The floating status bar: one surface spanning the map's bottom inset, on
//! the two wide widths.
//!
//! The docked `Panel::bottom` went with the full-bleed flip — the map now
//! reaches the bottom of the content rect and this bar floats over it, an
//! `egui::Area` under one constant id on both widths that draw it. Compact
//! draws no status bar at all (plan §1.6): the phone top bar carries the
//! short scan summary and the hover readout and shares the collapse state,
//! and the bottom edge belongs to the timeline and the bottom bar. The
//! content here is the docked bar's, moved whole: refresh, the auto-poll
//! state, the scan summary, the data age, the hover readout and the
//! right-aligned error. Two things changed with the float:
//!
//! * A `⏴` collapse button leads the row. Collapsed, the bar shrinks to just
//!   a `⏵` restore button, left-anchored, so the map's bottom edge is clear.
//! * The auto-poll **checkbox** became a display **chip**. A floating bar
//!   reads more than it is worked, and the toggle itself still lives where
//!   every other toggle lives — the ☰ menu's Auto-poll entry — so nothing
//!   became unreachable; the chip gained a `⏸ Auto-poll off` state to keep
//!   the off position readable now that no checkbox shows it.
//!
//! # Ids do not depend on the breakpoint
//!
//! The same discipline as everywhere else in the chrome (see `ui_shell.rs`):
//! one area id at every width, `roomy` gating only which *form* of the text
//! draws, and the error slot pinned to the row's right edge under an explicit
//! [`egui::UiBuilder::id`] — the full reasoning for that device is on the
//! scope itself.

use crate::actions::GuiAction;
use crate::ui_layout::{PointerModality, WidthClass};
use rustdar_radar::types::ScanInfo;
use rustdar_units::UserPreferences;

use super::{PaneState, fade};

/// The bar's inset from the map's left, right and bottom edges.
const BAR_INSET: f32 = 8.0;

/// The collapse button's glyph: the bar shrinks leftward to just a button.
/// `⏴`/`⏵` rather than the demo's `◧`, which egui's bundled fonts do not
/// carry (see `ui_glyphs.rs`). `pub(super)` because the phone top bar shares
/// both the state and the button that flips it (contract 75).
pub(super) const COLLAPSE_LABEL: &str = "\u{23f4}";
/// The restore button's glyph — the collapse's mirror, on the same terms.
pub(super) const RESTORE_LABEL: &str = "\u{23f5}";

impl super::Gui {
    /// The status bar along the bottom, floating over the map — on the two
    /// wide widths only. The phone shell has no status bar (plan §1.6): the
    /// short scan summary lives in the phone top bar, which also hosts the
    /// hover readout and shares the collapse state, and errors get their own
    /// toast (`ui_sheet.rs`).
    ///
    /// The hover readout keys on the *modality*. There is no hover without a
    /// pointing device, so a touchscreen has nothing to show however wide it
    /// is, and a narrow desktop window has a mouse and should keep it — in
    /// the top bar, since this bar is not there (contract 25's adaptation).
    pub(super) fn render_status_bar(
        &mut self,
        ctx: &egui::Context,
        map_rect: egui::Rect,
        actions: &mut Vec<GuiAction>,
    ) {
        // Written for the absences below too: the collapsed time chip anchors
        // above this rect (`ui_timeline.rs`), and a stale rect from a wider
        // or unfaded frame would hold the chip up over open map.
        self.statusbar_rect = None;
        if self.layout.width == WidthClass::Compact {
            // The probe is written even for the absence: a stale report from
            // a wider frame would claim a bar that is not on screen.
            #[cfg(test)]
            {
                self.last_status_bar = super::StatusBarProbe::default();
            }
            return;
        }
        // The fade (§1.8): a fully faded bar does not render at all — that
        // absence is what makes it input-transparent — and a transitioning
        // one draws dimmed and dead (`fade::dim`). The error display inside
        // it goes with it; while fully faded the toast presentation carries
        // the error instead (see `Gui::ui`).
        let Some(fade) = self.chrome_fade() else {
            #[cfg(test)]
            {
                self.last_status_bar = super::StatusBarProbe::default();
            }
            return;
        };
        // The collapse animates as a cross-form fade (§3.3): the full row
        // dims out over the restore button rather than vanishing between two
        // frames. Zero-time under test — see `ui_fade::anim_time`.
        let expanded_factor = ctx.animate_bool_with_time(
            egui::Id::new("statusbar_expanded"),
            !self.statusbar_collapsed,
            super::fade::anim_time(),
        );
        // The restore chip's incoming half — the timeline's two-factor shape,
        // run in sequence rather than in parallel because one frame hosts
        // both forms here: seeded at zero while the full row (or its remnant)
        // is up, so the chip fades in from the frame the remnant clears
        // instead of popping. Instant under test, like every factor.
        let restore_factor = ctx.animate_bool_with_time(
            egui::Id::new("statusbar_restore"),
            expanded_factor <= 0.0,
            super::fade::anim_time(),
        );
        let has_hover = self.layout.modality == PointerModality::Mouse;

        #[cfg(test)]
        let mut probe = super::StatusBarProbe::default();

        let frame = super::shell::chrome_frame(&ctx.global_style());
        let margin = frame.inner_margin;
        let inner_width = map_rect.width() - 2.0 * BAR_INSET - margin.sum().x;

        let area = egui::Area::new(egui::Id::new("status_bar"))
            .order(egui::Order::Middle)
            .pivot(egui::Align2::LEFT_BOTTOM)
            .fixed_pos(egui::pos2(
                map_rect.left() + BAR_INSET,
                map_rect.bottom() - BAR_INSET,
            ))
            .show(ctx, |ui| {
                frame.show(ui, |ui| {
                    fade::dim(ui, fade);
                    if expanded_factor <= 0.0 {
                        // The restore button alone, left-anchored: the whole
                        // point of collapsing is that the rest of the bottom
                        // edge is map. Dimmed (and dead) while its own fade-in
                        // is still in flight.
                        fade::dim(ui, restore_factor);
                        let restore = ui
                            .button(RESTORE_LABEL)
                            .on_hover_text("Restore the status bar");
                        #[cfg(test)]
                        {
                            probe.collapse = restore.rect;
                        }
                        if restore.clicked() {
                            self.statusbar_collapsed = false;
                        }
                        return;
                    }
                    if self.statusbar_collapsed {
                        // The closing remnant: the full row, dimming out and
                        // already dead — the collapse has happened in state.
                        fade::dim(ui, expanded_factor.min(0.99));
                    }

                    ui.set_width(inner_width);
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 8.0;

                        let collapse = ui
                            .button(COLLAPSE_LABEL)
                            .on_hover_text("Collapse the status bar");
                        #[cfg(test)]
                        {
                            probe.collapse = collapse.rect;
                        }
                        if collapse.clicked() {
                            self.statusbar_collapsed = true;
                        }

                        let refresh_button = ui.add_enabled(
                            !self.radar.fetching,
                            egui::Button::new("\u{21bb}").frame(false),
                        );
                        #[cfg(test)]
                        {
                            probe.refresh = refresh_button.rect;
                        }
                        if refresh_button.clicked() {
                            // The active pane's site, not `radar.config`'s global
                            // one — see `active_pane_fetch_config`.
                            actions
                                .push(GuiAction::FetchRadarScan(self.active_pane_fetch_config()));
                        }
                        refresh_button.on_hover_text("Refresh radar data");

                        ui.separator();

                        let drawn = render_auto_poll_status(
                            ui,
                            self.radar.fetching,
                            &self.auto_poll,
                            &self.chunk_status,
                        );
                        #[cfg(test)]
                        {
                            probe.poll_chip = drawn;
                        }
                        #[cfg(not(test))]
                        let _ = drawn;
                        ui.separator();

                        let scan_text = render_scan_info(
                            ui,
                            self.panes
                                .get(self.active_pane)
                                .and_then(|p| p.scan_info.as_ref()),
                            &self.preferences,
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
            });

        // The bar's real rect this frame, for the collapsed time chip to
        // anchor above when it would otherwise land on it (`ui_timeline.rs`
        // — the chip must never overlay this bar). The chip draws later the
        // same frame, so the rect is current.
        self.statusbar_rect = Some(area.response.rect);

        #[cfg(test)]
        {
            probe.rect = area.response.rect;
            probe.collapsed = self.statusbar_collapsed;
            self.last_status_bar = probe;
        }
        #[cfg(not(test))]
        let _ = area;
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

/// The auto-poll chip: what the polling machinery is doing, in one glanceable
/// state. Returns the chip's rect and text when one was drawn — while a fetch
/// is running there is a spinner instead.
///
/// A display chip rather than the checkbox it used to be: the toggle lives in
/// the ☰ menu (`Auto-poll`), beside every other toggle, and a floating bar is
/// read far more than it is clicked. The `⏸` state exists because the off
/// position used to be readable off the checkbox itself and must stay
/// readable off the chip.
///
/// The live states are three-valued because the two transports differ by two
/// orders of magnitude in latency and the user cannot otherwise tell which one
/// they are on. A feed that has silently retired takes a site from seconds
/// behind the radar to minutes behind it, which is exactly the kind of
/// downgrade a severe weather display should say out loud rather than absorb.
fn render_auto_poll_status(
    ui: &mut egui::Ui,
    fetching: bool,
    auto_poll: &super::AutoPollState,
    chunks: &super::ChunkFeedStatus,
) -> Option<(egui::Rect, String)> {
    if fetching {
        ui.label("\u{21bb}");
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
        // countdown to something that will not fire. `⏺` is the timeline's own
        // live glyph (the demo's `⚡` has no glyph in the bundled fonts), and
        // the retired state leads with a plain `!` on the same grounds.
        match chunks.tilt {
            Some(tilt) => format!(
                "\u{23fa} Live - {:.1}\u{b0} {}",
                tilt.elevation,
                describe_age(tilt.data_age_secs)
            ),
            None => "\u{23fa} Live - waiting for this tilt".to_owned(),
        }
    } else if chunks.retired {
        format!("! Live - real-time unavailable, {archive}")
    } else if !auto_poll.enabled {
        "\u{23f8} Auto-poll off".to_owned()
    } else {
        format!("Auto-poll ({archive})")
    };

    let response = ui.label(label.as_str());
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
        response.on_hover_text("Toggle auto-poll from the \u{2630} menu")
    };
    Some((response.rect, label))
}

/// The scan summary — the long form: this bar only exists on the widths with
/// room for it, and the phone top bar's chip is the short form's successor.
/// Returns the text it drew.
fn render_scan_info(
    ui: &mut egui::Ui,
    scan_info: Option<&ScanInfo>,
    prefs: &UserPreferences,
) -> String {
    let text = match scan_info {
        Some(scan_info) => format!(
            "Scan: {} @ {} ({} products)",
            scan_info.site.name,
            prefs
                .timezone
                .format_naive_utc(scan_info.timestamp, "%Y-%m-%d %H:%M:%S"),
            scan_info.available_products.len()
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
) -> Option<String> {
    let collected = pane?.data_time_on_screen()?;
    let age = format_product_age(chrono::Utc::now().naive_utc() - collected);
    let text = format!(
        "Data: {} ({age})",
        prefs
            .timezone
            .format_naive_utc(collected, "%Y-%m-%d %H:%M:%S")
    );
    ui.separator();
    ui.label(&text);
    Some(text)
}

/// The pointer readout: the first pane with a hover value.
///
/// Handed `Gui::panes()` — the visible slice — never the raw vector. A hidden
/// pane is not rendered, so nothing ever clears its `hover_value` again, and
/// scanning the full vector would surface that stale readout forever.
///
/// `pub(super)` because the phone top bar hosts the same readout on Compact
/// (contract 25: the readout follows the modality, not the width).
pub(super) fn render_hover_info(ui: &mut egui::Ui, panes: &[PaneState]) {
    let hover_info = panes.iter().find_map(|p| p.hover_value.as_ref());
    let overlay_hover = panes.iter().find_map(|p| p.overlay_hover_value.as_ref());
    if hover_info.is_some() || overlay_hover.is_some() {
        ui.label("\u{2316}");
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

/// `pub(super)` because the phone shell's error toast (`ui_sheet.rs`) hosts
/// the same dismissable body — the phone has no status bar row to carry it.
/// Returns the ×'s rect while an error is up, for the toast's probe.
pub(super) fn render_error_display(
    ui: &mut egui::Ui,
    error_message: &mut Option<String>,
) -> Option<egui::Rect> {
    let mut dismiss = false;
    let mut close = None;
    if let Some(msg) = error_message.as_deref() {
        let button = ui.button("\u{d7}");
        if button.clicked() {
            dismiss = true;
        }
        close = Some(button.rect);
        ui.label(msg);
    }
    if dismiss {
        *error_message = None;
    }
    close
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
