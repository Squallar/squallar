use std::any::Any;
use std::sync::Arc;

use chrono::Utc;
use rustdar_units::UserPreferences;

use crate::glm::fetch::GlmCache;
use crate::glm::{
    DeadFeed, FetchFailures, GLM_MAX_TIME_WINDOW_SECS, GLM_MIN_TIME_WINDOW_SECS, GlmDataLevel,
    GlmFetchResult, GlmFlash, GlmSatellite,
};
use crate::render::controls::{
    ControlButton, ControlEffect, ControlItem, ControlUpdate, ControlValue, PaneControlContext,
    PaneControlContextMut,
};
use crate::render::overlay_state::{
    ClickableItem, FetchConfig, FetchTask, OverlayHandler, OverlayItem, OverlayKind,
    OverlayState, PopupContent, PopupSection, RasterizeContext, RasterizeFn, RenderMode,
};
use crate::render::rasterize;
use crate::types::GeoBounds;

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
            rustdar_units::TimezonePreference::Utc => {
                f.time.format("%H:%M:%S UTC").to_string()
            }
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
            ("Energy".into(), format!("{:.2e} J", f.energy)),
        ];
        // Events have no area in the L2 LCFA product; omit the row rather than
        // showing a placeholder zero.
        //
        // TODO(fix/glm-cf-unpacking): "km²" is wrong. The product stores area as
        // an unsigned packed `short` with scale_factor 152601.9 and units "m2",
        // and no CF unpacking is applied, so this prints a raw count under a
        // km² label (and goes negative past 32767 via int16 wraparound). See the
        // note on `GlmFlash::area`; that branch fixes both this line and the
        // struct doc.
        if let Some(area) = f.area {
            grid.push(("Area".into(), format!("{area:.1} km²")));
        }

        let sections = vec![
            PopupSection::Text(format!("{time_str} — {}", f.satellite.display_name())),
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
/// The persisted form is the lowercase string produced by [`Self::as_str`] and
/// read back by [`Self::from_str`] (see `serialize_state`/`deserialize_state`
/// below) — it is also the dropdown's option id. Deriving serde here would
/// define a *second*, incompatible encoding (`"East"` rather than `"east"`)
/// that nothing reads, so the derives are deliberately absent.
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
    pub state: OverlayState<Vec<Arc<GlmFlashItem>>>,
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
    /// Kept across polls so the log message can fire on the transition into and
    /// out of the dead state rather than on every poll, and so the condition can
    /// be shown in the control panel — a user who never reads logcat is exactly
    /// the person the original year-long outage went unnoticed by.
    dead_feeds: Vec<DeadFeed>,
    /// Per-kind failure state — see [`GlmHandler::report_failures`].
    parse: FailureState,
    transport: FailureState,
}

/// What a batch of failures means, reduced to the states worth *announcing*.
///
/// Deliberately carries no counts: a batch that fails 7 files then 9 files has
/// not changed in any way a user needs told twice, and edge-triggering on raw
/// counts would flap.
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

