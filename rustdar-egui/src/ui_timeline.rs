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

/// **Where `now` sits on the archive rail, as a fraction of its travel, on a
/// pane whose transport layer reaches past the wall clock.** Chosen by the
/// user from measured options; `1.0` on every pane with no forecast timeline,
/// which is what makes a radar-only rail the one it has always been.
///
/// **A constant, and each side carries its own seconds per pixel.** One
/// linear axis over `span_secs + horizon` hands the past `P/(P+F)` of the
/// travel: 5.26% of it at an 18 h horizon and 2.04% at 48 h, which is 3.45
/// and 8.92 minutes per pixel against radar volumes 259-517 s apart. Archive
/// scrubbing would stop existing the moment a model layer was switched on,
/// with no user action on the scrubber at all. A fixed split costs the past
/// exactly `1/NOW_SPLIT` = 1.4286x whatever the horizon is.
///
/// **The two colours are that scale break made visible.** A rail painted one
/// colour with a hidden change of scale inside it would be a lie; the colour
/// break is where the seconds-per-pixel changes, so the landmark the reader
/// steers by and the thing they must know are the same mark.
pub(crate) const NOW_SPLIT: f32 = 0.70;

/// How far the forecast region's fill is carried from the past region's
/// toward the window's own background, in [`egui::Color32::lerp_to_gamma`].
/// The one knob over both themes: light moves 230 -> 239 and dark 60 -> 43,
/// each toward its own `window_fill`, so the future is the half that recedes
/// into the page either way.
const FUTURE_TOWARD_WINDOW: f32 = 0.5;

/// **How near the `now` boundary a release has to land to mean "back to
/// live"**, in points — a distance, where the rule it replaces was a fraction
/// (`0.99`) of a rail that is 477 px at any window 904 px or wider and 77 px
/// at a 480 px one. That is 4.8 px of live zone on the wide rail and 0.8 px on
/// the narrow one, which is not reachable with a mouse.
const LIVE_SNAP_PX: f32 = 6.0;

/// **The ceiling on [`LIVE_SNAP_PX`], as a share of the forecast region.**
///
/// A distance that is 1.26% of a 477 px rail is 7.8% of a 77 px one, and
/// 25.9% of that rail's 23.15 px forecast half: a quarter of the region a
/// user is aiming at would answer "live" instead. On a rail with a forecast
/// half the snap zone is capped here, so the zone can eat a tenth of the
/// forecast region and never more. On a rail without one there is no forecast
/// region to protect and the plain distance stands.
const LIVE_SNAP_MAX_FUTURE_SHARE: f32 = 0.10;

/// Which side of `now` an instant falls on, and so which region of the
/// archive rail carries it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RailSide {
    /// At or before the wall clock. **The tie is here**, not in `Future`.
    Past,
    /// Later than the wall clock.
    Future,
}

/// **The archive rail's two regions, and the scale each of them carries.**
///
/// One bar and one drag surface: this is not two rails, it is the mapping
/// between a fraction of one rail's travel and an instant. `split` is the
/// fraction at which `now` sits — [`NOW_SPLIT`] on a pane whose transport
/// layer reaches past the wall clock, `1.0` on every other pane, and at `1.0`
/// every expression below reduces to the single linear past rail the
/// scrubber has always been.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct RailRegions {
    /// The fraction of the travel at which `now` sits.
    pub split: f32,
    /// How much history the past region covers, in seconds. Always positive.
    pub past_secs: f32,
    /// How far forward the forecast region reaches, in seconds; `0.0` when
    /// there is no forecast region at all.
    pub future_secs: f32,
    /// The transport layer's own frame step, in seconds — the grid a release
    /// in the forecast region is quantised onto. `0` when the layer declares
    /// no step, and then nothing is quantised.
    pub step_secs: i64,
}

impl RailRegions {
    /// A rail with no forecast region: the whole travel is `past_secs` of
    /// history, which is the rail every pane had before WI-11.
    pub(crate) fn past_only(past_secs: f32) -> Self {
        Self {
            split: 1.0,
            past_secs: past_secs.max(1.0),
            future_secs: 0.0,
            step_secs: 0,
        }
    }

