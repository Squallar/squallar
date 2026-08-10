//! A resampled Cartesian volume: the native tilt ladder flattened onto an
//! axis-aligned grid of palette indices, for a GPU raymarcher to upload as one
//! 3D texture.
//!
//! [`crate::sampler`] answers "what did the radar measure *here*" in the
//! radar's own polar coordinates. A raymarcher cannot ask that question per
//! step — it walks a ray through a box and wants one texture fetch per step.
//! [`build_voxels`] is the bridge: it evaluates the sampler once per output
//! cell and packs the answers into a byte per cell plus a 1 KiB colour table.
//!
//! **This module contains no GPU code and no wire codec.** It produces a
//! [`VoxelGrid`] and stops; uploading it is WP-I's and carrying it across the
//! worker boundary is WP-D's. Everything here is host-side and testable with
//! no adapter.
//!
//! # Cost, and why the column primitive is the whole design
//!
//! One [`crate::sampler::VolumeSampler::column`] costs `4·N` gate reads — a
//! bilinear in azimuth × slant range per rung, ~64 on a 16-rung VCP 212
//! ladder — and every height after the first is free, a two-point lerp between
//! rungs already sampled. So a `nx × ny × nz` grid costs `nx·ny·4·N` gate
//! reads, not `nx·ny·nz·4·N`: an **`nz`-fold** saving, 128× on the desktop
//! shape. In numbers, [`DESKTOP_SHAPE`] is 65 536 columns over 8 388 608
//! cells, or 4.2 M gate reads against 537 M on a 16-rung ladder. The loop
//! below therefore runs `for y { for x { column_into(...); for z { ... } } }`
//! and nothing else — one [`crate::sampler::VolumeSampler::sample`] per voxel
//! is the version that does not fit in a frame.
//!
//! # Geometry
//!
//! The box is axis-aligned in a **site-centred azimuthal-equidistant tangent
//! plane**: `x` is km east of the radar, `y` is km north, `z` is km MSL, and
//! `(x, y)` maps to the polar pair the sampler wants by
//! `range = hypot(x, y)`, `azimuth = atan2(x, y)`. That is exactly invertible
//! against [`crate::beam::site_bearing_range_km`] — bearings and distances
//! *from the site* are both exact — which is what makes a column's coordinates
//! the radar's own rather than a projection's approximation of them. Distances
//! between two non-site points are distorted, which nothing here asks for.
//!
//! `index = z·(ny·nx) + y·nx + x`, cell centres at the half-step: cell `i`
//! along an axis spanning `(lo, hi)` over `n` cells sits at
//! `lo + (i + 0.5)·(hi − lo)/n`. All six range bounds plus [`VoxelGrid::site`]
//! travel in the output, so a renderer builds its model matrix from the grid
//! alone and looks nothing up. So do [`VoxelGrid::tilt_count`] and
//! [`VoxelGrid::widest_tilt_gap_deg`], for the same reason one level up: the
//! grid crosses the worker boundary and the sampler does not, so without them
//! nothing downstream can tell a volume off a 16-rung ladder from one off a
//! 3-rung ladder that interpolated a smooth layer into a 6° gap.
//!
//! `z` is **MSL**, because a 3D scene shares one vertical datum with terrain
//! and with every other overlay. The sampler's heights are above the antenna,
//! so the site's elevation is subtracted once per grid — via
//! [`crate::eet::radar_height_ft_near`] and the same `* 0.0003048` spelling
//! `render.rs` uses for `radar_km_msl`.
//!
//! There is no extrapolation anywhere. A cell under the lowest beam, over the
//! highest, past the last gate or outside every radial is
//! [`NO_DATA_INDEX`], and its value is `NaN`. The cone of silence and the
//! volume's outer shell are both just that.
//!
//! **The boundary is hard at cell resolution, and that is the sampler's
//! doing.** Its `blend` falls back to the nearest corner as soon as any corner
//! of an interpolation has no value, rather than averaging a measurement with
//! an absence — so a cell either carries a number or does not, and no
//! partial-alpha transition is baked into the grid. Every softening a viewer
//! sees at an echo edge comes from the GPU's `Linear` fetch across those hard
//! cells, which is exactly why the section below is about what that fetch
//! returns.
//!
//! # Vertical detail is the **ladder's**, never `nz`'s
//!
//! `nz` sets how finely the box is *diced*, not how finely the volume was
//! *measured*. Two consequences follow from the nearest-corner fallback above,
//! and both are inherited from the sampler rather than introduced here —
//! WP-B measured the same pair on a cross-section, and a voxel grid gets them
//! identically because it goes through the same
//! [`crate::sampler::Column::at_height_km`]:
//!
//! * **A layer between two rungs is invisible.** The radar only looked at the
//!   rung heights, so a 2 km slab at 100 km on a short ladder can sit entirely
//!   between tilts and paint nothing at all, however fine `nz` is.
//! * **A layer *on* one rung is smeared to the half-weight midpoints.** With a
//!   data rung between two that measured nothing, the data wins the blend
//!   wherever its weight exceeds its neighbour's — that is, out to the
//!   midpoint on each side. `a_layer_is_quantised_to_the_ladder_rather_than_to_nz`
//!   measures it: at 100 km on a 0.53 / 2.47 / 4.51° ladder, one rung paints a
//!   **3.48 km** band whatever the true layer's thickness was.
//!
//! Neither is a defect to fix — filling in between rungs is the fabrication
//! this whole feature is trying not to ship, and the alternative to smearing is
//! painting a beam as a zero-thickness sheet. Both are why
//! [`VoxelGrid::tilt_count`] and [`VoxelGrid::widest_tilt_gap_deg`] travel with
//! the grid: they are the numbers that say how much of the vertical structure
//! on screen was measured and how much is interpolation.
//!
//! # The encoding, and why index 0 is the bottom of the ramp
//!
//! The grid is `R8Unorm` **palette indices** with a 256-entry RGBA table
//! alongside, not `R32Float` values. Three reasons, in order of weight:
//!
//! 1. **Filterability.** `R32Float` is not a filterable texture format under
//!    `wgpu::Features::empty()`, which is the floor "all platforms at once"
//!    commits to. `R8Unorm` is. So the volume texture's sampler is `Linear`,
//!    and that is the *stated reason* for the format — not "one byte".
//! 2. Four times less across the worker boundary and in GPU memory.
//! 3. The table carries **alpha**, so the per-product transparency floors
//!    become the raymarcher's transfer function for free.
//!
//! Reason 1 is what forces the rest. Because index ↔ value is **affine**,
//! linear filtering *within* data is exactly linear interpolation of the
//! value — the elegant part, and the reason to keep `Linear`.
//!
//! But a fetch that straddles a data / no-data boundary interpolates between
//! whatever those two indices are. Had index 0 been reserved **out of band**,
//! sitting off the affine ramp, blending 0 with 195 would return ~97 — and 97
//! is a perfectly ordinary data index. Concretely, on an out-of-band ramp
//! spanning 0…95 dBZ over indices 1…255, a 65 dBZ core adjacent to nothing
//! renders a **32 dBZ, fully opaque** shell one voxel thick around every echo
//! and around the entire volume boundary. The alpha floor cannot rescue it:
//! the floor applies to the *fetched* index's table entry, not to the
//! neighbours it was blended from. On a feature whose whole risk register is
//! about not fabricating structure, that is a fabricated halo everywhere.
//!
//! **So index 0 is the bottom of the affine ramp *and* the no-data value.**
//! The interpolated value between data and no-data then falls monotonically
//! toward the ramp bottom instead of landing mid-ramp, and the ramp bottom is
//! placed where the palette is transparent, so the shell fades out rather than
//! stepping to an opaque middle. `an_echo_edge_fades_instead_of_fabricating_a_mid_value`
//! computes both encodings over the same edge and pins the difference: at a
//! 65 dBZ core's edge, bottom-of-ramp reads **16.25 dBZ** halfway across and
//! has faded to nothing by 67 % of the way, where the out-of-band encoding
//! reads **32.35 dBZ at full opacity** right up to the empty voxel itself.
//!
//! **How much of that is a fade rather than a step depends on the palette, and
//! only reflectivity's has a floor.** The band the fetch fades through is the
//! run of transparent entries at the bottom of the table, which exists only
//! where `get_color_for_value` refuses to paint. Measured, that is **64
//! indices for reflectivity — a quarter of the ramp — and 0 for the other
//! five**, whose palettes are opaque at every finite value; those five step
//! from opaque to absent in one quantisation level. The floors the paragraph
//! above cites (VIL, HHC, NROT) all belong to products
//! [`crate::sampler::samplable`] refuses, so reflectivity's `< 0 dBZ` is the
//! only one this module can reach.
//!
//! **For those five the shipped encoding is no worse — a wash or slightly
//! worse per moment — and the earlier claim that it was "strictly better"
//! was wrong.** The reasoning behind that claim was that an opaque
//! *end-of-ramp* colour beats an opaque *mid-ramp* one. That does not hold for
//! a bidirectional or centred palette, where the ramp's **midpoint is the
//! neutral** and its **bottom is the saturated extreme**. Half-edge fetches
//! under both encodings, measured by
//! `the_half_edge_costs_of_both_encodings_are_measured_per_moment`:
//!
//! | moment | echo | shipped | out-of-band |
//! |---|---|---:|---:|
//! | reflectivity | 65 dBZ | **16.25** | 32.35 |
//! | velocity | 30 m/s | **−17.00** | −3.12 |
//! | spectrum width | 4 m/s | 1.875 | 1.985 |
//! | ZDR | 1.5 dB | **−3.219** | −0.258 |
//! | ΦDP | 60° | 29.055 | 29.203 |
//! | ρHV | 0.98 | **0.588** | 0.714 |
//!
//! All ten of those are fully opaque. So every ρHV echo edge — and the whole
//! volume shell — gets a one-voxel shell at ρHV ≈ 0.59, squarely in the
//! debris / non-meteorological band, and velocity gets a −17 m/s *inbound*
//! shell around every outbound couplet edge. Reflectivity is the one moment
//! where the shipped encoding is unambiguously better, and it is also the one
//! that 3D volume rendering is for.
//!
//! **Shipping bottom-of-ramp is still right**, on a different argument than
//! the one it was given: the out-of-band ramp spans the *palette's* range
//! rather than the moment's, so it cannot represent the moment's floor at all
//! and clamps real measurements outside it — which is a wrong number, not
//! merely a wrong colour on a boundary.
//!
//! **The actionable consequence for WP-I.** Because [`VoxelGrid::fade_band`]
//! is **0** for those five, the renderer has to supply the fade itself; it
//! cannot be inherited from the palette, because the palette has no
//! transparent region. The cheap route is a short forced-transparent run at
//! the bottom of [`colormap_lut`] — exactly the move already made for entry 0,
//! extended from one entry to a handful — which costs the lowest few
//! quantisation levels of a moment nobody reads at its floor and buys a real
//! fade on every one of the five. That is a transfer-function decision, so it
//! is WP-I's to make and is deliberately not made here;
//! [`VoxelGrid::fade_band`] reports the number a renderer needs to decide.
//!
//! **Index 0 is one quantisation step *below* the moment's floor.** The ramp's
//! 255 *data* levels run from the moment's lowest decodable value at index 1
//! to its highest at index 255; index 0 is one step under index 1. This is the
//! difference between "the bottom of the ramp is −32 dBZ" and "the bottom
//! **data** level is −32 dBZ": the second is what the grid needs, because
//! −32 dBZ is a real Level II level (raw code 2) and a real measurement must
//! never be indistinguishable from no data. It also makes the step come out
//! *exactly* on the moment's own quantum for four of the six moments —
//! reflectivity lands on exactly 0.5 dB per level over −32…+95 dBZ, which is
//! Level II's own 8-bit resolution. `no_measurement_encodes_as_the_no_data_index`
//! walks every raw code of every moment and pins it.
//!
//! # The table is baked by calling the palette, never by reading its stops
//!
//! Every entry is `palette::get_color_for_value(product, ramp_value(i))`, with
//! entry 0 forced fully transparent. It is **not** built from
//! [`crate::LegendScale::thresholds`], and the difference is not cosmetic:
//!
//! * The per-product transparency floors live *only* inside
//!   `get_color_for_value` — VIL below 1.0, HHC below 10.0, NROT under
//!   |0.25|, reflectivity under 0 dBZ, and the rest.
//! * `extract_scale` **filters out non-finite stops**, so ZDR's
//!   `NEG_INFINITY` floor — the stop that colours everything below −2 dB — is
//!   absent from `thresholds` entirely. A table built from the stops would
//!   leave ZDR's whole bottom third wrong.
//! * The four non-gradient scales (spectrum width, POSH, MEHS, HHC) step
//!   rather than interpolate, and `scale_color` is the only place that
//!   distinction is applied.
//! * Velocity's stops are in mph and its two halves live in separate tables.
//!
//! `the_table_is_the_palette_function_not_its_stops` pins all four.
//!
//! **A non-gradient scale's table must be consumed `NEAREST`.** Interpolating
//! between two steps of a categorical scale names a category that is not
//! there — graupel blended into hail. [`VoxelGrid::lut_filter`] carries the
//! fact in the type so a renderer cannot get it wrong. Today the only
//! reachable non-gradient samplable moment is spectrum width, where the cost
//! of getting it wrong is merely a smoothed step; the hydrometeor
//! classification, where it would be a wrong category, is not a moment and
//! [`crate::sampler::samplable`] refuses it. The rule is carried anyway,
//! because that is the state a renderer would be written against.
//!
//! This is the table's *own* filter. The **volume texture** is always
//! `Linear`; that is reason 1 above and it is not negotiable per product.
//!
//! # Declared quantisation
//!
//! `value_range` starts from `get_legend_scale(product).{min_value, max_value}`
//! and is widened to the moment's Level II range, so the quantisation is
//! declared rather than implied. Per moment, `[bottom data level, top data
//! level]` and the resulting step:
//!
//! | moment | 8-bit encoding | decodes to | declared span | step |
//! |---|---|---|---|---|
//! | reflectivity | scale 2, offset 66 | −32.0 … 94.5 dBZ | −32.0 … 95.0 | **0.5 dBZ** |
//! | velocity | scale 2, offset 129 | −63.5 … 63.0 m/s | −63.5 … 63.5 | **0.5 m/s** |
//! | spectrum width | scale 2, offset 129 | 0 … 63.0 m/s | 0 … 63.5 | **0.25 m/s** |
//! | ZDR | scale 16, offset 128 | −7.875 … 7.9375 dB | −7.875 … 8.0 | **0.0625 dB** |
//! | ΦDP | 16-bit, scale 2.8361 | 0 … 360° | 0 … 360 | 1.4173° |
//! | ρHV | scale 300, offset −60.5 | 0.208 … 1.052 | 0.2 … 1.06 | 0.003386 |
//!
//! Four of the six land on the encoding's own quantum exactly, so those four
//! lose nothing at all. ρHV's 0.003386 against its encoding's 0.003333 is a
//! 1.6 % coarsening, which is under the width of the digit its readout shows.
//!
//! **ΦDP is a real loss and is stated as one.** Its 16-bit encoding carries
//! 1 022 levels of 0.3526° over the turn, and 255 levels of 1.4173° is **4×
//! coarser**. That is a consequence of the one-byte index — of the format
//! decision itself, not of where the ramp's bottom sits — and it is bounded:
//! the ΦDP palette's stops are 15° apart, ten ramp levels each, so no colour
//! boundary moves. When a caller needs the full precision it asks for
//! [`VoxelRequest::values_wanted`], which keeps `f32`.
//!
//! Velocity's legacy 1 m/s mode reaches ±127 m/s and clamps to the ramp's
//! ends here. A 64 m/s radial velocity is not meteorological, and the palette
//! saturates at ±36 m/s regardless, so the clamp costs nothing visible.
//!
//! # ΦDP wraps, and a linear filter cannot know that
//!
//! Differential phase is **circular**: 0° and 360° are the same measurement,
//! so the two ends of an affine ramp are the same physical value. Filtering
//! across that seam blends index 255 with index 1 and returns the middle of
//! the ramp — 180°, the opposite phase. The sampler already handles this
//! *within* a query (`Blend::Angular360`, which is why [`crate::kdp`]'s
//! unfolder exists), but no `R8Unorm` texture filter can. It is a real defect
//! of this encoding for exactly one moment, it is bounded to gates either side
//! of a fold, and it is left alone rather than papered over.
//! [`VoxelGrid::wraps`] reports it;
//! `the_wrapping_moment_is_named_and_its_seam_error_is_measured` measures the
//! worst case.
//!
//! **To be explicit, because "so WP-I can decide" invited the wrong reading: a
//! filter choice is not on the table.** Switching the volume texture to
//! `Nearest` for ΦDP would stair-step **every voxel of every ΦDP volume** in
//! order to repair the handful of texel pairs that straddle a fold, and it
//! would discard the filterability the `R8Unorm` format was chosen for in the
//! first place. The seam is a small, bounded, local error; the cure is a large,
//! global, permanent one. `wraps()` exists so a renderer can *say* so — in a
//! readout, or by declining to draw ΦDP isosurfaces — not so it can reach for
//! the sampler.
//!
//! # Shapes and memory
//!
//! Every axis is **≤ 256**, so one code path satisfies the `GL_MAX_3D_TEXTURE_SIZE`
//! of 256 that GLES 3.0 only *guarantees* and that a phone browser may report.
//! The 512-XY desktop variant was rejected for that reason: 0.31 km per cell at
//! a 40 km half-width already beats the 1 km cube this replaces.
//!
//! | shape | cells | indices | + values | + table |
//! |---|---|---|---|---|
//! | [`WASM_SHAPE`] 128×128×64 | 1 048 576 | 1 MiB | 4 MiB | 1 KiB |
//! | [`MOBILE_SHAPE`] 192×192×96 | 3 538 944 | 3.375 MiB | 13.5 MiB | 1 KiB |
//! | [`DESKTOP_SHAPE`] 256×256×128 | 8 388 608 | 8 MiB | 32 MiB | 1 KiB |
//!
//! The index plane is what becomes a GPU texture and is what
//! [`VOXEL_TEXTURE_BUDGET_BYTES`] bounds. The value plane is host-side, four
//! times larger, and exists only when a caller asks for it — see
//! [`VoxelRequest::values_wanted`].
//!
//! **[`default_shape`] cannot pick the mobile shape, and that is deliberate.**
//! The `mobile` cfg is emitted by `rustdar-frontend/build.rs`, and cargo scopes
//! a build script's cfgs to its own crate; this crate has no build script, so
//! `#[cfg(mobile)]` here would be an `unexpected_cfgs` warning attached to dead
//! code that silently took the desktop budget on a handheld. [`MOBILE_SHAPE`]
//! is therefore a named constant the frontend's grid-spec ladder selects
//! explicitly, alongside stepping down when a device reports less than 256.

use nexrad_model::data::Scan;

use crate::beam;
use crate::palette::{get_color_for_value, get_legend_scale};
use crate::sampler::{Column, VolumeSampler};
use crate::types::{MomentSlot, RadarProduct};

/// The palette index meaning "the radar did not measure anything here", and
/// simultaneously the bottom of the affine value ramp. See the module doc —
/// this pairing is the encoding decision, not a coincidence.
pub const NO_DATA_INDEX: u8 = 0;

/// Bytes in [`VoxelGrid::lut`]: 256 entries × RGBA.
pub const LUT_LEN: usize = 256 * 4;

/// The alpha at or under which a table entry counts as **see-through** for
/// [`VoxelGrid::see_through_indices`] — a quarter opacity.
///
/// At or under this, several voxels of depth stay visible behind an entry at
/// the renderer's default extinction, so a run of such entries reads as haze
/// rather than wall. A quarter of the *full* alpha scale, not of the palettes'
/// own 180 ceiling, so the measure keeps meaning if a palette's ceiling moves.
pub const SEE_THROUGH_ALPHA_CEILING: u8 = 64;

/// The largest any axis may be: the `GL_MAX_3D_TEXTURE_SIZE` GLES 3.0
/// guarantees. Not the largest any *device* allows — the largest every device
/// must allow.
pub const MAX_AXIS: usize = 256;

/// Narrowest half-width a request may ask for, km. Below this the grid is
/// finer than the radar's own 250 m gates over most of its extent and the
/// resample invents smoothness.
pub const MIN_HALF_WIDTH_KM: f64 = 10.0;

/// Widest half-width a request may ask for, km — the reflectivity
/// surveillance range, matching [`crate::types::MAX_RANGE_KM`].
pub const MAX_HALF_WIDTH_KM: f64 = 230.0;

/// Bottom of the box a 3D view resamples by default, kilometres MSL.
///
/// Sea level, not the antenna: this axis is MSL throughout
/// ([`VoxelGrid::z_range_km_msl`]), and a site at 400 m with a base at its own
/// height would silently clip the lowest 400 m of every echo — the part with the
/// storm's inflow in it.
///
/// Here rather than in the frontend because a 3D pane has to know the box's
/// **height** to do its own camera arithmetic — the pan scale and the pivot are
/// both fractions of the box — and the pane and the resampler disagreeing about
/// that height would be a pan that drifts against the picture. One constant, two
/// readers.
pub const DEFAULT_BASE_KM_MSL: f64 = 0.0;

/// Top of the box a 3D view resamples by default, kilometres MSL.
///
/// 18 km clears every overshooting top in the continental United States with
/// room to spare, and stopping there rather than at 20 km spends the cells on air
/// that has weather in it: at 128 layers, 18 km is 141 m per layer against 156 m.
///
/// See [`DEFAULT_BASE_KM_MSL`] for why the pair lives here.
pub const DEFAULT_TOP_KM_MSL: f64 = 18.0;

/// What one grid's index plane may occupy, bytes.
///
/// Not a runtime check — nothing measures against it, exactly as
/// `LOOP_TEXTURE_BUDGET_BYTES` is not measured against. It is the budget the
/// three named shapes were chosen to fit, written down so that adding a fourth
/// has to be a deliberate decision about GPU memory.
/// `every_named_shape_fits_the_texture_budget` enforces it.
///
/// The **value** plane is not in this budget: it is host memory, it is four
/// times larger, and it is optional. Its figures are in the module doc's
/// table.
///
/// **Not the same thing as `rustdar_frontend::constants::VOLUME_TEXTURE_BUDGET_BYTES`,
/// despite the names, and deliberately not bound to it.** That one is
/// per-target (1.5 MiB / 5 MiB / 12 MiB) and carries ~1.5× headroom for the
/// alignment and driver overhead a real GPU allocation costs; this one is a
/// flat ceiling equal to the largest index plane this module will produce, so
/// that adding a fourth shape has to be a decision. They answer different
/// questions — "will the allocation fit the device" versus "is this module
/// still producing what it said it would" — and binding them would make the
/// second untestable without a GPU. What *is* bound, because it is genuinely
/// one number in two places, is the grid's dimensions and its table size:
/// `the_grid_dimensions_match_the_shapes_rustdar_radar_names`.
pub const VOXEL_TEXTURE_BUDGET_BYTES: usize = 8 * 1024 * 1024;

/// 128 × 128 × 64 — one MiB of indices, for wasm's single worker and 4 GiB
/// linear memory.
pub const WASM_SHAPE: VoxelShape = VoxelShape {
    nx: 128,
    ny: 128,
    nz: 64,
};

/// 192 × 192 × 96 — 3.375 MiB. Selected explicitly by the frontend; see the
/// module doc on why [`default_shape`] cannot select it.
pub const MOBILE_SHAPE: VoxelShape = VoxelShape {
    nx: 192,
    ny: 192,
    nz: 96,
};

/// 256 × 256 × 128 — 8 MiB, every axis at the GLES 3.0 guarantee.
pub const DESKTOP_SHAPE: VoxelShape = VoxelShape {
    nx: 256,
    ny: 256,
    nz: 128,
};

