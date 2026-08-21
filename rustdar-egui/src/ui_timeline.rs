//! The floating timeline transport: time navigation and the radar loop, in
//! one surface floating over the map's bottom edge.

use crate::actions::GuiAction;
use crate::pane::{LoopFrame, TimeMode, TimeStep};

/// Available time step options, in the order the picker offers them. The
/// first is not a duration at all — [`TimeStep::OneFrame`] means "to the next
/// frame the pane's time-primary layer actually has", which is a different
/// distance at every site and every VCP. It used to be spelled as a `0`
/// seconds sentinel; the `0` survives only in the config file, where
/// [`TimeStep::as_secs`] still writes it.
pub(super) const TIME_STEP_OPTIONS: &[(TimeStep, &str)] = &[
    (TimeStep::OneFrame, "1 scan"),
    (TimeStep::Secs(600), "10 min"),
    (TimeStep::Secs(1800), "30 min"),
    (TimeStep::Secs(3600), "1 hr"),
    (TimeStep::Secs(7200), "2 hr"),
    (TimeStep::Secs(21600), "6 hr"),
    (TimeStep::Secs(43200), "12 hr"),
];

/// Why a one-frame step is not offered on a pane that has no layer supplying
/// frames — shown on hover rather than by hiding the entry, so the option a
/// pane could have is still visible from the pane that cannot.
const NO_FRAME_SERIES_REASON: &str = "no frame-series layer on this pane";

/// How far above the map's bottom edge the transport floats (plan §1.5) —
/// clear of the status bar spanning the bottom inset below it.
const BOTTOM_CLEARANCE: f32 = 44.0;

/// The transport's widest form, **outer edge to outer edge** — §1.5's
/// `min(880, full − 24)` is a claim about the surface on the glass, frame
/// included, so the frame's own margins are subtracted before the content is
/// sized (the status bar's margin math; the §5.9 bookkeeping fix).
const MAX_OUTER_WIDTH: f32 = 880.0;

/// What the transport leaves free at the sides on a narrow screen.
const SIDE_INSET: f32 = 24.0;

/// The collapsed chip's inset from the map's bottom-right corner.
const CHIP_INSET: f32 = 8.0;

/// The archive scrubber's live threshold: releasing at or past this fraction
/// of the rail means "back to live", not "an archive moment very near now".
const SCRUB_LIVE_THRESHOLD: f32 = 0.99;

/// Slider width for the row-2 tuning sliders — modest, so lookback and speed
/// share a row.
const TUNING_SLIDER_WIDTH: f32 = 120.0;

/// How much longer one interval must be than another before the caption calls
/// the difference out — as a ratio, and as an absolute floor in seconds. Both
/// must be cleared.
const NOTICEABLE_RATIO: f64 = 1.5;
/// The absolute half of [`NOTICEABLE_RATIO`]'s rule, in seconds.
const NOTICEABLE_FLOOR_SECS: i64 = 60;

/// Whether `longer` is enough longer than `shorter` for the caption to say so.
fn markedly_longer(longer: i64, shorter: i64) -> bool {
    longer - shorter >= NOTICEABLE_FLOOR_SECS && longer as f64 > shorter as f64 * NOTICEABLE_RATIO
}

/// A duration in words, for the loop caption: `"45 s"`, `"6 min"`, `"2h 54m"`.
fn format_span(secs: i64) -> String {
    if secs < 60 {
        return format!("{secs} s");
    }
    let mins = (secs + 30) / 60;
    if mins < 60 {
        format!("{mins} min")
    } else {
        format!("{}h {}m", mins / 60, mins % 60)
    }
}

/// The loop's own extent, in words, for the head of the row-2 caption.
fn loop_span_phrase(
    frames: &[LoopFrame],
    listing_sampled: Option<bool>,
    scan_step_secs: Option<u32>,
    settled: bool,
) -> Option<String> {
    let first = frames.first()?;
    let last = frames.last()?;
    let count = frames.len();
    let so_far = if settled { "" } else { " so far" };

    let span = (last.timestamp - first.timestamp).num_seconds();
    if count == 1 || span <= 0 {
        let plural = if count == 1 { "frame" } else { "frames" };
        return Some(format!(
            "This loop is {count} {plural}{so_far}, so it spans no time yet"
        ));
    }

    let mut gaps: Vec<i64> = frames
        .windows(2)
        .map(|pair| (pair[1].timestamp - pair[0].timestamp).num_seconds())
        .collect();
    gaps.sort_unstable();
    let shortest = gaps[0];
    let longest = gaps[gaps.len() - 1];
    let typical = gaps[gaps.len() / 2];

    let uneven = markedly_longer(longest, typical) || markedly_longer(typical, shortest);
    let spacing = if uneven {
        format!(
            "{} to {} apart",
            format_span(shortest),
            format_span(longest)
        )
    } else {
        format!("~{} apart", format_span(typical))
    };

    let fidelity = match (listing_sampled, scan_step_secs) {
        (Some(true), Some(step)) => {
            format!("sampled from ~{} scans, ", format_span(i64::from(step)))
        }
        (Some(true), None) => "sampled, ".to_owned(),
        (Some(false), _) => "every scan, ".to_owned(),
        (None, _) => String::new(),
    };

    Some(format!(
        "This loop spans {} over {count} frames{so_far}, {fidelity}{spacing}",
        format_span(span)
    ))
}

