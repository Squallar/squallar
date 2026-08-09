//! Drawing a vertical cross-section, and being honest about what it is.
//!
//! # The one thing this module exists to prevent
//!
//! A cross-section *looks* like a photograph of a storm's vertical structure.
//! It is not. It is a picture of a **tilt ladder**, interpolated: the radar
//! measured a handful of conical surfaces and everything between them was filled
//! in by two-point interpolation in beam height. How badly that misleads is
//! range-dependent and it misleads in *both* directions. Measured on a real
//! volume:
//!
//! * A 4–6 km slab at **65 km** paints 3.28–8.23 km tall — up to **2.5× its
//!   true thickness**, because the two rungs bracketing it are 5.6 km apart
//!   there and the interpolation smears the layer across the whole gap.
//! * The same slab at **100 km** falls *between* rungs and paints **nothing at
//!   all**. The gap is 8.6 km there; the layer fits inside it and no rung
//!   intersects it.
//!
//! So layer thickness and echo-top height cannot be read off a section as
//! though they were measured, and a user who does not know that will read them
//! anyway, because nothing about the picture says otherwise.
//!
//! Three things here say otherwise, in descending order of how much they help:
//!
//! 1. **The rungs are drawn.** [`paint_tilt_ladder`] traces each elevation
//!    cut's beam centre across the section as a dotted curve. Data exists
//!    *along those curves*; everything between them is interpolation. This is
//!    the honest device, because it is not a warning to be dismissed — it is
//!    the shape of the sampling, in the picture, at the range each part of the
//!    picture is at, and it visibly fans apart with distance exactly as the
//!    error does.
//! 2. **The raster is uploaded `NEAREST`.** Bilinear filtering would blend the
//!    rung edges into a smooth gradient and paint precisely the impression of
//!    continuous measurement this module is refusing. The blockiness is data.
//!    (That decision lives with the upload, in `app_render.rs`.)
//! 3. **The caption stays calm, and the words live behind the ⓘ.** The
//!    default caption is the product, the ladder's own count and top angle,
//!    and the time of the volume the picture was cut from — nothing else, in
//!    the pane's quiet text colour. Everything long-form — what a short ladder
//!    means for what is on screen, where the interpolation lives, why echoes
//!    can sit off the map's ground track — moves behind the ⓘ beside the
//!    caption, written to a user rather than to this module's reviewers.
//!
//!    That split is a lesson paid for, not a retreat from honesty. The first
//!    caption led, in **error styling**, with where the ladder stopped against
//!    what the pattern declares, and quoted the top-of-coverage height at
//!    maximum range. Watched with real users, it read as something broken —
//!    something *they* had broken — and it fired on almost every volume,
//!    because AVSET ends a precipitation scan once the echo tops are below the
//!    cuts that remain: measured live at KLNX, every VCP 212 volume in a
//!    ten-minute window topped out between 6.4° and 8.0° of a declared 19.5°.
//!    A warning styled as a fault and shown on the ordinary case is not
//!    honesty; it is an alarm people learn to skip. And its headline number —
//!    ≈114.5 kft MSL at 225 km — was above the pane's own axis: a height no
//!    echo could ever be drawn at, pure alarm carrying zero information.
//!
//!    So the redesign keeps every fact and re-homes it. Red is reserved for
//!    states that are genuinely broken — a failed cut, a volume with no data
//!    for this moment. A filling or AVSET-ended volume is captioned in the
//!    same calm voice as a complete one, with its top angle in the default
//!    line for whoever knows what it means. The ⓘ detail says the consequence
//!    in the user's own units — and quotes a ceiling height **only when it is
//!    on the chart**, because a figure above the axis is unfalsifiable by
//!    looking. The cause of a short ladder is still never blamed: filling,
//!    abandoned and AVSET all read the same from one volume, and a caption
//!    that guessed would be wrong most often exactly when it sounded surest.
//!
//! # And the second thing, which is about the map rather than the section
//!
//! `render_gate` — the plan view's projection — applies no `cos(e)` and no beam
//! height model: it draws a gate at its *slant* range from the site, on the
//! ground. This section applies both. They therefore disagree, by 0.9 px at
//! 2.4° and by 18 px at 19.5° and 70 km. The section is the correct one. A user
//! comparing the section against the ground track drawn on the map above it
//! can see this, which is why it is explained at all — but it is explained in
//! the ⓘ detail, in words about what the user sees, not shipped in the default
//! caption: *"the section is right"* is this module arguing with itself in
//! front of the user, and it read exactly that way.

use crate::pane::PaneState;
use rustdar_radar::beam;
use rustdar_radar::sampler::SampleStatus;
use rustdar_radar::types::RadarProduct;
use rustdar_radar::xsect::{CrossSection, SECTION_HEIGHT, SECTION_WIDTH, SectionAxes};
use rustdar_units::UserPreferences;

/// Width of the height-axis gutter, in points.
const HEIGHT_GUTTER: f32 = 44.0;
/// Height of the distance-axis gutter, in points.
const DISTANCE_GUTTER: f32 = 16.0;
/// Points a section needs before axis labels are worth the room they take.
const LABELLED_AXES_MIN_HEIGHT: f32 = 120.0;
/// The most of a pane's height the caption may take before detail lines are
/// dropped, last first, to make room.
///
/// The caption is **wrapped**, so its height is a function of the pane's width
/// as well as its own text — with the ⓘ detail open, a narrow pane wraps each
/// explanation over half a dozen rows. Dropping whole detail lines is the right
/// answer there and truncating one is not: a sentence cut off mid-clause reads
/// as a rendering fault, and the sentences here are the ones the user just
/// asked to read in full. The default line and the status line are never
/// dropped — see [`CaptionLine::essential`].
const CAPTION_MAX_HEIGHT_FRACTION: f32 = 0.45;
/// Points reserved beside the caption for the ⓘ detail toggle, so the first
/// line's wrap never runs under the glyph that expands it.
const INFO_TOGGLE_RESERVE: f32 = 26.0;
/// Headroom between the caption and the plot, for the height axis's unit label.
///
/// [`paint_axes`] writes `MSL kft` **bottom**-aligned on `plot.top() - 2.0`, in
/// the left gutter — so with only a two-point gap it is drawn *upward*, over the
/// last line of the caption, which the caption also occupies at that x. Reserved
/// rather than relocated because the label belongs at the top of the axis it
/// names; it is only claimed when there are axis labels to name.
const AXIS_UNIT_HEADROOM: f32 = 13.0;
/// How many points along the line each tilt curve is sampled at.
///
/// The curve is a smooth function of ground range, and 64 segments across a
/// pane no wider than ~2000 points is well under a point of chord error.
const TILT_CURVE_SAMPLES: usize = 64;

/// Feet per kilometre, for the height axis in a `Feet` locale.
const KM_TO_KFT: f64 = 1.0 / 0.3048;

/// Room left along whichever edge the colour bar took, in points.
///
/// `render_color_scale` is reused verbatim for a section pane — the scale is a
/// property of the moment, and two spellings of one legend is how they come to
/// disagree — but it paints straight onto the pane rect with no notion of what
/// else is in there. `SCALE_MARGIN` (16) plus `SCALE_BAR_WIDTH` (20) plus the
/// value labels beside the bar; rounded up so a three-digit label does not
/// overhang the section.
const COLOR_SCALE_RESERVE: f32 = 64.0;

