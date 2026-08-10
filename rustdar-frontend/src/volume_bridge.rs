//! The end of the 3D wire: what a `rustdar-egui` 3D pane asks for, and the wgpu
//! that answers it.
//!
//! Three things live here, and they are separate because they have three
//! different lifetimes.
//!
//! * [`VolumeStore`] — the built voxel grids, refcounted **by target** so two
//!   panes on one volume share one build. Lives as long as the `App`, survives a
//!   surface loss, and holds no GPU handle at all.
//! * [`VolumePainter`] — the object `Gui` is handed. Lives as long as a renderer:
//!   dropped by `clear_graphics_state` on suspend and on surface loss, which is
//!   what makes a stale GPU handle unreachable rather than merely unused.
//! * [`VolumeResources`] — the wgpu side, inside egui's `CallbackResources`.
//!   Lives as long as the `EguiRenderer` that owns the map.
//!
//! # The one hazard this module is written around
//!
//! `egui_wgpu` downcasts the `Arc<dyn Any>` in a `PaintCallback`. A payload of
//! the wrong type produces one `log::warn!` in `prepare` and a **silent
//! `continue`** in `paint`: a pane that draws nothing, with no panic, no error
//! on screen, and no failing test. Everything that can be tested without a GPU
//! therefore is, and the one thing that cannot — that the payload downcasts —
//! has its own test here, in the only crate that can name both types.
//!
//! # The transfer function: per-product profiles, and the gate that remains
//!
//! This module once refused five of the six samplable moments, because a
//! volume drawn through a palette designed for a plan view — where opacity
//! carries no meaning, since nothing is behind anything — saturates into a
//! solid block. That was rendered, not predicted: at 80 km half-width on
//! KSRX, 2026-07-30 22:33Z, reflectivity resolved into convective cells
//! standing above a stratiform sheet, and velocity — the same volume,
//! 677 933 cells with data — filled the pane with opaque green edge to edge.
//! Only reflectivity's palette had a transparency floor (a 64-index fade
//! band); the other five measured 0 and were refused by that number.
//!
//! The products WP made the five presentation judgements that refusal
//! deferred: every samplable moment's voxel table now ships a **per-product
//! transparency profile**, built into the grid's own LUT by
//! `rustdar_radar::voxel`'s `volume_alpha_scale` and documented there,
//! constant by constant. The judgements are shaped to each moment's physics
//! rather than forced onto the bottom of its ramp — the earlier measurement
//! that a forced 64-index bottom fade left velocity "still unusable — a
//! speckled disc" stands, and is exactly why velocity's see-through band is
//! its *middle* (calm air), ρHV's is its *top* (uniform precipitation), and
//! ΦDP's is a flat translucency over its whole site-offset, range-cumulative
//! scale.
//!
//! What remains here is a **gate, not a repair**: [`palette_refusal_for`]
//! refuses a grid whose table has fewer than [`MINIMUM_FADE_INDICES`]
//! see-through entries *anywhere on its ramp*
//! (`VoxelGrid::see_through_indices`). With the profiles shipped, every
//! samplable moment clears it — the gate's remaining job is to catch the
//! regression where a palette or profile change ships a wall-to-wall opaque
//! table again, and to say why the render would be a block rather than
//! painting one. It reads the grid's own table, never the user's Volume Alpha
//! curve: a curve cannot un-refuse a table (nor re-refuse one — a user who
//! paints their curve opaque gets the block they drew, on purpose).
//!
//! # The interpolation half of the story, and the second channel that fixed it
//!
//! This module's doc used to end by naming what the transfer function could
//! not repair: the volume texture was sampled `Linear` over `R8Unorm` palette
//! indices, so a fetch at a data/no-data boundary swept the bottom of the ramp
//! inside one voxel — for the two sequential moments harmlessly, and for the
//! other seven straight through palette bands the data never occupied. The
//! shipped mitigation was a per-product table that sent those seven to a
//! nearest-neighbour march: honest, and blocky.
//!
//! The clean fix it named — "a second channel saying *this cell has data*" —
//! is what the volume texture now carries. `VOLUME_TEXTURE_FORMAT` is
//! `Rg16Float` holding `R = coverage × index`, `G = coverage`; the shader
//! filters both `Linear` and reconstructs `index = R̄ / Ḡ`, the
//! coverage-weighted mean over covered texels alone. Air contributes 0 to
//! numerator and denominator alike, so it cannot drag a boundary sample
//! anywhere — the reconstruction lands inside the convex hull of the stored
//! indices around it, for every product. The per-product table and the nearest
//! path are both gone: all nine products take one reconstruction, and this
//! module no longer makes a per-product decision about it at all.
//!
//! What survives here is exactly the transfer-function half above: the
//! profiles, the refusal gate, the fade-anchored skip threshold and
//! [`EDGE_SOFT_WIDTH`]. Those are statements about the *palette*, and coverage
//! is a statement about the *measurement*; they were only ever entangled
//! because the palette's fade band was standing in for a mask the texture did
//! not carry.

use std::any::Any;
use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use egui_wgpu::wgpu;
use rustdar_egui::pane::VolumeTarget;
use rustdar_egui::volume_alpha::AlphaCurve;
use rustdar_egui::volume_view::{VolumeFrameState, VolumePaint, VolumePainter, view_for};
use rustdar_radar::voxel::VoxelGrid;

use crate::egui_renderer::AttachmentConfig;
use crate::volume::VolumeSupport;
use crate::volume::quality::VolumeQuality;
use crate::volume::raymarch::{OffscreenTarget, VolumePipelines, VolumeTextures};
use crate::volume::uniform::VolumeUniform;

/// The fewest see-through entries a grid's table may have, anywhere on its
/// ramp, before this renderer refuses to draw a volume through it.
///
/// Compared against `VoxelGrid::see_through_indices` — the count of data
/// entries at or under a quarter opacity, wherever they sit — because since
/// the per-product profiles landed the see-through band is mid-ramp for a
/// diverging moment and top-of-ramp for ρHV; the *bottom*-run measurement
/// (`fade_band`) still anchors the march's skip threshold, which really is
/// about the bottom.
///
/// 16 rather than 1 so that a table with a token one- or two-entry floor
/// cannot clear a `> 0` test; 16 rather than 64 so that the value is not
/// mistaken for reflectivity's own band, which it has no relationship to.
/// Every shipped profile clears it by at least 2× (the measured table lives
/// with `the_default_transparency_profile_is_measured_per_product` in
/// `rustdar_radar::voxel`), so a failure here is a regression in a palette or
/// profile, not a tuning problem.
///
/// It is a **bar**, not a repair — nothing here rewrites a colour table.
pub const MINIMUM_FADE_INDICES: u8 = 16;

/// Width of the shader's opacity ramp, in its 0-1 index units: eight palette
/// indices.
///
/// The ramp starts at the palette's own fade boundary —
/// [`empty_index_threshold_for`], half an index below the first entry whose
/// alpha is not zero — and reaches full palette alpha eight indices above it.
/// Without it the boundary is an alpha cliff one Nearest-sampled LUT step
/// wide: at an echo edge the interpolated index crosses that step inside a
/// single voxel, so every shelf and every echo top wears a hard rim
/// (the terraced shells of the 2026-08-09 report). Eight indices is half a
/// [`MINIMUM_FADE_INDICES`] and, on reflectivity's 0.5 dB-per-index ramp,
/// 4 dBZ of fade — chosen by rendering the KCRP 2017-08-26 (Harvey) volume at
/// 4, 8 and 16: at 4 the tilt shelves keep a faint rim, at 16 the shelf
/// structure the render is supposed to keep legible starts to blur together,
/// and 8 has neither. It softens *presentation* only: the palette, the field
/// and the skip threshold's position all stay the data's own.
///
/// One global constant, with no per-product plumbing behind it: the queued
/// products WP must make widening a per-product decision — a categorical
/// palette (HHC) must never be softened at its class boundaries — and today
/// the only guard is that HHC cannot reach this march at all, because
/// `rustdar_radar::sampler`'s samplable gate refuses every non-moment product
/// before a grid exists.
pub const EDGE_SOFT_WIDTH: f32 = 8.0 / 255.0;

/// Cells one march step advances along the ray on the cloud rung.
///
/// Half the instrument default. A finer step buys no resolution — the linear
/// filter band-limits the field to about a cell — but it halves the per-step
/// opacity quantum, and that quantum is what the per-pixel jitter turns into
/// visible noise: at one-cell steps over the Harvey volume each contributing
/// step absorbed ~35% of the remaining light, so the jittered comb position
/// moved a pixel's total opacity by whole shade steps and the deck wore an
/// ordered stipple. At half-cell steps the residual drops below the eight-bit
/// level and the surface reads as continuous. The cost is linear in the step
/// count and was measured, not assumed — see the table in `volume::quality`.
pub const CLOUD_STEP_CELLS: f32 = 0.5;

/// The reconstruction level the cloud look marches the grid at, in mip units
/// — **the ceiling of the knob's travel, not what every box gets**. The level
/// a frame actually marches at is [`cloud_reconstruction_lod_for`], which
/// tapers this to zero as the grid's cells coarsen.
///
/// 1.0 is the full blend into the hand-built two-cell mean below the raw
/// field — chosen by rendering the KCRP 2017-08-26 (Harvey) volume across the
/// knob's travel *at a region box*: below ~0.7 the single-voxel spikes over
/// the deck survive as hairs and the tilt shelves keep their cliff rims, and
/// there is nothing past 1.0 to reach. It is a *render* softness, the same
/// class of decision as [`EDGE_SOFT_WIDTH`]: the grid, the palette and the
/// threshold anchor are untouched, and the instrument default
/// (`VolumeUniform::new`) stays 0 — the bit-exact raw field.
pub const CLOUD_RECONSTRUCTION_LOD: f32 = 1.0;