/// Which door a failure came through. The two are never merged, because they
/// point at opposite causes and suggest opposite actions.
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
        }
    }

    /// Log feed-liveness *changes* only.
    ///
    /// At a 20-second poll interval, warning unconditionally would emit ~180
    /// identical lines per hour per satellite for as long as the outage lasts,
    /// which is cry-wolf in a quieter register: correct, and ignored. Report the
    /// edges instead — one warning when a feed goes dark, one recovery notice
    /// when it comes back.
    ///
    /// `queried` is what makes "recovered" a claim we are entitled to make.
    /// Absence from `current` only means "alive" for a satellite that was
    /// actually asked; for one the dropdown stopped selecting, it means nothing,
    /// and saying "feed recovered" would be a categorically false statement
    /// about the feed the user is most likely investigating.
    fn report_feed_changes(&mut self, queried: &[GlmSatellite], current: Vec<DeadFeed>) {
        for feed in &current {
            if !self.dead_feeds.iter().any(|d| d.satellite == feed.satellite) {
                log::warn!(
                    "GLM: {} feed is dead — bucket '{}' returned no objects at all under \
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
                    "GLM: {} feed recovered — bucket '{}' is returning objects again",
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
    /// A granule that downloads but will not parse is exactly as invisible as a
    /// granule that was never published — the map goes blank either way — but it
    /// arrives with a perfectly healthy S3 listing, so `dead_feeds` says nothing
    /// about it. Without this, a renamed variable reads to the user as "Updated
    /// 0s ago" over an empty map: the original bug, one layer up.
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
                    "GLM: all {} files in the window {} ({}) — the map is blank despite a \
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
                    log::info!("GLM: files {} again — recovered", match kind {
                        FailureKind::Parse => "are parsing",
                        FailureKind::Transport => "are downloading",
                    });
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
        if self.show_events { levels.push(GlmDataLevel::Event); }
        if self.show_groups { levels.push(GlmDataLevel::Group); }
        if self.show_flashes { levels.push(GlmDataLevel::Flash); }
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

    fn fetch_time(&self) -> Option<std::time::Instant> {
        self.state.fetch_time
    }

    fn item_count(&self) -> usize {
        self.state.data.len()
    }

    fn auto_poll_interval(&self) -> Option<u64> {
        Some(20)
    }

    fn clickable_items(&self) -> Vec<ClickableItem> {
        // Lightning uses hit-buffer click detection, not polygon containment.
        Vec::new()
    }

    fn apply_fetch_result(&mut self, result: Box<dyn Any + Send>) {
        let Some(fetch) = result.downcast::<GlmFetchResult>().ok() else {
            log::error!("GLM handler received unexpected fetch result type");
            return;
        };
        match fetch.0 {
            Ok(outcome) => {
                log::info!("Received {} GLM lightning flashes", outcome.flashes.len());
                self.report_feed_changes(&outcome.queried, outcome.dead_feeds);
                self.report_failures(FailureKind::Parse, outcome.parse_failures);
                self.report_failures(FailureKind::Transport, outcome.transport_failures);
                let items = outcome
                    .flashes
                    .into_iter()
                    .enumerate()
                    .map(|(i, flash)| Arc::new(GlmFlashItem { flash, index: i }))
                    .collect();
                self.state.set_data(items);
            }
            Err(e) => {
                // A failed fetch says nothing about feed liveness, so leave the
                // previous verdict standing rather than reporting a recovery.
                log::error!("GLM fetch failed: {e}");
            }
        }
        self.state.fetching = false;
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

    fn prepare_rasterize(
        &self,
        ctx: &RasterizeContext,
    ) -> Option<RasterizeFn> {
        if self.state.data.is_empty() {
            return None;
        }
        let flashes: Vec<GlmFlash> = self.state.data.iter().map(|i| i.flash.clone()).collect();
        let items: Vec<Arc<dyn OverlayItem>> =
            self.state.data.iter().map(|i| i.clone() as Arc<dyn OverlayItem>).collect();
        let zoom = ctx.zoom;
        let is_dark = ctx.is_dark;
        let time_window_secs = self.time_window_secs;
        let now = Utc::now().naive_utc();
        Some(Box::new(move |bounds: &GeoBounds, width, height| {
            rasterize::rasterize_glm_strikes(
                &flashes, &items, bounds, width, height,
                &rasterize::GlmRenderParams { zoom, is_dark, time_window_secs, now },
            )
        }))
    }

    fn create_fetch_tasks(&self, ctx: &FetchConfig) -> Vec<FetchTask> {
        log::info!("Fetching GLM lightning data");
        let client = ctx.client.clone();
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
                Box::new(GlmFetchResult(result)) as Box<dyn Any + Send>
            }),
        }]
    }

    fn controls(&self, _ctx: &PaneControlContext<'_>) -> Vec<ControlItem> {
        let count = self.state.data.len();
        let label = if count == 0 {
            "\u{26a1}  GLM Lightning".to_string()
        } else {
            format!("\u{26a1}  GLM Lightning ({count})")
        };

        let mut items = vec![ControlItem::Toggle {
            id: "enabled",
            label,
            enabled: self.enabled,
        }];

        if self.enabled {
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
                    label: "\u{1f504} Refresh".into(),
                    enabled: !self.state.fetching,
                    highlight: false,
                }],
            });

            if self.state.fetching {
                items.push(ControlItem::InfoText {
                    text: "Fetching\u{2026}".into(),
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

            // The whole point of detecting these is that someone notices. Logs
            // alone did not manage that for a year, so say it where the toggle
            // lives: an empty map with no explanation is the failure mode being
            // fixed, and it has two causes that look identical on screen.

            // Cause 1: the files were never published. Only mention satellites
            // the current selection actually queries — `dead_feeds` deliberately
            // remembers deselected ones so they do not read as recovered, but
            // reporting those here would be stale.
            let selected = self.satellite.to_satellites();
            for feed in self.dead_feeds.iter().filter(|f| selected.contains(&f.satellite)) {
                items.push(ControlItem::InfoText {
                    text: format!(
                        "\u{26a0} No data from {}: bucket '{}' is empty",
                        feed.satellite.display_name(),
                        feed.bucket,
                    ),
                });
            }

            // Cause 2: the files were published but never became usable — either
            // they would not download, or they would not parse. The S3 listing
            // is healthy in both cases, so nothing above catches them, and the
            // two are reported separately because they indict different things.
            for (kind, state) in
                [(FailureKind::Parse, &self.parse), (FailureKind::Transport, &self.transport)]
            {
                let Some(f) = &state.detail else { continue };
                let text = if f.is_total() {
                    format!(
                        "\u{26a0} All {} files in the window {} ({})",
                        f.in_window,
                        kind.noun(),
                        kind.total_hint(),
                    )
                } else {
                    format!(
                        "\u{26a0} {}/{} files {}",
                        f.failed,
                        f.in_window,
                        kind.noun(),
                    )
                };
                items.push(ControlItem::InfoText { text });
            }
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
                    if val && !self.has_data() && !self.state.fetching {
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
            self.time_window_secs =
                tw.clamp(GLM_MIN_TIME_WINDOW_SECS, GLM_MAX_TIME_WINDOW_SECS);
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
mod tests {
    use super::*;

    fn item(level: GlmDataLevel, area: Option<f32>) -> GlmFlashItem {
        GlmFlashItem {
            flash: GlmFlash {
                lat: 35.0,
                lon: -97.0,
                energy: 1.0e-14,
                area,
                time: chrono::NaiveDate::from_ymd_opt(2026, 7, 24)
                    .unwrap()
                    .and_hms_opt(12, 0, 0)
                    .unwrap(),
                satellite: GlmSatellite::GoesEast,
                level,
            },
            index: 0,
        }
    }

    fn grid_keys(item: &GlmFlashItem) -> Vec<String> {
        let prefs = UserPreferences {
            timezone: rustdar_units::TimezonePreference::Utc,
            ..UserPreferences::default()
        };
        item.popup_content(&prefs)
            .sections
            .into_iter()
            .filter_map(|s| match s {
                PopupSection::KeyValueGrid(rows) => Some(rows),
                _ => None,
            })
            .flatten()
            .map(|(k, _)| k)
            .collect()
    }

    #[test]
    fn event_popup_omits_area_row() {
        let keys = grid_keys(&item(GlmDataLevel::Event, None));
        assert!(
            !keys.iter().any(|k| k == "Area"),
            "events have no area in the L2 LCFA product, got rows {keys:?}"
        );
        assert!(keys.iter().any(|k| k == "Energy"));
    }

    #[test]
    fn flash_and_group_popups_keep_area_row() {
        for level in [GlmDataLevel::Flash, GlmDataLevel::Group] {
            let keys = grid_keys(&item(level, Some(128.0)));
            assert!(
                keys.iter().any(|k| k == "Area"),
                "{level:?} must still display area, got rows {keys:?}"
            );
        }
    }

    fn dead_east() -> DeadFeed {
        DeadFeed {
            satellite: GlmSatellite::GoesEast,
            bucket: "noaa-goes16",
            prefixes: vec!["GLM-L2-LCFA/2026/206/02/".into()],
        }
    }

    fn info_texts(handler: &GlmHandler) -> Vec<String> {
        let ctx = PaneControlContext {
            pane_idx: 0,
            pane_state: None,
        };
        handler
            .controls(&ctx)
            .into_iter()
            .filter_map(|i| match i {
                ControlItem::InfoText { text } => Some(text),
                _ => None,
            })
            .collect()
    }

    const BOTH: [GlmSatellite; 2] = [GlmSatellite::GoesEast, GlmSatellite::GoesWest];
    const WEST_ONLY: [GlmSatellite; 1] = [GlmSatellite::GoesWest];

    /// A dead feed must be visible without reading logs — that is the failure
    /// mode the whole change exists to prevent.
    #[test]
    fn dead_feed_is_surfaced_in_the_control_panel() {
        let mut handler = GlmHandler::new();
        handler.enabled = true;
        handler.report_feed_changes(&BOTH, vec![dead_east()]);

        let texts = info_texts(&handler);
        assert!(
            texts.iter().any(|t| t.contains("noaa-goes16") && t.contains("GOES-19 (East)")),
            "control panel should name the empty bucket, got {texts:?}"
        );
    }

    #[test]
    fn recovered_feed_clears_the_control_panel_notice() {
        let mut handler = GlmHandler::new();
        handler.enabled = true;
        handler.report_feed_changes(&BOTH, vec![dead_east()]);
        handler.report_feed_changes(&BOTH, Vec::new());

        let texts = info_texts(&handler);
        assert!(
            !texts.iter().any(|t| t.contains("noaa-goes16")),
            "notice should clear once the feed returns, got {texts:?}"
        );
    }

    /// Edge-triggering is the point: repeated polls in the same state must not
    /// accumulate, and re-entering the dead state must be reportable again.
    #[test]
    fn repeated_polls_do_not_accumulate_feed_state() {
        let mut handler = GlmHandler::new();
        handler.enabled = true;

        for _ in 0..5 {
            handler.report_feed_changes(&BOTH, vec![dead_east()]);
        }
        assert_eq!(handler.dead_feeds.len(), 1);
        assert_eq!(info_texts(&handler).iter().filter(|t| t.contains("noaa-goes16")).count(), 1);

        handler.report_feed_changes(&BOTH, Vec::new());
        assert!(handler.dead_feeds.is_empty());

        handler.report_feed_changes(&BOTH, vec![dead_east()]);
        assert_eq!(handler.dead_feeds.len(), 1);
    }

    /// A failed fetch tells us nothing about liveness, so the previous verdict
    /// must stand rather than being cleared into a false recovery.
    #[test]
    fn failed_fetch_leaves_feed_verdict_untouched() {
        let mut handler = GlmHandler::new();
        handler.enabled = true;
        handler.report_feed_changes(&BOTH, vec![dead_east()]);

        handler.apply_fetch_result(Box::new(GlmFetchResult(Err("network down".into()))));

        assert_eq!(handler.dead_feeds, vec![dead_east()]);
    }

    /// Deselecting a dead satellite must not read as recovery. The user
    /// switching to West to work around a dead East is the *likely* reaction to
    /// the notice, and it must not make the log contradict itself.
    #[test]
    fn deselecting_a_dead_satellite_is_not_a_recovery() {
        let mut handler = GlmHandler::new();
        handler.enabled = true;
        handler.satellite = SatelliteSelection::Both;
        handler.report_feed_changes(&BOTH, vec![dead_east()]);

        // User switches to West only; East is no longer queried.
        handler.satellite = SatelliteSelection::West;
        handler.report_feed_changes(&WEST_ONLY, Vec::new());

        assert_eq!(
            handler.dead_feeds,
            vec![dead_east()],
            "an unqueried satellite's verdict must be carried forward, not cleared"
        );
        // ...and the stale notice is not shown while East is deselected.
        assert!(
            !info_texts(&handler).iter().any(|t| t.contains("noaa-goes16")),
            "a deselected satellite should not occupy the panel"
        );
    }

    /// Because the verdict is retained rather than cleared, switching back does
    /// not re-fire the "is dead" warning — no alternating dead/recovered pairs
    /// driven purely by dropdown clicks.
    #[test]
    fn reselecting_a_still_dead_satellite_does_not_re_report_it() {
        let mut handler = GlmHandler::new();
        handler.enabled = true;
        handler.report_feed_changes(&BOTH, vec![dead_east()]);
        handler.report_feed_changes(&WEST_ONLY, Vec::new());
        handler.report_feed_changes(&BOTH, vec![dead_east()]);

        assert_eq!(handler.dead_feeds, vec![dead_east()]);
        assert_eq!(
            info_texts(&handler).iter().filter(|t| t.contains("noaa-goes16")).count(),
            1,
            "the notice should appear once, not duplicate per selection change"
        );
    }

    /// Recovery is still reported for a satellite that *was* queried.
    #[test]
    fn recovery_is_still_reported_for_a_queried_satellite() {
        let mut handler = GlmHandler::new();
        handler.enabled = true;
        handler.report_feed_changes(&BOTH, vec![dead_east()]);
        handler.report_feed_changes(&BOTH, Vec::new());

        assert!(handler.dead_feeds.is_empty());
    }

    fn total_failure() -> FetchFailures {
        FetchFailures {
            in_window: 12,
            failed: 12,
            sample_error: "GLM file has no 'flash_lat' variable (product schema change?)".into(),
        }
    }

    fn partial_failure(failed: usize) -> FetchFailures {
        FetchFailures { in_window: 12, failed, sample_error: "boom".into() }
    }

    /// The scenario that motivated this: a healthy S3 listing, every granule
    /// failing to parse, and previously nothing on screen but "Updated 0s ago".
    #[test]
    fn total_parse_failure_is_surfaced_in_the_control_panel() {
        let mut handler = GlmHandler::new();
        handler.enabled = true;
        handler.report_failures(FailureKind::Parse, Some(total_failure()));

        let texts = info_texts(&handler);
        assert!(
            texts.iter().any(|t| t.contains("failed to parse")),
            "a blank map from parse failures must be explained on screen, got {texts:?}"
        );
        assert!(
            handler.dead_feeds.is_empty(),
            "this failure mode has a healthy listing; it must not be reported as a dead feed"
        );
    }

    #[test]
    fn partial_parse_failure_is_distinguished_from_total() {
        let mut handler = GlmHandler::new();
        handler.enabled = true;
        handler.report_failures(FailureKind::Parse, Some(partial_failure(3)));

        let texts = info_texts(&handler);
        assert!(
            texts.iter().any(|t| t.contains("3/12")),
            "partial failures should report counts, got {texts:?}"
        );
        assert!(
            !texts.iter().any(|t| t.contains("All ")),
            "partial failure must not claim everything failed, got {texts:?}"
        );
    }

    #[test]
    fn parse_failure_notice_clears_when_files_parse_again() {
        let mut handler = GlmHandler::new();
        handler.enabled = true;
        handler.report_failures(FailureKind::Parse, Some(total_failure()));
        handler.report_failures(FailureKind::Parse, None);

        assert!(
            !info_texts(&handler).iter().any(|t| t.contains("failed to parse")),
            "notice should clear once parsing recovers"
        );
    }

    /// Health is tracked as a category so that a fluctuating failure count does
    /// not re-announce itself every poll.
    #[test]
    fn parse_health_does_not_flap_on_changing_counts() {
        let mut handler = GlmHandler::new();
        handler.enabled = true;

        for failed in [3usize, 7, 4, 9] {
            handler.report_failures(FailureKind::Parse, Some(partial_failure(failed)));
            assert_eq!(handler.parse.health, FailureHealth::Partial);
        }

        // Escalation to total is a real change and does update the category.
        handler.report_failures(FailureKind::Parse, Some(total_failure()));
        assert_eq!(handler.parse.health, FailureHealth::Total);
    }

    /// A network failure must never be dressed up as a product schema change.
    #[test]
    fn transport_failure_is_not_reported_as_a_product_change() {
        let mut handler = GlmHandler::new();
        handler.enabled = true;
        handler.report_failures(
            FailureKind::Transport,
            Some(FetchFailures {
                in_window: 12,
                failed: 12,
                sample_error: "a.nc: HTTP error: error sending request".into(),
            }),
        );

        let texts = info_texts(&handler);
        assert!(
            texts.iter().any(|t| t.contains("could not be downloaded")
                && t.contains("network down?")),
            "a transport failure should point at the network, got {texts:?}"
        );
        assert!(
            !texts.iter().any(|t| t.contains("product change?")),
            "S3 throttling must not be announced as a GLM product change, got {texts:?}"
        );
    }

    /// The two kinds track independently: a network blip must not clear or mask
    /// a live parse problem.
    #[test]
    fn parse_and_transport_failures_are_tracked_independently() {
        let mut handler = GlmHandler::new();
        handler.enabled = true;
        handler.report_failures(FailureKind::Parse, Some(total_failure()));
        handler.report_failures(FailureKind::Transport, Some(partial_failure(2)));

        let texts = info_texts(&handler);
        assert!(texts.iter().any(|t| t.contains("failed to parse")));
        assert!(texts.iter().any(|t| t.contains("could not be downloaded")));

        // Transport recovers; the parse problem stays on screen.
        handler.report_failures(FailureKind::Transport, None);
        let texts = info_texts(&handler);
        assert!(texts.iter().any(|t| t.contains("failed to parse")));
        assert!(!texts.iter().any(|t| t.contains("could not be downloaded")));
    }

    // ---- Log-output tests -------------------------------------------------
    //
    // Edge-triggering is the stated purpose of the dead-feed and failure
    // reporting, and it is only observable in the log. Without capturing the
    // log, both edge-trigger guards can be deleted with the suite still green.

    /// Captures records into `LOG_RECORDS` for assertion.
    struct CaptureLogger;

    static LOG_RECORDS: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());
    /// Serializes the log-observing tests, which necessarily share one global
    /// logger.
    static LOG_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    /// Only records from this thread are captured. The test harness runs other
    /// tests in parallel and several of them log; without this filter their
    /// output lands in the buffer and the counts below become nondeterministic.
    static CAPTURE_THREAD: std::sync::Mutex<Option<std::thread::ThreadId>> =
        std::sync::Mutex::new(None);

    impl log::Log for CaptureLogger {
        fn enabled(&self, _: &log::Metadata<'_>) -> bool {
            true
        }
        fn log(&self, record: &log::Record<'_>) {
            let capturing = *CAPTURE_THREAD.lock().unwrap_or_else(|e| e.into_inner());
            if capturing != Some(std::thread::current().id()) {
                return;
            }
            LOG_RECORDS
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(format!("{} {}", record.level(), record.args()));
        }
        fn flush(&self) {}
    }

    /// Run `f` and return everything it logged on this thread.
    fn captured_logs(f: impl FnOnce()) -> Vec<String> {
        static INIT: std::sync::Once = std::sync::Once::new();
        let _serial = LOG_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        INIT.call_once(|| {
            // If another logger is already installed the capture stays empty and
            // the assertions below fail loudly, which is the right outcome.
            let _ = log::set_logger(&CaptureLogger);
            log::set_max_level(log::LevelFilter::Trace);
        });

        LOG_RECORDS.lock().unwrap_or_else(|e| e.into_inner()).clear();
        *CAPTURE_THREAD.lock().unwrap_or_else(|e| e.into_inner()) =
            Some(std::thread::current().id());
        f();
        *CAPTURE_THREAD.lock().unwrap_or_else(|e| e.into_inner()) = None;
        LOG_RECORDS.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    fn count_containing(logs: &[String], needle: &str) -> usize {
        logs.iter().filter(|l| l.contains(needle)).count()
    }

    /// A dead feed warns once, not once per poll. At a 20 s interval the
    /// unguarded version emits ~180 lines an hour for as long as the outage
    /// lasts.
    #[test]
    fn dead_feed_warns_once_across_many_polls() {
        let logs = captured_logs(|| {
            let mut handler = GlmHandler::new();
            handler.enabled = true;
            for _ in 0..10 {
                handler.report_feed_changes(&BOTH, vec![dead_east()]);
            }
        });

        assert_eq!(
            count_containing(&logs, "feed is dead"),
            1,
            "expected exactly one dead-feed warning across 10 polls, got: {logs:?}"
        );
        assert!(
            logs.iter().any(|l| l.starts_with("WARN") && l.contains("noaa-goes16")),
            "the warning should name the bucket, got: {logs:?}"
        );
    }

    #[test]
    fn feed_recovery_logs_once_and_re_arms() {
        let logs = captured_logs(|| {
            let mut handler = GlmHandler::new();
            handler.enabled = true;
            handler.report_feed_changes(&BOTH, vec![dead_east()]);
            for _ in 0..5 {
                handler.report_feed_changes(&BOTH, Vec::new());
            }
            // Going dark again is a fresh edge and must warn again.
            handler.report_feed_changes(&BOTH, vec![dead_east()]);
        });

        assert_eq!(count_containing(&logs, "feed recovered"), 1, "{logs:?}");
        assert_eq!(count_containing(&logs, "feed is dead"), 2, "{logs:?}");
    }

    /// The reviewer's probe: deselecting a dead satellite must produce no log
    /// output at all — not a recovery, not a repeat warning.
    #[test]
    fn deselecting_a_dead_satellite_logs_nothing() {
        let logs = captured_logs(|| {
            let mut handler = GlmHandler::new();
            handler.enabled = true;
            handler.report_feed_changes(&BOTH, vec![dead_east()]);

            LOG_RECORDS.lock().unwrap_or_else(|e| e.into_inner()).clear();
            // Both -> West -> Both, with East still dark the whole time.
            handler.report_feed_changes(&WEST_ONLY, Vec::new());
            handler.report_feed_changes(&BOTH, vec![dead_east()]);
        });

        assert!(
            logs.is_empty(),
            "selection changes alone must not generate feed chatter, got: {logs:?}"
        );
    }

    /// The category guard is what stops a fluctuating count from re-announcing
    /// itself; without it these ten polls emit ten warnings.
    #[test]
    fn fluctuating_failure_counts_warn_once() {
        let logs = captured_logs(|| {
            let mut handler = GlmHandler::new();
            handler.enabled = true;
            for failed in [1usize, 5, 2, 7, 3, 6, 4, 8, 2, 9] {
                handler.report_failures(FailureKind::Parse, Some(partial_failure(failed)));
            }
        });

        assert_eq!(
            count_containing(&logs, "files in the window failed to parse"),
            1,
            "expected one warning across ten fluctuating polls, got: {logs:?}"
        );
    }

    #[test]
    fn escalation_and_recovery_each_log_once() {
        let logs = captured_logs(|| {
            let mut handler = GlmHandler::new();
            handler.enabled = true;
            handler.report_failures(FailureKind::Parse, Some(partial_failure(3)));
            handler.report_failures(FailureKind::Parse, Some(total_failure()));
            handler.report_failures(FailureKind::Parse, Some(total_failure()));
            handler.report_failures(FailureKind::Parse, None);
            handler.report_failures(FailureKind::Parse, None);
        });

        assert_eq!(count_containing(&logs, "3/12"), 1, "{logs:?}");
        assert_eq!(count_containing(&logs, "all 12 files"), 1, "{logs:?}");
        assert_eq!(count_containing(&logs, "recovered"), 1, "{logs:?}");
    }

    // ---- Seam tests -------------------------------------------------------
    //
    // Everything above drives the private reporting methods directly. These
    // drive apply_fetch_result with a populated outcome, which is the only path
    // production ever takes.

    fn outcome(
        queried: Vec<GlmSatellite>,
        dead_feeds: Vec<DeadFeed>,
        parse_failures: Option<FetchFailures>,
        transport_failures: Option<FetchFailures>,
    ) -> Box<dyn Any + Send> {
        Box::new(GlmFetchResult(Ok(crate::glm::GlmFetchOutcome {
            flashes: Vec::new(),
            dead_feeds,
            queried,
            parse_failures,
            transport_failures,
        })))
    }

    /// The Ok arm must actually forward the queried set and both failure
    /// reports. Passing `&[]` or `None` here silently severs the fixes above.
    #[test]
    fn apply_fetch_result_forwards_queried_set_and_failures() {
        let mut handler = GlmHandler::new();
        handler.enabled = true;

        // East goes dark through the real seam.
        handler.apply_fetch_result(outcome(
            vec![GlmSatellite::GoesEast, GlmSatellite::GoesWest],
            vec![dead_east()],
            None,
            None,
        ));
        assert_eq!(handler.dead_feeds, vec![dead_east()]);

        // Both failure kinds arrive through the seam and reach the panel.
        handler.apply_fetch_result(outcome(
            vec![GlmSatellite::GoesEast, GlmSatellite::GoesWest],
            vec![dead_east()],
            Some(total_failure()),
            Some(partial_failure(2)),
        ));
        let texts = info_texts(&handler);
        assert!(
            texts.iter().any(|t| t.contains("failed to parse")),
            "parse failures must survive the seam, got {texts:?}"
        );
        assert!(
            texts.iter().any(|t| t.contains("could not be downloaded")),
            "transport failures must survive the seam, got {texts:?}"
        );

        // Now East recovers, which is only correct because `queried` said we
        // asked. A seam that drops `queried` carries the dead verdict forward
        // instead of clearing it.
        handler.apply_fetch_result(outcome(
            vec![GlmSatellite::GoesEast, GlmSatellite::GoesWest],
            Vec::new(),
            None,
            None,
        ));
        assert!(
            handler.dead_feeds.is_empty(),
            "a queried satellite that stops being dead must clear through the seam"
        );
    }
}
