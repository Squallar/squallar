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
//! and `squallar-worker`'s `WIRE_FRAMING_ROWS` is untouched by this layer.
//!
//! # The clock, and the frames
//!
//! `TimeAxis::FrameSeries` at an hourly step, reaching no further forward than
//! the wall clock. The ruling `radar_takes_the_clock_wherever_it_is_drawn`
//! demanded before a third frame-series layer could register is made in that
//! test and rests on this layer's own `draw_order_weight`: **5 is the lowest
//! weight any layer claims**, so on a pane that also draws radar (30) or the
//! model (10) the transport does not move, and GMGSI takes a pane's clock only
//! where it is the sole frame-series layer enabled.
//!
//! # Grids are a staging area; the loop holds textures
//!
//! One mosaic is 3000 x 5000 `f32` = 60 MB, and [`GRID_CACHE_BYTES`] holds one
//! to four of them. A thirteen-frame loop is therefore **not** thirteen
//! granules: a loop frame is a rasterized *texture*, held by the pane, and a
//! granule is what one frame passes through on its way to becoming one.
//!
//! So exactly **one granule has to be resident at a time** for the pipeline to
//! advance — arrive, be described into a job (which takes its own refcount on
//! the raster, so the cache may drop it the instant after), be drawn. That is
//! why [`FRAME_STAGING_BYTES`] is one grid on every arm and why the frame
//! fetches are **serialised** through [`GmgsiHandler::frame_gate`]: thirteen
//! concurrent decodes would put 780 MB in flight before any cache saw them,
//! and no eviction policy can undo that.
//!
//! When a granule is evicted before its job is described — possible only if a
//! second granule lands inside one frame of the pump — that frame is left
//! **without** a picture until the next listing. It is never given another
//! hour's, which is the failure the frame-addressed `prepare_job` exists to
//! prevent.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;

use crate::fetch_policy::Whole;
use crate::gmgsi::{GmgsiChannel, GmgsiFetchResult, GmgsiFrameFetch, GmgsiListing};
use crate::render::controls::{
    ControlButton, ControlEffect, ControlItem, ControlUpdate, ControlValue,
};
use crate::render::gridded::ResidentGrid;
use crate::render::overlay_state::{
    FetchConfig, FetchPayload, FetchTask, OverlayHandler, OverlayLegend, OverlayState, PaneMut,
    PaneRef, RasterizeContext, RenderMode, Signed, Surface,
};
use crate::render::rasterize;
use squallar_geo::GeoBounds;
use squallar_source::handler::{FrameListingResult, TaskFuture};
use squallar_source::id::{LayerId, known};
use squallar_source::job::{DescribedJob, JobCodec};
use squallar_source::time::{FrameListing, FrameSource, FrameStamp, TimeAxis};

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
/// Spelled as a `cfg` cascade rather than resolved from `squallar-device-profile`
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

/// **How many bytes of loop-frame granule may stage at once: one mosaic, on
/// every arm.**
///
/// Not a per-arm cascade, because the figure is not a guess about the device —
/// it is what the pipeline needs. A loop frame's storage is its **texture**
/// (11.06 MB for a 1280x960-point pane at 1x, held by the pane, priced by
/// `overlay_frame_bytes`); the granule is a 60 MB staging buffer a frame
/// passes through on its way to one. A described job takes its own refcount on
/// the raster, so the slot is free again the moment `prepare_job` has run, and
/// [`GmgsiHandler::frame_gate`] admits one fetch at a time so nothing else can
/// ask for the slot in the meantime.
///
/// **Thirteen resident granules would be 780 MB** against a 96 MiB wasm model
/// pool and a 56 MiB wasm loop pool — 14x and 15x over. A grid-holding loop is
/// not a smaller version of this design, it is an infeasible one.
pub const FRAME_STAGING_BYTES: usize = GLOBAL_GRID_BYTES;

// The pipeline advances one granule at a time, so a staging area under one
// grid settles empty and no frame is ever rasterized. Same reason the live
// cache carries the same floor, and the same reason it is a **build** failure.
const _: () = assert!(FRAME_STAGING_BYTES >= GLOBAL_GRID_BYTES);

/// **One frame's granule at a time, application-wide.**
///
/// `dispatch_loop_frame_fetches` puts the whole render set on the wire in one
/// pass and states in as many words that it has **no throttle** — radar's
/// concurrency lives in its download manager, which this layer does not use.
/// Thirteen unthrottled GMGSI fetches would hold thirteen 7.5 MB bodies and
/// thirteen 60 MB decoded rasters at once, inside the futures, before any
/// cache could evict anything.
///
/// Serialising costs almost no wall time: the bytes are the bottleneck either
/// way (13 granules is ~97 MB whether they arrive together or in turn), and
/// FIFO fairness means they arrive in render-set order, which is playhead
/// outward.
type FrameGate = Arc<futures::lock::Mutex<()>>;

