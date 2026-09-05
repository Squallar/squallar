//! One device profile in, one immutable set of budgets out.

use crate::constants;
use crate::quality::{DeviceClass, GradientShading, VolumeQuality};
use crate::scene::{Capacity, CapacitySource};

/// Which APIs exist. **Not** which machine this is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Platform {
    /// A native build: Vulkan, Metal or DX12, real threads, real limits.
    Native,
    /// A browser: WebGL2 or WebGPU, one thread on the page, and no *measured*
    /// memory signal — `navigator.deviceMemory` is a declaration, and only
    /// some browsers make it.
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
    /// One rung. What an integrated GPU takes — and what a desktop-class
    /// adapter report earns on its own, before the device's shape and its
    /// memory declaration are asked about the ceiling.
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
    /// Measured VRAM, where a trustworthy signal exists — read beside wgpu,
    /// which reports no capacity on any backend: a Vulkan device-local heap
    /// sum or DXGI's budget on a discrete card, Metal's recommended working
    /// set on every class. `None` on every browser and on every adapter
    /// without a reader. Spent by [`Self::capacity`], and by nothing in
    /// [`resolve`].
    pub vram_bytes: Option<u64>,
    /// **Measured** system RAM: `/proc/meminfo`, `NSProcessInfo`,
    /// `GlobalMemoryStatusEx`. `None` where no API answers, which is every
    /// browser. Stands in for the GPU's own figure on a unified-memory part
    /// ([`Self::gpu_capacity_bytes`]); read by nothing in [`resolve`].
    pub system_ram_bytes: Option<u64>,
    /// **Declared** system RAM — a browser's `navigator.deviceMemory`, a
    /// coarse bucket the page reports about itself. Kept apart from
    /// [`Self::system_ram_bytes`] because a declaration is a hint and never a
    /// bound; nothing may treat the two as one figure.
    pub declared_ram_bytes: Option<u64>,
    /// Threads the host reports: `available_parallelism()` natively,
    /// `navigator.hardwareConcurrency` in a browser. `None` where nothing
    /// reported one — unknown is not spelled `1`.
    pub parallelism: Option<usize>,
    /// What the platform can say about the shape of the device: a build fact
    /// natively, a pointer-media classification in a browser.
    pub form_factor: Option<FormFactor>,
    /// **The maximum this instance's wasm linear memory was constructed
    /// with**, in bytes. `None` natively, and `None` in a browser instance
    /// nobody told.
    ///
    /// Outranks [`BudgetLimits::presumed_host_bytes`] in [`Self::capacity`],
    /// and that is the whole point of it: the bracket's figure is the bound
    /// the module was LINKED with, and the instance's is the wall this device
    /// actually got, which may be smaller. Nothing in [`resolve`] reads it —
    /// like every other reading here it is spent by `fit`, through
    /// [`Self::capacity`].
    pub linear_memory_max_bytes: Option<u64>,
    /// **The host pool this session may take a share of**, in bytes: what the
    /// OS says is available plus what this process already holds
    /// ([`crate::scene::host_pool_bytes`], which carries the arithmetic and
    /// the reason the two terms are summed). `None` where no reader answered
    /// — every browser, and any native build whose reader failed.
    ///
    /// **Outranks [`Self::system_ram_bytes`] as the host figure**, and that is
    /// the whole point of it: the total is what the machine has, this is what
    /// it would give. A percentage of the total is imaginary budget on a box
    /// whose memory is already spoken for.
    ///
    /// A time-varying reading, unlike every other field here: it moves with
    /// every other program on the machine, so it is re-read on the telemetry
    /// tick rather than once at construction. Nothing in [`resolve`] reads it
    /// — like every other reading here it is spent by `fit`, through
    /// [`Self::capacity`].
    pub host_pool_bytes: Option<u64>,
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
    ///
    /// The report separates a driver from a software rasteriser — both axes of
    /// [`DESKTOP_CLASS_REPORT`], and nothing below it leaves the floor. It does
    /// not separate a workstation from a tablet with a trackpad, and the form
    /// factor is what does: a desktop-class report earns the
    /// [`Promotion::Ceiling`] only on a device with a fine pointer that has not
    /// declared a handheld's memory. The same report on a handheld, on a device
    /// whose shape nobody classified, or beside a small `deviceMemory`
    /// declaration earns the [`Promotion::Step`]. The declaration can only
    /// lower the rung, never raise it
    /// ([`constants::DECLARED_RAM_HANDHELD_BYTES`]).
    fn reported_promotion(&self) -> Promotion {
        if !self.reports_desktop_class() {
            return Promotion::Floor;
        }
        let declared_small = self
            .declared_ram_bytes
            .is_some_and(|bytes| bytes <= constants::DECLARED_RAM_HANDHELD_BYTES);
        if self.form_factor == Some(FormFactor::Desktop) && !declared_small {
            Promotion::Ceiling
        } else {
            Promotion::Step
        }
    }

    /// Whether the adapter's report clears both axes of [`DESKTOP_CLASS_REPORT`]
    /// — the line that separates a real driver from a software rasteriser, on
    /// the one signal every backend and every browser gives.
    pub fn reports_desktop_class(&self) -> bool {
        self.adapter.max_texture_dimension_2d >= DESKTOP_CLASS_REPORT.max_texture_dimension_2d
            && self.adapter.max_texture_dimension_3d
                >= DESKTOP_CLASS_REPORT.max_texture_dimension_3d
    }

    /// Rungs of the ladder this machine has already surrendered. See [`demote`].
    fn steps_back(&self) -> u32 {
        self.memo.map_or(0, |memo| memo.steps_back)
    }
}

