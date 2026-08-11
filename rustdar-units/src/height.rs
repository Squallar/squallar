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

    /// Kilometres into the thousands-unit [`kilo_suffix`](Self::kilo_suffix)
    /// names: kilofeet in a `Feet` locale, kilometres in a `Meters` one.
    ///
    /// # Why this direction had to be added
    ///
    /// Every other conversion here starts from **feet**, because the Level III
    /// products this enum was written for state their heights in them. The
    /// vertical views do not: a cross-section's height axis, its hover readout
    /// and its ladder ceiling are all computed in km MSL out of the beam model,
    /// and nothing here took one. So each of those three sites carried its own
    /// `match prefs.height { Feet => km * 3.28…, Meters => km }` over its own
    /// copy of the feet-per-kilometre constant — three chances for a locale to
    /// be converted twice, or not at all, in the one picture whose entire
    /// subject is height.
    ///
    /// `f64`, unlike its `f32` neighbours, and deliberately: the section's
    /// axis arithmetic is `f64` end to end ([`rustdar_radar::beam`], via
    /// `SectionAxes`), and an axis converts *back* through
    /// [`convert_kilo_to_km`](Self::convert_kilo_to_km) to place the tick it
    /// chose — so a round trip narrowed to `f32` would move a label off the
    /// height it names.
    pub fn convert_km_to_kilo(self, km: f64) -> f64 {
        match self {
            HeightUnit::Feet => km / KM_PER_KFT,
            HeightUnit::Meters => km,
        }
    }

    /// The inverse of [`convert_km_to_kilo`](Self::convert_km_to_kilo).
    ///
    /// Both directions are load-bearing on one axis: the ticks are chosen as
    /// round numbers in the unit the user *reads* — 5, 10, 15 kft — and each
    /// then has to be placed at the height it names, which is a km. Without
    /// this a `Feet` locale gets its ticks at 3.28, 6.56 and 9.84.
    pub fn convert_kilo_to_km(self, shown: f64) -> f64 {
        match self {
            HeightUnit::Feet => shown * KM_PER_KFT,
            HeightUnit::Meters => shown,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            HeightUnit::Feet => "Feet",
            HeightUnit::Meters => "Meters",
        }
    }
}

/// Kilometres in one kilofoot — the international foot, exactly 0.3048 m, which
/// is the definition every other conversion in this module already uses.
const KM_PER_KFT: f64 = 0.3048;

impl UnitLabel for HeightUnit {
    fn display_label(self) -> &'static str {
        HeightUnit::label(self)
    }
}

#[cfg(test)]
mod tests {
    use super::HeightUnit;

    /// The two directions are inverses, so an axis that chooses a tick in the
    /// shown unit and then places it at the height it names lands on the line.
    ///
    /// The expectations are **independent constants**, not the conversion run
    /// backwards: 3.048 km is 10 kft by the definition of the international
    /// foot, and a test that computed its own expectation from
    /// `convert_km_to_kilo` would pass against any consistent pair of wrong
    /// formulas, including the identity.
    #[test]
    fn a_height_survives_the_round_trip_through_either_locale() {
        assert!((HeightUnit::Feet.convert_km_to_kilo(3.048) - 10.0).abs() < 1e-9);
        assert!((HeightUnit::Feet.convert_kilo_to_km(10.0) - 3.048).abs() < 1e-9);
        assert_eq!(HeightUnit::Meters.convert_km_to_kilo(3.048), 3.048);
        assert_eq!(HeightUnit::Meters.convert_kilo_to_km(3.048), 3.048);

        for unit in [HeightUnit::Feet, HeightUnit::Meters] {
            for km in [0.0, 0.4, 3.048, 20.4, 65.0] {
                let there_and_back = unit.convert_kilo_to_km(unit.convert_km_to_kilo(km));
                assert!(
                    (there_and_back - km).abs() < 1e-9,
                    "{unit:?} moved {km} km to {there_and_back} km round trip"
                );
            }
        }
    }

    /// The new direction agrees with the one that was already here, so the two
    /// families cannot drift into two definitions of a foot.
    #[test]
    fn kilometres_and_kilofeet_agree_with_the_existing_feet_conversions() {
        // 3.048 km is 10 000 ft, which `convert_from_feet` already knows.
        assert!((HeightUnit::Meters.convert_from_feet(10_000.0) - 3048.0).abs() < 1e-3);
        assert!((HeightUnit::Feet.convert_km_to_kilo(3.048) - 10.0).abs() < 1e-9);
        // And `convert_kft_to_kilo` maps 10 kft to the same 3.048 km.
        assert!((HeightUnit::Meters.convert_kft_to_kilo(10.0) - 3.048).abs() < 1e-5);
    }
}
