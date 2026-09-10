use crate::render::overlay_state::{PaneMut, PaneRef, PaneToggle};
use std::any::Any;
use std::sync::Arc;

use squallar_units::UserPreferences;

use crate::fetch_policy::Assembled;
use crate::metar::types::{MetarOb, WindDir};
use crate::render::controls::{
    ControlButton, ControlEffect, ControlItem, ControlUpdate, ControlValue,
};
use crate::render::draw::{DrawPointContext, HoverContext, MapPoint, PointPainter};
use crate::render::overlay_state::Surface;
use crate::render::overlay_state::{
    ClickableItem, FetchConfig, FetchPayload, FetchTask, OverlayHandler, OverlayItem, OverlayState,
    PopupContent, PopupSection, RasterizeContext, RenderMode,
};
use crate::render::station_model;
use squallar_source::id::{LayerId, known};
use squallar_source::job::{DescribedJob, JobCodec};
use squallar_source::time::TimeAxis;

pub(crate) struct MetarFetchResult(
    pub Result<crate::metar::fetch::MetarRound, crate::fetch_policy::FetchError>,
);
impl crate::fetch_policy::FetchRound for MetarFetchResult {
    type Shape = crate::fetch_policy::Assembled;
}

const METAR_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

/// **Not `ctx.client`.** The shared client sends a `User-Agent`, which makes
/// the request non-simple; the browser then preflights and IEM answers
/// `OPTIONS` with `405`, so the GET is never issued. Native and `curl` see
/// none of this. The rule is read from
/// [`DataSources::metar_sends_user_agent`](squallar_source::origins::DataSources::metar_sends_user_agent),
/// not restated here.
fn metar_client(
    sources: &squallar_source::origins::DataSources,
) -> Result<reqwest::Client, String> {
    sources
        .metar_client(METAR_TIMEOUT)
        .map_err(|e| format!("could not build the METAR client: {e}"))
}

#[derive(Debug)]
pub(crate) struct MetarItem {
    pub ob: MetarOb,
    /// Formatted once here, read every frame. See [`station_model::StationText`].
    pub text: station_model::StationText,
}

/// **The observation and the four strings formatted from it.** Written here
/// rather than beside every other layer's in `render::footprint` for one
/// reason: this handler's module is private, so nothing outside it can name
/// the type.
impl squallar_source::footprint::ItemFootprint for MetarItem {
    fn owned_bytes(&self) -> u64 {
        self.ob
            .owned_bytes()
            .saturating_add(self.text.owned_bytes())
    }
}

impl OverlayItem for MetarItem {
    fn layer_id(&self) -> LayerId {
        known::METAR
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
    /// The observation rows of the current generation, built once per poll
    /// and shared by every dispatch since — see [`Self::prepare_job`].
    pub(crate) obs_memo:
        crate::render::signature_memo::BuiltMemo<Arc<Vec<crate::metar::types::MetarOb>>>,
    /// **The extent the map last drew**, and what the next round is fetched
    /// for. `None` until a frame has drawn a map at all.
    viewport: Option<squallar_geo::GeoBounds>,
    /// **The state networks the round on `state.data` asked for**; `None`
    /// before any round has landed.
    ///
    /// This is the only layer in the tree whose *request* is a function of the
    /// map extent, and nothing in the tree recorded which extent a round was
    /// fetched for. So zooming out after enabling the layer left the narrow
    /// round's stations on a continental map for up to a whole poll interval —
    /// five minutes of a map missing most of its stations, which is what a user
    /// reported.
    round_networks: Option<Vec<&'static str>>,
    /// [`Self::round_covers_viewport`]'s answer, computed when the map moves
    /// rather than when the question is asked: the question is asked on every
    /// frame by the auto-poll walk, and the answer only changes when a round
    /// lands or the extent moves.
    round_covers: bool,
}

impl MetarHandler {
    pub fn new() -> Self {
        Self {
            // Parked, because this handler implements `take_retired`:
            // the two are set together, so a park always has a drain.
            state: OverlayState::parked(),
            cached_points: Vec::new(),
            enabled: false,
            obs_memo: crate::render::signature_memo::BuiltMemo::new(
                crate::render::footprint::metar_job,
            ),
            viewport: None,
            round_networks: None,
            round_covers: true,
        }
    }

    /// **The extent the next round is fetched for.**
    ///
    /// The extent this layer was told about, which is the one its due-ness was
    /// decided against — see
    /// [`OverlayHandler::round_covers_viewport`]. Fetching for a different
    /// extent than the one that made the round stale is how a refetch becomes
    /// a loop: the answer would arrive still not covering the map, and the
    /// layer would be due again on the next frame, for ever. `ctx.viewport` is
    /// the same extent one dispatch later, and stands in until a frame has
    /// drawn a map at all.
    fn round_extent(&self, ctx: &FetchConfig) -> squallar_geo::GeoBounds {
        self.viewport
            .or(ctx.viewport)
            .unwrap_or(crate::metar::networks::DEFAULT_VIEWPORT)
    }

