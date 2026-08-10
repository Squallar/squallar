// The offscreen volume raymarch, and the quad that composites it into egui.
//
// Both live in one module because they share the 48-byte fullscreen quad and
// the two sRGB transfer functions, and because the naga translation test then
// has one source to check rather than two. They do NOT share a bind group: the
// raymarch owns bindings 0..4 and the blit owns 5..6, so that the two pipeline
// layouts can each declare only what their own entry points use while every
// binding in the module stays unique. Reusing 0..1 for the blit would be a
// duplicate group/binding pair in one WGSL module, which the spec forbids
// whether or not any single entry point sees both.
//
// Rules this file follows, every one of them a naga constraint rather than a
// preference (see `volume_raymarch.rs`'s module doc for the citations):
//
//   * `textureSampleLevel` everywhere. The march breaks on a data-dependent
//     condition, and implicit-LOD sampling under non-uniform control flow is a
//     hard validator failure on every target.
//   * `RAYMARCH_STEP_CEILING` is a `const` so it folds to a literal in the loop.
//   * one sampler per texture per pipeline.
//   * `textureNumLevels` appears nowhere; it is gated on GLSL core 130 with no
//     ES version at all, so it is unreachable on WebGL2 forever.

// ---------------------------------------------------------------------------
// Uniform block
// ---------------------------------------------------------------------------

// One `mat4x4<f32>` plus six `vec4<f32>`: 64 + 96 = 160 bytes, std140-clean.
//
// Every member is `f32`, including the two that are conceptually integers
// (`grid_dims`) and the one that is conceptually a bool (`flags`). Mixing
// integer and float members in a std140 block is where driver bugs live, and
// the cost of the float round-trip is one `f32()` that the compiler folds.
//
// `volume_uniform.rs` writes these 160 bytes by hand and pins every offset.
struct Volume {
    // Clip space to box space, where box space is the unit cube [0,1]^3 over
    // the voxel grid. Built compositionally by the caller
    // (box_from_world * world_from_view * view_from_clip), never by inverting a
    // general 4x4.
    box_from_clip: mat4x4<f32>,
    // xyz: the camera position in box space. w: the isosurface threshold in
    // 0-1 index units, or negative for the lit-volume march — negative, not
    // zero, because an index-0 threshold is a real configuration ("the
    // surface of any data").
    //
    // xyz is the *perspective* eye. Rays are cast from it, which is what makes
    // a camera inside the box behave (the entry parameter clamps to zero rather
    // than starting behind the viewer). An orthographic camera has no such
    // point and would need a different derivation.
    eye_in_box: vec4<f32>,
    // xyz: the physical extent of the box in kilometres. w: the camera's
    // vertical exaggeration, >= 1 — the one place the shader is told about the
    // stretch, and only the *shading* reads it: normals are taken against the
    // displayed geometry, so a slope that is drawn steep is lit steep. Optical
    // depth stays against xyz alone, which is the honest, unexaggerated
    // kilometre.
    box_size_km: vec4<f32>,
    // xyz: the voxel counts along each axis, as floats. w: the centre index
    // a diverging product's isosurface measures its threshold from, in 0-1
    // index units, or negative for a sequential product whose threshold
    // reads the index directly. Only read in isosurface mode.
    grid_dims: vec4<f32>,
    // xyz: unit light direction in box space. w: the ambient term, 0..1.
    light_dir_ambient: vec4<f32>,
    // x: extinction per kilometre at LUT alpha 1.
    // y: the palette index at or below which a cell contributes nothing —
    //    the PALETTE's own transparent run, not an emptiness test. Air is
    //    excluded by coverage, which is a property of the measurement rather
    //    than of the table (COVERAGE_SKIP for the lit volume, COVERAGE_FLOOR
    //    for the isosurface's binary hit test).
    // z: the transmittance at which the march stops early.
    // w: the opacity ramp's width above y, in 0-1 index units; 0 is hard.
    transfer: vec4<f32>,
    // x: 1 to shade with the gradient, 0 to skip it.
    // y: the reconstruction level the march samples the grid at, in mip
    //    units: 0 is the raw trilinear field — the bit-exact instrument
    //    configuration every mask harness runs at — and values towards 1
    //    blend continuously into the hand-built two-cell mean below it. The
    //    render-side softening that turns single-voxel spikes and tilt-shelf
    //    cliffs into cloud. Never negative: the sentinel that used to select
    //    a nearest-neighbour snap went with the per-product split.
    // z: cells one step advances along the ray, in the grid's own anisotropic
    //    cell metric. 1 is the instrument default the silhouette harness
    //    mirrors; the cloud rung halves it, which is what takes the jitter's
    //    per-step opacity quantum below visibility. A zero (a stale buffer)
    //    falls to the dt floor against the ceiling rather than hanging.
    // w: 1 to draw the map floor — the ground texture on the box's bottom
    //    face, composited behind the volume at the ray's own plane hit. 0 —
    //    the instrument default — draws no floor and leaves every mask
    //    exactly as it was.
    flags: vec4<f32>,
}

