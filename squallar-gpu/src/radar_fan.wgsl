// One radar sweep drawn as a fan of per-radial sectors, coloured by a lookup
// into a 256-entry table rather than by a rasterised RGBA image.
//
// The vertex stage places a canonical disk mesh — SECTORS wedges x RINGS range
// segments, built once and never rebuilt — into the pane by evaluating the
// great-circle destination and the Web Mercator projection per vertex. The
// fragment stage recovers its own ground range from the interpolated
// normalized-Mercator offset, turns that into a gate index, reads one byte out
// of the code plane and one RGBA entry out of the LUT.
//
// **No samplers.** Every fetch is `textureLoad` at an explicit level: nearest
// at level 0 by construction, no `textureNumLevels` (unreachable on WebGL2),
// and no implicit LOD to go non-uniform under the fragment's early returns.
//
// **No geodesy literal.** Both radii — the sphere the ground range is measured
// on and the 4/3 effective radius the beam is bent by — arrive in the uniforms.
// `squallar-radar/tests/geodesy_one_definition.rs` scans every `.wgsl` in the
// workspace for a number in the 6300-6400 band, and this file therefore needs
// no entry in its allow-list.

const TWO_PI: f32 = 6.283185307179586;
const INV_TWO_PI: f32 = 0.15915494309189535;

// The vertex attribute's bit fields. Kept in step with `radar_fan.rs`'s
// `pack_vertex`, which is where the packing is defined and tested.
const SECTOR_MASK: u32 = 2047u;
const RING_SHIFT: u32 = 11u;
const RING_MASK: u32 = 31u;
const SIDE_SHIFT: u32 = 16u;

// Range segments per sector. A silhouette parameter only: the fragment solves
// for its own ground range, so the value below changes how round the arcs look
// and nothing about which gate a pixel reads.
const RINGS: f32 = 16.0;

struct Locals {
    /// The site's position inside the callback's viewport, in PIXELS from its
    /// top-left corner.
    site_px: vec2<f32>,
    /// The callback's viewport, in pixels — what egui set before `paint`, and
    /// therefore what clip space maps onto.
    viewport_px: vec2<f32>,
    /// Pixels the whole world spans at this zoom (walkers' `total_pixels`).
    world_px: f32,
    /// Paint-time layer opacity, 0-1, applied the way `tile_mesh` applies it:
    /// at the shader, because a `Shape::Callback` is the one shape
    /// `Painter::add` cannot tint.
    opacity: f32,
    /// Ground kilometres one screen pixel covers, for the mip selection.
    km_per_px: f32,
    /// The site's latitude, as its sine and cosine and its Mercator y —
    /// computed on the CPU in f64 and handed over already reduced, because
    /// every use below is a difference against it.
    sin_lat0: f32,
    cos_lat0: f32,
    merc_y_site: f32,
    /// The sphere ground range is measured on, and its reciprocal. Both are
    /// given rather than one derived, so neither stage pays a divide.
    earth_radius_km: f32,
    inv_earth_radius_km: f32,
}

@group(0) @binding(0) var<uniform> r_locals: Locals;

struct Sweep {
    /// Each radial's DRAWN azimuth span in radians, two radials to a `vec4`:
    /// element `k` is `(lo, hi)` of radial `2k` then `(lo, hi)` of `2k + 1`.
    /// A sector past the sweep's own radial count has `lo == hi` and its two
    /// triangles are degenerate, which is how one canonical mesh serves every
    /// sweep shape.
    edges: array<vec4<f32>, 720>,
    /// The GROUND range of the mesh's innermost and outermost rings, km.
    first_gate_km: f32,
    reach_km: f32,
    /// Gate 0's centre and one gate's depth, both ALONG THE BEAM, km — the two
    /// numbers `PolarGeometry::gate_at` divides by.
    first_gate_slant_km: f32,
    gate_interval_slant_km: f32,
    /// The tilt's elevation in radians, meaningful only when `has_elevation`
    /// is 1. Zero otherwise, never a NaN: a uniform lane carrying a NaN is a
    /// value every arithmetic path has to be checked against.
    elev_rad: f32,
    /// The 4/3 effective earth radius the beam bends over, and its reciprocal.
    /// A different sphere from `Locals::earth_radius_km` and deliberately a
    /// separate lane.
    re_eff_km: f32,
    inv_re_eff_km: f32,
    /// One gate's GROUND depth, km — the mip selection's denominator, and not
    /// the same number as `gate_interval_slant_km`.
    gate_interval_km: f32,
    has_elevation: u32,
    /// Radials this sweep really carries. Sectors at or past it draw nothing.
    radials: u32,
    /// Gates the sweep reached, which bounds the gate index.
    reach_gates: u32,
    /// Levels in the code plane's chain, counting level 0. `1` clamps the LOD
    /// to level 0 with no branch — which is what a categorical plane wants.
    mip_levels: u32,
}

