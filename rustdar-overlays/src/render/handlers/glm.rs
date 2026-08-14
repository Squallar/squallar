use std::any::Any;
use std::sync::Arc;

use chrono::Utc;
use rustdar_units::UserPreferences;

use crate::fetch_policy::Assembled;
use crate::glm::fetch::GlmCache;
use crate::glm::{
    DeadFeed, FetchFailures, GLM_MAX_TIME_WINDOW_SECS, GLM_MIN_TIME_WINDOW_SECS, GlmDataLevel,
    GlmFetchOutcome, GlmFetchResult, GlmFlash, GlmSatellite, LevelFailure, RecordDrops, WindowGap,
};
use crate::render::controls::{
    ControlButton, ControlEffect, ControlItem, ControlUpdate, ControlValue, PaneControlContext,
    PaneControlContextMut,
};
use crate::render::overlay_state::{
    ClickableItem, FetchConfig, FetchPayload, FetchTask, OverlayHandler, OverlayItem, OverlayKind,
    OverlayState, PopupContent, PopupSection, RasterizeContext, RasterizeFn, RenderMode,
};
use crate::render::rasterize;
use crate::types::GeoBounds;

/// What a poll's listings covered, in the layer-agnostic terms the UI renders.
///
/// GLM's round is one S3 listing per selected satellite and a dead one does not
/// fail the round — the survivor's flashes are real and cover most of CONUS. But
/// nothing said so: the round returned `Ok`, stamped a fresh clock and read
/// health `Ok`, while GOES-East's contribution drained out of the cache window
/// over the next half hour. This is that half of the round, written down.
///
/// # A listing is one of six ways this round under-delivers
///
/// The first draft of this counted [`listing_failures`] alone, and the others
/// each end a poll with flashes missing from the map and the ledger reading
/// whole:
///
/// - a [`DeadFeed`] — the listing answered `200` with **zero objects**, which
///   for a product that publishes a granule every twenty seconds is the feed
///   being gone rather than a quiet sky. It is in `queried`, so counting only
///   `listing_failures` counted it as a feed that delivered;
/// - a [`WindowGap`] — the listing answered `200` with objects, none of which
///   is a granule covering the window. The opposite listing to a dead feed and
///   the same result on screen: that satellite contributes nothing. Measured
///   over 24 hour-prefixes on both live buckets the granule cadence is 20.0 s
///   and never gapped past 40.0 s, against a 60 s minimum window, so a healthy
///   feed cannot produce this;
/// - [`transport_failures`](GlmFetchOutcome::transport_failures) — the granules
///   were listed and would not download. Its own log line already said what
///   this costs: *"the map is blank despite a healthy S3 listing"*;
/// - [`parse_failures`](GlmFetchOutcome::parse_failures) — they downloaded and
///   would not parse;
/// - [`level_failures`](GlmFetchOutcome::level_failures) — one hierarchy level
///   stopped parsing inside granules that otherwise did, so that layer alone is
///   empty;
/// - [`record_drops`](GlmFetchOutcome::record_drops) — the granules parsed and
///   individual records inside them were thrown away for a fill value or a
///   coordinate off the globe. The finest grain of the four, and the only one
///   whose denominator is **records** rather than granules or feeds.
///
/// Each was reported to the log and nowhere else. Measured over a socket with
/// both listings healthy and every granule refused: two feeds queried, zero
/// flashes, `is_complete()` = `true`.
///
/// # What a quiet sky looks like, and why it is not any of these
///
/// GLM has genuinely quiet periods and reporting one as incomplete would be a
/// false alarm that teaches people to ignore the mark. It cannot happen here,
/// because none of the six conditions is reachable by an absence of lightning:
/// a quiet sky lists its granules like any other (they are published on a
/// 20 s clock, not on activity), downloads them, parses them, and yields zero
/// flashes with `considered` records all kept. Every branch below needs a
/// *granule or a record* to have gone missing, which no weather can cause.
///
/// # Which bucket each falls in
///
/// A feed is [`missing`](DataCompleteness::missing) when nothing of it reached
/// the map and [`partial`](DataCompleteness::partial) when some of its window
/// did — the same rule the zone resolver uses per alert. A listing failure and
/// a dead feed are missing outright. Granule failures are counted against the
/// feeds that *did* list: total means those feeds delivered nothing, so they
/// are missing too; anything short of total leaves them drawing part of their
/// window.
///
/// `parts` are **granules**, the piece a feed is assembled from, which is the
/// denominator the granule failures are already measured against. The listing
/// half has no second denominator — a satellite *is* the unit there.
fn round_coverage(outcome: &GlmFetchOutcome) -> crate::fetch_policy::DataCompleteness {
    let GlmFetchOutcome {
        queried,
        dead_feeds,
        window_gaps,
        record_drops,
        listing_failures,
        transport_failures,
        parse_failures,
        level_failures,
        ..
    } = outcome;

    // Transport and parse partition the same granules — a file that failed to
    // download was never parsed — so their counts add and share `in_window`.
    let in_window = transport_failures
        .as_ref()
        .or(parse_failures.as_ref())
        .map_or(0, |f| f.in_window);
    let granules_failed = transport_failures.as_ref().map_or(0, |f| f.failed)
        + parse_failures.as_ref().map_or(0, |f| f.failed);

    // Feeds that put granules in the window. A dead feed listed no objects and
    // a window gap listed no *granules*, so both are already accounted for as
    // missing and neither can be charged again for the granule failures — they
    // contributed nothing to `in_window` to fail.
    let live = queried
        .len()
        .saturating_sub(dead_feeds.len())
        .saturating_sub(window_gaps.len());
    let (granule_partial, granule_missing) = if granules_failed == 0 {
        (0, 0)
    } else if granules_failed >= in_window {
        (0, live)
    } else {
        (live, 0)
    };
    // A level that stopped parsing empties that layer and leaves the others,
    // and a dropped record leaves a hole in a layer that is otherwise drawing:
    // either way a live feed is showing part of its window — unless it is
    // already counted as having delivered nothing, which is the stronger claim.
    let under_delivering = !level_failures.is_empty() || record_drops.dropped() > 0;
    let granule_partial = if granule_partial == 0 && granule_missing == 0 && under_delivering {
        live
    } else {
        granule_partial
    };

    let mut reasons: Vec<(String, usize)> = listing_failures
        .iter()
        .map(|(sat, e)| (format!("{}: {e}", sat.display_name()), 1))
        .collect();
    for feed in dead_feeds {
        reasons.push((
            format!(
                "{}: listing returned no objects",
                feed.satellite.display_name()
            ),
            1,
        ));
    }
    // Deliberately says the listing was *healthy*: the whole point of splitting
    // this from `dead_feeds` is that the operator's next move differs. An empty
    // bucket means look at the bucket name; a full bucket with nothing in the
    // window means look at the publisher or at our filename parsing.
    for gap in window_gaps {
        reasons.push((
            format!(
                "{}: listing healthy ({} objects) but no granule covers the window",
                gap.satellite.display_name(),
                gap.objects_seen,
            ),
            1,
        ));
    }
    if let Some(f) = transport_failures {
        reasons.push((
            format!("granule download failed ({})", f.sample_error),
            f.failed,
        ));
    }
    if let Some(f) = parse_failures {
        reasons.push((
            format!("granule would not parse ({})", f.sample_error),
            f.failed,
        ));
    }
    for failure in level_failures {
        reasons.push((
            format!(
                "{} {} level stopped parsing",
                failure.satellite.display_name(),
                failure.level.display_name(),
            ),
            1,
        ));
    }
    // Count 1 with the whole phrase in the string, like the listing reasons
    // above and unlike the granule ones: `status_note` renders a reason as
    // "{count} {why}", so the record denominator has to travel inside `why` or
    // it would be read against `parts_requested`, which counts granules. The
    // two causes stay apart because they indict different things - a fill value
    // is the product declining to place a record, an off-globe coordinate is
    // the product and this reader disagreeing about what the numbers mean.
    if record_drops.fill_values > 0 {
        reasons.push((
            format!(
                "{} of {} records dropped for fill values in position or time",
                record_drops.fill_values, record_drops.considered,
            ),
            1,
        ));
    }
    if record_drops.off_globe > 0 {
        reasons.push((
            format!(
                "{} of {} records dropped for coordinates off the globe (product change?)",
                record_drops.off_globe, record_drops.considered,
            ),
            1,
        ));
    }

    crate::fetch_policy::DataCompleteness {
        expected: queried.len() + listing_failures.len(),
        partial: granule_partial,
        missing: listing_failures.len() + dead_feeds.len() + window_gaps.len() + granule_missing,
        parts_requested: in_window,
        parts_resolved: in_window.saturating_sub(granules_failed),
        unit: "satellite feeds",
        part_unit: "granules",
        reasons,
    }
}