impl DeviceProfile {
    /// **The GPU capacity this profile's readings amount to**, or `None` where
    /// nothing it carries is a measurement of the GPU. Matches on what the
    /// adapter and the platform say, never on a `cfg`.
    ///
    /// A software or virtual adapter is `None` whatever it reads: a reading
    /// does not un-rasterise a rasteriser, and llvmpipe's "24 GiB" is the
    /// host's RAM wearing a heap flag. A browser is `None` whatever it reads:
    /// nothing a page reports about memory is a measurement, and
    /// `deviceMemory` is a declaration capped at 8 GiB. A native discrete card
    /// is its measured VRAM — the Vulkan heap sum, DXGI's budget, Metal's
    /// working set — and nothing else, so a card whose reader answered nothing
    /// stays presumed. A native integrated part is unified memory: Metal's
    /// figure where Metal answered, else the host's RAM over
    /// [`constants::UNIFIED_MEMORY_GPU_DIVISOR`]. A native adapter the driver
    /// would not class is believed as unified memory only when its report
    /// clears the desktop-class line — a 3090 over GL is `Other` to wgpu — and
    /// is `None` below it.
    pub fn gpu_capacity_bytes(&self) -> Option<u64> {
        match (self.platform, self.class) {
            (_, DeviceClass::Software | DeviceClass::Virtual) => None,
            (Platform::Web, _) => None,
            (Platform::Native, DeviceClass::Discrete) => self.vram_bytes,
            (Platform::Native, DeviceClass::Integrated) => self.unified_capacity(),
            (Platform::Native, DeviceClass::Unknown) if self.reports_desktop_class() => {
                self.unified_capacity()
            }
            (Platform::Native, DeviceClass::Unknown) => None,
        }
    }

    /// What a unified-memory part is taken to hold: a reader's own figure
    /// where one answered, else the host's RAM over the divisor.
    fn unified_capacity(&self) -> Option<u64> {
        self.vram_bytes.or_else(|| {
            self.system_ram_bytes
                .map(|ram| ram / constants::UNIFIED_MEMORY_GPU_DIVISOR)
        })
    }

