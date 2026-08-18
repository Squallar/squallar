use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;

use crate::fetch_policy::Whole;
use crate::hrrr::{HrrrFetchResult, HrrrGridData, ModelParameter};
use crate::render::controls::{
    ControlButton, ControlEffect, ControlItem, ControlUpdate, ControlValue, PaneControlContext,
    PaneControlContextMut,
};
use crate::render::overlay_state::{
    FetchConfig, FetchPayload, FetchTask, HandlerJobInput, OverlayHandler, OverlayKind,
    OverlayLegend, OverlayState, RasterizeContext, RenderMode,
};
use crate::render::rasterize;

/// How many parameters' grids stay resident at once.
///
/// # Sized by the pane count, not by a memory target
///
/// Panes configure this overlay independently: `PaneState::overlay_configs`
/// holds a serialized handler snapshot per pane and is swapped into the registry
/// around each access, and `deduplicate_overlay_renders` hands every unlinked
/// pane its own render request. So six panes can sit on six different
/// parameters, and each one's render loads that pane's config and then asks this
/// cache for the parameter that pane selected.
///
/// Below the pane count, a miss does not cost a refetch — it costs a **picture**.
/// `prepare_job` answers `None`, `app_fetch` clears the render marks and
/// returns, and the pane goes on drawing its last texture at its old bounds with
/// nothing in the code that will re-ask: [`OverlayHandler::auto_fetch_delay`]
/// reads the handler-global `fetch_time` that *any* parameter's successful fetch
/// stamps, [`OverlayHandler::create_fetch_tasks`] only ever fetches
/// `selected_param`, and the enable-fetch rule fires on a toggle. Re-picking the
/// starved pane's parameter just evicts another pane's.
///
/// So the cap is `rustdar_egui::pane::MAX_PANES_DESKTOP`, which is **6**
/// (`MAX_PANES_MOBILE` is 4, under it). It is spelled rather than imported
/// because the dependency runs rustdar-egui → rustdar-overlays and cannot run
/// back; if the pane maximum ever rises, this has to rise with it.
///
/// # What that costs
///
/// One object at a time, each figure naming the object it describes:
///
/// - **The values vector.** `HrrrGridData::values` is a `Vec<f32>` over HRRR
///   CONUS's 1,905,141 points: **7.62 MB** per resident grid. This is the whole
///   per-grid cost whenever the coordinates are [`GridCoords::Lambert`], which
///   is every HRRR fetch this crate makes — HRRR is GRIB2 template 3.30 for
///   every field, and `LambertGrid` is a fixed-size `Copy` struct of projection
///   constants rather than anything that scales with the point count.
/// - **The coordinate arrays.** A second cost only on the
///   [`GridCoords::Explicit`] arm, where two materialised `Vec<f64>` over those
///   same 1,905,141 points add **30.5 MB**, for a worst-case resident grid of
///   **38.1 MB**. No HRRR fetch reaches that arm; it is what a non-3.30 source
///   would decode to, and what these tests build.
///
/// [`ModelParameter::all`] has 16 members and the Parameter dropdown walks
/// them, so with no eviction the map held one grid per parameter: **122 MB** of
/// values on the arm this app actually takes, **610 MB** if every entry carried
/// explicit coordinates — on wasm32's 4 GiB address space, or Android's hard
/// per-app cap. Six entries bound those at **45.7 MB** and **228.6 MB**, i.e.
/// 2.7× under the unbounded figure on either arm. A tighter cap would save more
/// and is not available: the pane count is a floor, not a preference.
///
/// [`GridCoords::Lambert`]: crate::hrrr::GridCoords::Lambert
/// [`GridCoords::Explicit`]: crate::hrrr::GridCoords::Explicit
const MODEL_GRID_CACHE_ENTRIES: usize = 6;