    /// Recompute [`Self::round_covers`] from the extent and the round now held.
    ///
    /// A round that has not landed yet cannot be outrun — the poll clock
    /// already has such a layer due — so the answer there is `true` and the
    /// network table is not walked at all.
    fn recheck_coverage(&mut self) {
        self.round_covers = match (&self.round_networks, &self.viewport) {
            (Some(held), Some(view)) => {
                !crate::metar::networks::viewport_reaches_beyond(view, held)
            }
            _ => true,
        };
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
    fn id(&self) -> LayerId {
        known::METAR
    }

    /// **Current observations only.** IEM's `currents.json` answers with the
    /// latest report per station and this layer keeps no archive of its own, so
    /// its honest answer at a past instant is the same one it gives now. That
    /// is a real limit rather than a property of the weather — an observation
    /// archive exists and is not read here — and `Live` is what says so.
    fn time_axis(&self) -> TimeAxis {
        TimeAxis::Live
    }
    fn surface(&self) -> Surface {
        Surface::Ground
    }
    fn draw_order_weight(&self) -> u32 {
        80
    }

    fn display_name(&self) -> &str {
        "METAR Observations"
    }

    fn render_mode(&self) -> RenderMode {
        RenderMode::TextureAndPoint
    }

    fn is_enabled(&self, pane: &PaneRef<'_>) -> bool {
        PaneToggle::is_on(pane, self.enabled)
    }

    fn set_enabled(&mut self, enabled: bool, pane: &mut PaneMut<'_>) {
        if !PaneToggle::set(pane, enabled) {
            self.enabled = enabled;
        }
    }

    /// E.g. `"148 stations"` — how many observations the map is placing.
    fn status_line(&self, _pane: &PaneRef<'_>) -> Option<String> {
        if !self.enabled {
            return None;
        }
        Some(format!("{} stations", self.state.data.len()))
    }

    fn data_generation(&self) -> u64 {
        self.state.data_generation
    }

    fn has_data(&self, _pane: &PaneRef<'_>) -> bool {
        !self.state.data.is_empty()
    }

    fn is_fetching(&self) -> bool {
        self.state.fetching
    }

    fn set_fetching(&mut self, fetching: bool, _pane: &PaneRef<'_>) {
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

    fn item_count(&self, _pane: &PaneRef<'_>) -> usize {
        self.state.data.len()
    }

    fn auto_poll_interval(&self) -> Option<u64> {
        Some(300)
    }

    /// **The one viewport-scoped layer in the tree**, so the one that has
    /// anything to do with this.
    ///
    /// The extent is taken on every call — it is what the next round is
    /// fetched for — and the coverage question is asked only once the map has
    /// come to rest. A round started mid-gesture would be fetched for an
    /// extent the gesture had already left, and the next frame would start
    /// another one.
    fn note_viewport(
        &mut self,
        view: &squallar_geo::GeoBounds,
        motion: squallar_source::handler::ViewportMotion,
    ) {
        self.viewport = Some(*view);
        match motion {
            squallar_source::handler::ViewportMotion::Moving => self.round_covers = true,
            squallar_source::handler::ViewportMotion::Settled => self.recheck_coverage(),
        }
    }

    /// Cached — see [`Self::round_covers`]. The walk that asks this asks it of
    /// every polling layer on every frame.
    fn round_covers_viewport(&self) -> bool {
        self.round_covers
    }

    fn clickable_items<'a>(&'a self, _pane: &PaneRef<'_>) -> Vec<ClickableItem<'a>> {
        Vec::new()
    }

    /// The generation this layer's state parked and the inputs its memo
    /// retired, handed back for the app to free off the frame thread — see
    /// [`OverlayHandler::take_retired`].
    /// **No pane draws this layer, so its round goes** — parked for the
    /// discard seam, not freed here. See [`OverlayHandler::release_data`].
    fn release_data(&mut self) -> bool {
        if !self.state.release_data() {
            return false;
        }
        // The round went with the data: there is nothing left to be outrun,
        // and a set kept past its observations would let the next pane to
        // switch this layer on inherit a coverage claim over an empty map.
        self.round_networks = None;
        self.round_covers = true;
        self.rebuild_points();
        // The built inputs were made from the data that just went away, and
        // nothing dispatches this layer any more, so no later `get_or_build`
        // would retire them.
        self.obs_memo.retire_live_rows();
        true
    }

    fn take_retired(&self) -> Vec<Box<dyn std::any::Any + Send>> {
        crate::render::overlay_state::retired_batch(
            self.state.take_retired(),
            self.obs_memo.take_retired(),
        )
    }

    fn apply_fetch_result(&mut self, result: FetchPayload, _pane: &PaneRef<'_>) {
        let Some(fetch) = self.state.downcast_round::<MetarFetchResult>(result) else {
            log::error!("METAR handler received unexpected fetch result type");
            return;
        };
        match fetch.0 {
            Ok(round) => {
                log::info!("Received {} METAR observations", round.observations.len());
                let coverage = round.completeness();
                // **What the layer now holds an answer for.** Recorded from the
                // round rather than recomputed from the extent: the extent may
                // have moved again while the round was in flight, and a
                // recomputed set would claim coverage the bytes do not have.
                self.round_networks = Some(round.networks);
                // **Built into an exactly sized list, not `collect`ed.** A
                // `collect` here takes the standard library's in-place
                // specialization -- same alignment, and the destination
                // element is not larger -- so the `Arc` pointers are written
                // over the observations and the round's own buffer becomes
                // the parked list's. A `MetarOb` is 272 B against an `Arc`'s
                // 8, so the layer parks a buffer 34x the pointers in it,
                // whatever the round's own reservation was: measured at 1000
                // stations, `installed_item_bytes` reported 716,000 B, of
                // which the carried buffer was 272,000 B holding 8,000 B of
                // pointers. `Vec::extend` into an already-sized destination
                // appends instead, and the round's buffer is freed with the
                // iterator.
                let observations = round.observations;
                let mut items = Vec::with_capacity(observations.len());
                items.extend(observations.into_iter().map(|ob| {
                    let text = station_model::StationText::of(&ob);
                    Arc::new(MetarItem { ob, text })
                }));
                self.state.set_data_with_coverage(items, coverage);
                self.recheck_coverage();
            }
            Err(e) => {
                log::error!("METAR fetch failed: {e}");
                // The held round is unchanged, so what it covers is unchanged:
                // a failed widening leaves the layer due, and the failure
                // ladder — not the poll clock — is what paces the retry.
                self.state.record_failure(&e);
            }
        }
        self.rebuild_points();
    }

    fn retain_selections(&self, selections: &mut Vec<Arc<dyn OverlayItem>>, _pane: &PaneRef<'_>) {
        selections.retain(|sel| {
            if sel.layer_id() != known::METAR {
                return true;
            }
            self.state
                .data
                .iter()
                .any(|item| item.matches(sel.as_ref()))
        });
    }

    fn create_fetch_tasks(&self, ctx: &FetchConfig, _pane: &PaneRef<'_>) -> Vec<FetchTask> {
        // NOT `ctx.client` — see `metar_client`.
        let client = match metar_client(&ctx.sources) {
            Ok(c) => c,
            Err(e) => {
                log::error!("{e}");
                return Vec::new();
            }
        };
        let sources = ctx.sources.clone();
        let viewport = self.round_extent(ctx);
        log::info!("Fetching METAR observations for {viewport:?}");
        vec![FetchTask {
            kind: known::METAR,
            future: Box::pin(async move {
                let result =
                    crate::metar::fetch::fetch_current_metars(&client, &sources, &viewport).await;
                Box::new(MetarFetchResult(result)) as FetchPayload
            }),
        }]
    }

    // ── Per-frame point rendering ─────────────────────────────────────

    /// **Where a METAR click is resolved, and the only place.** This layer is
    /// `TextureAndPoint`, so the pane runs both of its click paths over one
    /// layer; these points are the half that answers. The rasterizer builds no
    /// hit map ([`rasterize::rasterize_metar_stations`]) and this handler
    /// answers no [`OverlayHandler::hit_items`], so the texture path finds
    /// nothing to test and the walk below is unopposed.
    ///
    /// It was not always: both paths answered, over this same list, at the same
    /// `station_model::hit_radius_for_zoom` radius, and a click pushed one
    /// station into `selected_overlays` twice — a popup reading "1 of 2" with
    /// the same observation on both pages. Resolving here rather than off the
    /// picture is also what makes the answer a function of where the station is
    /// **now** instead of where the last raster put it.
    fn per_frame_points(&self) -> &[MapPoint] {
        &self.cached_points
    }

    fn draw_point(&self, id: u32, painter: &mut dyn PointPainter, ctx: &DrawPointContext) {
        if let Some(item) = self.state.data.get(id as usize) {
            station_model::draw_metar_station(&item.ob, &item.text, painter, ctx);
        }
    }

    /// What the rasterizer reads, captured once.
    ///
    /// **Row `i` is `state.data[i]`'s station**, the same indexing
    /// [`Self::per_frame_points`] carries in `MapPoint::id`, so a station's
    /// drawn model and its click target are one index apart from one list.
    /// Storm reports and GLM keep the stricter form of this contract, where the
    /// row order is also the hit-map id space `HitMap::from_cells` zips on;
    /// this layer has no hit map to zip.
    ///
    /// **The rows are built once per poll, not once per dispatch.** An
    /// observation is three `String`s and a `Vec` of cloud layers, and the
    /// network is a couple of thousand of them; the zoom-quantised dispatch
    /// under a wheel gesture asked for that deep clone on every quantum. The
    /// rows depend on the generation alone, so they sit behind one `Arc` the
    /// memo hands out, and what is built per dispatch is the four-field
    /// input around it.
    fn prepare_job(&self, ctx: &RasterizeContext, _pane: &PaneRef<'_>) -> Option<DescribedJob> {
        if self.state.data.is_empty() {
            return None;
        }
        let obs = self
            .obs_memo
            .get_or_build(self.state.data_generation, 0, || {
                Some(Arc::new(
                    self.state
                        .data
                        .iter()
                        .map(|i| i.ob.clone())
                        .collect::<Vec<_>>(),
                ))
            })?;
        Some(DescribedJob::new(crate::render::rasterize::MetarInput {
            obs,
            zoom: ctx.zoom,
            is_dark: ctx.is_dark,
            device_scale: ctx.device_scale,
        }))
    }

    fn job_codec(&self) -> Option<&'static JobCodec> {
        crate::render::jobs::JOB_CODECS
            .iter()
            .find(|row| row.label == "overlay/metar")
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

    fn controls(&self, pane: &PaneRef<'_>) -> Vec<ControlItem> {
        let count = self.state.data.len();
        let label = if count == 0 {
            "METAR".to_string()
        } else {
            format!("METAR ({count})")
        };

        let mut items = vec![ControlItem::Toggle {
            id: "enabled",
            label,
            enabled: self.is_enabled(pane),
        }];

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

    fn apply_control(&mut self, update: &ControlUpdate, pane: &mut PaneMut<'_>) -> ControlEffect {
        match update.id {
            "enabled" => {
                if let ControlValue::Bool(val) = update.value {
                    self.enabled = val;
                    if val
                        && self
                            .state
                            .enable_should_refetch(self.has_data(&pane.as_ref()))
                    {
                        return ControlEffect::Fetch;
                    }
                }
                ControlEffect::None
            }
            "refresh" => ControlEffect::Fetch,
            _ => ControlEffect::None,
        }
    }

    // ── Per-pane state (WO-M10b) ──────────────────────────────────────
    //
    // This layer's only per-pane fact is whether the pane draws it, so its
    // state IS the toggle. `self.enabled` survives as the registry's own copy
    // until WO-M10c deletes the swap that keeps it; every answer below prefers
    // the pane's when a pane is supplied.

    fn create_pane_state(&self, enabled: bool) -> Option<FetchPayload> {
        PaneToggle::create(enabled)
    }

    fn deserialize_pane_state(
        &self,
        value: serde_json::Value,
        enabled: bool,
    ) -> Option<FetchPayload> {
        PaneToggle::restore(&value, enabled)
    }

    fn serialize_pane_state(&self, state: &dyn std::any::Any) -> serde_json::Value {
        PaneToggle::save(state)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metar::types::Visibility;
    use squallar_units::SpeedUnit;

    /// Asserted on the client the handler actually builds, because native
    /// `tls::client` is the only thing that adds a `User-Agent` — the wasm one
    /// drops it, so a wasm-only check passes on a broken native client.
    #[test]
    fn the_metar_client_sends_no_user_agent() {
        let client = metar_client(&squallar_source::origins::DataSources::production())
            .expect("the METAR client must build");
        assert!(
            !squallar_source::tls::sends_user_agent(&client),
            "the METAR client carries a User-Agent, so the browser preflights \
             the GET and IEM answers OPTIONS with 405 — the observations \
             silently never arrive, and only on web",
        );
    }

    #[test]
    fn the_metar_client_follows_the_origins_recorded_rule() {
        let sources = squallar_source::origins::DataSources {
            metar_sends_user_agent: true,
            ..squallar_source::origins::DataSources::production()
        };
        let client = metar_client(&sources).expect("the METAR client must build");
        assert!(
            squallar_source::tls::sends_user_agent(&client),
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
        let text = station_model::StationText::of(&ob);
        MetarItem { ob, text }
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
    use crate::render::overlay_state::{OverlayFetchResult, OverlayRegistry};

    fn plains() -> squallar_geo::GeoBounds {
        squallar_geo::GeoBounds {
            min_lat: 33.0,
            max_lat: 40.0,
            min_lon: -103.0,
            max_lon: -94.0,
        }
    }

    fn iem_refusing(dead: Option<&'static str>) -> squallar_source::origins::DataSources {
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
        squallar_source::origins::DataSources {
            iem_base: format!("http://127.0.0.1:{port}").into(),
            ..squallar_source::origins::DataSources::production()
        }
    }

    fn round(dead: Option<&'static str>) -> (Option<String>, Option<String>) {
        squallar_source::tls::init();
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

        let kind = known::METAR;
        let mut registry = OverlayRegistry::default();
        registry.set_enabled(&kind, true, &mut PaneMut::bare(0));
        registry.apply_fetch_result(
            OverlayFetchResult {
                kind: kind.clone(),
                data: Box::new(MetarFetchResult(result)) as FetchPayload,
            },
            &PaneRef::bare(0),
        );
        let ctx = PaneRef::bare(0);
        let note = registry
            .controls(&kind, &ctx)
            .into_iter()
            .find_map(|item| match item {
                ControlItem::InfoText { text } if text.starts_with("Incomplete") => Some(text),
                _ => None,
            });
        (registry.status_line(&kind, &PaneRef::bare(0)), note)
    }

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

    #[test]
    fn a_whole_round_carries_no_mark() {
        let (line, note) = round(None);
        let line = line.expect("line");
        assert!(!line.starts_with("!"), "nothing failed: {line}");
        assert_eq!(note, None);
    }
}

#[cfg(test)]
mod prepare_memo_tests {
    use super::*;
    use crate::metar::types::MetarOb;

    fn station(id: &str) -> MetarOb {
        MetarOb {
            station_id: id.into(),
            name: format!("{id} field"),
            lat: 35.0,
            lon: -97.0,
            elev_m: None,
            temp_c: Some(21.0),
            dewp_c: None,
            wind_dir: None,
            wind_speed_kt: None,
            wind_gust_kt: None,
            visibility: None,
            altimeter_hpa: None,
            mslp_hpa: None,
            flight_category: None,
            raw_ob: format!("{id} 041953Z AUTO"),
            clouds: Vec::new(),
            wx_string: None,
            obs_time: String::new(),
        }
    }

    /// **The parked list is sized by the pointers in it, not by the
    /// observations they were built from.**
    ///
    /// A `collect` from the round's `Vec<MetarOb>` into `Vec<Arc<MetarItem>>`
    /// takes the standard library's in-place specialization: the alignments
    /// match and the destination element is not larger, so the pointers are
    /// written over the observations and **the round's buffer is kept as the
    /// parked list's**. A `MetarOb` is 272 B against an `Arc`'s 8, so the
    /// layer parks a buffer 34 slots deep per pointer -- and the census
    /// prices `capacity`, so it reports every one of them.
    ///
    /// Red on `95980a8de`: capacity 34,000 for 1,000 stations. The source
    /// here is exactly sized, so what this pins is the conversion and not the
    /// round's own reservation.
    #[test]
    fn the_parked_list_is_sized_by_its_pointers_not_its_observations() {
        const STATIONS: usize = 1000;
        let mut handler = MetarHandler::new();

        let mut observations = Vec::with_capacity(STATIONS);
        observations.extend((0..STATIONS).map(|i| station(&format!("K{i:03}"))));
        assert_eq!(
            observations.capacity(),
            STATIONS,
            "the round's own buffer is exact, so only the conversion is on trial",
        );

        handler.apply_fetch_result(
            Box::new(MetarFetchResult(Ok(crate::metar::fetch::MetarRound {
                observations,
                failed_networks: Vec::new(),
                networks: vec!["OK"],
            }))) as FetchPayload,
            &PaneRef::bare(0),
        );

        assert_eq!(handler.state.data.len(), STATIONS, "every station parked");
        assert_eq!(
            handler.state.data.capacity(),
            STATIONS,
            "the parked list holds {} slots for {STATIONS} pointers -- {} B of \
             buffer for {} B of content, the round's Vec<MetarOb> allocation \
             carried forward by an in-place collect",
            handler.state.data.capacity(),
            handler.state.data.capacity() * size_of::<Arc<MetarItem>>(),
            STATIONS * size_of::<Arc<MetarItem>>(),
        );
    }

    fn handler_with(n: usize) -> MetarHandler {
        let mut handler = MetarHandler::new();
        handler.state.data = (0..n)
            .map(|i| {
                let ob = station(&format!("K{i:03}"));
                let text = station_model::StationText::of(&ob);
                Arc::new(MetarItem { ob, text })
            })
            .collect();
        handler
    }

    fn ctx(zoom: f64) -> RasterizeContext {
        let clock = chrono::NaiveDate::from_ymd_opt(2026, 8, 20)
            .unwrap()
            .and_hms_opt(19, 0, 0)
            .unwrap();
        RasterizeContext {
            is_dark: false,
            zoom,
            device_scale: 1.0,
            now: clock,
            as_of: clock,
            frame: None,
        }
    }

    fn obs_of(job: &DescribedJob) -> &Arc<Vec<MetarOb>> {
        &job.downcast_ref::<crate::render::rasterize::MetarInput>()
            .expect("a METAR job")
            .obs
    }

    /// **The rows are built once per poll.** Two dispatches a zoom quantum
    /// apart describe two inputs — the zoom differs — that share ONE row
    /// allocation, and the deep clone of every observation ran once.
    #[test]
    fn dispatches_at_two_zooms_share_one_built_row_set() {
        let handler = handler_with(50);
        let pane = PaneRef::bare(0);
        let near = handler.prepare_job(&ctx(7.0), &pane).unwrap();
        let far = handler.prepare_job(&ctx(7.5), &pane).unwrap();
        assert_ne!(near, far, "the zoom is in the input and moved");
        assert!(
            Arc::ptr_eq(obs_of(&near), obs_of(&far)),
            "the observation rows must be one shared allocation",
        );
        assert_eq!(obs_of(&near).len(), 50);
        assert_eq!(
            handler.obs_memo.builds.get(),
            1,
            "fifty observations were cloned once, not once per dispatch",
        );
    }

    /// A poll moves the generation and the rows rebuild — once — and the old
    /// rows are parked for the discard seam.
    #[test]
    fn a_poll_rebuilds_the_rows_once_and_parks_the_old_ones() {
        let mut handler = handler_with(3);
        let pane = PaneRef::bare(0);
        let before = handler.prepare_job(&ctx(7.0), &pane).unwrap();
        handler.state.data_generation = handler.state.data_generation.wrapping_add(1);
        let after = handler.prepare_job(&ctx(7.0), &pane).unwrap();
        handler.prepare_job(&ctx(7.25), &pane);
        assert!(!Arc::ptr_eq(obs_of(&before), obs_of(&after)));
        assert_eq!(handler.obs_memo.builds.get(), 2);
        assert_eq!(handler.obs_memo.take_retired().len(), 1);
    }

    #[test]
    fn an_empty_network_describes_no_job_and_builds_nothing() {
        let handler = MetarHandler::new();
        assert!(handler.prepare_job(&ctx(7.0), &PaneRef::bare(0)).is_none());
        assert_eq!(handler.obs_memo.builds.get(), 0);
    }
}

/// **The user's sequence**: zoom in, switch METAR on, zoom out.
///
/// Reported 2026-09-10 as "most of the metar sites are missing" — a continental
/// map carrying stations over the central third of the country and nothing on
/// either coast. Two defects made that picture, both of them here:
///
///   * nothing recorded which extent a round was fetched for, so the narrow
///     round stayed on the map for the rest of its five-minute interval; and
///   * the round was capped at the twelve networks nearest the middle of the
///     map, and reported the twelve it kept as the number it had asked for, so
///     the completeness row over a map missing thirty-six states read
///     `expected: 12, missing: 0`.
#[cfg(all(test, not(target_arch = "wasm32")))]
mod viewport_tests {
    use super::*;
    use crate::metar::networks::{NETWORKS, networks_for_viewport};
    use crate::render::overlay_state::{OverlayFetchResult, OverlayRegistry};
    use squallar_geo::GeoBounds;
    use squallar_source::handler::ViewportMotion;

    fn view(min_lat: f64, max_lat: f64, min_lon: f64, max_lon: f64) -> GeoBounds {
        GeoBounds {
            min_lat,
            max_lat,
            min_lon,
            max_lon,
        }
    }

    /// KTLX padded by a degree — the zoomed-in map the layer is switched on at.
    fn oklahoma() -> GeoBounds {
        view(34.3, 36.3, -98.3, -96.3)
    }

    /// The same map nudged, still inside Oklahoma and the Texas panhandle.
    fn oklahoma_nudged() -> GeoBounds {
        view(34.4, 36.4, -98.2, -96.2)
    }

    /// The screenshot: the lower 48 plus a margin.
    fn continent() -> GeoBounds {
        view(24.0, 50.0, -125.0, -66.0)
    }

    fn lerp(a: &GeoBounds, b: &GeoBounds, t: f64) -> GeoBounds {
        let mix = |x: f64, y: f64| x + (y - x) * t;
        GeoBounds {
            min_lat: mix(a.min_lat, b.min_lat),
            max_lat: mix(a.max_lat, b.max_lat),
            min_lon: mix(a.min_lon, b.min_lon),
            max_lon: mix(a.max_lon, b.max_lon),
        }
    }

    /// **A stand-in IEM.** Every `network=XX_ASOS` is answered with one station
    /// `KXX` at the centre of that network's published extent, so which
    /// networks a round covered is readable off the stations it left on the
    /// map. `dead` refuses one network with a 503.
    ///
    /// Returns the origins and the request counter, which is how a test says
    /// how many requests a sequence really cost.
    fn iem_serving(
        dead: Option<&'static str>,
    ) -> (
        squallar_source::origins::DataSources,
        std::sync::Arc<std::sync::atomic::AtomicUsize>,
    ) {
        use std::io::{Read, Write};
        let requests = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counted = requests.clone();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let port = listener.local_addr().expect("local addr").port();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                let mut scratch = [0u8; 8192];
                let read = stream.read(&mut scratch).unwrap_or(0);
                let request = String::from_utf8_lossy(&scratch[..read]).to_string();
                counted.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let state = request
                    .split("network=")
                    .nth(1)
                    .and_then(|rest| rest.split("_ASOS").next())
                    .unwrap_or("")
                    .to_string();
                let network = NETWORKS.iter().find(|n| n.state == state);
                let body = match network {
                    _ if dead == Some(state.as_str()) => None,
                    Some(n) => Some(format!(
                        "{{\"data\":[{{\"station\":\"K{}\",\"name\":\"{} field\",\
                         \"lat\":{},\"lon\":{},\"tmpf\":70.0}}]}}",
                        n.state,
                        n.state,
                        (n.min_lat + n.max_lat) / 2.0,
                        (n.min_lon + n.max_lon) / 2.0,
                    )),
                    None => Some("{\"data\":[]}".to_string()),
                };
                let out = match body {
                    Some(body) => format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\
                         Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len(),
                    ),
                    None => "HTTP/1.1 503 Service Unavailable\r\nContent-Length: 4\r\n\
                             Connection: close\r\n\r\ndown"
                        .to_string(),
                };
                let _ = stream.write_all(out.as_bytes());
                let _ = stream.flush();
            }
        });
        (
            squallar_source::origins::DataSources {
                iem_base: format!("http://127.0.0.1:{port}").into(),
                ..squallar_source::origins::DataSources::production()
            },
            requests,
        )
    }

    fn runtime() -> tokio::runtime::Runtime {
        squallar_source::tls::init();
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("test runtime")
    }

    fn fetch_config(
        sources: &squallar_source::origins::DataSources,
        viewport: GeoBounds,
    ) -> FetchConfig {
        FetchConfig {
            client: reqwest::Client::new(),
            zone_cache_dir: None,
            sources: sources.clone(),
            // What the app hands every layer: the extent of the last frame
            // that dispatched a render.
            viewport: Some(viewport),
            as_of: chrono::Utc::now().naive_utc(),
            depicted_span_secs: None,
            depicted_frames: Vec::new(),
        }
    }

    /// One frame of a still map, and then one more: the extent is published
    /// once as [`ViewportMotion::Moving`] and once as
    /// [`ViewportMotion::Settled`], which is what the registry does across the
    /// two frames of a gesture ending.
    fn show(handler: &mut MetarHandler, at: GeoBounds) {
        handler.note_viewport(&at, ViewportMotion::Moving);
        handler.note_viewport(&at, ViewportMotion::Settled);
    }

    fn due(handler: &MetarHandler) -> bool {
        handler.auto_fetch_delay().is_some_and(|d| d.is_zero())
    }

    /// Run the round the handler would start: **the handler's own choice of
    /// extent**, the real round over it, and the answer back in through
    /// `apply_fetch_result`.
    ///
    /// The one production step this stands in for is the client.
    /// `create_fetch_tasks` builds METAR's own through
    /// `squallar_source::tls::simple_client`, which sets `https_only` — so a
    /// task it built would refuse a loopback fixture before it sent anything,
    /// and every round here would come back empty for a reason that has
    /// nothing to do with what is being tested. The extent decision, which is
    /// what these tests are about, is the handler's own
    /// [`MetarHandler::round_extent`] either way.
    fn run_round(
        handler: &mut MetarHandler,
        sources: &squallar_source::origins::DataSources,
        rt: &tokio::runtime::Runtime,
        app_viewport: GeoBounds,
    ) {
        let pane = PaneRef::bare(0);
        let extent = handler.round_extent(&fetch_config(sources, app_viewport));
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .build()
            .expect("client");
        handler.set_fetching(true, &pane);
        let round = rt.block_on(crate::metar::fetch::fetch_current_metars(
            &client, sources, &extent,
        ));
        assert!(
            round.is_ok(),
            "the fixture answered nothing at all: {:?}",
            round.err().map(|e| e.message),
        );
        handler.apply_fetch_result(Box::new(MetarFetchResult(round)) as FetchPayload, &pane);
    }

    /// The stations on the map, one per network the round covered.
    fn stations(handler: &MetarHandler) -> Vec<String> {
        let mut ids: Vec<String> = handler
            .state
            .data
            .iter()
            .map(|item| item.ob.station_id.clone())
            .collect();
        ids.sort();
        ids
    }

    /// **The report, walked end to end.**
    #[test]
    fn zooming_out_after_a_narrow_round_refetches_and_covers_the_wide_map() {
        let (sources, _requests) = iem_serving(None);
        let rt = runtime();
        let mut handler = MetarHandler::new();

        // 1. Zoomed in over Oklahoma, METAR switched on. Nothing has been
        //    fetched, so the layer is due on its clock alone.
        show(&mut handler, oklahoma());
        assert!(due(&handler), "a layer that has never fetched is due now");
        run_round(&mut handler, &sources, &rt, oklahoma());
        assert_eq!(
            stations(&handler),
            ["KOK", "KTX"],
            "the narrow map asks for the two networks it overlaps",
        );
        assert!(
            !due(&handler),
            "a fresh round over the map it was fetched for is not due again",
        );

        // 2. The user zooms out. The round on the layer answers a question
        //    about Oklahoma; the map is now asking about the country.
        show(&mut handler, continent());
        assert!(
            due(&handler),
            "the map reaches {} networks this round never asked for, and the \
             poll clock would hold the narrow round for five minutes",
            NETWORKS
                .iter()
                .filter(|n| n.intersects(&continent()) && !["OK", "TX"].contains(&n.state))
                .count(),
        );

        // 3. And the round it starts covers the map, corner to corner.
        run_round(&mut handler, &sources, &rt, continent());
        let on_map = stations(&handler);
        for corner in ["KCA", "KWA", "KFL", "KME"] {
            assert!(
                on_map.contains(&corner.to_string()),
                "{corner} is on the continental map and is missing: {on_map:?}",
            );
        }
        assert_eq!(
            on_map.len(),
            networks_for_viewport(&continent()).len(),
            "one station per network the map overlaps: {on_map:?}",
        );
        assert!(
            !due(&handler),
            "the wide round covers the wide map: nothing is due, and a round \
             that left the layer due would be a fetch every frame",
        );
    }

    /// The damping. A pan that does not leave the networks the round holds is
    /// not a reason to fetch, and without this every mouse-up over a state
    /// border would be a round.
    #[test]
    fn a_pan_inside_the_same_networks_does_not_make_the_layer_due() {
        let (sources, requests) = iem_serving(None);
        let rt = runtime();
        let mut handler = MetarHandler::new();

        show(&mut handler, oklahoma());
        run_round(&mut handler, &sources, &rt, oklahoma());
        let after_first = requests.load(std::sync::atomic::Ordering::Relaxed);
        assert_eq!(after_first, 2, "the narrow round is OK and TX");

        assert_eq!(
            networks_for_viewport(&oklahoma_nudged()),
            networks_for_viewport(&oklahoma()),
            "premise: the nudge asks for the very same networks",
        );
        show(&mut handler, oklahoma_nudged());
        assert!(
            !due(&handler),
            "the map moved and asked for nothing new, so the round still \
             answers it",
        );
        assert_eq!(
            requests.load(std::sync::atomic::Ordering::Relaxed),
            after_first,
            "and no request went out",
        );
    }

    /// **What a continuous zoom-out costs, in rounds.** Forty frames of a
    /// gesture that ends over the whole country: nothing is due on any of
    /// them, and one round starts on the frame the map comes to rest.
    ///
    /// Without the settle gate this is one round per frame at which the
    /// network set grows — a gesture that ends in a 54-network round having
    /// started dozens of smaller ones on the way.
    #[test]
    fn a_continuous_zoom_out_starts_one_round_and_starts_it_at_the_end() {
        let (sources, requests) = iem_serving(None);
        let rt = runtime();
        let mut handler = MetarHandler::new();

        show(&mut handler, oklahoma());
        run_round(&mut handler, &sources, &rt, oklahoma());
        let before = requests.load(std::sync::atomic::Ordering::Relaxed);

        const FRAMES: usize = 40;
        let mut due_during = 0;
        let mut grew = 0;
        let mut held = networks_for_viewport(&oklahoma());
        for frame in 1..=FRAMES {
            let at = lerp(&oklahoma(), &continent(), frame as f64 / FRAMES as f64);
            let wanted = networks_for_viewport(&at);
            if wanted.len() > held.len() {
                grew += 1;
                held = wanted;
            }
            handler.note_viewport(&at, ViewportMotion::Moving);
            if due(&handler) {
                due_during += 1;
            }
        }
        assert!(
            grew > 5,
            "premise: the network set really does grow during this gesture \
             ({grew} of {FRAMES} frames), or the damping is untested",
        );
        assert_eq!(
            due_during, 0,
            "a round started mid-gesture is fetched for an extent the gesture \
             has already left",
        );
        assert_eq!(
            requests.load(std::sync::atomic::Ordering::Relaxed),
            before,
            "and nothing went out during the gesture",
        );

        // The map comes to rest.
        handler.note_viewport(&continent(), ViewportMotion::Settled);
        assert!(due(&handler), "the gesture ended somewhere new: one round");
        run_round(&mut handler, &sources, &rt, continent());
        assert!(!due(&handler), "and exactly one");
        assert_eq!(
            requests.load(std::sync::atomic::Ordering::Relaxed) - before,
            networks_for_viewport(&continent()).len(),
            "one request per network the settled map overlaps, and no round \
             before it",
        );
    }

    /// **A round the layer holds is fetched for the extent its due-ness was
    /// decided against**, and not for whatever extent the app last dispatched
    /// a render at. The two differ by a frame, and fetching for the older one
    /// leaves the layer due on arrival — which is a round every frame, for
    /// ever.
    #[test]
    fn the_round_is_fetched_for_the_extent_the_layer_was_told_about() {
        let (sources, _requests) = iem_serving(None);
        let rt = runtime();
        let mut handler = MetarHandler::new();

        show(&mut handler, continent());
        // The app's own viewport still says Oklahoma: one frame behind.
        assert_eq!(
            handler.round_extent(&fetch_config(&sources, oklahoma())),
            continent(),
            "the round follows the extent the layer was told about",
        );
        run_round(&mut handler, &sources, &rt, oklahoma());
        let on_map = stations(&handler);
        assert!(
            on_map.contains(&"KCA".to_string()) && on_map.contains(&"KME".to_string()),
            "the round followed the stale extent: {on_map:?}",
        );
        assert!(!due(&handler), "and so it left the layer due");
    }

    /// **A settle changes what the layer will ASK for, and nothing it draws.**
    ///
    /// The drawn list moves through exactly one door — `set_data_with_coverage`,
    /// which bumps `data_generation` — and the frame path keys held work on
    /// that generation: a pass holding a tessellated mesh for these stations
    /// holds the cull and the projection that produced it under the same key.
    /// Due-ness now moves on a second signal, and this is what says that signal
    /// is not a second door.
    ///
    /// A settle that bumped the generation would be just as wrong as one that
    /// rewrote the list: it would throw that held work away on every gesture
    /// end, for a set of stations that did not change.
    #[test]
    fn a_settle_makes_the_layer_due_without_touching_what_it_draws() {
        let (sources, _requests) = iem_serving(None);
        let rt = runtime();
        let mut handler = MetarHandler::new();

        show(&mut handler, oklahoma());
        run_round(&mut handler, &sources, &rt, oklahoma());
        let generation = handler.data_generation();
        let drawn: Vec<(f64, f64, u32)> = handler
            .per_frame_points()
            .iter()
            .map(|p| (p.lat, p.lon, p.id))
            .collect();
        assert!(!drawn.is_empty(), "premise: stations are on the map");

        // The zoom-out. The layer becomes due...
        show(&mut handler, continent());
        assert!(due(&handler), "premise: the settle made the layer due");

        // ...and nothing it draws has moved.
        assert_eq!(
            handler.data_generation(),
            generation,
            "the settle moved the generation, which discards every frame-path              cache keyed on it for a list that did not change",
        );
        let after: Vec<(f64, f64, u32)> = handler
            .per_frame_points()
            .iter()
            .map(|p| (p.lat, p.lon, p.id))
            .collect();
        assert_eq!(after, drawn, "the settle changed the drawn list");

        // **And the arrival that follows moves both.** So the two equalities
        // above are a property of the settle, and not of a layer whose drawn
        // list never changes at all.
        run_round(&mut handler, &sources, &rt, continent());
        assert_ne!(
            handler.data_generation(),
            generation,
            "a round's arrival must move the generation, or the checks above              cannot fail",
        );
        assert!(
            handler.per_frame_points().len() > drawn.len(),
            "the wide round must replace the drawn list",
        );
    }

    /// **A network that did not answer is counted against every network the
    /// map overlaps.** The layer has an instrument that says a whole state is
    /// blank; before the cap came off, a continental round reported
    /// `expected: 12, missing: 0` over a map that was blank over thirty-six
    /// states, because the count it published was taken after the truncation.
    #[test]
    fn an_incomplete_continental_round_counts_against_the_whole_map() {
        let (sources, _requests) = iem_serving(Some("CA"));
        let rt = runtime();
        let mut handler = MetarHandler::new();

        show(&mut handler, continent());
        run_round(&mut handler, &sources, &rt, continent());

        let overlapped = NETWORKS
            .iter()
            .filter(|n| n.intersects(&continent()))
            .count();
        let mut registry = OverlayRegistry::default();
        registry.set_enabled(&known::METAR, true, &mut PaneMut::bare(0));
        registry.apply_fetch_result(
            OverlayFetchResult {
                kind: known::METAR,
                data: Box::new(MetarFetchResult(Ok(crate::metar::fetch::MetarRound {
                    observations: Vec::new(),
                    failed_networks: vec![(
                        "CA".into(),
                        crate::fetch_policy::FetchError::transient("CA: 503"),
                    )],
                    networks: networks_for_viewport(&continent()),
                }))) as FetchPayload,
            },
            &PaneRef::bare(0),
        );
        let note = registry
            .controls(&known::METAR, &PaneRef::bare(0))
            .into_iter()
            .find_map(|item| match item {
                ControlItem::InfoText { text } if text.starts_with("Incomplete") => Some(text),
                _ => None,
            })
            .expect("a round that lost a network says so");
        assert!(
            note.contains(&format!("missing 1 of {overlapped} state networks")),
            "the denominator must be every network the map overlaps, not the \
             number the round kept: {note}",
        );

        // And the same round, through the real fetch: California is the only
        // state missing from the map.
        let on_map = stations(&handler);
        assert!(
            !on_map.contains(&"KCA".to_string()),
            "California refused: {on_map:?}",
        );
        assert_eq!(
            on_map.len(),
            overlapped - 1,
            "every other network answered: {on_map:?}",
        );
    }
}
