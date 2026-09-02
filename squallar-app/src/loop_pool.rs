//! One loop allowance for the whole application, given to the loops that
//! actually want one — each by its own need, and the rest by time.
//!
//! The unit of division is a **loop**, not a pane: two 3D panes orbiting one
//! volume are one resident set in one store, so they are one loop here.
//!
//! The pool's size is the **room** the rest of the scene leaves under the
//! device's capacity, capped at what the loops could ever fill —
//! [`LoopPool::for_scene`], on `squallar_device_profile::fit`'s arithmetic —
//! and held inside the bracket's floor and ceiling. [`LoopPool::plan`] then
//! gives every loop its **base** (what its lookback costs at the fitted rung,
//! which `fit` already made sure fits) and spends what is left as a
//! **balloon**, raising the loops' held frames until their temporal
//! resolution is equal, each stopping at what its listing holds. One pane and
//! six panes share one budget: more panes slice it thinner, and a lone pane
//! holds every scan in its window when the room allows. Nothing about the pool
//! is remembered across sessions, and what pressure teaches lowers the
//! session's capacity presumption rather than the pool itself. `mobile` is a
//! cfg for native Android/iOS: a browser on a phone is `wasm32`, not `mobile`.

use squallar_device_profile::budget::Budgets;
use squallar_device_profile::constants::{
    LOOP_IMAGE_SIZE, LOOP_POOL_CEILING_BYTES, LOOP_POOL_DWELL_FRAMES, LOOP_POOL_FLOOR_BYTES,
    LOOP_POOL_HYSTERESIS, MAX_LOOP_FRAMES, MAX_LOOP_RENDER_BUDGET, MIN_LOOP_FRAMES_PER_PANE,
    RENDER_HEIGHT, RENDER_WIDTH, VOLUME_GRID_CELLS,
};
use squallar_device_profile::fit::{GridBytes, loop_pool_bytes};
use squallar_device_profile::scene::{Capacity, CapacitySource, Scene};
use squallar_radar::types::RenderView;

/// The raymarch's own resident-grid arithmetic — every mip level as the
/// backend lays it out, the colour table's texture, the jitter tile — handed to
/// the floor crate's `need` and `fit`, which price a grid through it rather
/// than re-deriving it. The same function [`LoopFrameModel::from_budgets`]
/// reads, so the pool and the need cannot price one grid two ways.
pub const GRID_BYTES: GridBytes = squallar_volumetric::raymarch::resident_grid_bytes;

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
    /// already refused an allocation. Never goes below this, on either arm.
    pub floor: usize,
    /// The most this target will spend on loop textures **on the presumed
    /// arm**, where nothing has measured the card. A static ceiling on VRAM
    /// is exactly what a measurement retires, so where the capacity is
    /// measured or probed this does not bind and the room `loop_pool_bytes`
    /// already applied is the bound — see [`Self::on`].
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

    /// These bounds on `cap`'s arm. The floor holds everywhere. The ceiling is
    /// a presumption — the most a bracket spends on loops when nothing has
    /// measured the card — so it binds only a presumed capacity; against a
    /// measured or probed one the bound is the room under the allowance,
    /// which `loop_pool_bytes` has already applied, and no bracket constant
    /// caps what a scene needs on a card that was seen to hold it.
    pub fn on(self, cap: &Capacity) -> Self {
        match cap.source {
            CapacitySource::Presumed => self,
            CapacitySource::Measured | CapacitySource::Probed => Self {
                floor: self.floor,
                ceiling: usize::MAX,
            },
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
/// will ever texture or list.
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
    /// This class's `MAX_LOOP_RENDER_BUDGET`: the base a radar loop with no
    /// cadence yet asks for, and the most a radar loop of the fallback kind
    /// is textured to. **Radar's own figure**: it is what `loop_span_secs`
    /// costs at the fastest radar measured, so it is not applied to an
    /// overlay loop, whose cadence is its layer's (an HRRR run is hourly, and
    /// 48 h of it is 49 frames).
    pub render_budget: usize,
    /// This class's `MAX_LOOP_FRAMES`: the most frames any loop **lists**, and
    /// so the most a balloon can ever raise one to — a listing longer than
    /// this is sampled down to it before anything is fetched.
    pub list_cap: usize,
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
            list_cap: MAX_LOOP_FRAMES,
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
            list_cap: budgets.loop_frames_held,
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

    /// What one frame of a loop of `kind` costs on this class, before a pane
    /// has measured its own.
    pub fn price(&self, kind: LoopKind) -> usize {
        match kind {
            LoopKind::PlanView => self.plan_view,
            LoopKind::CrossSection => self.section,
            LoopKind::Volume => self.grid,
            LoopKind::Overlay => self.overlay,
        }
    }

    /// The most frames a loop of `kind` is textured to when nothing more
    /// specific is known about it: radar's three shapes stop at the render
    /// budget, an overlay loop at the list cap (its cadence is its layer's,
    /// and the render budget is radar's figure).
    fn kind_cap(&self, kind: LoopKind) -> usize {
        let cap = match kind {
            LoopKind::PlanView | LoopKind::CrossSection | LoopKind::Volume => self.render_budget,
            LoopKind::Overlay => self.list_cap,
        };
        // `render_budget` could be edited below the minimum, and `clamp`
        // panics on a crossed pair; the floor wins.
        cap.max(MIN_LOOP_FRAMES_PER_PANE)
    }
}

/// Which shape a loop's frames are, and so what one of them costs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum LoopKind {
    /// A radar plan-view raster.
    PlanView,
    /// A radar cross-section raster.
    CrossSection,
    /// A resident voxel grid.
    Volume,
    /// A rasterized overlay texture: any layer that is not radar.
    Overlay,
}

