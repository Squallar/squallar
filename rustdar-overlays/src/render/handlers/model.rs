use std::collections::HashMap;
use std::sync::Arc;

use crate::hrrr::{HrrrFetchResult, HrrrGridData, ModelParameter};
use crate::render::controls::{
    ControlButton, ControlEffect, ControlItem, ControlUpdate, ControlValue, PaneControlContext,
    PaneControlContextMut,
};
use crate::render::overlay_state::{
    FetchConfig, FetchPayload, FetchTask, OverlayHandler, OverlayKind, OverlayLegend, OverlayState,
    RasterizeContext, RenderMode,
};
use crate::render::rasterize::{self, RasterizeOutput};
use crate::types::GeoBounds;

pub(crate) struct ModelDataHandler {
    pub state: OverlayState<Option<Arc<HrrrGridData>>>,
    pub enabled: bool,
    pub selected_param: ModelParameter,
    /// Keyed per parameter so different panes can show different ones.
    pub cached_grids: HashMap<ModelParameter, Arc<HrrrGridData>>,
    /// Surfaced in the controls; otherwise a failed fetch appears only in the
    /// log. Cleared by the next success.
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

    /// The selected parameter's own name — which field of the model this
    /// layer is currently a picture of.
    fn status_line(&self) -> Option<String> {
        if !self.enabled {
            return None;
        }
        Some(self.selected_param.display_name().to_owned())
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

    fn fetch_time(&self) -> Option<web_time::Instant> {
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
        Some(3600) // HRRR runs hourly.
    }

    fn clickable_items(&self) -> Vec<crate::render::overlay_state::ClickableItem<'_>> {
        Vec::new() // Gridded, not feature-based; hover uses `hover_value_at`.
    }

