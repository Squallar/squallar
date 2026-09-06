use super::*;

/// Drives [`PointerTracker`] directly, one hand-built frame at a time.
struct TrackerDriver {
    ctx: egui::Context,
    tracker: PointerTracker,
    time: f64,
}

impl TrackerDriver {
    fn new() -> Self {
        Self {
            ctx: egui::Context::default(),
            tracker: PointerTracker::default(),
            time: 100.0,
        }
    }

    fn frame(&mut self, events: Vec<egui::Event>) -> PointerFrame {
        let raw_input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(800.0, 600.0),
            )),
            time: Some(self.time),
            events,
            ..Default::default()
        };
        self.ctx.begin_pass(raw_input);
        let frame = self.tracker.read(&self.ctx);
        let _ = self.ctx.end_pass();
        self.time += 1.0 / 60.0;
        frame
    }

    /// Run a frame `seconds` later than the previous one.
    fn frame_after(&mut self, seconds: f64, events: Vec<egui::Event>) -> PointerFrame {
        self.time += seconds;
        self.frame(events)
    }
}

fn button(button: egui::PointerButton, pressed: bool, pos: egui::Pos2) -> egui::Event {
    egui::Event::PointerButton {
        pos,
        button,
        pressed,
        modifiers: egui::Modifiers::default(),
    }
}

fn touch(id: u64, phase: egui::TouchPhase, pos: egui::Pos2) -> egui::Event {
    egui::Event::Touch {
        device_id: egui::TouchDeviceId(0),
        id: egui::TouchId(id),
        phase,
        pos,
        force: None,
    }
}

/// `egui-winit`'s `TouchPhase::Started`: the raw touch, then the emulated pointer
/// (`egui-winit-0.34.1/src/lib.rs:897`).
fn touch_down(id: u64, pos: egui::Pos2) -> Vec<egui::Event> {
    vec![
        touch(id, egui::TouchPhase::Start, pos),
        egui::Event::PointerMoved(pos),
        button(egui::PointerButton::Primary, true, pos),
    ]
}

/// A `PointerGone` is terminal until a press, whichever kind it was.
#[test]
fn an_excursion_is_terminal_until_a_press() {
    let mut d = TrackerDriver::new();
    let pos = egui::pos2(100.0, 100.0);

    assert!(
        d.frame(vec![button(egui::PointerButton::Primary, true, pos)])
            .down
    );
    assert!(!d.frame(vec![egui::Event::PointerGone]).down);

    for step in 1..=4 {
        let moved = d.frame(vec![egui::Event::PointerMoved(
            pos + egui::vec2(4.0 * step as f32, 0.0),
        )]);
        assert!(
            !moved.down,
            "step {step}: motion is not evidence of a held button"
        );
    }

    assert!(
        d.frame(vec![button(egui::PointerButton::Primary, true, pos)])
            .down,
        "a real press is what brings it back"
    );
}

/// A cancellation is terminal. Motion afterwards is some *other* input source — the
/// next finger, a mouse on a hybrid device, `mousemove` on the web — and must never
/// be read as the cancelled finger returning.
#[test]
fn a_cancelled_touch_is_not_revived_by_motion() {
    for motion in [
        egui::Event::PointerMoved(egui::pos2(300.0, 300.0)),
        egui::Event::MouseMoved(egui::vec2(3.0, 3.0)),
    ] {
        let mut d = TrackerDriver::new();
        let pos = egui::pos2(100.0, 100.0);

        assert!(d.frame(touch_down(0, pos)).down);
        assert!(
            !d.frame(vec![
                touch(0, egui::TouchPhase::Cancel, pos),
                egui::Event::PointerGone,
            ])
            .down
        );

        let after = d.frame(vec![motion.clone()]);
        assert!(!after.down, "a cancelled touch stays cancelled");
        assert!(!d.frame(vec![motion]).down);
    }
}

