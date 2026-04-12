use std::any::Any;
use std::sync::Arc;

use chrono::Utc;
use rustdar_units::UserPreferences;

use crate::glm::fetch::GlmCache;
use crate::glm::{GlmDataLevel, GlmFetchResult, GlmFlash, GlmSatellite};
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

        let sections = vec![
            PopupSection::Text(format!("{time_str} — {}", f.satellite.display_name())),
            PopupSection::KeyValueGrid(vec![
                ("Type".into(), f.level.display_name().into()),
                ("Latitude".into(), format!("{:.4}°", f.lat)),
                ("Longitude".into(), format!("{:.4}°", f.lon)),
                ("Energy".into(), format!("{:.2e} J", f.energy)),
                ("Area".into(), format!("{:.1} km²", f.area)),
            ]),
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) enum SatelliteSelection {
    East,
    West,
    Both,
}

impl SatelliteSelection {
    fn to_satellites(self) -> Vec<GlmSatellite> {
        match self {
            SatelliteSelection::East => vec![GlmSatellite::Goes16East],
            SatelliteSelection::West => vec![GlmSatellite::Goes18West],
            SatelliteSelection::Both => vec![GlmSatellite::Goes16East, GlmSatellite::Goes18West],
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

pub(crate) struct GlmHandler {
    pub state: OverlayState<Vec<Arc<GlmFlashItem>>>,
    pub enabled: bool,
    pub satellite: SatelliteSelection,
    /// Time window in seconds (60–1800).
    pub time_window_secs: f64,
    /// Which data hierarchy levels to include.
    pub show_events: bool,
    pub show_groups: bool,
    pub show_flashes: bool,
    /// Cached S3 file data for incremental fetching.
    pub cache: Arc<std::sync::Mutex<GlmCache>>,
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
        }
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
        let mut guard = self.cache.lock().unwrap();
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
            Ok(flashes) => {
                log::info!("Received {} GLM lightning flashes", flashes.len());
                let items = flashes
                    .into_iter()
                    .enumerate()
                    .map(|(i, flash)| Arc::new(GlmFlashItem { flash, index: i }))
                    .collect();
                self.state.set_data(items);
            }
            Err(e) => {
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
                    let guard = cache.lock().unwrap();
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
                    let mut guard = cache.lock().unwrap();
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
                    ("east".into(), "GOES-16 (East)".into()),
                    ("west".into(), "GOES-18 (West)".into()),
                    ("both".into(), "Both".into()),
                ],
                selected: self.satellite.as_str().into(),
            });

            // Time window slider: 1–30 minutes (60–1800 seconds)
            let mins = self.time_window_secs / 60.0;
            items.push(ControlItem::Slider {
                id: "time_window",
                label: "Time Window".into(),
                min: 1.0,
                max: 30.0,
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
                        // Satellite change needs a re-fetch
                        return ControlEffect::Fetch;
                    }
                }
                ControlEffect::None
            }
            "time_window" => {
                if let ControlValue::Float(mins) = update.value {
                    self.time_window_secs = (mins * 60.0).clamp(60.0, 1800.0);
                    return ControlEffect::Fetch;
                }
                ControlEffect::None
            }
            "show_events" => {
                if let ControlValue::Bool(val) = update.value {
                    self.show_events = val;
                    self.clear_cache();
                    return ControlEffect::Fetch;
                }
                ControlEffect::None
            }
            "show_groups" => {
                if let ControlValue::Bool(val) = update.value {
                    self.show_groups = val;
                    self.clear_cache();
                    return ControlEffect::Fetch;
                }
                ControlEffect::None
            }
            "show_flashes" => {
                if let ControlValue::Bool(val) = update.value {
                    self.show_flashes = val;
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
            self.time_window_secs = tw.clamp(60.0, 1800.0);
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