/// What the timeline drew last frame, as it was drawn. Reported by the
/// renderer, never rebuilt by a test — see `ui_menu::DrawnMenuLeaf` for the
/// pattern.
#[cfg(test)]
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct TimelineProbe {
    /// The expanded transport's whole rect, off the area's own response.
    pub rect: egui::Rect,
    /// Whether the transport was collapsed to its chip this frame.
    pub collapsed: bool,
    /// The restore chip's rect, when collapsed.
    pub chip: egui::Rect,
    /// The Live button, and whether it was drawn in the red not-live style.
    pub live: (egui::Rect, bool),
    /// The back (⏴) button.
    pub back: egui::Rect,
    /// The forward (⏵) button, and whether it was enabled.
    pub fwd: (egui::Rect, bool),
    /// The step picker's collapsed combo box.
    pub step_dropdown: egui::Rect,
    /// The loop toggle, and whether it read as on.
    pub loop_toggle: (egui::Rect, bool),
    /// The scrubber slider.
    pub scrubber: egui::Rect,
    /// The timestamp button, and the text it showed.
    pub timestamp: (egui::Rect, String),
    /// The age chip's text — empty when there is no data time to age.
    pub age_text: String,
    /// The `...` row-2 expander.
    pub expander: egui::Rect,
    /// The `⏷` collapse button.
    pub collapse: egui::Rect,
    /// Row 2, when it was drawn.
    pub row2: Option<TimelineRow2Probe>,
}

#[cfg(test)]
impl Default for TimelineProbe {
    fn default() -> Self {
        Self {
            rect: egui::Rect::NOTHING,
            collapsed: false,
            chip: egui::Rect::NOTHING,
            live: (egui::Rect::NOTHING, false),
            back: egui::Rect::NOTHING,
            fwd: (egui::Rect::NOTHING, false),
            step_dropdown: egui::Rect::NOTHING,
            loop_toggle: (egui::Rect::NOTHING, false),
            scrubber: egui::Rect::NOTHING,
            timestamp: (egui::Rect::NOTHING, String::new()),
            age_text: String::new(),
            expander: egui::Rect::NOTHING,
            collapse: egui::Rect::NOTHING,
            row2: None,
        }
    }
}

/// What the Lookback and Speed sliders say about their reach.
///
/// `set_loop_span_secs` and `set_loop_speed_fps` write **every** pane,
/// including fully unlinked ones — one window, one number — while sitting in
/// the same row as a transport that respects the links. The behaviour is
/// defensible and unchanged; the silence was not.
const TUNING_SCOPE_CAPTION: &str = "Lookback and Speed apply to every pane, linked or not.";

/// Row 2 of the probe: the loop tuning as drawn. The transport rects are
/// [`egui::Rect::NOTHING`] and the texts empty while no loop is active — the
/// row draws its tuning sliders unconditionally and its frame transport only
/// for a loop that exists, exactly as the layers panel's block did.
#[cfg(test)]
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct TimelineRow2Probe {
    pub lookback: egui::Rect,
    pub speed: egui::Rect,
    /// The caption under the two sliders, naming what they reach.
    pub tuning_scope: String,
    pub prev: egui::Rect,
    pub play: egui::Rect,
    pub next: egui::Rect,
    pub seek: egui::Rect,
    /// The current frame's timestamp text, as drawn.
    pub frame_text: String,
    /// The "n/m frames rendered" (or "Rendering n/m...") line, as drawn.
    pub rendered_text: String,
    /// The row's closing caption — the platform's frame budget and the
    /// per-pane unlink hint — as drawn.
    pub caption: String,
}