@group(1) @binding(0) var t_codes: texture_2d<u32>;
@group(1) @binding(1) var t_lut: texture_2d<f32>;
@group(1) @binding(2) var<uniform> r_sweep: Sweep;

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    /// This fragment's offset from the site in NORMALIZED Web Mercator, one
    /// unit to the whole world. It interpolates exactly across a triangle
    /// because `walkers::Projector::project` is affine from this frame to
    /// screen pixels, which is what lets the fragment solve for its own ground
    /// range instead of being handed an interpolated one.
    @location(0) rel: vec2<f32>,
    @location(1) @interpolate(flat) radial: u32,
}

@vertex
fn vs_main(@location(0) packed: u32) -> VertexOutput {
    let sector = packed & SECTOR_MASK;
    let ring = (packed >> RING_SHIFT) & RING_MASK;
    let side = (packed >> SIDE_SHIFT) & 1u;

    let pair = r_sweep.edges[sector >> 1u];
    let lo_hi = select(pair.xy, pair.zw, (sector & 1u) == 1u);
    let az = select(lo_hi.x, lo_hi.y, side == 1u);

    let t = f32(ring) / RINGS;
    let ground_km = mix(r_sweep.first_gate_km, r_sweep.reach_km, t);

    // The great-circle destination, term for term as
    // `squallar_geo::great_circle_destination` and `MercatorProjection::pixel_at`
    // spell it — with one change of form, and only one.
    let d = ground_km * r_locals.inv_earth_radius_km;
    let sin_d = sin(d);
    let cos_d = cos(d);
    let sin_az = sin(az);
    let cos_az = cos(az);

    // The change: `cos_d - 1` is formed as `-2 sin^2(d/2)` and the latitude is
    // carried as a DIFFERENCE from the site's. Forming `merc_y - merc_y_site`
    // instead would subtract two O(1) f32 numbers and lose ~1e-7 absolute,
    // which is pixels at the zooms this draws at.
    let half_d = sin(0.5 * d);
    let cos_d_minus_one = -2.0 * half_d * half_d;
    let d_sin_lat = r_locals.sin_lat0 * cos_d_minus_one + r_locals.cos_lat0 * sin_d * cos_az;
    let sin_lat = clamp(r_locals.sin_lat0 + d_sin_lat, -1.0, 1.0);

    let dlon = atan2(sin_az * sin_d * r_locals.cos_lat0, cos_d - r_locals.sin_lat0 * sin_lat);
    // atanh(a) - atanh(b) == atanh((a - b) / (1 - a*b)), and every term on the
    // right is already a difference.
    let d_merc_y = atanh(clamp(d_sin_lat / (1.0 - r_locals.sin_lat0 * sin_lat), -0.999999, 0.999999));

    let rel = vec2<f32>(dlon * INV_TWO_PI, -d_merc_y * INV_TWO_PI);
    let screen_px = r_locals.site_px + r_locals.world_px * rel;

    var out: VertexOutput;
    out.rel = rel;
    out.radial = sector;
    out.position = vec4<f32>(
        screen_px / r_locals.viewport_px * vec2<f32>(2.0, -2.0) + vec2<f32>(-1.0, 1.0),
        0.0,
        1.0,
    );
    return out;
}

/// The gate this fragment sits over, or a negative number where it sits over
/// none. The whole of the picture's agreement with the hover readout is here:
/// it is `PolarGeometry::gate_at`'s expression, on a ground range this stage
/// solves for rather than interpolates.
fn gate_at(rel: vec2<f32>) -> i32 {
    // Invert the vertex stage's Mercator step. `tanh(d_merc_y)` is the same
    // quantity the vertex fed to `atanh`, so this is exact rather than an
    // approximation of it, and it stays a small number throughout.
    let t = tanh(-TWO_PI * rel.y);
    let dlon = TWO_PI * rel.x;
    let s0 = r_locals.sin_lat0;
    let c0 = r_locals.cos_lat0;

    // sin_lat = (s0 + t) / (1 + s0*t) by the tanh addition formula, so every
    // quantity below has a cancellation-free closed form in `t`. Spelling the
    // haversine over `sin_lat` directly instead would subtract two latitudes
    // that agree to six digits a few kilometres from the site.
    let denom = 1.0 + s0 * t;
    let root = sqrt(max(1.0 - t * t, 0.0));
    let cos_lat = c0 * root / denom;
    // sin^2(dlat/2) == c0^2 * t^2 / (2 * (1 + root) * denom).
    let half_lat_sq = (c0 * c0 * t * t) / (2.0 * (1.0 + root) * denom);

    let half_dlon = sin(0.5 * dlon);
    let a = clamp(half_lat_sq + c0 * cos_lat * half_dlon * half_dlon, 0.0, 1.0);
    let ground_km = 2.0 * atan2(sqrt(a), sqrt(1.0 - a)) * r_locals.earth_radius_km;

    // `beam::slant_range_for_ground_km`: RE_EFF * sin(theta) / cos(elev + theta).
    // The `has_elevation` arm is the sweep whose two range numbers are ALREADY
    // ground ranges, which must not be converted at all.
    var along_beam_km = ground_km;
    if r_sweep.has_elevation == 1u {
        let theta = ground_km * r_sweep.inv_re_eff_km;
        along_beam_km = r_sweep.re_eff_km * sin(theta) / cos(r_sweep.elev_rad + theta);
    }

    let g = floor(
        (along_beam_km - r_sweep.first_gate_slant_km) / r_sweep.gate_interval_slant_km + 0.5,
    );
    if g < 0.0 || g >= f32(r_sweep.reach_gates) {
        return -1;
    }
    return i32(g);
}