/// The default shape for a device class, as a function of the class rather
/// than of the `cfg`.
///
/// **Split out so both answers are reachable from a host test.** A `cfg`-gated
/// body is invisible to every target that does not compile it, and the wasm
/// rows of this workspace's gate are `cargo check`, never `cargo test` — so a
/// wasm arm that named the wrong constant would pass everything that actually
/// runs. Mutation testing found exactly that: replacing the wasm arm's body
/// wholesale survived the entire suite. Routing both arms through one testable
/// function is the move `rustdar-frontend`'s `mobile_cfg.rs` already makes for
/// the `mobile` predicate, for the same reason.
///
/// What stays unpinned on the host is only the `cfg` dispatch itself — that
/// the wasm arm exists and passes `true`. Nothing can pin that but a wasm test
/// runner.
const fn default_shape_for(is_wasm: bool) -> VoxelShape {
    if is_wasm { WASM_SHAPE } else { DESKTOP_SHAPE }
}

/// The shape this target builds by default.
///
/// wasm gets [`WASM_SHAPE`], everything else [`DESKTOP_SHAPE`].
/// [`MOBILE_SHAPE`] is **not** reachable from here — see the module doc. A
/// caller with a real device capability in hand should pass the shape it wants
/// rather than start from this.
#[cfg(target_arch = "wasm32")]
pub fn default_shape() -> VoxelShape {
    default_shape_for(true)
}

/// The shape this target builds by default. See the wasm arm.
#[cfg(not(target_arch = "wasm32"))]
pub fn default_shape() -> VoxelShape {
    default_shape_for(false)
}

/// How many cells a grid has along each axis.
///
/// `nx` runs east, `ny` north, `nz` up.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct VoxelShape {
    pub nx: usize,
    pub ny: usize,
    pub nz: usize,
}

impl VoxelShape {
    /// Total cells — the length of [`VoxelGrid::indices`].
    pub const fn cells(self) -> usize {
        self.nx * self.ny * self.nz
    }

    /// Whether every axis is between 1 and [`MAX_AXIS`] inclusive.
    ///
    /// A zero axis is refused rather than yielding an empty grid: a renderer
    /// dividing an extent by a zero dimension gets an infinity, and an empty
    /// grid is indistinguishable from a volume with nothing in it.
    pub const fn is_supported(self) -> bool {
        const fn ok(n: usize) -> bool {
            n >= 1 && n <= MAX_AXIS
        }
        ok(self.nx) && ok(self.ny) && ok(self.nz)
    }
}

/// What to resample, over what box.
///
/// The fields are public because this is an input record with no invariant to
/// protect: [`build_voxels`] clamps `half_width_km` and refuses everything
/// else it cannot honour, so there is no way to build one that lies about its
/// contents. [`VoxelGrid`]'s fields are private for the opposite reason.
#[derive(Debug, Clone, PartialEq)]
pub struct VoxelRequest {
    /// Latitude and longitude of the box's horizontal centre. Need not be the
    /// site; the output's `x`/`y` ranges are relative to the **site** either
    /// way.
    pub centre: (f64, f64),
    /// Half the box's east–west and north–south extent, km. Clamped to
    /// `[MIN_HALF_WIDTH_KM, MAX_HALF_WIDTH_KM]` rather than refused, because a
    /// zoom control that reaches the end of its travel should stop, not fail.
    pub half_width_km: f64,
    /// Bottom of the box, km MSL.
    pub base_km_msl: f64,
    /// Top of the box, km MSL. Must be strictly above `base_km_msl`.
    pub top_km_msl: f64,
    /// Which moment. Anything [`crate::sampler::samplable`] refuses yields
    /// `None`.
    pub product: RadarProduct,
    /// Cells per axis. Every axis must be in `1..=`[`MAX_AXIS`]; see
    /// [`default_shape`] and the three named shapes for the sizes this module
    /// budgets for.
    pub shape: VoxelShape,
    /// Whether to also keep the values in their own units.
    ///
    /// A raymarcher needs only the indices; a hover readout over a 3D pane
    /// needs real numbers. The plane costs four bytes per cell — 32 MiB at
    /// [`DESKTOP_SHAPE`] — so it is opt-in rather than always present.
    pub values_wanted: bool,
}

/// How the colour table itself must be sampled.
///
/// **Not** how the volume texture is sampled: that is always `Linear`, which
/// is the whole reason the indices are `R8Unorm`. See the module doc.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LutFilter {
    /// The product's scale interpolates between stops, so the table may be
    /// interpolated too.
    Linear,
    /// The product's scale **steps**. Interpolating between two entries names
    /// a value the scale does not define — for a categorical scale, a
    /// category that is not there.
    Nearest,
}

/// A resampled Cartesian volume, ready to become one 3D texture and one 1D
/// colour table.
///
/// Fields are private so the parts cannot come apart: the index plane, the
/// optional value plane, the table and the shape all have to agree, and three
/// of the four are large enough that a caller would not notice if they did
/// not.
#[derive(Clone)]
pub struct VoxelGrid {
    indices: Vec<u8>,
    values: Option<Vec<f32>>,
    lut: Vec<u8>,
    shape: VoxelShape,
    x_range_km: (f64, f64),
    y_range_km: (f64, f64),
    z_range_km_msl: (f64, f64),
    site: (f64, f64),
    value_range: (f32, f32),
    /// Kept so [`VoxelGrid::lut_filter`] and [`VoxelGrid::wraps`] can be
    /// *derived*. Storing either alongside the product would be two fields
    /// that can disagree.
    product: RadarProduct,
    tilt_count: usize,
    widest_tilt_gap_deg: f64,
}

/// One line, never the grid.
///
/// **Hand-written for the reason [`crate::sampler::VolumeSampler`]'s is.** A
/// derived `Debug` prints the index plane byte by byte — 8 MiB at
/// [`DESKTOP_SHAPE`] — and `assert_eq!` reaches for `Debug` on failure, so the
/// derive would turn a one-line test failure into an unreadable one. The
/// summary carries the numbers a failure is actually about, including how many
/// cells hold data, which is the difference two grids most often have.
impl std::fmt::Debug for VoxelGrid {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let filled = self.indices.iter().filter(|&&i| i != NO_DATA_INDEX).count();
        write!(
            f,
            "{} {}x{}x{} x{:?} y{:?} z{:?} km msl, site {:?}, range {:?}, \
             {} rungs (widest gap {:.2}°), {filled}/{} cells with data, \
             values {}",
            self.product.code(),
            self.shape.nx,
            self.shape.ny,
            self.shape.nz,
            self.x_range_km,
            self.y_range_km,
            self.z_range_km_msl,
            self.site,
            self.value_range,
            self.tilt_count,
            self.widest_tilt_gap_deg,
            self.indices.len(),
            if self.values.is_some() {
                "kept"
            } else {
                "dropped"
            },
        )
    }
}

/// Equality that compares the value plane **bitwise**.
///
/// **A derived `PartialEq` makes almost every grid unequal to itself.** The
/// value plane stores `f32::NAN` in every cell the radar did not reach — which
/// on a real volume is most of the box, since the box is a cube and the
/// coverage is a cone — and `NaN != NaN`. This is
/// [`crate::sampler::Sample`]'s hand-written `PartialEq` one level up, for the
/// same reason and with more cells at stake: WP-D's worker reply asserts
/// `assert_eq!(execute(&…), None)` on a `JobOutput` that transitively contains
/// this type, and a byte-identical copy of a grid comparing unequal to it
/// would fail with nothing in the message saying why.
///
/// Bitwise rather than "equal or both NaN" so the comparison is a payload
/// comparison: two grids are equal exactly when their bytes are, which is what
/// a wire round trip needs to assert. A caller who put a signalling `NaN` in
/// one and a quiet one in the other has two different payloads.
impl PartialEq for VoxelGrid {
    fn eq(&self, other: &Self) -> bool {
        fn same_values(a: Option<&Vec<f32>>, b: Option<&Vec<f32>>) -> bool {
            match (a, b) {
                (None, None) => true,
                (Some(a), Some(b)) => {
                    a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x.to_bits() == y.to_bits())
                }
                _ => false,
            }
        }
        self.shape == other.shape
            && self.product == other.product
            && self.tilt_count == other.tilt_count
            && self.widest_tilt_gap_deg == other.widest_tilt_gap_deg
            && self.x_range_km == other.x_range_km
            && self.y_range_km == other.y_range_km
            && self.z_range_km_msl == other.z_range_km_msl
            && self.site == other.site
            && self.value_range == other.value_range
            && self.indices == other.indices
            && self.lut == other.lut
            && same_values(self.values.as_ref(), other.values.as_ref())
    }
}

impl VoxelGrid {
    /// One palette index per cell, `nx·ny·nz` of them, ordered
    /// `z·(ny·nx) + y·nx + x`. Upload as `R8Unorm` with a `Linear` sampler.
    pub fn indices(&self) -> &[u8] {
        &self.indices
    }

    /// The same cells in the product's own units, `NaN` wherever
    /// [`indices`](Self::indices) holds [`NO_DATA_INDEX`]. `None` unless
    /// [`VoxelRequest::values_wanted`] asked for it.
    pub fn values(&self) -> Option<&[f32]> {
        self.values.as_deref()
    }

    /// Exactly [`LUT_LEN`] bytes: 256 RGBA entries, entry `i` the colour of
    /// index `i`. Entry 0 is fully transparent.
    pub fn lut(&self) -> &[u8] {
        &self.lut
    }

    pub fn shape(&self) -> VoxelShape {
        self.shape
    }

    /// Km east of the site at the box's west and east faces.
    pub fn x_range_km(&self) -> (f64, f64) {
        self.x_range_km
    }

    /// Km north of the site at the box's south and north faces.
    pub fn y_range_km(&self) -> (f64, f64) {
        self.y_range_km
    }

    /// Km MSL at the box's bottom and top faces.
    pub fn z_range_km_msl(&self) -> (f64, f64) {
        self.z_range_km_msl
    }

    /// The radar's `(latitude, longitude)` — the origin the `x`/`y` ranges are
    /// measured from.
    pub fn site(&self) -> (f64, f64) {
        self.site
    }

    /// The values index 0 and index 255 stand for. Index 0 is one
    /// quantisation step below the moment's lowest data level; see the module
    /// doc.
    pub fn value_range(&self) -> (f32, f32) {
        self.value_range
    }

    pub fn product(&self) -> RadarProduct {
        self.product
    }

    /// How many rungs the tilt ladder had when this grid was resampled.
    ///
    /// **Carried because the grid crosses the worker boundary and the sampler
    /// does not.** A volume rendered from a short ladder interpolates across
    /// whatever gap the ladder leaves and draws a smooth layer that is not
    /// there — no error, no `NaN`, and it looks better than the truth. That is
    /// the plan's own risk 2, and it is the reason WP-B's `SectionAxes` will
    /// carry the same pair for a cross-section: without them the only thing
    /// that knows is a [`crate::sampler::VolumeSampler`] that no longer
    /// exists by the time anything is drawn.
    ///
    /// A ladder of **one** rung is the degenerate case and it does not
    /// fabricate: a single beam has no vertical extent to interpolate over, so
    /// [`crate::sampler::Column::at_height_km`] answers only at exactly that
    /// beam's height and the grid comes back empty rather than smeared.
    /// `a_single_tilt_volume_fills_nothing_rather_than_smearing_one_beam` pins
    /// it.
    ///
    /// **That emptiness is measure-zero, not an invariant, so branch on this
    /// count rather than on the index plane.** A cell centre *can* land
    /// bit-exactly on the beam height — `at_height_km` returns the rung's own
    /// sample when the query equals the top rung's height — it just has
    /// probability zero over arbitrary box bounds. A caller that decided "is
    /// this volume usable" by testing whether every index is
    /// [`NO_DATA_INDEX`] would therefore be right almost always and wrong
    /// without warning, which is the worst available failure mode. `== 1` is
    /// the honest test.
    pub fn tilt_count(&self) -> usize {
        self.tilt_count
    }

    /// The largest angular step between adjacent rungs, degrees — `0.0` for a
    /// single-rung ladder. The size of the gap
    /// [`tilt_count`](Self::tilt_count) warns about.
    pub fn widest_tilt_gap_deg(&self) -> f64 {
        self.widest_tilt_gap_deg
    }

    /// How [`lut`](Self::lut) must be sampled. Derived from the product's
    /// scale, never stored. See the module doc.
    pub fn lut_filter(&self) -> LutFilter {
        if get_legend_scale(self.product).is_gradient {
            LutFilter::Linear
        } else {
            LutFilter::Nearest
        }
    }

    /// Whether the moment is **circular**, so that the two ends of the ramp
    /// are the same physical value and a linear filter across the seam returns
    /// the opposite phase rather than a blend. True only for differential
    /// phase; see the module doc.
    pub fn wraps(&self) -> bool {
        self.product == RadarProduct::DifferentialPhase
    }

    /// The value index `i` stands for. Affine in `i` over the whole 0..=255
    /// range, including the no-data index — that is the encoding decision.
    pub fn index_to_value(&self, index: u8) -> f32 {
        ramp_value(self.value_range, index)
    }

    /// The index a value encodes to. Never [`NO_DATA_INDEX`] for a finite
    /// value, so a measurement can never be mistaken for an absence.
    pub fn value_to_index(&self, value: f32) -> u8 {
        ramp_index(self.value_range, value)
    }

    /// How many indices above [`NO_DATA_INDEX`] the table is still fully
    /// transparent — the width, in index steps, of the band a `Linear` fetch
    /// fades through when it straddles an echo edge.
    ///
    /// This is the number that says whether the encoding's fade is a fade or a
    /// single step, and it is a property of the product's palette rather than
    /// of this module: it is large only where `get_color_for_value` has a
    /// transparency floor above the ramp's bottom. Reflectivity — the product
    /// 3D volume rendering is actually for — has one, so its band is a quarter
    /// of the whole ramp. `the_fade_band_is_measured_per_product` records
    /// every product's.
    pub fn fade_band(&self) -> u8 {
        match self.lut.chunks_exact(4).position(|entry| entry[3] != 0) {
            // Entry 0 is forced transparent, so the first opaque entry is at
            // index 1 or above and the band under it is `n − 1` wide.
            // `saturating_sub` rather than `−` because an opaque entry 0 —
            // which `colormap_lut` cannot produce — would mean a band of 0,
            // not a panic. `n` is a position in a 256-entry table, so the cast
            // cannot truncate.
            Some(n) => n.saturating_sub(1) as u8,
            // No opaque entry anywhere: the whole ramp fades. Unreachable from
            // `build_voxels`, since every product's palette is opaque
            // somewhere, and reachable by hand — which is how it is tested.
            None => u8::MAX,
        }
    }

    /// How many of the 255 **data** entries are see-through — at or under
    /// [`SEE_THROUGH_ALPHA_CEILING`] — wherever they sit on the ramp.
    ///
    /// The generalisation of [`Self::fade_band`] that the per-product
    /// transparency profiles need: velocity's see-through band is its *middle*
    /// (calm air), ρHV's is its *top* (uniform precipitation), and ΦDP's is
    /// its whole ramp at a flat low alpha — a bottom-run measurement reads 0
    /// for all three even when most of the ramp is see-through. "At or under
    /// a quarter opacity" rather than "exactly zero" because a fade's shoulder
    /// and a flat translucency both read as haze rather than wall, which is
    /// the property the renderer's solid-block gate actually needs; `fade_band`
    /// remains the march's skip-threshold anchor, which really is about the
    /// bottom of the ramp.
    pub fn see_through_indices(&self) -> u16 {
        self.lut
            .chunks_exact(4)
            .skip(1)
            .filter(|entry| entry[3] <= SEE_THROUGH_ALPHA_CEILING)
            .count() as u16
    }

    /// The offset of cell `(x, y, z)` in [`indices`](Self::indices) and
    /// [`values`](Self::values). `None` outside the grid.
    pub fn cell_offset(&self, x: usize, y: usize, z: usize) -> Option<usize> {
        (x < self.shape.nx && y < self.shape.ny && z < self.shape.nz)
            .then(|| z * self.shape.ny * self.shape.nx + y * self.shape.nx + x)
    }

    /// The index at cell `(x, y, z)`, or `None` outside the grid.
    pub fn index_at(&self, x: usize, y: usize, z: usize) -> Option<u8> {
        self.cell_offset(x, y, z).map(|o| self.indices[o])
    }

    /// The value at cell `(x, y, z)`, or `None` outside the grid or with no
    /// value plane. `Some(NaN)` where there is no data.
    pub fn value_at(&self, x: usize, y: usize, z: usize) -> Option<f32> {
        let o = self.cell_offset(x, y, z)?;
        self.values.as_ref().map(|v| v[o])
    }

    /// The centre of cell `(x, y, z)` as `(km east, km north, km MSL)`, all
    /// relative to [`site`](Self::site) except the last which is MSL. `None`
    /// outside the grid.
    pub fn cell_centre_km(&self, x: usize, y: usize, z: usize) -> Option<(f64, f64, f64)> {
        self.cell_offset(x, y, z)?;
        Some((
            axis_centre(self.x_range_km, self.shape.nx, x),
            axis_centre(self.y_range_km, self.shape.ny, y),
            axis_centre(self.z_range_km_msl, self.shape.nz, z),
        ))
    }

    /// Bytes this grid holds: index plane, value plane if present, and table.
    /// Only the index plane counts against [`VOXEL_TEXTURE_BUDGET_BYTES`].
    pub fn memory_bytes(&self) -> usize {
        self.indices.len() + self.values.as_ref().map_or(0, |v| v.len() * 4) + self.lut.len()
    }
}

/// The centre of cell `i` on an axis spanning `range` in `n` cells.
fn axis_centre(range: (f64, f64), n: usize, i: usize) -> f64 {
    range.0 + (i as f64 + 0.5) * (range.1 - range.0) / n as f64
}

/// The value palette index `i` stands for, affine over the whole 0..=255.
fn ramp_value(range: (f32, f32), index: u8) -> f32 {
    let (lo, hi) = range;
    lo + (hi - lo) * (f32::from(index) / 255.0)
}

/// The inverse, clamped to `1..=255` so no finite measurement encodes as
/// [`NO_DATA_INDEX`].
///
/// Computed in `f64` so the round trip through [`ramp_value`] is exact for
/// every one of the 255 data indices of every moment, which
/// `the_ramp_is_affine_and_round_trips_every_data_index` pins.
fn ramp_index(range: (f32, f32), value: f32) -> u8 {
    if !value.is_finite() {
        return NO_DATA_INDEX;
    }
    let (lo, hi) = (f64::from(range.0), f64::from(range.1));
    let step = (f64::from(value) - lo) / (hi - lo) * 255.0;
    if !step.is_finite() {
        return NO_DATA_INDEX;
    }
    step.round().clamp(1.0, 255.0) as u8
}

/// The bottom and top **data** levels of a moment: the values index 1 and
/// index 255 stand for.
///
/// The union of the legend's finite stops and the moment's Level II decoded
/// range, rounded outward to the encoding's own quantum where that makes the
/// step land on it exactly. The module doc tabulates all six with their
/// derivations; this function is where they are written down.
///
/// Keyed on [`MomentSlot`] with no wildcard arm, so a seventh moment cannot be
/// added without declaring its range.
fn data_levels(slot: MomentSlot) -> (f32, f32) {
    match slot {
        // Legend 0…95; encoding (2, 66) decodes codes 2…255 to −32.0…94.5 dBZ.
        // Span 127 over 254 steps is exactly Level II's own 0.5 dB.
        MomentSlot::Reflectivity => (-32.0, 95.0),
        // Legend ±36.01 m/s; encoding (2, 129) decodes to −63.5…+63.0 m/s. The
        // top is carried to +63.5 so the step is exactly the encoding's 0.5 m/s
        // and the ramp is symmetric about zero, which the bidirectional
        // velocity palette wants.
        MomentSlot::Velocity => (-63.5, 63.5),
        // Legend 0…10.2889; the same (2, 129) encoding, non-negative half, to
        // 63.0 m/s. Carried to 63.5 for a step of exactly 0.25 m/s.
        MomentSlot::SpectrumWidth => (0.0, 63.5),
        // Legend −2.0…5.5 (its NEG_INFINITY floor is not a value); encoding
        // (16, 128) decodes to −7.875…+7.9375 dB. Carried to 8.0 for a step of
        // exactly 1/16 dB.
        MomentSlot::DifferentialReflectivity => (-7.875, 8.0),
        // A circular moment over its whole turn. Legend stops end at 345°; the
        // ramp must reach 360° because the palette wraps there.
        MomentSlot::DifferentialPhase => (0.0, 360.0),
        // Legend 0.45…0.98; encoding (300, −60.5) decodes to 0.208…1.052.
        // Widened to 0.2…1.06 so both decoded ends are inside the ramp rather
        // than clamped at it.
        MomentSlot::CorrelationCoefficient => (0.2, 1.06),
    }
}

/// [`data_levels`], with the derived products' own ranges layered over the
/// slot's.
///
/// A derived product borrows a native moment's *slot* but not its units:
/// NROT is unitless rotation in a velocity slot, KDP is °/km in a ΦDP slot —
/// encoded into the slot's ramp they would read as nonsense (±4 rotation
/// squeezed into ±63.5 m/s is half an index of signal). SRV keeps velocity's
/// range: same units, same symmetric-about-zero palette. The ranges here
/// match `derive`'s codecs exactly — raw 2..=255 and index 1..=255 both span
/// `[lo, hi]`.
fn data_levels_for(product: RadarProduct, slot: MomentSlot) -> (f32, f32) {
    match product {
        // Unitless; GR pins the meso class near |1|, ±4 keeps extreme
        // couplets on scale at 0.031 resolution.
        RadarProduct::NormalizedRotation => (-4.0, 4.0),
        // The estimator's own display clamp.
        RadarProduct::SpecificDifferentialPhase => {
            (crate::kdp::KDP_MIN_DISPLAY, crate::kdp::KDP_MAX_DISPLAY)
        }
        _ => data_levels(slot),
    }
}

/// The full ramp: [`data_levels`] with index 0 placed one step below index 1.
///
/// The 255 data indices span `[lo, hi]`, so one step is `(hi − lo)/254` and
/// the ramp runs from `lo − step` at index 0 to `hi` at index 255. See the
/// module doc on why index 0 is *below* the moment's floor rather than on it.
fn value_range_for(slot: MomentSlot) -> (f32, f32) {
    let (lo, hi) = data_levels(slot);
    let step = (f64::from(hi) - f64::from(lo)) / 254.0;
    ((f64::from(lo) - step) as f32, hi)
}

/// [`value_range_for`] keyed by product first — the derived products carry
/// their own ranges (see [`data_levels_for`]).
fn value_range_for_product(product: RadarProduct, slot: MomentSlot) -> (f32, f32) {
    match product {
        RadarProduct::NormalizedRotation | RadarProduct::SpecificDifferentialPhase => {
            let (lo, hi) = data_levels_for(product, slot);
            let step = (f64::from(hi) - f64::from(lo)) / 254.0;
            ((f64::from(lo) - step) as f32, hi)
        }
        _ => value_range_for(slot),
    }
}

/// Where a moment's default 3D transparency starts and ends, in the moment's
/// own units. Each row is the WP-I transfer-function decision the module doc
/// deferred, made per product and written down here so a test can pin it and a
/// reviewer can argue with it.
///
/// The clear edge is where the volume becomes fully transparent; the opaque
/// edge is where it reaches the palette's own alpha. Between them the alpha
/// rises smoothly. The 2D palettes are untouched — this shapes only the voxel
/// table, and it only ever *multiplies* the palette's alpha, so nothing here
/// can make a value more opaque than its plan-view colour.
mod volume_alpha_profile {
    /// Velocity (and, by the same physics, storm-relative velocity when it is
    /// admitted): the palette is diverging, so the uninteresting band is the
    /// **middle** — near-zero radial velocity, which fills most of any volume
    /// because ambient flow is everywhere — not the bottom of the ramp, which
    /// is the strongest inbound air and must stay opaque. Clear inside
    /// ±4 m/s (ambient drift and noise), fully opaque by ±20 m/s, the range
    /// where cores and couplets live. GR2Analyst's velocity volumes read the
    /// same way: the storm-scale wind structure stands free of the ambient
    /// field.
    pub const VELOCITY_CLEAR_MS: f32 = 4.0;
    pub const VELOCITY_OPAQUE_MS: f32 = 20.0;

