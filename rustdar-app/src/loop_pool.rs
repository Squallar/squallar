//! One loop allowance for the whole application, divided among the loops that
//! actually want one.
//!
//! The unit of division is a **loop**, not a pane: two 3D panes orbiting one
//! volume are one resident set in one store, so they cost one share.
//!
//! wgpu 29.0.4 exposes no memory capacity on any backend and WebGL2 reports no
//! device type, so each target's pool is a floor/ceiling around a runtime
//! class, with [`LoopPool::back_off`] halving toward the floor on a device
//! error or a lost surface as the backstop. `mobile` is a cfg for native
//! Android/iOS: a browser on a phone is `wasm32`, not `mobile`.

use rustdar_device_profile::budget::{Budgets, Promotion};
use rustdar_device_profile::constants::{
    LOOP_IMAGE_SIZE, LOOP_POOL_CEILING_BYTES, LOOP_POOL_DWELL_FRAMES, LOOP_POOL_FLOOR_BYTES,
    LOOP_POOL_HYSTERESIS, MAX_LOOP_RENDER_BUDGET, MIN_LOOP_FRAMES_PER_PANE, VOLUME_GRID_CELLS,
};
use rustdar_device_profile::quality::DeviceClass;
use rustdar_kv::KvStore;
use rustdar_radar::types::RenderView;

/// Key the discovered pool size is persisted under.
pub const LOOP_POOL_KEY: &str = "loop_pool";

/// The bounds this target holds a discovered pool between.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LoopPoolLimits {
    /// What the pool is on a device that can tell us nothing, or that has
    /// already refused an allocation. Never goes below this.
    pub floor: usize,
    /// The most this target will ever spend on loop textures, however much the
    /// device claims to have.
    pub ceiling: usize,
}

impl LoopPoolLimits {
    /// The compiled target's bounds.
    pub const fn for_target() -> Self {
        Self {
            floor: LOOP_POOL_FLOOR_BYTES,
            ceiling: LOOP_POOL_CEILING_BYTES,
        }
    }

    /// The bounds a resolved [`Budgets`] carries.
    pub const fn from_budgets(budgets: &Budgets) -> Self {
        Self {
            floor: budgets.loop_pool_floor_bytes,
            ceiling: budgets.loop_pool_ceiling_bytes,
        }
    }

    /// `bytes` held between the two, with the floor winning a crossed pair.
    fn hold(&self, bytes: usize) -> usize {
        bytes.clamp(self.floor, self.ceiling.max(self.floor))
    }
}

/// What one loop frame costs on this device class, and how many the dispatcher
/// will ever texture.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LoopFrameModel {
    /// A [`LOOP_IMAGE_SIZE`]² RGBA raster.
    pub plan_view: usize,
    /// `SECTION_WIDTH × SECTION_HEIGHT` RGBA — half a plan-view frame.
    pub section: usize,
    /// One resident voxel grid with its mips and its colour table.
    pub grid: usize,
    /// This class's `MAX_LOOP_RENDER_BUDGET`: no share buys more frames
    /// than the dispatcher would texture.
    pub render_budget: usize,
}

impl LoopFrameModel {
    /// The compiled target's figures.
    pub fn for_target() -> Self {
        let side = LOOP_IMAGE_SIZE;
        Self {
            plan_view: side * side * 4,
            section: rustdar_radar::xsect::SECTION_WIDTH * rustdar_radar::xsect::SECTION_HEIGHT * 4,
            grid: rustdar_volumetric::raymarch::resident_grid_bytes(VOLUME_GRID_CELLS)
                .unwrap_or(usize::MAX),
            render_budget: MAX_LOOP_RENDER_BUDGET,
        }
    }

    /// The figures a resolved [`Budgets`] carries.
    pub fn from_budgets(budgets: &Budgets) -> Self {
        Self {
            plan_view: budgets.loop_frame_bytes(),
            section: budgets.section_frame_bytes(),
            // Bytes one resident voxel grid costs: every mip level, its
            // colour table's own texture and the jitter tile beside it, read
            // from the upload path's own arithmetic (`None` only on a `usize`
            // overflow).
            grid: rustdar_volumetric::raymarch::resident_grid_bytes(budgets.grid_cells)
                .unwrap_or(usize::MAX),
            render_budget: budgets.loop_render_budget,
        }
    }
}

