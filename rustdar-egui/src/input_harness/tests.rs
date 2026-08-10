use super::*;

/// The two durations that bracket the idle backstop, deliberately **not**
/// derived from `POINTER_IDLE_TIMEOUT_S`.
///
/// A probe that sizes its own loop off the constant under test cannot
/// notice that constant changing — it just moves with it, and both a 32s
/// and a 600s backstop pass. These are absolute claims about the behaviour
/// instead: a still hold must survive the first, and a pointer that has
/// gone silent must not survive the second.
///
/// They also have to be *close enough together to say something*. At 90s
/// the upper claim admitted any backstop up to a minute and a half, so
/// stretching the shipped minute to 75s left the suite green while the map
/// stayed hostage to a dead integration a quarter longer. The band below is
/// 45–70s around a shipped 60: comfortably clear of the longest hold a user
/// plausibly performs while reading a value, and short enough that "the map
/// comes back after about a minute" is a claim rather than a gesture.
const HOLD_MUST_SURVIVE_S: f64 = 45.0;
const SILENCE_MUST_EXPIRE_S: f64 = 70.0;

/// Long enough for a deferred single tap to be confirmed
/// (`DOUBLE_TAP_TIMEOUT_S` is 0.4s).
const AFTER_DOUBLE_TAP_TIMEOUT: f64 = 0.5;

/// How long a "the gesture really ended" assertion must keep watching.
///
/// It has to clear `LONG_PRESS_DURATION_S` (0.8s) by a wide margin — that
/// is how long a detector that re-arms itself off a stale pointer takes to
/// come back — and it also has to be long enough that a pointer which is
/// *supposed* to stay dead is watched over a realistic span rather than a
/// couple of seconds. Half a minute of frame-by-frame checking is cheap
/// here (the whole suite runs headless in well under a second).
const WATCH_PAST_LONG_PRESS: f64 = 30.0;

/// 1. A single mouse click reports a click position at the clicked point
///    and never suppresses panning.
#[test]
fn mouse_single_click_reports_click_pos() {
    let mut h = InputHarness::new();
    let pos = h.map_center();

    let outcome = h.mouse_click(pos);

    assert_eq!(outcome.mouse.overlay_click_pos, Some(pos));
    assert!(!outcome.mouse.suppress_pan);
    assert_eq!(outcome.mouse.long_press_pos, None);

    // The click is a single-frame event: the next frame is clean again.
    let next = h.frame_after(FRAME_DT);
    assert_eq!(next.mouse.overlay_click_pos, None);
}

/// 2. A mouse double click reports a click on each release, and the touch
///    pipeline defers instead of firing two overlay taps.
#[test]
fn mouse_double_click_reports_each_click() {
    let mut h = InputHarness::new();
    let pos = h.map_center();

    let first = h.mouse_click(pos);
    assert_eq!(first.mouse.overlay_click_pos, Some(pos));

    // Second click inside egui's double-click window.
    let second = h.mouse_click(pos);
    assert_eq!(second.mouse.overlay_click_pos, Some(pos));
    assert!(!second.mouse.suppress_pan);

    // The touch pipeline treats the same input as a double-tap: no overlay
    // tap is emitted while the second press is pending.
    assert_eq!(first.touch.overlay_click_pos, None);
    assert_eq!(second.touch.overlay_click_pos, None);
}

/// 3. Pressing and holding for ~1s without moving is a long press: it
///    reports the held position and suppresses map panning, and it is not
///    a click.
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

    // Hold for ~1s (LONG_PRESS_DURATION_S is 0.8s) without moving.
    let held = h.frames_for(10, 0.1);
    assert_eq!(held.touch.long_press_pos, Some(pos));
    assert!(held.touch.suppress_pan, "long press owns the pointer");
    assert_eq!(
        held.mouse.overlay_click_pos, None,
        "a press with no release is not a click"
    );

    // Releasing ends the long press; the slow release is not a tap either.
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
///
///     Test 3 only pins the late direction — that a 1s hold does raise the
///     tooltip — so shortening `LONG_PRESS_DURATION_S` to a hundredth of a
///     second passes it. Early is the direction that hurts: the tooltip
///     appearing under an ordinary tap, or the moment a pan begins, taking
///     the drag away from the map.
#[test]
fn a_long_press_does_not_fire_before_its_threshold() {
    let mut h = InputHarness::new();
    let pos = h.map_center();

    h.touch_start(pos);
    // 0.7s of held, motionless finger: still short of LONG_PRESS_DURATION_S.
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

    // Past it, it does fire — so the assertion above is about *when*, not
    // about the detector having been switched off.
    let held = h.frames_for(4, 0.05);
    assert_eq!(held.touch.long_press_pos, Some(pos));
}

/// 3c. A finger that keeps moving never becomes a long press, however long
///     it goes on.
///
///     The cancel-on-movement branch is what separates a hold from a pan,
///     and nothing pinned it: widening `LONG_PRESS_MAX_MOVE_PX` to 2000, or
///     dropping the branch entirely, raises a tooltip mid-drag and
///     suppresses the pan under it. The finger zigzags rather than running
///     off in one direction so the distance per frame, not the distance from
///     the start, is what the detector has to be reading.
#[test]
fn a_moving_finger_never_becomes_a_long_press() {
    let mut h = InputHarness::new();
    let pos = h.map_center();

    h.touch_start(pos);
    h.frame_after(FRAME_DT);
    // 1.2s — comfortably past LONG_PRESS_DURATION_S — of 30px steps.
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

/// 4. A touch tap is deferred until the double-tap window closes, then
///    reported once at the tapped position.
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

    // Consumed exactly once.
    let next = h.frame_after(FRAME_DT);
    assert_eq!(next.touch.overlay_click_pos, None);
}

/// 5. Tap, then press again and drag down: the map zooms, panning is
///    suppressed for the whole drag, and no overlay tap is emitted.
#[test]
fn touch_double_tap_drag_zooms_and_suppresses_pan() {
    let mut h = InputHarness::new();
    let start = h.map_center();
    let zoom_before = h.zoom();

    // First tap.
    h.touch_tap(start);

    // Second press within the double-tap window enters the zoom drag.
    h.touch_start(start);
    let dragging = h.frame_after(0.05);
    assert!(
        dragging.touch.suppress_pan,
        "zoom drag must block map panning"
    );
    assert_eq!(dragging.touch.overlay_click_pos, None);
    assert_eq!(dragging.touch.long_press_pos, None);

    // Drag downward: ZOOM_DRAG_SENSITIVITY is 150px per zoom level.
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

    // Lifting ends the gesture and does not emit an overlay tap.
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
///
///     `DOUBLE_TAP_DISTANCE_PX` is the only thing keeping two unrelated taps
///     in quick succession — which is what walking a finger across the map
///     looks like — out of a zoom drag. Test 5 taps twice at the same point,
///     so it holds neither this bound nor the `&&` joining it to the timeout.
#[test]
fn a_second_tap_far_away_does_not_enter_a_zoom_drag() {
    let mut h = InputHarness::new();
    let near = h.map_center();
    let far = near + egui::vec2(200.0, 0.0);
    let zoom_before = h.zoom();

    h.touch_tap(near);
    // Well inside DOUBLE_TAP_TIMEOUT_S, well outside DOUBLE_TAP_DISTANCE_PX.
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
///
///     The other direction of the same bound, and the one that decides
///     whether the gesture is usable at all: a finger never lands twice on
///     the same pixel, so a `DOUBLE_TAP_DISTANCE_PX` tightened towards zero
///     leaves double-tap-zoom looking simply broken.
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
///
///     `handle_press` enters a zoom drag on `dt < DOUBLE_TAP_TIMEOUT_S &&
///     dist < DOUBLE_TAP_DISTANCE_PX`. 5b fails only the distance conjunct
///     and 5c satisfies both, so nothing failed *only* the timeout: widened
///     to 0.5s the whole suite stayed green while a tap and an unrelated
///     second tap half a second later silently became a zoom drag —
///     the map jumping under a finger that was pointing at something.
///
///     Both taps land on the same pixel here, so the distance conjunct is
///     satisfied throughout and only the timeout can decide. The pair
///     straddles the bound, which is what pins it from both sides: a
///     timeout shortened towards zero fails the first assertion, because
///     two taps a third of a second apart are one ordinary double tap.
#[test]
fn the_double_tap_window_closes_between_two_unrelated_taps() {
    /// Tap, sit still for `gap` seconds, then press again and hold.
    /// Reports whether that second press entered a zoom drag.
    fn a_second_press_after(gap: f64) -> bool {
        let mut h = InputHarness::new();
        let pos = h.map_center();

        h.touch_tap(pos);
        // One frame `gap` after the release, so the promotion inside
        // `DoubleTapDragDetector::update` gets a frame to run on, exactly
        // as an idle app does.
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
///
///     The long press is polled only when no zoom drag is running, and
///     nothing pinned that exclusion. A user who double-taps and then holds
///     before dragging is doing something completely ordinary, and without
///     the gate they get the radar-value tooltip on top of the zoom.
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
///
///     Test 5 pins only the sign — that dragging down zooms in — so a
///     tenfold sensitivity passes it while slamming the map from one zoom
///     stop to the other on the smallest drag. This pins the rate.
#[test]
fn the_zoom_drag_moves_one_level_per_sensitivity_of_travel() {
    let mut h = InputHarness::new();
    let pos = h.map_center();
    let zoom_before = h.zoom();

    h.touch_tap(pos);
    h.touch_start(pos);
    h.frame_after(0.05);
    // Exactly ZOOM_DRAG_SENSITIVITY pixels of travel: one zoom level.
    h.touch_move(pos + egui::vec2(0.0, 150.0));
    let dragged = h.frame_after(FRAME_DT);

    assert!(
        (dragged.zoom - (zoom_before + 1.0)).abs() < 0.05,
        "150px of drag is one zoom level: {zoom_before} -> {}",
        dragged.zoom
    );
}

/// 6. **PROBE B — regression test for the stranded zoom drag.** The OS cancels the
///    touch mid-drag: only `PointerGone` arrives, no release, and egui keeps
///    reporting `pointer.down == true` forever. The gesture must still end,
///    or the map stays un-pannable until the app restarts.
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

    // System edge gesture / incoming call / browser `touchcancel`.
    h.touch_cancel(start + egui::vec2(0.0, 60.0));
    let cancelled = h.frame_after(FRAME_DT);
    assert!(
        !cancelled.touch.suppress_pan,
        "cancelled touch must not leave the map in zoom-drag"
    );
    assert_eq!(cancelled.touch.long_press_pos, None);

    // …and it must stay released, frame after frame, even though egui still
    // reports the primary button as down. This has to run well past
    // LONG_PRESS_DURATION_S (0.8s): the phantom finger is still "down", so a
    // detector that re-arms on `down` takes exactly that long to claim it
    // back — as a long press pinned at Pos2::ZERO, because `PointerGone`
    // cleared egui's pointer position.
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

/// 6b. **PROBE A** — the same cancellation, but during a long press: the
///     tooltip position must not stick, and must not come back either.
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

    // Watch past LONG_PRESS_DURATION_S: clearing the state once is not
    // enough if the detector is allowed to re-arm off egui's latched `down`.
    h.assert_every_frame_for(WATCH_PAST_LONG_PRESS, 0.1, |frame, outcome| {
        assert_eq!(
            outcome.touch.long_press_pos, None,
            "frame {frame}: the long press must not re-arm itself"
        );
        assert!(!outcome.touch.suppress_pan, "frame {frame}");
    });
}

/// 6c. A *secondary* finger being cancelled must not kill the primary
///     finger's live gesture. `Event::Touch { phase: Cancel }` carries a
///     `TouchId` that cannot be matched against the emulated pointer, so the
///     tracker keys on `PointerGone` alone.
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

    // The drag still zooms.
    let zoom_before = after.zoom;
    h.touch_move(start + egui::vec2(0.0, 120.0));
    let dragged = h.frame_after(FRAME_DT);
    assert!(dragged.touch.suppress_pan);
    assert!(dragged.zoom > zoom_before, "the drag must still be live");
}

/// 6d. **PROBE C** — a zoom drag that keeps moving must never be cut off,
///     however long it runs — a user framing a view can easily hold one for
///     many seconds. (The pointer backstop is keyed on inactivity, not on
///     gesture age, so this runs well past `POINTER_IDLE_TIMEOUT_S`.)
#[test]
fn long_active_zoom_drag_is_never_cut_off() {
    let mut h = InputHarness::new();
    let start = h.map_center();

    h.touch_tap(start);
    h.touch_start(start);
    assert!(h.frame_after(0.05).touch.suppress_pan);

    // 40 seconds of continuous dragging, well past any plausible backstop.
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

    // Still responding to movement at the end.
    let zoom_before = h.zoom();
    h.touch_move(start + egui::vec2(0.0, offset + 100.0));
    let dragged = h.frame_after(FRAME_DT);
    assert_ne!(dragged.zoom, zoom_before, "the drag must still zoom");
}

/// 6e. If pointer input simply stops arriving mid-gesture (the integration
///     went away without ever sending a release or a cancel), the stale
///     "finger is down" belief expires — and does not get handed to the long
///     press on the way out.
#[test]
fn silent_pointer_expires_and_stays_expired() {
    let mut h = InputHarness::new();
    let start = h.map_center();

    h.touch_tap(start);
    h.touch_start(start);
    assert!(h.frame_after(0.05).touch.suppress_pan);
    h.touch_move(start + egui::vec2(0.0, 40.0));
    assert!(h.frame_after(FRAME_DT).touch.suppress_pan);

    // No events at all from here on, for longer than the backstop allows.
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

/// 6f. **PROBE D** — the desktop excursion. The button is held, the cursor
///     leaves the window and comes back still held, and everything that
///     arrives in between is a `PointerMoved`: no press, ever.
///
///     `egui-winit` maps `CursorLeft` to a bare `PointerGone` and forgets
///     the pointer position, which also makes it drop a mouse release that
///     happens outside the window — so egui's `primary_down()` stays
///     latched right through the excursion. A latch that only a *press*
///     could clear therefore stranded the pointer here exactly as a
///     cancelled touch used to strand it: dead until the user clicked.
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

    // The cursor leaves. Nothing says whether the button survived the trip,
    // so the pointer must be distrusted for as long as it stays silent.
    h.cursor_left();
    let gone = h.frame_after(FRAME_DT);
    assert_eq!(
        gone.touch.long_press_pos, None,
        "the held position must not stick"
    );
    assert!(!gone.touch.suppress_pan);

    // It comes back, still dragging: five move events, no press among them.
    let mut back = inside;
    for step in 1..=5 {
        back = inside + egui::vec2(12.0 * step as f32, 7.0 * step as f32);
        h.mouse_move(back);
        h.frame_after(FRAME_DT);
    }

    // Coming back with nothing but motion is not enough to reopen: the
    // release that may have happened out of sight was discarded by the
    // integration (`lib.rs:796`), so this stream is indistinguishable from
    // a bare hover. A tooltip here would suppress panning until the user
    // clicked — see `ui_input::tests::an_excursion_is_terminal_until_a_press`.
    let hovering = h.frames_for(20, 0.1);
    assert_eq!(
        hovering.touch.long_press_pos, None,
        "a returning pointer must not open a hold nobody pressed for"
    );
    assert!(!hovering.touch.suppress_pan);

    // And it is not wedged either: one real press restores everything.
    h.mouse_press(back);
    let pressed = h.frames_for(10, 0.1);
    assert_eq!(pressed.touch.long_press_pos, Some(back));
    assert!(pressed.touch.suppress_pan);
}

/// 6f-R1. **PROBE R1** — a cancelled touch must not be resurrected by a
///        bare `PointerMoved`.
///
///        After a cancel, egui's `primary_down()` is latched `true` with
///        nothing left that will ever clear it, so the tracker's distrust
///        is the only thing standing between that and a phantom gesture.
///        Motion keeps arriving regardless — `egui-winit` clears
///        `pointer_touch_id` on cancel (`lib.rs:922`) and then admits the
///        *next* finger's moves as `PointerMoved` with no press
///        (`lib.rs:894`, `lib.rs:906`) — so "a cancel is followed by
///        silence" is true of that finger, not of the pointer stream.
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

    // A second finger, still on the glass, moves.
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

/// 6f-R2. **PROBE R2** — the same, for `MouseMoved`: a delta with no
///        coordinates at all. This is the worst resurrection vector,
///        because the phantom would land at `last_pos` — exactly where the
///        OS took the touch away.
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

/// 6f-R3. **PROBE R3** — and for a cancelled *zoom drag*: motion must not
///        hand the map back to a gesture the OS took away.
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
///        `Touch{Cancel}`.
///
///        eframe 0.34.1's `install_touchcancel` pushes `push_touches(Cancel)`
///        and nothing else (`eframe/src/web/events.rs:788`): no release, no
///        `PointerGone`. Keying cancellation on `PointerGone` alone never
///        fired here at all — the map stayed un-pannable behind a stuck
///        tooltip until the idle backstop, a minute later. Browsers fire
///        `touchcancel` routinely (scroll takeover, page hide, too many
///        contact points).
#[test]
fn web_touch_cancel_releases_the_map() {
    let mut h = InputHarness::new();
    let pos = h.map_center();

    h.web_touch_start(pos);
    h.frame_after(FRAME_DT);
    // A little jitter, as a real finger produces — and as a browser
    // delivers it, so the cancellation below is not reached through an
    // artificially silent stream.
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

    // `mousemove` on the canvas is a bare `PointerMoved` and does not care
    // that a touch was involved — it must not undo the cancellation.
    h.web_mouse_move(pos + egui::vec2(70.0, 40.0));
    h.frame_after(FRAME_DT);

    h.assert_every_frame_for(WATCH_PAST_LONG_PRESS, 0.1, |frame, outcome| {
        assert_eq!(outcome.touch.long_press_pos, None, "frame {frame}");
        assert!(!outcome.touch.suppress_pan, "frame {frame}");
    });
}

/// 6f-R5. **PROBE R5** — the button was released *outside* the window.
///
///        `egui-winit` drops a mouse release while the cursor is out of the
///        window (`lib.rs:796` needs a position it no longer has), so egui
///        reports the button as down forever afterwards. Coming back is
///        then indistinguishable from coming back still holding it, which
///        is why no hold may arm on the strength of motion alone.
#[test]
fn a_release_outside_the_window_does_not_return_as_a_hold() {
    let mut h = InputHarness::new();
    let inside = h.map_center();

    h.mouse_press(inside);
    h.frame_after(FRAME_DT);

    // Out of the window; the release that happens out there never arrives.
    h.cursor_left();
    h.frame_after(FRAME_DT);

    // Back in, hovering, nothing held.
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

/// 6g. **PROBE G** — recovery after the idle backstop fires. The finger was
///     resting, the backstop stopped believing in it, and then it moves.
///     Expiry latches (so the long press cannot pick a phantom finger back
///     up), but the latch has to be undoable by the finger itself —
///     otherwise a resumed drag needs a lift and a fresh press.
#[test]
fn pointer_recovers_from_idle_expiry_without_a_lift() {
    let mut h = InputHarness::new();
    let start = h.map_center();

    h.touch_start(start);
    assert!(h.frame_after(FRAME_DT).touch.long_press_pos.is_none());

    // Total silence, past the backstop.
    let expired = h.frames_for((SILENCE_MUST_EXPIRE_S / 0.5) as usize, 0.5);
    assert_eq!(
        expired.touch.long_press_pos, None,
        "precondition: the backstop gave up on the pointer"
    );
    assert!(!expired.touch.suppress_pan);

    // The finger was there all along, and starts moving again.
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
///
///     This is the case the idle backstop has to be sized against: reading
///     a radar value means holding a finger in one place and emitting
///     nothing at all, and half a minute of that is an ordinary thing to do.
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

    // Not one event for thirty seconds; the finger has not moved a pixel.
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
///
///    egui buckets touches by `TouchDeviceId` and only builds a gesture from
///    two touches on one device. winit's web backend fabricates a device id
///    per finger, so each finger landed in its own bucket holding one touch
///    and `zoom_delta()` never moved off 1.0. Same fingers, same positions —
///    only the device ids differ.
#[test]
fn a_pinch_only_forms_when_both_fingers_share_a_touch_device() {
    /// Spread two fingers from 100px apart to 200px apart, and report what
    /// egui made of it. `devices` is the device id each finger arrives on.
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
                    // egui refuses to *start* a gesture without a pointer
                    // position (`touch_state.rs:229`). `egui-winit` emulates
                    // one from the first finger, so a bare two-finger event
                    // stream would be unrepresentative without it.
                    egui::Event::PointerMoved(first),
                    touch(devices[1], 2, phase, centre + egui::vec2(half, 0.0)),
                ],
                ..Default::default()
            }
        };

        // Three frames, not two. egui hands `TouchState` the *previous*
        // frame's `interact_pos` (`input_state/mod.rs:390`) and refuses to
        // start a gesture without one, so the gesture only begins on the
        // second frame — and begins with no `previous`, i.e. a zoom of 1.0.
        // The third frame is the first that can report a ratio at all.
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
///
///     Collapsing the `TouchId`s too would leave egui with one finger and no
///     gesture at all, which looks identical from the outside until you
///     notice nothing zooms.
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
///
///     Driven through `Gui::ui`, so this is walkers' own `zoom_delta()` path
///     acting on the shipped pane — the thing that was dead in the browser.
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

/// 7d. …and pinching in zooms out. Direction, pinned separately, because a
///     sign error passes 7c whenever the gap happens to grow.
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

/// 7e. A pinch is not a tap and not a long press. Two fingers resting while
///     the gesture runs must not open an overlay popup or raise a tooltip.
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
    // Frame by frame, not just the last one: a confirmed tap surfaces
    // exactly [`DOUBLE_TAP_TIMEOUT_S`] after the release and
    // `take_confirmed_tap` consumes it on that one frame, so a run that only
    // reads the final outcome is looking 0.6s after the evidence went away.
    h.assert_every_frame_for(1.0, 0.2, |frame, outcome| {
        assert_eq!(
            outcome.resolved.overlay_click_pos, None,
            "frame {frame}: lifting out of a pinch must not become a \
                 deferred tap either"
        );
    });
}

/// 7e-i. A quick flick is not a tap — **distance alone**.
///
///     The classifier is `duration < A && distance < B`, and the pinch above
///     breaks both conjuncts at once, so it holds neither one in place: with
///     `TAP_DISTANCE_MAX_PX` widened to 2000 the pinch is still too slow to
///     be a tap and 7e passes regardless. This flick is comfortably inside
///     the duration bound and only outside the distance one, so it fails if
///     and only if the distance bound stops doing its job — which is a drag
///     of the map opening an overlay popup under the finger.
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
    // ~0.08s and 120px: well under TAP_DURATION_MAX_S, well over
    // TAP_DISTANCE_MAX_PX.
    h.touch_end(pos + egui::vec2(120.0, 0.0));

    h.assert_every_frame_for(1.0, 0.1, |frame, outcome| {
        assert_eq!(
            outcome.touch.overlay_click_pos, None,
            "frame {frame}: a 120px flick is a drag, not a tap"
        );
    });
}

