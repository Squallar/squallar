//! The floor is framed on the box, and the zoom gesture moves the eye and
//! nothing else.
//!
//! The eleven tests this file used to hold were all about deriving a box from a
//! viewport — containment on four sides, the quantum that stopped the pane
//! rebuilding for ever, the ceiling and floor the derivation was clamped to.
//! None of those things exists: a 3D pane's region is stored rather than
//! measured, so there is no measurement to contain, no per-frame key to keep
//! still and no derived extent to clamp. They are named in the change's test
//! accounting rather than reconstructed here — a test for a concept that has
//! been deleted cannot fail, and a suite full of tests that cannot fail is the
//! defect this codebase keeps having to fix.
//!
//! What is pinned instead is the implication running the *other* way — that a
//! strip framed on a stored box covers the whole of it, tightly, at every
//! latitude and for every shape of pane — plus the arithmetic between a wheel
//! notch and a standoff and the refusals that keep a bad frame's number out of
//! the camera. The *gate* — which pane a gesture belongs to — is pinned through
//! the real UI by `ui_map::volume_arm_tests`, because it is a question about
//! layers and hover that no unit fixture can ask honestly.
//!
//! Since the selector came back there is a third subject here: the one writer
//! of a stored region. [`RegionDrag`]'s arithmetic — Chebyshev, capped, refused
//! below the resampler's floor, square on both axes — is pinned as a function,
//! because every one of those is a decision that can be got wrong silently. The
//! *gesture* around it is `ui_map::region_pick_tests`, for the reason the gate
//! is over there.

use super::{
    COVERAGE_MARGIN, COVERAGE_TARGET, MAX_FRAMING_PASSES, MAX_ZOOM_LEVEL, MIN_ZOOM_LEVEL,
    RegionDrag, corners_for, dolly_for_step, ground_half_extent, solve_viewport,
    viewport_for_region, wheel_rate_correction, zoom_step,
};

/// A frame whose timing is unusable leaves walkers exactly as it found it.
///
/// **This is what makes [`wheel_rate_correction`]'s finiteness test reachable**,
/// and it is reachable: `f32::clamp` is two `if`s that a NaN fails both of, so a
/// NaN `stable_dt` comes out of the clamp as a NaN rather than being bounded
/// away. `stable_dt` is a wall-clock difference the platform hands egui, so that
/// is an input and not a hypothetical — and the correction is multiplied into
/// `smooth_scroll_delta`, which walkers puts into `MapMemory::zoom`, which is
/// *stored*. One NaN frame would poison the pane's zoom until it was rebuilt.
///
/// 1.0 rather than a refusal because leaving walkers alone is the conservative
/// answer: the frame zooms by whatever walkers would have done unaided, which is
/// the behaviour that shipped before this module existed.
#[test]
fn a_frame_with_unusable_timing_leaves_walkers_alone() {
    let with = |stable_dt: f32, predicted_dt: f32| {
        let mut input = egui::InputState::default();
        input.stable_dt = stable_dt;
        input.predicted_dt = predicted_dt;
        wheel_rate_correction(&input)
    };
    assert_eq!(
        with(f32::NAN, 1.0 / 60.0),
        1.0,
        "a NaN frame time survives the clamp, so it has to be caught here",
    );
    assert_eq!(
        with(1.0 / 60.0, 0.0),
        1.0,
        "a zero predicted_dt clamps the multiplier to zero, which would divide by it",
    );
    // The other side of the same guard: an infinity *is* bounded by the clamp,
    // so it must come out as a real correction rather than be refused with it.
    assert!(
        (with(f32::INFINITY, 1.0 / 60.0) - 0.5).abs() < 1e-6,
        "an infinite frame time clamps to predicted_dt * 2, a real correction",
    );
    // The control: an ordinary 60 Hz frame is corrected by exactly nothing,
    // because 1/60 is the rate the constant is calibrated at.
    assert!(
        (with(1.0 / 60.0, 1.0 / 60.0) - 1.0).abs() < 1e-6,
        "a 60Hz frame must need no correction at all",
    );
}

/// Opening a second [`super::steady_wheel`] inside the first is refused.
///
/// The correction is a *multiplication* into a shared field, so nesting squares
/// it: at the measured web frame time that is a 17× scroll rather than a 1×,
/// and the map would leap seventeen zoom levels on one notch. Nothing nests
/// today — the one call site is a leaf — so this exists to keep the guard from
/// being the thing it is guarding against: an unreachable protection nobody
/// ever proved fires. It fires.
///
/// Debug-only because `debug_assert` compiles out of a release build, and a
/// `should_panic` test that cannot panic is a test that fails.
#[cfg(debug_assertions)]
#[test]
#[should_panic(expected = "nesting squares the correction")]
fn nesting_the_wheel_guard_is_refused() {
    let ctx = egui::Context::default();
    super::steady_wheel(&ctx, || super::steady_wheel(&ctx, || ()));
}

/// An input frame carrying `scroll` points of wheel and nothing else, at
/// egui's own default 60 Hz timing.
///
/// `zoom_factor_delta` is private and defaults to exactly 1.0, which is the
/// "no pinch this frame" value [`zoom_step`] tests for — so a default frame
/// with a scroll written into it drives the wheel branch, which is the one a
/// desktop user reaches.
fn scroll_frame(scroll: f32) -> egui::InputState {
    let mut input = egui::InputState::default();
    input.smooth_scroll_delta.y = scroll;
    input
}

/// The whole of one wheel notch, in zoom levels, at a frame rate of `dt`.
///
/// **This is the measurement, not a fixture.** `scroll_frame` writes
/// `smooth_scroll_delta` by hand and so cannot see the thing being tested:
/// egui does not hand a frame the scroll that arrived during it, it hands the
/// frame a *slice* of an accumulator it drains over about 100 ms
/// (`input_state/wheel_state.rs::after_events`). So one notch is spread across
/// however many frames fit in that window, and what a user feels is the
/// **sum**, not any one frame's share. Only a real `egui::Context` run frame by
/// frame produces that, which is what this does.
///
/// Runs until the accumulator is dry and stays dry, so the answer is the whole
/// gesture rather than a prefix of it.
fn notch_levels(dt: f64, notch_points: f32) -> f64 {
    // The shipped `predicted_dt`. `egui-winit` never writes the field — it is
    // not mentioned anywhere in the crate — so it holds `RawInput::default()`'s
    // 60 Hz for the life of the process, whatever the app is really running at.
    // That is what bounds `zoom_step`'s clamp, so a measurement that moved it
    // with the frame rate would be measuring a program nobody runs.
    const SHIPPED_PREDICTED_DT: f32 = 1.0 / 60.0;
    let ctx = egui::Context::default();
    let mut time = 0.0;
    let mut total = 0.0;
    let mut quiet = 0;
    for frame in 0..2000 {
        let events = if frame == 1 {
            vec![egui::Event::MouseWheel {
                unit: egui::MouseWheelUnit::Point,
                delta: egui::vec2(0.0, notch_points),
                phase: egui::TouchPhase::Move,
                modifiers: egui::Modifiers::default(),
            }]
        } else {
            Vec::new()
        };
        ctx.begin_pass(egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1000.0, 800.0),
            )),
            time: Some(time),
            predicted_dt: SHIPPED_PREDICTED_DT,
            events,
            ..Default::default()
        });
        let step = ctx.input(zoom_step);
        let _ = ctx.end_pass();
        total += step;
        if frame > 1 {
            if step == 0.0 {
                quiet += 1;
                if quiet > 30 {
                    break;
                }
            } else {
                quiet = 0;
            }
        }
        time += dt;
    }
    total
}

/// The frame rates this app is measured at: a 240 Hz desktop, a hitched web
/// frame, and everything between.
///
/// The last row is not hypothetical — it is the measured p50 frame duration of
/// the shipped web build in Chromium with the NWS alerts overlay on, where the
/// overlay raster runs inline on the main thread.
const FRAME_RATES: &[(&str, f64)] = &[
    ("240Hz", 1.0 / 240.0),
    ("120Hz", 1.0 / 120.0),
    ("60Hz", 1.0 / 60.0),
    ("30Hz", 1.0 / 30.0),
    ("10Hz", 0.1),
    ("3.5Hz web p50", 0.2895),
];

/// How far apart two frame rates' answers are allowed to land, as a fraction.
///
/// Set by what the `f32` accumulator inside egui's own smoothing leaves behind
/// — 4.4e-8 at the worst of the rates above — with a decimal order of margin.
/// The defect this bounds was a factor of **four**, so this is four hundred
/// thousand times tighter than the thing it is watching for.
const RATE_TOLERANCE: f64 = 1e-6;

