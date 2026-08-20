use crate::render::overlay_state::{PaneMut, PaneRef};
use std::any::Any;
use std::sync::Arc;

use rustdar_units::UserPreferences;

use crate::fetch_policy::Assembled;
use crate::glm::fetch::GlmCache;
use crate::glm::{
    DeadFeed, FetchFailures, GLM_MAX_TIME_WINDOW_SECS, GLM_MIN_TIME_WINDOW_SECS, GlmDataLevel,
    GlmFetchOutcome, GlmFetchResult, GlmFlash, GlmSatellite, LevelFailure, RecordDrops, WindowGap,
};
use crate::render::controls::{
    ControlButton, ControlEffect, ControlItem, ControlUpdate, ControlValue,
};
use crate::render::overlay_state::Surface;
use crate::render::overlay_state::{
    ClickableItem, FetchConfig, FetchPayload, FetchTask, OverlayHandler, OverlayItem, OverlayState,
    PopupContent, PopupSection, RasterizeContext, RenderMode,
};
use crate::render::rasterize;
use rustdar_source::id::{LayerId, known};
use rustdar_source::job::{DescribedJob, JobCodec};

/// What a poll's listings covered, in the layer-agnostic terms the UI renders.
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

    let in_window = transport_failures
        .as_ref()
        .or(parse_failures.as_ref())
        .map_or(0, |f| f.in_window);
    let granules_failed = transport_failures.as_ref().map_or(0, |f| f.failed)
        + parse_failures.as_ref().map_or(0, |f| f.failed);

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
    fn layer_id(&self) -> LayerId {
        known::LIGHTNING
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
    record_drops: RecordDrops,
}

/// What a batch of failures means, reduced to the states worth *announcing*.
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

    fn paint_input(&self, ctx: &RasterizeContext) -> Option<rasterize::GlmStrikesInput> {
        if self.state.data.is_empty() {
            return None;
        }
        Some(rasterize::GlmStrikesInput {
            flashes: self
                .state
                .data
                .iter()
                .map(|i| rasterize::FlashPaint {
                    lat: i.flash.lat,
                    lon: i.flash.lon,
                    time: i.flash.time,
                    energy: i.flash.energy,
                })
                .collect(),
            zoom: ctx.zoom,
            is_dark: ctx.is_dark,
            time_window_secs: self.time_window_secs,
            now: ctx.now,
            device_scale: ctx.device_scale,
        })
    }

    /// Log window-gap *changes* only, and keep the current set for the panel.
    fn report_window_gaps(&mut self, queried: &[GlmSatellite], current: Vec<WindowGap>) {
        for gap in &current {
            if !self
                .window_gaps
                .iter()
                .any(|g| g.satellite == gap.satellite)
            {
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
    fn id(&self) -> LayerId {
        known::LIGHTNING
    }
    fn surface(&self) -> Surface {
        Surface::Ground
    }
    fn draw_order_weight(&self) -> u32 {
        70
    }

    fn display_name(&self) -> &str {
        "GLM Lightning"
    }

    fn render_mode(&self) -> RenderMode {
        RenderMode::Texture
    }

    /// `is_dark` rides into the described job (`GlmInput`) and decides the
    /// flash outline and the alpha the age decay fades to, so a cached raster
    /// is a raster in one theme.
    fn theme_sensitive(&self) -> bool {
        true
    }

    fn is_enabled(&self) -> bool {
        self.enabled
    }

    fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
    }

    /// E.g. `"312 flashes · 10 min"`: what the layer is holding, and how wide
    /// its window is.
    fn status_line(&self, _pane: &PaneRef<'_>) -> Option<String> {
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
                self.state.set_data_with_coverage(items, coverage);
            }
            Err(e) => {
                // A failed fetch says nothing about feed liveness, so leave the
                // previous verdict standing rather than reporting a recovery.
                log::error!("GLM fetch failed: {e}");
                self.state.record_failure(&e);
            }
        }
    }

    fn retain_selections(&self, selections: &mut Vec<Arc<dyn OverlayItem>>) {
        let count = self.state.data.len();
        selections.retain(|sel| {
            if sel.layer_id() != known::LIGHTNING {
                return true;
            }
            sel.as_any()
                .downcast_ref::<GlmFlashItem>()
                .is_some_and(|f| f.index < count)
        });
    }

    fn prepare_job(&self, ctx: &RasterizeContext, _pane: &PaneRef<'_>) -> Option<DescribedJob> {
        // Captures the dispatch's own `ctx.now`, which is what keeps the
        // flash ages a worker renders the ages this page computed.
        Some(DescribedJob::new(self.paint_input(ctx)?))
    }

    fn job_codec(&self) -> Option<&'static JobCodec> {
        crate::render::jobs::JOB_CODECS
            .iter()
            .find(|row| row.label == "overlay/glm")
    }

    /// Index-aligned with [`Self::paint_input`]'s rows: both iterate
    /// `state.data` in order, so `hit_items()[i]` **is** the item whose flash
    /// travelled at row `i` — the invariant
    /// [`rasterize::HitMap::from_cells`] zips on.
    fn hit_items(&self) -> Option<Vec<Arc<dyn OverlayItem>>> {
        if self.state.data.is_empty() {
            return None;
        }
        Some(
            self.state
                .data
                .iter()
                .map(|i| i.clone() as Arc<dyn OverlayItem>)
                .collect(),
        )
    }

    fn create_fetch_tasks(&self, ctx: &FetchConfig, _pane: &PaneRef<'_>) -> Vec<FetchTask> {
        log::info!("Fetching GLM lightning data");
        let client = ctx.client.clone();
        let sources = ctx.sources.clone();
        let satellites = self.satellite.to_satellites();
        let time_window_secs = self.time_window_secs;
        let levels = self.active_levels();
        let cache = Arc::clone(&self.cache);
        vec![FetchTask {
            kind: known::LIGHTNING,
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
                {
                    let mut guard = cache.lock().unwrap_or_else(|e| e.into_inner());
                    *guard = local_cache;
                }
                Box::new(GlmFetchResult(result)) as FetchPayload
            }),
        }]
    }

    fn controls(&self, _ctx: &PaneRef<'_>) -> Vec<ControlItem> {
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

    fn apply_control(&mut self, update: &ControlUpdate, _ctx: &mut PaneMut<'_>) -> ControlEffect {
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