/// Draw a section pane: the picture, its axes, the tilt ladder over it, and the
/// caption that says what it is.
///
/// Writes `pane.hover_value`, which the status bar reads — see
/// [`hover_readout`] for why that readout is most of the value of the status
/// plane.
pub(super) fn render_cross_section(
    ui: &mut egui::Ui,
    pane: &mut PaneState,
    pane_rect: egui::Rect,
    horizontal_color_scale: bool,
    prefs: &UserPreferences,
) {
    pane.hover_value = None;

    let product = pane.selected_product;
    let site = pane.scan_info.as_ref().map(|s| (s.site.lat, s.site.lon));

    let Some(state) = pane.cross_section() else {
        return;
    };
    let Some(line) = state.line else {
        paint_centered(ui, pane_rect, super::CROSS_SECTION_EMPTY_STATE);
        return;
    };
    let (Some(section), Some(texture)) = (state.section.clone(), state.texture.clone()) else {
        // Nothing rendered. Either a stated reason, or a cut in flight — and
        // the two must not look alike: "waiting" that will never end is the
        // worst state a pane can be in, because there is nothing to do about it
        // and no way to tell.
        let message = state.unavailable.map_or_else(
            || "Cutting the cross-section\u{2026}".to_owned(),
            |u| u.message(),
        );
        paint_centered(ui, pane_rect, &message);
        return;
    };
    let unavailable = state.unavailable;
    let detail_open = state.detail_open;
    // The volume the picture on screen was actually cut from — not the pane's
    // `scan_info`, which follows the feed and can already name the *next*
    // volume while this raster is still of the last one. The caption describes
    // the picture, so its time has to be the picture's.
    let collected = state.rendered_for.as_ref().map(|t| t.volume.collected);

    let axes = *section.axes();

    let painter = ui.painter().with_clip_rect(pane_rect);
    // The caption is laid out before the layout is computed, because the layout
    // needs its height and its height is only known once it has been wrapped to
    // this pane's width.
    let caption = lay_out_caption(
        &painter,
        pane_rect,
        caption_lines(
            &axes,
            product,
            collected,
            unavailable,
            detail_open,
            ui.visuals(),
            prefs,
        ),
    );
    let caption_height = caption.iter().map(|g| g.rect.height()).sum();
    let layout = SectionLayout::new(pane_rect, caption_height, horizontal_color_scale);

    painter.rect_filled(pane_rect, 0.0, ui.visuals().extreme_bg_color);
    painter.image(
        texture.id(),
        layout.plot,
        egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
        egui::Color32::WHITE,
    );

    if layout.labelled_axes {
        paint_axes(&painter, &layout, &axes, ui.visuals(), prefs);
    }
    if let Some((site_lat, site_lon)) = site {
        paint_tilt_ladder(
            &painter,
            &layout,
            &axes,
            (line.a().lat, line.a().lon),
            (line.b().lat, line.b().lon),
            site_lat,
            site_lon,
            section.tilt_elevations_deg(),
        );
    }
    painter.rect_stroke(
        layout.plot,
        0.0,
        egui::Stroke::new(1.0, ui.visuals().weak_text_color()),
        egui::StrokeKind::Outside,
    );

    // Each galley was laid out with its own colour, so the third argument is
    // only the fallback egui uses for a galley that carries none.
    let mut y = layout.caption.top();
    let mut first_line_end = egui::pos2(layout.caption.left(), y);
    for (i, galley) in caption.into_iter().enumerate() {
        let height = galley.rect.height();
        if i == 0 {
            // Where the first *visual* row ends, not where the galley's widest
            // row does: the toggle sits beside the sentence it expands.
            let first_row_width = galley.rows.first().map_or(0.0, |row| row.rect().width());
            first_line_end = egui::pos2(layout.caption.left() + first_row_width + 6.0, y);
        }
        painter.galley(
            egui::pos2(layout.caption.left(), y),
            galley,
            ui.visuals().text_color(),
        );
        y += height;
    }

    // The ⓘ detail toggle, at the end of the caption's first line. A widget in
    // a pane that is otherwise pure painting, because the whole point of the
    // redesign is that the long-form honesty is *reachable* rather than shouted:
    // a glyph that could not be clicked would be a decoration.
    let toggle_color = if detail_open {
        ui.visuals().hyperlink_color
    } else {
        ui.visuals().weak_text_color()
    };
    // `ℹ` (U+2139) rather than the prettier `ⓘ` (U+24D8): egui's bundled
    // fonts carry a glyph for the former (NotoEmoji) and none for the latter,
    // and a tofu box is not an affordance.
    let glyph_rect = painter.text(
        first_line_end,
        egui::Align2::LEFT_TOP,
        "\u{2139}",
        egui::FontId::proportional(12.0),
        toggle_color,
    );
    let response = ui
        .interact(
            glyph_rect.expand(4.0),
            ui.id().with("section_detail_toggle"),
            egui::Sense::click(),
        )
        .on_hover_text("What this picture is \u{2014} and what it is not");
    if response.clicked()
        && let Some(state) = pane.cross_section_mut()
    {
        state.detail_open = !detail_open;
    }

    // The pan/sweep controls and the bearing readout, over the plot's top-right
    // corner. A step is one deliberate action, so — unlike the map-side drag,
    // which previews live and re-cuts only on the drop — each step writes the
    // line immediately and lets the ordinary staleness poll re-cut: one click,
    // one cut, and the pane feels like a control rather than a queue. The
    // picture stands until the new cut lands, for the reason
    // `Gui::apply_pending_section_edit` gives at length.
    if let Some(new_line) = render_line_controls(ui, &painter, &layout, line, prefs)
        && let Some(state) = pane.cross_section_mut()
    {
        state.line = Some(new_line);
    }

    if let Some(pos) = ui.ctx().pointer_hover_pos()
        && pane_rect.contains(pos)
    {
        pane.hover_value = hover_readout(&section, &layout, pos, product, prefs);
    }
}

/// One step-control button: a small dark chip with a glyph, clickable.
///
/// Painted rather than a widget beyond the `interact`, like everything else in
/// this pane — and each chip carries a tooltip because four bare glyphs in a
/// corner are a puzzle, not a control.
fn control_chip(
    ui: &egui::Ui,
    painter: &egui::Painter,
    rect: egui::Rect,
    glyph: &str,
    salt: &str,
    tooltip: &str,
) -> bool {
    let response = ui.interact(
        rect,
        ui.id().with(("section_line_control", salt)),
        egui::Sense::click(),
    );
    painter.rect_filled(
        rect,
        4.0,
        egui::Color32::from_rgba_unmultiplied(0, 0, 0, 150),
    );
    painter.rect_stroke(
        rect,
        4.0,
        egui::Stroke::new(1.0, ui.visuals().weak_text_color()),
        egui::StrokeKind::Inside,
    );
    painter.text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        glyph,
        egui::FontId::proportional(12.0),
        ui.visuals().text_color(),
    );
    response.on_hover_text(tooltip).clicked()
}

/// The section pane's own line controls: pan the line across itself, sweep it
/// about its midpoint, and read its bearing and length — GR2Analyst's Position
/// slider, as steps, plus the orientation that makes sweeping feel aimed.
///
/// Returns the stepped line for the caller to commit, or `None`.
///
/// # Why these live on the section pane
///
/// The map-side gestures (issue #9's handles, the body drag) are the coarse,
/// direct motion; these are the fine one, and they belong where the user is
/// *looking* while walking a cut through a storm — at the section. A hand on
/// these chips and eyes on the picture is the whole "sweep along a storm"
/// workflow, and it needs no second pane on a phone.
///
/// # The readout
///
/// `056° · 62 mi` — the A→B bearing at the line's midpoint and the line's
/// ground length, in the user's units. Without it a sweep is blind: every step
/// looks like "the picture changed", and only the number says which way the
/// cut now faces. Three digits so north reads `000°`, not `0°`.
///
/// The glyphs are chosen from what egui's bundled fonts actually carry
/// (`◀`/`▶` from NotoEmoji, `↺`/`↻` from emoji-icon-font) — see the caption's
/// ⓘ for the tofu lesson.
fn render_line_controls(
    ui: &egui::Ui,
    painter: &egui::Painter,
    layout: &SectionLayout,
    line: crate::pane::SectionLine,
    prefs: &UserPreferences,
) -> Option<crate::pane::SectionLine> {
    use crate::ui_section_edit as edit;

    let button = egui::vec2(22.0, 18.0);
    let gap = 4.0;
    let top = layout.plot.top() + 4.0;
    // Too narrow for the row means no row: the plot keeps its corner, and the
    // map-side gestures still work.
    if layout.plot.width() < 4.0 * (button.x + gap) + 80.0 {
        return None;
    }

    let mut right = layout.plot.right() - 6.0;
    let mut chip = |glyph: &str, salt: &str, tooltip: &str| -> bool {
        let rect = egui::Rect::from_min_size(egui::pos2(right - button.x, top), button);
        right -= button.x + gap;
        control_chip(ui, painter, rect, glyph, salt, tooltip)
    };

    // Right-to-left, so on screen the row reads ◀ ▶ ↺ ↻.
    let cw = chip(
        "\u{21bb}",
        "cw",
        "Sweep the line clockwise about its middle",
    );
    let ccw = chip(
        "\u{21ba}",
        "ccw",
        "Sweep the line counter-clockwise about its middle",
    );
    let pan_right = chip(
        "\u{25b6}",
        "pan_right",
        "Slide the line to the right of its A\u{2192}B direction",
    );
    let pan_left = chip(
        "\u{25c0}",
        "pan_left",
        "Slide the line to the left of its A\u{2192}B direction",
    );

    let length_km = edit::length_km(line);
    painter.text(
        egui::pos2(right - 4.0, top + button.y * 0.5),
        egui::Align2::RIGHT_CENTER,
        format!(
            "{:03.0}\u{b0} \u{b7} {:.0}{}",
            edit::bearing_deg(line).rem_euclid(360.0),
            prefs.distance.convert_from_km(length_km),
            prefs.distance.suffix(),
        ),
        egui::FontId::proportional(10.0),
        ui.visuals().text_color(),
    );

    let step_km = edit::pan_step_km(length_km);
    if pan_left {
        edit::panned(line, -step_km)
    } else if pan_right {
        edit::panned(line, step_km)
    } else if ccw {
        edit::rotated(line, -edit::SWEEP_STEP_DEG)
    } else if cw {
        edit::rotated(line, edit::SWEEP_STEP_DEG)
    } else {
        None
    }
}

/// Where each part of a section pane goes.
///
/// Computed from the pane's rect alone, so the same pane in a 2×2 split and in
/// a full-window layout differ only in how much room the picture gets — and so
/// the hover readout and the drawing agree about where the plot is by
/// construction rather than by two matching calculations.
struct SectionLayout {
    /// The section raster's rect.
    plot: egui::Rect,
    /// Where the caption's lines start.
    caption: egui::Rect,
    /// Whether there was room for axis labels.
    labelled_axes: bool,
}

/// The width the caption's text is wrapped to on a pane of this shape.
///
/// [`INFO_TOGGLE_RESERVE`] is held back so the first line's text can never run
/// under the ⓘ drawn at its end — and it is held back from every line rather
/// than only the first, because two wrap widths in one block would make the
/// detail lines ragged against the line above them.
fn caption_wrap_width(pane_rect: egui::Rect) -> f32 {
    (pane_rect.width() - 8.0 - INFO_TOGGLE_RESERVE).max(1.0)
}

impl SectionLayout {
    /// `caption_height` is **measured**, not counted: the caption wraps, so how
    /// many rows it occupies is a function of the pane's width and of how long
    /// the sentences came out in the user's own units. Counting lines instead is
    /// what let the registration caveat run flush off the right-hand edge of a
    /// pane in a 2×2 split and be clipped mid-sentence.
    ///
    /// `horizontal_color_scale` is the orientation `ColorScaleOrientation`
    /// resolved for the whole panel, and it is an *input* here rather than
    /// something read back afterwards: the colour bar is painted straight onto
    /// the pane rect by `render_color_scale`, so the plot has to leave room on
    /// whichever edge the bar took, or the bar lands on top of the section.
    fn new(pane_rect: egui::Rect, caption_height: f32, horizontal_color_scale: bool) -> Self {
        let labelled_axes = pane_rect.height() >= LABELLED_AXES_MIN_HEIGHT;
        let caption = egui::Rect::from_min_size(
            pane_rect.min + egui::vec2(4.0, 2.0),
            egui::vec2(caption_wrap_width(pane_rect), caption_height),
        );
        let (left, bottom) = if labelled_axes {
            (HEIGHT_GUTTER, DISTANCE_GUTTER)
        } else {
            (2.0, 2.0)
        };
        let (scale_right, scale_bottom) = if horizontal_color_scale {
            (0.0, COLOR_SCALE_RESERVE)
        } else {
            (COLOR_SCALE_RESERVE, 0.0)
        };
        // The gap is the axis unit label's, and it is only owed when there is an
        // axis unit label. See `AXIS_UNIT_HEADROOM`.
        let top_gap = if labelled_axes {
            AXIS_UNIT_HEADROOM
        } else {
            2.0
        };
        let plot = egui::Rect::from_min_max(
            egui::pos2(pane_rect.left() + left, caption.bottom() + top_gap),
            egui::pos2(
                pane_rect.right() - 4.0 - scale_right,
                pane_rect.bottom() - bottom - scale_bottom,
            ),
        );
        Self {
            plot,
            caption,
            labelled_axes,
        }
    }