@group(0) @binding(0) var<uniform> volume: Volume;

// The voxel grid: `Rg8Unorm`, **coverage-premultiplied**, sampled `Linear` on
// both channels.
//
//   R = coverage x index      G = coverage
//
// where coverage is 1 for a cell the radar measured and 0 for empty air. The
// march reconstructs `index = R_bar / G_bar` — the coverage-weighted mean over
// the covered texels alone, because air contributes 0 to BOTH the numerator
// and the denominator and so drops out of the average instead of taking part
// in it as a value. See `field_at`.
@group(0) @binding(1) var grid_texture: texture_3d<f32>;
@group(0) @binding(2) var grid_sampler: sampler;

// The 256-entry colour table those indices name, as a 256x1 2D texture sampled
// `Nearest`. A `texture_1d` would be the honest shape and is not usable: GLES
// 3.0 has no `sampler1D` at all.
@group(0) @binding(3) var lut_texture: texture_2d<f32>;
@group(0) @binding(4) var lut_sampler: sampler;

// The map floor: the ground the box stands on, as the 2D pane would draw it,
// registered to the box footprint (u across x, v down from the north edge).
// In its own group because its lifetime is its own — group 0 is rebuilt per
// grid upload, the floor per floor render, and a pipeline may bind a
// placeholder here when no floor is in hand. Straight gamma-encoded RGBA,
// opaque where there is ground; the march decodes and composites it at the
// ray's own hit with the bottom plane.
@group(1) @binding(0) var floor_texture: texture_2d<f32>;
@group(1) @binding(1) var floor_sampler: sampler;

// ---------------------------------------------------------------------------
// sRGB transfer functions
// ---------------------------------------------------------------------------
//
// Character-for-character egui's own (`egui-wgpu-0.35.0/src/egui.wgsl:44-57`).
// Matching egui is the requirement here, not being right in the abstract, so
// these are copied rather than rewritten.

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

// ---------------------------------------------------------------------------
// The raymarch
// ---------------------------------------------------------------------------

// The most samples one ray may take, whatever the step length works out to.
//
// A `const` rather than a uniform, so the loop bound is a compile-time constant
// — naga emits it as `const int RAYMARCH_STEP_CEILING = 512;` and folds it
// where a conversion forces the issue. A uniform bound would compile, look
// identical, and hide the march's cost from the driver on the target where
// fill rate is the whole risk.
//
// It is a **ceiling**, not the step count: the step length arrives per-frame
// in `flags.z`, the loop breaks at the box exit, and the ceiling only
// matters if a span ever outruns it. 1024 rather than 512 because the cloud
// rung marches half-cell steps and the desktop 256 x 256 x 128 grid's longest
// diagonal is 384 cells — 768 half-cell steps, which must fit or the far
// corner of the box would fall to the stretched-dt fallback on every diagonal
// view. When a span does outrun it, the `dt` floor in `fs_raymarch` stretches
// the steps to cover it rather than truncating the far side of the volume.
const RAYMARCH_STEP_CEILING: i32 = 1024;

// Entries in the colour table. Must equal `constants::VOLUME_LUT_BYTES / 4`;
// `the_shader_and_the_lut_constant_agree` pins that.
const LUT_ENTRIES: f32 = 256.0;

// Smallest ray-direction component the slab test will divide by. Guards the
// axis-parallel ray without relying on infinity arithmetic, which WGSL leaves
// implementation-defined and WebGL2 drivers disagree about.
const RAY_DIRECTION_EPSILON: f32 = 1e-6;

// How far under the bottom plane, in box heights, the eye travels before the
// floor is fully gone. From above the floor is the opaque ground the box
// stands on; from below it must not wall the volume off — the user asked for
// exactly that — so its coverage is scaled by eye depth below the plane:
// 1 at the plane and above (every above-plane pixel is bit-identical to the
// pre-fade composite), 0 at this depth — ~1.4 km of the default 18 km
// column, so the wall never persists: steady-state below is fully
// transparent, and the descent dissolves it over the first FLOOR_BELOW_FADE
// of eye depth rather than in a single step.
//
// What this is NOT: a pop-free crossing. Coverage is continuous in eye depth
// (1 on both sides of the plane), but the composite's *order* switches at
// the crossing — behind the accumulation from above, in front of it from
// below — so a pixel where the volume occludes the floor can jump on the
// crossing frame, at up to full coverage. The band was still put entirely
// BELOW the plane on purpose: a band reaching above it would thin the
// resting ground out of every low-angle above-plane view — a permanent cost
// to the GR-solid floor — to soften a transient the descent already
// dissolves in under a band-width of travel.
const FLOOR_BELOW_FADE: f32 = 0.08;

