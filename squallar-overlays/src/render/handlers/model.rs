use crate::render::overlay_state::{PaneMut, PaneRef};
use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;

use crate::fetch_policy::Whole;
use crate::hrrr::{HrrrFetchResult, HrrrGridData, ModelParameter};
use crate::render::controls::{
    ControlButton, ControlEffect, ControlItem, ControlUpdate, ControlValue,
};
use crate::render::overlay_state::Surface;
use crate::render::overlay_state::{
    FetchConfig, FetchPayload, FetchTask, FrameListingResult, OverlayHandler, OverlayLegend,
    OverlayState, RasterizeContext, RenderMode, Signed,
};
use crate::render::rasterize;
use chrono::Timelike;
use squallar_source::id::{LayerId, known};
use squallar_source::job::{DescribedJob, JobCodec};
use squallar_source::time::{FrameListing, FrameSource, FrameStamp, TimeAxis};

/// **The one identity of a decoded HRRR grid**: which field, off which run, at
/// which forecast hour.
///
/// The parameter alone was the key until the layer supplied frames, and the
/// cache was documented as run-blind — which was exactly right while every
/// fetch asked for the latest run at the parameter's own floor, and is exactly
/// wrong now that a scrub asks for f00 and f18 of the same run, or the same
/// hour of two runs, and expects two pictures.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(crate) struct GridKey {
    pub param: ModelParameter,
    pub run: chrono::NaiveDateTime,
    pub f_hour: u8,
}

impl GridKey {
    /// The key a decoded grid files itself under — read off the grid, never
    /// spelled beside it, so a fetch whose hour was clamped up to the
    /// parameter's floor cannot be filed under the hour it asked for.
    fn of(grid: &HrrrGridData) -> Self {
        Self {
            param: grid.parameter,
            run: grid.ref_time,
            f_hour: grid.forecast_hour,
        }
    }
}

/// What one resident grid really costs, in bytes.
///
/// `values` is the whole figure in practice — 1,905,141 values at
/// [`HrrrGridData::ELEMENT_BYTES`] = **7,620,564 bytes** on the CONUS grid —
/// but the `Explicit` coordinate arm is counted too rather than assumed away:
/// it is 30.5 MB at that size, four times the values, and it is what a
/// non-Lambert HRRR-shaped source would arrive on.
///
/// **The values term is read off the store, not spelled here.** It was
/// `grid.values.len() * size_of::<f32>()`, and a literal width beside a field
/// that owns the real one is a figure that goes on compiling while the store
/// under it moves — the way [`super::gmgsi::GLOBAL_GRID_BYTES`] went on
/// pricing four bytes a point after its mosaic narrowed to one, with its own
/// `== N` pin still green. `size_of_val` over the slice reads the field's own
/// element, so this figure cannot disagree with the store; and — measured, not
/// asserted — it stops compiling outright if `values` ever becomes a tagged
/// [`GridValues`](crate::render::gridded::GridValues) rather than narrowing in
/// place, which is the swap the old spelling accepted in silence.
///
/// **The coordinate terms are still literals**, and knowingly: `lats`,
/// `lons` and the two axes are geodetic `f64`, where the width is a precision
/// requirement rather than a storage choice, and narrowing one would redden
/// `squallar-geo` long before it reached a byte figure. Same shape, different
/// risk; stated rather than left to read as an oversight.
fn grid_bytes(grid: &HrrrGridData) -> usize {
    let coords = match &grid.coords {
        crate::hrrr::GridCoords::Explicit { lats, lons } => {
            (lats.len() + lons.len()) * std::mem::size_of::<f64>()
        }
        // One entry per row plus one per column, not one pair per point: 64 KB
        // on GMGSI's 3000 x 5000 grid, where `Explicit` would be 240 MB.
        crate::hrrr::GridCoords::Separable { lat_axis, lon_axis } => {
            (lat_axis.len() + lon_axis.len()) * std::mem::size_of::<f64>()
        }
        // Closed forms: the whole grid is its scalars, already counted by the
        // `size_of` below.
        crate::hrrr::GridCoords::Lambert(_) | crate::hrrr::GridCoords::Regular { .. } => 0,
    };
    std::mem::size_of::<HrrrGridData>() + std::mem::size_of_val(grid.values.as_slice()) + coords
}

/// **How many bytes of decoded grid this target keeps resident**, across every
/// pane.
///
/// A *byte* budget and not an entry count, because [`GridKey`] carries a run
/// and a forecast hour: a 24-hour scrub over six panes names 144 distinct
/// grids, which an entry cap of six counts as "six" and a byte budget counts
/// as 1.1 GB. The cap it replaces was sized by the pane count and said nothing
/// at all about memory.
///
/// At 7,620,564 bytes per CONUS grid the three arms buy:
///
/// | target | budget | grids | pane cap | spare |
/// |---|---|---|---|---|
/// | wasm32 | 64 MiB | 8 | 6 | 2 |
/// | mobile | 64 MiB | 8 | 4 | 4 |
/// | desktop | 96 MiB | 13 | 6 | 7 |
///
/// **The spare column is the whole of the scrub history**, and it is what
/// these budgets are now sized for: the pinned set is one grid per enabled
/// pane, and everything above it is hours a user stepped off and may step
/// back onto. Before the loop frames were staged
/// ([`MODEL_FRAME_STAGING_BYTES`]) this cache was also where a loop's frames
/// lived, so the arms were 96/192/512 MiB — 13, 26 and 70 grids — and a
/// 60-frame desktop loop really did fill 436.1 MiB of it. Nothing spends
/// against loop length here any more, so the budget is the pane set plus a
/// history, and the arms come down by 32, 128 and 416 MiB.
///
/// The figure is not only what the heap holds: `source_grid_budget_bytes`
/// hands it to the admission door as what a pane enabling this layer asks the
/// heap to be able to hold, so an over-large budget is priced against every
/// other layer whether or not a grid ever arrives.
///
/// **Never below the pane count.** Below it the budget stops being one: every
/// pane's key is pinned, `ModelGridCache::insert` runs out of unpinned victims
/// and takes its `break` arm, and the cache holds one grid per pane anyway
/// while this constant says less — a silent *overrun*, not a blank pane, since
/// a pinned grid is never a victim and `prepare_job` keeps answering. The
/// `const _` below holds the floor as a build failure;
/// `the_byte_budget_holds_at_least_one_grid_per_pane` restates it from a host
/// test, which is why each arm is a named constant rather than only a `cfg`
/// arm.
///
/// **Spelled here rather than read from `squallar-device-profile`**, where the
/// rest of this application's budgets live. That crate declares
/// `squallar-radar`, so an overlays → device-profile edge puts the whole radar
/// pipeline back into every overlay handler's compile graph — the edge
/// `squallar-source/tests/charter.rs::the_overlays_to_radar_edge_stays_cut`
/// exists to keep cut, and which today leaves this crate standing on exactly
/// `{squallar-geo, squallar-source, squallar-units}`. It is also why the pane cap
/// below is spelled and not imported from `squallar-egui`.
pub const WASM_MODEL_GRID_BUDGET_BYTES: usize = 64 * 1024 * 1024;
pub const MOBILE_MODEL_GRID_BUDGET_BYTES: usize = 64 * 1024 * 1024;
pub const DESKTOP_MODEL_GRID_BUDGET_BYTES: usize = 96 * 1024 * 1024;

/// The pane cap the budget is measured against. Spelled, for the reason above;
/// `squallar_device_profile::budget::MAX_PANES_DESKTOP` = 6 and
/// `MAX_PANES_MOBILE` = 4 are the definitions.
pub const MAX_PANES_DESKTOP: usize = 6;
pub const MAX_PANES_MOBILE: usize = 4;

/// What one CONUS grid costs, as [`grid_bytes`] counts it: 1799 × 1059 =
/// 1,905,141 values at [`HrrrGridData::ELEMENT_BYTES`], on a `Lambert`
/// coordinate arm that adds nothing. Measured 2026-08-21; the opening
/// assertion of `the_byte_budget_holds_at_least_one_grid_per_pane` is what
/// keeps it from drifting away from the function.
///
/// **The width is the store's, not a literal `4`.** It was, and that made this
/// the denominator of every budget below without depending on the thing it
/// prices: the shape moved with the product and the width moved with nothing.
/// The test that holds this equal to [`grid_bytes`] could not tell, because
/// both sides were the same literal — a mutual-consistency pin over two copies
/// of one constant, which is exactly the arrangement that let
/// [`super::gmgsi::GLOBAL_GRID_BYTES`] and `mrms::volume::CONUS_STACK_BYTES`
/// each survive a change to their own store.
pub const HRRR_CONUS_GRID_BYTES: usize = 1799 * 1059 * HrrrGridData::ELEMENT_BYTES;

// **The two terms pinned APART**, so a build failure names which one moved
// rather than only that a total did, and so no reader can retype one figure
// green. Neither pin existed before: the constant was arithmetic over a
// literal, so an `== N` over it would have been one literal checked against
// another. The width pin is the one the old spelling could not have had.
const _: () = assert!(HrrrGridData::ELEMENT_BYTES == 4);
const _: () = assert!(HRRR_CONUS_GRID_BYTES == 7_620_564);

/// **The floor, as a build failure.** A budget below one grid per pane is
/// overrun *silently* — the pinned keys are never victims, so the cache holds
/// them past the figure and the figure under-reports the heap — so it is
/// caught here rather than in a test somebody has to have run on the right
/// target.
const _: () = {
    assert!(WASM_MODEL_GRID_BUDGET_BYTES / HRRR_CONUS_GRID_BYTES >= MAX_PANES_DESKTOP);
    assert!(MOBILE_MODEL_GRID_BUDGET_BYTES / HRRR_CONUS_GRID_BYTES >= MAX_PANES_MOBILE);
    assert!(DESKTOP_MODEL_GRID_BUDGET_BYTES / HRRR_CONUS_GRID_BYTES >= MAX_PANES_DESKTOP);
};

/// **What one frame of a model loop passes through on its way to a texture.**
///
/// One CONUS grid, on every arm, however many frames the loop holds — the
/// same shape as [`crate::mrms::FRAME_STAGING_BYTES`] and
/// [`super::gmgsi::FRAME_STAGING_BYTES`], and for the same reason: a loop
/// frame's storage is its *texture*, held by the pane, and the grid is only
/// what one frame is rasterized from. [`ModelDataHandler::frame_gate`]
/// serialises the decodes, so this is what the layer holds rather than what
/// it has in flight.
///
/// **This replaces `model_loop_frame_cap`**, which computed how many frames a
/// model loop could hold against the grid budget — 8, 23 and 65 on the three
/// arms. That function had no production caller: `layer_share` builds an
/// overlay loop's frame list with `count_cap: None` and prices one frame at
/// `overlay_frame_price`, which is the *texture*, so the 7.6 MB grid each
/// frame pinned was in no bound the loop was ever built under. The question
/// it answered is gone rather than re-sited — with a loop frame staged
/// through one grid, how many frames a loop holds no longer bears on the grid
/// budget at all.
pub const MODEL_FRAME_STAGING_BYTES: usize = HRRR_CONUS_GRID_BYTES;

// The device class, spelled as its rule rather than as the `mobile` cfg:
// cargo scopes a build script's cfgs to the crate that declares it, so
// `cfg(mobile)` is unset everywhere outside `squallar-device-profile`, and a
// `#[cfg(mobile)]` written here would be silently false on a handheld. The
// rule is `squallar-device-profile/src/mobile_cfg.rs::is_mobile_target` —
// `target_os` in {android, ios}. Selecting a value, never forking behaviour.
#[cfg(target_arch = "wasm32")]
pub(crate) const MODEL_GRID_BUDGET_BYTES: usize = WASM_MODEL_GRID_BUDGET_BYTES;
#[cfg(all(
    not(target_arch = "wasm32"),
    any(target_os = "android", target_os = "ios")
))]
pub(crate) const MODEL_GRID_BUDGET_BYTES: usize = MOBILE_MODEL_GRID_BUDGET_BYTES;
#[cfg(all(
    not(target_arch = "wasm32"),
    not(any(target_os = "android", target_os = "ios"))
))]
pub(crate) const MODEL_GRID_BUDGET_BYTES: usize = DESKTOP_MODEL_GRID_BUDGET_BYTES;

/// The resident grids, bounded by bytes and evicted least-recently-touched
/// first.
///
/// An entries map plus a recency list holding exactly the keys of `entries`,
/// oldest use first; both private so no caller can desynchronise them. The list
/// is behind a `RefCell` because every *reader* reaches it through an `&self`
/// method of [`OverlayHandler`], and a lookup that did not count as a use would
/// let the pane on screen age out.
///
/// **No history budget, unlike the MRMS and GMGSI grid caches.** Theirs key by
/// the layer's selectable product — two and four — so "how many unpinned grids
/// may stay beyond the pinned set" is a small count a governor can lower and
/// restore. This cache keys by [`GridKey`]: parameter, run *and* forecast
/// hour, and a scrub across a run's hours or a run rolling over mints keys
/// without bound. An unpinned count is not a count of anything stable here, so
/// the byte budget is the whole of the lever.
struct ModelGridCache {
    entries: HashMap<GridKey, Arc<HrrrGridData>>,
    recency: RefCell<Vec<GridKey>>,
    /// Sum of [`grid_bytes`] over `entries`, maintained on every insert and
    /// eviction rather than recomputed: an eviction loop that re-walked the
    /// map would be O(n²) in the resident set.
    bytes: usize,
    /// The ceiling `bytes` is held under. A field and not the constant so a
    /// test can state a budget in whole grids of its own fixture size — the
    /// production value is [`MODEL_GRID_BUDGET_BYTES`].
    budget: usize,
}

impl ModelGridCache {
    fn new() -> Self {
        Self::with_budget(MODEL_GRID_BUDGET_BYTES)
    }

    fn with_budget(budget: usize) -> Self {
        Self {
            entries: HashMap::new(),
            recency: RefCell::new(Vec::new()),
            bytes: 0,
            budget,
        }
    }

    fn touch(&self, key: GridKey) {
        let mut recency = self.recency.borrow_mut();
        if let Some(pos) = recency.iter().position(|k| *k == key) {
            recency.remove(pos);
            recency.push(key);
        }
    }

    fn get(&self, key: GridKey) -> Option<&Arc<HrrrGridData>> {
        let grid = self.entries.get(&key)?;
        self.touch(key);
        Some(grid)
    }

    /// Whether `key`'s grid is resident, marking it most-recently-used.
    ///
    /// Every read counts as a use, including the bare predicate: which accessor
    /// a caller reached for is not a fact about what the user is looking at.
    fn contains(&self, key: GridKey) -> bool {
        self.get(key).is_some()
    }

    /// The maintained sum of [`grid_bytes`] over the resident entries — the
    /// figure the budget is spent against, read rather than recomputed.
    fn resident_bytes(&self) -> usize {
        self.bytes
    }

    /// Whether one of the resident entries **is this very allocation**.
    ///
    /// Deliberately not [`Self::contains`]: this is an accounting question
    /// about a pointer, not a look at a picture, and answering it through the
    /// keyed accessor would mark a grid most-recently-used and reorder the
    /// eviction queue behind a census read. Pointer identity rather than the
    /// key for the same reason — the pane's carry can outlive its cache entry,
    /// and then the key matches while the allocation does not.
    fn holds(&self, grid: &Arc<HrrrGridData>) -> bool {
        self.entries.values().any(|g| Arc::ptr_eq(g, grid))
    }

    /// **The most recently used resident grid of `param`**, whatever run and
    /// hour it is off.
    ///
    /// The fallback for a pane that has not been parked on a frame — which is
    /// every pane until a clock moves it. Not a use of its own: it is a search
    /// over keys, and the caller reads the answer back through [`Self::get`].
    fn latest_of(&self, param: ModelParameter) -> Option<GridKey> {
        self.recency
            .borrow()
            .iter()
            .rev()
            .find(|key| key.param == param)
            .copied()
    }

    /// [`Self::latest_of`], but never past `ceiling`.
    ///
    /// This cache is shared by every pane and ordered by **use**, so
    /// `latest_of` will hand a pane parked in April a grid another pane pulled
    /// off today's run — the two panes differ in nothing this search can see.
    /// A pane resolving its own run through [`ModelDataHandler::run_at`] passes
    /// the newest run its depicted instant could name, and a live pane's
    /// ceiling is at or above every run the cache can hold, so its answer is
    /// `latest_of`'s unchanged.
    fn latest_not_after(
        &self,
        param: ModelParameter,
        ceiling: chrono::NaiveDateTime,
    ) -> Option<GridKey> {
        self.recency
            .borrow()
            .iter()
            .rev()
            .find(|key| key.param == param && key.run <= ceiling)
            .copied()
    }

    /// Neither the entry going in nor anything in `pinned` is ever evicted.
    ///
    /// `pinned` is the **union** of every ENABLED pane's current key, not one pane's:
    /// this cache is shared by every pane, and evicting what another pane is
    /// showing to make room is the cross-pane collision the pane state exists
    /// to prevent. The budget being at least one grid per pane is what makes
    /// the union fit; the pin is what makes it hold when an arrival lands
    /// mid-cycle.
    ///
    /// An arrival that alone exceeds the budget is still installed: the loop
    /// stops when it runs out of unpinned victims, because a pane with no grid
    /// draws nothing and has nothing to re-ask.
    fn insert(&mut self, key: GridKey, grid: Arc<HrrrGridData>, pinned: &[GridKey]) {
        let cost = grid_bytes(&grid);
        match self.entries.insert(key, grid) {
            // A re-fetch of a resident key replaces its own entry — same
            // parameter, same run, same forecast hour. Another run of the same
            // parameter is a different key and lands beside it.
            Some(old) => {
                self.bytes = self.bytes - grid_bytes(&old) + cost;
                self.touch(key);
            }
            None => {
                self.bytes += cost;
                self.recency.borrow_mut().push(key);
            }
        }
        while self.bytes > self.budget {
            let victim = {
                let mut recency = self.recency.borrow_mut();
                let Some(pos) = recency
                    .iter()
                    .position(|k| *k != key && !pinned.contains(k))
                else {
                    break;
                };
                recency.remove(pos)
            };
            if let Some(grid) = self.entries.remove(&victim) {
                self.bytes -= grid_bytes(&grid);
            }
        }
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether `key` is resident, **without** marking it used — the
    /// non-touching twin of [`Self::contains`], for
    /// [`ModelFrameCache::is_staged`]'s reason: `fetch_frame` asking "must
    /// this be fetched" is not a look at a picture, and answering it through
    /// the touching accessor would let the dispatcher's walk reorder the
    /// eviction queue behind the pane on the glass.
    fn is_resident(&self, key: GridKey) -> bool {
        self.entries.contains_key(&key)
    }

    /// **Drop every resident grid**, answering whether any were held.
    ///
    /// The pin set is not consulted, and that is the point rather than an
    /// oversight: the one caller is [`ModelDataHandler::release_data`], which
    /// runs only for a layer **no pane draws**, so there is nothing on the
    /// glass for a pin to protect. `pinned_keys` already skips a disabled
    /// pane, so on this path it answers empty anyway.
    fn release_all(&mut self) -> bool {
        if self.entries.is_empty() {
            return false;
        }
        self.entries.clear();
        self.recency.borrow_mut().clear();
        self.bytes = 0;
        true
    }

    /// The eviction order, projected to parameters: the suite that asks about
    /// it predates the run and the forecast hour being in the key, and is
    /// asking about *order* rather than about identity.
    #[cfg(test)]
    fn recency_params(&self) -> Vec<ModelParameter> {
        self.recency.borrow().iter().map(|k| k.param).collect()
    }
}

/// **One frame's grid at a time, application-wide** — the same gate MRMS and
/// GMGSI hold their loop-frame decodes behind, and needed here for the same
/// reason: `dispatch_loop_frame_fetches` has no throttle of its own, so a
/// forecast loop puts its whole render set on the wire at once. Forty-nine
/// unthrottled decodes — f00 to f48 of one run — would hold 373.4 MB of
/// decoded grid in flight before any cache saw a byte, which is the figure
/// this whole item exists to remove.
///
/// Serialising costs almost no wall time: the bytes are the bottleneck either
/// way, and FIFO fairness means grids arrive in render-set order, which is
/// playhead outward.
type FrameGate = Arc<futures::lock::Mutex<()>>;

/// The staged loop-frame grids, bounded by **bytes** and evicted
/// least-recently-used first.
///
/// Deliberately **not** [`ModelGridCache`], for the reason
/// [`super::mrms::MrmsFrameCache`] is not that layer's live cache: this holds
/// what one loop frame is passing through on its way to a texture, and the
/// live cache holds what the panes are showing. Two stores, two purposes, two
/// budgets — and the live one is bounded by the pane set rather than by how
/// long a loop is.
///
/// **The same key space as the live cache**, which the mosaic layers cannot
/// have: MRMS keys its live cache by product and its frame cache by
/// `(product, stamp)`, so it must invent a second key. A model grid is
/// `(param, run, hour)` whether a pane parked on it or a loop asked for it,
/// so [`GridKey`] serves both — and that is what lets the readers below check
/// one store and then the other rather than choosing between them.
struct ModelFrameCache {
    entries: HashMap<GridKey, Arc<HrrrGridData>>,
    recency: RefCell<Vec<GridKey>>,
    /// Sum of [`grid_bytes`] over `entries`, maintained rather than
    /// recomputed — see [`ModelGridCache::bytes`].
    bytes: usize,
    /// The ceiling `bytes` is held under. A field and not the constant so a
    /// test can state a budget in whole grids of its own fixture size — the
    /// production value is [`MODEL_FRAME_STAGING_BYTES`].
    budget: usize,
}

impl ModelFrameCache {
    fn new() -> Self {
        Self::with_budget(MODEL_FRAME_STAGING_BYTES)
    }

    fn with_budget(budget: usize) -> Self {
        Self {
            entries: HashMap::new(),
            recency: RefCell::new(Vec::new()),
            bytes: 0,
            budget,
        }
    }

    fn touch(&self, key: GridKey) {
        let mut recency = self.recency.borrow_mut();
        if let Some(pos) = recency.iter().position(|k| *k == key) {
            recency.remove(pos);
            recency.push(key);
        }
    }

    fn get(&self, key: GridKey) -> Option<&Arc<HrrrGridData>> {
        let grid = self.entries.get(&key)?;
        self.touch(key);
        Some(grid)
    }

    /// Whether `key` is staged, **without** marking it used.
    ///
    /// The opposite choice to [`ModelGridCache::contains`], and the difference
    /// is what each question is for: there, a bare predicate is still a look
    /// at a picture, so it counts as a use. Here every caller is asking "must
    /// this frame be fetched" — `fetch_frame`'s early-out and
    /// `frames_resident`'s walk — and answering it through the touching
    /// accessor would let the frame the dispatcher happened to ask about last
    /// outlive the one it is about to rasterize.
    fn is_staged(&self, key: GridKey) -> bool {
        self.entries.contains_key(&key)
    }

    fn resident_bytes(&self) -> usize {
        self.bytes
    }

    /// Whether one of the staged entries **is this very allocation** — the
    /// accounting question, answered on pointer identity for the reason at
    /// [`ModelGridCache::holds`].
    fn holds(&self, grid: &Arc<HrrrGridData>) -> bool {
        self.entries.values().any(|g| Arc::ptr_eq(g, grid))
    }

