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
//! # The clock, and the frames (WB-10)
//!
//! `TimeAxis::FrameSeries` at a ~two-minute step, reaching no further forward
//! than the wall clock. MRMS held `TimeAxis::Live` until the ruling
//! `radar_takes_the_clock_wherever_it_is_drawn` demanded was made; it is made
//! now, and it is recorded in that pin: **the weight order stands.** MRMS
//! joins the frame-series set at its existing weight of 15 — above the model
//! (10) and GMGSI (5), below radar (30) — and therefore takes the clock on
//! any radar-off pane that also shows the model or the satellite. MRMS *is*
//! observed radar, a mosaic of the same physics, so on a pane without
//! single-site radar it is the most radar-like clock available, and its
//! two-minute cadence is the finest scrub grain of the non-radar layers.
//!
//! **No `min_loop_frames` floor, stated rather than omitted.** GMGSI and the
//! model declare 13 because their frames are an hour apart and the Lookback
//! slider's default hour would buy two. At MRMS's 120 s cadence that same
//! default hour is already ~30 frames, so the slider's own spans yield real
//! loops everywhere and the layer takes the trait's 0. A floor under 31
//! frames would be dead code at the default span; one past it would *widen*
//! every MRMS pane's rail for no reason. Neither buys anything.
//!
//! # Grids are a staging area; the loop holds textures
//!
//! The same design as [`super::gmgsi`], at a heavier weight: one CONUS grid
//! is 98,000,000 B and one decode peaks ~147 MB transient, so
//! [`crate::mrms::FRAME_STAGING_BYTES`] stages **one** granule on every arm
//! and [`MrmsHandler::frame_gate`] serialises the frame fetches. A loop frame
//! is a rasterized *texture* held by the pane; the granule is what one frame
//! passes through on its way to becoming one, and `prepare_job` reads
//! `ctx.frame` before the pane's selection so an unstaged frame describes
//! **no job** rather than another stamp's picture.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;

use crate::fetch_policy::Whole;
use crate::mrms::{
    FRAME_STAGING_BYTES, GRID_CACHE_BYTES, MrmsFetchResult, MrmsFrameFetch, MrmsGrid, MrmsListing,
    MrmsProduct,
};
use crate::render::controls::{
    ControlButton, ControlEffect, ControlItem, ControlUpdate, ControlValue,
};
use crate::render::overlay_state::{
    FetchConfig, FetchPayload, FetchTask, OverlayHandler, OverlayLegend, OverlayState, PaneMut,
    PaneRef, RasterizeContext, RenderMode, Signed, Surface,
};
use crate::render::rasterize;
use squallar_source::handler::{FrameListingResult, TaskFuture};
use squallar_source::id::{LayerId, known};
use squallar_source::job::{DescribedJob, JobCodec};
use squallar_source::time::{FrameListing, FrameSource, FrameStamp, TimeAxis};

/// **One frame's granule at a time, application-wide** — the same shape as
/// GMGSI's gate, needed harder here: `dispatch_loop_frame_fetches` has no
/// throttle of its own, and one MRMS decode peaks ~147 MB transient (49 MB
/// PNG image buffer + the 98 MB values vector, `crate::mrms::decode`'s own
/// arithmetic). Thirty unthrottled fetches — one slider-default hour at the
/// ~2-minute cadence — would hold ~4.4 GB in flight before any cache saw a
/// byte.
///
/// Serialising costs almost no wall time: the bytes are the bottleneck either
/// way, and FIFO fairness means granules arrive in render-set order, which is
/// playhead outward.
type FrameGate = Arc<futures::lock::Mutex<()>>;

/// A staged loop-frame granule's identity: **the product and the stamp it
/// depicts**.
///
/// No run, and none is invented on the way back out either: MRMS is observed
/// data with no cycle behind it, so [`FrameStamp::run`] is `None` in every
/// stamp this layer names and in every stamp it is asked about. The stamp
/// carries the granule's own non-clock-aligned seconds (`000039`, `000242`),
/// exactly as the listing read them off the key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct FrameKey {
    product: MrmsProduct,
    valid: chrono::NaiveDateTime,
}

/// The staged loop-frame granules, bounded by **bytes** and evicted
/// least-recently-used first.
///
/// Deliberately **not** [`MrmsGridCache`]: that one is keyed by product and
/// holds the pane's live picture, so thirty stamps of one product would
/// overwrite each other in it and the pinned live granule would fight the
/// loop for its slot. Two stores, two purposes, two budgets, and the live one
/// is unchanged by this item.
struct MrmsFrameCache {
    entries: HashMap<FrameKey, MrmsGrid>,
    recency: RefCell<Vec<FrameKey>>,
    /// Injected for the same reason [`MrmsGridCache::budget`] is: the shipped
    /// budget is one 98 MB mosaic and no test can afford to overflow it.
    budget: usize,
}

