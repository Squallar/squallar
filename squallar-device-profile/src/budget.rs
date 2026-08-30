//! One device profile in, one immutable set of budgets out.

use crate::constants;
use crate::quality::{DeviceClass, GradientShading, VolumeQuality};

/// Which APIs exist. **Not** which machine this is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Platform {
    /// A native build: Vulkan, Metal or DX12, real threads, real limits.
    Native,
    /// A browser: WebGL2, one thread, and no memory signal of any kind.
    Web,
}

/// What the page or the window manager can say about the shape of the device.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FormFactor {
    /// Coarse pointer and no fine one.
    Handheld,
    /// A fine pointer is available, whether or not a coarse one also is.
    Desktop,
}

/// The adapter's own reported ceilings — the numbers the app already reads.
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
    pub const WEBGL2_GUARANTEE: Self = Self {
        max_texture_dimension_2d: squallar_radar::types::WEBGL2_MAX_TEXTURE_DIMENSION_2D as u32,
        max_texture_dimension_3d: constants::WEBGL2_MAX_TEXTURE_DIMENSION_3D,
    };
}

/// The componentwise least either desktop-class machine this project has
/// **measured** a browser report on.
pub const DESKTOP_CLASS_REPORT: AdapterCeilings = AdapterCeilings {
    max_texture_dimension_2d: 16384,
    max_texture_dimension_3d: 8192,
};

/// How far up its bracket each budget may be spent on this device.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Promotion {
    /// What a device that says nothing about itself gets: the shipped
    /// constants, unchanged.
    Floor,
    /// One rung. What an integrated GPU takes, which is the rule
    /// `LoopPool::for_device` already ships for the pool.
    Step,
    /// The most this build will spend on this class of machine.
    Ceiling,
}

impl Promotion {
    /// What the driver's own classification is worth.
    pub fn for_class(class: DeviceClass) -> Self {
        match class {
            DeviceClass::Discrete => Self::Ceiling,
            DeviceClass::Integrated => Self::Step,
            DeviceClass::Virtual | DeviceClass::Unknown | DeviceClass::Software => Self::Floor,
        }
    }
}

/// What a previous session learned by **failing**.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct BudgetMemo {
    /// The loop pool this machine settled at, in bytes. `None` where nothing
    /// has been learned, which is every first launch.
    pub loop_pool_bytes: Option<usize>,
    /// Rungs of the degradation ladder this machine has already surrendered.
    pub steps_back: u32,
}

/// Everything known about the machine, at the moment the budgets are decided.
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
    /// How far up its bracket every budget may be spent here.
    pub fn promotion(&self) -> Promotion {
        match self.class {
            DeviceClass::Unknown => self.reported_promotion(),
            named => Promotion::for_class(named),
        }
    }

    /// What the adapter's own reported ceilings are worth, where nothing else
    /// answered.
    fn reported_promotion(&self) -> Promotion {
        let desktop_class = self.adapter.max_texture_dimension_2d
            >= DESKTOP_CLASS_REPORT.max_texture_dimension_2d
            && self.adapter.max_texture_dimension_3d
                >= DESKTOP_CLASS_REPORT.max_texture_dimension_3d;
        if desktop_class {
            Promotion::Ceiling
        } else {
            Promotion::Floor
        }
    }

    /// Rungs of the ladder this machine has already surrendered. See [`demote`].
    fn steps_back(&self) -> u32 {
        self.memo.map_or(0, |memo| memo.steps_back)
    }
}

impl DeviceProfile {
    /// The profile this build has before it has met an adapter.
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
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Bracket {
    /// Never resolved below this.
    pub floor: usize,
    /// What [`Promotion::Step`] resolves to.
    pub step: usize,
    /// Never resolved above this.
    pub ceiling: usize,
}

impl Bracket {
    /// A number with no room to move: all three rungs are the same.
    pub const fn pinned(value: usize) -> Self {
        Self {
            floor: value,
            step: value,
            ceiling: value,
        }
    }

    /// A pair only a [`Promotion::Ceiling`] reaches.
    pub const fn new(floor: usize, ceiling: usize) -> Self {
        Self {
            floor,
            step: floor,
            ceiling,
        }
    }