/// The web is the case a `PointerGone`-keyed rule missed entirely: `touchcancel`
/// there is a lone `Touch{Cancel}` (`eframe/src/web/events.rs:788`) — no release,
/// no `PointerGone` — so nothing ever clears egui's latched `down`.
#[test]
fn a_bare_touch_cancel_for_the_primary_finger_is_a_cancellation() {
    let mut d = TrackerDriver::new();
    let pos = egui::pos2(100.0, 100.0);

    assert!(
        d.frame(vec![
            button(egui::PointerButton::Primary, true, pos),
            touch(7, egui::TouchPhase::Start, pos),
        ])
        .down
    );

    assert!(
        !d.frame(vec![touch(7, egui::TouchPhase::Cancel, pos)]).down,
        "a lone Touch{{Cancel}} for the primary finger is the whole signal"
    );
    assert!(
        !d.frame(vec![egui::Event::PointerMoved(pos + egui::vec2(50.0, 0.0))])
            .down,
        "and it is terminal, exactly as a paired cancellation is"
    );
}

/// A whole gesture can arrive in one frame.
#[test]
fn a_touchstart_and_touchcancel_in_one_frame_is_a_cancellation() {
    let mut d = TrackerDriver::new();
    let pos = egui::pos2(100.0, 100.0);

    let batched = d.frame(vec![
        button(egui::PointerButton::Primary, true, pos),
        touch(3, egui::TouchPhase::Start, pos),
        touch(3, egui::TouchPhase::Cancel, pos),
    ]);
    assert!(
        !batched.down,
        "the finger this frame adopted is the finger this frame cancelled"
    );
    assert!(
        !d.frame(vec![egui::Event::PointerMoved(pos + egui::vec2(40.0, 0.0))])
            .down,
        "and it is terminal like any other cancellation"
    );
}

/// Non-primary buttons must not touch the sequence at all.
#[test]
fn a_non_primary_press_does_not_revive_a_lost_pointer() {
    let mut d = TrackerDriver::new();
    let pos = egui::pos2(100.0, 100.0);

    assert!(d.frame(touch_down(0, pos)).down);
    assert!(!d.frame(vec![touch(0, egui::TouchPhase::Cancel, pos)]).down);

    for b in [egui::PointerButton::Secondary, egui::PointerButton::Middle] {
        assert!(
            !d.frame(vec![button(b, true, pos)]).down,
            "{b:?} press must not resurrect a cancelled pointer"
        );
    }
}

/// …and a non-primary *release* must not close the sequence either.
#[test]
fn a_non_primary_release_does_not_close_the_sequence() {
    let mut d = TrackerDriver::new();
    let pos = egui::pos2(100.0, 100.0);

    assert!(d.frame(touch_down(0, pos)).down);
    assert!(
        d.frame(vec![button(egui::PointerButton::Secondary, false, pos)])
            .down,
        "a right-button release is not this sequence ending"
    );

    let second = pos + egui::vec2(80.0, 0.0);
    d.frame(vec![
        button(egui::PointerButton::Primary, true, pos),
        touch(1, egui::TouchPhase::Start, second),
    ]);

    assert!(
        d.frame(vec![touch(1, egui::TouchPhase::Cancel, second)])
            .down,
        "the second finger must not have taken the identity"
    );
    assert!(
        !d.frame(vec![touch(0, egui::TouchPhase::Cancel, pos)]).down,
        "the original finger must still be recognised when it is cancelled"
    );
}

/// A mouse click inside the window emits **no** `PointerGone`
/// (`egui-winit-0.34.1/src/lib.rs:791` pushes the button event alone), so the
/// release is the only thing that says the sequence ended.
#[test]
fn a_mouse_release_lets_the_next_touch_adopt_its_finger() {
    let mut d = TrackerDriver::new();
    let pos = egui::pos2(100.0, 100.0);

    d.frame(vec![button(egui::PointerButton::Primary, true, pos)]);
    d.frame(vec![button(egui::PointerButton::Primary, false, pos)]);

    assert!(d.frame(touch_down(2, pos)).down, "a finger, now");
    assert!(
        !d.frame(vec![touch(2, egui::TouchPhase::Cancel, pos)]).down,
        "the touch that opened this sequence must have been adopted"
    );
}

