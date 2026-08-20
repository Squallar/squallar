// The offscreen volume raymarch, and the quad that composites it into egui.
//
// Every rule below is a naga/WGSL validator constraint, not a preference:
//   * `textureSampleLevel` for everything sampled: the march breaks on a
//     data-dependent condition, and implicit-LOD sampling under non-uniform
//     control flow is a hard validator failure. `textureLoad` takes an explicit
//     level and so is not implicit-LOD.
//   * `RAYMARCH_STEP_CEILING` stays `const` so the loop bound folds to a literal.
//   * One sampler per texture per pipeline.
//   * Group/binding pairs are unique across the whole module, whether or not one
//     entry point sees both: raymarch owns 0..4 and 7, blit owns 5..6.
//   * `textureNumLevels` is GLSL core 130 with no ES version, so it is
//     unreachable on WebGL2 forever and appears nowhere.

// One `mat4x4<f32>` plus ten `vec4<f32>`: 64 + 160 = 224 bytes, std140-clean.
// Every member is `f32`, including the conceptually-integer (`grid_dims`) and
// conceptually-bool (`flags`) ones: mixing integer and float members in a std140
// block is where driver bugs live. `volume_uniform.rs` writes these 224 bytes by
// hand and pins every offset; `REQUIRED_UNIFORM_BINDING_SIZE` is 256.
struct Volume {
    // Clip space to box space, box space being the unit cube [0,1]^3 over the
    // voxel grid. Built as box_from_world * world_from_view * view_from_clip,
    // never by inverting a general 4x4.
    box_from_clip: mat4x4<f32>,
    // xyz: the *perspective* camera position in box space, rays cast from it.
    // w: isosurface threshold in 0-1 index units, negative for the lit-volume
    // march — negative, not zero, since an index-0 threshold is a real setting.
    eye_in_box: vec4<f32>,
    // xyz: box extent in km. w: vertical exaggeration, >= 1, read only by the
    // shading (normals are against the displayed geometry); optical depth uses
    // xyz alone.
    box_size_km: vec4<f32>,
    // xyz: voxel counts per axis, as floats. w: the centre index a diverging
    // product's isosurface measures its threshold from, in 0-1 index units, or
    // negative for a sequential product. Isosurface mode only.
    grid_dims: vec4<f32>,
    // xyz: unit light direction in box space. w: the ambient term, 0..1.
    light_dir_ambient: vec4<f32>,
    // x: extinction per kilometre at LUT alpha 1.
    // y: index at or below which a cell contributes nothing — the PALETTE's own
    //    transparent run; air is excluded by coverage instead.
    // z: transmittance at which the march stops early.
    // w: the opacity ramp's width above y, in 0-1 index units; 0 is hard.
    transfer: vec4<f32>,
    // x: 1 to shade with the gradient, 0 to skip it.
    // y: reconstruction level, in mip units: 0 the raw trilinear field, towards 1
    //    blending into the hand-built two-cell mean. Never negative.
    // z: cells one step advances along the ray, in the grid's own anisotropic
    //    cell metric. Zero falls to the dt floor rather than hanging.
    // w: 1 to draw the map floor on the box's bottom face.
    flags: vec4<f32>,
    // x, y: (u, v) of the site itself in the pane mirror. z: u per degree of
    // longitude east. w: v per unit of Mercator y.
    floor_uv: vec4<f32>,
    // x: site latitude, degrees. y, z: the box's west and south edges, km
    // east/north of the site — its position, which `box_size_km` does not carry.
    // w: 1 when the mirror holds gamma-encoded texels, 0 when linear.
    floor_geo: vec4<f32>,
    // xyz: per-axis scale from the drawn box's unit cube into the GRID texture.
    // w: 1 when the drawn box reaches outside the grid, so every fetch has to be
    //    bounds-tested and answered as air outside; 0 when it is inside.
    grid_from_box_a: vec4<f32>,
    // xyz: per-axis offset. w: reserved, written zero. Together
    // `t = grid_from_box_a.xyz * p + grid_from_box_b.xyz`; scale 1 and offset 0 —
    // the ordinary case — make that `t = p` exactly.
    grid_from_box_b: vec4<f32>,
}

@group(0) @binding(0) var<uniform> volume: Volume;

fn grid_coord(p: vec3<f32>) -> vec3<f32> {
    return p * volume.grid_from_box_a.xyz + volume.grid_from_box_b.xyz;
}

// Outside the grid must read as air. The sampler's alternative — clamp to the
// edge texel — paints the grid's rim across ground the radar never reported.
fn outside_grid(t: vec3<f32>) -> bool {
    return volume.grid_from_box_a.w > 0.5
        && (any(t < vec3<f32>(0.0)) || any(t > vec3<f32>(1.0)));
}