/// **The reported bug, as a gate**: one wheel notch is the same zoom whatever
/// the frame rate.
///
/// > scrolling behavior has gotten choppy. When I'm zoomed out wide, a zoom
/// > moves a lot more than a close zoom, which is a tiny jump. I think it has
/// > to do with the processing done during zoom.
///
/// The user's diagnosis was right, and this is the arithmetic of it. Before the
/// fix this table read 0.50 / 0.50 / 1.00 / 2.00 / 2.00 / 2.00 — a 4× spread
/// driven by nothing but how long the frame took, saturating at both ends of
/// walkers' `clamp(predicted_dt * 0.5, predicted_dt * 2.0)`. Zoomed out wide is
/// when the app is slowest, so that is where a notch was worth a *quadrupling*
/// of scale; zoomed in the frames are quick and the same notch was a 1.41×.
///
/// One level per notch is the mapping every map application has, and
/// [`super::POINTS_PER_ZOOM_LEVEL`] is what holds it.
///
/// The tolerance is [`RATE_TOLERANCE`], which is what is left of the coupling
/// rather than a margin for it: at 240 Hz the notch is spread over about
/// twenty-five frames and egui's accumulator is `f32`, so the sum lands 4.4e-8
/// off. That is the arithmetic of the filter, not the frame time — a returning
/// bug moves this by factors, not by ulps.
#[test]
fn a_notch_is_the_same_zoom_at_every_frame_rate() {
    for &(name, dt) in FRAME_RATES {
        let levels = notch_levels(dt, 120.0);
        assert!(
            (levels - 1.0).abs() < RATE_TOLERANCE,
            "one notch at {name} moved {levels} zoom levels, not the 1.0 it moves \
             everywhere else - the frame time is back in the zoom step",
        );
    }
}

/// The gesture is linear in how far the wheel turned, at any frame rate.
///
/// The fix could have been a clamp — pin the step and cap it — and that would
/// pass the table above while quietly making a fast flick worth the same as a
/// slow one. Two notches are two levels, and a half notch is half a level.
#[test]
fn the_zoom_follows_how_far_the_wheel_turned() {
    for &(name, dt) in FRAME_RATES {
        for (points, want) in [(240.0, 2.0), (60.0, 0.5), (-120.0, -1.0)] {
            let levels = notch_levels(dt, points);
            assert!(
                (levels - want).abs() < RATE_TOLERANCE * want.abs(),
                "{points} points at {name} moved {levels} zoom levels, not {want}",
            );
        }
    }
}

/// One wheel notch over the **plan view**, in zoom levels, at a frame rate of
/// `dt` — driven through the real UI and the real `walkers::Map`.
///
/// The two tests above measure [`zoom_step`], which is the 3D pane's arm. The
/// plan view does not go through it: it reaches `walkers::Map::zoom_delta`,
/// which holds its own copy of the frame-time multiplier and cannot be
/// configured out of it. So the plan view is measured where it actually lives —
/// through `render_panes`, through walkers, off `MapMemory::zoom` — and
/// [`super::steady_wheel`] is what has to make this flat.
///
/// Runs until the zoom stops moving, for the same reason [`notch_levels`] does:
/// a notch is spread over about 100 ms of frames, so a fixed frame count reads
/// a different fraction of the gesture at every rate and would report the
/// coupling as fixed when it was not.
fn plan_view_notch_levels(dt: f64) -> f64 {
    use crate::input_harness::InputHarness;

    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    h.set_pane_count(1);
    h.load_scan("KTLX");
    h.gui_mut()
        .pane_mut(0)
        .expect("a pane")
        .map_memory
        .set_zoom(9.0)
        .expect("9 is inside walkers' range");
    h.warm_up();

    let rect = h.pane_rects()[0];
    let zoom = |h: &mut InputHarness| h.gui_mut().pane(0).expect("a pane").map_memory.zoom();
    let before = zoom(&mut h);

    h.scroll_at(rect.center(), egui::vec2(0.0, 120.0));
    let mut last = before;
    let mut quiet = 0;
    for _ in 0..600 {
        h.frame_after(dt);
        let now = zoom(&mut h);
        if now == last {
            quiet += 1;
            if quiet > 40 {
                break;
            }
        } else {
            quiet = 0;
            last = now;
        }
    }
    last - before
}

/// **The reported bug on the surface it was reported from.** The plan view is
/// the main map, it is where the overlays that make a frame take 289 ms
/// (measured at main@ebe0ad3b, 2026-08-12 web-baseline campaign;
/// instrumentation 3673d316) are drawn, and it is the arm that reaches
/// walkers' multiplier rather than this
/// module's.
///
/// Fixing [`zoom_step`] alone would have left this one at the 4× spread *and*
/// broken the agreement between the two panes that
/// `a_scroll_moves_a_3d_pane_the_same_distance_it_moves_a_plan_view` pins — a
/// half fix that reads as a whole one, because the 3D pane is the one with the
/// unit tests. Hence [`super::steady_wheel`], and hence this.
///
/// # The 1.3% this does not remove, and why it is left
///
/// Measured after the fix: 0.9867 at 240 Hz, 0.9983 at 120, 1.0000 at 60 and
/// 30, 0.9990 at 10 and below. A **1.3% spread**, against the 4× it replaced.
///
/// What is left is a *second* frame-rate coupling in walkers, and a much
/// smaller one: `handle_gestures` only zooms at all when
/// `(zoom_delta - 1.0).abs() > 0.001`, so a frame carrying less than 0.24
/// points of wheel is dropped on the floor rather than accumulated. The faster
/// the frame rate the more of the notch arrives in slices that small — at
/// 240 Hz the exponential tail spends about 1.6 of its 120 points under the
/// bar, which is the 1.3%.
///
/// Left because removing it means carrying the sub-threshold remainder across
/// frames — real state, in the input path, to recover a part in eighty of a
/// gesture nobody can feel. The defect this test exists for was a factor of
/// four in the same quantity. The bound below is set where the measurement
/// actually is so that a *regression* moves it, rather than at a round number
/// that would let one hide.
#[test]
fn a_notch_moves_the_plan_view_the_same_distance_at_every_frame_rate() {
    let measured: Vec<(&str, f64)> = FRAME_RATES
        .iter()
        .map(|&(name, dt)| (name, plan_view_notch_levels(dt)))
        .collect();
    for &(name, levels) in &measured {
        assert!(
            (levels - 1.0).abs() < 0.02,
            "one notch on the plan view at {name} moved {levels} zoom levels, \
             not 1.0 - walkers' frame-time multiplier is reaching the map again. \
             Whole sweep: {measured:?}",
        );
    }
    // The user's complaint was the *spread* — "a zoom moves a lot more than a
    // close zoom" — so it is asserted directly, and not merely implied by six
    // separate distances from 1.0.
    let widest = measured.iter().map(|&(_, l)| l).fold(f64::MIN, f64::max);
    let tightest = measured.iter().map(|&(_, l)| l).fold(f64::MAX, f64::min);
    assert!(
        widest / tightest < 1.02,
        "the same notch moved the plan view {widest} zoom levels at one frame \
         rate and {tightest} at another - a {:.1}x spread. Whole sweep: {measured:?}",
        widest / tightest,
    );
}

/// **The property the user asked for, in the one place it is arithmetic**: a
/// zoom level in is a halving of the eye's standoff, so the ground the pane
/// looks at halves while the box under it does not move at all.
///
/// One Web Mercator zoom level is a factor of two of ground per point by the
/// projection's definition, and a perspective camera sees `2 · d · tan(fov/2)`
/// of ground at its pivot plane — linear in `d`. So the conversion is `2^step`
/// exactly, and this is what says the exponent has not been dropped, doubled or
/// inverted.
#[test]
fn one_zoom_level_is_one_halving_of_the_standoff() {
    assert_eq!(dolly_for_step(1.0), 2.0, "a level in halves the standoff");
    assert_eq!(dolly_for_step(-1.0), 0.5, "a level out doubles it");
    assert_eq!(dolly_for_step(2.0), 4.0, "two levels in is two halvings");
}

/// A frame with no gesture leaves the eye exactly where it is.
///
/// The neutral value is 1.0 and not 0.0 because this is a *divisor*, and the
/// two are one character apart at the call site. A 0.0 would divide the
/// standoff by zero and put an infinity in the camera, whose staleness key
/// would then never equal itself — a permanently rebuilding pane whose only
/// symptom is a fan.
#[test]
fn no_gesture_leaves_the_eye_where_it_is() {
    assert_eq!(dolly_for_step(0.0), 1.0);
}

/// Zooming out undoes zooming in, exactly, at every magnitude a gesture
/// produces.
///
/// `2^s · 2^-s = 1` is arithmetic, but it is arithmetic in `f64` that is then
/// rounded to `f32` twice, and the thing being pinned is that a user who
/// scrolls one notch each way finds the pane where they left it rather than
/// drifting a little further out every round trip.
#[test]
fn zooming_out_undoes_zooming_in() {
    for step in [0.05_f64, 0.25, 0.5, 1.0, 3.0, 7.0] {
        let round_trip = f64::from(dolly_for_step(step)) * f64::from(dolly_for_step(-step));
        assert!(
            (round_trip - 1.0).abs() < 1e-6,
            "a {step}-level round trip scaled the standoff by {round_trip}",
        );
    }
}

