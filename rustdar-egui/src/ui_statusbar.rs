//! The floating status bar: one surface spanning the map's bottom inset, on the two
//! wide widths.

use crate::actions::GuiAction;
use crate::ui_layout::{PointerModality, WidthClass};
use rustdar_radar::types::ScanInfo;
use rustdar_units::UserPreferences;

use super::{PaneState, fade};

/// The bar's inset from the map's left, right and bottom edges.
const BAR_INSET: f32 = 8.0;

/// The collapse button's glyph: the bar shrinks leftward to just a button.
pub(super) const COLLAPSE_LABEL: &str = "\u{23f4}";
/// The restore button's glyph — the collapse's mirror, on the same terms.
pub(super) const RESTORE_LABEL: &str = "\u{23f5}";

impl super::Gui {
    /// The status bar along the bottom, floating over the map — on the two wide
    /// widths only.
    pub(super) fn render_status_bar(
        &mut self,
        ctx: &egui::Context,
        map_rect: egui::Rect,
        actions: &mut Vec<GuiAction>,
    ) {
        self.statusbar_rect = None;
        self.status_bar_tick = None;
        if self.layout.width == WidthClass::Compact {
            #[cfg(test)]
            {
                self.probes.last_status_bar = super::StatusBarProbe::default();
            }
            return;
        }
        let Some(fade) = self.chrome_fade() else {
            #[cfg(test)]
            {
                self.probes.last_status_bar = super::StatusBarProbe::default();
            }
            return;
        };
        let expanded_factor = ctx.animate_bool_with_time(
            egui::Id::new("statusbar_expanded"),
            !self.statusbar_collapsed,
            super::fade::anim_time(),
        );
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
                        self.status_bar_tick = drawn.as_ref().and_then(|&(_, _, tick)| tick);
                        #[cfg(test)]
                        {
                            probe.poll_chip = drawn.map(|(rect, label, _)| (rect, label));
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

        self.statusbar_rect = Some(area.response.rect);

        #[cfg(test)]
        {
            probe.rect = area.response.rect;
            probe.collapsed = self.statusbar_collapsed;
            self.probes.last_status_bar = probe;
        }
        #[cfg(not(test))]
        let _ = area;
    }
}

/// How stale a tilt is, in words a status bar has room for.
fn describe_age(secs: u64) -> String {
    match secs {
        0..=9 => "just now".to_owned(),
        s if s < 90 => format!("{s}s old"),
        s => format!("{}m old", (s + 30) / 60),
    }
}

/// How often [`describe_age`] would print something new at this age.
fn age_tick(secs: u64) -> std::time::Duration {
    if secs < 90 {
        std::time::Duration::from_secs(1)
    } else {
        std::time::Duration::from_secs(60)
    }
}

/// The auto-poll chip: what the polling machinery is doing, in one glanceable
/// state.
fn render_auto_poll_status(
    ui: &mut egui::Ui,
    fetching: bool,
    auto_poll: &super::AutoPollState,
    chunks: &rustdar_radar::chunk_feed::ChunkFeedStatus,
) -> Option<(egui::Rect, String, Option<std::time::Duration>)> {
    if fetching {
        ui.label("\u{21bb}");
        ui.label("Downloading");
        ui.spinner();
        return None;
    }

    let (archive, archive_tick) = match auto_poll.time_until_next() {
        Some(remaining) if auto_poll.enabled => (
            format!("archive {remaining}s"),
            auto_poll.countdown_tick_delay(),
        ),
        _ => ("archive off".to_owned(), None),
    };
    let mut tick = archive_tick;

    let label = if chunks.feeding {
        match chunks.tilt {
            Some(tilt) => {
                tick = Some(age_tick(tilt.data_age_secs));
                format!(
                    "\u{23fa} Live - {:.1}\u{b0} {}",
                    tilt.elevation,
                    describe_age(tilt.data_age_secs)
                )
            }
            None => {
                tick = None;
                "\u{23fa} Live - waiting for this tilt".to_owned()
            }
        }
    } else if chunks.retired {
        format!("! Live - real-time unavailable, {archive}")
    } else if !auto_poll.enabled {
        tick = None;
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
    Some((response.rect, label, tick))
}

/// The scan summary — the long form: this bar only exists on the widths with room
/// for it, and the phone top bar's chip is the short form's successor.
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
/// how long ago that was.
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

/// `pub(super)` because the phone shell's error toast (`ui_sheet.rs`) hosts the
/// same dismissable body — the phone has no status bar row to carry it.
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

    /// Very fresh data reads as "just now" rather than as a jittering single-digit
    /// counter — the poll is every 5s, so the number would never settle.
    #[test]
    fn seconds_old_data_reads_as_just_now() {
        assert_eq!(describe_age(0), "just now");
        assert_eq!(describe_age(4), "just now");
        assert_eq!(describe_age(9), "just now");
    }

    /// Through the middle range the exact second is useful: it is how a user sees
    /// the beam coming back round.
    #[test]
    fn the_middle_range_reads_in_seconds() {
        assert_eq!(describe_age(10), "10s old");
        assert_eq!(describe_age(89), "89s old");
    }

    /// Past ninety seconds it switches to minutes, which is the scale the archive
    /// path permanently lives on — so the two transports read on one scale and the
    /// difference between them is obvious.
    #[test]
    fn older_data_reads_in_rounded_minutes() {
        assert_eq!(describe_age(90), "2m old");
        assert_eq!(describe_age(120), "2m old");
        assert_eq!(describe_age(330), "6m old");
    }
}
