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
//! so the site's **feedhorn** height is subtracted once per grid — via
//! [`crate::eet::radar_height_ft_near`] on
//! [`crate::sites::Datum::Feedhorn`], and the same `* 0.0003048` spelling
//! `render.rs` uses for `radar_km_msl`. The antenna is what the heights are
//! above; the ground under the tower is 30–115 ft lower and was what this
//! subtracted before the datum was named.
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
//! # What the renderer does with the no-data boundary, and what it no longer
//! needs from this encoding
//!
//! Everything above about the *bottom-of-ramp* decision still stands as an
//! encoding decision — index 0 must sit one quantisation step below the
//! moment's floor so that no measurement is indistinguishable from an absence,
//! and the out-of-band alternative clamps real values. What no longer stands
//! is the paragraph that made the palette's fade band the renderer's only
//! defence at a data/no-data boundary.
//!
//! `rustdar-frontend`'s raymarch uploads this grid as **`Rg16Float`**, not
//! `R8Unorm`: `R = coverage × index`, `G = coverage`, where coverage is 1 for
//! a cell whose index is not [`NO_DATA_INDEX`] and 0 for one whose is. Both
//! channels are filtered `Linear` in hardware and the shader reconstructs
//! `index = R̄ / Ḡ`, which is the coverage-weighted mean **over covered
//! texels only** — empty air contributes 0 to numerator and denominator
//! alike, so it drops out of the average rather than participating in it as a
//! value. The reconstructed index therefore always lies in the convex hull of
//! the *stored* indices around the sample, for every product, and `Ḡ` is
//! itself the emptiness test.
//!
//! The consequence for this module: the "the renderer has to supply the fade
//! itself, because five of the six palettes have none" note in the WP-I
//! paragraph above is **obsolete**. So is the per-product
//! blend-or-march-nearest table that lived here as
//! `no_data_blends_at_ramp_bottom`: all nine renderable products take one
//! reconstruction path now, because the boundary problem it worked around
//! cannot arise. [`VoxelGrid::fade_band`] survives for a different job — it
//! is where the *palette's own* transparent run ends, which is what the
//! march's skip threshold and soft-edge ramp anchor on, and that is a
//! statement about the table rather than about no-data.
//!
//! One thing this does **not** change: the CPU-side readers (the section
//! pane, `index_at`, `value_at`) sample without any filter at all, so the
//! encoding they see is exactly the one described above.
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

use crate::beam;
use crate::palette::{get_color_for_value, get_legend_scale};
use crate::par::*;
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

/// Narrowest half-extent a request may ask for on either axis, km. Below this
/// the grid is finer than the radar's own 250 m gates over most of its extent
/// and the resample invents smoothness.
pub const MIN_HALF_WIDTH_KM: f64 = 10.0;

/// The half-width a box is given when nothing can be said about how far its
/// volume reaches, km — the WSR-88D's nominal unambiguous range, and the box
/// this resampler was fixed at for its whole life.
///
/// Reached only through [`box_half_width_km`]'s `NaN` and non-positive arms,
/// which is to say: a volume carrying no gate count at all for the moment
/// asked for. Every such box is the one that has always been drawn, so an
/// unreadable reach degrades to yesterday's picture rather than to a 20 km
/// box with a storm outside it.
pub const BASE_HALF_WIDTH_KM: f64 = 230.0;

/// Furthest a box's **corner** may stand from its centre, km.
///
/// [`crate::types::MAX_EXTENT_KM`] itself, because that is the same guard read
/// through the same geometry: the plan view's cap is against a mis-framed
/// radial claiming sixty thousand gates, and a box whose corner is inside the
/// reach the raster will project is a box the raster can be laid under. A
/// reach the raster refuses to zoom out past is a reach this must refuse to
/// widen past.
///
/// The corner, rather than either side, because that is the quantity
/// [`box_half_width_km`] has always capped — its `half ≤ MAX_HALF_WIDTH_KM` is
/// `half · √2 ≤ MAX_EXTENT_KM` written for the one case where the two sides
/// are equal. A rectangle has no single "width" to bound, and bounding each
/// side separately would let a 470 × 470 km half-extent through, whose corner
/// stands 664.7 km out.
pub const MAX_HALF_DIAGONAL_KM: f64 = crate::types::MAX_EXTENT_KM;

/// Widest half-width a **square** request may ask for, km.
///
/// [`MAX_HALF_DIAGONAL_KM`] over √2, and written as that division so the two
/// cannot drift: it is [`HalfExtentKm::clamped`]'s corner bound solved for the
/// case `east_km == north_km`, which is the only case that has a half-width at
/// all.
///
/// 332.34 km — a 665 km box — is therefore not a range any radar reaches. The
/// widest honest one in this display is the 460.1 km WSR-88D surveillance
/// cut, which asks for 325.4.
pub const MAX_HALF_WIDTH_KM: f64 = MAX_HALF_DIAGONAL_KM / std::f64::consts::SQRT_2;

/// Half a box's east–west and north–south extent, km.
///
/// **A named struct rather than two `f64` arguments or a pair**, and that is
/// the whole reason it exists: the two numbers are adjacent, same-typed and
/// name different axes, so a transposition between them is a change no
/// compiler and no square fixture can see. Every grid this module has ever
/// built had `east_km == north_km`, which means a swap was invisible by
/// construction until now.
///
/// Both fields are half-extents, not full ones, so
/// `HalfExtentKm::square(230.0)` is the 460 km box this resampler was fixed at
/// for its whole life.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HalfExtentKm {
    /// Half the box's east–west extent, km. Becomes [`VoxelGrid::x_range_km`].
    pub east_km: f64,
    /// Half the box's north–south extent, km. Becomes
    /// [`VoxelGrid::y_range_km`].
    pub north_km: f64,
}

impl HalfExtentKm {
    /// The same half-extent on both axes — the box every caller asked for
    /// before a 3D pane's viewport could be anything but square.
    pub const fn square(km: f64) -> Self {
        Self {
            east_km: km,
            north_km: km,
        }
    }

    /// How far the box's corner stands from its centre, km.
    ///
    /// The quantity [`MAX_HALF_DIAGONAL_KM`] bounds, and the rectangle's form
    /// of the inscribed-square rule in [`box_half_width_km`]: a box whose
    /// corner sits on the data's own range circle is the largest one of that
    /// shape with no permanently empty corner cells.
    pub fn corner_km(self) -> f64 {
        self.east_km.hypot(self.north_km)
    }

    /// Whether both axes are finite. `NaN` reaches
    /// [`VoxelGrid::cell_centre_km`] and makes every cell unplaceable with no
    /// error anywhere, so [`build_voxels`] refuses it at the door.
    pub fn is_finite(self) -> bool {
        self.east_km.is_finite() && self.north_km.is_finite()
    }

