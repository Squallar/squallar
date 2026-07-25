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
    /// Whether a *new* gesture may arm on this frame.
    ///
    /// `down` can be true on evidence that something is moving without evidence
    /// that a button is still held: after a `PointerGone` the integration
    /// discards a release that happens out of sight
    /// (`egui-winit-0.34.1/src/lib.rs:796`), so a pointer that comes back
    /// hovering is indistinguishable from one that comes back still dragging,
    /// and egui reports `down` for both. That is enough to carry an existing
    /// gesture across the gap, and deliberately not enough to start one — a
    /// hold that armed there would suppress panning behind a tooltip the user
    /// never asked for, and only a click would clear it.
    pub can_arm: bool,
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
/// *lost*.
///
/// # Why we lost it decides what can bring it back
///
/// The three ways a sequence can stop being trustworthy are *not*
/// interchangeable, and collapsing them into one boolean is how this module
/// previously grew a hole in each direction. See [`LostCause`]: a cancelled
/// touch is terminal, a `PointerGone` leaves the button state unknowable, and
/// an idle expiry never actually said the pointer went anywhere.
///
/// `PointerGone` in particular is not only a cancelled touch. `egui-winit`
/// emits it for `WindowEvent::CursorLeft` too
/// (`egui-winit-0.34.1/src/lib.rs:340`), dropping the pointer position at the
/// same time — which makes it discard a mouse release that happens outside the
/// window (`lib.rs:796`), so egui's `down` stays latched there as well. A latch
/// only a press could clear therefore stranded that case: hold the button,
/// leave, come back still dragging, and every following frame is a
/// `PointerMoved` with no press in it. But un-latching on motion
/// *unconditionally* is worse, because after a touch cancellation egui's
/// `down` is stale-`true` forever and motion does keep coming — from the next
/// finger (`lib.rs:894` admits it once `lib.rs:922` has cleared
/// `pointer_touch_id`), from a mouse on a hybrid device, or from `mousemove` on
/// the web. That resurrects the phantom-at-a-stale-position this whole module
/// exists to prevent.
///
/// So motion un-latches, but never past a cancellation, and never all the way:
/// see [`PointerFrame::can_arm`].
///
/// # Identifying a cancellation
///
/// This does not need per-backend knowledge — the evidence is in the stream:
///
/// * `egui-winit` pushes `Touch{phase: Cancel}` (`lib.rs:874`) in the same
///   frame as the cancel's `PointerGone` (`lib.rs:924`); a `CursorLeft`
///   `PointerGone` never has one. So a `PointerGone` sharing a frame with a
///   cancel is a cancellation, and one on its own is an excursion.
/// * eframe 0.34.1's web canvas emits **nothing else at all** for
///   `touchcancel` — `install_touchcancel` is one `push_touches(Cancel)` with
///   no release and no `PointerGone` (`eframe/src/web/events.rs:788`). Keying
///   only on `PointerGone` therefore never fired on the web *at all*: the map
///   stayed un-pannable with a stuck tooltip until the idle backstop, minutes
///   later. So a raw `Touch{Cancel}` also acts on its own — but only for the
///   finger we positively identified as backing the emulated pointer, so a
///   *secondary* finger's cancel still cannot kill a live gesture.
///
/// The primary touch id is adopted from the `Touch{Start}` sharing a frame with
/// the press that opened the sequence. Both integrations emit that pair; only
/// the order differs (winit `Touch{Start}` first, web the press first), so the
/// correlation is computed over the whole frame rather than in event order.
#[derive(Clone, Default)]
pub(crate) struct PointerTracker {
    /// Why egui's latched `down` is not currently believed, if it is not.
    lost: Option<LostCause>,
    /// Set when `down` was restored by motion after a [`LostCause::Gone`]: we
    /// know something is moving, but not that a button is still held. Cleared
    /// only by a real press. Drives [`PointerFrame::can_arm`].
    unconfirmed: bool,
    /// The touch id backing egui's emulated pointer, when this sequence started
    /// from a touch. `None` for mouse sequences, and whenever we did not see
    /// the `Touch{Start}` that opened the sequence.
    primary_touch: Option<egui::TouchId>,
    /// Last position egui actually reported (survives `PointerGone`).
    ///
    /// egui clears `interact_pos()` on the frame *after* a `PointerGone`
    /// (`egui-0.34.1/src/input_state/mod.rs:1111`), so a frame can restore
    /// `down` while egui has no position to offer — but only via
    /// [`egui::Event::MouseMoved`], which carries a delta and no position.
    /// **Nothing in this workspace currently produces `MouseMoved`**:
    /// `egui-winit`'s `on_mouse_motion` (`lib.rs:759`) is only reached from
    /// `DeviceEvent`, which `rustdar-platform/src/egui_renderer.rs:59` does not
    /// forward, and eframe's web `mousemove` pushes `PointerMoved`. So this
    /// fallback is **defensive**, exercised by the harness rather than by any
    /// live integration — kept because `Pos2::ZERO` in its place would pin a
    /// gesture to the screen corner, which is the exact failure this module
    /// was written to stop.
    last_pos: egui::Pos2,
    /// Wall-clock time of the last frame that carried any pointer activity.
    last_activity: Option<f64>,
}

