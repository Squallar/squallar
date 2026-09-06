#[derive(thiserror::Error, Debug, PartialEq, Eq)]
#[error("invalid zoom level")]
pub struct InvalidZoom;

#[derive(Debug, Clone, Copy)]
#[cfg_attr(feature = "serde", derive(::serde::Serialize, ::serde::Deserialize))]
pub(crate) struct Zoom(f64);

impl TryFrom<f64> for Zoom {
    type Error = InvalidZoom;

    fn try_from(value: f64) -> Result<Self, Self::Error> {
        // The upper limit is artificial. Should it be removed altogether?
        if !(0. ..=26.).contains(&value) {
            Err(InvalidZoom)
        } else {
            Ok(Self(value))
        }
    }
}

// The reverse shouldn't be implemented, since we already have TryInto<f32>.
#[allow(clippy::from_over_into)]
impl Into<f64> for Zoom {
    fn into(self) -> f64 {
        self.0
    }
}

impl Default for Zoom {
    fn default() -> Self {
        Self(16.)
    }
}

impl Zoom {
    pub fn round(&self) -> u8 {
        self.0.round() as u8
    }

    pub fn zoom_in(&mut self) -> Result<(), InvalidZoom> {
        *self = Self::try_from(self.0 + 1.)?;
        Ok(())
    }

    pub fn zoom_out(&mut self) -> Result<(), InvalidZoom> {
        *self = Self::try_from(self.0 - 1.)?;
        Ok(())
    }

    /// Zoom using a relative value.
    pub fn zoom_by(&mut self, value: f64) {
        if let Ok(new_self) = Self::try_from(self.0 + value) {
            *self = new_self;
        }
    }

    /// Raise this zoom to `floor` if it is below it. Returns whether it moved.
    ///
    /// **Only ever raises**, and it is a separate operation from `zoom_by`
    /// rather than a second range on this type because the floor is not a
    /// property of a zoom at all — it is a property of the viewport the map is
    /// being drawn into, which only the widget knows. The artificial `0..=26`
    /// above is untouched.
    ///
    /// A floor this type cannot represent moves nothing: below zero it is
    /// already satisfied by the range's own bottom, and above 26 it would need
    /// a viewport 1.7e10 points across. Neither is a reason to leave the zoom
    /// somewhere `try_from` refuses. `NaN` moves nothing for the same reason a
    /// `NaN` viewport says nothing about coverage.
    pub(crate) fn raise_to(&mut self, floor: f64) -> bool {
        if floor.is_nan() || floor <= self.0 {
            return false;
        }
        match Self::try_from(floor) {
            Ok(raised) => {
                *self = raised;
                true
            }
            Err(InvalidZoom) => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_constructing_zoom() {
        assert_eq!(16, Zoom::default().round());
        assert_eq!(26, Zoom::try_from(26.).unwrap().round());
        assert_eq!(InvalidZoom, Zoom::try_from(27.).unwrap_err());
    }

    #[test]
    fn test_zooming_in() {
        let mut zoom = Zoom::try_from(25.).unwrap();
        assert!(zoom.zoom_in().is_ok());
        assert_eq!(26, zoom.round());
        assert_eq!(Err(InvalidZoom), zoom.zoom_in());
    }

    /// A floor raises a zoom below it, leaves one at or above it alone, and is
    /// itself bounded by the type's own range.
    #[test]
    fn raising_a_zoom_to_a_viewport_floor() {
        const FLOOR: f64 = 3.4908508767402977;

        let mut zoom = Zoom::try_from(3.326757482253017).unwrap();
        assert!(zoom.raise_to(FLOOR));
        assert_eq!(Into::<f64>::into(zoom).to_bits(), FLOOR.to_bits());

        // Idempotent: the second frame's floor is the same floor.
        assert!(!zoom.raise_to(FLOOR));
        assert_eq!(Into::<f64>::into(zoom).to_bits(), FLOOR.to_bits());

        // One ulp either side of it, through the type this time.
        let mut zoom = Zoom::try_from(f64::from_bits(FLOOR.to_bits() - 1)).unwrap();
        assert!(zoom.raise_to(FLOOR));
        assert_eq!(Into::<f64>::into(zoom).to_bits(), FLOOR.to_bits());

        let above = f64::from_bits(FLOOR.to_bits() + 1);
        let mut zoom = Zoom::try_from(above).unwrap();
        assert!(!zoom.raise_to(FLOOR));
        assert_eq!(Into::<f64>::into(zoom).to_bits(), above.to_bits());

        // A floor the type cannot hold, and a floor that is no floor at all.
        let mut zoom = Zoom::try_from(5.).unwrap();
        assert!(!zoom.raise_to(27.));
        assert!(!zoom.raise_to(-3.));
        assert!(!zoom.raise_to(f64::NAN));
        assert!(!zoom.raise_to(f64::NEG_INFINITY));
        assert_eq!(Into::<f64>::into(zoom).to_bits(), 5.0f64.to_bits());
    }

    #[test]
    fn test_zooming_out() {
        let mut zoom = Zoom::try_from(1.).unwrap();
        assert!(zoom.zoom_out().is_ok());
        assert_eq!(0, zoom.round());
        assert_eq!(Err(InvalidZoom), zoom.zoom_out());
    }
}
