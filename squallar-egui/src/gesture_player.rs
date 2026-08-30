//! The scripted-input player: four deterministic gesture scripts, each a pure
//! function of **elapsed time** (never frame count), injected into the
//! renderer's raw input ahead of the wheel/touch normalizers so a synthetic
//! event takes exactly the rewrite path a real one would on every target.
//!
//! Every script loops every [`LOOP_SECONDS`] and is built so consecutive loops
//! see the same picture: the 2D pan strokes come in mirrored pairs whose
//! displacements cancel, the wheel/dolly/pinch legs zoom in exactly as much as
//! they zoom back out, and the UI sweep re-toggles everything it toggled. Each
//! loop also contains a scripted number of event-free *quiet phases* — long
//! enough past `overlay_cache::SETTLE_REPAINT_DELAY` (100 ms) that a settle
//! can fire — published per script as `QUIET_PHASES`/`ZOOM_QUIET_PHASES` so a
//! later gate can derive expected settle counts from the script alone.
//!
//! Armed by the `gesture_script` config key or the `SQUALLAR_GESTURE_SCRIPT`
//! environment variable; absent both, nothing here runs and the raw input is
//! byte-identical to an unarmed build.

use std::collections::BTreeMap;

use crate::input_fidelity;

/// Seconds per loop, every script.
pub const LOOP_SECONDS: f64 = 20.0;

/// The floor under every scripted quiet phase, seconds. Chosen comfortably
/// above the 100 ms settle delay so a quiet phase always outlives it.
pub const QUIET_MIN_SECONDS: f64 = 1.5;

/// The 2D map scenario: eight mirrored pairs of drag strokes, then a wheel
/// zoom in and back out, with quiet phases between.
pub mod pan_zoom_2d {
    /// Event-free windows per loop, each at least
    /// [`QUIET_MIN_SECONDS`](super::QUIET_MIN_SECONDS) long.
    pub const QUIET_PHASES: u32 = 3;
    /// The quiet windows that directly follow zoom motion — the ones a
    /// settle re-raster follows.
    pub const ZOOM_QUIET_PHASES: u32 = 2;
    /// Sixteen strokes: mirrored pairs, so the loop's net pan is zero.
    pub(crate) const STROKES: u32 = 16;
    pub(crate) const STROKE_PERIOD: f64 = 0.5;
    /// Seconds of each period spent pressed; the rest coasts on inertia.
    pub(crate) const STROKE_HOLD: f64 = 0.45;
    pub(crate) const DRAG_END: f64 = 8.0;
    pub(crate) const QUIET_1_END: f64 = 10.0;
    pub(crate) const ZOOM_IN_END: f64 = 13.5;
    pub(crate) const QUIET_2_END: f64 = 15.0;
    pub(crate) const ZOOM_OUT_END: f64 = 18.5;
    /// Wheel notches per zoom leg; in and out are equal, so the loop's net
    /// zoom is zero.
    pub(crate) const NOTCHES_PER_LEG: i64 = 10;
    /// Points per second at pair 0; each later pair adds
    /// [`STROKE_SPEED_STEP`].
    pub(crate) const STROKE_SPEED_BASE: f32 = 200.0;
    pub(crate) const STROKE_SPEED_STEP: f32 = 225.0;
}

/// The 3D volume scenario: a closed Lissajous orbit drag, then a wheel dolly
/// in and back out — ten notches each way, the magnitude the spike showed
/// crosses the mirror-rung flip.
pub mod orbit_3d {
    pub const QUIET_PHASES: u32 = 3;
    pub const ZOOM_QUIET_PHASES: u32 = 2;
    pub(crate) const DRAG_END: f64 = 7.0;
    pub(crate) const QUIET_1_END: f64 = 8.75;
    pub(crate) const DOLLY_IN_END: f64 = 12.75;
    pub(crate) const QUIET_2_END: f64 = 14.5;
    pub(crate) const DOLLY_OUT_END: f64 = 18.5;
    pub(crate) const NOTCHES_PER_LEG: i64 = 10;
    /// Periods chosen so both axes complete whole cycles by
    /// [`DRAG_END`]: the path closes and the loop's net orbit is zero.
    pub(crate) const X_PERIOD: f64 = 3.5;
    pub(crate) const Y_PERIOD: f64 = 7.0;
}

