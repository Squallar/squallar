//! The raymarch's uniform block, packed by hand.
//!
//! # Why by hand
//!
//! `rustdar-frontend` **is** `#![forbid(unsafe_code)]` (`lib.rs:2`), and
//! `forbid` cannot be lifted by an inner `allow`, so `bytemuck`'s derive — which
//! emits a bare `unsafe impl` with no allow of its own — is genuinely barred
//! here. (An earlier draft of this comment had that backwards, and named
//! `rustdar-egui` as the crate with the attribute; it does not have one.)
//!
//! But impossibility is not the reason, because there is a way round it:
//! `f32` is already `Pod`, so `[f32; 40]` plus `cast_slice` needs no derive and
//! no `unsafe`. The reason to write the bytes out anyway is **testability**. A
//! hand-written `to_bytes` makes every std140 offset an assertable number
//! rather than a property of a `#[repr(C)]` a reviewer has to trust. A
//! transposed matrix or a swapped pair of `vec4`s is exactly the sort of
//! mistake that produces a plausible-looking image.
//!
//! `every_lane_lands_at_its_std140_offset` is what catches it — and note it
//! only does so because it pins the offsets as **literals** first. Indexing
//! with the same `OFFSET_*` constants `to_bytes` writes at would move reader
//! and writer together, which is a test that cannot see the mistake it is
//! named for.
//!
//! # The layout
//!
//! One `mat4x4<f32>` and six `vec4<f32>`: 160 bytes, all naturally 16-byte
//! aligned, so std140 inserts no padding of its own.
//!
//! | offset | member              |
//! |-------:|---------------------|
//! |      0 | `box_from_clip`     |
//! |     64 | `eye_in_box`        |
//! |     80 | `box_size_km`       |
//! |     96 | `grid_dims`         |
//! |    112 | `light_dir_ambient` |
//! |    128 | `transfer`          |
//! |    144 | `flags`             |
//!
//! Lanes the shader does not read are written as **zero** rather than left to
//! whatever was there. A uniform buffer is reused across frames, so a reserved
//! lane that is never written is a stale value waiting for the day someone adds
//! a field and reads it before writing it.

use crate::constants::VOLUME_LUT_BYTES;

/// Bytes in the uniform block. One `mat4x4<f32>` + six `vec4<f32>`.
pub const VOLUME_UNIFORM_BYTES: usize = 160;

/// `f32` lanes in the uniform block.
pub const VOLUME_UNIFORM_LANES: usize = VOLUME_UNIFORM_BYTES / 4;

/// Byte offset of each member, in declaration order. Public because the
/// pipeline's minimum-binding-size assertion and the tests both name them.
pub const OFFSET_BOX_FROM_CLIP: usize = 0;
/// See [`OFFSET_BOX_FROM_CLIP`].
pub const OFFSET_EYE_IN_BOX: usize = 64;
/// See [`OFFSET_BOX_FROM_CLIP`].
pub const OFFSET_BOX_SIZE_KM: usize = 80;
/// See [`OFFSET_BOX_FROM_CLIP`].
pub const OFFSET_GRID_DIMS: usize = 96;
/// See [`OFFSET_BOX_FROM_CLIP`].
pub const OFFSET_LIGHT_DIR_AMBIENT: usize = 112;
/// See [`OFFSET_BOX_FROM_CLIP`].
pub const OFFSET_TRANSFER: usize = 128;
/// See [`OFFSET_BOX_FROM_CLIP`].
pub const OFFSET_FLAGS: usize = 144;

/// Extinction per kilometre at a palette entry whose alpha is 1.
///
/// Chosen so that a kilometre of the most opaque colour in the table absorbs
/// about 63% of the light through it (`1 - exp(-1)`), which makes a 20 km deep
/// storm core read as solid without turning a 240 km wide box into fog. It is a
/// presentation constant, not a physical one — there is no radiative transfer
/// happening here, only alpha compositing that happens to use the same algebra.
pub const DEFAULT_EXTINCTION_PER_KM: f32 = 1.0;

/// Palette indices at or below which a cell is skipped entirely.
///
/// **The palette's own transparent run, not an emptiness test.** Empty air is
/// excluded by the reconstructed *coverage* (`volume.wgsl`'s `COVERAGE_FLOOR`),
/// which is a property of the measurement rather than of the table, so this
/// lane's whole job is the run of fully transparent entries at the bottom of
/// the ramp: below it a sample's LUT entry absorbs nothing, so the march can
/// skip it and its up-to-seven shading fetches without changing a pixel.
///
/// The default is the half-texel that selects exactly index 0 — which a
/// covered sample can no longer reconstruct to, since `R̄ / Ḡ` lies in the
/// convex hull of stored indices and `ramp_index` clamps every measurement to
/// `1..=255`, so at the default this lane skips nothing and costs nothing.
/// The production value comes from the effective fade band
/// (`volume::bridge::empty_index_threshold_for`). Raising it trades faint
/// returns for fill rate; setting it below zero disables the skip, which is
/// how the spike measured the un-skipped worst case.
pub const DEFAULT_EMPTY_INDEX_THRESHOLD: f32 = 0.5 / 255.0;