// Below this the central difference is noise rather than a surface, and
// normalising it would point the normal in an arbitrary direction.
//
// The gradient it bounds is per *displayed kilometre* (`shading` divides the
// index differences by `cell_km`), so the same field measures differently as
// the cell size changes: this floor rescales with the grid, and 1e-6 was
// tuned against the old unitless difference. It stays correct as a NaN guard
// because it sits orders of magnitude under any real surface at every
// shipped cell size — one R8 index step (1/255) across the coarsest 1.8 km
// cell is still ~2e-3 per km — but it is a zero-detector, not a
// surface-classifier, and must not be read as a tuned threshold.
const GRADIENT_EPSILON: f32 = 1e-6;

// Bisection steps refining an isosurface hit between the sample that crossed
// and the one before it. A `const` bound for the same naga reason as
// RAYMARCH_STEP_CEILING. Eight halvings of one march step place the surface
// to under 1/256 of a step — finer than the eight-bit index can express — so
// the per-pixel jitter's one-step start offset stops wobbling the surface.
const ISO_REFINE_STEPS: i32 = 8;

// Reconstructed coverage at or above which a sample is INSIDE the data, for
// the one decision that has to be binary: the isosurface's hit test.
//
// 0.5 rather than any other number because it is the *nearest-neighbour
// decision boundary*: along an axis the trilinear coverage field's half level
// set is exactly the midpoint between the last covered texel centre and the
// first uncovered one, so a sample above it sits in the cell of a texel that
// holds data and one below it in the cell of a texel that does not. An
// isosurface is a level set — a point is on one side or the other — so it gets
// a surface with the same *reach* as an honest nearest march, and one that is
// smooth rather than a staircase of cube faces.
//
// That equivalence is exact in 1-D and only approximate in 3-D, where the tent
// is the product of three axis weights: at u = v = w = 0.49 from a lone covered
// texel, trilinear coverage is 0.51^3 = 0.133, well under the cut, while
// nearest says inside. The corners of a lone texel's cell are therefore clipped
// — 0.5 is a rounded nearest march, not a bit-exact one, and the smooth surface
// is what it is chosen for.
//
// # It is a level-0 constant, and the isosurface marches at level 0
//
// Everything above is a statement about the RAW trilinear tent. At
// reconstruction level 1 the coverage field is a two-cell box convolved with
// that tent, and 0.5 stops meaning anything about texel cells: a lone measured
// voxel is an eighth of its coarse texel and reconstructs to coverage
// 32/255 = 0.125, a one-cell sheet to 128/255 = 0.502, so a `>= 0.5` cut
// deletes the first outright and all but destroys the second. This is the same
// erasure COVERAGE_SKIP refuses for the lit volume, arriving at the same
// features through the gate.
//
// So `volume::bridge` sends `reconstruction_lod = 0` on the isosurface branch
// — the smoothing rung is a presentation knob, and presentation is not what a
// level set of the data is — and this cut is only ever applied to the field the
// claim above is about. `an_isosurface_at_the_shipped_rung_keeps_its_sub_
// kernel_features` measures both rungs and pins it.
//
// **The lit volume does not use it**, and that is deliberate rather than an
// omission — see COVERAGE_SKIP.
const COVERAGE_FLOOR: f32 = 0.5;

// Coverage below which the LIT VOLUME skips a sample outright.
//
// A fill-rate and precision floor, **not** a decision about where the data is,
// because for an integrated quantity there is no such decision to make: the
// march accumulates optical depth along a ray, coverage is the fraction of a
// sample's reconstruction footprint that was measured, and weighting the
// optical depth by it is the partial-volume answer. The trilinear tent is a
// partition of unity, so the coverage field integrates to exactly the hard
// field's volume: the weighting REDISTRIBUTES an edge voxel's opacity across
// the reconstruction footprint rather than adding any, which is what a
// band-limited reconstruction of a hard edge is. (That conservation is of
// `coverage x extinction`, so it is exact in the LUT's alpha only where the
// alpha is constant across the indices the edge sweeps; where the ramp's alpha
// varies the reconstruction still redistributes rather than invents, but the
// integral is the alpha-weighted one, not the hard field's.)
//
// A COVERAGE_FLOOR-style cut here would instead destroy optical depth, and
// above reconstruction level 0 it destroys whole features: a lone measured
// voxel occupies an eighth of its coarse texel, so at the cloud rung's level
// its coverage is 0.125 everywhere and a 0.5 cut erases it outright —
// measured, `the_smoothed_reconstruction_spreads_a_lone_voxel` went from 43
// painted pixels to 0. That is the same class of erasure as the naive mip's
// (-90% of top-class pixels at 160 km), arriving through the gate instead of
// through the mean, and the reconstruction rung exists to soften spikes rather
// than to delete them.
//
// 1/255 is one stored quantum of the coverage channel — less coverage than a
// single texel can hold. It is a fill-rate floor and not a claim of
// invisibility: at DEFAULT_EXTINCTION_PER_KM over a several-kilometre segment a
// sample at exactly this coverage absorbs on the order of a couple of percent
// (~5 levels of 255 across a ~5 km segment), which is one or two eight-bit
// steps in the pixel behind it, not none. What licenses the skip is that the
// samples it drops are the outermost tail of the reconstruction tent, where the
// alternative to a small error is the whole march paying for footprints that
// are almost entirely air.
const COVERAGE_SKIP: f32 = 1.0 / 255.0;