    /// Screen `y` of a height in km MSL. Row 0 of the raster is the top, and so
    /// is `top_km_msl`.
    fn y_of_height(&self, axes: &SectionAxes, km_msl: f64) -> f32 {
        let span = axes.top_km_msl - axes.base_km_msl;
        if span <= 0.0 {
            return self.plot.bottom();
        }
        let frac = (axes.top_km_msl - km_msl) / span;
        self.plot.top() + (frac as f32) * self.plot.height()
    }

    /// Screen `x` of a distance in km from the line's `a` end.
    fn x_of_distance(&self, axes: &SectionAxes, km: f64) -> f32 {
        if axes.length_km <= 0.0 {
            return self.plot.left();
        }
        self.plot.left() + ((km / axes.length_km) as f32) * self.plot.width()
    }
}

/// One line of centred, muted text on an otherwise empty pane.
///
/// Wrapped rather than elided: every message that reaches here is a sentence
/// explaining a state, and half a sentence explains nothing.
fn paint_centered(ui: &mut egui::Ui, pane_rect: egui::Rect, text: &str) {
    let galley = ui.painter().layout(
        text.to_owned(),
        egui::FontId::proportional(14.0),
        ui.visuals().weak_text_color(),
        pane_rect.width() - 32.0,
    );
    let at = pane_rect.center() - galley.size() / 2.0;
    ui.painter()
        .galley(at, galley, ui.visuals().weak_text_color());
}

/// Height ticks up the left gutter and distance ticks along the bottom.
fn paint_axes(
    painter: &egui::Painter,
    layout: &SectionLayout,
    axes: &SectionAxes,
    visuals: &egui::Visuals,
    prefs: &UserPreferences,
) {
    let label = visuals.weak_text_color();
    let grid = egui::Color32::from_rgba_unmultiplied(160, 160, 160, 45);

    // Heights, in the user's own unit. The axis is in km MSL internally
    // whatever the locale, so the ticks are chosen in the *displayed* unit and
    // converted back — otherwise a `Feet` user gets ticks at 3.28, 6.56, 9.84.
    let to_display = |km: f64| match prefs.height {
        rustdar_units::HeightUnit::Feet => km * KM_TO_KFT,
        rustdar_units::HeightUnit::Meters => km,
    };
    let from_display = |shown: f64| match prefs.height {
        rustdar_units::HeightUnit::Feet => shown / KM_TO_KFT,
        rustdar_units::HeightUnit::Meters => shown,
    };
    let step = nice_step(
        to_display(axes.top_km_msl - axes.base_km_msl),
        (layout.plot.height() / 34.0) as f64,
    );
    let mut shown = (to_display(axes.base_km_msl) / step).ceil() * step;
    while from_display(shown) <= axes.top_km_msl {
        let y = layout.y_of_height(axes, from_display(shown));
        painter.line_segment(
            [
                egui::pos2(layout.plot.left(), y),
                egui::pos2(layout.plot.right(), y),
            ],
            egui::Stroke::new(1.0, grid),
        );
        painter.text(
            egui::pos2(layout.plot.left() - 4.0, y),
            egui::Align2::RIGHT_CENTER,
            format!("{shown:.0}"),
            egui::FontId::proportional(10.0),
            label,
        );
        shown += step;
    }
    painter.text(
        egui::pos2(layout.plot.left() - 4.0, layout.plot.top() - 2.0),
        egui::Align2::RIGHT_BOTTOM,
        format!("MSL {}", prefs.height.kilo_suffix()),
        egui::FontId::proportional(10.0),
        label,
    );

    // Distance along the line, from the `A` end. Same nice-number treatment.
    let shown_length = prefs.distance.convert_from_km(axes.length_km);
    let step = nice_step(shown_length, (layout.plot.width() / 70.0) as f64);
    let mut at = 0.0;
    while at <= shown_length {
        let km = at / prefs.distance.convert_from_km(1.0);
        let x = layout.x_of_distance(axes, km);
        painter.line_segment(
            [
                egui::pos2(x, layout.plot.top()),
                egui::pos2(x, layout.plot.bottom()),
            ],
            egui::Stroke::new(1.0, grid),
        );
        painter.text(
            egui::pos2(x, layout.plot.bottom() + 2.0),
            egui::Align2::CENTER_TOP,
            format!("{at:.0}"),
            egui::FontId::proportional(10.0),
            label,
        );
        at += step;
    }
    // The two ends, named as they are on the map's ground track. Without them
    // a mirrored section is indistinguishable from the one that was drawn.
    painter.text(
        egui::pos2(layout.plot.left() + 2.0, layout.plot.bottom() + 2.0),
        egui::Align2::LEFT_TOP,
        "A",
        egui::FontId::proportional(11.0),
        super::SECTION_TRACK_COLOR,
    );
    painter.text(
        egui::pos2(layout.plot.right() - 2.0, layout.plot.bottom() + 2.0),
        egui::Align2::RIGHT_TOP,
        format!("B  ({})", prefs.distance.suffix()),
        egui::FontId::proportional(11.0),
        super::SECTION_TRACK_COLOR,
    );
}

/// The tilt ladder's bright dash.
///
/// A function rather than an inline literal so a harness test can ask which
/// segments in a painted pane are the ladder's. Three different things in this
/// pane are drawn with `line_segment` — the axis grid, the ladder's halo and the
/// ladder — and a test that could not tell them apart could not tell a missing
/// ladder from a grid with more ticks on it.
pub(crate) fn tilt_rung_color() -> egui::Color32 {
    egui::Color32::from_rgba_unmultiplied(255, 255, 255, 130)
}

/// The dark halo drawn under [`tilt_rung_color`]. See its use for why alpha
/// alone does not survive a 65 dBZ core.
pub(crate) fn tilt_rung_halo() -> egui::Color32 {
    egui::Color32::from_rgba_unmultiplied(0, 0, 0, 90)
}

/// Trace each elevation cut's beam centre across the section.
///
/// **This is the honest device.** See the module documentation: a section is a
/// picture of the tilt ladder, and these curves are where the ladder actually
/// is. Data exists along them; everything between is two-point interpolation in
/// beam height, and the curves fan apart with range at exactly the rate the
/// interpolation error grows.
///
/// `elevations` is [`CrossSection::tilt_elevations_deg`] — **the section's own
/// ladder**, not one looked up beside it. That is a correctness property rather
/// than a convenience: these curves say "the data is here", so drawing them
/// from any list that could differ from the one the raster was sampled at is a
/// fabrication, and a more convincing one than drawing nothing.
///
/// It used to be looked up, from `ScanInfo::product_elevations`, guarded by a
/// count comparison against `axes.tilt_count`. The two lists count different
/// things — `ScanInfo` rounds each sweep's median to 0.1° and dedups, the
/// sampler groups by the cut table's nominal angle — so they disagreed whenever
/// two sweeps of one cut had medians straddling an `x.x5` boundary. The guard
/// then did the only safe thing and drew nothing: measured dark on five of ten
/// complete VCP 212/215 reflectivity volumes across 19 sites, and on 20 of 23
/// mid-volume fill states at KLNX. The honesty device was absent exactly where
/// a coarse ladder makes it matter.
#[allow(clippy::too_many_arguments)]
fn paint_tilt_ladder(
    painter: &egui::Painter,
    layout: &SectionLayout,
    axes: &SectionAxes,
    a: (f64, f64),
    b: (f64, f64),
    site_lat: f64,
    site_lon: f64,
    elevations: &[f64],
) {
    let Some(curves) = tilt_curves(layout, axes, a, b, site_lat, site_lon, elevations) else {
        return;
    };
    // A dark halo under a bright dash, the same trick the ground track uses.
    // Alpha alone does not survive a 65 dBZ core: a faint white line over red
    // disappears exactly where the section most needs to say that its vertical
    // extent is the ladder's rather than the storm's.
    let halo = tilt_rung_halo();
    let color = tilt_rung_color();
    let painter = painter.with_clip_rect(layout.plot);

    for points in curves {
        // Dashed rather than solid: a solid line over a reflectivity core reads
        // as a boundary *in the data*, which is the one thing these must not be
        // mistaken for.
        for pair in points.windows(2) {
            let mid = pair[0] + (pair[1] - pair[0]) * 0.55;
            painter.line_segment([pair[0], mid], egui::Stroke::new(3.0, halo));
            painter.line_segment([pair[0], mid], egui::Stroke::new(1.0, color));
        }
    }
}

/// Where each rung's beam centre crosses the section, in pane coordinates —
/// one polyline per elevation, ascending with the ladder.
///
/// Split out from the painting so the geometry has something to be tested
/// against.
///
/// `None` only for a ladder with no rungs — a section of a volume that carried
/// no cut of this moment, which the caption already reports in red and which
/// has no curve to draw anyway.
///
/// There is deliberately **no** agreement check any more. The elevations are
/// the section's own (see [`paint_tilt_ladder`]), and
/// [`CrossSection::from_parts`] refuses a section whose ladder is not
/// `tilt_count` long — so "these angles belong to this raster" is an invariant
/// of the type rather than a comparison made here, and a comparison made here
/// could only ever be wrong in the direction of silence.
#[allow(clippy::too_many_arguments)]
fn tilt_curves(
    layout: &SectionLayout,
    axes: &SectionAxes,
    a: (f64, f64),
    b: (f64, f64),
    site_lat: f64,
    site_lon: f64,
    elevations: &[f64],
) -> Option<Vec<Vec<egui::Pos2>>> {
    if elevations.is_empty() {
        return None;
    }
    // The ground range of each sample point, computed once and shared by every
    // rung: it is a property of the line and the site, not of the elevation.
    let ranges: Vec<(f32, f64)> = (0..=TILT_CURVE_SAMPLES)
        .map(|i| {
            let t = i as f64 / TILT_CURVE_SAMPLES as f64;
            let (lat, lon) = beam::great_circle_point(a, b, t);
            let (_, ground_km) = beam::site_bearing_range_km(site_lat, site_lon, lat, lon);
            (layout.x_of_distance(axes, t * axes.length_km), ground_km)
        })
        .collect();

    Some(
        elevations
            .iter()
            .map(|&elev| {
                ranges
                    .iter()
                    .map(|&(x, ground_km)| {
                        let km_msl = axes.base_km_msl + beam::height_at_ground_km(ground_km, elev);
                        egui::pos2(x, layout.y_of_height(axes, km_msl))
                    })
                    .collect()
            })
            .collect(),
    )
}