/// Cell size at or below which the cloud rung smooths at the full
/// [`CLOUD_RECONSTRUCTION_LOD`], in kilometres per cell.
///
/// 0.65 km covers both shipped region rungs on the desktop grid — a 60 km
/// box is 0.23 km/cell and a 160 km one 0.625 — where the two-cell kernel
/// (≤ 1.3 km) stays inside the few-kilometre width of a real convective
/// core, so the smoothing softens the *rendering* of a feature the grid
/// still resolves. Measured on the Harvey eyewall (see
/// [`cloud_reconstruction_lod_for`]).
pub const CLOUD_SMOOTHING_FULL_CELL_KM: f32 = 0.65;

/// Cell size at or above which the cloud rung smooths not at all, in
/// kilometres per cell.
///
/// 1.75 km puts the default whole-volume box — 460 km over 256 cells,
/// 1.8 km/cell — at exactly zero: there the two-cell kernel is 3.6 km, wider
/// than the features it lands on, and the smoothing was measured *erasing*
/// them rather than softening them (the Harvey table in
/// [`cloud_reconstruction_lod_for`]).
pub const CLOUD_SMOOTHING_RAW_CELL_KM: f32 = 1.75;

/// The reconstruction level the cloud rung marches a grid of this cell size
/// at: [`CLOUD_RECONSTRUCTION_LOD`] at or below
/// [`CLOUD_SMOOTHING_FULL_CELL_KM`], zero at or above
/// [`CLOUD_SMOOTHING_RAW_CELL_KM`], linear between. `largest_cell_km` is the
/// grid's coarsest axis — the kilometres one cell spans, which on every
/// shipped box is the horizontal.
///
/// # Why the smoothing scales with cell size
///
/// Smoothing is a reconstruction luxury: it is honest exactly when the data
/// outresolves the display, so the kernel rounds off sampling artifacts of a
/// feature the grid still holds. When the cells are coarser than the
/// features, the same kernel averages the features *away*. Measured — KCRP
/// 2017-08-26 04:41Z (Harvey), `volume_real_mask` hard-mask painted-pixel
/// counts at the class cut, desktop shape, one camera (yaw 225, pitch 25,
/// dist 2.5), centre 28.02 N −97.05 E, cloud step 0.5; Δ is against the raw
/// field (LOD 0, step 1) at the same box:
///
/// | box | km/cell | LOD at 1.0 (shipped fixed) | LOD by this taper |
/// |---|---|---|---|
/// | 60 km | 0.23 | ≥20 dBZ −1.6%, ≥35 −6.6%, ≥50 −31% | same (taper = 1.0) |
/// | 160 km | 0.625 | ≥20 −0.8%, ≥35 −15%, ≥50 −51% | same (taper = 1.0) |
/// | 460 km (default) | 1.80 | ≥20 −3.2%, ≥35 **−30%**, ≥50 **−100%** (0 px) | ≥20 +1.0%, ≥35 +3.0%, ≥50 +17% of 83 px |
///
/// At the shipped default view the ≥50 dBZ eyewall pixels went to **zero**
/// under the fixed LOD — the 2D pane showed a red core the 3D pane had
/// erased — and that erasure is the kernel's, not only the old mip's
/// no-data bias: the figures above are already through the
/// occupancy-weighted mip (`volume::raymarch::downsampled_grid`), which
/// recovers the data-edge classes a few percent (≥20 dBZ at the default box:
/// −8.6% with the full-cube mean, −3.2% with the occupancy mean) but cannot
/// save a core thinner than the kernel. The remaining region-box ≥50 losses
/// are the kernel averaging *measured* neighbours below the cut — the
/// honest price of the cloud look where it is still bought.
///
/// The knee values are the measured rungs, not a curve fit: full smoothing
/// at the region boxes that keep the cloud look under it (the 60 km
/// before/after renders differ in 0.8% of pixels), none at the default box
/// the kernel erases, and a linear ramp between because nothing measured
/// justifies a fancier shape.
pub fn cloud_reconstruction_lod_for(largest_cell_km: f32) -> f32 {
    let travel = CLOUD_SMOOTHING_RAW_CELL_KM - CLOUD_SMOOTHING_FULL_CELL_KM;
    let weight = ((CLOUD_SMOOTHING_RAW_CELL_KM - largest_cell_km) / travel).clamp(0.0, 1.0);
    CLOUD_RECONSTRUCTION_LOD * weight
}

/// The march's skip threshold for a palette whose
/// [`VoxelGrid::fade_band`](rustdar_radar::voxel::VoxelGrid::fade_band) is
/// `band`, in the shader's 0-1 index units.
///
/// `fade_band()` is a **count**: how many indices above the no-data index are
/// still fully transparent. The first entry whose alpha is not zero is
/// therefore `band + 1`, and a Nearest-sampled LUT fetch of an interpolated
/// grid index `i` (in 0-1 units) returns a visible entry exactly when
/// `i * 255 > band + 0.5` — the midpoint between the last transparent entry
/// and the first visible one. So `(band + 0.5) / 255` is the exact boundary:
/// below it the march can skip the sample — and its up-to-seven shading
/// fetches — without changing a pixel, and the [`EDGE_SOFT_WIDTH`] ramp rises
/// from the same boundary, so the first visible index fades in at about 1%
/// opacity instead of arriving as a cliff.
///
/// An earlier version anchored at `(band - 0.5) / 255`, one whole index low:
/// a one-index shell of guaranteed-transparent samples paid full fetch cost
/// for nothing, and the ramp's foot sat below the palette's own fade boundary
/// so the first visible index rendered at ~9% opacity. One function on
/// purpose — the march-cost and real-mask harnesses import it, so an anchor
/// change here cannot leave the instruments measuring a different threshold
/// than production ships.
pub fn empty_index_threshold_for(band: u8) -> f32 {
    (f32::from(band) + 0.5) / 255.0
}

/// The fade band the march should anchor on: the palette's own, unless the
/// user has drawn a Volume Alpha curve — then the **curve's**.
///
/// # The fade-anchor decision, in full
///
/// The skip threshold and the soft-edge ramp both anchor at
/// [`empty_index_threshold_for`] of this band, and the band must describe the
/// alpha the march will actually fetch — which, with a curve applied, is the
/// curve and not the palette. Anchoring on the palette while rendering
/// through the curve fails in both directions at once:
///
/// * A user who **strips the low end** (the canonical Volume Alpha gesture —
///   erase the sub-30 dBZ haze) raises the first visible entry far above the
///   palette's band. The palette-anchored march would sample — and shade, up
///   to seven fetches per step — every cell in the stripped shell, paying
///   full cost for guaranteed-zero alpha; and the soft ramp's foot would sit
///   dozens of indices below the first visible entry, so the new visible
///   bottom would arrive as the hard cliff the ramp exists to dissolve.
/// * A user who **paints alpha into the palette's fade band** lowers the
///   first visible entry below the palette's band. The palette-anchored
///   march would *skip* those samples: visible data, silently erased —
///   the one thing a skip threshold must never do.
///
/// So the threshold follows the effective curve, exactly:
/// [`AlphaCurve::fade_band`] mirrors [`VoxelGrid::fade_band`]'s rule entry
/// for entry, and the separation property — the threshold sits strictly
/// between the last transparent entry and the first visible one — holds for
/// every curve by the same arithmetic the palette case is pinned by. Zero-
/// alpha runs *above* the first visible entry are not skipped, only unlit
/// (`entry.a = 0` absorbs nothing): conservative, correct, and the cost the
/// user asked for. An all-transparent curve yields band 255, a threshold
/// above every representable index, and an honestly empty pane — no
/// division anywhere on the path (the ramp's divisor is the constant
/// [`EDGE_SOFT_WIDTH`], floored at `1e-6` in the shader).
///
/// The **refusal gate** ([`palette_refusal`]) deliberately stays on the
/// palette's band: it is a statement about the product's palette design —
/// "this table was built for a plan view" — not about the session's curve. A
/// refused moment never reaches the LUT seam, so a curve cannot un-refuse
/// velocity; and a reflectivity curve that paints the low end cannot refuse
/// the user out of their own product mid-edit. The instrument path is
/// untouched by construction: `VolumeUniform::new`'s defaults and the
/// GPU-test uploads never see a frame state, which is the only carrier a
/// curve has.
pub fn effective_fade_band(palette_band: u8, curve: Option<&AlphaCurve>) -> u8 {
    curve.map_or(palette_band, AlphaCurve::fade_band)
}

/// The colour table as the GPU should hold it: the grid's own bytes, with the
/// alpha channel replaced by the user's curve when one exists.
///
/// `None` borrows the input unchanged — **bit-exact by construction**, which
/// is the untouched editor's whole contract: not "an equal copy" but the very
/// bytes `VoxelGrid::lut()` handed over, so no rewrite of this function can
/// drift the no-curve path away from the palette. `Some` copies once and
/// touches only every fourth byte: colours are the palette's at every entry,
/// alpha is the curve's, and entry 0 is forced transparent a third time here
/// (after the curve's constructor and the stroke's re-clamp) because this is
/// the last line before the bytes leave the CPU.
pub fn effective_lut<'a>(base: &'a [u8], curve: Option<&AlphaCurve>) -> Cow<'a, [u8]> {
    let Some(curve) = curve else {
        return Cow::Borrowed(base);
    };
    let mut out = base.to_vec();
    for (entry, alpha) in out.chunks_exact_mut(4).zip(curve.alphas()) {
        entry[3] = *alpha;
    }
    if let Some(no_data) = out.get_mut(3) {
        *no_data = 0;
    }
    Cow::Owned(out)
}

/// A voxel grid the store is holding, or the state of not holding one yet.
#[derive(Clone)]
pub enum VolumeEntry {
    /// A build is in flight for this target.
    ///
    /// This entry **is** the dedupe for the worker path. `PrepareVolume` is
    /// level-triggered — the pane re-asks every frame — and when the build was
    /// synchronous, what stopped the storm was the result existing before the
    /// next frame. A posted job leaves nothing in hand for hundreds of
    /// milliseconds, so this placeholder stands in: a second frame, or a
    /// second pane, finds it and attaches instead of dispatching again.
    Building,
    /// Built. The `Arc` is shared with every callback that draws it.
    Ready(Arc<VoxelGrid>),
    /// Not built, and why — in a sentence fit for the centre of a pane.
    ///
    /// Kept rather than retried, because every reason `build_voxels` returns
    /// `None` is a property of the volume rather than of the moment: a scan with
    /// no coverage pattern (a volume joined mid-flight, before its VCP message
    /// lands) does not acquire one, and a product with no native moment never
    /// gains one. Retrying every frame would be a 100 ms resample per frame that
    /// fails identically each time. A *new* volume gets a new target and a
    /// fresh answer.
    Refused(String),
}