/// Transmittance below which the march stops.
///
/// 0.004 is under one part in 255, so nothing behind it could change the
/// eight-bit result. Setting it to zero disables the early-out, which is the
/// other half of the spike's worst case.
pub const DEFAULT_EARLY_OUT_TRANSMITTANCE: f32 = 0.004;

/// Width of the opacity ramp above [`VolumeUniform::empty_index_threshold`],
/// in the shader's 0-1 index units. **Zero here, deliberately.**
///
/// Zero is the hard threshold every mask-instrument test was written against:
/// with it, a cell contributes its full palette alpha the moment the
/// interpolated index clears the threshold, which is what makes a saturating
/// extinction render a binary silhouette. The *production* width lives in
/// `volume::bridge`, which anchors the threshold at the palette's own fade
/// boundary and widens the ramp — see `EDGE_SOFT_WIDTH` there for the number
/// and the measurement behind it. A soft default here would put a grey band on
/// every instrument's edge instead — observed, not asserted: the index-1 shape
/// in `tests/volume_silhouette.rs`'s mask-instrument test greys hundreds of
/// boundary pixels the moment this is not zero.
pub const DEFAULT_EDGE_SOFT_WIDTH: f32 = 0.0;

/// Fraction of a lit surface's colour that survives facing away from the light.
///
/// Shading multiplies colour by `ambient + (1 - ambient) * lambert`, so this is
/// the floor. Zero would make away-facing cells black rather than dark, which
/// on a volume with no opaque surfaces reads as holes.
pub const DEFAULT_AMBIENT: f32 = 0.35;

/// The camera-relative light direction the volume is lit from, in box space.
///
/// Up and over the viewer's left shoulder, which is the convention GR2Analyst's
/// 3D view uses and the one that makes an overshooting top read as a bump
/// rather than a dent. Not normalised here — the shader normalises it, so a
/// caller cannot make the light vanish by handing over a short vector.
pub const DEFAULT_LIGHT_DIR: [f32; 3] = [-0.4, -0.5, 0.77];