    /// All three rungs, named.
    pub const fn stepped(floor: usize, step: usize, ceiling: usize) -> Self {
        Self {
            floor,
            step,
            ceiling,
        }
    }

    /// `value` held inside the pair, with the floor winning a crossed one.
    pub fn hold(&self, value: usize) -> usize {
        value.clamp(self.floor, self.ceiling.max(self.floor))
    }

    /// The rung `promotion` buys, held inside the pair.
    pub fn at(&self, promotion: Promotion) -> usize {
        self.hold(match promotion {
            Promotion::Floor => self.floor,
            Promotion::Step => self.step,
            Promotion::Ceiling => self.ceiling,
        })
    }
}

/// The same, for the one field that is three numbers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct CellBracket {
    /// The cell triple this build never budgets below.
    pub floor: [u32; 3],
    /// What [`Promotion::Step`] budgets.
    pub step: [u32; 3],
    /// The cell triple it never budgets above.
    pub ceiling: [u32; 3],
}

impl CellBracket {
    /// A triple with no room to move yet.
    pub const fn pinned(cells: [u32; 3]) -> Self {
        Self {
            floor: cells,
            step: cells,
            ceiling: cells,
        }
    }

    /// A triple only a [`Promotion::Ceiling`] reaches. See [`Bracket::new`].
    pub const fn new(floor: [u32; 3], ceiling: [u32; 3]) -> Self {
        Self {
            floor,
            step: floor,
            ceiling,
        }
    }

    /// The rung `promotion` buys.
    pub fn at(&self, promotion: Promotion) -> [u32; 3] {
        let raw = match promotion {
            Promotion::Floor => self.floor,
            Promotion::Step => self.step,
            Promotion::Ceiling => self.ceiling,
        };
        let mut out = [0u32; 3];
        for axis in 0..3 {
            out[axis] = raw[axis]
                .max(self.floor[axis])
                .min(self.ceiling[axis].max(self.floor[axis]));
        }
        out
    }
}

/// The same again, for the field that is a quality rather than a number.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct QualityBracket {
    /// The best quality a device that says nothing about itself may select.
    pub floor: VolumeQuality,
    /// What [`Promotion::Step`] allows.
    pub step: VolumeQuality,
    /// The best this build will ever allow on this class of machine.
    pub ceiling: VolumeQuality,
}

impl QualityBracket {
    /// One ceiling for every rung. See the type doc for why all three shipped
    /// arms are this.
    pub const fn pinned(quality: VolumeQuality) -> Self {
        Self {
            floor: quality,
            step: quality,
            ceiling: quality,
        }
    }

    /// The rung `promotion` buys, capped by the bracket's own ceiling.
    pub fn at(&self, promotion: Promotion) -> VolumeQuality {
        match promotion {
            Promotion::Floor => self.floor,
            Promotion::Step => self.step,
            Promotion::Ceiling => self.ceiling,
        }
        .capped_by(self.ceiling)
    }
}

/// Maximum number of panes on desktop.
pub const MAX_PANES_DESKTOP: usize = 6;

/// Maximum number of panes on mobile.
pub const MAX_PANES_MOBILE: usize = 4;