impl LoopKind {
    /// The kind a radar loop of `view` is.
    pub fn of(view: RenderView) -> Self {
        match view {
            RenderView::PlanView => Self::PlanView,
            RenderView::CrossSection => Self::CrossSection,
            RenderView::Volume => Self::Volume,
        }
    }
}

/// **One loop's identity in the pool**: the pane it runs on. A pane runs at
/// most one loop the pool can see — its radar loop, or one overlay loop when
/// radar is off — so the pane index is the whole key. Two 3D panes orbiting
/// one volume are one loop under the first pane's key, and the second is an
/// **alias** of it ([`LoopDemand::alias`]), so both read the same grant while
/// the pool charges the set once. A frame identity shared across panes can
/// join this key later; it is a struct rather than a bare index so that it
/// can.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LoopKey {
    /// The pane the loop runs on.
    pub pane: usize,
}

/// **What one loop asks the pool for.** Built by the pane walk that also
/// describes the scene for `fit`, from what the pane knows this frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LoopNeed {
    /// Which loop this is.
    pub key: LoopKey,
    /// What its frames are, and so what one costs.
    pub kind: LoopKind,
    /// The pane's lookback in seconds — the width the frames are spread over,
    /// which is what the balloon equalises against.
    pub span_secs: u64,
    /// The source's own frame cadence over the window, once its listing has
    /// said; `None` before then.
    pub cadence_secs: Option<u32>,
    /// Bytes one frame of this loop costs: the model's price for radar's
    /// three shapes, the pane's own measured raster for an overlay loop.
    pub frame_bytes: usize,
    /// **The base**: the frames `fit` already made room for — the pane's
    /// lookback at its cadence, held to the fitted rung's span and render
    /// budget (`Budgets::frames_for_span_of`). What `fit`'s need charged, so
    /// the pool can always pay it when the ladder has done its job.
    pub base_frames: usize,
    /// **The most this loop can hold**: every scan the listing named in the
    /// window, or the window at the cadence when no listing has landed, never
    /// more than the class's list cap. A balloon never inflates a loop past
    /// what exists to show.
    pub max_frames: usize,
}

