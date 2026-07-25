//! Headless input harness for [`Gui::ui`].
//!
//! Drives the real UI through a real [`egui::Context`] with hand-constructed
//! [`egui::RawInput`] — no window, no winit, no wgpu. Each [`InputHarness::frame`]
//! runs one full egui pass (`Gui::ui`, all panels, dialogs and map panes) and
//! then resolves the pane pointer state through the *same* entry points that
//! `ui_map.rs` uses:
//!
//! * [`MapPointerFrame::from_mouse`] — the desktop path
//! * [`TouchGestures::update`] — the Android path
//!
//! so a regression in either one fails here rather than on a device.
//!
//! # Event fidelity
//!
//! The pointer helpers emit exactly the event sequences the real integrations
//! produce, which is what makes the cancellation tests meaningful. They do not
//! agree with each other, and the disagreements are the whole reason the
//! tracker is shaped the way it is.
//!
//! `egui-winit` 0.35.0 (`src/lib.rs`) — `on_touch`'s body is byte-identical to
//! 0.34.1's, so every row below survived the bump unchanged:
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
//! eframe 0.35.0's web canvas (`src/web/events.rs`) — the four touch handlers
//! are likewise byte-identical to 0.34.1's:
//!
//! | DOM event     | emitted here                                                |
//! |---------------|-------------------------------------------------------------|
//! | `touchstart`  | `PointerButton{down}` **then** `Touch{Start}` — order flipped |
//! | `touchmove`   | `PointerMoved`, `Touch{Move}`                               |
//! | `touchend`    | `PointerButton{up}`, `PointerGone`, `Touch{End}`            |
//! | `touchcancel` | `Touch{Cancel}` **alone** — no release, no `PointerGone`    |
//! | `mousemove`   | `PointerMoved`                                              |
//!
//! Two rows carry the weight. A cancelled touch never reports a release and
//! egui does not clear `pointer.down` on `PointerGone`, so any gesture that
//! only exits on "pointer up" stays stuck forever — and on the web there is no
//! `PointerGone` either, so a tracker keying on that alone never notices the
//! cancellation at all.

use crate::Gui;
use crate::ui_input::{MapPointerFrame, TouchGestures};

/// Viewport size used by the harness — a landscape desktop-ish window.
const SCREEN_SIZE: egui::Vec2 = egui::vec2(1024.0, 768.0);

/// Nominal seconds between harness frames (only used by [`InputHarness::frame`]).
const FRAME_DT: f64 = 1.0 / 60.0;

/// The pane pointer state produced by one harness frame.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct FrameOutcome {
    /// Pointer resolution taken by the desktop build (egui click detection).
    pub mouse: MapPointerFrame,
    /// Pointer resolution taken by the Android build (touch gesture pipeline).
    pub touch: MapPointerFrame,
    /// Map zoom after the frame — observes the double-tap-drag zoom gesture.
    pub zoom: f64,
}

/// Drives [`Gui::ui`] frame by frame with synthetic input.
pub(crate) struct InputHarness {
    ctx: egui::Context,
    gui: Gui,
    /// Touch gesture detectors for the "active pane", as `ui_map.rs` keeps them.
    gestures: TouchGestures,
    /// Map viewport the zoom gesture acts on.
    map_memory: walkers::MapMemory,
    /// Screen rect of the active pane's map, used to reject taps on chrome.
    pane_rect: egui::Rect,
    /// Wall-clock time reported to egui, in seconds.
    time: f64,
    /// Events queued for the next frame.
    events: Vec<egui::Event>,
    screen_rect: egui::Rect,
    /// Every rect painted during the last frame, in paint order. Lets a test
    /// assert on what was actually *drawn* rather than on an intermediate value
    /// — the only way to pin that a resolved decision reached the renderer.
    last_rects: Vec<egui::Rect>,
    /// `RawInput::max_texture_side` — what `egui_winit` is handed from
    /// `device.limits().max_texture_dimension_2d`, and what
    /// `plan_overlay_texture` reads back through `ui.ctx().input(..)`.
    /// `None` leaves egui on its own default of 2048.
    max_texture_side: Option<usize>,
    /// The [`GuiAction`]s `Gui::ui` returned from the last frame.
    last_actions: Vec<crate::actions::GuiAction>,
}