/// The compile-time brackets this build resolves inside.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct BudgetLimits {
    /// Which set this is, for failure messages. Not a behavioural term.
    pub name: &'static str,
    /// `squallar_radar::types::IMAGE_SIZE` — the side a static plan-view render
    /// takes at the base size.
    pub image_side_px: Bracket,
    pub long_range_image_side_px: Bracket,
    pub loop_image_side_px: Bracket,
    /// `squallar_radar::xsect::SECTION_WIDTH`.
    pub section_width_px: Bracket,
    pub concurrent_renders: Bracket,
    pub concurrent_loop_downloads: Bracket,
    pub loop_frames_held: Bracket,
    /// The loop budget, in seconds of wall clock: a frame is a volume scan, and
    /// that is 259 s on a WSR-88D in precip against 360 s on a TDWR.
    pub loop_span_secs: Bracket,
    /// What [`Self::loop_span_secs`] costs in frames at the fastest radar
    /// measured: the ceiling on the figure [`Budgets::frames_for_span`] resolves.
    pub loop_render_budget: Bracket,
    pub loop_pool_bytes: Bracket,
    pub grid_cells: CellBracket,
    pub volume_texture_bytes: Bracket,
    pub offscreen_bytes: Bracket,
    pub mirror_bytes: Bracket,
    pub render_cache_entries: Bracket,
    /// `quality::PLATFORM_CEILING`.
    pub quality_ceiling: QualityBracket,
    /// The pane cap for this class. Not a `cfg` cascade — the UI narrows at
    /// runtime by width class. It is the other half of the multiplication the
    /// whole-application texture ceiling makes.
    pub max_panes: Bracket,
    pub app_texture_ceiling_bytes: Bracket,
    /// The largest side a static plan-view raster may reach on this class,
    /// however much the adapter offers. A bracket rather than one number
    /// because a browser's class is not a `cfg`: the same wasm build runs on a
    /// blocklisted driver and on a workstation GPU, and only the adapter's own
    /// report separates them.
    pub raster_side_ceiling_px: Bracket,
}

impl BudgetLimits {
    /// The wasm32 bracket.
    pub const WASM: Self = Self {
        name: "wasm32",
        image_side_px: Bracket::pinned(squallar_radar::types::WASM_IMAGE_SIZE),
        long_range_image_side_px: Bracket::pinned(constants::WASM_LONG_RANGE_IMAGE_SIZE),
        loop_image_side_px: Bracket::pinned(constants::WASM_LOOP_IMAGE_SIZE),
        section_width_px: Bracket::pinned(squallar_radar::xsect::WASM_SECTION_WIDTH),
        concurrent_renders: Bracket::pinned(constants::WASM_MAX_CONCURRENT_RENDERS),
        concurrent_loop_downloads: Bracket::pinned(
            constants::NON_MOBILE_MAX_CONCURRENT_LOOP_DOWNLOADS,
        ),
        loop_frames_held: Bracket::pinned(constants::WASM_MAX_LOOP_FRAMES),
        loop_span_secs: Bracket::pinned(constants::WASM_LOOP_SPAN_BUDGET_SECS),
        loop_render_budget: Bracket::pinned(constants::WASM_MAX_LOOP_RENDER_BUDGET),
        loop_pool_bytes: Bracket::new(
            constants::WASM_LOOP_POOL_FLOOR_BYTES,
            constants::WASM_LOOP_POOL_CEILING_BYTES,
        ),
        // The one promotion a browser can earn. The ceiling is the mobile
        // tier's own budget, so the worst a misread can do is hand a phone
        // browser a phone's budget. The desktop tier is not offered: its cells
        // are 8x the web floor's and no browser frame has been measured.
        grid_cells: CellBracket::new(
            constants::WASM_VOLUME_GRID_CELLS,
            constants::MOBILE_VOLUME_GRID_CELLS,
        ),
        // Follows the grid it has to pay for: a promoted grid against an
        // unpromoted per-pane budget is a refused allocation.
        volume_texture_bytes: Bracket::new(
            constants::WASM_VOLUME_TEXTURE_BUDGET_BYTES,
            constants::MOBILE_VOLUME_TEXTURE_BUDGET_BYTES,
        ),
        // The offscreen's promotion is paid in fill rate, not only memory:
        // `VolumeQuality::fit` steps down until it fits. No web frame measured.
        offscreen_bytes: Bracket::pinned(constants::WASM_VOLUME_OFFSCREEN_BUDGET_BYTES),
        mirror_bytes: Bracket::pinned(constants::WASM_VOLUME_MIRROR_BYTES_MAX),
        render_cache_entries: Bracket::pinned(constants::NON_MOBILE_MAX_RENDER_CACHE_ENTRIES),
        quality_ceiling: QualityBracket::pinned(crate::quality::WASM_PLATFORM_CEILING),
        max_panes: Bracket::pinned(MAX_PANES_DESKTOP),
        // Pinned, and it is the app ceiling's own arithmetic that pins it:
        // `app_texture_bytes` here is the loop pool ceiling plus the volume
        // loop plus six offscreens, none of which this adapter measurement
        // speaks to, and `check_invariants` holds the ceiling within 1.25x of
        // that sum. Raising this alone would fail that test; raising it
        // together with a term would be raising a term nothing measured.
        app_texture_ceiling_bytes: Bracket::pinned(constants::WASM_APP_TEXTURE_BUDGET_BYTES),
        // **The second promotion a browser can earn.** See
        // `WASM_RASTER_SIDE_CEILING_PROMOTED` for the four-leg adapter
        // measurement and for why the 3D cap is the axis that separates a
        // software rasteriser from a driver. The floor is untouched, so every
        // adapter that does not clear `DESKTOP_CLASS_REPORT` — llvmpipe and
        // SwiftShader both, as measured — resolves exactly what it resolved
        // before.
        raster_side_ceiling_px: Bracket::new(
            constants::WASM_RASTER_SIDE_CEILING,
            constants::WASM_RASTER_SIDE_CEILING_PROMOTED,
        ),
    };