/// The two-finger scenario, spoken in the web backend's per-finger touch
/// vocabulary: a slow pinch out and back, then a fast one.
pub mod pinch_2d {
    pub const QUIET_PHASES: u32 = 3;
    pub const ZOOM_QUIET_PHASES: u32 = 3;
    pub(crate) const GAP_MIN: f32 = 80.0;
    pub(crate) const GAP_MAX: f32 = 400.0;
    pub(crate) const OUT_END: f64 = 4.0;
    pub(crate) const QUIET_1_END: f64 = 6.0;
    pub(crate) const IN_END: f64 = 10.0;
    pub(crate) const QUIET_2_END: f64 = 12.0;
    pub(crate) const FAST_OUT_END: f64 = 14.0;
    pub(crate) const FAST_IN_END: f64 = 16.0;
}

/// The UI-responsiveness scenario: every registered layer eye toggled off in
/// one pass and back on in a second, the layers panel closed and reopened,
/// the inspector opened, a slider dragged out and back, and the inspector
/// closed by its own button — all through the click registry, so the events
/// land on the rects the widgets really drew.
pub mod ui_sweep {
    pub const QUIET_PHASES: u32 = 3;
    pub const ZOOM_QUIET_PHASES: u32 = 0;
    /// Eye slots per pass. A stack longer than this is driven only this far.
    pub(crate) const EYE_SLOTS: usize = 12;
    pub(crate) const EYE_STEP: f64 = 0.3;
    /// Seconds from a press to its release.
    pub(crate) const PRESS_TO_RELEASE: f64 = 0.15;
    /// First off-pass press. Not 0.0: the registry needs one drawn frame
    /// before there is anything to aim at.
    pub(crate) const OFF_PASS_START: f64 = 0.05;
    pub(crate) const ON_PASS_START: f64 = 5.4;
    pub(crate) const LAYERS_CLOSE_PRESS: f64 = 10.8;
    pub(crate) const LAYERS_OPEN_PRESS: f64 = 11.1;
    pub(crate) const INSPECTOR_OPEN_PRESS: f64 = 11.4;
    pub(crate) const SLIDER_PRESS: f64 = 11.7;
    /// The drag reaches +[`SLIDER_TRAVEL`] here, then returns.
    pub(crate) const SLIDER_OUT_END: f64 = 12.3;
    pub(crate) const SLIDER_RELEASE: f64 = 12.9;
    pub(crate) const CLOSE_BUTTON_PRESS: f64 = 13.2;
    pub(crate) const SLIDER_TRAVEL: f32 = 60.0;

    /// One eye per stack row registers under this prefix plus the layer id.
    pub const EYE_PREFIX: &str = "stack_eye:";
    pub const LAYERS_TOGGLE: &str = "topbar_layers";
    pub const INSPECTOR_TOGGLE: &str = "topbar_inspector";
    pub const INSPECTOR_CLOSE: &str = "inspector_close";
    /// Sliders register under this prefix; the sweep drags the first, in id
    /// order.
    pub const SLIDER_PREFIX: &str = "control_slider:";
}

/// Where UiSweep's targets are this frame: widgets the sweep drives register
/// their rect as they draw, the player takes the whole map at the top of the
/// next frame, and anything not re-registered by then has expired with the
/// frame that drew it.
///
/// Thread-local rather than process-global on purpose: the app registers and
/// reads on the one thread that draws, and a process-global would let every
/// concurrently running test's draw bleed into another's snapshot.
pub mod click_registry {
    use std::cell::RefCell;
    use std::collections::BTreeMap;

    thread_local! {
        static REGISTRY: RefCell<Registry> = RefCell::new(Registry::default());
    }

    #[derive(Default)]
    struct Registry {
        collecting: bool,
        current: BTreeMap<String, egui::Rect>,
    }

    /// Whether a UiSweep player on this thread is collecting targets. The
    /// draw-site guard: a site whose id needs building checks this first, so
    /// a dormant install never allocates for the registry.
    pub fn collecting() -> bool {
        REGISTRY.with(|r| r.borrow().collecting)
    }