/// How a holder holds what it asks the store for.
///
/// The store began with one rule — a pane holds one grid, and attaching to a
/// new one sheds the old — because "one pane shows one volume" was the whole
/// truth. A 3D loop breaks that: its frames *are* grids, and the pane holds
/// every one of them at once. The two rules cannot both be the default, and
/// leaving it implicit would mean a loop's set being silently shed by the next
/// live rebuild that came through [`VolumeStore::share`].
///
/// So the holder says which it means, and the shed is conditional on the
/// answer rather than on what the store can infer. What replaces the shed for
/// a set holder is [`VolumeStore::retain_set`] — the holder states the whole
/// set, and everything outside it goes — plus
/// [`VolumeStore::enforce_budget`], which is the only hard bound on the
/// store's size.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hold {
    /// One grid at a time. Attaching sheds everything else this holder has,
    /// keeping a same-scope resolved grid only while a build is in flight —
    /// the seamless swap.
    Single,
    /// One of a set. Attaching sheds nothing, and the holder is obliged to
    /// state the whole set through [`VolumeStore::retain_set`] on every pass
    /// so that a set it has stopped wanting is released rather than leaked.
    Set,
}

/// The built voxel grids, refcounted by target.
///
/// # Why refcounting is by target and not by pane
///
/// Two 3D panes showing the same volume and moment — the ordinary way to compare
/// two camera angles — must share one 8 MiB build and one GPU upload. Keying the
/// store by pane would build it twice and upload it twice, and nothing on screen
/// would say so.
///
/// # Why a `Mutex` and not a `RefCell`
///
/// `VolumePainter` is `Send + Sync`, because egui's callback payloads are
/// required to be and the `Gui` holds the painter across frames. `RefCell` is
/// neither. The lock is uncontended in practice — every access is on the frame
/// thread — and the alternative is a bound that would have to be unpicked the
/// first time anything touches this from a worker.
pub struct VolumeStore {
    inner: Mutex<StoreInner>,
}

#[derive(Default)]
struct StoreInner {
    /// The next id to hand out. Ids identify an upload on the GPU side, where
    /// `VolumeTarget` cannot go: it holds a `NaiveDateTime` and a `String` and
    /// is not `Hash`, and making it so would put a hashing obligation on a UI
    /// type for the sake of a texture cache.
    next_id: u64,
    /// At most one per 3D pane, so a linear scan is the right structure —
    /// and it means `VolumeTarget`'s derived `PartialEq` is the only comparison
    /// needed, rather than a hand-written `Hash` that has to agree with it.
    entries: Vec<StoredVolume>,
    /// Panes holding a *set* rather than one grid — see [`Hold::Set`].
    ///
    /// A list beside the entries rather than a flag on each `panes` element,
    /// because the property belongs to the **holder**, not to the holding: a
    /// pane animating a 3D loop holds every one of its grids the same way, and
    /// the question `shed` and `complete` have to ask is "may I shed this
    /// pane's other entries", which is about the pane alone. Recording it per
    /// entry would let one pane be a set holder for some of its own grids and
    /// not others, which is not a state that means anything.
    set_holders: Vec<usize>,
}

struct StoredVolume {
    id: u64,
    target: VolumeTarget,
    entry: VolumeEntry,
    /// Which panes are holding this. Empty is impossible: the entry is dropped
    /// when the last pane lets go.
    panes: Vec<usize>,
}

impl StoredVolume {
    /// GPU texture bytes this entry's upload occupies, or 0 while there is
    /// nothing uploaded.
    ///
    /// Computed from the grid's own shape through
    /// `raymarch::grid_bytes_with_mips` — the same arithmetic the upload path
    /// allocates by, including the coarse mip level — rather than from a
    /// per-target constant, because the eviction has to measure what is
    /// actually resident, and a runtime step-down can hand the store a grid
    /// smaller than [`crate::constants::VOLUME_GRID_CELLS`].
    ///
    /// A shape whose product overflows a `usize` reports 0 rather than
    /// panicking in the paint path. It cannot happen — the shapes are
    /// compiled-in — and a store that panicked while counting bytes would take
    /// the frame thread with it.
    fn texture_bytes(&self) -> usize {
        let VolumeEntry::Ready(grid) = &self.entry else {
            return 0;
        };
        let shape = grid.shape();
        let Ok(cells) = [shape.nx, shape.ny, shape.nz]
            .iter()
            .map(|&n| u32::try_from(n))
            .collect::<Result<Vec<u32>, _>>()
            .map(|v| [v[0], v[1], v[2]])
        else {
            return 0;
        };
        crate::volume::raymarch::grid_bytes_with_mips(cells)
            .unwrap_or(0)
            .saturating_add(crate::constants::VOLUME_LUT_BYTES)
    }
}

/// What the store holds for one target, with the id its GPU upload is keyed by.
pub struct VolumeLookup {
    pub id: u64,
    pub entry: VolumeEntry,
}