/// A gesture frame that produced a `NaN` or an infinity does not reach the
/// camera.
///
/// [`crate::pane::OrbitCamera::nudge`] would refuse such a factor itself — but
/// it refuses the **whole delta**, which throws away the same frame's orbit and
/// pan. Answering the identity here keeps those two verbs working through a
/// frame whose scroll arrived unusable, which is what a user dragging and
/// scrolling at once actually does.
#[test]
fn a_non_finite_gesture_does_not_reach_the_camera() {
    for step in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        assert_eq!(dolly_for_step(step), 1.0, "step {step} was not refused");
    }
}

/// A finite step whose exponential is not finite is refused too.
///
/// The one arithmetic step between [`zoom_step`]'s own finiteness check and the
/// camera's: `2^129` is past `f32::MAX` and `2^-150` is under `f32::MIN_POSITIVE`
/// even as a subnormal, so a finite input can still produce an infinity or a
/// zero. Neither may be divided into a standoff, and testing the *output* rather
/// than bounding the input is what keeps this true if the exponent's base ever
/// changes.
#[test]
fn a_step_that_overflows_the_factor_is_refused() {
    assert_eq!(dolly_for_step(1000.0), 1.0, "an overflow to infinity");
    assert_eq!(dolly_for_step(-1000.0), 1.0, "an underflow to zero");
}

/// Scrolling up brings the eye in and scrolling down takes it out.
///
/// Convention rather than arithmetic — a sign error here dollies perfectly well
/// and merely feels backwards, which is the kind of defect that survives review
/// — and it is walkers' own convention, so that a wheel means the same thing
/// over a 3D pane as over the plan view beside it.
#[test]
fn scrolling_up_brings_the_eye_in() {
    assert!(
        dolly_for_step(zoom_step(&scroll_frame(50.0))) > 1.0,
        "a scroll up must divide the standoff by more than one",
    );
    assert!(
        dolly_for_step(zoom_step(&scroll_frame(-50.0))) < 1.0,
        "a scroll down must divide it by less than one",
    );
    assert_eq!(
        dolly_for_step(zoom_step(&scroll_frame(0.0))),
        1.0,
        "a frame with no wheel is not a gesture",
    );
}

/// The range the gesture can actually reach, measured off the camera's own
/// constants rather than restated.
///
/// The module doc claims 160× and 7.32 zoom levels as the bound the box's
/// bounds were replaced by. Both are read back here, so a change to either
/// constant fails this instead of leaving the prose describing a range that no
/// longer exists.
#[test]
fn the_gesture_runs_from_inside_the_box_to_well_outside_it() {
    let span = f64::from(crate::pane::MAX_EYE_DISTANCE / crate::pane::MIN_EYE_DISTANCE);
    assert!(
        (span - 160.0).abs() < 0.5,
        "the standoff range is {span}x, not the 160x the module doc states",
    );
    let levels = span.log2();
    assert!(
        (levels - 7.32).abs() < 0.01,
        "the standoff range is {levels} zoom levels, not the 7.32 the module doc states",
    );
}

/// A box of `half_east_km` × `half_north_km` about a point.
fn half(east_km: f64, north_km: f64) -> rustdar_radar::voxel::HalfExtentKm {
    rustdar_radar::voxel::HalfExtentKm { east_km, north_km }
}

/// **The property the floor exists for**: the strip covers the whole box, on
/// every axis, for every shape of box and pane the application can produce.
///
/// `floor_colour` clips the floor to the mirror's `0..1` and answers
/// *transparent* outside it — off the mirror is ground the strip is not showing
/// and has no colour to report, and clamping would smear the strip's border
/// across the rest of the box as if it were map. So a strip that falls short on
/// any side is a volume standing on nothing along that side, which is the exact
/// symptom the stored region was going to reintroduce and this closes.
///
/// Swept rather than sampled once: the binding axis changes with the pane's
/// aspect and the box's, and a framing that solved for the wrong one would pass
/// on a square fixture. The latitudes matter for the same reason — Mercator's
/// north lane is the one the single-logarithm solve is only approximate on.
#[test]
fn the_framed_strip_covers_the_whole_box() {
    for (w, h) in [
        (700.0, 450.0),
        (450.0, 700.0),
        (900.0, 200.0),
        (400.0, 400.0),
    ] {
        let rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(w, h));
        for lat in [0.0, 25.0, 35.33, 49.0, 64.8] {
            let centre = walkers::lat_lon(lat, -97.28);
            for box_half in [
                half(460.125, 460.125),
                half(300.0, 300.0),
                half(120.0, 75.0),
                half(75.0, 120.0),
                half(10.0, 10.0),
            ] {
                let memory = viewport_for_region(rect, centre, box_half)
                    .expect("a finite box in a pane with area must be framable");
                let covered = ground_half_extent(rect, &memory, centre)
                    .expect("a framed viewport must be measurable");
                assert!(
                    covered.east_km >= box_half.east_km && covered.north_km >= box_half.north_km,
                    "a {w}x{h} strip at {lat}N framed on {box_half:?} covers only \
                     {covered:?} - the volume stands on transparency out there",
                );
            }
        }
    }
}

/// How far a delivered margin may sit from the band's ends before it counts as
/// outside it, as a fraction of the end.
///
/// The solve computes its target and its step in one arithmetic; a test measures
/// what came back by building a **fresh** projector and running the geodesy
/// again. The two agree to a few parts in 1e8 rather than exactly, and the band
/// this file asserts is 1e-3 wide, so an exact `contains` would be asserting
/// that a round trip through Mercator and a haversine is lossless. Measured
/// worst deviation over the 865 280-framing sweep: 2.2e-8 below
/// [`COVERAGE_MARGIN`] would be needed to trip the floor, and 8.5e-11 above
/// [`COVERAGE_TARGET`] was the largest excess seen at the ceiling.
///
/// One part in a million, which is four orders clear of both and still six
/// orders inside the defect it exists to catch — a solve that settles for zero
/// margin delivers 3.3e-8, not 9.99e-4.
const BAND_ROUND_TRIP: f64 = 1e-6;

/// **The margin is delivered, not merely aimed at.**
///
/// [`COVERAGE_MARGIN`] promises "0.1% of a 920 km box is 920 m … the price of
/// never being short", and for as long as the solve aimed and settled at the
/// same number that promise was not kept. The east–west lane is *exact* — one
/// logarithm lands the strip on the target to rounding — so a solve aimed at the
/// box plus the margin and settling for the box plus nothing came back covering
/// the box and nothing more. Measured over this sweep with the old bar: the
/// worst delivered margin was **3.28e-8**, 2.09% of framings delivered under a
/// tenth of the margin, and 438 of them delivered under `f32::EPSILON`. The
/// mirror registers through walkers' **f32** `Projector::project`, so those last
/// ones had spent the margin past the precision of the projection it protects.
///
/// The old coverage tests could not see it: they assert `covered >= box`, which
/// zero margin satisfies exactly. This asserts the *band* —
/// [`COVERAGE_MARGIN`] at the floor, [`COVERAGE_TARGET`] at the ceiling — which
/// is a claim with a number in it that a solve can come in under.
///
/// The sweep is the shipped one at fixture scale: ten strip sizes on each axis,
/// every whole degree of latitude both sides of the equator, and eight boxes
/// including the 470 km cap a maximal drag commits and the two rectangles that
/// straddle it. A solve that ran out of zoom is excluded — a strip physically
/// too small to show the box has no margin to deliver, and
/// [`a_box_wider_than_the_strip_can_show_is_framed_as_wide_as_walkers_allows`]
/// is what covers that exit.
///
/// [`a_box_wider_than_the_strip_can_show_is_framed_as_wide_as_walkers_allows`]:
///     a_box_wider_than_the_strip_can_show_is_framed_as_wide_as_walkers_allows
#[test]
fn the_framing_delivers_the_margin_it_promises() {
    let mut framings = 0usize;
    let mut worst = f64::INFINITY;
    let mut worst_at = String::new();
    let mut widest = f64::NEG_INFINITY;
    let mut widest_at = String::new();

    for w in FRAMING_SWEEP_SIDES {
        for h in FRAMING_SWEEP_SIDES {
            let rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(w, h));
            // Half a degree, not whole ones: the framing that spent the whole
            // margin sits at 64.5°N, and a sweep on integers walks past it.
            for half_degrees in -168..=168 {
                let lat = f64::from(half_degrees) * 0.5;
                let centre = walkers::lat_lon(lat, -97.28);
                for box_half in FRAMING_SWEEP_BOXES {
                    let (memory, passes) =
                        solve_viewport(rect, centre, box_half).expect("framable");
                    // Out of zoom is a strip that cannot show the box at all.
                    // The budget's last pass is the one exit where a settle and
                    // an exhaustion look alike from outside, and the band below
                    // is the settle condition restated — so asserting it there
                    // would be asserting that nothing ever exhausts, which
                    // `a_solve_that_runs_out_of_passes_is_short_only_beside_the_pole`
                    // measures instead.
                    if memory.zoom() <= MIN_ZOOM_LEVEL || passes == MAX_FRAMING_PASSES {
                        continue;
                    }
                    framings += 1;
                    let covered = ground_half_extent(rect, &memory, centre).expect("measurable");
                    let delivered = (covered.east_km / box_half.east_km)
                        .min(covered.north_km / box_half.north_km)
                        - 1.0;
                    if delivered < worst {
                        worst = delivered;
                        worst_at = format!("{w}x{h} at {lat}N framed on {box_half:?}");
                    }
                    if delivered > widest {
                        widest = delivered;
                        widest_at = format!("{w}x{h} at {lat}N framed on {box_half:?}");
                    }
                }
            }
        }
    }

    assert!(
        framings > 100_000,
        "the sweep settled only {framings} framings, which is too few to be \
         the measurement this doc quotes",
    );
    assert!(
        worst >= COVERAGE_MARGIN * (1.0 - BAND_ROUND_TRIP),
        "the worst framing delivered a margin of {worst:e} against the {:e} \
         `COVERAGE_MARGIN` promises, at {worst_at} - short of the promise is \
         the volume standing on transparency, and under f32::EPSILON ({:e}) it \
         is short of the precision the mirror is drawn in",
        COVERAGE_MARGIN,
        f32::EPSILON,
    );
    assert!(
        widest <= COVERAGE_TARGET * (1.0 + BAND_ROUND_TRIP),
        "a framing was left {widest:e} wider than its box against the {:e} \
         ceiling `COVERAGE_TARGET` sets, at {widest_at} - past the band is \
         mirror resolution spent on ground the box clips away",
        COVERAGE_TARGET,
    );
}

