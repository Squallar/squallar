//! What is on screen, as the budget system prices it — and what the device can
//! hold, as it was learned.
//!
//! Resident memory has three parts. **Need** is what the scene costs: a function
//! of what is shown and at what resolution, never of the machine, so the same
//! scene costs the same bytes on a desktop and a tablet. **Capacity** is what
//! the device can hold — measured where an API exists, probed where a clean
//! probe exists, presumed otherwise — and it only ever *limits*. **Economy** is
//! what is resident beyond need, the first thing evicted under pressure. This
//! module holds the first two as plain data; [`crate::fit`] does the
//! arithmetic.

use crate::budget::{BudgetLimits, Promotion};
use crate::constants::{ECONOMY_FRACTION, NEED_FRACTION, UNIFIED_MEMORY_GPU_DIVISOR};
use crate::quality::GroundPass;
use squallar_radar::types::RenderView;

/// Everything on screen that costs resident memory.
///
/// Built by the application from the panes it already walks every frame for
/// the loop pool's sake, so a pane is described here in the terms that walk
/// has in hand. `Clone` and never `Copy`: it holds the panes as a list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Scene {
    /// One entry per visible pane.
    pub panes: Vec<PaneNeed>,
    /// One entry per map tile source drawing onto the glass.
    pub tile_sources: Vec<TileNeed>,
    /// The pane-mirror texture's size in texels, `[0, 0]` when no 3D pane is
    /// drawing a floor and the mirror has been released.
    pub mirror_px: [u32; 2],
    /// **One entry per gridded overlay layer enabled on any pane**, at the
    /// host bytes its handler keeps decoded source under. Scene-level and
    /// counted once however many panes show the layer: an overlay's handler
    /// is one instance for the whole application, so its grids are shared by
    /// construction. The figure is handed in the way [`crate::fit::GridBytes`]
    /// is, because the handlers live in a crate this one sits under.
    pub overlay_grids: Vec<OverlayGridNeed>,
}

impl Scene {
    /// Nothing on screen: what a fresh application has before its first frame.
    pub fn empty() -> Self {
        Self {
            panes: Vec::new(),
            tile_sources: Vec::new(),
            mirror_px: [0, 0],
            overlay_grids: Vec::new(),
        }
    }
}

/// One gridded overlay layer's decoded source, on the host.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OverlayGridNeed {
    /// The key-space grid budget the layer's handler states for itself —
    /// every product or channel a pane can select, resident at once, as the
    /// handler's own `GRID_CACHE_BYTES` (or the model layer's budget)
    /// declares on this build's arm. The budget rather than the bytes
    /// resident now: what a pane on the layer asks the heap to be able to
    /// hold, which is the admission question, and a figure that does not
    /// move with the poll.
    ///
    /// **The grid CACHE's budget, and nothing beside it.** The second
    /// population a gridded handler holds is [`Self::staging_bytes`]; the two
    /// are summed into [`crate::fit::NeedTerms::overlay_grids_host`] and are
    /// separate fields because neither is derivable from the other.
    pub budget_bytes: u64,
    /// **What the layer's handler holds beside its cache while a loop of it
    /// runs** — the handler's own `source_grid_staging_bytes`: its frame-granule
    /// cache at its `FRAME_STAGING_BYTES` budget, plus the retained decode
    /// buffer its staging pool parks between granules. Zero for a gridded layer
    /// that stages nothing (the model layer) and for every layer that is not
    /// gridded at all.
    ///
    /// **A second figure rather than a coefficient over [`Self::budget_bytes`],
    /// because no coefficient is the same statement on both layers**: MRMS's
    /// grid is half its cache budget and GMGSI's is a quarter of its, so
    /// `staging == budget` on one and `staging == budget / 2` on the other. A
    /// ratio written here would also be a budget spelled as arithmetic over
    /// another crate's private constants — the shape that re-derives silently
    /// the day one of them moves. The handler states its own figure, the way it
    /// already states its cache budget, and this field carries it.
    ///
    /// It was a named zero until 2026-09-06, and the gap it left was the
    /// largest unpriced host family on the census: measured on the Tier-2
    /// `huge` leg the gridded populations together read up to 444,458,992 B
    /// against 258,663,296 B priced — some 177 MiB of resident memory the
    /// admission door could not see. This field is 128,000,000 B of that on
    /// the arm those figures were taken on (MRMS 98,000,000 + GMGSI
    /// 30,000,000); what is left over is residency above the budgets, which is
    /// what a ceiling is for and not a term.
    pub staging_bytes: u64,
}

