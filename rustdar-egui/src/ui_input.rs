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
// pipeline. Everything stays compiled everywhere so the follow-on responsive UI
// has a single implementation to adopt, so don't warn about the half the current
// target happens not to use. The lint stays ON under `cfg(test)`, where the
// harness exercises both halves, so genuinely dead code is still reported.
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
///
/// This is deliberately keyed on *inactivity*, not on how long the gesture has
/// run: a drag that is still emitting motion is still real, however long it
/// lasts, while a gesture whose input simply stopped arriving (the integration
/// went away mid-sequence without ever sending a release or a cancel) is not.
/// A finger resting perfectly still emits nothing, so this must stay far longer
/// than any deliberate pause.
const POINTER_IDLE_TIMEOUT_S: f64 = 10.0;

/// One frame's pointer facts, with `down` corrected for sequences that egui
/// never ends. Produced by [`PointerTracker::read`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct PointerFrame {
    /// The primary button went down this frame.
    pub pressed: bool,
    /// The primary button was released this frame.
    pub released: bool,
    /// Whether a real finger/button is still down — **not** egui's raw
    /// `pointer.primary_down()`, see [`PointerTracker`].
    pub down: bool,
    /// Pointer position; falls back to the last position egui reported rather
    /// than to the origin, since `PointerGone` clears egui's latest position.
    pub pos: egui::Pos2,
    /// Wall-clock time of this frame, in seconds.
    pub time: f64,
}

/// Decides whether egui's latched `pointer.primary_down()` can still be
/// believed, and is the single place any "the pointer went away" policy lives.
///
/// egui only ever mutates its `down[]` flags on an [`egui::Event::PointerButton`]
/// (`egui-0.34.1/src/input_state/mod.rs`). Two things follow, and together they
/// are the whole "stuck gesture" bug class:
///
/// * `egui-winit` maps `winit::event::TouchPhase::Cancelled` to **only**
///   [`egui::Event::PointerGone`] — no release event. egui deliberately does not
///   treat `PointerGone` as a release ("when dragging a slider and the mouse
///   leaves the viewport, we still want the drag to work"), so after an
///   OS-cancelled touch `primary_down()` reports `true` *forever*.
/// * `PointerGone` also clears egui's latest pointer position, so
///   `interact_pos()` returns `None` while `down` still says `true` — a detector
///   that unwraps it to `Pos2::ZERO` reports gestures at the screen corner.
///
/// Clearing a detector's state once on the cancel frame is not enough: on the
/// very next frame `down` is still `true`, so any detector that arms itself on
/// "button is down" immediately re-arms and the gesture comes back (for the long
/// press, [`LONG_PRESS_DURATION_S`] later, pinned at the corner). So the fix is
/// a latch: once a sequence ends without a release, the pointer is considered
/// *lost*, and only a fresh press can clear that.
#[derive(Clone, Default)]
pub(crate) struct PointerTracker {
    /// Set when a sequence ended without a release; cleared by the next press.
    lost: bool,
    /// Last position egui actually reported (survives `PointerGone`).
    last_pos: egui::Pos2,
    /// Wall-clock time of the last frame that carried any pointer activity.
    last_activity: Option<f64>,
}

