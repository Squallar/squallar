//! The event sequences the real integrations produce, as emitters onto a
//! plain `Vec<egui::Event>` — the one copy of the tables, shared by the
//! headless [`input_harness`](crate::input_harness) and the
//! [`gesture_player`](crate::gesture_player), so a scripted gesture and a test
//! gesture cannot drift apart in what they feed egui.
//!
//! `egui-winit` 0.35.0 (`src/lib.rs`):
//!
//! | winit event                | emitted here                                          |
//! |----------------------------|-------------------------------------------------------|
//! | `TouchPhase::Started`      | `Touch{Start}`, `PointerMoved`, `PointerButton{down}` |
//! | `TouchPhase::Moved`        | `Touch{Move}`, `PointerMoved`                         |
//! | `TouchPhase::Ended`        | `Touch{End}`, `PointerButton{up}`, `PointerGone`      |
//! | `TouchPhase::Cancelled`    | `Touch{Cancel}`, `PointerGone` — **no release**       |
//! | `WindowEvent::CursorLeft`  | `PointerGone` alone — and the position is forgotten,  |
//! |                            | so a release out there is dropped (`lib.rs:784`)      |
//!
//! eframe 0.35.0's web canvas (`src/web/events.rs`):
//!
//! | DOM event     | emitted here                                                  |
//! |---------------|---------------------------------------------------------------|
//! | `touchstart`  | `PointerButton{down}` **then** `Touch{Start}` — order flipped |
//! | `touchmove`   | `PointerMoved`, `Touch{Move}`                                 |
//! | `touchend`    | `PointerButton{up}`, `PointerGone`, `Touch{End}`              |
//! | `touchcancel` | `Touch{Cancel}` **alone** — no release, no `PointerGone`      |
//! | `mousemove`   | `PointerMoved`                                                |
//!
//! A cancelled touch never reports a release and egui does not clear
//! `pointer.down` on `PointerGone`, so any gesture that only exits on "pointer
//! up" stays stuck forever; on the web there is no `PointerGone` either.
//!
//! Emitters marked *harness-only* have no shipped caller yet — only the
//! `cfg(test)` harness reaches them — so they carry
//! `cfg_attr(not(test), allow(dead_code))` rather than moving back out of the
//! shared table.

pub(crate) fn pointer_button(pos: egui::Pos2, pressed: bool) -> egui::Event {
    pointer_button_of(pos, egui::PointerButton::Primary, pressed)
}

pub(crate) fn pointer_button_of(
    pos: egui::Pos2,
    button: egui::PointerButton,
    pressed: bool,
) -> egui::Event {
    egui::Event::PointerButton {
        pos,
        button,
        pressed,
        modifiers: egui::Modifiers::default(),
    }
}

pub(crate) fn touch(phase: egui::TouchPhase, pos: egui::Pos2) -> egui::Event {
    egui::Event::Touch {
        device_id: egui::TouchDeviceId(0),
        id: egui::TouchId(0),
        phase,
        pos,
        force: None,
    }
}

/// The browser `pointerId`s the two fingers arrive under. winit's web backend
/// uses that one number for **both** the touch id and the device id
/// (`window_target.rs:410`), so these deliberately do the same.
pub(crate) const WEB_FINGER_A: u64 = 3;
pub(crate) const WEB_FINGER_B: u64 = 4;

/// A touch exactly as winit's web backend reports it: a device id fabricated
/// per finger from the pointer id.
pub(crate) fn web_touch(pointer_id: u64, phase: egui::TouchPhase, pos: egui::Pos2) -> egui::Event {
    egui::Event::Touch {
        device_id: egui::TouchDeviceId(pointer_id),
        id: egui::TouchId(pointer_id),
        phase,
        pos,
        force: None,
    }
}

// ── mouse (mirrors egui-winit's cursor + button handling) ──

pub(crate) fn mouse_move(events: &mut Vec<egui::Event>, pos: egui::Pos2) {
    events.push(egui::Event::PointerMoved(pos));
}

pub(crate) fn mouse_press(events: &mut Vec<egui::Event>, pos: egui::Pos2) {
    mouse_move(events, pos);
    events.push(pointer_button(pos, true));
}

pub(crate) fn mouse_release(events: &mut Vec<egui::Event>, pos: egui::Pos2) {
    mouse_move(events, pos);
    events.push(pointer_button(pos, false));
}

