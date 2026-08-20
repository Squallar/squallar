//! Platform-independent pointer and gesture handling for the map.

// Which half of this module is live depends on the target: desktop calls only
// `MapPointerFrame::from_mouse`, Android only the touch pipeline. The lint stays
// ON under `cfg(test)`, where the harness exercises both halves.
#![cfg_attr(not(test), allow(dead_code))]

/// Maximum time (seconds) between first tap release and second press
/// for it to count as a double-tap.
const DOUBLE_TAP_TIMEOUT_S: f64 = 0.4;
/// Maximum distance (pixels) between first and second tap positions
/// for it to count as a double-tap.
const DOUBLE_TAP_DISTANCE_PX: f32 = 50.0;
/// Maximum duration (seconds) for a press-release to classify as a "tap".
const TAP_DURATION_MAX_S: f64 = 0.3;
/// Maximum movement (pixels) for a press-release to classify as a "tap".
const TAP_DISTANCE_MAX_PX: f32 = 20.0;
/// Pixels of vertical drag per 1.0 zoom level change.
const ZOOM_DRAG_SENSITIVITY: f32 = 150.0;
/// Minimum hold duration (seconds) for a long press to be recognized.
const LONG_PRESS_DURATION_S: f64 = 0.8;
/// Maximum movement (pixels) during a long press before cancelling.
const LONG_PRESS_MAX_MOVE_PX: f32 = 20.0;
/// How long (seconds) a "pointer is down" belief survives complete pointer
/// silence before [`PointerTracker`] stops trusting it.
const POINTER_IDLE_TIMEOUT_S: f64 = 60.0;

/// The single touch device every finger is reported on after normalisation.
const CANONICAL_TOUCH_DEVICE: egui::TouchDeviceId = egui::TouchDeviceId(0);

/// Collapse every touch in a frame onto one device, so egui can see a pinch.
pub fn normalize_touch_devices(input: &mut egui::RawInput) {
    for event in &mut input.events {
        if let egui::Event::Touch { device_id, .. } = event {
            *device_id = CANONICAL_TOUCH_DEVICE;
        }
    }
}

/// CSS pixels one `DOM_DELTA_LINE` line is worth.
const PX_PER_WHEEL_LINE: f32 = 20.0;

/// Rewrite line-mode wheel events as pixel-mode ones, so a notch zooms the same
/// whichever way the browser spelled it.
pub fn normalize_wheel_units(input: &mut egui::RawInput, zoom_factor: f32) {
    let scale = PX_PER_WHEEL_LINE / zoom_factor.max(f32::EPSILON);
    for event in &mut input.events {
        if let egui::Event::MouseWheel { unit, delta, .. } = event
            && *unit == egui::MouseWheelUnit::Line
        {
            *unit = egui::MouseWheelUnit::Point;
            *delta *= scale;
        }
    }
}

/// One frame's pointer facts, with `down` corrected for sequences that egui
/// never ends. Produced by [`PointerTracker::read`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct PointerFrame {
    pub pressed: bool,
    pub released: bool,
    /// Whether a real finger/button is still down — **not** egui's raw
    /// `pointer.primary_down()`, see [`PointerTracker`].
    pub down: bool,
    /// Pointer position. egui's `interact_pos()`, which is `Some` on every
    /// frame where `down` is true — see [`PointerTracker::read`].
    pub pos: egui::Pos2,
    pub time: f64,
}

/// Decides whether egui's latched `pointer.primary_down()` can still be
/// believed, and is the single place any "the pointer went away" policy lives.
#[derive(Clone, Default)]
pub(crate) struct PointerTracker {
    /// Why egui's latched `down` is not currently believed, if it is not.
    lost: Option<LostCause>,
    sequence_live: bool,
    /// The touch id backing egui's emulated pointer, when this sequence started
    /// from a touch. `None` for mouse sequences, and whenever we did not see
    /// the `Touch{Start}` that opened the sequence.
    primary_touch: Option<egui::TouchId>,
    last_activity: Option<f64>,
}