/// What the panes are asking for, this frame.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LoopDemand {
    /// Panes running a plan-view loop.
    pub plan_view_loops: usize,
    /// Panes running a cross-section loop.
    pub section_loops: usize,
    /// **Distinct volume loop keys**, not panes. Two 3D panes on one volume are
    /// one entry here, because they are one resident set in one store.
    pub volume_sets: usize,
}

impl LoopDemand {
    /// How many ways the pool is split.
    pub fn shares(&self) -> usize {
        self.plan_view_loops + self.section_loops + self.volume_sets
    }

    /// Fold one more loop of `view` in, deduplicating 3D loops by `key`.
    /// `key` is `None` for the two raster kinds, which never share.
    pub fn add(&mut self, view: RenderView, already_counted: bool) {
        match view {
            RenderView::PlanView => self.plan_view_loops += 1,
            RenderView::CrossSection => self.section_loops += 1,
            RenderView::Volume => {
                if !already_counted {
                    self.volume_sets += 1;
                }
            }
        }
    }
}

/// What each loop gets, once the pool has been divided.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LoopAllocation {
    /// One loop's slice of the pool, in bytes.
    pub share_bytes: usize,
    /// Textured frames a plan-view loop may keep.
    pub plan_view_frames: usize,
    /// Textured frames a cross-section loop may keep — more than a plan-view
    /// loop at the same share, because a section frame is half the size.
    pub section_frames: usize,
    /// Resident grids **one** 3D loop may keep, which for that kind is also its
    /// whole frame list. See [`Self::volume_reserve_bytes`].
    pub volume_frames: usize,
    /// The distinct 3D loops this allocation was computed for.
    pub volume_sets: usize,
}

impl LoopAllocation {
    /// Frames of a loop of this view that are ready to show at once.
    pub fn frames_for(&self, view: RenderView) -> usize {
        match view {
            RenderView::PlanView => self.plan_view_frames,
            RenderView::CrossSection => self.section_frames,
            RenderView::Volume => self.volume_frames,
        }
    }

    /// What `VolumeStore::enforce_budget` is held to.
    pub fn volume_reserve_bytes(&self) -> usize {
        self.share_bytes * self.volume_sets
    }

    /// What this allocation costs if every loop in `demand` fills it.
    pub fn bytes(&self, model: LoopFrameModel, demand: LoopDemand) -> usize {
        demand.plan_view_loops * self.plan_view_frames * model.plan_view
            + demand.section_loops * self.section_frames * model.section
            + demand.volume_sets * self.volume_frames * model.grid
    }
}

/// The application's whole loop allowance, in bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LoopPool {
    bytes: usize,
}

impl LoopPool {
    /// A pool of exactly `bytes`, held inside `limits`.
    pub fn new(bytes: usize, limits: LoopPoolLimits) -> Self {
        Self {
            bytes: limits.hold(bytes),
        }
    }

    /// What this device gets, before anything has been rendered.
    pub fn for_device(
        class: DeviceClass,
        remembered: Option<usize>,
        limits: LoopPoolLimits,
    ) -> Self {
        Self::for_promotion(Promotion::for_class(class), remembered, limits)
    }

    /// The same, at the [`Promotion`] the whole budget set was resolved at.
    pub fn for_promotion(
        promotion: Promotion,
        remembered: Option<usize>,
        limits: LoopPoolLimits,
    ) -> Self {
        if let Some(bytes) = remembered {
            return Self::new(bytes, limits);
        }
        let bytes = match promotion {
            Promotion::Ceiling => limits.ceiling,
            Promotion::Step => limits.floor.saturating_mul(2),
            Promotion::Floor => limits.floor,
        };
        Self::new(bytes, limits)
    }

    /// The pool, in bytes.
    pub fn bytes(&self) -> usize {
        self.bytes
    }

    /// Step down after the device refused, and say whether anything moved.
    pub fn back_off(&mut self, limits: LoopPoolLimits) -> bool {
        let reduced = limits.hold(self.bytes / 2);
        if reduced >= self.bytes {
            return false;
        }
        self.bytes = reduced;
        true
    }

