use std::any::Any;
use std::sync::Arc;

use rustdar_units::UserPreferences;

use crate::metar::types::{MetarOb, WindDir};
use crate::render::controls::{
    ControlButton, ControlEffect, ControlItem, ControlUpdate, ControlValue, PaneControlContext,
    PaneControlContextMut,
};
use crate::render::draw::{DrawPointContext, HoverContext, MapPoint, PointPainter};
use crate::render::overlay_state::{
    ClickableItem, FetchConfig, FetchPayload, FetchTask, OverlayHandler, OverlayItem, OverlayKind,
    OverlayState, PopupContent, PopupSection, RasterizeContext, RasterizeFn, RenderMode,
};
use crate::render::station_model;

pub(crate) struct MetarFetchResult(pub Result<Vec<MetarOb>, String>);

const METAR_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

/// **Not `ctx.client`.** The shared client sends a `User-Agent`, which makes
/// the request non-simple; the browser then preflights and IEM answers
/// `OPTIONS` with `405`, so the GET is never issued. Native and `curl` see
/// none of this. The rule is read from
/// [`DataSources::metar_sends_user_agent`](rustdar_radar::sources::DataSources::metar_sends_user_agent),
/// not restated here.
fn metar_client(sources: &rustdar_radar::sources::DataSources) -> Result<reqwest::Client, String> {
    sources
        .metar_client(METAR_TIMEOUT)
        .build()
        .map_err(|e| format!("could not build the METAR client: {e}"))
}

#[derive(Debug)]
pub(crate) struct MetarItem {
    pub ob: MetarOb,
}

impl OverlayItem for MetarItem {
    fn kind(&self) -> OverlayKind {
        OverlayKind::Metar
    }

    fn popup_content(&self, prefs: &UserPreferences) -> PopupContent {
        let ob = &self.ob;

        let mut kv = Vec::new();

        if let Some(tc) = ob.temp_c {
            let tf = tc * 9.0 / 5.0 + 32.0;
            kv.push(("Temperature".into(), format!("{tf:.0}°F / {tc:.0}°C")));
        }

        if let Some(td) = ob.dewp_c {
            let tdf = td * 9.0 / 5.0 + 32.0;
            kv.push(("Dewpoint".into(), format!("{tdf:.0}°F / {td:.0}°C")));
        }

        {
            let speed = ob.wind_speed_kt.unwrap_or(0);
            let converted = prefs.speed.convert_from_knots(speed as f32);
            // "CALM at 0 kt" reads as a malfunction; calm has no speed to give.
            let mut wind_text = match ob.wind_dir {
                Some(WindDir::Calm) => "Calm".to_string(),
                Some(dir) => format!("{} at {converted:.0} {}", dir.label(), prefs.speed.suffix()),
                None => format!("{converted:.0} {}", prefs.speed.suffix()),
            };
            if let Some(gust) = ob.wind_gust_kt {
                let g_converted = prefs.speed.convert_from_knots(gust as f32);
                wind_text.push_str(&format!(
                    ", gusts {g_converted:.0} {}",
                    prefs.speed.suffix()
                ));
            }
            kv.push(("Wind".into(), wind_text));
        }

        if let Some(vis) = ob.visibility {
            kv.push(("Visibility".into(), format!("{} mi", vis.label())));
        }

        if let Some(alt) = ob.altimeter_hpa {
            let in_hg = alt * 0.02953;
            kv.push((
                "Altimeter".into(),
                format!("{in_hg:.2} inHg / {alt:.0} hPa"),
            ));
        }

        if let Some(fc) = ob.flight_category {
            kv.push(("Flight Cat.".into(), fc.label().to_string()));
        }

        if !ob.clouds.is_empty() {
            let cloud_str: Vec<String> = ob
                .clouds
                .iter()
                .map(|c| {
                    if let Some(base) = c.base_ft {
                        let converted = prefs.height.convert_from_feet(base as f32);
                        format!("{} {converted:.0}{}", c.cover, prefs.height.suffix())
                    } else {
                        c.cover.clone()
                    }
                })
                .collect();
            kv.push(("Clouds".into(), cloud_str.join(", ")));
        }

        if let Some(ref wx) = ob.wx_string {
            kv.push(("Weather".into(), wx.clone()));
        }

        if let Some(elev) = ob.elev_m {
            let elev_ft = elev * 3.28084;
            let converted = prefs.height.convert_from_feet(elev_ft as f32);
            kv.push((
                "Elevation".into(),
                format!("{converted:.0}{}", prefs.height.suffix()),
            ));
        }

        if !ob.obs_time.is_empty() {
            kv.push((
                "Obs Time".into(),
                prefs.timezone.format_rfc3339(&ob.obs_time),
            ));
        }

        let accent_rgb = ob
            .flight_category
            .map(|fc| {
                let c = fc.color_rgba();
                [c[0], c[1], c[2]]
            })
            .unwrap_or([150, 150, 150]);

        let mut sections = vec![PopupSection::KeyValueGrid(kv)];

        if !ob.raw_ob.is_empty() {
            sections.push(PopupSection::Separator);
            sections.push(PopupSection::ScrollableText {
                text: ob.raw_ob.clone(),
                monospace: true,
                max_height: 80.0,
            });
        }

        let title = if ob.name == ob.station_id {
            ob.station_id.clone()
        } else {
            format!("{} — {}", ob.station_id, ob.name)
        };

        PopupContent {
            title,
            accent_rgb,
            width: 380.0,
            sections,
            actions: Vec::new(),
        }
    }

