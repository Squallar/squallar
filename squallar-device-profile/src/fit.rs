//! The one function: what a scene costs at a given [`Budgets`], and the largest
//! [`Budgets`] whose cost fits a [`Capacity`].
//!
//! [`need`] sums terms the tree already prices — `Budgets::loop_frame_bytes`,
//! `Budgets::section_frame_bytes`, `Budgets::static_frame_bytes`, the raymarch's
//! own resident-grid arithmetic handed in as [`GridBytes`],
//! `quality::VolumeQuality::fit` for the offscreen, `quality::offscreen_bytes`
//! for the mirror, the tile cache's measured entry cost,
//! `Budgets::prism_vram_bytes` for a pane that draws buildings, each gridded
//! overlay's own source budget handed in on the scene, and for the decoded
//! volume behind each radar loop frame its measured size where the cache
//! holds it and `LOOP_SCAN_RESERVE_BYTES` where it does not. Nothing here
//! invents a byte figure: the one constant of this module's own is a measured
//! maximum rounded up, charged only for what has not arrived, and says so.
//!
//! [`need_terms_for_pane`] prices one pane; [`need_terms`] is that over the
//! panes plus the scene-level terms, and the two agree bit for bit by
//! construction (`fit/tests.rs` folds them back).
//!
//! [`fit`] is the degradation ladder driven by arithmetic instead of a counter:
//! start at the rung the class earns, price the scene, and while the price is
//! over the allowance take the next rung of `budget::demote`'s own table. No
//! rung counter is remembered and nothing is persisted; the same scene against
//! the same capacity fits to the same budgets on every start.
//!
//! The capacity has two live arms and `fit` does not know which it is on. On
//! the **measured** arm (`budget::DeviceProfile::capacity`, where the
//! profile's readings amount to a measurement) the allowance is
//! `NEED_FRACTION` of the card and no bracket constant binds the pool: a 3090
//! holds what six two-hour loops cost and a 4 GiB card shortens them. On the
//! **presumed** arm the bracket's constant is the capacity and the allowance,
//! exactly as before a reader existed. [`fit_holds`] is the one invariant both
//! arms promise, checked where the application adopts an answer.

use crate::budget::{
    BudgetLimits, Budgets, DeviceProfile, TileCacheBudget, resolve, step_down, step_down_for,
};
use crate::constants::LOOP_SCAN_RESERVE_BYTES;
use crate::quality::{GroundPass, offscreen_bytes};
use crate::scene::{Capacity, Need, PaneNeed, Scene};
use squallar_radar::types::RenderView;

/// Bytes one resident voxel grid of a cell budget costs the device, `None` on
/// overflow — `squallar_volumetric::raymarch::resident_grid_bytes`'s shape. The
/// figure is the raymarch's arithmetic (every mip level as the backend lays it
/// out, the colour table's texture, the jitter tile) and this crate sits under
/// that one, so the application hands the function in rather than this crate
/// re-deriving it; the agreement test beside the raymarch runs [`need`] and
/// [`fit`] with the real one.
pub type GridBytes = fn([u32; 3]) -> Option<usize>;