/// Whether the ladder this section was cut from reached the top of the
/// coverage pattern it belongs to.
///
/// The one predicate behind two user-visible claims — the caption's wording and
/// what a hover says about a blank pixel above the volume — because they are the
/// same fact and two copies of it would eventually disagree, leaving a pane that
/// captions itself as truncated and then explains its own ceiling as the cone of
/// silence.
///
/// Both numbers come off the same cut table (see [`SectionAxes::top_tilt_deg`]),
/// so this is an exact comparison and not a tolerance. `>=` rather than `==`
/// because a section that arrived over a wire has had no such promise made about
/// it, and "the ladder is somehow above the pattern" is not a truncation.
fn ladder_reaches_pattern_top(axes: &SectionAxes) -> bool {
    axes.top_tilt_deg >= axes.top_declared_cut_deg
}

/// One line of the caption, before it is laid out.
struct CaptionLine {
    text: String,
    color: egui::Color32,
    size: f32,
    /// Whether [`lay_out_caption`] may drop this line to fit the pane.
    ///
    /// The default line and the status line always survive: a pane whose
    /// caption vanished entirely names nothing, and a transient failure that
    /// could be squeezed off screen is one the user never learns about. The
    /// ⓘ detail lines are the droppable ones — they were asked for, and on a
    /// pane too small to hold them the alternative is eating the picture they
    /// describe.
    essential: bool,
}

/// The caption: one calm line saying what this is, and — behind the ⓘ — the
/// detail saying what it is not.
///
/// # The default line is deliberately minimal
///
/// Product, tilt count, top angle, volume time. Watched with real users, the
/// previous caption — a red paragraph of ladder numbers and a registration
/// caveat, on every ordinary section — read as an error state they had caused.
/// The module documentation carries the full account; the short version is that
/// a warning shown on almost every volume in error styling is an alarm people
/// learn to skip, which serves honesty worse than a calm line with the facts
/// one tap away.
///
/// # What the colours mean now
///
/// **Red is reserved for genuinely broken states**: a cut that failed, a volume
/// carrying no data for this moment. A filling or AVSET-ended volume is the
/// ordinary case — measured live at KLNX, *every* VCP 212 volume in a
/// ten-minute window topped out between 6.4° and 8.0° of a declared 19.5° — so
/// it is captioned in the same quiet colour as a complete one. Pinned by
/// `red_is_reserved_for_broken_states`.
///
/// Built as text before anything is measured or drawn, because the caption's
/// height decides where the plot starts and the caption's height is only known
/// once its text has been wrapped to the pane's width.
fn caption_lines(
    axes: &SectionAxes,
    product: RadarProduct,
    collected: Option<chrono::NaiveDateTime>,
    unavailable: Option<crate::pane::SectionUnavailable>,
    detail_open: bool,
    visuals: &egui::Visuals,
    prefs: &UserPreferences,
) -> Vec<CaptionLine> {
    let mut lines = Vec::new();
    let calm = visuals.weak_text_color();

    // The volume time in the user's zone, because "how fresh is this picture"
    // is the third thing the default line owes — a section is cut once per
    // volume and a storm outruns one in minutes.
    let stamp = collected.map_or_else(String::new, |t| {
        format!("  \u{b7}  {}", prefs.timezone.format_naive_utc(t, "%H:%M"))
    });

    let (headline, headline_color) = match axes.tilt_count {
        // No data for this moment at all is a genuinely broken picture, and the
        // one ladder state red is still for: nothing below was measured, and no
        // amount of waiting on *this* volume changes that.
        0 => (
            format!(
                "{}  \u{2014}  no tilts: this volume carried none of this product, so \
                 nothing below was measured",
                product.name(),
            ),
            visuals.error_fg_color,
        ),
        // One rung is the worst *picture* there is — a single conical surface
        // smeared over the pane — but it is a routine early-volume state, not a
        // fault, so it says what it is in the calm colour rather than shouting.
        1 => (
            format!(
                "{}  \u{2014}  one tilt: a single scanned surface, not a vertical profile",
                product.name(),
            ),
            calm,
        ),
        // The ordinary case, complete or filling alike: the count and the top
        // angle are the ladder's own numbers, compact enough to stay calm and
        // exact enough that a reader who knows what 8.0° of a VCP means loses
        // nothing to the redesign. Which pattern the volume flies, and where
        // this one stopped against it, is the ⓘ detail's to explain.
        rungs => (
            format!(
                "{}  \u{2014}  {rungs} tilts to {:.1}\u{b0}{stamp}",
                product.name(),
                axes.top_tilt_deg,
            ),
            calm,
        ),
    };
    lines.push(CaptionLine {
        text: headline,
        color: headline_color,
        size: 11.0,
        essential: true,
    });

    if detail_open {
        detail_lines(axes, visuals, prefs, &mut lines);
    }

    // Last, under everything, so a transient state never pushes the caption off
    // the top of the pane. **Red only when the state is broken**: a failed cut
    // is a dead end, but a volume still downloading or a pattern still on its
    // way resolve themselves, and painting the ordinary path to a picture in
    // error styling is exactly what the redesign exists to stop.
    if let Some(reason) = unavailable {
        let broken = matches!(reason, crate::pane::SectionUnavailable::RenderFailed);
        lines.push(CaptionLine {
            text: if broken {
                format!("\u{26a0} {}", reason.message())
            } else {
                reason.message()
            },
            color: if broken { visuals.error_fg_color } else { calm },
            size: 10.0,
            essential: true,
        });
    }

    lines
}

/// The ⓘ detail: the long-form honesty, in the user's words and units.
///
/// Three facts, one line each, every one droppable on a pane too small to hold
/// it ([`CaptionLine::essential`]):
///
/// 1. **Where the ladder stopped**, when it stopped short of its pattern. The
///    cause is deliberately not named — filling, abandoned and AVSET read the
///    same from one volume — and the ceiling height is quoted **only when it is
///    on the chart**: the old caption quoted the top of coverage at maximum
///    range, ≈114.5 kft against an axis ending near 65, a number no echo could
///    be drawn at and therefore pure alarm.
/// 2. **Where the interpolation lives**, with the ladder's own gap numbers so
///    the sentence is a measurement of this volume rather than boilerplate.
///    These are the figures the old caption led with; they moved here because
///    mid-volume they *flatter* — a four-rung ladder's "widest gap 0.5°" reads
///    better than a complete VCP 212's "4.9°" — so they inform alongside the
///    truncation line rather than standing in for it.
/// 3. **Why echoes sit off the map's track**, described as what the user sees.
///    The old spelling ended "The section is right", which is this module
///    arguing with its sibling in front of the user; which renderer applies
///    the beam model is a fact for `render_gate`'s docs, not for the pane.
fn detail_lines(
    axes: &SectionAxes,
    visuals: &egui::Visuals,
    prefs: &UserPreferences,
    lines: &mut Vec<CaptionLine>,
) {
    let color = visuals.weak_text_color();
    let mut push = |text: String| {
        lines.push(CaptionLine {
            text,
            color,
            size: 10.0,
            essential: false,
        });
    };

    if axes.tilt_count >= 2 && !ladder_reaches_pattern_top(axes) {
        let mut text = format!(
            "The radar scanned to {:.1}\u{b0} this volume, of the {:.1}\u{b0} its pattern \
             can reach \u{2014} air above the top dotted curve was not sampled. That is \
             ordinary: scans often stop climbing once storm tops sit below the remaining \
             tilts, and the picture fills in if more arrive.",
            axes.top_tilt_deg, axes.top_declared_cut_deg,
        );
        // The ceiling as a height, because "1.8°" is not a height and a
        // forecaster reading a section is reading heights — but **only when the
        // height is on the pane's own axis**. Above it, the figure describes a
        // place the picture cannot show, and a warning about an off-chart
        // number is indistinguishable from a bug.
        let ceiling_km_msl = axes.base_km_msl
            + beam::height_at_ground_km(axes.coverage_ground_range_km, axes.top_tilt_deg);
        if ceiling_km_msl <= axes.top_km_msl {
            let ceiling_shown = match prefs.height {
                rustdar_units::HeightUnit::Feet => ceiling_km_msl * KM_TO_KFT,
                rustdar_units::HeightUnit::Meters => ceiling_km_msl,
            };
            text.push_str(&format!(
                " The picture's ceiling is \u{2248}{:.0} {} MSL at the far end of the line.",
                ceiling_shown,
                prefs.height.kilo_suffix(),
            ));
        }
        push(text);
    }

    if axes.tilt_count >= 2 {
        let widest_gap_km = axes.widest_tilt_gap_deg.to_radians() * axes.coverage_ground_range_km;
        push(format!(
            "Data exists along the dotted curves \u{2014} one per tilt \u{2014} and the \
             picture between them is interpolated: layer depth and echo tops are set by \
             the tilt spacing, not measured. The widest step here is {:.1}\u{b0}, \
             \u{2248}{:.0}{} at {:.0}{}.",
            axes.widest_tilt_gap_deg,
            prefs.distance.convert_from_km(widest_gap_km),
            prefs.distance.suffix(),
            prefs
                .distance
                .convert_from_km(axes.coverage_ground_range_km),
            prefs.distance.suffix(),
        ));
    }

    let mut registration = String::from(
        "Echoes can sit a little off the line drawn on the map, most visibly on high \
         tilts: the map flattens every tilt to the ground, while this section keeps \
         the beam's height.",
    );
    // Only when it is true of *this* section: a line that ran out of data half
    // way along is a fact about the picture, and saying it always would make it
    // noise.
    if axes.coverage_ground_range_km + 0.5 < axes.far_ground_range_km {
        registration.push_str(&format!(
            "  Data ends {:.0}{} of {:.0}{} along the line.",
            prefs
                .distance
                .convert_from_km(axes.coverage_ground_range_km),
            prefs.distance.suffix(),
            prefs.distance.convert_from_km(axes.far_ground_range_km),
            prefs.distance.suffix(),
        ));
    }
    push(registration);
}