    /// Whether this rail has a forecast region at all — the one condition
    /// under which anything about the scrubber differs from what it was.
    pub(crate) fn has_future(&self) -> bool {
        self.split < 1.0 && self.future_secs > 0.0
    }

    /// **Which region carries `valid`.** The tie goes to the past: `valid ==
    /// now` is history, the same `<=` the frame lookup uses, so a forecast
    /// frame crosses the boundary at the instant its valid time arrives and
    /// is never briefly in neither region.
    pub(crate) fn side_of(
        &self,
        valid: chrono::NaiveDateTime,
        now: chrono::NaiveDateTime,
    ) -> RailSide {
        if valid <= now {
            RailSide::Past
        } else {
            RailSide::Future
        }
    }

    /// **The instant a release at `frac` names, as seconds either side of
    /// `now`** — negative into history, positive into the forecast.
    ///
    /// Each side divides its own seconds by its own share of the travel,
    /// which is the whole point of the split: the past keeps `past_secs`
    /// across `split` of the rail whatever the horizon does.
    pub(crate) fn offset_secs(&self, frac: f32) -> f32 {
        let frac = frac.clamp(0.0, 1.0);
        if frac <= self.split || !self.has_future() {
            // `split` is never 0.0: `past_only` pins it at 1.0 and the
            // forecast form at `NOW_SPLIT`.
            -self.past_secs * (1.0 - frac / self.split)
        } else {
            self.future_secs * (frac - self.split) / (1.0 - self.split)
        }
    }

    /// [`Self::offset_secs`] inverted — where on the travel an instant
    /// `offset_secs` either side of `now` rests. Clamped into the rail, so a
    /// clock older than the window pins at the left end rather than running
    /// off it.
    pub(crate) fn frac_of_offset(&self, offset_secs: f32) -> f32 {
        if offset_secs <= 0.0 || !self.has_future() {
            self.split * (1.0 + offset_secs / self.past_secs).clamp(0.0, 1.0)
        } else {
            self.split + (1.0 - self.split) * (offset_secs / self.future_secs).clamp(0.0, 1.0)
        }
    }

    /// **How wide the live zone around the `now` boundary is**, in points,
    /// over a rail whose travel is `travel_px`. See [`LIVE_SNAP_PX`] and
    /// [`LIVE_SNAP_MAX_FUTURE_SHARE`].
    pub(crate) fn live_snap_px(&self, travel_px: f32) -> f32 {
        let future_px = (1.0 - self.split) * travel_px;
        if future_px <= 0.0 {
            return LIVE_SNAP_PX;
        }
        LIVE_SNAP_PX.min(LIVE_SNAP_MAX_FUTURE_SHARE * future_px)
    }

    /// Whether a release at `frac` means "back to live" rather than an
    /// instant. Symmetric about the boundary, so on a rail with no forecast
    /// region — where the boundary is the right end — only its left half is
    /// reachable and the rule reads exactly as the old right-end one did.
    pub(crate) fn is_live_release(&self, frac: f32, travel_px: f32) -> bool {
        ((frac.clamp(0.0, 1.0) - self.split) * travel_px).abs() <= self.live_snap_px(travel_px)
    }

    /// **Quantise a forecast instant onto the transport layer's frame grid.**
    ///
    /// A 48 h horizon over the forecast region's 143 px is 2.92 px per frame,
    /// so a pixel-accurate pick is not a thing a hand can do; a release in
    /// the forecast region lands on the nearest stamp instead. The grid is
    /// the layer's own [`rustdar_source::time::TimeAxis`] step, anchored on
    /// the epoch, which is where an hourly model's valid times are. It names
    /// a grid, not a promise that a frame exists on it — the same bound
    /// `SourceHandler::frame_horizon` carries.
    pub(crate) fn snap_future(&self, target: chrono::NaiveDateTime) -> chrono::NaiveDateTime {
        if self.step_secs <= 0 {
            return target;
        }
        let secs = target.and_utc().timestamp();
        let step = self.step_secs;
        let rounded = (secs as f64 / step as f64).round() as i64 * step;
        chrono::DateTime::from_timestamp(rounded, 0).map_or(target, |dt| dt.naive_utc())
    }
}