// The voxel grid: `Rg16Float`, **coverage-premultiplied**, sampled `Linear`.
//   R = coverage x index    G = coverage    (1 measured, 0 empty air)
// The march reconstructs `index = R_bar / G_bar`; see `field_at`.
@group(0) @binding(1) var grid_texture: texture_3d<f32>;
@group(0) @binding(2) var grid_sampler: sampler;

// The 256-entry colour table, as a 256x1 2D texture sampled `Nearest`. A
// `texture_1d` would be the honest shape: GLES 3.0 has no `sampler1D` at all.
@group(0) @binding(3) var lut_texture: texture_2d<f32>;
@group(0) @binding(4) var lut_sampler: sampler;

// The march's stratification tile: `BLUE_NOISE_EDGE` square, `R8Unorm`, read with
// `textureLoad` and so carrying no sampler of its own.
@group(0) @binding(7) var jitter_texture: texture_2d<f32>;

// Must equal `blue_noise::BLUE_NOISE_EDGE - 1`;
// `the_shader_and_the_blue_noise_tile_agree` pins this literal to it.
const JITTER_TILE_MASK: i32 = 63;

// The pane mirror: the 2D pane's own egui geometry drawn a second time offscreen.
// A **Web Mercator** picture covering the whole frame, not the box footprint, so
// `floor_colour` reprojects into it. Premultiplied alpha, in whichever encoding
// `floor_geo.w` names. Its own group: group 0 is rebuilt per grid upload.
@group(1) @binding(0) var floor_texture: texture_2d<f32>;
@group(1) @binding(1) var floor_sampler: sampler;

// sRGB transfer functions, character-for-character egui's own
// (`egui-wgpu-0.35.0/src/egui.wgsl:44-57`). Matching egui is the requirement.

// 0-1 linear from 0-1 sRGB gamma
fn linear_from_gamma_rgb(srgb: vec3<f32>) -> vec3<f32> {
    let cutoff = srgb < vec3<f32>(0.04045);
    let lower = srgb / vec3<f32>(12.92);
    let higher = pow((srgb + vec3<f32>(0.055)) / vec3<f32>(1.055), vec3<f32>(2.4));
    return select(higher, lower, cutoff);
}

// 0-1 sRGB gamma from 0-1 linear
fn gamma_from_linear_rgb(rgb: vec3<f32>) -> vec3<f32> {
    let cutoff = rgb < vec3<f32>(0.0031308);
    let lower = rgb * vec3<f32>(12.92);
    let higher = vec3<f32>(1.055) * pow(rgb, vec3<f32>(1.0 / 2.4)) - vec3<f32>(0.055);
    return select(higher, lower, cutoff);
}

// A **ceiling** on samples per ray, not the step count: the step length arrives
// in `flags.z` and the loop breaks at the box exit. `const` so naga folds the
// bound to a literal. 1024 covers the desktop 256x256x128 grid's 384-cell
// diagonal at the half-cell rung (768 steps); longer spans are covered by the
// `dt` floor in `fs_raymarch` stretching the steps, not by truncation.
const RAYMARCH_STEP_CEILING: i32 = 1024;

// Must equal `constants::VOLUME_LUT_BYTES / 4`;
// `the_shader_and_the_lut_constant_agree` pins that.
const LUT_ENTRIES: f32 = 256.0;

// Smallest ray-direction component the slab test divides by: guards the
// axis-parallel ray without infinity arithmetic, which WGSL leaves
// implementation-defined.
const RAY_DIRECTION_EPSILON: f32 = 1e-6;

// How far under the bottom plane, in box heights, the eye travels before the
// floor is fully gone. Coverage is 1 at the plane and above, 0 at this depth
// (~1.4 km of the default 18 km column), so a below-plane eye is not walled off.
// Entirely below the plane: a band reaching above would thin the ground in every
// low-angle above-plane view.
const FLOOR_BELOW_FADE: f32 = 0.08;

// Below this the central difference is noise rather than a surface, and
// normalising it would point the normal in an arbitrary direction. Units:
// normalised palette index per DISPLAYED kilometre — the differenced field runs
// 0 (air) to 1 (top of table), and `shading`/`iso_shading` divide by `cell_km`,
// so the floor rescales with box, grid shape and exaggeration. A zero-detector,
// not a tuned threshold: one palette step across the coarsest shipped cell is
// 3.9e-3 / 1.797 km ~ 2.2e-3 per km, ~2200x this value.
const GRADIENT_EPSILON: f32 = 1e-6;

// Bisection steps refining an isosurface hit. `const` for the same naga reason as
// RAYMARCH_STEP_CEILING. Eight halvings place the surface to under 1/256 of a
// step — finer than the eight-bit index can express.
const ISO_REFINE_STEPS: i32 = 8;