impl VolumeStore {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(StoreInner::default()),
        }
    }

    /// Attach `pane_idx` to `target`'s entry if one exists — built, building
    /// or refused — and say whether it did.
    ///
    /// `true` means the pane is served: a grid is in hand, a build is already
    /// in flight, or the volume was refused. `false` means nothing is known
    /// about this target and the caller owns dispatching a build.
    ///
    /// The two halves are one call because they have to be atomic against each
    /// other: a second pane asking for a volume that is already in hand or in
    /// flight must attach without triggering a second build. Attaching also
    /// sheds what the pane can no longer show — see [`StoreInner::shed`] — but
    /// deliberately keeps a same-scope `Ready` grid when the found entry is
    /// still `Building`: that old grid is the picture the pane goes on
    /// painting until the new one lands, which is what makes a live rebuild a
    /// seamless swap rather than a flash of "Building…" every sealed sweep.
    pub fn share(&self, pane_idx: usize, target: &VolumeTarget) -> bool {
        self.share_held(pane_idx, target, Hold::Single)
    }

    /// [`Self::share`], saying how the holder holds it. See [`Hold`].
    pub fn share_held(&self, pane_idx: usize, target: &VolumeTarget, hold: Hold) -> bool {
        let mut inner = self.lock();
        let Some(found) = inner.entries.iter().position(|e| &e.target == target) else {
            return false;
        };
        let keep_old = matches!(inner.entries[found].entry, VolumeEntry::Building);
        match hold {
            Hold::Single => {
                inner.set_holders.retain(|&p| p != pane_idx);
                inner.shed(pane_idx, target, keep_old);
            }
            Hold::Set => inner.mark_set_holder(pane_idx),
        }
        // Re-found after the shed, which prunes entries and moves positions.
        // The target's own entry cannot have been pruned — `shed` skips it and
        // an entry always has at least one pane — but where it sits can shift,
        // and indexing by the stale position was an out-of-bounds panic the
        // store tests caught.
        let Some(entry) = inner.entries.iter_mut().find(|e| &e.target == target) else {
            return false;
        };
        if !entry.panes.contains(&pane_idx) {
            entry.panes.push(pane_idx);
        }
        true
    }

    /// Open a `Building` entry for `target`, attached to `pane_idx` — the
    /// worker path's in-flight marker, opened at dispatch.
    ///
    /// Sheds the pane's other `Building` entry if it has one: a pane re-aimed
    /// mid-build supersedes its own build, and the orphaned entry's absence is
    /// what makes the stale reply drop in [`Self::complete`]. The pane's
    /// same-scope `Ready` grid is kept — it is the picture on screen until
    /// this build lands.
    pub fn begin_build(&self, pane_idx: usize, target: &VolumeTarget) {
        self.begin_build_held(pane_idx, target, Hold::Single);
    }

    /// [`Self::begin_build`], saying how the holder holds it. See [`Hold`].
    pub fn begin_build_held(&self, pane_idx: usize, target: &VolumeTarget, hold: Hold) {
        let mut inner = self.lock();
        match hold {
            Hold::Single => {
                inner.set_holders.retain(|&p| p != pane_idx);
                inner.shed(pane_idx, target, true);
            }
            Hold::Set => inner.mark_set_holder(pane_idx),
        }
        let id = inner.next_id;
        inner.next_id += 1;
        inner.entries.push(StoredVolume {
            id,
            target: target.clone(),
            entry: VolumeEntry::Building,
            panes: vec![pane_idx],
        });
    }

    /// Resolve `target`'s `Building` entry with what the build produced, and
    /// say whether anything was waiting for it.
    ///
    /// `false` drops the result on the floor, and that is correct for both
    /// ways it happens: the build was superseded (every pane re-aimed and the
    /// orphaned entry was pruned) or already resolved (a duplicate reply). On
    /// `true`, every attached pane sheds its other entries — the old grids it
    /// was painting through the wait — which is the other half of the
    /// seamless swap.
    pub fn complete(&self, target: &VolumeTarget, entry: VolumeEntry) -> bool {
        let mut inner = self.lock();
        let Some(found) = inner
            .entries
            .iter()
            .position(|e| &e.target == target && matches!(e.entry, VolumeEntry::Building))
        else {
            return false;
        };
        inner.entries[found].entry = entry;
        let panes = inner.entries[found].panes.clone();
        for pane in panes {
            // A set holder is exempt, and this is the line that makes a 3D
            // loop possible at all: the seamless swap's rule is "the grid that
            // just landed supersedes the one this pane was painting through the
            // wait", which is right for one grid and destroys fourteen. What
            // bounds a set holder instead is `retain_set` and `enforce_budget`.
            if inner.set_holders.contains(&pane) {
                continue;
            }
            inner.shed(pane, target, false);
        }
        true
    }

    /// State the whole set `pane_idx` holds, detaching it from everything else
    /// and dropping whatever nobody is left holding. Returns how many entries
    /// were dropped outright.
    ///
    /// The set holder's replacement for [`StoreInner::shed`], and the reason
    /// [`Hold::Set`] is safe: a holder that stops asking for a grid stops
    /// paying for it on the very next pass, without any single attach having to
    /// guess which of its siblings are still wanted.
    ///
    /// Calling it with an empty `keep` is the release-before-build rule a
    /// region, product or vector change needs. `share`'s `keep_old` deliberately
    /// holds the old grid through a rebuild so the swap is seamless — right for
    /// one grid, and for fourteen it is a peak of two full sets at once (1008
    /// MiB against a 512 MiB budget on desktop). A set holder therefore
    /// releases *first* and rebuilds after, and accepts the first-build message
    /// for the fraction of a second that costs.
    pub fn retain_set(&self, pane_idx: usize, keep: &[VolumeTarget]) -> usize {
        let mut inner = self.lock();
        inner.mark_set_holder(pane_idx);
        for entry in &mut inner.entries {
            if keep.contains(&entry.target) {
                continue;
            }
            entry.panes.retain(|&p| p != pane_idx);
        }
        let before = inner.entries.len();
        inner.entries.retain(|e| !e.panes.is_empty());
        before - inner.entries.len()
    }

    /// Whether `pane_idx` is holding a set rather than one grid. See [`Hold`].
    pub fn holds_set(&self, pane_idx: usize) -> bool {
        self.lock().set_holders.contains(&pane_idx)
    }

    /// Release everything `pane_idx` holds **as a set**, and stop treating it
    /// as a set holder. Returns how many entries were dropped outright.
    ///
    /// A no-op for a pane that was never a set holder, and that exemption is
    /// the point: this is called for every pane whose loop is not active, and
    /// a live 3D pane is one of those. Without the exemption it would detach
    /// that pane from the single grid it is painting, every frame, and the
    /// pane would rebuild an 8 MiB grid per frame for ever with a hot CPU as
    /// the only symptom.
    pub fn release_set(&self, pane_idx: usize) -> usize {
        if !self.lock().set_holders.contains(&pane_idx) {
            return 0;
        }
        self.retain_set(pane_idx, &[])
    }

    /// Evict resolved grids, oldest first, until the store's GPU texture bytes
    /// fit `budget`. Returns how many were evicted.
    ///
    /// **The store's only hard bound, and the only one in this file that is
    /// enforced rather than stated.** Every other rule here is about
    /// correctness — what a pane may paint — and bounds memory only as a side
    /// effect of "one pane, one grid". A set holder has no such side effect, so
    /// this is what stands in its place: whatever the holders ask for, the
    /// resident grids fit.
    ///
    /// Oldest-first by store id, which is *build* order rather than playback
    /// order. That is deliberate. In steady state this never fires — the frame
    /// counts in `MAX_LOOP_VOLUME_FRAMES` are chosen to fit — and the case it
    /// exists for is the transition, where a pane holds its live grid and a
    /// loop set at the same time. The live grid was built first, so it is
    /// exactly what should go.
    ///
    /// `Building` entries are never evicted: there is nothing to reclaim (the
    /// grid does not exist yet) and dropping the placeholder would make the
    /// reply that is already in flight a stale one, silently. `Refused` entries
    /// are a sentence in a `String` and are left for the same reason.
    pub fn enforce_budget(&self, budget: usize) -> usize {
        let mut inner = self.lock();
        let mut evicted = 0;
        loop {
            let total: usize = inner.entries.iter().map(StoredVolume::texture_bytes).sum();
            if total <= budget {
                return evicted;
            }
            let Some(oldest) = inner
                .entries
                .iter()
                .filter(|e| matches!(e.entry, VolumeEntry::Ready(_)))
                .map(|e| e.id)
                .min()
            else {
                // Over budget with nothing resolved to give back. Reported by
                // returning what was actually evicted rather than looping: the
                // in-flight builds land, and the next pass reclaims.
                return evicted;
            };
            inner.entries.retain(|e| e.id != oldest);
            evicted += 1;
        }
    }

    /// GPU texture bytes the store's resolved grids occupy — what
    /// [`Self::enforce_budget`] measures.
    ///
    /// Separate from [`Self::memory_bytes`], which is the *host* side, and the
    /// two are different numbers for the same grid: the host holds one byte per
    /// cell of palette index, and the upload turns it into four bytes of
    /// coverage-premultiplied `Rg16Float` plus a mip level plus the LUT. The
    /// budget is about the GPU, so it is this one the eviction measures.
    pub fn texture_bytes(&self) -> usize {
        self.lock()
            .entries
            .iter()
            .map(StoredVolume::texture_bytes)
            .sum()
    }

    /// Record a synchronously-known result. `pane_idx` is attached to it and
    /// holds nothing else afterwards — this is for answers that need no build,
    /// like a refusal decided at dispatch time.
    pub fn insert(&self, pane_idx: usize, target: VolumeTarget, entry: VolumeEntry) {
        self.insert_held(pane_idx, target, entry, Hold::Single);
    }

    /// [`Self::insert`], saying how the holder holds it. See [`Hold`].
    ///
    /// The detach is the part `Hold::Set` turns off, and leaving it on was the
    /// defect that made a 3D loop over volumes with nothing to resample hold
    /// exactly one frame: every refusal detached the pane from the thirteen
    /// entries it already had.
    pub fn insert_held(
        &self,
        pane_idx: usize,
        target: VolumeTarget,
        entry: VolumeEntry,
        hold: Hold,
    ) {
        let mut inner = self.lock();
        match hold {
            Hold::Single => inner.detach(pane_idx),
            Hold::Set => inner.mark_set_holder(pane_idx),
        }
        let id = inner.next_id;
        inner.next_id += 1;
        inner.entries.push(StoredVolume {
            id,
            target,
            entry,
            panes: vec![pane_idx],
        });
    }

    /// This pane is holding nothing. Drops whatever it was holding if it was
    /// the last one.
    pub fn release(&self, pane_idx: usize) {
        self.lock().detach(pane_idx);
    }

    /// Drop every entry whose target names `product`.
    ///
    /// For a render parameter that is not part of the target: the storm
    /// motion vector changes what an SRV grid *contains* without changing the
    /// `VolumeTarget` that keys it, so an override edit must evict here or
    /// every SRV pane keeps painting the old vector's field for the rest of
    /// the volume. The plan-view cache has the same rule
    /// (`RenderDispatcher::set_storm_motion_override`); this is its 3D
    /// counterpart. Panes re-ask through the level-triggered `PrepareVolume`
    /// once their `rendered_for` is cleared, which the caller does.
    pub fn evict_product(&self, product: rustdar_radar::types::RadarProduct) {
        let mut inner = self.lock();
        inner.entries.retain(|e| e.target.product != product);
    }

    /// What is in hand for `target`, if anything.
    pub fn lookup(&self, target: &VolumeTarget) -> Option<VolumeLookup> {
        let inner = self.lock();
        inner
            .entries
            .iter()
            .find(|e| &e.target == target)
            .map(|e| VolumeLookup {
                id: e.id,
                entry: e.entry.clone(),
            })
    }

    /// What pane `pane_idx` should paint for `target`: the target's own entry
    /// when it is resolved, else the newest same-scope grid the pane still
    /// holds — the old picture, painted through a rebuild.
    ///
    /// `None` while nothing is paintable at all, which the painter renders as
    /// the first-build message. The fallback is scoped to the same site,
    /// product and region on purpose: after a site or product switch the old
    /// grid answers a question nobody is asking, and painting it with a
    /// caption describing the new target would be the lie the swap must never
    /// tell. Newest-first because a pane can transiently hold two resolved
    /// grids and the later build is the newer picture.
    pub fn lookup_for_pane(&self, pane_idx: usize, target: &VolumeTarget) -> Option<VolumeLookup> {
        let inner = self.lock();
        if let Some(found) = inner
            .entries
            .iter()
            .find(|e| &e.target == target && !matches!(e.entry, VolumeEntry::Building))
        {
            return Some(VolumeLookup {
                id: found.id,
                entry: found.entry.clone(),
            });
        }
        // The `same_scope` clause is **belt and braces, and no test can see
        // it**: `share` and `begin_build` shed the pane's out-of-scope
        // entries before this can run, so under the public API there is never
        // an out-of-scope grid attached to fall back to — mutation testing
        // confirmed removing *this clause alone* changes nothing observable.
        // The scope decision itself is load-bearing one layer down, in
        // `shed`'s `keep_old` arm, and that layer is what
        // `an_out_of_scope_grid_never_stands_in` pins — against a held
        // `Ready` grid, the one shape that can ever stand in. (An earlier
        // note here implied the shed layer was already covered; it was not:
        // with the pin's held entry a `Refused` stub, the `Ready`-match
        // refused it before any scope decision, and a `same_scope` answering
        // always-true survived the whole suite.) The clause stays because the
        // two guards protect different things (`shed` bounds memory, this
        // bounds what is *painted*), and a future caller that attaches
        // without shedding would otherwise paint another site's storm under
        // this pane's caption — the one lie the swap must never tell,
        // recorded here rather than left as an unexplained survivor.
        inner
            .entries
            .iter()
            .filter(|e| {
                e.panes.contains(&pane_idx)
                    && same_scope(&e.target, target)
                    && matches!(e.entry, VolumeEntry::Ready(_))
            })
            .max_by_key(|e| e.id)
            .map(|e| VolumeLookup {
                id: e.id,
                entry: e.entry.clone(),
            })
    }

    /// Every id the store is still holding. The GPU side keeps exactly these
    /// uploads and frees the rest.
    pub fn live_ids(&self) -> Vec<u64> {
        self.lock().entries.iter().map(|e| e.id).collect()
    }

    /// Host bytes the store is holding, and how many volumes that is.
    ///
    /// Reported rather than bounded, and logged on every build — because the
    /// bound is "one grid per 3D pane", and 8 MiB a pane is the kind of figure
    /// that wants to be visible in a log the day someone finds a path that
    /// keeps a grid a pane no longer needs. One such path is already known:
    /// reducing the pane count hides a 3D pane without converting it, and
    /// `ReleaseVolume` fires only on a *kind* change.
    pub fn memory_bytes(&self) -> usize {
        self.lock()
            .entries
            .iter()
            .map(|e| match &e.entry {
                VolumeEntry::Building => 0,
                VolumeEntry::Ready(grid) => grid.memory_bytes(),
                VolumeEntry::Refused(why) => why.len(),
            })
            .sum()
    }

    /// A poisoned lock is recovered from rather than propagated.
    ///
    /// The only thing that can poison it is a panic inside one of the six short
    /// methods above, none of which can panic on their own — so a poisoned lock
    /// means the process is already unwinding. Taking the guard anyway keeps a
    /// second panic out of the paint path, where on wasm a main-thread panic
    /// aborts the whole application.
    fn lock(&self) -> std::sync::MutexGuard<'_, StoreInner> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