impl InputHarness {
    /// Build a harness with a fresh [`Gui`] and run enough frames for egui to
    /// settle (areas need a frame to register their rects).
    pub(crate) fn new() -> Self {
        Self::with_screen(SCREEN_SIZE)
    }

    /// A harness on a screen of the given size — e.g. a portrait phone, where
    /// the pane grid and the panel disagree about which way up they are.
    pub(crate) fn with_screen(size: egui::Vec2) -> Self {
        let screen_rect = egui::Rect::from_min_size(egui::Pos2::ZERO, size);
        let mut harness = Self {
            ctx: egui::Context::default(),
            gui: Gui::new(),
            gestures: TouchGestures::default(),
            map_memory: walkers::MapMemory::default(),
            // The map occupies the middle of the window: inset generously so
            // the harness never depends on exact panel widths.
            pane_rect: egui::Rect::from_min_max(
                egui::pos2(220.0, 80.0),
                egui::pos2(1004.0, 690.0),
            ),
            time: 100.0,
            events: Vec::new(),
            screen_rect,
            last_rects: Vec::new(),
            max_texture_side: None,
            last_actions: Vec::new(),
        };
        harness.warm_up();
        harness
    }

    /// Report `side` as the adapter's `max_texture_dimension_2d`, the way
    /// `EguiRenderer::new` reports the real device's limit to `egui_winit`.
    ///
    /// This is how a WebGL2-class limit is exercised without a wasm target: the
    /// number reaches `plan_overlay_texture` through exactly the path it does in
    /// the real app, `RawInput` -> `InputState` -> `ui.ctx().input(..)`.
    pub(crate) fn set_max_texture_side(&mut self, side: usize) {
        self.max_texture_side = Some(side);
        self.warm_up();
    }

    /// The actions the last frame's `Gui::ui` emitted.
    pub(crate) fn last_actions(&self) -> &[crate::actions::GuiAction] {
        &self.last_actions
    }

    /// Split the map into `count` panes, as the settings UI does.
    pub(crate) fn set_pane_count(&mut self, count: usize) {
        self.gui.set_pane_count_for_test(count);
        self.warm_up();
    }

    /// The pane rects the real layout produces inside the map panel.
    pub(crate) fn pane_rects(&self) -> Vec<egui::Rect> {
        self.gui.pane_rects_for_test()
    }

    /// The rect the pane grid is laid out in, as `render_map` sees it.
    pub(crate) fn map_panel_rect(&self) -> egui::Rect {
        self.gui.map_panel_rect_for_test()
    }

    /// The color-scale legend strips painted inside `pane`, classified by the
    /// axis they were drawn along.
    ///
    /// `render_color_scale` paints the bar as a run of 2px strips: `(2, 20)`
    /// for a bottom-edge bar, `(20, 2)` for a right-edge one
    /// (`ui_map_pane.rs:632` — `SCALE_BAR_WIDTH` is 20). That signature is what
    /// makes it possible to assert on the drawn result rather than on the value
    /// that was supposed to produce it.
    pub(crate) fn color_scale_strips(&self, pane: egui::Rect) -> (usize, usize) {
        let mut horizontal = 0;
        let mut vertical = 0;
        for rect in &self.last_rects {
            if !pane.contains(rect.center()) {
                continue;
            }
            let (w, h) = (rect.width(), rect.height());
            if (h - 20.0).abs() < 0.5 && w <= 4.0 {
                horizontal += 1;
            } else if (w - 20.0).abs() < 0.5 && h <= 4.0 {
                vertical += 1;
            }
        }
        (horizontal, vertical)
    }

    /// Run a few input-free frames so panels, areas and windows have registered
    /// their layer rects before any assertion depends on them.
    pub(crate) fn warm_up(&mut self) {
        for _ in 0..3 {
            self.frame();
        }
    }

    /// The centre of the map pane — a safe "on the map" position.
    pub(crate) fn map_center(&self) -> egui::Pos2 {
        self.pane_rect.center()
    }

    /// The centre of the viewport, where modal dialogs are placed.
    pub(crate) fn screen_center(&self) -> egui::Pos2 {
        self.screen_rect.center()
    }

    /// Mutable access to the UI under test (e.g. to open a dialog).
    pub(crate) fn gui_mut(&mut self) -> &mut Gui {
        &mut self.gui
    }

