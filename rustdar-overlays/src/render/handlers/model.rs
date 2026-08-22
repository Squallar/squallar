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
use rustdar_source::id::{LayerId, known};
use rustdar_source::job::{DescribedJob, JobCodec};
use rustdar_source::time::{FrameListing, FrameStamp, TimeAxis};

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
/// `values` is the whole figure in practice — 1,905,141 `f32` = **7,620,564
/// bytes** on the CONUS grid — but the `Explicit` coordinate arm is counted
/// too rather than assumed away: it is 30.5 MB at that size, four times the
/// values, and it is what a non-Lambert HRRR-shaped source would arrive on.
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
    std::mem::size_of::<HrrrGridData>() + grid.values.len() * std::mem::size_of::<f32>() + coords
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
/// | target | budget | grids | pane cap |
/// |---|---|---|---|
/// | wasm32 | 96 MiB | 13 | 6 |
/// | mobile | 192 MiB | 26 | 4 |
/// | desktop | 512 MiB | 70 | 6 |
///
/// **Never below the pane count.** Below it a pane loses its grid to another
/// pane's arrival, and it fails *silently*: `prepare_job` answers `None` and
/// the pane goes on drawing its last texture with nothing that will re-ask.
/// `the_byte_budget_holds_at_least_one_grid_per_pane` holds that floor on all
/// three arms from a host test, which is why each is a named constant rather
/// than only a `cfg` arm.
///
/// **Spelled here rather than read from `rustdar-device-profile`**, where the
/// rest of this application's budgets live. That crate declares
/// `rustdar-radar`, so an overlays → device-profile edge puts the whole radar
/// pipeline back into every overlay handler's compile graph — the edge
/// `rustdar-source/tests/charter.rs::the_overlays_to_radar_edge_stays_cut`
/// exists to keep cut, and which today leaves this crate standing on exactly
/// `{rustdar-geo, rustdar-source, rustdar-units}`. It is also why the pane cap
/// below is spelled and not imported from `rustdar-egui`.
pub const WASM_MODEL_GRID_BUDGET_BYTES: usize = 96 * 1024 * 1024;
pub const MOBILE_MODEL_GRID_BUDGET_BYTES: usize = 192 * 1024 * 1024;
pub const DESKTOP_MODEL_GRID_BUDGET_BYTES: usize = 512 * 1024 * 1024;

/// The pane cap the budget is measured against. Spelled, for the reason above;
/// `rustdar_device_profile::budget::MAX_PANES_DESKTOP` = 6 and
/// `MAX_PANES_MOBILE` = 4 are the definitions.
pub const MAX_PANES_DESKTOP: usize = 6;
pub const MAX_PANES_MOBILE: usize = 4;

/// What one CONUS grid costs, as [`grid_bytes`] counts it: 1799 × 1059 =
/// 1,905,141 `f32`, on a `Lambert` coordinate arm that adds nothing. Measured
/// 2026-08-21; the opening assertion of
/// `the_byte_budget_holds_at_least_one_grid_per_pane` is what keeps it from
/// drifting away from the function.
pub const HRRR_CONUS_GRID_BYTES: usize = 1799 * 1059 * 4;

/// **The floor, as a build failure.** A budget below one grid per pane starves
/// a pane *silently* — `prepare_job` answers `None` and it goes on drawing its
/// last texture — so it is caught here rather than in a test somebody has to
/// have run on the right target.
const _: () = {
    assert!(WASM_MODEL_GRID_BUDGET_BYTES / HRRR_CONUS_GRID_BYTES >= MAX_PANES_DESKTOP);
    assert!(MOBILE_MODEL_GRID_BUDGET_BYTES / HRRR_CONUS_GRID_BYTES >= MAX_PANES_MOBILE);
    assert!(DESKTOP_MODEL_GRID_BUDGET_BYTES / HRRR_CONUS_GRID_BYTES >= MAX_PANES_DESKTOP);
};

