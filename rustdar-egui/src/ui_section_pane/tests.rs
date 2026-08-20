use super::*;
use rustdar_radar::fields as radar_fields;

/// A measured caption height standing in for one wrapped row, for the tests that
/// are about the *rest* of the layout.
const ONE_LINE: f32 = 15.0;
/// Feet per kilometre, for the tests that check a `Feet` locale's height.
const KM_TO_KFT: f64 = 1.0 / 0.3048;
/// A ladder carrying no rungs and no clocks, for the tests that are about something
/// other than age.
const BARE_LADDER: Ladder<'static> = Ladder {
    elevations_deg: &[],
    collected_ms: &[],
};
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

/// VCP 212's reflectivity ladder as KTLX really flies it, in the sampler's own
/// median angles rather than in round numbers — the shape a section arrives
/// carrying.
const VCP_212: [f64; 14] = [
    0.4834, 0.8789, 1.3184, 1.8018, 2.4170, 3.1201, 4.0430, 5.0977, 6.4160, 8.0273, 10.0195,
    12.5000, 15.6006, 19.5117,
];

/// The two mappings are inverses of the raster's own convention: row 0 is the
/// **top**, so the top of the axis is the top of the plot.
#[test]
fn the_top_of_the_axis_is_the_top_of_the_plot() {
    let rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(400.0, 300.0));
    let layout = SectionLayout::new(rect, crate::ui::PILL_ROW_CLEARANCE, ONE_LINE, false);
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

/// A degenerate axis must not divide by zero.
#[test]
fn a_degenerate_axis_maps_to_the_edges_rather_than_to_nan() {
    let rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(400.0, 300.0));
    let layout = SectionLayout::new(rect, crate::ui::PILL_ROW_CLEARANCE, ONE_LINE, false);
    let flat = SectionAxes {
        length_km: 0.0,
        top_km_msl: 0.4,
        ..axes()
    };
    assert_eq!(layout.y_of_height(&flat, 1.0), layout.plot.bottom());
    assert_eq!(layout.x_of_distance(&flat, 1.0), layout.plot.left());
}

/// `nice_step` is what the two tick loops advance by, so a step of zero or `NaN` is
/// not a cosmetic bug — it is an infinite loop on the frame thread, which on wasm
/// is the whole application.
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

/// Every reason a pixel is blank has its own words.
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

    let flying = SectionAxes {
        top_tilt_deg: 1.8,
        top_declared_cut_deg: 19.5,
        ..axes()
    };
    assert!(ladder_reaches_pattern_top(&axes()));
    assert!(!ladder_reaches_pattern_top(&flying));
}

/// The caption band shrinks on a short pane, and the picture never collapses to
/// nothing.
#[test]
fn a_short_pane_drops_the_second_caption_line_and_keeps_a_picture() {
    let rect = |w: f32, h: f32| egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(w, h));

    assert!(
        SectionLayout::new(
            rect(600.0, 400.0),
            crate::ui::PILL_ROW_CLEARANCE,
            TWO_LINES,
            false
        )
        .labelled_axes
    );

    let short = SectionLayout::new(
        rect(600.0, 200.0),
        crate::ui::PILL_ROW_CLEARANCE,
        ONE_LINE,
        false,
    );
    assert!(short.labelled_axes);
    assert!(short.plot.height() > 0.0);

    let tiny = SectionLayout::new(
        rect(300.0, 110.0),
        crate::ui::PILL_ROW_CLEARANCE,
        ONE_LINE,
        false,
    );
    assert!(!tiny.labelled_axes, "no room for labels at 110 points");
    assert!(
        tiny.plot.left() < tiny.plot.right(),
        "the picture must not be squeezed out by its own gutters"
    );
}

