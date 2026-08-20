use serde::{Deserialize, Serialize};

mod distance;
mod hail_size;
mod height;
mod precip_rate;
mod quantity;
mod speed;
mod temperature;
mod timezone;

pub use distance::DistanceUnit;
pub use hail_size::HailSizeUnit;
pub use height::HeightUnit;
pub use precip_rate::PrecipRateUnit;
pub use quantity::{Measured, Quantity};
pub use speed::SpeedUnit;
pub use temperature::TemperatureUnit;
pub use timezone::TimezonePreference;

pub trait UnitLabel {
    fn display_label(self) -> &'static str;
}

/// `PartialEq/Eq/Hash` because this is a **memo key**: every user-facing number
/// is formatted through it, so anything caching a formatted string has to be
/// able to say "the preferences did not move" without re-formatting. There is
/// no float in here, and one arriving later would have to answer for `Eq`.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq, Hash)]
#[serde(default)]
pub struct UserPreferences {
    pub speed: SpeedUnit,
    pub distance: DistanceUnit,
    pub height: HeightUnit,
    pub precip_rate: PrecipRateUnit,
    pub hail_size: HailSizeUnit,
    pub temperature: TemperatureUnit,
    pub timezone: TimezonePreference,
}