/// A scene's cost, one figure per kind of thing resident. Kept apart so the
/// loop pool can be sized from the loop term against the room the rest leaves,
/// and so a telemetry line can name which term moved.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NeedTerms {
    /// One static render per 2D pane: the raster ceiling's worst case for a
    /// plan view, the section frame for a cross-section.
    pub static_rasters: u64,
    /// Every looping pane's frames for its span, at its frame's cost.
    pub loops: u64,
    /// The live grids 3D panes keep beside their loops.
    pub grids: u64,
    /// The pane-sized targets 3D panes raymarch into, fitted as the painter
    /// fits them.
    pub offscreens: u64,
    /// The one pane-mirror texture.
    pub mirror: u64,
    /// The prism buffers of every pane drawing buildings, each at the ceiling
    /// its mesh is fitted inside.
    pub buildings: u64,
    /// The tile working set, on the host.
    pub tiles_host: u64,
    /// **Every shown overlay picture, on the host**: each pane's
    /// `overlay_pictures` at [`picture_bytes`] for that pane at the budget's
    /// oversampling. A picture is a page buffer from the moment the
    /// worker's reply is copied in until its last upload band has crossed to
    /// the GPU — on a ringless device four MiB a frame, so a 43 MB picture
    /// is eleven frames of residency and thirteen shown layers that
    /// re-rasterise together are all resident at once.
    pub pictures_host: u64,
    /// **One more picture, on the host, for the arrival in flight**: the
    /// largest picture any pane shows, once. The reply is decoded into a
    /// second buffer while the first is alive and then converted into the
    /// image the GPU is handed while the second is alive, so at every
    /// arrival one picture is resident twice for a moment. Zero where no
    /// pane shows a picture. **A max across panes, not a sum** — the one
    /// term of a scene's cost that is not one pane's plus the next's, which
    /// is why [`PaneTerms`] carries its candidate apart from the pane's own
    /// totals.
    pub picture_arrival_host: u64,
    /// **Every enabled gridded overlay's decoded source, on the host**, at
    /// the budget its handler states — counted once however many panes show
    /// the layer, because the handler is one instance for the whole
    /// application ([`Scene::overlay_grids`]). Nothing until a pane enables a
    /// gridded layer; then MRMS, GMGSI or the model at their key-space grid
    /// budgets, which is what a gridded layer asks the heap to be able to
    /// hold.
    pub overlay_grids_host: u64,
    /// **One decoded Level II volume per radar loop frame, on the host** —
    /// resident frames at their measured size, pending frames at the
    /// reserve: every radar-looping pane's
    /// [`PaneNeed::loop_scans_resident_bytes`] plus
    /// [`LOOP_SCAN_RESERVE_BYTES`] for each frame of its base the cache does
    /// not hold yet, a pane whose site is already counted
    /// ([`PaneNeed::loop_scans_shared`]) contributing nothing. The loop
    /// download cache holds these beside the textures the `loops` term
    /// prices, and until this term existed the scene's largest host family
    /// — some 650 MiB resting on desktop web — was priced at zero. A bound is
    /// never charged for a measured thing, and a live allocation is never
    /// priced under its size: a loop holding more frames than its base
    /// charges every one of them.
    ///
    /// **A Level III loop charges nothing here**: its frames are rendered
    /// from paired objects, so its site's volumes are dropped
    /// ([`PaneNeed::loop_scans_needed`]) and only a pane parked at a still
    /// keeps one.
    pub loop_scans_host: u64,
}

impl NeedTerms {
    /// The two totals.
    pub fn total(&self) -> Need {
        Need {
            gpu_bytes: self.gpu_without_loops().saturating_add(self.loops),
            host_bytes: self
                .tiles_host
                .saturating_add(self.pictures_host)
                .saturating_add(self.picture_arrival_host)
                .saturating_add(self.overlay_grids_host)
                .saturating_add(self.loop_scans_host),
        }
    }

    /// Fold one pane's terms in: every additive term saturating-added, the
    /// arrival candidate taken as a max.
    fn fold_pane(&mut self, pane: &PaneTerms) {
        self.static_rasters = self.static_rasters.saturating_add(pane.static_rasters);
        self.loops = self.loops.saturating_add(pane.loops);
        self.grids = self.grids.saturating_add(pane.grids);
        self.offscreens = self.offscreens.saturating_add(pane.offscreens);
        self.buildings = self.buildings.saturating_add(pane.buildings);
        self.pictures_host = self.pictures_host.saturating_add(pane.pictures_host);
        self.picture_arrival_host = self.picture_arrival_host.max(pane.picture_host);
        self.loop_scans_host = self.loop_scans_host.saturating_add(pane.loop_scans_host);
    }

    /// Every GPU term but the loops — what the loop pool has to fit beside.
    pub fn gpu_without_loops(&self) -> u64 {
        self.static_rasters
            .saturating_add(self.grids)
            .saturating_add(self.offscreens)
            .saturating_add(self.mirror)
            .saturating_add(self.buildings)
    }
}