#[cfg_attr(not(test), allow(dead_code))] // harness-only; see module doc
pub(crate) fn mouse_press_secondary(events: &mut Vec<egui::Event>, pos: egui::Pos2) {
    mouse_move(events, pos);
    events.push(pointer_button_of(pos, egui::PointerButton::Secondary, true));
}

#[cfg_attr(not(test), allow(dead_code))] // harness-only; see module doc
pub(crate) fn mouse_release_secondary(events: &mut Vec<egui::Event>, pos: egui::Pos2) {
    mouse_move(events, pos);
    events.push(pointer_button_of(
        pos,
        egui::PointerButton::Secondary,
        false,
    ));
}

/// The cursor left the window: `egui-winit` maps `WindowEvent::CursorLeft`
/// to a bare [`egui::Event::PointerGone`] and forgets the pointer position
/// (`egui-winit-0.34.1/src/lib.rs:340`). **No release is reported** — and
/// while the position is forgotten, a real mouse release happening outside
/// the window is dropped on the floor too (`lib.rs:796`), which is why
/// egui's `primary_down()` can stay latched across the excursion.
#[cfg_attr(not(test), allow(dead_code))] // harness-only; see module doc
pub(crate) fn cursor_left(events: &mut Vec<egui::Event>) {
    events.push(egui::Event::PointerGone);
}

/// Raw device motion (`DeviceEvent::MouseMotion` → [`egui::Event::MouseMoved`]).
#[cfg_attr(not(test), allow(dead_code))] // harness-only; see module doc
pub(crate) fn mouse_moved_raw(events: &mut Vec<egui::Event>, delta: egui::Vec2) {
    events.push(egui::Event::MouseMoved(delta));
}

/// One wheel report over `pos`, in whichever unit the integration chose.
/// `egui-winit` derives the unit straight from winit's `MouseScrollDelta`, so
/// the unit is the only thing that differs between a browser that sends
/// `DOM_DELTA_PIXEL` and one that sends `DOM_DELTA_LINE` — and a native mouse
/// notch is one `Line`, which egui's own `line_scroll_speed` scales.
pub(crate) fn wheel(
    events: &mut Vec<egui::Event>,
    pos: egui::Pos2,
    unit: egui::MouseWheelUnit,
    delta: egui::Vec2,
) {
    events.push(egui::Event::PointerMoved(pos));
    events.push(egui::Event::MouseWheel {
        unit,
        delta,
        // What egui documents for an unknown phase, and what a discrete
        // mouse notch reads as.
        phase: egui::TouchPhase::Move,
        modifiers: egui::Modifiers::default(),
    });
}

// ── touch (mirrors egui-winit's `on_touch`) ──

#[cfg_attr(not(test), allow(dead_code))] // harness-only; see module doc
pub(crate) fn touch_start(events: &mut Vec<egui::Event>, pos: egui::Pos2) {
    events.push(touch(egui::TouchPhase::Start, pos));
    events.push(egui::Event::PointerMoved(pos));
    events.push(pointer_button(pos, true));
}

#[cfg_attr(not(test), allow(dead_code))] // harness-only; see module doc
pub(crate) fn touch_move(events: &mut Vec<egui::Event>, pos: egui::Pos2) {
    events.push(touch(egui::TouchPhase::Move, pos));
    events.push(egui::Event::PointerMoved(pos));
}

#[cfg_attr(not(test), allow(dead_code))] // harness-only; see module doc
pub(crate) fn touch_end(events: &mut Vec<egui::Event>, pos: egui::Pos2) {
    events.push(touch(egui::TouchPhase::End, pos));
    events.push(pointer_button(pos, false));
    events.push(egui::Event::PointerGone);
}

/// The OS/browser took the gesture away: **no release is reported**, only
/// `PointerGone`, exactly as `egui-winit` does for `TouchPhase::Cancelled`.
#[cfg_attr(not(test), allow(dead_code))] // harness-only; see module doc
pub(crate) fn touch_cancel(events: &mut Vec<egui::Event>, pos: egui::Pos2) {
    events.push(touch(egui::TouchPhase::Cancel, pos));
    events.push(egui::Event::PointerGone);
}

/// A *secondary* finger's touch being cancelled: a raw `Touch{Cancel}` for
/// another `TouchId`, with no `PointerGone`, since the emulated pointer is
/// still owned by the primary finger.
#[cfg_attr(not(test), allow(dead_code))] // harness-only; see module doc
pub(crate) fn secondary_touch_cancel(events: &mut Vec<egui::Event>, pos: egui::Pos2) {
    events.push(egui::Event::Touch {
        device_id: egui::TouchDeviceId(0),
        id: egui::TouchId(1),
        phase: egui::TouchPhase::Cancel,
        pos,
        force: None,
    });
}