impl PointerTracker {
    /// Read this frame's pointer state. Call exactly once per frame, before any
    /// detector runs, and unconditionally — a frame skipped here is a
    /// cancellation missed.
    pub(crate) fn read(&mut self, ctx: &egui::Context) -> PointerFrame {
        ctx.input(|i| {
            let mut activity = false;

            // Walk the events in order so a cancel followed by a fresh press
            // within one frame ends up armed, not lost.
            for event in &i.events {
                match event {
                    egui::Event::PointerButton { pressed, .. } => {
                        activity = true;
                        if *pressed {
                            self.lost = false;
                        }
                    }
                    // The pointer vanished without a release: a cancelled touch,
                    // or the cursor leaving the window. Also emitted right after
                    // a normal touch-up, in the same frame as the release, where
                    // it changes nothing.
                    //
                    // Raw `Event::Touch { phase: Cancel }` is deliberately NOT
                    // treated as a cancellation: it carries a `TouchId`, egui
                    // exposes no way to tell which id backs the emulated
                    // pointer, and acting on a *secondary* finger's cancel would
                    // kill a primary finger's live gesture. Every integration
                    // that cancels the primary touch (egui-winit, and eframe's
                    // web backend for `touchcancel`) pairs it with `PointerGone`.
                    egui::Event::PointerGone => {
                        activity = true;
                        self.lost = true;
                    }
                    egui::Event::PointerMoved(_)
                    | egui::Event::MouseMoved(_)
                    | egui::Event::Touch { .. } => activity = true,
                    _ => {}
                }
            }

            if let Some(pos) = i.pointer.interact_pos() {
                self.last_pos = pos;
            }

            if activity || self.last_activity.is_none() {
                self.last_activity = Some(i.time);
            }

            let raw_down = i.pointer.primary_down();

            // Backstop: if we believe a button is down but no pointer input at
            // all has arrived for a long time, the belief is stale. Latching
            // `lost` (rather than just ending one gesture) is what keeps the
            // long-press detector from picking the phantom finger straight back
            // up.
            if raw_down
                && i.time - self.last_activity.unwrap_or(i.time) >= POINTER_IDLE_TIMEOUT_S
            {
                self.lost = true;
            }

            PointerFrame {
                pressed: i.pointer.primary_pressed(),
                released: i.pointer.primary_released(),
                down: raw_down && !self.lost,
                pos: self.last_pos,
                time: i.time,
            }
        })
    }
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
    /// `input` must come from [`PointerTracker::read`] — its `down` is the
    /// corrected one, which is what lets the zoom drag end when the OS takes the
    /// touch away.
    ///
    /// `map_rect` is the current pane's screen rect — taps outside it are
    /// discarded so that sidebar buttons and other non-map UI don't become
    /// deferred overlay clicks.
    pub(crate) fn update(
        &mut self,
        ctx: &egui::Context,
        input: PointerFrame,
        map_memory: &mut walkers::MapMemory,
        map_rect: egui::Rect,
    ) {
        let PointerFrame { pressed, released, down, pos, time } = input;

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
        if let GestureState::ZoomDragging { drag_start_y, initial_zoom } = self.state {
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
    ///
    /// `input` must come from [`PointerTracker::read`]: an intentional hold has
    /// no natural end, so this detector has no timeout of its own and relies
    /// entirely on the tracker to say when the finger is really gone. Given
    /// egui's raw `pointer.down`, a cancelled touch would re-arm the hold every
    /// [`LONG_PRESS_DURATION_S`] forever.
    pub(crate) fn update(&mut self, input: PointerFrame) -> Option<egui::Pos2> {
        let PointerFrame { down, pos, time, .. } = input;

        if !down {
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
    // Only the Android pane loop has inactive panes; the desktop path resolves
    // the mouse for every pane.
    #[allow(dead_code)]
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
}

impl TouchGestures {
    /// Run the touch gesture pipeline for the active pane and resolve this
    /// frame's pointer state.
    ///
    /// Order matters and mirrors the historical Android path: the pointer is
    /// read once (so a cancellation can never be missed, whichever gesture is
    /// running), the zoom drag is processed first (it may change `map_memory`),
    /// the long press is only polled when no zoom drag is active, and the
    /// overlay tap is the deferred single tap (confirmed only after the
    /// double-tap timeout, so double-tap-to-zoom never opens an overlay popup).
    pub(crate) fn update(
        &mut self,
        ctx: &egui::Context,
        map_memory: &mut walkers::MapMemory,
        pane_rect: egui::Rect,
    ) -> MapPointerFrame {
        let input = self.tracker.read(ctx);

        self.double_tap.update(ctx, input, map_memory, pane_rect);
        let is_zoom_dragging = self.double_tap.is_zooming();

        let long_press_pos = if is_zoom_dragging {
            None
        } else {
            self.long_press.update(input)
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