/// Why [`PointerTracker`] stopped believing egui's latched `down`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LostCause {
    /// The pointer *went away* without a release — a cancelled touch, or the
    /// cursor leaving the window. **Terminal: only a fresh press clears it.**
    Gone,
    /// The idle backstop fired: nothing whatsoever arrived for
    /// [`POINTER_IDLE_TIMEOUT_S`]. **Motion undoes it.**
    Idle,
}

impl PointerTracker {
    /// Read this frame's pointer state. Call exactly once per frame, before any
    /// detector runs, and unconditionally — a frame skipped here is a
    /// cancellation missed.
    pub(crate) fn read(&mut self, ctx: &egui::Context) -> PointerFrame {
        ctx.input(|i| {
            let mut activity = false;
            let raw_down = i.pointer.primary_down();

            // --- order-independent frame facts ------------------------------
            // The integrations disagree about ordering within a frame, and a
            // whole gesture can arrive batched into one of them, so anything
            // that correlates two events of a frame is decided here.
            let mut touch_started = None;
            for event in &i.events {
                if let egui::Event::Touch {
                    id,
                    phase: egui::TouchPhase::Start,
                    ..
                } = event
                {
                    // First wins: eframe picks the primary as
                    // `all_touches.first()` and pushes changed touches in the
                    // same order (`web/input.rs:30`, `:85`).
                    touch_started.get_or_insert(*id);
                }
            }
            // The finger this frame's events belong to: the one already being
            // followed, or — when the frame opens the sequence — the one
            // starting in it — both halves are needed.
            let frame_primary = self.primary_touch.or(touch_started);

            // Walk the events in order so a cancel followed by a fresh press
            // within one frame ends up armed, not lost.
            for event in &i.events {
                match event {
                    egui::Event::PointerButton {
                        pressed, button, ..
                    } => {
                        activity = true;
                        // Only the primary button drives the sequence. `down`
                        // is `primary_down()`, so a right- or middle-click says
                        // nothing about the input we are tracking.
                        if *button == egui::PointerButton::Primary {
                            if *pressed {
                                // Adopt the finger only on a press that *opens*
                                // a sequence — not on eframe's re-emitted press
                                // for a second finger. "Opens" cannot be "we
                                // hold no finger", because a `Gone` leaves the
                                // old id in place with no release to clear it.
                                if !self.sequence_live || self.lost.is_some() {
                                    self.primary_touch = touch_started;
                                }
                                self.lost = None;
                                self.sequence_live = true;
                            } else {
                                self.sequence_live = false;
                            }
                        }
                    }
                    // The pointer vanished without a release: a cancelled touch,
                    // or the cursor leaving the window.
                    egui::Event::PointerGone => {
                        activity = true;
                        self.lost = Some(LostCause::Gone);
                    }
                    // A raw `Touch{Cancel}` acts on its own **only** for the
                    // finger backing the emulated pointer, so a secondary
                    // finger's cancel can never kill a live gesture — the whole
                    // cancellation path on the web, where `touchcancel` emits no
                    // release and no `PointerGone` (`eframe/src/web/events.rs:788`).
                    egui::Event::Touch { id, phase, .. } => {
                        activity = true;
                        if *phase == egui::TouchPhase::Cancel && Some(*id) == frame_primary {
                            self.lost = Some(LostCause::Gone);
                        }
                    }
                    // Motion is a sign of life, which undoes a timer running out
                    // but says nothing about a pointer that reported itself
                    // gone — see [`LostCause`].
                    egui::Event::PointerMoved(_) | egui::Event::MouseMoved(_) => {
                        activity = true;
                        if self.lost == Some(LostCause::Idle) {
                            self.lost = None;
                        }
                    }
                    _ => {}
                }
            }

            if activity || self.last_activity.is_none() {
                self.last_activity = Some(i.time);
            }

            // Backstop: if we believe a button is down but no pointer input at
            // all has arrived for a long time, the belief is stale. Latching
            // (rather than just ending one gesture) keeps the long-press detector
            // from picking the phantom finger back up.
            if raw_down
                && self.lost.is_none()
                && i.time - self.last_activity.unwrap_or(i.time) >= POINTER_IDLE_TIMEOUT_S
            {
                self.lost = Some(LostCause::Idle);
            }

            let down = raw_down && self.lost.is_none();

            // egui only lacks a position between a `PointerGone` and the next
            // positional event (`egui-0.34.1/src/input_state/mod.rs:1111`,
            // `:1208`) — and that is exactly the window in which a
            // `LostCause::Gone` is latched, so `down` is false throughout it.
            let pos = i.pointer.interact_pos();
            debug_assert!(
                pos.is_some() || !down,
                "pointer is down with no position: something cleared `lost` \
                 without positional evidence"
            );

            PointerFrame {
                pressed: i.pointer.primary_pressed(),
                released: i.pointer.primary_released(),
                down,
                pos: pos.unwrap_or_default(),
                time: i.time,
            }
        })
    }
}