/// Clickable item representing a single GLM lightning flash.
#[derive(Debug)]
pub(crate) struct GlmFlashItem {
    pub flash: GlmFlash,
    pub index: usize,
}

impl OverlayItem for GlmFlashItem {
    fn kind(&self) -> OverlayKind {
        OverlayKind::Lightning
    }

    fn popup_content(&self, prefs: &UserPreferences) -> PopupContent {
        let f = &self.flash;
        let time_str = match prefs.timezone {
            rustdar_units::TimezonePreference::Utc => f.time.format("%H:%M:%S UTC").to_string(),
            rustdar_units::TimezonePreference::Local => {
                let utc_dt = chrono::TimeZone::from_utc_datetime(&chrono::Utc, &f.time);
                let local_dt = utc_dt.with_timezone(&chrono::Local);
                local_dt.format("%H:%M:%S %Z").to_string()
            }
        };

        let title = match f.level {
            GlmDataLevel::Event => "GLM Lightning Event",
            GlmDataLevel::Group => "GLM Lightning Group",
            GlmDataLevel::Flash => "GLM Lightning Flash",
        };

        let mut grid = vec![
            ("Type".into(), f.level.display_name().into()),
            ("Latitude".into(), format!("{:.4}°", f.lat)),
            ("Longitude".into(), format!("{:.4}°", f.lon)),
        ];
        // Omitted when the product did not report them: an absent row says
        // "not reported", a "0.0 km²" row would claim a measurement.
        if let Some(energy) = f.energy {
            grid.push(("Energy".into(), format!("{energy:.2e} J")));
        }
        if let Some(area) = f.area {
            grid.push(("Area".into(), format!("{area:.1} km²")));
        }

        let sections = vec![
            PopupSection::Text(format!("{time_str} - {}", f.satellite.display_name())),
            PopupSection::KeyValueGrid(grid),
        ];

        PopupContent {
            title: title.into(),
            accent_rgb: [255, 220, 50],
            width: 300.0,
            sections,
            actions: Vec::new(),
        }
    }

