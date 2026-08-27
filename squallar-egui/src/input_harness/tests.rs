use super::*;
use squallar_radar::fields as radar_fields;
use squallar_source::handler::PaneRef;
use squallar_source::id::{LayerId, known};

/// The two durations that bracket the idle backstop, deliberately **not** derived
/// from `POINTER_IDLE_TIMEOUT_S`.
const HOLD_MUST_SURVIVE_S: f64 = 45.0;
const SILENCE_MUST_EXPIRE_S: f64 = 70.0;

/// Long enough for a deferred single tap to be confirmed (`DOUBLE_TAP_TIMEOUT_S` is
/// 0.4s).
const AFTER_DOUBLE_TAP_TIMEOUT: f64 = 0.5;

/// How long a "the gesture really ended" assertion must keep watching.
const WATCH_PAST_LONG_PRESS: f64 = 30.0;

/// 1. A single mouse click reports a click position at the clicked point and never
///    suppresses panning.
#[test]
fn mouse_single_click_reports_click_pos() {
    let mut h = InputHarness::new();
    let pos = h.map_center();

    let outcome = h.mouse_click(pos);

    assert_eq!(outcome.mouse.overlay_click_pos, Some(pos));
    assert!(!outcome.mouse.suppress_pan);
    assert_eq!(outcome.mouse.long_press_pos, None);

    let next = h.frame_after(FRAME_DT);
    assert_eq!(next.mouse.overlay_click_pos, None);
}

/// 2. A mouse double click reports a click on each release, and the touch pipeline
///    defers instead of firing two overlay taps.
#[test]
fn mouse_double_click_reports_each_click() {
    let mut h = InputHarness::new();
    let pos = h.map_center();

    let first = h.mouse_click(pos);
    assert_eq!(first.mouse.overlay_click_pos, Some(pos));

    let second = h.mouse_click(pos);
    assert_eq!(second.mouse.overlay_click_pos, Some(pos));
    assert!(!second.mouse.suppress_pan);

    assert_eq!(first.touch.overlay_click_pos, None);
    assert_eq!(second.touch.overlay_click_pos, None);
}

/// 3. Pressing and holding for ~1s without moving is a long press: it reports the
///    held position and suppresses map panning, and it is not a click.
#[test]
fn press_and_hold_becomes_long_press() {
    let mut h = InputHarness::new();
    let pos = h.map_center();

    h.mouse_press(pos);
    let pressed = h.frame_after(FRAME_DT);
    assert_eq!(
        pressed.touch.long_press_pos, None,
        "not held long enough yet"
    );
    assert!(!pressed.touch.suppress_pan);

    let held = h.frames_for(10, 0.1);
    assert_eq!(held.touch.long_press_pos, Some(pos));
    assert!(held.touch.suppress_pan, "long press owns the pointer");
    assert_eq!(
        held.mouse.overlay_click_pos, None,
        "a press with no release is not a click"
    );

    h.mouse_release(pos);
    let released = h.frame_after(FRAME_DT);
    assert_eq!(released.touch.long_press_pos, None);
    assert!(!released.touch.suppress_pan);

    let settled = h.frames_for(3, 0.3);
    assert_eq!(
        settled.touch.overlay_click_pos, None,
        "a 1s hold is not a tap"
    );
}

/// 3b. The long press must not fire **early**.
#[test]
fn a_long_press_does_not_fire_before_its_threshold() {
    let mut h = InputHarness::new();
    let pos = h.map_center();

    h.touch_start(pos);
    h.assert_every_frame_for(0.7, 0.05, |frame, outcome| {
        assert_eq!(
            outcome.touch.long_press_pos, None,
            "frame {frame}: the hold is not old enough to be a long press"
        );
        assert!(
            !outcome.touch.suppress_pan,
            "frame {frame}: and nothing may take the pan yet either"
        );
    });

    let held = h.frames_for(4, 0.05);
    assert_eq!(held.touch.long_press_pos, Some(pos));
}

/// 3c. A finger that keeps moving never becomes a long press, however long it goes
/// on.
#[test]
fn a_moving_finger_never_becomes_a_long_press() {
    let mut h = InputHarness::new();
    let pos = h.map_center();

    h.touch_start(pos);
    h.frame_after(FRAME_DT);
    for step in 0..12 {
        let jitter = if step % 2 == 0 { 30.0 } else { 0.0 };
        h.touch_move(pos + egui::vec2(0.0, jitter));
        let moving = h.frame_after(0.1);
        assert_eq!(
            moving.touch.long_press_pos, None,
            "step {step}: a finger still moving is a pan, not a hold"
        );
        assert!(
            !moving.touch.suppress_pan,
            "step {step}: pan must stay live"
        );
    }
}

/// 4. A touch tap is deferred until the double-tap window closes, then reported
///    once at the tapped position.
#[test]
fn touch_tap_is_deferred_then_confirmed() {
    let mut h = InputHarness::new();
    let pos = h.map_center();

    let on_release = h.touch_tap(pos);
    assert_eq!(
        on_release.touch.overlay_click_pos, None,
        "tap must wait out the double-tap window"
    );
    assert!(!on_release.touch.suppress_pan);

    let confirmed = h.frame_after(AFTER_DOUBLE_TAP_TIMEOUT);
    assert_eq!(confirmed.touch.overlay_click_pos, Some(pos));
    assert!(!confirmed.touch.suppress_pan);

    let next = h.frame_after(FRAME_DT);
    assert_eq!(next.touch.overlay_click_pos, None);
}

/// 5. Tap, then press again and drag down: the map zooms, panning is suppressed for
///    the whole drag, and no overlay tap is emitted.
#[test]
fn touch_double_tap_drag_zooms_and_suppresses_pan() {
    let mut h = InputHarness::new();
    let start = h.map_center();
    let zoom_before = h.zoom();

    h.touch_tap(start);

    h.touch_start(start);
    let dragging = h.frame_after(0.05);
    assert!(
        dragging.touch.suppress_pan,
        "zoom drag must block map panning"
    );
    assert_eq!(dragging.touch.overlay_click_pos, None);
    assert_eq!(dragging.touch.long_press_pos, None);

    for step in 1..=3 {
        h.touch_move(start + egui::vec2(0.0, 50.0 * step as f32));
        let frame = h.frame_after(FRAME_DT);
        assert!(frame.touch.suppress_pan);
    }
    let dragged = h.frame_after(FRAME_DT);
    assert!(
        dragged.zoom > zoom_before,
        "dragging down should zoom in: {} -> {}",
        zoom_before,
        dragged.zoom
    );

    h.touch_end(start + egui::vec2(0.0, 150.0));
    let lifted = h.frame_after(FRAME_DT);
    assert!(!lifted.touch.suppress_pan, "pan must be restored on lift");

    let settled = h.frames_for(3, 0.3);
    assert_eq!(
        settled.touch.overlay_click_pos, None,
        "double-tap-drag must never open an overlay popup"
    );
}

/// 5b. A second tap somewhere else is a separate tap, not a double tap.
#[test]
fn a_second_tap_far_away_does_not_enter_a_zoom_drag() {
    let mut h = InputHarness::new();
    let near = h.map_center();
    let far = near + egui::vec2(200.0, 0.0);
    let zoom_before = h.zoom();

    h.touch_tap(near);
    h.touch_start(far);
    let pressed = h.frame_after(0.05);
    assert!(
        !pressed.touch.suppress_pan,
        "a tap 200px away is not the second half of a double tap"
    );

    for step in 1..=3 {
        h.touch_move(far + egui::vec2(0.0, 50.0 * step as f32));
        h.frame_after(FRAME_DT);
    }
    let dragged = h.frame_after(FRAME_DT);
    assert!(
        (dragged.zoom - zoom_before).abs() < 1e-9,
        "dragging after an unrelated tap must pan, not zoom: {zoom_before} \
             -> {}",
        dragged.zoom
    );
}

/// 5c. …but the second tap does not have to be pixel-exact.
#[test]
fn a_double_tap_tolerates_the_jitter_between_two_real_taps() {
    let mut h = InputHarness::new();
    let pos = h.map_center();

    h.touch_tap(pos);
    h.touch_start(pos + egui::vec2(10.0, 0.0));
    let pressed = h.frame_after(0.05);
    assert!(
        pressed.touch.suppress_pan,
        "10px between two taps is a double tap, not two singles"
    );
}

/// 5c-ii. …and it closes. **The other conjunct of the same classifier.**
#[test]
fn the_double_tap_window_closes_between_two_unrelated_taps() {
    /// Tap, sit still for `gap` seconds, then press again and hold.
    fn a_second_press_after(gap: f64) -> bool {
        let mut h = InputHarness::new();
        let pos = h.map_center();

        h.touch_tap(pos);
        h.frame_after(gap);
        h.touch_start(pos);
        h.frame_after(FRAME_DT).touch.suppress_pan
    }

    assert!(
        a_second_press_after(0.30),
        "two taps a third of a second apart are one double tap — a user \
             cannot double-tap faster than the timeout allows"
    );
    assert!(
        !a_second_press_after(0.45),
        "two taps nearly half a second apart are two taps: the second must \
             pan the map, not zoom it"
    );
}

/// 5d. A zoom drag held still is still a zoom drag.
#[test]
fn a_stationary_zoom_drag_does_not_become_a_long_press() {
    let mut h = InputHarness::new();
    let pos = h.map_center();

    h.touch_tap(pos);
    h.touch_start(pos);
    let dragging = h.frame_after(0.05);
    assert!(
        dragging.touch.suppress_pan,
        "precondition: the second press must have entered a zoom drag"
    );

    h.assert_every_frame_for(1.5, 0.1, |frame, outcome| {
        assert_eq!(
            outcome.touch.long_press_pos, None,
            "frame {frame}: a zoom drag owns the finger, tooltip and all"
        );
    });
}

/// 5e. The zoom drag moves one level per `ZOOM_DRAG_SENSITIVITY` pixels.
#[test]
fn the_zoom_drag_moves_one_level_per_sensitivity_of_travel() {
    let mut h = InputHarness::new();
    let pos = h.map_center();
    let zoom_before = h.zoom();

    h.touch_tap(pos);
    h.touch_start(pos);
    h.frame_after(0.05);
    h.touch_move(pos + egui::vec2(0.0, 150.0));
    let dragged = h.frame_after(FRAME_DT);

    assert!(
        (dragged.zoom - (zoom_before + 1.0)).abs() < 0.05,
        "150px of drag is one zoom level: {zoom_before} -> {}",
        dragged.zoom
    );
}

/// 6. **PROBE B — regression test for the stranded zoom drag.**
#[test]
fn touch_cancelled_mid_drag_releases_the_map() {
    let mut h = InputHarness::new();
    let start = h.map_center();

    h.touch_tap(start);
    h.touch_start(start);
    let dragging = h.frame_after(0.05);
    assert!(
        dragging.touch.suppress_pan,
        "precondition: zoom drag active"
    );

    h.touch_move(start + egui::vec2(0.0, 60.0));
    assert!(h.frame_after(FRAME_DT).touch.suppress_pan);

    h.touch_cancel(start + egui::vec2(0.0, 60.0));
    let cancelled = h.frame_after(FRAME_DT);
    assert!(
        !cancelled.touch.suppress_pan,
        "cancelled touch must not leave the map in zoom-drag"
    );
    assert_eq!(cancelled.touch.long_press_pos, None);

    h.assert_every_frame_for(WATCH_PAST_LONG_PRESS, 0.1, |frame, outcome| {
        assert!(
            !outcome.touch.suppress_pan,
            "frame {frame}: map must remain pannable after a cancelled touch"
        );
        assert_eq!(
            outcome.touch.long_press_pos, None,
            "frame {frame}: a cancelled touch must not become a long press"
        );
        assert_eq!(outcome.touch.overlay_click_pos, None, "frame {frame}");
    });
}

/// 6b. **PROBE A** — the same cancellation, but during a long press: the tooltip
/// position must not stick, and must not come back either.
#[test]
fn touch_cancelled_during_long_press_clears_it() {
    let mut h = InputHarness::new();
    let pos = h.map_center();

    h.touch_start(pos);
    let held = h.frames_for(10, 0.1);
    assert_eq!(
        held.touch.long_press_pos,
        Some(pos),
        "precondition: long press"
    );
    assert!(held.touch.suppress_pan);

    h.touch_cancel(pos);
    let cancelled = h.frame_after(FRAME_DT);
    assert_eq!(cancelled.touch.long_press_pos, None);
    assert!(!cancelled.touch.suppress_pan);

    h.assert_every_frame_for(WATCH_PAST_LONG_PRESS, 0.1, |frame, outcome| {
        assert_eq!(
            outcome.touch.long_press_pos, None,
            "frame {frame}: the long press must not re-arm itself"
        );
        assert!(!outcome.touch.suppress_pan, "frame {frame}");
    });
}

/// 6c. A *secondary* finger being cancelled must not kill the primary finger's live
/// gesture.
#[test]
fn secondary_touch_cancel_does_not_end_the_drag() {
    let mut h = InputHarness::new();
    let start = h.map_center();

    h.touch_tap(start);
    h.touch_start(start);
    assert!(
        h.frame_after(0.05).touch.suppress_pan,
        "precondition: zoom drag active"
    );

    h.secondary_touch_cancel(start + egui::vec2(80.0, 0.0));
    let after = h.frame_after(FRAME_DT);
    assert!(
        after.touch.suppress_pan,
        "another finger's cancellation must not end the primary gesture"
    );

    let zoom_before = after.zoom;
    h.touch_move(start + egui::vec2(0.0, 120.0));
    let dragged = h.frame_after(FRAME_DT);
    assert!(dragged.touch.suppress_pan);
    assert!(dragged.zoom > zoom_before, "the drag must still be live");
}

/// 6d. **PROBE C** — a zoom drag that keeps moving must never be cut off, however
/// long it runs — a user framing a view can easily hold one for many seconds.
#[test]
fn long_active_zoom_drag_is_never_cut_off() {
    let mut h = InputHarness::new();
    let start = h.map_center();

    h.touch_tap(start);
    h.touch_start(start);
    assert!(h.frame_after(0.05).touch.suppress_pan);

    let mut offset = 0.0_f32;
    for step in 0..80 {
        offset = if step % 2 == 0 { 40.0 } else { -40.0 };
        h.touch_move(start + egui::vec2(0.0, offset));
        let frame = h.frame_after(0.5);
        assert!(
            frame.touch.suppress_pan,
            "step {step}: an actively moving drag must stay in control"
        );
        assert_eq!(
            frame.touch.long_press_pos, None,
            "step {step}: the drag must not hand the finger to the long press"
        );
    }

    let zoom_before = h.zoom();
    h.touch_move(start + egui::vec2(0.0, offset + 100.0));
    let dragged = h.frame_after(FRAME_DT);
    assert_ne!(dragged.zoom, zoom_before, "the drag must still zoom");
}

/// 6e. If pointer input simply stops arriving mid-gesture (the integration went
/// away without ever sending a release or a cancel), the stale "finger is down"
/// belief expires — and does not get handed to the long press on the way out.
#[test]
fn silent_pointer_expires_and_stays_expired() {
    let mut h = InputHarness::new();
    let start = h.map_center();

    h.touch_tap(start);
    h.touch_start(start);
    assert!(h.frame_after(0.05).touch.suppress_pan);
    h.touch_move(start + egui::vec2(0.0, 40.0));
    assert!(h.frame_after(FRAME_DT).touch.suppress_pan);

    let expired = h.frames_for((SILENCE_MUST_EXPIRE_S / 0.5) as usize, 0.5);
    assert!(
        !expired.touch.suppress_pan,
        "a pointer that stopped reporting must not hold the map hostage"
    );

    h.assert_every_frame_for(WATCH_PAST_LONG_PRESS, 0.1, |frame, outcome| {
        assert!(!outcome.touch.suppress_pan, "frame {frame}");
        assert_eq!(
            outcome.touch.long_press_pos, None,
            "frame {frame}: an expired pointer must not become a long press"
        );
    });
}

/// 6f. **PROBE D** — the desktop excursion.
#[test]
fn pointer_returning_to_the_window_recovers_without_a_click() {
    let mut h = InputHarness::new();
    let inside = h.map_center();

    h.mouse_press(inside);
    let held = h.frames_for(10, 0.1);
    assert_eq!(
        held.touch.long_press_pos,
        Some(inside),
        "precondition: the pointer is live and held"
    );

    h.cursor_left();
    let gone = h.frame_after(FRAME_DT);
    assert_eq!(
        gone.touch.long_press_pos, None,
        "the held position must not stick"
    );
    assert!(!gone.touch.suppress_pan);

    let mut back = inside;
    for step in 1..=5 {
        back = inside + egui::vec2(12.0 * step as f32, 7.0 * step as f32);
        h.mouse_move(back);
        h.frame_after(FRAME_DT);
    }

    let hovering = h.frames_for(20, 0.1);
    assert_eq!(
        hovering.touch.long_press_pos, None,
        "a returning pointer must not open a hold nobody pressed for"
    );
    assert!(!hovering.touch.suppress_pan);

    h.mouse_press(back);
    let pressed = h.frames_for(10, 0.1);
    assert_eq!(pressed.touch.long_press_pos, Some(back));
    assert!(pressed.touch.suppress_pan);
}

/// 6f-R1. **PROBE R1** — a cancelled touch must not be resurrected by a bare
/// `PointerMoved`.
#[test]
fn motion_after_a_cancel_does_not_resurrect_the_pointer() {
    let mut h = InputHarness::new();
    let pos = h.map_center();

    h.touch_start(pos);
    assert_eq!(
        h.frames_for(10, 0.1).touch.long_press_pos,
        Some(pos),
        "precondition: long press active"
    );

    h.touch_cancel(pos);
    assert_eq!(h.frame_after(FRAME_DT).touch.long_press_pos, None);

    h.mouse_move(pos + egui::vec2(90.0, 60.0));
    h.frame_after(FRAME_DT);

    h.assert_every_frame_for(WATCH_PAST_LONG_PRESS, 0.1, |frame, outcome| {
        assert_eq!(
            outcome.touch.long_press_pos, None,
            "frame {frame}: motion is not the cancelled finger coming back"
        );
        assert!(!outcome.touch.suppress_pan, "frame {frame}");
    });
}

/// 6f-R2. **PROBE R2** — the same, for `MouseMoved`: a delta with no coordinates at
/// all.
#[test]
fn positionless_motion_after_a_cancel_does_not_resurrect_the_pointer() {
    let mut h = InputHarness::new();
    let pos = h.map_center();

    h.touch_start(pos);
    assert_eq!(h.frames_for(10, 0.1).touch.long_press_pos, Some(pos));

    h.touch_cancel(pos);
    assert_eq!(h.frame_after(FRAME_DT).touch.long_press_pos, None);

    h.mouse_moved_raw(egui::vec2(2.0, 1.0));
    h.frame_after(FRAME_DT);

    h.assert_every_frame_for(WATCH_PAST_LONG_PRESS, 0.1, |frame, outcome| {
        assert_eq!(
            outcome.touch.long_press_pos, None,
            "frame {frame}: a cancelled touch must not come back at its own last position"
        );
        assert!(!outcome.touch.suppress_pan, "frame {frame}");
    });
}

/// 6f-R3. **PROBE R3** — and for a cancelled *zoom drag*: motion must not hand the
/// map back to a gesture the OS took away.
#[test]
fn motion_after_a_cancelled_zoom_drag_does_not_restore_it() {
    let mut h = InputHarness::new();
    let start = h.map_center();

    h.touch_tap(start);
    h.touch_start(start);
    assert!(
        h.frame_after(0.05).touch.suppress_pan,
        "precondition: zoom drag"
    );

    h.touch_cancel(start);
    assert!(!h.frame_after(FRAME_DT).touch.suppress_pan);

    h.mouse_move(start + egui::vec2(0.0, 80.0));
    h.frame_after(FRAME_DT);

    h.assert_every_frame_for(WATCH_PAST_LONG_PRESS, 0.1, |frame, outcome| {
        assert!(
            !outcome.touch.suppress_pan,
            "frame {frame}: the map must stay pannable after a cancelled drag"
        );
        assert_eq!(outcome.touch.long_press_pos, None, "frame {frame}");
    });
}

/// 6f-R4. **PROBE R4** — a cancellation on the web, which arrives as a bare
/// `Touch{Cancel}`.
#[test]
fn web_touch_cancel_releases_the_map() {
    let mut h = InputHarness::new();
    let pos = h.map_center();

    h.web_touch_start(pos);
    h.frame_after(FRAME_DT);
    h.web_touch_move(pos + egui::vec2(2.0, 1.0));
    assert_eq!(
        h.frames_for(10, 0.1).touch.long_press_pos,
        Some(pos + egui::vec2(2.0, 1.0)),
        "precondition: long press active"
    );

    h.web_touch_cancel(pos + egui::vec2(2.0, 1.0));
    let cancelled = h.frame_after(FRAME_DT);
    assert_eq!(
        cancelled.touch.long_press_pos, None,
        "a bare Touch{{Cancel}} is the whole cancellation signal on the web"
    );
    assert!(!cancelled.touch.suppress_pan);

    h.web_mouse_move(pos + egui::vec2(70.0, 40.0));
    h.frame_after(FRAME_DT);

    h.assert_every_frame_for(WATCH_PAST_LONG_PRESS, 0.1, |frame, outcome| {
        assert_eq!(outcome.touch.long_press_pos, None, "frame {frame}");
        assert!(!outcome.touch.suppress_pan, "frame {frame}");
    });
}

/// 6f-R5. **PROBE R5** — the button was released *outside* the window.
#[test]
fn a_release_outside_the_window_does_not_return_as_a_hold() {
    let mut h = InputHarness::new();
    let inside = h.map_center();

    h.mouse_press(inside);
    h.frame_after(FRAME_DT);

    h.cursor_left();
    h.frame_after(FRAME_DT);

    h.mouse_move(inside + egui::vec2(30.0, 20.0));
    h.frame_after(FRAME_DT);

    h.assert_every_frame_for(WATCH_PAST_LONG_PRESS, 0.1, |frame, outcome| {
        assert_eq!(
            outcome.touch.long_press_pos, None,
            "frame {frame}: hovering must not become a hold"
        );
        assert!(
            !outcome.touch.suppress_pan,
            "frame {frame}: a phantom hold would kill panning until the next click"
        );
    });
}

/// 6g. **PROBE G** — recovery after the idle backstop fires.
#[test]
fn pointer_recovers_from_idle_expiry_without_a_lift() {
    let mut h = InputHarness::new();
    let start = h.map_center();

    h.touch_start(start);
    assert!(h.frame_after(FRAME_DT).touch.long_press_pos.is_none());

    let expired = h.frames_for((SILENCE_MUST_EXPIRE_S / 0.5) as usize, 0.5);
    assert_eq!(
        expired.touch.long_press_pos, None,
        "precondition: the backstop gave up on the pointer"
    );
    assert!(!expired.touch.suppress_pan);

    let resumed = start + egui::vec2(0.0, 60.0);
    h.touch_move(resumed);
    h.frame_after(FRAME_DT);

    let recovered = h.frames_for(10, 0.1);
    assert_eq!(
        recovered.touch.long_press_pos,
        Some(resumed),
        "a resumed gesture must recover on its own, with no lift and no re-press"
    );
    assert!(recovered.touch.suppress_pan);
}

/// 6h. **PROBE H** — a deliberately still hold keeps its tooltip.
#[test]
fn a_deliberately_still_hold_keeps_its_tooltip() {
    let mut h = InputHarness::new();
    let pos = h.map_center();

    h.touch_start(pos);
    let held = h.frames_for(10, 0.1);
    assert_eq!(
        held.touch.long_press_pos,
        Some(pos),
        "precondition: long press"
    );

    h.assert_every_frame_for(HOLD_MUST_SURVIVE_S, 0.25, |frame, outcome| {
        assert_eq!(
            outcome.touch.long_press_pos,
            Some(pos),
            "frame {frame}: the tooltip must survive a still finger"
        );
        assert!(outcome.touch.suppress_pan, "frame {frame}");
    });
}

/// 7. **PROBE I — the root cause, pinned against egui itself.**
#[test]
fn a_pinch_only_forms_when_both_fingers_share_a_touch_device() {
    /// Spread two fingers from 100px apart to 200px apart, and report what egui
    /// made of it.
    fn spread(devices: [u64; 2]) -> (f32, bool) {
        let ctx = egui::Context::default();
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1024.0, 768.0));
        let centre = egui::pos2(500.0, 400.0);
        let touch = |dev: u64, id: u64, phase, pos| egui::Event::Touch {
            device_id: egui::TouchDeviceId(dev),
            id: egui::TouchId(id),
            phase,
            pos,
            force: None,
        };
        let pass = |time: f64, half: f32, phase| {
            let first = centre - egui::vec2(half, 0.0);
            egui::RawInput {
                screen_rect: Some(screen),
                time: Some(time),
                events: vec![
                    touch(devices[0], 1, phase, first),
                    egui::Event::PointerMoved(first),
                    touch(devices[1], 2, phase, centre + egui::vec2(half, 0.0)),
                ],
                ..Default::default()
            }
        };

        ctx.begin_pass(pass(1.0, 50.0, egui::TouchPhase::Start));
        let _ = ctx.end_pass();
        ctx.begin_pass(pass(1.0 + FRAME_DT, 50.0, egui::TouchPhase::Move));
        let _ = ctx.end_pass();
        ctx.begin_pass(pass(1.0 + 2.0 * FRAME_DT, 100.0, egui::TouchPhase::Move));
        let seen = ctx.input(|i| (i.zoom_delta(), i.multi_touch().is_some()));
        let _ = ctx.end_pass();
        seen
    }

    let (web_zoom, web_multi) = spread([WEB_FINGER_A, WEB_FINGER_B]);
    assert!(
        !web_multi,
        "a device id per finger must be what breaks it — if egui now pairs \
             them across devices, `normalize_touch_devices` is obsolete"
    );
    assert_eq!(
        web_zoom, 1.0,
        "this is the bug: two fingers on two devices produce no zoom at all"
    );

    let (shared_zoom, shared_multi) = spread([0, 0]);
    assert!(shared_multi, "one device, two fingers: a gesture must form");
    assert!(
        shared_zoom > 1.9 && shared_zoom < 2.1,
        "doubling the gap must double the zoom factor, got {shared_zoom}"
    );
}

/// 7b. The fix in isolation: fingers keep their identities, devices merge.
#[test]
fn normalizing_merges_devices_and_leaves_the_fingers_alone() {
    let mut input = egui::RawInput {
        events: vec![
            web_touch(
                WEB_FINGER_A,
                egui::TouchPhase::Start,
                egui::pos2(10.0, 10.0),
            ),
            web_touch(
                WEB_FINGER_B,
                egui::TouchPhase::Start,
                egui::pos2(90.0, 10.0),
            ),
        ],
        ..Default::default()
    };
    crate::ui_input::normalize_touch_devices(&mut input);

    let seen: Vec<(egui::TouchDeviceId, egui::TouchId)> = input
        .events
        .iter()
        .filter_map(|e| match e {
            egui::Event::Touch { device_id, id, .. } => Some((*device_id, *id)),
            _ => None,
        })
        .collect();

    let devices: std::collections::BTreeSet<_> = seen.iter().map(|(d, _)| *d).collect();
    assert_eq!(devices.len(), 1, "both fingers must land on one device");
    let fingers: std::collections::BTreeSet<_> = seen.iter().map(|(_, f)| *f).collect();
    assert_eq!(
        fingers.len(),
        2,
        "the two fingers must stay distinct, or there is no gesture to form"
    );
}

/// 7c. **End to end: pinching out zooms the real map in.**
#[test]
fn a_web_pinch_out_zooms_the_map_in() {
    let mut h = InputHarness::new();
    let centre = h.pane_rects()[0].center();
    let before = h.frame_after(FRAME_DT).resolved_zoom;

    let pinched = h.web_pinch(centre, 80.0, 320.0, 8);

    assert_eq!(
        pinched.modality,
        PointerModality::Touch,
        "two fingers are touch"
    );
    assert!(
        pinched.resolved_zoom > before + 0.2,
        "pinching out must zoom the map in: {before} -> {}",
        pinched.resolved_zoom
    );
}

/// 7d. …and pinching in zooms out.
#[test]
fn a_web_pinch_in_zooms_the_map_out() {
    let mut h = InputHarness::new();
    let centre = h.pane_rects()[0].center();
    let before = h.frame_after(FRAME_DT).resolved_zoom;

    let pinched = h.web_pinch(centre, 320.0, 80.0, 8);

    assert!(
        pinched.resolved_zoom < before - 0.2,
        "pinching in must zoom the map out: {before} -> {}",
        pinched.resolved_zoom
    );
}

/// 7e. A pinch is not a tap and not a long press.
#[test]
fn a_pinch_is_not_a_tap() {
    let mut h = InputHarness::new();
    let centre = h.pane_rects()[0].center();

    let pinched = h.web_pinch(centre, 80.0, 320.0, 8);
    assert_eq!(
        pinched.resolved.overlay_click_pos, None,
        "a pinch must never resolve as an overlay tap"
    );

    h.web_second_finger_up(centre + egui::vec2(160.0, 0.0));
    h.web_first_finger_up(centre - egui::vec2(160.0, 0.0));
    h.assert_every_frame_for(1.0, 0.2, |frame, outcome| {
        assert_eq!(
            outcome.resolved.overlay_click_pos, None,
            "frame {frame}: lifting out of a pinch must not become a \
                 deferred tap either"
        );
    });
}

/// 7e-i. A quick flick is not a tap — **distance alone**.
#[test]
fn a_quick_flick_is_not_a_tap() {
    let mut h = InputHarness::new();
    let pos = h.map_center();

    h.touch_start(pos);
    h.frame_after(FRAME_DT);
    for step in 1..=4 {
        h.touch_move(pos + egui::vec2(30.0 * step as f32, 0.0));
        h.frame_after(FRAME_DT);
    }
    h.touch_end(pos + egui::vec2(120.0, 0.0));

    h.assert_every_frame_for(1.0, 0.1, |frame, outcome| {
        assert_eq!(
            outcome.touch.overlay_click_pos, None,
            "frame {frame}: a 120px flick is a drag, not a tap"
        );
    });
}

/// 7e-ii. A slow stationary press is not a tap — **duration alone**.
#[test]
fn a_slow_stationary_press_is_not_a_tap() {
    let mut h = InputHarness::new();
    let pos = h.map_center();

    h.touch_start(pos);
    h.frame_after(FRAME_DT);
    let held = h.frames_for(5, 0.1);
    assert_eq!(
        held.touch.long_press_pos, None,
        "precondition: 0.5s must still be short of a long press, or this \
             probe is testing the long-press path instead"
    );
    h.touch_end(pos);

    h.assert_every_frame_for(1.0, 0.1, |frame, outcome| {
        assert_eq!(
            outcome.touch.overlay_click_pos, None,
            "frame {frame}: a 0.5s hold is not a tap"
        );
    });
}

/// 7f. **A pinch that ends one finger at a time must not strand the map.**
#[test]
fn a_pinch_ending_one_finger_at_a_time_leaves_the_map_pannable() {
    let mut h = InputHarness::new();
    let centre = h.pane_rects()[0].center();

    h.web_pinch(centre, 80.0, 320.0, 8);

    let left = centre - egui::vec2(160.0, 0.0);
    let right = centre + egui::vec2(160.0, 0.0);
    h.web_first_finger_up(left);
    let lifted = h.frame_after(FRAME_DT);
    assert!(
        !lifted.resolved.suppress_pan,
        "the map must be released the moment the primary finger goes"
    );

    for step in 1..=4 {
        h.events.push(web_touch(
            WEB_FINGER_B,
            egui::TouchPhase::Move,
            right + egui::vec2(0.0, 10.0 * step as f32),
        ));
        let moving = h.frame_after(FRAME_DT);
        assert!(
            !moving.resolved.suppress_pan,
            "step {step}: a leftover finger must not re-suppress panning"
        );
    }
    h.web_second_finger_up(right + egui::vec2(0.0, 40.0));

    h.assert_every_frame_for(WATCH_PAST_LONG_PRESS, 0.1, |frame, outcome| {
        assert!(
            !outcome.resolved.suppress_pan,
            "frame {frame}: map must remain pannable after a pinch ends"
        );
        assert_eq!(
            outcome.resolved.long_press_pos, None,
            "frame {frame}: a finished pinch must not become a long press"
        );
    });
}

/// 7g. **A wheel notch must zoom the same however the browser spelled it.**
#[test]
fn a_wheel_notch_zooms_the_same_in_either_wheel_unit() {
    /// Four notches over the pane centre, and the zoom they moved.
    fn notches(unit: egui::MouseWheelUnit, delta_y: f32) -> f64 {
        let mut h = InputHarness::new();
        let centre = h.pane_rects()[0].center();
        let before = h.frame_after(FRAME_DT).resolved_zoom;
        let mut last = before;
        for _ in 0..4 {
            h.wheel_notch(centre, unit, delta_y);
            last = h.frames_for(12, FRAME_DT).resolved_zoom;
        }
        last - before
    }

    let chromium = notches(egui::MouseWheelUnit::Point, 120.0);
    let firefox = notches(egui::MouseWheelUnit::Line, 6.0);

    assert!(
        chromium.abs() > 0.5,
        "precondition: a pixel-mode notch must zoom at all, got {chromium}"
    );
    let ratio = firefox / chromium;
    assert!(
        (0.98..=1.02).contains(&ratio),
        "one notch must move the map the same in either browser: \
             Chromium {chromium}, Firefox {firefox} (ratio {ratio})"
    );
}

/// 7h. The rewrite in isolation: units converge, everything else survives.
#[test]
fn normalizing_wheel_units_converts_only_the_line_events() {
    let wheel = |unit, delta: egui::Vec2| egui::Event::MouseWheel {
        unit,
        delta,
        phase: egui::TouchPhase::Move,
        modifiers: egui::Modifiers::CTRL,
    };
    let mut input = egui::RawInput {
        events: vec![
            wheel(egui::MouseWheelUnit::Line, egui::vec2(2.0, 6.0)),
            wheel(egui::MouseWheelUnit::Point, egui::vec2(0.0, 120.0)),
        ],
        ..Default::default()
    };
    crate::ui_input::normalize_wheel_units(&mut input, 1.0);

    let seen: Vec<_> = input
        .events
        .iter()
        .filter_map(|e| match e {
            egui::Event::MouseWheel {
                unit,
                delta,
                phase,
                modifiers,
            } => Some((*unit, *delta, *phase, *modifiers)),
            _ => None,
        })
        .collect();

    assert!(
        seen.iter().all(|(u, ..)| *u == egui::MouseWheelUnit::Point),
        "every wheel event must leave in point units, got {seen:?}"
    );
    assert_eq!(seen[0].1, egui::vec2(40.0, 120.0), "line delta must scale");
    assert_eq!(
        seen[1].1,
        egui::vec2(0.0, 120.0),
        "a point delta was already normal and must not be touched"
    );
    assert_eq!(
        seen[0].2,
        egui::TouchPhase::Move,
        "phase must survive — egui starts and ends wheel gestures on it"
    );
    assert_eq!(
        seen[0].3,
        egui::Modifiers::CTRL,
        "modifiers must survive — ctrl is what routes a wheel to zoom"
    );
}

/// 7i. The app's UI scale must not change the wheel step in one unit only.
#[test]
fn the_wheel_rewrite_divides_by_the_zoom_factor() {
    let mut input = egui::RawInput {
        events: vec![egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Line,
            delta: egui::vec2(0.0, 6.0),
            phase: egui::TouchPhase::Move,
            modifiers: egui::Modifiers::default(),
        }],
        ..Default::default()
    };
    crate::ui_input::normalize_wheel_units(&mut input, 2.0);

    let egui::Event::MouseWheel { delta, .. } = input.events[0] else {
        panic!("expected a wheel event");
    };
    assert_eq!(
        delta.y, 60.0,
        "a 2x UI scale must halve the rewritten delta, as it already does \
             to the pixel deltas egui-winit produced"
    );
}

/// 8. **The panel decides which edge, and that decision reaches the paint.**
#[test]
fn every_pane_draws_its_color_scale_on_the_same_edge() {
    let mut h = InputHarness::with_screen(egui::vec2(1010.0, 1450.0));
    h.set_pane_count(3);
    h.frame();

    let panes = h.pane_rects();
    assert_eq!(panes.len(), 3, "precondition: a [2, 1] grid");

    let ratio = |r: egui::Rect| r.height() / r.width();
    assert!(
        ratio(panes[0]) > 1.35,
        "top panes must be clearly portrait, got {}",
        ratio(panes[0])
    );
    assert!(
        ratio(panes[2]) < 1.05,
        "the bottom pane must be clearly landscape, got {} — otherwise the \
             panes do not disagree and this test proves nothing",
        ratio(panes[2])
    );

    for (idx, pane) in panes.iter().enumerate() {
        let (horizontal, vertical) = h.color_scale_bars(*pane);
        assert!(
            horizontal > 0,
            "pane {idx}: expected a bottom-edge colour bar, painted none"
        );
        assert_eq!(
            vertical, 0,
            "pane {idx}: painted a right-edge bar — the panes disagree, \
                 which is the whole artefact the panel-keyed decision removes"
        );
    }
}

/// 8b. **The panel is the key, not the active pane.**
#[test]
fn the_color_scale_axis_comes_from_the_panel_not_a_pane() {
    let mut h = InputHarness::with_screen(egui::vec2(1180.0, 1000.0));
    h.set_pane_count(2);
    h.frame();

    let panel = h.map_panel_rect();
    let panes = h.pane_rects();
    assert_eq!(panes.len(), 2);

    let ratio = |r: egui::Rect| r.height() / r.width();
    assert!(
        ratio(panel) < 1.05,
        "precondition: the panel must be clearly not portrait, got {}",
        ratio(panel)
    );
    assert!(
        ratio(panes[0]) > 1.35,
        "precondition: each pane must be clearly portrait, got {} — \
             otherwise panel and pane agree and this test proves nothing",
        ratio(panes[0])
    );

    for (idx, pane) in panes.iter().enumerate() {
        let (horizontal, vertical) = h.color_scale_bars(*pane);
        assert!(
            vertical > 0,
            "pane {idx}: the landscape *panel* decides, so the bar belongs \
                 on the right edge — painted none there"
        );
        assert_eq!(
            horizontal, 0,
            "pane {idx}: painted a bottom bar, i.e. the axis was taken from \
                 the pane's own shape"
        );
    }
}

/// 8c. **The hail-size preference reaches the MEHS colour bar on the glass.**
#[test]
fn the_mehs_colour_bar_paints_the_users_hail_size_unit() {
    use squallar_units::HailSizeUnit;

    /// The ¼-in stops of `palette::MEHS` as the bar labels them in inches.
    const INCH_TICKS: [&str; 8] = ["0.2", "0.5", "0.8", "1.2", "1.5", "1.8", "2.5", "3.5"];
    /// The same stops in whole millimetres, 1.00 in landing on 25.
    const MM_TICKS: [&str; 12] = [
        "6", "13", "19", "25", "32", "38", "44", "51", "64", "76", "89", "102",
    ];

    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    h.select_product(0, &radar_fields::known::MAX_EXPECTED_HAIL_SIZE);
    let pane = h.pane_rects()[0];

    let painted = h.painted_text_strings_in(pane);
    assert!(
        painted.iter().any(|t| t == "in"),
        "no `in` title over the default MEHS bar; painted: {painted:?}",
    );
    for tick in INCH_TICKS {
        assert!(
            painted.iter().any(|t| t == tick),
            "the inch bar is missing its {tick} tick; painted: {painted:?}",
        );
    }

    h.gui_mut().preferences.hail_size = HailSizeUnit::Millimeters;
    h.warm_up();

    let painted = h.painted_text_strings_in(pane);
    assert!(
        painted.iter().any(|t| t == "mm"),
        "the bar still is not titled `mm`; painted: {painted:?}",
    );
    for tick in MM_TICKS {
        assert!(
            painted.iter().any(|t| t == tick),
            "the mm bar is missing its {tick} tick; painted: {painted:?}",
        );
    }
    assert!(
        !painted.iter().any(|t| t == "in"),
        "`in` is still over a bar labelled in millimetres; painted: {painted:?}",
    );
    for tick in INCH_TICKS {
        assert!(
            !painted.iter().any(|t| t == tick),
            "the {tick} inch tick survived the switch to millimetres; \
                 painted: {painted:?}",
        );
    }
}

/// KTLX's 0.5° Doppler cut's own declaration, m/s — 2026-08-11 at 10:09, and the
/// narrowest of the ten WSR-88D volumes `squallar_radar::nyquist` measured.
const DECLARED_NYQUIST_MS: f64 = 23.84;

/// A declaration past the end of the ramp, m/s.
const PAST_THE_BAR_MS: f64 = 63.5;

/// A landscape pane on `site` showing a finished velocity render that declares
/// `nyquist_ms`, radar layer on.
fn velocity_pane(site: &str, nyquist_ms: Option<f64>) -> InputHarness {
    velocity_pane_on(egui::vec2(1400.0, 900.0), site, nyquist_ms)
}

/// The same, on a screen of the caller's choosing — the portrait case puts the bar
/// along the bottom, where the annotation is laid out differently.
fn velocity_pane_on(screen: egui::Vec2, site: &str, nyquist_ms: Option<f64>) -> InputHarness {
    let mut h = InputHarness::with_screen(screen);
    h.load_scan(site);
    h.gui_mut()
        .pane_mut(0)
        .unwrap()
        .set_overlay_enabled(known::RADAR, true);
    h.offer_product(0, &radar_fields::known::REFLECTIVITY, 0.5);
    h.offer_product(0, &radar_fields::known::VELOCITY, 0.5);
    h.select_product(0, &radar_fields::known::VELOCITY);
    h.place_radar_image(
        0,
        &radar_fields::known::VELOCITY,
        0.5,
        nyquist_ms,
        None,
        None,
    );
    h
}

/// The `folds ±N` line the pane painted, if it painted one.
fn fold_line_painted(h: &InputHarness) -> Option<String> {
    h.painted_text_strings_in(h.pane_rects()[0])
        .into_iter()
        .find(|t| t.starts_with("folds"))
}

/// The fold markers painted over the bar: rects as long as the 20-point bar plus
/// its 3-point overhang on each face, which is a shape nothing else on the legend
/// has.
fn fold_markers_painted(h: &InputHarness) -> Vec<egui::Rect> {
    const MARKER_LENGTH: f32 = 26.0;
    let pane = h.pane_rects()[0];
    h.painted_rects()
        .iter()
        .filter(|r| pane.contains(r.center()))
        .filter(|r| {
            (r.width() - MARKER_LENGTH).abs() < 0.5 || (r.height() - MARKER_LENGTH).abs() < 0.5
        })
        .copied()
        .collect()
}

/// 8d. **The velocity bar says where the picture on it folds, in the reader's own
/// speed unit.**
#[test]
fn the_velocity_bar_says_where_its_own_sweep_folds_in_every_speed_unit() {
    use squallar_units::SpeedUnit;

    let mut h = velocity_pane("KTLX", Some(DECLARED_NYQUIST_MS));
    let pane = h.pane_rects()[0];

    let expected = [
        (SpeedUnit::Mph, "folds \u{b1}53"),
        (SpeedUnit::MetersPerSec, "folds \u{b1}24"),
        (SpeedUnit::KilometersPerHour, "folds \u{b1}86"),
        (SpeedUnit::Knots, "folds \u{b1}46"),
    ];
    let mut marker_positions: Option<Vec<egui::Rect>> = None;
    for (unit, line) in expected {
        h.gui_mut().preferences.speed = unit;
        h.warm_up();
        assert_eq!(
            fold_line_painted(&h).as_deref(),
            Some(line),
            "{unit:?}: the bar's fold annotation is not the declared limit in \
             the unit the reader asked for; painted: {:?}",
            h.painted_text_strings_in(pane),
        );
        let markers = fold_markers_painted(&h);
        assert_eq!(
            markers.len(),
            2,
            "{unit:?}: expected a marker at each of ±Vny, got {markers:?}",
        );
        match &marker_positions {
            None => marker_positions = Some(markers),
            Some(first) => assert_eq!(
                &markers, first,
                "{unit:?}: the fold markers moved when the unit changed, so \
                 the bar was rescaled rather than relabelled",
            ),
        }
    }

    h.gui_mut().preferences.speed = SpeedUnit::MetersPerSec;
    h.warm_up();
    let painted = h.painted_text_strings_in(pane);
    assert!(
        painted.iter().any(|t| t == "m/s"),
        "the annotation replaced the unit title instead of standing under it; \
         painted: {painted:?}",
    );
}

/// 8e. **A fold the bar cannot reach is named and not marked.**
#[test]
fn a_nyquist_past_the_end_of_the_bar_is_named_but_not_marked() {
    let h = velocity_pane("KTLX", Some(PAST_THE_BAR_MS));

    assert_eq!(
        fold_line_painted(&h).as_deref(),
        Some("folds \u{b1}142"),
        "the off-scale limit is not stated; painted: {:?}",
        h.painted_text_strings_in(h.pane_rects()[0]),
    );
    assert!(
        fold_markers_painted(&h).is_empty(),
        "a fold at {PAST_THE_BAR_MS} m/s was marked on a bar that stops at \
         36.01: {:?}",
        fold_markers_painted(&h),
    );

    let mut h = h;
    h.gui_mut().preferences.speed = squallar_units::SpeedUnit::KilometersPerHour;
    h.warm_up();
    let pane = h.pane_rects()[0];
    let widest: Vec<(egui::Rect, String)> = h
        .painted_text_rects()
        .into_iter()
        .filter(|(_, text)| text == "folds \u{b1}229")
        .collect();
    assert!(
        !widest.is_empty(),
        "the annotation did not follow the unit change; painted: {:?}",
        h.painted_text_strings_in(pane),
    );
    for (rect, text) in widest {
        assert!(
            pane.contains_rect(rect),
            "{text:?} at {rect:?} runs outside the pane {pane:?}",
        );
    }
}

/// 8f. **An undeclared sweep leaves the bar exactly as it was.**
#[test]
fn a_sweep_that_declared_no_nyquist_gets_the_bar_unchanged() {
    let mut h = velocity_pane("KTLX", None);
    let pane = h.pane_rects()[0];

    let painted = h.painted_text_strings_in(pane);
    assert!(
        painted.iter().any(|t| t == "mph"),
        "precondition: the velocity bar must be drawn at all; painted: \
         {painted:?}",
    );
    assert_eq!(
        fold_line_painted(&h),
        None,
        "a bar with nothing declared behind it claimed a fold limit; painted: \
         {painted:?}",
    );
    assert!(
        fold_markers_painted(&h).is_empty(),
        "markers were painted for a sweep that declared no fold limit",
    );

    let title_baseline = |h: &InputHarness, unit: &str| -> f32 {
        h.painted_text_rects()
            .into_iter()
            .filter(|(rect, text)| text == unit && pane.contains(rect.center()))
            .map(|(rect, _)| rect.bottom())
            .fold(f32::INFINITY, f32::min)
    };
    let undeclared = title_baseline(&h, "mph");
    h.select_product(0, &radar_fields::known::REFLECTIVITY);
    h.place_radar_image(0, &radar_fields::known::REFLECTIVITY, 0.5, None, None, None);
    assert_eq!(
        undeclared,
        title_baseline(&h, "dBZ"),
        "the velocity title sits at a different height from the reflectivity \
         title on the same bar, so an absent annotation still moved the block",
    );
}

/// 8f(ii). **A fold limit of zero is no fold limit, and the bar says nothing rather
/// than `folds ±0`.**
#[test]
fn a_pane_with_no_usable_fold_limit_captions_nothing() {
    for absurd in [0.0, -1.0, f64::NAN] {
        let h = velocity_pane("KTLX", Some(absurd));
        let pane = h.pane_rects()[0];
        let painted = h.painted_text_strings_in(pane);
        assert!(
            painted.iter().any(|t| t == "mph"),
            "precondition: the velocity bar must be drawn at all; painted: \
             {painted:?}",
        );
        assert_eq!(
            fold_line_painted(&h),
            None,
            "a fold limit of {absurd} was captioned rather than dropped; \
             painted: {painted:?}",
        );
        assert!(
            fold_markers_painted(&h).is_empty(),
            "a fold limit of {absurd} was marked on the bar",
        );
    }

    let good = velocity_pane("KTLX", Some(DECLARED_NYQUIST_MS));
    assert_eq!(fold_line_painted(&good).as_deref(), Some("folds \u{b1}53"));
}

/// 8g. **The annotation describes the pixels, not the selection.**
#[test]
fn a_pane_annotates_no_fold_while_its_image_lags_the_selection() {
    let mut h = velocity_pane("KTLX", Some(DECLARED_NYQUIST_MS));
    assert!(
        fold_line_painted(&h).is_some(),
        "precondition: the pane must be annotating its own render",
    );

    h.select_product(0, &radar_fields::known::REFLECTIVITY);
    assert_eq!(
        fold_line_painted(&h),
        None,
        "the reflectivity bar carries a velocity sweep's fold limit",
    );

    h.select_product(0, &radar_fields::known::VELOCITY);
    h.place_radar_image(
        0,
        &radar_fields::known::VELOCITY,
        2.4,
        Some(DECLARED_NYQUIST_MS),
        None,
        None,
    );
    assert_eq!(
        fold_line_painted(&h),
        None,
        "a fold limit was drawn over a sweep the pane did not select",
    );
    h.place_radar_image(
        0,
        &radar_fields::known::VELOCITY,
        0.5,
        Some(DECLARED_NYQUIST_MS),
        None,
        None,
    );
    assert!(
        fold_line_painted(&h).is_some(),
        "the annotation did not come back with the pane's own sweep",
    );
}

/// 8h. **The annotation is drawn where nothing else is, and does not change what
/// the bar is made of.**
#[test]
fn the_fold_annotation_leaves_the_bar_and_the_pane_alone() {
    let plain = velocity_pane("KTLX", None);
    let pane = plain.pane_rects()[0];
    let bare = plain.color_scale_bars(pane);
    assert!(
        bare.1 > 0,
        "precondition: a landscape panel draws a right-edge bar, got {bare:?}",
    );

    let annotated = velocity_pane("KTLX", Some(DECLARED_NYQUIST_MS));
    assert_eq!(
        annotated.color_scale_bars(pane),
        bare,
        "the fold markers or the range-folded swatch are being counted as ramp",
    );

    for (rect, text) in annotated.painted_text_rects() {
        if text.starts_with("folds") || text == "RF" {
            assert!(
                pane.contains_rect(rect),
                "{text:?} at {rect:?} runs outside the pane {pane:?}",
            );
        }
    }
    for marker in fold_markers_painted(&annotated) {
        assert!(
            pane.contains_rect(marker),
            "a fold marker at {marker:?} runs outside the pane {pane:?}",
        );
    }
}

/// 8h(ii). **…on the bottom-edge bar too, which lays the annotation out somewhere
/// else entirely.**
#[test]
fn a_bottom_edge_bar_annotates_under_itself_rather_than_off_the_pane() {
    let portrait = egui::vec2(900.0, 1400.0);
    let plain = velocity_pane_on(portrait, "KTLX", None);
    let pane = plain.pane_rects()[0];
    let bare = plain.color_scale_bars(pane);
    assert!(
        bare.0 > 0,
        "precondition: a portrait panel draws a bottom-edge bar, got {bare:?}",
    );

    let h = velocity_pane_on(portrait, "KTLX", Some(DECLARED_NYQUIST_MS));
    assert_eq!(
        h.color_scale_bars(pane),
        bare,
        "the fold markers or the range-folded swatch are being counted as ramp",
    );
    assert_eq!(
        fold_line_painted(&h).as_deref(),
        Some("folds \u{b1}53"),
        "painted: {:?}",
        h.painted_text_strings_in(pane),
    );
    assert_eq!(fold_markers_painted(&h).len(), 2);

    for (rect, text) in h.painted_text_rects() {
        if text.starts_with("folds") || text == "RF" {
            assert!(
                pane.contains_rect(rect),
                "{text:?} at {rect:?} runs outside the pane {pane:?}, where \
                 the pane's own clip rect would cut it in half",
            );
        }
    }
    for marker in fold_markers_painted(&h) {
        assert!(pane.contains_rect(marker), "{marker:?} outside {pane:?}");
    }
}

/// 8h(iv). **On a phone the whole legend clears the bottom bar, rather than being
/// painted under it.**
#[test]
fn a_phones_colour_scale_clears_the_bottom_bar_that_was_hiding_it() {
    let phone = egui::vec2(432.0, 936.0);
    let h = velocity_pane_on(phone, "KTLX", Some(DECLARED_NYQUIST_MS));
    let pane = h.pane_rects()[0];

    let bar = h.bottom_bar().rect;
    assert!(
        bar.is_finite() && bar.height() > 0.0,
        "precondition: a 432\u{d7}936 window must be Compact and draw the phone \
         bottom bar — without it this test proves nothing; got {bar:?}",
    );
    let bars = h.color_scale_bars(pane);
    assert!(
        bars.0 > 0,
        "precondition: a portrait panel draws a bottom-edge colour bar, got \
         {bars:?}",
    );

    let ramp: Vec<egui::Rect> = h
        .painted_images_in(pane)
        .into_iter()
        .map(|i| i.rect)
        .filter(|r| (r.height() - 20.0).abs() < 0.5 && r.width() > 40.0)
        .collect();
    assert!(!ramp.is_empty(), "precondition: the ramp must be painted");

    for strip in &ramp {
        assert!(
            strip.bottom() <= bar.top() + 0.5,
            "the ramp at {strip:?} runs under the bottom bar {bar:?}, where \
             nothing on it can be read",
        );
    }

    let legend_text: Vec<(egui::Rect, String)> = h
        .painted_text_rects()
        .into_iter()
        .filter(|(rect, _)| pane.contains(rect.center()))
        .filter(|(_, text)| {
            text == "mph"
                || text == "RF"
                || text.starts_with("folds")
                || text.parse::<i32>().is_ok()
        })
        .collect();
    assert!(
        legend_text.iter().any(|(_, t)| t == "mph"),
        "precondition: the unit title must be painted; got {legend_text:?}",
    );
    assert!(
        legend_text.iter().any(|(_, t)| t.starts_with("folds")),
        "precondition: the fold line must be painted; got {legend_text:?}",
    );
    for (rect, text) in &legend_text {
        assert!(
            rect.bottom() <= bar.top() + 0.5,
            "{text:?} at {rect:?} is drawn under the bottom bar {bar:?}",
        );
    }

    for marker in fold_markers_painted(&h) {
        assert!(
            marker.bottom() <= bar.top() + 0.5,
            "a fold marker at {marker:?} is drawn under the bottom bar {bar:?}",
        );
    }
}

/// 8h(iii). **Every unit title is drawn inside the pane it labels.**
#[test]
fn every_products_unit_title_fits_inside_its_pane() {
    use squallar_units::{HailSizeUnit, HeightUnit, PrecipRateUnit, SpeedUnit};

    for screen in [egui::vec2(900.0, 1400.0), egui::vec2(1400.0, 900.0)] {
        let mut h = velocity_pane_on(screen, "KTLX", None);
        let pane = h.pane_rects()[0];
        for &speed in SpeedUnit::ALL {
            for &height in HeightUnit::ALL {
                for &hail_size in HailSizeUnit::ALL {
                    for &precip_rate in PrecipRateUnit::ALL {
                        {
                            let prefs = &mut h.gui_mut().preferences;
                            prefs.speed = speed;
                            prefs.height = height;
                            prefs.hail_size = hail_size;
                            prefs.precip_rate = precip_rate;
                        }
                        for product in radar_fields::known::ALL.iter() {
                            h.select_product(0, product);
                            let unit =
                                crate::field_facts::unit_label(product, &h.gui_mut().preferences);
                            let titles: Vec<egui::Rect> = h
                                .painted_text_rects()
                                .into_iter()
                                .filter(|(rect, text)| text == unit && pane.contains(rect.center()))
                                .map(|(rect, _)| rect)
                                .collect();
                            assert!(
                                !titles.is_empty(),
                                "{product:?} painted no {unit:?} title at all on \
                                 a {screen:?} screen; painted: {:?}",
                                h.painted_text_strings_in(pane),
                            );
                            for rect in titles {
                                assert!(
                                    pane.contains_rect(rect),
                                    "{product:?}'s {unit:?} title at {rect:?} \
                                     hangs outside the pane {pane:?} on a \
                                     {screen:?} screen, where the pane's clip \
                                     rect cuts it off",
                                );
                            }
                        }
                    }
                }
            }
        }
    }
}

/// 8i. **The purple on the map has a key, on the two products that can paint it.**
#[test]
fn the_range_folded_purple_is_keyed_on_the_bars_that_can_paint_it() {
    let mut h = velocity_pane("KTLX", Some(DECLARED_NYQUIST_MS));
    let pane = h.pane_rects()[0];
    let purple = {
        let (r, g, b, a) = squallar_radar::RANGE_FOLDED;
        egui::Color32::from_rgba_unmultiplied(r, g, b, a)
    };
    let keyed = |h: &InputHarness| {
        h.painted_text_strings_in(pane).iter().any(|t| t == "RF")
            && h.painted_fills_within(pane, 0.0).contains(&purple)
    };

    assert!(
        keyed(&h),
        "the velocity bar has no range-folded key; painted: {:?}",
        h.painted_text_strings_in(pane),
    );

    h.offer_product(0, &radar_fields::known::SPECTRUM_WIDTH, 0.5);
    h.select_product(0, &radar_fields::known::SPECTRUM_WIDTH);
    h.place_radar_image(
        0,
        &radar_fields::known::SPECTRUM_WIDTH,
        0.5,
        None,
        None,
        None,
    );
    assert!(
        keyed(&h),
        "spectrum width carries range-folded gates too and has no key for \
         them; painted: {:?}",
        h.painted_text_strings_in(pane),
    );
    assert_eq!(fold_line_painted(&h), None);

    h.select_product(0, &radar_fields::known::REFLECTIVITY);
    h.place_radar_image(0, &radar_fields::known::REFLECTIVITY, 0.5, None, None, None);
    assert!(
        !keyed(&h),
        "the reflectivity bar keys a colour its surveillance cut does not \
         paint; painted: {:?}",
        h.painted_text_strings_in(pane),
    );
}

/// 8j. **The annotation is render-derived, so it comes back with the picture and
/// nothing has to persist it.**
#[test]
fn the_fold_annotation_returns_with_the_picture_rather_than_from_a_config() {
    let mut h = velocity_pane("KTLX", Some(DECLARED_NYQUIST_MS));
    let before = fold_line_painted(&h).expect("precondition: an annotated pane");

    h.gui_mut()
        .pane_mut(0)
        .unwrap()
        .overlay_cache_mut(&known::RADAR)
        .clear();
    h.warm_up();
    assert_eq!(
        fold_line_painted(&h),
        None,
        "a pane with no picture on it still claimed to know where that \
         picture folded",
    );

    h.place_radar_image(
        0,
        &radar_fields::known::VELOCITY,
        0.5,
        Some(DECLARED_NYQUIST_MS),
        None,
        None,
    );
    assert_eq!(
        fold_line_painted(&h).as_deref(),
        Some(before.as_str()),
        "the annotation did not come back with the restored picture",
    );
}

/// 53. A tap that lands on a floating dialog is filtered out by the dialog-blocking
///     gate — for both the mouse and the touch path.
#[test]
fn tap_on_floating_dialog_is_filtered_out() {
    let mut h = InputHarness::new();
    h.gui_mut().set_time_dialog_open_for_test(true);
    h.warm_up();

    let pos = h.screen_center();
    assert!(
        h.is_floating_layer_at(pos),
        "precondition: the time dialog must cover the viewport centre"
    );
    assert!(
        h.map_center().distance(pos) < 200.0,
        "precondition: the dialog sits over the map pane, so only the \
             dialog gate can filter this click"
    );

    let clicked = h.mouse_click(pos);
    assert_eq!(clicked.mouse.overlay_click_pos, None);
    assert!(!clicked.mouse.suppress_pan);

    let tapped = h.touch_tap(pos);
    assert_eq!(tapped.touch.overlay_click_pos, None);
    let settled = h.frames_for(3, 0.3);
    assert_eq!(settled.touch.overlay_click_pos, None);

    h.gui_mut().set_time_dialog_open_for_test(false);
    h.warm_up();
    assert!(!h.is_floating_layer_at(pos));
    let clicked = h.mouse_click(pos);
    assert_eq!(clicked.mouse.overlay_click_pos, Some(pos));
}

/// 53b. A touch tap is deferred by 0.4s, so a dialog can open *during* the
/// deferral. The tap was legitimately on the map when it happened, so the
/// detector's own on-release check passes it through, and only
/// `filter_dialog_blocked` can stop it from punching through the dialog that is now
/// covering it.
#[test]
fn tap_confirmed_under_a_dialog_is_filtered_out() {
    let mut h = InputHarness::new();
    let pos = h.map_center();

    assert!(!h.is_floating_layer_at(pos));
    let tapped = h.touch_tap(pos);
    assert_eq!(tapped.touch.overlay_click_pos, None, "still deferred");

    h.gui_mut().set_time_dialog_open_for_test(true);
    h.frame_after(FRAME_DT);
    assert!(
        h.is_floating_layer_at(pos),
        "precondition: the dialog now covers the tapped point"
    );

    let confirmed = h.frame_after(AFTER_DOUBLE_TAP_TIMEOUT);
    assert_eq!(
        confirmed.touch.overlay_click_pos, None,
        "a tap confirmed under a dialog must not reach the map"
    );
    let settled = h.frames_for(3, 0.3);
    assert_eq!(settled.touch.overlay_click_pos, None);

    h.gui_mut().set_time_dialog_open_for_test(false);
    h.warm_up();
    h.touch_tap(pos);
    let confirmed = h.frame_after(AFTER_DOUBLE_TAP_TIMEOUT);
    assert_eq!(confirmed.touch.overlay_click_pos, Some(pos));
}

/// 9. **A slow mouse press is not a long press.**
#[test]
fn a_slow_mouse_press_never_becomes_a_long_press() {
    let mut h = InputHarness::new();
    let pos = h.map_center();

    h.mouse_press(pos);
    let held = {
        h.frame_after(FRAME_DT);
        h.frames_for(10, 0.1)
    };

    assert_eq!(
        held.modality,
        PointerModality::Mouse,
        "precondition: mouse events must have latched the mouse modality"
    );
    assert_eq!(
        held.touch.long_press_pos,
        Some(pos),
        "precondition: ungated, this input really does trip the detector — \
             otherwise the assertion below is satisfied by nothing happening"
    );

    assert_eq!(
        held.resolved.long_press_pos, None,
        "the gate must keep the long-press detector off a mouse"
    );
    assert!(
        !held.resolved.suppress_pan,
        "a held mouse button must still pan the map"
    );
}

/// 10. **A mouse click is not deferred.**
#[test]
fn a_mouse_click_reports_immediately_rather_than_after_the_tap_window() {
    let mut h = InputHarness::new();
    let pos = h.map_center();

    let clicked = h.mouse_click(pos);
    assert_eq!(clicked.modality, PointerModality::Mouse);
    assert_eq!(
        clicked.resolved.overlay_click_pos,
        Some(pos),
        "the click must land on the frame it happened"
    );
    assert_eq!(
        clicked.touch.overlay_click_pos, None,
        "precondition: the touch pipeline would still be deferring it, so \
             the assertion above is about the gate"
    );
}

/// 10b. The touch path keeps its deferral, so the test above is a statement about
/// the modality and not about the deferral having been deleted.
#[test]
fn a_real_touch_tap_is_still_deferred_through_the_gate() {
    let mut h = InputHarness::new();
    let pos = h.map_center();

    let tapped = h.touch_tap(pos);
    assert_eq!(
        tapped.modality,
        PointerModality::Touch,
        "precondition: touch events latch the touch modality"
    );
    assert_eq!(tapped.resolved.overlay_click_pos, None, "still deferred");

    let confirmed = h.frame_after(AFTER_DOUBLE_TAP_TIMEOUT);
    assert_eq!(confirmed.resolved.overlay_click_pos, Some(pos));
}

/// 11. **A mouse double-click does not enter a zoom drag.**
#[test]
fn a_mouse_double_click_does_not_start_a_zoom_drag() {
    let mut h = InputHarness::new();
    let pos = h.map_center();

    let before = h.frame_after(FRAME_DT).resolved_zoom;

    h.mouse_click(pos);
    h.mouse_press(pos);
    h.frame_after(0.05);
    h.mouse_move(pos + egui::vec2(0.0, 150.0));
    let dragged = h.frame_after(FRAME_DT);

    assert_eq!(dragged.modality, PointerModality::Mouse);
    assert_eq!(
        dragged.resolved_zoom, before,
        "a mouse double-click-drag must not scrub the map zoom"
    );
    assert!(
        !dragged.resolved.suppress_pan,
        "and it must leave panning to the map"
    );
}

/// 11b. The same gesture on the ungated touch path *does* zoom, so the test above
/// is not simply asserting that the gesture never works.
#[test]
fn the_same_drag_does_zoom_when_it_really_is_a_touch() {
    let mut h = InputHarness::new();
    let pos = h.map_center();

    let before = h.frame_after(FRAME_DT).resolved_zoom;

    h.touch_tap(pos);
    h.touch_start(pos);
    h.frame_after(0.05);
    h.touch_move(pos + egui::vec2(0.0, 150.0));
    let dragged = h.frame_after(FRAME_DT);

    assert_eq!(dragged.modality, PointerModality::Touch);
    assert_ne!(
        dragged.resolved_zoom, before,
        "the touch gesture must reach the map through the gate"
    );
    assert!(
        dragged.resolved.suppress_pan,
        "the zoom drag owns the pointer"
    );
}

/// 12. **A gesture interrupted by a modality change is abandoned, and stays
///     abandoned when the modality comes back.**
#[test]
fn a_touch_gesture_interrupted_by_a_mouse_does_not_resume_when_touch_returns() {
    let mut h = InputHarness::new();
    let stale = h.map_center();

    let tapped = h.touch_tap(stale);
    assert_eq!(tapped.modality, PointerModality::Touch);
    assert_eq!(
        tapped.resolved.overlay_click_pos, None,
        "precondition: the tap is pending, not yet confirmed"
    );

    let elsewhere = stale + egui::vec2(200.0, 0.0);
    h.mouse_move(elsewhere);
    let switched = h.frame_after(FRAME_DT);
    assert_eq!(switched.modality, PointerModality::Mouse);
    assert_eq!(
        switched.resolved.overlay_click_pos, None,
        "nothing should fire while the mouse is in charge"
    );

    h.frames_for(5, 0.2);

    h.touch_start(elsewhere);
    let resumed = h.frame_after(FRAME_DT);
    assert_eq!(
        resumed.modality,
        PointerModality::Touch,
        "precondition: touch is driving again, so the detector is polled"
    );
    assert_eq!(
        resumed.resolved.overlay_click_pos, None,
        "the stale tap must not be promoted when touch resumes"
    );

    let settled = h.frames_for(4, 0.2);
    assert_eq!(
        settled.resolved.overlay_click_pos, None,
        "and it must not surface on any later frame either"
    );
}

/// 13. **Only the active pane sees a touch; every pane sees the mouse.**
#[test]
fn a_touch_reaches_only_the_active_pane_but_a_click_reaches_them_all() {
    let mut h = InputHarness::new();
    h.set_pane_count(2);
    h.close_layers();
    let pos = h.pane_rects()[0].center();
    assert!(
        h.pane_rects().len() == 2 && !h.pane_rects()[1].contains(pos),
        "precondition: two distinct panes, and the click lands in pane 0"
    );

    let clicked = h.mouse_click(pos);
    assert_eq!(clicked.modality, PointerModality::Mouse);
    assert_eq!(
        clicked.resolved.overlay_click_pos,
        Some(pos),
        "precondition: the active pane got the click"
    );
    assert_eq!(
        clicked.resolved_inactive.map(|f| f.overlay_click_pos),
        Some(Some(pos)),
        "a mouse click is resolved for every pane, not just the active one"
    );

    let mut h = InputHarness::new();
    h.set_pane_count(2);
    h.close_layers();

    let tapped = h.touch_tap(pos);
    assert_eq!(tapped.modality, PointerModality::Touch);
    assert_eq!(
        tapped.mouse.overlay_click_pos,
        Some(pos),
        "precondition: on this frame the mouse path does resolve a click, \
             so `None` below is the touch branch and not an empty frame"
    );
    assert_eq!(
        tapped.resolved_inactive.map(|f| f.overlay_click_pos),
        Some(None),
        "an inactive pane takes no part in a touch gesture"
    );

    let confirmed = h.frame_after(AFTER_DOUBLE_TAP_TIMEOUT);
    assert_eq!(
        confirmed.resolved.overlay_click_pos,
        Some(pos),
        "the tap was deferred, not swallowed"
    );
}

/// A click can only hand the active-pane slot to a pane that exists.
#[test]
fn a_click_on_a_cell_no_pane_occupies_leaves_the_active_pane_alone() {
    let mut h = InputHarness::new();
    h.set_pane_count(2);
    h.claim_pane_count(4);
    let panel = h.map_panel_rect();

    let ghost = crate::pane::PaneLayout::for_count(
        4,
        crate::ui_layout::WidthClass::Expanded,
        crate::pane::SplitOrientation::Auto,
    )
    .pane_rect(3, panel)
    .center();
    assert!(
        h.pane_rects().iter().all(|r| !r.contains(ghost)),
        "precondition: the click lands outside every pane the frame drew"
    );

    h.mouse_click(ghost);

    assert_eq!(h.active_pane_index(), 0);
    assert_eq!(
        h.gui_mut().active_pane().site(),
        "KTLX",
        "the slot still resolves to a pane rather than panicking"
    );
}

/// 14. **Crossing a breakpoint must not move any widget's egui `Id`.**
#[test]
fn crossing_a_breakpoint_does_not_move_any_widget_id() {
    let mut h = InputHarness::with_screen(egui::vec2(1200.0, 500.0));
    // **A stack the user has filled from the catalog.** A curated stack
    // starts at the handful of layers that ship enabled, and this test's
    // subject is a LONG list - the panel's scroll, its clamped height, the
    // ids a scrolled body keeps. Built the way a user builds one, rather
    // than relied on as a property of the build's layer count.
    h.fill_stack();
    h.set_drawer_open(true);
    h.gui_mut().open_settings();
    h.warm_up();

    assert_eq!(
        h.width_class(),
        crate::ui_layout::WidthClass::Expanded,
        "precondition: start above the sidebar breakpoint"
    );
    let expanded = h.widget_id_probes();
    assert!(
        !expanded.is_empty(),
        "precondition: the panel must have reported some ids, or this test \
             compares two empty lists and passes for free"
    );
    assert!(
        expanded.iter().any(|(name, _)| *name == "inspector_scroll"),
        "precondition: the open inspector must report its scroll id, so the \
             comparison really covers the inspector's ids too"
    );

    let combo_id = expanded
        .iter()
        .find(|(name, _)| *name == "time_step_sel")
        .expect("precondition: the time step combo must report an id")
        .1;
    assert!(
        h.widget_exists(combo_id),
        "the time_step_sel probe reported an id egui has no widget for, so \
             it is a reconstruction rather than the combo box's own"
    );

    let scroll_id = expanded
        .iter()
        .find(|(name, _)| *name == "layers_scroll")
        .expect("precondition: the scroll area must report an id")
        .1;
    h.scroll_at(egui::pos2(80.0, 400.0), egui::vec2(0.0, -120.0));
    h.frames_for(3, FRAME_DT);
    let scrolled = h.scroll_offset(scroll_id);
    assert!(
        scrolled.is_some_and(|o| o.y > 0.0),
        "precondition: the layers panel must have actually scrolled under \
             the probed id, got {scrolled:?}"
    );

    h.set_screen(egui::vec2(800.0, 500.0));
    h.set_drawer_open(true);
    assert_eq!(
        h.width_class(),
        crate::ui_layout::WidthClass::Medium,
        "precondition: the resize really did cross the 1000pt breakpoint"
    );
    let medium = h.widget_id_probes();

    assert_eq!(
        expanded, medium,
        "a widget id moved with the layout: everything egui remembers under \
             it — scroll offset, combo state — is silently discarded on resize"
    );
    assert_eq!(
        h.scroll_offset(scroll_id),
        scrolled,
        "the scroll position must survive the resize"
    );

    h.set_screen(egui::vec2(500.0, 500.0));
    h.set_drawer_open(true);
    assert_eq!(
        h.width_class(),
        crate::ui_layout::WidthClass::Compact,
        "precondition: the resize crossed the 600pt breakpoint"
    );
    let compact_probes = h.widget_id_probes();
    assert!(
        compact_probes
            .iter()
            .any(|(name, _)| *name == "inspector_scroll"),
        "precondition: the sheet's Inspector page must be up and reporting"
    );
    for probe in &compact_probes {
        assert!(
            expanded.contains(probe),
            "{:?} resolved a different id inside the sheet than in the \
             floating hosts — the host switch re-keyed it",
            probe.0
        );
    }

    h.close_inspector();
    let restored = h
        .widget_id_probes()
        .iter()
        .find(|(name, _)| *name == "layers_scroll")
        .expect("the sheet's Layers page must report the stack's scroll id")
        .1;
    assert_eq!(
        restored, scroll_id,
        "the sheet's Layers page keys its scroll area on a different id, so \
         everything egui remembered under the old one is orphaned"
    );
    assert_eq!(
        h.scroll_offset(scroll_id),
        scrolled,
        "the scroll position did not survive the 600pt host switch"
    );
}

/// 16b. **The map is full-bleed: the content rect minus the top bar, exactly, at
/// every breakpoint — and every floating surface sits inside it.**
#[test]
fn the_map_is_full_bleed_under_the_top_bar() {
    for (size, expected) in [
        (
            egui::vec2(420.0, 800.0),
            crate::ui_layout::WidthClass::Compact,
        ),
        (
            egui::vec2(800.0, 800.0),
            crate::ui_layout::WidthClass::Medium,
        ),
        (
            egui::vec2(1400.0, 900.0),
            crate::ui_layout::WidthClass::Expanded,
        ),
    ] {
        let mut h = InputHarness::with_screen(size);
        assert_eq!(
            h.width_class(),
            expected,
            "precondition: {size:?} should be {expected:?}"
        );

        for insets in [(0.0, 0.0, 0.0, 0.0), (24.0, 16.0, 6.0, 6.0)] {
            let (top, bottom, left, right) = insets;
            h.set_safe_area_insets(top, bottom, left, right);
            let content = egui::Rect::from_min_max(
                egui::pos2(left, top),
                egui::pos2(size.x - right, size.y - bottom),
            );

            for drawer in [false, true] {
                h.set_drawer_open(drawer);
                let panel = h.map_panel_rect();
                let top_bar = h.top_bar().rect;
                let expected_panel = egui::Rect::from_min_max(
                    egui::pos2(content.left(), top_bar.bottom()),
                    content.right_bottom(),
                );
                assert_eq!(
                    panel, expected_panel,
                    "{expected:?} (drawer={drawer}, insets={insets:?}): the \
                     map is not exactly the content rect minus the top bar"
                );

                let mut floating = vec![("timeline", h.timeline().rect)];
                if expected == crate::ui_layout::WidthClass::Compact {
                    assert_eq!(
                        h.status_bar().rect,
                        egui::Rect::NOTHING,
                        "the phone shell drew a status bar it does not have"
                    );
                    floating.push(("bottom bar", h.bottom_bar().rect));
                    if let Some(sheet) = h.sheet_rect() {
                        floating.push(("sheet", sheet));
                    }
                } else {
                    floating.push(("status bar", h.status_bar().rect));
                }
                for (name, rect) in floating {
                    assert!(
                        panel.contains_rect(rect),
                        "{expected:?} (drawer={drawer}, insets={insets:?}): \
                         the {name} at {rect:?} is not inside the map {panel:?}"
                    );
                }
                if expected == crate::ui_layout::WidthClass::Compact {
                    assert!(
                        h.timeline().rect.bottom() <= h.bottom_bar().rect.top(),
                        "{expected:?} (drawer={drawer}, insets={insets:?}): \
                         the inline timeline at {:?} runs into the bottom bar \
                         at {:?}",
                        h.timeline().rect,
                        h.bottom_bar().rect
                    );
                }
                if h.layers_panel_on_screen() {
                    let layers = h
                        .layers_panel_rect()
                        .expect("the panel is on screen, so its area has a rect");
                    assert!(
                        panel.contains_rect(layers),
                        "{expected:?} (drawer={drawer}, insets={insets:?}): \
                         the layers panel at {layers:?} is not inside the \
                         map {panel:?}"
                    );
                }
            }

            h.set_drawer_open(false);
            let closed = h.map_panel_rect();
            h.set_drawer_open(true);
            assert_eq!(
                closed,
                h.map_panel_rect(),
                "{expected:?} (insets={insets:?}): opening the layers panel \
                 resized the map — it has started claiming panel space again"
            );
        }
    }
}

/// A compact harness with the menu open — the sheet's Menu page since the phone
/// shell: `open_menu` routes through the bottom bar's Menu item down here, and the
/// leaves come off `render_menu_drawer` over the same model the ☰ dropdown renders
/// on the wide widths.
fn compact_with_menu() -> InputHarness {
    let mut h = InputHarness::with_screen(egui::vec2(420.0, 1200.0));
    assert_eq!(
        h.width_class(),
        crate::ui_layout::WidthClass::Compact,
        "precondition: the fixture is about the narrowest width class"
    );
    h.open_menu();
    h
}

/// A compact harness with the layers drawer open — the narrow form of the layers
/// panel, for tests about the controls it hosts.
fn compact_with_layers_drawer() -> InputHarness {
    let mut h = InputHarness::with_screen(egui::vec2(420.0, 1200.0));
    assert_eq!(
        h.width_class(),
        crate::ui_layout::WidthClass::Compact,
        "precondition: the drawer presentation only exists below 600pt"
    );
    h.set_drawer_open(true);
    h
}

/// The rect of a drawn menu leaf, checked to be somewhere a click can actually
/// reach it.
fn clickable_leaf(h: &InputHarness, label: &str) -> egui::Rect {
    let leaf = h
        .menu_leaf(label)
        .unwrap_or_else(|| panic!("the menu did not draw {label:?}"));
    assert!(
        h.screen_rect().contains(leaf.rect.center()),
        "{label:?} was laid out at {:?}, outside the {:?} viewport — a \
             click there hits nothing and would pass for the wrong reason",
        leaf.rect,
        h.screen_rect()
    );
    leaf.rect
}

/// 17. **The dropdown's checkboxes show the live pane's state.**
#[test]
fn the_dropdown_checkboxes_show_the_live_pane_not_a_default_one() {
    let mut h = compact_with_menu();
    h.gui_mut().enable_overlay_for_test(&known::RADAR_SITES);
    h.warm_up();

    assert!(
        h.overlay_enabled(&known::RADAR_SITES),
        "precondition: the live pane must really have the overlay on"
    );

    let drawn = h.menu_leaf("Show radar sites").expect(
        "precondition: the open dropdown must draw the overlay toggles, \
         or there is no checkbox to be wrong about",
    );
    assert_eq!(
        drawn.value,
        Some(true),
        "the dropdown drew the checkbox from a default pane, not the live \
         one: it renders unchecked and every click turns the overlay *on*",
    );

    assert_eq!(
        h.menu_leaf("Auto-poll").map(|l| l.value),
        Some(Some(true)),
        "precondition: auto-poll defaults on and reads off `self`, so it \
             was never affected by the pane being taken"
    );
}

/// 18. **A checkbox in the dropdown turns the overlay off, and it stays off.**
#[test]
fn clicking_a_dropdown_checkbox_toggles_the_overlay_both_ways() {
    let mut h = compact_with_menu();
    h.gui_mut().enable_overlay_for_test(&known::RADAR_SITES);
    h.warm_up();
    assert!(h.overlay_enabled(&known::RADAR_SITES), "precondition");

    h.mouse_click(clickable_leaf(&h, "Show radar sites").center());
    assert!(
        !h.overlay_enabled(&known::RADAR_SITES),
        "clicking a checked box left the overlay on — the dropdown can turn \
             an overlay on but never off"
    );

    assert_eq!(
        h.menu_leaf("Show radar sites").map(|l| l.value),
        Some(Some(true)),
        "the probe recorded the post-click value, so it can no longer show \
             a checkbox being drawn from the wrong state"
    );
    for frame in 0..5 {
        h.frame_after(FRAME_DT);
        assert!(
            !h.overlay_enabled(&known::RADAR_SITES),
            "the overlay came back on {} frame(s) after the click: the \
                 toggle reached `enabled_overlays` but not `overlay_configs`, \
                 so the layers panel reloaded it from the config and undid it",
            frame + 1
        );
    }

    h.mouse_click(clickable_leaf(&h, "Show radar sites").center());
    h.frames_for(5, FRAME_DT);
    assert!(
        h.overlay_enabled(&known::RADAR_SITES),
        "the toggle did not come back on"
    );

    assert_eq!(
        h.menu_leaf("Show radar sites").map(|l| l.value),
        Some(Some(true)),
        "the pane is on but the dropdown still draws the box unchecked"
    );
}

/// A compact dropdown harness split into two panes, with pane 1 made active the way
/// a user does it — by tapping that pane on the map.
fn compact_menu_with_pane_1_active() -> InputHarness {
    let mut h = InputHarness::with_screen(egui::vec2(420.0, 1200.0));
    h.set_pane_count(2);

    let target = h.pane_rects()[1].center();
    h.mouse_click(target);
    h.warm_up();
    assert_eq!(
        h.active_pane_index(),
        1,
        "precondition: tapping pane 1 must make it active, or this fixture \
             is testing pane 0 twice"
    );

    h.open_menu();
    h
}

/// 27. **"The live active pane" means the active one, not pane 0.**
#[test]
fn the_menu_reads_and_writes_the_active_pane_not_pane_zero() {
    let mut h = compact_menu_with_pane_1_active();
    h.set_layer_links(false);

    h.set_overlay_on_pane(0, &known::RADAR_SITES, false);
    h.set_overlay_on_pane(0, &known::CITY_LABELS, false);
    h.set_overlay_on_pane(1, &known::RADAR_SITES, true);
    h.set_overlay_on_pane(1, &known::CITY_LABELS, true);
    h.warm_up();
    assert!(
        h.overlay_enabled_on(1, &known::RADAR_SITES)
            && !h.overlay_enabled_on(0, &known::RADAR_SITES)
            && h.overlay_enabled_on(1, &known::CITY_LABELS)
            && !h.overlay_enabled_on(0, &known::CITY_LABELS),
        "precondition: the panes must disagree about both kinds"
    );

    assert_eq!(
        h.menu_leaf("Show radar sites").map(|l| l.value),
        Some(Some(true)),
        "the drawer drew pane 0's state while pane 1 is active"
    );

    h.mouse_click(clickable_leaf(&h, "Show radar sites").center());
    h.frames_for(5, FRAME_DT);
    assert!(
        !h.overlay_enabled_on(1, &known::RADAR_SITES),
        "the toggle did not reach the active pane"
    );
    assert!(
        !h.overlay_enabled_on(0, &known::RADAR_SITES),
        "the toggle wrote to pane 0, which is not the active pane"
    );

    assert!(
        h.overlay_enabled_on(1, &known::CITY_LABELS),
        "toggling radar sites on pane 1 also turned its city labels off: \
             the config was read from pane 0, which had them off"
    );
    assert!(
        !h.overlay_enabled_on(0, &known::CITY_LABELS),
        "pane 0's city labels changed, though it is not the active pane"
    );
}

/// 29. **A menu toggle saves the active pane's *own* overlay config.**
#[test]
fn a_menu_toggle_loads_the_active_panes_config_before_saving_it() {
    let mut h = InputHarness::with_screen(egui::vec2(800.0, 900.0));
    assert_eq!(h.width_class(), crate::ui_layout::WidthClass::Medium);
    h.set_pane_count(2);
    h.set_layer_links(false);
    assert_eq!(
        h.active_pane_index(),
        0,
        "precondition: pane 0 active, so the *last drawn* pane 1 is the one \
             whose config is left in the handlers"
    );

    h.set_overlay_on_pane(0, &known::CITY_LABELS, true);
    h.set_overlay_on_pane(1, &known::CITY_LABELS, false);
    h.set_overlay_on_pane(0, &known::RADAR_SITES, false);
    h.warm_up();
    assert!(
        !h.layers_panel_on_screen(),
        "precondition: no layers panel, or its reload masks this"
    );

    h.open_menu();
    h.mouse_click(clickable_leaf(&h, "Show radar sites").center());
    h.frames_for(5, FRAME_DT);

    assert!(
        h.overlay_enabled_on(0, &known::RADAR_SITES),
        "precondition: the toggle must have taken effect"
    );
    assert!(
        h.overlay_enabled_on(0, &known::CITY_LABELS),
        "the active pane's city labels were overwritten by pane 1's config: \
             the handlers were saved without loading the active pane first"
    );
}

/// 28. **A menu toggle propagates to the other panes when sync is on.**
#[test]
fn a_menu_toggle_propagates_to_the_other_panes_when_sync_is_on() {
    let mut h = InputHarness::with_screen(egui::vec2(800.0, 900.0));
    assert_eq!(
        h.width_class(),
        crate::ui_layout::WidthClass::Medium,
        "precondition: Medium keeps the layers panel closed by default"
    );
    h.set_pane_count(2);
    h.mouse_click(h.pane_rects()[1].center());
    h.warm_up();
    assert_eq!(h.active_pane_index(), 1, "precondition: pane 1 is active");

    assert!(
        h.all_layer_linked(),
        "precondition: every pane's layer link is on by default"
    );
    assert!(
        !h.layers_panel_on_screen(),
        "precondition: the layers panel must NOT be on screen, or its own \
             `propagate_pane_sync` masks the arm under test"
    );

    h.set_overlay_on_pane(0, &known::RADAR_SITES, false);
    h.set_overlay_on_pane(1, &known::RADAR_SITES, false);
    h.warm_up();

    h.open_menu();
    h.mouse_click(clickable_leaf(&h, "Show radar sites").center());
    h.frames_for(5, FRAME_DT);

    assert!(
        h.overlay_enabled_on(1, &known::RADAR_SITES),
        "precondition: the active pane must have taken the toggle"
    );
    assert!(
        h.overlay_enabled_on(0, &known::RADAR_SITES),
        "the toggle did not propagate to the other pane, though layer sync \
             is on"
    );
}

/// 76. **The menu carries the whole model at every width — the ☰ dropdown on the
///     wide widths, the sheet's Menu page on the phone.**
#[test]
fn the_app_menu_dropdown_carries_the_whole_menu_at_every_width() {
    for (size, expected) in [
        (
            egui::vec2(420.0, 1200.0),
            crate::ui_layout::WidthClass::Compact,
        ),
        (
            egui::vec2(800.0, 1200.0),
            crate::ui_layout::WidthClass::Medium,
        ),
        (
            egui::vec2(1400.0, 900.0),
            crate::ui_layout::WidthClass::Expanded,
        ),
    ] {
        let mut h = InputHarness::with_screen(size);
        assert_eq!(h.width_class(), expected, "precondition: {size:?}");
        assert!(
            h.menu_leaves().is_empty(),
            "{expected:?}: the menu drew itself before the \u{2630} button \
             was ever clicked"
        );

        h.open_menu();
        if expected == crate::ui_layout::WidthClass::Compact {
            assert_eq!(
                h.sheet().page,
                Some(crate::ui::SheetPage::Menu),
                "the phone menu must be the sheet's Menu page"
            );
        }
        let drawn: Vec<&str> = h.menu_leaves().iter().map(|l| l.label).collect();
        for wanted in h.menu_leaf_labels() {
            let leaf = h.menu_leaf(wanted).unwrap_or_else(|| {
                panic!(
                    "{expected:?}: the dropdown never drew {wanted:?} — \
                     drew {drawn:?}"
                )
            });
            assert!(
                h.screen_rect().contains(leaf.rect.center()),
                "{expected:?}: {wanted:?} was drawn at {:?}, outside the \
                 viewport {:?}",
                leaf.rect,
                h.screen_rect()
            );
        }
    }
}

/// 20. **Invoking a command from the dropdown really dispatches it.**
#[test]
fn a_command_invoked_from_the_dropdown_reaches_the_dispatcher() {
    let mut h = compact_with_menu();
    let exit = clickable_leaf(&h, "Exit");

    h.mouse_click(exit.center());
    assert!(
        h.last_actions()
            .iter()
            .any(|a| matches!(a, crate::actions::GuiAction::Exit)),
        "clicking Exit in the dropdown emitted no Exit action ({} actions in all)",
        h.last_actions().len()
    );
}

/// 21. **The dropdown's events reach the dispatcher on a desktop too.**
#[test]
fn a_toggle_flipped_in_the_dropdown_reaches_the_dispatcher() {
    let mut h = InputHarness::with_screen(egui::vec2(1200.0, 800.0));
    assert_eq!(
        h.width_class(),
        crate::ui_layout::WidthClass::Expanded,
        "precondition: the widest class, with the sidebar up"
    );
    h.gui_mut().enable_overlay_for_test(&known::RADAR_SITES);
    h.warm_up();
    assert!(h.overlay_enabled(&known::RADAR_SITES), "precondition");

    h.open_menu();
    assert_eq!(
        h.menu_leaf("Show radar sites").map(|l| l.value),
        Some(Some(true)),
        "the open dropdown must draw the toggle, from the live pane"
    );

    h.mouse_click(clickable_leaf(&h, "Show radar sites").center());
    h.frames_for(5, FRAME_DT);
    assert!(
        !h.overlay_enabled(&known::RADAR_SITES),
        "the dropdown's toggle never reached apply_menu_event, or was \
             reverted by the layers panel on a later frame"
    );
}

/// 22. **The pane picker narrows on a phone; the config clamp does not.**
#[test]
fn the_pane_picker_offers_fewer_panes_on_a_phone_than_on_a_desktop() {
    use squallar_device_profile::budget::{MAX_PANES_DESKTOP, MAX_PANES_MOBILE};

    let enabled_counts = |h: &InputHarness| -> Vec<usize> {
        h.pane_options()
            .iter()
            .filter(|o| o.enabled)
            .map(|o| o.count)
            .collect()
    };

    let mut compact = InputHarness::with_screen(egui::vec2(420.0, 1200.0));
    assert_eq!(
        compact.width_class(),
        crate::ui_layout::WidthClass::Compact,
        "precondition"
    );
    assert!(
        compact.pane_option_counts().is_empty(),
        "the phone top bar drew pane segments it should not carry"
    );
    compact.open_layers();
    assert_eq!(
        compact.pane_option_counts(),
        (1..=MAX_PANES_DESKTOP).collect::<Vec<_>>(),
        "the full row must be drawn — the counts past the offer read as \
         disabled, not as absent"
    );
    assert_eq!(
        enabled_counts(&compact),
        (1..=MAX_PANES_MOBILE).collect::<Vec<_>>(),
        "the picker offered the desktop range enabled on a phone"
    );

    let mut expanded = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    assert_eq!(
        expanded.width_class(),
        crate::ui_layout::WidthClass::Expanded,
        "precondition"
    );
    assert_eq!(
        enabled_counts(&expanded),
        (1..=MAX_PANES_DESKTOP).collect::<Vec<_>>(),
        "the picker narrowed a desktop to the phone range"
    );

    assert!(
        enabled_counts(&compact).len() < enabled_counts(&expanded).len(),
        "precondition: the two ranges must differ, or both assertions above \
             are satisfied by one constant"
    );

    assert_eq!(
        compact.top_bar().pane_count_max,
        enabled_counts(&compact).len(),
        "the compact probe's pane_count_max disagrees with the enabled \
         buttons on screen"
    );
    assert_eq!(
        expanded.top_bar().pane_count_max,
        enabled_counts(&expanded).len(),
        "the expanded probe's pane_count_max disagrees with the enabled \
         buttons on screen"
    );

    let six = compact
        .pane_options()
        .iter()
        .find(|o| o.count == MAX_PANES_DESKTOP)
        .expect("the full row includes the absolute maximum")
        .rect;
    compact.mouse_click(six.center());
    compact.warm_up();
    assert!(
        compact.pane_count() <= MAX_PANES_MOBILE,
        "clicking a disabled pane-count button split the layout anyway"
    );

    let selected: Vec<usize> = expanded
        .pane_options()
        .iter()
        .filter(|o| o.selected)
        .map(|o| o.count)
        .collect();
    assert_eq!(
        selected,
        vec![expanded.pane_count()],
        "the picker's selected button must be the live pane count"
    );

    let three = expanded
        .pane_options()
        .iter()
        .find(|o| o.count == 3)
        .expect("the desktop range must include 3")
        .rect;
    assert_ne!(expanded.pane_count(), 3, "precondition");
    expanded.mouse_click(three.center());
    expanded.warm_up();
    assert_eq!(
        expanded.pane_count(),
        3,
        "clicking a pane-count button did not change the layout"
    );
    assert_eq!(
        expanded.pane_rects().len(),
        3,
        "the map still laid out the old number of panes"
    );
}

/// 77. **The Layers toggle hides and restores the Expanded sidebar with its state
///     intact.**
#[test]
fn the_layers_toggle_hides_and_restores_the_expanded_sidebar_with_its_state() {
    let mut h = InputHarness::with_screen(egui::vec2(1200.0, 500.0));
    // **A stack the user has filled from the catalog.** A curated stack
    // starts at the handful of layers that ship enabled, and this test's
    // subject is a LONG list - the panel's scroll, its clamped height, the
    // ids a scrolled body keeps. Built the way a user builds one, rather
    // than relied on as a property of the build's layer count.
    h.fill_stack();
    assert_eq!(
        h.width_class(),
        crate::ui_layout::WidthClass::Expanded,
        "precondition: the width with a persistent sidebar"
    );
    assert!(
        h.layers_panel_on_screen(),
        "precondition: the sidebar is the shell default on Expanded"
    );

    let scroll_id = h
        .widget_id_probes()
        .iter()
        .find(|(name, _)| *name == "layers_scroll")
        .expect("precondition: the panel must report its scroll id")
        .1;
    h.scroll_at(egui::pos2(80.0, 400.0), egui::vec2(0.0, -120.0));
    h.frames_for(3, FRAME_DT);
    let scrolled = h.scroll_offset(scroll_id);
    assert!(
        scrolled.is_some_and(|o| o.y > 0.0),
        "precondition: the panel must really have scrolled, got {scrolled:?}"
    );

    let (toggle, open) = h.top_bar().layers_toggle;
    assert!(open, "the toggle must read as open while the panel shows");
    h.mouse_click(toggle.center());
    h.warm_up();
    assert!(
        !h.layers_panel_on_screen(),
        "clicking the Layers toggle did not hide the persistent sidebar"
    );
    assert!(
        !h.widget_id_probes()
            .iter()
            .any(|(name, _)| *name == "layers_scroll" || *name == "product_sel"),
        "the panel is gone but still reported widget ids, so something \
         of it is still rendering (the timeline's own probes remain — it \
         is a separate surface and stays up)"
    );
    assert!(
        !h.top_bar().layers_toggle.1,
        "the toggle still reads as open with the panel hidden"
    );

    h.mouse_click(h.top_bar().layers_toggle.0.center());
    h.warm_up();
    assert!(
        h.layers_panel_on_screen(),
        "a second click did not bring the sidebar back"
    );
    let restored_id = h
        .widget_id_probes()
        .iter()
        .find(|(name, _)| *name == "layers_scroll")
        .expect("the restored panel must report its scroll id")
        .1;
    assert_eq!(
        restored_id, scroll_id,
        "the restored panel keys its scroll area on a different id, so \
         everything egui remembered under the old one is orphaned"
    );
    assert_eq!(
        h.scroll_offset(scroll_id),
        scrolled,
        "the scroll position did not survive the round trip"
    );
}

/// 78. **An explicit sidebar choice neither leaks into the drawer nor expires at
///     the breakpoint.**
#[test]
fn an_explicit_sidebar_choice_survives_the_breakpoint_without_leaking() {
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    assert_eq!(h.width_class(), crate::ui_layout::WidthClass::Expanded);
    assert!(
        h.layers_panel_on_screen(),
        "precondition: the shell default"
    );

    h.mouse_click(h.top_bar().layers_toggle.0.center());
    h.warm_up();
    assert!(!h.layers_panel_on_screen(), "precondition: closed by hand");

    h.set_screen(egui::vec2(800.0, 900.0));
    assert_eq!(
        h.width_class(),
        crate::ui_layout::WidthClass::Medium,
        "precondition: crossed below the sidebar breakpoint"
    );
    assert!(
        !h.layers_panel_on_screen(),
        "the drawer must start closed on Medium"
    );

    h.mouse_click(h.top_bar().layers_toggle.0.center());
    h.warm_up();
    assert!(
        h.layers_panel_on_screen(),
        "the sidebar's explicit close leaked into the drawer: the toggle \
         could not open it on Medium"
    );
    h.mouse_click(h.top_bar().layers_toggle.0.center());
    h.warm_up();
    assert!(
        !h.layers_panel_on_screen(),
        "the drawer did not close again"
    );

    h.set_screen(egui::vec2(1400.0, 900.0));
    assert_eq!(h.width_class(), crate::ui_layout::WidthClass::Expanded);
    assert!(
        !h.layers_panel_on_screen(),
        "crossing the breakpoint and back reopened a sidebar the user \
         explicitly closed"
    );

    h.mouse_click(h.top_bar().layers_toggle.0.center());
    h.warm_up();
    assert!(
        h.layers_panel_on_screen(),
        "the toggle could not reopen the sidebar after the round trip"
    );
}

/// 79. **A fresh session opens the layers panel only where it is persistent.**
#[test]
fn a_fresh_session_opens_the_sidebar_only_where_it_is_persistent() {
    for (size, expected, open) in [
        (
            egui::vec2(1400.0, 900.0),
            crate::ui_layout::WidthClass::Expanded,
            true,
        ),
        (
            egui::vec2(800.0, 900.0),
            crate::ui_layout::WidthClass::Medium,
            false,
        ),
        (
            egui::vec2(420.0, 900.0),
            crate::ui_layout::WidthClass::Compact,
            false,
        ),
    ] {
        let h = InputHarness::with_screen(size);
        assert_eq!(h.width_class(), expected, "precondition: {size:?}");
        assert_eq!(
            h.layers_panel_on_screen(),
            open,
            "{expected:?}: fresh state must show the panel only where the \
             sidebar is persistent"
        );
        let toggle_open = if expected == crate::ui_layout::WidthClass::Compact {
            assert!(
                h.bottom_bar().layers.0.is_positive(),
                "{expected:?}: the phone shell drew no bottom-bar Layers item"
            );
            h.bottom_bar().layers.1
        } else {
            h.top_bar().layers_toggle.1
        };
        assert_eq!(
            toggle_open, open,
            "{expected:?}: the toggle's drawn state disagrees with the \
             panel it controls"
        );
    }
}

/// 80. **The bar's arm toggle arms and disarms through real clicks.**
#[test]
fn the_bars_arm_toggle_arms_and_disarms_through_real_clicks() {
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    let (section, on) = h.top_bar().section_arm;
    assert!(!on, "precondition: nothing armed in a fresh session");

    h.mouse_click(section.center());
    h.warm_up();
    assert!(
        h.section_draw_armed(),
        "clicking the bar's X-sec toggle did not arm the draw"
    );
    assert!(
        h.top_bar().section_arm.1,
        "the draw is armed but the toggle does not show it"
    );

    h.mouse_click(h.top_bar().section_arm.0.center());
    h.warm_up();
    assert!(
        !h.section_draw_armed(),
        "a second click on the armed toggle did not disarm"
    );
    assert!(
        !h.top_bar().section_arm.1,
        "the draw is disarmed but the toggle still shows it armed"
    );

    let region = h.top_bar().region_arm;
    assert!(!region.1, "precondition: the region pick is not armed");
    h.mouse_click(region.0.center());
    h.warm_up();
    assert!(
        h.region_pick_armed(),
        "clicking the bar's Region toggle did not arm the pick"
    );
    assert!(
        h.top_bar().region_arm.1,
        "the pick is armed but the toggle does not show it"
    );

    h.mouse_click(h.top_bar().section_arm.0.center());
    h.warm_up();
    assert!(
        h.section_draw_armed() && !h.region_pick_armed(),
        "arming the draw left the region pick armed - one press on one map pane \
         would be both a line and a box, through one shared detector"
    );
    assert!(
        h.top_bar().section_arm.1 && !h.top_bar().region_arm.1,
        "the bar shows {:?} for the two arms, which is not the state the Gui is in",
        (h.top_bar().section_arm.1, h.top_bar().region_arm.1),
    );

    h.mouse_click(h.top_bar().region_arm.0.center());
    h.warm_up();
    assert!(
        h.region_pick_armed() && !h.section_draw_armed(),
        "arming the region pick left the cross-section draw armed"
    );
    assert!(
        h.top_bar().region_arm.1 && !h.top_bar().section_arm.1,
        "the bar shows {:?} for the two arms after the reverse swap",
        (h.top_bar().section_arm.1, h.top_bar().region_arm.1),
    );
}

/// The phone bar carries **both** arm icons, and each one works.
#[test]
fn the_phone_bar_carries_both_arms_and_they_still_exclude_each_other() {
    let mut h = InputHarness::with_screen(egui::vec2(420.0, 1200.0));
    assert_eq!(
        h.width_class(),
        crate::ui_layout::WidthClass::Compact,
        "precondition: this must be the phone bar, not the wide one",
    );

    let region = h.top_bar().region_arm;
    assert!(
        region.0.is_positive(),
        "the phone bar drew no Region arm, so a phone user has no route to the \
         one control that buys resolution",
    );
    h.mouse_click(region.0.center());
    h.warm_up();
    assert!(
        h.region_pick_armed(),
        "clicking the phone bar's Region arm did not arm the pick",
    );
    assert!(
        h.top_bar().region_arm.1,
        "the pick is armed but the phone bar's icon does not show it",
    );

    h.mouse_click(h.top_bar().section_arm.0.center());
    h.warm_up();
    assert!(
        h.section_draw_armed() && !h.region_pick_armed(),
        "the phone bar armed the draw and left the pick armed - one press on one \
         map pane would be both a line and a box",
    );
    assert!(
        !h.top_bar().region_arm.1,
        "the phone bar still lights the Region arm for a mode that is off",
    );

    h.mouse_click(h.top_bar().section_arm.0.center());
    h.warm_up();
    assert!(
        !h.section_draw_armed() && !h.region_pick_armed(),
        "a second click on the phone bar's X-sec arm did not disarm it",
    );
}

/// 81. **Arming from the bar closes the open ☰ dropdown.**
#[test]
fn arming_from_the_bar_closes_the_open_dropdown() {
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    h.open_menu();
    assert!(
        !h.menu_leaves().is_empty(),
        "precondition: the dropdown is open"
    );

    let (section, _) = h.top_bar().section_arm;
    assert!(
        !h.is_floating_layer_at(section.center()),
        "precondition: the toggle must not sit under the open popup, or \
         the click below lands on the popup instead"
    );

    h.mouse_click(section.center());
    h.frame_after(FRAME_DT);
    assert!(
        h.menu_leaves().is_empty(),
        "the dropdown stayed open over the armed drag"
    );
    assert!(
        h.section_draw_armed(),
        "closing the dropdown ate the click that was also an arm"
    );
}

/// 82. **The bar never overlaps itself at Medium's narrowest width.**
#[test]
fn the_bar_never_overlaps_at_mediums_narrowest_width() {
    let mut h = InputHarness::with_screen(egui::vec2(600.0, 900.0));
    assert_eq!(
        h.width_class(),
        crate::ui_layout::WidthClass::Medium,
        "precondition: 600pt is Medium's floor"
    );
    h.set_pane_count(squallar_device_profile::budget::MAX_PANES_DESKTOP);
    assert_eq!(
        h.pane_count(),
        squallar_device_profile::budget::MAX_PANES_DESKTOP,
        "precondition: the widest segment row the bar can be asked for"
    );

    assert!(
        h.painted_text_strings()
            .iter()
            .all(|t| t != "Panes:" && t != "Pane:"),
        "the bar kept its roomy captions at a width they cannot fit"
    );
    assert!(
        h.painted_text_strings().iter().any(|t| t == "Panes")
            && h.painted_text_strings().iter().any(|t| t == "Pane"),
        "the tight bar dropped its compact captions - two unlabeled number \
         runs are the second user test's exact finding"
    );
    let wide = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    assert!(
        wide.painted_text_strings().iter().any(|t| t == "Panes:"),
        "a 1400pt bar dropped its captions, so the adaptive decision is \
         stuck tight and the assertion above says nothing"
    );

    let probe = h.top_bar();
    let bar = probe.rect.expand(0.5);
    let arms = [probe.section_arm.0, probe.region_arm.0];
    for (name, rect) in [
        ("the \u{2630} button", probe.menu_button),
        ("the Layers toggle", probe.layers_toggle.0),
        ("the X-sec toggle", probe.section_arm.0),
        ("the Region toggle", probe.region_arm.0),
    ] {
        assert!(
            bar.contains_rect(rect),
            "{name} at {rect:?} leaked out of the bar {bar:?}"
        );
    }

    for option in h.pane_options() {
        assert!(
            bar.contains_rect(option.rect),
            "pane-count button {} at {:?} leaked out of the bar {bar:?}",
            option.count,
            option.rect
        );
        for arm in arms {
            assert!(
                !option.rect.intersects(arm),
                "pane-count button {} at {:?} lies under the arm toggle at \
                 {arm:?}",
                option.count,
                option.rect
            );
        }
    }

    for (rect, text) in h.painted_text_rects() {
        if !probe.rect.intersects(rect) || arms.iter().any(|a| a.contains(rect.center())) {
            continue;
        }
        for arm in arms {
            assert!(
                !rect.intersects(arm),
                "{text:?} at {rect:?} was painted under the arm toggle at \
                 {arm:?}"
            );
        }
    }

    h.mouse_click(probe.section_arm.0.center());
    h.warm_up();
    assert!(
        h.section_draw_armed(),
        "the X-sec toggle stopped arming at the squeezed width"
    );
    h.mouse_click(h.top_bar().section_arm.0.center());
    h.warm_up();
    assert!(
        !h.section_draw_armed(),
        "the X-sec toggle stopped disarming at the squeezed width"
    );
}

/// 83. **A dismiss with the ☰ dropdown open closes it, and only it.**
#[test]
fn a_dismiss_with_the_dropdown_open_closes_it_and_only_it() {
    let mut h = InputHarness::with_screen(egui::vec2(800.0, 1200.0));
    assert_eq!(
        h.width_class(),
        crate::ui_layout::WidthClass::Medium,
        "precondition: a width where the drawer is the layer beneath"
    );
    h.set_drawer_open(true);
    h.open_menu();
    assert!(
        h.layers_panel_on_screen() && !h.menu_leaves().is_empty(),
        "precondition: drawer and dropdown both open"
    );

    assert!(
        h.gui_mut().dismiss_top_layer(),
        "the press must be consumed against the open dropdown"
    );
    h.key_press(egui::Key::Escape);
    h.warm_up();
    assert!(
        h.menu_leaves().is_empty(),
        "the dropdown survived the press"
    );
    assert!(
        h.layers_panel_on_screen(),
        "one press took the drawer under the popup with it — egui's own \
         close and the consumed dismiss are not converging on one layer"
    );

    assert!(h.gui_mut().dismiss_top_layer(), "the drawer was still open");
    h.warm_up();
    assert!(
        !h.layers_panel_on_screen(),
        "the second press did not close the drawer"
    );
}

/// 83b. **Android's back closes the menu with no key event at all.**
#[test]
fn an_android_back_press_closes_the_dropdown_without_a_key_event() {
    let mut h = compact_with_menu();
    assert!(
        !h.menu_leaves().is_empty(),
        "precondition: the dropdown is open"
    );

    assert!(
        h.gui_mut().dismiss_top_layer(),
        "the press must be consumed against the open dropdown"
    );
    h.frames_for(2, FRAME_DT);
    assert!(
        h.menu_leaves().is_empty(),
        "the popup stayed open behind a back press egui never saw"
    );

    assert!(
        !h.gui_mut().dismiss_top_layer(),
        "the popup press left something else consumed as well"
    );
}

/// A two-pane Expanded harness with pane 1 made active the user's way — a click on
/// that pane — so the stack and inspector demonstrably describe the pane the user
/// is working in, not pane 0.
fn expanded_with_pane_1_active() -> InputHarness {
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    h.set_pane_count(2);
    let target = h.pane_rects()[1].center();
    h.mouse_click(target);
    h.warm_up();
    assert_eq!(
        h.active_pane_index(),
        1,
        "precondition: clicking pane 1 must make it active, or this fixture \
             is testing pane 0 twice"
    );
    h
}

/// 84. **The stack's rows read and write the live active pane, not pane 0.**
#[test]
fn the_stacks_rows_read_and_write_the_active_pane_not_pane_zero() {
    let mut h = expanded_with_pane_1_active();
    h.set_layer_links(false);
    h.set_overlay_on_pane(0, &known::RADAR_SITES, false);
    h.set_overlay_on_pane(0, &known::CITY_LABELS, false);
    h.set_overlay_on_pane(1, &known::RADAR_SITES, true);
    h.set_overlay_on_pane(1, &known::CITY_LABELS, true);
    h.warm_up();

    let row = h
        .stack_row(&known::RADAR_SITES)
        .expect("the stack must draw a RadarSites row");
    assert!(
        row.eye_on,
        "the eye drew pane 0's state while pane 1 is active"
    );

    h.mouse_click(row.eye.center());
    h.frames_for(5, FRAME_DT);
    assert!(
        !h.overlay_enabled_on(1, &known::RADAR_SITES),
        "the eye did not reach the active pane"
    );
    assert!(
        !h.overlay_enabled_on(0, &known::RADAR_SITES),
        "the eye wrote to pane 0, which is not the active pane"
    );
    assert!(
        h.overlay_enabled_on(1, &known::CITY_LABELS),
        "toggling radar sites on pane 1 also turned its city labels off: \
             the config was read from the wrong pane"
    );
    assert!(
        !h.overlay_enabled_on(0, &known::CITY_LABELS),
        "pane 0's city labels changed, though it is not the active pane"
    );
}

/// 85. **The eye turns a layer off, and it stays off — and back on.**
#[test]
fn the_eye_toggles_a_layer_both_ways_and_it_sticks() {
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    h.gui_mut().enable_overlay_for_test(&known::RADAR_SITES);
    h.warm_up();
    assert!(h.overlay_enabled(&known::RADAR_SITES), "precondition");

    let row = h.stack_row(&known::RADAR_SITES).expect("row drawn");
    assert!(row.eye_on, "the eye must draw the live state");
    h.mouse_click(row.eye.center());
    for frame in 0..5 {
        h.frame_after(FRAME_DT);
        assert!(
            !h.overlay_enabled(&known::RADAR_SITES),
            "the overlay came back on {} frame(s) after the eye click: the \
                 toggle reached `enabled_overlays` but not `overlay_configs`",
            frame + 1
        );
    }
    assert!(
        !h.stack_row(&known::RADAR_SITES).expect("row").eye_on,
        "the layer is off but the eye still draws it on"
    );

    let row = h.stack_row(&known::RADAR_SITES).expect("row");
    h.mouse_click(row.eye.center());
    h.frames_for(5, FRAME_DT);
    assert!(
        h.overlay_enabled(&known::RADAR_SITES),
        "the eye did not turn the layer back on"
    );
}

/// 86. **The layer body carries no master toggle: the stack row's eye owns
///     visibility.**
#[test]
fn the_layer_body_carries_no_master_toggle() {
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    h.gui_mut().enable_overlay_for_test(&known::RADAR_SITES);
    h.warm_up();

    for kind in [known::RADAR_SITES, known::COLOR_SCALE, known::RADAR] {
        h.open_layer_in_inspector(&kind);
        let rect = h.inspector_rect().expect("the inspector is open");
        let name = h.overlay_display_name(&kind).to_owned();
        assert!(
            !h.text_painted_in(rect, &format!("Show {name}")),
            "{kind:?}'s layer body drew a \"Show {name}\" master toggle - \
             the stack row's eye owns visibility"
        );
    }

    let rect = h.inspector_rect().expect("the Radar body is open");
    assert!(
        h.text_painted_in(rect, "Pane properties..."),
        "the Radar body must point at Pane properties rather than stand empty"
    );
    let button = h
        .painted_text_rects()
        .into_iter()
        .find(|(r, text)| text == "Pane properties..." && rect.contains(r.center()))
        .expect("the button was just painted")
        .0;
    h.mouse_click(button.center());
    h.warm_up();
    assert_eq!(
        h.inspector().mode,
        Some(crate::ui::InspectorSelection::PaneProps),
        "the Radar body's button must open Pane properties"
    );
}

/// 87. **An eye toggle saves the active pane's *own* overlay config.**
#[test]
fn an_eye_toggle_loads_the_active_panes_config_before_saving_it() {
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    h.set_pane_count(2);
    h.set_layer_links(false);
    assert_eq!(
        h.active_pane_index(),
        0,
        "precondition: pane 0 active, so the *last drawn* pane 1 is the one \
             whose config could be left in the handlers"
    );

    h.set_overlay_on_pane(0, &known::CITY_LABELS, true);
    h.set_overlay_on_pane(1, &known::CITY_LABELS, false);
    // The layer ships disabled, so a curated stack does not hold it: it is
    // added first, then hidden, which is the "in the stack, eye off" state the
    // toggle below needs a row for.
    h.add_layer_to_pane(0, &known::RADAR_SITES);
    h.set_overlay_on_pane(0, &known::RADAR_SITES, false);
    h.warm_up();

    let row = h.stack_row(&known::RADAR_SITES).expect("row drawn");
    h.mouse_click(row.eye.center());
    h.frames_for(5, FRAME_DT);

    assert!(
        h.overlay_enabled_on(0, &known::RADAR_SITES),
        "precondition: the eye must have taken effect"
    );
    assert!(
        h.overlay_enabled_on(0, &known::CITY_LABELS),
        "the active pane's city labels were overwritten by pane 1's config: \
             the handlers were saved without loading the active pane first"
    );
}

/// 88. **An eye toggle propagates over the layer-link fan-out mask — linked source
///     to linked targets; an unlinked target is untouched; an unlinked source stays
///     local.**
#[test]
fn an_eye_toggle_propagates_over_the_layer_link_mask() {
    let mut h = expanded_with_pane_1_active();
    assert!(
        h.all_layer_linked(),
        "precondition: every pane's layer link defaults on"
    );
    // The layer ships disabled, so neither curated stack holds it: both panes
    // are given it, then both are hidden - the "in the stack, eye off" state
    // on each end of the link that the fan-out below is about.
    h.add_layer_to_pane(0, &known::RADAR_SITES);
    h.add_layer_to_pane(1, &known::RADAR_SITES);
    h.set_overlay_on_pane(0, &known::RADAR_SITES, false);
    h.set_overlay_on_pane(1, &known::RADAR_SITES, false);
    h.warm_up();

    let row = h.stack_row(&known::RADAR_SITES).expect("row drawn");
    h.mouse_click(row.eye.center());
    h.frames_for(5, FRAME_DT);
    assert!(
        h.overlay_enabled_on(1, &known::RADAR_SITES),
        "precondition: the active pane must have taken the toggle"
    );
    assert!(
        h.overlay_enabled_on(0, &known::RADAR_SITES),
        "the toggle did not propagate to the linked pane, though both ends \
             are linked"
    );

    h.gui_mut().pane_mut(0).expect("pane 0").layer_link = false;
    h.warm_up();
    let row = h.stack_row(&known::RADAR_SITES).expect("row drawn");
    h.mouse_click(row.eye.center());
    h.frames_for(5, FRAME_DT);
    assert!(
        !h.overlay_enabled_on(1, &known::RADAR_SITES),
        "precondition: the active pane must have taken the toggle off"
    );
    assert!(
        h.overlay_enabled_on(0, &known::RADAR_SITES),
        "the toggle reached a layer-unlinked target pane"
    );

    {
        let gui = h.gui_mut();
        gui.pane_mut(0).expect("pane 0").layer_link = true;
        gui.pane_mut(1).expect("pane 1").layer_link = false;
    }
    h.set_overlay_on_pane(0, &known::CITY_LABELS, true);
    h.set_overlay_on_pane(1, &known::CITY_LABELS, false);
    h.warm_up();
    assert!(
        h.overlay_enabled_on(0, &known::CITY_LABELS)
            && !h.overlay_enabled_on(1, &known::CITY_LABELS),
        "precondition: the panes must disagree about the witness kind, or \
             local-stays-local is unobservable"
    );
    let row = h.stack_row(&known::RADAR_SITES).expect("row drawn");
    h.mouse_click(row.eye.center());
    h.frames_for(5, FRAME_DT);
    assert!(
        h.overlay_enabled_on(1, &known::RADAR_SITES),
        "precondition: the unlinked active pane must have taken its own \
             toggle"
    );
    assert!(
        h.overlay_enabled_on(0, &known::CITY_LABELS),
        "an unlinked source pane propagated: pane 0's city labels were \
             overwritten with pane 1's, though pane 1's layer link is off"
    );
}

/// 94. **Turning a dataless layer on fetches it.**
#[test]
fn enabling_a_dataless_layer_fetches_it() {
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    // Outlooks ship disabled, so a curated stack does not hold one: the layer
    // is added and hidden, which is the state this test's subject - the eye
    // turning a *dataless* layer on - needs a row in.
    h.add_layer_to_pane(0, &known::SPC_OUTLOOK);
    h.set_overlay_on_pane(0, &known::SPC_OUTLOOK, false);
    let row = h.stack_row(&known::SPC_OUTLOOK).expect("row drawn");
    assert!(!row.eye_on, "precondition: the row is drawn hidden");
    h.mouse_click(row.eye.center());
    assert!(
        h.last_actions().iter().any(|a| matches!(
            a,
            crate::actions::GuiAction::FetchOverlay { kind, .. }
                if *kind == known::SPC_OUTLOOK
        )),
        "the eye enabled a layer with no data and no auto-poll, and nothing \
             will ever fetch it"
    );
}

/// Drag one stack row's grip from its own centre to `to`, through the real
/// press-move-release sequence the drag machinery sees.
fn drag_stack_row(h: &mut InputHarness, kind: &LayerId, to: egui::Pos2) {
    let handle = h
        .stack_row(kind)
        .unwrap_or_else(|| panic!("{kind:?}'s row is drawn"))
        .handle;
    assert_ne!(handle, egui::Rect::NOTHING, "{kind:?}'s row has a grip");
    let from = handle.center();
    h.mouse_press(from);
    h.frame_after(FRAME_DT);
    h.mouse_move(egui::pos2(from.x, (from.y + to.y) / 2.0));
    h.frame_after(FRAME_DT);
    h.mouse_move(to);
    h.frame_after(FRAME_DT);
    h.mouse_release(to);
    h.frames_for(2, FRAME_DT);
}

/// [`drag_stack_row`] by touch: the same grip, through the egui-winit touch
/// sequence — whose *release* frame batches `PointerButton{up}` with `PointerGone`
/// (the harness's event-fidelity table), the pair that clears egui's `latest_pos`
/// before the drag resolver runs.
fn touch_drag_stack_row(h: &mut InputHarness, kind: &LayerId, to: egui::Pos2) {
    let handle = h
        .stack_row(kind)
        .unwrap_or_else(|| panic!("{kind:?}'s row is drawn"))
        .handle;
    assert_ne!(handle, egui::Rect::NOTHING, "{kind:?}'s row has a grip");
    let from = handle.center();
    h.touch_start(from);
    h.frame_after(FRAME_DT);
    h.touch_move(egui::pos2(from.x, (from.y + to.y) / 2.0));
    h.frame_after(FRAME_DT);
    h.touch_move(to);
    h.frame_after(FRAME_DT);
    h.touch_end(to);
    h.frames_for(2, FRAME_DT);
}

/// 68c. **The same grip drag lands by touch.**
#[test]
fn a_touch_drag_on_the_grip_lands_the_reorder() {
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    let before = h.gui_mut().pane(0).expect("pane 0").draw_order_vec();
    let n = before.len();
    assert!(n >= 2, "precondition: a real layer list");
    let rows = h.stack().rows;
    let second = rows[1].kind.clone();
    let above_top = egui::pos2(rows[0].rect.center().x, rows[0].rect.top() - 4.0);

    touch_drag_stack_row(&mut h, &second, above_top);

    let mut expected = before.clone();
    expected.swap(n - 1, n - 2);
    assert_eq!(
        h.gui_mut().pane(0).expect("pane 0").draw_order_vec(),
        expected,
        "the touch release must land the reorder - a resolver that reads \
         latest_pos loses the landing position to the release batch's \
         PointerGone and springs the row back"
    );
    assert_eq!(
        h.stack().rows[0].kind,
        second,
        "the touch-promoted layer's row did not move to the top"
    );
}

/// 68. **Dragging a row by its grip really reorders the draw order — permuted,
///     persisted, redrawn.**
#[test]
fn dragging_a_row_by_its_grip_permutes_the_draw_order_and_it_persists() {
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    let before = h.gui_mut().pane(0).expect("pane 0").draw_order_vec();
    let n = before.len();
    assert!(n >= 3, "precondition: a real layer list");
    let rows = h.stack().rows;
    assert_eq!(rows.len(), n, "one row per layer");

    let second = rows[1].kind.clone();
    let above_top = egui::pos2(rows[0].rect.center().x, rows[0].rect.top() - 4.0);
    drag_stack_row(&mut h, &second, above_top);
    let mut expected = before.clone();
    expected.swap(n - 1, n - 2);
    assert_eq!(
        h.gui_mut().pane(0).expect("pane 0").draw_order_vec(),
        expected,
        "dropping the second row above the top one must swap the draw \
         order's last two entries"
    );

    let rows = h.stack().rows;
    assert_eq!(
        rows[0].kind, second,
        "the promoted layer's row did not move to the top"
    );

    let below_second = egui::pos2(rows[1].rect.center().x, rows[1].rect.bottom() - 2.0);
    drag_stack_row(&mut h, &second, below_second);
    assert_eq!(
        h.gui_mut().pane(0).expect("pane 0").draw_order_vec(),
        before,
        "dragging the promoted row back down must put the order back"
    );

    let top = h.stack().rows[0].kind.clone();
    let below_last = {
        let rows = h.stack().rows;
        egui::pos2(rows[n - 1].rect.center().x, rows[n - 1].rect.bottom() + 4.0)
    };
    drag_stack_row(&mut h, &top, below_last);
    assert_eq!(
        h.gui_mut().pane(0).expect("pane 0").draw_order_vec()[0],
        top,
        "a drop below the last row must make the layer the first drawn"
    );

    let reordered = h.gui_mut().pane(0).expect("pane 0").draw_order_vec();
    assert_ne!(reordered, before, "precondition: a real reorder to persist");
    let store = squallar_kv::MemoryKvStore::default();
    h.gui_mut().save_ui_config(&store);
    let mut fresh = crate::Gui::new();
    assert!(fresh.load_ui_config(&store), "the saved config must load");
    assert_eq!(
        fresh.pane(0).expect("pane 0").draw_order_vec(),
        reordered,
        "the reorder did not survive the ui_config round trip"
    );
}

/// 68b. **The user's exact case: City Labels dragged above the Color Scale changes
/// what paints over what.**
#[test]
fn city_labels_dragged_above_the_color_scale_paint_after_it() {
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    h.gui_mut().enable_overlay_for_test(&known::CITY_LABELS);
    h.gui_mut().enable_overlay_for_test(&known::COLOR_SCALE);
    h.warm_up();

    let paint_pos = |h: &InputHarness, kind: &LayerId| -> usize {
        h.paint_order(0)
            .iter()
            .position(|(k, _)| k == kind)
            .unwrap_or_else(|| panic!("{kind:?} was not dispatched"))
    };
    let labels_before = paint_pos(&h, &known::CITY_LABELS);
    let scale_before = paint_pos(&h, &known::COLOR_SCALE);
    assert!(
        labels_before < scale_before,
        "precondition: the default draw order paints the scale over the labels"
    );

    let scale_row = h
        .stack_row(&known::COLOR_SCALE)
        .expect("the Color Scale row is drawn");
    let above_scale = egui::pos2(scale_row.rect.center().x, scale_row.rect.top() - 4.0);
    drag_stack_row(&mut h, &known::CITY_LABELS, above_scale);
    h.warm_up();

    assert!(
        paint_pos(&h, &known::CITY_LABELS) > paint_pos(&h, &known::COLOR_SCALE),
        "City Labels moved above the Color Scale in the stack, but the pane \
         still paints them under it - the reorder changed nothing, which is \
         the second user test's exact report"
    );
}

/// 95. **The pane paints every enabled kind at its `draw_order` position, on one
///     paint list.**
#[test]
fn the_pane_paints_every_enabled_kind_in_draw_order_on_one_paint_list() {
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    h.gui_mut().enable_overlay_for_test(&known::CITY_LABELS);
    h.gui_mut().enable_overlay_for_test(&known::COLOR_SCALE);
    h.gui_mut().enable_overlay_for_test(&known::RADAR_SITES);
    h.warm_up();

    let expected: Vec<LayerId> = {
        let order = h.gui_mut().pane(0).expect("pane 0").draw_order_vec();
        order
            .into_iter()
            .filter(|kind| h.overlay_enabled_on(0, kind))
            .collect()
    };
    assert!(
        expected.contains(&known::CITY_LABELS) && expected.contains(&known::COLOR_SCALE),
        "precondition: the two kinds of the user's case are enabled and ordered"
    );

    let order = h.paint_order(0);
    assert!(!order.is_empty(), "the map pane recorded no paint order");
    let kinds: Vec<LayerId> = order.iter().map(|(kind, _)| kind.clone()).collect();
    assert_eq!(
        kinds, expected,
        "the pane's paint sequence is not its enabled draw_order"
    );

    let (first_kind, first_layer) = order[0].clone();
    for (kind, layer) in &order {
        assert_eq!(
            *layer, first_layer,
            "{kind:?} paints on its own layer while {first_kind:?} paints on \
             the pane's - their stacking is then egui's hash-order layer \
             drain, not draw_order"
        );
    }
}

/// **A texture layer with a landed raster paints a quad the layer-off scene
/// does not.**
///
/// The complement of
/// [`the_pane_paints_every_enabled_kind_in_draw_order_on_one_paint_list`],
/// which reads the pane's *dispatch* record: a layer holding no texture is in
/// that record too, so the paint order alone cannot say anything reached the
/// screen. This counts the textured quads the pane actually painted, and the
/// middle reading is what makes it a statement about the raster rather than
/// about enabling the layer — enabling alone must not be what adds the quad.
///
/// Ported from the deleted `fake-source` acceptance suite, which was the only
/// place this leg existed, and re-pointed at a real layer.
#[test]
fn a_texture_layer_paints_a_quad_only_once_its_raster_has_landed() {
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    h.warm_up();
    let pane_rect = h.pane_rects()[0];

    // The alert layer is on by default, so the layer-off reading is taken by
    // switching it off rather than by assuming it starts there.
    h.gui_mut()
        .set_overlay_on_pane_for_test(0, &known::NWS_ALERTS, false);
    h.warm_up();
    assert!(
        !h.overlay_enabled_on(0, &known::NWS_ALERTS),
        "fixture: the layer did not switch off, so the reading below is not a \
         layer-off reading",
    );
    assert!(
        !h.paint_order(0)
            .iter()
            .any(|(id, _)| *id == known::NWS_ALERTS),
        "the layer is off and the pane still painted it",
    );
    let off = h.painted_images_in(pane_rect).len();

    h.gui_mut().enable_overlay_for_test(&known::NWS_ALERTS);
    h.warm_up();
    assert!(
        h.paint_order(0)
            .iter()
            .any(|(id, _)| *id == known::NWS_ALERTS),
        "the layer is enabled and the pane's paint order does not contain it",
    );
    let enabled_no_raster = h.painted_images_in(pane_rect).len();

    // The raster lands: real data in, then the cache stood up exactly as the
    // frame's own render request asked for it.
    let ground = h.ground_at(0, pane_rect.center());
    ingest_alerts(
        &mut h,
        vec![alert_over("a", "Tornado Warning", ground.y(), ground.x())],
    );
    h.warm_up();
    settle_overlay_cache(&mut h, &known::NWS_ALERTS);
    h.warm_up();
    let with_raster = h.painted_images_in(pane_rect).len();

    assert!(
        with_raster > enabled_no_raster,
        "the layer's overlay cache holds a texture and the pane painted no \
         more textured quads than it did holding none ({enabled_no_raster} \
         then {with_raster}) - the layer is in the draw order and nothing \
         draws its raster",
    );
    assert_eq!(
        enabled_no_raster, off,
        "enabling the layer painted a quad before any raster landed \
         ({off} then {enabled_no_raster}), so the count above cannot \
         distinguish a landed raster from a toggled checkbox",
    );
}

/// 89. **A stack row click selects that layer in the inspector, which opens
///     itself.**
#[test]
fn a_stack_row_click_opens_that_layers_options_in_the_inspector() {
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    assert!(
        !h.inspector().open,
        "precondition: the inspector starts closed"
    );

    let row = h.stack_row(&known::NWS_ALERTS).expect("row drawn");
    h.mouse_click(row.rect.center());
    h.warm_up();

    let inspector = h.inspector();
    assert!(inspector.open, "the row click did not open the inspector");
    assert_eq!(
        inspector.mode,
        Some(crate::ui::InspectorSelection::Layer(known::NWS_ALERTS)),
        "the inspector opened on something other than the clicked layer"
    );
    assert_eq!(
        inspector.crumb, "Pane 1 \u{203a} NWS Alerts",
        "the crumb does not name the selection"
    );
    assert!(
        h.stack_row(&known::NWS_ALERTS)
            .expect("row still drawn")
            .selected,
        "the selected layer's row must draw selected"
    );

    // The crumb is the way back up, one level per segment — what the deleted
    // \u{d7}-deselect used to do in one jump, now spelled where it reads.
    let (text, seg) = inspector
        .crumb_links
        .first()
        .cloned()
        .expect("the layer body's crumb must offer its Pane N segment");
    assert_eq!(text, "Pane 1", "the crumb's link segment names the pane");
    h.mouse_click(seg.center());
    h.warm_up();
    let inspector = h.inspector();
    assert!(inspector.open, "navigating must not close the inspector");
    assert_eq!(
        inspector.mode,
        Some(crate::ui::InspectorSelection::PaneProps),
        "the layer body's Pane N segment must go one level up, to the pane's properties"
    );

    let (_, seg) = inspector
        .crumb_links
        .first()
        .cloned()
        .expect("the pane-props body's crumb must offer its Pane N segment too");
    h.mouse_click(seg.center());
    h.warm_up();
    let inspector = h.inspector();
    assert!(inspector.open, "navigating must not close the inspector");
    assert_eq!(
        inspector.mode,
        Some(crate::ui::InspectorSelection::AppSettings),
        "the pane-props body's Pane N segment must reach the root, App \u{203a} Settings"
    );
    assert!(
        inspector.crumb_links.is_empty(),
        "App \u{203a} Settings is the root: it has nowhere to navigate to"
    );

    h.mouse_click(inspector.close.center());
    h.warm_up();
    assert!(!h.inspector().open, "\u{d7} must close the inspector");
}

/// 89b. **Closing the inspector never rewrites what you come back to** — every
///     close route, at both hosts, leaves the selection where the user left it.
///
/// A back press used to reset `inspector_sel` to App › Settings, and so did
/// `clear_sheet_pages` (the sheet's ×, the bar items' second tap, and the
/// map-click fade at every width), while the crumb's own close kept it. The
/// same panel therefore reopened on two different bodies depending on which
/// control shut it. There is one close now (W1) and the distinction that
/// justified the reset went with it.
#[test]
fn closing_the_inspector_preserves_the_selection_on_every_route() {
    let layer = crate::ui::InspectorSelection::Layer(known::NWS_ALERTS);

    // Reopen the wide inspector without naming a body: the ⚙ toggle is the one
    // route that only flips the panel's visibility.
    fn reopen(h: &mut InputHarness) -> Option<crate::ui::InspectorSelection> {
        let toggle = h.top_bar().inspector_toggle;
        assert!(!toggle.1, "precondition: the inspector is shut");
        h.mouse_click(toggle.0.center());
        h.warm_up();
        assert!(h.inspector().open, "the ⚙ toggle did not reopen the panel");
        h.inspector().mode
    }

    // Route 1: the back press, wide branch.
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    h.open_layer_in_inspector(&known::NWS_ALERTS);
    assert_eq!(
        h.inspector().mode,
        Some(layer.clone()),
        "precondition: the layer body is up"
    );
    assert!(h.gui_mut().dismiss_top_layer(), "the press closes it");
    h.warm_up();
    assert!(!h.inspector().open, "precondition: it really closed");
    assert_eq!(
        reopen(&mut h),
        Some(layer.clone()),
        "the back press rewrote the selection"
    );

    // Route 2: the crumb's own ×, which never reset it and still must not.
    let close = h.inspector().close;
    h.mouse_click(close.center());
    h.warm_up();
    assert!(!h.inspector().open, "precondition: the × closed it");
    assert_eq!(
        reopen(&mut h),
        Some(layer.clone()),
        "the × lost the selection"
    );

    // Route 3: `clear_sheet_pages`, reached at this width by the map-click
    // fade — the route that closes every panel in state, not just in paint.
    let spot = h.map_center();
    h.mouse_click(spot);
    h.warm_up();
    if !h.faded() {
        h.mouse_click(spot);
        h.warm_up();
    }
    assert!(h.faded(), "precondition: the bare-map tap faded the chrome");
    assert!(
        !h.inspector().open,
        "precondition: the fade closed the panel for real"
    );
    assert!(h.gui_mut().dismiss_top_layer(), "the press unfades");
    h.warm_up();
    assert_eq!(
        reopen(&mut h),
        Some(layer.clone()),
        "the fade's close rewrote the selection"
    );

    // Route 4: the Compact branch of the back press, and the sheet's own ×.
    // The phone offers no reopen that does not also name a body, so the
    // selection itself is the instrument here — see
    // `Gui::inspector_selection_for_test`.
    /// One close route: what to call it, and how to perform it.
    type Route = (&'static str, fn(&mut InputHarness));
    let routes: [Route; 2] = [
        ("the phone back press", |h| {
            assert!(h.gui_mut().dismiss_top_layer(), "back pops the page");
        }),
        ("the sheet's ×", |h| {
            let close = h.sheet().close;
            assert!(close.is_positive(), "the sheet draws its own ×");
            h.mouse_click(close.center());
        }),
    ];
    for (name, close) in routes {
        let mut h = phone();
        h.open_layer_in_inspector(&known::NWS_ALERTS);
        assert_eq!(
            h.sheet().page,
            Some(crate::ui::SheetPage::Inspector),
            "precondition: the layer body is the sheet's page"
        );
        assert_eq!(h.inspector().mode, Some(layer.clone()));
        close(&mut h);
        h.warm_up();
        assert!(!h.inspector().open, "{name}: the panel did not close");
        assert_eq!(
            h.gui_mut().inspector_selection_for_test(),
            &layer,
            "{name} rewrote the selection"
        );
    }
}

/// 90. **The ⚙ toggle and the menu's Settings… entry both reach the settings
///     body.**
#[test]
fn the_inspector_toggle_and_the_settings_entry_reach_the_settings_body() {
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    let (toggle, open) = h.top_bar().inspector_toggle;
    assert!(!open, "precondition: the inspector starts closed");

    h.mouse_click(toggle.center());
    h.warm_up();
    let inspector = h.inspector();
    assert!(
        inspector.open,
        "the \u{2699} toggle did not open the inspector"
    );
    assert_eq!(
        inspector.mode,
        Some(crate::ui::InspectorSelection::AppSettings),
        "a fresh session's inspector must open on App \u{203a} Settings"
    );
    assert_eq!(inspector.crumb, "App \u{203a} Settings");
    assert!(
        h.top_bar().inspector_toggle.1,
        "the toggle must read as open while the panel shows"
    );
    let panel = h.inspector_rect().expect("the open inspector has a rect");
    assert!(
        h.map_panel_rect().contains_rect(panel),
        "the inspector at {panel:?} is not inside the map \
             {:?} — it must float over the map like every other surface",
        h.map_panel_rect()
    );

    h.mouse_click(h.top_bar().inspector_toggle.0.center());
    h.warm_up();
    assert!(
        !h.inspector().open,
        "a second \u{2699} click did not close the inspector"
    );

    h.open_settings();
    assert_eq!(
        h.inspector().mode,
        Some(crate::ui::InspectorSelection::AppSettings),
        "the menu's Settings\u{2026} entry did not land on the settings body"
    );
    assert!(
        h.settings_row("units.timezone").is_some(),
        "the settings body drew no rows"
    );
}

/// 91. **The double-render counter really counts.**
#[test]
fn the_control_pass_counter_counts_the_layer_body() {
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    assert_eq!(
        h.gui_mut().control_render_passes_for_test(),
        0,
        "no layer body is open, so no pass should have run"
    );

    h.open_layer_in_inspector(&known::NWS_ALERTS);
    assert_eq!(
        h.gui_mut().control_render_passes_for_test(),
        1,
        "the open layer body must count exactly one pass per frame"
    );

    h.close_inspector();
    assert_eq!(
        h.gui_mut().control_render_passes_for_test(),
        0,
        "the closed inspector still ran a control pass"
    );
}

/// 92. **The auto-poll chip's off state reads `⏸ Auto-poll off`, and its hover
///     names the way back.**
#[test]
fn the_auto_poll_chip_pins_its_off_text_and_hover() {
    let mut h = InputHarness::new();
    h.open_menu();
    h.mouse_click(clickable_leaf(&h, "Auto-poll").center());
    h.close_menu();
    h.warm_up();

    let (chip, text) = h
        .status_bar()
        .poll_chip
        .expect("the chip must be drawn while nothing is fetching");
    assert_eq!(
        text, "\u{23f8} Auto-poll off",
        "the chip's off state must say so"
    );

    h.mouse_move(chip.center());
    h.frames_for(12, 0.1);
    assert!(
        h.painted_text_strings()
            .iter()
            .any(|t| t.contains("Toggle auto-poll from the \u{2630} menu")),
        "hovering the off chip must say where the toggle lives; painted: {:?}",
        h.painted_text_strings()
    );
}

/// 93. **A shrunk window does not cap the stack forever.**
#[test]
fn the_stack_regains_its_height_after_a_shrink_and_regrow() {
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    // **A stack the user has filled from the catalog.** A curated stack
    // starts at the handful of layers that ship enabled, and this test's
    // subject is a LONG list - the panel's scroll, its clamped height, the
    // ids a scrolled body keeps. Built the way a user builds one, rather
    // than relied on as a property of the build's layer count.
    h.fill_stack();
    let before = h.stack().rect.height();
    assert!(
        before > 300.0,
        "precondition: the full-height stack must be tall, got {before}"
    );

    h.set_screen(egui::vec2(1400.0, 300.0));
    let shrunk = h.stack().rect.height();
    assert!(
        shrunk < before / 2.0,
        "precondition: the shrink must really clamp the stack, got {shrunk}"
    );

    h.set_screen(egui::vec2(1400.0, 900.0));
    let regrown = h.stack().rect.height();
    assert!(
        (regrown - before).abs() < 1.0,
        "the stack came back {regrown} tall after shrinking to {shrunk}; \
             it was {before} — the committed area size has become the ceiling \
             again"
    );
}

/// 23. **Host safe-area insets reach the chrome.**
#[test]
fn host_safe_area_insets_inset_the_chrome() {
    const TOP: f32 = 60.0;
    const BOTTOM: f32 = 40.0;
    const LEFT: f32 = 30.0;
    const RIGHT: f32 = 20.0;

    let mut h = InputHarness::with_screen(egui::vec2(1200.0, 900.0));
    let bare = h.map_panel_rect();

    h.set_safe_area_insets(TOP, BOTTOM, LEFT, RIGHT);
    let inset = h.map_panel_rect();

    assert_eq!(inset.left() - bare.left(), LEFT, "left inset ignored");
    assert_eq!(bare.right() - inset.right(), RIGHT, "right inset ignored");
    assert_eq!(inset.top() - bare.top(), TOP, "top inset ignored");
    assert_eq!(
        bare.bottom() - inset.bottom(),
        BOTTOM,
        "bottom inset ignored"
    );

    let mut h = InputHarness::with_screen(egui::vec2(420.0, 1000.0));
    assert_eq!(
        h.width_class(),
        crate::ui_layout::WidthClass::Compact,
        "precondition: the narrowest class gets the same docked bar"
    );
    let bare = h.top_bar().rect;
    h.set_safe_area_insets(TOP, 0.0, LEFT, 0.0);
    let inset = h.top_bar().rect;
    assert_eq!(
        (inset.left() - bare.left(), inset.top() - bare.top()),
        (LEFT, TOP),
        "the top bar ignored the insets and stayed under the system bars"
    );
}

/// 24. **Insets move the breakpoint, not just the padding.**
#[test]
fn host_insets_move_the_breakpoint_through_the_real_ui() {
    let mut h = InputHarness::with_screen(egui::vec2(610.0, 900.0));
    assert_eq!(
        h.width_class(),
        crate::ui_layout::WidthClass::Medium,
        "precondition: 610pt of raw viewport is Medium"
    );

    h.set_safe_area_insets(0.0, 0.0, 20.0, 20.0);
    assert_eq!(
        h.width_class(),
        crate::ui_layout::WidthClass::Compact,
        "570pt of content is Compact: the insets never reached the breakpoint"
    );
}

/// 25. **The hover readout follows the pointer, not the window width — and Compact
///     has none at all (M9-17's revision).**
#[test]
fn the_hover_readout_follows_the_modality_not_the_width() {
    let mut narrow = InputHarness::with_screen(egui::vec2(500.0, 800.0));
    narrow.mouse_click(narrow.map_center());
    assert_eq!(narrow.width_class(), crate::ui_layout::WidthClass::Compact);
    assert!(
        !narrow.top_bar().hover,
        "the Compact top bar still hosts the truncating readout M9-17 removed"
    );

    let mut wide = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    wide.mouse_click(wide.map_center());
    assert!(
        wide.status_bar().hover,
        "a wide mouse window lost its status-bar readout"
    );

    let mut tablet = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    tablet.touch_tap(tablet.map_center());
    assert_eq!(tablet.width_class(), crate::ui_layout::WidthClass::Expanded);
    assert!(
        !tablet.status_bar().hover && !tablet.top_bar().hover,
        "a touch device was given a hover readout that can never fill in"
    );

    let mut touch_phone = InputHarness::with_screen(egui::vec2(420.0, 900.0));
    touch_phone.touch_tap(touch_phone.map_center());
    assert_eq!(
        touch_phone.width_class(),
        crate::ui_layout::WidthClass::Compact
    );
    assert!(
        !touch_phone.top_bar().hover,
        "a touch phone's top bar drew a hover readout that can never fill in"
    );
}

/// M9-17. **On Compact a mouse press-and-hold raises the value popup.**
#[test]
fn a_compact_mouse_press_and_hold_raises_the_value_popup() {
    let mut h = InputHarness::with_screen(egui::vec2(420.0, 900.0));
    h.load_scan("KTLX");
    let spot = h.pane_rects()[0].center();
    h.place_radar_image(0, &radar_fields::known::REFLECTIVITY, 0.5, None, None, None);
    h.warm_up();

    h.mouse_press(spot);
    let pressed = h.frame_after(FRAME_DT);
    assert_eq!(
        pressed.resolved.long_press_pos, None,
        "not held long enough yet"
    );
    let held = h.frames_for(10, 0.1);
    assert_eq!(
        held.resolved.long_press_pos,
        Some(spot),
        "a Compact mouse hold must resolve as the long press"
    );
    assert!(
        h.painted_text_strings()
            .iter()
            .any(|t| t.contains("dBZ") || t == "No data"),
        "the hold did not raise the value popup; painted {:?}",
        h.painted_text_strings()
    );
    h.mouse_release(spot);
    h.warm_up();

    let mut wide = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    wide.load_scan("KTLX");
    let spot = wide.pane_rects()[0].center();
    wide.mouse_press(spot);
    let held = wide.frames_for(10, 0.1);
    assert_eq!(
        held.resolved.long_press_pos, None,
        "a wide mouse window must not grow the long-press (it steals the pan)"
    );
    wide.mouse_release(spot);
}

/// 26. **The phone top bar carries the short scan text; the long form stays on the
///     desktop status bar — and the Auto-poll toggle stays reachable through the menu
///     everywhere.**
#[test]
fn a_compact_status_bar_drops_the_long_summary_and_the_auto_poll_box() {
    let mut phone = InputHarness::with_screen(egui::vec2(420.0, 900.0));
    phone.load_scan("KABR");
    assert_eq!(phone.width_class(), crate::ui_layout::WidthClass::Compact);

    let mut desk = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    desk.load_scan("KABR");
    assert_eq!(desk.width_class(), crate::ui_layout::WidthClass::Expanded);
    let roomy_bar = desk.status_bar();

    assert_eq!(
        phone.status_bar().rect,
        egui::Rect::NOTHING,
        "the phone shell drew a status bar it does not have"
    );
    let (_, chip_text) = roomy_bar
        .poll_chip
        .clone()
        .expect("a desktop status bar lost its auto-poll chip");
    assert!(
        chip_text.contains("Auto-poll"),
        "the chip must name the state it shows, got {chip_text:?}"
    );

    for h in [&mut phone, &mut desk] {
        h.open_menu();
        assert_eq!(
            h.menu_leaf("Auto-poll").map(|l| l.value),
            Some(Some(true)),
            "the menu must carry the Auto-poll toggle, checked while on"
        );
        h.close_menu();
    }

    let scan_chip = phone.top_bar().scan_text;
    assert!(
        scan_chip.contains("KABR") && roomy_bar.scan_text.contains("KABR"),
        "precondition: both forms should name the site, got {scan_chip:?} \
         and {:?}",
        roomy_bar.scan_text
    );
    assert!(
        scan_chip.contains("\u{23fa}"),
        "the phone chip must carry the live/archive posture glyph: {scan_chip:?}"
    );
    assert!(
        phone.text_painted_in(phone.top_bar().rect, &scan_chip),
        "the chip's text must actually be painted in the top bar"
    );
    assert!(
        roomy_bar.scan_text.contains("2 products") && roomy_bar.scan_text.contains("2026-07-24"),
        "the roomy bar dropped the long scan summary: {:?}",
        roomy_bar.scan_text
    );
    assert!(
        !scan_chip.contains("products") && !scan_chip.contains("2026-07-24"),
        "the phone chip drew the long scan summary: {scan_chip:?}"
    );
}

/// Data collected `ago` before now.
fn written_ago(minutes: i64) -> chrono::NaiveDateTime {
    chrono::Utc::now().naive_utc() - chrono::Duration::seconds(minutes * 60 + 30)
}

/// 26b. **Every product says how old the data behind it is, the same way.**
#[test]
fn every_products_data_age_is_drawn_the_same_way_in_the_status_bar() {
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    h.load_scan("KTLX");
    assert_eq!(
        h.status_bar().product_age_text,
        None,
        "precondition: a pane with no render yet has no data time to report, \
             so the line below is not simply always there"
    );

    h.set_data_time(0, Some(written_ago(23)));

    let bar = h.status_bar();
    let drawn = bar
        .product_age_text
        .as_deref()
        .expect("a pane showing an image must report when its data was collected");
    assert!(
        drawn.starts_with("Data:") && drawn.contains("(23 min old)"),
        "the roomy bar should give the data time and its age, got {drawn:?}"
    );
    assert!(
        !drawn.contains("Level III") && !drawn.contains("L3"),
        "the line must not name a datasource: {drawn:?}"
    );
    assert!(
        h.text_painted_in(bar.rect, "23 min old"),
        "the age never reached the glass: nothing was painted inside the \
             status bar rect {:?}. Painted: {:?}",
        bar.rect,
        h.painted_text_strings()
    );
}

/// 26d. **A looping pane dates the frame it is playing, not the still it
/// replaced.**
#[test]
fn a_looping_pane_reports_its_current_frames_time() {
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    h.load_scan("KTLX");
    h.set_data_time(0, Some(written_ago(90)));

    let frame_time = written_ago(7);
    {
        let pane = h.gui_mut().pane_mut(0).unwrap();
        *pane.time_state_mut(&known::RADAR) = crate::radar_layer::begin_loop(
            600,
            squallar_radar::sites::get_radar_site("KTLX").unwrap(),
            squallar_radar::types::RenderView::PlanView,
        );
        pane.time_state_mut(&known::RADAR).frames = vec![crate::pane::LoopFrame {
            timestamp: frame_time,
            image: None,
            render_in_flight: false,
            render_failed: false,
        }];
        pane.park_on_frame(&known::RADAR, 0);
    }
    h.warm_up();

    let drawn = h
        .status_bar()
        .product_age_text
        .expect("a looping pane still reports a data time");
    assert!(
        drawn.contains("(7 min old)"),
        "the playing frame's own time must be reported, got {drawn:?}"
    );
    assert!(
        !drawn.contains("90 min old"),
        "the static render's time captioned the animation: {drawn:?}"
    );
}

/// 66. **Collapsing the transport leaves a restore chip at the map's bottom-right,
///     and the chip restores it.**
#[test]
fn collapsing_the_transport_leaves_a_chip_that_restores_it() {
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    h.warm_up();
    let before = h.timeline();
    assert!(
        !before.collapsed && before.live.0.is_positive(),
        "precondition: the transport starts expanded with row 1 drawn"
    );

    h.mouse_click(before.collapse.center());
    h.warm_up();
    let collapsed = h.timeline();
    assert!(collapsed.collapsed, "the \u{25be} button did not collapse");

    let map = h.map_panel_rect();
    assert!(
        map.contains_rect(collapsed.chip),
        "the chip at {:?} is outside the map {map:?}",
        collapsed.chip
    );
    assert!(
        map.right() - collapsed.chip.right() < 24.0 && collapsed.chip.center().x > map.center().x,
        "the chip at {:?} is not right-aligned in {map:?}",
        collapsed.chip
    );

    assert!(
        !collapsed.live.0.is_positive()
            && !collapsed.scrubber.is_positive()
            && !collapsed.step_dropdown.is_positive(),
        "row-1 widgets were still recorded while collapsed"
    );
    assert!(
        !h.painted_text_strings()
            .iter()
            .any(|t| t == "\u{23fa} Live"),
        "the Live button was still painted while collapsed"
    );

    h.mouse_click(collapsed.chip.center());
    h.warm_up();
    let restored = h.timeline();
    assert!(
        !restored.collapsed && restored.live.0.is_positive(),
        "clicking the chip did not restore the transport"
    );
}

/// 66b. **The status bar's ◧ collapses it to a restore button, left- anchored, and
/// the same button brings it back.**
#[test]
fn collapsing_the_status_bar_leaves_only_its_restore_button() {
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    h.load_scan("KTLX");
    let bar = h.status_bar();
    assert!(
        !bar.collapsed && bar.refresh.is_positive(),
        "precondition: the bar starts expanded with its content drawn"
    );

    h.mouse_click(bar.collapse.center());
    h.warm_up();
    let collapsed = h.status_bar();
    assert!(collapsed.collapsed, "the \u{25e7} button did not collapse");
    assert!(
        !collapsed.refresh.is_positive(),
        "the refresh button was still drawn while collapsed"
    );
    assert!(
        !h.text_painted_in(collapsed.rect, "Scan:"),
        "the scan summary was still painted while collapsed"
    );
    let map = h.map_panel_rect();
    assert!(
        collapsed.collapse.left() - map.left() < 24.0,
        "the restore button at {:?} is not left-anchored in {map:?}",
        collapsed.collapse
    );

    h.mouse_click(collapsed.collapse.center());
    h.warm_up();
    let restored = h.status_bar();
    assert!(
        !restored.collapsed && restored.refresh.is_positive(),
        "clicking the restore button did not bring the bar back"
    );
}

/// **A countdown on screen buys itself a frame a second, and nothing else buys one
/// at all.**
#[test]
fn the_status_bar_countdown_pays_for_its_own_frames_and_nothing_else_does() {
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    h.load_scan("KTLX");

    let chip = h
        .status_bar()
        .poll_chip
        .expect("precondition: the scan cleared the fetch, so the chip is up");
    assert!(
        chip.1.contains("archive "),
        "precondition: the chip is printing the archive countdown, got {:?}",
        chip.1
    );

    let tick = h
        .gui_mut()
        .status_tick_delay()
        .expect("a countdown on screen is owed the frame that advances it");
    assert!(
        !tick.is_zero() && tick <= std::time::Duration::from_secs(1),
        "the countdown asked for a {tick:?} wake: a second is how fast it \
             moves, and anything shorter is the repaint loop back again"
    );

    let collapse = h.status_bar().collapse;
    h.mouse_click(collapse.center());
    h.warm_up();
    assert!(
        h.status_bar().collapsed,
        "precondition: the bar collapsed to its restore button"
    );
    assert_eq!(
        h.gui_mut().status_tick_delay(),
        None,
        "a countdown nobody can see is still holding the event loop awake \
             once a second"
    );
}

/// The phone shell draws no status bar at all (plan §1.6), so it owes no frames to
/// a chip it never drew — the same claim as above, reached by the other route into
/// the absence.
#[test]
fn a_phone_owes_no_frames_to_a_status_bar_it_never_draws() {
    let mut h = InputHarness::with_screen(egui::vec2(400.0, 800.0));
    h.load_scan("KTLX");
    assert!(
        h.status_bar().poll_chip.is_none(),
        "precondition: Compact draws no status bar"
    );

    assert_eq!(h.gui_mut().status_tick_delay(), None);
}

/// **The timestamp chip opens the Set Time dialog** — the timeline's own route to
/// it; the menu's Time...
#[test]
fn the_timestamp_chip_opens_the_time_dialog() {
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    h.warm_up();
    let (rect, text) = h.timeline().timestamp;
    assert!(
        text.ends_with("live"),
        "precondition: a fresh pane's timestamp reads live, got {text:?}"
    );
    h.mouse_click(rect.center());
    h.warm_up();
    assert!(
        h.text_painted_in(h.screen_rect(), "Select Time"),
        "clicking the timestamp chip did not open the Set Time dialog"
    );
}

/// **Back steps into the archive; forward is dead while live** — the navigation
/// semantics that moved from the layers panel, driven through the timeline's drawn
/// rects for the first time.
#[test]
fn back_steps_into_the_archive_and_forward_is_dead_while_live() {
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    h.load_scan("KTLX");

    let t = h.timeline();
    assert!(
        !t.live.1,
        "precondition: a live pane's Live button is not in the red style"
    );
    assert!(!t.fwd.1, "forward must be disabled while live");
    h.mouse_click(t.fwd.0.center());
    assert!(
        !h.last_actions().iter().any(|a| {
            matches!(
                a,
                crate::actions::GuiAction::NavigateTime { .. }
                    | crate::actions::GuiAction::NavigateOneScan { .. }
            )
        }),
        "a disabled forward button still navigated"
    );

    h.mouse_click(h.timeline().back.center());
    assert!(
        h.last_actions().iter().any(|a| matches!(
            a,
            crate::actions::GuiAction::NavigateTime {
                pane_idx: 0,
                step_secs: -600,
            }
        )),
        "back must step one default step backwards"
    );
    h.warm_up();
    let t = h.timeline();
    assert!(
        !h.gui_mut().pane(0).expect("pane 0").viewing_live,
        "back must drop the pane out of live"
    );
    assert!(
        t.live.1,
        "an archive pane's Live button must show the red not-live style"
    );
    assert!(t.fwd.1, "forward must come alive in the archive");
}

/// 74. **A wheel over the floating chrome zooms nothing underneath it.**
#[test]
fn a_wheel_over_the_floating_chrome_zooms_nothing_underneath() {
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    h.set_pane_count(2);
    h.load_scan("KTLX");
    h.make_pane_volume(1);
    h.warm_up();

    let timeline = h.timeline().rect;
    let status_bar = h.status_bar().rect;
    let panes = h.pane_rects();
    let ground = |h: &mut InputHarness| {
        h.gui_mut()
            .pane(1)
            .expect("pane 1 exists")
            .volume()
            .expect("pane 1 is in the 3D render mode")
            .camera
            .eye_distance()
    };

    let clear_volume = egui::pos2(panes[1].center().x, panes[1].center().y);
    assert!(
        !h.is_floating_layer_at(clear_volume),
        "precondition: the control point must be open map"
    );
    let before = ground(&mut h);
    h.scroll_at(clear_volume, egui::vec2(0.0, 200.0));
    h.frames_for(2, FRAME_DT);
    assert!(
        ground(&mut h) < before,
        "control: a scroll on the open volume pane must zoom it"
    );

    let covered = egui::pos2(
        (panes[1].left() + 40.0).max(timeline.left() + 8.0),
        timeline.center().y,
    );
    assert!(
        timeline.contains(covered) && panes[1].contains(covered),
        "precondition: the point is on the timeline over the volume pane"
    );
    let before = ground(&mut h);
    h.scroll_at(covered, egui::vec2(0.0, 200.0));
    h.frames_for(2, FRAME_DT);
    assert_eq!(
        ground(&mut h),
        before,
        "a wheel over the timeline dollied the 3D pane under it"
    );

    let clear_map = egui::pos2(panes[0].center().x + 80.0, panes[0].center().y);
    assert!(
        !h.is_floating_layer_at(clear_map),
        "precondition: the map control point must be open map"
    );
    let before = h.frame().resolved_zoom;
    h.scroll_at(clear_map, egui::vec2(0.0, 200.0));
    let zoomed = h.frames_for(12, FRAME_DT).resolved_zoom;
    assert!(
        zoomed != before,
        "control: a scroll on the open map pane must zoom it"
    );

    let covered = egui::pos2(panes[0].center().x + 80.0, status_bar.center().y);
    assert!(
        status_bar.contains(covered) && panes[0].contains(covered),
        "precondition: the point is on the status bar over the map pane"
    );
    let before = h.frame().resolved_zoom;
    h.scroll_at(covered, egui::vec2(0.0, 200.0));
    let after = h.frames_for(12, FRAME_DT).resolved_zoom;
    assert_eq!(
        after, before,
        "a wheel over the status bar zoomed the map under it"
    );
    let covered = egui::pos2(panes[0].right() - 80.0, timeline.center().y);
    assert!(
        timeline.contains(covered) && panes[0].contains(covered),
        "precondition: the point is on the timeline over the map pane"
    );
    let before = h.frame().resolved_zoom;
    h.scroll_at(covered, egui::vec2(0.0, 200.0));
    let after = h.frames_for(12, FRAME_DT).resolved_zoom;
    assert_eq!(
        after, before,
        "a wheel over the timeline zoomed the map under it"
    );
}

/// **Scrubbing drops out of live, on release** (plan §3.7).
#[test]
fn scrubbing_the_archive_commits_once_on_release_and_drops_live() {
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    h.load_scan("KTLX");
    assert!(
        h.gui_mut().pane(0).expect("pane 0").viewing_live,
        "precondition: the pane starts live"
    );

    let scrub = h.timeline().scrubber;
    assert!(scrub.is_positive(), "precondition: the scrubber is drawn");
    let mid = scrub.center();
    h.mouse_move(mid);
    h.frame();
    h.mouse_press(mid);
    h.frame();
    let dragged_to = mid + egui::vec2(-30.0, 0.0);
    h.mouse_move(dragged_to);
    h.frame();
    let navigated = |h: &InputHarness| {
        h.last_actions().iter().any(|a| {
            matches!(
                a,
                crate::actions::GuiAction::NavigateTime { .. }
                    | crate::actions::GuiAction::JumpToLive { .. }
            )
        })
    };
    assert!(
        !navigated(&h),
        "the scrub emitted a navigation mid-drag: that is a fetch per \
         drag frame"
    );

    h.mouse_release(dragged_to);
    h.frame();
    assert!(
        h.last_actions().iter().any(|a| matches!(
            a,
            crate::actions::GuiAction::NavigateTime { pane_idx: 0, .. }
        )),
        "releasing the scrub mid-rail emitted no NavigateTime"
    );
    assert!(
        !h.gui_mut().pane(0).expect("pane 0").viewing_live,
        "the committed scrub left the pane claiming to be live"
    );
}

/// **Scrubbing to the right end restores live** (plan §3.7) — **on a pane
/// with no forecast timeline, which is the only kind of pane this still
/// holds for.**
///
/// WI-11 moved the live zone from "the last 1% of the rail" to "within
/// `LIVE_SNAP_PX` of the `now` boundary". On this pane those are the same
/// place, because `NOW_SPLIT` is `1.0` and `now` *is* the right end; on a
/// pane whose transport reaches forward the right end names the far edge of
/// the forecast horizon and answering live there would answer a question the
/// user did not ask. That case is
/// `the_live_zone_sits_at_now_and_not_at_the_far_end`, and it asserts the
/// opposite of this deliberately.
#[test]
fn scrubbing_to_the_right_end_restores_live() {
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    h.load_scan("KTLX");
    h.gui_mut().pane_mut(0).expect("pane 0").viewing_live = false;
    h.warm_up();

    let scrub = h.timeline().scrubber;
    let end = egui::pos2(scrub.right() - 1.0, scrub.center().y);
    h.mouse_move(end);
    h.frame();
    h.mouse_press(end);
    h.frame();
    h.mouse_release(end);
    h.frame();

    assert!(
        h.last_actions()
            .iter()
            .any(|a| matches!(a, crate::actions::GuiAction::JumpToLive { pane_idx: 0 })),
        "releasing the scrub at the right end must jump back to live"
    );
    assert!(
        !h.last_actions()
            .iter()
            .any(|a| matches!(a, crate::actions::GuiAction::NavigateTime { .. })),
        "the right end must mean live, not an archive moment near now"
    );
}

/// 26e. **A product whose tilts have not arrived keeps its tilt picker.**
#[test]
fn a_product_whose_tilts_have_not_arrived_keeps_its_tilt_picker() {
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    h.load_scan("KTLX");
    h.open_pane_props();
    {
        let pane = h.gui_mut().pane_mut(0).unwrap();
        pane.set_overlay_enabled(known::RADAR, true);
        let info = pane.scan_info.as_mut().expect("a scan was loaded");
        info.product_elevations.insert(
            squallar_radar::fields::product_for(&radar_fields::known::REFLECTIVITY)
                .expect("a registered field"),
            vec![0.5, 1.5],
        );
        info.available_products.push(
            squallar_radar::fields::product_for(&radar_fields::known::ECHO_TOPS)
                .expect("a registered field"),
        );
        info.product_elevations.insert(
            squallar_radar::fields::product_for(&radar_fields::known::ECHO_TOPS)
                .expect("a registered field"),
            Vec::new(),
        );
        pane.set_selected_product(radar_fields::known::ECHO_TOPS);
        pane.set_selected_elevation(0.0);
    }
    h.warm_up();

    assert!(
        h.painted_text_strings().iter().any(|t| t == "0.0\u{b0}"),
        "the tilt picker vanished for a product whose angles have not landed; \
             painted: {:?}",
        h.painted_text_strings(),
    );
    assert_eq!(
        h.gui_mut().pane(0).unwrap().get_rendering_params(),
        Some((radar_fields::known::ECHO_TOPS, 0.0)),
    );

    h.gui_mut()
        .pane_mut(0)
        .unwrap()
        .set_selected_product(radar_fields::known::REFLECTIVITY);
    h.warm_up();
    assert!(
        h.painted_text_strings().iter().any(|t| t == "0.5\u{b0}"),
        "painted: {:?}",
        h.painted_text_strings(),
    );
}

/// A harness with one pane on KTLX offering a Level II and a Level III product at
/// 0.5°, radar layer on, showing a finished `showing` image.
fn pane_showing(showing: &squallar_source::product::FieldId) -> InputHarness {
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    h.load_scan("KTLX");
    h.gui_mut()
        .pane_mut(0)
        .unwrap()
        .set_overlay_enabled(known::RADAR, true);
    h.offer_product(0, &radar_fields::known::REFLECTIVITY, 0.5);
    h.offer_product(0, &radar_fields::known::ECHO_TOPS, 0.5);
    h.select_product(0, showing);
    h.place_radar_image(0, showing, 0.5, None, None, None);
    h
}

/// The pending-render notice for `product`, as the pane paints it.
fn notice_painted(h: &InputHarness, product: squallar_source::product::FieldId) -> bool {
    h.painted_text_strings()
        .iter()
        .any(|t| t.starts_with('\u{27f3}') && t.contains(crate::field_facts::name(&product)))
}

/// Any pending-render notice at all, whatever it names.
fn any_notice_painted(h: &InputHarness) -> bool {
    h.painted_text_strings()
        .iter()
        .any(|t| t.starts_with('\u{27f3}'))
}

/// 26f. **A pane says when its image is not the product it is labelled with.**
#[test]
fn a_pane_says_when_its_image_is_not_the_selected_product() {
    let mut h = pane_showing(&radar_fields::known::REFLECTIVITY);
    assert!(
        !any_notice_painted(&h),
        "a pane showing what it selected has nothing to disown; painted: {:?}",
        h.painted_text_strings(),
    );

    h.select_product(0, &radar_fields::known::ECHO_TOPS);
    assert!(
        notice_painted(&h, radar_fields::known::REFLECTIVITY),
        "the pane is showing reflectivity and labelled echo tops, and said \
             nothing; painted: {:?}",
        h.painted_text_strings(),
    );
    let pane_rect = h.pane_rects()[0];
    assert!(
        h.text_painted_in(pane_rect, "showing Reflectivity"),
        "the notice was painted outside the pane it describes; painted: {:?}",
        h.painted_text_strings(),
    );
    assert!(
        h.gui_mut()
            .pane(0)
            .unwrap()
            .overlay_cache(&known::RADAR)
            .and_then(|c| c.current())
            .is_some(),
        "the pane was cleared rather than annotated",
    );

    h.place_radar_image(0, &radar_fields::known::ECHO_TOPS, 0.5, None, None, None);
    assert!(
        !any_notice_painted(&h),
        "the notice outlived the render it was waiting for; painted: {:?}",
        h.painted_text_strings(),
    );
}

/// 26g. **…and it does not flash on a routine refresh.**
#[test]
fn a_same_selection_re_render_draws_no_notice() {
    let mut h = pane_showing(&radar_fields::known::REFLECTIVITY);
    for _ in 0..2 {
        h.place_radar_image(0, &radar_fields::known::REFLECTIVITY, 0.5, None, None, None);
        assert!(
            !any_notice_painted(&h),
            "a routine re-render of the selected product drew a notice; \
                 painted: {:?}",
            h.painted_text_strings(),
        );
    }

    h.gui_mut().pane_mut(0).unwrap().set_selected_elevation(0.6);
    h.warm_up();
    assert_eq!(
        h.gui_mut().pane(0).unwrap().get_rendering_params(),
        Some((radar_fields::known::REFLECTIVITY, 0.5)),
        "precondition: the selection snaps to the drawn sweep",
    );
    assert!(
        !any_notice_painted(&h),
        "the snapped selection is the image on screen; painted: {:?}",
        h.painted_text_strings(),
    );
}

/// 26h. **The notice is the same for a Level II and a Level III product.**
#[test]
fn the_pending_notice_is_identical_for_both_datasources() {
    use squallar_source::product::FieldId;

    let (l2, l3) = (
        &radar_fields::known::REFLECTIVITY,
        &radar_fields::known::ECHO_TOPS,
    );
    let l3_of =
        |id: &FieldId| squallar_radar::fields::product_for(id).is_some_and(|p| p.is_level3());
    assert!(!l3_of(l2) && l3_of(l3), "one of each datasource");

    let mut awaiting_l3 = pane_showing(l2);
    awaiting_l3.select_product(0, l3);
    let mut awaiting_l2 = pane_showing(l3);
    awaiting_l2.select_product(0, l2);

    let notice_of = |h: &InputHarness| -> String {
        h.painted_text_strings()
            .iter()
            .find(|t| t.starts_with('\u{27f3}'))
            .cloned()
            .unwrap_or_else(|| {
                panic!(
                    "no pending-render notice painted: {:?}",
                    h.painted_text_strings()
                )
            })
    };
    assert_eq!(
        notice_of(&awaiting_l3),
        "\u{27f3} showing Reflectivity 0.5\u{b0}"
    );
    assert_eq!(
        notice_of(&awaiting_l2),
        "\u{27f3} showing Echo Tops 0.5\u{b0}"
    );

    let strip = |t: &str, name: &str| t.replace(name, "<product>");
    assert_eq!(
        strip(&notice_of(&awaiting_l3), crate::field_facts::name(l2)),
        strip(&notice_of(&awaiting_l2), crate::field_facts::name(l3)),
        "the two datasources drew differently shaped notices, which is a way \
             to tell them apart",
    );

    awaiting_l3.place_radar_image(0, l3, 0.5, None, None, None);
    awaiting_l2.place_radar_image(0, l2, 0.5, None, None, None);
    assert!(!any_notice_painted(&awaiting_l3));
    assert!(!any_notice_painted(&awaiting_l2));
}

/// 26i. **A pane with no image says nothing, and neither does a looping one.**
#[test]
fn nothing_is_said_where_there_is_no_stale_image() {
    let mut bare = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    bare.load_scan("KTLX");
    bare.gui_mut()
        .pane_mut(0)
        .unwrap()
        .set_overlay_enabled(known::RADAR, true);
    bare.offer_product(0, &radar_fields::known::ECHO_TOPS, 0.5);
    bare.select_product(0, &radar_fields::known::ECHO_TOPS);
    assert!(
        !any_notice_painted(&bare),
        "an empty pane has no pixels to disown; painted: {:?}",
        bare.painted_text_strings(),
    );

    let mut looping = pane_showing(&radar_fields::known::REFLECTIVITY);
    let site = squallar_radar::sites::get_radar_site("KTLX").expect("a real radar");
    {
        let pane = looping.gui_mut().pane_mut(0).unwrap();
        *pane.time_state_mut(&known::RADAR) =
            crate::radar_layer::begin_loop(600, site, squallar_radar::types::RenderView::PlanView);
    }
    looping.select_product(0, &radar_fields::known::ECHO_TOPS);
    assert!(
        looping
            .gui_mut()
            .pane(0)
            .unwrap()
            .time_state(&known::RADAR)
            .is_active(),
        "precondition: the loop is running",
    );
    assert!(
        !any_notice_painted(&looping),
        "a looping pane drew the static image's notice; painted: {:?}",
        looping.painted_text_strings(),
    );
}

/// 26c. **…and day-old data reads as hours, not as 1,560 minutes.**
#[test]
fn day_old_data_reads_in_hours() {
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    h.load_scan("KTLX");
    h.set_data_time(0, Some(written_ago(26 * 60 + 5)));

    let bar = h.status_bar();
    assert_eq!(
        bar.product_age_text
            .as_deref()
            .map(|t| t.contains("(26h 5m old)")),
        Some(true),
        "26-hour-old data must read in hours, got {:?}",
        bar.product_age_text
    );
    assert!(
        h.text_painted_in(bar.rect, "26h 5m old"),
        "…and be painted: {:?}",
        h.painted_text_strings()
    );

    let mut phone = InputHarness::with_screen(egui::vec2(420.0, 900.0));
    phone.load_scan("KTLX");
    phone.set_data_time(0, Some(written_ago(26 * 60 + 5)));
    assert!(
        phone.status_bar().rect == egui::Rect::NOTHING,
        "the phone shell drew a status bar"
    );
    let t = phone.timeline();
    assert!(
        t.age_text.is_empty(),
        "the narrow transport dropped its age chip (M9-10), got {:?}",
        t.age_text
    );
    assert!(
        !t.timestamp.1.is_empty(),
        "with the age chip gone the timestamp is the moment's one statement"
    );
}

use crate::actions::GuiAction;

/// The texture plans the last frame asked for.
fn requested_plans(h: &InputHarness) -> Vec<crate::overlay_cache::OverlayTexturePlan> {
    h.last_actions()
        .iter()
        .filter_map(|a| match a {
            GuiAction::RenderOverlay { texture, .. } => Some(*texture),
            _ => None,
        })
        .collect()
}

/// A harness with a texture overlay switched on, so the map pane emits
/// `RenderOverlay`.
fn harness_requesting_overlays() -> InputHarness {
    let mut h = InputHarness::new();
    h.gui_mut().enable_overlay_for_test(&known::RADAR_SITES);
    h.warm_up();
    h
}

/// A 2D overlay is planned in **physical pixels**, so a display at two of them per
/// point gets twice the texels per axis and not the same texture stretched over
/// twice the pixels.
#[test]
fn a_hidpi_pane_plans_its_overlay_in_physical_pixels_not_points() {
    let mut h = harness_requesting_overlays();
    h.set_max_texture_side(16384);
    let at_1x = requested_plans(&h);
    assert!(
        !at_1x.is_empty(),
        "fixture must actually reach the render path — no RenderOverlay was emitted",
    );
    for plan in &at_1x {
        assert_eq!(
            plan.pixels_per_point, 1.0,
            "the harness starts unscaled, which is what makes the 2x run mean \
             something",
        );
    }

    h.set_pixels_per_point(2.0);
    h.frames_for(2, FRAME_DT);
    let at_2x = requested_plans(&h);
    assert_eq!(
        at_2x.len(),
        at_1x.len(),
        "the same overlays must still be requested at 2x",
    );

    for (one, two) in at_1x.iter().zip(&at_2x) {
        assert_eq!(
            two.pixels_per_point, 2.0,
            "the plan must carry the density it was sized at, or the rasterizer \
             draws every marker at half size",
        );
        for (axis, a, b) in [
            ("width", one.width, two.width),
            ("height", one.height, two.height),
        ] {
            assert!(
                b.abs_diff(a * 2) <= 1,
                "{axis}: {a} px at 1x became {b} px at 2x, which is not the \
                 doubling a 2x display asks for",
            );
        }
    }
}

/// The whole point of the change, exercised through the real UI: the number the
/// adapter reports reaches `plan_overlay_texture` via `RawInput` and bounds what
/// the pane asks for.
#[test]
fn a_small_adapter_limit_bounds_what_the_pane_requests() {
    const LIMIT: u32 = 1024;

    let mut h = harness_requesting_overlays();
    let unclamped = requested_plans(&h);
    assert!(
        !unclamped.is_empty(),
        "fixture must actually reach the render path — no RenderOverlay was emitted"
    );
    assert!(
        unclamped
            .iter()
            .any(|p| p.width > LIMIT || p.height > LIMIT),
        "fixture must cross the limit before it is imposed, else the clamp is never \
             exercised; got {unclamped:?}"
    );

    h.set_max_texture_side(LIMIT as usize);
    let clamped = requested_plans(&h);
    assert!(
        !clamped.is_empty(),
        "still expected a render request after clamping"
    );
    for plan in &clamped {
        assert!(
            plan.width <= LIMIT && plan.height <= LIMIT,
            "requested {}x{} against a {LIMIT} limit",
            plan.width,
            plan.height
        );
        assert!(
            plan.overdraw < crate::overlay_cache::OVERDRAW_FRACTION,
            "overdraw must have been given up to fit"
        );
    }
}

/// Desktop is untouched: a limit no window can reach leaves the full overdraw in
/// place, so the plan is what the pre-clamp arithmetic produced.
#[test]
fn a_desktop_class_limit_leaves_the_request_alone() {
    let mut h = harness_requesting_overlays();
    h.set_max_texture_side(1024);
    let small_limit = requested_plans(&h);
    assert!(!small_limit.is_empty());

    h.set_max_texture_side(16384);
    let desktop = requested_plans(&h);
    assert!(!desktop.is_empty());
    for plan in &desktop {
        assert_eq!(
            plan.overdraw,
            crate::overlay_cache::OVERDRAW_FRACTION,
            "a desktop adapter must not cost any overdraw"
        );
    }
    assert_ne!(
        small_limit, desktop,
        "precondition: the small limit must clamp this pane, or this test \
             proves nothing about the limit being read at all"
    );
}

/// Off-centre, but well inside a 24pt icon.
const INSIDE_THE_ICON: egui::Vec2 = egui::vec2(5.0, 5.0);

/// The site switches the last frame asked the app for.
fn site_switches(h: &InputHarness) -> Vec<(String, usize)> {
    h.last_actions()
        .iter()
        .filter_map(|a| match a {
            GuiAction::SwitchRadarSite { site, pane_idx } => Some((site.clone(), *pane_idx)),
            _ => None,
        })
        .collect()
}

/// A harness showing `site`, with the radar-site overlay on, plus the screen
/// position that site's icon is drawn at.
fn harness_showing_site(site: &str) -> (InputHarness, egui::Pos2) {
    let mut h = InputHarness::new();
    h.load_scan(site);
    h.gui_mut().enable_overlay_for_test(&known::RADAR_SITES);
    h.warm_up();
    assert!(
        h.overlay_enabled(&known::RADAR_SITES),
        "precondition: the radar-site overlay must be on, or nothing draws \
             an icon to click"
    );
    let icon = h.pane_rects()[0].center();
    assert!(
        !h.is_floating_layer_at(icon),
        "precondition: the icon must not already be under a floating layer"
    );
    (h, icon)
}

/// 30. **Clicking a radar site icon switches to that radar.**
#[test]
fn clicking_a_radar_site_icon_switches_to_that_site() {
    let (mut h, icon) = harness_showing_site("KTLX");
    let target = icon + INSIDE_THE_ICON;

    h.mouse_move(target);
    h.frames_for(3, FRAME_DT);
    assert!(
        !h.is_floating_layer_at(target),
        "the readout claimed the pointer, so the dialog gate will read the \
             click that follows as landing on a floating window"
    );

    h.mouse_click(target);

    assert_eq!(
        site_switches(&h),
        vec![("KTLX".to_owned(), 0)],
        "clicking KTLX's icon did not ask the app to switch to KTLX"
    );
}

/// 31. **Tapping one switches too.**
#[test]
fn tapping_a_radar_site_icon_switches_to_that_site() {
    let (mut h, icon) = harness_showing_site("KTLX");

    h.touch_tap(icon + INSIDE_THE_ICON);
    h.frame_after(AFTER_DOUBLE_TAP_TIMEOUT);

    assert_eq!(
        site_switches(&h),
        vec![("KTLX".to_owned(), 0)],
        "tapping KTLX's icon did not ask the app to switch to KTLX"
    );
}

/// 32. **...and clicking beside one does not.**
#[test]
fn clicking_beside_a_radar_site_icon_switches_nothing() {
    let (mut h, icon) = harness_showing_site("KTLX");
    let beside = icon + egui::vec2(40.0, 0.0);
    assert!(
        h.pane_rects()[0].contains(beside),
        "precondition: the spot must still be on the map"
    );

    h.mouse_click(beside);

    assert_eq!(
        site_switches(&h),
        vec![],
        "a click 40pt clear of the icon still switched sites"
    );
}

/// 32b. **The pane rect is what keeps a click off the map off the map.**
#[test]
fn a_click_outside_the_pane_does_not_reach_a_site_icon_straddling_its_edge() {
    let mut h = InputHarness::new();
    h.gui_mut().enable_overlay_for_test(&known::RADAR_SITES);
    h.warm_up();

    let pane = h.pane_rects()[0];
    let edge = egui::pos2(pane.center().x + 150.0, pane.top());
    h.place_site_at(0, "KTLX", edge);

    let on_map = edge + egui::vec2(0.0, 5.0);
    let off_pane = edge - egui::vec2(0.0, 5.0);
    let pane = h.pane_rects()[0];
    assert!(
        pane.contains(on_map),
        "precondition: one click is on the pane"
    );
    assert!(!pane.contains(off_pane), "precondition: the other is not");
    assert!(
        h.screen_rect().contains(off_pane),
        "precondition: the blocked click must still be on screen — this is \
         a click on the chrome, not a click on nothing"
    );
    assert!(
        h.map_excluded_rects().is_empty(),
        "precondition: a wide screen excludes no floating chrome, so the \
         excluded-rect condition cannot be what blocks this"
    );
    assert!(
        !h.is_floating_layer_at(off_pane),
        "precondition: the top bar is a background layer, so the layer \
         condition cannot be what blocks this either"
    );

    h.mouse_click(on_map);
    assert!(
        site_switches(&h).contains(&("KTLX".to_owned(), 0)),
        "control: the icon really is under both clicks — if this fails the \
             site was never placed and the assertion below is vacuous. Got {:?}",
        site_switches(&h)
    );

    h.mouse_click(off_pane);
    assert_eq!(
        site_switches(&h),
        vec![],
        "a click in the top bar switched the radar site: the map is \
             hit-testing chrome"
    );
}

/// 32c. **A dialog over a site icon takes its hover readout away.**
#[test]
fn a_dialog_over_a_site_icon_suppresses_its_hover_readout() {
    let mut h = InputHarness::new();
    h.gui_mut().enable_overlay_for_test(&known::RADAR_SITES);
    h.warm_up();
    let target = h.screen_center();
    h.place_site_at(0, "KTLX", target);
    assert!(
        h.pane_rects()[0].contains(target),
        "precondition: the icon is on the pane, so the pane-rect condition \
             cannot be what blocks the hover"
    );
    assert!(
        h.map_excluded_rects().is_empty(),
        "precondition: nothing is excluded on a wide screen either"
    );

    h.mouse_move(target);
    h.frames_for(3, FRAME_DT);
    assert!(
        h.painted_text_strings()
            .iter()
            .any(|t| t.contains("KTLX\nLat:")),
        "control: hovering the icon must draw the site readout, or the \
             assertion below passes for free. Painted: {:?}",
        h.painted_text_strings()
    );

    h.gui_mut().set_time_dialog_open_for_test(true);
    h.warm_up();
    assert!(
        h.is_floating_layer_at(target),
        "precondition: the time dialog must cover the icon"
    );

    h.mouse_move(target);
    h.frames_for(3, FRAME_DT);
    assert!(
        !h.painted_text_strings()
            .iter()
            .any(|t| t.contains("KTLX\nLat:")),
        "the site readout came up through an open dialog: the map is \
             hovering what the dialog is covering"
    );
}

/// 33. **A dropdown's collapsed box says what its open list says.**
#[test]
fn a_dropdown_shows_its_option_label_not_the_raw_value() {
    for host in [known::MODEL_DATA, known::LIGHTNING] {
        let mut h = compact_with_layers_drawer();
        h.set_overlay_on_pane(0, &host, true);
        h.open_layer_in_inspector(&host);

        let drawn = h.dropdowns();
        assert!(
            !drawn.is_empty(),
            "precondition: {host:?} must be offering a dropdown, got none"
        );

        for dropdown in &drawn {
            let (options, selected) = h
                .dropdown_model(&dropdown.label)
                .unwrap_or_else(|| panic!("no handler offers a {:?} dropdown", dropdown.label));
            let expected = options
                .iter()
                .find(|(value, _)| *value == selected)
                .map(|(_, display)| display.clone())
                .unwrap_or_else(|| {
                    panic!(
                        "the {:?} dropdown's selected value {selected:?} is not \
                             among the options it offers: {options:?}",
                        dropdown.label
                    )
                });
            assert_eq!(
                dropdown.selected_text, expected,
                "the {:?} dropdown's collapsed box disagrees with the label its \
                     own list puts against {selected:?}",
                dropdown.label
            );
            assert!(
                h.text_painted_in(dropdown.rect, &dropdown.selected_text),
                "the {:?} dropdown reported {:?} but egui painted no such text \
                     inside {:?}",
                dropdown.label,
                dropdown.selected_text,
                dropdown.rect
            );
        }

        for dropdown in &drawn {
            let mut h = compact_with_layers_drawer();
            h.set_overlay_on_pane(0, &host, true);
            h.open_layer_in_inspector(&host);
            let (options, _) = h.dropdown_model(&dropdown.label).expect("still offered");
            let dropdown = h
                .dropdowns()
                .into_iter()
                .find(|d| d.label == dropdown.label)
                .expect("the fresh harness draws the same dropdown");
            assert!(
                h.screen_rect().contains(dropdown.rect.center()),
                "the {:?} dropdown was laid out at {:?}, off the {:?} viewport, \
                     so the click below would open nothing",
                dropdown.label,
                dropdown.rect,
                h.screen_rect()
            );

            h.mouse_click(dropdown.rect.center());
            h.warm_up();

            let painted = h.painted_text_strings();
            let labels_shown = options
                .iter()
                .filter(|(_, display)| painted.contains(display))
                .count();
            assert!(
                labels_shown >= 2,
                "the {:?} list opened but painted fewer than two of its own \
                     option labels, so the check below has nothing to bite on; it \
                     painted {painted:?}",
                dropdown.label
            );
            for (value, display) in &options {
                assert!(
                    value == display || !painted.contains(value),
                    "the open {:?} list painted the raw option id {value:?} \
                         where its label is {display:?}",
                    dropdown.label
                );
            }
        }
    }
}

/// 34. **The scan arriving must not re-key a widget.**
#[test]
fn a_scan_arriving_moves_no_widget_id() {
    let mut h = InputHarness::with_screen(egui::vec2(750.0, 900.0));
    h.gui_mut()
        .apply(crate::shell_api::GuiEvent::Fetching(true));
    h.warm_up();
    assert!(
        h.status_bar().poll_chip.is_none(),
        "precondition: a fetch must be in flight, so the status bar is \
         showing the spinner rather than the auto-poll chip"
    );

    h.clear_id_changes();
    h.load_scan("KTLX");

    assert!(
        h.status_bar().poll_chip.is_some(),
        "precondition: the scan must have cleared the fetch, or the widget \
             count in the status bar never changed and this proves nothing"
    );
    assert_eq!(
        h.id_changes(),
        &[] as &[egui::Rect],
        "egui saw a widget rect come back under a different id when the \
             scan arrived: everything it remembers under those ids is discarded"
    );
}

/// 34b. **Crossing a breakpoint re-keys nothing.**
#[test]
fn crossing_a_breakpoint_re_keys_nothing() {
    let mut h = InputHarness::with_screen(egui::vec2(750.0, 480.0));
    // **A stack the user has filled from the catalog.** A curated stack
    // starts at the handful of layers that ship enabled, and this test's
    // subject is a LONG list - the panel's scroll, its clamped height, the
    // ids a scrolled body keeps. Built the way a user builds one, rather
    // than relied on as a property of the build's layer count.
    h.fill_stack();
    h.set_drawer_open(true);
    h.gui_mut().open_settings();
    h.load_scan("KTLX");
    assert_eq!(
        h.width_class(),
        crate::ui_layout::WidthClass::Medium,
        "precondition: start above the 600pt breakpoint"
    );

    let probes = h.widget_id_probes();
    let scroll_id = probes
        .iter()
        .find(|(name, _)| *name == "layers_scroll")
        .expect("precondition: the scroll area must report an id")
        .1;
    h.scroll_at(egui::pos2(80.0, 300.0), egui::vec2(0.0, -120.0));
    h.frames_for(3, FRAME_DT);
    let scrolled = h.scroll_offset(scroll_id);
    assert!(
        scrolled.is_some_and(|o| o.y > 0.0),
        "precondition: the layers panel must have scrolled, got {scrolled:?}"
    );

    h.clear_id_changes();
    h.set_screen(egui::vec2(550.0, 480.0));
    h.set_drawer_open(true);
    assert_eq!(
        h.width_class(),
        crate::ui_layout::WidthClass::Compact,
        "precondition: the resize crossed the 600pt breakpoint"
    );

    assert_eq!(
        h.id_changes(),
        &[] as &[egui::Rect],
        "a widget rect came back under a different id across the \
         breakpoint: everything egui remembers under the old id is \
         discarded on every resize past 600pt"
    );

    let compact_probes = h.widget_id_probes();
    assert!(
        compact_probes
            .iter()
            .any(|(name, _)| *name == "inspector_scroll"),
        "precondition: the sheet's Inspector page must be up and reporting"
    );
    for probe in &compact_probes {
        assert!(
            probes.contains(probe),
            "{:?} moved with the layout across the 600pt host switch",
            probe.0
        );
    }
    h.close_inspector();
    assert_eq!(
        h.widget_id_probes()
            .iter()
            .find(|(name, _)| *name == "layers_scroll")
            .expect("the Layers page must report the stack's scroll id")
            .1,
        scroll_id,
        "a widget id that keys stored state moved with the layout"
    );
    assert_eq!(
        h.scroll_offset(scroll_id),
        scrolled,
        "the scroll position did not survive the breakpoint"
    );
}

/// 34b-bis. **A delivered round takes the error banner down with it.**
///
/// The banner reads the layer's own retry ledger now rather than a copy of the
/// message the shell kept until someone dismissed it, so it says "this layer is
/// failing" rather than "this layer failed once". A successful round resets the
/// ledger and the banner goes with it.
///
/// That is a behaviour change and this is where it is written down: the error
/// used to outlive the condition and sit over freshly delivered radar until the
/// user clicked the cross.
#[test]
fn a_delivered_round_takes_the_error_banner_down_with_it() {
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    h.gui_mut()
        .apply(crate::shell_api::GuiEvent::Error("boom".to_owned()));
    h.warm_up();
    assert!(
        h.painted_text_strings().iter().any(|t| t == "boom"),
        "precondition: the error must be on screen before the scan, or this \
         test cannot see it leave"
    );

    h.load_scan("KTLX");
    assert!(
        !h.painted_text_strings().iter().any(|t| t == "boom"),
        "a scan arrived and the failure banner stayed up. It is a view of the \
         layer's health, and the layer is not failing any more"
    );
}

/// 34c. **An error on screen keeps its id while the row moves around it.**
#[test]
fn an_error_on_screen_keeps_its_id_while_the_row_changes_around_it() {
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    h.gui_mut()
        .apply(crate::shell_api::GuiEvent::Error("boom".to_owned()));
    h.gui_mut()
        .apply(crate::shell_api::GuiEvent::Fetching(true));
    h.warm_up();
    assert!(
        h.status_bar().poll_chip.is_none(),
        "precondition: a fetch must be in flight, so the bar is showing the \
         spinner rather than the chip"
    );
    assert!(
        h.painted_text_strings().iter().any(|t| t == "boom"),
        "precondition: the error must be on screen, or the slot under test \
         is not allocated at all"
    );

    h.clear_id_changes();
    // The fetch ends without a delivery. It used to be `load_scan`, which is a
    // *successful* round -- and since the banner became a view of the layer's
    // own health rather than a second copy of the message, a success clears it,
    // so the error would not have been on screen to keep an id. What this test
    // is about is unchanged: something to the left of the error changes width,
    // and the error's slot must not be re-keyed by it.
    h.gui_mut()
        .apply(crate::shell_api::GuiEvent::Fetching(false));
    h.warm_up();
    assert!(
        h.status_bar().poll_chip.is_some(),
        "precondition: the fetch must have ended, or nothing to the left of \
             the error changed"
    );
    assert!(
        h.painted_text_strings().iter().any(|t| t == "boom"),
        "precondition: the error must still be on screen after the fetch ended"
    );
    assert_eq!(
        h.id_changes(),
        &[] as &[egui::Rect],
        "a scan arriving re-keyed the error slot: its rect is pinned to the \
             row's right edge while its id follows the widget count to its left"
    );

    h.clear_id_changes();
    h.set_data_time(0, Some(written_ago(5)));
    assert!(
        h.status_bar().product_age_text.is_some(),
        "precondition: the age line must have appeared, or nothing moved"
    );
    assert_eq!(
        h.id_changes(),
        &[] as &[egui::Rect],
        "the data age line appearing re-keyed the error slot"
    );
}

/// 35. **...and the probe that says so can see a real one.**
#[test]
fn the_id_change_probe_reports_a_real_id_change() {
    let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(400.0, 300.0));
    let ctx = egui::Context::default();
    let mut prev_widgets = egui::WidgetRects::default();
    let mut seen = Vec::new();
    for pass in 0..3u32 {
        ctx.begin_pass(egui::RawInput {
            screen_rect: Some(screen),
            ..Default::default()
        });
        let root = egui::Ui::new(
            ctx.clone(),
            egui::Id::new("canary_root"),
            egui::UiBuilder::new()
                .layer_id(egui::LayerId::background())
                .max_rect(screen),
        );
        let rect = egui::Rect::from_min_size(egui::pos2(10.0, 10.0), egui::vec2(50.0, 20.0));
        root.interact(rect, egui::Id::new(("canary", pass)), egui::Sense::click());
        let _ = ctx.end_pass();
        let widgets = pass_widgets(&ctx);
        seen.extend(id_changes_between(&prev_widgets, &widgets));
        prev_widgets = widgets;
    }
    assert!(
        !seen.is_empty(),
        "the id-change reader saw nothing for a widget that changed id \
             every pass, so the assertion it backs is vacuous"
    );
}

/// 36. **A pane born from the pane-count picker inherits the layer state.**
#[test]
fn a_pane_added_by_the_picker_still_shows_radar_with_layer_sync_off() {
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    assert_eq!(
        h.width_class(),
        crate::ui_layout::WidthClass::Expanded,
        "precondition: an arbitrary width — the picker is in the top bar \
         at all of them"
    );
    h.set_layer_links(false);
    assert!(
        h.overlay_enabled(&known::RADAR),
        "precondition: the active pane must have Radar on, or there is no \
             state for the newcomer to inherit"
    );

    let two = h
        .pane_options()
        .into_iter()
        .find(|o| o.count == 2)
        .expect("the picker must offer a 2-pane split on a desktop width");
    h.mouse_click(two.rect.center());
    h.frames_for(3, FRAME_DT);
    assert_eq!(
        h.pane_count(),
        2,
        "precondition: the click must have split the map"
    );
    assert!(
        !h.gui_mut().pane(0).expect("pane 0").layer_link,
        "precondition: the active pane must still be layer-unlinked, or the \
             sync pass did the seeding"
    );

    assert!(
        h.overlay_enabled_on(1, &known::RADAR),
        "the picker's new pane came up with every overlay off — its empty \
             `enabled_overlays` was never seeded from the handler state"
    );
}

/// The site of the first `FetchRadarScan` among `actions`, if any.
fn fetched_site(actions: &[crate::actions::GuiAction]) -> Option<String> {
    actions.iter().find_map(|a| match a {
        crate::actions::GuiAction::FetchRadarScan(config) => Some(config.site.clone()),
        _ => None,
    })
}

/// 37. **Refresh fetches the site the active pane is viewing.**
///
/// Was `refresh_fetches_the_active_panes_site_not_the_global_one`. The
/// property is unchanged; its contrast partner is not, because the global it
/// named no longer exists (WO-SITE). The site that could wrongly be fetched
/// instead is now **another pane's**, so the fixture is two panes on
/// different radars with the second one active.
#[test]
fn refresh_fetches_the_active_panes_site_not_another_panes() {
    let mut h = InputHarness::with_screen(egui::vec2(800.0, 900.0));
    assert_eq!(
        h.width_class(),
        crate::ui_layout::WidthClass::Medium,
        "precondition: an arbitrary width — the dropdown is the same at all \
         of them"
    );

    h.set_pane_count(2);
    h.gui_mut()
        .pane_mut(0)
        .expect("pane 0")
        .set_site("KDMX".to_owned());
    h.gui_mut()
        .pane_mut(1)
        .expect("pane 1")
        .set_site("KTLX".to_owned());
    h.mouse_click(h.pane_rects()[1].center());
    h.warm_up();
    assert_eq!(
        h.active_pane_index(),
        1,
        "precondition: the second pane must be the active one"
    );
    assert_eq!(
        h.gui_mut().active_pane().site(),
        "KTLX",
        "precondition: the active pane must be the one set to KTLX"
    );
    assert_eq!(
        h.gui_mut().pane(0).expect("pane 0").site(),
        "KDMX",
        "precondition: the two panes must disagree, or this test could pass \
         off either one"
    );

    h.mouse_click(h.status_bar().refresh.center());
    assert_eq!(
        fetched_site(h.last_actions()).as_deref(),
        Some("KTLX"),
        "the status-bar Refresh fetched another pane's site, not the active \
         pane's"
    );

    h.open_menu();
    h.mouse_click(clickable_leaf(&h, "Refresh Radar").center());
    assert_eq!(
        fetched_site(h.last_actions()).as_deref(),
        Some("KTLX"),
        "the menu's Refresh fetched another pane's site, not the active \
         pane's"
    );
}

/// 37b. **The Set Time dialog fetches the ACTIVE PANE's site**, the same
/// answer as the Refresh above.
///
/// **This row's assertion is inverted from the one it replaces**, and that is
/// the point of it. Was `the_time_dialogs_ok_fetches_the_global_site_not_the_active_panes`:
/// added at WO-E8d to pin that this one control deliberately read the
/// persisted global rather than the pane in front of the user, so the two
/// controls disagreed on purpose. WO-SITE retires the global on the ruling
/// that nothing is app-wide, and this dialog belongs to a pane like every
/// other control. The two controls now agree, and both stay pinned because
/// a pane's site is the only site there is to get wrong.
///
/// The fixture keeps its teeth: two panes on different radars, the active one
/// second, so an arm that reached for any other pane reads red.
#[test]
fn the_time_dialogs_ok_fetches_the_active_panes_site() {
    let mut h = InputHarness::with_screen(egui::vec2(800.0, 900.0));

    h.set_pane_count(2);
    h.gui_mut()
        .pane_mut(0)
        .expect("pane 0")
        .set_site("KDMX".to_owned());
    h.gui_mut()
        .pane_mut(1)
        .expect("pane 1")
        .set_site("KTLX".to_owned());
    h.mouse_click(h.pane_rects()[1].center());
    h.gui_mut().set_time_dialog_open_for_test(true);
    h.warm_up();

    assert_eq!(
        h.active_pane_index(),
        1,
        "precondition: the second pane must be the active one"
    );
    assert_eq!(
        h.gui_mut().active_pane().site(),
        "KTLX",
        "precondition: the active pane must be the one set to KTLX"
    );
    assert_eq!(
        h.gui_mut().pane(0).expect("pane 0").site(),
        "KDMX",
        "precondition: the two panes must disagree, or this test could pass \
         off either one"
    );
    assert!(
        h.text_painted_in(h.screen_rect(), "Select Time"),
        "precondition: the dialog must be on screen to click its OK"
    );

    let ok = h
        .painted_text_rects()
        .into_iter()
        .find(|(_, text)| text == "OK")
        .expect("the time dialog draws an OK button")
        .0;
    h.mouse_click(ok.center());

    assert_eq!(
        fetched_site(h.last_actions()).as_deref(),
        Some("KTLX"),
        "the Set Time dialog fetched a site that is not the active pane's"
    );
}

/// 37c. **The Set Time dialog shows the time that is actually selected.**
///
/// The two text boxes are a *view* of `TimeDialogState::timestamp`, and the
/// three writers that must keep them in step are the shell's push, "Use
/// Current Time" and Cancel — the last of which is how a half-typed edit is
/// thrown away. WO-E8d put all three behind `TimeDialogState::select`.
///
/// Pinned here because **nothing pinned it before**: a tamper that made
/// `select` write the timestamp and leave both strings stale was green across
/// both packages, which would have shown the user the previous scan's time
/// while fetching a different one.
#[test]
fn the_time_dialog_shows_the_time_the_shell_last_selected() {
    let mut h = InputHarness::with_screen(egui::vec2(800.0, 900.0));

    let picked = chrono::NaiveDate::from_ymd_opt(2019, 5, 20)
        .expect("a real date")
        .and_hms_opt(21, 7, 33)
        .expect("a real time");
    h.gui_mut()
        .apply(crate::shell_api::GuiEvent::SelectedTime(picked));
    h.gui_mut().set_time_dialog_open_for_test(true);
    h.warm_up();

    let screen = h.screen_rect();
    assert!(
        h.text_painted_in(screen, "Select Time"),
        "precondition: the dialog must be on screen for its boxes to be read"
    );
    assert!(
        h.text_painted_in(screen, "2019-05-20"),
        "the date box shows a date that is not the selected one"
    );
    assert!(
        h.text_painted_in(screen, "21:07:33"),
        "the time box shows a time that is not the selected one"
    );
}

/// 38. **A hover readout dies with the radar that produced it.**
#[test]
fn a_hover_readout_does_not_outlive_its_pane_or_its_radar() {
    let mut h = InputHarness::with_screen(egui::vec2(800.0, 900.0));
    h.mouse_move(h.map_center());
    h.warm_up();
    assert!(
        h.status_bar().hover,
        "precondition: mouse modality, or there is no readout to go stale"
    );

    h.gui_mut().pane_mut(0).unwrap().hover_value = Some("LIVE READOUT".to_owned());
    h.frame();
    assert!(
        h.painted_text_strings().iter().any(|t| t == "LIVE READOUT"),
        "precondition: a visible pane's readout must reach the status bar"
    );

    h.frame();
    assert!(
        !h.painted_text_strings().iter().any(|t| t == "LIVE READOUT"),
        "a readout with no radar left behind it froze in the status bar"
    );

    h.set_pane_count(4);
    h.gui_mut().pane_mut(3).unwrap().hover_value = Some("HIDDEN PANE READOUT".to_owned());
    h.set_pane_count(2);
    h.frame();
    assert!(
        !h.painted_text_strings()
            .iter()
            .any(|t| t == "HIDDEN PANE READOUT"),
        "a hidden pane's stale readout surfaced in the status bar"
    );
}

/// Two points either side of a storm near KTLX, as the ends of a drawn line.
fn section_ends() -> (GeoPoint, GeoPoint) {
    (
        GeoPoint {
            lat: 35.0,
            lon: -97.8,
        },
        GeoPoint {
            lat: 35.6,
            lon: -96.9,
        },
    )
}

/// KTLX's reflectivity ladder on VCP 212, as the sampler resolves it — the chosen
/// sweeps' **median** elevations, not the cut table's round numbers.
fn vcp_212_rungs() -> Vec<f64> {
    vec![
        0.4834, 0.8789, 1.3184, 1.8018, 2.4170, 3.1201, 4.0430, 5.0977, 6.4160, 8.0273, 10.0195,
        12.5000, 15.6006, 19.5117,
    ]
}

/// The axes of a complete VCP 212 reflectivity section 100 km long, whose ladder is
/// [`vcp_212_rungs`].
fn vcp_212_axes() -> squallar_radar::xsect::SectionAxes {
    squallar_radar::xsect::SectionAxes {
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

/// 39. **Every pane reports a pointer frame, whatever kind it is.**
#[test]
fn every_pane_reports_a_pointer_frame_whatever_its_kind() {
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    h.set_pane_count(3);
    let (a, b) = section_ends();
    h.make_pane_cross_section(1, a, b);
    h.make_pane_volume(2);
    assert_eq!(
        h.pane_kinds(),
        vec![
            squallar_radar::types::RenderView::PlanView,
            squallar_radar::types::RenderView::CrossSection,
            squallar_radar::types::RenderView::Volume
        ],
        "precondition: one pane of each kind, or this proves nothing"
    );

    assert_eq!(
        h.pane_pointers()
            .iter()
            .map(|probe| probe.pane_idx)
            .collect::<Vec<_>>(),
        vec![0, 1, 2],
        "a pane resolved no pointer state for the frame"
    );

    let rects = h.pane_rects();
    h.mouse_click(rects[2].center());
    h.frames_for(2, FRAME_DT);
    assert_eq!(
        h.active_pane_index(),
        2,
        "precondition: clicking the volume pane must make it active"
    );
    assert_eq!(
        h.pane_pointers()
            .iter()
            .filter(|probe| probe.is_active)
            .map(|probe| probe.pane_idx)
            .collect::<Vec<_>>(),
        vec![2],
        "exactly one pane must own the pointer, and it is the active one"
    );
}

/// 40. **Converting a pane keeps what it was looking at.**
#[test]
fn a_converted_pane_keeps_its_site_and_viewport() {
    let mut h = InputHarness::new();
    h.load_scan("KAMA");
    {
        let pane = h
            .gui_mut()
            .pane_mut(0)
            .expect("a fresh harness has one pane");
        pane.set_selected_product(radar_fields::known::VELOCITY);
        pane.set_selected_elevation(1.5);
        pane.viewing_live = false;
        let _ = pane.map_memory.set_zoom(9.25);
        pane.map_memory.center_at(walkers::lat_lon(35.0, -97.8));
    }
    h.warm_up();

    /// Everything about a pane that is *not* its kind.
    fn looking_at(
        h: &mut InputHarness,
    ) -> (
        String,
        Option<&'static str>,
        String,
        f32,
        bool,
        f64,
        Option<walkers::Position>,
    ) {
        let pane = h.gui_mut().pane(0).expect("pane 0");
        (
            pane.site().to_string(),
            pane.scan_info.as_ref().map(|info| info.site.name),
            crate::field_facts::name(&pane.selected_product()).to_owned(),
            pane.selected_elevation(),
            pane.viewing_live,
            pane.map_memory.zoom(),
            pane.map_memory.detached(),
        )
    }

    let before = looking_at(&mut h);
    assert_eq!(
        h.pane_kinds(),
        vec![squallar_radar::types::RenderView::PlanView],
        "precondition: it starts as a map"
    );

    let (a, b) = section_ends();
    h.make_pane_cross_section(0, a, b);

    assert_eq!(
        h.pane_kinds(),
        vec![squallar_radar::types::RenderView::CrossSection],
        "precondition: the conversion must actually have happened"
    );
    assert_eq!(
        looking_at(&mut h),
        before,
        "converting the pane changed what it is looking at"
    );

    assert_eq!(
        h.gui_mut()
            .pane(0)
            .expect("pane 0")
            .cross_section()
            .and_then(|section| section.line)
            .map(|line| (line.a(), line.b())),
        Some((a, b)),
    );
}

/// 41. **A non-map pane paints its empty state, in its own rect.**
#[test]
fn a_non_map_pane_paints_its_empty_state() {
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    h.set_pane_count(3);
    h.make_pane_unaimed_cross_section(1);
    h.make_pane_volume(2);

    let rects = h.pane_rects();
    assert_eq!(
        h.pane_content_probes()
            .iter()
            .map(|probe| (probe.pane_idx, probe.view, probe.rect))
            .collect::<Vec<_>>(),
        vec![
            (0, squallar_radar::types::RenderView::PlanView, rects[0]),
            (1, squallar_radar::types::RenderView::CrossSection, rects[1]),
            (2, squallar_radar::types::RenderView::Volume, rects[2]),
        ],
        "the arm that ran for a pane is not the arm for that pane's kind"
    );

    for (idx, copy) in [
        (1usize, crate::ui::CROSS_SECTION_EMPTY_STATE),
        (2, crate::ui::VOLUME_EMPTY_STATE),
    ] {
        assert!(
            h.text_painted_in(rects[idx], copy),
            "pane {idx} did not paint {copy:?}; it painted {:?}",
            h.painted_text_strings()
        );
        for other in (0..3).filter(|other| *other != idx) {
            assert!(
                !h.text_painted_in(rects[other], copy),
                "pane {other} painted pane {idx}'s empty state"
            );
        }
    }
}

/// 43. **Converting the active pane from the dropdown really converts it.**
#[test]
fn converting_the_active_pane_from_the_dropdown_makes_it_a_volume_pane() {
    let mut h = compact_with_menu();
    h.load_scan("KTLX");
    assert_eq!(
        h.pane_kinds(),
        vec![squallar_radar::types::RenderView::PlanView],
        "precondition: it starts as a map"
    );
    assert_eq!(
        h.menu_leaf(crate::ui::VOLUME_PANE_LABEL).map(|l| l.value),
        Some(Some(false)),
        "precondition: the dropdown must draw the toggle, unchecked"
    );

    h.mouse_click(clickable_leaf(&h, crate::ui::VOLUME_PANE_LABEL).center());
    h.frames_for(3, FRAME_DT);

    assert_eq!(
        h.pane_kinds(),
        vec![squallar_radar::types::RenderView::Volume],
        "the click never reached the pane: the write landed on the pane the \
             layers panel had taken out of the vector"
    );
    assert_eq!(
        h.pane_content_probes()
            .iter()
            .map(|probe| probe.view)
            .collect::<Vec<_>>(),
        vec![squallar_radar::types::RenderView::Volume],
        "the pane converted but the map arm still drew it"
    );
    assert!(
        h.text_painted_in(h.pane_rects()[0], crate::ui::VOLUME_EMPTY_STATE),
        "the volume pane painted {:?} instead of its empty state",
        h.painted_text_strings()
    );
    assert_eq!(
        h.menu_leaf(crate::ui::VOLUME_PANE_LABEL).map(|l| l.value),
        Some(Some(true)),
        "the checkbox did not read back the conversion, so it looks to the \
             user as though the click did nothing"
    );

    h.mouse_click(clickable_leaf(&h, crate::ui::VOLUME_PANE_LABEL).center());
    h.frames_for(3, FRAME_DT);
    assert_eq!(
        h.pane_kinds(),
        vec![squallar_radar::types::RenderView::PlanView]
    );
}

/// 44. **A non-map pane keeps the controls that apply to it and drops the rest.**
#[test]
fn a_non_map_pane_keeps_the_controls_that_apply_to_it_and_drops_the_rest() {
    /// Which of the radar combos the last frame resolved an id for — the
    /// inspector's own report, not a reconstruction of it.
    fn combos(h: &InputHarness) -> Vec<&'static str> {
        h.widget_id_probes()
            .into_iter()
            .map(|(name, _)| name)
            .filter(|name| *name == "product_sel" || *name == "elev_sel")
            .collect()
    }

    for kind in [
        squallar_radar::types::RenderView::CrossSection,
        squallar_radar::types::RenderView::Volume,
    ] {
        let mut h = InputHarness::with_screen(egui::vec2(1200.0, 900.0));
        h.load_scan("KTLX");
        h.offer_product(0, &radar_fields::known::REFLECTIVITY, 0.5);
        h.open_pane_props();
        assert_eq!(
            combos(&h),
            vec!["product_sel", "elev_sel"],
            "precondition: a map pane with a tilt on offer draws both, so the \
                 absence below is the pane's kind and not a missing scan"
        );
        h.mouse_click(h.timeline().loop_toggle.0.center());
        assert!(
            h.last_actions()
                .iter()
                .any(|a| matches!(a, crate::actions::GuiAction::EnableLoop { .. })),
            "precondition: a map pane's loop toggle must enable the loop"
        );
        assert!(
            h.stack_row(&known::NWS_ALERTS).is_some(),
            "precondition: a map pane's stack draws the layer rows"
        );

        match kind {
            squallar_radar::types::RenderView::CrossSection => {
                let (a, b) = section_ends();
                h.make_pane_cross_section(0, a, b);
            }
            _ => h.make_pane_volume(0),
        }
        h.frames_for(2, FRAME_DT);
        assert_eq!(
            h.pane_kinds(),
            vec![kind],
            "precondition: the active pane must really have converted"
        );

        assert_eq!(
            combos(&h),
            vec!["product_sel"],
            "{kind:?}: either the product picker went with the map — leaving \
             this pane unable to be pointed at another moment — or a tilt \
             picker was drawn for a pane that reads every cut"
        );
        let timeline = h.timeline();
        assert!(
            timeline.step_dropdown.is_positive() && timeline.back.is_positive(),
            "{kind:?}: time navigation went with the map, so this pane can \
             only ever show the live volume"
        );
        h.mouse_click(h.timeline().loop_toggle.0.center());
        let armed = h.last_actions().iter().any(|a| {
            matches!(
                a,
                crate::actions::GuiAction::EnableLoop { .. }
                    | crate::actions::GuiAction::DisableLoop { .. }
            )
        });
        assert!(
            armed,
            "{kind:?}: the loop toggle did nothing, so this pane cannot be \
             animated even though every frame of one is something the loop \
             machinery fills",
        );
        if kind == squallar_radar::types::RenderView::CrossSection {
            assert!(
                h.stack().rows.is_empty(),
                "{kind:?}: layer rows were drawn for a pane with no projector \
                 anywhere in its frame: {:?}",
                h.stack().rows
            );
        } else {
            assert!(
                h.stack_row(&known::NWS_ALERTS).is_some(),
                "{kind:?}: the layer rows went with the plan view, leaving \
                 this pane's floor honouring a layer set nothing on screen \
                 can reach: {:?}",
                h.stack().rows
            );
        }
    }
}

/// 45. **A pane with no ground does not keep the label-tile pyramid downloading —
///     and a 3D pane's floor is ground.**
#[test]
fn only_a_pane_with_ground_keeps_the_label_tiles_downloading() {
    fn tiles_remade_after_a_reset(h: &mut InputHarness) -> bool {
        h.gui_mut().clear_graphics_state();
        assert!(
            !h.gui.label_tiles_made_for_test(),
            "precondition: the reset must really have dropped the tile sources"
        );
        h.frames_for(2, FRAME_DT);
        h.gui.label_tiles_made_for_test()
    }

    fn lone_pane_wanting_labels() -> InputHarness {
        let mut h = InputHarness::with_screen(egui::vec2(1200.0, 900.0));
        h.set_overlay_on_pane(0, &known::CITY_LABELS, true);
        h
    }

    let mut on_a_map = lone_pane_wanting_labels();
    assert!(
        tiles_remade_after_a_reset(&mut on_a_map),
        "precondition: a map pane with city labels on must fetch label tiles"
    );

    let mut on_a_floor = lone_pane_wanting_labels();
    on_a_floor.make_pane_volume(0);
    assert!(
        on_a_floor.overlay_enabled(&known::CITY_LABELS),
        "precondition: the pane still *remembers* wanting labels, which is what \
             makes this a filter rather than a cleared flag"
    );
    assert_eq!(
        on_a_floor.gui_mut().mirror_source_rects().len(),
        1,
        "precondition: the pane is not even asking for a floor strip, so the \
             labels below would have nowhere to land whatever this decides",
    );
    assert!(
        tiles_remade_after_a_reset(&mut on_a_floor),
        "a lone 3D pane's floor draws the city-label layer and nothing fetched \
             the tiles for it: `draw_floor_strip` was handed `label_tiles: \
             None`, so the floor came up with no city names on it",
    );

    let mut floor_hidden = lone_pane_wanting_labels();
    floor_hidden.make_pane_volume(0);
    floor_hidden
        .gui_mut()
        .pane_mut(0)
        .expect("pane 0")
        .volume_mut()
        .expect("a 3D pane has volume state")
        .hide_floor = true;
    assert!(
        floor_hidden.gui_mut().mirror_source_rects().is_empty(),
        "precondition: a hidden floor must not be asking for a strip",
    );
    assert!(
        !tiles_remade_after_a_reset(&mut floor_hidden),
        "a 3D pane with its floor switched off kept the label-tile pyramid \
             downloading for a surface that is not drawn",
    );

    let mut section = lone_pane_wanting_labels();
    section.make_pane_unaimed_cross_section(0);
    assert!(
        !tiles_remade_after_a_reset(&mut section),
        "a pane with no map to draw labels on kept the label-tile pyramid \
             downloading"
    );
}

/// 46. **A non-map pane's product picker survives the Radar layer being off.**
#[test]
fn a_non_map_panes_product_picker_ignores_the_radar_layer_toggle() {
    let mut h = InputHarness::with_screen(egui::vec2(1200.0, 900.0));
    h.load_scan("KTLX");
    h.open_pane_props();
    h.set_overlay_on_pane(0, &known::RADAR, false);
    h.frames_for(2, FRAME_DT);

    let has_product = |h: &InputHarness| {
        h.widget_id_probes()
            .iter()
            .any(|(name, _)| *name == "product_sel")
    };
    assert!(
        !has_product(&h),
        "precondition: a map pane with the Radar layer off draws no product \
             picker, or the assertion below is about nothing"
    );

    h.make_pane_volume(0);
    h.frames_for(2, FRAME_DT);

    assert!(
        has_product(&h),
        "the Radar layer toggle suppressed the product picker on a pane with \
             no map, which has no such layer to turn off"
    );
}

/// The stack's screen rect — the floating area's own rect, from egui's area state
/// rather than a reconstruction of the panel's position constants, so it keeps
/// meaning "the layer stack" if the insets ever change.
fn sidebar_rect(h: &InputHarness) -> egui::Rect {
    h.layers_panel_rect()
        .expect("the layer stack must be on screen")
}

/// The inspector's screen rect, on the same terms as [`sidebar_rect`].
fn inspector_rect(h: &InputHarness) -> egui::Rect {
    h.inspector_rect().expect("the inspector must be on screen")
}

/// 49. **Every pane kind's Pane-properties body opens with the same identity
///     line.**
#[test]
fn every_pane_kinds_sidebar_opens_with_the_same_identity_line() {
    let mut h = InputHarness::with_screen(egui::vec2(1200.0, 900.0));
    h.load_scan("KDMX");
    h.open_pane_props();
    assert_eq!(
        h.inspector().crumb,
        "Pane 1 \u{203a} Properties",
        "the crumb must name the pane-props body"
    );
    let inspector = inspector_rect(&h);

    assert!(
        h.text_painted_in(inspector, "KDMX - Map"),
        "a map pane's properties body must open with its identity line; \
             painted: {:?}",
        h.painted_text_strings_in(inspector)
    );

    h.make_pane_volume(0);
    h.frames_for(2, FRAME_DT);
    assert!(
        h.text_painted_in(inspector, "KDMX - 3D volume"),
        "a 3D pane's properties body must open with the same identity line, \
             with its kind in it; painted: {:?}",
        h.painted_text_strings_in(inspector)
    );

    h.make_pane_unaimed_cross_section(0);
    h.frames_for(2, FRAME_DT);
    assert!(
        h.text_painted_in(inspector, "KDMX - Cross-section"),
        "a section pane's properties body must open with the same identity \
             line, with its kind in it; painted: {:?}",
        h.painted_text_strings_in(inspector)
    );
}

/// 50. **The missing layer list is explained, in one line, for the one kind that is
///     missing it — and for no other.**
#[test]
fn the_missing_layer_list_is_explained_for_the_kind_that_is_missing_it() {
    let mut h = InputHarness::with_screen(egui::vec2(1200.0, 900.0));
    h.load_scan("KTLX");
    let sidebar = sidebar_rect(&h);
    assert!(
        !h.text_painted_in(sidebar, crate::ui::NON_MAP_LAYERS_NOTE),
        "a map pane draws the layer list itself and must not also \
             carry the note explaining its absence"
    );

    h.make_pane_volume(0);
    h.frames_for(2, FRAME_DT);
    assert!(
        !h.text_painted_in(sidebar, crate::ui::NON_MAP_LAYERS_NOTE),
        "a 3D pane draws the layer list too — its floor and its glass both \
             honour the set — so the note explaining an absence is a claim \
             about nothing; painted: {:?}",
        h.painted_text_strings_in(sidebar)
    );

    h.make_pane_map(0);
    h.make_pane_unaimed_cross_section(0);
    h.frames_for(2, FRAME_DT);
    assert!(
        h.text_painted_in(sidebar, crate::ui::NON_MAP_LAYERS_NOTE),
        "the layer list is omitted with nothing to say why, which is what \
             made the panel read as broken; painted: {:?}",
        h.painted_text_strings_in(sidebar)
    );
}

/// 50a. **A 3D pane's layer rows are the layers a 3D pane draws — all of them, on
/// both of its surfaces — and a cross-section still has none.**
#[test]
fn a_3d_panes_layer_rows_are_the_layers_a_3d_pane_draws() {
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    h.load_scan("KTLX");
    h.open_layers();

    let on_a_map: Vec<LayerId> = h.stack().rows.iter().map(|row| row.kind.clone()).collect();
    assert!(
        on_a_map.contains(&known::COLOR_SCALE) && on_a_map.len() > 1,
        "precondition: a map pane lists every layer, colour scale included: \
         {on_a_map:?}"
    );

    h.set_overlay_on_pane(0, &known::CITY_LABELS, false);
    h.make_pane_volume(0);
    h.open_layers();

    let stack = h.stack();
    let on_a_volume: Vec<LayerId> = stack.rows.iter().map(|row| row.kind.clone()).collect();
    assert_eq!(
        on_a_volume, on_a_map,
        "a 3D pane draws the ground kinds onto its floor and the colour scale \
         onto its glass, each gated on its own `is_overlay_enabled` — so its \
         rows are the same list, in the same order"
    );
    assert_eq!(
        stack.non_map_note,
        egui::Rect::NOTHING,
        "the explained absence was drawn beside a list that is present"
    );
    assert_ne!(
        stack.add_top,
        egui::Rect::NOTHING,
        "a 3D pane has a map to add a layer to — its floor"
    );

    let labels = h
        .stack_row(&known::CITY_LABELS)
        .expect("the city-labels row is in the list asserted above");
    assert!(
        !labels.eye_on,
        "the row must show the state the floor is actually honouring, which \
         is the one the user set before the conversion"
    );

    h.mouse_click(labels.eye.center());
    h.warm_up();
    assert!(
        h.gui_mut()
            .pane(0)
            .expect("pane 0")
            .is_overlay_enabled(&known::CITY_LABELS),
        "the eye on a 3D pane's row did not reach the pane's own layer state"
    );

    h.make_pane_map(0);
    h.make_pane_unaimed_cross_section(0);
    h.open_layers();
    let stack = h.stack();
    assert!(
        stack.rows.is_empty(),
        "a cross-section has no projector anywhere in its frame, so a row \
         here would toggle nothing: {:?}",
        stack.rows
    );
    assert_ne!(
        stack.non_map_note,
        egui::Rect::NOTHING,
        "the one kind with no list must still say why"
    );
}

/// 51. **A converted pane's own controls sit inside the Pane-properties body's
///     shared structure, in its order.**
#[test]
fn kind_specific_blocks_sit_inside_the_shared_sidebar_structure() {
    /// The y-centre of the topmost painted run containing `needle`, inside the
    /// sidebar.
    fn y_of(h: &InputHarness, sidebar: egui::Rect, needle: &str) -> f32 {
        h.painted_text_rects()
            .iter()
            .filter(|(r, text)| sidebar.contains(r.center()) && text.contains(needle))
            .map(|(r, _)| r.center().y)
            .min_by(f32::total_cmp)
            .unwrap_or_else(|| {
                panic!(
                    "{needle:?} was not painted in the sidebar; painted: {:?}",
                    h.painted_text_strings_in(sidebar)
                )
            })
    }

    fn assert_descending_order(h: &InputHarness, sidebar: egui::Rect, anchors: &[&str]) {
        let ys: Vec<(f32, &str)> = anchors.iter().map(|n| (y_of(h, sidebar, n), *n)).collect();
        for pair in ys.windows(2) {
            assert!(
                pair[0].0 < pair[1].0,
                "sidebar structure broken: {:?} (y={}) must sit above {:?} (y={})",
                pair[0].1,
                pair[0].0,
                pair[1].1,
                pair[1].0
            );
        }
    }

    let mut h = InputHarness::with_screen(egui::vec2(1200.0, 900.0));
    h.load_scan("KDMX");
    h.open_pane_props();

    h.make_pane_volume(0);
    h.frames_for(2, FRAME_DT);
    assert_descending_order(
        &h,
        inspector_rect(&h),
        &[
            "KDMX - 3D volume",
            "Reflectivity",
            crate::ui::VOLUME_SIDEBAR_HEADER,
            "Vertical:",
            "Mode:",
            "Map floor",
            "Reset view",
        ],
    );

    h.gui_mut()
        .pane_mut(0)
        .expect("pane 0 exists")
        .volume_mut()
        .expect("pane 0 is a 3D pane")
        .view_mode = crate::pane::VolumeViewMode::Isosurface;
    h.gui_mut().volume_alpha.set(
        &radar_fields::known::REFLECTIVITY,
        crate::volume_alpha::AlphaCurve::from_alphas([7u8; crate::volume_alpha::CURVE_LEN]),
    );
    h.frames_for(2, FRAME_DT);
    assert_descending_order(
        &h,
        inspector_rect(&h),
        &[
            crate::ui::VOLUME_SIDEBAR_HEADER,
            "Vertical:",
            "Mode:",
            "\u{2265}:",
            "applies to the lit volume only",
            "Map floor",
            "Reset view",
        ],
    );

    let (a, b) = section_ends();
    h.make_pane_cross_section(0, a, b);
    h.frames_for(2, FRAME_DT);
    assert_descending_order(
        &h,
        inspector_rect(&h),
        &[
            "KDMX - Cross-section",
            "Reflectivity",
            crate::ui::SECTION_SIDEBAR_HEADER,
            "A - B: 105 km",
        ],
    );

    h.gui_mut()
        .pane_mut(0)
        .expect("pane 0 exists")
        .set_view(squallar_radar::types::RenderView::PlanView);
    h.make_pane_unaimed_cross_section(0);
    h.frames_for(2, FRAME_DT);
    assert_descending_order(
        &h,
        inspector_rect(&h),
        &[crate::ui::SECTION_SIDEBAR_HEADER, "No line drawn yet"],
    );

    assert!(
        h.text_painted_in(sidebar_rect(&h), crate::ui::NON_MAP_LAYERS_NOTE),
        "the stack must carry the layer-list note for a converted pane"
    );
}

/// The Map floor checkbox **acts**: a click on the drawn row flips the pane's
/// `hide_floor`, both ways, and the flipped state survives a restart.
#[test]
fn the_map_floor_click_flips_the_pane_and_survives_a_restart() {
    use squallar_kv::MemoryKvStore;

    let mut h = InputHarness::with_screen(egui::vec2(1200.0, 900.0));
    h.load_scan("KDMX");
    h.open_pane_props();
    h.make_pane_volume(0);
    h.frames_for(2, FRAME_DT);

    let floor_row = |h: &InputHarness| {
        h.painted_text_rects()
            .into_iter()
            .find(|(_, text)| text.contains("Map floor"))
            .expect("the volume body draws its Map floor row")
            .0
    };

    h.mouse_click(floor_row(&h).center());
    h.frames_for(2, FRAME_DT);
    assert!(
        h.gui_mut().pane(0).unwrap().volume().unwrap().hide_floor,
        "clicking Map floor must hide the floor"
    );

    let store = MemoryKvStore::default();
    h.gui_mut().save_ui_config(&store);
    let mut restored = crate::Gui::new();
    assert!(restored.load_ui_config(&store));
    assert!(
        restored.pane(0).unwrap().volume().unwrap().hide_floor,
        "the clicked-off floor must come back off after a restart"
    );

    h.mouse_click(floor_row(&h).center());
    h.frames_for(2, FRAME_DT);
    assert!(
        !h.gui_mut().pane(0).unwrap().volume().unwrap().hide_floor,
        "a second click must show the floor again"
    );
}

/// The Vertical slider **acts**: dragging it moves the pane's vertical exaggeration
/// in the direction of the drag, and the dragged value survives a restart.
#[test]
fn the_vertical_slider_drag_stretches_the_box_and_survives_a_restart() {
    use squallar_kv::MemoryKvStore;

    let mut h = InputHarness::with_screen(egui::vec2(1200.0, 900.0));
    h.load_scan("KDMX");
    h.open_pane_props();
    h.make_pane_volume(0);
    h.frames_for(2, FRAME_DT);

    let exaggeration = |h: &mut InputHarness| {
        h.gui_mut()
            .pane(0)
            .unwrap()
            .volume()
            .unwrap()
            .camera
            .vertical_exaggeration()
    };
    let drag = |h: &mut InputHarness, from_x: f32, by: f32| {
        let label = h
            .painted_text_rects()
            .into_iter()
            .find(|(_, text)| text.contains("Vertical:"))
            .expect("the volume body draws its Vertical row")
            .0;
        let start = egui::pos2(label.right() + from_x, label.center().y);
        h.mouse_press(start);
        h.frame();
        h.mouse_move(start + egui::vec2(by, 0.0));
        h.frame();
        h.mouse_release(start + egui::vec2(by, 0.0));
        h.frames_for(2, FRAME_DT);
    };

    let shipped = exaggeration(&mut h);
    drag(&mut h, 60.0, 40.0);
    let stretched = exaggeration(&mut h);
    assert!(
        (stretched - shipped).abs() > 0.05,
        "dragging the Vertical slider must move the exaggeration off its \
         shipped {shipped}; it is still {stretched}"
    );

    drag(&mut h, 40.0, -20.0);
    let eased = exaggeration(&mut h);
    assert!(
        eased < stretched,
        "a leftward drag must ease the stretch: {eased} is not below \
         {stretched}"
    );

    let store = MemoryKvStore::default();
    h.gui_mut().save_ui_config(&store);
    let mut restored = crate::Gui::new();
    assert!(restored.load_ui_config(&store));
    let back = restored
        .pane(0)
        .unwrap()
        .volume()
        .unwrap()
        .camera
        .vertical_exaggeration();
    assert!(
        (back - eased).abs() < 1e-4,
        "the dragged exaggeration must survive a restart: saved {eased}, \
         loaded {back}"
    );
}

/// 52. **Converting the active pane keeps the panels' own widget ids.**
#[test]
fn converting_the_active_pane_keeps_the_sidebars_widget_ids() {
    fn shared_ids(h: &InputHarness) -> Vec<(&'static str, egui::Id)> {
        h.widget_id_probes()
            .into_iter()
            .filter(|(name, _)| {
                matches!(
                    *name,
                    "product_sel" | "time_step_sel" | "layers_scroll" | "inspector_scroll"
                )
            })
            .collect()
    }

    let mut h = InputHarness::with_screen(egui::vec2(1200.0, 900.0));
    h.load_scan("KTLX");
    h.open_pane_props();
    let before = shared_ids(&h);
    assert_eq!(
        before.len(),
        4,
        "precondition: all four shared controls must report ids, got {before:?}"
    );

    h.make_pane_volume(0);
    h.frames_for(2, FRAME_DT);
    assert_eq!(
        shared_ids(&h),
        before,
        "making the active pane 3D re-keyed a shared sidebar control: \
             everything egui remembers under the old id is silently discarded"
    );

    h.make_pane_unaimed_cross_section(0);
    h.frames_for(2, FRAME_DT);
    assert_eq!(
        shared_ids(&h),
        before,
        "making the active pane a section re-keyed a shared sidebar control"
    );
}

/// 48. **Converting a pane must not move any widget's egui `Id`.**
#[test]
fn converting_a_pane_moves_no_widget_id() {
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 500.0));
    // **A stack the user has filled from the catalog.** A curated stack
    // starts at the handful of layers that ship enabled, and this test's
    // subject is a LONG list - the panel's scroll, its clamped height, the
    // ids a scrolled body keeps. Built the way a user builds one, rather
    // than relied on as a property of the build's layer count.
    h.fill_stack();
    h.set_pane_count(3);
    h.load_scan("KTLX");

    let probes = h.widget_id_probes();
    let scroll_id = probes
        .iter()
        .find(|(name, _)| *name == "layers_scroll")
        .expect("precondition: the layers panel must report a scroll id")
        .1;
    h.scroll_at(egui::pos2(80.0, 400.0), egui::vec2(0.0, -120.0));
    h.frames_for(3, FRAME_DT);
    let scrolled = h.scroll_offset(scroll_id);
    assert!(
        scrolled.is_some_and(|offset| offset.y > 0.0),
        "precondition: the layers panel must have scrolled, got {scrolled:?}"
    );

    h.clear_id_changes();
    h.make_pane_unaimed_cross_section(1);

    assert_eq!(
        h.pane_kinds(),
        vec![
            squallar_radar::types::RenderView::PlanView,
            squallar_radar::types::RenderView::CrossSection,
            squallar_radar::types::RenderView::PlanView
        ],
        "precondition: the middle pane converted and the last one did not"
    );
    assert!(
        h.text_painted_in(h.pane_rects()[1], crate::ui::CROSS_SECTION_EMPTY_STATE),
        "precondition: pane 1 must really be drawing something else now"
    );

    assert_eq!(
        h.id_changes(),
        &[] as &[egui::Rect],
        "egui saw a widget rect come back under a different id when a pane \
             was converted: everything it remembers under those ids is discarded"
    );
    assert_eq!(
        probes,
        h.widget_id_probes(),
        "a widget id that keys stored state moved when a pane was converted"
    );
    assert_eq!(
        h.scroll_offset(scroll_id),
        scrolled,
        "the scroll position did not survive converting another pane"
    );
}

/// 54. **The menu checkbox arms the draw, and a drag on a map becomes a section.**
#[test]
fn the_menus_checkbox_arms_the_cross_section_draw() {
    let mut h = compact_with_menu();
    h.load_scan("KTLX");
    assert!(!h.section_draw_armed(), "precondition: it starts unarmed");
    assert_eq!(
        h.menu_leaf(crate::ui::DRAW_CROSS_SECTION_LABEL)
            .map(|l| l.value),
        Some(Some(false)),
        "precondition: the dropdown must draw the toggle, unchecked"
    );

    h.mouse_click(clickable_leaf(&h, crate::ui::DRAW_CROSS_SECTION_LABEL).center());
    h.frames_for(3, FRAME_DT);

    assert!(h.section_draw_armed(), "the checkbox did not arm the draw");
    assert_eq!(
        h.menu_leaf(crate::ui::DRAW_CROSS_SECTION_LABEL),
        None,
        "the dropdown stayed open over the map the line has to be drawn on"
    );

    h.open_menu();
    assert_eq!(
        h.menu_leaf(crate::ui::DRAW_CROSS_SECTION_LABEL)
            .map(|l| l.value),
        Some(Some(true)),
        "the checkbox does not show the mode it just turned on"
    );

    h.mouse_click(clickable_leaf(&h, crate::ui::DRAW_CROSS_SECTION_LABEL).center());
    h.frames_for(3, FRAME_DT);
    assert!(
        !h.section_draw_armed(),
        "the checkbox could not turn it off"
    );
}

/// The dropdown's other arm entry, on the same terms: it arms the 3D region pick,
/// closes itself over the map the box has to be dragged on, shows the mode it
/// turned on when reopened, and turns it off again.
#[test]
fn the_menus_checkbox_arms_the_3d_region_pick() {
    let mut h = compact_with_menu();
    h.load_scan("KTLX");
    assert!(!h.region_pick_armed(), "precondition: it starts unarmed");
    assert_eq!(
        h.menu_leaf(crate::ui::PICK_REGION_LABEL).map(|l| l.value),
        Some(Some(false)),
        "precondition: the dropdown must draw the toggle, unchecked"
    );

    h.mouse_click(clickable_leaf(&h, crate::ui::PICK_REGION_LABEL).center());
    h.frames_for(3, FRAME_DT);

    assert!(h.region_pick_armed(), "the checkbox did not arm the pick");
    assert_eq!(
        h.menu_leaf(crate::ui::PICK_REGION_LABEL),
        None,
        "the dropdown stayed open over the map the box has to be dragged on"
    );

    h.open_menu();
    assert_eq!(
        h.menu_leaf(crate::ui::PICK_REGION_LABEL).map(|l| l.value),
        Some(Some(true)),
        "the checkbox does not show the mode it just turned on"
    );

    h.mouse_click(clickable_leaf(&h, crate::ui::PICK_REGION_LABEL).center());
    h.frames_for(3, FRAME_DT);
    assert!(!h.region_pick_armed(), "the checkbox could not turn it off");
}

/// 55. **An armed drag on a map becomes a section aimed where it was drawn.**
#[test]
fn an_armed_drag_on_a_map_becomes_a_cross_section_aimed_where_it_was_drawn() {
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    h.load_scan("KTLX");
    h.set_section_draw_armed(true);
    h.warm_up();

    let pane = h.pane_rects()[0];
    let from = pane.center() - egui::vec2(120.0, 60.0);
    let to = pane.center() + egui::vec2(120.0, 60.0);
    let centre_before = h.pane_center(0);
    let want_a = h.ground_at(0, from);
    let want_b = h.ground_at(0, to);

    h.mouse_move(from);
    h.frame();
    h.mouse_press(from);
    h.frame();
    for step in 1..=4 {
        h.mouse_move(from + (to - from) * (step as f32 / 4.0));
        h.frame();
    }
    h.mouse_release(to);
    h.frames_for(2, FRAME_DT);

    assert!(
        !h.section_draw_armed(),
        "the mode stayed armed after producing a line: the next pan is a \
             second section"
    );
    assert_eq!(
        h.pane_center(0),
        centre_before,
        "the map panned during the draw — the drag belongs to the line"
    );

    let target = h
        .pane_kinds()
        .iter()
        .position(|k| *k == squallar_radar::types::RenderView::CrossSection)
        .expect("the drag produced no section pane");
    let line = h
        .section_line(target)
        .expect("the section pane has no line");
    assert!(
        (line.a().lat - want_a.y()).abs() < 1e-3 && (line.a().lon - want_a.x()).abs() < 1e-3,
        "the line starts at {:?}, not under the press at {want_a:?}",
        line.a()
    );
    assert!(
        (line.b().lat - want_b.y()).abs() < 1e-3 && (line.b().lon - want_b.x()).abs() < 1e-3,
        "the line ends at {:?}, not under the release at {want_b:?}",
        line.b()
    );
    assert_ne!(
        line.a(),
        line.b(),
        "both ends resolved to the same ground, so the drag is not being read"
    );
}

/// 56. **A rendered section's caption is calm by default, and the honesty detail is
///     one click away — reachable, in the user's words, and closable again.**
#[test]
fn a_rendered_sections_caption_is_calm_and_its_detail_is_one_click_away() {
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    h.load_scan("KTLX");
    let (a, b) = section_ends();
    h.make_pane_cross_section(0, a, b);
    h.place_section(0, vcp_212_axes(), &vcp_212_rungs());

    h.close_layers();

    let pane = h.pane_rects()[0];
    assert!(
        !h.text_painted_in(pane, crate::ui::CROSS_SECTION_EMPTY_STATE),
        "precondition: the pane must be drawing a picture, not its empty state"
    );

    assert!(
        h.text_painted_in(pane, "14 tilts"),
        "the default caption lost the ladder's own count; it painted {:?}",
        h.painted_text_strings_in(pane)
    );
    for wall_of_text in ["not measured", "slant range", "widest", "Echoes sit at"] {
        assert!(
            !h.text_painted_in(pane, wall_of_text),
            "the long-form detail is back in the default caption \
                 ({wall_of_text:?}); it painted {:?}",
            h.painted_text_strings_in(pane)
        );
    }

    let glyph = h
        .painted_text_rects()
        .into_iter()
        .find(|(rect, text)| text == "\u{2139}" && pane.contains(rect.center()))
        .map(|(rect, _)| rect)
        .expect("the caption has no \u{2139} detail toggle");
    h.mouse_click(glyph.center());
    h.frame();

    for phrase in ["4.9", "not measured", "Echoes sit at"] {
        assert!(
            h.text_painted_in(pane, phrase),
            "the opened detail never said {phrase:?}; it painted {:?}",
            h.painted_text_strings_in(pane)
        );
    }
    assert!(
        !h.text_painted_in(pane, "The section is right"),
        "the detail still argues with the map in front of the user"
    );

    h.mouse_click(glyph.center());
    h.frame();
    assert!(
        !h.text_painted_in(pane, "not measured"),
        "the detail did not close on a second click"
    );
    assert!(
        h.text_painted_in(pane, "14 tilts"),
        "closing the detail lost the caption itself"
    );
}

/// 46b. **A rendered section is drawn the right way up, over the caption that
/// describes it, with its ladder on it and its readout live.**
#[test]
fn a_rendered_section_is_the_right_way_up_and_carries_its_ladder() {
    let mut h = InputHarness::with_screen(egui::vec2(480.0, 900.0));
    h.load_scan("KTLX");
    let (a, b) = section_ends();
    h.make_pane_cross_section(0, a, b);
    h.place_section(0, vcp_212_axes(), &vcp_212_rungs());
    h.gui_mut()
        .pane_mut(0)
        .expect("pane 0 exists")
        .cross_section_mut()
        .expect("pane 0 is a section pane")
        .detail_open = true;
    h.warm_up();

    let pane = h.pane_rects()[0];
    let (bars_h, bars_v) = h.color_scale_bars(pane);
    let images: Vec<_> = h
        .painted_images_in(pane)
        .into_iter()
        .filter(|i| {
            let (w, ht) = (i.rect.width(), i.rect.height());
            !((ht - 20.0).abs() < 0.5 && w > 40.0 || (w - 20.0).abs() < 0.5 && ht > 40.0)
        })
        .collect();
    assert_eq!(
        bars_h + bars_v,
        1,
        "precondition: a section pane draws exactly one colour bar, so the \
             filter above is removing a known quad rather than an unknown one",
    );
    assert_eq!(
        images.len(),
        1,
        "a section pane paints exactly one textured quad that is not its \
             colour bar, its raster; found {images:?}"
    );
    let raster = images[0];
    assert!(
        raster.rect.width() > 0.0 && raster.rect.height() > 0.0,
        "the raster was painted into an empty rect: {:?}",
        raster.rect
    );

    assert_eq!(
        (raster.uv_at_top_left.x, raster.uv_at_top_left.y),
        (0.0, 0.0),
        "the top of the section's height axis is sampling the bottom of its \
             raster: the picture is drawn upside down, and a flipped storm still \
             looks like a storm"
    );
    assert_eq!(
        (raster.uv_at_bottom_right.x, raster.uv_at_bottom_right.y),
        (1.0, 1.0),
        "the section's raster is mirrored or cropped: uv {:?}..{:?}",
        raster.uv_at_top_left,
        raster.uv_at_bottom_right
    );

    let caption_rows: Vec<(egui::Rect, String)> = h
        .painted_text_rects()
        .into_iter()
        .filter(|(rect, text)| {
            pane.contains(rect.center())
                && (text.contains("tilts")
                    || text.contains("dotted curves")
                    || text.contains("Echoes sit at"))
        })
        .collect();
    assert_eq!(
        caption_rows.len(),
        3,
        "precondition: the headline and both detail sentences have to be on \
             the pane, or the overlap check below is looking at the wrong \
             thing: {:?}",
        h.painted_text_strings_in(pane)
    );
    let measured: f32 = caption_rows.iter().map(|(rect, _)| rect.height()).sum();
    let counted = caption_rows.len() as f32 * 13.0;
    assert!(
        measured - counted > 15.0,
        "precondition: the caption occupies {measured} points against a \
             counted {counted} — they agree at this pane width, so nothing here \
             says which of the two the layout used: {caption_rows:?}"
    );
    for (rect, text) in &caption_rows {
        assert!(
            rect.bottom() <= raster.rect.top() + 0.5,
            "a caption row was painted over the picture (row bottom {}, \
                 picture top {}): {text:?}",
            rect.bottom(),
            raster.rect.top(),
        );
    }

    let color = crate::ui::map::section_render::tilt_rung_color();
    let rungs = h.painted_segments_in(egui::Rect::EVERYTHING, color);
    assert!(
        !rungs.is_empty(),
        "no tilt rungs were drawn over the section, so nothing in the \
             picture says where the data actually is"
    );
    let left = rungs.iter().map(|(p, _)| p.x).fold(f32::INFINITY, f32::min);
    let starts = rungs
        .iter()
        .filter(|(p, _)| (p.x - left).abs() < 0.01)
        .count();
    assert_eq!(
        starts,
        vcp_212_rungs().len(),
        "the ladder drew {starts} curves for a 14-rung section",
    );
    assert_eq!(
        rungs.len() % starts,
        0,
        "{} segments do not divide into {starts} curves",
        rungs.len()
    );
    assert!(
        rungs.len() >= starts * 32,
        "the rungs were drawn as {} segments across {starts} curves, which \
             is not a traced beam centre",
        rungs.len(),
    );
    let over_the_picture = h.painted_segments_in(raster.rect, color).len();
    assert!(
        over_the_picture * 3 >= rungs.len(),
        "only {over_the_picture} of {} rung segments landed inside the \
             raster, so the ladder is not drawn where the data is",
        rungs.len(),
    );

    h.mouse_move(raster.rect.center());
    h.frame();
    let readout = h
        .gui
        .pane(0)
        .expect("pane 0")
        .hover_value
        .clone()
        .expect("a pointer over the picture wrote no readout");
    assert!(
        readout.contains("MSL") && readout.contains("along"),
        "the readout says nothing about where in the section the pointer is: \
             {readout}"
    );

    h.gui_mut()
        .pane_mut(0)
        .expect("pane 0")
        .cross_section_mut()
        .expect("a section pane")
        .unavailable = Some(crate::pane::SectionUnavailable::AwaitingCoveragePattern);
    h.frame();
    let notice = crate::pane::SectionUnavailable::AwaitingCoveragePattern.message();
    let head = notice
        .split_whitespace()
        .take(4)
        .collect::<Vec<_>>()
        .join(" ");
    assert!(
        h.text_painted_in(pane, &head),
        "the pane is showing a stale picture with no word of why it is \
             stale; it painted {:?}",
        h.painted_text_strings_in(pane)
    );
}

/// 47. **A wheel-zoom part-way through a drag does not move the anchor.**
#[test]
fn a_wheel_zoom_mid_drag_leaves_the_anchor_on_the_ground_it_was_put_on() {
    fn drag(zoom_mid_drag: bool) -> (SectionLine, f64) {
        let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
        h.load_scan("KTLX");
        h.set_section_draw_armed(true);
        h.warm_up();

        let pane = h.pane_rects()[0];
        let from = pane.center() - egui::vec2(120.0, 60.0);
        let to = pane.center() + egui::vec2(120.0, 60.0);

        h.mouse_move(from);
        h.frame();
        h.mouse_press(from);
        h.frame();

        if zoom_mid_drag {
            h.wheel_notch(pane.center(), egui::MouseWheelUnit::Line, -3.0);
        }
        h.frames_for(6, FRAME_DT);

        h.mouse_move(to);
        h.frame();
        h.mouse_release(to);
        h.frames_for(2, FRAME_DT);

        let target = h
            .pane_kinds()
            .iter()
            .position(|k| *k == squallar_radar::types::RenderView::CrossSection)
            .expect("the drag produced no section pane");
        let zoom = h.gui_mut().pane(0).unwrap().map_memory.zoom();
        (
            h.section_line(target)
                .expect("the section pane has no line"),
            zoom,
        )
    }

    let (plain, plain_zoom) = drag(false);
    let (zoomed, zoomed_zoom) = drag(true);

    assert!(
        (plain_zoom - zoomed_zoom).abs() > 0.05,
        "precondition: the wheel must really have zoomed ({plain_zoom} -> \
             {zoomed_zoom}), or nothing below distinguishes a held anchor from \
             an ignored wheel event"
    );
    assert_eq!(
        plain.a(),
        zoomed.a(),
        "the zoom moved the anchor: it is being held as a pixel, so the \
             line's near end drifted to whatever ground that pixel names now"
    );
    assert_ne!(
        plain.b(),
        zoomed.b(),
        "the release end did not move, so the zoom changed nothing about \
             what a pixel means and the assertion above proves nothing"
    );
}

/// A map on pane 0 feeding a rendered section on pane 1, exactly as the armed draw
/// leaves the layout — the fixture every endpoint-drag test starts from.
fn harness_with_committed_section() -> (InputHarness, GeoPoint, GeoPoint) {
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    h.set_pane_count(2);
    h.load_scan("KTLX");
    h.close_layers();
    h.warm_up();
    h.warm_up();
    let pane = h.pane_rects()[0];
    let to_geo = |pos: walkers::Position| GeoPoint {
        lat: pos.y(),
        lon: pos.x(),
    };
    let a = to_geo(h.ground_at(0, pane.center() - egui::vec2(140.0, 70.0)));
    let b = to_geo(h.ground_at(0, pane.center() + egui::vec2(140.0, 70.0)));
    h.make_pane_cross_section(1, a, b);
    h.gui_mut()
        .pane_mut(1)
        .expect("pane 1 exists")
        .cross_section_mut()
        .expect("pane 1 is a section pane")
        .source_pane = Some(0);
    h.place_section(1, vcp_212_axes(), &vcp_212_rungs());
    (h, a, b)
}

/// 45c. **Dragging an endpoint previews live and re-aims the section only on the
/// drop** — the pointer's whole journey changes nothing the cut dispatch can see,
/// and the release changes exactly the line.
#[test]
fn dragging_an_endpoint_re_aims_the_section_on_drop_and_never_mid_drag() {
    let (mut h, a, b) = harness_with_committed_section();
    let line_before = h.section_line(1).expect("the fixture committed a line");
    let centre_before = h.pane_center(0);

    let b_px = h.screen_of(0, b);
    let target_px = b_px + egui::vec2(-70.0, 45.0);
    let want = h.ground_at(0, target_px);

    h.mouse_move(b_px);
    h.frame();
    h.mouse_press(b_px);
    let pressed = h.frame();
    assert!(
        pressed.resolved.suppress_pan,
        "the press frame left the map free to pan out from under the grab"
    );

    for step in 1..=4 {
        h.mouse_move(b_px + (target_px - b_px) * (step as f32 / 4.0));
        h.frame();
        assert_eq!(
            h.section_line(1),
            Some(line_before),
            "the stored line moved mid-drag on step {step}: every one of \
                 these frames is a re-cut the dispatcher will run"
        );
    }
    assert_eq!(h.pane_center(0), centre_before, "the map panned mid-drag");

    h.mouse_release(target_px);
    h.frames_for(2, FRAME_DT);

    let line = h.section_line(1).expect("the drop lost the line");
    assert_ne!(line, line_before, "the drop committed nothing");
    assert_eq!(
        line.a(),
        line_before.a(),
        "grabbing B moved A: the drag is a redraw, not a handle"
    );
    assert!(
        (line.b().lat - want.y()).abs() < 1e-3 && (line.b().lon - want.x()).abs() < 1e-3,
        "B landed at {:?}, not under the drop at {want:?}",
        line.b()
    );
    assert!(
        (line.a().lat - a.lat).abs() < 1e-9,
        "A drifted from the ground the fixture named"
    );
    assert!(
        h.gui_mut()
            .pane(1)
            .and_then(|p| p.cross_section())
            .is_some_and(|s| s.texture.is_some() && s.section.is_some()),
        "the drop blanked the section pane"
    );
}

/// 45d. **A mid-drag zoom keeps the grabbed endpoint's ground.**
#[test]
fn a_mid_drag_zoom_keeps_the_grabbed_endpoints_ground() {
    fn drag(zoom_mid_drag: bool) -> (SectionLine, f64) {
        let (mut h, _, b) = harness_with_committed_section();
        let b_px = h.screen_of(0, b);
        let target_px = b_px + egui::vec2(-70.0, 45.0);

        h.mouse_move(b_px);
        h.frame();
        h.mouse_press(b_px);
        h.frame();
        h.mouse_move(target_px);
        h.frame();

        if zoom_mid_drag {
            h.wheel_notch(target_px, egui::MouseWheelUnit::Line, -3.0);
        }
        h.frames_for(6, FRAME_DT);

        h.mouse_release(target_px);
        h.frames_for(2, FRAME_DT);

        let zoom = h.gui_mut().pane(0).expect("pane 0").map_memory.zoom();
        (h.section_line(1).expect("the drop lost the line"), zoom)
    }

    let (plain, plain_zoom) = drag(false);
    let (zoomed, zoomed_zoom) = drag(true);

    assert!(
        (plain_zoom - zoomed_zoom).abs() > 0.05,
        "precondition: the wheel must really have zoomed ({plain_zoom} -> \
             {zoomed_zoom}), or the equality below proves nothing"
    );
    assert_eq!(
        plain.b(),
        zoomed.b(),
        "the zoom moved the grabbed endpoint: a stationary pointer is \
             being re-unprojected through the zoomed projector"
    );
    assert_eq!(plain.a(), zoomed.a(), "the fixed end moved under a zoom");
}

/// 45e. **A press beyond the grab radius is still a pan.**
#[test]
fn a_press_beside_the_handles_still_pans_the_map() {
    let (mut h, a, b) = harness_with_committed_section();
    let a_px = h.screen_of(0, a);
    let b_px = h.screen_of(0, b);
    let mid = a_px + (b_px - a_px) * 0.5;
    let start = mid + egui::vec2(0.0, -120.0);
    assert!(
        h.pane_rects()[0].contains(start),
        "precondition: the press is inside pane 0"
    );

    let centre_before = h.pane_center(0);
    h.mouse_move(start);
    h.frame();
    h.mouse_press(start);
    let pressed = h.frame();
    assert!(
        !pressed.resolved.suppress_pan,
        "a press {:.0} points from the nearest handle suppressed panning",
        (start - a_px).length().min((start - b_px).length())
    );
    for step in 1..=3 {
        h.mouse_move(start + egui::vec2(30.0 * step as f32, 15.0 * step as f32));
        h.frame();
    }
    h.mouse_release(start + egui::vec2(90.0, 45.0));
    h.frames_for(2, FRAME_DT);

    assert_ne!(
        h.pane_center(0),
        centre_before,
        "an ordinary pan near a section line went missing"
    );
    assert_eq!(
        h.section_line(1),
        Some(SectionLine::new(a, b).expect("the fixture's line")),
        "a pan rewrote the section's line"
    );
}

/// 45f. **While the draw mode is armed, the handles go inert** — the same press
/// that would grab B draws a fresh line instead, exactly as it did before handles
/// existed.
#[test]
fn an_armed_draw_wins_the_press_over_a_handle() {
    let (mut h, a, b) = harness_with_committed_section();
    h.set_section_draw_armed(true);
    h.warm_up();

    let b_px = h.screen_of(0, b);
    let to_px = b_px + egui::vec2(-160.0, 90.0);
    let want_from = h.ground_at(0, b_px);

    h.mouse_move(b_px);
    h.frame();
    h.mouse_press(b_px);
    h.frame();
    h.mouse_move(to_px);
    h.frame();
    h.mouse_release(to_px);
    h.frames_for(2, FRAME_DT);

    let line = h.section_line(1).expect("the armed draw re-aims pane 1");
    assert!(
        (line.a().lat - want_from.y()).abs() < 1e-3 && (line.a().lon - want_from.x()).abs() < 1e-3,
        "the press on a handle did not start a fresh armed line: A is at \
             {:?}, expected under the press at {want_from:?}",
        line.a()
    );
    assert!(
        (line.a().lat - a.lat).abs() > 1e-4 || (line.a().lon - a.lon).abs() > 1e-4,
        "precondition: the fixture's A and the press ground must differ, \
             or this cannot tell the two gestures apart"
    );
    assert!(
        !h.section_draw_armed(),
        "the armed draw did not disarm after producing its line"
    );
}

/// 45g. **Escape mid-drag cancels the edit and keeps the line** — the same layer
/// the armed drags sit on, because a drag in flight is the most immediate thing a
/// "back out" gesture can be aimed at.
#[test]
fn escape_mid_drag_cancels_the_edit_and_keeps_the_line() {
    let (mut h, _, b) = harness_with_committed_section();
    let line_before = h.section_line(1).expect("the fixture committed a line");
    let b_px = h.screen_of(0, b);

    h.mouse_move(b_px);
    h.frame();
    h.mouse_press(b_px);
    h.frame();
    h.mouse_move(b_px + egui::vec2(-60.0, 30.0));
    h.frame();

    assert!(
        h.gui_mut().dismiss_top_layer(),
        "a drag in flight gave the back gesture nothing to dismiss"
    );
    h.frame();
    h.mouse_release(b_px + egui::vec2(-80.0, 40.0));
    h.frames_for(2, FRAME_DT);

    assert_eq!(
        h.section_line(1),
        Some(line_before),
        "a cancelled drag still moved the line"
    );
}

/// 45h. **Dragging the line's body slides it rigidly** — length and bearing kept —
/// **previewing live and re-cutting only on the drop**, exactly like an endpoint
/// drag.
#[test]
fn a_body_drag_slides_the_line_rigidly_and_re_cuts_on_drop() {
    let (mut h, _, _) = harness_with_committed_section();
    let line_before = h.section_line(1).expect("the fixture committed a line");
    let mid_px = h.screen_of(0, crate::ui_section_edit::midpoint(line_before));
    let target_px = mid_px + egui::vec2(20.0, -60.0);

    h.mouse_move(mid_px);
    h.frame();
    h.mouse_press(mid_px);
    let pressed = h.frame();
    assert!(
        pressed.resolved.suppress_pan,
        "a press on the line's body left the map free to pan"
    );
    for step in 1..=4 {
        h.mouse_move(mid_px + (target_px - mid_px) * (step as f32 / 4.0));
        h.frame();
        assert_eq!(
            h.section_line(1),
            Some(line_before),
            "the stored line moved mid-drag on step {step}"
        );
    }
    h.mouse_release(target_px);
    h.frames_for(2, FRAME_DT);

    let line = h.section_line(1).expect("the drop lost the line");
    assert_ne!(line, line_before, "the body drag committed nothing");
    let (len_before, len_after) = (
        crate::ui_section_edit::length_km(line_before),
        crate::ui_section_edit::length_km(line),
    );
    assert!(
        (len_after - len_before).abs() < len_before * 0.01,
        "a body drag stretched the line: {len_before} km -> {len_after} km"
    );
    let (bearing_before, bearing_after) = (
        crate::ui_section_edit::bearing_deg(line_before),
        crate::ui_section_edit::bearing_deg(line),
    );
    assert!(
        (bearing_after - bearing_before).abs() < 0.5,
        "a body drag turned the line: {bearing_before}\u{b0} -> {bearing_after}\u{b0}"
    );
    assert_ne!(line.a(), line_before.a());
    assert_ne!(line.b(), line_before.b());
}

/// 45i. **A shift-drag on the body sweeps the line about its midpoint** — midpoint
/// and length kept, bearing following the pointer.
#[test]
fn a_shift_body_drag_sweeps_about_the_midpoint() {
    let (mut h, a, b) = harness_with_committed_section();
    let line_before = h.section_line(1).expect("the fixture committed a line");
    let mid_before = crate::ui_section_edit::midpoint(line_before);
    let (press_lat, press_lon) =
        squallar_geo::great_circle_point((a.lat, a.lon), (b.lat, b.lon), 0.75);
    let press_px = h.screen_of(
        0,
        GeoPoint {
            lat: press_lat,
            lon: press_lon,
        },
    );
    let release_ground = h.ground_at(0, press_px + egui::vec2(-40.0, -48.0));
    let (want_bearing, _) = squallar_geo::site_bearing_range_km(
        mid_before.lat,
        mid_before.lon,
        release_ground.y(),
        release_ground.x(),
    );

    h.set_modifiers(egui::Modifiers {
        shift: true,
        ..Default::default()
    });
    h.mouse_move(press_px);
    h.frame();
    h.mouse_press(press_px);
    h.frame();
    for step in 1..=4 {
        h.mouse_move(press_px + egui::vec2(-10.0, -12.0) * step as f32);
        h.frame();
    }
    h.mouse_release(press_px + egui::vec2(-40.0, -48.0));
    h.frames_for(2, FRAME_DT);
    h.set_modifiers(egui::Modifiers::default());

    let line = h.section_line(1).expect("the drop lost the line");
    assert_ne!(line, line_before, "the sweep committed nothing");
    let mid_after = crate::ui_section_edit::midpoint(line);
    assert!(
        (mid_after.lat - mid_before.lat).abs() < 1e-6
            && (mid_after.lon - mid_before.lon).abs() < 1e-6,
        "the sweep moved its own pivot: {mid_before:?} -> {mid_after:?}"
    );
    assert!(
        (crate::ui_section_edit::length_km(line) - crate::ui_section_edit::length_km(line_before))
            .abs()
            < 0.5,
        "the sweep changed the line's length"
    );
    assert!(
        (crate::ui_section_edit::bearing_deg(line)
            - crate::ui_section_edit::bearing_deg(line_before))
        .abs()
            > 2.0,
        "a drag across the line's run turned it by nothing"
    );
    let got = crate::ui_section_edit::bearing_deg(line).rem_euclid(360.0);
    let off = (got - want_bearing.rem_euclid(360.0)).rem_euclid(360.0);
    assert!(
        off.min(360.0 - off) < 3.0,
        "the grabbed point swept away from the pointer: the line's \
             bearing landed at {got}\u{b0}, the pointer sat on \
             {want_bearing}\u{b0} from the pivot"
    );
}

/// A single section pane with a rendered cut, for the step-control tests — the
/// layout a phone gets, where the chips are the only pan/sweep there is.
fn harness_with_section_pane() -> (InputHarness, SectionLine) {
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    h.load_scan("KTLX");
    let (a, b) = section_ends();
    h.make_pane_cross_section(0, a, b);
    h.place_section(0, vcp_212_axes(), &vcp_212_rungs());
    let line = h.section_line(0).expect("the fixture committed a line");
    (h, line)
}

/// The rect a control chip's glyph was painted in, inside `pane`.
fn chip_rect(h: &InputHarness, pane: egui::Rect, glyph: &str) -> egui::Rect {
    h.painted_text_rects()
        .into_iter()
        .find(|(rect, text)| text == glyph && pane.contains(rect.center()))
        .map(|(rect, _)| rect)
        .unwrap_or_else(|| {
            panic!(
                "no {glyph:?} chip on the section pane; painted {:?}",
                h.painted_text_strings_in(pane)
            )
        })
}

/// 45j. **The section pane's pan chips slide the line perpendicular to itself, one
/// step per click, keeping the picture** — and the pane says which way the line
/// faces while you do it.
#[test]
fn a_pan_step_on_the_section_pane_slides_the_line_and_keeps_the_picture() {
    let (mut h, line_before) = harness_with_section_pane();
    let pane = h.pane_rects()[0];

    let expected_readout = format!(
        "{:03}\u{b0} - {:.0}{}",
        (crate::ui_section_edit::bearing_deg(line_before)
            .rem_euclid(360.0)
            .round() as u32)
            % 360,
        squallar_units::UserPreferences::default()
            .distance
            .convert_from_km(crate::ui_section_edit::length_km(line_before)),
        squallar_units::UserPreferences::default().distance.suffix(),
    );
    assert!(
        h.text_painted_in(pane, &expected_readout),
        "the pane never says which way the line faces (wanted \
             {expected_readout:?}); it painted {:?}",
        h.painted_text_strings_in(pane)
    );

    h.mouse_click(chip_rect(&h, pane, "\u{23f4}").center());
    h.frame();

    let line = h.section_line(0).expect("the step lost the line");
    assert_ne!(line, line_before, "the pan chip moved nothing");
    assert!(
        (crate::ui_section_edit::length_km(line) - crate::ui_section_edit::length_km(line_before))
            .abs()
            < 0.1,
        "a pan step stretched the line"
    );
    assert!(
        (crate::ui_section_edit::bearing_deg(line)
            - crate::ui_section_edit::bearing_deg(line_before))
        .abs()
            < 0.1,
        "a pan step turned the line"
    );
    let mid_before = crate::ui_section_edit::midpoint(line_before);
    let mid_after = crate::ui_section_edit::midpoint(line);
    let (moved_bearing, moved_km) = squallar_geo::site_bearing_range_km(
        mid_before.lat,
        mid_before.lon,
        mid_after.lat,
        mid_after.lon,
    );
    let step = crate::ui_section_edit::pan_step_km(crate::ui_section_edit::length_km(line_before));
    assert!(
        (moved_km - step).abs() < step * 0.05,
        "one click moved the line {moved_km} km for a {step} km step"
    );
    let want_bearing = (crate::ui_section_edit::bearing_deg(line_before) - 90.0).rem_euclid(360.0);
    let off = (moved_bearing - want_bearing).rem_euclid(360.0);
    assert!(
        off.min(360.0 - off) < 1.0,
        "the ◀ chip moved the line on bearing {moved_bearing}\u{b0}, not \
             perpendicular-left at {want_bearing}\u{b0}"
    );
    assert!(
        h.gui_mut()
            .pane(0)
            .and_then(|p| p.cross_section())
            .is_some_and(|s| s.texture.is_some()),
        "a pan step blanked the pane"
    );
}

/// 45k. **The sweep chips rotate the line about its midpoint by one step** — the
/// fine-grained spelling of the sweep, and the only one a touch screen with no
/// modifier keys gets.
#[test]
fn a_sweep_step_on_the_section_pane_rotates_about_the_midpoint() {
    let (mut h, line_before) = harness_with_section_pane();
    let pane = h.pane_rects()[0];

    h.mouse_click(chip_rect(&h, pane, "\u{21bb}").center());
    h.frame();

    let line = h.section_line(0).expect("the step lost the line");
    let turned = (crate::ui_section_edit::bearing_deg(line)
        - crate::ui_section_edit::bearing_deg(line_before))
    .rem_euclid(360.0);
    assert!(
        (turned - crate::ui_section_edit::SWEEP_STEP_DEG).abs() < 0.1,
        "the ↻ chip turned the line {turned}\u{b0} for a \
             {}\u{b0} step",
        crate::ui_section_edit::SWEEP_STEP_DEG
    );
    let mid_before = crate::ui_section_edit::midpoint(line_before);
    let mid_after = crate::ui_section_edit::midpoint(line);
    assert!(
        (mid_after.lat - mid_before.lat).abs() < 1e-6
            && (mid_after.lon - mid_before.lon).abs() < 1e-6,
        "a sweep step moved the pivot"
    );
    assert!(
        (crate::ui_section_edit::length_km(line) - crate::ui_section_edit::length_km(line_before))
            .abs()
            < 0.1,
        "a sweep step changed the length"
    );
}

/// 45l. **The grab radii in absolute points: a press 30 points off a cap and 20
/// points off the body still pans.**
#[test]
fn a_press_thirty_points_off_a_cap_and_twenty_off_the_body_still_pans() {
    let (mut h, a, b) = harness_with_committed_section();
    let a_px = h.screen_of(0, a);
    let b_px = h.screen_of(0, b);
    let along = (b_px - a_px).normalized();
    let across = egui::vec2(along.y, -along.x);
    let start = a_px + along * (30.0f32.powi(2) - 20.0f32.powi(2)).sqrt() + across * 20.0;
    assert!(
        h.pane_rects()[0].contains(start),
        "precondition: the press is inside pane 0"
    );
    assert!(
        ((start - a_px).length() - 30.0).abs() < 0.1,
        "precondition: the press sits 30 points from the A cap"
    );

    let centre_before = h.pane_center(0);
    h.mouse_move(start);
    h.frame();
    h.mouse_press(start);
    let pressed = h.frame();
    assert!(
        !pressed.resolved.suppress_pan,
        "a press 30 points from the cap and 20 from the body suppressed \
             panning: a grab radius has grown into the map's pan gesture"
    );
    for step in 1..=3 {
        h.mouse_move(start + egui::vec2(30.0 * step as f32, 15.0 * step as f32));
        h.frame();
    }
    h.mouse_release(start + egui::vec2(90.0, 45.0));
    h.frames_for(2, FRAME_DT);

    assert_ne!(
        h.pane_center(0),
        centre_before,
        "an ordinary pan 30 points off a cap went missing"
    );
    assert_eq!(
        h.section_line(1),
        Some(SectionLine::new(a, b).expect("the fixture's line")),
        "a pan beside the line rewrote it"
    );
}

/// 45n. **Arming the section draw mid-flight kills the handle drag too** — the
/// other armed setter, making the same claim, pinned the same way.
#[test]
fn arming_the_section_draw_clears_a_handle_drag_in_flight() {
    let (mut h, a, b) = harness_with_committed_section();
    let b_px = h.screen_of(0, b);

    h.mouse_move(b_px);
    h.frame();
    h.mouse_press(b_px);
    h.frame();
    h.mouse_move(b_px + egui::vec2(-60.0, 30.0));
    h.frame();
    assert!(
        h.gui_mut().section_edit_drag_for_test().is_some(),
        "precondition: the press on the B cap began a drag"
    );

    h.set_section_draw_armed(true);
    assert!(
        h.gui_mut().section_edit_drag_for_test().is_none(),
        "arming the section draw left the handle drag alive: one drag on \
             one map pane would be two gestures"
    );

    h.frame();
    h.mouse_release(b_px + egui::vec2(-80.0, 40.0));
    h.frames_for(2, FRAME_DT);
    h.set_section_draw_armed(false);
    h.frames_for(2, FRAME_DT);
    assert_eq!(
        h.section_line(1),
        Some(SectionLine::new(a, b).expect("the fixture's line")),
        "a drag killed by the arming still moved the line"
    );
}

/// 57. **A tap while armed is discarded, and the mode stays armed.**
#[test]
fn a_tap_while_armed_draws_nothing_and_leaves_the_mode_armed() {
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    h.load_scan("KTLX");
    h.set_section_draw_armed(true);
    h.warm_up();

    let pane = h.pane_rects()[0];
    let at = pane.center();
    h.mouse_move(at);
    h.frame();
    h.mouse_press(at);
    h.frame();
    h.mouse_move(at + egui::vec2(9.0, 6.0));
    h.frame();
    h.mouse_release(at + egui::vec2(9.0, 6.0));
    h.frames_for(2, FRAME_DT);

    assert!(
        h.pane_kinds()
            .iter()
            .all(|k| *k == squallar_radar::types::RenderView::PlanView),
        "an 11-point drag became a cross-section"
    );
    assert!(
        h.section_draw_armed(),
        "a discarded drag disarmed the mode, throwing away the intent"
    );

    let to = at + egui::vec2(150.0, 90.0);
    h.mouse_press(at);
    h.frame();
    h.mouse_move(to);
    h.frame();
    h.mouse_release(to);
    h.frames_for(2, FRAME_DT);
    assert!(
        h.pane_kinds()
            .contains(&squallar_radar::types::RenderView::CrossSection),
        "the still-armed mode did not draw the next line"
    );
}

/// 58. **While armed, a press on a map fires no overlay click and the map does not
///     pan** — for every pane the frame resolves, not just the one the line is on.
#[test]
fn an_armed_press_suppresses_panning_and_fires_no_overlay_click() {
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    h.load_scan("KTLX");
    h.warm_up();

    let pane = h.pane_rects()[0];
    let at = pane.center();

    let unarmed = h.mouse_click(at);
    assert!(
        unarmed.resolved.overlay_click_pos.is_some(),
        "precondition: an unarmed click must reach the overlays"
    );
    assert!(!unarmed.resolved.suppress_pan, "precondition");

    h.set_section_draw_armed(true);
    h.warm_up();
    h.mouse_move(at);
    h.frame();
    let pressed = {
        h.mouse_press(at);
        h.frame()
    };
    assert_eq!(
        pressed.resolved.overlay_click_pos, None,
        "a press that starts a section line also opened an overlay popup \
             over the map being drawn on"
    );
    assert!(
        pressed.resolved.suppress_pan,
        "the map was left free to pan while a line was being drawn"
    );
    h.mouse_release(at);
    h.frames_for(2, FRAME_DT);
}

/// 59. **A pane that is not a map ignores the armed mode entirely.**
#[test]
fn arming_the_draw_changes_nothing_for_a_pane_with_no_map() {
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    h.set_pane_count(2);
    h.load_scan("KTLX");
    h.make_pane_unaimed_cross_section(0);
    h.set_section_draw_armed(true);
    h.warm_up();

    let at = h.pane_rects()[0].center();
    h.mouse_move(at);
    h.frame();
    h.mouse_press(at);
    let pressed = h.frame();
    assert!(
        !pressed.resolved.suppress_pan,
        "arming the draw suppressed panning on a pane that cannot be drawn on"
    );
    h.mouse_release(at);
    h.frames_for(2, FRAME_DT);
    assert_eq!(
        h.section_line(0),
        None,
        "a drag on a section pane aimed it at itself"
    );
    assert!(h.section_draw_armed(), "the mode should still be waiting");
}

/// **A back press cancels the armed modal drag.**
#[test]
fn a_back_press_cancels_the_armed_modal_drag() {
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    h.load_scan("KTLX");

    h.set_section_draw_armed(true);
    assert!(h.gui_mut().dismiss_top_layer(), "the draw was armed");
    assert!(!h.section_draw_armed());

    assert!(
        !h.gui_mut().dismiss_top_layer(),
        "with nothing left, a back press is a request to leave the app"
    );
}

/// A refusal is a decision only the user can reverse, wherever their platform keeps
/// it.
#[test]
fn settings_offers_no_way_to_ask_once_the_os_has_refused() {
    let mut h = InputHarness::new();
    h.open_settings();
    h.set_location_state(squallar_location::LocationPermission::Prompt, false);
    h.warm_up();
    assert!(
        h.painted_text_strings()
            .iter()
            .any(|t| t == "Use my location"),
        "control: an unasked platform must offer the button, or the \
             assertion below passes for free. Painted: {:?}",
        h.painted_text_strings()
    );

    h.set_location_state(squallar_location::LocationPermission::Denied, false);
    h.warm_up();

    let painted = h.painted_text_strings();
    assert!(
        !painted.iter().any(|t| t == "Use my location"),
        "the OS has refused and the pane still offers to ask it again. \
             Painted: {painted:?}"
    );
    assert!(
        painted.iter().any(|t| t == crate::ui::LOCATION_DENIED_NOTE),
        "a denial with no button and no explanation is the state this \
             whole feature exists to remove. Painted: {painted:?}"
    );
}

/// The control is a window onto the OS, not a switch with a memory.
#[test]
fn the_location_control_follows_the_os_rather_than_a_remembered_toggle() {
    let mut h = InputHarness::new();
    h.open_settings();
    h.set_location_state(squallar_location::LocationPermission::Granted, true);
    h.warm_up();
    let painted = h.painted_text_strings();
    assert!(
        painted.iter().any(|t| t == "Turn off"),
        "a live location stream offers no way to stop it. Painted: {painted:?}"
    );

    h.set_location_state(squallar_location::LocationPermission::Denied, false);
    h.warm_up();

    let painted = h.painted_text_strings();
    assert!(
        !painted.iter().any(|t| t == "Turn off"),
        "the permission was revoked and the pane still shows a live \
             stream. Painted: {painted:?}"
    );
    assert!(
        painted.iter().any(|t| t == "Denied."),
        "Painted: {painted:?}"
    );
}

/// A platform with no location service must not be told to go and enable one: the
/// advice leads nowhere and the button would do nothing.
#[test]
fn a_platform_without_location_is_told_so_and_offered_nothing() {
    let mut h = InputHarness::new();
    h.open_settings();
    h.set_location_state(squallar_location::LocationPermission::Unavailable, false);
    h.warm_up();

    let painted = h.painted_text_strings();
    assert!(
        painted.iter().any(|t| t.contains("Not available")),
        "Painted: {painted:?}"
    );
    for offered in ["Use my location", "Turn off"] {
        assert!(
            !painted.iter().any(|t| t == offered),
            "a platform with no location service is offering {offered:?}. \
                 Painted: {painted:?}"
        );
    }
}

/// The one thing worth offering after a refusal — and only where there is somewhere
/// to send the user.
#[test]
fn a_denial_offers_the_system_settings_page_only_where_there_is_one() {
    const BUTTON: &str = "Open location settings";

    let mut h = InputHarness::new();
    h.open_settings();
    h.set_location_state(squallar_location::LocationPermission::Denied, false);
    h.warm_up();

    let painted = h.painted_text_strings();
    assert!(
        !painted.iter().any(|t| t == BUTTON),
        "a platform that never claimed to have a settings page is offering \
             to open one. Painted: {painted:?}"
    );

    h.set_location_settings_available(true);
    h.warm_up();

    let painted = h.painted_text_strings();
    assert!(
        painted.iter().any(|t| t == BUTTON),
        "the OS refused, there is a page to send the user to, and the pane \
             does not offer it. Painted: {painted:?}"
    );

    for state in [
        squallar_location::LocationPermission::Granted,
        squallar_location::LocationPermission::Prompt,
        squallar_location::LocationPermission::Unknown,
        squallar_location::LocationPermission::Unavailable,
    ] {
        h.set_location_state(state, false);
        h.warm_up();
        let painted = h.painted_text_strings();
        assert!(
            !painted.iter().any(|t| t == BUTTON),
            "{state:?} is offering the remediation for a refusal. \
                 Painted: {painted:?}"
        );
    }
}

/// A refusal has to say something a user can act on, and on Linux the generic
/// sentence cannot: the switch that refused is xdg-desktop-portal's
/// `disable-location`, `xdg-desktop-portal-gtk` answers it from
/// `org.gnome.system.location enabled`, that key defaults to **false**, and no
/// desktop except GNOME has a page for it.
#[cfg(target_os = "linux")]
#[test]
fn a_linux_refusal_names_the_setting_that_would_undo_it() {
    let mut h = InputHarness::new();
    h.open_settings();
    h.set_location_state(squallar_location::LocationPermission::Denied, false);
    h.warm_up();

    let painted = h.painted_text_strings();
    let advice = painted
        .iter()
        .find(|t| t.contains("gsettings"))
        .unwrap_or_else(|| panic!("no advice a user could follow. Painted: {painted:?}"));
    assert!(
        advice.contains("org.gnome.system.location enabled true"),
        "the advice does not name the key or its value: {advice:?}"
    );
}

/// The gap the ungated line closes.
#[test]
fn a_granted_permission_with_no_fix_yet_says_so() {
    let mut h = InputHarness::new();
    h.open_settings();
    h.set_location_state(squallar_location::LocationPermission::Granted, true);
    h.warm_up();

    let painted = h.painted_text_strings();
    assert!(
        painted.iter().any(|t| t.contains("Waiting for a fix")),
        "location is on, no position has arrived, and the pane says only \
             'On.'. Painted: {painted:?}"
    );

    h.set_gps_fix(squallar_location::Fix::from_device_position(35.25, -97.5));
    h.warm_up();

    let painted = h.painted_text_strings();
    assert!(
        !painted.iter().any(|t| t.contains("Waiting for a fix")),
        "a fix arrived and the pane is still waiting for one. Painted: \
             {painted:?}"
    );
    assert!(
        painted.iter().any(|t| t.contains("Last fix")),
        "Painted: {painted:?}"
    );
}

/// **The site list is grouped by network, WSR-88D first, and the search spans
/// both groups.**
///
/// The grouping is presentation only: every radar the flat list offered is
/// still offered, still pickable, and what a pick persists is unchanged.
#[test]
fn the_site_list_groups_by_network_and_the_search_reaches_both() {
    use squallar_radar::sites::RadarNetwork;

    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    h.open_pane_props();

    let inspector = h.inspector();
    let networks: Vec<RadarNetwork> = inspector
        .site_rows
        .iter()
        .map(|(name, _, _)| RadarNetwork::of_id(name))
        .collect();

    // Non-triviality: both groups must be populated, or "grouped" is a claim
    // about a list with one kind in it and the ordering below cannot fail.
    assert!(
        networks.contains(&RadarNetwork::Wsr88d) && networks.contains(&RadarNetwork::Tdwr),
        "the harness table must offer both networks: {networks:?}",
    );

    let first_tdwr = networks
        .iter()
        .position(|n| *n == RadarNetwork::Tdwr)
        .expect("a terminal radar is drawn");
    assert!(
        networks[first_tdwr..]
            .iter()
            .all(|n| *n == RadarNetwork::Tdwr),
        "a WSR-88D is drawn below a TDWR, so the list is not grouped: {networks:?}",
    );

    // Nothing was dropped on the way into the groups.
    let total = squallar_radar::sites::radars().len() + squallar_radar::sites::unplaced().len();
    assert_eq!(
        inspector.site_rows.len(),
        total,
        "the grouping lost a radar"
    );

    // The search spans both groups: a terminal radar is reachable by typing.
    h.mouse_click(inspector.site_search.center());
    h.type_text("tokc");
    h.warm_up();
    let inspector = h.inspector();
    assert_eq!(
        inspector
            .site_rows
            .iter()
            .map(|(code, _, _)| code.as_str())
            .collect::<Vec<_>>(),
        vec!["TOKC"],
        "the search must reach the TDWR group",
    );
}

/// 69. **The site search narrows the list, highlights the current site, and a row
///     click switches the pane's site.**
#[test]
fn the_site_search_narrows_the_list_and_a_row_click_switches_the_site() {
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    h.open_pane_props();

    let inspector = h.inspector();
    let placed = squallar_radar::sites::radars().len();
    let unplaced = squallar_radar::sites::unplaced();
    let total = placed + unplaced.len();
    assert!(!unplaced.is_empty(), "the harness lists one radar unplaced");
    assert_eq!(
        inspector.site_rows.len(),
        total,
        "the unfiltered list must offer every radar this process knows of"
    );
    for name in unplaced {
        assert!(
            inspector.site_rows.iter().any(|(row, ..)| row == name),
            "{name} has no position and must still be pickable: its data is \
             fetched by identifier, and the volume then places it",
        );
    }
    // The caption counts RADARS, not rows: the shortcut sections above the
    // groups repeat some of them, so a bare "N shown" would disagree with what
    // a reader can count on screen. Both halves of the ratio are radars.
    assert!(
        inspector
            .site_caption
            .starts_with(&format!("{total} of {total} radars")),
        "the caption must count what is shown against a named total; drew {:?}",
        inspector.site_caption
    );
    let highlighted: Vec<&str> = inspector
        .site_rows
        .iter()
        .filter(|(_, _, current)| *current)
        .map(|(code, _, _)| code.as_str())
        .collect();
    assert_eq!(
        highlighted,
        vec!["KTLX"],
        "exactly the pane's current site is highlighted"
    );

    h.mouse_click(inspector.site_search.center());
    h.type_text("kmkx");
    h.warm_up();
    let inspector = h.inspector();
    assert_eq!(
        inspector
            .site_rows
            .iter()
            .map(|(code, _, _)| code.as_str())
            .collect::<Vec<_>>(),
        vec!["KMKX"],
        "the filter must narrow to the match"
    );
    assert!(
        inspector
            .site_caption
            .starts_with(&format!("1 of {total} radars")),
        "the caption must follow the filter; drew {:?}",
        inspector.site_caption
    );

    h.mouse_click(inspector.site_rows[0].1.center());
    assert!(
        h.last_actions().iter().any(|a| matches!(
            a,
            crate::actions::GuiAction::SwitchRadarSite { site, pane_idx: 0 } if site == "KMKX"
        )),
        "clicking the row did not emit SwitchRadarSite for the active pane"
    );
}

/// **A site list that is not the network yet does not state a total.**
#[test]
fn a_site_list_still_short_of_the_network_states_no_total() {
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    h.open_pane_props();

    let settled = h.inspector().site_caption;
    let total = squallar_radar::sites::radars().len() + squallar_radar::sites::unplaced().len();
    assert!(
        settled.contains(&format!("of {total} radars")),
        "precondition: a settled list states its total; drew {settled:?}",
    );

    h.set_catalogue_pending(true);
    let pending = h.inspector().site_caption;
    assert!(
        pending.starts_with(&format!("{total} radars so far")),
        "it still says what it is showing; drew {pending:?}",
    );
    assert!(
        !pending.contains("sites") && !pending.contains("NEXRAD"),
        "but it must claim no total and no split — that is the claim about \
         the network nothing has made yet; drew {pending:?}",
    );
    assert!(
        pending.contains("still finding the network"),
        "and it must say why; drew {pending:?}",
    );
}

/// 70. **An unlinked pane is excluded from shared time — the loop fan-out and the
///     sync pass's time pair — and the Pane-properties sync section mirrors the
///     popover: the same five rows, its time checkbox reflecting and toggling.**
#[test]
fn an_unlinked_pane_is_excluded_from_shared_nav_and_loop_fan_out() {
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    h.set_pane_count(3);
    h.load_scan("KTLX");

    h.open_pane_props();
    assert_eq!(
        h.inspector()
            .sync_rows
            .iter()
            .map(|(label, _, _)| label.clone())
            .collect::<Vec<_>>(),
        vec![
            "Group".to_owned(),
            "Sync viewport".to_owned(),
            "Sync layers".to_owned(),
            "Sync time".to_owned(),
            "Match all panes to this view".to_owned(),
            "Re-link this group here".to_owned(),
            "Unlink this group".to_owned(),
        ],
        "the Pane-properties sync section must carry the popover's rows"
    );

    let time_row = |h: &mut InputHarness| {
        h.inspector()
            .sync_rows
            .iter()
            .find(|(label, _, _)| label == "Sync time")
            .map(|&(_, rect, on)| (rect, on))
            .expect("a multi-pane layout draws the sync section")
    };
    let (link, on) = time_row(&mut h);
    assert!(on, "a fresh pane starts linked");
    h.mouse_click(link.center());
    h.warm_up();
    assert!(
        !h.gui_mut().pane(0).expect("pane 0").time_link,
        "the click must unlink the pane"
    );
    let (link, on) = time_row(&mut h);
    assert!(!on, "the checkbox must reflect the stored state");
    h.mouse_click(link.center());
    h.warm_up();
    assert!(
        h.gui_mut().pane(0).expect("pane 0").time_link,
        "a second click must relink it"
    );

    h.gui_mut().pane_mut(1).expect("pane 1").time_link = false;
    h.warm_up();

    h.mouse_click(h.timeline().loop_toggle.0.center());
    let targets: Vec<usize> = h
        .last_actions()
        .iter()
        .filter_map(|a| match a {
            crate::actions::GuiAction::EnableLoop { pane_idx, .. } => Some(*pane_idx),
            _ => None,
        })
        .collect();
    assert_eq!(
        targets,
        vec![0, 2],
        "the loop must fan out over the linked panes and only them"
    );

    {
        let gui = h.gui_mut();
        gui.pane_mut(1).expect("pane 1").viewing_live = false;
        gui.pane_mut(1).expect("pane 1").time.step = crate::pane::TimeStep::from_secs(0);
        gui.pane_mut(2).expect("pane 2").viewing_live = false;
    }
    h.warm_up();
    let gui = h.gui_mut();
    assert!(
        gui.pane(2).expect("pane 2").viewing_live,
        "the linked pane must be dragged back to the active pane's live state"
    );
    assert!(
        !gui.pane(1).expect("pane 1").viewing_live,
        "the unlinked pane must stay frozen"
    );
    assert_eq!(
        gui.pane(1).expect("pane 1").time.step.as_secs(),
        0,
        "the unlinked pane's step must stay its own"
    );
    assert_eq!(
        gui.pane(1).expect("pane 1").site(),
        "KTLX",
        "unlink is about time: every other synced field still converges"
    );

    // **The gap this pin used to have.** It covered `viewing_live` and
    // `time.step` but never the *moment on display* — the field the render
    // reads to find its volume, and the one an archive delivery overwrote on
    // every same-site pane regardless of the link. That is why the defect
    // `UNLINK_NOTE` describes survived this test. See
    // `ui::scan_info_audience_tests` for the delivery rule itself.
    let parked = gui
        .pane(1)
        .and_then(|p| p.scan_info.as_ref().map(|i| i.timestamp))
        .expect("load_scan put a volume on every pane");
    let scrubbed_to = parked - chrono::Duration::minutes(20);
    let mut arrival = gui
        .pane(0)
        .and_then(|p| p.scan_info.clone())
        .expect("load_scan put a volume on every pane");
    arrival.timestamp = scrubbed_to;
    gui.apply(crate::shell_api::GuiEvent::ScanInfoForTimeGroup {
        site: "KTLX".to_owned(),
        requester: 0,
        info: arrival,
    });
    let gui = h.gui_mut();
    assert_eq!(
        gui.pane(0)
            .and_then(|p| p.scan_info.as_ref().map(|i| i.timestamp)),
        Some(scrubbed_to),
        "the pane that scrubbed must show what it scrubbed to",
    );
    assert_eq!(
        gui.pane(1)
            .and_then(|p| p.scan_info.as_ref().map(|i| i.timestamp)),
        Some(parked),
        "the unlinked pane was dragged to the scrubbing pane's moment; \
         `UNLINK_NOTE` promises it holds its own",
    );
    assert_eq!(
        gui.pane(2)
            .and_then(|p| p.scan_info.as_ref().map(|i| i.timestamp)),
        Some(scrubbed_to),
        "and the linked pane must still follow",
    );
}

/// **A keyboard nudge on the archive scrubber commits** (§5.9 carried finding:
/// `changed()` without a drag used to store the position and wait for a release
/// that never comes).
#[test]
fn a_keyboard_nudge_on_the_archive_scrubber_commits() {
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    h.load_scan("KTLX");
    h.gui_mut().pane_mut(0).expect("pane 0").viewing_live = false;
    h.warm_up();

    let scrubber_id = h
        .widget_id_probes()
        .into_iter()
        .find(|(name, _)| *name == "timeline_scrubber")
        .expect("the scrubber must report its id")
        .1;
    h.focus_widget(scrubber_id);

    for _ in 0..20 {
        h.key_press(egui::Key::ArrowLeft);
    }
    h.frame();

    assert!(
        h.last_actions().iter().any(|a| matches!(
            a,
            crate::actions::GuiAction::NavigateTime { pane_idx: 0, .. }
        )),
        "the keyboard nudge must commit a navigation, not park an in-flight \
         drag position forever; actions: none matching NavigateTime"
    );
}

/// 67a. **The catalog's search filters every group, and a product tile aims the
/// active pane.**
#[test]
fn the_catalog_search_filters_and_a_product_tile_aims_the_active_pane() {
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    h.open_catalog();

    let catalog = h.catalog();
    for group in [
        crate::ui::CatalogGroup::Presets,
        crate::ui::CatalogGroup::Layers,
        crate::ui::CatalogGroup::Fields(squallar_radar::fields::GROUP),
        crate::ui::CatalogGroup::Fields(squallar_overlays::hrrr::fields::GROUP),
    ] {
        assert!(
            catalog.tiles.iter().any(|tile| tile.group == group),
            "{group:?} drew no tiles on the unfiltered view"
        );
    }
    let unfiltered = catalog.tiles.len();

    h.mouse_click(catalog.search.center());
    h.type_text("spectrum");
    h.warm_up();
    let filtered = h.catalog().tiles;
    assert!(
        !filtered.is_empty() && filtered.len() < unfiltered,
        "the query must narrow the catalog ({} of {unfiltered} left)",
        filtered.len()
    );
    assert!(
        filtered
            .iter()
            .all(|tile| tile.label.to_lowercase().contains("spectrum")),
        "a tile that does not match survived the filter: {filtered:?}"
    );

    let tile = h
        .catalog_tile(
            crate::ui::CatalogGroup::Fields(squallar_radar::fields::GROUP),
            "Spectrum Width",
        )
        .expect("the product tile survives its own name as the query");
    h.mouse_click(tile.rect.center());
    h.warm_up();

    assert!(!h.catalog().open, "applying a tile must close the catalog");
    let pane = h.gui_mut().pane(0).expect("pane 0");
    assert_eq!(
        pane.selected_product(),
        radar_fields::known::SPECTRUM_WIDTH,
        "the tile did not set the active pane's product"
    );
    assert_eq!(
        pane.selected_elevation(),
        0.0,
        "the old product's tilt must not survive the switch"
    );
    assert!(
        h.overlay_enabled_on(0, &known::RADAR),
        "a product under a hidden radar layer is a click that did nothing"
    );
    assert_eq!(
        h.inspector().mode,
        Some(crate::ui::InspectorSelection::Layer(known::RADAR)),
        "the Radar layer's options must be selected"
    );
}

/// 67b. **An overlay tile enables the layer — with the shared enable-fetch rule —
/// selects it, and closes the catalog.**
#[test]
fn an_overlay_tile_enables_the_layer_and_selects_it() {
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    assert!(
        !h.overlay_enabled_on(0, &known::SPC_OUTLOOK),
        "precondition: outlooks start off, so the tile has something to do"
    );

    h.open_catalog();
    let tile = h
        .catalog_tile(crate::ui::CatalogGroup::Layers, "SPC Outlooks")
        .expect("the overlays group offers SPC Outlooks");
    h.mouse_click(tile.rect.center());
    assert!(
        h.last_actions().iter().any(|a| matches!(
            a,
            crate::actions::GuiAction::FetchOverlay { kind, pane_idx: 0 }
                if *kind == known::SPC_OUTLOOK
        )),
        "enabling a dataless, never-polled layer must queue its first fetch"
    );
    h.warm_up();

    assert!(!h.catalog().open, "applying a tile must close the catalog");
    assert!(
        h.overlay_enabled_on(0, &known::SPC_OUTLOOK),
        "the tile did not enable the layer"
    );
    assert_eq!(
        h.inspector().mode,
        Some(crate::ui::InspectorSelection::Layer(known::SPC_OUTLOOK)),
        "the enabled layer's options must be selected"
    );
}

/// **A catalog tile brings its stack row into view** — the other half of
/// "nothing is ever added".
///
/// Every pane holds a row for every registered layer, always, so a tile turns
/// one on rather than creating one. That only reads as true if the row it
/// refers to is where the user can see it when the modal closes; otherwise the
/// visible result of "Show a layer" is a panel that did not obviously change.
#[test]
fn a_catalog_tile_scrolls_the_stack_to_the_row_it_turned_on() {
    // Short enough that the stack cannot draw its whole inventory at once.
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 460.0));
    // **A stack the user has filled from the catalog.** A curated stack
    // starts at the handful of layers that ship enabled, and this test's
    // subject is a LONG list - the panel's scroll, its clamped height, the
    // ids a scrolled body keeps. Built the way a user builds one, rather
    // than relied on as a property of the build's layer count.
    h.fill_stack();
    h.open_layers();

    let panel = h.layers_panel_rect().expect("the stack was just opened");
    // Out of the *panel*, not off the screen: a scrolled-past row still lays
    // out at a screen coordinate, it is simply outside the viewport that
    // clips it.
    let off_screen = |h: &InputHarness| {
        h.stack_row(&known::SPC_OUTLOOK).is_some_and(|row| {
            h.layers_panel_rect()
                .is_none_or(|panel| !panel.contains(row.rect.center()))
        })
    };
    // Drive the list to the far end from the target row, and insist it got
    // there — without this precondition the assertion below cannot fail.
    let scrolled = h.scroll_until(panel.center(), egui::vec2(0.0, 120.0), 60, off_screen)
        || h.scroll_until(panel.center(), egui::vec2(0.0, -120.0), 60, off_screen);
    assert!(
        scrolled,
        "the stack drew its whole inventory on a 460 pt screen, so this test \
         cannot tell a scroll from a no-op"
    );

    h.open_catalog();
    assert!(
        off_screen(&h),
        "opening the catalog must not have scrolled the stack on its own"
    );
    let tile = h
        .catalog_tile(crate::ui::CatalogGroup::Layers, "SPC Outlooks")
        .expect("the overlays group offers SPC Outlooks");
    h.mouse_click(tile.rect.center());
    // Real frames with real dt: egui animates a `scroll_to_me`, and
    // `warm_up`'s three zero-dt frames would never let the animation run.
    h.frames_for(30, 1.0 / 60.0);

    let row = h
        .stack_row(&known::SPC_OUTLOOK)
        .expect("the layer the tile turned on still has its row");
    let panel = h.layers_panel_rect().expect("the stack is still open");
    assert!(
        panel.contains(row.rect.center()),
        "the tile left its row outside the panel: row {:?}, panel {panel:?} - \
         the catalog and the stack must visibly name the same thing",
        row.rect
    );
    assert!(row.selected, "and the row it scrolled to reads as selected");
}

/// 67c. **An HRRR tile enables the model layer and sets the parameter through the
/// handler's own control route.**
#[test]
fn an_hrrr_tile_enables_the_model_layer_and_sets_the_parameter() {
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    h.open_catalog();

    let tile = h
        .catalog_tile(
            crate::ui::CatalogGroup::Fields(squallar_overlays::hrrr::fields::GROUP),
            "Surface-Based CAPE",
        )
        .expect("the HRRR group offers the parameter");
    h.mouse_click(tile.rect.center());
    assert!(
        h.last_actions().iter().any(|a| matches!(
            a,
            crate::actions::GuiAction::FetchOverlay { kind, .. }
                if *kind == known::MODEL_DATA
        )),
        "an uncached parameter must ask for its data"
    );
    h.warm_up();

    assert!(!h.catalog().open);
    assert!(
        h.overlay_enabled_on(0, &known::MODEL_DATA),
        "the tile did not enable the model layer"
    );
    assert_eq!(
        h.inspector().mode,
        Some(crate::ui::InspectorSelection::Layer(known::MODEL_DATA)),
        "the model layer's options must be selected"
    );
    let (_, selected) = h
        .dropdown_model("Parameter")
        .expect("the model layer's body offers the parameter dropdown");
    assert_eq!(
        selected, "sbcape",
        "the tile's parameter must be the one selected"
    );
}

/// **Presets: saving captures the view, the tile appears, applying reproduces the
/// capture, deleting removes it** (§3.11).
#[test]
fn a_saved_preset_appears_applies_and_deletes() {
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    h.load_scan("KTLX");
    h.set_layer_links(false);

    h.set_pane_count(2);
    h.gui_mut()
        .pane_mut(0)
        .expect("pane 0")
        .set_selected_product(radar_fields::known::VELOCITY);
    h.set_overlay_on_pane(0, &known::STORM_REPORTS, true);
    h.warm_up();

    h.open_catalog();
    h.mouse_click(h.catalog().save_tile.center());
    h.warm_up();
    let field = h.catalog().save_field.expect("the name editor opens");
    h.mouse_click(field.center());
    h.type_text("Chase day");
    h.warm_up();
    let save = h.catalog().save_button.expect("the Save button is drawn");
    h.mouse_click(save.center());
    h.warm_up();

    let tile = h
        .catalog_tile(crate::ui::CatalogGroup::Presets, "Chase day")
        .expect("the saved preset must appear as a tile");
    assert!(
        tile.delete.is_some(),
        "a user tile carries its delete button"
    );
    assert!(
        h.catalog_tile(crate::ui::CatalogGroup::Presets, "Severe Wx")
            .expect("the built-ins stay")
            .delete
            .is_none(),
        "a built-in tile must offer no delete"
    );

    h.set_pane_count(1);
    h.gui_mut()
        .pane_mut(0)
        .expect("pane 0")
        .set_selected_product(radar_fields::known::REFLECTIVITY);
    h.set_overlay_on_pane(0, &known::STORM_REPORTS, false);
    h.warm_up();
    let tile = h
        .catalog_tile(crate::ui::CatalogGroup::Presets, "Chase day")
        .expect("still offered");
    h.mouse_click(tile.rect.center());
    h.warm_up();

    assert!(
        !h.catalog().open,
        "applying a preset must close the catalog"
    );
    assert_eq!(h.pane_count(), 2, "the preset's pane count must come back");
    assert_eq!(
        h.gui_mut().pane(0).expect("pane 0").selected_product(),
        radar_fields::known::VELOCITY,
        "the preset's per-pane product must come back"
    );
    assert!(
        h.overlay_enabled_on(0, &known::STORM_REPORTS)
            && h.overlay_enabled_on(1, &known::STORM_REPORTS),
        "the preset's overlay set must land on every pane"
    );

    h.open_catalog();
    let tile = h
        .catalog_tile(crate::ui::CatalogGroup::Presets, "Chase day")
        .expect("still offered");
    h.mouse_click(tile.delete.expect("a user tile").center());
    h.warm_up();
    assert!(
        h.catalog_tile(crate::ui::CatalogGroup::Presets, "Chase day")
            .is_none(),
        "the deleted preset must vanish from the catalog"
    );
    assert!(
        h.gui_mut().presets_for_test().is_empty(),
        "and from the store the config writer persists"
    );
}

/// **Escape closes the catalog before anything beneath it** — the §3.4 slot, as
/// amended: after the ☰ dropdown, before the feature and time dialogs.
#[test]
fn a_back_press_closes_the_catalog_first() {
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    h.open_catalog();
    h.gui_mut().set_time_dialog_open_for_test(true);
    h.warm_up();

    assert!(h.gui_mut().dismiss_top_layer(), "something was open");
    h.warm_up();
    assert!(
        !h.catalog().open,
        "the first dismissal must take the catalog"
    );

    assert!(h.gui_mut().dismiss_top_layer(), "the dialog is still open");
    h.warm_up();
    assert!(
        !h.text_painted_in(h.screen_rect(), "Select Time"),
        "the second dismissal must take the time dialog"
    );
}

/// **The Data & live rows and the ☰ menu toggles read one field** — flipping either
/// side moves the other, because there is only one thing to move.
#[test]
fn the_data_and_live_rows_share_state_with_the_menu_toggles() {
    /// How far inside the inspector's own edges the label has to sit before
    /// the click is the user's click: the body's scroll area is inset from
    /// the panel and its clip ends short of the panel's bottom.
    const CHROME_EDGE_CLEARANCE: f32 = 24.0;

    fn radar_auto_poll(h: &mut InputHarness) -> bool {
        let json = h.gui_mut().ui_config_json().expect("serialises");
        let value: serde_json::Value = serde_json::from_str(&json).expect("parses");
        value["overlay_states"]["Radar"]["auto_poll"]
            .as_bool()
            .expect("a bool")
    }

    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    h.open_settings();
    assert!(radar_auto_poll(&mut h), "precondition: auto-poll starts on");

    let inspector = h.inspector_rect().expect("the inspector is open");
    let scroll_pos = inspector.center();
    // **Scrolled until the label is where a user could hit it**, not merely
    // until it is somewhere on the screen: the inspector's body is a scroll
    // area with its own clip, and a row painted past its bottom edge is drawn
    // but not clickable. Asking for the label *inside the inspector's own
    // rect, clear of its edges* is what makes the click below the user's
    // click rather than a coordinate that happens to work at one row count.
    let visible = |h: &InputHarness| {
        h.painted_text_rects().iter().any(|(rect, text)| {
            text == "Auto-poll"
                && inspector
                    .shrink(CHROME_EDGE_CLEARANCE)
                    .contains(rect.center())
        })
    };
    let found = h.scroll_until(scroll_pos, egui::vec2(0.0, -60.0), 200, visible);
    assert!(found, "the Auto-poll checkbox never became clickable");
    h.frames_for(10, 0.05);
    let label = h
        .painted_text_rects()
        .into_iter()
        .find(|(_, text)| text == "Auto-poll")
        .expect("the checkbox label is painted")
        .0;
    h.mouse_click(label.center());
    h.warm_up();
    assert!(
        !radar_auto_poll(&mut h),
        "the settings checkbox must write the flag the menu reads"
    );

    h.open_menu();
    let leaf = h.menu_leaf("Auto-poll").expect("the menu still offers it");
    assert_eq!(
        leaf.value,
        Some(false),
        "the menu's checkbox must reflect the settings row's write"
    );

    h.mouse_click(leaf.rect.center());
    h.warm_up();
    assert!(
        radar_auto_poll(&mut h),
        "the menu toggle must write the same field back"
    );
}

/// **The Lookback and Speed sliders say they reach every pane** — the one
/// thing about them that was never on screen.
///
/// `set_loop_span_secs` and `set_loop_speed_fps` write every pane, unlinked
/// ones included, while the transport two rows down honours the links. The
/// asymmetry is deliberate (one window, one number) and unchanged here; the
/// pin is that the sliders now say so, and that a pane with its time link off
/// still takes the number.
#[test]
fn the_loop_tuning_sliders_say_they_apply_to_every_pane() {
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    h.mouse_click(h.timeline().expander.center());
    h.warm_up();

    let row2 = h.timeline().row2.expect("the expander must open row 2");
    assert!(
        row2.tuning_scope.contains("every pane"),
        "the sliders must name their reach; drew {:?}",
        row2.tuning_scope
    );
    assert!(
        h.text_painted_in(h.screen_rect(), &row2.tuning_scope),
        "the caption is a probe string that never reached the glass"
    );
    let caption_below_sliders = row2.lookback.bottom() <= row2.speed.bottom() + 1.0;
    assert!(
        caption_below_sliders,
        "the two sliders share a row, so one caption serves both"
    );

    // And the claim is true: an unlinked pane takes the number anyway.
    h.set_pane_count(2);
    h.gui_mut().pane_mut(1).expect("pane 1 exists").time_link = false;
    h.gui_mut().set_loop_span_secs(900);
    assert_eq!(
        h.gui_mut().pane(1).expect("pane 1 exists").time.span_secs,
        900,
        "the caption would be a lie: the unlinked pane did not take the span"
    );
}

/// **Row 2's closing caption states this platform's frame budget and the unlink
/// hint** (§5.9 carried into M4) — the number is the frontend's push
/// (`set_loop_frame_budget`), never a guess from the width.
#[test]
fn the_timeline_row2_caption_states_the_pushed_frame_budget() {
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    h.set_loop_frame_budget(12);
    h.mouse_click(h.timeline().expander.center());
    h.warm_up();

    let row2 = h.timeline().row2.expect("the expander must open row 2");
    assert!(
        row2.caption.contains("up to 12 frames"),
        "the caption must state the pushed budget; drew {:?}",
        row2.caption
    );
    assert!(
        row2.caption.contains("\"Sync time\""),
        "the caption must carry the per-pane unlink hint, by the toggle's \
         own name (the Sync popover's and the inspector checkbox's); drew {:?}",
        row2.caption
    );
    assert!(
        !row2.caption.contains("This loop"),
        "with no loop running there is no span to state; drew {:?}",
        row2.caption
    );
}

/// **The caption states the running loop's own time span, ahead of the standing
/// budget** — the whole point being that a frame count says nothing about how much
/// weather a user is actually looking at.
#[test]
fn the_timeline_row2_caption_states_the_running_loops_span_and_fidelity() {
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    {
        let pane = h.gui_mut().pane_mut(0).unwrap();
        *pane.time_state_mut(&known::RADAR) = crate::radar_layer::begin_loop(
            3600,
            squallar_radar::sites::get_radar_site("KTLX").unwrap(),
            squallar_radar::types::RenderView::PlanView,
        );
        pane.time_state_mut(&known::RADAR).phase = crate::pane::LoopPhase::Playing;
        pane.time_state_mut(&known::RADAR).sampled = Some(false);
        pane.time_state_mut(&known::RADAR).cadence_secs = Some(259);
        let base = written_ago(60);
        pane.time_state_mut(&known::RADAR).frames = (0..14)
            .map(|i| crate::pane::LoopFrame {
                timestamp: base + chrono::Duration::seconds(i * 259),
                image: None,
                render_in_flight: false,
                render_failed: false,
            })
            .collect();
        pane.park_on_frame(&known::RADAR, 13);
    }
    h.mouse_click(h.timeline().expander.center());
    h.warm_up();

    let row2 = h.timeline().row2.expect("the expander must open row 2");
    assert!(
        row2.caption
            .starts_with("This loop spans 56 min over 14 frames, every scan, ~4 min apart - "),
        "the span leads the caption, and states fidelity as well as extent; drew {:?}",
        row2.caption
    );
    assert!(
        row2.caption.contains("Loops keep up to"),
        "the standing budget sentence still follows it; drew {:?}",
        row2.caption
    );
}

use crate::ui::PillKind;

/// A wide two-pane harness with the layers panel closed, so pane 0's pill row is
/// not under the floating stack — the state every pill test drives from.
fn pill_harness() -> InputHarness {
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    h.set_pane_count(2);
    h.close_layers();
    h
}

/// 73a. **A click on a pill is never a map click — and its popover opens anchored
/// to the pill.**
#[test]
fn a_pill_click_never_reaches_the_map_and_its_popover_anchors() {
    let mut h = pill_harness();
    h.gui_mut().enable_overlay_for_test(&known::RADAR_SITES);
    h.warm_up();

    h.mouse_click(h.pane_rects()[1].center());
    assert_eq!(h.active_pane_index(), 1, "precondition: pane 1 is active");

    let (text, pill) = h.pill(0, PillKind::Site).expect("pane 0 draws a site pill");
    assert_eq!(text, "KTLX", "the pill names the pane's site");
    h.place_site_at(0, "KTLX", pill.center());
    assert!(
        h.is_floating_layer_at(pill.center()),
        "precondition: the pill row is a floating layer over the map — that \
         is the whole blocking mechanism under test"
    );

    let outcome = h.mouse_click(pill.center());
    assert_eq!(
        site_switches(&h),
        vec![],
        "the icon under the pill answered a click the pill row should have \
         blocked"
    );
    assert!(
        outcome.resolved.overlay_click_pos.is_none(),
        "the click still reached the map's resolved pointer frame"
    );
    assert!(
        !h.click_consumed(),
        "nothing on the map may consume a click that never reached it"
    );
    assert_eq!(
        h.active_pane_index(),
        0,
        "the pill's own activate is the one side effect a pill click has"
    );

    let popover = h.pill_popover().expect("the site pill's popover opened");
    assert_eq!((popover.pane_idx, popover.pill), (0, PillKind::Site));
    assert!(
        (popover.rect.top() - pill.bottom()).abs() < 24.0
            && popover.rect.left() < pill.right()
            && pill.left() < popover.rect.right(),
        "the popover is not anchored to its pill: pill {pill:?}, popover {:?}",
        popover.rect
    );
    assert!(
        popover.search.is_some(),
        "the site popover leads with its search field"
    );
}

/// 73b. **A dim row still hit-tests: on touch the first tap reveals and is
/// swallowed, the second acts — and a confirmed map tap elsewhere puts the row back
/// to sleep.**
#[test]
fn a_dim_rows_first_touch_tap_reveals_and_swallows() {
    let mut h = pill_harness();

    h.touch_tap(h.pane_rects()[1].center());
    h.frames_for(10, 0.05);
    assert_eq!(h.active_pane_index(), 1, "precondition: pane 1 is active");

    let row = h.pill_row(0).expect("pane 0 draws a pill row");
    assert!(
        !row.full_opacity,
        "precondition: with no pointer hover on touch, the row idles dim"
    );

    let (_, pill) = h.pill(0, PillKind::Site).expect("the site pill is drawn");
    h.touch_tap(pill.center());
    h.frames_for(10, 0.05);

    assert!(
        h.pill_row(0).expect("still drawn").full_opacity,
        "the first tap on a dim row must reveal it"
    );
    assert!(
        h.pill_popover().is_none(),
        "the revealing tap is swallowed: no popover may open on it"
    );
    assert_eq!(
        h.active_pane_index(),
        1,
        "the revealing tap is swallowed: it must not activate the pane either"
    );

    let (_, pill) = h.pill(0, PillKind::Site).expect("still drawn");
    h.touch_tap(pill.center());
    h.frames_for(2, 0.05);
    assert_eq!(h.active_pane_index(), 0, "the second tap activates");
    let popover = h.pill_popover().expect("the second tap opens the popover");
    assert_eq!((popover.pane_idx, popover.pill), (0, PillKind::Site));

    let map_spot = h.pane_rects()[0].center();
    h.touch_tap(map_spot);
    h.frames_for(12, 0.05);
    assert!(
        h.pill_popover().is_none(),
        "a tap outside the popover closes it"
    );
    assert!(
        !h.pill_row(0).expect("still drawn").full_opacity,
        "a confirmed map tap elsewhere must put the revealed row back to sleep"
    );
}

/// 73c. **"Pin pane controls" forces the rows to full opacity — through the real
/// settings row, and persisted.**
#[test]
fn pin_pane_controls_forces_full_opacity_and_persists() {
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    h.close_layers();

    h.mouse_move(egui::pos2(700.0, 12.0));
    h.frames_for(2, FRAME_DT);
    assert!(
        !h.pill_row(0)
            .expect("the pane draws a pill row")
            .full_opacity,
        "precondition: unpinned and unhovered, the row idles dim"
    );

    h.open_settings();
    let scroll_pos = h.inspector_rect().expect("the inspector is open").center();
    let found = h.scroll_until(scroll_pos, egui::vec2(0.0, -160.0), 120, |h| {
        h.settings_row("interface.pin_controls")
            .is_some_and(|row| h.screen_rect().contains(row.rect.center()))
    });
    assert!(found, "the Interface row never scrolled on screen");
    h.frames_for(10, 0.05);
    let found = h.scroll_until(scroll_pos, egui::vec2(0.0, -40.0), 40, |h| {
        h.painted_text_rects()
            .iter()
            .any(|(_, text)| text == "Pin pane controls")
    });
    assert!(found, "the Pin pane controls checkbox never became visible");
    h.frames_for(10, 0.05);
    let label = h
        .painted_text_rects()
        .into_iter()
        .find(|(_, text)| text == "Pin pane controls")
        .expect("the checkbox label is painted")
        .0;
    h.mouse_click(label.center());
    h.close_inspector();

    h.mouse_move(egui::pos2(700.0, 12.0));
    h.frames_for(2, FRAME_DT);
    assert!(
        h.pill_row(0).expect("still drawn").full_opacity,
        "pinned, the row must draw at full opacity with no hover at all"
    );

    let json = h.gui_mut().ui_config_json().expect("serialises");
    let value: serde_json::Value = serde_json::from_str(&json).expect("parses");
    assert_eq!(
        value["pin_pane_controls"].as_bool(),
        Some(true),
        "the pin must be written to the config"
    );
}

/// 73d. **The site pill's popover searches the one site list and a pick emits the
/// map icon's own `SwitchRadarSite`.**
#[test]
fn the_site_pill_popover_searches_and_switches() {
    let mut h = pill_harness();

    let (_, pill) = h.pill(0, PillKind::Site).expect("the site pill is drawn");
    h.mouse_click(pill.center());
    h.frame();
    let popover = h.pill_popover().expect("the popover opened");
    let search = popover.search.expect("with its search field");
    assert_eq!(
        popover.rows.len(),
        squallar_radar::sites::radars().len() + squallar_radar::sites::unplaced().len(),
        "unfiltered, the popover offers every radar this process knows of, \
         placed or not — the inspector's own list"
    );

    h.mouse_click(search.center());
    h.type_text("kmkx");
    h.warm_up();
    let popover = h.pill_popover().expect("still open");
    assert_eq!(
        popover
            .rows
            .iter()
            .map(|(code, _, _)| code.as_str())
            .collect::<Vec<_>>(),
        vec!["KMKX"],
        "the filter must narrow to the match"
    );

    h.mouse_click(popover.rows[0].1.center());
    assert!(
        site_switches(&h).contains(&("KMKX".to_owned(), 0)),
        "the pick did not emit SwitchRadarSite for the pill's pane; got {:?}",
        site_switches(&h)
    );
    h.warm_up();
    assert!(h.pill_popover().is_none(), "a pick closes the popover");
}

/// 73e. **The product and tilt popovers offer the combos' own lists, and a pick
/// writes the pane — with the product pick resetting the tilt.**
#[test]
fn the_product_and_tilt_pill_popovers_write_the_pane() {
    let mut h = pill_harness();
    h.load_scan("KTLX");
    h.offer_product(0, &radar_fields::known::REFLECTIVITY, 0.5);
    h.offer_product(0, &radar_fields::known::REFLECTIVITY, 1.5);
    h.close_layers();

    let (code, pill) = h.pill(0, PillKind::Product).expect("a product pill");
    assert_eq!(code, "REF", "the pill shows the product code");
    h.gui_mut()
        .pane_mut(0)
        .expect("pane 0")
        .set_selected_elevation(1.5);
    h.mouse_click(pill.center());
    h.frame(); // the popup's debut frame only registers it
    let popover = h.pill_popover().expect("the popover opened");
    assert_eq!(
        popover
            .rows
            .iter()
            .map(|(label, _, _)| label.as_str())
            .collect::<Vec<_>>(),
        vec!["Reflectivity", "Velocity"],
        "the popover offers the scan's own products — the combo's list"
    );
    let velocity = popover.rows[1].1;
    h.mouse_click(velocity.center());
    h.warm_up();
    {
        let pane = h.gui_mut().pane(0).expect("pane 0");
        assert_eq!(
            pane.selected_product(),
            radar_fields::known::VELOCITY,
            "the pick did not set the pane's product"
        );
        assert_eq!(
            pane.selected_elevation(),
            0.0,
            "the old product's tilt must not survive the switch"
        );
    }

    h.select_product(0, &radar_fields::known::REFLECTIVITY);
    let (_, pill) = h
        .pill(0, PillKind::Tilt)
        .expect("a map pane draws a tilt pill");
    h.mouse_click(pill.center());
    h.frame(); // the popup's debut frame only registers it
    let popover = h.pill_popover().expect("the popover opened");
    assert_eq!(
        popover
            .rows
            .iter()
            .map(|(label, _, _)| label.as_str())
            .collect::<Vec<_>>(),
        vec!["0.5\u{b0}", "1.5\u{b0}"],
        "the popover offers the product's own tilts — the combo's list"
    );
    h.mouse_click(popover.rows[1].1.center());
    h.warm_up();
    assert_eq!(
        h.gui_mut().pane(0).expect("pane 0").selected_elevation(),
        1.5,
        "the pick did not set the pane's tilt"
    );
}

/// One sync row out of a popover probe, by its label. **By label, never by
/// index**: the section's row list is the thing these tests are about, so a
/// row added to it must not silently move every click one row down.
fn popover_row(popover: &crate::ui::PillPopoverProbe, label: &str) -> egui::Rect {
    popover
        .rows
        .iter()
        .find(|(drawn, _, _)| drawn == label)
        .unwrap_or_else(|| panic!("the popover drew no {label:?} row: {:?}", popover.rows))
        .1
}

/// 73f. **The Sync pill's popover is the per-pane five-row section: the three link
/// checkboxes flip this pane's own fields, the two action rows ride under them, and
/// the honest unlink sentence is on screen.**
#[test]
fn the_sync_pill_popover_flips_all_three_per_pane_links() {
    let mut h = pill_harness();
    assert!(
        h.all_layer_linked(),
        "precondition: every pane's links default on"
    );

    let (label, pill) = h.pill(0, PillKind::Link).expect("a Sync pill");
    assert_eq!(
        label, "Sync A",
        "a fresh pane's pill reads Sync and names the group it is in"
    );
    h.mouse_click(pill.center());
    h.frame(); // the popup's debut frame only registers it
    let popover = h.pill_popover().expect("the popover opened");
    assert_eq!(
        popover
            .rows
            .iter()
            .map(|(label, _, _)| label.as_str())
            .collect::<Vec<_>>(),
        vec![
            "Group",
            "Sync viewport",
            "Sync layers",
            "Sync time",
            "Match all panes to this view",
            "Re-link this group here",
            "Unlink this group",
        ],
        "the per-pane section's honest labels, group first"
    );
    assert!(
        h.painted_text_strings()
            .iter()
            .any(|t| t.contains("still follows new scans")),
        "the popover must carry the honest unlink caption"
    );

    h.mouse_click(popover_row(&popover, "Sync viewport").center());
    h.frame();
    {
        let gui = h.gui_mut();
        assert!(
            !gui.pane(0).expect("pane 0").viewport_link,
            "the Sync viewport toggle did not unlink this pane's viewport"
        );
        assert!(
            gui.pane(1).expect("pane 1").viewport_link,
            "the toggle is per-pane: pane 1's viewport link must not move"
        );
    }
    let (label, _) = h.pill(0, PillKind::Link).expect("still drawn");
    assert_eq!(
        label, "\u{2297} Sync A",
        "the pill must mark an unlinked viewport distinctly, without losing \
         the group it is still in"
    );

    let popover = h.pill_popover().expect("the popover stays up for toggles");
    h.mouse_click(popover_row(&popover, "Sync layers").center());
    h.frame();
    {
        let gui = h.gui_mut();
        assert!(
            !gui.pane(0).expect("pane 0").layer_link,
            "the Sync layers toggle did not unlink this pane's layers"
        );
        assert!(
            gui.pane(1).expect("pane 1").layer_link,
            "the toggle is per-pane: pane 1's layer link must not move"
        );
    }

    let popover = h.pill_popover().expect("still up");
    h.mouse_click(popover_row(&popover, "Sync time").center());
    h.warm_up();
    {
        let gui = h.gui_mut();
        assert!(
            !gui.pane(0).expect("pane 0").time_link,
            "the Sync time toggle did not unlink the pane"
        );
        assert!(
            gui.pane(1).expect("pane 1").time_link,
            "the toggle is per-pane: pane 1's time link must not move"
        );
    }
    let (label, _) = h.pill(0, PillKind::Link).expect("still drawn");
    assert_eq!(
        label, "\u{2297} Sync A",
        "the pill must keep marking the unlinked state distinctly"
    );
}

/// 73i. **A pane in the 3D view is offered no viewport link, at either route — and
/// a plan-view pane still is.**
#[test]
fn a_3d_pane_is_offered_no_viewport_link_at_either_route() {
    /// The sync section's rows as the inspector's Pane-properties body drew them,
    /// labels only.
    fn inspector_rows(h: &mut InputHarness) -> Vec<String> {
        h.open_pane_props();
        h.inspector()
            .sync_rows
            .iter()
            .map(|(label, _, _)| label.clone())
            .collect()
    }

    /// The same section as the Sync pill's popover drew it.
    fn popover_rows(h: &mut InputHarness) -> Vec<String> {
        h.close_inspector();
        h.close_layers();
        let (_, pill) = h.pill(0, PillKind::Link).expect("pane 0's Sync pill");
        h.mouse_click(pill.center());
        h.frame(); // the popup's debut frame only registers it
        let rows = h
            .pill_popover()
            .expect("the popover opened")
            .rows
            .iter()
            .map(|(label, _, _)| label.clone())
            .collect();
        h.key_press(egui::Key::Escape);
        h.warm_up();
        rows
    }

    let [_group, viewport, layers, time, _, _, _] =
        crate::ui::SYNC_SECTION_LABELS.map(ToOwned::to_owned);

    let mut h = pill_harness();

    assert!(
        popover_rows(&mut h).contains(&viewport),
        "precondition: a map pane's Sync popover offers the viewport link, \
         or the absence below is about a section that offers nobody anything"
    );
    assert!(
        inspector_rows(&mut h).contains(&viewport),
        "precondition: a map pane's inspector offers the viewport link"
    );

    h.gui_mut().pane_mut(0).expect("pane 0").viewport_link = false;
    h.warm_up();

    h.make_pane_volume(0);
    for (route, rows) in [
        ("the Sync pill's popover", popover_rows(&mut h)),
        ("the inspector's Pane properties", inspector_rows(&mut h)),
    ] {
        assert!(
            !rows.contains(&viewport),
            "{route} offered a 3D pane the viewport link, which nothing on \
             either end of `sync_viewports` would honour: {rows:?}"
        );
        assert!(
            rows.contains(&layers) && rows.contains(&time),
            "{route}: a 3D pane sits out the viewport dimension, not the \
             group — the other two links must still be on offer: {rows:?}"
        );
    }
    h.close_inspector();
    h.close_layers();
    h.warm_up();

    let (label, _) = h.pill(0, PillKind::Link).expect("still drawn");
    assert_eq!(
        label, "Sync A",
        "the pill marked a 3D pane unlinked over a viewport link it is not \
         offered and does not take part in"
    );

    assert!(
        !h.gui_mut().pane(0).expect("pane 0").viewport_link,
        "the conversion cleared the stored link instead of leaving it inert"
    );
    h.make_pane_map(0);
    assert!(
        inspector_rows(&mut h).contains(&viewport),
        "the row must come back with the plan view"
    );
    assert!(
        !h.gui_mut().pane(0).expect("pane 0").viewport_link,
        "the round trip through 3D silently re-linked a pane the user had \
         unlinked — a reopen must be the screen that was left"
    );
}

/// 73h. **The popover's two action rows do exactly what they say: match-all copies
/// this pane's viewport to every map pane and leaves the links alone; re-link-all
/// makes that copy and turns all three links back on for every visible pane.**
#[test]
fn the_sync_popover_action_rows_match_and_relink_the_grid() {
    let moved_to = 7.0;
    let mut h = pill_harness();

    {
        let gui = h.gui_mut();
        let pane = gui.pane_mut(0).expect("pane 0");
        pane.viewport_link = false;
        pane.layer_link = false;
        pane.time_link = false;
    }
    h.warm_up();

    {
        let gui = h.gui_mut();
        gui.pane_mut(1)
            .expect("pane 1")
            .map_memory
            .set_zoom(moved_to)
            .expect("the test zoom must be in walkers' accepted range");
        assert_ne!(
            gui.pane(0).expect("pane 0").map_memory.zoom(),
            moved_to,
            "precondition: the panes must disagree, or the copy is invisible"
        );
    }

    let (_, pill) = h.pill(1, PillKind::Link).expect("pane 1's Sync pill");
    h.mouse_click(pill.center());
    h.frame(); // the popup's debut frame only registers it
    let popover = h.pill_popover().expect("the popover opened");
    assert_eq!(popover.pane_idx, 1, "precondition: pane 1's popover");
    h.mouse_click(popover_row(&popover, "Match all panes to this view").center());
    h.frame();
    {
        let gui = h.gui_mut();
        assert_eq!(
            gui.pane(0).expect("pane 0").map_memory.zoom(),
            moved_to,
            "match-all must copy the viewport to every map pane, linked or \
                 not"
        );
        let pane0 = gui.pane(0).expect("pane 0");
        assert!(
            !pane0.viewport_link && !pane0.layer_link && !pane0.time_link,
            "match-all must leave every link exactly as it was"
        );
    }

    h.warm_up();
    h.gui_mut()
        .pane_mut(0)
        .expect("pane 0")
        .map_memory
        .set_zoom(4.0)
        .expect("in range");
    let (_, pill) = h.pill(1, PillKind::Link).expect("pane 1's Sync pill");
    h.mouse_click(pill.center());
    h.frame();
    let popover = h.pill_popover().expect("the popover opened");
    h.mouse_click(popover_row(&popover, "Re-link this group here").center());
    h.frame();
    {
        let gui = h.gui_mut();
        let pane0 = gui.pane(0).expect("pane 0");
        assert!(
            pane0.viewport_link && pane0.layer_link && pane0.time_link,
            "re-link-all must turn all three links back on for every pane"
        );
        assert_eq!(
            pane0.map_memory.zoom(),
            moved_to,
            "re-link-all must also make the match-all copy"
        );
        let pane1 = gui.pane(1).expect("pane 1");
        assert!(
            pane1.viewport_link && pane1.layer_link && pane1.time_link,
            "the popover's own pane relinks too"
        );
    }
    assert_eq!(
        h.active_pane_index(),
        1,
        "re-link-all makes its pane the group's reference: everything came \
             home to it"
    );
}

/// 73g. **The kind pill's popover converts through the deferred applier — pending
/// on the pick frame, converted the next — and choosing an unaimed cross-section
/// arms the draw, matching the inspector.**
#[test]
fn the_kind_pill_popover_converts_next_frame_and_arms_the_unaimed_section() {
    let mut h = pill_harness();

    let (label, pill) = h.pill(0, PillKind::Kind).expect("a kind pill");
    assert_eq!(label, "Map");
    h.mouse_click(pill.center());
    h.frame(); // the popup's debut frame only registers it
    let popover = h.pill_popover().expect("the popover opened");
    assert_eq!(
        popover
            .rows
            .iter()
            .map(|(label, _, _)| label.as_str())
            .collect::<Vec<_>>(),
        vec!["Map", "3D Volume", "Cross-section"],
        "the popover offers the inspector's own three kinds"
    );

    h.mouse_click(popover.rows[1].1.center());
    assert_eq!(
        h.gui_mut().pending_pane_view_for_test(),
        Some((0, squallar_radar::types::RenderView::Volume)),
        "the pick must go through the deferred applier"
    );
    assert_eq!(
        h.pane_kinds()[0],
        squallar_radar::types::RenderView::PlanView,
        "…and not convert mid-frame"
    );
    h.frame();
    assert_eq!(
        h.pane_kinds()[0],
        squallar_radar::types::RenderView::Volume,
        "the applier must convert on the next frame"
    );
    let (label, _) = h.pill(0, PillKind::Kind).expect("still drawn");
    assert_eq!(label, "3D Volume", "the pill must follow the conversion");
    assert!(
        h.pill(0, PillKind::Tilt).is_none(),
        "a non-map pane offers no tilt pill"
    );

    assert!(
        !h.section_draw_armed(),
        "precondition: the draw starts unarmed"
    );
    let (_, pill) = h.pill(0, PillKind::Kind).expect("still drawn");
    h.mouse_click(pill.center());
    h.frame(); // the popup's debut frame only registers it
    let popover = h.pill_popover().expect("the popover opened");
    h.mouse_click(popover.rows[2].1.center());
    h.warm_up();
    assert_eq!(
        h.pane_kinds()[0],
        squallar_radar::types::RenderView::CrossSection
    );
    assert!(
        h.section_draw_armed(),
        "choosing an unaimed cross-section must arm the draw"
    );
}

/// **The armed-tool hint chip sits on the active map pane, and only there.**
#[test]
fn the_armed_hint_chip_follows_the_active_map_pane() {
    let mut h = pill_harness();
    let panes = h.pane_rects();

    let (section_toggle, armed) = h.top_bar().section_arm;
    assert!(!armed, "precondition: the section draw starts unarmed");
    h.mouse_click(section_toggle.center());
    h.warm_up();

    let hint = crate::ui::map::SECTION_ARM_HINT.to_owned();
    assert!(
        h.text_painted_in(panes[0], &hint),
        "the active map pane must paint the section hint; painted {:?}",
        h.painted_text_strings_in(panes[0])
    );
    assert!(
        !h.text_painted_in(panes[1], &hint),
        "an inactive pane must not paint the chip"
    );

    h.mouse_click(panes[1].center());
    h.warm_up();
    assert!(!h.text_painted_in(panes[0], &hint));
    assert!(h.text_painted_in(panes[1], &hint));

    let (section_toggle, _) = h.top_bar().section_arm;
    h.mouse_click(section_toggle.center());
    h.warm_up();
    assert!(
        !h.text_painted_in(panes[1], crate::ui::map::SECTION_ARM_HINT),
        "the chip must vanish when the arm does"
    );

    h.make_pane_volume(1);
    h.mouse_click(h.top_bar().section_arm.0.center());
    h.warm_up();
    assert!(
        !h.text_painted_in(panes[1], &hint),
        "a volume pane must not promise a drag it cannot host"
    );
}

/// **The `click_consumed` probe: a feature that answers a map click sets it; a
/// click on bare map does not.**
#[test]
fn a_consumed_map_click_reports_itself_and_a_bare_one_does_not() {
    let mut h = InputHarness::new();
    h.close_layers();
    h.gui_mut().enable_overlay_for_test(&known::RADAR_SITES);
    h.warm_up();

    let pane = h.pane_rects()[0];
    let spot = egui::pos2(pane.center().x + 150.0, pane.center().y);
    h.place_site_at(0, "KTLX", spot);
    h.mouse_click(spot);
    assert!(
        site_switches(&h).contains(&("KTLX".to_owned(), 0)),
        "control: the icon really is under the click — without this the \
         assertion below is vacuous"
    );
    assert!(
        h.click_consumed(),
        "a site icon that answered the click must report the consumption"
    );

    h.set_overlay_on_pane(0, &known::RADAR_SITES, false);
    h.mouse_click(spot);
    assert_eq!(site_switches(&h), vec![], "control: nothing answers now");
    assert!(
        !h.click_consumed(),
        "a click that fell through to the bare map must not read as consumed"
    );
}

/// **Saving a user preset under a built-in's name is refused, with the reason
/// inline** (§5.9 carried from the M4 review).
#[test]
fn a_user_preset_cannot_shadow_a_builtin_name() {
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    h.open_catalog();
    h.mouse_click(h.catalog().save_tile.center());
    h.warm_up();
    let field = h.catalog().save_field.expect("the name editor opens");
    h.mouse_click(field.center());
    h.type_text("Severe Wx");
    h.warm_up();

    assert!(
        h.painted_text_strings()
            .iter()
            .any(|t| t.contains("is a built-in preset")),
        "the refusal must be explained inline; painted {:?}",
        h.painted_text_strings()
    );
    let save = h.catalog().save_button.expect("the Save button is drawn");
    h.mouse_click(save.center());
    h.warm_up();
    assert!(
        h.gui_mut().presets_for_test().is_empty(),
        "the shadowing preset must not be stored"
    );
    let severe: Vec<_> = h
        .catalog()
        .tiles
        .iter()
        .filter(|tile| tile.label == "Severe Wx")
        .cloned()
        .collect();
    assert_eq!(severe.len(), 1, "exactly the built-in tile remains");
    assert!(
        severe[0].delete.is_none(),
        "and it is the undeletable built-in"
    );
}

/// **The save tile hides while the search is filtering** (§5.9 pinned rule): the
/// search is for finding tiles, and a save offer matching the query would be the
/// one tile that is not a result.
#[test]
fn the_save_tile_hides_while_searching() {
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    h.open_catalog();
    assert!(
        h.catalog().save_tile.is_positive(),
        "precondition: the unfiltered view offers the save tile"
    );
    h.mouse_click(h.catalog().save_tile.center());
    h.warm_up();
    assert!(h.catalog().save_field.is_some(), "the editor opened");

    h.mouse_click(h.catalog().search.center());
    h.type_text("sev");
    h.warm_up();
    let catalog = h.catalog();
    assert!(
        !catalog.save_tile.is_positive(),
        "the save tile must hide while a query filters"
    );
    assert!(
        catalog.save_field.is_none(),
        "and the open name editor hides with it"
    );
    assert!(
        catalog.tiles.iter().any(|tile| tile.label == "Severe Wx"),
        "control: the query still finds the built-in, so the hide is about \
         the save tile and not the group"
    );
}

/// **Applying a preset queues at most one fetch per overlay kind** (§5.9 pinned
/// rule): the handlers are global, so one fetch serves every pane the preset
/// enabled a layer on — four panes must not mean four downloads.
#[test]
fn a_preset_apply_queues_one_fetch_per_kind() {
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    h.open_catalog();
    let tile = h
        .catalog_tile(crate::ui::CatalogGroup::Presets, "Severe Wx")
        .expect("the built-in is offered");
    h.mouse_click(tile.rect.center());

    let mut fetched: Vec<LayerId> = Vec::new();
    for action in h.last_actions() {
        if let GuiAction::FetchOverlay { kind, .. } = action {
            assert!(
                !fetched.contains(kind),
                "{kind:?} was fetched twice by one preset apply"
            );
            fetched.push(kind.clone());
        }
    }
    assert!(
        fetched.contains(&known::SPC_OUTLOOK),
        "control: the preset enables a dataless, never-polled layer, so \
         exactly one fetch for it must be queued; got {fetched:?}"
    );
    assert_eq!(h.pane_count(), 4, "control: the preset really fanned out");
}

/// **A mid-session pane growth leaves the open stack above every pill row.**
#[test]
fn a_pane_growth_keeps_the_open_stack_above_the_pill_rows() {
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    // **A stack the user has filled from the catalog.** A curated stack
    // starts at the handful of layers that ship enabled, and this test's
    // subject is a LONG list - the panel's scroll, its clamped height, the
    // ids a scrolled body keeps. Built the way a user builds one, rather
    // than relied on as a property of the build's layer count.
    h.fill_stack();
    h.set_pane_count(2);
    h.open_layers();
    h.warm_up();
    let stack = h.layers_panel_rect().expect("the stack is open");
    let row0 = h.pill_row(0).expect("pane 0 draws a pill row").rect;
    let startup = stack.intersect(row0);
    assert!(
        startup.is_positive(),
        "precondition: the stack floats across pane 0's corner"
    );
    assert_eq!(
        h.top_layer_id_at(startup.center()),
        Some(egui::Id::new("layers_panel")),
        "control: the startup raise already holds the stack above row 0"
    );

    let four = h
        .pane_options()
        .iter()
        .find(|o| o.count == 4)
        .expect("the Panes segment offers 4")
        .rect;
    h.mouse_click(four.center());
    h.warm_up();
    assert_eq!(h.pane_count(), 4, "precondition: the grid really grew");

    let stack = h.layers_panel_rect().expect("the stack is still open");
    for row in h.pill_rows() {
        let overlap = stack.intersect(row.rect);
        if !overlap.is_positive() {
            continue;
        }
        assert_eq!(
            h.top_layer_id_at(overlap.center()),
            Some(egui::Id::new("layers_panel")),
            "pane {}'s pill row surfaced above the open stack",
            row.pane_idx
        );
    }
    let row2 = h.pill_row(2).expect("pane 2 draws a pill row").rect;
    assert!(
        stack.intersect(row2).is_positive(),
        "precondition: pane 2's debuting row overlaps the stack — without \
         this the loop above asserted nothing about a debut"
    );
}

/// **The same growth leaves the open inspector above the debuting row.**
#[test]
fn a_pane_growth_keeps_the_open_inspector_above_the_pill_rows() {
    let mut h = InputHarness::with_screen(egui::vec2(1020.0, 900.0));
    h.set_pane_count(1);
    h.open_settings();
    h.warm_up();

    let two = h
        .pane_options()
        .iter()
        .find(|o| o.count == 2)
        .expect("the Panes segment offers 2")
        .rect;
    h.mouse_click(two.center());
    h.warm_up();
    assert_eq!(h.pane_count(), 2, "precondition: the grid really grew");

    let insp = h.inspector_rect().expect("the inspector stayed open");
    let row1 = h.pill_row(1).expect("pane 1 draws a pill row").rect;
    let overlap = insp.intersect(row1);
    assert!(
        overlap.is_positive(),
        "precondition: pane 1's debuting row overlaps the inspector — \
         without this the assertion below says nothing"
    );
    assert_eq!(
        h.top_layer_id_at(overlap.center()),
        Some(egui::Id::new("inspector_panel")),
        "pane 1's pill row surfaced above the open inspector"
    );
}

/// **Saving under an existing user preset's name replaces it, whatever the casing**
/// — the same case-insensitivity the built-in refusal keeps, and for the same
/// reason: "storm" and "Storm" would be two tiles a glance cannot tell apart.
#[test]
fn saving_a_preset_under_an_existing_name_replaces_it_case_insensitively() {
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    let save = |h: &mut InputHarness, name: &str| {
        h.open_catalog();
        if h.catalog().save_field.is_none() {
            h.mouse_click(h.catalog().save_tile.center());
            h.warm_up();
        }
        let field = h.catalog().save_field.expect("the name editor opens");
        h.mouse_click(field.center());
        h.type_text(name);
        h.warm_up();
        let button = h.catalog().save_button.expect("the Save button is drawn");
        h.mouse_click(button.center());
        h.warm_up();
    };

    save(&mut h, "storm");
    assert_eq!(
        h.gui_mut()
            .presets_for_test()
            .iter()
            .map(|p| p.name.clone())
            .collect::<Vec<_>>(),
        vec!["storm".to_owned()],
        "precondition: the first save stored one preset"
    );

    save(&mut h, "Storm");
    assert_eq!(
        h.gui_mut()
            .presets_for_test()
            .iter()
            .map(|p| p.name.clone())
            .collect::<Vec<_>>(),
        vec!["Storm".to_owned()],
        "resaving under the same name in another case must replace, not \
         duplicate \u{2014} and the tile takes the newly typed casing"
    );
    let tiles: Vec<_> = h
        .catalog()
        .tiles
        .iter()
        .filter(|tile| tile.label.eq_ignore_ascii_case("storm"))
        .cloned()
        .collect();
    assert_eq!(tiles.len(), 1, "exactly one tile carries the name");
}

/// A phone-sized harness: the Compact shell with the bottom bar and the sheet.
fn phone() -> InputHarness {
    let h = InputHarness::with_screen(egui::vec2(420.0, 1400.0));
    assert_eq!(
        h.width_class(),
        crate::ui_layout::WidthClass::Compact,
        "precondition: the phone shell only exists below 600pt"
    );
    h
}

/// An overlay item whose details page is a fixed stub — how a test opens the
/// sheet's Feature page without staging a real alert under a map click.
#[derive(Debug)]
struct SheetStubFeature;

impl squallar_overlays::render::overlay_state::OverlayItem for SheetStubFeature {
    fn layer_id(&self) -> squallar_source::id::LayerId {
        squallar_source::id::known::NWS_ALERTS
    }
    fn popup_content(
        &self,
        _prefs: &squallar_units::UserPreferences,
    ) -> squallar_overlays::render::overlay_state::PopupContent {
        squallar_overlays::render::overlay_state::PopupContent {
            title: "Stub feature".to_owned(),
            accent_rgb: [200, 60, 60],
            width: 300.0,
            sections: vec![
                squallar_overlays::render::overlay_state::PopupSection::Text(
                    "stub body".to_owned(),
                ),
            ],
            actions: Vec::new(),
        }
    }
    fn matches(&self, _other: &dyn squallar_overlays::render::overlay_state::OverlayItem) -> bool {
        false
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

/// 64. **A bar item tap switches to its page; the shown page's item closes the
///     sheet whole.**
#[test]
fn a_bar_item_switches_pages_and_the_shown_pages_item_closes_the_sheet() {
    let mut h = phone();
    assert_eq!(h.sheet().page, None, "a fresh session's sheet is closed");

    h.mouse_click(h.bottom_bar().layers.0.center());
    h.warm_up();
    assert_eq!(h.sheet().page, Some(crate::ui::SheetPage::Layers));
    assert!(
        h.bottom_bar().layers.1,
        "the open page's item must highlight"
    );

    h.mouse_click(h.bottom_bar().pane.0.center());
    h.warm_up();
    assert_eq!(h.sheet().page, Some(crate::ui::SheetPage::Inspector));
    assert_eq!(
        h.inspector().mode,
        Some(crate::ui::InspectorSelection::PaneProps),
        "the Pane item must assert the pane-properties body"
    );
    assert!(
        h.bottom_bar().pane.1 && !h.bottom_bar().layers.1,
        "the highlight must follow the page on top"
    );

    h.mouse_click(h.bottom_bar().pane.0.center());
    h.warm_up();
    assert_eq!(
        h.sheet().page,
        None,
        "the shown page's item must close the sheet whole - falling through \
         to the Layers page is the second user test's exact bug"
    );

    h.mouse_click(h.bottom_bar().pane.0.center());
    h.warm_up();
    h.mouse_click(h.bottom_bar().app.0.center());
    h.warm_up();
    assert_eq!(h.sheet().page, Some(crate::ui::SheetPage::Inspector));
    assert_eq!(
        h.inspector().mode,
        Some(crate::ui::InspectorSelection::AppSettings),
        "the App item must assert the settings body"
    );
    assert!(
        h.bottom_bar().app.1 && !h.bottom_bar().pane.1,
        "same page, but the highlight follows the selection"
    );

    h.mouse_click(h.bottom_bar().app.0.center());
    h.warm_up();
    assert_eq!(h.sheet().page, None, "the App item's second tap closes it");

    h.mouse_click(h.bottom_bar().menu.0.center());
    h.warm_up();
    assert_eq!(h.sheet().page, Some(crate::ui::SheetPage::Menu));
    assert!(h.bottom_bar().menu.1);
    h.mouse_click(h.bottom_bar().layers.0.center());
    h.warm_up();
    assert_eq!(
        h.sheet().page,
        Some(crate::ui::SheetPage::Layers),
        "Menu to Layers is a switch"
    );
    h.mouse_click(h.bottom_bar().layers.0.center());
    h.warm_up();
    assert_eq!(
        h.sheet().page,
        None,
        "the Layers item's second tap closes it"
    );
}

/// M9-9. **Bar items stack their icon above their label.**
#[test]
fn bar_items_stack_icon_above_label() {
    let mut h = phone();
    h.warm_up();
    let bar = h.bottom_bar();
    for (which, (icon, label)) in ["Menu", "Layers", "Pane", "App"].iter().zip(bar.icon_label) {
        assert_ne!(icon, egui::Rect::NOTHING, "{which} drew no icon");
        assert_ne!(label, egui::Rect::NOTHING, "{which} drew no label");
        assert!(
            label.top() >= icon.bottom() - 0.5,
            "{which}'s label at {label:?} is not below its icon at {icon:?}"
        );
        assert!(
            (label.center().x - icon.center().x).abs() <= 1.5,
            "{which}'s label and icon are not stacked on one centre line"
        );
    }
    for (which, (item, (icon, label))) in ["Menu", "Layers", "Pane", "App"].iter().zip(
        [bar.menu, bar.layers, bar.pane, bar.app]
            .into_iter()
            .zip(bar.icon_label),
    ) {
        assert!(
            item.0.contains_rect(icon) && item.0.contains_rect(label),
            "{which}'s stack leaks outside its click target"
        );
    }
}

/// M9-12. **The inline transport sits flush on the bar, full width.**
#[test]
fn the_inline_transport_sits_flush_on_the_bar() {
    let mut h = phone();
    h.warm_up();
    let bar = h.bottom_bar().rect;
    let timeline = h.timeline();
    assert!(
        !timeline.collapsed,
        "precondition: the phone transport opens expanded"
    );
    assert!(
        (timeline.rect.bottom() - bar.top()).abs() <= 0.5,
        "the transport at {:?} does not sit flush on the bar at {bar:?}",
        timeline.rect
    );
    let map = h.map_panel_rect();
    assert!(
        (timeline.rect.left() - map.left()).abs() <= 0.5
            && (map.right() - timeline.rect.right()).abs() <= 0.5,
        "the inline transport must span the full width like the bar under it"
    );
}

/// M9-15. **The Layers page's segments header survives the smallest sheet.**
#[test]
fn the_sheet_keeps_the_layers_page_segments_at_its_smallest() {
    let mut h = phone();
    h.set_pane_count(2);
    h.open_layers();
    let handle = h.sheet().handle;
    let map = h.map_panel_rect();
    let near_bottom = egui::pos2(handle.center().x, map.bottom() - 10.0);

    h.mouse_press(handle.center());
    h.frame_after(FRAME_DT);
    h.mouse_move(egui::pos2(handle.center().x, handle.center().y + 60.0));
    h.frame_after(FRAME_DT);
    h.mouse_move(near_bottom);
    h.frame_after(FRAME_DT);

    let sheet = h.sheet_rect().expect("mid-drag the sheet is still up");
    let options = h.pane_options();
    assert!(
        !options.is_empty(),
        "the Layers page must draw its segments header"
    );
    for opt in options {
        assert!(
            sheet.contains_rect(opt.rect),
            "segment button {} at {:?} is cut off by the {sheet:?} sheet at \
             its floor",
            opt.count,
            opt.rect
        );
    }
    h.mouse_release(near_bottom);
    h.warm_up();
}

/// M9-10. **The narrow transport never overlaps itself: essentials on row one, the
/// scrubber on its own full-width row, the age chip dropped.**
#[test]
fn the_narrow_transport_puts_the_scrubber_on_its_own_row_and_nothing_overlaps() {
    let mut h = phone();
    h.load_scan("KMKX");
    h.warm_up();
    let t = h.timeline();
    assert!(!t.collapsed, "precondition: the transport is expanded");

    let row1: Vec<(&str, egui::Rect)> = vec![
        ("Live", t.live.0),
        ("back", t.back),
        ("fwd", t.fwd.0),
        ("step", t.step_dropdown),
        ("loop", t.loop_toggle.0),
        ("timestamp", t.timestamp.0),
        ("expander", t.expander),
        ("collapse", t.collapse),
    ];
    for (i, &(name_a, a)) in row1.iter().enumerate() {
        assert_ne!(a, egui::Rect::NOTHING, "{name_a} was not drawn");
        for &(name_b, b) in &row1[i + 1..] {
            assert!(
                a.intersect(b).size().min_elem() <= 0.0,
                "{name_a} at {a:?} overlaps {name_b} at {b:?} on the narrow \
                 transport - the mangled-row bug"
            );
        }
    }

    let scrubber = t.scrubber;
    let row1_bottom = row1
        .iter()
        .map(|&(_, r)| r.bottom())
        .fold(f32::MIN, f32::max);
    assert!(
        scrubber.top() >= row1_bottom - 0.5,
        "the scrubber at {scrubber:?} does not sit on its own row below the \
         controls (row 1 bottom {row1_bottom})"
    );
    assert!(
        scrubber.width() >= 0.8 * t.rect.width(),
        "the scrubber's own row should hand it nearly the transport's width"
    );

    assert!(
        t.age_text.is_empty(),
        "the narrow transport still draws the age chip: {:?}",
        t.age_text
    );

    let mut wide = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    wide.load_scan("KMKX");
    wide.set_data_time(0, Some(written_ago(5)));
    let t = wide.timeline();
    assert!(
        !t.age_text.is_empty(),
        "the wide transport must keep the age chip"
    );
}

/// M9-16. **The Live/timestamp chip never intersects the bar's items.**
#[test]
fn the_live_chip_never_intersects_the_bar_items() {
    for width in [420.0, 330.0] {
        let mut h = InputHarness::with_screen(egui::vec2(width, 900.0));
        h.load_scan("KMKX");
        h.gui_mut().active_pane_mut().viewing_live = false;
        h.warm_up();
        let bar = h.bottom_bar();
        let (chip, _) = bar.live_chip;
        if chip == egui::Rect::NOTHING {
            continue;
        }
        for (which, item) in ["Menu", "Layers", "Pane", "App"]
            .iter()
            .zip([bar.menu, bar.layers, bar.pane, bar.app])
        {
            assert!(
                chip.intersect(item.0).size().min_elem() <= 0.0,
                "the chip at {chip:?} bleeds into {which} at {:?} on a \
                 {width}pt screen",
                item.0
            );
        }
    }
}

/// 71. **Dialogs are modals at ≥600pt and sheet pages below it — the phone never
///     draws a modal.**
#[test]
fn dialogs_are_modals_on_wide_screens_and_sheet_pages_on_the_phone() {
    let mut desk = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    desk.open_catalog();
    assert_eq!(desk.sheet().page, None, "no sheet on a desktop");
    assert!(
        desk.area_rect(egui::Id::new("add_layer_catalog")).is_some(),
        "the desktop catalog must be the egui Modal"
    );
    desk.mouse_click(egui::pos2(40.0, 400.0));
    desk.warm_up();
    assert!(
        !desk.catalog().open,
        "the modal's backdrop click must close it"
    );

    let mut h = phone();
    h.open_catalog();
    let sheet = h.sheet();
    assert_eq!(sheet.page, Some(crate::ui::SheetPage::Catalog));
    assert_eq!(
        sheet.extent,
        crate::ui::SheetExtent::Full,
        "the catalog is a full-height page (plan §1.10)"
    );
    assert!(
        h.area_rect(egui::Id::new("add_layer_catalog")).is_none(),
        "the phone drew the catalog Modal it must never draw"
    );
    let sheet_rect = h.sheet_rect().expect("the sheet is open");
    let search = h.catalog().search;
    assert!(
        sheet_rect.contains_rect(search),
        "the catalog's search field at {search:?} is not inside the sheet \
         {sheet_rect:?}"
    );

    let above = egui::pos2(sheet_rect.center().x, sheet_rect.top() - 12.0);
    assert!(
        above.y > h.top_bar().rect.bottom(),
        "precondition: the backdrop click must land on the scrim, not the bar"
    );
    h.mouse_click(above);
    h.warm_up();
    assert!(!h.catalog().open, "the scrim click must close the catalog");
    assert_eq!(
        h.sheet().page,
        Some(crate::ui::SheetPage::Layers),
        "closing the catalog must reveal the page beneath"
    );

    h.close_layers();
    let (stamp, _) = h.timeline().timestamp;
    h.mouse_click(stamp.center());
    h.warm_up();
    assert_eq!(h.sheet().page, Some(crate::ui::SheetPage::Time));
    assert!(
        h.text_painted_in(h.sheet_rect().expect("open"), "Select Time"),
        "the Time page must carry the dialog body"
    );
    assert!(
        h.area_rect(egui::Id::new("Set Time")).is_none(),
        "the phone drew the Set Time window it must never draw"
    );
    assert!(h.gui_mut().dismiss_top_layer(), "close the time page again");
    h.warm_up();

    h.gui_mut().overlays.selected_overlays = vec![std::sync::Arc::new(SheetStubFeature)];
    h.warm_up();
    let sheet = h.sheet();
    assert_eq!(sheet.page, Some(crate::ui::SheetPage::Feature));
    assert_eq!(
        sheet.title, "Stub feature",
        "the sheet's title row must carry the feature's own title"
    );
    assert!(
        h.text_painted_in(h.sheet_rect().expect("open"), "stub body"),
        "the Feature page must render the feature's sections"
    );
    assert!(
        h.area_rect(egui::Id::new("overlay_pager_popup")).is_none(),
        "the phone drew the pager window it must never draw"
    );
}

/// 75. **The phone top bar shares the status bar's collapse state: collapsed, only
///     the wordmark and the restore button remain.**
#[test]
fn the_phone_top_bar_shares_the_status_collapse_state() {
    let mut h = phone();
    h.load_scan("KABR");
    let bar = h.top_bar();
    assert!(
        !bar.scan_text.is_empty() && bar.section_arm.0.is_positive(),
        "precondition: the expanded phone bar carries the chip and the arms"
    );

    h.mouse_click(bar.collapse.center());
    h.warm_up();
    let collapsed = h.top_bar();
    assert!(
        collapsed.scan_text.is_empty(),
        "the collapsed bar still carried the scan chip"
    );
    assert!(
        !collapsed.section_arm.0.is_positive(),
        "the collapsed bar still drew the arm toggles"
    );
    assert!(
        h.text_painted_in(collapsed.rect, "SQUALL"),
        "the wordmark must survive the collapse"
    );
    assert!(
        !h.text_painted_in(collapsed.rect, "KABR"),
        "the scan text was still painted while collapsed"
    );

    h.set_screen(egui::vec2(1400.0, 900.0));
    assert!(
        h.status_bar().collapsed,
        "the phone bar's collapse did not reach the status bar it shares \
         state with"
    );

    h.mouse_click(h.status_bar().collapse.center());
    h.warm_up();
    assert!(!h.status_bar().collapsed, "precondition: restored");
    h.set_screen(egui::vec2(420.0, 1400.0));
    assert!(
        !h.top_bar().scan_text.is_empty(),
        "the status bar's restore did not reach the phone bar"
    );
}

/// **The sheet's handle snaps Half ↔ Full, and a deep drag-down dismisses** (plan
/// §1.13): the release decides what the drag meant — past the midpoint towards Full
/// snaps Full, back below it snaps Half, and a release more than a quarter below
/// the Half height clears every page flag.
#[test]
fn the_sheet_handle_snaps_between_half_full_and_dismissal() {
    let mut h = phone();
    h.open_layers();
    assert_eq!(h.sheet().extent, crate::ui::SheetExtent::Half);
    let half_height = h.sheet_rect().expect("open").height();

    let start = h.sheet().handle.center();
    h.mouse_press(start);
    h.frame_after(FRAME_DT);
    for step in 1..=6 {
        h.mouse_move(start - egui::vec2(0.0, 80.0 * step as f32));
        h.frame_after(FRAME_DT);
    }
    h.mouse_release(start - egui::vec2(0.0, 480.0));
    h.warm_up();
    assert_eq!(
        h.sheet().extent,
        crate::ui::SheetExtent::Full,
        "a release past the midpoint must snap to Full"
    );
    let full_height = h.sheet_rect().expect("still open").height();
    assert!(
        full_height > half_height + 100.0,
        "Full must actually be taller: {half_height} -> {full_height}"
    );

    let start = h.sheet().handle.center();
    h.mouse_press(start);
    h.frame_after(FRAME_DT);
    for step in 1..=6 {
        h.mouse_move(start + egui::vec2(0.0, 80.0 * step as f32));
        h.frame_after(FRAME_DT);
    }
    h.mouse_release(start + egui::vec2(0.0, 480.0));
    h.warm_up();
    assert_eq!(
        h.sheet().extent,
        crate::ui::SheetExtent::Half,
        "a release back below the midpoint must snap to Half"
    );

    let start = h.sheet().handle.center();
    h.mouse_press(start);
    h.frame_after(FRAME_DT);
    for step in 1..=5 {
        h.mouse_move(start + egui::vec2(0.0, 80.0 * step as f32));
        h.frame_after(FRAME_DT);
    }
    h.mouse_release(start + egui::vec2(0.0, 400.0));
    h.warm_up();
    assert_eq!(
        h.sheet().page,
        None,
        "a deep drag-down must dismiss the sheet"
    );
    assert!(
        !h.layers_panel_on_screen(),
        "the dismissal must clear the page's flag, not just hide the sheet"
    );
}

/// **A back press walks the phone sheet pages top-down, one visible pop per press**
/// (plan §3.4; scope item 7): Feature → Time → Menu → Inspector → Layers → the
/// armed drag — the projection order, driven through the same `dismiss_top_layer`
/// entry every width shares.
#[test]
fn a_back_press_walks_the_phone_sheet_pages_top_down() {
    let mut h = phone();
    h.set_drawer_open(true);
    h.gui_mut().open_settings();
    h.gui_mut().set_sheet_menu_open_for_test(true);
    h.gui_mut().set_time_dialog_open_for_test(true);
    h.gui_mut().overlays.selected_overlays = vec![std::sync::Arc::new(SheetStubFeature)];
    h.set_section_draw_armed(true);
    h.warm_up();

    let walk = |h: &mut InputHarness, expect: Option<crate::ui::SheetPage>| {
        assert!(
            h.gui_mut().dismiss_top_layer(),
            "a press with pages open must be consumed"
        );
        h.warm_up();
        assert_eq!(h.sheet().page, expect, "the pop was not the visible one");
    };

    assert_eq!(h.sheet().page, Some(crate::ui::SheetPage::Feature));
    walk(&mut h, Some(crate::ui::SheetPage::Time));
    walk(&mut h, Some(crate::ui::SheetPage::Menu));
    walk(&mut h, Some(crate::ui::SheetPage::Inspector));
    walk(&mut h, Some(crate::ui::SheetPage::Layers));
    walk(&mut h, None);
    assert!(h.gui_mut().dismiss_top_layer(), "the armed drag is below");
    assert!(
        !h.section_draw_armed(),
        "the press must disarm the region drag"
    );
    assert!(
        !h.gui_mut().dismiss_top_layer(),
        "nothing is left; the next press belongs to the exit path"
    );

    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    h.gui_mut().overlays.selected_overlays = vec![std::sync::Arc::new(SheetStubFeature)];
    h.open_catalog();
    h.set_screen(egui::vec2(420.0, 1400.0));
    assert_eq!(
        h.sheet().page,
        Some(crate::ui::SheetPage::Feature),
        "precondition: the projection puts the feature over the open catalog"
    );
    assert!(h.gui_mut().dismiss_top_layer(), "a press with pages open");
    h.warm_up();
    assert_eq!(
        h.sheet().page,
        Some(crate::ui::SheetPage::Catalog),
        "the pop must take the visible Feature page and leave the catalog \
         its flag — never the invisible layer first"
    );
    assert!(h.gui_mut().dismiss_top_layer(), "the catalog is now on top");
    h.warm_up();
    assert_eq!(h.sheet().page, None, "two pages, two pops, sheet closed");
    assert!(
        !h.gui_mut().dismiss_top_layer(),
        "nothing invisible was left behind the two visible pops"
    );

    let mut h = phone();
    h.open_catalog();
    assert_eq!(h.sheet().page, Some(crate::ui::SheetPage::Catalog));
    assert!(h.gui_mut().dismiss_top_layer(), "the catalog was open");
    h.warm_up();
    assert_eq!(
        h.sheet().page,
        Some(crate::ui::SheetPage::Layers),
        "popping the catalog must reveal the Layers page it was opened from"
    );
}

/// **Stack rows carry a trailing › on the drawer and sheet hosts, and none on the
/// desktop sidebar** (plan §1.3): where a row click pushes the inspector *over* the
/// list, the chevron says so; where the inspector opens beside it, there is nothing
/// to push.
#[test]
fn stack_rows_carry_a_chevron_only_in_the_drawer_and_sheet_hosts() {
    let desk = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    let row = desk
        .stack_row(&known::NWS_ALERTS)
        .expect("the desktop sidebar is open by default");
    assert_eq!(
        row.chevron, None,
        "a desktop sidebar row grew a chevron it has nothing to push for"
    );

    let mut tablet = InputHarness::with_screen(egui::vec2(800.0, 1200.0));
    tablet.open_layers();
    let row = tablet
        .stack_row(&known::NWS_ALERTS)
        .expect("the drawer is open");
    assert!(
        row.chevron.is_some_and(|c| c.is_positive()),
        "a drawer row must carry the chevron"
    );

    let mut ph = phone();
    ph.open_layers();
    let row = ph
        .stack_row(&known::NWS_ALERTS)
        .expect("the sheet's Layers page is open");
    let chevron = row.chevron.expect("a sheet row must carry the chevron");
    assert!(
        ph.sheet_rect().expect("open").contains(chevron.center()),
        "the chevron must be drawn inside the sheet"
    );
}

/// **The phone error toast sits under the top bar, clear of the arm toggles, and
/// its ✕ dismisses** — the status bar's error contract, moved to the one chrome
/// strip the phone keeps at the top.
#[test]
fn the_phone_error_toast_sits_under_the_top_bar_and_its_cross_dismisses() {
    let mut h = phone();
    h.gui_mut().apply(crate::shell_api::GuiEvent::Error(
        "the feed went away".to_owned(),
    ));
    h.warm_up();

    let toast = h.error_toast().expect("an error must put the toast up");
    let bar = h.top_bar();
    assert!(
        toast.rect.top() >= bar.rect.bottom(),
        "the toast at {:?} must render under the docked bar at {:?}",
        toast.rect,
        bar.rect
    );
    assert!(
        !toast.rect.intersects(bar.section_arm.0),
        "the toast must not cover the arm toggles"
    );
    assert!(
        h.text_painted_in(toast.rect, "the feed went away"),
        "the toast must carry the error text"
    );

    h.mouse_click(toast.close.center());
    h.warm_up();
    assert!(
        h.error_toast().is_none(),
        "\u{2715} must clear the error and take the toast down"
    );
}

/// **The phone error toast stays visible and dismissible while a sheet page is
/// up.**
#[test]
fn the_phone_error_toast_stays_visible_and_dismissible_over_an_open_sheet() {
    let mut h = phone();
    h.open_catalog();
    h.gui_mut().apply(crate::shell_api::GuiEvent::Error(
        "the feed went away".to_owned(),
    ));
    h.warm_up();

    let toast = h
        .error_toast()
        .expect("the toast must draw with a page open");
    assert!(
        toast.rect.bottom() < h.sheet_rect().expect("the page is open").top(),
        "precondition: the toast sits in the scrim's band above the sheet, \
         or the layering assertion below tests nothing"
    );
    assert_eq!(
        h.top_layer_id_at(toast.rect.center()),
        Some(egui::Id::new("phone_error_toast")),
        "the toast must be the top layer where it draws — above the scrim"
    );
    assert!(
        h.text_painted_in(toast.rect, "the feed went away"),
        "the toast must carry the error text over the open page"
    );

    h.mouse_click(toast.close.center());
    h.warm_up();
    assert!(
        h.error_toast().is_none(),
        "\u{2715} must work through the scrim's band"
    );
    assert_eq!(
        h.sheet().page,
        Some(crate::ui::SheetPage::Catalog),
        "dismissing the toast must not also dismiss the page under it"
    );
}

/// **A release on the forced-Full Catalog page keeps the stored snap.**
#[test]
fn a_release_on_the_forced_full_catalog_page_keeps_the_stored_snap() {
    let mut h = phone();
    h.open_layers();
    assert_eq!(
        h.sheet().extent,
        crate::ui::SheetExtent::Half,
        "precondition: the stored snap starts at Half"
    );
    h.open_catalog();
    assert_eq!(
        h.sheet().extent,
        crate::ui::SheetExtent::Full,
        "precondition: the Catalog page forces Full"
    );

    let start = h.sheet().handle.center();
    h.mouse_press(start);
    h.frame_after(FRAME_DT);
    for step in 1..=3 {
        h.mouse_move(start + egui::vec2(0.0, 30.0 * step as f32));
        h.frame_after(FRAME_DT);
    }
    h.mouse_release(start + egui::vec2(0.0, 90.0));
    h.warm_up();
    assert_eq!(
        h.sheet().page,
        Some(crate::ui::SheetPage::Catalog),
        "precondition: the release was a settle, not a dismissal"
    );

    assert!(h.gui_mut().dismiss_top_layer());
    h.warm_up();
    assert_eq!(h.sheet().page, Some(crate::ui::SheetPage::Layers));
    assert_eq!(
        h.sheet().extent,
        crate::ui::SheetExtent::Half,
        "the forced-Full release must not overwrite the stored snap"
    );
}

/// **Arming ╱ from the phone top bar closes the open sheet** — the Menu page's own
/// rule for its arm entry, applied to the bar's route: the next thing the user does
/// is a drag on the map the sheet is covering.
#[test]
fn arming_from_the_phone_top_bar_closes_the_open_sheet() {
    let mut h = phone();
    h.open_layers();
    assert_eq!(h.sheet().page, Some(crate::ui::SheetPage::Layers));

    let (section, armed) = h.top_bar().section_arm;
    assert!(!armed, "precondition: the mode starts disarmed");
    h.mouse_click(section.center());
    h.warm_up();
    assert!(h.section_draw_armed(), "the tap must arm the drag");
    assert_eq!(
        h.sheet().page,
        None,
        "arming needs the map: the sheet must close with it"
    );

    h.open_layers();
    let (section, armed) = h.top_bar().section_arm;
    assert!(armed, "precondition: still armed across the reopen");
    h.mouse_click(section.center());
    h.warm_up();
    assert!(!h.section_draw_armed(), "the second tap must disarm");
    assert_eq!(
        h.sheet().page,
        Some(crate::ui::SheetPage::Layers),
        "disarming closes nothing"
    );
}

/// Whether the floating chrome is on the glass, read off the probes the renderers
/// write — the timeline and the status bar on the wide widths, the timeline and the
/// bottom bar on the phone.
fn chrome_on_screen(h: &InputHarness) -> bool {
    let timeline = h.timeline();
    let timeline_drawn = timeline.rect != egui::Rect::NOTHING || timeline.collapsed;
    if h.width_class() == crate::ui_layout::WidthClass::Compact {
        timeline_drawn && h.bottom_bar().rect != egui::Rect::NOTHING
    } else {
        timeline_drawn && h.status_bar().rect != egui::Rect::NOTHING
    }
}

/// 60. **A qualifying tap fades all the floating chrome; the second restores it; a
///     drag, a consumed click and an armed tool do not fade.**
#[test]
fn a_qualifying_tap_fades_the_chrome_and_the_second_restores_it() {
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    let spot = h.map_center();
    assert!(
        chrome_on_screen(&h) && !h.pill_rows().is_empty(),
        "precondition: the chrome is up"
    );

    h.mouse_press(spot);
    for i in 1..=4 {
        h.mouse_move(spot + egui::vec2(8.0 * i as f32, 0.0));
        h.frame_after(FRAME_DT);
    }
    h.mouse_release(spot + egui::vec2(32.0, 0.0));
    h.warm_up();
    assert!(!h.faded() && chrome_on_screen(&h), "a drag must not fade");

    h.set_section_draw_armed(true);
    h.mouse_click(spot);
    h.warm_up();
    assert!(
        !h.faded() && chrome_on_screen(&h),
        "an armed draw must not fade"
    );
    h.set_section_draw_armed(false);
    h.set_region_pick_armed(true);
    h.mouse_click(spot);
    h.warm_up();
    assert!(
        !h.faded() && chrome_on_screen(&h),
        "an armed region must not fade"
    );
    h.set_region_pick_armed(false);

    h.close_layers();
    h.gui_mut().enable_overlay_for_test(&known::RADAR_SITES);
    h.warm_up();
    let site_spot = egui::pos2(spot.x - 150.0, spot.y);
    h.place_site_at(0, "KTLX", site_spot);
    h.mouse_click(site_spot);
    assert!(h.click_consumed(), "precondition: the icon took the click");
    h.warm_up();
    assert!(
        !h.faded() && chrome_on_screen(&h),
        "a consumed click must not fade"
    );

    h.mouse_click(spot);
    h.warm_up();
    assert!(h.faded(), "the bare-map click must fade");
    assert!(
        h.timeline().rect == egui::Rect::NOTHING && h.timeline().chip == egui::Rect::NOTHING,
        "the timeline must not render while faded"
    );
    assert_eq!(
        h.status_bar().rect,
        egui::Rect::NOTHING,
        "the status bar must not render while faded"
    );
    assert!(
        h.pill_rows().is_empty(),
        "the pill rows must not render while faded"
    );
    assert_ne!(
        h.top_bar().rect,
        egui::Rect::NOTHING,
        "the docked top bar never fades"
    );

    h.mouse_click(spot);
    h.warm_up();
    assert!(!h.faded(), "the second tap must restore");
    assert!(
        chrome_on_screen(&h) && !h.pill_rows().is_empty(),
        "the chrome must be back"
    );
}

/// 60b. **The same trigger on the phone: the bottom cluster fades and the second
/// tap restores it — and an armed tool still does not fade.**
#[test]
fn a_qualifying_tap_fades_the_phone_cluster_and_the_second_restores_it() {
    let mut h = phone();
    let spot = h.pane_rects()[0].center();
    assert!(chrome_on_screen(&h), "precondition: the cluster is up");

    h.set_region_pick_armed(true);
    h.mouse_click(spot);
    h.warm_up();
    assert!(
        !h.faded() && chrome_on_screen(&h),
        "an armed region must not fade"
    );
    h.set_region_pick_armed(false);

    h.mouse_click(spot);
    h.warm_up();
    assert!(h.faded(), "the bare-map tap must fade");
    assert_eq!(
        h.bottom_bar().rect,
        egui::Rect::NOTHING,
        "the bottom bar must not render while faded"
    );
    assert_eq!(
        h.timeline().rect,
        egui::Rect::NOTHING,
        "the inline transport must not render while faded"
    );
    assert_ne!(h.top_bar().rect, egui::Rect::NOTHING, "the top bar stays");

    h.mouse_click(spot);
    h.warm_up();
    assert!(
        !h.faded() && chrome_on_screen(&h),
        "the second tap restores"
    );
}

/// 61. **Fading closes the panels and the sheet for real — state, not paint — and
///     unfading reopens nothing.**
#[test]
fn fading_closes_the_panels_for_real_and_unfading_reopens_nothing() {
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    h.gui_mut().open_settings();
    h.warm_up();
    assert!(
        h.layers_panel_on_screen() && h.inspector().open,
        "precondition: both panels open"
    );

    h.mouse_click(h.map_center());
    h.warm_up();
    assert!(h.faded(), "the click beside the panels must fade");
    assert!(
        !h.layers_panel_on_screen() && !h.inspector().open,
        "the fade must close both panels"
    );
    assert!(h.gui_mut().dismiss_top_layer(), "the fade itself");
    assert!(
        !h.gui_mut().dismiss_top_layer(),
        "something stayed open invisibly under the fade"
    );
    h.warm_up();

    assert!(
        !h.layers_panel_on_screen() && !h.inspector().open,
        "unfading must not reopen the panels"
    );
    assert!(chrome_on_screen(&h), "the unconditional chrome is back");
}

/// 61b. **The phone bottom cluster is edge-flush and seals the map while a page is
/// open** — the full-bleed direction of the second user test, which retired the
/// sliver band this contract used to fade through.
#[test]
fn the_phone_bottom_cluster_is_edge_flush_and_seals_the_map() {
    let mut h = phone();
    h.open_layers();
    let map = h.map_panel_rect();
    let bar = h.bottom_bar().rect;
    assert!(
        (bar.left() - map.left()).abs() <= 0.5 && (map.right() - bar.right()).abs() <= 0.5,
        "the bar must span the full width: bar {bar:?} in map {map:?}"
    );
    assert!(
        (map.bottom() - bar.bottom()).abs() <= 0.5,
        "the bar must touch the bottom edge: bar {bar:?} in map {map:?}"
    );

    let sheet = h.sheet_rect().expect("the Layers page is open");
    assert!(
        (sheet.bottom() - bar.top()).abs() <= 0.5,
        "the sheet must sit flush on the bar: sheet bottom {} vs bar top {}",
        sheet.bottom(),
        bar.top()
    );

    let above = egui::pos2(map.left() + 3.0, (map.top() + sheet.top()) / 2.0);
    assert!(
        h.is_floating_layer_at(above),
        "the scrim must cover the map above the sheet"
    );
}

/// 61c. **The fade closes the Volume Alpha editor for real — per-pane floating
/// chrome, on the same terms as the panels (§1.8) — and unfading does not reopen
/// it.**
#[test]
fn the_fade_closes_the_volume_alpha_editor_for_real() {
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    h.set_pane_count(2);
    h.make_pane_volume(1);

    let button = h
        .painted_text_rects()
        .into_iter()
        .find(|(_, text)| text.contains("Volume alpha"))
        .expect("the 3D pane draws its Volume alpha corner button")
        .0;
    h.mouse_click(button.center());
    h.warm_up();
    let editor_open = |h: &mut InputHarness| {
        h.gui_mut()
            .pane(1)
            .expect("pane 1 exists")
            .volume()
            .expect("pane 1 is a 3D pane")
            .alpha_editor_open
    };
    assert!(editor_open(&mut h), "precondition: the editor is open");

    let spot = h.pane_rects()[0].center();
    h.mouse_click(spot);
    h.warm_up();
    h.mouse_click(spot);
    h.warm_up();
    assert!(h.faded(), "the bare-map tap must fade");
    assert!(
        !editor_open(&mut h),
        "the fade must close the editor for real — state, not paint"
    );
    assert!(
        !h.text_painted_in(h.screen_rect(), "Volume Alpha"),
        "no editor window survives on the glass"
    );

    h.mouse_click(spot);
    h.warm_up();
    assert!(!h.faded(), "the second tap restores");
    assert!(!editor_open(&mut h), "unfading must not reopen the editor");
}

/// 62. **A top-bar interaction while faded unfades first, then performs — nothing
///     opens invisibly.**
#[test]
fn a_top_bar_interaction_while_faded_unfades_and_performs() {
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    h.mouse_click(h.map_center());
    h.warm_up();
    assert!(h.faded(), "precondition: faded");

    h.mouse_click(h.top_bar().menu_button.center());
    h.warm_up();
    assert!(!h.faded(), "the bar press must clear the fade");
    assert!(
        !h.menu_leaves().is_empty(),
        "the click must still perform: the menu opens, visible"
    );
    assert!(chrome_on_screen(&h), "the chrome returns with it");

    let mut h = phone();
    h.mouse_click(h.pane_rects()[0].center());
    h.warm_up();
    assert!(h.faded(), "precondition: faded");

    h.mouse_click(h.top_bar().collapse.center());
    h.warm_up();
    assert!(!h.faded(), "the bar press must clear the fade");
    assert!(
        h.top_bar().scan_text.is_empty(),
        "the click must still perform: the bar collapses to its wordmark"
    );
    assert_ne!(
        h.bottom_bar().rect,
        egui::Rect::NOTHING,
        "the bottom cluster returns with the unfade"
    );
}

/// 62b. **A keyboard activation while faded unfades too — one frame later, through
/// the invariant's repair, and the surface it opened stays.**
#[test]
fn a_keyboard_activation_while_faded_unfades_and_the_surface_stays() {
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    h.mouse_click(h.map_center());
    h.warm_up();
    assert!(h.faded(), "precondition: faded");
    assert!(
        !h.layers_panel_on_screen(),
        "precondition: the stack is closed"
    );

    let toggle = h
        .widget_id_probes()
        .iter()
        .find(|(name, _)| *name == "layers_toggle")
        .expect("the top bar reports its Layers toggle id")
        .1;
    h.focus_widget(toggle);
    assert!(h.faded(), "focus alone opens nothing and must not unfade");

    h.key_press(egui::Key::Enter);
    h.frame_after(FRAME_DT); // the activation frame: the stack opens in state
    h.frame_after(FRAME_DT); // the repair frame: the invariant unfades
    assert!(!h.faded(), "the keyboard activation must unfade");
    assert!(
        h.layers_panel_on_screen(),
        "and the stack it opened must be on screen, not re-closed"
    );
    h.warm_up();
    assert!(
        !h.faded() && h.layers_panel_on_screen() && chrome_on_screen(&h),
        "the repair holds: chrome back, stack open, nothing flapping"
    );
}

/// 63. **The top bar stays present and interactive while faded — the docked
///     exception to §1.8's "fade all chrome".**
#[test]
fn the_top_bar_stays_present_and_interactive_while_faded() {
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    h.mouse_click(h.map_center());
    h.warm_up();
    assert!(h.faded(), "precondition: faded");
    assert_ne!(h.top_bar().rect, egui::Rect::NOTHING, "the bar is drawn");

    let two = h
        .pane_options()
        .into_iter()
        .find(|option| option.count == 2)
        .expect("the segments are drawn while faded");
    h.mouse_click(two.rect.center());
    h.warm_up();
    assert_eq!(h.pane_count(), 2, "the segment performed");
    assert!(!h.faded(), "and the press cleared the fade first");

    let mut h = phone();
    h.mouse_click(h.pane_rects()[0].center());
    h.warm_up();
    assert!(h.faded(), "precondition: faded");
    assert_ne!(h.top_bar().rect, egui::Rect::NOTHING, "the bar is drawn");

    h.mouse_click(h.top_bar().section_arm.0.center());
    h.warm_up();
    assert!(h.section_draw_armed(), "the arm performed");
    assert!(!h.faded(), "and the press cleared the fade first");
}

/// 65. **The full Esc/back order, fade included: fade → catalog → feature → time →
///     inspector → drawer → armed drag, one layer per press.**
#[test]
fn a_back_press_walks_the_full_wide_chain_in_order() {
    let mut h = InputHarness::with_screen(egui::vec2(800.0, 1200.0));
    assert_eq!(h.width_class(), crate::ui_layout::WidthClass::Medium);

    h.mouse_click(h.map_center());
    h.warm_up();
    assert!(h.faded(), "precondition: faded");
    assert!(h.gui_mut().dismiss_top_layer(), "the press unfades");
    assert!(!h.faded());
    h.warm_up();
    assert!(chrome_on_screen(&h), "Esc means restore my UI");

    h.set_section_draw_armed(true);
    h.set_drawer_open(true);
    h.gui_mut().open_settings();
    h.gui_mut().set_time_dialog_open_for_test(true);
    h.gui_mut().overlays.selected_overlays = vec![std::sync::Arc::new(SheetStubFeature)];
    h.gui_mut().set_catalog_open_for_test(true);
    h.warm_up();

    assert!(h.gui_mut().dismiss_top_layer(), "the catalog is on top");
    h.warm_up();
    assert!(!h.catalog().open, "press 1 closes the catalog");

    assert!(h.gui_mut().dismiss_top_layer(), "the feature is next");
    h.warm_up();
    assert!(
        h.gui_mut().overlays.selected_overlays.is_empty(),
        "press 2 closes the feature popup"
    );

    assert!(h.gui_mut().dismiss_top_layer(), "the time dialog is next");
    h.warm_up();
    assert!(
        !h.text_painted_in(h.screen_rect(), "Select Time"),
        "press 3 closes the time dialog"
    );

    assert!(h.gui_mut().dismiss_top_layer(), "the inspector is next");
    h.warm_up();
    assert!(!h.inspector().open, "press 4 closes the inspector");

    assert!(h.gui_mut().dismiss_top_layer(), "the drawer is next");
    h.warm_up();
    assert!(!h.layers_panel_on_screen(), "press 5 closes the drawer");

    assert!(h.gui_mut().dismiss_top_layer(), "the armed drag is last");
    assert!(!h.section_draw_armed(), "press 6 disarms");

    assert!(
        !h.gui_mut().dismiss_top_layer(),
        "press 7 falls through to the exit path"
    );
}

/// Put a Volume Alpha editor on screen, open, on a pane that is not the
/// active one — the arm's harder half, since the chain must still find it.
fn open_alpha_editor(h: &mut InputHarness) {
    h.set_pane_count(2);
    h.make_pane_volume(1);
    h.gui_mut()
        .pane_mut(1)
        .expect("pane 1 exists")
        .volume_mut()
        .expect("pane 1 is a 3D pane")
        .alpha_editor_open = true;
    h.warm_up();
}

/// Whether pane `idx`'s Volume Alpha editor is open.
fn alpha_editor_open(h: &mut InputHarness, idx: usize) -> bool {
    h.gui_mut()
        .pane(idx)
        .expect("the pane exists")
        .volume()
        .expect("the pane is a 3D pane")
        .alpha_editor_open
}

/// **The Volume Alpha editor is on the dismiss chain** — Escape and Android
/// back close it, at both widths, and it yields to the modal above it.
///
/// `ui_fade.rs` already counted the editor as an open surface, so the fade
/// invariant knew about it while the back chain did not: a window on screen
/// that Escape could not reach.
#[test]
fn a_back_press_closes_the_volume_alpha_editor() {
    for (name, mut h) in [
        ("wide", InputHarness::with_screen(egui::vec2(1400.0, 900.0))),
        ("phone", phone()),
    ] {
        open_alpha_editor(&mut h);
        assert!(
            alpha_editor_open(&mut h, 1),
            "{name}: precondition: the editor is open"
        );
        assert!(
            h.gui_mut().back_would_dismiss(),
            "{name}: the predicate does not see the open editor"
        );
        assert!(h.gui_mut().dismiss_top_layer(), "{name}: the press acts");
        h.warm_up();
        assert!(
            !alpha_editor_open(&mut h, 1),
            "{name}: the press did not close the editor"
        );
        assert!(
            !h.gui_mut().dismiss_top_layer(),
            "{name}: one press closed one thing, and there was nothing under it"
        );
    }

    // The modal above it goes first: one layer per press, in order.
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    open_alpha_editor(&mut h);
    h.gui_mut().set_catalog_open_for_test(true);
    h.warm_up();
    assert!(h.gui_mut().dismiss_top_layer(), "the catalog is on top");
    h.warm_up();
    assert!(!h.catalog().open, "press 1 closes the catalog");
    assert!(
        alpha_editor_open(&mut h, 1),
        "press 1 took the editor under the catalog with it"
    );
    assert!(h.gui_mut().dismiss_top_layer(), "the editor is next");
    h.warm_up();
    assert!(!alpha_editor_open(&mut h, 1), "press 2 closes the editor");
}

/// 65c. **The predicate and the act never disagree** — `back_would_dismiss()`
///     answers, for every state the chain above walks, exactly what
///     `dismiss_top_layer()` then does.
///
/// This pair is not a convenience. Android's predictive-back dispatcher decides
/// BEFORE the gesture whether the app or the system owns it, and takes no answer
/// afterwards, so the app publishes the predicate's verdict ahead of the press
/// (`App::push_back_claim` → `BackHandler.setClaimed`). A pair that has drifted
/// is therefore a claim that lies in one of two directions: a press swallowed
/// with nothing to close (no back-to-home, no preview animation), or the
/// Activity finished out from under an open sheet.
///
/// The app is not opted into that dispatcher today (`BackHandler.kt` and the
/// Android manifest carry the device measurement that keeps it out), so the
/// published claim is currently read by nothing. That is exactly why this pin
/// matters rather than why it does not: a drift introduced while the route is
/// dark surfaces on the day it is switched on, and nothing between here and
/// there would have caught it.
///
/// The walk is the same matrix tests 65 and 65b use, plus the two arms they do
/// not reach — the armed region pick and a section-endpoint drag in flight — and
/// the rung counters at the end are what stop this passing on a chain that never
/// found anything open.
#[test]
fn back_would_dismiss_agrees_with_dismiss_top_layer_in_every_ui_state() {
    /// One rung: ask, then act, then insist they said the same thing.
    fn paired(h: &mut InputHarness, state: &str) -> bool {
        let predicted = h.gui_mut().back_would_dismiss();
        let dismissed = h.gui_mut().dismiss_top_layer();
        assert_eq!(
            predicted, dismissed,
            "back_would_dismiss() answered {predicted} and dismiss_top_layer()              then did {dismissed}, with {state}: the predictive-back claim is a              lie in this state"
        );
        dismissed
    }

    /// One isolated rung: what to call the state, and how to raise it.
    type Rung = (&'static str, fn(&mut InputHarness));

    // Rungs where a press had something to close, and rungs where it did not.
    let mut had = 0usize;
    let mut had_not = 0usize;

    // ── The wide chain ───────────────────────────────────────────────────
    let mut h = InputHarness::with_screen(egui::vec2(800.0, 1200.0));
    assert_eq!(h.width_class(), crate::ui_layout::WidthClass::Medium);
    assert!(!paired(&mut h, "a fresh wide screen, nothing open"));
    had_not += 1;

    h.mouse_click(h.map_center());
    h.warm_up();
    assert!(h.faded(), "precondition: the click beside the chrome faded");
    assert!(paired(&mut h, "the fade"));
    had += 1;
    h.warm_up();

    h.set_section_draw_armed(true);
    h.set_drawer_open(true);
    h.gui_mut().open_settings();
    h.gui_mut().set_time_dialog_open_for_test(true);
    h.gui_mut().overlays.selected_overlays = vec![std::sync::Arc::new(SheetStubFeature)];
    h.gui_mut().set_catalog_open_for_test(true);
    h.warm_up();
    for state in [
        "the catalog on top",
        "the feature popup on top",
        "the time dialog on top",
        "the inspector on top",
        "the drawer on top",
        "an armed section draw and nothing above it",
    ] {
        assert!(paired(&mut h, state), "{state}: the chain stopped early");
        had += 1;
        h.warm_up();
    }
    assert!(!paired(&mut h, "the wide chain walked to its end"));
    had_not += 1;

    h.set_region_pick_armed(true);
    h.warm_up();
    assert!(paired(&mut h, "an armed region pick"));
    had += 1;
    assert!(!paired(&mut h, "the region pick disarmed again"));
    had_not += 1;

    // ── The Compact projection ───────────────────────────────────────────
    let mut h = phone();
    h.set_drawer_open(true);
    h.gui_mut().open_settings();
    h.gui_mut().set_sheet_menu_open_for_test(true);
    h.gui_mut().set_time_dialog_open_for_test(true);
    h.gui_mut().overlays.selected_overlays = vec![std::sync::Arc::new(SheetStubFeature)];
    h.set_section_draw_armed(true);
    h.warm_up();
    for state in [
        "the Feature sheet page",
        "the Time sheet page",
        "the Menu sheet page",
        "the Inspector sheet page",
        "the Layers sheet page",
        "an armed section draw below the closed sheet",
    ] {
        assert!(
            paired(&mut h, state),
            "{state}: the projection stopped early"
        );
        had += 1;
        h.warm_up();
    }
    assert!(!paired(&mut h, "the phone projection walked to its end"));
    had_not += 1;

    // ── Every arm ALONE ──────────────────────────────────────────────────
    //
    // The two walks above stack states, and a stacked walk cannot see a single
    // inverted arm: with the section draw still armed underneath, a
    // `back_would_dismiss` that has stopped believing in the drawer still
    // answers `true` at the drawer rung, for the wrong reason, and agrees with
    // a `dismiss_top_layer` that closed the drawer. Measured — inverting the
    // drawer arm left the stacked walk GREEN. So each arm is also raised on its
    // own, where nothing below it can answer in its place.
    let arms: [Rung; 8] = [
        ("the catalog alone", |h| {
            h.gui_mut().set_catalog_open_for_test(true)
        }),
        ("the feature popup alone", |h| {
            h.gui_mut().overlays.selected_overlays = vec![std::sync::Arc::new(SheetStubFeature)]
        }),
        ("the Volume Alpha editor alone", open_alpha_editor),
        ("the time dialog alone", |h| {
            h.gui_mut().set_time_dialog_open_for_test(true)
        }),
        ("the inspector alone", |h| h.gui_mut().open_settings()),
        ("the drawer alone", |h| h.set_drawer_open(true)),
        ("an armed section draw alone", |h| {
            h.set_section_draw_armed(true)
        }),
        ("an armed region pick alone", |h| {
            h.set_region_pick_armed(true)
        }),
    ];
    for (name, raise) in arms {
        let mut h = InputHarness::with_screen(egui::vec2(800.0, 1200.0));
        h.warm_up();
        assert!(
            !paired(&mut h, "a fresh wide screen, before the arm is raised"),
            "{name}: the harness did not start empty, so this rung proves nothing"
        );
        had_not += 1;

        raise(&mut h);
        h.warm_up();
        assert!(
            paired(&mut h, name),
            "{name}: raising the arm closed nothing"
        );
        had += 1;
        h.warm_up();

        assert!(
            !paired(&mut h, &format!("{name}, after it was dismissed")),
            "{name}: something was left behind the one arm this rung raised"
        );
        had_not += 1;
    }

    // The menu popup on its own. No setter reaches `menu_popup_open` — it is
    // written from the top bar's own `Popup::is_id_open` read — so this rung
    // goes through the button, which is also the only way a user gets there.
    let mut h = InputHarness::with_screen(egui::vec2(800.0, 1200.0));
    h.warm_up();
    assert!(!paired(
        &mut h,
        "a fresh wide screen, before the menu is opened"
    ));
    had_not += 1;
    h.open_menu();
    assert!(paired(&mut h, "the menu popup alone"));
    had += 1;
    h.warm_up();
    assert!(!paired(
        &mut h,
        "the menu popup alone, after it was dismissed"
    ));
    had_not += 1;

    // ── Every Compact sheet page ALONE ───────────────────────────────────
    //
    // Same reason as the arms above, for the other half of the branch: the
    // phone walk stacks pages over an armed section draw, and the arm below
    // answers for a `top_sheet_page()` the predicate has stopped consulting.
    // Measured — inverting the Compact branch left the stacked phone walk GREEN.
    let pages: [Rung; 5] = [
        ("the Feature page alone", |h| {
            h.gui_mut().overlays.selected_overlays = vec![std::sync::Arc::new(SheetStubFeature)]
        }),
        ("the Time page alone", |h| {
            h.gui_mut().set_time_dialog_open_for_test(true)
        }),
        ("the Menu page alone", |h| {
            h.gui_mut().set_sheet_menu_open_for_test(true)
        }),
        ("the Inspector page alone", |h| h.gui_mut().open_settings()),
        ("the Layers page alone", |h| h.set_drawer_open(true)),
    ];
    for (name, raise) in pages {
        let mut h = phone();
        h.warm_up();
        assert!(
            !paired(&mut h, "a fresh phone screen, before the page is opened"),
            "{name}: the harness did not start empty, so this rung proves nothing"
        );
        had_not += 1;

        raise(&mut h);
        h.warm_up();
        assert!(
            h.sheet().page.is_some(),
            "{name}: the setup did not put a page on the sheet"
        );
        assert!(paired(&mut h, name), "{name}: the page closed nothing");
        had += 1;
        h.warm_up();

        assert!(
            !paired(&mut h, &format!("{name}, after it was dismissed")),
            "{name}: something was left behind the one page this rung opened"
        );
        had_not += 1;
    }

    // The Volume Alpha editor on the phone, which is not a sheet page: the
    // Compact branch carries its own arm below the sheet block, and the loop
    // above cannot reach it.
    let mut h = phone();
    h.warm_up();
    assert!(!paired(
        &mut h,
        "a fresh phone screen, before the editor is opened"
    ));
    had_not += 1;
    open_alpha_editor(&mut h);
    h.warm_up();
    assert!(paired(
        &mut h,
        "the Volume Alpha editor alone, on the phone"
    ));
    had += 1;
    h.warm_up();
    assert!(!paired(
        &mut h,
        "the Volume Alpha editor alone, on the phone, after it was dismissed"
    ));
    had_not += 1;

    // The fade on its own, which no setter reaches — it is a click.
    let mut h = InputHarness::with_screen(egui::vec2(800.0, 1200.0));
    h.mouse_click(h.map_center());
    h.warm_up();
    assert!(h.faded(), "precondition: the click beside the chrome faded");
    assert!(paired(&mut h, "the fade alone"));
    had += 1;
    h.warm_up();
    assert!(!paired(&mut h, "the fade alone, after it was dismissed"));
    had_not += 1;

    // ── The arm neither chain above reaches: a drag in flight ────────────
    let (mut h, _a, b) = harness_with_committed_section();
    let b_px = h.screen_of(0, b);
    h.mouse_move(b_px);
    h.frame();
    h.mouse_press(b_px);
    h.frame();
    h.mouse_move(b_px + egui::vec2(-60.0, 30.0));
    h.frame();
    assert!(
        h.gui_mut().section_edit_drag_for_test().is_some(),
        "precondition: the press on the B cap began a section endpoint drag"
    );
    assert!(paired(&mut h, "a section endpoint drag in flight"));
    had += 1;

    // The floor. Without it, a chain that silently stopped finding anything open
    // would agree with a predicate that always answers `false`, and this test
    // would pass while proving nothing.
    assert!(
        had >= 30,
        "only {had} of the walked states had something for a press to close;          the matrix has stopped exercising the chain"
    );
    assert!(
        had_not >= 35,
        "only {had_not} of the walked states had nothing to close; a pair that          always answers `true` would survive this walk"
    );
}

/// 65b. **The Compact chain keeps its projection-first order with the fade at its
/// head** — the fade leg, then the sheet walk of
/// `a_back_press_walks_the_phone_sheet_pages_top_down`, abbreviated to the seam
/// this contract adds.
#[test]
fn a_back_press_on_the_phone_unfades_then_walks_the_projection() {
    let mut h = phone();
    h.mouse_click(h.pane_rects()[0].center());
    h.warm_up();
    assert!(h.faded(), "precondition: faded");
    assert!(h.gui_mut().dismiss_top_layer(), "the press unfades");
    assert!(!h.faded());
    h.warm_up();
    assert!(chrome_on_screen(&h), "back means restore my UI");

    h.set_drawer_open(true);
    h.gui_mut().open_settings();
    h.warm_up();
    assert_eq!(h.sheet().page, Some(crate::ui::SheetPage::Inspector));
    assert!(h.gui_mut().dismiss_top_layer());
    h.warm_up();
    assert_eq!(h.sheet().page, Some(crate::ui::SheetPage::Layers));
    assert!(h.gui_mut().dismiss_top_layer());
    h.warm_up();
    assert_eq!(h.sheet().page, None);
    assert!(!h.gui_mut().dismiss_top_layer(), "nothing is left");
}

/// **A click that dismisses an open popover does not fade** — the popup was what
/// the click was aimed at (egui closes it on the click outside), and the evidence
/// is recorded at press time because the popup is gone by the confirm frame.
#[test]
fn a_click_that_dismisses_a_popover_does_not_fade() {
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    h.close_layers();
    let (_, pill) = h
        .pill(0, crate::ui::PillKind::Site)
        .expect("the site pill is drawn");
    h.mouse_click(pill.center());
    h.warm_up();
    assert!(
        h.pill_popover().is_some(),
        "precondition: the popover is open"
    );

    h.mouse_click(h.map_center());
    h.warm_up();
    assert!(h.pill_popover().is_none(), "the click closes the popover");
    assert!(
        !h.faded() && chrome_on_screen(&h),
        "and that is all it does: the dismissal is not a fade gesture"
    );

    h.mouse_click(h.map_center());
    h.warm_up();
    assert!(h.faded(), "the follow-up click is the fade gesture");
}

/// **A first click on an inactive pane only activates — the fade needs a click on
/// the *already*-active pane** (§1.8).
#[test]
fn a_click_that_activates_a_pane_does_not_fade() {
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    h.set_pane_count(2);
    h.close_layers();
    let panes = h.pane_rects();
    assert_eq!(h.active_pane_index(), 0, "precondition: pane 0 active");

    h.mouse_click(panes[1].center());
    h.warm_up();
    assert_eq!(h.active_pane_index(), 1, "the click activated pane 1");
    assert!(
        !h.faded() && chrome_on_screen(&h),
        "activation must be all it did"
    );

    h.mouse_click(panes[1].center());
    h.warm_up();
    assert!(h.faded(), "the second click on the now-active pane fades");
}

/// **A feature click while faded unfades — its dialog must not open into an
/// invisible UI** (the consumed-click refinement in `ui_fade.rs`).
#[test]
fn a_consumed_click_while_faded_unfades() {
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    h.close_layers();
    h.gui_mut().enable_overlay_for_test(&known::RADAR_SITES);
    h.warm_up();
    let spot = h.map_center();
    let site_spot = egui::pos2(spot.x - 150.0, spot.y);
    h.place_site_at(0, "KTLX", site_spot);
    h.mouse_click(spot);
    h.warm_up();
    assert!(h.faded(), "precondition: faded");

    h.mouse_click(site_spot);
    assert!(h.click_consumed(), "precondition: the icon took the click");
    h.warm_up();
    assert!(
        !h.faded() && chrome_on_screen(&h),
        "the map's own answer belongs in a working UI"
    );
}

/// **A touch long-press starting on floating chrome does not raise the map
/// tooltip** (§5.9: `long_press_pos` is chrome-filtered like the click).
#[test]
fn a_long_press_on_floating_chrome_raises_no_map_tooltip() {
    let mut h = InputHarness::new();

    let map_spot = h.map_center();
    h.mouse_press(map_spot);
    let held = h.frames_for(10, 0.1);
    assert_eq!(
        held.touch.long_press_pos,
        Some(map_spot),
        "control: the detector works on bare map"
    );
    h.mouse_release(map_spot);
    h.frames_for(3, 0.3);

    let chrome_spot = h.timeline().rect.center();
    assert!(
        h.is_floating_layer_at(chrome_spot),
        "precondition: the spot is floating chrome"
    );
    h.mouse_press(chrome_spot);
    let held = h.frames_for(10, 0.1);
    assert_eq!(
        held.touch.long_press_pos, None,
        "a hold on chrome must not become a map long press"
    );
    h.mouse_release(chrome_spot);
}

/// **The loop and archive scrubbers resolve distinct widget ids** (§5.9's
/// same-auto-id-slot corner): the two forms share one row slot, so without distinct
/// ids a loop landing mid-drag would hand an archive drag to the frame-seek slider
/// — same id, new meaning.
#[test]
fn the_loop_and_archive_scrubbers_resolve_distinct_ids() {
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    let archive_id = h
        .widget_id_probes()
        .into_iter()
        .find(|(name, _)| *name == "timeline_scrubber")
        .expect("the archive form reports its id")
        .1;

    {
        let pane = h.gui_mut().pane_mut(0).unwrap();
        *pane.time_state_mut(&known::RADAR) = crate::radar_layer::begin_loop(
            600,
            squallar_radar::sites::get_radar_site("KTLX").unwrap(),
            squallar_radar::types::RenderView::PlanView,
        );
        pane.time_state_mut(&known::RADAR).frames = vec![crate::pane::LoopFrame {
            timestamp: chrono::Utc::now().naive_utc(),
            image: None,
            render_in_flight: false,
            render_failed: false,
        }];
        pane.park_on_frame(&known::RADAR, 0);
    }
    h.warm_up();
    let loop_id = h
        .widget_id_probes()
        .into_iter()
        .find(|(name, _)| *name == "timeline_scrubber_loop")
        .expect("the loop form reports its id")
        .1;

    assert_ne!(
        archive_id, loop_id,
        "the two scrubber forms share an id: a mid-drag form flip would \
         carry the drag across meanings"
    );
}

/// **The transport's stated width is its outer width** (§1.5's `min(880, full −
/// 24)`, the §5.9 bookkeeping fix): the surface on the glass, frame included, lands
/// on the formula — not the formula plus the frame's margins.
#[test]
fn the_transport_outer_width_is_the_stated_formula() {
    let h = InputHarness::with_screen(egui::vec2(800.0, 1200.0));
    let map = h.map_panel_rect();
    let expected = (map.width() - 24.0).min(880.0);
    let drawn = h.timeline().rect.width();
    assert!(
        (drawn - expected).abs() < 1.0,
        "the transport drew {drawn} pt wide; §1.5 states {expected} pt"
    );

    let h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    let drawn = h.timeline().rect.width();
    assert!(
        (drawn - 880.0).abs() < 1.0,
        "the transport drew {drawn} pt wide; §1.5 caps it at 880"
    );
}

/// **The sheet host draws no duplicate headers** (M7's sheet-header polish): the
/// sheet's title row is the single header — the stack's own header row and ⟨ do not
/// render there — while the wider hosts keep all of it.
///
/// The inspector's × is the exception, and deliberately so: it is the panel's
/// one close, and the host that suppressed it left the crumb's *other* × — a
/// deselect — as the only × in the body, forty points under the sheet's real
/// one. With one honest close there is nothing to suppress.
#[test]
fn the_sheet_host_draws_no_duplicate_headers() {
    let mut h = phone();
    h.open_layers();
    let stack = h.stack();
    assert!(stack.open, "precondition: the Layers page hosts the stack");
    assert_eq!(
        stack.header,
        egui::Rect::NOTHING,
        "the stack's own header must not draw under the sheet's title row"
    );
    assert_eq!(
        stack.collapse,
        egui::Rect::NOTHING,
        "the ⟨ collapse is the back-chain's job in the sheet"
    );
    assert!(
        h.text_painted_in(
            h.sheet_rect().expect("open"),
            "The same layer stack as on a desktop"
        ),
        "the Layers page must carry the §1.3 helper caption"
    );

    h.open_layer_in_inspector(&known::NWS_ALERTS);
    let insp = h.inspector();
    assert!(
        insp.close.is_positive(),
        "the crumb's × draws in the sheet too: it is the inspector's one close"
    );
    h.mouse_click(insp.close.center());
    h.warm_up();
    assert!(
        !h.inspector().open,
        "and it closes the inspector from the sheet host"
    );

    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    h.gui_mut().open_settings();
    h.warm_up();
    assert_ne!(h.stack().header, egui::Rect::NOTHING);
    assert_ne!(h.stack().collapse, egui::Rect::NOTHING);
    assert!(h.inspector().close.is_positive());
    assert!(
        !h.text_painted_in(h.screen_rect(), "The same layer stack as on a desktop"),
        "the helper caption is the phone page's alone"
    );
}

/// **The error surface outranks the fade** — the deliberate §1.8 refinement
/// recorded in `ui_fade.rs`: an error one accidental tap could hide is an error
/// unseen.
#[test]
fn the_error_surface_stays_visible_while_faded() {
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    h.gui_mut().apply(crate::shell_api::GuiEvent::Error(
        "the feed went away".to_owned(),
    ));
    h.warm_up();
    assert!(
        h.error_toast().is_none(),
        "precondition: unfaded, the status bar hosts the error and no toast \
         draws"
    );

    h.mouse_click(h.map_center());
    h.warm_up();
    assert!(h.faded(), "precondition: faded");
    assert_eq!(h.status_bar().rect, egui::Rect::NOTHING, "the bar is faded");
    let toast = h.error_toast().expect("the toast must carry the error");
    h.mouse_click(toast.close.center());
    h.warm_up();
    assert!(
        h.error_toast().is_none(),
        "the toast's \u{2715} must dismiss the error while faded"
    );

    let mut h = phone();
    h.gui_mut().apply(crate::shell_api::GuiEvent::Error(
        "the feed went away".to_owned(),
    ));
    h.warm_up();
    assert!(h.error_toast().is_some(), "precondition: the toast is up");
    h.mouse_click(h.pane_rects()[0].center());
    h.warm_up();
    assert!(h.faded(), "precondition: faded");
    assert!(
        h.error_toast().is_some(),
        "the fade must not take the error with it"
    );
}

/// M8-3. **The top bar has breathing room at every width.**
#[test]
fn the_top_bar_has_breathing_room_at_every_width() {
    for size in [
        egui::vec2(420.0, 800.0),
        egui::vec2(800.0, 800.0),
        egui::vec2(1400.0, 900.0),
    ] {
        let h = InputHarness::with_screen(size);
        let bar = h.top_bar().rect;
        assert!(
            bar.height() >= crate::ui::MIN_BAR_HEIGHT,
            "at {size:?} the top bar is {}pt tall, under its own floor of {}",
            bar.height(),
            crate::ui::MIN_BAR_HEIGHT,
        );
    }
}

/// M8-6. **A layer-less pane's stack body is the explained absence plus the one
/// action that applies.**
#[test]
fn a_layerless_stack_body_offers_the_caption_and_pane_properties() {
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    h.make_pane_unaimed_cross_section(0);
    h.open_layers();

    let stack = h.stack();
    assert!(
        stack.rows.is_empty(),
        "a cross-section pane has no layer rows"
    );
    assert_eq!(
        stack.add_top,
        egui::Rect::NOTHING,
        "no catalog button: the catalog shows map layers"
    );
    assert_ne!(
        stack.non_map_note,
        egui::Rect::NOTHING,
        "the explained absence was not drawn"
    );
    assert!(
        h.text_painted_in(stack.rect, crate::ui::NON_MAP_LAYERS_NOTE),
        "the caption's text never reached the glass"
    );
    assert_ne!(
        stack.props_button,
        egui::Rect::NOTHING,
        "the Pane properties... button was not drawn"
    );

    h.mouse_click(stack.props_button.center());
    h.warm_up();
    assert_eq!(
        h.inspector().mode,
        Some(crate::ui::InspectorSelection::PaneProps),
        "the button must open the inspector on Pane properties"
    );
}

/// M8-7. **The time chip and timestamp fall back to a real time on a non-map
/// pane.**
#[test]
fn the_time_chip_falls_back_to_a_map_panes_time_on_a_non_map_pane() {
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    assert!(
        h.timeline().timestamp.1.contains("--:--:--"),
        "precondition: a fresh session has no data time anywhere"
    );

    h.set_pane_count(2);
    h.make_pane_volume(1);
    let t = chrono::NaiveDate::from_ymd_opt(2026, 8, 10)
        .unwrap()
        .and_hms_opt(11, 11, 18)
        .unwrap();
    {
        let pane0 = h.gui_mut().pane_mut(0).expect("pane 0 exists");
        pane0.data_time = Some(t);
        pane0.viewing_live = false;
    }
    h.mouse_click(h.pane_rects()[1].center());
    h.warm_up();
    assert_eq!(h.active_pane_index(), 1, "precondition: pane 1 is active");

    let expected_time = h
        .gui_mut()
        .preferences
        .timezone
        .format_naive_utc(t, "%H:%M:%S");
    let stamp = h.timeline().timestamp.1.clone();
    assert!(
        stamp.contains(&expected_time),
        "the timestamp button reads {stamp:?}, not the map pane's {expected_time}"
    );
    assert!(
        stamp.contains("archive"),
        "the annotation must describe the fallback source (an archive-parked \
         map pane), got {stamp:?}"
    );

    h.mouse_click(h.timeline().collapse.center());
    h.warm_up();
    let chip = h.timeline().chip;
    assert!(
        h.text_painted_in(chip, &expected_time),
        "the collapsed chip does not show the fallback time"
    );
    assert!(
        !h.text_painted_in(chip, "--:--:--"),
        "the chip shows --:--:-- with a loaded map pane on screen"
    );
}

/// M8-8/9. **The collapsed chip sits above the bottom-edge bars and lays out on one
/// line.**
#[test]
fn the_collapsed_chip_clears_the_bars_and_never_wraps() {
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    h.mouse_click(h.timeline().collapse.center());
    h.warm_up();
    let chip = h.timeline().chip;
    let bar = h.status_bar().rect;
    assert_ne!(chip, egui::Rect::NOTHING, "precondition: the chip is up");
    assert_ne!(
        bar,
        egui::Rect::NOTHING,
        "precondition: the status bar is up"
    );
    assert!(
        !chip.intersects(bar) && chip.bottom() <= bar.top(),
        "the chip ({chip:?}) overlays the status bar ({bar:?})"
    );
    assert_single_line_chip(&h, chip);

    h.mouse_click(h.status_bar().collapse.center());
    h.warm_up();
    let chip = h.timeline().chip;
    let bar = h.status_bar().rect;
    assert!(
        bar.width() < 100.0,
        "precondition: the bar is down to its restore button, got {bar:?}"
    );
    assert!(
        !chip.intersects(bar),
        "the chip ({chip:?}) overlays the collapsed bar ({bar:?})"
    );
    let map = h.map_panel_rect();
    assert!(
        map.bottom() - chip.bottom() < 24.0,
        "the chip ({chip:?}) floats above open map instead of hugging the \
         bottom of {map:?}"
    );
    assert_single_line_chip(&h, chip);

    let mut h = InputHarness::with_screen(egui::vec2(420.0, 800.0));
    h.mouse_click(h.timeline().collapse.center());
    h.warm_up();
    let chip = h.timeline().chip;
    let bar = h.bottom_bar().rect;
    assert_ne!(chip, egui::Rect::NOTHING, "precondition: the chip is up");
    assert_ne!(
        bar,
        egui::Rect::NOTHING,
        "precondition: the bottom bar is up"
    );
    assert!(
        !chip.intersects(bar) && chip.bottom() <= bar.top(),
        "the chip ({chip:?}) overlays the bottom bar ({bar:?})"
    );
    assert_single_line_chip(&h, chip);
}

/// The chip's one-line claim, asserted on the glass: wider than tall, and its whole
/// label painted as a single text row.
fn assert_single_line_chip(h: &InputHarness, chip: egui::Rect) {
    assert!(
        chip.width() > chip.height(),
        "the chip is taller than wide ({chip:?}) - the wrapped-column bug"
    );
    let label = h
        .painted_text_rects()
        .into_iter()
        .find(|(rect, text)| chip.contains(rect.center()) && text.contains(":"))
        .expect("the chip painted no time text");
    assert!(
        label.0.height() < 22.0,
        "the chip's label wrapped: its galley is {}pt tall for {:?}",
        label.0.height(),
        label.1,
    );
}

/// M8-10. **A layer row is a full-width, comfortably tall click target.**
#[test]
fn layer_rows_are_full_width_click_targets() {
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    // **A stack the user has filled from the catalog.** A curated stack
    // starts at the handful of layers that ship enabled, and this test's
    // subject is a LONG list - the panel's scroll, its clamped height, the
    // ids a scrolled body keeps. Built the way a user builds one, rather
    // than relied on as a property of the build's layer count.
    h.fill_stack();
    h.open_layers();
    let stack = h.stack();
    assert!(
        stack.rows.len() >= 10,
        "precondition: the stack draws the layer inventory"
    );
    let panel_width = stack.rect.width();
    for row in &stack.rows {
        assert!(
            row.rect.height() >= 27.5,
            "{:?}'s row is only {}pt tall",
            row.kind,
            row.rect.height()
        );
        assert!(
            row.rect.width() >= 0.8 * panel_width,
            "{:?}'s row is {}pt wide in a {}pt panel - a text-width target",
            row.kind,
            row.rect.width(),
            panel_width
        );
    }

    let radar = h.stack_row(&known::RADAR).expect("the Radar row is drawn");
    let far_right = egui::pos2(radar.rect.right() - 6.0, radar.rect.center().y);
    h.mouse_click(far_right);
    h.warm_up();
    assert_eq!(
        h.inspector().mode,
        Some(crate::ui::InspectorSelection::Layer(known::RADAR)),
        "a click at the row's right end must select the layer"
    );

    let row = h.stack_row(&known::CITY_LABELS).expect("row drawn");
    let was_on = row.eye_on;
    h.mouse_click(row.eye.center());
    h.warm_up();
    assert_eq!(
        h.overlay_enabled(&known::CITY_LABELS),
        !was_on,
        "the eye stopped toggling under the full-width row"
    );
    assert_ne!(
        h.inspector().mode,
        Some(crate::ui::InspectorSelection::Layer(known::CITY_LABELS)),
        "the eye click leaked into a row selection"
    );
}

/// M9-18. **In-pane text stays inside its pane and clear of the pill row.**
#[test]
fn in_pane_text_stays_inside_its_pane_and_clear_of_the_pill_rows() {
    let mut h = InputHarness::with_screen(egui::vec2(1000.0, 900.0));
    h.set_pane_count(6);
    h.load_scan("KTLX");
    h.make_pane_cross_section(
        4,
        squallar_geo::GeoPoint {
            lat: 35.1,
            lon: -97.6,
        },
        squallar_geo::GeoPoint {
            lat: 35.5,
            lon: -97.0,
        },
    );
    h.close_layers();
    h.set_section_draw_armed(true);
    h.warm_up();

    let panes = h.pane_rects();
    let hints: Vec<(egui::Rect, String)> = h
        .painted_text_rects()
        .into_iter()
        .filter(|(_, text)| text.contains(crate::ui::map::SECTION_ARM_HINT))
        .collect();
    assert!(!hints.is_empty(), "the armed mode paints its hint chip");
    for (rect, text) in hints {
        assert!(
            panes
                .iter()
                .any(|pane| pane.expand(1.0).contains_rect(rect)),
            "the hint {text:?} at {rect:?} runs outside every pane - the \
             clipped-mid-word bug"
        );
    }

    for row in h.pill_rows() {
        for (rect, text) in h.painted_text_rects() {
            if rect.intersects(row.rect) && !row.rect.expand(1.0).contains_rect(rect) {
                panic!(
                    "{text:?} at {rect:?} collides with pane {}'s pill row at \
                     {:?} - the wrapped-row clearance failure",
                    row.pane_idx, row.rect
                );
            }
        }
    }
}

/// M9-13. **A mouse click-drag on a panel body scrolls it.**
#[test]
fn a_mouse_drag_on_a_panel_body_scrolls_it() {
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 420.0));
    // **A stack the user has filled from the catalog.** A curated stack
    // starts at the handful of layers that ship enabled, and this test's
    // subject is a LONG list - the panel's scroll, its clamped height, the
    // ids a scrolled body keeps. Built the way a user builds one, rather
    // than relied on as a property of the build's layer count.
    h.fill_stack();
    h.warm_up();
    let scroll_id = h
        .widget_id_probes()
        .iter()
        .find(|(name, _)| *name == "layers_scroll")
        .expect("the stack's scroll area reports its id")
        .1;
    let before = h.scroll_offset(scroll_id).unwrap_or_default().y;

    let row = h.stack().rows[1].clone();
    let start = row.name.center();
    h.mouse_press(start);
    h.frame_after(FRAME_DT);
    h.mouse_move(start - egui::vec2(0.0, 40.0));
    h.frame_after(FRAME_DT);
    h.mouse_move(start - egui::vec2(0.0, 90.0));
    h.frame_after(FRAME_DT);
    let after = h.scroll_offset(scroll_id).unwrap_or_default().y;
    assert!(
        after > before + 20.0,
        "a mouse drag on the row body moved the offset only {before} to \
         {after} - drag-to-scroll is still touch-only"
    );
    h.mouse_release(start - egui::vec2(0.0, 90.0));
    h.warm_up();

    assert_ne!(
        h.inspector().mode,
        Some(crate::ui::InspectorSelection::Layer(row.kind)),
        "the scroll drag leaked into a row selection"
    );
}

/// M9-1. **A row's text block sits vertically centred in its row.**
#[test]
fn stack_row_text_is_vertically_centred() {
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    // **A stack the user has filled from the catalog.** A curated stack
    // starts at the handful of layers that ship enabled, and this test's
    // subject is a LONG list - the panel's scroll, its clamped height, the
    // ids a scrolled body keeps. Built the way a user builds one, rather
    // than relied on as a property of the build's layer count.
    h.fill_stack();
    h.warm_up();
    let rows = h.stack().rows;
    assert!(rows.len() >= 10, "precondition: the stack draws its rows");
    assert!(
        rows.iter().any(|row| row.status_line.is_some())
            && rows.iter().any(|row| row.status_line.is_none()),
        "precondition: both block shapes are on screen"
    );
    for row in rows {
        assert_ne!(row.name, egui::Rect::NOTHING, "{:?} drew no text", row.kind);
        let off = (row.name.center().y - row.rect.center().y).abs();
        assert!(
            off <= 1.5,
            "{:?}'s text block sits {off:.1}pt off its row's vertical centre",
            row.kind
        );
    }
}

/// M8-11. **The fade hides the 3D pane's Volume Alpha corner button.**
#[test]
fn the_fade_hides_the_volume_alpha_button_and_restores_it() {
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    h.set_pane_count(2);
    h.make_pane_volume(1);
    assert!(
        h.alpha_buttons().iter().any(|&(idx, _)| idx == 1),
        "precondition: the 3D pane draws its corner button"
    );

    let spot = h.pane_rects()[0].center();
    h.mouse_click(spot);
    h.warm_up();
    assert!(h.faded(), "precondition: the bare-map click fades");
    assert!(
        h.alpha_buttons().is_empty(),
        "the corner button must not render while faded"
    );
    assert!(
        !h.text_painted_in(
            h.screen_rect(),
            crate::ui::map::volume_alpha_editor::ALPHA_BUTTON_LABEL
        ),
        "the button's label survives on the glass while faded"
    );

    h.mouse_click(spot);
    h.warm_up();
    assert!(!h.faded(), "precondition: the second tap restores");
    assert!(
        h.alpha_buttons().iter().any(|&(idx, _)| idx == 1),
        "the corner button must return on the unfade"
    );
}

/// M8-11b. **A click on the Volume Alpha button is the button's click, and a click
/// on a 3D pane is never the fade gesture.**
#[test]
fn the_volume_alpha_button_takes_the_click_the_pane_would_have_faded_on() {
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    h.set_pane_count(2);
    h.make_pane_volume(1);

    let editor_open = |h: &mut InputHarness| {
        h.gui_mut()
            .pane(1)
            .expect("pane 1 exists")
            .volume()
            .expect("pane 1 is a 3D pane")
            .alpha_editor_open
    };

    h.mouse_click(h.pane_rects()[1].center());
    h.warm_up();
    assert_eq!(
        h.active_pane_index(),
        1,
        "precondition: the first click activates the 3D pane"
    );
    assert!(
        !h.faded(),
        "precondition: the activating click never fades (§1.8)"
    );
    assert!(!editor_open(&mut h), "precondition: the editor starts shut");

    let button = h
        .alpha_buttons()
        .into_iter()
        .find(|&(idx, _)| idx == 1)
        .expect("precondition: the 3D pane draws its Volume alpha corner button")
        .1;
    h.mouse_click(button.center());
    h.warm_up();
    assert!(
        editor_open(&mut h),
        "the click never reached the Volume Alpha button - the editor did not open"
    );
    assert!(
        !h.faded(),
        "the click also reached the pane underneath: it hid the UI like a bare-map tap"
    );

    h.gui_mut()
        .pane_mut(1)
        .expect("pane 1 exists")
        .volume_mut()
        .expect("pane 1 is a 3D pane")
        .alpha_editor_open = false;
    h.mouse_click(h.pane_rects()[1].center());
    h.warm_up();
    assert!(
        !h.faded(),
        "a click on a 3D pane is the pane's own gesture, not the map fade"
    );

    let map_spot = h.pane_rects()[0].center();
    h.mouse_click(map_spot);
    h.warm_up();
    h.mouse_click(map_spot);
    h.warm_up();
    assert!(
        h.faded(),
        "the map pane's bare-surface tap must still fade the UI"
    );
}

/// M8-12. **The active-pane border shows all four edges at every grid position.**
#[test]
fn every_pane_border_lies_inside_its_pane_at_every_position() {
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    h.set_pane_count(6);
    h.close_layers();
    let map = h.map_panel_rect();

    for target in 0..6 {
        if h.active_pane_index() != target {
            h.mouse_click(h.pane_rects()[target].center());
            h.warm_up();
        }
        assert_eq!(h.active_pane_index(), target, "activation failed");
        assert!(!h.faded(), "activation must not fade");

        let borders = h.pane_borders();
        assert_eq!(borders.len(), 6, "every pane draws its border");
        let rects = h.pane_rects();
        for &(idx, painted, marks) in &borders {
            assert_eq!(
                marks.is_active,
                idx == target,
                "pane {idx}'s border misreports the active highlight"
            );
            assert!(
                rects[idx].contains_rect(painted) && map.contains_rect(painted),
                "pane {idx}'s border ({painted:?}) leaks outside its pane \
                 ({:?}) or the map ({map:?}) - the clipped-edges bug",
                rects[idx],
            );
        }
    }
}

/// M8-13. **The release frame of a handle drag paints the dropped line, never the
/// stale pre-drag one.**
#[test]
fn the_release_frame_paints_the_dropped_section_line() {
    let (mut h, _a, b) = harness_with_committed_section();

    let b_px = h.screen_of(0, b);
    let target_px = b_px + egui::vec2(-70.0, 45.0);
    h.mouse_move(b_px);
    h.frame();
    h.mouse_press(b_px);
    h.frame();
    for step in 1..=4 {
        h.mouse_move(b_px + (target_px - b_px) * (step as f32 / 4.0));
        h.frame();
    }

    let painted_b = |h: &InputHarness| -> egui::Pos2 {
        let tracks = h.section_tracks();
        let &(_, _, a_end, b_end) = tracks
            .iter()
            .find(|&&(map_pane, section_pane, ..)| map_pane == 0 && section_pane == 1)
            .expect("the map pane paints its section track");
        if (a_end - target_px).length() < (b_end - target_px).length() {
            a_end
        } else {
            b_end
        }
    };

    h.mouse_release(target_px);
    h.frame();
    let on_release = painted_b(&h);
    assert!(
        (on_release - target_px).length() < 8.0,
        "the release frame painted the line's end at {on_release:?}, not the \
         drop at {target_px:?} - the pop-back"
    );
    assert!(
        (on_release - b_px).length() > 20.0,
        "the release frame still painted the pre-drag end at {on_release:?}"
    );

    h.frame();
    let after = painted_b(&h);
    assert!(
        (after - target_px).length() < 8.0,
        "the applied frame painted {after:?}, not the drop at {target_px:?}"
    );
}

/// **Every transport control emits the exact payload the frontend acts on.**
#[test]
fn the_transport_controls_emit_the_exact_payloads_the_frontend_acts_on() {
    use crate::actions::GuiAction;

    /// The frame's navigation-shaped actions, so the assertions cannot be satisfied
    /// by the overlay fetches that share the vector.
    fn nav(h: &InputHarness) -> Vec<String> {
        h.last_actions()
            .iter()
            .filter(|a| {
                matches!(
                    a,
                    GuiAction::NavigateTime { .. }
                        | GuiAction::NavigateOneScan { .. }
                        | GuiAction::JumpToLive { .. }
                        | GuiAction::EnableLoop { .. }
                        | GuiAction::DisableLoop { .. }
                )
            })
            .map(|a| format!("{a}"))
            .collect()
    }

    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    h.load_scan("KTLX");

    h.mouse_click(h.timeline().live.0.center());
    assert_eq!(
        nav(&h),
        Vec::<String>::new(),
        "Live while already live must emit nothing"
    );

    h.mouse_click(h.timeline().back.center());
    assert_eq!(
        nav(&h),
        vec!["Navigate time by -600 seconds for pane 0"],
        "Back must step the active pane one default step into the archive"
    );
    assert!(
        !h.gui_mut().pane(0).expect("pane 0").viewing_live,
        "Back must park the pane out of live"
    );
    h.warm_up();

    h.mouse_click(h.timeline().fwd.0.center());
    assert_eq!(
        nav(&h),
        vec!["Navigate time by 600 seconds for pane 0"],
        "Forward must step the active pane one default step toward now"
    );
    h.warm_up();

    h.mouse_click(h.timeline().live.0.center());
    assert_eq!(
        nav(&h),
        vec!["Jump to live for pane 0"],
        "Live from the archive must jump exactly the active pane"
    );
    h.warm_up();

    h.mouse_click(h.timeline().step_dropdown.center());
    h.frame_after(FRAME_DT);
    let entry = h
        .painted_text_rects()
        .into_iter()
        .find(|(_, text)| text == "1 scan")
        .expect("the open step combo lists '1 scan'");
    h.mouse_click(entry.0.center());
    assert_eq!(
        h.gui_mut().pane(0).expect("pane 0").time.step.as_secs(),
        0,
        "picking '1 scan' must write the active pane's step"
    );
    h.warm_up();
    h.mouse_click(h.timeline().back.center());
    assert_eq!(
        nav(&h),
        vec!["Navigate one scan (forward=false) for pane 0"],
        "Back at '1 scan' must ask for the adjacent scan, not a time step"
    );
    h.warm_up();
    h.mouse_click(h.timeline().fwd.0.center());
    assert_eq!(
        nav(&h),
        vec!["Navigate one scan (forward=true) for pane 0"],
        "Forward at '1 scan' must ask for the adjacent scan"
    );
    h.warm_up();

    h.mouse_click(h.timeline().loop_toggle.0.center());
    assert_eq!(
        nav(&h),
        vec!["Enable loop for pane 0 (3600s lookback)"],
        "the loop toggle must enable with the shared lookback"
    );
    let site = squallar_radar::sites::get_radar_site("KTLX").expect("known site");
    *h.gui_mut()
        .pane_mut(0)
        .expect("pane 0")
        .time_state_mut(&known::RADAR) =
        crate::radar_layer::begin_loop(3600, site, squallar_radar::types::RenderView::PlanView);
    h.warm_up();
    let (rect, on) = h.timeline().loop_toggle;
    assert!(on, "precondition: the probe reports the loop as on");
    h.mouse_click(rect.center());
    assert_eq!(
        nav(&h),
        vec!["Disable loop for pane 0"],
        "the on toggle must disable the loop"
    );
}

/// **The scrubber's release payload names the released moment** — not merely "some
/// NavigateTime".
#[test]
fn the_scrubbers_release_payload_names_the_released_moment() {
    use crate::actions::GuiAction;

    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    h.load_scan("KTLX");

    let scrub = h.timeline().scrubber;
    let frac = 0.4_f32;
    let target = egui::pos2(scrub.left() + scrub.width() * frac, scrub.center().y);
    let scan_time = h
        .gui_mut()
        .pane(0)
        .expect("pane 0")
        .scan_info
        .as_ref()
        .expect("a scan is loaded")
        .timestamp;
    let lookback = 3600.0_f32;
    let expected = |now: chrono::NaiveDateTime, frac: f32| {
        let target = now - chrono::Duration::seconds((lookback * (1.0 - frac)) as i64);
        (target - scan_time).num_seconds()
    };

    let before = chrono::Utc::now().naive_utc();
    h.mouse_press(scrub.center());
    h.frame_after(FRAME_DT);
    h.mouse_move(target);
    h.frame_after(FRAME_DT);
    h.mouse_release(target);
    h.frame_after(0.05);
    let after = chrono::Utc::now().naive_utc();

    let step = h
        .last_actions()
        .iter()
        .find_map(|a| match a {
            GuiAction::NavigateTime {
                pane_idx: 0,
                step_secs,
            } => Some(*step_secs),
            _ => None,
        })
        .expect("releasing the scrub mid-rail must emit NavigateTime for pane 0");
    let low = expected(before, frac) - 60;
    let high = expected(after, frac) + 60;
    assert!(
        (low..=high).contains(&step),
        "the released moment's step was {step}, outside [{low}, {high}] - \
         the payload does not name the released moment"
    );
}

/// **The loop toggle is a real button at the bar's interact size, and its on-state
/// is painted in the selection colour.**
#[test]
fn the_loop_toggle_is_a_real_button_with_a_visible_on_state() {
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    h.load_scan("KTLX");
    h.frames_for(5, 0.1);

    let interact = h.interact_size();
    let (rect, on) = h.timeline().loop_toggle;
    assert!(!on, "precondition: no loop is running");
    assert!(
        rect.width() >= interact.x - 0.5 && rect.height() >= interact.y - 0.5,
        "the loop toggle's interact rect {rect:?} is below the bar's \
         minimum interact size {interact:?}"
    );

    let selection = h.selection_bg_fill();
    assert!(
        !h.painted_fills_within(rect, 1.0).is_empty(),
        "the off-state toggle painted no background frame at all - the \
         frameless selectable-label form is back"
    );
    assert!(
        !h.painted_fills_within(rect, 1.0).contains(&selection),
        "the off-state toggle is painted in the selection colour"
    );

    let site = squallar_radar::sites::get_radar_site("KTLX").expect("known site");
    *h.gui_mut()
        .pane_mut(0)
        .expect("pane 0")
        .time_state_mut(&known::RADAR) =
        crate::radar_layer::begin_loop(3600, site, squallar_radar::types::RenderView::PlanView);
    h.frames_for(5, 0.1);
    let (rect, on) = h.timeline().loop_toggle;
    assert!(on, "the probe must report the loop as on");
    assert!(
        h.painted_fills_within(rect, 1.0).contains(&selection),
        "the on-state toggle is not painted in the selection colour: \
         fills {:?}",
        h.painted_fills_within(rect, 1.0)
    );
}

/// A square (lat, lon) ring of `half_deg` about a point, closed as GeoJSON rings
/// arrive.
fn ring_about(lat: f64, lon: f64, half_deg: f64) -> Vec<(f64, f64)> {
    vec![
        (lat - half_deg, lon - half_deg),
        (lat - half_deg, lon + half_deg),
        (lat + half_deg, lon + half_deg),
        (lat + half_deg, lon - half_deg),
        (lat - half_deg, lon - half_deg),
    ]
}

/// One NWS alert covering a square about (`lat`, `lon`), shaped like the
/// zone-resolved alerts `nws::zones` builds: geometry in `features`, one feature
/// per affected area.
fn alert_over(
    id: &str,
    event: &str,
    lat: f64,
    lon: f64,
) -> squallar_overlays::nws::alert::NwsAlert {
    use squallar_overlays::nws::alert::{AlertCategory, NwsAlert};
    let (fill, stroke) = squallar_overlays::nws::colors::alert_color(event);
    NwsAlert {
        id: id.to_string(),
        event: event.to_string(),
        category: AlertCategory::from_event(event),
        severity: "Severe".parse().expect("a CAP severity"),
        urgency: "Immediate".parse().expect("a CAP urgency"),
        certainty: "Observed".parse().expect("a CAP certainty"),
        headline: None,
        description: String::new(),
        instruction: None,
        area_desc: String::new(),
        sender_name: String::new(),
        effective: String::new(),
        expires: String::new(),
        onset: None,
        ends: None,
        valid_from: None,
        valid_until: None,
        affected_zones: Vec::new(),
        features: std::sync::Arc::new(vec![squallar_overlays::types::OverlayFeature::new(
            vec![vec![ring_about(lat, lon, 0.25)]],
            fill,
            stroke,
            event.to_string(),
            String::new(),
            squallar_overlays::types::HatchPattern::None,
        )]),
    }
}

/// A **zone-based** NWS alert, shaped as `nws::alert` admits one and
/// `nws::zones::resolve_zone_geometry` then fills it: `affectedZones` listed, no
/// `geometry` of its own, and `zones_resolved` features — one per zone whose fetch
/// succeeded on this poll, which is `0` when they all failed.
fn zone_alert_over(
    id: &str,
    event: &str,
    lat: f64,
    lon: f64,
    zones_resolved: usize,
) -> squallar_overlays::nws::alert::NwsAlert {
    let mut alert = alert_over(id, event, lat, lon);
    alert.affected_zones = (0..3)
        .map(|i| format!("https://api.weather.gov/zones/county/OKC{i:03}"))
        .collect();
    let (fill, stroke) = squallar_overlays::nws::colors::alert_color(event);
    alert.features = std::sync::Arc::new(
        (0..zones_resolved)
            .map(|i| {
                squallar_overlays::types::OverlayFeature::new(
                    vec![vec![ring_about(lat + 0.1 * i as f64, lon, 0.25)]],
                    fill,
                    stroke,
                    event.to_string(),
                    String::new(),
                    squallar_overlays::types::HatchPattern::None,
                )
            })
            .collect(),
    );
    alert
}

/// Feed `alerts` in through the production ingest path, exactly as the national
/// fetch delivers them.
fn ingest_alerts(h: &mut InputHarness, alerts: Vec<squallar_overlays::nws::alert::NwsAlert>) {
    use squallar_overlays::render::overlay_state::{OverlayFetchResult, OverlayRegistry};
    h.gui_mut().overlays.apply_fetch_result(
        OverlayFetchResult {
            kind: known::NWS_ALERTS,
            data: OverlayRegistry::nws_alerts_payload(alerts),
        },
        &PaneRef::bare(0),
    );
}

/// A click inside a warning's polygon still selects that warning.
#[test]
fn a_click_inside_an_alert_polygon_still_selects_it() {
    let mut h = InputHarness::new();
    h.gui_mut().enable_overlay_for_test(&known::NWS_ALERTS);
    h.warm_up();

    let target = h.pane_rects()[0].center();
    let ground = h.ground_at(0, target);
    ingest_alerts(
        &mut h,
        vec![alert_over("a", "Tornado Warning", ground.y(), ground.x())],
    );
    h.warm_up();

    h.mouse_click(target);

    let selected = &h.gui_mut().overlays.selected_overlays;
    assert_eq!(
        selected.len(),
        1,
        "a click in the middle of a tornado warning selected {} items",
        selected.len()
    );
    let prefs = squallar_units::UserPreferences::default();
    assert_eq!(
        selected[0].popup_content(&prefs).title,
        "Tornado Warning",
        "the click selected something, but not the warning it landed in"
    );
}

/// …and a click well outside every polygon still selects nothing, so the test above
/// cannot pass by selecting everything the handler holds.
#[test]
fn a_click_outside_every_alert_polygon_still_selects_nothing() {
    let mut h = InputHarness::new();
    h.gui_mut().enable_overlay_for_test(&known::NWS_ALERTS);
    h.warm_up();

    let pane = h.pane_rects()[0];
    let elsewhere = h.ground_at(0, pane.center());
    ingest_alerts(
        &mut h,
        vec![alert_over(
            "a",
            "Tornado Warning",
            elsewhere.y() + 10.0,
            elsewhere.x() + 10.0,
        )],
    );
    h.warm_up();

    h.mouse_click(pane.center());

    assert!(
        h.gui_mut().overlays.selected_overlays.is_empty(),
        "a click ten degrees from the only warning selected it anyway"
    );
}

/// An MD writes its number on the map on an ordinary frame — no click, no pointer.
#[test]
fn an_md_still_labels_itself_on_a_frame_with_no_click() {
    use squallar_overlays::render::overlay_state::{OverlayFetchResult, OverlayRegistry};
    use squallar_overlays::spc::colors::{md_fill_color, md_stroke_color};
    use squallar_overlays::spc::discussion::{MdType, SpcDiscussion};

    let mut h = InputHarness::new();
    h.gui_mut().enable_overlay_for_test(&known::SPC_DISCUSSIONS);
    h.warm_up();

    let ground = h.ground_at(0, h.pane_rects()[0].center());
    let md_type = MdType::Convective;
    let polygon = vec![ring_about(ground.y(), ground.x(), 0.25)];
    let md = SpcDiscussion {
        number: 1234,
        title: "Mesoscale Discussion #1234".into(),
        text: String::new(),
        link: String::new(),
        md_type,
        polygon: polygon.clone(),
        feature: squallar_overlays::types::OverlayFeature::new(
            vec![polygon],
            md_fill_color(&md_type),
            md_stroke_color(&md_type),
            "MD 1234".into(),
            String::new(),
            squallar_overlays::types::HatchPattern::None,
        ),
        concerning: None,
        valid_from: None,
        valid_until: None,
    };
    h.gui_mut().overlays.apply_fetch_result(
        OverlayFetchResult {
            kind: known::SPC_DISCUSSIONS,
            data: OverlayRegistry::spc_discussions_payload(vec![md]),
        },
        &PaneRef::bare(0),
    );

    h.warm_up();

    assert!(
        h.painted_text_strings().iter().any(|t| t == "MD 1234"),
        "no MD label was painted on a frame with no click. Painted: {:?}",
        h.painted_text_strings()
    );
}

/// The cache token the last frame asked `kind`'s raster to be keyed by — the value
/// `OverlayTextureCache::needs_rerender` compares the cached texture against, so
/// two frames agreeing on it means no re-rasterize.
fn requested_cache_token(h: &InputHarness, kind: &LayerId) -> u64 {
    let tokens: Vec<u64> = h
        .last_actions()
        .iter()
        .filter_map(|a| match a {
            GuiAction::RenderOverlay {
                overlay_kind,
                data_generation,
                ..
            } if overlay_kind == kind => Some(*data_generation),
            _ => None,
        })
        .collect();
    assert!(
        !tokens.is_empty(),
        "fixture must reach the render path — no RenderOverlay for {kind:?} was emitted"
    );
    tokens[0]
}

/// How many rasterizes the last frame asked for, for `kind`.
fn rasterizes_requested(h: &InputHarness, kind: &LayerId) -> usize {
    h.last_actions()
        .iter()
        .filter(
            |a| matches!(a, GuiAction::RenderOverlay { overlay_kind, .. } if overlay_kind == kind),
        )
        .count()
}

/// Stand every asking pane's cache up as a landed render would leave it: stamped
/// with exactly the bounds, zoom and token the last frame requested.
fn settle_overlay_cache(h: &mut InputHarness, kind: &LayerId) {
    let requests: Vec<_> = h
        .last_actions()
        .iter()
        .filter_map(|a| match a {
            GuiAction::RenderOverlay {
                pane_idx,
                overlay_kind,
                geo_bounds,
                texture,
                data_generation,
                zoom,
            } if overlay_kind == kind => {
                Some((*pane_idx, *geo_bounds, *texture, *data_generation, *zoom))
            }
            _ => None,
        })
        .collect();
    assert!(
        !requests.is_empty(),
        "fixture: nothing asked for a {kind:?} raster, so there is no landed \
         render to stand in for"
    );
    for (pane_idx, geo_bounds, plan, token, zoom) in requests {
        let texture = h.ctx.load_texture(
            format!("settled-{kind:?}-{pane_idx}"),
            egui::ColorImage::filled([1, 1], egui::Color32::RED),
            egui::TextureOptions::default(),
        );
        h.gui_mut().panes_mut()[pane_idx]
            .overlay_cache_mut(kind)
            .show(crate::overlay_cache::OverlayTextureData {
                texture,
                placed: squallar_geo::PlacedRaster::of(plan.coverage(&geo_bounds)),
                data_generation: token,
                render_zoom: zoom,
                width: plan.width,
                height: plan.height,
                radar_meta: None,
                hit_map: None,
            });
    }
}

/// A poll that returns the same warning set must not re-rasterize the alert
/// overlay, and the map pane is where that is decided.
#[test]
fn an_unchanged_warning_set_does_not_re_rasterize_the_alert_overlay() {
    let mut h = InputHarness::new();
    h.gui_mut().enable_overlay_for_test(&known::NWS_ALERTS);
    h.warm_up();

    let ground = h.ground_at(0, h.pane_rects()[0].center());
    let warning_set = || vec![alert_over("a", "Tornado Warning", ground.y(), ground.x())];

    ingest_alerts(&mut h, warning_set());
    h.warm_up();
    settle_overlay_cache(&mut h, &known::NWS_ALERTS);
    h.warm_up();
    assert_eq!(
        rasterizes_requested(&h, &known::NWS_ALERTS),
        0,
        "fixture: a settled cache with nothing new must ask for nothing, or a \
         zero below would say nothing about the fix",
    );

    for poll in 1..=10 {
        let generation_before = h.gui_mut().overlays.data_generation(&known::NWS_ALERTS);
        ingest_alerts(&mut h, warning_set());
        h.warm_up();
        assert_ne!(
            h.gui_mut().overlays.data_generation(&known::NWS_ALERTS),
            generation_before,
            "fixture: poll {poll} must really have replaced the data — that bump \
             is exactly what the pane used to key on",
        );
        assert_eq!(
            rasterizes_requested(&h, &known::NWS_ALERTS),
            0,
            "poll {poll} of an unchanged warning set re-rasterized the whole \
             alert overlay: a worker pass plus a ~47 ms frame-thread ColorImage \
             conversion for a byte-identical texture",
        );
    }

    ingest_alerts(
        &mut h,
        vec![
            alert_over("a", "Tornado Warning", ground.y(), ground.x()),
            alert_over(
                "b",
                "Severe Thunderstorm Warning",
                ground.y() + 0.5,
                ground.x(),
            ),
        ],
    );
    h.warm_up();
    assert_eq!(
        rasterizes_requested(&h, &known::NWS_ALERTS),
        1,
        "a newly issued warning did not buy a re-rasterize, so the ten zeros \
         above are a cache that never refreshes rather than one that refreshes \
         when the picture moves",
    );
}

/// …and the other half, or the pane would sit on a stale warning for ever: a
/// warning **issuing** or **expiring** must move the token.
#[test]
fn a_changed_warning_set_moves_the_alert_overlays_cache_token() {
    let mut h = InputHarness::new();
    h.gui_mut().enable_overlay_for_test(&known::NWS_ALERTS);
    h.warm_up();

    let ground = h.ground_at(0, h.pane_rects()[0].center());
    ingest_alerts(
        &mut h,
        vec![alert_over("a", "Tornado Warning", ground.y(), ground.x())],
    );
    h.warm_up();
    let one_warning = requested_cache_token(&h, &known::NWS_ALERTS);

    ingest_alerts(
        &mut h,
        vec![
            alert_over("a", "Tornado Warning", ground.y(), ground.x()),
            alert_over(
                "b",
                "Severe Thunderstorm Warning",
                ground.y() + 0.5,
                ground.x(),
            ),
        ],
    );
    h.warm_up();
    let two_warnings = requested_cache_token(&h, &known::NWS_ALERTS);
    assert_ne!(
        two_warnings, one_warning,
        "a newly issued warning left the pane on its old raster",
    );

    ingest_alerts(
        &mut h,
        vec![alert_over(
            "b",
            "Severe Thunderstorm Warning",
            ground.y() + 0.5,
            ground.x(),
        )],
    );
    h.warm_up();
    assert_ne!(
        requested_cache_token(&h, &known::NWS_ALERTS),
        two_warnings,
        "an expired warning stayed painted",
    );
}

/// The same warning, on a later poll, with the counties it draws — through the
/// whole pane, because the consequence is a warning that is not on the map.
#[test]
fn a_warning_that_gains_its_polygons_re_rasterizes_the_alert_overlay() {
    let mut h = InputHarness::new();
    h.gui_mut().enable_overlay_for_test(&known::NWS_ALERTS);
    h.warm_up();

    let ground = h.ground_at(0, h.pane_rects()[0].center());
    let zone_warning = |resolved| {
        vec![zone_alert_over(
            "a",
            "Tornado Warning",
            ground.y(),
            ground.x(),
            resolved,
        )]
    };

    ingest_alerts(&mut h, zone_warning(0));
    h.warm_up();
    settle_overlay_cache(&mut h, &known::NWS_ALERTS);
    h.warm_up();
    assert_eq!(
        rasterizes_requested(&h, &known::NWS_ALERTS),
        0,
        "fixture: the empty-geometry raster must have settled, or the ask below \
         is just the cache still filling",
    );

    ingest_alerts(&mut h, zone_warning(3));
    h.warm_up();
    assert_eq!(
        rasterizes_requested(&h, &known::NWS_ALERTS),
        1,
        "a warning arrived with its counties and the pane kept the raster that \
         does not contain it — the warning stays off the map until the user \
         happens to pan or zoom",
    );
}

/// Put the pane's map at `zoom`, which is all a wheel or pinch gesture leaves
/// behind once it has been resolved.
fn set_pane_zoom(h: &mut InputHarness, zoom: f64) {
    h.gui_mut().panes_mut()[0]
        .map_memory
        .set_zoom(zoom)
        .expect("zoom within walkers' range");
}

/// The zoom the last frame's `RenderOverlay` for `kind` was keyed at.
fn requested_render_zoom(h: &InputHarness, kind: &LayerId) -> i32 {
    h.last_actions()
        .iter()
        .find_map(|a| match a {
            GuiAction::RenderOverlay {
                overlay_kind, zoom, ..
            } if overlay_kind == kind => Some(*zoom),
            _ => None,
        })
        .expect("no RenderOverlay was emitted for this kind")
}

/// One alert overlay, rasterised and landed, with the map at `zoom`, and the frame
/// loop *idle* — nothing else on the pane still asking to be redrawn.
fn settled_alert_pane(zoom: f64) -> InputHarness {
    let mut h = InputHarness::new();
    h.gui_mut().enable_overlay_for_test(&known::NWS_ALERTS);
    h.warm_up();
    let ground = h.ground_at(0, h.pane_rects()[0].center());
    ingest_alerts(
        &mut h,
        vec![alert_over("a", "Tornado Warning", ground.y(), ground.x())],
    );
    set_pane_zoom(&mut h, zoom);
    h.warm_up();
    settle_overlay_cache(&mut h, &known::NWS_ALERTS);
    for _ in 0..60 {
        if h.repaint_delay() == std::time::Duration::MAX {
            break;
        }
        h.frame_after(0.25);
    }
    assert_eq!(
        rasterizes_requested(&h, &known::NWS_ALERTS),
        0,
        "fixture: a settled cache with nothing new must ask for nothing"
    );
    h
}

/// Land every raster the pane asks for, up to `budget` of them, and report how many
/// it took to go quiet.
fn renders_until_quiet(h: &mut InputHarness, kind: &LayerId, budget: usize) -> usize {
    for landed in 0..budget {
        h.frame();
        if rasterizes_requested(h, kind) == 0 {
            return landed;
        }
        settle_overlay_cache(h, kind);
    }
    budget
}

/// **The raster that never stops.**
#[test]
fn every_zoom_the_map_offers_reaches_a_cache_that_is_satisfied() {
    for z in [0.0f64, 1.0, 2.0, 3.0, 4.0, 5.0, 7.0, 10.0, 14.0, 20.0, 26.0] {
        let mut h = settled_alert_pane(7.0);
        set_pane_zoom(&mut h, z);
        let landed = renders_until_quiet(&mut h, &known::NWS_ALERTS, 20);
        assert!(
            landed < 20,
            "at zoom {z} the pane asked for a raster on every one of 20 \
             consecutive frames, each one landed in full. Nothing is moving \
             and nothing decays: this is a texture upload per frame for as long \
             as the app is open",
        );
    }
}

/// Walk the zoom by `step` for `frames` frames, one 120 Hz frame per step, and
/// return how many rasterizes were asked for along the way.
fn zoom_gesture(h: &mut InputHarness, from: f64, step: f64, frames: usize) -> usize {
    let mut asked = 0;
    for i in 1..=frames {
        set_pane_zoom(h, from + step * i as f64);
        h.frame_after(1.0 / 120.0);
        asked += rasterizes_requested(h, &known::NWS_ALERTS);
    }
    asked
}

/// The win. A zoom gesture that stays inside `ZOOM_REBUILD_BAND` costs **no**
/// rasterizes at all while it is moving.
#[test]
fn a_zoom_inside_the_band_asks_for_nothing_while_it_moves() {
    const Z0: f64 = 7.0;
    let mut h = settled_alert_pane(Z0);

    let asked = zoom_gesture(&mut h, Z0, 0.05, 12);
    assert_eq!(
        asked, 0,
        "a zoom gesture well inside ZOOM_REBUILD_BAND asked for {asked} \
         rasterizes while it was moving; the cache is keyed on the zoom itself \
         again rather than on a band"
    );
}

/// …and the other half: past the band, it does re-rasterize, so the zero above is a
/// tolerance and not a cache that has stopped listening.
#[test]
fn a_zoom_past_the_band_re_rasterizes_while_it_moves() {
    const Z0: f64 = 7.0;
    let mut h = settled_alert_pane(Z0);

    let inside = zoom_gesture(&mut h, Z0, 0.05, 12);
    assert_eq!(inside, 0, "fixture: the first 0.6 must be free");

    set_pane_zoom(&mut h, Z0 + crate::overlay_cache::ZOOM_REBUILD_BAND + 0.01);
    h.frame();
    assert_eq!(
        rasterizes_requested(&h, &known::NWS_ALERTS),
        1,
        "a zoom past ZOOM_REBUILD_BAND left the pane on a texture more than a \
         factor of two off its own scale"
    );
}

/// **The settle.** A gesture that ends inside the band buys exactly one rasterize
/// when the map stops, at the zoom it stopped at — and then stops asking.
#[test]
fn a_zoom_that_stops_inside_the_band_settles_exactly_once() {
    const Z0: f64 = 7.0;
    let mut h = settled_alert_pane(Z0);

    let asked = zoom_gesture(&mut h, Z0, 0.05, 12);
    assert_eq!(asked, 0, "fixture: the gesture itself must be free");
    let stopped_at = Z0 + 0.6;

    h.frame_after(1.0 / 120.0);
    assert_eq!(
        rasterizes_requested(&h, &known::NWS_ALERTS),
        0,
        "one 120 Hz frame of stillness was called a settle; on a coalesced \
         touch stream that fires in the middle of the gesture"
    );

    h.frame_after(crate::overlay_cache::SETTLE_REPAINT_DELAY.as_secs_f64());
    assert_eq!(
        rasterizes_requested(&h, &known::NWS_ALERTS),
        1,
        "the gesture ended and nothing asked for the texture this zoom wants; \
         the overlay stays soft until something else invalidates it"
    );
    assert_eq!(
        requested_render_zoom(&h, &known::NWS_ALERTS),
        crate::overlay_cache::current_quantized_zoom(stopped_at),
        "the settle render was keyed at a zoom the map is not at"
    );

    settle_overlay_cache(&mut h, &known::NWS_ALERTS);
    for frame in 0..4 {
        h.frame_after(1.0 / 120.0);
        assert_eq!(
            rasterizes_requested(&h, &known::NWS_ALERTS),
            0,
            "frame {frame} after the settle landed asked for another raster; \
             the settle is a loop rather than a one-shot"
        );
    }
    assert!(
        !h.gui_mut().panes_mut()[0]
            .overlay_cache_mut(&known::NWS_ALERTS)
            .zoom_is_stale(stopped_at),
        "the settle landed and the overlay is still at another zoom — this is \
         the permanent-blur failure, and it is invisible without this assertion"
    );
}

/// **The misfire that hid the alert layer on a phone.**
#[test]
fn a_zoom_pause_shorter_than_the_settle_delay_is_not_a_settle() {
    const Z0: f64 = 7.0;
    let mut h = settled_alert_pane(Z0);

    set_pane_zoom(&mut h, Z0 + 0.2);
    h.frame_after(1.0 / 120.0);
    assert_eq!(
        rasterizes_requested(&h, &known::NWS_ALERTS),
        0,
        "fixture: a zoom inside the band must be free while it moves",
    );
    for frame in 0..4 {
        h.frame_after(1.0 / 120.0);
        assert_eq!(
            rasterizes_requested(&h, &known::NWS_ALERTS),
            0,
            "frame {frame} of a coalesced pause dispatched a raster \
             mid-gesture: the settle misfire, back again",
        );
    }
    set_pane_zoom(&mut h, Z0 + 0.4);
    h.frame_after(1.0 / 120.0);
    assert_eq!(
        rasterizes_requested(&h, &known::NWS_ALERTS),
        0,
        "the gesture resumed and the resume frame itself dispatched",
    );

    h.frame_after(crate::overlay_cache::SETTLE_REPAINT_DELAY.as_secs_f64());
    assert_eq!(
        rasterizes_requested(&h, &known::NWS_ALERTS),
        1,
        "control: a real stop no longer settles, so the zeros above prove \
         nothing",
    );
}

/// The settle is level-triggered, so a frame it cannot act on does not consume it.
#[test]
fn a_settle_frame_lost_to_an_in_flight_render_is_not_a_settle_lost() {
    const Z0: f64 = 7.0;
    let mut h = settled_alert_pane(Z0);

    let asked = zoom_gesture(&mut h, Z0, 0.05, 12);
    assert_eq!(asked, 0, "fixture: the gesture itself must be free");

    h.gui_mut().panes_mut()[0]
        .overlay_cache_mut(&known::NWS_ALERTS)
        .renders
        .record(crate::overlay_cache::RenderTicket::whole(
            0,
            squallar_geo::GeoBounds {
                min_lat: 34.0,
                max_lat: 36.0,
                min_lon: -98.0,
                max_lon: -96.0,
            },
        ));
    h.frame_after(crate::overlay_cache::SETTLE_REPAINT_DELAY.as_secs_f64() + 0.01);
    assert_eq!(
        rasterizes_requested(&h, &known::NWS_ALERTS),
        0,
        "fixture: an in-flight render must suppress the dispatch, or this test \
         is not about what it says"
    );

    h.gui_mut().panes_mut()[0]
        .overlay_cache_mut(&known::NWS_ALERTS)
        .renders
        .abandon_all();
    h.frame_after(1.0 / 120.0);
    assert_eq!(
        rasterizes_requested(&h, &known::NWS_ALERTS),
        1,
        "the settle was spent on a frame that could not dispatch it, and the \
         overlay is now permanently at the wrong zoom"
    );
}

/// And the frame the settle needs is *asked for*, rather than left to arrive.
#[test]
fn a_stale_zoom_asks_for_the_frame_its_settle_needs() {
    const Z0: f64 = 7.0;
    let mut h = settled_alert_pane(Z0);
    assert_eq!(
        h.repaint_delay(),
        std::time::Duration::MAX,
        "fixture: a pane whose overlay is at the map's own zoom must not be \
         holding the frame loop awake, or the assertion below says nothing. \
         `settled_alert_pane` drives to this condition and gave up; something \
         other than the overlay is asking for frames without stopping"
    );

    set_pane_zoom(&mut h, Z0 + 0.05);
    h.frame();
    assert!(
        h.repaint_delay() <= crate::overlay_cache::SETTLE_REPAINT_DELAY,
        "the overlay is at a zoom the map is not at and nothing asked for \
         another frame; on a reactive UI the settle render is then waiting for \
         an input event that may never come — got {:?}",
        h.repaint_delay(),
    );
}

/// A harness with one pane on KTLX showing a finished hybrid-classification image
/// that stood on `source`.
fn classification_showing(source: Option<squallar_radar::hca::MeltingLayerSource>) -> InputHarness {
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    h.load_scan("KTLX");
    h.gui_mut()
        .pane_mut(0)
        .unwrap()
        .set_overlay_enabled(known::RADAR, true);
    h.offer_product(0, &radar_fields::known::HYDROMETEOR_CLASSIFICATION, 0.5);
    h.select_product(0, &radar_fields::known::HYDROMETEOR_CLASSIFICATION);
    h.place_radar_image(
        0,
        &radar_fields::known::HYDROMETEOR_CLASSIFICATION,
        0.5,
        None,
        source,
        None,
    );
    h
}

/// The melting-layer notice a pane painted, if any.
fn melting_layer_notice_painted(h: &InputHarness) -> Option<String> {
    use squallar_radar::hca::MeltingLayerSource;
    let captions = [
        MeltingLayerSource::Rpg.caption(),
        MeltingLayerSource::RadarDetected.caption(),
        MeltingLayerSource::Sounding.caption(),
        MeltingLayerSource::FleetDefault.caption(),
    ];
    h.painted_text_strings()
        .iter()
        .find(|t| captions.contains(&t.as_str()))
        .cloned()
}

/// **A viewer can tell a classification from a guess — and is not nagged about the
/// ordinary case.**
#[test]
fn a_classification_says_when_its_melting_layer_was_never_measured() {
    use squallar_radar::hca::MeltingLayerSource;

    for measured in [MeltingLayerSource::Rpg, MeltingLayerSource::RadarDetected] {
        let h = classification_showing(Some(measured));
        assert!(
            measured.is_measured(),
            "precondition: {measured:?} is a measured layer",
        );
        assert_eq!(
            melting_layer_notice_painted(&h),
            None,
            "{measured:?} drew a caveat on the ordinary case; painted: {:?}",
            h.painted_text_strings(),
        );
    }

    let h = classification_showing(Some(MeltingLayerSource::FleetDefault));
    let notice = melting_layer_notice_painted(&h).unwrap_or_else(|| {
        panic!(
            "a classification on the fleet default said nothing; painted: {:?}",
            h.painted_text_strings()
        )
    });
    assert!(
        notice.contains(MeltingLayerSource::FleetDefault.caption()),
        "the notice does not carry the source's own caption: {notice:?}",
    );
    let pane_rect = h.pane_rects()[0];
    assert!(
        h.text_painted_in(pane_rect, MeltingLayerSource::FleetDefault.caption()),
        "the notice was painted outside the pane it describes; painted: {:?}",
        h.painted_text_strings(),
    );

    let sounding = classification_showing(Some(MeltingLayerSource::Sounding));
    assert!(
        melting_layer_notice_painted(&sounding)
            .is_some_and(|t| t.contains(MeltingLayerSource::Sounding.caption())),
        "a classification on a sounding's freezing level said nothing; painted: {:?}",
        sounding.painted_text_strings(),
    );

    assert_eq!(
        melting_layer_notice_painted(&classification_showing(None)),
        None
    );
}

/// **No two top-of-pane notices are ever on screen together — and there are three
/// of them now.**
#[test]
fn the_top_of_pane_notices_never_stack() {
    use squallar_radar::hca::MeltingLayerSource;
    use squallar_radar::srv::StormMotionSource;

    let mut h = classification_showing(Some(MeltingLayerSource::FleetDefault));
    assert!(
        melting_layer_notice_painted(&h).is_some() && !any_notice_painted(&h),
        "precondition: the melting-layer notice alone; painted: {:?}",
        h.painted_text_strings(),
    );
    assert_eq!(
        srm_legend_line_painted(&h),
        None,
        "a classification pane drew a storm-motion vector; painted: {:?}",
        h.painted_text_strings(),
    );

    h.offer_product(0, &radar_fields::known::REFLECTIVITY, 0.5);
    h.select_product(0, &radar_fields::known::REFLECTIVITY);
    assert!(
        any_notice_painted(&h),
        "precondition: the pane is showing a classification labelled \
             reflectivity; painted: {:?}",
        h.painted_text_strings(),
    );
    assert_eq!(
        melting_layer_notice_painted(&h),
        None,
        "both notices were painted over one another; painted: {:?}",
        h.painted_text_strings(),
    );

    let mut srv = storm_relative_showing(Some(srm_vector(StormMotionSource::BunkersRightMover)));
    assert!(
        srm_legend_line_painted(&srv).is_some() && !any_notice_painted(&srv),
        "precondition: the legend line and no plate; painted: {:?}",
        srv.painted_text_strings(),
    );
    assert_eq!(
        melting_layer_notice_painted(&srv),
        None,
        "an SRV pane drew a melting-layer notice; painted: {:?}",
        srv.painted_text_strings(),
    );

    srv.offer_product(0, &radar_fields::known::REFLECTIVITY, 0.5);
    srv.select_product(0, &radar_fields::known::REFLECTIVITY);
    assert!(
        any_notice_painted(&srv),
        "precondition: the pane is showing a storm-relative field labelled \
             reflectivity; painted: {:?}",
        srv.painted_text_strings(),
    );
    assert_eq!(
        srm_legend_line_painted(&srv),
        None,
        "the previous product's vector was drawn over reflectivity; painted: {:?}",
        srv.painted_text_strings(),
    );
}

/// A harness with one pane on KTLX showing a finished storm-relative velocity image
/// that was shifted by `source`.
fn storm_relative_showing(source: Option<squallar_radar::srv::SrvMotion>) -> InputHarness {
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    h.load_scan("KTLX");
    h.gui_mut()
        .pane_mut(0)
        .unwrap()
        .set_overlay_enabled(known::RADAR, true);
    h.offer_product(0, &radar_fields::known::STORM_RELATIVE_VELOCITY, 0.5);
    h.select_product(0, &radar_fields::known::STORM_RELATIVE_VELOCITY);
    h.place_radar_image(
        0,
        &radar_fields::known::STORM_RELATIVE_VELOCITY,
        0.5,
        None,
        None,
        source,
    );
    h
}

/// The storm-motion line a pane painted into its legend, if any.
fn srm_legend_line_painted(h: &InputHarness) -> Option<String> {
    h.painted_text_strings()
        .iter()
        .find(|t| t.starts_with("SRM "))
        .cloned()
}

/// A vector on `source`'s rung, with numbers distinct enough per rung that a test
/// which mixed two rungs up would also mix two readings up.
fn srm_vector(source: squallar_radar::srv::StormMotionSource) -> squallar_radar::srv::SrvMotion {
    use squallar_radar::srv::StormMotionSource as S;
    let (speed_kt, direction_deg) = match source {
        S::UserOverride => (45.0, 210.0),
        S::RpgScitAverage => (31.0, 246.0),
        S::MeanWind => (26.0, 209.0),
        S::BunkersRightMover => (38.0, 224.0),
    };
    squallar_radar::srv::SrvMotion {
        speed_kt,
        direction_deg,
        source,
    }
}

/// **A viewer reads the vector their picture was shifted by, not an apology.**
#[test]
fn a_storm_relative_field_shows_the_vector_it_was_shifted_by() {
    use squallar_radar::srv::StormMotionSource;

    for rung in [
        StormMotionSource::RpgScitAverage,
        StormMotionSource::UserOverride,
        StormMotionSource::MeanWind,
        StormMotionSource::BunkersRightMover,
    ] {
        let motion = srm_vector(rung);
        let mut h = storm_relative_showing(Some(motion));
        h.gui_mut().preferences.speed = squallar_units::SpeedUnit::Knots;
        h.warm_up();
        let line = srm_legend_line_painted(&h).unwrap_or_else(|| {
            panic!(
                "{rung:?} drew no vector; painted: {:?}",
                h.painted_text_strings()
            )
        });

        assert!(
            line.contains(&format!("{:.0} kt", motion.speed_kt)),
            "{rung:?}'s line names no speed: {line:?}",
        );
        assert!(
            line.contains(&format!("{:03.0}\u{b0}", motion.direction_deg)),
            "{rung:?}'s line names no direction: {line:?}",
        );
        assert!(
            line.contains(rung.tag()),
            "{rung:?}'s line does not say where the vector came from: {line:?}",
        );

        let pane_rect = h.pane_rects()[0];
        assert!(
            h.text_painted_in(pane_rect, &line),
            "{rung:?}'s line was painted outside the pane it describes; \
             painted: {:?}",
            h.painted_text_strings(),
        );
        assert!(
            line.len() <= 32,
            "{rung:?}'s legend line is paragraph-sized: {line:?}",
        );
    }

    assert_eq!(srm_legend_line_painted(&storm_relative_showing(None)), None);
}

/// **The vector relabels with the reader's speed unit, in the same frame.**
#[test]
fn the_storm_motion_vector_is_drawn_in_the_readers_speed_unit() {
    use squallar_radar::srv::{SrvMotion, StormMotionSource};
    use squallar_units::SpeedUnit;

    let motion = SrvMotion {
        speed_kt: 30.0,
        direction_deg: 240.0,
        source: StormMotionSource::RpgScitAverage,
    };
    let expected = [
        (SpeedUnit::Mph, "SRM 35 mph @ 240\u{b0}"),
        (SpeedUnit::MetersPerSec, "SRM 15 m/s @ 240\u{b0}"),
        (SpeedUnit::KilometersPerHour, "SRM 56 km/h @ 240\u{b0}"),
        (SpeedUnit::Knots, "SRM 30 kt @ 240\u{b0}"),
    ];
    let mut h = storm_relative_showing(Some(motion));
    for (unit, prefix) in expected {
        h.gui_mut().preferences.speed = unit;
        h.warm_up();
        let line = srm_legend_line_painted(&h).unwrap_or_else(|| panic!("{unit:?}: nothing drawn"));
        assert!(
            line.starts_with(prefix),
            "{unit:?}: the vector is not in the unit the reader asked for: {line:?}",
        );
        assert!(
            line.contains(StormMotionSource::RpgScitAverage.tag()),
            "{unit:?}: the source tag went missing with the unit change: {line:?}",
        );
    }
}

/// **The pane never apologises for the rung it is on.**
#[test]
fn no_storm_motion_rung_apologises_over_the_radar() {
    use squallar_radar::srv::StormMotionSource;

    for rung in [
        StormMotionSource::RpgScitAverage,
        StormMotionSource::UserOverride,
        StormMotionSource::MeanWind,
        StormMotionSource::BunkersRightMover,
    ] {
        let h = storm_relative_showing(Some(srm_vector(rung)));
        for painted in h.painted_text_strings() {
            let lower = painted.to_ascii_lowercase();
            for lament in [
                "no rpg storm motion",
                "cell average",
                "unpredictable",
                "too little shear",
                "can differ",
            ] {
                assert!(
                    !lower.contains(lament),
                    "{rung:?} painted {painted:?}, which laments rather than reports",
                );
            }
        }
    }
}

// ── WO-E8b: the chunk feed's switches, in the radar layer's own body ──────

/// Scroll `label`'s painted text into the inspector's clickable interior and
/// return its rect. The inspector body is a scroll area with its own clip:
/// a control drawn past its bottom edge is painted but not hittable, so
/// "somewhere on the screen" is not a place a user could click.
fn radar_control_rect(h: &mut InputHarness, label: &str) -> egui::Rect {
    let panel = h
        .inspector_rect()
        .expect("the radar layer body is open in the inspector");
    let interior = panel.shrink(24.0);
    let visible = |h: &InputHarness| {
        h.painted_text_rects()
            .iter()
            .any(|(rect, text)| text == label && interior.contains(rect.center()))
    };
    assert!(
        h.scroll_until(panel.center(), egui::vec2(0.0, -60.0), 200, visible),
        "{label:?} never scrolled into the inspector's clickable interior",
    );
    h.painted_text_rects()
        .into_iter()
        .find(|(_, text)| text == label)
        .expect("just asserted painted")
        .0
}

/// **The radar layer's Refresh button asks the shell for THIS pane's scan.**
///
/// The row moved out of `SETTINGS_ROWS` into the layer's own control tree at
/// WO-E8b, and it could not move as a `ControlEffect::Fetch`: that routes to
/// `push_user_overlay_fetch`, which is the generic overlay fetch and not the
/// site-and-timestamp fetch this layer's data arrives by. So the button is
/// recognised by the radar glue and answered here, and this is the pin that
/// says the answer is the right action for the right site.
#[test]
fn the_radar_layers_refresh_button_asks_the_shell_for_this_panes_scan() {
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    // A site no pane carries by default, so an action naming it can only
    // have come from the pane set here.
    assert_ne!(
        h.gui_mut().pane(0).expect("pane 0").site(),
        "KGRR",
        "precondition: a fresh pane must not already be on this site, or the \
         assertion below could pass off the default",
    );
    h.gui_mut()
        .pane_mut(0)
        .expect("pane 0")
        .set_site("KGRR".to_string());
    h.warm_up();

    h.open_layer_in_inspector(&known::RADAR);
    let button = radar_control_rect(&mut h, squallar_radar::source::REFRESH_LABEL);
    assert_eq!(
        h.gui_mut().pane(0).expect("pane 0").site(),
        "KGRR",
        "premise: the pane is still on the site set above",
    );
    // Read on the release frame: `last_actions` is one frame's worth, and a
    // warm-up would run past the frame the click was answered on.
    h.mouse_click(button.center());

    let asked: Vec<&str> = h
        .last_actions()
        .iter()
        .filter_map(|action| match action {
            crate::actions::GuiAction::FetchRadarScan(config) => Some(config.site.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(
        asked,
        vec!["KGRR"],
        "the Refresh button must ask for the active pane's site, exactly once",
    );
}

/// **One live-chunk switch still means every pane — from the inspector too.**
///
/// The switch is two facts: the layer's global, which `apply_control` writes,
/// and a copy in every pane's radar slot *config*, which no contract door can
/// reach. The fan-out lives in the radar glue (ruling (27), route (b)) and the
/// inspector is the awkward caller: the pane being edited is `mem::take`n out
/// of the pane vector while its body renders, so a fan-out over the vector
/// alone writes a placeholder and leaves the edited pane behind.
///
/// **Non-triviality floor**: both panes are asserted to start in agreement at
/// the *opposite* value, so neither assertion below can pass on a fixture that
/// already held the answer.
#[test]
fn the_inspectors_live_chunk_switch_reaches_every_pane_including_the_edited_one() {
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    h.set_pane_count(2);
    h.warm_up();
    for i in 0..2 {
        h.gui_mut()
            .pane_mut(i)
            .expect("both panes")
            .set_radar_live_chunks(true);
    }
    h.warm_up();
    for i in 0..2 {
        assert_eq!(
            h.gui_mut().pane(i).expect("both panes").radar_live_chunks(),
            Some(true),
            "precondition: pane {i} starts ON, or the assertion below could \
             pass without the switch having moved anything",
        );
    }

    h.open_layer_in_inspector(&known::RADAR);
    let toggle = radar_control_rect(&mut h, squallar_radar::source::LIVE_CHUNKS_LABEL);
    h.mouse_click(toggle.center());
    h.warm_up();

    for i in 0..2 {
        assert_eq!(
            h.gui_mut().pane(i).expect("both panes").radar_live_chunks(),
            Some(false),
            "pane {i} kept the old value — one switch no longer means every pane",
        );
    }
    assert!(
        !crate::radar_layer::live_chunks_enabled(h.gui_mut()),
        "the layer's own answer did not move with the panes",
    );
}

/// **Typing in the endpoint box reaches the layer that owns the endpoint.**
///
/// `ControlItem::TextField` is new plumbing at WO-E8b — a variant, a renderer
/// arm, a parity-walk kind and a shape row — and the half no other control
/// exercises is the edit coming back as a `ControlValue::String`. Without this
/// the box would draw, walk and persist while silently discarding every
/// keystroke.
#[test]
fn typing_in_the_notifier_box_reaches_the_radar_layer() {
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    h.open_layer_in_inspector(&known::RADAR);
    assert_eq!(
        crate::radar_layer::notifier_endpoint_raw(h.gui_mut()),
        squallar_radar::source::DEFAULT_NOTIFIER_ENDPOINT,
        "precondition: the box starts at the built-in default",
    );

    let label = radar_control_rect(&mut h, squallar_radar::source::NOTIFIER_ENDPOINT_LABEL);
    // The box itself sits directly under its label, in the same column.
    h.mouse_click(egui::pos2(label.center().x, label.max.y + 12.0));
    h.warm_up();
    // To the end first: the click lands wherever in the string it lands, and
    // this pin is about the keystroke arriving, not about where the caret was.
    h.key_press(egui::Key::End);
    h.type_text("!");
    h.warm_up();

    assert_eq!(
        crate::radar_layer::notifier_endpoint_raw(h.gui_mut()),
        format!("{}!", squallar_radar::source::DEFAULT_NOTIFIER_ENDPOINT),
        "the keystroke never reached the handler — the TextField's update \
         path is not wired",
    );
}

/// Applying a preset that names a field this build does not register leaves the
/// pane's own field **as it was**, and still applies everything else.
///
/// **This test exists because a tamper found nothing to fail.** WO-E9d land 2's
/// preserve rule was pinned only on the config load/save path
/// (`an_unknown_preset_field_is_preserved_and_costs_neither_pane_nor_file`),
/// which never calls `apply_preset` — so substituting a default at the *apply*
/// site passed every suite. Preservation on disk is worth little if applying
/// the preset silently rewrites the pane to Reflectivity, so the apply site
/// gets its own pin.
#[test]
fn applying_a_preset_that_names_an_unregistered_field_leaves_the_pane_alone() {
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    h.load_scan("KTLX");
    h.set_pane_count(1);
    // Resolved through the registry rather than by naming the product enum:
    // this test is about field ids, and reading one back through
    // `fields::product_for` is both the path under test and one fewer place the
    // enum has to be spelled.
    let start = squallar_source::product::FieldId::from_static("SpectrumWidth");
    assert!(
        squallar_radar::fields::spec_for(&start).is_some(),
        "the radar layer registers SpectrumWidth"
    );
    h.gui_mut()
        .pane_mut(0)
        .expect("pane 0")
        .set_selected_product(start.clone());
    h.warm_up();

    h.gui_mut().push_preset_for_test(crate::ui::PresetConfig {
        name: "From a newer build".into(),
        pane_count: 1,
        panes: vec![crate::ui::PresetPane {
            product: squallar_source::product::FieldId::from_static("FutureProduct"),
            elevation: 2.5,
        }],
        overlays: vec![known::RADAR].into(),
    });
    h.warm_up();

    h.open_catalog();
    let tile = h
        .catalog_tile(crate::ui::CatalogGroup::Presets, "From a newer build")
        .expect("the installed preset must be offered");
    h.mouse_click(tile.rect.center());
    h.warm_up();

    assert_eq!(
        h.gui_mut().pane(0).expect("pane 0").selected_product(),
        start,
        "a preset naming a field this build does not register must leave the \
         pane's field untouched — substituting a default here silently \
         rewrites a newer build's preset the next time this one saves",
    );
    // Non-vacuity: the preset really was applied, so the assertion above is
    // about preservation and not about the click having done nothing.
    assert!(
        (h.gui_mut().pane(0).expect("pane 0").selected_elevation() - 2.5).abs() < 0.01,
        "the rest of the preset must still apply — otherwise this test would \
         pass on a preset that was never applied at all",
    );
}

// ── WI-11: the two-colour time scrubber ──────────────────────────────────
//
// The rail's arithmetic is pinned in `ui_timeline/rail_tests.rs`. Everything
// below drives the real widget and reads what it really painted and really
// emitted, because this campaign has twice landed an item whose hand-armed
// tests all passed while the behaviour was absent.

/// Long enough for the chrome fade to settle, so a colour read off the paint
/// is the colour and not a stage of an animation.
const FADE_SETTLED_FRAMES: usize = 30;

/// The floor under the forecast fill's channel spread — `max(r,g,b) -
/// min(r,g,b)` — the pin that reds a slide back to grey-on-grey. Measured at
/// the 0.45 accent mix: 50 in light, 58 in dark.
const FUTURE_MIN_CHROMA: i32 = 24;

/// The floor under the widest per-channel gap between the two regions'
/// fills. The floor it replaces was 8, on the red channel alone — the
/// grey-on-grey palette's own margin, which the user vetoed as too subtle.
/// Measured at the 0.45 accent mix: 39 in light, 31 in dark.
const FILL_MIN_SEPARATION: i32 = 24;

/// **The palette floors on a two-colour rail, in whichever theme is up** —
/// shared by the archive rail and the loop rail, which paint through one
/// expression and must not drift apart. Three pins, each with a regression
/// it reds:
///
/// 1. the forecast fill is *colour*, not a second grey — its widest channel
///    clears its narrowest by [`FUTURE_MIN_CHROMA`]. The grey-on-grey
///    palette the user vetoed had no such pin;
/// 2. the two regions are mutually visible — some channel separates them by
///    [`FILL_MIN_SEPARATION`];
/// 3. the fill is derived from the live theme — every channel sits between
///    the past fill's and this theme's own `selection.bg_fill`, so a pair
///    hard-coded for the light theme reds the dark arm and the other way
///    round.
fn assert_forecast_fill_is_this_themes_accent_tint(
    theme: &str,
    past_fill: egui::Color32,
    future_fill: egui::Color32,
    accent: egui::Color32,
) {
    let rgb = |c: egui::Color32| [i32::from(c.r()), i32::from(c.g()), i32::from(c.b())];
    let (past, future, accent) = (rgb(past_fill), rgb(future_fill), rgb(accent));

    let chroma =
        future.iter().max().expect("3 channels") - future.iter().min().expect("3 channels");
    assert!(
        chroma >= FUTURE_MIN_CHROMA,
        "{theme}: the forecast fill {future:?} spreads its channels by \
         {chroma} of 255, which is grey, not the colour the user asked for"
    );

    let separation = (0..3)
        .map(|i| (past[i] - future[i]).abs())
        .max()
        .expect("3 channels");
    assert!(
        separation >= FILL_MIN_SEPARATION,
        "{theme}: the regions {past:?} and {future:?} sit {separation} of \
         255 apart at their widest channel, which is not a denotation a \
         reader can see"
    );

    for i in 0..3 {
        let (lo, hi) = (past[i].min(accent[i]), past[i].max(accent[i]));
        assert!(
            (lo..=hi).contains(&future[i]),
            "{theme}: channel {i} of the forecast fill {future:?} is outside \
             [{lo}, {hi}] - not a mix of this theme's trough {past:?} and \
             accent {accent:?}, so the fill is not coming from the live theme"
        );
    }
}

/// Put pane 0 on a forecast transport: the model layer on, radar off. Radar
/// outranks the model in the draw order, so a pane carrying both has a radar
/// transport and a past-only rail — which is itself the subject of
/// `a_radar_only_rail_is_the_one_bar_it_always_was`.
fn on_a_forecast_pane(h: &mut InputHarness) {
    h.load_scan("KTLX");
    h.set_overlay_on_pane(0, &known::MODEL_DATA, true);
    h.set_overlay_on_pane(0, &known::RADAR, false);
    h.frames_for(FADE_SETTLED_FRAMES, 0.1);
    assert_eq!(
        *h.gui_mut().pane(0).expect("pane 0").transport_layer(),
        known::MODEL_DATA,
        "precondition: the transport must address the forecast layer, or \
         nothing below is about a forecast rail at all"
    );
}

/// Every rect the last frame painted inside the scrubber that covers any
/// pixels at all, with its fill.
fn rail_rects(h: &InputHarness, scrub: egui::Rect) -> Vec<(egui::Rect, egui::Color32)> {
    h.painted_rects()
        .iter()
        .copied()
        .zip(h.painted_fills().iter().copied())
        .filter(|(r, _)| scrub.expand(1.0).contains_rect(*r) && r.height() > 0.0)
        .collect()
}

/// Where a fraction of the rail's travel sits, **read this frame**.
///
/// Row 1 is laid out from the timestamp's own galley, so committing a scrub
/// can change that text and move the rail under a position captured before
/// the gesture. Every position below is resolved immediately before it is
/// used, which is the difference between driving the widget and driving a
/// snapshot of where it once was.
fn rail_x(h: &InputHarness, frac: f32) -> egui::Pos2 {
    let scrub = h.timeline().scrubber;
    let shape = h.handle_shape();
    egui::pos2(
        scrub.left()
            + crate::ui::slider_end_inset(scrub, shape)
            + frac * crate::ui::slider_travel_px(scrub, shape),
        scrub.center().y,
    )
}

/// Press at `from`, drag to `to` and let go, one frame apart, re-resolving
/// each position against the rail as it is at that moment.
fn drag_rail(h: &mut InputHarness, from: f32, to: f32) {
    let start = rail_x(h, from);
    h.mouse_press(start);
    h.frame_after(0.05);
    let end = rail_x(h, to);
    h.mouse_move(end);
    h.frame_after(0.05);
    let end = rail_x(h, to);
    h.mouse_move(end);
    h.frame_after(0.05);
    h.mouse_release(end);
    h.frame_after(0.05);
}

/// Where the colour break sits on `scrub`, from the widget's own geometry.
fn break_x(h: &InputHarness, scrub: egui::Rect, split: f32) -> f32 {
    let shape = h.handle_shape();
    scrub.left()
        + crate::ui::slider_end_inset(scrub, shape)
        + split * crate::ui::slider_travel_px(scrub, shape)
}

/// **A pane with no forecast timeline paints the one bar it always painted.**
///
/// Not "close enough": one rail-band rect, at egui's own rail geometry, in
/// egui's own trough colour, with no second fill over it and no boundary
/// drawn across it. `NOW_SPLIT` is `1.0` on such a pane and the two-colour
/// path is not entered at all, which is what makes this a promise about the
/// shape set and not about a resemblance.
#[test]
fn a_radar_only_rail_is_the_one_bar_it_always_was() {
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    h.load_scan("KTLX");
    h.frames_for(FADE_SETTLED_FRAMES, 0.1);
    let scrub = h.timeline().scrubber;
    assert!(scrub.is_positive(), "precondition: the scrubber is drawn");

    let trough = h.inactive_bg_fill();
    let rail_height = h.slider_rail_height();
    let expected = egui::Rect::from_min_max(
        egui::pos2(scrub.left(), scrub.center().y - rail_height / 2.0),
        egui::pos2(scrub.right(), scrub.center().y + rail_height / 2.0),
    );

    let rail: Vec<_> = rail_rects(&h, scrub)
        .into_iter()
        .filter(|(r, _)| (r.height() - rail_height).abs() < 0.01)
        .collect();
    assert_eq!(
        rail.len(),
        1,
        "a pane with no forecast timeline painted {} rail-band rects, not \
         the one egui's own slider paints: {rail:?}",
        rail.len(),
    );
    let (rect, fill) = rail[0];
    assert!(
        (rect.left() - expected.left()).abs() < 0.01
            && (rect.right() - expected.right()).abs() < 0.01,
        "the rail spans {rect:?}, where egui's own spans {expected:?}"
    );
    assert_eq!(
        fill, trough,
        "the rail is not the trough colour any more, so a radar-only pane no \
         longer looks the way it did"
    );
    assert!(
        h.all_segments_in(scrub.expand(2.0)).is_empty(),
        "a boundary was drawn across a rail that has no boundary to draw"
    );
}

/// **One bar, two colours inside it, meeting at `now`** — in whichever theme
/// is up.
///
/// The two fills and the boundary are read off the live `Visuals` by one
/// expression, so this walks both: a pair of colours hard-coded for the light
/// theme reds the dark arm and the other way round. The break's position is
/// asserted against the widget's own travel, because the whole point of the
/// colour is to mark where the seconds per pixel changes.
#[test]
fn the_forecast_rail_paints_two_colours_meeting_at_now() {
    for dark in [false, true] {
        let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
        h.set_os_theme(dark);
        on_a_forecast_pane(&mut h);
        let theme = if dark { "dark" } else { "light" };

        let scrub = h.timeline().scrubber;
        let rail_height = h.slider_rail_height();
        let split = crate::ui::NOW_SPLIT;
        let boundary_x = break_x(&h, scrub, split);

        let bands: Vec<_> = rail_rects(&h, scrub)
            .into_iter()
            .filter(|(r, _)| (r.height() - rail_height).abs() < 0.01)
            .collect();
        assert_eq!(
            bands.len(),
            2,
            "{theme}: the rail is {} bands, not the two the user asked for: \
             {bands:?}",
            bands.len(),
        );
        let (past_rect, past_fill) = bands[0];
        let (future_rect, future_fill) = bands[1];

        assert!(
            (past_rect.left() - scrub.left()).abs() < 0.01
                && (past_rect.right() - boundary_x).abs() < 0.05
                && (future_rect.left() - boundary_x).abs() < 0.05
                && (future_rect.right() - scrub.right()).abs() < 0.01,
            "{theme}: the two regions are {past_rect:?} and {future_rect:?}, \
             which do not meet at {boundary_x:.2} and cover the rail exactly \
             once"
        );

        let segments = h.all_segments_in(scrub.expand(2.0));
        assert_eq!(
            segments.len(),
            1,
            "{theme}: {} boundary strokes, not one: {segments:?}",
            segments.len(),
        );
        let (a, b, stroke) = segments[0];
        assert!(
            (a.x - boundary_x).abs() < 0.05 && (b.x - boundary_x).abs() < 0.05,
            "{theme}: the boundary is at {a:?}-{b:?}, not at {boundary_x:.2}"
        );

        // Mutually distinguishable, and derived from this theme's own
        // Visuals: the past is the trough colour it has always been, the
        // future is that colour carried toward this theme's accent, and the
        // boundary is this theme's active stroke.
        assert_eq!(
            past_fill,
            h.inactive_bg_fill(),
            "{theme}: the past region is no longer today's trough"
        );
        assert_ne!(
            past_fill, future_fill,
            "{theme}: the two regions are the same colour, so there is no \
             denotation between past and future at all"
        );
        assert_ne!(
            stroke.color, past_fill,
            "{theme}: the boundary is invisible"
        );
        assert_ne!(
            stroke.color, future_fill,
            "{theme}: the boundary is invisible against the forecast region"
        );
        assert_forecast_fill_is_this_themes_accent_tint(
            theme,
            past_fill,
            future_fill,
            h.selection_bg_fill(),
        );
    }
}

/// **The two fills are read from the theme, not written into the code.**
///
/// The light and dark arms above would both pass on a hard-coded pair chosen
/// to satisfy whichever ran first; what rules that out is that the pair
/// itself moves with the theme: each arm's forecast fill is a mix of that
/// theme's own trough and that theme's own accent, and the two themes'
/// accents are different colours, so one pair cannot satisfy both arms.
#[test]
fn the_rails_two_colours_come_from_the_live_theme() {
    let mut arms = Vec::new();
    for dark in [false, true] {
        let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
        h.set_os_theme(dark);
        on_a_forecast_pane(&mut h);
        let scrub = h.timeline().scrubber;
        let rail_height = h.slider_rail_height();
        let bands: Vec<_> = rail_rects(&h, scrub)
            .into_iter()
            .filter(|(r, _)| (r.height() - rail_height).abs() < 0.01)
            .map(|(_, fill)| fill)
            .collect();
        assert_eq!(bands.len(), 2);
        arms.push((bands[0], bands[1], h.selection_bg_fill()));
    }
    let (light_past, light_future, light_accent) = arms[0];
    let (dark_past, dark_future, dark_accent) = arms[1];
    assert_ne!(
        light_past, dark_past,
        "the past region is the same colour in both themes, so it is not \
         coming from the theme"
    );
    assert_ne!(light_future, dark_future);
    assert_ne!(
        light_accent, dark_accent,
        "precondition: the two themes' accents differ, or a tint checked \
         against either accent could not tell a hard-coded pair from a \
         derived one"
    );
    assert_forecast_fill_is_this_themes_accent_tint(
        "light",
        light_past,
        light_future,
        light_accent,
    );
    assert_forecast_fill_is_this_themes_accent_tint("dark", dark_past, dark_future, dark_accent);
}

/// **The break is a change of colour, not a change of widget** — one press
/// crosses it in either direction and commits once.
///
/// Two rails would clamp a drag at the region it started in: dragging right
/// out of the past could never name a forecast instant, and dragging left out
/// of the forecast could only ever name `now`. Both directions are walked,
/// because either half alone passes on one of the two rails.
#[test]
fn a_drag_crosses_the_colour_break_in_one_hit_target() {
    use crate::actions::GuiAction;

    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    on_a_forecast_pane(&mut h);
    let split = crate::ui::NOW_SPLIT;
    let past_centre = split / 2.0;
    let future_centre = split + (1.0 - split) / 2.0;

    // ── Past to forecast ─────────────────────────────────────────────────
    let before = chrono::Utc::now().naive_utc();
    h.mouse_press(rail_x(&h, past_centre));
    h.frame_after(0.05);
    h.mouse_move(rail_x(&h, future_centre));
    h.frame_after(0.05);
    assert!(
        !h.last_actions().iter().any(|a| matches!(
            a,
            GuiAction::NavigateTime { .. } | GuiAction::JumpToLive { .. }
        )),
        "the scrub committed mid-drag, which is a fetch per drag frame"
    );
    let end = rail_x(&h, future_centre);
    h.mouse_move(end);
    h.frame_after(0.05);
    h.mouse_release(end);
    h.frame_after(0.05);

    assert!(
        !h.last_actions()
            .iter()
            .any(|a| matches!(a, GuiAction::JumpToLive { .. })),
        "a release in the middle of the forecast region answered live"
    );
    let mode = h.gui_mut().pane(0).expect("pane 0").time.mode;
    let crate::pane::TimeMode::AsOf(landed) = mode else {
        panic!("the release left the pane's clock at {mode:?}, not on the instant it named");
    };
    let ahead = (landed - before).num_seconds();
    // The forecast half is 30% of the travel over the run's whole horizon, so
    // its centre is about half the horizon out. Bounded on both sides, so a
    // release that never left the past and one that ran off the end both red.
    assert!(
        (6 * 3600..=27 * 3600).contains(&ahead),
        "the drag out of the past region landed {} h from now, which is not \
         the middle of an 18-to-48 h horizon. A rail split into two widgets \
         would clamp here at the past region's own right end",
        ahead / 3600,
    );

    // ── Forecast back to past ────────────────────────────────────────────
    h.clear_actions();
    drag_rail(&mut h, future_centre, past_centre);

    let navigations: Vec<i64> = h
        .last_actions()
        .iter()
        .filter_map(|a| match a {
            GuiAction::NavigateTime {
                pane_idx: 0,
                step_secs,
            } => Some(*step_secs),
            _ => None,
        })
        .collect();
    assert_eq!(
        navigations.len(),
        1,
        "the drag out of the forecast region committed {} times, not once",
        navigations.len(),
    );
    let mode = h.gui_mut().pane(0).expect("pane 0").time.mode;
    let crate::pane::TimeMode::AsOf(landed) = mode else {
        panic!("the return leg left the pane's clock at {mode:?}");
    };
    let behind = (chrono::Utc::now().naive_utc() - landed).num_seconds();
    assert!(
        behind > 0,
        "the drag out of the forecast region landed {behind} s behind now, so \
         it never crossed back"
    );
}

/// **The live zone sits at `now`, which is no longer the right end** — and it
/// is checked at the two widths the rail actually has.
///
/// The rule it replaces was a fraction of the rail (`0.99`), which is 4.8 pt
/// of live zone on the wide rail and 0.8 pt on the narrow one. This is a
/// deliberate change of behaviour at **both** widths, and on a pane with a
/// forecast region it moves the zone off the end entirely: the far right is a
/// forecast instant now, and answering "live" there would be answering a
/// question the user did not ask.
#[test]
fn the_live_zone_sits_at_now_and_not_at_the_far_end() {
    use crate::actions::GuiAction;

    for screen in [egui::vec2(1400.0, 900.0), egui::vec2(480.0, 800.0)] {
        let mut h = InputHarness::with_screen(screen);
        on_a_forecast_pane(&mut h);
        if !h.timeline().scrubber.is_positive() {
            continue;
        }
        let shape = h.handle_shape();
        let travel = crate::ui::slider_travel_px(h.timeline().scrubber, shape);
        let split = crate::ui::NOW_SPLIT;

        // The far end names the end of the horizon, not live.
        h.clear_actions();
        drag_rail(&mut h, 0.5, 1.0);
        assert!(
            !h.last_actions()
                .iter()
                .any(|a| matches!(a, GuiAction::JumpToLive { .. })),
            "travel {travel:.1} pt: the far end of a rail with a forecast \
             region answered live"
        );
        // Non-vacuity: that release really did commit, so the negative above
        // is about where it landed and not about nothing having happened.
        let mode = h.gui_mut().pane(0).expect("pane 0").time.mode;
        let crate::pane::TimeMode::AsOf(landed) = mode else {
            panic!("travel {travel:.1} pt: the far end committed nothing at all");
        };
        assert!(
            landed > chrono::Utc::now().naive_utc(),
            "travel {travel:.1} pt: the far end named {landed}, which is not \
             a forecast instant"
        );

        // The boundary does.
        h.clear_actions();
        drag_rail(&mut h, 0.2, split);
        assert!(
            h.last_actions()
                .iter()
                .any(|a| matches!(a, GuiAction::JumpToLive { pane_idx: 0 })),
            "travel {travel:.1} pt: releasing on the now boundary did not \
             restore live"
        );
    }
}

/// **A pane carrying no radar can be scrubbed.**
///
/// The clock write used to sit behind radar's `scan_info`, so a pane with the
/// radar layer off moved nothing at all when its rail was dragged — every
/// clock-aware layer on it stayed at the instant it started on. The write is
/// layer-agnostic now, and radar's fetch is the half that stayed conditional.
#[test]
fn a_pane_with_no_radar_scan_still_moves_when_it_is_scrubbed() {
    use crate::actions::GuiAction;

    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    h.set_overlay_on_pane(0, &known::MODEL_DATA, true);
    h.set_overlay_on_pane(0, &known::RADAR, false);
    h.frames_for(FADE_SETTLED_FRAMES, 0.1);
    assert!(
        h.gui_mut().pane(0).expect("pane 0").scan_info.is_none(),
        "precondition: this pane has never held a radar scan"
    );
    let before = h.gui_mut().pane(0).expect("pane 0").time.mode;

    drag_rail(&mut h, 0.6, 0.2);

    let after = h.gui_mut().pane(0).expect("pane 0").time.mode;
    assert_ne!(
        before, after,
        "a pane with no radar scan did not move when its rail was dragged: \
         the clock write is still behind radar's scan"
    );
    assert!(
        matches!(after, crate::pane::TimeMode::AsOf(_)),
        "the release left the clock at {after:?} rather than on an instant"
    );
    assert!(
        !h.last_actions()
            .iter()
            .any(|a| matches!(a, GuiAction::NavigateTime { .. })),
        "a pane with no scan asked for a radar volume to step from"
    );
}

// -- WI-11b: the same two colours on the loop rail ------------------------
//
// While a loop is running the bar on screen is the frame-index slider, not
// the archive rail, so the user's sentence is only honoured in that state if
// this bar breaks too. Its split rule is a different one: the frames are
// evenly spaced, so there is no change of scale to signal and the break goes
// where the frames straddle `now`. Every position below is read off what the
// widget really painted.

/// Frames `step` seconds apart, the `past` oldest of them at or before the
/// wall clock, offset by half a step so that no frame sits within half a step
/// of `now`. The count must not depend on how long the harness takes to reach
/// the render.
fn loop_frames_straddling_now(total: usize, past: usize, step: i64) -> Vec<crate::pane::LoopFrame> {
    let now = chrono::Utc::now().naive_utc();
    (0..total)
        .map(|i| crate::pane::LoopFrame {
            timestamp: now + chrono::Duration::seconds((i as i64 - past as i64) * step + step / 2),
            image: None,
            render_in_flight: false,
            render_failed: false,
        })
        .collect()
}

/// Put pane 0 on a running loop of `total` frames, the `past` oldest of them
/// history. `past == total` is the radar case and the one that must not move.
fn on_a_running_loop(h: &mut InputHarness, total: usize, past: usize, step: i64) {
    let pane = h.gui_mut().pane_mut(0).expect("pane 0");
    *pane.time_state_mut(&known::RADAR) = crate::radar_layer::begin_loop(
        3600,
        squallar_radar::sites::get_radar_site("KTLX").expect("KTLX"),
        squallar_radar::types::RenderView::PlanView,
    );
    pane.time_state_mut(&known::RADAR).phase = crate::pane::LoopPhase::Playing;
    pane.time_state_mut(&known::RADAR).frames = loop_frames_straddling_now(total, past, step);
    pane.park_on_frame(&known::RADAR, 0);
    h.frames_for(FADE_SETTLED_FRAMES, 0.1);
    assert!(
        h.widget_id_probes()
            .iter()
            .any(|(name, _)| *name == "timeline_scrubber_loop"),
        "precondition: a running loop must put the frame-index rail on the \
         row, or nothing below is about the loop rail at all"
    );
}

/// The rail bands the last frame painted inside `scrub`: the shapes at the
/// slider's own rail geometry, with their fills, in paint order, which is
/// past then future. A region of no width is not a band, so a rail with only
/// one region reports one.
fn loop_rail_bands(h: &InputHarness, scrub: egui::Rect) -> Vec<(egui::Rect, egui::Color32)> {
    let rail_height = h.slider_rail_height();
    rail_rects(h, scrub)
        .into_iter()
        .filter(|(r, _)| (r.height() - rail_height).abs() < 0.01 && r.width() > 0.0)
        .collect()
}

/// **A radar loop's rail is the one bar it always was.**
///
/// Every frame of a radar loop is history, so there is no break to draw and
/// the two-colour path is never entered: one rail-band rect, at egui's own
/// geometry, in egui's own trough colour, with nothing over it and no
/// boundary across it. This is the common case - it is what a radar user sees
/// every time they press the loop button - and it is the one WI-11b must not
/// disturb.
#[test]
fn a_radar_loop_rail_is_the_one_bar_it_always_was() {
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    h.load_scan("KTLX");
    on_a_running_loop(&mut h, 9, 9, 300);

    let scrub = h.timeline().scrubber;
    assert!(scrub.is_positive(), "precondition: the loop rail is drawn");
    let rail_height = h.slider_rail_height();
    let expected = egui::Rect::from_min_max(
        egui::pos2(scrub.left(), scrub.center().y - rail_height / 2.0),
        egui::pos2(scrub.right(), scrub.center().y + rail_height / 2.0),
    );

    let bands = loop_rail_bands(&h, scrub);
    assert_eq!(
        bands.len(),
        1,
        "a loop with no forecast frame in it painted {} rail-band rects, not \
         the one egui's own slider paints: {bands:?}",
        bands.len(),
    );
    let (rect, fill) = bands[0];
    assert!(
        (rect.left() - expected.left()).abs() < 0.01
            && (rect.right() - expected.right()).abs() < 0.01,
        "the loop rail spans {rect:?}, where egui's own spans {expected:?}"
    );
    assert_eq!(
        fill,
        h.inactive_bg_fill(),
        "the loop rail is not the trough colour any more, so a radar loop no \
         longer looks the way it did"
    );
    assert!(
        h.all_segments_in(scrub.expand(2.0)).is_empty(),
        "a boundary was drawn across a loop rail that has no boundary to draw"
    );
}

/// **The loop rail carries the same two colours, broken where the frames
/// straddle `now`** - in both themes, and the break moves with the frames.
///
/// Four things at once, because hand-arming each of them separately is how
/// this campaign has previously shipped an item whose tests all passed over
/// absent behaviour:
///
/// 1. the break lands between the two frames that straddle the wall clock,
///    at a position written here from those frames' own handle positions;
/// 2. one more frame falling behind the clock moves it right by exactly one
///    frame's width, and nothing else moves;
/// 3. a loop whose every frame is forecast has no past region at all;
/// 4. the two fills are mutually visible and come from the live `Visuals`,
///    so a pair hard-coded for one theme reds the other arm.
#[test]
fn the_loop_rail_breaks_where_the_frames_straddle_now() {
    const TOTAL: usize = 9;
    const PAST: usize = 5;
    const STEP: i64 = 300;
    let spacing = 1.0 / (TOTAL - 1) as f32;
    // The two frames that straddle now, as fractions of the slider's travel,
    // and the point midway between them. Written from the frame indices, not
    // from `loop_rail_split`.
    let straddle_break = 0.5 * ((PAST - 1) as f32 * spacing + PAST as f32 * spacing);

    for dark in [false, true] {
        let theme = if dark { "dark" } else { "light" };
        let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
        h.set_os_theme(dark);
        h.load_scan("KTLX");
        on_a_running_loop(&mut h, TOTAL, PAST, STEP);

        // -- 1. the break is between the straddling frames ---------------
        let scrub = h.timeline().scrubber;
        let boundary_x = break_x(&h, scrub, straddle_break);
        let bands = loop_rail_bands(&h, scrub);
        assert_eq!(
            bands.len(),
            2,
            "{theme}: the loop rail is {} bands, not the two the user asked \
             for: {bands:?}",
            bands.len(),
        );
        let (past_rect, past_fill) = bands[0];
        let (future_rect, future_fill) = bands[1];
        assert!(
            (past_rect.left() - scrub.left()).abs() < 0.01
                && (past_rect.right() - boundary_x).abs() < 0.05
                && (future_rect.left() - boundary_x).abs() < 0.05
                && (future_rect.right() - scrub.right()).abs() < 0.01,
            "{theme}: the loop rail's regions are {past_rect:?} and \
             {future_rect:?}. They must meet at {boundary_x:.2}, midway \
             between frame {} and frame {PAST} of {TOTAL}. A break pinned at \
             a fixed 0.70 would sit at {:.2}",
            PAST - 1,
            break_x(&h, scrub, crate::ui::NOW_SPLIT),
        );
        let segments = h.all_segments_in(scrub.expand(2.0));
        assert_eq!(
            segments.len(),
            1,
            "{theme}: {} boundary strokes across the loop rail, not one: \
             {segments:?}",
            segments.len(),
        );
        let (a, b, stroke) = segments[0];
        assert!(
            (a.x - boundary_x).abs() < 0.05 && (b.x - boundary_x).abs() < 0.05,
            "{theme}: the loop rail's boundary is at {a:?}-{b:?}, not at \
             {boundary_x:.2}"
        );

        // -- 4. the colours are visible and this theme's own --------------
        assert_eq!(
            past_fill,
            h.inactive_bg_fill(),
            "{theme}: the loop rail's past region is no longer today's trough"
        );
        assert_ne!(
            past_fill, future_fill,
            "{theme}: the loop rail's two regions are the same colour, so \
             there is no denotation between observed and forecast frames at \
             all"
        );
        assert_ne!(
            stroke.color, past_fill,
            "{theme}: the boundary is invisible"
        );
        assert_ne!(
            stroke.color, future_fill,
            "{theme}: the boundary is invisible against the forecast region"
        );
        assert_forecast_fill_is_this_themes_accent_tint(
            theme,
            past_fill,
            future_fill,
            h.selection_bg_fill(),
        );

        // -- 2. the break tracks the frames ------------------------------
        // Every frame one step earlier is what the wall clock passing one
        // more frame looks like from the rail.
        {
            let pane = h.gui_mut().pane_mut(0).expect("pane 0");
            for frame in &mut pane.time_state_mut(&known::RADAR).frames {
                frame.timestamp -= chrono::Duration::seconds(STEP);
            }
        }
        h.frames_for(2, 0.1);
        let scrub = h.timeline().scrubber;
        let rolled = loop_rail_bands(&h, scrub);
        assert_eq!(rolled.len(), 2, "{theme}: the rolled loop is {rolled:?}");
        let moved = rolled[0].0.right() - boundary_x;
        let one_frame = spacing * crate::ui::slider_travel_px(scrub, h.handle_shape());
        assert!(
            (moved - one_frame).abs() < 0.1,
            "{theme}: the clock passing one more frame moved the break \
             {moved:.2} pt, from {boundary_x:.2} to {:.2}. One frame's width \
             is {one_frame:.2} pt",
            rolled[0].0.right(),
        );

        // -- 3. a loop that is all forecast has no past region ------------
        on_a_running_loop(&mut h, TOTAL, 0, STEP);
        let scrub = h.timeline().scrubber;
        let all_future = loop_rail_bands(&h, scrub);
        assert_eq!(
            all_future.len(),
            1,
            "{theme}: a loop whose every frame is forecast painted {} \
             regions, not the one: {all_future:?}",
            all_future.len(),
        );
        let (rect, fill) = all_future[0];
        assert_eq!(
            fill, future_fill,
            "{theme}: a loop whose every frame is forecast painted its rail \
             in the past colour"
        );
        assert!(
            (rect.left() - scrub.left()).abs() < 0.01
                && (rect.right() - scrub.right()).abs() < 0.01,
            "{theme}: the forecast region covers {rect:?}, not the whole rail \
             {scrub:?} - the break is not at the far left"
        );
    }
}

// -- WI-9 / WI-10: the transport tells the truth about a forecast pane ----

/// **The chip names the forecast pane's own valid time, and says `forecast`**
/// (WI-9) — read off the timestamp the transport really painted, on a
/// two-pane layout built to expose the old fallthrough: pane 0 loops a
/// forecast parked on f06, pane 1 sits on live radar with a fresh stamp of
/// its own. `data_time_on_screen` used to read radar's slot by definition, so
/// pane 0 reported no time and the chip borrowed pane 1's clock — captioned
/// `live`, over a picture of five and a half hours from now.
#[test]
fn the_chip_names_a_forecast_loops_own_valid_time_and_says_forecast() {
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    h.set_pane_count(2);
    h.set_overlay_on_pane(0, &known::MODEL_DATA, true);
    h.set_overlay_on_pane(0, &known::RADAR, false);
    h.frames_for(FADE_SETTLED_FRAMES, 0.1);
    assert_eq!(
        *h.gui_mut().pane(0).expect("pane 0").transport_layer(),
        known::MODEL_DATA,
        "precondition: pane 0's transport must address the forecast layer"
    );

    let now = chrono::Utc::now().naive_utc();
    // f00 half an hour old, hourly frames: f06 is five and a half hours out.
    let valid = |f_hour: i64| now + chrono::Duration::hours(f_hour) - chrono::Duration::minutes(30);
    let neighbour_stamp = now - chrono::Duration::minutes(3);
    {
        let gui = h.gui_mut();
        gui.preferences.timezone = squallar_units::TimezonePreference::Utc;
        let pane1 = gui.pane_mut(1).expect("pane 1");
        pane1.data_time = Some(neighbour_stamp);
        assert!(pane1.viewing_live, "precondition: pane 1 follows live");
        let pane0 = gui.pane_mut(0).expect("pane 0");
        let ts = pane0.transport_state_mut();
        ts.phase = crate::pane::LoopPhase::Paused;
        ts.frames = (0..=18)
            .map(|f_hour| crate::pane::LoopFrame {
                timestamp: valid(f_hour),
                image: None,
                render_in_flight: false,
                render_failed: false,
            })
            .collect();
        assert!(
            pane0.park_on_transport_frame(6),
            "precondition: f06 exists to park on"
        );
    }
    h.frames_for(2, 0.1);

    let stamp = h.timeline().timestamp.1.clone();
    // Expected from the stamp this test wrote, formatted independently of the
    // chip's own path (the preference was pinned to UTC above).
    let expected = format!("{} UTC", valid(6).format("%H:%M:%S"));
    assert!(
        stamp.contains(&expected),
        "the transport stamp reads {stamp:?}, not pane 0's own f06 valid \
         time {expected:?}"
    );
    assert!(
        stamp.contains("forecast"),
        "a stamp five and a half hours from now must be captioned \
         `forecast`, got {stamp:?}"
    );
    let neighbour = format!("{} UTC", neighbour_stamp.format("%H:%M:%S"));
    assert!(
        !stamp.contains(&neighbour),
        "the chip borrowed pane 1's clock ({neighbour:?}) over the forecast \
         pane's own: {stamp:?}"
    );
}

/// **A pane parked on a forecast frame keeps its forward step, rests its
/// handle on the instant it depicts, and keeps its site in the chunk feed**
/// (WI-10).
///
/// `viewing_live` means the *selection* follows live data, and it stays true
/// here — which is exactly why the two widget reads must ask
/// `depicts_future` instead. The `live_sites` assertion is the control: a
/// "fix" that cleared `viewing_live` on a future-depicting pane would green
/// the two widget reads and drop the pane's site from the chunk feed, and
/// this test is built to red on that.
#[test]
fn a_pane_parked_on_a_forecast_frame_keeps_forward_step_and_its_chunk_feed() {
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    on_a_forecast_pane(&mut h);

    let now = chrono::Utc::now().naive_utc();
    {
        let pane = h.gui_mut().pane_mut(0).expect("pane 0");
        assert!(pane.viewing_live, "precondition: the pane follows live");
        // Park on f12 by the clock: the posture is untouched, the instant
        // depicted is twelve hours out.
        pane.set_time_mode(crate::pane::TimeMode::AsOf(
            now + chrono::Duration::hours(12),
        ));
        assert!(
            pane.viewing_live,
            "precondition: parking the clock must not clear the live posture"
        );
    }
    h.frames_for(2, 0.1);

    assert!(
        h.timeline().fwd.1,
        "forward-step is disabled on a pane parked twelve hours short of its \
         horizon — `viewing_live` misread as `depicts now`"
    );

    let frac = h
        .timeline()
        .scrub_frac
        .expect("precondition: the archive rail is up (no loop is running)");
    assert!(
        frac > crate::ui::NOW_SPLIT + 0.01,
        "the handle rests at {frac:.3}, at or left of the now boundary \
         ({:.2}) — hours left of the instant the pane depicts",
        crate::ui::NOW_SPLIT,
    );
    assert!(
        frac < 0.99,
        "the handle rests at {frac:.3}, the far right edge — f12 is not the \
         end of the horizon"
    );

    assert!(
        h.gui_mut().live_sites().iter().any(|s| s == "KTLX"),
        "the pane's site left the chunk feed: the fix cleared `viewing_live` \
         instead of asking the right question"
    );
}

// -- WI-12: loop sync respects the transport layer ------------------------

/// **A seek on a forecast transport reaches only the panes on that
/// transport** (WI-12) — driven through the loop rail itself, read off the
/// actions the widget really emitted. Pane 1 is radar, time-linked, same
/// group: without the transport filter it takes the same
/// [`GuiAction::SeekLoopFrame`] index, which on a radar timeline is whichever
/// scan happens to sit at that offset.
#[test]
fn a_seek_on_a_forecast_transport_does_not_reach_a_linked_radar_pane() {
    use crate::actions::GuiAction;

    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    h.set_pane_count(2);
    h.load_scan("KTLX");
    h.set_overlay_on_pane(0, &known::MODEL_DATA, true);
    h.set_overlay_on_pane(0, &known::RADAR, false);
    h.frames_for(FADE_SETTLED_FRAMES, 0.1);
    {
        let gui = h.gui_mut();
        assert_eq!(
            *gui.pane(0).expect("pane 0").transport_layer(),
            known::MODEL_DATA,
            "precondition: pane 0's transport is the forecast layer"
        );
        assert_eq!(
            *gui.pane(1).expect("pane 1").transport_layer(),
            known::RADAR,
            "precondition: pane 1's transport is radar"
        );
        assert!(
            gui.panes_time_linked(0, 1),
            "precondition: the two panes share time"
        );
        let now = chrono::Utc::now().naive_utc();
        let pane0 = gui.pane_mut(0).expect("pane 0");
        let ts = pane0.transport_state_mut();
        ts.phase = crate::pane::LoopPhase::Paused;
        ts.frames = (0..=18)
            .map(|f_hour| crate::pane::LoopFrame {
                timestamp: now + chrono::Duration::hours(f_hour) - chrono::Duration::minutes(30),
                image: None,
                render_in_flight: false,
                render_failed: false,
            })
            .collect();
        pane0.park_on_transport_frame(0);
    }
    h.frames_for(2, 0.1);
    assert!(
        h.widget_id_probes()
            .iter()
            .any(|(name, _)| *name == "timeline_scrubber_loop"),
        "precondition: the frame-index rail is up on pane 0's loop"
    );

    let scrub = h.timeline().scrubber;
    let target = egui::pos2(scrub.left() + 0.8 * scrub.width(), scrub.center().y);
    h.mouse_press(target);
    h.frame();
    h.mouse_release(target);
    h.frame();

    let seeks: Vec<usize> = h
        .last_actions()
        .iter()
        .filter_map(|a| match a {
            GuiAction::SeekLoopFrame { pane_idx, .. } => Some(*pane_idx),
            _ => None,
        })
        .collect();
    assert!(
        seeks.contains(&0),
        "non-vacuity: the press on the rail seeked nothing at all"
    );
    assert!(
        !seeks.contains(&1),
        "the seek fanned out to the linked radar pane, which would park it \
         on whichever scan sits at a forecast index: {seeks:?}"
    );
}

/// 66. **The basemap credit is drawn exactly once per panel, whatever the pane
///     count, and the words really land on screen.**
///
/// Two obligations put it there — ODbL for the OpenStreetMap data and
/// OpenMapTiles' CC-BY, which asks for visible credit in the corner of the map
/// — and both are satisfied by one credit, not by one per pane. The failure
/// this pins is a credit that slipped inside `render_panes`' pane loop: four
/// panes, four copies, stacked in one corner and illegible.
///
/// The count alone would pass against a zero-sized area, so the rect has to
/// have area and the text has to be found by the painted-text scan.
///
/// **And a drawn credit is not a readable one.** The corner it wants is
/// already spoken for three ways — the colour scale's bar and tick labels, the
/// floating status bar, and the timeline — and the first cut of this placement
/// landed on all of them at once: measured at 1920x1080, the credit sat wholly
/// inside the status bar's rect, took the scale's bottom tick label across its
/// own last word, and clipped 3px of the bar. So the corner assertion is
/// joined by two collision assertions, and they are what the landscape arm of
/// the placement in `draw_basemap_attribution` is for.
///
/// **A portrait pane is placed the other way up, and this pins that too.**
/// There the colour scale is a horizontal bar across the pane's bottom, and
/// giving way to it upwards is what puts the credit *over* the bar, out in the
/// map. The user asked for it under the bar, so the second leg below asserts
/// the relationship by name: the credit's top edge is at or below the bar's
/// bottom edge. The bar is found by its painted geometry — a wide, 20pt-tall
/// quad — rather than by asking the placement code where it put it, which
/// would only ask the belief under test to confirm itself.
///
/// It is joined by a floor: the credit must stay off the phone shell's bottom
/// bar. That pair is what makes "under the bar" a placement rather than a
/// slide off the bottom of the screen — the strip between the two is 16pt, the
/// bar's own pane-edge margin, and the notice has to land inside it.
///
/// **What this test deliberately does not assert on the portrait leg is that
/// the credit is unoccluded**, because at 402x874 it is not: the expanded
/// transport spans `[0,772]-[402,825]` and already covers the whole
/// colour-scale strip — the bar at `[16,789]-[386,809]` and its `dBZ` title
/// with it. Anything under that bar is under the transport. Asserting
/// visibility here would pin a property the requested placement cannot have
/// while the scale itself is invisible, and the fix for both is one level down
/// in `color_scale_floor`, not in where the credit hangs.
#[test]
fn the_basemap_credit_is_drawn_once_per_panel_not_once_per_pane() {
    /// Everything true of the credit whichever way up the pane is.
    fn shared(h: &InputHarness, label: &str) -> egui::Rect {
        let panel = h.map_panel_rect();
        let rects = h.attribution_rects().to_vec();

        assert_eq!(
            rects.len(),
            1,
            "{label} drew {} credits; one per panel is the contract",
            rects.len(),
        );

        let rect = rects[0];
        assert!(
            rect.width() > 0.0 && rect.height() > 0.0,
            "non-vacuity: {label} reported a credit occupying no area \
             ({rect:?}) — the count above would pass against nothing drawn",
        );
        assert!(
            panel.contains_rect(rect),
            "{label}: the credit at {rect:?} left the map panel {panel:?}",
        );
        assert!(
            rect.center().x > panel.center().x && rect.center().y > panel.center().y,
            "{label}: the credit landed at {rect:?}, not in the panel's \
             bottom-right corner of {panel:?}",
        );
        assert!(
            h.text_painted_in(panel, "OpenStreetMap"),
            "{label}: the credit reported a rect but no painted text naming \
             OpenStreetMap — a rect is not a notice",
        );

        // Nothing else's words inside the credit's box. The colour scale's
        // bottom tick label is the one that used to be there, but the scan is
        // deliberately every text run rather than that label by name.
        let trespass: Vec<_> = h
            .painted_text_rects()
            .into_iter()
            .filter(|(other, text)| other.intersects(rect) && !text.contains("OpenStreetMap"))
            .collect();
        assert!(
            trespass.is_empty(),
            "{label}: the credit at {rect:?} has other painted text inside \
             it: {trespass:?}",
        );

        rect
    }

    // --- Landscape: the scale is a vertical bar on the right, and the credit
    //     lifts over the bottom chrome rather than printing on it.
    for count in [1usize, 2, 4] {
        let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
        h.set_pane_count(count);
        h.frame();

        let label = format!("landscape, {count} pane(s)");
        let rect = shared(&h, &label);

        // Not on the floating status bar, which spans nearly the panel's
        // whole width along the bottom edge the credit wants.
        let bar = h.status_bar().rect;
        assert!(
            bar.is_finite(),
            "non-vacuity: {label} drew no status bar, so the collision \
             assertion below could not have failed",
        );
        assert!(
            !rect.intersects(bar),
            "{label}: the credit at {rect:?} is drawn on the status bar at \
             {bar:?}",
        );
    }

    // --- Portrait: the scale is a horizontal bar along the bottom, and the
    //     credit goes *under* it.
    {
        let mut h = InputHarness::with_screen(egui::vec2(402.0, 874.0));
        h.set_pane_count(1);
        h.frame();

        let label = "portrait, 1 pane";
        let rect = shared(&h, label);

        let panel = h.map_panel_rect();
        let bar = h
            .painted_images_in(panel)
            .into_iter()
            .map(|image| image.rect)
            .find(|r| (r.height() - 20.0).abs() < 0.5 && r.width() > 100.0)
            .expect(
                "non-vacuity: no horizontal colour-scale bar was painted, so \
                 there was nothing for the credit to be under",
            );
        assert!(
            bar.width() > bar.height(),
            "non-vacuity: the quad found at {bar:?} is not a horizontal bar, \
             so this leg is measuring the wrong thing",
        );

        assert!(
            rect.top() >= bar.bottom(),
            "the credit at {rect:?} is not under the colour bar at {bar:?} — \
             its top edge {} is above the bar's bottom edge {}",
            rect.top(),
            bar.bottom(),
        );

        // And it stays out of the phone shell's bottom bar: "under the bar"
        // has to mean the strip below it, not off the bottom of the map.
        let shell = h.bottom_bar().rect;
        assert!(
            shell.is_finite(),
            "non-vacuity: no phone bottom bar was drawn at 402x874, so the \
             floor below could not have failed",
        );
        assert!(
            rect.bottom() <= shell.top(),
            "the credit at {rect:?} hangs into the phone shell's bottom bar \
             at {shell:?}",
        );
    }
}

/// 67. **The colour scale is not drawn underneath the floating chrome.**
///
/// The bar, every tick label read against it and the unit title are one
/// picture: a legend whose bottom labels are behind the status bar is a legend
/// that cannot be read, and the failure is silent — the scale reports a rect,
/// the painter paints it, and the pixels land under another surface.
///
/// Both targets had it, measured before the fix:
///
/// * **402x874** — the expanded transport spans `[0,772]-[402,825]` and the
///   bar sat at `[16,789]-[386,809]`, wholly inside it, with its ticks and its
///   `dBZ` title. The portrait colour scale was not on screen at all.
/// * **1400x900** — the vertical bar ran to `[1364,72]-[1384,884]` and its
///   bottom labels (`"0"` at `[1354.8,877.5]-[1361,890.5]`) sat inside the
///   status bar at `[8,860]-[1394,892]`.
///
/// `Gui::color_scale_floor` is what fixes both, and this is its gate. It is a
/// separate test from the basemap credit's (66) on purpose: this property is
/// worth holding whether or not anything is placed under the bar.
///
/// **Touching is not overlapping here, deliberately.** The floor *is* the
/// chrome's top edge, so a scale that fills the space exactly meets it — at
/// 402x874 the `dBZ` title's bottom edge lands on `772.0`, the transport's
/// top, to the pixel. That is the fix working, not a violation, so the test
/// asks for positive-area overlap rather than `Rect::intersects`.
#[test]
fn the_colour_scale_is_not_drawn_under_the_floating_chrome() {
    /// Overlap with area. `Rect::intersects` counts a shared edge, which is
    /// the exact case this fix produces.
    fn overlaps(a: egui::Rect, b: egui::Rect) -> bool {
        let hit = a.intersect(b);
        hit.width() > 0.0 && hit.height() > 0.0
    }

    for (label, w, ht) in [
        ("landscape", 1400.0f32, 900.0f32),
        ("portrait", 402.0f32, 874.0f32),
    ] {
        for collapsed in [false, true] {
            let mut h = InputHarness::with_screen(egui::vec2(w, ht));
            h.set_pane_count(1);
            h.frame();
            if collapsed {
                let transport = h.timeline();
                assert!(
                    !transport.collapsed,
                    "{label}: the transport was already collapsed, so the \
                     click below is not what put it there",
                );
                h.mouse_click(transport.collapse.center());
                h.warm_up();
            }
            h.frame();

            let case = format!(
                "{label}, transport {}",
                if collapsed { "collapsed" } else { "expanded" }
            );
            let panel = h.map_panel_rect();

            // The bar by its painted geometry — a SCALE_BAR_WIDTH-thick quad,
            // long on the other axis — rather than by asking the placement
            // code where it put it.
            let bar = h
                .painted_images_in(panel)
                .into_iter()
                .map(|image| image.rect)
                .find(|r| {
                    (r.height() - 20.0).abs() < 0.5 && r.width() > 100.0
                        || (r.width() - 20.0).abs() < 0.5 && r.height() > 100.0
                })
                .unwrap_or_else(|| {
                    panic!(
                        "non-vacuity: {case} painted no colour-scale bar, so \
                         there was nothing for this test to find covered",
                    )
                });

            // Its tick labels and unit title: the text painted in the gutter
            // around the bar. Numeric runs only, plus the unit — the `...`
            // the transport draws is not a tick.
            let zone = bar.expand(60.0);
            let legend: Vec<(egui::Rect, String)> = h
                .painted_text_rects()
                .into_iter()
                .filter(|(r, text)| {
                    zone.contains(r.center())
                        && (text == "dBZ"
                            || (text.chars().any(|c| c.is_ascii_digit())
                                && text.chars().all(|c| c.is_ascii_digit() || c == '.')))
                })
                .collect();
            assert!(
                legend.len() >= 5,
                "non-vacuity: {case} found only {} legend labels around the \
                 bar at {bar:?}; the scale draws a tick per threshold, so the \
                 collision scan below would have had almost nothing to check: \
                 {legend:?}",
                legend.len(),
            );

            // Every floating surface that actually drew this frame.
            let transport = h.timeline();
            let chrome: Vec<(&str, egui::Rect)> = [
                ("status bar", h.status_bar().rect),
                ("transport", transport.rect),
                ("timeline chip", transport.chip),
                ("phone bottom bar", h.bottom_bar().rect),
            ]
            .into_iter()
            .filter(|(_, r)| r.is_finite())
            .collect();
            assert!(
                !chrome.is_empty(),
                "non-vacuity: {case} drew no bottom chrome at all, so nothing \
                 below could have failed",
            );

            for (name, rect) in &chrome {
                assert!(
                    !overlaps(bar, *rect),
                    "{case}: the colour bar at {bar:?} is drawn under the \
                     {name} at {rect:?}",
                );
                let buried: Vec<_> = legend.iter().filter(|(r, _)| overlaps(*r, *rect)).collect();
                assert!(
                    buried.is_empty(),
                    "{case}: the {name} at {rect:?} is drawn over the colour \
                     scale's own labels: {buried:?}",
                );
            }
        }
    }
}

/// 66b. **The basemap credit clears the bottom chrome at every width class,
///      every pane count and both transport forms.**
///
/// Test 66 above pins the credit at two geometries — 1400x900 and 402x874 —
/// and both were green while the notice was invisible across the whole
/// 600-1000 medium band. This is the sweep that found that, kept as the gate.
///
/// **The defect it pins is a lift test that could not see the collision.** The
/// expanded transport is a fixed ~880pt-wide *centred* area, so how much of
/// the bottom-right corner it leaves free is not monotonic in the panel's
/// width. `draw_basemap_attribution` asked whether a bar's span covered the
/// credit's right **edge**; on a panel wide enough to leave that edge clear but
/// too narrow to clear the whole notice, the transport's right edge lands
/// *inside* the credit's box and the edge test reports no collision. Measured
/// before the fix: at 1200x874 the credit spanned `[994,1137]` against a
/// transport ending at `1040`, 46pt of overlap; at 800x874 with two panes it
/// missed by 4pt. 33 of the 198 cases below failed, every one of them with the
/// transport expanded.
///
/// So the widths are chosen, not round: `WidthClass::from_width` splits at 600
/// and 1000, and each boundary is walked from both sides, because a shell that
/// changes at a threshold is exactly where a placement stops being tested by
/// the geometries either side of it. 1200 is here because it failed and 1400 —
/// test 66's own width — did not, which is the non-monotonicity in one pair.
///
/// **The short height is deliberate.** 560pt leaves a four-pane grid's corner
/// pane barely taller than the chrome under it, which is where a credit gets
/// clamped off the top edge instead of merely covered. That the clamp in
/// `draw_basemap_attribution` never fires into a collision is asserted here by
/// the same on-screen test as everything else, rather than by naming it.
///
/// Three failures are possible and they are not the same bug, so the
/// assertions separate them: *not drawn at all* (the count), *drawn off
/// screen* (the containment), and *drawn but covered* (the overlap scan). The
/// defect this found was the third.
#[test]
fn the_basemap_credit_clears_the_bottom_chrome_at_every_width() {
    /// Overlap with area. `Rect::intersects` counts a shared edge, and a
    /// credit resting exactly on a bar's top edge is the placement working.
    fn overlaps(a: egui::Rect, b: egui::Rect) -> bool {
        let hit = a.intersect(b);
        hit.width() > 0.0 && hit.height() > 0.0
    }

    // Compact | the phone 66 pins | both sides of 600 | the medium band |
    // both sides of 1000 | the width that failed | 66's own | desktop.
    let widths = [
        360.0f32, 402.0, 599.0, 600.0, 700.0, 900.0, 999.0, 1000.0, 1200.0, 1400.0, 1920.0,
    ];
    // Short enough to squeeze a 4-pane grid, the phone's own, and desktop.
    let heights = [560.0f32, 874.0, 1080.0];

    let mut widths_seen = 0;
    let mut credit_widths: Vec<f32> = Vec::new();

    for w in widths {
        widths_seen += 1;
        for ht in heights {
            for panes in [1usize, 2, 4] {
                for collapsed in [false, true] {
                    let mut h = InputHarness::with_screen(egui::vec2(w, ht));
                    h.set_pane_count(panes);
                    h.frame();
                    if collapsed {
                        let transport = h.timeline();
                        assert!(
                            !transport.collapsed,
                            "{w}x{ht}: the transport was already collapsed, so \
                             the click below is not what put it there",
                        );
                        h.mouse_click(transport.collapse.center());
                        h.warm_up();
                    }
                    h.frame();

                    let case = format!(
                        "{w}x{ht}, {panes} pane(s), transport {}",
                        if collapsed { "collapsed" } else { "expanded" }
                    );

                    // --- Failure 1: not drawn at all.
                    let rects = h.attribution_rects().to_vec();
                    assert_eq!(
                        rects.len(),
                        1,
                        "{case}: drew {} credits; one per panel is the contract",
                        rects.len(),
                    );
                    let rect = rects[0];
                    assert!(
                        rect.width() > 0.0 && rect.height() > 0.0,
                        "non-vacuity: {case} reported a credit occupying no \
                         area ({rect:?}) — every test below would pass against \
                         nothing drawn",
                    );
                    let panel = h.map_panel_rect();
                    assert!(
                        h.text_painted_in(panel, "OpenStreetMap"),
                        "{case}: a credit rect at {rect:?} but no painted text \
                         naming OpenStreetMap — a rect is not a notice",
                    );
                    credit_widths.push(rect.width());

                    // --- Failure 2: drawn off screen. The window, not the
                    //     panel: a notice clamped above the panel's top edge is
                    //     still lost, and the panel test alone would miss a
                    //     credit pushed under the top bar.
                    let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(w, ht));
                    assert!(
                        screen.contains_rect(rect),
                        "{case}: the credit at {rect:?} is not wholly on the \
                         {w}x{ht} screen",
                    );
                    assert!(
                        panel.contains_rect(rect),
                        "{case}: the credit at {rect:?} left the map panel \
                         {panel:?}",
                    );

                    // --- Failure 3: drawn, on screen, and covered.
                    let transport = h.timeline();
                    let chrome: Vec<(&str, egui::Rect)> = [
                        ("status bar", h.status_bar().rect),
                        ("transport", transport.rect),
                        ("timeline chip", transport.chip),
                        ("phone bottom bar", h.bottom_bar().rect),
                    ]
                    .into_iter()
                    .filter(|(_, r)| r.is_finite())
                    .collect();
                    assert!(
                        !chrome.is_empty(),
                        "non-vacuity: {case} drew no bottom chrome at all, so \
                         the overlap scan below could not have failed",
                    );
                    for (name, bar) in &chrome {
                        assert!(
                            !overlaps(rect, *bar),
                            "{case}: the credit at {rect:?} is covered by the \
                             {name} at {bar:?}",
                        );
                    }

                    // Nothing else's words inside the credit's box either —
                    // the colour scale's tick labels reach into this corner.
                    let trespass: Vec<_> = h
                        .painted_text_rects()
                        .into_iter()
                        .filter(|(other, text)| {
                            overlaps(*other, rect) && !text.contains("OpenStreetMap")
                        })
                        .collect();
                    assert!(
                        trespass.is_empty(),
                        "{case}: the credit at {rect:?} has other painted text \
                         inside it: {trespass:?}",
                    );
                }
            }
        }
    }

    assert_eq!(
        widths_seen, 11,
        "non-vacuity: the sweep walked {widths_seen} widths, not the 11 the \
         doc above claims — a trimmed list would still pass every assertion",
    );

    // The notice is one fixed string at one fixed size, so it lays out to one
    // width everywhere. A width that varies means it wrapped or was elided,
    // and it is also what `attribution_span` predicts in order to place the
    // notice: if that prediction drifts from the drawn box, the lift test goes
    // back to missing collisions by a few points, silently.
    let (min, max) = credit_widths
        .iter()
        .fold((f32::MAX, f32::MIN), |(lo, hi), &w| (lo.min(w), hi.max(w)));
    assert!(
        (max - min).abs() < 0.5,
        "the credit laid out between {min} and {max} wide across the sweep; \
         one fixed string at one fixed size must not vary",
    );
}