// Reconstructed coverage at or above which a sample is INSIDE the data, for the
// one decision that has to be binary: the isosurface's hit test. 0.5 is the
// nearest-neighbour decision boundary — along an axis the trilinear coverage
// field's half level set is the midpoint between the last covered texel centre
// and the first uncovered one. Exact in 1-D only (at u = v = w = 0.49 from a lone
// texel, coverage is 0.51^3 = 0.133), so lone cells have their corners clipped.
// It is a claim about the RAW tent, and the isosurface marches at level 0
// (`volume::bridge` sends `reconstruction_lod = 0`). The lit volume does not use
// it — see COVERAGE_SKIP.
const COVERAGE_FLOOR: f32 = 0.5;

// Coverage below which the LIT VOLUME skips a sample: a fill-rate and precision
// floor, **not** a decision about where the data is. For an integrated quantity,
// weighting optical depth by coverage IS the partial-volume answer — the
// trilinear tent is a partition of unity, so it redistributes an edge voxel's
// opacity across the reconstruction footprint rather than adding any. (The
// conserved quantity is `coverage x extinction`, exact in the LUT alpha only
// where that alpha is constant across the indices the edge sweeps.) 1/255 is one
// step of the palette the index channel carries. A COVERAGE_FLOOR-style cut here
// would delete whole features above level 0, where a lone voxel is an eighth of
// its coarse texel and reads coverage 0.125.
const COVERAGE_SKIP: f32 = 1.0 / 255.0;

// Divisor floor for the coverage reconstruction, far under COVERAGE_SKIP: an
// all-air fetch (R = G = 0 exactly) yields index 0 rather than a NaN.
const COVERAGE_EPSILON: f32 = 1e-6;

struct RaymarchVertex {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) ndc: vec2<f32>,
}

@vertex
fn vs_raymarch(@location(0) clip_xy: vec2<f32>) -> RaymarchVertex {
    var out: RaymarchVertex;
    out.clip_position = vec4<f32>(clip_xy, 0.0, 1.0);
    out.ndc = clip_xy;
    return out;
}

fn unproject(ndc: vec2<f32>, depth: f32) -> vec3<f32> {
    let homogeneous = volume.box_from_clip * vec4<f32>(ndc, depth, 1.0);
    return homogeneous.xyz / homogeneous.w;
}

// The ray direction as every plane crossing here solves against it: each
// component's magnitude floored at RAY_DIRECTION_EPSILON, its sign kept. One
// function because `floor_hit` and `slab_entry_exit` must give the bottom face
// the same answer to the BIT, not two a ULP apart.
fn slab_direction(rd: vec3<f32>) -> vec3<f32> {
    let magnitude = max(abs(rd), vec3<f32>(RAY_DIRECTION_EPSILON));
    return select(magnitude, -magnitude, rd < vec3<f32>(0.0));
}

// Where the ray enters and leaves the unit cube, as (entry, exit) parameters.
// `exit <= entry` means it misses. Entry is clamped to zero so a camera inside
// the box marches from itself rather than from behind itself.
fn slab_entry_exit(ro: vec3<f32>, rd: vec3<f32>) -> vec2<f32> {
    let inverse = vec3<f32>(1.0) / slab_direction(rd);
    let to_min = (vec3<f32>(0.0) - ro) * inverse;
    let to_max = (vec3<f32>(1.0) - ro) * inverse;
    let near = min(to_min, to_max);
    let far = max(to_min, to_max);
    let entry = max(max(near.x, near.y), max(near.z, 0.0));
    let exit = min(far.x, min(far.y, far.z));
    return vec2<f32>(entry, exit);
}

// Kilometres one `dt` step covers along `rd`. The direction is INSIDE the length:
// `dt * length(box_size_km)` gives every direction the box's diagonal, which on a
// 240 x 240 x 20 km box makes a vertical step 17x too long and a horizontal 1.4x.
fn step_length_km(rd: vec3<f32>, dt: f32) -> f32 {
    return length(rd * dt * volume.box_size_km.xyz);
}

// The texel centre of palette entry `index`, in the 0-1 units `field_at` returns.
fn lut_coord(index: f32) -> vec2<f32> {
    return vec2<f32>((index * (LUT_ENTRIES - 1.0) + 0.5) / LUT_ENTRIES, 0.5);
}