impl Default for VolumeStore {
    fn default() -> Self {
        Self::new()
    }
}

impl StoreInner {
    /// Detach `pane_idx` from whatever it holds, dropping entries nobody
    /// holds.
    fn detach(&mut self, pane_idx: usize) {
        self.set_holders.retain(|&p| p != pane_idx);
        for entry in &mut self.entries {
            entry.panes.retain(|&p| p != pane_idx);
        }
        self.entries.retain(|e| !e.panes.is_empty());
    }

    /// Record that `pane_idx` holds a set. Idempotent — every attach a set
    /// holder makes says so, and saying it twice must not double the list.
    fn mark_set_holder(&mut self, pane_idx: usize) {
        if !self.set_holders.contains(&pane_idx) {
            self.set_holders.push(pane_idx);
        }
    }

    /// Detach `pane_idx` from everything it can no longer show, given that it
    /// is now aimed at `target`.
    ///
    /// Always sheds out-of-scope entries (another site, product or region —
    /// nothing there is ever painted for this target again) and the pane's
    /// other `Building` entries (a pane supersedes its own in-flight build by
    /// re-aiming). Sheds same-scope resolved entries too unless `keep_old`:
    /// those are the old picture, kept exactly while a build for `target` is
    /// (or is about to be) in flight, painted until it lands.
    fn shed(&mut self, pane_idx: usize, target: &VolumeTarget, keep_old: bool) {
        for entry in &mut self.entries {
            if &entry.target == target {
                continue;
            }
            let keep = keep_old
                && same_scope(&entry.target, target)
                && !matches!(entry.entry, VolumeEntry::Building);
            if !keep {
                entry.panes.retain(|&p| p != pane_idx);
            }
        }
        self.entries.retain(|e| !e.panes.is_empty());
    }
}

/// Whether two targets differ only in their volume stamp — the same site,
/// moment and region, at two data times. The seamless swap is licensed exactly
/// this far: an older picture of the *same question* may stand in while a
/// newer one builds, and nothing else may.
fn same_scope(a: &VolumeTarget, b: &VolumeTarget) -> bool {
    a.volume.site == b.volume.site && a.product == b.product && a.region == b.region
}

/// The painter a `Gui` is handed. Turns a pane's frame state into a payload
/// `egui_wgpu` can draw, or into a sentence saying why it cannot.
pub struct BridgeVolumePainter {
    store: Arc<VolumeStore>,
    /// The quality this adapter was classified into, from
    /// `AdapterInfo::device_type`. Fixed for the life of the renderer: a device
    /// does not change class, and the thing that *does* change per frame — the
    /// pane's size — is applied by `fit_to_budget` below.
    quality: VolumeQuality,
    /// What the capability probe said when the renderer was built. Re-consulted
    /// through `volume::support` on every frame, so a device error latched
    /// halfway through a session degrades the pane rather than being remembered
    /// only until the next restart.
    probed: VolumeSupport,
    /// The largest floor magnification any pane reported since this was last
    /// taken — what the adaptive mirror rung is chosen from.
    ///
    /// # Why it is recorded here rather than computed where the mirror is sized
    ///
    /// The mirror is planned in `app_render::present_frame`, after the egui pass
    /// has been built, and by then nothing knows a pane's box: the extent comes
    /// off the `VoxelGrid`, which only the store can resolve, through the same
    /// pane-scoped lookup `paint` already does. Recomputing it there would mean
    /// a second lookup per pane per frame, against a store whose answer can have
    /// changed — so the number is taken at the one moment the grid, the camera
    /// and the source pane's affine are all in hand together.
    ///
    /// Interior mutability because [`VolumePainter::paint`] takes `&self`: the
    /// painter is the seam's read-only side by design, and widening it to
    /// `&mut self` to carry one `f32` would put a mutable borrow of the renderer
    /// through the whole UI pass.
    ///
    /// An atomic rather than a `Cell` because `VolumePainter` is `Send + Sync`,
    /// and rather than a `Mutex` because a poisoned lock in the frame path is a
    /// panic on every subsequent frame — a heavy failure mode for one `f32` that
    /// is only ever written from the pane loop. [`NO_FLOOR_DEMAND`] is the
    /// sentinel for "no pane asked for a floor this frame", which
    /// `MirrorRungs::observe` treats as "hold the rung" rather than "want rung
    /// 1"; see there for why that distinction is worth naming at all.
    floor_demand: std::sync::atomic::AtomicU32,
}

/// The bit pattern [`BridgeVolumePainter::floor_demand`] holds when no pane
/// asked for a floor.
///
/// A quiet NaN, which no real demand can collide with: `floor_magnification`
/// answers `None` rather than a NaN, and `record_floor_demand` only ever stores
/// what it returned.
const NO_FLOOR_DEMAND: u32 = u32::MAX;

impl BridgeVolumePainter {
    pub fn new(store: Arc<VolumeStore>, quality: VolumeQuality, probed: VolumeSupport) -> Self {
        Self {
            store,
            quality,
            probed,
            floor_demand: std::sync::atomic::AtomicU32::new(NO_FLOOR_DEMAND),
        }
    }

    /// The frame's floor magnification demand, clearing it for the next frame.
    ///
    /// Taken rather than read so a frame in which no pane painted — the window
    /// between a surface loss and the rebuild, say — cannot leave a stale
    /// demand standing.
    pub fn take_floor_demand(&self) -> Option<f32> {
        let bits = self
            .floor_demand
            .swap(NO_FLOOR_DEMAND, std::sync::atomic::Ordering::Relaxed);
        (bits != NO_FLOOR_DEMAND).then(|| f32::from_bits(bits))
    }

    /// Fold one pane's magnification into the frame's demand, keeping the
    /// largest.
    ///
    /// The largest, because the mirror is **one texture for the whole
    /// application**: two 3D panes on two different maps both find their ground
    /// in it, so a rung chosen for the average would leave the closer camera
    /// exactly as soft as it was.
    fn record_floor_demand(&self, magnification: f32) {
        use std::sync::atomic::Ordering::Relaxed;
        let seen = self.floor_demand.load(Relaxed);
        let folded = if seen == NO_FLOOR_DEMAND {
            magnification
        } else {
            f32::from_bits(seen).max(magnification)
        };
        self.floor_demand.store(folded.to_bits(), Relaxed);
    }
}

