use super::UserPreferences;
use crate::HailSizeUnit;

/// The unit domain a measured radar value lives in — what a number *is*, so
/// that converting it, suffixing it and choosing its precision are one decision
/// made where the value is declared. Each variant names the **source** unit a
/// value arrives in; the user's [`UserPreferences`] pick the display unit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Quantity {
    SpeedMps,
    /// Height in thousands of feet, as Level III products state heights.
    HeightKft,
    DistanceKm,
    PrecipRateInPerHr,
    /// Hail size in inches, as US hail sizes are reported.
    HailSizeIn,
    TemperatureC,
    EnergyJ,
    /// A dimensionless value with the fixed unit string its colour bar prints.
    Unitless {
        label: &'static str,
    },
}

impl Quantity {
    /// `value` (in this quantity's source unit) converted to the unit the user's
    /// preferences ask for; `EnergyJ` and `Unitless` pass it through.
    pub fn convert(self, value: f32, prefs: &UserPreferences) -> f32 {
        match self {
            Quantity::SpeedMps => prefs.speed.convert_from_ms(value),
            Quantity::HeightKft => prefs.height.convert_kft_to_kilo(value),
            Quantity::DistanceKm => prefs.distance.convert_from_km(f64::from(value)) as f32,
            Quantity::PrecipRateInPerHr => prefs.precip_rate.convert_from_in_per_hr(value),
            Quantity::HailSizeIn => prefs.hail_size.convert_from_inches(value),
            Quantity::TemperatureC => prefs.temperature.convert_from_c(f64::from(value)) as f32,
            Quantity::EnergyJ | Quantity::Unitless { .. } => value,
        }
    }

    pub fn suffix(self, prefs: &UserPreferences) -> &'static str {
        match self {
            Quantity::SpeedMps => prefs.speed.suffix(),
            Quantity::HeightKft => prefs.height.kilo_suffix(),
            Quantity::DistanceKm => prefs.distance.suffix(),
            Quantity::PrecipRateInPerHr => prefs.precip_rate.suffix(),
            // `HailSizeUnit::suffix()` is the inch *mark*, which reads well
            // against a bare number but not as a colour-bar title. `in` is
            // also what MEHS has printed since it shipped.
            Quantity::HailSizeIn => match prefs.hail_size {
                HailSizeUnit::Inches => "in",
                unit => unit.suffix(),
            },
            Quantity::TemperatureC => prefs.temperature.suffix(),
            Quantity::EnergyJ => "J",
            Quantity::Unitless { label } => label,
        }
    }

    /// Decimals a value of this quantity reads well in, in the preferred unit,
    /// matching what `RadarProduct::format_value` prints today.
    pub fn decimals(self, prefs: &UserPreferences) -> usize {
        match self {
            Quantity::SpeedMps => 1,
            Quantity::HeightKft => 1,
            Quantity::DistanceKm => 1,
            Quantity::PrecipRateInPerHr => 2,
            Quantity::HailSizeIn => prefs.hail_size.decimals(),
            Quantity::TemperatureC => 0,
            Quantity::EnergyJ => 0,
            Quantity::Unitless { .. } => 1,
        }
    }
}

/// A value together with the quantity it measures — enough to convert and print
/// it under any [`UserPreferences`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Measured {
    pub value: f32,
    pub quantity: Quantity,
}

impl Measured {
    pub fn convert(&self, prefs: &UserPreferences) -> f32 {
        self.quantity.convert(self.value, prefs)
    }