// Divisor floor for the coverage reconstruction, far under COVERAGE_SKIP so it
// can only ever be reached by a sample that is about to be discarded. It exists
// so an all-air fetch — R = G = 0 exactly — yields index 0 rather than a NaN
// that would poison the comparisons downstream.
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

// Where the ray enters and leaves the unit cube, as (entry, exit) parameters.
// `exit <= entry` means it misses. Entry is clamped to zero so that a camera
// inside the box marches from itself rather than from behind itself.
fn slab_entry_exit(ro: vec3<f32>, rd: vec3<f32>) -> vec2<f32> {
    let magnitude = max(abs(rd), vec3<f32>(RAY_DIRECTION_EPSILON));
    let signed = select(magnitude, -magnitude, rd < vec3<f32>(0.0));
    let inverse = vec3<f32>(1.0) / signed;
    let to_min = (vec3<f32>(0.0) - ro) * inverse;
    let to_max = (vec3<f32>(1.0) - ro) * inverse;
    let near = min(to_min, to_max);
    let far = max(to_min, to_max);
    let entry = max(max(near.x, near.y), max(near.z, 0.0));
    let exit = min(far.x, min(far.y, far.z));
    return vec2<f32>(entry, exit);
}

// Kilometres one `dt` step covers along `rd`.
//
// The direction is INSIDE the length, not outside it. `dt * length(box_size_km)`
// compiles, reads plausibly and is wrong: it gives every direction the box's
// diagonal. On a 240 x 240 x 20 km box that is 340 km, so a vertical step comes
// out 17x too long and a horizontal one 1.4x — leaving a vertical ray 12x more
// opaque, relative to a horizontal one, than it should be. It looks like haze
// rather than like a bug.
fn step_length_km(rd: vec3<f32>, dt: f32) -> f32 {
    return length(rd * dt * volume.box_size_km.xyz);
}

// The texel centre of palette entry `index`, where `index` is the 0-1 value
// `field_at` reconstructs — the same units an eight-bit unorm fetch returns.
fn lut_coord(index: f32) -> vec2<f32> {
    return vec2<f32>((index * (LUT_ENTRIES - 1.0) + 0.5) / LUT_ENTRIES, 0.5);
}

// The field the march reads: `x` is the reconstructed palette index, `y` the
// reconstructed coverage, both at the level flags.y, from ONE texture fetch.
//
// # The reconstruction
//
// The texture holds `R = coverage x index`, `G = coverage`. Hardware `Linear`
// filtering returns the tent-weighted means `R_bar` and `G_bar` of the same
// eight (or, between levels, sixteen) texels under the same weights, so
//
//     R_bar / G_bar  =  sum(w_i c_i x_i) / sum(w_i c_i)
//
// which is the coverage-weighted mean of the index over the **covered** texels
// alone. Empty air has c = 0 and contributes nothing to either sum, so it
// cannot drag the result anywhere: the reconstructed index always lies inside
// the convex hull of the stored indices that surround the sample, for every
// product, whatever shape its palette ramp has. That is the whole point of the
// premultiplication, and it is what retires the per-product
// nearest-versus-blend split this shader used to carry — under which the seven
// diverging, inverted and flat-ramped products marched nearest and looked
// blocky, because a plain `R8Unorm` blend against the no-data index 0 swept
// through every intervening palette band and manufactured structure. The shape
// of the defect is the KLOT 2026-08-10 NROT arcs; the number is the 8^3
// synthetic fixture that reproduces them, `coverage_reconstruction_never_paints
// _a_band_the_data_does_not_occupy`, whose control render paints 6267 pixels of
// a band the data never occupies against 122 honest ones.
//
// A legitimate index 0 would still work — it adds 0 to R and 1 to G, so it is
// counted **as a zero** rather than as an absence — though the encoding does
// not produce one: `rustdar_radar::voxel::ramp_index` clamps every finite
// measurement to 1..=255.
//
// # The level
//
// The grid travels with one hand-built mip below it — each level-1 texel the
// plain box mean of its eight level-0 texels **in both channels**, which under
// the ratio above is exactly the occupancy-weighted mean of the index and the
// occupancy itself — and the sampler filters between levels, so this one fetch
// reconstructs the field through a kernel that widens continuously with
// flags.y: 0 is the raw trilinear tent, 1 is a two-cell box convolved with a
// tent. The alternatives measured and rejected on fill-rate grounds were a
// tricubic B-spline (eight taps) and a four-tap tetrahedral average (which
// moired against the per-pixel jitter).
//
// This softening is *presentation*, exactly like the opacity ramp: the grid,
// the palette and the threshold's anchor are untouched, and flags.y = 0 — the
// uniform's default — is the bit-exact raw field every mask instrument was
// written against (at LOD exactly 0 the level-1 weight is exactly zero).
// flags.y is never negative any more; the sentinel that used to select a
// nearest snap is gone with the split it served.
fn field_at(p: vec3<f32>) -> vec2<f32> {
    let texel = textureSampleLevel(grid_texture, grid_sampler, p, volume.flags.y).rg;
    // No `select`: an all-air fetch is R = G = 0 exactly, so the floored
    // divisor returns index 0 for it, and every covered fetch has G well above
    // the floor by the time the sample is used at all.
    return vec2<f32>(texel.r / max(texel.g, COVERAGE_EPSILON), texel.g);
}