/// One pane, in the terms the cost functions price.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PaneNeed {
    /// What a 3D pane's offscreen is fitted from: the pane's own size in
    /// physical pixels as the painter was last told it, or the window's until
    /// the painter has fitted one for the pane — a stand-in that over-prices
    /// by at most the offscreen budget and never under-prices. `[0, 0]` for a
    /// 2D pane, none of whose terms is sized from it, and before a surface
    /// exists.
    pub px: [u32; 2],
    /// Which kind of picture the pane draws.
    pub view: RenderView,
    /// Whether the pane is running a loop, of radar or of another layer.
    pub looping: bool,
    /// The pane's own lookback, in seconds of wall clock — the width of loop
    /// the pane is asking for. Converted to frames at [`Self::cadence_secs`]
    /// and held to the budget's own span.
    pub loop_span_secs: usize,
    /// The looping layer's frame cadence, once its listing has said; `None`
    /// buys the whole render budget, as `Budgets::frames_for_span` already
    /// answers for a loop with no cadence yet.
    pub cadence_secs: Option<u32>,
    /// Bytes one frame of a loop of a layer that is **not** radar costs on this
    /// pane — measured off the texture the pane is drawing with, or the class's
    /// nominal overlay frame before one exists. `0` for a radar loop, whose
    /// three shapes are priced from the budgets. Carried rather than derived
    /// because an overlay frame is the pane's own raster, planned by a crate
    /// this one sits under.
    pub overlay_frame_bytes: usize,
    /// **Bytes one frame of this pane's RADAR plan-view loop costs, measured
    /// off the polar payloads the pane is holding** — or `0` where its frames
    /// are rasters, or where none has landed yet.
    ///
    /// The companion to [`Self::overlay_frame_bytes`] and carried for the same
    /// reason: a frame's size is the producing crate's answer and this one
    /// cannot see it. What differs is which of two representations the
    /// renderer produced, and the two are ~369x apart for one surveillance
    /// tilt — so this is read from `RadarSurface::resident_bytes` over the frames
    /// **actually on the pane**, and never from a build flag, a feature gate
    /// or the presence of a renderer. A price and a producer must not be able
    /// to disagree about which representation a frame is.
    ///
    /// **The zero means "ask the raster's price", not "free".** A pane whose
    /// loop has not landed a frame yet, and a pane whose sweeps were refused a
    /// code plane on fidelity grounds, both hold rasters as far as anything
    /// here can tell, and `crate::fit`'s plan-view arm prices them at
    /// [`crate::budget::Budgets::loop_frame_cost`] exactly as it always has.
    /// A polar frame is the cheaper of the two, so the fallback over-prices
    /// and never under-prices.
    ///
    /// **The worst of the pane's frames, not their mean.** A budget is a
    /// commitment against a peak, and one pane's loop can hold cuts of two
    /// widths while a product change works through it.
    pub radar_frame_bytes: usize,
    /// Voxel grids the pane keeps resident beside any loop: one live grid for a
    /// 3D pane, none for a 2D one. A second pane orbiting the same volume adds
    /// none — the grids live in one store keyed by target.
    pub volume_grids: usize,
    /// Whether a 3D pane's offscreen carries the ground pass's attachments —
    /// the pass the painter decided on its last fit, `Off` until it has.
    pub ground: GroundPass,
    /// Whether the pane draws 3D buildings: prism geometry fitted inside
    /// `Budgets::prism_vram_bytes` and priced at that ceiling. `false` until a
    /// `BuildingMeshJob` is dispatched for the pane, which no production
    /// caller does yet.
    pub buildings: bool,
    /// **Whole-picture overlay layers this pane shows**: every texture layer
    /// with a picture on the glass or one on its way, radar excluded (its
    /// raster is its own pipeline's and priced as the static render). Each is
    /// one raster of the pane at the budget's oversampling, crossing the host
    /// heap on its way to the GPU — see `crate::fit::picture_bytes`.
    pub overlay_pictures: usize,
    /// **The glass a whole-picture overlay of this pane covers**, in physical
    /// pixels: the pane's rect as the planner was last handed it, before the
    /// oversampling margin — the figure `crate::fit::picture_bytes` scales.
    /// The window's size until the pane has dispatched a picture, which
    /// over-prices a split pane by the pane count and a lone one by the top
    /// bar, never under-prices. Kept apart from [`Self::px`], which a 2D pane
    /// leaves at `[0, 0]` because none of its GPU terms is sized from it.
    pub picture_px: [u32; 2],
    /// **Whether the decoded volumes this pane's radar loop plays from are
    /// already counted under another pane's loop.** The loop scan cache
    /// (`squallar-radar`'s `loop_downloads`) holds one decoded volume per
    /// named frame per **site**, whatever product or view each pane draws
    /// from it, so two panes looping one site — the same picture set (an
    /// alias, priced as one loop) or two products (two loops, two texture
    /// sets, one scan cache) — hold one set of volumes between them. The
    /// application marks every pane on an already-counted site, leaving the
    /// count with the pane whose lookback is widest; `false` for that pane
    /// and for every pane looping nothing radar. Read by
    /// `crate::fit::NeedTerms::loop_scans_host` and by nothing on the GPU
    /// axis.
    pub loop_scans_shared: bool,
    /// **What this pane's radar loop already holds decoded, at its measured
    /// size**: of the frames the loop names, the ones the download cache
    /// holds, summed at the price each was given on arrival
    /// (`squallar-radar`'s `LoopDownloadManager::cached_scan_bytes_for`).
    /// The reconciliation half of the scan term — a resident frame costs
    /// what it was measured at, a frame still to come costs the reserve —
    /// handed in the way `crate::fit::GridBytes` is, because the cache lives
    /// in a crate this one sits beside. Zero before the first volume arrives
    /// and on every pane marked [`Self::loop_scans_shared`]; on a site no
    /// Level II loop reads ([`Self::loop_scans_needed`] `false`) it is the
    /// one volume a pane parked at a still is holding, or zero.
    pub loop_scans_resident_bytes: u64,
    /// How many of the loop's named frames [`Self::loop_scans_resident_bytes`]
    /// covers — the frames the reserve is **not** charged for.
    pub loop_scans_resident_frames: usize,
    /// **Whether this loop's frames are rendered from decoded Level II
    /// volumes at all.** A Level III loop derives its frames from the paired
    /// objects and reads nothing from the volume, so on a site where every
    /// live loop is Level III the download cache holds no volume for the loop
    /// — only whatever single volume a pane parked at a still there is
    /// keeping. The application answers this with
    /// `squallar_radar::loop_downloads::site_needs_decoded_source`, the same
    /// predicate the eviction sweep retains by, so the price and the
    /// residency cannot disagree.
    ///
    /// `false` charges no reserve at all: the two resident fields then carry
    /// the parked volume alone, which is what that site holds. `true` — a
    /// Level II loop on the site, or a loop that has not dispatched yet, the
    /// safe direction — prices every named frame.
    pub loop_scans_needed: bool,
    /// **What one of this pane's not-yet-arrived volumes is reserved at**, or
    /// `0` to use the class bootstrap.
    ///
    /// A decoded radar volume is the one term of the scene whose size is not
    /// knowable in advance: no `Content-Length` is read on the download path,
    /// and S3's `Size` sits in a listing document nothing parses. So it is
    /// reserved rather than measured, and the figure is
    /// `squallar_radar::loop_downloads::LoopDownloadManager::site_scan_reserve_bytes`
    /// — the bootstrap raised to the largest volume this session has actually
    /// seen from that site.
    ///
    /// **Zero means the bootstrap**, so a caller that says nothing gets
    /// exactly the arithmetic this crate did before the field existed, field
    /// for field. That is not laziness about `Option`: every construction
    /// site that predates this reserve means "the class figure" by its
    /// silence, and a sentinel they already write is a smaller change than an
    /// `Option` they must all learn to spell.
    pub loop_scan_reserve_bytes: u64,
}

/// One map tile source's working set.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TileNeed {
    /// Tiles covering the glass at the zoom being drawn.
    pub tiles_on_glass: usize,
    /// The coarser ancestors kept so the map never goes blank while a tile is
    /// on the wire.
    pub ancestor_net: usize,
    /// Bytes one resident styled entry costs, as measured by the tile cache.
    pub bytes_per_tile: usize,
}

/// What a scene costs, on the two memories it draws from.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Need {
    /// Textures: loop frames, grids, offscreens, static rasters, the mirror.
    pub gpu_bytes: u64,
    /// Host memory: the tile working set; every shown overlay picture at the
    /// budget's oversampling, **twice** — once for the batch the dispatch
    /// holds and once for the same batch in the renderer's upload queue; one
    /// more picture for the arrival in flight; one dispatch pass of overlay
    /// loop-frame rasters; every enabled gridded overlay's source budget; one
    /// decoded volume per radar loop frame (resident frames at their measured
    /// size, pending frames at the reserve) and one more for each 2D pane
    /// parked at a still; and one radar render's peak.
    ///
    /// **Three families measured on the `huge` leg are still priced at
    /// zero**, and `crate::fit`'s module header carries the reason for each:
    /// the gridded handlers' per-frame staging grids, `loans out` and `tile
    /// bodies`.
    pub host_bytes: u64,
}

