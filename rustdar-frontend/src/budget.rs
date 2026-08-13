//! One device profile in, one immutable set of budgets out.
//!
//! # What this replaces, and what it deliberately does not
//!
//! Every per-target number in [`crate::constants`] is a `cfg` cascade: the
//! compiler picks an arm, one arm out of three is compiled, and one arm out of
//! three is testable. The cascade is not *wrong* — a wasm build genuinely
//! cannot hold what a desktop build can — but it answers the question with the
//! only fact available at compile time, which is **which APIs exist**, not
//! **what the machine is**. `crate::loop_pool`'s own module doc states that
//! taxonomy and the pool already obeys it; this module is that idea generalised
//! to every budget rather than one.
//!
//! The shape is `LoopPool`'s, deliberately and to the letter:
//!
//! * a compile-time **[`BudgetLimits`]** bracket per field — a floor that is a
//!   *decision* about the worst device this build will work on, and a ceiling
//!   that is a *guard against a lie*, so a driver reporting a shared-memory
//!   iGPU's whole DRAM as VRAM cannot claim it;
//! * a **[`DeviceProfile`]** of everything known about the machine, with
//!   `Option` fields whose `None` arm is the majority case rather than a
//!   fallback — every browser reports no VRAM, ever;
//! * a pure **[`resolve`]** from the one to the other, with no `cfg!` in its
//!   body and no globals, so all three shipped configurations and every
//!   synthetic one in between are reachable from a single host test run.
//!
//! # What it does *today*: nothing, exactly
//!
//! [`resolve`] takes each field's floor. The brackets are populated from the
//! shipped constants, so the floor **is** the constant on every arm, and
//! `the_resolver_reproduces_every_shipped_constant` puts the two side by side
//! field for field. That is the whole point of landing this step on its own: the
//! app stops reading nineteen `cfg` cascades and starts reading one struct, and
//! not a byte moves. What is gained is that every arm of every budget is now
//! exercised by the host test run instead of one of three, and that the app is
//! one constructor argument away from being device-aware.
//!
//! Making a field leave its floor — promoting a discrete GPU's grid, promoting a
//! browser that reports 16384 rather than the 2048 WebGL2 guarantees — is the
//! *next* step and is a change in behaviour that has to be argued on its own.
//! `docs/cross-platform-resource-limits.md` is where that argument lives.
//!
//! # There is no browser in [`DeviceProfile`], and there must not be
//!
//! Firefox and Chromium are the same binary, from the same origin, on the same
//! silicon, and they may report different figures. A `cfg` cannot express that
//! question at all — which is the strongest argument for this module — but the
//! answer is not a `Browser` enum either: a user-agent sniff is spoofable and
//! goes stale. They are separated the way the parity rule sanctions, by **what
//! they report**, so two browsers on one machine produce two [`AdapterCeilings`]
//! and therefore two [`Budgets`], with no browser-identity term anywhere.
//! Firefox governing is expressed as a *bracket*, not as a branch.
//!
//! # Capability is a different question and is already answered
//!
//! *Can this device do the thing at all* is `crate::volume::probe`, which reads
//! four limits and two format features and returns a human-readable reason.
//! *How much of the thing can it afford* is this module. Keeping them apart is
//! what makes "available in one browser, absent in the other on the same box" a
//! first-class outcome rather than a surprise.

use crate::constants;
use crate::volume::quality::{DeviceClass, VolumeQuality};

/// Which APIs exist. **Not** which machine this is.
///
/// The distinction `crate::loop_pool`'s module doc draws: the `cfg` tells you
/// what is callable, and the device class, discovered at runtime, tells you what
/// the machine is. Two variants, and deliberately no third for `mobile` — a
/// native Android build and a native desktop build have the same API surface and
/// differ only in what the *bracket* may safely be, which is [`BudgetLimits`]'
/// job.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Platform {
    /// A native build: Vulkan, Metal or DX12, real threads, real limits.
    Native,
    /// A browser: WebGL2, one thread, and no memory signal of any kind.
    Web,
}

