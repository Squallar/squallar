use crate::{Position, position::AdjustedPosition};
use egui::{DragPanButtons, PointerButton, Response, Vec2};

/// Time constant of the inertia stopping filter, in **seconds**.
///
/// The coast's velocity falls by `1/e` every `INERTIA_TAU`, so everything it
/// has left to travel at any instant is exactly `velocity * INERTIA_TAU`.
const INERTIA_TAU: f32 = 0.2f32;

/// How much unspent coast counts as standing still, in points.
///
/// The coast stops once `velocity * INERTIA_TAU` — its whole remaining travel —
/// falls under this. That is not a free saving: it is a **position error**. The
/// map stops up to this many points short of where the exponential would have
/// put it, and the error is invisible only because nothing draws the
/// un-truncated trajectory beside it. One point is what
/// [`Center::PulledToMyPosition`] already calls close enough, so the two
/// animations agree on the word.
///
/// Stating the threshold in remaining *distance* rather than in per-frame
/// travel is what keeps the truncation the same size at every frame rate; the
/// per-frame spelling it replaced truncated 1.2 points at 60 Hz and 4.9 at
/// 240 Hz.
const INERTIA_STOP_POINTS: f32 = 1.0;

/// Time constant of the pull back to `my_position`, in seconds.
///
/// Chosen so a 60 Hz frame reproduces exactly the per-frame halving this
/// replaced: `exp(-(1/60) / PULL_TAU) == 0.5`, hence `PULL_TAU = 1 / (60 ln 2)`.
const PULL_TAU: f32 = 1.0 / (60.0 * std::f32::consts::LN_2);

/// The shortest frame an animation will integrate over, in seconds.
///
/// Load-bearing rather than decorative: egui hands the first frame a
/// `stable_dt` of `0.0`, and a zero step decays nothing and moves nothing, so
/// the animation would never terminate. 1 kHz is well past any display.
const MIN_ANIMATION_DT: f32 = 1.0 / 1000.0;

/// The longest frame an animation will integrate over, in seconds.
///
/// A 250 ms hitch is real time that really passed, but spending it in one step
/// lands the entire remaining coast in a single frame — a teleport. Clamping
/// spreads it over the frames that follow instead, and it costs no distance:
/// [`Center::update_movement`]'s shift telescopes to
/// `INERTIA_TAU * (v_start - v_end)` for *any* sequence of steps, so a clamped
/// frame stretches the coast in wall-clock without shortening it in points.
const MAX_ANIMATION_DT: f32 = 1.0 / 10.0;

/// One frame's length, bounded into the range an animation can integrate over.
///
/// `f32::clamp` propagates a `NaN` rather than bounding it, so a frame whose
/// timing is unusable is given the shortest step instead of one that would turn
/// the whole state `NaN` and never terminate.
fn animation_dt(delta_time: f32) -> f32 {
    if delta_time.is_nan() {
        MIN_ANIMATION_DT
    } else {
        delta_time.clamp(MIN_ANIMATION_DT, MAX_ANIMATION_DT)
    }
}

/// Position of the map's center. Initially, the map follows `my_position` argument which typically
/// is meant to be fed by a GPS sensor or other geo-localization method. If user drags the map,
/// it becomes "detached" and stays this way until [`MapMemory::center_mode`] is changed back to
/// [`Center::MyPosition`].
#[derive(Debug, Clone, PartialEq, Default)]
#[cfg_attr(feature = "serde", derive(::serde::Serialize, ::serde::Deserialize))]
pub(crate) enum Center {
    /// Centered at `my_position` argument of the [`Map::new()`] function.
    #[default]
    MyPosition,

    /// Centered at the exact position.
    Exact(AdjustedPosition),

    /// Map is being dragged by mouse or finger.
    Moving {
        position: AdjustedPosition,
        direction: Vec2,
        /// Whether the drag was started from a detached state.
        from_detached: bool,
    },

    /// Map is moving, but due to inertia, and will slow down and stop in a short while.
    Inertia {
        position: AdjustedPosition,
        /// Points **per second**, not per frame. Per-frame is what made the
        /// same flick travel a different distance on a 120 Hz display than on
        /// a 60 Hz one: the shift was applied once per frame while the decay
        /// was computed from `delta_time`, so the two disagreed about what a
        /// second was.
        velocity: Vec2,
    },

    /// Map is being pulled back to the `my_position`. This happens when the user releases the
    /// dragging gesture, but the map is too close to the `my_position`.
    PulledToMyPosition(AdjustedPosition),
}

