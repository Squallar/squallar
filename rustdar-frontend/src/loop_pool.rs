//! One loop allowance for the whole application, divided among the loops that
//! actually want one.
//!
//! # What this replaces
//!
//! [`crate::constants::LOOP_POOL_FLOOR_BYTES`] used to be called
//! `LOOP_TEXTURE_BUDGET_BYTES` and it was a **per-pane** allowance: 512 MiB on
//! desktop, 256 on mobile, 48 in a browser, and nothing anywhere multiplied it
//! by the pane count. `MAX_PANES_DESKTOP` is 6 and `MAX_PANES_MOBILE` is 4, so
//! the reachable totals were **3.0 GiB on desktop and 1.0 GiB on a phone** —
//! four panes and a loop toggle. The two halves of that multiplication live in
//! different crates (`MAX_PANES_*` is `rustdar_egui::pane`'s), which is why no
//! test put them side by side until `APP_TEXTURE_BUDGET_BYTES` was written down.
//!
//! So the allowance is now one pool, and the panes that want loops divide it.
//! Adding a pane shortens every loop; closing one gives the length back.
//!
//! # The unit of division is a **loop**, not a pane
//!
//! That distinction is the whole reason a naive per-pane split would be wrong.
//! A plan-view or cross-section loop caches *pictures*, and two panes showing
//! the same site at the same product each cache their own — so they are two
//! loops and cost two shares. A 3D loop caches **inputs**: its frames are
//! resident voxel grids in the single application-wide
//! [`crate::volume::bridge::VolumeStore`], keyed by `VolumeTarget`, so two 3D
//! panes orbiting one volume from two angles already share one build, one
//! upload and one set. They are **one** loop and cost **one** share.
//!
//! [`LoopDemand::volume_sets`] is therefore a count of *distinct volume loop
//! keys*, never of panes, and `the_3d_set_is_not_double_counted_across_two_panes`
//! is what says so.
//!
//! # Degrade smoothly, never cliff
//!
//! Every share is floored at [`MIN_LOOP_FRAMES_PER_PANE`], so a pane arriving
//! makes its neighbours' loops *shorter* and can never blank one. That floor is
//! only reachable at all because [`crate::constants::LOOP_POOL_FLOOR_BYTES`] is
//! chosen to seat every pane the width class admits at it — see
//! `the_floor_seats_every_pane_without_blanking_one`.
//!
//! # Do not thrash
//!
//! A pane appearing and disappearing must not re-fetch and re-render the world
//! each time, so the demand passes through [`LoopPoolState`], which is the same
//! shape as `crate::egui_renderer::MirrorRungs`: a dwell of
//! [`LOOP_POOL_DWELL_FRAMES`] before any change is taken at all, and a dead band
//! of [`LOOP_POOL_HYSTERESIS`] on the direction that is optional.
//!
//! # The size of the pool is discovered, not compiled in
//!
//! A single per-target figure is the wrong instrument: any number safe on a
//! budget phone throws away most of a flagship's headroom, and any number that
//! suits a flagship kills the app on the low end. So the per-target constants
//! are a **floor and a ceiling**, and the value between them comes from the
//! device — [`LoopPool::for_device`], which is `MirrorLimits::for_device`'s idea
//! applied to bytes instead of to a texture side, and `quality::select`'s shape
//! applied to memory instead of to a shading rung.
//!
//! What can actually be asked, per backend, is less than one would hope, and it
//! is worth writing down because the next person to look will assume otherwise:
//!
//! * **wgpu 29.0.4 exposes no memory capacity on any backend.**
//!   `Device::generate_allocator_report` reports what *we* have allocated, not
//!   what the device has (`wgpu-29.0.4/src/api/device.rs:542`).
//!   `VK_EXT_memory_budget` **is** read by wgpu-hal — `heap_budget` and
//!   `heap_usage` at `wgpu-hal-29.0.4/src/vulkan/device.rs:859-860` and
//!   `:2718-2719` — but only to *refuse* an allocation past a percentage the
//!   application sets through `wgt::MemoryBudgetThresholds`. The figures are
//!   never handed back.
//! * **`AdapterInfo::device_type` is queryable and is the one real signal.**
//!   `crate::volume::quality::DeviceClass::from_device_type` already classifies
//!   it and is reused here rather than a parallel enum being invented.
//! * **WebGL2 reports nothing at all.** Every browser is
//!   [`DeviceClass::Unknown`] whatever the silicon is (see that variant's doc),
//!   so the browser arm sits at its floor and can only ever back *off*.
//!
//! # `mobile` is a cfg, not a device class, and the browser is where that bites
//!
//! `mobile` is emitted by this crate's `build.rs` for native Android and iOS.
//! **A browser on a phone is not `mobile`** — the shipped PWA is
//! `target_arch = "wasm32"`, the same arm as a browser on a workstation, out of
//! one binary served to both. The tightest real target this application has and
//! the roomiest browser target are indistinguishable at compile time.
//!
//! So the honest taxonomy is: the `cfg` tells you which **APIs exist**, and the
//! device class, discovered at runtime, tells you what the machine **is**. That
//! is why every arm here is a floor and a ceiling around a runtime value rather
//! than one number — and on wasm32 it is not merely nicer, it is the only way to
//! target the arm at all.
//!
//! Today the browser takes the floor, which is the right number for a phone and
//! a conservative one for a workstation. Raising the workstation browser needs a
//! signal that is not `AdapterInfo`, and the shortlist is shorter than it looks:
//!
//! * **`matchMedia('(pointer: coarse)')` with `(any-pointer: fine)`** is the
//!   only device-class signal stock Chromium, Safari and Firefox all implement
//!   unclamped, Baseline since 2018. A handheld is coarse-and-not-also-fine; a
//!   touchscreen laptop is both, which is exactly the case a naive `coarse` test
//!   gets wrong. `navigator.maxTouchPoints` is the tiebreak — WebKit hard-codes
//!   5 on iOS/iPadOS and 0 on macOS.
//! * **Screen area is not a class signal**, however tempting. Phones run a
//!   device pixel ratio of 3, so their physical pixel counts *exceed* a
//!   workstation's: 1080p desktop 2.07 Mpx, Pixel 10 2.62, iPhone 16 Pro Max
//!   3.79. Ranking by pixels puts a flagship phone above a laptop with an 8 GiB
//!   discrete GPU behind it. It is a good *rendering-cost* term and a bad
//!   classifier, and the distinction is the whole reason to say so here.
//! * **`navigator.deviceMemory` refines only on Chromium.** WebKit filed a
//!   formal oppose position in April 2026, so it will never exist in Safari, and
//!   Chrome 147 recut the buckets to `{1,2,4,8}` on Android — a 16 GiB flagship
//!   and an 8 GiB midrange are now the same value. `hardwareConcurrency` is
//!   worse: Safari's is a two-valued function of 4 or 8, and returns a
//!   pseudorandom 1..63 to a tracker-classified script under the fingerprinting
//!   protection that is on by default from iOS 26.
//!
//! All of these are *browser* facts, so they would arrive through `rustdar-web`
//! and the platform bridge rather than through wgpu, and all of them feed this
//! same [`LoopPool::for_device`] seam. Nothing else has to change when they do —
//! which is the point of the seam.
//!
//! And every one of them is spoofable in a line of JavaScript, so none is a
//! bound. **The learn-from-failure path is the real backstop**, which is what
//! [`LoopPool::back_off`] and the memo below are for.
//!
//! # The case no signal can see, which is why the memo matters
//!
//! An **installed iOS Home Screen PWA** is the tightest thing this application
//! ships, and an iPhone in Safari and the same iPhone in a Home Screen PWA are
//! identical to every signal above.
//!
//! It really is a different process: WebKit checks
//! `applicationBundleIsEqualTo("com.apple.webapp")`, and auxiliary processes are
//! namespaced to the host bundle, so a PWA gets its own WebContent, Networking
//! and GPU processes rather than sharing Safari's. The background lifecycle is
//! unchanged at HEAD and is harsher than a desktop's by construction —
//! background process assertions time out after 30 s on iOS and not at all on
//! macOS, suspension follows at 20 s, all assertions are dropped at 4 minutes,
//! and `BoostedJetsam` is taken only under `PLATFORM(MAC)`. WebKit's own
//! statement in May 2026 is that nothing in iOS 26 changed memory accounting or
//! budgets for WebKit, and jetsam kills are still being filed against an
//! iPhone 17 Pro.
//!
//! What is **not** established either way is whether a PWA's *memory limit*
//! differs from a Safari tab's — no primary source says so, and the widely
//! repeated ~200 MB figure appears in none. The figure that is sourced is
//! WebKit's own: ~1.5 GB for `WebContent` on most iPhones. The 56 MiB floor is
//! under 4 % of that, which is the margin this arm is entitled to given that it cannot
//! measure, cannot predict, and cannot recover without taking every other tab
//! with it.
//!
//! # Behaviour is the fallback where nothing can be queried
//!
//! [`LoopPool::back_off`] halves the pool toward the floor. It is driven from
//! the same two signals `crate::volume::degrade` already latches — an
//! uncaptured device error and a lost surface — because those are the only
//! evidence a browser will ever give that an allocation was too large. Starting
//! conservative and stepping down on refusal is the honest shape when the
//! capacity cannot be read.
//!
//! # What is discovered is remembered
//!
//! Under [`LOOP_POOL_KEY`], its own `KvStore` entry, written synchronously,
//! encoded as a decimal count of MiB. Not a field on `UiConfig`, for the reason
//! `crate::site_positions` and `crate::location_permission::LOCATION_MEMO_KEY`
//! both give: `autosave_config` writes that blob on a 3 s timer behind a string
//! compare, so a value learned in the last three seconds of a session is lost —
//! and, much worse, one unreadable field in it costs *every* setting on the next
//! load. A back-off learned by crashing the GPU is exactly the value that must
//! not be lost, and the blast radius of a corrupt entry here is one integer.
//!
//! It is also what keeps reopening 1:1: without it, the second launch would
//! re-probe, and a user who had backed off would see a different loop length
//! every start.