/// **What `egui::Slider` shortens its travel by at each end**, so the handle
/// stays inside the rect it was given.
///
/// `rect.height() / 2.5`, then narrowed by the aspect ratio when the style
/// draws a rectangular handle — which the default style does, at 0.75. On the
/// transport's 18.0 pt rail that is 7.2 x 0.75 = **5.40 pt per end**, and it
/// is read from the style rather than assumed because a circular handle
/// shortens by the full 7.2.
pub(crate) fn slider_end_inset(rect: egui::Rect, handle_shape: egui::style::HandleShape) -> f32 {
    let radius = rect.height() / 2.5;
    match handle_shape {
        egui::style::HandleShape::Circle => radius,
        egui::style::HandleShape::Rect { aspect_ratio } => radius * aspect_ratio,
    }
}

/// **How far a slider's handle actually travels inside `rect`** — the
/// distance the `0.0..=1.0` fraction is spread over, and the denominator of
/// every figure in seconds or frames per pixel.
///
/// Not the rect's width: [`slider_end_inset`] comes off each end, turning the
/// transport's 488.0 pt rail rect into **477.2 pt of travel**.
pub(crate) fn slider_travel_px(rect: egui::Rect, handle_shape: egui::style::HandleShape) -> f32 {
    (rect.width() - 2.0 * slider_end_inset(rect, handle_shape)).max(0.0)
}

/// **Where a loop rail's past/future break sits, as a fraction of the
/// slider's travel** — `None` when there is no break to draw.
///
/// **A loop rail is not the archive rail and does not split at
/// [`NOW_SPLIT`].** Its frames are evenly spaced whatever their stamps are,
/// so there is no change of seconds per pixel to signal; the colour there
/// says the simpler, per-frame thing — *which of these frames are observed
/// and which are forecast*. So the break goes where the frames straddle the
/// wall clock. A fixed fraction would paint the first 70% of an f00-f48 run
/// as history, which is a lie about every frame it covers.
///
/// **It sits midway between the two frames that straddle `now`.** An
/// `egui::Slider` over `0..=total-1` puts frame `i`'s handle at `i/(total-1)`
/// of the travel: the frames are *positions*, not cells, so "the trailing
/// edge of the last past frame" and "the leading edge of the first future
/// frame" are the same point, and that point is the midpoint between the two
/// handle positions. Putting the break on either frame's own position instead
/// would leave that frame's handle sitting astride the boundary — and that is
/// exactly the frame whose side the reader most needs to read off.
///
/// The three ends of the range, in the order they are decided:
/// - **no frames, or every frame at or before `now`** — `None`. A radar loop
///   is always the latter, so it never enters the two-colour path at all and
///   its rail stays the shape set it has always painted.
/// - **every frame after `now`** — `0.0`: the break is the rail's own left
///   edge and there is no past region at all.
/// - otherwise the midpoint above, which is strictly inside `(0, 1)`.
///
/// **The tie is that `valid <= now` is past** — the same `<=` as
/// [`RailRegions::side_of`] and as `LayerTimeState::qualifying_frame_at`, read
/// here as a `partition_point` over stamps the loop already keeps in
/// ascending order, which is the ordering that same lookup depends on.
///
/// **The break moves as the cycle rolls**, and that is the accepted cost: it
/// is a statement about these frames against this instant, so a refetch or a
/// resample lands it somewhere else. A landmark that held still while the
/// frames under it changed side would be the wrong landmark.
pub(crate) fn loop_rail_split(frames: &[LoopFrame], now: chrono::NaiveDateTime) -> Option<f32> {
    let total = frames.len();
    let past = frames.partition_point(|frame| frame.timestamp <= now);
    if past == total {
        // Every frame is history — and the empty loop, where `0 == 0`.
        return None;
    }
    if past == 0 {
        return Some(0.0);
    }
    // `total >= 2` here: `1 <= past < total`.
    Some((past as f32 - 0.5) / (total - 1) as f32)
}