#[cfg(test)]
impl Default for TimelineRow2Probe {
    fn default() -> Self {
        Self {
            lookback: egui::Rect::NOTHING,
            speed: egui::Rect::NOTHING,
            tuning_scope: String::new(),
            prev: egui::Rect::NOTHING,
            play: egui::Rect::NOTHING,
            next: egui::Rect::NOTHING,
            seek: egui::Rect::NOTHING,
            frame_text: String::new(),
            rendered_text: String::new(),
            caption: String::new(),
        }
    }
}

impl super::Gui {
    /// Draw the timeline transport (or its collapsed chip) over the map.
    pub(super) fn render_timeline(
        &mut self,
        ctx: &egui::Context,
        map_rect: egui::Rect,
        phone_bar_top: Option<f32>,
        actions: &mut Vec<GuiAction>,
    ) {
        #[cfg(test)]
        {
            self.probes.last_timeline = TimelineProbe::default();
        }

        let Some(chrome) = self.chrome_fade() else {
            return;
        };

        let expanded_factor = ctx.animate_bool_with_time(
            egui::Id::new("timeline_expanded"),
            !self.timeline_collapsed,
            super::fade::anim_time(),
        );
        let chip_factor = ctx.animate_bool_with_time(
            egui::Id::new("timeline_chip"),
            self.timeline_collapsed,
            super::fade::anim_time(),
        );

        if chip_factor > 0.0 {
            let opacity = if self.timeline_collapsed {
                chrome
            } else {
                (chrome * chip_factor).min(0.99)
            };
            self.render_timeline_chip(ctx, map_rect, phone_bar_top, opacity);
        }
        if expanded_factor <= 0.0 {
            return;
        }
        let opacity = if self.timeline_collapsed {
            (chrome * expanded_factor).min(0.99)
        } else {
            chrome
        };

        let frame = super::shell::chrome_frame(&ctx.global_style());
        let (anchor_bottom, outer_width) = match phone_bar_top {
            Some(bar_top) => (bar_top, map_rect.width()),
            None => (
                map_rect.bottom() - BOTTOM_CLEARANCE,
                (map_rect.width() - SIDE_INSET).min(MAX_OUTER_WIDTH),
            ),
        };
        let inner_width = outer_width - frame.inner_margin.sum().x - 2.0 * frame.stroke.width;
        let area = egui::Area::new(egui::Id::new("timeline"))
            .order(egui::Order::Middle)
            .pivot(egui::Align2::CENTER_BOTTOM)
            .fixed_pos(egui::pos2(map_rect.center().x, anchor_bottom))
            .show(ctx, |ui| {
                frame.show(ui, |ui| {
                    super::fade::dim(ui, opacity);
                    ui.set_width(inner_width);
                    self.render_timeline_row1(ui, actions);
                    if self.timeline_row2 {
                        self.render_timeline_row2(ui, actions);
                    }
                });
            });

        #[cfg(test)]
        {
            self.probes.last_timeline.rect = area.response.rect;
        }
        #[cfg(not(test))]
        let _ = area;
    }

    /// The collapsed form: a ⏱-and-timestamp chip at the map's bottom-right
    /// — above the bottom bar on the phone, whose Live chip is the other
    /// restore route (plan §1.5), and above the floating status bar on the
    /// wider widths: both bars own the bottom edge, and a chip anchored to
    /// the map's corner sat on top of them (the first-run finding). The
    /// offsets come from the bars' real rects this frame, never a guessed
    /// constant.
    fn render_timeline_chip(
        &mut self,
        ctx: &egui::Context,
        map_rect: egui::Rect,
        phone_bar_top: Option<f32>,
        opacity: f32,
    ) {
        let would_land_on = |bar: egui::Rect| {
            let Some(size) = ctx
                .memory(|m| m.area_rect(egui::Id::new("timeline_chip")))
                .map(|r| r.size())
            else {
                return true;
            };
            let corner = egui::pos2(
                map_rect.right() - CHIP_INSET,
                map_rect.bottom() - CHIP_INSET,
            );
            egui::Rect::from_min_size(corner - size, size).intersects(bar)
        };
        let bottom = phone_bar_top
            .or_else(|| {
                self.statusbar_rect
                    .filter(|&bar| would_land_on(bar))
                    .map(|bar| bar.top())
            })
            .unwrap_or(map_rect.bottom());
        let area = egui::Area::new(egui::Id::new("timeline_chip"))
            .order(egui::Order::Middle)
            .pivot(egui::Align2::RIGHT_BOTTOM)
            .fixed_pos(egui::pos2(
                map_rect.right() - CHIP_INSET,
                bottom - CHIP_INSET,
            ))
            .show(ctx, |ui| {
                super::shell::chrome_frame(&ctx.global_style()).show(ui, |ui| {
                    super::fade::dim(ui, opacity);
                    ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);
                    let (_, live) = self.chip_time_source();
                    let chip = ui.button(format!(
                        "\u{23f1} {} - {}",
                        self.active_time_label(),
                        if live { "live" } else { "archive" }
                    ));
                    if chip.clicked() {
                        self.timeline_collapsed = false;
                    }
                });
            });

