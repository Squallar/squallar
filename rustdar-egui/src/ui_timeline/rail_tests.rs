//! The archive rail's two regions: the mapping between a fraction of one
//! bar's travel and an instant, and the rules that hang off it.
//!
//! Every figure here is against a **travel**, never a rect: see
//! [`super::slider_travel_px`]. The two measured widths the whole file works
//! in are the transport's own, read off the real widget at two screen sizes.

use super::{
    LIVE_SNAP_MAX_FUTURE_SHARE, LIVE_SNAP_PX, NOW_SPLIT, RailRegions, RailSide, loop_rail_split,
    slider_travel_px,
};
use crate::pane::LoopFrame;

/// The travel a 1400 x 900 screen gives the rail, measured off
/// `h.timeline().scrubber` (a 488.0 pt rect less 5.40 pt per end). Any window
/// 904 pt or wider gives this same number, because `MAX_OUTER_WIDTH` binds.
const WIDE_TRAVEL: f32 = 477.17;

/// The travel a 480 pt window gives it, where the transport has collapsed but
/// not yet flipped to its two-row form. The narrowest the one-row rail gets.
const NARROW_TRAVEL: f32 = 77.17;

/// The default Lookback, in seconds.
const SPAN: f32 = 3600.0;

/// An HRRR run off a 00/06/12/18Z cycle: 48 forecast hours.
const HORIZON_48H: f32 = 48.0 * 3600.0;

/// Every other hour's run: 18.
const HORIZON_18H: f32 = 18.0 * 3600.0;

/// The rail a pane whose transport reaches `horizon` seconds forward gets.
fn forecast_rail(past_secs: f32, horizon: f32) -> RailRegions {
    RailRegions {
        split: NOW_SPLIT,
        past_secs,
        future_secs: horizon,
        step_secs: 3600,
    }
}

/// **Seconds per pixel in the past region, read off the mapping the widget
/// actually releases through** — not off a second expression that would agree
/// with the first by construction.
fn past_secs_per_px(rail: &RailRegions, travel: f32) -> f32 {
    let at = 0.5 * rail.split;
    (rail.offset_secs(at + 1.0 / travel) - rail.offset_secs(at)).abs()
}

/// The same, one pixel into the forecast region.
fn future_secs_per_px(rail: &RailRegions, travel: f32) -> f32 {
    let at = rail.split + 0.5 * (1.0 - rail.split);
    (rail.offset_secs(at + 1.0 / travel) - rail.offset_secs(at)).abs()
}

/// **The past is not crushed** — the property the whole two-colour design
/// exists to hold, and the one a shared linear axis fails.
///
/// A single linear rail over `span + horizon` hands the past `P/(P+F)` of the
/// travel, so switching a forecast layer on would multiply the past's seconds
/// per pixel by 19 at an 18 h horizon and by 49 at 48 h — with no user action
/// on the scrubber at all, and no way to see it had happened. A fixed split
/// costs `1/NOW_SPLIT` and nothing else, whatever the horizon is, which is
/// why the number asserted here is a **ratio**: it is scale-free, so it does
/// not move when the rail does.
#[test]
fn the_forecast_rail_does_not_crush_the_past() {
    let plain = RailRegions::past_only(SPAN);
    let plain_rate = past_secs_per_px(&plain, WIDE_TRAVEL);

    for horizon in [HORIZON_18H, HORIZON_48H] {
        let rail = forecast_rail(SPAN, horizon);
        let ratio = past_secs_per_px(&rail, WIDE_TRAVEL) / plain_rate;
        assert!(
            ratio <= 2.0,
            "a {} h forecast layer multiplied the past's seconds per pixel by \
             {ratio:.2}. The rail must cost the past a constant 1/NOW_SPLIT \
             = {:.4}x, not a share that collapses as the horizon grows",
            horizon / 3600.0,
            1.0 / NOW_SPLIT,
        );
        assert!(
            (ratio - 1.0 / NOW_SPLIT).abs() < 0.01,
            "the past's cost was {ratio:.4}x, not the {:.4}x the split \
             fixes it at - the split is not what is dividing the rail",
            1.0 / NOW_SPLIT,
        );
    }

    // Non-vacuity: the two horizons really do give different rails, so the
    // constant ratio above is a property and not an artifact of one input.
    assert!(
        (future_secs_per_px(&forecast_rail(SPAN, HORIZON_48H), WIDE_TRAVEL)
            - future_secs_per_px(&forecast_rail(SPAN, HORIZON_18H), WIDE_TRAVEL))
        .abs()
            > 1.0,
        "the 18 h and 48 h rails have the same forecast scale, so this test \
         never distinguished them"
    );
}

