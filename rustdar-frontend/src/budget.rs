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
//! # What it does: spends a [`Promotion`], and only ever upward from the floor
//!
//! A device that says nothing about itself still takes every floor, and the
//! floors are the shipped constants — `the_resolver_reproduces_every_shipped_constant`
//! puts the two side by side field for field, so a machine this build cannot
//! read gets exactly what it got before any of this existed.
//!
//! What a device *can* say is spent through one three-rung [`Promotion`],
//! derived once in [`DeviceProfile::promotion`] from two signals that are
//! positive evidence apiece and are available on different targets:
//!
//! * the **device class** the driver names, which is the rule
//!   `LoopPool::for_device` already ships — discrete takes the ceiling,
//!   integrated one step, anything unnamed the floor;
//! * the **ceilings the adapter reports**, which is the only signal a browser
//!   offers at all and the one that separates a desktop browser from a phone
//!   browser without asking either what it is called.
//!
//! Neither is a prerequisite for the other, so the better of the two wins: a
//! browser has no class, a desktop GPU behind GL reports `Other` and no useful
//! class either, and a discrete card behind a downlevel adapter is still
//! discrete. Taking the better answer is what lets one rule serve both
//! platforms with **no `Platform` term in it at all**.
//!
//! # And only ever downward again from failure
//!
//! [`BudgetMemo::steps_back`] is what a machine learned by refusing an
//! allocation, in rungs of the ladder `docs/cross-platform-resource-limits.md`
//! §4.3 orders — lighting, then offscreen resolution, then loop history, then
//! grid cells, then the raster side. [`demote`] walks it. A rung never crosses
//! its bracket floor, so the worst a machine that keeps failing can reach is
//! the configuration this build already shipped to it, and below that the 3D
//! view retires entirely, which `volume::degrade` already latches.
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
use crate::volume::quality::{DeviceClass, GradientShading, VolumeQuality};
use rustdar_egui::config_store::ConfigStore;

/// Key the ladder position [`BudgetMemo::steps_back`] is persisted under.
///
/// Its own `ConfigStore` entry, beside `crate::loop_pool::LOOP_POOL_KEY` and
/// for the identical reason: `autosave_config` writes the `UiConfig` blob on a
/// 3 s timer behind a string compare, so a value learned in the last three
/// seconds of a session is lost — and a session that has just lost its
/// rendering surface may not get three more seconds. One entry holding one
/// integer also means the blast radius of a corrupt value is one integer,
/// rather than every setting on the next load.
///
/// **One key for the whole struct, not one per field.** The ladder is an
/// ordering over subsystems and a per-field memo could not express it: three
/// separate counts could describe a machine that had surrendered its grid
/// without surrendering its lighting, which is a state this ladder says does
/// not exist.
pub const BUDGET_MEMO_KEY: &str = "budget_steps";

/// What a previous session learned, read back.
///
/// A decimal count of rungs and nothing else, the format
/// `crate::loop_pool::remembered` already argues for: one integer, not JSON,
/// because a format with structure gives a corrupt entry more ways to be
/// almost-readable. Anything unreadable is `None`, which is the same answer a
/// first launch gets — the cost of losing it is one re-probe, and configuration
/// is never allowed to be load-bearing.
pub fn remembered_steps(store: Option<&dyn ConfigStore>) -> Option<u32> {
    let raw = store?.load(BUDGET_MEMO_KEY)?;
    raw.trim().parse().ok().or_else(|| {
        log::warn!("budget memo is not a number ({raw:?}); starting this device at its ladder top");
        None
    })
}

/// Write what this session settled on, synchronously. See [`BUDGET_MEMO_KEY`].
pub fn remember_steps(store: Option<&dyn ConfigStore>, steps: u32) {
    let Some(store) = store else {
        return;
    };
    if let Err(e) = store.store(BUDGET_MEMO_KEY, &steps.to_string()) {
        log::warn!("could not persist the budget ladder position: {e}");
    }
}

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
/// against what the device will actually hand back.
///
/// **One thing now spends against `max_texture_dimension_2d`**, and it is the
/// worked example of how: [`Budgets::raster_side_for_adapter`] takes *half* of
/// what is reported, which is the margin those two readings force — one of them
/// overstates by a doubling and the number alone does not say which — and then
/// holds the result inside a bracket whose floor is what this build already
/// ships. So the reading can only ever add, and it can only add as far as a
/// figure that was argued in bytes rather than taken from the adapter.
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

