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
use crate::constants::{ECONOMY_FRACTION, NEED_FRACTION};
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
    pub budget_bytes: u64,
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
    /// Host memory: the tile working set, every shown overlay picture at the
    /// budget's oversampling, one more picture for the arrival in flight,
    /// every enabled gridded overlay's source budget, and one decoded volume
    /// per radar loop frame — resident frames at their measured size, pending
    /// frames at the reserve.
    pub host_bytes: u64,
}

/// How a capacity figure was obtained, in descending order of trust.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CapacitySource {
    /// Read from the driver: a Vulkan device-local heap sum, DXGI's budget,
    /// Metal's recommended working set.
    Measured,
    /// Found by allocating until the API refused — a browser's per-tab
    /// allowance, which no API states.
    Probed,
    /// Nothing answered, or nothing that could be believed, so the bracket's
    /// constant stands in — every browser, every native adapter without a
    /// reader, and every software or virtual adapter whatever it read
    /// (`crate::budget::DeviceProfile::gpu_capacity_bytes`).
    Presumed,
}

/// **A ceiling a governor may put under the capacity in force, per pool** —
/// the third clamp term in the application's `capacity()` chain, beside the
/// two session presumptions (`held_to`, `host_held_to`).
///
/// The seam and not yet its producer: `NONE` (the `Default`) is the identity,
/// and nothing in the tree writes anything else yet. What will: a dwell-based
/// step on the GPU axis after an out-of-memory event and a host step on a
/// platform memory warning, both of which come back UP when their dwell
/// expires — which is exactly why this is a separate term and not another
/// `session_capacity`: a presumption is latched down for the session, a
/// modulation is re-derived and can lift. Whatever produces it, a modulation
/// can only LOWER — it is `min`'d against the capacity, never substituted for
/// it — so a producer's bug cannot promise more than the hardware.
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
            host_bytes: limits.presumed_host_bytes.map(|bytes| bytes as u64),
            source: CapacitySource::Presumed,
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
        }
    }

    /// This capacity, held to what the session has learned: pressure lowers a
    /// session's presumption and never raises it, and the lowering is
    /// discarded at exit.
    pub fn held_to(self, session_gpu_bytes: Option<u64>) -> Self {
        Self {
            gpu_bytes: session_gpu_bytes.map_or(self.gpu_bytes, |cap| cap.min(self.gpu_bytes)),
            ..self
        }
    }

    /// The host side of [`Self::held_to`]: a page heap that reached its
    /// watermark lowers what this session presumes the host holds, never
    /// raises it, and the lowering dies with the process. A capacity with no
    /// host figure stays without one — there is nothing to hold down.
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
    /// A presumed figure is a bracket constant argued with its own headroom, and
    /// today's sum proof already spends up to it, so the constant is the
    /// allowance and the fraction is not applied twice.
    pub fn allowance(&self) -> u64 {
        match self.source {
            CapacitySource::Measured | CapacitySource::Probed => {
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
    pub fn gpu_bytes_for_allowance(&self, allowance: u64) -> u64 {
        match self.source {
            // In `u128`: the product overflows a `u64` well before the
            // quotient does, so saturating the multiply would answer a third
            // of the truth for every figure above `u64::MAX / 4`.
            CapacitySource::Measured | CapacitySource::Probed => u64::try_from(
                (u128::from(allowance) * u128::from(NEED_FRACTION.1))
                    .div_ceil(u128::from(NEED_FRACTION.0)),
            )
            .unwrap_or(u64::MAX),
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
            volume_grids: 0,
            ground: GroundPass::Off,
            buildings: false,
            overlay_pictures: 0,
            picture_px: [0, 0],
            loop_scans_shared: false,
            loop_scans_resident_bytes: 0,
            loop_scans_resident_frames: 0,
            loop_scans_needed: true,
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
    /// never priced one by one: the size is the middle of the 46.1–46.8 MiB
    /// median band of the same 208-volume measurement the reserve was rounded
    /// up from. The same scene before its first volume arrives — every frame
    /// pending, at the reserve — is [`huge_pending`], and the two together
    /// are the same leg at its two prices.
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
    /// 46.5 MiB, the middle of the measured median band. A stand-in for the
    /// leg's own volumes, which were never priced one by one.
    pub(crate) const HUGE_LEG_SCAN_BYTES: u64 = 48_758_784;

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
            volume_grids: 1,
            ground,
            buildings: false,
            overlay_pictures: 0,
            picture_px: [0, 0],
            loop_scans_shared: false,
            loop_scans_resident_bytes: 0,
            loop_scans_resident_frames: 0,
            loop_scans_needed: true,
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