/// The canonical dialog-blocking gate for map click positions: discard any
/// click that lands on a floating dialog or popup window (an egui layer ordered
/// above [`egui::Order::Background`]).
pub(crate) fn filter_dialog_blocked(
    ctx: &egui::Context,
    pos: Option<egui::Pos2>,
) -> Option<egui::Pos2> {
    pos.filter(|&pos| {
        !ctx.layer_id_at(pos)
            .is_some_and(|l| l.order > egui::Order::Background)
    })
}

/// Detects a "double-tap and drag" gesture commonly used on touch devices
/// for one-handed zooming. The gesture flow is:
/// 1. Tap (short press-release)
/// 2. Within [`DOUBLE_TAP_TIMEOUT_S`], press down again and hold
/// 3. Drag vertically: up = zoom in, down = zoom out
#[derive(Clone, Default)]
pub(crate) enum GestureState {
    #[default]
    Idle,
    WaitingForSecondTap {
        tap_time: f64,
        tap_pos: egui::Pos2,
    },
    ZoomDragging {
        drag_start_y: f32,
        initial_zoom: f64,
    },
}

#[derive(Clone)]
pub(crate) struct DoubleTapDragDetector {
    state: GestureState,
    /// A confirmed single tap this frame (no double-tap followed).
    confirmed_tap_pos: Option<egui::Pos2>,
    press_time: f64,
    press_pos: egui::Pos2,
}

impl Default for DoubleTapDragDetector {
    fn default() -> Self {
        Self {
            state: GestureState::Idle,
            confirmed_tap_pos: None,
            press_time: 0.0,
            press_pos: egui::Pos2::ZERO,
        }
    }
}

impl DoubleTapDragDetector {
    /// Process this frame's input and update the map zoom if a
    /// double-tap-drag gesture is active.
    pub(crate) fn update(
        &mut self,
        ctx: &egui::Context,
        input: PointerFrame,
        map_memory: &mut walkers::MapMemory,
        map_rect: egui::Rect,
    ) {
        let PointerFrame {
            pressed,
            released,
            down,
            pos,
            time,
            ..
        } = input;

        self.confirmed_tap_pos = None;

        if let GestureState::WaitingForSecondTap { tap_time, tap_pos } = self.state
            && time - tap_time >= DOUBLE_TAP_TIMEOUT_S
        {
            self.confirmed_tap_pos = Some(tap_pos);
            self.state = GestureState::Idle;
        }

        if let GestureState::ZoomDragging { .. } = self.state {
            self.handle_zoom_drag(pos, down, map_memory);
            return;
        }
        if pressed {
            self.handle_press(pos, time, map_memory);
        }
        if released {
            self.handle_release(pos, time);
            // Don't record taps on non-map UI (sidebar buttons, popups, etc.)
            // — check now while the current frame's layout is still valid,
            // rather than 0.4s later when the layout may have changed.
            if let GestureState::WaitingForSecondTap { .. } = self.state {
                let outside_map = !map_rect.contains(pos);
                let on_floating_ui = ctx
                    .layer_id_at(pos)
                    .is_some_and(|l| l.order > egui::Order::Background);
                if outside_map || on_floating_ui {
                    self.state = GestureState::Idle;
                }
            }
        }
    }