/// How a capacity figure was obtained, in descending order of trust.
///
/// **Provenance, not a rank to test with `>=`.** Every consumer matches every
/// arm, so a new way of learning a figure makes the compiler name each place
/// that has to decide what it buys. Two of them did read this as a boolean
/// once — the loop pool's ceiling and the tile caches' — and a figure this
/// crate had *computed* took what a driver reading buys, which is how an
/// integrated part's budgets went unbounded on a machine with 86.2 GiB of RAM.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CapacitySource {
    /// Read from the driver: a Vulkan device-local heap sum, DXGI's budget,
    /// Metal's recommended working set.
    Measured,
    /// Found by allocating until the API refused — a browser's per-tab
    /// allowance, which no API states.
    Probed,
    /// **Computed here from a figure read about something else.** No API was
    /// asked about the GPU: this is the host's own RAM over
    /// [`crate::constants::UNIFIED_MEMORY_GPU_DIVISOR`], the share
    /// [`Capacity::unified`] gives a unified adapter where no reader answered
    /// for it. A guess with a machine's number in it is still a guess, and the
    /// arithmetic that produced it can be wrong in either direction on any
    /// given part — so it buys exactly what [`Self::Presumed`] buys at every
    /// consumer, and differs from it only in carrying this machine's figure
    /// rather than the bracket's constant.
    Derived,
    /// Nothing answered, or nothing that could be believed, so the bracket's
    /// constant stands in — every browser, every native adapter without a
    /// reader, and every software or virtual adapter whatever it read
    /// (`crate::budget::DeviceProfile::gpu_capacity_bytes`).
    Presumed,
}

/// **A ceiling a governor may put under the capacity in force, per pool** —
/// the last clamp term in the application's `capacity()` chain.
///
/// **Both fields have a producer**, and both are `squallar_app::recovery`.
/// Each replaced a session presumption that latched down and never came back
/// up, and each had to REPLACE it rather than sit beside it: a presumption
/// and a modulation are both a `min` against the capacity, so a latch left in
/// the chain would clamp everything this term lifts and the governor would be
/// a silent no-op.
///
/// **What still differs is the rule that lifts each, and that is a fact about
/// instruments.** [`Self::host_ceiling`] comes back when a margin has held
/// across successive capacity readings, because `squallar_alloc::live_bytes`
/// observes a page heap coming back. Nothing observes a card's, so
/// [`Self::gpu_ceiling`] comes back on a wall-clock dwell with doubling
/// backoff instead — an inference the application's readout labels as one
/// (`budget state:`'s `gpu ... dwell 4x 120 s`) rather than a measurement it
/// does not have.
///
/// Whatever produces it, a modulation can only LOWER — it is `min`'d against
/// the capacity, never substituted for it — so a producer's bug cannot
/// promise more than the hardware.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Modulation {
    /// The most GPU texture memory the term allows, in bytes; `None` leaves
    /// the figure in force alone.
    pub gpu_ceiling: Option<u64>,
    /// The most host memory the term allows, in bytes; `None` leaves the
    /// figure in force alone, and a capacity with no host figure keeps none.
    pub host_ceiling: Option<u64>,
}

impl Modulation {
    /// No modulation: the identity on every capacity.
    pub const NONE: Self = Self {
        gpu_ceiling: None,
        host_ceiling: None,
    };
}

/// **The share of each pool the user is willing to let this application
/// take**, as whole percents.
///
/// # A ceiling the user LOWERS, never a floor they raise
///
/// [`Self::FULL`] — 100 % on both pools — is the default, and that is not a
/// taste. Every install starts with nothing written down, every config file
/// produced before this field existed has none, and "Reset to defaults"
/// clears it: whatever the absent value means is what a very large number of
/// sessions will do. Shaped as a floor the user raises, a fresh install, a
/// downgrade and a reset would each silently hold *less* than the user last
/// asked for. Neutrality is the only default that cannot do that, and it
/// makes this what every other quality control here is — a ceiling, at the
/// top until somebody lowers it.
///
/// # Two percents, one per pool, and they are independent
///
/// A discrete card's VRAM and the host's RAM are two memories
/// ([`Pools::Split`]), and a user who wants the tile caches out of their RAM
/// has said nothing about their card. On a unified adapter the two bind one
/// memory and [`Capacity::scaled_to`] resolves that; the *setting* stays two
/// numbers so it means the same thing on every machine a config file can be
/// carried to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PoolPercents {
    /// The share of [`Capacity::gpu_bytes`] this application may take.
    pub gpu: u8,
    /// The share of [`Capacity::host_bytes`] this application may take.
    pub host: u8,
}

impl PoolPercents {
    /// The whole of both pools: the identity on every capacity, and what an
    /// install with nothing written down gets.
    pub const FULL: Self = Self {
        gpu: 100,
        host: 100,
    };

    /// **The smallest percent the setting offers, and why that number.**
    ///
    /// A decision, not an arbitrary clamp. Below roughly a tenth of any pool
    /// this application ships against, `crate::fit` has already walked every
    /// rung to its stop — the offscreen, the oversample, the loop span, the
    /// grid — so a 9 % and a 1 % resolve to the identical budgets and buy the
    /// identical picture. A control whose lower half cannot be observed to do
    /// anything is worse than a shorter control: it invites the user to
    /// believe they have kept lowering something. **Widening this downward
    /// needs a measurement showing the rungs still move there**, not a view
    /// that more range is friendlier.
    pub const FLOOR: u8 = 10;

    /// A pair read from a config file or a widget, held inside
    /// [`Self::FLOOR`]`..=100`. A hand-edited `0`, a `250`, and a value from a
    /// build whose range was wider all cost the user nothing.
    pub fn clamped(gpu: u8, host: u8) -> Self {
        Self {
            gpu: gpu.clamp(Self::FLOOR, 100),
            host: host.clamp(Self::FLOOR, 100),
        }
    }
}

impl Default for PoolPercents {
    fn default() -> Self {
        Self::FULL
    }
}

/// **What is actually holding one pool's figure down**, for the readout that
/// shows a requested percentage beside the one in force.
///
/// Three terms can lower a pool and a bare number cannot say which did. A
/// user who set 20 % months ago and forgot needs to see their own setting
/// named; a user whose machine is smaller than they think needs to see that
/// their 100 % is already the whole of it; and a user whose session is under
/// the page heap's governor needs to see that the figure is *modulated*,
/// rather than reading it as their setting or as their hardware.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PoolBinder {
    /// Nothing lowered it: the figure in force is what the machine has.
    Hardware,
    /// The user's own percentage is the term in force.
    UserPercent,
    /// A session latch ([`Capacity::held_to`]) or the page heap's
    /// [`Modulation`] sits under the user's percentage. Only the second of
    /// those can lift again, which is why the readout carries recovery
    /// separately rather than folding it in here.
    Governor,
}

/// **Which of the three terms bound a pool**, given the figure before any of
/// them, the figure after the user's percentage, and the figure in force.
///
/// The governor is asked first because it is the term that can lift: a
/// session under pressure is a different thing to say than a machine that is
/// small, and while both are true the recoverable one is the news. Equality
/// falls through — a percentage at 100 % has bound nothing, and a governor
/// sitting exactly on the user's line is not the reason the line is there.
pub fn pool_binder(
    hardware_bytes: u64,
    after_percent_bytes: u64,
    in_force_bytes: u64,
) -> PoolBinder {
    if in_force_bytes < after_percent_bytes {
        PoolBinder::Governor
    } else if after_percent_bytes < hardware_bytes {
        PoolBinder::UserPercent
    } else {
        PoolBinder::Hardware
    }
}