impl LoopNeed {
    /// What this loop holds at its base, once the base is held to what exists
    /// and to the two-frame floor.
    fn held_at_base(&self) -> usize {
        self.base_frames
            .min(self.max_frames)
            .max(MIN_LOOP_FRAMES_PER_PANE)
    }

    /// Bytes this loop charges whatever its frame count: a 3D loop's live grid
    /// lives in the same store as its frames, so its slice carries one grid
    /// beside them — the headroom the shipped 3D frame counts were derived
    /// with. Nothing for the raster kinds.
    fn fixed_bytes(&self) -> usize {
        match self.kind {
            LoopKind::Volume => self.frame_bytes,
            LoopKind::PlanView | LoopKind::CrossSection | LoopKind::Overlay => 0,
        }
    }
}

/// **The most frames a loop can hold**, for [`LoopNeed::max_frames`]: what
/// its listing holds when one has landed (`listed`), else the window at the
/// cadence, else the base — never more than the class's list cap. Pure, so the
/// pane walk and the tests spell it once.
pub fn loop_ceiling_frames(
    listed: Option<usize>,
    span_secs: u64,
    cadence_secs: Option<u32>,
    base_frames: usize,
    list_cap: usize,
) -> usize {
    let known = listed.unwrap_or_else(|| {
        cadence_secs
            .filter(|secs| *secs > 0)
            .map_or(base_frames, |secs| {
                1 + usize::try_from(span_secs / u64::from(secs)).unwrap_or(usize::MAX)
            })
    });
    known.min(list_cap)
}

/// What the loops are asking for, this frame: one [`LoopNeed`] per loop, and
/// which panes read another pane's loop.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LoopDemand {
    needs: Vec<LoopNeed>,
    /// `(pane, owner)`: `pane` orbits the volume `owner`'s loop already holds,
    /// so it reads `owner`'s grant and asks for nothing of its own.
    aliases: Vec<(usize, usize)>,
}

impl LoopDemand {
    /// Fold one more loop in. A second need under a key already present
    /// replaces the first, so a walk that visits a pane twice cannot charge
    /// it twice.
    pub fn push(&mut self, need: LoopNeed) {
        match self.needs.iter_mut().find(|n| n.key == need.key) {
            Some(slot) => *slot = need,
            None => self.needs.push(need),
        }
    }

    /// Record that `pane` shares `owner`'s loop — a second 3D pane on one
    /// volume, which is one resident set in one store and so one loop here.
    pub fn alias(&mut self, pane: usize, owner: usize) {
        if !self.aliases.contains(&(pane, owner)) {
            self.aliases.push((pane, owner));
        }
    }

    /// The loops, in the order the panes were walked.
    pub fn needs(&self) -> &[LoopNeed] {
        &self.needs
    }

    /// How many loops the pool is given to.
    pub fn shares(&self) -> usize {
        self.needs.len()
    }

    /// How many loops of `kind` are asking.
    pub fn count(&self, kind: LoopKind) -> usize {
        self.needs.iter().filter(|n| n.kind == kind).count()
    }
}

/// **What one loop was given**: its need, and the frames it may hold.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LoopGrant {
    /// Which loop.
    pub key: LoopKey,
    /// What its frames are.
    pub kind: LoopKind,
    /// The width the frames are spread over, from the need.
    pub span_secs: u64,
    /// The cadence the need was planned with — `None` when the loop's listing
    /// had not landed, which a consumer reads as "planned before this loop
    /// knew its cadence".
    pub cadence_secs: Option<u32>,
    /// Bytes one frame costs, from the need.
    pub frame_bytes: usize,
    /// The frames the base bought, once held to what exists and to the floor.
    pub base: usize,
    /// The most frames the loop can hold, from the need.
    pub max: usize,
    /// **Frames this loop may hold**: the base plus its share of the
    /// balloon, or less than the base when the pool could not pay every base.
    pub frames: usize,
    /// The bytes charged whatever the frame count — a 3D loop's live grid.
    fixed_bytes: usize,
}

