use serde::{Deserialize, Serialize};

use super::UnitLabel;

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub enum SpeedUnit {
    #[default]
    Mph,
    MetersPerSec,
    KilometersPerHour,
    Knots,
}

impl SpeedUnit {
    pub const ALL: &[SpeedUnit] = &[
        SpeedUnit::Mph,
        SpeedUnit::MetersPerSec,
        SpeedUnit::KilometersPerHour,
        SpeedUnit::Knots,
    ];

    pub fn convert_from_ms(self, value: f32) -> f32 {
        match self {
            SpeedUnit::Mph => value * 2.23694,
            SpeedUnit::MetersPerSec => value,
            SpeedUnit::KilometersPerHour => value * 3.6,
            SpeedUnit::Knots => value * 1.94384,
        }
    }

    pub fn convert_from_knots(self, value: f32) -> f32 {
        match self {
            SpeedUnit::Mph => value * 1.15078,
            SpeedUnit::MetersPerSec => value * 0.514444,
            SpeedUnit::KilometersPerHour => value * 1.852,
            SpeedUnit::Knots => value,
        }
    }

    pub fn convert_from_mph(self, value: f32) -> f32 {
        match self {
            SpeedUnit::Mph => value,
            SpeedUnit::MetersPerSec => value / 2.23694,
            SpeedUnit::KilometersPerHour => value * 1.60934,
            SpeedUnit::Knots => value * 0.868976,
        }
    }

    pub fn suffix(self) -> &'static str {
        match self {
            SpeedUnit::Mph => "mph",
            SpeedUnit::MetersPerSec => "m/s",
            SpeedUnit::KilometersPerHour => "km/h",
            SpeedUnit::Knots => "kt",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            SpeedUnit::Mph => "mph",
            SpeedUnit::MetersPerSec => "m/s",
            SpeedUnit::KilometersPerHour => "km/h",
            SpeedUnit::Knots => "knots",
        }
    }
}

impl UnitLabel for SpeedUnit {
    fn display_label(self) -> &'static str {
        SpeedUnit::label(self)
    }
}