/// The height axis's unit label gets its own room, rather than being drawn upward
/// over the last line of the caption.
#[test]
fn the_axis_unit_label_has_room_above_the_plot() {
    let rect = |h: f32| egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(600.0, h));

    let labelled = SectionLayout::new(rect(400.0), crate::ui::PILL_ROW_CLEARANCE, ONE_LINE, false);
    assert!(labelled.labelled_axes, "precondition");
    assert!(
        labelled.plot.top() - 2.0 - 10.0 >= labelled.caption.bottom(),
        "the MSL unit label is drawn over the caption: plot top {}, caption \
             bottom {}",
        labelled.plot.top(),
        labelled.caption.bottom()
    );

    let bare = SectionLayout::new(rect(110.0), crate::ui::PILL_ROW_CLEARANCE, ONE_LINE, false);
    assert!(!bare.labelled_axes, "precondition");
    assert!(
        bare.plot.top() - bare.caption.bottom() < AXIS_UNIT_HEADROOM,
        "room was reserved for a label this pane has no room to draw"
    );
}

/// The caption is **wrapped and then measured**, so no sentence in it is ever
/// clipped and no wrapped row is ever painted over the picture.
#[test]
fn the_caption_wraps_and_the_layout_pays_for_the_rows_it_takes() {
    let ctx = egui::Context::default();
    let _ = ctx.run_ui(egui::RawInput::default(), |_| {});
    let prefs = UserPreferences::default();
    let visuals = egui::Visuals::dark();
    let truncated = SectionAxes {
        coverage_ground_range_km: 64.0,
        top_tilt_deg: 6.4,
        ..axes()
    };

    let rect = |w: f32, h: f32| egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(w, h));
    let measure = |w: f32, h: f32| {
        let rect = rect(w, h);
        let painter = egui::Painter::new(ctx.clone(), egui::LayerId::debug(), rect);
        let galleys = lay_out_caption(
            &painter,
            rect,
            false,
            caption_lines(
                &truncated,
                &radar_fields::known::REFLECTIVITY,
                None,
                BARE_LADDER,
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
            widest <= caption_wrap_width(rect(w, h), false) + 0.5,
            "at {w}x{h} the caption ran {widest} points wide and was clipped"
        );
        assert!(
            height <= h * CAPTION_MAX_HEIGHT_FRACTION,
            "at {w}x{h} the caption ate {height} points of the pane"
        );
        let layout = SectionLayout::new(rect(w, h), crate::ui::PILL_ROW_CLEARANCE, height, false);
        assert!(
            layout.plot.top() >= layout.caption.top() + height,
            "at {w}x{h} the plot starts inside the {rows}-row caption above it"
        );
        assert!(layout.plot.height() > 0.0, "no picture left at {w}x{h}");
    }

    let (_, _, wide) = measure(1780.0, 900.0);
    let (_, _, medium) = measure(620.0, 500.0);
    assert!(
        medium > wide,
        "the caption did not wrap on a narrower pane ({medium} against {wide})"
    );

    let (rows_narrow, _, narrow) = measure(150.0, 300.0);
    let (rows_roomy, _, _) = measure(400.0, 400.0);
    assert!(
        rows_narrow < rows_roomy,
        "a caption with no room to wrap kept every line anyway"
    );
    assert!(narrow <= 300.0 * CAPTION_MAX_HEIGHT_FRACTION);
    let (rows_tiny, _, _) = measure(150.0, 120.0);
    assert!(
        rows_tiny >= 1,
        "the essential line was dropped to fit the budget"
    );

    let squeezed = {
        let rect = rect(150.0, 300.0);
        let painter = egui::Painter::new(ctx.clone(), egui::LayerId::debug(), rect);
        lay_out_caption(
            &painter,
            rect,
            false,
            caption_lines(
                &truncated,
                &radar_fields::known::REFLECTIVITY,
                None,
                BARE_LADDER,
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

/// A one-rung ladder is the **worst** case, and the caption must not describe it in
/// the ordinary case's words.
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
            &radar_fields::known::REFLECTIVITY,
            None,
            BARE_LADDER,
            None,
            false,
            &visuals,
            &prefs,
        )
        .swap_remove(0)
    };

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

    let ordinary = caption(14, 4.9);
    assert!(ordinary.text.contains("14 tilts"), "{}", ordinary.text);
    assert!(ordinary.text.contains("19.5"), "{}", ordinary.text);
    assert_ne!(
        ordinary.color, visuals.error_fg_color,
        "the ordinary case must not be styled as a fault"
    );
    assert!(
        !ordinary.text.contains("widest gap"),
        "the default line took the detail's numbers back: {}",
        ordinary.text
    );
}

/// **A ladder that stopped short is captioned as the ordinary case it is**, and the
/// truncation is explained — in the user's words, on request.
#[test]
fn a_ladder_that_stopped_short_stays_calm_and_explains_on_request() {
    let prefs = UserPreferences::default();
    let visuals = egui::Visuals::dark();
    let lines = |axes: SectionAxes, detail_open: bool| {
        caption_lines(
            &axes,
            &radar_fields::known::REFLECTIVITY,
            None,
            BARE_LADDER,
            None,
            detail_open,
            &visuals,
            &prefs,
        )
    };

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
    for fault in ["cut short", "abandoned", "failed", "error"] {
        assert!(
            !detail.contains(fault),
            "the detail blames a scan for a ceiling AVSET puts there on \
                 purpose ({fault:?}): {detail}"
        );
    }
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

    let ceiling_km = 0.4 + beam::height_at_ground_km(86.0, 1.8);
    assert!(
        ceiling_km <= filling_axes.top_km_msl,
        "precondition: this ceiling is on the chart"
    );
    let kft = format!(
        "~{:.0} {} MSL",
        ceiling_km * KM_TO_KFT,
        prefs.height.kilo_suffix()
    );
    assert!(
        detail.contains(&kft),
        "an on-chart ceiling was not quoted ({kft} expected): {detail}"
    );

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
#[test]
fn red_is_reserved_for_broken_states() {
    use crate::pane::SectionUnavailable;
    let prefs = UserPreferences::default();
    let visuals = egui::Visuals::dark();
    let lines = |axes: SectionAxes, unavailable: Option<SectionUnavailable>| {
        caption_lines(
            &axes,
            &radar_fields::known::REFLECTIVITY,
            None,
            BARE_LADDER,
            unavailable,
            false,
            &visuals,
            &prefs,
        )
    };

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

    for (reason, broken) in [
        (SectionUnavailable::AwaitingVolume, false),
        (SectionUnavailable::AwaitingCoveragePattern, false),
        (
            SectionUnavailable::ProductHasNoVerticalStructure(
                radar_fields::known::VERTICALLY_INTEGRATED_LIQUID,
            ),
            false,
        ),
        (SectionUnavailable::RenderFailed, true),
    ] {
        let all = lines(axes(), Some(reason.clone()));
        let status = all.last().expect("a status line was pushed");
        assert_eq!(
            status.color == visuals.error_fg_color,
            broken,
            "{reason:?} has the wrong styling: {}",
            status.text
        );
        assert_eq!(
            status.text.starts_with('!'),
            broken,
            "{reason:?} carries the wrong glyph: {}",
            status.text
        );
    }
}

/// **A real VCP 212 ladder draws.**
#[test]
fn a_real_tilt_ladder_draws_and_fans_apart_with_range() {
    let rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(800.0, 500.0));
    let layout = SectionLayout::new(rect, crate::ui::PILL_ROW_CLEARANCE, ONE_LINE, false);
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

    for pair in curves.windows(2) {
        assert!(
            pair[1][0].y < pair[0][0].y,
            "the rungs are not in ascending order of height"
        );
    }

    let near = curves[1][0].y - curves[0][0].y;
    let far = curves[1][TILT_CURVE_SAMPLES].y - curves[0][TILT_CURVE_SAMPLES].y;
    assert!(
        far.abs() > near.abs() * 1.2,
        "the rungs do not fan apart with range ({near} near, {far} far), so \
             the drawing says nothing about where the ladder is coarsest"
    );

    assert!(
        tilt_curves(&layout, &axes, a, b, site_lat, site_lon, &[]).is_none(),
        "an empty ladder has no rungs to draw"
    );

    let partial = &VCP_212[..4];
    let mid_flight = SectionAxes {
        tilt_count: partial.len(),
        ..axes
    };
    let curves = tilt_curves(&layout, &mid_flight, a, b, site_lat, site_lon, partial)
        .expect("a volume four cuts into its flight still has four real rungs");
    assert_eq!(curves.len(), partial.len());
}

/// A pane carrying a status line makes room for it rather than drawing it over the
/// picture.
#[test]
fn a_status_line_takes_room_from_the_picture_not_from_the_warning() {
    let rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(600.0, 400.0));
    let without = SectionLayout::new(rect, crate::ui::PILL_ROW_CLEARANCE, ONE_LINE, false);
    let with = SectionLayout::new(rect, crate::ui::PILL_ROW_CLEARANCE, TWO_LINES, false);
    assert!(with.caption.height() > without.caption.height());
    assert!(with.plot.top() > without.plot.top());
    assert!(with.plot.height() < without.plot.height());
}