/// The strip sizes and boxes the framing properties are swept over.
///
/// Awkward on purpose. The shapes that stress the solve hardest are the
/// lopsided ones — a 536 × 8 strip at 58°N, a 1256 × 56 at 65°N — so a list of
/// round sizes measures the solve as easier than it is.
const FRAMING_SWEEP_SIDES: [f32; 10] = [
    8.0, 24.0, 56.0, 96.0, 200.0, 392.0, 536.0, 680.0, 1016.0, 1256.0,
];

/// The boxes [`FRAMING_SWEEP_SIDES`] is swept against.
///
/// The whole ring, the **470 km cap a maximal drag commits**, 300 km, the 10 km
/// floor, the 664 × 10 km rectangle the corner bound allows and its transpose,
/// an ordinary 120 × 75, and the 235 × 470 rectangle that binds on the axis the
/// square one does not — with its transpose, which is the shape that spent the
/// margin hardest: a 96 × 56 strip at 64.5°N framed on 470 × 235 is the framing
/// that delivered **3.28e-8** while the solve aimed and settled at the same
/// number.
const FRAMING_SWEEP_BOXES: [rustdar_radar::voxel::HalfExtentKm; 9] = [
    rustdar_radar::voxel::HalfExtentKm {
        east_km: 460.125,
        north_km: 460.125,
    },
    rustdar_radar::voxel::HalfExtentKm {
        east_km: 470.0,
        north_km: 470.0,
    },
    rustdar_radar::voxel::HalfExtentKm {
        east_km: 300.0,
        north_km: 300.0,
    },
    rustdar_radar::voxel::HalfExtentKm {
        east_km: 10.0,
        north_km: 10.0,
    },
    rustdar_radar::voxel::HalfExtentKm {
        east_km: 664.0,
        north_km: 10.0,
    },
    rustdar_radar::voxel::HalfExtentKm {
        east_km: 10.0,
        north_km: 664.0,
    },
    rustdar_radar::voxel::HalfExtentKm {
        east_km: 120.0,
        north_km: 75.0,
    },
    rustdar_radar::voxel::HalfExtentKm {
        east_km: 235.0,
        north_km: 470.0,
    },
    rustdar_radar::voxel::HalfExtentKm {
        east_km: 470.0,
        north_km: 235.0,
    },
];

/// The framing is **tight**, not merely sufficient.
///
/// The mirror is a fixed number of pixels, so every kilometre of ground outside
/// the box is floor resolution spent on ground `floor_hit` will clip away. A
/// solve that zoomed out "to be safe" would pass the coverage test above and
/// cost the floor detail on every 3D pane in the application.
///
/// The binding axis is asserted to land inside [`COVERAGE_TARGET`], the ceiling
/// of the band the solve settles in — the margin is what the solve converges
/// *through*, so landing a fraction of it wide is the intended outcome rather
/// than slack. The other axis is free, because a square box in a 16:9 strip must
/// overhang east–west and there is no framing that avoids it.
///
/// 0.2% of the 470 km cap a maximal drag commits is 940 m of mirror outside the
/// box on each side, which is where the ceiling was set and why.
#[test]
fn the_framing_spends_no_more_of_the_mirror_than_the_box_needs() {
    let rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(700.0, 450.0));
    let centre = walkers::lat_lon(35.33, -97.28);
    for box_half in [half(460.125, 460.125), half(120.0, 75.0), half(10.0, 10.0)] {
        let memory = viewport_for_region(rect, centre, box_half).expect("framable");
        let covered = ground_half_extent(rect, &memory, centre).expect("measurable");
        let slack = (covered.east_km / box_half.east_km).min(covered.north_km / box_half.north_km);
        assert!(
            slack <= 1.0 + COVERAGE_TARGET * (1.0 + BAND_ROUND_TRIP),
            "framing {box_half:?} left the binding axis {slack:.6}x wider than \
             the box; every bit past the margin is mirror resolution the box \
             clips away",
        );
    }
}

/// **The early-out fires.** [`MAX_FRAMING_PASSES`] is a budget the solve stops
/// short of, and this counts the passes it really ran.
///
/// Two settle tests ago the loop compared a shortfall measured against the box
/// *plus* [`COVERAGE_MARGIN`] to `1.0` — a value the margin's own doc says the
/// solve approaches from above and never reaches. This doc used to conclude from
/// that the condition was unsatisfiable and every framing ran the whole budget.
/// **It is not what happened**, and the difference is the point of the test.
/// The margin's "never reaches" is true of the real numbers and false of `f64`:
/// restoring that loop whole and running it over the sweep
/// [`the_framing_budget_covers_every_latitude_a_region_can_sit_at`] performs
/// today, it **settled on 6,347 of 76,500 framings, 8.30%**, at passes two,
/// three and four. [`MAX_FRAMING_PASSES`] carries the full reading, and says
/// which of the several "old bars" this one is.
///
/// So the constant did not describe a ceiling nothing could come in under. It
/// described one that a twelfth of framings came in under and the rest did not,
/// for a reason no reading of either the loop or the prose would reveal — which
/// is why the count has to come out of the shipped loop rather than out of an
/// argument about it.
///
/// The predecessor of this test could not have caught it, and the shape of the
/// mistake is worth keeping in view: it re-ran the solve's arithmetic in the
/// test body and counted *that* loop's passes. Its copy used the settle
/// condition the prose describes and the real loop used a different one, so both
/// were green while disagreeing. So this drives [`super::solve_viewport`] — the
/// shipped loop — and reads the count out of it.
///
/// The pass counts below are measured, and the fixtures are chosen to show what
/// decides them. It is not latitude alone: which lane *binds* is the box's shape
/// against the strip's, and the east lane is exact in one step where the north
/// lane has to iterate. So a whole-ring box in a **tall** strip settles in two at
/// 64.8°N — that is the shape the old constant's "two passes to spare" was
/// measured on — and the same box in a **wide** strip at the same latitude takes
/// four.
#[test]
fn the_solve_stops_as_soon_as_the_strip_covers_the_box() {
    let ring = half(460.125, 460.125);
    for (w, h, lat, box_half, expect) in [
        // Tall strip, so the exact east lane binds however far north it is.
        (450.0, 900.0, 64.8, ring, 2),
        // Wide strip: the north lane binds and the count follows the latitude.
        (700.0, 450.0, 0.0, ring, 2),
        (700.0, 450.0, 35.33, ring, 3),
        (700.0, 450.0, 64.8, ring, 4),
        // A tight pick is a smaller ask on the same lane, not an easier one.
        (700.0, 450.0, 35.33, half(10.0, 10.0), 2),
    ] {
        let rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(w, h));
        let centre = walkers::lat_lon(lat, -97.28);
        let (memory, passes) =
            solve_viewport(rect, centre, box_half).expect("a finite box in a pane with area");
        assert_eq!(
            passes, expect,
            "a {w}x{h} strip at {lat}N framed on {box_half:?} took {passes}              passes, not the {expect} this doc records",
        );
        assert!(
            passes < MAX_FRAMING_PASSES,
            "the solve used its whole {MAX_FRAMING_PASSES}-pass budget on a              {w}x{h} strip at {lat}N, so the early-out did not fire",
        );

        // What the early-out fired *on*: the strip covers the box plus the
        // margin the constant promises, and is not past the ceiling the solve
        // aims at. Both directions matter — short is transparent floor, wide is
        // mirror resolution spent outside the box.
        let covered = ground_half_extent(rect, &memory, centre).expect("measurable");
        let delivered =
            (covered.east_km / box_half.east_km).min(covered.north_km / box_half.north_km) - 1.0;
        assert!(
            delivered >= COVERAGE_MARGIN * (1.0 - BAND_ROUND_TRIP),
            "the early-out fired on a strip delivering {delivered:e} over the \
             box, against the {COVERAGE_MARGIN:e} `COVERAGE_MARGIN` promises - \
             a bar the strip merely *covers* is one an exact lane clears with \
             nothing left over, which is what it used to be",
        );
        assert!(
            delivered <= COVERAGE_TARGET * (1.0 + BAND_ROUND_TRIP),
            "the early-out left a strip {delivered:e} wider than the box, past \
             the {COVERAGE_TARGET:e} ceiling the solve aims at",
        );
    }
}