    /// Stage `key`'s grid, evicting least-recently-used until the budget is
    /// met.
    ///
    /// **Nothing is pinned here**, unlike the live cache. A staged grid that
    /// is evicted before its frame was rasterized is not a blank pane: the
    /// frame keeps no picture, `frames_resident` stops naming it, and
    /// `refetch_owed_loop_frames` asks for it again on the next pass. That
    /// supply is the app-side half of this design and it already exists — it
    /// is what carries MRMS and GMGSI, whose staging area is one granule too.
    ///
    /// The arrival is never its own victim, so a budget smaller than one grid
    /// stages one anyway rather than staging nothing.
    fn insert(&mut self, key: GridKey, grid: Arc<HrrrGridData>) {
        let cost = grid_bytes(&grid);
        match self.entries.insert(key, grid) {
            Some(old) => {
                self.bytes = self.bytes - grid_bytes(&old) + cost;
                self.touch(key);
            }
            None => {
                self.bytes += cost;
                self.recency.borrow_mut().push(key);
            }
        }
        while self.bytes > self.budget {
            let victim = {
                let mut recency = self.recency.borrow_mut();
                let Some(pos) = recency.iter().position(|k| *k != key) else {
                    break;
                };
                recency.remove(pos)
            };
            if let Some(grid) = self.entries.remove(&victim) {
                self.bytes -= grid_bytes(&grid);
            }
        }
    }

    /// Drop every staged grid `keep` does not name, answering whether
    /// anything went.
    fn retain(&mut self, keep: impl Fn(GridKey) -> bool) -> bool {
        let doomed: Vec<GridKey> = self
            .entries
            .keys()
            .copied()
            .filter(|key| !keep(*key))
            .collect();
        for key in &doomed {
            if let Some(grid) = self.entries.remove(key) {
                self.bytes -= grid_bytes(&grid);
            }
        }
        self.recency.borrow_mut().retain(|key| keep(*key));
        !doomed.is_empty()
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.entries.len()
    }
}

/// **Which way time runs for this pane's model layer.**
///
/// One run's forecast hours, or one hour of many runs — two different sets of
/// frames over the same archive, and the pane picks one. They are not two
/// spellings of the same axis: `Forecast` is a closed form of the run and
/// needs no network at all, `Analysis` is a bucket listing.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub enum ModelAxis {
    /// f`min`..=f`horizon` of **one** run. See [`forecast_horizon`].
    #[default]
    Forecast,
    /// f00 of **many** runs — the analysis hour, walking backwards through the
    /// archive.
    Analysis,
}

impl ModelAxis {
    /// The persisted spelling. Saved config is matched on this, so it is not a
    /// display string and must not be reworded.
    pub fn as_str(self) -> &'static str {
        match self {
            ModelAxis::Forecast => "forecast",
            ModelAxis::Analysis => "analysis",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            ModelAxis::Forecast => "Forecast hours",
            ModelAxis::Analysis => "Past runs",
        }
    }

    /// `None` for anything this build does not name — a saved config from a
    /// later version keeps its pane on the default rather than failing to load.
    pub fn parse(text: &str) -> Option<Self> {
        [ModelAxis::Forecast, ModelAxis::Analysis]
            .into_iter()
            .find(|axis| axis.as_str() == text)
    }

    pub fn all() -> [ModelAxis; 2] {
        [ModelAxis::Forecast, ModelAxis::Analysis]
    }
}

/// **The last forecast hour a run publishes.**
///
/// Measured against the live archive on 2026-08-21: f00–f48 on the 00/06/12/18Z
/// cycles, f00–f18 on every other hour, `.idx` present for every hour, archive
/// back to `hrrr.20140730`. Taken from the run's *hour*, not from a table of
/// dates: the cycle is what decides, and it has been the same cycle since the
/// bucket opened.
pub fn forecast_horizon(run: chrono::NaiveDateTime) -> u8 {
    use chrono::Timelike;
    if run.hour().is_multiple_of(6) { 48 } else { 18 }
}

/// **How many past runs the run control lists** below `Latest`, newest first.
/// Twelve spans two whole synoptic cycles, so the 48-hour horizon of the most
/// recent 00/06/12/18Z run is always reachable from the menu.
const RUN_CHOICES: u8 = 12;

/// **The furthest back a run choice can be spelled at all**, in hours before
/// the latest run.
///
/// A bound on the vocabulary, not on the archive: a run is saved as a
/// *relative* choice, so a pane left parked further back than this comes back
/// on `Latest` rather than on an offset nothing can express.
const MAX_RUN_OFFSET: u8 = 48;

/// The run control's unpinned value: **`Latest`**, which is
/// `selected_frame == None` — the run is read off the clock at each fetch
/// rather than frozen at the moment the choice was made.
const RUN_LATEST: &str = "";

/// The stem every pinned run token is built on: `latest`, `latest-1`, ... The
/// offset is in hours, because hours are what the HRRR cycle is.
const RUN_STEM: &str = "latest";

/// The spelling an absolute run is accepted in, and the prefix that makes it
/// distinguishable from the one older builds wrote.
///
/// THE PREFIX IS THE WHOLE POINT. Builds before the relative encoding saved a
/// bare `"2026-07-25T03:00:00"`, and those must still resolve to nothing —
/// reopening a Friday forecast on Monday is the failure that encoding exists to
/// prevent, and it is pinned by `a_stale_absolute_run_does_not_survive_a_restart`.
/// A bare instant is therefore indistinguishable from a stale one and stays
/// refused; `at:` cannot appear in any config an older build produced, so it can
/// only mean a human wrote it on purpose.
const ABSOLUTE_RUN_PREFIX: &str = "at:";
const ABSOLUTE_RUN_FORMAT: &str = "%Y-%m-%dT%H:%M:%S";

/// [`crate::hrrr::fetch::run_for`] as an instant. `run_for` answers
/// `(date, hour)` because that is what a bucket key is spelled from; a
/// selection is an instant.
fn latest_run_at(now: chrono::NaiveDateTime) -> chrono::NaiveDateTime {
    let (date, hour) = crate::hrrr::fetch::run_for(now);
    date.and_hms_opt(u32::from(hour), 0, 0)
        .expect("run_for reports a wall-clock hour")
}

/// The forecast-hour control's value for hour `f_hour`.
///
/// `f06`, not `6`: a dropdown's option *values* must not be strings the rest
/// of the frame also paints, or
/// `a_dropdown_shows_its_option_label_not_the_raw_value` cannot tell a raw id
/// leaking into the list from any other widget that happens to draw `0`.
fn f_hour_token(f_hour: u8) -> String {
    format!("f{f_hour:02}")
}

fn parse_f_hour(text: &str) -> Option<u8> {
    text.strip_prefix('f')?.parse().ok()
}

/// The token for the run `back` hours before the latest one.
fn run_token(back: u8) -> String {
    if back == 0 {
        RUN_STEM.to_string()
    } else {
        format!("{RUN_STEM}-{back}")
    }
}

/// **A run as a relative choice**, or `None` when it lies further back than
/// the vocabulary reaches — or ahead of the latest run, which only a clock
/// that went backwards produces.
fn encode_run_choice(run: chrono::NaiveDateTime, now: chrono::NaiveDateTime) -> Option<String> {
    let back = u8::try_from((latest_run_at(now) - run).num_hours()).ok()?;
    (back <= MAX_RUN_OFFSET).then(|| run_token(back))
}

/// **A choice as a run**, against the clock that is reading it.
///
/// Two spellings, and the asymmetry between them is the design.
///
/// A RELATIVE token (`latest`, `latest-6`) is what [`encode_run_choice`] still
/// writes, and it carries the original reason: a config closed on Friday with
/// 18Z picked must not reopen on Monday three days in the past with nothing but
/// a small label to say so. Every save this build makes is relative, so nothing
/// about an ordinary session changed.
///
/// An ABSOLUTE instant, written `at:2020-08-10T17:00:00`, is accepted on read
/// and never produced on write. The `at:` prefix is load-bearing: a *bare*
/// instant is what older builds saved and stays refused, so this cannot
/// resurrect a stale one. That combination is deliberate. Only a hand-authored
/// config can contain one, which is exactly the case the relative encoding was
/// never protecting: a scene that names the 10 August 2020 derecho means that
/// run and no other, and resolving it to "six hours before now" would silently
/// show the wrong weather. A file this build wrote cannot drift into the past,
/// because it never writes this form.
///
/// `None` for a token this build does not spell, and for an offset past
/// [`MAX_RUN_OFFSET`].
fn decode_run_choice(text: &str, now: chrono::NaiveDateTime) -> Option<chrono::NaiveDateTime> {
    if let Some(rest) = text.strip_prefix(RUN_STEM) {
        let back: u8 = match rest {
            "" => 0,
            rest => rest.strip_prefix('-')?.parse().ok()?,
        };
        return (back <= MAX_RUN_OFFSET)
            .then(|| latest_run_at(now) - chrono::Duration::hours(i64::from(back)));
    }
    // Absolute, and snapped to the hour a run actually exists on: an instant
    // between runs names no grid, and rounding it here rather than at the fetch
    // keeps "which run did I ask for" answerable from the config alone.
    let text = text.strip_prefix(ABSOLUTE_RUN_PREFIX)?;
    let at = chrono::NaiveDateTime::parse_from_str(text, ABSOLUTE_RUN_FORMAT).ok()?;
    at.with_minute(0)?.with_second(0)?.with_nanosecond(0)
}

/// **What a resident grid is, in one phrase**: its valid time and forecast
/// hour, or its run time alone for an analysis.
///
/// One function because two surfaces say it — the layer's toggle label in the
/// options panel and the stack row over the map — and a user comparing the two
/// must not be reading two renderings of one grid. Built from the **grid**,
/// never from the selection: a pane whose pick has not landed is not drawing
/// it.
fn frame_label(grid: &HrrrGridData) -> String {
    // f01+ must show its *valid* time and F-hour: a 0-1 h maximum labelled
    // with the run time alone reads as an analysis valid now.
    if grid.forecast_hour > 0 {
        format!(
            "{} F{:02}",
            grid.valid_time().format("%H:%Mz"),
            grid.forecast_hour,
        )
    } else {
        grid.ref_time.format("%H:%Mz").to_string()
    }
}

/// **The `(run, forecast hour)` a fetch will ask for**, chosen here at the
/// dispatch and not inside the fetch.
///
/// A pane parked on a frame asks for **that** frame — which is what makes a
/// reopen 1:1 rather than snapping the pane back to the live hour. A pane with
/// no frame selected asks for the latest run **the instant it depicts** could
/// name, at the parameter's own floor.
///
/// `as_of` and not `latest_available_run()`. The wall-clock spelling put
/// `hrrr.<today>/...t13z` on panes parked in April: `as_of` is the wall clock
/// on a live pane, so that arm is unchanged there, and is the scrub instant on
/// a parked one, which is the whole of the difference.
///
/// **The second arm stays reachable, and is the primary path for a scrubbed
/// pane whose transport is off.** `selected_frame` is written only by the
/// `run`/`f_hour` controls and by the transport applying a listed frame, so a
/// pane scrubbed into the past with no loop running never acquires one and
/// draws from this arm on every poll. Closing the listing dispatch
/// ([`ModelDataHandler::run_at`]) made the arm *survivable* — a pane with a
/// loop lists frames and selects one — but it did not make it rare, which is
/// why the run here is resolved from `as_of` rather than merely guarded.
fn fetch_frame(
    view: &ModelPaneState,
    as_of: chrono::NaiveDateTime,
) -> ((chrono::NaiveDate, u8), u8) {
    match view.selected_frame {
        Some((run, f_hour)) => ((run.date(), run.hour() as u8), f_hour),
        None => (
            crate::hrrr::fetch::run_for(as_of),
            view.selected_param.min_forecast_hour(),
        ),
    }
}

/// **How far past its run one `Analysis`-axis frame is valid.**
///
/// Zero for fourteen of the sixteen parameters, and one hour for the two
/// windowed UH maxima: the run's `f00` record for those is identically zero
/// over a zero-length window, which is why
/// [`ModelParameter::min_forecast_hour`] is a floor and not a preference. The
/// bucket listing itself is still of `f00` *keys* — that is what proves a run
/// exists — but the frame this axis offers is the earliest one the field
/// actually publishes.
fn analysis_offset(param: ModelParameter) -> chrono::Duration {
    chrono::Duration::hours(i64::from(param.min_forecast_hour()))
}

/// **What a frame listing was dispatched for**, captured at the dispatch and
/// handed straight back to `apply_frame_listing`.
///
/// The arriving `PaneRef` is a `PaneRef::across` union whose config is null by
/// construction, so reading the pane back for the run or the axis files the
/// answer under whatever the pane happens to hold *now* — which after a run
/// roll or an axis flip is a different scope, silently. `RadarListing` is the
/// same shape for the same reason.
struct ModelListing {
    param: ModelParameter,
    run: chrono::NaiveDateTime,
    axis: ModelAxis,
    range: (chrono::NaiveDateTime, chrono::NaiveDateTime),
    /// The run times the listing named. Empty for a `Forecast` listing, whose
    /// frames are a closed form of `run` that `list_frames` already computes.
    runs: Vec<chrono::NaiveDateTime>,
}

/// The three fields a listing is filed under. `ModelListing` carries these
/// plus its payload; this is the key half alone, so a lookup does not have to
/// own one.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
struct ModelScope {
    param: ModelParameter,
    run: chrono::NaiveDateTime,
    axis: ModelAxis,
}

impl ModelListing {
    fn scope(&self) -> ModelScope {
        ModelScope {
            param: self.param,
            run: self.run,
            axis: self.axis,
        }
    }
}

/// One loop frame's grid on its way back from [`FrameSource::fetch_frame`].
///
/// A type of its own and **not** `HrrrFetchResult`: the two arrive through two
/// different doors — `apply_fetch_result` for the live round, `apply_frame`
/// for a frame — and a shared payload type is how a frame ends up calling
/// `set_data` and moving the live picture.
struct ModelFrameFetch {
    key: GridKey,
    grid: Option<HrrrGridData>,
}

/// **The whole per-pane state of the model layer**: whether this pane draws
/// it, which parameter it is showing, which way its time axis runs and which
/// frame it is parked on. All of it was fields of the handler once, which is
/// why two panes could never sit on two HRRR parameters — the config swap
/// re-installed one pane's before every read and called that independence.
///
/// The grid cache is **not** here: a decoded HRRR grid is megabytes and is the
/// same grid whichever pane asked for it, so it stays one shared cache and the
/// selections merely pin it.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ModelPaneState {
    pub enabled: bool,
    pub selected_param: ModelParameter,
    pub axis: ModelAxis,
    /// **The (run, forecast hour) this pane is showing**, or `None` for a pane
    /// that has not been parked on a frame. Written by the `run` and `f_hour`
    /// controls, and restored from the saved config; a clock will write it too
    /// once the transport lands.
    ///
    /// `None` is not "no picture": it resolves to the most recently used
    /// resident grid of `selected_param`, which is exactly what the
    /// parameter-keyed cache used to answer.
    pub selected_frame: Option<(chrono::NaiveDateTime, u8)>,
}

impl ModelPaneState {
    /// A pane that has saved nothing, with `enabled` supplied by the pane's
    /// own slot flag.
    fn new(enabled: bool) -> Self {
        Self {
            enabled,
            selected_param: ModelParameter::SurfaceBasedCin,
            axis: ModelAxis::default(),
            selected_frame: None,
        }
    }
}

pub(crate) struct ModelDataHandler {
    pub state: OverlayState<Option<Arc<HrrrGridData>>, Whole>,
    /// **The registry's own copy**, used only where no pane is supplied. The
    /// config swap keeps it in step until WO-M10c deletes the swap; every
    /// answer prefers [`PaneRef::state`] when there is one.
    pub defaults: ModelPaneState,
    cached_grids: ModelGridCache,
    /// **The loop's staging area**: one frame's grid at a time, whatever the
    /// loop's length. Separate from `cached_grids` so a loop cannot spend the
    /// pane set's budget — see [`MODEL_FRAME_STAGING_BYTES`].
    frame_grids: ModelFrameCache,
    /// Serialises the frame decodes that fill `frame_grids`; see
    /// [`FrameGate`].
    frame_gate: FrameGate,
    /// The run times an **analysis** listing named, filed under the scope it
    /// was dispatched for. A `Forecast` scope never appears here: its frames
    /// are a closed form of the run.
    frame_listings: HashMap<ModelScope, Vec<chrono::NaiveDateTime>>,
    /// The windows a listing really covered, per scope — what makes
    /// `list_frames` able to say `complete` about an analysis window without
    /// mistaking "I found none" for "none exist".
    covered: HashMap<ModelScope, Vec<(chrono::NaiveDateTime, chrono::NaiveDateTime)>>,
    pub last_error: Option<String>,
}

impl ModelDataHandler {
    pub fn new() -> Self {
        Self {
            // Not parked: no `take_retired` drain and no staging pool waiting
            // for the replaced grid, so a park slot would hold one whole extra
            // grid between polls and free it inline on the next arrival — the
            // reason `OverlayState::parks` is opt-in.
            state: OverlayState::new(),
            defaults: ModelPaneState::new(false),
            cached_grids: ModelGridCache::new(),
            frame_grids: ModelFrameCache::new(),
            frame_gate: FrameGate::default(),
            frame_listings: HashMap::new(),
            covered: HashMap::new(),
            last_error: None,
        }
    }

    /// **This pane's answer, or the registry's own copy** when no pane was
    /// supplied.
    fn view<'a>(&'a self, pane: &PaneRef<'a>) -> &'a ModelPaneState {
        pane.state_as::<ModelPaneState>().unwrap_or(&self.defaults)
    }

    /// Edit this pane's state, falling back to the registry's copy for a
    /// caller that supplied no pane.
    fn edit(&mut self, pane: &mut PaneMut<'_>, f: impl FnOnce(&mut ModelPaneState)) {
        match pane.state_as::<ModelPaneState>() {
            Some(state) => f(state),
            None => f(&mut self.defaults),
        }
    }

    /// **The grid key this pane's picture is**, or `None` when nothing of its
    /// parameter is resident and it has not been parked on a frame.
    ///
    /// A parked pane names its own `(run, hour)` whether or not that grid is
    /// resident — a key for a grid that has not landed is what makes
    /// `has_data` false and the fetch happen. An unparked pane falls back to
    /// the most recently used resident grid of its parameter, which is exactly
    /// what the parameter-keyed cache used to answer.
    fn key_of(&self, view: &ModelPaneState) -> Option<GridKey> {
        match view.selected_frame {
            Some((run, f_hour)) => Some(GridKey {
                param: view.selected_param,
                run,
                f_hour,
            }),
            None => self.cached_grids.latest_of(view.selected_param),
        }
    }

    /// This pane's resident grid, marking it most-recently-used.
    fn grid_of(&self, pane: &PaneRef<'_>) -> Option<&Arc<HrrrGridData>> {
        self.cached_grids.get(self.key_of(self.view(pane))?)
    }

    /// **Every grid some pane is showing**, deduplicated — what the shared
    /// cache must not evict. The union, per [`PaneRef::all_as`]; the registry's
    /// own copy stands in when no pane answered at all.
    ///
    /// **Showing, which is what `enabled` means.** A pane keeps its slot, its
    /// state, its parameter and its parked frame when the user switches the
    /// layer off, so this union answered for panes drawing nothing — and a
    /// pinned key is never an eviction victim, so a switched-off pane held a
    /// grid against a budget that is already the largest host-byte family
    /// this layer has. The flag is the same field [`Self::is_enabled`] answers
    /// from, so the pin set cannot disagree with the layer's own answer.
    ///
    /// **This makes the empty answer reachable**, and that is the fix rather
    /// than a hazard: `insert` never evicts the arrival itself, so a cache
    /// with nothing pinned still installs what lands and merely stops carrying
    /// what no pane is looking at. The `const _` floor beside
    /// [`MODEL_GRID_BUDGET_BYTES`] — one grid per pane — is unaffected in the
    /// safe direction: fewer panes pin, so the budget it holds is now an
    /// over-statement of what the pin set can force resident, never an
    /// under-statement.
    fn pinned_keys(&self, pane: &PaneRef<'_>) -> Vec<GridKey> {
        let mut pinned: Vec<GridKey> = Vec::new();
        let mut any_pane = false;
        for state in pane.all_as::<ModelPaneState>() {
            // A pane still ANSWERS when its layer is switched off — it keeps
            // its slot and its state — so `any_pane` is set outside the flag.
            // What it does not do is show a grid, and only a shown grid may
            // be pinned past the byte budget.
            any_pane = true;
            if state.enabled
                && let Some(key) = self.key_of(state)
                && !pinned.contains(&key)
            {
                pinned.push(key);
            }
        }
        if !any_pane
            && let Some(key) = self.key_of(&self.defaults)
            && !pinned.contains(&key)
        {
            pinned.push(key);
        }
        pinned
    }

    /// **The run this pane's frames belong to**, or `None` before anything has
    /// told it: the run it is parked on, else the run of its most recent
    /// resident grid.
    ///
    /// Deliberately does **not** fall back to `latest_available_run()`. That
    /// reads the wall clock, and a pane parked in the past would have its whole
    /// frame list walked forward under it. Every caller that holds the pane's
    /// own depicted instant asks [`Self::run_at`] instead, which answers the
    /// same two arms and then falls back to *that* instant; `None` is left for
    /// the callers that hold no instant at all.
    fn run_of(&self, view: &ModelPaneState) -> Option<chrono::NaiveDateTime> {
        view.selected_frame.map(|(run, _)| run).or_else(|| {
            self.cached_grids
                .latest_of(view.selected_param)
                .map(|k| k.run)
        })
    }

    /// **The run this pane's frames belong to, as of the instant it depicts.**
    ///
    /// [`Self::run_of`] with the "I do not know yet" hole closed by the pane's
    /// own clock rather than by the wall clock — the distinction the whole
    /// scrub path turns on. Under a parked pane `as_of` is *fixed*, so nothing
    /// here walks forward between frames; under a live pane it is the wall
    /// clock, and this is `latest_available_run()` by another route.
    ///
    /// Closing the hole is what makes a parked pane work at all. `run_of`
    /// answers `None` until the pane has a selection or the cache has a grid,
    /// and a freshly scrubbed pane has neither — so `scope_of` was `None`, so
    /// `create_frame_list_task` dispatched nothing, so the pane had no frame
    /// series, so nothing ever wrote `selected_frame`, so the fetch fell back
    /// to the wall clock and put today's run on a pane parked in April. The
    /// cycle has no entry point except this one.
    ///
    /// The cache arm is bounded by the same instant for the same reason: see
    /// [`ModelGridCache::latest_not_after`].
    fn run_at(&self, view: &ModelPaneState, as_of: chrono::NaiveDateTime) -> chrono::NaiveDateTime {
        let ceiling = latest_run_at(as_of);
        view.selected_frame
            .map(|(run, _)| run)
            .or_else(|| {
                self.cached_grids
                    .latest_not_after(view.selected_param, ceiling)
                    .map(|k| k.run)
            })
            .unwrap_or(ceiling)
    }

    fn scope_of(&self, view: &ModelPaneState) -> Option<ModelScope> {
        Some(ModelScope {
            param: view.selected_param,
            run: self.run_of(view)?,
            axis: view.axis,
        })
    }

    /// [`Self::scope_of`] against the instant the pane depicts, and therefore
    /// **total** — see [`Self::run_at`].
    ///
    /// Every caller that holds an instant uses this, and they must all use it:
    /// `create_frame_list_task` files a listing under the scope it computes
    /// here, and `list_frames` reads it back by that same key.
    fn scope_at(&self, view: &ModelPaneState, as_of: chrono::NaiveDateTime) -> ModelScope {
        ModelScope {
            param: view.selected_param,
            run: self.run_at(view, as_of),
            axis: view.axis,
        }
    }

