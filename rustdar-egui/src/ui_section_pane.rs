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
//! 3. **The caption says so in words**, with the ladder's own numbers in it —
//!    how many rungs, and how far apart the widest pair is.
//!
//! # And the second thing, which is about the map rather than the section
//!
//! `render_gate` — the plan view's projection — applies no `cos(e)` and no beam
//! height model: it draws a gate at its *slant* range from the site, on the
//! ground. This section applies both. They therefore disagree, by 0.9 px at
//! 2.4° and by 18 px at 19.5° and 70 km. The section is the correct one. A user
//! comparing the section against the ground track drawn on the map above it
//! **will** see this, so it is in the caption rather than only in a doc comment.

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
/// Height of one caption line, in points.
const CAPTION_LINE: f32 = 15.0;
/// Below this pane height the second caption line is dropped.
///
/// A **runtime** threshold on the pane's own rect, never `cfg!(target_os)`: one
/// wasm binary serves a phone in portrait and a desktop browser, and the same
/// desktop window can be dragged to either size.
const TWO_LINE_CAPTION_MIN_HEIGHT: f32 = 260.0;
/// Points a section needs before axis labels are worth the room they take.
const LABELLED_AXES_MIN_HEIGHT: f32 = 120.0;
/// How many points along the line each tilt curve is sampled at.
///
/// The curve is a smooth function of ground range, and 64 segments across a
/// pane no wider than ~2000 points is well under a point of chord error.
const TILT_CURVE_SAMPLES: usize = 64;

/// Feet per kilometre, for the height axis in a `Feet` locale.
const KM_TO_KFT: f64 = 1.0 / 0.3048;

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
    prefs: &UserPreferences,
) {
    pane.hover_value = None;

    let product = pane.selected_product;
    let site = pane.scan_info.as_ref().map(|s| (s.site.lat, s.site.lon));
    let elevations = pane
        .scan_info
        .as_ref()
        .and_then(|s| s.product_elevations.get(&product))
        .cloned()
        .unwrap_or_default();

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

    let axes = *section.axes();
    let layout = SectionLayout::new(pane_rect, unavailable.is_some());

    let painter = ui.painter().with_clip_rect(pane_rect);
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
            &elevations,
        );
    }
    painter.rect_stroke(
        layout.plot,
        0.0,
        egui::Stroke::new(1.0, ui.visuals().weak_text_color()),
        egui::StrokeKind::Outside,
    );

    paint_caption(
        &painter,
        &layout,
        &axes,
        product,
        elevations.len(),
        unavailable,
        ui.visuals(),
        prefs,
    );

    if let Some(pos) = ui.ctx().pointer_hover_pos()
        && pane_rect.contains(pos)
    {
        pane.hover_value = hover_readout(&section, &layout, pos, product, prefs);
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
    /// Whether there was room for the registration line of the caption.
    two_line_caption: bool,
}