/// An excursion leaves no release behind to close the sequence, so the next press
/// has to be able to adopt its own finger anyway.
#[test]
fn a_new_sequence_after_an_excursion_adopts_its_own_finger() {
    let mut d = TrackerDriver::new();
    let pos = egui::pos2(100.0, 100.0);

    assert!(d.frame(touch_down(0, pos)).down);
    assert!(!d.frame(vec![egui::Event::PointerGone]).down);

    assert!(d.frame(touch_down(1, pos)).down);

    assert!(
        d.frame(vec![touch(0, egui::TouchPhase::Cancel, pos)]).down,
        "the stale id must no longer be able to cancel anything"
    );
    assert!(
        !d.frame(vec![touch(1, egui::TouchPhase::Cancel, pos)]).down,
        "and the finger actually in play must be"
    );
}

/// When two fingers start in one frame, the first is the primary — that is what
/// eframe picks (`all_touches.first()`, `web/input.rs:30`) and it pushes changed
/// touches in the same order (`:85`).
#[test]
fn the_first_touch_start_in_a_frame_is_the_primary() {
    let mut d = TrackerDriver::new();
    let pos = egui::pos2(100.0, 100.0);

    assert!(
        d.frame(vec![
            button(egui::PointerButton::Primary, true, pos),
            touch(5, egui::TouchPhase::Start, pos),
            touch(6, egui::TouchPhase::Start, pos + egui::vec2(60.0, 0.0)),
        ])
        .down
    );

    assert!(
        d.frame(vec![touch(
            6,
            egui::TouchPhase::Cancel,
            pos + egui::vec2(60.0, 0.0)
        )])
        .down,
        "the second finger is not the primary"
    );
    assert!(
        !d.frame(vec![touch(5, egui::TouchPhase::Cancel, pos)]).down,
        "the first one is"
    );
}

/// A *secondary* finger's cancel must still do nothing: it is not the touch backing
/// the emulated pointer, and killing the primary's live gesture is the failure the
/// old "never act on raw `Touch{Cancel}`" rule was protecting against.
#[test]
fn a_secondary_finger_cancel_leaves_the_primary_alone() {
    let mut d = TrackerDriver::new();
    let pos = egui::pos2(100.0, 100.0);

    assert!(d.frame(touch_down(0, pos)).down);

    let after = d.frame(vec![touch(
        1,
        egui::TouchPhase::Cancel,
        pos + egui::vec2(80.0, 0.0),
    )]);
    assert!(after.down, "another finger's cancellation is not ours");
}

/// The idle backstop may only *add* a reason to distrust the pointer.
#[test]
fn the_backstop_does_not_downgrade_a_cancellation() {
    let mut d = TrackerDriver::new();
    let pos = egui::pos2(100.0, 100.0);

    assert!(d.frame(touch_down(0, pos)).down);
    assert!(
        !d.frame(vec![
            touch(0, egui::TouchPhase::Cancel, pos),
            egui::Event::PointerGone,
        ])
        .down
    );

    assert!(!d.frame_after(90.0, vec![]).down);

    assert!(
        !d.frame(vec![egui::Event::PointerMoved(pos + egui::vec2(30.0, 0.0))])
            .down,
        "waiting out the backstop must not make a cancellation revivable"
    );
}

/// A real release forgets which finger owned the sequence.
#[test]
fn a_release_forgets_the_finger_that_owned_the_sequence() {
    let mut d = TrackerDriver::new();
    let pos = egui::pos2(100.0, 100.0);

    assert!(d.frame(touch_down(0, pos)).down);
    d.frame(vec![
        touch(0, egui::TouchPhase::End, pos),
        button(egui::PointerButton::Primary, false, pos),
        egui::Event::PointerGone,
    ]);

    assert!(
        d.frame(vec![button(egui::PointerButton::Primary, true, pos)])
            .down
    );

    assert!(
        d.frame(vec![touch(
            0,
            egui::TouchPhase::Cancel,
            pos + egui::vec2(200.0, 0.0)
        )])
        .down,
        "a recycled touch id must not cancel a mouse sequence"
    );
}

