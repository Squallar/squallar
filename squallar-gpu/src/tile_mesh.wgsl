// One vector tile's tessellated fills and strokes, placed by a uniform rather
// than by the CPU.
//
// **This is egui's own shader with the texture factored out.** Every arithmetic
// step below — the colour unpack, the clip-space map, the dither, the two
// gamma conventions and which one each entry point applies — is copied from
// `egui-wgpu-0.35.0/src/egui.wgsl`, because the fills draw into the same pass,
// under the same blend state, beside primitives that go through that shader.
// A difference here is a seam in the map.
//
// The texture is a constant and not a sample: `mvt::render` emits every fill
// vertex at `WHITE_UV` with the default texture id, and the reserved texel at
// egui's atlas origin is opaque white, so `tex_gamma` is exactly `vec4(1.0)`
// and `in.color * tex_gamma` is `in.color` bit for bit. `squallar_egui`'s
// `tile_mesh::flatten` refuses any run that says otherwise, which is what
// keeps that a checked property rather than a belief.

struct VertexOutput {
    @location(0) color: vec4<f32>, // gamma 0-1
    @builtin(position) position: vec4<f32>,
};

struct Locals {
    /// The frame's size in POINTS, as egui's own uniform carries it.
    screen_size: vec2<f32>,
    /// `mvt::placement`'s affine: extent units to screen points.
    translation: vec2<f32>,
    scale: f32,
    /// 1 if dithering is enabled, 0 otherwise. Must be what
    /// `RendererOptions::dithering` gave egui's renderer.
    dithering: u32,
    _pad: vec2<f32>,
};
@group(0) @binding(0) var<uniform> r_locals: Locals;

fn interleaved_gradient_noise(n: vec2<f32>) -> f32 {
    let f = 0.06711056 * n.x + 0.00583715 * n.y;
    return fract(52.9829189 * fract(f));
}

fn dither_interleaved(rgb: vec3<f32>, levels: f32, frag_coord: vec4<f32>) -> vec3<f32> {
    var noise = interleaved_gradient_noise(frag_coord.xy);
    noise = (noise - 0.5) * 0.95;
    return rgb + noise / (levels - 1.0);
}

// 0-1 linear  from  0-1 sRGB gamma
fn linear_from_gamma_rgb(srgb: vec3<f32>) -> vec3<f32> {
    let cutoff = srgb < vec3<f32>(0.04045);
    let lower = srgb / vec3<f32>(12.92);
    let higher = pow((srgb + vec3<f32>(0.055)) / vec3<f32>(1.055), vec3<f32>(2.4));
    return select(higher, lower, cutoff);
}

// [u8; 4] SRGB as u32 -> [r, g, b, a] in 0.-1
fn unpack_color(color: u32) -> vec4<f32> {
    return vec4<f32>(
        f32(color & 255u),
        f32((color >> 8u) & 255u),
        f32((color >> 16u) & 255u),
        f32((color >> 24u) & 255u),
    ) / 255.0;
}

fn position_from_screen(screen_pos: vec2<f32>) -> vec4<f32> {
    return vec4<f32>(
        2.0 * screen_pos.x / r_locals.screen_size.x - 1.0,
        1.0 - 2.0 * screen_pos.y / r_locals.screen_size.y,
        0.0,
        1.0,
    );
}

@vertex
fn vs_main(
    @location(0) a_pos: vec2<f32>,
    @location(1) a_color: u32,
) -> VertexOutput {
    var out: VertexOutput;
    out.color = unpack_color(a_color);
    // `ShapeOrText::placed` spells this `scaling * p + translation`, and so
    // does this: same operands, same order.
    out.position = position_from_screen(r_locals.scale * a_pos + r_locals.translation);
    return out;
}

// One tile's pre-tessellated strokes.
//
// `a_pos` is the centreline point in MVT extent units, as an integer: MVT
// geometry arrives as integer varints and `tile_mesh::stroke` refuses any
// point that is not one, so this is a narrowing and not a quantisation.
//
// `a_offset` is what epaint's tessellator put between that point and this
// vertex — `normal * radius`, plus the end extrude — and it is in **screen
// points**, so the placement must not scale it. epaint reads the normal off
// the path's own points and the placement is a scale-and-translate with no
// rotation, which is what makes the offset the same number of points at every
// tile side and lets it be computed once at tile build. That is the whole of
// the difference from `vs_main`: one attribute, one add, after the placement
// rather than inside it.
@vertex
fn vs_stroke(
    @location(0) a_pos: vec2<i32>,
    @location(1) a_offset: vec2<f32>,
    @location(2) a_color: u32,
) -> VertexOutput {
    var out: VertexOutput;
    out.color = unpack_color(a_color);
    out.position = position_from_screen(
        r_locals.scale * vec2<f32>(a_pos) + r_locals.translation + a_offset
    );
    return out;
}

@fragment
fn fs_main_linear_framebuffer(in: VertexOutput) -> @location(0) vec4<f32> {
    var out_color_gamma = in.color;
    if r_locals.dithering == 1u {
        let out_color_gamma_rgb = dither_interleaved(out_color_gamma.rgb, 256.0, in.position);
        out_color_gamma = vec4<f32>(out_color_gamma_rgb, out_color_gamma.a);
    }
    let out_color_linear = linear_from_gamma_rgb(out_color_gamma.rgb);
    return vec4<f32>(out_color_linear, out_color_gamma.a);
}

@fragment
fn fs_main_gamma_framebuffer(in: VertexOutput) -> @location(0) vec4<f32> {
    var out_color_gamma = in.color;
    if r_locals.dithering == 1u {
        let out_color_gamma_rgb = dither_interleaved(out_color_gamma.rgb, 256.0, in.position);
        out_color_gamma = vec4<f32>(out_color_gamma_rgb, out_color_gamma.a);
    }
    return out_color_gamma;
}
