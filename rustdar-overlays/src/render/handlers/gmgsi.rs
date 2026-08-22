//! The GMGSI global satellite mosaic layer.
//!
//! Shaped after [`super::mrms`], which is itself shaped after [`super::model`]:
//! a gridded field, held whole, cut to the viewport at encode. The two
//! differences from MRMS are the ones the source forced:
//!
//! * **the key is a channel, not a product code.** Four channels, each its own
//!   granule and its own colour bar, selected by one dropdown;
//! * **the cache holds the raster alone.** [`crate::gmgsi::decode::GmgsiGrid`]
//!   carries its [`ResidentGrid`] by value, so the arrival is destructured once
//!   and the raster moved into an `Arc` — after that a described job costs a
//!   refcount and the 60 MB never moves again.
//!
//! **No codec row and no wire label.** `prepare_job` describes a
//! [`rasterize::GriddedInput::Resident`], the field-identified carry the gridded
//! substrate introduced, so this layer rides `overlay/model` exactly as MRMS
//! does. `texture_tests::raster_input_owner` is where that sharing is stated,
//! and `rustdar-worker`'s `WIRE_FRAMING_ROWS` is untouched by this layer.
//!
//! `TimeAxis::Live`, by taking the trait's default. GMGSI publishes a stamped
//! granule every hour and *could* be a frame series, but `sources.rs`'s
//! `radar_takes_the_clock_wherever_it_is_drawn` says a third `FrameSeries`
//! layer changes which layer a pane's clock follows and must be **ruled on, not
//! absorbed**. This layer draws the latest thing it fetched.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;

use crate::fetch_policy::Whole;
use crate::gmgsi::{GmgsiChannel, GmgsiFetchResult};
use crate::render::controls::{
    ControlButton, ControlEffect, ControlItem, ControlUpdate, ControlValue,
};
use crate::render::gridded::ResidentGrid;
use crate::render::overlay_state::{
    FetchConfig, FetchPayload, FetchTask, OverlayHandler, OverlayLegend, OverlayState, PaneMut,
    PaneRef, RasterizeContext, RenderMode, Signed, Surface,
};
use crate::render::rasterize;
use rustdar_geo::GeoBounds;
use rustdar_source::id::{LayerId, known};
use rustdar_source::job::{DescribedJob, JobCodec};

/// One mosaic's values, in bytes: 3000 x 5000 `f32` = **60 MB**.
///
/// Stated as the product's own shape rather than as a round number of
/// megabytes, so "one resident channel" stays one resident channel if the grid
/// ever changes shape.
pub const GLOBAL_GRID_BYTES: usize = 3000 * 5000 * std::mem::size_of::<f32>();

/// How many bytes of decoded GMGSI raster may stay resident at once: **one
/// channel on wasm, two on mobile, four on desktop**.
///
/// **A byte budget, not an entry count**, for the reason
/// [`crate::mrms::GRID_CACHE_BYTES`] states: all four channels resident is
/// 240 MB, which is not a figure a browser tab has spare beside a `px_coords`
/// buffer and a texture.
///
/// Spelled as a `cfg` cascade rather than resolved from `rustdar-device-profile`
/// because that crate sits **above** this one in the crate graph
/// (`ARCHITECTURE.md` §1), so the dependency cannot run back.
#[cfg(target_arch = "wasm32")]
pub const GRID_CACHE_BYTES: usize = GLOBAL_GRID_BYTES;
/// See the wasm arm.
#[cfg(all(
    not(target_arch = "wasm32"),
    any(target_os = "android", target_os = "ios")
))]
pub const GRID_CACHE_BYTES: usize = 2 * GLOBAL_GRID_BYTES;
/// See the wasm arm.
#[cfg(all(
    not(target_arch = "wasm32"),
    not(any(target_os = "android", target_os = "ios"))
))]
pub const GRID_CACHE_BYTES: usize = 4 * GLOBAL_GRID_BYTES;

// **A build failure, not a test failure**: every term is a compile-time
// constant, so a runtime assertion over them could not fail on a build that got
// as far as running tests — the arm that would be wrong is the one a *different*
// target selects, and only the compiler ever sees that.
//
// A budget under one grid settles the cache empty: the arrival evicts itself,
// `prepare_job` answers `None` for ever and every pane draws its last texture.
const _: () = assert!(GRID_CACHE_BYTES >= GLOBAL_GRID_BYTES);
const _: () = assert!(GRID_CACHE_BYTES.is_multiple_of(GLOBAL_GRID_BYTES));
const _: () = assert!(GLOBAL_GRID_BYTES == 60_000_000);