/// The pass table on [`MAX_FRAMING_PASSES`], re-measured — every row of it,
/// including the last one, which is the budget entire.
///
/// The budget is a claim about every strip, latitude and box this application
/// can produce, and the last one to be measured on a single fixture was measured
/// on the easy one. So this sweeps [`FRAMING_SWEEP_SIDES`] on each axis and
/// [`FRAMING_SWEEP_BOXES`] at every whole degree from the equator to 84°.
///
/// Two things are held. Every solve settles inside its band's row, and each row
/// is *reached* — a table whose numbers are all overstatements is a table nobody
/// can use to choose the budget, and until the 470 km cap and the 80°+ rows
/// joined it the table peaked at seven, so a [`MAX_FRAMING_PASSES`] of 7 would
/// have passed every test there was.
///
/// A solve that runs out of zoom rather than out of passes is excluded: the
/// clamp at [`MIN_ZOOM_LEVEL`] is the strip being physically unable to show the
/// box, which
/// [`a_box_wider_than_the_strip_can_show_is_framed_as_wide_as_walkers_allows`]
/// covers and this is not about.
///
/// [`a_box_wider_than_the_strip_can_show_is_framed_as_wide_as_walkers_allows`]:
///     a_box_wider_than_the_strip_can_show_is_framed_as_wide_as_walkers_allows
#[test]
fn the_framing_budget_covers_every_latitude_a_region_can_sit_at() {
    let mut reached = std::collections::BTreeMap::<usize, usize>::new();
    for w in FRAMING_SWEEP_SIDES {
        for h in FRAMING_SWEEP_SIDES {
            let rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(w, h));
            for degrees in 0..=84 {
                let lat = f64::from(degrees);
                let centre = walkers::lat_lon(lat, -97.28);
                for box_half in FRAMING_SWEEP_BOXES {
                    let (memory, passes) =
                        solve_viewport(rect, centre, box_half).expect("framable");
                    // Out of zoom is a different exit and has its own test.
                    if memory.zoom() <= MIN_ZOOM_LEVEL {
                        continue;
                    }
                    let budget = documented_passes(lat);
                    assert!(
                        passes <= budget,
                        "a {w}x{h} strip at {lat}N framed on {box_half:?} took \
                         {passes} passes against the {budget} its band records",
                    );
                    *reached.entry(budget).or_default() =
                        (*reached.entry(budget).or_default()).max(passes);
                }
            }
        }
    }

    for (budget, worst) in &reached {
        assert_eq!(
            budget, worst,
            "no shape in the band that documents {budget} passes needed more \
             than {worst}, so the table overstates what the budget is for",
        );
    }
    assert_eq!(
        reached.get(&MAX_FRAMING_PASSES),
        Some(&MAX_FRAMING_PASSES),
        "no shape in this sweep needed the whole {MAX_FRAMING_PASSES}-pass \
         budget, so the constant is an assertion about nothing and a smaller \
         one would pass every test here",
    );
}

/// The row of [`MAX_FRAMING_PASSES`]' table that `lat` falls in.
///
/// Free rather than nested so that
/// [`a_solve_that_runs_out_of_passes_is_short_only_beside_the_pole`] reads the
/// same table this one does.
fn documented_passes(lat: f64) -> usize {
    match lat.abs() as i64 {
        0..=29 => 3,
        30..=61 => 4,
        62..=72 => 5,
        73..=76 => 6,
        77..=79 => 7,
        _ => 8,
    }
}

/// **The budget's last pass is one a maximal drag really needs**, and the box
/// that needs it is the exact one the selector commits at its cap.
///
/// [`MAX_FRAMING_PASSES`] is 8 and the sweep above only ever showed 7 until the
/// 470 km square joined it, which is a constant asserting nothing: seven would
/// have been green everywhere. This drives the cap in a square strip at the
/// latitudes where the north lane's convergence has slowed to a crawl, and holds
/// **both** ends of the claim at once.
///
/// A budget of 7 fails the second assertion rather than the first: the solve
/// would still answer `(memory, 7)`, because running out of passes and settling
/// on the last one are the same count — but it would answer with the strip it
/// had not finished converging, short of the margin. A budget of 9 fails the
/// first. So the pair pins 8 from above and below, which one of them alone
/// cannot do.
#[test]
fn the_budgets_last_pass_is_one_a_maximal_drag_really_needs() {
    let cap = half(
        rustdar_radar::voxel::MAX_HALF_WIDTH_KM,
        rustdar_radar::voxel::MAX_HALF_WIDTH_KM,
    );
    let rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(96.0, 96.0));
    for lat in [82.5, 83.0, 83.5, 83.8] {
        let centre = walkers::lat_lon(lat, -97.28);
        let (memory, passes) = solve_viewport(rect, centre, cap).expect("framable");
        assert_eq!(
            passes,
            MAX_FRAMING_PASSES,
            "a 96x96 strip at {lat}N framed on the {} km cap took {passes} \
             passes, so this fixture no longer drives the budget to its end and \
             the constant is unheld again",
            rustdar_radar::voxel::MAX_HALF_WIDTH_KM,
        );
        assert!(
            memory.zoom() > MIN_ZOOM_LEVEL,
            "the fixture ran out of zoom rather than out of passes, which is a \
             different exit and a different test",
        );
        let covered = ground_half_extent(rect, &memory, centre).expect("measurable");
        let delivered = (covered.east_km / cap.east_km).min(covered.north_km / cap.north_km) - 1.0;
        assert!(
            delivered >= COVERAGE_MARGIN * (1.0 - BAND_ROUND_TRIP),
            "the last pass of the budget left {delivered:e} of margin at {lat}N \
             against the {COVERAGE_MARGIN:e} promised, so the budget is one \
             pass short of what this box needs",
        );
    }
}

