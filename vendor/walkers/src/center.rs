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

impl Center {
    pub(crate) fn handle_gestures(
        &mut self,
        response: &Response,
        my_position: Position,
        pull_to_my_position_threshold: f32,
        drag_pan_buttons: DragPanButtons,
    ) -> bool {
        if dragged_by(response, drag_pan_buttons) {
            self.dragged_by(my_position, response);
            true
        } else if response.drag_stopped() {
            self.drag_stopped(pull_to_my_position_threshold);
            true
        } else {
            false
        }
    }

    fn dragged_by(&mut self, my_position: Position, response: &Response) {
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
            direction: response.drag_delta(),
            from_detached,
        };
    }

    fn drag_stopped(&mut self, pull_to_my_position_threshold: f32) {
        if let Center::Moving {
            position,
            direction,
            from_detached,
        } = &self
        {
            if *from_detached || position.offset_length() > pull_to_my_position_threshold {
                *self = Center::Inertia {
                    position: position.clone(),
                    direction: direction.normalized(),
                    amount: direction.length(),
                };
            } else {
                *self = Center::PulledToMyPosition(position.to_owned());
            }
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
}
