use chrono::NaiveDateTime;
use serde::{Deserialize, Serialize};

// ————————————————————————————————————————————————————————————————————
// Unit enums
// ————————————————————————————————————————————————————————————————————

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default)]
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

    /// Convert a value in m/s to this unit.
    pub fn convert_from_ms(self, value: f32) -> f32 {
        match self {
            SpeedUnit::Mph => value * 2.23694,
            SpeedUnit::MetersPerSec => value,
            SpeedUnit::KilometersPerHour => value * 3.6,
            SpeedUnit::Knots => value * 1.94384,
        }
    }

    /// Convert a value in knots to this unit.
    pub fn convert_from_knots(self, value: f32) -> f32 {
        match self {
            SpeedUnit::Mph => value * 1.15078,
            SpeedUnit::MetersPerSec => value * 0.514444,
            SpeedUnit::KilometersPerHour => value * 1.852,
            SpeedUnit::Knots => value,
        }
    }

    /// Convert a value in mph to this unit.
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

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum HeightUnit {
    #[default]
    Feet,
    Meters,
}

impl HeightUnit {
    pub const ALL: &[HeightUnit] = &[HeightUnit::Feet, HeightUnit::Meters];

    /// Convert a value in feet to this unit.
    pub fn convert_from_feet(self, value: f32) -> f32 {
        match self {
            HeightUnit::Feet => value,
            HeightUnit::Meters => value * 0.3048,
        }
    }

    /// Convert a value in thousands of feet (kft) to this unit.
    /// Returns the value in the base unit (feet or meters), not thousands.
    pub fn convert_from_kft(self, value: f32) -> f32 {
        let feet = value * 1000.0;
        self.convert_from_feet(feet)
    }

    /// Suffix for base height values (feet or meters).
    pub fn suffix(self) -> &'static str {
        match self {
            HeightUnit::Feet => "ft",
            HeightUnit::Meters => "m",
        }
    }

    /// Suffix for kilo-height values (kft or km).
    pub fn kilo_suffix(self) -> &'static str {
        match self {
            HeightUnit::Feet => "kft",
            HeightUnit::Meters => "km",
        }
    }

    /// Convert a value in kft to the equivalent kilo-unit (kft or km).
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

    /// Convert a value in inches/hour to this unit.
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

    /// Convert a value in inches to this unit.
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

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum TimezonePreference {
    #[default]
    Local,
    Utc,
}

impl TimezonePreference {
    pub const ALL: &[TimezonePreference] = &[TimezonePreference::Local, TimezonePreference::Utc];

    pub fn label(self) -> &'static str {
        match self {
            TimezonePreference::Local => "Local Time",
            TimezonePreference::Utc => "UTC",
        }
    }

    /// Format a `NaiveDateTime` that is known to be in UTC, converting to the
    /// user's preferred timezone. Appends a timezone indicator.
    pub fn format_naive_utc(self, ts: NaiveDateTime, fmt: &str) -> String {
        match self {
            TimezonePreference::Utc => {
                format!("{} UTC", ts.format(fmt))
            }
            TimezonePreference::Local => {
                let utc_dt = chrono::TimeZone::from_utc_datetime(&chrono::Utc, &ts);
                let local_dt = utc_dt.with_timezone(&chrono::Local);
                // Use %Z for timezone abbreviation (e.g. "CDT", "EST")
                let extended_fmt = format!("{} %Z", fmt);
                local_dt.format(&extended_fmt).to_string()
            }
        }
    }

    /// Format an RFC 3339 / ISO 8601 timestamp string into a human-readable
    /// form in the user's preferred timezone.
    pub fn format_rfc3339(self, iso: &str) -> String {
        let Ok(dt) = chrono::DateTime::parse_from_rfc3339(iso) else {
            return iso.to_string();
        };
        match self {
            TimezonePreference::Utc => {
                let utc_dt = dt.with_timezone(&chrono::Utc);
                utc_dt.format("%b %d %Y %H:%M UTC").to_string()
            }
            TimezonePreference::Local => {
                let local_dt = dt.with_timezone(&chrono::Local);
                local_dt.format("%b %d %Y %H:%M %Z").to_string()
            }
        }
    }

    /// Convert a naive UTC datetime to the user's display timezone (as naive).
    /// Useful for populating time-picker fields.
    pub fn utc_to_display(self, ts: NaiveDateTime) -> NaiveDateTime {
        match self {
            TimezonePreference::Utc => ts,
            TimezonePreference::Local => {
                let utc_dt = chrono::TimeZone::from_utc_datetime(&chrono::Utc, &ts);
                utc_dt.with_timezone(&chrono::Local).naive_local()
            }
        }
    }
}

// ————————————————————————————————————————————————————————————————————
// User preferences
// ————————————————————————————————————————————————————————————————————

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(default)]
pub struct UserPreferences {
    pub speed: SpeedUnit,
    pub distance: DistanceUnit,
    pub height: HeightUnit,
    pub precip_rate: PrecipRateUnit,
    pub hail_size: HailSizeUnit,
    pub timezone: TimezonePreference,
}