use crate::budget::{Budgets, Promotion};
use crate::constants::{
    LOOP_IMAGE_SIZE, LOOP_POOL_CEILING_BYTES, LOOP_POOL_DWELL_FRAMES, LOOP_POOL_FLOOR_BYTES,
    LOOP_POOL_HYSTERESIS, MAX_LOOP_RENDER_BUDGET, MIN_LOOP_FRAMES_PER_PANE, VOLUME_GRID_CELLS,
};
use crate::volume::quality::DeviceClass;
use rustdar_kv::KvStore;
use rustdar_radar::types::RenderView;

/// Key the discovered pool size is persisted under.
///
/// Its own entry rather than a `UiConfig` field — see the module doc.
pub const LOOP_POOL_KEY: &str = "loop_pool";

/// The bounds this target holds a discovered pool between.
///
/// The pair is deliberately asymmetric in kind, exactly as
/// `crate::egui_renderer::MirrorLimits` is: the floor is a **decision** about
/// the worst device this target is willing to work on, and the value between
/// floor and ceiling is a **measurement**. The ceiling is what stops a
/// misread — or a device that lies — from claiming the whole GPU.
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
    ///
    /// The convenience arm over [`from_budgets`](Self::from_budgets), kept for
    /// the tests and the const contexts that want this build's own pair. The
    /// application takes the other one.
    pub const fn for_target() -> Self {
        Self {
            floor: LOOP_POOL_FLOOR_BYTES,
            ceiling: LOOP_POOL_CEILING_BYTES,
        }
    }

    /// The bounds a resolved [`Budgets`] carries.
    ///
    /// The pool is the one budget that leaves `budget::resolve` as a *pair*
    /// rather than as one figure, because it already has a runtime resolution
    /// (`LoopPool::for_device`) and a back-off path (`LoopPool::back_off`) of
    /// its own. This is where the two meet.
    pub const fn from_budgets(budgets: &Budgets) -> Self {
        Self {
            floor: budgets.loop_pool_floor_bytes,
            ceiling: budgets.loop_pool_ceiling_bytes,
        }
    }

    /// `bytes` held between the two, with the floor winning a crossed pair.
    ///
    /// The `max` is not defensive tidiness: `clamp` *panics* on `min > max`,
    /// and these are two independently editable constants in a file where a
    /// panic would be at startup on one target only.
    fn hold(&self, bytes: usize) -> usize {
        bytes.clamp(self.floor, self.ceiling.max(self.floor))
    }
}

