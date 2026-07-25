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

/// Shortest lightning aggregation window the UI allows, in seconds.
///
/// Do not lower this below ~45 s without revisiting the zero-object warning in
/// [`fetch`]. A window this short makes the S3 query cover a single hour prefix,
/// and that prefix is empty until the hour's first granule publishes (27–30 s
/// after the boundary, measured live). The warning treats "no objects at all" as
/// a dead feed, so a minimum window below the publish latency would fire a
/// spurious "feed dead" warning at the top of every hour. The 60 s floor leaves
/// roughly 30 s of headroom.
///
/// Lives here rather than in the UI handler so `fetch` can assert on it instead
/// of restating the number in prose.
pub const GLM_MIN_TIME_WINDOW_SECS: f64 = 60.0;

/// Longest lightning aggregation window the UI allows, in seconds (30 minutes).
pub const GLM_MAX_TIME_WINDOW_SECS: f64 = 1800.0;

/// Below this many files in the window, "everything failed" is not a claim
/// worth making.
///
/// Granules land every 20 s and the poll interval is also 20 s, so a typical
/// poll has very few uncached files — often one. Without a floor, a single
/// corrupt granule arriving on a quiet tick reads as "100% failure", which is
/// how a one-off becomes a product-schema alarm.
const MIN_FILES_FOR_TOTAL_VERDICT: usize = 3;

/// Files that were expected to contribute to the current window but did not.
///
/// A granule that fails is as invisible to the user as one that never existed —
/// the map just goes blank — but it arrives through a different door than
/// [`DeadFeed`]: the S3 listing is perfectly healthy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchFailures {
    /// Every file the listing placed inside the current time window.
    ///
    /// Deliberately *not* "files downloaded this poll". Successful parses are
    /// cached and drop out of subsequent polls while failures are never cached
    /// and are retried forever, so a per-poll denominator shrinks toward the
    /// failures themselves and every persistent failure eventually reads as
    /// total. Counting the whole window keeps the ratio stable.
    pub in_window: usize,
    /// How many of those are currently failing.
    pub failed: usize,
    /// One representative error, so the report can say *why*.
    pub sample_error: String,
}

impl FetchFailures {
    /// Nothing in the window is usable, over enough files for that to mean
    /// something systematic rather than one bad granule.
    pub fn is_total(&self) -> bool {
        self.in_window >= MIN_FILES_FOR_TOTAL_VERDICT && self.failed == self.in_window
    }
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
    /// Satellites this fetch actually asked about.
    ///
    /// Required to interpret `dead_feeds`: a satellite absent from `dead_feeds`
    /// is only alive if it was *queried*. Without this, deselecting a dead
    /// satellite in the dropdown reads as the feed recovering.
    pub queried: Vec<GlmSatellite>,
    /// Files that downloaded but would not parse.
    ///
    /// Kept separate from `transport_failures` because the two mean opposite
    /// things: a file that arrives and will not parse points at the *product*
    /// (a renamed variable, a restructured schema), while a file that never
    /// arrives points at the *network*. Reporting a 503 as a schema change is
    /// its own kind of false alarm.
    pub parse_failures: Option<FetchFailures>,
    /// Files that could not be downloaded at all (connection errors, non-2xx).
    pub transport_failures: Option<FetchFailures>,
}

/// Type-erased fetch result for GLM lightning data.
pub struct GlmFetchResult(pub Result<GlmFetchOutcome, String>);