impl MrmsFrameCache {
    fn new(budget: usize) -> Self {
        Self {
            entries: HashMap::new(),
            recency: RefCell::new(Vec::new()),
            budget,
        }
    }

    fn touch(&self, key: FrameKey) {
        let mut recency = self.recency.borrow_mut();
        if let Some(pos) = recency.iter().position(|k| *k == key) {
            recency.remove(pos);
            recency.push(key);
        }
    }

    fn get(&self, key: FrameKey) -> Option<&MrmsGrid> {
        let grid = self.entries.get(&key)?;
        self.touch(key);
        Some(grid)
    }

    fn resident_bytes(&self) -> usize {
        self.entries.values().map(|g| g.resident_bytes()).sum()
    }

    /// **Nothing is pinned.** A staged granule evicted before its job is
    /// described costs one frame its picture until the next listing; a staged
    /// granule *kept* past the budget costs 98 MB on an arm that has already
    /// said it cannot spare it. The live cache makes the opposite trade
    /// because a pane with no live granule has nothing that will re-ask.
    fn insert(&mut self, key: FrameKey, grid: MrmsGrid) {
        if self.entries.insert(key, grid).is_some() {
            self.touch(key);
        } else {
            self.recency.borrow_mut().push(key);
        }
        while self.resident_bytes() > self.budget {
            let victim = {
                let mut recency = self.recency.borrow_mut();
                let Some(pos) = recency.iter().position(|k| *k != key) else {
                    // The arrival alone is left and it is over budget by
                    // itself: keeping it is what lets the pipeline advance at
                    // all, and the build-time floor makes this unreachable in
                    // the shipped configuration.
                    break;
                };
                recency.remove(pos)
            };
            self.entries.remove(&victim);
        }
    }

    /// Drop everything but `keep` — the [`FrameSource::retain_frames`] door.
    fn retain(&mut self, keep: impl Fn(FrameKey) -> bool) {
        self.entries.retain(|key, _| keep(*key));
        self.recency.borrow_mut().retain(|key| keep(*key));
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.entries.len()
    }
}

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
    /// **What object each listed stamp holds**, per product — the whole of
    /// what a listing bought. `list_frames` reads its keys and `fetch_frame`
    /// reads its values, so a frame costs a share of 1 day-LIST at listing
    /// time and 1 GET at fetch time rather than 2 round trips each.
    frame_keys: HashMap<MrmsProduct, std::collections::BTreeMap<chrono::NaiveDateTime, String>>,
    /// Windows a **complete** listing has covered, per product. A listing that
    /// errored or was sampled leaves no row, so `list_frames` goes on saying
    /// "at least these" for that window.
    covered: HashMap<MrmsProduct, Vec<(chrono::NaiveDateTime, chrono::NaiveDateTime)>>,
    frame_grids: MrmsFrameCache,
    frame_gate: FrameGate,
}

impl MrmsHandler {
    pub fn new() -> Self {
        Self {
            state: OverlayState::new(),
            defaults: MrmsPaneState::new(false),
            cached_grids: MrmsGridCache::new(GRID_CACHE_BYTES),
            last_error: None,
            frame_keys: HashMap::new(),
            covered: HashMap::new(),
            frame_grids: MrmsFrameCache::new(FRAME_STAGING_BYTES),
            frame_gate: Arc::new(futures::lock::Mutex::new(())),
        }
    }

    /// The shipped handler with its staging area sized to `bytes` instead of
    /// [`FRAME_STAGING_BYTES`], for the reason [`MrmsGridCache::budget`] is
    /// injected: the shipped figure is one 98 MB mosaic, no test can build
    /// one, and an eviction policy that is never overflowed is a policy
    /// nothing has checked.
    #[cfg(test)]
    fn with_frame_budget(bytes: usize) -> Self {
        Self {
            frame_grids: MrmsFrameCache::new(bytes),
            ..Self::new()
        }
    }