// The field the march reads: `x` the reconstructed palette index, `y` the
// reconstructed coverage, both at level flags.y, from ONE fetch.
//
// The texture holds `R = coverage x index`, `G = coverage`, and `Linear`
// filtering returns tent-weighted means of the same texels under the same
// weights, so
//
//     R_bar / G_bar  =  sum(w_i c_i x_i) / sum(w_i c_i)
//
// the coverage-weighted mean of the index over the COVERED texels alone: air has
// c = 0 and contributes to neither sum, so the result always lies inside the
// convex hull of the surrounding stored indices, whatever the palette ramp. A
// legitimate index 0 counts AS A ZERO (0 into R, 1 into G), though
// `rustdar_radar::voxel::ramp_index` clamps finite measurements to 1..=255.
//
// The hull statement holds only as far as the filter's PRECISION does, which is
// why the format is float and not `Rg8Unorm`: a unorm filter's per-channel error
// is absolute, up to one quantum, and this division turns it into an index error
// of `2q / G_bar` — the whole palette one cell out from an echo edge. See
// `volume::VOLUME_TEXTURE_FORMAT`.
//
// The level: one hand-built mip below the grid, each level-1 texel the box mean
// of its eight level-0 texels in BOTH channels — under the ratio above, the
// occupancy-weighted mean of the index and the occupancy itself — so flags.y
// widens the kernel continuously: 0 the raw trilinear tent (level-1 weight
// exactly zero), 1 a two-cell box convolved with a tent.
fn field_at(p: vec3<f32>) -> vec2<f32> {
    let t = grid_coord(p);
    if outside_grid(t) {
        return vec2<f32>(0.0, 0.0);
    }
    let texel = textureSampleLevel(grid_texture, grid_sampler, t, volume.flags.y).rg;
    // No `select`: an all-air fetch is R = G = 0 exactly, so the floored divisor
    // already returns index 0.
    return vec2<f32>(texel.r / max(texel.g, COVERAGE_EPSILON), texel.g);
}

// The premultiplied channel alone — `coverage x index` — which is what the lit
// volume's gradient is taken of, **not** the reconstructed index: at an echo edge
// this falls continuously to zero while the reconstructed index stays a real mean
// of real neighbours, so this is the one with a gradient there, pointing out of
// the data.
fn shading_field(p: vec3<f32>) -> f32 {
    let t = grid_coord(p);
    if outside_grid(t) {
        return 0.0;
    }
    return textureSampleLevel(grid_texture, grid_sampler, t, volume.flags.y).r;
}

// Deterministic per-pixel jitter in [0, 1) offsetting the march's sample comb by
// that fraction of a step. Without it the comb is phase-locked to the eye and
// every iso-`t` shell draws a contour that stays put in screen space while the
// volume slides beneath it. Indexed by pixel coordinate alone — never add a time
// term, animated jitter is shimmer. A tile rather than a hash because IGN is a
// rank-1 lattice (81.6% of its energy in 0.1% of its bins, a 1.86 px grating at
// -35 degrees) that draws a diagonal weave no step count removes, while a white
// hash puts fifteen times as much energy in the low band. See
// `volume::blue_noise`.
fn blue_noise_jitter(px: vec2<f32>) -> f32 {
    // A mask rather than `%`: the edge is a power of two, and WGSL's remainder
    // takes the sign of its left operand, so the mask cannot index backwards.
    let at = vec2<i32>(px) & vec2<i32>(JITTER_TILE_MASK, JITTER_TILE_MASK);
    return textureLoad(jitter_texture, at, 0).r;
}

// Diffuse shading from the central-difference gradient, in 0..1. Six extra
// fetches against the march's one — 2.4x on an RTX 3090 at 1440x900 (0.774 ms
// against 0.325) — which is why it is a selectable rung.
//
//   * The gradient is taken in the DISPLAYED kilometre, not in box units: box
//     space is the unit cube over a pancake (25.6:1 at the widest default), so
//     raw box-space differences under-weight the vertical by the aspect ratio.
//   * Half-Lambert (Valve's wrap term, squared) instead of a clamped cosine: a
//     cloud has no terminator, and `max(dot, 0)` draws a hard day/night line
//     across every storm core exactly where the gradient is noisiest.
fn shading(p: vec3<f32>) -> f32 {
    let voxel = vec3<f32>(1.0) / volume.grid_dims.xyz;
    // One displayed cell per axis, in km; `box_size_km.w` is the exaggeration.
    let cell_km = vec3<f32>(
        volume.box_size_km.x,
        volume.box_size_km.y,
        volume.box_size_km.z * volume.box_size_km.w,
    ) * voxel;
    // Differences of the same reconstruction the march reads, at the same level,
    // of the *premultiplied* channel — see `shading_field`.
    let gradient = vec3<f32>(
        shading_field(p + vec3<f32>(voxel.x, 0.0, 0.0))
            - shading_field(p - vec3<f32>(voxel.x, 0.0, 0.0)),
        shading_field(p + vec3<f32>(0.0, voxel.y, 0.0))
            - shading_field(p - vec3<f32>(0.0, voxel.y, 0.0)),
        shading_field(p + vec3<f32>(0.0, 0.0, voxel.z))
            - shading_field(p - vec3<f32>(0.0, 0.0, voxel.z)),
    ) / cell_km;
    let ambient = volume.light_dir_ambient.w;
    let magnitude = length(gradient);
    if magnitude < GRADIENT_EPSILON {
        return 1.0;
    }
    // The gradient climbs towards denser cells, so the outward normal is negated.
    let normal = -gradient / magnitude;
    let wrap = 0.5 + 0.5 * dot(normal, normalize(volume.light_dir_ambient.xyz));
    return ambient + (1.0 - ambient) * wrap * wrap;
}

