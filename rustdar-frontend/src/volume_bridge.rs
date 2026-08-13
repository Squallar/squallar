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
use crate::volume::raymarch::staging::VolumeStaging;
use crate::volume::raymarch::{CoarseLevel, OffscreenTarget, VolumePipelines, VolumeTextures};
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

/// Whether a grid uploaded on this device, at this cell size, will ever have
/// its coarse mip level sampled — and so whether the upload should allocate
/// one. See [`CoarseLevel`] for the cost, and the cross-reference behind the
/// answer.
///
/// Both arguments are the *same two numbers* the paint path above writes into
/// `gradient_shading` and feeds [`cloud_reconstruction_lod_for`], and this is
/// deliberately expressed by calling that function rather than by restating
/// its knee: the level is uploaded exactly when the level would be read, and
/// a taper that moved without this moving with it would either waste the
/// memory again or — much worse — leave the smoothing sampling a level that is
/// not there.
///
/// It cannot consult the view mode. Isosurface takes the reconstruction level
/// to 0 per frame, but the upload is cached across frames and a pane can be
/// switched back to the lit volume without rebuilding its grid; an upload that
/// dropped the level for a mode would leave the volume unsmoothed on the way
/// back. Skipping this is only correct for things that cannot change under the
/// cached upload, and the mode is not one.
fn coarse_level_for(gradient_shading: bool, largest_cell_km: f32) -> CoarseLevel {
    if gradient_shading && cloud_reconstruction_lod_for(largest_cell_km) > 0.0 {
        CoarseLevel::Built
    } else {
        CoarseLevel::Omitted
    }
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
    /// [`crate::volume::raymarch::resident_grid_bytes`] — what the upload
    /// path's descriptors actually reserve — rather than from a
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
        crate::volume::raymarch::resident_grid_bytes(cells).unwrap_or(0)
    }
}

/// What the store holds for one target, with the id its GPU upload is keyed by.
pub struct VolumeLookup {
    pub id: u64,
    pub entry: VolumeEntry,
    /// This is not the target's own entry — it is a grid the pane was already
    /// holding, standing in while the build for `target` is in flight.
    ///
    /// Told rather than inferred. The caller could compare the grid's box
    /// against the target's and guess, and it would be right almost always and
    /// silently wrong for a target whose box the caller cannot compute (a
    /// `None` region, whose width is the volume's own reach and needs the
    /// scan). A stand-in that a caller mistook for the real answer would be
    /// drawn in the wrong box under a caption claiming it was the right one,
    /// which is the one lie the swap must never tell — so the store, which
    /// knows, says.
    pub stood_in: bool,
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
            // wait", which is right for one grid and destroys thirteen. What
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
    /// one grid, and for thirteen it is a peak of two full sets at once (936
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

    /// Every pane index this store is still holding something for that the
    /// layout has stopped showing — at or past `visible_panes` — in ascending
    /// order.
    ///
    /// # The path this closes
    ///
    /// Splitting to fewer panes does not convert the panes it hides: they keep
    /// their `PaneState` so a re-split remembers them, and `ReleaseVolume`
    /// fires only on a *kind* change. So a 3D pane hidden mid-build kept its
    /// entry — `Building` through resolved grid — and nothing ever came back
    /// to ask. At 36 MiB of GPU texture and ~8 MiB of host bytes per resolved
    /// grid, and up to five hideable panes on desktop, that is the store
    /// spending its whole enforced budget on panes nobody can see: what
    /// [`Self::enforce_budget`] then evicts, oldest first, is a *live* 3D
    /// loop's frames.
    ///
    /// # Why an index test and not a pane test
    ///
    /// This deliberately asks nothing about the `PaneState`. A pane that is
    /// merely **not currently drawn** is not a pane that is **gone**: panes are
    /// `mem::take`n during the UI pass and the taken slot reads as a default
    /// `PaneState` — a `Map` with an empty site — so any predicate that asked
    /// "is pane *i* still a 3D pane?" from inside that window would release a
    /// live pane's grid on the frame its own settings panel was open. The
    /// layout's count is a fact about the *layout* and is true at every point
    /// in the frame; the caller's job is to evaluate it at one where the
    /// vector is whole. `App::release_hidden_pane_volumes` is that caller and
    /// says where.
    ///
    /// Set holders are named too, and by the same index test, because they are
    /// the ones with nothing else to bound them: a hidden pane is one
    /// `dispatch_loop_renders` never walks, so its
    /// [`Self::retain_set`] is never restated and its whole resident set —
    /// thirteen grids, 468 MiB on desktop — outlives the layout that asked for
    /// it. A holder marked as a set holder but holding no entry is named as
    /// well, so that [`Self::release`] can un-mark it: coming back as a
    /// *single* holder while still on that list would exempt it from every
    /// shed there is.
    ///
    /// Naming only what is actually held is what makes the sweep
    /// edge-triggered rather than per-frame work: [`Self::release`] detaches
    /// the pane and unmarks it, so the next pass answers with an empty vector.
    pub fn hidden_holders(&self, visible_panes: usize) -> Vec<usize> {
        let inner = self.lock();
        let mut hidden: Vec<usize> = inner
            .entries
            .iter()
            .flat_map(|e| e.panes.iter().copied())
            .chain(inner.set_holders.iter().copied())
            .filter(|&pane| pane >= visible_panes)
            .collect();
        hidden.sort_unstable();
        hidden.dedup();
        hidden
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
    /// counts `LoopPool::plan` derives are chosen to fit — and the case it
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
                stood_in: false,
            })
    }

    /// What pane `pane_idx` should paint for `target`: the target's own entry
    /// when it is resolved, else the newest same-scope grid the pane still
    /// holds — the old picture, painted through a rebuild, and flagged
    /// [`VolumeLookup::stood_in`] so the caller draws it in the box that was
    /// asked for rather than in the one it was built over.
    ///
    /// `None` while nothing is paintable at all, which the painter renders as
    /// the first-build message. The fallback is scoped to the same site and
    /// product on purpose: after a site or product switch the old grid answers
    /// a question nobody is asking, and painting it with a caption describing
    /// the new target would be the lie the swap must never tell. Newest-first
    /// because a pane can transiently hold two resolved grids and the later
    /// build is the newer picture.
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
                stood_in: false,
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
                stood_in: true,
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
    /// keeps a grid a pane no longer needs.
    ///
    /// Two such paths have been disclosed here, and both are closed:
    ///
    /// * Reducing the pane count hides a 3D pane without converting it, and
    ///   `ReleaseVolume` fires only on a *kind* change.
    ///   [`Self::hidden_holders`] names those panes and
    ///   `App::release_hidden_pane_volumes` releases them, once per frame,
    ///   outside every `mem::take` window.
    /// * Switching radar site leaves the pane on a site with no published
    ///   stamp, and `ui_map`'s volume arm returns its empty state *before* it
    ///   emits `PrepareVolume` or paints — so nothing reached [`Self::share`],
    ///   nothing reached [`StoreInner::shed`], and the radar the pane just left
    ///   stayed attached to it until the *new* site's first volume was
    ///   extracted, which on a site with no data is never.
    ///   `GuiAction::SwitchRadarSite` now calls `App::handle_release_volume`
    ///   for every pane that really changed radar, on the switch itself.
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

