//! Drawing a vertical cross-section, and being honest about what it is.

use crate::pane::PaneState;
use rustdar_radar::beam;
use rustdar_radar::sampler::SampleStatus;
use rustdar_radar::xsect::{CrossSection, SECTION_HEIGHT, SECTION_WIDTH, SectionAxes};
use rustdar_source::product::FieldId;
use rustdar_units::UserPreferences;

/// Width of the height-axis gutter, in points.
const HEIGHT_GUTTER: f32 = 44.0;
/// Height of the distance-axis gutter, in points.
const DISTANCE_GUTTER: f32 = 16.0;
/// Points a section needs before axis labels are worth the room they take.
const LABELLED_AXES_MIN_HEIGHT: f32 = 120.0;
/// The most of a pane's height the caption may take before detail lines are
/// dropped, last first, to make room.
const CAPTION_MAX_HEIGHT_FRACTION: f32 = 0.45;
/// Points reserved beside the caption for the ⓘ detail toggle, so the first line's
/// wrap never runs under the glyph that expands it.
const INFO_TOGGLE_RESERVE: f32 = 26.0;
/// Headroom between the caption and the plot, for the height axis's unit label.
const AXIS_UNIT_HEADROOM: f32 = 13.0;
/// How many points along the line each tilt curve is sampled at.
const TILT_CURVE_SAMPLES: usize = 64;

/// How far apart the ladder's oldest and newest rung must be before the caption
/// says so, in seconds.
const ASSEMBLY_SPAN_CAPTION_MIN_SECS: i64 = 300;

/// How old a rung must be, against the newest rung in the same section, before the
/// hover readout mentions its age at all.
const HOVER_AGE_MIN_SECS: i64 = 60;

/// Room left along whichever edge the colour bar took, in points.
const COLOR_SCALE_RESERVE: f32 = 64.0;