    /// Whether a floating layer (dialog / popup) currently covers `pos`.
    /// Used by tests to assert their own preconditions.
    pub(crate) fn is_floating_layer_at(&self, pos: egui::Pos2) -> bool {
        self.ctx
            .layer_id_at(pos)
            .is_some_and(|l| l.order > egui::Order::Background)
    }

    /// Current map zoom.
    pub(crate) fn zoom(&self) -> f64 {
        self.map_memory.zoom()
    }

    /// Advance the harness clock without running a frame.
    pub(crate) fn advance(&mut self, seconds: f64) {
        self.time += seconds;
    }

    /// Advance the clock by `seconds`, then run one frame.
    pub(crate) fn frame_after(&mut self, seconds: f64) -> FrameOutcome {
        self.advance(seconds);
        self.frame()
    }

    /// Run `count` frames spaced `seconds` apart and return the last outcome.
    pub(crate) fn frames_for(&mut self, count: usize, seconds: f64) -> FrameOutcome {
        let mut outcome = FrameOutcome::default();
        for _ in 0..count {
            outcome = self.frame_after(seconds);
        }
        outcome
    }

    /// Run input-free frames for `seconds` of wall clock, asserting `check` on
    /// **every** frame.
    ///
    /// Watching only the last frame is how a re-arming gesture slips through: a
    /// stuck long press needs [`LONG_PRESS_DURATION_S`] to come back, so any
    /// "it stayed released" assertion has to cover well past that, frame by
    /// frame.
    pub(crate) fn assert_every_frame_for(
        &mut self,
        seconds: f64,
        step: f64,
        mut check: impl FnMut(usize, &FrameOutcome),
    ) -> FrameOutcome {
        let count = (seconds / step).ceil() as usize;
        let mut outcome = FrameOutcome::default();
        for frame in 0..count {
            outcome = self.frame_after(step);
            check(frame, &outcome);
        }
        outcome
    }

    // --- mouse input (mirrors egui-winit's cursor + button handling) --------

    pub(crate) fn mouse_move(&mut self, pos: egui::Pos2) {
        self.events.push(egui::Event::PointerMoved(pos));
    }

    pub(crate) fn mouse_press(&mut self, pos: egui::Pos2) {
        self.mouse_move(pos);
        self.events.push(pointer_button(pos, true));
    }

    pub(crate) fn mouse_release(&mut self, pos: egui::Pos2) {
        self.mouse_move(pos);
        self.events.push(pointer_button(pos, false));
    }

    /// The cursor left the window: `egui-winit` maps `WindowEvent::CursorLeft`
    /// to a bare [`egui::Event::PointerGone`] and forgets the pointer position
    /// (`egui-winit-0.34.1/src/lib.rs:340`). **No release is reported** — and
    /// while the position is forgotten, a real mouse release happening outside
    /// the window is dropped on the floor too (`lib.rs:796`), which is why
    /// egui's `primary_down()` can stay latched across the excursion.
    pub(crate) fn cursor_left(&mut self) {
        self.events.push(egui::Event::PointerGone);
    }

    /// Raw device motion (`DeviceEvent::MouseMotion` → [`egui::Event::MouseMoved`]).
    /// It carries a delta and **no position**, so egui has nothing to put in
    /// `interact_pos()` on such a frame.
    ///
    /// No integration in this workspace actually produces this:
    /// `egui-winit`'s `on_mouse_motion` (`lib.rs:759`) is reachable only from
    /// `DeviceEvent`, and `rustdar-platform/src/egui_renderer.rs:59` forwards
    /// `on_window_event` only. It is here to exercise the tracker's defensive
    /// position fallback, and to prove a delta with no coordinates cannot
    /// resurrect a cancelled touch.
    pub(crate) fn mouse_moved_raw(&mut self, delta: egui::Vec2) {
        self.events.push(egui::Event::MouseMoved(delta));
    }

    // --- web input (mirrors eframe 0.34.1's canvas listeners) ---------------

    /// `touchstart`, as eframe's web canvas emits it: the primary
    /// `PointerButton{pressed}` **first**, then `push_touches(Start)`
    /// (`eframe/src/web/events.rs:676`) — the opposite order to `egui-winit`,
    /// which is why the tracker correlates the pair over the whole frame.
    pub(crate) fn web_touch_start(&mut self, pos: egui::Pos2) {
        self.events.push(pointer_button(pos, true));
        self.events.push(touch(egui::TouchPhase::Start, pos));
    }