        #[cfg(test)]
        {
            self.probes.last_timeline.collapsed = true;
            self.probes.last_timeline.chip = area.response.rect;
        }
        #[cfg(not(test))]
        let _ = area;
    }

    /// The time the chips describe, and whether its source pane is live —
    /// with the fallback that keeps a non-map active pane honest (the
    /// first-run `--:--:--` finding): the active pane's on-screen time,
    /// else the static [`PaneState::data_time`] it carries whatever its
    /// kind, else the freshest visible pane's on-screen time. The
    /// live/archive flag travels with whichever pane supplied the time, so
    /// the annotation describes the time actually shown.
    pub(super) fn chip_time_source(&self) -> (Option<chrono::NaiveDateTime>, bool) {
        let active = &self.panes[self.active_pane];
        if let Some(t) = active.data_time_on_screen().or(active.data_time) {
            return (Some(t), active.viewing_live);
        }
        self.panes()
            .iter()
            .filter_map(|pane| pane.data_time_on_screen().map(|t| (t, pane.viewing_live)))
            .max_by_key(|&(t, _)| t)
            .map_or((None, active.viewing_live), |(t, live)| (Some(t), live))
    }

    /// The time of [`Self::chip_time_source`], as the timestamp button, the
    /// collapsed chip and the bottom bar's Live chip all print it. One
    /// function so the three cannot drift.
    pub(super) fn active_time_label(&self) -> String {
        match self.chip_time_source().0 {
            Some(t) => self.preferences.timezone.format_naive_utc(t, "%H:%M:%S"),
            None => "--:--:--".to_owned(),
        }
    }

    /// Row 1: the always-on transport.
    fn render_timeline_row1(&mut self, ui: &mut egui::Ui, actions: &mut Vec<GuiAction>) {
        let (source_time, source_live) = self.chip_time_source();
        let age_text = source_time
            .map(|collected| {
                super::statusbar::format_product_age(chrono::Utc::now().naive_utc() - collected)
            })
            .unwrap_or_default();
        let stamp_text = format!(
            "{} - {}",
            self.active_time_label(),
            if source_live { "live" } else { "archive" }
        );

        let narrow = !self.timeline_row1_fits(ui, &stamp_text, &age_text);

        ui.horizontal(|ui| {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let collapse = ui.button("\u{23f7}").on_hover_text("Collapse the timeline");
                #[cfg(test)]
                {
                    self.probes.last_timeline.collapse = collapse.rect;
                }
                if collapse.clicked() {
                    self.timeline_collapsed = true;
                }

                let expander = ui
                    .selectable_label(self.timeline_row2, "...")
                    .on_hover_text("Loop settings");
                #[cfg(test)]
                {
                    self.probes.last_timeline.expander = expander.rect;
                }
                if expander.clicked() {
                    self.timeline_row2 = !self.timeline_row2;
                }

                if !narrow {
                    ui.label(egui::RichText::new(age_text.as_str()).small().weak());
                }
                #[cfg(test)]
                {
                    self.probes.last_timeline.age_text =
                        if narrow { String::new() } else { age_text };
                }
                #[cfg(not(test))]
                let _ = age_text;

                let stamp = ui
                    .button(stamp_text.as_str())
                    .on_hover_text("Set the time to view");
                #[cfg(test)]
                {
                    self.probes.last_timeline.timestamp = (stamp.rect, stamp_text);
                }
                if stamp.clicked() {
                    self.time_dialog.show = true;
                }

                let nav_scope = egui::UiBuilder::new()
                    .id(ui.id().with("timeline_nav"))
                    .layout(egui::Layout::left_to_right(egui::Align::Center));
                ui.scope_builder(nav_scope, |ui| {
                    self.render_timeline_nav(ui, actions, !narrow);
                });
            });
        });

        if narrow {
            ui.horizontal(|ui| {
                self.render_timeline_scrubber_scope(ui, actions);
            });
        }
    }

    /// Whether row 1's one-row form fits `avail`: the essentials, the two
    /// trailing chips and a usable scrubber, measured from the real galleys
    /// at the real style — the top bar's own device (`roomy_run_width`), so
    /// no width constant can drift from the fonts. Deliberately generous on
    /// the spacing side: the tie flips to the two-row form, which degrades
    /// gracefully, where the one-row form overlaps.
    fn timeline_row1_fits(&self, ui: &egui::Ui, stamp_text: &str, age_text: &str) -> bool {
        let button_font = egui::TextStyle::Button.resolve(ui.style());
        let small_font = egui::TextStyle::Small.resolve(ui.style());
        let text = |font: &egui::FontId, s: &str| -> f32 {
            ui.painter()
                .layout_no_wrap(s.to_owned(), font.clone(), egui::Color32::PLACEHOLDER)
                .size()
                .x
        };
        let pad = 2.0 * ui.spacing().button_padding.x;
        let widths = [
            text(&button_font, "\u{23fa} Live") + pad,
            text(&button_font, "\u{23f4}") + pad,
            text(&button_font, "\u{23f5}") + pad,
            70.0 + pad, // the step combo's fixed width
            (text(&button_font, "\u{221e}") + pad).max(ui.spacing().interact_size.x),
            60.0, // the scrubber's minimum useful rail
            text(&button_font, stamp_text) + pad,
            text(&small_font, age_text),
            text(&button_font, "...") + pad,
            text(&button_font, "\u{23f7}") + pad,
        ];
        let needed =
            widths.iter().sum::<f32>() + ui.spacing().item_spacing.x * (widths.len() + 1) as f32;
        ui.available_width() >= needed
    }

    /// The scrubber, under one explicit host id whichever row hosts it —
    /// `UiBuilder::id` makes the scope's id independent of its parent, so
    /// the wide form (inline in the nav cluster) and the narrow form (its
    /// own row) key the slider identically and a mid-resize drag survives.
    fn render_timeline_scrubber_scope(&mut self, ui: &mut egui::Ui, actions: &mut Vec<GuiAction>) {
        let scope = egui::UiBuilder::new()
            .id(egui::Id::new("timeline_scrubber_host"))
            .layout(egui::Layout::left_to_right(egui::Align::Center));
        ui.scope_builder(scope, |ui| {
            self.render_timeline_scrubber(ui, actions);
        });
    }

    /// The navigation cluster: Live, back/forward, the step picker, the loop
    /// toggle and — in the roomy form — the scrubber.
    fn render_timeline_nav(
        &mut self,
        ui: &mut egui::Ui,
        actions: &mut Vec<GuiAction>,
        with_scrubber: bool,
    ) {
        let pane_idx = self.active_pane;
        let viewing_live = self.panes[pane_idx].viewing_live;

        let live_button = if viewing_live {
            egui::Button::new("\u{23fa} Live")
        } else {
            egui::Button::new(egui::RichText::new("\u{23fa} Live").color(egui::Color32::WHITE))
                .fill(egui::Color32::from_rgb(200, 50, 50))
        };
        let live = ui.add(live_button);
        #[cfg(test)]
        {
            self.probes.last_timeline.live = (live.rect, !viewing_live);
        }
        if live.clicked() && !viewing_live {
            actions.push(GuiAction::JumpToLive { pane_idx });
        }

        let step = self.panes[pane_idx].time.step;
        let back = ui.button("\u{23f4}").on_hover_text("Back one step");
        #[cfg(test)]
        {
            self.probes.last_timeline.back = back.rect;
        }
        if back.clicked() {
            self.panes[pane_idx].viewing_live = false;
            match step {
                TimeStep::OneFrame => actions.push(GuiAction::NavigateOneScan {
                    pane_idx,
                    forward: false,
                }),
                TimeStep::Secs(secs) => actions.push(GuiAction::NavigateTime {
                    pane_idx,
                    step_secs: -secs,
                }),
            }
        }

        let fwd = ui
            .add_enabled(!viewing_live, egui::Button::new("\u{23f5}"))
            .on_hover_text("Forward one step");
        #[cfg(test)]
        {
            self.probes.last_timeline.fwd = (fwd.rect, !viewing_live);
        }
        if fwd.clicked() {
            match step {
                TimeStep::OneFrame => actions.push(GuiAction::NavigateOneScan {
                    pane_idx,
                    forward: true,
                }),
                TimeStep::Secs(secs) => actions.push(GuiAction::NavigateTime {
                    pane_idx,
                    step_secs: secs,
                }),
            }
        }

        // A one-frame step needs a layer that has frames. A pane with none
        // still SEES the entry — disabled, with the reason on hover — because
        // an option that vanishes is an option the user cannot ask about.
        let offers_frames = self.pane_has_frame_series_layer(pane_idx);
        let step_label = TIME_STEP_OPTIONS
            .iter()
            .find(|(s, _)| *s == step)
            .map(|(_, l)| *l)
            .unwrap_or("10 min");
        let mut new_step = step;
        let combo = egui::ComboBox::from_id_salt("layers_time_step_sel")
            .selected_text(step_label)
            .width(70.0)
            .show_ui(ui, |ui| {
                for &(option, label) in TIME_STEP_OPTIONS {
                    if option == TimeStep::OneFrame && !offers_frames {
                        ui.add_enabled_ui(false, |ui| {
                            ui.selectable_value(&mut new_step, option, label)
                        })
                        .response
                        .on_hover_text(NO_FRAME_SERIES_REASON);
                        continue;
                    }
                    ui.selectable_value(&mut new_step, option, label);
                }
            });
        #[cfg(test)]
        {
            self.probes
                .widget_id_probes
                .push(("time_step_sel", combo.response.id));
            self.probes.last_timeline.step_dropdown = combo.response.rect;
        }
        #[cfg(not(test))]
        let _ = combo;
        if new_step != step {
            self.panes[pane_idx].time.step = new_step;
        }

        let can_loop = self.panes[pane_idx].can_loop();
        let loop_active = self.panes[pane_idx].loop_state().is_active();
        let loop_toggle = ui
            .add_enabled(
                can_loop,
                egui::Button::new("\u{221e}")
                    .selected(loop_active)
                    .min_size(ui.spacing().interact_size),
            )
            .on_hover_text("Radar loop");
        #[cfg(test)]
        {
            self.probes.last_timeline.loop_toggle = (loop_toggle.rect, loop_active);
        }
        if loop_toggle.clicked() {
            if loop_active {
                for pane_idx in self.loop_sync_targets() {
                    actions.push(GuiAction::DisableLoop { pane_idx });
                }
            } else {
                for pane_idx in self.loop_sync_targets() {
                    actions.push(GuiAction::EnableLoop {
                        pane_idx,
                        // The pane's own window, which carries the one number
                        // the file holds — see `Gui::set_loop_span_secs`.
                        lookback_secs: self.panes[pane_idx].time.span_secs,
                    });
                }
            }
        }

        if with_scrubber {
            self.render_timeline_scrubber_scope(ui, actions);
        }
    }

    /// The scrubber (plan §3.7) — one slider, two meanings.
    fn render_timeline_scrubber(&mut self, ui: &mut egui::Ui, actions: &mut Vec<GuiAction>) {
        let pane_idx = self.active_pane;

        ui.spacing_mut().slider_width =
            (ui.available_width() - ui.spacing().item_spacing.x).max(60.0);

        let loop_state = self.panes[pane_idx].loop_state();
        let loop_frames = loop_state
            .is_active()
            .then_some(loop_state.frames.len())
            .filter(|&total| total > 0);

        if let Some(total) = loop_frames {
            let seek = ui
                .push_id("scrub_loop", |ui| {
                    let mut frame_idx = self.panes[pane_idx].loop_state().current_frame();
                    let seek = ui
                        .add(egui::Slider::new(&mut frame_idx, 0..=(total - 1)).show_value(false));
                    if seek.changed() {
                        for pane_idx in self.loop_sync_targets() {
                            actions.push(GuiAction::SeekLoopFrame {
                                pane_idx,
                                frame_index: frame_idx,
                            });
                        }
                    }
                    seek
                })
                .inner;
            #[cfg(test)]
            {
                self.probes
                    .widget_id_probes
                    .push(("timeline_scrubber_loop", seek.id));
                self.probes.last_timeline.scrubber = seek.rect;
            }
            #[cfg(not(test))]
            let _ = seek;
            return;
        }

        let lookback_secs = self.panes[pane_idx].time.span_secs.max(1) as f32;
        let resting = if self.panes[pane_idx].viewing_live {
            1.0
        } else {
            match self.panes[pane_idx].data_time_on_screen() {
                Some(t) => {
                    let age = (chrono::Utc::now().naive_utc() - t).num_seconds() as f32;
                    (1.0 - age / lookback_secs).clamp(0.0, 1.0)
                }
                None => 1.0,
            }
        };
        let mut frac = self.timeline_scrub.unwrap_or(resting);
        let scrub = ui
            .push_id("scrub_archive", |ui| {
                ui.add(egui::Slider::new(&mut frac, 0.0..=1.0).show_value(false))
            })
            .inner;
        #[cfg(test)]
        {
            self.probes
                .widget_id_probes
                .push(("timeline_scrubber", scrub.id));
            self.probes.last_timeline.scrubber = scrub.rect;
        }
        if scrub.drag_stopped() {
            self.timeline_scrub = None;
            self.commit_archive_scrub(frac, lookback_secs, actions);
        } else if scrub.dragged() {
            self.timeline_scrub = Some(frac);
        } else if scrub.changed() {
            self.timeline_scrub = None;
            self.commit_archive_scrub(frac, lookback_secs, actions);
        } else {
            self.timeline_scrub = None;
        }
    }

    /// Commit a scrub position: the right end means live, anywhere else means
    /// the archive moment that fraction of the lookback window names. One
    /// function for the release and the keyboard nudge, so the two routes
    /// cannot drift.
    fn commit_archive_scrub(
        &mut self,
        frac: f32,
        lookback_secs: f32,
        actions: &mut Vec<GuiAction>,
    ) {
        let pane_idx = self.active_pane;
        if frac >= SCRUB_LIVE_THRESHOLD {
            actions.push(GuiAction::JumpToLive { pane_idx });
        } else if let Some(scan_time) = self.panes[pane_idx]
            .scan_info
            .as_ref()
            .map(|info| info.timestamp)
        {
            let now = chrono::Utc::now().naive_utc();
            let target = now - chrono::Duration::seconds((lookback_secs * (1.0 - frac)) as i64);
            let step_secs = (target - scan_time).num_seconds();
            self.panes[pane_idx].viewing_live = false;
            // The release names an INSTANT, so say so: the pane's clock moves
            // to it and every layer on the pane is shown at that moment. The
            // scan fetch below is radar's half of the same answer.
            self.panes[pane_idx].set_time_mode(TimeMode::AsOf(target));
            actions.push(GuiAction::NavigateTime {
                pane_idx,
                step_secs,
            });
        }
    }

    /// Row 2: the loop tuning, shown behind `⋯`.
    fn render_timeline_row2(&mut self, ui: &mut egui::Ui, actions: &mut Vec<GuiAction>) {
        let pane_idx = self.active_pane;
        let loop_active = self.panes[pane_idx].loop_state().is_active();
        #[cfg(test)]
        let mut row2 = TimelineRow2Probe::default();

        ui.separator();
        ui.horizontal(|ui| {
            ui.spacing_mut().slider_width = TUNING_SLIDER_WIDTH;

            let mut lookback_mins = (self.loop_lookback_secs as f32 / 60.0).round();
            ui.label("Lookback:");
            let lookback = ui.add(
                egui::Slider::new(&mut lookback_mins, 5.0..=1440.0)
                    .logarithmic(true)
                    .suffix(" min")
                    .clamping(egui::SliderClamping::Always),
            );
            #[cfg(test)]
            {
                row2.lookback = lookback.rect;
            }
            if lookback.drag_stopped() {
                let new_secs = (lookback_mins * 60.0) as u64;
                if new_secs != self.loop_lookback_secs {
                    self.set_loop_span_secs(new_secs);
                    if loop_active {
                        for pane_idx in self.loop_sync_targets() {
                            actions.push(GuiAction::EnableLoop {
                                pane_idx,
                                lookback_secs: self.panes[pane_idx].time.span_secs,
                            });
                        }
                    }
                }
            }

            ui.label("Speed:");
            let mut fps = self.loop_speed_fps;
            let speed = ui.add(
                egui::Slider::new(&mut fps, 1.0..=30.0)
                    .suffix(" fps")
                    .clamping(egui::SliderClamping::Always),
            );
            if fps != self.loop_speed_fps {
                self.set_loop_speed_fps(fps);
            }
            #[cfg(test)]
            {
                row2.speed = speed.rect;
            }
            #[cfg(not(test))]
            let _ = speed;
        });
        ui.label(egui::RichText::new(TUNING_SCOPE_CAPTION).small().weak());
        #[cfg(test)]
        {
            row2.tuning_scope = TUNING_SCOPE_CAPTION.to_owned();
        }

        if loop_active {
            let ls = self.panes[pane_idx].loop_state();
            let rendered = ls.frames.iter().filter(|f| f.image.is_some()).count();
            let total = ls.frames.len();
            let rendering = total > 0 && !ls.is_render_ready();
            let playing = ls.is_playing();
            let fetching = ls.is_fetching();
            let current_frame = ls.current_frame();
            let frame_time = ls.frames.get(current_frame).map(|f| f.timestamp);

            if fetching {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("Loading scan list...");
                });
            } else if total == 0 {
                ui.label("No frames found");
            } else {
                ui.horizontal(|ui| {
                    let prev = ui.button("\u{23ee}").on_hover_text("Previous frame");
                    #[cfg(test)]
                    {
                        row2.prev = prev.rect;
                    }
                    if prev.clicked() {
                        for pane_idx in self.loop_sync_targets() {
                            actions.push(GuiAction::StepLoopFrame {
                                pane_idx,
                                forward: false,
                            });
                        }
                    }

                    let play_label = if playing { "\u{23f8}" } else { "\u{23f5}" };
                    let play_hover = if playing {
                        "Pause".to_owned()
                    } else if rendering {
                        format!("Waiting for renders ({rendered}/{total})")
                    } else {
                        "Play".to_owned()
                    };
                    let play = ui
                        .add_enabled(!rendering || playing, egui::Button::new(play_label))
                        .on_hover_text(play_hover);
                    #[cfg(test)]
                    {
                        row2.play = play.rect;
                    }
                    if play.clicked() {
                        for pane_idx in self.loop_sync_targets() {
                            actions.push(GuiAction::ToggleLoopPlayback { pane_idx });
                        }
                    }

                    let next = ui.button("\u{23ed}").on_hover_text("Next frame");
                    #[cfg(test)]
                    {
                        row2.next = next.rect;
                    }
                    if next.clicked() {
                        for pane_idx in self.loop_sync_targets() {
                            actions.push(GuiAction::StepLoopFrame {
                                pane_idx,
                                forward: true,
                            });
                        }
                    }

                    ui.spacing_mut().slider_width = (ui.available_width() * 0.5).clamp(60.0, 240.0);
                    let mut frame_idx = current_frame;
                    let seek = ui
                        .add(egui::Slider::new(&mut frame_idx, 0..=(total - 1)).show_value(false));
                    #[cfg(test)]
                    {
                        row2.seek = seek.rect;
                    }
                    if seek.changed() {
                        for pane_idx in self.loop_sync_targets() {
                            actions.push(GuiAction::SeekLoopFrame {
                                pane_idx,
                                frame_index: frame_idx,
                            });
                        }
                    }

                    if let Some(timestamp) = frame_time {
                        let text = self
                            .preferences
                            .timezone
                            .format_naive_utc(timestamp, "%H:%M:%S");
                        ui.label(egui::RichText::new(text.as_str()).small());
                        #[cfg(test)]
                        {
                            row2.frame_text = text;
                        }
                        #[cfg(not(test))]
                        let _ = text;
                    }
                });

                if rendering {
                    let text = format!("Rendering {rendered}/{total}...");
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(text.as_str());
                    });
                    ui.add(
                        egui::ProgressBar::new(rendered as f32 / total as f32).show_percentage(),
                    );
                    #[cfg(test)]
                    {
                        row2.rendered_text = text;
                    }
                    #[cfg(not(test))]
                    let _ = text;
                } else {
                    let text = format!("{rendered}/{total} frames rendered");
                    ui.label(text.as_str());
                    #[cfg(test)]
                    {
                        row2.rendered_text = text;
                    }
                    #[cfg(not(test))]
                    let _ = text;
                }
            }
        }

        // **The caption describes the layer the pane's clock walks** — its
        // time-primary layer — rather than radar by name. On every pane in
        // this build that IS radar (weight 30, above the model's 10), so the
        // four inputs are the four it always read; what changes is that a
        // pane animating something else describes that instead of describing
        // an empty radar timeline. `markedly_longer` and the `NOTICEABLE_*`
        // rules below it are untouched: that decision was tuned against KPBZ
        // and travels whole.
        let span = self.panes[pane_idx]
            .clock_layer()
            .cloned()
            .map(|id| self.panes[pane_idx].time_state(&id))
            .and_then(|ls| {
                loop_span_phrase(
                    &ls.frames,
                    ls.sampled,
                    ls.cadence_secs,
                    ls.is_render_ready(),
                )
            });
        let budget = format!(
            "Loops keep up to {} frames on this platform - a pane with \
             \"Sync time\" off sits out the loop and shared navigation",
            self.loop_frame_budget
        );
        let caption = match span {
            Some(span) => format!("{span} - {budget}"),
            None => budget,
        };
        ui.label(egui::RichText::new(caption.as_str()).small().weak());
        #[cfg(test)]
        {
            row2.caption = caption;
        }
        #[cfg(not(test))]
        let _ = caption;

        #[cfg(test)]
        {
            self.probes.last_timeline.row2 = Some(row2);
        }
    }
}

#[path = "ui_timeline/tests.rs"]
#[cfg(test)]
mod tests;
