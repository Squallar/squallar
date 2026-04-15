use std::any::Any;
use std::collections::HashMap;
use std::sync::Arc;

use crate::hrrr::{HrrrFetchResult, HrrrGridData, ModelParameter};
use crate::render::controls::{
    ControlButton, ControlEffect, ControlItem, ControlUpdate, ControlValue, PaneControlContext,
    PaneControlContextMut,
};
use crate::render::overlay_state::{
    FetchConfig, FetchTask, OverlayHandler, OverlayKind, OverlayLegend, OverlayState,
    RasterizeContext, RenderMode,
};
use crate::render::rasterize::{self, RasterizeOutput};
use crate::types::GeoBounds;

pub(crate) struct ModelDataHandler {
    pub state: OverlayState<Option<Arc<HrrrGridData>>>,
    pub enabled: bool,
    pub selected_param: ModelParameter,
    /// Per-parameter grid cache so different panes can render different parameters.
    pub cached_grids: HashMap<ModelParameter, Arc<HrrrGridData>>,
}

impl ModelDataHandler {
    pub fn new() -> Self {
        Self {
            state: OverlayState::new(),
            enabled: false,
            selected_param: ModelParameter::SurfaceBasedCin,
            cached_grids: HashMap::new(),
        }
    }
}

impl OverlayHandler for ModelDataHandler {
    fn kind(&self) -> OverlayKind {
        OverlayKind::ModelData
    }

    fn display_name(&self) -> &str {
        "Model Data"
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
        self.cached_grids.contains_key(&self.selected_param)
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
        self.state
            .data
            .as_ref()
            .map(|d| d.values.len())
            .unwrap_or(0)
    }

    fn auto_poll_interval(&self) -> Option<u64> {
        // HRRR updates hourly; refresh every 3600s.
        Some(3600)
    }

    fn clickable_items(
        &self,
    ) -> Vec<crate::render::overlay_state::ClickableItem> {
        // Model data grids are not clickable.
        Vec::new()
    }

    fn apply_fetch_result(&mut self, result: Box<dyn Any + Send>) {
        let Some(fetch) = result.downcast::<HrrrFetchResult>().ok() else {
            log::error!("ModelData handler received unexpected fetch result type");
            return;
        };
        match fetch.0 {
            Ok(grid) => {
                log::info!(
                    "Received HRRR {} data: {}×{} grid, {} points",
                    grid.parameter.display_name(),
                    grid.ni,
                    grid.nj,
                    grid.values.len(),
                );
                let param = grid.parameter;
                let arc = Arc::new(grid);
                self.cached_grids.insert(param, arc.clone());
                self.state.set_data(Some(arc));
            }
            Err(e) => {
                log::error!("HRRR fetch failed: {e}");
            }
        }
        self.state.fetching = false;
    }

    fn retain_selections(
        &self,
        _selections: &mut Vec<Arc<dyn crate::render::overlay_state::OverlayItem>>,
    ) {
        // No selectable items.
    }

    fn hover_value_at(&self, lat: f64, lon: f64) -> Option<String> {
        let grid = self.cached_grids.get(&self.selected_param)?;
        // Quick bounds check.
        if lat < grid.bounds.min_lat || lat > grid.bounds.max_lat
            || lon < grid.bounds.min_lon || lon > grid.bounds.max_lon
        {
            return None;
        }
        // Nearest-neighbor lookup: find grid point closest to (lat, lon).
        // HRRR grid is ~3 km, so nearest-neighbor is sufficient for tooltips.
        let mut best_dist_sq = f64::MAX;
        let mut best_val = f32::NAN;
        for (i, &value) in grid.values.iter().enumerate() {
            if i >= grid.lats.len() || i >= grid.lons.len() {
                break;
            }
            let dlat = grid.lats[i] - lat;
            let dlon = grid.lons[i] - lon;
            let d2 = dlat * dlat + dlon * dlon;
            if d2 < best_dist_sq {
                best_dist_sq = d2;
                best_val = value;
            }
        }
        // Only show if the nearest point is within ~5 km (~0.05° at mid-latitudes).
        if best_dist_sq > 0.05 * 0.05 {
            return None;
        }
        let text = grid.parameter.format_value(best_val);
        if text.is_empty() { None } else { Some(text) }
    }

    fn legend(&self) -> Option<OverlayLegend> {
        if !self.enabled {
            return None;
        }
        let thresholds = self.selected_param.legend_thresholds();
        let min = thresholds.first().map_or(0.0, |e| e.0);
        let max = thresholds.last().map_or(1.0, |e| e.0);
        Some(OverlayLegend {
            thresholds,
            is_gradient: true,
            min_value: min,
            max_value: max,
            unit_label: self.selected_param.unit_label(),
        })
    }

    fn prepare_rasterize(
        &self,
        _ctx: &RasterizeContext,
    ) -> Option<Box<dyn FnOnce(&GeoBounds, u32, u32) -> RasterizeOutput + Send>> {
        let grid = self.cached_grids.get(&self.selected_param)?.clone();
        Some(Box::new(move |bounds: &GeoBounds, width, height| {
            rasterize::rasterize_model_data(&grid, bounds, width, height)
        }))
    }

    fn create_fetch_tasks(&self, ctx: &FetchConfig) -> Vec<FetchTask> {
        let client = ctx.client.clone();
        let param = self.selected_param;
        vec![FetchTask {
            kind: OverlayKind::ModelData,
            future: Box::pin(async move {
                let result = if param.is_composite() {
                    crate::hrrr::fetch::fetch_composite_hrrr_data(&client, &param).await
                } else {
                    crate::hrrr::fetch::fetch_hrrr_data(&client, &param).await
                };
                Box::new(result) as Box<dyn Any + Send>
            }),
        }]
    }

    fn controls(&self, _ctx: &PaneControlContext<'_>) -> Vec<ControlItem> {
        let label = if let Some(grid) = self.cached_grids.get(&self.selected_param) {
            let time_str = grid.ref_time.format("%H:%Mz").to_string();
            format!("\u{1f321}\u{fe0f}  Model Data ({time_str})")
        } else {
            "\u{1f321}\u{fe0f}  Model Data".to_string()
        };

        let mut items = vec![ControlItem::Toggle {
            id: "enabled",
            label,
            enabled: self.enabled,
        }];

        if self.enabled {
            items.push(ControlItem::Dropdown {
                id: "parameter",
                label: "Parameter".into(),
                options: ModelParameter::all()
                    .iter()
                    .map(|p| (p.as_str().into(), p.display_name().into()))
                    .collect(),
                selected: self.selected_param.as_str().into(),
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
            "parameter" => {
                if let ControlValue::String(ref val) = update.value {
                    let new_param: ModelParameter = val.parse().unwrap();
                    if new_param != self.selected_param {
                        self.selected_param = new_param;
                        // If we already have cached data for this parameter,
                        // bump data_generation to trigger a re-render without
                        // a new fetch.
                        if self.cached_grids.contains_key(&new_param) {
                            self.state.data_generation = self.state.data_generation.wrapping_add(1);
                            return ControlEffect::None;
                        }
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
        serde_json::json!({
            "enabled": self.enabled,
            "parameter": self.selected_param.as_str(),
        })
    }

    fn deserialize_state(&mut self, value: serde_json::Value) {
        if let Some(enabled) = value.get("enabled").and_then(|v| v.as_bool()) {
            self.enabled = enabled;
        }
        if let Some(param) = value.get("parameter").and_then(|v| v.as_str()) {
            self.selected_param = param.parse().unwrap();
        }
    }
}