    fn matches(&self, other: &dyn OverlayItem) -> bool {
        other
            .as_any()
            .downcast_ref::<MetarItem>()
            .is_some_and(|o| o.ob.station_id == self.ob.station_id)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

pub(crate) struct MetarHandler {
    pub state: OverlayState<Vec<Arc<MetarItem>>>,
    cached_points: Vec<MapPoint>,
    pub enabled: bool,
}

impl MetarHandler {
    pub fn new() -> Self {
        Self {
            state: OverlayState::new(),
            cached_points: Vec::new(),
            enabled: false,
        }
    }

    /// Must run after every `set_data`: `MapPoint::id` indexes `state.data`.
    fn rebuild_points(&mut self) {
        self.cached_points = self
            .state
            .data
            .iter()
            .enumerate()
            .map(|(i, item)| MapPoint {
                lat: item.ob.lat,
                lon: item.ob.lon,
                id: i as u32,
                selection: item.clone() as Arc<dyn OverlayItem>,
            })
            .collect();
    }
}

impl OverlayHandler for MetarHandler {
    fn kind(&self) -> OverlayKind {
        OverlayKind::Metar
    }

    fn display_name(&self) -> &str {
        "METAR Observations"
    }

    fn render_mode(&self) -> RenderMode {
        RenderMode::PerFramePoint
    }

    fn is_enabled(&self) -> bool {
        self.enabled
    }

    fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
    }

    /// E.g. `"148 stations"` — how many observations the map is placing.
    fn status_line(&self) -> Option<String> {
        if !self.enabled {
            return None;
        }
        Some(format!("{} stations", self.state.data.len()))
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

    fn fetch_time(&self) -> Option<web_time::Instant> {
        self.state.fetch_time
    }

    fn item_count(&self) -> usize {
        self.state.data.len()
    }

    fn auto_poll_interval(&self) -> Option<u64> {
        Some(300)
    }

    fn clickable_items(&self) -> Vec<ClickableItem> {
        Vec::new()
    }

    fn apply_fetch_result(&mut self, result: FetchPayload) {
        let Some(fetch) = result.downcast::<MetarFetchResult>().ok() else {
            log::error!("METAR handler received unexpected fetch result type");
            return;
        };
        match fetch.0 {
            Ok(observations) => {
                log::info!("Received {} METAR observations", observations.len());
                let items = observations
                    .into_iter()
                    .map(|ob| Arc::new(MetarItem { ob }))
                    .collect();
                self.state.set_data(items);
            }
            Err(e) => {
                log::error!("METAR fetch failed: {e}");
            }
        }
        self.state.fetching = false;
        self.rebuild_points();
    }

    fn retain_selections(&self, selections: &mut Vec<Arc<dyn OverlayItem>>) {
        selections.retain(|sel| {
            if sel.kind() != OverlayKind::Metar {
                return true;
            }
            self.state
                .data
                .iter()
                .any(|item| item.matches(sel.as_ref()))
        });
    }

    fn prepare_rasterize(&self, _ctx: &RasterizeContext) -> Option<RasterizeFn> {
        None // PerFramePoint mode; nothing is rasterized in the background.
    }

    fn create_fetch_tasks(&self, ctx: &FetchConfig) -> Vec<FetchTask> {
        // NOT `ctx.client` — see `metar_client`.
        let client = match metar_client(&ctx.sources) {
            Ok(c) => c,
            Err(e) => {
                log::error!("{e}");
                return Vec::new();
            }
        };
        let sources = ctx.sources.clone();
        let viewport = ctx
            .viewport
            .unwrap_or(crate::metar::networks::DEFAULT_VIEWPORT);
        log::info!("Fetching METAR observations for {viewport:?}");
        vec![FetchTask {
            kind: OverlayKind::Metar,
            future: Box::pin(async move {
                let result =
                    crate::metar::fetch::fetch_current_metars(&client, &sources, &viewport).await;
                Box::new(MetarFetchResult(result)) as FetchPayload
            }),
        }]
    }

    // ── Per-frame point rendering ─────────────────────────────────────

    fn per_frame_points(&self) -> &[MapPoint] {
        &self.cached_points
    }

    fn draw_point(&self, id: u32, painter: &mut dyn PointPainter, ctx: &DrawPointContext) {
        if let Some(item) = self.state.data.get(id as usize) {
            station_model::draw_metar_station(&item.ob, painter, ctx);
        }
    }

    fn point_hit_radius(&self, zoom: f32) -> f32 {
        station_model::hit_radius_for_zoom(zoom)
    }

    fn hover_text(&self, id: u32, ctx: &HoverContext<'_>) -> Option<String> {
        self.state
            .data
            .get(id as usize)
            .map(|item| station_model::hover_text_for_metar(&item.ob, ctx.prefs))
    }

    fn controls(&self, _ctx: &PaneControlContext<'_>) -> Vec<ControlItem> {
        let count = self.state.data.len();
        let label = if count == 0 {
            "\u{1f321}  METAR".to_string()
        } else {
            format!("\u{1f321}  METAR ({count})")
        };

        let mut items = vec![ControlItem::Toggle {
            id: "enabled",
            label,
            enabled: self.enabled,
        }];

        if self.enabled {
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
            "refresh" => ControlEffect::Fetch,
            _ => ControlEffect::None,
        }
    }

    fn serialize_state(&self) -> serde_json::Value {
        serde_json::json!({ "enabled": self.enabled })
    }

    fn deserialize_state(&mut self, value: serde_json::Value) {
        if let Some(enabled) = value.get("enabled").and_then(|v| v.as_bool()) {
            self.enabled = enabled;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metar::types::Visibility;
    use rustdar_units::SpeedUnit;

    /// Asserted on the client the handler actually builds, because native
    /// `tls::client` is the only thing that adds a `User-Agent` — the wasm one
    /// drops it, so a wasm-only check passes on a broken native client.
    #[test]
    fn the_metar_client_sends_no_user_agent() {
        let client = metar_client(&rustdar_radar::sources::DataSources::production())
            .expect("the METAR client must build");
        assert!(
            !rustdar_radar::tls::sends_user_agent(&client),
            "the METAR client carries a User-Agent, so the browser preflights \
             the GET and IEM answers OPTIONS with 405 — the observations \
             silently never arrive, and only on web",
        );
    }

    /// Fails if `metar_client` is hardwired to `simple_client`, which passes
    /// the test above while `metar_sends_user_agent` is read by nothing.
    #[test]
    fn the_metar_client_follows_the_origins_recorded_rule() {
        let sources = rustdar_radar::sources::DataSources {
            metar_sends_user_agent: true,
            ..rustdar_radar::sources::DataSources::production()
        };
        let client = metar_client(&sources).expect("the METAR client must build");
        assert!(
            rustdar_radar::tls::sends_user_agent(&client),
            "metar_client ignores DataSources::metar_sends_user_agent",
        );
    }

    fn ob(vis: Option<Visibility>) -> MetarOb {
        wind_ob(None, None, vis)
    }

    fn wind_ob(dir: Option<WindDir>, speed: Option<u16>, vis: Option<Visibility>) -> MetarOb {
        MetarOb {
            station_id: "KTST".into(),
            name: "KTST".into(),
            lat: 35.0,
            lon: -97.0,
            elev_m: None,
            temp_c: None,
            dewp_c: None,
            wind_dir: dir,
            wind_speed_kt: speed,
            wind_gust_kt: None,
            visibility: vis,
            altimeter_hpa: None,
            flight_category: None,
            raw_ob: String::new(),
            clouds: Vec::new(),
            wx_string: None,
            obs_time: String::new(),
        }
    }

    fn rows(ob: MetarOb) -> Vec<(String, String)> {
        let prefs = UserPreferences {
            speed: SpeedUnit::Knots,
            ..Default::default()
        };
        MetarItem { ob }
            .popup_content(&prefs)
            .sections
            .into_iter()
            .find_map(|s| match s {
                PopupSection::KeyValueGrid(kv) => Some(kv),
                _ => None,
            })
            .expect("popup must carry a key-value grid")
    }

    fn field(ob: MetarOb, key: &str) -> Option<String> {
        rows(ob).into_iter().find(|(k, _)| k == key).map(|(_, v)| v)
    }

    #[test]
    fn the_popup_reports_unrestricted_visibility() {
        let vis = Some(Visibility {
            miles: 10.0,
            or_greater: true,
        });
        assert_eq!(field(ob(vis), "Visibility").as_deref(), Some("10+ mi"));
    }

    #[test]
    fn the_popup_keeps_a_measurement_distinct_from_the_bound() {
        let vis = Some(Visibility {
            miles: 15.0,
            or_greater: false,
        });
        assert_eq!(field(ob(vis), "Visibility").as_deref(), Some("15 mi"));
    }

    #[test]
    fn the_popup_omits_visibility_when_the_station_reports_none() {
        assert_eq!(field(ob(None), "Visibility"), None);
    }

    /// Fails if a variable wind renders as the bearing "000°".
    #[test]
    fn the_popup_says_vrb_for_a_variable_wind() {
        let wind = field(wind_ob(Some(WindDir::Variable), Some(6), None), "Wind").unwrap();
        assert_eq!(wind, "VRB at 6 kt");
        assert!(
            !wind.contains("000"),
            "a variable wind is not a 000° bearing"
        );
    }

    #[test]
    fn the_popup_says_calm_without_inventing_a_direction() {
        let wind = field(wind_ob(Some(WindDir::Calm), Some(0), None), "Wind").unwrap();
        assert_eq!(wind, "Calm");
    }

    #[test]
    fn the_popup_keeps_a_real_bearing() {
        let wind = field(wind_ob(Some(WindDir::Degrees(360)), Some(3), None), "Wind").unwrap();
        assert_eq!(wind, "360° at 3 kt");
    }
}
