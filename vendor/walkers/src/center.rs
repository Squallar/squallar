use crate::{Position, position::AdjustedPosition};
use egui::{DragPanButtons, PointerButton, Response, Vec2};

/// Time constant of inertia stopping filter
const INERTIA_TAU: f32 = 0.2f32;

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
        direction: Vec2,
        amount: f32,
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

    /// A drag ended and the widget saw the release edge.
    Released,

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

/// Whether a gesture that ends a drag hands the map its momentum.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Coast {
    Yes,
    No,
}

impl Center {
    pub(crate) fn handle_gestures(
        &mut self,
        response: &Response,
        my_position: Position,
        pull_to_my_position_threshold: f32,
        drag_pan_buttons: DragPanButtons,
    ) -> bool {
        let gesture = self.classify(response, drag_pan_buttons);
        self.apply(gesture, my_position, pull_to_my_position_threshold)
    }

    /// Read one frame's response as a [`Gesture`].
    ///
    /// `drag_stopped` is asked before the state is, and it is button-agnostic
    /// while `dragged_by` is not: a drag on a button that does not pan still
    /// ends through the [`Gesture::Released`] arm, exactly as it did when this
    /// was an `if`/`else if` chain over the response alone.
    pub(crate) fn classify(&self, response: &Response, buttons: DragPanButtons) -> Gesture {
        if dragged_by(response, buttons) {
            Gesture::Dragging(response.drag_delta())
        } else if response.drag_stopped() {
            Gesture::Released
        } else {
            Gesture::quiet(self)
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
            Gesture::Released => {
                self.drag_stopped(pull_to_my_position_threshold, Coast::Yes);
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
            direction,
            from_detached,
        } = &self
        {
            if *from_detached || position.offset_length() > pull_to_my_position_threshold {
                *self = match coast {
                    Coast::Yes => Center::Inertia {
                        position: position.clone(),
                        direction: direction.normalized(),
                        amount: direction.length(),
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
            Center::Inertia {
                position,
                direction,
                amount,
            } => {
                *self = if amount < &mut 0.1 {
                    Center::Exact(position.to_owned())
                } else {
                    // Exponentially drive the `amount` value towards zero
                    let lp_factor = INERTIA_TAU / (delta_time + INERTIA_TAU);

                    Center::Inertia {
                        position: position.clone().shift(*direction * *amount, zoom),
                        direction: *direction,
                        amount: *amount * lp_factor,
                    }
                };
                true
            }
            Center::PulledToMyPosition(position) => {
                let position = position.clone().half_offset();
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
            Center::Inertia {
                position,
                direction,
                amount,
            } => Center::Inertia {
                position: position.shift(offset, zoom),
                direction,
                amount,
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
                direction: Vec2::new(1., 0.),
                amount: 5.,
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

        assert!(center.apply(Gesture::Released, my, THRESHOLD));
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
}
