use super::*;

/// Drives [`PointerTracker`] directly, one hand-built frame at a time.
///
/// The end-to-end probes live in `input_harness.rs`; this is for the event
/// orderings that pipeline cannot easily produce — a `PointerButton` for a
/// button other than the primary one, in particular, which is the only way
/// a *release* can reach the tracker while egui still reports the primary
/// as down.
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

/// `egui-winit`'s `TouchPhase::Started`: the raw touch, then the emulated
/// pointer (`egui-winit-0.34.1/src/lib.rs:897`).
fn touch_down(id: u64, pos: egui::Pos2) -> Vec<egui::Event> {
    vec![
        touch(id, egui::TouchPhase::Start, pos),
        egui::Event::PointerMoved(pos),
        button(egui::PointerButton::Primary, true, pos),
    ]
}

/// A `PointerGone` is terminal until a press, whichever kind it was.
///
/// For the excursion half this is a policy choice between two failures, and
/// it takes the benign one. The integration discards a release that happens
/// out of the window (`lib.rs:796`), so a cursor that comes back hovering
/// and one that comes back still dragging are the same event stream: the
/// cost of being wrong one way is re-pressing to carry on, and the other
/// way is a hold nobody asked for suppressing panning until they click.
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

/// A cancellation is terminal. Motion afterwards is some *other* input
/// source — the next finger, a mouse on a hybrid device, `mousemove` on the
/// web — and must never be read as the cancelled finger returning.
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
        // ...and keeps staying cancelled.
        assert!(!d.frame(vec![motion]).down);
    }
}