    /// The converted value at [`Quantity::decimals`] precision followed by
    /// [`Quantity::suffix`], with no trailing space when the suffix is empty.
    pub fn display(&self, prefs: &UserPreferences) -> String {
        let converted = self.convert(prefs);
        let decimals = self.quantity.decimals(prefs);
        let suffix = self.quantity.suffix(prefs);
        if suffix.is_empty() {
            format!("{converted:.decimals$}")
        } else {
            format!("{converted:.decimals$} {suffix}")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        DistanceUnit, HeightUnit, PrecipRateUnit, SpeedUnit, TemperatureUnit, UserPreferences,
    };

    /// Every variant, under default preferences and under one non-default
    /// preference each, against **hand-written** expected strings, so a
    /// consistent pair of wrong formulas cannot pass.
    #[test]
    fn every_quantity_converts_suffixes_and_displays_per_the_preference() {
        let defaults = UserPreferences::default();
        let all_flipped = UserPreferences {
            speed: SpeedUnit::MetersPerSec,
            distance: DistanceUnit::Miles,
            height: HeightUnit::Meters,
            precip_rate: PrecipRateUnit::MillimetersPerHour,
            hail_size: HailSizeUnit::Centimeters,
            temperature: TemperatureUnit::Celsius,
            ..UserPreferences::default()
        };

        let metric_speed = UserPreferences {
            speed: SpeedUnit::MetersPerSec,
            ..UserPreferences::default()
        };
        let metric_height = UserPreferences {
            height: HeightUnit::Meters,
            ..UserPreferences::default()
        };
        let miles = UserPreferences {
            distance: DistanceUnit::Miles,
            ..UserPreferences::default()
        };
        let mm_per_hr = UserPreferences {
            precip_rate: PrecipRateUnit::MillimetersPerHour,
            ..UserPreferences::default()
        };
        let cm_hail = UserPreferences {
            hail_size: HailSizeUnit::Centimeters,
            ..UserPreferences::default()
        };
        let celsius = UserPreferences {
            temperature: TemperatureUnit::Celsius,
            ..UserPreferences::default()
        };

        let table: &[(&str, Quantity, f32, &UserPreferences, &str, usize, &str)] = &[
            // 10 m/s is 22.3694 mph.
            (
                "speed default",
                Quantity::SpeedMps,
                10.0,
                &defaults,
                "mph",
                1,
                "22.4 mph",
            ),
            (
                "speed m/s",
                Quantity::SpeedMps,
                10.0,
                &metric_speed,
                "m/s",
                1,
                "10.0 m/s",
            ),
            // 10 kft is 3.048 km by the definition of the international foot.
            (
                "height default",
                Quantity::HeightKft,
                10.0,
                &defaults,
                "kft",
                1,
                "10.0 kft",
            ),
            (
                "height metric",
                Quantity::HeightKft,
                10.0,
                &metric_height,
                "km",
                1,
                "3.0 km",
            ),
            // 10 km is 6.21371 mi.
            (
                "distance default",
                Quantity::DistanceKm,
                10.0,
                &defaults,
                "km",
                1,
                "10.0 km",
            ),
            (
                "distance miles",
                Quantity::DistanceKm,
                10.0,
                &miles,
                "mi",
                1,
                "6.2 mi",
            ),
            // 1.5 in is 38.1 mm.
            (
                "precip default",
                Quantity::PrecipRateInPerHr,
                1.5,
                &defaults,
                "in/hr",
                2,
                "1.50 in/hr",
            ),
            (
                "precip mm/hr",
                Quantity::PrecipRateInPerHr,
                1.5,
                &mm_per_hr,
                "mm/hr",
                2,
                "38.10 mm/hr",
            ),
            // Inches suffix is "in", never the inch mark; 1.75 in is 4.445 cm.
            (
                "hail default",
                Quantity::HailSizeIn,
                1.75,
                &defaults,
                "in",
                2,
                "1.75 in",
            ),
            (
                "hail cm",
                Quantity::HailSizeIn,
                1.75,
                &cm_hail,
                "cm",
                1,
                "4.4 cm",
            ),
            // 20 °C is 68 °F.
            (
                "temp default",
                Quantity::TemperatureC,
                20.0,
                &defaults,
                "°F",
                0,
                "68 °F",
            ),
            (
                "temp celsius",
                Quantity::TemperatureC,
                20.0,
                &celsius,
                "°C",
                0,
                "20 °C",
            ),
            // No preference: identical under defaults and everything flipped.
            (
                "energy default",
                Quantity::EnergyJ,
                5.0,
                &defaults,
                "J",
                0,
                "5 J",
            ),
            (
                "energy flipped",
                Quantity::EnergyJ,
                5.0,
                &all_flipped,
                "J",
                0,
                "5 J",
            ),
            (
                "unitless default",
                Quantity::Unitless { label: "dBZ" },
                42.5,
                &defaults,
                "dBZ",
                1,
                "42.5 dBZ",
            ),
            (
                "unitless flipped",
                Quantity::Unitless { label: "dBZ" },
                42.5,
                &all_flipped,
                "dBZ",
                1,
                "42.5 dBZ",
            ),
        ];

        for &(case, quantity, value, prefs, suffix, decimals, display) in table {
            assert_eq!(quantity.suffix(prefs), suffix, "{case}: suffix");
            assert_eq!(quantity.decimals(prefs), decimals, "{case}: decimals");
            let measured = Measured { value, quantity };
            assert_eq!(measured.display(prefs), display, "{case}: display");
        }
    }

    #[test]
    fn an_empty_suffix_is_trimmed_from_the_display() {
        let prefs = UserPreferences::default();
        let bare = Measured {
            value: 2.5,
            quantity: Quantity::Unitless { label: "" },
        };
        assert_eq!(bare.display(&prefs), "2.5");
    }

    /// One spot check per converting variant against a hand-computed value.
    #[test]
    fn convert_routes_each_quantity_through_its_own_unit_table() {
        let prefs = UserPreferences {
            speed: SpeedUnit::KilometersPerHour,
            height: HeightUnit::Meters,
            distance: DistanceUnit::NauticalMiles,
            precip_rate: PrecipRateUnit::MillimetersPerHour,
            hail_size: HailSizeUnit::Millimeters,
            temperature: TemperatureUnit::Fahrenheit,
            ..UserPreferences::default()
        };

        assert!((Quantity::SpeedMps.convert(10.0, &prefs) - 36.0).abs() < 1e-3);
        assert!((Quantity::HeightKft.convert(10.0, &prefs) - 3.048).abs() < 1e-4);
        assert!((Quantity::DistanceKm.convert(10.0, &prefs) - 5.39957).abs() < 1e-4);
        assert!((Quantity::PrecipRateInPerHr.convert(2.0, &prefs) - 50.8).abs() < 1e-3);
        assert!((Quantity::HailSizeIn.convert(2.0, &prefs) - 50.8).abs() < 1e-3);
        assert!((Quantity::TemperatureC.convert(0.0, &prefs) - 32.0).abs() < 1e-4);
        assert_eq!(Quantity::EnergyJ.convert(7.25, &prefs), 7.25);
        assert_eq!(
            Quantity::Unitless { label: "NROT" }.convert(-3.5, &prefs),
            -3.5
        );
    }
}