/// Adopting the primary finger must survive a second finger arriving.
#[test]
fn a_second_finger_does_not_steal_the_primary_identity() {
    let mut d = TrackerDriver::new();
    let pos = egui::pos2(100.0, 100.0);

    assert!(d.frame(touch_down(0, pos)).down);

    d.frame(vec![
        button(egui::PointerButton::Primary, true, pos),
        touch(1, egui::TouchPhase::Start, pos + egui::vec2(80.0, 0.0)),
    ]);

    assert!(
        d.frame(vec![touch(
            1,
            egui::TouchPhase::Cancel,
            pos + egui::vec2(80.0, 0.0)
        )])
        .down,
        "the second finger never became the primary"
    );
    assert!(
        !d.frame(vec![touch(0, egui::TouchPhase::Cancel, pos)]).down,
        "and the first finger still is"
    );
}

/// Only a *press* re-arms a lost pointer.
#[test]
fn a_release_does_not_re_arm_a_lost_pointer() {
    let mut d = TrackerDriver::new();
    let pos = egui::pos2(100.0, 100.0);

    assert!(
        d.frame(vec![
            egui::Event::PointerMoved(pos),
            button(egui::PointerButton::Primary, true, pos),
        ])
        .down,
        "precondition: pressed and down"
    );

    assert!(
        !d.frame(vec![egui::Event::PointerGone]).down,
        "precondition: the cancelled pointer is distrusted"
    );

    assert!(
        !d.frame(vec![button(egui::PointerButton::Secondary, false, pos)])
            .down,
        "a release must not resurrect a cancelled pointer"
    );

    assert!(
        d.frame(vec![button(egui::PointerButton::Primary, true, pos)])
            .down,
        "a fresh press re-arms"
    );
}

/// Motion un-latches, but a sequence that really ended stays ended: the pointer is
/// only "down" while egui says a button is down *and* we still believe it, so a
/// bare hover after a release re-arms nothing.
#[test]
fn hovering_after_a_real_release_does_not_re_arm() {
    let mut d = TrackerDriver::new();
    let pos = egui::pos2(100.0, 100.0);

    d.frame(vec![button(egui::PointerButton::Primary, true, pos)]);
    d.frame(vec![
        button(egui::PointerButton::Primary, false, pos),
        egui::Event::PointerGone,
    ]);

    let hovered = d.frame(vec![egui::Event::PointerMoved(pos + egui::vec2(5.0, 5.0))]);
    assert!(!hovered.down, "hovering is not holding");
}

/// A hand-built [`PointerFrame`], for driving [`ArmedDragDetector`] directly.
fn frame(pressed: bool, released: bool, down: bool, x: f32) -> PointerFrame {
    PointerFrame {
        pressed,
        released,
        down,
        pos: egui::pos2(x, 100.0),
        time: 0.0,
        stale_down: false,
    }
}

/// The whole gesture: press, move, release.
#[test]
fn a_press_a_move_and_a_release_are_an_anchor_a_drag_and_a_line() {
    let mut d = ArmedDragDetector::default();

    assert_eq!(
        d.update(frame(false, false, false, 10.0)),
        ArmedDragGesture::Idle
    );
    assert_eq!(
        d.update(frame(true, false, true, 10.0)),
        ArmedDragGesture::Anchored(egui::pos2(10.0, 100.0))
    );
    assert_eq!(
        d.update(frame(false, false, true, 50.0)),
        ArmedDragGesture::Dragging(egui::pos2(50.0, 100.0))
    );
    assert_eq!(
        d.update(frame(false, true, false, 90.0)),
        ArmedDragGesture::Released(egui::pos2(90.0, 100.0))
    );
    assert_eq!(
        d.update(frame(false, false, false, 90.0)),
        ArmedDragGesture::Idle,
        "the draw ended; a later frame must not report a second release"
    );
}