/// **One pane's share of a scene's cost**, term by term — what
/// [`need_terms_for_pane`] prices and [`need_terms`] sums over the panes
/// before the scene-level terms (the mirror, the tiles, the overlay grids,
/// the arrival) join. Every field but [`Self::picture_host`] adds across
/// panes. That one is this pane's candidate for the scene's
/// `picture_arrival_host` — a max across panes, not a sum — and is **not**
/// in the pane's own totals: the arrival is one buffer for the whole
/// application, and attributing it to a pane would make the parts sum to
/// more than the whole.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PaneTerms {
    /// The pane's static render, if it is 2D.
    pub static_rasters: u64,
    /// Its loop's frames for its span, at its frame's cost — zero for a pane
    /// that is an alias of another pane's loop.
    pub loops: u64,
    /// The live grid a 3D pane keeps beside its loop.
    pub grids: u64,
    /// The offscreen a 3D pane raymarches into.
    pub offscreens: u64,
    /// Its prism buffers, if it draws buildings.
    pub buildings: u64,
    /// Every whole-picture overlay it shows, on the host.
    pub pictures_host: u64,
    /// One decoded volume per frame of its radar loop, on the host — the
    /// resident ones at their measured size, the pending ones at the
    /// reserve; zero for a pane whose site another pane already counts, and
    /// for a loop of a layer that is not radar.
    pub loop_scans_host: u64,
    /// One of its pictures, for the scene's arrival term to take the max of.
    /// Not in [`Self::host_bytes`].
    pub picture_host: u64,
}

impl PaneTerms {
    /// What this pane costs the GPU.
    pub fn gpu_bytes(&self) -> u64 {
        self.static_rasters
            .saturating_add(self.loops)
            .saturating_add(self.grids)
            .saturating_add(self.offscreens)
            .saturating_add(self.buildings)
    }

    /// What this pane costs the host, the arrival excluded.
    pub fn host_bytes(&self) -> u64 {
        self.pictures_host.saturating_add(self.loop_scans_host)
    }

    /// The two totals.
    pub fn total(&self) -> Need {
        Need {
            gpu_bytes: self.gpu_bytes(),
            host_bytes: self.host_bytes(),
        }
    }
}

/// What `scene` costs at `budgets`, term by term: every pane's
/// [`need_terms_for_pane`] folded in, then the scene-level terms — the
/// mirror, the tile working set, the enabled gridded overlays' budgets.
pub fn need_terms(scene: &Scene, budgets: &Budgets, grid_bytes: GridBytes) -> NeedTerms {
    let grid = grid_cost(budgets, grid_bytes);
    let mut terms = NeedTerms::default();
    for pane in &scene.panes {
        terms.fold_pane(&pane_terms(pane, budgets, grid));
    }
    terms.mirror = mirror_term(scene.mirror_px, budgets);
    for source in &scene.tile_sources {
        terms.tiles_host = terms.tiles_host.saturating_add(
            ((source.tiles_on_glass + source.ancestor_net) as u64)
                .saturating_mul(source.bytes_per_tile as u64),
        );
    }
    for layer in &scene.overlay_grids {
        terms.overlay_grids_host = terms.overlay_grids_host.saturating_add(layer.budget_bytes);
    }
    terms
}

/// What one pane of a scene costs at `budgets`, term by term. Summed over
/// the panes by [`need_terms`], so the two cannot disagree; priced alone
/// where a per-pane figure is wanted — a readout, or the increment a pane
/// being opened would add.
pub fn need_terms_for_pane(pane: &PaneNeed, budgets: &Budgets, grid_bytes: GridBytes) -> PaneTerms {
    pane_terms(pane, budgets, grid_cost(budgets, grid_bytes))
}