// The premultiplied channel on its own — `coverage x index` — which is what
// the lit volume's gradient is taken of.
//
// **Not** the reconstructed index. Inside the data coverage is 1 and the two
// are identical, so nothing about interior shading changes; at an echo edge
// the premultiplied channel falls continuously to zero while the reconstructed
// index does not (it stays a real mean of real neighbours right up to the
// cut), so this is the one that has a gradient there at all — and it points
// out of the data, which is the normal of the surface being drawn. One fetch,
// like the field it comes from.
fn shading_field(p: vec3<f32>) -> f32 {
    return textureSampleLevel(grid_texture, grid_sampler, p, volume.flags.y).r;
}

// Deterministic per-pixel jitter in [0, 1): Jimenez's interleaved gradient
// noise over the fragment's framebuffer coordinate.
//
// The march's sample comb is offset by this fraction of a step, per pixel.
// Without it the comb is phase-locked to the eye, and every iso-`t` shell
// draws a contour that stays put in screen space while the volume slides
// beneath it — the "slithering" the 2026-08-09 recording shows. The jitter
// trades that coherent crawling for fine noise that is **static**: the hash
// reads nothing but the pixel coordinate, so it must never be given a time
// term — animated jitter is shimmer, which is the same artifact at one remove.
//
// This polynomial rather than a sin-based hash because `sin` at large
// arguments is where mobile GLES precision goes to die; fract/dot/multiply
// stay exact in f32 at these magnitudes.
fn interleaved_gradient_noise(px: vec2<f32>) -> f32 {
    let magic = vec3<f32>(0.06711056, 0.00583715, 52.9829189);
    return fract(magic.z * fract(dot(px, magic.xy)));
}

// Diffuse shading from the central-difference gradient, in 0..1.
//
// Six extra fetches against the march's one, which measured 2.4x on an RTX 3090
// at 1440x900 (0.774 ms against 0.325). That is the whole reason this is a
// separately selectable rung rather than something the shader always does.
//
// Two decisions here are the difference between "lit voxels" and "lit cloud",
// and both were arrived at by rendering a real convective volume (KCRP
// 2017-08-26, the Harvey landfall) rather than by argument:
//
//   * The gradient is taken in the *displayed* kilometre, not in box units.
//     Box space is the unit cube over a pancake — 160 x 160 x 18 km at the
//     tightest default, 25.6:1 at the widest — so a difference of raw box-space
//     samples under-weights the vertical component by the box's aspect ratio,
//     and every echo top is lit as though it were nearly flat. Dividing each
//     component by that axis's displayed cell size (the true cell, stretched
//     by the exaggeration in w) makes the normal the normal of the surface
//     the user is actually looking at.
//
//   * Half-Lambert (Valve's wrap term, squared) instead of a clamped cosine.
//     A cloud has no terminator: light scatters through it, so the away side
//     is dimmer, never cut off. `max(dot, 0)` draws a hard day/night line
//     across every storm core, and that line lands exactly where the gradient
//     is noisiest — it reads as a torn edge. The wrap term is monotone in the
//     same dot product with no clamp corner, so the same geometry shades
//     smoothly from lit to ambient.
fn shading(p: vec3<f32>) -> f32 {
    let voxel = vec3<f32>(1.0) / volume.grid_dims.xyz;
    // One displayed cell along each axis, in kilometres. `box_size_km.w` is
    // the vertical exaggeration, >= 1 by the uniform's contract.
    let cell_km = vec3<f32>(
        volume.box_size_km.x,
        volume.box_size_km.y,
        volume.box_size_km.z * volume.box_size_km.w,
    ) * voxel;
    // Differences of the same reconstruction the march reads, at the same
    // level, so the normal belongs to the surface being drawn: raw differences
    // over a smoothed field would light every voxel corner the smoothing just
    // removed. Of the *premultiplied* channel — see `shading_field` for why
    // that and not the reconstructed index.
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
    // The gradient climbs towards denser cells, so the outward-facing normal is
    // its negation.
    let normal = -gradient / magnitude;
    let wrap = 0.5 + 0.5 * dot(normal, normalize(volume.light_dir_ambient.xyz));
    return ambient + (1.0 - ambient) * wrap * wrap;
}