// The device class, spelled as its rule rather than as the `mobile` cfg:
// cargo scopes a build script's cfgs to the crate that declares it, so
// `cfg(mobile)` is unset everywhere outside `rustdar-device-profile`, and a
// `#[cfg(mobile)]` written here would be silently false on a handheld. The
// rule is `rustdar-device-profile/src/mobile_cfg.rs::is_mobile_target` —
// `target_os` in {android, ios}. Selecting a value, never forking behaviour.
#[cfg(target_arch = "wasm32")]
const MODEL_GRID_BUDGET_BYTES: usize = WASM_MODEL_GRID_BUDGET_BYTES;
#[cfg(all(
    not(target_arch = "wasm32"),
    any(target_os = "android", target_os = "ios")
))]
const MODEL_GRID_BUDGET_BYTES: usize = MOBILE_MODEL_GRID_BUDGET_BYTES;
#[cfg(all(
    not(target_arch = "wasm32"),
    not(any(target_os = "android", target_os = "ios"))
))]
const MODEL_GRID_BUDGET_BYTES: usize = DESKTOP_MODEL_GRID_BUDGET_BYTES;

/// The resident grids, bounded by bytes and evicted least-recently-touched
/// first.
///
/// An entries map plus a recency list holding exactly the keys of `entries`,
/// oldest use first; both private so no caller can desynchronise them. The list
/// is behind a `RefCell` because every *reader* reaches it through an `&self`
/// method of [`OverlayHandler`], and a lookup that did not count as a use would
/// let the pane on screen age out.
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

    /// Neither the entry going in nor anything in `pinned` is ever evicted.
    ///
    /// `pinned` is the **union** of every pane's current key, not one pane's:
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

    #[cfg(test)]
    fn is_resident(&self, key: GridKey) -> bool {
        self.entries.contains_key(&key)
    }

    /// The eviction order, projected to parameters: the suite that asks about
    /// it predates the run and the forecast hour being in the key, and is
    /// asking about *order* rather than about identity.
    #[cfg(test)]
    fn recency_params(&self) -> Vec<ModelParameter> {
        self.recency.borrow().iter().map(|k| k.param).collect()
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

/// **A relative choice as a run**, against the clock that is reading it.
///
/// `None` for `Latest`, for a token this build does not spell, and — the
/// point of the whole encoding — for an **absolute** instant, which is what
/// the build before this one saved. A config closed on Friday with 18Z picked
/// would otherwise reopen on Monday three days into the past, with nothing but
/// a small label to say so. It reopens on `Latest` instead.
fn decode_run_choice(text: &str, now: chrono::NaiveDateTime) -> Option<chrono::NaiveDateTime> {
    let back: u8 = match text.strip_prefix(RUN_STEM)? {
        "" => 0,
        rest => rest.strip_prefix('-')?.parse().ok()?,
    };
    (back <= MAX_RUN_OFFSET).then(|| latest_run_at(now) - chrono::Duration::hours(i64::from(back)))
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
/// reopen 1:1 rather than snapping the pane back to the live hour. An unparked
/// pane asks for the latest run at the parameter's own floor, the behaviour
/// the fetch used to hardcode.
fn fetch_frame(view: &ModelPaneState) -> ((chrono::NaiveDate, u8), u8) {
    match view.selected_frame {
        Some((run, f_hour)) => ((run.date(), run.hour() as u8), f_hour),
        None => (
            crate::hrrr::fetch::latest_available_run(),
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

/// One loop frame's grid on its way back from [`OverlayHandler::fetch_frame`].
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
            state: OverlayState::new(),
            defaults: ModelPaneState::new(false),
            cached_grids: ModelGridCache::new(),
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
    fn pinned_keys(&self, pane: &PaneRef<'_>) -> Vec<GridKey> {
        let mut pinned: Vec<GridKey> = Vec::new();
        let mut any_pane = false;
        for state in pane.all_as::<ModelPaneState>() {
            any_pane = true;
            if let Some(key) = self.key_of(state)
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
    /// reads the wall clock, and `list_frames` is a synchronous read every
    /// frame may make — a clock in it would walk the whole frame list forward
    /// under a parked pane. "I do not know yet" is the honest answer, and the
    /// first fetch settles it.
    fn run_of(&self, view: &ModelPaneState) -> Option<chrono::NaiveDateTime> {
        view.selected_frame.map(|(run, _)| run).or_else(|| {
            self.cached_grids
                .latest_of(view.selected_param)
                .map(|k| k.run)
        })
    }

    fn scope_of(&self, view: &ModelPaneState) -> Option<ModelScope> {
        Some(ModelScope {
            param: view.selected_param,
            run: self.run_of(view)?,
            axis: view.axis,
        })
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
    fn frame_target(&self, view: &ModelPaneState, stamp: &FrameStamp) -> Option<GridKey> {
        let run = stamp.run?;
        let hours = (stamp.valid - run).num_hours();
        let f_hour = u8::try_from(hours).ok()?;
        if f_hour < view.selected_param.min_forecast_hour() {
            return None;
        }
        let scope = self.scope_of(view)?;
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

impl OverlayHandler for ModelDataHandler {
    /// The sixteen HRRR parameters this layer offers, projected into the
    /// substrate's read contract by [`crate::hrrr::fields`].
    fn products(&self) -> &'static [rustdar_source::product::ProductSpec] {
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
    fn current_field(&self, pane: &PaneRef<'_>) -> Option<rustdar_source::product::FieldId> {
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

    /// HRRR is a run-based forecast: hourly cycles, each carrying grids valid
    /// at the run time plus a forecast hour — discrete stamped frames, and the
    /// stamps run **ahead** of the wall clock.
    fn time_axis(&self) -> TimeAxis {
        TimeAxis::FrameSeries {
            typical_step: std::time::Duration::from_secs(3600),
            extends_future: true,
        }
    }

    /// **The frames this pane is holding**: every resident grid of its own
    /// parameter that sits on its own axis — one run's forecast hours, or the
    /// analysis hour of many runs. `valid` is the grid's valid time and `run`
    /// its reference time.
    ///
    /// This pane's scope and not the cache's whole contents: the other
    /// resident grids belong to other panes' parameters and runs, and pooling
    /// them here would offer this pane frames it cannot draw.
    fn frames_resident(&self, pane: &PaneRef<'_>) -> Vec<FrameStamp> {
        let view = self.view(pane);
        let Some(scope) = self.scope_of(view) else {
            return Vec::new();
        };
        let mut frames: Vec<FrameStamp> = self
            .cached_grids
            .entries
            .iter()
            .filter(|(key, _)| {
                key.param == scope.param
                    && match scope.axis {
                        ModelAxis::Forecast => key.run == scope.run,
                        ModelAxis::Analysis => key.f_hour == scope.param.min_forecast_hour(),
                    }
            })
            .map(|(_, grid)| FrameStamp {
                valid: grid.valid_time(),
                run: Some(grid.ref_time),
            })
            .collect();
        frames.sort_by_key(|stamp| stamp.valid);
        frames
    }

    /// **What frames exist over `range`, per axis.**
    ///
    /// `Forecast` is a closed form of the run — `min_forecast_hour` to the
    /// cycle's horizon, every hour published with an `.idx` — so it is
    /// `complete` on arithmetic alone and never needs a round trip to say so.
    /// `Analysis` is a bucket listing, and is `complete` only where one has
    /// landed covering the whole window: "I found none" is not "none exist".
    ///
    /// Both answer empty before anything has named this pane's run. A synchronous
    /// read must not reach for the wall clock — see [`Self::run_of`].
    fn list_frames(
        &self,
        _ctx: &FetchConfig,
        pane: &PaneRef<'_>,
        range: (chrono::NaiveDateTime, chrono::NaiveDateTime),
    ) -> FrameListing {
        let view = self.view(pane);
        let Some(scope) = self.scope_of(view) else {
            return FrameListing::empty(range);
        };
        let (mut frames, complete) = match scope.axis {
            ModelAxis::Forecast => (Self::forecast_stamps(scope.param, scope.run), true),
            ModelAxis::Analysis => (
                self.frame_listings
                    .get(&scope)
                    .map(|runs| {
                        runs.iter()
                            .map(|run| FrameStamp {
                                valid: *run + analysis_offset(scope.param),
                                run: Some(*run),
                            })
                            .collect()
                    })
                    .unwrap_or_default(),
                self.covers(&scope, range),
            ),
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
        let scope = self.scope_of(view)?;
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
        let key = self.frame_target(view, stamp)?;
        let client = ctx.client.clone();
        let sources = ctx.sources.clone();
        let GridKey { param, run, f_hour } = key;
        let run_pair = (run.date(), run.hour() as u8);
        Some(FetchTask {
            kind: known::MODEL_DATA,
            future: Box::pin(async move {
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

    /// Install one frame's grid under the key its fetch was dispatched for.
    ///
    /// The key comes back on the payload rather than being recomputed from
    /// `stamp` and the pane, for the same reason the listing's scope does.
    fn apply_frame(&mut self, _stamp: FrameStamp, data: FetchPayload, pane: &PaneRef<'_>) {
        let Ok(frame) = data.downcast::<ModelFrameFetch>() else {
            log::error!("a frame reached the model layer under another layer's payload");
            return;
        };
        let Some(grid) = frame.grid else {
            return;
        };
        let pinned = self.pinned_keys(pane);
        self.cached_grids.insert(frame.key, Arc::new(grid), &pinned);
    }

    /// A no-op: this layer's residency is the grid cache's business
    /// ([`MODEL_GRID_BUDGET_BYTES`], evicting by use under a byte budget), and
    /// a second eviction authority would fight it.
    fn retain_frames(&mut self, _pane: &PaneRef<'_>, _keep: &[FrameStamp]) {}

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
    fn prepare_job(&self, _ctx: &RasterizeContext, pane: &PaneRef<'_>) -> Option<DescribedJob> {
        let grid = self.grid_of(pane)?.clone();
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
        let (run, f_hour) = fetch_frame(view);
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustdar_geo::GeoBounds;

    const RUN_HOUR: u32 = 3;

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

    /// A handler whose cache holds exactly [`CACHE_ENTRIES`] fixture grids.
    fn new_handler() -> ModelDataHandler {
        let mut h = ModelDataHandler::new();
        h.cached_grids = ModelGridCache::with_budget(test_budget());
        h
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

    /// A full desktop layout is six unlinked panes, each free to select its own
    /// parameter. This is the case a cap below the pane count breaks, and it
    /// breaks *silently*: `prepare_job` answers `None` and the starved pane goes
    /// on drawing its last texture, with nothing to re-ask.
    #[test]
    fn every_pane_of_a_full_desktop_layout_keeps_a_drawable_grid() {
        // Spelled, not imported: rustdar-overlays cannot depend on rustdar-egui.
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
                 {panes} panes. Below one grid per pane a pane loses its grid \
                 to another pane's arrival and there is no symptom: \
                 `prepare_job` answers None and it keeps drawing the last \
                 texture.",
            );
        }
        assert_eq!(
            (
                WASM_MODEL_GRID_BUDGET_BYTES / HRRR_CONUS_GRID_BYTES,
                MOBILE_MODEL_GRID_BUDGET_BYTES / HRRR_CONUS_GRID_BYTES,
                DESKTOP_MODEL_GRID_BUDGET_BYTES / HRRR_CONUS_GRID_BYTES,
            ),
            (13, 26, 70),
            "the figures the module doc states",
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

    // ── The frame axis (WO-M11) ───────────────────────────────────────────

    fn fetch_cfg() -> FetchConfig {
        // A `reqwest::Client` cannot be built before the process has a rustls
        // provider, and a test that builds one is otherwise green only when
        // some EARLIER test in the same binary happened to install it.
        rustdar_source::tls::init();
        FetchConfig {
            client: Default::default(),
            zone_cache_dir: None,
            sources: rustdar_source::origins::DataSources::default(),
            viewport: None,
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
                h.frame_target(&h.defaults, &stamp),
                Some(GridKey {
                    param: ModelParameter::SurfaceBasedCape,
                    run,
                    f_hour,
                }),
                "f{f_hour:02}",
            );
        }
    }

    /// A frame installs under the key its own fetch was dispatched for, and
    /// leaves the live picture alone.
    #[test]
    fn an_arriving_frame_installs_under_its_own_key() {
        let mut h = seeded(ModelParameter::SurfaceBasedCape, 0);
        let generation = h.data_generation();
        let target = GridKey {
            param: ModelParameter::SurfaceBasedCape,
            run: run_time(),
            f_hour: 9,
        };
        assert!(
            !h.cached_grids.is_resident(target),
            "premise: f09 is not resident yet",
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

        assert!(h.cached_grids.is_resident(target), "the frame is resident");
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
        let sources = rustdar_source::origins::DataSources::default();

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
            let ((date, run_hour), f_hour) = fetch_frame(view_of(&state));
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
        assert_eq!(
            fetch_frame(view_of(&state)),
            (
                crate::hrrr::fetch::latest_available_run(),
                param.min_forecast_hour()
            ),
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
        assert_eq!(fetch_frame(view_of(&state)).1, 18);
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
}