/// What one loop frame costs on this device class, and how many the dispatcher
/// will ever texture.
///
/// A parameter rather than the `cfg`-selected constants read inline, for the
/// reason `quality::select` takes its ceiling as one: this workspace runs
/// `cargo test` on exactly one of three arms, and every row of the budget
/// tables has to be reachable from that one host build.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LoopFrameModel {
    /// A [`LOOP_IMAGE_SIZE`]² RGBA raster.
    ///
    /// **Not `IMAGE_SIZE`**, and the difference is not cosmetic: a loop frame
    /// is rendered at `LOOP_IMAGE_SIZE`, which on the web is 1024 against a static
    /// render's 2048. Modelling a browser's loop frame at the static size would
    /// overstate it fourfold — 16 MiB against the real 4 MiB — and the
    /// division would hand every browser loop the
    /// [`MIN_LOOP_FRAMES_PER_PANE`] floor and then blow through the pool
    /// anyway, which is what `the_pool_actually_bounds_the_sum` sees.
    pub plan_view: usize,
    /// `SECTION_WIDTH × SECTION_HEIGHT` RGBA — exactly half the above, by
    /// construction rather than coincidence: `xsect` pins the section width per
    /// target at the same figure [`LOOP_IMAGE_SIZE`] takes, and the height is
    /// half of it. See `an_equal_share_buys_a_section_loop_more_history`.
    pub section: usize,
    /// One resident voxel grid with its mips and its colour table.
    pub grid: usize,
    /// This class's `MAX_LOOP_RENDER_BUDGET`: no share ever buys more frames
    /// than the dispatcher would texture, because the surplus would be memory
    /// spent on frames `evict_textures_outside_render_set` strips every pass.
    pub render_budget: usize,
}