impl SectionLayout {
    fn new(pane_rect: egui::Rect, has_status_line: bool) -> Self {
        let two_line_caption = pane_rect.height() >= TWO_LINE_CAPTION_MIN_HEIGHT;
        let labelled_axes = pane_rect.height() >= LABELLED_AXES_MIN_HEIGHT;
        let caption_lines =
            1.0 + f32::from(u8::from(two_line_caption)) + f32::from(u8::from(has_status_line));
        let caption_height = caption_lines * CAPTION_LINE;
        let caption = egui::Rect::from_min_size(
            pane_rect.min + egui::vec2(4.0, 2.0),
            egui::vec2(pane_rect.width() - 8.0, caption_height),
        );
        let (left, bottom) = if labelled_axes {
            (HEIGHT_GUTTER, DISTANCE_GUTTER)
        } else {
            (2.0, 2.0)
        };
        let plot = egui::Rect::from_min_max(
            egui::pos2(pane_rect.left() + left, caption.bottom() + 2.0),
            egui::pos2(pane_rect.right() - 4.0, pane_rect.bottom() - bottom),
        );
        Self {
            plot,
            caption,
            labelled_axes,
            two_line_caption,
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

/// Trace each elevation cut's beam centre across the section.
///
/// **This is the honest device.** See the module documentation: a section is a
/// picture of the tilt ladder, and these curves are where the ladder actually
/// is. Data exists along them; everything between is two-point interpolation in
/// beam height, and the curves fan apart with range at exactly the rate the
/// interpolation error grows.
///
/// Drawn only when the ladder the UI knows about has the same number of rungs
/// the section was cut from. They are two different reads — `ScanInfo` discovers
/// elevations from the sweeps, the sampler keys them through the coverage
/// pattern's cut table — and if they disagree, these curves would be drawn in
/// the wrong places over a correct picture, which is worse than not drawing them
/// at all.
#[allow(clippy::too_many_arguments)]
fn paint_tilt_ladder(
    painter: &egui::Painter,
    layout: &SectionLayout,
    axes: &SectionAxes,
    a: (f64, f64),
    b: (f64, f64),
    site_lat: f64,
    site_lon: f64,
    elevations: &[f32],
) {
    if elevations.len() != axes.tilt_count || elevations.is_empty() {
        return;
    }
    let color = egui::Color32::from_rgba_unmultiplied(255, 255, 255, 70);
    let painter = painter.with_clip_rect(layout.plot);

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

    for &elev in elevations {
        let points: Vec<egui::Pos2> = ranges
            .iter()
            .map(|&(x, ground_km)| {
                let km_msl =
                    axes.base_km_msl + beam::height_at_ground_km(ground_km, f64::from(elev));
                egui::pos2(x, layout.y_of_height(axes, km_msl))
            })
            .collect();
        // Dashed rather than solid: a solid line over a reflectivity core reads
        // as a boundary *in the data*, which is the one thing these must not be
        // mistaken for.
        for pair in points.windows(2) {
            let mid = pair[0] + (pair[1] - pair[0]) * 0.55;
            painter.line_segment([pair[0], mid], egui::Stroke::new(1.0, color));
        }
    }
}

/// The caption: what this is, how coarse it is, and where it disagrees with the
/// map above it.
///
/// Not a dismissible banner and not a tooltip. Both of those are read once and
/// then never again, and the thing being said is not a one-off notice — it is
/// the standing meaning of every pixel in the pane.
#[allow(clippy::too_many_arguments)]
fn paint_caption(
    painter: &egui::Painter,
    layout: &SectionLayout,
    axes: &SectionAxes,
    product: RadarProduct,
    known_rungs: usize,
    unavailable: Option<crate::pane::SectionUnavailable>,
    visuals: &egui::Visuals,
    prefs: &UserPreferences,
) {
    let mut y = layout.caption.top();
    let mut line = |text: String, color: egui::Color32, size: f32| {
        painter.text(
            egui::pos2(layout.caption.left(), y),
            egui::Align2::LEFT_TOP,
            text,
            egui::FontId::proportional(size),
            color,
        );
        y += CAPTION_LINE;
    };

    // The ladder's own numbers, so the warning is a measurement rather than
    // boilerplate: 14 rungs 0.5° apart and 5 rungs 5° apart are the same
    // sentence with wildly different consequences.
    let widest_gap_km = axes.widest_tilt_gap_deg.to_radians() * axes.coverage_ground_range_km;
    let gap_shown = prefs.distance.convert_from_km(widest_gap_km);
    line(
        format!(
            "{}  \u{2014}  {} tilts, widest gap {:.1}\u{b0} (\u{2248}{:.0}{} apart at {:.0}{}): \
             layer depth and echo tops here are set by the ladder, not measured",
            product.name(),
            axes.tilt_count,
            axes.widest_tilt_gap_deg,
            gap_shown,
            prefs.distance.suffix(),
            prefs
                .distance
                .convert_from_km(axes.coverage_ground_range_km),
            prefs.distance.suffix(),
        ),
        visuals.warn_fg_color,
        11.0,
    );

    if layout.two_line_caption {
        // The registration caveat, in the pane the user is looking at rather
        // than only in a doc comment — because the disagreement is visible.
        // `render_gate` draws a gate at its slant range on the ground with no
        // `cos(e)` and no beam height; this applies both. 0.9 px at 2.4°,
        // 18 px at 19.5° and 70 km.
        let mut second = String::from(
            "The map draws gates at slant range, ignoring beam tilt; this section does not, \
             so echoes sit slightly off the ground track on high tilts. The section is right.",
        );
        // Only when it is true of *this* section: a line that ran out of data
        // half way along is a fact about the picture, and saying it always
        // would make it noise.
        if axes.coverage_ground_range_km + 0.5 < axes.far_ground_range_km {
            second.push_str(&format!(
                "  Coverage stops at {:.0}{} of {:.0}{}.",
                prefs
                    .distance
                    .convert_from_km(axes.coverage_ground_range_km),
                prefs.distance.suffix(),
                prefs.distance.convert_from_km(axes.far_ground_range_km),
                prefs.distance.suffix(),
            ));
        }
        line(second, visuals.weak_text_color(), 10.0);
    }

    // Last, under both, so a transient state never pushes the standing warning
    // off the top of the pane.
    if let Some(reason) = unavailable {
        line(
            format!("\u{26a0} {}", reason.message()),
            visuals.error_fg_color,
            10.0,
        );
    }

    let _ = known_rungs;
}

/// What the status bar says about the pixel under the pointer.
///
/// # Most of the value of the status plane is here
///
/// The sampler propagates seven reasons a pixel has no number, and this is the
/// first place in the codebase that can *say* one. A blank region of a section
/// is not one thing: below the lowest beam is a permanent blind spot near the
/// ground, above the volume is the cone of silence, beyond range is an upper cut
/// that stops short, range folded is a real echo at an ambiguous distance, and
/// below threshold is the radar looking and finding nothing. Those are five
/// completely different facts and they are painted identically.
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
        None => describe_missing(sample.status()).to_owned(),
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
fn describe_missing(status: SampleStatus) -> &'static str {
    match status {
        SampleStatus::Value => "no value",
        SampleStatus::BelowThreshold => "below threshold (the radar looked and saw nothing)",
        SampleStatus::RangeFolded => "range folded (a real echo, at an ambiguous distance)",
        SampleStatus::BelowLowestBeam => "below the lowest beam",
        SampleStatus::AboveVolume => "above the volume (cone of silence)",
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
        }
    }

    /// The two mappings are inverses of the raster's own convention: row 0 is
    /// the **top**, so the top of the axis is the top of the plot.
    ///
    /// Getting this upside down is the single most likely mistake in the
    /// module and the least likely to be noticed — a flipped section of a
    /// mature storm still looks like a storm.
    #[test]
    fn the_top_of_the_axis_is_the_top_of_the_plot() {
        let rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(400.0, 300.0));
        let layout = SectionLayout::new(rect, false);
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
        let layout = SectionLayout::new(rect, false);
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
        let mut seen: Vec<&str> = all.iter().copied().map(describe_missing).collect();
        seen.sort_unstable();
        let before = seen.len();
        seen.dedup();
        assert_eq!(
            before,
            seen.len(),
            "two blank reasons read the same: {seen:?}"
        );
    }

