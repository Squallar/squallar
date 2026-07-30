//! The raymarch's uniform block, packed by hand.
//!
//! # Why by hand
//!
//! `rustdar-frontend` is not `forbid(unsafe_code)` — `rustdar-egui` is — so
//! `bytemuck` is not literally barred here, and `f32` is already `Pod`, which
//! means `[f32; 40]` plus `cast_slice` would need no derive and no `unsafe`.
//! The reason to write the bytes out anyway is **testability**: a hand-written
//! `to_bytes` makes every std140 offset an assertable number rather than a
//! property of a `#[repr(C)]` a reviewer has to trust. A transposed matrix or a
//! swapped pair of `vec4`s is exactly the sort of mistake that produces a
//! plausible-looking image, and `every_lane_lands_at_its_std140_offset` is what
//! catches it.
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
/// Index 0 is the bottom of the affine ramp *and* the no-data value (WP-C), so
/// this is the half-texel that selects exactly index 0. Raising it trades
/// faint returns for fill rate; setting it below zero disables the skip, which
/// is how the spike measured the un-skipped worst case.
pub const DEFAULT_EMPTY_INDEX_THRESHOLD: f32 = 0.5 / 255.0;

/// Transmittance below which the march stops.
///
/// 0.004 is under one part in 255, so nothing behind it could change the
/// eight-bit result. Setting it to zero disables the early-out, which is the
/// other half of the spike's worst case.
pub const DEFAULT_EARLY_OUT_TRANSMITTANCE: f32 = 0.004;

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
/// on a machine with no GPU — which is every CI row this repository has.
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
    /// Whether to shade with the central-difference gradient. The expensive
    /// knob: seven texture fetches per step against one, measured at 2.4x.
    pub gradient_shading: bool,
}

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
            grid_dims,
            light_dir: DEFAULT_LIGHT_DIR,
            ambient: DEFAULT_AMBIENT,
            extinction_per_km: DEFAULT_EXTINCTION_PER_KM,
            empty_index_threshold: DEFAULT_EMPTY_INDEX_THRESHOLD,
            early_out_transmittance: DEFAULT_EARLY_OUT_TRANSMITTANCE,
            gradient_shading: true,
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
        write_vec4(&mut out, OFFSET_EYE_IN_BOX, xyz_w(self.eye_in_box, 0.0));
        write_vec4(&mut out, OFFSET_BOX_SIZE_KM, xyz_w(self.box_size_km, 0.0));
        write_vec4(
            &mut out,
            OFFSET_GRID_DIMS,
            xyz_w(self.grid_dims.map(|n| n as f32), 0.0),
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
                0.0,
            ],
        );
        write_vec4(
            &mut out,
            OFFSET_FLAGS,
            [f32::from(u8::from(self.gradient_shading)), 0.0, 0.0, 0.0],
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Decode the packed block back to lanes, for assertions about offsets.
    fn lanes(bytes: &[u8; VOLUME_UNIFORM_BYTES]) -> [f32; VOLUME_UNIFORM_LANES] {
        let mut out = [0.0; VOLUME_UNIFORM_LANES];
        for (lane, slot) in out.iter_mut().enumerate() {
            let start = lane * 4;
            *slot = f32::from_le_bytes(
                <[u8; 4]>::try_from(&bytes[start..start + 4]).expect("four bytes per lane"),
            );
        }
        out
    }

    /// A uniform whose every lane is a distinct, recognisable number.
    ///
    /// Distinctness is the point: a `to_bytes` that swapped two `vec4`s, or
    /// transposed the matrix, or wrote the light direction into the transfer
    /// slot would still round-trip through a decoder that mirrored it. Only
    /// absolute positions with unique values catch that.
    fn distinct() -> VolumeUniform {
        let mut matrix = [[0.0f32; 4]; 4];
        for (column, values) in matrix.iter_mut().enumerate() {
            for (row, slot) in values.iter_mut().enumerate() {
                // Column-major, so the lane index is column * 4 + row, and the
                // value says which is which: 10 * column + row.
                *slot = (column * 10 + row) as f32;
            }
        }
        VolumeUniform {
            box_from_clip: matrix,
            eye_in_box: [101.0, 102.0, 103.0],
            box_size_km: [201.0, 202.0, 203.0],
            grid_dims: [301, 302, 303],
            light_dir: [401.0, 402.0, 403.0],
            ambient: 404.0,
            extinction_per_km: 501.0,
            empty_index_threshold: 502.0,
            early_out_transmittance: 503.0,
            gradient_shading: true,
        }
    }

    /// The block is exactly 160 bytes, and the shader declares the same.
    ///
    /// Both halves matter: the Rust side could be 160 while the WGSL grew a
    /// member, and then every lane after the new one is read from the wrong
    /// place with no error at all — a uniform buffer larger than the shader's
    /// block is legal.
    #[test]
    fn the_block_is_a_mat4_and_six_vec4s_on_both_sides() {
        assert_eq!(VOLUME_UNIFORM_BYTES, 64 + 6 * 16);
        assert_eq!(OFFSET_FLAGS + 16, VOLUME_UNIFORM_BYTES);

        let source = include_str!("volume.wgsl");
        let declaration = source
            .split_once("struct Volume {")
            .and_then(|(_, rest)| rest.split_once('}'))
            .map(|(body, _)| body)
            .expect("volume.wgsl no longer declares `struct Volume`");

        let mat4s = declaration.matches("mat4x4<f32>").count();
        let vec4s = declaration.matches("vec4<f32>").count();
        assert_eq!(
            (mat4s, vec4s),
            (1, 6),
            "volume.wgsl's uniform block is {mat4s} mat4x4 and {vec4s} vec4, \
             which is {} bytes, not the {VOLUME_UNIFORM_BYTES} this file packs. \
             A block smaller than the buffer is legal, so nothing would report \
             the mismatch — every member past the change would simply read the \
             wrong lane.",
            mat4s * 64 + vec4s * 16
        );
    }

    /// The declaration order in the WGSL is the order this file packs.
    ///
    /// Reordering two `vec4<f32>` members in the shader is a one-line edit that
    /// leaves the block the same size and every test above green, while the
    /// camera reads the box size and the box size reads the camera.
    #[test]
    fn the_shader_declares_the_members_in_the_order_this_file_packs_them() {
        let source = include_str!("volume.wgsl");
        let declaration = source
            .split_once("struct Volume {")
            .and_then(|(_, rest)| rest.split_once('}'))
            .map(|(body, _)| body)
            .expect("volume.wgsl no longer declares `struct Volume`");

        let mut at = 0usize;
        for member in [
            "box_from_clip",
            "eye_in_box",
            "box_size_km",
            "grid_dims",
            "light_dir_ambient",
            "transfer",
            "flags",
        ] {
            let needle = format!("{member}:");
            let found = declaration[at..].find(&needle).unwrap_or_else(|| {
                panic!(
                    "volume.wgsl's uniform block does not declare `{member}` \
                     after the members before it; the shader's order no longer \
                     matches the byte offsets this file writes"
                )
            });
            at += found + needle.len();
        }
    }

    /// Every lane lands at its documented std140 offset.
    #[test]
    fn every_lane_lands_at_its_std140_offset() {
        let packed = lanes(&distinct().to_bytes());

        // Column-major: column c occupies lanes 4c..4c+4.
        assert_eq!(
            &packed[0..16],
            &[
                0.0, 1.0, 2.0, 3.0, // column 0
                10.0, 11.0, 12.0, 13.0, // column 1
                20.0, 21.0, 22.0, 23.0, // column 2
                30.0, 31.0, 32.0, 33.0, // column 3
            ],
            "box_from_clip is not written column-major; WGSL's mat4x4 and \
             std140 both are, so a transpose here rotates the camera's axes"
        );

        for (offset, expected, member) in [
            (OFFSET_EYE_IN_BOX, [101.0, 102.0, 103.0, 0.0], "eye_in_box"),
            (
                OFFSET_BOX_SIZE_KM,
                [201.0, 202.0, 203.0, 0.0],
                "box_size_km",
            ),
            (OFFSET_GRID_DIMS, [301.0, 302.0, 303.0, 0.0], "grid_dims"),
            (
                OFFSET_LIGHT_DIR_AMBIENT,
                [401.0, 402.0, 403.0, 404.0],
                "light_dir_ambient",
            ),
            (OFFSET_TRANSFER, [501.0, 502.0, 503.0, 0.0], "transfer"),
            (OFFSET_FLAGS, [1.0, 0.0, 0.0, 0.0], "flags"),
        ] {
            let lane = offset / 4;
            assert_eq!(
                &packed[lane..lane + 4],
                &expected,
                "`{member}` is not at byte {offset}"
            );
        }
    }

    /// Reserved lanes are written as zero, not left as whatever was there.
    ///
    /// The buffer is reused every frame, so an unwritten lane holds the
    /// previous frame's value — which is invisible until someone gives the lane
    /// a meaning and reads it before the writer exists.
    #[test]
    fn reserved_lanes_are_written_as_zero() {
        // Start from a block that is entirely non-zero, so "still zero" cannot
        // be an accident of the array's initialisation.
        let bytes = distinct().to_bytes();
        let packed = lanes(&bytes);

        for (offset, lane_in_member, member) in [
            (OFFSET_EYE_IN_BOX, 3, "eye_in_box.w"),
            (OFFSET_BOX_SIZE_KM, 3, "box_size_km.w"),
            (OFFSET_GRID_DIMS, 3, "grid_dims.w"),
            (OFFSET_TRANSFER, 3, "transfer.w"),
            (OFFSET_FLAGS, 1, "flags.y"),
            (OFFSET_FLAGS, 2, "flags.z"),
            (OFFSET_FLAGS, 3, "flags.w"),
        ] {
            let lane = offset / 4 + lane_in_member;
            assert_eq!(packed[lane], 0.0, "`{member}` is not written as zero");
        }
    }

    /// The shading flag is 1.0 or 0.0, and the shader's threshold sits between.
    #[test]
    fn the_shading_flag_is_one_or_zero() {
        let mut uniform = distinct();

        uniform.gradient_shading = true;
        assert_eq!(lanes(&uniform.to_bytes())[OFFSET_FLAGS / 4], 1.0);

        uniform.gradient_shading = false;
        assert_eq!(lanes(&uniform.to_bytes())[OFFSET_FLAGS / 4], 0.0);

        assert!(
            include_str!("volume.wgsl").contains("volume.flags.x > 0.5"),
            "the shader no longer tests the shading flag against 0.5, so the \
             1.0/0.0 this file writes may no longer select what it selects"
        );
    }

    /// Grid dimensions cross as floats, not as integers reinterpreted.
    ///
    /// `grid_dims` is the one member whose Rust type is an integer, and the
    /// mistake with teeth is writing `n.to_le_bytes()` for a `u32`: 256 then
    /// arrives as 3.6e-43 and the gradient's voxel step becomes astronomically
    /// large, which reads as a completely unshaded volume rather than as an
    /// error.
    #[test]
    fn the_grid_dimensions_cross_as_floats() {
        let uniform = VolumeUniform::new([240.0, 240.0, 20.0], [256, 256, 128]);
        let packed = lanes(&uniform.to_bytes());
        let lane = OFFSET_GRID_DIMS / 4;
        assert_eq!(&packed[lane..lane + 3], &[256.0, 256.0, 128.0]);
    }

    /// `new` produces a uniform whose defaults the shader can actually march.
    ///
    /// Each of these is a value that makes the raymarch degenerate rather than
    /// merely ugly: a zero axis divides by zero in the gradient's voxel step, a
    /// non-positive extinction makes every cell perfectly transparent, and an
    /// early-out at or above 1 stops the march on its first sample.
    #[test]
    fn the_defaults_are_a_marchable_configuration() {
        let uniform = VolumeUniform::new([240.0, 240.0, 20.0], [128, 128, 64]);
        assert!(uniform.grid_dims.iter().all(|&n| n > 0));
        assert!(uniform.box_size_km.iter().all(|&km| km > 0.0));
        assert!(uniform.extinction_per_km > 0.0);
        assert!((0.0..1.0).contains(&uniform.early_out_transmittance));
        assert!((0.0..=1.0).contains(&uniform.ambient));
        assert!(uniform.light_dir.iter().any(|&c| c != 0.0));
        assert_eq!(uniform.box_from_clip, IDENTITY);
    }

    /// The default light really does come from above and from the left.
    ///
    /// Added after both minus signs in `DEFAULT_LIGHT_DIR` survived a mutation
    /// pass: `the_defaults_are_a_marchable_configuration` only asks that the
    /// vector is not all zeroes, which a light shining up from underneath the
    /// storm satisfies. That is not a crash and not a NaN — it is a volume
    /// whose overshooting tops read as dents, which is the failure this
    /// convention exists to avoid.
    ///
    /// Box space is z-up, and x/y run east and north, so "up and over the
    /// viewer's left shoulder" is `z > 0` with `x < 0` and `y < 0`.
    #[test]
    fn the_default_light_comes_from_above_and_over_the_left_shoulder() {
        let [x, y, z] = DEFAULT_LIGHT_DIR;
        assert!(
            z > 0.0,
            "the default light shines from below (z = {z}), so an overshooting \
             top would be shaded like a dent"
        );
        assert!(
            x < 0.0 && y < 0.0,
            "the default light no longer comes over the viewer's left shoulder \
             (x = {x}, y = {y})"
        );
        // Not normalised — the shader does that — but it must not be so short
        // that it is indistinguishable from the zero vector after normalising.
        let magnitude = (x * x + y * y + z * z).sqrt();
        assert!(
            magnitude > 0.5,
            "the default light vector is {magnitude} long"
        );
    }

    /// The empty-cell threshold selects index 0 and nothing else.
    ///
    /// The shader skips a cell when `index > threshold` is false, and an
    /// `R8Unorm` fetch of palette entry `n` returns `n / 255`. So the threshold
    /// has to sit strictly between 0 and 1/255 — and it has to be *stated* as
    /// that rather than as a small number, because WP-C's whole no-data
    /// decision is that index 0 is the bottom of the ramp.
    #[test]
    fn the_empty_threshold_selects_exactly_palette_index_zero() {
        let threshold = DEFAULT_EMPTY_INDEX_THRESHOLD;
        assert!(
            0.0 < threshold && threshold < 1.0 / 255.0,
            "an empty-cell threshold of {threshold} does not separate palette \
             index 0 from index 1"
        );
    }

    /// The shader's palette size is the one the LUT budget pays for.
    ///
    /// `VOLUME_LUT_BYTES` sizes the upload; `LUT_ENTRIES` in the shader turns a
    /// fetched index into a texture coordinate. If they disagree the volume is
    /// painted with a table shifted by a fraction of a texel — every colour
    /// slightly wrong, nothing obviously broken.
    #[test]
    fn the_shader_and_the_lut_constant_agree() {
        let expected = format!("const LUT_ENTRIES: f32 = {LUT_ENTRIES}.0;");
        assert!(
            include_str!("volume.wgsl").contains(&expected),
            "volume.wgsl does not declare `{expected}`, so its palette \
             coordinate no longer matches the {VOLUME_LUT_BYTES}-byte table \
             `constants::VOLUME_LUT_BYTES` sizes"
        );
    }
}