/// **What percent of `hardware_bytes` `in_force_bytes` actually is**, or
/// `None` where there is no pool to take a percentage of.
///
/// **Rounded to nearest, and deliberately not floored.** [`Capacity::scaled_to`]
/// floors, so the exact reciprocal of a share nothing else has touched is a
/// hair *under* it — `40 %` of 16 GiB read back as 39.999999994 %. Flooring
/// here as well would print every user's own untouched setting one point below
/// what they set it to, on the commonest path there is, which is a
/// self-inflicted wrongness far worse than the half-point it would buy.
///
/// **What says whether anything is binding is [`pool_binder`], which is
/// exact.** This figure is the magnitude beside it, not the verdict, so it
/// does not have to carry a conservative bias the verdict already carries
/// precisely.
pub fn effective_percent(hardware_bytes: u64, in_force_bytes: u64) -> Option<u8> {
    if hardware_bytes == 0 {
        return None;
    }
    let hardware = u128::from(hardware_bytes);
    let percent = (u128::from(in_force_bytes) * 100 + hardware / 2) / hardware;
    Some(u8::try_from(percent).unwrap_or(u8::MAX))
}

/// **The host pool a percentage is taken of**: what the OS says is available
/// plus what this process already holds.
///
/// # Why the sum, and not `available` alone
///
/// Every OS's "available" figure — Linux's `MemAvailable`, Darwin's free plus
/// inactive pages, Windows' `ullAvailPhys` — is memory available to a process
/// that holds none of it. **It already excludes this one.** So a rule spelled
/// `own = pct × available` reads its own consumption back as a smaller
/// machine, and the ceiling recedes as the app approaches it.
///
/// Write `S` for the figure available when the app held nothing, `R` for what
/// the OS has charged this process and `A = S − R` for what is available with
/// `R` held. The naive rule settles where `R = pct × (S − R)`, i.e.
/// `R = S · pct/(1 + pct)` — at 40 % that is **28.6 % of `S`**, not 40 %, and
/// at 100 % it is 50 %. Worse than the wrong number: the ceiling *moves* while
/// the app allocates, so a governor watching it can never reach it and never
/// tell that it has not.
///
/// The sum is the term that pushes back: `available + own live = S − (R − L)`,
/// where `L` is what the allocator handed out. For every byte the two figures
/// share, the line is fixed, `pct × pool` is something the app can walk up to
/// and sit on, and `pct` means what its label says.
///
/// # What the sum does not close, and by how much
///
/// **`R` is not `L`, and the difference is not added back.** What the OS
/// subtracted from its available figure is this process's *resident set*; what
/// this function adds back is the *heap request* total
/// (`squallar_alloc::live_bytes`). Between them sit the allocator's chunk
/// headers, its arena retention, huge-page rounding, and — the larger half —
/// every mapping that never went through `malloc`: the executable, the shared
/// libraries, thread stacks, mapped fonts, and the graphics driver's device
/// maps. All of it was subtracted and none of it is credited, so **the pool is
/// under-stated by `R − L`**.
///
/// Measured on this workspace's discrete-GPU Linux arm, 2026-09-06: the
/// non-heap resident set held **256.7–265.0 MiB, a 3.2 % range across every
/// condition tested** — empty app, empty steady scene, four quiet legs and one
/// contaminated at loadavg 21. About 102.4 MiB of it is `/dev/nvidia*`
/// mappings, constant to 0.1 % over a day and not this app's to release. A
/// ~152 MiB GPU staging ring (`squallar_gpu::staging_ring`) is driver-owned in
/// ordinary system RAM and **does not appear as a separable increment**: at the
/// empty instant the anonymous remainder is only 22.5 MiB, and at scene it
/// cannot be told from the app's own buffers. It is a possible further term,
/// not a confirmed one, and it is not to be added to the 265.
///
/// **Why the counter cannot see it, rather than only that it cannot**: on the
/// same arm the app's irreducible heap floor — empty app, every layer off,
/// device and surface up — is **4.23–4.37 MiB against a ~300 MiB resident
/// set**. Essentially none of what this process costs the machine at rest is
/// heap, so there is no allocator hook anywhere that could have counted it.
/// The gap is not a counter that under-reports; it is a term outside the
/// counter's domain.
///
/// That denominator is one arm on one OS and is not a constant for any other.
/// What makes the residual readable rather than re-derivable is
/// `squallar_alloc::process::resident`, which reads `R` in ~11 µs flat in RSS,
/// and the `budget state:` line, which prints `rss` and `pool residual` beside
/// `live` on every telemetry tick.
///
/// **The direction is the safe one, and it is left uncorrected deliberately.**
/// An under-stated pool is an under-stated allowance is rungs shed that did not
/// need to be — the over-firing direction, which refuses rather than
/// over-promises. Closing it is not a doc change: the only reader of `R` is
/// Linux-only, so a correction would fix one target and leave the rest, and it
/// would *raise* every Linux pool by about a quarter of a gigabyte — the
/// over-promising direction, on the one box whose hard freeze began this
/// campaign.
///
/// Monotonicity holds in the argument and not in the process. The sum can only
/// rise as `own_live_bytes` rises
/// (`the_pool_never_recedes_as_its_own_live_argument_grows`, named for the
/// argument because that is the whole of what it can observe), but a process
/// that grows its *non-heap* footprint — one more thread, one more driver
/// mapping — is charged for it in `available` and credited nothing, so the
/// pool recedes by exactly that growth. The 3.2 % range above is the measured
/// bound on how far.
///
/// `own_live_bytes` is `squallar_alloc::live_bytes()` where the counting
/// allocator is installed and `None` where it is not; `None` is read as zero
/// here, which drops the `L` term as well and under-states the pool by the
/// whole resident set. The same direction at a larger magnitude, and it is the
/// arm every process without the allocator takes.
///
/// **The pool is not RAM this app may take.** It is the figure a percentage
/// is taken OF, and what may be taken is [`Capacity::host_allowance`] of that
/// product. The percentage is [`PoolPercents::host`], applied by
/// [`Capacity::scaled_to`] before any allowance is computed; at its default
/// of 100 % the pool reaches `Capacity::host_bytes` whole and the only clamp
/// is the three-quarters allowance every host figure has always had, which is
/// what every session before the setting existed did.
pub fn host_pool_bytes(available_bytes: u64, own_live_bytes: Option<u64>) -> u64 {
    available_bytes.saturating_add(own_live_bytes.unwrap_or(0))
}

/// **Whether a capacity's two figures are two memories or one.**
///
/// Carried in the type rather than re-derived from the source, because the two
/// questions come apart: Metal's recommended working set is *read from a
/// driver* — [`CapacitySource::Measured`], the same arm a discrete card's VRAM
/// reading takes — and on Apple silicon it still names the same physical RAM
/// the host figure does. Nothing about how a figure was learned says what
/// memory it is a figure *of*.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Pools {
    /// Two memories. A discrete card's VRAM is not the host's RAM: a byte
    /// spent on a texture is not a byte taken from the tile cache, so each
    /// need is tested against its own allowance. Also every arm that knows
    /// nothing — a presumption is a bracket constant beside a page heap, and
    /// neither figure is a claim about silicon.
    Split,
    /// **One memory under two names.** Built only by [`Capacity::unified`],
    /// which cuts one pool into two shares; every byte either figure names is
    /// a byte the other cannot also have.
    Unified,
}