/// The plot leaves the colour bar its edge, whichever edge that is.
#[test]
fn the_plot_leaves_room_for_whichever_edge_the_colour_bar_took() {
    let rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(800.0, 500.0));
    let vertical = SectionLayout::new(rect, crate::ui::PILL_ROW_CLEARANCE, ONE_LINE, false);
    let horizontal = SectionLayout::new(rect, crate::ui::PILL_ROW_CLEARANCE, ONE_LINE, true);

    assert!(
        rect.right() - vertical.plot.right() >= COLOR_SCALE_RESERVE,
        "a right-edge colour bar would be painted over the section"
    );
    assert!(
        rect.bottom() - horizontal.plot.bottom() >= COLOR_SCALE_RESERVE,
        "a bottom-edge colour bar would be painted over the section"
    );
    assert!(horizontal.plot.right() > vertical.plot.right());
    assert!(vertical.plot.bottom() > horizontal.plot.bottom());
}

/// **One line, one length, one unit.**
#[test]
fn the_line_chip_reads_in_the_users_own_distance_unit() {
    use rustdar_units::DistanceUnit;

    let line = crate::pane::SectionLine::new(
        rustdar_geo::GeoPoint {
            lat: 35.0,
            lon: -97.0,
        },
        rustdar_geo::GeoPoint {
            lat: 36.0,
            lon: -97.0,
        },
    )
    .expect("two distinct finite points");

    let with = |unit: DistanceUnit| {
        let prefs = UserPreferences {
            distance: unit,
            ..UserPreferences::default()
        };
        line_readout(line, &prefs)
    };

    assert_eq!(
        with(DistanceUnit::Kilometers),
        "000\u{b0} - 111km",
        "a metric user's chip is not in kilometres"
    );
    assert_eq!(
        with(DistanceUnit::Miles),
        "000\u{b0} - 69mi",
        "a miles user's chip is not in miles"
    );
    assert_eq!(
        with(DistanceUnit::NauticalMiles),
        "000\u{b0} - 60nmi",
        "a degree of latitude is sixty nautical miles by definition, so this \
         is the arithmetic checking itself"
    );

    let all = [
        with(DistanceUnit::Kilometers),
        with(DistanceUnit::Miles),
        with(DistanceUnit::NauticalMiles),
    ];
    for (i, a) in all.iter().enumerate() {
        for b in &all[i + 1..] {
            assert_ne!(a, b, "two distance units render the same chip");
        }
    }
}