/// What the page or the window manager can say about the shape of the device.
///
/// `None` on every target that has no bridge to ask. Browser-side this is
/// `matchMedia('(pointer: coarse)')` and friends, and the shortlist
/// `crate::loop_pool`'s module doc surveys — a touchscreen laptop reports
/// *both* coarse and fine, which is the case a naive `coarse` test gets wrong.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FormFactor {
    /// Coarse pointer and no fine one.
    Handheld,
    /// A fine pointer is available, whether or not a coarse one also is.
    Desktop,
}

/// The adapter's own reported ceilings — the numbers the app already reads.
///
/// `max_texture_dimension_2d` and `_3d` are the two the web arm of
/// `app_state::device_limits` already lifts through `using_resolution`, and the
/// two `volume::limits_shortfall` compares the grid against. They are the
/// cheapest real signal available on the one target that offers no others.
///
/// **A reported figure is not an allocatable one.** Measured on this project's
/// own machines: Firefox on an RTX 3090 reports `MAX_TEXTURE_SIZE` 32768 and
/// refuses to allocate above 16384, while Chrome on an AMD Radeon 890M reports
/// 16384 and allocates it. Anything that spends against these has to cap
/// against what the device will actually hand back, which is why nothing spends
/// against them yet.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct AdapterCeilings {
    /// The largest 2D texture, per axis, the adapter says it will accept.
    pub max_texture_dimension_2d: u32,
    /// The largest 3D texture, per axis. `shape_for_budget` spends the cell
    /// budget against this.
    pub max_texture_dimension_3d: u32,
}

impl AdapterCeilings {
    /// What a device reporting exactly the WebGL2 guarantee says.
    ///
    /// The conservative answer, and the one a caller with no adapter yet takes —
    /// which is what makes `constants::VOLUME_GRID_FLOOR_SHAPE` a constant.
    pub const WEBGL2_GUARANTEE: Self = Self {
        max_texture_dimension_2d: rustdar_radar::types::WEBGL2_MAX_TEXTURE_DIMENSION_2D as u32,
        max_texture_dimension_3d: constants::WEBGL2_MAX_TEXTURE_DIMENSION_3D,
    };
}

/// What a previous session learned by **failing**.
///
/// Evidence beats classification: a figure arrived at by watching this machine
/// refuse an allocation is better than any guess from a device type, and
/// honouring it is also what keeps a reopen 1:1 rather than showing a different
/// loop length on every start. `crate::loop_pool::LOOP_POOL_KEY` is where the
/// one field of it is persisted today, in its own `ConfigStore` entry written
/// synchronously — because a value learned by crashing the GPU is exactly the
/// value that must not be lost to a 3 s autosave timer.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct BudgetMemo {
    /// The loop pool this machine settled at, in bytes.
    pub loop_pool_bytes: usize,
}

/// Everything known about the machine, at the moment the budgets are decided.
///
/// Constructed at one seam and never consulted again: the budgets are resolved
/// once and threaded, so nothing downstream can re-derive a different answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DeviceProfile {
    /// Which APIs exist. From `cfg`, once, here and nowhere else.
    pub platform: Platform,
    /// The compile-time bracket this build is held inside. `mobile`'s whole
    /// remaining job — see [`BudgetLimits`].
    pub limits: BudgetLimits,
    /// The device-class hint from the driver. `Unknown` on every browser.
    pub class: DeviceClass,
    /// What the adapter says it can hold.
    pub adapter: AdapterCeilings,
    /// Measured VRAM, where a trustworthy signal exists. `None` is the
    /// **majority** arm, not a fallback: wgpu 29.0.4 reports no capacity on any
    /// backend, and no browser API answers the question at all.
    pub vram_bytes: Option<u64>,
    /// Measured or declared system RAM. `None` for the same reasons.
    pub system_ram_bytes: Option<u64>,
    /// Threads actually available. 1 in a browser.
    pub parallelism: usize,
    /// Browser-side class refinement, when a platform bridge can supply it.
    pub form_factor: Option<FormFactor>,
    /// See [`BudgetMemo`]. Wins outright when present.
    pub memo: Option<BudgetMemo>,
}