    /// Divide the pool among the loops that want one.
    pub fn plan(&self, model: LoopFrameModel, demand: LoopDemand) -> LoopAllocation {
        let share_bytes = self.bytes / demand.shares().max(1);
            // `render_budget` could be edited below the minimum, and `clamp`
            // panics on a crossed pair; the floor wins.
        let cap = model.render_budget.max(MIN_LOOP_FRAMES_PER_PANE);
            // A frame that costs nothing is a model built wrong; the cap
            // cannot then divide by zero and cannot become an unbounded loop.
        let frames = |budget: usize, cost: usize| {
            budget
                .checked_div(cost)
                .unwrap_or(cap)
                .clamp(MIN_LOOP_FRAMES_PER_PANE, cap)
        };
        let volume_frames = frames(share_bytes.saturating_sub(model.grid), model.grid);
        LoopAllocation {
            share_bytes,
            plan_view_frames: frames(share_bytes, model.plan_view),
            section_frames: frames(share_bytes, model.section),
            volume_frames,
            volume_sets: demand.volume_sets,
        }
    }
}

/// The allocation in force, and how long the panes have disagreed with it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LoopPoolState {
    allocation: LoopAllocation,
    /// The demand [`Self::allocation`] was last settled against — a growth
    /// refused by the dead band still moves this, so a declined demand is not
    /// reconsidered on every frame for ever.
    settled_for: LoopDemand,
    pending: Option<(LoopDemand, u32)>,
}

impl LoopPoolState {
    /// Start with nothing looping, which is what a fresh application has.
    pub fn new(pool: LoopPool, model: LoopFrameModel) -> Self {
        let demand = LoopDemand::default();
        Self {
            allocation: pool.plan(model, demand),
            settled_for: demand,
            pending: None,
        }
    }

    /// The allocation in force.
    pub fn allocation(&self) -> LoopAllocation {
        self.allocation
    }

    /// Fold one frame's demand into the allocation.
    pub fn observe(
        &mut self,
        pool: LoopPool,
        model: LoopFrameModel,
        demand: LoopDemand,
    ) -> LoopAllocation {
        if demand == self.settled_for {
            self.pending = None;
            return self.allocation;
        }
        self.pending = match self.pending {
            Some((pending, frames)) if pending == demand => Some((demand, frames + 1)),
            _ => Some((demand, 1)),
        };
        if let Some((demand, frames)) = self.pending
            && frames >= LOOP_POOL_DWELL_FRAMES
        {
            let bare = pool.plan(model, demand);
            if Self::worth_taking(self.allocation, bare) {
                self.allocation = bare;
            }
            self.settled_for = demand;
            self.pending = None;
        }
        self.allocation
    }

    /// Whether `bare` is a change worth re-planning every loop on screen for.
    fn worth_taking(in_force: LoopAllocation, bare: LoopAllocation) -> bool {
        if bare.share_bytes <= in_force.share_bytes {
            return true;
        }
        bare.share_bytes as f64 >= in_force.share_bytes as f64 * LOOP_POOL_HYSTERESIS
    }
}

/// What a previous session learned this machine can hold, if anything.
/// Anything unreadable, or zero, is `None` — the caller falls back to the
/// device classification, which is the same answer a first launch gets.
pub fn remembered(store: Option<&dyn KvStore>, limits: LoopPoolLimits) -> Option<usize> {
    let raw = store?.load(LOOP_POOL_KEY)?;
    let mib: usize = raw.trim().parse().ok().or_else(|| {
        log::warn!("loop pool memo is not a number ({raw:?}); re-probing this device");
        None
    })?;
    let bytes = mib.checked_mul(1024 * 1024)?;
    if bytes == 0 {
        return None;
    }
    Some(limits.hold(bytes))
}

/// Write what this session settled on, synchronously.
pub fn remember(store: Option<&dyn KvStore>, bytes: usize) {
    let Some(store) = store else {
        return;
    };
    let mib = bytes / (1024 * 1024);
    if let Err(e) = store.store_now(LOOP_POOL_KEY, &mib.to_string()) {
        log::warn!("could not persist the loop pool size: {e}");
    }
}

#[cfg(test)]
mod tests;