    /// The mobile bracket — native Android and iOS.
    pub const MOBILE: Self = Self {
        name: "mobile",
        image_side_px: Bracket::pinned(squallar_radar::types::NATIVE_IMAGE_SIZE),
        long_range_image_side_px: Bracket::pinned(constants::MOBILE_LONG_RANGE_IMAGE_SIZE),
        loop_image_side_px: Bracket::pinned(constants::MOBILE_LOOP_IMAGE_SIZE),
        section_width_px: Bracket::pinned(squallar_radar::xsect::NATIVE_SECTION_WIDTH),
        concurrent_renders: Bracket::pinned(constants::MOBILE_MAX_CONCURRENT_RENDERS),
        concurrent_loop_downloads: Bracket::pinned(constants::MOBILE_MAX_CONCURRENT_LOOP_DOWNLOADS),
        loop_frames_held: Bracket::pinned(constants::MOBILE_MAX_LOOP_FRAMES),
        loop_span_secs: Bracket::pinned(constants::MOBILE_LOOP_SPAN_BUDGET_SECS),
        loop_render_budget: Bracket::pinned(constants::MOBILE_MAX_LOOP_RENDER_BUDGET),
        loop_pool_bytes: Bracket::new(
            constants::MOBILE_LOOP_POOL_FLOOR_BYTES,
            constants::MOBILE_LOOP_POOL_CEILING_BYTES,
        ),
        // Every field of this bracket is pinned: aarch64 is entirely
        // unmeasured, and macOS/iOS/Android all report `IntegratedGpu` or
        // `Other`. Open item: `docs/cross-platform-resource-limits.md` §7.4.
        grid_cells: CellBracket::pinned(constants::MOBILE_VOLUME_GRID_CELLS),
        volume_texture_bytes: Bracket::pinned(constants::MOBILE_VOLUME_TEXTURE_BUDGET_BYTES),
        offscreen_bytes: Bracket::pinned(constants::MOBILE_VOLUME_OFFSCREEN_BUDGET_BYTES),
        mirror_bytes: Bracket::pinned(constants::MOBILE_VOLUME_MIRROR_BYTES_MAX),
        render_cache_entries: Bracket::pinned(constants::MOBILE_MAX_RENDER_CACHE_ENTRIES),
        quality_ceiling: QualityBracket::pinned(crate::quality::MOBILE_PLATFORM_CEILING),
        max_panes: Bracket::pinned(MAX_PANES_MOBILE),
        app_texture_ceiling_bytes: Bracket::pinned(constants::MOBILE_APP_TEXTURE_BUDGET_BYTES),
        raster_side_ceiling_px: Bracket::pinned(constants::MOBILE_RASTER_SIDE_CEILING),
    };