/// What the device can hold. It only ever limits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Capacity {
    /// GPU texture memory, in bytes.
    pub gpu_bytes: u64,
    /// Host memory, in bytes, where a reader answered.
    pub host_bytes: Option<u64>,
    /// How [`Self::gpu_bytes`] was learned, which decides how much of it need
    /// may take — see [`Self::allowance`].
    pub source: CapacitySource,
    /// Whether the two figures above are two memories or one, which decides
    /// whether the scene faces two admission tests or one
    /// ([`crate::fit::over`]).
    pub pools: Pools,
}

impl Capacity {
    /// The presumed arm: the bracket's whole-application texture constant **is**
    /// the capacity. Its floor, whatever rung the class earned — the three
    /// numbers 288 / 1024 / 3840 MiB are what those constants always were.
    /// The host figure is the bracket's declared ceiling where it has one —
    /// a browser's linear memory — and unknown otherwise
    /// ([`BudgetLimits::presumed_host_bytes`]).
    pub fn presumed(limits: &BudgetLimits) -> Self {
        Self {
            gpu_bytes: limits.app_texture_ceiling_bytes.at(Promotion::Floor) as u64,
            host_bytes: limits.presumed_host_bytes,
            source: CapacitySource::Presumed,
            pools: Pools::Split,
        }
    }

    /// A figure read from the driver. What
    /// `crate::budget::DeviceProfile::capacity` builds where the profile's
    /// readings amount to a measurement.
    pub fn measured(gpu_bytes: u64, host_bytes: Option<u64>) -> Self {
        Self {
            gpu_bytes,
            host_bytes,
            source: CapacitySource::Measured,
            pools: Pools::Split,
        }
    }

    /// A figure a probe found by allocating until refused. Constructible and
    /// priced here; the browser probe that will feed one in has not landed, so
    /// no profile produces it.
    pub fn probed(gpu_bytes: u64) -> Self {
        Self {
            gpu_bytes,
            host_bytes: None,
            source: CapacitySource::Probed,
            pools: Pools::Split,
        }
    }

    /// **A unified adapter: one physical memory under two names.**
    ///
    /// It takes ONE figure because there is one memory. On an integrated part
    /// the bytes a texture takes are bytes the tile cache cannot also take:
    /// the GPU figure and the host figure are the same silicon, and every
    /// other constructor here hands out two figures that were read
    /// independently and may therefore be spent twice. On a Framework 13 with
    /// 86.2 GiB of RAM that is what happened — 32.32 GiB of GPU allowance and
    /// 64.65 GiB of host allowance, both authorised over the one 86.2 GiB
    /// pool, and the machine froze hard enough to need a power cycle.
    ///
    /// The fix is structural rather than a smaller constant. `gpu_bytes` is a
    /// **share** of the pool — a reader's own figure where one answered
    /// (Metal's recommended working set), else the pool over
    /// [`UNIFIED_MEMORY_GPU_DIVISOR`], and never more than the pool itself —
    /// and `host_bytes` is what the pool has **left after it**, a residual and
    /// not a second reading. So
    ///
    /// ```text
    /// gpu_bytes + host_bytes == pool_bytes
    /// ```
    ///
    /// exactly, for every input, and since each allowance is a fraction under
    /// one of its own share, the two allowances sum to at most the pool
    /// whatever fraction anyone later picks
    /// (`a_unified_adapters_two_allowances_never_sum_past_its_one_pool`).
    /// There is no second reading here for a future reader to double count
    /// with, which is the property a divisor could not have carried.
    ///
    /// **`pool_bytes` is the receding figure — what the OS says is available
    /// plus what this process holds — and not the machine's total.** That is
    /// the second half of the incident and the half that made it a freeze
    /// rather than a shed. The host figure was already re-read from
    /// `MemAvailable` on every telemetry tick, but the GPU figure was half of
    /// `MemTotal`, which never moves, and nothing in this tree measures GPU
    /// residency — so on an integrated part the one arm that could have
    /// noticed the machine filling was structurally incapable of it, and `fit`
    /// could not demote a rung however tight RAM got. Cut from the available
    /// reading, **both halves shrink together by construction**: the GPU share
    /// tracks availability with no new reader and no new term, and the ladder
    /// sheds on the way down
    /// (`a_falling_pool_takes_the_gpu_share_down_with_it`). A machine with no
    /// available reader falls back to its total and says so in the source.
    ///
    /// The source follows the GPU share, which is the figure
    /// [`Self::source`] documents: a driver answered, or this arithmetic did.
    pub fn unified(pool_bytes: u64, measured_gpu_bytes: Option<u64>) -> Self {
        let gpu_bytes = measured_gpu_bytes
            .unwrap_or(pool_bytes / UNIFIED_MEMORY_GPU_DIVISOR)
            .min(pool_bytes);
        Self {
            gpu_bytes,
            host_bytes: Some(pool_bytes - gpu_bytes),
            source: match measured_gpu_bytes {
                Some(_) => CapacitySource::Measured,
                None => CapacitySource::Derived,
            },
            pools: Pools::Unified,
        }
    }

    /// **The one allowance a unified capacity's whole need is tested
    /// against**: its two shares' allowances, summed. Since the shares
    /// partition the pool, this is the pool's own fraction — one number for
    /// one memory, and the figure [`crate::fit::over`] asks a unified scene
    /// about instead of asking two questions of two pools that do not exist.
    ///
    /// Meaningless on a [`Pools::Split`] capacity and not to be read there: a
    /// discrete card's VRAM and the host's RAM cannot be summed, which is why
    /// this is only ever reached under a `Pools::Unified` arm.
    pub fn joint_allowance(&self) -> u64 {
        self.allowance()
            .saturating_add(self.host_allowance().unwrap_or(0))
    }

