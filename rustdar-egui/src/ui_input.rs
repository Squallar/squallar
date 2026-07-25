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
///
/// The feature that sets the floor here is the long-press radar-value tooltip:
/// its *normal* operating state is a finger held deliberately still, emitting
/// nothing at all, while the user reads a value. So this constant has to clear
/// the longest hold a user might plausibly perform, not merely the longest
/// pause inside a drag — at ten seconds the tooltip died under a finger that
/// was still on the glass. A minute of literally zero pointer events with a
/// finger down cannot happen on real capacitive hardware (jitter alone keeps
/// `ACTION_MOVE` flowing), so anything that quiet really is a dead integration.
///
/// Expiry is recoverable: it latches `lost` (so the long-press detector cannot
/// pick the phantom finger straight back up), but any subsequent pointer motion
/// un-latches it — see [`PointerTracker`]. A hold that resumes moving therefore
/// comes back on its own, without needing a lift and a fresh press.
const POINTER_IDLE_TIMEOUT_S: f64 = 60.0;

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
    /// Pointer position. egui's `interact_pos()`, which is `Some` on every
    /// frame where `down` is true — see [`PointerTracker::read`].
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
/// *lost*.
///
/// # Why we lost it decides what can bring it back
///
/// The two ways a sequence can stop being trustworthy are *not*
/// interchangeable, and collapsing them into one boolean is how this module
/// grew a hole in each direction. See [`LostCause`]: the pointer *going away*
/// is terminal, while an idle expiry never said the pointer went anywhere at
/// all and is undone by the next sign of life.
///
/// The tempting middle position — "motion means the pointer came back, so
/// un-latch" — is wrong, and wrong in the direction that resurrects the bug
/// this module exists for. After a touch cancellation egui's `down` is
/// stale-`true` forever and motion keeps arriving: from the next finger
/// (`lib.rs:894` admits it once `lib.rs:922` has cleared `pointer_touch_id`),
/// from a mouse on a hybrid device, or from `mousemove` on the web. None of
/// that is the cancelled finger returning, and treating it as such puts a
/// phantom hold at the position the OS took the touch away from.
///
/// The excursion case (`CursorLeft`, `egui-winit-0.34.1/src/lib.rs:340`) is
/// genuinely undecidable rather than merely unproven: the integration drops a
/// release that happens out of the window (`lib.rs:796`), so a pointer that
/// hovers back in is indistinguishable from one that comes back still dragging,
/// and egui reports `down` for both. Terminal-until-a-press picks the benign
/// failure — the user re-presses to carry on — over the malignant one, a hold
/// nobody asked for suppressing panning until they click.
///
/// # Identifying a cancellation
///
/// This does not need per-backend knowledge, which matters because the two
/// backends behave differently:
///
/// * `egui-winit` pairs a cancel with a `PointerGone` (`lib.rs:924`), which is
///   enough on its own — both mean the pointer went away, and both are
///   terminal.
/// * eframe 0.34.1's web canvas emits **nothing else at all** for
///   `touchcancel` — `install_touchcancel` is one `push_touches(Cancel)` with
///   no release and no `PointerGone` (`eframe/src/web/events.rs:788`). Keying
///   only on `PointerGone` therefore never fired on the web *at all*: the map
///   stayed un-pannable behind a stuck tooltip until the idle backstop, a
///   minute later. So a raw `Touch{Cancel}` also acts on its own — but only for
///   the finger positively identified as backing the emulated pointer, so a
///   *secondary* finger's cancel cannot kill a live gesture.
///
/// # Identifying the finger
///
/// The primary touch id is adopted from the `Touch{Start}` sharing a frame with
/// the press that *opens* a sequence. Three things make that fiddlier than it
/// sounds, and each has bitten:
///
/// * The two integrations emit the pair in opposite orders (winit
///   `Touch{Start}` first, web the press first), so the correlation is computed
///   over the whole frame rather than in event order.
/// * eframe re-emits a primary press for **every** `touchstart`, including a
///   second finger's, at the *first* finger's position (`events.rs:676`;
///   `primary_touch_pos` keeps the stored primary for as long as it appears in
///   `touches()`). A frame like that carries the *new* finger's `Touch{Start}`,
///   so "the press frame's touch id is the primary" hands the identity to the
///   wrong finger. Hence adoption only on a press that opens a sequence.
/// * Whole gestures can arrive batched into one `RawInput`. eframe's touch
///   listeners only request a repaint (`events.rs:695`) rather than running a
///   frame synchronously, so every DOM event between two animation frames lands
///   together — and on a map app decoding tiles those frames are long. A
///   `touchstart` and its `touchcancel` in the same frame is an ordinary
///   browser gesture takeover, so "the finger this frame belongs to" has to
///   include one adopted *by* this frame.
#[derive(Clone, Default)]
pub(crate) struct PointerTracker {
    /// Why egui's latched `down` is not currently believed, if it is not.
    lost: Option<LostCause>,
    /// Whether a press has opened a sequence that no release has closed.
    ///
    /// Not the same as egui's `down`, which stays latched through exactly the
    /// failures this type exists for; this is only used to tell a press that
    /// *opens* a sequence from one emitted part-way through an existing one.
    sequence_live: bool,
    /// The touch id backing egui's emulated pointer, when this sequence started
    /// from a touch. `None` for mouse sequences, and whenever we did not see
    /// the `Touch{Start}` that opened the sequence.
    primary_touch: Option<egui::TouchId>,
    /// Wall-clock time of the last frame that carried any pointer activity.
    last_activity: Option<f64>,
}