/// **A pane with no forecast timeline gets the rail it always had** — the
/// algebraic half of the promise; `input_harness::tests` holds the painted
/// half.
///
/// The old rail was one closed form, `now - span * (1 - frac)`, spread over
/// the whole travel. It is written out here rather than called, so that
/// re-deriving it from [`RailRegions`] could not make this agree with itself.
#[test]
fn a_rail_with_no_forecast_is_the_single_linear_past_rail() {
    let rail = RailRegions::past_only(SPAN);
    assert_eq!(rail.split, 1.0, "a past-only rail puts now at the far end");
    assert!(!rail.has_future());

    for step in 0..=100 {
        let frac = step as f32 / 100.0;
        let was = -SPAN * (1.0 - frac);
        assert!(
            (rail.offset_secs(frac) - was).abs() < 0.5,
            "at frac {frac} the rail names {} s, where it named {was} s",
            rail.offset_secs(frac),
        );
        assert!(
            (rail.frac_of_offset(was) - frac).abs() < 1e-4,
            "the resting position for {was} s of age is {}, not {frac}",
            rail.frac_of_offset(was),
        );
    }
}

/// **The tie at `now` is history.** The same `<=` the frame lookup uses, so a
/// forecast frame changes region at the instant its valid time arrives.
///
/// **What this does not assert, and why.** The brief's floor for this rule
/// was that `<` would leave the frame "in neither region for one instant".
/// It would not: the two regions meet *continuously* at the boundary —
/// `frac_of_offset(0)` is `split` from either side — so a `<` here changes
/// which arm answers and never what it answers. This pins the documented rule
/// where it is used (`Gui::commit_archive_scrub` decides both the
/// quantising and radar's fetch off it); the continuity is asserted beside
/// it, because that is the fact that makes no gap possible.
#[test]
fn the_tie_at_now_is_history() {
    let rail = forecast_rail(SPAN, HORIZON_48H);
    let now = chrono::DateTime::from_timestamp(1_760_000_000, 0)
        .expect("a fixed instant")
        .naive_utc();

    assert_eq!(rail.side_of(now, now), RailSide::Past, "now is history");
    assert_eq!(
        rail.side_of(now - chrono::Duration::seconds(1), now),
        RailSide::Past,
    );
    assert_eq!(
        rail.side_of(now + chrono::Duration::seconds(1), now),
        RailSide::Future,
    );

    // The regions meet, with no gap and no overlap, at exactly `split`.
    assert!((rail.frac_of_offset(0.0) - rail.split).abs() < 1e-6);
    assert!((rail.frac_of_offset(-1.0) - rail.split).abs() < 1.0 / WIDE_TRAVEL);
    assert!((rail.frac_of_offset(1.0) - rail.split).abs() < 1.0 / WIDE_TRAVEL);
    assert!((rail.offset_secs(rail.split)).abs() < 0.5);
}