/// Whether an entry built for `a` may stand in for `b` — the same radar and
/// the same moment, at another data time **or over another patch of ground**.
/// The seamless swap is licensed exactly this far, and nothing else may.
///
/// # Why the region is not part of it
///
/// It was, back when the region was derived from the pane's viewport, and that
/// is what made zooming take the view away: every frame of a scroll named a new
/// target, and a target scoped away from the grid in hand blanked the pane to
/// "Building…" — on a gesture the user expects to be continuous.
///
/// The gap was never *one build*. Because every gesture frame named a new box,
/// no build in flight ever answered what the pane was currently asking for, so
/// the blank lasted the whole gesture **plus** a build — measured at 12 frames
/// over a 200 ms scroll on this machine, and it grew with how long the user kept
/// scrolling rather than with what a resample cost. That is why making the build
/// faster was never the fix.
///
/// A stored region cannot do that any more: a gesture does not write it, so a
/// scroll names one target from beginning to end. What is left for this to
/// license is the case it was always really about — the **stamp** advancing on
/// every sealed sweep, thirteen times a volume, over a region that has not
/// moved. Excluding the region is still right for that, and it is now right for
/// a reason that does not depend on a gesture: two targets differing only in
/// their region differ by a *crop*, and a crop is an affine on a grid rather
/// than a different question.
///
/// The reason it was excluded is real and is answered rather than dropped: a
/// grid over one patch of ground painted under a caption naming another would
/// be a lie. So the picture is not painted where the grid is — it is painted
/// in the box the pane asked for, with the held grid fetched through an affine
/// ([`DrawnBox`]), and the caption says what resolution that picture actually
/// has ([`rustdar_egui::volume_view::Showing`]). What must still never stand in
/// is another *radar* or another *product*: no transform can make one of those
/// into an answer to the question being asked, which is why those two stay.
fn same_scope(a: &VolumeTarget, b: &VolumeTarget) -> bool {
    a.volume.site == b.volume.site && a.product == b.product
}

