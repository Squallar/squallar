use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;

use crate::fetch_policy::Whole;
use crate::hrrr::{HrrrFetchResult, HrrrGridData, ModelParameter};
use crate::render::controls::{
    ControlButton, ControlEffect, ControlItem, ControlUpdate, ControlValue, PaneControlContext,
    PaneControlContextMut,
};
use crate::render::overlay_state::Surface;
use crate::render::overlay_state::{
    FetchConfig, FetchPayload, FetchTask, OverlayHandler, OverlayLegend, OverlayState,
    RasterizeContext, RenderMode, Signed,
};
use crate::render::rasterize;
use rustdar_source::id::{LayerId, known};
use rustdar_source::job::{DescribedJob, JobCodec};

/// How many parameters' grids stay resident at once.
///
/// Sized by the pane count, not by a memory target: six panes can sit on six
/// different parameters, and below the pane count a miss costs a **picture**,
/// not a refetch — `prepare_job` answers `None` and the pane goes on drawing its
/// last texture with nothing that will re-ask. So the cap is
/// `rustdar_egui::pane::MAX_PANES_DESKTOP` = 6, spelled rather than imported
/// because the dependency cannot run back.
///
/// `HrrrGridData::values` is 1,905,141 `f32` — **7.62 MB** per resident grid —
/// plus **30.5 MB** of coordinates on the [`GridCoords::Explicit`] arm, which no
/// HRRR fetch reaches.
///
/// [`GridCoords::Explicit`]: crate::hrrr::GridCoords::Explicit
const MODEL_GRID_CACHE_ENTRIES: usize = 6;

// At a cap of 1, an insert of a parameter that is not the selected one protects
// both the arrival and the pin and the cache settles at two entries for ever.
// Two is where the eviction loop is guaranteed a victim.
const _: () = assert!(MODEL_GRID_CACHE_ENTRIES >= 2);

/// The resident grids, bounded and evicted least-recently-touched first.
///
/// An entries map plus a recency list holding exactly the keys of `entries`,
/// oldest use first; both private so no caller can desynchronise them. The list
/// is behind a `RefCell` because every *reader* reaches it through an `&self`
/// method of [`OverlayHandler`], and a lookup that did not count as a use would
/// let the pane on screen age out.
struct ModelGridCache {
    entries: HashMap<ModelParameter, Arc<HrrrGridData>>,
    recency: RefCell<Vec<ModelParameter>>,
}

impl ModelGridCache {
    fn new() -> Self {
        Self {
            entries: HashMap::new(),
            recency: RefCell::new(Vec::new()),
        }
    }

    fn touch(&self, param: ModelParameter) {
        let mut recency = self.recency.borrow_mut();
        if let Some(pos) = recency.iter().position(|p| *p == param) {
            recency.remove(pos);
            recency.push(param);
        }
    }

    fn get(&self, param: ModelParameter) -> Option<&Arc<HrrrGridData>> {
        let grid = self.entries.get(&param)?;
        self.touch(param);
        Some(grid)
    }

    /// Whether `param`'s grid is resident, marking it most-recently-used.
    ///
    /// Every read counts as a use, including the bare predicate: which accessor
    /// a caller reached for is not a fact about what the user is looking at.
    fn contains(&self, param: ModelParameter) -> bool {
        self.get(param).is_some()
    }

    /// Neither the entry going in nor `pinned` is ever evicted; that all visible
    /// parameters stay resident comes from the cap being at least the pane count.
    fn insert(&mut self, param: ModelParameter, grid: Arc<HrrrGridData>, pinned: ModelParameter) {
        if self.entries.insert(param, grid).is_some() {
            // A re-fetch of a resident parameter replaces its own key. Nothing
            // in this cache is keyed by run.
            self.touch(param);
        } else {
            self.recency.borrow_mut().push(param);
        }
        while self.entries.len() > MODEL_GRID_CACHE_ENTRIES {
            let victim = {
                let mut recency = self.recency.borrow_mut();
                let Some(pos) = recency.iter().position(|p| *p != param && *p != pinned) else {
                    break;
                };
                recency.remove(pos)
            };
            self.entries.remove(&victim);
        }
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.entries.len()
    }