// ---------------------------------------------------------------------------
// The isosurface
// ---------------------------------------------------------------------------
//
// A per-pane view mode beside the lit volume: instead of accumulating alpha,
// the march finds the first crossing of a threshold, refines it by bisection
// and paints it as one opaque, gradient-lit surface. Selected by the sign of
// `eye_in_box.w` (the threshold; negative = lit volume). The threshold reads
// the DATA — the interpolated palette index — never the LUT's alpha, so a
// Volume Alpha curve restyles the lit volume and leaves the isosurface where
// the values put it.

// The scalar field the isosurface is a level set of: the index itself for a
// sequential product, the distance from the diverging centre (`grid_dims.w`)
// for a diverging one — which renders BOTH lobes of a velocity couplet, each
// wearing its own palette colour.
fn iso_field(index: f32) -> f32 {
    return select(index, abs(index - volume.grid_dims.w), volume.grid_dims.w >= 0.0);
}

// Whether the field at `sample` — (index, coverage), as `field_at` returns it
// — is at or beyond the iso threshold.
//
// The coverage term excludes unmeasured air, and it is what the old
// `index > transfer.y` term was standing in for: without an air test a
// diverging centre reads the no-data index 0 as a strong inbound crossing and
// shrink-wraps the whole coverage cone, and for ρHV — whose centre sits at the
// *top* of its ramp — index 0 is the most extreme "hit" the field can produce.
// Coverage says the same thing directly, for every product, and it says it
// without borrowing the palette's fade band, which is a statement about the
// table rather than about no-data.
fn iso_hit_test(sample: vec2<f32>) -> bool {
    return sample.y >= COVERAGE_FLOOR && iso_field(sample.x) >= volume.eye_in_box.w;
}

// The crossing parameter between a sample outside the surface at `t_lo` and
// one inside at `t_hi`, by ISO_REFINE_STEPS halvings.
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

// The isosurface's own level-set function at `p`, coverage-premultiplied:
// `iso_field(index) x coverage`.
//
// The coverage factor is the same move `shading_field` makes one level down,
// for the same reason. `iso_field` of an air sample is meaningless — for ρHV,
// whose centre is at the top of the ramp, it is the largest value the function
// takes — so an unweighted difference across the data boundary would point the
// normal into the air rather than out of it. Multiplying by coverage sends air
// to zero, which reads as "far outside the surface" for every product, and
// inside the data coverage is 1 so the level set is exactly the one
// `iso_hit_test` uses.
fn iso_shading_field(p: vec3<f32>) -> f32 {
    let sample = field_at(p);
    return iso_field(sample.x) * sample.y;
}

// `shading`, over the isosurface's own field: the normal must belong to the
// level set being drawn, and for a diverging product that set's gradient is
// not the density's — on the inbound lobe the density *falls* toward the
// core, so a density normal would light the surface from inside. Same six
// fetches, same displayed-kilometre metric, same half-Lambert wrap.
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

// The lit, linear colour of the isosurface at `p`: the palette's colour for
// the value there, always gradient-lit — an unlit opaque surface is a
// silhouette, so the isosurface shades on every quality rung.
fn iso_surface_colour(p: vec3<f32>) -> vec3<f32> {
    let index = field_at(p).x;
    let entry = textureSampleLevel(lut_texture, lut_sampler, lut_coord(index), 0.0);
    return linear_from_gamma_rgb(entry.rgb) * iso_shading(p);
}

// Where this ray meets the box's bottom face, or a negative number for a ray
// that never does.
//
// The floor is the z = 0 plane clipped to the unit square in x and y — the
// box's own bottom face, so a hit is always on the box boundary: for an eye
// above the plane it coincides with the ray's box exit, and for an eye below
// it with (or before) the entry. That dichotomy is what lets the march
// composite the floor with one comparison after the loop — behind the
// accumulation from above, in front of it (faded) from below — instead of
// interleaving it into the loop.
fn floor_hit(eye: vec3<f32>, direction: vec3<f32>) -> f32 {
    if abs(direction.z) < RAY_DIRECTION_EPSILON {
        return -1.0;
    }
    let t = -eye.z / direction.z;
    if t <= 0.0 {
        return -1.0;
    }
    let hit = eye + direction * t;
    if hit.x < 0.0 || hit.x > 1.0 || hit.y < 0.0 || hit.y > 1.0 {
        return -1.0;
    }
    return t;
}

