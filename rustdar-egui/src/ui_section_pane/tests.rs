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

/// A degenerate axis must not divide by zero. `render_section` refuses one,
/// so this is about a section that arrived over a wire and about the
/// mappings being total rather than about a state production reaches.
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

    let labelled = SectionLayout::new(rect(400.0), crate::ui::PILL_ROW_CLEARANCE, ONE_LINE, false);
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
    let bare = SectionLayout::new(rect(110.0), crate::ui::PILL_ROW_CLEARANCE, ONE_LINE, false);
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
            false,
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
        "~{:.0} {} MSL",
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
        // The warning glyph follows the same rule: a leading "!" on a
        // routine state is the old alarm back in miniature.
        assert_eq!(
            status.text.starts_with('!'),
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
    let layout = SectionLayout::new(rect, crate::ui::PILL_ROW_CLEARANCE, ONE_LINE, false);
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
    let without = SectionLayout::new(rect, crate::ui::PILL_ROW_CLEARANCE, ONE_LINE, false);
    let with = SectionLayout::new(rect, crate::ui::PILL_ROW_CLEARANCE, TWO_LINES, false);
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
    // And each orientation gives back the room the other one took, rather
    // than reserving both edges always.
    assert!(horizontal.plot.right() > vertical.plot.right());
    assert!(vertical.plot.bottom() > horizontal.plot.bottom());
}
