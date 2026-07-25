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
    /// Last fetch error, surfaced in the controls so a failed fetch is not
    /// visible only in the log. Cleared by the next success.
    pub last_error: Option<String>,
}

impl ModelDataHandler {
    pub fn new() -> Self {
        Self {
            state: OverlayState::new(),
            enabled: false,
            selected_param: ModelParameter::SurfaceBasedCin,
            cached_grids: HashMap::new(),
            last_error: None,
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
                if let Some(notice) = grid.blank_notice() {
                    log::warn!("HRRR {}: {notice}", grid.parameter.short_name());
                }
                let param = grid.parameter;
                let arc = Arc::new(grid);
                self.cached_grids.insert(param, arc.clone());
                self.state.set_data(Some(arc));
                self.last_error = None;
            }
            Err(e) => {
                log::error!("HRRR fetch failed: {e}");
                self.last_error = Some(e);
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
        let grid = self.cached_grids.get(&self.selected_param);

        // The toggle label carries the timestamp, so it is the one place a
        // forecast-hour difference must not be hidden: showing a 0-1 h
        // maximum's run time alone would read as an analysis valid *now*.
        // Label f01+ with its valid time and the F-hour explicitly.
        let label = match grid {
            Some(g) if g.forecast_hour > 0 => format!(
                "\u{1f321}\u{fe0f}  Model Data ({} F{:02})",
                g.valid_time().format("%H:%Mz"),
                g.forecast_hour,
            ),
            Some(g) => format!(
                "\u{1f321}\u{fe0f}  Model Data ({})",
                g.ref_time.format("%H:%Mz")
            ),
            None => "\u{1f321}\u{fe0f}  Model Data".to_string(),
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

            // A fetch that failed leaves the previous parameter's grid on
            // screen, or nothing at all. Neither says "this is broken", so
            // the log was the only place the HTTP 500 that made both UH
            // parameters useless ever appeared.
            if let Some(err) = &self.last_error {
                items.push(ControlItem::InfoText {
                    text: format!("\u{26a0} {err}"),
                });
            }

            if let Some(grid) = self.cached_grids.get(&self.selected_param) {
                // Windowed fields are maxima over a period, not readings at an
                // instant. The F-hour in the label says *when*; this says
                // *what*, because "UH2-5 at 04:00z" still invites reading it
                // as a snapshot.
                if grid.forecast_hour > 0 && self.selected_param.is_windowed() {
                    items.push(ControlItem::InfoText {
                        text: format!(
                            "\u{2139} Maximum over {}\u{2013}{}, not an analysis field",
                            grid.ref_time.format("%H:%Mz"),
                            grid.valid_time().format("%H:%Mz"),
                        ),
                    });
                }

                // The failure this whole change exists to prevent: a grid that
                // fetched and decoded perfectly, and paints nothing.
                if let Some(notice) = grid.blank_notice() {
                    items.push(ControlItem::InfoText { text: notice });
                }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::GeoBounds;

    const RUN_HOUR: u32 = 3;

    fn grid(parameter: ModelParameter, values: Vec<f32>) -> HrrrGridData {
        let n = values.len();
        let (visible_points, value_range) = crate::hrrr::summarize_values(&values, parameter);
        HrrrGridData {
            parameter,
            values,
            lats: vec![35.0; n],
            lons: vec![-97.0; n],
            ni: n,
            nj: 1,
            bounds: GeoBounds {
                min_lat: 35.0,
                max_lat: 35.0,
                min_lon: -97.0,
                max_lon: -97.0,
            },
            ref_time: chrono::NaiveDate::from_ymd_opt(2026, 7, 25)
                .unwrap()
                .and_hms_opt(RUN_HOUR, 0, 0)
                .unwrap(),
            forecast_hour: parameter.forecast_hour(),
            visible_points,
            value_range,
        }
    }

    /// An enabled handler showing `parameter`, holding `values` for it.
    fn handler(parameter: ModelParameter, values: Vec<f32>) -> ModelDataHandler {
        let mut h = ModelDataHandler::new();
        h.enabled = true;
        h.selected_param = parameter;
        h.apply_fetch_result(Box::new(HrrrFetchResult(Ok(grid(parameter, values)))));
        h
    }

    fn controls_of(h: &ModelDataHandler) -> Vec<ControlItem> {
        h.controls(&PaneControlContext {
            pane_idx: 0,
            pane_state: None,
        })
    }

    /// The toggle label, which is where the timestamp lives.
    fn toggle_label(h: &ModelDataHandler) -> String {
        controls_of(h)
            .into_iter()
            .find_map(|i| match i {
                ControlItem::Toggle { label, .. } => Some(label),
                _ => None,
            })
            .expect("a toggle")
    }

    /// Every `InfoText` line, in order.
    fn info_lines(h: &ModelDataHandler) -> Vec<String> {
        controls_of(h)
            .into_iter()
            .filter_map(|i| match i {
                ControlItem::InfoText { text } => Some(text),
                _ => None,
            })
            .collect()
    }

    /// UH comes from f01, so it is valid an hour after the run. Labelling it
    /// with the run time alone would present a forecast as the analysis.
    #[test]
    fn a_forecast_hour_is_visible_in_the_toggle_label() {
        let label = toggle_label(&handler(ModelParameter::MaxUH2to5km, vec![120.0]));
        assert!(label.contains("F01"), "{label}");
        assert!(label.contains("04:00z"), "forecast valid time expected: {label}");
        assert!(!label.contains("03:00z"), "run time must not stand in: {label}");
    }

    /// Analysis fields must not grow a forecast suffix.
    #[test]
    fn an_analysis_field_is_labelled_with_its_run_time_only() {
        let label = toggle_label(&handler(ModelParameter::SurfaceBasedCin, vec![-400.0]));
        assert!(label.contains("03:00z"), "{label}");
        assert!(!label.contains("F0"), "{label}");
    }

    /// "UH2-5 at 04:00z" still invites reading as a snapshot; it is a maximum
    /// over the whole hour and has to say so.
    #[test]
    fn a_windowed_parameter_states_its_accumulation_window() {
        let lines = info_lines(&handler(ModelParameter::MaxUH2to5km, vec![120.0]));
        let note = lines
            .iter()
            .find(|l| l.contains("Maximum over"))
            .unwrap_or_else(|| panic!("no window note in {lines:?}"));
        assert!(note.contains("03:00z"), "{note}");
        assert!(note.contains("04:00z"), "{note}");
        assert!(note.contains("not an analysis"), "{note}");
    }

    #[test]
    fn an_analysis_field_has_no_window_note() {
        let lines = info_lines(&handler(ModelParameter::SurfaceBasedCin, vec![-400.0]));
        assert!(
            !lines.iter().any(|l| l.contains("Maximum over")),
            "{lines:?}",
        );
    }

    /// The failure this change exists to prevent: a grid that fetched and
    /// decoded perfectly and paints nothing must not do so quietly.
    #[test]
    fn a_blank_overlay_explains_itself_in_the_controls() {
        let lines = info_lines(&handler(ModelParameter::MaxUH2to5km, vec![0.0; 8]));
        let notice = lines
            .iter()
            .find(|l| l.contains("uniformly"))
            .unwrap_or_else(|| panic!("a blank overlay said nothing: {lines:?}"));
        assert!(notice.contains("UH2-5"), "{notice}");
        assert!(notice.contains("0 m\u{b2}/s\u{b2}"), "{notice}");
    }

    /// A field with data must stay quiet, or the notice is just noise.
    #[test]
    fn a_populated_overlay_reports_no_problem() {
        let lines = info_lines(&handler(ModelParameter::MaxUH2to5km, vec![120.0, 0.0]));
        assert!(!lines.iter().any(|l| l.contains('\u{26a0}')), "{lines:?}");
    }

    /// The HTTP 500 that made both UH parameters useless appeared only in the
    /// log. An enabled overlay with no data has to say why.
    #[test]
    fn a_fetch_error_is_reported_in_the_controls() {
        let mut h = ModelDataHandler::new();
        h.enabled = true;
        h.selected_param = ModelParameter::MaxUH2to5km;
        h.apply_fetch_result(Box::new(HrrrFetchResult(Err("HTTP 500".into()))));

        let lines = info_lines(&h);
        assert!(
            lines.iter().any(|l| l.contains("HTTP 500")),
            "fetch error must be surfaced, got {lines:?}",
        );
    }

    /// A recovered fetch must clear the stale error.
    #[test]
    fn a_successful_fetch_clears_a_previous_error() {
        let mut h = ModelDataHandler::new();
        h.enabled = true;
        h.selected_param = ModelParameter::MaxUH2to5km;
        h.apply_fetch_result(Box::new(HrrrFetchResult(Err("HTTP 500".into()))));
        h.apply_fetch_result(Box::new(HrrrFetchResult(Ok(grid(
            ModelParameter::MaxUH2to5km,
            vec![120.0],
        )))));

        let lines = info_lines(&h);
        assert!(!lines.iter().any(|l| l.contains("HTTP 500")), "{lines:?}");
    }
}
