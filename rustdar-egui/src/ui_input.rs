//! Platform-independent pointer and gesture handling for the map.
//!
//! Everything here is pure `egui` pointer + wall-clock logic — no Android,
//! winit or wgpu APIs — so it compiles on every target and can be exercised
//! headlessly by the input harness (`input_harness.rs`).
//!
//! Two consumers drive this module:
//! * `ui_map.rs` resolves each pane's pointer state once per frame
//!   ([`MapPointerFrame`]), from the mouse on desktop and from [`TouchGestures`]
//!   on Android.
//! * the headless harness drives the identical entry points from tests.

// Which half of this module is live depends on the target: the desktop build
// only calls `MapPointerFrame::from_mouse`, the Android build only the touch
// pipeline, and the harness calls both. Everything stays compiled everywhere so
// the follow-on responsive UI has a single implementation to adopt, so don't
// warn about the half that the current target happens not to use.
#![allow(dead_code)]

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
/// Wall-clock backstop (seconds) for a zoom-drag gesture.
///
/// Neither a release nor a cancellation event is guaranteed to arrive (a
/// suspended activity may simply stop delivering pointer input), so the gesture
/// also expires on its own. A one-handed zoom drag lasts a second or two, so
/// this only ever fires on a stuck gesture.
const ZOOM_DRAG_MAX_DURATION_S: f64 = 10.0;
/// Minimum hold duration (seconds) for a long press to be recognized.
const LONG_PRESS_DURATION_S: f64 = 0.8;
/// Maximum movement (pixels) during a long press before cancelling.
const LONG_PRESS_MAX_MOVE_PX: f32 = 20.0;

/// Whether an event ends a pointer sequence *without* reporting a release.
///
/// This is the crux of the "stuck gesture" class of bugs. When the OS or the
/// browser takes over a touch sequence (Android system edge gesture, incoming
/// call, notification shade, `touchcancel` on the web):
///
/// * `egui-winit` maps `winit::event::TouchPhase::Cancelled` to **only**
///   [`egui::Event::PointerGone`] — no `PointerButton { pressed: false }`.
/// * `egui`'s `InputState` deliberately does not treat `PointerGone` as a
///   release ("when dragging a slider and the mouse leaves the viewport, we
///   still want the drag to work"), so `pointer.primary_down()` stays `true`
///   forever.
///
/// A gesture that is only exited on `!down` therefore never exits. Detectors
/// must treat these events as an end-of-gesture.
///
/// Note that `PointerGone` is *also* emitted right after a normal touch-up, in
/// the same frame as the release — so acting on it matches the release path and
/// changes nothing for well-behaved gestures.
fn is_gesture_cancel_event(event: &egui::Event) -> bool {
    matches!(
        event,
        egui::Event::PointerGone
            | egui::Event::Touch {
                phase: egui::TouchPhase::Cancel,
                ..
            }
    )
}

/// The canonical dialog-blocking gate for map click positions: discard any
/// click that lands on a floating dialog or popup window (an egui layer ordered
/// above [`egui::Order::Background`]).
///
/// **CONVENTION:** new map click handlers MUST consume the pre-filtered
/// `PaneRenderCtx::overlay_click_pos`, which comes from here — never read raw
/// click events via `ctx.input()` for map-level interactions, as that bypasses
/// dialog blocking. `is_pos_blocked()` in `ui_map_overlays.rs` applies this same
/// rule plus the pane-rect and excluded-rect checks.
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
        /// Wall-clock time the drag was entered, for the expiry backstop.
        start_time: f64,
    },
}