// The isosurface: the march finds the first crossing of a threshold, refines it
// by bisection and paints it as one opaque, gradient-lit surface. Selected by the
// sign of `eye_in_box.w` (negative = lit volume). The threshold reads the DATA —
// the interpolated palette index — never the LUT's alpha.

// The scalar field the isosurface is a level set of: the index itself for a
// sequential product, the distance from the diverging centre (`grid_dims.w`) for
// a diverging one, which renders BOTH lobes of a velocity couplet.
fn iso_field(index: f32) -> f32 {
    return select(index, abs(index - volume.grid_dims.w), volume.grid_dims.w >= 0.0);
}

// The coverage term excludes unmeasured air: without it a diverging centre reads
// the no-data index 0 as a strong inbound crossing, and for rhoHV — whose centre
// sits at the TOP of its ramp — index 0 is the most extreme hit possible.
fn iso_hit_test(sample: vec2<f32>) -> bool {
    return sample.y >= COVERAGE_FLOOR && iso_field(sample.x) >= volume.eye_in_box.w;
}

// The crossing parameter between a sample outside the surface at `t_lo` and one
// inside at `t_hi`, by ISO_REFINE_STEPS halvings.
fn refine_iso_hit(eye: vec3<f32>, direction: vec3<f32>, t_lo_in: f32, t_hi_in: f32) -> f32 {
    var lo = t_lo_in;
    var hi = t_hi_in;
    for (var i: i32 = 0; i < ISO_REFINE_STEPS; i = i + 1) {
        let mid = 0.5 * (lo + hi);
        if iso_hit_test(field_at(eye + direction * mid)) {
            hi = mid;
        } else {
            lo = mid;
        }
    }
    return hi;
}

// The isosurface's level-set function at `p`, coverage-premultiplied. `iso_field`
// of air is meaningless — for rhoHV it is the function's largest value — so an
// unweighted difference across the data boundary would point the normal into the
// air; coverage sends air to zero and is 1 inside the data.
fn iso_shading_field(p: vec3<f32>) -> f32 {
    let sample = field_at(p);
    return iso_field(sample.x) * sample.y;
}

// `shading` over the isosurface's own field: for a diverging product the level
// set's gradient is not the density's — on the inbound lobe density falls toward
// the core, so a density normal would light the surface from inside.
fn iso_shading(p: vec3<f32>) -> f32 {
    let voxel = vec3<f32>(1.0) / volume.grid_dims.xyz;
    let cell_km = vec3<f32>(
        volume.box_size_km.x,
        volume.box_size_km.y,
        volume.box_size_km.z * volume.box_size_km.w,
    ) * voxel;
    let gradient = vec3<f32>(
        iso_shading_field(p + vec3<f32>(voxel.x, 0.0, 0.0))
            - iso_shading_field(p - vec3<f32>(voxel.x, 0.0, 0.0)),
        iso_shading_field(p + vec3<f32>(0.0, voxel.y, 0.0))
            - iso_shading_field(p - vec3<f32>(0.0, voxel.y, 0.0)),
        iso_shading_field(p + vec3<f32>(0.0, 0.0, voxel.z))
            - iso_shading_field(p - vec3<f32>(0.0, 0.0, voxel.z)),
    ) / cell_km;
    let ambient = volume.light_dir_ambient.w;
    let magnitude = length(gradient);
    if magnitude < GRADIENT_EPSILON {
        return 1.0;
    }
    let normal = -gradient / magnitude;
    let wrap = 0.5 + 0.5 * dot(normal, normalize(volume.light_dir_ambient.xyz));
    return ambient + (1.0 - ambient) * wrap * wrap;
}

// Always gradient-lit: an unlit opaque surface is a silhouette.
fn iso_surface_colour(p: vec3<f32>) -> vec3<f32> {
    let index = field_at(p).x;
    let entry = textureSampleLevel(lut_texture, lut_sampler, lut_coord(index), 0.0);
    return linear_from_gamma_rgb(entry.rgb) * iso_shading(p);
}

