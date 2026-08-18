use super::UserPreferences;
use crate::HailSizeUnit;

/// The unit domain a measured radar value lives in — what a number *is*, so
/// that converting it, suffixing it and choosing its precision are one
/// decision made where the value is declared rather than three matches kept
/// in sync where it is printed.
///
/// Each variant names the **source** unit a value arrives in (`SpeedMps` is
/// metres per second, `HeightKft` is thousands of feet, …); the user's
/// [`UserPreferences`] pick what it is displayed as. `Unitless` carries its
/// own fixed label because a dimensionless field still titles its colour bar
/// (`dBZ`, `CC`, `NROT`).
///
/// Declared here, in `rustdar-units`, so that a product registry (in
/// `rustdar-radar`) and the things that print values (legends, readouts) can
/// share the vocabulary without either depending on the other.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Quantity {
    /// Speed in metres per second.
    SpeedMps,
    /// Height in thousands of feet (the unit Level III products state
    /// heights in).
    HeightKft,
    /// Distance in kilometres.
    DistanceKm,
    /// Precipitation rate in inches per hour.
    PrecipRateInPerHr,
    /// Hail size in inches (the unit US hail sizes are reported in).
    HailSizeIn,
    /// Temperature in degrees Celsius.
    TemperatureC,
    /// Energy in joules. No preference exists for it; it passes through.
    EnergyJ,
    /// A dimensionless value, labelled with the fixed unit string its colour
    /// bar and readout print (`""` for a truly bare number).
    Unitless { label: &'static str },
}

impl Quantity {
    /// `value` (in this quantity's source unit) converted to the unit the
    /// user's preferences ask for.
    ///
    /// Distance and temperature convert through `f64` — the round trip their
    /// unit enums expose — and narrow back to `f32` at the end. `EnergyJ` and
    /// `Unitless` have no preference to consult and pass the value through.
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

    /// The unit string printed after a converted value — and as a colour
    /// bar's title.
    pub fn suffix(self, prefs: &UserPreferences) -> &'static str {
        match self {
            Quantity::SpeedMps => prefs.speed.suffix(),
            Quantity::HeightKft => prefs.height.kilo_suffix(),
            Quantity::DistanceKm => prefs.distance.suffix(),
            Quantity::PrecipRateInPerHr => prefs.precip_rate.suffix(),
            // `HailSizeUnit::suffix()` is the inch *mark*, which reads well
            // pressed against a bare number (`1.75"`, as the storm-report popup
            // writes it) but not as a colour-bar title, and not after the space
            // this crate's readouts put before their unit. `in` is also what
            // MEHS has printed since it shipped, so the default reading is
            // character for character what it was. Every other unit takes its
            // own suffix.
            Quantity::HailSizeIn => match prefs.hail_size {
                HailSizeUnit::Inches => "in",
                unit => unit.suffix(),
            },
            Quantity::TemperatureC => prefs.temperature.suffix(),
            Quantity::EnergyJ => "J",
            Quantity::Unitless { label } => label,
        }
    }

    /// Decimals a value of this quantity reads well in, in the preferred
    /// unit.
    ///
    /// Speed (1), height (1), precipitation rate (2) and hail size
    /// (`prefs.hail_size.decimals()`) match the precision
    /// `RadarProduct::format_value` prints those quantities at today.
    /// Distance (1), temperature (0), energy (0) and `Unitless` (1) have no
    /// consumer of this method yet: those are stated defaults, adjustable
    /// when their consumer lands (E9/M9).
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

/// A value together with the quantity it measures — enough to convert and
/// print it under any [`UserPreferences`] without asking anything else.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Measured {
    /// In `quantity`'s source unit.
    pub value: f32,
    pub quantity: Quantity,
}

impl Measured {
    /// [`Quantity::convert`] applied to this value.
    pub fn convert(&self, prefs: &UserPreferences) -> f32 {
        self.quantity.convert(self.value, prefs)
    }

    /// The converted value at [`Quantity::decimals`] precision, followed by
    /// [`Quantity::suffix`] — with no trailing space when the suffix is
    /// empty.
    ///
    /// Not consumed by `rustdar-radar` at M4: `RadarProduct::format_value`
    /// deliberately keeps its per-product string shapes (the comment there
    /// says why). This is the vocabulary the overlay legends and the E9
    /// field registry adopt.
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
    /// preference each, against **hand-written** expected strings — computed
    /// by hand from the unit tables, not by running the conversion the test
    /// is checking, so a consistent pair of wrong formulas cannot pass.
    ///
    /// `EnergyJ` and `Unitless` have no preference; their "non-default" rows
    /// flip every preference at once and pin that the output does not move.
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

        // (case, quantity, raw value, prefs, suffix, decimals, display)
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
            // Inches suffix is "in" — the colour-bar rule — never the inch
            // mark `"`; 1.75 in is 4.445 cm, at centimetres' one decimal.
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
            // No preference: identical under defaults and under everything
            // flipped at once.
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

    /// An empty `Unitless` label leaves a bare number: no trailing space.
    #[test]
    fn an_empty_suffix_is_trimmed_from_the_display() {
        let prefs = UserPreferences::default();
        let bare = Measured {
            value: 2.5,
            quantity: Quantity::Unitless { label: "" },
        };
        assert_eq!(bare.display(&prefs), "2.5");
    }

    /// The conversions are the unit enums' own: one spot check per converting
    /// variant against an independently hand-computed value, so `convert`
    /// cannot silently route a quantity through the wrong unit table.
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

        // 10 m/s = 36 km/h.
        assert!((Quantity::SpeedMps.convert(10.0, &prefs) - 36.0).abs() < 1e-3);
        // 10 kft = 3.048 km.
        assert!((Quantity::HeightKft.convert(10.0, &prefs) - 3.048).abs() < 1e-4);
        // 10 km = 5.39957 nmi.
        assert!((Quantity::DistanceKm.convert(10.0, &prefs) - 5.39957).abs() < 1e-4);
        // 2 in/hr = 50.8 mm/hr.
        assert!((Quantity::PrecipRateInPerHr.convert(2.0, &prefs) - 50.8).abs() < 1e-3);
        // 2 in = 50.8 mm.
        assert!((Quantity::HailSizeIn.convert(2.0, &prefs) - 50.8).abs() < 1e-3);
        // 0 °C = 32 °F.
        assert!((Quantity::TemperatureC.convert(0.0, &prefs) - 32.0).abs() < 1e-4);
        // Pass-throughs.
        assert_eq!(Quantity::EnergyJ.convert(7.25, &prefs), 7.25);
        assert_eq!(
            Quantity::Unitless { label: "NROT" }.convert(-3.5, &prefs),
            -3.5
        );
    }
}
