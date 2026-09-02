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

use squallar_device_profile::budget::{Budgets, Promotion};
use squallar_device_profile::constants::{
    LOOP_IMAGE_SIZE, LOOP_POOL_CEILING_BYTES, LOOP_POOL_DWELL_FRAMES, LOOP_POOL_FLOOR_BYTES,
    LOOP_POOL_HYSTERESIS, MAX_LOOP_RENDER_BUDGET, MIN_LOOP_FRAMES_PER_PANE, RENDER_HEIGHT,
    RENDER_WIDTH, VOLUME_GRID_CELLS,
};
use squallar_device_profile::quality::DeviceClass;
use squallar_radar::types::RenderView;

/// The key a stale pool size sits under in an older install's store, in MiB.
/// Never read, never written: the pool is sized from the device class at every
/// launch, and what a lost surface teaches it lives for this process only. Kept
/// because the store has no delete, so the entry is named rather than
/// mysterious.
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

/// **Bytes one overlay loop frame costs, before any pane has measured its
/// own.**
///
/// An overlay frame has no fixed side the way a radar loop frame does. It is
/// the pane's own raster, planned from the pane rect and the overdraw margin
/// by [`plan_overlay_texture`], so the pre-measurement figure is that same
/// planner run on the default window ([`RENDER_WIDTH`] × [`RENDER_HEIGHT`]) —
/// the largest pane a single-pane layout gives. Those two are **physical
/// pixels**, and the planner's own input is `screen_rect × pixels_per_point`,
/// also physical pixels, so a density of 1.0 here is the whole window and not
/// a 1x assumption about the display. A pane that has rasterized once is
/// priced off the texture it is actually drawing with instead
/// (`app_render::overlay_frame_bytes`).
///
/// 1920 × 1.5 = 2880 and 1080 × 1.5 = 1620 at the full `OVERDRAW_FRACTION`, so
/// a frame is **18,662,400 B (18.66 MB)** — against a radar plan-view frame's
/// 4 MiB on wasm and 16 MiB on both native arms.
///
/// **No device-class ceiling is applied, and that is the correction.** The
/// planner's only clamp is `InputState::max_texture_side`, which
/// `EguiRenderer::new` fills from `device.limits().max_texture_dimension_2d`;
/// on the web `squallar_gpu::device::device_limits` is
/// `downlevel_webgl2_defaults().using_resolution(adapter)`, and
/// `using_resolution` copies the adapter's 2D resolution **verbatim**. Nothing
/// on the overlay path is ever held to `raster_side_ceiling_px` — that is the
/// cap on *static plan-view radar* rasters, which must upload on every browser
/// — and WebGL2's 2048 is a guarantee the adapter is at **least** that, not a
/// cap that it is at most that. Firefox reports 32768 on a real driver.
///
/// Planning this against the wasm arm's 2048 was therefore a **1.98×
/// under-price on every browser whose adapter clears 2880 px**, which is every
/// browser on a real driver and both software rasterisers besides:
/// 2048×1152×4 = 9,437,184 B charged for a texture that is really 18,662,400 B.
/// The two native arms were already right, because 4096 and 8192 both afford
/// the whole margin and clamp nothing.
///
/// An adapter that really does stop at 2048 is now over-priced by that same
/// factor. That costs it loop frames rather than over-committing its memory,
/// and it is the only side of the error a budget resolved without probing the
/// machine may be on.
///
/// [`plan_overlay_texture`]: squallar_egui::overlay_cache::plan_overlay_texture
/// [`OVERDRAW_FRACTION`]: squallar_egui::overlay_cache::OVERDRAW_FRACTION
/// [`RENDER_WIDTH`]: squallar_device_profile::constants::RENDER_WIDTH
/// [`RENDER_HEIGHT`]: squallar_device_profile::constants::RENDER_HEIGHT
pub fn nominal_overlay_frame_bytes() -> usize {
    let plan = squallar_egui::overlay_cache::plan_overlay_texture(
        egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(RENDER_WIDTH as f32, RENDER_HEIGHT as f32),
        ),
        // Not a class ceiling. The largest side any adapter could impose, so
        // the margin is the full `OVERDRAW_FRACTION` and the answer is the
        // maximum over every adapter rather than one adapter's.
        u32::MAX,
        1.0,
    );
    plan.width as usize * plan.height as usize * 4
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
    /// **One frame of a loop of any layer that is not radar** — a model field,
    /// a satellite band — which is a rasterized overlay texture and not one of
    /// radar's three shapes. See [`nominal_overlay_frame_bytes`].
    pub overlay: usize,
    /// This class's `MAX_LOOP_RENDER_BUDGET`: no share buys more frames
    /// than the dispatcher would texture. **Radar's own figure**: it is what
    /// `loop_span_secs` costs at the fastest radar measured, so it is not
    /// applied to an overlay loop, whose cadence is its layer's (an HRRR run
    /// is hourly, and 48 h of it is 49 frames).
    pub render_budget: usize,
}