// Where this ray meets the box's bottom face — the z = 0 plane clipped to the
// unit square — or negative for a ray that never does. For an eye above the plane
// a hit coincides with the box exit, below it with (or before) the entry. Solved
// through `slab_direction` so that coincidence is EXACT: `slab_entry_exit`
// multiplies by a reciprocal, and `a * (1.0 / b)` is not `a / b`.
fn floor_hit(eye: vec3<f32>, direction: vec3<f32>) -> f32 {
    if abs(direction.z) < RAY_DIRECTION_EPSILON {
        return -1.0;
    }
    let t = (0.0 - eye.z) * (1.0 / slab_direction(direction).z);
    if t <= 0.0 {
        return -1.0;
    }
    let hit = eye + direction * t;
    if hit.x < 0.0 || hit.x > 1.0 || hit.y < 0.0 || hit.y > 1.0 {
        return -1.0;
    }
    return t;
}

// `rustdar_radar::types::KM_PER_DEGREE_LAT` (`EARTH_RADIUS_KM * pi/180`). WGSL
// cannot read a Rust constant, so this is the only copy of it here;
// `volume_uniform::tests::the_shaders_km_per_degree_is_the_radar_crates_own` pins
// the literal.
const KM_PER_DEGREE_LAT: f32 = 111.194927;

// Web Mercator's y: `ln(tan(pi/4 + phi/2))`, the projection's definition.
fn mercator_y(lat_rad: f32) -> f32 {
    return log(tan(0.78539816 + lat_rad * 0.5));
}

// The same y from `sin phi`, by `ln(tan(pi/4 + phi/2)) == atanh(sin phi)`
// (`rustdar_radar::types::mercator_y_from_sin_lat`): the reprojection below
// produces latitude as a sine, and the angle would mean an `asin` undone by `tan`.
fn mercator_y_from_sin(sin_lat: f32) -> f32 {
    return 0.5 * log((1.0 + sin_lat) / (1.0 - sin_lat));
}

// The floor's colour where the ray lands: STRAIGHT (un-premultiplied) linear RGB
// with the mirror's own alpha, which is what the composite arms below expect.
//
// It reprojects rather than indexing the mirror directly: the mirror is Web
// Mercator and the box is a tangent plane in km east/north of the site, so a
// scale and translate is off by 7.6 km across and 3.7 km down at the corners of
// the shipped 460 km box (see `VolumeUniform::floor_uv`). The conversion is
// spherical because the box is polar — `build_voxels` makes it a site-centred
// azimuthal-equidistant tangent plane (`range = hypot(x, y)`,
// `azimuth = atan2(x, y)`) — so this is the direct spherical problem from the
// site, `rustdar_radar::beam::great_circle_destination`, which is where the
// raster's own gates are painted. An equirectangular approximation differs by
// ~15 km at the corners of the default box at 41.7 N.
fn floor_colour(eye: vec3<f32>, direction: vec3<f32>, t: f32) -> vec4<f32> {
    let hit = eye + direction * t;

    let x_km = volume.floor_geo.y + hit.x * volume.box_size_km.x;
    let y_km = volume.floor_geo.z + hit.y * volume.box_size_km.y;

    let site_lat_deg = volume.floor_geo.x;
    let site_lat_rad = radians(site_lat_deg);
    let sin_phi0 = sin(site_lat_rad);
    let cos_phi0 = cos(site_lat_rad);

    // The angle this box point subtends at the earth's centre, as
    // `radians(km / KM_PER_DEGREE_LAT)`, so the radius is never written twice.
    let range_km = length(vec2<f32>(x_km, y_km));
    let delta = radians(range_km / KM_PER_DEGREE_LAT);
    let sd = sin(delta);
    let cd = cos(delta);
    // The bearing's sine and cosine without a trig call: `(x, y)/range` is already
    // `(sin az, cos az)`. Zero at the site, where `sd` is zero and both terms
    // below vanish — the site maps to the site.
    let inv = select(0.0, 1.0 / range_km, range_km > 0.0);
    let sin_az = x_km * inv;
    let cos_az = y_km * inv;

    let sin_lat = clamp(sin_phi0 * cd + cos_phi0 * sd * cos_az, -1.0, 1.0);
    if abs(sin_lat) > 0.999999 {
        return vec4<f32>(0.0, 0.0, 0.0, 0.0);
    }
    let d_lon_deg = degrees(atan2(sin_az * sd * cos_phi0, cd - sin_phi0 * sin_lat));
    let d_merc = mercator_y_from_sin(sin_lat) - mercator_y(site_lat_rad);

    let uv = vec2<f32>(
        volume.floor_uv.x + d_lon_deg * volume.floor_uv.z,
        volume.floor_uv.y + d_merc * volume.floor_uv.w,
    );
    // Off the mirror is ground the source pane is not showing, which has no colour
    // to report; clamping would smear the pane's border across the box.
    if uv.x < 0.0 || uv.x > 1.0 || uv.y < 0.0 || uv.y > 1.0 {
        return vec4<f32>(0.0, 0.0, 0.0, 0.0);
    }

    let sample = textureSampleLevel(floor_texture, floor_sampler, uv, 0.0);
    if sample.a <= 0.0 {
        return vec4<f32>(0.0, 0.0, 0.0, 0.0);
    }
    // egui premultiplies in GAMMA space, so the un-premultiply must be too, in
    // BOTH arms. `egui-wgpu-0.35.0/src/egui.wgsl` writes `gamma(C) * A` for a
    // non-sRGB swapchain and `linear_from_gamma_rgb(gamma(C) * A)` for an sRGB one
    // — an encoding of an already-premultiplied gamma value, not a premultiply in
    // linear space. Dividing the linear texel by `A` returns 0.428 where 1.0 is
    // correct at `C = 1, A = 0.5`.
    let gamma_premultiplied = select(
        gamma_from_linear_rgb(sample.rgb),
        sample.rgb,
        volume.floor_geo.w > 0.5,
    );
    let linear = linear_from_gamma_rgb(gamma_premultiplied / sample.a);
    return vec4<f32>(linear, sample.a);
}

