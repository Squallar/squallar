//! The MRMS national mosaic layer.
//!
//! Shaped after [`super::model`] — a gridded field, held whole, cut to the
//! viewport at encode — with two differences that are the whole content of this
//! file:
//!
//! * **the cache is bounded by bytes, not entries.** One CONUS grid is 98 MB,
//!   so `crate::mrms::GRID_CACHE_BYTES` is the budget and the entry count falls
//!   out of it;
//! * **the raster input carries no source enum.** `prepare_job` describes a
//!   [`rasterize::GriddedInput::Resident`], which is the field-identified carry
//!   the gridded substrate introduced, so this layer rides the existing
//!   `overlay/model` codec row rather than adding a byte-identical second wire
//!   form. `texture_tests::raster_input_owner` is where that sharing is stated.
//!
//! `TimeAxis::Live`, deliberately. MRMS publishes stamped granules every two
//! minutes and *could* be a frame series, but exactly two layers declare
//! `FrameSeries` today and `sources.rs`'s
//! `radar_takes_the_clock_wherever_it_is_drawn` says in as many words that a
//! third changes which layer a pane's clock follows and must be **ruled on,
//! not absorbed**. This layer draws the latest thing it fetched.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;

use crate::fetch_policy::Whole;
use crate::mrms::{GRID_CACHE_BYTES, MrmsFetchResult, MrmsGrid, MrmsProduct};
use crate::render::controls::{
    ControlButton, ControlEffect, ControlItem, ControlUpdate, ControlValue,
};
use crate::render::overlay_state::{
    FetchConfig, FetchPayload, FetchTask, OverlayHandler, OverlayLegend, OverlayState, PaneMut,
    PaneRef, RasterizeContext, RenderMode, Signed, Surface,
};
use crate::render::rasterize;
use rustdar_source::id::{LayerId, known};
use rustdar_source::job::{DescribedJob, JobCodec};

/// The resident mosaics, bounded by **bytes** and evicted least-recently-used
/// first.
///
/// An entries map plus a recency list holding exactly its keys, oldest use
/// first; both private so no caller can desynchronise them. The list is behind a
/// `RefCell` because every *reader* reaches it through an `&self` method of
/// [`OverlayHandler`], and a lookup that did not count as a use would let the
/// product on screen age out.
///
/// An entry count would be the wrong instrument here for a reason the model's
/// six-entry cache does not have: an MRMS grid is thirteen times an HRRR grid,
/// so "six" would mean 588 MB on a phone and 588 MB in a browser tab.
struct MrmsGridCache {
    entries: HashMap<MrmsProduct, Arc<MrmsGrid>>,
    recency: RefCell<Vec<MrmsProduct>>,
    /// **Injected, not read from the constant.** The shipped handler passes
    /// [`GRID_CACHE_BYTES`]; a test passes a budget it can actually overflow.
    /// A cache whose only budget was 98 MB × 4 could not have its eviction
    /// policy exercised at all, and an untested eviction policy is how a cache
    /// settles at one entry and every other pane stops drawing.
    budget: usize,
}

impl MrmsGridCache {
    fn new(budget: usize) -> Self {
        Self {
            entries: HashMap::new(),
            recency: RefCell::new(Vec::new()),
            budget,
        }
    }

    fn touch(&self, product: MrmsProduct) {
        let mut recency = self.recency.borrow_mut();
        if let Some(pos) = recency.iter().position(|p| *p == product) {
            recency.remove(pos);
            recency.push(product);
        }
    }

    fn get(&self, product: MrmsProduct) -> Option<&Arc<MrmsGrid>> {
        let grid = self.entries.get(&product)?;
        self.touch(product);
        Some(grid)
    }

    /// Whether `product`'s mosaic is resident, marking it most-recently-used.
    ///
    /// Every read counts as a use, including the bare predicate: which accessor
    /// a caller reached for is not a fact about what the user is looking at.
    fn contains(&self, product: MrmsProduct) -> bool {
        self.get(product).is_some()
    }

