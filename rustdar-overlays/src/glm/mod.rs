//! GOES GLM (Geostationary Lightning Mapper) data types and fetch logic.
//!
//! Fetches Level 2 LCFA (Lightning Cluster-Filter Algorithm) flash data from
//! the public `noaa-goes16` and `noaa-goes18` AWS S3 buckets. Each NetCDF4
//! file covers ~20 seconds of data; we aggregate flashes over a configurable
//! time window (default 5 minutes).

pub mod fetch;

/// Which GOES satellite to fetch GLM data from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum GlmSatellite {
    /// GOES-16 (GOES-East), covering roughly -25°W to -105°W.
    Goes16East,
    /// GOES-18 (GOES-West), covering roughly -105°W to 170°W.
    Goes18West,
}

impl GlmSatellite {
    /// S3 bucket name for this satellite.
    pub fn bucket(self) -> &'static str {
        match self {
            GlmSatellite::Goes16East => "noaa-goes16",
            GlmSatellite::Goes18West => "noaa-goes18",
        }
    }

    /// Display name.
    pub fn display_name(self) -> &'static str {
        match self {
            GlmSatellite::Goes16East => "GOES-16 (East)",
            GlmSatellite::Goes18West => "GOES-18 (West)",
        }
    }
}

/// GLM detection hierarchy level.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum GlmDataLevel {
    /// Individual pixel detections (~2ms resolution, highest density).
    Event,
    /// Spatially connected events within a single frame (medium density).
    Group,
    /// Temporally connected groups forming a complete flash (lowest density).
    Flash,
}

impl GlmDataLevel {
    pub fn display_name(self) -> &'static str {
        match self {
            GlmDataLevel::Event => "Events",
            GlmDataLevel::Group => "Groups",
            GlmDataLevel::Flash => "Flashes",
        }
    }
}

/// A single GLM lightning observation (event, group, or flash).
#[derive(Debug, Clone)]
pub struct GlmFlash {
    pub lat: f64,
    pub lon: f64,
    /// Radiant energy in joules.
    pub energy: f32,
    /// Area in km².
    pub area: f32,
    /// UTC timestamp.
    pub time: chrono::NaiveDateTime,
    /// Which satellite observed this.
    pub satellite: GlmSatellite,
    /// Detection hierarchy level.
    pub level: GlmDataLevel,
}

/// Type-erased fetch result for GLM lightning data.
pub struct GlmFetchResult(pub Result<Vec<GlmFlash>, String>);
