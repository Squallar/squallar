//! GOES GLM (Geostationary Lightning Mapper) data types and fetch logic.
//!
//! Fetches Level 2 LCFA (Lightning Cluster-Filter Algorithm) flash data from
//! the public `noaa-goes19` and `noaa-goes18` AWS S3 buckets. Each NetCDF4
//! file covers ~20 seconds of data; we aggregate flashes over a configurable
//! time window (default 5 minutes).

pub mod fetch;

/// Which GOES orbital slot to fetch GLM data from.
///
/// Variants name the *slot*, not the spacecraft: NOAA rotates satellites through
/// the East/West positions (GOES-19 replaced GOES-16 as GOES-East in April 2025),
/// and the bucket must follow the operational satellite.
///
/// Deliberately not `Serialize`/`Deserialize`: nothing persists this type.
/// The only GLM setting written to `ui.json` is the satellite *selection*,
/// which round-trips through hand-rolled lowercase strings in
/// `render::handlers::glm`. Adding derives here would advertise a wire format
/// that does not exist and make variant renames look load-bearing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GlmSatellite {
    /// GOES-East (currently GOES-19), covering roughly -25°W to -105°W.
    GoesEast,
    /// GOES-West (currently GOES-18), covering roughly -105°W to 170°W.
    GoesWest,
}

impl GlmSatellite {
    /// S3 bucket name for the satellite currently operating this slot.
    pub fn bucket(self) -> &'static str {
        match self {
            // GOES-16 was decommissioned as GOES-East in April 2025; the
            // `noaa-goes16` bucket has no GLM data after 2025 day 097.
            GlmSatellite::GoesEast => "noaa-goes19",
            GlmSatellite::GoesWest => "noaa-goes18",
        }
    }

    /// Display name.
    pub fn display_name(self) -> &'static str {
        match self {
            GlmSatellite::GoesEast => "GOES-19 (East)",
            GlmSatellite::GoesWest => "GOES-18 (West)",
        }
    }
}

/// GLM detection hierarchy level.
///
/// Not serialized: the UI persists three independent `show_*` booleans, not
/// this enum. See the note on [`GlmSatellite`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
    /// Area, when the product reports one.
    ///
    /// TODO(fix/glm-cf-unpacking): the unit here is currently a lie. `flash_area`
    /// and `group_area` are stored as `short` with `_Unsigned = "true"`,
    /// `scale_factor = 152601.9` and `units = "m2"`, and nothing applies CF
    /// unpacking, so this field holds a raw packed count — not km², not even m².
    /// Any raw value above 32767 additionally wraps negative through int16.
    /// The `fix/glm-cf-unpacking` branch owns the fix; until it lands, treat the
    /// number as uncalibrated.
    ///
    /// `None` at [`GlmDataLevel::Event`]: the L2 LCFA product only carries
    /// `group_area` and `flash_area`. Individual events are single sensor
    /// pixels and have no area variable, so reporting a number there would be
    /// a fabrication.
    pub area: Option<f32>,
    /// UTC timestamp.
    pub time: chrono::NaiveDateTime,
    /// Which satellite observed this.
    pub satellite: GlmSatellite,
    /// Detection hierarchy level.
    pub level: GlmDataLevel,
}

/// A satellite whose S3 listing came back with no objects whatsoever.
///
/// Distinct from "no flashes": the files themselves are absent, so the feed is
/// gone rather than the sky being quiet. Carries the detail needed to make the
/// report actionable without the reporting code having to re-derive it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeadFeed {
    pub satellite: GlmSatellite,
    pub bucket: &'static str,
    /// The S3 prefixes that were queried and returned nothing.
    pub prefixes: Vec<String>,
}

/// What one GLM fetch produced.
pub struct GlmFetchOutcome {
    pub flashes: Vec<GlmFlash>,
    /// Enabled satellites whose listing returned zero objects this poll.
    ///
    /// Reported by the handler rather than by the fetch itself, so that the
    /// message can be edge-triggered against the previous poll instead of
    /// repeating every 20 seconds.
    pub dead_feeds: Vec<DeadFeed>,
}

/// Type-erased fetch result for GLM lightning data.
pub struct GlmFetchResult(pub Result<GlmFetchOutcome, String>);
