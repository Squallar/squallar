//! GOES GLM (Geostationary Lightning Mapper) data types and fetch logic.
//!
//! Fetches Level 2 LCFA (Lightning Cluster-Filter Algorithm) flash data from
//! the public `noaa-goes19` and `noaa-goes18` AWS S3 buckets. Each NetCDF4
//! file covers ~20 seconds of data; we aggregate flashes over a configurable
//! time window (default 5 minutes).

mod cf;
pub mod fetch;
mod h5;
#[cfg(test)]
mod tests;

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
    /// Radiant energy in joules, when the product reports one.
    ///
    /// `None` means *unknown*, and callers must not substitute a number for
    /// it. Zero is not available as a sentinel: every GLM energy variable
    /// carries `add_offset = 2.8515e-16`, so the smallest value the product
    /// can express is 2.85e-16 J and zero is out of band. It is also not
    /// harmless — `rasterize` sizes bolts by `energy.log10()`, and 0.0 clamps
    /// to the bottom of that window, drawing "unknown" as "weakest".
    ///
    /// The *column* is required: a renamed or absent `*_energy` variable is a
    /// schema change and fails the level loudly. Only a per-record
    /// `_FillValue` reaches here as `None`. See `fetch::parse_level_records`.
    pub energy: Option<f32>,
    /// Area in km², when the product reports one.
    ///
    /// The product stores this as an `_Unsigned` packed `short` with
    /// `scale_factor = 152601.9` and `units = "m2"`; `fetch` applies the CF
    /// unpacking and converts to km² using the file's own `units` attribute,
    /// so the unit named here is the unit held here.
    ///
    /// `None` at [`GlmDataLevel::Event`]: the L2 LCFA product carries only
    /// `group_area` and `flash_area`. An event is a single sensor pixel and
    /// has no area variable, so any number here would be invented. Also
    /// `None` for a per-record `_FillValue`.
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
/// Two, not three, and the difference matters at the slider minimum. Measured
/// against live granules, `in_window` is exactly `floor(window / 20) - 1` and is
/// stable tick to tick, so a 60 s window holds exactly 2 files. A floor of 3
/// would put `is_total` permanently out of reach there: a user watching live
/// convection at the 1-minute setting would get "2/2 files failed to parse"
/// with no "(product change?)" hint and the partial log line instead of the
/// escalated one — withholding the diagnostic precisely when the ratio is
/// unambiguous.
///
/// Two still does the job it was added for. The primary defence is the
/// denominator itself ([`FetchFailures::in_window`] counts the whole window,
/// not this poll's downloads), which already keeps one bad granule at 1/N; this
/// floor is belt-and-braces against a degenerate single-file window.
const MIN_FILES_FOR_TOTAL_VERDICT: usize = 2;

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

/// A hierarchy level that would not parse, inside files that otherwise did.
///
/// Third failure shape, and it needed its own: [`DeadFeed`] is "the files are
/// absent", [`FetchFailures`] is "the files are present and unusable", and this
/// is "the files are present and usable, but one *layer* inside them is not".
///
/// It cannot be folded into either. Counting it as a failed file would make
/// `is_total()` announce "all N files failed to parse" while groups and events
/// are still drawing on the map. Leaving it out — which is what the first cut
/// of the level-tolerant parse did — means a `flash_*` schema change empties
/// the Flashes layer with nothing on screen to say why, which is the exact
/// failure mode this whole area of the code exists to prevent, just scoped to
/// one layer instead of the whole overlay.
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
    /// Hierarchy levels that failed to parse in files that otherwise parsed.
    ///
    /// Deduplicated per (satellite, level): a schema change affects every
    /// granule in the window identically, so the count of files it appears in
    /// carries no information the user needs.
    pub level_failures: Vec<LevelFailure>,
    /// (satellite, level) pairs this poll actually learned something about.
    ///
    /// Required to interpret `level_failures`, exactly as `queried` is required
    /// to interpret `dead_feeds`: a pair absent from `level_failures` is only
    /// *healthy* if it was evaluated. A poll that downloaded no new granules
    /// evaluates nothing, and calling that a recovery would be a false
    /// statement about the layer the user is most likely investigating.
    pub evaluated_levels: Vec<(GlmSatellite, GlmDataLevel)>,
}

/// Type-erased fetch result for GLM lightning data.
pub struct GlmFetchResult(pub Result<GlmFetchOutcome, String>);