impl LoopFrameModel {
    /// The compiled target's figures.
    pub fn for_target() -> Self {
        let side = LOOP_IMAGE_SIZE;
        Self {
            plan_view: side * side * 4,
            section: squallar_radar::xsect::SECTION_WIDTH
                * squallar_radar::xsect::SECTION_HEIGHT
                * 4,
            grid: squallar_volumetric::raymarch::resident_grid_bytes(VOLUME_GRID_CELLS)
                .unwrap_or(usize::MAX),
            overlay: nominal_overlay_frame_bytes(),
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
            grid: squallar_volumetric::raymarch::resident_grid_bytes(budgets.grid_cells)
                .unwrap_or(usize::MAX),
            overlay: nominal_overlay_frame_bytes(),
            render_budget: budgets.loop_render_budget,
        }
    }

    /// **What one frame of a radar loop of `view` costs.** The three arms are
    /// radar's own shapes; every other layer's frame is [`Self::overlay`].
    pub fn bytes_for(&self, view: RenderView) -> usize {
        match view {
            RenderView::PlanView => self.plan_view,
            RenderView::CrossSection => self.section,
            RenderView::Volume => self.grid,
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
    /// **Panes looping a layer that is not radar, and no radar loop of their
    /// own.**
    ///
    /// A share is a *pane's* slice, not a layer's: a pane already counted by
    /// one of the three arms above is not counted again for the model field it
    /// is animating beside its radar, because `app_render::layer_share`
    /// divides that one share across every layer the pane animates. This arm
    /// exists for the pane those three cannot see at all — a radar-off pane
    /// looping a forecast or a satellite band, which before WB-7 asked the
    /// pool for nothing and was then handed a share sized as though it were
    /// the only thing running.
    pub overlay_loops: usize,
}

impl LoopDemand {
    /// How many ways the pool is split.
    pub fn shares(&self) -> usize {
        self.plan_view_loops + self.section_loops + self.volume_sets + self.overlay_loops
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

    /// Fold in one pane whose loops are all a layer other than radar's.
    /// See [`Self::overlay_loops`] for why it is per pane and not per layer.
    pub fn add_overlay_pane(&mut self) {
        self.overlay_loops += 1;
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
    /// Textured frames a loop of a **non-radar** layer may keep, at the device
    /// class's nominal overlay frame ([`LoopFrameModel::overlay`]). A pane that
    /// has already rasterized the layer is priced off the texture it is really
    /// drawing with — [`Self::frames_at`] takes that measurement — and this is
    /// the pool's own answer before one exists.
    ///
    /// Unlike the three radar arms it is **not** capped by
    /// [`LoopFrameModel::render_budget`]: that figure is what radar's
    /// `loop_span_secs` costs at the fastest radar measured, and an overlay
    /// loop's cadence is its own layer's.
    pub overlay_frames: usize,
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
            + demand.overlay_loops * self.overlay_frames * model.overlay
    }

    /// **Frames one share buys at a measured `frame_bytes` apiece** — the count
    /// a layer gets when it is the only thing its pane animates.
    ///
    /// Floored at [`MIN_LOOP_FRAMES_PER_PANE`], deliberately and in the open:
    /// two frames is where a loop stops being a loop, and it is the same floor
    /// [`LoopPool::plan`] applies to every other arm. **On a share that does
    /// not buy two frames the floor wins and the byte bound is exceeded** — one
    /// frame over, by construction, and stating it is the alternative to an
    /// animation that cannot animate.
    ///
    /// [`MIN_LOOP_FRAMES_PER_PANE`]: squallar_device_profile::constants::MIN_LOOP_FRAMES_PER_PANE
    pub fn frames_at(&self, frame_bytes: usize) -> usize {
        self.share_bytes
            .checked_div(frame_bytes)
            .unwrap_or(MIN_LOOP_FRAMES_PER_PANE)
            .max(MIN_LOOP_FRAMES_PER_PANE)
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
        let mut allocation = LoopAllocation {
            share_bytes,
            plan_view_frames: frames(share_bytes, model.plan_view),
            section_frames: frames(share_bytes, model.section),
            volume_frames,
            overlay_frames: MIN_LOOP_FRAMES_PER_PANE,
            volume_sets: demand.volume_sets,
        };
        // Bytes and the floor, and no `render_budget` — see
        // `LoopAllocation::overlay_frames`. Through `frames_at` rather than
        // spelled again here, so the pool's own answer and a pane's measured
        // one cannot come out of two different divisions.
        allocation.overlay_frames = allocation.frames_at(model.overlay);
        allocation
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

#[cfg(test)]
mod tests;