/// **One bar, two colours inside it, meeting at `now`.**
///
/// One `egui::Slider`: one id, one hit target, one continuous drag surface
/// that a press in either region can cross without letting go. The regions
/// are paint, not widgets. Both rails come through here — the archive rail,
/// where `split` is [`NOW_SPLIT`], and the loop rail, where it is
/// [`loop_rail_split`] — so the two forms cannot drift apart in colour,
/// geometry or layering.
///
/// **Why the fills go under the slider rather than over it.** egui paints a
/// slider as rail, then handle, and the handle's radius is larger than the
/// rail is tall — so a fill laid over the rail would bury the handle wherever
/// the two met. The two fills are added to slots reserved *before* the widget
/// and filled in after it, which puts them under both; egui's own one-colour
/// rail is collapsed to nothing by zeroing `slider_rail_height`, a number the
/// widget uses for that rail and for nothing else — not for its size, its
/// travel, its handle or its hit rect. The zero-height rail rect it still
/// emits covers no pixels.
///
/// **A rail with no break does not come through here at all**: `split` is
/// `None`, and it takes the untouched widget, with no reserved slots and no
/// style edit, so its rail is the shape set it has always painted. That is
/// what keeps a radar pane — archive rail and loop rail both — pixel for
/// pixel what it was.
fn paint_split_rail(
    ui: &mut egui::Ui,
    split: Option<f32>,
    add_slider: impl FnOnce(&mut egui::Ui) -> egui::Response,
) -> egui::Response {
    let Some(split) = split else {
        return add_slider(ui);
    };

    let rail_height = ui.spacing().slider_rail_height;
    let corner_radius = ui.visuals().widgets.inactive.corner_radius;
    // The past keeps today's trough exactly. The future is that colour
    // carried halfway to the window's own background, so it recedes into the
    // page in whichever theme is up: 230 -> 239 in light, 60 -> 43 in dark.
    // One expression, both themes, read off the live `Visuals` rather than
    // from a pair of hard-coded colours that would be right in one of them.
    let past_fill = ui.visuals().widgets.inactive.bg_fill;
    let future_fill = past_fill.lerp_to_gamma(ui.visuals().window_fill, FUTURE_TOWARD_WINDOW);
    let boundary_color = ui.visuals().widgets.active.fg_stroke.color;

    let past_slot = ui.painter().add(egui::Shape::Noop);
    let future_slot = ui.painter().add(egui::Shape::Noop);
    let boundary_slot = ui.painter().add(egui::Shape::Noop);
    ui.spacing_mut().slider_rail_height = 0.0;
    let response = add_slider(ui);
    // Put the style back: the loop rail is added inline beside the transport
    // buttons and the frame stamp, so the zero must not outlive the widget it
    // was for.
    ui.spacing_mut().slider_rail_height = rail_height;

    let rect = response.rect;
    let rail = egui::Rect::from_min_max(
        egui::pos2(rect.left(), rect.center().y - rail_height / 2.0),
        egui::pos2(rect.right(), rect.center().y + rail_height / 2.0),
    );
    // Where the handle sits when it reads `now` — the value position, not the
    // rect's own fraction, so the colour break and the handle land on the
    // same pixel.
    //
    // **At either extreme the break is the rail's own edge**, not the
    // handle's end cap. A loop whose every frame is forecast splits at `0.0`,
    // and carrying the cap's 5.4 pt in the past colour would draw a past
    // region onto a rail that has no past frame on it.
    let handle_shape = ui.style().visuals.handle_shape;
    let break_x = if split <= 0.0 {
        rect.left()
    } else if split >= 1.0 {
        rect.right()
    } else {
        rect.left()
            + slider_end_inset(rect, handle_shape)
            + split * slider_travel_px(rect, handle_shape)
    };

    // A region with no width is not painted at all: on a loop whose every
    // frame is forecast the break is the rail's left edge, and "no past
    // region" has to be true of the shape set and not only of the pixels.
    let painter = ui.painter();
    let past_rect = egui::Rect::from_min_max(rail.min, egui::pos2(break_x, rail.max.y));
    if past_rect.width() > 0.0 {
        painter.set(
            past_slot,
            egui::epaint::RectShape::filled(past_rect, corner_radius, past_fill),
        );
    }
    let future_rect = egui::Rect::from_min_max(egui::pos2(break_x, rail.min.y), rail.max);
    if future_rect.width() > 0.0 {
        painter.set(
            future_slot,
            egui::epaint::RectShape::filled(future_rect, corner_radius, future_fill),
        );
    }
    painter.set(
        boundary_slot,
        egui::Shape::LineSegment {
            points: [
                egui::pos2(break_x, rail.top()),
                egui::pos2(break_x, rail.bottom()),
            ],
            stroke: egui::Stroke::new(1.0, boundary_color),
        },
    );
    response
}