/// [`need_terms_for_pane`] with the grid already priced, so the scene walk
/// prices it once.
fn pane_terms(pane: &PaneNeed, budgets: &Budgets, grid: u64) -> PaneTerms {
    let mut terms = PaneTerms {
        static_rasters: static_raster_bytes(pane, budgets),
        grids: (pane.volume_grids as u64).saturating_mul(grid),
        offscreens: offscreen_term(pane, budgets),
        buildings: buildings_term(pane, budgets),
        ..PaneTerms::default()
    };
    if pane.looping {
        let frames = loop_frames(pane, budgets) as u64;
        terms.loops = frames.saturating_mul(loop_frame_bytes(pane, budgets, grid));
        // A radar loop whose frames are rendered from decoded volumes holds
        // one per frame: the ones already there at what they were measured
        // at, the ones still to come at the reserve. Monotone down the ladder
        // — the resident part is fixed and the pending count falls with the
        // base — and never under the resident bytes, whatever the rung.
        //
        // Three loops charge nothing here. A loop of another layer holds its
        // own rasters and no volume. A pane on a site already counted holds
        // the first pane's. And a loop whose frames read Level III objects
        // renders from no volume at all
        // ([`PaneNeed::loop_scans_needed`]): its site holds only the volume a
        // pane parked at a still is keeping, which the resident bytes carry,
        // and no reserve is charged for frames that will never fetch one.
        if pane.overlay_frame_bytes == 0 && !pane.loop_scans_shared {
            let pending = if pane.loop_scans_needed {
                frames.saturating_sub(pane.loop_scans_resident_frames as u64)
            } else {
                0
            };
            terms.loop_scans_host = pane
                .loop_scans_resident_bytes
                .saturating_add(pending.saturating_mul(LOOP_SCAN_RESERVE_BYTES));
        }
    }
    if pane.overlay_pictures > 0 {
        let picture = picture_bytes(pane.picture_px, budgets.overlay_oversample_percent);
        terms.pictures_host = (pane.overlay_pictures as u64).saturating_mul(picture);
        terms.picture_host = picture;
    }
    terms
}

/// What `scene` costs at `budgets`.
pub fn need(scene: &Scene, budgets: &Budgets, grid_bytes: GridBytes) -> Need {
    need_terms(scene, budgets, grid_bytes).total()
}

/// **Bytes one whole-picture overlay raster of a pane of `px` costs at
/// `oversample_percent` per side**: `(w * p / 100) * (h * p / 100) * 4`,
/// integer division per side — the planner's own arithmetic
/// (`squallar_egui::overlay_cache::plan_overlay_texture`: `(side * scale) as
/// u32` in `f32`, which truncates the same way for every scale in
/// `constants::OVERLAY_OVERSAMPLE_PERCENTS`, each a dyadic rational). It is
/// the figure the application's `overlay pictures:` line reports per pane,
/// restated here so the need model and the telemetry cannot disagree. The
/// adapter's texture limit, which the planner also clamps to, is not known
/// here: a pane wider than the limit is over-priced, never under — the side
/// a budget resolved without probing the machine may be on.
pub fn picture_bytes(px: [u32; 2], oversample_percent: u16) -> u64 {
    let side = |n: u32| u64::from(n) * u64::from(oversample_percent) / 100;
    side(px[0]).saturating_mul(side(px[1])).saturating_mul(4)
}

/// Frames one looping pane wants: its own lookback converted at its cadence and
/// held to the budget's span and render budget — `Budgets::frames_for_span`'s
/// arithmetic with the pane's span in place of the budget's.
pub fn loop_frames(pane: &PaneNeed, budgets: &Budgets) -> usize {
    budgets.frames_for_span_of(pane.loop_span_secs, pane.cadence_secs)
}

/// What the loops need, in bytes: every looping pane's frames at its frame's
/// cost.
pub fn loop_need(scene: &Scene, budgets: &Budgets, grid_bytes: GridBytes) -> u64 {
    need_terms(scene, budgets, grid_bytes).loops
}

/// What `cap` leaves for the loops once everything else in `scene` is paid for.
pub fn loop_room(scene: &Scene, budgets: &Budgets, cap: &Capacity, grid_bytes: GridBytes) -> u64 {
    cap.allowance()
        .saturating_sub(need_terms(scene, budgets, grid_bytes).gpu_without_loops())
}

/// **The most frames one looping pane could ever hold**: its whole lookback
/// at its cadence — not held to the budget's span, which is what
/// [`loop_frames`] is held to — and never more than the class's list cap,
/// since a longer listing is sampled down to that before anything is fetched.
/// The base itself where no cadence is known yet: nothing says what more
/// exists. Never below the base.
pub fn loop_frames_ceiling(pane: &PaneNeed, budgets: &Budgets) -> usize {
    let base = loop_frames(pane, budgets);
    let Some(cadence) = pane.cadence_secs.filter(|secs| *secs > 0) else {
        return base;
    };
    (1 + pane.loop_span_secs / cadence as usize)
        .min(budgets.loop_frames_held)
        .max(base)
}