/// Milliseconds since the epoch that every age fixture below is measured from.
const T0: i64 = 1_760_000_000_000;

/// A ladder whose rungs were flown `secs` apart, oldest (lowest) first — the order
/// a radar actually flies a volume in.
fn ladder_flown_over(elevations: &[f64], step_secs: i64) -> (Vec<f64>, Vec<i64>) {
    let clocks = (0..elevations.len())
        .map(|i| T0 + (i as i64) * step_secs * 1000)
        .collect();
    (elevations.to_vec(), clocks)
}

/// **The caption says how long the section took to assemble only when that is
/// beyond what one volume accounts for.**
#[test]
fn the_caption_names_the_assembly_span_only_when_it_is_beyond_one_volume() {
    let prefs = UserPreferences::default();
    let visuals = egui::Visuals::dark();
    let headline = |elevations: &[f64], step_secs: i64| {
        let (degs, clocks) = ladder_flown_over(elevations, step_secs);
        let axes = axes();
        caption_lines(
            &axes,
            &radar_fields::known::REFLECTIVITY,
            None,
            Ladder {
                elevations_deg: &degs,
                collected_ms: &clocks,
            },
            None,
            false,
            &visuals,
            &prefs,
        )
        .swap_remove(0)
        .text
    };

    let tight = headline(&[0.5, 1.5, 2.4, 3.4], 80);
    assert!(
        !tight.contains("assembled"),
        "a four-minute volume was qualified, which is the constant-qualifier \
         failure the caption was redesigned to remove: {tight}"
    );

    let stretched = headline(&[0.5, 1.5, 2.4, 3.4], 120);
    assert!(
        stretched.contains("assembled over 6 min"),
        "a six-minute spread was not reported: {stretched}"
    );

    let (degs, mut clocks) = ladder_flown_over(&[0.5, 1.5], 0);
    let at_threshold = {
        clocks[1] = T0 + ASSEMBLY_SPAN_CAPTION_MIN_SECS * 1000;
        let axes = axes();
        caption_lines(
            &axes,
            &radar_fields::known::REFLECTIVITY,
            None,
            Ladder {
                elevations_deg: &degs,
                collected_ms: &clocks,
            },
            None,
            false,
            &visuals,
            &prefs,
        )
        .swap_remove(0)
        .text
    };
    assert!(
        at_threshold.contains("assembled over 5 min"),
        "the threshold is exclusive where it is documented inclusive: \
         {at_threshold}"
    );
    let just_under = {
        clocks[1] = T0 + (ASSEMBLY_SPAN_CAPTION_MIN_SECS - 1) * 1000;
        let axes = axes();
        caption_lines(
            &axes,
            &radar_fields::known::REFLECTIVITY,
            None,
            Ladder {
                elevations_deg: &degs,
                collected_ms: &clocks,
            },
            None,
            false,
            &visuals,
            &prefs,
        )
        .swap_remove(0)
        .text
    };
    assert!(
        !just_under.contains("assembled"),
        "one second under the threshold still fired: {just_under}"
    );

    let silent = headline(&[0.5, 1.5, 2.4], 0);
    assert!(
        !silent.contains("assembled"),
        "a ladder flown in one instant claimed a span: {silent}"
    );
    let unclocked = {
        let axes = axes();
        caption_lines(
            &axes,
            &radar_fields::known::REFLECTIVITY,
            None,
            Ladder {
                elevations_deg: &[0.5, 1.5, 2.4],
                collected_ms: &[0, 0, 0],
            },
            None,
            false,
            &visuals,
            &prefs,
        )
        .swap_remove(0)
        .text
    };
    assert!(
        !unclocked.contains("assembled"),
        "a section that knows nothing about when it was flown described its \
         own assembly: {unclocked}"
    );
}