    /// Spectrum width is sequential and its floor really is uninteresting:
    /// low width is laminar flow or pure noise. Clear below 2 m/s, opaque by
    /// 8 m/s — the band where turbulence, shear and mesocyclone interiors
    /// report.
    pub const SW_CLEAR_MS: f32 = 2.0;
    pub const SW_OPAQUE_MS: f32 = 8.0;

    /// Differential reflectivity diverges about rain's own background rather
    /// than about zero: drizzle and small drops sit near +0.25 dB. Clear
    /// within ±0.75 dB of that centre, opaque beyond ±3 dB from it — big-drop
    /// columns on the positive side, hail and graupel cores on the negative.
    pub const ZDR_CENTRE_DB: f32 = 0.25;
    pub const ZDR_CLEAR_DB: f32 = 0.75;
    pub const ZDR_OPAQUE_DB: f32 = 3.0;

    /// Correlation coefficient inverts the usual shape: uniform precipitation
    /// reads 0.97–1.0, and that is the background to see through. Clear above
    /// 0.97, opaque below 0.90 — the melting layer, debris and
    /// non-meteorological scatterers. A tornado debris signature is a low-ρHV
    /// column, and this profile is what makes it a column instead of a wall.
    pub const CC_OPAQUE: f32 = 0.90;
    pub const CC_CLEAR: f32 = 0.97;

    /// Differential phase gets a flat translucency instead of a value band,
    /// because no value band of ΦDP is honestly "background": the moment is
    /// cumulative along the ray and offset by a per-site system phase, so a
    /// fixed clear band would hide different physics at different sites. At
    /// ~35 % alpha the field reads as a haze with visible interior structure
    /// rather than a wall.
    pub const PHI_ALPHA: f32 = 0.35;

    /// Storm-relative velocity diverges about zero exactly as base velocity
    /// does, and shares its profile: with the storm motion subtracted, the
    /// near-zero band is air moving *with* the storm — the background — and
    /// the couplet cores the product exists to show sit beyond ±20 m/s.
    /// (The constants are velocity's own, reused at the profile table.)
    ///
    /// Normalized rotation: clear under |0.4| (the shear noise floor — the
    /// 2D palette's own first visible class), opaque at |1.0| and beyond,
    /// the mesocyclone convention GR pins its meso class to. A rotation
    /// volume is then a pair of standing columns where couplets stack.
    pub const NROT_CLEAR: f32 = 0.4;
    pub const NROT_OPAQUE: f32 = 1.0;

    /// Specific differential phase is sequential like reflectivity: clear
    /// under 0.25 °/km (drizzle and noise — below the estimator's own
    /// significance), opaque by 1.5 °/km, where heavy rain cores and
    /// hail-with-rain shafts live.
    pub const KDP_CLEAR_DEG_KM: f32 = 0.25;
    pub const KDP_OPAQUE_DEG_KM: f32 = 1.5;
}

