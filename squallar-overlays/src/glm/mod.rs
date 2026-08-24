//! GOES GLM (Geostationary Lightning Mapper) data types and fetch logic.
//!
//! Level 2 LCFA (Lightning Cluster-Filter Algorithm) data from the public GOES
//! S3 buckets declared in [`squallar_source::origins::DataSources`]. Each NetCDF4
//! granule covers ~20 s; flashes are aggregated over a configurable window
//! (default 5 minutes).

use squallar_source::origins::DataSources;

pub mod fetch;
#[cfg(test)]
mod tests;

/// Variants name the *slot*, not the spacecraft: NOAA rotates satellites through
/// the East/West positions (GOES-19 replaced GOES-16 as GOES-East in April 2025).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GlmSatellite {
    /// GOES-East (currently GOES-19), covering roughly -25°W to -105°W.
    GoesEast,
    /// GOES-West (currently GOES-18), covering roughly -105°W to 170°W.
    GoesWest,
}

impl GlmSatellite {
    /// S3 bucket for the satellite currently operating this slot, resolved from
    /// the origin table rather than hardcoded: the Android network-security and
    /// web service-worker validations read [`DataSources`], so a bucket named
    /// anywhere else is invisible to both.
    pub fn bucket(self, sources: &DataSources) -> &str {
        match self {
            GlmSatellite::GoesEast => &sources.goes_east_bucket,
            GlmSatellite::GoesWest => &sources.goes_west_bucket,
        }
    }

    pub fn display_name(self) -> &'static str {
        match self {
            GlmSatellite::GoesEast => "GOES-19 (East)",
            GlmSatellite::GoesWest => "GOES-18 (West)",
        }
    }
}

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

#[derive(Debug, Clone)]
pub struct GlmFlash {
    pub lat: f64,
    pub lon: f64,
    /// `None` means unknown; do not substitute 0.0. Every GLM energy variable
    /// carries `add_offset = 2.8515e-16`, so zero is out of band, and `rasterize`
    /// sizes bolts by `energy.log10()`.
    pub energy: Option<f32>,
    /// Stored as an `_Unsigned` packed `short` with `scale_factor = 152601.9` and
    /// `units = "m2"`. `None` at [`GlmDataLevel::Event`]: the L2 LCFA product has
    /// only `group_area` and `flash_area`.
    pub area: Option<f32>,
    pub time: chrono::NaiveDateTime,
    pub satellite: GlmSatellite,
    pub level: GlmDataLevel,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeadFeed {
    pub satellite: GlmSatellite,
    pub bucket: String,
    pub prefixes: Vec<String>,
}

/// A satellite whose listing answered with objects, none of which is a granule
/// covering the requested window.
///
/// [`DeadFeed`] is "the prefix is empty"; this is "the prefix is full and none of
/// it is ours". Never a quiet sky: GLM publishes a granule every 20 s whether or
/// not anything flashed. Measured over 24 hour-prefixes on both live buckets:
/// 4313 granules, inter-granule gap 20.0 s in 4285 of 4289 cases and 40.0 s in
/// the other four, **never above 40.0 s**, against a 60 s minimum window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowGap {
    pub satellite: GlmSatellite,
    pub objects_seen: usize,
}

/// Records a granule delivered that the parser refused to place, against the
/// number it looked at.
///
/// `considered` is **records**, carried rather than inferred because every other
/// count in a GLM round is granules or feeds. Measured over 105 granules across
/// both satellites: **0 drops in 1584507 records**, and 0 in 76003 on a holdout.
/// Reported because a non-zero is a disagreement between the product and this
/// reader — `normalize_longitude` deleted 60 of 3228 events per granule with
/// nothing on screen moving.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct RecordDrops {
    pub considered: usize,
    /// Dropped for a `_FillValue` in latitude, longitude or time.
    pub fill_values: usize,
    /// Dropped for a coordinate not on the globe after unwrapping: a product
    /// change or an unpacking mismatch, never a weather condition.
    pub off_globe: usize,
}

impl RecordDrops {
    pub fn dropped(&self) -> usize {
        self.fill_values + self.off_globe
    }

    pub(crate) fn absorb(&mut self, other: RecordDrops) {
        self.considered += other.considered;
        self.fill_values += other.fill_values;
        self.off_globe += other.off_globe;
    }
}

