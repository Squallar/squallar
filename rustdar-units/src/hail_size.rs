use serde::{Deserialize, Serialize};

use super::UnitLabel;

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub enum HailSizeUnit {
    #[default]
    Inches,
    Centimeters,
    Millimeters,
}

impl HailSizeUnit {
    pub const ALL: &[HailSizeUnit] = &[
        HailSizeUnit::Inches,
        HailSizeUnit::Centimeters,
        HailSizeUnit::Millimeters,
    ];

    pub fn convert_from_inches(self, value: f32) -> f32 {
        match self {
            HailSizeUnit::Inches => value,
            HailSizeUnit::Centimeters => value * 2.54,
            HailSizeUnit::Millimeters => value * 25.4,
        }
    }

    pub fn suffix(self) -> &'static str {
        match self {
            HailSizeUnit::Inches => "\"",
            HailSizeUnit::Centimeters => "cm",
            HailSizeUnit::Millimeters => "mm",
        }
    }

    /// Decimals a hail size reads well in, in this unit.
    ///
    /// Hail is *reported* in quarter-inch steps, so roughly 6 mm is all the
    /// resolution the number carries: `25.40 mm` claims a precision no hail size
    /// has. Hundredths of an inch land the quarter steps exactly; tenths of a
    /// centimetre and whole millimetres land them to within half a millimetre.
    pub fn decimals(self) -> usize {
        match self {
            HailSizeUnit::Inches => 2,
            HailSizeUnit::Centimeters => 1,
            HailSizeUnit::Millimeters => 0,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            HailSizeUnit::Inches => "Inches",
            HailSizeUnit::Centimeters => "Centimeters",
            HailSizeUnit::Millimeters => "Millimeters",
        }
    }
}

impl UnitLabel for HailSizeUnit {
    fn display_label(self) -> &'static str {
        HailSizeUnit::label(self)
    }
}