    fn matches(&self, other: &dyn OverlayItem) -> bool {
        other
            .as_any()
            .downcast_ref::<GlmFlashItem>()
            .is_some_and(|o| o.index == self.index)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Which satellites to query for GLM data.
///
/// Persisted as the lowercase string from [`Self::as_str`], which is also the
/// dropdown's option id. No serde derives: they would define a second,
/// incompatible encoding (`"East"`, not `"east"`) that nothing reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SatelliteSelection {
    East,
    West,
    Both,
}

impl SatelliteSelection {
    fn to_satellites(self) -> Vec<GlmSatellite> {
        match self {
            SatelliteSelection::East => vec![GlmSatellite::GoesEast],
            SatelliteSelection::West => vec![GlmSatellite::GoesWest],
            SatelliteSelection::Both => vec![GlmSatellite::GoesEast, GlmSatellite::GoesWest],
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            SatelliteSelection::East => "east",
            SatelliteSelection::West => "west",
            SatelliteSelection::Both => "both",
        }
    }

    fn from_str(s: &str) -> Self {
        match s {
            "east" => SatelliteSelection::East,
            "west" => SatelliteSelection::West,
            _ => SatelliteSelection::Both,
        }
    }
}

const SECS_PER_MIN: f64 = 60.0;

pub(crate) struct GlmHandler {
    pub state: OverlayState<Vec<Arc<GlmFlashItem>>, Assembled>,
    pub enabled: bool,
    pub satellite: SatelliteSelection,
    /// Time window in seconds, clamped to
    /// [`GLM_MIN_TIME_WINDOW_SECS`]..=[`GLM_MAX_TIME_WINDOW_SECS`].
    pub time_window_secs: f64,
    /// Which data hierarchy levels to include.
    pub show_events: bool,
    pub show_groups: bool,
    pub show_flashes: bool,
    /// Cached S3 file data for incremental fetching.
    pub cache: Arc<std::sync::Mutex<GlmCache>>,
    /// Satellites whose last listing returned no objects at all.
    ///
    /// Kept across polls so the log fires on the transition rather than every
    /// poll, and so the panel can show the condition.
    dead_feeds: Vec<DeadFeed>,
    /// Per-kind failure state — see [`GlmHandler::report_failures`].
    parse: FailureState,
    transport: FailureState,
    /// Hierarchy levels that stopped parsing while the files themselves are
    /// fine. Kept across polls for the same reason as `dead_feeds`.
    level_failures: Vec<LevelFailure>,
    /// Satellites whose last listing was healthy but placed no granule in the
    /// window. Kept across polls for the same reason as `dead_feeds`.
    window_gaps: Vec<WindowGap>,
    /// Records the last poll that actually parsed a granule threw away.
    ///
    /// Carried rather than read from each round because a poll that downloads
    /// nothing new parses nothing and so counts nothing — routine, with a 20 s
    /// poll interval racing a ~20 s granule cadence. Taking the round's value
    /// unconditionally would blank this notice on every other poll. The gate is
    /// `considered > 0`, which is the same "did we look at anything" question
    /// `evaluated_levels` answers for the level failures.
    record_drops: RecordDrops,
}

/// What a batch of failures means, reduced to the states worth *announcing*.
///
/// Carries no counts: edge-triggering on raw counts flaps (7 files then 9 is
/// not a change a user needs told twice).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum FailureHealth {
    #[default]
    Ok,
    /// Some files failed; the map still shows the rest.
    Partial,
    /// Nothing in the window is usable, over enough files for that to be a
    /// systematic cause rather than one bad granule.
    Total,
}

/// Edge-trigger state plus the detail the panel renders, for one failure kind.
#[derive(Default)]
struct FailureState {
    health: FailureHealth,
    detail: Option<FetchFailures>,
}

/// Which door a failure came through. Never merged: opposite causes, opposite
/// actions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FailureKind {
    /// Files arrived and would not parse — suspect the product.
    Parse,
    /// Files never arrived — suspect the network.
    Transport,
}

impl FailureKind {
    fn noun(self) -> &'static str {
        match self {
            FailureKind::Parse => "failed to parse",
            FailureKind::Transport => "could not be downloaded",
        }
    }

    /// What a *total* failure of this kind most likely means.
    fn total_hint(self) -> &'static str {
        match self {
            FailureKind::Parse => "product change?",
            FailureKind::Transport => "network down?",
        }
    }
}

impl GlmHandler {
    pub fn new() -> Self {
        Self {
            state: OverlayState::new(),
            enabled: false,
            satellite: SatelliteSelection::Both,
            time_window_secs: 300.0,
            show_events: false,
            show_groups: true,
            show_flashes: true,
            cache: Arc::new(std::sync::Mutex::new(GlmCache::default())),
            dead_feeds: Vec::new(),
            parse: FailureState::default(),
            transport: FailureState::default(),
            level_failures: Vec::new(),
            window_gaps: Vec::new(),
            record_drops: RecordDrops::default(),
        }
    }