/// What one frame's [`Response`] means for a map that is or is not already
/// being dragged.
///
/// [`Center::Moving`] is the only state that keeps re-shifting the map by a
/// stored delta, and the only thing that leaves it is a gesture. Naming the
/// gestures separately from applying them is what lets the state machine be
/// driven without an `egui::Response`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Gesture {
    /// A pan drag is in progress; this frame's `drag_delta`.
    Dragging(Vec2),

    /// A drag ended and the widget saw the release edge, carrying the velocity
    /// the coast starts from in points per second.
    Released(Vec2),

    /// The map is [`Center::Moving`], but this frame's response reports
    /// neither a drag nor a release. The release edge is only ever offered on
    /// the frame it happens, to the widget it happens on, so it is lost
    /// whenever that widget is not shown or stops sensing the button: a hidden
    /// pane, a tab switch, a pan suppressed mid-drag, or a pointer released
    /// outside the canvas on the web.
    Vanished,

    /// Nothing to do: the map is not `Moving` and no drag is being reported.
    Idle,
}

impl Gesture {
    /// What a frame that reports neither a drag nor a release means. The
    /// answer depends on the state, which is why [`Gesture::Vanished`] and
    /// [`Gesture::Idle`] are not the same gesture.
    pub(crate) fn quiet(center: &Center) -> Self {
        if matches!(center, Center::Moving { .. }) {
            Gesture::Vanished
        } else {
            Gesture::Idle
        }
    }
}

/// Whether a gesture that ends a drag hands the map its momentum, and at what
/// velocity in points per second if so.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Coast {
    Yes(Vec2),
    No,
}

/// The parts of one frame's [`egui::InputState`] a [`Center`] needs and a
/// [`Response`] does not carry.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct InputFrame {
    /// `egui`'s own smoothed pointer velocity, points per second. Already the
    /// right quantity and already smoothed over several frames, which is why
    /// it is preferred to anything reconstructed here.
    pub(crate) pointer_velocity: Vec2,

    /// How long this frame took, in seconds, unclamped.
    pub(crate) delta_time: f32,
}

impl Center {
    pub(crate) fn handle_gestures(
        &mut self,
        response: &Response,
        my_position: Position,
        pull_to_my_position_threshold: f32,
        drag_pan_buttons: DragPanButtons,
        input: InputFrame,
    ) -> bool {
        let gesture = self.classify(response, drag_pan_buttons, input);
        self.apply(gesture, my_position, pull_to_my_position_threshold)
    }

    /// Read one frame's response as a [`Gesture`].
    ///
    /// `drag_stopped` is asked before the state is, and it is button-agnostic
    /// while `dragged_by` is not: a drag on a button that does not pan still
    /// ends through the [`Gesture::Released`] arm, exactly as it did when this
    /// was an `if`/`else if` chain over the response alone.
    pub(crate) fn classify(
        &self,
        response: &Response,
        buttons: DragPanButtons,
        input: InputFrame,
    ) -> Gesture {
        if dragged_by(response, buttons) {
            Gesture::Dragging(response.drag_delta())
        } else if response.drag_stopped() {
            Gesture::Released(self.release_velocity(input))
        } else {
            Gesture::quiet(self)
        }
    }

    /// The velocity a release this frame hands to the coast, points per second.
    ///
    /// `egui` documents `pointer.velocity()` as possibly zero when the frame
    /// rate is bad, so a zero reading falls back to the drag's own last
    /// per-frame delta over the time a frame took — the same quantity, measured
    /// worse, and exactly what the per-frame coast this replaced used as its
    /// only source.
    fn release_velocity(&self, input: InputFrame) -> Vec2 {
        if input.pointer_velocity != Vec2::ZERO {
            return input.pointer_velocity;
        }
        match self {
            Center::Moving { direction, .. } => *direction / animation_dt(input.delta_time),
            _ => Vec2::ZERO,
        }
    }

    /// Drive the state machine one frame. Returns whether the state changed.
    pub(crate) fn apply(
        &mut self,
        gesture: Gesture,
        my_position: Position,
        pull_to_my_position_threshold: f32,
    ) -> bool {
        match gesture {
            Gesture::Dragging(delta) => {
                self.dragged_by(my_position, delta);
                true
            }
            Gesture::Released(velocity) => {
                self.drag_stopped(pull_to_my_position_threshold, Coast::Yes(velocity));
                true
            }
            // `direction` is the last `drag_delta` anyone observed, and in this
            // case nobody knows how old it is — the pane may have been hidden
            // for a minute. Coasting on it would lurch.
            Gesture::Vanished => {
                self.drag_stopped(pull_to_my_position_threshold, Coast::No);
                true
            }
            Gesture::Idle => false,
        }
    }

    fn dragged_by(&mut self, my_position: Position, drag_delta: Vec2) {
        let from_detached = if let Center::Moving { from_detached, .. } = self {
            *from_detached
        } else {
            // Only `MyPosition` state has no adjusted position.
            self.adjusted_position().is_some()
        };

        *self = Center::Moving {
            position: self
                .adjusted_position()
                .unwrap_or(AdjustedPosition::new(my_position)),
            direction: drag_delta,
            from_detached,
        };
    }