/// Why [`PointerTracker`] stopped believing egui's latched `down`.
///
/// The distinction is the whole point: it decides what is allowed to bring the
/// pointer back, and each variant answers a different question about what was
/// actually observed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LostCause {
    /// The pointer *went away* without a release — a cancelled touch, or the
    /// cursor leaving the window. **Terminal: only a fresh press clears it.**
    ///
    /// Both halves report the same thing, that the input we were following is
    /// no longer there, and neither leaves any way to tell later motion apart
    /// from a different input source. A cancelled finger is gone for good; a
    /// departed cursor may or may not still have its button held, and the
    /// integration threw away the evidence.
    Gone,
    /// The idle backstop fired: nothing whatsoever arrived for
    /// [`POINTER_IDLE_TIMEOUT_S`]. **Motion undoes it.**
    ///
    /// Nothing ever said the pointer went away — this is a timer running out
    /// under a finger that was resting, which is the long-press tooltip's
    /// normal operating state. It is only reached when no [`LostCause::Gone`]
    /// is already latched, and no integration we have read drops a release
    /// without also reporting the departure: `egui-winit` clears
    /// `pointer_pos_in_points` at three sites and all three push `PointerGone`,
    /// and eframe's web `touchend` pushes both the release and `PointerGone`.
    /// (Not a universal guarantee: eframe installs no `pointercancel` handler,
    /// so a browser firing `pointercancel` instead of `pointerup` would drop a
    /// release silently. Reaching a bad state from there needs that *and* a
    /// full [`POINTER_IDLE_TIMEOUT_S`] of silence *and* the cursor never
    /// leaving the canvas.)
    Idle,
}

