use serde::{Deserialize, Serialize};

mod distance;
mod hail_size;
mod height;
mod precip_rate;
mod speed;
mod temperature;
mod timezone;

pub use distance::DistanceUnit;
pub use hail_size::HailSizeUnit;
pub use height::HeightUnit;
pub use precip_rate::PrecipRateUnit;
pub use speed::SpeedUnit;
pub use temperature::TemperatureUnit;
pub use timezone::TimezonePreference;

pub trait UnitLabel {
    fn display_label(self) -> &'static str;
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
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