/// The premultiplied colour this fragment carries, in GAMMA space — what egui's
/// own shader calls `out_color_gamma`, so the two entry points below differ
/// from egui's by nothing but the source of the colour.
fn shade(in: VertexOutput) -> vec4<f32> {
    // **`in.radial` is in bounds by construction and is not re-checked here.**
    // The canonical mesh carries a sector per radial the producer may declare;
    // a sweep with fewer leaves the surplus sectors' drawn edges equal, so
    // their triangles are degenerate and no fragment of one is ever raised.
    // That single mechanism is what keeps the picture right AND what keeps this
    // `textureLoad` inside the plane's `radials` rows, and
    // `sectors_past_the_sweeps_radials_draw_nothing` is its gate.
    //
    // A second `radial >= radials` guard stood here and was removed: it made
    // the two defences cover for each other, so breaking either one on its own
    // left the picture correct and nothing could see the loss.
    let gate = gate_at(in.rel);
    if gate < 0 {
        return vec4<f32>(0.0);
    }

    // One level of the chain, chosen by how much ground a pixel covers. At
    // `mip_levels == 1` the clamp pins level 0 whatever `km_per_px` is.
    let ratio = max(r_locals.km_per_px / r_sweep.gate_interval_km, 1.0);
    let lod = i32(clamp(floor(log2(ratio)), 0.0, f32(r_sweep.mip_levels - 1u)));
    // **The index is clamped to the level, and the clamp is load-bearing on
    // every odd extent.** A mip level is `max(1, extent >> lod)` wide, so on an
    // odd parent the last gate's `gate >> lod` lands one past the last texel —
    // 1831 >> 4 is 114 where level 4 of an 1832-gate plane holds 0..113. The
    // producer's own chain reduces that odd remainder INTO the last cell
    // (`CodePlane::build_chain`), so clamping reads the footprint the gate
    // really belongs to; without it `textureLoad` answers zero out of bounds
    // and the outermost gates vanish into unpainted as soon as a fragment
    // covers more than one gate.
    let dim = vec2<i32>(textureDimensions(t_codes, u32(lod)));
    let at = min(
        vec2<i32>(gate >> u32(lod), i32(in.radial) >> u32(lod)),
        dim - vec2<i32>(1, 1),
    );
    let code = textureLoad(t_codes, at, lod).r;

    // The table is STRAIGHT alpha (`Lut::to_rgba_bytes`), so the premultiply
    // happens here and exactly once. An entry at alpha zero contributes
    // nothing under egui's blend, which is why a below-threshold gate needs
    // neither a discard nor a branch.
    let entry = textureLoad(t_lut, vec2<i32>(i32(code), 0), 0);
    let alpha = entry.a * r_locals.opacity;
    return vec4<f32>(entry.rgb * alpha, alpha);
}

// The two gamma conventions, copied from `egui-wgpu`'s own shader through
// `tile_mesh.wgsl`: egui picks its fragment entry point off the target's
// sRGB-ness and this pass draws into the same render pass, so it must pick the
// same one or every radar pixel is gamma-shifted against the map under it.
//
// Neither dithers. egui's dither exists for its gradients; a fan's colour is a
// palette entry read whole out of a table, and there is no ramp between two
// entries for a banding artefact to appear in.

@fragment
fn fs_main_linear_framebuffer(in: VertexOutput) -> @location(0) vec4<f32> {
    let gamma = shade(in);
    let cutoff = gamma.rgb < vec3<f32>(0.04045);
    let lower = gamma.rgb / vec3<f32>(12.92);
    let higher = pow((gamma.rgb + vec3<f32>(0.055)) / vec3<f32>(1.055), vec3<f32>(2.4));
    return vec4<f32>(select(higher, lower, cutoff), gamma.a);
}

@fragment
fn fs_main_gamma_framebuffer(in: VertexOutput) -> @location(0) vec4<f32> {
    return shade(in);
}
