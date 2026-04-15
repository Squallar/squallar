use serde::{Deserialize, Serialize};

use super::UnitLabel;

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum TemperatureUnit {
    #[default]
    Fahrenheit,
    Celsius,
}

impl TemperatureUnit {
    pub const ALL: &[TemperatureUnit] = &[
        TemperatureUnit::Fahrenheit,
        TemperatureUnit::Celsius,
    ];

    /// Convert a value in °F to this unit.
    pub fn convert_from_f(self, value: f64) -> f64 {
        match self {
            TemperatureUnit::Fahrenheit => value,
            TemperatureUnit::Celsius => (value - 32.0) * 5.0 / 9.0,
        }
    }

    /// Convert a value in °C to this unit.
    pub fn convert_from_c(self, value: f64) -> f64 {
        match self {
            TemperatureUnit::Fahrenheit => value * 9.0 / 5.0 + 32.0,
            TemperatureUnit::Celsius => value,
        }
    }

    pub fn suffix(self) -> &'static str {
        match self {
            TemperatureUnit::Fahrenheit => "°F",
            TemperatureUnit::Celsius => "°C",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            TemperatureUnit::Fahrenheit => "Fahrenheit",
            TemperatureUnit::Celsius => "Celsius",
        }
    }
}

impl UnitLabel for TemperatureUnit {
    fn display_label(self) -> &'static str {
        TemperatureUnit::label(self)
    }
}