impl DeviceProfile {
    /// The profile this build has before it has met an adapter.
    ///
    /// Every runtime field is at its most conservative reading: the class no
    /// driver would name, the ceilings a device at the WebGL2 guarantee
    /// reports, no memory signal, and no memo. It is the honest description of
    /// what is known at construction, and it resolves to exactly the constants
    /// this target ships — see [`resolve`].
    ///
    /// **The one `cfg` in this module is `BudgetLimits::for_target`'s**, and it
    /// selects a bracket rather than a budget.
    pub fn for_target() -> Self {
        Self {
            platform: if cfg!(target_arch = "wasm32") {
                Platform::Web
            } else {
                Platform::Native
            },
            limits: BudgetLimits::for_target(),
            class: DeviceClass::Unknown,
            adapter: AdapterCeilings::WEBGL2_GUARANTEE,
            vram_bytes: None,
            system_ram_bytes: None,
            parallelism: 1,
            form_factor: None,
            memo: None,
        }
    }
}

/// A compile-time `[floor, ceiling]` pair for one number.
///
/// The two halves are different *kinds* of statement and the asymmetry is the
/// whole design, exactly as `crate::loop_pool::LoopPoolLimits`' doc puts it:
///
/// * **floor** — a decision. The worst device this build is willing to work on,
///   never crossed downward whatever happens. It is what the wasm `cargo check`
///   row's const-assert guards for the grid shape today.
/// * **ceiling** — a guard against a lie. The most this build will ever spend
///   even if the device claims infinity.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Bracket {
    /// Never resolved below this.
    pub floor: usize,
    /// Never resolved above this.
    pub ceiling: usize,
}

impl Bracket {
    /// A number with no room to move yet: floor and ceiling are the same.
    ///
    /// Most brackets are this today, and saying so in one place is better than
    /// nineteen repetitions of the same pair. Raising a ceiling off its floor is
    /// what "no compromises when the hardware is available" costs, and it is a
    /// deliberate, reviewed, one-line change per field once the measurement to
    /// justify it exists.
    pub const fn pinned(value: usize) -> Self {
        Self {
            floor: value,
            ceiling: value,
        }
    }

    /// A genuine pair.
    pub const fn new(floor: usize, ceiling: usize) -> Self {
        Self { floor, ceiling }
    }

    /// `value` held inside the pair, with the floor winning a crossed one.
    ///
    /// The `max` is not defensive tidiness — `clamp` **panics** on `min > max`,
    /// and these are independently editable constants, so a crossed pair would
    /// be a startup panic on one target only, which is the arm no host test can
    /// reach.
    pub fn hold(&self, value: usize) -> usize {
        value.clamp(self.floor, self.ceiling.max(self.floor))
    }
}

/// The same, for the one field that is three numbers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct CellBracket {
    /// The cell triple this build never budgets below.
    pub floor: [u32; 3],
    /// The cell triple it never budgets above.
    pub ceiling: [u32; 3],
}

impl CellBracket {
    /// A triple with no room to move yet.
    pub const fn pinned(cells: [u32; 3]) -> Self {
        Self {
            floor: cells,
            ceiling: cells,
        }
    }
}