/// **What running out of passes really answers.** It is not "biased outward",
/// and the comment that said so is the reason this is measured.
///
/// The claim was that the loop's last act is a step, so an exhausted answer is
/// one whole measured shortfall wider than the last thing measured short. The
/// step is real; the conclusion does not follow, because the north lane's ground
/// is **sub**linear in the zoom it is bought with — a step sized by the linear
/// model gains less than it asks for, and near the pole it gains almost nothing.
/// Measured before [`ground_half_extent`]'s longitude clamp landed, the worst
/// exhausted answer covered **13.9%** of its box: a 460 km square at 83.5°N in a
/// 536 × 96 strip, where the east lane was folding back on itself and shrinking
/// as the strip widened, so the solve had nothing to converge to. That was the
/// fold, and it is fixed.
///
/// What is left is the pole, and it is bounded on both sides:
///
/// * Exhaustion needs a centre at **81.5°** or higher *and* a box whose poleward
///   edge reaches within **2.53°** of the pole — 87.47°N at the nearest measured.
///   Mercator's `y` runs to infinity there, so the ground the last pass is
///   buying costs an unbounded number of zoom levels.
/// * No box a **drag** can commit is left short at all. The selector caps at
///   [`MAX_HALF_WIDTH_KM`](rustdar_radar::voxel::MAX_HALF_WIDTH_KM), and every
///   exhausted framing of a box inside that cap still covered its box — worst
///   measured 1.00068, which is margin rather than shortfall.
/// * Past the cap — the rectangles the corner bound allows, up to 664 km on an
///   axis — an exhausted answer covers at worst **95.9%** of its box.
///
/// That last one is a real 4% of transparent floor and it is answered rather
/// than refused, for the same reason the clamp exit is: the alternative is the
/// caller's own map memory, centred on the site rather than on the box, and
/// `the_floor_is_framed_on_a_dragged_region_rather_than_on_the_site` is what
/// that costs.
#[test]
fn a_solve_that_runs_out_of_passes_is_short_only_beside_the_pole() {
    let mut exhausted = 0usize;
    let mut lowest_centre = f64::INFINITY;
    let mut nearest_edge_to_pole = f64::INFINITY;
    let mut worst_covered = f64::INFINITY;
    let mut worst_covered_at = String::new();
    let mut worst_within_cap = f64::INFINITY;
    let mut worst_within_cap_at = String::new();

    for w in FRAMING_SWEEP_SIDES {
        for h in FRAMING_SWEEP_SIDES {
            let rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(w, h));
            for degrees in -84..=84 {
                let lat = f64::from(degrees);
                let centre = walkers::lat_lon(lat, -97.28);
                for box_half in FRAMING_SWEEP_BOXES {
                    let (memory, passes) =
                        solve_viewport(rect, centre, box_half).expect("framable");
                    if passes != MAX_FRAMING_PASSES || memory.zoom() <= MIN_ZOOM_LEVEL {
                        continue;
                    }
                    let covered = ground_half_extent(rect, &memory, centre).expect("measurable");
                    let delivered = (covered.east_km / box_half.east_km)
                        .min(covered.north_km / box_half.north_km);
                    // Settling on the budget's last pass is not exhausting it.
                    if delivered >= 1.0 + COVERAGE_MARGIN * (1.0 - BAND_ROUND_TRIP) {
                        continue;
                    }
                    exhausted += 1;
                    lowest_centre = lowest_centre.min(lat.abs());
                    // Where the box's poleward edge sits. A bound rather than a
                    // position, so the codebase's one degrees-to-kilometres
                    // figure is exactly the right instrument for it.
                    let edge_deg = lat.abs() + box_half.north_km / rustdar_geo::KM_PER_DEGREE_LAT;
                    nearest_edge_to_pole = nearest_edge_to_pole.min(90.0 - edge_deg);
                    let what = format!("a {w}x{h} strip at {lat}N framed on {box_half:?}");
                    if delivered < worst_covered {
                        worst_covered = delivered;
                        worst_covered_at = what.clone();
                    }
                    let within_cap = box_half.east_km <= rustdar_radar::voxel::MAX_HALF_WIDTH_KM
                        && box_half.north_km <= rustdar_radar::voxel::MAX_HALF_WIDTH_KM;
                    if within_cap && delivered < worst_within_cap {
                        worst_within_cap = delivered;
                        worst_within_cap_at = what;
                    }
                }
            }
        }
    }

    assert!(
        exhausted > 0,
        "nothing in this sweep ran out of passes, so the exit this measures no \
         longer describes anything and the prose on `solve_viewport` is stale",
    );
    assert!(
        lowest_centre >= 80.0,
        "a solve ran out of passes at {lowest_centre}N, inside the latitudes \
         `MAX_FRAMING_PASSES` says its table covers",
    );
    assert!(
        nearest_edge_to_pole <= 3.0,
        "a solve ran out of passes on a box whose poleward edge was still \
         {nearest_edge_to_pole} degrees from the pole, so the reason this exit \
         is documented with - Mercator's y running away at the pole - is not \
         the reason it is being taken",
    );
    assert!(
        worst_covered >= 0.95,
        "an exhausted framing covered {worst_covered} of its box, at \
         {worst_covered_at} - the doc bounds this at 95.9%",
    );
    assert!(
        worst_within_cap >= 1.0,
        "an exhausted framing of a box inside the selector's \
         {} km cap covered only {worst_within_cap} of it, at \
         {worst_within_cap_at} - a box a user can actually drag is left \
         standing on transparency",
        rustdar_radar::voxel::MAX_HALF_WIDTH_KM,
    );
}

/// **A strip wider than the world measures the world, not the fold.**
///
/// walkers' `unproject` is the plain inverse Mercator and does not wrap, so a
/// point a whole world left of the centre reads back as `centre − 360°`
/// literally — and the geodesy between two longitudes exactly 360° apart is the
/// distance from a point to itself. Measured before the clamp landed, at
/// [`MIN_ZOOM_LEVEL`] about 35.33°N:
///
/// | strip width | worlds across | east half-extent |
/// |---|---|---|
/// | 128 pt | 0.50 | 7 835 km |
/// | 256 pt | 1.00 | 12 158 km |
/// | 300 pt | 1.17 | 11 529 km |
/// | 512 pt | 2.00 | **2.47e-12 km** |
/// | 513 pt | 2.00 | 63.8 km |
/// | 1400 pt | 5.47 | 8 271 km |
///
/// Two failures in one. The near-zero is **one guard from `None`**, and `None`
/// here is [`viewport_for_region`] refusing, which drops the caller onto the
/// pane's own map memory centred on the *site* — the fallback the stored region
/// exists to keep away from and the one that put a 470 km box on a fraction of
/// a kilometre of floor. And the non-monotonicity broke the framing solve
/// outright: [`super::solve_viewport`] steps by a logarithm of the shortfall on
/// the assumption that a wider strip shows more ground, and out there a wider
/// strip showed less — so a solve whose north lane had already converged could
/// never settle. That is what left the worst exhausted framing covering 13.9%
/// of its box, and
/// [`a_solve_that_runs_out_of_passes_is_short_only_beside_the_pole`] is what
/// bounds the exit now that this is fixed.
///
/// So this asserts the property the solve needs rather than the six numbers
/// above: **monotone in width, and saturating at the half-circumference.** Half
/// a turn is the whole of the axis, and a strip showing two worlds is showing
/// the same ground as a strip showing one.
///
/// The north lane is checked alongside to show it needs no such clamp: latitude
/// comes back through `atan(sinh(y))`, which is bounded into ±90° for every
/// pixel there is, so a latitude offset cannot reach half a turn to begin with.
#[test]
fn a_strip_wider_than_the_world_measures_the_world_rather_than_the_fold() {
    let centre = walkers::lat_lon(35.33, -97.28);
    // The whole world is one 256-point tile at `MIN_ZOOM_LEVEL`.
    let mut world = walkers::MapMemory::default();
    world
        .set_zoom(MIN_ZOOM_LEVEL)
        .expect("walkers accepts its own wide end");

    // Half the world either side of the centre: the most ground an east-west
    // axis has, and what every wider strip has to answer.
    let (_, half_circumference_km) =
        rustdar_geo::site_bearing_range_km(centre.y(), centre.x(), centre.y(), centre.x() + 180.0);

    let mut previous = 0.0_f64;
    let mut previous_width = 0.0_f32;
    for width in [
        64.0_f32, 128.0, 200.0, 250.0, 256.0, 257.0, 300.0, 400.0, 511.0, 512.0, 513.0, 700.0,
        1024.0, 1400.0, 4096.0,
    ] {
        let rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(width, 400.0));
        let measured = ground_half_extent(rect, &world, centre)
            .expect("a strip with area over a world with area is measurable");
        assert!(
            measured.east_km >= previous,
            "a {width}-point strip measured {} km of east-west ground where a \
             {previous_width}-point one measured {previous} km - the solve \
             steps by a logarithm on the assumption that a wider strip shows \
             more ground, and a fold makes that assumption false",
            measured.east_km,
        );
        assert!(
            measured.east_km <= half_circumference_km * (1.0 + BAND_ROUND_TRIP),
            "a {width}-point strip measured {} km either side of the centre, \
             past the {half_circumference_km} km there is",
            measured.east_km,
        );
        if f64::from(width) >= 512.0 {
            assert!(
                (measured.east_km - half_circumference_km).abs()
                    <= half_circumference_km * BAND_ROUND_TRIP,
                "a {width}-point strip shows every meridian there is and \
                 measured {} km against the {half_circumference_km} km that is",
                measured.east_km,
            );
        }
        // The north lane is bounded by the projection itself and needs nothing.
        assert!(
            measured.north_km > 0.0 && measured.north_km <= half_circumference_km,
            "the north lane measured {} km, which is not ground",
            measured.north_km,
        );
        previous = measured.east_km;
        previous_width = width;
    }
}

/// A box or a pane that cannot be framed is refused, so the caller falls back to
/// the pane's own map memory rather than to a viewport built from a `NaN`.
///
/// The extent is checked for being **positive** as well as finite: a zero-width
/// box divides to an infinity in the solve, and an infinite zoom step reaches
/// `set_zoom` as a `NaN` that walkers accepts as a number.
#[test]
fn an_unframable_box_or_pane_is_refused() {
    let centre = walkers::lat_lon(35.33, -97.28);
    let rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(700.0, 450.0));
    let flat = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(700.0, 0.0));

    assert!(
        viewport_for_region(flat, centre, half(120.0, 75.0)).is_none(),
        "a pane collapsed to nothing by a divider drag has no viewport to frame",
    );
    for bad in [
        half(0.0, 75.0),
        half(120.0, 0.0),
        half(-120.0, 75.0),
        half(f64::NAN, 75.0),
        half(120.0, f64::INFINITY),
    ] {
        assert!(
            viewport_for_region(rect, centre, bad).is_none(),
            "{bad:?} was framed rather than refused",
        );
    }
}