/// One decoded channel, as the layer holds it.
///
/// The raster is behind an `Arc` and everything else is a scalar: describing a
/// job must cost a refcount, never a 60 MB copy. `crate::gmgsi::decode` hands
/// its `ResidentGrid` over by value, so the `Arc` is made once, here, out of a
/// move.
struct GmgsiGranule {
    grid: Arc<ResidentGrid>,
    bounds: GeoBounds,
    /// `time_coverage_start` — the hour the blend depicts, not the hour it was
    /// fetched. On a source that lands ~40 minutes late those are different
    /// facts and the first is the one on screen.
    valid_time: chrono::NaiveDateTime,
}

impl GmgsiGranule {
    fn resident_bytes(&self) -> usize {
        self.grid.values.len() * std::mem::size_of::<f32>()
    }
}

/// The resident channels, bounded by **bytes** and evicted least-recently-used
/// first.
///
/// An entries map plus a recency list holding exactly its keys, oldest use
/// first; both private so no caller can desynchronise them. The list is behind a
/// `RefCell` because every *reader* reaches it through an `&self` method of
/// [`OverlayHandler`], and a lookup that did not count as a use would let the
/// channel on screen age out.
struct GmgsiGridCache {
    entries: HashMap<GmgsiChannel, GmgsiGranule>,
    recency: RefCell<Vec<GmgsiChannel>>,
    /// **Injected, not read from the constant.** The shipped handler passes
    /// [`GRID_CACHE_BYTES`]; a test passes a budget it can actually overflow.
    /// A cache whose only budget was 60 MB x 4 could not have its eviction
    /// policy exercised at all, and an untested eviction policy is how a cache
    /// settles at one entry and every other pane stops drawing.
    budget: usize,
}

impl GmgsiGridCache {
    fn new(budget: usize) -> Self {
        Self {
            entries: HashMap::new(),
            recency: RefCell::new(Vec::new()),
            budget,
        }
    }

    fn touch(&self, channel: GmgsiChannel) {
        let mut recency = self.recency.borrow_mut();
        if let Some(pos) = recency.iter().position(|c| *c == channel) {
            recency.remove(pos);
            recency.push(channel);
        }
    }

    fn get(&self, channel: GmgsiChannel) -> Option<&GmgsiGranule> {
        let granule = self.entries.get(&channel)?;
        self.touch(channel);
        Some(granule)
    }

    /// Whether `channel`'s mosaic is resident, marking it most-recently-used.
    ///
    /// Every read counts as a use, including the bare predicate: which accessor
    /// a caller reached for is not a fact about what the user is looking at.
    fn contains(&self, channel: GmgsiChannel) -> bool {
        self.get(channel).is_some()
    }

    /// Bytes of decoded values currently held — the figure the budget is spent
    /// against, summed rather than estimated.
    fn resident_bytes(&self) -> usize {
        self.entries.values().map(|g| g.resident_bytes()).sum()
    }

