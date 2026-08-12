//! The zoom gesture moves the eye, and it moves nothing else.
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
//! What is left to pin is the arithmetic between a wheel notch and a standoff,
//! and the refusals that keep a bad frame's number out of the camera. The
//! *gate* — which pane a gesture belongs to — is pinned through the real UI by
//! `ui_map::volume_arm_tests`, because it is a question about layers and hover
//! that no unit fixture can ask honestly.

use super::{dolly_for_step, zoom_step};

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