impl LoopGrant {
    /// What this grant charges the pool.
    pub fn bytes(&self) -> usize {
        self.frames
            .saturating_mul(self.frame_bytes)
            .saturating_add(self.fixed_bytes)
    }

    /// The frames' own bytes — what a pane's animating layers divide.
    pub fn frame_bytes_held(&self) -> usize {
        self.frames.saturating_mul(self.frame_bytes)
    }

    /// The bytes above the base — this loop's balloon, 0 when it holds its
    /// base or less.
    pub fn balloon_bytes(&self) -> usize {
        self.frames
            .saturating_sub(self.base)
            .saturating_mul(self.frame_bytes)
    }

    /// Seconds one frame stands for — the temporal resolution the balloon
    /// equalises. Compared as a ratio through [`Self::coarser_than`] so the
    /// decision never rounds.
    fn resolution(&self) -> (u128, u128) {
        (
            u128::from(self.span_secs.max(1)),
            self.frames.max(1) as u128,
        )
    }

    /// Whether this loop's frames stand for more seconds apiece than
    /// `other`'s — exact, by cross-multiplication.
    fn coarser_than(&self, other: &Self) -> bool {
        let (span_a, frames_a) = self.resolution();
        let (span_b, frames_b) = other.resolution();
        span_a * frames_b > span_b * frames_a
    }
}

/// What each loop gets, once the pool has been given out.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LoopAllocation {
    grants: Vec<LoopGrant>,
    aliases: Vec<(usize, usize)>,
    model: LoopFrameModel,
    /// The pool this was planned from.
    pool_bytes: usize,
    /// **Mean bytes one loop holds** — the pool over the loops when every
    /// loop's grant is summed, the whole pool when nothing loops. The `loop
    /// state:` line's `share`; one figure for a division that is no longer
    /// equal, so it is a summary and not any one loop's slice.
    pub share_bytes: usize,
    /// The most frames a plan-view loop holds under this plan, or what one
    /// would hold as the sole loop when none does — the ceiling a plan-view
    /// loop the plan has not seen is held to.
    pub plan_view_frames: usize,
    /// The same for a cross-section loop.
    pub section_frames: usize,
    /// The same for a 3D loop, whose frames are resident grids and whose
    /// frame list is its resident set.
    pub volume_frames: usize,
    /// The same for a loop of a **non-radar** layer, at the class's nominal
    /// overlay frame ([`LoopFrameModel::overlay`]). Not held to the render
    /// budget — see [`LoopFrameModel::kind_cap`] — but to the list cap.
    pub overlay_frames: usize,
    /// The distinct 3D loops this allocation was computed for.
    pub volume_sets: usize,
}

impl LoopAllocation {
    /// Every grant, in the order the panes were walked.
    pub fn grants(&self) -> &[LoopGrant] {
        &self.grants
    }

    /// The grant `pane`'s loop reads — its own, or the one it is an alias of.
    pub fn grant_for_pane(&self, pane: usize) -> Option<&LoopGrant> {
        let owner = self
            .aliases
            .iter()
            .find(|(alias, _)| *alias == pane)
            .map_or(pane, |(_, owner)| *owner);
        self.grants.iter().find(|g| g.key.pane == owner)
    }

    /// Frames `pane`'s loop may hold, or `None` for a pane the plan has not
    /// seen — a loop that started inside the dwell, which the caller holds to
    /// the kind's ceiling ([`Self::frames_for`]) until the plan catches up.
    pub fn frames_for_pane(&self, pane: usize) -> Option<usize> {
        self.grant_for_pane(pane).map(|g| g.frames)
    }

    /// Frames of a loop of `kind` that are ready to show at once, when the
    /// plan names nothing more specific: the kind's ceiling.
    pub fn frames_for_kind(&self, kind: LoopKind) -> usize {
        match kind {
            LoopKind::PlanView => self.plan_view_frames,
            LoopKind::CrossSection => self.section_frames,
            LoopKind::Volume => self.volume_frames,
            LoopKind::Overlay => self.overlay_frames,
        }
    }