    fn drag_stopped(&mut self, pull_to_my_position_threshold: f32, coast: Coast) {
        if let Center::Moving {
            position,
            from_detached,
            ..
        } = &self
        {
            if *from_detached || position.offset_length() > pull_to_my_position_threshold {
                *self = match coast {
                    Coast::Yes(velocity) => Center::Inertia {
                        position: position.clone(),
                        velocity,
                    },
                    Coast::No => Center::Exact(position.clone()),
                };
            } else {
                *self = Center::PulledToMyPosition(position.to_owned());
            }
        }
    }

    /// Whether a pan drag is in progress, awaiting its release.
    ///
    /// Deliberately not folded into [`Center::animating`], which excludes
    /// dragging by documented intent.
    pub(crate) fn dragging(&self) -> bool {
        matches!(self, Center::Moving { .. })
    }

    /// End any gesture or animation, leaving the map where it is.
    ///
    /// Idempotent, and [`Center::position`] is unchanged by it for every
    /// variant: `MyPosition` has no position of its own to keep, and the other
    /// four carry the [`AdjustedPosition`] straight over.
    pub(crate) fn settle(&mut self) {
        if let Some(position) = self.adjusted_position() {
            *self = Center::Exact(position);
        }
    }

    pub(crate) fn update_movement(&mut self, delta_time: f32, zoom: f64) -> bool {
        match &self {
            Center::Moving {
                position,
                direction,
                from_detached,
            } => {
                *self = Center::Moving {
                    position: position.clone().shift(*direction, zoom),
                    direction: *direction,
                    from_detached: *from_detached,
                };
                true
            }
            Center::Inertia { position, velocity } => {
                *self = if velocity.length() * INERTIA_TAU < INERTIA_STOP_POINTS {
                    Center::Exact(position.to_owned())
                } else {
                    let dt = animation_dt(delta_time);
                    let decay = (-dt / INERTIA_TAU).exp();

                    // Integrate the exponential over the frame rather than
                    // taking one Euler step of `velocity * dt`:
                    //
                    //     ∫₀^dt v·e^(-t/τ) dt = v·τ·(1 - e^(-dt/τ))
                    //
                    // Summed over frames this telescopes to τ·(v_start - v_end)
                    // for *any* sequence of steps, because each frame's shift is
                    // exactly τ·(vₙ - vₙ₊₁). That identity is the whole fix: the
                    // total distance a flick travels stops depending on how the
                    // frames happened to be chopped up. A plain Euler step would
                    // still overshoot by 8% at 60 Hz and 58% at 5 Hz.
                    Center::Inertia {
                        position: position
                            .clone()
                            .shift(*velocity * (INERTIA_TAU * (1.0 - decay)), zoom),
                        velocity: *velocity * decay,
                    }
                };
                true
            }
            Center::PulledToMyPosition(position) => {
                // Same treatment, same reason: halving the offset once per
                // frame made the pull's duration a count of frames rather than
                // a length of time. `PULL_TAU` is set so 60 Hz still halves it.
                let decay = (-animation_dt(delta_time) / PULL_TAU).exp();
                let position = position.clone().scale_offset(f64::from(decay));
                *self = if position.offset_length() < 1.0 {
                    Center::MyPosition
                } else {
                    Center::PulledToMyPosition(position)
                };
                true
            }
            _ => false,
        }
    }

    /// Returns exact position if map is detached (i.e. not following `my_position`),
    /// `None` otherwise.
    pub(crate) fn detached(&self) -> Option<Position> {
        self.adjusted_position().map(|p| p.position())
    }

    /// Whether the map is detached, i.e. not following `my_position`.
    ///
    /// [`Center::detached`] answers the same question, but producing the position it
    /// returns costs a clone of the [`AdjustedPosition`] and an `unproject(project(..))`
    /// round trip through [`AdjustedPosition::position`]. A caller that only wants the
    /// yes/no should not pay for a position it is about to drop.
    pub(crate) fn is_detached(&self) -> bool {
        !matches!(self, Center::MyPosition)
    }

    pub fn animating(&self) -> bool {
        matches!(self, Center::Inertia { .. } | Center::PulledToMyPosition(_))
    }

    fn adjusted_position(&self) -> Option<AdjustedPosition> {
        match self {
            Center::MyPosition => None,
            Center::Exact(position)
            | Center::PulledToMyPosition(position)
            | Center::Moving { position, .. }
            | Center::Inertia { position, .. } => Some(position.to_owned()),
        }
    }

    /// Get the real position at the map's center.
    pub fn position(&self, my_position: Position) -> Position {
        self.detached().unwrap_or(my_position)
    }