    /// Log window-gap *changes* only, and keep the current set for the panel.
    ///
    /// Same shape as [`report_feed_changes`](Self::report_feed_changes) and for
    /// the same reasons: the condition is steady while it lasts, a 20 s poll
    /// interval would otherwise emit ~180 identical lines an hour per
    /// satellite, and `queried` is what entitles the recovery notice — a
    /// deselected satellite must not read as having come back.
    ///
    /// Not folded into `report_feed_changes` despite the shape: a dead feed and
    /// a window gap are opposite listings (`0` objects against `objects_seen`
    /// of them) and want opposite investigations, and merging the two states
    /// would let one condition clear the other's notice.
    fn report_window_gaps(&mut self, queried: &[GlmSatellite], current: Vec<WindowGap>) {
        for gap in &current {
            if !self.window_gaps.iter().any(|g| g.satellite == gap.satellite) {
                log::warn!(
                    "GLM: {} published no granule covering the window - its S3 listing is \
                     healthy ({} objects under the queried prefixes), so the files are not \
                     absent, they are not being named or timed the way this reader parses \
                     them, or publishing has stalled for longer than the window. Not a \
                     quiet sky: a quiet sky still publishes a granule every 20 s.",
                    gap.satellite.display_name(),
                    gap.objects_seen,
                );
            }
        }

        let mut next: Vec<WindowGap> = Vec::new();
        for previous in std::mem::take(&mut self.window_gaps) {
            let still_gapped = current.iter().any(|g| g.satellite == previous.satellite);
            if still_gapped {
                continue;
            }
            if queried.contains(&previous.satellite) {
                log::info!(
                    "GLM: {} is publishing granules in the window again - recovered",
                    previous.satellite.display_name(),
                );
            } else {
                next.push(previous);
            }
        }
        next.extend(current);
        self.window_gaps = next;
    }

    /// Keep the newest drop tally that came from a poll which actually looked.
    ///
    /// A poll that parsed no granule has `considered == 0` and knows nothing —
    /// it must not clear a standing notice, exactly as an unevaluated level
    /// must not read as recovered.
    fn report_record_drops(&mut self, drops: RecordDrops) {
        if drops.considered == 0 {
            return;
        }
        if drops.dropped() > 0 && self.record_drops.dropped() == 0 {
            log::warn!(
                "GLM: {} of {} records in the granules just parsed were dropped before \
                 reaching the map ({} for a fill value in position or time, {} for a \
                 coordinate off the globe). The granules themselves parsed, so this is \
                 not a file failure - it is this reader and the product disagreeing \
                 about individual records.",
                drops.dropped(),
                drops.considered,
                drops.fill_values,
                drops.off_globe,
            );
        } else if drops.dropped() == 0 && self.record_drops.dropped() > 0 {
            log::info!("GLM: every record in the granules just parsed reached the map - recovered");
        }
        self.record_drops = drops;
    }

    /// Log level-parse *changes* only, and keep the current set for the panel.
    ///
    /// Edge-triggered: a schema change is permanent, so an unconditional
    /// warning emits ~180 identical lines an hour and buries itself.
    ///
    /// `evaluated` is what entitles us to say "recovered", on both axes this
    /// failure is keyed on. Deselecting a satellite or a level means nothing
    /// asks about it any more; and a poll that downloads no new granules
    /// evaluates nothing at all — routine, with a 20 s poll interval racing a
    /// ~20 s granule cadence.
    fn report_level_failures(
        &mut self,
        evaluated: &[(GlmSatellite, GlmDataLevel)],
        current: Vec<LevelFailure>,
    ) {
        for failure in &current {
            let known = self
                .level_failures
                .iter()
                .any(|f| f.satellite == failure.satellite && f.level == failure.level);
            if !known {
                log::warn!(
                    "GLM: the {} layer from {} stopped parsing while the granules \
                     themselves are fine - that layer is now empty on the map, the \
                     others are unaffected. First error: {}",
                    failure.level.display_name(),
                    failure.satellite.display_name(),
                    failure.sample_error,
                );
            }
        }

        // Carry forward anything we did not look at, so it neither reads as
        // recovered now nor re-fires the "stopped parsing" warning later.
        let mut next: Vec<LevelFailure> = Vec::new();
        for previous in std::mem::take(&mut self.level_failures) {
            let still_failing = current
                .iter()
                .any(|f| f.satellite == previous.satellite && f.level == previous.level);
            if still_failing {
                continue;
            }
            let looked = evaluated
                .iter()
                .any(|&(s, l)| s == previous.satellite && l == previous.level);
            if looked {
                log::info!(
                    "GLM: the {} layer from {} is parsing again - recovered",
                    previous.level.display_name(),
                    previous.satellite.display_name(),
                );
            } else {
                next.push(previous);
            }
        }
        next.extend(current);
        self.level_failures = next;
    }