impl VolumePainter for BridgeVolumePainter {
    fn paint(&self, frame: &VolumeFrameState) -> VolumePaint {
        // Re-asked every frame rather than cached: `volume::support` folds in
        // the process-global latch that `install_error_latch` and the two-strike
        // surface-loss counter write, and neither of those had happened when
        // this painter was built.
        if let Some(why) = crate::volume::support(&self.probed).reason() {
            return VolumePaint::Empty(why.to_owned());
        }

        // Through the pane-scoped lookup, which is the seamless swap: while a
        // rebuild for this target is in flight, the pane's previous grid of
        // the same site, moment and region answers, so a live volume updating
        // every sealed sweep repaints rather than flashing "Building…".
        let Some(found) = self.store.lookup_for_pane(frame.pane_idx, &frame.target) else {
            // Nothing paintable at all — the very first build, or a hard
            // retarget with nothing old worth showing.
            return VolumePaint::Empty(format!(
                "Building the {} volume...",
                frame.target.product.code(),
            ));
        };
        let grid = match found.entry {
            VolumeEntry::Ready(grid) => grid,
            // Unreachable through `lookup_for_pane`, which never answers with
            // a `Building` entry — but the enum says it can, so the honest
            // fallback is the same first-build message.
            VolumeEntry::Building => {
                return VolumePaint::Empty(format!(
                    "Building the {} volume...",
                    frame.target.product.code(),
                ));
            }
            VolumeEntry::Refused(why) => return VolumePaint::Empty(why),
        };

        // On the tilt *count*, never on "the index plane is all no-data".
        //
        // A single-tilt volume does yield an empty grid, but that emptiness is
        // measure-zero rather than an invariant: a cell centre landing
        // bit-exactly on the beam's height paints, so the "all empty" test is
        // right almost always and silently wrong the rest of the time. And the
        // user is owed the reason, not an empty box.
        if grid.tilt_count() == 1 {
            return VolumePaint::Empty(
                "This volume has a single tilt, so there is no vertical structure to render. \
                 Wait for a full scan."
                    .to_owned(),
            );
        }

        // After the grid is built rather than before, deliberately: the answer
        // is a property of the table that travels *inside* the grid, and reading
        // it from a second copy of the palette would be a second copy to keep in
        // step. The build is not wasted either — the store keeps it, so
        // switching back to a moment that renders costs nothing.
        if let Some(why) = palette_refusal(&grid) {
            return VolumePaint::Empty(why);
        }

        let fitted = self.quality.fit_to_budget(frame.size_px);
        let box_size_km = box_size_km(&grid);
        let aspect = fitted.size[0] as f32 / fitted.size[1] as f32;
        let Some(view) = view_for(frame.camera, box_size_km, aspect) else {
            // Reached by a pane collapsed to nothing by a divider drag, and by a
            // grid whose box has a zero axis. Both are transient or impossible;
            // neither may hand the GPU a matrix of NaN.
            return VolumePaint::Empty("This pane is too small to draw a volume in.".to_owned());
        };

        let shape = grid.shape();
        let mut uniform = VolumeUniform::new(
            box_size_km,
            [shape.nx as u32, shape.ny as u32, shape.nz as u32],
        );
        uniform.box_from_clip = view.box_from_clip;
        uniform.eye_in_box = view.eye_in_box;
        // The stretch the pane is drawn at, for the shading's normals only —
        // `OrbitCamera` floors it at 1, which is what licenses the shader to
        // divide by it unguarded.
        uniform.vertical_exaggeration = frame.camera.vertical_exaggeration();
        // The rung this pane actually got, not the one the adapter was offered:
        // `fit_to_budget` can step the resolution down, and shading rides the
        // same struct. The smoothed reconstruction rides the same rung as the
        // lighting on purpose — together they are the cloud look, and a device
        // that cannot afford one cannot afford the other; the floor rung stays
        // the jagged-unlit raw march. The reconstruction level is per-frame
        // from this grid's own cell size: full smoothing where the data
        // outresolves the display, none where the kernel would be wider than
        // the features — see `cloud_reconstruction_lod_for` for the Harvey
        // measurement behind the taper. The isosurface branch below takes the
        // level back to 0 for itself; see the reasoning there.
        uniform.gradient_shading = fitted.quality.shading.is_on();
        if fitted.quality.shading.is_on() {
            uniform.reconstruction_lod = cloud_reconstruction_lod_for(largest_cell_km(&uniform));
            uniform.step_cells = CLOUD_STEP_CELLS;
        }
        // The march's transfer edge, anchored at the **effective** fade
        // boundary: the palette's own unless a Volume Alpha curve is applied,
        // then the curve's — [`effective_fade_band`] holds the whole decision
        // and its reasoning. Either way the band counts the fully transparent
        // indices above the no-data index, so the first visible entry is
        // `band + 1` and [`empty_index_threshold_for`] — `(band + 0.5) / 255`
        // — is exactly where a Nearest LUT fetch of the *uploaded* table
        // starts returning visible entries: below it the march can skip the
        // sample — and its up-to-seven shading fetches — without changing a
        // pixel. The ramp then dissolves the alpha cliff at that same
        // boundary over [`EDGE_SOFT_WIDTH`].
        uniform.empty_index_threshold =
            empty_index_threshold_for(effective_fade_band(grid.fade_band(), frame.alpha.as_ref()));
        uniform.edge_soft_width = EDGE_SOFT_WIDTH;

        // The view mode. In isosurface mode the two formerly-reserved lanes
        // carry the crossing parameters, translated against this grid's own
        // ramp so the surface sits exactly where the ramp puts the value.
        //
        // The skip threshold drops back to the index-0 default for tidiness
        // rather than for effect: the shader reads `transfer.y` only in the
        // lit arm, so on this branch the lane is never fetched at all. What
        // the assignment does is keep the uniform *honest* — a struct dumped
        // in a capture, or read by an instrument, says index 0 rather than
        // carrying a fade band that has no bearing on where this pane's
        // surface sits. The property it stands for is real and is enforced by
        // the shader's shape: the isosurface reads the DATA, so neither the
        // palette's fade band nor the user's Volume Alpha curve can move the
        // surface. (The sidebar says the same to the user when a curve is
        // active.)
        //
        // # Why the isosurface marches the RAW field
        //
        // The smoothed reconstruction is a *presentation* knob — that is
        // exactly [`CLOUD_RECONSTRUCTION_LOD`]'s stated contract, and it holds
        // for the lit volume, where the level widens a reconstruction kernel
        // and the march integrates whatever comes out. It does **not** hold
        // here. An isosurface is a level set of the field: smoothing the field
        // moves the surface, so on this path the knob stops being a render
        // softness and becomes a reshaping of the object being drawn.
        //
        // And it erases. `volume.wgsl`'s `COVERAGE_FLOOR` is 0.5 because 0.5
        // is the nearest-neighbour decision boundary *of the raw trilinear
        // tent* — a level-0 statement, and the only level at which it is one.
        // At level 1 the coverage field is a two-cell box convolved with that
        // tent, so a lone measured voxel reconstructs to coverage 0.125 and a
        // one-cell sheet to 0.502, and a `>= 0.5` cut deletes them. Measured
        // by `an_isosurface_at_the_shipped_rung_keeps_its_sub_kernel_features`
        // — sequential isosurface at threshold 100/255, 128 x 128 px, one
        // camera, same fixtures at both levels:
        //
        //   lone voxel:    level 0    74 px    level 1      0 px
        //   1-cell sheet:  level 0 16384 px    level 1    782 px  (-95%)
        //
        // Both shipped region rungs take level 1 from the taper above, so that
        // is a narrow hail core, a TDS shell, an updraft tip or a bright-band
        // sheet vanishing from the 3D surface while the 2D pane and the lit
        // volume both still show it — the erasure class the mip work and the
        // taper were themselves added to close. It is a regression rather than
        // a pre-existing loss: on `main` at 3b41eb64, same fixtures and camera,
        // the smoothing GREW the lone voxel (128 px at level 0 to 591 at level
        // 1) and left the sheet whole at 16384.
        //
        // Scaling the cut with the level was the alternative and is worse: any
        // cut that survives a lone voxel is at or under 0.125, which abandons
        // the nearest-neighbour reading the constant is chosen for, and it
        // would make the surface's reach a function of the *quality rung* — a
        // level set of data that moves when a device steps down. Level 0
        // instead makes `COVERAGE_FLOOR`'s contract true rather than
        // aspirational, and costs nothing in smoothness: the raw tent is
        // continuous and `refine_iso_hit` bisects to under 1/256 of a step, so
        // the surface is smooth rather than a staircase of cube faces, which
        // is the whole claim.
        //
        // What it does cost, stated rather than buried: a lone voxel draws at
        // 74 px where `main` drew 591. Most of that gap is the smoothing
        // dilating a one-cell feature across its whole coarse texel — surface
        // painted where nothing was measured, which for an *opaque* surface is
        // a claim about a core's size rather than the haze it reads as in the
        // lit volume. The remainder (74 against main's 128 at level 0) is the
        // 3-D corner clipping `COVERAGE_FLOOR` documents: a rounded nearest
        // reach instead of the old index gate's near-full cube. Both are the
        // design; erasure was not.
        //
        // `step_cells` is untouched — that is march density, not
        // reconstruction, and a finer comb only helps the crossing be found.
        if frame.view_mode == rustdar_egui::pane::VolumeViewMode::Isosurface {
            let (centre, threshold) = grid.iso_uniform_params(frame.iso_threshold);
            uniform.iso_centre = centre;
            uniform.iso_threshold = threshold;
            uniform.empty_index_threshold = empty_index_threshold_for(0);
            uniform.reconstruction_lod = 0.0;
        }

        // The floor: drawn only when the pane wants it AND the map it was
        // dragged on has told us where it is. The flag and the registration
        // travel together on purpose — a raised flag with no affine behind it
        // would sample the mirror through zeroed lanes, which paints the one
        // texel at the mirror's corner across the whole ground.
        //
        // The *rest* of the floor's uniform cannot be filled in here: it
        // depends on the frame's pixel size, which only `prepare` is told. So
        // this carries the geography and `prepare` finishes the arithmetic —
        // see `FloorSource`.
        let floor = frame.floor.then_some(frame.source).flatten().map(|geo| {
            let (site_lat, site_lon) = grid.site();
            let site_points = geo.project(site_lat, site_lon);
            FloorSource {
                site_points: [site_points.x, site_points.y],
                points_per_degree_lon: geo.points_per_degree_lon as f32,
                points_per_mercator_y: geo.points_per_mercator_y as f32,
                site_lat: site_lat as f32,
                // The box's west and south edges as kilometres east and north
                // of the site — its *position*, which `box_size_km` does not
                // carry. The shader measures its reprojection from the site,
                // so these are what turn a unit-cube coordinate into ground.
                west_km: grid.x_range_km().0 as f32,
                south_km: grid.y_range_km().0 as f32,
            }
        });
        uniform.map_floor = floor.is_some();

        // What the adaptive mirror rung is chosen from. Recorded only when a
        // floor is actually resolved, so a pane with the floor hidden — or one
        // whose source map has not said where it is — asks for no texels.
        if let Some(geo) = floor.is_some().then_some(frame.source).flatten() {
            let (site_lat, _) = grid.site();
            if let Some(magnification) = rustdar_egui::volume_view::floor_magnification(
                frame.camera,
                uniform.box_size_km,
                frame.size_px[1] as f32 / frame.pixels_per_point.max(f32::MIN_POSITIVE),
                geo.points_per_degree_lon,
                site_lat,
            ) {
                self.record_floor_demand(magnification);
            }
        }

        let callback = VolumeCallback {
            pane_idx: frame.pane_idx,
            grid_id: found.id,
            grid,
            floor,
            // The Volume Alpha curve rides to `prepare`, which owns the LUT
            // upload — the one seam the curve is applied at.
            alpha: frame.alpha.clone(),
            uniform,
            offscreen_px: fitted.size,
            live_ids: self.store.live_ids(),
        };

        VolumePaint::Callback(paint_payload(callback))
    }

    /// The grid's own colour table, for the Volume Alpha editor's palette
    /// strip and default curve — through the same pane-scoped lookup `paint`
    /// draws by, so the editor always shows the table the pane is actually
    /// rendering through, stand-in grid and all.
    fn palette(&self, pane_idx: usize, target: &VolumeTarget) -> Option<Vec<u8>> {
        match self.store.lookup_for_pane(pane_idx, target)?.entry {
            VolumeEntry::Ready(grid) => Some(grid.lut().to_vec()),
            VolumeEntry::Building | VolumeEntry::Refused(_) => None,
        }
    }
}