/// What the loops could ever fill, in bytes: every looping pane's
/// [`loop_frames_ceiling`] at its frame's cost. Not a term of [`need`] — `fit`
/// never charges it.
pub fn loop_ceiling(scene: &Scene, budgets: &Budgets, grid_bytes: GridBytes) -> u64 {
    let grid = grid_cost(budgets, grid_bytes);
    scene
        .panes
        .iter()
        .filter(|pane| pane.looping)
        .fold(0u64, |sum, pane| {
            sum.saturating_add(
                (loop_frames_ceiling(pane, budgets) as u64)
                    .saturating_mul(loop_frame_bytes(pane, budgets, grid)),
            )
        })
}

/// The loop pool's size: **the room the rest of the scene leaves**, capped at
/// what the loops could ever fill ([`loop_ceiling`]). The two functions here
/// ask two different questions: [`fit`] asks *does the scene fit* — its need
/// is every loop's base, the lookback held to the rung's span — and this asks
/// *how much room is left* once it does. The application's own planner gives
/// that room out base first and the rest by time, so a device with more room
/// holds the loops the scene asked for more densely, never longer.
pub fn loop_pool_bytes(
    scene: &Scene,
    budgets: &Budgets,
    cap: &Capacity,
    grid_bytes: GridBytes,
) -> u64 {
    let terms = need_terms(scene, budgets, grid_bytes);
    loop_ceiling(scene, budgets, grid_bytes)
        .min(cap.allowance().saturating_sub(terms.gpu_without_loops()))
}

/// Which of a scene's two needs are over `cap`'s allowances at `budgets`:
/// `(gpu, host)`. A capacity with no host figure has no host allowance, and
/// nothing is ever over one.
pub fn over(
    scene: &Scene,
    budgets: &Budgets,
    cap: &Capacity,
    grid_bytes: GridBytes,
) -> (bool, bool) {
    let need = need(scene, budgets, grid_bytes);
    (
        need.gpu_bytes > cap.allowance(),
        cap.host_allowance()
            .is_some_and(|allowance| need.host_bytes > allowance),
    )
}

/// The largest budgets whose need for `scene` fits `cap`'s allowances.
///
/// Starts from `resolve(profile)` — the rung the class earns, so a scene that
/// fits changes nothing — and while a need is over its allowance takes the
/// next rung of the shed order `budget::demote` walks **that lowers an axis
/// which is over**: 3D lighting, 3D offscreen resolution, loop span (halving
/// toward the two-frame floor), overlay oversampling, tile sharpness, 3D
/// grid, raster side. A page heap over its allowance never costs the loop
/// its history, and a card over its allowance never costs a picture its
/// margin for a byte the GPU model does not price. Stops when the scene
/// fits, or when every rung that could answer is at its stop — then the
/// floor budgets come back and the runtime clamps and logs. `steps_back`
/// counts the rungs taken.
pub fn fit(
    scene: &Scene,
    profile: &DeviceProfile,
    cap: &Capacity,
    grid_bytes: GridBytes,
) -> Budgets {
    let limits = &profile.limits;
    let mut budgets = resolve(profile);
    loop {
        let (gpu_over, host_over) = over(scene, &budgets, cap, grid_bytes);
        if !gpu_over && !host_over {
            break;
        }
        if !step_down_for(&mut budgets, limits, gpu_over, host_over) {
            break;
        }
        budgets.steps_back = budgets.steps_back.saturating_add(1);
    }
    budgets
}

/// Whether no rung of the ladder can move `budgets` any further — the floor
/// configuration, where [`fit`] gives up and the runtime clamps.
pub fn every_rung_at_its_stop(budgets: &Budgets, limits: &BudgetLimits) -> bool {
    let mut probe = *budgets;
    !step_down(&mut probe, limits)
}