/// The compile-time brackets this build resolves inside.
///
/// **This is all `mobile` still does.** It no longer selects nineteen budgets;
/// it selects one bracket set, which is legitimately a compile-time fact about
/// which binary is being built — a native Android build and a native desktop
/// build differ in what the ceiling may safely be even before any device is
/// seen, because a ceiling is a promise about the worst *shipped* device and
/// those populations do not overlap.
///
/// Every field is populated from the constant of the same name in
/// [`crate::constants`], so the constants stay the single statement of each
/// figure and this is a second view of them rather than a second copy.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct BudgetLimits {
    /// Which set this is, for failure messages. Not a behavioural term.
    pub name: &'static str,
    /// `rustdar_radar::types::IMAGE_SIZE` — the side a static plan-view render
    /// takes at the base size.
    pub image_side_px: Bracket,
    /// `constants::LONG_RANGE_IMAGE_SIZE`.
    pub long_range_image_side_px: Bracket,
    /// `constants::LOOP_IMAGE_SIZE`.
    pub loop_image_side_px: Bracket,
    /// `rustdar_radar::xsect::SECTION_WIDTH`.
    pub section_width_px: Bracket,
    /// `constants::MAX_CONCURRENT_RENDERS`.
    pub concurrent_renders: Bracket,
    /// `constants::MAX_CONCURRENT_LOOP_DOWNLOADS`.
    pub concurrent_loop_downloads: Bracket,
    /// `constants::MAX_LOOP_FRAMES`.
    pub loop_frames_held: Bracket,
    /// `constants::MAX_LOOP_RENDER_BUDGET`.
    pub loop_render_budget: Bracket,
    /// `constants::LOOP_POOL_FLOOR_BYTES` and `LOOP_POOL_CEILING_BYTES` — the
    /// one bracket in this struct that was already a bracket in the source.
    pub loop_pool_bytes: Bracket,
    /// `constants::VOLUME_GRID_CELLS`.
    pub grid_cells: CellBracket,
    /// `constants::VOLUME_TEXTURE_BUDGET_BYTES`.
    pub volume_texture_bytes: Bracket,
    /// `constants::VOLUME_OFFSCREEN_BUDGET_BYTES`.
    pub offscreen_bytes: Bracket,
    /// `constants::VOLUME_MIRROR_BYTES_MAX`.
    pub mirror_bytes: Bracket,
    /// `constants::MAX_RENDER_CACHE_ENTRIES`.
    pub render_cache_entries: Bracket,
    /// `volume::quality::PLATFORM_CEILING` — already a ceiling, so it has no
    /// pair. The floor of a quality ladder is `VolumeQuality::CHEAPEST` on
    /// every target and naming it would state nothing.
    pub quality_ceiling: VolumeQuality,
    /// `rustdar_egui::pane`'s pane cap for this class. Not a `cfg` cascade over
    /// there — it is chosen at runtime by width class — and it is carried here
    /// because it is the other half of the multiplication the whole-application
    /// ceiling makes, and the two halves live in different crates.
    pub max_panes: Bracket,
    /// `constants::APP_TEXTURE_BUDGET_BYTES`.
    ///
    /// Deliberately **not** device-derived, now or later: the moment both sides
    /// of the snugness test move together it becomes a tautology and stops
    /// catching anything. If a 24 GiB card should raise it, raise the constant,
    /// deliberately, and let `the_app_ceiling_is_not_slack_enough_to_hide_a_doubling`
    /// bite and be re-argued.
    pub app_texture_ceiling_bytes: Bracket,
}

impl BudgetLimits {
    /// The wasm32 bracket.
    pub const WASM: Self = Self {
        name: "wasm32",
        image_side_px: Bracket::pinned(rustdar_radar::types::WASM_IMAGE_SIZE),
        long_range_image_side_px: Bracket::pinned(constants::WASM_LONG_RANGE_IMAGE_SIZE),
        loop_image_side_px: Bracket::pinned(constants::WASM_LOOP_IMAGE_SIZE),
        section_width_px: Bracket::pinned(rustdar_radar::xsect::WASM_SECTION_WIDTH),
        concurrent_renders: Bracket::pinned(constants::WASM_MAX_CONCURRENT_RENDERS),
        concurrent_loop_downloads: Bracket::pinned(
            constants::NON_MOBILE_MAX_CONCURRENT_LOOP_DOWNLOADS,
        ),
        loop_frames_held: Bracket::pinned(constants::WASM_MAX_LOOP_FRAMES),
        loop_render_budget: Bracket::pinned(constants::WASM_MAX_LOOP_RENDER_BUDGET),
        loop_pool_bytes: Bracket::new(
            constants::WASM_LOOP_POOL_FLOOR_BYTES,
            constants::WASM_LOOP_POOL_CEILING_BYTES,
        ),
        grid_cells: CellBracket::pinned(constants::WASM_VOLUME_GRID_CELLS),
        volume_texture_bytes: Bracket::pinned(constants::WASM_VOLUME_TEXTURE_BUDGET_BYTES),
        offscreen_bytes: Bracket::pinned(constants::WASM_VOLUME_OFFSCREEN_BUDGET_BYTES),
        mirror_bytes: Bracket::pinned(constants::WASM_VOLUME_MIRROR_BYTES_MAX),
        render_cache_entries: Bracket::pinned(constants::NON_MOBILE_MAX_RENDER_CACHE_ENTRIES),
        quality_ceiling: crate::volume::quality::WASM_PLATFORM_CEILING,
        max_panes: Bracket::pinned(rustdar_egui::pane::MAX_PANES_DESKTOP),
        app_texture_ceiling_bytes: Bracket::pinned(constants::WASM_APP_TEXTURE_BUDGET_BYTES),
    };