/// Everything the raymarch reads that is not a texture.
///
/// Deliberately plain data with no wgpu in it, so the packing is unit-testable
/// on a machine with no GPU — which is every CI row this repository has. The
/// `gpu` job does render, but on a software rasteriser, and it is not where a
/// packing bug should first be caught.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VolumeUniform {
    /// Clip space to box space, **column-major**: `box_from_clip[c][r]`.
    ///
    /// Column-major is WGSL's own convention for `mat4x4<f32>` and std140's, so
    /// the four `[f32; 4]`s go out in order with no transpose. Getting this
    /// backwards produces a camera that responds to drags in the wrong axis,
    /// which is easy to mistake for a sign error in the orbit maths.
    pub box_from_clip: [[f32; 4]; 4],
    /// The perspective eye, in box space.
    pub eye_in_box: [f32; 3],
    /// The box's physical extent in kilometres.
    pub box_size_km: [f32; 3],
    /// The camera's vertical exaggeration, `>= 1`. Rides `box_size_km.w`.
    ///
    /// Read by exactly one thing: the gradient shading, which takes its
    /// normals against the *displayed* geometry so a slope drawn steep is lit
    /// steep. Optical depth stays against the true `box_size_km` — the stretch
    /// is a drawing convention and must never reach a measurement, which is
    /// the same line `OrbitCamera::vertical_exaggeration` draws and the
    /// `optical_depth_is_measured_against_the_unexaggerated_box` GPU test
    /// pins. At 1.0 (the default) displayed and true geometry coincide.
    ///
    /// Never zero: the shader divides a cell extent by nothing else, and a
    /// zero here would make every vertical difference infinite and every
    /// normal NaN. The one production writer copies
    /// `OrbitCamera::vertical_exaggeration`, whose floor is 1.
    pub vertical_exaggeration: f32,
    /// Voxels along each axis.
    pub grid_dims: [u32; 3],
    /// Light direction in box space. Normalised by the shader.
    pub light_dir: [f32; 3],
    /// Ambient floor, 0..1. See [`DEFAULT_AMBIENT`].
    pub ambient: f32,
    /// See [`DEFAULT_EXTINCTION_PER_KM`].
    pub extinction_per_km: f32,
    /// See [`DEFAULT_EMPTY_INDEX_THRESHOLD`].
    pub empty_index_threshold: f32,
    /// See [`DEFAULT_EARLY_OUT_TRANSMITTANCE`].
    pub early_out_transmittance: f32,
    /// See [`DEFAULT_EDGE_SOFT_WIDTH`]. Rides `transfer.w`.
    pub edge_soft_width: f32,
    /// Whether to shade with the central-difference gradient. The expensive
    /// knob: seven texture fetches per step against one, measured at 2.4x.
    pub gradient_shading: bool,
    /// Cells one march step advances along the ray, in the grid's own
    /// anisotropic cell metric. Rides `flags.z`.
    ///
    /// The default is 1 — one sample per cell, the rate the linear filter's
    /// band limit supports and the value the silhouette harness's host-side
    /// mirror marches at (`volume::raymarch::RAYMARCH_STEP_CELLS`; the two
    /// are pinned together). The cloud rung halves it: a finer step buys no
    /// resolution from a band-limited field, but it halves the per-step
    /// opacity quantum, which is what takes the stratified jitter's residual
    /// from a visible stipple to noise below the eight-bit level. Zero is
    /// safe by construction — the shader's dt floor against the step ceiling
    /// covers the span in ceiling-many steps rather than hanging — but no
    /// writer produces it.
    pub step_cells: f32,
    /// Whether the march draws the map floor at the box's bottom face. Rides
    /// `flags.w`.
    ///
    /// **`false` here, deliberately**, like every other production knob on
    /// this struct: the floor moves alpha on every ray that meets the ground,
    /// so the instrument default is no floor, and the bridge sets this only
    /// when it also bound a real floor texture at group 1 — a flag raised
    /// over the placeholder would compositate a transparent ground, which is
    /// a no-op but a lie about what was drawn.
    pub map_floor: bool,
    /// The mip level the march reconstructs the field at, `0..=1`. Rides
    /// `flags.y`.
    ///
    /// The grid texture carries one hand-built level below the raw field —
    /// each coarse texel the box mean of its eight fine ones, in both
    /// channels, which under the shader's `R̄ / Ḡ` reconstruction is the
    /// occupancy-weighted mean of the index, to under 4 index units (both
    /// channels quantise to u8 before the shader divides, and the divisor
    /// steps in units of 255/8; see `volume::raymarch::downsampled_grid`) —
    /// and the sampler blends between the two, so this is a continuous
    /// softness knob at no extra fetches: 0 is the raw trilinear tent, 1 is a
    /// two-cell box convolved with a tent.
    ///
    /// **Zero here, deliberately**, for exactly the reason
    /// [`DEFAULT_EDGE_SOFT_WIDTH`] is zero: the raw field is the instrument
    /// configuration every mask harness measures against — at LOD exactly 0
    /// the coarse level's filter weight is exactly zero — and any smoothing
    /// moves alpha at every boundary. The production value lives in
    /// `volume::bridge`, beside the soft width, and rides the same quality
    /// rung as the lighting: together they are the cloud look, and the floor
    /// rung stays the jagged-unlit raw march.
    ///
    /// **The isosurface march is always 0**, on every rung. The knob is a
    /// presentation softness for an *integrated* field; an isosurface is a
    /// level set, so smoothing the field moves the surface rather than
    /// softening its rendering, and `volume.wgsl`'s `COVERAGE_FLOOR` is a
    /// statement about the raw tent that erases sub-kernel features at any
    /// level above it. `volume::bridge`'s isosurface branch holds the
    /// reasoning and the measurement.
    ///
    /// **Never negative.** A negative value used to be a sentinel selecting a
    /// nearest-neighbour snap, for the seven products whose no-data boundary a
    /// plain `R8Unorm` filter could not be trusted across. The volume texture
    /// is coverage-premultiplied `Rg8Unorm` now, so a filtered sample beside
    /// empty air can no longer be dragged anywhere the data was not; all nine
    /// products take one path and the sentinel is gone with the split.
    pub reconstruction_lod: f32,
    /// The isosurface threshold in the shader's 0-1 index units, or negative
    /// for the lit-volume march. Rides `eye_in_box.w`, one of the two lanes
    /// that were reserved-zero before the view-mode work.
    ///
    /// Negative — not zero — is the lit-volume sentinel, because an index-0
    /// threshold is a real configuration ("the surface of any data at all").
    /// [`ISO_OFF`] is the sentinel every writer uses.
    ///
    /// In isosurface mode the march paints the first crossing of the
    /// threshold as an opaque, gradient-lit surface and stops. The threshold
    /// reads the **data** (the interpolated palette index), never the LUT's
    /// alpha — a Volume Alpha curve restyles the lit volume and leaves the
    /// isosurface where the values put it, which the UI says in as many
    /// words.
    pub iso_threshold: f32,
    /// The centre index a **diverging** product's isosurface measures its
    /// threshold from, in 0-1 index units, or negative for a sequential
    /// product whose threshold reads the index directly. Rides `grid_dims.w`,
    /// the other formerly-reserved lane.
    ///
    /// A diverging moment's interesting surfaces sit on *both* sides of its
    /// background — a velocity couplet is an inbound lobe and an outbound
    /// lobe — so its crossing test is `|index − centre| >= threshold`, which
    /// renders both lobes, each wearing its own palette colour. ρHV rides
    /// this lane too, with its centre at the **top** of its ramp, so "at or
    /// under a bound" is the same test. Only read in isosurface mode.
    pub iso_centre: f32,
}