// ── web canvas (mirrors eframe 0.34.1's listeners) ──

/// `touchstart`, as eframe's web canvas emits it: the primary
/// `PointerButton{pressed}` **first**, then `push_touches(Start)`
/// (`eframe/src/web/events.rs:676`) — the opposite order to `egui-winit`,
/// which is why the tracker correlates the pair over the whole frame.
#[cfg_attr(not(test), allow(dead_code))] // harness-only; see module doc
pub(crate) fn web_touch_start(events: &mut Vec<egui::Event>, pos: egui::Pos2) {
    events.push(pointer_button(pos, true));
    events.push(touch(egui::TouchPhase::Start, pos));
}

/// `touchmove` (`events.rs:709`): a bare `PointerMoved`, with the raw touch
/// pushed alongside it.
#[cfg_attr(not(test), allow(dead_code))] // harness-only; see module doc
pub(crate) fn web_touch_move(events: &mut Vec<egui::Event>, pos: egui::Pos2) {
    events.push(egui::Event::PointerMoved(pos));
    events.push(touch(egui::TouchPhase::Move, pos));
}

/// `touchcancel` (`events.rs:788`): `push_touches(Cancel)` and **nothing
/// else** — no release, no `PointerGone`. egui's `primary_down()` therefore
/// stays latched `true` with no event ever clearing it, so a tracker that
/// keys cancellation on `PointerGone` alone never fires at all here.
#[cfg_attr(not(test), allow(dead_code))] // harness-only; see module doc
pub(crate) fn web_touch_cancel(events: &mut Vec<egui::Event>, pos: egui::Pos2) {
    events.push(touch(egui::TouchPhase::Cancel, pos));
}

/// `mousemove` (`events.rs:627`): a bare `PointerMoved`. Note this reaches
/// the canvas whether or not any touch is involved, which is what makes a
/// motion-based un-latch dangerous after a cancellation on the web.
#[cfg_attr(not(test), allow(dead_code))] // harness-only; see module doc
pub(crate) fn web_mouse_move(events: &mut Vec<egui::Event>, pos: egui::Pos2) {
    events.push(egui::Event::PointerMoved(pos));
}

// ── web multi-touch (mirrors winit's web backend, per-finger devices) ──

/// The first finger goes down, on the web backend's per-finger device.
pub(crate) fn web_first_finger_down(events: &mut Vec<egui::Event>, pos: egui::Pos2) {
    events.push(web_touch(WEB_FINGER_A, egui::TouchPhase::Start, pos));
    events.push(egui::Event::PointerMoved(pos));
    events.push(pointer_button(pos, true));
}

/// A second finger lands while the first stays down.
pub(crate) fn web_second_finger_down(events: &mut Vec<egui::Event>, pos: egui::Pos2) {
    events.push(web_touch(WEB_FINGER_B, egui::TouchPhase::Start, pos));
}

/// Both fingers move. Only the first drives the emulated pointer.
pub(crate) fn web_pinch_move(events: &mut Vec<egui::Event>, a: egui::Pos2, b: egui::Pos2) {
    events.push(web_touch(WEB_FINGER_A, egui::TouchPhase::Move, a));
    events.push(egui::Event::PointerMoved(a));
    events.push(web_touch(WEB_FINGER_B, egui::TouchPhase::Move, b));
}

/// Lift the **second** finger, leaving the first down — pinch ending with
/// one finger still on the glass.
pub(crate) fn web_second_finger_up(events: &mut Vec<egui::Event>, pos: egui::Pos2) {
    events.push(web_touch(WEB_FINGER_B, egui::TouchPhase::End, pos));
}

/// Lift the **first** finger — the one backing the emulated pointer —
/// while the second stays down. `egui-winit` releases and drops the pointer
/// here (`lib.rs:904`), so this is the ordering that can strand the map.
pub(crate) fn web_first_finger_up(events: &mut Vec<egui::Event>, pos: egui::Pos2) {
    events.push(web_touch(WEB_FINGER_A, egui::TouchPhase::End, pos));
    events.push(pointer_button(pos, false));
    events.push(egui::Event::PointerGone);
}