@fragment
fn fs_raymarch(in: RaymarchVertex) -> @location(0) vec4<f32> {
    let eye = volume.eye_in_box.xyz;
    let direction = normalize(unproject(in.ndc, 1.0) - eye);
    let span = slab_entry_exit(eye, direction);
    if span.y <= span.x {
        return vec4<f32>(0.0, 0.0, 0.0, 0.0);
    }

    let floor_t = select(-1.0, floor_hit(eye, direction), volume.flags.w > 0.5);
    let floor_fade = clamp(1.0 + eye.z / FLOOR_BELOW_FADE, 0.0, 1.0);

    // Cells this ray crosses per unit of `t`, in the grid's anisotropic cell
    // metric — the same "direction inside the length" shape as `step_length_km`,
    // so the step is flags.z cells ALONG THE RAY whatever the direction. The floor
    // on `dt` honours the ceiling from the other side: a span that outruns
    // RAYMARCH_STEP_CEILING steps is covered in stretched steps, not truncated.
    let cells_per_t = max(length(direction * volume.grid_dims.xyz), 1.0);
    let dt = max(volume.flags.z / cells_per_t, (span.y - span.x) / f32(RAYMARCH_STEP_CEILING));
    let segment_km = step_length_km(direction, dt);
    let shade = volume.flags.x > 0.5;
    // The view mode, selected by the threshold lane's sign — see the uniform.
    let iso = volume.eye_in_box.w >= 0.0;

    // Stratified sampling: the comb starts a per-pixel fraction of a step past the
    // entry. The expected sample count over the jitter is exactly `span / dt`, so
    // the integral stays unbiased and the residual quantisation is per-pixel noise
    // rather than screen-space contours.
    let jitter = blue_noise_jitter(in.clip_position.xy);
    var t = span.x + jitter * dt;
    var transmittance = 1.0;
    // Premultiplied and LINEAR; converted to egui's convention once, at the end.
    var accumulated = vec3<f32>(0.0, 0.0, 0.0);

    for (var i: i32 = 0; i < RAYMARCH_STEP_CEILING; i = i + 1) {
        if t >= span.y {
            break;
        }
        let p = eye + direction * t;
        let sample = field_at(p);
        let index = sample.x;
        let coverage = sample.y;
        if iso {
            // First crossing wins. The floor arm below still composites: an opaque
            // surface leaves zero transmittance, so ground behind it stays hidden
            // and beside it visible, which puts the isosurface ON the floor.
            if iso_hit_test(sample) {
                let hit_t = refine_iso_hit(eye, direction, max(t - dt, span.x), t);
                let colour = iso_surface_colour(eye + direction * hit_t);
                accumulated = accumulated + transmittance * colour;
                transmittance = 0.0;
                break;
            }
        } else if coverage >= COVERAGE_SKIP && index > volume.transfer.y {
            let entry = textureSampleLevel(lut_texture, lut_sampler, lut_coord(index), 0.0);
            // The table holds gamma-encoded colour (the same `get_color_for_value`
            // the 2D products paint with); accumulation is physical, so decode.
            var colour = linear_from_gamma_rgb(entry.rgb);
            if shade {
                colour = colour * shading(p);
            }
            // The opacity ramp: 0 at the skip threshold, 1 at `transfer.w` index
            // units above it, smoothstep between. It scales the OPTICAL DEPTH, not
            // the accumulated alpha, so a saturating extinction still saturates. At
            // `transfer.w = 0` the 1e-6 divisor floor makes it hard.
            let rise = clamp((index - volume.transfer.y) / max(volume.transfer.w, 1e-6), 0.0, 1.0);
            let opacity_ramp = rise * rise * (3.0 - 2.0 * rise);
            // Coverage likewise scales the optical depth — the reconstruction's own
            // weighting applied to absorption. See COVERAGE_SKIP.
            let absorbed =
                1.0 - exp(-entry.a * opacity_ramp * coverage * volume.transfer.x * segment_km);
            accumulated = accumulated + transmittance * absorbed * colour;
            transmittance = transmittance * (1.0 - absorbed);
            if transmittance < volume.transfer.z {
                break;
            }
        }
        t = t + dt;
    }

    // Which side of the bottom plane the EYE is on decides the composite order,
    // and it must be the same function of the same number as `floor_fade` or one
    // frame composites its floor two ways in two pixels (measured against the
    // per-pixel `floor_t > span.x` this replaced: 68 of 175 swept cameras were not
    // uniform in their arm). `>= 0.0` rather than `> 0.0` because an eye exactly
    // on the plane has no floor hit — `floor_hit` refuses `t <= 0`.
    let eye_above_plane = eye.z >= 0.0;

    // The floor behind the volume: an eye above the plane meets it at the box
    // exit, so light the march did not absorb lands on ground and composites under
    // the accumulation. Coverage is the floor's alpha times the fade, 1 above.
    var transmitted = transmittance;
    if floor_t >= 0.0 && eye_above_plane {
        let ground = floor_colour(eye, direction, floor_t);
        let cover = ground.a * floor_fade;
        accumulated = accumulated + transmittance * cover * ground.rgb;
        transmitted = transmittance * (1.0 - cover);
    } else if floor_t >= 0.0 && floor_fade > 0.0 {
        // The floor in front: an eye under the plane meets it at (or before) the
        // box entry, so the faded ground composites OVER the march; at fade 0 the
        // arm vanishes. An inside-the-box eye is above the plane.
        let ground = floor_colour(eye, direction, floor_t);
        let cover = ground.a * floor_fade;
        accumulated = ground.rgb * cover + accumulated * (1.0 - cover);
        transmitted = transmitted * (1.0 - cover);
    }

    let alpha = 1.0 - transmitted;
    if alpha <= 0.0 {
        return vec4<f32>(0.0, 0.0, 0.0, 0.0);
    }
    // egui premultiplies in GAMMA space, so the offscreen must hold gamma(C) * A:
    // un-premultiply, encode, re-premultiply. `accumulated` is bounded above by
    // `alpha`, so the division cannot overshoot.
    let straight_linear = accumulated / alpha;
    return vec4<f32>(gamma_from_linear_rgb(straight_linear) * alpha, alpha);
}