    /// The desktop bracket.
    pub const DESKTOP: Self = Self {
        name: "desktop",
        image_side_px: Bracket::pinned(squallar_radar::types::NATIVE_IMAGE_SIZE),
        long_range_image_side_px: Bracket::pinned(constants::DESKTOP_LONG_RANGE_IMAGE_SIZE),
        loop_image_side_px: Bracket::pinned(constants::DESKTOP_LOOP_IMAGE_SIZE),
        section_width_px: Bracket::pinned(squallar_radar::xsect::NATIVE_SECTION_WIDTH),
        concurrent_renders: Bracket::pinned(constants::DESKTOP_MAX_CONCURRENT_RENDERS),
        concurrent_loop_downloads: Bracket::pinned(
            constants::NON_MOBILE_MAX_CONCURRENT_LOOP_DOWNLOADS,
        ),
        loop_frames_held: Bracket::pinned(constants::DESKTOP_MAX_LOOP_FRAMES),
        loop_span_secs: Bracket::pinned(constants::DESKTOP_LOOP_SPAN_BUDGET_SECS),
        loop_render_budget: Bracket::pinned(constants::DESKTOP_MAX_LOOP_RENDER_BUDGET),
        loop_pool_bytes: Bracket::new(
            constants::DESKTOP_LOOP_POOL_FLOOR_BYTES,
            constants::DESKTOP_LOOP_POOL_CEILING_BYTES,
        ),
        // Pinned: `squallar_radar::voxel::VOXEL_TEXTURE_BUDGET_BYTES` is one
        // byte per cell of the largest index plane, the desktop tier is exactly
        // at it, and `decode_job` refuses a request past it at the wire.
        grid_cells: CellBracket::pinned(constants::DESKTOP_VOLUME_GRID_CELLS),
        volume_texture_bytes: Bracket::pinned(constants::DESKTOP_VOLUME_TEXTURE_BUDGET_BYTES),
        // 20 MiB pays for the 2560 x 1440 reference pane at `Native` and
        // nothing larger; the ceiling is a 4K pane (31.64 MiB) plus the same
        // 1.4x headroom, rounded to 48 MiB. Measured on a 3090: 0.766 ms for
        // the cloud rung at 1440 x 900, fetch-bound and linear in covered
        // pixels, so 4K native is ~4.9 ms of a 16.7 ms frame. The step stays at
        // the floor — an integrated desktop GPU extrapolates to 12-23 ms at
        // 1440 x 900.
        offscreen_bytes: Bracket::new(
            constants::DESKTOP_VOLUME_OFFSCREEN_BUDGET_BYTES,
            constants::DESKTOP_VOLUME_OFFSCREEN_CEILING_BYTES,
        ),
        mirror_bytes: Bracket::pinned(constants::DESKTOP_VOLUME_MIRROR_BYTES_MAX),
        render_cache_entries: Bracket::pinned(constants::NON_MOBILE_MAX_RENDER_CACHE_ENTRIES),
        quality_ceiling: QualityBracket::pinned(crate::quality::DESKTOP_PLATFORM_CEILING),
        max_panes: Bracket::pinned(MAX_PANES_DESKTOP),
        // Moves because the offscreen bracket did. Both rungs are named
        // constants argued in bytes, so the snugness test bites at each rung.
        app_texture_ceiling_bytes: Bracket::new(
            constants::DESKTOP_APP_TEXTURE_BUDGET_BYTES,
            constants::DESKTOP_APP_TEXTURE_CEILING_BYTES,
        ),
        raster_side_ceiling_px: Bracket::pinned(constants::DESKTOP_RASTER_SIDE_CEILING),
    };

    /// The bracket this build compiled.
    #[cfg(target_arch = "wasm32")]
    pub const fn for_target() -> Self {
        Self::WASM
    }

    #[cfg(all(not(target_arch = "wasm32"), mobile))]
    pub const fn for_target() -> Self {
        Self::MOBILE
    }

    #[cfg(all(not(target_arch = "wasm32"), not(mobile)))]
    pub const fn for_target() -> Self {
        Self::DESKTOP
    }

    /// The three shipped brackets, whatever this target compiled.
    pub const SHIPPED: [Self; 3] = [Self::WASM, Self::MOBILE, Self::DESKTOP];
}