/// 7e-ii. A slow stationary press is not a tap — **duration alone**.
///
///     The mirror of 7e-i, and what pins `TAP_DURATION_MAX_S`: the finger
///     never moves, so the distance bound is satisfied throughout and only
///     the duration bound can reject it. Half a second is past
///     `TAP_DURATION_MAX_S` and short of `LONG_PRESS_DURATION_S`, so this is
///     the deliberate-but-not-yet-a-long-press hold, which must resolve to
///     nothing at all.
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
///
///     The dangerous order is the *first* finger going up while the second
///     stays down: that is the one backing the emulated pointer, so
///     `egui-winit` fires a release and a `PointerGone` while a finger is
///     still on the glass. The second finger's later events must not put the
///     map back into a suppressed state.
#[test]
fn a_pinch_ending_one_finger_at_a_time_leaves_the_map_pannable() {
    let mut h = InputHarness::new();
    let centre = h.pane_rects()[0].center();

    h.web_pinch(centre, 80.0, 320.0, 8);

    // The emulated-pointer finger leaves first; the other is still down.
    let left = centre - egui::vec2(160.0, 0.0);
    let right = centre + egui::vec2(160.0, 0.0);
    h.web_first_finger_up(left);
    let lifted = h.frame_after(FRAME_DT);
    assert!(
        !lifted.resolved.suppress_pan,
        "the map must be released the moment the primary finger goes"
    );

    // The survivor keeps moving, then lifts.
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

    // And it must stay that way well past the long-press threshold.
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
///
///     One detent of the same wheel: Chromium reports `deltaY: 120` in
///     `DOM_DELTA_PIXEL`, Firefox `deltaY: 6` in `DOM_DELTA_LINE`. Those two
///     numbers come from two different browsers, which is what makes this
///     test independent of [`PX_PER_WHEEL_LINE`] — anchoring the line side on
///     the pixel number the *same* browser would have sent only restates the
///     constant. Without the rewrite egui scales the line form by
///     `line_scroll_speed`, 8.0 on web, so Firefox's notch lands as 48
///     against Chromium's 120 and the map zooms 2.5x slower. Nothing errors;
///     the wheel just feels wrong in one browser, which is why this is pinned
///     on the ratio rather than on any absolute step.
///
///     The band is 2% because the ratio at the shipped constant is exactly
///     1.0 — `6 × 20` *is* 120, so the two runs see identical events. A 5%
///     error in `PX_PER_WHEEL_LINE` moves it 5%, so anything looser stops
///     being a calibration and starts being a smoke test.
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
            // egui bleeds a wheel impulse out over several frames, so the
            // step is only whole once the smoothing has drained.
            last = h.frames_for(12, FRAME_DT).resolved_zoom;
        }
        last - before
    }

    // Negative delta is a scroll *down*, which walkers zooms out on; take
    // the magnitude so the two are compared on the same axis.
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
    // 6 lines is the notch Firefox reports; 20 px a line makes it the 120 px
    // Chromium reports for the same detent.
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
///
///     `egui-winit` divides pixel deltas by `pixels_per_point`, which folds
///     in the zoom factor; line deltas never went through it. Left alone,
///     turning up the UI scale would slow the wheel in one browser and not
///     the other.
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
///
///    A portrait phone split into three panes (`[2, 1]`) is the case no
///    per-pane threshold can get right: the two top panes come out clearly
///    portrait and the bottom one clearly landscape, so keying on each
///    pane's own rect paints two bottom bars and one right-hand bar on the
///    same screen.
///
///    This asserts on the *painted strips*, not on the resolved value,
///    because the resolved value was never the part at risk: what needed
///    pinning was that `render_panes` resolves from the panel, that the
///    answer is threaded through `PaneRenderCtx`, and that neither renderer
///    quietly recomputes it from the pane it happens to be drawing.
#[test]
fn every_pane_draws_its_color_scale_on_the_same_edge() {
    // Sized so the full-bleed map keeps the same shape the docked chrome
    // used to leave — portrait top panes over a landscape bottom one —
    // while staying Expanded, where the open layers panel keeps layer
    // sync seeding the panes the split adds.
    let mut h = InputHarness::with_screen(egui::vec2(1010.0, 1450.0));
    h.set_pane_count(3);
    h.frame();

    let panes = h.pane_rects();
    assert_eq!(panes.len(), 3, "precondition: a [2, 1] grid");

    // Preconditions, so this fails loudly rather than silently stopping
    // being a test if the layout maths ever changes.
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
        let (horizontal, vertical) = h.color_scale_strips(*pane);
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
///
///     The `[2, 1]` test above cannot see the difference: in that grid
///     pane 0 is `panel_w/2 × panel_h/2`, which has the *same* aspect ratio
///     as the panel, so keying on the panel and keying on the active pane
///     agree by construction and its precondition is simultaneously a
///     statement about both.
///
///     A 2-pane grid separates them: each pane is `panel_w/2 × panel_h`, so
///     its ratio is exactly twice the panel's. At 1180×1000 the panel comes
///     out landscape while both panes are emphatically portrait, and the
///     two candidate keys give opposite answers.
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
        let (horizontal, vertical) = h.color_scale_strips(*pane);
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
///
///     `format_legend_value` and `RadarProduct::unit_label` are pinned
///     directly in `ui_map_pane.rs`; this is the other end of the same wire.
///     What it adds is that the preference the settings dialog writes is the
///     one `render_color_scale` is handed — it travels the whole way through
///     `Gui::ui` and `PaneRenderCtx` — and that a unit change relabels every
///     tick, not just the title above them. A bar reading `in` over
///     millimetre numbers, or millimetre ticks under an inch title, is the
///     half-converted state this rules out.
#[test]
fn the_mehs_colour_bar_paints_the_users_hail_size_unit() {
    use rustdar_radar::types::RadarProduct;
    use rustdar_units::HailSizeUnit;

    /// The ¼-in stops of `palette::MEHS` as the bar labels them in inches.
    const INCH_TICKS: [&str; 8] = ["0.2", "0.5", "0.8", "1.2", "1.5", "1.8", "2.5", "3.5"];
    /// The same stops in whole millimetres, 1.00 in landing on 25.
    const MM_TICKS: [&str; 12] = [
        "6", "13", "19", "25", "32", "38", "44", "51", "64", "76", "89", "102",
    ];

    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    h.select_product(0, RadarProduct::MaxExpectedHailSize);
    let pane = h.pane_rects()[0];

    // The default nobody has to choose.
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

/// 53. A tap that lands on a floating dialog is filtered out by the
///     dialog-blocking gate — for both the mouse and the touch path.
///     (Renumbered from a colliding 7 — the gesture suite owns that block.)
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

    // Mouse path: egui reports the click, the gate drops it.
    let clicked = h.mouse_click(pos);
    assert_eq!(clicked.mouse.overlay_click_pos, None);
    assert!(!clicked.mouse.suppress_pan);

    // Touch path: the deferred tap is dropped as well, and nothing is
    // emitted once the double-tap window closes. (Note this half is caught
    // earlier, by the on-floating-UI check inside DoubleTapDragDetector —
    // `tap_confirmed_under_a_dialog_is_filtered_out` covers the gate
    // itself.)
    let tapped = h.touch_tap(pos);
    assert_eq!(tapped.touch.overlay_click_pos, None);
    let settled = h.frames_for(3, 0.3);
    assert_eq!(settled.touch.overlay_click_pos, None);

    // Sanity: with the dialog closed, the same position is clickable again.
    h.gui_mut().set_time_dialog_open_for_test(false);
    h.warm_up();
    assert!(!h.is_floating_layer_at(pos));
    let clicked = h.mouse_click(pos);
    assert_eq!(clicked.mouse.overlay_click_pos, Some(pos));
}

/// 53b. A touch tap is deferred by 0.4s, so a dialog can open *during* the
///     deferral. The tap was legitimately on the map when it happened, so
///     the detector's own on-release check passes it through, and only
///     `filter_dialog_blocked` can stop it from punching through the dialog
///     that is now covering it.
#[test]
fn tap_confirmed_under_a_dialog_is_filtered_out() {
    let mut h = InputHarness::new();
    let pos = h.map_center();

    // Tap on the bare map: nothing is floating there yet.
    assert!(!h.is_floating_layer_at(pos));
    let tapped = h.touch_tap(pos);
    assert_eq!(tapped.touch.overlay_click_pos, None, "still deferred");

    // A dialog opens over the tap position before the window closes.
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

    // Sanity: the identical sequence without the dialog does deliver the
    // tap, so the assertion above is about the gate and not about the tap
    // being swallowed somewhere else.
    h.gui_mut().set_time_dialog_open_for_test(false);
    h.warm_up();
    h.touch_tap(pos);
    let confirmed = h.frame_after(AFTER_DOUBLE_TAP_TIMEOUT);
    assert_eq!(confirmed.touch.overlay_click_pos, Some(pos));
}

// ── The modality gate ────────────────────────────────────────────────
//
// These are the only tests that read `FrameOutcome::resolved`. The `mouse`
// and `touch` fields deliberately bypass the gate, so asserting on them
// here would prove nothing about it.

/// 9. **A slow mouse press is not a long press.**
///
///    `LongPressDetector` keys purely on "primary down for 0.8s", so under
///    a mouse it fires on an ordinary slow click — and because a long press
///    raises `suppress_pan`, it takes the drag away from the map. Every map
///    pan starts with the button going down and staying down, so an ungated
///    detector breaks mouse panning outright.
///
///    The `touch` assertion is the contrast that stops this being vacuous:
///    the identical input *does* drive the detector when it is not gated,
///    so what the test observes is the gate and not a dead detector.
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
///
///     The touch path withholds every tap for `DOUBLE_TAP_TIMEOUT_S` so a
///     double-tap can claim it. Under a mouse that is 400ms of latency on
///     every overlay click, for a gesture a mouse cannot even perform.
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

/// 10b. The touch path keeps its deferral, so the test above is a statement
///      about the modality and not about the deferral having been deleted.
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
///
///     Double-clicking is an ordinary thing to do with a mouse.
///     `DoubleTapDragDetector` would read it as the opening of a
///     double-tap-drag and start scrubbing the zoom with vertical motion.
#[test]
fn a_mouse_double_click_does_not_start_a_zoom_drag() {
    let mut h = InputHarness::new();
    let pos = h.map_center();

    let before = h.frame_after(FRAME_DT).resolved_zoom;

    // Two clicks well inside the double-tap window, then drag downwards
    // while still held — the exact shape of the touch zoom gesture.
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

/// 11b. The same gesture on the ungated touch path *does* zoom, so the test
///      above is not simply asserting that the gesture never works.
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
///
///     A tap waiting for its double-tap partner is state held *inside* the
///     detector. Merely switching to a mouse hides it, because the mouse
///     branch never polls the detector at all — so the interesting case is
///     the round trip. Without an explicit reset the pending tap is still
///     sitting there when touch resumes, its 0.4s window long since
///     elapsed, and the very next touch frame promotes it: an overlay click
///     fires at a stale position the user last touched minutes ago.
///
///     Asserting only on the mouse leg would be satisfied by the branch
///     structure alone and would prove nothing about the reset.
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

    // The user picks up a mouse, somewhere else entirely.
    let elsewhere = stale + egui::vec2(200.0, 0.0);
    h.mouse_move(elsewhere);
    let switched = h.frame_after(FRAME_DT);
    assert_eq!(switched.modality, PointerModality::Mouse);
    assert_eq!(
        switched.resolved.overlay_click_pos, None,
        "nothing should fire while the mouse is in charge"
    );

    // Well past the double-tap window, so a surviving pending tap is now
    // eligible for promotion the moment the detector is polled again.
    h.frames_for(5, 0.2);

    // The finger comes back. This is the frame that would resurrect it.
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
///
///     The touch pipeline is single-pointer and stateful, so running it for
///     more than one pane would mean several detectors racing over one
///     finger. The mouse carries no such state, and resolving it for every
///     pane is what lets a click land on an overlay in a pane that is not
///     yet the active one — behaviour the desktop build always had.
///
///     Split into two real panes, because an inactive pane is not a thing
///     that exists in a one-pane layout: `render_panes` would resolve exactly
///     one pane and there would be nothing to compare it against.
#[test]
fn a_touch_reaches_only_the_active_pane_but_a_click_reaches_them_all() {
    let mut h = InputHarness::new();
    h.set_pane_count(2);
    // Pane 0's centre sits under the floating layers panel on Expanded;
    // a click there belongs to the panel, so take it off screen first.
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

    // The release frame is the one that separates the two branches: a
    // touch release carries the synthetic `PointerButton{up}` that makes
    // egui report a click, so the mouse path *would* return a position
    // here. A later, event-free frame would let both branches agree on
    // `None` and prove nothing.
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

    // ...and the active pane still gets it, once the deferral elapses.
    let confirmed = h.frame_after(AFTER_DOUBLE_TAP_TIMEOUT);
    assert_eq!(
        confirmed.resolved.overlay_click_pos,
        Some(pos),
        "the tap was deferred, not swallowed"
    );
}

/// A click can only hand the active-pane slot to a pane that exists.
///
/// `Gui::active_pane` resolves the slot as `self.panes[self.active_pane]`, so
/// an index the layout drew a cell for but the vector never grew to reach is
/// not a pane that quietly goes unpainted — it is a panic waiting for the next
/// reader, and the shell's stack+inspector `mem::take` is one of them. The skew is
/// built by hand because no production writer can produce it: both grow the
/// vector before assigning the layout. See `Gui::claim_pane_count_for_test`.
#[test]
fn a_click_on_a_cell_no_pane_occupies_leaves_the_active_pane_alone() {
    let mut h = InputHarness::new();
    h.set_pane_count(2);
    h.claim_pane_count(4);
    let panel = h.map_panel_rect();

    // The 2×2 grid's bottom-right cell: a rect for pane 3, which no
    // `PaneState` occupies.
    let ghost = crate::pane::PaneLayout::for_count(4)
        .pane_rect(3, panel)
        .center();
    assert!(
        h.pane_rects().iter().all(|r| !r.contains(ghost)),
        "precondition: the click lands outside every pane the frame drew"
    );

    h.mouse_click(ghost);

    assert_eq!(h.active_pane_index(), 0);
    assert_eq!(
        h.gui_mut().active_pane().site,
        "KTLX",
        "the slot still resolves to a pane rather than panicking"
    );
}

// ── Responsive layout ────────────────────────────────────────────────

/// 14. **Crossing a breakpoint must not move any widget's egui `Id`.**
///
///     egui keys widget memory — combo open state, scroll offsets, panel
///     sizes — on `Id`. An `Id` derived from anything layout-dependent
///     therefore looks like a *different widget* on the other side of a
///     resize, and every one of those becomes a silent reset: the user
///     drags a window edge and their scroll position jumps to the top.
///
///     This compares the `Id`s the panel actually resolved on two runs
///     rather than restating the constants that produced them, so it fails
///     for a layout-keyed `Id` however that keying was introduced.
#[test]
fn crossing_a_breakpoint_does_not_move_any_widget_id() {
    // Short enough that the stack's rows genuinely overflow their scroll
    // area — on a taller window there is no offset to lose.
    let mut h = InputHarness::with_screen(egui::vec2(1200.0, 500.0));
    // The drawer is what shows the panel below the sidebar breakpoint;
    // opening it up front means the panel is on screen for both runs.
    h.set_drawer_open(true);
    // The inspector too, so its ids join the compared set (M3 review) —
    // `insp_open` is session state, so it stays open across every resize
    // below.
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

    // Every probed id must be one egui actually knows. Without this a
    // probe reporting a constant — or an id rebuilt from a format string
    // the widget itself no longer uses — would compare equal to itself on
    // both sides of the resize and prove nothing at all.
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

    // Put real egui state behind one of those ids, so the comparison below
    // is backed by something that would visibly be lost. Reading it through
    // the probed id also pins that the panel really does key its scroll
    // area on that id rather than on a positional auto-id.
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

    // ...and across the 600pt breakpoint too, so all three widths resolve
    // the same ids. This is the hardest case: below 600pt the panels are
    // sheet pages, one at a time — the settings page is on top, so the
    // stack's ids are off screen until it is closed — but every id that IS
    // on screen must be the id the wider hosts used, or the host switch
    // silently orphans the state egui keyed on it.
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

    // Close the settings page; the Layers page beneath comes to the top,
    // under the same scroll id, with the offset stored behind it intact.
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

// 15 retired (synthesis-m1): the hamburger is gone and the chrome excludes
// no rects; M5's pill contract replaces rect exclusion with layer-based
// blocking.

/// 16b. **The map is full-bleed: the content rect minus the top bar,
///      exactly, at every breakpoint — and every floating surface sits
///      inside it.**
///
///      The Synthesis full-bleed rule: the top bar is the one docked
///      chrome, and everything else floats *over* the map. The map's rect
///      feeds pane hit-testing, `excluded_rects` and overlay texture
///      sizing, so both failure directions are silent everywhere rather
///      than obviously broken in one place: chrome that claims panel
///      space again shrinks it back, and a floating surface positioned
///      outside it is chrome floating over nothing.
///
///      Exact equality, not bounds: "the remainder after the top bar" is
///      the whole contract, and a stray one-point margin is how a second
///      docked panel starts.
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

        // With and without insets: the content rect is what the map must
        // fill, not the raw viewport.
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

                // Every floating surface floats *inside* the map. The phone
                // shell swaps the status bar for the bottom bar (and the
                // sheet, while the drawer flag has a page open).
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
                // The inline transport sits above the bottom bar, not on it.
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

            // Opening and closing the layers panel no longer resizes the
            // map — the panel floats over it.
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

// ── The menu, through the top bar's ☰ dropdown ───────────────────────

/// A compact harness with the menu open — the sheet's Menu page since the
/// phone shell: `open_menu` routes through the bottom bar's Menu item down
/// here, and the leaves come off `render_menu_drawer` over the same model
/// the ☰ dropdown renders on the wide widths. Compact is still the
/// interesting fixture: it is the width with the least room and the one the
/// old drawer-hosted menu served, so anything a presentation strands is
/// stranded here first.
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

/// A compact harness with the layers drawer open — the narrow form of the
/// layers panel, for tests about the controls it hosts.
///
/// Tall rather than phone-shaped on purpose: the controls sit inside a
/// `ScrollArea`, so on an 800pt screen the lower ones lay out past the
/// bottom edge and a synthetic click at their rect would land on nothing
/// at all — passing every "nothing changed" assertion for the wrong
/// reason. Only the *width* decides the presentation.
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

/// The rect of a drawn menu leaf, checked to be somewhere a click can
/// actually reach it.
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
///
///     Building the model inside a `mem::take` window hands every overlay
///     toggle the taken pane's empty `enabled_overlays`, so the box
///     renders unchecked and each click emits `Toggled(kind, true)` — the
///     drawer-hosted menu had exactly that bug once. The top bar builds
///     the model before any pane is taken; this holds it to that.
///     Auto-poll escapes either way by living on `self`, which is why the
///     model's own unit tests could never catch it.
#[test]
fn the_dropdown_checkboxes_show_the_live_pane_not_a_default_one() {
    let mut h = compact_with_menu();
    h.gui_mut().enable_overlay_for_test(OverlayKind::RadarSites);
    h.warm_up();

    assert!(
        h.overlay_enabled(OverlayKind::RadarSites),
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

    // Auto-poll is the control that never broke — asserting it alone would
    // have passed throughout, so it is the contrast, not the claim.
    assert_eq!(
        h.menu_leaf("Auto-poll").map(|l| l.value),
        Some(Some(true)),
        "precondition: auto-poll defaults on and reads off `self`, so it \
             was never affected by the pane being taken"
    );
}

/// 18. **A checkbox in the dropdown turns the overlay off, and it stays
///     off.**
///
///     The watched frames are the point: `apply_menu_event` used to write
///     `enabled_overlays` only, and the layers panel reloaded the config
///     over it on the next frame. Asserting straight after the click
///     passes; the user, who sees the frame after, never got the change.
///     Also pins that `render_menu_popup`'s events reach the dispatcher,
///     and that a click inside the popup does not close it — the close
///     behavior is click-outside, so flipping two toggles is one open.
#[test]
fn clicking_a_dropdown_checkbox_toggles_the_overlay_both_ways() {
    let mut h = compact_with_menu();
    h.gui_mut().enable_overlay_for_test(OverlayKind::RadarSites);
    h.warm_up();
    assert!(h.overlay_enabled(OverlayKind::RadarSites), "precondition");

    h.mouse_click(clickable_leaf(&h, "Show radar sites").center());
    assert!(
        !h.overlay_enabled(OverlayKind::RadarSites),
        "clicking a checked box left the overlay on — the dropdown can turn \
             an overlay on but never off"
    );

    // On the click frame itself the probe must report the state the
    // checkbox was *handed*, not the one the click produced. Recording
    // egui's post-click `current` instead would make a checkbox that
    // renders stale look correct on exactly the frame that matters.
    assert_eq!(
        h.menu_leaf("Show radar sites").map(|l| l.value),
        Some(Some(true)),
        "the probe recorded the post-click value, so it can no longer show \
             a checkbox being drawn from the wrong state"
    );
    for frame in 0..5 {
        h.frame_after(FRAME_DT);
        assert!(
            !h.overlay_enabled(OverlayKind::RadarSites),
            "the overlay came back on {} frame(s) after the click: the \
                 toggle reached `enabled_overlays` but not `overlay_configs`, \
                 so the layers panel reloaded it from the config and undid it",
            frame + 1
        );
    }

    // ...and back on, so this cannot pass by the click being read as an
    // unconditional "off".
    h.mouse_click(clickable_leaf(&h, "Show radar sites").center());
    h.frames_for(5, FRAME_DT);
    assert!(
        h.overlay_enabled(OverlayKind::RadarSites),
        "the toggle did not come back on"
    );

    // The checkbox on screen now agrees with the pane again — the two
    // halves of the round trip, not just the state behind it.
    assert_eq!(
        h.menu_leaf("Show radar sites").map(|l| l.value),
        Some(Some(true)),
        "the pane is on but the dropdown still draws the box unchecked"
    );
}

/// A compact dropdown harness split into two panes, with pane 1 made
/// active the way a user does it — by tapping that pane on the map.
fn compact_menu_with_pane_1_active() -> InputHarness {
    let mut h = InputHarness::with_screen(egui::vec2(420.0, 1200.0));
    h.set_pane_count(2);

    // Tap pane 1 before opening the menu, so the popup cannot be what the
    // tap lands on and the two pane rects are unambiguous.
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
///
///     With `active_pane` stuck at 0 in every fixture, both `menu_model`
///     reading `&self.panes[0]` and `set_active_pane_overlay` writing it
///     survived. In the app: pane 1 active, tap a toggle in the drawer, the
///     overlay lands on pane 0. Sync is off so the panes can disagree —
///     with it on it copies the write back and hides the bug.
#[test]
fn the_menu_reads_and_writes_the_active_pane_not_pane_zero() {
    let mut h = compact_menu_with_pane_1_active();
    h.set_sync_layers(false);

    // The panes must disagree about **two** kinds, not one.
    //
    // `RadarSites` is the kind being toggled, and `set_enabled` overwrites
    // it whichever config was loaded — so on its own it cannot show the
    // *read* going to the wrong pane. `CityLabels` is the witness:
    // `serialize_state` carries `enabled`, so loading pane 0's configs
    // imports pane 0's on/off state for every kind except the one being
    // set, and pane 1's city labels would silently go out.
    h.set_overlay_on_pane(0, OverlayKind::RadarSites, false);
    h.set_overlay_on_pane(0, OverlayKind::CityLabels, false);
    h.set_overlay_on_pane(1, OverlayKind::RadarSites, true);
    h.set_overlay_on_pane(1, OverlayKind::CityLabels, true);
    h.warm_up();
    assert!(
        h.overlay_enabled_on(1, OverlayKind::RadarSites)
            && !h.overlay_enabled_on(0, OverlayKind::RadarSites)
            && h.overlay_enabled_on(1, OverlayKind::CityLabels)
            && !h.overlay_enabled_on(0, OverlayKind::CityLabels),
        "precondition: the panes must disagree about both kinds"
    );

    // The checkbox must show pane 1's state, not pane 0's.
    assert_eq!(
        h.menu_leaf("Show radar sites").map(|l| l.value),
        Some(Some(true)),
        "the drawer drew pane 0's state while pane 1 is active"
    );

    // ...and clicking it must write to pane 1, leaving pane 0 alone.
    h.mouse_click(clickable_leaf(&h, "Show radar sites").center());
    h.frames_for(5, FRAME_DT);
    assert!(
        !h.overlay_enabled_on(1, OverlayKind::RadarSites),
        "the toggle did not reach the active pane"
    );
    assert!(
        !h.overlay_enabled_on(0, OverlayKind::RadarSites),
        "the toggle wrote to pane 0, which is not the active pane"
    );

    // The untouched kind must be untouched — on the active pane, and on
    // the one that was not being edited.
    assert!(
        h.overlay_enabled_on(1, OverlayKind::CityLabels),
        "toggling radar sites on pane 1 also turned its city labels off: \
             the config was read from pane 0, which had them off"
    );
    assert!(
        !h.overlay_enabled_on(0, OverlayKind::CityLabels),
        "pane 0's city labels changed, though it is not the active pane"
    );
}

/// 29. **A menu toggle saves the active pane's *own* overlay config.**
///
///     `render_pane_map_content` loads each pane's config as it draws it,
///     so mid-frame the handlers hold the last-drawn pane's settings.
///     `set_active_pane_overlay` then snapshots the handlers onto the
///     active pane — and `serialize_state` carries `enabled`, so a
///     snapshot taken against the wrong pane's config silently rewrites
///     every *other* overlay kind's on/off flag on the active pane.
///
///     Two separate things keep the handlers correct at that moment: the
///     reload at the end of `Gui::ui`, and the load at the top of
///     `set_active_pane_overlay`. Either alone is sufficient, so **neither
///     is individually killable** — removing just one is an equivalent
///     mutant. Removing both fails here, and only here.
///
///     Medium with the drawer shut: anywhere the layers panel is on screen
///     it reloads the active pane's config every frame and hides this.
#[test]
fn a_menu_toggle_loads_the_active_panes_config_before_saving_it() {
    let mut h = InputHarness::with_screen(egui::vec2(800.0, 900.0));
    assert_eq!(h.width_class(), crate::ui_layout::WidthClass::Medium);
    h.set_pane_count(2);
    h.set_sync_layers(false);
    assert_eq!(
        h.active_pane_index(),
        0,
        "precondition: pane 0 active, so the *last drawn* pane 1 is the one \
             whose config is left in the handlers"
    );

    h.set_overlay_on_pane(0, OverlayKind::CityLabels, true);
    h.set_overlay_on_pane(1, OverlayKind::CityLabels, false);
    h.set_overlay_on_pane(0, OverlayKind::RadarSites, false);
    h.warm_up();
    assert!(
        !h.layers_panel_on_screen(),
        "precondition: no layers panel, or its reload masks this"
    );

    h.open_menu();
    h.mouse_click(clickable_leaf(&h, "Show radar sites").center());
    h.frames_for(5, FRAME_DT);

    assert!(
        h.overlay_enabled_on(0, OverlayKind::RadarSites),
        "precondition: the toggle must have taken effect"
    );
    assert!(
        h.overlay_enabled_on(0, OverlayKind::CityLabels),
        "the active pane's city labels were overwritten by pane 1's config: \
             the handlers were saved without loading the active pane first"
    );
}

/// 28. **A menu toggle propagates to the other panes when sync is on.**
///
///     Driven on Medium with the drawer shut — the menu is always on
///     screen behind ☰, and this is a width where the layers panel is not.
///     With the panel up, the shell's stack+inspector pass calls
///     `propagate_layer_sync` itself every frame and masks the arm: a
///     panel-open version of this test passes with the call deleted.
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

    assert!(h.sync_layers(), "precondition: layer sync is on by default");
    assert!(
        !h.layers_panel_on_screen(),
        "precondition: the layers panel must NOT be on screen, or its own \
             `propagate_layer_sync` masks the arm under test"
    );

    h.set_overlay_on_pane(0, OverlayKind::RadarSites, false);
    h.set_overlay_on_pane(1, OverlayKind::RadarSites, false);
    h.warm_up();

    // Through the dropdown: open ☰, then tick the box.
    h.open_menu();
    h.mouse_click(clickable_leaf(&h, "Show radar sites").center());
    h.frames_for(5, FRAME_DT);

    assert!(
        h.overlay_enabled_on(1, OverlayKind::RadarSites),
        "precondition: the active pane must have taken the toggle"
    );
    assert!(
        h.overlay_enabled_on(0, OverlayKind::RadarSites),
        "the toggle did not propagate to the other pane, though layer sync \
             is on"
    );
}

// 19 retired (synthesis-m1): the drawer no longer hosts the menu; contract
// 76 below holds the ☰ dropdown to carrying the whole menu at every width.

/// 76. **The menu carries the whole model at every width — the ☰ dropdown
///     on the wide widths, the sheet's Menu page on the phone.**
///
///     One route to Settings, Time, Exit, Refresh and every toggle per
///     shell: the top bar's dropdown at ≥600pt, the bottom bar's Menu item
///     below it. The wanted labels are the model's own
///     (`menu_model_leaf_labels`), so a new entry joins this audit by
///     construction and a renderer that drops one fails it — naming the
///     label and the width, since "reachable on a desktop" and "reachable
///     on a phone" are separate claims.
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
            // The phone half of the contract: the whole menu is the sheet's
            // Menu page, not a popup squeezed onto a phone.
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
///
///     A click on "Exit" has to become a `GuiAction::Exit`. `Exit` and
///     `RefreshRadar` dispatch to a one-line arm, so a test that only walks
///     the model and calls `apply_menu_event` proves nothing about them:
///     an exhaustive `match` already guarantees the arm exists.
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
///
///     The widest width, driven the way a user drives it: click ☰ to open
///     the dropdown, then click the checkbox inside it. Nothing here
///     reaches into egui's popup memory — and the layers sidebar is open
///     at this width, so this is also the width where a half-written
///     toggle would be reverted by the panel's per-frame reload.
#[test]
fn a_toggle_flipped_in_the_dropdown_reaches_the_dispatcher() {
    let mut h = InputHarness::with_screen(egui::vec2(1200.0, 800.0));
    assert_eq!(
        h.width_class(),
        crate::ui_layout::WidthClass::Expanded,
        "precondition: the widest class, with the sidebar up"
    );
    h.gui_mut().enable_overlay_for_test(OverlayKind::RadarSites);
    h.warm_up();
    assert!(h.overlay_enabled(OverlayKind::RadarSites), "precondition");

    h.open_menu();
    assert_eq!(
        h.menu_leaf("Show radar sites").map(|l| l.value),
        Some(Some(true)),
        "the open dropdown must draw the toggle, from the live pane"
    );

    h.mouse_click(clickable_leaf(&h, "Show radar sites").center());
    h.frames_for(5, FRAME_DT);
    assert!(
        !h.overlay_enabled(OverlayKind::RadarSites),
        "the dropdown's toggle never reached apply_menu_event, or was \
             reverted by the layers panel on a later frame"
    );
}

/// 22. **The pane picker narrows on a phone; the config clamp does not.**
///
///     The two limits differ deliberately and each has to be read by the
///     right code. The values were pinned as constants, but nothing checked
///     the picker consulted the width class at all. The other half — a wide
///     layout surviving a load on a phone — is pinned in `ui_config.rs`.
///
///     The top bar draws the full row of buttons at every width and
///     disables the ones past the offer, so "narrows" is now a claim about
///     the enabled subset — and about a disabled button really refusing
///     the click.
#[test]
fn the_pane_picker_offers_fewer_panes_on_a_phone_than_on_a_desktop() {
    use crate::pane::{MAX_PANES_DESKTOP, MAX_PANES_MOBILE};

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
    // The phone top bar has no segments (plan §1.2): the sheet's Layers
    // page header carries them, so the picker is read with that page open.
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

    // Compared as *rendered* ranges rather than as the two constants, which
    // clippy would fold to `true` — and a precondition that is true by
    // construction is not one.
    assert!(
        enabled_counts(&compact).len() < enabled_counts(&expanded).len(),
        "precondition: the two ranges must differ, or both assertions above \
             are satisfied by one constant"
    );

    // The probe's own statement of the offer must be the enabled subset
    // the bar really drew — the counts are contiguous from 1, so the
    // subset's size *is* its ceiling. This is what M6's phone sheet header
    // will read, and a probe that merely restated the constant would let
    // the two drift apart unseen.
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

    // A disabled button is drawn but refuses the click — the difference
    // between "disabled" and "decorative".
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

    // The buttons must be real: exactly one reads as selected, and it is
    // the count actually in force. A probe rebuilt from `max_panes` would
    // agree with the range above while the loop drew nothing.
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

    // ...and clicking one takes effect, which is the half no probe of the
    // *offered range* can reach.
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

// ── The top bar's own controls ───────────────────────────────────────

/// 77. **The Layers toggle hides and restores the Expanded sidebar with
///     its state intact.**
///
///     The `stack_open` contract, driven the user's way: a click on the
///     really-drawn toggle, never the setter. The restore half is the one
///     that carries weight — the panel must come back under the same
///     `layers_scroll` id with the same offset stored behind it, because
///     a toggle that rebuilt the panel under a fresh id would look
///     identical and silently cost the user their place in the list.
#[test]
fn the_layers_toggle_hides_and_restores_the_expanded_sidebar_with_its_state() {
    // Short enough that the stack's rows genuinely overflow their
    // scroll area — on a taller window there is no offset to preserve.
    let mut h = InputHarness::with_screen(egui::vec2(1200.0, 500.0));
    assert_eq!(
        h.width_class(),
        crate::ui_layout::WidthClass::Expanded,
        "precondition: the width with a persistent sidebar"
    );
    assert!(
        h.layers_panel_on_screen(),
        "precondition: the sidebar is the shell default on Expanded"
    );

    // Real state behind a real id, so "intact" is a claim about something.
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

/// 78. **An explicit sidebar choice neither leaks into the drawer nor
///     expires at the breakpoint.**
///
///     `stack_open` and `drawer_open` answer at different widths and
///     remember independently — that is the whole reason they are two
///     fields. Closing the Expanded sidebar, crossing to Medium, working
///     the drawer there and coming back must find the explicit
///     `Some(false)` still standing; a widened `drawer_open` would fail
///     one direction or the other.
#[test]
fn an_explicit_sidebar_choice_survives_the_breakpoint_without_leaking() {
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    assert_eq!(h.width_class(), crate::ui_layout::WidthClass::Expanded);
    assert!(
        h.layers_panel_on_screen(),
        "precondition: the shell default"
    );

    // The explicit choice: close the sidebar.
    h.mouse_click(h.top_bar().layers_toggle.0.center());
    h.warm_up();
    assert!(!h.layers_panel_on_screen(), "precondition: closed by hand");

    // Below the breakpoint the drawer governs, and it starts closed —
    // not because the sidebar was closed, but because it always does.
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

    // The same toggle opens and closes the drawer, unencumbered by the
    // sidebar's `Some(false)`.
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

    // Back on Expanded, the explicit choice is still in force — working
    // the drawer must not have expired it.
    h.set_screen(egui::vec2(1400.0, 900.0));
    assert_eq!(h.width_class(), crate::ui_layout::WidthClass::Expanded);
    assert!(
        !h.layers_panel_on_screen(),
        "crossing the breakpoint and back reopened a sidebar the user \
         explicitly closed"
    );

    // ...and it is a choice, not a latch: the toggle still reopens it.
    h.mouse_click(h.top_bar().layers_toggle.0.center());
    h.warm_up();
    assert!(
        h.layers_panel_on_screen(),
        "the toggle could not reopen the sidebar after the round trip"
    );
}

/// 79. **A fresh session opens the layers panel only where it is
///     persistent.**
///
///     The `None` default of `stack_open`, observed at every width: the
///     sidebar up on Expanded with nothing having asked for it, and the
///     drawer down on Medium and Compact — with the toggle's drawn state
///     agreeing, since a toggle that read open over a closed drawer would
///     invert its first click.
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
        // The control that answers for the panel differs by shell: the top
        // bar's toggle on the wide widths, the bottom bar's Layers item on
        // the phone.
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

/// 80. **The bar's arm toggles arm, swap and disarm through real clicks.**
///
///     The end-to-end route the probe fields exist for: a click on the
///     drawn Region toggle arms the drag and the next frame's probe shows
///     it on; a click on X-sec swaps the arms — the mutual exclusion via
///     the *bar*, not just the menu path that `set_region_arm`'s own tests
///     cover; a second click on the armed toggle disarms.
#[test]
fn the_bars_arm_toggles_arm_swap_and_disarm_through_real_clicks() {
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    let (region, on) = h.top_bar().region_arm;
    assert!(!on, "precondition: nothing armed in a fresh session");
    assert!(
        !h.top_bar().section_arm.1,
        "precondition: nor the section draw"
    );

    h.mouse_click(region.center());
    h.warm_up();
    assert!(
        h.region_arm(),
        "clicking the bar's Region toggle did not arm"
    );
    assert!(
        h.top_bar().region_arm.1,
        "the drag is armed but the toggle does not show it"
    );

    // The swap: arming the other drag through the bar un-arms this one.
    h.mouse_click(h.top_bar().section_arm.0.center());
    h.warm_up();
    assert!(
        h.section_draw_armed(),
        "clicking the bar's X-sec toggle did not arm the draw"
    );
    assert!(
        !h.region_arm(),
        "the region drag stayed armed beside the section draw: the bar's \
         clicks bypass the mutual exclusion the setters carry"
    );
    assert_eq!(
        (h.top_bar().section_arm.1, h.top_bar().region_arm.1),
        (true, false),
        "the two toggles do not show the swap"
    );

    // And off again, from the bar.
    h.mouse_click(h.top_bar().section_arm.0.center());
    h.warm_up();
    assert!(
        !h.section_draw_armed() && !h.region_arm(),
        "a second click on the armed toggle did not disarm"
    );
}

/// 81. **Arming from the bar closes the open ☰ dropdown.**
///
///     The bar-click sibling of the in-menu rule (`render_top_bar`'s
///     `close_kind`): the next thing the user does is a drag on the map,
///     and an open menu is in its way. The bar's toggle is outside the
///     popup, so this is the `CloseOnClickOutside` half — the click both
///     arms and closes, and neither eats the other.
#[test]
fn arming_from_the_bar_closes_the_open_dropdown() {
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    h.open_menu();
    assert!(
        !h.menu_leaves().is_empty(),
        "precondition: the dropdown is open"
    );

    let (region, _) = h.top_bar().region_arm;
    assert!(
        !h.is_floating_layer_at(region.center()),
        "precondition: the toggle must not sit under the open popup, or \
         the click below lands on the popup instead"
    );

    h.mouse_click(region.center());
    h.frame_after(FRAME_DT);
    assert!(
        h.menu_leaves().is_empty(),
        "the dropdown stayed open over the armed drag"
    );
    assert!(
        h.region_arm(),
        "closing the dropdown ate the click that was also an arm"
    );
}

/// 82. **The bar never overlaps itself at Medium's narrowest width.**
///
///     600pt with six panes is the worst case the docked bar must simply
///     absorb: the roomy run would overrun the width, and before the arms
///     claimed the right edge first the right-to-left toggles were laid
///     out in whatever sliver was left — overlapping the segments, or
///     degenerate. Asserted on the drawn rects: everything inside the
///     bar's own rect, no segment under an arm, no painted label under an
///     arm, and the arms still taking clicks.
#[test]
fn the_bar_never_overlaps_at_mediums_narrowest_width() {
    let mut h = InputHarness::with_screen(egui::vec2(600.0, 900.0));
    assert_eq!(
        h.width_class(),
        crate::ui_layout::WidthClass::Medium,
        "precondition: 600pt is Medium's floor"
    );
    h.set_pane_count(crate::pane::MAX_PANES_DESKTOP);
    assert_eq!(
        h.pane_count(),
        crate::pane::MAX_PANES_DESKTOP,
        "precondition: the widest segment row the bar can be asked for"
    );

    // The captions are the first thing the squeeze gives up — their
    // absence here is how a test can see the tight form really engaged,
    // rather than the roomy one happening to fit a wider font's luck.
    assert!(
        h.painted_text_strings()
            .iter()
            .all(|t| t != "Panes:" && t != "Pane:"),
        "the bar kept its captions at a width they cannot fit"
    );
    // ...and the contrast that keeps that from passing vacuously: a roomy
    // desktop draws them.
    let wide = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    assert!(
        wide.painted_text_strings().iter().any(|t| t == "Panes:"),
        "a 1400pt bar dropped its captions, so the adaptive decision is \
         stuck tight and the assertion above says nothing"
    );

    let probe = h.top_bar();
    let bar = probe.rect.expand(0.5);
    let arms = [probe.region_arm.0, probe.section_arm.0];
    for (name, rect) in [
        ("the \u{2630} button", probe.menu_button),
        ("the Layers toggle", probe.layers_toggle.0),
        ("the Region toggle", probe.region_arm.0),
        ("the X-sec toggle", probe.section_arm.0),
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

    // Every text the bar painted — which reaches the probe-less widgets,
    // the active-pane selector above all — must stay clear of the arms
    // too. The arms' own labels are exempted by their centre.
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

    // Squeezed, not sacrificed: the toggles still take their clicks.
    h.mouse_click(probe.region_arm.0.center());
    h.warm_up();
    assert!(
        h.region_arm(),
        "the Region toggle stopped responding at the squeezed width"
    );
    h.mouse_click(h.top_bar().section_arm.0.center());
    h.warm_up();
    assert!(
        h.section_draw_armed() && !h.region_arm(),
        "the X-sec toggle stopped responding at the squeezed width"
    );
}

/// 83. **A dismiss with the ☰ dropdown open closes it, and only it.**
///
///     Escape's real shape: egui's `Popup` closes itself on the Escape
///     *it* sees, and the frontend independently resolves the same press
///     through `dismiss_top_layer` — so without the popup at the chain's
///     head, one press closed the popup *and* the layer beneath it. Both
///     halves are driven here exactly as they ship: the dismiss first,
///     the key event in the following frame's input.
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

    // The second press is the drawer's.
    assert!(h.gui_mut().dismiss_top_layer(), "the drawer was still open");
    h.warm_up();
    assert!(
        !h.layers_panel_on_screen(),
        "the second press did not close the drawer"
    );
}

/// 83b. **Android's back closes the menu with no key event at all.**
///
///      The other route: a logical back press never enters egui's queue.
///      On the wide widths the popup's own Escape handling cannot see it
///      and the request flag is what closes the dropdown; on the phone the
///      menu is the sheet's Menu page and the chain's `menu_open` arm is
///      the whole mechanism — this fixture is the phone, so it pins that
///      arm.
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

    // Only the popup was open, so the next press must fall through to
    // the exit path — one layer per press, counted exactly.
    assert!(
        !h.gui_mut().dismiss_top_layer(),
        "the popup press left something else consumed as well"
    );
}

// ── The layer stack and the inspector ────────────────────────────────

/// A two-pane Expanded harness with pane 1 made active the user's way — a
/// click on that pane — so the stack and inspector demonstrably describe
/// the pane the user is working in, not pane 0.
fn expanded_with_pane_1_active() -> InputHarness {
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    h.set_pane_count(2);
    // Clear of the stack on the left and the chrome bands.
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
///
///     Contract 27's claim, ported to the new pass: the stack renders from
///     the pane the shell `mem::take`s, so a stack that read the placeholder
///     — or indexed `panes[0]` — would draw every eye from the wrong pane
///     and write every click to it. Sync is off so the panes can disagree;
///     `CityLabels` is the witness kind for the write half, exactly as in
///     contract 27: `serialize_state` carries `enabled`, so a snapshot taken
///     against the wrong pane's config silently rewrites every *other*
///     kind's flag.
#[test]
fn the_stacks_rows_read_and_write_the_active_pane_not_pane_zero() {
    let mut h = expanded_with_pane_1_active();
    h.set_sync_layers(false);
    h.set_overlay_on_pane(0, OverlayKind::RadarSites, false);
    h.set_overlay_on_pane(0, OverlayKind::CityLabels, false);
    h.set_overlay_on_pane(1, OverlayKind::RadarSites, true);
    h.set_overlay_on_pane(1, OverlayKind::CityLabels, true);
    h.warm_up();

    // The read half: the eye shows pane 1's state.
    let row = h
        .stack_row(OverlayKind::RadarSites)
        .expect("the stack must draw a RadarSites row");
    assert!(
        row.eye_on,
        "the eye drew pane 0's state while pane 1 is active"
    );

    // The write half: clicking it writes pane 1, and only the toggled kind.
    h.mouse_click(row.eye.center());
    h.frames_for(5, FRAME_DT);
    assert!(
        !h.overlay_enabled_on(1, OverlayKind::RadarSites),
        "the eye did not reach the active pane"
    );
    assert!(
        !h.overlay_enabled_on(0, OverlayKind::RadarSites),
        "the eye wrote to pane 0, which is not the active pane"
    );
    assert!(
        h.overlay_enabled_on(1, OverlayKind::CityLabels),
        "toggling radar sites on pane 1 also turned its city labels off: \
             the config was read from the wrong pane"
    );
    assert!(
        !h.overlay_enabled_on(0, OverlayKind::CityLabels),
        "pane 0's city labels changed, though it is not the active pane"
    );
}

/// 85. **The eye turns a layer off, and it stays off — and back on.**
///
///     Contract 18's watched-frames claim, ported to the eye: a toggle that
///     reached `enabled_overlays` but not `overlay_configs` is undone the
///     next time the handlers reload from the config, so asserting straight
///     after the click passes while the user's change evaporates. Both
///     directions, so this cannot pass by the click being read as an
///     unconditional "off".
#[test]
fn the_eye_toggles_a_layer_both_ways_and_it_sticks() {
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    h.gui_mut().enable_overlay_for_test(OverlayKind::RadarSites);
    h.warm_up();
    assert!(h.overlay_enabled(OverlayKind::RadarSites), "precondition");

    let row = h.stack_row(OverlayKind::RadarSites).expect("row drawn");
    assert!(row.eye_on, "the eye must draw the live state");
    h.mouse_click(row.eye.center());
    for frame in 0..5 {
        h.frame_after(FRAME_DT);
        assert!(
            !h.overlay_enabled(OverlayKind::RadarSites),
            "the overlay came back on {} frame(s) after the eye click: the \
                 toggle reached `enabled_overlays` but not `overlay_configs`",
            frame + 1
        );
    }
    assert!(
        !h.stack_row(OverlayKind::RadarSites).expect("row").eye_on,
        "the layer is off but the eye still draws it on"
    );

    let row = h.stack_row(OverlayKind::RadarSites).expect("row");
    h.mouse_click(row.eye.center());
    h.frames_for(5, FRAME_DT);
    assert!(
        h.overlay_enabled(OverlayKind::RadarSites),
        "the eye did not turn the layer back on"
    );
}

/// 86. **The inspector's Show toggle is the eye's equal: both ways, and it
///     sticks.**
///
///     The layer body's master toggle goes through the same
///     `write_pane_overlay` discipline as the eye, from inside the same
///     take window — this holds it there. Also pins that the master shows
///     the live state, through the probe's handed-value convention.
#[test]
fn the_inspectors_show_toggle_toggles_both_ways_and_it_sticks() {
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    h.gui_mut().enable_overlay_for_test(OverlayKind::RadarSites);
    h.warm_up();
    h.open_layer_in_inspector(OverlayKind::RadarSites);

    let (master, on) = h.inspector().master.expect("the layer body's toggle");
    assert!(on, "the Show toggle must draw the live state");
    h.mouse_click(master.center());
    for frame in 0..5 {
        h.frame_after(FRAME_DT);
        assert!(
            !h.overlay_enabled(OverlayKind::RadarSites),
            "the overlay came back on {} frame(s) after the Show click",
            frame + 1
        );
    }

    let (master, on) = h.inspector().master.expect("still drawn");
    assert!(!on, "the layer is off but the Show toggle still draws on");
    h.mouse_click(master.center());
    h.frames_for(5, FRAME_DT);
    assert!(
        h.overlay_enabled(OverlayKind::RadarSites),
        "the Show toggle did not turn the layer back on"
    );
}

/// 87. **An eye toggle saves the active pane's *own* overlay config.**
///
///     Contract 29's claim, ported to the eye. `render_pane_map_content`
///     loads each pane's config as it draws it, so at the end of a frame
///     the handlers hold the *last-drawn* pane's settings — and
///     `serialize_state` carries `enabled`, so a snapshot taken against the
///     wrong pane's config silently rewrites every other kind's flag on the
///     active pane. Three separate things keep the handlers correct at the
///     moment of the write: the frame-end reload in `Gui::ui`, the shell's
///     pre-take load, and the load inside `write_pane_overlay` itself. Any
///     alone is sufficient — so no single one is killable — but removing
///     all three fails here, and this is the test that names the failure.
#[test]
fn an_eye_toggle_loads_the_active_panes_config_before_saving_it() {
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    h.set_pane_count(2);
    h.set_sync_layers(false);
    assert_eq!(
        h.active_pane_index(),
        0,
        "precondition: pane 0 active, so the *last drawn* pane 1 is the one \
             whose config could be left in the handlers"
    );

    h.set_overlay_on_pane(0, OverlayKind::CityLabels, true);
    h.set_overlay_on_pane(1, OverlayKind::CityLabels, false);
    h.set_overlay_on_pane(0, OverlayKind::RadarSites, false);
    h.warm_up();

    let row = h.stack_row(OverlayKind::RadarSites).expect("row drawn");
    h.mouse_click(row.eye.center());
    h.frames_for(5, FRAME_DT);

    assert!(
        h.overlay_enabled_on(0, OverlayKind::RadarSites),
        "precondition: the eye must have taken effect"
    );
    assert!(
        h.overlay_enabled_on(0, OverlayKind::CityLabels),
        "the active pane's city labels were overwritten by pane 1's config: \
             the handlers were saved without loading the active pane first"
    );
}

/// 88. **An eye toggle propagates to the other panes when sync is on.**
///
///     Contract 28's claim, ported to the eye: the shell's pass ends with
///     `propagate_layer_sync` after the pane goes back, and this is what
///     makes a layer flipped on one pane a layer flipped on all of them.
#[test]
fn an_eye_toggle_propagates_to_the_other_panes_when_sync_is_on() {
    let mut h = expanded_with_pane_1_active();
    assert!(h.sync_layers(), "precondition: layer sync defaults on");
    h.set_overlay_on_pane(0, OverlayKind::RadarSites, false);
    h.set_overlay_on_pane(1, OverlayKind::RadarSites, false);
    h.warm_up();

    let row = h.stack_row(OverlayKind::RadarSites).expect("row drawn");
    h.mouse_click(row.eye.center());
    h.frames_for(5, FRAME_DT);

    assert!(
        h.overlay_enabled_on(1, OverlayKind::RadarSites),
        "precondition: the active pane must have taken the toggle"
    );
    assert!(
        h.overlay_enabled_on(0, OverlayKind::RadarSites),
        "the toggle did not propagate to the other pane, though layer sync \
             is on"
    );
}

/// 94. **Turning a dataless layer on fetches it, eye and Show toggle alike.**
///
///     SPC outlooks are the layer that makes this a contract rather than a
///     nicety: the handler never auto-polls, so an enable that emitted no
///     `FetchOverlay` would leave an enabled layer that stays blank forever.
///     The layer's own sub-toggles ask for the fetch through their
///     `ControlEffect`; the master routes bypass them, so they carry the
///     rule themselves.
#[test]
fn enabling_a_dataless_layer_fetches_it() {
    // The eye.
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    let row = h.stack_row(OverlayKind::SpcOutlook).expect("row drawn");
    assert!(!row.eye_on, "precondition: outlooks default off");
    h.mouse_click(row.eye.center());
    assert!(
        h.last_actions().iter().any(|a| matches!(
            a,
            crate::actions::GuiAction::FetchOverlay {
                kind: OverlayKind::SpcOutlook,
                ..
            }
        )),
        "the eye enabled a layer with no data and no auto-poll, and nothing \
             will ever fetch it"
    );

    // The Show toggle.
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    h.open_layer_in_inspector(OverlayKind::SpcOutlook);
    let (master, on) = h.inspector().master.expect("the layer body's toggle");
    assert!(!on, "precondition: outlooks default off");
    h.mouse_click(master.center());
    assert!(
        h.last_actions().iter().any(|a| matches!(
            a,
            crate::actions::GuiAction::FetchOverlay {
                kind: OverlayKind::SpcOutlook,
                ..
            }
        )),
        "the Show toggle enabled a dataless layer without fetching it"
    );
}

/// 68. **The ▲▼ buttons really reorder the draw order — permuted, bounded,
///     persisted, redrawn.**
///
///     `PaneState::draw_order` has been persisted per pane since multi-pane
///     landed, with no UI able to change it; the stack's reorder buttons are
///     that UI, and each of the four claims has its own silent failure:
///     a ▲ that swaps the wrong neighbours (the display list is the draw
///     order *reversed*, so the index arithmetic is exactly the thing to
///     pin), an end button that wraps instead of disabling, a reorder that
///     evaporates on restart because it never reached the config, and rows
///     that keep their old positions because the renderer iterated a copy.
#[test]
fn the_reorder_buttons_permute_the_draw_order_and_it_persists() {
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    let before = h.gui_mut().pane(0).expect("pane 0").draw_order.clone();
    let n = before.len();
    assert!(n >= 3, "precondition: a real layer list");

    // The ends are disabled: the top row cannot move up, the bottom cannot
    // move down — and a click there does nothing rather than wrapping.
    let rows = h.stack().rows;
    assert_eq!(rows.len(), n, "one row per layer");
    assert!(!rows[0].up.1, "the top row's \u{25b2} must be disabled");
    assert!(rows[0].down.1, "the top row's \u{25bc} must be enabled");
    assert!(
        !rows[n - 1].down.1,
        "the bottom row's \u{25bc} must be disabled"
    );
    h.mouse_click(rows[0].up.0.center());
    h.frames_for(2, FRAME_DT);
    assert_eq!(
        h.gui_mut().pane(0).expect("pane 0").draw_order,
        before,
        "clicking a disabled \u{25b2} still permuted the order"
    );

    // ▲ on the second row: drawn later, i.e. towards the *end* of
    // `draw_order` — the top row is the last-drawn layer.
    let second = rows[1].kind;
    h.mouse_click(rows[1].up.0.center());
    h.frames_for(2, FRAME_DT);
    let mut expected = before.clone();
    expected.swap(n - 1, n - 2);
    assert_eq!(
        h.gui_mut().pane(0).expect("pane 0").draw_order,
        expected,
        "\u{25b2} on the second row must swap the draw order's last two \
             entries"
    );

    // The rows re-render in the new order, from the mutated field.
    let rows = h.stack().rows;
    assert_eq!(
        rows[0].kind, second,
        "the promoted layer's row did not move to the top"
    );

    // ▼ undoes it, so the wiring is symmetric.
    h.mouse_click(rows[0].down.0.center());
    h.frames_for(2, FRAME_DT);
    assert_eq!(
        h.gui_mut().pane(0).expect("pane 0").draw_order,
        before,
        "\u{25bc} on the promoted row must put the order back"
    );

    // And the change survives the config round trip: reorder again, save,
    // and load into a fresh session.
    let rows = h.stack().rows;
    h.mouse_click(rows[1].up.0.center());
    h.frames_for(2, FRAME_DT);
    let reordered = h.gui_mut().pane(0).expect("pane 0").draw_order.clone();
    assert_ne!(reordered, before, "precondition: a real reorder to persist");

    let store = crate::config_store::MemoryConfigStore::default();
    h.gui_mut().save_ui_config(&store);
    let mut fresh = crate::Gui::new();
    assert!(fresh.load_ui_config(&store), "the saved config must load");
    assert_eq!(
        fresh.pane(0).expect("pane 0").draw_order,
        reordered,
        "the reorder did not survive the ui_config round trip"
    );
}

/// 89. **A stack row click selects that layer in the inspector, which opens
///     itself.**
///
///     The row is the route to a layer's options (plan §3.8): the click
///     must auto-open a closed inspector and land on *that* layer's body —
///     asserted through the probe's `mode`, which the body arm writes as a
///     literal, so a mis-wired dispatch cannot fake it. The crumb is the
///     user-visible half of the same claim, and `✕` deselect is the way
///     back to App › Settings.
#[test]
fn a_stack_row_click_opens_that_layers_options_in_the_inspector() {
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    assert!(
        !h.inspector().open,
        "precondition: the inspector starts closed"
    );

    let row = h.stack_row(OverlayKind::NwsAlerts).expect("row drawn");
    h.mouse_click(row.rect.center());
    h.warm_up();

    let inspector = h.inspector();
    assert!(inspector.open, "the row click did not open the inspector");
    assert_eq!(
        inspector.mode,
        Some(crate::ui::InspectorSelection::Layer(OverlayKind::NwsAlerts)),
        "the inspector opened on something other than the clicked layer"
    );
    assert_eq!(
        inspector.crumb, "Pane 1 \u{203a} NWS Alerts",
        "the crumb does not name the selection"
    );
    assert!(
        h.stack_row(OverlayKind::NwsAlerts)
            .expect("row still drawn")
            .selected,
        "the selected layer's row must draw selected"
    );

    // ✕ returns to App › Settings without closing the panel.
    h.mouse_click(inspector.deselect.center());
    h.warm_up();
    let inspector = h.inspector();
    assert!(inspector.open, "deselecting must not close the inspector");
    assert_eq!(
        inspector.mode,
        Some(crate::ui::InspectorSelection::AppSettings),
        "\u{2715} must return to App \u{203a} Settings"
    );
}

/// 90. **The ⚙ toggle and the menu's Settings… entry both reach the
///     settings body.**
///
///     The toggle mirrors the Layers toggle for the right-hand panel; the
///     menu entry lands on App › Settings specifically. Both asserted
///     through the body-arm probe, and the panel itself must float inside
///     the map like every other surface.
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

    // The menu route: Settings… opens the same body.
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
///
///     The harness holds `control_render_passes` to at most one after every
///     frame, which is vacuous if nothing increments it — this is the
///     canary: a frame with a layer body on screen counts exactly one pass,
///     and a frame without one counts none.
#[test]
fn the_control_pass_counter_counts_the_layer_body() {
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    assert_eq!(
        h.gui_mut().control_render_passes_for_test(),
        0,
        "no layer body is open, so no pass should have run"
    );

    h.open_layer_in_inspector(OverlayKind::NwsAlerts);
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

/// 92. **The auto-poll chip's off state reads `⏸ Auto-poll off`, and its
///     hover names the way back.** (Plan §5.9's carried pin.)
///
///     The chip replaced the checkbox at the full-bleed flip, so the off
///     position — which the checkbox used to show by itself — has to stay
///     readable off the chip, and the hover has to say where the toggle
///     went, or the user who turned polling off has nothing to find it by.
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

    // The hover: where the toggle went.
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

/// 93. **A shrunk window does not cap the stack forever.** (Plan §5.9's
///     carried finding.)
///
///     `Area::default_size` applies only while the stored `AreaState` size
///     is `None`, so after frame 1 the committed size becomes the sizing
///     ceiling — and a `ScrollArea` fills what it is offered, so the old
///     panel came back from a shrink stuck at its smallest-ever height.
///     The stack sizes its body from the map every frame instead; this
///     drives the shrink-then-grow and requires the height back.
#[test]
fn the_stack_regains_its_height_after_a_shrink_and_regrow() {
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    let before = h.stack().rect.height();
    assert!(
        before > 300.0,
        "precondition: the full-height stack must be tall, got {before}"
    );

    // Shrink far enough that the body's per-frame ceiling clamps it…
    h.set_screen(egui::vec2(1400.0, 300.0));
    let shrunk = h.stack().rect.height();
    assert!(
        shrunk < before / 2.0,
        "precondition: the shrink must really clamp the stack, got {shrunk}"
    );

    // …and back. The old panel stayed at `shrunk` forever.
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
///
///     `set_safe_area_insets` -> `LayoutCtx::resolve` -> the root `Ui`'s
///     rect, which is what insets every nested `Panel`. That last hop was
///     untested: dropping `.max_rect(..)` leaves the chrome under the
///     status bar, and nothing in the suite ever set an inset.
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

    // The map is what is left after the panels claim their space, so it
    // moves by exactly what the insets took off each edge.
    assert_eq!(inset.left() - bare.left(), LEFT, "left inset ignored");
    assert_eq!(bare.right() - inset.right(), RIGHT, "right inset ignored");
    assert_eq!(inset.top() - bare.top(), TOP, "top inset ignored");
    assert_eq!(
        bare.bottom() - inset.bottom(),
        BOTTOM,
        "bottom inset ignored"
    );

    // The top bar is laid out inside `content_rect` too, so on a phone it
    // must sit below the notch rather than under the system bars.
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
///
///     Through `Gui::ui` rather than only on `shrink_to_content`, which is
///     what proves `Gui::safe_area_insets` is threaded into the resolve.
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

/// 25. **The hover readout follows the pointer, not the window width.**
///
///     Keying it on `WidthClass` gets both ends wrong: a 500pt desktop
///     window loses a readout it can use, a 1400pt tablet gets an empty one.
///
///     Since the phone shell the *host* follows the width — Compact has no
///     status bar, so the readout lives in the phone top bar there — but
///     whether a readout exists at all stays the modality's question alone.
#[test]
fn the_hover_readout_follows_the_modality_not_the_width() {
    // A narrow *desktop* window: compact, but there is a mouse — the phone
    // top bar hosts the readout.
    let mut narrow = InputHarness::with_screen(egui::vec2(500.0, 800.0));
    narrow.mouse_click(narrow.map_center());
    assert_eq!(narrow.width_class(), crate::ui_layout::WidthClass::Compact);
    assert!(
        narrow.top_bar().hover,
        "a compact window with a mouse lost its hover readout"
    );

    // A wide *touch* device: roomy, but nothing can hover.
    let mut tablet = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    tablet.touch_tap(tablet.map_center());
    assert_eq!(tablet.width_class(), crate::ui_layout::WidthClass::Expanded);
    assert!(
        !tablet.status_bar().hover && !tablet.top_bar().hover,
        "a touch device was given a hover readout that can never fill in"
    );

    // ...and a *touch* phone gets none either: the phone bar hosts the
    // readout for a mouse, not for the width.
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

/// 26. **The phone top bar carries the short scan text; the long form stays
///     on the desktop status bar — and the Auto-poll toggle stays reachable
///     through the menu everywhere.**
///
///     The compact status bar's successor claim: the phone shell draws no
///     status bar at all, so the short scan summary the compact bar used to
///     carry lives in the phone top bar's chip — site, time, posture glyph —
///     while the long form (date, product count, poll chip) stays where the
///     room is. Asserted on the text drawn, not the flag.
///
///     Since the full-bleed flip the auto-poll *checkbox* is a display
///     chip; the toggle itself lives in the menu. The menu assertion is
///     what keeps that a move rather than a removal — through the sheet's
///     Menu page on the phone, the ☰ dropdown on the desktop.
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

    // The chip is display-only; the toggle lives in the menu — reachable at
    // every width, so the phone shell dropping the bar strands nothing.
    for h in [&mut phone, &mut desk] {
        h.open_menu();
        assert_eq!(
            h.menu_leaf("Auto-poll").map(|l| l.value),
            Some(Some(true)),
            "the menu must carry the Auto-poll toggle, checked while on"
        );
        h.close_menu();
    }

    // Both forms name the site, so the difference is the *detail*: only the
    // long form carries the date and the product count.
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
///
/// Offset by half a minute so the whole-minute truncation in
/// `format_product_age` cannot land on a boundary and read one lower while
/// the test runs.
fn written_ago(minutes: i64) -> chrono::NaiveDateTime {
    chrono::Utc::now().naive_utc() - chrono::Duration::seconds(minutes * 60 + 30)
}

/// 26b. **Every product says how old the data behind it is, the same way.**
///
///      The scan line beside this is the *volume* time and answers a
///      different question. For a product fetched from the Level III bucket
///      the two can be days apart — `level3::latest_key` falls back to the
///      previous UTC day, so a site that went down yesterday paints a field
///      up to ~48h old under a scan line that looks perfectly current.
///
///      The line used to read `Level III:` and be drawn *only* for the
///      bucket-fetched products, which made the datasource readable off the
///      status bar and made its absence informative too. It is now one
///      uniform line: same label, same format, drawn whenever the pane knows
///      when its data was collected, whatever produced it.
///
///      Asserted on the text egui laid out inside the bar's own rect, not
///      just on the probe: a probe records what the renderer was handed,
///      and the thing that matters is what reached the glass.
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
///      replaced.**
///
///      The bar used to draw nothing at all while a loop ran, because
///      `data_time` describes the static render the animation stands in
///      for — captioning someone else's picture. The frame's own volume time
///      is the right answer, and it is the same answer whichever datasource
///      the loop reads, since a loop frame *is* a volume.
#[test]
fn a_looping_pane_reports_its_current_frames_time() {
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    h.load_scan("KTLX");
    // A time the static render would never report, so the two are telling.
    h.set_data_time(0, Some(written_ago(90)));

    let frame_time = written_ago(7);
    {
        let pane = h.gui_mut().pane_mut(0).unwrap();
        pane.loop_state = crate::pane::LoopPlaybackState::new_for_loop(
            600,
            rustdar_radar::sites::get_radar_site("KTLX").unwrap(),
        );
        pane.loop_state.frames = vec![crate::pane::LoopFrame {
            timestamp: frame_time,
            texture: None,
            render_in_flight: false,
            render_failed: false,
        }];
        pane.loop_state.current_frame = 0;
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

// ── The floating timeline transport ──────────────────────────────────

/// 66. **Collapsing the transport leaves a restore chip at the map's
///     bottom-right, and the chip restores it.**
///
///     Collapse that merely hid the widgets would leave their rects
///     recorded and their clicks landing; collapse that left no chip
///     would be a transport with no way back. Both halves are driven the
///     user's way, through the drawn rects.
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

    // The chip: inside the map, hugging its bottom-right corner.
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

    // Row 1 is really gone — no rects to click, no text on the glass.
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

    // And the chip is the way back.
    h.mouse_click(collapsed.chip.center());
    h.warm_up();
    let restored = h.timeline();
    assert!(
        !restored.collapsed && restored.live.0.is_positive(),
        "clicking the chip did not restore the transport"
    );
}

/// 66b. **The status bar's ◧ collapses it to a restore button, left-
///      anchored, and the same button brings it back.**
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

/// **The timestamp chip opens the Set Time dialog** — the timeline's own
/// route to it; the menu's Time... entry is the other, and the dialog
/// itself is unchanged.
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

/// **Back steps into the archive; forward is dead while live** — the
/// navigation semantics that moved from the layers panel, driven through
/// the timeline's drawn rects for the first time.
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

    // Back: one default step (10 min) into the archive, dropping live.
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
///
///     Pane rects run under the timeline and the status bar since the
///     full-bleed flip, so "the pointer is over this pane" no longer
///     implies "the pointer is over the map". Two readers could fall for
///     it: walkers' own gate for the 2D map, and the 3D pane's globally
///     read `zoom_delta` — the second is the one `volume_pane_outcome`'s
///     topmost-layer check exists for. Each half has a control scroll on
///     open map first, so a pass cannot come from zooming being broken
///     altogether.
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
    let eye = |h: &mut InputHarness| {
        h.gui_mut()
            .pane(1)
            .expect("pane 1 exists")
            .volume()
            .expect("pane 1 is a volume")
            .camera
            .eye_distance()
    };

    // Control: a scroll on the open volume pane moves its camera.
    let clear_volume = egui::pos2(panes[1].center().x, panes[1].center().y);
    assert!(
        !h.is_floating_layer_at(clear_volume),
        "precondition: the control point must be open map"
    );
    let before = eye(&mut h);
    h.scroll_at(clear_volume, egui::vec2(0.0, 200.0));
    h.frames_for(2, FRAME_DT);
    assert!(
        eye(&mut h) < before,
        "control: a scroll on the open volume pane must zoom it"
    );

    // Over the timeline, above the same volume pane: nothing.
    let covered = egui::pos2(
        (panes[1].left() + 40.0).max(timeline.left() + 8.0),
        timeline.center().y,
    );
    assert!(
        timeline.contains(covered) && panes[1].contains(covered),
        "precondition: the point is on the timeline over the volume pane"
    );
    let before = eye(&mut h);
    h.scroll_at(covered, egui::vec2(0.0, 200.0));
    h.frames_for(2, FRAME_DT);
    assert_eq!(
        eye(&mut h),
        before,
        "a wheel over the timeline flew the 3D camera under it"
    );

    // Control: a scroll on the open map pane zooms the map.
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

    // Over the status bar, above the same map pane: nothing.
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
    // ...and over the timeline too.
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
///
///     With no loop running, the scrubber spans the lookback window and
///     commits once, on `drag_stopped`: a release inside the rail emits
///     `NavigateTime` to the released moment and clears `viewing_live`.
///     Nothing may fire mid-drag — every intermediate position would be a
///     volume fetch nobody asked to wait for.
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

/// **Scrubbing to the right end restores live** (plan §3.7).
#[test]
fn scrubbing_to_the_right_end_restores_live() {
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    h.load_scan("KTLX");
    // Park the pane in the archive first, so JumpToLive has something to do.
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
///
///      Only Level III products can be in this state: `ScanInfo::from_scan`
///      lists them the moment a volume loads and fills their angle in when the
///      fetch lands, and every archive poll rebuilds `ScanInfo` from the volume
///      alone, so the window reopens on every poll. Skipping the combo box
///      while the list was empty made the control *vanish* and the layers panel
///      reflow around it — visible on first selection and then flickering once
///      a minute, which is exactly the kind of thing that tells a user this
///      product is not like the others.
#[test]
fn a_product_whose_tilts_have_not_arrived_keeps_its_tilt_picker() {
    use rustdar_radar::types::RadarProduct;

    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    h.load_scan("KTLX");
    // The pickers live in the inspector's Pane-properties body now.
    h.open_pane_props();
    {
        let pane = h.gui_mut().pane_mut(0).unwrap();
        pane.set_overlay_enabled(OverlayKind::Radar, true);
        let info = pane.scan_info.as_mut().expect("a scan was loaded");
        info.product_elevations
            .insert(RadarProduct::Reflectivity, vec![0.5, 1.5]);
        // As `from_scan` lists a Level III product: available, no angles yet.
        info.available_products.push(RadarProduct::EchoTops);
        info.product_elevations
            .insert(RadarProduct::EchoTops, Vec::new());
        pane.selected_product = RadarProduct::EchoTops;
        pane.selected_elevation = 0.0;
    }
    h.warm_up();

    assert!(
        h.painted_text_strings().iter().any(|t| t == "0.0\u{b0}"),
        "the tilt picker vanished for a product whose angles have not landed; \
             painted: {:?}",
        h.painted_text_strings(),
    );
    // And the render path agrees: the selection stands, so a render is
    // dispatched rather than the previous product's image being held.
    assert_eq!(
        h.gui_mut().pane(0).unwrap().get_rendering_params(),
        Some((RadarProduct::EchoTops, 0.0)),
    );

    // The populated case still lists its angles, so the assertion above is
    // about a *present but empty* picker rather than about a combo box that
    // never shows anything.
    h.gui_mut().pane_mut(0).unwrap().selected_product = RadarProduct::Reflectivity;
    h.warm_up();
    assert!(
        h.painted_text_strings().iter().any(|t| t == "0.5\u{b0}"),
        "painted: {:?}",
        h.painted_text_strings(),
    );
}

/// A harness with one pane on KTLX offering a Level II and a Level III
/// product at 0.5°, radar layer on, showing a finished `showing` image.
///
/// Both products at the same angle deliberately: it makes the product the only
/// thing that differs between the two selections, so a notice that appeared
/// would be about the product switch and nothing else.
fn pane_showing(showing: rustdar_radar::types::RadarProduct) -> InputHarness {
    use rustdar_radar::types::RadarProduct;

    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    h.load_scan("KTLX");
    h.gui_mut()
        .pane_mut(0)
        .unwrap()
        .set_overlay_enabled(OverlayKind::Radar, true);
    h.offer_product(0, RadarProduct::Reflectivity, 0.5);
    h.offer_product(0, RadarProduct::EchoTops, 0.5);
    h.select_product(0, showing);
    h.place_radar_image(0, showing, 0.5);
    h
}

/// The pending-render notice for `product`, as the pane paints it.
///
/// Read out of the *painted* text rather than from `stale_image_on_screen`, so
/// what is asserted is what a user would be looking at. Matched on the product
/// name the notice names — the one on screen — because that is the whole
/// message.
fn notice_painted(h: &InputHarness, product: rustdar_radar::types::RadarProduct) -> bool {
    h.painted_text_strings()
        .iter()
        .any(|t| t.starts_with('\u{27f3}') && t.contains(product.name()))
}

/// Any pending-render notice at all, whatever it names.
fn any_notice_painted(h: &InputHarness) -> bool {
    h.painted_text_strings()
        .iter()
        .any(|t| t.starts_with('\u{27f3}'))
}

/// 26f. **A pane says when its image is not the product it is labelled with.**
///
///      Switching product holds the previous product's image until the new
///      render lands, while the color scale, the tilt picker and the status
///      bar's data line have all already moved to the new selection. That is a
///      label claiming something the pixels do not show — a small correctness
///      problem, not a cosmetic one — and it lasts as long as a render, longer
///      for a Level III product whose object has not landed.
///
///      The notice names what is *on screen*, since everything else on the
///      pane already names the selection. The imagery stays up and undimmed
///      behind it: one product's echoes are better than none.
#[test]
fn a_pane_says_when_its_image_is_not_the_selected_product() {
    use rustdar_radar::types::RadarProduct;

    let mut h = pane_showing(RadarProduct::Reflectivity);
    assert!(
        !any_notice_painted(&h),
        "a pane showing what it selected has nothing to disown; painted: {:?}",
        h.painted_text_strings(),
    );

    // The switch, with no render landed yet.
    h.select_product(0, RadarProduct::EchoTops);
    assert!(
        notice_painted(&h, RadarProduct::Reflectivity),
        "the pane is showing reflectivity and labelled echo tops, and said \
             nothing; painted: {:?}",
        h.painted_text_strings(),
    );
    // Over the pane it is about, not somewhere in the chrome: in a split
    // layout each pane answers for its own image.
    let pane_rect = h.pane_rects()[0];
    assert!(
        h.text_painted_in(pane_rect, "showing Reflectivity"),
        "the notice was painted outside the pane it describes; painted: {:?}",
        h.painted_text_strings(),
    );
    // …and the picture is still there. The notice is drawn over the imagery,
    // never instead of it.
    assert!(
        h.gui_mut()
            .pane(0)
            .unwrap()
            .overlay_cache(OverlayKind::Radar)
            .and_then(|c| c.current.as_ref())
            .is_some(),
        "the pane was cleared rather than annotated",
    );

    // The render lands and the notice goes.
    h.place_radar_image(0, RadarProduct::EchoTops, 0.5);
    assert!(
        !any_notice_painted(&h),
        "the notice outlived the render it was waiting for; painted: {:?}",
        h.painted_text_strings(),
    );
}

/// 26g. **…and it does not flash on a routine refresh.**
///
///      A new volume for the site drops every pane's `last_rendered` and
///      re-renders it, several times a scan under the real-time feed. The image
///      on screen still depicts the selected product throughout, so there is
///      nothing to disown — which is why the notice is derived from the
///      *image's* own metadata rather than from "is a render in flight".
#[test]
fn a_same_selection_re_render_draws_no_notice() {
    use rustdar_radar::types::RadarProduct;

    let mut h = pane_showing(RadarProduct::Reflectivity);
    // Two more volumes' worth of the same selection re-rendered, as an
    // auto-poll or the chunk feed produces.
    for _ in 0..2 {
        h.place_radar_image(0, RadarProduct::Reflectivity, 0.5);
        assert!(
            !any_notice_painted(&h),
            "a routine re-render of the selected product drew a notice; \
                 painted: {:?}",
            h.painted_text_strings(),
        );
    }

    // Nor does a selection the scan snaps back onto the sweep already drawn:
    // 0.6° snaps to the 0.5° sweep, which is the image on screen.
    h.gui_mut().pane_mut(0).unwrap().selected_elevation = 0.6;
    h.warm_up();
    assert_eq!(
        h.gui_mut().pane(0).unwrap().get_rendering_params(),
        Some((RadarProduct::Reflectivity, 0.5)),
        "precondition: the selection snaps to the drawn sweep",
    );
    assert!(
        !any_notice_painted(&h),
        "the snapped selection is the image on screen; painted: {:?}",
        h.painted_text_strings(),
    );
}

/// 26h. **The notice is the same for a Level II and a Level III product.**
///
///      The point of the parity work is that a user cannot tell the two
///      datasources apart, so a notice that appeared for only one of them — or
///      worded itself differently — would be a way to read the datasource off
///      the screen, exactly the tell the uniform data line removed. Driven both
///      ways round through the real UI so the claim is about what is painted
///      rather than about a shared code path.
#[test]
fn the_pending_notice_is_identical_for_both_datasources() {
    use rustdar_radar::types::RadarProduct;

    // A Level III selection over a Level II image, and the reverse.
    let (l2, l3) = (RadarProduct::Reflectivity, RadarProduct::EchoTops);
    assert!(!l2.is_level3() && l3.is_level3(), "one of each datasource");

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

    // The wording is one format string with the product substituted, so the
    // two differ in the product name and nowhere else.
    let strip = |t: &str, name: &str| t.replace(name, "<product>");
    assert_eq!(
        strip(&notice_of(&awaiting_l3), l2.name()),
        strip(&notice_of(&awaiting_l2), l3.name()),
        "the two datasources drew differently shaped notices, which is a way \
             to tell them apart",
    );

    // And each one clears on its own render landing, the same way.
    awaiting_l3.place_radar_image(0, l3, 0.5);
    awaiting_l2.place_radar_image(0, l2, 0.5);
    assert!(!any_notice_painted(&awaiting_l3));
    assert!(!any_notice_painted(&awaiting_l2));
}

/// 26i. **A pane with no image says nothing, and neither does a looping one.**
///
///      Two states with nothing to report, for opposite reasons. An empty pane
///      makes no claim its pixels contradict — there are none, and the site
///      spinner already covers a first load. A looping pane never *holds* a
///      stale frame: `retarget_renders` drops every frame texture the instant
///      the selection moves, so the animation is blank rather than wrong, and
///      the loop's own phase chrome covers the wait.
#[test]
fn nothing_is_said_where_there_is_no_stale_image() {
    use rustdar_radar::types::RadarProduct;

    // No image at all, on a selection nothing has rendered.
    let mut bare = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    bare.load_scan("KTLX");
    bare.gui_mut()
        .pane_mut(0)
        .unwrap()
        .set_overlay_enabled(OverlayKind::Radar, true);
    bare.offer_product(0, RadarProduct::EchoTops, 0.5);
    bare.select_product(0, RadarProduct::EchoTops);
    assert!(
        !any_notice_painted(&bare),
        "an empty pane has no pixels to disown; painted: {:?}",
        bare.painted_text_strings(),
    );

    // A looping pane, mid-switch. The static image's metadata still describes
    // the old product, and must not be reported: it is not what is on screen.
    let mut looping = pane_showing(RadarProduct::Reflectivity);
    let site = rustdar_radar::sites::get_radar_site("KTLX").expect("a real radar");
    {
        let pane = looping.gui_mut().pane_mut(0).unwrap();
        pane.loop_state = crate::pane::LoopPlaybackState::new_for_loop(600, site);
    }
    looping.select_product(0, RadarProduct::EchoTops);
    assert!(
        looping.gui_mut().pane(0).unwrap().loop_state.is_active(),
        "precondition: the loop is running",
    );
    assert!(
        !any_notice_painted(&looping),
        "a looping pane drew the static image's notice; painted: {:?}",
        looping.painted_text_strings(),
    );
}

/// 26c. **…and day-old data reads as hours, not as 1,560 minutes.**
///
///      This is the case the line exists for. `level3::latest_key` falls
///      back to the previous UTC day when today's prefix is empty, so the
///      product a downed site serves is not a few minutes stale, it is
///      most of a day — and a bar that only ever counted minutes would say
///      so in a unit nobody reads at a glance.
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

    // The phone has no status bar to carry the line — the timeline's age
    // chip is where the age lives down there, and it must read on the same
    // hour scale, or a downed site's day-old field looks minutes stale on
    // exactly the screen most likely to be glanced at.
    let mut phone = InputHarness::with_screen(egui::vec2(420.0, 900.0));
    phone.load_scan("KTLX");
    phone.set_data_time(0, Some(written_ago(26 * 60 + 5)));
    assert!(
        phone.status_bar().rect == egui::Rect::NOTHING,
        "the phone shell drew a status bar"
    );
    let age = phone.timeline().age_text;
    assert_eq!(
        age, "26h 5m old",
        "the phone timeline's age chip must carry the age in hours"
    );
    assert!(
        !age.contains("L3") && !age.contains("Level III"),
        "nor may the chip name a datasource: {age:?}"
    );
}

// 16 retired (synthesis-m1): `excluded_rects` is unconditionally empty
// since the hamburger went, so "a wide screen excludes nothing" held for
// every screen and pinned nothing. M5's pill-row contract (73) replaced
// rect exclusion with layer-based assertions — see
// `a_pill_click_never_reaches_the_map_and_its_popover_anchors`.

// ── Overlay texture budget ───────────────────────────────────────────

use crate::actions::GuiAction;
use rustdar_overlays::render::overlay_state::OverlayKind;

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
/// `RenderOverlay`. `RadarSites` is the one overlay whose `has_data` is
/// unconditionally true, so it needs no fetch to reach the render path.
fn harness_requesting_overlays() -> InputHarness {
    let mut h = InputHarness::new();
    h.gui_mut().enable_overlay_for_test(OverlayKind::RadarSites);
    h.warm_up();
    h
}

/// The whole point of the change, exercised through the real UI: the number the
/// adapter reports reaches `plan_overlay_texture` via `RawInput` and bounds what
/// the pane asks for. Forcing the limit is how a WebGL2-class device is tested
/// without a wasm target.
#[test]
fn a_small_adapter_limit_bounds_what_the_pane_requests() {
    // The smallest limit egui itself tolerates: `TextureAtlas::new` asserts
    // `size[0] >= 1024`, so a WebGL2 2048 cannot be halved further here. Still
    // well under what this pane asks for unclamped.
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
    let default_limit = requested_plans(&h);

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
    // egui's own default is 2048, which this pane already exceeds — so the two
    // sets differ, which is what makes the assertion above about the limit
    // rather than about the pane being small.
    assert_ne!(
        default_limit, desktop,
        "precondition: egui's 2048 default must clamp this pane, or this test \
             proves nothing about the limit being read at all"
    );
}

// ── Radar site icons: from the click to the action ───────────────────

/// Off-centre, but well inside a 24pt icon. A click at the exact centre
/// still lands inside a zero-sized `Rect`, so a hit-test collapsed to
/// nothing would pass there.
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

/// A harness showing `site`, with the radar-site overlay on, plus the
/// screen position that site's icon is drawn at.
///
/// `render_panes` centres a pane on its own scan's site, so the icon lands on
/// the pane centre. That comes from the layout, not from the hit-testing
/// under test, so shrinking or inflating the icon cannot move it.
fn harness_showing_site(site: &str) -> (InputHarness, egui::Pos2) {
    let mut h = InputHarness::new();
    h.load_scan(site);
    h.gui_mut().enable_overlay_for_test(OverlayKind::RadarSites);
    h.warm_up();
    assert!(
        h.overlay_enabled(OverlayKind::RadarSites),
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
///
///     The click resolves — that is what the whole pointer suite pins — but
///     nothing checked that the action it is supposed to produce ever
///     reached the app. Two things were eating it, and either alone is
///     enough to make every site unselectable on every platform:
///     `handle_radar_site_interactions` was handed the excluded rects with
///     *the site icons already in them*, and the readout it opens on hover
///     was an interactable layer sitting over the pointer, which the dialog
///     gate then read as a click on a floating window.
#[test]
fn clicking_a_radar_site_icon_switches_to_that_site() {
    let (mut h, icon) = harness_showing_site("KTLX");
    let target = icon + INSIDE_THE_ICON;

    // Rest on the icon first, as a mouse user does. That opens the site
    // readout, and a readout that takes part in layer hit-testing then eats
    // the click it was opened by — the pointer is inside it.
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
///
///     The same handler under the other modality, which reaches it by a
///     different route: the touch pipeline confirms the tap only after
///     `DOUBLE_TAP_TIMEOUT_S`, a frame on which nothing is pressed at all.
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
///
///     The complement: without it an icon stretched over the pane satisfies
///     the two above while turning every map click into a site switch.
///     40pt out is comfortably clear of a 24pt icon and still far nearer
///     than the next radar, which at this zoom is hundreds of points away.
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
///
///      `is_pos_blocked`'s three conditions mask each other everywhere they
///      normally meet, so each has to be reached alone. This is the
///      pane-rect one: a site icon straddling the pane's **top** edge, and
///      two clicks 10pt apart — one on the map, one in the top bar. The
///      icon's own hit-test cannot tell them apart, and neither can the
///      other two conditions (nothing is excluded on a wide screen, and a
///      panel is a background layer), so only the pane rect stands between
///      a click on the chrome and a radar site change. The top edge
///      because the top bar is the one docked chrome left: since the
///      full-bleed flip the pane's other three edges are the screen's, and
///      the chrome along the bottom is a floating layer — which would
///      reach the *layer* condition, not this one.
///
///      `screen_rect.expand(100.0)` in `render_pane_map_content` is what
///      makes this reachable at all: sites just off the pane are still
///      drawn and still hit-tested, so "off the pane" is not implied by
///      "not drawn".
#[test]
fn a_click_outside_the_pane_does_not_reach_a_site_icon_straddling_its_edge() {
    let mut h = InputHarness::new();
    h.gui_mut().enable_overlay_for_test(OverlayKind::RadarSites);
    h.warm_up();

    // Off the pane's centre-line, so the blocked click lands on the top
    // bar's empty stretch rather than on one of its widgets — a click
    // that *did* something would fail this test for an unrelated reason.
    let pane = h.pane_rects()[0];
    let edge = egui::pos2(pane.center().x + 150.0, pane.top());
    h.place_site_at(0, "KTLX", edge);

    // 5pt either side of the edge: both well inside an 18pt icon, so the
    // icon hit-test says yes to both.
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
    // `contains`, not equality: at this zoom the three Oklahoma City
    // radars sit inside one icon of each other, and which of them also
    // answers says nothing about the condition under test.
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
///
///      The layer condition, reached alone — and reachable only through
///      *hover*: a click is already stripped upstream by
///      `ui_input::filter_dialog_blocked`, so with the layer check deleted
///      from `is_pos_blocked` every click test still passes. Hover has no
///      such pre-filter; `pointer_hover_pos()` is read raw.
///
///      Asserted on the readout that was painted, not on a flag: the site
///      readout is the thing a user sees follow the cursor through a
///      dialog, and it is the same `Area` that once claimed `layer_id_at`
///      and ate the click behind it.
#[test]
fn a_dialog_over_a_site_icon_suppresses_its_hover_readout() {
    let mut h = InputHarness::new();
    h.gui_mut().enable_overlay_for_test(OverlayKind::RadarSites);
    h.warm_up();
    // Put the icon where a modal dialog lands, so one hover position
    // serves both halves.
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
///
///     `ControlItem::Dropdown` carries `(value, display_label)` pairs. The
///     open list has always shown the label; `selected_text` showed the raw
///     *value*, so Model Data's Parameter box read `sbcin` and GLM's
///     Satellite box read `both` until you opened them.
///
///     Asserted against the label the handler itself offers for the
///     selected value, not against a string this test spells out: a
///     renderer that started formatting labels its own way would still have
///     to agree with the list beside it. The painted-text check is the
///     other end — it is the text egui actually laid out inside the combo's
///     rect, so a probe reporting a value the widget never received fails.
#[test]
fn a_dropdown_shows_its_option_label_not_the_raw_value() {
    // Per handler, through the inspector's layer body — the one place a
    // handler's dropdowns render since the stack/inspector split.
    for host in [OverlayKind::ModelData, OverlayKind::Lightning] {
        let mut h = compact_with_layers_drawer();
        h.set_overlay_on_pane(0, host, true);
        h.open_layer_in_inspector(host);

        // Every dropdown on screen — the layer body's, by construction.
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

        // …and the list the box opens is the other half of the claim. Assert
        // on the labels it *paints*, so that "both halves use one formatter"
        // is checked against two rendered results rather than one rendered
        // result and one reading of the model.
        for dropdown in &drawn {
            let mut h = compact_with_layers_drawer();
            h.set_overlay_on_pane(0, host, true);
            h.open_layer_in_inspector(host);
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
            // The list scrolls, so only the options that fit are laid out —
            // hence "the ones on screen are labels", not "all of them are".
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
///
///     egui compares each pass's widget rects against the last and warns
///     when the same rect comes back under a different `Id` — the same
///     hazard [`crossing_a_breakpoint_does_not_move_any_widget_id`] guards
///     across a resize, caught by egui itself rather than by a probe list.
///
///     The status bar used to allocate its right-aligned error slot every
///     frame whether or not there was an error. Empty, that scope is a
///     zero-area rect welded to the row's right edge, while its id moves
///     with the widget count before it — and the auto-poll block draws
///     three widgets mid-fetch and one after. So the frame the first scan
///     landed re-keyed a widget, twice per launch.
///
///     This reads the verdict off egui's per-pass widget bookkeeping, so
///     it fires for any widget that acquires the same hazard, not just
///     this one.
#[test]
fn a_scan_arriving_moves_no_widget_id() {
    // Roughly a Fold 7 inner screen: wide enough for the long status bar,
    // too narrow for the sidebar, which is where this was seen.
    let mut h = InputHarness::with_screen(egui::vec2(750.0, 900.0));
    h.gui_mut().set_fetching(true);
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
///
///      The strengthening the top bar bought. The menu-bar panel used to
///      appear and vanish at 600pt, advancing the root `Ui`'s auto-id
///      counter one step more or less before the status bar — and
///      `Ui::new_child` folds that counter into every child scope's
///      `unique_id`, `id_salt` moving only the *stable* id, so the whole
///      status bar came back under new ids on the far side. The old form
///      of this test could only hold the *extent* of that shift. With the
///      top bar drawn at every width nothing above the status bar is
///      conditional, so the claim is now total: egui's own per-pass widget
///      bookkeeping must see no rect come back under a different id.
#[test]
fn crossing_a_breakpoint_re_keys_nothing() {
    // Either side of 600pt, and narrow enough to have no sidebar on
    // either, so the layers panel is the drawer both times. Short on
    // purpose: the drawer's body must overflow its slot, or the scrolled
    // offset this test carries across the breakpoint would be zero and the
    // claim empty (the M8 full-row rework left the rows tighter than the
    // old two-button stack, so a 600pt-tall drawer no longer scrolls).
    let mut h = InputHarness::with_screen(egui::vec2(750.0, 480.0));
    h.set_drawer_open(true);
    // The inspector joins the crossing too (M3 review): its ids are part of
    // "nothing", and egui's bookkeeping only sees what is on screen.
    h.gui_mut().open_settings();
    h.load_scan("KTLX");
    assert_eq!(
        h.width_class(),
        crate::ui_layout::WidthClass::Medium,
        "precondition: start above the 600pt breakpoint"
    );

    // Real stored state behind a real widget id, so "nothing was lost" is
    // a claim about something rather than about an empty set.
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

    // ...and the ids that key stored state are the same ids. Below 600pt
    // the panels are sheet pages, one at a time — the settings page is on
    // top — so the comparison is per id on screen rather than list-equal.
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
    // The stack's page beneath keeps its id and the state stored under it.
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

/// 34c. **An error on screen keeps its id while the row moves around it.**
///
///      [`a_scan_arriving_moves_no_widget_id`] fixed and pinned the *empty*
///      half of this hazard — a slot allocated with nothing in it. The
///      occupied half survived, and nothing in the suite ever put an error
///      on screen to see it: the slot is right-aligned, so when there
///      really is an error its rect is welded to the row's edge while
///      everything to its left comes and goes, and the slot plus all three
///      widgets inside it come back under new ids. Two things move it — the
///      auto-poll block when a scan lands, and the data age line when a
///      render does — so both are driven here.
#[test]
fn an_error_on_screen_keeps_its_id_while_the_row_changes_around_it() {
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    h.gui_mut().set_error("boom".to_owned());
    // `set_error` ends the fetch, so put it back: the transition under test
    // is the auto-poll spinner (three widgets) becoming the chip (one).
    h.gui_mut().set_fetching(true);
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
    h.load_scan("KTLX");
    assert!(
        h.status_bar().poll_chip.is_some(),
        "precondition: the scan must have cleared the fetch, or nothing to \
             the left of the error changed"
    );
    assert!(
        h.painted_text_strings().iter().any(|t| t == "boom"),
        "precondition: the error must still be on screen after the scan"
    );
    assert_eq!(
        h.id_changes(),
        &[] as &[egui::Rect],
        "a scan arriving re-keyed the error slot: its rect is pinned to the \
             row's right edge while its id follows the widget count to its left"
    );

    // …and the same slot, moved by the other neighbour.
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
///
///     [`a_scan_arriving_moves_no_widget_id`] asserts on an empty list, so
///     a reader that matched nothing would pass it forever — which is
///     exactly what happened when the reader was egui's painted debug
///     marker: that marker is compiled out of release builds, and only
///     this test noticed. This drives egui with a deliberately unstable id
///     at a fixed rect and requires the same reader the harness uses to
///     report it, in whichever profile is running.
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
        // Same rect, new id, same parent: exactly what egui warns about.
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
///
///     `PaneState::with_site` starts with empty overlay maps, and
///     `is_overlay_enabled` reads a missing entry as *off* — so a pane the
///     picker adds would draw no overlays at all, Radar included. Layer
///     sync masks this by copying the active pane over the newcomer every
///     frame the layers panel is up, so it runs with sync off.
#[test]
fn a_pane_added_by_the_picker_still_shows_radar_with_layer_sync_off() {
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    assert_eq!(
        h.width_class(),
        crate::ui_layout::WidthClass::Expanded,
        "precondition: an arbitrary width — the picker is in the top bar \
         at all of them"
    );
    h.set_sync_layers(false);
    assert!(
        h.overlay_enabled(OverlayKind::Radar),
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
        !h.sync_layers(),
        "precondition: sync must still be off, or it did the seeding"
    );

    assert!(
        h.overlay_enabled_on(1, OverlayKind::Radar),
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
///     `radar.config.site` is a *global* last-switched site — the
///     frontend's `SwitchRadarSite` writes it whichever pane switched,
///     sync on or off — so with per-pane sites a Refresh that clones the
///     config verbatim can fetch a site the active pane never showed.
///     Both entry points, driven the way a user reaches them: the status
///     bar's button and the ☰ dropdown's item.
#[test]
fn refresh_fetches_the_active_panes_site_not_the_global_one() {
    let mut h = InputHarness::with_screen(egui::vec2(800.0, 900.0));
    assert_eq!(
        h.width_class(),
        crate::ui_layout::WidthClass::Medium,
        "precondition: an arbitrary width — the dropdown is the same at all \
         of them"
    );

    // Some other pane switched sites last: the global config points away
    // from what the active pane is viewing.
    let mut config = h.gui_mut().get_radar_config().clone();
    config.site = "KDMX".to_owned();
    h.gui_mut().set_radar_config(config);
    h.warm_up();
    assert_eq!(
        h.gui_mut().active_pane().site,
        "KTLX",
        "precondition: the active pane and the global config must disagree"
    );

    h.mouse_click(h.status_bar().refresh.center());
    assert_eq!(
        fetched_site(h.last_actions()).as_deref(),
        Some("KTLX"),
        "the status-bar Refresh fetched the global site, not the active pane's"
    );

    h.open_menu();
    h.mouse_click(clickable_leaf(&h, "Refresh Radar").center());
    assert_eq!(
        fetched_site(h.last_actions()).as_deref(),
        Some("KTLX"),
        "the menu's Refresh fetched the global site, not the active pane's"
    );
}

/// 38. **A hover readout dies with the radar that produced it.**
///
///     `pane.hover_value` is written only by the radar arm of
///     `render_pane_map_content` and read by the status bar. Two ways the
///     writer stops running while the reader keeps going: the pane stops
///     drawing radar under the pointer, or the pane is hidden by splitting
///     back down and is not rendered at all. Neither may freeze the last
///     readout on the glass.
#[test]
fn a_hover_readout_does_not_outlive_its_pane_or_its_radar() {
    let mut h = InputHarness::with_screen(egui::vec2(800.0, 900.0));
    h.mouse_move(h.map_center());
    h.warm_up();
    assert!(
        h.status_bar().hover,
        "precondition: mouse modality, or there is no readout to go stale"
    );

    // A readout on a visible pane reaches the glass — the channel every
    // staleness assertion below depends on.
    h.gui_mut().pane_mut(0).unwrap().hover_value = Some("LIVE READOUT".to_owned());
    h.frame();
    assert!(
        h.painted_text_strings().iter().any(|t| t == "LIVE READOUT"),
        "precondition: a visible pane's readout must reach the status bar"
    );

    // ...for exactly as long as the radar arm re-sets it: with no radar
    // image under the pointer, the pane's next render clears it.
    h.frame();
    assert!(
        !h.painted_text_strings().iter().any(|t| t == "LIVE READOUT"),
        "a readout with no radar left behind it froze in the status bar"
    );

    // A pane hidden by splitting back down is never rendered, so nothing
    // can clear it — the status bar must not be reading it at all.
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

// ── Pane kinds ───────────────────────────────────────────────────────

/// Two points either side of a storm near KTLX, as the ends of a drawn
/// line. Any finite pair with two distinct ends would do; these are
/// plausible so a failure message reads like a section someone asked for.
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

/// KTLX's reflectivity ladder on VCP 212, as the sampler resolves it — the
/// chosen sweeps' **median** elevations, not the cut table's round numbers.
///
/// Real-shaped on purpose. A synthetic ladder of round degrees would draw
/// perfectly well against a build whose rungs came from the wrong list; the
/// production failure this pins was measured on exactly these angles, where
/// `ScanInfo`'s 0.1°-rounded, deduped view of the same volume reports 16
/// entries for the sampler's 14 rungs (0.4394 → 0.4 and 0.4779 → 0.5 for
/// one 0.4834° cut; 0.8350 → 0.8 and 0.9229 → 0.9 for one 0.8789° cut).
fn vcp_212_rungs() -> Vec<f64> {
    vec![
        0.4834, 0.8789, 1.3184, 1.8018, 2.4170, 3.1201, 4.0430, 5.0977, 6.4160, 8.0273, 10.0195,
        12.5000, 15.6006, 19.5117,
    ]
}

/// The axes of a complete VCP 212 reflectivity section 100 km long, whose
/// ladder is [`vcp_212_rungs`].
fn vcp_212_axes() -> rustdar_radar::xsect::SectionAxes {
    rustdar_radar::xsect::SectionAxes {
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
///
///     The pointer probe is pushed in `render_panes`' shared preamble,
///     above the kind branch, and it has to stay there. `InputHarness::frame`
///     reads the *active* pane's probe out of that vector and panics when it
///     finds none, so a kind whose arm skipped the push would take down the
///     whole pointer suite — several thousand lines of it — with a message
///     about the pointer pipeline never running, pointing at nothing that
///     changed.
#[test]
fn every_pane_reports_a_pointer_frame_whatever_its_kind() {
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    h.set_pane_count(3);
    let (a, b) = section_ends();
    h.make_pane_cross_section(1, a, b);
    h.make_pane_volume(2);
    assert_eq!(
        h.pane_kinds(),
        vec![PaneKind::Map, PaneKind::CrossSection, PaneKind::Volume],
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

    // The active half, and it needs a *non-map* active pane: the active
    // probe is the one `frame()` demands, so a section pane that skipped
    // the push would only be caught while it is the one being driven.
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
///
///     Site, scan, product, elevation, live-or-parked and viewport are flat
///     fields on `PaneState` precisely so that converting a pane cannot
///     touch them. A user who panned to a storm and picked a tilt has said
///     something; asking for a section of it is not a reason to forget it.
///
///     Run on a single pane deliberately: both `propagate_layer_sync` and
///     `sync_viewports` return early below two panes, so every field below
///     is observed changing (or not) for exactly one reason.
#[test]
fn a_converted_pane_keeps_its_site_and_viewport() {
    let mut h = InputHarness::new();
    h.load_scan("KAMA");
    {
        let pane = h
            .gui_mut()
            .pane_mut(0)
            .expect("a fresh harness has one pane");
        pane.selected_product = rustdar_radar::types::RadarProduct::Velocity;
        pane.selected_elevation = 1.5;
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
            pane.site.clone(),
            pane.scan_info.as_ref().map(|info| info.site.name),
            pane.selected_product.name().to_owned(),
            pane.selected_elevation,
            pane.viewing_live,
            pane.map_memory.zoom(),
            pane.map_memory.detached(),
        )
    }

    let before = looking_at(&mut h);
    assert_eq!(
        h.pane_kinds(),
        vec![PaneKind::Map],
        "precondition: it starts as a map"
    );

    let (a, b) = section_ends();
    h.make_pane_cross_section(0, a, b);

    assert_eq!(
        h.pane_kinds(),
        vec![PaneKind::CrossSection],
        "precondition: the conversion must actually have happened"
    );
    assert_eq!(
        looking_at(&mut h),
        before,
        "converting the pane changed what it is looking at"
    );

    // ...and the line it was aimed with is still the line, several frames
    // later. Geographic ends, so nothing in the UI pass can move them.
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
///
///     What each arm *recorded* rather than what the branch was handed:
///     `panes[i].kind()` is the branch's input, so a test reading it back
///     agrees with an arm that ignored it, and with an arm that read the
///     kind off the `mem::take`n slot (where every pane is a map). Each arm
///     writes its own kind as a literal, and this compares that against the
///     copy that actually reached the glass and the rect it reached it in.
#[test]
fn a_non_map_pane_paints_its_empty_state() {
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    h.set_pane_count(3);
    // Unaimed, because `CROSS_SECTION_EMPTY_STATE` is what a section pane
    // says while it has *no line*. A pane that has been aimed and is
    // waiting for its cut says something else, and this test is about the
    // arm painting into its own rect rather than about which of the two
    // states it is in.
    h.make_pane_unaimed_cross_section(1);
    h.make_pane_volume(2);

    let rects = h.pane_rects();
    assert_eq!(
        h.pane_content_probes()
            .iter()
            .map(|probe| (probe.pane_idx, probe.kind, probe.rect))
            .collect::<Vec<_>>(),
        vec![
            (0, PaneKind::Map, rects[0]),
            (1, PaneKind::CrossSection, rects[1]),
            (2, PaneKind::Volume, rects[2]),
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
///
///     The end-to-end version of
///     `a_pane_kind_request_survives_the_pane_being_held_out_of_the_vector`,
///     driven by a real click through the real presentation. The dispatcher
///     records the conversion rather than writing it, and the deferred
///     applier runs after the pane loop's own `mem::take` window — the
///     obvious `self.panes[self.active_pane].set_kind(..)` shape has been
///     silently discarded from inside such a window before, and this is
///     what keeps the checkbox sticking whatever hosts the menu.
///
///     Asserted all the way to the glass: the pane's kind, the arm that
///     actually drew it, the copy on screen, and the checkbox reading back the
///     new state on the following frame. The last of those is what a user sees
///     first, and it is the one a half-wired conversion would fail.
#[test]
fn converting_the_active_pane_from_the_dropdown_makes_it_a_volume_pane() {
    let mut h = compact_with_menu();
    h.load_scan("KTLX");
    assert_eq!(
        h.pane_kinds(),
        vec![PaneKind::Map],
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
        vec![PaneKind::Volume],
        "the click never reached the pane: the write landed on the pane the \
             layers panel had taken out of the vector"
    );
    assert_eq!(
        h.pane_content_probes()
            .iter()
            .map(|probe| probe.kind)
            .collect::<Vec<_>>(),
        vec![PaneKind::Volume],
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

    // …and back, from the same box. This is the only route out of a non-map
    // pane, so a one-way toggle would be a trap.
    h.mouse_click(clickable_leaf(&h, crate::ui::VOLUME_PANE_LABEL).center());
    h.frames_for(3, FRAME_DT);
    assert_eq!(h.pane_kinds(), vec![PaneKind::Map]);
}

/// 44. **A non-map pane keeps the controls that apply to it and drops the
///     rest.**
///
///     Four claims, each with its own failure:
///
///     * **A product picker.** It used to be gated on the Radar *overlay*
///       toggle, which asks whether the map should draw the radar image over
///       its tiles — a question a pane with no tiles does not have. Left
///       gated, a pane converted while that toggle happened to be off would
///       have no product control at all, absent rather than disabled, for a
///       reason nothing on screen explains.
///     * **Time navigation**, because a section of last hour's volume is a
///       perfectly good thing to ask for. Since the full-bleed flip it
///       lives on the floating timeline, which stays up for every pane
///       kind — asserted through the timeline's own probe.
///     * **No tilt picker.** Both non-map kinds read the whole ladder, which
///       is what `PaneKind::consumes_whole_volume` means, so every entry in
///       the combo would select the same picture.
///     * **No loop, and no overlay tree.** A loop frame *is* a rendered
///       plan-view tilt and nothing now feeds one to a pane like this. The
///       layers panel expressed that by omitting the transport; the
///       timeline expresses it by disabling its loop toggle — pinned here
///       by clicking the toggle and requiring no loop action out of it,
///       the failure a user would actually hit. The overlay tree is still
///       the panel's: every entry in it is a layer drawn over map tiles
///       against a projector this pane does not have.
///
///     The rows are also what makes this the test that notices the stack
///     reading `self.panes[active]` instead of the pane it was handed. That
///     slot holds a `mem::take` placeholder for the whole of the shell's
///     pass and therefore reads as a *map* — so a body drawn from it would
///     keep the rows for a converted pane, and the visible difference is
///     the rows being drawn. The tilt picker would *not* reveal it: that
///     is decided inside `render_radar_controls` from the pane passed down.
///     (The timeline cannot have this bug: it runs outside every take
///     window, which is why it may read the slot directly.)
///
///     The combos are read off the ids the body actually resolved rather than
///     off the model, for the same reason `time_step_sel` is: a test rebuilding
///     the expected id from the same format string could agree with a body
///     that drew neither control.
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

    for kind in [PaneKind::CrossSection, PaneKind::Volume] {
        let mut h = InputHarness::with_screen(egui::vec2(1200.0, 900.0));
        h.load_scan("KTLX");
        h.offer_product(0, rustdar_radar::types::RadarProduct::Reflectivity, 0.5);
        h.open_pane_props();
        assert_eq!(
            combos(&h),
            vec!["product_sel", "elev_sel"],
            "precondition: a map pane with a tilt on offer draws both, so the \
                 absence below is the pane's kind and not a missing scan"
        );
        // The loop toggle answers for a map pane: clicking it emits the
        // enable fan-out. (The harness has no App, so nothing consumes
        // the action and the pane's own state is unchanged.)
        h.mouse_click(h.timeline().loop_toggle.0.center());
        assert!(
            h.last_actions()
                .iter()
                .any(|a| matches!(a, crate::actions::GuiAction::EnableLoop { .. })),
            "precondition: a map pane's loop toggle must enable the loop"
        );
        // A row from the middle of the stack, so the check is not satisfied
        // by the first one alone.
        assert!(
            h.stack_row(OverlayKind::NwsAlerts).is_some(),
            "precondition: a map pane's stack draws the layer rows"
        );

        match kind {
            PaneKind::CrossSection => {
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
        assert!(
            !h.last_actions().iter().any(|a| {
                matches!(
                    a,
                    crate::actions::GuiAction::EnableLoop { .. }
                        | crate::actions::GuiAction::DisableLoop { .. }
                )
            }),
            "{kind:?}: the loop toggle armed a loop for a pane nothing \
             renders loop frames for, so enabling it would wait for ever"
        );
        assert!(
            h.stack().rows.is_empty(),
            "{kind:?}: layer rows were drawn for a pane with no map to draw \
             overlays on: {:?}",
            h.stack().rows
        );
    }
}

/// 45. **A pane with no map does not keep the label-tile pyramid downloading.**
///
///     City labels are raster tiles drawn *over* the base map, so a pane with
///     no tiles has nowhere to put one — yet its `enabled_overlays` is
///     inherited across the conversion and still says the layer is on, which is
///     what kept `ensure_label_tiles` fetching. Mild in itself; it is here
///     because it is the same shape as the overlay auto-poll gate, which is
///     not mild, and because the two must agree.
///
///     `clear_graphics_state` in the middle of each arm, because
///     `ensure_label_tiles` only ever *creates* the source — a harness that
///     had already made them would keep them and prove nothing. That is not a
///     contrivance either: dropping the tile sources and letting the next
///     frame re-make them is exactly what a suspend or a surface loss does.
#[test]
fn a_pane_with_no_map_stops_the_label_tiles_downloading() {
    fn tiles_remade_after_a_reset(h: &mut InputHarness) -> bool {
        h.gui_mut().clear_graphics_state();
        assert!(
            !h.gui.label_tiles_made_for_test(),
            "precondition: the reset must really have dropped the tile sources"
        );
        h.frames_for(2, FRAME_DT);
        h.gui.label_tiles_made_for_test()
    }

    let mut on_a_map = InputHarness::with_screen(egui::vec2(1200.0, 900.0));
    on_a_map.set_overlay_on_pane(0, OverlayKind::CityLabels, true);
    assert!(
        tiles_remade_after_a_reset(&mut on_a_map),
        "precondition: a map pane with city labels on must fetch label tiles"
    );

    let mut converted = InputHarness::with_screen(egui::vec2(1200.0, 900.0));
    converted.set_overlay_on_pane(0, OverlayKind::CityLabels, true);
    converted.make_pane_volume(0);
    assert!(
        converted.overlay_enabled(OverlayKind::CityLabels),
        "precondition: the pane still *remembers* wanting labels, which is what \
             makes this a filter rather than a cleared flag"
    );

    assert!(
        !tiles_remade_after_a_reset(&mut converted),
        "a pane with no map to draw labels on kept the label-tile pyramid \
             downloading"
    );
}

/// 46. **A non-map pane's product picker survives the Radar layer being off.**
///
///     The picker used to be gated on `is_overlay_enabled(OverlayKind::Radar)`,
///     which asks whether the *map* should draw the radar image over its tiles.
///     A pane with no tiles has no such layer, so a pane converted while that
///     toggle happened to be off would have had no product control at all —
///     absent, not disabled, for a reason nothing on screen explains. A map
///     pane must still honour the toggle, which is the second half here.
#[test]
fn a_non_map_panes_product_picker_ignores_the_radar_layer_toggle() {
    let mut h = InputHarness::with_screen(egui::vec2(1200.0, 900.0));
    h.load_scan("KTLX");
    // The picker lives in the inspector's Pane-properties body now.
    h.open_pane_props();
    h.set_overlay_on_pane(0, OverlayKind::Radar, false);
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

/// The stack's screen rect — the floating area's own rect, from egui's area
/// state rather than a reconstruction of the panel's position constants, so
/// it keeps meaning "the layer stack" if the insets ever change.
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
///
///     Site code, then kind. A map pane's properties body is full of content
///     that describes itself; a converted pane's loses most of that bulk,
///     and without one shared line saying what the pane *is*, the shorter
///     body reads as a different thing altogether rather than as the same
///     body showing a pane with fewer controls — the presentation bug this
///     whole contract exists to fix, carried from the old sidebar into the
///     inspector. The assertion is on the full rendered string, so three
///     per-kind headers that drifted apart in format could not keep it
///     green.
///
///     The fixture site is KDMX, deliberately **not** the default KTLX:
///     for the whole shell pass the active slot in `self.panes` holds a
///     `mem::take` placeholder whose site *is* the default, so on a
///     default-site fixture an identity line that read its site through
///     the placeholder would paint the right string for the wrong reason.
#[test]
fn every_pane_kinds_sidebar_opens_with_the_same_identity_line() {
    let mut h = InputHarness::with_screen(egui::vec2(1200.0, 900.0));
    h.load_scan("KDMX");
    h.open_pane_props();
    // The crumb itself, asserted as the string a user reads (M3 review) —
    // `open_pane_props` already pinned the mode probe, which is the arm and
    // not the text.
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

/// 50. **The missing layer list is explained, in one line, for both
///     non-map kinds — and only for them.**
///
///     The panel is titled "Layers", and for a 3D or section pane the
///     layer tree is omitted because every entry in it is a layer drawn
///     over map tiles (test 44 pins the omission). Omitted *silently*, the
///     void where most of the panel used to be is what made the sidebar
///     read as broken. The convention pinned here is omission plus one
///     explanatory line, the same line for both kinds — and its absence on
///     a map pane, where the list itself is present and the note would be
///     a false claim about it.
#[test]
fn the_missing_layer_list_is_explained_for_both_non_map_kinds() {
    for kind in [PaneKind::CrossSection, PaneKind::Volume] {
        let mut h = InputHarness::with_screen(egui::vec2(1200.0, 900.0));
        h.load_scan("KTLX");
        let sidebar = sidebar_rect(&h);
        assert!(
            !h.text_painted_in(sidebar, crate::ui::NON_MAP_LAYERS_NOTE),
            "a map pane draws the layer list itself and must not also \
                 carry the note explaining its absence"
        );

        match kind {
            PaneKind::CrossSection => h.make_pane_unaimed_cross_section(0),
            _ => h.make_pane_volume(0),
        }
        h.frames_for(2, FRAME_DT);
        assert!(
            h.text_painted_in(sidebar, crate::ui::NON_MAP_LAYERS_NOTE),
            "{kind:?}: the layer list is omitted with nothing to say why, \
                 which is what made the panel read as broken; painted: {:?}",
            h.painted_text_strings_in(sidebar)
        );
    }
}

/// 51. **A converted pane's own controls sit inside the Pane-properties
///     body's shared structure, in its order.**
///
///     The contract, top to bottom, for either non-map kind: identity
///     line, product picker, then the kind's own block under its own
///     header. (The explained absence of the layer list moved with the
///     list itself — it is the stack's, pinned by test 50; time navigation
///     left for the floating timeline at the full-bleed flip.) The
///     assertion is on the *positions* the body painted, not merely on
///     the strings existing: a kind block rendered above the shared
///     controls would paint every one of these strings and still read as
///     a bolted-on foreign block.
///
///     The headers are named through the constants the body itself
///     renders, so renaming a header moves the test with it, while
///     *removing* one — or demoting the section block back to nothing —
///     fails on a missing anchor.
///
///     KDMX rather than the default KTLX for the reason test 49 gives:
///     the `mem::take` placeholder in the active slot carries the default
///     site, and only a non-default fixture makes the identity line's
///     site half observable. The section anchor pins the painted length
///     too: [`section_ends`]'s line is 105.46 km by the same haversine
///     the readout quotes, `{:.0}` in the default kilometres, so a
///     readout scaled or converted wrongly misses the anchor.
#[test]
fn kind_specific_blocks_sit_inside_the_shared_sidebar_structure() {
    /// The y-centre of the topmost painted run containing `needle`, inside
    /// the sidebar.
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
    // `inspector_rect` is re-read after every conversion: the panel
    // shrink-wraps to its body, so the previous kind's rect can be shorter
    // than the next kind's content.
    assert_descending_order(
        &h,
        inspector_rect(&h),
        &[
            "KDMX - 3D volume",
            "Reflectivity",
            crate::ui::VOLUME_SIDEBAR_HEADER,
            // "Lit volume" and "Isosurface" share this row; the label
            // anchors it (the order test wants one needle per row).
            "Mode:",
            "Reset view",
        ],
    );

    // Isosurface mode reveals its threshold slider — labelled with the
    // product's own comparison — and, with an alpha curve drawn, the
    // honest word that the surface reads the data rather than the curve.
    h.gui_mut()
        .pane_mut(0)
        .expect("pane 0 exists")
        .volume_mut()
        .expect("pane 0 is a 3D pane")
        .view_mode = crate::pane::VolumeViewMode::Isosurface;
    h.gui_mut().volume_alpha.set(
        rustdar_radar::types::RadarProduct::Reflectivity,
        crate::volume_alpha::AlphaCurve::from_alphas([7u8; crate::volume_alpha::CURVE_LEN]),
    );
    h.frames_for(2, FRAME_DT);
    assert_descending_order(
        &h,
        inspector_rect(&h),
        &[
            crate::ui::VOLUME_SIDEBAR_HEADER,
            "Mode:",
            "\u{2265}:",
            "applies to the lit volume only",
            "Reset view",
        ],
    );

    // The section pane, aimed, reports its line inside the same structure…
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

    // …and unaimed it says so in the same place, rather than dropping the
    // block and going back to reading as a stub. Through Map first:
    // `set_kind` to the kind the pane already is keeps its content, which
    // here would keep the line and test the aimed state twice.
    h.gui_mut()
        .pane_mut(0)
        .expect("pane 0 exists")
        .set_kind(PaneKind::Map);
    h.make_pane_unaimed_cross_section(0);
    h.frames_for(2, FRAME_DT);
    assert_descending_order(
        &h,
        inspector_rect(&h),
        &[crate::ui::SECTION_SIDEBAR_HEADER, "No line drawn yet"],
    );

    // The explained absence of the layer list is the *stack's* — beside
    // this body, not inside it: the rows it explains are the stack's rows.
    assert!(
        h.text_painted_in(sidebar_rect(&h), crate::ui::NON_MAP_LAYERS_NOTE),
        "the stack must carry the layer-list note for a converted pane"
    );
}

/// 52. **Converting the active pane keeps the panels' own widget ids.**
///
///     Test 48 pins that converting a *non-active* pane moves nothing, but
///     there the panels' content never changes. Converting the **active**
///     pane rebuilds the inspector's kind block and swaps the stack's rows
///     for the note, and the hazard is the panels' own: the shared controls
///     key their stored state — combo state, both scroll offsets — on ids
///     derived from each panel's scope, so a scope re-keyed by the
///     conversion (salting it with the kind is the natural mistake) turns
///     "the user made this pane 3D" into "egui forgot the panel's state".
///     Compared per name, through the ids the panels actually resolved, for
///     the reason test 14 gives: rebuilding the expected ids from the same
///     format strings would prove nothing.
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
    // The product combo lives in the inspector's Pane-properties body.
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

// 47 retired (synthesis-m1): the drawer no longer appends the menu, so
// there is nothing below the kind block for a conversion to re-key — the
// menu lives in the top bar's popup, above every take window. Tests 48 and
// 52 hold what remains of the claim; the kind-scope's rationale is on
// `render_pane_props_body`.

/// 48. **Converting a pane must not move any widget's egui `Id`.**
///
///     The `"pane_map"` id salt is a key, not a description: every widget
///     inside a pane derives its `Id` from it, so egui's memory of what the
///     pane remembers hangs off it. Re-keying it — by renaming it to
///     something kind-neutral, or by folding the kind into it — would turn
///     "the user made pane 2 a section" into "egui forgot everything pane 2
///     remembered".
///
///     [`crossing_a_breakpoint_does_not_move_any_widget_id`] will not catch
///     this: it compares the layers panel's own probed ids across a resize,
///     and a pane's ids are not in that list. This reads egui's per-pass
///     widget bookkeeping instead, so it fires for the kind-specific work
///     later work packages add inside each arm as much as for a renamed
///     salt.
///
///     The mechanism it is a net for: `Ui::new_child` folds the parent's
///     auto-id counter into every child's unique id — an `id_salt` moves only
///     the *stable* id, as `ui_shell.rs`'s module note on the breakpoint
///     records — so an arm that stopped building the shared child `Ui`, or
///     built an extra one, re-keys every pane the loop reaches **after** it
///     while leaving their rects exactly where they were.
///
///     Hence the **middle** of three, which is what makes the assertion bite
///     today, verified by mutation both ways round: with an arm building a
///     spare child `Ui`, converting the middle pane fails this test and
///     converting the *last* one does not. The reason is that the only
///     auto-id'd widget inside a pane right now is the map's own, so a
///     later map pane is the only thing whose id can be seen to move. It is
///     specifically *not* the divider layer downstream of the loop: that
///     child `Ui`'s own id does shift, but `PaneLayout::handle_dividers`
///     gives every divider an explicit `Id::new(("h_div", …))`, and an
///     explicit id does not read the parent's counter.
///
///     A one- or two-pane version of this assertion is still worth writing
///     once an arm registers widgets of its own — from then on a converted
///     pane re-keys *itself* and needs no later pane to reveal it.
#[test]
fn converting_a_pane_moves_no_widget_id() {
    // Short enough that the stack's rows overflow and there is a real
    // scroll offset to lose — the row list fits anything much taller.
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 500.0));
    h.set_pane_count(3);
    h.load_scan("KTLX");

    // Real stored state behind a real widget id, so "nothing was lost" is a
    // claim about something rather than about an empty set.
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
        vec![PaneKind::Map, PaneKind::CrossSection, PaneKind::Map],
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

/// 54. **The menu checkbox arms the draw, and a drag on a map becomes a
///     section.** (Renumbered from a colliding 44.)
///
///     Through the dropdown's own checkbox, which is where the mode is
///     armed and — just as importantly — where it is turned off again: a
///     mode whose state is invisible is a map that has mysteriously
///     stopped panning.
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
    // Arming closes the dropdown, and must: the next thing the user does
    // is a drag on the map, and an open menu is in its way.
    assert_eq!(
        h.menu_leaf(crate::ui::DRAW_CROSS_SECTION_LABEL),
        None,
        "the dropdown stayed open over the map the line has to be drawn on"
    );

    // Re-opened, the checkbox shows the mode it turned on — which is what a
    // user who armed it by accident needs in order to un-tick it.
    h.open_menu();
    assert_eq!(
        h.menu_leaf(crate::ui::DRAW_CROSS_SECTION_LABEL)
            .map(|l| l.value),
        Some(Some(true)),
        "the checkbox does not show the mode it just turned on"
    );

    // And un-ticking it disarms, so the mode is never a trap.
    h.mouse_click(clickable_leaf(&h, crate::ui::DRAW_CROSS_SECTION_LABEL).center());
    h.frames_for(3, FRAME_DT);
    assert!(
        !h.section_draw_armed(),
        "the checkbox could not turn it off"
    );
}

/// 55. **An armed drag on a map becomes a section aimed where it was drawn.**
///     (Renumbered from a colliding 45.)
///
///     Through the real pointer pipeline, `render_panes`' resolution and the
///     deferred apply.
///
///     Two claims beyond "a pane appeared", and both are about what the
///     drag did *not* do. The map must not have panned — the drag belongs to
///     the line, and a section drawn while the ground slid under it is of
///     nowhere in particular. And the mode must have disarmed itself, or the
///     user's next pan is a second section.
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
    // Taken **before** the drag, and it has to be: applying the line grows
    // the layout, which halves pane 0's rect and therefore changes what
    // every pixel in it names. Recomputing afterwards would compare the line
    // against a projector that never drew it.
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
        .position(|k| *k == PaneKind::CrossSection)
        .expect("the drag produced no section pane");
    let line = h
        .section_line(target)
        .expect("the section pane has no line");
    // A thousandth of a degree — about 90 m, or a third of a pixel at this
    // zoom. Loose enough to absorb walkers still settling its zoom
    // animation across the warm-up frames, and three orders of magnitude
    // tighter than the failure it is written against: an endpoint mapped
    // through the wrong rect, or through no projector at all, lands
    // kilometres away or in Kansas.
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

/// 56. **A rendered section's caption is calm by default, and the honesty
///     detail is one click away — reachable, in the user's words, and
///     closable again.** (Renumbered from a colliding 46.)
///
///     The redesign's whole contract, end to end. The old caption painted
///     the ladder warning and the registration caveat on every ordinary
///     section, the warning in error styling — and watched with real users
///     it read as something broken, something they had broken. The drawn
///     rungs and the `NEAREST` upload stay as the standing honesty devices;
///     the words move behind the ⓘ.
///
///     Asserted through the ladder's *own* numbers on both sides of the
///     toggle, because a detail saying the same thing whatever the volume
///     was would be boilerplate a reader learns to skip. 14 rungs 0.5°
///     apart and 5 rungs 5° apart are the same sentence with entirely
///     different consequences.
#[test]
fn a_rendered_sections_caption_is_calm_and_its_detail_is_one_click_away() {
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    h.load_scan("KTLX");
    let (a, b) = section_ends();
    h.make_pane_cross_section(0, a, b);
    h.place_section(0, vcp_212_axes(), &vcp_212_rungs());

    // The caption sits along the pane's top-left, which the floating
    // layers panel now covers; close it — the user's own remedy — so the
    // detail-toggle click reaches the caption rather than the chrome.
    h.close_layers();

    let pane = h.pane_rects()[0];
    assert!(
        !h.text_painted_in(pane, crate::ui::CROSS_SECTION_EMPTY_STATE),
        "precondition: the pane must be drawing a picture, not its empty state"
    );

    // The default: the ladder's own headline numbers, and none of the
    // long-form copy that read as an error state.
    assert!(
        h.text_painted_in(pane, "14 tilts"),
        "the default caption lost the ladder's own count; it painted {:?}",
        h.painted_text_strings_in(pane)
    );
    for wall_of_text in ["not measured", "slant range", "widest", "Echoes can sit"] {
        assert!(
            !h.text_painted_in(pane, wall_of_text),
            "the long-form detail is back in the default caption \
                 ({wall_of_text:?}); it painted {:?}",
            h.painted_text_strings_in(pane)
        );
    }

    // The ⓘ is on the pane, and clicking it opens the detail.
    let glyph = h
        .painted_text_rects()
        .into_iter()
        .find(|(rect, text)| text == "\u{2139}" && pane.contains(rect.center()))
        .map(|(rect, _)| rect)
        .expect("the caption has no \u{2139} detail toggle");
    h.mouse_click(glyph.center());
    h.frame();

    for phrase in [
        // The ladder's widest step: a measurement of *this* volume.
        "4.9",
        // What the reader must not do with the picture.
        "not measured",
        // And why echoes sit off the map's track, in words about what the
        // user sees rather than about which renderer is right.
        "Echoes can sit",
    ] {
        assert!(
            h.text_painted_in(pane, phrase),
            "the opened detail never said {phrase:?}; it painted {:?}",
            h.painted_text_strings_in(pane)
        );
    }
    // The developer-voice sentence is gone for good, open or closed: "the
    // section is right" is the app arguing with itself in front of the
    // user.
    assert!(
        !h.text_painted_in(pane, "The section is right"),
        "the detail still argues with the map in front of the user"
    );

    // And the detail closes again: an explanation that cannot be dismissed
    // is the old wall of text with an extra click in front of it.
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

/// 46b. **A rendered section is drawn the right way up, over the caption
///      that describes it, with its ladder on it and its readout live.**
///
///      Test 46 above is the only other thing that drives
///      `render_cross_section` end to end, and it reads back **painted text
///      only**. Every mutation inside the function body that does not change
///      a word therefore survived it: the tilt ladder gated off entirely,
///      `caption_height` reverted to a counted `len * 13.0`, the image's uv
///      flipped so the section is drawn upside down, `hover_value` never
///      written, and the transient status line never pushed into the
///      caption. Five mutants, one test, no failures — because nothing here
///      asserted a shape.
///
///      Two of the five are pointed:
///
///      * **The flip.** `y_of_height` is pinned in the module's own tests
///        and it is only half of the mapping; the image's uv rect is the
///        half that actually turns the picture over, and it was pinned
///        nowhere. The section module's doc calls this "the single most
///        likely mistake in the module and the least likely to be noticed —
///        a flipped section of a mature storm still looks like a storm".
///      * **`caption_height`.** It is the whole of F8's fix. The module's
///        own test computes the height itself and hands it to
///        `SectionLayout::new`, so reverting the production line to a count
///        passes it; only a test that goes through the real wiring, on a
///        pane narrow enough to make the caption *wrap*, can tell.
#[test]
fn a_rendered_section_is_the_right_way_up_and_carries_its_ladder() {
    // Narrow and tall on purpose, and the shape is load-bearing twice. The
    // width makes the caption *wrap* to half a dozen rows, which is the only
    // condition under which a measured caption height differs from a counted
    // one by more than a point or two — at 760 points both sentences fit on
    // one row each, `len * 13.0` lands within two points of the truth, and
    // the mutant passes. The height keeps it clear of
    // `TWO_LINE_CAPTION_MIN_HEIGHT` and of `CAPTION_MAX_HEIGHT_FRACTION`, so
    // the registration caveat is really there to be wrapped.
    let mut h = InputHarness::with_screen(egui::vec2(480.0, 900.0));
    h.load_scan("KTLX");
    let (a, b) = section_ends();
    h.make_pane_cross_section(0, a, b);
    h.place_section(0, vcp_212_axes(), &vcp_212_rungs());
    // With the ⓘ detail open — the caption's longest shape, and the only
    // one whose lines wrap enough rows for a counted height to be wrong by
    // more than a point or two.
    h.gui_mut()
        .pane_mut(0)
        .expect("pane 0 exists")
        .cross_section_mut()
        .expect("pane 0 is a section pane")
        .detail_open = true;
    h.warm_up();

    let pane = h.pane_rects()[0];
    let images = h.painted_images_in(pane);
    assert_eq!(
        images.len(),
        1,
        "a section pane paints exactly one textured quad, its raster; found \
             {images:?}"
    );
    let raster = images[0];
    assert!(
        raster.rect.width() > 0.0 && raster.rect.height() > 0.0,
        "the raster was painted into an empty rect: {:?}",
        raster.rect
    );

    // **The right way up.** egui's uv origin is the texture's top left and
    // the section's row 0 is the top of the height axis, so the quad's
    // top-left corner must sample the texture's top-left corner. Swapping
    // the uv rect's corners flips the picture with no error and no layout
    // change.
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

    // **The caption is paid for.** Every row of it is above the picture, not
    // over it — which is only true if the layout used the caption's
    // *measured* wrapped height rather than a count of its lines.
    // Both sentences, not just the first. A galley's rect spans every row it
    // wrapped to, and it is the *caveat* — the last line of the block — that
    // a short measurement pushes onto the picture; the ladder warning is
    // first and sits above the plot however wrong the height is.
    let caption_rows: Vec<(egui::Rect, String)> = h
        .painted_text_rects()
        .into_iter()
        .filter(|(rect, text)| {
            pane.contains(rect.center())
                && (text.contains("tilts")
                    || text.contains("dotted curves")
                    || text.contains("Echoes can sit"))
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

    // **The ladder is on it.** The rungs are the section's first honesty
    // device; without them the picture is a smooth field with nothing saying
    // which parts of it were measured.
    let color = crate::ui::map::section_render::tilt_rung_color();
    // Over everything rather than over the picture: the upper rungs leave
    // the top of the height axis part way along the line — a 19.5° beam is
    // 33 km up at 90 km — and are *clipped* when painted rather than
    // shortened, so filtering on the plot rect would read a real rung as a
    // missing one.
    let rungs = h.painted_segments_in(egui::Rect::EVERYTHING, color);
    assert!(
        !rungs.is_empty(),
        "no tilt rungs were drawn over the section, so nothing in the \
             picture says where the data actually is"
    );
    // One polyline per rung: every curve is sampled from the `A` end, so
    // each contributes exactly one segment starting at the plot's left
    // edge, and counting those counts the rungs.
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
    // Each curve is traced rather than dashed once, and all of them the
    // same length.
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
    // And they are on the *picture*, not merely somewhere on the pane.
    let over_the_picture = h.painted_segments_in(raster.rect, color).len();
    assert!(
        over_the_picture * 3 >= rungs.len(),
        "only {over_the_picture} of {} rung segments landed inside the \
             raster, so the ladder is not drawn where the data is",
        rungs.len(),
    );

    // **The readout is live.** Most of the value of the status plane is the
    // hover: it is the first place in the codebase that can say *why* a
    // pixel is blank, and it is written by this function after everything
    // above it has drawn.
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

    // **A transient state reaches the caption.** It is pushed last, under
    // the standing warning, so it can never push that off the pane — and a
    // build that never pushed it at all looks identical.
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
///
///     The reason the anchor is stored as *ground* and converted inside
///     `Map::show` on the press frame. An armed draw suppresses panning but
///     not zooming — walkers reads the wheel itself — so with a pixel anchor
///     a mid-drag notch would silently re-aim the line's near end while the
///     far end tracked the finger, and the section would be a convincing
///     picture of ground nobody pointed at.
///
///     Compared against the *same drag without the wheel* rather than
///     against a recomputed projection, so the claim is exact and needs no
///     tolerance: two identical presses on two identically warmed harnesses
///     must produce the same anchor, whatever happened afterwards.
///
///     And the release end must **differ**, which is the calibration: it
///     proves the zoom really landed and really changed what a pixel means,
///     so the anchor's stability is a property of the anchor rather than of
///     a wheel event that went nowhere.
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
            .position(|k| *k == PaneKind::CrossSection)
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

/// A map on pane 0 feeding a rendered section on pane 1, exactly as the
/// armed draw leaves the layout — the fixture every endpoint-drag test
/// starts from.
///
/// Returns the line's two ends as **ground**, not pixels: the warm-up
/// frames let walkers settle its zoom, so a test aims at a handle by
/// projecting the ground through [`InputHarness::screen_of`] at use time
/// rather than trusting a pixel recorded before the settling.
fn harness_with_committed_section() -> (InputHarness, GeoPoint, GeoPoint) {
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    h.set_pane_count(2);
    h.load_scan("KTLX");
    // The A end of the line sits under the floating layers panel on
    // Expanded, and a press there belongs to the panel.
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

/// 45c. **Dragging an endpoint previews live and re-aims the section only
///      on the drop** — the pointer's whole journey changes nothing the
///      cut dispatch can see, and the release changes exactly the line.
///
///      The stored line *is* the drag's contribution to the section
///      staleness key (`SectionTarget.line`), and the frontend dispatcher
///      re-cuts precisely when that key moves — so "the line holds still
///      until the drop" is this crate's half of "a drag in progress never
///      triggers an extraction". A re-cut is a multi-MB walk of the merged
///      volume's gate bytes; per drag frame it would be the most expensive
///      thing in the app.
///
///      Also pinned here because they are the same gesture: the map does
///      not pan while a handle is held (from the press frame, via the
///      recorded handle spots), and the drop does not blank the pane —
///      walking a line through a storm must not strobe the picture on
///      every drop.
#[test]
fn dragging_an_endpoint_re_aims_the_section_on_drop_and_never_mid_drag() {
    let (mut h, a, b) = harness_with_committed_section();
    let line_before = h.section_line(1).expect("the fixture committed a line");
    let centre_before = h.pane_center(0);

    let b_px = h.screen_of(0, b);
    let target_px = b_px + egui::vec2(-70.0, 45.0);
    // The ground the drop should land on, computed before the drag: the
    // map must not move under the gesture, so the answer must not either.
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
    // A is still where the fixture put it on the ground, so the whole
    // gesture read the projector rather than shifting pixels.
    assert!(
        (line.a().lat - a.lat).abs() < 1e-9,
        "A drifted from the ground the fixture named"
    );
    // The old picture stands until the re-cut lands: blanking here would
    // strobe the pane on every drop of a walk through a storm.
    assert!(
        h.gui_mut()
            .pane(1)
            .and_then(|p| p.cross_section())
            .is_some_and(|s| s.texture.is_some() && s.section.is_some()),
        "the drop blanked the section pane"
    );
}

/// 45d. **A mid-drag zoom keeps the grabbed endpoint's ground.**
///
///      The drag suppresses panning but not zooming — walkers reads the
///      wheel itself — so a wheel notch mid-drag is ordinary. The preview
///      is geographic and the pointer is folded in only on frames it
///      moved, so a zoom-only frame must change nothing: re-unprojecting a
///      stationary pointer would slide the endpoint to whatever ground its
///      pixel names after the zoom.
///
///      Same construction as the armed-draw anchor test above: two
///      identical drags, one with a wheel notch before the release, must
///      commit the same line — and the zooms must really differ, or the
///      equality proves nothing.
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
            // At the pointer's own position, as a user zooming in on the
            // endpoint they are placing does — so the pointer does not
            // move, and only the ground under it changes.
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
///
///      The handles are unarmed, so their radius is the entire contract
///      with the map's primary gesture: inside it a press edits, outside
///      it the map pans exactly as it did before handles existed. A radius
///      that swallowed nearby presses would make every pan near a section
///      line a coin flip — the exact failure the armed modes exist to
///      avoid.
#[test]
fn a_press_beside_the_handles_still_pans_the_map() {
    let (mut h, a, b) = harness_with_committed_section();
    let a_px = h.screen_of(0, a);
    let b_px = h.screen_of(0, b);
    // Well off both handles and off the line's body alike.
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

/// 45f. **While the draw mode is armed, the handles go inert** — the same
///      press that would grab B draws a fresh line instead, exactly as it
///      did before handles existed.
///
///      One drag on one map pane cannot be two gestures. The armed mode
///      was asked for last (from the menu), so it wins; the two setters
///      also drop any edit drag in flight, which keeps the exclusion true
///      from both directions.
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
    // The armed draw's signature: the press became the *A end of a new
    // line*. An endpoint edit would have kept A where the fixture put it.
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

/// 45g. **Escape mid-drag cancels the edit and keeps the line** — the
///      same layer the armed drags sit on, because a drag in flight is the
///      most immediate thing a "back out" gesture can be aimed at.
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

/// 45h. **Dragging the line's body slides it rigidly** — length and
///      bearing kept — **previewing live and re-cutting only on the
///      drop**, exactly like an endpoint drag.
///
///      This is the "walk the section through the storm" motion (GR's
///      Position slider, as a direct gesture), so the mid-drag stillness
///      matters twice over: it is one re-cut per walk step instead of one
///      per frame, and the body is the *likeliest* thing to be dragged
///      repeatedly.
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
    // And it really moved: both ends, together.
    assert_ne!(line.a(), line_before.a());
    assert_ne!(line.b(), line_before.b());
}

/// 45i. **A shift-drag on the body sweeps the line about its midpoint** —
///      midpoint and length kept, bearing following the pointer.
///
///      The rotate affordance of issue #10. Shift is latched at the press,
///      so the verb cannot change mid-gesture; the fine-step spelling for
///      pointers with no modifier lives on the section pane's chips.
#[test]
fn a_shift_body_drag_sweeps_about_the_midpoint() {
    let (mut h, a, b) = harness_with_committed_section();
    let line_before = h.section_line(1).expect("the fixture committed a line");
    let mid_before = crate::ui_section_edit::midpoint(line_before);
    // Press three quarters of the way along, where a bearing about the
    // midpoint is well defined.
    let (press_lat, press_lon) =
        rustdar_radar::beam::great_circle_point((a.lat, a.lon), (b.lat, b.lon), 0.75);
    let press_px = h.screen_of(
        0,
        GeoPoint {
            lat: press_lat,
            lon: press_lon,
        },
    );
    // Where the pointer will end, unprojected now — the drag suppresses
    // panning, so the ground under that pixel must not move either. The
    // sweep's signed contract: the grabbed point sits on the pivot→B
    // side, so the committed line's bearing must land where the *pointer*
    // is about the pivot — not merely differ from the old bearing.
    let release_ground = h.ground_at(0, press_px + egui::vec2(-40.0, -48.0));
    let (want_bearing, _) = rustdar_radar::beam::site_bearing_range_km(
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
    // Pull the grabbed point across the line's run — a turn, not a slide.
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
    // And it turned the right way: the grabbed point followed the pointer
    // about the pivot, so the line's bearing landed on the pointer's. A
    // negated sweep delta turns the line *away* from the pointer by the
    // same magnitude — every assertion above still passes, and this one
    // misses by roughly twice the swing (~100° here).
    let got = crate::ui_section_edit::bearing_deg(line).rem_euclid(360.0);
    let off = (got - want_bearing.rem_euclid(360.0)).rem_euclid(360.0);
    assert!(
        off.min(360.0 - off) < 3.0,
        "the grabbed point swept away from the pointer: the line's \
             bearing landed at {got}\u{b0}, the pointer sat on \
             {want_bearing}\u{b0} from the pivot"
    );
}

/// A single section pane with a rendered cut, for the step-control tests —
/// the layout a phone gets, where the chips are the only pan/sweep there
/// is.
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

/// 45j. **The section pane's pan chips slide the line perpendicular to
///      itself, one step per click, keeping the picture** — and the pane
///      says which way the line faces while you do it.
///
///      A chip click is one deliberate action, so unlike the map drags it
///      commits immediately: one click, one re-cut, which is what makes
///      stepping feel like a control. The picture stands until the new cut
///      lands — strobing to "Cutting…" on every step is the exact failure
///      the walk-through-the-storm flow cannot have.
#[test]
fn a_pan_step_on_the_section_pane_slides_the_line_and_keeps_the_picture() {
    let (mut h, line_before) = harness_with_section_pane();
    let pane = h.pane_rects()[0];

    // The orientation readout: bearing (three digits wrapped to 0–359,
    // so north is 000° and never "360°") and length, in the user's units
    // — sweeping blind is the alternative.
    let expected_readout = format!(
        "{:03}\u{b0} - {:.0}{}",
        (crate::ui_section_edit::bearing_deg(line_before)
            .rem_euclid(360.0)
            .round() as u32)
            % 360,
        rustdar_units::UserPreferences::default()
            .distance
            .convert_from_km(crate::ui_section_edit::length_km(line_before)),
        rustdar_units::UserPreferences::default().distance.suffix(),
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
    // Perpendicular, to the left of A→B, by exactly one step.
    let mid_before = crate::ui_section_edit::midpoint(line_before);
    let mid_after = crate::ui_section_edit::midpoint(line);
    let (moved_bearing, moved_km) = rustdar_radar::beam::site_bearing_range_km(
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
    // The picture stands until the re-cut lands.
    assert!(
        h.gui_mut()
            .pane(0)
            .and_then(|p| p.cross_section())
            .is_some_and(|s| s.texture.is_some()),
        "a pan step blanked the pane"
    );
}

/// 45k. **The sweep chips rotate the line about its midpoint by one step**
///      — the fine-grained spelling of the sweep, and the only one a
///      touch screen with no modifier keys gets.
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

/// 45l. **The grab radii in absolute points: a press 30 points off a cap
///      and 20 points off the body still pans.**
///
///      45e proves *a* pan survives, but its press sits ~107 points from
///      the body and ~236 from the caps — beyond any plausible radius —
///      and the unit probes compute their presses *from* the constants, so
///      they follow a mutated radius wherever it goes. This press is
///      absolute: close enough to the line that a radius grown to a few
///      dozen points would swallow it, far enough that the shipped radii
///      (14 and 8) must not. The module doc calls the radius "the whole
///      contract with panning"; this is that contract observed at the
///      numbers it is set at.
#[test]
fn a_press_thirty_points_off_a_cap_and_twenty_off_the_body_still_pans() {
    let (mut h, a, b) = harness_with_committed_section();
    let a_px = h.screen_of(0, a);
    let b_px = h.screen_of(0, b);
    // 20 points perpendicular off the track, at the along-track distance
    // that puts the press exactly 30 points from the A cap:
    // sqrt(30² − 20²) ≈ 22.4 points toward B.
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

/// 45m. **Arming the region drag mid-flight kills the handle drag, and
///      the dead drag never commits.**
///
///      Three doc comments claim "both armed setters clear an in-flight
///      drag"; nothing observed it. The un-cleared failure is quiet today
///      — the drag freezes under the armed mode, misses its release, and
///      dies un-committed one frame after disarm — but that is three
///      accidents deep, and one refactor from committing a line the user
///      dragged half a gesture ago. The clear is pinned where it is
///      claimed: at the setter, the instant the mode goes on.
#[test]
fn arming_the_region_drag_clears_a_handle_drag_in_flight() {
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

    h.set_region_arm(true);
    assert!(
        h.gui_mut().section_edit_drag_for_test().is_none(),
        "arming the region drag left the handle drag alive: one drag on \
             one map pane would be two gestures"
    );

    // And nothing about the dead drag ever commits: the release lands
    // where a live drag would have re-aimed the line, and the line does
    // not move.
    h.frame();
    h.mouse_release(b_px + egui::vec2(-80.0, 40.0));
    h.frames_for(2, FRAME_DT);
    h.set_region_arm(false);
    h.frames_for(2, FRAME_DT);
    assert_eq!(
        h.section_line(1),
        Some(SectionLine::new(a, b).expect("the fixture's line")),
        "a drag killed by the arming still moved the line"
    );
}

/// 45n. **Arming the section draw mid-flight kills the handle drag too**
///      — the other armed setter, making the same claim, pinned the same
///      way. 45f proves the handles go *inert* while the draw is armed;
///      this pins the half the setters' docs add: the drag that already
///      existed is gone the instant the mode goes on, and never commits.
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
///     (Renumbered from a colliding 46.)
///
///     A stray tap is the single most likely thing to happen right after
///     arming — it is how a user checks which pane they are on. Turning it
///     into a zero-ish-length section is wrong; *silently disarming* is
///     worse, because the intent the user just expressed is gone with
///     nothing on screen to say so.
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
    // Under `MIN_SECTION_DRAG_PT` (24), and deliberately not zero: a
    // threshold mutated to `> 0.0` would pass a test that never moved.
    h.mouse_move(at + egui::vec2(9.0, 6.0));
    h.frame();
    h.mouse_release(at + egui::vec2(9.0, 6.0));
    h.frames_for(2, FRAME_DT);

    assert!(
        h.pane_kinds().iter().all(|k| *k == PaneKind::Map),
        "an 11-point drag became a cross-section"
    );
    assert!(
        h.section_draw_armed(),
        "a discarded drag disarmed the mode, throwing away the intent"
    );

    // And a real drag straight afterwards still works, so "stays armed"
    // means armed rather than merely not-disarmed.
    let to = at + egui::vec2(150.0, 90.0);
    h.mouse_press(at);
    h.frame();
    h.mouse_move(to);
    h.frame();
    h.mouse_release(to);
    h.frames_for(2, FRAME_DT);
    assert!(
        h.pane_kinds().contains(&PaneKind::CrossSection),
        "the still-armed mode did not draw the next line"
    );
}

/// 58. **While armed, a press on a map fires no overlay click and the map
///     does not pan** — for every pane the frame resolves, not just the one
///     the line is on. (Renumbered from a colliding 47.)
///
///     `ArmedSectionFrame` makes both properties of the returned value
///     rather than rules each caller remembers, and this reads them back out
///     of the probe `render_panes` records from the very locals that feed
///     `PaneRenderCtx` and `drag_pan_buttons`.
#[test]
fn an_armed_press_suppresses_panning_and_fires_no_overlay_click() {
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    h.load_scan("KTLX");
    h.warm_up();

    let pane = h.pane_rects()[0];
    let at = pane.center();

    // Unarmed first, so the assertion below is about being armed rather
    // than about a click that never happens.
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
///     (Renumbered from a colliding 48.)
///
///     A line is aimed with a projector and a section pane has none, so
///     arming the mode with one active leaves it exactly as it was — and in
///     particular does not suppress that pane's pointer or swallow its
///     clicks. The press that picks a map out of the layout is the same
///     press that starts the line, because `detect_active_pane_click` runs
///     at the top of the frame.
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

// --- The two armed modal drags, together --------------------------------
//
// Neither feature's author could have written these: the cross-section draw
// and the 3D region drag were built on the same base, in parallel, and this
// is the first branch on which both exist. Everything below is about the
// pair rather than about either one.

/// **The region checkbox closes the dropdown on arm, exactly as the
/// section checkbox does.**
///
/// The next thing the user does after arming is a drag on the map, and an
/// open menu is in its way. Un-ticking is the asymmetric half and it is
/// pinned too: disarming needs no map, so the menu stays open where the
/// user is.
#[test]
fn the_menus_checkbox_arms_the_region_drag_and_closes_the_dropdown() {
    let mut h = compact_with_menu();
    h.load_scan("KTLX");
    assert!(!h.region_arm(), "precondition: it starts unarmed");

    h.mouse_click(clickable_leaf(&h, crate::ui::REGION_ARM_LABEL).center());
    h.frames_for(3, FRAME_DT);

    assert!(h.region_arm(), "the checkbox did not arm the drag");
    assert_eq!(
        h.menu_leaf(crate::ui::REGION_ARM_LABEL),
        None,
        "the dropdown stayed open over the map the box has to be dragged on"
    );

    // Re-opened, the checkbox shows the mode it turned on — which is what a
    // user who armed it by accident needs in order to un-tick it.
    h.open_menu();
    assert_eq!(
        h.menu_leaf(crate::ui::REGION_ARM_LABEL).map(|l| l.value),
        Some(Some(true)),
        "the checkbox does not show the mode it just turned on"
    );

    // Un-ticking disarms and leaves the dropdown where the user is: only
    // arming needs the map underneath.
    h.mouse_click(clickable_leaf(&h, crate::ui::REGION_ARM_LABEL).center());
    h.frames_for(3, FRAME_DT);
    assert!(!h.region_arm(), "the checkbox could not turn it off");
    assert!(
        h.menu_leaf(crate::ui::REGION_ARM_LABEL).is_some(),
        "disarming needs no map, so it must not slam the dropdown shut"
    );
}

/// **Arming either modal drag disarms the other, and the menu says so.**
///
/// The two entries are adjacent checkboxes in the same submenu and they arm
/// the same gesture — press, move, release, on a map pane. With both on, one
/// drag would have to mean two things at once, so exactly one may be armed.
///
/// Driven through the dropdown's own checkboxes rather than through the
/// setters, because the claim is about what a user sees: the box that
/// un-ticked itself has to *read* as un-ticked, or the mode they think they
/// are in is not the one a drag will do. A rule enforced only in the setter
/// would leave two ticked boxes on screen and one working gesture.
#[test]
fn arming_either_modal_drag_un_ticks_the_other_in_the_menu() {
    let mut h = compact_with_menu();
    h.load_scan("KTLX");
    assert!(!h.section_draw_armed() && !h.region_arm(), "both start off");

    // Region first, then section. Arming closes the dropdown each time,
    // so each step re-opens it the user's way.
    h.mouse_click(clickable_leaf(&h, crate::ui::REGION_ARM_LABEL).center());
    h.frames_for(3, FRAME_DT);
    assert!(h.region_arm(), "the region checkbox did not arm the drag");

    h.open_menu();
    h.mouse_click(clickable_leaf(&h, crate::ui::DRAW_CROSS_SECTION_LABEL).center());
    h.frames_for(3, FRAME_DT);
    assert!(h.section_draw_armed(), "the section checkbox did not arm");
    assert!(
        !h.region_arm(),
        "both drags are armed: one press would anchor a line and start a box"
    );

    h.open_menu();
    assert_eq!(
        h.menu_leaf(crate::ui::REGION_ARM_LABEL).map(|l| l.value),
        Some(Some(false)),
        "the region checkbox still shows ticked after being un-armed"
    );
    assert_eq!(
        h.menu_leaf(crate::ui::DRAW_CROSS_SECTION_LABEL)
            .map(|l| l.value),
        Some(Some(true)),
        "the section checkbox does not show the mode it just turned on"
    );

    // And the other way round, which is not symmetric for free: the two
    // dispatcher arms are separate code.
    h.mouse_click(clickable_leaf(&h, crate::ui::REGION_ARM_LABEL).center());
    h.frames_for(3, FRAME_DT);
    assert!(h.region_arm(), "the region checkbox did not re-arm");
    assert!(
        !h.section_draw_armed(),
        "arming the region drag left the section draw armed"
    );

    h.open_menu();
    assert_eq!(
        h.menu_leaf(crate::ui::DRAW_CROSS_SECTION_LABEL)
            .map(|l| l.value),
        Some(Some(false)),
        "the section checkbox still shows ticked after being un-armed"
    );
}

/// **A back press cancels whichever modal drag is armed.**
///
/// One layer for both, below every painted layer — see
/// `Gui::dismiss_top_layer`. The reason it matters for the region drag is the
/// reason it mattered for the section draw: on Android a back press with a
/// mode on would otherwise exit the app, which is the reading of it least
/// likely to be what was meant.
#[test]
fn a_back_press_cancels_whichever_modal_drag_is_armed() {
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    h.load_scan("KTLX");

    h.set_region_arm(true);
    assert!(h.gui_mut().dismiss_top_layer(), "the region drag was armed");
    assert!(!h.region_arm());

    h.set_section_draw_armed(true);
    assert!(h.gui_mut().dismiss_top_layer(), "the draw was armed");
    assert!(!h.section_draw_armed());

    assert!(
        !h.gui_mut().dismiss_top_layer(),
        "with nothing left, a back press is a request to leave the app"
    );
}

/// **Two appliers, and never both with something to apply.**
///
/// `Gui::ui` runs `apply_pending_section_line` and then
/// `apply_pending_region` after the pane loop, and each of them can grow the
/// layout. Two growths in one frame is the case neither feature was written
/// for: the second applier's target rule would run against a layout the first
/// had already changed, and in a full layout both rules' last resort is the
/// same pane — so the second would convert the pane the first had just
/// filled, and one of two completed gestures would visibly produce nothing.
///
/// It cannot happen, and this is why: arming is exclusive, only an armed mode
/// records a pending, and each pending is recorded and consumed inside one
/// frame. So one drag, however the modes were armed, produces **one** new
/// pane of **one** kind — the kind the mode armed *last* asked for.
///
/// Asserted on the pane count as well as the kinds, because a rule that
/// produced the right kind by converting a pane the other applier had just
/// grown would leave the count at three and one of the two panes empty.
#[test]
fn two_appliers_never_both_have_something_to_apply() {
    for section_last in [true, false] {
        let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
        h.load_scan("KTLX");
        // Both armed, in turn, through the setters the menu uses. The second
        // call is what disarms the first, and that is the whole subject.
        if section_last {
            h.set_region_arm(true);
            h.set_section_draw_armed(true);
        } else {
            h.set_section_draw_armed(true);
            h.set_region_arm(true);
        }
        h.warm_up();
        let before = h.pane_kinds().len();
        assert_eq!(before, 1, "precondition: one map pane to drag on");

        let pane = h.pane_rects()[0];
        let from = pane.center() - egui::vec2(120.0, 60.0);
        let to = pane.center() + egui::vec2(120.0, 60.0);
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

        let kinds = h.pane_kinds();
        assert_eq!(
            kinds.len(),
            before + 1,
            "one drag grew the layout to {} panes (section_last={section_last}): {kinds:?}",
            kinds.len(),
        );
        assert_eq!(kinds[0], PaneKind::Map, "the map under the drag was spent");
        let wanted = if section_last {
            PaneKind::CrossSection
        } else {
            PaneKind::Volume
        };
        assert_eq!(
            kinds[1], wanted,
            "the mode armed last is not the one the drag did \
                 (section_last={section_last})",
        );
        assert_eq!(
            kinds.iter().filter(|k| **k != PaneKind::Map).count(),
            1,
            "one drag produced two non-map panes: {kinds:?}",
        );
    }
}

/// **While the section draw is armed, a drag commits no region — and the
/// converse.**
///
/// The narrower claim under the test above, and the one that would break
/// first. The two gestures are read by *different* code on different paths:
/// the section draw goes through `InteractionState::resolve_armed`, while
/// `handle_region_drag` reads `ui.ctx().input()` raw from inside `Map::show`.
/// Neither path knows about the other, so if the exclusion at the menu were
/// ever relaxed both would fire from the same press — and the symptom would
/// be a section pane *and* a 3D pane from one drag, which is exactly what a
/// reader would assume could not happen.
#[test]
fn an_armed_section_drag_leaves_no_region_behind_and_the_converse() {
    for section in [true, false] {
        let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
        h.load_scan("KTLX");
        if section {
            h.set_section_draw_armed(true);
        } else {
            h.set_region_arm(true);
        }
        h.warm_up();

        let pane = h.pane_rects()[0];
        let from = pane.center() - egui::vec2(120.0, 60.0);
        let to = pane.center() + egui::vec2(120.0, 60.0);
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

        let panes = h.pane_kinds().len();
        let aimed_regions = (0..panes)
            .filter(|idx| {
                h.gui_mut()
                    .pane(*idx)
                    .and_then(|p| p.volume())
                    .is_some_and(|v| v.region.is_some())
            })
            .count();
        let lines = (0..panes)
            .filter(|idx| h.section_line(*idx).is_some())
            .count();
        if section {
            assert_eq!(lines, 1, "the drag drew no section line");
            assert_eq!(
                aimed_regions, 0,
                "a section drag also committed a 3D region"
            );
        } else {
            assert_eq!(aimed_regions, 1, "the drag committed no region");
            assert_eq!(lines, 0, "a region drag also drew a section line");
        }
    }
}

// ── The Location control ─────────────────────────────────────────────
//
// Asserted against painted text rather than against `Gui` state, because
// the claim is about what the user is *offered*. A control that reads the
// right permission and renders the wrong button is exactly the failure this
// section exists to catch, and no state assertion can see it.

/// A refusal is a decision only the user can reverse, wherever their
/// platform keeps it. Offering a button here would be offering a dialog the
/// platform will not show — and on the design that mapped Windows'
/// `NotDeclaredByApp` to `Denied`, it would have bricked that arm outright.
#[test]
fn settings_offers_no_way_to_ask_once_the_os_has_refused() {
    let mut h = InputHarness::new();
    h.open_settings();
    h.gui_mut()
        .set_location_state(rustdar_gps::LocationPermission::Prompt, false);
    h.warm_up();
    assert!(
        h.painted_text_strings()
            .iter()
            .any(|t| t == "Use my location"),
        "control: an unasked platform must offer the button, or the \
             assertion below passes for free. Painted: {:?}",
        h.painted_text_strings()
    );

    h.gui_mut()
        .set_location_state(rustdar_gps::LocationPermission::Denied, false);
    h.warm_up();

    let painted = h.painted_text_strings();
    assert!(
        !painted.iter().any(|t| t == "Use my location"),
        "the OS has refused and the pane still offers to ask it again. \
             Painted: {painted:?}"
    );
    // The constant rather than a phrase: where a refusal is undone differs
    // by platform — Windows has a Settings page, Linux has a GSettings key
    // and no page at all — so a literal here would only ever pin whichever
    // one CI happened to run. What must hold everywhere is that the arm
    // renders the explanation beside the word.
    assert!(
        painted.iter().any(|t| t == crate::ui::LOCATION_DENIED_NOTE),
        "a denial with no button and no explanation is the state this \
             whole feature exists to remove. Painted: {painted:?}"
    );
}

/// The control is a window onto the OS, not a switch with a memory.
///
/// The tempting implementation keeps a local "the user turned it on" bool
/// and renders from that, which goes on reading `On.` after the permission
/// has been revoked underneath it — the exact reason `location_active()`
/// is a method on the bridge rather than a field in the gate.
#[test]
fn the_location_control_follows_the_os_rather_than_a_remembered_toggle() {
    let mut h = InputHarness::new();
    h.open_settings();
    h.gui_mut()
        .set_location_state(rustdar_gps::LocationPermission::Granted, true);
    h.warm_up();
    let painted = h.painted_text_strings();
    assert!(
        painted.iter().any(|t| t == "Turn off"),
        "a live location stream offers no way to stop it. Painted: {painted:?}"
    );

    // Revoked in system settings. Nothing in this crate was told to change
    // its mind; the cached state simply moved underneath it.
    h.gui_mut()
        .set_location_state(rustdar_gps::LocationPermission::Denied, false);
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

/// A platform with no location service must not be told to go and enable
/// one: the advice leads nowhere and the button would do nothing.
#[test]
fn a_platform_without_location_is_told_so_and_offered_nothing() {
    let mut h = InputHarness::new();
    h.open_settings();
    h.gui_mut()
        .set_location_state(rustdar_gps::LocationPermission::Unavailable, false);
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

/// The one thing worth offering after a refusal — and only where there is
/// somewhere to send the user.
///
/// Both halves are failures with no symptom. A button on a platform with no
/// settings page reads as "click here to fix this" and opens nothing; the
/// same button beside a *granted* permission invites the user into Settings
/// to solve a problem they do not have. It is Windows that has the page —
/// `ms-settings:privacy-location` — and the browser that has nothing of the
/// kind.
#[test]
fn a_denial_offers_the_system_settings_page_only_where_there_is_one() {
    const BUTTON: &str = "Open location settings";

    let mut h = InputHarness::new();
    h.open_settings();
    h.gui_mut()
        .set_location_state(rustdar_gps::LocationPermission::Denied, false);
    h.warm_up();

    let painted = h.painted_text_strings();
    assert!(
        !painted.iter().any(|t| t == BUTTON),
        "a platform that never claimed to have a settings page is offering \
             to open one. Painted: {painted:?}"
    );

    h.gui_mut().set_location_settings_available(true);
    h.warm_up();

    let painted = h.painted_text_strings();
    assert!(
        painted.iter().any(|t| t == BUTTON),
        "the OS refused, there is a page to send the user to, and the pane \
             does not offer it. Painted: {painted:?}"
    );

    // Every other state has something better on offer, or nothing to fix.
    for state in [
        rustdar_gps::LocationPermission::Granted,
        rustdar_gps::LocationPermission::Prompt,
        rustdar_gps::LocationPermission::Unknown,
        rustdar_gps::LocationPermission::Unavailable,
    ] {
        h.gui_mut().set_location_state(state, false);
        h.warm_up();
        let painted = h.painted_text_strings();
        assert!(
            !painted.iter().any(|t| t == BUTTON),
            "{state:?} is offering the remediation for a refusal. \
                 Painted: {painted:?}"
        );
    }
}

/// A refusal has to say something a user can act on, and on Linux the
/// generic sentence cannot: the switch that refused is
/// xdg-desktop-portal's `disable-location`, `xdg-desktop-portal-gtk`
/// answers it from `org.gnome.system.location enabled`, that key defaults
/// to **false**, and no desktop except GNOME has a page for it. So the one
/// arm with no settings button is also the one arm that must name the
/// command — otherwise the default state of a stock KDE machine reads
/// "Denied. …check your system settings" and there is nothing there.
///
/// `cfg`'d rather than asserted both ways, because the copy is `cfg`'d: on
/// every other platform "your system settings" is a real place.
#[cfg(target_os = "linux")]
#[test]
fn a_linux_refusal_names_the_setting_that_would_undo_it() {
    let mut h = InputHarness::new();
    h.open_settings();
    h.gui_mut()
        .set_location_state(rustdar_gps::LocationPermission::Denied, false);
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

/// The gap the ungated line closes. `Fix:`/`No GPS fix` lives inside
/// `#[cfg(feature = "gps-serial")]`, so on web, Android, iOS and every
/// build without a serial port the section would read `On.` beside an empty
/// map and explain nothing — which is the likely Linux outcome too, where
/// the portal can take a while or answer with nothing at all.
#[test]
fn a_granted_permission_with_no_fix_yet_says_so() {
    let mut h = InputHarness::new();
    h.open_settings();
    h.gui_mut()
        .set_location_state(rustdar_gps::LocationPermission::Granted, true);
    h.warm_up();

    let painted = h.painted_text_strings();
    assert!(
        painted.iter().any(|t| t.contains("Waiting for a fix")),
        "location is on, no position has arrived, and the pane says only \
             'On.'. Painted: {painted:?}"
    );

    h.gui_mut()
        .set_gps_fix(rustdar_gps::GpsFix::from_device_position(35.25, -97.5));
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

// ── M4: site search, time links, the catalog and presets ────────────────

/// 69. **The site search narrows the list, highlights the current site, and
///     a row click switches the pane's site.**
///
///     The inspector's Pane-properties body is the first *list* route to a
///     site — the map's clickable icons were the only picker before — and a
///     row click must mean exactly what an icon click means: the same
///     `SwitchRadarSite`, aimed at the active pane. The count caption is
///     computed from the compiled-in table, so it is asserted against the
///     table too rather than as a literal.
#[test]
fn the_site_search_narrows_the_list_and_a_row_click_switches_the_site() {
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    h.open_pane_props();

    let inspector = h.inspector();
    let total = rustdar_radar::sites::RADARS.len();
    assert_eq!(
        inspector.site_rows.len(),
        total,
        "the unfiltered list must offer the whole table"
    );
    assert!(
        inspector
            .site_caption
            .starts_with(&format!("{total} shown")),
        "the caption must count what is shown; drew {:?}",
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

    // Type a query — lowercase on purpose; the codes are uppercase.
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
        inspector.site_caption.starts_with("1 shown"),
        "the caption must follow the filter; drew {:?}",
        inspector.site_caption
    );

    // The click emits the map-icon path's own action.
    h.mouse_click(inspector.site_rows[0].1.center());
    assert!(
        h.last_actions().iter().any(|a| matches!(
            a,
            crate::actions::GuiAction::SwitchRadarSite { site, pane_idx: 0 } if site == "KMKX"
        )),
        "clicking the row did not emit SwitchRadarSite for the active pane"
    );
}

/// 70. **An unlinked pane is excluded from shared time — the loop fan-out
///     and the sync pass's time pair — and the link checkbox reflects and
///     toggles.**
///
///     The checkbox lives in the Pane-properties sync section and writes the
///     *taken* pane; the fan-out reads `time_sync_targets`, so the loop
///     actions name exactly the linked map panes; and
///     `propagate_layer_sync` leaves an unlinked pane's `viewing_live` and
///     `time_step_secs` alone while still converging everything else.
#[test]
fn an_unlinked_pane_is_excluded_from_shared_nav_and_loop_fan_out() {
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    h.set_pane_count(3);
    h.load_scan("KTLX");

    // The checkbox reflects the stored state and toggles it.
    h.open_pane_props();
    let (link, on) = h
        .inspector()
        .time_link
        .expect("a multi-pane layout draws the link checkbox");
    assert!(on, "a fresh pane starts linked");
    h.mouse_click(link.center());
    h.warm_up();
    assert!(
        !h.gui_mut().pane(0).expect("pane 0").time_link,
        "the click must unlink the pane"
    );
    let (link, on) = h.inspector().time_link.expect("still drawn");
    assert!(!on, "the checkbox must reflect the stored state");
    h.mouse_click(link.center());
    h.warm_up();
    assert!(
        h.gui_mut().pane(0).expect("pane 0").time_link,
        "a second click must relink it"
    );

    // Unlink pane 1; panes 0 and 2 stay linked.
    h.gui_mut().pane_mut(1).expect("pane 1").time_link = false;
    h.warm_up();

    // The loop fan-out skips it.
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

    // The sync pass leaves the frozen pane's time posture alone while the
    // linked pane converges. Everything else still syncs — the site here.
    {
        let gui = h.gui_mut();
        gui.pane_mut(1).expect("pane 1").viewing_live = false;
        gui.pane_mut(1).expect("pane 1").time_step_secs = 0;
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
        gui.pane(1).expect("pane 1").time_step_secs,
        0,
        "the unlinked pane's step must stay its own"
    );
    assert_eq!(
        gui.pane(1).expect("pane 1").site,
        "KTLX",
        "unlink is about time: every other synced field still converges"
    );
}

/// **A keyboard nudge on the archive scrubber commits** (§5.9 carried
/// finding: `changed()` without a drag used to store the position and wait
/// for a release that never comes).
///
/// egui's slider reads its arrow keys only while focused, and only a
/// `TextEdit` takes focus from a click — so focus is granted through the
/// id the renderer reported, as tabbing to the slider would.
#[test]
fn a_keyboard_nudge_on_the_archive_scrubber_commits() {
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    h.load_scan("KTLX");
    // Parked in the archive, so a nudge is an archive step rather than a
    // jump back to live from the rail's right end.
    h.gui_mut().pane_mut(0).expect("pane 0").viewing_live = false;
    h.warm_up();

    let scrubber_id = h
        .widget_id_probes()
        .into_iter()
        .find(|(name, _)| *name == "timeline_scrubber")
        .expect("the scrubber must report its id")
        .1;
    h.focus_widget(scrubber_id);

    // Several presses in one frame: each is one rail point, and the commit
    // threshold near the right end must be cleanly crossed.
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

/// 67a. **The catalog's search filters every group, and a product tile aims
///      the active pane.**
///
///      A product tile means "show me this picture": it sets the pane's
///      product (resetting the tilt, as the combo does), turns the Radar
///      layer on, selects the Radar layer in the inspector, and closes the
///      catalog.
#[test]
fn the_catalog_search_filters_and_a_product_tile_aims_the_active_pane() {
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    h.open_catalog();

    let catalog = h.catalog();
    for group in [
        crate::ui::CatalogGroup::Presets,
        crate::ui::CatalogGroup::Overlays,
        crate::ui::CatalogGroup::Products,
        crate::ui::CatalogGroup::Hrrr,
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
        .catalog_tile(crate::ui::CatalogGroup::Products, "Spectrum Width")
        .expect("the product tile survives its own name as the query");
    h.mouse_click(tile.rect.center());
    h.warm_up();

    assert!(!h.catalog().open, "applying a tile must close the catalog");
    let pane = h.gui_mut().pane(0).expect("pane 0");
    assert_eq!(
        pane.selected_product,
        rustdar_radar::types::RadarProduct::SpectrumWidth,
        "the tile did not set the active pane's product"
    );
    assert_eq!(
        pane.selected_elevation, 0.0,
        "the old product's tilt must not survive the switch"
    );
    assert!(
        h.overlay_enabled_on(0, OverlayKind::Radar),
        "a product under a hidden radar layer is a click that did nothing"
    );
    assert_eq!(
        h.inspector().mode,
        Some(crate::ui::InspectorSelection::Layer(OverlayKind::Radar)),
        "the Radar layer's options must be selected"
    );
}

/// 67b. **An overlay tile enables the layer — with the shared enable-fetch
///      rule — selects it, and closes the catalog.**
///
///      SPC outlooks are the layer that makes the fetch half a contract: off
///      by default and never auto-polled, so without the queued fetch the
///      tile would enable a layer that never draws anything.
#[test]
fn an_overlay_tile_enables_the_layer_and_selects_it() {
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    assert!(
        !h.overlay_enabled_on(0, OverlayKind::SpcOutlook),
        "precondition: outlooks start off, so the tile has something to do"
    );

    h.open_catalog();
    let tile = h
        .catalog_tile(crate::ui::CatalogGroup::Overlays, "SPC Outlooks")
        .expect("the overlays group offers SPC Outlooks");
    h.mouse_click(tile.rect.center());
    assert!(
        h.last_actions().iter().any(|a| matches!(
            a,
            crate::actions::GuiAction::FetchOverlay {
                kind: OverlayKind::SpcOutlook,
                pane_idx: 0
            }
        )),
        "enabling a dataless, never-polled layer must queue its first fetch"
    );
    h.warm_up();

    assert!(!h.catalog().open, "applying a tile must close the catalog");
    assert!(
        h.overlay_enabled_on(0, OverlayKind::SpcOutlook),
        "the tile did not enable the layer"
    );
    assert_eq!(
        h.inspector().mode,
        Some(crate::ui::InspectorSelection::Layer(
            OverlayKind::SpcOutlook
        )),
        "the enabled layer's options must be selected"
    );
}

/// 67c. **An HRRR tile enables the model layer and sets the parameter
///      through the handler's own control route.**
#[test]
fn an_hrrr_tile_enables_the_model_layer_and_sets_the_parameter() {
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    h.open_catalog();

    let tile = h
        .catalog_tile(crate::ui::CatalogGroup::Hrrr, "Surface-Based CAPE")
        .expect("the HRRR group offers the parameter");
    h.mouse_click(tile.rect.center());
    assert!(
        h.last_actions().iter().any(|a| matches!(
            a,
            crate::actions::GuiAction::FetchOverlay {
                kind: OverlayKind::ModelData,
                ..
            }
        )),
        "an uncached parameter must ask for its data"
    );
    h.warm_up();

    assert!(!h.catalog().open);
    assert!(
        h.overlay_enabled_on(0, OverlayKind::ModelData),
        "the tile did not enable the model layer"
    );
    assert_eq!(
        h.inspector().mode,
        Some(crate::ui::InspectorSelection::Layer(OverlayKind::ModelData)),
        "the model layer's options must be selected"
    );
    // The parameter landed in the handler the inspector now shows: the
    // dropdown's own model says so.
    let (_, selected) = h
        .dropdown_model("Parameter")
        .expect("the model layer's body offers the parameter dropdown");
    assert_eq!(
        selected, "sbcape",
        "the tile's parameter must be the one selected"
    );
}

/// **Presets: saving captures the view, the tile appears, applying
/// reproduces the capture, deleting removes it** (§3.11).
#[test]
fn a_saved_preset_appears_applies_and_deletes() {
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    h.load_scan("KTLX");
    // Sync off, so what apply writes per pane is what this test observes —
    // not what the sync pass copied a frame later.
    h.set_sync_layers(false);

    // A distinctive view: two panes, velocity on the first, storm reports on.
    h.set_pane_count(2);
    h.gui_mut().pane_mut(0).expect("pane 0").selected_product =
        rustdar_radar::types::RadarProduct::Velocity;
    h.set_overlay_on_pane(0, OverlayKind::StormReports, true);
    h.warm_up();

    // Save it under a name.
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

    // Wreck the view, then apply: the capture must come back.
    h.set_pane_count(1);
    h.gui_mut().pane_mut(0).expect("pane 0").selected_product =
        rustdar_radar::types::RadarProduct::Reflectivity;
    h.set_overlay_on_pane(0, OverlayKind::StormReports, false);
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
        h.gui_mut().pane(0).expect("pane 0").selected_product,
        rustdar_radar::types::RadarProduct::Velocity,
        "the preset's per-pane product must come back"
    );
    assert!(
        h.overlay_enabled_on(0, OverlayKind::StormReports)
            && h.overlay_enabled_on(1, OverlayKind::StormReports),
        "the preset's overlay set must land on every pane"
    );

    // Delete removes the tile and the stored preset.
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

/// **Escape closes the catalog before anything beneath it** — the §3.4 slot,
/// as amended: after the ☰ dropdown, before the feature and time dialogs.
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
    // The second dismissal reached the layer beneath — the time dialog.
    assert!(
        !h.text_painted_in(h.screen_rect(), "Select Time"),
        "the second dismissal must take the time dialog"
    );
}

/// **The Data & live rows and the ☰ menu toggles read one field** — flipping
/// either side moves the other, because there is only one thing to move.
///
/// The state is observed through `ui_config_json`, which serialises the flag
/// itself — `is_auto_poll_active` cannot see it, because overlay auto-polls
/// keep that answer true regardless.
#[test]
fn the_data_and_live_rows_share_state_with_the_menu_toggles() {
    fn radar_auto_poll(h: &mut InputHarness) -> bool {
        let json = h.gui_mut().ui_config_json().expect("serialises");
        let value: serde_json::Value = serde_json::from_str(&json).expect("parses");
        value["auto_poll"].as_bool().expect("a bool")
    }

    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    h.open_settings();
    assert!(radar_auto_poll(&mut h), "precondition: auto-poll starts on");

    // The row sits far down the settings body: scroll it on screen the way a
    // user does, let the smooth-scroll animation settle, then click the
    // checkbox by its own painted label.
    let scroll_pos = h.inspector_rect().expect("the inspector is open").center();
    let found = h.scroll_until(scroll_pos, egui::vec2(0.0, -160.0), 120, |h| {
        h.settings_row("data.auto_poll")
            .is_some_and(|row| h.screen_rect().contains(row.rect.center()))
    });
    assert!(found, "the auto-poll row never scrolled on screen");
    // The row's probe rect is recorded even while the scroll clip still
    // hides the widget, so drain the smooth-scroll animation with real time
    // and nudge until the checkbox itself is painted before clicking it.
    h.frames_for(10, 0.05);
    let found = h.scroll_until(scroll_pos, egui::vec2(0.0, -40.0), 40, |h| {
        h.painted_text_rects()
            .iter()
            .any(|(_, text)| text == "Auto-poll")
    });
    assert!(found, "the Auto-poll checkbox never became visible");
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

    // The menu's toggle shows the same state — one field, two routes...
    h.open_menu();
    let leaf = h.menu_leaf("Auto-poll").expect("the menu still offers it");
    assert_eq!(
        leaf.value,
        Some(false),
        "the menu's checkbox must reflect the settings row's write"
    );

    // ...and writes it too: flipping it back through the menu is what the
    // settings row reads next frame.
    h.mouse_click(leaf.rect.center());
    h.warm_up();
    assert!(
        radar_auto_poll(&mut h),
        "the menu toggle must write the same field back"
    );
}

/// **Row 2's closing caption states this platform's frame budget and the
/// unlink hint** (§5.9 carried into M4) — the number is the frontend's push
/// (`set_loop_frame_budget`), never a guess from the width.
#[test]
fn the_timeline_row2_caption_states_the_pushed_frame_budget() {
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    // A deliberately non-default budget, so a caption printing the default
    // could not pass by coincidence.
    h.gui_mut().set_loop_frame_budget(12);
    h.mouse_click(h.timeline().expander.center());
    h.warm_up();

    let row2 = h.timeline().row2.expect("the expander must open row 2");
    assert!(
        row2.caption.contains("up to 12 frames"),
        "the caption must state the pushed budget; drew {:?}",
        row2.caption
    );
    assert!(
        row2.caption.contains("Follows shared time"),
        "the caption must carry the per-pane unlink hint, by the checkbox's \
         own name; drew {:?}",
        row2.caption
    );
}

// ── M5: pane pills, popovers and the armed hint ─────────────────────────

use crate::ui::PillKind;

/// A wide two-pane harness with the layers panel closed, so pane 0's pill
/// row is not under the floating stack — the state every pill test drives
/// from.
fn pill_harness() -> InputHarness {
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    h.set_pane_count(2);
    h.close_layers();
    h
}

/// 73a. **A click on a pill is never a map click — and its popover opens
///      anchored to the pill.**
///
///      Layer-based, per plan §3.3's resolution: the pills are egui `Area`s
///      above `Order::Background`, so the click gate every map resolver runs
///      (`filter_dialog_blocked` / `is_pos_blocked`) drops the position with
///      no excluded-rect plumbing. Staged with a radar-site icon placed
///      exactly under the site pill: without the layer the click would
///      switch the site, and with it the only thing that may happen is the
///      pill's own activate plus its popover.
#[test]
fn a_pill_click_never_reaches_the_map_and_its_popover_anchors() {
    let mut h = pill_harness();
    h.gui_mut().enable_overlay_for_test(OverlayKind::RadarSites);
    h.warm_up();

    // Pane 1 active, so the pill's own activate is observable.
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
    // Anchored to its pill: directly under it, horizontally overlapping.
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
///      swallowed, the second acts — and a confirmed map tap elsewhere puts
///      the row back to sleep.**
///
///      Opacity is `Ui::set_opacity`, painting only — that is what makes a
///      dim row reachable at all. The swallowed tap must have *no* pill
///      effect: no popover, no activation. The reveal is per pane and ends
///      with the gesture that means "I am working the map again".
#[test]
fn a_dim_rows_first_touch_tap_reveals_and_swallows() {
    let mut h = pill_harness();

    // Latch touch modality with a tap on the map of pane 1 — which also
    // makes pane 1 active, so pane 0's row has an activation to swallow.
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

    // The second tap acts: popover open, pane active.
    let (_, pill) = h.pill(0, PillKind::Site).expect("still drawn");
    h.touch_tap(pill.center());
    h.frames_for(2, 0.05);
    assert_eq!(h.active_pane_index(), 0, "the second tap activates");
    let popover = h.pill_popover().expect("the second tap opens the popover");
    assert_eq!((popover.pane_idx, popover.pill), (0, PillKind::Site));

    // Close the popover with a tap on pane 0's own map, far from the row —
    // a confirmed map tap, which also ends the reveal.
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

/// 73c. **"Pin pane controls" forces the rows to full opacity — through the
///      real settings row, and persisted.**
///
///      Driven the user's way: the Interface section's checkbox in the
///      inspector's App › Settings body. The parity walk covers the row's
///      presence; this pins what it does and that `ui_config_json` carries
///      it.
#[test]
fn pin_pane_controls_forces_full_opacity_and_persists() {
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    h.close_layers();

    // Control: pointer parked on the top bar — outside every pane — leaves
    // the row dim.
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
    // Drain the smooth-scroll animation with real time and require the
    // painted checkbox itself — the row's probe rect is recorded even while
    // the scroll clip still hides the widget.
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

    // Persisted, so the preference survives the session.
    let json = h.gui_mut().ui_config_json().expect("serialises");
    let value: serde_json::Value = serde_json::from_str(&json).expect("parses");
    assert_eq!(
        value["pin_pane_controls"].as_bool(),
        Some(true),
        "the pin must be written to the config"
    );
}

/// 73d. **The site pill's popover searches the one site list and a pick
///      emits the map icon's own `SwitchRadarSite`.**
#[test]
fn the_site_pill_popover_searches_and_switches() {
    let mut h = pill_harness();

    let (_, pill) = h.pill(0, PillKind::Site).expect("the site pill is drawn");
    h.mouse_click(pill.center());
    // The popup's debut frame only registers it; its widgets are clickable
    // from the next frame — the same "areas need a frame" rule the whole
    // harness warms up for.
    h.frame();
    let popover = h.pill_popover().expect("the popover opened");
    let search = popover.search.expect("with its search field");
    assert_eq!(
        popover.rows.len(),
        rustdar_radar::sites::RADARS.len(),
        "unfiltered, the popover offers the whole table — the inspector's \
         own list"
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

/// 73e. **The product and tilt popovers offer the combos' own lists, and a
///      pick writes the pane — with the product pick resetting the tilt.**
#[test]
fn the_product_and_tilt_pill_popovers_write_the_pane() {
    let mut h = pill_harness();
    h.load_scan("KTLX");
    h.offer_product(0, rustdar_radar::types::RadarProduct::Reflectivity, 0.5);
    h.offer_product(0, rustdar_radar::types::RadarProduct::Reflectivity, 1.5);
    h.close_layers();

    // -- product --
    let (code, pill) = h.pill(0, PillKind::Product).expect("a product pill");
    assert_eq!(code, "REF", "the pill shows the product code");
    h.gui_mut().pane_mut(0).expect("pane 0").selected_elevation = 1.5;
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
            pane.selected_product,
            rustdar_radar::types::RadarProduct::Velocity,
            "the pick did not set the pane's product"
        );
        assert_eq!(
            pane.selected_elevation, 0.0,
            "the old product's tilt must not survive the switch"
        );
    }

    // -- tilt, back on reflectivity where two angles are offered --
    h.select_product(0, rustdar_radar::types::RadarProduct::Reflectivity);
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
        h.gui_mut().pane(0).expect("pane 0").selected_elevation,
        1.5,
        "the pick did not set the pane's tilt"
    );
}

/// 73f. **The link pill's popover toggles the pane's time link, and its
///      caption is the honest unlink sentence the inspector shares.**
#[test]
fn the_link_pill_popover_toggles_the_time_link() {
    let mut h = pill_harness();

    let (glyph, pill) = h.pill(0, PillKind::Link).expect("a link pill");
    assert_eq!(glyph, "\u{26d3}", "a fresh pane reads linked");
    h.mouse_click(pill.center());
    h.frame(); // the popup's debut frame only registers it
    let popover = h.pill_popover().expect("the popover opened");
    assert_eq!(popover.rows.len(), 2, "the follow / unlink pair");
    assert!(
        popover.rows[0].2 && !popover.rows[1].2,
        "the linked state must read selected"
    );
    // The shared honesty sentence — `ui_pills::UNLINK_NOTE`, the inspector
    // checkbox's own hover — is on screen with the choice.
    assert!(
        h.painted_text_strings()
            .iter()
            .any(|t| t.contains("still follows new scans")),
        "the popover must carry the honest unlink caption"
    );

    h.mouse_click(popover.rows[1].1.center());
    h.warm_up();
    assert!(
        !h.gui_mut().pane(0).expect("pane 0").time_link,
        "the pick did not unlink the pane"
    );
    let (glyph, _) = h.pill(0, PillKind::Link).expect("still drawn");
    assert_eq!(
        glyph, "\u{2297}",
        "the pill must reflect the unlinked state"
    );
}

/// 73g. **The kind pill's popover converts through the deferred applier —
///      pending on the pick frame, converted the next — and choosing an
///      unaimed cross-section arms the draw, matching the inspector.**
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

    // 3D Volume: recorded as pending on the pick frame, applied the next.
    h.mouse_click(popover.rows[1].1.center());
    assert_eq!(
        h.gui_mut().pending_pane_kind_for_test(),
        Some((0, PaneKind::Volume)),
        "the pick must go through the deferred applier"
    );
    assert_eq!(
        h.pane_kinds()[0],
        PaneKind::Map,
        "…and not convert mid-frame"
    );
    h.frame();
    assert_eq!(
        h.pane_kinds()[0],
        PaneKind::Volume,
        "the applier must convert on the next frame"
    );
    let (label, _) = h.pill(0, PillKind::Kind).expect("still drawn");
    assert_eq!(label, "3D Volume", "the pill must follow the conversion");
    assert!(
        h.pill(0, PillKind::Tilt).is_none(),
        "a non-map pane offers no tilt pill"
    );

    // Cross-section with no line: the pick arms the draw, as the
    // inspector's segmented row does.
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
    assert_eq!(h.pane_kinds()[0], PaneKind::CrossSection);
    assert!(
        h.section_draw_armed(),
        "choosing an unaimed cross-section must arm the draw"
    );
}

/// **The armed-tool hint chip sits on the active map pane, and only there.**
///
/// While Region or X-sec is armed, the active map pane paints the centred
/// dashed chip naming the drag — painter only, so it is asserted through
/// the painted text. It follows the active pane, swaps wording with the
/// armed mode, and vanishes with the arm — and a non-map active pane gets
/// none, because the drag it explains only exists on a map.
#[test]
fn the_armed_hint_chip_follows_the_active_map_pane() {
    let mut h = pill_harness();
    let panes = h.pane_rects();

    // Arm the region drag the user's way: the top bar toggle.
    let (region_toggle, armed) = h.top_bar().region_arm;
    assert!(!armed, "precondition: the region drag starts unarmed");
    h.mouse_click(region_toggle.center());
    h.warm_up();

    let hint = crate::ui::map::region_arm_hint();
    assert!(
        h.text_painted_in(panes[0], &hint),
        "the active map pane must paint the region hint; painted {:?}",
        h.painted_text_strings_in(panes[0])
    );
    assert!(
        !h.text_painted_in(panes[1], &hint),
        "an inactive pane must not paint the chip"
    );

    // The chip follows the active pane.
    h.mouse_click(panes[1].center());
    h.warm_up();
    assert!(!h.text_painted_in(panes[0], &hint));
    assert!(h.text_painted_in(panes[1], &hint));

    // Arming the section swaps the wording — the two arms are mutually
    // exclusive, so exactly one chip text exists at a time.
    let (section_toggle, _) = h.top_bar().section_arm;
    h.mouse_click(section_toggle.center());
    h.warm_up();
    assert!(
        h.text_painted_in(panes[1], crate::ui::map::SECTION_ARM_HINT),
        "the section arm must paint its own hint"
    );
    assert!(
        !h.text_painted_in(panes[1], &hint),
        "the region hint must go with the region arm"
    );

    // Disarming takes the chip with it.
    let (section_toggle, _) = h.top_bar().section_arm;
    h.mouse_click(section_toggle.center());
    h.warm_up();
    assert!(
        !h.text_painted_in(panes[1], crate::ui::map::SECTION_ARM_HINT),
        "the chip must vanish when the arm does"
    );

    // A non-map active pane paints none: the drag the chip explains needs a
    // projector, and the pane has none.
    h.make_pane_volume(1);
    h.mouse_click(region_toggle.center());
    h.warm_up();
    assert!(
        !h.text_painted_in(panes[1], &hint),
        "a volume pane must not promise a drag it cannot host"
    );
}

/// **The `click_consumed` probe: a feature that answers a map click sets it;
/// a click on bare map does not.**
///
/// The consumption half of the fade trigger (`ui_fade.rs`), plumbed so
/// every consumer inherits the convention — asserted through the radar-site icon, the
/// consumer a test can stage without overlay data.
#[test]
fn a_consumed_map_click_reports_itself_and_a_bare_one_does_not() {
    let mut h = InputHarness::new();
    h.close_layers();
    h.gui_mut().enable_overlay_for_test(OverlayKind::RadarSites);
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

    h.set_overlay_on_pane(0, OverlayKind::RadarSites, false);
    h.mouse_click(spot);
    assert_eq!(site_switches(&h), vec![], "control: nothing answers now");
    assert!(
        !h.click_consumed(),
        "a click that fell through to the bare map must not read as consumed"
    );
}

/// **Saving a user preset under a built-in's name is refused, with the
/// reason inline** (§5.9 carried from the M4 review).
///
/// A user "Severe Wx" would put two identical tiles on screen with only one
/// deletable. The refusal disables Save and says why; nothing is stored.
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

/// **The save tile hides while the search is filtering** (§5.9 pinned rule):
/// the search is for finding tiles, and a save offer matching the query
/// would be the one tile that is not a result. The open name editor hides
/// with it.
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

/// **Applying a preset queues at most one fetch per overlay kind** (§5.9
/// pinned rule): the handlers are global, so one fetch serves every pane
/// the preset enabled a layer on — four panes must not mean four downloads.
#[test]
fn a_preset_apply_queues_one_fetch_per_kind() {
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    h.open_catalog();
    let tile = h
        .catalog_tile(crate::ui::CatalogGroup::Presets, "Severe Wx")
        .expect("the built-in is offered");
    h.mouse_click(tile.rect.center());

    let mut fetched: Vec<OverlayKind> = Vec::new();
    for action in h.last_actions() {
        if let GuiAction::FetchOverlay { kind, .. } = action {
            assert!(
                !fetched.contains(kind),
                "{kind:?} was fetched twice by one preset apply"
            );
            fetched.push(*kind);
        }
    }
    assert!(
        fetched.contains(&OverlayKind::SpcOutlook),
        "control: the preset enables a dataless, never-polled layer, so \
         exactly one fetch for it must be queued; got {fetched:?}"
    );
    assert_eq!(h.pane_count(), 4, "control: the preset really fanned out");
}

/// **A mid-session pane growth leaves the open stack above every pill row.**
///
/// egui auto-tops every area on its debut frame (`!visible_last_frame`), so
/// the rows a pane-count growth debuts — the top bar's Panes segment here;
/// a preset apply and a drawn section line grow the grid the same way —
/// would land above the open panels if the pills pass' raise were a spent
/// one-shot, and stay there until the user happened to click the panel. The
/// pass re-arms its deferred raise on every debut instead (`ui_pills.rs`'s
/// stacking note). Asserted through `layer_id_at` — the authority every
/// click resolver consults — at points where the stack and a row really
/// overlap.
#[test]
fn a_pane_growth_keeps_the_open_stack_above_the_pill_rows() {
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
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

    // Grow the grid under the open stack, the user's way.
    let four = h
        .pane_options()
        .iter()
        .find(|o| o.count == 4)
        .expect("the Panes segment offers 4")
        .rect;
    h.mouse_click(four.center());
    h.warm_up();
    assert_eq!(h.pane_count(), 4, "precondition: the grid really grew");

    // Re-read rather than reused: the growth reflows nothing about the
    // stack today, but the claim is about where it stands *now*.
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
    // The loop must not have passed vacuously: the debuting bottom-left
    // pane's row lands under the stack's lower half in this layout.
    let row2 = h.pill_row(2).expect("pane 2 draws a pill row").rect;
    assert!(
        stack.intersect(row2).is_positive(),
        "precondition: pane 2's debuting row overlaps the stack — without \
         this the loop above asserted nothing about a debut"
    );
}

/// **The same growth leaves the open inspector above the debuting row.**
///
/// The stack test's twin for the other panel the debut would sink: at
/// 1020pt — barely Expanded — the inspector's left edge reaches past the
/// map's midline, so the second pane's debuting row lands under it.
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

/// **Saving under an existing user preset's name replaces it, whatever the
/// casing** — the same case-insensitivity the built-in refusal keeps, and
/// for the same reason: "storm" and "Storm" would be two tiles a glance
/// cannot tell apart. The replacement takes the newly typed casing.
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

// ── The phone shell: bottom bar and sheet ────────────────────────────

/// A phone-sized harness: the Compact shell with the bottom bar and the
/// sheet. Tall, like the drawer fixture, so sheet pages have room to lay
/// their content out on screen.
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
/// sheet's Feature page without staging a real alert under a map click. The
/// concrete items are `pub(crate)` to `rustdar-overlays`; the trait is not.
#[derive(Debug)]
struct SheetStubFeature;

impl rustdar_overlays::render::overlay_state::OverlayItem for SheetStubFeature {
    fn kind(&self) -> OverlayKind {
        OverlayKind::NwsAlerts
    }
    fn popup_content(
        &self,
        _prefs: &rustdar_units::UserPreferences,
    ) -> rustdar_overlays::render::overlay_state::PopupContent {
        rustdar_overlays::render::overlay_state::PopupContent {
            title: "Stub feature".to_owned(),
            accent_rgb: [200, 60, 60],
            width: 300.0,
            sections: vec![rustdar_overlays::render::overlay_state::PopupSection::Text(
                "stub body".to_owned(),
            )],
            actions: Vec::new(),
        }
    }
    fn matches(&self, _other: &dyn rustdar_overlays::render::overlay_state::OverlayItem) -> bool {
        false
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

/// 64. **The bottom bar's items toggle their own page and switch between
///     pages.**
///
///     Tapping the item whose page is on top clears that page's flag — the
///     sheet pops to whatever is beneath, or closes — and tapping a
///     different item switches to its page. Pane and App are both the
///     Inspector page and differ only in the selection they assert, so
///     switching between them changes the body without closing the sheet.
#[test]
fn the_bottom_bar_toggles_its_pages_and_switches_between_them() {
    let mut h = phone();
    assert_eq!(h.sheet().page, None, "a fresh session's sheet is closed");

    // Layers opens its page, highlighted.
    h.mouse_click(h.bottom_bar().layers.0.center());
    h.warm_up();
    assert_eq!(h.sheet().page, Some(crate::ui::SheetPage::Layers));
    assert!(
        h.bottom_bar().layers.1,
        "the open page's item must highlight"
    );

    // A different item switches pages; the Layers flag stays set beneath.
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

    // App is the same page under a different selection: the body switches,
    // the sheet stays.
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

    // The same item again closes its page — popping to the Layers page the
    // switch left open beneath.
    h.mouse_click(h.bottom_bar().app.0.center());
    h.warm_up();
    assert_eq!(
        h.sheet().page,
        Some(crate::ui::SheetPage::Layers),
        "closing the top page must reveal the one beneath, not the map"
    );

    // ...and closing the last page closes the sheet.
    h.mouse_click(h.bottom_bar().layers.0.center());
    h.warm_up();
    assert_eq!(
        h.sheet().page,
        None,
        "the last page's toggle closes the sheet"
    );

    // The Menu item follows the same toggle contract.
    h.mouse_click(h.bottom_bar().menu.0.center());
    h.warm_up();
    assert_eq!(h.sheet().page, Some(crate::ui::SheetPage::Menu));
    assert!(h.bottom_bar().menu.1);
    h.mouse_click(h.bottom_bar().menu.0.center());
    h.warm_up();
    assert_eq!(h.sheet().page, None, "the Menu item's second tap closes it");
}

/// 71. **Dialogs are modals at ≥600pt and sheet pages below it — the phone
///     never draws a modal.**
///
///     One flag, two presentations (plan §1.9): `catalog_open` is an
///     `egui::Modal` on the desktop and the sheet's full-height Catalog
///     page on the phone; `time_dialog.show` is a window there and the Time
///     page here; a selected feature is the pager window there and the
///     Feature page here. The modal-absence half is read off egui's own
///     area bookkeeping: a fresh phone session that never drew the modal
///     has no state under its id to have drawn it with.
#[test]
fn dialogs_are_modals_on_wide_screens_and_sheet_pages_on_the_phone() {
    // Desktop: the catalog is a modal, and no sheet exists to host it.
    let mut desk = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    desk.open_catalog();
    assert_eq!(desk.sheet().page, None, "no sheet on a desktop");
    assert!(
        desk.area_rect(egui::Id::new("add_layer_catalog")).is_some(),
        "the desktop catalog must be the egui Modal"
    );
    // Backdrop click closes — the modal's own contract.
    desk.mouse_click(egui::pos2(40.0, 400.0));
    desk.warm_up();
    assert!(
        !desk.catalog().open,
        "the modal's backdrop click must close it"
    );

    // Phone: the same flag presents as the sheet's Catalog page, at forced
    // Full extent, and the Modal is never created.
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

    // The scrim is the backdrop: a click above the sheet closes the top
    // page, revealing the Layers page the catalog was opened from.
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

    // The Time dialog: the timeline's timestamp opens the Time page, and
    // the phone never draws the Set Time window.
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

    // A selected feature: the Feature page, never the pager window.
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

/// 75. **The phone top bar shares the status bar's collapse state:
///     collapsed, only the wordmark and the restore button remain.**
///
///     One field (`statusbar_collapsed`), two bars by width — §1.6's rule:
///     the phone has no status bar, so the collapse the ◧ means lives on
///     the bar that carries the scan text. Crossing the breakpoint carries
///     the state across, in both directions, because it is the same state.
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
        !collapsed.section_arm.0.is_positive() && !collapsed.region_arm.0.is_positive(),
        "the collapsed bar still drew the arm toggles"
    );
    assert!(
        h.text_painted_in(collapsed.rect, "RUST"),
        "the wordmark must survive the collapse"
    );
    assert!(
        !h.text_painted_in(collapsed.rect, "KABR"),
        "the scan text was still painted while collapsed"
    );

    // The state is the status bar's: widen past the breakpoint and the
    // status bar comes back collapsed.
    h.set_screen(egui::vec2(1400.0, 900.0));
    assert!(
        h.status_bar().collapsed,
        "the phone bar's collapse did not reach the status bar it shares \
         state with"
    );

    // ...and the restore crosses back the other way.
    h.mouse_click(h.status_bar().collapse.center());
    h.warm_up();
    assert!(!h.status_bar().collapsed, "precondition: restored");
    h.set_screen(egui::vec2(420.0, 1400.0));
    assert!(
        !h.top_bar().scan_text.is_empty(),
        "the status bar's restore did not reach the phone bar"
    );
}

/// **The sheet's handle snaps Half ↔ Full, and a deep drag-down dismisses**
/// (plan §1.13): the release decides what the drag meant — past the midpoint
/// towards Full snaps Full, back below it snaps Half, and a release more
/// than a quarter below the Half height clears every page flag.
#[test]
fn the_sheet_handle_snaps_between_half_full_and_dismissal() {
    let mut h = phone();
    h.open_layers();
    assert_eq!(h.sheet().extent, crate::ui::SheetExtent::Half);
    let half_height = h.sheet_rect().expect("open").height();

    // Up, well past the midpoint: Full.
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

    // Down, back below the midpoint but above the dismiss band: Half.
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

    // Down again, deep into the dismiss band: the sheet goes, flags and all.
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

/// **A back press walks the phone sheet pages top-down, one visible pop per
/// press** (plan §3.4; scope item 7): Feature → Time → Menu → Inspector →
/// Layers → the armed drag — the projection order, driven through the same
/// `dismiss_top_layer` entry every width shares. Below the breakpoint the
/// dismissal *is* the projection: it pops whichever page the sheet shows on
/// top, whatever order the flags were stacked in. The second leg builds the
/// stack a fixed chain would mis-order — a Feature page over an open
/// Catalog, through a real route: flags set on a wider width and carried
/// under 600 pt by a resize (a feature tap through the scrim's map slivers
/// builds the same state without leaving the phone) — and requires the
/// visible page to pop first, the invisible flag to stay.
#[test]
fn a_back_press_walks_the_phone_sheet_pages_top_down() {
    let mut h = phone();
    h.set_drawer_open(true);
    h.gui_mut().open_settings();
    h.gui_mut().set_sheet_menu_open_for_test(true);
    h.gui_mut().set_time_dialog_open_for_test(true);
    h.gui_mut().overlays.selected_overlays = vec![std::sync::Arc::new(SheetStubFeature)];
    h.set_region_arm(true);
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
    // Closing the last page closes the sheet...
    walk(&mut h, None);
    // ...and only then does the press reach the armed drag, then the exit.
    assert!(h.gui_mut().dismiss_top_layer(), "the armed drag is below");
    assert!(!h.region_arm(), "the press must disarm the region drag");
    assert!(
        !h.gui_mut().dismiss_top_layer(),
        "nothing is left; the next press belongs to the exit path"
    );

    // The stacked state a fixed chain would mis-order: a Feature page over
    // an open Catalog, built on the desktop — a feature window up, the
    // stack's + Add layer — and carried under the breakpoint by a resize.
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

    // The Catalog page's own pop, from the state the phone's routes produce.
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

/// **Stack rows carry a trailing › on the drawer and sheet hosts, and none
/// on the desktop sidebar** (plan §1.3): where a row click pushes the
/// inspector *over* the list, the chevron says so; where the inspector opens
/// beside it, there is nothing to push.
#[test]
fn stack_rows_carry_a_chevron_only_in_the_drawer_and_sheet_hosts() {
    let desk = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    let row = desk
        .stack_row(OverlayKind::NwsAlerts)
        .expect("the desktop sidebar is open by default");
    assert_eq!(
        row.chevron, None,
        "a desktop sidebar row grew a chevron it has nothing to push for"
    );

    let mut tablet = InputHarness::with_screen(egui::vec2(800.0, 1200.0));
    tablet.open_layers();
    let row = tablet
        .stack_row(OverlayKind::NwsAlerts)
        .expect("the drawer is open");
    assert!(
        row.chevron.is_some_and(|c| c.is_positive()),
        "a drawer row must carry the chevron"
    );

    let mut ph = phone();
    ph.open_layers();
    let row = ph
        .stack_row(OverlayKind::NwsAlerts)
        .expect("the sheet's Layers page is open");
    let chevron = row.chevron.expect("a sheet row must carry the chevron");
    assert!(
        ph.sheet_rect().expect("open").contains(chevron.center()),
        "the chevron must be drawn inside the sheet"
    );
}

/// **The phone bar's hover readout never paints over the arm toggles.** The
/// readout is the one unbounded string the bar hosts (contract 25 puts it
/// here whenever a mouse drives), and the ⬚/╱ toggles own the right edge —
/// so a long value must truncate at the width they left, not extend across
/// them: the module note's overlap rule, in its truncation form.
#[test]
fn the_phone_hover_readout_never_paints_over_the_arm_toggles() {
    let mut h = phone();
    h.mouse_move(h.map_center());
    h.warm_up();
    assert!(
        h.top_bar().hover,
        "precondition: mouse modality, or the bar hosts no readout"
    );

    // A readout far wider than the whole screen, let alone the bar's run.
    let long = format!("READOUT {}", "far too long ".repeat(40));
    h.gui_mut().pane_mut(0).unwrap().hover_value = Some(long.clone());
    h.frame();

    let bar = h.top_bar();
    let (readout, _) = {
        let rects = h.painted_text_rects();
        rects
            .iter()
            .find(|(_, text)| text.starts_with("READOUT"))
            .cloned()
            .expect("precondition: the readout must be on the glass")
    };
    assert!(
        !readout.intersects(bar.region_arm.0) && !readout.intersects(bar.section_arm.0),
        "the hover readout at {readout:?} paints over the arm toggles at \
         {:?} / {:?}",
        bar.region_arm.0,
        bar.section_arm.0
    );
    assert!(
        readout.right() <= bar.region_arm.0.left(),
        "the readout must end where the toggles' run begins"
    );

    // ...and the toggles stay clickable under the same readout.
    h.gui_mut().pane_mut(0).unwrap().hover_value = Some(long);
    h.mouse_click(bar.region_arm.0.center());
    h.warm_up();
    assert!(
        h.region_arm(),
        "the \u{2b1a} toggle under a long readout did not take the click"
    );
}

/// **The phone error toast sits under the top bar, clear of the arm
/// toggles, and its ✕ dismisses** — the status bar's error contract, moved
/// to the one chrome strip the phone keeps at the top.
#[test]
fn the_phone_error_toast_sits_under_the_top_bar_and_its_cross_dismisses() {
    let mut h = phone();
    h.gui_mut().set_error("the feed went away".to_owned());
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
        !toast.rect.intersects(bar.region_arm.0) && !toast.rect.intersects(bar.section_arm.0),
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

/// **The phone error toast stays visible and dismissible while a sheet page
/// is up.** The scrim and sheet are `Order::Foreground`; the toast rides
/// `Order::Tooltip` above them (see `render_phone_error_toast` for why that
/// device) — an error surface a page could bury would go unseen exactly
/// when the user is busiest.
#[test]
fn the_phone_error_toast_stays_visible_and_dismissible_over_an_open_sheet() {
    let mut h = phone();
    h.open_catalog();
    h.gui_mut().set_error("the feed went away".to_owned());
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

/// **A release on the forced-Full Catalog page keeps the stored snap.** The
/// page draws at Full whatever the snap says (plan §1.10), so a settle drag
/// there decides nothing — writing Full over the user's Half would change
/// how every later page opens. Dismiss-by-drag still works from it.
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

    // A small settle drag, released well above the dismiss band — where a
    // snap write would have recorded the forced Full.
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

    // Pop the catalog: the Layers page beneath comes back at the snap the
    // user chose, not at the Full the forced page drew at.
    assert!(h.gui_mut().dismiss_top_layer());
    h.warm_up();
    assert_eq!(h.sheet().page, Some(crate::ui::SheetPage::Layers));
    assert_eq!(
        h.sheet().extent,
        crate::ui::SheetExtent::Half,
        "the forced-Full release must not overwrite the stored snap"
    );
}

/// **Arming ⬚/╱ from the phone top bar closes the open sheet** — the Menu
/// page's own rule for its two arm entries, applied to the bar's route: the
/// next thing the user does is a drag on the map the sheet is covering.
/// Disarming closes nothing, as the dropdown's reasoning goes.
#[test]
fn arming_from_the_phone_top_bar_closes_the_open_sheet() {
    let mut h = phone();
    h.open_layers();
    assert_eq!(h.sheet().page, Some(crate::ui::SheetPage::Layers));

    let (region, armed) = h.top_bar().region_arm;
    assert!(!armed, "precondition: the mode starts disarmed");
    h.mouse_click(region.center());
    h.warm_up();
    assert!(h.region_arm(), "the tap must arm the drag");
    assert_eq!(
        h.sheet().page,
        None,
        "arming needs the map: the sheet must close with it"
    );

    // Disarming keeps whatever is up.
    h.open_layers();
    let (region, armed) = h.top_bar().region_arm;
    assert!(armed, "precondition: still armed across the reopen");
    h.mouse_click(region.center());
    h.warm_up();
    assert!(!h.region_arm(), "the second tap must disarm");
    assert_eq!(
        h.sheet().page,
        Some(crate::ui::SheetPage::Layers),
        "disarming closes nothing"
    );
}

// ── The UI fade and the finalized Esc chain (M7) ─────────────────────

/// Whether the floating chrome is on the glass, read off the probes the
/// renderers write — the timeline and the status bar on the wide widths, the
/// timeline and the bottom bar on the phone. One reader for every fade
/// contract, so "faded" and "restored" are the same claim throughout.
fn chrome_on_screen(h: &InputHarness) -> bool {
    let timeline = h.timeline();
    let timeline_drawn = timeline.rect != egui::Rect::NOTHING || timeline.collapsed;
    if h.width_class() == crate::ui_layout::WidthClass::Compact {
        timeline_drawn && h.bottom_bar().rect != egui::Rect::NOTHING
    } else {
        timeline_drawn && h.status_bar().rect != egui::Rect::NOTHING
    }
}

/// 60. **A qualifying tap fades all the floating chrome; the second restores
///     it; a drag, a consumed click and an armed tool do not fade.**
///
///     The trigger sentence of §1.8, condition by condition, on the width
///     with the most chrome to lose. The qualifying click is a confirmed
///     click (not a drag) on the already-active pane's bare map — no feature
///     or site under it, no armed drag owning the gesture.
#[test]
fn a_qualifying_tap_fades_the_chrome_and_the_second_restores_it() {
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    let spot = h.map_center();
    assert!(
        chrome_on_screen(&h) && !h.pill_rows().is_empty(),
        "precondition: the chrome is up"
    );

    // A drag does not fade: ≥ the click threshold of movement makes the
    // gesture a pan, and a pan is map work.
    h.mouse_press(spot);
    for i in 1..=4 {
        h.mouse_move(spot + egui::vec2(8.0 * i as f32, 0.0));
        h.frame_after(FRAME_DT);
    }
    h.mouse_release(spot + egui::vec2(32.0, 0.0));
    h.warm_up();
    assert!(!h.faded() && chrome_on_screen(&h), "a drag must not fade");

    // An armed tool does not fade: the click is the tool's (a discarded
    // too-short gesture), whichever tool is armed.
    h.set_section_draw_armed(true);
    h.mouse_click(spot);
    h.warm_up();
    assert!(
        !h.faded() && chrome_on_screen(&h),
        "an armed draw must not fade"
    );
    h.set_section_draw_armed(false);
    h.set_region_arm(true);
    h.mouse_click(spot);
    h.warm_up();
    assert!(
        !h.faded() && chrome_on_screen(&h),
        "an armed region must not fade"
    );
    h.set_region_arm(false);

    // A consumed click does not fade: the site icon answered it.
    h.close_layers();
    h.gui_mut().enable_overlay_for_test(OverlayKind::RadarSites);
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

    // The qualifying tap fades everything floating; the docked top bar
    // stays (contract 63 pins its half).
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

    // The second qualifying tap restores.
    h.mouse_click(spot);
    h.warm_up();
    assert!(!h.faded(), "the second tap must restore");
    assert!(
        chrome_on_screen(&h) && !h.pill_rows().is_empty(),
        "the chrome must be back"
    );
}

/// 60b. **The same trigger on the phone: the bottom cluster fades and the
///      second tap restores it — and an armed tool still does not fade.**
#[test]
fn a_qualifying_tap_fades_the_phone_cluster_and_the_second_restores_it() {
    let mut h = phone();
    let spot = h.pane_rects()[0].center();
    assert!(chrome_on_screen(&h), "precondition: the cluster is up");

    h.set_region_arm(true);
    h.mouse_click(spot);
    h.warm_up();
    assert!(
        !h.faded() && chrome_on_screen(&h),
        "an armed region must not fade"
    );
    h.set_region_arm(false);

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

/// 61. **Fading closes the panels and the sheet for real — state, not
///     paint — and unfading reopens nothing.**
///
///     The state half is read through `dismiss_top_layer`: while faded the
///     only consumable layer is the fade itself, and after it nothing is
///     left — an invisible open panel would answer the second press.
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
    // State, not paint: the fade is the one consumable layer, and nothing
    // hides beneath it.
    assert!(h.gui_mut().dismiss_top_layer(), "the fade itself");
    assert!(
        !h.gui_mut().dismiss_top_layer(),
        "something stayed open invisibly under the fade"
    );
    h.warm_up();

    // Unfading reopens nothing: the panels stay closed until asked for.
    assert!(
        !h.layers_panel_on_screen() && !h.inspector().open,
        "unfading must not reopen the panels"
    );
    assert!(chrome_on_screen(&h), "the unconditional chrome is back");
}

/// 61b. **On the phone the fade closes the sheet for real — through the map
///      sliver the scrim leaves beside the bottom bar.**
///
///      The scrim covers the map above the sheet, so the one place a map tap
///      can land with a page open is the sliver band by the bottom bar
///      (`ui_sheet.rs`'s own note). That tap is the fade gesture: pages
///      closed in state, cluster gone, nothing left under the fade.
#[test]
fn a_sliver_tap_fades_and_closes_the_sheet_for_real() {
    let mut h = phone();
    h.open_layers();
    let sheet_bottom = h.sheet_rect().expect("the Layers page is open").bottom();
    let bar_top = h.bottom_bar().rect.top();
    assert!(
        sheet_bottom < bar_top,
        "precondition: a sliver exists between the sheet and the bar"
    );
    let sliver = egui::pos2(
        h.pane_rects()[0].left() + 3.0,
        (sheet_bottom + bar_top) / 2.0,
    );
    assert!(
        !h.is_floating_layer_at(sliver),
        "precondition: the sliver is bare map, not scrim or bar"
    );

    h.mouse_click(sliver);
    h.warm_up();
    assert!(h.faded(), "the sliver tap must fade");
    assert_eq!(h.sheet().page, None, "the sheet must close in state");
    assert!(h.gui_mut().dismiss_top_layer(), "the fade itself");
    assert!(
        !h.gui_mut().dismiss_top_layer(),
        "a page flag survived the fade invisibly"
    );
}

/// 61c. **The fade closes the Volume Alpha editor for real — per-pane
///      floating chrome, on the same terms as the panels (§1.8) — and
///      unfading does not reopen it.**
#[test]
fn the_fade_closes_the_volume_alpha_editor_for_real() {
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    h.set_pane_count(2);
    h.make_pane_volume(1);

    // Open the editor through its own corner button on the 3D pane.
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

    // The qualifying fade tap on the active map pane: the first click on
    // pane 0 only activates it (§1.8), the second fades.
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

    // Unfading reopens nothing, the editor included.
    h.mouse_click(spot);
    h.warm_up();
    assert!(!h.faded(), "the second tap restores");
    assert!(!editor_open(&mut h), "unfading must not reopen the editor");
}

/// 62. **A top-bar interaction while faded unfades first, then performs —
///     nothing opens invisibly.**
#[test]
fn a_top_bar_interaction_while_faded_unfades_and_performs() {
    // Wide: the ☰ opens the menu into a restored UI.
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

    // Phone: the ◧ collapse unfades and collapses.
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

/// 62b. **A keyboard activation while faded unfades too — one frame later,
///      through the invariant's repair, and the surface it opened stays.**
///
///      egui's Tab-focus plus Enter activates a bar control with no pointer
///      event, so the spatial unfade guard never sees it (`ui_fade.rs`). The
///      frame-top invariant catches the opened surface and repairs by
///      unfading — the §3.6 answer, one frame late — rather than re-closing,
///      which would make the toggle read as dead to exactly the user who
///      cannot aim a pointer at it.
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

    // Focus the Layers toggle under the id the bar itself keyed, then press
    // Enter: an activation with no pointer event anywhere near the bar.
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
    // Wide: a pane-count segment still takes its click.
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

    // Phone: the ╱ arm still takes its tap.
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

/// 65. **The full Esc/back order, fade included: fade → catalog → feature →
///     time → inspector → drawer → armed drag, one layer per press.**
///
///     The ☰ dropdown's place at the chain's head is contract 83's own
///     claim (its route cannot be stacked under the catalog's modal
///     backdrop); the in-flight handle drag above everything is the section
///     suite's. This walks everything between, on the width whose stack
///     form (the drawer) is an Esc target, with the fade at its head — and
///     the fade leg runs first, because fading *closes* the very layers the
///     walk stacks.
#[test]
fn a_back_press_walks_the_full_wide_chain_in_order() {
    let mut h = InputHarness::with_screen(egui::vec2(800.0, 1200.0));
    assert_eq!(h.width_class(), crate::ui_layout::WidthClass::Medium);

    // The fade head: Esc while faded restores the UI and consumes the press.
    h.mouse_click(h.map_center());
    h.warm_up();
    assert!(h.faded(), "precondition: faded");
    assert!(h.gui_mut().dismiss_top_layer(), "the press unfades");
    assert!(!h.faded());
    h.warm_up();
    assert!(chrome_on_screen(&h), "Esc means restore my UI");

    // The stack under the fade, deepest first.
    h.set_region_arm(true);
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
    assert!(!h.region_arm(), "press 6 disarms");

    assert!(
        !h.gui_mut().dismiss_top_layer(),
        "press 7 falls through to the exit path"
    );
}

/// 65b. **The Compact chain keeps its projection-first order with the fade
///      at its head** — the fade leg, then the sheet walk of
///      `a_back_press_walks_the_phone_sheet_pages_top_down`, abbreviated to
///      the seam this contract adds.
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

    // The projection resumes beneath: pages pop top-down as ever.
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

/// **A click that dismisses an open popover does not fade** — the popup was
/// what the click was aimed at (egui closes it on the click outside), and
/// the evidence is recorded at press time because the popup is gone by the
/// confirm frame.
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

    // The next bare click, with nothing open, does fade.
    h.mouse_click(h.map_center());
    h.warm_up();
    assert!(h.faded(), "the follow-up click is the fade gesture");
}

/// **A first click on an inactive pane only activates — the fade needs a
/// click on the *already*-active pane** (§1.8).
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

    // The same spot again — now the already-active pane — fades.
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
    h.gui_mut().enable_overlay_for_test(OverlayKind::RadarSites);
    h.warm_up();
    let spot = h.map_center();
    // Park the icon away from the centre first — the default view has the
    // pane's own site under the centre, and the fading click below must be
    // a bare-map click.
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
///
/// Driven through the shipped `TouchGestures::update`, whose output the
/// harness's parallel touch probe is. The control half holds the same press
/// on bare map, so the filter — not a dead detector — is what the assertion
/// sees.
#[test]
fn a_long_press_on_floating_chrome_raises_no_map_tooltip() {
    let mut h = InputHarness::new();

    // Control: a still hold on bare map is a long press.
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

    // The same hold on the floating timeline is a hold on the timeline.
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
/// same-auto-id-slot corner): the two forms share one row slot, so without
/// distinct ids a loop landing mid-drag would hand an archive drag to the
/// frame-seek slider — same id, new meaning.
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
        pane.loop_state = crate::pane::LoopPlaybackState::new_for_loop(
            600,
            rustdar_radar::sites::get_radar_site("KTLX").unwrap(),
        );
        pane.loop_state.frames = vec![crate::pane::LoopFrame {
            timestamp: chrono::Utc::now().naive_utc(),
            texture: None,
            render_in_flight: false,
            render_failed: false,
        }];
        pane.loop_state.current_frame = 0;
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

/// **The transport's stated width is its outer width** (§1.5's
/// `min(880, full − 24)`, the §5.9 bookkeeping fix): the surface on the
/// glass, frame included, lands on the formula — not the formula plus the
/// frame's margins.
#[test]
fn the_transport_outer_width_is_the_stated_formula() {
    // Narrow enough that `full − 24` is the binding arm.
    let h = InputHarness::with_screen(egui::vec2(800.0, 1200.0));
    let map = h.map_panel_rect();
    let expected = (map.width() - 24.0).min(880.0);
    let drawn = h.timeline().rect.width();
    assert!(
        (drawn - expected).abs() < 1.0,
        "the transport drew {drawn} pt wide; §1.5 states {expected} pt"
    );

    // ...and wide enough that 880 binds.
    let h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    let drawn = h.timeline().rect.width();
    assert!(
        (drawn - 880.0).abs() < 1.0,
        "the transport drew {drawn} pt wide; §1.5 caps it at 880"
    );
}

/// **The sheet host draws no duplicate headers** (M7's sheet-header polish):
/// the sheet's title row is the single header — the stack's own header row
/// and ⟨ do not render there, the inspector's crumb keeps its ✕-deselect
/// (selection, not navigation) but drops its ⟩ — while the wider hosts keep
/// all of it.
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

    // The inspector page: crumb yes, ⟩ no, ✕-deselect yes.
    h.open_layer_in_inspector(OverlayKind::NwsAlerts);
    let insp = h.inspector();
    assert_eq!(
        insp.collapse,
        egui::Rect::NOTHING,
        "the ⟩ collapse is the back-chain's job in the sheet"
    );
    assert_ne!(
        insp.deselect,
        egui::Rect::NOTHING,
        "the crumb's ✕-deselect stays: it is selection, not navigation"
    );

    // The wider hosts keep both header rows whole.
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    h.gui_mut().open_settings();
    h.warm_up();
    assert_ne!(h.stack().header, egui::Rect::NOTHING);
    assert_ne!(h.stack().collapse, egui::Rect::NOTHING);
    assert_ne!(h.inspector().collapse, egui::Rect::NOTHING);
    assert!(
        !h.text_painted_in(h.screen_rect(), "The same layer stack as on a desktop"),
        "the helper caption is the phone page's alone"
    );
}

/// **The error surface outranks the fade** — the deliberate §1.8 refinement
/// recorded in `ui_fade.rs`: an error one accidental tap could hide is an
/// error unseen. On the wide widths the error normally rides in the status
/// bar, which fades — so while faded the toast presentation carries it; on
/// the phone the toast simply stays.
#[test]
fn the_error_surface_stays_visible_while_faded() {
    // Wide: the status bar goes, the toast carries the error.
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    h.gui_mut().set_error("the feed went away".to_owned());
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

    // Phone: the toast stays up through the fade.
    let mut h = phone();
    h.gui_mut().set_error("the feed went away".to_owned());
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

// ---------------------------------------------------------------------------
// M8: the first-run fixes, each pinned headlessly. The glyph inventory and
// the UI-string allowlist live in `ui_glyphs.rs`; the selectable-labels rule
// is pinned at its frontend site. Everything else is here.
// ---------------------------------------------------------------------------

/// M8-3. **The top bar has breathing room at every width.** The floor is the
/// bar's own stated one — the vertical margins plus one interact row — so
/// dropping either constant back to the cramped strip fails by name here.
/// The full-bleed contract test above already holds the map to whatever
/// height the bar really claims.
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

/// M8-6. **A non-map pane's stack body is the explained absence plus the one
/// action that applies.** No layer rows and no Add-layer buttons (correct —
/// there is no map to layer), but the body must offer the caption and a
/// `Pane properties...` button that opens the inspector where the pane's
/// real controls live — not read as a broken panel.
#[test]
fn a_non_map_stack_body_offers_the_caption_and_pane_properties() {
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    h.make_pane_volume(0);
    h.open_layers();

    let stack = h.stack();
    assert!(stack.rows.is_empty(), "a 3D pane has no layer rows");
    assert_eq!(
        stack.add_top,
        egui::Rect::NOTHING,
        "no Add-layer button: the catalog adds map layers"
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
/// pane.** The active pane's on-screen time, else its own static
/// `data_time`, else the freshest visible map pane's — with the live/archive
/// annotation following whichever pane supplied the time. `--:--:--` only
/// when genuinely nothing is loaded.
#[test]
fn the_time_chip_falls_back_to_a_map_panes_time_on_a_non_map_pane() {
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    assert!(
        h.timeline().timestamp.1.contains("--:--:--"),
        "precondition: a fresh session has no data time anywhere"
    );

    h.set_pane_count(2);
    h.make_pane_volume(1);
    // Pane 0 (a map) has data on screen, parked in the archive; pane 1 (3D)
    // has none of its own.
    let t = chrono::NaiveDate::from_ymd_opt(2026, 8, 10)
        .unwrap()
        .and_hms_opt(11, 11, 18)
        .unwrap();
    {
        let pane0 = h.gui_mut().pane_mut(0).expect("pane 0 exists");
        pane0.data_time = Some(t);
        pane0.viewing_live = false;
    }
    // Make the 3D pane the active one, the user's way.
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

    // The collapsed chip prints the same fallback.
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

/// M8-8/9. **The collapsed chip sits above the bottom-edge bars and lays out
/// on one line.** Anchored off the bars' real rects — the floating status
/// bar on the wide widths, the bottom bar on the phone — and sized to its
/// text, so the time can never fold into a vertical column.
#[test]
fn the_collapsed_chip_clears_the_bars_and_never_wraps() {
    // Wide: above the floating status bar.
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

    // Phone: above the bottom bar.
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

/// The chip's one-line claim, asserted on the glass: wider than tall, and
/// its whole label painted as a single text row.
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
/// Every row spans the panel's inner width at 28pt or more, and a click far
/// from the label text — the row's right end, where only empty row used to
/// be — selects the layer.
#[test]
fn layer_rows_are_full_width_click_targets() {
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
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

    let radar = h
        .stack_row(OverlayKind::Radar)
        .expect("the Radar row is drawn");
    let far_right = egui::pos2(radar.rect.right() - 6.0, radar.rect.center().y);
    h.mouse_click(far_right);
    h.warm_up();
    assert_eq!(
        h.inspector().mode,
        Some(crate::ui::InspectorSelection::Layer(OverlayKind::Radar)),
        "a click at the row's right end must select the layer"
    );

    // The buttons layered on the row keep their own precedence: the eye
    // click toggles visibility rather than re-selecting.
    let row = h.stack_row(OverlayKind::CityLabels).expect("row drawn");
    let was_on = row.eye_on;
    h.mouse_click(row.eye.center());
    h.warm_up();
    assert_eq!(
        h.overlay_enabled(OverlayKind::CityLabels),
        !was_on,
        "the eye stopped toggling under the full-width row"
    );
    assert_ne!(
        h.inspector().mode,
        Some(crate::ui::InspectorSelection::Layer(
            OverlayKind::CityLabels
        )),
        "the eye click leaked into a row selection"
    );
}

/// M8-11. **The fade hides the 3D pane's Volume Alpha corner button.** It is
/// floating chrome over the picture: hidden for real while faded — not
/// drawn, so input-transparent — and back after (the contract-61 family).
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

/// M8-12. **The active-pane border shows all four edges at every grid
/// position.** The stroke paints inside the pane rect, inside the map's
/// content rect — with the outside stroke it shipped with, the outer edges
/// were clipped away entirely (the top-left pane showed no border at all)
/// and only inter-pane edges survived.
#[test]
fn every_pane_border_lies_inside_its_pane_at_every_position() {
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    h.set_pane_count(6);
    h.close_layers();
    let map = h.map_panel_rect();

    for target in 0..6 {
        if h.active_pane_index() != target {
            // The user's route: the first click on an inactive pane
            // activates it (and never fades).
            h.mouse_click(h.pane_rects()[target].center());
            h.warm_up();
        }
        assert_eq!(h.active_pane_index(), target, "activation failed");
        assert!(!h.faded(), "activation must not fade");

        let borders = h.pane_borders();
        assert_eq!(borders.len(), 6, "every pane draws its border");
        let rects = h.pane_rects();
        for &(idx, painted, active) in &borders {
            assert_eq!(
                active,
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

/// M8-13. **The release frame of a handle drag paints the dropped line,
/// never the stale pre-drag one.** The drop records a pending edit that the
/// applier consumes after the pane loop, so without the bridge the release
/// frame painted the old committed geometry - a visible pop-back. (The gap
/// predates the Synthesis rebuild; the drag machinery always committed
/// through the deferred applier.)
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
        // The moved end is whichever cap sits nearer the grab.
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
