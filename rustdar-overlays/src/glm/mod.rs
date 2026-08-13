//! GOES GLM (Geostationary Lightning Mapper) data types and fetch logic.
//!
//! Level 2 LCFA (Lightning Cluster-Filter Algorithm) data from the public GOES
//! S3 buckets declared in [`rustdar_radar::sources::DataSources`]. Each NetCDF4
//! granule covers ~20 s; flashes are aggregated over a configurable window
//! (default 5 minutes).

use rustdar_radar::sources::DataSources;

mod cf;
pub mod fetch;
mod h5;
#[cfg(test)]
mod tests;

/// Which GOES orbital slot to fetch GLM data from.
///
/// Variants name the *slot*, not the spacecraft: NOAA rotates satellites through
/// the East/West positions (GOES-19 replaced GOES-16 as GOES-East in April 2025).
///
/// Not `Serialize`/`Deserialize`: nothing persists this type. `ui.json` stores
/// only the satellite *selection*, as lowercase strings owned by
/// `render::handlers::glm`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GlmSatellite {
    /// GOES-East (currently GOES-19), covering roughly -25°W to -105°W.
    GoesEast,
    /// GOES-West (currently GOES-18), covering roughly -105°W to 170°W.
    GoesWest,
}

impl GlmSatellite {
    /// S3 bucket for the satellite currently operating this slot, resolved
    /// from the origin table rather than hardcoded here: the two derived
    /// validations (the Android network-security-config test, the web
    /// service-worker never-cache test) read [`DataSources`], so a bucket
    /// named anywhere else is invisible to both — which is how `noaa-goes16`
    /// outlived GOES-16's rotation out of the East slot.
    pub fn bucket(self, sources: &DataSources) -> &str {
        match self {
            GlmSatellite::GoesEast => &sources.goes_east_bucket,
            GlmSatellite::GoesWest => &sources.goes_west_bucket,
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
/// Not serialized: the UI persists three independent `show_*` booleans.
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
    /// Radiant energy in joules, when the product reports one.
    ///
    /// `None` means unknown; do not substitute 0.0. Every GLM energy variable
    /// carries `add_offset = 2.8515e-16`, so zero is out of band, and
    /// `rasterize` sizes bolts by `energy.log10()` — 0.0 draws "unknown" as
    /// "weakest".
    ///
    /// The *column* is required and an absent one fails the level; only a
    /// per-record `_FillValue` reaches here as `None`.
    pub energy: Option<f32>,
    /// Area in km², when the product reports one.
    ///
    /// Stored as an `_Unsigned` packed `short` with `scale_factor = 152601.9`
    /// and `units = "m2"`; `fetch` unpacks and converts to km².
    ///
    /// `None` at [`GlmDataLevel::Event`]: the L2 LCFA product has only
    /// `group_area` and `flash_area`. Also `None` for a per-record `_FillValue`.
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
/// Distinct from "no flashes": the files themselves are absent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeadFeed {
    pub satellite: GlmSatellite,
    /// Owned, not `&'static`: the bucket is resolved from [`DataSources`],
    /// which a test can point at a mock.
    pub bucket: String,
    /// The S3 prefixes that were queried and returned nothing.
    pub prefixes: Vec<String>,
}

/// Shortest lightning aggregation window the UI allows, in seconds.
///
/// Must stay above the S3 publish latency of the hour's first granule (27–30 s
/// after the boundary, measured live). A shorter window queries a single hour
/// prefix, which is empty until then, and [`fetch`]'s zero-object check would
/// read that as a dead feed once an hour. Do not lower below ~45 s.
pub const GLM_MIN_TIME_WINDOW_SECS: f64 = 60.0;

/// Longest lightning aggregation window the UI allows, in seconds (30 minutes).
pub const GLM_MAX_TIME_WINDOW_SECS: f64 = 1800.0;

/// Below this many files in the window, "everything failed" is not a claim
/// worth making.
///
/// Two, not three: measured against live granules `in_window` is exactly
/// `floor(window / 20) - 1`, so the 60 s slider minimum holds exactly 2 files
/// and a floor of 3 would put `is_total` permanently out of reach there.
const MIN_FILES_FOR_TOTAL_VERDICT: usize = 2;

/// Files that were expected to contribute to the current window but did not.
///
/// Unlike [`DeadFeed`], the S3 listing is healthy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchFailures {
    /// Every file the listing placed inside the current time window.
    ///
    /// Deliberately *not* "files downloaded this poll": successful parses are
    /// cached and drop out of later polls while failures are never cached and
    /// are retried forever, so a per-poll denominator makes every persistent
    /// failure eventually read as total.
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

/// A hierarchy level that would not parse, inside files that otherwise did.
///
/// The third failure shape: [`DeadFeed`] is "files absent", [`FetchFailures`]
/// is "files present and unusable", this is "files usable, one *layer* is not".
/// Counting it as a failed file would make `is_total()` announce "all N files
/// failed to parse" while the other layers are still drawing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LevelFailure {
    pub satellite: GlmSatellite,
    pub level: GlmDataLevel,
    /// Representative error for this level, for the log.
    pub sample_error: String,
}

/// What one GLM fetch produced.
pub struct GlmFetchOutcome {
    pub flashes: Vec<GlmFlash>,
    /// Enabled satellites whose listing returned zero objects this poll.
    ///
    /// Reported rather than logged here so the handler can edge-trigger the
    /// message against the previous poll instead of repeating it every 20 s.
    pub dead_feeds: Vec<DeadFeed>,
    /// Satellites this fetch actually asked about.
    ///
    /// Required to interpret `dead_feeds`: absence from `dead_feeds` only means
    /// alive if the satellite was queried. Without this, deselecting a dead
    /// satellite reads as the feed recovering.
    pub queried: Vec<GlmSatellite>,
    /// Files that downloaded but would not parse — suspect the product.
    pub parse_failures: Option<FetchFailures>,
    /// Files that could not be downloaded at all (connection errors, non-2xx)
    /// — suspect the network. Never merged with `parse_failures`.
    pub transport_failures: Option<FetchFailures>,
    /// Hierarchy levels that failed to parse in files that otherwise parsed.
    ///
    /// Deduplicated per (satellite, level): a schema change hits every granule
    /// in the window identically.
    pub level_failures: Vec<LevelFailure>,
    /// (satellite, level) pairs this poll actually learned something about.
    ///
    /// Required to interpret `level_failures` as `queried` is for `dead_feeds`.
    /// A poll that downloaded no new granules evaluates nothing, and must not
    /// read as a recovery.
    pub evaluated_levels: Vec<(GlmSatellite, GlmDataLevel)>,
    /// Satellites whose S3 listing never answered this poll — the complement of
    /// `queried`, and the thing this outcome had no way to express.
    ///
    /// A round that lists one satellite and fails the other returns `Ok`,
    /// deliberately, because the survivor's flashes are real. It then went
    /// through `set_data`, stamped a fresh clock and reported health `Ok`, with
    /// nothing anywhere saying half the sky had stopped arriving: GOES-East
    /// dies, its flashes drain out of the cache window over the next half hour,
    /// and most of CONUS quietly loses its lightning under an `Updated 0s ago`
    /// line. Carried here so the handler can file it as
    /// [`DataCompleteness`](crate::fetch_policy::DataCompleteness) beside the
    /// data that did arrive.
    pub listing_failures: Vec<(GlmSatellite, crate::fetch_policy::FetchError)>,
}

/// Type-erased fetch result for GLM lightning data.
///
/// The error is a [`FetchError`](crate::fetch_policy::FetchError) rather than a
/// `String` so the round's verdict survives the trip to the handler. It used to
/// be a `String`, and the handler had nothing left to classify it by, so it
/// recorded every GLM failure as `Transient` — which at a 20 s interval is 180
/// attempts an hour against a bucket that may have been renamed a year ago.
pub struct GlmFetchResult(pub Result<GlmFetchOutcome, crate::fetch_policy::FetchError>);