    /// While zoom-dragging, apply vertical drag to map zoom or end the gesture.
    fn handle_zoom_drag(
        &mut self,
        pos: egui::Pos2,
        down: bool,
        map_memory: &mut walkers::MapMemory,
    ) {
        if !down {
            self.state = GestureState::Idle;
            return;
        }
        if let GestureState::ZoomDragging {
            drag_start_y,
            initial_zoom,
        } = self.state
        {
            let dy = pos.y - drag_start_y;
            let zoom_delta = dy as f64 / ZOOM_DRAG_SENSITIVITY as f64;
            let new_zoom = (initial_zoom + zoom_delta).clamp(1.0, 19.0);
            let _ = map_memory.set_zoom(new_zoom);
        }
    }

    /// On press, check if this is the second tap of a double-tap sequence.
    fn handle_press(&mut self, pos: egui::Pos2, time: f64, map_memory: &mut walkers::MapMemory) {
        if let GestureState::WaitingForSecondTap { tap_time, tap_pos } = self.state {
            let dt = time - tap_time;
            let dist = (pos - tap_pos).length();
            if dt < DOUBLE_TAP_TIMEOUT_S && dist < DOUBLE_TAP_DISTANCE_PX {
                self.state = GestureState::ZoomDragging {
                    drag_start_y: pos.y,
                    initial_zoom: map_memory.zoom(),
                };
                return;
            }
        }
        self.press_time = time;
        self.press_pos = pos;
    }

    /// On release, classify the press-release as a tap or a drag/long-press.
    fn handle_release(&mut self, pos: egui::Pos2, time: f64) {
        let duration = time - self.press_time;
        let distance = (pos - self.press_pos).length();
        if duration < TAP_DURATION_MAX_S && distance < TAP_DISTANCE_MAX_PX {
            self.state = GestureState::WaitingForSecondTap {
                tap_time: time,
                tap_pos: pos,
            };
        } 
    }

    pub(crate) fn is_zooming(&self) -> bool {
        matches!(self.state, GestureState::ZoomDragging { .. })
    }

    /// Returns and consumes a confirmed single-tap position, if available.
    pub(crate) fn take_confirmed_tap(&mut self) -> Option<egui::Pos2> {
        self.confirmed_tap_pos.take()
    }
}

/// Detects a long-press gesture on touch devices.
#[derive(Clone, Default)]
pub(crate) struct LongPressDetector {
    /// Start time of the current press, or `None` if no finger is down.
    press_start: Option<f64>,
    press_pos: egui::Pos2,
    /// Whether the long press has been recognized (hold threshold exceeded).
    /// Once active, finger movement no longer cancels — the tooltip follows the finger.
    active: bool,
}

impl LongPressDetector {
    /// Process this frame's input and return the held position if a long press is active.
    pub(crate) fn update(&mut self, input: PointerFrame) -> Option<egui::Pos2> {
        let PointerFrame {
            down, pos, time, ..
        } = input;

        if !down {
            self.press_start = None;
            self.active = false;
            return None;
        }

        if self.active {
            return Some(pos);
        }

        if self.press_start.is_none() {
            self.press_start = Some(time);
            self.press_pos = pos;
            return None;
        }

        // Cancel if finger moved too far (only before activation). The
        // cancel clears the press, not the gesture: the pointer is still
        // down, so the next frame re-arms at wherever it is now. A pan
        // that *pauses* ≥ [`LONG_PRESS_DURATION_S`] mid-drag therefore grows
        // the hold and the popup takes the pan's remainder — deliberate.
        if (pos - self.press_pos).length() > LONG_PRESS_MAX_MOVE_PX {
            self.press_start = None;
            return None;
        }

        let elapsed = time - self.press_start.unwrap();
        if elapsed >= LONG_PRESS_DURATION_S {
            self.active = true;
            Some(pos)
        } else {
            None
        }
    }
}

/// The shortest drag, in points, that becomes a cross-section line.
pub(crate) const MIN_SECTION_DRAG_PT: f32 = 24.0;

/// What an armed modal drag saw this frame, in **screen** space.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum ArmedDragGesture {
    Idle,
    Anchored(egui::Pos2),
    /// Still down, now here. For the preview — a rubber band or a box; nothing
    /// is committed.
    Dragging(egui::Pos2),
    Released(egui::Pos2),
    /// The pointer went away without releasing — a cancelled touch, or the
    /// cursor leaving the window. Nothing committed, and the anchor is dropped.
    Cancelled,
}