/// A pointer that goes away without releasing cancels the draw.
#[test]
fn a_pointer_that_vanishes_cancels_the_draw_rather_than_finishing_it() {
    let mut d = ArmedDragDetector::default();
    d.update(frame(true, false, true, 10.0));
    d.update(frame(false, false, true, 40.0));

    assert_eq!(
        d.update(frame(false, false, false, 40.0)),
        ArmedDragGesture::Cancelled,
        "a pointer that is no longer down, with no release, is a cancellation"
    );
    assert_eq!(
        d.update(frame(false, false, false, 40.0)),
        ArmedDragGesture::Idle,
        "and the detector is idle afterwards, not stuck in a drag"
    );
}

/// A press part-way through a draw starts a new one.
#[test]
fn a_second_press_re_anchors_rather_than_being_ignored() {
    let mut d = ArmedDragDetector::default();
    d.update(frame(true, false, true, 10.0));
    assert_eq!(
        d.update(frame(true, false, true, 70.0)),
        ArmedDragGesture::Anchored(egui::pos2(70.0, 100.0))
    );
    assert_eq!(
        d.update(frame(false, true, false, 90.0)),
        ArmedDragGesture::Released(egui::pos2(90.0, 100.0)),
        "the re-anchored draw is a real draw, not a discarded one"
    );
}

/// A tap — press and release inside one frame — never becomes a line.
#[test]
fn a_press_and_release_in_one_frame_does_not_finish_a_line() {
    let mut d = ArmedDragDetector::default();
    assert_eq!(
        d.update(frame(true, true, false, 10.0)),
        ArmedDragGesture::Anchored(egui::pos2(10.0, 100.0)),
        "the press wins the frame: there is nothing to release yet"
    );
    assert_eq!(
        d.update(frame(false, false, false, 10.0)),
        ArmedDragGesture::Cancelled,
        "and the pointer is already gone, so the anchor is dropped"
    );
}

/// The two properties of an armed frame are properties of the **type**.
#[test]
fn every_armed_frame_suppresses_panning_and_fires_no_overlay_click() {
    for gesture in [
        ArmedDragGesture::Idle,
        ArmedDragGesture::Anchored(egui::pos2(1.0, 2.0)),
        ArmedDragGesture::Dragging(egui::pos2(3.0, 4.0)),
        ArmedDragGesture::Released(egui::pos2(5.0, 6.0)),
        ArmedDragGesture::Cancelled,
    ] {
        let armed = ArmedDragFrame::new(gesture);
        assert_eq!(armed.gesture(), gesture);
        assert!(
            armed.pointer().suppress_pan,
            "{gesture:?} let the map pan while a line was being drawn"
        );
        assert_eq!(
            armed.pointer().overlay_click_pos,
            None,
            "{gesture:?} fired an overlay click from a press that starts a line"
        );
        assert_eq!(armed.pointer().long_press_pos, None, "{gesture:?}");
    }
}

// --- the phantom latched button --------------------------------------------
//
// Two different ways a pan drag loses its ending, and they are not the same
// bug. `a_drag_whose_release_is_never_seen_stops_the_pane` (`ui_map/tests.rs`)
// is the one where the *widget* stops being shown: the release edge is offered
// and nobody is there to take it, and the map then panned at constant velocity
// forever. These are the other one: the *pointer* goes away while the widget
// is drawn every frame, egui goes on latching `primary_down()`, and the map
// repaints at full rate without moving at all.