/// `x` mapped smoothly from 0 at `edge0` to 1 at `edge1`, clamped.
fn smoothstep(edge0: f32, edge1: f32, x: f32) -> f32 {
    let t = ((x - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// The default 3D alpha multiplier for `product` at `value` — the per-product
/// transparency profile the volume table ships with (constants and rationale
/// in [`volume_alpha_profile`]).
///
/// `1.0` for reflectivity **deliberately**: its palette already fades over the
/// lowest quarter of its scale, that fade is the reference look every other
/// profile is measured against, and identity keeps every pre-WP reflectivity
/// grid bit-exact. The match is exhaustive over the samplable moments with no
/// wildcard, for the same reason `data_levels` has none: a newly admitted
/// product must have its transparency argued, not inherited — above all a
/// categorical palette, which must never be softened at all.
fn volume_alpha_scale(product: RadarProduct, value: f32) -> f32 {
    use volume_alpha_profile as p;
    match product {
        RadarProduct::Reflectivity => 1.0,
        RadarProduct::Velocity => {
            smoothstep(p::VELOCITY_CLEAR_MS, p::VELOCITY_OPAQUE_MS, value.abs())
        }
        RadarProduct::SpectrumWidth => smoothstep(p::SW_CLEAR_MS, p::SW_OPAQUE_MS, value),
        RadarProduct::DifferentialReflectivity => smoothstep(
            p::ZDR_CLEAR_DB,
            p::ZDR_OPAQUE_DB,
            (value - p::ZDR_CENTRE_DB).abs(),
        ),
        RadarProduct::CorrelationCoefficient => 1.0 - smoothstep(p::CC_OPAQUE, p::CC_CLEAR, value),
        RadarProduct::DifferentialPhase => p::PHI_ALPHA,
        // The derived products, admitted by `crate::derive`. SRV shares
        // velocity's profile — same units, same diverging shape, same
        // physics once the storm motion is subtracted.
        RadarProduct::StormRelativeVelocity => {
            smoothstep(p::VELOCITY_CLEAR_MS, p::VELOCITY_OPAQUE_MS, value.abs())
        }
        RadarProduct::NormalizedRotation => smoothstep(p::NROT_CLEAR, p::NROT_OPAQUE, value.abs()),
        RadarProduct::SpecificDifferentialPhase => {
            smoothstep(p::KDP_CLEAR_DEG_KM, p::KDP_OPAQUE_DEG_KM, value)
        }
        // Unreachable today: `crate::derive::volume_slot` refuses everything
        // below before a table is built. Spelled out rather than wildcarded so
        // a new `RadarProduct` variant fails to compile until it is classified
        // here, and so the arm anyone widening the vertical-view product set
        // must move a product out of is this one — with its transparency
        // argued, not inherited. Above all the categorical classification
        // must never be softened.
        RadarProduct::EchoTops
        | RadarProduct::EchoTopsInterpolated
        | RadarProduct::VerticallyIntegratedLiquid
        | RadarProduct::VilDensity
        | RadarProduct::ProbabilityOfSevereHail
        | RadarProduct::MaxExpectedHailSize
        | RadarProduct::HydrometeorClassification
        | RadarProduct::PrecipitationRate => 1.0,
    }
}

/// The 256-entry RGBA table for a product over a ramp, entry 0 forced fully
/// transparent.
///
/// Built by **calling** `get_color_for_value`, never by reading
/// `LegendScale::thresholds` — see the module doc for the four things that
/// would break. The alpha channel is the palette's own, scaled by the
/// product's [`volume_alpha_scale`] profile — the WP-I decision the module doc
/// deferred: the five moments whose palettes are opaque at every finite value
/// get their see-through band here, each shaped to its own physics rather
/// than by a forced run at the bottom, because a diverging palette's
/// uninteresting band is its middle and ρHV's is its **top**.
fn colormap_lut(product: RadarProduct, range: (f32, f32)) -> Vec<u8> {
    let mut lut = Vec::with_capacity(LUT_LEN);
    // Entry 0 is the no-data entry. Forced rather than taken from the palette
    // because only reflectivity and spectrum width have a transparency floor
    // the ramp's bottom falls under; velocity, ZDR, ΦDP and ρHV would each
    // hand back an opaque colour there, and an opaque no-data index paints the
    // whole outside of the volume.
    lut.extend_from_slice(&[0, 0, 0, 0]);
    for index in 1..=255u8 {
        let value = ramp_value(range, index);
        let (r, g, b, a) = get_color_for_value(product, value);
        let a = (f32::from(a) * volume_alpha_scale(product, value)).round() as u8;
        lut.extend_from_slice(&[r, g, b, a]);
    }
    lut
}

/// Resample `scan` onto a Cartesian grid, or `None` if it cannot be done
/// honestly.
///
/// `lat`/`lon` are the **radar's**, not the request's centre. `None` means one
/// of:
///
/// * the product has no native Level II moment — [`crate::sampler::samplable`];
/// * the scan's tilt ladder cannot be built, which above all includes a scan
///   reconstructed from a `RenderInput`, whose coverage pattern is a
///   placeholder with no cuts (see [`crate::sampler`]'s module doc — this is
///   the whole reason that refusal exists, and it is why nothing may call this
///   from the render worker until WP-D carries the cut angles);
/// * an axis outside `1..=`[`MAX_AXIS`];
/// * a non-finite number anywhere in the request or the site, or a top at or
///   below the base.
///
/// A `half_width_km` outside `[MIN_HALF_WIDTH_KM, MAX_HALF_WIDTH_KM]` is
/// **clamped**, not refused.
pub fn build_voxels(scan: &Scan, req: &VoxelRequest, lat: f64, lon: f64) -> Option<VoxelGrid> {
    build_voxels_with_motion(scan, req, lat, lon, None)
}

/// [`build_voxels`] with the user's storm motion override
/// `(speed_kt, direction_from_deg)`, read only when the product is
/// storm-relative velocity. Separate entry point rather than a request field
/// so the override never rides the voxel job's wire encoding — the worker
/// reads it off the `RenderInput`, which already carries it.
pub fn build_voxels_with_motion(
    scan: &Scan,
    req: &VoxelRequest,
    lat: f64,
    lon: f64,
    storm_motion_override: Option<(f32, f32)>,
) -> Option<VoxelGrid> {
    let shape = req.shape;
    if !shape.is_supported() {
        log::warn!(
            "voxel grid refused: shape {}x{}x{} has an axis outside 1..={MAX_AXIS}",
            shape.nx,
            shape.ny,
            shape.nz,
        );
        return None;
    }
    if !(req.half_width_km.is_finite()
        && req.base_km_msl.is_finite()
        && req.top_km_msl.is_finite()
        && req.centre.0.is_finite()
        && req.centre.1.is_finite()
        && lat.is_finite()
        && lon.is_finite())
    {
        log::warn!("voxel grid refused: a non-finite coordinate in the request or the site");
        return None;
    }
    if req.top_km_msl <= req.base_km_msl {
        log::warn!(
            "voxel grid refused: top {} km MSL is not above base {} km MSL",
            req.top_km_msl,
            req.base_km_msl,
        );
        return None;
    }

    // The derivation seam, shared with `xsect::render_section`: native
    // moments pass through as a borrow; SRV/NROT/KDP are computed per sweep
    // here, before anything samples, so a raw volume can never be resampled
    // under a derived label.
    let slot = crate::derive::volume_slot(req.product)?;
    let prepared = crate::derive::prepare(scan, req.product, storm_motion_override)?;
    let sampler = match &prepared {
        crate::derive::Prepared::Native(scan) => VolumeSampler::new(scan, req.product).ok()?,
        crate::derive::Prepared::Derived(scan) => {
            VolumeSampler::for_derived(scan, req.product, slot).ok()?
        }
    };

    let half = req
        .half_width_km
        .clamp(MIN_HALF_WIDTH_KM, MAX_HALF_WIDTH_KM);

    // The box's centre as km east / north of the site. Polar from the site and
    // back, so this is the same tangent plane the per-cell mapping below uses
    // and a centre *at* the site lands exactly on (0, 0).
    let (bearing_deg, range_km) = beam::site_bearing_range_km(lat, lon, req.centre.0, req.centre.1);
    let bearing = bearing_deg.to_radians();
    let (cx, cy) = (range_km * bearing.sin(), range_km * bearing.cos());

    let x_range_km = (cx - half, cx + half);
    let y_range_km = (cy - half, cy + half);
    let z_range_km_msl = (req.base_km_msl, req.top_km_msl);

    // The same spelling `render.rs` uses for `radar_km_msl`.
    let site_km_msl = crate::eet::radar_height_ft_near(lat, lon) * 0.0003048;

    let value_range = value_range_for_product(req.product, slot);
    let lut = colormap_lut(req.product, value_range);

    let (nx, ny, nz) = (shape.nx, shape.ny, shape.nz);
    let cells = shape.cells();
    let mut indices = vec![NO_DATA_INDEX; cells];
    let mut values = req.values_wanted.then(|| vec![f32::NAN; cells]);

    // Heights above the antenna, one per z row, hoisted out of the column loop
    // because the site's elevation does not vary over the box.
    let heights_km: Vec<f64> = (0..nz)
        .map(|iz| axis_centre(z_range_km_msl, nz, iz) - site_km_msl)
        .collect();

    let plane = ny * nx;
    let mut column = Column::new();
    for iy in 0..ny {
        let y_km = axis_centre(y_range_km, ny, iy);
        for ix in 0..nx {
            let x_km = axis_centre(x_range_km, nx, ix);
            let ground_range_km = x_km.hypot(y_km);
            let azimuth_deg = x_km.atan2(y_km).to_degrees().rem_euclid(360.0);
            sampler.column_into(azimuth_deg, ground_range_km, &mut column);

            for (iz, &height_km) in heights_km.iter().enumerate() {
                // One rule for both planes: a sample is carried only if it has
                // a finite number. Splitting the test would let an infinity
                // reach the value plane while the index plane called the same
                // cell empty.
                let Some(value) = column
                    .at_height_km(height_km)
                    .value()
                    .filter(|v| v.is_finite())
                else {
                    continue;
                };
                let offset = iz * plane + iy * nx + ix;
                indices[offset] = ramp_index(value_range, value);
                if let Some(values) = values.as_mut() {
                    values[offset] = value;
                }
            }
        }
    }

    Some(VoxelGrid {
        indices,
        values,
        lut,
        shape,
        x_range_km,
        y_range_km,
        z_range_km_msl,
        site: (lat, lon),
        value_range,
        product: req.product,
        tilt_count: sampler.tilt_count(),
        widest_tilt_gap_deg: sampler.widest_tilt_gap_deg(),
    })
}

// ── Codec ────────────────────────────────────────────────────────────────────
//
// The payload type owns its codec; the job framing that carries it lives in
// `rustdar-frontend`'s `offload`. That split is `render_input`'s, kept for the
// reason it was made there: a grid that can encode itself can be put on a
// message port, in an IndexedDB blob or in a test fixture without any of the
// three learning its layout, and there is one place where the layout is
// written down.
//
// So the frame is self-delimiting and self-describing — its own magic, its own
// version, its own lengths — rather than relying on the envelope to say how
// long it is or what it is. An envelope that had to know would be a second
// description of this layout.

/// Identifies a voxel payload, so a message that is not one fails on its first
/// four bytes instead of being read as a wildly-sized allocation.
///
/// Distinct from `render_input`'s `RDRI` and `xsect`'s `RDXS` on purpose: all
/// three travel over the same port, and a job that carried the wrong one has
/// to fail here rather than deep inside a decode that happens to line up.
const MAGIC: [u8; 4] = *b"RDVX";

/// Bumped whenever the layout below changes. The two ends of a worker boundary
/// can be different builds — see `rustdar-web`'s build-token handshake — so a
/// mismatch has to be a clean `None`, not a misparse.
const FORMAT_VERSION: u16 = 1;

impl VoxelGrid {
    /// Encode for transport. Little-endian throughout; the index plane and the
    /// colour table are copied verbatim, and the index plane is where nearly
    /// all the bytes are — 8 MiB at [`DESKTOP_SHAPE`], against 104 bytes of
    /// everything else.
    ///
    /// The value plane is written as raw `f32` bit patterns, which is what
    /// makes the round trip mean anything: this type's [`PartialEq`] compares
    /// that plane **bitwise**, so two `NaN`s with different payloads are two
    /// different grids, and an encoder that normalised them would be caught by
    /// its own equality.
    ///
    /// The optional plane is a `u32` count and then the data, with `0` for
    /// "absent". That encoding is unambiguous only because
    /// [`VoxelShape::is_supported`] requires every axis to be at least 1 and
    /// so guarantees [`VoxelShape::cells`] is at least 1 — a zero-cell shape
    /// would make "no plane" and "a plane of nothing" the same bytes.
    /// `a_supported_shape_always_has_a_cell_so_an_absent_plane_is_unambiguous`
    /// is what holds that.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.encoded_len());
        out.extend_from_slice(&MAGIC);
        out.extend_from_slice(&FORMAT_VERSION.to_le_bytes());
        out.extend_from_slice(&self.product.wire_code().to_le_bytes());

        out.extend_from_slice(&(self.shape.nx as u32).to_le_bytes());
        out.extend_from_slice(&(self.shape.ny as u32).to_le_bytes());
        out.extend_from_slice(&(self.shape.nz as u32).to_le_bytes());

        for (lo, hi) in [self.x_range_km, self.y_range_km, self.z_range_km_msl] {
            out.extend_from_slice(&lo.to_le_bytes());
            out.extend_from_slice(&hi.to_le_bytes());
        }
        out.extend_from_slice(&self.site.0.to_le_bytes());
        out.extend_from_slice(&self.site.1.to_le_bytes());
        out.extend_from_slice(&self.value_range.0.to_le_bytes());
        out.extend_from_slice(&self.value_range.1.to_le_bytes());

        // A `u32` for a `usize` field. The ladder has one rung per elevation
        // the volume flew — a couple of dozen on the longest operational VCP,
        // and the model numbers its cuts in a `u8` — so there is no reachable
        // count this narrows.
        out.extend_from_slice(&(self.tilt_count as u32).to_le_bytes());
        out.extend_from_slice(&self.widest_tilt_gap_deg.to_le_bytes());

        out.extend_from_slice(&(self.lut.len() as u32).to_le_bytes());
        out.extend_from_slice(&self.lut);
        out.extend_from_slice(&(self.indices.len() as u32).to_le_bytes());
        out.extend_from_slice(&self.indices);
        match &self.values {
            None => out.extend_from_slice(&0u32.to_le_bytes()),
            Some(values) => {
                out.extend_from_slice(&(values.len() as u32).to_le_bytes());
                for value in values {
                    out.extend_from_slice(&value.to_le_bytes());
                }
            }
        }
        out
    }

    /// Decode a payload [`to_bytes`](Self::to_bytes) produced.
    ///
    /// `None` on anything malformed. Every length is checked against what
    /// remains before it is used, so a corrupt frame cannot ask for a large
    /// allocation, and nothing is assembled into a `VoxelGrid` until all of
    /// these have passed:
    ///
    /// * wrong magic, or a version this build does not speak;
    /// * a product wire code this build does not have, or one it has but
    ///   cannot resample — the same [`samplable`] refusal [`build_voxels`]
    ///   makes, so the wire and the builder accept the same set of grids;
    /// * a shape with an axis outside `1..=`[`MAX_AXIS`]
    ///   ([`VoxelShape::is_supported`], *read* rather than restated);
    /// * an index plane that is not [`VoxelShape::cells`] long, a table that
    ///   is not exactly [`LUT_LEN`], or a value plane that is neither absent
    ///   nor exactly `cells` long;
    /// * truncation anywhere, or trailing bytes.
    ///
    /// The plane lengths are the ones that would be silent rather than loud.
    /// Every accessor on this type indexes with an offset computed from the
    /// *shape* — [`cell_offset`](Self::cell_offset) bounds-checks against
    /// `nx`, `ny`, `nz` and then indexes the plane — so a shape that claims
    /// more cells than the plane holds panics in [`index_at`](Self::index_at)
    /// and [`value_at`](Self::value_at), on whatever thread is drawing. A
    /// shape claiming fewer would instead upload a truncated texture and paint
    /// a volume with a corner missing. Both are refused here.
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        let mut r = Reader::new(bytes);
        if r.take(4)? != MAGIC {
            return None;
        }
        if r.u16()? != FORMAT_VERSION {
            return None;
        }
        let product = RadarProduct::from_wire_code(r.u16()?)?;
        // The same refusal `build_voxels` makes. A payload naming a product
        // with neither a native moment nor a derivation has no ramp
        // `value_range` could have come from, so its indices would decode to
        // numbers in units nothing measures.
        let slot = crate::derive::volume_slot(product)?;

        let shape = VoxelShape {
            nx: r.u32()? as usize,
            ny: r.u32()? as usize,
            nz: r.u32()? as usize,
        };
        // Before `cells()`, which multiplies three untrusted numbers: with
        // every axis at or under `MAX_AXIS` the product is at most 16.7 M and
        // cannot overflow, and with a zero axis it would be a plane length of
        // zero that every later check then agreed with.
        if !shape.is_supported() {
            return None;
        }
        let cells = shape.cells();

        let x_range_km = (r.f64()?, r.f64()?);
        let y_range_km = (r.f64()?, r.f64()?);
        let z_range_km_msl = (r.f64()?, r.f64()?);
        let site = (r.f64()?, r.f64()?);
        let value_range = (r.f32()?, r.f32()?);
        let tilt_count = r.u32()? as usize;
        let widest_tilt_gap_deg = r.f64()?;

        // Every number that describes where the box *is*. `build_voxels` emits
        // only finite ones — the extents are clamped and the site is a
        // latitude and a longitude — so this refuses nothing it produces, and
        // it closes the same hole `CrossSection::from_parts` closes on its
        // axes. A `NaN` extent divides into a cell size of `NaN` and every
        // `cell_centre_km` answers `NaN`; an infinite one collapses the cell
        // size to zero and puts every cell centre at the same place. Neither
        // panics, which is exactly why neither would be noticed.
        if ![
            x_range_km.0,
            x_range_km.1,
            y_range_km.0,
            y_range_km.1,
            z_range_km_msl.0,
            z_range_km_msl.1,
            site.0,
            site.1,
            widest_tilt_gap_deg,
        ]
        .iter()
        .all(|v| v.is_finite())
            || !value_range.0.is_finite()
            || !value_range.1.is_finite()
        {
            return None;
        }

        // `value_range` and the table are both **functions of the product**, so
        // a payload states each of them twice and the copies can disagree.
        // Neither disagreement fails: `index_to_value` would read the indices
        // off a ramp they were not quantised against, and the raymarch would
        // paint a table that is not this product's — a volume that renders,
        // looks like weather, and is a different field. Recomputed and compared
        // rather than trusted, which is `JobRequest`'s rule for the product
        // appearing twice, applied one level down.
        if value_range != value_range_for_product(product, slot) {
            return None;
        }

        // One byte per element on both of these, so `take` is the bound: it
        // can only hand back a slice that is really there, and nothing is
        // reserved on the claimed length before that.
        let lut_len = r.u32()?;
        let lut = r.take(lut_len as usize)?.to_vec();
        if lut.len() != LUT_LEN || lut != colormap_lut(product, value_range) {
            return None;
        }
        let index_len = r.u32()?;
        let indices = r.take(index_len as usize)?.to_vec();
        if indices.len() != cells {
            return None;
        }

        // Four bytes per element, so the claimed count is measured against
        // what remains *before* it becomes a capacity — a believed `u32::MAX`
        // would otherwise reserve 16 GiB and then fail the read. Bounding
        // first also means the absent/present discrimination below is made on
        // a count that could physically be there.
        let value_len = r.u32()?;
        let value_len = r.bounded(value_len, 4)?;
        let values = match value_len {
            // `is_supported` put at least one cell in the grid, so zero can
            // only mean "no plane" and never "a plane the size of the grid".
            0 => None,
            n if n == cells => {
                let mut values = Vec::with_capacity(n);
                for _ in 0..n {
                    values.push(r.f32()?);
                }
                Some(values)
            }
            // Any other length is a plane that does not describe this grid.
            // Accepting one would leave `value_at` indexing it with an offset
            // computed from the shape.
            _ => return None,
        };

        // Trailing bytes mean the two ends disagree about the layout even
        // though the version matched. Better to refuse than to raymarch half a
        // volume from it.
        if !r.at_end() {
            return None;
        }
        Some(Self {
            indices,
            values,
            lut,
            shape,
            x_range_km,
            y_range_km,
            z_range_km_msl,
            site,
            value_range,
            product,
            tilt_count,
            widest_tilt_gap_deg,
        })
    }

    /// What [`to_bytes`](Self::to_bytes) will write, exactly.
    ///
    /// Exactly, not approximately: a grid is 8 MiB of indices and up to 32 MiB
    /// of values at [`DESKTOP_SHAPE`], and a reallocation partway through
    /// copies all of it. Wrong by a little is only that copy; wrong by a lot
    /// means the layout and the estimate have drifted, which
    /// `the_encoded_length_of_a_grid_is_exact` is what catches.
    fn encoded_len(&self) -> usize {
        // Magic, version, product, three axes, three ranges, the site, the
        // value range, the tilt count and the widest gap.
        let header = 4 + 2 + 2 + 3 * 4 + 3 * 16 + 16 + 8 + 4 + 8;
        header
            + (4 + self.lut.len())
            + (4 + self.indices.len())
            + (4 + self.values.as_ref().map_or(0, |v| v.len() * 4))
    }
}

/// A bounds-checked cursor. Every accessor returns `None` rather than
/// panicking, because the bytes come off a message port and are not trusted.
///
/// A private copy of `render_input`'s, deliberately rather than a shared one.
/// It is thirty lines with no state beyond an offset, and the alternative —
/// a public type, or a fourth crate for it — would make the byte layout of
/// three payloads depend on one shared decoder's idea of what a `u32` is.
/// Each module owning its own reader is what lets each own its own format.
struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, at: 0 }
    }

    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let end = self.at.checked_add(n)?;
        let slice = self.bytes.get(self.at..end)?;
        self.at = end;
        Some(slice)
    }

    fn u16(&mut self) -> Option<u16> {
        Some(u16::from_le_bytes(self.take(2)?.try_into().ok()?))
    }

    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(self.take(4)?.try_into().ok()?))
    }

    fn f32(&mut self) -> Option<f32> {
        Some(f32::from_le_bytes(self.take(4)?.try_into().ok()?))
    }

    fn f64(&mut self) -> Option<f64> {
        Some(f64::from_le_bytes(self.take(8)?.try_into().ok()?))
    }

    /// `count` as a capacity, refused if the buffer cannot possibly hold that
    /// many items of `min_size` bytes each. Keeps a corrupt length from
    /// reserving gigabytes before the read fails.
    fn bounded(&self, count: u32, min_size: usize) -> Option<usize> {
        let count = count as usize;
        (count.checked_mul(min_size)? <= self.bytes.len() - self.at).then_some(count)
    }

    fn at_end(&self) -> bool {
        self.at == self.bytes.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sampler::{Sample, SampleStatus, samplable};
    use nexrad_model::data::{
        ChannelConfiguration, ElevationCut, MomentData, PulseWidth, Radial, RadialStatus, Sweep,
        VolumeCoveragePattern, WaveformType,
    };

    // ── Fixtures ────────────────────────────────────────────────────────────
    //
    // Built to *fail* a wrong implementation, per the sampler's own
    // experience: its first mutation pass left 13 survivors and every one was
    // a fixture too tidy to discriminate. So nothing here is symmetric or
    // tidy —
    //
    //  * sweeps arrive high cut first, so a builder trusting collection order
    //    gets the ladder upside down;
    //  * azimuths start away from 0 and wrap through it in collection order;
    //  * the two tilts carry different radial counts (720 super-res below,
    //    360 above), so a test that only ever reads the low tilt proves
    //    nothing about the high one;
    //  * the upper tilt's gates stop short, so range truncation is reachable;
    //  * fields vary along **both** azimuth and range, because a field
    //    constant along range cannot tell a half-cell offset from a correct
    //    one;
    //  * and the boundary fixtures plant a **sharp echo edge** — below
    //    threshold on one side, 65 dBZ on the other — because a fixture where
    //    every voxel has data cannot test the behaviour that motivates the
    //    whole encoding decision. The tests that need one assert that it is
    //    there before relying on it.

    const REFL_SCALE: f32 = 2.0;
    const REFL_OFFSET: f32 = 66.0;
    /// Operational super-resolution first-gate *centre*. Nonzero on purpose: a
    /// builder that forgot it is 2 km inward everywhere and still passes any
    /// fixture whose gates start at the origin.
    const FIRST_GATE_M: u16 = 2125;
    const GATE_M: u16 = 250;

    /// KTLX, whose elevation `eet`'s own test pins at 1213 ft.
    const SITE: (f64, f64) = (35.33306, -97.2775);
    const SITE_ELEV_FT: f64 = 1213.0;

    fn encode_refl(dbz: f64) -> u8 {
        ((dbz * f64::from(REFL_SCALE) + f64::from(REFL_OFFSET)).round() as i64).clamp(2, 255) as u8
    }

    /// What `encode_refl` round-trips to, so a 0.5 dB quantisation step is not
    /// mistaken for a builder error.
    fn round_trip_refl(dbz: f64) -> f32 {
        (f32::from(encode_refl(dbz)) - REFL_OFFSET) / REFL_SCALE
    }

    fn gate_slant_km(j: usize) -> f64 {
        f64::from(FIRST_GATE_M) / 1000.0 + j as f64 * f64::from(GATE_M) / 1000.0
    }

    /// dBZ at an azimuth and slant range, or `None` for below threshold — the
    /// no-data half of every edge in this module.
    type Field<'f> = &'f dyn Fn(f64, f64) -> Option<f64>;

    /// One reflectivity sweep, azimuths given explicitly in **collection**
    /// order.
    fn refl_sweep(
        elevation_number: u8,
        elevation_deg: f32,
        azimuths: &[f32],
        n_gates: usize,
        field: Field<'_>,
    ) -> Sweep {
        let spacing = 360.0 / azimuths.len() as f32;
        let radials = azimuths
            .iter()
            .enumerate()
            .map(|(i, &az)| {
                let bytes: Vec<u8> = (0..n_gates)
                    .map(|j| match field(f64::from(az), gate_slant_km(j)) {
                        None => 0,
                        Some(v) => encode_refl(v),
                    })
                    .collect();
                Radial::new(
                    0,
                    i as u16,
                    az,
                    spacing,
                    RadialStatus::IntermediateRadialData,
                    elevation_number,
                    elevation_deg,
                    Some(MomentData::from_fixed_point(
                        bytes.len() as u16,
                        FIRST_GATE_M,
                        GATE_M,
                        8,
                        REFL_SCALE,
                        REFL_OFFSET,
                        bytes,
                    )),
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                )
            })
            .collect();
        Sweep::new(elevation_number, radials)
    }

    /// One velocity sweep over `field`, m/s through the (2, 129) codec —
    /// the fixture for the fold-guard test, shaped like [`refl_sweep`].
    fn vel_sweep(
        elevation_number: u8,
        elevation_deg: f32,
        azimuths: &[f32],
        n_gates: usize,
        field: Field<'_>,
    ) -> Sweep {
        const VEL_SCALE: f32 = 2.0;
        const VEL_OFFSET: f32 = 129.0;
        let spacing = 360.0 / azimuths.len() as f32;
        let radials = azimuths
            .iter()
            .enumerate()
            .map(|(i, &az)| {
                let bytes: Vec<u8> = (0..n_gates)
                    .map(|j| match field(f64::from(az), gate_slant_km(j)) {
                        None => 0,
                        Some(v) => ((v * f64::from(VEL_SCALE) + f64::from(VEL_OFFSET)).round()
                            as i64)
                            .clamp(2, 255) as u8,
                    })
                    .collect();
                Radial::new(
                    0,
                    i as u16,
                    az,
                    spacing,
                    RadialStatus::IntermediateRadialData,
                    elevation_number,
                    elevation_deg,
                    None,
                    Some(MomentData::from_fixed_point(
                        bytes.len() as u16,
                        FIRST_GATE_M,
                        GATE_M,
                        8,
                        VEL_SCALE,
                        VEL_OFFSET,
                        bytes,
                    )),
                    None,
                    None,
                    None,
                    None,
                    None,
                )
            })
            .collect();
        Sweep::new(elevation_number, radials)
    }

    /// Azimuths in collection order: `n` of them, starting at `start` and
    /// wrapping through 0.
    fn wrapped_azimuths(n: usize, start: f64) -> Vec<f32> {
        let step = 360.0 / n as f64;
        (0..n)
            .map(|i| (start + i as f64 * step).rem_euclid(360.0) as f32)
            .collect()
    }

    fn cut(angle_deg: f64) -> ElevationCut {
        ElevationCut::new(
            angle_deg,
            ChannelConfiguration::ConstantPhase,
            WaveformType::CS,
            20.0,
            true,
            true,
            false,
            false,
            1,
            20,
            0.0,
            0.0,
            0.0,
            0.0,
            0.0,
            0.0,
            false,
            0,
            false,
            0,
            false,
            false,
        )
    }

    fn vcp(cut_angles: &[f64]) -> VolumeCoveragePattern {
        VolumeCoveragePattern::new(
            212,
            0,
            0.5,
            PulseWidth::Short,
            false,
            0,
            false,
            0,
            false,
            false,
            0,
            false,
            false,
            cut_angles.iter().copied().map(cut).collect(),
        )
    }

    /// The two elevations every fixture below flies, and the numbers the tests
    /// compute expected heights from.
    const LOW_DEG: f32 = 0.53;
    const HIGH_DEG: f32 = 4.47;
    const LOW_GATES: usize = 600; // to 151.9 km slant
    const HIGH_GATES: usize = 200; // stops at 51.9 km — range truncation

    /// A two-tilt volume over `field`.
    fn scan_of(field: Field<'_>) -> Scan {
        Scan::new(
            vcp(&[0.5, 4.5]),
            vec![
                refl_sweep(
                    2,
                    HIGH_DEG,
                    &wrapped_azimuths(360, 211.0),
                    HIGH_GATES,
                    field,
                ),
                refl_sweep(1, LOW_DEG, &wrapped_azimuths(720, 293.5), LOW_GATES, field),
            ],
        )
    }

    /// A two-tilt volume carrying **all six** moments, so the per-product
    /// tests build real populated grids rather than tables in isolation.
    ///
    /// Every moment gets its own raw byte per gate through its **own** scale
    /// and offset, which is how a real radial is laid out and is why a builder
    /// that reached for the wrong moment slot reads a plausible number in the
    /// wrong units rather than nothing at all.
    fn six_moment_scan() -> Scan {
        // (scale, offset) per moment, from the ICD; the same pairs
        // `no_measurement_encodes_as_the_no_data_index` restates.
        const CODECS: [(f32, f32); 6] = [
            (2.0, 66.0),    // reflectivity
            (2.0, 129.0),   // velocity
            (2.0, 129.0),   // spectrum width
            (16.0, 128.0),  // ZDR
            (2.8361, 2.0),  // PhiDP
            (300.0, -60.5), // rho HV
        ];
        let sweep = |elevation_number: u8, elevation_deg: f32, start: f64, n_gates: usize| {
            let azimuths = wrapped_azimuths(360, start);
            let spacing = 360.0 / azimuths.len() as f32;
            let radials = azimuths
                .iter()
                .enumerate()
                .map(|(i, &az)| {
                    // Not constant along range, and a different code in every
                    // moment: a wrong slot is then a wrong number rather than
                    // a lucky match.
                    let moment = |slot: usize| {
                        let (scale, offset) = CODECS[slot];
                        // The lowest raw code each moment's encoding actually
                        // carries. Spectrum width shares velocity's (2, 129)
                        // codec but is non-negative, so the RDA never emits a
                        // code under 129 for it; a fixture that did would be
                        // testing the out-of-span clamp rather than the
                        // builder, and the clamp has its own test.
                        let floor = usize::from(if slot == 2 { 129u8 } else { 2 });
                        let bytes: Vec<u8> = (0..n_gates)
                            .map(|j| (floor + ((j * 7 + slot * 31 + i) % (256 - floor))) as u8)
                            .collect();
                        Some(MomentData::from_fixed_point(
                            bytes.len() as u16,
                            FIRST_GATE_M,
                            GATE_M,
                            8,
                            scale,
                            offset,
                            bytes,
                        ))
                    };
                    Radial::new(
                        0,
                        i as u16,
                        az,
                        spacing,
                        RadialStatus::IntermediateRadialData,
                        elevation_number,
                        elevation_deg,
                        moment(0),
                        moment(1),
                        moment(2),
                        moment(3),
                        moment(4),
                        moment(5),
                        None,
                    )
                })
                .collect();
            Sweep::new(elevation_number, radials)
        };
        Scan::new(
            vcp(&[0.5, 4.5]),
            vec![
                sweep(1, LOW_DEG, 117.5, LOW_GATES),
                sweep(2, HIGH_DEG, 41.0, HIGH_GATES),
            ],
        )
    }

    /// Three rungs that all reach 100 km, with reflectivity above threshold on
    /// **exactly one** of them (or none).
    ///
    /// The other two carry the moment and report below threshold, so the
    /// ladder still has three rungs — which is the whole point: the question
    /// is what a *measured* layer on one rung does to its neighbours, not what
    /// a one-rung ladder does.
    fn one_rung_carries_data(carrier: Option<usize>) -> Scan {
        let full: Field<'_> = &|_, _| Some(45.0);
        let empty: Field<'_> = &|_, _| None;
        // Medians deliberately off their nominal cuts, as real ones are.
        let medians = [0.53f32, 2.47, 4.51];
        let sweeps = (0..3)
            .map(|i| {
                refl_sweep(
                    (i + 1) as u8,
                    medians[i],
                    &wrapped_azimuths(360, 137.0 + i as f64),
                    LOW_GATES,
                    if carrier == Some(i) { full } else { empty },
                )
            })
            .collect();
        Scan::new(vcp(&[0.5, 2.5, 4.5]), sweeps)
    }

    /// A scan whose coverage pattern has no cuts — what a scan reconstructed
    /// from a `RenderInput` looks like.
    fn placeholder_scan() -> Scan {
        Scan::new(
            vcp(&[]),
            vec![refl_sweep(
                1,
                LOW_DEG,
                &wrapped_azimuths(360, 0.0),
                LOW_GATES,
                &|_, _| Some(30.0),
            )],
        )
    }

    fn request(shape: VoxelShape) -> VoxelRequest {
        VoxelRequest {
            centre: SITE,
            half_width_km: 60.0,
            base_km_msl: 0.0,
            top_km_msl: 12.0,
            product: RadarProduct::Reflectivity,
            shape,
            values_wanted: true,
        }
    }

    /// A shape with three **different** axes, so a transposed index cannot
    /// pass by accident.
    const ODD: VoxelShape = VoxelShape {
        nx: 11,
        ny: 13,
        nz: 7,
    };

    /// Every moment a grid can be built for.
    const SLOTS: [MomentSlot; 6] = [
        MomentSlot::Reflectivity,
        MomentSlot::Velocity,
        MomentSlot::SpectrumWidth,
        MomentSlot::DifferentialReflectivity,
        MomentSlot::DifferentialPhase,
        MomentSlot::CorrelationCoefficient,
    ];

    /// The products those moments belong to, in the same order.
    const SAMPLABLE: [RadarProduct; 6] = [
        RadarProduct::Reflectivity,
        RadarProduct::Velocity,
        RadarProduct::SpectrumWidth,
        RadarProduct::DifferentialReflectivity,
        RadarProduct::DifferentialPhase,
        RadarProduct::CorrelationCoefficient,
    ];

    // ── Shapes, budget and the target default ───────────────────────────────

    #[test]
    fn every_named_shape_fits_the_texture_budget() {
        for (name, shape) in [
            ("wasm", WASM_SHAPE),
            ("mobile", MOBILE_SHAPE),
            ("desktop", DESKTOP_SHAPE),
        ] {
            assert!(
                shape.is_supported(),
                "{name} has an axis outside 1..={MAX_AXIS}",
            );
            assert!(
                shape.cells() <= VOXEL_TEXTURE_BUDGET_BYTES,
                "{name} needs {} bytes of index plane against a \
                 {VOXEL_TEXTURE_BUDGET_BYTES} byte budget",
                shape.cells(),
            );
        }
    }

    /// The module doc's memory table, as arithmetic rather than as prose.
    #[test]
    fn the_named_shapes_cost_what_the_module_doc_says() {
        const MIB: usize = 1024 * 1024;
        assert_eq!(WASM_SHAPE.cells(), MIB, "wasm: 1 MiB of indices");
        assert_eq!(MOBILE_SHAPE.cells(), 3_538_944, "mobile: 3.375 MiB");
        assert_eq!(DESKTOP_SHAPE.cells(), 8 * MIB, "desktop: 8 MiB");
        // The value plane is four times the index plane, which is what makes
        // the desktop grid 40 MiB rather than 8.
        assert_eq!(DESKTOP_SHAPE.cells() * 4, 32 * MIB);
    }

    /// wasm gets the small shape, everything else the large one, and the
    /// **mobile** shape is deliberately unreachable from here — see the module
    /// doc.
    #[test]
    fn default_shape_is_the_targets() {
        #[cfg(target_arch = "wasm32")]
        assert_eq!(default_shape(), WASM_SHAPE);
        #[cfg(not(target_arch = "wasm32"))]
        assert_eq!(default_shape(), DESKTOP_SHAPE);
        assert_ne!(
            default_shape(),
            MOBILE_SHAPE,
            "this crate has no build script, so it cannot see the `mobile` \
             cfg; the frontend selects MOBILE_SHAPE explicitly",
        );
    }

    /// Both arms of the `cfg` cascade, from a host that compiles only one of
    /// them.
    ///
    /// The test above can only ever check the arm it was built for, so the
    /// wasm arm's *content* would otherwise be checked by nothing that runs —
    /// mutation testing found precisely that hole. See
    /// [`default_shape_for`]'s doc.
    #[test]
    fn both_target_classes_get_their_own_default_shape() {
        assert_eq!(default_shape_for(true), WASM_SHAPE);
        assert_eq!(default_shape_for(false), DESKTOP_SHAPE);
        assert_ne!(default_shape_for(true), default_shape_for(false));
    }

    #[test]
    fn an_axis_outside_the_guarantee_is_refused() {
        let scan = scan_of(&|_, _| Some(40.0));
        // Each axis independently, in both directions, so a guard that checks
        // only one of the three survives none of these.
        for bad in [
            VoxelShape { nx: 0, ..ODD },
            VoxelShape { ny: 0, ..ODD },
            VoxelShape { nz: 0, ..ODD },
            VoxelShape { nx: 257, ..ODD },
            VoxelShape { ny: 257, ..ODD },
            VoxelShape { nz: 257, ..ODD },
        ] {
            assert_eq!(
                build_voxels(&scan, &request(bad), SITE.0, SITE.1),
                None,
                "{bad:?} should be refused",
            );
        }
        assert!(
            build_voxels(
                &scan,
                &request(VoxelShape {
                    nx: MAX_AXIS,
                    ny: 1,
                    nz: 1
                }),
                SITE.0,
                SITE.1,
            )
            .is_some(),
            "256 is the guarantee, so it is allowed",
        );
    }

    // ── Refusals ────────────────────────────────────────────────────────────

    /// Two refusal kinds, distinguished on purpose. The integrals and the
    /// classification have **no per-tilt field at all** — `derive::volume_slot`
    /// refuses them on any volume. The derived products (SRV, NROT, KDP) have
    /// one, but this fixture is reflectivity-only, so their derivation finds
    /// no source moment and refuses **this volume** — the same products build
    /// real grids on a velocity/ΦDP-carrying volume, which
    /// `derive::tests::a_derived_voxel_grid_resamples_the_derived_field` pins.
    #[test]
    fn a_product_with_no_native_moment_is_refused() {
        let scan = scan_of(&|_, _| Some(40.0));
        for product in [
            RadarProduct::VerticallyIntegratedLiquid,
            RadarProduct::EchoTops,
            RadarProduct::HydrometeorClassification,
            RadarProduct::VilDensity,
        ] {
            let req = VoxelRequest {
                product,
                ..request(ODD)
            };
            assert_eq!(
                build_voxels(&scan, &req, SITE.0, SITE.1),
                None,
                "{} has no per-tilt field to resample, on any volume",
                product.name(),
            );
        }
        for product in [
            RadarProduct::NormalizedRotation,
            RadarProduct::StormRelativeVelocity,
            RadarProduct::SpecificDifferentialPhase,
        ] {
            let req = VoxelRequest {
                product,
                ..request(ODD)
            };
            assert_eq!(
                build_voxels(&scan, &req, SITE.0, SITE.1),
                None,
                "{} derives from a moment this volume does not carry",
                product.name(),
            );
        }
    }

    /// The refusal that keeps a render worker from silently building a
    /// different ladder from the main thread's. Until WP-D carries the cut
    /// angles on the wire, this is the only thing standing between the two.
    #[test]
    fn a_placeholder_coverage_pattern_is_refused() {
        let scan = placeholder_scan();
        assert_eq!(build_voxels(&scan, &request(ODD), SITE.0, SITE.1), None);
    }

    #[test]
    fn a_non_finite_number_anywhere_is_refused() {
        let scan = scan_of(&|_, _| Some(40.0));
        let base = request(ODD);
        for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            // Every scalar independently: a guard covering six of the seven
            // survives whichever one it missed.
            let cases = [
                VoxelRequest {
                    half_width_km: bad,
                    ..base.clone()
                },
                VoxelRequest {
                    base_km_msl: bad,
                    ..base.clone()
                },
                VoxelRequest {
                    top_km_msl: bad,
                    ..base.clone()
                },
                VoxelRequest {
                    centre: (bad, SITE.1),
                    ..base.clone()
                },
                VoxelRequest {
                    centre: (SITE.0, bad),
                    ..base.clone()
                },
            ];
            for req in cases {
                assert_eq!(
                    build_voxels(&scan, &req, SITE.0, SITE.1),
                    None,
                    "{req:?} carries {bad} and should be refused",
                );
            }
            assert_eq!(build_voxels(&scan, &base, bad, SITE.1), None, "site lat");
            assert_eq!(build_voxels(&scan, &base, SITE.0, bad), None, "site lon");
        }
    }

    #[test]
    fn a_top_at_or_below_the_base_is_refused() {
        let scan = scan_of(&|_, _| Some(40.0));
        for (base_km_msl, top_km_msl) in [(5.0, 5.0), (5.0, 4.0)] {
            let req = VoxelRequest {
                base_km_msl,
                top_km_msl,
                ..request(ODD)
            };
            assert_eq!(build_voxels(&scan, &req, SITE.0, SITE.1), None);
        }
        let req = VoxelRequest {
            base_km_msl: 5.0,
            top_km_msl: 5.001,
            ..request(ODD)
        };
        assert!(build_voxels(&scan, &req, SITE.0, SITE.1).is_some());
    }

    /// A zoom control that runs out of travel should stop, not fail — so the
    /// half-width clamps where everything else refuses.
    #[test]
    fn the_half_width_is_clamped_rather_than_refused() {
        let scan = scan_of(&|_, _| Some(40.0));
        for (asked, want) in [
            (0.0, MIN_HALF_WIDTH_KM),
            (1.0, MIN_HALF_WIDTH_KM),
            (-500.0, MIN_HALF_WIDTH_KM),
            (60.0, 60.0),
            (10_000.0, MAX_HALF_WIDTH_KM),
        ] {
            let req = VoxelRequest {
                half_width_km: asked,
                ..request(ODD)
            };
            let grid = build_voxels(&scan, &req, SITE.0, SITE.1)
                .unwrap_or_else(|| panic!("{asked} km should clamp, not refuse"));
            let (lo, hi) = grid.x_range_km();
            assert!(
                (hi - lo - 2.0 * want).abs() < 1e-9,
                "asked {asked} km, wanted a {want} km half-width, got {:?}",
                grid.x_range_km(),
            );
        }
    }

    // ── Orientation and cell centres ────────────────────────────────────────

    /// x east, y north, z up — pinned with a quadrant field, on a shape whose
    /// three axes are all different so a transposed index cannot pass.
    #[test]
    fn the_grid_is_indexed_x_east_y_north_z_up() {
        // 60 dBZ strictly inside the north-east quadrant, 15 elsewhere.
        let scan = scan_of(&|az, _| {
            Some(if (0.0..90.0).contains(&az) {
                60.0
            } else {
                15.0
            })
        });
        let shape = VoxelShape {
            nx: 21,
            ny: 23,
            nz: 5,
        };
        // The corner columns below sit at 43.7 km ground range, where the two
        // rungs bracket 0.517 … 3.529 km above the antenna. Rows are 1 km
        // apart from 1.0 km MSL, so row 1 (1.63 km over the antenna) is inside
        // the bracket and row 4 (4.63 km) is over the top of it.
        let req = VoxelRequest {
            half_width_km: 40.0,
            base_km_msl: 0.5,
            top_km_msl: 5.5,
            ..request(shape)
        };
        let grid = build_voxels(&scan, &req, SITE.0, SITE.1).unwrap();

        // Corner columns, well away from the quadrant boundaries: their
        // azimuths are 44.2°, 135.8°, 224.2° and 315.8°.
        let iz = 1;
        let (west, east) = (2, shape.nx - 3);
        let (south, north) = (2, shape.ny - 3);
        let strong = grid.value_at(east, north, iz).unwrap();
        assert!(
            (strong - round_trip_refl(60.0)).abs() < 0.05,
            "north-east should read the 60 dBZ quadrant, read {strong}",
        );
        for (x, y, corner) in [
            (west, north, "north-west"),
            (east, south, "south-east"),
            (west, south, "south-west"),
        ] {
            let weak = grid.value_at(x, y, iz).unwrap();
            assert!(
                (weak - round_trip_refl(15.0)).abs() < 0.05,
                "{corner} should read the 15 dBZ background, read {weak}",
            );
        }

        // And z is up: the top row of the box is above the 4.47° beam at these
        // ranges, so nothing may be extrapolated into it.
        assert_eq!(
            grid.index_at(east, north, shape.nz - 1),
            Some(NO_DATA_INDEX),
        );
    }

    /// Cell centres at the half-step, proved by a field that **varies along
    /// range**: a builder sampling the cell's edge reads a different dBZ, and
    /// a builder sampling a constant field could not tell.
    #[test]
    fn cell_centres_sit_at_the_half_step() {
        // dBZ that names the ground range it was read at.
        let scan =
            scan_of(&|_, slant| Some(20.0 + beam::ground_range_km(slant, f64::from(LOW_DEG))));
        let shape = VoxelShape {
            nx: 2,
            ny: 1,
            nz: 3,
        };
        // At 20 km ground range the two rungs bracket 0.209 … 1.587 km over
        // the antenna, so rows at 0.7 / 1.1 / 1.5 km MSL — 0.33 / 0.73 /
        // 1.13 km over it — all sit inside.
        let req = VoxelRequest {
            half_width_km: 40.0,
            base_km_msl: 0.5,
            top_km_msl: 1.7,
            ..request(shape)
        };
        let grid = build_voxels(&scan, &req, SITE.0, SITE.1).unwrap();

        // Two columns over an 80 km span: centres at −20 and +20 km east, both
        // on the y = 0 line. Ground range 20 km, not 0 and not 40.
        assert_eq!(
            grid.cell_centre_km(0, 0, 0).map(|c| (c.0, c.1)),
            Some((-20.0, 0.0)),
        );
        assert_eq!(
            grid.cell_centre_km(1, 0, 0).map(|c| (c.0, c.1)),
            Some((20.0, 0.0)),
        );

        for ix in 0..2 {
            for iz in 0..shape.nz {
                let read = grid.value_at(ix, 0, iz).unwrap();
                assert!(
                    (read - round_trip_refl(40.0)).abs() < 0.3,
                    "column {ix} row {iz} sits at 20 km ground range, so the \
                     field reads 40 dBZ; got {read}. An edge-sampled column \
                     would read 20 or 60.",
                );
            }
        }
    }

    /// The vertical axis is MSL and the site's own elevation is subtracted
    /// exactly once.
    ///
    /// KTLX stands at 1213 ft — 0.3697 km — which is 7 rows of this grid. A
    /// builder that skipped the subtraction, or applied it with the wrong
    /// sign, moves the lowest row with data by 7 or 14 rows.
    #[test]
    fn the_height_axis_is_msl_above_the_sites_own_elevation() {
        let scan = scan_of(&|_, _| Some(35.0));
        let nz = 240;
        let (base_km_msl, top_km_msl) = (0.0, 12.0);
        let dz = (top_km_msl - base_km_msl) / nz as f64;
        let shape = VoxelShape { nx: 2, ny: 1, nz };
        let req = VoxelRequest {
            half_width_km: 40.0,
            base_km_msl,
            top_km_msl,
            ..request(shape)
        };
        let grid = build_voxels(&scan, &req, SITE.0, SITE.1).unwrap();

        let lowest_beam_km = beam::height_at_ground_km(20.0, f64::from(LOW_DEG));
        let site_km_msl = SITE_ELEV_FT * 0.0003048;
        assert!(
            site_km_msl / dz > 5.0,
            "precondition: the site's elevation must be several rows deep or \
             this test cannot see the subtraction ({site_km_msl} km over a \
             {dz} km row)",
        );

        let first_with_data = (0..nz)
            .find(|&iz| grid.index_at(0, 0, iz) != Some(NO_DATA_INDEX))
            .expect("the column crosses the beam somewhere");
        let got_msl = base_km_msl + (first_with_data as f64 + 0.5) * dz;
        let want_msl = lowest_beam_km + site_km_msl;
        assert!(
            (got_msl - want_msl).abs() <= dz,
            "lowest row with data is at {got_msl} km MSL; the 0.53° beam is \
             {lowest_beam_km} km over a {site_km_msl} km site, so it should be \
             {want_msl}. Dropping the site elevation would put it at \
             {lowest_beam_km}.",
        );
    }

    /// The box may be centred away from the radar, and the ranges it reports
    /// stay measured **from the site** — which is what lets a renderer place
    /// the box knowing only `site`.
    #[test]
    fn the_centre_may_sit_away_from_the_site() {
        let scan = scan_of(&|_, _| Some(30.0));
        // ~50 km due east of KTLX.
        let east_lon = SITE.1 + 50.0 / (111.320 * SITE.0.to_radians().cos());
        let req = VoxelRequest {
            centre: (SITE.0, east_lon),
            half_width_km: 20.0,
            ..request(ODD)
        };
        let grid = build_voxels(&scan, &req, SITE.0, SITE.1).unwrap();
        assert!(
            (grid.x_range_km().0 - 30.0).abs() < 0.5 && (grid.x_range_km().1 - 70.0).abs() < 0.5,
            "a box 50 km east with a 20 km half-width spans 30..70 km east of \
             the site; got {:?}",
            grid.x_range_km(),
        );
        assert!(
            (grid.y_range_km().0 + 20.0).abs() < 0.5 && (grid.y_range_km().1 - 20.0).abs() < 0.5,
            "and stays on the site's own latitude; got {:?}",
            grid.y_range_km(),
        );
        assert_eq!(grid.site(), SITE);

        // A box centred on the site itself lands exactly on zero, with no
        // rounding drift out of the polar round trip.
        let centred = build_voxels(&scan, &request(ODD), SITE.0, SITE.1).unwrap();
        assert_eq!(centred.x_range_km(), (-60.0, 60.0));
        assert_eq!(centred.y_range_km(), (-60.0, 60.0));
    }

    /// Every number a renderer builds its model matrix from, asserted
    /// together.
    ///
    /// The six range bounds plus the site are the whole contract of the
    /// output: a renderer reading them is not allowed to look anything else
    /// up, so an accessor quietly returning the wrong pair would put the
    /// volume somewhere else on screen with nothing else disagreeing.
    /// Mutation testing found `z_range_km_msl` and the height half of
    /// `cell_centre_km` unasserted for exactly that reason — the horizontal
    /// axes were covered and the vertical one was not.
    #[test]
    fn the_output_carries_everything_a_model_matrix_needs() {
        let scan = scan_of(&|_, _| Some(35.0));
        let req = VoxelRequest {
            half_width_km: 37.5,
            base_km_msl: 0.75,
            top_km_msl: 15.25,
            ..request(ODD)
        };
        let grid = build_voxels(&scan, &req, SITE.0, SITE.1).unwrap();
        assert_eq!(grid.shape(), ODD);
        assert_eq!(grid.x_range_km(), (-37.5, 37.5));
        assert_eq!(grid.y_range_km(), (-37.5, 37.5));
        assert_eq!(grid.z_range_km_msl(), (0.75, 15.25));
        assert_eq!(grid.site(), SITE);
        assert_eq!(grid.product(), RadarProduct::Reflectivity);
        assert_eq!(
            grid.value_range(),
            (-32.5, 95.0),
            "255 data levels of 0.5 dBZ from −32.0, with index 0 half a step \
             under the bottom of them",
        );

        // Cell centres on all three axes at once, at the half-step, at both
        // ends — a fencepost error moves the corner cells and leaves the
        // middle alone.
        let (dx, dy, dz) = (75.0 / 11.0, 75.0 / 13.0, 14.5 / 7.0);
        let close = |got: Option<(f64, f64, f64)>, want: (f64, f64, f64)| {
            let g = got.expect("inside the grid");
            assert!(
                (g.0 - want.0).abs() < 1e-9
                    && (g.1 - want.1).abs() < 1e-9
                    && (g.2 - want.2).abs() < 1e-9,
                "cell centre {g:?} should be {want:?}",
            );
        };
        close(
            grid.cell_centre_km(0, 0, 0),
            (-37.5 + dx / 2.0, -37.5 + dy / 2.0, 0.75 + dz / 2.0),
        );
        close(
            grid.cell_centre_km(10, 12, 6),
            (37.5 - dx / 2.0, 37.5 - dy / 2.0, 15.25 - dz / 2.0),
        );
        // And each axis's bound independently, so a guard covering two of the
        // three survives neither.
        assert_eq!(grid.cell_centre_km(11, 0, 0), None);
        assert_eq!(grid.cell_centre_km(0, 13, 0), None);
        assert_eq!(grid.cell_centre_km(0, 0, 7), None);
        assert_eq!(grid.index_at(11, 0, 0), None);
        assert_eq!(grid.value_at(0, 0, 7), None);
    }

    /// The ladder the grid was resampled from travels with it, because the
    /// sampler does not cross the worker boundary and the grid does.
    #[test]
    fn the_grid_reports_the_ladder_it_was_built_from() {
        let scan = scan_of(&|_, _| Some(35.0));
        let grid = build_voxels(&scan, &request(ODD), SITE.0, SITE.1).unwrap();
        assert_eq!(grid.tilt_count(), 2);
        assert!(
            (grid.widest_tilt_gap_deg() - (f64::from(HIGH_DEG) - f64::from(LOW_DEG))).abs() < 1e-6,
            "0.53° and 4.47° are 3.94° apart; reported {}",
            grid.widest_tilt_gap_deg(),
        );
        // Which is a wide enough gap to be worth warning about: at 60 km a
        // 3.94° step is over 4 km of unmeasured height.
        assert!(grid.widest_tilt_gap_deg() > 3.0);

        // And it is the sampler's own answer, not a recount.
        let sampler = VolumeSampler::new(&scan, RadarProduct::Reflectivity).unwrap();
        assert_eq!(grid.tilt_count(), sampler.tilt_count());
        assert_eq!(grid.widest_tilt_gap_deg(), sampler.widest_tilt_gap_deg());
    }

    /// A one-rung ladder is the degenerate case, and it fabricates **nothing**.
    ///
    /// A single beam has no vertical extent to interpolate over, so
    /// `Column::at_height_km` answers only at exactly that beam's height —
    /// which no cell centre lands on — and the grid comes back empty. That is
    /// the right answer and the opposite of the plan's risk 2: the danger with
    /// a short ladder is a smooth layer that is not there, and one rung cannot
    /// draw one. The grid still builds, and says why through
    /// [`VoxelGrid::tilt_count`].
    #[test]
    fn a_single_tilt_volume_fills_nothing_rather_than_smearing_one_beam() {
        let scan = Scan::new(
            vcp(&[0.5]),
            vec![refl_sweep(
                1,
                LOW_DEG,
                &wrapped_azimuths(720, 293.5),
                LOW_GATES,
                &|_, _| Some(50.0),
            )],
        );
        let grid = build_voxels(&scan, &request(ODD), SITE.0, SITE.1).unwrap();
        assert_eq!(grid.tilt_count(), 1);
        assert_eq!(grid.widest_tilt_gap_deg(), 0.0);
        assert!(
            grid.indices().iter().all(|&i| i == NO_DATA_INDEX),
            "one rung has no vertical extent, so nothing may be filled in",
        );
        // The same volume with a second cut *does* fill, so the emptiness
        // above is the ladder's doing and not a broken fixture.
        let two = scan_of(&|_, _| Some(50.0));
        let filled = build_voxels(&two, &request(ODD), SITE.0, SITE.1).unwrap();
        assert!(filled.indices().iter().any(|&i| i != NO_DATA_INDEX));
    }

    /// **Vertical detail belongs to the tilt ladder, not to `nz`.**
    ///
    /// WP-B measured this on a cross-section; a voxel grid inherits it exactly,
    /// because both go through `Column::at_height_km`, whose `blend` returns
    /// the **nearest** rung the moment its bracket partner has no value. So a
    /// measured layer on one rung is painted out to the half-weight midpoint on
    /// each side, and a layer that falls between rungs is painted nowhere.
    ///
    /// Both are pinned here in the grid's own units, on a 200-row box whose
    /// rows are 60 m apart — a resolution 58× finer than the band the ladder
    /// actually resolves, which is the point.
    #[test]
    fn a_layer_is_quantised_to_the_ladder_rather_than_to_nz() {
        let nz = 200;
        let (base_km_msl, top_km_msl) = (0.0, 12.0);
        let dz = (top_km_msl - base_km_msl) / nz as f64;
        let shape = VoxelShape { nx: 2, ny: 1, nz };
        // Half-width 200 km with two columns puts their centres at ±100 km
        // east on the y = 0 line — WP-B's own range.
        let req = VoxelRequest {
            half_width_km: 200.0,
            base_km_msl,
            top_km_msl,
            ..request(shape)
        };
        let site_km_msl = SITE_ELEV_FT * 0.0003048;
        let beam = |deg: f64| beam::height_at_ground_km(100.0, deg);
        let (low, middle, high) = (beam(0.53), beam(2.47), beam(4.51));

        // ── a layer measured on exactly one rung ──
        let grid = build_voxels(&one_rung_carries_data(Some(1)), &req, SITE.0, SITE.1).unwrap();
        assert_eq!(grid.tilt_count(), 3, "all three rungs must survive");
        let rows: Vec<usize> = (0..nz)
            .filter(|&iz| grid.index_at(1, 0, iz) != Some(NO_DATA_INDEX))
            .collect();
        assert!(!rows.is_empty(), "the middle rung's layer must paint");
        let height_of = |iz: usize| base_km_msl + (iz as f64 + 0.5) * dz - site_km_msl;
        let (first, last) = (height_of(rows[0]), height_of(rows[rows.len() - 1]));
        assert_eq!(
            rows.len(),
            rows[rows.len() - 1] - rows[0] + 1,
            "and it must paint one contiguous band, not a striped one",
        );

        let lower_mid = (low + middle) / 2.0;
        let upper_mid = (middle + high) / 2.0;
        assert!(
            (first - lower_mid).abs() <= dz,
            "the band's floor is the half-weight midpoint to the rung below \
             ({lower_mid} km), not the beam itself ({middle} km); got {first}",
        );
        assert!(
            (last - upper_mid).abs() <= dz,
            "and its ceiling is the midpoint to the rung above ({upper_mid} \
             km); got {last}",
        );

        // The fabricated thickness, as a number. One tilt, 3.48 km of band.
        assert!(
            ((last - first) - 3.48).abs() < 0.1,
            "one rung paints a {} km band at 100 km on this ladder",
            last - first,
        );
        assert!(
            (last - first) / dz > 50.0,
            "which is {}x the row height, so no amount of nz recovers the \
             layer's true thickness",
            (last - first) / dz,
        );

        // ── a layer that no rung looked at ──
        let missed = build_voxels(&one_rung_carries_data(None), &req, SITE.0, SITE.1).unwrap();
        assert_eq!(missed.tilt_count(), 3, "the ladder is the same one");
        assert!(
            missed.indices().iter().all(|&i| i == NO_DATA_INDEX),
            "a layer between tilts is measured by nothing and painted nowhere, \
             however fine the grid",
        );
    }

    // ── The builder adds no geometry of its own ─────────────────────────────

    /// Every cell is the sampler's own answer at that cell's coordinates.
    ///
    /// The guard against this module quietly growing a second copy of the beam
    /// geometry: the coordinates below are written out longhand rather than
    /// through `axis_centre`, so the two spellings have to agree.
    #[test]
    fn every_cell_is_the_samplers_own_answer() {
        let scan = scan_of(&|az, slant| (az < 200.0).then_some(10.0 + (slant % 37.0) + az / 12.0));
        let shape = VoxelShape {
            nx: 9,
            ny: 8,
            nz: 6,
        };
        let req = VoxelRequest {
            half_width_km: 55.0,
            base_km_msl: 0.5,
            top_km_msl: 9.5,
            ..request(shape)
        };
        let grid = build_voxels(&scan, &req, SITE.0, SITE.1).unwrap();
        let sampler = VolumeSampler::new(&scan, RadarProduct::Reflectivity).unwrap();
        let site_km_msl = SITE_ELEV_FT * 0.0003048;

        let mut with_data = 0usize;
        for iz in 0..shape.nz {
            let z_msl = 0.5 + (iz as f64 + 0.5) * (9.5 - 0.5) / shape.nz as f64;
            for iy in 0..shape.ny {
                let y = -55.0 + (iy as f64 + 0.5) * 110.0 / shape.ny as f64;
                for ix in 0..shape.nx {
                    let x = -55.0 + (ix as f64 + 0.5) * 110.0 / shape.nx as f64;
                    let want = sampler.sample(
                        x.atan2(y).to_degrees().rem_euclid(360.0),
                        x.hypot(y),
                        z_msl - site_km_msl,
                    );
                    let got_index = grid.index_at(ix, iy, iz).unwrap();
                    let got_value = grid.value_at(ix, iy, iz).unwrap();
                    match want.value().filter(|v| v.is_finite()) {
                        Some(v) => {
                            with_data += 1;
                            assert_eq!(got_value, v, "value at {ix},{iy},{iz}");
                            assert_eq!(
                                got_index,
                                grid.value_to_index(v),
                                "index at {ix},{iy},{iz}",
                            );
                        }
                        None => {
                            assert_eq!(got_index, NO_DATA_INDEX, "index at {ix},{iy},{iz}");
                            assert!(got_value.is_nan(), "value at {ix},{iy},{iz}");
                        }
                    }
                }
            }
        }
        // Both halves of the comparison have to be exercised, or the loop
        // above proves only that empty grids match empty grids.
        assert!(
            with_data > 0 && with_data < shape.cells(),
            "precondition: the fixture must produce both data and no-data \
             cells; got {with_data} of {}",
            shape.cells(),
        );
    }

    /// Nothing is filled in above the highest tilt, below the lowest, or past
    /// the last gate — the volume's shell is no-data, not extrapolated.
    #[test]
    fn nothing_is_extrapolated_outside_the_ladder() {
        let scan = scan_of(&|_, _| Some(45.0));
        let shape = VoxelShape {
            nx: 3,
            ny: 3,
            nz: 40,
        };
        // A box reaching well past the low tilt's last gate (151.9 km slant)
        // and well above the high tilt's beam.
        let req = VoxelRequest {
            half_width_km: 220.0,
            base_km_msl: 0.0,
            top_km_msl: 25.0,
            ..request(shape)
        };
        let grid = build_voxels(&scan, &req, SITE.0, SITE.1).unwrap();

        // Over the site every beam centre is at zero height, so the whole
        // column above the ground is above the volume — the cone of silence,
        // reported rather than invented.
        let centre = (shape.nx / 2, shape.ny / 2);
        assert!(
            (0..shape.nz).all(|iz| grid.index_at(centre.0, centre.1, iz) == Some(NO_DATA_INDEX)),
            "the cone of silence must stay empty",
        );

        // The corner column sits at 220·√2 = 311 km, past every gate.
        assert!(
            (0..shape.nz).all(|iz| grid.index_at(0, 0, iz) == Some(NO_DATA_INDEX)),
            "311 km is past the last gate of both tilts",
        );

        // The top of the box is over the 4.47° beam everywhere in it.
        let top = shape.nz - 1;
        assert!(
            (0..shape.nx)
                .all(|ix| (0..shape.ny).all(|iy| grid.index_at(ix, iy, top) == Some(NO_DATA_INDEX))),
            "25 km MSL is above the highest tilt at every range in this box",
        );

        // And the fixture is not simply empty.
        assert!(
            grid.indices().iter().any(|&i| i != NO_DATA_INDEX),
            "precondition: something in this grid must have data, or the \
             assertions above are vacuous",
        );
    }

    // ── The two planes ──────────────────────────────────────────────────────

    #[test]
    fn the_value_plane_is_absent_unless_asked_for() {
        let scan = scan_of(&|_, _| Some(40.0));
        let req = VoxelRequest {
            values_wanted: false,
            ..request(ODD)
        };
        let lean = build_voxels(&scan, &req, SITE.0, SITE.1).unwrap();
        assert_eq!(lean.values(), None);
        assert_eq!(lean.value_at(0, 0, 0), None);
        assert_eq!(lean.memory_bytes(), ODD.cells() + LUT_LEN);

        let full = build_voxels(&scan, &request(ODD), SITE.0, SITE.1).unwrap();
        assert_eq!(full.values().map(<[f32]>::len), Some(ODD.cells()));
        assert_eq!(full.memory_bytes(), ODD.cells() * 5 + LUT_LEN);
        // Same indices either way: the value plane is a copy, not a different
        // resample.
        assert_eq!(lean.indices(), full.indices());
    }

    /// The two planes say the same thing about every cell: `NaN` exactly where
    /// the index is [`NO_DATA_INDEX`], and never one without the other.
    #[test]
    fn the_two_planes_agree_cell_for_cell() {
        let scan = scan_of(&|az, slant| (az < 140.0 && slant < 80.0).then_some(52.0));
        let grid = build_voxels(&scan, &request(ODD), SITE.0, SITE.1).unwrap();
        let values = grid.values().unwrap();
        let (mut empty, mut filled) = (0, 0);
        for (index, value) in grid.indices().iter().zip(values) {
            if *index == NO_DATA_INDEX {
                empty += 1;
                assert!(value.is_nan(), "no-data cell carries {value}");
            } else {
                filled += 1;
                assert!(value.is_finite(), "data cell carries {value}");
                assert_eq!(*index, grid.value_to_index(*value));
            }
        }
        assert!(
            empty > 0 && filled > 0,
            "precondition: this fixture must produce both, or the loop proves \
             nothing ({empty} empty, {filled} filled)",
        );
    }

    // ── Equality and Debug ──────────────────────────────────────────────────

    /// The reason `PartialEq` is hand-written: the value plane is mostly
    /// `NaN`, and a derived one would make a grid unequal to a byte-identical
    /// copy of itself.
    #[test]
    fn two_identical_grids_compare_equal_through_the_nan_value_plane() {
        let scan = scan_of(&|az, _| (az < 90.0).then_some(48.0));
        let a = build_voxels(&scan, &request(ODD), SITE.0, SITE.1).unwrap();
        let b = build_voxels(&scan, &request(ODD), SITE.0, SITE.1).unwrap();

        assert!(
            a.values().unwrap().iter().any(|v| v.is_nan()),
            "precondition: without a NaN in the value plane this test would \
             pass under a derived PartialEq too, and prove nothing",
        );
        assert_eq!(a, b);
        assert_eq!(a, a.clone());
        // The derive's behaviour, shown rather than described.
        assert!(
            !a.values()
                .unwrap()
                .iter()
                .zip(b.values().unwrap())
                .all(|(x, y)| x == y),
            "an element-wise `==` over the value planes disagrees, which is \
             exactly what `#[derive(PartialEq)]` would have used",
        );

        // And it still discriminates: a different box is a different grid.
        let moved = VoxelRequest {
            half_width_km: 61.0,
            ..request(ODD)
        };
        assert_ne!(a, build_voxels(&scan, &moved, SITE.0, SITE.1).unwrap());
        let lean = VoxelRequest {
            values_wanted: false,
            ..request(ODD)
        };
        assert_ne!(a, build_voxels(&scan, &lean, SITE.0, SITE.1).unwrap());
    }

    /// A grid built by hand, so the equality tests can vary **one** field.
    ///
    /// Every field before the value plane in `eq`'s `&&` chain short-circuits,
    /// and the index plane is a quantisation of the value plane — so a pair
    /// built from two different scans differs on `indices` and never reaches
    /// the value comparison at all. Mutation testing found all three of
    /// `same_values`' arms unreachable from `build_voxels` alone for exactly
    /// that reason.
    fn hand_built(values: Option<Vec<f32>>) -> VoxelGrid {
        let value_range = value_range_for(MomentSlot::Reflectivity);
        VoxelGrid {
            indices: vec![0, 7, 200, 255],
            values,
            lut: colormap_lut(RadarProduct::Reflectivity, value_range),
            shape: VoxelShape {
                nx: 2,
                ny: 2,
                nz: 1,
            },
            x_range_km: (-10.0, 10.0),
            y_range_km: (-10.0, 10.0),
            z_range_km_msl: (0.0, 5.0),
            site: SITE,
            value_range,
            product: RadarProduct::Reflectivity,
            tilt_count: 2,
            widest_tilt_gap_deg: 3.94,
        }
    }

    /// The value plane is compared **bitwise**, its length counts, and having
    /// no plane at all is a state of its own.
    #[test]
    fn the_value_plane_is_compared_bit_for_bit_and_its_absence_is_a_state() {
        let nan = f32::NAN;
        let a = hand_built(Some(vec![nan, -20.0, 45.0, 62.5]));
        assert_eq!(a, hand_built(Some(vec![nan, -20.0, 45.0, 62.5])));

        // Same index plane, different values — the pair `build_voxels` cannot
        // produce.
        let different = hand_built(Some(vec![nan, -20.0, 45.25, 62.5]));
        assert_eq!(
            a.indices(),
            different.indices(),
            "precondition: only the value plane may differ, or this proves \
             nothing about `same_values`",
        );
        assert_ne!(a, different, "a different value plane is a different grid");

        // A shorter plane is a different payload, not a prefix match.
        assert_ne!(a, hand_built(Some(vec![nan, -20.0, 45.0])));

        // Bitwise: two NaNs with different payloads are two different
        // payloads, even though neither equals itself as a float.
        let other_nan = hand_built(Some(vec![
            f32::from_bits(nan.to_bits() ^ 1),
            -20.0,
            45.0,
            62.5,
        ]));
        assert!(other_nan.values().unwrap()[0].is_nan());
        assert_ne!(a, other_nan);

        // No plane at all: equal to another grid with none, unequal to one
        // with a plane, in both directions.
        assert_eq!(hand_built(None), hand_built(None));
        assert_ne!(a, hand_built(None));
        assert_ne!(hand_built(None), a);
    }

    /// `Debug` is a summary, for the reason the sampler's is: `assert_eq!`
    /// reaches for it on failure, and the derive would print 8 MiB.
    #[test]
    fn debug_is_a_summary_rather_than_the_grid() {
        let scan = scan_of(&|az, _| (az < 90.0).then_some(48.0));
        let grid = build_voxels(&scan, &request(ODD), SITE.0, SITE.1).unwrap();
        let text = format!("{grid:?}");
        assert_eq!(text.lines().count(), 1, "{text}");
        assert!(text.len() < 400, "{} chars: {text}", text.len());
        assert!(text.contains("ref"), "{text}");
        assert!(text.contains("11x13x7"), "{text}");

        // The fill count is what two grids most often differ by, so it is the
        // one number in the summary worth checking rather than merely
        // formatting. Counted here rather than trusted.
        let filled = grid
            .indices()
            .iter()
            .filter(|&&i| i != NO_DATA_INDEX)
            .count();
        assert!(
            filled > 0 && filled < ODD.cells(),
            "precondition: a partly filled grid, or the count below cannot \
             discriminate ({filled} of {})",
            ODD.cells(),
        );
        assert_ne!(
            filled,
            ODD.cells() - filled,
            "precondition: filled and empty must differ, or reporting the \
             wrong one of the two would read the same",
        );
        assert!(
            text.contains(&format!("{filled}/{}", ODD.cells())),
            "the summary must report {filled} of {} cells with data: {text}",
            ODD.cells(),
        );
    }

    // ── The ramp ────────────────────────────────────────────────────────────

    /// Affine, and exact both ways for all 255 data indices of all six
    /// moments.
    #[test]
    fn the_ramp_is_affine_and_round_trips_every_data_index() {
        for slot in SLOTS {
            let range = value_range_for(slot);
            let step = f64::from(range.1 - range.0) / 255.0;
            for index in 1..=255u8 {
                let value = ramp_value(range, index);
                assert_eq!(
                    ramp_index(range, value),
                    index,
                    "{slot:?} index {index} -> {value} -> {}",
                    ramp_index(range, value),
                );
                // Affine: the gap to the entry below is one step everywhere,
                // including across index 1 — which is what makes filtering
                // within data exactly linear interpolation of the value.
                let below = ramp_value(range, index - 1);
                assert!(
                    (f64::from(value - below) - step).abs() < step * 1e-4,
                    "{slot:?} step {}->{index} is {} not {step}",
                    index - 1,
                    value - below,
                );
            }
        }
    }

    /// Index 0 is **below** the moment's lowest data level, not on it — so a
    /// real measurement can never read as an absence.
    ///
    /// Index 1 is compared to within a ten-thousandth of a step rather than
    /// exactly, because `value_range` is `f32` and reconstructing
    /// `(lo − step) + step` cancels: ΦDP's bottom level comes back as 1.2e−7
    /// instead of 0. That is four decimal orders below the 1.4° step and eight
    /// below the span, and the round trip through
    /// [`ramp_index`] is still exact — which
    /// `the_ramp_is_affine_and_round_trips_every_data_index` pins separately,
    /// so nothing here is being waved through.
    #[test]
    fn index_zero_is_one_step_below_the_bottom_data_level() {
        for slot in SLOTS {
            let (lo, hi) = data_levels(slot);
            let range = value_range_for(slot);
            let step = (f64::from(hi) - f64::from(lo)) / 254.0;
            assert!(
                (f64::from(ramp_value(range, 1)) - f64::from(lo)).abs() < step * 1e-4,
                "{slot:?}: index 1 must be the bottom data level {lo}, is {}",
                ramp_value(range, 1),
            );
            assert_eq!(
                ramp_value(range, 255),
                hi,
                "{slot:?}: index 255 must be the top data level exactly",
            );
            assert!(
                range.0 < lo,
                "{slot:?}: index 0 ({}) must sit under the bottom data level \
                 ({lo})",
                range.0,
            );
            // And by a whole step, not by a rounding crumb — that is what
            // keeps a real measurement off the no-data index.
            assert!(
                (f64::from(lo) - f64::from(range.0) - step).abs() < step * 1e-4,
                "{slot:?}: index 0 must sit one full step ({step}) below {lo}, \
                 sits {} below",
                f64::from(lo) - f64::from(range.0),
            );
        }
    }

    /// Every raw code of every moment's Level II encoding lands on a data
    /// index, inside the declared span.
    ///
    /// The encodings are written out here rather than read from a fixture:
    /// they are the ICD's, they are what `data_levels` was derived from, and
    /// restating them is the only way this test can disagree with the table.
    #[test]
    fn no_measurement_encodes_as_the_no_data_index() {
        // (slot, scale, offset) for the 8-bit moments; ΦDP is 16-bit and is
        // walked over its own turn instead.
        let encodings = [
            (MomentSlot::Reflectivity, 2.0, 66.0),
            (MomentSlot::Velocity, 2.0, 129.0),
            (MomentSlot::SpectrumWidth, 2.0, 129.0),
            (MomentSlot::DifferentialReflectivity, 16.0, 128.0),
            (MomentSlot::CorrelationCoefficient, 300.0, -60.5),
        ];
        for (slot, scale, offset) in encodings {
            let range = value_range_for(slot);
            let (lo, hi) = data_levels(slot);
            for code in 2..=255u32 {
                let value = ((code as f32) - offset) / scale;
                // Spectrum width shares velocity's encoding but is
                // non-negative; its negative half is not a measurement.
                if slot == MomentSlot::SpectrumWidth && value < 0.0 {
                    continue;
                }
                assert!(
                    value >= lo && value <= hi,
                    "{slot:?} code {code} decodes to {value}, outside the \
                     declared span {lo}..={hi}",
                );
                assert_ne!(
                    ramp_index(range, value),
                    NO_DATA_INDEX,
                    "{slot:?} code {code} ({value}) encodes as no-data",
                );
            }
        }
        // ΦDP over its whole turn, at a resolution finer than its 16-bit
        // encoding's 1/2.8361 of a degree.
        let range = value_range_for(MomentSlot::DifferentialPhase);
        for step in 0..=3600 {
            let value = step as f32 / 10.0;
            assert_ne!(ramp_index(range, value), NO_DATA_INDEX, "PhiDP {value}");
        }
        // And the clamp has teeth: something off either end still lands on a
        // data index rather than being silently reclassified as absent.
        let refl = value_range_for(MomentSlot::Reflectivity);
        assert_eq!(ramp_index(refl, -1000.0), 1);
        assert_eq!(ramp_index(refl, 1000.0), 255);
        for bad in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            assert_eq!(ramp_index(refl, bad), NO_DATA_INDEX, "{bad}");
        }
    }

    /// A value outside the declared span clamps to the nearest **data** level
    /// — never to the no-data index — and the value plane keeps the number the
    /// radar actually reported.
    ///
    /// Found by a fixture that wrote spectrum-width raw codes under 129. The
    /// RDA does not emit those: spectrum width shares velocity's `(2, 129)`
    /// codec, so codes 2…128 decode to a *negative* width, which is not a
    /// measurement, and the ICD's defined range for the moment is 0…63 m/s.
    /// So the case is unreachable from valid Level II — but it is reachable
    /// from a malformed file, and the two planes then say different things
    /// about the same cell **on purpose**: the index plane must land on the
    /// ramp, and the value plane must not launder a bad number into a
    /// plausible one. The one visible consequence is that such a cell paints
    /// the palette's 0 m/s grey where the plan view paints it transparent,
    /// which is one index out of 256 on data that should not exist.
    #[test]
    fn a_value_outside_the_declared_span_clamps_to_the_nearest_data_level() {
        let range = value_range_for(MomentSlot::SpectrumWidth);
        let (lo, hi) = data_levels(MomentSlot::SpectrumWidth);
        // A raw code of 5 through spectrum width's codec.
        let impossible = (5.0 - 129.0) / 2.0;
        assert!(impossible < lo, "precondition: {impossible} is under {lo}");
        assert_eq!(
            ramp_index(range, impossible),
            1,
            "an under-range value takes the bottom data level, not no-data",
        );
        assert_eq!(ramp_index(range, hi + 100.0), 255);
        // And the same on the other five moments, so the clamp is not a
        // spectrum-width special case.
        for slot in SLOTS {
            let range = value_range_for(slot);
            let (lo, hi) = data_levels(slot);
            assert_eq!(ramp_index(range, lo - 1e6), 1, "{slot:?}");
            assert_eq!(ramp_index(range, hi + 1e6), 255, "{slot:?}");
        }
    }

    /// Four of the six steps land exactly on the moment's own quantum, and the
    /// two that do not are recorded rather than rounded away.
    #[test]
    fn the_declared_steps_are_measured() {
        let step = |slot| {
            let (lo, hi) = data_levels(slot);
            (f64::from(hi) - f64::from(lo)) / 254.0
        };
        assert_eq!(step(MomentSlot::Reflectivity), 0.5, "Level II's own 0.5 dB");
        assert_eq!(step(MomentSlot::Velocity), 0.5, "the 0.5 m/s encoding");
        assert_eq!(step(MomentSlot::SpectrumWidth), 0.25);
        assert_eq!(
            step(MomentSlot::DifferentialReflectivity),
            0.0625,
            "1/16 dB"
        );
        // Marginally coarser than their encodings, and far finer than a viewer
        // can distinguish. Pinned so a change to either span is noticed.
        assert!((step(MomentSlot::DifferentialPhase) - 1.417_32).abs() < 1e-5);
        assert!((step(MomentSlot::CorrelationCoefficient) - 0.003_385_8).abs() < 1e-7);
    }

    // ── The colour table ────────────────────────────────────────────────────

    /// All six moments build, all six carry a full table, and all six come
    /// back with data in them — the end-to-end check the single-moment
    /// fixtures cannot make.
    #[test]
    fn every_samplable_moment_builds_a_populated_grid_and_a_full_table() {
        assert_eq!(LUT_LEN, 1024);
        let scan = six_moment_scan();
        for product in SAMPLABLE {
            let req = VoxelRequest {
                product,
                half_width_km: 40.0,
                base_km_msl: 0.5,
                top_km_msl: 4.0,
                ..request(ODD)
            };
            let grid = build_voxels(&scan, &req, SITE.0, SITE.1).unwrap();
            assert_eq!(grid.lut().len(), LUT_LEN, "{}", product.name());
            assert_eq!(grid.product(), product);
            let filled = grid
                .indices()
                .iter()
                .filter(|&&i| i != NO_DATA_INDEX)
                .count();
            assert!(
                filled > 0,
                "{} came back empty, so every per-product assertion below it \
                 would be vacuous",
                product.name(),
            );
            // Every value sits inside the declared span, which is what makes
            // the quantisation declared rather than hoped for.
            let (lo, hi) = grid.value_range();
            for value in grid.values().unwrap().iter().filter(|v| v.is_finite()) {
                assert!(
                    *value >= lo && *value <= hi,
                    "{} read {value} outside {lo}..={hi}",
                    product.name(),
                );
            }
        }
    }

    /// The no-data entry is fully transparent for **every** product — forced,
    /// not inherited, because four of the six palettes hand back an opaque
    /// colour at the bottom of their ramp and an opaque no-data index paints
    /// the entire outside of the volume.
    #[test]
    fn the_no_data_entry_is_transparent_for_every_product() {
        for product in SAMPLABLE {
            let range = value_range_for(samplable(product).unwrap());
            let lut = colormap_lut(product, range);
            assert_eq!(
                &lut[0..4],
                &[0, 0, 0, 0],
                "{} entry 0 must be transparent",
                product.name(),
            );
        }
        // The precondition that makes the forcing necessary rather than
        // decorative: these four palettes are opaque at the ramp's bottom.
        for product in [
            RadarProduct::Velocity,
            RadarProduct::DifferentialReflectivity,
            RadarProduct::DifferentialPhase,
            RadarProduct::CorrelationCoefficient,
        ] {
            let range = value_range_for(samplable(product).unwrap());
            let (_, _, _, alpha) = get_color_for_value(product, ramp_value(range, 0));
            assert_ne!(
                alpha,
                0,
                "{} paints its ramp bottom opaque, which is why entry 0 is \
                 forced",
                product.name(),
            );
        }
    }

    /// The table comes from `get_color_for_value`, not from
    /// `LegendScale::thresholds`. Four things that would break, each shown.
    #[test]
    fn the_table_is_the_palette_function_not_its_stops() {
        // 1. `extract_scale` filters non-finite stops, so ZDR's NEG_INFINITY
        //    floor — the stop colouring everything under −2 dB — is absent
        //    from `thresholds` entirely.
        let zdr = get_legend_scale(RadarProduct::DifferentialReflectivity);
        assert!(
            zdr.thresholds.iter().all(|(v, _)| *v >= -2.0),
            "precondition: the ZDR stops start at −2 dB, so a table built \
             from them has no colour under it",
        );
        let range = value_range_for(MomentSlot::DifferentialReflectivity);
        let lut = colormap_lut(RadarProduct::DifferentialReflectivity, range);
        // Index 1 is −7.875 dB, well under the lowest surviving stop.
        assert_eq!(&lut[4..8], &[66, 66, 66, 180], "ZDR's floor colour");

        // 2. The per-product transparency floor lives only in the function:
        //    reflectivity under 0 dBZ is transparent, and no stop says so.
        let refl_range = value_range_for(MomentSlot::Reflectivity);
        let refl = colormap_lut(RadarProduct::Reflectivity, refl_range);
        let below_zero = ramp_index(refl_range, -0.5);
        assert_eq!(refl[usize::from(below_zero) * 4 + 3], 0, "−0.5 dBZ");
        assert_ne!(refl[usize::from(ramp_index(refl_range, 0.5)) * 4 + 3], 0);
        assert!(
            get_legend_scale(RadarProduct::Reflectivity)
                .thresholds
                .iter()
                .any(|(v, _)| *v == 0.0),
            "precondition: the stops *do* carry 0 dBZ, with a colour — so a \
             table built from them would paint everything under it opaque",
        );

        // 3. Velocity's stops are in mph in two separate tables; the function
        //    is the only thing that knows the input is m/s.
        let vel_range = value_range_for(MomentSlot::Velocity);
        let vel = colormap_lut(RadarProduct::Velocity, vel_range);
        let inbound = usize::from(ramp_index(vel_range, -30.0)) * 4;
        let outbound = usize::from(ramp_index(vel_range, 30.0)) * 4;
        assert!(
            vel[inbound + 1] > vel[inbound] && vel[outbound] > vel[outbound + 1],
            "inbound must be green and outbound red; got {:?} and {:?}",
            &vel[inbound..inbound + 4],
            &vel[outbound..outbound + 4],
        );

        // 4. Every entry is exactly the function's answer: the colour
        //    verbatim, the alpha scaled by the product's own 3D transparency
        //    profile — and by nothing else, so the profile can only ever make
        //    an entry *more* transparent than its plan-view colour.
        for product in SAMPLABLE {
            let range = value_range_for(samplable(product).unwrap());
            let lut = colormap_lut(product, range);
            for index in 1..=255u8 {
                let value = ramp_value(range, index);
                let (r, g, b, a) = get_color_for_value(product, value);
                let scaled = (f32::from(a) * volume_alpha_scale(product, value)).round() as u8;
                let at = usize::from(index) * 4;
                assert_eq!(
                    &lut[at..at + 4],
                    &[r, g, b, scaled],
                    "{} entry {index}",
                    product.name(),
                );
                assert!(
                    lut[at + 3] <= a,
                    "{} entry {index}: the 3D profile must never exceed the \
                     palette's own alpha",
                    product.name(),
                );
            }
        }
    }

    /// A non-gradient scale's table must be consumed `NEAREST`, or a blend
    /// names a step the scale does not define.
    #[test]
    fn the_table_filter_is_nearest_only_for_a_non_gradient_scale() {
        let scan = six_moment_scan();
        for product in SAMPLABLE {
            let req = VoxelRequest {
                product,
                ..request(ODD)
            };
            let grid = build_voxels(&scan, &req, SITE.0, SITE.1).unwrap();
            let want = if product == RadarProduct::SpectrumWidth {
                LutFilter::Nearest
            } else {
                LutFilter::Linear
            };
            assert_eq!(grid.lut_filter(), want, "{}", product.name());
            assert_eq!(
                grid.lut_filter() == LutFilter::Linear,
                get_legend_scale(product).is_gradient,
                "the filter is derived from the scale, never stored",
            );
        }
        // The rule exists for the categorical case, which the sampler refuses
        // — stated here so the reason survives if spectrum width's scale ever
        // becomes a gradient.
        assert_eq!(
            samplable(RadarProduct::HydrometeorClassification),
            None,
            "HHC is the scale where a blended step would be a wrong category, \
             and it is not a moment",
        );
        assert!(!get_legend_scale(RadarProduct::HydrometeorClassification).is_gradient);
    }

    // ── The boundary, which is the whole point of the encoding ──────────────

    /// What a `Linear` fetch of an `R8Unorm` texture returns between two
    /// texels: the hardware normalises each to [0, 1], interpolates, and hands
    /// back a float, which the shader scales by 255 to index the table.
    fn fetched_index(a: u8, b: u8, t: f64) -> f64 {
        f64::from(a) * (1.0 - t) + f64::from(b) * t
    }

    fn ramp_value_at(range: (f32, f32), index: f64) -> f64 {
        f64::from(range.0) + (f64::from(range.1) - f64::from(range.0)) * index / 255.0
    }

    fn alpha_at(lut: &[u8], index: f64) -> u8 {
        lut[(index.round() as usize).min(255) * 4 + 3]
    }

    /// **The test the encoding decision exists for.** A sharp echo edge, and
    /// the filtered result across it — fading out rather than jumping to an
    /// opaque middle.
    ///
    /// The comparison is against the *rejected* encoding, computed here rather
    /// than described, because "bottom-of-ramp is better" is only a claim
    /// until both are evaluated over the same edge.
    #[test]
    fn an_echo_edge_fades_instead_of_fabricating_a_mid_value() {
        // A 65 dBZ core with a hard azimuthal and radial edge: outside it the
        // radar looked and saw nothing (raw code 0, below threshold), which is
        // no-data, not a low value.
        let scan = scan_of(&|az, slant| {
            ((40.0..80.0).contains(&az) && (20.0..50.0).contains(&slant)).then_some(65.0)
        });
        let shape = VoxelShape {
            nx: 64,
            ny: 64,
            nz: 24,
        };
        let req = VoxelRequest {
            half_width_km: 60.0,
            base_km_msl: 0.5,
            top_km_msl: 8.0,
            ..request(shape)
        };
        let grid = build_voxels(&scan, &req, SITE.0, SITE.1).unwrap();
        let range = grid.value_range();

        // Find a real edge in the built grid: two x-adjacent cells, one with
        // data and one without. A fixture where every voxel has data cannot
        // test this at all, which is why the field above has an edge in it.
        let mut edge = None;
        for iz in 0..shape.nz {
            for iy in 0..shape.ny {
                for ix in 0..shape.nx - 1 {
                    let a = grid.index_at(ix, iy, iz).unwrap();
                    let b = grid.index_at(ix + 1, iy, iz).unwrap();
                    if a != NO_DATA_INDEX && b == NO_DATA_INDEX && a > 150 {
                        edge = Some((a, b));
                    }
                }
            }
        }
        let (data, empty) = edge.expect("the fixture must contain a strong echo edge");
        assert_eq!(empty, NO_DATA_INDEX);
        // Measured, so the numbers below are numbers and not a description:
        // the 65 dBZ core resamples to index 195 exactly, which is
        // −32.5 + 195 × 0.5.
        assert_eq!((data, grid.index_to_value(data)), (195, 65.0));

        // ── ours: bottom of ramp ──
        let mut previous = f64::INFINITY;
        let mut first_transparent = None;
        let data_value = ramp_value_at(range, f64::from(data));
        for step in 0..=64 {
            let t = f64::from(step) / 64.0;
            let index = fetched_index(data, empty, t);
            let value = ramp_value_at(range, index);
            assert!(
                value <= previous,
                "the fetched value must fall monotonically toward the ramp \
                 bottom; at t={t} it rose to {value} from {previous}",
            );
            assert!(
                value <= data_value + 1e-9,
                "nothing on the boundary may be stronger than the echo it \
                 borders: {value} > {data_value} at t={t}",
            );
            previous = value;
            if first_transparent.is_none() && alpha_at(grid.lut(), index) == 0 {
                first_transparent = Some(t);
            }
        }
        let faded_at = first_transparent.expect("the boundary must reach transparency");
        assert!(
            faded_at < 1.0,
            "alpha must reach zero *before* the no-data neighbour, or the \
             fade is a single step at the very end; reached it at t={faded_at}",
        );
        assert!(
            faded_at < 0.75,
            "the fade should be a real fraction of the edge, not a rounding \
             artefact; reached transparency only at t={faded_at}",
        );
        // Measured: index 195 × (1 − t) drops to 64 — the top of the
        // transparent band, −0.5 dBZ — at t = 43/64, so the last third of the
        // way to the empty neighbour is already invisible.
        assert_eq!(faded_at, 43.0 / 64.0);

        // ── the rejected encoding: index 0 out of band ──
        //
        // Data indices 1..=255 span the palette's own 0..95 dBZ and 0 means
        // "no data", off the ramp. Same edge, same filter.
        let (oob_lo, oob_hi) = (0.0f64, 95.0f64);
        let oob_value = |index: f64| oob_lo + (index - 1.0) / 254.0 * (oob_hi - oob_lo);
        let oob_data = (1.0 + (data_value - oob_lo) / (oob_hi - oob_lo) * 254.0).round();
        let oob_half = fetched_index(oob_data as u8, 0, 0.5);
        let fabricated = oob_value(oob_half);
        assert!(
            fabricated > 25.0,
            "the rejected encoding is supposed to fabricate a mid-dBZ shell; \
             halfway across the edge it reads {fabricated} dBZ",
        );
        // Fully opaque, because that index is an ordinary data index.
        assert_ne!(
            get_color_for_value(RadarProduct::Reflectivity, fabricated as f32).3,
            0,
            "and the alpha floor cannot rescue it: the floor applies to the \
             fetched index, and {fabricated} dBZ is a perfectly ordinary echo",
        );

        // Ours, at the same place on the same edge.
        let ours_half = ramp_value_at(range, fetched_index(data, empty, 0.5));
        assert!(
            ours_half < fabricated - 10.0,
            "bottom-of-ramp must read materially weaker halfway across the \
             edge than the out-of-band encoding: {ours_half} dBZ against \
             {fabricated} dBZ",
        );

        // The whole comparison as three numbers, so a change to any of them
        // is a change to the decision rather than to a wording.
        assert_eq!(
            (
                (data_value * 100.0).round(),
                (ours_half * 100.0).round(),
                (fabricated * 100.0).round(),
            ),
            (6500.0, 1625.0, 3235.0),
            "65.00 dBZ core; halfway across its edge bottom-of-ramp reads \
             16.25 dBZ and fades out a third of the way further on, while the \
             rejected out-of-band encoding reads 32.35 dBZ at full opacity and \
             only vanishes on the empty voxel itself",
        );
    }

    /// **What the shipped encoding actually costs the five non-fading
    /// moments, measured rather than argued.**
    ///
    /// The module doc used to claim bottom-of-ramp was "strictly better" than
    /// an out-of-band index for every moment, on the reasoning that an opaque
    /// *end-of-ramp* colour beats an opaque *mid-ramp* one. **That reasoning
    /// is wrong for a bidirectional or centred palette**, and this test is why
    /// the claim was corrected: for velocity and ZDR the out-of-band ramp's
    /// midpoint *is* the palette's neutral, while our ramp's bottom is its
    /// saturated extreme — so the shipped encoding paints the **more**
    /// alarming halo, not the less.
    ///
    /// Each row is a half-edge fetch: a plausible echo value adjacent to
    /// nothing, filtered at `t = 0.5`, decoded under both encodings.
    ///
    /// The out-of-band ramp is data indices 1..=255 over the **palette's own**
    /// range, with 0 reserved off it. Spanning the palette rather than the
    /// moment's physical range is what an out-of-band design would actually
    /// do, and the distinction is the whole comparison: widening below the
    /// palette floor is *our* construction's requirement — index 0 has to be a
    /// value — and an encoding that reserves 0 has no such need. Over the same
    /// span the two are the identical mapping for every `i >= 1`, so comparing
    /// them there would measure nothing.
    ///
    /// Shipping bottom-of-ramp is still right — the out-of-band ramp cannot
    /// represent the moment's floor at all, so it clamps real measurements
    /// outside the palette range — but the honest summary is "no worse, and a
    /// wash or slightly worse per moment", not "strictly better".
    #[test]
    fn the_half_edge_costs_of_both_encodings_are_measured_per_moment() {
        let rows: Vec<(&str, f64, f64)> = SLOTS
            .iter()
            .zip(SAMPLABLE)
            .map(|(&slot, product)| {
                // A plausible echo for the moment, at a hard edge.
                let echo: f32 = match slot {
                    MomentSlot::Reflectivity => 65.0,
                    MomentSlot::Velocity => 30.0,
                    MomentSlot::SpectrumWidth => 4.0,
                    MomentSlot::DifferentialReflectivity => 1.5,
                    MomentSlot::DifferentialPhase => 60.0,
                    MomentSlot::CorrelationCoefficient => 0.98,
                };
                let range = value_range_for(slot);
                let shipped = ramp_value_at(
                    range,
                    fetched_index(ramp_index(range, echo), NO_DATA_INDEX, 0.5),
                );

                // The rejected encoding, over the palette's own range.
                let legend = get_legend_scale(product);
                let (lo, hi) = (f64::from(legend.min_value), f64::from(legend.max_value));
                let oob_index = (1.0 + (f64::from(echo) - lo) / (hi - lo) * 254.0).round();
                let oob_half = fetched_index(oob_index as u8, 0, 0.5);
                let out_of_band = lo + (oob_half - 1.0) / 254.0 * (hi - lo);

                let round3 = |v: f64| (v * 1000.0).round() / 1000.0;
                (product.code(), round3(shipped), round3(out_of_band))
            })
            .collect();

        assert_eq!(
            rows,
            vec![
                // Reflectivity: unambiguously better, and the only moment with
                // a transparent band to fade into.
                ("ref", 16.25, 32.352),
                // Velocity: ours is a −17 m/s *inbound* shell around every
                // outbound couplet edge; the out-of-band ramp's midpoint is
                // near zero, which the palette paints dark.
                ("vel", -17.0, -3.119),
                // Spectrum width and ΦDP: a wash, sub-metre and sub-degree.
                ("sw", 1.875, 1.985),
                // ZDR: ours saturates the negative extreme, theirs sits near
                // the neutral 0 dB.
                ("zdr", -3.219, -0.258),
                ("phi", 29.055, 29.203),
                // ρHV: ours paints a 0.588 shell — squarely in the
                // debris/non-meteorological band — around every echo edge and
                // around the whole volume boundary.
                ("rho", 0.588, 0.714),
            ],
            "half-edge fetch, shipped vs out-of-band, per moment",
        );

        // When this table was first measured, all five non-reflectivity
        // moments were fully opaque at every data entry — the half-edge fetch
        // painted at full strength, and the measurement's conclusion was that
        // WP-I had to supply the fade itself. It now has: every moment's table
        // carries either a transparent band (shaped to its own physics by
        // `volume_alpha_scale`) or a flat translucency (ΦDP), so the wrong
        // colours tabulated above land at reduced or zero alpha wherever the
        // profile says the value is background.
        for (slot, product) in SLOTS.iter().zip(SAMPLABLE) {
            if product == RadarProduct::Reflectivity {
                continue;
            }
            let range = value_range_for(*slot);
            let lut = colormap_lut(product, range);
            let see_through = lut
                .chunks_exact(4)
                .skip(1)
                .filter(|entry| entry[3] <= SEE_THROUGH_ALPHA_CEILING)
                .count();
            assert!(
                see_through >= 16,
                "{}: only {see_through} see-through entries — the solid-block \
                 failure the profiles exist to prevent",
                product.name(),
            );
        }
    }

    /// How wide the **bottom** fade is, per product — the number that anchors
    /// the march's skip threshold.
    ///
    /// **Recorded, not asserted to be large**, because since the per-product
    /// transparency profiles landed this is no longer the number that decides
    /// drawability — [`VoxelGrid::clear_indices`] is. A diverging moment's
    /// see-through band sits mid-ramp, so its bottom band is honestly 0: the
    /// ramp's bottom is its saturated extreme and must stay opaque. Only the
    /// two moments whose *floor* is background — reflectivity (palette's own
    /// quarter-ramp fade) and spectrum width (the profile's laminar-flow
    /// fade) — have one.
    #[test]
    fn the_fade_band_is_measured_per_product() {
        let scan = six_moment_scan();
        let mut measured = Vec::new();
        for product in SAMPLABLE {
            let req = VoxelRequest {
                product,
                ..request(ODD)
            };
            let grid = build_voxels(&scan, &req, SITE.0, SITE.1).unwrap();
            measured.push((product.code(), grid.fade_band()));
        }
        assert_eq!(
            measured,
            vec![
                // −32.5 … −0.5 dBZ is transparent: a quarter of the ramp.
                ("ref", 64),
                // The ramp's bottom is −64 m/s, the strongest inbound air —
                // velocity's see-through band is mid-ramp, measured by
                // `the_default_transparency_profile_is_measured_per_product`.
                ("vel", 0),
                // 0 … 2 m/s: the profile's laminar-flow floor.
                ("sw", 9),
                // Bottom = −7.9 dB, a saturated hail extreme: opaque.
                ("zdr", 0),
                // Flat translucency, no transparent band at all.
                ("phi", 0),
                // Bottom = lowest ρHV, the most non-meteorological: opaque.
                ("rho", 0),
            ],
            "the fade band per product",
        );

        // What that means for reflectivity, in the units that matter.
        let grid = build_voxels(&scan, &request(ODD), SITE.0, SITE.1).unwrap();
        assert_eq!(
            grid.index_to_value(grid.fade_band()),
            -0.5,
            "the top of the transparent band is the last level under 0 dBZ",
        );
        assert!(
            f64::from(grid.fade_band()) / 255.0 > 0.24,
            "a quarter of the whole ramp",
        );

        // The two ends of the measurement, which no product's palette reaches
        // and only a hand-built table can: a table opaque from index 1 has no
        // band, and one transparent throughout fades over the whole ramp.
        let mut opaque = hand_built(None);
        opaque.lut = vec![255; LUT_LEN];
        opaque.lut[3] = 0;
        assert_eq!(opaque.fade_band(), 0);
        let mut clear = hand_built(None);
        clear.lut = vec![0; LUT_LEN];
        assert_eq!(clear.fade_band(), u8::MAX);
    }

    /// The per-product 3D transparency profile, pinned at physical landmarks.
    ///
    /// Each row is a rationale made testable: the value named is *why* the
    /// profile has its shape, so a change to any constant in
    /// `volume_alpha_profile` fails here with the physics in the message
    /// rather than as an index diff. The last block is the drawability
    /// measurement the renderer's solid-block gate reads.
    #[test]
    fn the_default_transparency_profile_is_measured_per_product() {
        let alpha = |product: RadarProduct, value: f32| {
            let range = value_range_for(samplable(product).unwrap());
            let lut = colormap_lut(product, range);
            lut[usize::from(ramp_index(range, value)) * 4 + 3]
        };
        // "Solid" means the palette's own plan-view alpha, verbatim — several
        // 2D palettes are themselves translucent in places, and the profile
        // may only scale them down, never up.
        let palette_alpha = |product: RadarProduct, value: f32| {
            let range = value_range_for(samplable(product).unwrap());
            get_color_for_value(product, ramp_value(range, ramp_index(range, value))).3
        };
        let solid = |product: RadarProduct, value: f32, what: &str| {
            assert_eq!(
                alpha(product, value),
                palette_alpha(product, value),
                "{what}: full plan-view strength",
            );
            assert!(alpha(product, value) > 0, "{what}: visible at all");
        };

        // Velocity: calm air is invisible, cores are solid — in both signs.
        assert_eq!(alpha(RadarProduct::Velocity, 0.0), 0, "calm air");
        assert_eq!(alpha(RadarProduct::Velocity, 3.5), 0, "ambient drift");
        assert_eq!(
            alpha(RadarProduct::Velocity, -3.5),
            0,
            "ambient drift, inbound"
        );
        solid(RadarProduct::Velocity, 30.0, "an outbound core");
        solid(RadarProduct::Velocity, -30.0, "an inbound core");
        let mid = alpha(RadarProduct::Velocity, 10.0);
        assert!(
            mid > 0 && mid < palette_alpha(RadarProduct::Velocity, 10.0),
            "the fade between drift and core is a fade, not a step: {mid}",
        );

        // Spectrum width: laminar flow is invisible, turbulence is solid.
        assert_eq!(alpha(RadarProduct::SpectrumWidth, 1.0), 0, "laminar flow");
        solid(RadarProduct::SpectrumWidth, 10.0, "turbulence");

        // ZDR: rain's own background is invisible; big-drop columns and hail
        // cores are solid on their opposite sides of it.
        assert_eq!(
            alpha(RadarProduct::DifferentialReflectivity, 0.25),
            0,
            "small drops"
        );
        solid(RadarProduct::DifferentialReflectivity, 4.0, "a ZDR column");
        solid(RadarProduct::DifferentialReflectivity, -3.5, "a hail core");

        // ρHV: uniform precipitation is invisible — the profile inverts,
        // because this moment's background is the TOP of its scale. Debris
        // reads solid at the palette's own (translucent) strength.
        assert_eq!(
            alpha(RadarProduct::CorrelationCoefficient, 1.0),
            0,
            "pure rain"
        );
        assert_eq!(alpha(RadarProduct::CorrelationCoefficient, 0.99), 0, "rain");
        let (r, g, b, debris_2d) = get_color_for_value(RadarProduct::CorrelationCoefficient, 0.5);
        let _ = (r, g, b);
        assert_eq!(
            alpha(RadarProduct::CorrelationCoefficient, 0.5),
            debris_2d,
            "a debris signature keeps its full plan-view alpha",
        );
        assert!(
            alpha(RadarProduct::CorrelationCoefficient, 0.85)
                > alpha(RadarProduct::CorrelationCoefficient, 0.95),
            "alpha must rise as ρHV falls away from rain",
        );

        // ΦDP: flat translucency — a cumulative, site-offset moment has no
        // honest background band, so no value is favoured over another.
        let phi_alphas: Vec<u8> = {
            let range = value_range_for(MomentSlot::DifferentialPhase);
            colormap_lut(RadarProduct::DifferentialPhase, range)
                .chunks_exact(4)
                .skip(1)
                .map(|e| e[3])
                .collect()
        };
        let phi_max = *phi_alphas.iter().max().unwrap();
        assert!(
            phi_max <= 128,
            "ΦDP must stay translucent everywhere: max alpha {phi_max}",
        );
        assert!(
            phi_alphas.iter().all(|a| *a > 0),
            "…but visible everywhere it is measured: no value band is favoured",
        );

        // Reflectivity: bit-exact identity with the palette — the reference
        // look every other profile is measured against.
        {
            let range = value_range_for(MomentSlot::Reflectivity);
            let lut = colormap_lut(RadarProduct::Reflectivity, range);
            for index in 1..=255u8 {
                let (_, _, _, a) =
                    get_color_for_value(RadarProduct::Reflectivity, ramp_value(range, index));
                assert_eq!(
                    lut[usize::from(index) * 4 + 3],
                    a,
                    "reflectivity entry {index}"
                );
            }
        }

        // The drawability number the solid-block gate reads, per product, and
        // the tables' alpha ceiling for context. Every palette's plan-view
        // maximum is 180 — the radar layer's own translucency convention — so
        // 180 is also the ceiling here; ΦDP's 63 is that ceiling times its
        // flat 0.35, which puts its whole 255-entry ramp under the
        // see-through bar: translucent everywhere is the other honest way not
        // to be a wall.
        let scan = six_moment_scan();
        let mut measured = Vec::new();
        for product in SAMPLABLE {
            let req = VoxelRequest {
                product,
                ..request(ODD)
            };
            let grid = build_voxels(&scan, &req, SITE.0, SITE.1).unwrap();
            let max_alpha = grid
                .lut()
                .chunks_exact(4)
                .skip(1)
                .map(|e| e[3])
                .max()
                .unwrap();
            measured.push((product.code(), grid.see_through_indices(), max_alpha));
        }
        assert_eq!(
            measured,
            vec![
                ("ref", 64, 180),
                ("vel", 41, 180),
                ("sw", 18, 180),
                ("zdr", 53, 180),
                ("phi", 255, 63),
                ("rho", 35, 180),
            ],
            "see-through data entries and max data alpha, per product",
        );
    }

    /// The sampler's velocity fold guard rides into the voxel grid unchanged.
    ///
    /// A field folding at ±24.5 m/s with a hard seam: without the guard,
    /// blends across the seam invent every speed between the endpoints —
    /// including calm air — and the voxel grid stores the inventions. The
    /// guard (armed per rung by `Blend::folds_at_measured_limit`, applied
    /// across gates and across tilts) answers with one endpoint or the other,
    /// so **every** valued cell must read exactly ±24.5. The grid samples
    /// through `Column::at_height_km` with no fold logic of its own, which is
    /// what this pins: nobody may give the voxel path its own sampling that
    /// forgets the guard.
    #[test]
    fn the_velocity_fold_guard_rides_into_the_voxel_grid() {
        let seam_km = 10.0;
        let field: Field<'_> = &move |_az, slant| Some(if slant < seam_km { 24.5 } else { -24.5 });
        let scan = Scan::new(
            vcp(&[0.5, 4.5]),
            vec![
                vel_sweep(
                    2,
                    HIGH_DEG,
                    &wrapped_azimuths(360, 211.0),
                    HIGH_GATES,
                    field,
                ),
                vel_sweep(1, LOW_DEG, &wrapped_azimuths(720, 293.5), LOW_GATES, field),
            ],
        );
        let req = VoxelRequest {
            product: RadarProduct::Velocity,
            half_width_km: 20.0,
            top_km_msl: 4.0,
            shape: VoxelShape {
                nx: 64,
                ny: 64,
                nz: 16,
            },
            ..request(ODD)
        };
        let grid = build_voxels(&scan, &req, SITE.0, SITE.1).expect("velocity builds");

        let (mut inbound, mut outbound) = (0usize, 0usize);
        for z in 0..16 {
            for y in 0..64 {
                for x in 0..64 {
                    let Some(v) = grid.value_at(x, y, z).filter(|v| v.is_finite()) else {
                        continue;
                    };
                    assert!(
                        v == 24.5 || v == -24.5,
                        "cell ({x},{y},{z}) reads {v} m/s across a seam whose two \
                         sides measured only ±24.5 — a blend crossed the fold",
                    );
                    if v > 0.0 {
                        outbound += 1;
                    } else {
                        inbound += 1;
                    }
                }
            }
        }
        assert!(
            inbound > 100 && outbound > 100,
            "precondition: both sides of the seam must be in the grid \
             (inbound {inbound}, outbound {outbound}), or nothing straddled",
        );
    }

    /// Differential phase is circular, so the two ends of its ramp are the
    /// same measurement and a linear filter across the seam returns the
    /// opposite phase. Named, measured, and left alone.
    #[test]
    fn the_wrapping_moment_is_named_and_its_seam_error_is_measured() {
        let scan = six_moment_scan();
        for product in SAMPLABLE {
            let req = VoxelRequest {
                product,
                ..request(ODD)
            };
            let grid = build_voxels(&scan, &req, SITE.0, SITE.1).unwrap();
            assert_eq!(
                grid.wraps(),
                product == RadarProduct::DifferentialPhase,
                "{}",
                product.name(),
            );
        }

        // The seam: 1° and 359° are 2° apart on the circle. Filtered halfway
        // between their indices the fetch reads 180°, the opposite phase — an
        // error of 180°, the worst there is.
        let range = value_range_for(MomentSlot::DifferentialPhase);
        let (a, b) = (ramp_index(range, 1.0), ramp_index(range, 359.0));
        let middle = ramp_value_at(range, fetched_index(a, b, 0.5));
        assert!(
            (middle - 180.0).abs() < 1.5,
            "a fetch across the PhiDP seam reads {middle}, where the truth is \
             0 / 360",
        );
    }

    // ── The status the grid drops, stated ───────────────────────────────────

    /// Every non-`Value` status collapses to one index. The grid carries no
    /// status plane — a raymarcher has no use for one — so "below the lowest
    /// beam" and "range folded" are the same byte here, and a hover readout
    /// that needs the distinction must ask the sampler, not the grid.
    #[test]
    fn every_reason_for_no_value_collapses_to_the_one_index() {
        let range = value_range_for(MomentSlot::Reflectivity);
        for status in [
            SampleStatus::BelowThreshold,
            SampleStatus::RangeFolded,
            SampleStatus::BelowLowestBeam,
            SampleStatus::AboveVolume,
            SampleStatus::BeyondRange,
            SampleStatus::NoCoverage,
        ] {
            let sample = Sample::missing(status);
            assert_eq!(sample.value(), None, "{status:?}");
            assert_eq!(ramp_index(range, sample.value_or_nan()), NO_DATA_INDEX);
        }
    }

    // ── The wire codec ──────────────────────────────────────────────────────

    /// Where the three trailing planes' length prefixes sit in an encoded
    /// grid: after the 104-byte header, then after each preceding plane.
    ///
    /// Written out here rather than taken from the encoder, so a layout change
    /// that moved a field has to be made in both places — the mutations these
    /// offsets support are the whole point of the tests below, and an offset
    /// derived from the code under test would follow it wherever it went.
    /// `the_length_prefixes_are_where_the_tests_think_they_are` checks them.
    const HEADER_BYTES: usize = 4 + 2 + 2 + 3 * 4 + 3 * 16 + 16 + 8 + 4 + 8;
    /// The first axis of the shape, so a test can plant an unsupported one.
    const SHAPE_AT: usize = 4 + 2 + 2;
    const LUT_LEN_AT: usize = HEADER_BYTES;
    const INDEX_LEN_AT: usize = LUT_LEN_AT + 4 + LUT_LEN;

    fn value_len_at(cells: usize) -> usize {
        INDEX_LEN_AT + 4 + cells
    }

    fn prefix_at(bytes: &[u8], at: usize) -> u32 {
        u32::from_le_bytes(bytes[at..at + 4].try_into().expect("four bytes"))
    }

    /// A grid with a real echo edge in it, on the deliberately-asymmetric
    /// [`ODD`] shape, and with the value plane present.
    fn wire_fixture() -> VoxelGrid {
        let scan = scan_of(&|az, slant| (az < 120.0 && slant < 90.0).then_some(48.0));
        build_voxels(&scan, &request(ODD), SITE.0, SITE.1).expect("the fixture grid builds")
    }

    /// The "no value plane" encoding is unambiguous only because a supported
    /// shape has at least one cell — otherwise `0` would mean both "absent"
    /// and "as many values as this grid has cells".
    ///
    /// A claim about [`VoxelShape::is_supported`] rather than about the codec,
    /// which is why it is asserted over the boundary and the named shapes
    /// rather than over one fixture.
    #[test]
    fn a_supported_shape_always_has_a_cell_so_an_absent_plane_is_unambiguous() {
        let smallest = VoxelShape {
            nx: 1,
            ny: 1,
            nz: 1,
        };
        for shape in [smallest, ODD, WASM_SHAPE, MOBILE_SHAPE, DESKTOP_SHAPE] {
            assert!(shape.is_supported(), "{shape:?}");
            assert!(
                shape.cells() >= 1,
                "{shape:?} is supported but has no cells, so an absent value \
                 plane and a full one encode to the same four bytes",
            );
        }
        // And the reason it holds: a zero axis is not supported in the first
        // place, on any of the three.
        for zeroed in [
            VoxelShape { nx: 0, ..smallest },
            VoxelShape { ny: 0, ..smallest },
            VoxelShape { nz: 0, ..smallest },
        ] {
            assert!(!zeroed.is_supported(), "{zeroed:?}");
            assert_eq!(zeroed.cells(), 0);
        }
    }

    /// The three offsets the mutation tests below index by are the three the
    /// encoder actually wrote.
    ///
    /// Every refusal test plants a value at one of them, so an offset that had
    /// drifted would leave those tests corrupting a byte of some other field
    /// and passing for the wrong reason — the classic way a suite of negative
    /// assertions goes green while testing nothing.
    #[test]
    fn the_length_prefixes_are_where_the_tests_think_they_are() {
        let grid = wire_fixture();
        let bytes = grid.to_bytes();
        let cells = ODD.cells();
        assert_eq!(prefix_at(&bytes, SHAPE_AT), ODD.nx as u32);
        assert_eq!(prefix_at(&bytes, SHAPE_AT + 4), ODD.ny as u32);
        assert_eq!(prefix_at(&bytes, SHAPE_AT + 8), ODD.nz as u32);
        assert_eq!(prefix_at(&bytes, LUT_LEN_AT), LUT_LEN as u32);
        assert_eq!(prefix_at(&bytes, INDEX_LEN_AT), cells as u32);
        assert_eq!(prefix_at(&bytes, value_len_at(cells)), cells as u32);
        assert_eq!(bytes.len(), value_len_at(cells) + 4 + cells * 4);
    }

    /// The version this layout ships is **1**, and it sits where a decoder
    /// from another build reads it — as does the magic, `RDVX` by literal.
    ///
    /// `a_malformed_grid_payload_is_refused_rather_than_misread` plants
    /// `0xFF 0xFF` in the version and watches the decode refuse, which pins
    /// that *a* version check exists — not *which* version ships. Both ends of
    /// this codec move together, so every other test here round-trips through
    /// one build and passes whatever the constant says; the constant is only
    /// load-bearing *between* builds.
    ///
    /// Between builds is where it is the only defence. `rustdar-web`'s
    /// page/worker handshake is `build_token = version/PROTOCOL_VERSION/
    /// GITHUB_SHA`, and `GITHUB_SHA` is absent outside CI, so it degrades to
    /// `…/dev` and a stale worker shares a token with a fresh page. `RDVX` has
    /// never been bumped, so the exposure is the *first* bump being forgotten:
    /// a layout change that reorders two same-width fields — the three `f64`
    /// axis ranges, or `site` against them — round-trips perfectly through its
    /// own build's codec, so the stale worker would decode a fresh payload
    /// into the new field order and raymarch a volume with its axes swapped.
    ///
    /// The magic is here for the same reason at lower stakes. The relabel loop
    /// in that test pins `RDVX` only against `RDRI` and `RDXS`, its two
    /// port-mates; any *unused* four bytes would have stayed green, and the
    /// far end of the port has no matching constant to move with it. A changed
    /// magic is at least a clean refusal rather than a misparse.
    ///
    /// Byte 4 is where the version starts because [`to_bytes`](VoxelGrid::to_bytes)
    /// writes [`MAGIC`] and then it — the same reading [`SHAPE_AT`] is built
    /// on. Mirrors `render_input`'s and `xsect`'s tests of the same name.
    #[test]
    fn the_format_version_is_the_one_this_layout_ships() {
        assert_eq!(FORMAT_VERSION, 1);
        let bytes = wire_fixture().to_bytes();
        assert_eq!(&bytes[..4], b"RDVX", "the magic moved");
        assert_eq!(
            u16::from_le_bytes([bytes[4], bytes[5]]),
            1,
            "the version is not where a decoder from another build looks for it",
        );
    }

    /// A real grid survives the wire, for every moment, with and without the
    /// value plane.
    ///
    /// This is what [`VoxelGrid`]'s hand-written `PartialEq` was written for:
    /// the value plane is `NaN` wherever the radar did not reach, which on a
    /// cube over a cone is most of it, and under derived semantics
    /// `assert_eq!` here would fail on a byte-identical payload with nothing
    /// in the message saying why.
    #[test]
    fn a_grid_round_trips_through_its_wire_form() {
        let scan = six_moment_scan();
        for product in SAMPLABLE {
            for values_wanted in [true, false] {
                let req = VoxelRequest {
                    product,
                    values_wanted,
                    ..request(ODD)
                };
                let grid = build_voxels(&scan, &req, SITE.0, SITE.1)
                    .unwrap_or_else(|| panic!("{} builds", product.name()));
                let what = format!("{} values={values_wanted}", product.name());

                if values_wanted {
                    // precondition: without a NaN in the plane the round trip
                    // would pass under a derived `PartialEq` too, and the
                    // claim about the codec would be weaker than it looks.
                    assert!(
                        grid.values().unwrap().iter().any(|v| v.is_nan()),
                        "{what}: the value plane has no NaN in it",
                    );
                    assert!(
                        grid.values().unwrap().iter().any(|v| v.is_finite()),
                        "{what}: the value plane has no numbers in it",
                    );
                }

                let decoded = VoxelGrid::from_bytes(&grid.to_bytes())
                    .unwrap_or_else(|| panic!("{what} did not decode"));
                assert_eq!(grid, decoded, "{what} changed in transit");
                // The absent plane is a state of its own and has to survive as
                // one: `Some(vec![NaN; cells])` compares unequal to `None`, so
                // this is not implied by the assertion above, but stating it
                // says which way round the failure would be.
                assert_eq!(
                    decoded.values().is_some(),
                    values_wanted,
                    "{what}: the value plane's presence did not survive",
                );
                assert_eq!(decoded.product(), product, "{what}");
                assert_eq!(decoded.shape(), ODD, "{what}");
                assert_eq!(decoded.lut(), grid.lut(), "{what}");
                assert_eq!(decoded.tilt_count(), grid.tilt_count(), "{what}");
                // And re-encoding is byte-identical, which says more than
                // equality does: `PartialEq` compares the value plane bitwise,
                // but an encoder that reordered two fields of equal width
                // would still satisfy it.
                assert_eq!(grid.to_bytes(), decoded.to_bytes(), "{what}");
            }
        }

        // The comparison is not vacuous: two grids that differ decode to two
        // grids that differ, in the plane, in the shape and in the box.
        let a = build_voxels(&scan, &request(ODD), SITE.0, SITE.1).unwrap();
        let elsewhere = build_voxels(
            &scan,
            &VoxelRequest {
                half_width_km: 61.0,
                ..request(ODD)
            },
            SITE.0,
            SITE.1,
        )
        .unwrap();
        let lean = build_voxels(
            &scan,
            &VoxelRequest {
                values_wanted: false,
                ..request(ODD)
            },
            SITE.0,
            SITE.1,
        )
        .unwrap();
        for (name, other) in [("a different box", &elsewhere), ("no value plane", &lean)] {
            assert_ne!(
                VoxelGrid::from_bytes(&a.to_bytes()).unwrap(),
                VoxelGrid::from_bytes(&other.to_bytes()).unwrap(),
                "{name} decoded to the same grid",
            );
        }
    }

    /// `to_bytes` reserves exactly what it writes. A grid is up to 40 MiB, so
    /// a wrong estimate is a copy of all of it.
    #[test]
    fn the_encoded_length_of_a_grid_is_exact() {
        let scan = six_moment_scan();
        // Both plane states and two shapes, so the estimate is pinned against
        // more than one total — the optional plane is the term most likely to
        // be dropped from it.
        for shape in [
            ODD,
            VoxelShape {
                nx: 4,
                ny: 5,
                nz: 3,
            },
        ] {
            for values_wanted in [true, false] {
                let req = VoxelRequest {
                    values_wanted,
                    shape,
                    ..request(shape)
                };
                let grid = build_voxels(&scan, &req, SITE.0, SITE.1).unwrap();
                assert_eq!(
                    grid.encoded_len(),
                    grid.to_bytes().len(),
                    "{shape:?} values={values_wanted}",
                );
            }
        }
    }

    /// The header's geometry numbers must all be finite, and the two fields
    /// that are **functions of the product** must agree with the product.
    ///
    /// `CrossSection::from_parts` already refuses a non-finite axis; this is
    /// the same hole one level over, plus the pair a grid states twice. None of
    /// these fails downstream — that is the point. A `NaN` extent divides into
    /// a `NaN` cell size and every `cell_centre_km` answers `NaN`; an infinite
    /// one collapses the cell size to zero and stacks every cell centre in one
    /// place; a mismatched ramp reads the indices off a scale they were not
    /// quantised against; a mismatched table paints another product's colours
    /// over this product's numbers. Each renders, and each is wrong in a way
    /// that looks like weather.
    #[test]
    fn a_grid_header_that_cannot_describe_its_own_product_is_refused() {
        let good = wire_fixture().to_bytes();
        assert!(
            VoxelGrid::from_bytes(&good).is_some(),
            "precondition: the unmutated payload must decode, or every \
             assertion below passes for the wrong reason"
        );

        // The four f64 pairs and the lone f64, by offset into the header.
        for (name, at) in [
            ("x_range.0", 20),
            ("x_range.1", 28),
            ("y_range.0", 36),
            ("y_range.1", 44),
            ("z_range.0", 52),
            ("z_range.1", 60),
            ("site.0", 68),
            ("site.1", 76),
            ("widest_tilt_gap_deg", 96),
        ] {
            for (what, bits) in [("NaN", f64::NAN), ("inf", f64::INFINITY)] {
                let mut bad = good.clone();
                bad[at..at + 8].copy_from_slice(&bits.to_le_bytes());
                assert!(
                    VoxelGrid::from_bytes(&bad).is_none(),
                    "{name} = {what} decoded",
                );
            }
        }
        // precondition: those offsets really name the header fields, rather
        // than landing in the padding of a layout that has since moved.
        let mut moved = good.clone();
        moved[20..28].copy_from_slice(&(-999.0f64).to_le_bytes());
        assert!(
            VoxelGrid::from_bytes(&moved).is_some_and(|g| g.x_range_km().0 == -999.0),
            "offset 20 is not x_range.0, so the finiteness assertions above are \
             corrupting some other field into invalidity",
        );

        // The ramp is `value_range_for(slot)` and nothing else, so any other
        // pair is a payload whose indices mean something this build cannot
        // reproduce.
        //
        // Planted **with the colour table that matches it**, which is the whole
        // point: a bare range edit is also caught by the table check below,
        // because that recomputes the table from the decoded range — so it
        // leaves the range check itself untested. Only a self-consistent
        // wrong-ramp payload — which is exactly what a build with a different
        // quantisation would send — isolates it.
        let mut ramp = good.clone();
        let bogus = (0.0f32, 60.0f32);
        assert_ne!(
            bogus,
            value_range_for(MomentSlot::Reflectivity),
            "precondition: the planted range is the real one",
        );
        ramp[84..88].copy_from_slice(&bogus.0.to_le_bytes());
        ramp[88..92].copy_from_slice(&bogus.1.to_le_bytes());
        ramp[LUT_LEN_AT + 4..LUT_LEN_AT + 4 + LUT_LEN]
            .copy_from_slice(&colormap_lut(RadarProduct::Reflectivity, bogus));
        assert!(
            VoxelGrid::from_bytes(&ramp).is_none(),
            "a value range this product's quantisation never produces decoded, \
             carrying a colour table built to agree with it — so `index_to_value` \
             would have read every index off the wrong scale",
        );

        // A length-correct table built for a different product. The fixture is
        // reflectivity; velocity's ramp colours the same 256 indices
        // completely differently.
        let alien = colormap_lut(
            RadarProduct::Velocity,
            value_range_for(MomentSlot::Velocity),
        );
        assert_eq!(alien.len(), LUT_LEN, "precondition: same length");
        let mut swapped = good.clone();
        swapped[LUT_LEN_AT + 4..LUT_LEN_AT + 4 + LUT_LEN].copy_from_slice(&alien);
        assert_ne!(
            good[LUT_LEN_AT + 4..LUT_LEN_AT + 4 + LUT_LEN],
            swapped[LUT_LEN_AT + 4..LUT_LEN_AT + 4 + LUT_LEN],
            "precondition: the two palettes are identical here, so swapping \
             them proves nothing",
        );
        assert!(
            VoxelGrid::from_bytes(&swapped).is_none(),
            "a colour table built for another product decoded, and the \
             raymarch would have painted it",
        );
    }

    /// The bytes arrive off a message port. Every malformed shape has to be a
    /// clean `None` — the two ends of that port can be different builds.
    #[test]
    fn a_malformed_grid_payload_is_refused_rather_than_misread() {
        let grid = wire_fixture();
        let good = grid.to_bytes();
        let cells = ODD.cells();
        let values_prefix_at = value_len_at(cells);

        assert!(VoxelGrid::from_bytes(&[]).is_none(), "empty");
        assert!(VoxelGrid::from_bytes(b"nope").is_none(), "wrong magic");

        // A **whole** payload relabelled, including with the two magics that
        // share this port. Mutation testing is why: a four-byte buffer cannot
        // pin the magic test, because it fails on the version read instead —
        // deleting the magic comparison outright left every short-buffer
        // assertion here green, and a section frame would then have been
        // decoded as a grid.
        for wrong in [*b"nope", *b"RDRI", *b"RDXS"] {
            let mut relabelled = good.clone();
            relabelled[..4].copy_from_slice(&wrong);
            assert!(
                VoxelGrid::from_bytes(&relabelled).is_none(),
                "a whole payload labelled {} decoded as a grid",
                String::from_utf8_lossy(&wrong),
            );
        }

        let mut wrong_version = good.clone();
        wrong_version[4] = 0xFF;
        wrong_version[5] = 0xFF;
        assert!(
            VoxelGrid::from_bytes(&wrong_version).is_none(),
            "an unknown version decoded",
        );

        // A product code this build does not have, and one it has but cannot
        // resample. The second is the interesting half: `RadarProduct` knows
        // what VIL is, so only the `samplable` refusal stops a payload whose
        // `value_range` came from no moment's ramp.
        let mut unknown_product = good.clone();
        unknown_product[6..8].copy_from_slice(&0xFFFEu16.to_le_bytes());
        assert!(
            VoxelGrid::from_bytes(&unknown_product).is_none(),
            "an unknown product code decoded",
        );
        let mut underivable = good.clone();
        underivable[6..8].copy_from_slice(
            &RadarProduct::VerticallyIntegratedLiquid
                .wire_code()
                .to_le_bytes(),
        );
        assert!(
            samplable(RadarProduct::VerticallyIntegratedLiquid).is_none(),
            "precondition: VIL became samplable, so this is no longer the \
             refusal this assertion is about",
        );
        assert!(
            VoxelGrid::from_bytes(&underivable).is_none(),
            "a product with no native moment decoded",
        );

        // An axis outside `1..=MAX_AXIS`, on each of the three, at both ends.
        for axis in 0..3 {
            for bad in [0u32, (MAX_AXIS + 1) as u32, u32::MAX] {
                let mut broken = good.clone();
                broken[SHAPE_AT + axis * 4..SHAPE_AT + axis * 4 + 4]
                    .copy_from_slice(&bad.to_le_bytes());
                assert!(
                    VoxelGrid::from_bytes(&broken).is_none(),
                    "axis {axis} of {bad} decoded",
                );
            }
        }
        // A *supported* shape that is not the one the planes are sized for is
        // refused too, and by the plane checks rather than by `is_supported` —
        // this is the cross-build case, since the three named shapes differ.
        let mut reshaped = good.clone();
        reshaped[SHAPE_AT..SHAPE_AT + 4].copy_from_slice(&((ODD.nx + 1) as u32).to_le_bytes());
        assert!(
            VoxelGrid::from_bytes(&reshaped).is_none(),
            "a shape claiming more cells than the index plane holds decoded — \
             every accessor indexes that plane with an offset from the shape",
        );

        // An unsupported shape whose planes **agree** with it, so nothing but
        // `is_supported` can object. Mutation testing needs these two: every
        // bad-axis assertion above is caught downstream by the index-plane
        // length instead, and deleting `is_supported` from `from_bytes`
        // altogether left all of them green.
        //
        // Zero is the dangerous half, and it is why `is_supported` refuses a
        // zero axis rather than yielding an empty grid: without the guard this
        // frame decodes into a grid of no cells, whose extents a renderer
        // divides by a zero dimension to get an infinity, and which is
        // indistinguishable from a volume with nothing in it.
        for axis in 0..3 {
            let mut empty = good[..INDEX_LEN_AT].to_vec();
            empty[SHAPE_AT + axis * 4..SHAPE_AT + axis * 4 + 4]
                .copy_from_slice(&0u32.to_le_bytes());
            // Both planes sized to match `cells() == 0`: none, and absent.
            empty.extend_from_slice(&0u32.to_le_bytes());
            empty.extend_from_slice(&0u32.to_le_bytes());
            assert!(
                VoxelGrid::from_bytes(&empty).is_none(),
                "axis {axis} of zero decoded into a grid with no cells",
            );
        }

        // And the other end, past `MAX_AXIS`, again with planes to match: one
        // axis at the GLES 3.0 guarantee, bumped one over it, and both planes
        // grown by the one cell that implies.
        let tall = VoxelShape {
            nx: MAX_AXIS,
            ny: 1,
            nz: 1,
        };
        let over_shape = build_voxels(&scan_of(&|_, _| Some(40.0)), &request(tall), SITE.0, SITE.1)
            .expect("a shape at the guarantee builds");
        let mut over = over_shape.to_bytes();
        let tall_cells = tall.cells();
        over[SHAPE_AT..SHAPE_AT + 4].copy_from_slice(&((MAX_AXIS + 1) as u32).to_le_bytes());
        over[INDEX_LEN_AT..INDEX_LEN_AT + 4]
            .copy_from_slice(&((tall_cells + 1) as u32).to_le_bytes());
        over.insert(INDEX_LEN_AT + 4 + tall_cells, NO_DATA_INDEX);
        // The insert above pushed the value prefix along by that one byte.
        let moved = value_len_at(tall_cells) + 1;
        over[moved..moved + 4].copy_from_slice(&((tall_cells + 1) as u32).to_le_bytes());
        over.extend_from_slice(&f32::NAN.to_le_bytes());
        assert!(
            VoxelGrid::from_bytes(&over).is_none(),
            "an axis of {} — one over the GLES 3.0 guarantee — decoded, with \
             planes sized to agree with it",
            MAX_AXIS + 1,
        );

        for cut in [
            1,
            8,
            SHAPE_AT,
            HEADER_BYTES,
            LUT_LEN_AT + 4,
            INDEX_LEN_AT,
            INDEX_LEN_AT + 4,
            values_prefix_at,
            values_prefix_at + 4,
            good.len() / 2,
            good.len() - 1,
        ] {
            assert!(
                VoxelGrid::from_bytes(&good[..cut]).is_none(),
                "truncated to {cut} bytes",
            );
        }

        let mut trailing = good.clone();
        trailing.push(0);
        assert!(
            VoxelGrid::from_bytes(&trailing).is_none(),
            "trailing bytes mean the layouts disagree",
        );

        // A length that cannot fit in what remains, on each of the three
        // planes. The value plane is the one that matters: four bytes an
        // element, so a believed `u32::MAX` reserves 16 GiB before the read
        // fails.
        for (name, at) in [
            ("table", LUT_LEN_AT),
            ("index", INDEX_LEN_AT),
            ("value", values_prefix_at),
        ] {
            let mut absurd = good.clone();
            absurd[at..at + 4].copy_from_slice(&u32::MAX.to_le_bytes());
            assert!(
                VoxelGrid::from_bytes(&absurd).is_none(),
                "an absurd {name} plane length reached a read",
            );
        }

        // Each plane one element short of what the shape declares, with its
        // prefix moved to match, so the frame is well-formed right through to
        // `at_end` and only the plane check can object.
        for (name, at, element) in [
            ("table", LUT_LEN_AT, 1usize),
            ("index", INDEX_LEN_AT, 1),
            ("value", values_prefix_at, 4),
        ] {
            let mut short = good.clone();
            let count = prefix_at(&short, at) as usize;
            let plane_end = at + 4 + count * element;
            short[at..at + 4].copy_from_slice(&((count - 1) as u32).to_le_bytes());
            short.drain(plane_end - element..plane_end);
            assert!(
                VoxelGrid::from_bytes(&short).is_none(),
                "a {name} plane one element short decoded",
            );
        }

        // A value plane that is neither absent nor the size of the grid. One
        // element is the shape a sender that meant "no plane" but wrote a
        // sentinel would produce, and it must not be read as either.
        let mut one_value = good.clone();
        one_value.truncate(values_prefix_at + 4 + 4);
        one_value[values_prefix_at..values_prefix_at + 4].copy_from_slice(&1u32.to_le_bytes());
        assert!(
            VoxelGrid::from_bytes(&one_value).is_none(),
            "a one-element value plane decoded",
        );

        // Absent, on the other hand, is a state: zero and nothing after it is
        // the encoding `values_wanted: false` produces, and it decodes.
        let mut absent = good.clone();
        absent.truncate(values_prefix_at + 4);
        absent[values_prefix_at..values_prefix_at + 4].copy_from_slice(&0u32.to_le_bytes());
        let decoded = VoxelGrid::from_bytes(&absent)
            .expect("a grid with no value plane is a grid, not a malformed one");
        assert_eq!(decoded.values(), None);
        assert_eq!(decoded.indices(), grid.indices());

        // precondition: the fixture the mutations were made against decodes,
        // so every refusal above is the mutation's doing and not the
        // fixture's.
        assert_eq!(
            VoxelGrid::from_bytes(&good).expect("the unmutated payload decodes"),
            grid,
        );
        assert!(
            VoxelGrid::from_bytes(&over_shape.to_bytes()).is_some(),
            "precondition: the shape-at-the-guarantee payload does not decode \
             unmutated either, so the assertion about it says nothing",
        );
    }

    /// The capacity guard, tested directly, because nothing end to end can
    /// see it.
    ///
    /// [`Reader::bounded`] does not change *what* [`VoxelGrid::from_bytes`]
    /// answers. `take` bounds every read, so a believed length fails on the
    /// read either way and the payload is refused with or without it. What it
    /// changes is whether four billion elements are reserved **first** — a
    /// 16 GiB allocation on the way to a `None`, on a worker thread, in a
    /// browser tab. Mutation testing confirms the gap rather than assuming it:
    /// deleting the call from `from_bytes` leaves the whole suite green, which
    /// is why the helper is named and pinned here instead.
    #[test]
    fn the_capacity_guard_refuses_a_length_the_buffer_cannot_hold() {
        let bytes = [0u8; 16];
        let r = Reader::new(&bytes);
        assert_eq!(r.bounded(4, 4), Some(4), "16 bytes hold four f32");
        assert_eq!(r.bounded(0, 4), Some(0));
        assert_eq!(r.bounded(5, 4), None, "20 bytes claimed from 16");
        assert_eq!(r.bounded(u32::MAX, 4), None, "16 GiB claimed from 16 bytes");

        // It measures against what is *left*, not against the whole buffer —
        // otherwise a length prefix late in a frame would be judged against
        // bytes already consumed.
        let mut part_way = Reader::new(&bytes);
        part_way.take(8).expect("half the buffer");
        assert_eq!(part_way.bounded(2, 4), Some(2));
        assert_eq!(part_way.bounded(3, 4), None);

        // And the multiply cannot overflow into a pass.
        assert_eq!(Reader::new(&bytes).bounded(u32::MAX, usize::MAX), None);
    }
}