    /// The mobile bracket — native Android and iOS.
    pub const MOBILE: Self = Self {
        name: "mobile",
        image_side_px: Bracket::pinned(rustdar_radar::types::NATIVE_IMAGE_SIZE),
        long_range_image_side_px: Bracket::pinned(constants::MOBILE_LONG_RANGE_IMAGE_SIZE),
        loop_image_side_px: Bracket::pinned(constants::MOBILE_LOOP_IMAGE_SIZE),
        section_width_px: Bracket::pinned(rustdar_radar::xsect::NATIVE_SECTION_WIDTH),
        concurrent_renders: Bracket::pinned(constants::MOBILE_MAX_CONCURRENT_RENDERS),
        concurrent_loop_downloads: Bracket::pinned(constants::MOBILE_MAX_CONCURRENT_LOOP_DOWNLOADS),
        loop_frames_held: Bracket::pinned(constants::MOBILE_MAX_LOOP_FRAMES),
        loop_render_budget: Bracket::pinned(constants::MOBILE_MAX_LOOP_RENDER_BUDGET),
        loop_pool_bytes: Bracket::new(
            constants::MOBILE_LOOP_POOL_FLOOR_BYTES,
            constants::MOBILE_LOOP_POOL_CEILING_BYTES,
        ),
        grid_cells: CellBracket::pinned(constants::MOBILE_VOLUME_GRID_CELLS),
        volume_texture_bytes: Bracket::pinned(constants::MOBILE_VOLUME_TEXTURE_BUDGET_BYTES),
        offscreen_bytes: Bracket::pinned(constants::MOBILE_VOLUME_OFFSCREEN_BUDGET_BYTES),
        mirror_bytes: Bracket::pinned(constants::MOBILE_VOLUME_MIRROR_BYTES_MAX),
        render_cache_entries: Bracket::pinned(constants::MOBILE_MAX_RENDER_CACHE_ENTRIES),
        quality_ceiling: crate::volume::quality::MOBILE_PLATFORM_CEILING,
        max_panes: Bracket::pinned(rustdar_egui::pane::MAX_PANES_MOBILE),
        app_texture_ceiling_bytes: Bracket::pinned(constants::MOBILE_APP_TEXTURE_BUDGET_BYTES),
    };