    /// **This capacity with the user's percentages taken of it** — the pool
    /// figures themselves, before any allowance is computed.
    ///
    /// # Why the pool and not the allowance
    ///
    /// [`Self::allowance`] does not apply `NEED_FRACTION` on the presumed arm
    /// (see there), so a percentage applied *after* it would leave every
    /// browser and every unread native adapter untouched and the control would
    /// not do what its label says on the arms most users are on. Multiplying
    /// the pool is also what puts the percentage in front of
    /// [`Self::economy_allowance`] and everything `crate::fit` derives from a
    /// capacity — the tile caches included, which are what a RAM slider has to
    /// move to be worth having.
    ///
    /// # Where this sits in the chain, and why it is first
    ///
    /// The application's chain is this, then [`Self::held_to`], then
    /// [`Self::modulated_by`] — the percentage multiplies the **raw** pool and
    /// the session latch and the governor's ceiling bound the product. The
    /// order is load-bearing and not a style: `min(hw, latch) × p` is not
    /// `min(hw × p, latch)`. With `hw = 100`, `latch = 50`, `p = ½` the first
    /// answers 25 and the second 50. The latch and the modulation are absolute
    /// byte ceilings learned under pressure; the user asked for `p` % of the
    /// **pool**, not for `p` % of a latch, so the product is what those two
    /// then bound. It also leaves all three terms as plain `min`s against one
    /// another, which is what lets [`pool_binder`] name the binding one
    /// honestly instead of decoratively.
    ///
    /// # The unified arm
    ///
    /// On [`Pools::Split`] each figure takes its own percent: two memories,
    /// two independent settings. On [`Pools::Unified`] the two figures are two
    /// shares of ONE memory, so both percentages bind the same pool and the
    /// effective figure is **the lower of the two products** — which is both
    /// shares scaled by `min(gpu, host)`.
    ///
    /// That scaling is also the only one that keeps [`Self::unified`]'s
    /// partition invariant. Both shares are floored independently, and
    /// `floor(a) + floor(b) <= floor(a + b)`, so
    /// `gpu_bytes + host_bytes <= pool × m` for every input — the two
    /// allowances still cannot sum past the one pool. **A future editor
    /// "simplifying" this into a re-cut of the scaled pool, or into per-pool
    /// percents on a unified adapter, breaks that inequality**; it is written
    /// here because it is not visible from the code.
    ///
    /// [`PoolPercents::FULL`] is the identity: `bytes × 100 / 100` is `bytes`
    /// on every arm, which is the regression that protects every user who
    /// never opens the setting.
    pub fn scaled_to(self, percents: PoolPercents) -> Self {
        let share = |bytes: u64, percent: u8| -> u64 {
            u64::try_from(u128::from(bytes) * u128::from(percent) / 100).unwrap_or(u64::MAX)
        };
        let (gpu_percent, host_percent) = match self.pools {
            Pools::Split => (percents.gpu, percents.host),
            Pools::Unified => {
                let lower = percents.gpu.min(percents.host);
                (lower, lower)
            }
        };
        Self {
            gpu_bytes: share(self.gpu_bytes, gpu_percent),
            host_bytes: self.host_bytes.map(|host| share(host, host_percent)),
            ..self
        }
    }

    /// This capacity, held to the GPU ceiling a governor has learned. **This
    /// function only ever lowers, and the figure handed to it is what may
    /// rise** — it is [`Modulation::gpu_ceiling`] that reaches here, through
    /// [`Self::modulated_by`], and that ceiling steps back up when
    /// `squallar_app::recovery`'s dwell has run without a new event. Nothing
    /// latches a GPU figure for the session any more, and the lowering is
    /// discarded at exit.
    pub fn held_to(self, session_gpu_bytes: Option<u64>) -> Self {
        Self {
            gpu_bytes: session_gpu_bytes.map_or(self.gpu_bytes, |cap| cap.min(self.gpu_bytes)),
            ..self
        }
    }

    /// The host side of [`Self::held_to`]: a page heap that reached its
    /// watermark lowers what the session takes the host to hold. **This
    /// function only ever lowers, and the figure handed to it is what may
    /// rise** — it is [`Modulation::host_ceiling`] that reaches here, through
    /// [`Self::modulated_by`], and that ceiling steps back up when
    /// `squallar_app::recovery`'s margin has held. Nothing latches a host
    /// figure for the session any more. A capacity with no host figure stays
    /// without one — there is nothing to hold down.
    pub fn host_held_to(self, session_host_bytes: Option<u64>) -> Self {
        Self {
            host_bytes: self
                .host_bytes
                .map(|own| session_host_bytes.map_or(own, |cap| cap.min(own))),
            ..self
        }
    }

    /// This capacity under a [`Modulation`]: each pool held to the ceiling the
    /// term names, the way [`Self::held_to`] and [`Self::host_held_to`] hold
    /// it to the session's — `min`, never a substitution, so the term can
    /// only lower and [`Modulation::NONE`] is the identity. The source is
    /// untouched: a modulated figure was learned the way it was learned.
    pub fn modulated_by(self, modulation: Modulation) -> Self {
        self.held_to(modulation.gpu_ceiling)
            .host_held_to(modulation.host_ceiling)
    }

    /// The most host memory the scene's need may occupy here, or `None`
    /// where the host is unbounded because nothing reads it.
    ///
    /// `NEED_FRACTION` of the figure **on every arm**, the presumed one
    /// included, and that is the difference from [`Self::allowance`]: the
    /// GPU presumption is a bracket constant argued with its own headroom,
    /// where a browser's linear memory is a wall the module header declares
    /// with none — every byte the allocator, the transport's copies and the
    /// picture in flight take is under it — and a native RAM reading is raw
    /// hardware the way a VRAM reading is.
    pub fn host_allowance(&self) -> Option<u64> {
        self.host_bytes.map(|host| {
            host / NEED_FRACTION.1 * NEED_FRACTION.0
                + (host % NEED_FRACTION.1) * NEED_FRACTION.0 / NEED_FRACTION.1
        })
    }

    /// The most GPU memory the scene's need may occupy here.
    ///
    /// A measured or probed figure is raw hardware and needs headroom for the
    /// driver, the compositor and the picture in flight: `NEED_FRACTION` of it.
    /// **A derived figure takes the same fraction**: it is a share of the
    /// machine's real RAM, so it is raw hardware in exactly the way a heap
    /// reading is — a wall, not a budget argued with headroom already inside
    /// it — and spending it whole is what put 32.32 GiB of textures and 64.65
    /// GiB of host allowance over one 86.2 GiB pool.
    ///
    /// A presumed figure is a bracket constant argued with its own headroom,
    /// and today's sum proof already spends up to it, so the constant is the
    /// allowance and the fraction is not applied twice.
    ///
    /// **The unconditional rule — the fraction on every arm, the presumed one
    /// included, because a presumption is a wall too — is a written decision
    /// and is deferred, not rejected.** It is not in this commit because every
    /// browser is on the presumed arm, and the web build has a live complaint
    /// that the smallest pan re-renders its tiles and redraws its alerts: the
    /// margin that prevents that is the overlay oversampling rung, whose
    /// bottom rung is no margin at all
    /// (`crate::budget::overdraw_for_oversample`), and it is the first thing
    /// the ladder sheds. Taking a quarter of the presumed allowance would push
    /// that arm further down the very rung the complaint is about. It lands
    /// when there is a measurement of what it costs the oversample rung on the
    /// web, which is a separate lane's.
    pub fn allowance(&self) -> u64 {
        match self.source {
            CapacitySource::Measured | CapacitySource::Probed | CapacitySource::Derived => {
                self.gpu_bytes / NEED_FRACTION.1 * NEED_FRACTION.0
                    + (self.gpu_bytes % NEED_FRACTION.1) * NEED_FRACTION.0 / NEED_FRACTION.1
            }
            CapacitySource::Presumed => self.gpu_bytes,
        }
    }