    /// Say where a sweep target is this frame. A no-op unless a UiSweep
    /// player is collecting; call it every frame the widget draws.
    pub fn register(id: &str, rect: egui::Rect) {
        REGISTRY.with(|r| {
            let mut r = r.borrow_mut();
            if r.collecting {
                r.current.insert(id.to_owned(), rect);
            }
        });
    }

    pub(crate) fn set_collecting(on: bool) {
        REGISTRY.with(|r| {
            let mut r = r.borrow_mut();
            r.collecting = on;
            if !on {
                r.current.clear();
            }
        });
    }

    /// The frame's snapshot, taken by the player before the next draw begins.
    /// Taking it empties the registry — that is the expiry.
    pub(crate) fn take_frame() -> BTreeMap<String, egui::Rect> {
        REGISTRY.with(|r| std::mem::take(&mut r.borrow_mut().current))
    }
}

/// The four scripts. Names are the config vocabulary: `pan-zoom-2d`,
/// `orbit-3d`, `pinch-2d`, `ui-sweep`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GestureScript {
    PanZoom2D,
    Orbit3D,
    Pinch2D,
    UiSweep,
}

impl GestureScript {
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "pan-zoom-2d" => Some(Self::PanZoom2D),
            "orbit-3d" => Some(Self::Orbit3D),
            "pinch-2d" => Some(Self::Pinch2D),
            "ui-sweep" => Some(Self::UiSweep),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::PanZoom2D => "pan-zoom-2d",
            Self::Orbit3D => "orbit-3d",
            Self::Pinch2D => "pinch-2d",
            Self::UiSweep => "ui-sweep",
        }
    }
}

/// The marker the rig brackets a run with. A value so a test can pin the
/// sentence the rig's regex must keep matching.
fn begin_line(script: GestureScript) -> String {
    format!("gesture script {} begin", script.name())
}

/// The per-loop marker. `frames` is how many frames the finished loop served
/// — a reported count only, never an input to the schedule.
fn loop_complete_line(script: GestureScript, frames: u32) -> String {
    format!(
        "gesture script {} loop complete: {} frames",
        script.name(),
        frames
    )
}

/// A press waiting for its scheduled release.
struct PendingRelease {
    due: f64,
    pos: egui::Pos2,
    id: String,
}

/// The UiSweep slider drag in flight.
struct SliderDrag {
    start: egui::Pos2,
    id: String,
}

pub struct GesturePlayer {
    script: GestureScript,
    armed_at: web_time::Instant,
    /// Loop-local time of the previous call; `None` before the first.
    prev_t: Option<f64>,
    loops_completed: u64,
    frames_this_loop: u32,
    /// Whether the synthetic primary button is down, and where it last was.
    down_at: Option<egui::Pos2>,
    /// Signed wheel notches already emitted this loop.
    notches_emitted: i64,
    /// Last two-finger positions emitted, for the pinch release.
    pinch_last: Option<(egui::Pos2, egui::Pos2)>,
    /// The gap the active pinch window ends on; the release lands here.
    pinch_end_gap: f32,
    pending_release: Option<PendingRelease>,
    slider: Option<SliderDrag>,
    /// Press/release pairs delivered per target id, cumulative — what the
    /// click-registry gate counts.
    delivered: BTreeMap<String, u32>,
}

impl GesturePlayer {
    /// Arm a player, or `None` for a name no script answers to. Logs the
    /// begin marker the rig brackets from.
    pub fn from_name(name: &str) -> Option<Self> {
        let Some(script) = GestureScript::from_name(name) else {
            log::warn!("gesture script {name:?} is not one this build knows; player disarmed");
            return None;
        };
        log::info!("{}", begin_line(script));
        Some(Self {
            script,
            armed_at: web_time::Instant::now(),
            prev_t: None,
            loops_completed: 0,
            frames_this_loop: 0,
            down_at: None,
            notches_emitted: 0,
            pinch_last: None,
            pinch_end_gap: pinch_2d::GAP_MIN,
            pending_release: None,
            slider: None,
            delivered: BTreeMap::new(),
        })
    }