    /// The desktop bracket.
    pub const DESKTOP: Self = Self {
        name: "desktop",
        image_side_px: Bracket::pinned(rustdar_radar::types::NATIVE_IMAGE_SIZE),
        long_range_image_side_px: Bracket::pinned(constants::DESKTOP_LONG_RANGE_IMAGE_SIZE),
        loop_image_side_px: Bracket::pinned(constants::DESKTOP_LOOP_IMAGE_SIZE),
        section_width_px: Bracket::pinned(rustdar_radar::xsect::NATIVE_SECTION_WIDTH),
        concurrent_renders: Bracket::pinned(constants::DESKTOP_MAX_CONCURRENT_RENDERS),
        concurrent_loop_downloads: Bracket::pinned(
            constants::NON_MOBILE_MAX_CONCURRENT_LOOP_DOWNLOADS,
        ),
        loop_frames_held: Bracket::pinned(constants::DESKTOP_MAX_LOOP_FRAMES),
        loop_render_budget: Bracket::pinned(constants::DESKTOP_MAX_LOOP_RENDER_BUDGET),
        loop_pool_bytes: Bracket::new(
            constants::DESKTOP_LOOP_POOL_FLOOR_BYTES,
            constants::DESKTOP_LOOP_POOL_CEILING_BYTES,
        ),
        grid_cells: CellBracket::pinned(constants::DESKTOP_VOLUME_GRID_CELLS),
        volume_texture_bytes: Bracket::pinned(constants::DESKTOP_VOLUME_TEXTURE_BUDGET_BYTES),
        offscreen_bytes: Bracket::pinned(constants::DESKTOP_VOLUME_OFFSCREEN_BUDGET_BYTES),
        mirror_bytes: Bracket::pinned(constants::DESKTOP_VOLUME_MIRROR_BYTES_MAX),
        render_cache_entries: Bracket::pinned(constants::NON_MOBILE_MAX_RENDER_CACHE_ENTRIES),
        quality_ceiling: crate::volume::quality::DESKTOP_PLATFORM_CEILING,
        max_panes: Bracket::pinned(rustdar_egui::pane::MAX_PANES_DESKTOP),
        app_texture_ceiling_bytes: Bracket::pinned(constants::DESKTOP_APP_TEXTURE_BUDGET_BYTES),
    };

    /// The bracket this build compiled.
    ///
    /// The cascade shape `constants::MAX_LOOP_FRAMES` documents, and the only
    /// `cfg` in this module: `cfg` arms have no ordering and no fallthrough, so
    /// the `not(target_arch = "wasm32")` guard on the lower two is what keeps
    /// wasm32 from matching two of them.
    #[cfg(target_arch = "wasm32")]
    pub const fn for_target() -> Self {
        Self::WASM
    }

    /// See the wasm32 arm.
    #[cfg(all(not(target_arch = "wasm32"), mobile))]
    pub const fn for_target() -> Self {
        Self::MOBILE
    }

    /// See the wasm32 arm.
    #[cfg(all(not(target_arch = "wasm32"), not(mobile)))]
    pub const fn for_target() -> Self {
        Self::DESKTOP
    }

    /// The three shipped brackets, whatever this target compiled.
    ///
    /// What makes every arm reachable from one host test run, which is the
    /// property `constants::WASM_VOLUME_GRID_CELLS`' doc argues for at length
    /// and which this module makes true of the *whole* budget set at once.
    pub const SHIPPED: [Self; 3] = [Self::WASM, Self::MOBILE, Self::DESKTOP];
}