    /// **The smallest capacity figure, learned the way this one was, whose
    /// [`Self::allowance`] covers `allowance`** — the inverse of that
    /// arithmetic, so a caller holding a need can say what capacity would
    /// fit it. On the presumed arm the constant is its own allowance and the
    /// figure is `allowance` itself; on a measured or probed arm it is the
    /// ceiling of `allowance / NEED_FRACTION`, and one byte less allows one
    /// byte too few (`a_capacity_for_an_allowance_is_the_smallest_that_covers_it`).
    /// Saturates rather than wraps at the top of `u64`.
    ///
    /// The arms are [`Self::allowance`]'s, inverted — they have to be the same
    /// two sets or the round trip stops closing, so the derived arm moved here
    /// with it.
    pub fn gpu_bytes_for_allowance(&self, allowance: u64) -> u64 {
        match self.source {
            // In `u128`: the product overflows a `u64` well before the
            // quotient does, so saturating the multiply would answer a third
            // of the truth for every figure above `u64::MAX / 4`.
            CapacitySource::Measured | CapacitySource::Probed | CapacitySource::Derived => {
                u64::try_from(
                    (u128::from(allowance) * u128::from(NEED_FRACTION.1))
                        .div_ceil(u128::from(NEED_FRACTION.0)),
                )
                .unwrap_or(u64::MAX)
            }
            CapacitySource::Presumed => allowance,
        }
    }

    /// What may be resident **beyond** `need` here: `ECONOMY_FRACTION` of the
    /// capacity less the need itself, and nothing when the need already
    /// reaches that line. The one figure that legitimately grows with the
    /// machine — a bigger card keeps more tiles panned away from, more parsed
    /// geometry, a larger render cache — and the first thing pressure evicts.
    /// Computed and printed; its first consumer is the tile cache's budget,
    /// which has not joined this arithmetic yet.
    pub fn economy_allowance(&self, need: Need) -> u64 {
        let ceiling = self.gpu_bytes / ECONOMY_FRACTION.1 * ECONOMY_FRACTION.0
            + (self.gpu_bytes % ECONOMY_FRACTION.1) * ECONOMY_FRACTION.0 / ECONOMY_FRACTION.1;
        ceiling.saturating_sub(need.gpu_bytes)
    }
}

#[path = "scene/percent_tests.rs"]
#[cfg(test)]
mod percent_tests;

/// Scenes and stand-ins the crate's tests share.
#[cfg(test)]
pub(crate) mod fixtures {
    use super::*;
    use crate::budget::{AdapterCeilings, DeviceProfile, Platform};
    use crate::quality::DeviceClass;

    /// A profile for one shipped bracket, with every runtime field at its most
    /// conservative reading.
    pub(crate) fn shipped_profile(limits: BudgetLimits) -> DeviceProfile {
        DeviceProfile {
            platform: if limits.name == "wasm32" {
                Platform::Web
            } else {
                Platform::Native
            },
            limits,
            class: DeviceClass::Unknown,
            adapter: AdapterCeilings::WEBGL2_GUARANTEE,
            vram_bytes: None,
            system_ram_bytes: None,
            declared_ram_bytes: None,
            parallelism: None,
            form_factor: None,
            // Nothing said, so the bracket's own presumption stands — which
            // for the wasm bracket is the bound the module was linked with.
            // A browser page that chose a smaller wall would carry it here.
            linear_memory_max_bytes: None,
            // No available-RAM reader has answered, so the profile carries no
            // pool and the host figure falls back the way it did before one
            // existed — which is what makes every expectation in this crate a
            // control on the reader rather than a re-typed value.
            host_pool_bytes: None,
            memo: None,
        }
    }

    /// A stand-in for the raymarch's `resident_grid_bytes`: the raw cells at
    /// the volume format's four bytes apiece, with no mip levels, no colour
    /// table and no jitter tile. It under-prices a grid on purpose — the
    /// property these tests hold is how `fit` sheds, not what a grid costs, and
    /// the real arithmetic meets `need` in squallar-volumetric's agreement test.
    pub(crate) fn stand_in_grid_bytes(cells: [u32; 3]) -> Option<usize> {
        cells
            .iter()
            .try_fold(4usize, |acc, &n| acc.checked_mul(n as usize))
    }

    /// A plan-view pane of `px`, looping at `cadence_secs` over `span_secs`
    /// when `looping`.
    pub(crate) fn plan_pane(
        px: [u32; 2],
        looping: bool,
        span_secs: usize,
        cadence_secs: Option<u32>,
    ) -> PaneNeed {
        PaneNeed {
            px,
            view: RenderView::PlanView,
            looping,
            loop_span_secs: span_secs,
            cadence_secs,
            overlay_frame_bytes: 0,
            radar_frame_bytes: 0,
            volume_grids: 0,
            ground: GroundPass::Off,
            buildings: false,
            overlay_pictures: 0,
            picture_px: [0, 0],
            loop_scans_shared: false,
            loop_scans_resident_bytes: 0,
            loop_scans_resident_frames: 0,
            loop_scans_needed: true,
            loop_scan_reserve_bytes: 0,
        }
    }

    /// The user's own window with `pictures` whole-picture overlay layers
    /// shown on it, at the tile working set it needs between zooms: the
    /// Tier-2 `huge` leg's scene (KTLX, seventeen layers of which thirteen
    /// are texture pictures, the radar loop playing, a 2878 x 1651 window),
    /// which the page's 1 GiB linear memory could not hold at 1.5x.
    ///
    /// **The pane is 2878 x 1611, not the window.** The forty rows between
    /// them are the top bar, which on a web canvas at device pixel ratio 1
    /// is forty physical pixels. Neither figure is modelled here: the leg's
    /// own `overlay pictures:` line reported 4317 x 2416, and 2416 is
    /// `1611 * 150 / 100`. The two legs' allocation failures name the same
    /// picture from the other side — twelve of the twenty `alloc failed:`
    /// lines Firefox printed asked for exactly 41,719,488 B, which is
    /// `4317 * 2416 * 4`. Measured on both sides, derived on neither.
    ///
    /// The tile entry cost is the measured city-core tail
    /// (`squallar_egui::tile_source::MEASURED_STYLED_ENTRY_BYTES`), restated
    /// here because this crate sits under that one.
    ///
    /// **The loop had been playing the whole leg, so its named frames are
    /// decoded**: [`HUGE_LEG_SCANS_HELD`] of them at [`HUGE_LEG_SCAN_BYTES`]
    /// apiece — a **modelled** steady state, since the leg's own volumes were
    /// never priced one by one: the size is the median of the same
    /// 208-volume measurement the reserve was rounded up from. The same scene
    /// before its first volume arrives — every frame pending, at the reserve —
    /// is [`huge_pending`], and the two together are the same leg at its two
    /// prices.
    pub(crate) fn huge(pictures: usize) -> Scene {
        const MEASURED_STYLED_ENTRY_BYTES: usize = 1_462_708;
        Scene {
            panes: vec![PaneNeed {
                overlay_pictures: pictures,
                picture_px: [2878, 1611],
                loop_scans_resident_frames: HUGE_LEG_SCANS_HELD,
                loop_scans_resident_bytes: HUGE_LEG_SCANS_HELD as u64 * HUGE_LEG_SCAN_BYTES,
                ..plan_pane([0, 0], true, 2 * 60 * 60, Some(259))
            }],
            tile_sources: vec![TileNeed {
                tiles_on_glass: 187,
                ancestor_net: 6,
                bytes_per_tile: MEASURED_STYLED_ENTRY_BYTES,
            }],
            mirror_px: [0, 0],
            overlay_grids: Vec::new(),
        }
    }