impl LoopFrameModel {
    /// The compiled target's figures.
    ///
    /// The convenience arm over [`from_budgets`](Self::from_budgets), kept for
    /// the tests that want this build's own row. The application takes the
    /// other one.
    pub fn for_target() -> Self {
        let side = LOOP_IMAGE_SIZE;
        Self {
            plan_view: side * side * 4,
            section: rustdar_radar::xsect::SECTION_WIDTH * rustdar_radar::xsect::SECTION_HEIGHT * 4,
            grid: crate::volume::raymarch::resident_grid_bytes(VOLUME_GRID_CELLS)
                .unwrap_or(usize::MAX),
            render_budget: MAX_LOOP_RENDER_BUDGET,
        }
    }

    /// The figures a resolved [`Budgets`] carries.
    ///
    /// The section width is the budget's own rather than
    /// `rustdar_radar::xsect::SECTION_WIDTH` read inline, so a row that is not
    /// the compiled arm's is expressible — which is the whole reason the model
    /// is a parameter in the first place.
    pub fn from_budgets(budgets: &Budgets) -> Self {
        Self {
            plan_view: budgets.loop_frame_bytes(),
            section: budgets.section_frame_bytes(),
            grid: budgets.volume_bytes().unwrap_or(usize::MAX),
            render_budget: budgets.loop_render_budget,
        }
    }
}

/// What the panes are asking for, this frame.
///
/// Counted in **loops**, not panes. See the module doc for why the third field
/// is the odd one out.
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
    ///
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
    /// Textured frames a cross-section loop may keep. Larger than
    /// [`Self::plan_view_frames`] at the same share, because a section frame is
    /// half the size — the pool is bytes, and a cheaper frame buys more of them.
    pub section_frames: usize,
    /// Resident grids **one** 3D loop may keep, which for that kind is also its
    /// whole frame list. See [`Self::volume_reserve_bytes`].
    pub volume_frames: usize,
    /// The distinct 3D loops this allocation was computed for.
    pub volume_sets: usize,
}

impl LoopAllocation {
    /// Frames of a loop of this view that are ready to show at once.
    ///
    /// The dispatcher, the texture eviction and the readiness check all read
    /// this, for the reason `LoopPlaybackState::render_set_indices`' doc gives:
    /// a set the dispatcher fills and a set readiness waits on that could
    /// differ is a loop that plays over frames nothing rendered.
    pub fn frames_for(&self, view: RenderView) -> usize {
        match view {
            RenderView::PlanView => self.plan_view_frames,
            RenderView::CrossSection => self.section_frames,
            RenderView::Volume => self.volume_frames,
        }
    }

    /// What `VolumeStore::enforce_budget` is held to.
    ///
    /// One share per **distinct** 3D loop, and zero when none is running — a
    /// session with no 3D loop hands the store a bound of 0 for its *loop* sets,
    /// which is right, because it holds none.
    ///
    /// The store is one for the whole application and its eviction is
    /// oldest-first, so this has to be the sum across sets rather than a
    /// per-set figure: two distinct 3D loops are two sets in one store, and a
    /// bound that named only one would evict the older loop's frames for ever.
    pub fn volume_reserve_bytes(&self) -> usize {
        self.share_bytes * self.volume_sets
    }