/// Where `walkers::MapMemory::set_zoom` answers `Err(InvalidZoom)`, pinned so
/// that [`MIN_ZOOM_LEVEL`] and [`MAX_ZOOM_LEVEL`] cannot drift from the crate
/// they restate.
///
/// walkers keeps the bounds inside a `pub(crate)` type — `Zoom::try_from` is
/// `if !(0. ..=26.).contains(&value) { Err(InvalidZoom) }` at
/// `walkers-0.56.0/src/zoom.rs:14` — so a consumer has no constant to import and
/// no way to read them but to try. A version bump that moved either end would
/// leave this module clamping to the wrong place, and the symptom would be the
/// silent refusal the clamp exists to remove, back again with a clamp in front
/// of it.
///
/// The `NaN` row is the one that is easy to get wrong: `contains` is false for
/// a `NaN`, so walkers refuses one — and `f64::clamp` **propagates** a `NaN`
/// rather than bounding it, so clamping is not a defence against one. That is
/// why the solve's finiteness test runs before the clamp.
#[test]
fn a_zoom_walkers_refuses_is_one_this_module_clamps_away() {
    let accepts = |zoom: f64| walkers::MapMemory::default().set_zoom(zoom).is_ok();

    assert!(accepts(MIN_ZOOM_LEVEL), "walkers moved the wide end");
    assert!(accepts(MAX_ZOOM_LEVEL), "walkers moved the tight end");
    for outside in [
        MIN_ZOOM_LEVEL - 1e-9,
        MAX_ZOOM_LEVEL + 1e-9,
        -1.0,
        27.0,
        f64::NEG_INFINITY,
        f64::INFINITY,
        f64::NAN,
    ] {
        assert!(
            !accepts(outside),
            "walkers accepted {outside}, so this module's range is wrong",
        );
    }
    // And the clamp's own contract: every finite zoom the solve can compute
    // lands somewhere walkers takes.
    for asked in [-3.5_f64, -0.0001, 0.0, 13.0, 26.0, 99.0, f64::MAX, f64::MIN] {
        let clamped = asked.clamp(MIN_ZOOM_LEVEL, MAX_ZOOM_LEVEL);
        assert!(
            accepts(clamped),
            "a step of {asked} clamped to {clamped}, which walkers still refuses",
        );
    }
}

/// **The refusal that used to be silent.** A strip too small to reach the box
/// inside walkers' zoom range is framed as wide as walkers allows, centred on
/// the box, rather than abandoned to the caller's fallback.
///
/// The solve starts at `MapMemory::default()`'s zoom 16 and buys ground by
/// zooming out, so it has 16 levels — a factor of 65 536 — before walkers
/// refuses the step. A pane whose shorter side is a handful of points needs more
/// than that to reach a continental box, and that gap is what the stored region
/// made reachable: the box used to *be* the viewport, so the two could not
/// disagree by a factor of 65 536; a picked region is a fact about the ground
/// that outlives any pane size, including a pane a divider drag or a canvas
/// resize has left a few points across.
///
/// **The trip is asserted as well as the answer.** A fixture that quietly
/// stopped driving `set_zoom` out of range would leave this passing while
/// testing nothing, so the first step is computed here and checked to be a zoom
/// walkers really does refuse.
#[test]
fn a_box_wider_than_the_strip_can_show_is_framed_as_wide_as_walkers_allows() {
    let rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(8.0, 6.0));
    let centre = walkers::lat_lon(35.33, -97.28);
    let box_half = half(470.0, 470.0);

    // The trigger, computed the way the solve computes it.
    let opening = walkers::MapMemory::default();
    let covered = ground_half_extent(rect, &opening, centre).expect("measurable");
    let shortfall = (box_half.east_km * (1.0 + COVERAGE_TARGET) / covered.east_km)
        .max(box_half.north_km * (1.0 + COVERAGE_TARGET) / covered.north_km);
    let step = opening.zoom() - shortfall.log2();
    assert!(
        step < MIN_ZOOM_LEVEL,
        "this fixture no longer drives the solve past walkers' wide end - it \
         asks for zoom {step}, which walkers accepts, so nothing here is tested",
    );
    assert!(
        walkers::MapMemory::default().set_zoom(step).is_err(),
        "walkers accepted zoom {step}",
    );

    let framed = viewport_for_region(rect, centre, box_half)
        .expect("a box too wide for the strip is framed as wide as it can be");
    assert_eq!(
        framed.zoom(),
        MIN_ZOOM_LEVEL,
        "the framing stopped short of the widest zoom walkers allows",
    );

    // And it is the answer that is worth having: centred on the box, showing
    // most of it, against the fallback's fraction of one kilometre of it.
    let projector = walkers::Projector::new(rect, &framed, centre);
    let middle = projector.unproject(rect.center().to_vec2());
    let (_, off_km) =
        rustdar_geo::site_bearing_range_km(centre.y(), centre.x(), middle.y(), middle.x());
    assert!(
        off_km < 0.001,
        "the clamped strip's middle is {off_km} km from the box's centre",
    );
    let clamped = ground_half_extent(rect, &framed, centre).expect("measurable");
    assert!(
        clamped.north_km > 300.0 && clamped.east_km > 400.0,
        "the clamped strip covers only {clamped:?} of a 470 km box",
    );
    assert!(
        clamped.north_km > 1000.0 * covered.north_km,
        "the clamped strip covers {clamped:?} against the opening zoom's \
         {covered:?} - the fallback this replaces is that second one",
    );
}

/// A framed strip is centred on the box, not on wherever the pane's map was
/// left.
///
/// The centre is the other half of "the two clips are the same rectangle": a
/// strip zoomed out far enough to cover a 920 km box but centred 400 km away
/// still leaves a side of the box transparent. It is asserted through the
/// projector rather than off `MapMemory`, because what the shader ends up
/// sampling is the projection.
#[test]
fn a_framed_strip_is_centred_on_the_box() {
    let rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(700.0, 450.0));
    let centre = walkers::lat_lon(35.33, -97.28);
    let memory = viewport_for_region(rect, centre, half(120.0, 75.0)).expect("framable");
    let projector = walkers::Projector::new(rect, &memory, centre);
    let middle = projector.unproject(rect.center().to_vec2());
    let (_, off_km) =
        rustdar_geo::site_bearing_range_km(centre.y(), centre.x(), middle.y(), middle.x());
    assert!(
        off_km < 0.001,
        "the strip's middle is {off_km} km from the box's centre",
    );
    assert!(
        memory.detached().is_none(),
        "a detached memory would centre the strip on the pane's last pan \
         instead of on the box",
    );
}

/// A point on Earth near KTLX, for the drag fixtures.
fn ktlx() -> rustdar_geo::GeoPoint {
    rustdar_geo::GeoPoint {
        lat: 35.33,
        lon: -97.28,
    }
}

/// Kilometres a fixture's stated offset may be out by, at the ~100 km distances
/// used below.
///
/// The offsets are built with the **flat** approximation
/// ([`OFFSET_TOLERANCE_KM`]'s companion, `corners_for`'s own arithmetic) while
/// the drag measures with [`rustdar_geo::site_bearing_range_km`], the
/// real geodesy. The two disagree by a fraction of a percent, so an exact
/// assertion would be an assertion about which approximation the fixture used
/// rather than about the drag. 1.5 km on a 100 km offset is 1.5% — an order of
/// magnitude looser than the disagreement and two orders tighter than every
/// error these tests exist to catch, all of which are 30% or a factor of two.
const OFFSET_TOLERANCE_KM: f64 = 1.5;

/// A point `east_km` east and `north_km` north of `from`.
///
/// The flat approximation, deliberately independent of the geodesy the drag
/// measures with: the exact inverse exists
/// (`rustdar_geo::great_circle_destination`), but a fixture built on it would
/// share its arithmetic with the code under test. See [`OFFSET_TOLERANCE_KM`]
/// for what the independence costs and why it costs nothing that matters.
fn offset(from: rustdar_geo::GeoPoint, east_km: f64, north_km: f64) -> rustdar_geo::GeoPoint {
    let per_deg = rustdar_geo::KM_PER_DEGREE_LAT;
    rustdar_geo::GeoPoint {
        lat: from.lat + north_km / per_deg,
        lon: from.lon + east_km / (per_deg * from.lat.to_radians().cos()),
    }
}