/// Wrap a callback in whatever `egui_wgpu` downcasts to.
///
/// `egui_wgpu::Callback`'s field is private and its only constructor hands back
/// a whole `epaint::PaintCallback`, so the payload can only be obtained by
/// building one and taking its `callback` field. The rect passed in is
/// **discarded**, and that is exact rather than approximate: `new_paint_callback`
/// stores the rect on the `PaintCallback` it returns and puts nothing but the
/// boxed trait object inside the `Arc`. `rustdar-egui` supplies the real rect
/// when it constructs its own `PaintCallback`.
///
/// Generic over the callback so the tests can exercise the wrapper without
/// a `VoxelGrid` — which has no constructor outside `build_voxels` and would
/// need a synthetic `Scan` to obtain. That `VolumeCallback` itself satisfies
/// `CallbackTrait` is proven by this function's one production call site
/// compiling; what needs a *test* is that the wrapper still produces the type
/// `egui_wgpu` downcasts to, which is exactly what would change if someone
/// simplified this to `Arc::new(callback)`.
fn paint_payload(callback: impl egui_wgpu::CallbackTrait + 'static) -> Arc<dyn Any + Send + Sync> {
    egui_wgpu::Callback::new_paint_callback(egui::Rect::ZERO, callback).callback
}

/// The floor's two uniform `vec4`s: the geography `paint` resolved, normalised
/// against the frame this `prepare` was handed.
///
/// The mirror covers the whole frame, so a position in points becomes a texture
/// coordinate by `point · pixels_per_point ÷ frame_pixels` — and the mirror's
/// *own* size does not appear, because the reduced-resolution path halves
/// `size_in_pixels` and `pixels_per_point` together and the quotient is
/// unchanged.
///
/// `gamma_encoded` is not cosmetic and cannot be decided here: `egui_wgpu` chose
/// its fragment entry point from the **swapchain's** format once, at
/// `Renderer::new`, and that same pipeline is what drew the mirror. Guessing
/// wrong gives a floor merely a little too dark or too light, with no validation
/// error and nothing to catch it but a test that looks.
///
/// A free function so it can be pinned without a `wgpu::Device` — this is where
/// a swapped axis or a lost sign would live.
fn floor_lanes(
    source: &FloorSource,
    size_in_pixels: [u32; 2],
    pixels_per_point: f32,
    gamma_encoded: bool,
) -> ([f32; 4], [f32; 4]) {
    let per_point_u = pixels_per_point / size_in_pixels[0].max(1) as f32;
    let per_point_v = pixels_per_point / size_in_pixels[1].max(1) as f32;
    (
        [
            source.site_points[0] * per_point_u,
            source.site_points[1] * per_point_v,
            source.points_per_degree_lon * per_point_u,
            source.points_per_mercator_y * per_point_v,
        ],
        [
            source.site_lat,
            source.west_km,
            source.south_km,
            if gamma_encoded { 1.0 } else { 0.0 },
        ],
    )
}

/// The box's physical extent in kilometres, along each axis.
fn box_size_km(grid: &VoxelGrid) -> [f32; 3] {
    let (x0, x1) = grid.x_range_km();
    let (y0, y1) = grid.y_range_km();
    let (z0, z1) = grid.z_range_km_msl();
    [(x1 - x0) as f32, (y1 - y0) as f32, (z1 - z0) as f32]
}

/// The grid's coarsest cell in kilometres — the axis extent over that axis'
/// cell count, maximised over the three axes. This is what
/// [`cloud_reconstruction_lod_for`] scales the smoothing by; on every shipped
/// box the horizontal axes are the coarse ones (the vertical is ~0.14 km).
/// Off the uniform rather than the grid so the value fed to the taper is
/// bit-identical to the extent and dims the same uniform hands the shader.
fn largest_cell_km(uniform: &VolumeUniform) -> f32 {
    (0..3)
        .map(|axis| uniform.box_size_km[axis] / uniform.grid_dims[axis].max(1) as f32)
        .fold(0.0f32, f32::max)
}

/// Why this moment cannot be drawn as a volume, or `None` if it can.
///
/// The solid-block regression bar, in one predicate over one measured number.
/// See the module doc for what was rendered to arrive at it. Since the
/// per-product profiles landed every samplable moment clears it; a refusal
/// here means a palette or profile change shipped a wall-to-wall opaque
/// table.
fn palette_refusal(grid: &VoxelGrid) -> Option<String> {
    palette_refusal_for(grid.see_through_indices(), grid.product().name())
}

/// [`palette_refusal`] over the two things it actually reads, so the decision is
/// testable without a `VoxelGrid` — which has no constructor outside
/// `build_voxels` and would need a synthetic `Scan` to obtain.
fn palette_refusal_for(see_through: u16, moment: &str) -> Option<String> {
    if see_through >= u16::from(MINIMUM_FADE_INDICES) {
        return None;
    }
    Some(format!(
        "{moment} cannot be drawn as a volume.\n\nIts colour table is opaque across its whole \
         scale, so every measured cell would paint at full strength and the render would be a \
         solid block, not a picture. A volume needs a see-through part of its scale - its \
         product's transparency profile is missing or has regressed.",
    ))
}

/// The wgpu side, held in egui's `CallbackResources`.
///
/// One inserted type is one slot for the **whole application** — `CallbackResources`
/// is a `TypeMap` keyed by type, not by pane or by callback — so the per-pane
/// split has to live inside this struct rather than beside it. Two 3D panes at
/// different sizes need two offscreen targets, and there is no second slot to
/// put the other one in.
pub struct VolumeResources {
    pipelines: VolumePipelines,
    /// One offscreen per pane, sized to that pane. `Option` because
    /// `VolumePipelines::ensure_offscreen` takes the slot and decides whether to
    /// reallocate, which is what keeps a pane-sized texture from being churned
    /// at the frame rate.
    targets: HashMap<usize, Option<OffscreenTarget>>,
    /// One upload per grid, keyed by the store's id. Two panes on one volume
    /// share the entry, which is the GPU half of the store's refcounting.
    uploads: HashMap<u64, VolumeUpload>,
    /// The pane mirror: one frame-sized copy of the 2D panes' own render,
    /// shared by every 3D pane.
    ///
    /// One, not one per pane, and not one per floor: it covers the whole frame
    /// rather than any box's footprint, so two 3D panes sourced from two
    /// different maps each find their ground in it by sampling a different
    /// region.
    ///
    /// `None` on every frame that has nothing to mirror, not merely until the
    /// first frame that does: the frame path calls `release_mirror` whenever
    /// its guest list is empty, so closing the last 3D pane gives the whole
    /// texture back rather than holding up to 64 MiB on desktop — 16 MiB on web
    /// and mobile — for the session. A machine
    /// that never opens a 3D pane never leaves `None` and pays nothing; one that
    /// opens and closes it returns there.
    mirror: Option<crate::volume::raymarch::PaneMirror>,
}

/// One grid's GPU upload, and which Volume Alpha curve its colour table was
/// written through.
///
/// The curve is the staleness key for the 1 KiB LUT alone: the grid beside it
/// never changes for a given store id, so an edit rewrites the table in place
/// (`VolumeTextures::write_lut`) instead of re-uploading 16 MiB of texels.
/// Compared every frame, rewritten only on change — `AlphaCurve`'s equality
/// takes the `Arc` pointer fast path, so the steady-state cost of an open
/// editor is one pointer comparison per pane per frame.
struct VolumeUpload {
    textures: VolumeTextures,
    /// The curve the uploaded table reflects — `None` for the grid's own
    /// palette, which is the bit-exact untouched-editor state.
    applied_alpha: Option<AlphaCurve>,
}

impl VolumeResources {
    /// Build the pipelines for the pass egui draws into.
    pub fn new(
        device: &wgpu::Device,
        egui_attachments: AttachmentConfig,
        queue: &wgpu::Queue,
    ) -> Self {
        let pipelines = VolumePipelines::new(device, egui_attachments);
        pipelines.upload_quad(queue);
        Self {
            pipelines,
            targets: HashMap::new(),
            uploads: HashMap::new(),
            mirror: None,
        }
    }

    /// Free everything `pane_idx` was the only user of.
    ///
    /// This is what makes `GuiAction::ReleaseVolume` actually give memory back:
    /// a pane-sized `Rgba8Unorm` target (~3 MiB at 900²) and, when the last pane
    /// on a volume lets go, the 3D texture and its table. Dropping the handles
    /// is the free — wgpu reference-counts them and the allocation goes when the
    /// last reference does. The floor uploads are pruned on the next frame's
    /// `prepare` against the store's own floor ids, which the release has
    /// already shrunk.
    ///
    /// The **mirror is not freed here**, and deliberately: it is one texture for
    /// the whole application rather than a per-pane resource, so which pane just
    /// let go says nothing about whether anyone still wants it.
    /// [`Self::release_mirror`] is the answer to that question, and the frame
    /// path asks it every frame.
    pub fn release_pane(&mut self, pane_idx: usize, live_ids: &[u64]) {
        self.targets.remove(&pane_idx);
        self.uploads.retain(|id, _| live_ids.contains(id));
    }

    /// Give the pane mirror back, for a frame on which nothing wants a floor.
    ///
    /// The mirror is up to 64 MiB on desktop, 16 MiB on web and mobile
    /// (`constants::VOLUME_MIRROR_BYTES_MAX`), and it
    /// is *not* per-pane, so nothing in [`Self::release_pane`] can decide its
    /// fate: closing the last 3D pane frees that pane's target and, without
    /// this, leaves the frame-sized mirror live for the rest of the session.
    /// The frame path calls this on exactly the frames it does not call
    /// [`Self::ensure_mirror`] on — which is the same predicate, evaluated
    /// once, in the one place that already knows the answer.
    ///
    /// Idempotent, and free when there is nothing to free: the steady state for
    /// a machine with no 3D pane is a `None` being set to `None`.
    pub fn release_mirror(&mut self) {
        self.mirror = None;
    }

