//! The raymarch's uniform block, packed by hand.

use squallar_device_profile::constants::VOLUME_LUT_BYTES;

/// Bytes in the uniform block. Two `mat4x4<f32>` + twelve `vec4<f32>`.
///
/// **Append-only.** The ground pass shares this one block with the raymarch
/// rather than carrying its own, which is what makes it structurally
/// impossible for the mesh and the march to disagree about the camera. Growing
/// it at the end is what keeps every `OFFSET_*` below where it already was —
/// B1 grew it 224 to 304 for `clip_from_box` and the occluder, B3 grew it 304
/// to 320 for [`OFFSET_GROUND_BOX`], and neither moved anything before it.
/// `REQUIRED_UNIFORM_BINDING_SIZE` is 512 and did not have to move either.
pub const VOLUME_UNIFORM_BYTES: usize = 320;

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
pub const OFFSET_CLIP_FROM_BOX: usize = 224;
pub const OFFSET_OCCLUDER: usize = 288;
pub const OFFSET_GROUND_BOX: usize = 304;

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
    /// Box space to clip space, **column-major**, the direction the ground
    /// mesh is drawn through. The forward twin of
    /// [`VolumeUniform::box_from_clip`], built rather than inverted; sharing
    /// one uniform block with the march is what makes the two impossible to
    /// disagree.
    pub clip_from_box: [[f32; 4]; 4],
    /// The ray parameter a fully-saturated occluder texel decodes to, in box
    /// units, and **zero when no ground pass ran** — which is what the march
    /// tests to decide whether to read the occluder at all.
    ///
    /// An **over-estimate**, deliberately: a `t` clamped to 1 by the packing
    /// decodes to this value, which is past the far side of the box, so the
    /// `min` against the box exit is a no-op rather than a clip. Rides
    /// `occluder.x`.
    pub occluder_t_scale: f32,
    /// The ground surface's greatest height, in box `z`. Zero when no ground
    /// pass ran, by the same sentinel discipline as
    /// [`Self::occluder_t_scale`] — but **not** an iff, because a mesh that is
    /// flat at sea level across the whole box is zero with a pass running, and
    /// that ambiguity is half of why the composite does not read it.
    ///
    /// **Reserved by B1 for the composite's arm, and B2 measured that it is the
    /// wrong number for it, so nothing reads it yet.** Two reasons, both in
    /// `volume.wgsl`'s arm comment: the march is CLIPPED against the ground, so
    /// an eye under the crest still has every accumulated sample in front of
    /// the surface and "above the ceiling" discards 10817 of 10817 pixels of
    /// volume standing in front of a ridge; and the ceiling is a knife edge
    /// where a pass sentinel is not, since a mesh that happens to be flat would
    /// flip the frame's composite on the terrain's content. The arm reads
    /// [`Self::occluder_t_scale`] as its sentinel instead. Rides `occluder.y`.
    pub ground_max_z: f32,
    /// One raw `R16Uint` height sample turned into box `z`:
    /// `z = raw * height_scale + height_offset`. Rides `occluder.z`.
    ///
    /// Composed by [`Self::height_affine`] out of the field's own quantum and
    /// base and the drawn box's z range, in `f64`, so metres are divided by
    /// kilometres once and on the host. The shader clamps the result into the
    /// unit cube, which is what keeps [`Self::t_scale_for`]'s corner bound
    /// sound against a field whose peaks reach above the box.
    pub height_scale: f32,
    /// See [`Self::height_scale`]. Rides `occluder.w`.
    pub height_offset: f32,
    /// Where the height field's own footprint sits in the drawn box's unit
    /// square: `(scale_x, scale_y, offset_x, offset_y)`, applied as
    /// `p.xy = scale * uv + offset`.
    ///
    /// [`IDENTITY_GROUND_BOX`] is the settled case — a field resampled for the
    /// box being drawn — and it is a multiply by one and an add of zero. It is
    /// not the identity while a field built for an older box stands in, which
    /// is what keeps a pane drawn rather than blank while a newer field is in
    /// flight: the mesh is laid over the box the field actually covers, where
    /// its heights are true, instead of being stretched over a box it was
    /// never resampled for. Rides `ground_box`.
    pub ground_box: [f32; 4],
}

/// The lit-volume sentinel for [`VolumeUniform::iso_threshold`] and the
/// sequential sentinel for [`VolumeUniform::iso_centre`].
pub const ISO_OFF: f32 = -1.0;

/// `(scale, offset)` for a box that **is** the grid — the ordinary case. See
/// [`VolumeUniform::grid_from_box_scale`].
pub const IDENTITY_GRID_FROM_BOX: ([f32; 3], [f32; 3]) = ([1.0; 3], [0.0; 3]);

