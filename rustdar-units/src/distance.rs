use serde::{Deserialize, Serialize};

use super::UnitLabel;

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum DistanceUnit {
    #[default]
    Kilometers,
    Miles,
    NauticalMiles,
}

impl DistanceUnit {
    pub const ALL: &[DistanceUnit] = &[
        DistanceUnit::Kilometers,
        DistanceUnit::Miles,
        DistanceUnit::NauticalMiles,
    ];

    /// Convert a value in kilometers to this unit.
    pub fn convert_from_km(self, value: f64) -> f64 {
        match self {
            DistanceUnit::Kilometers => value,
            DistanceUnit::Miles => value * 0.621371,
            DistanceUnit::NauticalMiles => value * 0.539957,
        }
    }

    pub fn suffix(self) -> &'static str {
        match self {
            DistanceUnit::Kilometers => "km",
            DistanceUnit::Miles => "mi",
            DistanceUnit::NauticalMiles => "nmi",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            DistanceUnit::Kilometers => "Kilometers",
            DistanceUnit::Miles => "Miles",
            DistanceUnit::NauticalMiles => "Nautical miles",
        }
    }
}

impl UnitLabel for DistanceUnit {
    fn display_label(self) -> &'static str {
        DistanceUnit::label(self)
    }
}
