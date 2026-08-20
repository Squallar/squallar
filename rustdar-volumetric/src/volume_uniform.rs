//! The raymarch's uniform block, packed by hand.

use rustdar_device_profile::constants::VOLUME_LUT_BYTES;

/// Bytes in the uniform block. One `mat4x4<f32>` + ten `vec4<f32>`.
pub const VOLUME_UNIFORM_BYTES: usize = 224;

/// `f32` lanes in the uniform block.
pub const VOLUME_UNIFORM_LANES: usize = VOLUME_UNIFORM_BYTES / 4;

/// Byte offset of each member, in declaration order. Public because the
/// pipeline's minimum-binding-size assertion and the tests both name them.
pub const OFFSET_BOX_FROM_CLIP: usize = 0;
pub const OFFSET_EYE_IN_BOX: usize = 64;
pub const OFFSET_BOX_SIZE_KM: usize = 80;
pub const OFFSET_GRID_DIMS: usize = 96;
pub const OFFSET_LIGHT_DIR_AMBIENT: usize = 112;
pub const OFFSET_TRANSFER: usize = 128;
pub const OFFSET_FLAGS: usize = 144;
pub const OFFSET_FLOOR_UV: usize = 160;
pub const OFFSET_FLOOR_GEO: usize = 176;
pub const OFFSET_GRID_FROM_BOX_A: usize = 192;
pub const OFFSET_GRID_FROM_BOX_B: usize = 208;

/// Extinction per kilometre at a palette entry whose alpha is 1.
pub const DEFAULT_EXTINCTION_PER_KM: f32 = 1.0;

/// Palette indices at or below which a cell is skipped entirely.
pub const DEFAULT_EMPTY_INDEX_THRESHOLD: f32 = 0.5 / 255.0;

/// Transmittance below which the march stops.
pub const DEFAULT_EARLY_OUT_TRANSMITTANCE: f32 = 0.004;

/// Width of the opacity ramp above [`VolumeUniform::empty_index_threshold`],
/// in the shader's 0-1 index units. **Zero here, deliberately.**
pub const DEFAULT_EDGE_SOFT_WIDTH: f32 = 0.0;

/// Fraction of a lit surface's colour that survives facing away from the light.
pub const DEFAULT_AMBIENT: f32 = 0.35;

/// The camera-relative light direction the volume is lit from, in box space.
pub const DEFAULT_LIGHT_DIR: [f32; 3] = [-0.4, -0.5, 0.77];

/// Everything the raymarch reads that is not a texture.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VolumeUniform {
    /// Clip space to box space, **column-major**: `box_from_clip[c][r]`.
    pub box_from_clip: [[f32; 4]; 4],
    /// The perspective eye, in box space.
    pub eye_in_box: [f32; 3],
    /// The box's physical extent in kilometres.
    pub box_size_km: [f32; 3],
    /// The camera's vertical exaggeration, `>= 1`. Rides `box_size_km.w`.
    ///
    /// Read only by the gradient shading, which takes its normals against the
    /// *displayed* geometry; optical depth stays against the true
    /// `box_size_km`. Never zero — the shader divides a cell extent by it.
    pub vertical_exaggeration: f32,
    /// Voxels along each axis, read as *cells per unit of box space* rather
    /// than as the texture's dimensions. Under a crop the drawn box spans
    /// `grid_dims * grid_from_box_scale` cells; this lane is **not corrected**
    /// for it, because correcting it measured worse across a swap.
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
    pub step_cells: f32,
    /// Whether the march draws the map floor at the box's bottom face. Rides
    /// `flags.w`.
    pub map_floor: bool,
    /// The mip level the march reconstructs the field at, `0..=1`. Rides
    /// `flags.y`. Never negative, and always 0 for the isosurface march.
    pub reconstruction_lod: f32,
    /// The isosurface threshold in the shader's 0-1 index units, or negative
    /// for the lit-volume march. Rides `eye_in_box.w`, one of the two lanes
    /// that were reserved-zero before the view-mode work.
    pub iso_threshold: f32,
    /// The centre index a **diverging** product's isosurface measures its
    /// threshold from, in 0-1 index units, or negative for a sequential
    /// product whose threshold reads the index directly. Rides `grid_dims.w`,
    /// the other formerly-reserved lane.
    pub iso_centre: f32,
    /// Where the box's own site sits in the pane mirror, and how fast the
    /// mirror's texture coordinates run with geography: `(u_at_site,
    /// v_at_site, u_per_degree_east, v_per_mercator_y)`. Rides `floor_uv`.
    ///
    /// ```text
    /// δ      = hypot(x_km, y_km) / EARTH_RADIUS_KM
    /// sin φ  = sin φ₀·cos δ + cos φ₀·sin δ·cos az
    /// λ − λ₀ = atan2(sin az·sin δ·cos φ₀,  cos δ − sin φ₀·sin φ)
    /// (u, v) = (u₀ + (λ − λ₀)·u_per_deg,  v₀ + (mercᵧ(φ) − mercᵧ(φ₀))·v_per_merc)
    /// ```
    pub floor_uv: [f32; 4],
    /// The geography the mirror is sampled with: `(site_latitude_degrees,
    /// box_west_edge_km, box_south_edge_km, gamma_encoded)`. Rides
    /// `floor_geo`.
    pub floor_geo: [f32; 4],
    /// Where in the **grid texture** a position in the drawn box's unit cube
    /// sits: `t = grid_from_box_scale · p + grid_from_box_offset`, per axis.
    pub grid_from_box_scale: [f32; 3],
    /// Whether the drawn box reaches outside the grid, so the march has to
    /// test each fetch against the grid's bounds. Rides `grid_from_box_a.w`.
    pub grid_bounded: bool,
    /// See [`VolumeUniform::grid_from_box_scale`]. Rides `grid_from_box_b.xyz`;
    /// `w` is reserved and written zero.
    pub grid_from_box_offset: [f32; 3],
}

/// The lit-volume sentinel for [`VolumeUniform::iso_threshold`] and the
/// sequential sentinel for [`VolumeUniform::iso_centre`].
pub const ISO_OFF: f32 = -1.0;

/// `(scale, offset)` for a box that **is** the grid — the ordinary case. See
/// [`VolumeUniform::grid_from_box_scale`].
pub const IDENTITY_GRID_FROM_BOX: ([f32; 3], [f32; 3]) = ([1.0; 3], [0.0; 3]);

impl VolumeUniform {
    /// A uniform with the defaults above, an identity transform and no camera.
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
            // No mirror: `map_floor` is false above, so the shader never reads
            // these. Zero rather than a plausible-looking placement.
            floor_uv: [0.0; 4],
            floor_geo: [0.0; 4],
            grid_from_box_scale: IDENTITY_GRID_FROM_BOX.0,
            grid_bounded: false,
            grid_from_box_offset: IDENTITY_GRID_FROM_BOX.1,
        }
    }

    /// The 224 bytes the GPU reads, little-endian.
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
        write_vec4(&mut out, OFFSET_FLOOR_UV, self.floor_uv);
        write_vec4(&mut out, OFFSET_FLOOR_GEO, self.floor_geo);
        write_vec4(
            &mut out,
            OFFSET_GRID_FROM_BOX_A,
            xyz_w(
                self.grid_from_box_scale,
                f32::from(u8::from(self.grid_bounded)),
            ),
        );
        write_vec4(
            &mut out,
            OFFSET_GRID_FROM_BOX_B,
            xyz_w(self.grid_from_box_offset, 0.0),
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
