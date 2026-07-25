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

/// Reported horizontal visibility.
///
/// AWC's `visibility_statute_mi` column is *not* a plain number: an unrestricted
/// report is written with a trailing `+` — `10+` for a US `10SM`, `6+` for an
/// ICAO `9999` (10 km, ≈6.21 SM). Those two spellings are the **majority** of
/// the feed (measured on a live cache: 3,569 of 4,549 non-empty values, 78.5%),
/// so a bare `f64` cannot represent the column and parsing into one silently
/// discards exactly the good-visibility reports.
///
/// [`or_greater`](Self::or_greater) keeps that distinction: `10+` is a *lower
/// bound*, whereas a numeric `15` (Canadian `15SM`, or a metric report converted
/// from km) is an actual measurement. Both occur — do not collapse them.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Visibility {
    /// Distance in statute miles. For an `or_greater` report this is the
    /// reported floor, not the true visibility.
    pub miles: f64,
    /// True when the report was "this far or better" (`10+`, `6+`).
    pub or_greater: bool,
}

impl Visibility {
    /// Parse one `visibility_statute_mi` cell, honouring the trailing `+`.
    ///
    /// Returns `None` for empty, negative, non-finite, or unparseable values.
    pub fn parse(s: &str) -> Option<Self> {
        let s = s.trim();
        let (digits, or_greater) = match s.strip_suffix('+') {
            Some(rest) => (rest, true),
            None => (s, false),
        };
        let miles: f64 = digits.parse().ok()?;
        // Reject inf/NaN and negatives: `"inf"` and `"-1"` both parse as f64 but
        // are not visibilities, and would poison the comparisons downstream.
        if !miles.is_finite() || miles < 0.0 {
            return None;
        }
        Some(Visibility { miles, or_greater })
    }

    /// Compact display form: `"10+"`, `"6+"`, `"15"`, `"2.5"`.
    ///
    /// Whole numbers lose the decimal so the station-model plot stays legible at
    /// small font sizes; the `+` is never dropped, since losing it is the bug
    /// this type exists to prevent.
    pub fn label(self) -> String {
        let n = if self.miles.fract() == 0.0 {
            format!("{:.0}", self.miles)
        } else {
            format!("{:.1}", self.miles)
        };
        if self.or_greater { format!("{n}+") } else { n }
    }
}

/// Reported wind direction.
///
/// AWC writes `wind_dir_degrees=0` for **both** calm and variable winds and
/// never leaves it empty for either, so the column alone cannot separate "no
/// wind" from "direction changing" from a genuine northerly — which AWC reports
/// as `360`, never `0`. On a live cache 1,249 of 4,933 rows (25.3%) carried
/// `wind_dir_degrees=0`; treating those as a bearing points a wind barb due
/// north for every one of them.
///
/// The raw METAR text in the same row states the case explicitly (`00000KT`
/// versus `VRBnnKT`), so [`crate::metar::fetch`] reads it from there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindDir {
    /// Calm: no wind and therefore no direction (`00000KT`).
    Calm,
    /// Variable: a real wind whose direction is not steady (`VRBnnKT`).
    Variable,
    /// A definite bearing in degrees true, 1–360.
    Degrees(u16),
}

impl WindDir {
    /// Short display form: `"CALM"`, `"VRB"`, `"180°"`.
    pub fn label(self) -> String {
        match self {
            WindDir::Calm => "CALM".to_string(),
            WindDir::Variable => "VRB".to_string(),
            WindDir::Degrees(d) => format!("{d:03}°"),
        }
    }

    /// The bearing to point a wind barb along, or `None` when there is no
    /// direction to point (calm or variable).
    ///
    /// Callers must not substitute a default: `unwrap_or(0)` here is exactly the
    /// bug that drew a due-north barb for a quarter of the feed.
    pub fn bearing(self) -> Option<u16> {
        match self {
            WindDir::Calm | WindDir::Variable => None,
            WindDir::Degrees(d) => Some(d),
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
    /// Wind direction: calm, variable, or a bearing. `None` when the report
    /// carries no wind data at all.
    pub wind_dir: Option<WindDir>,
    /// Wind speed in knots.
    pub wind_speed_kt: Option<u16>,
    /// Wind gust in knots (None if no gusts).
    pub wind_gust_kt: Option<u16>,
    /// Horizontal visibility, preserving AWC's "or greater" marker.
    pub visibility: Option<Visibility>,
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
