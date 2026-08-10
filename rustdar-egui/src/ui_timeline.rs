//! The floating timeline transport: time navigation and the radar loop, in
//! one surface floating over the map's bottom edge.
//!
//! This is where the layers panel's time-navigation and loop-control blocks
//! went. The semantics moved whole — the same actions, the same fan-out over
//! [`Gui::loop_sync_targets`](super::Gui), the same red not-live styling — but
//! the host changed twice over: the controls float over a full-bleed map
//! instead of docking beside it, and they act on the **active pane read live
//! out of `self.panes`** instead of on a pane a panel pass held out with
//! `mem::take`. The second half is why [`Gui::ui`](super::Gui::ui) calls
//! [`Gui::render_timeline`](super::Gui) after `render_panes` and the pending
//! appliers: every take window in the frame has closed, so
//! `self.panes[self.active_pane]` is the real pane and direct writes stick.
//!
//! Row 1 is always on while the transport is expanded: Live · back / forward ·
//! step picker · loop toggle · scrubber · timestamp (opens the Set Time
//! dialog) · age chip · `⋯` (row 2) · `▾` (collapse). Row 2 adds the loop
//! tuning: lookback, speed, the frame transport, the seek slider, the render
//! progress, and a closing caption stating this platform's frame budget and
//! the per-pane unlink hint. Collapsed, the whole transport becomes a small
//! 🕐 chip at the map's bottom-right corner; clicking it restores.
//!
//! # Ids do not depend on the width — or on the data
//!
//! The transport is one `egui::Area` under one constant id at every
//! [`WidthClass`](crate::ui_layout::WidthClass), so nothing about it re-keys
//! at a breakpoint. Two further rules keep the ids still while the *content*
//! moves. The trailing chips (timestamp, age) are drawn unconditionally —
//! placeholder text rather than absence — so a scan or a render landing
//! cannot change the row's widget count. And the left-hand run sits in a
//! scope with an explicit [`egui::UiBuilder::id`], which takes the trailing
//! run's auto-id counter out of every id inside it — the same device, for the
//! same reason, as the status bar's `status_error` scope. The step combo keeps
//! the `layers_time_step_sel` salt it had in the layers panel: moving hosts
//! re-keyed it once, but the salt keeps it independent of everything that
//! renders around it.

use crate::actions::GuiAction;

/// Available time step options: (seconds, label). 0 = "one scan". Moved here
/// from the layers panel with the navigation buttons that consume it.
pub(super) const TIME_STEP_OPTIONS: &[(i64, &str)] = &[
    (0, "1 scan"),
    (600, "10 min"),
    (1800, "30 min"),
    (3600, "1 hr"),
    (7200, "2 hr"),
    (21600, "6 hr"),
    (43200, "12 hr"),
];

/// How far above the map's bottom edge the transport floats (plan §1.5) —
/// clear of the status bar spanning the bottom inset below it.
const BOTTOM_CLEARANCE: f32 = 44.0;

/// The transport's widest inner form.
const MAX_INNER_WIDTH: f32 = 880.0;

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
    /// The back (◀) button.
    pub back: egui::Rect,
    /// The forward (▶) button, and whether it was enabled.
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
    /// The `⋯` row-2 expander.
    pub expander: egui::Rect,
    /// The `▾` collapse button.
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