    /// **The forecast hours of one run, as a closed form.** No listing, no
    /// network: the floor is the parameter's own, the horizon is the run
    /// cycle's, and every hour between them is published with an `.idx`.
    fn forecast_stamps(param: ModelParameter, run: chrono::NaiveDateTime) -> Vec<FrameStamp> {
        (param.min_forecast_hour()..=forecast_horizon(run))
            .map(|f_hour| FrameStamp {
                valid: run + chrono::Duration::hours(i64::from(f_hour)),
                run: Some(run),
            })
            .collect()
    }

    /// **Every stamp a scope names, unclipped by any window** — the closed
    /// form on the `Forecast` axis, the filed run times on `Analysis`.
    ///
    /// One expression, because two of them would be two answers to "what can
    /// this pane draw": `list_frames` narrows this to a range and
    /// `latest_at` picks one out of it, and neither may disagree with the
    /// other about what the set is.
    fn stamps_of(&self, scope: &ModelScope) -> Vec<FrameStamp> {
        match scope.axis {
            ModelAxis::Forecast => Self::forecast_stamps(scope.param, scope.run),
            ModelAxis::Analysis => self
                .frame_listings
                .get(scope)
                .map(|runs| {
                    runs.iter()
                        .map(|run| FrameStamp {
                            valid: *run + analysis_offset(scope.param),
                            run: Some(*run),
                        })
                        .collect()
                })
                .unwrap_or_default(),
        }
    }

    /// Whether a listing has covered the whole of `range` for `scope`.
    fn covers(
        &self,
        scope: &ModelScope,
        range: (chrono::NaiveDateTime, chrono::NaiveDateTime),
    ) -> bool {
        self.covered
            .get(scope)
            .is_some_and(|windows| windows.iter().any(|w| w.0 <= range.0 && range.1 <= w.1))
    }

    /// **The `(run, forecast hour)` a stamp names for this pane**, or `None`
    /// when no listing named it.
    ///
    /// `f_hour = (valid - run).num_hours()` — the inverse of
    /// [`HrrrGridData::valid_time`], which is the only arithmetic either
    /// direction of this axis uses.
    fn frame_target(
        &self,
        view: &ModelPaneState,
        as_of: chrono::NaiveDateTime,
        stamp: &FrameStamp,
    ) -> Option<GridKey> {
        let run = stamp.run?;
        let hours = (stamp.valid - run).num_hours();
        let f_hour = u8::try_from(hours).ok()?;
        if f_hour < view.selected_param.min_forecast_hour() {
            return None;
        }
        let scope = self.scope_at(view, as_of);
        let named = match view.axis {
            // The set is the closed form, so "did a listing name it" is
            // answered by arithmetic rather than by a map.
            ModelAxis::Forecast => run == scope.run && f_hour <= forecast_horizon(run),
            // The analysis axis is f00 of a listed run, and nothing else.
            ModelAxis::Analysis => {
                f_hour == view.selected_param.min_forecast_hour()
                    && self
                        .frame_listings
                        .get(&scope)
                        .is_some_and(|runs| runs.contains(&run))
            }
        };
        named.then_some(GridKey {
            param: view.selected_param,
            run,
            f_hour,
        })
    }

    /// **What a change of frame costs.** Nothing when the grid is already
    /// resident — [`Self::content_signature`] carries the frame, so the raster
    /// re-dispatches on its own — and a fetch when it is not. The shape the
    /// parameter and axis arms already use.
    fn frame_changed(&mut self, pane: &mut PaneMut<'_>) -> ControlEffect {
        if self.has_data(&pane.as_ref()) {
            self.state.data_generation = self.state.data_generation.wrapping_add(1);
            return ControlEffect::None;
        }
        ControlEffect::Fetch
    }
}

impl FrameSource for ModelDataHandler {
    /// **This pane's run decides how far forward the rail reaches** — f48 on a
    /// 00/06/12/18Z cycle, f18 on every other hour. See [`forecast_horizon`].
    ///
    /// Zero on the `Analysis` axis, which lists the f00 of past runs and so
    /// never reaches past the wall clock, even though [`Self::time_axis`]
    /// declares `extends_future` for the layer as a whole.
    ///
    /// The fallback matters: [`Self::run_of`] answers `None` until this pane
    /// has a selection or the cache has a grid, which is exactly the state a
    /// pane is in when its loop is first switched on. Answering zero there
    /// would give the loop a backward-only range and no forecast frame would
    /// survive the clip in [`Self::create_frame_list_task`].
    fn frame_horizon(&self, pane: &PaneRef<'_>) -> chrono::Duration {
        let view = self.view(pane);
        if view.axis != ModelAxis::Forecast {
            return chrono::Duration::zero();
        }
        let now = chrono::Utc::now().naive_utc();
        let run = self.run_of(view).unwrap_or_else(|| latest_run_at(now));
        // Measured from the wall clock rather than from the run, so the range
        // still covers f48 of a run whose f00 is already hours old.
        chrono::Duration::hours(i64::from(forecast_horizon(run)))
    }

    /// **The frames this pane is holding**: every grid of its own parameter,
    /// on its own axis, in **either** store — the one staged frame and the
    /// pane set's live pictures. `valid` is the grid's valid time and `run`
    /// its reference time.
    ///
    /// This pane's scope and not the stores' whole contents: the other grids
    /// belong to other panes' parameters and runs, and pooling them here would
    /// offer this pane frames it cannot draw.
    ///
    /// **Both stores, and that is the one place this layer does better than
    /// the mosaics it copies.** MRMS keys its live cache by product and its
    /// frame cache by `(product, stamp)`, so it cannot ask its live cache
    /// whether it is already holding a loop frame's granule, and re-fetches
    /// one it has. A model grid has one identity either side of the seam, so
    /// the pane's own parked hour — which is also a frame of its loop — is
    /// named here, `prepare_job` draws it from wherever it sits, and no round
    /// trip is spent on a grid the process is already holding.
    ///
    /// Neither walk goes through a touching accessor: which frames the
    /// dispatcher asked about is not a fact about what the user is looking
    /// at, and reordering either eviction queue behind this would age out the
    /// grid on the glass.
    fn frames_resident(&self, pane: &PaneRef<'_>) -> Vec<FrameStamp> {
        let view = self.view(pane);
        let Some(scope) = self.scope_of(view) else {
            return Vec::new();
        };
        let in_scope = |key: &GridKey| {
            key.param == scope.param
                && match scope.axis {
                    ModelAxis::Forecast => key.run == scope.run,
                    ModelAxis::Analysis => key.f_hour == scope.param.min_forecast_hour(),
                }
        };
        let mut frames: Vec<FrameStamp> = self
            .frame_grids
            .entries
            .keys()
            .chain(self.cached_grids.entries.keys())
            .filter(|key| in_scope(key))
            .map(|key| FrameStamp {
                // The same arithmetic as `HrrrGridData::valid_time`, which is
                // the inverse of `frame_target`'s. Read off the key rather
                // than the grid so one stamp cannot be minted two ways when a
                // key is in both stores.
                valid: key.run + chrono::Duration::hours(i64::from(key.f_hour)),
                run: Some(key.run),
            })
            .collect();
        frames.sort_by_key(|stamp| stamp.valid);
        frames.dedup();
        frames
    }

    /// **The newest grid this pane's scope reaches at or before `t`**, over
    /// every stamp the scope names and with no window to clip it at.
    ///
    /// `run` is carried, and here it is load-bearing rather than decorative:
    /// on the `Analysis` axis two runs' f00 grids are two different pictures,
    /// and on `Forecast` the whole set belongs to one run whose identity is
    /// what tells a forecast frame from the analysis of the hour it depicts.
    ///
    /// `t` is the instant the pane depicts, so the scope is resolved against
    /// *it* rather than against the wall clock — see [`Self::run_at`].
    fn latest_at(
        &self,
        pane: &PaneRef<'_>,
        t: chrono::NaiveDateTime,
    ) -> Option<squallar_source::time::FrameStamp> {
        let scope = self.scope_at(self.view(pane), t);
        let mut frames = self.stamps_of(&scope);
        frames.sort_by_key(|stamp| stamp.valid);
        squallar_source::time::newest_at_or_before(&frames, t)
    }

    /// **What frames exist over `range`, per axis.**
    ///
    /// `Forecast` is a closed form of the run — `min_forecast_hour` to the
    /// cycle's horizon, every hour published with an `.idx` — so it is
    /// `complete` on arithmetic alone and never needs a round trip to say so.
    /// `Analysis` is a bucket listing, and is `complete` only where one has
    /// landed covering the whole window: "I found none" is not "none exist".
    ///
    /// The scope is resolved against [`FetchConfig::as_of`] — the instant this
    /// pane depicts — so a pane parked in the past lists that window's runs and
    /// not the wall clock's. See [`Self::run_at`].
    fn list_frames(
        &self,
        ctx: &FetchConfig,
        pane: &PaneRef<'_>,
        range: (chrono::NaiveDateTime, chrono::NaiveDateTime),
    ) -> FrameListing {
        let view = self.view(pane);
        let scope = self.scope_at(view, ctx.as_of);
        let (mut frames, complete) = match scope.axis {
            ModelAxis::Forecast => (self.stamps_of(&scope), true),
            ModelAxis::Analysis => (self.stamps_of(&scope), self.covers(&scope, range)),
        };
        frames.retain(|stamp| range.0 <= stamp.valid && stamp.valid <= range.1);
        frames.sort_by_key(|stamp| stamp.valid);
        FrameListing {
            range,
            frames,
            complete,
        }
    }

    /// **The listing that fills [`Self::list_frames`]**, scoped by the
    /// `(parameter, run, axis)` captured HERE, at dispatch.
    ///
    /// The `Forecast` arm performs **no network round trip at all**: the set
    /// is a closed form of the run, so the future is ready the moment it is
    /// built. It is still a task rather than a `None` because the driver above
    /// builds a loop out of the *arrival*, and a layer that answers `None`
    /// never gets one.
    ///
    /// The `Analysis` arm lists `hrrr.YYYYMMDD/conus/` for every UTC day the
    /// window touches and keeps the `f00` keys — the analysis grid of each run.
    fn create_frame_list_task(
        &self,
        ctx: &FetchConfig,
        pane: &PaneRef<'_>,
        range: (chrono::NaiveDateTime, chrono::NaiveDateTime),
    ) -> Option<FetchTask> {
        let view = self.view(pane);
        // Captured at dispatch and carried in the scope. The `PaneRef` that
        // arrives with the answer is a `PaneRef::across` union whose config is
        // null by construction, so reading any of these three back on arrival
        // files the listing under whatever the pane holds by then.
        // Total, and that is the fix: `scope_of` answers `None` for a pane
        // that has neither a selection nor a resident grid, and returning
        // `None` here left such a pane with no frame series at all — an empty
        // scrubber, and a `selected_frame` nothing would ever write.
        let scope = self.scope_at(view, ctx.as_of);
        let ModelScope { param, run, axis } = scope;

        if axis == ModelAxis::Forecast {
            let frames = Self::forecast_stamps(param, run);
            return Some(FrameListingResult::task(known::MODEL_DATA, async move {
                FrameListingResult {
                    listing: FrameListing {
                        range,
                        frames: frames
                            .into_iter()
                            .filter(|s| range.0 <= s.valid && s.valid <= range.1)
                            .collect(),
                        complete: true,
                    },
                    scope: Box::new(ModelListing {
                        param,
                        run,
                        axis,
                        range,
                        runs: Vec::new(),
                    }),
                }
            }));
        }

        let client = ctx.client.clone();
        let sources = ctx.sources.clone();
        Some(FrameListingResult::task(known::MODEL_DATA, async move {
            let (runs, complete) =
                match crate::hrrr::fetch::list_analysis_runs(&client, &sources, range).await {
                    Ok(runs) => {
                        log::info!("Model: found {} HRRR analysis runs in range", runs.len());
                        (runs, true)
                    }
                    Err(e) => {
                        // An empty list is how a failed listing reaches the
                        // pane, and `complete: false` is how it stays honest
                        // about why.
                        log::error!("Model: HRRR analysis listing failed: {e:?}");
                        (Vec::new(), false)
                    }
                };
            let frames = runs
                .iter()
                .map(|run| FrameStamp {
                    valid: *run + analysis_offset(param),
                    run: Some(*run),
                })
                .collect();
            FrameListingResult {
                listing: FrameListing {
                    range,
                    frames,
                    complete,
                },
                scope: Box::new(ModelListing {
                    param,
                    run,
                    axis,
                    range,
                    runs,
                }),
            }
        }))
    }

    /// **The one door to a frame's grid.**
    ///
    /// `None` when no listing named that stamp for this pane — which is also
    /// the answer for a pane whose loop is being rebuilt on another run or
    /// another axis while its old queue drains.
    fn fetch_frame(
        &self,
        ctx: &FetchConfig,
        pane: &PaneRef<'_>,
        stamp: &FrameStamp,
    ) -> Option<FetchTask> {
        let view = self.view(pane);
        let key = self.frame_target(view, ctx.as_of, stamp)?;
        // **Already held, either side of the seam.** The staged grid is the
        // ordinary case; the live cache answers for a frame the pane is
        // itself parked on, which `frames_resident` also names, so the two
        // agree about which frames are owed a round trip.
        if self.frame_grids.is_staged(key) || self.cached_grids.is_resident(key) {
            return None;
        }
        let client = ctx.client.clone();
        let sources = ctx.sources.clone();
        let gate = Arc::clone(&self.frame_gate);
        let GridKey { param, run, f_hour } = key;
        let run_pair = (run.date(), run.hour() as u8);
        Some(FetchTask {
            kind: known::MODEL_DATA,
            future: Box::pin(async move {
                // Held across the fetch as well as the decode, so the peak is
                // one grid rather than one per concurrent request. The bytes
                // are the bottleneck either way; see [`FrameGate`].
                let _one_at_a_time = gate.lock().await;
                let result = if param.is_composite() {
                    crate::hrrr::fetch::fetch_composite_hrrr_data(
                        &client, &sources, &param, run_pair, f_hour,
                    )
                    .await
                } else {
                    crate::hrrr::fetch::fetch_hrrr_data(&client, &sources, &param, run_pair, f_hour)
                        .await
                };
                let grid = match result.0 {
                    Ok(grid) => Some(grid),
                    Err(e) => {
                        log::error!(
                            "Model frame fetch failed for {param:?} {run} f{f_hour:02}: {e:?}"
                        );
                        None
                    }
                };
                Box::new(ModelFrameFetch { key, grid }) as FetchPayload
            }),
        })
    }

    /// File a listing under the scope it was **dispatched for**, from the
    /// scope payload and never from the pane: the `PaneRef` an arrival carries
    /// is the union across panes and its config is null by construction.
    ///
    /// A `Forecast` listing teaches this handler nothing — its frames are the
    /// closed form [`Self::list_frames`] already computes — so only the run
    /// times of an `Analysis` listing are kept, and coverage only where the
    /// listing really covered the window.
    fn apply_frame_listing(
        &mut self,
        listing: FrameListing,
        scope: FetchPayload,
        _pane: &PaneRef<'_>,
    ) {
        let Ok(scope) = scope.downcast::<ModelListing>() else {
            log::error!("a frame listing reached the model layer under another layer's scope");
            return;
        };
        let key = scope.scope();
        // Nothing is filed for a forecast listing, not even its coverage:
        // `list_frames` answers that axis from arithmetic and never reads
        // either map, so a row here would be state nothing consults and a
        // window pushed per loop rebuild for the length of a session.
        if scope.axis == ModelAxis::Forecast {
            return;
        }
        let known = self.frame_listings.entry(key).or_default();
        for run in &scope.runs {
            if !known.contains(run) {
                known.push(*run);
            }
        }
        known.sort_unstable();
        // Coverage is recorded only for a listing that really covered the
        // window. A failure arrives empty so the pane can retire its loop, and
        // must not leave `list_frames` claiming the window is settled.
        if listing.complete && !self.covers(&key, scope.range) {
            self.covered.entry(key).or_default().push(scope.range);
        }
    }

    /// **Stage** one frame's grid under the key its fetch was dispatched for.
    ///
    /// Into [`ModelFrameCache`] and not the live cache, which is the whole of
    /// this item: a loop frame is passing through on its way to a texture, and
    /// the pane set's budget is not what pays for it.
    ///
    /// The key comes back on the payload rather than being recomputed from
    /// `stamp` and the pane, for the same reason the listing's scope does.
    fn apply_frame(&mut self, _stamp: FrameStamp, data: FetchPayload, _pane: &PaneRef<'_>) {
        let Ok(frame) = data.downcast::<ModelFrameFetch>() else {
            log::error!("a frame reached the model layer under another layer's payload");
            return;
        };
        let Some(grid) = frame.grid else {
            return;
        };
        self.frame_grids.insert(frame.key, Arc::new(grid));
    }

    /// Drop every staged grid this pane's `keep` does not name.
    ///
    /// **Only the staging area.** The live cache holds what the panes are
    /// showing and is evicted by its own byte budget; a second authority over
    /// it would fight that, which is what the no-op this replaces was written
    /// to avoid. Over the staging area there is no such conflict — nothing
    /// there is pinned, and its budget is one grid.
    ///
    /// Matched on the stamp as this layer names it: `valid` **and** `run`,
    /// because on the analysis axis two runs produce the same forecast hour
    /// and on the forecast axis the run is what tells a frame from the
    /// analysis of the hour it depicts. A stamp with no run keeps nothing,
    /// which is correct — this layer names no such stamp.
    ///
    /// **Nothing above calls this yet** (the production frame-eviction
    /// authority is still each layer's own budget), so it is exercised by this
    /// layer's own suite rather than by the loop — the same position MRMS's is
    /// in.
    fn retain_frames(&mut self, pane: &PaneRef<'_>, keep: &[FrameStamp]) {
        let Some(scope) = self.scope_of(self.view(pane)) else {
            return;
        };
        self.frame_grids.retain(|key| {
            key.param != scope.param
                || keep.iter().any(|stamp| {
                    stamp.run == Some(key.run)
                        && stamp.valid == key.run + chrono::Duration::hours(i64::from(key.f_hour))
                })
        });
    }
}