    /// Seconds since arming, for the shipped caller. Tests never use this —
    /// they hand [`Self::events_for_frame`] explicit times.
    pub fn elapsed_secs(&self) -> f64 {
        self.armed_at.elapsed().as_secs_f64()
    }

    /// This frame's scripted events — a deterministic function of
    /// `now_secs` (seconds since arming) and `screen` (the window in egui
    /// points). For UiSweep it also consumes the click registry's snapshot of
    /// what last frame drew.
    pub fn events_for_frame(&mut self, now_secs: f64, screen: egui::Rect) -> Vec<egui::Event> {
        if self.script == GestureScript::UiSweep {
            click_registry::set_collecting(true);
            let targets = click_registry::take_frame();
            self.events_with_targets(now_secs, screen, &targets)
        } else {
            self.events_with_targets(now_secs, screen, &BTreeMap::new())
        }
    }

    /// Press/release pairs delivered per registry id, cumulative.
    pub fn pairs_delivered(&self) -> &BTreeMap<String, u32> {
        &self.delivered
    }

    pub fn loops_completed(&self) -> u64 {
        self.loops_completed
    }

    /// [`Self::events_for_frame`] with the UiSweep targets passed explicitly
    /// — the pure core, and what the unit tests drive.
    pub(crate) fn events_with_targets(
        &mut self,
        now_secs: f64,
        screen: egui::Rect,
        targets: &BTreeMap<String, egui::Rect>,
    ) -> Vec<egui::Event> {
        let mut events = Vec::new();
        let now_secs = now_secs.max(0.0);
        let loop_idx = (now_secs / LOOP_SECONDS) as u64;
        let t = now_secs - loop_idx as f64 * LOOP_SECONDS;

        while self.loops_completed < loop_idx {
            // Close out the finishing loop: run its tail window, put every
            // held control back, and reset the per-loop cursors.
            let tail_from = if self.loops_completed == loop_idx - 1 {
                self.prev_t.unwrap_or(0.0)
            } else {
                // A whole loop passed with no frame at all; nothing was
                // emitted in it, so there is nothing to finish.
                0.0
            };
            self.step(&mut events, screen, targets, tail_from, LOOP_SECONDS);
            self.release_all(&mut events);
            self.notches_emitted = 0;
            self.loops_completed += 1;
            log::info!("{}", loop_complete_line(self.script, self.frames_this_loop));
            self.frames_this_loop = 0;
            self.prev_t = Some(0.0);
        }

        let from = self.prev_t.unwrap_or(-f64::EPSILON);
        self.step(&mut events, screen, targets, from, t);
        self.prev_t = Some(t);
        self.frames_this_loop += 1;
        events
    }

    /// Emit everything the script schedules in the window `(from, to]` of the
    /// current loop.
    fn step(
        &mut self,
        events: &mut Vec<egui::Event>,
        screen: egui::Rect,
        targets: &BTreeMap<String, egui::Rect>,
        from: f64,
        to: f64,
    ) {
        if to <= from {
            return;
        }
        match self.script {
            GestureScript::PanZoom2D => self.pan_zoom_2d(events, screen, to),
            GestureScript::Orbit3D => self.orbit_3d(events, screen, to),
            GestureScript::Pinch2D => self.pinch_2d(events, screen, to),
            GestureScript::UiSweep => self.ui_sweep(events, targets, from, to),
        }
    }

    /// Let go of everything a loop can hold, at the loop boundary.
    fn release_all(&mut self, events: &mut Vec<egui::Event>) {
        if let Some(pending) = self.pending_release.take() {
            input_fidelity::mouse_release(events, pending.pos);
            *self.delivered.entry(pending.id).or_default() += 1;
        }
        if let Some(drag) = self.slider.take() {
            input_fidelity::mouse_release(events, drag.start);
            *self.delivered.entry(drag.id).or_default() += 1;
        }
        if let Some(pos) = self.down_at.take() {
            input_fidelity::mouse_release(events, pos);
        }
        if let Some((a, b)) = self.pinch_last.take() {
            input_fidelity::web_second_finger_up(events, b);
            input_fidelity::web_first_finger_up(events, a);
        }
    }