    /// What this allocation costs if every loop in `demand` fills it.
    ///
    /// The figure `the_pool_actually_bounds_the_sum` measures.
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
    ///
    /// `remembered` is what a previous session learned (see [`remembered`]) and
    /// wins outright when present: a value arrived at by watching this machine
    /// refuse an allocation is better evidence than any classification, and
    /// honouring it is also what keeps a reopen 1:1 rather than showing a
    /// different loop length on every start.
    ///
    /// With nothing remembered the class decides, and the reasoning is the
    /// reasoning `DeviceClass`' own doc gives for quality:
    ///
    /// * **`Discrete`** has memory of its own that nothing else is competing
    ///   for, so it takes the ceiling. That ceiling is exactly what this
    ///   application could already reach before the pool existed, so a desktop
    ///   with the memory loses nothing at all.
    /// * **`Integrated`** shares one pool of DRAM with the operating system and
    ///   every other application, so it takes one doubling above the floor and
    ///   no more. A phone is here whenever the driver names itself.
    /// * **`Virtual`** and **`Unknown`** are unknown quantities that could be
    ///   either, and `Unknown` is **every browser** — WebGL2 exposes no device
    ///   type at all. They take the floor. Guessing high on the one target that
    ///   answers exhaustion by destroying the rendering context is the wrong
    ///   way round.
    /// * **`Software`** takes the floor for the same reason it takes the bottom
    ///   of the quality ladder.
    pub fn for_device(
        class: DeviceClass,
        remembered: Option<usize>,
        limits: LoopPoolLimits,
    ) -> Self {
        Self::for_promotion(Promotion::for_class(class), remembered, limits)
    }

    /// The same, at the [`Promotion`] the whole budget set was resolved at.
    ///
    /// **The arm the application takes, and the one that fixes the browser.**
    /// The class is the only signal [`Self::for_device`] has, and on the web it
    /// says `Unknown` whatever the silicon is — so a desktop browser and a
    /// phone browser both took the floor, which is one half of the complaint
    /// this stage answers. `crate::budget::DeviceProfile::promotion` folds the
    /// adapter's own reported ceilings in beside the class, so a browser that
    /// reports desktop-class figures reaches this ceiling on exactly the rung
    /// every other budget it gets was resolved at, and one that does not is
    /// left where it was.
    ///
    /// Nothing native moves: `Promotion::for_class` reproduces the arms above
    /// exactly, so a `Discrete` adapter took the ceiling before and takes it
    /// now, and an `Integrated` one took a doubling and takes a doubling.
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
    ///
    /// Halving rather than dropping to the floor, so a machine that is one step
    /// too ambitious does not lose its whole loop over one event; and never
    /// below the floor, which is the size this target is willing to work at
    /// whatever happens. `false` means the pool was already at the floor, which
    /// is the caller's cue not to write the same value to the config store
    /// again.
    ///
    /// One-way, like `crate::volume::degrade`'s counters and for the same
    /// reason: a device that could not serve a texture will not be able to
    /// serve it after a restart either.
    pub fn back_off(&mut self, limits: LoopPoolLimits) -> bool {
        let reduced = limits.hold(self.bytes / 2);
        if reduced >= self.bytes {
            return false;
        }
        self.bytes = reduced;
        true
    }