/// What every subsystem is handed instead of a `cfg` constant.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Budgets {
    /// The bracket set this came out of, for failure messages.
    pub name: &'static str,
    /// How far up its bracket each field was spent.
    pub promotion: Promotion,
    /// Rungs of the ladder surrendered before this was resolved. See [`demote`].
    pub steps_back: u32,
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
    /// **The loop budget**: the wall clock one loop keeps ready to draw.
    /// [`Budgets::frames_for_span`] converts it at the site's own cadence.
    pub loop_span_secs: usize,
    /// Frames a loop may keep *textured* — what [`Self::loop_span_secs`] costs
    /// at the fastest radar measured, so the ceiling on the per-site figure
    /// rather than the figure itself.
    pub loop_render_budget: usize,
    /// The loop pool's floor. A pair rather than one resolved figure because
    /// `LoopPool::for_device` picks inside it and `back_off` walks back down it.
    pub loop_pool_floor_bytes: usize,
    /// The loop pool's ceiling. See [`Self::loop_pool_floor_bytes`].
    pub loop_pool_ceiling_bytes: usize,
    /// The voxel grid **cell budget** — the count every allocation is sized
    /// against, not the shape that gets built. See [`Self::grid_shape`].
    pub grid_cells: [u32; 3],
    /// Ceiling on one pane's 3D volume textures.
    pub volume_texture_bytes: usize,
    /// Ceiling on **every attachment** of the pane-sized target one volume
    /// raymarches into, not on the colour target alone: `VolumeQuality::fit`
    /// prices a pane against `quality::GroundPass::bytes_per_pixel`, so a pane
    /// drawing 3D ground is fitted four times smaller out of this same figure
    /// rather than being allowed to spend four times it.
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
    /// The largest side a static plan-view raster may reach on this class.
    pub raster_side_ceiling_px: usize,
}

impl Budgets {
    /// Ceiling on the resident voxel grids a 3D loop may hold, application-wide.
    pub fn volume_loop_bytes(&self) -> usize {
        self.loop_pool_floor_bytes
    }

    /// Bytes one loop frame's texture occupies: RGBA at the loop side squared.
    pub fn loop_frame_bytes(&self) -> usize {
        self.loop_image_side_px * self.loop_image_side_px * 4
    }

    /// The bytes the shared render cache's entries may occupy between them,
    /// which is the bound that actually holds on it.
    pub fn render_cache_budget_bytes(&self) -> usize {
        self.render_cache_entries * constants::raster_bytes(self.long_range_image_side_px)
    }

    /// Bytes one **static** pane render's texture occupies, worst case: the
    /// raster ceiling, since that is the most a device on this class can be
    /// asked to hold.
    pub fn static_frame_bytes(&self) -> usize {
        self.raster_side_ceiling_px * self.raster_side_ceiling_px * 4
    }