    /// `touchmove` (`events.rs:709`): a bare `PointerMoved`, with the raw touch
    /// pushed alongside it.
    pub(crate) fn web_touch_move(&mut self, pos: egui::Pos2) {
        self.events.push(egui::Event::PointerMoved(pos));
        self.events.push(touch(egui::TouchPhase::Move, pos));
    }

    /// `touchcancel` (`events.rs:788`): `push_touches(Cancel)` and **nothing
    /// else** — no release, no `PointerGone`. egui's `primary_down()` therefore
    /// stays latched `true` with no event ever clearing it, so a tracker that
    /// keys cancellation on `PointerGone` alone never fires at all here.
    pub(crate) fn web_touch_cancel(&mut self, pos: egui::Pos2) {
        self.events.push(touch(egui::TouchPhase::Cancel, pos));
    }

    /// `mousemove` (`events.rs:627`): a bare `PointerMoved`. Note this reaches
    /// the canvas whether or not any touch is involved, which is what makes a
    /// motion-based un-latch dangerous after a cancellation on the web.
    pub(crate) fn web_mouse_move(&mut self, pos: egui::Pos2) {
        self.events.push(egui::Event::PointerMoved(pos));
    }

    // --- touch input (mirrors egui-winit's `on_touch`) ----------------------

    pub(crate) fn touch_start(&mut self, pos: egui::Pos2) {
        self.events.push(touch(egui::TouchPhase::Start, pos));
        self.events.push(egui::Event::PointerMoved(pos));
        self.events.push(pointer_button(pos, true));
    }

    pub(crate) fn touch_move(&mut self, pos: egui::Pos2) {
        self.events.push(touch(egui::TouchPhase::Move, pos));
        self.events.push(egui::Event::PointerMoved(pos));
    }

    pub(crate) fn touch_end(&mut self, pos: egui::Pos2) {
        self.events.push(touch(egui::TouchPhase::End, pos));
        self.events.push(pointer_button(pos, false));
        self.events.push(egui::Event::PointerGone);
    }

    /// The OS/browser took the gesture away: **no release is reported**, only
    /// `PointerGone`, exactly as `egui-winit` does for `TouchPhase::Cancelled`.
    pub(crate) fn touch_cancel(&mut self, pos: egui::Pos2) {
        self.events.push(touch(egui::TouchPhase::Cancel, pos));
        self.events.push(egui::Event::PointerGone);
    }

    /// A *secondary* finger's touch being cancelled: a raw `Touch{Cancel}` for
    /// another `TouchId`, with no `PointerGone`, since the emulated pointer is
    /// still owned by the primary finger.
    pub(crate) fn secondary_touch_cancel(&mut self, pos: egui::Pos2) {
        self.events.push(egui::Event::Touch {
            device_id: egui::TouchDeviceId(0),
            id: egui::TouchId(1),
            phase: egui::TouchPhase::Cancel,
            pos,
            force: None,
        });
    }

    // --- composite gestures -------------------------------------------------

    /// A quick touch tap (press + release within the tap thresholds), spread
    /// over two frames like a real one.
    pub(crate) fn touch_tap(&mut self, pos: egui::Pos2) -> FrameOutcome {
        self.touch_start(pos);
        self.frame_after(FRAME_DT);
        self.touch_end(pos);
        self.frame_after(0.05)
    }

    /// A quick mouse click (press + release), spread over two frames.
    pub(crate) fn mouse_click(&mut self, pos: egui::Pos2) -> FrameOutcome {
        self.mouse_press(pos);
        self.frame_after(FRAME_DT);
        self.mouse_release(pos);
        self.frame_after(0.05)
    }