    /// The caption band shrinks on a short pane, and the picture never
    /// collapses to nothing.
    ///
    /// A **runtime** decision on the rect, so one wasm binary serves a phone in
    /// portrait and a desktop browser — pinned because `cfg!(target_os)` is the
    /// tempting wrong answer and would compile.
    #[test]
    fn a_short_pane_drops_the_second_caption_line_and_keeps_a_picture() {
        let tall = SectionLayout::new(
            egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(600.0, 400.0)),
            false,
        );
        assert!(tall.two_line_caption);
        assert!(tall.labelled_axes);

        let short = SectionLayout::new(
            egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(600.0, 200.0)),
            false,
        );
        assert!(!short.two_line_caption);
        assert!(short.labelled_axes);
        assert!(short.plot.height() > 0.0);

        let tiny = SectionLayout::new(
            egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(300.0, 110.0)),
            false,
        );
        assert!(!tiny.labelled_axes, "no room for labels at 110 points");
        assert!(
            tiny.plot.left() < tiny.plot.right(),
            "the picture must not be squeezed out by its own gutters"
        );
    }

    /// A pane carrying a status line makes room for it rather than drawing it
    /// over the picture.
    #[test]
    fn a_status_line_takes_room_from_the_picture_not_from_the_warning() {
        let rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(600.0, 400.0));
        let without = SectionLayout::new(rect, false);
        let with = SectionLayout::new(rect, true);
        assert!(with.caption.height() > without.caption.height());
        assert!(with.plot.top() > without.plot.top());
        assert!(with.plot.height() < without.plot.height());
    }
}