/// The cursor leaves the window mid-drag. `down` goes false and `stale_down`
/// goes true on the same frame, and stays true — egui has no later event that
/// would unwind it, because a release out there is dropped.
#[test]
fn a_pointer_that_left_the_window_reads_as_a_stale_down() {
    let mut d = TrackerDriver::new();
    let pos = egui::pos2(100.0, 100.0);

    let pressed = d.frame(vec![button(egui::PointerButton::Primary, true, pos)]);
    assert!(pressed.down, "precondition: the press did not take");
    assert!(
        !pressed.stale_down,
        "a believed down must never also be a stale one"
    );

    let gone = d.frame(vec![egui::Event::PointerGone]);
    assert!(!gone.down, "the pointer is gone; nothing is held");
    assert!(
        gone.stale_down,
        "egui still latches the button down, and nothing said so"
    );

    // Ten seconds of the silence that follows a cursor that left.
    for frame in 0..600 {
        let quiet = d.frame(vec![]);
        assert!(!quiet.down, "frame {frame}");
        assert!(
            quiet.stale_down,
            "frame {frame}: the phantom stopped being reported while it is \
             still there"
        );
    }

    // A real press is the one thing that clears it.
    let repressed = d.frame(vec![button(egui::PointerButton::Primary, true, pos)]);
    assert!(repressed.down);
    assert!(!repressed.stale_down);
}

/// The control: an ordinary press-drag-release never reports a stale down, so
/// the test above is not agreeing with a constant.
#[test]
fn an_ordinary_drag_never_reads_as_a_stale_down() {
    let mut d = TrackerDriver::new();
    let pos = egui::pos2(100.0, 100.0);

    for events in [
        vec![button(egui::PointerButton::Primary, true, pos)],
        vec![egui::Event::PointerMoved(pos + egui::vec2(30.0, 0.0))],
        vec![egui::Event::PointerMoved(pos + egui::vec2(60.0, 0.0))],
        vec![button(
            egui::PointerButton::Primary,
            false,
            pos + egui::vec2(60.0, 0.0),
        )],
        vec![],
    ] {
        assert!(
            !d.frame(events).stale_down,
            "an ordinary drag reported a phantom"
        );
    }
}

/// A hand-built [`PointerFrame`] whose latched button is a phantom.
fn stale_frame() -> PointerFrame {
    PointerFrame {
        pressed: false,
        released: false,
        down: false,
        pos: egui::pos2(10.0, 10.0),
        time: 0.0,
        stale_down: true,
    }
}

/// A hand-built [`PointerFrame`] with no pointer activity at all.
fn quiet_frame() -> PointerFrame {
    PointerFrame {
        stale_down: false,
        ..stale_frame()
    }
}

/// A pan the map is in the middle of when the pointer goes away is suppressed,
/// and **stays** suppressed after walkers has settled the map — the phantom
/// outlives the settle, so a suppression that lasted one frame would let the
/// map back into `Center::Moving` on the next.
#[test]
fn a_stranded_pan_stays_suppressed_until_the_pointer_comes_back() {
    let mut gestures = TouchGestures::default();
    let mut memory = walkers::MapMemory::default();

    // Nothing is stranded while the pointer is real.
    memory.center_at(walkers::lon_lat(17.0, 51.0));
    assert!(!gestures.pan_stranded(quiet_frame(), &memory));

    // Mid-pan, the pointer goes away.
    let mut dragging = walkers::MapMemory::default();
    dragging.center_at(walkers::lon_lat(17.0, 51.0));
    drive_into_moving(&mut dragging);
    assert!(
        dragging.dragging(),
        "precondition: the fixture map is not mid-pan"
    );
    assert!(gestures.pan_stranded(stale_frame(), &dragging));

    // walkers settles it on that frame; the pointer is still gone, so the
    // suppression has to hold.
    dragging.settle();
    assert!(!dragging.dragging(), "precondition: settle did not settle");
    for frame in 0..600 {
        assert!(
            gestures.pan_stranded(stale_frame(), &dragging),
            "frame {frame}: the suppression lapsed while the phantom is still there"
        );
    }

    // And it ends the moment the pointer is believable again.
    assert!(!gestures.pan_stranded(quiet_frame(), &dragging));
}