/// Wrap the caption to the pane's width, dropping detail lines — last first —
/// if what is left would eat the picture.
///
/// The measurement is what makes the caption honest at every pane shape. Before
/// it, the caption was drawn with `Painter::text` and no wrap width and the band
/// was *counted* at one row per line — so on a wide pane in a 2×2 split a
/// caption line ran flush to the pane's edge and was clipped mid-sentence, and
/// there was nothing in the layout that could have noticed.
///
/// Only lines marked droppable are dropped ([`CaptionLine::essential`]), and
/// whole lines rather than rows: a sentence cut off mid-clause reads as a
/// rendering fault. If every droppable line is gone and the essentials still
/// overrun the budget, the essentials are kept anyway — a caption that names
/// nothing is worse than a plot that starts low.
fn lay_out_caption(
    painter: &egui::Painter,
    pane_rect: egui::Rect,
    mut lines: Vec<CaptionLine>,
) -> Vec<std::sync::Arc<egui::Galley>> {
    let wrap = caption_wrap_width(pane_rect);
    let layout_all = |lines: &[CaptionLine]| -> Vec<std::sync::Arc<egui::Galley>> {
        lines
            .iter()
            .map(|l| {
                painter.layout(
                    l.text.clone(),
                    egui::FontId::proportional(l.size),
                    l.color,
                    wrap,
                )
            })
            .collect()
    };
    let total = |galleys: &[std::sync::Arc<egui::Galley>]| -> f32 {
        galleys.iter().map(|g| g.rect.height()).sum()
    };

    let budget = pane_rect.height() * CAPTION_MAX_HEIGHT_FRACTION;
    let mut galleys = layout_all(&lines);
    while total(&galleys) > budget {
        // Last droppable line first: the detail lines are ordered most- to
        // least-load-bearing, so the registration note goes before the
        // interpolation note goes before the truncation note.
        let Some(idx) = lines.iter().rposition(|l| !l.essential) else {
            return galleys;
        };
        lines.remove(idx);
        galleys = layout_all(&lines);
    }
    galleys
}

/// What the status bar says about the pixel under the pointer.
///
/// # Most of the value of the status plane is here
///
/// The sampler propagates seven reasons a pixel has no number, and this is the
/// first place in the codebase that can *say* one. A blank region of a section
/// is not one thing: below the lowest beam is a permanent blind spot near the
/// ground, above the volume is either the cone of silence or air the antenna
/// has not reached yet, beyond range is an upper cut that stops short, range
/// folded is a real echo at an ambiguous distance, and below threshold is the
/// radar looking and finding nothing. Those are six completely different facts
/// and they are painted identically.
///
/// It is also the only place that can *tell* the two halves of `AboveVolume`
/// apart — the sampler raises one status for both — which is why
/// [`describe_missing`] takes the ladder's completeness as well as the status.
fn hover_readout(
    section: &CrossSection,
    layout: &SectionLayout,
    pos: egui::Pos2,
    product: RadarProduct,
    prefs: &UserPreferences,
) -> Option<String> {
    if !layout.plot.contains(pos) {
        return None;
    }
    let axes = section.axes();
    let col_frac = (pos.x - layout.plot.left()) / layout.plot.width();
    let row_frac = (pos.y - layout.plot.top()) / layout.plot.height();
    let col = ((col_frac * SECTION_WIDTH as f32) as usize).min(SECTION_WIDTH - 1);
    let row = ((row_frac * SECTION_HEIGHT as f32) as usize).min(SECTION_HEIGHT - 1);
    let sample = section.sample(col, row)?;

    let along = prefs.distance.convert_from_km(axes.column_distance_km(col));
    let height_km = axes.row_height_km_msl(row);
    let height_shown = match prefs.height {
        rustdar_units::HeightUnit::Feet => height_km * KM_TO_KFT,
        rustdar_units::HeightUnit::Meters => height_km,
    };

    let value = match sample.value() {
        Some(value) => product.format_value(value, prefs),
        None => describe_missing(sample.status(), ladder_reaches_pattern_top(axes)).to_owned(),
    };
    Some(format!(
        "{:.1}{} along  |  {:.1} {} MSL  |  {}",
        along,
        prefs.distance.suffix(),
        height_shown,
        prefs.height.kilo_suffix(),
        value,
    ))
}

/// Why there is no number here, in words a forecaster would use.
///
/// [`SampleStatus::Value`] is unreachable — [`hover_readout`] only asks after
/// [`rustdar_radar::sampler::Sample::value`] has answered `None` — and is given
/// a phrase anyway rather than a `unreachable!`, because the cost of being wrong
/// about that is a panic on the main thread, which under wasm takes the tab
/// down.
///
/// # Why `AboveVolume` needs a second question asked
///
/// The sampler raises it for one condition — the query height is above the top
/// rung's beam — and that condition covers two situations a forecaster would
/// never confuse. Over the site, a complete volume's highest cut is still nearly
/// horizontal, so everything above it is the **cone of silence**: a permanent
/// property of how a radar scans, and a real explanation. Away from the site,
/// the top rung's beam is kilometres up, and a pixel lands above it only because
/// the ladder stopped early — a live volume four rungs into its flight tops out
/// at 1.8°, which at 100 km is under 3 km, so *everything* above 3 km at that
/// range is `AboveVolume`. Calling that the cone of silence at 10 km and 100 km
/// is flatly false, and it is the worst kind of false: a confident
/// meteorological explanation for a blank region, and the wrong one. The user
/// stops looking.
///
/// `ladder_reaches_top` is [`ladder_reaches_pattern_top`], the same predicate
/// the caption's wording turns on.
fn describe_missing(status: SampleStatus, ladder_reaches_top: bool) -> &'static str {
    match status {
        SampleStatus::Value => "no value",
        SampleStatus::BelowThreshold => "below threshold (the radar looked and saw nothing)",
        SampleStatus::RangeFolded => "range folded (a real echo, at an ambiguous distance)",
        SampleStatus::BelowLowestBeam => "below the lowest beam",
        SampleStatus::AboveVolume if ladder_reaches_top => "above the volume (cone of silence)",
        SampleStatus::AboveVolume => {
            "above the highest tilt this volume flew \u{2014} not the cone of silence: the \
             scan ended below its pattern's top"
        }
        SampleStatus::BeyondRange => "beyond this tilt's range",
        SampleStatus::NoCoverage => "no coverage",
    }
}

