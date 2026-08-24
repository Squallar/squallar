//! METAR surface observation data types.

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
pub enum FlightCategory {
    VFR,
    MVFR,
    IFR,
    LIFR,
}

impl FlightCategory {
    pub fn color_rgba(self) -> [u8; 4] {
        match self {
            FlightCategory::VFR => [0, 180, 0, 220],    // green
            FlightCategory::MVFR => [0, 100, 255, 220], // blue
            FlightCategory::IFR => [220, 40, 40, 220],  // red
            FlightCategory::LIFR => [180, 0, 180, 220], // magenta
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

/// Horizontal visibility, statute miles, with the "or more" bound preserved.
///
/// An unrestricted report is a *lower bound*: `10+` for a US `10SM`, `6+` for an
/// ICAO `9999` (10 km ≈ 6.21 SM). A numeric `15` (Canadian `15SM`, or km
/// converted) is an actual measurement. Both occur; do not collapse them.
/// Measured on a live AWC cache, bounds are 3,569 of 4,549 non-empty values
/// (78.5%), so an `f64` discards exactly the good-visibility reports.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Visibility {
    pub miles: f64,
    pub or_greater: bool,
}

impl Visibility {
    pub fn parse(s: &str) -> Option<Self> {
        let s = s.trim();
        let (digits, or_greater) = match s.strip_suffix('+') {
            Some(rest) => (rest, true),
            None => (s, false),
        };
        let miles: f64 = digits.parse().ok()?;
        // `"inf"` and `"-1"` both parse as f64 but are not visibilities.
        if !miles.is_finite() || miles < 0.0 {
            return None;
        }
        Some(Visibility { miles, or_greater })
    }

    /// `"10+"`, `"6+"`, `"15"`, `"2.5"`; the `+` is never dropped.
    pub fn label(self) -> String {
        let n = if self.miles.fract() == 0.0 {
            format!("{:.0}", self.miles)
        } else {
            format!("{:.1}", self.miles)
        };
        if self.or_greater { format!("{n}+") } else { n }
    }
}

/// A numeric wind-direction column cannot express this: `0` means calm *and*
/// variable, and a genuine northerly is `360`, never `0`. Measured on a live AWC
/// cache, 1,249 of 4,933 rows (25.3%) carried `wind_dir_degrees=0` — a barb
/// pointed due north for every one. [`crate::metar::fetch`] reads the raw METAR
/// text instead, which states the case outright.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindDir {
    /// No wind, therefore no direction (`00000KT`).
    Calm,
    /// A real wind whose direction is not steady (`VRBnnKT`).
    Variable,
    Degrees(u16),
}

impl WindDir {
    pub fn label(self) -> String {
        match self {
            WindDir::Calm => "CALM".to_string(),
            WindDir::Variable => "VRB".to_string(),
            WindDir::Degrees(d) => format!("{d:03}°"),
        }
    }

    /// `None` for calm or variable. Callers must not substitute a default:
    /// `unwrap_or(0)` draws a due-north barb for a quarter of the feed.
    pub fn bearing(self) -> Option<u16> {
        match self {
            WindDir::Calm | WindDir::Variable => None,
            WindDir::Degrees(d) => Some(d),
        }
    }
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct CloudLayer {
    pub cover: String,
    /// Feet AGL. `None` for CLR/SKC.
    #[serde(rename = "base")]
    pub base_ft: Option<u32>,
}

#[derive(Debug, Clone)]
pub struct MetarOb {
    pub station_id: String,
    pub name: String,
    pub lat: f64,
    pub lon: f64,
    pub elev_m: Option<f64>,
    pub temp_c: Option<f64>,
    pub dewp_c: Option<f64>,
    pub wind_dir: Option<WindDir>,
    pub wind_speed_kt: Option<u16>,
    pub wind_gust_kt: Option<u16>,
    pub visibility: Option<Visibility>,
    /// Altimeter setting, hectopascals. A cockpit setting, reduced to sea
    /// level through the *standard* atmosphere — not this station's own air.
    /// It is not mean sea level pressure and it is not station pressure; at
    /// KLXV (3026 m) the three read 1028.5, 1013.3 and 960.3 hPa.
    pub altimeter_hpa: Option<f64>,
    /// Mean sea level pressure, hectopascals, reduced by the reporting station
    /// with its own temperature. This is the quantity the station model's
    /// pressure code carries. `None` on most of the network: the feed published
    /// it for 572 of 1324 records across 20 state ASOS networks, and the gap
    /// cannot be filled by deriving one.
    pub mslp_hpa: Option<f64>,
    pub flight_category: Option<FlightCategory>,
    pub raw_ob: String,
    pub clouds: Vec<CloudLayer>,
    pub wx_string: Option<String>,
    pub obs_time: String,
}
