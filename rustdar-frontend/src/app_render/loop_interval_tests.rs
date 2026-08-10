use super::loop_interval;
use crate::constants::{DEFAULT_LOOP_SPEED_FPS, MAX_LOOP_SPEED_FPS, MIN_LOOP_SPEED_FPS};

/// A stored speed the UI cannot produce must not take the app down.
///
/// Every one of these panics `Duration::from_secs_f32` — zero and the
/// negatives through the reciprocal, the rest directly — and it panics in
/// `advance_loop_playback`, which runs on every frame. There is no getting
/// out of that: the frame that would let the user fix the slider is the
/// frame that dies. The values are all reachable, because the save-side
/// guard checks only `is_finite` and the load assigns whatever it finds.
#[test]
fn a_speed_no_slider_could_have_set_still_yields_a_frame_interval() {
    for fps in [0.0, -1.0, -0.0, f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
        let interval = loop_interval(fps);
        assert!(
            interval.as_secs_f32().is_finite() && !interval.is_zero(),
            "{fps} produced {interval:?}",
        );
    }
}

/// And the speeds the slider *can* set are honoured exactly.
///
/// A clamp that quietly rounded every speed to one value would satisfy the
/// test above and make the setting inert.
#[test]
fn a_speed_the_slider_can_set_is_used_as_it_stands() {
    assert_eq!(loop_interval(5.0).as_secs_f32(), 0.2);
    assert_eq!(
        loop_interval(MIN_LOOP_SPEED_FPS).as_secs_f32(),
        1.0 / MIN_LOOP_SPEED_FPS,
    );
    assert_eq!(
        loop_interval(MAX_LOOP_SPEED_FPS).as_secs_f32(),
        1.0 / MAX_LOOP_SPEED_FPS,
    );
    assert_eq!(
        loop_interval(f32::NAN),
        loop_interval(DEFAULT_LOOP_SPEED_FPS),
        "a value that is not a number falls back to the UI's own default",
    );
}