/// **The ⓘ detail carries the whole ladder: rung, elevation, age.**
#[test]
fn the_info_detail_lists_every_rung_with_its_own_age() {
    let prefs = UserPreferences::default();
    let visuals = egui::Visuals::dark();
    let lines = |degs: &[f64], clocks: &[i64], detail_open: bool| {
        let axes = axes();
        caption_lines(
            &axes,
            &radar_fields::known::REFLECTIVITY,
            None,
            Ladder {
                elevations_deg: degs,
                collected_ms: clocks,
            },
            None,
            detail_open,
            &visuals,
            &prefs,
        )
    };

    let (degs, clocks) = ladder_flown_over(&[0.5, 2.4, 8.0], 180);
    let detail: String = lines(&degs, &clocks, true)
        .iter()
        .skip(1)
        .map(|l| l.text.as_str())
        .collect::<Vec<_>>()
        .join("\n");

    assert!(
        detail.contains("1: 0.5\u{b0} 6 min older"),
        "the lowest rung's own age is missing: {detail}"
    );
    assert!(
        detail.contains("2: 2.4\u{b0} 3 min older"),
        "the middle rung's age is missing or misattributed: {detail}"
    );
    assert!(
        detail.contains("3: 8.0\u{b0} newest"),
        "the reference rung is given as a rounding artefact rather than named: \
         {detail}"
    );
    assert!(
        detail.contains("older each is than the newest tilt"),
        "the list does not state what its numbers are measured against, and \
         '6 min' alone is ambiguous by a whole volume: {detail}"
    );

    let closed = lines(&degs, &clocks, false);
    assert_eq!(closed.len(), 1, "a closed detail contributed caption lines");
    assert!(
        !closed[0].text.contains("0.5\u{b0} 6 min"),
        "the per-rung ladder leaked into the calm default line: {}",
        closed[0].text
    );

    let unclocked: String = lines(&degs, &[0, 0, 0], true)
        .iter()
        .skip(1)
        .map(|l| l.text.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        !unclocked.contains("flown one at a time"),
        "a section with no clocks listed ages anyway: {unclocked}"
    );
    assert!(
        unclocked.contains("interpolated"),
        "the clock-less section lost the rest of its detail: {unclocked}"
    );
}