    /// Run one egui pass: `Gui::ui` followed by the pane pointer resolution.
    pub(crate) fn frame(&mut self) -> FrameOutcome {
        let raw_input = egui::RawInput {
            screen_rect: Some(self.screen_rect),
            time: Some(self.time),
            events: std::mem::take(&mut self.events),
            max_texture_side: self.max_texture_side,
            ..Default::default()
        };

        // `begin_pass`/`end_pass` rather than `run_ui`, so the body runs exactly
        // once per frame: a repeated pass would feed the same events to the
        // gesture detectors twice.
        let ctx = self.ctx.clone();
        ctx.begin_pass(raw_input);

        // The real UI, panels, dialogs and map panes included.
        self.last_actions = self.gui.ui(&ctx);

        // The same two entry points `ui_map.rs` uses for the active pane. Both
        // funnel their click position through `filter_dialog_blocked`.
        let outcome = FrameOutcome {
            mouse: MapPointerFrame::from_mouse(&ctx),
            touch: self
                .gestures
                .update(&ctx, &mut self.map_memory, self.pane_rect),
            zoom: self.map_memory.zoom(),
        };

        let full_output = ctx.end_pass();
        self.last_rects = full_output
            .shapes
            .iter()
            .filter_map(|clipped| match &clipped.shape {
                egui::Shape::Rect(rect_shape) => Some(rect_shape.rect),
                _ => None,
            })
            .collect();
        outcome
    }
}

fn pointer_button(pos: egui::Pos2, pressed: bool) -> egui::Event {
    egui::Event::PointerButton {
        pos,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: egui::Modifiers::default(),
    }
}

fn touch(phase: egui::TouchPhase, pos: egui::Pos2) -> egui::Event {
    egui::Event::Touch {
        device_id: egui::TouchDeviceId(0),
        id: egui::TouchId(0),
        phase,
        pos,
        force: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The two durations that bracket the idle backstop, deliberately **not**
    /// derived from `POINTER_IDLE_TIMEOUT_S`.
    ///
    /// A probe that sizes its own loop off the constant under test cannot
    /// notice that constant changing — it just moves with it, and both a 32s
    /// and a 600s backstop pass. These are absolute claims about the behaviour
    /// instead: a still hold must survive the first, and a pointer that has
    /// gone silent must not survive the second.
    const HOLD_MUST_SURVIVE_S: f64 = 45.0;
    const SILENCE_MUST_EXPIRE_S: f64 = 90.0;

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
        assert_eq!(pressed.touch.long_press_pos, None, "not held long enough yet");
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
        assert!(dragging.touch.suppress_pan, "zoom drag must block map panning");
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
        assert!(dragging.touch.suppress_pan, "precondition: zoom drag active");

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
        assert_eq!(held.touch.long_press_pos, Some(pos), "precondition: long press");
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
        assert_eq!(gone.touch.long_press_pos, None, "the held position must not stick");
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
        assert!(h.frame_after(0.05).touch.suppress_pan, "precondition: zoom drag");

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
        assert_eq!(held.touch.long_press_pos, Some(pos), "precondition: long press");

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
    ///    pinning was that `render_map` resolves from the panel, that the
    ///    answer is threaded through `PaneRenderCtx`, and that neither renderer
    ///    quietly recomputes it from the pane it happens to be drawing.
    #[test]
    fn every_pane_draws_its_color_scale_on_the_same_edge() {
        let mut h = InputHarness::with_screen(egui::vec2(1080.0, 1273.0));
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

    /// 7. A tap that lands on a floating dialog is filtered out by the
    ///    dialog-blocking gate — for both the mouse and the touch path.
    #[test]
    fn tap_on_floating_dialog_is_filtered_out() {
        let mut h = InputHarness::new();
        h.gui_mut().show_settings = true;
        h.warm_up();

        let pos = h.screen_center();
        assert!(
            h.is_floating_layer_at(pos),
            "precondition: the settings dialog must cover the viewport centre"
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
        h.gui_mut().show_settings = false;
        h.warm_up();
        assert!(!h.is_floating_layer_at(pos));
        let clicked = h.mouse_click(pos);
        assert_eq!(clicked.mouse.overlay_click_pos, Some(pos));
    }

    /// 7b. A touch tap is deferred by 0.4s, so a dialog can open *during* the
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
        h.gui_mut().show_settings = true;
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
        h.gui_mut().show_settings = false;
        h.warm_up();
        h.touch_tap(pos);
        let confirmed = h.frame_after(AFTER_DOUBLE_TAP_TIMEOUT);
        assert_eq!(confirmed.touch.overlay_click_pos, Some(pos));
    }

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
            unclamped.iter().any(|p| p.width > LIMIT || p.height > LIMIT),
            "fixture must cross the limit before it is imposed, else the clamp is never \
             exercised; got {unclamped:?}"
        );

        h.set_max_texture_side(LIMIT as usize);
        let clamped = requested_plans(&h);
        assert!(!clamped.is_empty(), "still expected a render request after clamping");
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
}