/// A 1/2/5-times-a-power-of-ten step that puts roughly `wanted` ticks across
/// `span`.
///
/// Always positive and always finite, whatever it is handed: a zero or
/// non-finite span would otherwise produce a step of zero and turn the tick
/// loops above into infinite ones — on the frame thread.
fn nice_step(span: f64, wanted: f64) -> f64 {
    if !span.is_finite() || span <= 0.0 || !wanted.is_finite() || wanted < 1.0 {
        // A literal, not `span.max(1.0)`: `f64::INFINITY.max(1.0)` is infinity,
        // and an infinite step makes the tick loop draw one label and stop —
        // which looks like an axis with a bug rather than like an axis with a
        // degenerate input, and hides the real problem upstream.
        return 1.0;
    }
    let raw = span / wanted;
    let magnitude = 10f64.powf(raw.log10().floor());
    let normalized = raw / magnitude;
    let step = if normalized <= 1.0 {
        1.0
    } else if normalized <= 2.0 {
        2.0
    } else if normalized <= 5.0 {
        5.0
    } else {
        10.0
    };
    (step * magnitude).max(f64::MIN_POSITIVE)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A measured caption height standing in for one wrapped row, for the tests
    /// that are about the *rest* of the layout. Real ones come from
    /// [`lay_out_caption`], which needs fonts.
    const ONE_LINE: f32 = 15.0;
    /// Two wrapped rows, for the tests about the caption taking room.
    const TWO_LINES: f32 = 30.0;

    fn axes() -> SectionAxes {
        SectionAxes {
            length_km: 100.0,
            base_km_msl: 0.4,
            top_km_msl: 20.4,
            near_ground_range_km: 10.0,
            far_ground_range_km: 110.0,
            coverage_ground_range_km: 110.0,
            cone_of_silence_km: 0.0,
            tilt_count: 14,
            widest_tilt_gap_deg: 4.9,
            top_tilt_deg: 19.5,
            top_declared_cut_deg: 19.5,
        }
    }

    /// VCP 212's reflectivity ladder as KTLX really flies it, in the sampler's
    /// own median angles rather than in round numbers — the shape a section
    /// arrives carrying.
    const VCP_212: [f64; 14] = [
        0.4834, 0.8789, 1.3184, 1.8018, 2.4170, 3.1201, 4.0430, 5.0977, 6.4160, 8.0273, 10.0195,
        12.5000, 15.6006, 19.5117,
    ];

    /// The two mappings are inverses of the raster's own convention: row 0 is
    /// the **top**, so the top of the axis is the top of the plot.
    ///
    /// Getting this upside down is the single most likely mistake in the
    /// module and the least likely to be noticed — a flipped section of a
    /// mature storm still looks like a storm.
    #[test]
    fn the_top_of_the_axis_is_the_top_of_the_plot() {
        let rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(400.0, 300.0));
        let layout = SectionLayout::new(rect, ONE_LINE, false);
        let axes = axes();

        assert_eq!(
            layout.y_of_height(&axes, axes.top_km_msl),
            layout.plot.top()
        );
        assert_eq!(
            layout.y_of_height(&axes, axes.base_km_msl),
            layout.plot.bottom()
        );
        assert!(
            layout.y_of_height(&axes, 15.0) < layout.y_of_height(&axes, 5.0),
            "a higher height must be nearer the top of the screen"
        );

        assert_eq!(layout.x_of_distance(&axes, 0.0), layout.plot.left());
        assert_eq!(
            layout.x_of_distance(&axes, axes.length_km),
            layout.plot.right()
        );
    }

    /// A degenerate axis must not divide by zero. `render_section` refuses one,
    /// so this is about a section that arrived over a wire and about the
    /// mappings being total rather than about a state production reaches.
    #[test]
    fn a_degenerate_axis_maps_to_the_edges_rather_than_to_nan() {
        let rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(400.0, 300.0));
        let layout = SectionLayout::new(rect, ONE_LINE, false);
        let flat = SectionAxes {
            length_km: 0.0,
            top_km_msl: 0.4,
            ..axes()
        };
        assert_eq!(layout.y_of_height(&flat, 1.0), layout.plot.bottom());
        assert_eq!(layout.x_of_distance(&flat, 1.0), layout.plot.left());
    }

    /// `nice_step` is what the two tick loops advance by, so a step of zero or
    /// `NaN` is not a cosmetic bug — it is an infinite loop on the frame
    /// thread, which on wasm is the whole application.
    #[test]
    fn a_tick_step_is_always_a_positive_finite_number() {
        for span in [0.0, -1.0, f64::NAN, f64::INFINITY, 1e-9, 20.0, 65_000.0] {
            for wanted in [0.0, 0.5, 1.0, 8.0, f64::NAN, f64::INFINITY] {
                let step = nice_step(span, wanted);
                assert!(
                    step.is_finite() && step > 0.0,
                    "nice_step({span}, {wanted}) = {step}"
                );
            }
        }
    }

    /// Every reason a pixel is blank has its own words. Collapsing any two of
    /// them loses the distinction the status plane exists to carry — and the
    /// pair most worth keeping apart is `BelowThreshold` (the radar looked and
    /// saw nothing) against `NoCoverage` (the radar never looked).
    ///
    /// `AboveVolume` is **seven** reasons' worth of one status and it has to
    /// read as two, because the sampler cannot tell them apart and the section
    /// can. See [`describe_missing`].
    #[test]
    fn every_blank_reason_reads_differently() {
        let all = [
            SampleStatus::BelowThreshold,
            SampleStatus::RangeFolded,
            SampleStatus::BelowLowestBeam,
            SampleStatus::AboveVolume,
            SampleStatus::BeyondRange,
            SampleStatus::NoCoverage,
        ];
        for complete in [true, false] {
            let mut seen: Vec<&str> = all
                .iter()
                .copied()
                .map(|status| describe_missing(status, complete))
                .collect();
            seen.sort_unstable();
            let before = seen.len();
            seen.dedup();
            assert_eq!(
                before,
                seen.len(),
                "two blank reasons read the same: {seen:?}"
            );
        }
    }

    /// **A volume that has not been flown is not the cone of silence.**
    ///
    /// One sampler status, two facts. Over the site, above a *complete* volume's
    /// highest cut, is the cone of silence: a permanent property of how a radar
    /// scans, and a real answer. Above a ladder that stopped at 1.8° because the
    /// antenna has not got there yet is unscanned air — at 100 km that is
    /// everything over about 3 km, which live is most of the pane. Naming the
    /// second as the first is not vague, it is a confident meteorological
    /// explanation that is wrong, and the user stops looking.
    #[test]
    fn air_the_antenna_never_reached_is_not_called_the_cone_of_silence() {
        let complete = describe_missing(SampleStatus::AboveVolume, true);
        let truncated = describe_missing(SampleStatus::AboveVolume, false);

        assert!(
            complete.contains("cone of silence"),
            "a complete volume's ceiling really is the cone of silence: {complete}"
        );
        assert_ne!(
            complete, truncated,
            "a volume that stopped short explains its own ceiling exactly as a \
             complete one does"
        );
        assert!(
            !truncated.contains("(cone of silence)"),
            "unscanned air was named as the cone of silence: {truncated}"
        );
        assert!(
            truncated.contains("not the cone of silence"),
            "the wrong answer is the one a forecaster will reach for on their \
             own, so it has to be refused by name: {truncated}"
        );

        // And the predicate is the caption's, so the pane cannot label itself
        // truncated in words and then explain its ceiling as the cone of
        // silence three centimetres below.
        let flying = SectionAxes {
            top_tilt_deg: 1.8,
            top_declared_cut_deg: 19.5,
            ..axes()
        };
        assert!(ladder_reaches_pattern_top(&axes()));
        assert!(!ladder_reaches_pattern_top(&flying));
    }

    /// The caption band shrinks on a short pane, and the picture never
    /// collapses to nothing.
    ///
    /// A **runtime** decision on the rect, so one wasm binary serves a phone in
    /// portrait and a desktop browser — pinned because `cfg!(target_os)` is the
    /// tempting wrong answer and would compile.
    #[test]
    fn a_short_pane_drops_the_second_caption_line_and_keeps_a_picture() {
        let rect =
            |w: f32, h: f32| egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(w, h));

        assert!(SectionLayout::new(rect(600.0, 400.0), TWO_LINES, false).labelled_axes);

        let short = SectionLayout::new(rect(600.0, 200.0), ONE_LINE, false);
        assert!(short.labelled_axes);
        assert!(short.plot.height() > 0.0);

        let tiny = SectionLayout::new(rect(300.0, 110.0), ONE_LINE, false);
        assert!(!tiny.labelled_axes, "no room for labels at 110 points");
        assert!(
            tiny.plot.left() < tiny.plot.right(),
            "the picture must not be squeezed out by its own gutters"
        );
    }

    /// The height axis's unit label gets its own room, rather than being drawn
    /// upward over the last line of the caption.
    ///
    /// `paint_axes` writes `MSL kft` bottom-aligned on `plot.top() - 2.0`, in the
    /// left gutter — the same strip of pane the caption's left edge occupies. It
    /// was overdrawn in every screenshot the feature ever produced, and only when
    /// there are axis labels at all, which is why the reservation is conditional
    /// on the same predicate the labels are.
    #[test]
    fn the_axis_unit_label_has_room_above_the_plot() {
        let rect = |h: f32| egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(600.0, h));

        let labelled = SectionLayout::new(rect(400.0), ONE_LINE, false);
        assert!(labelled.labelled_axes, "precondition");
        // 10 pt text bottom-aligned two points above the plot: its top sits at
        // `plot.top() - 2 - height`, which has to clear the caption.
        assert!(
            labelled.plot.top() - 2.0 - 10.0 >= labelled.caption.bottom(),
            "the MSL unit label is drawn over the caption: plot top {}, caption \
             bottom {}",
            labelled.plot.top(),
            labelled.caption.bottom()
        );

        // And a pane with no axis labels does not pay for a label it never draws.
        let bare = SectionLayout::new(rect(110.0), ONE_LINE, false);
        assert!(!bare.labelled_axes, "precondition");
        assert!(
            bare.plot.top() - bare.caption.bottom() < AXIS_UNIT_HEADROOM,
            "room was reserved for a label this pane has no room to draw"
        );
    }

    /// The caption is **wrapped and then measured**, so no sentence in it is ever
    /// clipped and no wrapped row is ever painted over the picture.
    ///
    /// Both halves matter and they fail in different places. Before the wrap, a
    /// caption line was drawn with `Painter::text` and ran flush to the pane's
    /// edge on a 2×2 split of a wide window — the clip cut it mid-sentence.
    /// Before the measurement, the band was *counted* at one row per line, so
    /// any wrap at all landed on the plot.
    ///
    /// Driven with the ⓘ detail **open**, which is the longest shape the
    /// caption takes: a truncated ladder over stopped-short coverage puts every
    /// detail line in, each with its extra clause.
    #[test]
    fn the_caption_wraps_and_the_layout_pays_for_the_rows_it_takes() {
        let ctx = egui::Context::default();
        // One frame, so the fonts exist to lay text out with.
        let _ = ctx.run_ui(egui::RawInput::default(), |_| {});
        let prefs = UserPreferences::default();
        let visuals = egui::Visuals::dark();
        // A ladder stopped short over coverage stopped short: every detail line
        // present, each with its appended clause — the longest the caption ever
        // gets, and the shape the clip was found on.
        let truncated = SectionAxes {
            coverage_ground_range_km: 64.0,
            top_tilt_deg: 6.4,
            ..axes()
        };

        let rect =
            |w: f32, h: f32| egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(w, h));
        let measure = |w: f32, h: f32| {
            let rect = rect(w, h);
            let painter = egui::Painter::new(ctx.clone(), egui::LayerId::debug(), rect);
            let galleys = lay_out_caption(
                &painter,
                rect,
                caption_lines(
                    &truncated,
                    RadarProduct::Reflectivity,
                    None,
                    None,
                    true,
                    &visuals,
                    &prefs,
                ),
            );
            let widest = galleys
                .iter()
                .map(|g| g.rect.width())
                .fold(0.0_f32, f32::max);
            let height: f32 = galleys.iter().map(|g| g.rect.height()).sum();
            (galleys.len(), widest, height)
        };

        // Nothing overruns the width it was wrapped to, at any pane shape, and
        // the plot always starts below every row the caption took.
        for (w, h) in [
            (1780.0f32, 900.0f32),
            (880.0, 500.0),
            (620.0, 500.0),
            (400.0, 400.0),
            (300.0, 400.0),
            (200.0, 300.0),
            (150.0, 300.0),
            (150.0, 700.0),
        ] {
            let (rows, widest, height) = measure(w, h);
            assert!(
                widest <= caption_wrap_width(rect(w, h)) + 0.5,
                "at {w}x{h} the caption ran {widest} points wide and was clipped"
            );
            assert!(
                height <= h * CAPTION_MAX_HEIGHT_FRACTION,
                "at {w}x{h} the caption ate {height} points of the pane"
            );
            let layout = SectionLayout::new(rect(w, h), height, false);
            assert!(
                layout.plot.top() >= layout.caption.top() + height,
                "at {w}x{h} the plot starts inside the {rows}-row caption above it"
            );
            assert!(layout.plot.height() > 0.0, "no picture left at {w}x{h}");
        }

        // The wrap really happens rather than every pane happening to fit: a
        // 620-point pane needs more rows than a 1780-point one, and pays for them.
        let (_, _, wide) = measure(1780.0, 900.0);
        let (_, _, medium) = measure(620.0, 500.0);
        assert!(
            medium > wide,
            "the caption did not wrap on a narrower pane ({medium} against {wide})"
        );

        // And when even the wrapped caption would eat the pane, whole detail
        // lines are dropped rather than a sentence being cut in half.
        let (rows_narrow, _, narrow) = measure(150.0, 300.0);
        let (rows_roomy, _, _) = measure(400.0, 400.0);
        assert!(
            rows_narrow < rows_roomy,
            "a caption with no room to wrap kept every line anyway"
        );
        assert!(narrow <= 300.0 * CAPTION_MAX_HEIGHT_FRACTION);
        // The default line survives every squeeze: the last thing a pane may
        // lose is its own name.
        let (rows_tiny, _, _) = measure(150.0, 120.0);
        assert!(
            rows_tiny >= 1,
            "the essential line was dropped to fit the budget"
        );

        // And a **status line survives the squeeze too**, even though it is
        // last in the vector: the droppable lines are the detail's, wherever
        // they sit, and a transient failure squeezed off screen is one the
        // user never learns about. This is the case that tells "drop
        // non-essential lines" from "drop from the end".
        let squeezed = {
            let rect = rect(150.0, 300.0);
            let painter = egui::Painter::new(ctx.clone(), egui::LayerId::debug(), rect);
            lay_out_caption(
                &painter,
                rect,
                caption_lines(
                    &truncated,
                    RadarProduct::Reflectivity,
                    None,
                    Some(crate::pane::SectionUnavailable::RenderFailed),
                    true,
                    &visuals,
                    &prefs,
                ),
            )
        };
        assert!(
            squeezed
                .iter()
                .any(|g| g.text().contains("could not be cut")),
            "the squeeze dropped the failure status instead of a detail line: {:?}",
            squeezed.iter().map(|g| g.text()).collect::<Vec<_>>()
        );
    }

    /// A one-rung ladder is the **worst** case, and the caption must not
    /// describe it in the ordinary case's words.
    ///
    /// `widest_tilt_gap_deg` is `0.0` for a single rung because there is no
    /// second rung to be apart from, so wording that reached for the general
    /// template would render "1 tilts" with a zero gap — which reads as perfect
    /// sampling. It was also the standing state of every live section before the
    /// staleness key learned to notice a volume filling.
    #[test]
    fn a_degenerate_ladder_does_not_report_itself_as_a_perfect_one() {
        let prefs = UserPreferences::default();
        let visuals = egui::Visuals::dark();
        let caption = |tilt_count: usize, widest_tilt_gap_deg: f64| {
            let axes = SectionAxes {
                tilt_count,
                widest_tilt_gap_deg,
                ..axes()
            };
            caption_lines(
                &axes,
                RadarProduct::Reflectivity,
                None,
                None,
                false,
                &visuals,
                &prefs,
            )
            .swap_remove(0)
        };

        // No tilts at all: nothing below the caption was measured, which is a
        // genuinely broken picture and the one ladder state red is still for.
        let empty = caption(0, 0.0);
        assert!(
            empty.text.contains("measured"),
            "an empty ladder has to say nothing was measured: {}",
            empty.text
        );
        assert_eq!(
            empty.color, visuals.error_fg_color,
            "a picture with no data behind it is a broken state"
        );

        // One tilt: the worst picture there is, and it says what it is — but in
        // the calm colour, because a volume one rung in is a routine state, not
        // a fault the user caused.
        let single = caption(1, 0.0);
        assert!(
            single.text.contains("not a vertical profile"),
            "a one-tilt section has to refuse the reading a user will make: {}",
            single.text
        );
        assert!(!single.text.contains("1 tilts"), "{}", single.text);
        assert_ne!(
            single.color, visuals.error_fg_color,
            "a filling volume's first rung is not an error"
        );

        for degenerate in [&empty, &single] {
            assert!(
                !degenerate.text.contains("widest gap"),
                "a ladder with nothing to be apart from reported a gap: {}",
                degenerate.text
            );
        }

        // The ordinary case names the ladder's own count and top angle — a
        // measurement of this volume, compact enough to stay calm.
        let ordinary = caption(14, 4.9);
        assert!(ordinary.text.contains("14 tilts"), "{}", ordinary.text);
        assert!(ordinary.text.contains("19.5"), "{}", ordinary.text);
        assert_ne!(
            ordinary.color, visuals.error_fg_color,
            "the ordinary case must not be styled as a fault"
        );
        // And the gap figures are the detail's, not the headline's: they are
        // exactly the numbers that flatter a truncated volume (see
        // `a_ladder_that_stopped_short_stays_calm_and_explains_on_request`).
        assert!(
            !ordinary.text.contains("widest gap"),
            "the default line took the detail's numbers back: {}",
            ordinary.text
        );
    }

    /// **A ladder that stopped short is captioned as the ordinary case it is**,
    /// and the truncation is explained — in the user's words, on request.
    ///
    /// The contract this replaces led with a red sentence on almost every
    /// volume, because AVSET ends a precipitation scan once echo tops are below
    /// the cuts that remain: measured live at KLNX, *every* VCP 212 volume in a
    /// ten-minute window topped out between 6.4° and 8.0° of a declared 19.5°.
    /// Watched with real users, that read as an error they had caused. The
    /// redesign's contract, pinned here:
    ///
    /// * the default line is **calm** — same colour as a complete volume's, no
    ///   error styling, no wall of text;
    /// * the default line still carries the ladder's top angle, so nothing is
    ///   hidden — a reader who knows what 1.8° means loses nothing;
    /// * the **detail**, when opened, names where the ladder stopped against
    ///   its pattern, blames nothing, and quotes the ceiling height **only when
    ///   it is on the chart**.
    #[test]
    fn a_ladder_that_stopped_short_stays_calm_and_explains_on_request() {
        let prefs = UserPreferences::default();
        let visuals = egui::Visuals::dark();
        let lines = |axes: SectionAxes, detail_open: bool| {
            caption_lines(
                &axes,
                RadarProduct::Reflectivity,
                None,
                None,
                detail_open,
                &visuals,
                &prefs,
            )
        };

        // KMPX four rungs into VCP 212, with the SAILS repeat already in: the
        // exact state the old caption was read in with users watching.
        let filling_axes = SectionAxes {
            tilt_count: 4,
            widest_tilt_gap_deg: 0.5,
            top_tilt_deg: 1.8,
            top_declared_cut_deg: 19.5,
            coverage_ground_range_km: 86.0,
            ..axes()
        };
        let complete_axes = SectionAxes {
            coverage_ground_range_km: 86.0,
            ..axes()
        };

        // --- The default is calm, and identical in styling to a complete
        // volume's: a state that is true of nearly every volume ever flown must
        // not be dressed as a fault.
        let filling = lines(filling_axes, false).swap_remove(0);
        let complete = lines(complete_axes, false).swap_remove(0);
        assert_ne!(
            filling.color, visuals.error_fg_color,
            "a filling volume is captioned in error styling: {}",
            filling.text
        );
        assert_eq!(
            filling.color, complete.color,
            "a filling volume is styled differently from a complete one, which \
             makes its ordinary state read as a state to worry about"
        );
        assert!(
            filling.text.contains("4 tilts to 1.8\u{b0}"),
            "the default line lost the ladder's own numbers: {}",
            filling.text
        );
        assert!(
            complete.text.contains("14 tilts to 19.5\u{b0}"),
            "a complete ladder does not say how high it reaches: {}",
            complete.text
        );
        // The long-form explanation is *not* in the default: it is the wall of
        // text users read as an error.
        for (line, name) in [(&filling, "filling"), (&complete, "complete")] {
            for leaked in ["pattern", "not measured", "interpolated", "MSL"] {
                assert!(
                    !line.text.contains(leaked),
                    "the {name} default line carries detail copy ({leaked:?}): {}",
                    line.text
                );
            }
        }
        assert_eq!(
            lines(filling_axes, false).len(),
            1,
            "a closed detail still contributed caption lines"
        );

        // --- The detail, opened, says where the ladder stopped — against what
        // the pattern flies, as one phrase, because which number is which is
        // the whole sentence. (Two independent `contains` cannot tell
        // "to 1.8° of the 19.5°" from its swap, which compiles and reads as a
        // ladder that overshot its pattern.)
        let opened = lines(filling_axes, true);
        let detail: String = opened
            .iter()
            .skip(1)
            .map(|l| l.text.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            detail.contains("scanned to 1.8\u{b0} this volume, of the 19.5\u{b0}"),
            "the detail did not name where the ladder stops against what the \
             pattern can reach, in that order: {detail}"
        );
        // Blame nothing: filling, abandoned and AVSET read the same from one
        // volume, and a guessed cause is wrong exactly when it sounds surest.
        for fault in ["cut short", "abandoned", "failed", "error"] {
            assert!(
                !detail.contains(fault),
                "the detail blames a scan for a ceiling AVSET puts there on \
                 purpose ({fault:?}): {detail}"
            );
        }
        // The interpolation truth is stated, with the ladder's own gap numbers,
        // and every detail line is in the calm colour.
        assert!(
            detail.contains("not measured"),
            "the detail no longer says what the picture is not: {detail}"
        );
        assert!(detail.contains("0.5\u{b0}"), "{detail}");
        for line in opened.iter().skip(1) {
            assert_ne!(
                line.color, visuals.error_fg_color,
                "a detail line is styled as an error: {}",
                line.text
            );
        }

        // --- The ceiling height appears only when it is on the chart.
        //
        // On: a 1.8° beam at 86 km is ~3 km MSL against a 20.4 km axis, and it
        // is given as a height because a forecaster reading a section is
        // reading heights, not degrees.
        let ceiling_km = 0.4 + beam::height_at_ground_km(86.0, 1.8);
        assert!(
            ceiling_km <= filling_axes.top_km_msl,
            "precondition: this ceiling is on the chart"
        );
        let kft = format!(
            "\u{2248}{:.0} {} MSL",
            ceiling_km * KM_TO_KFT,
            prefs.height.kilo_suffix()
        );
        assert!(
            detail.contains(&kft),
            "an on-chart ceiling was not quoted ({kft} expected): {detail}"
        );

        // Off: the old caption's absurdity — the top of coverage at maximum
        // range, ≈114.5 kft against an axis ending at ~67 — must never be
        // quoted. A figure the pane cannot show is pure alarm.
        let absurd_axes = SectionAxes {
            tilt_count: 9,
            top_tilt_deg: 8.0,
            top_declared_cut_deg: 19.5,
            coverage_ground_range_km: 225.0,
            ..axes()
        };
        let absurd_ceiling = 0.4 + beam::height_at_ground_km(225.0, 8.0);
        assert!(
            absurd_ceiling > absurd_axes.top_km_msl,
            "precondition: this ceiling is off the chart ({absurd_ceiling} km \
             against a {} km axis)",
            absurd_axes.top_km_msl
        );
        let absurd_detail: String = lines(absurd_axes, true)
            .iter()
            .skip(1)
            .map(|l| l.text.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            !absurd_detail.contains("MSL at the far end"),
            "an off-chart ceiling height was quoted — a number no echo could \
             ever be drawn at: {absurd_detail}"
        );
        assert!(
            absurd_detail.contains("scanned to 8.0\u{b0}"),
            "dropping the off-chart figure must not drop the truncation fact \
             itself: {absurd_detail}"
        );

        // --- And a complete volume's detail carries no truncation line at all:
        // there is nothing to explain, and a standing explanation would be the
        // old noise back under a new glyph.
        let complete_detail: String = lines(complete_axes, true)
            .iter()
            .skip(1)
            .map(|l| l.text.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            !complete_detail.contains("can reach"),
            "a complete ladder explains a truncation it does not have: \
             {complete_detail}"
        );
        assert!(
            complete_detail.contains("widest step here is 4.9\u{b0}"),
            "the complete detail lost the interpolation measurement: \
             {complete_detail}"
        );
    }

    /// **Red is reserved for genuinely broken states.**
    ///
    /// The one-sentence contract of issue #8: routine states — a volume
    /// filling, a volume AVSET ended, a first download in flight — are calm,
    /// and error styling is spent only where something is actually wrong, so
    /// that when it does appear it still means something.
    #[test]
    fn red_is_reserved_for_broken_states() {
        use crate::pane::SectionUnavailable;
        let prefs = UserPreferences::default();
        let visuals = egui::Visuals::dark();
        let lines = |axes: SectionAxes, unavailable: Option<SectionUnavailable>| {
            caption_lines(
                &axes,
                RadarProduct::Reflectivity,
                None,
                unavailable,
                false,
                &visuals,
                &prefs,
            )
        };

        // Routine ladders: never red.
        for (name, axes) in [
            ("complete", axes()),
            (
                "filling",
                SectionAxes {
                    tilt_count: 4,
                    top_tilt_deg: 1.8,
                    ..axes()
                },
            ),
            (
                "one rung",
                SectionAxes {
                    tilt_count: 1,
                    widest_tilt_gap_deg: 0.0,
                    ..axes()
                },
            ),
        ] {
            let line = lines(axes, None).swap_remove(0);
            assert_ne!(
                line.color, visuals.error_fg_color,
                "the {name} ladder is styled as an error: {}",
                line.text
            );
        }

        // No data at all for this moment: red, because nothing below the
        // caption was measured and waiting on this volume will not change it.
        let empty = lines(
            SectionAxes {
                tilt_count: 0,
                widest_tilt_gap_deg: 0.0,
                ..axes()
            },
            None,
        )
        .swap_remove(0);
        assert_eq!(empty.color, visuals.error_fg_color);

        // Transient states resolve themselves and are told calmly; a failed cut
        // is a dead end and is the one status line red is for.
        for (reason, broken) in [
            (SectionUnavailable::AwaitingVolume, false),
            (SectionUnavailable::AwaitingCoveragePattern, false),
            (
                SectionUnavailable::ProductHasNoVerticalStructure(
                    RadarProduct::VerticallyIntegratedLiquid,
                ),
                false,
            ),
            (SectionUnavailable::RenderFailed, true),
        ] {
            let all = lines(axes(), Some(reason));
            let status = all.last().expect("a status line was pushed");
            assert_eq!(
                status.color == visuals.error_fg_color,
                broken,
                "{reason:?} has the wrong styling: {}",
                status.text
            );
            // The warning glyph follows the same rule: a ⚠ on a routine state
            // is the old alarm back in miniature.
            assert_eq!(
                status.text.starts_with('\u{26a0}'),
                broken,
                "{reason:?} carries the wrong glyph: {}",
                status.text
            );
        }
    }

    /// **A real VCP 212 ladder draws.** The rungs are the section's first
    /// honesty device, and the way it failed was not a wrong line — it was no
    /// line at all, on half of every precipitation volume.
    ///
    /// # Why this test is the shape it is
    ///
    /// Its predecessor asserted a *refusal*: an eight-entry list against a
    /// nine-rung section drew nothing, because the pane looked the ladder up in
    /// `ScanInfo::product_elevations` and could not trust a list that disagreed.
    /// The refusal was correct, the test passed, and the feature was dark
    /// anyway — because the two lists count different things. `ScanInfo` rounds
    /// each sweep's median to 0.1\u{b0} and dedups; the sampler groups by the cut
    /// table's nominal angle. One cut flown twice with medians straddling an
    /// `x.x5` boundary becomes two entries for one rung. Measured at KLNX on a
    /// **complete** volume: 0.4834\u{b0} flown at 0.4394 and 0.4779, 0.8789\u{b0} flown
    /// at 0.8350 and 0.9229 — 16 against 14, refused. Across 19 sites, five of
    /// ten complete VCP 212/215 reflectivity volumes were dark; mid-volume, 20
    /// of 23 fill states at KLNX.
    ///
    /// So the ladder now arrives *with* the section, the refusal is gone, and
    /// what replaces it starts from the angles the failure was measured on. A
    /// synthetic ladder of round degrees would have drawn under the old code
    /// too, which is why the old test could not see any of this.
    #[test]
    fn a_real_tilt_ladder_draws_and_fans_apart_with_range() {
        let rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(800.0, 500.0));
        let layout = SectionLayout::new(rect, ONE_LINE, false);
        // KTLX, and a line running away from it, so the ground range along the
        // section really does change.
        let (site_lat, site_lon) = (35.3333, -97.2778);
        let a = (35.5, -96.5);
        let b = (36.2, -95.4);
        let axes = SectionAxes {
            tilt_count: VCP_212.len(),
            ..axes()
        };

        let curves = tilt_curves(&layout, &axes, a, b, site_lat, site_lon, &VCP_212)
            .expect("a complete VCP 212 reflectivity ladder must draw its rungs");
        assert_eq!(curves.len(), VCP_212.len(), "one polyline per rung");

        // Ascending: a higher elevation is a higher beam, which on screen is a
        // smaller y. Getting this inverted would draw the ladder upside down
        // over a correct picture.
        for pair in curves.windows(2) {
            assert!(
                pair[1][0].y < pair[0][0].y,
                "the rungs are not in ascending order of height"
            );
        }

        // And the gap between adjacent rungs **grows with range**, which is the
        // whole reason drawing them is honest rather than decorative: it is a
        // picture of the interpolation getting worse further out, at the place
        // in the section where it is getting worse.
        let near = curves[1][0].y - curves[0][0].y;
        let far = curves[1][TILT_CURVE_SAMPLES].y - curves[0][TILT_CURVE_SAMPLES].y;
        assert!(
            far.abs() > near.abs() * 1.2,
            "the rungs do not fan apart with range ({near} near, {far} far), so \
             the drawing says nothing about where the ladder is coarsest"
        );

        // The one refusal left, and it is not an agreement check: a volume that
        // carried no cut of this moment has no rung to draw, and its caption
        // already says so in red.
        assert!(
            tilt_curves(&layout, &axes, a, b, site_lat, site_lon, &[]).is_none(),
            "an empty ladder has no rungs to draw"
        );

        // A **mid-volume** ladder draws too, and that is the half the count
        // check got most wrong — KLNX refused 20 of 23 fill states, and a
        // partial ladder is precisely when a section interpolates furthest.
        let partial = &VCP_212[..4];
        let mid_flight = SectionAxes {
            tilt_count: partial.len(),
            ..axes
        };
        let curves = tilt_curves(&layout, &mid_flight, a, b, site_lat, site_lon, partial)
            .expect("a volume four cuts into its flight still has four real rungs");
        assert_eq!(curves.len(), partial.len());
    }

    /// A pane carrying a status line makes room for it rather than drawing it
    /// over the picture.
    #[test]
    fn a_status_line_takes_room_from_the_picture_not_from_the_warning() {
        let rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(600.0, 400.0));
        let without = SectionLayout::new(rect, ONE_LINE, false);
        let with = SectionLayout::new(rect, TWO_LINES, false);
        assert!(with.caption.height() > without.caption.height());
        assert!(with.plot.top() > without.plot.top());
        assert!(with.plot.height() < without.plot.height());
    }

    /// The plot leaves the colour bar its edge, whichever edge that is.
    ///
    /// `render_color_scale` is reused verbatim and paints straight onto the pane
    /// rect with no notion of what else is in there, so the *only* thing keeping
    /// the legend off the section is this inset. Which edge it takes is decided
    /// by the panel's shape, once for the whole grid, so both orientations have
    /// to be right.
    #[test]
    fn the_plot_leaves_room_for_whichever_edge_the_colour_bar_took() {
        let rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(800.0, 500.0));
        let vertical = SectionLayout::new(rect, ONE_LINE, false);
        let horizontal = SectionLayout::new(rect, ONE_LINE, true);

        assert!(
            rect.right() - vertical.plot.right() >= COLOR_SCALE_RESERVE,
            "a right-edge colour bar would be painted over the section"
        );
        assert!(
            rect.bottom() - horizontal.plot.bottom() >= COLOR_SCALE_RESERVE,
            "a bottom-edge colour bar would be painted over the section"
        );
        // And each orientation gives back the room the other one took, rather
        // than reserving both edges always.
        assert!(horizontal.plot.right() > vertical.plot.right());
        assert!(vertical.plot.bottom() > horizontal.plot.bottom());
    }
}