impl PointerTracker {
    /// Read this frame's pointer state. Call exactly once per frame, before any
    /// detector runs, and unconditionally — a frame skipped here is a
    /// cancellation missed.
    pub(crate) fn read(&mut self, ctx: &egui::Context) -> PointerFrame {
        ctx.input(|i| {
            let mut activity = false;
            // egui has already folded this frame's events into `pointer` by the
            // time we run, so this is the frame's *final* button state and is
            // the same value `down` is derived from below.
            let raw_down = i.pointer.primary_down();

            // --- order-independent frame facts ------------------------------
            // The integrations disagree about ordering within a frame, and a
            // whole gesture can arrive batched into one of them, so anything
            // that correlates two events of a frame is decided here rather than
            // in the ordered walk below. See `PointerTracker`.
            let mut touch_started = None;
            for event in &i.events {
                if let egui::Event::Touch { id, phase: egui::TouchPhase::Start, .. } = event {
                    // First wins: eframe picks the primary as
                    // `all_touches.first()` and pushes changed touches in the
                    // same order (`web/input.rs:30`, `:85`).
                    touch_started.get_or_insert(*id);
                }
            }
            // The finger this frame's events belong to: the one already being
            // followed, or — when the frame opens the sequence — the one
            // starting in it. Both halves are needed: a `touchstart` and its
            // `touchcancel` batched together would otherwise be compared
            // against an identity this frame is only just adopting.
            let frame_primary = self.primary_touch.or(touch_started);

            // Walk the events in order so a cancel followed by a fresh press
            // within one frame ends up armed, not lost.
            for event in &i.events {
                match event {
                    egui::Event::PointerButton { pressed, button, .. } => {
                        activity = true;
                        // Only the primary button drives the sequence. `down`
                        // is `primary_down()`, so a right- or middle-click says
                        // nothing about whether the input we are tracking is
                        // still there: letting one clear the latch would revive
                        // a loss that is meant to be terminal, and letting one
                        // close the sequence would drop the finger id that is
                        // the entire cancellation signal on the web.
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
                                // Closing the sequence is all a release has to
                                // do — the next press re-adopts from scratch,
                                // so clearing `primary_touch` here as well
                                // would be a second mechanism for the same
                                // thing, and an untestable one.
                                self.sequence_live = false;
                            }
                        }
                    }
                    // The pointer vanished without a release: a cancelled touch,
                    // or the cursor leaving the window. Also emitted right after
                    // a normal touch-up, in the same frame as the release, where
                    // it changes nothing.
                    egui::Event::PointerGone => {
                        activity = true;
                        self.lost = Some(LostCause::Gone);
                    }
                    // A raw `Touch{Cancel}` acts on its own **only** for the
                    // finger backing the emulated pointer, so a secondary
                    // finger's cancel can never kill a live gesture. This is the
                    // whole cancellation path on the web, where `touchcancel`
                    // emits no release and no `PointerGone`
                    // (`eframe/src/web/events.rs:788`).
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
            // (rather than just ending one gesture) is what keeps the long-press
            // detector from picking the phantom finger straight back up; the
            // motion rule above is what lets a still-live finger undo it without
            // a lift.
            //
            // Only ever *adds* a reason to distrust the pointer: downgrading an
            // existing one to `Idle` would make a cancellation recoverable by
            // motion after [`POINTER_IDLE_TIMEOUT_S`], which is precisely the
            // resurrection `Cancelled` is terminal to prevent.
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
            // The assertion is the guard: if a future policy lets something
            // clear the latch without a position, every gesture would silently
            // start being reported at the screen corner, which is the failure
            // this module was written to stop.
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
        let PointerFrame { pressed, released, down, pos, time, .. } = input;

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

/// One pane's resolved pointer state **as `render_map` actually used it**.
///
/// Recorded from the very locals that feed `PaneRenderCtx` and
/// `Map::drag_pan_buttons`, so a test observes the decision the shipped
/// renderer took rather than a second, parallel run of the same resolver. A
/// harness that drives its own [`InteractionState`] alongside `Gui::ui` and
/// then asserts on *that* validates a replica: every one of `ui_map.rs`'s
/// pointer decisions can be broken without such a test noticing.
#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct PanePointerProbe {
    pub pane_idx: usize,
    pub is_active: bool,
    /// The modality `render_map` resolved for the frame, read from the same
    /// local that selects the pipeline.
    pub modality: crate::ui_layout::PointerModality,
    pub frame: MapPointerFrame,
}

impl MapPointerFrame {
    /// A pane that takes no part in pointer interaction this frame.
    ///
    /// Reached whenever a touch gesture is in play and the pane is not the one
    /// that owns it — the touch pipeline is single-pointer and stateful, so it
    /// runs for the active pane only. Nothing about that is Android-specific.
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

/// Owns the touch gesture detectors and decides, per frame, whether they run
/// at all.
///
/// # Why the gate exists
///
/// Both detectors were written for a finger and misfire on a mouse. This was
/// verified, not assumed:
///
/// * [`LongPressDetector`] keys purely on "the primary button is down for
///   [`LONG_PRESS_DURATION_S`]". A user who holds the left button still for a
///   moment before dragging — an ordinary slow click, and the *start of every
///   map pan* — trips it, which raises `suppress_pan` and takes the drag away
///   from the map. Running it under a mouse breaks mouse panning outright.
/// * [`DoubleTapDragDetector`] defers every single tap by
///   [`DOUBLE_TAP_TIMEOUT_S`] so that a double-tap can claim it. On a mouse
///   that is 400ms of latency added to every overlay click, and a double-click
///   — a completely ordinary thing to do with a mouse — silently enters a
///   zoom-drag instead.
///
/// So the modality is not a cosmetic choice about which code path is tidier:
/// running the touch pipeline under a mouse is a functional regression in two
/// separate places, and running the mouse path under a finger loses
/// double-tap-zoom and the long-press tooltip.
#[derive(Clone, Default)]
pub(crate) struct InteractionState {
    gestures: TouchGestures,
    /// The modality the last frame ran under, so a change can be noticed.
    last_modality: Option<crate::ui_layout::PointerModality>,
}

impl InteractionState {
    /// Resolve the **active** pane's pointer state for this frame.
    ///
    /// `map_memory` is the active pane's viewport; the zoom-drag gesture writes
    /// to it directly.
    pub(crate) fn resolve_active(
        &mut self,
        ctx: &egui::Context,
        modality: crate::ui_layout::PointerModality,
        map_memory: &mut walkers::MapMemory,
        pane_rect: egui::Rect,
    ) -> MapPointerFrame {
        use crate::ui_layout::PointerModality;

        // A modality change abandons any gesture in flight. Without this a
        // half-formed gesture — a first tap waiting for its partner, or a
        // `LostCause` latch — survives the switch and resolves against input it
        // was never watching. The user put the finger down and picked up a
        // mouse; nothing about that first tap is still true.
        if self.last_modality != Some(modality) {
            self.gestures = TouchGestures::default();
            self.last_modality = Some(modality);
        }

        match modality {
            PointerModality::Touch => self.gestures.update(ctx, map_memory, pane_rect),
            PointerModality::Mouse => MapPointerFrame::from_mouse(ctx),
        }
    }

    /// Resolve a pane that is **not** the active one.
    ///
    /// The touch pipeline is single-pointer and stateful, so it only ever runs
    /// for the pane that owns the gesture. The mouse has no such state, so it
    /// is resolved for every pane exactly as the desktop build always did —
    /// which is what lets a click land on an overlay in a pane before that
    /// pane becomes active.
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
mod tests {
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

        assert!(d.frame(vec![button(egui::PointerButton::Primary, true, pos)]).down);
        assert!(!d.frame(vec![egui::Event::PointerGone]).down);

        for step in 1..=4 {
            let moved = d.frame(vec![egui::Event::PointerMoved(
                pos + egui::vec2(4.0 * step as f32, 0.0),
            )]);
            assert!(!moved.down, "step {step}: motion is not evidence of a held button");
        }

        assert!(
            d.frame(vec![button(egui::PointerButton::Primary, true, pos)]).down,
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
            !d.frame(vec![egui::Event::PointerMoved(pos + egui::vec2(40.0, 0.0))]).down,
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
            d.frame(vec![button(egui::PointerButton::Secondary, false, pos)]).down,
            "a right-button release is not this sequence ending"
        );

        // eframe's re-emitted press for a second finger.
        let second = pos + egui::vec2(80.0, 0.0);
        d.frame(vec![
            button(egui::PointerButton::Primary, true, pos),
            touch(1, egui::TouchPhase::Start, second),
        ]);

        assert!(
            d.frame(vec![touch(1, egui::TouchPhase::Cancel, second)]).down,
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
            d.frame(vec![touch(6, egui::TouchPhase::Cancel, pos + egui::vec2(60.0, 0.0))])
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

        let after = d.frame(vec![touch(1, egui::TouchPhase::Cancel, pos + egui::vec2(80.0, 0.0))]);
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
        assert!(d.frame(vec![button(egui::PointerButton::Primary, true, pos)]).down);

        assert!(
            d.frame(vec![touch(0, egui::TouchPhase::Cancel, pos + egui::vec2(200.0, 0.0))])
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
            d.frame(vec![touch(1, egui::TouchPhase::Cancel, pos + egui::vec2(80.0, 0.0))])
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
}