    /// The mirror this frame's pass should draw into, sized to the frame and
    /// created or resized if it has to be.
    ///
    /// Hands back a **clone** of the view rather than a borrow, and that is
    /// structural rather than lazy: the mirror pass runs inside
    /// `EguiRenderer::end_pass_and_upload`, which needs `&mut` on the very
    /// renderer this lives inside. `wgpu::TextureView` is a refcounted handle,
    /// so the clone is a bump.
    ///
    /// `format` must have the same sRGB-ness as the swapchain — see
    /// [`VolumePipelines::ensure_mirror`] for what goes wrong when it does
    /// not, and note that nothing validates it.
    ///
    /// The counterpart is [`Self::release_mirror`], which the frame path calls
    /// on the frames this one is *not* called on.
    pub fn ensure_mirror(
        &mut self,
        device: &wgpu::Device,
        size: [u32; 2],
        format: wgpu::TextureFormat,
    ) -> wgpu::TextureView {
        self.pipelines
            .ensure_mirror(device, &mut self.mirror, size, format);
        // Cannot be `None`: `ensure_mirror` either kept a mirror or made one.
        // Answered rather than unwrapped because a panic here would be on the
        // frame path, where on wasm it aborts the whole application.
        self.mirror
            .as_ref()
            .map(|mirror| mirror.view().clone())
            .unwrap_or_else(|| {
                // Unreachable; a fresh 1×1 view is a cheaper failure than a
                // dead application, and the pass that draws into it is
                // harmless.
                device
                    .create_texture(&wgpu::TextureDescriptor {
                        label: Some("volume.mirror.fallback"),
                        size: wgpu::Extent3d {
                            width: 1,
                            height: 1,
                            depth_or_array_layers: 1,
                        },
                        mip_level_count: 1,
                        sample_count: 1,
                        dimension: wgpu::TextureDimension::D2,
                        format,
                        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                        view_formats: &[],
                    })
                    .create_view(&wgpu::TextureViewDescriptor::default())
            })
    }
}

/// One 3D pane's draw, for one frame.
///
/// Carries the grid rather than a handle to it because the upload may not have
/// happened yet: `prepare` is the first place a `wgpu::Device` exists, so the
/// bytes have to travel this far. The `Arc` makes that a refcount bump.
struct VolumeCallback {
    pane_idx: usize,
    grid_id: u64,
    grid: Arc<VoxelGrid>,
    /// Where this box's ground is inside the pane mirror, when the pane wants
    /// a floor and its source map has said where it is. `uniform.map_floor` is
    /// true exactly when this is `Some`.
    floor: Option<FloorSource>,
    /// The Volume Alpha curve the LUT must be uploaded through, or `None` for
    /// the grid's own table, bit-exactly. `prepare` compares this against
    /// what the upload cache holds and rewrites the 1 KiB table only on
    /// change — never per unchanged frame.
    alpha: Option<AlphaCurve>,
    uniform: VolumeUniform,
    offscreen_px: [u32; 2],
    /// Every grid the store still holds, so `prepare` can free the uploads for
    /// the ones it does not. Carried on the callback rather than read from the
    /// store because `prepare` runs with no access to anything but its
    /// arguments.
    live_ids: Vec<u64>,
}

/// Everything the floor's uniform lanes need that does not depend on the
/// frame's pixel size.
///
/// The split is not arbitrary. The mirror covers the whole frame, so a point
/// on the frame becomes a texture coordinate by `point · pixels_per_point ÷
/// frame_pixels` — and `paint` is told neither of those two numbers, while
/// `prepare` is handed both on its `ScreenDescriptor`. So the geography is
/// resolved where the map is known and the normalisation where the frame is,
/// and neither half guesses the other's numbers.
///
/// Note what cancels: the mirror's *own* size does not appear. A mirror
/// rendered at half resolution is half as many pixels over the same frame, so
/// the quotient is unchanged — which is exactly why the reduced-resolution
/// path halves `size_in_pixels` and `pixels_per_point` together.
#[derive(Clone, Copy, Debug, PartialEq)]
struct FloorSource {
    /// Where the radar site lands on the frame, in points.
    site_points: [f32; 2],
    /// Points of frame x per degree of longitude east.
    points_per_degree_lon: f32,
    /// Points of frame y per unit of Mercator y. Negative.
    points_per_mercator_y: f32,
    /// The site's latitude, degrees north — the origin the shader's
    /// reprojection measures from.
    site_lat: f32,
    /// The box's west edge, km east of the site.
    west_km: f32,
    /// The box's south edge, km north of the site.
    south_km: f32,
}

impl egui_wgpu::CallbackTrait for VolumeCallback {
    fn prepare(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        screen_descriptor: &egui_wgpu::ScreenDescriptor,
        egui_encoder: &mut wgpu::CommandEncoder,
        callback_resources: &mut egui_wgpu::CallbackResources,
    ) -> Vec<wgpu::CommandBuffer> {
        let Some(resources) = callback_resources.get_mut::<VolumeResources>() else {
            // The renderer was built without volume support, or the resources
            // were never inserted. Logged rather than silent because this is the
            // one wiring mistake that produces an ordinary-looking empty pane.
            log::warn!("3D volume view: no VolumeResources in the callback map; nothing to draw");
            return Vec::new();
        };
        // Destructured so the borrow checker can see that the pipelines are read
        // while the two maps are written.
        let VolumeResources {
            pipelines,
            targets,
            uploads,
            mirror,
        } = resources;

        uploads.retain(|id, _| self.live_ids.contains(id));

        let slot = targets.entry(self.pane_idx).or_default();
        pipelines.ensure_offscreen(device, slot, self.offscreen_px);
        let Some(target) = slot.as_ref() else {
            return Vec::new();
        };

        // Through the entry API rather than `contains_key` + `insert`, which is
        // one hash lookup instead of two — and the upload is refusable, so this
        // is a `match` on the entry rather than `or_insert_with`.
        let upload = match uploads.entry(self.grid_id) {
            std::collections::hash_map::Entry::Occupied(occupied) => {
                let upload = occupied.into_mut();
                // The Volume Alpha seam's steady state: rewrite the 1 KiB
                // table only when the curve actually changed — a pointer
                // comparison almost every frame — and leave the 16 MiB grid
                // untouched always. `effective_lut` with `None` is the grid's
                // own bytes, so clearing a curve restores the palette
                // bit-exactly through the very same path that applied it.
                if upload.applied_alpha != self.alpha {
                    upload
                        .textures
                        .write_lut(queue, &effective_lut(self.grid.lut(), self.alpha.as_ref()));
                    upload.applied_alpha = self.alpha.clone();
                }
                upload
            }
            std::collections::hash_map::Entry::Vacant(vacant) => {
                let shape = self.grid.shape();
                let Some(textures) = pipelines.upload_volume(
                    device,
                    queue,
                    [shape.nx as u32, shape.ny as u32, shape.nz as u32],
                    self.grid.indices(),
                    // The grid's own table, through the one seam a user curve
                    // may rewrite its alpha at — `effective_lut` borrows the
                    // grid's bytes untouched when there is no curve. See the
                    // module doc.
                    &effective_lut(self.grid.lut(), self.alpha.as_ref()),
                ) else {
                    // `upload_volume` has already logged which invariant it
                    // refused on. Nothing to add, and nothing to draw.
                    return Vec::new();
                };
                vacant.insert(VolumeUpload {
                    textures,
                    applied_alpha: self.alpha.clone(),
                })
            }
        };
        let textures = &upload.textures;

        // The floor. Nothing is uploaded here — the mirror is a render target
        // the frame path drew into before this ran, so all that is left is to
        // finish the uniform's two floor lanes against the frame this
        // `prepare` was actually given.
        //
        // Both halves must be present or neither is used: a raised `map_floor`
        // over the placeholder mirror composites a transparent ground, which
        // draws nothing while claiming to.
        let mut uniform = self.uniform;
        let floor_texture = match (self.floor.as_ref(), mirror.as_ref()) {
            (Some(source), Some(mirror)) => {
                let (uv, geo) = floor_lanes(
                    source,
                    screen_descriptor.size_in_pixels,
                    screen_descriptor.pixels_per_point,
                    mirror.is_gamma_encoded(),
                );
                uniform.floor_uv = uv;
                uniform.floor_geo = geo;
                Some(mirror)
            }
            _ => {
                uniform.map_floor = false;
                None
            }
        };

        textures.write_uniform(queue, &uniform);
        // Into egui's own encoder, which egui submits before its own commands —
        // so the offscreen is written before the blit reads it. The other order
        // paints the previous frame's volume, which reads as input lag.
        pipelines.encode_raymarch_with_floor(egui_encoder, target, textures, floor_texture);

        Vec::new()
    }

    fn paint(
        &self,
        info: egui::PaintCallbackInfo,
        render_pass: &mut wgpu::RenderPass<'static>,
        callback_resources: &egui_wgpu::CallbackResources,
    ) {
        let Some(resources) = callback_resources.get::<VolumeResources>() else {
            return;
        };
        let Some(Some(target)) = resources.targets.get(&self.pane_idx) else {
            return;
        };
        // Nothing was uploaded, so the offscreen holds whatever the last draw
        // left. Better an empty pane than another pane's volume.
        if !resources.uploads.contains_key(&self.grid_id) {
            return;
        }

        let viewport = info.viewport_in_pixels();
        if viewport.width_px <= 0 || viewport.height_px <= 0 {
            return;
        }
        // The quad covers all of clip space, so the viewport is what places it
        // over the pane. egui re-binds pipeline, scissor and viewport after
        // every callback, so nothing here has to be put back.
        render_pass.set_viewport(
            viewport.left_px as f32,
            viewport.top_px as f32,
            viewport.width_px as f32,
            viewport.height_px as f32,
            0.0,
            1.0,
        );
        resources.pipelines.paint_blit(render_pass, target);
    }
}

#[path = "volume_bridge/tests.rs"]
#[cfg(test)]
mod tests;
