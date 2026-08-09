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
    // xyz: the camera position in box space. w: reserved, written as zero.
    //
    // This is the *perspective* eye. Rays are cast from it, which is what makes
    // a camera inside the box behave (the entry parameter clamps to zero rather
    // than starting behind the viewer). An orthographic camera has no such
    // point and would need a different derivation.
    eye_in_box: vec4<f32>,
    // xyz: the physical extent of the box in kilometres. w: reserved, zero.
    box_size_km: vec4<f32>,
    // xyz: the voxel counts along each axis, as floats. w: reserved, zero.
    grid_dims: vec4<f32>,
    // xyz: unit light direction in box space. w: the ambient term, 0..1.
    light_dir_ambient: vec4<f32>,
    // x: extinction per kilometre at LUT alpha 1.
    // y: the palette index at or below which a cell contributes nothing.
    // z: the transmittance at which the march stops early.
    // w: the opacity ramp's width above y, in 0-1 index units; 0 is hard.
    transfer: vec4<f32>,
    // x: 1 to shade with the gradient, 0 to skip it. y, z, w: reserved, zero.
    flags: vec4<f32>,
}

@group(0) @binding(0) var<uniform> volume: Volume;

// The voxel grid: `R8Unorm` palette indices, sampled `Linear`. Filtering
// *within* data is exactly linear dBZ interpolation because index-to-dBZ is
// affine, which is the stated reason for the format.
@group(0) @binding(1) var grid_texture: texture_3d<f32>;
@group(0) @binding(2) var grid_sampler: sampler;

// The 256-entry colour table those indices name, as a 256x1 2D texture sampled
// `Nearest`. A `texture_1d` would be the honest shape and is not usable: GLES
// 3.0 has no `sampler1D` at all.
@group(0) @binding(3) var lut_texture: texture_2d<f32>;
@group(0) @binding(4) var lut_sampler: sampler;

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
// It is a **ceiling**, not the step count: the step length is derived from the
// voxel size below, the loop breaks at the box exit, and the ceiling only
// matters if a grid ever outgrows it — the desktop 256 x 256 x 128 grid's
// longest diagonal is 384 cells, under it. When a grid does outgrow it, the
// `dt` floor in `fs_raymarch` stretches the steps to cover the span rather
// than truncating the far side of the volume.
const RAYMARCH_STEP_CEILING: i32 = 512;

// Cells one step advances along the ray, measured in the grid's own
// (anisotropic) cell metric.
//
// 1.0 is the sampling rate the data supports: the linear filter band-limits
// the field to about one cell, so one sample per cell — decorrelated between
// neighbouring pixels by the jitter below — resolves everything the grid
// holds. The 96-step march this replaced took one sample per ~2.7 cells on a
// horizontal ray and per ~15 z-cells on the shipped grid, and every surface it
// drew carried the quantisation as terracing that crawled under camera motion
// (measured: banding phase-locked to the screen within 2 px while the volume
// moved 17-45 px, recording of 2026-08-09).
const STEP_CELLS: f32 = 1.0;

// Entries in the colour table. Must equal `constants::VOLUME_LUT_BYTES / 4`;
// `the_shader_and_the_lut_constant_agree` pins that.
const LUT_ENTRIES: f32 = 256.0;

// Smallest ray-direction component the slab test will divide by. Guards the
// axis-parallel ray without relying on infinity arithmetic, which WGSL leaves
// implementation-defined and WebGL2 drivers disagree about.
const RAY_DIRECTION_EPSILON: f32 = 1e-6;

// Below this the central difference is noise rather than a surface, and
// normalising it would point the normal in an arbitrary direction.
const GRADIENT_EPSILON: f32 = 1e-6;

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

// The texel centre of palette entry `index`, where `index` is the 0-1 value an
// `R8Unorm` fetch returns.
fn lut_coord(index: f32) -> vec2<f32> {
    return vec2<f32>((index * (LUT_ENTRIES - 1.0) + 0.5) / LUT_ENTRIES, 0.5);
}

fn grid_at(p: vec3<f32>) -> f32 {
    return textureSampleLevel(grid_texture, grid_sampler, p, 0.0).r;
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
fn shading(p: vec3<f32>) -> f32 {
    let voxel = vec3<f32>(1.0) / volume.grid_dims.xyz;
    let gradient = vec3<f32>(
        grid_at(p + vec3<f32>(voxel.x, 0.0, 0.0)) - grid_at(p - vec3<f32>(voxel.x, 0.0, 0.0)),
        grid_at(p + vec3<f32>(0.0, voxel.y, 0.0)) - grid_at(p - vec3<f32>(0.0, voxel.y, 0.0)),
        grid_at(p + vec3<f32>(0.0, 0.0, voxel.z)) - grid_at(p - vec3<f32>(0.0, 0.0, voxel.z)),
    );
    let ambient = volume.light_dir_ambient.w;
    let magnitude = length(gradient);
    if magnitude < GRADIENT_EPSILON {
        return 1.0;
    }
    // The gradient climbs towards denser cells, so the outward-facing normal is
    // its negation.
    let normal = -gradient / magnitude;
    let lambert = max(dot(normal, normalize(volume.light_dir_ambient.xyz)), 0.0);
    return ambient + (1.0 - ambient) * lambert;
}

@fragment
fn fs_raymarch(in: RaymarchVertex) -> @location(0) vec4<f32> {
    let eye = volume.eye_in_box.xyz;
    let direction = normalize(unproject(in.ndc, 1.0) - eye);
    let span = slab_entry_exit(eye, direction);
    if span.y <= span.x {
        return vec4<f32>(0.0, 0.0, 0.0, 0.0);
    }

    // Cells this ray crosses per unit of `t`, in the grid's anisotropic cell
    // metric — the same "direction inside the length" shape as
    // `step_length_km`, for the same reason. The step is then STEP_CELLS cells
    // *along the ray* whatever the direction: a vertical ray through the
    // shipped grid takes ~128 samples and a horizontal one ~256, instead of
    // both taking 96 samples of wildly different physical lengths.
    //
    // The floor on `dt` is the ceiling honoured from the other side: a grid
    // whose chord exceeds RAYMARCH_STEP_CEILING cells gets the whole span in
    // ceiling-many stretched steps rather than a volume truncated mid-box.
    let cells_per_t = max(length(direction * volume.grid_dims.xyz), 1.0);
    let dt = max(STEP_CELLS / cells_per_t, (span.y - span.x) / f32(RAYMARCH_STEP_CEILING));
    let segment_km = step_length_km(direction, dt);
    let shade = volume.flags.x > 0.5;

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
        let index = grid_at(p);
        if index > volume.transfer.y {
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
            // R8Unorm fetch can express. The production bridge passes a real
            // width, which is what dissolves the palette's alpha cliff into a
            // fade — the hard shelf rims of the 2026-08-09 report — and it is
            // a *render* of the same data, softened exactly at the boundary
            // the palette already declares, never a reshaping of the field.
            let rise = clamp((index - volume.transfer.y) / max(volume.transfer.w, 1e-6), 0.0, 1.0);
            let opacity_ramp = rise * rise * (3.0 - 2.0 * rise);
            let absorbed =
                1.0 - exp(-entry.a * opacity_ramp * volume.transfer.x * segment_km);
            accumulated = accumulated + transmittance * absorbed * colour;
            transmittance = transmittance * (1.0 - absorbed);
            if transmittance < volume.transfer.z {
                break;
            }
        }
        t = t + dt;
    }

    let alpha = 1.0 - transmittance;
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