/// Turns the pointer into an [`ArmedDragGesture`] while either modal drag is
/// armed.
#[derive(Clone, Default)]
pub(crate) struct ArmedDragDetector {
    /// Whether a press has opened a drag that no release has closed.
    drawing: bool,
}

impl ArmedDragDetector {
    /// Process this frame's pointer and say what the draw is doing.
    pub(crate) fn update(&mut self, input: PointerFrame) -> ArmedDragGesture {
        if input.pressed {
            self.drawing = true;
            return ArmedDragGesture::Anchored(input.pos);
        }
        if !self.drawing {
            return ArmedDragGesture::Idle;
        }
        if input.released {
            self.drawing = false;
            return ArmedDragGesture::Released(input.pos);
        }
        // `down` is the tracker's corrected answer, not egui's latched one, so
        // this is where a cancelled touch actually ends the draw.
        if !input.down {
            self.drawing = false;
            return ArmedDragGesture::Cancelled;
        }
        ArmedDragGesture::Dragging(input.pos)
    }
}

/// The active pane's resolved state for a frame in which either modal drag is
/// armed: what the drag saw, and the pointer frame the rest of the pane loop
/// must use.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct ArmedDragFrame {
    gesture: ArmedDragGesture,
    pointer: MapPointerFrame,
}

impl ArmedDragFrame {
    fn new(gesture: ArmedDragGesture) -> Self {
        Self {
            gesture,
            pointer: MapPointerFrame {
                // A press while armed is the first point of a line or the
                // centre of a box. Letting it also count as an overlay click
                // would open a storm-report popup over the map the user is
                // drawing on.
                overlay_click_pos: None,
                // Nothing long-presses while armed: the press is the gesture.
                long_press_pos: None,
                // Unconditional. The drag belongs to the armed mode.
                suppress_pan: true,
            },
        }
    }

    pub(crate) fn gesture(self) -> ArmedDragGesture {
        self.gesture
    }

    /// The pointer frame every other consumer in the pane loop must use.
    pub(crate) fn pointer(self) -> MapPointerFrame {
        self.pointer
    }
}

/// One pane's resolved pointer state for the current frame.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct MapPointerFrame {
    /// Screen position of a confirmed overlay click/tap, already passed through
    /// [`filter_dialog_blocked`], or `None` if nothing was clicked this frame.
    pub overlay_click_pos: Option<egui::Pos2>,
    pub long_press_pos: Option<egui::Pos2>,
    /// Whether map panning must be suppressed this frame (a zoom-drag or a
    /// long press owns the pointer).
    pub suppress_pan: bool,
}

/// One pane's resolved pointer state **as `render_panes` actually used it**.
#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct PanePointerProbe {
    pub pane_idx: usize,
    pub is_active: bool,
    pub modality: crate::ui_layout::PointerModality,
    pub frame: MapPointerFrame,
}

impl MapPointerFrame {
    /// A pane that takes no part in pointer interaction this frame: a touch
    /// gesture is in play and this pane does not own it. The touch pipeline is
    /// single-pointer and stateful, so it runs for the active pane only.
    pub(crate) fn inactive() -> Self {
        Self::default()
    }

    /// Desktop/mouse resolution: egui's built-in click detection (instant),
    /// with no gesture deferral.
    pub(crate) fn from_mouse(ctx: &egui::Context) -> Self {
        let click_pos = ctx.input(|i| {
            if i.pointer.any_click() {
                i.pointer.interact_pos()
            } else {
                None
            }
        });
        Self {
            overlay_click_pos: filter_dialog_blocked(ctx, click_pos),
            long_press_pos: None,
            suppress_pan: false,
        }
    }
}

/// The touch gesture detectors that run for the active pane, plus the shared
/// [`PointerTracker`] they are gated on.
#[derive(Clone, Default)]
pub(crate) struct TouchGestures {
    pub tracker: PointerTracker,
    pub double_tap: DoubleTapDragDetector,
    pub long_press: LongPressDetector,
    /// Whichever modal drag is armed — the cross-section draw or the 3D region
    /// pick. Lives here, beside the two touch detectors, because it shares
    /// their [`PointerTracker`] — exactly one of the two pipelines runs per
    /// frame for the active pane, so the tracker is read once either way.
    pub armed_drag: ArmedDragDetector,
}