    /// [`Self::frames_for_kind`], for a radar loop of `view`.
    pub fn frames_for(&self, view: RenderView) -> usize {
        self.frames_for_kind(LoopKind::of(view))
    }

    /// **Bytes `pane`'s loop holds its frames in** — what its animating
    /// layers divide: the grant's frames at the grant's price, or
    /// [`Self::share_bytes`] for a pane the plan has not seen — the whole
    /// pool on an idle application, one loop's mean beside others — until the
    /// plan catches up within the dwell, which is the figure the equal split
    /// handed such a pane before.
    pub fn share_bytes_for(&self, pane: usize) -> usize {
        self.grant_for_pane(pane)
            .map_or(self.share_bytes, LoopGrant::frame_bytes_held)
    }

    /// What `VolumeStore::enforce_budget` is held to: every 3D loop's grids
    /// and the live grid beside them, which is what each was charged.
    pub fn volume_reserve_bytes(&self) -> usize {
        self.grants
            .iter()
            .filter(|g| g.kind == LoopKind::Volume)
            .fold(0usize, |sum, g| sum.saturating_add(g.bytes()))
    }

    /// What this allocation charges the pool in all.
    pub fn bytes(&self) -> usize {
        self.grants
            .iter()
            .fold(0usize, |sum, g| sum.saturating_add(g.bytes()))
    }

    /// Bytes held above every loop's base — the balloon in force, 0 when
    /// every loop holds its base or less.
    pub fn balloon_bytes(&self) -> usize {
        self.grants
            .iter()
            .fold(0usize, |sum, g| sum.saturating_add(g.balloon_bytes()))
    }