/// The lit-volume sentinel for [`VolumeUniform::iso_threshold`] and the
/// sequential sentinel for [`VolumeUniform::iso_centre`].
pub const ISO_OFF: f32 = -1.0;

impl VolumeUniform {
    /// A uniform with the defaults above, an identity transform and no camera.
    ///
    /// Not `Default::default()`: an all-zero `box_from_clip` and an all-zero
    /// `grid_dims` are both degenerate (the latter divides by zero in the
    /// gradient), and a derived `Default` would hand them out silently.
    pub fn new(box_size_km: [f32; 3], grid_dims: [u32; 3]) -> Self {
        Self {
            box_from_clip: IDENTITY,
            eye_in_box: [0.5, 0.5, 4.0],
            box_size_km,
            vertical_exaggeration: 1.0,
            grid_dims,
            light_dir: DEFAULT_LIGHT_DIR,
            ambient: DEFAULT_AMBIENT,
            extinction_per_km: DEFAULT_EXTINCTION_PER_KM,
            empty_index_threshold: DEFAULT_EMPTY_INDEX_THRESHOLD,
            early_out_transmittance: DEFAULT_EARLY_OUT_TRANSMITTANCE,
            edge_soft_width: DEFAULT_EDGE_SOFT_WIDTH,
            gradient_shading: true,
            step_cells: 1.0,
            reconstruction_lod: 0.0,
            map_floor: false,
            iso_threshold: ISO_OFF,
            iso_centre: ISO_OFF,
        }
    }

    /// The 160 bytes the GPU reads, little-endian.
    ///
    /// Little-endian unconditionally: every target wgpu supports is
    /// little-endian, and `to_le_bytes` says so at the call site rather than
    /// depending on the host happening to agree.
    pub fn to_bytes(&self) -> [u8; VOLUME_UNIFORM_BYTES] {
        let mut out = [0u8; VOLUME_UNIFORM_BYTES];

        for (column, values) in self.box_from_clip.iter().enumerate() {
            write_vec4(&mut out, OFFSET_BOX_FROM_CLIP + column * 16, *values);
        }
        write_vec4(
            &mut out,
            OFFSET_EYE_IN_BOX,
            xyz_w(self.eye_in_box, self.iso_threshold),
        );
        write_vec4(
            &mut out,
            OFFSET_BOX_SIZE_KM,
            xyz_w(self.box_size_km, self.vertical_exaggeration),
        );
        write_vec4(
            &mut out,
            OFFSET_GRID_DIMS,
            xyz_w(self.grid_dims.map(|n| n as f32), self.iso_centre),
        );
        write_vec4(
            &mut out,
            OFFSET_LIGHT_DIR_AMBIENT,
            xyz_w(self.light_dir, self.ambient),
        );
        write_vec4(
            &mut out,
            OFFSET_TRANSFER,
            [
                self.extinction_per_km,
                self.empty_index_threshold,
                self.early_out_transmittance,
                self.edge_soft_width,
            ],
        );
        write_vec4(
            &mut out,
            OFFSET_FLAGS,
            [
                f32::from(u8::from(self.gradient_shading)),
                self.reconstruction_lod,
                self.step_cells,
                f32::from(u8::from(self.map_floor)),
            ],
        );

        out
    }
}

/// The identity, column-major.
const IDENTITY: [[f32; 4]; 4] = [
    [1.0, 0.0, 0.0, 0.0],
    [0.0, 1.0, 0.0, 0.0],
    [0.0, 0.0, 1.0, 0.0],
    [0.0, 0.0, 0.0, 1.0],
];

fn xyz_w(xyz: [f32; 3], w: f32) -> [f32; 4] {
    [xyz[0], xyz[1], xyz[2], w]
}

fn write_vec4(out: &mut [u8; VOLUME_UNIFORM_BYTES], at: usize, values: [f32; 4]) {
    for (lane, value) in values.into_iter().enumerate() {
        let start = at + lane * 4;
        out[start..start + 4].copy_from_slice(&value.to_le_bytes());
    }
}

/// The number of palette entries the shader indexes, taken from the byte
/// budget the table travels in. See `the_shader_and_the_lut_constant_agree`.
pub const LUT_ENTRIES: usize = VOLUME_LUT_BYTES / 4;

#[path = "volume_uniform/tests.rs"]
#[cfg(test)]
mod tests;