/// A phantom with no pan behind it suppresses nothing: a cancelled touch that
/// was not panning the map leaves the map pannable, which is what
/// `touch_cancelled_mid_drag_releases_the_map` pins at the harness level.
#[test]
fn a_phantom_with_no_pan_behind_it_suppresses_nothing() {
    let mut gestures = TouchGestures::default();
    let mut memory = walkers::MapMemory::default();
    memory.center_at(walkers::lon_lat(17.0, 51.0));

    for frame in 0..600 {
        assert!(
            !gestures.pan_stranded(stale_frame(), &memory),
            "frame {frame}: a map that is not panning had its pan suppressed"
        );
    }
}

/// Put a [`walkers::MapMemory`] into `Center::Moving` the only way a caller
/// can: through the widget's own gesture handling. Rather than asserting on a
/// state this crate cannot name, the loop below stops as soon as walkers says
/// `dragging()`.
fn drive_into_moving(memory: &mut walkers::MapMemory) {
    let ctx = egui::Context::default();
    let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0));
    let mut pos = egui::pos2(400.0, 300.0);
    // The first frame only registers the widget — egui resolves a press
    // against the *previous* frame's rects, so a press on frame zero hits
    // nothing at all.
    let mut events = Vec::new();
    let mut time = 1.0;

    for frame in 0..20 {
        let raw_input = egui::RawInput {
            screen_rect: Some(screen),
            time: Some(time),
            events: std::mem::take(&mut events),
            ..Default::default()
        };
        ctx.begin_pass(raw_input);
        let mut ui = egui::Ui::new(
            ctx.clone(),
            egui::Id::new("stranded_pan_map"),
            egui::UiBuilder::new().max_rect(screen),
        );
        ui.add(walkers::Map::new(
            None,
            memory,
            walkers::lon_lat(17.0, 51.0),
        ));
        let _ = ctx.end_pass();
        if memory.dragging() {
            return;
        }

        time += 1.0 / 60.0;
        if frame == 0 {
            events.push(egui::Event::PointerMoved(pos));
            events.push(button(egui::PointerButton::Primary, true, pos));
        } else {
            pos += egui::vec2(20.0, 0.0);
            events.push(egui::Event::PointerMoved(pos));
        }
    }
}

/// The pane the map's zoom floor was reported from, in points.
///
/// Kept beside the value it produces rather than only in
/// `vendor/walkers/src/viewport.rs`, so that a reader of this test can see both
/// halves of the arithmetic without leaving the file.
const REAL_VIEWPORT: (f32, f32) = (2878.0, 1651.0);

/// `log2(2878 / 256)`, to the bit: [`walkers::viewport::min_zoom`] for
/// [`REAL_VIEWPORT`], and the same constant `vendor/walkers/src/viewport.rs`'s
/// own tests pin under this name.
///
/// Written out rather than computed, deliberately and in both places: a change
/// to the floor's arithmetic has to *break* two pins, not move them together.
const REAL_FLOOR: f64 = 3.4908508767402977;

fn real_viewport_rect() -> egui::Rect {
    egui::Rect::from_min_size(
        egui::Pos2::ZERO,
        egui::vec2(REAL_VIEWPORT.0, REAL_VIEWPORT.1),
    )
}

/// The zoom one tile-less `walkers::Map` frame over `rect` leaves in `memory` —
/// **what is drawn**, as against what the host believes.
///
/// Tile-less because the floor is a property of the widget's rect and of
/// nothing a tile source does, and one frame because that is the whole
/// question: the widget raises a below-floor zoom on the first frame it is
/// shown at, so a host that agrees with this has agreed on the same frame.
fn zoom_the_widget_draws(rect: egui::Rect, memory: &mut walkers::MapMemory) -> f64 {
    let ctx = egui::Context::default();
    ctx.begin_pass(egui::RawInput {
        screen_rect: Some(rect),
        ..Default::default()
    });
    let mut ui = egui::Ui::new(
        ctx.clone(),
        egui::Id::new("zoom floor under test"),
        egui::UiBuilder::new()
            .layer_id(egui::LayerId::background())
            .max_rect(rect),
    );
    ui.set_clip_rect(rect);
    ui.add(walkers::Map::new(
        None,
        memory,
        walkers::lon_lat(-97.28, 35.33),
    ));
    let _ = ctx.end_pass();
    memory.zoom()
}