@group(0) @binding(5) var blit_texture: texture_2d<f32>;
@group(0) @binding(6) var blit_sampler: sampler;

struct BlitVertex {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) uv: vec2<f32>,
}

@vertex
fn vs_blit(@location(0) clip_xy: vec2<f32>) -> BlitVertex {
    var out: BlitVertex;
    out.clip_position = vec4<f32>(clip_xy, 0.0, 1.0);
    // Clip space has y up; a texture has v down.
    out.uv = vec2<f32>(clip_xy.x, -clip_xy.y) * 0.5 + vec2<f32>(0.5, 0.5);
    return out;
}

// The non-sRGB target: egui writes gamma-encoded premultiplied colour and blends
// it in gamma space, which the offscreen already holds, so this is a pass-through.
@fragment
fn fs_blit_gamma_framebuffer(in: BlitVertex) -> @location(0) vec4<f32> {
    return textureSampleLevel(blit_texture, blit_sampler, in.uv, 0.0);
}

// The sRGB target. egui's `fs_main_linear_framebuffer` calls
// `linear_from_gamma_rgb` on a value it has ALREADY premultiplied in gamma space,
// so it composites `linear(C*A)`, not `linear(C)*A`. The principled version
// measured 60/255 off against egui's own `rect_filled`; decoding the
// premultiplied value directly took the delta to zero.
@fragment
fn fs_blit_linear_framebuffer(in: BlitVertex) -> @location(0) vec4<f32> {
    let premultiplied_gamma = textureSampleLevel(blit_texture, blit_sampler, in.uv, 0.0);
    return vec4<f32>(
        linear_from_gamma_rgb(premultiplied_gamma.rgb),
        premultiplied_gamma.a,
    );
}