    /// Log feed-liveness *changes* only: one warning when a feed goes dark, one
    /// recovery notice when it comes back. At a 20 s poll interval an
    /// unconditional warning is ~180 identical lines an hour per satellite.
    ///
    /// `queried` is what entitles us to say "recovered": absence from `current`
    /// means "alive" only for a satellite that was actually asked.
    fn report_feed_changes(&mut self, queried: &[GlmSatellite], current: Vec<DeadFeed>) {
        for feed in &current {
            if !self
                .dead_feeds
                .iter()
                .any(|d| d.satellite == feed.satellite)
            {
                log::warn!(
                    "GLM: {} feed is dead - bucket '{}' returned no objects at all under \
                     prefixes [{}]. Not a quiet sky: the files themselves are absent \
                     (satellite rotated out of this slot?).",
                    feed.satellite.display_name(),
                    feed.bucket,
                    feed.prefixes.join(", "),
                );
            }
        }

        // Carry forward any satellite we did not ask about. This keeps a
        // deselected-while-dead feed from reading as recovered, and keeps
        // re-selecting it from re-firing the "is dead" warning.
        let mut next: Vec<DeadFeed> = Vec::new();
        for previous in std::mem::take(&mut self.dead_feeds) {
            let still_dead = current.iter().any(|d| d.satellite == previous.satellite);
            if still_dead {
                continue;
            }
            if queried.contains(&previous.satellite) {
                log::info!(
                    "GLM: {} feed recovered - bucket '{}' is returning objects again",
                    previous.satellite.display_name(),
                    previous.bucket,
                );
            } else {
                next.push(previous);
            }
        }
        next.extend(current);
        self.dead_feeds = next;
    }

    /// Announce failure *transitions* for one kind, and keep the detail for the
    /// panel.
    ///
    /// A granule that downloads but will not parse blanks the map exactly like
    /// one that was never published, but its S3 listing is healthy so
    /// `dead_feeds` says nothing about it — without this the user sees
    /// "Updated 0s ago" over an empty map.
    fn report_failures(&mut self, kind: FailureKind, failures: Option<FetchFailures>) {
        let health = match &failures {
            None => FailureHealth::Ok,
            Some(f) if f.is_total() => FailureHealth::Total,
            Some(_) => FailureHealth::Partial,
        };

        let state = match kind {
            FailureKind::Parse => &mut self.parse,
            FailureKind::Transport => &mut self.transport,
        };

        if health != state.health {
            match (&failures, health) {
                (Some(f), FailureHealth::Total) => log::warn!(
                    "GLM: all {} files in the window {} ({}) - the map is blank despite a \
                     healthy S3 listing. First error: {}",
                    f.in_window,
                    kind.noun(),
                    kind.total_hint(),
                    f.sample_error,
                ),
                (Some(f), FailureHealth::Partial) => log::warn!(
                    "GLM: {}/{} files in the window {}. First error: {}",
                    f.failed,
                    f.in_window,
                    kind.noun(),
                    f.sample_error,
                ),
                (_, FailureHealth::Ok) => {
                    log::info!(
                        "GLM: files {} again - recovered",
                        match kind {
                            FailureKind::Parse => "are parsing",
                            FailureKind::Transport => "are downloading",
                        }
                    );
                }
                (None, _) => {}
            }
            state.health = health;
        }

        state.detail = failures;
    }

    /// Build the list of active data levels from the checkbox flags.
    fn active_levels(&self) -> Vec<GlmDataLevel> {
        let mut levels = Vec::new();
        if self.show_events {
            levels.push(GlmDataLevel::Event);
        }
        if self.show_groups {
            levels.push(GlmDataLevel::Group);
        }
        if self.show_flashes {
            levels.push(GlmDataLevel::Flash);
        }
        levels
    }

    /// Clear the file cache (needed when level selection changes).
    fn clear_cache(&self) {
        let mut guard = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        *guard = GlmCache::default();
    }
}

impl OverlayHandler for GlmHandler {
    fn kind(&self) -> OverlayKind {
        OverlayKind::Lightning
    }

    fn display_name(&self) -> &str {
        "GLM Lightning"
    }

    fn render_mode(&self) -> RenderMode {
        RenderMode::Texture
    }

    fn is_enabled(&self) -> bool {
        self.enabled
    }

    fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
    }

    /// E.g. `"312 flashes · 10 min"`: what the layer is holding, and how wide
    /// its window is.
    ///
    /// It is **not** the number of bolts on screen, and this comment used to
    /// say it was. Three culls run after this count is taken, inside
    /// `rasterize_glm_strikes`: the viewport bounding box, a second age test
    /// against a clock sampled later than the fetch's, and a pixel-window test
    /// after projection. For a full-disk product on a regional view the first
    /// of those alone removes most of what this number counts, so reading it
    /// as a drawn count let an under-drawing map read as a complete one.
    ///
    /// Kept as the held count rather than swapped for a drawn one: the held
    /// count is a property of the data and answers "is the feed alive", which
    /// is what a status line is for, while a drawn count changes on every pan
    /// and would need a `RasterizeOutput` field every other overlay would then
    /// have to carry. The claim was wrong, not the number.
    fn status_line(&self) -> Option<String> {
        if !self.enabled {
            return None;
        }
        Some(format!(
            "{} flashes - {:.0} min",
            self.state.data.len(),
            self.time_window_secs / SECS_PER_MIN,
        ))
    }

    fn data_generation(&self) -> u64 {
        self.state.data_generation
    }

    fn has_data(&self) -> bool {
        !self.state.data.is_empty()
    }

    fn is_fetching(&self) -> bool {
        self.state.fetching
    }

    fn set_fetching(&mut self, fetching: bool) {
        self.state.fetching = fetching;
    }

    fn retry(&self) -> Option<&crate::fetch_policy::FetchRetry> {
        Some(&self.state.retry)
    }

    fn retry_mut(&mut self) -> Option<&mut crate::fetch_policy::FetchRetry> {
        Some(&mut self.state.retry)
    }

    fn fetch_time(&self) -> Option<web_time::Instant> {
        self.state.fetch_time
    }

    fn item_count(&self) -> usize {
        self.state.data.len()
    }

    fn auto_poll_interval(&self) -> Option<u64> {
        Some(20)
    }

    fn clickable_items(&self) -> Vec<ClickableItem<'_>> {
        // Lightning uses hit-buffer click detection, not polygon containment.
        Vec::new()
    }

    fn apply_fetch_result(&mut self, result: FetchPayload) {
        let Some(fetch) = self.state.downcast_round::<GlmFetchResult>(result) else {
            log::error!("GLM handler received unexpected fetch result type");
            return;
        };
        match fetch.0 {
            Ok(outcome) => {
                log::info!("Received {} GLM lightning flashes", outcome.flashes.len());
                // Before the reports below, which take the outcome's failure
                // lists by value: every one of them is a way this round
                // under-delivered, and the coverage report needs all four.
                let coverage = round_coverage(&outcome);
                self.report_feed_changes(&outcome.queried, outcome.dead_feeds);
                self.report_window_gaps(&outcome.queried, outcome.window_gaps);
                self.report_record_drops(outcome.record_drops);
                self.report_failures(FailureKind::Parse, outcome.parse_failures);
                self.report_failures(FailureKind::Transport, outcome.transport_failures);
                self.report_level_failures(&outcome.evaluated_levels, outcome.level_failures);
                let items = outcome
                    .flashes
                    .into_iter()
                    .enumerate()
                    .map(|(i, flash)| Arc::new(GlmFlashItem { flash, index: i }))
                    .collect();
                // The satellite that answered gives real flashes on a fresh
                // clock; the one that did not is recorded beside them rather
                // than instead of them.
                self.state.set_data_with_coverage(items, coverage);
            }
            Err(e) => {
                // A failed fetch says nothing about feed liveness, so leave the
                // previous verdict standing rather than reporting a recovery.
                log::error!("GLM fetch failed: {e}");
                // The verdict travels with the error now. It used to be
                // hardcoded `transient` here, reasoned as "the outer error
                // means the round did not complete, which is transient by
                // construction" — and that was wrong in the direction that
                // costs something. At a 20 s interval, a GLM bucket renamed a
                // year ago is 180 requests an hour for ever, and no rung of the
                // ladder can slow a `Transient` past the ceiling. The S3
                // listing classifies its own statuses; see `glm::fetch`.
                self.state.record_failure(&e);
            }
        }
    }

    fn retain_selections(&self, selections: &mut Vec<Arc<dyn OverlayItem>>) {
        let count = self.state.data.len();
        selections.retain(|sel| {
            if sel.kind() != OverlayKind::Lightning {
                return true;
            }
            sel.as_any()
                .downcast_ref::<GlmFlashItem>()
                .is_some_and(|f| f.index < count)
        });
    }

    fn prepare_rasterize(&self, ctx: &RasterizeContext) -> Option<RasterizeFn> {
        if self.state.data.is_empty() {
            return None;
        }
        let flashes: Vec<GlmFlash> = self.state.data.iter().map(|i| i.flash.clone()).collect();
        let items: Vec<Arc<dyn OverlayItem>> = self
            .state
            .data
            .iter()
            .map(|i| i.clone() as Arc<dyn OverlayItem>)
            .collect();
        let zoom = ctx.zoom;
        let is_dark = ctx.is_dark;
        let device_scale = ctx.device_scale;
        let time_window_secs = self.time_window_secs;
        let now = Utc::now().naive_utc();
        Some(Box::new(move |bounds: &GeoBounds, width, height| {
            rasterize::rasterize_glm_strikes(
                &flashes,
                &items,
                bounds,
                width,
                height,
                &rasterize::GlmRenderParams {
                    device_scale,
                    zoom,
                    is_dark,
                    time_window_secs,
                    now,
                },
            )
        }))
    }

    fn create_fetch_tasks(&self, ctx: &FetchConfig) -> Vec<FetchTask> {
        log::info!("Fetching GLM lightning data");
        let client = ctx.client.clone();
        let sources = ctx.sources.clone();
        let satellites = self.satellite.to_satellites();
        let time_window_secs = self.time_window_secs;
        let levels = self.active_levels();
        let cache = Arc::clone(&self.cache);
        vec![FetchTask {
            kind: OverlayKind::Lightning,
            future: Box::pin(async move {
                // Clone the cache out so we don't hold a std::sync::Mutex across await
                let mut local_cache = {
                    let guard = cache.lock().unwrap_or_else(|e| e.into_inner());
                    guard.clone()
                };
                let result = crate::glm::fetch::fetch_glm_flashes(
                    &client,
                    &sources,
                    &satellites,
                    time_window_secs,
                    &levels,
                    &mut local_cache,
                )
                .await;
                // Write the updated cache back
                {
                    let mut guard = cache.lock().unwrap_or_else(|e| e.into_inner());
                    *guard = local_cache;
                }
                Box::new(GlmFetchResult(result)) as FetchPayload
            }),
        }]
    }

    fn controls(&self, _ctx: &PaneControlContext<'_>) -> Vec<ControlItem> {
        let count = self.state.data.len();
        let label = if count == 0 {
            "GLM Lightning".to_string()
        } else {
            format!("GLM Lightning ({count})")
        };

        let mut items = vec![ControlItem::Toggle {
            id: "enabled",
            label,
            enabled: self.enabled,
        }];

        // Ungated on enabled (the every-option rule, M9.1): a hidden
        // layer's options stay visible and editable - edits take effect
        // when the eye shows it again - Refresh still fetches (nothing
        // on the fetch path reads enabled), and the status lines keep
        // reporting.
        items.push(ControlItem::Dropdown {
            id: "satellite",
            label: "Satellite".into(),
            options: vec![
                ("east".into(), "GOES-19 (East)".into()),
                ("west".into(), "GOES-18 (West)".into()),
                ("both".into(), "Both".into()),
            ],
            selected: self.satellite.as_str().into(),
        });

        // Time window slider, in minutes. The minimum is load-bearing —
        // see GLM_MIN_TIME_WINDOW_SECS before lowering it.
        let mins = self.time_window_secs / SECS_PER_MIN;
        items.push(ControlItem::Slider {
            id: "time_window",
            label: "Time Window".into(),
            min: GLM_MIN_TIME_WINDOW_SECS / SECS_PER_MIN,
            max: GLM_MAX_TIME_WINDOW_SECS / SECS_PER_MIN,
            value: mins,
            logarithmic: true,
            format: "{:.0} min".into(),
        });

        items.push(ControlItem::Toggle {
            id: "show_events",
            label: "Events (highest density)".to_string(),
            enabled: self.show_events,
        });
        items.push(ControlItem::Toggle {
            id: "show_groups",
            label: "Groups (medium density)".to_string(),
            enabled: self.show_groups,
        });
        items.push(ControlItem::Toggle {
            id: "show_flashes",
            label: "Flashes (lowest density)".to_string(),
            enabled: self.show_flashes,
        });

        items.push(ControlItem::ButtonRow {
            buttons: vec![ControlButton {
                id: "refresh",
                label: "\u{21bb} Refresh".into(),
                enabled: !self.state.fetching,
                highlight: false,
            }],
        });

        if self.state.fetching {
            items.push(ControlItem::InfoText {
                text: "Fetching...".into(),
            });
        }
        if let Some(t) = self.state.fetch_time {
            let secs = t.elapsed().as_secs();
            let text = if secs < 60 {
                format!("Updated {secs}s ago")
            } else {
                format!("Updated {}m ago", secs / 60)
            };
            items.push(ControlItem::InfoText { text });
        }

        // An empty map with no explanation has five causes that look
        // identical on screen. Logs alone did not get one of them noticed
        // for a year, so each is stated where the toggle lives.
        //
        // A sixth thing an empty map can mean is that nothing flashed, and
        // that one deliberately shows nothing at all: every notice below
        // needs a granule or a record to have gone missing, and no quiet
        // sky can produce one. A mark that fired on a calm night is a mark
        // people stop reading.

        // Cause 1: the files were never published. Only satellites the
        // current selection queries — `dead_feeds` remembers deselected
        // ones so they do not read as recovered, but showing those is
        // stale.
        let selected = self.satellite.to_satellites();
        for feed in self
            .dead_feeds
            .iter()
            .filter(|f| selected.contains(&f.satellite))
        {
            items.push(ControlItem::InfoText {
                text: format!(
                    "! No data from {}: bucket '{}' is empty",
                    feed.satellite.display_name(),
                    feed.bucket,
                ),
            });
        }

        // Cause 2: the bucket is full and none of it covers the window.
        // The opposite listing to cause 1 and the same empty map, so it gets
        // its own line rather than sharing one: "bucket is empty" would send
        // the reader to the bucket name, which is the one thing that is fine.
        for gap in self
            .window_gaps
            .iter()
            .filter(|g| selected.contains(&g.satellite))
        {
            items.push(ControlItem::InfoText {
                text: format!(
                    "! No granules from {} cover the window (listing is healthy)",
                    gap.satellite.display_name(),
                ),
            });
        }

        // Cause 3: the files were published but would not download or would
        // not parse. The S3 listing is healthy in both cases, and the two
        // are reported separately because they indict different things.
        //
        // Both can show at once, but two *totals* cannot: each file yields
        // exactly one FileError, so the counts partition the failures and
        // at most one can equal `in_window`.
        for (kind, state) in [
            (FailureKind::Parse, &self.parse),
            (FailureKind::Transport, &self.transport),
        ] {
            let Some(f) = &state.detail else { continue };
            let text = if f.is_total() {
                format!(
                    "! All {} files in the window {} ({})",
                    f.in_window,
                    kind.noun(),
                    kind.total_hint(),
                )
            } else {
                format!("! {}/{} files {}", f.failed, f.in_window, kind.noun(),)
            };
            items.push(ControlItem::InfoText { text });
        }

        // Cause 4: the files are fine and one *layer* inside them is not.
        // The granule parsed, so it is not a failed file, and the listing
        // is healthy, so it is not a dead feed.
        //
        // Filtered on *both* selection dimensions: `level_failures`
        // remembers verdicts it could not re-examine, and `clear_cache()`
        // on a level toggle guarantees a deselected layer is never
        // re-evaluated, so nothing else would ever take the notice down.
        let selected = self.satellite.to_satellites();
        let levels = self.active_levels();
        for failure in self
            .level_failures
            .iter()
            .filter(|f| selected.contains(&f.satellite) && levels.contains(&f.level))
        {
            items.push(ControlItem::InfoText {
                text: format!(
                    "! {} unavailable from {} (product change?)",
                    failure.level.display_name(),
                    failure.satellite.display_name(),
                ),
            });
        }

        // Cause 5: the granules parsed and individual records inside them did
        // not survive. The finest grain, and the only notice here whose
        // denominator is records — stated inline so it is never read against
        // the file counts standing above it.
        //
        // Not filtered by satellite or level, unlike causes 1, 2 and 4: the
        // tally is summed across whatever the last parsing poll was asked for,
        // and inventing a per-bird split it does not have would be a figure
        // with no denominator behind it.
        if self.record_drops.dropped() > 0 {
            items.push(ControlItem::InfoText {
                text: format!(
                    "! {} of {} records dropped before drawing ({} fill values, \
                     {} off-globe coordinates)",
                    self.record_drops.dropped(),
                    self.record_drops.considered,
                    self.record_drops.fill_values,
                    self.record_drops.off_globe,
                ),
            });
        }

        items
    }

    fn apply_control(
        &mut self,
        update: &ControlUpdate,
        _ctx: &mut PaneControlContextMut<'_>,
    ) -> ControlEffect {
        match update.id {
            "enabled" => {
                if let ControlValue::Bool(val) = update.value {
                    self.enabled = val;
                    if val && self.state.enable_should_refetch(self.has_data()) {
                        return ControlEffect::Fetch;
                    }
                }
                ControlEffect::None
            }
            "satellite" => {
                if let ControlValue::String(ref val) = update.value {
                    let new_sat = SatelliteSelection::from_str(val);
                    if new_sat != self.satellite {
                        self.satellite = new_sat;
                        self.state.data_generation = self.state.data_generation.wrapping_add(1);
                        // Deliberately no `clear_cache()`, unlike the level
                        // toggles below: cached records carry their satellite
                        // and `glm::fetch::flashes_in_window` filters by the
                        // current selection, so a deselected bird stops
                        // rendering on the very next poll and re-selecting it
                        // restores instantly from cache. Levels must clear
                        // because a deselected level was never parsed into the
                        // cache at all.
                        return ControlEffect::Fetch;
                    }
                }
                ControlEffect::None
            }
            "time_window" => {
                if let ControlValue::Float(mins) = update.value {
                    self.time_window_secs = (mins * SECS_PER_MIN)
                        .clamp(GLM_MIN_TIME_WINDOW_SECS, GLM_MAX_TIME_WINDOW_SECS);
                    self.state.data_generation = self.state.data_generation.wrapping_add(1);
                    return ControlEffect::Fetch;
                }
                ControlEffect::None
            }
            "show_events" => {
                if let ControlValue::Bool(val) = update.value {
                    self.show_events = val;
                    self.state.data_generation = self.state.data_generation.wrapping_add(1);
                    self.clear_cache();
                    return ControlEffect::Fetch;
                }
                ControlEffect::None
            }
            "show_groups" => {
                if let ControlValue::Bool(val) = update.value {
                    self.show_groups = val;
                    self.state.data_generation = self.state.data_generation.wrapping_add(1);
                    self.clear_cache();
                    return ControlEffect::Fetch;
                }
                ControlEffect::None
            }
            "show_flashes" => {
                if let ControlValue::Bool(val) = update.value {
                    self.show_flashes = val;
                    self.state.data_generation = self.state.data_generation.wrapping_add(1);
                    self.clear_cache();
                    return ControlEffect::Fetch;
                }
                ControlEffect::None
            }
            "refresh" => ControlEffect::Fetch,
            _ => ControlEffect::None,
        }
    }

    fn serialize_state(&self) -> serde_json::Value {
        serde_json::json!({
            "enabled": self.enabled,
            "satellite": self.satellite.as_str(),
            "time_window_secs": self.time_window_secs,
            "show_events": self.show_events,
            "show_groups": self.show_groups,
            "show_flashes": self.show_flashes,
        })
    }

    fn deserialize_state(&mut self, value: serde_json::Value) {
        if let Some(enabled) = value.get("enabled").and_then(|v| v.as_bool()) {
            self.enabled = enabled;
        }
        if let Some(sat) = value.get("satellite").and_then(|v| v.as_str()) {
            self.satellite = SatelliteSelection::from_str(sat);
        }
        if let Some(tw) = value.get("time_window_secs").and_then(|v| v.as_f64()) {
            self.time_window_secs = tw.clamp(GLM_MIN_TIME_WINDOW_SECS, GLM_MAX_TIME_WINDOW_SECS);
        }
        if let Some(v) = value.get("show_events").and_then(|v| v.as_bool()) {
            self.show_events = v;
        }
        if let Some(v) = value.get("show_groups").and_then(|v| v.as_bool()) {
            self.show_groups = v;
        }
        if let Some(v) = value.get("show_flashes").and_then(|v| v.as_bool()) {
            self.show_flashes = v;
        }
    }
}

#[cfg(test)]
mod tests;