    /// Shift position by given number of pixels, if detached.
    pub(crate) fn shift(self, offset: Vec2, zoom: f64) -> Self {
        match self {
            Center::MyPosition => Center::MyPosition,
            Center::PulledToMyPosition(position) => {
                Center::PulledToMyPosition(position.shift(offset, zoom))
            }
            Center::Exact(position) => Center::Exact(position.shift(offset, zoom)),
            Center::Moving {
                position,
                direction,
                from_detached,
            } => Center::Moving {
                position: position.shift(offset, zoom),
                direction,
                from_detached,
            },
            Center::Inertia { position, velocity } => Center::Inertia {
                position: position.shift(offset, zoom),
                velocity,
            },
        }
    }
}

fn dragged_by(response: &Response, buttons: DragPanButtons) -> bool {
    buttons.iter().any(|button| match button {
        DragPanButtons::PRIMARY => response.dragged_by(PointerButton::Primary),
        DragPanButtons::SECONDARY => response.dragged_by(PointerButton::Secondary),
        DragPanButtons::MIDDLE => response.dragged_by(PointerButton::Middle),
        DragPanButtons::EXTRA_1 => response.dragged_by(PointerButton::Extra1),
        DragPanButtons::EXTRA_2 => response.dragged_by(PointerButton::Extra2),
        _ => false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lon_lat;

    fn adjusted() -> AdjustedPosition {
        AdjustedPosition::new(lon_lat(17.03664, 51.09916)).shift(Vec2::new(10., -20.), 10.)
    }

    /// Naming every variant in an exhaustive `match` is what keeps [`every_variant`]
    /// honest: adding a variant to [`Center`] stops this compiling, rather than quietly
    /// leaving the new one untested.
    fn variant_name(center: &Center) -> &'static str {
        match center {
            Center::MyPosition => "MyPosition",
            Center::Exact(_) => "Exact",
            Center::Moving { .. } => "Moving",
            Center::Inertia { .. } => "Inertia",
            Center::PulledToMyPosition(_) => "PulledToMyPosition",
        }
    }

    fn every_variant() -> [Center; 5] {
        [
            Center::MyPosition,
            Center::Exact(adjusted()),
            Center::Moving {
                position: adjusted(),
                direction: Vec2::new(1., 2.),
                from_detached: true,
            },
            Center::Inertia {
                position: adjusted(),
                velocity: Vec2::new(300., 0.),
            },
            Center::PulledToMyPosition(adjusted()),
        ]
    }

    /// `is_detached` exists so callers can skip the position `detached` builds. The two
    /// must give the same answer on every variant of the enum, or the substitution at
    /// its call site is a behaviour change rather than a saving.
    #[test]
    fn is_detached_agrees_with_detached_on_every_variant() {
        let variants = every_variant();

        let mut names: Vec<_> = variants.iter().map(variant_name).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(
            variants.len(),
            names.len(),
            "every Center variant must appear exactly once: {names:?}"
        );

        for center in &variants {
            assert_eq!(
                center.detached().is_some(),
                center.is_detached(),
                "{}",
                variant_name(center)
            );
        }

        // Agreement on a constant would be agreement about nothing.
        assert!(!Center::MyPosition.is_detached());
        assert_eq!(4, variants.iter().filter(|c| c.is_detached()).count());
    }

    /// The whole point of the cheaper spelling: it must not go through
    /// `AdjustedPosition::position`, which is where the round trip is.
    #[test]
    fn is_detached_does_not_resolve_the_position() {
        // An offset large enough that the round trip moves the position a long way, so a
        // `detached()`-shaped implementation could not agree with this by accident.
        let far = Center::Exact(
            AdjustedPosition::new(lon_lat(0., 0.)).shift(Vec2::new(1_000_000., 1_000_000.), 1.),
        );

        assert!(far.is_detached());
        assert_ne!(lon_lat(0., 0.), far.detached().expect("detached"));
    }

    // --- the drag state machine ------------------------------------------
    //
    // Driven through `Gesture::quiet`, which is the same function
    // `Center::classify` ends with, so what these tests exercise is the real
    // `Moving` gate rather than a copy of it kept in the test module.

    const DT: f32 = 1.0 / 60.0;
    const ZOOM: f64 = 16.0;
    const THRESHOLD: f32 = 16.0;
    /// Ten seconds at 60 Hz.
    const TEN_SECONDS: usize = 600;

    fn my_position() -> Position {
        lon_lat(17.03664, 51.09916)
    }

    /// One frame in which the widget reports neither a drag nor a release,
    /// exactly as `Map::show` runs it: gestures first, then movement. Answers
    /// whether anything at all asked for another frame.
    fn quiet_frame(center: &mut Center) -> bool {
        let gesture = Gesture::quiet(center);
        let changed = center.apply(gesture, my_position(), THRESHOLD);
        changed | center.update_movement(DT, ZOOM)
    }

    /// A drag whose release edge is never observed — the pane was hidden, the
    /// tab switched, the pointer came up off-canvas. Nothing after the drag
    /// reports anything, and the map must stop rather than pan forever.
    ///
    /// On the code before `Gesture` existed this failed on frame 599 of 600,
    /// with the centre 0.1287 degrees of longitude from where the drag left
    /// it and `update_movement` still returning `true`.
    #[test]
    fn a_drag_that_is_never_released_stops_moving() {
        let my = my_position();
        let mut center = Center::Exact(adjusted());

        assert!(center.apply(Gesture::Dragging(Vec2::new(10., 0.)), my, THRESHOLD));
        assert!(
            center.update_movement(DT, ZOOM),
            "the frame that saw the drag must still move the map"
        );
        let where_the_drag_left_it = center.position(my);
        assert_ne!(
            where_the_drag_left_it,
            Center::Exact(adjusted()).position(my),
            "the drag moved the map by nothing, so the rest of this proves nothing"
        );

        let mut stopped_after = None;
        for frame in 0..TEN_SECONDS {
            if !quiet_frame(&mut center) {
                stopped_after = Some(frame);
                break;
            }
        }
        let stopped_after = stopped_after.unwrap_or_else(|| {
            let drift = center.position(my).x() - where_the_drag_left_it.x();
            panic!(
                "still demanding a repaint after {TEN_SECONDS} input-free frames; \
                 the centre has drifted {drift} deg of longitude"
            )
        });
        assert!(
            stopped_after < 4,
            "took {stopped_after} frames to stop, not a few"
        );

        // And it stopped where the drag left it, without coasting on a delta
        // whose age nobody knows.
        assert_eq!(where_the_drag_left_it, center.position(my));
        for _ in 0..TEN_SECONDS {
            assert!(!quiet_frame(&mut center), "it started moving again");
        }
        assert_eq!(where_the_drag_left_it, center.position(my));
    }

    /// The positive control for [`a_drag_that_is_never_released_stops_moving`]:
    /// a drag that *is* released still coasts, so that test cannot be
    /// satisfied by deleting inertia. Measured before the change: 0.002763 deg
    /// over 59 frames.
    #[test]
    fn a_drag_that_is_released_still_coasts() {
        let my = my_position();
        let mut center = Center::Exact(adjusted());
        center.apply(Gesture::Dragging(Vec2::new(10., 0.)), my, THRESHOLD);
        center.update_movement(DT, ZOOM);
        let at_release = center.position(my);

        // 10 points in one 60 Hz frame is 600 points per second, which is what
        // egui's smoothed velocity would have reported for this drag.
        assert!(center.apply(Gesture::Released(Vec2::new(600., 0.)), my, THRESHOLD));
        assert!(
            matches!(center, Center::Inertia { .. }),
            "a released drag must coast"
        );

        let mut frames = 0;
        while center.update_movement(DT, ZOOM) {
            frames += 1;
            assert!(frames < TEN_SECONDS, "inertia never stopped");
        }
        let coasted = (center.position(my).x() - at_release.x()).abs();
        assert!(
            coasted > 0.0,
            "the map did not coast at all after the release"
        );
        assert!(
            matches!(center, Center::Exact(_)),
            "inertia must end in Exact, not {:?}",
            variant_name(&center)
        );
    }

    /// The choice pinned: the two endings of a drag are different. A vanished
    /// one keeps the position and drops the momentum, so changing that is a
    /// deliberate edit to this test rather than a silent one.
    #[test]
    fn a_vanished_drag_does_not_coast() {
        let my = my_position();
        let mut center = Center::Exact(adjusted());
        center.apply(Gesture::Dragging(Vec2::new(10., 0.)), my, THRESHOLD);
        center.update_movement(DT, ZOOM);
        let before = center.position(my);

        assert_eq!(Gesture::Vanished, Gesture::quiet(&center));
        assert!(center.apply(Gesture::Vanished, my, THRESHOLD));

        assert!(
            matches!(center, Center::Exact(_)),
            "a vanished drag must settle, not coast: {}",
            variant_name(&center)
        );
        assert_eq!(before, center.position(my), "settling moved the map");
        assert!(!center.update_movement(DT, ZOOM));
        assert_eq!(Gesture::Idle, Gesture::quiet(&center));
    }

    /// `settle` is the caller's escape hatch, so it has to work from every
    /// state and leave the map where it was.
    #[test]
    fn settle_clears_every_state() {
        let my = my_position();
        for mut center in every_variant() {
            let name = variant_name(&center);
            let before = center.position(my);

            center.settle();
            assert!(
                !center.update_movement(DT, ZOOM),
                "{name} still demands a repaint after settle"
            );
            assert!(!center.dragging(), "{name} still reads as dragging");
            assert!(!center.animating(), "{name} still reads as animating");
            assert_eq!(before, center.position(my), "{name} moved");

            // Idempotent.
            center.settle();
            assert!(!center.update_movement(DT, ZOOM), "{name}, settled twice");
            assert_eq!(before, center.position(my), "{name}, settled twice");
        }
    }

    /// `dragging` must answer for exactly one variant, or it is either the
    /// wrong question or a constant.
    #[test]
    fn dragging_is_true_for_moving_alone() {
        let dragging: Vec<_> = every_variant()
            .iter()
            .filter(|c| c.dragging())
            .map(variant_name)
            .collect();
        assert_eq!(vec!["Moving"], dragging);
    }

    // --- the coast is measured in seconds, not in frames -----------------

    /// The frame rates a coast is measured at. 240 Hz and 30 Hz are the ends of
    /// what a desktop or a phone actually runs at; 120 Hz is the one the
    /// reported defect was noticed on.
    const COAST_RATES: [f32; 4] = [30., 60., 120., 240.];

    /// Total travel of a coast released at `v` points per second on a display
    /// running at `1/dt` Hz, in **points**, with the number of frames it took.
    ///
    /// Reading the answer off [`AdjustedPosition::offset_length`] is exact
    /// rather than a projection: every shift here happens at the same `ZOOM`,
    /// and `shift` rescales an existing offset by `2^(zoom - self.zoom)`, which
    /// is 1 when the zoom does not move. So the offset accumulates the points
    /// that were shifted, with no round trip through a geographic position.
    fn coast(v: f32, dt: f32) -> (f32, usize) {
        let mut center = Center::Inertia {
            position: AdjustedPosition::new(my_position()),
            velocity: Vec2::new(v, 0.),
        };
        let mut frames = 0usize;
        while center.update_movement(dt, ZOOM) {
            frames += 1;
            assert!(
                frames < 100_000,
                "the coast never stopped at {} Hz",
                1. / dt
            );
        }
        let Center::Exact(end) = &center else {
            panic!("a coast must end in Exact")
        };
        (end.offset_length(), frames)
    }

    /// **(A) The reported defect, as a gate.** One flick travels one distance,
    /// whatever the display is running at.
    ///
    /// This is a property of the integration and not of the numbers: each frame
    /// shifts by exactly `τ·(vₙ - vₙ₊₁)`, so the sum telescopes to
    /// `τ·(v_start - v_end)` for any sequence of steps at all. The only thing
    /// left that can vary is which side of [`INERTIA_STOP_POINTS`] the last
    /// step lands on, which is why the tolerance is a fraction of a point
    /// rather than zero.
    ///
    /// **Negative control**, measured on the per-frame coast this replaced
    /// (`41bf44c2`, same velocities, same rates): the spread was **1.193x at
    /// 500 pt/s, 1.168x at 1000, 1.155x at 2000** — 16% to 19%, every one of
    /// them a failure of the 1% below. (Not the ~2x that a per-*frame* reading
    /// suggests: a real flick at a fixed velocity hands a 120 Hz display half
    /// the per-frame delta it hands a 60 Hz one, and that cancels most of it.)
    #[test]
    fn a_flick_coasts_the_same_distance_at_every_frame_rate() {
        for v in [500., 1000., 2000.] {
            let measured: Vec<(f32, f32)> = COAST_RATES
                .iter()
                .map(|&hz| (hz, coast(v, 1. / hz).0))
                .collect();
            let widest = measured.iter().map(|&(_, d)| d).fold(f32::MIN, f32::max);
            let tightest = measured.iter().map(|&(_, d)| d).fold(f32::MAX, f32::min);
            assert!(
                tightest > 0.,
                "a {v} pt/s flick coasted nowhere, so this proves nothing"
            );
            assert!(
                widest / tightest < 1.01,
                "a {v} pt/s flick coasted {widest} points at one frame rate and \
                 {tightest} at another - a {:.4}x spread. Whole sweep: {measured:?}",
                widest / tightest,
            );
        }
    }

    /// The distance itself, and not merely its agreement with itself: the coast
    /// travels `velocity * INERTIA_TAU`, less the unspent tail the stop
    /// threshold cuts off.
    ///
    /// Without this, [`a_flick_coasts_the_same_distance_at_every_frame_rate`]
    /// would be satisfied by a coast that went nowhere at all.
    #[test]
    fn the_coast_travels_the_whole_of_what_the_velocity_is_worth() {
        for v in [500.0f32, 1000., 2000.] {
            for hz in COAST_RATES {
                let (travelled, _) = coast(v, 1. / hz);
                let ideal = v * INERTIA_TAU;
                let shortfall = ideal - travelled;
                assert!(
                    (0.0..=INERTIA_STOP_POINTS).contains(&shortfall),
                    "a {v} pt/s flick at {hz} Hz travelled {travelled} points \
                     against an ideal {ideal}; the shortfall {shortfall} is \
                     outside the 0..={INERTIA_STOP_POINTS} points the stop \
                     threshold can account for",
                );
            }
        }
    }

    /// **(B) The feel at 60 Hz is preserved.**
    ///
    /// The per-frame coast this replaced summed, for a release at `v` points
    /// per second on a frame of `dt` seconds, to
    ///
    /// ```text
    ///   Σ (v·dt)·lpⁿ = v·dt/(1 - lp)   where lp = τ/(dt + τ)
    ///                = v·(dt + τ)
    /// ```
    ///
    /// — 0.2167·v at 60 Hz, against 0.2·v now, so the derivation predicts
    /// **7.7% shorter**; measured it is **7.6%** (215.38 → 199.03 points at
    /// 1000 pt/s), the difference being the point of unspent tail the stop
    /// threshold cuts. All of that shortening is the old explicit-Euler step
    /// overshooting the integral it was approximating. Well inside the 15% a
    /// feel change is allowed.
    #[test]
    fn sixty_hertz_still_feels_like_it_did() {
        const DT_60: f32 = 1. / 60.;
        for v in [500.0f32, 1000., 2000.] {
            let before = v * (DT_60 + INERTIA_TAU);
            let (after, _) = coast(v, DT_60);
            let ratio = after / before;
            assert!(
                (0.85..=1.15).contains(&ratio),
                "a {v} pt/s flick used to coast {before} points at 60 Hz and now \
                 coasts {after} - a {ratio}x change, outside the 0.85..=1.15 a \
                 feel change may make",
            );
            // And the direction and size of it are the derivation above, not
            // just anything inside the band.
            assert!(
                (ratio - 0.923).abs() < 0.01,
                "the 60 Hz coast is {ratio}x of what it was, not the 0.923 the \
                 derivation in this test predicts",
            );
        }
    }

    /// **(C) Frames per flick, measured** — with the stop criterion named,
    /// because the two are not separable. Here a coast stops when its whole
    /// remaining travel is under [`INERTIA_STOP_POINTS`]; the coast this
    /// replaced stopped when its *per-frame* travel fell under 0.1 points.
    ///
    /// Measured at 1000 pt/s, before (`41bf44c2`) → after:
    /// 30 Hz **39 → 33**, 60 Hz **65 → 65**, 120 Hz **110 → 129**,
    /// 240 Hz **182 → 256**.
    ///
    /// **There is no frames-saved percentage here.** The count fell at 30 Hz,
    /// did not move at 60, and *rose* at the high rates — which is exactly what
    /// it means for a duration to be a length of time rather than a count of
    /// frames: the coast now lasts 1.07–1.10 s at every rate, so a faster
    /// display spends proportionally more frames on it. Any single percentage
    /// quoted for this change is quoting one row of that table.
    #[test]
    fn a_coast_lasts_the_same_time_rather_than_the_same_frames() {
        let seconds: Vec<(f32, f32)> = COAST_RATES
            .iter()
            .map(|&hz| (hz, coast(1000., 1. / hz).1 as f32 / hz))
            .collect();
        let widest = seconds.iter().map(|&(_, s)| s).fold(f32::MIN, f32::max);
        let tightest = seconds.iter().map(|&(_, s)| s).fold(f32::MAX, f32::min);
        assert!(
            widest - tightest < 0.05,
            "the same flick lasted {widest} s at one frame rate and {tightest} s \
             at another. Whole sweep, seconds: {seconds:?}",
        );
        // The frame counts genuinely differ, or the test above is about nothing.
        let frames: Vec<usize> = COAST_RATES
            .iter()
            .map(|&hz| coast(1000., 1. / hz).1)
            .collect();
        assert!(
            frames.iter().max() > frames.iter().min(),
            "every frame rate spent the same number of frames: {frames:?}",
        );
    }

    /// A frame that reports no elapsed time at all is what egui hands the very
    /// first frame, and it used to leave `lp_factor` at exactly 1.0 — a coast
    /// that decayed by nothing and never terminated.
    #[test]
    fn a_frame_with_no_elapsed_time_still_ends_the_coast() {
        for delta_time in [0.0, -1.0, f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            let mut center = Center::Inertia {
                position: AdjustedPosition::new(my_position()),
                velocity: Vec2::new(1000., 0.),
            };
            let mut frames = 0usize;
            while center.update_movement(delta_time, ZOOM) {
                frames += 1;
                assert!(
                    frames < 100_000,
                    "a coast on delta_time {delta_time} never stopped",
                );
            }
            assert!(matches!(center, Center::Exact(_)));
        }
    }

    /// A single enormous frame does not teleport the map: the clamp caps how
    /// much of the coast one step may spend, and the total is unaffected
    /// because the shift telescopes.
    #[test]
    fn a_hitch_does_not_teleport_the_coast() {
        let (whole, _) = coast(1000., 1. / 60.);

        let mut center = Center::Inertia {
            position: AdjustedPosition::new(my_position()),
            velocity: Vec2::new(1000., 0.),
        };
        center.update_movement(0.25, ZOOM);
        let after_the_hitch = center
            .adjusted_position()
            .expect("a coast has a position")
            .offset_length();
        assert!(
            after_the_hitch < whole * 0.75,
            "one 250 ms frame spent {after_the_hitch} of the {whole} points the \
             whole coast is worth",
        );

        // And the rest of it still arrives.
        while center.update_movement(1. / 60., ZOOM) {}
        let Center::Exact(end) = &center else {
            panic!("a coast must end in Exact")
        };
        assert!(
            (end.offset_length() - whole).abs() < 1.0,
            "a hitched coast travelled {} points against an unhitched {whole}",
            end.offset_length(),
        );
    }

    /// The pull back to `my_position` got the same treatment, so it too has to
    /// take the same *time* rather than the same number of frames — and a
    /// 60 Hz frame must still halve the offset, exactly as the per-frame
    /// halving it replaced did.
    ///
    /// **Negative control**, computed on the per-frame halving this replaced: a
    /// 256-point offset needs 9 halvings to get under 1 point at *any* frame
    /// rate, so the pull took 0.300 s at 30 Hz and 0.0375 s at 240 Hz — an **8x
    /// spread**, far worse than the coast's 1.17x, because nothing about it
    /// referred to `delta_time` at all.
    ///
    /// The duration is compared in frames-worth rather than in seconds because
    /// it can only end on a frame boundary: at 30 Hz one frame *is* 0.033 s, so
    /// a tolerance in seconds tighter than that would be measuring the
    /// quantisation and not the property.
    #[test]
    fn the_pull_home_takes_the_same_time_at_every_frame_rate() {
        let far = AdjustedPosition::new(my_position()).shift(Vec2::new(256., 0.), ZOOM);

        // A 60 Hz frame halves it, which is what `PULL_TAU` is derived from.
        let mut one_frame = Center::PulledToMyPosition(far.clone());
        one_frame.update_movement(1. / 60., ZOOM);
        let halved = one_frame
            .adjusted_position()
            .expect("still pulling")
            .offset_length();
        assert!(
            (halved - 128.).abs() < 0.01,
            "one 60 Hz frame took the offset from 256 to {halved}, not 128",
        );

        let seconds: Vec<(f32, f32)> = COAST_RATES
            .iter()
            .map(|&hz| {
                let mut center = Center::PulledToMyPosition(far.clone());
                let mut frames = 0usize;
                while center.update_movement(1. / hz, ZOOM) {
                    frames += 1;
                    assert!(frames < 100_000, "the pull never finished at {hz} Hz");
                }
                assert_eq!(Center::MyPosition, center, "the pull must end at home");
                (hz, frames as f32 / hz)
            })
            .collect();
        // What the exponential says it should take: the offset decays from 256
        // points to the 1 point that counts as home.
        let ideal = PULL_TAU * 256.0f32.ln();
        assert!(
            (ideal - 8. / 60.).abs() < 1e-4,
            "the derivation moved: {ideal} s is not the 8 frames at 60 Hz that \
             9 per-frame halvings used to be",
        );
        for &(hz, s) in &seconds {
            assert!(s > 0., "the pull at {hz} Hz finished instantly");
            assert!(
                (s - ideal).abs() * hz < 1.5,
                "the pull took {s} s at {hz} Hz against an ideal {ideal} s - \
                 {:.2} frames out, and it can only be one. Whole sweep, \
                 seconds: {seconds:?}",
                (s - ideal).abs() * hz,
            );
        }
    }

    /// A release egui had no smoothed velocity for still coasts, off the drag's
    /// own last per-frame delta — which is the only source the coast this
    /// replaced ever had.
    #[test]
    fn a_release_with_no_pointer_velocity_falls_back_to_the_drag() {
        let moving = Center::Moving {
            position: adjusted(),
            direction: Vec2::new(10., 0.),
            from_detached: true,
        };
        let fallback = moving.release_velocity(InputFrame {
            pointer_velocity: Vec2::ZERO,
            delta_time: 1. / 60.,
        });
        assert!(
            (fallback.x - 600.).abs() < 1e-3,
            "10 points in a 60 Hz frame is 600 points per second, not {fallback:?}",
        );

        // egui's own reading wins whenever it has one.
        let smoothed = moving.release_velocity(InputFrame {
            pointer_velocity: Vec2::new(42., 0.),
            delta_time: 1. / 60.,
        });
        assert_eq!(Vec2::new(42., 0.), smoothed);
    }
}
