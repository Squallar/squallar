use serde::{Deserialize, Serialize};

use super::UnitLabel;

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum PrecipRateUnit {
    #[default]
    InchesPerHour,
    MillimetersPerHour,
}

impl PrecipRateUnit {
    pub const ALL: &[PrecipRateUnit] = &[
        PrecipRateUnit::InchesPerHour,
        PrecipRateUnit::MillimetersPerHour,
    ];

    pub fn convert_from_in_per_hr(self, value: f32) -> f32 {
        match self {
            PrecipRateUnit::InchesPerHour => value,
            PrecipRateUnit::MillimetersPerHour => value * 25.4,
        }
    }

    pub fn suffix(self) -> &'static str {
        match self {
            PrecipRateUnit::InchesPerHour => "in/hr",
            PrecipRateUnit::MillimetersPerHour => "mm/hr",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            PrecipRateUnit::InchesPerHour => "in/hr",
            PrecipRateUnit::MillimetersPerHour => "mm/hr",
        }
    }
}

impl UnitLabel for PrecipRateUnit {
    fn display_label(self) -> &'static str {
        PrecipRateUnit::label(self)
    }
}