    /// The pool this was planned from.
    pub fn pool_bytes(&self) -> usize {
        self.pool_bytes
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

    /// **The pool a scene leaves for its loops**: the room the rest of the
    /// scene leaves under `cap`'s allowance, capped at what the loops could
    /// ever fill — every looping pane's window at its cadence, at its frame's
    /// cost, no loop past the list cap — and held inside `limits` on `cap`'s
    /// arm ([`LoopPoolLimits::on`]). One two-hour loop on the desktop bracket
    /// is 36 x 16 MiB = 576 MiB whatever card it runs on; six are 3456 MiB,
    /// or the room if that is less — under the 3840 MiB presumption the room
    /// is 2304 MiB, under a measured 3090 it is not the bound and the 3072 MiB
    /// bracket ceiling is not either. The class of the machine is not an
    /// input: the same scene costs the same bytes on every bracket, and what
    /// a bigger card buys is room — which [`Self::plan`] spends on the density
    /// of the window the user asked for, never on a longer one.
    pub fn for_scene(
        scene: &Scene,
        budgets: &Budgets,
        cap: &Capacity,
        limits: LoopPoolLimits,
    ) -> Self {
        let bytes = loop_pool_bytes(scene, budgets, cap, GRID_BYTES);
        Self::new(usize::try_from(bytes).unwrap_or(usize::MAX), limits.on(cap))
    }

    /// The pool, in bytes.
    pub fn bytes(&self) -> usize {
        self.bytes
    }

    /// **Give the pool to the loops that want one: base first, then the
    /// balloon, by time.**
    ///
    /// Every loop is first held at its base — what `fit` made room for, held
    /// to what its listing holds and to the two-frame floor. When the bases
    /// together fit the pool, what is left is spent as a **balloon**: one
    /// frame at a time to whichever growable loop's frames stand for the most
    /// seconds apiece (exact, by cross-multiplication — no float decides),
    /// ties to the earlier pane, each loop stopping at its `max_frames` or
    /// when one more of its frames would not fit. Loops of unequal cost and
    /// equal span therefore reach the same temporal resolution, not the same
    /// bytes; a cheaper frame buys its loop nothing its neighbour does not
    /// get, and a longer window holds proportionally more frames. When the
    /// bases together do **not** fit — the ladder had nothing left to shed,
    /// or the presumed arm's ceiling binds — the same rule runs downward: one
    /// frame at a time from whichever loop's frames stand for the fewest
    /// seconds, none below the floor, until the plan fits or nothing can
    /// shrink. That replaces the equal-bytes split, which gave a section loop
    /// twice a plan-view loop's history for the same lookback and held six
    /// panes to the density one pane earned.
    ///
    /// Deterministic: the same pool, model and demand plan the same grants.
    pub fn plan(&self, model: LoopFrameModel, demand: &LoopDemand) -> LoopAllocation {
        let mut grants: Vec<LoopGrant> = demand
            .needs
            .iter()
            .map(|need| {
                let base = need.held_at_base();
                LoopGrant {
                    key: need.key,
                    kind: need.kind,
                    span_secs: need.span_secs,
                    cadence_secs: need.cadence_secs,
                    frame_bytes: need.frame_bytes,
                    base,
                    max: need.max_frames.max(base),
                    frames: base,
                    fixed_bytes: need.fixed_bytes(),
                }
            })
            .collect();

        let charged = |grants: &[LoopGrant]| {
            grants
                .iter()
                .fold(0usize, |sum, g| sum.saturating_add(g.bytes()))
        };

        if charged(&grants) <= self.bytes {
            // Up: the coarsest growable loop takes the next frame.
            loop {
                let spent = charged(&grants);
                let room = self.bytes - spent;
                let next = grants
                    .iter()
                    .enumerate()
                    .filter(|(_, g)| g.frames < g.max && g.frame_bytes <= room)
                    .fold(None::<usize>, |best, (i, g)| match best {
                        Some(b) if !g.coarser_than(&grants[b]) => Some(b),
                        _ => Some(i),
                    });
                let Some(i) = next else {
                    break;
                };
                grants[i].frames += 1;
            }
        } else {
            // Down: the finest shrinkable loop gives the next frame back.
            while charged(&grants) > self.bytes {
                let next = grants
                    .iter()
                    .enumerate()
                    .filter(|(_, g)| g.frames > MIN_LOOP_FRAMES_PER_PANE)
                    .fold(None::<usize>, |best, (i, g)| match best {
                        Some(b) if !grants[b].coarser_than(g) => Some(b),
                        _ => Some(i),
                    });
                let Some(i) = next else {
                    break;
                };
                grants[i].frames -= 1;
            }
        }

        // The per-kind ceilings: the most any loop of the kind holds, or the
        // single-loop answer where none does — the whole pool as one share,
        // held to the kind's cap, and a 3D loop leaving one grid of headroom
        // for the live grid it keeps beside its frames.
        let ceiling = |kind: LoopKind| {
            grants
                .iter()
                .filter(|g| g.kind == kind)
                .map(|g| g.frames)
                .max()
                .unwrap_or_else(|| {
                    let price = model.price(kind);
                    let budget = match kind {
                        LoopKind::Volume => self.bytes.saturating_sub(price),
                        _ => self.bytes,
                    };
                    // A frame that costs nothing is a model built wrong; the
                    // cap answers rather than a division by zero.
                    let cap = model.kind_cap(kind);
                    budget
                        .checked_div(price)
                        .unwrap_or(cap)
                        .clamp(MIN_LOOP_FRAMES_PER_PANE, cap)
                })
        };
        let share_bytes = if grants.is_empty() {
            self.bytes
        } else {
            charged(&grants) / grants.len()
        };
        LoopAllocation {
            plan_view_frames: ceiling(LoopKind::PlanView),
            section_frames: ceiling(LoopKind::CrossSection),
            volume_frames: ceiling(LoopKind::Volume),
            overlay_frames: ceiling(LoopKind::Overlay),
            volume_sets: demand.count(LoopKind::Volume),
            share_bytes,
            grants,
            aliases: demand.aliases.clone(),
            model,
            pool_bytes: self.bytes,
        }
    }
}

/// What an allocation was planned against: the pool, the frame model and the
/// demand. A change in any of the three is a change the dwell and the dead
/// band answer — the pool moves when the scene's need or its room does, the
/// model when the budgets re-fit, the demand when a loop starts or stops, or
/// when a listing lands and says what a loop can hold.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Planned {
    pool: LoopPool,
    model: LoopFrameModel,
    demand: LoopDemand,
}