// At a cap of 1, an insert of a parameter that is not the selected one protects
// both the arrival and the pin, the `else { break }` in `insert` fires, and the
// cache settles at two entries for ever — a bound that silently does not hold.
// Two is where the eviction loop is guaranteed a victim.
const _: () = assert!(MODEL_GRID_CACHE_ENTRIES >= 2);

/// The resident grids, bounded by [`MODEL_GRID_CACHE_ENTRIES`], evicted
/// least-recently-touched first.
///
/// Shaped after `rustdar_frontend::render_dispatch::RenderCache`: an entries
/// map plus a recency list holding exactly the keys of `entries`, each exactly
/// once, oldest use first. Every method that touches one touches the other, and
/// both fields are private so no caller can desynchronise them.
///
/// The recency list is behind a `RefCell` because every *reader* of a grid
/// reaches it through an `&self` method of [`OverlayHandler`] —
/// `hover_value_at`, `prepare_job`, `controls`, `has_data`. `RenderCache`
/// takes `&mut self` in `get` for exactly this reason, "a lookup that did not
/// count as a use would let the pane currently on screen age out while an
/// unwatched one survived"; here the trait forbids `&mut self`, so the
/// mutability moves inside rather than the rule being dropped. There are at
/// most 16 parameters, so a linear scan of a `Vec` is the whole cost and an
/// `lru` dependency would be heavier than the thing it replaced.
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

    /// Move `param` to the most-recently-used end. No-op if absent.
    fn touch(&self, param: ModelParameter) {
        let mut recency = self.recency.borrow_mut();
        if let Some(pos) = recency.iter().position(|p| *p == param) {
            recency.remove(pos);
            recency.push(param);
        }
    }

    /// Look up a grid, marking it most-recently-used.
    fn get(&self, param: ModelParameter) -> Option<&Arc<HrrrGridData>> {
        let grid = self.entries.get(&param)?;
        self.touch(param);
        Some(grid)
    }

    /// Whether `param`'s grid is resident, marking it most-recently-used.
    ///
    /// Every read counts as a use, including the bare predicate: which accessor
    /// a caller happened to reach for is not a fact about what the user is
    /// looking at, and making eviction depend on it is how the grid on screen
    /// ends up the oldest thing in the list.
    fn contains(&self, param: ModelParameter) -> bool {
        self.get(param).is_some()
    }

    /// Install `param`'s grid, evicting least-recently-touched entries until
    /// the map is within [`MODEL_GRID_CACHE_ENTRIES`].
    ///
    /// Neither the entry going in nor `pinned` is ever the one evicted:
    /// evicting the arrival would make the fetch that just landed pointless,
    /// and evicting what is on screen would blank a layer the user is looking
    /// at.
    ///
    /// `pinned` is the registry's `selected_param` **at the moment the payload
    /// arrives**, which with several panes is whichever pane's config
    /// `load_pane_configs` swapped in last — not necessarily the pane that
    /// asked for this fetch. So it protects *a* visible parameter rather than
    /// every visible one; the guarantee that all of them stay resident comes
    /// from the cap being at least the pane count, not from this argument.
    ///
    /// With at most two protected keys and a cap of at least two, there is
    /// always a victim, so the `break` is structural insurance rather than a
    /// case that runs.
    fn insert(&mut self, param: ModelParameter, grid: Arc<HrrrGridData>, pinned: ModelParameter) {
        if self.entries.insert(param, grid).is_some() {
            // A re-fetch of a parameter already resident replaces its own key,
            // so a superseded run's grid is dropped here rather than aging out
            // later beside its own replacement. Nothing in this cache is keyed
            // by run: a stale-run grid of an *unselected* parameter is an
            // ordinary entry, and being unselected is exactly what walks it to
            // the evictable end of the list.
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

    /// Resident grid count.
    #[cfg(test)]
    fn len(&self) -> usize {
        self.entries.len()
    }

    /// Residency **without** counting as a use, so an assertion cannot change
    /// the order it is about to assert on.
    #[cfg(test)]
    fn is_resident(&self, param: ModelParameter) -> bool {
        self.entries.contains_key(&param)
    }

    /// Oldest use first.
    #[cfg(test)]
    fn recency_order(&self) -> Vec<ModelParameter> {
        self.recency.borrow().clone()
    }
}

pub(crate) struct ModelDataHandler {
    pub state: OverlayState<Option<Arc<HrrrGridData>>, Whole>,
    pub enabled: bool,
    pub selected_param: ModelParameter,
    /// Keyed per parameter so different panes can show different ones, and
    /// bounded so cycling the dropdown cannot walk off the end of a phone's
    /// memory; see [`ModelGridCache`].
    cached_grids: ModelGridCache,
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
            cached_grids: ModelGridCache::new(),
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
                // The verdict comes with the error now, merged across the two
                // candidate runs by `hrrr::fetch::round_verdict`. It used to be
                // hardcoded `transient` on the argument that "another run can
                // fix it" — true of a missed forecast hour, and false of a
                // moved product, which then stayed on the poll for ever.
                //
                // A run the bucket does not carry yet classifies as `Absent`,
                // which is the honest reading and keeps the layer on its
                // ordinary hourly interval rather than climbing a ladder.
                self.last_error = Some(e.message.clone());
                self.state.record_failure(&e);
            }
        }
    }

    fn retain_selections(
        &self,
        _selections: &mut Vec<Arc<dyn crate::render::overlay_state::OverlayItem>>,
    ) {
        // No selectable items.
    }

    fn hover_value_at(&self, lat: f64, lon: f64) -> Option<String> {
        let grid = self.cached_grids.get(self.selected_param)?;
        if !grid.bounds.contains_point(lat, lon) {
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

    /// The [`Whole`](rasterize::ModelDataInput::Whole) carry: an `Arc` clone
    /// of the selected parameter's resident grid, so describing the job costs
    /// a refcount here and the values memcpy — the window cut — happens only
    /// where bytes must be built anyway, in the web encoder that knows the
    /// texture's bounds. The `Arc` never crosses a port; see
    /// [`rasterize::ModelDataInput`].
    fn prepare_job(&self, _ctx: &RasterizeContext) -> Option<HandlerJobInput> {
        let grid = self.cached_grids.get(self.selected_param)?.clone();
        Some(HandlerJobInput::ModelData(
            rasterize::ModelDataInput::Whole(grid),
        ))
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
                        // Cached parameters re-render on a generation bump
                        // alone; no refetch.
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
        h.apply_fetch_result(Box::new(HrrrFetchResult(Err(
            crate::fetch_policy::FetchError::transient("HTTP 500"),
        ))));

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

    // ── Cache bound ───────────────────────────────────────────────────────

    /// The parameters these tests fill the cache with, in fetch order: exactly
    /// enough to fill it, plus one more to overflow it.
    ///
    /// Taken from [`ModelParameter::all`] so the set follows
    /// [`MODEL_GRID_CACHE_ENTRIES`], rather than a hand-written list that would
    /// silently stop overflowing anything the day the cap rose.
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

    /// The keys a full cache holds, oldest use first.
    fn resident_order() -> &'static [ModelParameter] {
        &fill_order()[..MODEL_GRID_CACHE_ENTRIES]
    }

    /// The least recently touched entry of a freshly filled cache — what an
    /// overflowing insert must take.
    fn oldest() -> ModelParameter {
        fill_order()[0]
    }

    /// The one behind it, which an overflow must take instead when `oldest` is
    /// spared.
    fn next_oldest() -> ModelParameter {
        fill_order()[1]
    }

    /// One parameter past the cap, so fetching it overflows a full cache.
    fn overflow() -> ModelParameter {
        fill_order()[MODEL_GRID_CACHE_ENTRIES]
    }

    /// A fetch for `parameter` landing, exactly as the fetch path delivers one.
    fn deliver(h: &mut ModelDataHandler, parameter: ModelParameter) {
        h.apply_fetch_result(Box::new(HrrrFetchResult(Ok(grid(parameter, vec![300.0])))));
    }

    fn rasterize_ctx() -> RasterizeContext {
        RasterizeContext {
            is_dark: false,
            zoom: 5.0,
            device_scale: 1.0,
            // A literal: nothing on the model grid's raster path reads a
            // clock, which is exactly what keeps this fixture deterministic.
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

    /// A cache filled exactly to the cap, each parameter selected and then
    /// fetched, so the recency list is [`resident_order`], oldest use first.
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

    /// A full desktop layout is `rustdar_egui::pane::MAX_PANES_DESKTOP` = 6
    /// unlinked panes, each free to select its own parameter, each rendered from
    /// its own config. Every one of them must find its grid here.
    ///
    /// This is the case a cap below the pane count breaks, and it breaks
    /// *silently*: `prepare_job` answers `None`, `app_fetch` clears the
    /// render marks and returns, and the starved pane goes on drawing its last
    /// texture at its old bounds. Nothing re-asks — `auto_fetch_delay` reads the
    /// handler-global `fetch_time` that any parameter's fetch stamps, and
    /// `create_fetch_tasks` only ever fetches `selected_param` — so unlike an
    /// ordinary eviction this costs no refetch and no gesture, just a wrong
    /// picture that persists.
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

        // Each pane's render swaps that pane's config in, then rasterizes.
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

    /// Fails if the map grows past the cap, and names which grid should have
    /// gone: unbounded, this held one 7.62 MB values vector per parameter — plus
    /// 30.5 MB of coordinates apiece on the `Explicit` arm these fixtures use.
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
    /// sixteen grids. The count is asserted to be *exactly* what is resident at
    /// every step, not merely under the cap: "never exceeds the cap" is
    /// satisfied by a cache that never holds anything at all.
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

    /// Every `&self` reader of a grid must count as a use. If one does not, the
    /// parameter on screen ages out while one nobody has looked at survives.
    ///
    /// Each step reads the parameter that is currently *oldest* and requires
    /// that read to have moved it to the most-recent end, so no step can pass by
    /// touching nothing. Each step also asserts the read actually answered, so a
    /// reader that silently returned `None` could not pass either.
    #[test]
    fn every_read_path_counts_as_a_use() {
        // The fixture requirement, stated so that lowering the cap fails the
        // build rather than quietly weakening this test into one that walks
        // fewer parameters than it claims to. `const` rather than a runtime
        // `assert!`, which clippy reads as a constant-valued assertion.
        const _: () = assert!(
            MODEL_GRID_CACHE_ENTRIES >= 3,
            "this test walks three distinct parameters through the cache",
        );
        let mut h = full_cache();

        // A read has counted as a use when the key it read is now the most
        // recent, i.e. last. Each step reads the key that was first, so this is
        // a move across the whole list and not a coincidence of position.
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

        // Picking a cached parameter out of the dropdown: no refetch, but it is
        // the strongest signal there is that the user wants it kept.
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

    /// The grid the user hovered must outlive one that was only ever fetched.
    /// Without the hover counting as a use, `oldest` is still the oldest entry
    /// and is exactly what the overflowing insert takes.
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
    /// thing in the list: a bare assignment to `selected_param` is what
    /// `deserialize_state` does when `load_pane_configs` swaps a pane in, so a
    /// pane can sit on a grid nothing has touched since. Evicting it would blank
    /// the layer under the user.
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
    /// run costs nothing and evicts nobody — the cache carries no run identity,
    /// and a stale-run grid of an unselected parameter is just an ordinary entry
    /// aging towards the evictable end.
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
    /// that has to happen: an evicted parameter has no data, so re-enabling the
    /// layer must re-ask rather than draw nothing. The counterpart is asserted
    /// too, or the test would pass for a handler that refetches on every toggle.
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