    /// The largest static plan-view raster a device reporting
    /// `max_texture_dimension_2d` may be asked for.
    pub fn raster_side_for_adapter(&self, max_texture_dimension_2d: u32) -> usize {
        let reported = max_texture_dimension_2d as usize;
        Bracket::new(self.long_range_image_side_px, self.raster_side_ceiling_px)
            .hold(reported / 2)
            .min(reported.max(1))
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

    /// Frames of `cadence_secs` apiece it takes to cover [`Self::loop_span_secs`].
    pub fn frames_for_span(&self, cadence_secs: Option<u32>) -> usize {
        let Some(cadence) = cadence_secs.filter(|secs| *secs > 0) else {
            return self.loop_render_budget;
        };
        (1 + self.loop_span_secs / cadence as usize).clamp(
            constants::MIN_LOOP_FRAMES_PER_PANE,
            self.loop_render_budget
                .max(constants::MIN_LOOP_FRAMES_PER_PANE),
        )
    }

    /// The grid shape to **request** on a device whose 3D textures may be
    /// `max_axis` on a side.
    pub fn grid_shape(&self, max_axis: u32) -> squallar_radar::voxel::VoxelShape {
        constants::volume_grid_shape_of(self.grid_cells, max_axis)
    }

    /// Every GPU texture the application budgets at once, worst case.
    ///
    /// The offscreen term is `max_panes * offscreen_bytes` and stays that
    /// whether or not the panes draw 3D ground, because
    /// [`Self::offscreen_bytes`] is a ceiling on the whole attachment set and
    /// `VolumeQuality::fit` is what holds a ground-drawing pane to it. If that
    /// ever stops being true — if a pane is allowed to add attachments on top
    /// of the figure it was fitted against — this sum understates by three
    /// times the colour target per pane.
    pub fn app_texture_bytes(&self) -> usize {
        self.loop_pool_ceiling_bytes
            + self.volume_loop_bytes()
            + self.max_panes * self.offscreen_bytes
    }
}

/// The budgets this device gets.
pub fn resolve(profile: &DeviceProfile) -> Budgets {
    let limits = &profile.limits;
    let promotion = profile.promotion();
    let mut budgets = Budgets {
        name: limits.name,
        promotion,
        steps_back: 0,
        image_side_px: limits.image_side_px.at(promotion),
        long_range_image_side_px: limits.long_range_image_side_px.at(promotion),
        loop_image_side_px: limits.loop_image_side_px.at(promotion),
        section_width_px: limits.section_width_px.at(promotion),
        concurrent_renders: limits.concurrent_renders.at(promotion),
        concurrent_loop_downloads: limits.concurrent_loop_downloads.at(promotion),
        loop_frames_held: limits.loop_frames_held.at(promotion),
        loop_span_secs: limits.loop_span_secs.at(promotion),
        loop_render_budget: limits.loop_render_budget.at(promotion),
        loop_pool_floor_bytes: limits.loop_pool_bytes.floor,
        loop_pool_ceiling_bytes: limits
            .loop_pool_bytes
            .ceiling
            .max(limits.loop_pool_bytes.floor),
        grid_cells: limits.grid_cells.at(promotion),
        volume_texture_bytes: limits.volume_texture_bytes.at(promotion),
        offscreen_bytes: limits.offscreen_bytes.at(promotion),
        mirror_bytes: limits.mirror_bytes.at(promotion),
        render_cache_entries: limits.render_cache_entries.at(promotion),
        quality_ceiling: limits.quality_ceiling.at(promotion),
        max_panes: limits.max_panes.at(promotion),
        app_texture_ceiling_bytes: limits.app_texture_ceiling_bytes.at(promotion),
        raster_side_ceiling_px: limits
            .raster_side_ceiling_px
            .at(promotion)
            .max(limits.long_range_image_side_px.floor),
    };
    demote(&mut budgets, limits, profile.steps_back());
    budgets
}

/// Walk `steps` rungs down the degradation ladder, in the order
/// `docs/cross-platform-resource-limits.md` §4.3 fixes.
pub fn demote(budgets: &mut Budgets, limits: &BudgetLimits, steps: u32) {
    /// One rung: mutate, and say whether anything actually moved.
    type Rung = fn(&mut Budgets, &BudgetLimits) -> bool;

    const LADDER: [Rung; 4] = [
        |b, _| {
            let cheaper = b.quality_ceiling.shading.cheaper_of(GradientShading::Off);
            let moved = cheaper != b.quality_ceiling.shading;
            b.quality_ceiling.shading = cheaper;
            moved
        },
        |b, limits| {
            let coarser = b.quality_ceiling.resolution.next_coarser();
            let floor = limits.offscreen_bytes.floor;
            let moved = coarser.is_some() || b.offscreen_bytes > floor;
            if let Some(coarser) = coarser {
                b.quality_ceiling.resolution = coarser;
            }
            b.offscreen_bytes = floor;
            // The bound the offscreen's promotion moved comes back with it.
            b.app_texture_ceiling_bytes = limits.app_texture_ceiling_bytes.floor;
            moved
        },
        |b, limits| {
            let moved = b.grid_cells != limits.grid_cells.floor;
            b.grid_cells = limits.grid_cells.floor;
            b.volume_texture_bytes = limits.volume_texture_bytes.floor;
            moved
        },
        |b, limits| {
            let floor = limits.long_range_image_side_px.floor;
            let moved = b.raster_side_ceiling_px > floor;
            b.raster_side_ceiling_px = floor;
            moved
        },
    ];

    budgets.steps_back = steps;
    for _ in 0..steps {
        // The *first rung that moves*, not the nth rung.
        for rung in LADDER {
            if rung(budgets, limits) {
                break;
            }
        }
    }
}

#[path = "budget/tests.rs"]
#[cfg(test)]
mod tests;