/// The web is the case a `PointerGone`-keyed rule missed entirely:
/// `touchcancel` there is a lone `Touch{Cancel}`
/// (`eframe/src/web/events.rs:788`) — no release, no `PointerGone` — so
/// nothing ever clears egui's latched `down`.
#[test]
fn a_bare_touch_cancel_for_the_primary_finger_is_a_cancellation() {
    let mut d = TrackerDriver::new();
    let pos = egui::pos2(100.0, 100.0);

    // eframe pushes the press *before* the raw touch — the opposite order
    // to egui-winit — so the pairing must not depend on it.
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
///
/// eframe's touch listeners only request a repaint (`events.rs:695`)
/// instead of running a frame synchronously, so every DOM event between two
/// animation frames lands in a single `RawInput` — and on a map app
/// decoding tiles those frames are long. A browser taking the gesture over
/// on the first move (an iOS Safari edge swipe, Android Chrome
/// pull-to-refresh) delivers `touchstart` and `touchcancel` exactly that
/// way. Comparing the cancel against the identity held at frame *entry*
/// misses it, even though the adoption has already run earlier in the same
/// walk.
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
///
/// `down` is `primary_down()`, so a right-click says nothing about the
/// finger or left button being tracked. A press that cleared the latch
/// would revive a loss documented as terminal, while egui's `down` is still
/// stale-`true`.
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
///
/// Closing it would let the very next press re-adopt — and eframe emits a
/// press for every `touchstart`, so a second finger arriving after a
/// right-button release would take the identity. The finger id is the
/// entire cancellation signal on the web, so losing it that way makes the
/// real finger's cancel invisible.
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

    // eframe's re-emitted press for a second finger.
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
/// (`egui-winit-0.34.1/src/lib.rs:791` pushes the button event alone), so
/// the release is the only thing that says the sequence ended. If it did
/// not close it, the next press would be treated as mid-sequence and the
/// finger opening it would never be adopted — on a hybrid device, a click
/// followed by a touch.
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

/// An excursion leaves no release behind to close the sequence, so the next
/// press has to be able to adopt its own finger anyway. Otherwise the old,
/// **recycled** id sticks — and a different finger carrying it, cancelled
/// anywhere on screen, kills a live sequence it has nothing to do with.
#[test]
fn a_new_sequence_after_an_excursion_adopts_its_own_finger() {
    let mut d = TrackerDriver::new();
    let pos = egui::pos2(100.0, 100.0);

    assert!(d.frame(touch_down(0, pos)).down);
    // Mid-sequence excursion: `PointerGone` with no release at all.
    assert!(!d.frame(vec![egui::Event::PointerGone]).down);

    // A new touch, with a different id.
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

/// When two fingers start in one frame, the first is the primary — that is
/// what eframe picks (`all_touches.first()`, `web/input.rs:30`) and it
/// pushes changed touches in the same order (`:85`).
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

/// A *secondary* finger's cancel must still do nothing: it is not the touch
/// backing the emulated pointer, and killing the primary's live gesture is
/// the failure the old "never act on raw `Touch{Cancel}`" rule was
/// protecting against.
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
///
/// After a cancellation nothing else arrives, so the backstop's condition
/// goes on being true forever. If it were allowed to overwrite the cause,
/// a cancelled touch would quietly become an idle one a minute later — and
/// idle is the cause motion is allowed to clear, so the phantom would come
/// back on the next stray event.
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

    // Well past POINTER_IDLE_TIMEOUT_S of complete silence.
    assert!(!d.frame_after(90.0, vec![]).down);

    assert!(
        !d.frame(vec![egui::Event::PointerMoved(pos + egui::vec2(30.0, 0.0))])
            .down,
        "waiting out the backstop must not make a cancellation revivable"
    );
}

/// A real release forgets which finger owned the sequence.
///
/// Touch ids are reused. If the tracker kept the old one, a later
/// *mouse* press would inherit it — and then a fresh touch reusing that id,
/// cancelled somewhere else on a hybrid device, would kill the mouse
/// gesture that has nothing to do with it.
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

    // A mouse press: no touch involved at all.
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
/// eframe re-emits a primary press for *every* `touchstart`
/// (`events.rs:676`), and that frame carries the *new* finger's
/// `Touch{Start}` — so a naive "the press frame's touch id is the primary"
/// rule would hand the identity to the wrong finger, and then honour that
/// finger's cancel while ignoring the real one's.
#[test]
fn a_second_finger_does_not_steal_the_primary_identity() {
    let mut d = TrackerDriver::new();
    let pos = egui::pos2(100.0, 100.0);

    assert!(d.frame(touch_down(0, pos)).down);

    // Web-shaped second touchstart: a redundant primary press at the
    // *first* finger's position, plus the second finger's raw touch.
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

/// Only a *press* re-arms a lost pointer. A release is not evidence that
/// the pointer came back — it is the opposite — so it must leave the latch
/// alone even when it arrives for a button egui is not tracking as down.
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

    // Cancelled: `PointerGone` with no release. egui goes on reporting the
    // primary button as down from here.
    assert!(
        !d.frame(vec![egui::Event::PointerGone]).down,
        "precondition: the cancelled pointer is distrusted"
    );

    // A secondary-button release arrives. It clears nothing in egui's
    // primary `down`, and it says nothing about the primary finger.
    assert!(
        !d.frame(vec![button(egui::PointerButton::Secondary, false, pos)])
            .down,
        "a release must not resurrect a cancelled pointer"
    );

    // A press, however, does — that is a new sequence.
    assert!(
        d.frame(vec![button(egui::PointerButton::Primary, true, pos)])
            .down,
        "a fresh press re-arms"
    );
}

/// Motion un-latches, but a sequence that really ended stays ended: the
/// pointer is only "down" while egui says a button is down *and* we still
/// believe it, so a bare hover after a release re-arms nothing.
#[test]
fn hovering_after_a_real_release_does_not_re_arm() {
    let mut d = TrackerDriver::new();
    let pos = egui::pos2(100.0, 100.0);

    d.frame(vec![button(egui::PointerButton::Primary, true, pos)]);
    // A normal touch-up: release *and* `PointerGone`, in that order.
    d.frame(vec![
        button(egui::PointerButton::Primary, false, pos),
        egui::Event::PointerGone,
    ]);

    let hovered = d.frame(vec![egui::Event::PointerMoved(pos + egui::vec2(5.0, 5.0))]);
    assert!(!hovered.down, "hovering is not holding");
}

/// A hand-built [`PointerFrame`], for driving [`SectionLineDetector`]
/// directly.
///
/// Straight construction rather than through [`PointerTracker`], because
/// the detector's contract is with the *frame* — the tracker's own
/// behaviour has its own suite above, and routing through it would make
/// these tests fail for its reasons as well as their own.
fn frame(pressed: bool, released: bool, down: bool, x: f32) -> PointerFrame {
    PointerFrame {
        pressed,
        released,
        down,
        pos: egui::pos2(x, 100.0),
        time: 0.0,
    }
}

/// The whole gesture: press, move, release.
#[test]
fn a_press_a_move_and_a_release_are_an_anchor_a_drag_and_a_line() {
    let mut d = SectionLineDetector::default();

    assert_eq!(
        d.update(frame(false, false, false, 10.0)),
        SectionGesture::Idle
    );
    assert_eq!(
        d.update(frame(true, false, true, 10.0)),
        SectionGesture::Anchored(egui::pos2(10.0, 100.0))
    );
    assert_eq!(
        d.update(frame(false, false, true, 50.0)),
        SectionGesture::Dragging(egui::pos2(50.0, 100.0))
    );
    assert_eq!(
        d.update(frame(false, true, false, 90.0)),
        SectionGesture::Released(egui::pos2(90.0, 100.0))
    );
    assert_eq!(
        d.update(frame(false, false, false, 90.0)),
        SectionGesture::Idle,
        "the draw ended; a later frame must not report a second release"
    );
}

/// A pointer that goes away without releasing cancels the draw.
///
/// This is not hypothetical tidiness. `down` here is the tracker's
/// *corrected* answer, and correcting it is the whole reason that type
/// exists: after an OS-cancelled touch egui's own `primary_down()` stays
/// `true` for ever. A detector keyed on egui's flag would leave `drawing`
/// set, and `ArmedSectionFrame` makes `suppress_pan` unconditional — so the
/// map would stay un-pannable with nothing on screen to say why.
#[test]
fn a_pointer_that_vanishes_cancels_the_draw_rather_than_finishing_it() {
    let mut d = SectionLineDetector::default();
    d.update(frame(true, false, true, 10.0));
    d.update(frame(false, false, true, 40.0));

    assert_eq!(
        d.update(frame(false, false, false, 40.0)),
        SectionGesture::Cancelled,
        "a pointer that is no longer down, with no release, is a cancellation"
    );
    assert_eq!(
        d.update(frame(false, false, false, 40.0)),
        SectionGesture::Idle,
        "and the detector is idle afterwards, not stuck in a drag"
    );
}

/// A press part-way through a draw starts a new one.
///
/// The only ways to produce one are a fresh finger and a fresh button, and
/// both mean "start here" more plausibly than they mean "ignore me".
#[test]
fn a_second_press_re_anchors_rather_than_being_ignored() {
    let mut d = SectionLineDetector::default();
    d.update(frame(true, false, true, 10.0));
    assert_eq!(
        d.update(frame(true, false, true, 70.0)),
        SectionGesture::Anchored(egui::pos2(70.0, 100.0))
    );
    assert_eq!(
        d.update(frame(false, true, false, 90.0)),
        SectionGesture::Released(egui::pos2(90.0, 100.0)),
        "the re-anchored draw is a real draw, not a discarded one"
    );
}

/// A tap — press and release inside one frame — never becomes a line.
///
/// egui batches a whole gesture into one `RawInput` whenever frames are
/// long, which on a map app decoding tiles is routine, so this ordering is
/// ordinary rather than exotic.
#[test]
fn a_press_and_release_in_one_frame_does_not_finish_a_line() {
    let mut d = SectionLineDetector::default();
    assert_eq!(
        d.update(frame(true, true, false, 10.0)),
        SectionGesture::Anchored(egui::pos2(10.0, 100.0)),
        "the press wins the frame: there is nothing to release yet"
    );
    assert_eq!(
        d.update(frame(false, false, false, 10.0)),
        SectionGesture::Cancelled,
        "and the pointer is already gone, so the anchor is dropped"
    );
}

/// The two properties of an armed frame are properties of the **type**.
///
/// `ArmedSectionFrame::new` is the only constructor and the field is
/// private, so there is no inhabitant for which panning is allowed or an
/// overlay click fires. The alternative — returning a bare
/// [`MapPointerFrame`] and asking each caller to clear two fields — is the
/// shape of rule that gets obeyed at the site it was written for and
/// nowhere else.
#[test]
fn every_armed_frame_suppresses_panning_and_fires_no_overlay_click() {
    for gesture in [
        SectionGesture::Idle,
        SectionGesture::Anchored(egui::pos2(1.0, 2.0)),
        SectionGesture::Dragging(egui::pos2(3.0, 4.0)),
        SectionGesture::Released(egui::pos2(5.0, 6.0)),
        SectionGesture::Cancelled,
    ] {
        let armed = ArmedSectionFrame::new(gesture);
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
