use std::any::Any;
use std::sync::Arc;

use rustdar_units::UserPreferences;

use crate::fetch_policy::Assembled;
use crate::metar::types::{MetarOb, WindDir};
use crate::render::controls::{
    ControlButton, ControlEffect, ControlItem, ControlUpdate, ControlValue, PaneControlContext,
    PaneControlContextMut,
};
use crate::render::draw::{DrawPointContext, HoverContext, MapPoint, PointPainter};
use crate::render::overlay_state::{
    ClickableItem, FetchConfig, FetchPayload, FetchTask, OverlayHandler, OverlayItem, OverlayKind,
    OverlayState, PopupContent, PopupSection, RenderMode,
};
use crate::render::station_model;

pub(crate) struct MetarFetchResult(
    pub Result<crate::metar::fetch::MetarRound, crate::fetch_policy::FetchError>,
);
/// [`Assembled`]: one request per state network, refused as a round only when
/// every one of them was. A single state's network declining takes every
/// station in that state off the map while the round still returns `Ok`,
/// because the rest of the country's observations are real — and a viewport
/// centred on the one dead state then drew nothing at all under `Updated 0s
/// ago`.
///
/// [`Assembled`]: crate::fetch_policy::Assembled
impl crate::fetch_policy::FetchRound for MetarFetchResult {
    type Shape = crate::fetch_policy::Assembled;
}

const METAR_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