/// The painter a `Gui` is handed. Turns a pane's frame state into a payload
/// `egui_wgpu` can draw, or into a sentence saying why it cannot.
pub struct BridgeVolumePainter {
    store: Arc<VolumeStore>,
    /// The quality this adapter was classified into, from
    /// `AdapterInfo::device_type`. Fixed for the life of the renderer: a device
    /// does not change class, and the thing that *does* change per frame — the
    /// pane's size — is applied by `VolumeQuality::fit` below.
    quality: VolumeQuality,
    /// The resolved `Budgets::offscreen_bytes` this renderer fits every pane's
    /// raymarch target into. Handed in rather than read from a `cfg` constant,
    /// for the reason `quality` is: a budget read inline is a budget checkable
    /// on the one arm the test runner compiled.
    offscreen_bytes: usize,
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
    /// and the pane's own map affine are all in hand together.
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
    pub fn new(
        store: Arc<VolumeStore>,
        quality: VolumeQuality,
        offscreen_bytes: usize,
        probed: VolumeSupport,
    ) -> Self {
        Self {
            store,
            quality,
            offscreen_bytes,
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
        // the same site and product answers, so a live volume updating every
        // sealed sweep — or a box the user has just zoomed — repaints rather
        // than flashing "Building…".
        let Some(found) = self.store.lookup_for_pane(frame.pane_idx, &frame.target) else {
            // Nothing paintable at all — the very first build, or a hard
            // retarget with nothing old worth showing.
            return VolumePaint::Empty(format!(
                "Building the {} volume...",
                frame.target.product.code(),
            ));
        };
        let grid = match &found.entry {
            VolumeEntry::Ready(grid) => Arc::clone(grid),
            // Unreachable through `lookup_for_pane`, which never answers with
            // a `Building` entry — but the enum says it can, so the honest
            // fallback is the same first-build message.
            VolumeEntry::Building => {
                return VolumePaint::Empty(format!(
                    "Building the {} volume...",
                    frame.target.product.code(),
                ));
            }
            VolumeEntry::Refused(why) => return VolumePaint::Empty(why.clone()),
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

        let fitted = self.quality.fit(frame.size_px, self.offscreen_bytes);
        // The box the pane asked for, which is the grid's own whenever the
        // build for it has landed. While it has not, this is what makes the
        // zoom immediate: the camera frames the new box and the floor is
        // registered to it on the very frame the wheel turned, and the held
        // grid is fetched through an affine rather than blanked. See
        // `DrawnBox`.
        let Some(drawn) = DrawnBox::for_lookup(&found, &frame.target, &grid) else {
            // A stand-in whose target's box cannot be placed — see
            // `DrawnBox::for_target`. Blank rather than draw a picture over
            // ground it is not over.
            return VolumePaint::Empty(format!(
                "Building the {} volume...",
                frame.target.product.code(),
            ));
        };
        let box_size_km = drawn.size_km();
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
        // Where the drawn box's unit cube sits in the grid. The identity while
        // nothing is pending, which is a multiply by one and an add of zero.
        uniform.grid_from_box_scale = drawn.scale;
        uniform.grid_from_box_offset = drawn.offset;
        uniform.grid_bounded = drawn.bounded;
        // The stretch the pane is drawn at, for the shading's normals only —
        // `OrbitCamera` floors it at 1, which is what licenses the shader to
        // divide by it unguarded.
        uniform.vertical_exaggeration = frame.camera.vertical_exaggeration();
        // The rung this pane actually got, not the one the adapter was offered:
        // The fit can step the resolution down, and shading rides the
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
        // — `#[ignore]`d, it needs a real adapter, so run it with
        // `cargo test -p rustdar-frontend --test volume_gpu -- --ignored`.
        // Sequential isosurface at threshold 100/255, 128 x 128 px, one
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
                //
                // The **drawn** box, not the grid's: the mirror is the pane's
                // own map at the zoom the user has reached this frame, and the
                // floor covers the drawn box by construction only if it is
                // registered to the same box the camera is framing. Registered
                // to a held grid's older, wider box instead, everything past
                // the viewport's edge would fall outside the mirror and the
                // ground would visibly eat itself inwards as the user scrolled.
                west_km: drawn.x_km.0 as f32,
                south_km: drawn.y_km.0 as f32,
                mirror_size_points: frame.mirror_size_points,
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

        // What the caption is allowed to claim. The box it names is the drawn
        // one and is therefore true either way; what it must not do is report
        // the *requested* box's cell size while a coarser grid is on screen.
        let showing = rustdar_egui::volume_view::Showing {
            cell_km: cell_km(&grid),
            stale: found.stood_in,
            partial: drawn.bounded,
        };

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

        VolumePaint::Callback {
            payload: paint_payload(callback),
            showing,
        }
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

    /// Through the same pane-scoped lookup [`Self::paint`] uses, and through
    /// the same [`DrawnBox`] it hands the uniform — so the box the pan gesture
    /// is scaled against, the box the caption names and the box the shader
    /// marches are one derivation, not three that agree by inspection.
    ///
    /// The **drawn** box and not the held grid's, which is the whole point
    /// while a stand-in is up: a pane that has just been zoomed is framing the
    /// box it asked for, so a pan scaled to the older, wider grid would run at
    /// the wrong speed for exactly as long as the picture was soft, and the
    /// caption would name a box nobody is looking at.
    fn box_size_km(&self, pane_idx: usize, target: &VolumeTarget) -> Option<[f32; 3]> {
        let found = self.store.lookup_for_pane(pane_idx, target)?;
        let VolumeEntry::Ready(grid) = &found.entry else {
            return None;
        };
        Some(DrawnBox::for_lookup(&found, target, grid)?.size_km())
    }

    /// The **held grid's** horizontal cell count, not the drawn box's.
    ///
    /// The opposite choice from [`Self::box_size_km`] directly above, and for
    /// the same reason that one takes the drawn box: the caption divides this
    /// into that, and what the reader is being told is the resolution of the
    /// picture actually on screen. While a stand-in is up, the box is the one
    /// the pane asked for and the cells are the ones the older grid has, so
    /// pairing the drawn box with the held grid's count is what makes the
    /// printed km-per-cell true of the pixels rather than of a grid that has
    /// not been built yet.
    fn grid_cells_across(&self, pane_idx: usize, target: &VolumeTarget) -> Option<usize> {
        let found = self.store.lookup_for_pane(pane_idx, target)?;
        let VolumeEntry::Ready(grid) = &found.entry else {
            return None;
        };
        Some(grid.shape().nx)
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
/// against the mirror it will be sampled from.
///
/// A position in points becomes a texture coordinate by `point ÷
/// mirror_size_points` — the mirror's extent in **points**, which is the frame
/// plus however far below it this frame's off-screen map strips reach
/// (`rustdar_egui::Gui::mirror_size_points`).
///
/// Points rather than texels, and the mirror's rather than the frame's. The
/// first half is why the adaptive rung cannot move registration: scaling the
/// mirror halves or doubles `size_in_pixels` and `pixels_per_point` together
/// and leaves this quotient alone. The second half is what the off-screen strip
/// changed — the mirror used to *be* the frame, so the frame's own
/// `ScreenDescriptor` answered this correctly by coincidence, and now it would
/// stretch every floor vertically by the ratio between the two.
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
    mirror_size_points: [f32; 2],
    gamma_encoded: bool,
) -> ([f32; 4], [f32; 4]) {
    let per_point_u = 1.0 / mirror_size_points[0].max(f32::MIN_POSITIVE);
    let per_point_v = 1.0 / mirror_size_points[1].max(f32::MIN_POSITIVE);
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

/// The box a pane is drawing, and where that box's unit cube sits inside the
/// grid being drawn through it.
///
/// # Why the two can differ at all
///
/// The box is the pane's own viewport, so a scroll retargets it, and the grid
/// for the new box is a resample and a GPU upload away (measured together at
/// ~37 ms here; see `same_scope` for why the number is not what mattered).
/// Through that wait the pane keeps the grid it already has — that is the
/// store's stand-in, and it is what stops a zoom taking the view away — but it
/// draws it **in the box the user just asked for**, not in the box the grid
/// was built for. Everything geographic in the uniform is therefore the requested
/// box: the camera frames it (so the zoom is immediate), the floor is
/// registered to it (so it still covers the box exactly, which is the whole
/// point of the region being the viewport), and only where the field is
/// fetched from is old. [`Self::scale`] and [`Self::offset`] are that fetch.
///
/// When nothing is pending the two boxes are the same object computed from the
/// same inputs by the same function — see
/// [`rustdar_radar::voxel::horizontal_ranges_km`] — so they are bit-equal and
/// the affine is exactly the identity.
///
/// # What the handover looks like, measured — nothing moves
///
/// A user reported the "bands" of the voxels *shifting* when the fresh grid
/// replaces the held one. They do not, and this is the paragraph to read before
/// trying to stop them.
///
/// Measured on a real KTLX volume at a 100 km → 25 km zoom, rendering the
/// cropped held grid and the freshly built one at the identical camera and box:
/// the best whole-pixel alignment between the two frames is **(0, 0)** on both
/// a luma cross-correlation and a mask IoU, the correlation peak is sharp and
/// centred, and the alpha centroid moves **0.15 px**. The instrument can see a
/// translation when there is one — a control that shifts the box by exactly one
/// cell registers at exactly (+1, 0) — so this is a measurement and not a
/// failure to detect.
///
/// What actually happens is that **83.8% of painted pixels re-colour by more
/// than 8/255 while the picture stays registered**. Band *boundaries* relocate
/// by a fraction of a pixel across a palette that is sampled `Nearest`, and a
/// contour crossing a colour step is what reads as motion. The amplifiers, by
/// measured size: the stepped palette is worth 5.1× (33.98 against 6.18 on a
/// smooth ramp), and the march's own sample comb about 47% (33.98 against 18.17
/// with both sides marched at a quarter cell). Neither is a property of the
/// stand-in.
///
/// **And neither is the artefact.** Move the settled box by half a fine cell —
/// 98 metres — and rebuild, with no stand-in anywhere in the frame, and the
/// picture changes by 28.090 of those same units: 83% of the whole handover
/// discontinuity. This is what a resampled field does when its box moves at
/// all. The crop did not create it; it made it *visible*, because before the
/// crop the pane blanked between the two pictures and nobody ever saw them back
/// to back.
///
/// Two repairs were tried against it and both are recorded where someone would
/// reach for them: correcting the cell metric (`VolumeUniform::grid_dims`)
/// makes it worse in every configuration, and anchoring the resample lattice on
/// the site ([`rustdar_radar::voxel::horizontal_ranges_km`]) buys 1.5%.
///
/// # Zooming out: real data in the middle and nothing outside it
///
/// Zooming in, the requested box is inside the held grid and the picture is
/// simply softer until the build lands. Zooming out it is not, and the ring
/// beyond the grid has to be *something*. It is nothing — transparent, with the
/// floor showing through it and the caption saying so ("over the middle,
/// filling in"). The alternative considered and rejected was a resident coarse
/// whole-volume grid per 3D pane, kept as a permanent backdrop.
///
/// **It does not fit, and it is not the memory that decides it.** The bytes are
/// real: a whole-volume grid reserves 36.6 MiB of GPU texture on desktop (the
/// desktop cell budget at four bytes a cell, and then the whole mip pyramid a
/// coarse level buys — see `volume::raymarch::grid_bytes_at`), 15.5 MiB on
/// mobile and 4.58 MiB on wasm. At `MAX_PANES_DESKTOP` = 6 that is 219 MiB of
/// backdrop — 43% of [`crate::constants::LOOP_POOL_FLOOR_BYTES`]
/// (512 MiB desktop), which is also `VOLUME_LOOP_TEXTURE_BUDGET_BYTES`, so it
/// comes straight out of what a 3D loop may hold: six of the thirteen grids
/// that floor buys, gone. On wasm the floor is 48 MiB and four panes of
/// backdrop are 18 MiB of it, 37.5%. Against
/// [`crate::constants::APP_TEXTURE_BUDGET_BYTES`] alone (3840 MiB desktop, 256
/// MiB wasm) it would look affordable, and that is exactly the reading to
/// distrust: `enforce_budget` evicts oldest-first, so what a permanent
/// per-pane resident actually displaces is a *live* loop's frames.
///
/// Two things rule it out before the bytes do:
///
/// * **It would cost the user the sharp grid.** The backdrop is a build, and
///   builds go through one budget. `MAX_CONCURRENT_RENDERS` is 1 on wasm, so a
///   backdrop build takes the slot the build the user is actually waiting for
///   needs — the backdrop would lengthen the very wait it exists to cover.
/// * **It would put two times in one box.** The volume is live and rebuilds
///   every sealed sweep. A backdrop refreshed on the same cadence costs a
///   second build per pane per sweep for ever; one that is not is a picture of
///   an older sweep painted beside a picture of the current one, in the same
///   box, with no way for a caption to say which is which *per pixel*. The
///   whole licence for the stand-in is that the caption can describe the
///   picture; a two-time composite is the one picture it cannot.
///
/// And the payoff was small in the first place. The blank the stand-in
/// removed lasted the whole gesture plus a build — 12 frames over a 200 ms
/// scroll, measured. What the backdrop would cover is the ring outside the
/// held grid, on an outward zoom only, for the one build the user has left to
/// wait: measured at ~37 ms here. Transparent ground under a caption that says
/// the rest is filling in is an honest answer to that, and a free one.
#[derive(Clone, Copy, Debug, PartialEq)]
struct DrawnBox {
    x_km: (f64, f64),
    y_km: (f64, f64),
    z_km_msl: (f64, f64),
    /// Grid texture coordinate from unit-cube position: `t = scale · p +
    /// offset`, per axis.
    scale: [f32; 3],
    offset: [f32; 3],
    /// The box reaches outside the grid on some axis, so the march has to
    /// answer air out there rather than let the sampler clamp.
    bounded: bool,
}

impl DrawnBox {
    fn size_km(&self) -> [f32; 3] {
        [
            (self.x_km.1 - self.x_km.0) as f32,
            (self.y_km.1 - self.y_km.0) as f32,
            (self.z_km_msl.1 - self.z_km_msl.0) as f32,
        ]
    }

    /// The grid drawn in its own box — the settled case, and the one every
    /// mask instrument in this repository measures.
    fn settled(grid: &VoxelGrid) -> Self {
        Self {
            x_km: grid.x_range_km(),
            y_km: grid.y_range_km(),
            z_km_msl: grid.z_range_km_msl(),
            scale: crate::volume::uniform::IDENTITY_GRID_FROM_BOX.0,
            offset: crate::volume::uniform::IDENTITY_GRID_FROM_BOX.1,
            bounded: false,
        }
    }

    /// The box `target` asks for, drawn through `grid`.
    ///
    /// The vertical is the grid's own and not a third thing: the request's
    /// `base_km_msl`/`top_km_msl` are two constants that a region cannot
    /// touch (`voxel_request_for` says so in as many words), so the grid's
    /// vertical *is* the requested one, and taking it from the grid means the
    /// stand-in cannot introduce a vertical pop when the real build lands.
    ///
    /// The site comes off the **grid**, not from a site-table lookup. That is
    /// deliberate: it is the origin the grid's own `x`/`y` ranges were measured
    /// from, so the two boxes are guaranteed to be in one frame. A second
    /// opinion about where the radar is would put the stand-in off its floor by
    /// however much the two disagreed, and the error would then vanish the
    /// moment the real grid landed — a discontinuity of exactly the kind the
    /// stand-in exists to remove.
    /// # A target with no picked region is drawn in the grid's own box
    ///
    /// Such a target's width is the volume's own reach
    /// (`rustdar_radar::voxel::box_half_width_km` over `volume_reach_km`) — a
    /// fact about a scan the renderer never sees, so there is no rectangle here
    /// to crop into. The held grid's own box is the honest answer rather than a
    /// guess, because `same_scope` has already pinned the stand-in to the same
    /// **site** and the same **product**, and the reach is a function of exactly
    /// those two: across 150 archive volumes from 53 sites every WSR-88D
    /// reports the same 460.1 km reflectivity reach, and each moment follows its
    /// own cut. So the box the pending build is about to produce is the box the
    /// held grid was already built in, and drawing it there is not a
    /// registration error — it is the same ground.
    ///
    /// Refusing instead was what this did, and it was affordable only while a
    /// picked region was the common case. With `None` the ordinary state of a
    /// pane, refusing here would blank the pane to "Building…" on **every sealed
    /// sweep** — the live 3D view flashing empty every time new data landed,
    /// which is the seamless swap's whole reason for existing, inverted.
    ///
    /// The one case the two can disagree is a volume still filling: the reach is
    /// a maximum over the sweeps present, so a half-arrived volume could report
    /// a shorter one. In practice it cannot move after the first sealed sweep,
    /// because every moment's longest cut is at the bottom of the ladder — a
    /// WSR-88D's split cuts put the 460 km surveillance sweep and the 300 km
    /// Doppler sweep at 0.5°, first and second in the volume. If it ever did
    /// move, this is a stand-in behaving as a stand-in: one pop when the real
    /// grid lands, where the alternative is a blank pane every volume.
    fn for_target(target: &VolumeTarget, grid: &VoxelGrid) -> Option<Self> {
        let Some(region) = target.region else {
            return Some(Self::settled(grid));
        };
        let (site_lat, site_lon) = grid.site();
        // `clamped` is the resampler's own, not a copy of its bounds:
        // `horizontal_ranges_km` gives the arithmetic that needs the two to
        // agree bit for bit. Applied here even though `VolumeRegion::new`
        // already ran it, because `build_voxels` runs it unconditionally on
        // whatever the request carries — matching it call for call is what the
        // `==` below is comparing, and dropping it because the input "is
        // already clamped" would be an argument about a function this side
        // cannot see.
        let (x_km, y_km) = rustdar_radar::voxel::horizontal_ranges_km(
            (region.centre().lat, region.centre().lon),
            region.half_extent_km().clamped(),
            site_lat,
            site_lon,
        );
        let settled = Self::settled(grid);
        if (x_km, y_km) == (settled.x_km, settled.y_km) {
            return Some(settled);
        }
        // A grid with a zero horizontal axis. Impossible for anything
        // `build_voxels` produced, and a division that returned infinities here
        // would reach the GPU as a NaN matrix.
        let (scale, offset) = crop_into(grid, x_km, y_km)?;
        Some(Self {
            x_km,
            y_km,
            z_km_msl: settled.z_km_msl,
            scale,
            offset,
            // An affine that stays within `[0, 1]` on every axis needs no
            // bounds test at all, and the zoom-*in* case — the common one — is
            // exactly that.
            bounded: (0..3).any(|axis| offset[axis] < 0.0 || offset[axis] + scale[axis] > 1.0),
        })
    }

    /// The box a pane holding `lookup` for `target` is drawing.
    ///
    /// The target's own grid is drawn in its own box, always: it *is* the
    /// answer, so there is nothing to crop and nothing that could be asked for
    /// that it is not already. Only a stand-in is placed against the target,
    /// and only a stand-in can fail to be placeable — see [`Self::for_target`].
    fn for_lookup(lookup: &VolumeLookup, target: &VolumeTarget, grid: &VoxelGrid) -> Option<Self> {
        if lookup.stood_in {
            Self::for_target(target, grid)
        } else {
            Some(Self::settled(grid))
        }
    }
}

/// Kilometres across one horizontal cell of `grid`, east–west and north–south
/// — the resolution the picture on screen really has, which is not the
/// requested region's while a stand-in is up. `None` for a grid with no cells
/// across either axis, which `build_voxels` does not produce.
///
/// Both axes, because a box is the pane's viewport rectangle: the two extents
/// differ while `nx` and `ny` do not, so the two cell sizes differ in exactly
/// the pane's own proportion. Reporting only `x` would have made a wide pane's
/// caption overstate its north–south sharpness by that ratio.
fn cell_km(grid: &VoxelGrid) -> Option<(f32, f32)> {
    let axis = |(a, b): (f64, f64), cells: usize| {
        let cells = u32::try_from(cells).ok()?;
        (cells > 0).then(|| ((b - a) / f64::from(cells)) as f32)
    };
    Some((
        axis(grid.x_range_km(), grid.shape().nx)?,
        axis(grid.y_range_km(), grid.shape().ny)?,
    ))
}

/// `(scale, offset)` taking the unit cube of the box `x_km × y_km` (with the
/// grid's own vertical) to a coordinate in `grid`'s texture. `None` if the
/// grid has a zero horizontal axis.
///
/// A free function so the arithmetic can be pinned without a grid fixture at
/// every zoom ratio: this is where a swapped axis or an inverted offset would
/// live, and it is one line of algebra —
///
/// ```text
/// world = box_min + p · box_size          (what the shader marches)
/// t     = (world − grid_min) / grid_size  (where the texture has it)
/// ```
fn crop_into(grid: &VoxelGrid, x_km: (f64, f64), y_km: (f64, f64)) -> Option<([f32; 3], [f32; 3])> {
    let axes = [
        (x_km, grid.x_range_km()),
        (y_km, grid.y_range_km()),
        // The vertical is the grid's own, so it is the identity by
        // construction rather than by arithmetic that happens to cancel.
        (grid.z_range_km_msl(), grid.z_range_km_msl()),
    ];
    let mut scale = [0.0f32; 3];
    let mut offset = [0.0f32; 3];
    for (axis, (drawn, held)) in axes.into_iter().enumerate() {
        let held_size = held.1 - held.0;
        if !(held_size.is_finite() && held_size > 0.0) {
            return None;
        }
        scale[axis] = ((drawn.1 - drawn.0) / held_size) as f32;
        offset[axis] = ((drawn.0 - held.0) / held_size) as f32;
    }
    Some((scale, offset))
}

/// The grid's coarsest cell in kilometres — the axis extent over that axis'
/// cell count, maximised over the three axes. This is what
/// [`cloud_reconstruction_lod_for`] scales the smoothing by; on every shipped
/// box the horizontal axes are the coarse ones (the vertical is ~0.14 km).
/// Off the uniform rather than the grid so the value fed to the taper is
/// bit-identical to the extent and dims the same uniform hands the shader.
///
/// # The `max` decides something now that the two horizontal axes differ
///
/// A box is the rectangle of ground its pane is showing and the cell count is
/// the same on both axes, so a 16:9 box's east–west cell is 1.78× its
/// north–south one and this picks the east one. That makes a wide pane cross
/// the taper's 1.75 km/cell knee at a **tighter** zoom than a square box did —
/// measured on a 533 × 300 km box against a 300 × 300 km one, level 0 against
/// 0.526.
///
/// It is the right axis, and [`cloud_reconstruction_lod_for`]'s own measurement
/// is the argument rather than symmetry. The kernel spans two cells, so on that
/// box it is 4.2 km wide east–west and 2.3 km north–south; the table in that
/// function was taken at 1.80 km/cell, where a 3.6 km kernel took the ≥50 dBZ
/// eyewall to **zero** painted pixels. Keying on the finer axis would apply a
/// wider kernel than the one already measured erasing cores. The `max` is the
/// conservative half of the pair and the cost is only a luxury forgone: the
/// north–south axis is smoothed less than its own cells could take.
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
    /// The host memory every grid upload widens its index plane into, held
    /// across uploads instead of allocated inside each one.
    ///
    /// Here rather than inside `VolumePipelines` because `prepare` already
    /// reaches this struct through a `&mut`, and `CallbackResources` is natively
    /// a `Send + Sync` `TypeMap` — so a plain field costs nothing, where interior
    /// mutability would have to justify itself against that bound. Uploading is
    /// still a `&self` operation on the pipelines, which is what lets the mutant
    /// and silhouette suites keep sharing one built pipeline set.
    ///
    /// One of these for the whole application, like the map above it: uploads
    /// are serialised on the frame thread, so there is never a second one in
    /// flight to need a second.
    ///
    /// **Permanently resident once anything has been uploaded**, at the largest
    /// shape this process has seen. Which memory that is depends on the route
    /// the device can take, and [`VolumeStaging`] states both figures and the
    /// arithmetic between them: on desktop it is a 64.00 MiB ring of two DMA
    /// staging buffers, and the 32.00 MiB widening buffer that used to be the
    /// whole cost is never touched there. Not given back by `release_pane` or
    /// `retain_uploads`: a session that closed its last 3D pane is exactly the
    /// one likely to open another, and the whole point is that the pages are
    /// bought once. `VolumePipelines::upload_volume_at` has the measurement.
    /// It is host memory and so is outside the GPU budget
    /// `crate::constants::APP_TEXTURE_BUDGET_BYTES` states — `resident_bytes`
    /// below counts device textures and deliberately does not count this.
    staging: VolumeStaging,
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
            // Empty, not pre-sized: the shape is not known until the first
            // upload, and a machine that never opens a 3D pane must pay
            // nothing — the same rule the mirror above follows. `new` takes the
            // device only to read one feature bit off it; it allocates nothing.
            staging: VolumeStaging::new(device),
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
    /// The uploads half is not redundant with [`Self::retain_uploads`]'s call
    /// in `prepare`, and this is the case that says why: when the pane that
    /// went away was the **last** 3D pane, no `prepare` runs again at all, so
    /// this is the only thing that ever frees the grid texture. That is also
    /// why a pane hidden by a pane-count reduction has to reach here —
    /// `App::release_hidden_pane_volumes` is the caller that makes it, and
    /// `VolumeStore::hidden_holders` the predicate.
    ///
    /// The **mirror is not freed here**, and deliberately: it is one texture for
    /// the whole application rather than a per-pane resource, so which pane just
    /// let go says nothing about whether anyone still wants it.
    /// [`Self::release_mirror`] is the answer to that question, and the frame
    /// path asks it every frame.
    pub fn release_pane(&mut self, pane_idx: usize, live_ids: &[u64]) {
        self.targets.remove(&pane_idx);
        self.retain_uploads(live_ids);
    }

    /// Keep the uploads `live_ids` names and drop the rest.
    ///
    /// One line, named because it has two callers that must not drift: this
    /// frame's `prepare`, which prunes what the store has let go of before
    /// making its own resident, and [`Self::release_pane`], which is the only
    /// one of the two that still runs once the last 3D pane has gone.
    pub fn retain_uploads(&mut self, live_ids: &[u64]) {
        self.uploads.retain(|id, _| live_ids.contains(id));
    }

    /// Give `pane_idx` an offscreen of `size_px`, creating or resizing one only
    /// if it has to, and say whether one is in hand afterwards.
    ///
    /// `prepare`'s own first step, named rather than inlined so that the
    /// resources a pane holds can be made real — actual textures on an actual
    /// device — by something other than a paint callback. `egui_wgpu::Callback`
    /// wraps its `Box<dyn CallbackTrait>` in a private field, so a test holding
    /// the payload `paint` produces cannot call `prepare` on it, and a release
    /// test that could not allocate first would be asserting over two empty
    /// maps.
    pub fn ensure_pane_offscreen(
        &mut self,
        device: &wgpu::Device,
        pane_idx: usize,
        size_px: [u32; 2],
    ) -> bool {
        let slot = self.targets.entry(pane_idx).or_default();
        self.pipelines.ensure_offscreen(device, slot, size_px);
        slot.is_some()
    }

    /// Make `grid_id`'s upload resident — the texels once, the colour table
    /// whenever the effective one changed — and say whether it is.
    ///
    /// `prepare`'s second step, named for the reason
    /// [`Self::ensure_pane_offscreen`] gives. `palette` is the grid's **own**
    /// table and `alpha` the user's curve over it: the effective bytes are
    /// resolved here through [`effective_lut`], which is the one seam a curve
    /// is applied at, so no caller can upload a table the curve has not been
    /// through.
    ///
    /// The steady state is one hash lookup and one `Option<AlphaCurve>`
    /// comparison — `AlphaCurve`'s equality takes the `Arc` pointer fast path —
    /// and the 16 MiB of texels are written exactly once per grid, whatever the
    /// editor does.
    #[allow(clippy::too_many_arguments)]
    pub fn ensure_upload(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        grid_id: u64,
        cells: [u32; 3],
        indices: &[u8],
        palette: &[u8],
        alpha: Option<&AlphaCurve>,
        coarse: CoarseLevel,
    ) -> bool {
        // Through the entry API rather than `contains_key` + `insert`, which is
        // one hash lookup instead of two — and the upload is refusable, so this
        // is a `match` on the entry rather than `or_insert_with`.
        match self.uploads.entry(grid_id) {
            std::collections::hash_map::Entry::Occupied(occupied) => {
                let upload = occupied.into_mut();
                // The Volume Alpha seam's steady state: rewrite the 1 KiB
                // table only when the curve actually changed — a pointer
                // comparison almost every frame — and leave the 16 MiB grid
                // untouched always. `effective_lut` with `None` is the grid's
                // own bytes, so clearing a curve restores the palette
                // bit-exactly through the very same path that applied it.
                if upload.applied_alpha.as_ref() != alpha {
                    upload
                        .textures
                        .write_lut(queue, &effective_lut(palette, alpha));
                    upload.applied_alpha = alpha.cloned();
                }
                true
            }
            std::collections::hash_map::Entry::Vacant(vacant) => {
                let Some(textures) = self.pipelines.upload_volume_at(
                    device,
                    queue,
                    cells,
                    indices,
                    &effective_lut(palette, alpha),
                    coarse,
                    &mut self.staging,
                ) else {
                    // `upload_volume` has already logged which invariant it
                    // refused on. Nothing to add, and nothing to draw.
                    return false;
                };
                vacant.insert(VolumeUpload {
                    textures,
                    applied_alpha: alpha.cloned(),
                });
                true
            }
        }
    }

    /// GPU texture bytes this is holding in the two maps
    /// [`Self::release_pane`] gives back: the panes' offscreens, and the grid
    /// uploads with their colour tables.
    ///
    /// The mirror is **not** counted, for the same reason `release_pane` does
    /// not free it — it is one texture for the whole application, released by
    /// [`Self::release_mirror`] on its own predicate, and folding it in here
    /// would make a per-pane figure move when no pane changed.
    ///
    /// The counterpart of `VolumeStore::texture_bytes`, and the two are
    /// different numbers on purpose: the store measures what the *host* has
    /// decided should be resident, this measures what the device is actually
    /// holding for it. A release that moved one without the other is exactly
    /// the leak this reports.
    pub fn resident_bytes(&self) -> usize {
        let offscreens: usize = self
            .targets
            .values()
            .flatten()
            .map(|target| crate::volume::quality::offscreen_bytes(target.size()))
            .sum();
        let uploads: usize = self
            .uploads
            .values()
            .map(|upload| upload.textures.texture_bytes())
            .sum();
        offscreens.saturating_add(uploads)
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
    /// The mirror's extent in points, which the positions above are normalised
    /// against. See [`floor_lanes`] and
    /// `rustdar_egui::volume_view::VolumeFrameState::mirror_size_points`.
    mirror_size_points: [f32; 2],
}

impl egui_wgpu::CallbackTrait for VolumeCallback {
    fn prepare(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        _screen_descriptor: &egui_wgpu::ScreenDescriptor,
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
        // Everything the store has let go of, before this frame's own is made
        // resident beside it. The same line `release_pane` runs, and the reason
        // both exist is written there.
        resources.retain_uploads(&self.live_ids);

        if !resources.ensure_pane_offscreen(device, self.pane_idx, self.offscreen_px) {
            return Vec::new();
        }
        let shape = self.grid.shape();
        if !resources.ensure_upload(
            device,
            queue,
            self.grid_id,
            [shape.nx as u32, shape.ny as u32, shape.nz as u32],
            self.grid.indices(),
            // The grid's own table, through the one seam a user curve may
            // rewrite its alpha at — `ensure_upload` resolves the effective
            // bytes. See the module doc.
            self.grid.lut(),
            self.alpha.as_ref(),
            // Off the uniform, because the uniform is where the two facts
            // already agree: `gradient_shading` is the adapter's shading rung,
            // which is fixed for the renderer's life, and the cell size comes
            // from the same extents and dims the shader is handed. See
            // `coarse_level_for`.
            coarse_level_for(
                self.uniform.gradient_shading,
                largest_cell_km(&self.uniform),
            ),
        ) {
            return Vec::new();
        }

        // Destructured so the borrow checker can see that the pipelines are read
        // while the two maps are read beside them.
        let VolumeResources {
            pipelines,
            targets,
            uploads,
            mirror,
            // The upload above is the only reader, and it has already run —
            // including the staging ring's own submit, so nothing here is
            // waiting on a plane that has not been handed to the queue.
            staging: _,
        } = resources;
        // Both are known present — the two calls above answered `true` — and
        // both are answered rather than unwrapped because this runs on the frame
        // thread, where on wasm a panic aborts the whole application.
        let (Some(Some(target)), Some(upload)) =
            (targets.get(&self.pane_idx), uploads.get(&self.grid_id))
        else {
            return Vec::new();
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
                let (uv, geo) =
                    floor_lanes(source, source.mirror_size_points, mirror.is_gamma_encoded());
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

/// `pub(crate)` for one item alone: `tests::ready_grid`, the crate's only
/// real `VoxelGrid` — `build_voxels` is the sole constructor, and a second
/// copy of its fixture in another test module would be a second thing to keep
/// in step with the resampler. Everything else in here is a `#[test]`.
#[path = "volume_bridge/tests.rs"]
#[cfg(test)]
pub(crate) mod tests;