    /// Neither the entry going in nor anything in `pinned` is ever evicted.
    ///
    /// `pinned` is the **union** of every pane's selected channel, not one
    /// pane's: this cache is shared, and evicting what another pane is showing
    /// to make room is the cross-pane collision the pane state exists to
    /// prevent. Below the budget's capacity a miss costs a **picture** rather
    /// than a refetch — `prepare_job` answers `None` and the pane goes on
    /// drawing its last texture with nothing that will re-ask — so the pin is
    /// what keeps a visible pane drawn when an arrival lands mid-cycle.
    fn insert(&mut self, channel: GmgsiChannel, granule: GmgsiGranule, pinned: &[GmgsiChannel]) {
        if self.entries.insert(channel, granule).is_some() {
            self.touch(channel);
        } else {
            self.recency.borrow_mut().push(channel);
        }
        while self.resident_bytes() > self.budget {
            let victim = {
                let mut recency = self.recency.borrow_mut();
                let Some(pos) = recency
                    .iter()
                    .position(|c| *c != channel && !pinned.contains(c))
                else {
                    // Every remaining entry is the arrival or pinned. Going
                    // over budget is the lesser failure: dropping a pinned
                    // channel blanks a pane that has nothing to re-ask.
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

/// One pane's GMGSI state — the whole of what "reopen is 1:1" means for this
/// layer, and the whole of what `serialize_pane_state` writes.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct GmgsiPaneState {
    enabled: bool,
    selected_channel: GmgsiChannel,
}

impl GmgsiPaneState {
    /// A pane that has saved nothing, with `enabled` supplied by the pane's own
    /// slot flag.
    fn new(enabled: bool) -> Self {
        Self {
            enabled,
            selected_channel: GmgsiChannel::LongwaveIr,
        }
    }
}

pub(crate) struct GmgsiHandler {
    pub state: OverlayState<Option<Arc<ResidentGrid>>, Whole>,
    /// **The registry's own copy**, used only where no pane is supplied; every
    /// answer prefers [`PaneRef::state`] when there is one.
    pub defaults: GmgsiPaneState,
    cached_grids: GmgsiGridCache,
    pub last_error: Option<String>,
}

impl GmgsiHandler {
    pub fn new() -> Self {
        Self {
            state: OverlayState::new(),
            defaults: GmgsiPaneState::new(false),
            cached_grids: GmgsiGridCache::new(GRID_CACHE_BYTES),
            last_error: None,
        }
    }

    fn view<'a>(&'a self, pane: &PaneRef<'a>) -> &'a GmgsiPaneState {
        pane.state_as::<GmgsiPaneState>().unwrap_or(&self.defaults)
    }

    fn edit(&mut self, pane: &mut PaneMut<'_>, f: impl FnOnce(&mut GmgsiPaneState)) {
        match pane.state_as::<GmgsiPaneState>() {
            Some(state) => f(state),
            None => f(&mut self.defaults),
        }
    }

    /// **Every channel some pane is showing**, deduplicated — what the shared
    /// cache must not evict.
    fn pinned_channels(&self, pane: &PaneRef<'_>) -> Vec<GmgsiChannel> {
        let mut pinned: Vec<GmgsiChannel> = Vec::new();
        for state in pane.all_as::<GmgsiPaneState>() {
            if !pinned.contains(&state.selected_channel) {
                pinned.push(state.selected_channel);
            }
        }
        if pinned.is_empty() {
            pinned.push(self.defaults.selected_channel);
        }
        pinned
    }
}

impl OverlayHandler for GmgsiHandler {
    /// The GMGSI channels this layer offers, projected into the substrate's
    /// read contract by [`crate::gmgsi::fields`].
    fn products(&self) -> &'static [rustdar_source::product::ProductSpec] {
        crate::gmgsi::fields::products()
    }

    /// The channel dropdown: its option values are the channels' `as_str()`
    /// spellings, which are exactly the `FieldId`s [`crate::gmgsi::fields`]
    /// registers, so a catalogue tile's id goes straight through
    /// `apply_control`.
    fn field_control_id(&self) -> Option<&'static str> {
        Some("channel")
    }

    /// **This pane's own channel**, projected through its registry row — never
    /// spelled as a fresh string, so the id can only ever be one this layer
    /// publishes.
    fn current_field(&self, pane: &PaneRef<'_>) -> Option<rustdar_source::product::FieldId> {
        Some(
            crate::gmgsi::fields::spec(self.view(pane).selected_channel)
                .id
                .clone(),
        )
    }

    fn id(&self) -> LayerId {
        known::GMGSI
    }

    fn surface(&self) -> Surface {
        Surface::Ground
    }

    /// **5**: below the model's 10, and the lowest weight any layer claims. A
    /// global cloud mosaic is the backdrop everything else is read against, so
    /// nothing draws under it.
    fn draw_order_weight(&self) -> u32 {
        5
    }

    fn display_name(&self) -> &str {
        "Global Satellite"
    }

    fn render_mode(&self) -> RenderMode {
        RenderMode::Texture
    }

    /// Nothing here is hatched or theme-coloured: the bar is the channel's own
    /// greyscale and reads the same on either background.
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
        Some(view.selected_channel.display_name().to_owned())
    }

    fn data_generation(&self) -> u64 {
        self.state.data_generation
    }

    /// **The selected channel is in the token**, not just the fetch counter:
    /// the render dispatch groups panes by this, and one token for two channels
    /// is one raster for both.
    fn content_signature(&self, pane: &PaneRef<'_>) -> u64 {
        self.data_generation() ^ (self.view(pane).selected_channel as u64 + 1).rotate_left(32)
    }

    fn has_data(&self, pane: &PaneRef<'_>) -> bool {
        self.cached_grids.contains(self.view(pane).selected_channel)
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
            .map(|grid| grid.values.len())
            .unwrap_or(0)
    }

    /// **600 s.** The blend is hourly and lands 34 to 42 minutes after the hour
    /// it covers (`crate::gmgsi::fetch`), so the arrival instant is not
    /// predictable to better than ten minutes. Polling on the hour would show
    /// an hour-old picture for most of every hour; polling faster would list a
    /// prefix that cannot have changed.
    fn auto_poll_interval(&self) -> Option<u64> {
        Some(600)
    }

    fn clickable_items<'a>(
        &'a self,
        _pane: &PaneRef<'_>,
    ) -> Vec<crate::render::overlay_state::ClickableItem<'a>> {
        Vec::new() // Gridded, not feature-based; hover uses `hover_value_at`.
    }

    fn apply_fetch_result(&mut self, result: FetchPayload, pane: &PaneRef<'_>) {
        let Some(fetch) = self.state.downcast_round::<GmgsiFetchResult>(result) else {
            log::error!("GMGSI handler received unexpected fetch result type");
            return;
        };
        match fetch.0 {
            Ok(granule) => {
                let crate::gmgsi::decode::GmgsiGrid {
                    channel,
                    grid,
                    bounds,
                    valid_time,
                } = granule;
                log::info!(
                    "Received GMGSI {} valid {}: {}x{} grid",
                    channel.display_name(),
                    valid_time,
                    grid.ni,
                    grid.nj,
                );
                // The one place the raster is moved. Everything after this is a
                // refcount.
                let grid = Arc::new(grid);
                let pinned = self.pinned_channels(pane);
                self.cached_grids.insert(
                    channel,
                    GmgsiGranule {
                        grid: Arc::clone(&grid),
                        bounds,
                        valid_time,
                    },
                    &pinned,
                );
                self.state.set_data(Some(grid));
                self.last_error = None;
            }
            Err(e) => {
                log::error!("GMGSI fetch failed: {e}");
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

    /// Nearest neighbour, not interpolation: the mosaic is ~0.072 degrees in
    /// longitude, finer than a tooltip needs.
    ///
    /// **A `_FillValue` point answers `None`**, because
    /// [`rustdar_netcdf::cf::unpack`] marked it missing and [`crate::gmgsi::decode`]
    /// carried that through as a `NaN`. The guard is
    /// [`GridCoords::cell_span_degrees`], which is *local* on this grid — the
    /// rows span 0.029 degrees at the equator and 0.068 at the top — so one
    /// global figure would over-reach at one end.
    ///
    /// [`GridCoords::cell_span_degrees`]: crate::hrrr::GridCoords::cell_span_degrees
    fn hover_value_at(&self, lat: f64, lon: f64, pane: &PaneRef<'_>) -> Option<String> {
        let view = self.view(pane);
        let granule = self.cached_grids.get(view.selected_channel)?;
        if !granule.bounds.contains_point(lat, lon) {
            return None;
        }
        let index = granule.grid.coords.nearest(lat, lon)?;
        let (glat, glon) = granule.grid.coords.at(index)?;
        let value = *granule.grid.values.get(index)?;
        if !value.is_finite() {
            return None;
        }
        let reach = granule
            .grid
            .coords
            .cell_span_degrees(lat)
            .map(|span| 2.0 * span)?;
        let (dlat, dlon) = (glat - lat, glon - lon);
        if dlat * dlat + dlon * dlon > reach * reach {
            return None;
        }
        Some(format!(
            "{}: {:.0} {}",
            view.selected_channel.display_name(),
            value,
            crate::gmgsi::fields::UNIT_LABEL,
        ))
    }

    /// The bar is a pure function of the selected channel, so the signature is
    /// the channel and nothing else — deliberately **not** `data_generation`,
    /// which every poll bumps. `+ 1` keeps the first channel off `0`.
    fn legend(&self, pane: &PaneRef<'_>) -> Option<Signed<OverlayLegend>> {
        let view = self.view(pane);
        if !view.enabled {
            return None;
        }
        let spec = crate::gmgsi::fields::spec(view.selected_channel);
        Some(Signed {
            signature: view.selected_channel as u64 + 1,
            items: OverlayLegend {
                thresholds: spec.scale.thresholds.clone(),
                is_gradient: spec.scale.is_gradient,
                min_value: spec.scale.min_value,
                max_value: spec.scale.max_value,
                unit_label: crate::gmgsi::fields::UNIT_LABEL,
            },
        })
    }

    /// The [`Resident`](rasterize::GriddedInput::Resident) carry: an `Arc` clone
    /// of the resident raster, so describing the job costs a refcount and the
    /// 60 MB never moves. The values memcpy happens only in the web encoder,
    /// which knows the texture's bounds and writes the window's rows alone.
    fn prepare_job(&self, _ctx: &RasterizeContext, pane: &PaneRef<'_>) -> Option<DescribedJob> {
        let granule = self.cached_grids.get(self.view(pane).selected_channel)?;
        Some(DescribedJob::new(rasterize::GriddedInput::Resident(
            Arc::clone(&granule.grid),
        )))
    }

    /// **The gridded row, shared with the model layer and with MRMS.** All
    /// three describe a `GriddedInput`, which carries a `FieldId` rather than
    /// any source's own enum, so one wire form serves them and this layer adds
    /// no codec row and no digest change. `texture_tests::raster_input_owner`
    /// states the sharing; `LABEL` stays `"overlay/model"` because it is a wire
    /// code, and renaming a wire code renumbers shipped clients.
    fn job_codec(&self) -> Option<&'static JobCodec> {
        crate::render::jobs::JOB_CODECS
            .iter()
            .find(|row| row.label == "overlay/model")
    }

    fn create_fetch_tasks(&self, ctx: &FetchConfig, pane: &PaneRef<'_>) -> Vec<FetchTask> {
        let client = ctx.client.clone();
        let sources = ctx.sources.clone();
        let channel = self.view(pane).selected_channel;
        // Captured here rather than inside the future so the instant the
        // listing walks back from is the instant the round was asked for.
        let now = chrono::Utc::now().naive_utc();
        vec![FetchTask {
            kind: known::GMGSI,
            future: Box::pin(async move {
                let result =
                    crate::gmgsi::fetch::fetch_latest(&client, &sources, channel, now).await;
                Box::new(GmgsiFetchResult(result)) as FetchPayload
            }),
        }]
    }

    fn controls(&self, pane: &PaneRef<'_>) -> Vec<ControlItem> {
        let view = self.view(pane);
        let granule = self.cached_grids.get(view.selected_channel);

        // The granule's own valid time, not the fetch time: the blend lands
        // ~40 minutes after the hour it covers, so "updated 30s ago" and
        // "valid 12:00z" are different facts and the second is on screen.
        let label = match granule {
            Some(g) => format!("Global Satellite ({})", g.valid_time.format("%H:%Mz")),
            None => "Global Satellite".to_string(),
        };

        let mut items = vec![ControlItem::Toggle {
            id: "enabled",
            label,
            enabled: view.enabled,
        }];

        // Ungated on `enabled`: a hidden layer's options stay visible and
        // editable, Refresh still fetches, and the status lines keep reporting.
        items.push(ControlItem::Dropdown {
            id: "channel",
            label: "Channel".into(),
            options: GmgsiChannel::all()
                .iter()
                .map(|c| (c.as_str().into(), c.display_name().into()))
                .collect(),
            selected: view.selected_channel.as_str().into(),
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
            "channel" => {
                if let ControlValue::String(ref val) = update.value
                    && let Ok(new_channel) = val.parse::<GmgsiChannel>()
                    && new_channel != self.view(&pane.as_ref()).selected_channel
                {
                    self.edit(pane, |state| state.selected_channel = new_channel);
                    // A resident channel needs no network; bump the generation
                    // so the pane re-rasterizes what is already in hand.
                    if self.cached_grids.contains(new_channel) {
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
        Some(Box::new(GmgsiPaneState::new(enabled)))
    }

    fn deserialize_pane_state(
        &self,
        value: serde_json::Value,
        enabled: bool,
    ) -> Option<FetchPayload> {
        let mut state = GmgsiPaneState::new(enabled);
        if let Some(on) = value.get("enabled").and_then(|v| v.as_bool()) {
            state.enabled = on;
        }
        if let Some(channel) = value
            .get("channel")
            .and_then(|v| v.as_str())
            .and_then(|s| s.parse::<GmgsiChannel>().ok())
        {
            state.selected_channel = channel;
        }
        Some(Box::new(state))
    }

    fn serialize_pane_state(&self, state: &dyn std::any::Any) -> serde_json::Value {
        let Some(state) = state.downcast_ref::<GmgsiPaneState>() else {
            return serde_json::Value::Null;
        };
        serde_json::json!({
            "enabled": state.enabled,
            "channel": state.selected_channel.as_str(),
        })
    }
}

#[cfg(test)]
mod tests;