#[derive(Clone)]
pub(crate) struct DoubleTapDragDetector {
    /// The current gesture state.
    state: GestureState,
    /// A confirmed single tap this frame (no double-tap followed).
    confirmed_tap_pos: Option<egui::Pos2>,
    /// Time when the current/last primary press started
    press_time: f64,
    /// Position where the current/last primary press started
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
    ///
    /// `map_rect` is the current pane's screen rect — taps outside it are
    /// discarded so that sidebar buttons and other non-map UI don't become
    /// deferred overlay clicks.
    pub(crate) fn update(
        &mut self,
        ctx: &egui::Context,
        map_memory: &mut walkers::MapMemory,
        map_rect: egui::Rect,
    ) {
        let (pressed, released, down, pos, time, cancelled) = ctx.input(|i| {
            (
                i.pointer.primary_pressed(),
                i.pointer.primary_released(),
                i.pointer.primary_down(),
                i.pointer.interact_pos(),
                i.time,
                i.events.iter().any(is_gesture_cancel_event),
            )
        });
        let pos = pos.unwrap_or(egui::Pos2::ZERO);

        // Clear last frame's confirmed tap
        self.confirmed_tap_pos = None;

        // Promote pending tap to confirmed if double-tap timeout elapsed
        if let GestureState::WaitingForSecondTap { tap_time, tap_pos } = self.state
            && time - tap_time >= DOUBLE_TAP_TIMEOUT_S
        {
            self.confirmed_tap_pos = Some(tap_pos);
            self.state = GestureState::Idle;
        }

        if let GestureState::ZoomDragging { .. } = self.state {
            self.handle_zoom_drag(pos, down, cancelled, time, map_memory);
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
        cancelled: bool,
        time: f64,
        map_memory: &mut walkers::MapMemory,
    ) {
        if let GestureState::ZoomDragging { drag_start_y, initial_zoom, start_time } = self.state {
            // Exit on lift, on a cancelled pointer sequence (which never
            // reports a release — see `is_gesture_cancel_event`), or once the
            // gesture outlives its wall-clock backstop. Missing any of these
            // strands the detector in `ZoomDragging`, which pins `suppress_pan`
            // and leaves the map permanently un-pannable.
            if !down || cancelled || time - start_time >= ZOOM_DRAG_MAX_DURATION_S {
                self.state = GestureState::Idle;
                return;
            }
            let dy = pos.y - drag_start_y;
            let zoom_delta = dy as f64 / ZOOM_DRAG_SENSITIVITY as f64;
            let new_zoom = (initial_zoom + zoom_delta).clamp(1.0, 19.0);
            let _ = map_memory.set_zoom(new_zoom);
        }
    }

    /// On press, check if this is the second tap of a double-tap sequence.
    fn handle_press(
        &mut self,
        pos: egui::Pos2,
        time: f64,
        map_memory: &mut walkers::MapMemory,
    ) {
        if let GestureState::WaitingForSecondTap { tap_time, tap_pos } = self.state {
            let dt = time - tap_time;
            let dist = (pos - tap_pos).length();
            if dt < DOUBLE_TAP_TIMEOUT_S && dist < DOUBLE_TAP_DISTANCE_PX {
                self.state = GestureState::ZoomDragging {
                    drag_start_y: pos.y,
                    initial_zoom: map_memory.zoom(),
                    start_time: time,
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
        } else {
            // Long press or drag — not a tap, don't record
        }
    }

    /// Whether a zoom-drag gesture is currently active.
    pub(crate) fn is_zooming(&self) -> bool {
        matches!(self.state, GestureState::ZoomDragging { .. })
    }

    /// Returns and consumes a confirmed single-tap position, if available.
    ///
    /// A tap is only confirmed after [`DOUBLE_TAP_TIMEOUT_S`] elapses without
    /// a second press, ensuring double-tap-to-zoom doesn't trigger overlay popups.
    pub(crate) fn take_confirmed_tap(&mut self) -> Option<egui::Pos2> {
        self.confirmed_tap_pos.take()
    }
}

/// Detects a long-press gesture on touch devices.
///
/// When the user holds a finger down for [`LONG_PRESS_DURATION_S`] without
/// moving more than [`LONG_PRESS_MAX_MOVE_PX`], this reports the held position.
#[derive(Clone, Default)]
pub(crate) struct LongPressDetector {
    /// Start time of the current press, or `None` if no finger is down.
    press_start: Option<f64>,
    /// Position where the current press started.
    press_pos: egui::Pos2,
    /// Whether the long press has been recognized (hold threshold exceeded).
    /// Once active, finger movement no longer cancels — the tooltip follows the finger.
    active: bool,
}

impl LongPressDetector {
    /// Process this frame's input and return the held position if a long press is active.
    ///
    /// Once the hold threshold is exceeded, returns the **current** finger position
    /// (not the initial press position), allowing the tooltip to follow the finger.
    pub(crate) fn update(&mut self, ctx: &egui::Context) -> Option<egui::Pos2> {
        let (down, pos, time, cancelled) = ctx.input(|i| {
            (
                i.pointer.primary_down(),
                i.pointer.interact_pos(),
                i.time,
                i.events.iter().any(is_gesture_cancel_event),
            )
        });
        let pos = pos.unwrap_or(egui::Pos2::ZERO);

        // A cancelled pointer sequence never reports a release, so `down` stays
        // `true` forever (see `is_gesture_cancel_event`). Treat it exactly like
        // a lift: otherwise an active long press keeps reporting a position,
        // which pins the tooltip and suppresses map panning for good.
        //
        // No wall-clock backstop here on purpose: an intentional hold has no
        // natural end (the tooltip is meant to stay while the finger rests), so
        // expiring it would break the feature rather than protect it.
        if !down || cancelled {
            self.press_start = None;
            self.active = false;
            return None;
        }

        // Already recognized — follow the finger
        if self.active {
            return Some(pos);
        }

        if self.press_start.is_none() {
            self.press_start = Some(time);
            self.press_pos = pos;
            return None;
        }

        // Cancel if finger moved too far (only before activation)
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

/// One pane's resolved pointer state for the current frame.
///
/// Produced by [`MapPointerFrame::from_mouse`] (desktop) or
/// [`TouchGestures::update`] (touch), and consumed by `ui_map.rs`.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct MapPointerFrame {
    /// Screen position of a confirmed overlay click/tap, already passed through
    /// [`filter_dialog_blocked`], or `None` if nothing was clicked this frame.
    pub overlay_click_pos: Option<egui::Pos2>,
    /// Screen position of an active long press (touch only).
    pub long_press_pos: Option<egui::Pos2>,
    /// Whether map panning must be suppressed this frame (a zoom-drag or a
    /// long press owns the pointer).
    pub suppress_pan: bool,
}

impl MapPointerFrame {
    /// A pane that takes no part in pointer interaction this frame.
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

/// The touch gesture detectors that run for the active pane.
#[derive(Clone, Default)]
pub(crate) struct TouchGestures {
    pub double_tap: DoubleTapDragDetector,
    pub long_press: LongPressDetector,
}

impl TouchGestures {
    /// Run the touch gesture pipeline for the active pane and resolve this
    /// frame's pointer state.
    ///
    /// Order matters and mirrors the historical Android path: the zoom drag is
    /// processed first (it may change `map_memory`), the long press is only
    /// polled when no zoom drag is active, and the overlay tap is the deferred
    /// single tap (confirmed only after the double-tap timeout, so
    /// double-tap-to-zoom never opens an overlay popup).
    pub(crate) fn update(
        &mut self,
        ctx: &egui::Context,
        map_memory: &mut walkers::MapMemory,
        pane_rect: egui::Rect,
    ) -> MapPointerFrame {
        self.double_tap.update(ctx, map_memory, pane_rect);
        let is_zoom_dragging = self.double_tap.is_zooming();

        let long_press_pos = if is_zoom_dragging {
            None
        } else {
            self.long_press.update(ctx)
        };

        let overlay_click_pos =
            filter_dialog_blocked(ctx, self.double_tap.take_confirmed_tap());

        MapPointerFrame {
            overlay_click_pos,
            long_press_pos,
            suppress_pan: is_zoom_dragging || long_press_pos.is_some(),
        }
    }
}