    /// Bytes of decoded values currently held — the figure the budget is spent
    /// against, summed rather than estimated.
    fn resident_bytes(&self) -> usize {
        self.entries.values().map(|g| g.resident_bytes()).sum()
    }

    /// Neither the entry going in nor anything in `pinned` is ever evicted.
    ///
    /// `pinned` is the **union** of every pane's selected product, not one
    /// pane's: this cache is shared, and evicting what another pane is showing
    /// to make room is the cross-pane collision the pane state exists to
    /// prevent. Below the budget's capacity a miss costs a **picture** rather
    /// than a refetch — `prepare_job` answers `None` and the pane goes on
    /// drawing its last texture with nothing that will re-ask — so the pin is
    /// what keeps a visible pane drawn when an arrival lands mid-cycle.
    fn insert(&mut self, product: MrmsProduct, grid: Arc<MrmsGrid>, pinned: &[MrmsProduct]) {
        if self.entries.insert(product, grid).is_some() {
            self.touch(product);
        } else {
            self.recency.borrow_mut().push(product);
        }
        while self.resident_bytes() > self.budget {
            let victim = {
                let mut recency = self.recency.borrow_mut();
                let Some(pos) = recency
                    .iter()
                    .position(|p| *p != product && !pinned.contains(p))
                else {
                    // Every remaining entry is the arrival or pinned. Going
                    // over budget is the lesser failure: dropping a pinned
                    // product blanks a pane that has nothing to re-ask.
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
}

/// One pane's MRMS state — the whole of what "reopen is 1:1" means for this
/// layer, and the whole of what `serialize_pane_state` writes.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct MrmsPaneState {
    enabled: bool,
    selected_product: MrmsProduct,
}

impl MrmsPaneState {
    /// A pane that has saved nothing, with `enabled` supplied by the pane's own
    /// slot flag.
    fn new(enabled: bool) -> Self {
        Self {
            enabled,
            selected_product: MrmsProduct::ReflectivityComposite,
        }
    }
}

pub(crate) struct MrmsHandler {
    pub state: OverlayState<Option<Arc<MrmsGrid>>, Whole>,
    /// **The registry's own copy**, used only where no pane is supplied; every
    /// answer prefers [`PaneRef::state`] when there is one.
    pub defaults: MrmsPaneState,
    cached_grids: MrmsGridCache,
    pub last_error: Option<String>,
}

impl MrmsHandler {
    pub fn new() -> Self {
        Self {
            state: OverlayState::new(),
            defaults: MrmsPaneState::new(false),
            cached_grids: MrmsGridCache::new(GRID_CACHE_BYTES),
            last_error: None,
        }
    }

    fn view<'a>(&'a self, pane: &PaneRef<'a>) -> &'a MrmsPaneState {
        pane.state_as::<MrmsPaneState>().unwrap_or(&self.defaults)
    }

    fn edit(&mut self, pane: &mut PaneMut<'_>, f: impl FnOnce(&mut MrmsPaneState)) {
        match pane.state_as::<MrmsPaneState>() {
            Some(state) => f(state),
            None => f(&mut self.defaults),
        }
    }

    /// **Every product some pane is showing**, deduplicated — what the shared
    /// cache must not evict.
    fn pinned_products(&self, pane: &PaneRef<'_>) -> Vec<MrmsProduct> {
        let mut pinned: Vec<MrmsProduct> = Vec::new();
        for state in pane.all_as::<MrmsPaneState>() {
            if !pinned.contains(&state.selected_product) {
                pinned.push(state.selected_product);
            }
        }
        if pinned.is_empty() {
            pinned.push(self.defaults.selected_product);
        }
        pinned
    }
}

impl OverlayHandler for MrmsHandler {
    /// The MRMS products this layer offers, projected into the substrate's read
    /// contract by [`crate::mrms::fields`].
    fn products(&self) -> &'static [rustdar_source::product::ProductSpec] {
        crate::mrms::fields::products()
    }

    /// The product dropdown: its option values are the products' `as_str()`
    /// spellings, which are exactly the `FieldId`s [`crate::mrms::fields`]
    /// registers, so a catalogue tile's id goes straight through
    /// `apply_control`.
    fn field_control_id(&self) -> Option<&'static str> {
        Some("product")
    }

    /// **This pane's own product**, projected through its registry row — never
    /// spelled as a fresh string, so the id can only ever be one this layer
    /// publishes.
    fn current_field(&self, pane: &PaneRef<'_>) -> Option<rustdar_source::product::FieldId> {
        Some(
            crate::mrms::fields::spec(self.view(pane).selected_product)
                .id
                .clone(),
        )
    }

    fn id(&self) -> LayerId {
        known::MRMS
    }

    fn surface(&self) -> Surface {
        Surface::Ground
    }

    /// **15**: above the model's 10 and below the outlooks' 20. A national
    /// mosaic covers a model field and is covered by the risk polygons drawn
    /// over both.
    fn draw_order_weight(&self) -> u32 {
        15
    }

    fn display_name(&self) -> &str {
        "MRMS Mosaic"
    }

    fn render_mode(&self) -> RenderMode {
        RenderMode::Texture
    }

    /// Nothing here is hatched or theme-coloured: the bar is the product's own
    /// and reads the same on either background.
    fn theme_sensitive(&self) -> bool {
        false
    }

    fn is_enabled(&self, pane: &PaneRef<'_>) -> bool {
        self.view(pane).enabled
    }

    fn set_enabled(&mut self, enabled: bool, pane: &mut PaneMut<'_>) {
        self.edit(pane, |state| state.enabled = enabled);
    }

    fn status_line(&self, pane: &PaneRef<'_>) -> Option<String> {
        let view = self.view(pane);
        if !view.enabled {
            return None;
        }
        Some(view.selected_product.display_name().to_owned())
    }

    fn data_generation(&self) -> u64 {
        self.state.data_generation
    }

    /// **The selected product is in the token**, not just the fetch counter:
    /// the render dispatch groups panes by this, and one token for two products
    /// is one raster for both.
    fn content_signature(&self, pane: &PaneRef<'_>) -> u64 {
        self.data_generation() ^ (self.view(pane).selected_product as u64 + 1).rotate_left(32)
    }

    fn has_data(&self, pane: &PaneRef<'_>) -> bool {
        self.cached_grids.contains(self.view(pane).selected_product)
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
        self.state
            .data
            .as_ref()
            .map(|d| d.grid.values.len())
            .unwrap_or(0)
    }

    /// **120 s**, matching the mosaic's own ~2-minute publish cadence. Faster
    /// would list a prefix that has not changed; slower would draw a mosaic
    /// older than the radar beside it.
    fn auto_poll_interval(&self) -> Option<u64> {
        Some(120)
    }

    fn clickable_items<'a>(
        &'a self,
        _pane: &PaneRef<'_>,
    ) -> Vec<crate::render::overlay_state::ClickableItem<'a>> {
        Vec::new() // Gridded, not feature-based; hover uses `hover_value_at`.
    }

    fn apply_fetch_result(&mut self, result: FetchPayload, pane: &PaneRef<'_>) {
        let Some(fetch) = self.state.downcast_round::<MrmsFetchResult>(result) else {
            log::error!("MRMS handler received unexpected fetch result type");
            return;
        };
        match fetch.0 {
            Ok(grid) => {
                log::info!(
                    "Received MRMS {} valid {}: {}×{} grid, {} drawable points",
                    grid.product.display_name(),
                    grid.valid,
                    grid.grid.ni,
                    grid.grid.nj,
                    grid.visible_points,
                );
                if let Some(notice) = grid.blank_notice() {
                    log::info!("MRMS: {notice}");
                }
                let product = grid.product;
                let arc = Arc::new(grid);
                let pinned = self.pinned_products(pane);
                self.cached_grids.insert(product, arc.clone(), &pinned);
                self.state.set_data(Some(arc));
                self.last_error = None;
            }
            Err(e) => {
                log::error!("MRMS fetch failed: {e}");
                self.last_error = Some(e.message.clone());
                self.state.record_failure(&e);
            }
        }
    }

    fn retain_selections(
        &self,
        _selections: &mut Vec<Arc<dyn crate::render::overlay_state::OverlayItem>>,
        _pane: &PaneRef<'_>,
    ) {
    }

    /// Nearest neighbour, not interpolation: the mosaic is 0.01° (~1 km), finer
    /// than a tooltip needs.
    ///
    /// **A no-coverage point answers `None`**, because
    /// [`crate::mrms::decode::to_reading`] made it `NaN` and
    /// [`MrmsProduct::format_value`] formats a non-finite reading as nothing.
    /// That is what stops the tooltip claiming "−999.0 dBZ" over the ocean.
    fn hover_value_at(&self, lat: f64, lon: f64, pane: &PaneRef<'_>) -> Option<String> {
        let grid = self.cached_grids.get(self.view(pane).selected_product)?;
        if !grid.bounds.contains_point(lat, lon) {
            return None;
        }
        let index = grid.grid.coords.nearest(lat, lon)?;
        let (glat, glon) = grid.grid.coords.at(index)?;
        let value = *grid.grid.values.get(index)?;
        let (dlat, dlon) = (glat - lat, glon - lon);
        // ~0.02°, two cells of a 0.01° grid.
        if dlat * dlat + dlon * dlon > 0.02 * 0.02 {
            return None;
        }
        let text = grid.product.format_value(value);
        if text.is_empty() { None } else { Some(text) }
    }

    /// The bar is a pure function of the selected product, so the signature is
    /// the product and nothing else — deliberately **not** `data_generation`,
    /// which every two-minute poll bumps. `+ 1` keeps the first product off `0`.
    fn legend(&self, pane: &PaneRef<'_>) -> Option<Signed<OverlayLegend>> {
        let view = self.view(pane);
        if !view.enabled {
            return None;
        }
        let spec = crate::mrms::fields::spec(view.selected_product);
        Some(Signed {
            signature: view.selected_product as u64 + 1,
            items: OverlayLegend {
                thresholds: spec.scale.thresholds.clone(),
                is_gradient: spec.scale.is_gradient,
                min_value: spec.scale.min_value,
                max_value: spec.scale.max_value,
                unit_label: view.selected_product.unit_label(),
            },
        })
    }

    /// The [`Resident`](rasterize::GriddedInput::Resident) carry: an `Arc` clone
    /// of the resident mosaic, so describing the job costs a refcount and the
    /// 98 MB never moves. The values memcpy happens only in the web encoder,
    /// which knows the texture's bounds and writes the window's rows alone.
    fn prepare_job(&self, _ctx: &RasterizeContext, pane: &PaneRef<'_>) -> Option<DescribedJob> {
        let grid = self.cached_grids.get(self.view(pane).selected_product)?;
        Some(DescribedJob::new(rasterize::GriddedInput::Resident(
            Arc::clone(&grid.grid),
        )))
    }

    /// **The gridded row, shared with the model layer.** Both describe a
    /// `GriddedInput`, which carries a `FieldId` rather than either source's own
    /// enum, so one wire form serves both and MRMS adds no codec row and no
    /// digest change. `texture_tests::raster_input_owner` states the sharing;
    /// `LABEL` stays `"overlay/model"` because it is a wire code, and renaming a
    /// wire code renumbers shipped clients.
    fn job_codec(&self) -> Option<&'static JobCodec> {
        crate::render::jobs::JOB_CODECS
            .iter()
            .find(|row| row.label == "overlay/model")
    }

    fn create_fetch_tasks(&self, ctx: &FetchConfig, pane: &PaneRef<'_>) -> Vec<FetchTask> {
        let client = ctx.client.clone();
        let sources = ctx.sources.clone();
        let product = self.view(pane).selected_product;
        vec![FetchTask {
            kind: known::MRMS,
            future: Box::pin(async move {
                let result = crate::mrms::fetch::fetch_latest(&client, &sources, product).await;
                Box::new(result) as FetchPayload
            }),
        }]
    }

    fn controls(&self, pane: &PaneRef<'_>) -> Vec<ControlItem> {
        let view = self.view(pane);
        let grid = self.cached_grids.get(view.selected_product);

        // The mosaic's own valid time, not the fetch time: a two-minute cadence
        // means "updated 10s ago" and "valid 00:04z" are different facts and
        // the second is the one on screen.
        let label = match grid {
            Some(g) => format!("MRMS Mosaic ({})", g.valid.format("%H:%Mz")),
            None => "MRMS Mosaic".to_string(),
        };

        let mut items = vec![ControlItem::Toggle {
            id: "enabled",
            label,
            enabled: view.enabled,
        }];

        // Ungated on `enabled`: a hidden layer's options stay visible and
        // editable, Refresh still fetches, and the status lines keep reporting.
        items.push(ControlItem::Dropdown {
            id: "product",
            label: "Product".into(),
            options: MrmsProduct::all()
                .iter()
                .map(|p| (p.as_str().into(), p.display_name().into()))
                .collect(),
            selected: view.selected_product.as_str().into(),
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
        if let Some(notice) = grid.and_then(|g| g.blank_notice()) {
            items.push(ControlItem::InfoText { text: notice });
        }

        items
    }

    fn apply_control(&mut self, update: &ControlUpdate, pane: &mut PaneMut<'_>) -> ControlEffect {
        match update.id {
            "enabled" => {
                if let ControlValue::Bool(val) = update.value {
                    self.edit(pane, |state| state.enabled = val);
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
            "product" => {
                if let ControlValue::String(ref val) = update.value
                    && let Ok(new_product) = val.parse::<MrmsProduct>()
                    && new_product != self.view(&pane.as_ref()).selected_product
                {
                    self.edit(pane, |state| state.selected_product = new_product);
                    // A resident product needs no network; bump the generation
                    // so the pane re-rasterizes what is already in hand.
                    if self.cached_grids.contains(new_product) {
                        self.state.data_generation = self.state.data_generation.wrapping_add(1);
                        return ControlEffect::None;
                    }
                    return ControlEffect::Fetch;
                }
                ControlEffect::None
            }
            "refresh" => ControlEffect::Fetch,
            _ => ControlEffect::None,
        }
    }

    // ── Per-pane state ────────────────────────────────────

    fn create_pane_state(&self, enabled: bool) -> Option<FetchPayload> {
        Some(Box::new(MrmsPaneState::new(enabled)))
    }

    fn deserialize_pane_state(
        &self,
        value: serde_json::Value,
        enabled: bool,
    ) -> Option<FetchPayload> {
        let mut state = MrmsPaneState::new(enabled);
        if let Some(on) = value.get("enabled").and_then(|v| v.as_bool()) {
            state.enabled = on;
        }
        if let Some(product) = value
            .get("product")
            .and_then(|v| v.as_str())
            .and_then(|s| s.parse().ok())
        {
            state.selected_product = product;
        }
        Some(Box::new(state))
    }

    fn serialize_pane_state(&self, state: &dyn std::any::Any) -> serde_json::Value {
        let Some(state) = state.downcast_ref::<MrmsPaneState>() else {
            return serde_json::Value::Null;
        };
        serde_json::json!({
            "enabled": state.enabled,
            "product": state.selected_product.as_str(),
        })
    }
}

#[cfg(test)]
mod tests;