/// The componentwise least either desktop-class machine this project has
/// **measured** a browser report on.
///
/// | machine | `max_texture_dimension_2d` | `_3d` |
/// |---|---:|---:|
/// | Firefox 153, RTX 3090 | 32768 | 16384 |
/// | Chrome 151, Radeon 890M | 16384 | 8192 |
///
/// Stated for what it is: a **measured lower bound on desktop-class**, not a
/// proven upper bound on handhelds. The two rows differ in both browser and
/// GPU, so neither is attributable to a browser alone; what they establish
/// between them is that the reported ceiling genuinely varies across real
/// devices and that the least a desktop-class one offered is this pair.
///
/// # Why no halving here, when [`Budgets::raster_side_for_adapter`] halves
///
/// Because nothing is *allocated* at this size. The 3090 reports 32768 and
/// refuses to allocate it, which is fatal to a rule that spends the figure and
/// irrelevant to one that only reads it: what gets allocated after this line is
/// crossed is the **tier's own budget**, a figure argued in bytes and already
/// shipped to hardware. Overstating a ceiling cannot overstate a budget that
/// was never derived from it.
///
/// # What a wrong answer costs
///
/// A handheld browser that reports desktop-class figures is promoted one tier,
/// and one tier up from the web floor is the budget this project already ships
/// to phones — so the cost of the misclassification is bounded by hardware that
/// runs it. Below that, `LoopPool::back_off` and [`BudgetMemo::steps_back`] are
/// the behavioural backstop, which is what actually makes the web safe: every
/// browser signal is spoofable in a line of JavaScript and a lost context is
/// not.
pub const DESKTOP_CLASS_REPORT: AdapterCeilings = AdapterCeilings {
    max_texture_dimension_2d: 16384,
    max_texture_dimension_3d: 8192,
};

/// How far up its bracket each budget may be spent on this device.
///
/// Three rungs and not a scalar, because every bracket names its middle rung
/// explicitly: a budget interpolated halfway up a pair is a number nobody
/// chose, and the whole argument for the brackets is that each figure was
/// argued somewhere. `Ord` is the rung order, which is what lets
/// [`DeviceProfile::promotion`] take the better of two signals with a `max`.
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
    ///
    /// Exactly `LoopPool::for_device`'s rule, restated once so that the pool
    /// and everything beside it cannot drift: **`Discrete`** has memory nothing
    /// else competes for; **`Integrated`** shares one pool of DRAM with the
    /// operating system and takes one step; **`Virtual`**, **`Software`** and
    /// **`Unknown`** are unknown quantities, and `Unknown` is every browser.
    pub fn for_class(class: DeviceClass) -> Self {
        match class {
            DeviceClass::Discrete => Self::Ceiling,
            DeviceClass::Integrated => Self::Step,
            DeviceClass::Virtual | DeviceClass::Unknown | DeviceClass::Software => Self::Floor,
        }
    }
}