/// A height field whose footprint **is** the drawn box — the settled case.
/// See [`VolumeUniform::ground_box`].
pub const IDENTITY_GROUND_BOX: [f32; 4] = [1.0, 1.0, 0.0, 0.0];

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
            clip_from_box: IDENTITY,
            // No ground pass: zero is the sentinel the march tests, so the
            // occluder arm is dead and the group-2 placeholder is never read
            // for anything but its own zeroed alpha.
            occluder_t_scale: 0.0,
            ground_max_z: 0.0,
            height_scale: 0.0,
            height_offset: 0.0,
            ground_box: IDENTITY_GROUND_BOX,
        }
    }

    /// The `(scale, offset)` that turns one raw `R16Uint` height sample into
    /// box `z`, for a field encoded at `base_m`/`quantum_m` standing in a box
    /// whose z runs `z_km_msl.0 ..= z_km_msl.1`.
    ///
    /// **One derivation, in `f64`.** The field's encoding is metres and the
    /// box's is kilometres MSL; doing that division per post, or in `f32` on
    /// the GPU, is how a terrain ends up a few metres out everywhere for no
    /// visible reason. `None` for a box with no vertical extent, which
    /// `build_voxels` does not produce and which would reach the GPU as an
    /// infinity.
    pub fn height_affine(base_m: f64, quantum_m: f64, z_km_msl: (f64, f64)) -> Option<(f32, f32)> {
        let span_km = z_km_msl.1 - z_km_msl.0;
        if !span_km.is_finite() || span_km <= 0.0 {
            return None;
        }
        let scale = quantum_m / 1000.0 / span_km;
        let offset = (base_m / 1000.0 - z_km_msl.0) / span_km;
        (scale.is_finite() && offset.is_finite()).then_some((scale as f32, offset as f32))
    }

    /// The `t` scale to pack an occluder against for this camera: **1.05 times
    /// the farthest unit-cube corner from the eye**.
    ///
    /// The unit cube is convex and `|p − eye|` is a convex function of `p`, so
    /// its maximum over the cube is attained at a corner — every vertex of a
    /// ground mesh authored in box space is therefore inside this bound before
    /// the 5% is added. `pinned by the_t_scale_covers_every_post_of_the_grid`.
    pub fn t_scale_for(eye_in_box: [f32; 3]) -> f32 {
        let mut farthest = 0.0f32;
        for corner in 0..8u32 {
            let p = [
                (corner & 1) as f32,
                ((corner >> 1) & 1) as f32,
                ((corner >> 2) & 1) as f32,
            ];
            let d = ((p[0] - eye_in_box[0]).powi(2)
                + (p[1] - eye_in_box[1]).powi(2)
                + (p[2] - eye_in_box[2]).powi(2))
            .sqrt();
            farthest = farthest.max(d);
        }
        T_SCALE_MARGIN * farthest
    }

    /// Turn the occluder on **for this uniform's own eye**, and put the lid
    /// out.
    ///
    /// The one blessed way to set the scale. `occluder_t_scale` is derived from
    /// `eye_in_box`, and the two are independent public fields, so a scale
    /// computed against a different eye would silently mis-clip every ray in
    /// the frame — the picture would look plausible and be wrong.
    /// `VolumeTextures::write_uniform` re-checks the pair under
    /// `debug_assertions`, at the one seam a uniform reaches the GPU through.
    ///
    /// **It clears `map_floor`, and that is not a convenience.** The mesh *is*
    /// the ground, so a frame that draws one has no flat lid at z = 0 to draw
    /// as well; holding both painted the lid behind the march at full opacity
    /// wherever the ray crossed z = 0 without meeting the mesh, which B1
    /// measured at 76, 74 and 33 pixels at the three below-floor cameras. The
    /// shader refuses the pair on its own — see `map_t`'s guard in
    /// `volume.wgsl` — so this is the pair being *honest* rather than the pair
    /// being *harmless*, and each is worth having without the other.
    pub fn aim_occluder(&mut self, ground_max_z: f32, height_scale: f32, height_offset: f32) {
        self.occluder_t_scale = Self::t_scale_for(self.eye_in_box);
        self.ground_max_z = ground_max_z;
        self.height_scale = height_scale;
        self.height_offset = height_offset;
        self.map_floor = false;
    }

    /// Whether this uniform asks for the flat map lid and a ground mesh at
    /// once — the pair the shader's own `map_t` guard refuses.
    ///
    /// Read by `VolumeTextures::write_uniform`, at the one seam a uniform
    /// reaches the GPU through and beside the same check on
    /// [`Self::occluder_is_aimed_at_its_own_eye`] - but as a `log::error!`
    /// rather than a `debug_assert!`, because a wiring mistake reaching a user
    /// reaches them on a release web build where a debug assertion says
    /// nothing. The shader is what makes the picture right; this is what tells
    /// a caller that built the pair.
    pub fn ground_is_one_surface(&self) -> bool {
        !(self.map_floor && self.occluder_t_scale > 0.0)
    }

    /// Whether the occluder's scale is the one **this** uniform's eye implies.
    ///
    /// True when the occluder is off, which is the zero sentinel. Checked at
    /// the seam a uniform reaches the GPU through — `VolumeTextures::
    /// write_uniform` — rather than in [`Self::to_bytes`], which is a pure
    /// serialiser that byte-layout fixtures drive with deliberately
    /// incoherent lanes.
    pub fn occluder_is_aimed_at_its_own_eye(&self) -> bool {
        self.occluder_t_scale == 0.0
            || (self.occluder_t_scale - Self::t_scale_for(self.eye_in_box)).abs()
                <= 1e-3 * self.occluder_t_scale
    }

    /// The 320 bytes the GPU reads, little-endian.
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
        for (column, values) in self.clip_from_box.iter().enumerate() {
            write_vec4(&mut out, OFFSET_CLIP_FROM_BOX + column * 16, *values);
        }
        write_vec4(
            &mut out,
            OFFSET_OCCLUDER,
            [
                self.occluder_t_scale,
                self.ground_max_z,
                self.height_scale,
                self.height_offset,
            ],
        );
        write_vec4(&mut out, OFFSET_GROUND_BOX, self.ground_box);

        out
    }
}

/// How far past the farthest corner [`VolumeUniform::t_scale_for`] reaches.
/// A saturating `t` must decode to something the box exit's `min` ignores, so
/// the scale has to be an over-estimate rather than a tight one.
pub const T_SCALE_MARGIN: f32 = 1.05;

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