/// Shortest lightning aggregation window the UI allows, in seconds.
///
/// Must stay above the S3 publish latency of the hour's first granule (27–30 s
/// after the boundary, measured live): a shorter window queries a single hour
/// prefix, and the zero-object check would read that as a dead feed hourly.
pub const GLM_MIN_TIME_WINDOW_SECS: f64 = 60.0;

pub const GLM_MAX_TIME_WINDOW_SECS: f64 = 1800.0;

/// Below this many files in the window, "everything failed" is not a claim
/// worth making.
///
/// Two, not three: measured against live granules `in_window` is exactly
/// `floor(window / 20) - 1`, so the 60 s slider minimum holds exactly 2 files.
const MIN_FILES_FOR_TOTAL_VERDICT: usize = 2;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchFailures {
    /// Every file the listing placed inside the current time window.
    ///
    /// Deliberately *not* "files downloaded this poll": successful parses are
    /// cached and drop out of later polls while failures are retried forever, so
    /// a per-poll denominator makes every persistent failure read as total.
    pub in_window: usize,
    pub failed: usize,
    pub sample_error: String,
}

impl FetchFailures {
    /// Nothing in the window is usable, over enough files to mean something
    /// systematic rather than one bad granule.
    pub fn is_total(&self) -> bool {
        self.in_window >= MIN_FILES_FOR_TOTAL_VERDICT && self.failed == self.in_window
    }
}

/// A hierarchy level that would not parse, inside files that otherwise did:
/// [`DeadFeed`] is "files absent", [`FetchFailures`] is "files present and
/// unusable", this is "files usable, one *layer* is not".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LevelFailure {
    pub satellite: GlmSatellite,
    pub level: GlmDataLevel,
    pub sample_error: String,
}

pub struct GlmFetchOutcome {
    pub flashes: Vec<GlmFlash>,
    /// Enabled satellites whose listing answered with objects but placed no
    /// granule in the window. See [`WindowGap`].
    pub window_gaps: Vec<WindowGap>,
    pub record_drops: RecordDrops,
    /// Enabled satellites whose listing returned zero objects this poll.
    ///
    /// Reported rather than logged so the handler can edge-trigger the message
    /// against the previous poll instead of repeating it every 20 s.
    pub dead_feeds: Vec<DeadFeed>,
    /// Satellites this fetch actually asked about.
    ///
    /// Required to interpret `dead_feeds`: absence from `dead_feeds` only means
    /// alive if the satellite was queried.
    pub queried: Vec<GlmSatellite>,
    pub parse_failures: Option<FetchFailures>,
    /// Files that could not be downloaded at all (connection errors, non-2xx)
    /// — suspect the network. Never merged with `parse_failures`.
    pub transport_failures: Option<FetchFailures>,
    /// Hierarchy levels that failed to parse in files that otherwise parsed,
    /// deduplicated per (satellite, level).
    pub level_failures: Vec<LevelFailure>,
    /// (satellite, level) pairs this poll learned something about — required to
    /// interpret `level_failures` as `queried` is for `dead_feeds`.
    pub evaluated_levels: Vec<(GlmSatellite, GlmDataLevel)>,
    /// Satellites whose S3 listing never answered this poll — the complement of
    /// `queried`.
    ///
    /// A round that lists one satellite and fails the other returns `Ok`, because
    /// the survivor's flashes are real — but then stamped a fresh clock with
    /// nothing saying half the sky had stopped arriving.
    pub listing_failures: Vec<(GlmSatellite, crate::fetch_policy::FetchError)>,
}

/// The error is a [`FetchError`](crate::fetch_policy::FetchError) rather than a
/// `String` so the round's verdict survives the trip to the handler; as a
/// `String` every GLM failure recorded as `Transient`, which at a 20 s interval
/// is 180 attempts an hour.
pub struct GlmFetchResult(pub Result<GlmFetchOutcome, crate::fetch_policy::FetchError>);

/// [`Assembled`]: two satellite listings and a request per granule, and
/// [`GlmFetchOutcome`] carries five separate lists of what did not come back.
///
/// Declared next to those lists because this is the layer that proves a
/// declaration made anywhere else does not hold: wiring `listing_failures` alone
/// left the other four silent through a green suite.
///
/// [`Assembled`]: crate::fetch_policy::Assembled
impl crate::fetch_policy::FetchRound for GlmFetchResult {
    type Shape = crate::fetch_policy::Assembled;
}
