//! The one function: what a scene costs at a given [`Budgets`], and the largest
//! [`Budgets`] whose cost fits a [`Capacity`].
//!
//! [`need`] sums terms the tree already prices — `Budgets::loop_frame_bytes`,
//! `Budgets::section_frame_bytes`, `Budgets::static_frame_bytes`, the raymarch's
//! own resident-grid arithmetic handed in as [`GridBytes`],
//! `quality::VolumeQuality::fit` for the offscreen, `quality::offscreen_bytes`
//! for the mirror, the tile cache's measured entry cost, and
//! `Budgets::prism_vram_bytes` for a pane that draws buildings. Nothing here
//! invents a byte figure.
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

use crate::budget::{BudgetLimits, Budgets, DeviceProfile, resolve, step_down};
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
}

impl NeedTerms {
    /// The two totals.
    pub fn total(&self) -> Need {
        Need {
            gpu_bytes: self.gpu_without_loops().saturating_add(self.loops),
            host_bytes: self.tiles_host,
        }
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

/// What `scene` costs at `budgets`, term by term.
pub fn need_terms(scene: &Scene, budgets: &Budgets, grid_bytes: GridBytes) -> NeedTerms {
    let grid = grid_cost(budgets, grid_bytes);
    let mut terms = NeedTerms::default();
    for pane in &scene.panes {
        terms.static_rasters = terms
            .static_rasters
            .saturating_add(static_raster_bytes(pane, budgets));
        if pane.looping {
            terms.loops = terms.loops.saturating_add(
                (loop_frames(pane, budgets) as u64)
                    .saturating_mul(loop_frame_bytes(pane, budgets, grid)),
            );
        }
        terms.grids = terms
            .grids
            .saturating_add((pane.volume_grids as u64).saturating_mul(grid));
        terms.offscreens = terms
            .offscreens
            .saturating_add(offscreen_term(pane, budgets));
        terms.buildings = terms
            .buildings
            .saturating_add(buildings_term(pane, budgets));
    }
    terms.mirror = mirror_term(scene.mirror_px, budgets);
    for source in &scene.tile_sources {
        terms.tiles_host = terms.tiles_host.saturating_add(
            ((source.tiles_on_glass + source.ancestor_net) as u64)
                .saturating_mul(source.bytes_per_tile as u64),
        );
    }
    terms
}

/// What `scene` costs at `budgets`.
pub fn need(scene: &Scene, budgets: &Budgets, grid_bytes: GridBytes) -> Need {
    need_terms(scene, budgets, grid_bytes).total()
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

/// The loop pool's size: what the loops need, capped by the room. The pool is
/// then divided among the loops by the application's own planner; a device with
/// more room does not hold more loop than the scene asks for.
pub fn loop_pool_bytes(
    scene: &Scene,
    budgets: &Budgets,
    cap: &Capacity,
    grid_bytes: GridBytes,
) -> u64 {
    let terms = need_terms(scene, budgets, grid_bytes);
    terms
        .loops
        .min(cap.allowance().saturating_sub(terms.gpu_without_loops()))
}

/// The largest budgets whose need for `scene` fits `cap`'s allowance.
///
/// Starts from `resolve(profile)` — the rung the class earns, so a scene that
/// fits changes nothing — and while the need is over the allowance takes the
/// next rung of the shed order `budget::demote` walks: 3D lighting, 3D
/// offscreen resolution, loop span (halving toward the two-frame floor), tile
/// sharpness, 3D grid, raster side. Stops when the scene fits, or when every
/// rung is at its stop — then the floor budgets come back and the runtime
/// clamps and logs. `steps_back` counts the rungs taken.
pub fn fit(
    scene: &Scene,
    profile: &DeviceProfile,
    cap: &Capacity,
    grid_bytes: GridBytes,
) -> Budgets {
    let limits = &profile.limits;
    let mut budgets = resolve(profile);
    let allowance = cap.allowance();
    while need(scene, &budgets, grid_bytes).gpu_bytes > allowance {
        if !step_down(&mut budgets, limits) {
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

/// **The invariant [`fit`] promises**, stated once so the runtime can check
/// the answer it adopts: `scene`'s need at `budgets` fits `cap`'s allowance,
/// or every rung is at its stop and there was nothing left to shed. `fit`
/// holds it by construction on both arms; a `false` here is a defect in the
/// arithmetic — a term that stopped being monotone down the ladder — and the
/// application logs it and holds the pool at its floor rather than trusting
/// the budgets.
pub fn fit_holds(
    scene: &Scene,
    budgets: &Budgets,
    limits: &BudgetLimits,
    cap: &Capacity,
    grid_bytes: GridBytes,
) -> bool {
    need(scene, budgets, grid_bytes).gpu_bytes <= cap.allowance()
        || every_rung_at_its_stop(budgets, limits)
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