/// **`now` stays on one pixel while the frames move across it.**
///
/// The clock advances all day and the run rolls under it; what a reader
/// steers by is the colour break, so the break may not wander. A forecast
/// frame's place on the rail walks steadily left and crosses the break
/// exactly when the wall clock reaches its valid time.
#[test]
fn the_boundary_holds_still_while_frames_cross_it() {
    let rail = forecast_rail(SPAN, HORIZON_48H);
    let valid_in = 6.0 * 3600.0;

    let mut previous = f32::INFINITY;
    let mut crossings = 0;
    for elapsed_mins in 0..=(8 * 60) {
        let elapsed = (elapsed_mins * 60) as f32;
        let frac = rail.frac_of_offset(valid_in - elapsed);
        assert!(
            frac <= previous + 1e-6,
            "the frame's place on the rail moved right as the clock advanced"
        );
        if previous > rail.split && frac <= rail.split {
            crossings += 1;
            assert!(
                (elapsed - valid_in).abs() < 61.0,
                "the frame crossed the boundary {} minutes off its valid \
                 time",
                (elapsed - valid_in).abs() / 60.0,
            );
        }
        previous = frac;
        // The break itself is a constant of the rail, not of the clock.
        assert_eq!(rail.split, NOW_SPLIT);
    }
    assert_eq!(
        crossings, 1,
        "the frame crossed the boundary {crossings} times"
    );
}

/// **The live zone is reachable on a wide rail and does not eat the forecast
/// region on a narrow one** — the two halves of the same rule, which a plain
/// distance in points cannot satisfy at both widths.
///
/// 6.0 pt is 1.26% of the 477 pt rail and 7.8% of the 77 pt one, where it
/// would claim 25.9% of that rail's 23.15 pt forecast region: a quarter of
/// what the user is aiming at would answer "live". The share cap is what
/// stops it, and it is asserted at both widths because a rule checked at one
/// is a rule checked nowhere.
#[test]
fn the_live_zone_never_eats_the_forecast_region() {
    for travel in [WIDE_TRAVEL, NARROW_TRAVEL] {
        let rail = forecast_rail(SPAN, HORIZON_48H);
        let zone = rail.live_snap_px(travel);
        let future_px = (1.0 - rail.split) * travel;
        assert!(
            zone <= LIVE_SNAP_MAX_FUTURE_SHARE * future_px + 1e-4,
            "at {travel} pt of travel the live zone is {zone:.2} pt, which is \
             {:.1}% of the {future_px:.2} pt forecast region",
            100.0 * zone / future_px,
        );
        assert!(
            zone > 0.0 && zone <= LIVE_SNAP_PX,
            "the live zone at {travel} pt of travel is {zone:.2} pt"
        );

        // A release at the far right end is a forecast instant, never live -
        // which is the assertion the old fraction rule inverted.
        assert!(
            !rail.is_live_release(1.0, travel),
            "the far end of a rail with a forecast region answered live"
        );
        assert!(
            rail.is_live_release(rail.split, travel),
            "the now boundary did not answer live"
        );
    }

    // On the wide rail the plain distance stands; only the narrow one is
    // capped. Both arms of the `min` are therefore live.
    let wide = forecast_rail(SPAN, HORIZON_48H).live_snap_px(WIDE_TRAVEL);
    let narrow = forecast_rail(SPAN, HORIZON_48H).live_snap_px(NARROW_TRAVEL);
    assert!(
        (wide - LIVE_SNAP_PX).abs() < 1e-4 && narrow < LIVE_SNAP_PX,
        "wide {wide:.2} narrow {narrow:.2}: the cap either never binds or \
         always binds, so one arm of the rule is dead"
    );

    // A rail with no forecast region has nothing to protect and keeps the
    // plain distance at either width.
    for travel in [WIDE_TRAVEL, NARROW_TRAVEL] {
        assert_eq!(
            RailRegions::past_only(SPAN).live_snap_px(travel),
            LIVE_SNAP_PX
        );
    }
}

