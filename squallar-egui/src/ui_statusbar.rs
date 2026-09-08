//! The floating status bar: one surface spanning the map's bottom inset, on the two
//! wide widths.

use crate::actions::GuiAction;
use crate::ui_layout::{PointerModality, WidthClass};
use squallar_radar::types::ScanInfo;
use squallar_units::UserPreferences;

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
        let expanded_factor = crate::frame_need::animate_bool(
            ctx,
            egui::Id::new("statusbar_expanded"),
            !self.statusbar_collapsed,
            super::fade::anim_time(),
        );
        let restore_factor = crate::frame_need::animate_bool(
            ctx,
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
                            !self.fetching(),
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
                            self.fetching(),
                            ArchivePoll::of(&self.overlays),
                            &crate::radar_layer::chunk_status(self.liveness()),
                        );
                        self.status_bar_tick = drawn.as_ref().and_then(|&(_, _, tick)| tick);
                        // **The chip is the one thing on this bar that
                        // restates the clock**, and the tick set on the line
                        // above is what buys the frames that restate it. Those
                        // frames needed drawing exactly when the words moved,
                        // which is a fact about the words — see
                        // `note_clock_change`.
                        if let Some((_, label, _)) = drawn.as_ref() {
                            note_clock_change(&mut self.status_bar_chip_text, label);
                        }
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

                        let radar_error = self.layer_error(&crate::radar_layer::POLL_LAYER);
                        if radar_error.is_some() {
                            let mut dismissed = false;
                            ui.scope_builder(
                                egui::UiBuilder::new()
                                    .id(ui.id().with("status_error"))
                                    .layout(egui::Layout::right_to_left(egui::Align::Center)),
                                |ui| {
                                    render_error_display(
                                        ui,
                                        radar_error.as_deref(),
                                        &mut dismissed,
                                    );
                                },
                            );
                            if dismissed {
                                self.dismiss_layer_error(&crate::radar_layer::POLL_LAYER);
                            }
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

/// The live-feed chip's tooltip.
///
/// A function rather than an argument, so the frame path can leave it unbuilt:
/// `Response::on_hover_text` takes its text already made, and this paragraph
/// is a `format!` over a second `format!`.
fn chunk_feed_hover(chunks: &squallar_radar::chunk_feed::ChunkFeedStatus) -> String {
    #[cfg(test)]
    super::hover_text_count::note();
    format!(
        "Assembled from the real-time chunk feed{}. The age is how long ago \
         the radar collected this tilt; it climbs until the beam comes back \
         round. The archive is polled only if the feed stops.",
        if chunks.pushed {
            ", fetched as each chunk is published".to_owned()
        } else {
            format!(", checked every {}s", chunks.interval_secs)
        }
    )
}

/// How stale a tilt is, in words a status bar has room for.
fn describe_age(secs: u64) -> String {
    match secs {
        0..=9 => "just now".to_owned(),
        s if s < 90 => format!("{s}s old"),
        s => format!("{}m old", (s + 30) / 60),
    }
}

/// Raise [`crate::frame_need::NeedCause::Clock`] when the auto-poll chip is
/// about to draw **different words** from the ones it last drew, and remember
/// the new ones. Reports whether it raised.
///
/// # Why this reads the string and not the tick
///
/// [`age_tick`] arms a repaint every second and [`describe_age`] prints "just
/// now" throughout a tilt's first ten seconds, so a cause taken off the tick
/// would call the nine frames inside that window necessary while they showed
/// nothing new — a reclassification by timing, which is the one thing
/// `crate::frame_need`'s design refuses. The
/// words are the picture; a tick that repaints the same words is still waste
/// and this leaves it counted as waste.
///
/// The comparison itself is [`crate::frame_need::note_if_changed`]'s — this
/// chip was its first site, and the transport's listing counter, the map's
/// loading plate and the offline download's byte line now share it. What
/// `last` holds and what a change costs are documented there.
fn note_clock_change(last: &mut Option<String>, text: &str) -> bool {
    crate::frame_need::note_if_changed(last, text, crate::frame_need::NeedCause::Clock)
}

/// How often [`describe_age`] would print something new at this age.
///
/// **A ceiling on the words moving, not a promise that they will.** Below ten
/// seconds [`describe_age`] prints "just now" throughout and this still asks
/// for a frame a second; that is why [`note_clock_change`] reads the string
/// rather than trusting this. Above ninety the string moves once a minute but
/// on `(s + 30) % 60`'s phase, which this sixty-second period does not know,
/// so a minute reading can sit up to a period stale.
fn age_tick(secs: u64) -> std::time::Duration {
    if secs < 90 {
        std::time::Duration::from_secs(1)
    } else {
        std::time::Duration::from_secs(60)
    }
}

/// One second, for [`countdown_tick`]'s remainder.
const NANOS_PER_SEC: u32 = 1_000_000_000;

/// How long until a countdown printing whole seconds of `remaining` prints a
/// different number, or `None` when the number on screen has stopped moving.
///
/// The remainder is load-bearing and is why this is not simply one second:
/// anything faster redraws the same string, anything slower drops a number.
/// Strictly positive by construction — a zero-length sleep re-armed every
/// iteration is the spin this path exists to avoid.
///
/// `remaining.subsec_nanos()` is the same quantity the elapsed-side spelling
/// computed as `NANOS_PER_SEC - elapsed.subsec_nanos()`: the interval is whole
/// seconds, so the two differ only where the clock lands exactly on a second,
/// which is the case the zero arm carries.
fn countdown_tick(remaining: std::time::Duration) -> Option<std::time::Duration> {
    if remaining.is_zero() {
        return None;
    }
    let remainder = remaining.subsec_nanos();
    Some(std::time::Duration::from_nanos(u64::from(
        if remainder == 0 {
            NANOS_PER_SEC
        } else {
            remainder
        },
    )))
}

/// What the archive poll is doing, as the status bar needs to read it — the
/// three questions the chip asks the radar layer, taken once.
#[derive(Clone, Copy, Debug)]
pub(super) struct ArchivePoll {
    /// The switch. Off means the chip says so and no countdown runs.
    enabled: bool,
    /// How long until the next round may start; `None` when no round has ever
    /// been asked for, so there is no timer to print.
    remaining: Option<std::time::Duration>,
}

impl ArchivePoll {
    pub(super) fn of(overlays: &squallar_overlays::render::overlay_state::OverlayRegistry) -> Self {
        Self {
            enabled: crate::radar_layer::auto_poll_enabled(overlays),
            remaining: crate::radar_layer::archive_poll_started(overlays)
                .then(|| crate::radar_layer::archive_poll_delay(overlays))
                .flatten(),
        }
    }

    /// The number the chip prints, rounded **up** to the second: a countdown
    /// showing `0` while a whole fraction of a second is still to run has
    /// dropped a number, and the elapsed-side spelling this replaced counted
    /// the same way.
    pub(super) fn secs(self) -> Option<u64> {
        let remaining = self.remaining?;
        Some(remaining.as_secs() + u64::from(remaining.subsec_nanos() > 0))
    }

    /// How long until [`Self::secs`] would print a different number, or `None`
    /// when there is no number on screen to move: the poll is off, no round
    /// has been asked for yet, or the count has bottomed out.
    pub(super) fn countdown_tick(self) -> Option<std::time::Duration> {
        if !self.enabled {
            return None;
        }
        countdown_tick(self.remaining?)
    }
}

/// The auto-poll chip: what the polling machinery is doing, in one glanceable
/// state.
fn render_auto_poll_status(
    ui: &mut egui::Ui,
    fetching: bool,
    auto_poll: ArchivePoll,
    chunks: &squallar_radar::chunk_feed::ChunkFeedStatus,
) -> Option<(egui::Rect, String, Option<std::time::Duration>)> {
    if fetching {
        ui.label("\u{21bb}");
        ui.label("Downloading");
        // A wait mark, not a spinner: the fetch's answer comes back through a
        // channel the app drains, from a worker that posts its own wake, and
        // the spinner that turned here bought a frame at the display's rate
        // for the whole download with nothing new on any of them.
        super::wait::mark(ui);
        return None;
    }

    let (archive, archive_tick) = match auto_poll.secs() {
        Some(remaining) if auto_poll.enabled => {
            (format!("archive {remaining}s"), auto_poll.countdown_tick())
        }
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
        // `on_hover_ui`, not `on_hover_text`: the latter is exactly this
        // closure with its text ALREADY built, so a paragraph nobody was
        // hovering was formatted on every frame of a live feed. egui runs
        // this body only while the tooltip is up.
        response.on_hover_ui(|ui| {
            ui.set_max_width(ui.spacing().tooltip_width);
            ui.add(egui::Label::new(chunk_feed_hover(chunks)));
        })
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
/// Draw `message` with a dismiss cross, and report through `dismissed` whether
/// the cross was clicked.
///
/// The widget reports rather than mutates: what a dismissal means is "the user
/// acknowledged *this failure*", and only the caller knows which failure that
/// is (see [`Gui::layer_error`]). It used to take `&mut Option<String>` and
/// clear it, which is what made the message a piece of state the shell had to
/// own a copy of.
pub(super) fn render_error_display(
    ui: &mut egui::Ui,
    message: Option<&str>,
    dismissed: &mut bool,
) -> Option<egui::Rect> {
    let mut close = None;
    if let Some(msg) = message {
        let button = ui.button("\u{d7}");
        if button.clicked() {
            *dismissed = true;
        }
        close = Some(button.rect);
        ui.label(msg);
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

/// **The clock cause, held to the words rather than to the tick.**
///
/// The status bar's age string is a picture that moves on a clock and, until
/// this cause existed, told nobody: `age_tick` armed a repaint a second and
/// every one of the frames it bought was scored waste. The reclassification is
/// only honest if it is exact in both directions, so both are held here — the
/// frames where the words move, and the frames where the same tick fires and
/// they do not.
#[cfg(test)]
mod clock_cause_tests {
    use super::{age_tick, describe_age, note_clock_change};
    use crate::frame_need::{NeedCause, take};

    /// One frame of the chip, answered **through the register** rather than
    /// through the helper's return value: the register is what the verdict
    /// reads, and a helper that returned `true` and raised nothing would be
    /// exactly the failure this is here to exclude.
    fn frame(last: &mut Option<String>, text: &str) -> bool {
        let _ = take();
        let said = note_clock_change(last, text);
        let raised = take() & NeedCause::Clock.bit() != 0;
        assert_eq!(
            said, raised,
            "`note_clock_change` reported {said} and raised {raised}: what it \
             says and what the verdict reads have come apart",
        );
        raised
    }

    /// **The same words raise nothing; different words raise once.**
    #[test]
    fn the_cause_follows_the_words_and_not_the_call() {
        let mut last = None;
        assert!(
            frame(&mut last, "10s old"),
            "the chip drew words where there had been none and raised nothing",
        );
        for _ in 0..10 {
            assert!(
                !frame(&mut last, "10s old"),
                "redrawing the same words raised a cause, so a tick that shows \
                 nothing new is being called a necessary frame",
            );
        }
        assert!(frame(&mut last, "11s old"));
        assert!(!frame(&mut last, "11s old"));
    }

    /// **The seconds the tick buys and the words do not move.**
    ///
    /// `age_tick` asks for a frame a second across the whole sub-ninety range,
    /// but `describe_age` prints "just now" throughout a tilt's first ten
    /// seconds — so nine of the frames that window buys are waste and must
    /// stay counted as waste. This is the half of the reclassification that
    /// does not flatter the app, and a cause taken off the tick instead of off
    /// the string would lose it.
    #[test]
    fn the_just_now_window_of_a_tilt_is_still_waste() {
        for secs in 0..90 {
            assert_eq!(
                age_tick(secs),
                std::time::Duration::from_secs(1),
                "precondition: this range is where the chip asks for a frame a \
                 second, and {secs}s does not",
            );
        }
        let mut last = Some(describe_age(0));
        let mut raised = 0;
        for secs in 1..=9 {
            raised += u32::from(frame(&mut last, &describe_age(secs)));
        }
        assert_eq!(
            raised, 0,
            "the chip printed \"just now\" throughout and {raised} of those \
             nine tick-bought frames were called necessary",
        );
    }

    /// **And the seconds where the words really do move are paid for.**
    ///
    /// The other direction, over the range `describe_age` prints seconds in:
    /// every one of these frames shows a number nobody had seen, so every one
    /// of them raises.
    #[test]
    fn every_second_of_the_seconds_range_shows_new_words() {
        let mut last = Some(describe_age(10));
        for secs in 11..=89 {
            assert!(
                frame(&mut last, &describe_age(secs)),
                "the chip went from {:?} to {:?} and raised nothing",
                describe_age(secs - 1),
                describe_age(secs),
            );
        }
        // Past ninety it is minutes, and the words stop moving every second
        // even though the age keeps climbing.
        assert!(frame(&mut last, &describe_age(90)));
        for secs in 91..=120 {
            assert!(
                !frame(&mut last, &describe_age(secs)),
                "{secs}s reads the same as {}s and raised a cause",
                secs - 1,
            );
        }
    }

    /// **The tamper: freeze the string and the frames go back to waste.**
    ///
    /// A reclassification that survives its own input being frozen is not a
    /// reclassification, it is a threshold wearing a cause's name. Here the
    /// tick still fires sixty times, the chip is still drawn sixty times, and
    /// with the words held still not one of those frames is called necessary.
    #[test]
    fn a_frozen_string_buys_no_frames_at_all() {
        const FROZEN_AGE: u64 = 42;
        let mut last = Some(describe_age(FROZEN_AGE));
        let mut raised = 0;
        for _ in 0..60 {
            assert_eq!(
                age_tick(FROZEN_AGE),
                std::time::Duration::from_secs(1),
                "precondition: the tick is still armed while the words are held",
            );
            raised += u32::from(frame(&mut last, &describe_age(FROZEN_AGE)));
        }
        assert_eq!(
            raised, 0,
            "{raised} of sixty frames were called necessary while the words on \
             screen never changed",
        );
    }
}
