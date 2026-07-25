use serde::{Deserialize, Serialize};

use super::UnitLabel;

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default)]
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