/// What a previous session learned by **failing**.
///
/// Evidence beats classification: a figure arrived at by watching this machine
/// refuse an allocation is better than any guess from a device type, and
/// honouring it is also what keeps a reopen 1:1 rather than showing a different
/// loop length on every start. Both fields are persisted in their own
/// `ConfigStore` entries, written synchronously — `crate::loop_pool::LOOP_POOL_KEY`
/// for the pool and `crate::budget::BUDGET_MEMO_KEY` for the ladder — because a
/// value learned by crashing the GPU is exactly the value that must not be lost
/// to a 3 s autosave timer.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct BudgetMemo {
    /// The loop pool this machine settled at, in bytes. `None` where nothing
    /// has been learned, which is every first launch.
    pub loop_pool_bytes: Option<usize>,
    /// Rungs of the degradation ladder this machine has already surrendered.
    ///
    /// One per latched failure, and one-way: a device that could not serve a
    /// texture will not be able to serve it after a restart either. See
    /// [`demote`] for the ladder and for which rungs a running session can
    /// actually reach.
    pub steps_back: u32,
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
    /// How far up its bracket every budget may be spent here.
    ///
    /// **The better of two signals, and no `Platform` term.** Each is positive
    /// evidence on its own and each is missing on some target: a browser
    /// reports no class at all, this project's own RTX 3090 reports
    /// `DiscreteGpu` through Vulkan and `Other` through GL, and a discrete card
    /// behind a downlevel adapter is still discrete. A `min` would let the
    /// missing half veto the present one and pin every browser to the floor for
    /// ever, which is the complaint this whole module exists to answer.
    ///
    /// **`Software` overrides both**, and is the one class where the reports
    /// say nothing worth having: a software rasteriser will happily advertise
    /// 16384 and then take seconds a frame, and `quality::select` already puts
    /// it at the bottom of its own ladder for that reason.
    pub fn promotion(&self) -> Promotion {
        if matches!(self.class, DeviceClass::Software) {
            return Promotion::Floor;
        }
        Promotion::for_class(self.class).max(self.reported_promotion())
    }

    /// What the adapter's own reported ceilings are worth on their own.
    ///
    /// Two rungs rather than three: [`DESKTOP_CLASS_REPORT`] is a measured
    /// line and there is no second measured line to put between it and the
    /// WebGL2 guarantee, so a middle rung here would be a number nobody read
    /// off a machine.
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
///
/// The **step** between them is named rather than interpolated. Half of a pair
/// is a number nobody argued, and every figure in this file was argued
/// somewhere — usually as a tier this project already ships to real hardware,
/// which is what makes a wrong promotion a disappointment rather than a crash.
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
    ///
    /// Most brackets are still this, and saying so in one place is better than
    /// nineteen repetitions of the same triple. Raising a ceiling off its floor
    /// is what "no compromises when the hardware is available" costs, and it is
    /// a deliberate, reviewed, one-line change per field once the measurement
    /// to justify it exists — so a field that stays pinned here is a field
    /// whose measurement does not.
    pub const fn pinned(value: usize) -> Self {
        Self {
            floor: value,
            step: value,
            ceiling: value,
        }
    }

    /// A pair only a [`Promotion::Ceiling`] reaches.
    ///
    /// The step is the floor, which is the honest shape wherever the middle
    /// rung has no evidence behind it: an integrated GPU gets what it got
    /// before, rather than half of a promotion nobody measured.
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
    ///
    /// The `max` is not defensive tidiness — `clamp` **panics** on `min > max`,
    /// and these are independently editable constants, so a crossed pair would
    /// be a startup panic on one target only, which is the arm no host test can
    /// reach.
    pub fn hold(&self, value: usize) -> usize {
        value.clamp(self.floor, self.ceiling.max(self.floor))
    }

    /// The rung `promotion` buys, held inside the pair.
    ///
    /// Held rather than taken raw, so a `step` edited above the ceiling or
    /// below the floor is a budget that stays inside its bracket rather than a
    /// promotion that escapes it.
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
    ///
    /// Held per axis against the floor, so a ceiling edited below the floor on
    /// one axis cannot quietly coarsen that axis — a cell budget is three
    /// numbers and a per-axis regression is exactly the kind that hides.
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
///
/// **Every arm ships pinned, and that is a finding rather than an omission.**
/// A quality rung is paid for in *frame time*, not in bytes, and the only
/// frame-time table this project has is `volume::quality`'s, measured on one
/// RTX 3090 over Vulkan. What it already supports is shipped: the desktop
/// ceiling is `VolumeQuality::BEST` and a discrete GPU there already reaches
/// it. What it does not support is raising the other two arms —
///
/// * **mobile** is held at Half and unshaded by an *extrapolation* from that
///   table (cloud at native size lands at 23-38 ms on an integrated GPU), and
///   an extrapolation is a reason to stay put, not a reason to move;
/// * **web** is held there by the same figures plus the fact that a browser
///   reports `DeviceClass::Unknown` whatever the silicon is, so it is the
///   *class*, not this ceiling, that picks Half and unshaded. Raising the
///   ceiling above what the class picks changes nothing at all; changing what
///   the class picks is a claim about a browser's frame, and nothing has
///   measured a browser's frame.
///
/// So the mechanism is here and every rung is reachable from the host tests,
/// and the numbers wait on a measurement. Promoting a rung on a target where
/// the cost is a frame the user is panning in would trade interaction latency
/// for picture quality, which is the one trade this application does not make.
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
    /// `constants::LOOP_SPAN_BUDGET_SECS` — the loop budget, in seconds of
    /// wall clock. The one budget in this struct whose unit is not bytes,
    /// pixels or a count, and deliberately so: a frame is a volume scan and a
    /// volume scan is 259 s on a WSR-88D in precip against 360 s on a TDWR, so
    /// a frame count is not a statement about anything a user reads.
    pub loop_span_secs: Bracket,
    /// `constants::MAX_LOOP_RENDER_BUDGET` — what [`Self::loop_span_secs`]
    /// costs in frames at the fastest radar measured, and so the ceiling on the
    /// per-site figure [`Budgets::frames_for_span`] resolves.
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
    /// `volume::quality::PLATFORM_CEILING`. See [`QualityBracket`] for why
    /// every shipped arm of this one is pinned.
    pub quality_ceiling: QualityBracket,
    /// `rustdar_egui::pane`'s pane cap for this class. Not a `cfg` cascade over
    /// there — it is chosen at runtime by width class — and it is carried here
    /// because it is the other half of the multiplication the whole-application
    /// ceiling makes, and the two halves live in different crates.
    pub max_panes: Bracket,
    /// `constants::APP_TEXTURE_BUDGET_BYTES`.
    ///
    /// Deliberately **not** device-derived, now or later: the moment both sides
    /// of the snugness test move together it becomes a tautology and stops
    /// catching anything.
    ///
    /// A bracket is not that. Every rung of this one is a constant somebody
    /// argued in bytes, so raising it for a 24 GiB card is still the deliberate,
    /// reviewed change that recommendation asks for — it is simply written as a
    /// second rung rather than as an edit to the first, so the machines that do
    /// not earn the promotion keep the ceiling they were proved against. The
    /// snugness test runs at **every** rung, which is what keeps it biting:
    /// desktop is 3768 MiB against 3840 at the floor and 3936 against 4032 at
    /// the ceiling, 1.02x and 1.02x, both far inside the 1.25x line.
    ///
    /// The ceiling stays under 4096 MiB for a reason that is not aesthetic:
    /// this is a `usize`, wasm32's is 32 bits, and `BudgetLimits::DESKTOP` is a
    /// `const` compiled on every target — 4096 MiB is exactly `u32::MAX + 1`.
    pub app_texture_ceiling_bytes: Bracket,
    /// The largest side a static plan-view raster may reach on this class,
    /// however much the adapter offers.
    ///
    /// Already a ceiling, so like [`Self::quality_ceiling`] it has no pair: its
    /// floor is the long-range image side, because the whole point of
    /// reading the device is that it may only ever *add* to what this build
    /// already draws. [`Budgets::raster_side_for_adapter`] spends between them.
    ///
    /// **This is the first field whose ceiling sits off its floor**, which is
    /// the step [`Bracket::pinned`]'s doc describes as costing a measurement.
    /// That measurement is on `constants::DESKTOP_RASTER_SIDE_CEILING`; the two
    /// classes that stay pinned are pinned because no such measurement exists
    /// for them, not because they could not use one.
    pub raster_side_ceiling_px: usize,
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
        loop_span_secs: Bracket::pinned(constants::WASM_LOOP_SPAN_BUDGET_SECS),
        loop_render_budget: Bracket::pinned(constants::WASM_MAX_LOOP_RENDER_BUDGET),
        loop_pool_bytes: Bracket::new(
            constants::WASM_LOOP_POOL_FLOOR_BYTES,
            constants::WASM_LOOP_POOL_CEILING_BYTES,
        ),
        // **The one promotion a browser can earn, and the whole of what
        // separates a desktop browser from a phone browser.** The ceiling is
        // the *mobile tier's own budget* rather than a new number: a browser
        // reporting `DESKTOP_CLASS_REPORT` gets the grid this project already
        // ships to handheld hardware at the same Half-and-unshaded rung, so the
        // worst a misread can do is hand a phone browser a phone's budget. The
        // desktop tier is deliberately **not** offered here — its cells are
        // 8x the web floor's and the march steps one cell along the ray, so
        // that is a frame-time claim, and nothing has measured a browser's
        // frame.
        grid_cells: CellBracket::new(
            constants::WASM_VOLUME_GRID_CELLS,
            constants::MOBILE_VOLUME_GRID_CELLS,
        ),
        // Follows the grid it has to pay for, to the same tier. A promoted grid
        // against an unpromoted per-pane budget would be a refused allocation.
        volume_texture_bytes: Bracket::new(
            constants::WASM_VOLUME_TEXTURE_BUDGET_BYTES,
            constants::MOBILE_VOLUME_TEXTURE_BUDGET_BYTES,
        ),
        // Pinned, and not because nobody thought about it: the offscreen is the
        // one budget whose promotion is paid in *fill rate* rather than only in
        // memory — `VolumeQuality::fit` steps the rung down until it fits, so
        // raising it is what puts a 4K browser pane back at Half instead of
        // Quarter, and that is four times the march. On web there is no frame
        // measurement to justify it. See `QualityBracket`.
        offscreen_bytes: Bracket::pinned(constants::WASM_VOLUME_OFFSCREEN_BUDGET_BYTES),
        mirror_bytes: Bracket::pinned(constants::WASM_VOLUME_MIRROR_BYTES_MAX),
        render_cache_entries: Bracket::pinned(constants::NON_MOBILE_MAX_RENDER_CACHE_ENTRIES),
        quality_ceiling: QualityBracket::pinned(crate::volume::quality::WASM_PLATFORM_CEILING),
        max_panes: Bracket::pinned(rustdar_egui::pane::MAX_PANES_DESKTOP),
        app_texture_ceiling_bytes: Bracket::pinned(constants::WASM_APP_TEXTURE_BUDGET_BYTES),
        raster_side_ceiling_px: constants::WASM_RASTER_SIDE_CEILING,
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
        loop_span_secs: Bracket::pinned(constants::MOBILE_LOOP_SPAN_BUDGET_SECS),
        loop_render_budget: Bracket::pinned(constants::MOBILE_MAX_LOOP_RENDER_BUDGET),
        loop_pool_bytes: Bracket::new(
            constants::MOBILE_LOOP_POOL_FLOOR_BYTES,
            constants::MOBILE_LOOP_POOL_CEILING_BYTES,
        ),
        // **Every field of this bracket is pinned, deliberately.** aarch64 is
        // three of five targets and is entirely unmeasured: macOS, iOS and
        // Android all report `IntegratedGpu` or `Other`, so a promotion rule
        // written from a Linux box would be a guess applied to the population
        // with the least headroom. An M-series Mac with 64 GiB of unified
        // memory taking a laptop's budget is the user's complaint verbatim and
        // it stays open here — `docs/cross-platform-resource-limits.md` §7
        // item 4 is the standing note, and the answer is somebody running this
        // on Apple Silicon and on a flagship and a bargain handset, not a
        // number chosen here. What mobile does get from this stage is the
        // back-off ladder, which is a floor rather than a ceiling.
        grid_cells: CellBracket::pinned(constants::MOBILE_VOLUME_GRID_CELLS),
        volume_texture_bytes: Bracket::pinned(constants::MOBILE_VOLUME_TEXTURE_BUDGET_BYTES),
        offscreen_bytes: Bracket::pinned(constants::MOBILE_VOLUME_OFFSCREEN_BUDGET_BYTES),
        mirror_bytes: Bracket::pinned(constants::MOBILE_VOLUME_MIRROR_BYTES_MAX),
        render_cache_entries: Bracket::pinned(constants::MOBILE_MAX_RENDER_CACHE_ENTRIES),
        quality_ceiling: QualityBracket::pinned(crate::volume::quality::MOBILE_PLATFORM_CEILING),
        max_panes: Bracket::pinned(rustdar_egui::pane::MAX_PANES_MOBILE),
        app_texture_ceiling_bytes: Bracket::pinned(constants::MOBILE_APP_TEXTURE_BUDGET_BYTES),
        raster_side_ceiling_px: constants::MOBILE_RASTER_SIDE_CEILING,
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
        loop_span_secs: Bracket::pinned(constants::DESKTOP_LOOP_SPAN_BUDGET_SECS),
        loop_render_budget: Bracket::pinned(constants::DESKTOP_MAX_LOOP_RENDER_BUDGET),
        loop_pool_bytes: Bracket::new(
            constants::DESKTOP_LOOP_POOL_FLOOR_BYTES,
            constants::DESKTOP_LOOP_POOL_CEILING_BYTES,
        ),
        // Pinned at the tier's own cells, and the reason is in another crate:
        // `rustdar_radar::voxel::VOXEL_TEXTURE_BUDGET_BYTES` is one byte per
        // cell of the largest index plane this workspace produces, the desktop
        // tier is *exactly* at it, and `offload::decode_job` refuses a request
        // past it at the wire boundary. A fourth shape is a decision in
        // `rustdar-radar` about what travels, not a promotion this resolver may
        // make on its own.
        grid_cells: CellBracket::pinned(constants::DESKTOP_VOLUME_GRID_CELLS),
        volume_texture_bytes: Bracket::pinned(constants::DESKTOP_VOLUME_TEXTURE_BUDGET_BYTES),
        // **The compromise a 24 GiB card was still eating.** 20 MiB pays for
        // the 2560 x 1440 reference pane at `Native` and nothing larger, so a
        // maximised pane on a 4K display — 3840 x 2160 x 4 B = 31.64 MiB — was
        // stepped down to `Half` and upscaled by the blit, on a machine with
        // 24576 MiB of VRAM. The ceiling is that pane plus the same headroom
        // the shipped figure keeps (14.06 -> 20 MiB is 1.42x; 31.64 x 1.42 =
        // 44.9, rounded up to a whole 48 MiB, which is 1.52x — room for the
        // alignment a real allocation carries, not enough to hide a doubling).
        //
        // The fill rate is measured rather than assumed: `volume::quality`'s
        // 3090 table is 0.766 ms for the cloud rung at 1440 x 900 over a dense
        // real volume, and the cost model behind it is fetch-bound and linear
        // in covered pixels, so 4K native is 6.4x that — about 4.9 ms of a
        // 16.7 ms frame for one pane, which is what a maximised pane is.
        //
        // The step stays at the floor: an integrated desktop GPU extrapolates
        // to 12-23 ms at *1440 x 900* on the same model, so it is the one class
        // the measurement argues against promoting.
        offscreen_bytes: Bracket::new(
            constants::DESKTOP_VOLUME_OFFSCREEN_BUDGET_BYTES,
            constants::DESKTOP_VOLUME_OFFSCREEN_CEILING_BYTES,
        ),
        mirror_bytes: Bracket::pinned(constants::DESKTOP_VOLUME_MIRROR_BYTES_MAX),
        render_cache_entries: Bracket::pinned(constants::NON_MOBILE_MAX_RENDER_CACHE_ENTRIES),
        quality_ceiling: QualityBracket::pinned(crate::volume::quality::DESKTOP_PLATFORM_CEILING),
        max_panes: Bracket::pinned(rustdar_egui::pane::MAX_PANES_DESKTOP),
        // The one bracket that moves *because another one did*. See
        // `Self::app_texture_ceiling_bytes` — both rungs are named constants
        // argued in bytes, neither is measured off the device, so the snugness
        // test still bites at each rung rather than degenerating into two sides
        // that move together.
        app_texture_ceiling_bytes: Bracket::new(
            constants::DESKTOP_APP_TEXTURE_BUDGET_BYTES,
            constants::DESKTOP_APP_TEXTURE_CEILING_BYTES,
        ),
        raster_side_ceiling_px: constants::DESKTOP_RASTER_SIDE_CEILING,
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
    /// How far up its bracket each field was spent. Carried so a log line and a
    /// failure message can say *why* a machine got what it got, and so a test
    /// can assert that a profile which should have been promoted was.
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
    /// The largest side a static plan-view raster may reach on this class.
    ///
    /// Carried as the *ceiling* of a pair whose floor is
    /// the long-range image side, rather than as one resolved figure,
    /// for the same reason [`Self::loop_pool_floor_bytes`] is: the number is
    /// settled at a seam this resolver runs before. The pool's seam is a
    /// failure it learns from; this one is simply **the adapter arriving** —
    /// `AppState::new` is where a real `max_texture_dimension_2d` first exists,
    /// and [`resolve`] runs before it, against
    /// [`AdapterCeilings::WEBGL2_GUARANTEE`].
    /// [`Self::raster_side_for_adapter`] is what closes it.
    pub raster_side_ceiling_px: usize,
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

    /// The bytes the shared render cache's entries may occupy between them,
    /// which is the bound that actually holds on it.
    ///
    /// **[`Self::render_cache_entries`] was a statement about memory and
    /// stopped being one.** Eight entries of `4096² × 8` is 1 GiB, and that is
    /// what the count meant for as long as a plan view was one of three sizes.
    /// Once the side became the device's own answer, spent as far as a sweep's
    /// gates justify, the same eight entries of a 7362 px surveillance cut are
    /// **3.3 GiB** — a regression the cache would have taken on silently,
    /// because nothing in it was counting bytes.
    ///
    /// Derived rather than bracketed, so that it is the *same* ceiling the count
    /// used to imply and this moves no memory: it is [`Self::render_cache_entries`]
    /// rasters at the size this class shipped before the device was ever asked.
    /// Both bounds apply and either may bind — on desktop 1 GiB is two
    /// long-range rasters, eight base-size ones (the count, exactly as before),
    /// or thirty-two of a browser's loop frames, which is the case a fixed
    /// count served worst.
    ///
    /// The long-range image side and **not** [`Self::raster_side_ceiling_px`]:
    /// the ceiling is what one raster may reach, and sizing the cache off it
    /// would raise the ceiling and the budget together, which is the tautology
    /// [`BudgetLimits::app_texture_ceiling_bytes`]' doc refuses for the app
    /// sum. What the cache is owed is the memory it was already promised.
    pub fn render_cache_budget_bytes(&self) -> usize {
        self.render_cache_entries * constants::raster_bytes(self.long_range_image_side_px)
    }

    /// Bytes one **static** pane render's texture occupies, worst case: the
    /// raster ceiling, since that is the most a device on this class can be
    /// asked to hold.
    ///
    /// [`Self::raster_side_ceiling_px`] and not the long-range image side,
    /// which is what this read before the ceiling could leave its floor. The
    /// two are still the same number on every class whose ceiling is pinned;
    /// where they differ, the larger is the honest worst case, and
    /// `the_static_render_textures_are_named_even_though_the_ceiling_omits_them`
    /// is where the difference is stated in megabytes.
    pub fn static_frame_bytes(&self) -> usize {
        self.raster_side_ceiling_px * self.raster_side_ceiling_px * 4
    }

    /// The largest static plan-view raster a device reporting
    /// `max_texture_dimension_2d` may be asked for.
    ///
    /// The one place in this module that spends an [`AdapterCeilings`] figure,
    /// and the shape any other field promoted off its floor should copy.
    ///
    /// # Half of what is reported
    ///
    /// Because a reported limit is not an allocatable one and the number alone
    /// does not say which kind it is. Measured: this project's RTX 3090 reports
    /// 32768 through Vulkan and refuses to allocate 32768 in either browser
    /// (`GL_OUT_OF_MEMORY` in Firefox 153, `GL_INVALID_VALUE` in Chrome) while
    /// allocating 16384; an AMD 890M reports 16384 and allocates it. Halving is
    /// what makes one rule safe on both, and it costs nothing here because both
    /// then land at or under 8192 — a full doubling below the 16384 each was
    /// measured to hand back.
    ///
    /// # It can only add
    ///
    /// the long-range image side is the floor, so a device reporting
    /// exactly that much keeps the raster it draws today rather than losing it
    /// to the halving. The `min` afterwards is the other end: a device is never
    /// offered more than it just said it has, which is what makes a machine
    /// reporting the GLES 3.0 floor fall to the base size instead of to a
    /// texture creation that fails and leaves the pane blank.
    ///
    /// | reports | half | result on desktop |
    /// |--------:|-----:|------------------:|
    /// |   32768 |16384 | 8192 (the ceiling)|
    /// |   16384 | 8192 | 8192 (the ceiling)|
    /// |    8192 | 4096 | 4096 (unchanged)  |
    /// |    4096 | 2048 | 4096 (the floor)  |
    /// |    2048 | 1024 | 2048 (the report) |
    ///
    /// The ceiling is a ceiling and not a size:
    /// `rustdar_radar::types::raster_side_px` spends it only as far as a
    /// sweep's own gates justify, which for the widest cut a WSR-88D flies is
    /// 7362 px.
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
    ///
    /// The **worst case** over every site: [`Self::frames_for_span`] is what a
    /// given loop actually gets, and it is this only where the radar is as fast
    /// as the fastest one measured.
    pub fn textured_frames(&self) -> usize {
        self.loop_render_budget.min(self.loop_frames_held)
    }

    /// Frames of `cadence_secs` apiece it takes to cover [`Self::loop_span_secs`].
    ///
    /// **This is where the loop budget stops being a frame count.** `n` frames
    /// span `n - 1` gaps, so the answer is `1 + span / cadence` with a
    /// truncating divide — the most frames whose span does not *exceed* the
    /// budget, which is what makes it a cap rather than a target. At the
    /// measured medians a two-hour desktop budget is 21 frames on a TDWR, 28 on
    /// a WSR-88D in precip and 14 on the same WSR-88D in clear air.
    ///
    /// # The two clamps are the two rules this must not break
    ///
    /// * `MIN_LOOP_FRAMES_PER_PANE` — a one-frame loop is not a loop, and a
    ///   site slow enough that one volume outlasts the whole budget still gets
    ///   a loop. Never degraded, whatever the cadence says.
    /// * [`Self::loop_render_budget`] — the memory the ceiling was re-derived
    ///   against. A listing whose median gap is implausibly short (a duplicated
    ///   key, a backfill landing all at once) cannot buy frames the pool cannot
    ///   pay for.
    ///
    /// # `None` is the *safe* arm, not the degraded one
    ///
    /// `LoopPlaybackState::scan_step_secs` is `None` until a listing has been
    /// accepted, and again after a pane really changes radar, which replaces
    /// the whole state. With no cadence there is no honest conversion, so the
    /// answer is the arm's full render budget: the loop behaves exactly as it
    /// did before this budget existed until the listing that tells us what a
    /// frame is worth arrives. Erring the other way — assuming the fastest
    /// radar and then shrinking — would make a loop visibly lose frames a
    /// second after opening.
    ///
    /// **Re-picking the site a pane is already on is not that**, and it keeps
    /// both the loop and its cadence. That is the right answer rather than a
    /// gap in the reset: the figure describes the radar, the radar has not
    /// changed, and re-measuring it would cost the whole listing to arrive at
    /// the same number. `SwitchRadarSite` gates the teardown on the pane having
    /// actually left a radar.
    ///
    /// A loop that keeps every scan re-measures as it polls — see
    /// `app_fetch::append_polled_frame`, which re-reads the median after each
    /// append precisely so a VCP change moves the cadence — so the figure this
    /// converts is the site's current one rather than the one it ran at when
    /// the listing was taken. A sampled loop is deliberately left on the
    /// listing's median, which is the honest figure for it: every gap in a
    /// sampled frame list is a sampled gap.
    ///
    /// A zero cadence takes the same arm rather than dividing: `median_step_secs`
    /// already drops non-positive gaps, so it is unreachable from the listing
    /// path, and a caller who reaches it is asking a question with no answer.
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
/// # A device that says nothing still takes every floor
///
/// The brackets are populated from the shipped constants, so a floor **is** the
/// constant on its arm, and a profile with no class, no useful report and no
/// memo reproduces today's configuration byte for byte on all three.
/// `the_resolver_reproduces_every_shipped_constant` puts the two side by side
/// field for field rather than asserting the claim, and `DeviceProfile::for_target`
/// — what the application resolves before it has met an adapter — is exactly
/// such a profile.
///
/// # What a device that says something gets
///
/// One [`Promotion`], spent on every bracket at once. Most brackets are still
/// pinned, so most fields do not move; the ones that do are named in
/// [`BudgetLimits`] beside the measurement that pays for them. Spending one
/// rung across the whole struct rather than a different signal per field is
/// what makes the ordering *between* subsystems expressible at all — the thing
/// nineteen independent `cfg` cascades could not say.
///
/// The loop pool is the one field that leaves as a *pair* — see
/// [`Budgets::loop_pool_floor_bytes`] — because it already has a runtime
/// resolution and a back-off path of its own. `LoopPool::for_promotion` is
/// where the same rung is spent on it.
///
/// # And then walks back down
///
/// [`demote`] applies what this machine already learned by failing, after the
/// promotion rather than before it: a memo is evidence about the machine, and
/// evidence outranks classification. A device that has backed off twice and
/// then reports desktop-class ceilings is still a device that failed twice.
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
            .max(limits.long_range_image_side_px.floor),
    };
    demote(&mut budgets, limits, profile.steps_back());
    budgets
}