    /// One decoded volume of the `huge` leg's loop as [`huge`] models it:
    /// **48.88 MiB, the measured median** of 208 real archive volumes decoded
    /// under a counting global allocator. A stand-in for the leg's own
    /// volumes, which were never priced one by one.
    ///
    /// It used to read 46.5 MiB, the middle of a 46.1–46.8 MiB band, and that
    /// band was wrong: the instrument that produced it took its baseline
    /// **after** the compressed archive buffer had been read, and that buffer
    /// is freed inside the measurement window, so every volume was discounted
    /// by its own compressed size (0.34–17.96 MiB, median 5.56). A modelled
    /// resident volume that reads low makes a scene look like it fits when it
    /// does not, which is the error direction that costs the process rather
    /// than a rung.
    pub(crate) const HUGE_LEG_SCAN_BYTES: u64 = 51_254_395;

    /// The frames the `huge` leg's loop holds decoded once it has settled:
    /// **eleven**, every frame the leg's own arm names — 1 + 2700 / 259, the
    /// two-hour lookback held to the web bracket's 45-minute span at the
    /// precipitation cadence. The leg was a web leg and had been playing for
    /// the whole capture, so its named frames had all arrived.
    ///
    /// It is a count of the leg's frames, not of the cache's entries, so on a
    /// bracket that names more (desktop names 28) the same scene prices
    /// eleven resident frames beside seventeen still to come — which is the
    /// mixed case, and the one the reserve exists for.
    pub(crate) const HUGE_LEG_SCANS_HELD: usize = 11;

    /// **[`huge`] with a Level III loop**: the same leg, the same thirteen
    /// pictures, playing a product derived from paired Level III objects. Its
    /// frames read no decoded volume, so on that site the download cache
    /// holds none and the scan term is nothing at all — the shape
    /// `site_needs_decoded_source` answers `false` for.
    pub(crate) fn huge_level3(pictures: usize) -> Scene {
        let mut scene = huge(pictures);
        scene.panes[0].loop_scans_needed = false;
        scene.panes[0].loop_scans_resident_frames = 0;
        scene.panes[0].loop_scans_resident_bytes = 0;
        scene
    }

    /// [`huge`] before its first volume has arrived: every named frame still
    /// pending, so the loop's scans are priced at the reserve alone — the
    /// admission price of the same scene.
    pub(crate) fn huge_pending(pictures: usize) -> Scene {
        let mut scene = huge(pictures);
        scene.panes[0].loop_scans_resident_frames = 0;
        scene.panes[0].loop_scans_resident_bytes = 0;
        scene
    }

    /// A 3D pane of `px` holding one live grid, drawing ground or not.
    pub(crate) fn volume_pane(px: [u32; 2], ground: GroundPass) -> PaneNeed {
        PaneNeed {
            px,
            view: RenderView::Volume,
            looping: false,
            loop_span_secs: 0,
            cadence_secs: None,
            overlay_frame_bytes: 0,
            radar_frame_bytes: 0,
            volume_grids: 1,
            ground,
            buildings: false,
            overlay_pictures: 0,
            picture_px: [0, 0],
            loop_scans_shared: false,
            loop_scans_resident_bytes: 0,
            loop_scans_resident_frames: 0,
            loop_scans_needed: true,
            loop_scan_reserve_bytes: 0,
        }
    }

    /// **Two panes on one loop, as `App::loop_demand` describes them**: the
    /// first owns the set — a two-hour plan-view loop at the precipitation
    /// cadence — and the second is its alias: the same site, product, tilt
    /// and window, so it is written down as not looping, with no grid of its
    /// own and its scans counted under the first. Ruling 8 as the scene
    /// encodes it: the second pane owes no loop cost at all.
    pub(crate) fn two_panes_one_loop() -> Scene {
        let owner = plan_pane([1920, 1080], true, 2 * 60 * 60, Some(259));
        let alias = PaneNeed {
            looping: false,
            loop_scans_shared: true,
            ..owner
        };
        Scene {
            panes: vec![owner, alias],
            ..Scene::empty()
        }
    }

    /// **Two panes looping one site at two products**: two loops, so two
    /// texture sets, but one scan cache — the second pane's frames are its
    /// own and its decoded volumes are the first's.
    pub(crate) fn two_panes_one_site() -> Scene {
        let owner = plan_pane([1920, 1080], true, 2 * 60 * 60, Some(259));
        let other_product = PaneNeed {
            loop_scans_shared: true,
            ..owner
        };
        Scene {
            panes: vec![owner, other_product],
            ..Scene::empty()
        }
    }

    /// The scenes every profile is fitted against: nothing; one loop; a full
    /// screen of two-hour loops; a 3D pane drawing ground; the same pane
    /// drawing buildings too; the user's own 2878 x 1651 window with the
    /// 193 tiles it needs between zooms, at the 1.03 MB a styled entry was
    /// measured to cost; the `huge` leg; and the two shared-loop shapes,
    /// an alias and a second product on one site.
    pub(crate) fn scene_table() -> Vec<(&'static str, Scene)> {
        const HD: [u32; 2] = [1920, 1080];
        const TWO_HOURS: usize = 2 * 60 * 60;
        const PRECIP: Option<u32> = Some(259);
        vec![
            ("empty", Scene::empty()),
            (
                "one looping pane",
                Scene {
                    panes: vec![plan_pane(HD, true, TWO_HOURS, PRECIP)],
                    tile_sources: Vec::new(),
                    mirror_px: [0, 0],
                    overlay_grids: Vec::new(),
                },
            ),
            (
                "six looping panes at two hours",
                Scene {
                    panes: vec![plan_pane(HD, true, TWO_HOURS, PRECIP); 6],
                    tile_sources: Vec::new(),
                    mirror_px: [0, 0],
                    overlay_grids: Vec::new(),
                },
            ),
            (
                "one volume pane with ground",
                Scene {
                    panes: vec![volume_pane([2560, 1440], GroundPass::On)],
                    tile_sources: Vec::new(),
                    mirror_px: [2560, 1440],
                    overlay_grids: Vec::new(),
                },
            ),
            (
                "one volume pane with ground and buildings",
                Scene {
                    panes: vec![PaneNeed {
                        buildings: true,
                        ..volume_pane([2560, 1440], GroundPass::On)
                    }],
                    tile_sources: Vec::new(),
                    mirror_px: [2560, 1440],
                    overlay_grids: Vec::new(),
                },
            ),
            (
                "the user's 2878 x 1651 canvas with 193 tiles",
                Scene {
                    panes: vec![plan_pane([2878, 1651], false, TWO_HOURS, None)],
                    tile_sources: vec![TileNeed {
                        tiles_on_glass: 193,
                        ancestor_net: 0,
                        bytes_per_tile: 1_030_000,
                    }],
                    mirror_px: [0, 0],
                    overlay_grids: Vec::new(),
                },
            ),
            (
                "the huge leg: thirteen pictures on the user's canvas",
                huge(13),
            ),
            (
                "the huge leg before its first volume arrived",
                huge_pending(13),
            ),
            ("the huge leg playing a Level III product", huge_level3(13)),
            ("two panes on one loop", two_panes_one_loop()),
            (
                "two panes looping one site at two products",
                two_panes_one_site(),
            ),
        ]
    }
}