/// **A release in the forecast region lands on the layer's frame grid.**
///
/// 49 frames across the region's 143 pt is 2.92 pt per frame; a hand does not
/// resolve that, so the release is quantised. The grid is the transport
/// layer's own declared step, and a past release is not quantised at all
/// because a past instant is a genuine free choice.
#[test]
fn a_forecast_release_lands_on_the_layers_frame_grid() {
    let rail = forecast_rail(SPAN, HORIZON_48H);
    let base = chrono::DateTime::from_timestamp(1_760_000_000, 0)
        .expect("a fixed instant")
        .naive_utc();

    for offset_mins in [0, 7, 29, 31, 59, 61, 1_439] {
        let snapped = rail.snap_future(base + chrono::Duration::minutes(offset_mins));
        assert_eq!(
            snapped.and_utc().timestamp() % rail.step_secs,
            0,
            "a forecast release landed at {snapped}, which is not on the \
             layer's {} s grid",
            rail.step_secs,
        );
        let moved = (snapped - (base + chrono::Duration::minutes(offset_mins)))
            .num_seconds()
            .abs();
        assert!(
            moved <= rail.step_secs / 2,
            "the snap moved the release {moved} s, more than half a step"
        );
    }

    // A layer that declares no step is not quantised: the grid is the
    // layer's, never this file's.
    let unstepped = RailRegions {
        step_secs: 0,
        ..rail
    };
    let odd = base + chrono::Duration::minutes(7);
    assert_eq!(unstepped.snap_future(odd), odd);
}

/// **The travel is the rect less one end inset per end**, and the inset
/// follows the style's handle shape rather than being assumed.
#[test]
fn the_travel_is_the_rect_less_one_handle_inset_per_end() {
    // The transport's measured rail rect at 1400 x 900.
    let rect = egui::Rect::from_min_max(egui::pos2(502.0, 831.0), egui::pos2(990.0, 849.0));
    let rectangular = egui::style::HandleShape::Rect { aspect_ratio: 0.75 };
    assert!(
        (slider_travel_px(rect, rectangular) - WIDE_TRAVEL).abs() < 0.05,
        "the measured 488.0 pt rect gives {} pt of travel, not {WIDE_TRAVEL}",
        slider_travel_px(rect, rectangular),
    );
    // A circular handle shortens by the full radius, not by the aspect ratio
    // of it, so the two shapes must not agree.
    assert!(
        slider_travel_px(rect, egui::style::HandleShape::Circle)
            < slider_travel_px(rect, rectangular),
    );
}

// -- WI-11b: the loop rail's break ---------------------------------------

/// A fixed instant, so nothing below depends on when it is run.
fn fixed_now() -> chrono::NaiveDateTime {
    chrono::DateTime::from_timestamp(1_760_000_000, 0)
        .expect("a fixed instant")
        .naive_utc()
}

/// A loop of `total` frames `step` seconds apart, the `past` oldest of them
/// at or before `now` - **and the last of those landing exactly on `now`**,
/// so the tie rule is exercised by construction rather than by a case bolted
/// on beside it.
fn straddling_loop(
    now: chrono::NaiveDateTime,
    total: usize,
    past: usize,
    step: i64,
) -> Vec<LoopFrame> {
    (0..total)
        .map(|i| LoopFrame {
            timestamp: now + chrono::Duration::seconds((i as i64 - (past as i64 - 1)) * step),
            image: None,
            render_in_flight: false,
            render_failed: false,
        })
        .collect()
}