/// Draw a section pane: the picture, its axes, the tilt ladder over it, and the
/// caption that says what it is.
pub(super) fn render_cross_section(
    ui: &mut egui::Ui,
    pane: &mut PaneState,
    pane_rect: egui::Rect,
    top_clearance: f32,
    horizontal_color_scale: bool,
    prefs: &UserPreferences,
) {
    pane.hover_value = None;

    let product = pane.selected_product();
    let site = pane.scan_info.as_ref().map(|s| (s.site.lat, s.site.lon));

    // **Radar-addressed, and it stays that way** (WI-1 left this site for WI-6
    // to judge). A section pane draws radar's cross-section and nothing else —
    // `LoopFrameImage` has no non-radar section shape — so the loop this pane
    // animates is radar's by construction.
    let looping = pane.loop_state().is_active();
    let (state, line) = {
        let Some(state) = pane.cross_section() else {
            return;
        };
        let Some(line) = state.line else {
            paint_centered(ui, pane_rect, super::CROSS_SECTION_EMPTY_STATE);
            return;
        };
        (state, line)
    };
    let unavailable = state.unavailable.clone();
    let detail_open = state.detail_open;
    let live = (state.section.clone(), state.texture.clone());
    let live_collected = state.rendered_for.as_ref().map(|t| t.volume.collected);

    let (section, texture, axes, tilts, clocks, collected) = if looping {
        let Some(frame) = pane.active_section_image().cloned() else {
            paint_centered(ui, pane_rect, "Cutting the cross-section...");
            return;
        };
        let collected = pane.data_time_on_screen();
        (
            None,
            frame.texture,
            frame.axes,
            frame.tilt_elevations_deg,
            frame.tilt_collected_ms,
            collected,
        )
    } else {
        let (Some(section), Some(texture)) = live else {
            let message = state.unavailable.as_ref().map_or_else(
                || "Cutting the cross-section...".to_owned(),
                |u| u.message(),
            );
            paint_centered(ui, pane_rect, &message);
            return;
        };
        let axes = *section.axes();
        let tilts = section.tilt_elevations_deg().to_vec();
        let clocks = section.tilt_collected_ms().to_vec();
        (Some(section), texture, axes, tilts, clocks, live_collected)
    };
    let ladder = Ladder {
        elevations_deg: &tilts,
        collected_ms: &clocks,
    };

    let painter = ui.painter().with_clip_rect(pane_rect);
    let caption = lay_out_caption(
        &painter,
        pane_rect,
        horizontal_color_scale,
        caption_lines(
            &axes,
            &product,
            collected,
            ladder,
            unavailable,
            detail_open,
            ui.visuals(),
            prefs,
        ),
    );
    let caption_height = caption.iter().map(|g| g.rect.height()).sum();
    let layout = SectionLayout::new(
        pane_rect,
        top_clearance,
        caption_height,
        horizontal_color_scale,
    );

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
            &tilts,
        );
    }
    painter.rect_stroke(
        layout.plot,
        0.0,
        egui::Stroke::new(1.0, ui.visuals().weak_text_color()),
        egui::StrokeKind::Outside,
    );

    let mut y = layout.caption.top();
    let mut first_line_end = egui::pos2(layout.caption.left(), y);
    for (i, galley) in caption.into_iter().enumerate() {
        let height = galley.rect.height();
        if i == 0 {
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

    let toggle_color = if detail_open {
        ui.visuals().hyperlink_color
    } else {
        ui.visuals().weak_text_color()
    };
    // `ℹ` (U+2139) rather than the prettier `ⓘ` (U+24D8), and painted in the
    // **monospace** family on purpose: of egui's bundled fonts only Hack
    // carries the glyph — the proportional family has neither char, and a
    // tofu box is not an affordance. The M8 glyph audit (`ui_glyphs.rs`)
    // verifies exactly this pairing, in `MONO_ICON_GLYPHS`.
    let glyph_rect = painter.text(
        first_line_end,
        egui::Align2::LEFT_TOP,
        "\u{2139}",
        egui::FontId::monospace(12.0),
        toggle_color,
    );
    let response = ui
        .interact(
            glyph_rect.expand(4.0),
            ui.id().with("section_detail_toggle"),
            egui::Sense::click(),
        )
        .on_hover_text("What this picture is - and what it is not");
    if response.clicked()
        && let Some(state) = pane.cross_section_mut()
    {
        state.detail_open = !detail_open;
    }

    if let Some(new_line) = render_line_controls(ui, &painter, &layout, line, prefs)
        && let Some(state) = pane.cross_section_mut()
    {
        state.line = Some(new_line);
    }

    if let Some(pos) = ui.ctx().pointer_hover_pos()
        && pane_rect.contains(pos)
        && let Some(section) = section.as_deref()
    {
        pane.hover_value = hover_readout(
            section,
            &layout,
            pos,
            &product,
            site.map(|(lat, lon)| SectionSource {
                ladder,
                line,
                site_lat: lat,
                site_lon: lon,
            }),
            prefs,
        );
    }
}

/// One step-control button: a small dark chip with a glyph, clickable.
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

/// The section pane's own line controls: pan the line across itself, sweep it about
/// its midpoint, and read its bearing and length — GR2Analyst's Position slider, as
/// steps, plus the orientation that makes sweeping feel aimed.
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
    if layout.plot.width() < 4.0 * (button.x + gap) + 80.0 {
        return None;
    }

    let mut right = layout.plot.right() - 6.0;
    let mut chip = |glyph: &str, salt: &str, tooltip: &str| -> bool {
        let rect = egui::Rect::from_min_size(egui::pos2(right - button.x, top), button);
        right -= button.x + gap;
        control_chip(ui, painter, rect, glyph, salt, tooltip)
    };

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
        "\u{23f5}",
        "pan_right",
        "Slide the line to the right of its A-to-B direction",
    );
    let pan_left = chip(
        "\u{23f4}",
        "pan_left",
        "Slide the line to the left of its A-to-B direction",
    );

    let length_km = edit::length_km(line);
    painter.text(
        egui::pos2(right - 4.0, top + button.y * 0.5),
        egui::Align2::RIGHT_CENTER,
        line_readout(line, prefs),
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

/// The chip's text: the line's bearing and its ground length, the latter **in the
/// user's own distance unit**.
fn line_readout(line: crate::pane::SectionLine, prefs: &UserPreferences) -> String {
    use crate::ui_section_edit as edit;
    let bearing = (edit::bearing_deg(line).rem_euclid(360.0).round() as u32) % 360;
    format!(
        "{bearing:03}\u{b0} - {:.0}{}",
        prefs.distance.convert_from_km(edit::length_km(line)),
        prefs.distance.suffix(),
    )
}

/// Where each part of a section pane goes.
struct SectionLayout {
    /// The section raster's rect.
    plot: egui::Rect,
    /// Where the caption's lines start.
    caption: egui::Rect,
    /// Whether there was room for axis labels.
    labelled_axes: bool,
}

/// The width the caption's text is wrapped to on a pane of this shape.
fn caption_wrap_width(pane_rect: egui::Rect, horizontal_color_scale: bool) -> f32 {
    let scale_reserve = if horizontal_color_scale {
        0.0
    } else {
        COLOR_SCALE_RESERVE
    };
    (pane_rect.width() - 8.0 - INFO_TOGGLE_RESERVE - scale_reserve).max(1.0)
}

impl SectionLayout {
    /// `caption_height` is **measured**, not counted: the caption wraps, so how
    /// many rows it occupies is a function of the pane's width and of how long the
    /// sentences came out in the user's own units.
    fn new(
        pane_rect: egui::Rect,
        top_clearance: f32,
        caption_height: f32,
        horizontal_color_scale: bool,
    ) -> Self {
        let labelled_axes = pane_rect.height() >= LABELLED_AXES_MIN_HEIGHT;
        let caption = egui::Rect::from_min_size(
            pane_rect.min + egui::vec2(4.0, top_clearance),
            egui::vec2(
                caption_wrap_width(pane_rect, horizontal_color_scale),
                caption_height,
            ),
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

    /// Screen `y` of a height in km MSL.
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

    let to_display = |km: f64| prefs.height.convert_km_to_kilo(km);
    let from_display = |shown: f64| prefs.height.convert_kilo_to_km(shown);
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
pub(crate) fn tilt_rung_color() -> egui::Color32 {
    egui::Color32::from_rgba_unmultiplied(255, 255, 255, 130)
}

/// The dark halo drawn under [`tilt_rung_color`].
pub(crate) fn tilt_rung_halo() -> egui::Color32 {
    egui::Color32::from_rgba_unmultiplied(0, 0, 0, 90)
}

/// Trace each elevation cut's beam centre across the section.
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
    let halo = tilt_rung_halo();
    let color = tilt_rung_color();
    let painter = painter.with_clip_rect(layout.plot);

    for points in curves {
        for pair in points.windows(2) {
            let mid = pair[0] + (pair[1] - pair[0]) * 0.55;
            painter.line_segment([pair[0], mid], egui::Stroke::new(3.0, halo));
            painter.line_segment([pair[0], mid], egui::Stroke::new(1.0, color));
        }
    }
}

/// Where each rung's beam centre crosses the section, in pane coordinates — one
/// polyline per elevation, ascending with the ladder.
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
    let ranges: Vec<(f32, f64)> = (0..=TILT_CURVE_SAMPLES)
        .map(|i| {
            let t = i as f64 / TILT_CURVE_SAMPLES as f64;
            let (lat, lon) = rustdar_geo::great_circle_point(a, b, t);
            let (_, ground_km) = rustdar_geo::site_bearing_range_km(site_lat, site_lon, lat, lon);
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

/// Whether the ladder this section was cut from reached the top of the coverage
/// pattern it belongs to.
fn ladder_reaches_pattern_top(axes: &SectionAxes) -> bool {
    axes.top_tilt_deg >= axes.top_declared_cut_deg
}

/// The tilt ladder this picture was cut from: where each rung is, and **when it was
/// flown**.
#[derive(Clone, Copy)]
struct Ladder<'a> {
    elevations_deg: &'a [f64],
    collected_ms: &'a [i64],
}

impl<'a> Ladder<'a> {
    /// The newest rung's clock, or `None` when no rung carries one.
    fn newest_ms(self) -> Option<i64> {
        self.collected_ms.iter().copied().filter(|&ms| ms > 0).max()
    }

    /// How long this ladder took to fly, in seconds — see
    /// [`rustdar_radar::xsect::assembly_span_secs`], which is the one definition.
    fn span_secs(self) -> Option<i64> {
        rustdar_radar::xsect::assembly_span_secs(self.collected_ms)
    }

    /// Each rung as `(number, elevation, age against the newest rung)`, in cut
    /// order — ascending, which is the order the dotted curves are drawn in and the
    /// order a reader's eye climbs the picture.
    fn rungs(self) -> impl Iterator<Item = (usize, f64, Option<i64>)> + 'a {
        let newest = self.newest_ms();
        self.elevations_deg
            .iter()
            .copied()
            .zip(self.collected_ms.iter().copied())
            .enumerate()
            .map(move |(i, (deg, ms))| {
                let age = newest.filter(|_| ms > 0).map(|newest| (newest - ms) / 1000);
                (i + 1, deg, age)
            })
    }
}

/// Whole minutes, rounded to nearest, from a count of seconds.
fn whole_minutes(secs: i64) -> i64 {
    (secs + 30) / 60
}

/// One line of the caption, before it is laid out.
struct CaptionLine {
    text: String,
    color: egui::Color32,
    size: f32,
    /// Whether [`lay_out_caption`] may drop this line to fit the pane.
    essential: bool,
}

/// The caption: one calm line saying what this is, and — behind the ⓘ — the detail
/// saying what it is not.
#[allow(clippy::too_many_arguments)]
fn caption_lines(
    axes: &SectionAxes,
    product: &FieldId,
    collected: Option<chrono::NaiveDateTime>,
    ladder: Ladder<'_>,
    unavailable: Option<crate::pane::SectionUnavailable>,
    detail_open: bool,
    visuals: &egui::Visuals,
    prefs: &UserPreferences,
) -> Vec<CaptionLine> {
    let mut lines = Vec::new();
    let calm = visuals.weak_text_color();

    let mut stamp = collected.map_or_else(String::new, |t| {
        format!("  -  {}", prefs.timezone.format_naive_utc(t, "%H:%M"))
    });
    if let Some(span) = ladder.span_secs()
        && span >= ASSEMBLY_SPAN_CAPTION_MIN_SECS
    {
        stamp.push_str(&format!("  -  assembled over {} min", whole_minutes(span)));
    }

    let (headline, headline_color) = match axes.tilt_count {
        0 => (
            format!(
                "{} - no tilts: this volume carried none of this product, so \
                 nothing below was measured{stamp}",
                crate::field_facts::name(product),
            ),
            visuals.error_fg_color,
        ),
        1 => (
            format!(
                "{} - one tilt: a single scanned surface, not a vertical \
                 profile{stamp}",
                crate::field_facts::name(product),
            ),
            calm,
        ),
        rungs => (
            format!(
                "{} - {rungs} tilts to {:.1}\u{b0}{stamp}",
                crate::field_facts::name(product),
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
        detail_lines(axes, ladder, visuals, prefs, &mut lines);
    }

    if let Some(reason) = unavailable {
        let broken = matches!(reason, crate::pane::SectionUnavailable::RenderFailed);
        lines.push(CaptionLine {
            text: if broken {
                format!("! {}", reason.message())
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
fn detail_lines(
    axes: &SectionAxes,
    ladder: Ladder<'_>,
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
             can reach - air above the top dotted curve was not sampled. That is \
             ordinary: scans often stop climbing once storm tops sit below the remaining \
             tilts, and the picture fills in if more arrive.",
            axes.top_tilt_deg, axes.top_declared_cut_deg,
        );
        let ceiling_km_msl = axes.base_km_msl
            + beam::height_at_ground_km(axes.coverage_ground_range_km, axes.top_tilt_deg);
        if ceiling_km_msl <= axes.top_km_msl {
            let ceiling_shown = prefs.height.convert_km_to_kilo(ceiling_km_msl);
            text.push_str(&format!(
                " The picture's ceiling is ~{:.0} {} MSL at the far end of the line.",
                ceiling_shown,
                prefs.height.kilo_suffix(),
            ));
        }
        push(text);
    }

    if axes.tilt_count >= 2 {
        let widest_gap_km = axes.widest_tilt_gap_deg.to_radians() * axes.coverage_ground_range_km;
        push(format!(
            "Data exists along the dotted curves - one per tilt - and the \
             picture between them is interpolated: layer depth and echo tops are set by \
             the tilt spacing, not measured. The widest step here is {:.1}\u{b0}, \
             ~{:.0}{} at {:.0}{}.",
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
        "Echoes sit at the same distance from the radar here as they do on the map - \
         both place a tilt under its beam rather than out along it. The height is \
         what only this view has.",
    );
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

    if let Some(list) = ladder_ages_sentence(ladder) {
        push(list);
    }
}

/// The ⓘ panel's per-rung ladder: number, elevation, and how much older that tilt
/// is than the newest in this section.
fn ladder_ages_sentence(ladder: Ladder<'_>) -> Option<String> {
    ladder.newest_ms()?;
    let entries: Vec<String> = ladder
        .rungs()
        .map(|(number, deg, age)| {
            let when = match age.map(whole_minutes) {
                Some(0) => "newest".to_owned(),
                Some(min) => format!("{min} min older"),
                None => "no time recorded".to_owned(),
            };
            format!("{number}: {deg:.1}\u{b0} {when}")
        })
        .collect();
    if entries.is_empty() {
        return None;
    }
    Some(format!(
        "The tilts were flown one at a time, so each rung of this picture is a \
         different moment - here is how much older each is than the newest tilt \
         in it, from the bottom up. {}.",
        entries.join(", "),
    ))
}

/// Wrap the caption to the pane's width, dropping detail lines — last first — if
/// what is left would eat the picture.
fn lay_out_caption(
    painter: &egui::Painter,
    pane_rect: egui::Rect,
    horizontal_color_scale: bool,
    mut lines: Vec<CaptionLine>,
) -> Vec<std::sync::Arc<egui::Galley>> {
    let wrap = caption_wrap_width(pane_rect, horizontal_color_scale);
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
        let Some(idx) = lines.iter().rposition(|l| !l.essential) else {
            return galleys;
        };
        lines.remove(idx);
        galleys = layout_all(&lines);
    }
    galleys
}

/// What the status bar says about the pixel under the pointer.
fn hover_readout(
    section: &CrossSection,
    layout: &SectionLayout,
    pos: egui::Pos2,
    product: &FieldId,
    source: Option<SectionSource<'_>>,
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
    let height_shown = prefs.height.convert_km_to_kilo(height_km);

    let value = match sample.value() {
        Some(value) => crate::field_facts::format_value(product, value, prefs),
        None => describe_missing(sample.status(), ladder_reaches_pattern_top(axes)).to_owned(),
    };
    let from = source.map_or_else(String::new, |source| {
        describe_source(source, axes, col, height_km)
            .map_or_else(String::new, |text| format!("  |  {text}"))
    });
    Some(format!(
        "{:.1}{} along  |  {:.1} {} MSL  |  {}{}",
        along,
        prefs.distance.suffix(),
        height_shown,
        prefs.height.kilo_suffix(),
        value,
        from,
    ))
}

/// What a hover needs in order to say **which sweep** answered it: the ladder, the
/// line it was cut along, and where the radar is.
#[derive(Clone, Copy)]
struct SectionSource<'a> {
    ladder: Ladder<'a>,
    line: crate::pane::SectionLine,
    site_lat: f64,
    site_lon: f64,
}

/// `4.3° sweep - 3 min old` for the pixel under the pointer, or `None` when no rung
/// reaches it.
fn describe_source(
    source: SectionSource<'_>,
    axes: &SectionAxes,
    col: usize,
    height_km_msl: f64,
) -> Option<String> {
    let t = axes.column_distance_km(col) / axes.length_km;
    if !t.is_finite() {
        return None;
    }
    let a = (source.line.a().lat, source.line.a().lon);
    let b = (source.line.b().lat, source.line.b().lon);
    let (lat, lon) = rustdar_geo::great_circle_point(a, b, t);
    let (_, ground_km) =
        rustdar_geo::site_bearing_range_km(source.site_lat, source.site_lon, lat, lon);
    let query_arl_km = height_km_msl - axes.base_km_msl;

    let mut nearest: Option<(f64, f64, Option<i64>)> = None;
    let (mut lowest, mut highest) = (f64::INFINITY, f64::NEG_INFINITY);
    for (_, deg, age) in source.ladder.rungs() {
        let rung_arl_km = beam::height_at_ground_km(ground_km, deg);
        if !rung_arl_km.is_finite() {
            continue;
        }
        lowest = lowest.min(rung_arl_km);
        highest = highest.max(rung_arl_km);
        let gap = (rung_arl_km - query_arl_km).abs();
        if nearest.is_none_or(|(best, _, _)| gap < best) {
            nearest = Some((gap, deg, age));
        }
    }
    let (_, deg, age) = nearest?;
    if query_arl_km < lowest || query_arl_km > highest {
        return None;
    }

    match age.filter(|&secs| secs >= HOVER_AGE_MIN_SECS) {
        Some(secs) => Some(format!(
            "{deg:.1}\u{b0} sweep - {} min old",
            whole_minutes(secs)
        )),
        None => Some(format!("{deg:.1}\u{b0} sweep")),
    }
}

/// Why there is no number here, in words a forecaster would use.
fn describe_missing(status: SampleStatus, ladder_reaches_top: bool) -> &'static str {
    match status {
        SampleStatus::Value => "no value",
        SampleStatus::BelowThreshold => "below threshold (the radar looked and saw nothing)",
        SampleStatus::RangeFolded => "range folded (a real echo, at an ambiguous distance)",
        SampleStatus::BelowLowestBeam => "below the lowest beam",
        SampleStatus::AboveVolume if ladder_reaches_top => "above the volume (cone of silence)",
        SampleStatus::AboveVolume => {
            "above the highest tilt this volume flew - not the cone of silence: the \
             scan ended below its pattern's top"
        }
        SampleStatus::BeyondRange => "beyond this tilt's range",
        SampleStatus::NoCoverage => "no coverage",
    }
}

/// A 1/2/5-times-a-power-of-ten step that puts roughly `wanted` ticks across
/// `span`.
fn nice_step(span: f64, wanted: f64) -> f64 {
    if !span.is_finite() || span <= 0.0 || !wanted.is_finite() || wanted < 1.0 {
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

#[path = "ui_section_pane/tests.rs"]
#[cfg(test)]
mod tests;