/// **The half-width is Chebyshev, not Euclidean**: the square's *edge* follows
/// the pointer, so a straight drag grows the box at the rate the pointer moves.
///
/// The alternative reads as the box tracking something behind the cursor — a
/// pointer 100 km east would give a 70.7 km half-width, and the edge the user is
/// watching would lag their finger by 30%. Both axes are swept, and a diagonal,
/// because a max() written as a min() passes any fixture that only pulls one way.
#[test]
fn the_squares_edge_follows_the_pointer_rather_than_its_corner() {
    for (east_km, north_km, want) in [
        (100.0, 0.0, 100.0),
        (0.0, 100.0, 100.0),
        (-100.0, 0.0, 100.0),
        (0.0, -100.0, 100.0),
        (100.0, 40.0, 100.0),
        (40.0, 100.0, 100.0),
        (100.0, 100.0, 100.0),
    ] {
        let mut drag = RegionDrag::begin(0, ktlx()).expect("KTLX is on Earth");
        drag.extend_to(offset(ktlx(), east_km, north_km));
        assert!(
            (drag.half_width_km() - want).abs() < OFFSET_TOLERANCE_KM,
            "a pointer {east_km} km east and {north_km} km north gave a \
             {:.2} km half-width, not the {want} km its furthest axis stands at \
             - the edge is no longer under the pointer",
            drag.half_width_km(),
        );
    }
}

/// A drag below the resampler's own minimum **commits nothing**.
///
/// The bar is [`rustdar_radar::voxel::MIN_HALF_WIDTH_KM`] rather than a pixel
/// count because `build_voxels` *clamps* rather than refuses: a 4 km box would
/// be resampled as a 10 km one, so committing it would put the pane over ground
/// the user did not draw and make its own resolution readout a description of
/// the wrong picture. Refusing means every region that commits is honoured
/// exactly.
#[test]
fn a_drag_under_the_resamplers_minimum_commits_nothing() {
    let min = rustdar_radar::voxel::MIN_HALF_WIDTH_KM;
    for east_km in [0.0, 0.5, min * 0.5, min - 0.5] {
        let mut drag = RegionDrag::begin(0, ktlx()).expect("on Earth");
        drag.extend_to(offset(ktlx(), east_km, 0.0));
        assert_eq!(
            drag.commit(),
            None,
            "a {:.1} km box committed, and `build_voxels` would have widened it \
             to {:.0} km behind the user's back",
            2.0 * east_km,
            2.0 * min,
        );
    }

    let mut drag = RegionDrag::begin(0, ktlx()).expect("on Earth");
    // Clear of the bar by more than `OFFSET_TOLERANCE_KM`, so this precondition
    // is about the bar rather than about which approximation built the fixture.
    drag.extend_to(offset(ktlx(), min + 2.0 * OFFSET_TOLERANCE_KM, 0.0));
    assert!(
        drag.commit().is_some(),
        "precondition: a box clear of the minimum must commit, or the sweep \
         above passes because nothing ever commits",
    );
}

/// The preview **stops** at the widest box the resampler will build.
///
/// `VolumeRegion::new` clamps on commit, so an uncapped drag would paint an
/// ever-bigger square past the stop and release the same box every time — what
/// is drawn has to be what is resampled. Asserted as the pair: the drag's own
/// half-width stops, and the region it commits agrees with it.
#[test]
fn a_long_drag_stops_at_the_widest_box_the_resampler_will_build() {
    let max = rustdar_radar::voxel::MAX_HALF_WIDTH_KM;
    let mut drag = RegionDrag::begin(0, ktlx()).expect("on Earth");
    drag.extend_to(offset(ktlx(), 3.0 * max, 0.0));
    assert!(
        (drag.half_width_km() - max).abs() < 1e-9,
        "a drag three times past the stop stands at {:.2} km, not the {max:.2} km \
         ceiling - the preview is drawing a box that cannot be resampled",
        drag.half_width_km(),
    );
    let region = drag.commit().expect("a maximal box is a legal box");
    assert!(
        (region.half_east_km() - max).abs() < 1e-6 && (region.half_north_km() - max).abs() < 1e-6,
        "the committed box is {:.2} x {:.2} km of half-extent where the preview \
         drew {max:.2} - the hint and the picture would disagree",
        region.half_east_km(),
        region.half_north_km(),
    );
}

/// **Every committed region is square**, and that is a decision rather than an
/// accident of the fixture.
///
/// `VolumeRegion` carries an axis each and says at length why. The *drag*
/// produces equal axes because the alternative — keying the box on the pane's
/// aspect — puts the divider back on the list of things that change the region,
/// which is the whole defect the stored region exists to remove. A pull that is
/// long on one axis and short on the other is the case that would expose a
/// per-axis measurement, so that is what is pulled.
#[test]
fn a_committed_region_is_square_whatever_shape_the_pull_was() {
    for (east_km, north_km) in [(120.0, 20.0), (20.0, 120.0), (200.0, 199.0), (60.0, 60.0)] {
        let mut drag = RegionDrag::begin(0, ktlx()).expect("on Earth");
        drag.extend_to(offset(ktlx(), east_km, north_km));
        let region = drag.commit().expect("a box well over the minimum");
        assert_eq!(
            region.half_east_km(),
            region.half_north_km(),
            "a {east_km} x {north_km} km pull committed a rectangle - a region \
             keyed on a shape the user dragged is a region a divider can change",
        );
    }
}

/// A corner the projector could not place leaves the drag **exactly** as it was.
///
/// This runs every frame of a drag, so a single laundered NaN would stick for
/// the rest of it — the box would freeze, or worse commit somewhere nobody
/// pointed. Refused rather than clamped, because `f64::clamp` propagates NaN
/// and there is no nearest sensible patch of ground.
#[test]
fn a_corner_off_earth_leaves_the_drag_where_it_was() {
    let mut drag = RegionDrag::begin(0, ktlx()).expect("on Earth");
    drag.extend_to(offset(ktlx(), 80.0, 0.0));
    let settled = drag;
    for bad in [
        rustdar_geo::GeoPoint {
            lat: f64::NAN,
            lon: -97.28,
        },
        rustdar_geo::GeoPoint {
            lat: 35.33,
            lon: f64::INFINITY,
        },
        rustdar_geo::GeoPoint {
            lat: 95.0,
            lon: -97.28,
        },
    ] {
        drag.extend_to(bad);
        assert_eq!(
            drag, settled,
            "a corner at {bad:?} moved the drag - one bad frame would stick for \
             the rest of the gesture",
        );
    }
}

/// A press the projector could not place starts **no drag at all**.
///
/// Reachable for a pane a divider has collapsed to nothing. `None` leaves the
/// mode armed and costs the user nothing, where a laundered centre would commit
/// a box somewhere they never pointed.
#[test]
fn a_press_off_earth_starts_no_drag() {
    for bad in [
        rustdar_geo::GeoPoint {
            lat: f64::NAN,
            lon: 0.0,
        },
        rustdar_geo::GeoPoint {
            lat: 0.0,
            lon: f64::NAN,
        },
        rustdar_geo::GeoPoint {
            lat: 91.0,
            lon: 0.0,
        },
    ] {
        assert_eq!(
            RegionDrag::begin(0, bad),
            None,
            "a press at {bad:?} began a drag",
        );
    }
}

/// The drawn box is square **in kilometres**, not in degrees.
///
/// A degree-square box at 35°N is 22% wider than it is tall, and it would not be
/// the box that gets resampled — the outline would promise ground the grid never
/// covers. Swept over latitude because the error is `1/cos(lat)` and vanishes at
/// the equator, where a degree-square fixture would pass.
#[test]
fn the_drawn_box_is_square_in_kilometres_rather_than_degrees() {
    for lat in [0.0, 25.0, 35.33, 49.0, 64.8] {
        let centre = rustdar_geo::GeoPoint { lat, lon: -97.28 };
        let half = rustdar_radar::voxel::HalfExtentKm::square(100.0);
        let (nw, se) = corners_for(centre, half).expect("a box away from the poles");
        let (_, north_km) =
            rustdar_geo::site_bearing_range_km(centre.lat, centre.lon, nw.lat, centre.lon);
        let (_, east_km) =
            rustdar_geo::site_bearing_range_km(centre.lat, centre.lon, centre.lat, se.lon);
        // The two are compared against *each other*, not against 100, so the
        // flat approximation cancels: what is asserted is that the box is as
        // wide as it is tall in kilometres, which is the claim that matters.
        assert!(
            (east_km - north_km).abs() < OFFSET_TOLERANCE_KM,
            "at {lat}N the drawn box is {east_km:.1} km east by {north_km:.1} km \
             north - the outline is not the box the grid will cover",
        );
    }
}

/// A polar centre draws nothing rather than an infinity.
///
/// `cos(lat)` is zero at the pole and every longitude is the same place, so the
/// east half-width has no degree measure. No NEXRAD site is within 20° of one;
/// the refusal is here because the alternative reaches a painter.
#[test]
fn a_polar_box_has_no_corners() {
    for lat in [90.0, -90.0] {
        assert_eq!(
            corners_for(
                rustdar_geo::GeoPoint { lat, lon: 0.0 },
                rustdar_radar::voxel::HalfExtentKm::square(100.0),
            ),
            None,
            "a box at {lat}N produced corners",
        );
    }
}