    fn press_at(&mut self, events: &mut Vec<egui::Event>, pos: egui::Pos2) {
        input_fidelity::mouse_press(events, pos);
        self.down_at = Some(pos);
    }

    fn move_to(&mut self, events: &mut Vec<egui::Event>, pos: egui::Pos2) {
        input_fidelity::mouse_move(events, pos);
        if self.down_at.is_some() {
            self.down_at = Some(pos);
        }
    }

    fn release_if_down(&mut self, events: &mut Vec<egui::Event>, pos: egui::Pos2) {
        if self.down_at.take().is_some() {
            input_fidelity::mouse_release(events, pos);
        }
    }

    /// Emit one Line-unit wheel notch per unit the signed schedule crossed
    /// since the last frame, over the screen centre. Line units on purpose:
    /// the injection lands before `normalize_wheel_units`, so the web build's
    /// rewrite sees exactly what a browser notch would be.
    fn emit_due_notches(&mut self, events: &mut Vec<egui::Event>, screen: egui::Rect, due: i64) {
        let delta = due - self.notches_emitted;
        let step = if delta >= 0 { 1.0 } else { -1.0 };
        for _ in 0..delta.abs().min(30) {
            input_fidelity::wheel(
                events,
                screen.center(),
                egui::MouseWheelUnit::Line,
                egui::vec2(0.0, step),
            );
        }
        self.notches_emitted = due;
    }

    /// A leg's cumulative notch count at `t`: `per_leg` notches spread evenly
    /// over `(start, end]`.
    fn leg_notches(t: f64, start: f64, end: f64, per_leg: i64) -> i64 {
        let u = ((t - start) / (end - start)).clamp(0.0, 1.0);
        (u * per_leg as f64).floor() as i64
    }

    // ── pan-zoom-2d ──

    fn pan_zoom_2d(&mut self, events: &mut Vec<egui::Event>, screen: egui::Rect, to: f64) {
        use pan_zoom_2d::*;
        let center = screen.center();
        let t = to;
        if t < DRAG_END {
            let stroke = ((t / STROKE_PERIOD) as u32).min(STROKES - 1);
            let t_in = t - f64::from(stroke) * STROKE_PERIOD;
            let pair = stroke / 2;
            let speed = STROKE_SPEED_BASE + STROKE_SPEED_STEP * pair as f32;
            let mut angle = 0.7 * pair as f32;
            if stroke % 2 == 1 {
                // The mirror stroke: same speed, opposite direction, so the
                // pair's displacements cancel and the loop re-centres.
                angle += std::f32::consts::PI;
            }
            let reach_cap = 0.4 * screen.width().min(screen.height());
            let reach = |dt: f64| (speed * dt as f32).min(reach_cap);
            if t_in < STROKE_HOLD {
                if self.down_at.is_none() {
                    self.press_at(events, center);
                }
                let pos = center + egui::vec2(angle.cos(), angle.sin()) * reach(t_in);
                self.move_to(events, pos);
            } else {
                // Release exactly at the hold-time reach, whatever the frame
                // cadence did: the release carries its own move, so the net
                // stroke displacement is cadence-independent.
                let pos = center + egui::vec2(angle.cos(), angle.sin()) * reach(STROKE_HOLD);
                self.release_if_down(events, pos);
            }
        } else {
            self.release_if_down(events, center);
            // Evaluated past the legs' ends too, not only inside them: the
            // frame cadence lands the u=1 crossing just after a leg's end
            // time, and a branch closed at exactly that time would strand the
            // leg's last notch — a net zoom drift per loop.
            let due = Self::leg_notches(t, QUIET_1_END, ZOOM_IN_END, NOTCHES_PER_LEG)
                - Self::leg_notches(t, QUIET_2_END, ZOOM_OUT_END, NOTCHES_PER_LEG);
            self.emit_due_notches(events, screen, due);
        }
        // ZOOM_OUT_END..LOOP_SECONDS: quiet #3 (the flush above emits at most
        // one notch just past ZOOM_OUT_END, then the schedule is flat).
    }

    // ── orbit-3d ──