/// Row 2 of the probe: the loop tuning as drawn. The transport rects are
/// [`egui::Rect::NOTHING`] and the texts empty while no loop is active — the
/// row draws its tuning sliders unconditionally and its frame transport only
/// for a loop that exists, exactly as the layers panel's block did.
#[cfg(test)]
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct TimelineRow2Probe {
    pub lookback: egui::Rect,
    pub speed: egui::Rect,
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
    ///
    /// Runs from [`Gui::ui`](super::Gui::ui) **after** the pane loop and the
    /// pending appliers, so no `mem::take` window is open and the active pane
    /// read out of `self.panes` is the real one — the module note explains why
    /// that ordering is the whole design.
    ///
    /// `phone_bar_top` is `Some` on Compact: the top edge of the bottom bar
    /// the phone shell drew this frame. The transport then presents inline —
    /// full inset width, sitting directly above the bar (plan §1.5) — and the
    /// collapsed chip right-aligns above the bar instead of hugging the map's
    /// corner. Same `Area` id either way: only the geometry is the phone's.
    pub(super) fn render_timeline(
        &mut self,
        ctx: &egui::Context,
        map_rect: egui::Rect,
        phone_bar_top: Option<f32>,
        actions: &mut Vec<GuiAction>,
    ) {
        #[cfg(test)]
        {
            self.last_timeline = TimelineProbe::default();
        }

        if self.timeline_collapsed {
            self.render_timeline_chip(ctx, map_rect, phone_bar_top);
            return;
        }

        let (anchor_bottom, inner_width) = match phone_bar_top {
            Some(bar_top) => (bar_top - CHIP_INSET, map_rect.width() - 2.0 * CHIP_INSET),
            None => (
                map_rect.bottom() - BOTTOM_CLEARANCE,
                (map_rect.width() - SIDE_INSET).min(MAX_INNER_WIDTH),
            ),
        };
        let area = egui::Area::new(egui::Id::new("timeline"))
            .order(egui::Order::Middle)
            .pivot(egui::Align2::CENTER_BOTTOM)
            .fixed_pos(egui::pos2(map_rect.center().x, anchor_bottom))
            .show(ctx, |ui| {
                egui::Frame::window(&ctx.global_style()).show(ui, |ui| {
                    ui.set_width(inner_width);
                    self.render_timeline_row1(ui, actions);
                    if self.timeline_row2 {
                        self.render_timeline_row2(ui, actions);
                    }
                });
            });

        #[cfg(test)]
        {
            self.last_timeline.rect = area.response.rect;
        }
        #[cfg(not(test))]
        let _ = area;
    }

    /// The collapsed form: a 🕐-and-timestamp chip at the map's bottom-right
    /// — above the bottom bar on the phone, whose Live chip is the other
    /// restore route (plan §1.5).
    ///
    /// Bottom-**right** while the transport itself is bottom-centred, so the
    /// chip does not sit where the middle of the map's bottom edge is most
    /// likely to be looked at — the whole point of collapsing.
    fn render_timeline_chip(
        &mut self,
        ctx: &egui::Context,
        map_rect: egui::Rect,
        phone_bar_top: Option<f32>,
    ) {
        let bottom = phone_bar_top.map_or(map_rect.bottom(), |bar_top| bar_top);
        let area = egui::Area::new(egui::Id::new("timeline_chip"))
            .order(egui::Order::Middle)
            .pivot(egui::Align2::RIGHT_BOTTOM)
            .fixed_pos(egui::pos2(map_rect.right() - CHIP_INSET, bottom - CHIP_INSET))
            .show(ctx, |ui| {
                egui::Frame::window(&ctx.global_style()).show(ui, |ui| {
                    let chip = ui.button(format!("\u{1f550} {}", self.active_time_label()));
                    if chip.clicked() {
                        self.timeline_collapsed = false;
                    }
                });
            });

        #[cfg(test)]
        {
            self.last_timeline.collapsed = true;
            self.last_timeline.chip = area.response.rect;
        }
        #[cfg(not(test))]
        let _ = area;
    }

    /// The active pane's on-screen data time, as the timestamp chip, the
    /// collapsed chip and the bottom bar's Live chip all print it. One
    /// function so the three cannot drift.
    pub(super) fn active_time_label(&self) -> String {
        match self.panes[self.active_pane].data_time_on_screen() {
            Some(t) => self.preferences.timezone.format_naive_utc(t, "%H:%M:%S"),
            None => "--:--:--".to_owned(),
        }
    }

    /// Row 1: the always-on transport.
    ///
    /// Laid out like the top bar: the trailing chips claim the right edge in a
    /// right-to-left run first, then the navigation cluster takes what is left
    /// and hands every spare point to the scrubber. The left-hand run's scope
    /// carries an explicit id — see the module note on why.
    fn render_timeline_row1(&mut self, ui: &mut egui::Ui, actions: &mut Vec<GuiAction>) {
        let pane_idx = self.active_pane;
        ui.horizontal(|ui| {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                // First added is rightmost: ▾ at the edge, then ⋯, the age
                // chip and the timestamp, reading left-to-right as
                // timestamp · age · ⋯ · ▾.
                let collapse = ui.button("\u{25be}").on_hover_text("Collapse the timeline");
                #[cfg(test)]
                {
                    self.last_timeline.collapse = collapse.rect;
                }
                if collapse.clicked() {
                    self.timeline_collapsed = true;
                }

                let expander = ui
                    .selectable_label(self.timeline_row2, "\u{22ef}")
                    .on_hover_text("Loop settings");
                #[cfg(test)]
                {
                    self.last_timeline.expander = expander.rect;
                }
                if expander.clicked() {
                    self.timeline_row2 = !self.timeline_row2;
                }

                // The age chip and the timestamp are drawn even when there is
                // nothing to say — placeholder text, not absence — so data
                // arriving cannot change this run's widget count and re-key
                // everything drawn after it (see the module note).
                let age_text = self.panes[pane_idx]
                    .data_time_on_screen()
                    .map(|collected| {
                        super::statusbar::format_product_age(
                            chrono::Utc::now().naive_utc() - collected,
                        )
                    })
                    .unwrap_or_default();
                ui.label(egui::RichText::new(age_text.as_str()).small().weak());
                #[cfg(test)]
                {
                    self.last_timeline.age_text = age_text;
                }
                #[cfg(not(test))]
                let _ = age_text;

                let viewing_live = self.panes[pane_idx].viewing_live;
                let stamp_text = format!(
                    "{} \u{b7} {}",
                    self.active_time_label(),
                    if viewing_live { "live" } else { "archive" }
                );
                let stamp = ui
                    .button(stamp_text.as_str())
                    .on_hover_text("Set the time to view");
                #[cfg(test)]
                {
                    self.last_timeline.timestamp = (stamp.rect, stamp_text);
                }
                if stamp.clicked() {
                    // The same flag the menu's Time... entry raises; the
                    // dialog itself is unchanged.
                    self.time_dialog.show = true;
                }

                // The navigation cluster, reading left-to-right again, under
                // an explicit id so the chips above cannot re-key it.
                let nav_scope = egui::UiBuilder::new()
                    .id(ui.id().with("timeline_nav"))
                    .layout(egui::Layout::left_to_right(egui::Align::Center));
                ui.scope_builder(nav_scope, |ui| {
                    self.render_timeline_nav(ui, actions);
                });
            });
        });
    }

    /// The navigation cluster: Live, back/forward, the step picker, the loop
    /// toggle and the scrubber.
    fn render_timeline_nav(&mut self, ui: &mut egui::Ui, actions: &mut Vec<GuiAction>) {
        let pane_idx = self.active_pane;
        let viewing_live = self.panes[pane_idx].viewing_live;

        // Live button — highlighted red when NOT live to indicate "click to
        // return", exactly the styling the layers panel used.
        let live_button = if viewing_live {
            egui::Button::new("\u{23fa} Live")
        } else {
            egui::Button::new(egui::RichText::new("\u{23fa} Live").color(egui::Color32::WHITE))
                .fill(egui::Color32::from_rgb(200, 50, 50))
        };
        let live = ui.add(live_button);
        #[cfg(test)]
        {
            self.last_timeline.live = (live.rect, !viewing_live);
        }
        if live.clicked() && !viewing_live {
            actions.push(GuiAction::JumpToLive { pane_idx });
        }

        // Back: drop out of live and step backwards by the step picker's
        // choice — one scan, or a fixed span.
        let step_secs = self.panes[pane_idx].time_step_secs;
        let back = ui.button("\u{25c0}").on_hover_text("Back one step");
        #[cfg(test)]
        {
            self.last_timeline.back = back.rect;
        }
        if back.clicked() {
            self.panes[pane_idx].viewing_live = false;
            if step_secs == 0 {
                actions.push(GuiAction::NavigateOneScan {
                    pane_idx,
                    forward: false,
                });
            } else {
                actions.push(GuiAction::NavigateTime {
                    pane_idx,
                    step_secs: -step_secs,
                });
            }
        }

        // Forward — disabled while live, since there is nothing ahead of now.
        let fwd = ui
            .add_enabled(!viewing_live, egui::Button::new("\u{25b6}"))
            .on_hover_text("Forward one step");
        #[cfg(test)]
        {
            self.last_timeline.fwd = (fwd.rect, !viewing_live);
        }
        if fwd.clicked() {
            if step_secs == 0 {
                actions.push(GuiAction::NavigateOneScan {
                    pane_idx,
                    forward: true,
                });
            } else {
                actions.push(GuiAction::NavigateTime {
                    pane_idx,
                    step_secs,
                });
            }
        }

        // The step picker. The salt is the one it had in the layers panel —
        // `layers_time_step_sel` — kept so the stored combo state survived the
        // move once and stays put hereafter; see the module note.
        let step_label = TIME_STEP_OPTIONS
            .iter()
            .find(|(s, _)| *s == step_secs)
            .map(|(_, l)| *l)
            .unwrap_or("10 min");
        let mut new_step = step_secs;
        let combo = egui::ComboBox::from_id_salt("layers_time_step_sel")
            .selected_text(step_label)
            .width(70.0)
            .show_ui(ui, |ui| {
                for &(secs, label) in TIME_STEP_OPTIONS {
                    ui.selectable_value(&mut new_step, secs, label);
                }
            });
        // Report the id the combo box really resolved, rather than building a
        // second one from the same salt: the two could disagree silently, and
        // a test comparing reconstructions either side of a resize would then
        // prove nothing about the state egui actually keyed on.
        #[cfg(test)]
        {
            self.widget_id_probes.push(("time_step_sel", combo.response.id));
            self.last_timeline.step_dropdown = combo.response.rect;
        }
        #[cfg(not(test))]
        let _ = combo;
        if new_step != step_secs {
            self.panes[pane_idx].time_step_secs = new_step;
        }

        // The loop toggle. Enabled for map panes only: a loop frame is a
        // rendered plan-view tilt and nothing feeds one to a section or a
        // volume pane, so enabling it there would wait for ever — the layers
        // panel expressed the same rule by omitting the whole block. Read off
        // the real pane, which this renderer can do and the panel could not:
        // no take window is open here.
        let is_map = self.panes[pane_idx].is_map();
        let loop_active = self.panes[pane_idx].loop_state.is_active();
        let loop_toggle = ui
            .add_enabled(is_map, egui::Button::selectable(loop_active, "\u{1f501}"))
            .on_hover_text("Radar loop");
        #[cfg(test)]
        {
            self.last_timeline.loop_toggle = (loop_toggle.rect, loop_active);
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
                        lookback_secs: self.loop_lookback_secs,
                    });
                }
            }
        }

        self.render_timeline_scrubber(ui, actions);
    }

    /// The scrubber (plan §3.7) — one slider, two meanings.
    ///
    /// **While a loop is active** it is a live frame-seek: dragging emits
    /// [`GuiAction::SeekLoopFrame`] per change, mirroring row 2's seek slider,
    /// because the frames are already fetched and seeking is free.
    ///
    /// **With no loop** it spans the archive window `[now − lookback, now]`
    /// and commits **only on release** (`drag_stopped`): a release inside the
    /// rail drops out of live and emits [`GuiAction::NavigateTime`] to the
    /// released moment; a release at the right end ([`SCRUB_LIVE_THRESHOLD`])
    /// emits [`GuiAction::JumpToLive`] instead. That honours the design's
    /// scrub-drops-live / scrub-to-end-restores-live without fetching a volume
    /// per drag frame — every intermediate position is a fetch nobody asked
    /// to wait for.
    fn render_timeline_scrubber(&mut self, ui: &mut egui::Ui, actions: &mut Vec<GuiAction>) {
        let pane_idx = self.active_pane;

        // Everything to the left has been laid out; the scrubber takes the
        // rest of the row.
        ui.spacing_mut().slider_width =
            (ui.available_width() - ui.spacing().item_spacing.x).max(60.0);

        let loop_state = &self.panes[pane_idx].loop_state;
        let loop_frames = loop_state
            .is_active()
            .then_some(loop_state.frames.len())
            .filter(|&total| total > 0);

        if let Some(total) = loop_frames {
            // Loop form: seek frames live.
            let mut frame_idx = self.panes[pane_idx].loop_state.current_frame;
            let seek = ui.add(egui::Slider::new(&mut frame_idx, 0..=(total - 1)).show_value(false));
            #[cfg(test)]
            {
                self.last_timeline.scrubber = seek.rect;
            }
            if seek.changed() {
                for pane_idx in self.loop_sync_targets() {
                    actions.push(GuiAction::SeekLoopFrame {
                        pane_idx,
                        frame_index: frame_idx,
                    });
                }
            }
            return;
        }

        // Archive form. The resting position restates where the pane is
        // looking: pinned right while live, else the on-screen data time's
        // place in the lookback window. While a drag is in flight the
        // position is the drag's own, remembered across frames in
        // `timeline_scrub` so the handle follows the pointer instead of
        // snapping home every frame.
        let lookback_secs = self.loop_lookback_secs.max(1) as f32;
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
        let scrub = ui.add(egui::Slider::new(&mut frac, 0.0..=1.0).show_value(false));
        #[cfg(test)]
        {
            // Reported like `time_step_sel`, so the keyboard test can put
            // real focus behind the id egui actually keyed the slider on.
            self.widget_id_probes.push(("timeline_scrubber", scrub.id));
            self.last_timeline.scrubber = scrub.rect;
        }
        if scrub.drag_stopped() {
            // A release commits once — checked first, because the release
            // frame can report `changed` too and must not commit twice.
            self.timeline_scrub = None;
            self.commit_archive_scrub(frac, lookback_secs, actions);
        } else if scrub.dragged() {
            self.timeline_scrub = Some(frac);
        } else if scrub.changed() {
            // Changed with no drag in flight: a keyboard nudge on the
            // focused slider (§5.9 carried finding — this used to store the
            // position and wait for a release that never comes). There is
            // nothing to wait out, so it commits now, exactly as the loop
            // form's seek does.
            self.timeline_scrub = None;
            self.commit_archive_scrub(frac, lookback_secs, actions);
        } else {
            // No drag this frame and no release to commit: whatever position
            // was remembered belongs to a gesture that ended without a
            // release — a cancelled touch reports no `drag_stopped`, ever —
            // and holding it would pin the handle to a drag that no longer
            // exists. Dropping it uncommitted is the cancel behaving like a
            // cancel.
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
            // `NavigateTime` steps relative to the pane's scan time, so
            // the committed absolute moment becomes a step from there.
            let now = chrono::Utc::now().naive_utc();
            let target = now - chrono::Duration::seconds((lookback_secs * (1.0 - frac)) as i64);
            let step_secs = (target - scan_time).num_seconds();
            self.panes[pane_idx].viewing_live = false;
            actions.push(GuiAction::NavigateTime {
                pane_idx,
                step_secs,
            });
        }
    }

    /// Row 2: the loop tuning, shown behind `⋯`.
    ///
    /// The tuning sliders draw unconditionally; the frame transport, the seek
    /// slider and the progress read exist only for a loop that exists —
    /// the same states, spinner for spinner, that the layers panel's block
    /// drew.
    fn render_timeline_row2(&mut self, ui: &mut egui::Ui, actions: &mut Vec<GuiAction>) {
        let pane_idx = self.active_pane;
        let loop_active = self.panes[pane_idx].loop_state.is_active();
        #[cfg(test)]
        let mut row2 = TimelineRow2Probe::default();

        ui.separator();
        ui.horizontal(|ui| {
            ui.spacing_mut().slider_width = TUNING_SLIDER_WIDTH;

            // Lookback duration slider. Committed on release, not per drag
            // frame: each commit re-fetches the loop's whole scan list.
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
                    self.loop_lookback_secs = new_secs;
                    // Re-issue the loop at the new depth — only where a loop
                    // is already running; tuning the dial must not start one.
                    if loop_active {
                        for pane_idx in self.loop_sync_targets() {
                            actions.push(GuiAction::EnableLoop {
                                pane_idx,
                                lookback_secs: new_secs,
                            });
                        }
                    }
                }
            }

            // Speed slider.
            ui.label("Speed:");
            let speed = ui.add(
                egui::Slider::new(&mut self.loop_speed_fps, 1.0..=30.0)
                    .suffix(" fps")
                    .clamping(egui::SliderClamping::Always),
            );
            #[cfg(test)]
            {
                row2.speed = speed.rect;
            }
            #[cfg(not(test))]
            let _ = speed;
        });

        if loop_active {
            let ls = &self.panes[pane_idx].loop_state;
            let rendered = ls.frames.iter().filter(|f| f.texture.is_some()).count();
            let total = ls.frames.len();
            let rendering = total > 0 && !ls.is_render_ready();
            let playing = ls.is_playing();
            let fetching = ls.is_fetching();
            let current_frame = ls.current_frame;
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
                    // Step backward
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

                    // Play/pause
                    let play_label = if playing { "\u{23f8}" } else { "\u{25b6}" };
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

                    // Step forward
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

                    // Frame seek slider — the row-1 scrubber mirrors this
                    // while the loop runs.
                    ui.spacing_mut().slider_width =
                        (ui.available_width() * 0.5).clamp(60.0, 240.0);
                    let mut frame_idx = current_frame;
                    let seek =
                        ui.add(egui::Slider::new(&mut frame_idx, 0..=(total - 1)).show_value(false));
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

                    // Current frame timestamp
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

                // Progress bar while rendering, plain text when done.
                if rendering {
                    let text = format!("Rendering {rendered}/{total}...");
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(text.as_str());
                    });
                    ui.add(egui::ProgressBar::new(rendered as f32 / total as f32).show_percentage());
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

        // The closing caption (plan §1.5): what this platform's loops can
        // hold, and the escape hatch from shared time. The budget is the
        // running build's own, pushed in by the frontend
        // (`set_loop_frame_budget`) — not a guess from the width, which a
        // 1400 pt Android tablet would get wrong. "Sits out", not "stays
        // frozen": scan delivery is site-keyed and ignores the link, so a
        // live unlinked pane still follows new scans — the checkbox's own
        // hover (`ui_pills::UNLINK_NOTE`) spells the full claim out.
        let caption = format!(
            "Loops keep up to {} frames on this platform \u{b7} a pane with \
             \u{201c}Follows shared time\u{201d} off sits out the loop and \
             shared navigation",
            self.loop_frame_budget
        );
        ui.label(egui::RichText::new(caption.as_str()).small().weak());
        #[cfg(test)]
        {
            row2.caption = caption;
        }
        #[cfg(not(test))]
        let _ = caption;

        #[cfg(test)]
        {
            self.last_timeline.row2 = Some(row2);
        }
    }
}