/// **The hover names the sweep the pixel under the pointer came from, and how much
/// older it is than the freshest tilt in the same picture.**
#[test]
fn the_hover_names_the_sweep_it_came_from_and_how_old_it_is() {
    use rustdar_radar::sampler::SampleStatus;
    use rustdar_radar::xsect::CrossSection;

    let (site_lat, site_lon) = (35.0, -97.0);
    let line = crate::pane::SectionLine::new(
        rustdar_geo::GeoPoint {
            lat: site_lat,
            lon: -96.0,
        },
        rustdar_geo::GeoPoint {
            lat: site_lat,
            lon: -95.0,
        },
    )
    .expect("two distinct finite points");

    let (degs, clocks) = ladder_flown_over(&[0.5, 5.0], 360);
    let axes = SectionAxes {
        length_km: 91.0,
        base_km_msl: 0.0,
        top_km_msl: 20.0,
        near_ground_range_km: 91.0,
        far_ground_range_km: 182.0,
        coverage_ground_range_km: 182.0,
        cone_of_silence_km: 0.0,
        tilt_count: 2,
        widest_tilt_gap_deg: 4.5,
        top_tilt_deg: 5.0,
        top_declared_cut_deg: 19.5,
    };
    let pixels = SECTION_WIDTH * SECTION_HEIGHT;
    let section = CrossSection::from_parts(
        vec![0u8; pixels * 4],
        vec![f32::NAN; pixels],
        vec![SampleStatus::BelowThreshold.wire_code(); pixels],
        axes,
        degs.clone(),
        clocks.clone(),
    )
    .expect("a full-size section with a two-rung clocked ladder is well formed");

    let rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(800.0, 500.0));
    let layout = SectionLayout::new(rect, crate::ui::PILL_ROW_CLEARANCE, ONE_LINE, false);
    let source = SectionSource {
        ladder: Ladder {
            elevations_deg: &degs,
            collected_ms: &clocks,
        },
        line,
        site_lat,
        site_lon,
    };
    let at = |km_msl: f64| egui::pos2(layout.plot.left() + 1.0, layout.y_of_height(&axes, km_msl));
    let read = |km_msl: f64, prefs: &UserPreferences| {
        hover_readout(
            &section,
            &layout,
            at(km_msl),
            &radar_fields::known::REFLECTIVITY,
            Some(source),
            prefs,
        )
        .expect("the pointer is inside the plot")
    };
    let prefs = UserPreferences::default();

    let low = read(1.5, &prefs);
    assert!(
        low.contains("0.5\u{b0} sweep - 6 min old"),
        "the low rung was not named, or was paired with the wrong clock: {low}"
    );

    let high = read(7.5, &prefs);
    assert!(
        high.contains("5.0\u{b0} sweep"),
        "the high rung was not named: {high}"
    );
    assert!(
        !high.contains("min old"),
        "the freshest rung in the section was qualified as old: {high}"
    );

    for outside in [0.05, 19.5] {
        let text = read(outside, &prefs);
        assert!(
            !text.contains("sweep"),
            "a pixel at {outside} km MSL, outside the ladder entirely, was \
             attributed to a tilt: {text}"
        );
    }

    assert!(low.contains("below threshold"), "{low}");
    assert!(low.contains("MSL"), "{low}");

    let metric = UserPreferences {
        height: rustdar_units::HeightUnit::Meters,
        ..UserPreferences::default()
    };
    assert!(
        read(1.5, &metric).contains("1.5 km MSL"),
        "{}",
        read(1.5, &metric)
    );
    let imperial = UserPreferences {
        height: rustdar_units::HeightUnit::Feet,
        ..UserPreferences::default()
    };
    assert!(
        read(1.5, &imperial).contains("4.9 kft MSL"),
        "{}",
        read(1.5, &imperial)
    );
}