    /// **What the device can hold, as this profile knows it**: a measured
    /// capacity where [`Self::gpu_capacity_bytes`] answers, carrying the
    /// host's figure beside it, else the bracket's presumption
    /// ([`Capacity::presumed`]). The one function the application asks; the
    /// probed arm is a browser's to fill, and no profile produces it.
    ///
    /// **The two pools are decoupled.** The host figure used to ride on the
    /// GPU arm — set only where a VRAM reader had answered — so a native
    /// machine with no readable card carried no host capacity at all and
    /// `fit`'s host arm was inert on it. [`Self::host_pool_bytes`] now stands
    /// on both arms: which pool was read says nothing about the other. Each
    /// arm keeps its own fallback beneath it, so a profile with no pool
    /// reading answers exactly what it answered before one existed.
    ///
    /// **Total RAM is the measured arm's last resort and is not promoted onto
    /// the presumed arm.** It is kept so no machine loses the figure it had;
    /// three quarters of a machine's *total* RAM is precisely the imaginary
    /// budget the available reader was written to replace, and a profile that
    /// knows only the total goes on saying nothing about the host.
    pub fn capacity(&self) -> Capacity {
        match self.gpu_capacity_bytes() {
            Some(gpu_bytes) => Capacity {
                gpu_bytes,
                host_bytes: self.host_pool_bytes.or(self.system_ram_bytes),
                source: CapacitySource::Measured,
            },
            None => {
                let mut presumed = Capacity::presumed(&self.limits);
                // **The pool, else the instance's own wall, outranks the
                // bracket's presumption.** The bracket states what the module
                // was LINKED with; a browser page chooses its memory's
                // maximum per device below that bound before the module is
                // instantiated, and it is that figure the scene has to fit
                // inside. The two never compete — no browser has a pool
                // reader and no native build has a linear memory — and a
                // profile nobody told keeps the bracket's presumption, which
                // is every native arm and every pre-plumbing test.
                if let Some(bytes) = self.host_pool_bytes.or(self.linear_memory_max_bytes) {
                    presumed.host_bytes = Some(bytes);
                }
                presumed
            }
        }
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
            declared_ram_bytes: None,
            parallelism: None,
            form_factor: None,
            linear_memory_max_bytes: None,
            host_pool_bytes: None,
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

    /// All three rungs, named. See [`Bracket::stepped`].
    pub const fn stepped(floor: [u32; 3], step: [u32; 3], ceiling: [u32; 3]) -> Self {
        Self {
            floor,
            step,
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
    /// Bytes the building geometry on one pane may occupy: the row
    /// `squallar_buildings`' prism ladder is fitted inside, resolved here so
    /// the figure a job is dispatched with is a budget and not a constant the
    /// worker chose for itself. Pinned on every arm at the one machine's
    /// measurement (`constants::DESKTOP_PRISM_GEOMETRY_BYTES`).
    pub prism_geometry_bytes: Bracket,
    /// Host bytes the basemap's **styled** tile entries may occupy beyond the
    /// working set — the LRU history a pan comes back to. The argument for
    /// every tile figure is written once, on
    /// [`constants::WASM_TILE_STYLED_BYTES`].
    pub tile_styled_bytes: Bracket,
    /// Host bytes the basemap's **parsed** geometry may occupy: the
    /// style-independent decodes a theme flip restyles from without a fetch.
    /// Economy through and through — an evicted parse is one refetch on the
    /// next restyle, never a frame.
    pub tile_parsed_bytes: Bracket,
    /// Bytes the **terrain** hillshade's raster tiles may occupy beyond their
    /// working set. GPU textures, priced here and omitted from
    /// [`Budgets::app_texture_bytes`] by name — see the constants' doc.
    pub tile_terrain_bytes: Bracket,
    /// What the three tile allowances may sum to at each rung, the way
    /// [`Self::app_texture_ceiling_bytes`] bounds the GPU sum; `check_budgets`
    /// holds it within 1.25x of that sum.
    pub tile_host_ceiling_bytes: Bracket,
    /// **What the host memory is presumed to hold where nothing reads it.**
    /// `Some` only on the wasm32 bracket: the page's linear memory has a
    /// ceiling the module header declares
    /// ([`constants::WASM_LINEAR_MEMORY_MAX_BYTES`]) — read, never probed — so
    /// a browser is the one platform whose host capacity is *known* without a
    /// reader. A native bracket says nothing here; its RAM reaches
    /// [`Capacity`] through the profile's own `system_ram_bytes` on the
    /// measured arm, and on the presumed arm the host is unbounded, as it
    /// always was.
    ///
    /// **This is the bound the module was LINKED with, and it is the ceiling
    /// of the per-device choice rather than the choice itself.** A page picks
    /// its memory's maximum at or below it before the module is instantiated
    /// and tells the app what it picked, and
    /// [`DeviceProfile::linear_memory_max_bytes`] outranks this figure
    /// wherever one arrived.
    pub presumed_host_bytes: Option<usize>,
}

/// The three tile allowances a set of budgets hands the tile caches, in
/// bytes — what `squallar_egui::tiles::MapTileState` is told every frame
/// through `FrameInputs`. `u64` because the caches count resident bytes in it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TileCacheBudget {
    /// The basemap source's styled entries. See [`BudgetLimits::tile_styled_bytes`].
    pub styled_bytes: u64,
    /// The basemap source's parsed geometry. See [`BudgetLimits::tile_parsed_bytes`].
    pub parsed_bytes: u64,
    /// The terrain source's rasters. See [`BudgetLimits::tile_terrain_bytes`].
    pub terrain_bytes: u64,
    /// Whether the ladder's tile-sharpness rung is taken —
    /// [`Budgets::tile_whole_zoom`], the scene-level input to each source's
    /// snap decision (`squallar_egui::tile_source::snap`). It rides with the
    /// allowances because it is decided where they are, by `fit` against the
    /// same capacity, and consumed where they are, per source per frame.
    pub whole_zoom: bool,
}

/// A bracket from a `[floor, step, ceiling]` triple, the shape the tile
/// allowances are written in.
const fn rungs(triple: [usize; 3]) -> Bracket {
    Bracket::stepped(triple[0], triple[1], triple[2])
}

impl BudgetLimits {
    /// The wasm32 bracket.
    ///
    /// Every field that moves here is `stepped(floor, ceiling, ceiling)`: the
    /// step **is** the ceiling. A browser has two rungs above the floor to earn
    /// — [`Promotion::Step`] on a desktop-class adapter report alone,
    /// [`Promotion::Ceiling`] when the device also has a desktop form factor
    /// and no small memory declaration — and today they buy the same numbers,
    /// so a browser that resolves `Step` gets exactly what it resolved as
    /// `Ceiling` before the form factor was read. The `Ceiling` rung is the
    /// slot a measured or probed desktop-browser tier fills later; until a
    /// browser frame has been measured at anything above the mobile tier it
    /// equals the step, and nothing here moves, by construction. Pinned by
    /// `the_web_step_is_todays_ceiling_until_a_desktop_browser_tier_is_measured`.
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
        loop_pool_bytes: Bracket::stepped(
            constants::WASM_LOOP_POOL_FLOOR_BYTES,
            constants::WASM_LOOP_POOL_CEILING_BYTES,
            constants::WASM_LOOP_POOL_CEILING_BYTES,
        ),
        // The one promotion a browser can earn, at either rung. Both rungs are
        // the mobile tier's own budget, so the worst a misread can do is hand a
        // phone browser a phone's budget. The desktop tier is not offered: its
        // cells are 8x the web floor's and no browser frame has been measured.
        grid_cells: CellBracket::stepped(
            constants::WASM_VOLUME_GRID_CELLS,
            constants::MOBILE_VOLUME_GRID_CELLS,
            constants::MOBILE_VOLUME_GRID_CELLS,
        ),
        // Follows the grid it has to pay for: a promoted grid against an
        // unpromoted per-pane budget is a refused allocation.
        volume_texture_bytes: Bracket::stepped(
            constants::WASM_VOLUME_TEXTURE_BUDGET_BYTES,
            constants::MOBILE_VOLUME_TEXTURE_BUDGET_BYTES,
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
        // **The second promotion a browser can earn**, again at either rung.
        // See `WASM_RASTER_SIDE_CEILING_PROMOTED` for the four-leg adapter
        // measurement and for why the 3D cap is the axis that separates a
        // software rasteriser from a driver. The floor is untouched, so every
        // adapter that does not clear `DESKTOP_CLASS_REPORT` — llvmpipe and
        // SwiftShader both, as measured — resolves exactly what it resolved
        // before.
        raster_side_ceiling_px: Bracket::stepped(
            constants::WASM_RASTER_SIDE_CEILING,
            constants::WASM_RASTER_SIDE_CEILING_PROMOTED,
            constants::WASM_RASTER_SIDE_CEILING_PROMOTED,
        ),
        prism_geometry_bytes: Bracket::pinned(constants::WASM_PRISM_GEOMETRY_BYTES),
        // Host-heap figures with a real step: a desktop-class adapter report
        // buys a longer tile history. The ceiling is the step, as on every
        // other field here, until a desktop-browser tier is measured — see
        // the constants' doc for the figures that tier would fill.
        tile_styled_bytes: rungs(constants::WASM_TILE_STYLED_BYTES),
        tile_parsed_bytes: rungs(constants::WASM_TILE_PARSED_BYTES),
        tile_terrain_bytes: rungs(constants::WASM_TILE_TERRAIN_BYTES),
        tile_host_ceiling_bytes: rungs(constants::WASM_TILE_HOST_CEILING_BYTES),
        // The bound the module header declares. A page that told us what its
        // own memory was built with outranks this — see the field's doc.
        presumed_host_bytes: Some(constants::WASM_LINEAR_MEMORY_MAX_BYTES as usize),
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
        prism_geometry_bytes: Bracket::pinned(constants::MOBILE_PRISM_GEOMETRY_BYTES),
        // Pinned at the wasm floor, like every other mobile field: unmeasured.
        tile_styled_bytes: Bracket::pinned(constants::MOBILE_TILE_STYLED_BYTES),
        tile_parsed_bytes: Bracket::pinned(constants::MOBILE_TILE_PARSED_BYTES),
        tile_terrain_bytes: Bracket::pinned(constants::MOBILE_TILE_TERRAIN_BYTES),
        tile_host_ceiling_bytes: Bracket::pinned(constants::MOBILE_TILE_HOST_CEILING_BYTES),
        // A native heap has no declared ceiling; RAM reaches the capacity
        // through the profile's reading where one answers.
        presumed_host_bytes: None,
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
        prism_geometry_bytes: Bracket::pinned(constants::DESKTOP_PRISM_GEOMETRY_BYTES),
        tile_styled_bytes: rungs(constants::DESKTOP_TILE_STYLED_BYTES),
        tile_parsed_bytes: rungs(constants::DESKTOP_TILE_PARSED_BYTES),
        tile_terrain_bytes: rungs(constants::DESKTOP_TILE_TERRAIN_BYTES),
        tile_host_ceiling_bytes: rungs(constants::DESKTOP_TILE_HOST_CEILING_BYTES),
        presumed_host_bytes: None,
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
    /// Rungs of the ladder surrendered to produce this: what a memo asked
    /// [`demote`] for, plus every rung `crate::fit::fit` took to make the
    /// scene fit its capacity. A count of what was shed, never a position to
    /// remember.
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
    /// the pool is held inside it: what the loops need, capped by the room the
    /// rest of the scene leaves under the capacity (`crate::fit::loop_pool_bytes`),
    /// never below this and never above the ceiling.
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
    /// Whether map tiles are drawn at the whole zoom below the fractional one
    /// — fewer, larger tiles covering the same glass. The tile-sharpness rung
    /// of the ladder sets it; `false` at every class rung. Delivered to every
    /// tile source through [`TileCacheBudget::whole_zoom`] as one of the two
    /// inputs to its snap decision (`squallar_egui::tile_source::snap`; the
    /// other is the source's own measured overrun), which snaps after a dwell
    /// and releases only once this is `false` again.
    pub tile_whole_zoom: bool,
    /// **How much larger than its pane a whole-picture overlay raster is
    /// planned**, per side, in percent — one of
    /// [`constants::OVERLAY_OVERSAMPLE_PERCENTS`], `150` at every class rung.
    /// The overlay-oversampling rung of the ladder steps it down one entry
    /// at a time. Delivered to the planner
    /// (`squallar_egui::overlay_cache::plan_overlay_texture`) as the overdraw
    /// fraction in force, and priced by `crate::fit::need` as host bytes for
    /// every picture a pane shows: a picture crosses the page heap on its
    /// way to the GPU, and thirteen of them at 1.5x are half a browser's
    /// linear memory.
    pub overlay_oversample_percent: u16,
    /// Bytes the building geometry on one pane may occupy -- the `vram_bytes`
    /// ceiling a `BuildingMeshJob` is to be dispatched with once a production
    /// caller exists. Today no dispatch site reads it, so nothing spends it.
    /// **Buffers, not textures**: not a term of [`Self::app_texture_bytes`],
    /// priced by `crate::fit::need` for a pane that draws buildings.
    pub prism_vram_bytes: usize,
    /// The basemap's styled-entry allowance. See [`BudgetLimits::tile_styled_bytes`].
    pub tile_styled_bytes: usize,
    /// The basemap's parsed-geometry allowance. See [`BudgetLimits::tile_parsed_bytes`].
    pub tile_parsed_bytes: usize,
    /// The terrain rasters' allowance. See [`BudgetLimits::tile_terrain_bytes`].
    pub tile_terrain_bytes: usize,
    /// What the three may sum to. See [`BudgetLimits::tile_host_ceiling_bytes`].
    pub tile_host_ceiling_bytes: usize,
}

impl Budgets {
    /// The three tile allowances, as the tile caches take them.
    pub fn tile_cache(&self) -> TileCacheBudget {
        TileCacheBudget {
            styled_bytes: self.tile_styled_bytes as u64,
            parsed_bytes: self.tile_parsed_bytes as u64,
            terrain_bytes: self.tile_terrain_bytes as u64,
            whole_zoom: self.tile_whole_zoom,
        }
    }

    /// Every host byte the tile caches may hold between them, worst case: the
    /// figure [`Self::tile_host_ceiling_bytes`] bounds. Not a term of
    /// [`Self::app_texture_bytes`] — see the constants' doc for the omission.
    pub fn tile_host_bytes(&self) -> usize {
        self.tile_styled_bytes + self.tile_parsed_bytes + self.tile_terrain_bytes
    }

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
        self.frames_for_span_of(self.loop_span_secs, cadence_secs)
    }

    /// Frames of `cadence_secs` apiece it takes to cover `span_secs` of a
    /// pane's own lookback, held to [`Self::loop_span_secs`] and to the render
    /// budget — the same clamp as [`Self::frames_for_span`], with the pane's
    /// span in place of the budget's. A loop with no cadence yet buys the whole
    /// render budget, as it always has.
    pub fn frames_for_span_of(&self, span_secs: usize, cadence_secs: Option<u32>) -> usize {
        let Some(cadence) = cadence_secs.filter(|secs| *secs > 0) else {
            return self.loop_render_budget;
        };
        (1 + span_secs.min(self.loop_span_secs) / cadence as usize).clamp(
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
        tile_whole_zoom: false,
        overlay_oversample_percent: constants::OVERLAY_OVERSAMPLE_PERCENTS[0],
        prism_vram_bytes: limits.prism_geometry_bytes.at(promotion),
        tile_styled_bytes: limits.tile_styled_bytes.at(promotion),
        tile_parsed_bytes: limits.tile_parsed_bytes.at(promotion),
        tile_terrain_bytes: limits.tile_terrain_bytes.at(promotion),
        tile_host_ceiling_bytes: limits.tile_host_ceiling_bytes.at(promotion),
    };
    demote(&mut budgets, limits, profile.steps_back());
    budgets
}

/// One rung's knob: mutate, and say whether anything actually moved.
type Step = fn(&mut Budgets, &BudgetLimits) -> bool;

/// **Which of the two memories a rung can give back.** A scene is fitted on
/// two axes — GPU textures and host bytes — and a rung is worth taking only
/// against an axis it lowers: shedding the loop history frees not one byte
/// of a browser's page heap, and shedding a picture's overdraw frees no VRAM
/// the need model prices. `crate::fit::fit` takes a rung only when it lowers
/// an axis that is over; [`demote`], a counted walk, takes every rung.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Lowers {
    /// The rung reduces a term of the GPU need.
    pub gpu: bool,
    /// The rung reduces a term of the host need.
    pub host: bool,
}

impl Lowers {
    const GPU: Self = Self {
        gpu: true,
        host: false,
    };
    const BOTH: Self = Self {
        gpu: true,
        host: true,
    };

    /// Whether this rung answers a scene over on `gpu_over` / `host_over`.
    pub fn answers(self, gpu_over: bool, host_over: bool) -> bool {
        (self.gpu && gpu_over) || (self.host && host_over)
    }
}

/// One rung of the ladder: its knob and the axes it lowers.
#[derive(Clone, Copy)]
pub struct Rung {
    /// The knob.
    pub step: Step,
    /// What taking it gives back.
    pub lowers: Lowers,
}

/// The degradation ladder, in the order `docs/cross-platform-resource-limits.md`
/// §4.3 fixes: degrade what the user is least likely to notice, and degrade
/// smoothly before degrading discretely. One table, walked by [`demote`] for a
/// counted step and by `crate::fit::fit` for as many steps as the scene's need
/// asks. A rung whose knob the scene does not exercise still moves the budget
/// — the budget is the ladder's position — and costs the picture nothing.
///
/// 1. **3D lighting**: the gradient shading, seven fetches a step against one.
/// 2. **3D offscreen resolution**, one coarsening a step (`Native`, `Half`,
///    `Quarter`), and the offscreen and app ceilings to their floors with it.
/// 3. **Loop history, 2D before 3D**: the render budget halves toward
///    `MIN_LOOP_FRAMES_PER_PANE`, one halving a step, so a scene that is a
///    little over sheds a little history rather than a rung of detail. A
///    shorter loop is the least destructive thing in the application — nothing
///    on screen gets worse, there is just less of it.
/// 4. **Overlay oversampling**, one entry of
///    [`constants::OVERLAY_OVERSAMPLE_PERCENTS`] a step (1.5x, 1.25x, 1x per
///    side). After the history because a shorter loop is *less of the same
///    picture* where a thinner margin is a blank strip at the leading edge of
///    a fast pan until the next raster lands — brief, and only while panning,
///    but a picture defect where the history's loss is not one. Before the
///    tiles because a softened basemap is on every frame for as long as the
///    rung holds and this costs nothing while the map stands still; and
///    because it is the largest lever per step in the table — a whole-picture
///    overlay is re-rasterised on every move at 2.25x the pane's pixels, and
///    thirteen of them at the user's canvas are 556 MB of a 1 GiB page heap.
///    Lowers **both** axes: the picture is a GPU texture as well as a page
///    buffer, even though only the host side is priced today.
/// 5. **Tile sharpness**: fewer, larger tiles cover the same glass. Above the
///    grid because a softer basemap is a softer picture and a coarser grid is
///    a wrong-looking one. Host bytes (the styled working set) — and taken
///    for the GPU axis too, as it has been since it landed: the terrain
///    rasters it shrinks are textures omitted from the GPU sum by name.
/// 6. **3D grid cells**, and the volume texture budget with them: the first
///    rung a user calls "worse".
/// 7. **Raster side**, to the long-range floor: the most visible, so last.
const LADDER: [Rung; 7] = [
    Rung {
        step: |b, _| {
            let cheaper = b.quality_ceiling.shading.cheaper_of(GradientShading::Off);
            let moved = cheaper != b.quality_ceiling.shading;
            b.quality_ceiling.shading = cheaper;
            moved
        },
        lowers: Lowers::GPU,
    },
    Rung {
        step: |b, limits| {
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
        lowers: Lowers::GPU,
    },
    Rung {
        step: |b, _| {
            let halved = (b.loop_render_budget / 2).max(constants::MIN_LOOP_FRAMES_PER_PANE);
            let moved = halved < b.loop_render_budget;
            b.loop_render_budget = halved;
            moved
        },
        lowers: Lowers::GPU,
    },
    Rung {
        step: |b, _| {
            let next = constants::OVERLAY_OVERSAMPLE_PERCENTS
                .iter()
                .copied()
                .find(|percent| *percent < b.overlay_oversample_percent);
            match next {
                Some(percent) => {
                    b.overlay_oversample_percent = percent;
                    true
                }
                None => false,
            }
        },
        lowers: Lowers::BOTH,
    },
    Rung {
        step: |b, _| {
            let moved = !b.tile_whole_zoom;
            b.tile_whole_zoom = true;
            moved
        },
        lowers: Lowers::BOTH,
    },
    Rung {
        step: |b, limits| {
            let moved = b.grid_cells != limits.grid_cells.floor;
            b.grid_cells = limits.grid_cells.floor;
            b.volume_texture_bytes = limits.volume_texture_bytes.floor;
            moved
        },
        lowers: Lowers::GPU,
    },
    Rung {
        step: |b, limits| {
            let floor = limits.long_range_image_side_px.floor;
            let moved = b.raster_side_ceiling_px > floor;
            b.raster_side_ceiling_px = floor;
            moved
        },
        lowers: Lowers::GPU,
    },
];

/// Walk `steps` rungs down the degradation ladder — each step the *first rung
/// that moves*, not the nth rung, so a machine already at a rung's stop steps
/// the next one. Total: past the last rung's stop nothing moves.
pub fn demote(budgets: &mut Budgets, limits: &BudgetLimits, steps: u32) {
    budgets.steps_back = steps;
    for _ in 0..steps {
        if !step_down(budgets, limits) {
            break;
        }
    }
}

/// One step down the ladder: the first rung that moves, and whether one did.
pub(crate) fn step_down(budgets: &mut Budgets, limits: &BudgetLimits) -> bool {
    step_down_for(budgets, limits, true, true)
}

/// One step down the ladder **against the axes that are over**: the first
/// rung that both answers an over axis ([`Lowers::answers`]) and moves, and
/// whether one did. With both axes over this is [`step_down`].
pub(crate) fn step_down_for(
    budgets: &mut Budgets,
    limits: &BudgetLimits,
    gpu_over: bool,
    host_over: bool,
) -> bool {
    LADDER
        .iter()
        .filter(|rung| rung.lowers.answers(gpu_over, host_over))
        .any(|rung| (rung.step)(budgets, limits))
}

#[path = "budget/tests.rs"]
#[cfg(test)]
mod tests;