/// A staged loop-frame granule's identity: **the channel and the hour it
/// depicts**.
///
/// No run, and none is invented on the way back out either: GMGSI is observed
/// data with no cycle behind it, so [`FrameStamp::run`] is `None` in every
/// stamp this layer names and in every stamp it is asked about. The frame list
/// above carries only an instant and reconstructs `FrameStamp { valid, run:
/// None }` where a layer's own stamp is missing — which resolves here, by
/// construction, rather than by luck.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
struct FrameKey {
    channel: GmgsiChannel,
    valid: chrono::NaiveDateTime,
}

/// The staged loop-frame granules, bounded by **bytes** and evicted
/// least-recently-used first.
///
/// Deliberately **not** [`GmgsiGridCache`]: that one is keyed by channel and
/// holds the pane's live picture, so thirteen hours of one channel would
/// overwrite each other in it and the pinned live granule would fight the
/// loop for its only slot. Two stores, two purposes, two budgets, and the
/// live one is unchanged by this item.
struct GmgsiFrameCache {
    entries: HashMap<FrameKey, GmgsiGranule>,
    recency: RefCell<Vec<FrameKey>>,
    /// Injected for the same reason [`GmgsiGridCache::budget`] is: the shipped
    /// budget is one 60 MB mosaic and no test can afford to overflow it.
    budget: usize,
}

impl GmgsiFrameCache {
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

    fn get(&self, key: FrameKey) -> Option<&GmgsiGranule> {
        let granule = self.entries.get(&key)?;
        self.touch(key);
        Some(granule)
    }

    fn resident_bytes(&self) -> usize {
        self.entries.values().map(|g| g.resident_bytes()).sum()
    }