    #[cfg(test)]
    fn is_resident(&self, param: ModelParameter) -> bool {
        self.entries.contains_key(&param)
    }

    #[cfg(test)]
    fn recency_order(&self) -> Vec<ModelParameter> {
        self.recency.borrow().clone()
    }
}

pub(crate) struct ModelDataHandler {
    pub state: OverlayState<Option<Arc<HrrrGridData>>, Whole>,
    pub enabled: bool,
    pub selected_param: ModelParameter,
    cached_grids: ModelGridCache,
    pub last_error: Option<String>,
}

impl ModelDataHandler {
    pub fn new() -> Self {
        Self {
            state: OverlayState::new(),
            enabled: false,
            selected_param: ModelParameter::SurfaceBasedCin,
            cached_grids: ModelGridCache::new(),
            last_error: None,
        }
    }
}

impl OverlayHandler for ModelDataHandler {
    fn id(&self) -> LayerId {
        known::MODEL_DATA
    }
    fn surface(&self) -> Surface {
        Surface::Ground
    }
    fn draw_order_weight(&self) -> u32 {
        10
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
        self.cached_grids.contains(self.selected_param)
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
        let Some(fetch) = self.state.downcast_round::<HrrrFetchResult>(result) else {
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
                // The pin is the parameter the pane is showing, which is what
                // every read path below keys on; an arrival must never blank it.
                let selected = self.selected_param;
                self.cached_grids.insert(param, arc.clone(), selected);
                self.state.set_data(Some(arc));
                self.last_error = None;
            }
            Err(e) => {
                log::error!("HRRR fetch failed: {e}");
                // The verdict comes with the error, merged across the two
                // candidate runs by `hrrr::fetch::round_verdict`. A run the
                // bucket does not carry yet classifies as `Absent`, which keeps
                // the layer on its ordinary hourly interval.
                self.last_error = Some(e.message.clone());
                self.state.record_failure(&e);
            }
        }
    }

    fn retain_selections(
        &self,
        _selections: &mut Vec<Arc<dyn crate::render::overlay_state::OverlayItem>>,
    ) {
    }

    fn hover_value_at(&self, lat: f64, lon: f64) -> Option<String> {
        let grid = self.cached_grids.get(self.selected_param)?;
        if !grid.bounds.contains_point(lat, lon) {
            return None;
        }
        // Nearest neighbour, not interpolation: the HRRR grid is ~3 km, finer
        // than a tooltip needs.
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

    /// The signature is the selected parameter and nothing else, since the bar is
    /// a pure function of it — deliberately **not** `data_generation`, which every
    /// HRRR fetch bumps. `+ 1` keeps the first parameter's signature off `0`.
    fn legend(&self) -> Option<Signed<OverlayLegend>> {
        if !self.enabled {
            return None;
        }
        let thresholds = self.selected_param.legend_thresholds();
        let min = thresholds.first().map_or(0.0, |e| e.0);
        let max = thresholds.last().map_or(1.0, |e| e.0);
        Some(Signed {
            signature: self.selected_param as u64 + 1,
            items: OverlayLegend {
                thresholds,
                is_gradient: true,
                min_value: min,
                max_value: max,
                unit_label: self.selected_param.unit_label(),
            },
        })
    }

    /// The [`Whole`](rasterize::ModelDataInput::Whole) carry: an `Arc` clone of
    /// the resident grid, so describing the job costs a refcount and the values
    /// memcpy happens only in the web encoder that knows the texture's bounds.
    fn prepare_job(&self, _ctx: &RasterizeContext) -> Option<DescribedJob> {
        let grid = self.cached_grids.get(self.selected_param)?.clone();
        Some(DescribedJob::new(rasterize::ModelDataInput::Whole(grid)))
    }

    fn job_codec(&self) -> Option<&'static JobCodec> {
        crate::render::jobs::JOB_CODECS
            .iter()
            .find(|row| row.label == "overlay/model")
    }

    fn create_fetch_tasks(&self, ctx: &FetchConfig) -> Vec<FetchTask> {
        let client = ctx.client.clone();
        let sources = ctx.sources.clone();
        let param = self.selected_param;
        vec![FetchTask {
            kind: known::MODEL_DATA,
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
        let grid = self.cached_grids.get(self.selected_param);

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

        // Ungated on enabled: a hidden layer's options stay visible and
        // editable, Refresh still fetches, and the status lines keep reporting.
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

        if let Some(err) = &self.last_error {
            items.push(ControlItem::InfoText {
                text: format!("! {err}"),
            });
        }

        if let Some(grid) = self.cached_grids.get(self.selected_param) {
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
                    if val && self.state.enable_should_refetch(self.has_data()) {
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
                        if self.cached_grids.contains(new_param) {
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
    use rustdar_geo::GeoBounds;

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

    #[test]
    fn an_analysis_field_is_labelled_with_its_run_time_only() {
        let label = toggle_label(&handler(ModelParameter::SurfaceBasedCin, vec![-400.0]));
        assert!(label.contains("03:00z"), "{label}");
        assert!(!label.contains("F0"), "{label}");
    }

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

    #[test]
    fn a_populated_overlay_reports_no_problem() {
        let lines = info_lines(&handler(ModelParameter::MaxUH2to5km, vec![120.0, 0.0]));
        assert!(!lines.iter().any(|l| l.contains('\u{26a0}')), "{lines:?}");
    }


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

    #[test]
    fn hover_is_silent_outside_the_grid_bounds() {
        let h = hover_handler();
        assert_eq!(h.hover_value_at(40.0, -97.05), None);
        assert_eq!(h.hover_value_at(35.05, -90.0), None);
    }

    /// Inside the bounds but ~7.8 km from all four points, past the 0.05° cutoff.
    #[test]
    fn hover_is_silent_further_than_the_cutoff_from_every_point() {
        assert_eq!(hover_handler().hover_value_at(35.05, -97.05), None);
    }

    /// 0.02° north of the top edge: outside the bounds but *inside* the 0.05°
    /// cutoff, so only the bounds test can reject it.
    #[test]
    fn hover_is_silent_just_outside_the_bounds_beside_a_real_point() {
        assert_eq!(hover_handler().hover_value_at(35.12, -97.0), None);
    }

    #[test]
    fn hover_is_silent_before_any_data_arrives() {
        assert_eq!(ModelDataHandler::new().hover_value_at(35.0, -97.0), None);
    }

    #[test]
    fn a_fetch_error_is_reported_in_the_controls() {
        let mut h = ModelDataHandler::new();
        h.enabled = true;
        h.selected_param = ModelParameter::MaxUH2to5km;
        h.apply_fetch_result(Box::new(HrrrFetchResult(Err(
            crate::fetch_policy::FetchError::transient("HTTP 500"),
        ))));

        let lines = info_lines(&h);
        assert!(
            lines.iter().any(|l| l.contains("HTTP 500")),
            "fetch error must be surfaced, got {lines:?}",
        );
    }

    #[test]
    fn a_successful_fetch_clears_a_previous_error() {
        let mut h = ModelDataHandler::new();
        h.enabled = true;
        h.selected_param = ModelParameter::MaxUH2to5km;
        h.apply_fetch_result(Box::new(HrrrFetchResult(Err(
            crate::fetch_policy::FetchError::transient("HTTP 500"),
        ))));
        h.apply_fetch_result(Box::new(HrrrFetchResult(Ok(grid(
            ModelParameter::MaxUH2to5km,
            vec![120.0],
        )))));

        let lines = info_lines(&h);
        assert!(!lines.iter().any(|l| l.contains("HTTP 500")), "{lines:?}");
    }


    /// The parameters these tests fill the cache with, in fetch order: exactly
    /// enough to fill it, plus one more to overflow it. Taken from
    /// [`ModelParameter::all`] so the set follows [`MODEL_GRID_CACHE_ENTRIES`].
    fn fill_order() -> &'static [ModelParameter] {
        let need = MODEL_GRID_CACHE_ENTRIES + 1;
        let all = ModelParameter::all();
        assert!(
            all.len() >= need,
            "these tests need {need} distinct parameters to overflow a cache of \
             {MODEL_GRID_CACHE_ENTRIES}, and there are {}",
            all.len(),
        );
        &all[..need]
    }

    fn resident_order() -> &'static [ModelParameter] {
        &fill_order()[..MODEL_GRID_CACHE_ENTRIES]
    }

    fn oldest() -> ModelParameter {
        fill_order()[0]
    }

    fn next_oldest() -> ModelParameter {
        fill_order()[1]
    }

    fn overflow() -> ModelParameter {
        fill_order()[MODEL_GRID_CACHE_ENTRIES]
    }

    fn deliver(h: &mut ModelDataHandler, parameter: ModelParameter) {
        h.apply_fetch_result(Box::new(HrrrFetchResult(Ok(grid(parameter, vec![300.0])))));
    }

    fn rasterize_ctx() -> RasterizeContext {
        RasterizeContext {
            is_dark: false,
            zoom: 5.0,
            device_scale: 1.0,
            now: chrono::NaiveDate::from_ymd_opt(2026, 7, 25)
                .unwrap()
                .and_hms_opt(3, 0, 0)
                .unwrap(),
        }
    }

    fn control_ctx<'a>() -> PaneControlContextMut<'a> {
        PaneControlContextMut {
            pane_idx: 0,
            pane_state: None,
        }
    }

    fn full_cache() -> ModelDataHandler {
        let mut h = ModelDataHandler::new();
        h.enabled = true;
        for &p in resident_order() {
            h.selected_param = p;
            deliver(&mut h, p);
        }
        assert_eq!(
            h.cached_grids.len(),
            MODEL_GRID_CACHE_ENTRIES,
            "the fixture must be full before a test evicts from it",
        );
        assert_eq!(
            h.cached_grids.recency_order(),
            resident_order().to_vec(),
            "fixture recency, oldest first",
        );
        h
    }

    /// A full desktop layout is six unlinked panes, each free to select its own
    /// parameter. This is the case a cap below the pane count breaks, and it
    /// breaks *silently*: `prepare_job` answers `None` and the starved pane goes
    /// on drawing its last texture, with nothing to re-ask.
    #[test]
    fn every_pane_of_a_full_desktop_layout_keeps_a_drawable_grid() {
        // Spelled, not imported: rustdar-overlays cannot depend on rustdar-egui.
        const MAX_PANES_DESKTOP: usize = 6;
        let panes = &ModelParameter::all()[..MAX_PANES_DESKTOP];

        let mut h = ModelDataHandler::new();
        h.enabled = true;
        for &p in panes {
            h.selected_param = p;
            deliver(&mut h, p);
        }

        for &p in panes {
            h.selected_param = p;
            assert!(h.has_data(), "the pane showing {p:?} has no grid");
            assert!(
                h.prepare_job(&rasterize_ctx()).is_some(),
                "the pane showing {p:?} would be skipped by app_fetch and left \
                 drawing a stale texture",
            );
        }
        assert_eq!(
            h.cached_grids.len(),
            MAX_PANES_DESKTOP,
            "every pane's grid must be resident at once",
        );
    }

    /// Fails if the map grows past the cap: unbounded, this held one 7.62 MB
    /// values vector per parameter.
    #[test]
    fn an_overflowing_parameter_evicts_the_least_recently_touched() {
        let mut h = full_cache();
        h.selected_param = overflow();
        deliver(&mut h, overflow());

        assert!(
            !h.cached_grids.is_resident(oldest()),
            "the least recently touched grid survived an overflowing insert",
        );
        for &p in &fill_order()[1..] {
            assert!(
                h.cached_grids.is_resident(p),
                "{p:?} must still be resident"
            );
        }
        assert_eq!(h.cached_grids.len(), MODEL_GRID_CACHE_ENTRIES);
        assert_eq!(h.cached_grids.recency_order(), fill_order()[1..].to_vec());
    }

    /// Cycling the whole Parameter dropdown is the gesture that grew this map to
    /// sixteen grids. The count is asserted *exactly*: "never exceeds the cap" is
    /// satisfied by a cache holding nothing.
    #[test]
    fn cycling_every_parameter_leaves_exactly_the_cap_resident() {
        let mut h = ModelDataHandler::new();
        h.enabled = true;
        for (i, p) in ModelParameter::all().iter().enumerate() {
            h.selected_param = *p;
            deliver(&mut h, *p);
            let expected = (i + 1).min(MODEL_GRID_CACHE_ENTRIES);
            assert_eq!(
                h.cached_grids.len(),
                expected,
                "after {} of {} parameters",
                i + 1,
                ModelParameter::all().len(),
            );
            assert!(
                h.cached_grids.is_resident(*p),
                "the parameter just fetched is the one on screen: {p:?}",
            );
            assert_eq!(
                h.cached_grids.recency_order().len(),
                expected,
                "the recency list must hold exactly the keys of the map",
            );
        }
        let tail = &ModelParameter::all()[ModelParameter::all().len() - MODEL_GRID_CACHE_ENTRIES..];
        assert_eq!(h.cached_grids.recency_order(), tail.to_vec());
    }

    /// Every `&self` reader of a grid must count as a use, or the parameter on
    /// screen ages out while one nobody has looked at survives. Each step reads
    /// the currently *oldest* parameter and requires the read to have moved it.
    /// read to have moved it to the most-recent end, and asserts it answered.
    #[test]
    fn every_read_path_counts_as_a_use() {
        // The fixture requirement, stated so that lowering the cap fails the
        // build rather than quietly walking fewer parameters than it claims.
        const _: () = assert!(
            MODEL_GRID_CACHE_ENTRIES >= 3,
            "this test walks three distinct parameters through the cache",
        );
        let mut h = full_cache();

        fn counted_as_a_use(h: &ModelDataHandler, read: ModelParameter, path: &str) {
            let order = h.cached_grids.recency_order();
            assert_eq!(
                order.len(),
                MODEL_GRID_CACHE_ENTRIES,
                "{path}: the recency list must still hold every key, got {order:?}",
            );
            assert_eq!(
                order.last(),
                Some(&read),
                "{path}: the read did not count as a use, order is {order:?}",
            );
        }

        let p = h.cached_grids.recency_order()[0];
        h.selected_param = p;
        assert!(
            h.hover_value_at(35.0, -97.0).is_some(),
            "the fixture must answer a hover, or this step proves nothing",
        );
        counted_as_a_use(&h, p, "hover_value_at");

        let p = h.cached_grids.recency_order()[0];
        h.selected_param = p;
        assert!(
            h.prepare_job(&rasterize_ctx()).is_some(),
            "the fixture must answer a rasterize",
        );
        counted_as_a_use(&h, p, "prepare_job");

        let p = h.cached_grids.recency_order()[0];
        h.selected_param = p;
        assert_ne!(
            toggle_label(&h),
            "Model Data",
            "the label must be the one built from a resident grid — only the \
             `Some(grid)` arm of `controls` can produce a time in it",
        );
        counted_as_a_use(&h, p, "controls");

        let p = h.cached_grids.recency_order()[0];
        h.selected_param = p;
        assert!(h.has_data(), "{p:?} is resident");
        counted_as_a_use(&h, p, "has_data");

        let p = h.cached_grids.recency_order()[0];
        assert_ne!(
            p, h.selected_param,
            "the dropdown branch runs only on a change"
        );
        let effect = h.apply_control(
            &ControlUpdate {
                id: "parameter",
                value: ControlValue::String(p.as_str().into()),
            },
            &mut control_ctx(),
        );
        assert_eq!(
            effect,
            ControlEffect::None,
            "a resident parameter must re-render, not refetch",
        );
        counted_as_a_use(&h, p, "apply_control(parameter)");
    }

    /// The grid the user hovered must outlive one that was only ever fetched:
    /// without the hover counting as a use, `oldest` is what the insert takes.
    #[test]
    fn a_hovered_parameter_outlives_one_that_was_only_fetched() {
        let mut h = full_cache();
        h.selected_param = oldest();
        assert!(
            h.hover_value_at(35.0, -97.0).is_some(),
            "the fixture must answer a hover",
        );

        h.selected_param = overflow();
        deliver(&mut h, overflow());

        assert!(
            h.cached_grids.is_resident(oldest()),
            "the hovered grid was evicted anyway",
        );
        assert!(
            !h.cached_grids.is_resident(next_oldest()),
            "the oldest use is what must go",
        );
        assert_eq!(h.cached_grids.len(), MODEL_GRID_CACHE_ENTRIES);
    }

    /// The parameter the pane is showing is pinned, even when it is the oldest
    /// thing in the list: `deserialize_state` assigns it bare when a pane is
    /// swapped in, so a pane can sit on a grid nothing has touched since.
    #[test]
    fn the_selected_parameter_survives_an_insert_that_would_evict_it() {
        let mut h = full_cache();
        h.selected_param = oldest();
        assert_eq!(
            h.cached_grids.recency_order(),
            resident_order().to_vec(),
            "a bare assignment must not count as a use",
        );

        // Another parameter's fetch lands while `oldest` is still on screen.
        deliver(&mut h, overflow());

        assert!(
            h.cached_grids.is_resident(oldest()),
            "the parameter on screen was evicted under the user",
        );
        assert!(
            !h.cached_grids.is_resident(next_oldest()),
            "the eviction must still happen, one entry along",
        );
        assert!(h.cached_grids.is_resident(overflow()));
        assert_eq!(h.cached_grids.len(), MODEL_GRID_CACHE_ENTRIES);
    }

    /// A re-fetch replaces its own key rather than adding one, so a superseded
    /// run costs nothing and evicts nobody — the cache carries no run identity.
    #[test]
    fn a_refetch_of_a_resident_parameter_replaces_its_own_key() {
        let mut h = full_cache();
        h.selected_param = oldest();
        deliver(&mut h, oldest());

        assert_eq!(
            h.cached_grids.len(),
            MODEL_GRID_CACHE_ENTRIES,
            "a re-fetch must not grow the map",
        );
        for &p in resident_order() {
            assert!(
                h.cached_grids.is_resident(p),
                "{p:?} must still be resident"
            );
        }
        let order = h.cached_grids.recency_order();
        assert_eq!(
            order.last(),
            Some(&oldest()),
            "the replaced key is the most recent use, order is {order:?}",
        );
        assert_eq!(
            order.first(),
            Some(&next_oldest()),
            "and the one behind it becomes the oldest, order is {order:?}",
        );
    }

    /// Eviction costs one refetch when the user returns, and the toggle is where
    /// that has to happen. The counterpart is asserted too.
    #[test]
    fn an_evicted_parameter_refetches_when_the_layer_is_toggled_back_on() {
        let mut h = full_cache();
        h.selected_param = overflow();
        deliver(&mut h, overflow()); // evicts `oldest`

        h.selected_param = oldest();
        assert!(
            !h.has_data(),
            "{:?} must be gone, or this is not the eviction case",
            oldest(),
        );
        h.enabled = false;
        assert_eq!(
            h.apply_control(
                &ControlUpdate {
                    id: "enabled",
                    value: ControlValue::Bool(true),
                },
                &mut control_ctx(),
            ),
            ControlEffect::Fetch,
            "an evicted parameter must refetch on the toggle, not leave the layer blank",
        );

        h.selected_param = overflow();
        assert!(h.has_data(), "{:?} is resident", overflow());
        h.enabled = false;
        assert_eq!(
            h.apply_control(
                &ControlUpdate {
                    id: "enabled",
                    value: ControlValue::Bool(true),
                },
                &mut control_ctx(),
            ),
            ControlEffect::None,
            "a resident grid must not be refetched on the toggle",
        );
    }
}