    /// Divide the pool among the loops that want one.
    ///
    /// Equal shares by **bytes**, so a cross-section loop — whose frames are
    /// half the size — comes out of an equal share with twice the history, and
    /// a 3D loop with as much as its grids allow.
    ///
    /// Two properties of the 3D row are load-bearing and neither is an
    /// afterthought:
    ///
    /// * **One grid is subtracted before the division into frames.** A live 3D
    ///   pane beside a looping one is one more grid in the same store, and
    ///   `enforce_budget` evicts *oldest first* — so what would go is the loop's
    ///   own frame 0, which the dispatcher re-plans on the next pass, at ~89 ms
    ///   of resample, for ever. `a_full_3d_loop_leaves_room_for_a_live_grid_
    ///   beside_it` is that property; this subtraction is what makes it hold at
    ///   *every* pool size rather than at the one a constant was tuned for.
    /// * **The 3D row is capped at `render_budget` like the others.** A 3D loop
    ///   is not licensed to hold more history than the plan-view loop beside it
    ///   on the same device merely because its grids happen to be cheaper there,
    ///   which is what binds the browser and mobile arms.
    pub fn plan(&self, model: LoopFrameModel, demand: LoopDemand) -> LoopAllocation {
        let share_bytes = self.bytes / demand.shares().max(1);
        // `render_budget` could in principle be edited below the minimum, and a
        // `clamp` with a crossed pair panics. The floor wins, because a loop
        // too short to be a loop is the failure this whole module exists to
        // avoid.
        let cap = model.render_budget.max(MIN_LOOP_FRAMES_PER_PANE);
        // A frame that costs nothing is not a device this ships for, it is a
        // model built wrong; the cap is the answer that cannot then divide by
        // zero *and* cannot silently become an unbounded loop.
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
///
/// One per application, because the pool is one allowance for the whole
/// application. The shape is `crate::egui_renderer::MirrorRungs`', deliberately:
/// a pending demand that has to survive a dwell before it is taken, and a dead
/// band on the direction that is optional.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LoopPoolState {
    allocation: LoopAllocation,
    /// The demand [`Self::allocation`] was last settled against. Not the same as
    /// "the demand last observed": a growth refused by the dead band still moves
    /// this, so a demand that has been considered and declined is not
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
    ///
    /// # Why the two directions are not treated alike
    ///
    /// `MIRROR_RUNG_DWELL_FRAMES` applies in both directions because a rung is
    /// a rendering quality and neither direction is more urgent than the other.
    /// A pool is a **bound**, and the two directions here are not alike:
    ///
    /// * **A shrink** — a pane opened — is taken as soon as the dwell allows,
    ///   with no dead band. Being over the bound is not something to be sticky
    ///   about. The dwell is still there, and it is what makes a pane that
    ///   appears and vanishes inside a quarter-second cost nothing at all.
    /// * **A growth** — a pane closed — is optional, so it also has to clear
    ///   [`LOOP_POOL_HYSTERESIS`]. Closing the sixth of six panes would buy each
    ///   survivor 20 % more share and a frame or two of history, and re-fetching
    ///   and re-rendering every loop on screen for that is not a trade worth
    ///   making. Closing the second of two doubles the share and is taken.
    ///
    /// Read the other way round, the band is what makes the *reachable* pane
    /// counts behave: 1→2 halves the share, 2→3 is 1.5x, 3→4 is 1.33x — all
    /// past the band — while the last steps up to a full screen are not, so the
    /// busiest layouts settle instead of rippling.
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
    ///
    /// See [`Self::observe`]. Shrinks always are; growths have to clear the
    /// dead band. Compared on `share_bytes` rather than on a frame count
    /// because the share is the continuous quantity — a frame count is an
    /// integer division of it, and two shares 20 % apart can round to the same
    /// number of frames on one loop kind and not on another.
    fn worth_taking(in_force: LoopAllocation, bare: LoopAllocation) -> bool {
        if bare.share_bytes <= in_force.share_bytes {
            return true;
        }
        bare.share_bytes as f64 >= in_force.share_bytes as f64 * LOOP_POOL_HYSTERESIS
    }
}

/// What a previous session learned this machine can hold, if anything.
///
/// A decimal count of MiB and nothing else. Not JSON: this is one integer, and
/// the whole argument for its own key is that a value that cannot be read costs
/// only itself. A format with structure would give a corrupt entry more ways to
/// be almost-readable.
///
/// Anything unreadable, or zero, is `None` — the caller falls back to the
/// device classification, which is the same answer a first launch gets. A value
/// outside the target's bounds is *held* to them rather than rejected: the
/// bounds are this build's decision and a config written by an older build is
/// still evidence about the machine.
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
///
/// Called at the moment of the decision — a back-off — rather than left to
/// `autosave_config`'s 3 s timer, because the session that learns this is by
/// definition one where the graphics device is misbehaving and may not get
/// three more seconds. A failed write is logged and dropped: losing the memo
/// costs one re-probe, and configuration is never allowed to be load-bearing.
///
/// [`KvStore::store_now`] is what keeps that true. The ordinary `store`
/// defers to a writer thread, and a process killed moments later drops the memo
/// exactly as if it had waited for the timer — which is the outcome this
/// function was written to rule out.
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