/// What every subsystem is handed instead of a `cfg` constant.
///
/// Immutable, resolved once, threaded from the constructor. No global, no
/// `OnceLock`, no `thread_local` — a global would be untestable across the
/// matrix, and `volume::quality::select`, `LoopPool::for_device` and
/// `MirrorLimits::for_device` already prove the argument-passing style scales,
/// each taking its limits as a parameter *specifically* so that all arms are
/// reachable from one host test run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Budgets {
    /// The bracket set this came out of, for failure messages.
    pub name: &'static str,
    /// The side a static plan-view render takes at the base size.
    pub image_side_px: usize,
    /// The side it may grow to for a sweep reaching past the base extent.
    pub long_range_image_side_px: usize,
    /// The side a **loop** frame renders at, whatever its sweep reaches.
    pub loop_image_side_px: usize,
    /// The cross-section raster's width; its height is half of it.
    pub section_width_px: usize,
    /// Background radar renders that may be in flight at once.
    pub concurrent_renders: usize,
    /// Loop scan downloads that may be in flight per pane.
    pub concurrent_loop_downloads: usize,
    /// Frames a loop may *hold*.
    pub loop_frames_held: usize,
    /// Frames a loop may keep *textured*, which is the binding term.
    pub loop_render_budget: usize,
    /// The loop pool's floor. Carried as a pair rather than as one resolved
    /// figure because the pool is the one budget that already learns from
    /// failure at runtime: `LoopPool::for_device` picks inside the pair and
    /// `LoopPool::back_off` walks back down it. Folding that into this struct
    /// means moving the back-off path too, which is a change in behaviour.
    pub loop_pool_floor_bytes: usize,
    /// The loop pool's ceiling. See [`Self::loop_pool_floor_bytes`].
    pub loop_pool_ceiling_bytes: usize,
    /// The voxel grid **cell budget** — the count every allocation is sized
    /// against, not the shape that gets built. See [`Self::grid_shape`].
    pub grid_cells: [u32; 3],
    /// Ceiling on one pane's 3D volume textures.
    pub volume_texture_bytes: usize,
    /// Ceiling on the pane-sized target one volume raymarches into.
    pub offscreen_bytes: usize,
    /// Ceiling on the whole application's single pane-mirror texture.
    pub mirror_bytes: usize,
    /// Entries the shared render cache keeps.
    pub render_cache_entries: usize,
    /// The best quality this build may select, whatever the adapter claims.
    pub quality_ceiling: VolumeQuality,
    /// Panes this class can show at once.
    pub max_panes: usize,
    /// The whole-application GPU texture ceiling the sum is held to.
    pub app_texture_ceiling_bytes: usize,
}

impl Budgets {
    /// Ceiling on the resident voxel grids a 3D loop may hold, application-wide.
    ///
    /// An alias of the pool floor rather than a figure of its own, and that is
    /// the design: a screen showing a 3D loop instead of a map loop should cost
    /// about the same, so this is one share of a floor-sized pool. See
    /// `constants::VOLUME_LOOP_TEXTURE_BUDGET_BYTES`, which is the same alias
    /// stated as a constant.
    pub fn volume_loop_bytes(&self) -> usize {
        self.loop_pool_floor_bytes
    }

    /// Bytes one loop frame's texture occupies: RGBA at the loop side squared.
    ///
    /// Loop frames carry no value grid, so this is the whole cost — unlike a
    /// static pane render.
    pub fn loop_frame_bytes(&self) -> usize {
        self.loop_image_side_px * self.loop_image_side_px * 4
    }

    /// Bytes one **static** pane render's texture occupies, worst case: the
    /// long-range side, since that is what a device passing the gate can be
    /// asked to hold.
    pub fn static_frame_bytes(&self) -> usize {
        self.long_range_image_side_px * self.long_range_image_side_px * 4
    }

    /// Bytes one **cross-section** loop frame occupies: RGBA at
    /// `section_width × section_width / 2`.
    pub fn section_frame_bytes(&self) -> usize {
        self.section_width_px * (self.section_width_px / 2) * 4
    }

    /// Frames that hold a texture at once. `evict_textures_outside_render_set`
    /// runs every dispatch with the render budget, so a loop holding
    /// [`Self::loop_frames_held`] keeps only the render set textured.
    pub fn textured_frames(&self) -> usize {
        self.loop_render_budget.min(self.loop_frames_held)
    }

    /// Bytes one resident voxel grid costs the device: every mip level it is
    /// laid out with, its colour table's own texture, and the jitter tile
    /// beside it. Read from `volume::raymarch::resident_grid_bytes` rather than
    /// recomputed, so the budget is checked against the arithmetic the upload
    /// path allocates by.
    ///
    /// `None` only where the shape overflows a `usize`, which no shipped or
    /// synthetic bracket does.
    pub fn volume_bytes(&self) -> Option<usize> {
        crate::volume::raymarch::resident_grid_bytes(self.grid_cells)
    }

    /// The grid shape to **request** on a device whose 3D textures may be
    /// `max_axis` on a side.
    ///
    /// The cell count is the budget and how it is spent over three axes is
    /// free — 512×512×32 and 256×256×128 are the same 8,388,608 cells — so this
    /// spends the budget on the largest square the device will hold.
    pub fn grid_shape(&self, max_axis: u32) -> rustdar_radar::voxel::VoxelShape {
        constants::volume_grid_shape_of(self.grid_cells, max_axis)
    }