    fn orbit_3d(&mut self, events: &mut Vec<egui::Event>, screen: egui::Rect, t: f64) {
        use orbit_3d::*;
        let center = screen.center();
        if t < DRAG_END - 0.05 {
            if self.down_at.is_none() {
                self.press_at(events, center);
            }
            let a = 0.22 * screen.width();
            let b = 0.18 * screen.height();
            let pos = center
                + egui::vec2(
                    a * (std::f64::consts::TAU * t / X_PERIOD).sin() as f32,
                    b * (std::f64::consts::TAU * t / Y_PERIOD).sin() as f32,
                );
            self.move_to(events, pos);
        } else {
            // Both Lissajous axes complete whole cycles by DRAG_END, so the
            // path closes here: release at the centre it started from.
            self.release_if_down(events, center);
            // Past the legs' ends too — see the pan-zoom flush note.
            let due = Self::leg_notches(t, QUIET_1_END, DOLLY_IN_END, NOTCHES_PER_LEG)
                - Self::leg_notches(t, QUIET_2_END, DOLLY_OUT_END, NOTCHES_PER_LEG);
            self.emit_due_notches(events, screen, due);
        }
        // DOLLY_OUT_END..LOOP_SECONDS: quiet #3, after the one-notch flush.
    }

    // ── pinch-2d ──

    /// The two-finger gap at `t` and the gaps the active window starts and
    /// ends on, or `None` between pinch windows.
    fn pinch_gap(t: f64) -> Option<(f32, f32, f32)> {
        use pinch_2d::*;
        let leg = |from: f32, to: f32, u: f64| {
            Some((from + (to - from) * u.clamp(0.0, 1.0) as f32, from, to))
        };
        if t < OUT_END {
            leg(GAP_MIN, GAP_MAX, t / OUT_END)
        } else if (QUIET_1_END..IN_END).contains(&t) {
            leg(GAP_MAX, GAP_MIN, (t - QUIET_1_END) / (IN_END - QUIET_1_END))
        } else if (QUIET_2_END..FAST_OUT_END).contains(&t) {
            leg(
                GAP_MIN,
                GAP_MAX,
                (t - QUIET_2_END) / (FAST_OUT_END - QUIET_2_END),
            )
        } else if (FAST_OUT_END..FAST_IN_END).contains(&t) {
            leg(
                GAP_MAX,
                GAP_MIN,
                (t - FAST_OUT_END) / (FAST_IN_END - FAST_OUT_END),
            )
        } else {
            None
        }
    }

    fn pinch_2d(&mut self, events: &mut Vec<egui::Event>, screen: egui::Rect, t: f64) {
        let center = screen.center();
        let at = |gap: f32| {
            (
                center - egui::vec2(gap / 2.0, 0.0),
                center + egui::vec2(gap / 2.0, 0.0),
            )
        };
        match Self::pinch_gap(t) {
            Some((gap, start_gap, end_gap)) => {
                let (a, b) = at(gap);
                if self.pinch_last.is_none() {
                    // The fingers land exactly on the window's start gap and
                    // move to the sampled one in the same frame: a session's
                    // zoom is its last gap over its first, and only exact
                    // boundaries at both ends keep the loop's product at 1
                    // whatever the frame cadence sampled.
                    let (a0, b0) = at(start_gap);
                    input_fidelity::web_first_finger_down(events, a0);
                    input_fidelity::web_second_finger_down(events, b0);
                    input_fidelity::web_pinch_move(events, a, b);
                } else {
                    input_fidelity::web_pinch_move(events, a, b);
                }
                self.pinch_last = Some((a, b));
                self.pinch_end_gap = end_gap;
            }
            None => {
                if self.pinch_last.take().is_some() {
                    // The matching exact landing on the way out.
                    let (a, b) = at(self.pinch_end_gap);
                    input_fidelity::web_pinch_move(events, a, b);
                    input_fidelity::web_second_finger_up(events, b);
                    input_fidelity::web_first_finger_up(events, a);
                }
            }
        }
    }

    // ── ui-sweep ──