/// **The loop rail breaks where the frames straddle `now`, and every frame's
/// handle sits clear of the break.**
///
/// The break is not a constant here and must not be: a loop's frames are
/// evenly spaced whatever their stamps are, so the colour says which frames
/// are observed and which are forecast, one frame at a time. A pinned
/// [`NOW_SPLIT`] would call the first six frames of an f00-f48 run history.
///
/// The expected position is written from the two straddling frames' own
/// handle positions - the midpoint of `(past-1)/(total-1)` and
/// `past/(total-1)` - rather than by re-spelling the closed form the function
/// under test uses, so the two cannot agree by construction.
#[test]
fn the_loop_break_sits_between_the_frames_that_straddle_now() {
    const TOTAL: usize = 9;
    const PAST: usize = 5;
    const STEP: i64 = 300;
    let now = fixed_now();
    let frames = straddling_loop(now, TOTAL, PAST, STEP);
    let spacing = 1.0 / (TOTAL - 1) as f32;

    let last_past = (PAST - 1) as f32 * spacing;
    let first_future = PAST as f32 * spacing;
    let expected = 0.5 * (last_past + first_future);

    let split = loop_rail_split(&frames, now).expect("frames straddling now carry a break");
    assert!(
        (split - expected).abs() < 1e-5,
        "the break is at {split:.4} of the travel, which is frame {:.2}; the \
         frames straddling now are {} at {last_past:.4} and {} at \
         {first_future:.4}, so it belongs at {expected:.4}. A break pinned at \
         a fixed fraction sits at NOW_SPLIT = {NOW_SPLIT:.4}, which is frame \
         {:.2} of {TOTAL}",
        split / spacing,
        PAST - 1,
        PAST,
        NOW_SPLIT / spacing,
    );

    // **Why the midpoint and not either frame's own position.** Both the
    // trailing edge of the last past frame and the leading edge of the first
    // forecast frame would put one handle exactly on the boundary, and it is
    // the boundary frame whose side the reader most needs to read off.
    for (i, frame) in frames.iter().enumerate() {
        let at = i as f32 * spacing;
        assert!(
            (at - split).abs() > 1e-3,
            "frame {i}'s handle sits on the break at {split:.4}, so its \
             colour says nothing about which side it is on"
        );
        assert_eq!(
            at < split,
            frame.timestamp <= now,
            "frame {i} is at {at:.4} of the travel and stamped {}, which the \
             break at {split:.4} puts on the wrong side",
            frame.timestamp,
        );
    }

    // **The tie at `now` is history.** Frame PAST-1 is stamped exactly `now`
    // and lies left of the break; a `<` there would drag the break one whole
    // frame left.
    assert_eq!(frames[PAST - 1].timestamp, now);
    assert!(
        split > last_past,
        "the frame stamped exactly now is at {last_past:.4} and the break is \
         at {split:.4}, so a frame valid at this instant is being called \
         forecast"
    );

    // **The break tracks the frames.** One more frame falling behind the wall
    // clock moves it exactly one frame's width right, and nothing else.
    let rolled =
        loop_rail_split(&frames, now + chrono::Duration::seconds(STEP)).expect("still straddling");
    assert!(
        (rolled - (split + spacing)).abs() < 1e-5,
        "the clock passing one more frame moved the break from {split:.4} to \
         {rolled:.4}; one frame's width is {spacing:.4}"
    );
}

/// **The three ends of the range**, each of which the paint has to answer
/// differently - and the all-history one is the common case, because every
/// radar loop is it.
#[test]
fn a_loop_with_nothing_to_break_has_no_break() {
    const STEP: i64 = 300;
    let now = fixed_now();

    assert_eq!(
        loop_rail_split(&[], now),
        None,
        "an empty loop drew a break across frames it does not have"
    );

    // Every frame at or before now - a radar loop. No break, and so no
    // two-colour path and no change to the bar at all.
    for total in [1, 2, 9] {
        assert_eq!(
            loop_rail_split(&straddling_loop(now, total, total, STEP), now),
            None,
            "a {total}-frame loop with no forecast frame in it drew a break"
        );
    }

    // Every frame after now - f00..f48 of a run published ahead of the clock.
    // The break is the rail's left edge: no past region, and emphatically not
    // a fixed fraction, which would call most of the run history.
    for total in [1, 2, 9] {
        assert_eq!(
            loop_rail_split(&straddling_loop(now, total, 0, STEP), now),
            Some(0.0),
            "a {total}-frame loop with no history in it did not put the break \
             at the far left"
        );
    }

    // And the break is strictly inside the rail whenever both regions have a
    // frame in them, at every count, so neither end case leaks into the
    // straddling one.
    for total in 2..=12 {
        for past in 1..total {
            let split = loop_rail_split(&straddling_loop(now, total, past, STEP), now)
                .expect("a straddling loop has a break");
            assert!(
                split > 0.0 && split < 1.0,
                "{past} of {total} frames in history put the break at \
                 {split:.4}, which is off the rail"
            );
        }
    }
}