/// The allocation in force, and how long the panes have disagreed with it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LoopPoolState {
    allocation: LoopAllocation,
    /// What [`Self::allocation`] was last settled against — a growth refused
    /// by the dead band still moves this, so a declined ask is not
    /// reconsidered on every frame for ever.
    settled_for: Planned,
    pending: Option<(Planned, u32)>,
}

impl LoopPoolState {
    /// Start with nothing looping, which is what a fresh application has.
    pub fn new(pool: LoopPool, model: LoopFrameModel) -> Self {
        let planned = Planned {
            pool,
            model,
            demand: LoopDemand::default(),
        };
        Self {
            allocation: pool.plan(model, &planned.demand),
            settled_for: planned,
            pending: None,
        }
    }

    /// The allocation in force.
    pub fn allocation(&self) -> &LoopAllocation {
        &self.allocation
    }

    /// Fold one frame's pool, model and demand into the allocation.
    pub fn observe(
        &mut self,
        pool: LoopPool,
        model: LoopFrameModel,
        demand: LoopDemand,
    ) -> &LoopAllocation {
        let asked = Planned {
            pool,
            model,
            demand,
        };
        if asked == self.settled_for {
            self.pending = None;
            return &self.allocation;
        }
        self.pending = match self.pending.take() {
            Some((pending, frames)) if pending == asked => Some((asked, frames + 1)),
            _ => Some((asked, 1)),
        };
        if let Some((asked, frames)) = self.pending.take() {
            if frames >= LOOP_POOL_DWELL_FRAMES {
                let bare = asked.pool.plan(asked.model, &asked.demand);
                if Self::worth_taking(&self.allocation, &bare) {
                    self.allocation = bare;
                }
                self.settled_for = asked;
            } else {
                self.pending = Some((asked, frames));
            }
        }
        &self.allocation
    }

    /// Whether `bare` is a change worth re-planning every loop on screen for:
    /// a loop that started (it needs a grant of its own), any loop or kind
    /// ceiling that shrinks, or one that grows by the dead band's ratio **in
    /// frames** — the thing a re-plan actually changes on screen. A loop that
    /// stopped is not by itself a reason: its grant lingers, charging nothing
    /// real, until a change worth taking arrives — the dead band's whole
    /// point is that five loops are not re-sampled denser for a 1.2x gain the
    /// moment a sixth closes.
    fn worth_taking(in_force: &LoopAllocation, bare: &LoopAllocation) -> bool {
        let started = bare
            .grants
            .iter()
            .any(|g| in_force.grant_for_pane(g.key.pane).is_none())
            || bare
                .aliases
                .iter()
                .any(|alias| !in_force.aliases.contains(alias));
        if started {
            return true;
        }
        let kinds = [
            LoopKind::PlanView,
            LoopKind::CrossSection,
            LoopKind::Volume,
            LoopKind::Overlay,
        ];
        let pairs = bare
            .grants
            .iter()
            .filter_map(|now| {
                in_force
                    .grants
                    .iter()
                    .find(|was| was.key == now.key)
                    .map(|was| (was.frames, now.frames))
            })
            .chain(
                kinds
                    .iter()
                    .map(|kind| (in_force.frames_for_kind(*kind), bare.frames_for_kind(*kind))),
            )
            .collect::<Vec<_>>();
        pairs.iter().any(|(was, now)| now < was)
            || pairs
                .iter()
                .any(|(was, now)| *now as f64 >= *was as f64 * LOOP_POOL_HYSTERESIS)
    }
}

#[cfg(test)]
mod tests;