/// Why [`PointerTracker`] stopped believing egui's latched `down`.
///
/// The distinction is the whole point: it decides what is allowed to bring the
/// pointer back, and each variant answers a different question about what we
/// actually observed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LostCause {
    /// The touch backing the emulated pointer was cancelled by the OS or the
    /// browser. **Terminal** — only a fresh press clears it. Motion after a
    /// cancellation is some *other* input source (the next finger, a mouse on
    /// a hybrid device, `mousemove` on the web); treating it as proof the
    /// cancelled finger came back is what resurrects a phantom gesture at the
    /// position the OS took the touch away from.
    Cancelled,
    /// A bare `PointerGone`: the pointer left the window. A release that
    /// happened while it was away was discarded by the integration, so we
    /// genuinely cannot tell a still-held button from one that was let go out
    /// of sight — and egui reports `down` either way. Motion restores `down`,
    /// because something is demonstrably there, but leaves the sequence
    /// *unconfirmed*: no new gesture may arm off an inferred button.
    Gone,
    /// The idle backstop fired: nothing whatsoever arrived for
    /// [`POINTER_IDLE_TIMEOUT_S`]. Nothing ever said the pointer went away —
    /// this is only a timer running out under a finger that was resting — and
    /// a missed release always arrives with a `PointerGone` or a cancel, both
    /// of which take precedence. So motion restores trust completely.
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
            // Integrations disagree about ordering within a frame (egui-winit
            // pushes `Touch{Start}` before the press it belongs to, eframe's web
            // canvas after it), so anything that has to correlate two events of
            // one frame is decided here rather than in the ordered walk below.
            let primary_at_entry = self.primary_touch;
            let mut touch_started = None;
            let mut any_cancel = false;
            let mut primary_cancel = false;
            for event in &i.events {
                if let egui::Event::Touch { id, phase, .. } = event {
                    match phase {
                        egui::TouchPhase::Start => {
                            touch_started.get_or_insert(*id);
                        }
                        egui::TouchPhase::Cancel => {
                            any_cancel = true;
                            primary_cancel |= Some(*id) == primary_at_entry;
                        }
                        _ => {}
                    }
                }
            }
            // A `PointerGone` sharing a frame with a cancel is a cancellation —
            // unless we positively know the cancel belonged to a different
            // finger, in which case the `PointerGone` is something else. When we
            // never identified a primary finger we assume the worse of the two,
            // because over-classifying costs a re-press and under-classifying
            // costs a phantom gesture.
            let gone_is_cancel = any_cancel && (primary_cancel || primary_at_entry.is_none());

            // Walk the events in order so a cancel followed by a fresh press
            // within one frame ends up armed, not lost.
            for event in &i.events {
                match event {
                    egui::Event::PointerButton { pressed, .. } => {
                        activity = true;
                        if *pressed {
                            self.lost = None;
                            self.unconfirmed = false;
                            // Adopt the finger that opened this sequence. Only
                            // when we are not already following one: eframe's
                            // web canvas re-emits a primary press for *every*
                            // touchstart including a second finger's
                            // (`events.rs:676`), and that frame carries the
                            // second finger's `Touch{Start}`.
                            if self.primary_touch.is_none() {
                                self.primary_touch = touch_started;
                            }
                        } else {
                            // A real release ends the sequence outright.
                            self.primary_touch = None;
                        }
                    }
                    // The pointer vanished without a release: a cancelled touch,
                    // or the cursor leaving the window. Also emitted right after
                    // a normal touch-up, in the same frame as the release, where
                    // it changes nothing.
                    egui::Event::PointerGone => {
                        activity = true;
                        self.lost = Some(if gone_is_cancel {
                            self.primary_touch = None;
                            LostCause::Cancelled
                        } else {
                            LostCause::Gone
                        });
                    }
                    // A raw `Touch{Cancel}` acts on its own **only** for the
                    // finger we identified as backing the emulated pointer, so
                    // a secondary finger's cancel can never kill a live
                    // gesture. This is the whole cancellation path on the web,
                    // where `touchcancel` emits no release and no `PointerGone`
                    // (`eframe/src/web/events.rs:788`).
                    egui::Event::Touch { id, phase, .. } => {
                        activity = true;
                        if *phase == egui::TouchPhase::Cancel
                            && Some(*id) == primary_at_entry
                        {
                            self.lost = Some(LostCause::Cancelled);
                            self.primary_touch = None;
                        }
                    }
                    // Motion says something is there. What that is worth
                    // depends entirely on why we stopped believing the pointer
                    // — see [`LostCause`].
                    egui::Event::PointerMoved(_) | egui::Event::MouseMoved(_) => {
                        activity = true;
                        match self.lost {
                            // Terminal. Motion after a cancellation is some
                            // other input source, not the cancelled finger.
                            Some(LostCause::Cancelled) => {}
                            // Something is moving, but the button state is now
                            // inferred rather than observed.
                            Some(LostCause::Gone) => {
                                self.lost = None;
                                self.unconfirmed = true;
                            }
                            // Nothing ever said the pointer left; it was just
                            // still. Full trust.
                            Some(LostCause::Idle) => self.lost = None,
                            None => {}
                        }
                    }
                    _ => {}
                }
            }

            if let Some(pos) = i.pointer.interact_pos() {
                self.last_pos = pos;
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

            PointerFrame {
                pressed: i.pointer.primary_pressed(),
                released: i.pointer.primary_released(),
                down: raw_down && self.lost.is_none(),
                can_arm: !self.unconfirmed,
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
        let PointerFrame { down, can_arm, pos, time, .. } = input;

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
            // A hold must start from an *observed* press, never from an
            // inferred one: see [`PointerFrame::can_arm`]. A gesture already
            // under way is unaffected — this only refuses to open a new one.
            if !can_arm {
                return None;
            }
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

    /// After a `PointerGone` the pointer is distrusted, but motion says
    /// *something* is there. `down` comes back so a gesture in flight is not
    /// cut off — and `can_arm` does not, because a release that happened while
    /// the cursor was out of the window was discarded by the integration
    /// (`lib.rs:796`), leaving egui's `down` stale-`true` whether the user is
    /// still holding the button or hovering with nothing pressed.
    #[test]
    fn motion_after_an_excursion_restores_down_but_not_arming() {
        let mut d = TrackerDriver::new();
        let pos = egui::pos2(100.0, 100.0);

        let pressed = d.frame(vec![button(egui::PointerButton::Primary, true, pos)]);
        assert!(pressed.down && pressed.can_arm, "a real press confirms both");

        assert!(
            !d.frame(vec![egui::Event::PointerGone]).down,
            "precondition: the excursion is distrusted"
        );

        let returned = d.frame(vec![egui::Event::PointerMoved(pos + egui::vec2(4.0, 0.0))]);
        assert!(returned.down, "something is moving, so the pointer is there");
        assert!(
            !returned.can_arm,
            "but nothing observed says a button is still held"
        );

        // It stays inferred for as long as no press arrives...
        let still = d.frame(vec![egui::Event::PointerMoved(pos + egui::vec2(8.0, 0.0))]);
        assert!(still.down && !still.can_arm);

        // ...and a real press is what settles it.
        let repressed = d.frame(vec![button(egui::PointerButton::Primary, true, pos)]);
        assert!(repressed.down && repressed.can_arm);
    }

    /// Positionless motion (`MouseMoved` is a delta) on a frame where
    /// `PointerGone` has already cleared egui's position must report the last
    /// real position, not the origin. Defensive: nothing in this workspace
    /// emits `MouseMoved` (see [`PointerTracker::last_pos`]), so this is the
    /// only place the fallback is exercised.
    #[test]
    fn positionless_motion_reports_the_last_real_position() {
        let mut d = TrackerDriver::new();
        let pos = egui::pos2(240.0, 310.0);

        d.frame(vec![
            egui::Event::PointerMoved(pos),
            button(egui::PointerButton::Primary, true, pos),
        ]);
        d.frame(vec![egui::Event::PointerGone]);

        let moved = d.frame(vec![egui::Event::MouseMoved(egui::vec2(2.0, 1.0))]);
        assert!(moved.down, "raw motion with the button down is a live pointer");
        assert_eq!(
            moved.pos, pos,
            "with no position to be had, report the last real one — not the corner"
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
        assert!(after.can_arm);
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