/// **What `scene` costs at the ladder's floor** — every rung at its stop, the
/// budgets [`fit`] hands back when nothing can pay. Below this figure a
/// capacity presumption buys nothing: the ladder has nothing left to shed, so
/// a presumption lowered past it only makes the readout lie about a wall the
/// scene was never going to fit under. The application's pressure decay is
/// floored here for exactly that reason.
///
/// Walks the ladder from the class rung rather than reading a constant: the
/// floor is a configuration of every rung, and the need at it is the scene's
/// (its panes, its loops, its grids) priced at that configuration.
pub fn floor_need(scene: &Scene, profile: &DeviceProfile, grid_bytes: GridBytes) -> Need {
    let mut floor = resolve(profile);
    while step_down(&mut floor, &profile.limits) {}
    need(scene, &floor, grid_bytes)
}

/// Whether no rung that lowers the GPU need can move `budgets` any further.
pub fn every_gpu_rung_at_its_stop(budgets: &Budgets, limits: &BudgetLimits) -> bool {
    let mut probe = *budgets;
    !step_down_for(&mut probe, limits, true, false)
}

/// Whether no rung that lowers the host need can move `budgets` any further.
pub fn every_host_rung_at_its_stop(budgets: &Budgets, limits: &BudgetLimits) -> bool {
    let mut probe = *budgets;
    !step_down_for(&mut probe, limits, false, true)
}

/// **The invariant [`fit`] promises**, stated once so the runtime can check
/// the answer it adopts: on each axis, `scene`'s need at `budgets` fits
/// `cap`'s allowance or every rung that lowers that axis is at its stop and
/// there was nothing left to shed. `fit` holds it by construction on both
/// arms; a `false` here is a defect in the arithmetic — a term that stopped
/// being monotone down the ladder — and the application logs it and holds
/// the pool at its floor rather than trusting the budgets.
pub fn fit_holds(
    scene: &Scene,
    budgets: &Budgets,
    limits: &BudgetLimits,
    cap: &Capacity,
    grid_bytes: GridBytes,
) -> bool {
    let (gpu_over, host_over) = over(scene, budgets, cap, grid_bytes);
    (!gpu_over || every_gpu_rung_at_its_stop(budgets, limits))
        && (!host_over || every_host_rung_at_its_stop(budgets, limits))
}

/// What may be resident beyond `scene`'s need at `budgets` under `cap`:
/// [`Capacity::economy_allowance`] at the scene's price.
pub fn economy_allowance(
    scene: &Scene,
    budgets: &Budgets,
    cap: &Capacity,
    grid_bytes: GridBytes,
) -> u64 {
    cap.economy_allowance(need(scene, budgets, grid_bytes))
}

/// The shares of the economy allowance the three tile populations take on
/// the measured arm — styled, parsed, terrain — as parts of five. Two, two
/// and one: the styled history and the parsed geometry are the two economies
/// a pan and a restyle come back to, and a terrain raster is a small fraction
/// of a styled entry's tail.
pub const TILE_ECONOMY_SHARES: [u64; 3] = [2, 2, 1];

/// **What the tile caches may hold under `cap`**, per population.
///
/// On the **presumed** arm this is the class rung's own figures
/// ([`Budgets::tile_cache`]), exactly as every other presumed allowance is
/// the bracket's constant. On the **measured or probed** arm the economy
/// allowance — `ECONOMY_FRACTION` of the card less what the scene needs — is
/// split [`TILE_ECONOMY_SHARES`] ways, and each share is **held inside its
/// bracket**: never below the floor (the worst device this build works on
/// still holds its history), never above the ceiling (a 24 GiB card does not
/// hold gigabytes of tiles because it could; the ceiling is the generous cap
/// ruling 5 asked for). So a card with room resolves the ceiling whatever
/// rung its class earned, and a 4 GiB card whose scene has taken most of it
/// resolves toward the floor.
///
/// The tile-sharpness rung rides along on both arms: `whole_zoom` is
/// [`Budgets::tile_whole_zoom`] whatever the capacity's source, because it is
/// the ladder's answer and not the economy's.
pub fn tile_cache_budget(
    scene: &Scene,
    budgets: &Budgets,
    limits: &BudgetLimits,
    cap: &Capacity,
    grid_bytes: GridBytes,
) -> TileCacheBudget {
    if cap.source == crate::scene::CapacitySource::Presumed {
        return budgets.tile_cache();
    }
    let economy = economy_allowance(scene, budgets, cap, grid_bytes);
    let parts: u64 = TILE_ECONOMY_SHARES.iter().sum();
    let share = |n: u64, bracket: crate::budget::Bracket| {
        let raw = usize::try_from(economy / parts * n).unwrap_or(usize::MAX);
        bracket.hold(raw) as u64
    };
    TileCacheBudget {
        styled_bytes: share(TILE_ECONOMY_SHARES[0], limits.tile_styled_bytes),
        parsed_bytes: share(TILE_ECONOMY_SHARES[1], limits.tile_parsed_bytes),
        terrain_bytes: share(TILE_ECONOMY_SHARES[2], limits.tile_terrain_bytes),
        whole_zoom: budgets.tile_whole_zoom,
    }
}