    /// This extent floored at [`MIN_HALF_WIDTH_KM`] per axis, then brought
    /// inside [`MAX_HALF_DIAGONAL_KM`] **without changing its shape** — the
    /// clamp [`VoxelRequest::half_extent_km`] promises.
    ///
    /// **The one definition of that clamp**, because two callers need the same
    /// answer and neither may re-spell it: the resampler, and the renderer,
    /// which has to know the box a request *will* produce before the build that
    /// produces it has run. [`horizontal_ranges_km`] gives the arithmetic that
    /// depends on them agreeing bit for bit, and the 5.7e-14 km a second
    /// spelling costs. (The `None` arm of that field is a different question
    /// and is answered by [`box_half_width_km`], which needs the scan.)
    ///
    /// The upper stop scales both axes by one factor rather than clamping each
    /// on its own. Independent clamps would turn a 450 × 200 km ask into
    /// 332 × 200 — a box of a different aspect ratio from the one the pane is
    /// framing, which is a silently wrong picture rather than a stopped zoom.
    /// One factor keeps the ratio and lands the corner exactly on the bound.
    ///
    /// The floor is applied first, so an extent past 47:1 with its corner over
    /// the bound comes back with its short axis under [`MIN_HALF_WIDTH_KM`]
    /// again — 10 km scaled by `470 / corner`. Shape is the property worth
    /// keeping there: the floor exists because a box finer than the radar's
    /// own 250 m gates invents smoothness, and no viewport this is fed from is
    /// anywhere near that long and thin.
    pub fn clamped(self) -> Self {
        let floored = Self {
            east_km: self.east_km.max(MIN_HALF_WIDTH_KM),
            north_km: self.north_km.max(MIN_HALF_WIDTH_KM),
        };
        let corner = floored.corner_km();
        if corner <= MAX_HALF_DIAGONAL_KM {
            return floored;
        }
        let scale = MAX_HALF_DIAGONAL_KM / corner;
        Self {
            east_km: floored.east_km * scale,
            north_km: floored.north_km * scale,
        }
    }
}

/// Half-width of the box to resample, km, given how far the volume's data
/// reaches — **the largest square that fits inside the sweep's own range
/// circle**.
///
/// [`crate::types::plan_view_extent_km`] is this function's counterpart, and
/// the difference between the two is the whole design. Both take a reach off
/// the wire and answer a half-width; the raster's answer *circumscribes* the
/// data circle and this one is *inscribed* in it, because a raster's corners
/// are free and a box's corners are not:
///
/// * a raster grows its side in pixels with its extent
///   ([`crate::types::raster_side_px`]), so a surveillance cut's 460.1 km of
///   ground costs 4.4512 px/km against the 230 km floor's 4.4522, and the
///   corner pixels outside the data circle are pixels the projection simply
///   leaves empty;
/// * a box has `nx · ny · nz` cells and `nx` is pinned at [`MAX_AXIS`] — the
///   3D texture size GLES 3.0 *guarantees*, which the browser is held to — so
///   its resolution is exactly `2 · half_width / nx` and a corner cell outside
///   the data circle is a cell that can never hold a measurement **and** a
///   cell of resolution taken from the ones that can.
///
/// So the box stops where its corners touch the data. One cell wider and the
/// corners are provably empty: at a 460 km reach, a 920 km box (the raster's
/// own extent, doubled) leaves 21.5% of its cells — 1.8 M of 8.4 M —
/// permanently [`NO_DATA_INDEX`], and spends 3.59 km per cell to do it.
///
/// # What the trade was measured to be
///
/// KCRP 2017-08-26 04:41Z (Harvey), [`DESKTOP_SHAPE`], reflectivity, cells
/// holding each dBZ class after the resample, against the share of the
/// volume's own ≥5 dBZ gates that fall outside the box:
///
/// | half-width | box | km/cell | ≥20 dBZ | ≥35 dBZ | ≥50 dBZ | echo outside | build |
/// |---|---|---|---|---|---|---|---|
/// | 230 (the old fixed box) | 460 km | 1.80 | 513 827 | 52 783 | 97 | 1.72% | 28 ms |
/// | 300 | 600 km | 2.34 | 310 585 | 31 305 | 77 | 0.03% | — |
/// | **325.4 (this rule)** | **651 km** | **2.54** | 264 617 | 26 449 | 71 | 0.02% | 24 ms |
/// | 460 (the raster's extent) | 920 km | 3.59 | 132 096 | 13 184 | 27 | 0% | 20 ms |
///
/// Widening is free in **memory** and slightly *cheaper* in **time**. The
/// shape is fixed, so the index plane is 8 MiB and the optional value plane
/// 32 MiB at every width; and a wider box puts more of its columns outside
/// the data, where a column costs a range search that finds nothing. The
/// times are best of fifteen, release, one fresh process per width, on a
/// 32-thread Ryzen 9 7950X with the one-minute load average under 20 —
/// measured that way because timing all three widths inside one process made
/// the widest look 30 ms and the 512-cell shape below 202 ms, which is the
/// allocator warming to 160 MiB a build rather than anything about the grids.
///
/// **Resolution is the entire cost**, and it is why the rule stops at the
/// corners rather than at the raster's extent: the last doubling buys 0.02%
/// of the echo for three quarters of the ≥35 dBZ cells.
///
/// # Why the grid is not simply made bigger
///
/// Because [`MAX_AXIS`] is not a tuning knob — it is `GL_MAX_3D_TEXTURE_SIZE`
/// as GLES 3.0 guarantees it, so 256 is what a browser has to accept. Lifting
/// it was measured anyway, since it is the only arrangement that gets both
/// the reach and the resolution: 512 × 512 × 128 over the 920 km box holds
/// 1.80 km/cell and every dBZ class the 460 km box had (≥50 dBZ 97 against
/// 97, ≥35 53 283 against 52 783), for **32 MiB of indices and 128 MiB of
/// values, and 77 ms against 28** — four times [`VOXEL_TEXTURE_BUDGET_BYTES`],
/// two and a half times `rustdar_frontend`'s largest per-target volume budget,
/// unreachable on the web at all, and thirteen such grids in a desktop 3D
/// loop.
///
/// # Why this is worth following per volume rather than pinning
///
/// Across 150 archive volumes from 53 sites, every single WSR-88D reports a
/// **460.1 km** reflectivity reach — there is no variance in it at all, so
/// for reflectivity this rule and the constant 325.4 are the same number. The
/// variance is in the other moments, and it runs the *helpful* way: a Doppler
/// cut reaches 300 km, so velocity, spectrum width, ZDR, ΦDP, ρHV and every
/// product derived from them ask for a 212 km half-width — a 424 km box at
/// 1.66 km/cell, both tighter and **finer** than the fixed 230 they used to
/// get, with the whole 300 km circle inside it. A TDWR's ~89 km Doppler
/// volume asks for 63 km, and gets 0.49 km/cell where it used to be given
/// 1.80 and four fifths of an empty box.
///
/// A `NaN` or non-positive reach answers [`BASE_HALF_WIDTH_KM`]. `clamp`
/// propagates `NaN`, and a `NaN` half-width reaches [`VoxelGrid::x_range_km`]
/// and makes every cell of the grid unplaceable with no error anywhere.
pub fn box_half_width_km(data_reach_km: f64) -> f64 {
    // `is_nan` spelled out rather than folded into the comparison: every
    // ordering against a `NaN` is false, so `<= 0.0` alone would let one
    // through to a `clamp` that propagates it.
    if data_reach_km.is_nan() || data_reach_km <= 0.0 {
        return BASE_HALF_WIDTH_KM;
    }
    (data_reach_km / std::f64::consts::SQRT_2).clamp(MIN_HALF_WIDTH_KM, MAX_HALF_WIDTH_KM)
}