/// **Not `ctx.client`.** The shared client sends a `User-Agent`, which makes
/// the request non-simple; the browser then preflights and IEM answers
/// `OPTIONS` with `405`, so the GET is never issued. Native and `curl` see
/// none of this. The rule is read from
/// [`DataSources::metar_sends_user_agent`](rustdar_source::origins::DataSources::metar_sends_user_agent),
/// not restated here.
fn metar_client(sources: &rustdar_source::origins::DataSources) -> Result<reqwest::Client, String> {
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

        // Its own row rather than folded into the altimeter's: the two are
        // different reductions of the same air and differ by a median 0.49 hPa,
        // up to 11.6 across 20 state networks.
        if let Some(mslp) = ob.mslp_hpa {
            kv.push(("Sea level".into(), format!("{mslp:.1} hPa")));
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
            format!("{} - {}", ob.station_id, ob.name)
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
    pub state: OverlayState<Vec<Arc<MetarItem>>, Assembled>,
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
        Some(300)
    }

    fn clickable_items(&self) -> Vec<ClickableItem<'_>> {
        Vec::new()
    }

    fn apply_fetch_result(&mut self, result: FetchPayload) {
        let Some(fetch) = self.state.downcast_round::<MetarFetchResult>(result) else {
            log::error!("METAR handler received unexpected fetch result type");
            return;
        };
        match fetch.0 {
            Ok(round) => {
                log::info!("Received {} METAR observations", round.observations.len());
                // A state network that did not answer takes every station in
                // that state off the map, whole, and the round still returns
                // `Ok` because the rest are real. That is the coverage axis,
                // not the health one — it used to go through `set_data`, which
                // declares an answer whole, and a viewport centred on the one
                // dead state drew nothing under a green `Updated 0s ago`.
                let coverage = round.completeness();
                let items = round
                    .observations
                    .into_iter()
                    .map(|ob| Arc::new(MetarItem { ob }))
                    .collect();
                self.state.set_data_with_coverage(items, coverage);
            }
            Err(e) => {
                log::error!("METAR fetch failed: {e}");
                // The round's verdict is computed where the per-request ones
                // are, by `FetchFailure::of_round` in `metar::fetch`, and is
                // refused only if every state network was. This used to be
                // hardcoded `transient` on the argument that one verdict for a
                // whole round could not be sharper than that — which cost the
                // sharpness in the wrong direction: a METAR endpoint that is
                // gone stays on the poll for ever.
                self.state.record_failure(&e);
            }
        }
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
            "METAR".to_string()
        } else {
            format!("METAR ({count})")
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
        let client = metar_client(&rustdar_source::origins::DataSources::production())
            .expect("the METAR client must build");
        assert!(
            !rustdar_source::tls::sends_user_agent(&client),
            "the METAR client carries a User-Agent, so the browser preflights \
             the GET and IEM answers OPTIONS with 405 — the observations \
             silently never arrive, and only on web",
        );
    }

    /// Fails if `metar_client` is hardwired to `simple_client`, which passes
    /// the test above while `metar_sends_user_agent` is read by nothing.
    #[test]
    fn the_metar_client_follows_the_origins_recorded_rule() {
        let sources = rustdar_source::origins::DataSources {
            metar_sends_user_agent: true,
            ..rustdar_source::origins::DataSources::production()
        };
        let client = metar_client(&sources).expect("the METAR client must build");
        assert!(
            rustdar_source::tls::sends_user_agent(&client),
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
            mslp_hpa: None,
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

#[cfg(all(test, not(target_arch = "wasm32")))]
mod round_tests {
    use super::*;
    use crate::render::controls::PaneControlContext;
    use crate::render::overlay_state::{OverlayFetchResult, OverlayRegistry};

    /// A viewport spanning several plains ASOS networks, so a round really is
    /// several requests and one of them can be refused on its own.
    fn plains() -> crate::types::GeoBounds {
        crate::types::GeoBounds {
            min_lat: 33.0,
            max_lat: 40.0,
            min_lon: -103.0,
            max_lon: -94.0,
        }
    }

    /// Serve `currents.json` from loopback, refusing exactly one state network.
    fn iem_refusing(dead: Option<&'static str>) -> rustdar_source::origins::DataSources {
        use std::io::{Read, Write};
        fn http(status_line: &str, body: &str) -> String {
            format!(
                "HTTP/1.1 {status_line}\r\nContent-Type: application/json\r\n\
                 Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len(),
            )
        }
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let port = listener.local_addr().expect("local addr").port();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                let mut scratch = [0u8; 8192];
                let read = stream.read(&mut scratch).unwrap_or(0);
                let request = String::from_utf8_lossy(&scratch[..read]).to_string();
                let refused = dead.is_some_and(|d| request.contains(&format!("network={d}_ASOS")));
                let out = if refused {
                    http("503 Service Unavailable", "down")
                } else {
                    http("200 OK", "{\"data\":[]}")
                };
                let _ = stream.write_all(out.as_bytes());
                let _ = stream.flush();
            }
        });
        rustdar_source::origins::DataSources {
            iem_base: format!("http://127.0.0.1:{port}").into(),
            ..rustdar_source::origins::DataSources::production()
        }
    }

    /// Fetch over the socket and push the result through the production ingest
    /// path, returning the row and the options note a user would see.
    fn round(dead: Option<&'static str>) -> (Option<String>, Option<String>) {
        rustdar_source::tls::init();
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(5))
            .build()
            .expect("client");
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("test runtime");
        let sources = iem_refusing(dead);
        let result = runtime.block_on(crate::metar::fetch::fetch_current_metars(
            &client,
            &sources,
            &plains(),
        ));

        let kind = OverlayKind::Metar;
        let mut registry = OverlayRegistry::default();
        registry.set_enabled(kind, true);
        registry.apply_fetch_result(OverlayFetchResult {
            kind,
            data: Box::new(MetarFetchResult(result)) as FetchPayload,
        });
        let ctx = PaneControlContext {
            pane_idx: 0,
            pane_state: None,
        };
        let note = registry
            .controls(kind, &ctx)
            .into_iter()
            .find_map(|item| match item {
                ControlItem::InfoText { text } if text.starts_with("Incomplete") => Some(text),
                _ => None,
            });
        (registry.status_line(kind), note)
    }

    /// **A state network that did not answer blanks that state.**
    ///
    /// The round returns `Ok` because the other seven networks are real, which
    /// is right. What was wrong is that it then said nothing: measured over
    /// this socket against the shipped code, eight networks asked, Oklahoma's
    /// refused 503, health `Ok`, `is_incomplete()` false, no mark — and every
    /// Oklahoma station absent from a map most likely centred on Oklahoma.
    #[test]
    fn a_state_network_that_did_not_answer_marks_the_layer_and_names_it() {
        assert!(
            crate::metar::networks::networks_for_viewport(&plains()).contains(&"OK"),
            "premise: the viewport asks Oklahoma's network",
        );
        let (line, note) = round(Some("OK"));
        let line = line.expect("an enabled METAR layer states its own line");
        assert!(
            line.starts_with("! incomplete"),
            "a whole state is blank and the row says nothing: {line}",
        );
        let note = note.expect("the options must say what the row is marking");
        assert!(
            note.contains("missing 1 of 8 state networks"),
            "the note must count the networks, not the stations: {note}",
        );
        assert!(
            note.contains("OK") && note.contains("503"),
            "the note must name which state and why: {note}",
        );
        assert!(
            !line.contains("not updating"),
            "seven networks answered on a fresh clock — not stale: {line}",
        );
    }

    /// The control: every network answering carries no mark. Without it the
    /// assertion above would pass on a report that always claimed incomplete.
    #[test]
    fn a_whole_round_carries_no_mark() {
        let (line, note) = round(None);
        let line = line.expect("line");
        assert!(!line.starts_with("!"), "nothing failed: {line}");
        assert_eq!(note, None);
    }
}
