use std::any::Any;

use rustdar_units::UserPreferences;

use crate::metar::types::MetarOb;
use crate::render::draw::{DrawPointContext, HoverContext, MapPoint, PointPainter};
use crate::render::overlay_state::{
    ClickableItem, FetchConfig, FetchTask, OverlayHandler, OverlayKind, OverlayState,
    PopupContent, PopupSection, RasterizeContext, SelectedOverlay,
};
use crate::render::rasterize::RasterizeOutput;
use crate::render::station_model;
use crate::types::GeoBounds;

/// Type-erased fetch result for METAR observations.
pub(crate) struct MetarFetchResult(pub Result<Vec<MetarOb>, String>);

pub(crate) struct MetarHandler {
    pub state: OverlayState<Vec<MetarOb>>,
    cached_points: Vec<MapPoint>,
}

impl MetarHandler {
    pub fn new() -> Self {
        Self {
            state: OverlayState::new(),
            cached_points: Vec::new(),
        }
    }

    /// Rebuild the `cached_points` vec from current observation data.
    fn rebuild_points(&mut self) {
        self.cached_points = self
            .state
            .data
            .iter()
            .enumerate()
            .map(|(i, ob)| MapPoint {
                lat: ob.lat,
                lon: ob.lon,
                id: i as u32,
                selection: SelectedOverlay::Metar {
                    station_id: ob.station_id.clone(),
                },
            })
            .collect();
    }
}

impl OverlayHandler for MetarHandler {
    fn kind(&self) -> OverlayKind {
        OverlayKind::Metar
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
        Some(300) // Refresh every 5 minutes
    }

    fn clickable_items(
        &self,
        _layers: &crate::render::layers::LayerManager,
    ) -> Vec<ClickableItem<'_>> {
        // Surface obs use hit-buffer-based click detection, not polygon containment.
        Vec::new()
    }

    fn popup_content(
        &self,
        selected: &SelectedOverlay,
        prefs: &UserPreferences,
    ) -> Option<PopupContent> {
        let SelectedOverlay::Metar { station_id } = selected else {
            return None;
        };
        let ob = self.state.data.iter().find(|o| o.station_id == *station_id)?;

        let mut kv = Vec::new();

        // Temperature (show both °F and °C)
        if let Some(tc) = ob.temp_c {
            let tf = tc * 9.0 / 5.0 + 32.0;
            kv.push(("Temperature".into(), format!("{tf:.0}°F / {tc:.0}°C")));
        }

        // Dewpoint
        if let Some(td) = ob.dewp_c {
            let tdf = td * 9.0 / 5.0 + 32.0;
            kv.push(("Dewpoint".into(), format!("{tdf:.0}°F / {td:.0}°C")));
        }

        // Wind
        {
            let dir_str = ob
                .wind_dir
                .map(|d| format!("{d:03}°"))
                .unwrap_or_else(|| "VRB".to_string());
            let speed = ob.wind_speed_kt.unwrap_or(0);
            let converted = prefs.speed.convert_from_knots(speed as f32);
            let mut wind_text = format!("{dir_str} at {converted:.0} {}", prefs.speed.suffix());
            if let Some(gust) = ob.wind_gust_kt {
                let g_converted = prefs.speed.convert_from_knots(gust as f32);
                wind_text.push_str(&format!(", gusts {g_converted:.0} {}", prefs.speed.suffix()));
            }
            kv.push(("Wind".into(), wind_text));
        }

        // Visibility
        if let Some(vis) = ob.visibility_mi {
            let vis_str = if vis >= 10.0 {
                "10+ mi".to_string()
            } else {
                format!("{vis:.1} mi")
            };
            kv.push(("Visibility".into(), vis_str));
        }

        // Altimeter
        if let Some(alt) = ob.altimeter_hpa {
            let in_hg = alt * 0.02953;
            kv.push(("Altimeter".into(), format!("{in_hg:.2} inHg / {alt:.0} hPa")));
        }

        // Flight category
        if let Some(fc) = ob.flight_category {
            kv.push(("Flight Cat.".into(), fc.label().to_string()));
        }

        // Clouds
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

        // Present weather
        if let Some(ref wx) = ob.wx_string {
            kv.push(("Weather".into(), wx.clone()));
        }

        // Elevation
        if let Some(elev) = ob.elev_m {
            let elev_ft = elev * 3.28084;
            let converted = prefs.height.convert_from_feet(elev_ft as f32);
            kv.push((
                "Elevation".into(),
                format!("{converted:.0}{}", prefs.height.suffix()),
            ));
        }

        // Observation time
        if !ob.obs_time.is_empty() {
            kv.push(("Obs Time".into(), prefs.timezone.format_rfc3339(&ob.obs_time)));
        }

        let accent_rgb = ob
            .flight_category
            .map(|fc| {
                let c = fc.color_rgba();
                [c[0], c[1], c[2]]
            })
            .unwrap_or([150, 150, 150]);

        let mut sections = vec![PopupSection::KeyValueGrid(kv)];

        // Raw METAR
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

        Some(PopupContent {
            title,
            accent_rgb,
            width: 380.0,
            sections,
            actions: Vec::new(),
        })
    }

    fn apply_fetch_result(&mut self, result: Box<dyn Any + Send>) {
        let Some(fetch) = result.downcast::<MetarFetchResult>().ok() else {
            log::error!("METAR handler received unexpected fetch result type");
            return;
        };
        match fetch.0 {
            Ok(observations) => {
                log::info!("Received {} METAR observations", observations.len());
                self.state.set_data(observations);
            }
            Err(e) => {
                log::error!("METAR fetch failed: {e}");
            }
        }
        self.state.fetching = false;
        self.rebuild_points();
    }

    fn retain_selections(&self, selections: &mut Vec<SelectedOverlay>) {
        let ids: std::collections::HashSet<&str> = self
            .state
            .data
            .iter()
            .map(|o| o.station_id.as_str())
            .collect();
        selections.retain(|s| {
            if let SelectedOverlay::Metar { station_id } = s {
                ids.contains(station_id.as_str())
            } else {
                true
            }
        });
    }

    fn prepare_rasterize(
        &self,
        _ctx: &RasterizeContext,
    ) -> Option<Box<dyn FnOnce(&GeoBounds, u32, u32) -> RasterizeOutput + Send>> {
        None // Metar uses per-frame rendering, not background rasterization
    }

    fn create_fetch_tasks(&self, ctx: &FetchConfig) -> Vec<FetchTask> {
        log::info!("Fetching METAR observations");
        let client = ctx.client.clone();
        vec![FetchTask {
            kind: OverlayKind::Metar,
            future: Box::pin(async move {
                let result = crate::metar::fetch::fetch_current_metars(&client).await;
                Box::new(MetarFetchResult(result)) as Box<dyn Any + Send>
            }),
        }]
    }

    // ── Per-frame point rendering ─────────────────────────────────────

    fn per_frame_points(&self) -> &[MapPoint] {
        &self.cached_points
    }

    fn draw_point(&self, id: u32, painter: &mut dyn PointPainter, ctx: &DrawPointContext) {
        if let Some(ob) = self.state.data.get(id as usize) {
            station_model::draw_metar_station(ob, painter, ctx);
        }
    }

    fn point_hit_radius(&self, zoom: f32) -> f32 {
        station_model::hit_radius_for_zoom(zoom)
    }

    fn hover_text(&self, id: u32, ctx: &HoverContext<'_>) -> Option<String> {
        self.state.data.get(id as usize).map(|ob| {
            station_model::hover_text_for_metar(ob, ctx.prefs)
        })
    }
}
