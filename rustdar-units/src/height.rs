use serde::{Deserialize, Serialize};

use super::UnitLabel;

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum HeightUnit {
    #[default]
    Feet,
    Meters,
}

impl HeightUnit {
    pub const ALL: &[HeightUnit] = &[HeightUnit::Feet, HeightUnit::Meters];

    pub fn convert_from_feet(self, value: f32) -> f32 {
        match self {
            HeightUnit::Feet => value,
            HeightUnit::Meters => value * 0.3048,
        }
    }

    /// Returns the base unit (feet or meters), not thousands of it.
    pub fn convert_from_kft(self, value: f32) -> f32 {
        let feet = value * 1000.0;
        self.convert_from_feet(feet)
    }

    /// Base units.
    pub fn suffix(self) -> &'static str {
        match self {
            HeightUnit::Feet => "ft",
            HeightUnit::Meters => "m",
        }
    }

    /// Thousands: kft or km.
    pub fn kilo_suffix(self) -> &'static str {
        match self {
            HeightUnit::Feet => "kft",
            HeightUnit::Meters => "km",
        }
    }

    pub fn convert_kft_to_kilo(self, value: f32) -> f32 {
        match self {
            HeightUnit::Feet => value,
            HeightUnit::Meters => value * 0.3048, // kft → km
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            HeightUnit::Feet => "Feet",
            HeightUnit::Meters => "Meters",
        }
    }
}

impl UnitLabel for HeightUnit {
    fn display_label(self) -> &'static str {
        HeightUnit::label(self)
    }
}