/// How far `product`'s data reaches over the **ground**, km, across every
/// sweep of `scan` that carries it — 0.0 if none does.
///
/// The volume's counterpart to [`crate::render`]'s `compute_max_range`, and
/// the same gate arithmetic: a moment's first gate plus its gate count times
/// its interval. Two things differ, and both come from what the answer is
/// for.
///
/// It is a **maximum over the whole volume** rather than one sweep's, because
/// one box holds every rung: a WSR-88D's split cuts put a 460 km surveillance
/// sweep and a 300 km Doppler sweep at the same elevation, and a box sized to
/// the shorter of them would crop the taller one it also resamples.
///
/// It is a **ground** range, `cos e` of the sweep's median elevation folded
/// in, because that is the coordinate the box's axes are in
/// ([`crate::sampler::VolumeSampler::column_into`] is asked for a ground
/// range). The correction is 0.004% at 0.5° and would be nothing at all if
/// only the lowest cut could ever be the longest — but the reach is a maximum
/// over cuts, and a sweep whose gates were shortened by the projection while
/// its reach was not is exactly the disagreement the plan view had to fix.
pub fn volume_reach_km(scan: &nexrad_model::data::Scan, product: RadarProduct) -> f64 {
    use nexrad_model::data::DataMoment;

    let mut reach = 0.0f64;
    for sweep in scan.sweeps() {
        let radials = sweep.radials();
        let ground = match crate::volumetric::sweep_elevation_deg(radials) {
            Some(e) if e.is_finite() => e.to_radians().cos().clamp(0.0, 1.0),
            _ => 1.0,
        };
        for radial in radials {
            let Some(moment) = product.get_moment(radial) else {
                continue;
            };
            let slant = moment.first_gate_range_km()
                + f64::from(moment.gate_count()) * moment.gate_interval_km();
            reach = reach.max(slant * ground);
        }
    }
    reach
}

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
///
/// # The cells are rectangular, and what that costs — measured
///
/// Every named shape has `nx == ny`, but a box's two horizontal extents are
/// **not** equal: a 3D pane's box is the rectangle of ground its viewport is
/// showing, so a 16:9 pane spends the same 256 cells over 1.78× as much ground
/// east–west as north–south. The cells are anisotropic by that ratio, and the
/// question that raises — whether the coarser axis loses features — is answered
/// here rather than argued about, because the alternative on the table was a
/// cap on the box's aspect (letterboxing the excess ground, which is the defect
/// the rectangle exists to fix).
///
/// Measured on four storm volumes — KCRP 2017-08-26 04:41Z (Harvey), KFTG
/// 2023-06-22 03:46Z, KDMX 2022-03-05 23:23Z, KMSX 2022-06-04 20:05Z — as
/// **reflectivity volume in km³ at or above a class cut, over the ground the
/// two boxes share**, at [`DESKTOP_SHAPE`]. Two regimes: a mid zoom holding the
/// north extent and widening east to 16:9 (cells 2.083 × 1.172 km against a
/// square box's 1.172), and a wide-open pane on the resampler's own ceiling
/// (cells 3.200 × 1.800 km against a square box's 2.596). The figure is the
/// rectangle's volume against the square's:
///
/// | cut | mid zoom, four sites | wide open, four sites |
/// |---|---|---|
/// | ≥20 dBZ | −0.09, −0.29, −0.15, −0.08 % | −0.02, −0.13, −0.68, −1.01 % |
/// | ≥35 dBZ | +0.62, −0.41, +0.20, −1.80 % | −0.26, +0.11, −0.18, +1.58 % |
/// | ≥50 dBZ | −2.42, +0.31, −0.49, −100 % | +4.29, −2.28, −1.43, −100 % |
///
/// **So the anisotropy costs nothing at the classes a reader is looking at**:
/// under 1.1% everywhere at ≥20 and under 1.9% at ≥35, in both directions,
/// which is sampling noise of a cell or two on a coarser axis rather than a
/// loss. The two −100% entries are both KMSX, whose ≥50 dBZ class is 4 cells
/// (0.8 km³) and 2 cells (1.9 km³) in the square box — a feature already below
/// either grid's resolution, and the honest reading is that it was never
/// resolved rather than that the rectangle erased it. No aspect cap: it would
/// trade a measured nothing for the ground the box exists to show.
///
/// The one thing that *does* change is the cloud rung.
/// `volume_bridge::largest_cell_km` feeds `cloud_reconstruction_lod_for` the
/// **coarsest** axis, so a wide box reaches that taper's 1.75 km/cell knee at a
/// tighter zoom than a square one did — measured on the mid-zoom pair above,
/// the square box gets reconstruction level 0.526 where the 16:9 box gets 0.
/// That is the correct direction and the taper's own measurement is why: the
/// kernel spans two cells, so on the 16:9 box it is 4.2 km wide east–west, and
/// at 1.80 km/cell that kernel was measured taking the ≥50 dBZ eyewall to zero
/// painted pixels. Keying on the finer axis would apply it anyway. The cost is
/// that the north–south axis is smoothed less than its own 1.17 km cells could
/// take; a per-axis reconstruction level is the fix if anyone ever wants it,
/// and nobody has asked.
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
/// protect: [`build_voxels`] clamps `half_extent_km` and refuses everything
/// else it cannot honour, so there is no way to build one that lies about its
/// contents. [`VoxelGrid`]'s fields are private for the opposite reason.
#[derive(Debug, Clone, PartialEq)]
pub struct VoxelRequest {
    /// Latitude and longitude of the box's horizontal centre. Need not be the
    /// site; the output's `x`/`y` ranges are relative to the **site** either
    /// way.
    pub centre: (f64, f64),
    /// Half the box's east–west and north–south extent, km, or `None` to take
    /// the square half-width [`box_half_width_km`] derives from the volume's
    /// own reach.
    ///
    /// `None` is the ordinary case — a 3D pane with no picked region — and it
    /// is an absence rather than a copy of the default for the reason
    /// [`crate::types::ImageBounds::from_radar_site`] takes its extent as an
    /// argument: the number depends on the volume, only [`build_voxels`] has
    /// the volume, and a caller that computed it from a *different* volume
    /// would resample ground the pane beside it is not drawing, with nothing
    /// to notice it. It is also the case with no aspect ratio to be had: the
    /// caller is a worker holding a volume and no pane.
    ///
    /// `Some` is a region the user picked, and it need not be square — a 3D
    /// pane's viewport is not. It is put through [`HalfExtentKm::clamped`]
    /// rather than refused, because a zoom control that reaches the end of its
    /// travel should stop, not fail.
    pub half_extent_km: Option<HalfExtentKm>,
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