impl TouchGestures {
    /// Run the touch gesture pipeline for the active pane and resolve this
    /// frame's pointer state.
    pub(crate) fn update(
        &mut self,
        ctx: &egui::Context,
        map_memory: &mut walkers::MapMemory,
        pane_rect: egui::Rect,
    ) -> MapPointerFrame {
        let input = self.tracker.read(ctx);

        self.double_tap.update(ctx, input, map_memory, pane_rect);
        let is_zoom_dragging = self.double_tap.is_zooming();

        // Chrome-filtered like the tap below (§5.9): a long press is a map
        // gesture — it raises the value tooltip — and a hold that starts on
        // gesture — and a hold that starts on the floating timeline or a pill
        // row is a hold on *that* control. The filter runs on the held position
        // each frame, the same gate the click goes through.
        let long_press_pos = if is_zoom_dragging {
            None
        } else {
            filter_dialog_blocked(ctx, self.long_press.update(input))
        };

        let overlay_click_pos = filter_dialog_blocked(ctx, self.double_tap.take_confirmed_tap());

        MapPointerFrame {
            overlay_click_pos,
            long_press_pos,
            suppress_pan: is_zoom_dragging || long_press_pos.is_some(),
        }
    }
}

/// Owns the touch gesture detectors and decides, per frame, whether they run
/// at all.
#[derive(Clone, Default)]
pub(crate) struct InteractionState {
    gestures: TouchGestures,
    /// The modality the last frame ran under, so a change can be noticed.
    last_modality: Option<crate::ui_layout::PointerModality>,
}

impl InteractionState {
    /// Resolve the **active** pane's pointer state for this frame.
    pub(crate) fn resolve_active(
        &mut self,
        ctx: &egui::Context,
        modality: crate::ui_layout::PointerModality,
        compact: bool,
        map_memory: &mut walkers::MapMemory,
        pane_rect: egui::Rect,
    ) -> MapPointerFrame {
        use crate::ui_layout::PointerModality;

        self.settle_modality(modality);

        match modality {
            PointerModality::Touch => self.gestures.update(ctx, map_memory, pane_rect),
            PointerModality::Mouse => {
                let mut frame = MapPointerFrame::from_mouse(ctx);
                if compact {
                    // The same detector, the same chrome filter, the same
                    // pan suppression the touch pipeline applies — a hold
                    // is a hold whichever pointer spells it.
                    let input = self.gestures.tracker.read(ctx);
                    let held = filter_dialog_blocked(ctx, self.gestures.long_press.update(input));
                    frame.suppress_pan |= held.is_some();
                    frame.long_press_pos = held;
                }
                frame
            }
        }
    }

    /// Resolve the active pane for a frame in which a modal drag is **armed**
    /// — the cross-section draw or the 3D region pick — whichever pointer the
    /// user has.
    pub(crate) fn resolve_armed(
        &mut self,
        ctx: &egui::Context,
        modality: crate::ui_layout::PointerModality,
    ) -> ArmedDragFrame {
        self.settle_modality(modality);
        let input = self.gestures.tracker.read(ctx);
        ArmedDragFrame::new(self.gestures.armed_drag.update(input))
    }

    /// A modality change abandons any gesture in flight.
    fn settle_modality(&mut self, modality: crate::ui_layout::PointerModality) {
        if self.last_modality != Some(modality) {
            self.gestures = TouchGestures::default();
            self.last_modality = Some(modality);
        }
    }

    pub(crate) fn resolve_inactive(
        &self,
        ctx: &egui::Context,
        modality: crate::ui_layout::PointerModality,
    ) -> MapPointerFrame {
        match modality {
            crate::ui_layout::PointerModality::Touch => MapPointerFrame::inactive(),
            crate::ui_layout::PointerModality::Mouse => MapPointerFrame::from_mouse(ctx),
        }
    }
}

#[cfg(test)]
mod tests;