/// Put a detector mid-zoom-drag from `initial_zoom`, then drag `dy` points.
fn zoom_drag(initial_zoom: f64, dy: f32, map_rect: egui::Rect) -> walkers::MapMemory {
    let ctx = egui::Context::default();
    let mut memory = walkers::MapMemory::default();
    memory
        .set_zoom(initial_zoom)
        .expect("the fixture's starting zoom is inside walkers' range");

    let mut detector = DoubleTapDragDetector {
        state: GestureState::ZoomDragging {
            drag_start_y: 100.0,
            initial_zoom,
        },
        ..Default::default()
    };
    detector.update(
        &ctx,
        PointerFrame {
            pressed: false,
            released: false,
            down: true,
            pos: egui::pos2(200.0, 100.0 + dy),
            time: 1.0,
            stale_down: false,
        },
        &mut memory,
        map_rect,
    );
    memory
}

/// **The gap this closes**: the gesture's own `1.0` is not the widest legal
/// zoom — the rect is, and a drag that asks for less comes back at the floor.
#[test]
fn a_zoom_drag_below_the_viewports_floor_lands_on_the_floor() {
    let rect = real_viewport_rect();
    assert_eq!(
        walkers::viewport::min_zoom(rect).to_bits(),
        REAL_FLOOR.to_bits(),
        "the pinned floor is no longer what the widget computes for {REAL_VIEWPORT:?}",
    );

    // Two full pane-heights of upward drag: far enough to run the gesture into
    // its own `1.0` lower bound, which is 2.49 levels under this rect's floor.
    let memory = zoom_drag(5.0, -2.0 * REAL_VIEWPORT.1, rect);
    assert_eq!(
        memory.zoom().to_bits(),
        REAL_FLOOR.to_bits(),
        "a zoom drag off the bottom of a {REAL_VIEWPORT:?} pane left the host at \
         {} - below the floor the widget draws at, so the host is holding a zoom \
         nothing was ever drawn at",
        memory.zoom(),
    );
}

/// The other half of the same fact: the floor only ever *raises*, so a drag
/// that stays above it is untouched.
#[test]
fn a_zoom_drag_above_the_viewports_floor_is_left_alone() {
    let rect = real_viewport_rect();
    // 300 points of downward drag at 150 points per level: two levels in.
    let memory = zoom_drag(8.0, 300.0, rect);
    assert_eq!(
        memory.zoom().to_bits(),
        10.0f64.to_bits(),
        "a drag well inside the legal range was moved by the floor",
    );
}

/// **Host state and drawn state agree on the same frame.**
///
/// Not on the next one: the widget's own raise is what used to close this gap,
/// and it closes it a frame late, because it happens inside a `Map::show` the
/// host has already read its zoom out of. Showing the widget over the same rect
/// the gesture was handed must now move nothing at all.
#[test]
fn the_host_zoom_a_drag_leaves_is_the_zoom_the_widget_draws() {
    let rect = real_viewport_rect();
    let mut memory = zoom_drag(5.0, -2.0 * REAL_VIEWPORT.1, rect);
    let host = memory.zoom();
    let drawn = zoom_the_widget_draws(rect, &mut memory);
    assert_eq!(
        host.to_bits(),
        drawn.to_bits(),
        "the host left zoom at {host} and the widget drew {drawn} - the two \
         disagree for the frame in between, and every reader of zoom in that \
         window, a rebuild's attribution included, sees the number that was \
         not drawn",
    );
    assert_eq!(
        drawn.to_bits(),
        REAL_FLOOR.to_bits(),
        "the agreed value is not the floor either end was supposed to reach",
    );
}