/// The archive rail's call into [`paint_split_rail`]: `now` sits at
/// [`RailRegions::split`], and a pane with no forecast region has no break.
fn paint_two_colour_rail(
    ui: &mut egui::Ui,
    regions: RailRegions,
    frac: &mut f32,
) -> egui::Response {
    paint_split_rail(ui, regions.has_future().then_some(regions.split), |ui| {
        ui.add(egui::Slider::new(frac, 0.0..=1.0).show_value(false))
    })
}

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
    /// The fraction of the archive rail the handle was drawn at this frame —
    /// `None` while the loop (frame-index) rail is up instead.
    pub scrub_frac: Option<f32>,
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
            scrub_frac: None,
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
///
/// True of the **setting**. What a loop is listed over is the addressed
/// layer's own window, and `Gui::tuning_scope_caption` adds that figure when
/// the two differ.
const TUNING_SCOPE_CAPTION: &str = "Lookback and Speed apply to every pane, linked or not.";

/// The word the time chip prints beside its stamp, for the pane that supplied
/// it (WI-9). Three-way, and the stamp outranks the posture: a stamp after the
/// wall clock is a **forecast** whatever [`PaneState::viewing_live`] says —
/// a pane parked on a forecast frame still follows its live site, and calling
/// that picture "live" is the misreading this word exists to close.
fn chip_word(
    pane: &crate::pane::PaneState,
    t: Option<chrono::NaiveDateTime>,
    now: chrono::NaiveDateTime,
) -> &'static str {
    if t.is_some_and(|t| t > now) {
        "forecast"
    } else if pane.viewing_live {
        "live"
    } else {
        "archive"
    }
}