    /// The isosurface uniform pair `(centre, threshold)` for a user-facing
    /// threshold in the product's own units, both in the shader's 0-1 index
    /// space.
    ///
    /// `centre` is negative for a sequential product (the shader then reads
    /// the index directly) and the diverging centre's index otherwise;
    /// `threshold` is the crossing distance in index units. The translation
    /// runs through [`Self::value_to_index`], so the surface sits exactly
    /// where the ramp puts the value — the same quantisation the lit volume
    /// paints through. The user value's shape per product is
    /// [`iso_shape`]; non-finite input falls back to
    /// [`default_iso_threshold`], the same refusal every persisted float
    /// gets.
    pub fn iso_uniform_params(&self, user_threshold: f32) -> (f32, f32) {
        let user = if user_threshold.is_finite() {
            user_threshold
        } else {
            default_iso_threshold(self.product)
        };
        let norm = |index: u8| f32::from(index) / 255.0;
        match iso_shape(self.product) {
            IsoShape::Sequential => (-1.0, norm(self.value_to_index(user))),
            IsoShape::DeviationFrom { centre } => {
                let c = self.value_to_index(centre);
                let at = self.value_to_index(centre + user.abs());
                (norm(c), norm(at.saturating_sub(c).max(1)))
            }
            IsoShape::AtOrBelow => {
                let top = 255u8;
                let at = self.value_to_index(user);
                (norm(top), norm(top.saturating_sub(at).max(1)))
            }
        }
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

    /// Differential reflectivity's quiet band is the interval the crate's own
    /// ORPG-derived HCA leaves for ordinary rain — and it does **not**
    /// contain zero.
    ///
    /// The shipped profile centred a fully clear band on +0.25 dB and claimed
    /// opacity beyond ±3 dB showed "hail and graupel cores on the negative".
    /// [`crate::hca`] contradicts the second half outright: graupel is refused
    /// above [`crate::hca::MAX_ZDR_GR`] = 2.0 dB, and `HailSize`'s hard limit
    /// is [`crate::hca::HSDA_MAX_ZDR`] = 2.0, commented in that module as
    /// "high ZDR is never large/giant hail". Hail is a tumbling,
    /// near-isotropic scatterer: its signature is ZDR ≈ 0 under high Z, not
    /// ZDR ≪ 0. A clear band over [−0.5, +1.0] reaching full opacity only
    /// past −2.75 dB therefore rendered the canonical hail core as a **hole**
    /// — the same pixels the volume shows where there is no data at all —
    /// and spent the ramp on a negative tail nature seldom reaches.
    ///
    /// A diverging moment's boring band is near zero only when the scatterers
    /// are *rain*. So the quiet band is put where the HCA's own class kills
    /// say rain and nothing else lives: from [`crate::hca::MIN_ZDR_BD`] = 0.5
    /// dB (under it a return has already been refused the big-drop class, and
    /// [`crate::hca::MIN_ZDR_HR`] = 1.0 refuses heavy rain as well) up to
    /// [`crate::hca::MAX_ZDR_GR`] = 2.0 (over it every ice, graupel and hail
    /// class has been refused, and [`crate::hca::MAX_ZDR_DS`] puts dry snow
    /// at the same bound). Inside that interval ZDR has excluded nothing and
    /// is reporting the rain that fills a volume; outside it, either way, ZDR
    /// has excluded a class and is carrying information.
    ///
    /// The two departures are not symmetric and the profile is not either,
    /// and the asymmetry is a measurement rather than a preference.
    ///
    /// Upward is drop size: a smooth continuum that only becomes a ZDR column
    /// well above the rain band, so the rise runs out to [`ZDR_COLUMN_DB`]
    /// and reaches the palette's full alpha there.
    ///
    /// Downward is a change of *phase* — ice, graupel, tumbling hail, and
    /// under 0 dB even wet snow is refused ([`crate::hca::MIN_ZDR_WS`]) — but
    /// it is emphatically **not** rare, and that is the thing to get right.
    /// Counted over four volumes (KFTG 2023-06-22, KLWX 2018-03-02, KDMX
    /// 2025-03-14, KTLX 2019-07-15), ZDR in [−0.5, +0.5] is **68 % of every
    /// data voxel in the box** — noise at long range and the dry snow and
    /// small ice that fill the top of any volume, sharing the band with the
    /// hail signature and indistinguishable from it without Z. A profile that
    /// simply ramped this side to full opacity drew 91 % of the volume at a
    /// mean alpha of 110 of 180. That is a wall, and a wall is the other way
    /// of telling the user nothing.
    ///
    /// So the low side is a **plateau, not a ramp**. It rises from clear at
    /// the rain floor to [`ZDR_TUMBLING_ALPHA`] at
    /// [`ZDR_TUMBLING_DB`] — 0 dB, the tumbling-scatterer value itself and
    /// the crate's own wet-snow kill — and stays there until it climbs to
    /// full at [`ZDR_NEGATIVE_DB`], which nature seldom reaches.
    ///
    /// Which half of that is measured, plainly: the **shape** is, the
    /// **level** is not. The 68 % count above is what rules a ramp out, and
    /// it is a count over real volumes. The plateau's height of 0.35 is
    /// [`PHI_ALPHA`] taken by reference, and the case for reusing it is an
    /// *analogy* — one side of one product is in the position ΦDP is in
    /// whole, a population that has to be visible without becoming the
    /// volume, so it gets that moment's translucency. No measurement
    /// distinguishes 0.35 from 0.3 or 0.4 here, and none is claimed; the
    /// test pins the plateau against `PHI_ALPHA`'s identity for exactly that
    /// reason, rather than against a number of its own.
    ///
    /// The hail signature is then plainly present at 63 of 180 where it used
    /// to be a hole, the ice and noise mass above it tapers off toward the
    /// rain band, and the rare deep negative — the three-body spike, the
    /// vertically aligned ice — still stands out at full strength.
    ///
    /// What this deliberately does not claim: ZDR alone cannot tell that
    /// near-zero hail from that dry snow — `MAX_ZDR_DS` is 2.0 too, and the
    /// discriminator is Z, which a one-moment volume does not carry. The
    /// profile makes the region *visible*, not *hail-coloured*; the palette's
    /// own colours still say only what the value is, and the plateau is what
    /// keeps the volume from asserting more than the moment knows.
    pub const ZDR_RAIN_LO_DB: f32 = crate::hca::MIN_ZDR_BD as f32;
    pub const ZDR_RAIN_HI_DB: f32 = crate::hca::MAX_ZDR_GR as f32;
    pub const ZDR_TUMBLING_DB: f32 = crate::hca::MIN_ZDR_WS as f32;
    pub const ZDR_TUMBLING_ALPHA: f32 = PHI_ALPHA;
    pub const ZDR_NEGATIVE_DB: f32 = -3.0;
    pub const ZDR_COLUMN_DB: f32 = 3.0;

    /// The diverging centre the **isosurface** reads for ZDR — and, alone
    /// among the ZDR constants here, a display choice rather than a
    /// derivation. It is a bare literal because it derives from nothing;
    /// dressing it as a `crate::hca::…` reference would be the same false
    /// rationale this campaign exists to remove.
    ///
    /// 0.25 dB is where the shipped profile put its clear band, and it
    /// predates the rain-band argument above. That argument does not reach
    /// it, and it is deliberately **not** moved onto the HCA interval. The
    /// quiet band answers "which ZDR values discriminate nothing", which is
    /// a transparency question the classifier's own class kills settle; this
    /// constant answers "where does a `DeviationFrom` level set take its
    /// origin", which is a framing question the classifier has no opinion
    /// on.
    ///
    /// Holding it is not free of consequence, so the consequence is stated.
    /// [`default_iso_threshold`] is `ZDR_COLUMN_DB - ZDR_CENTRE_DB` = 2.75
    /// dB, which puts the default surface's positive lobe exactly on
    /// [`ZDR_COLUMN_DB`] — the same +3 dB the transparency profile above
    /// reaches full alpha at — and its negative lobe at −2.5. Re-centring on
    /// the rain band's midpoint of 1.25 dB moves that surface either way it
    /// is then read: hold the 2.75 dB span and the lobes go to +4.0 and
    /// −1.5, neither a landmark this module names; hold the
    /// `ZDR_COLUMN_DB -` derivation and the span shrinks to 1.75 dB, putting
    /// the negative lobe at −0.5 dB, inside the near-zero band the paragraph
    /// below says an isosurface is the wrong instrument for. Either is a
    /// user-visible change to what the default ZDR surface draws, and nothing
    /// has been measured that says the moved pair reads better. Until
    /// something has, this stays where it is, and the test pins both lobes so
    /// that a later move is a deliberate one.
    ///
    /// Not the centre of the quiet band above, and not used by the
    /// transparency profile at all — that one is two-sided and has no single
    /// centre. Kept separate because the near-zero hail signature is a band
    /// *around* this centre, and no `DeviationFrom` level set can enclose a
    /// band around its own centre: the isosurface draws big-drop columns and
    /// the rare negative tail, and the lit volume is the instrument for the
    /// hail value. Said here so the next reader does not mistake the two
    /// numbers for one that drifted.
    pub const ZDR_CENTRE_DB: f32 = 0.25;

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

    /// Storm-relative velocity keeps velocity's shape and velocity's numbers
    /// — but **not** velocity's justification, and the difference is the
    /// whole entry.
    ///
    /// [`crate::srv`] computes `SRV = V + speed·cos(direction − az)`. Set `V`
    /// to zero — air at rest over the ground — and what is left is a cosine
    /// in azimuth of amplitude equal to the storm's own speed. So the
    /// near-zero band of an SRV volume is *not* its background: it is the
    /// narrow ridge of azimuths perpendicular to the motion vector, plus
    /// whatever air happens to be travelling with the storm. Everywhere else
    /// ambient air reads well away from zero — for a 40 kt storm, still air
    /// 45° off the motion axis reads 14.6 m/s and this profile renders it
    /// about 73 % opaque — so an SRV volume grows two broad opacity lobes
    /// along the motion vector out of the vector alone.
    ///
    /// Those lobes are kept, deliberately. They are not an artefact of the
    /// transfer function; they are the entire content of the subtraction.
    /// SRV minus that ambient cosine is, term for term, base velocity, so a
    /// profile that suppressed them would be showing V under SRV's name — and
    /// the plan-view SRV palette colours exactly the same air for exactly the
    /// same reason. A volume that agrees with the plan view is the honest
    /// outcome.
    ///
    /// The alternative considered and rejected: widen the clear band to the
    /// storm speed, which is the tightest band that makes still air invisible
    /// at *every* azimuth. It fails in the direction this campaign forbids.
    /// Perpendicular to the motion the ambient contribution is zero, so a
    /// 10 m/s storm-relative flow there is unambiguous signal, and a band
    /// sized for the motion axis would erase it. Lighting ambient air is a
    /// false positive a forecaster can read past; deleting an inflow jet is
    /// not.
    ///
    /// Named here rather than aliased to velocity's constants at the profile
    /// table so that this argument is attached to SRV, and so moving one
    /// product's band cannot silently move the other's.
    pub const SRV_CLEAR_MS: f32 = VELOCITY_CLEAR_MS;
    pub const SRV_OPAQUE_MS: f32 = VELOCITY_OPAQUE_MS;

    /// Normalized rotation: clear under [`crate::nrot::SIGNIFICANT`], opaque at
    /// |1.0| and beyond — the mesocyclone convention GR pins its meso class
    /// to. A rotation volume is then a pair of standing columns where
    /// couplets stack.
    ///
    /// The clear point is taken **by reference** from the algorithm rather
    /// than chosen here, and that is the whole point of the constant. NROT's
    /// palette is class-structured by construction — the `.999`/`.499` stop
    /// trick spells out weak / significant / strong / very strong / extreme —
    /// so any clear point above the first class does not soften a gradient,
    /// it *relocates a class boundary*: everything the algorithm painted
    /// "weak" is moved into "nothing". A shipped 0.4 did exactly that,
    /// pushing the nothing→weak edge to ≈0.43 on the smoothstep and rendering
    /// 8 033 of the 8 039 voxels a real tornado-warned volume painted at a
    /// mean alpha of 2–4 out of 180: a forecaster saw rotation in the plan
    /// view and in the section, and an empty box in 3D.
    ///
    /// So the number belongs to `nrot`, not to this table. Both halves of the
    /// old justification were also false as written: the palette's first
    /// visible class is that constant, not 0.4, and the algorithm's own
    /// significance floor — what `despeckle_nrot` counts a bin as painted at
    /// — is that same constant.
    ///
    /// Aligning the constant alone is not enough, and this is the second half
    /// of the fix. A smoothstep leaving 0 at the clear point puts the bottom
    /// of the *weak class* at alpha 0.005 of the palette's 180 — which rounds
    /// to zero for the first several ramp indices, so the class boundary
    /// merely moves from 0.43 to about 0.27 and most of the class is still
    /// erased. For a gradient moment that would be a fade; for a
    /// class-structured one it is the same relocation in miniature. So the
    /// profile **steps where the palette steps**: nothing under the
    /// significance floor is drawn at all, everything at or over it starts at
    /// [`NROT_WEAK_ALPHA`] of the palette's own alpha, and the smoothstep
    /// ramps from there to full strength at the meso convention. A quarter is
    /// plainly visible against an empty box and plainly subordinate to a
    /// couplet at full strength; what it is not is a value the algorithm
    /// painted and the volume did not draw.
    pub const NROT_CLEAR: f32 = crate::nrot::SIGNIFICANT as f32;
    pub const NROT_OPAQUE: f32 = 1.0;
    pub const NROT_WEAK_ALPHA: f32 = 0.25;

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

/// How a product's isosurface threshold reads its scale — the per-product
/// twin of the transparency profile above, for the other view mode.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum IsoShape {
    /// The surface of `value >= threshold`: the sequential products, whose
    /// interesting side is up-scale.
    Sequential,
    /// The surface of `|value − centre| >= threshold`: the diverging
    /// products, whose interesting surfaces sit on *both* sides of their
    /// background — a velocity couplet is an inbound lobe and an outbound
    /// lobe, and an isosurface that drew only one would be half a picture.
    DeviationFrom { centre: f32 },
    /// The surface of `value <= threshold`: ρHV, whose background is the top
    /// of its scale. Implemented as a deviation from the ramp top, so the
    /// shader has one diverging test.
    AtOrBelow,
}

/// The isosurface shape per product. Same exhaustiveness rule as
/// [`volume_alpha_scale`]: a new product cannot inherit a shape.
pub fn iso_shape(product: RadarProduct) -> IsoShape {
    use volume_alpha_profile as p;
    match product {
        RadarProduct::Reflectivity
        | RadarProduct::SpectrumWidth
        | RadarProduct::DifferentialPhase
        | RadarProduct::SpecificDifferentialPhase => IsoShape::Sequential,
        RadarProduct::Velocity
        | RadarProduct::StormRelativeVelocity
        | RadarProduct::NormalizedRotation => IsoShape::DeviationFrom { centre: 0.0 },
        RadarProduct::DifferentialReflectivity => IsoShape::DeviationFrom {
            centre: p::ZDR_CENTRE_DB,
        },
        RadarProduct::CorrelationCoefficient => IsoShape::AtOrBelow,
        // Not renderable in 3D at all (`crate::derive::volume_slot`); the
        // shape is never read, and Sequential is the least surprising
        // answer if one is ever admitted without updating this table —
        // which the exhaustive match makes a compile error for new variants.
        RadarProduct::EchoTops
        | RadarProduct::EchoTopsInterpolated
        | RadarProduct::VerticallyIntegratedLiquid
        | RadarProduct::VilDensity
        | RadarProduct::ProbabilityOfSevereHail
        | RadarProduct::MaxExpectedHailSize
        | RadarProduct::HydrometeorClassification
        | RadarProduct::PrecipitationRate => IsoShape::Sequential,
    }
}

/// The default isosurface threshold per product, in the units
/// [`iso_shape`] gives the slider: a value for the sequential products, a
/// deviation for the diverging ones, a bound for ρHV.
///
/// * Reflectivity 18 dBZ — the class boundary GR2Analyst's isosurface
///   defaults near: the outline of precipitation proper, above clear-air
///   returns.
/// * Velocity / SRV 20 m/s — where the transparency profile reaches opaque:
///   the cores and couplets, free of ambient flow.
/// * Spectrum width 8 m/s — the profile's turbulence edge.
/// * ZDR 2.75 dB from the +0.25 dB display centre
///   ([`volume_alpha_profile::ZDR_CENTRE_DB`], a framing choice and not the
///   rain band's midpoint) — the big-drop column at +3 dB and the rare
///   negative tail at −2.5. **Not** the hail signature: that one is ZDR ≈ 0,
///   a band around this centre rather than beyond it, and a
///   `DeviationFrom` surface cannot enclose it. The lit volume shows it
///   ([`volume_alpha_profile::ZDR_TUMBLING_ALPHA`]); the isosurface does not.
/// * ΦDP 180° — mid-turn; a cumulative site-offset moment has no principled
///   default, and the slider is the instrument here.
/// * ρHV at or under 0.90 — the profile's opaque edge: the melting layer,
///   debris and non-meteorological surfaces.
/// * KDP 1.5 °/km — the profile's opaque edge: heavy-rain shafts.
/// * NROT 1.0 — the mesocyclone convention GR pins its meso class to.
pub fn default_iso_threshold(product: RadarProduct) -> f32 {
    use volume_alpha_profile as p;
    match product {
        RadarProduct::Reflectivity => 18.0,
        RadarProduct::Velocity => p::VELOCITY_OPAQUE_MS,
        RadarProduct::StormRelativeVelocity => p::SRV_OPAQUE_MS,
        RadarProduct::SpectrumWidth => p::SW_OPAQUE_MS,
        RadarProduct::DifferentialReflectivity => p::ZDR_COLUMN_DB - p::ZDR_CENTRE_DB,
        RadarProduct::DifferentialPhase => 180.0,
        RadarProduct::CorrelationCoefficient => p::CC_OPAQUE,
        RadarProduct::SpecificDifferentialPhase => p::KDP_OPAQUE_DEG_KM,
        RadarProduct::NormalizedRotation => p::NROT_OPAQUE,
        RadarProduct::EchoTops
        | RadarProduct::EchoTopsInterpolated
        | RadarProduct::VerticallyIntegratedLiquid
        | RadarProduct::VilDensity
        | RadarProduct::ProbabilityOfSevereHail
        | RadarProduct::MaxExpectedHailSize
        | RadarProduct::HydrometeorClassification
        | RadarProduct::PrecipitationRate => 0.0,
    }
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
        // Two-sided and asymmetric, not a deviation from one centre: the
        // quiet band is `[ZDR_RAIN_LO_DB, ZDR_RAIN_HI_DB]` and the two ways
        // out of it are different physics. See the profile entry.
        RadarProduct::DifferentialReflectivity => {
            if value >= p::ZDR_RAIN_LO_DB {
                smoothstep(p::ZDR_RAIN_HI_DB, p::ZDR_COLUMN_DB, value)
            } else {
                // The plateau: up to the tumbling value, then held there
                // until the deep negative tail earns full strength.
                let toward_tumbling =
                    1.0 - smoothstep(p::ZDR_TUMBLING_DB, p::ZDR_RAIN_LO_DB, value);
                let deep = 1.0 - smoothstep(p::ZDR_NEGATIVE_DB, p::ZDR_TUMBLING_DB, value);
                (1.0 - p::ZDR_TUMBLING_ALPHA)
                    .mul_add(deep, p::ZDR_TUMBLING_ALPHA * toward_tumbling)
                    .min(1.0)
            }
        }
        RadarProduct::CorrelationCoefficient => 1.0 - smoothstep(p::CC_OPAQUE, p::CC_CLEAR, value),
        RadarProduct::DifferentialPhase => p::PHI_ALPHA,
        // The derived products, admitted by `crate::derive`. SRV carries
        // velocity's numbers under its own names and its own argument — read
        // the profile entry before moving either.
        RadarProduct::StormRelativeVelocity => {
            smoothstep(p::SRV_CLEAR_MS, p::SRV_OPAQUE_MS, value.abs())
        }
        // Stepped, not faded, at the significance floor: NROT's palette is
        // class-structured, so the volume must go visible exactly where the
        // plan view does. See the profile entry.
        RadarProduct::NormalizedRotation => {
            let magnitude = value.abs();
            if magnitude < p::NROT_CLEAR {
                0.0
            } else {
                (1.0 - p::NROT_WEAK_ALPHA)
                    .mul_add(
                        smoothstep(p::NROT_CLEAR, p::NROT_OPAQUE, magnitude),
                        p::NROT_WEAK_ALPHA,
                    )
                    .min(1.0)
            }
        }
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

/// One y row's share of a grid under construction: the row's `nx` cells in
/// each of the `nz` horizontal planes, cut out of the output so the row can be
/// filled without touching anything another row owns.
///
/// See [`build_voxels_with_motion`] for why the cut is `nz` slices and not one.
struct VoxelRow<'grid> {
    iy: usize,
    indices: Vec<&'grid mut [u8]>,
    /// Empty unless [`VoxelRequest::values_wanted`] asked for the value plane,
    /// which is what makes the write below a `get_mut` rather than an index.
    values: Vec<&'grid mut [f32]>,
}

/// A box of `half` about `centre`, as `(x_range_km, y_range_km)` —
/// kilometres east and north **of the radar**, which is the frame
/// [`VoxelGrid::x_range_km`] and [`VoxelGrid::y_range_km`] report in.
///
/// The extent is taken as already decided, and **already clamped**:
/// [`build_voxels`] resolves the request's `Option` first, because its `None`
/// arm is the volume's own reach and only the scan can answer it, and puts a
/// `Some` through [`HalfExtentKm::clamped`]. A caller that has a picked region
/// in hand must do the same, and through that same function — see below.
///
/// # Why this is a named function and not four lines inside the resampler
///
/// [`build_voxels`] is the only thing that *can* produce a grid, and it takes
/// hundreds of milliseconds. The renderer needs the same two ranges for a box
/// whose grid has not been built yet: while a rebuild is in flight it draws
/// the grid it already has, cropped to the box the user has since asked for,
/// and the crop is the affine between this answer and the held grid's own
/// ranges. Computed a second time anywhere else, a disagreement of even a
/// kilometre would put the volume off its floor — and would then *snap* into
/// place when the real build landed, which is precisely the discontinuity the
/// stand-in exists to remove. So the resampler calls this too, and the two
/// cannot drift.
///
/// **The clamp is inside that guarantee, not beside it.** `DrawnBox::for_target`
/// compares its answer against the settled grid's own ranges with `==` and
/// draws the identity affine when they match, so the two sides have to agree
/// *bit for bit* rather than nearly. A separate `clamp(MIN_HALF_WIDTH_KM,
/// MAX_HALF_WIDTH_KM)` on the renderer's side is not that agreement: for a
/// square ask past the stop it answers `470 / √2` where [`HalfExtentKm::clamped`]
/// answers `half · (470 / hypot(half, half))`, and the two differ by one or two
/// ULP — measured, 332.3401871576773 against 332.34018715767735 for a 400 km
/// ask and 332.3401871576774 for the ~1800 km one a wide-open pane measures.
///
/// **That divergence is reachable, and the route to it is not the obvious
/// one.** Two sides both under [`MAX_HALF_WIDTH_KM`] cannot have a corner past
/// [`MAX_HALF_DIAGONAL_KM`] — the second is the first times √2, so
/// `hypot(a, b) ≤ hypot(MAX_W, MAX_W) = MAX_DIAG` for every such pair, and no
/// ask can slip past a per-axis clamp and be caught by the corner one. What
/// makes the two spellings differ is the opposite: `clamped` lands the corner
/// *on* the bound, which puts each side of a square box one ULP **above**
/// `MAX_HALF_WIDTH_KM`. A renderer that re-clamped per axis would shave that
/// ULP off a box `build_voxels` keeps, and the pane would sit permanently on
/// the crop path with a near-identity affine instead of on the settled grid.
/// `VolumeRegion::new` running this same function is what makes such a box
/// storable, so the hazard is live rather than hypothetical.
///
/// Polar from the site and back, so this is the same tangent plane the
/// resampler's per-cell mapping uses and a centre *at* the site lands exactly
/// on `(0, 0)`.
///
/// # The lattice is not anchored, and that was measured rather than assumed
///
/// Cell centres sit at `x0 + (i + 0.5) · pitch`, so a box that moves or
/// resizes puts them over different ground: two builds of one volume at two
/// boxes sample it on lattices with no common point. The obvious repair is to
/// snap the box so a cell centre lands exactly on the radar, which every pitch
/// would then share — and at a 4:1 pitch ratio that nests the coarse lattice
/// strictly inside the fine one. **It is not worth doing.**
///
/// Measured on a real KTLX volume, drawing a held 100 km grid cropped into a
/// freshly requested 25 km box and comparing it against the grid built for
/// that box — the moment a zoom hands over:
///
/// ```text
/// unanchored          33.975 mean |dRGB|/255, 83.2% of pixels past 8/255
/// both axes anchored  33.470                  82.7%
/// ```
///
/// 1.5% on the product palette, 2.5% at a quarter-cell march step, 2.7% on a
/// smooth ramp; the alpha-weighted centroid delta gets *worse*, 0.403 to 0.628
/// px. The mechanism fires exactly as the arithmetic says — in texel
/// coordinates the unanchored crop reads `0.25·p + 96.125`, so no pixel ever
/// lands on a coarse texel centre, and anchored reads `0.25·p + 96.375`, so
/// every fourth one lands dead on `i + 0.5`, residual phase zero to nine
/// places. It simply buys half a unit of thirty-four.
///
/// The control beside it is why. Moving the *settled* box by half a fine cell
/// — 98 metres — and rebuilding changes the picture by 28.090, **83% of the
/// whole handover discontinuity, with no stand-in involved at all.** A
/// resampled field is that sensitive to any sub-cell change of its box. Lattice
/// phase is not a fixable relationship between two grids: at a 4:1 ratio the
/// held grid is four times too coarse for the box it is drawn in, and no
/// alignment repairs a resolution deficit.
///
/// The cost avoided: the snap has to go *inward* to keep the containment the
/// floor depends on, so it needs the per-axis cell counts threaded through this
/// signature, and every grid fixture's ranges move — all for a change nobody
/// can see.
///
/// # And the one regime where it would have paid does not exist
///
/// The measurement above is one camera, one site, one 4:1 zoom. The regime
/// anchoring would genuinely win is a pitch **ratio of 1** — two boxes of the
/// same width at a sub-cell offset. There it does not merely align the two
/// lattices, it makes them the same lattice, so the resampled field comes out
/// identical and the artefact is zero rather than smaller. An isolated
/// half-cell phase change at equal pitch was worth 21.19 of the units above,
/// which is the whole reason to look.
///
/// That regime is the live rebuild: a 3D pane reseals every volume for as long
/// as it is open, at the same width, and the box centre is the viewport's,
/// which nothing quantises — only the half-width is held to
/// `ui_region::HALF_WIDTH_STEP_KM`. So the fear is that every sweep resamples a
/// fraction of a cell from the last and the bands re-shuffle under a user who
/// is not touching anything.
///
/// **They do not, because the centre does not move either.** It is unprojected
/// from the pane rect and the map memory, and both are still when nobody moves
/// them, so the region comes out bit-identical frame after frame and every
/// reseal lands on the very same lattice. `rustdar_egui`'s
/// `a_settled_pane_asks_for_one_box_for_ever` is what says so — 120 input-free
/// frames naming one box, with a wheel notch beside it as the control that the
/// readback can see a change at all. There is nothing at ratio 1 for anchoring
/// to win.
///
/// A **pan** does move the centre continuously, and that is the last place
/// anchoring could have applied. It would hurt there: snapping quantises the
/// box's ground position to whole cells, so the volume's coverage would lag the
/// viewport by up to a cell and then jump — against a floor that pans smoothly,
/// and in flat contradiction of the region *being* the viewport. The
/// re-colouring it would trade that for is invisible under a gesture that is
/// already moving the whole picture.
pub fn horizontal_ranges_km(
    centre: (f64, f64),
    half: HalfExtentKm,
    site_lat: f64,
    site_lon: f64,
) -> ((f64, f64), (f64, f64)) {
    let (bearing_deg, range_km) =
        beam::site_bearing_range_km(site_lat, site_lon, centre.0, centre.1);
    let bearing = bearing_deg.to_radians();
    let (cx, cy) = (range_km * bearing.sin(), range_km * bearing.cos());
    (
        (cx - half.east_km, cx + half.east_km),
        (cy - half.north_km, cy + half.north_km),
    )
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
/// A `half_extent_km` of `Some` outside [`HalfExtentKm::clamped`]'s bounds is
/// **clamped**, not refused; a `half_extent_km` of `None` is answered by
/// [`box_half_width_km`] off the volume's own reach, on both axes.
pub fn build_voxels<'a>(
    volume: impl Into<crate::nyquist::Volume<'a>>,
    req: &VoxelRequest,
    lat: f64,
    lon: f64,
) -> Option<VoxelGrid> {
    build_voxels_with_motion(volume, req, lat, lon, None)
}

/// [`build_voxels`] with the user's storm motion override
/// `(speed_kt, direction_from_deg)`, read only when the product is
/// storm-relative velocity. Separate entry point rather than a request field
/// so the override never rides the voxel job's wire encoding — the worker
/// reads it off the `RenderInput`, which already carries it.
pub fn build_voxels_with_motion<'a>(
    volume: impl Into<crate::nyquist::Volume<'a>>,
    req: &VoxelRequest,
    lat: f64,
    lon: f64,
    storm_motion_override: Option<(f32, f32)>,
) -> Option<VoxelGrid> {
    let volume = volume.into();
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
    if !(req.half_extent_km.is_none_or(HalfExtentKm::is_finite)
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
    let prepared = crate::derive::prepare(volume, req.product, storm_motion_override)?;
    // The declared Nyquist table follows the scan through the derivation: it
    // is keyed by elevation number, which `prepare` preserves, and a derived
    // scan's rungs are the same cuts flown at the same PRFs. `prepare` reads
    // the same table on the way in — SRV and NROT unfold around the limit each
    // cut declared — so the field that arrives here was built against the
    // limits this sampler is about to guard on.
    let declared = volume.declared_nyquist();
    let sampler = match &prepared {
        crate::derive::Prepared::Native(scan) => {
            VolumeSampler::new(crate::nyquist::Volume::new(scan, declared), req.product).ok()?
        }
        crate::derive::Prepared::Derived(scan) => VolumeSampler::for_derived(
            crate::nyquist::Volume::new(scan, declared),
            req.product,
            slot,
        )
        .ok()?,
    };

    // The box's extent, decided once and here — the counterpart of
    // `render_with_projection` deciding the raster's extent once, and for the
    // same reason: the ranges the grid reports, the cells the sampler is asked
    // for and the km-per-cell the pane prints all have to come off one pair of
    // numbers.
    //
    // The reach is measured on the volume's own scan rather than on `prepared`:
    // a derivation rewrites a moment's *values*, never its gate geometry, and
    // reading it here keeps the box a property of the volume the user asked for
    // rather than of whichever derivation the product happens to go through.
    let half = match req.half_extent_km {
        Some(picked) => picked.clamped(),
        None => HalfExtentKm::square(box_half_width_km(volume_reach_km(
            volume.scan(),
            req.product,
        ))),
    };

    // Placed through the one definition of it — see `horizontal_ranges_km` for
    // why the renderer has to be able to ask the same question without a grid
    // in hand.
    let (x_range_km, y_range_km) = horizontal_ranges_km(req.centre, half, lat, lon);
    let z_range_km_msl = (req.base_km_msl, req.top_km_msl);

    // The same spelling `render.rs` uses for `radar_km_msl`, including what it
    // does with coordinates the table cannot place: no MSL datum to add, so the
    // grid's heights are above the antenna. See `render::render_site_height_ft`.
    let site_km_msl = crate::eet::radar_height_ft_near(lat, lon, crate::sites::Datum::Feedhorn)
        .unwrap_or(0.0)
        * 0.0003048;

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

    // One task per **y row**, with that row's output cut out of the grid and
    // handed to it before anything runs.
    //
    // The cut is not one slice. The grid is z-major — cell `(ix, iy, iz)` sits
    // at `iz·ny·nx + iy·nx + ix` — so a row is `nz` runs of `nx`, one per
    // horizontal plane, and `chunks_mut` twice is what names them. Doing that up
    // front is the whole reason the rows can run at once: a task holds `&mut`
    // slices no other task can reach, so no cell is written twice and nothing is
    // summed across tasks. There is no reduction here and no shared accumulator,
    // which is why the answer is the serial loop's cell for cell — see
    // [`tests::the_rows_build_the_grid_the_one_buffer_serial_loop_built`].
    //
    // The `Column` buffer moves with the row rather than being shared by the
    // whole grid, which is what forced this loop serial. It is still allocated
    // once per `nx` columns, so [`crate::sampler::VolumeSampler::column_into`]'s
    // reason for existing survives — a raster sweeping many columns does not
    // allocate per column. `column_into` clears and overwrites every field, so a
    // row's first column starts from exactly the state the serial loop's would.
    let mut rows: Vec<VoxelRow<'_>> = (0..ny)
        .map(|iy| VoxelRow {
            iy,
            indices: Vec::with_capacity(nz),
            values: Vec::new(),
        })
        .collect();
    for plane_cells in indices.chunks_mut(plane) {
        for (iy, row) in plane_cells.chunks_mut(nx).enumerate() {
            rows[iy].indices.push(row);
        }
    }
    if let Some(values) = values.as_mut() {
        for plane_cells in values.chunks_mut(plane) {
            for (iy, row) in plane_cells.chunks_mut(nx).enumerate() {
                rows[iy].values.push(row);
            }
        }
    }

    rows.into_par_iter().for_each(|mut row| {
        let y_km = axis_centre(y_range_km, ny, row.iy);
        let mut column = Column::new();
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
                row.indices[iz][ix] = ramp_index(value_range, value);
                if let Some(plane_cells) = row.values.get_mut(iz) {
                    plane_cells[ix] = value;
                }
            }
        }
    });

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
///
/// **Whenever a renderer change tempts a bump, read
/// `the_format_version_is_the_one_this_layout_ships` first.** It records which
/// changes oblige one and which do not, and why the frontend's
/// coverage-premultiplied `Rg16Float` volume texture — a quadrupling of the
/// GPU grid, over two changes — did not: coverage is `index != NO_DATA_INDEX`,
/// synthesised at upload, and the half-float widening that followed is a
/// property of the sampler's arithmetic, so not one byte here changed in
/// layout or in meaning. The obligation is on
/// the bytes, not on what reads them.
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
mod tests;
