//! METAR surface observation data types.

/// Flight category derived from ceiling and visibility.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
pub enum FlightCategory {
    VFR,
    MVFR,
    IFR,
    LIFR,
}

impl FlightCategory {
    /// RGBA fill color for map markers (standard aviation colors).
    pub fn color_rgba(self) -> [u8; 4] {
        match self {
            FlightCategory::VFR => [0, 180, 0, 220],       // green
            FlightCategory::MVFR => [0, 100, 255, 220],    // blue
            FlightCategory::IFR => [220, 40, 40, 220],     // red
            FlightCategory::LIFR => [180, 0, 180, 220],    // magenta
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            FlightCategory::VFR => "VFR",
            FlightCategory::MVFR => "MVFR",
            FlightCategory::IFR => "IFR",
            FlightCategory::LIFR => "LIFR",
        }
    }
}

/// A cloud layer from a METAR observation.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct CloudLayer {
    /// Coverage abbreviation: FEW, SCT, BKN, OVC, CLR, SKC, etc.
    pub cover: String,
    /// Cloud base in feet AGL (None for CLR/SKC).
    #[serde(rename = "base")]
    pub base_ft: Option<u32>,
}

/// A decoded METAR surface observation.
#[derive(Debug, Clone)]
pub struct MetarOb {
    /// ICAO station identifier (e.g. "KTLX").
    pub station_id: String,
    /// Station name (e.g. "Oklahoma City/Tinker AFB").
    pub name: String,
    /// Latitude in decimal degrees.
    pub lat: f64,
    /// Longitude in decimal degrees.
    pub lon: f64,
    /// Station elevation in meters MSL.
    pub elev_m: Option<f64>,
    /// Temperature in degrees Celsius.
    pub temp_c: Option<f64>,
    /// Dewpoint in degrees Celsius.
    pub dewp_c: Option<f64>,
    /// Wind direction in degrees true (None if calm or variable).
    pub wind_dir: Option<u16>,
    /// Wind speed in knots.
    pub wind_speed_kt: Option<u16>,
    /// Wind gust in knots (None if no gusts).
    pub wind_gust_kt: Option<u16>,
    /// Visibility string (e.g. "10+" or "3").
    pub visibility_mi: Option<f64>,
    /// Altimeter setting in hectopascals (hPa).
    pub altimeter_hpa: Option<f64>,
    /// Flight category (VFR/MVFR/IFR/LIFR).
    pub flight_category: Option<FlightCategory>,
    /// Raw METAR observation string.
    pub raw_ob: String,
    /// Cloud layers.
    pub clouds: Vec<CloudLayer>,
    /// Present weather string (e.g. "-RA BR").
    pub wx_string: Option<String>,
    /// Observation time in ISO 8601 format.
    pub obs_time: String,
}