/// One resident grid's cost, `u64::MAX` where the raymarch's arithmetic
/// overflowed — a grid that cannot be priced cannot be afforded.
fn grid_cost(budgets: &Budgets, grid_bytes: GridBytes) -> u64 {
    grid_bytes(budgets.grid_cells).map_or(u64::MAX, |bytes| bytes as u64)
}

/// The static render a 2D pane holds: the raster ceiling's worst case, since
/// that is the most a device on this class can be asked to hold; the section
/// frame for a cross-section. A 3D pane's picture is its offscreen and grids.
fn static_raster_bytes(pane: &PaneNeed, budgets: &Budgets) -> u64 {
    match pane.view {
        RenderView::PlanView => budgets.static_frame_bytes() as u64,
        RenderView::CrossSection => budgets.section_frame_bytes() as u64,
        RenderView::Volume => 0,
    }
}

/// What one frame of this pane's loop costs: the layer's own measured frame
/// for a loop that is not radar, else radar's three shapes as the loop pool's
/// frame model prices them.
fn loop_frame_bytes(pane: &PaneNeed, budgets: &Budgets, grid: u64) -> u64 {
    if pane.overlay_frame_bytes > 0 {
        return pane.overlay_frame_bytes as u64;
    }
    match pane.view {
        RenderView::PlanView => budgets.loop_frame_bytes() as u64,
        RenderView::CrossSection => budgets.section_frame_bytes() as u64,
        RenderView::Volume => grid,
    }
}

/// The offscreen a 3D pane raymarches into, fitted exactly as the painter fits
/// it: the pane at the quality ceiling's resolution rung, stepped down until it
/// fits the offscreen budget, every attachment the ground pass implies counted.
fn offscreen_term(pane: &PaneNeed, budgets: &Budgets) -> u64 {
    match pane.view {
        RenderView::Volume => budgets
            .quality_ceiling
            .fit(pane.px, budgets.offscreen_bytes, pane.ground)
            .bytes() as u64,
        RenderView::PlanView | RenderView::CrossSection => 0,
    }
}

/// What a pane's buildings cost: the ceiling the prism ladder is fitted
/// inside, [`Budgets::prism_vram_bytes`], and nothing for a pane that draws
/// none. The fitted rung's own `PrismBudget::budgeted_bytes` is the exact
/// figure and it is not reachable from here -- `squallar-buildings` sits
/// beside this crate, not under it, and this crate's charter declares
/// `squallar-radar` and nothing else -- so the term is the worst case, the
/// way the static raster term is the raster ceiling's worst case rather than
/// the sweep's own side. The ladder never exceeds its ceiling while the
/// ceiling clears the floor rung's 1.18 MB, which every shipped arm does.
fn buildings_term(pane: &PaneNeed, budgets: &Budgets) -> u64 {
    if pane.buildings {
        budgets.prism_vram_bytes as u64
    } else {
        0
    }
}

/// The pane mirror: one colour target of its size, held to the mirror budget.
fn mirror_term(mirror_px: [u32; 2], budgets: &Budgets) -> u64 {
    (offscreen_bytes(mirror_px, GroundPass::Off) as u64).min(budgets.mirror_bytes as u64)
}

#[path = "fit/tests.rs"]
#[cfg(test)]
mod tests;