    fn apply_fetch_result(&mut self, result: FetchPayload) {
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
        if lat < grid.bounds.min_lat
            || lat > grid.bounds.max_lat
            || lon < grid.bounds.min_lon
            || lon > grid.bounds.max_lon
        {
            return None;
        }
        // Nearest neighbour, not interpolation: the HRRR grid is ~3 km, finer
        // than a tooltip needs. Lambert grids answer this by forward-projecting
        // the cursor; everything else still scans.
        let index = grid.coords.nearest(lat, lon)?;
        let (glat, glon) = grid.coords.at(index)?;
        let best_val = *grid.values.get(index)?;
        let (dlat, dlon) = (glat - lat, glon - lon);
        // ~0.05° ≈ 5 km at mid-latitudes.
        if dlat * dlat + dlon * dlon > 0.05 * 0.05 {
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
        let sources = ctx.sources.clone();
        let param = self.selected_param;
        vec![FetchTask {
            kind: OverlayKind::ModelData,
            future: Box::pin(async move {
                let result = if param.is_composite() {
                    crate::hrrr::fetch::fetch_composite_hrrr_data(&client, &sources, &param).await
                } else {
                    crate::hrrr::fetch::fetch_hrrr_data(&client, &sources, &param).await
                };
                Box::new(result) as FetchPayload
            }),
        }]
    }

    fn controls(&self, _ctx: &PaneControlContext<'_>) -> Vec<ControlItem> {
        let grid = self.cached_grids.get(&self.selected_param);

        // f01+ must show its *valid* time and F-hour: a 0-1 h maximum labelled
        // with the run time alone reads as an analysis valid now.
        let label = match grid {
            Some(g) if g.forecast_hour > 0 => format!(
                "Model Data ({} F{:02})",
                g.valid_time().format("%H:%Mz"),
                g.forecast_hour,
            ),
            Some(g) => format!("Model Data ({})", g.ref_time.format("%H:%Mz")),
            None => "Model Data".to_string(),
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

        // A failed fetch leaves the previous parameter's grid on screen, or
        // nothing. Neither reads as "broken".
        if let Some(err) = &self.last_error {
            items.push(ControlItem::InfoText {
                text: format!("! {err}"),
            });
        }

        if let Some(grid) = self.cached_grids.get(&self.selected_param) {
            // Windowed fields are maxima over a period, not instantaneous
            // readings; "UH2-5 at 04:00z" alone reads as a snapshot.
            if grid.forecast_hour > 0 && self.selected_param.is_windowed() {
                items.push(ControlItem::InfoText {
                    text: format!(
                        "Maximum over {}-{}, not an analysis field",
                        grid.ref_time.format("%H:%Mz"),
                        grid.valid_time().format("%H:%Mz"),
                    ),
                });
            }

            // A grid can fetch and decode perfectly and still paint nothing.
            if let Some(notice) = grid.blank_notice() {
                items.push(ControlItem::InfoText { text: notice });
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
                        // Cached parameters re-render on a generation bump
                        // alone; no refetch.
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
            coords: crate::hrrr::GridCoords::Explicit {
                lats: vec![35.0; n],
                lons: vec![-97.0; n],
            },
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

    fn toggle_label(h: &ModelDataHandler) -> String {
        controls_of(h)
            .into_iter()
            .find_map(|i| match i {
                ControlItem::Toggle { label, .. } => Some(label),
                _ => None,
            })
            .expect("a toggle")
    }

    fn info_lines(h: &ModelDataHandler) -> Vec<String> {
        controls_of(h)
            .into_iter()
            .filter_map(|i| match i {
                ControlItem::InfoText { text } => Some(text),
                _ => None,
            })
            .collect()
    }

    /// Fails if a forecast is labelled with its run time. UH comes from f01, so
    /// it is valid an hour after the run.
    #[test]
    fn a_forecast_hour_is_visible_in_the_toggle_label() {
        let label = toggle_label(&handler(ModelParameter::MaxUH2to5km, vec![120.0]));
        assert!(label.contains("F01"), "{label}");
        assert!(
            label.contains("04:00z"),
            "forecast valid time expected: {label}"
        );
        assert!(
            !label.contains("03:00z"),
            "run time must not stand in: {label}"
        );
    }

    /// The counterpart: analysis fields must not grow an F-hour suffix.
    #[test]
    fn an_analysis_field_is_labelled_with_its_run_time_only() {
        let label = toggle_label(&handler(ModelParameter::SurfaceBasedCin, vec![-400.0]));
        assert!(label.contains("03:00z"), "{label}");
        assert!(!label.contains("F0"), "{label}");
    }

    /// Fails if a windowed field does not state its accumulation window.
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

    /// Fails if a grid that decoded perfectly and paints nothing stays silent.
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

    /// The counterpart: a populated field must stay quiet, or it is just noise.
    #[test]
    fn a_populated_overlay_reports_no_problem() {
        let lines = info_lines(&handler(ModelParameter::MaxUH2to5km, vec![120.0, 0.0]));
        assert!(!lines.iter().any(|l| l.contains('\u{26a0}')), "{lines:?}");
    }

    // ── Hover ─────────────────────────────────────────────────────────────

    /// A 2x2 grid whose four points carry four different values, so a lookup
    /// that lands on the wrong one is visible in the text.
    fn hover_handler() -> ModelDataHandler {
        let parameter = ModelParameter::SurfaceBasedCape;
        let values = vec![300.0, 1200.0, 2600.0, 4100.0];
        let (visible_points, value_range) = crate::hrrr::summarize_values(&values, parameter);
        let g = HrrrGridData {
            parameter,
            values,
            coords: crate::hrrr::GridCoords::Explicit {
                lats: vec![35.0, 35.0, 35.1, 35.1],
                lons: vec![-97.1, -97.0, -97.1, -97.0],
            },
            ni: 2,
            nj: 2,
            bounds: GeoBounds {
                min_lat: 35.0,
                max_lat: 35.1,
                min_lon: -97.1,
                max_lon: -97.0,
            },
            ref_time: chrono::NaiveDate::from_ymd_opt(2026, 7, 25)
                .unwrap()
                .and_hms_opt(RUN_HOUR, 0, 0)
                .unwrap(),
            forecast_hour: parameter.forecast_hour(),
            visible_points,
            value_range,
        };
        let mut h = ModelDataHandler::new();
        h.enabled = true;
        h.selected_param = parameter;
        h.apply_fetch_result(Box::new(HrrrFetchResult(Ok(g))));
        h
    }

    /// Each corner must report its own point's reading.
    #[test]
    fn hover_reports_the_nearest_grid_points_value() {
        let h = hover_handler();
        assert_eq!(
            h.hover_value_at(35.001, -97.099).as_deref(),
            Some("SBCAPE: 300 J/kg"),
        );
        assert_eq!(
            h.hover_value_at(35.099, -97.001).as_deref(),
            Some("SBCAPE: 4100 J/kg"),
        );
        assert_eq!(
            h.hover_value_at(35.001, -97.001).as_deref(),
            Some("SBCAPE: 1200 J/kg"),
        );
    }

    /// Outside the grid's bounds there is nothing to report.
    #[test]
    fn hover_is_silent_outside_the_grid_bounds() {
        let h = hover_handler();
        assert_eq!(h.hover_value_at(40.0, -97.05), None);
        assert_eq!(h.hover_value_at(35.05, -90.0), None);
    }

    /// Inside the bounds but ~7.8 km from all four points, which is past the
    /// 0.05° cutoff — a reading must not be stretched across a gap.
    #[test]
    fn hover_is_silent_further_than_the_cutoff_from_every_point() {
        assert_eq!(hover_handler().hover_value_at(35.05, -97.05), None);
    }

    /// 0.02° north of the top edge: outside the bounds, but *inside* the 0.05°
    /// cutoff of a real point. The bounds test is the only thing that can
    /// reject it, so the cases above would pass without it.
    #[test]
    fn hover_is_silent_just_outside_the_bounds_beside_a_real_point() {
        assert_eq!(hover_handler().hover_value_at(35.12, -97.0), None);
    }

    /// A parameter with no grid fetched has nothing to hover over.
    #[test]
    fn hover_is_silent_before_any_data_arrives() {
        assert_eq!(ModelDataHandler::new().hover_value_at(35.0, -97.0), None);
    }

    /// Fails if a fetch error is only logged. An HTTP 500 once made both UH
    /// parameters useless with nothing on screen to say so.
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