    /// The sweep's press agenda for the window `(from, to]`, resolved against
    /// this frame's registry snapshot. Presses whose target is not registered
    /// are skipped — a closed panel simply is not driven.
    fn ui_sweep(
        &mut self,
        events: &mut Vec<egui::Event>,
        targets: &BTreeMap<String, egui::Rect>,
        from: f64,
        to: f64,
    ) {
        use ui_sweep::*;

        // Any due release first, so a pair finishes before the next begins.
        if let Some(pending) = self.pending_release.as_ref()
            && pending.due <= to
        {
            let pending = self.pending_release.take().expect("just checked");
            input_fidelity::mouse_release(events, pending.pos);
            *self.delivered.entry(pending.id).or_default() += 1;
        }

        let crossed = |at: f64| from < at && at <= to;
        let eye_ids: Vec<String> = targets
            .keys()
            .filter(|id| id.starts_with(EYE_PREFIX))
            .take(EYE_SLOTS)
            .cloned()
            .collect();
        fn press(
            this: &mut GesturePlayer,
            events: &mut Vec<egui::Event>,
            targets: &BTreeMap<String, egui::Rect>,
            id: &str,
            at: f64,
        ) {
            let Some(rect) = targets.get(id) else {
                return;
            };
            // A stalled frame can cross two presses at once; finish the
            // earlier pair before opening the next so no release is lost.
            if let Some(pending) = this.pending_release.take() {
                input_fidelity::mouse_release(events, pending.pos);
                *this.delivered.entry(pending.id).or_default() += 1;
            }
            let pos = rect.center();
            input_fidelity::mouse_press(events, pos);
            this.pending_release = Some(PendingRelease {
                due: at + PRESS_TO_RELEASE,
                pos,
                id: id.to_owned(),
            });
        }

        for (slot, id) in eye_ids.iter().enumerate() {
            for pass_start in [OFF_PASS_START, ON_PASS_START] {
                let at = pass_start + slot as f64 * EYE_STEP;
                if crossed(at) {
                    press(self, events, targets, id, at);
                }
            }
        }
        for (at, id) in [
            (LAYERS_CLOSE_PRESS, LAYERS_TOGGLE),
            (LAYERS_OPEN_PRESS, LAYERS_TOGGLE),
            (INSPECTOR_OPEN_PRESS, INSPECTOR_TOGGLE),
            (CLOSE_BUTTON_PRESS, INSPECTOR_CLOSE),
        ] {
            if crossed(at) {
                press(self, events, targets, id, at);
            }
        }

        // The slider drag: press, ramp out, ramp back, release where it
        // started.
        if crossed(SLIDER_PRESS)
            && let Some((id, rect)) = targets
                .iter()
                .find(|(id, _)| id.starts_with(SLIDER_PREFIX))
                .map(|(id, rect)| (id.clone(), *rect))
        {
            let pos = rect.center();
            input_fidelity::mouse_press(events, pos);
            self.slider = Some(SliderDrag { start: pos, id });
        }
        if let Some(drag) = self.slider.as_ref() {
            if to < SLIDER_RELEASE {
                let out = (SLIDER_OUT_END - SLIDER_PRESS).max(f64::EPSILON);
                let u = if to <= SLIDER_OUT_END {
                    (to - SLIDER_PRESS) / out
                } else {
                    1.0 - (to - SLIDER_OUT_END) / (SLIDER_RELEASE - SLIDER_OUT_END)
                };
                let pos = drag.start + egui::vec2(SLIDER_TRAVEL * u.clamp(0.0, 1.0) as f32, 0.0);
                input_fidelity::mouse_move(events, pos);
            } else {
                let drag = self.slider.take().expect("just checked");
                input_fidelity::mouse_release(events, drag.start);
                *self.delivered.entry(drag.id).or_default() += 1;
            }
        }

        // A due release scheduled by a press in this very window (large dt).
        if let Some(pending) = self.pending_release.as_ref()
            && pending.due <= to
        {
            let pending = self.pending_release.take().expect("just checked");
            input_fidelity::mouse_release(events, pending.pos);
            *self.delivered.entry(pending.id).or_default() += 1;
        }
    }
}

impl Drop for GesturePlayer {
    fn drop(&mut self) {
        if self.script == GestureScript::UiSweep {
            click_registry::set_collecting(false);
        }
    }
}

#[cfg(test)]
mod tests;