impl OverlayHandler for ModelDataHandler {
    /// The sixteen HRRR parameters this layer offers, projected into the
    /// substrate's read contract by [`crate::hrrr::fields`].
    fn products(&self) -> &'static [squallar_source::product::ProductSpec] {
        crate::hrrr::fields::products()
    }

    /// The parameter dropdown: its option values are the parameters'
    /// `as_str()` spellings, which are exactly the `FieldId`s
    /// [`crate::hrrr::fields`] registers, so a catalogue tile's id can be sent
    /// straight through `apply_control`.
    fn field_control_id(&self) -> Option<&'static str> {
        Some("parameter")
    }

    /// **The parameter this pane's dropdown is on**, taken from this layer's
    /// own per-pane state and projected through its registry row — never
    /// spelled as a fresh string, so the id can only ever be one
    /// [`crate::hrrr::fields`] registers.
    ///
    /// This layer is not [`SourceHandler::volume`]-capable, so the 3D walk
    /// stops before it asks; the answer is here because "which field is this
    /// pane showing" is a question about a layer with fields, not a question
    /// about 3D.
    fn current_field(&self, pane: &PaneRef<'_>) -> Option<squallar_source::product::FieldId> {
        Some(
            crate::hrrr::fields::spec(self.view(pane).selected_param)
                .id
                .clone(),
        )
    }
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

    /// Yesterday's look: the alpha every HRRR ramp painted into its texels
    /// before opacity became a layer property. See
    /// `render::gridded::DEFAULT_PLAN_ALPHA`.
    fn default_opacity(&self, _pane: &PaneRef<'_>) -> f32 {
        crate::render::gridded::DEFAULT_OPACITY
    }

    /// HRRR is a run-based forecast: hourly cycles, each carrying grids valid
    /// at the run time plus a forecast hour — discrete stamped frames, and the
    /// stamps run **ahead** of the wall clock.
    fn time_axis(&self) -> TimeAxis {
        TimeAxis::FrameSeries {
            typical_step: std::time::Duration::from_secs(3600),
            extends_future: true,
        }
    }

    /// **Thirteen hourly grids — half a day, and a loop rather than a
    /// before-and-after.**
    ///
    /// The Lookback slider's own default is 3600 s, which at
    /// [`Self::time_axis`]'s hourly step is *two* frames: the shortest thing
    /// that is a sequence at all. Radar reads the same 3600 s as a dozen
    /// volumes, which is why the number cannot be one number.
    ///
    /// It is the backward half this is about. The `Forecast` axis already
    /// reaches 18 to 48 hours forward through [`Self::frame_horizon`]; the
    /// `Analysis` axis reaches nowhere forward at all and is nothing *but*
    /// this window.
    fn min_loop_frames(&self) -> usize {
        13
    }

    /// **The grids these stops draw from, and none of the hours between
    /// them.**
    ///
    /// [`squallar_source::time::frame_residency`], routed through
    /// [`Self::latest_at`] like the other three framed layers.
    ///
    /// **The one layer whose stops can be ahead of the wall clock**, and this
    /// needs no arm for it: a forecast grid's [`FrameStamp::valid`] is the
    /// instant it depicts, so a stop two hours into the future is answered by
    /// the same `valid <= t` rule as a stop two hours behind. `run` is
    /// carried on the stamp and never compared here — which of two cycles a
    /// grid came from does not change *when* it must be held.
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

    fn is_enabled(&self, pane: &PaneRef<'_>) -> bool {
        self.view(pane).enabled
    }

    fn set_enabled(&mut self, enabled: bool, pane: &mut PaneMut<'_>) {
        self.edit(pane, |state| state.enabled = enabled);
    }

    /// **The stack row over the map**: this pane's field, and the frame it is
    /// drawing.
    ///
    /// The frame half is what puts a forecast hour on the pane itself rather
    /// than behind the options panel. It comes off the **resident** grid, the
    /// same source as the toggle label: a pane whose pick has not landed names
    /// its field and stops, instead of promising a picture that is not on the
    /// glass.
    fn status_line(&self, pane: &PaneRef<'_>) -> Option<String> {
        let view = self.view(pane);
        if !view.enabled {
            return None;
        }
        let name = view.selected_param.display_name();
        Some(match self.grid_of(pane) {
            Some(grid) => format!("{name} - {}", frame_label(grid)),
            None => name.to_owned(),
        })
    }

    fn data_generation(&self) -> u64 {
        self.state.data_generation
    }

    /// **The selected parameter is in the token**, not just the fetch counter.
    /// Two panes on two parameters draw two different grids, and the cache
    /// token is what the render dispatch groups panes by — one token for both
    /// is one raster for both, which is this layer's shape of the cross-pane
    /// collision the pane state exists to prevent.
    fn content_signature(&self, pane: &PaneRef<'_>) -> u64 {
        let view = self.view(pane);
        // The **frame** is in the token as well as the parameter: two panes on
        // one parameter at two forecast hours are two different pictures, and
        // one token for both is one raster for both.
        let frame = self.key_of(view).map_or(0, |key| {
            (key.run.and_utc().timestamp() as u64).rotate_left(16) ^ (u64::from(key.f_hour) + 1)
        });
        self.data_generation() ^ (view.selected_param as u64 + 1).rotate_left(32) ^ frame
    }

    fn has_data(&self, pane: &PaneRef<'_>) -> bool {
        self.key_of(self.view(pane))
            .is_some_and(|key| self.cached_grids.contains(key))
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
            .map(|d| d.values.len())
            .unwrap_or(0)
    }

    fn auto_poll_interval(&self) -> Option<u64> {
        Some(3600) // HRRR runs hourly.
    }

    fn clickable_items<'a>(
        &'a self,
        _pane: &PaneRef<'_>,
    ) -> Vec<crate::render::overlay_state::ClickableItem<'a>> {
        Vec::new() // Gridded, not feature-based; hover uses `hover_value_at`.
    }

    fn apply_fetch_result(&mut self, result: FetchPayload, pane: &PaneRef<'_>) {
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
                // Read off the grid, not off the request: the fetch clamps a
                // requested hour up to the parameter's floor, so the hour that
                // arrived is the only one this may be filed under.
                let key = GridKey::of(&grid);
                let arc = Arc::new(grid);
                // The pin is every grid SOME pane is showing, which is what
                // every read path below keys on; an arrival must never blank
                // one. The arrival carries no pane of its own — the union
                // across panes is the whole answer here.
                let pinned = self.pinned_keys(pane);
                self.cached_grids.insert(key, arc.clone(), &pinned);
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
        _pane: &PaneRef<'_>,
    ) {
    }

    fn hover_value_at(&self, lat: f64, lon: f64, pane: &PaneRef<'_>) -> Option<String> {
        let grid = self.grid_of(pane)?;
        // The pointer arrives in the pane's continuous frame — 190 past the
        // seam, where this grid is written at -170 — and is carried into the
        // grid's frame before the cull, the lookup and the reach read it. See
        // `render::geo::lon_into_bounds`.
        let lon = crate::render::geo::lon_into_bounds(lon, &grid.bounds);
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

    fn carries_legend(&self) -> bool {
        true
    }

    /// The signature is the selected parameter and nothing else, since the bar is
    /// a pure function of it — deliberately **not** `data_generation`, which every
    /// HRRR fetch bumps. `+ 1` keeps the first parameter's signature off `0`.
    fn legend(&self, pane: &PaneRef<'_>) -> Option<Signed<OverlayLegend>> {
        let view = self.view(pane);
        if !view.enabled {
            return None;
        }
        let thresholds = view.selected_param.legend_thresholds();
        let min = thresholds.first().map_or(0.0, |e| e.0);
        let max = thresholds.last().map_or(1.0, |e| e.0);
        Some(Signed {
            signature: view.selected_param as u64 + 1,
            items: OverlayLegend {
                thresholds,
                is_gradient: true,
                min_value: min,
                max_value: max,
                unit_label: view.selected_param.unit_label(),
            },
        })
    }

    /// The [`Whole`](rasterize::GriddedInput::Whole) carry: an `Arc` clone of
    /// the resident grid, so describing the job costs a refcount and the values
    /// memcpy happens only in the web encoder that knows the texture's bounds.
    ///
    /// **The frame [`RasterizeContext::frame`] names wins over the pane's own
    /// selection**, and that is what lets a loop be filled: a loop wants
    /// several of this layer's frames on screen over one second, each raster
    /// is exactly one of them, and the pane's `selected_frame` can only ever
    /// say one thing at a time. `None` is the live dispatch and reads
    /// [`Self::grid_of`] exactly as it did before the field existed.
    ///
    /// **Selected through [`Self::frame_target`], which is the same resolver
    /// [`Self::fetch_frame`] uses** — so the grid a frame is rasterized from
    /// is the grid that frame's fetch asked for, and the two cannot disagree
    /// about which `(run, forecast hour)` an instant names on this axis.
    ///
    /// A named frame whose grid is **not resident describes no job at all**,
    /// rather than falling back to the pane's picture. The fallback is another
    /// instant's forecast, and filing it into this frame is exactly the defect
    /// the draw fork exists to prevent — one hour's forecast presented,
    /// unlabelled, as another's.
    fn prepare_job(&self, ctx: &RasterizeContext, pane: &PaneRef<'_>) -> Option<DescribedJob> {
        let grid = match ctx.frame {
            Some(stamp) => {
                let key = self.frame_target(self.view(pane), ctx.as_of, &stamp)?;
                // **The staging area first, then the live cache** — one key
                // space, two stores, and a named frame is drawn from whichever
                // holds it. The second arm is not a fallback to another
                // instant's picture: it is the same `GridKey`, so the grid it
                // finds depicts exactly the stamp that was asked for. It is
                // what lets a pane parked on an hour its own loop also names
                // rasterize that frame without a round trip.
                self.frame_grids
                    .get(key)
                    .or_else(|| self.cached_grids.get(key))?
            }
            None => self.grid_of(pane)?,
        }
        .clone();
        Some(DescribedJob::new(rasterize::GriddedInput::Whole(grid)))
    }

    fn job_codec(&self) -> Option<&'static JobCodec> {
        crate::render::jobs::JOB_CODECS
            .iter()
            .find(|row| row.label == "overlay/model")
    }

    fn create_fetch_tasks(&self, ctx: &FetchConfig, pane: &PaneRef<'_>) -> Vec<FetchTask> {
        let client = ctx.client.clone();
        let sources = ctx.sources.clone();
        let view = self.view(pane);
        let param = view.selected_param;
        let (run, f_hour) = fetch_frame(view, ctx.as_of);
        vec![FetchTask {
            kind: known::MODEL_DATA,
            future: Box::pin(async move {
                let result = if param.is_composite() {
                    crate::hrrr::fetch::fetch_composite_hrrr_data(
                        &client, &sources, &param, run, f_hour,
                    )
                    .await
                } else {
                    crate::hrrr::fetch::fetch_hrrr_data(&client, &sources, &param, run, f_hour)
                        .await
                };
                Box::new(result) as FetchPayload
            }),
        }]
    }

    fn controls(&self, pane: &PaneRef<'_>) -> Vec<ControlItem> {
        let view = self.view(pane);
        let grid = self.grid_of(pane);

        let label = match grid {
            Some(g) => format!("Model Data ({})", frame_label(g)),
            None => "Model Data".to_string(),
        };

        let mut items = vec![ControlItem::Toggle {
            id: "enabled",
            label,
            enabled: view.enabled,
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
            selected: view.selected_param.as_str().into(),
        });

        // Which way this pane's time axis runs. Beside the parameter and not
        // behind the transport: it selects which frames exist, which is a
        // question about the layer, not about the playhead.
        items.push(ControlItem::Dropdown {
            id: "axis",
            label: "Time axis".into(),
            options: ModelAxis::all()
                .iter()
                .map(|a| (a.as_str().into(), a.label().into()))
                .collect(),
            selected: view.axis.as_str().into(),
        });

        // **The run and the forecast hour** — the two halves of the frame this
        // pane draws, and the whole of "show me f06" without a transport.
        //
        // The option *values* are relative tokens and the *labels* are
        // absolute times: the token is what persists (a saved instant reopens
        // days stale), and a time is what a user is actually picking.
        let now = chrono::Utc::now().naive_utc();
        let latest = latest_run_at(now);
        let picked_run = view.selected_frame.map(|(run, _)| run);
        // The menu reaches twelve runs back, and further when this pane's own
        // run has aged past that: a pane must always be able to read its own
        // selection out of its own control.
        let deepest = picked_run
            .and_then(|run| u8::try_from((latest - run).num_hours()).ok())
            .filter(|back| *back <= MAX_RUN_OFFSET)
            .map_or(RUN_CHOICES, |back| back.max(RUN_CHOICES));
        let mut run_options = vec![(RUN_LATEST.to_string(), "Latest".to_string())];
        run_options.extend((0..=deepest).map(|back| {
            let run = latest - chrono::Duration::hours(i64::from(back));
            // The date only when it is not the latest run's, so a twelve-hour
            // menu that crosses midnight does not offer two "23:00z".
            let when = if run.date() == latest.date() {
                run.format("%H:%Mz").to_string()
            } else {
                run.format("%m/%d %H:%Mz").to_string()
            };
            (
                run_token(back),
                // Each run states its own reach: 48 hours off a 00/06/12/18Z
                // cycle and 18 off every other one.
                format!("{when} (f00-f{})", forecast_horizon(run)),
            )
        }));
        items.push(ControlItem::Dropdown {
            id: "run",
            label: "Model run".into(),
            options: run_options,
            selected: picked_run
                .and_then(|run| encode_run_choice(run, now))
                .unwrap_or_else(|| RUN_LATEST.to_string()),
        });

        // The horizon is the RUN's, not the layer's, so this list is rebuilt
        // whenever the run above changes.
        let run = picked_run.unwrap_or(latest);
        let floor = view.selected_param.min_forecast_hour();
        let horizon = forecast_horizon(run);
        items.push(ControlItem::Dropdown {
            id: "f_hour",
            label: "Forecast hour".into(),
            options: (floor..=horizon)
                .map(|f_hour| {
                    let valid = run + chrono::Duration::hours(i64::from(f_hour));
                    (
                        f_hour_token(f_hour),
                        format!("F{f_hour:02} ({})", valid.format("%H:%Mz")),
                    )
                })
                .collect(),
            // The floor is not a preference: both MXUPHL maxima publish an
            // identically zero f00 over a zero-length window, so an unparked
            // pane of one of them is already drawing f01 and must say so.
            selected: f_hour_token(
                view.selected_frame
                    .map_or(floor, |(_, f_hour)| f_hour)
                    .clamp(floor, horizon),
            ),
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

        if let Some(grid) = self.grid_of(pane) {
            // Windowed fields are maxima over a period, not instantaneous
            // readings; "UH2-5 at 04:00z" alone reads as a snapshot.
            if grid.forecast_hour > 0 && view.selected_param.is_windowed() {
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
            "parameter" => {
                if let ControlValue::String(ref val) = update.value {
                    let new_param: ModelParameter = val.parse().unwrap();
                    if new_param != self.view(&pane.as_ref()).selected_param {
                        self.edit(pane, |state| state.selected_param = new_param);
                        if self.has_data(&pane.as_ref()) {
                            self.state.data_generation = self.state.data_generation.wrapping_add(1);
                            return ControlEffect::None;
                        }
                        return ControlEffect::Fetch;
                    }
                }
                ControlEffect::None
            }
            "axis" => {
                if let ControlValue::String(ref val) = update.value
                    && let Some(new_axis) = ModelAxis::parse(val)
                    && new_axis != self.view(&pane.as_ref()).axis
                {
                    self.edit(pane, |state| {
                        state.axis = new_axis;
                        // The analysis axis carries one hour per run, so a pane
                        // parked at f18 is parked on a frame this axis does not
                        // contain; the run it is on is kept.
                        if new_axis == ModelAxis::Analysis
                            && let Some((run, _)) = state.selected_frame
                        {
                            state.selected_frame =
                                Some((run, state.selected_param.min_forecast_hour()));
                        }
                    });
                    if self.has_data(&pane.as_ref()) {
                        self.state.data_generation = self.state.data_generation.wrapping_add(1);
                        return ControlEffect::None;
                    }
                    return ControlEffect::Fetch;
                }
                ControlEffect::None
            }
            // **The run this pane is parked on.** `Latest` unparks it, which
            // is the `None` arm of the fetch — byte for byte the behaviour of
            // every build before this control existed.
            "run" => {
                if let ControlValue::String(ref val) = update.value {
                    let now = chrono::Utc::now().naive_utc();
                    let (floor, current) = {
                        let view = self.view(&pane.as_ref());
                        (view.selected_param.min_forecast_hour(), view.selected_frame)
                    };
                    let frame = decode_run_choice(val, now).map(|run| {
                        // The horizon belongs to the run, so an f36 pick
                        // carried onto an off-cycle run has to come down to
                        // f18 — and up to the parameter's floor, never below.
                        let f_hour = current.map_or(floor, |(_, f_hour)| f_hour);
                        (run, f_hour.clamp(floor, forecast_horizon(run)))
                    });
                    if frame != current {
                        self.edit(pane, |state| state.selected_frame = frame);
                        return self.frame_changed(pane);
                    }
                }
                ControlEffect::None
            }
            // **The forecast hour.** Picking one parks the pane on a definite
            // run: left following the cycle, the picture under the selection
            // would change every time a new run published.
            "f_hour" => {
                if let ControlValue::String(ref val) = update.value
                    && let Some(picked) = parse_f_hour(val)
                {
                    let now = chrono::Utc::now().naive_utc();
                    let (floor, current) = {
                        let view = self.view(&pane.as_ref());
                        (view.selected_param.min_forecast_hour(), view.selected_frame)
                    };
                    let run = current.map_or_else(|| latest_run_at(now), |(run, _)| run);
                    let frame = Some((run, picked.clamp(floor, forecast_horizon(run))));
                    if frame != current {
                        self.edit(pane, |state| state.selected_frame = frame);
                        return self.frame_changed(pane);
                    }
                }
                ControlEffect::None
            }
            "refresh" => ControlEffect::Fetch,
            _ => ControlEffect::None,
        }
    }

    // ── Per-pane state (WO-M10c) ──────────────────────────

    fn create_pane_state(&self, enabled: bool) -> Option<FetchPayload> {
        Some(Box::new(ModelPaneState::new(enabled)))
    }

    /// Field for field what `deserialize_state` does, against the pane's own
    /// state instead of the registry's — and `enabled` falls back to the
    /// pane's slot flag rather than to whatever another pane last left here.
    fn deserialize_pane_state(
        &self,
        value: serde_json::Value,
        enabled: bool,
    ) -> Option<FetchPayload> {
        let mut state = ModelPaneState::new(enabled);
        if let Some(on) = value.get("enabled").and_then(|v| v.as_bool()) {
            state.enabled = on;
        }
        if let Some(param) = value
            .get("parameter")
            .and_then(|v| v.as_str())
            .and_then(|s| s.parse().ok())
        {
            state.selected_param = param;
        }
        if let Some(axis) = value
            .get("axis")
            .and_then(|v| v.as_str())
            .and_then(ModelAxis::parse)
        {
            state.axis = axis;
        }
        // Both halves or neither: a run with no hour is not a frame, and a
        // pane restored onto half a frame would draw the wrong hour rather
        // than nothing.
        //
        // The run half is a **relative** choice resolved against the clock
        // reading it, so an absolute instant left by an older build resolves
        // to nothing and the pane comes back on `Latest`.
        if let Some(run) = value
            .get("run")
            .and_then(|v| v.as_str())
            .and_then(|s| decode_run_choice(s, chrono::Utc::now().naive_utc()))
            && let Some(f_hour) = value
                .get("forecast_hour")
                .and_then(|v| v.as_u64())
                .and_then(|h| u8::try_from(h).ok())
        {
            // The saved hour was picked against the saved run's horizon, and
            // the run this offset resolves to now may be an off-cycle one with
            // 18 hours instead of 48. Clamped rather than dropped: the
            // furthest hour that exists is nearer the intent than no frame.
            let floor = state.selected_param.min_forecast_hour();
            state.selected_frame = Some((run, f_hour.clamp(floor, forecast_horizon(run))));
        }
        Some(Box::new(state))
    }

    /// **Every field of [`ModelPaneState`]**, which is what makes reopen 1:1:
    /// the pane comes back on its parameter, its axis and the frame it was
    /// left parked on. A pane that was never parked writes neither `run` nor
    /// `forecast_hour` and reads back as `None` — the same absence, not a
    /// stamp of the moment it was saved.
    ///
    /// The run is written as a **relative** choice (`latest`, `latest-3`) and
    /// not as an instant. Closing on Friday with 18Z picked and reopening on
    /// Monday must not restore a three-day-old forecast whose only clue is a
    /// small label; a run too far back to be spelled relatively drops both
    /// halves, and the pane reopens on `Latest`.
    fn serialize_pane_state(&self, state: &dyn std::any::Any) -> serde_json::Value {
        let Some(state) = state.downcast_ref::<ModelPaneState>() else {
            return serde_json::Value::Null;
        };
        let mut out = serde_json::json!({
            "enabled": state.enabled,
            "parameter": state.selected_param.as_str(),
            "axis": state.axis.as_str(),
        });
        if let Some((run, f_hour)) = state.selected_frame
            && let Some(choice) = encode_run_choice(run, chrono::Utc::now().naive_utc())
            && let Some(map) = out.as_object_mut()
        {
            map.insert("run".into(), serde_json::json!(choice));
            map.insert("forecast_hour".into(), serde_json::json!(f_hour));
        }
        out
    }

    /// **Two blocks**: the byte-budgeted live grid cache, which holds what the
    /// panes are showing, and the one-grid staging area a loop's frames pass
    /// through ([`MODEL_FRAME_STAGING_BYTES`]).
    ///
    /// Until the loop frames were staged there was one block, because a frame
    /// was a `(param, run, hour)` key beside the live pictures in the same
    /// store — which is what let a 60-frame desktop loop hold 436.1 MiB of
    /// decoded grid under a figure nothing gated on.
    ///
    /// Both figures are maintained fields, so this is two reads whatever the
    /// resident set — which matters more here than for the mosaic layers: a
    /// per-call walk of the entries would be the only figure on the census
    /// that scaled with the scene.
    ///
    /// **What is included beyond the values**: each entry's coordinate axes
    /// and the struct itself, exactly as [`grid_bytes`] prices them for the
    /// budget. **Excluded**: the pane's own carry (`state.data`), which is the
    /// same allocation as its cache entry and is added only when *neither*
    /// store still holds it; and the rasters and textures made from these
    /// grids, which belong to the overlay picture family and the GPU.
    fn resident_source_bytes(&self) -> u64 {
        let carried = match &self.state.data {
            Some(grid) if !self.cached_grids.holds(grid) && !self.frame_grids.holds(grid) => {
                grid_bytes(grid)
            }
            _ => 0,
        };
        (self.cached_grids.resident_bytes() + self.frame_grids.resident_bytes() + carried) as u64
    }

    /// **Let go of every decoded grid**, live cache and staging area both.
    ///
    /// Called once a frame by `Gui::release_data_of_layers_no_pane_draws` for
    /// every layer no pane draws. Until this existed the model layer answered
    /// the trait default `false` and released nothing, and that was the more
    /// serious half of this item rather than an omission beside it: the only
    /// route out of [`ModelGridCache`] is the eviction loop inside its own
    /// `insert`, and inserts stop when the layer is switched off. Every trim
    /// ran on an arrival, arrivals stop with the layer, so a model layer
    /// toggled off held its grids **for the life of the process** — up to the
    /// whole budget, which was 512 MiB on desktop.
    ///
    /// **The same shape MRMS and GMGSI release**, since 2026-09-09. This hook
    /// was written first and argued against theirs on an arithmetic that has
    /// not held up: it read their ceilings off their `GRID_CACHE_BYTES` at
    /// 98 MB and 49 MB and called this one — the largest host-byte family the
    /// crate has — the only one worth a refetch. Measured, MRMS's tiled mosaic
    /// costs 11,333,496 B for a looping pane rather than 98 MB, and GMGSI's
    /// four channels really are 60,001,024 B; and the layer with the *longest
    /// grace* on a released grid is this one, whose run is good for an hour
    /// against their 120 s and 600 s polls. So the ground given here — a
    /// refetch is latency, which the campaign's latitude covers; data held for
    /// a session by a layer nobody is looking at is not — reached all three,
    /// and all three now take it.
    ///
    /// **The way back is covered on both routes.** `OverlayState::release_data`
    /// clears the poll clock and bumps the generation, so the toggle asks
    /// `enable_should_refetch` — true on no data — and a layer that becomes
    /// visible some other way reads as due now. The pane's parked
    /// `(run, hour)` is untouched, so what comes back is the frame it left on.
    ///
    /// Answers whether anything went, so a caller asking every frame does not
    /// bump a generation — and invalidate every cache keyed on it — on a layer
    /// that is already empty.
    fn release_data(&mut self) -> bool {
        // `|_| false` keeps nothing; `retain` answers whether it dropped
        // anything, and both stores are asked before the `||` short-circuits
        // — a `let` apiece rather than an inline disjunction, which would
        // leave the staging area full whenever the live cache was not.
        let staged = self.frame_grids.retain(|_| false);
        let live = self.cached_grids.release_all();
        let carried = self.state.release_data();
        staged || live || carried
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use squallar_geo::GeoBounds;

    const RUN_HOUR: u32 = 3;

    fn at(y: i32, m: u32, d: u32, h: u32) -> chrono::NaiveDateTime {
        chrono::NaiveDate::from_ymd_opt(y, m, d)
            .unwrap()
            .and_hms_opt(h, 0, 0)
            .unwrap()
    }

    /// An absolute run resolves to itself, whatever the clock reading it says.
    ///
    /// This is the whole point of accepting the spelling: a scene naming the
    /// 10 August 2020 derecho means that run. Read on two clocks a year apart
    /// to prove it is not being resolved against `now`.
    #[test]
    fn an_absolute_run_ignores_the_clock() {
        let derecho = at(2020, 8, 10, 17);
        for now in [at(2020, 8, 10, 18), at(2021, 8, 10, 18), at(2026, 1, 1, 0)] {
            assert_eq!(
                decode_run_choice("at:2020-08-10T17:00:00", now),
                Some(derecho),
                "an absolute run must resolve to itself, not to an offset from {now}"
            );
        }
    }

    /// The relative vocabulary still resolves against the clock, unchanged.
    #[test]
    fn a_relative_run_still_tracks_the_clock() {
        let now = at(2026, 8, 24, 18);
        let latest = latest_run_at(now);
        assert_eq!(decode_run_choice("latest", now), Some(latest));
        assert_eq!(
            decode_run_choice("latest-6", now),
            Some(latest - chrono::Duration::hours(6))
        );
        assert_eq!(
            decode_run_choice(&format!("latest-{}", MAX_RUN_OFFSET as u16 + 1), now),
            None,
            "past the vocabulary is still nothing"
        );
    }

    /// Writing never produces the absolute form, so a config this build saves
    /// cannot drift into the past on reopen.
    #[test]
    fn saving_never_writes_an_absolute_run() {
        let now = at(2026, 8, 24, 18);
        let token = encode_run_choice(latest_run_at(now) - chrono::Duration::hours(6), now)
            .expect("six hours back is inside the vocabulary");
        assert_eq!(token, "latest-6");
        assert!(
            token.starts_with(RUN_STEM),
            "every written token must be relative, got {token}"
        );
    }

    /// A BARE instant stays refused, because that is what older builds wrote and
    /// resurrecting one is the exact failure the relative encoding prevents.
    #[test]
    fn a_bare_instant_is_still_refused() {
        let now = at(2026, 8, 24, 18);
        assert_eq!(decode_run_choice("2020-08-10T17:00:00", now), None);
    }

    /// An instant between runs snaps to the run hour rather than naming a grid
    /// that does not exist.
    #[test]
    fn an_absolute_run_snaps_to_the_hour() {
        let now = at(2026, 8, 24, 18);
        assert_eq!(
            decode_run_choice("at:2020-08-10T17:43:21", now),
            Some(at(2020, 8, 10, 17))
        );
    }

    fn grid(parameter: ModelParameter, values: Vec<f32>) -> HrrrGridData {
        let n = values.len();
        let (visible_points, value_range) =
            crate::hrrr::summarize_values(&values, |v| parameter.paints(v));
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
            forecast_hour: parameter.min_forecast_hour(),
            visible_points,
            value_range,
        }
    }

    fn handler(parameter: ModelParameter, values: Vec<f32>) -> ModelDataHandler {
        let mut h = new_handler();
        h.defaults.enabled = true;
        h.defaults.selected_param = parameter;
        h.apply_fetch_result(
            Box::new(HrrrFetchResult(Ok(grid(parameter, values)))),
            &PaneRef::across(&[]),
        );
        h
    }

    fn controls_of(h: &ModelDataHandler) -> Vec<ControlItem> {
        h.controls(&PaneRef::bare(0))
    }

    /// **This layer answers "which field is this pane showing" from its own
    /// per-pane state, and answers it with a registered id.**
    ///
    /// The pane-side 3D walk asks every layer this before it decides which to
    /// ask for a grid, so an answer spelled by hand rather than taken from the
    /// registry row would be an id no lookup can resolve.
    #[test]
    fn the_current_field_is_this_panes_own_parameter_as_the_registry_spells_it() {
        let h = handler(ModelParameter::SurfaceBasedCape, vec![1.0]);
        let state = pane_state(ModelParameter::SurfaceBasedCin);
        let pane = PaneRef {
            state: Some(&*state),
            ..PaneRef::bare(0)
        };

        let field = h
            .current_field(&pane)
            .expect("a layer with sixteen parameters has a current field");
        assert_eq!(
            field,
            crate::hrrr::fields::spec(ModelParameter::SurfaceBasedCin)
                .id
                .clone(),
            "the answer must be THIS PANE's parameter, not the registry \
             copy's — the pane holds CIN and the handler's own default is CAPE",
        );
        assert!(
            h.products().iter().any(|spec| spec.id == field),
            "the id must be one this layer publishes, or nothing above can \
             resolve it to a row",
        );
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
        let (visible_points, value_range) =
            crate::hrrr::summarize_values(&values, |v| parameter.paints(v));
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
            forecast_hour: parameter.min_forecast_hour(),
            visible_points,
            value_range,
        };
        let mut h = new_handler();
        h.defaults.enabled = true;
        h.defaults.selected_param = parameter;
        h.apply_fetch_result(Box::new(HrrrFetchResult(Ok(g))), &PaneRef::across(&[]));
        h
    }

    #[test]
    fn hover_reports_the_nearest_grid_points_value() {
        let h = hover_handler();
        assert_eq!(
            h.hover_value_at(35.001, -97.099, &PaneRef::bare(0))
                .as_deref(),
            Some("SBCAPE: 300 J/kg"),
        );
        assert_eq!(
            h.hover_value_at(35.099, -97.001, &PaneRef::bare(0))
                .as_deref(),
            Some("SBCAPE: 4100 J/kg"),
        );
        assert_eq!(
            h.hover_value_at(35.001, -97.001, &PaneRef::bare(0))
                .as_deref(),
            Some("SBCAPE: 1200 J/kg"),
        );
    }

    #[test]
    fn hover_is_silent_outside_the_grid_bounds() {
        let h = hover_handler();
        assert_eq!(h.hover_value_at(40.0, -97.05, &PaneRef::bare(0)), None);
        assert_eq!(h.hover_value_at(35.05, -90.0, &PaneRef::bare(0)), None);
    }

    /// Inside the bounds but ~7.8 km from all four points, past the 0.05° cutoff.
    #[test]
    fn hover_is_silent_further_than_the_cutoff_from_every_point() {
        assert_eq!(
            hover_handler().hover_value_at(35.05, -97.05, &PaneRef::bare(0)),
            None
        );
    }

    /// 0.02° north of the top edge: outside the bounds but *inside* the 0.05°
    /// cutoff, so only the bounds test can reject it.
    #[test]
    fn hover_is_silent_just_outside_the_bounds_beside_a_real_point() {
        assert_eq!(
            hover_handler().hover_value_at(35.12, -97.0, &PaneRef::bare(0)),
            None
        );
    }

    #[test]
    fn hover_is_silent_before_any_data_arrives() {
        assert_eq!(
            new_handler().hover_value_at(35.0, -97.0, &PaneRef::bare(0)),
            None
        );
    }

    /// A grid written past the seam, `185..195`, three points along one row.
    fn seam_hover_handler() -> ModelDataHandler {
        let parameter = ModelParameter::SurfaceBasedCape;
        let values = vec![300.0, 1200.0, 4100.0];
        let (visible_points, value_range) =
            crate::hrrr::summarize_values(&values, |v| parameter.paints(v));
        let g = HrrrGridData {
            parameter,
            values,
            coords: crate::hrrr::GridCoords::Explicit {
                lats: vec![35.0; 3],
                lons: vec![185.0, 190.0, 195.0],
            },
            ni: 3,
            nj: 1,
            bounds: GeoBounds {
                min_lat: 35.0,
                max_lat: 35.0,
                min_lon: 185.0,
                max_lon: 195.0,
            },
            ref_time: chrono::NaiveDate::from_ymd_opt(2026, 7, 25)
                .unwrap()
                .and_hms_opt(RUN_HOUR, 0, 0)
                .unwrap(),
            forecast_hour: parameter.min_forecast_hour(),
            visible_points,
            value_range,
        };
        let mut h = new_handler();
        h.defaults.enabled = true;
        h.defaults.selected_param = parameter;
        h.apply_fetch_result(Box::new(HrrrFetchResult(Ok(g))), &PaneRef::across(&[]));
        h
    }

    /// **The pointer past the seam, in both spellings.** `Projector::unproject`
    /// folds nothing, so over a grid at `185..195` the pointer reads 190 when
    /// the map was panned there and -170 when it was not; both are the same
    /// ground. The unfolded cull refused the second and the readout went blank.
    #[test]
    fn hover_hits_past_the_seam_whichever_way_the_pointer_is_written() {
        let h = seam_hover_handler();
        let at = |lon: f64| h.hover_value_at(35.0, lon, &PaneRef::bare(0));
        assert_eq!(at(190.0).as_deref(), Some("SBCAPE: 1200 J/kg"));
        assert_eq!(
            at(-170.0).as_deref(),
            Some("SBCAPE: 1200 J/kg"),
            "the same ground, written in +/-180"
        );
        assert_eq!(
            at(-165.0).as_deref(),
            Some("SBCAPE: 4100 J/kg"),
            "the eastern edge, written in +/-180"
        );
        assert_eq!(at(0.0), None, "half a world away in either spelling");
    }

    #[test]
    fn a_fetch_error_is_reported_in_the_controls() {
        let mut h = new_handler();
        h.defaults.enabled = true;
        h.defaults.selected_param = ModelParameter::MaxUH2to5km;
        h.apply_fetch_result(
            Box::new(HrrrFetchResult(Err(
                crate::fetch_policy::FetchError::transient("HTTP 500"),
            ))),
            &PaneRef::across(&[]),
        );

        let lines = info_lines(&h);
        assert!(
            lines.iter().any(|l| l.contains("HTTP 500")),
            "fetch error must be surfaced, got {lines:?}",
        );
    }

    #[test]
    fn a_successful_fetch_clears_a_previous_error() {
        let mut h = new_handler();
        h.defaults.enabled = true;
        h.defaults.selected_param = ModelParameter::MaxUH2to5km;
        h.apply_fetch_result(
            Box::new(HrrrFetchResult(Err(
                crate::fetch_policy::FetchError::transient("HTTP 500"),
            ))),
            &PaneRef::across(&[]),
        );
        h.apply_fetch_result(
            Box::new(HrrrFetchResult(Ok(grid(
                ModelParameter::MaxUH2to5km,
                vec![120.0],
            )))),
            &PaneRef::across(&[]),
        );

        let lines = info_lines(&h);
        assert!(!lines.iter().any(|l| l.contains("HTTP 500")), "{lines:?}");
    }

    /// **How many fixture grids these tests' cache holds.**
    ///
    /// The suite below predates the byte budget and is about eviction *order*,
    /// which is unchanged: it wants a cache that fills at a known count. So it
    /// states the count and [`test_budget`] converts it into the bytes the
    /// production cache is really bounded by — six one-point grids, not six
    /// entries.
    const CACHE_ENTRIES: usize = 6;

    // At a cap of 1, an insert of a key that is not the selected one protects
    // both the arrival and the pin and the cache settles at two entries for
    // ever. Two is where the eviction loop is guaranteed a victim.
    const _: () = assert!(CACHE_ENTRIES >= 2);

    /// [`CACHE_ENTRIES`] fixture grids' worth of bytes.
    fn test_budget() -> usize {
        CACHE_ENTRIES * grid_bytes(&grid(ModelParameter::all()[0], vec![300.0]))
    }

    /// A handler whose live cache holds exactly [`CACHE_ENTRIES`] fixture
    /// grids and whose staging area holds exactly **one** — the shipped
    /// proportion, at the fixture's scale.
    ///
    /// The staging budget has to be scaled too, and that is not housekeeping:
    /// it is spelled in *bytes* ([`MODEL_FRAME_STAGING_BYTES`] is one CONUS
    /// grid), so against one-point fixtures the shipped figure holds hundreds
    /// of them and every "the loop stages one frame" assertion passes on a
    /// store that was simply never full. Measured while writing this: 48
    /// fixture frames resident under the production budget.
    fn new_handler() -> ModelDataHandler {
        let mut h = ModelDataHandler::new();
        h.cached_grids = ModelGridCache::with_budget(test_budget());
        h.frame_grids = ModelFrameCache::with_budget(one_fixture_grid());
        h
    }

    /// What [`grid_bytes`] charges for one fixture grid — the staging budget's
    /// denominator, read out of the function rather than restated.
    fn one_fixture_grid() -> usize {
        grid_bytes(&grid(ModelParameter::all()[0], vec![300.0]))
    }

    /// The key a fixture grid of `param` files itself under: the fixture run,
    /// at the parameter's own floor.
    fn key(param: ModelParameter) -> GridKey {
        GridKey {
            param,
            run: run_time(),
            f_hour: param.min_forecast_hour(),
        }
    }

    /// The parameters these tests fill the cache with, in fetch order: exactly
    /// enough to fill it, plus one more to overflow it. Taken from
    /// [`ModelParameter::all`] so the set follows [`CACHE_ENTRIES`].
    fn fill_order() -> &'static [ModelParameter] {
        let need = CACHE_ENTRIES + 1;
        let all = ModelParameter::all();
        assert!(
            all.len() >= need,
            "these tests need {need} distinct parameters to overflow a cache of \
             {CACHE_ENTRIES}, and there are {}",
            all.len(),
        );
        &all[..need]
    }

    fn resident_order() -> &'static [ModelParameter] {
        &fill_order()[..CACHE_ENTRIES]
    }

    fn oldest() -> ModelParameter {
        fill_order()[0]
    }

    fn next_oldest() -> ModelParameter {
        fill_order()[1]
    }

    fn overflow() -> ModelParameter {
        fill_order()[CACHE_ENTRIES]
    }

    fn deliver(h: &mut ModelDataHandler, parameter: ModelParameter) {
        h.apply_fetch_result(
            Box::new(HrrrFetchResult(Ok(grid(parameter, vec![300.0])))),
            &PaneRef::across(&[]),
        );
    }

    fn rasterize_ctx() -> RasterizeContext {
        let clock = chrono::NaiveDate::from_ymd_opt(2026, 7, 25)
            .unwrap()
            .and_hms_opt(3, 0, 0)
            .unwrap();
        RasterizeContext {
            is_dark: false,
            zoom: 5.0,
            device_scale: 1.0,
            now: clock,
            as_of: clock,
            frame: None,
        }
    }

    fn control_ctx<'a>() -> PaneMut<'a> {
        PaneMut::bare(0)
    }

    fn full_cache() -> ModelDataHandler {
        let mut h = new_handler();
        h.defaults.enabled = true;
        for &p in resident_order() {
            h.defaults.selected_param = p;
            deliver(&mut h, p);
        }
        assert_eq!(
            h.cached_grids.len(),
            CACHE_ENTRIES,
            "the fixture must be full before a test evicts from it",
        );
        assert_eq!(
            h.cached_grids.recency_params(),
            resident_order().to_vec(),
            "fixture recency, oldest first",
        );
        h
    }

    /// One fixture grid of `param` at forecast hour `f_hour`, carrying `value`
    /// as its single point so the grid a job describes can be named by reading
    /// one number back out of it.
    fn valued_grid_at_hour(param: ModelParameter, f_hour: u8, value: f32) -> HrrrGridData {
        let mut g = grid(param, vec![value]);
        g.forecast_hour = f_hour;
        g
    }

    /// The single value of the grid a described job is carrying.
    fn job_value(job: &DescribedJob) -> f32 {
        let Some(rasterize::GriddedInput::Whole(grid)) = job.downcast_ref() else {
            panic!("the model layer described a job of another kind");
        };
        grid.values[0]
    }

    /// **WI-6b's central claim: the raster is of the frame the context names,
    /// not of the frame the pane has selected.**
    ///
    /// A loop wants several of this layer's frames on screen inside one second.
    /// `ModelPaneState::selected_frame` can only ever say one thing, so without
    /// this every frame of a forecast loop would receive the *same* picture —
    /// whichever hour the pane happens to be parked on — and each would be
    /// captioned with its own stamp. That is a lie on the glass, not a missing
    /// feature, and it is the reason `RasterizeContext::frame` exists.
    ///
    /// **Floor: give `prepare_job` back its `_ctx` and let it read `grid_of`
    /// unconditionally.** Both asks then answer 1.0 and the third assertion
    /// below fails naming the value it got — pinned as a value comparison and
    /// not as `is_some()`, because a presence check passes on exactly the
    /// defect this test exists for.
    #[test]
    fn a_named_frame_is_rasterized_from_that_frames_grid_and_not_the_panes() {
        let param = ModelParameter::SurfaceBasedCape;
        let run = run_time();
        // Two hours of one run, resident together — which is the state a loop
        // puts the cache in, and the state a single-frame pane never reaches.
        let mut h = new_handler();
        h.defaults.enabled = true;
        h.defaults.selected_param = param;
        h.defaults.axis = ModelAxis::Forecast;
        for (f_hour, value) in [(0u8, 1.0f32), (1, 2.0)] {
            h.cached_grids.insert(
                GridKey { param, run, f_hour },
                Arc::new(valued_grid_at_hour(param, f_hour, value)),
                &[],
            );
        }
        // The pane itself is parked on f00 — the "current selection" the
        // dispatch used to be forced to rasterize.
        h.defaults.selected_frame = Some((run, 0));

        let live = h
            .prepare_job(&rasterize_ctx(), &PaneRef::bare(0))
            .expect("the pane's own grid is resident");
        assert_eq!(
            job_value(&live),
            1.0,
            "control: a dispatch that names no frame must still rasterize the \
             pane's own selection, byte for byte as it did before \
             `RasterizeContext::frame` existed",
        );

        let ask = |f_hour: u8| {
            let ctx = RasterizeContext {
                frame: Some(FrameStamp {
                    valid: run + chrono::Duration::hours(i64::from(f_hour)),
                    run: Some(run),
                }),
                ..rasterize_ctx()
            };
            h.prepare_job(&ctx, &PaneRef::bare(0))
        };

        assert_eq!(
            job_value(&ask(0).expect("f00 is resident")),
            1.0,
            "the frame at f00 was not rasterized from f00's grid",
        );
        assert_eq!(
            job_value(&ask(1).expect("f01 is resident")),
            2.0,
            "THE claim: the frame at f01 was rasterized from the pane's own \
             selection (f00) instead of from f01's grid. Every frame of a \
             forecast loop would carry the same picture under a different \
             caption.",
        );

        // A frame whose grid has not landed describes nothing at all. The
        // alternative is the pane's current picture filed into another
        // instant's frame, which is the defect the draw fork exists to stop.
        assert!(
            ask(2).is_none(),
            "a frame with no resident grid fell back to the pane's picture",
        );
    }

    /// A full desktop layout is six unlinked panes, each free to select its own
    /// parameter. This is the case a cache that let another pane's arrival take
    /// a pinned grid would break, and it would break *silently*: `prepare_job`
    /// answers `None` and the starved pane goes on drawing its last texture,
    /// with nothing to re-ask.
    #[test]
    fn every_pane_of_a_full_desktop_layout_keeps_a_drawable_grid() {
        // Spelled, not imported: squallar-overlays cannot depend on squallar-egui.
        const MAX_PANES_DESKTOP: usize = 6;
        let panes = &ModelParameter::all()[..MAX_PANES_DESKTOP];

        let mut h = new_handler();
        h.defaults.enabled = true;
        for &p in panes {
            h.defaults.selected_param = p;
            deliver(&mut h, p);
        }

        for &p in panes {
            h.defaults.selected_param = p;
            assert!(
                h.has_data(&PaneRef::bare(0)),
                "the pane showing {p:?} has no grid"
            );
            assert!(
                h.prepare_job(&rasterize_ctx(), &PaneRef::bare(0)).is_some(),
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
        h.defaults.selected_param = overflow();
        deliver(&mut h, overflow());

        assert!(
            !h.cached_grids.is_resident(key(oldest())),
            "the least recently touched grid survived an overflowing insert",
        );
        for &p in &fill_order()[1..] {
            assert!(
                h.cached_grids.is_resident(key(p)),
                "{p:?} must still be resident"
            );
        }
        assert_eq!(h.cached_grids.len(), CACHE_ENTRIES);
        assert_eq!(h.cached_grids.recency_params(), fill_order()[1..].to_vec());
    }

    /// Cycling the whole Parameter dropdown is the gesture that grew this map to
    /// sixteen grids. The count is asserted *exactly*: "never exceeds the cap" is
    /// satisfied by a cache holding nothing.
    #[test]
    fn cycling_every_parameter_leaves_exactly_the_cap_resident() {
        let mut h = new_handler();
        h.defaults.enabled = true;
        for (i, p) in ModelParameter::all().iter().enumerate() {
            h.defaults.selected_param = *p;
            deliver(&mut h, *p);
            let expected = (i + 1).min(CACHE_ENTRIES);
            assert_eq!(
                h.cached_grids.len(),
                expected,
                "after {} of {} parameters",
                i + 1,
                ModelParameter::all().len(),
            );
            assert!(
                h.cached_grids.is_resident(key(*p)),
                "the parameter just fetched is the one on screen: {p:?}",
            );
            assert_eq!(
                h.cached_grids.recency_params().len(),
                expected,
                "the recency list must hold exactly the keys of the map",
            );
        }
        let tail = &ModelParameter::all()[ModelParameter::all().len() - CACHE_ENTRIES..];
        assert_eq!(h.cached_grids.recency_params(), tail.to_vec());
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
            CACHE_ENTRIES >= 3,
            "this test walks three distinct parameters through the cache",
        );
        let mut h = full_cache();

        fn counted_as_a_use(h: &ModelDataHandler, read: ModelParameter, path: &str) {
            let order = h.cached_grids.recency_params();
            assert_eq!(
                order.len(),
                CACHE_ENTRIES,
                "{path}: the recency list must still hold every key, got {order:?}",
            );
            assert_eq!(
                order.last(),
                Some(&read),
                "{path}: the read did not count as a use, order is {order:?}",
            );
        }

        let p = h.cached_grids.recency_params()[0];
        h.defaults.selected_param = p;
        assert!(
            h.hover_value_at(35.0, -97.0, &PaneRef::bare(0)).is_some(),
            "the fixture must answer a hover, or this step proves nothing",
        );
        counted_as_a_use(&h, p, "hover_value_at");

        let p = h.cached_grids.recency_params()[0];
        h.defaults.selected_param = p;
        assert!(
            h.prepare_job(&rasterize_ctx(), &PaneRef::bare(0)).is_some(),
            "the fixture must answer a rasterize",
        );
        counted_as_a_use(&h, p, "prepare_job");

        let p = h.cached_grids.recency_params()[0];
        h.defaults.selected_param = p;
        assert_ne!(
            toggle_label(&h),
            "Model Data",
            "the label must be the one built from a resident grid — only the \
             `Some(grid)` arm of `controls` can produce a time in it",
        );
        counted_as_a_use(&h, p, "controls");

        let p = h.cached_grids.recency_params()[0];
        h.defaults.selected_param = p;
        assert!(h.has_data(&PaneRef::bare(0)), "{p:?} is resident");
        counted_as_a_use(&h, p, "has_data");

        let p = h.cached_grids.recency_params()[0];
        assert_ne!(
            p, h.defaults.selected_param,
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
        h.defaults.selected_param = oldest();
        assert!(
            h.hover_value_at(35.0, -97.0, &PaneRef::bare(0)).is_some(),
            "the fixture must answer a hover",
        );

        h.defaults.selected_param = overflow();
        deliver(&mut h, overflow());

        assert!(
            h.cached_grids.is_resident(key(oldest())),
            "the hovered grid was evicted anyway",
        );
        assert!(
            !h.cached_grids.is_resident(key(next_oldest())),
            "the oldest use is what must go",
        );
        assert_eq!(h.cached_grids.len(), CACHE_ENTRIES);
    }

    /// The parameter the pane is showing is pinned, even when it is the oldest
    /// thing in the list: `deserialize_state` assigns it bare when a pane is
    /// swapped in, so a pane can sit on a grid nothing has touched since.
    #[test]
    fn the_selected_parameter_survives_an_insert_that_would_evict_it() {
        let mut h = full_cache();
        h.defaults.selected_param = oldest();
        assert_eq!(
            h.cached_grids.recency_params(),
            resident_order().to_vec(),
            "a bare assignment must not count as a use",
        );

        // Another parameter's fetch lands while `oldest` is still on screen.
        deliver(&mut h, overflow());

        assert!(
            h.cached_grids.is_resident(key(oldest())),
            "the parameter on screen was evicted under the user",
        );
        assert!(
            !h.cached_grids.is_resident(key(next_oldest())),
            "the eviction must still happen, one entry along",
        );
        assert!(h.cached_grids.is_resident(key(overflow())));
        assert_eq!(h.cached_grids.len(), CACHE_ENTRIES);
    }

    /// **A re-fetch replaces its own RUN**, not merely its own parameter.
    ///
    /// Deliberately updated at S2 2.5, not re-pointed: the key was the
    /// parameter alone and the cache was documented as carrying no run
    /// identity, so *any* re-fetch of a parameter replaced its entry. It now
    /// replaces only the entry of the same `(parameter, run, forecast hour)` —
    /// which is what a plain re-fetch is — and a fetch of another run of the
    /// same parameter lands **beside** it. Both halves are asserted; the
    /// second is what fails if the run silently left the key again.
    #[test]
    fn a_refetch_of_a_resident_parameter_replaces_its_own_run() {
        let mut h = full_cache();
        h.defaults.selected_param = oldest();
        deliver(&mut h, oldest());

        assert_eq!(
            h.cached_grids.len(),
            CACHE_ENTRIES,
            "a re-fetch must not grow the map",
        );
        for &p in resident_order() {
            assert!(
                h.cached_grids.is_resident(key(p)),
                "{p:?} must still be resident"
            );
        }
        let order = h.cached_grids.recency_params();
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

        // The other half: a different run of the same parameter is a
        // different picture and must not overwrite this one.
        let later = run_time() + chrono::Duration::hours(1);
        let mut grid = grid(oldest(), vec![300.0]);
        grid.ref_time = later;
        h.apply_fetch_result(Box::new(HrrrFetchResult(Ok(grid))), &PaneRef::across(&[]));
        assert!(
            h.cached_grids.is_resident(GridKey {
                run: later,
                ..key(oldest())
            }),
            "the newer run is not resident at all",
        );
        assert!(
            h.cached_grids.is_resident(key(oldest())),
            "a second run of one parameter overwrote the first — the cache is \
             run-blind again, and a scrub between two runs would refetch every \
             step",
        );
    }

    /// **Two forecast hours of one parameter are two resident grids.**
    ///
    /// The case the parameter-keyed cache could not express at all: a scrub
    /// from f00 to f06 held one entry that each arrival overwrote, so stepping
    /// back cost a refetch every time.
    ///
    /// Non-vacuity: the two grids are asserted to hold **different values**,
    /// so a cache that kept one entry and answered it for both keys fails.
    #[test]
    fn two_forecast_hours_of_one_parameter_are_both_resident() {
        let param = ModelParameter::SurfaceBasedCape;
        let mut h = new_handler();
        h.defaults.enabled = true;
        h.defaults.selected_param = param;

        for (f_hour, value) in [(0u8, 11.0f32), (6, 22.0)] {
            let mut g = grid(param, vec![value]);
            g.forecast_hour = f_hour;
            h.apply_fetch_result(Box::new(HrrrFetchResult(Ok(g))), &PaneRef::across(&[]));
        }

        for (f_hour, value) in [(0u8, 11.0f32), (6, 22.0)] {
            let resident = h
                .cached_grids
                .get(GridKey {
                    param,
                    run: run_time(),
                    f_hour,
                })
                .unwrap_or_else(|| panic!("f{f_hour:02} is not resident"));
            assert_eq!(
                resident.values,
                vec![value],
                "f{f_hour:02} answered another hour's grid, so the two keys \
                 share one entry",
            );
        }
        assert_eq!(h.cached_grids.len(), 2, "two hours, two entries");
    }

    /// **A pinned key is never the victim, even past the budget.**
    ///
    /// The union of the panes' current keys is what must survive; a pane whose
    /// grid is taken away fails *silently* — `prepare_job` answers `None` and
    /// it goes on drawing its last texture with nothing that will re-ask.
    ///
    /// Non-triviality floor: the budget is set to **one** grid and six panes
    /// are pinned, so every insert after the first is over budget and the
    /// eviction loop runs on every one of them. A cache that evicted pinned
    /// keys would be down to one entry by the end.
    #[test]
    fn the_cache_never_evicts_a_pinned_key() {
        let params = &ModelParameter::all()[..CACHE_ENTRIES];
        let states: Vec<FetchPayload> = params.iter().map(|p| pane_state(*p)).collect();
        let peers: Vec<&dyn std::any::Any> =
            states.iter().map(|s| &**s as &dyn std::any::Any).collect();

        let mut h = new_handler();
        h.cached_grids = ModelGridCache::with_budget(grid_bytes(&grid(params[0], vec![300.0])));
        for p in params {
            h.apply_fetch_result(
                Box::new(HrrrFetchResult(Ok(grid(*p, vec![300.0])))),
                &PaneRef::across(&peers),
            );
        }

        assert!(
            h.cached_grids.len() > 1,
            "premise: the budget really is smaller than the pinned set, or \
             nothing here was ever over budget",
        );
        for p in params {
            assert!(
                h.cached_grids.is_resident(key(*p)),
                "{p:?} is pinned by a pane and was evicted anyway",
            );
        }
        assert_eq!(h.cached_grids.len(), CACHE_ENTRIES);

        // And an UNpinned key still goes, or "never evicts a pinned key" is
        // true of a cache that never evicts anything.
        let spare = overflow();
        h.apply_fetch_result(
            Box::new(HrrrFetchResult(Ok(grid(spare, vec![300.0])))),
            &PaneRef::across(&peers),
        );
        h.apply_fetch_result(
            Box::new(HrrrFetchResult(Ok(grid(spare, vec![300.0])))),
            &PaneRef::across(&peers),
        );
        assert_eq!(
            h.cached_grids.len(),
            CACHE_ENTRIES + 1,
            "the unpinned arrival should be the only thing above the pinned \
             set, and the next one should take its place",
        );
    }

    /// **The byte budget holds at least one grid per pane, on every target.**
    ///
    /// The floor a byte budget can cross that an entry cap could not: below it
    /// a full pane layout starves, and it starves silently. Asserted for all
    /// three arms from a host test — a `cfg`'d figure checked only on the
    /// target that selects it is a figure nobody checked.
    ///
    /// The **denominator** is what makes this non-vacuous, so it is checked
    /// rather than assumed: [`HRRR_CONUS_GRID_BYTES`] is asserted to be what
    /// [`grid_bytes`] really counts for a CONUS-shaped grid, computed from a
    /// one-point fixture rather than allocating 7.6 MB.
    #[test]
    fn the_byte_budget_holds_at_least_one_grid_per_pane() {
        // What `grid_bytes` charges for a grid of `n` points on the arm HRRR
        // actually decodes on — read out of the function, not restated.
        let one = grid_bytes(&grid(ModelParameter::all()[0], vec![0.0]));
        let two = grid_bytes(&grid(ModelParameter::all()[0], vec![0.0, 0.0]));
        let per_point = two - one;
        assert_eq!(
            per_point,
            std::mem::size_of::<f32>() + 2 * std::mem::size_of::<f64>(),
            "the fixture's Explicit coordinates cost a point too; if this \
             moved, the CONUS figure below is measuring something else",
        );
        // The production arm is Lambert, whose coordinates are closed forms.
        let conus =
            crate::hrrr::lambert::LambertGrid::from_parts(crate::hrrr::lambert::LambertGridParts {
                a: 6_371_229.0,
                e: 0.0,
                n: 0.615_661_5,
                big_f: 1.5,
                rho0: 1.0,
                lon0: -97.5,
                x0: 0.0,
                y0: 0.0,
                dx: 3000.0,
                dy: 3000.0,
                ni: 1799,
                nj: 1059,
                i_consecutive: true,
                alternating: false,
                wraps_longitude: false,
            });
        let mut conus_grid = grid(ModelParameter::all()[0], vec![0.0]);
        conus_grid.coords =
            crate::hrrr::GridCoords::Lambert(conus.expect("the CONUS parts are a real grid"));
        conus_grid.values = vec![0.0; 1799 * 1059];
        assert_eq!(
            grid_bytes(&conus_grid) - std::mem::size_of::<HrrrGridData>(),
            HRRR_CONUS_GRID_BYTES,
            "HRRR_CONUS_GRID_BYTES is not what grid_bytes charges for a CONUS \
             grid, so every budget below is divided by the wrong number",
        );

        for (name, budget, panes) in [
            ("wasm32", WASM_MODEL_GRID_BUDGET_BYTES, MAX_PANES_DESKTOP),
            ("mobile", MOBILE_MODEL_GRID_BUDGET_BYTES, MAX_PANES_MOBILE),
            (
                "desktop",
                DESKTOP_MODEL_GRID_BUDGET_BYTES,
                MAX_PANES_DESKTOP,
            ),
        ] {
            let grids = budget / HRRR_CONUS_GRID_BYTES;
            assert!(
                grids >= panes,
                "{name} budgets {budget} bytes = {grids} CONUS grids for \
                 {panes} panes. Below one grid per pane the pinned keys are \
                 never victims, so the cache overruns the budget and there is \
                 no symptom: the figure says less than the heap holds.",
            );
        }
        assert_eq!(
            (
                WASM_MODEL_GRID_BUDGET_BYTES / HRRR_CONUS_GRID_BYTES,
                MOBILE_MODEL_GRID_BUDGET_BYTES / HRRR_CONUS_GRID_BYTES,
                DESKTOP_MODEL_GRID_BUDGET_BYTES / HRRR_CONUS_GRID_BYTES,
            ),
            (8, 8, 13),
            "the figures the module doc states",
        );
    }

    /// **A loop's frames cost the live cache nothing, however long the loop
    /// is** — the property that replaced `model_loop_frame_cap`.
    ///
    /// The deleted test here asked what that function computed: how many
    /// frames a loop could hold against the grid budget, 8/23/65 on the three
    /// arms. It could not go red on the thing that was actually wrong, because
    /// the function had **no production caller** — `layer_share` builds an
    /// overlay loop's list with `count_cap: None` and prices a frame at
    /// `overlay_frame_price`, the texture — so the grid each frame pinned was
    /// in no bound at all and the real ceiling was the budget itself.
    ///
    /// So the question is inverted: rather than pinning a cap nothing applied,
    /// pin that the live cache is **independent of loop length**. Denominator:
    /// one CONUS grid is [`HRRR_CONUS_GRID_BYTES`] = 7,620,564 B = 7.268 MiB,
    /// which `the_byte_budget_holds_at_least_one_grid_per_pane` proves is what
    /// [`grid_bytes`] really charges rather than assuming it.
    ///
    /// **Floor — route `apply_frame` back at `cached_grids`:** the live cache
    /// grows one entry per frame and the first assertion fails naming the
    /// count it got. Tampered both ways: with the staging budget raised to
    /// hold every frame, the *staging* assertion still holds it to one grid's
    /// worth of bytes, which is the conjunct that would otherwise pass on a
    /// cache that simply had room.
    #[test]
    fn a_loop_of_any_length_leaves_the_live_cache_alone() {
        let param = ModelParameter::SurfaceBasedCape;
        let run = run_time();
        let mut h = new_handler();
        h.defaults.enabled = true;
        h.defaults.selected_param = param;
        h.defaults.axis = ModelAxis::Forecast;
        h.defaults.selected_frame = Some((run, 0));
        // The pane's own picture, in the live cache where it belongs.
        h.cached_grids.insert(
            GridKey {
                param,
                run,
                f_hour: 0,
            },
            Arc::new(valued_grid_at_hour(param, 0, 1.0)),
            &[],
        );
        let live_before = h.cached_grids.len();
        let bytes_before = h.cached_grids.resident_bytes();

        // A whole 48-hour forecast loop, f01 through f48 — every frame the
        // longest HRRR cycle publishes, delivered the way the loop delivers
        // them.
        for f_hour in 1..=48u8 {
            h.apply_frame(
                FrameStamp {
                    valid: run + chrono::Duration::hours(i64::from(f_hour)),
                    run: Some(run),
                },
                Box::new(ModelFrameFetch {
                    key: GridKey { param, run, f_hour },
                    grid: Some(valued_grid_at_hour(param, f_hour, f32::from(f_hour))),
                }),
                &PaneRef::bare(0),
            );
        }

        assert_eq!(
            (h.cached_grids.len(), h.cached_grids.resident_bytes()),
            (live_before, bytes_before),
            "forty-eight loop frames moved the LIVE cache. That is the defect \
             this item removed: a frame was a `(param, run, hour)` key beside \
             the pane's own pictures, so a 60-frame desktop loop held \
             60 x 7,620,780 = 436.1 MiB of decoded grid in a store budgeted \
             for the pane set.",
        );
        assert_eq!(
            h.frame_grids.len(),
            1,
            "the staging area holds more than the one frame it stages",
        );
        assert!(
            h.frame_grids.resident_bytes() <= one_fixture_grid(),
            "the staging area is over one grid's worth at {} bytes against a \
             budget of {}, so the figure is bounded by the frame count after \
             all",
            h.frame_grids.resident_bytes(),
            one_fixture_grid(),
        );
    }

    /// **A layer no pane draws gives back every decoded grid, and says so
    /// once.**
    ///
    /// Before this the model layer had no `release_data` at all and took the
    /// trait default `false`. That was not a gap beside the loop-frame item
    /// but the more serious half of it: the only route out of
    /// [`ModelGridCache`] is the eviction loop inside its own `insert`, and
    /// inserts stop when the layer is switched off — so every trim ran on an
    /// arrival, arrivals stop with the layer, and a model layer toggled off
    /// held its grids for the life of the process. Up to the whole budget,
    /// which was 512 MiB on desktop when this was written.
    ///
    /// **Both stores and the carry**, which is why the first assertion is a
    /// byte figure and not a pair of `is_empty()`s:
    /// [`OverlayHandler::resident_source_bytes`] is the sum the census reads,
    /// so it is the thing that has to reach zero.
    ///
    /// **Answering `false` on the second call is a contract, not an
    /// optimisation.** `Gui::release_data_of_layers_no_pane_draws` asks every
    /// layer no pane draws once a *frame*; a handler that answered `true` on
    /// an empty store would bump its generation sixty times a second and
    /// invalidate every cache keyed on it.
    ///
    /// **The way back is a refetch of the frame the pane was left on**, which
    /// is the cost this trades for the bytes and is inside the campaign's
    /// latitude — latency, not a lost affordance. So the pane's parked
    /// `(run, hour)` must survive: releasing it would silently move the user
    /// to `Latest`, which is a changed picture rather than a slower one.
    ///
    /// **Floor — return the trait default `false` and release nothing:** the
    /// first assertion fails naming the bytes still held. Each conjunct
    /// tampered on its own; the second-call conjunct needs its own tamper
    /// (`release_all` answering `true` unconditionally), because a handler
    /// that really did release everything passes the first three regardless.
    #[test]
    fn a_layer_no_pane_draws_gives_back_every_grid_and_says_so_once() {
        let param = ModelParameter::SurfaceBasedCape;
        let run = run_time();
        let mut h = seeded(param, 0);
        h.defaults.selected_frame = Some((run, 0));
        // A staged loop frame beside the live picture, so both stores are
        // non-empty when the layer goes off.
        h.apply_frame(
            FrameStamp {
                valid: run + chrono::Duration::hours(6),
                run: Some(run),
            },
            Box::new(ModelFrameFetch {
                key: GridKey {
                    param,
                    run,
                    f_hour: 6,
                },
                grid: Some(valued_grid_at_hour(param, 6, 6.0)),
            }),
            &PaneRef::bare(0),
        );
        assert!(
            h.cached_grids.len() > 0 && h.frame_grids.len() > 0,
            "premise: both stores hold something before the release",
        );
        let parked = h.defaults.selected_frame;

        assert!(h.release_data(), "there was something to release");

        assert_eq!(
            h.resident_source_bytes(),
            0,
            "a layer nobody draws is still holding decoded grid. The only \
             other route out of the live cache is an arriving insert, and \
             arrivals stop when the layer is off — so what is left here is \
             held for the life of the process.",
        );
        assert_eq!(
            (h.cached_grids.len(), h.frame_grids.len()),
            (0, 0),
            "one of the two stores survived the release",
        );
        assert_eq!(
            h.defaults.selected_frame, parked,
            "the release moved the pane off the frame it was parked on. The \
             way back is a refetch of THAT frame; dropping the selection \
             makes it a different picture instead of a slower one.",
        );
        assert!(
            !h.release_data(),
            "an already-empty layer answered `true`. This hook runs once a \
             frame, so that is a generation bump per frame and every cache \
             keyed on it invalidated.",
        );
    }

    /// The budget is bytes and not entries: a cache holding one CONUS grid's
    /// worth is emptied by a single grid four times the size, and holds many
    /// small ones.
    ///
    /// This is the property an entry cap cannot express, so it is the one
    /// worth a test of its own.
    #[test]
    fn the_budget_counts_bytes_and_not_entries() {
        let param = ModelParameter::all()[0];
        // Big enough that `size_of::<HrrrGridData>()` — a fixed ~200 bytes
        // charged once per entry — does not blur the four-to-one ratio below.
        const POINTS: usize = 10_000;
        let small = grid_bytes(&grid(param, vec![0.0; POINTS]));
        let mut cache = ModelGridCache::with_budget(small * 4);

        for f_hour in 0..4u8 {
            cache.insert(
                GridKey {
                    param,
                    run: run_time(),
                    f_hour,
                },
                Arc::new(grid(param, vec![0.0; POINTS])),
                &[],
            );
        }
        assert_eq!(cache.len(), 4, "four small grids fit");

        // One grid of the same COUNT but four times the bytes takes the lot.
        cache.insert(
            GridKey {
                param,
                run: run_time(),
                f_hour: 9,
            },
            Arc::new(grid(param, vec![0.0; POINTS * 4])),
            &[],
        );
        assert_eq!(
            cache.len(),
            1,
            "an entry cap would have kept four; a byte budget keeps what fits",
        );
    }
    /// Eviction costs one refetch when the user returns, and the toggle is where
    /// that has to happen. The counterpart is asserted too.
    #[test]
    fn an_evicted_parameter_refetches_when_the_layer_is_toggled_back_on() {
        let mut h = full_cache();
        h.defaults.selected_param = overflow();
        deliver(&mut h, overflow()); // evicts `oldest`

        h.defaults.selected_param = oldest();
        assert!(
            !h.has_data(&PaneRef::bare(0)),
            "{:?} must be gone, or this is not the eviction case",
            oldest(),
        );
        h.defaults.enabled = false;
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

        h.defaults.selected_param = overflow();
        assert!(
            h.has_data(&PaneRef::bare(0)),
            "{:?} is resident",
            overflow()
        );
        h.defaults.enabled = false;
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

    // ── Per-pane state (WO-M10c) ──────────────────────────────────────

    /// A pane holding `param`, as the layer stack hands one over.
    fn pane_state(param: ModelParameter) -> FetchPayload {
        Box::new(ModelPaneState {
            enabled: true,
            selected_param: param,
            ..ModelPaneState::new(true)
        })
    }

    /// **Two panes, two HRRR parameters** — the order's named subject, and the
    /// thing the config swap could only fake by re-installing one pane's
    /// selection before every read.
    ///
    /// The panes are asserted **equal first**: a test that never sees them
    /// agree cannot tell divergence from two independently wrong answers. And
    /// `defaults` is asserted untouched at the end — the assertion that fires
    /// the moment any of these methods writes a per-pane value to `&mut self`.
    #[test]
    fn two_panes_hold_different_hrrr_parameters_and_the_registry_keeps_neither() {
        let left = ModelParameter::all()[0];
        let right = ModelParameter::all()[1];
        assert_ne!(left, right, "premise: two distinct parameters");

        let mut h = new_handler();
        // Both grids resident, so a difference in the answers is a difference
        // in the selection and not in what happens to be cached.
        deliver(&mut h, left);
        deliver(&mut h, right);

        let a = pane_state(left);
        let mut b = pane_state(left);
        let same_a = PaneRef {
            state: Some(&*a),
            ..PaneRef::bare(0)
        };
        let same_b = PaneRef {
            state: Some(&*b),
            ..PaneRef::bare(1)
        };
        assert_eq!(
            h.status_line(&same_a),
            h.status_line(&same_b),
            "premise: two panes on the same parameter answer the same",
        );

        // Diverge through the handler's own control route, not a field write.
        let effect = h.apply_control(
            &ControlUpdate {
                id: "parameter",
                value: ControlValue::String(right.as_str().to_owned()),
            },
            &mut PaneMut {
                pane_idx: 1,
                state: Some(&mut *b),
                peers: &[&*a],
            },
        );
        assert!(matches!(effect, ControlEffect::None), "{effect:?}");

        let pane_a = PaneRef {
            state: Some(&*a),
            ..PaneRef::bare(0)
        };
        let pane_b = PaneRef {
            state: Some(&*b),
            ..PaneRef::bare(1)
        };

        // The row is the field and the frame on the glass; both panes hold
        // the same fixture run, so only the field half may differ here.
        let row_of = |name: &str| format!("{name} - {}", run_time().format("%H:%Mz"));
        assert_eq!(
            h.status_line(&pane_a).as_deref(),
            Some(row_of(left.display_name()).as_str()),
            "pane 0's parameter",
        );
        assert_eq!(
            h.status_line(&pane_b).as_deref(),
            Some(row_of(right.display_name()).as_str()),
            "pane 1's parameter",
        );
        assert_ne!(
            h.legend(&pane_a).map(|l| l.signature),
            h.legend(&pane_b).map(|l| l.signature),
            "two parameters must not share one legend signature, or one pane \
             draws the other's colour bar",
        );
        assert_eq!(
            h.serialize_pane_state(&*a)["parameter"],
            serde_json::json!(left.as_str()),
            "pane 0's saved bytes",
        );
        assert_eq!(
            h.serialize_pane_state(&*b)["parameter"],
            serde_json::json!(right.as_str()),
            "pane 1's saved bytes",
        );
        assert_eq!(
            h.defaults.selected_param,
            ModelParameter::SurfaceBasedCin,
            "the registry's own copy took one of the panes' selections",
        );
    }

    /// **Two panes on two parameters must not share one cache token.** The
    /// render dispatch groups panes by `(layer, zoom, token, size)` and hands
    /// one raster to the whole group, so an equal token is one pane drawing
    /// the other's grid.
    #[test]
    fn two_panes_on_two_parameters_do_not_share_a_cache_token() {
        let left = ModelParameter::all()[0];
        let right = ModelParameter::all()[1];
        let h = new_handler();
        let a = pane_state(left);
        let b = pane_state(right);
        let same = pane_state(left);

        let token = |state: &FetchPayload| {
            h.content_signature(&PaneRef {
                state: Some(&**state),
                ..PaneRef::bare(0)
            })
        };
        assert_eq!(
            token(&a),
            token(&same),
            "premise: the same parameter is the same picture, so the same token",
        );
        assert_ne!(
            token(&a),
            token(&b),
            "two panes on two HRRR parameters shared one cache token",
        );
    }

    /// **The cache pin is the UNION of every pane's parameter, not one pane's.**
    ///
    /// The cache is shared: with a full cache and two panes on two parameters,
    /// an arrival has to evict something, and pinning only the pane that
    /// happens to be first takes the other pane's grid away — `prepare_job`
    /// then answers `None` and that pane is left drawing a stale texture with
    /// nothing to re-ask.
    ///
    /// Non-triviality floor: the two pinned parameters are the two **oldest**
    /// in the cache, so an unpinned run evicts one of them for certain.
    #[test]
    fn an_arrival_evicts_no_parameter_that_any_pane_is_showing() {
        let mut h = full_cache();
        let pinned_a = oldest();
        let pinned_b = next_oldest();
        assert_eq!(
            h.cached_grids.recency_params()[..2],
            [pinned_a, pinned_b],
            "premise: both pinned parameters are the next two to be evicted",
        );

        let a = pane_state(pinned_a);
        let b = pane_state(pinned_b);
        let peers: [&dyn std::any::Any; 2] = [&*a, &*b];
        h.apply_fetch_result(
            Box::new(HrrrFetchResult(Ok(grid(overflow(), vec![300.0])))),
            &PaneRef::across(&peers),
        );

        assert!(
            h.cached_grids.is_resident(key(pinned_a)),
            "pane 0's grid was evicted by another pane's arrival",
        );
        assert!(
            h.cached_grids.is_resident(key(pinned_b)),
            "pane 1's grid was evicted by another pane's arrival",
        );
        assert!(
            h.cached_grids.is_resident(key(overflow())),
            "premise: the arriving grid is resident",
        );
        assert_eq!(h.cached_grids.len(), CACHE_ENTRIES, "the cap still holds",);
    }

    /// A pane holding `param` with the layer **switched off** — the state a
    /// pane keeps across the toggle, which is the whole reason the pin set
    /// could count it.
    fn disabled_pane_state(param: ModelParameter) -> FetchPayload {
        Box::new(ModelPaneState {
            enabled: false,
            selected_param: param,
            ..ModelPaneState::new(false)
        })
    }

    /// **A pane with the layer switched off is not showing a grid**, so its
    /// key is not in the pin set.
    ///
    /// The state survives the toggle — slot, parameter and parked frame are
    /// all kept so that reopening is 1:1 — so the union this walks answered
    /// for it either way. Reading the same `enabled` field `is_enabled` reads
    /// is what makes the two answers agree.
    #[test]
    fn a_disabled_panes_key_is_not_pinned() {
        let h = full_cache();
        let on = pane_state(oldest());
        let off = disabled_pane_state(next_oldest());
        let peers: [&dyn std::any::Any; 2] = [&*on, &*off];
        let pinned = h.pinned_keys(&PaneRef::across(&peers));
        assert_eq!(
            pinned,
            vec![key(oldest())],
            "only the pane that is drawing may pin",
        );
    }

    /// **Every pane switched off pins nothing at all** — the answer that was
    /// unreachable before, and the whole of what lets the cache let go.
    ///
    /// Not a hazard: `insert` never evicts the arrival itself, so an empty pin
    /// set still installs what lands. It only stops the cache carrying grids
    /// no pane is looking at.
    #[test]
    fn every_pane_disabled_pins_nothing() {
        let h = full_cache();
        let a = disabled_pane_state(oldest());
        let b = disabled_pane_state(next_oldest());
        let peers: [&dyn std::any::Any; 2] = [&*a, &*b];
        assert!(
            h.pinned_keys(&PaneRef::across(&peers)).is_empty(),
            "a layer nobody is showing must pin nothing",
        );
    }

    /// **And the bytes actually go**: the mirror of
    /// `an_arrival_evicts_no_parameter_that_any_pane_is_showing`, with pane 1
    /// switched off.
    ///
    /// Same fixture, same arrival, one flag different — so what this measures
    /// is the flag and not the eviction order. Before the pin set read
    /// `enabled`, the switched-off pane's grid was pinned and the victim was
    /// the *third* parameter instead: a grid a pane might still want, given up
    /// for one no pane could draw.
    #[test]
    fn an_arrival_evicts_the_grid_of_a_pane_whose_layer_is_switched_off() {
        let mut h = full_cache();
        let showing = oldest();
        let switched_off = next_oldest();
        let bystander = fill_order()[2];
        assert_eq!(
            h.cached_grids.recency_params()[..3],
            [showing, switched_off, bystander],
            "premise: the switched-off pane's grid is the next victim after              the one that is showing",
        );

        let on = pane_state(showing);
        let off = disabled_pane_state(switched_off);
        let peers: [&dyn std::any::Any; 2] = [&*on, &*off];
        h.apply_fetch_result(
            Box::new(HrrrFetchResult(Ok(grid(overflow(), vec![300.0])))),
            &PaneRef::across(&peers),
        );

        assert!(
            h.cached_grids.is_resident(key(showing)),
            "the pane that IS showing must still be pinned",
        );
        assert!(
            !h.cached_grids.is_resident(key(switched_off)),
            "a switched-off pane's grid was held against the budget",
        );
        assert!(
            h.cached_grids.is_resident(key(bystander)),
            "the victim must be the switched-off pane's grid, not the next              unpinned one along",
        );
        assert!(
            h.cached_grids.is_resident(key(overflow())),
            "premise: the arriving grid is resident",
        );
        assert_eq!(h.cached_grids.len(), CACHE_ENTRIES, "the cap still holds");
    }

    // ── The frame axis (WO-M11) ───────────────────────────────────────────

    fn fetch_cfg() -> FetchConfig {
        // A `reqwest::Client` cannot be built before the process has a rustls
        // provider, and a test that builds one is otherwise green only when
        // some EARLIER test in the same binary happened to install it.
        squallar_source::tls::init();
        FetchConfig {
            client: Default::default(),
            zone_cache_dir: None,
            sources: squallar_source::origins::DataSources::default(),
            viewport: None,
            as_of: chrono::Utc::now().naive_utc(),
            depicted_span_secs: None,
            depicted_frames: Vec::new(),
        }
    }

    /// A grid at an explicit forecast hour, so the stamp arithmetic is pinned
    /// against a number `ModelParameter::min_forecast_hour` does not supply.
    fn grid_at_fh(parameter: ModelParameter, fh: u8) -> HrrrGridData {
        let mut g = grid(parameter, vec![10.0]);
        g.forecast_hour = fh;
        g
    }

    fn seeded(parameter: ModelParameter, fh: u8) -> ModelDataHandler {
        let mut h = new_handler();
        h.defaults.enabled = true;
        h.defaults.selected_param = parameter;
        h.apply_fetch_result(
            Box::new(HrrrFetchResult(Ok(grid_at_fh(parameter, fh)))),
            &PaneRef::across(&[]),
        );
        h
    }

    fn run_time() -> chrono::NaiveDateTime {
        chrono::NaiveDate::from_ymd_opt(2026, 7, 25)
            .unwrap()
            .and_hms_opt(RUN_HOUR, 0, 0)
            .unwrap()
    }

    /// **A resident grid is one frame, stamped run + forecast hour.** Two
    /// forecast hours off the same run, so `valid` is shown to be a function
    /// of the hour and not a second spelling of `run`.
    #[test]
    fn a_resident_grid_is_one_frame_stamped_at_its_run_plus_its_forecast_hour() {
        for fh in [0u8, 2, 5] {
            let h = seeded(ModelParameter::SurfaceBasedCape, fh);
            assert_eq!(
                h.frames_resident(&PaneRef::bare(0)),
                vec![FrameStamp {
                    valid: run_time() + chrono::Duration::hours(i64::from(fh)),
                    run: Some(run_time()),
                }],
                "forecast hour {fh}",
            );
        }
    }

    /// A pane whose parameter has no resident grid holds no frames — the
    /// answer is about **this pane's** selection, not the cache's contents.
    #[test]
    fn a_pane_on_a_parameter_with_no_grid_holds_no_frames() {
        let mut h = seeded(ModelParameter::SurfaceBasedCape, 2);
        h.defaults.selected_param = ModelParameter::MixedLayerCin;
        assert_eq!(
            h.frames_resident(&PaneRef::bare(0)),
            Vec::new(),
            "this pane was offered another parameter's grid as its own frame",
        );
    }

    /// **The forecast listing is the run's own hours clipped to the window,
    /// and it IS complete.**
    ///
    /// Deliberately updated at S2 2.6, not merely re-pointed. It used to
    /// assert `!complete` on the grounds that "there is no HRRR archive
    /// listing in this build" — which was true of the *analysis* axis and
    /// never true of this one: the forecast hours of a run are
    /// `min_forecast_hour..=horizon`, published with an `.idx` each, so
    /// "these are all of them" is arithmetic rather than a claim nothing
    /// checked.
    #[test]
    fn the_forecast_listing_clips_to_its_window_and_is_complete() {
        let h = seeded(ModelParameter::SurfaceBasedCape, 2);
        let run = run_time();
        // RUN_HOUR is 3 — an off-cycle run, so 18 hours and not 48.
        assert_eq!(forecast_horizon(run), 18, "premise: an off-cycle run");
        let inside = (
            run + chrono::Duration::hours(1),
            run + chrono::Duration::hours(3),
        );
        let before = (
            run - chrono::Duration::hours(4),
            run - chrono::Duration::hours(1),
        );

        let listing = h.list_frames(&fetch_cfg(), &PaneRef::bare(0), inside);
        assert_eq!(listing.range, inside, "the window is echoed back");
        assert_eq!(
            listing.frames,
            (1..=3)
                .map(|f| FrameStamp {
                    valid: run + chrono::Duration::hours(f),
                    run: Some(run),
                })
                .collect::<Vec<_>>(),
            "the forecast axis offers every hour of the run in the window, \
             not only the one hour that happens to be resident",
        );
        assert!(
            listing.complete,
            "the forecast hours of a run are a closed form; an incomplete \
             listing here makes the transport keep asking for a set nothing \
             will ever add to",
        );

        assert!(
            h.list_frames(&fetch_cfg(), &PaneRef::bare(0), before)
                .frames
                .is_empty(),
            "a frame outside the window was listed anyway, so the range is \
             decorative",
        );
    }

    /// A listing over the whole horizon is exactly the hours the archive
    /// publishes — the floor at one end, the cycle's horizon at the other.
    ///
    /// Non-vacuity: the two cycles are asserted to differ, so a horizon
    /// function that ignored its argument fails.
    #[test]
    fn the_forecast_listing_is_the_runs_own_published_hours() {
        let whole = |run: chrono::NaiveDateTime| {
            let mut h = new_handler();
            h.defaults.enabled = true;
            h.defaults.selected_param = ModelParameter::SurfaceBasedCape;
            h.defaults.selected_frame = Some((run, 0));
            h.list_frames(
                &fetch_cfg(),
                &PaneRef::bare(0),
                (run, run + chrono::Duration::days(4)),
            )
            .frames
            .len()
        };
        let cycle = chrono::NaiveDate::from_ymd_opt(2026, 8, 20)
            .unwrap()
            .and_hms_opt(12, 0, 0)
            .unwrap();
        let off_cycle = cycle + chrono::Duration::hours(1);
        assert_eq!(whole(cycle), 49, "f00..=f48 on a 12Z run");
        assert_eq!(whole(off_cycle), 19, "f00..=f18 on a 13Z run");
        assert_ne!(
            whole(cycle),
            whole(off_cycle),
            "the horizon does not read its run, so every cycle would offer \
             the same frames",
        );
    }

    /// **The horizon is the RUN's own cycle**, not the wall clock and not a
    /// constant. Measured against the live archive 2026-08-21: f00–f48 on
    /// 00/06/12/18Z, f00–f18 off-cycle.
    ///
    /// Every hour of the day is walked, so a rule that happened to be right
    /// for the four synoptic hours and wrong for one other cannot pass.
    #[test]
    fn the_forecast_horizon_is_the_runs_own_cycle() {
        let day = chrono::NaiveDate::from_ymd_opt(2026, 8, 20).unwrap();
        for hour in 0..24u32 {
            let run = day.and_hms_opt(hour, 0, 0).unwrap();
            let expected = if matches!(hour, 0 | 6 | 12 | 18) {
                48
            } else {
                18
            };
            assert_eq!(forecast_horizon(run), expected, "{hour:02}Z");
        }
    }

    /// **The forecast axis answers `complete` without a network round trip.**
    ///
    /// The task built for it is a ready future: nothing in its body reaches
    /// for the client, and it resolves on a bare `block_on` with no runtime
    /// I/O driver at all — which is the executable form of "needs no network".
    /// A task that issued a request would hang or fail here rather than
    /// answering.
    #[test]
    fn a_forecast_listing_is_complete_without_a_network_round_trip() {
        let h = seeded(ModelParameter::SurfaceBasedCape, 2);
        let run = run_time();
        let range = (run, run + chrono::Duration::hours(6));
        let task = h
            .create_frame_list_task(&fetch_cfg(), &PaneRef::bare(0), range)
            .expect("a pane that knows its run has a listing to build");
        assert_eq!(task.kind, known::MODEL_DATA);

        let payload = futures::executor::block_on(task.future);
        let result = payload
            .downcast::<FrameListingResult>()
            .expect("built through FrameListingResult::task");
        assert!(
            result.listing.complete,
            "the forecast listing must claim completeness — the set is known \
             exactly",
        );
        assert_eq!(
            result.listing.frames.len(),
            7,
            "f00..=f06 of the run, clipped to the window: {:?}",
            result.listing.frames,
        );
        let scope = result
            .scope
            .downcast::<ModelListing>()
            .expect("the scope is this layer's own");
        assert_eq!(scope.axis, ModelAxis::Forecast);
        assert_eq!(scope.run, run);
        assert!(
            scope.runs.is_empty(),
            "a forecast listing names no runs of its own; its frames are a \
             closed form of the one it was dispatched for",
        );
    }

    /// **The analysis axis is incomplete until a bucket listing lands**, and
    /// then complete only over the window that listing really covered.
    ///
    /// Three states asserted, not one: before any listing, after a listing
    /// that failed (empty, `complete: false`), and after one that answered.
    /// The middle state is the one that matters — "I found none" must not read
    /// as "none exist".
    #[test]
    fn an_analysis_listing_is_incomplete_until_the_bucket_answers() {
        let run = run_time();
        let range = (run - chrono::Duration::hours(3), run);
        let mut h = seeded(ModelParameter::SurfaceBasedCape, 0);
        h.defaults.axis = ModelAxis::Analysis;

        let before = h.list_frames(&fetch_cfg(), &PaneRef::bare(0), range);
        assert!(
            !before.complete,
            "the analysis axis claimed completeness with no listing at all",
        );
        assert!(before.frames.is_empty(), "{:?}", before.frames);

        let scope = || ModelListing {
            param: ModelParameter::SurfaceBasedCape,
            run,
            axis: ModelAxis::Analysis,
            range,
            runs: Vec::new(),
        };
        // A listing that failed: empty, and honest about why.
        h.apply_frame_listing(
            FrameListing {
                range,
                frames: Vec::new(),
                complete: false,
            },
            Box::new(scope()),
            &PaneRef::across(&[]),
        );
        let failed = h.list_frames(&fetch_cfg(), &PaneRef::bare(0), range);
        assert!(
            !failed.complete,
            "an empty listing that failed was recorded as coverage, so \
             `list_frames` now claims the window is settled and no retry ever \
             happens",
        );

        // And one that answered.
        let runs: Vec<_> = (1..=3)
            .map(|back| run - chrono::Duration::hours(back))
            .collect();
        h.apply_frame_listing(
            FrameListing {
                range,
                frames: Vec::new(),
                complete: true,
            },
            Box::new(ModelListing {
                runs: runs.clone(),
                ..scope()
            }),
            &PaneRef::across(&[]),
        );
        let answered = h.list_frames(&fetch_cfg(), &PaneRef::bare(0), range);
        assert!(answered.complete, "a covering listing landed");
        let mut expected = runs.clone();
        expected.sort_unstable();
        assert_eq!(
            answered.frames,
            expected
                .iter()
                .map(|r| FrameStamp {
                    valid: *r,
                    run: Some(*r),
                })
                .collect::<Vec<_>>(),
            "the analysis axis offers one frame per listed run",
        );
    }

    /// **A listing files under the scope it was DISPATCHED for, never the
    /// pane's current one.**
    ///
    /// The round trip is uncancellable and the pane can roll onto a new run
    /// while it is in the air; the `PaneRef` that arrives with it is a
    /// `PaneRef::across` union whose config is null by construction, so a
    /// handler that read the run back off the pane would file the old run's
    /// listing under the new run — silently, and with no symptom but a frame
    /// list that is one run wrong.
    ///
    /// Both directions are asserted: the dispatched run is covered, and the
    /// pane's current run is **not**. The second is what fails if the scope is
    /// ignored; the first is what fails if it is filed nowhere at all.
    #[test]
    fn a_listing_files_under_the_run_it_was_dispatched_for_not_the_panes_current_run() {
        let dispatched = run_time();
        let rolled = dispatched + chrono::Duration::hours(1);
        assert_ne!(dispatched, rolled, "premise: two runs");
        let range = (dispatched - chrono::Duration::hours(3), dispatched);

        let mut h = new_handler();
        h.defaults.enabled = true;
        h.defaults.selected_param = ModelParameter::SurfaceBasedCape;
        h.defaults.axis = ModelAxis::Analysis;
        // The pane has already rolled on by the time the answer lands.
        h.defaults.selected_frame = Some((rolled, 0));

        h.apply_frame_listing(
            FrameListing {
                range,
                frames: Vec::new(),
                complete: true,
            },
            Box::new(ModelListing {
                param: ModelParameter::SurfaceBasedCape,
                run: dispatched,
                axis: ModelAxis::Analysis,
                range,
                runs: vec![dispatched - chrono::Duration::hours(1)],
            }),
            &PaneRef::across(&[]),
        );

        let ask = |h: &ModelDataHandler, run: chrono::NaiveDateTime| {
            let mut probe = new_handler();
            probe.defaults = h.defaults.clone();
            probe.defaults.selected_frame = Some((run, 0));
            probe.frame_listings = h.frame_listings.clone();
            probe.covered = h.covered.clone();
            probe.list_frames(&fetch_cfg(), &PaneRef::bare(0), range)
        };

        assert!(
            ask(&h, dispatched).complete,
            "the listing was filed under neither run, so it was filed nowhere",
        );
        assert!(
            !ask(&h, rolled).complete,
            "the listing was filed under the run the PANE holds rather than \
             the run it was dispatched for — every frame it names belongs to \
             another run",
        );
        assert!(
            !ask(&h, rolled)
                .frames
                .iter()
                .any(|f| f.run == Some(dispatched - chrono::Duration::hours(1))),
            "the rolled pane was offered the dispatched run's frames",
        );
    }

    /// A stamp no listing named gets no fetch — the same answer radar gives a
    /// pane whose loop is being rebuilt while the old queue drains.
    ///
    /// Non-vacuity: the neighbouring stamp that *is* named answers `Some`, so
    /// a `fetch_frame` that refused everything fails this.
    #[test]
    fn a_stamp_no_listing_named_is_not_fetched() {
        let h = seeded(ModelParameter::SurfaceBasedCape, 2);
        let run = run_time();
        let named = FrameStamp {
            valid: run + chrono::Duration::hours(6),
            run: Some(run),
        };
        assert!(
            h.fetch_frame(&fetch_cfg(), &PaneRef::bare(0), &named)
                .is_some(),
            "premise: f06 of this pane's own run is a frame it can fetch",
        );

        for (stamp, why) in [
            (
                FrameStamp {
                    valid: run + chrono::Duration::hours(19),
                    run: Some(run),
                },
                "past the horizon of an off-cycle run",
            ),
            (
                FrameStamp {
                    valid: run + chrono::Duration::hours(2),
                    run: Some(run + chrono::Duration::hours(1)),
                },
                "off another run than the one this pane is on",
            ),
            (
                FrameStamp {
                    valid: run + chrono::Duration::hours(2),
                    run: None,
                },
                "carrying no run at all",
            ),
            (
                FrameStamp {
                    valid: run - chrono::Duration::hours(1),
                    run: Some(run),
                },
                "before its own run",
            ),
        ] {
            assert!(
                h.fetch_frame(&fetch_cfg(), &PaneRef::bare(0), &stamp)
                    .is_none(),
                "a stamp {why} was accepted for fetch",
            );
        }
    }

    /// **`f_hour = (valid - run).num_hours()`** — the inverse of
    /// [`HrrrGridData::valid_time`], and the whole of the stamp arithmetic.
    #[test]
    fn a_stamp_maps_back_to_its_parameter_run_and_forecast_hour() {
        let h = seeded(ModelParameter::SurfaceBasedCape, 0);
        let run = run_time();
        for f_hour in [0u8, 1, 6, 18] {
            let stamp = FrameStamp {
                valid: run + chrono::Duration::hours(i64::from(f_hour)),
                run: Some(run),
            };
            assert_eq!(
                h.frame_target(&h.defaults, run + chrono::Duration::hours(2), &stamp),
                Some(GridKey {
                    param: ModelParameter::SurfaceBasedCape,
                    run,
                    f_hour,
                }),
                "f{f_hour:02}",
            );
        }
    }

    /// A frame **stages** under the key its own fetch was dispatched for, and
    /// leaves the live picture alone.
    ///
    /// The store moved with the item that gave the loop its own staging area;
    /// what the test asks is unchanged — the key the payload carried, not one
    /// recomputed from the pane — and the "leaves the live picture alone" half
    /// is now literal rather than only about the generation.
    #[test]
    fn an_arriving_frame_stages_under_its_own_key() {
        let mut h = seeded(ModelParameter::SurfaceBasedCape, 0);
        let generation = h.data_generation();
        let live_before = h.cached_grids.len();
        let target = GridKey {
            param: ModelParameter::SurfaceBasedCape,
            run: run_time(),
            f_hour: 9,
        };
        assert!(
            !h.frame_grids.is_staged(target) && !h.cached_grids.is_resident(target),
            "premise: f09 is in neither store yet",
        );

        h.apply_frame(
            FrameStamp {
                valid: run_time() + chrono::Duration::hours(9),
                run: Some(run_time()),
            },
            Box::new(ModelFrameFetch {
                key: target,
                grid: Some(grid_at_fh(ModelParameter::SurfaceBasedCape, 9)),
            }),
            &PaneRef::across(&[]),
        );

        assert!(h.frame_grids.is_staged(target), "the frame is staged");
        assert!(
            !h.cached_grids.is_resident(target),
            "a loop frame landed in the LIVE cache, which is the defect the \
             staging area exists to remove: it puts the frame on the pane \
             set's byte budget and holds it there for as long as the key is \
             named",
        );
        assert_eq!(
            h.cached_grids.len(),
            live_before,
            "the live cache changed size on a frame arrival",
        );
        assert_eq!(
            h.data_generation(),
            generation,
            "a frame arrival moved the LIVE picture's generation, which is \
             what `apply_fetch_result` is for",
        );
    }

    /// A frame whose fetch failed installs nothing rather than a hole.
    #[test]
    fn a_failed_frame_installs_nothing() {
        let mut h = seeded(ModelParameter::SurfaceBasedCape, 0);
        let before = h.cached_grids.len();
        h.apply_frame(
            FrameStamp {
                valid: run_time() + chrono::Duration::hours(9),
                run: Some(run_time()),
            },
            Box::new(ModelFrameFetch {
                key: GridKey {
                    param: ModelParameter::SurfaceBasedCape,
                    run: run_time(),
                    f_hour: 9,
                },
                grid: None,
            }),
            &PaneRef::across(&[]),
        );
        assert_eq!(h.cached_grids.len(), before);
    }

    /// **The axis is a control and it persists.** Reopen is exactly 1:1.
    #[test]
    fn the_axis_survives_a_control_round_trip_and_a_reopen() {
        let mut h = new_handler();
        let mut state = h.create_pane_state(true).expect("a pane state");
        fn axis_of(state: &FetchPayload) -> ModelAxis {
            state
                .downcast_ref::<ModelPaneState>()
                .expect("this layer's own state")
                .axis
        }
        assert_eq!(axis_of(&state), ModelAxis::Forecast, "the default");

        h.apply_control(
            &ControlUpdate {
                id: "axis",
                value: ControlValue::String("analysis".into()),
            },
            &mut PaneMut {
                pane_idx: 0,
                state: Some(&mut *state),
                peers: &[],
            },
        );
        assert_eq!(axis_of(&state), ModelAxis::Analysis, "the control took");

        let json = h.serialize_pane_state(&*state);
        assert_eq!(
            json["axis"],
            serde_json::json!("analysis"),
            "the axis did not reach the file: {json}",
        );
        let back = h
            .deserialize_pane_state(json, true)
            .expect("the saved state reloads");
        assert_eq!(
            axis_of(&back),
            ModelAxis::Analysis,
            "the pane came back on the forecast axis it was not left on",
        );
    }

    /// The dropdown itself: both axes offered, the pane's own selected.
    #[test]
    fn the_axis_dropdown_offers_both_axes() {
        let mut h = new_handler();
        h.defaults.axis = ModelAxis::Analysis;
        let dropdown = controls_of(&h)
            .into_iter()
            .find_map(|item| match item {
                ControlItem::Dropdown {
                    id: "axis",
                    options,
                    selected,
                    ..
                } => Some((options, selected)),
                _ => None,
            })
            .expect("the model layer offers an axis dropdown");
        assert_eq!(
            dropdown
                .0
                .iter()
                .map(|(v, _)| v.clone())
                .collect::<Vec<_>>(),
            vec!["forecast".to_string(), "analysis".to_string()],
        );
        assert_eq!(dropdown.1, "analysis", "the pane's own axis is selected");
    }

    /// **The parked frame persists too** — the pane comes back on the run and
    /// the forecast hour it was left on, not on the live hour.
    ///
    /// Non-vacuity: the two halves are asserted individually, so a save that
    /// wrote the run and dropped the hour fails rather than reading as a
    /// round trip of a `None`.
    #[test]
    fn a_parked_frame_survives_a_reopen() {
        let h = new_handler();
        let mut state = h.create_pane_state(true).expect("a pane state");
        // The latest run, because the saved form is a RELATIVE choice: the
        // fixture run is weeks old and no offset can spell it (which is its
        // own test, `a_stale_absolute_run_does_not_survive_a_restart`).
        let before = chrono::Utc::now().naive_utc();
        let run = latest_run_at(before);
        state
            .downcast_mut::<ModelPaneState>()
            .expect("this layer's own state")
            .selected_frame = Some((run, 12));

        let json = h.serialize_pane_state(&*state);
        assert_eq!(json["run"], serde_json::json!(run_token(0)));
        assert_eq!(json["forecast_hour"], serde_json::json!(12));

        let back = h
            .deserialize_pane_state(json, true)
            .expect("the saved state reloads");
        assert_eq!(
            back.downcast_ref::<ModelPaneState>()
                .expect("this layer's own state")
                .selected_frame,
            Some((run, 12)),
            "the pane did not come back on the frame it was left on",
        );
        assert_eq!(
            latest_run_at(before),
            latest_run_at(chrono::Utc::now().naive_utc()),
            "premise: the HRRR cycle did not roll while this test ran",
        );

        // And a pane that was never parked comes back unparked rather than
        // stamped with the moment it was saved.
        let fresh = h.create_pane_state(true).expect("a pane state");
        let json = h.serialize_pane_state(&*fresh);
        assert!(json.get("run").is_none(), "{json}");
        assert_eq!(
            h.deserialize_pane_state(json, true)
                .expect("reloads")
                .downcast_ref::<ModelPaneState>()
                .expect("this layer's own state")
                .selected_frame,
            None,
        );
    }

    // ── Stage A: the run and forecast-hour controls ──────────────────────

    /// The fixture grid, at a run and forecast hour of the caller's choosing —
    /// what [`grid`] cannot do, since it always files itself at the fixture
    /// run and the parameter's floor.
    fn grid_at(parameter: ModelParameter, run: chrono::NaiveDateTime, f_hour: u8) -> HrrrGridData {
        HrrrGridData {
            ref_time: run,
            forecast_hour: f_hour,
            ..grid(parameter, vec![300.0])
        }
    }

    /// A control edit through the pane's real state, the way the inspector
    /// makes one — never a field write.
    fn pick(
        h: &mut ModelDataHandler,
        state: &mut FetchPayload,
        id: &'static str,
        value: &str,
    ) -> ControlEffect {
        h.apply_control(
            &ControlUpdate {
                id,
                value: ControlValue::String(value.to_owned()),
            },
            &mut PaneMut {
                pane_idx: 0,
                state: Some(&mut **state),
                peers: &[],
            },
        )
    }

    fn frame_of(state: &FetchPayload) -> Option<(chrono::NaiveDateTime, u8)> {
        state
            .downcast_ref::<ModelPaneState>()
            .expect("this layer's own state")
            .selected_frame
    }

    fn view_of(state: &FetchPayload) -> &ModelPaneState {
        state
            .downcast_ref::<ModelPaneState>()
            .expect("this layer's own state")
    }

    fn dropdown_of(
        h: &ModelDataHandler,
        state: &FetchPayload,
        want: &str,
    ) -> (Vec<(String, String)>, String) {
        h.controls(&PaneRef {
            state: Some(&**state),
            ..PaneRef::bare(0)
        })
        .into_iter()
        .find_map(|item| match item {
            ControlItem::Dropdown {
                id,
                options,
                selected,
                ..
            } if id == want => Some((options, selected)),
            _ => None,
        })
        .unwrap_or_else(|| panic!("the model layer offers no {want:?} dropdown"))
    }

    /// **The user's ask, end to end**: pick a run, pick a forecast hour, and
    /// the layer fetches *that* grid and rasterizes *that* grid.
    ///
    /// Every step goes through `apply_control` on the pane's own state, so
    /// nothing here is reachable by writing `selected_frame` by hand — which
    /// is exactly the gap this stage closed: the field existed and the fetch
    /// honoured it, and no control ever wrote it.
    ///
    /// **Non-vacuity floors**, in order: three different picks produce three
    /// distinct grid keys *and* three distinct fetch URLs (a control that
    /// ignored its value would collapse all three); and driving both controls
    /// back to `Latest`/floor reproduces the old `None` arm exactly.
    #[test]
    fn picking_a_run_and_an_hour_fetches_exactly_that_grid() {
        // Not composite and not windowed: one GRIB URL per frame, floor f00.
        let param = ModelParameter::SurfaceBasedCape;
        assert!(!param.is_composite(), "premise: one URL per frame");
        assert_eq!(param.min_forecast_hour(), 0, "premise: an f00 floor");

        let before = chrono::Utc::now().naive_utc();
        let latest = latest_run_at(before);
        let sources = squallar_source::origins::DataSources::default();

        let mut h = new_handler();
        let mut state = pane_state(param);
        assert_eq!(frame_of(&state), None, "premise: the pane starts unparked");

        let mut keys = Vec::new();
        let mut urls = Vec::new();
        // Each hour differs from the floor the run pick lands on, so every
        // pick below is a real change and every effect is a real fetch.
        for (token, back, hour) in [
            ("latest", 0u8, 3u8),
            ("latest-2", 2, 6),
            ("latest-5", 5, 12),
        ] {
            assert!(
                matches!(pick(&mut h, &mut state, "run", token), ControlEffect::Fetch),
                "{token}: nothing of that run is resident, so the pick must fetch",
            );
            assert!(
                matches!(
                    pick(&mut h, &mut state, "f_hour", &f_hour_token(hour)),
                    ControlEffect::Fetch
                ),
                "{token} F{hour}: that hour is not resident either",
            );

            let run = latest - chrono::Duration::hours(i64::from(back));
            assert_eq!(
                frame_of(&state),
                Some((run, hour)),
                "the pane parked on something other than what was picked",
            );

            // The dispatch's own choice, off the same function `create_fetch_tasks`
            // reads — not a re-derivation of it.
            let ((date, run_hour), f_hour) =
                fetch_frame(view_of(&state), chrono::Utc::now().naive_utc());
            assert_eq!(
                (date, run_hour, f_hour),
                (run.date(), run.hour() as u8, hour),
                "the fetch would ask for a frame nobody picked",
            );
            keys.push(
                h.key_of(view_of(&state))
                    .expect("a parked pane names its grid"),
            );
            urls.push(sources.hrrr_grib_url(&date, run_hour, f_hour));
        }

        // Floor one: three picks, three grids, three objects in the bucket.
        for (i, j) in [(0, 1), (0, 2), (1, 2)] {
            assert_ne!(keys[i], keys[j], "picks {i} and {j} share one grid key");
            assert_ne!(urls[i], urls[j], "picks {i} and {j} fetch one URL");
        }

        // The grid the last pick asked for arrives, and the raster draws it.
        let (run, hour) = (latest - chrono::Duration::hours(5), 12u8);
        h.apply_fetch_result(
            Box::new(HrrrFetchResult(Ok(grid_at(param, run, hour)))),
            &PaneRef::across(&[]),
        );
        let pane = PaneRef {
            state: Some(&*state),
            ..PaneRef::bare(0)
        };
        assert_eq!(
            h.key_of(view_of(&state)),
            Some(GridKey {
                param,
                run,
                f_hour: hour
            }),
            "the arrival is filed under a key the pane does not name",
        );
        let job = h
            .prepare_job(&rasterize_ctx(), &pane)
            .expect("the picked grid is resident, so the pane has a job");
        let drawn = match job
            .downcast_ref::<rasterize::GriddedInput>()
            .expect("the model layer describes a gridded job")
        {
            rasterize::GriddedInput::Whole(grid) => grid.clone(),
            other => panic!("the model layer carries the whole grid: {other:?}"),
        };
        assert_eq!(
            (drawn.ref_time, drawn.forecast_hour),
            (run, hour),
            "the raster names a different grid than the pane picked",
        );

        // Floor two: back to `Latest` at the floor is the old `None` arm,
        // which is what every build before these controls did.
        assert!(matches!(
            pick(&mut h, &mut state, "run", ""),
            ControlEffect::Fetch | ControlEffect::None
        ));
        assert_eq!(
            frame_of(&state),
            None,
            "`Latest` must unpark the pane, not pin it to this instant",
        );
        let now = chrono::Utc::now().naive_utc();
        assert_eq!(
            fetch_frame(view_of(&state), now),
            (crate::hrrr::fetch::run_for(now), param.min_forecast_hour()),
            "an unparked pane must fetch exactly what the old `None` arm did",
        );
        assert_eq!(
            latest_run_at(before),
            latest_run_at(chrono::Utc::now().naive_utc()),
            "premise: the HRRR cycle did not roll while this test ran",
        );
    }

    /// **The pane itself says which forecast hour is on the glass** — the
    /// stack row over the map, not a line behind the options panel.
    ///
    /// **Non-vacuity floor**: the phrase is built from the **resident** grid,
    /// never from the dropdown. A pane parked on f12 with only f06 resident is
    /// drawing neither, and must claim neither — the mutation this kills is
    /// "read the label off `selected_frame`", which would have the row promise
    /// F12 with nothing behind it.
    #[test]
    fn the_pane_states_the_forecast_hour_it_is_drawing() {
        let param = ModelParameter::SurfaceBasedCape;
        let run = latest_run_at(chrono::Utc::now().naive_utc());
        let mut h = new_handler();
        let mut state = pane_state(param);

        pick(&mut h, &mut state, "run", "latest");
        pick(&mut h, &mut state, "f_hour", &f_hour_token(6));
        h.apply_fetch_result(
            Box::new(HrrrFetchResult(Ok(grid_at(param, run, 6)))),
            &PaneRef::across(&[]),
        );
        fn row(h: &ModelDataHandler, state: &FetchPayload) -> String {
            h.status_line(&PaneRef {
                state: Some(&**state),
                ..PaneRef::bare(0)
            })
            .expect("an enabled model layer has a stack row")
        }

        let line = row(&h, &state);
        assert!(
            line.contains("F06"),
            "the pane does not say its hour: {line}"
        );
        assert!(
            line.contains(
                &(run + chrono::Duration::hours(6))
                    .format("%H:%Mz")
                    .to_string()
            ),
            "the pane does not say its valid time: {line}",
        );
        assert!(
            line.contains(param.display_name()),
            "the pane stopped saying its field: {line}",
        );

        // Floor: the pick moves to f12 and nothing of f12 is resident.
        pick(&mut h, &mut state, "f_hour", &f_hour_token(12));
        assert_eq!(
            frame_of(&state),
            Some((run, 12)),
            "premise: the pick landed"
        );
        let line = row(&h, &state);
        assert!(
            !line.contains("F12"),
            "the row is reading the dropdown, not the glass: {line}",
        );
        assert!(
            !line.contains("F06"),
            "the row is naming a grid this pane is no longer showing: {line}",
        );
        assert_eq!(
            line,
            param.display_name(),
            "with nothing resident the row is the field and nothing else",
        );

        // And it moves when the resident grid does, so the F06 above was not
        // a constant.
        h.apply_fetch_result(
            Box::new(HrrrFetchResult(Ok(grid_at(param, run, 12)))),
            &PaneRef::across(&[]),
        );
        assert!(row(&h, &state).contains("F12"), "{}", row(&h, &state));
    }

    /// **A run saved as an instant does not come back.**
    ///
    /// The saved form is a relative choice, so an absolute instant — what
    /// every build before this one wrote — resolves to nothing and the pane
    /// reopens on `Latest`. Closing on Friday with 18Z picked must not reopen
    /// on Monday showing a three-day-old forecast whose only clue is a small
    /// label.
    ///
    /// **Non-vacuity floor**: a *fresh* run survives the identical round trip
    /// unchanged, so "always reset" does not pass; and a run too far back to
    /// be spelled relatively drops **both** halves rather than half a frame.
    #[test]
    fn a_stale_absolute_run_does_not_survive_a_restart() {
        let h = new_handler();
        let param = ModelParameter::SurfaceBasedCape;
        let saved = |run: serde_json::Value| {
            serde_json::json!({
                "enabled": true,
                "parameter": param.as_str(),
                "axis": "forecast",
                "run": run,
                "forecast_hour": 6,
            })
        };
        let restored = |value: serde_json::Value| {
            frame_of(
                &h.deserialize_pane_state(value, true)
                    .expect("the saved state reloads"),
            )
        };

        assert_eq!(
            restored(saved(serde_json::json!("2026-07-25T03:00:00"))),
            None,
            "an absolute instant left by an older build must not be restored",
        );

        // Floor: the relative spelling of the same shape DOES come back.
        let before = chrono::Utc::now().naive_utc();
        assert_eq!(
            restored(saved(serde_json::json!("latest-2"))),
            Some((latest_run_at(before) - chrono::Duration::hours(2), 6)),
            "a fresh relative choice must survive, or `None` above is just \
             'always reset'",
        );

        // And the encoding itself, at a fixed clock: a run that has aged past
        // the vocabulary cannot be written, so both halves leave the file.
        let now = chrono::NaiveDate::from_ymd_opt(2026, 8, 21)
            .unwrap()
            .and_hms_opt(14, 30, 0)
            .unwrap();
        let latest = latest_run_at(now);
        assert_eq!(latest.format("%H:%M").to_string(), "12:00", "premise");
        assert_eq!(encode_run_choice(latest, now).as_deref(), Some("latest"));
        assert_eq!(
            encode_run_choice(latest - chrono::Duration::hours(3), now).as_deref(),
            Some("latest-3"),
        );
        assert_eq!(
            encode_run_choice(latest - chrono::Duration::days(3), now),
            None,
            "72 hours is past MAX_RUN_OFFSET and has no relative spelling",
        );
        assert_eq!(
            decode_run_choice("latest-3", now),
            Some(latest - chrono::Duration::hours(3))
        );
        assert_eq!(decode_run_choice("", now), None, "`Latest` is not a run");
        assert_eq!(
            decode_run_choice("2026-08-21T12:00:00", now),
            None,
            "an instant is not a choice",
        );

        let mut state = h.create_pane_state(true).expect("a pane state");
        state
            .downcast_mut::<ModelPaneState>()
            .expect("this layer's own state")
            .selected_frame = Some((before - chrono::Duration::days(3), 6));
        let json = h.serialize_pane_state(&*state);
        assert!(json.get("run").is_none(), "{json}");
        assert!(
            json.get("forecast_hour").is_none(),
            "both halves or neither: {json}",
        );
        assert_eq!(
            latest_run_at(before),
            latest_run_at(chrono::Utc::now().naive_utc()),
            "premise: the HRRR cycle did not roll while this test ran",
        );
    }

    /// The run menu: `Latest`, then the latest run and twelve before it, each
    /// stating its own reach.
    #[test]
    fn the_run_menu_offers_latest_and_the_runs_behind_it() {
        let h = new_handler();
        let state = pane_state(ModelParameter::SurfaceBasedCape);
        let (options, selected) = dropdown_of(&h, &state, "run");
        assert_eq!(
            options.len(),
            usize::from(RUN_CHOICES) + 2,
            "`Latest` plus {RUN_CHOICES} + 1 runs: {options:?}",
        );
        assert_eq!(options[0], (String::new(), "Latest".to_string()));
        assert_eq!(options[1].0, "latest");
        assert_eq!(options[2].0, "latest-1");
        assert_eq!(selected, "", "an unparked pane sits on Latest");
        // Every label states the run's reach, which is the run's own and not
        // the layer's.
        let reaches: Vec<&str> = options[1..]
            .iter()
            .map(|(_, label)| {
                if label.contains("f00-f48") {
                    "48"
                } else {
                    "18"
                }
            })
            .collect();
        assert!(
            reaches.contains(&"48") && reaches.contains(&"18"),
            "thirteen consecutive runs span both cycles: {options:?}",
        );
    }

    /// The forecast-hour list is the run's whole horizon — 49 entries on a
    /// synoptic run, 19 off-cycle — and it is rebuilt when the run changes.
    ///
    /// **Non-vacuity floor**: the two counts are asserted against runs picked
    /// through the control, so a list that ignored the run would fail one.
    #[test]
    fn a_forecast_hour_list_spans_the_runs_own_horizon() {
        let mut h = new_handler();
        let mut state = pane_state(ModelParameter::SurfaceBasedCape);
        let latest = latest_run_at(chrono::Utc::now().naive_utc());

        let mut seen: Vec<usize> = Vec::new();
        for back in 0..6u8 {
            pick(&mut h, &mut state, "run", &run_token(back));
            let run = latest - chrono::Duration::hours(i64::from(back));
            let (options, _) = dropdown_of(&h, &state, "f_hour");
            assert_eq!(
                options.len(),
                usize::from(forecast_horizon(run)) + 1,
                "the list must be the run's horizon, run {run}: {}",
                options.len(),
            );
            seen.push(options.len());
            assert_eq!(options[0].0, "f00");
            assert!(options[0].1.starts_with("F00 ("), "{:?}", options[0]);
        }
        assert!(
            seen.contains(&49) && seen.contains(&19),
            "six consecutive runs contain a synoptic one and an off-cycle \
             one, so both lengths must appear: {seen:?}",
        );
    }

    /// The two `MXUPHL` maxima publish an identically zero f00 over a
    /// zero-length window, so their floor is f01. The control clamps **up**
    /// to it and never down.
    #[test]
    fn a_forecast_hour_never_falls_below_the_parameters_floor() {
        let param = ModelParameter::MaxUH2to5km;
        assert_eq!(param.min_forecast_hour(), 1, "premise");
        let mut h = new_handler();
        let mut state = pane_state(param);

        let (options, selected) = dropdown_of(&h, &state, "f_hour");
        assert_eq!(
            options[0].0, "f01",
            "the list starts at the floor: {options:?}"
        );
        assert_eq!(selected, "f01", "an unparked pane already draws f01");

        pick(&mut h, &mut state, "run", "latest");
        pick(&mut h, &mut state, "f_hour", &f_hour_token(0));
        assert_eq!(
            frame_of(&state).map(|(_, f_hour)| f_hour),
            Some(1),
            "f00 must be raised to the floor, not accepted",
        );
        pick(&mut h, &mut state, "f_hour", &f_hour_token(18));
        assert_eq!(
            frame_of(&state).map(|(_, f_hour)| f_hour),
            Some(18),
            "the clamp only ever raises: f18 must stay f18",
        );

        // And a floor parameter never fetches below its floor either.
        assert_eq!(
            fetch_frame(view_of(&state), chrono::Utc::now().naive_utc()).1,
            18
        );
    }

    /// An hour picked against a 48-hour run comes back onto whatever run the
    /// offset now names, which may only reach 18 — so the restored hour is
    /// clamped down to a frame that exists rather than to a 404.
    #[test]
    fn a_restored_hour_cannot_outrun_the_run_it_lands_on() {
        let h = new_handler();
        let param = ModelParameter::SurfaceBasedCape;
        let now = chrono::Utc::now().naive_utc();
        let mut off_cycle = 0u8;
        while forecast_horizon(latest_run_at(now) - chrono::Duration::hours(i64::from(off_cycle)))
            != 18
        {
            off_cycle += 1;
            assert!(off_cycle < 6, "one of six consecutive runs is off-cycle");
        }
        let restored = h
            .deserialize_pane_state(
                serde_json::json!({
                    "enabled": true,
                    "parameter": param.as_str(),
                    "axis": "forecast",
                    "run": run_token(off_cycle),
                    "forecast_hour": 36,
                }),
                true,
            )
            .expect("reloads");
        assert_eq!(
            frame_of(&restored).map(|(_, f_hour)| f_hour),
            Some(18),
            "f36 does not exist on an 18-hour run",
        );
        // Floor: an hour the run does carry is restored untouched.
        let restored = h
            .deserialize_pane_state(
                serde_json::json!({
                    "enabled": true,
                    "parameter": param.as_str(),
                    "axis": "forecast",
                    "run": run_token(off_cycle),
                    "forecast_hour": 12,
                }),
                true,
            )
            .expect("reloads");
        assert_eq!(frame_of(&restored).map(|(_, f_hour)| f_hour), Some(12));
    }

    /// The axis itself: hourly cycles that run **ahead** of the wall clock.
    #[test]
    fn the_model_layer_declares_an_hourly_forecast_axis() {
        assert_eq!(
            new_handler().time_axis(),
            TimeAxis::FrameSeries {
                typical_step: std::time::Duration::from_secs(3600),
                extends_future: true,
            },
            "HRRR runs hourly and its grids are valid AHEAD of the clock; a \
             timeline reading this would offer the wrong step or refuse the \
             future half of its own range",
        );
    }

    /// **A pane parked in the past resolves its run from the instant it
    /// depicts, not from the wall clock.**
    ///
    /// The rig leg that found this parked six panes at 2026-04-27T06:00Z
    /// through the shipped scrub path and watched HRRR fetch
    /// `hrrr.20260910/...t13z` — that day's run, on panes parked in April.
    /// Two defects in one: `run_of` answers `None` for a pane that has neither
    /// a selection nor a resident grid, so `scope_of` is `None`, so **no
    /// listing is ever dispatched** and the pane has no frame series at all;
    /// and with `selected_frame` still `None`, `fetch_frame` falls back to
    /// `latest_available_run()`, which is `run_for(Utc::now())`.
    ///
    /// Both halves are asserted here against a fixed `as_of` with an empty
    /// cache and no selection — the exact state a freshly scrubbed pane is in.
    #[test]
    fn a_parked_pane_resolves_its_run_from_the_instant_it_depicts() {
        let h = new_handler();
        let as_of = at(2026, 4, 27, 6);
        let ctx = FetchConfig {
            as_of,
            ..fetch_cfg()
        };
        let pane = PaneRef::bare(0);
        let range = (
            as_of - chrono::Duration::hours(6),
            as_of + chrono::Duration::hours(6),
        );

        // The listing is dispatched at all. `None` here is the root: a pane
        // with no frame series has an empty scrubber, and never acquires the
        // `selected_frame` that would keep the fetch off the wall clock.
        assert!(
            h.create_frame_list_task(&ctx, &pane, range).is_some(),
            "a pane parked at {as_of} dispatched no frame listing, so it has no \
             frame series to scrub and nothing will ever write its selection",
        );

        let listing = h.list_frames(&ctx, &pane, range);
        assert!(
            !listing.frames.is_empty(),
            "a pane parked at {as_of} offers no frames over {range:?}",
        );

        // Every frame belongs to a run inside the depicted window. The bound
        // is the window itself, not a magic number: `run_for` is two hours
        // behind the instant it is given, and the clip is +/- 6 h.
        for stamp in &listing.frames {
            let run = stamp.run.expect("a model frame names the run it is off");
            assert!(
                (run - as_of).num_hours().abs() <= 6,
                "frame valid {} is off run {run}, which is not in the window \
                 around the depicted instant {as_of}",
                stamp.valid,
            );
        }

        // And the fetch asks for that run rather than today's. Stated against
        // the wall clock directly, because "today's run" is precisely what the
        // rig saw on the glass.
        let now = chrono::Utc::now().naive_utc();
        let ((date, hour), _f_hour) = fetch_frame(view_of_default(&h), as_of);
        assert_eq!(
            date,
            as_of.date(),
            "the fetch asked for the {date} run on a pane parked at {as_of}; \
             the wall clock reads {now}",
        );
        assert!(
            hour <= 6,
            "the fetch asked for the {hour:02}Z run on a pane parked at {as_of}",
        );
    }

    /// [`ModelDataHandler::view`] for a handler whose panes carry no state —
    /// the registry copy, which is what [`PaneRef::bare`] resolves to.
    fn view_of_default(h: &ModelDataHandler) -> &ModelPaneState {
        h.view(&PaneRef::bare(0))
    }
}
