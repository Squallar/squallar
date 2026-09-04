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
}

impl Scene {
    /// Nothing on screen: what a fresh application has before its first frame.
    pub fn empty() -> Self {
        Self {
            panes: Vec::new(),
            tile_sources: Vec::new(),
            mirror_px: [0, 0],
        }
    }
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
    /// budget's oversampling, and one more picture for the arrival in flight.
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
    pub(crate) fn huge(pictures: usize) -> Scene {
        const MEASURED_STYLED_ENTRY_BYTES: usize = 1_462_708;
        Scene {
            panes: vec![PaneNeed {
                overlay_pictures: pictures,
                picture_px: [2878, 1611],
                ..plan_pane([0, 0], true, 2 * 60 * 60, Some(259))
            }],
            tile_sources: vec![TileNeed {
                tiles_on_glass: 187,
                ancestor_net: 6,
                bytes_per_tile: MEASURED_STYLED_ENTRY_BYTES,
            }],
            mirror_px: [0, 0],
        }
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
        }
    }

    /// The scenes every profile is fitted against: nothing; one loop; a full
    /// screen of two-hour loops; a 3D pane drawing ground; the same pane
    /// drawing buildings too; and the user's own 2878 x 1651 window with the
    /// 193 tiles it needs between zooms, at the 1.03 MB a styled entry was
    /// measured to cost.
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
                },
            ),
            (
                "six looping panes at two hours",
                Scene {
                    panes: vec![plan_pane(HD, true, TWO_HOURS, PRECIP); 6],
                    tile_sources: Vec::new(),
                    mirror_px: [0, 0],
                },
            ),
            (
                "one volume pane with ground",
                Scene {
                    panes: vec![volume_pane([2560, 1440], GroundPass::On)],
                    tile_sources: Vec::new(),
                    mirror_px: [2560, 1440],
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
                },
            ),
            (
                "the huge leg: thirteen pictures on the user's canvas",
                huge(13),
            ),
        ]
    }
}