    /// Every GPU texture the application budgets at once, worst case.
    ///
    /// Three terms, and the middle one is the one a reading of the code misses:
    ///
    /// * the loop pool at its **ceiling** — one term, not `panes ×` a per-pane
    ///   figure, because the pool is divided among the loops that want one and
    ///   a 3D loop takes one share per *volume* rather than per pane;
    /// * the volume store's **floor**, which `App::setup_egui_frame` applies
    ///   with `.max(...)` *outside* the pool, so a screen with no 3D loop at all
    ///   spends the whole pool on raster frames and still leaves the store
    ///   floored;
    /// * one raymarch offscreen **per pane**, correctly per pane: no two panes
    ///   share one.
    ///
    /// It deliberately over-counts — a pane is only ever one kind at a time —
    /// and what matters is that raising any term has to come past
    /// `the_whole_application_fits_its_gpu_ceiling`.
    pub fn app_texture_bytes(&self) -> usize {
        self.loop_pool_ceiling_bytes
            + self.volume_loop_bytes()
            + self.max_panes * self.offscreen_bytes
    }
}

/// The budgets this device gets.
///
/// **A pure function, with no `cfg!` in its body and no globals**, which is what
/// makes the whole matrix testable without a GPU — the property
/// `volume::quality::select`, `LoopPool::for_device` and
/// `MirrorLimits::for_device` each already have for one number and this has for
/// all of them at once.
///
/// # Today it takes every floor, and that is the point
///
/// The brackets are populated from the shipped constants, so a floor **is** the
/// constant on its arm and this reproduces today's configuration byte for byte
/// on all three. `the_resolver_reproduces_every_shipped_constant` puts the two
/// side by side field for field rather than asserting the claim.
///
/// The loop pool is the one field that leaves as a *pair* — see
/// [`Budgets::loop_pool_floor_bytes`] — because it already has a runtime
/// resolution and a back-off path of its own, and moving those is a change in
/// behaviour rather than a change in plumbing.
///
/// Nothing here reads [`DeviceProfile::class`], [`DeviceProfile::adapter`],
/// [`DeviceProfile::vram_bytes`] or [`DeviceProfile::memo`] yet. That is not an
/// oversight to be tidied: promoting a field off its floor changes what the app
/// allocates on a real machine, and this step exists precisely so that change
/// can be argued and landed on its own, against a resolver whose every arm is
/// already under test.
pub fn resolve(profile: &DeviceProfile) -> Budgets {
    let limits = &profile.limits;
    Budgets {
        name: limits.name,
        image_side_px: limits.image_side_px.floor,
        long_range_image_side_px: limits.long_range_image_side_px.floor,
        loop_image_side_px: limits.loop_image_side_px.floor,
        section_width_px: limits.section_width_px.floor,
        concurrent_renders: limits.concurrent_renders.floor,
        concurrent_loop_downloads: limits.concurrent_loop_downloads.floor,
        loop_frames_held: limits.loop_frames_held.floor,
        loop_render_budget: limits.loop_render_budget.floor,
        loop_pool_floor_bytes: limits.loop_pool_bytes.floor,
        loop_pool_ceiling_bytes: limits
            .loop_pool_bytes
            .ceiling
            .max(limits.loop_pool_bytes.floor),
        grid_cells: limits.grid_cells.floor,
        volume_texture_bytes: limits.volume_texture_bytes.floor,
        offscreen_bytes: limits.offscreen_bytes.floor,
        mirror_bytes: limits.mirror_bytes.floor,
        render_cache_entries: limits.render_cache_entries.floor,
        quality_ceiling: limits.quality_ceiling,
        max_panes: limits.max_panes.floor,
        app_texture_ceiling_bytes: limits.app_texture_ceiling_bytes.floor,
    }
}

#[path = "budget/tests.rs"]
#[cfg(test)]
mod tests;