/// `secs` as a window a reader can hold: whole hours where it is whole hours,
/// minutes otherwise.
fn humanise_window(secs: u64) -> String {
    if secs >= 3600 && secs.is_multiple_of(3600) {
        format!("{} h", secs / 3600)
    } else if secs >= 60 {
        format!("{} min", secs / 60)
    } else {
        format!("{secs} s")
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
                    let (_, word) = self.chip_time_source();
                    let chip =
                        ui.button(format!("\u{23f1} {} - {}", self.active_time_label(), word));
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

    /// The time the chips describe, and the word that annotates it —
    /// with the fallback that keeps a non-map active pane honest (the
    /// first-run `--:--:--` finding): the active pane's on-screen time,
    /// else the static [`PaneState::data_time`] it carries whatever its
    /// kind, else the freshest visible pane's on-screen time. The word
    /// travels with whichever pane supplied the time, so the annotation
    /// describes the time actually shown.
    ///
    /// **The fallback runs only for a pane with no transport timeline**
    /// (WI-9). A pane whose transport is active owns its answer even while
    /// the playhead has no frame to name — borrowing another pane's clock
    /// there is how a forecast loop got captioned with a neighbour's radar
    /// stamp.
    pub(super) fn chip_time_source(&self) -> (Option<chrono::NaiveDateTime>, &'static str) {
        let now = chrono::Utc::now().naive_utc();
        let active = &self.panes[self.active_pane];
        let own = active.data_time_on_screen().or(active.data_time);
        if own.is_some() || active.transport_state().is_active() {
            return (own, chip_word(active, own, now));
        }
        self.panes()
            .iter()
            .filter_map(|pane| pane.data_time_on_screen().map(|t| (t, pane)))
            .max_by(|a, b| a.0.cmp(&b.0))
            .map_or((None, chip_word(active, None, now)), |(t, pane)| {
                (Some(t), chip_word(pane, Some(t), now))
            })
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
        let (source_time, source_word) = self.chip_time_source();
        let age_text = source_time
            .map(|collected| {
                super::statusbar::format_product_age(chrono::Utc::now().naive_utc() - collected)
            })
            .unwrap_or_default();
        let stamp_text = format!("{} - {}", self.active_time_label(), source_word);

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
        // The forward-step's real question (WI-10): is there anywhere forward
        // of here? At the live edge there is not; parked on a forecast frame
        // there is — whatever `viewing_live` says about the selection posture.
        let at_live_edge =
            viewing_live && !self.panes[pane_idx].depicts_future(chrono::Utc::now().naive_utc());

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
            .add_enabled(!at_live_edge, egui::Button::new("\u{23f5}"))
            .on_hover_text("Forward one step");
        #[cfg(test)]
        {
            self.probes.last_timeline.fwd = (fwd.rect, !at_live_edge);
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
        let loop_active = self.panes[pane_idx].transport_state().is_active();
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
                    // Which layer this loop is OF, asked at the moment it is
                    // started rather than inherited from whatever the pane
                    // last addressed — see `PaneState::refresh_transport`.
                    let Self {
                        overlays, panes, ..
                    } = self;
                    panes[pane_idx].refresh_transport(overlays);
                    actions.push(GuiAction::EnableLoop {
                        pane_idx,
                        // The window this pane's transport layer is actually
                        // looped over: the one number the file holds, raised
                        // to that layer's own floor — see
                        // `Gui::loop_span_secs_for`. Asked *after*
                        // `refresh_transport`, because the floor is the
                        // addressed layer's and the address was just decided.
                        lookback_secs: self.loop_span_secs_for(pane_idx),
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

        let loop_state = self.panes[pane_idx].transport_state();
        let loop_frames = loop_state
            .is_active()
            .then_some(loop_state.frames.len())
            .filter(|&total| total > 0);

        if let Some(total) = loop_frames {
            // The same bar in the same two colours as the archive form, split
            // on the rule a frame-index rail carries: where the frames
            // straddle `now`. A loop with no forecast frame in it - every
            // radar loop - answers `None` and paints exactly what it did.
            let split = loop_rail_split(
                &self.panes[pane_idx].transport_state().frames,
                chrono::Utc::now().naive_utc(),
            );
            let seek = ui
                .push_id("scrub_loop", |ui| {
                    let mut frame_idx = self.panes[pane_idx].transport_state().current_frame();
                    let seek = paint_split_rail(ui, split, |ui| {
                        ui.add(egui::Slider::new(&mut frame_idx, 0..=(total - 1)).show_value(false))
                    });
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

        let regions = self.rail_regions(pane_idx);
        let now = chrono::Utc::now().naive_utc();
        // `viewing_live` alone is the wrong question here (WI-10): a pane
        // parked on a forecast frame still follows its live site, and resting
        // the handle on the `now` boundary would paint it hours left of the
        // instant the pane depicts.
        let pane = &self.panes[pane_idx];
        let resting = if pane.viewing_live && !pane.depicts_future(now) {
            regions.split
        } else {
            // What the handle marks is the instant on screen. A forecast park
            // has no collected stamp there, so the clock is the mark; anywhere
            // else the data stamp stays the honest position.
            let marked = match pane.time.mode {
                TimeMode::AsOf(t) if t > now => Some(t),
                _ => pane.data_time_on_screen(),
            };
            match marked {
                Some(t) => regions.frac_of_offset((t - now).num_seconds() as f32),
                None => regions.split,
            }
        };
        let mut frac = self.timeline_scrub.unwrap_or(resting);
        let scrub = ui
            .push_id("scrub_archive", |ui| {
                paint_two_colour_rail(ui, regions, &mut frac)
            })
            .inner;
        #[cfg(test)]
        {
            self.probes
                .widget_id_probes
                .push(("timeline_scrubber", scrub.id));
            self.probes.last_timeline.scrubber = scrub.rect;
            self.probes.last_timeline.scrub_frac = Some(frac);
        }
        let travel_px = slider_travel_px(scrub.rect, ui.style().visuals.handle_shape);
        if scrub.drag_stopped() {
            self.timeline_scrub = None;
            self.commit_archive_scrub(frac, regions, travel_px, actions);
        } else if scrub.dragged() {
            self.timeline_scrub = Some(frac);
        } else if scrub.changed() {
            self.timeline_scrub = None;
            self.commit_archive_scrub(frac, regions, travel_px, actions);
        } else {
            self.timeline_scrub = None;
        }
    }

    /// **The two regions this pane's archive rail carries** — the past window
    /// it has always had, and the forecast region a transport layer reaching
    /// past the wall clock adds to it.
    ///
    /// **Both halves ask the transport layer, and nothing else asks anything.**
    /// The past is `Gui::loop_span_secs_for` — the Lookback setting raised
    /// to the floor that layer declares, which is the same window its loop is
    /// listed over, so turning the loop off no longer lands the clock outside
    /// the rail that has to show it. The forecast is
    /// `SourceHandler::frame_horizon`, read off the registry by id rather
    /// than re-spelled here as a `match` on the id.
    ///
    /// A layer whose axis does not declare `extends_future`, one the registry
    /// does not serve, or a forecast layer whose current selection happens to
    /// reach nowhere forward all get [`RailRegions::past_only`] — and with it
    /// the rail this scrubber had before WI-11, to the pixel.
    fn rail_regions(&self, pane_idx: usize) -> RailRegions {
        let past_only = RailRegions::past_only(self.loop_span_secs_for(pane_idx).max(1) as f32);
        let Some(pane) = self.panes.get(pane_idx) else {
            return past_only;
        };
        let id = pane.transport_layer().clone();
        let Some(handler) = self.overlays.handler_by_id(&id) else {
            return past_only;
        };
        let rustdar_source::time::TimeAxis::FrameSeries {
            typical_step,
            extends_future: true,
        } = handler.time_axis()
        else {
            return past_only;
        };
        let view = pane.view(pane_idx);
        let horizon = handler.frame_horizon(&view.layer(&id)).num_seconds();
        if horizon <= 0 {
            return past_only;
        }
        RailRegions {
            split: NOW_SPLIT,
            past_secs: past_only.past_secs,
            future_secs: horizon as f32,
            step_secs: typical_step.as_secs() as i64,
        }
    }

    /// **Commit a scrub position**: near the `now` boundary means live,
    /// anywhere else means the instant that point of the rail names — history
    /// left of the boundary, forecast right of it. One function for the
    /// release and the keyboard nudge, so the two routes cannot drift.
    ///
    /// **The clock write is layer-agnostic and unconditional**, which is what
    /// lets a pane carrying no radar be scrubbed at all. It used to sit
    /// behind radar's `scan_info`, so every clock-aware layer on a radar-less
    /// pane — alerts, storm reports, outlooks, a model loop — was frozen at
    /// whatever instant it started on no matter where the rail was dragged.
    fn commit_archive_scrub(
        &mut self,
        frac: f32,
        regions: RailRegions,
        travel_px: f32,
        actions: &mut Vec<GuiAction>,
    ) {
        let pane_idx = self.active_pane;
        if regions.is_live_release(frac, travel_px) {
            actions.push(GuiAction::JumpToLive { pane_idx });
            return;
        }
        let now = chrono::Utc::now().naive_utc();
        let released = now + chrono::Duration::seconds(regions.offset_secs(frac) as i64);
        // The forecast region quantises its release and the past region does
        // not: 2.92 px per frame at a 48 h horizon is not a distance a hand
        // resolves, while a past instant is a genuine free choice.
        let target = match regions.side_of(released, now) {
            RailSide::Past => released,
            RailSide::Future => regions.snap_future(released),
        };
        self.panes[pane_idx].viewing_live = false;
        // The release names an INSTANT, so say so: the pane's clock moves to
        // it and every layer on the pane is shown at that moment.
        self.panes[pane_idx].set_time_mode(TimeMode::AsOf(target));
        // Radar's half of the same answer, and only radar's. A released
        // instant later than the wall clock has no volume to fetch, and
        // asking for one would have `handle_navigate_time` clamp the target
        // back to now and report the pane live again while its clock names a
        // forecast hour — a pane claiming to be live over a picture of the
        // future.
        if regions.side_of(target, now) == RailSide::Past
            && let Some(scan_time) = self.panes[pane_idx]
                .scan_info
                .as_ref()
                .map(|info| info.timestamp)
        {
            actions.push(GuiAction::NavigateTime {
                pane_idx,
                step_secs: (target - scan_time).num_seconds(),
            });
        }
    }

    /// **What the Lookback and Speed sliders reach, and — when they are not
    /// the last word — the window this pane's loop actually gets.**
    ///
    /// [`TUNING_SCOPE_CAPTION`] describes the setting, which is still one
    /// number written to every pane. A layer whose frames are an hour apart
    /// declares a floor under that number (`Gui::loop_span_secs_for`), so on a
    /// pane addressing such a layer the slider is no longer the whole answer
    /// and the caption says what the answer is.
    ///
    /// **The quantity, never an apology.** The reader cannot act on "the
    /// slider is approximate"; they can read "Model Data loops 12 h - 13
    /// frames, 1 h apart" and know exactly what they are about to watch. The
    /// sentence appears only where the floor actually binds, so a radar pane's
    /// caption is the string it always was, character for character.
    fn tuning_scope_caption(&self, pane_idx: usize) -> String {
        let base = TUNING_SCOPE_CAPTION.to_owned();
        let Some(pane) = self.panes.get(pane_idx) else {
            return base;
        };
        let window = self.loop_span_secs_for(pane_idx);
        if window <= pane.time.span_secs {
            return base;
        }
        let Some(handler) = self.overlays.handler_by_id(pane.transport_layer()) else {
            return base;
        };
        let step = match handler.time_axis() {
            rustdar_source::time::TimeAxis::FrameSeries { typical_step, .. } => {
                typical_step.as_secs()
            }
            _ => return base,
        };
        if step == 0 {
            return base;
        }
        format!(
            "{base} {} loops {} - {} frames, {} apart.",
            handler.display_name(),
            humanise_window(window),
            window / step + 1,
            humanise_window(step),
        )
    }

    /// Row 2: the loop tuning, shown behind `⋯`.
    fn render_timeline_row2(&mut self, ui: &mut egui::Ui, actions: &mut Vec<GuiAction>) {
        let pane_idx = self.active_pane;
        let loop_active = self.panes[pane_idx].transport_state().is_active();
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
                                lookback_secs: self.loop_span_secs_for(pane_idx),
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
        let scope_caption = self.tuning_scope_caption(pane_idx);
        ui.label(egui::RichText::new(&scope_caption).small().weak());
        #[cfg(test)]
        {
            row2.tuning_scope = scope_caption;
        }

        if loop_active {
            let ls = self.panes[pane_idx].transport_state();
            let rendered = ls.frames.iter().filter(|f| f.image.is_some()).count();
            let total = ls.frames.len();
            let rendering = total > 0 && !ls.is_render_ready();
            let playing = ls.is_playing();
            let fetching = ls.is_fetching();
            // Two different questions off one playhead: the CAPTION says what
            // is on screen, so it is `None` when the clock names no frame; the
            // SLIDER below needs a handle position in `0..total`, and the
            // nearest frame is where the handle belongs.
            let current_frame = ls.current_frame();
            let frame_time = ls.playhead_stamp();
            let frame_split = loop_rail_split(&ls.frames, chrono::Utc::now().naive_utc());

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
                    // Row 2's seek is the same frame-index rail as row 1's,
                    // and the two are on screen together whenever the
                    // expander is open, so it carries the same break.
                    let seek = paint_split_rail(ui, frame_split, |ui| {
                        ui.add(egui::Slider::new(&mut frame_idx, 0..=(total - 1)).show_value(false))
                    });
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

/// The archive rail's two regions, and the rules that hang off the split.
#[path = "ui_timeline/rail_tests.rs"]
#[cfg(test)]
mod rail_tests;