    /// Whether a complete listing of this product has already covered `range`.
    fn covers(
        &self,
        product: MrmsProduct,
        range: (chrono::NaiveDateTime, chrono::NaiveDateTime),
    ) -> bool {
        self.covered
            .get(&product)
            .is_some_and(|windows| windows.iter().any(|w| w.0 <= range.0 && range.1 <= w.1))
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

impl FrameSource for MrmsHandler {
    /// **The newest mosaic of this pane's product at or before `t`.**
    ///
    /// A mosaic depicts its own stamp and nothing depicts the ~two minutes
    /// after it, so every instant until the next one is drawn by carrying it
    /// forward — which is what this answers, over the whole key store and with
    /// no window to clip it at.
    ///
    /// `run: None`: MRMS is observed and there is no cycle behind it.
    fn latest_at(
        &self,
        pane: &PaneRef<'_>,
        t: chrono::NaiveDateTime,
    ) -> Option<squallar_source::time::FrameStamp> {
        let product = self.view(pane).selected_product;
        let stamps: Vec<FrameStamp> = self
            .frame_keys
            .get(&product)?
            .keys()
            .map(|valid| FrameStamp {
                valid: *valid,
                run: None,
            })
            .collect();
        squallar_source::time::newest_at_or_before(&stamps, t)
    }

    /// **The stamps this product's listings have found for `range`** — every
    /// one inside it, and **the newest one at or before its start**.
    ///
    /// Synchronous and I/O-free: it reads the keys
    /// [`Self::create_frame_list_task`] filed and nothing else. Every stamp
    /// carries `run: None` — MRMS is observed, there is no cycle behind it.
    ///
    /// **The leading mosaic is [`Self::latest_at`]'s answer**, the same shape
    /// the satellite layer takes and for the same reason: a window opened
    /// between two mosaics has clock in front of its first listed stamp, and
    /// the only picture those stops can be drawn from is the mosaic the window
    /// opened after. Clipping at `range.0` dropped it. The gap it left here is
    /// bounded by this layer's ~2-minute cadence rather than by an hour, which
    /// is why it was never the report the satellite layer's identical clip
    /// produced — not why it was not the same defect.
    ///
    /// `complete` only where a listing that really covered this window landed:
    /// a failed or sampled listing leaves the answer readable as "at least
    /// these", which is what it is. It is a claim about the window, and
    /// carrying one earlier mosaic in does not widen it.
    fn list_frames(
        &self,
        _ctx: &FetchConfig,
        pane: &PaneRef<'_>,
        range: (chrono::NaiveDateTime, chrono::NaiveDateTime),
    ) -> FrameListing {
        let product = self.view(pane).selected_product;
        let inside = self
            .frame_keys
            .get(&product)
            .into_iter()
            .flat_map(|keys| keys.range(range.0..=range.1))
            .filter(|(valid, _)| **valid > range.0)
            .map(|(valid, _)| FrameStamp {
                valid: *valid,
                run: None,
            });
        let frames: Vec<FrameStamp> = self
            .latest_at(pane, range.0)
            .into_iter()
            .chain(inside)
            .collect();
        FrameListing {
            range,
            frames,
            complete: self.covers(product, range),
        }
    }

    /// **1 LIST per UTC day the window touches** — at most two for any window
    /// the Lookback slider can name, bounded at
    /// [`crate::mrms::fetch::MAX_FRAME_LIST_REQUESTS`].
    ///
    /// Cheaper per frame than GMGSI's listing, and the reason is worth
    /// keeping straight: GMGSI's object names end in an unpredictable creation
    /// stamp, so every frame's object must be found with its own hour LIST.
    /// An MRMS key is a pure function of its own timestamp, so **one day
    /// prefix LIST names the whole day's ~720 stamps in one page**, and the
    /// GETs later are ~1.3 MB gzipped apiece. The archive is real and deep —
    /// the day prefixes reach back to 2020-10-14 — so any window the rail can
    /// name is listable.
    ///
    /// The product is captured here, at dispatch, and travels in the scope.
    fn create_frame_list_task(
        &self,
        ctx: &FetchConfig,
        pane: &PaneRef<'_>,
        range: (chrono::NaiveDateTime, chrono::NaiveDateTime),
    ) -> Option<FetchTask> {
        let product = self.view(pane).selected_product;
        let client = ctx.client.clone();
        let sources = ctx.sources.clone();
        Some(FrameListingResult::task(known::MRMS, async move {
            let (keys, complete) =
                crate::mrms::fetch::list_frame_keys(&client, &sources, product, range).await;
            FrameListingResult {
                listing: FrameListing {
                    range,
                    frames: keys
                        .iter()
                        .map(|(valid, _)| FrameStamp {
                            valid: *valid,
                            run: None,
                        })
                        .collect(),
                    complete,
                },
                scope: Box::new(MrmsListing {
                    product,
                    range,
                    keys,
                    complete,
                }),
            }
        }))
    }

    /// **One GET, on the key a listing already found**, behind [`FrameGate`].
    ///
    /// `None` for a stamp no listing of this product named, and `None` for one
    /// already staged — both are the contract's own answers, and neither is a
    /// throttle. The throttle is inside the task: it takes the gate before it
    /// touches the network, so the whole render set may be dispatched at once
    /// (which it is — `dispatch_loop_frame_fetches` has no throttle of its
    /// own) while only one ~147 MB-peak decode exists at a time.
    fn fetch_frame(
        &self,
        ctx: &FetchConfig,
        pane: &PaneRef<'_>,
        stamp: &FrameStamp,
    ) -> Option<FetchTask> {
        let product = self.view(pane).selected_product;
        let key = FrameKey {
            product,
            valid: stamp.valid,
        };
        if self.frame_grids.entries.contains_key(&key) {
            return None;
        }
        let object = self.frame_keys.get(&product)?.get(&stamp.valid)?.clone();
        let client = ctx.client.clone();
        let sources = ctx.sources.clone();
        let gate = Arc::clone(&self.frame_gate);
        let valid = stamp.valid;
        let future: TaskFuture = Box::pin(async move {
            let _one_at_a_time = gate.lock().await;
            let grid =
                match crate::mrms::fetch::fetch_key(&client, &sources, product, &object).await {
                    Ok(grid) => Some(grid),
                    Err(e) => {
                        log::error!("MRMS frame fetch failed for {product:?} valid {valid}: {e:?}");
                        None
                    }
                };
            Box::new(MrmsFrameFetch {
                product,
                valid,
                grid,
            }) as FetchPayload
        });
        Some(FetchTask {
            kind: known::MRMS,
            future,
        })
    }

    /// **The staged granules of this pane's own product** — at most one, by
    /// [`FRAME_STAGING_BYTES`], however many frames the loop holds.
    ///
    /// This pane's product and not the whole store: another pane's rate
    /// granule is not a frame this pane can draw, and offering it would have
    /// the dispatch describe a job that paints the wrong field.
    fn frames_resident(&self, pane: &PaneRef<'_>) -> Vec<FrameStamp> {
        let product = self.view(pane).selected_product;
        let mut frames: Vec<FrameStamp> = self
            .frame_grids
            .entries
            .keys()
            .filter(|key| key.product == product)
            .map(|key| FrameStamp {
                valid: key.valid,
                run: None,
            })
            .collect();
        frames.sort_by_key(|stamp| stamp.valid);
        frames
    }

    /// Drop every staged granule not in `keep`, matching on the stamp alone —
    /// a caller's stamps carry the `run: None` this layer names, and a stamp
    /// rebuilt from a bare instant carries it too.
    ///
    /// **Nothing above calls this yet** (the one production frame-eviction
    /// authority is still the byte budget inside each layer), so it is
    /// exercised by this layer's own suite rather than by the loop.
    fn retain_frames(&mut self, pane: &PaneRef<'_>, keep: &[FrameStamp]) {
        let product = self.view(pane).selected_product;
        self.frame_grids.retain(|key| {
            key.product != product || keep.iter().any(|stamp| stamp.valid == key.valid)
        });
    }

    /// File a listing under the product it was **dispatched for**, never the
    /// one the arriving pane holds: a `PaneRef` on this path is the union
    /// across panes and its config is null by construction.
    fn apply_frame_listing(
        &mut self,
        _listing: FrameListing,
        scope: FetchPayload,
        _pane: &PaneRef<'_>,
    ) {
        let Ok(scope) = scope.downcast::<MrmsListing>() else {
            log::error!("a frame listing reached the MRMS layer under another layer's scope");
            return;
        };
        let keys = self.frame_keys.entry(scope.product).or_default();
        for (valid, object) in scope.keys {
            keys.insert(valid, object);
        }
        // Coverage only for a listing that really covered the window: a failed
        // or sampled one must not leave `list_frames` claiming it is settled.
        if scope.complete && !self.covers(scope.product, scope.range) {
            self.covered
                .entry(scope.product)
                .or_default()
                .push(scope.range);
        }
    }

    /// Stage one frame's granule under the `(product, stamp)` its fetch was
    /// dispatched for. A failed fetch stages nothing and the frame keeps no
    /// picture.
    fn apply_frame(&mut self, _stamp: FrameStamp, data: FetchPayload, _pane: &PaneRef<'_>) {
        let Ok(frame) = data.downcast::<MrmsFrameFetch>() else {
            log::error!("a frame reached the MRMS layer under another layer's payload");
            return;
        };
        let MrmsFrameFetch {
            product,
            valid,
            grid,
        } = *frame;
        let Some(grid) = grid else {
            return;
        };
        self.frame_grids.insert(FrameKey { product, valid }, grid);
    }

    /// **Zero, and it is a decision rather than an omission.** A national
    /// mosaic is made *from* observations, so none exists for an instant that
    /// has not happened — the same fact [`OverlayHandler::time_axis`] states
    /// as `extends_future: false`. The rail must not offer this layer a stop
    /// past the wall clock.
    fn frame_horizon(&self, _pane: &PaneRef<'_>) -> chrono::Duration {
        chrono::Duration::zero()
    }
}

impl OverlayHandler for MrmsHandler {
    /// The MRMS products this layer offers, projected into the substrate's read
    /// contract by [`crate::mrms::fields`].
    fn products(&self) -> &'static [squallar_source::product::ProductSpec] {
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
    fn current_field(&self, pane: &PaneRef<'_>) -> Option<squallar_source::product::FieldId> {
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
    ///
    /// Since WB-10 this weight is also half of the clock ruling: the transport
    /// is the topmost enabled `FrameSeries` layer with **zero special cases**,
    /// so 15 is what makes MRMS the clock of a radar-off pane that also shows
    /// the model or the satellite. `radar_takes_the_clock_wherever_it_is_drawn`
    /// pins the ordering and records why.
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

    // -- Time -----------------------------------------------------------------

    /// One national mosaic every ~two minutes, stamped with the instant it
    /// depicts, and never one for an instant that has not happened: discrete
    /// frames that stop at the wall clock.
    ///
    /// `typical_step` is nominal — the real stamps are not clock-aligned and
    /// occasionally skip a beat — which is exactly what a *typical* step is
    /// for. **No `min_loop_frames` accompanies it**: at this cadence the
    /// Lookback slider's default hour is already ~30 frames, so the slider's
    /// own spans yield real loops and a floor would widen every MRMS pane's
    /// rail for nothing.
    fn time_axis(&self) -> TimeAxis {
        TimeAxis::FrameSeries {
            typical_step: std::time::Duration::from_secs(120),
            extends_future: false,
        }
    }

    /// **The mosaics these stops draw from, and none of the minutes between
    /// them.**
    ///
    /// At a ~2-minute cadence a stop is rarely more than a step from the
    /// mosaic that draws it, so the answer here is a dense set of narrow
    /// ranges rather than the sparse one a satellite loop produces — the same
    /// derivation, [`squallar_source::time::frame_residency`], routed through
    /// [`Self::latest_at`] and never re-deriving `FrameSeries`'s rule.
    ///
    /// The **coalescing matters most here**: stops closer together than the
    /// gap between two mosaics collapse into one unbroken range, so a scrub
    /// asking about a hundred instants does not answer a hundred ranges.
    fn residency_for(
        &self,
        pane: &PaneRef<'_>,
        stops: &[chrono::NaiveDateTime],
    ) -> squallar_source::time::Residency {
        squallar_source::time::frame_residency(self, pane, stops)
    }

    /// This layer comes in stamped frames, and answers every one of
    /// [`FrameSource`]'s methods below.
    fn frames(&self) -> Option<&dyn FrameSource> {
        Some(self)
    }

    fn frames_mut(&mut self) -> Option<&mut dyn FrameSource> {
        Some(self)
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
    ///
    /// **A named frame is drawn from that frame's own granule.** `ctx.frame`
    /// is read before the pane's selection, and the lookup goes to the staging
    /// store keyed by the stamp — never to the live cache, which holds one
    /// granule per product and would hand every frame of a loop the same
    /// picture. A named frame whose granule is not staged **describes no job
    /// at all** rather than falling back to the pane's own picture: that
    /// fallback is one instant's mosaic presented, unlabelled, as another's,
    /// and it is the exact defect the frame-addressed dispatch exists to
    /// prevent.
    ///
    /// The refcount is also what frees the staging slot: the job owns the
    /// raster from here, so the store may evict the entry on the very next
    /// arrival without the raster going anywhere.
    fn prepare_job(&self, ctx: &RasterizeContext, pane: &PaneRef<'_>) -> Option<DescribedJob> {
        let product = self.view(pane).selected_product;
        let grid: &MrmsGrid = match ctx.frame {
            Some(stamp) => self.frame_grids.get(FrameKey {
                product,
                valid: stamp.valid,
            })?,
            None => self.cached_grids.get(product)?,
        };
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
        // The instant this pane depicts, not the wall clock. On a live pane the
        // two are equal and nothing moves; on a parked one this is what stopped
        // the mosaic being this evening's over an afternoon scan.
        let at = ctx.as_of;
        vec![FetchTask {
            kind: known::MRMS,
            future: Box::pin(async move {
                let result = crate::mrms::fetch::fetch_latest(&client, &sources, product, at).await;
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