// The floor's colour where the ray lands, linear and straight.
//
// v runs down from the box's north edge, which is row 0 of the floor image —
// the same convention as every raster the 2D pane draws.
fn floor_colour(eye: vec3<f32>, direction: vec3<f32>, t: f32) -> vec4<f32> {
    let hit = eye + direction * t;
    let sample = textureSampleLevel(floor_texture, floor_sampler, vec2<f32>(hit.x, 1.0 - hit.y), 0.0);
    return vec4<f32>(linear_from_gamma_rgb(sample.rgb), sample.a);
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
    // The floor's coverage: full at and above the bottom plane, fading to
    // nothing FLOOR_BELOW_FADE under it. An eye under the plane looking up
    // meets the floor at (or before) its entry into the volume, so a full-
    // coverage floor there would be an opaque wall in front of the whole
    // volume — the fade is what turns that wall transparent from below while
    // leaving every above-plane view exactly as composited before.
    let floor_fade = clamp(1.0 + eye.z / FLOOR_BELOW_FADE, 0.0, 1.0);

    // Cells this ray crosses per unit of `t`, in the grid's anisotropic cell
    // metric — the same "direction inside the length" shape as
    // `step_length_km`, for the same reason. The step is then flags.z cells
    // *along the ray* whatever the direction: at the instrument default of 1,
    // a vertical ray through the shipped grid takes ~128 samples and a
    // horizontal one ~256, instead of both taking 96 samples of wildly
    // different physical lengths. The linear filter band-limits the raw field
    // to about one cell, so 1 resolves everything the grid holds; the cloud
    // rung's half-cell step buys no resolution — it halves the per-step
    // opacity quantum, which is what takes the stratified jitter's residual
    // from a visible stipple to noise below an 8-bit level.
    //
    // The floor on `dt` is the ceiling honoured from the other side: a span
    // that outruns RAYMARCH_STEP_CEILING steps gets covered in ceiling-many
    // stretched steps rather than a volume truncated mid-box.
    let cells_per_t = max(length(direction * volume.grid_dims.xyz), 1.0);
    let dt = max(volume.flags.z / cells_per_t, (span.y - span.x) / f32(RAYMARCH_STEP_CEILING));
    let segment_km = step_length_km(direction, dt);
    let shade = volume.flags.x > 0.5;
    // The view mode, selected by the threshold lane's sign — see the uniform.
    let iso = volume.eye_in_box.w >= 0.0;

    // The sample comb starts a per-pixel fraction of a step past the entry —
    // stratified sampling, with the stratum offset hashed from the pixel. The
    // expected sample count over the jitter is exactly `span / dt`, so path
    // integrals stay unbiased; what the jitter buys is that the residual
    // quantisation is per-pixel noise instead of screen-space contours.
    let jitter = interleaved_gradient_noise(in.clip_position.xy);
    var t = span.x + jitter * dt;
    var transmittance = 1.0;
    // Premultiplied and LINEAR. The conversion to egui's gamma-space
    // premultiplied convention happens once, at the end.
    var accumulated = vec3<f32>(0.0, 0.0, 0.0);

    for (var i: i32 = 0; i < RAYMARCH_STEP_CEILING; i = i + 1) {
        // The step length is the voxel's, not the span's, so past the far face
        // is a real state the loop reaches rather than one it rounds into.
        if t >= span.y {
            break;
        }
        let p = eye + direction * t;
        let sample = field_at(p);
        let index = sample.x;
        let coverage = sample.y;
        if iso {
            // First crossing wins: refine it between this sample and the
            // last, paint it opaque and lit, and the march is over. The
            // floor arm below still composites — an opaque surface leaves
            // zero transmittance, so ground behind it stays hidden and
            // ground beside it stays visible, which is what puts the
            // isosurface ON the map floor rather than over it.
            if iso_hit_test(sample) {
                let hit_t = refine_iso_hit(eye, direction, max(t - dt, span.x), t);
                let colour = iso_surface_colour(eye + direction * hit_t);
                accumulated = accumulated + transmittance * colour;
                transmittance = 0.0;
                break;
            }
        } else if coverage >= COVERAGE_SKIP && index > volume.transfer.y {
            let entry = textureSampleLevel(lut_texture, lut_sampler, lut_coord(index), 0.0);
            // The table holds gamma-encoded colour, because it is produced by
            // the same `get_color_for_value` the 2D products paint with.
            // Accumulation is physical, so decode first.
            var colour = linear_from_gamma_rgb(entry.rgb);
            if shade {
                colour = colour * shading(p);
            }
            // The opacity ramp: 0 at the skip threshold, 1 at `transfer.w`
            // index units above it, smoothstep between. It scales the optical
            // depth rather than the accumulated alpha, so a saturating
            // extinction still saturates — which is what keeps the mask
            // harness's binary-alpha instrument meaningful.
            //
            // At `transfer.w = 0` (the uniform's default) the divisor's 1e-6
            // floor makes the ramp reach 1 within a millionth of an index step
            // of the threshold: the hard edge, to more precision than an
            // eight-bit index can express. The production bridge passes a real
            // width, which is what dissolves the palette's alpha cliff into a
            // fade — the hard shelf rims of the 2026-08-09 report — and it is
            // a *render* of the same data, softened exactly at the boundary
            // the palette already declares, never a reshaping of the field.
            let rise = clamp((index - volume.transfer.y) / max(volume.transfer.w, 1e-6), 0.0, 1.0);
            let opacity_ramp = rise * rise * (3.0 - 2.0 * rise);
            // Coverage scales the OPTICAL DEPTH, which is the same weighting
            // the reconstruction uses, applied to absorption: a sample whose
            // footprint is 60% measured absorbs 60% of what a fully measured
            // one would. It is 1 everywhere inside the data, so nothing in the
            // interior moves; across the outermost voxel it falls smoothly to
            // 0, which is what turns the silhouette from a step in alpha into
            // a ramp — and because the tent is a partition of unity the total
            // optical depth along a ray is the hard field's own, redistributed
            // rather than added to. See COVERAGE_SKIP.
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

    // The floor behind the volume: an eye above the plane meets it at the box
    // exit, so whatever light the march did not absorb lands on the ground
    // and composites under the accumulation — the same premultiplied algebra
    // as the volume's own samples, at the end because the plane bounds the
    // box from below and nothing can be behind it. Coverage is the floor's
    // own alpha times the fade — 1 above the plane, so this arm is unchanged
    // there.
    var transmitted = transmittance;
    if floor_t > span.x {
        let ground = floor_colour(eye, direction, floor_t);
        let cover = ground.a * floor_fade;
        accumulated = accumulated + transmittance * cover * ground.rgb;
        transmitted = transmittance * (1.0 - cover);
    } else if floor_t >= 0.0 && floor_fade > 0.0 {
        // The floor in front of the volume: an eye under the plane meets it
        // at (or before) the box entry, so the faded ground composites OVER
        // the march — the same over operator from the other side. At fade 0
        // this arm vanishes and the volume shows through where the wall
        // stood; `span.x` rather than 0 in the test above so an inside-the-
        // box eye — whose entry is clamped to 0 strictly above the plane —
        // never takes it.
        let ground = floor_colour(eye, direction, floor_t);
        let cover = ground.a * floor_fade;
        accumulated = ground.rgb * cover + accumulated * (1.0 - cover);
        transmitted = transmitted * (1.0 - cover);
    }

    let alpha = 1.0 - transmitted;
    if alpha <= 0.0 {
        return vec4<f32>(0.0, 0.0, 0.0, 0.0);
    }
    // egui premultiplies in GAMMA space (`Color32` is gamma-encoded and
    // multiplied by alpha after encoding), so the offscreen has to hold
    // gamma(C) * A. Encoding the premultiplied linear value directly would be
    // wrong at every alpha but 1, so un-premultiply, encode, re-premultiply.
    //
    // `accumulated` is bounded above by `alpha` — every contribution is
    // `transmittance * absorbed * colour` with `colour <= 1` — so the division
    // cannot overshoot.
    let straight_linear = accumulated / alpha;
    return vec4<f32>(gamma_from_linear_rgb(straight_linear) * alpha, alpha);
}

// ---------------------------------------------------------------------------
// The blit
// ---------------------------------------------------------------------------

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

// The non-sRGB target: egui writes gamma-encoded premultiplied colour and
// blends it in gamma space, and the offscreen already holds exactly that. So
// the blit is a pass-through and the blend state does the rest.
@fragment
fn fs_blit_gamma_framebuffer(in: BlitVertex) -> @location(0) vec4<f32> {
    return textureSampleLevel(blit_texture, blit_sampler, in.uv, 0.0);
}

// The sRGB target, where the colour-theoretically correct answer is measurably
// the wrong one.
//
// egui's `fs_main_linear_framebuffer` calls `linear_from_gamma_rgb` on a value
// it has ALREADY premultiplied in gamma space, i.e. it composites
// `linear(C*A)`, not `linear(C)*A`. The principled version — un-premultiply,
// decode, re-premultiply — measured 60/255 off against egui's own
// `rect_filled`; decoding the premultiplied value directly took the delta to
// zero. Matching egui is the requirement.
@fragment
fn fs_blit_linear_framebuffer(in: BlitVertex) -> @location(0) vec4<f32> {
    let premultiplied_gamma = textureSampleLevel(blit_texture, blit_sampler, in.uv, 0.0);
    return vec4<f32>(
        linear_from_gamma_rgb(premultiplied_gamma.rgb),
        premultiplied_gamma.a,
    );
}