    /// **Nothing is pinned.** A staged granule that is evicted before its job
    /// is described costs one frame its picture until the next listing; a
    /// staged granule that is *kept* past the budget costs 60 MB on an arm
    /// that has already said it cannot spare it. The live cache makes the
    /// opposite trade because a pane with no live granule has nothing that
    /// will re-ask.
    fn insert(&mut self, key: FrameKey, granule: GmgsiGranule) {
        if self.entries.insert(key, granule).is_some() {
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
                    // all, and the floor above makes this unreachable in the
                    // shipped configuration.
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
    /// **What object each listed hour holds**, per channel — the whole of what
    /// a listing bought. `list_frames` reads its keys and `fetch_frame` reads
    /// its values, so a frame costs 1 LIST at listing time and 1 GET at fetch
    /// time rather than 2 round trips each.
    frame_keys: HashMap<GmgsiChannel, std::collections::BTreeMap<chrono::NaiveDateTime, String>>,
    /// Windows a **complete** listing has covered, per channel. A listing that
    /// errored or was sampled leaves no row, so `list_frames` goes on saying
    /// "at least these" for that window.
    covered: HashMap<GmgsiChannel, Vec<(chrono::NaiveDateTime, chrono::NaiveDateTime)>>,
    frame_grids: GmgsiFrameCache,
    frame_gate: FrameGate,
}

impl GmgsiHandler {
    pub fn new() -> Self {
        Self {
            state: OverlayState::new(),
            defaults: GmgsiPaneState::new(false),
            cached_grids: GmgsiGridCache::new(GRID_CACHE_BYTES),
            last_error: None,
            frame_keys: HashMap::new(),
            covered: HashMap::new(),
            frame_grids: GmgsiFrameCache::new(FRAME_STAGING_BYTES),
            frame_gate: Arc::new(futures::lock::Mutex::new(())),
        }
    }

    /// The shipped handler with its staging area sized to `bytes` instead of
    /// [`FRAME_STAGING_BYTES`].
    ///
    /// The same reason [`GmgsiGridCache::budget`] is injected: the shipped
    /// figure is one 60 MB mosaic, no test can build one, and an eviction
    /// policy that is never overflowed is a policy nothing has checked.
    #[cfg(test)]
    fn with_frame_budget(bytes: usize) -> Self {
        Self {
            frame_grids: GmgsiFrameCache::new(bytes),
            ..Self::new()
        }
    }

    /// Whether a complete listing of this channel has already covered `range`.
    fn covers(
        &self,
        channel: GmgsiChannel,
        range: (chrono::NaiveDateTime, chrono::NaiveDateTime),
    ) -> bool {
        self.covered
            .get(&channel)
            .is_some_and(|windows| windows.iter().any(|w| w.0 <= range.0 && range.1 <= w.1))
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

impl FrameSource for GmgsiHandler {
    /// **The newest granule of this pane's channel at or before `t`.**
    ///
    /// An hour `H`'s granule depicts `H` and nothing depicts the minutes after
    /// it, so every instant in `H..H+1h` is drawn by carrying `H` forward —
    /// which is what this answers, over the whole key store and with no window
    /// to clip it at.
    ///
    /// `run: None`: GMGSI is observed and there is no cycle behind it.
    fn latest_at(
        &self,
        pane: &PaneRef<'_>,
        t: chrono::NaiveDateTime,
    ) -> Option<squallar_source::time::FrameStamp> {
        let channel = self.view(pane).selected_channel;
        let stamps: Vec<FrameStamp> = self
            .frame_keys
            .get(&channel)?
            .keys()
            .map(|valid| FrameStamp {
                valid: *valid,
                run: None,
            })
            .collect();
        squallar_source::time::newest_at_or_before(&stamps, t)
    }

    /// **The granules this channel's listings have found for `range`** — every
    /// one inside it, and **the newest one at or before its start**.
    ///
    /// Synchronous and I/O-free: it reads the keys
    /// [`Self::create_frame_list_task`] filed and nothing else. Every stamp
    /// carries `run: None` — GMGSI is observed, there is no cycle behind it.
    ///
    /// **Why the answer reaches one granule earlier than the window.** A
    /// window opened at `HH:MM` has `60 - MM` minutes of clock in front of its
    /// first *whole* hour, and the only picture any of them can be drawn from
    /// is the granule for the hour the window opened inside — which is earlier
    /// than `range.0`. Clipping it away left those stops with no frame at all:
    /// a loop enabled at any minute but `:00` opened on a blank satellite
    /// layer, and the caller had nothing to carry forward because nothing was
    /// offered. [`crate::gmgsi::fetch::hours_in_range`] lists that hour; this
    /// is what hands it on.
    ///
    /// **The leading granule is [`Self::latest_at`]'s answer, not a second
    /// derivation of it.** That is the whole shape of the fix: the rule "what
    /// would this layer draw at `T`" has one implementation, and a window is
    /// that answer at its own start plus the stamps that follow. One granule,
    /// never the whole tail, and a window that opens exactly on the hour
    /// reaches nothing extra — the granule at `range.0` is inside it already.
    ///
    /// `complete` only where a listing that really covered this window landed:
    /// a failed or sampled listing leaves the answer readable as "at least
    /// these", which is what it is. It is a claim about the window, and
    /// carrying one earlier granule in does not widen it.
    fn list_frames(
        &self,
        _ctx: &FetchConfig,
        pane: &PaneRef<'_>,
        range: (chrono::NaiveDateTime, chrono::NaiveDateTime),
    ) -> FrameListing {
        let channel = self.view(pane).selected_channel;
        let inside = self
            .frame_keys
            .get(&channel)
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
            complete: self.covers(channel, range),
        }
    }

    /// **1 LIST per hour in the window**, bounded by
    /// [`crate::gmgsi::fetch::MAX_FRAME_LIST_REQUESTS`], six in flight.
    ///
    /// There is no cheaper form. Every object name ends in a creation stamp
    /// that trails its hour by 34 to 42 minutes and by a different amount per
    /// channel, so no clock can build a key and
    /// [`squallar_source::origins::DataSources`] deliberately publishes no
    /// `gmgsi_key`. A thirteen-frame window is therefore **13 LISTs now and 13
    /// GETs later** — the LISTs return one XML document naming one object
    /// each, and the GETs are the 7.5 MB granules.
    ///
    /// The channel is captured here, at dispatch, and travels in the scope.
    fn create_frame_list_task(
        &self,
        ctx: &FetchConfig,
        pane: &PaneRef<'_>,
        range: (chrono::NaiveDateTime, chrono::NaiveDateTime),
    ) -> Option<FetchTask> {
        let channel = self.view(pane).selected_channel;
        let client = ctx.client.clone();
        let sources = ctx.sources.clone();
        Some(FrameListingResult::task(known::GMGSI, async move {
            let (keys, complete) =
                crate::gmgsi::fetch::list_frame_keys(&client, &sources, channel, range).await;
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
                scope: Box::new(GmgsiListing {
                    channel,
                    range,
                    keys,
                    complete,
                }),
            }
        }))
    }

    /// **One GET, on the key a listing already found**, behind
    /// [`FrameGate`].
    ///
    /// `None` for a stamp no listing of this channel named, and `None` for one
    /// already staged — both are the contract's own answers, and neither is a
    /// throttle. The throttle is inside the task: it takes the gate before it
    /// touches the network, so the whole render set may be dispatched at once
    /// (which it is — `dispatch_loop_frame_fetches` has no throttle of its
    /// own) while only one 60 MB decode exists at a time.
    fn fetch_frame(
        &self,
        ctx: &FetchConfig,
        pane: &PaneRef<'_>,
        stamp: &FrameStamp,
    ) -> Option<FetchTask> {
        let channel = self.view(pane).selected_channel;
        let key = FrameKey {
            channel,
            valid: stamp.valid,
        };
        if self.frame_grids.entries.contains_key(&key) {
            return None;
        }
        let object = self.frame_keys.get(&channel)?.get(&stamp.valid)?.clone();
        let client = ctx.client.clone();
        let sources = ctx.sources.clone();
        let gate = Arc::clone(&self.frame_gate);
        let valid = stamp.valid;
        let future: TaskFuture = Box::pin(async move {
            let _one_at_a_time = gate.lock().await;
            let grid = match crate::gmgsi::fetch::fetch_key(&client, &sources, channel, &object)
                .await
            {
                Ok(grid) => Some(grid),
                Err(e) => {
                    log::error!("GMGSI frame fetch failed for {channel:?} valid {valid}: {e:?}");
                    None
                }
            };
            Box::new(GmgsiFrameFetch {
                channel,
                valid,
                grid,
            }) as FetchPayload
        });
        Some(FetchTask {
            kind: known::GMGSI,
            future,
        })
    }

    /// **The staged granules of this pane's own channel** — at most one, by
    /// [`FRAME_STAGING_BYTES`], however many frames the loop holds.
    ///
    /// This pane's channel and not the whole store: another pane's Water
    /// Vapour granule is not a frame this pane can draw, and offering it would
    /// have the dispatch describe a job that paints the wrong band.
    fn frames_resident(&self, pane: &PaneRef<'_>) -> Vec<FrameStamp> {
        let channel = self.view(pane).selected_channel;
        let mut frames: Vec<FrameStamp> = self
            .frame_grids
            .entries
            .keys()
            .filter(|key| key.channel == channel)
            .map(|key| FrameStamp {
                valid: key.valid,
                run: None,
            })
            .collect();
        frames.sort_by_key(|stamp| stamp.valid);
        frames
    }

    /// Drop every staged granule not in `keep`, matching on the hour alone —
    /// a caller's stamps carry the `run: None` this layer names, and a stamp
    /// rebuilt from a bare instant carries it too.
    ///
    /// **Nothing above calls this yet** (the one production frame-eviction
    /// authority is still the byte budget inside each layer), so it is exercised
    /// by this layer's own suite rather than by the loop.
    fn retain_frames(&mut self, pane: &PaneRef<'_>, keep: &[FrameStamp]) {
        let channel = self.view(pane).selected_channel;
        self.frame_grids.retain(|key| {
            key.channel != channel || keep.iter().any(|stamp| stamp.valid == key.valid)
        });
    }

    /// File a listing under the channel it was **dispatched for**, never the
    /// one the arriving pane holds: a `PaneRef` on this path is the union
    /// across panes and its config is null by construction.
    fn apply_frame_listing(
        &mut self,
        _listing: FrameListing,
        scope: FetchPayload,
        _pane: &PaneRef<'_>,
    ) {
        let Ok(scope) = scope.downcast::<GmgsiListing>() else {
            log::error!("a frame listing reached the GMGSI layer under another layer's scope");
            return;
        };
        let keys = self.frame_keys.entry(scope.channel).or_default();
        for (valid, object) in scope.keys {
            keys.insert(valid, object);
        }
        // Coverage only for a listing that really covered the window: a failed
        // or sampled one must not leave `list_frames` claiming it is settled.
        if scope.complete && !self.covers(scope.channel, scope.range) {
            self.covered
                .entry(scope.channel)
                .or_default()
                .push(scope.range);
        }
    }

    /// Stage one frame's granule under the `(channel, hour)` its fetch was
    /// dispatched for. A failed fetch stages nothing and the frame keeps no
    /// picture.
    fn apply_frame(&mut self, _stamp: FrameStamp, data: FetchPayload, _pane: &PaneRef<'_>) {
        let Ok(frame) = data.downcast::<GmgsiFrameFetch>() else {
            log::error!("a frame reached the GMGSI layer under another layer's payload");
            return;
        };
        let GmgsiFrameFetch {
            channel,
            valid,
            grid,
        } = *frame;
        let Some(granule) = grid else {
            return;
        };
        self.frame_grids.insert(
            FrameKey { channel, valid },
            GmgsiGranule {
                grid: Arc::new(granule.grid),
                bounds: granule.bounds,
                valid_time: granule.valid_time,
            },
        );
    }

    /// **Zero, and it is a decision rather than an omission.** A blended
    /// mosaic is made *from* observations, so no granule exists for an hour
    /// that has not happened — which is the same fact
    /// [`OverlayHandler::time_axis`] states as `extends_future: false`. The
    /// rail must not offer this layer a stop past the wall clock.
    fn frame_horizon(&self, _pane: &PaneRef<'_>) -> chrono::Duration {
        chrono::Duration::zero()
    }
}

impl OverlayHandler for GmgsiHandler {
    /// The GMGSI channels this layer offers, projected into the substrate's
    /// read contract by [`crate::gmgsi::fields`].
    fn products(&self) -> &'static [squallar_source::product::ProductSpec] {
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
    fn current_field(&self, pane: &PaneRef<'_>) -> Option<squallar_source::product::FieldId> {
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

    // -- Time ---------------------------------------------------------------

    /// One blended mosaic per hour, stamped with the hour it depicts, and
    /// never one for an hour that has not happened: discrete frames that stop
    /// at the wall clock.
    fn time_axis(&self) -> TimeAxis {
        TimeAxis::FrameSeries {
            typical_step: std::time::Duration::from_secs(3600),
            extends_future: false,
        }
    }

    /// **Thirteen hourly mosaics — half a day of cloud, which is the shortest
    /// window a global satellite loop says anything with.**
    ///
    /// The same number the model layer declares, for the same arithmetic: at
    /// this layer's hourly step it is a twelve-hour window, `n - 1` steps end
    /// to end. Without it the Lookback slider's own default of 3600 s buys
    /// **two** frames here while buying a dozen radar volumes, and two frames
    /// is a before-and-after rather than a loop.
    ///
    /// A floor and not the window: drag Lookback past twelve hours and this
    /// layer widens with everything else.
    fn min_loop_frames(&self) -> usize {
        13
    }

    /// **The granules these stops draw from, and none of the hours between
    /// them.**
    ///
    /// A twelve-hour loop of thirteen hourly stops needs thirteen mosaics;
    /// the twelve hours they are spread across is archive nothing in this
    /// pane depicts. [`squallar_source::time::frame_residency`] is that
    /// derivation, shared by all four framed layers and routed through
    /// [`Self::latest_at`], so this is not a second reading of
    /// `FrameSeries`'s rule.
    ///
    /// Empty before a listing lands — this layer knows of no granule then, so
    /// there is none it can ask to keep. That is a state and not a silence:
    /// the same call after `apply_frame_listing` answers thirteen ranges.
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
    /// [`squallar_netcdf::cf::unpack`] marked it missing and [`crate::gmgsi::decode`]
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
    ///
    /// **A named frame is drawn from that frame's own granule.** `ctx.frame` is
    /// read before the pane's selection, and the lookup goes to the staging
    /// store keyed by the hour the stamp names — never to the live cache,
    /// which holds one granule per channel and would hand every frame of a
    /// loop the same picture.
    ///
    /// A named frame whose granule is not staged **describes no job at all**
    /// rather than falling back to the pane's own picture. That fallback is
    /// one hour's satellite image presented, unlabelled, as another's, and it
    /// is the exact defect the frame-addressed dispatch exists to prevent.
    ///
    /// The refcount is also what frees the staging slot: the job owns the
    /// raster from here, so the store may evict the entry on the very next
    /// arrival without the raster going anywhere.
    fn prepare_job(&self, ctx: &RasterizeContext, pane: &PaneRef<'_>) -> Option<DescribedJob> {
        let channel = self.view(pane).selected_channel;
        let granule = match ctx.frame {
            Some(stamp) => self.frame_grids.get(FrameKey {
                channel,
                valid: stamp.valid,
            })?,
            None => self.cached_grids.get(channel)?,
        };
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
        // THE INSTANT THIS PANE DEPICTS, not the wall clock. `fetch_latest`
        // already walks hourly prefixes backwards from whatever it is given, so
        // handing it the depicted instant is the whole of this layer's answer
        // to "what do you show at T" — it was simply being handed `now`.
        // Captured here rather than inside the future so the instant the
        // listing walks back from is the instant the round was asked for.
        let now = ctx.as_of;
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