/// Walk `steps` rungs down the degradation ladder, in the order
/// `docs/cross-platform-resource-limits.md` §4.3 fixes.
///
/// # The ordering principle, and where each rung came from
///
/// *Degrade what the user is least likely to notice, and degrade smoothly
/// before degrading discretely.* One rung per latched failure, so a machine
/// that is one step too ambitious does not lose everything over one event —
/// the same shape `LoopPool::back_off`'s halving already has, and for the same
/// reason.
///
/// 1. **Lighting.** `GradientShading::On -> Off`. The cheapest large saving in
///    the application (0.766 ms dense against 0.263 for the flat march on the
///    measured 3090) and the one a user is least likely to be able to name.
///    `volume::quality`'s own module doc already states "lighting degrades
///    before resolution"; this is that rule, one level up.
/// 2. **Offscreen resolution**, both halves at once: the quality's rung and the
///    budget that enforces it. ~3.4x per step at ~85 % efficiency. Blurrier,
///    still correct, still interactive.
/// 3. **Grid cells**, back to the bracket floor. Now the picture itself gets
///    coarser, so it is deliberately late.
/// 4. **The raster side ceiling**, back to the long-range image side. The most
///    visible rung of all, and the last one this function owns.
///
/// # Four rungs of a ladder of eight, and the four missing ones are not
/// oversights
///
/// * **Loop history** — the plan's rung 3, and it is `LoopPool::back_off`,
///   which already exists, already halves toward the pool floor, already
///   persists synchronously and is driven from the same event. Duplicating it
///   here would give one failure two effects on the same resource.
/// * **Overlay area** — the plan's rung 4. Not a field of [`Budgets`] yet; it
///   arrives with the overlay figures another agent is measuring.
/// * **Concurrency** — the plan's rung 7. Every arm is pinned, because
///   device-resolved concurrency is blocked on an unmeasured per-worker wasm
///   instance memory, so there is no rung to take.
/// * **Pane count** — the plan's rung 8, and it is *refused* rather than
///   deferred: `ui_layout` already documents why a saved layout is never
///   silently rewritten, and a budget is not a licence to take a pane the user
///   asked for.
///
/// # Only the first rungs are reachable, and that is by design
///
/// `volume::degrade` retires the 3D view after **two** surface losses. So a
/// machine walking this ladder gets one rung, then a second, then loses 3D
/// entirely — the floor the plan names. The later rungs exist so the ordering
/// is stated and testable, not because a session will spend them.
///
/// Every rung stops at its bracket floor, so this can never take a machine
/// below the configuration this build already shipped it.
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
            // Leaving it high would be harmless — it is a bound, not a spend —
            // but it would leave the snugness proof slack on exactly the
            // machine that just proved it could not afford the spend.
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
        // The *first rung that moves*, not the nth rung: a machine whose
        // lighting was already off should surrender its resolution on the next
        // failure rather than spending a step on a knob already at its stop.
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
