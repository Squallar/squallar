//! Proof that the GPU tests in this directory can actually fail.
//!
//! Every code review of the volume work has turned up a test that could not
//! fail, and every time a human found it by hand-editing the shader. The worst
//! of them: `volume.wgsl` reprojects the map floor using a cosine taken at the
//! pixel's own latitude, and swapping that for a cosine at the site's latitude
//! — the exact misregistration this repository already shipped once, worth
//! about 7.6 km of drift at the corners of a 460 km box — passed *every* test
//! in the tree. Three independent reasons, all fixture blindness: the
//! registration instrument is a CPU restatement that never runs the WGSL; every
//! GPU floor fixture is planted on the equator, where that error is
//! mathematically zero; and the one test that drives the shader at a real
//! latitude writes a PPM and asserts nothing. `cargo-mutants` cannot reach any
//! of it, because to Rust `volume.wgsl` is a string.
//!
//! This file is that string's mutation battery. wgpu compiles the shader at
//! *runtime* from a `&str`, so a mutant needs no rebuild: substitute text in
//! memory, build the real pipelines from the mutated source through
//! [`VolumePipelines::from_shader_source`], run a probe, and require the
//! probe's reading to move by more than that probe's own tolerance. A mutant
//! whose reading does not move is a shader property nothing in this repository
//! can see.
//!
//! ```text
//! cargo test -p rustdar-frontend --test volume_shader_mutants -- --ignored --nocapture
//! ```
//!
//! **An unmatched pattern is a hard failure.** A battery whose patterns have
//! stopped matching is a vacuously green test, which is the precise failure
//! class it exists to prevent — so every substitution asserts that its pattern
//! was found, that it was found exactly the declared number of times, and that
//! the resulting source differs from the original. None of those is a skip and
//! none is a warning. That check does not need a GPU and is therefore **not**
//! `#[ignore]`d: `every_mutant_still_matches_the_shader_it_mutates` runs in the
//! ordinary `cargo test`, so an edit to `volume.wgsl` that moves an anchor goes
//! red on a machine with no Vulkan loader at all. Loud breakage on a legitimate
//! shader edit is the design; re-anchor the pattern and, if the expression it
//! named is gone, delete the mutant deliberately rather than by attrition.
//!
//! Patterns are anchored on distinctive expressions, never on line numbers or
//! on whitespace runs, because this file moves under active development.
//!
//! Adding a mutant is one row of [`MUTANTS`]: a name, a class, the pattern, the
//! replacement, how many times the pattern occurs, and which probe should
//! notice. Adding a probe is one `fn` and one `static`.
//!
//! # What only this file sees
//!
//! Every row was also run the slow way, by patching `volume.wgsl` on disk and
//! running every suite that can observe it — `--lib`, `volume_shader`,
//! `volume_gpu`, `volume_silhouette`; nothing else in the workspace reads the
//! shader. **Ten of the thirty-one mutations left that whole set green.** They
//! are therefore shader properties this repository has no other instrument for,
//! and the probes below are the only thing standing on them.
//!
//! Coverage-premultiplied reconstruction — four of the ten, and the newest code
//! in the file:
//!
//!   * the coverage weight on the LIT VOLUME's optical depth. The isosurface's
//!     half of the feature is measured; the lit volume's is not, and it is the
//!     half that turns a silhouette's alpha from a step into a ramp.
//!   * `COVERAGE_FLOOR`'s *value*. That the surface excludes air is asserted;
//!     that it stops at the tent's **half** level set — the decision boundary
//!     the constant's doc is entirely about — is not, because no test moves the
//!     number and watches the silhouette follow.
//!   * the coverage weight on `iso_shading_field`, which is what stops a
//!     product whose diverging centre sits above its data lighting the surface
//!     from inside.
//!   * `shading_field` being the premultiplied channel rather than the
//!     reconstructed index. Identical everywhere except the echo edge — which
//!     is the entire surface being shaded.
//!
//! Shading, two more, and the reason is one fixture: the only
//! `gradient_shading` test renders a *uniform* grid, whose gradient is zero and
//! whose shading is therefore the 1.0 early exit.
//!
//!   * the sign of the normal — the difference between a lit cloud and one lit
//!     from inside; and
//!   * the displayed-kilometre metric the gradient is divided by, invisible
//!     unless an anisotropic box meets a gradient with more than one non-zero
//!     component, because a normal is invariant under isotropic rescaling.
//!
//! The opacity ramp, two: every shipped fixture leaves `edge_soft_width` at the
//! hard default, where the ramp is a step and neither its shape nor the foot it
//! rises from can matter.
//!
//! The map floor, two:
//!
//!   * the off-edge guard, because every floor fixture looks straight down,
//!     where each ray's plane crossing is inside the footprint by construction
//!     — and clamping instead of missing smears the boundary texel of the map
//!     across the whole ground beyond it;
//!   * a 1.65% scale error in the footprint — the shape of the misregistration
//!     above — because nothing measures the floor's position to better than the
//!     3 px the shipped seam test allows.
//!
//! Those ten are covered now, and they are equally the specification for the
//! shipped tests that should eventually carry them.
//!
//! Two further candidate mutations are deliberately **absent**, each because it
//! is genuinely equivalent rather than merely unobserved. Both are written out
//! at the point in [`MUTANTS`] where they would have gone, because a mutation
//! that provably cannot be seen is a fact about the shader worth keeping.
#![cfg(not(target_arch = "wasm32"))]

use egui_wgpu::wgpu;
use rustdar_egui::pane::OrbitCamera;
use rustdar_egui::volume_view::view_for;
use rustdar_frontend::constants::VOLUME_LUT_BYTES;
use rustdar_frontend::volume::raymarch::{VOLUME_SHADER_WGSL, VolumePipelines};
use rustdar_frontend::volume::uniform::{ISO_OFF, VolumeUniform};

mod gpu_harness;
use gpu_harness::{
    attachments, box_from_clip_down, centre, device, equatorial_box_km, equatorial_floor_lanes,
    eye_outside, gpu_lock, grey_ramp_lut, iso_uniform, opaque_white_lut, palette, planted_mirror,
    raymarch_once, raymarch_once_with_floor, slab_ramp,
};

/// The offscreen is format-independent; the blit is not, and the blit is not
/// under test here. One format for every pipeline the battery builds.
const SURFACE: wgpu::TextureFormat = wgpu::TextureFormat::Bgra8Unorm;

// ---------------------------------------------------------------------------
// The substitution
// ---------------------------------------------------------------------------

/// One text substitution in `volume.wgsl`, and the probe that must notice it.
struct Mutant {
    /// What the mutation does, in the shader's own vocabulary. Printed in the
    /// battery's table and in every failure.
    name: &'static str,
    /// Which of the classes in the module doc this belongs to. Only for the
    /// printed table, so a reader can see at a glance which parts of the shader
    /// are covered.
    class: &'static str,
    /// The distinctive expression to replace. Never a bare token and never
    /// whitespace-anchored: it has to survive a reflow and break loudly on a
    /// rewrite.
    pattern: &'static str,
    /// What to put in its place. Must be valid WGSL of the same type, or the
    /// pipeline build below fails its error scope and the battery says so.
    replacement: &'static str,
    /// How many times `pattern` occurs. Declared rather than derived, so an
    /// edit that duplicates or removes one occurrence is a failure rather than
    /// a quietly narrower mutation.
    occurrences: usize,
    /// The measurement that must move.
    probe: &'static Probe,
}

/// Why a substitution could not be applied. Always a hard failure.
fn mutate(source: &str, mutant: &Mutant) -> Result<String, String> {
    let found = source.matches(mutant.pattern).count();
    if found != mutant.occurrences {
        return Err(format!(
            "{}: the pattern was found {found} time(s) in volume.wgsl, not the \
             declared {}. The shader has moved under the battery. Re-anchor the \
             pattern on the expression it means, or — if that expression is \
             genuinely gone — delete the row deliberately. Pattern:\n    {}",
            mutant.name, mutant.occurrences, mutant.pattern,
        ));
    }
    let mutated = source.replace(mutant.pattern, mutant.replacement);
    if mutated == source {
        return Err(format!(
            "{}: the substitution changed nothing — the replacement is the \
             pattern. A no-op mutant is a test that cannot fail. Pattern:\n    {}",
            mutant.name, mutant.pattern,
        ));
    }
    Ok(mutated)
}

// ---------------------------------------------------------------------------
// The probes
// ---------------------------------------------------------------------------

/// A rendering reduced to a handful of numbers.
///
/// The reduction is what makes a mutant's effect assertable: `Vec<f64>` rather
/// than an image, so "the probe moved" is one `max` over a difference and the
/// failure message can print both readings in full.
type ProbeFn = fn(&wgpu::Device, &wgpu::Queue, &VolumePipelines) -> Vec<f64>;

struct Probe {
    /// Named after what it looks at, because the failure message is "nothing
    /// in the repository can see X" and the reader needs to know what X was
    /// measured with.
    name: &'static str,
    /// The largest per-element difference two readings may have and still count
    /// as the same picture. The renders are deterministic — the same shader
    /// gives bit-identical bytes — so this is not sampling noise: it is the
    /// bar for "materially different", set at a few 8-bit levels or a fraction
    /// of a pixel, so that a mutation whose whole effect is a rounding is
    /// correctly reported as invisible rather than counted as caught.
    tolerance: f64,
    run: ProbeFn,
}

/// The worst per-element difference between two readings of the same probe.
fn divergence(baseline: &[f64], mutated: &[f64]) -> f64 {
    assert_eq!(
        baseline.len(),
        mutated.len(),
        "a probe returned readings of two different shapes",
    );
    baseline
        .iter()
        .zip(mutated)
        .map(|(a, b)| (a - b).abs())
        .fold(0.0f64, f64::max)
}

/// One pixel as four 0-1 channels.
fn channels(px: [u8; 4]) -> Vec<f64> {
    px.iter().map(|c| f64::from(*c) / 255.0).collect()
}

/// The fraction of pixels the march painted anything into.
fn painted_fraction(pixels: &[[u8; 4]]) -> f64 {
    pixels.iter().filter(|px| px[3] > 0).count() as f64 / pixels.len() as f64
}

/// The mean of one channel over the whole image, 0-1.
fn channel_mean(pixels: &[[u8; 4]], channel: usize) -> f64 {
    let total: f64 = pixels.iter().map(|px| f64::from(px[channel])).sum();
    total / (pixels.len() as f64 * 255.0)
}

/// The centroid of the pixels `keep` accepts, in fractions of the image.
///
/// Fractions rather than pixels so that one probe's tolerance is one number:
/// a reading that mixes pixel coordinates with 0-1 channels would need two.
/// `(-1, -1)` for an empty selection, which is outside every real centroid and
/// therefore itself a detectable reading.
fn centroid_fraction(
    pixels: &[[u8; 4]],
    size: [u32; 2],
    keep: impl Fn([u8; 4]) -> bool,
) -> (f64, f64) {
    let (mut n, mut sx, mut sy) = (0usize, 0.0f64, 0.0f64);
    for (i, px) in pixels.iter().enumerate() {
        if keep(*px) {
            n += 1;
            sx += (i % size[0] as usize) as f64;
            sy += (i / size[0] as usize) as f64;
        }
    }
    if n == 0 {
        return (-1.0, -1.0);
    }
    (
        sx / n as f64 / f64::from(size[0]),
        sy / n as f64 / f64::from(size[1]),
    )
}

/// The lowest and highest grey level in the middle half of the image, and how
/// much of that region came back opaque.
///
/// Deliberately assertion-free, unlike `volume_gpu`'s `grey_span`: a mutant may
/// legitimately leave the surface translucent or absent, and that has to arrive
/// as a moved reading rather than as a panic from inside the instrument.
fn grey_statistics(pixels: &[[u8; 4]], size: [u32; 2]) -> Vec<f64> {
    let (mut lo, mut hi) = (u8::MAX, u8::MIN);
    let (mut opaque, mut total) = (0usize, 0usize);
    for y in size[1] / 4..size[1] * 3 / 4 {
        for x in size[0] / 4..size[0] * 3 / 4 {
            let px = pixels[(y * size[0] + x) as usize];
            total += 1;
            if px[3] == 255 {
                opaque += 1;
            }
            lo = lo.min(px[0]);
            hi = hi.max(px[0]);
        }
    }
    vec![
        f64::from(lo) / 255.0,
        f64::from(hi) / 255.0,
        opaque as f64 / total as f64,
    ]
}

/// A `10 x 10 x 10 km` box: cubic on purpose here, so a probe that moves is
/// reporting the mutation rather than the aspect ratio.
const CUBE_KM: [f32; 3] = [10.0, 10.0, 10.0];

/// A half-transparent uniform volume, read at the centre pixel.
///
/// Extinction is chosen to land alpha near 0.5, which is the whole point:
/// at alpha 1 the premultiply is the identity and every un-premultiply /
/// re-premultiply rule agrees, so a saturated fixture cannot see that half of
/// the colour pipeline at all. (That is exactly why the shipped
/// `a_uniform_grid_paints_its_palette_colour`, which saturates, does not.)
fn probe_translucent_volume(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    pipelines: &VolumePipelines,
) -> Vec<f64> {
    const INDEX: u8 = 200;
    let size = [64u32, 64];
    let cells = [8u32, 8, 8];
    let mut uniform = VolumeUniform::new(CUBE_KM, cells);
    uniform.box_from_clip = box_from_clip_down(2);
    uniform.eye_in_box = eye_outside(2);
    // 0.07 per km over a 10 km path: alpha 1 - exp(-0.7) = 0.503.
    uniform.extinction_per_km = 0.07;
    uniform.gradient_shading = false;
    let filled = vec![INDEX; (cells[0] * cells[1] * cells[2]) as usize];
    let lut = palette(INDEX, [200, 60, 30, 255]);
    let pixels = raymarch_once(
        device, queue, pipelines, cells, &filled, &lut, &uniform, size,
    );
    channels(centre(&pixels, size))
}

/// A volume sitting halfway up the opacity ramp.
///
/// Every shipped fixture leaves `edge_soft_width` at its hard default, where
/// the ramp reaches 1 within a millionth of an index step and is therefore
/// indistinguishable from no ramp at all. This one gives the ramp a real width
/// and plants the field in the middle of it.
fn probe_opacity_ramp(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    pipelines: &VolumePipelines,
) -> Vec<f64> {
    const INDEX: u8 = 150;
    let size = [64u32, 64];
    let cells = [8u32, 8, 8];
    let mut uniform = VolumeUniform::new(CUBE_KM, cells);
    uniform.box_from_clip = box_from_clip_down(2);
    uniform.eye_in_box = eye_outside(2);
    uniform.extinction_per_km = 0.3;
    uniform.gradient_shading = false;
    // The ramp runs from index 100 to index 200; the field sits at 150, so
    // smoothstep is exactly 0.5 and a ramp forced to 1 doubles the depth.
    uniform.empty_index_threshold = 100.0 / 255.0;
    uniform.edge_soft_width = 100.0 / 255.0;
    let filled = vec![INDEX; (cells[0] * cells[1] * cells[2]) as usize];
    let lut = palette(INDEX, [200, 60, 30, 255]);
    let pixels = raymarch_once(
        device, queue, pipelines, cells, &filled, &lut, &uniform, size,
    );
    channels(centre(&pixels, size))
}

/// A field graded **diagonally**, on an anisotropic box, under gradient
/// shading.
///
/// Two things this fixture has to get right, and neither is optional:
///
/// * A *uniform* grid has no gradient at all, so `shading` takes its 1.0 early
///   exit and every lighting mutation is invisible — which is exactly what the
///   `gradient_shading = true` arm of the shipped
///   `a_uniform_grid_paints_its_palette_colour` measures. So the index ramps.
/// * It ramps along **x and z together**, over a 240 x 240 x 20 km box. A
///   gradient with one non-zero component, or an isotropic cell, cannot see the
///   displayed-kilometre metric at all: `normal = -gradient / |gradient|`
///   is invariant under any isotropic rescaling, so dividing by `cell_km` or
///   not gives the identical normal. The pancake box and the diagonal ramp are
///   what put the 12:1 aspect ratio into the direction.
fn probe_lit_gradient(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    pipelines: &VolumePipelines,
) -> Vec<f64> {
    let size = [64u32, 64];
    let cells = [16u32, 16, 16];
    // A block with air all round it, so the render carries both an interior —
    // where coverage is 1 and the premultiplied channel equals the index — and
    // an echo edge, where they part company and only the premultiplied one has
    // a gradient pointing out of the data.
    let mut indices = vec![0u8; (cells[0] * cells[1] * cells[2]) as usize];
    for z in 3..13u32 {
        for y in 3..13u32 {
            for x in 3..13u32 {
                let ramp = 30 + 12 * (x + z);
                indices[((z * cells[1] + y) * cells[0] + x) as usize] = ramp as u8;
            }
        }
    }
    let mut uniform = VolumeUniform::new([240.0, 240.0, 20.0], cells);
    uniform.box_from_clip = box_from_clip_down(2);
    uniform.eye_in_box = eye_outside(2);
    // Enough to saturate over the 20 km vertical path, so the reading is the
    // near samples' shading rather than a blend with the background.
    uniform.extinction_per_km = 0.5;
    uniform.gradient_shading = true;
    let lut = opaque_white_lut();
    let pixels = raymarch_once(
        device, queue, pipelines, cells, &indices, &lut, &uniform, size,
    );
    let mut reading = channels(centre(&pixels, size));
    reading.push(channel_mean(&pixels, 0));
    reading.push(painted_fraction(&pixels));
    reading
}

/// An eye **inside** the box, looking down through an empty lower half at a
/// solid upper one.
///
/// The slab entry's clamp to zero is the only thing keeping the march from
/// starting behind the viewer. Every other fixture here puts the eye outside
/// the box, where the entry is positive and the clamp is dead code.
fn probe_eye_inside_the_box(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    pipelines: &VolumePipelines,
) -> Vec<f64> {
    let size = [64u32, 64];
    let cells = [16u32, 16, 16];
    // Solid above the eye, empty below it: looking down from the middle, the
    // correct picture is nothing at all, and a march that starts at the box's
    // top face instead of at the eye paints the slab it should not see.
    let mut indices = vec![0u8; (cells[0] * cells[1] * cells[2]) as usize];
    for z in cells[2] / 2..cells[2] {
        for y in 0..cells[1] {
            for x in 0..cells[0] {
                indices[((z * cells[1] + y) * cells[0] + x) as usize] = 255;
            }
        }
    }
    let mut uniform = VolumeUniform::new(CUBE_KM, cells);
    // The down-looking far plane, but the eye is at the box's centre rather
    // than outside its near face.
    uniform.box_from_clip = box_from_clip_down(2);
    uniform.eye_in_box = [0.5, 0.5, 0.5];
    uniform.extinction_per_km = 1000.0;
    uniform.gradient_shading = false;
    let lut = opaque_white_lut();
    let pixels = raymarch_once(
        device, queue, pipelines, cells, &indices, &lut, &uniform, size,
    );
    let mut reading = channels(centre(&pixels, size));
    reading.push(painted_fraction(&pixels));
    reading
}

/// Where the sequential isosurface landed, and how much it wobbles.
///
/// The grey ramp table under ambient-only light makes a pixel's grey level the
/// palette index the surface was found at, and the per-pixel jitter makes the
/// *spread* the tell for an unrefined hit.
fn probe_isosurface_level(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    pipelines: &VolumePipelines,
) -> Vec<f64> {
    let size = [64u32, 64];
    let (cells, indices) = slab_ramp(&[208, 152, 96, 40]);
    let mut uniform = iso_uniform(cells);
    uniform.iso_centre = ISO_OFF;
    uniform.iso_threshold = 113.0 / 255.0;
    let lut = grey_ramp_lut();
    let pixels = raymarch_once(
        device, queue, pipelines, cells, &indices, &lut, &uniform, size,
    );
    grey_statistics(&pixels, size)
}

/// Where the *diverging* isosurface landed: the level set of the deviation
/// from the centre index, which is a different surface from the index's own.
///
/// The near slab is **unmeasured air**. That is what puts `iso_hit_test`'s
/// coverage term on the picture: an air sample reconstructs to index 0, which
/// is as far from a diverging centre as any real inbound value, so without the
/// coverage floor the very first sample crosses and the surface shrink-wraps
/// the coverage cone — the stated reason the term is there, which no shipped
/// fixture reaches because every one of them fills its box.
fn probe_diverging_isosurface(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    pipelines: &VolumePipelines,
) -> Vec<f64> {
    const CENTRE: u8 = 128;
    let size = [64u32, 64];
    // The eye is above, so the ray meets slab 3 (air) first and slab 0 last.
    let (cells, indices) = slab_ramp(&[68, 88, 108, 0]);
    let mut uniform = iso_uniform(cells);
    uniform.iso_centre = f32::from(CENTRE) / 255.0;
    uniform.iso_threshold = 34.0 / 255.0;
    let lut = grey_ramp_lut();
    let pixels = raymarch_once(
        device, queue, pipelines, cells, &indices, &lut, &uniform, size,
    );
    grey_statistics(&pixels, size)
}

/// A red-north / blue-south floor under an empty box, read at top and bottom.
fn probe_floor_orientation(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    pipelines: &VolumePipelines,
) -> Vec<f64> {
    let size = [128u32, 128];
    let cells = [16u32, 16, 16];
    let side = 8usize;
    let mut rgba = Vec::with_capacity(side * side * 4);
    for row in 0..side {
        for _col in 0..side {
            if row < side / 2 {
                rgba.extend_from_slice(&[255, 0, 0, 255]);
            } else {
                rgba.extend_from_slice(&[0, 0, 255, 255]);
            }
        }
    }
    let floor = planted_mirror(device, queue, pipelines, [side as u32, side as u32], &rgba);

    let mut uniform = VolumeUniform::new(equatorial_box_km(), cells);
    uniform.box_from_clip = box_from_clip_down(2);
    uniform.eye_in_box = eye_outside(2);
    uniform.extinction_per_km = 1000.0;
    uniform.gradient_shading = false;
    uniform.map_floor = true;
    // The footprint over the whole mirror, north edge on row 0, established
    // through the reprojection rather than assumed of it — the same fixture
    // `volume_gpu.rs` drives, which is the point of sharing the harness.
    let (floor_uv, floor_geo) = equatorial_floor_lanes(floor.is_gamma_encoded());
    uniform.floor_uv = floor_uv;
    uniform.floor_geo = floor_geo;
    let empty = vec![0u8; (cells[0] * cells[1] * cells[2]) as usize];
    let lut = opaque_white_lut();
    let pixels = raymarch_once_with_floor(
        device, queue, pipelines, cells, &empty, &lut, &uniform, size, &floor,
    );
    let top = pixels[(size[1] / 4 * size[0] + size[0] / 2) as usize];
    let bottom = pixels[(3 * size[1] / 4 * size[0] + size[0] / 2) as usize];
    let mut reading = channels(top);
    reading.extend(channels(bottom));
    reading
}

/// Where one planted floor patch lands on screen, to a fraction of a pixel.
///
/// This is the registration seam as a *number*. The camera is 200 box-heights
/// up through the same far plane, which is orthographic to well under a pixel,
/// so the patch's centroid is a direct read of the shader's floor texture
/// mapping — and a mutation that scales the footprint by a percent or two
/// moves it by a couple of pixels, which is the shape of the misregistration
/// this repository shipped.
fn probe_floor_registration(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    pipelines: &VolumePipelines,
) -> Vec<f64> {
    let size = [128u32, 128];
    let cells = [32u32, 32, 32];
    // Off-centre on both axes and off the diagonal, so every flip, swap and
    // scale moves it.
    let (col_cell, row_cell) = (24u32, 20u32);
    let side = 64usize;
    let mut rgba = vec![0u8; side * side * 4];
    for px in rgba.chunks_exact_mut(4) {
        px[3] = 255;
    }
    let scale = side as u32 / cells[0];
    for row in (side as u32 - (row_cell + 1) * scale)..(side as u32 - row_cell * scale) {
        for col in (col_cell * scale)..((col_cell + 1) * scale) {
            let at = ((row * side as u32 + col) * 4) as usize;
            rgba[at..at + 4].copy_from_slice(&[255, 0, 0, 255]);
        }
    }
    let floor = planted_mirror(device, queue, pipelines, [side as u32, side as u32], &rgba);

    let mut uniform = VolumeUniform::new(equatorial_box_km(), cells);
    uniform.box_from_clip = box_from_clip_down(2);
    uniform.eye_in_box = [0.5, 0.5, 200.0];
    uniform.extinction_per_km = 1000.0;
    uniform.gradient_shading = false;
    uniform.map_floor = true;
    // The footprint over the whole mirror, north edge on row 0, established
    // through the reprojection rather than assumed of it — the same fixture
    // `volume_gpu.rs` drives, which is the point of sharing the harness.
    let (floor_uv, floor_geo) = equatorial_floor_lanes(floor.is_gamma_encoded());
    uniform.floor_uv = floor_uv;
    uniform.floor_geo = floor_geo;
    let empty = vec![0u8; (cells[0] * cells[1] * cells[2]) as usize];
    let lut = opaque_white_lut();
    let pixels = raymarch_once_with_floor(
        device, queue, pipelines, cells, &empty, &lut, &uniform, size, &floor,
    );
    let (cx, cy) = centroid_fraction(&pixels, size, |px| px[0] > 100 && px[2] < 100);
    vec![cx, cy, painted_fraction(&pixels)]
}

/// A shallow ray that leaves the box through its side and only meets the
/// bottom plane far outside the footprint.
///
/// The off-edge guard's only job. Looking straight down, every ray that hits
/// the box hits the floor inside the footprint too, so no shipped fixture can
/// distinguish "reject the hit" from "clamp to the edge texel" — and clamping
/// smears the boundary texel of the map across the whole ground beyond it.
///
/// Here the eye is beside the box at mid height and the rays fan downward, so
/// their plane crossings land at box x around -1: outside the unit square,
/// rejected, nothing painted. The fixture proves itself: if no ray reached the
/// plane at all, the clamping mutant would paint nothing either and the battery
/// would report it as invisible.
fn probe_grazing_ray_past_the_footprint(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    pipelines: &VolumePipelines,
) -> Vec<f64> {
    let size = [64u32, 64];
    let cells = [8u32, 8, 8];
    // An opaque blue floor: whatever the guard lets through is unmistakable
    // against an empty box's transparent black.
    let rgba: Vec<u8> = std::iter::repeat_n([0u8, 0, 255, 255], 64)
        .flatten()
        .collect();
    let floor = planted_mirror(device, queue, pipelines, [8, 8], &rgba);

    let mut uniform = VolumeUniform::new(CUBE_KM, cells);
    // Down the x axis: the eye is at x = 3 and the far plane at x = -1, so the
    // rays travel in -x while ndc.y tilts them in z.
    uniform.box_from_clip = box_from_clip_down(0);
    uniform.eye_in_box = eye_outside(0);
    uniform.extinction_per_km = 1000.0;
    uniform.gradient_shading = false;
    uniform.map_floor = true;
    let empty = vec![0u8; (cells[0] * cells[1] * cells[2]) as usize];
    let lut = opaque_white_lut();
    let pixels = raymarch_once_with_floor(
        device, queue, pipelines, cells, &empty, &lut, &uniform, size, &floor,
    );
    vec![painted_fraction(&pixels), channel_mean(&pixels, 2)]
}

/// An empty box seen from under its bottom plane, with the floor on.
///
/// The ground must have faded to nothing by this depth: a residual is the wall
/// the user reported.
fn probe_floor_from_below(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    pipelines: &VolumePipelines,
) -> Vec<f64> {
    let size = [64u32, 64];
    let cells = [8u32, 8, 8];
    let rgba: Vec<u8> = std::iter::repeat_n([255u8, 0, 0, 255], 64)
        .flatten()
        .collect();
    let floor = planted_mirror(device, queue, pipelines, [8, 8], &rgba);

    // The mirror of `box_from_clip_down(2)`: depth 1 unprojects one box above
    // the top face and the eye sits one box below the bottom.
    let mut up = [[0.0f32; 4]; 4];
    up[0][0] = 0.5;
    up[1][1] = 0.5;
    up[3][0] = 0.5;
    up[3][1] = 0.5;
    up[2][2] = 2.5;
    up[3][2] = -0.5;
    up[3][3] = 1.0;
    let mut uniform = VolumeUniform::new(CUBE_KM, cells);
    uniform.box_from_clip = up;
    uniform.eye_in_box = [0.5, 0.5, -1.0];
    uniform.extinction_per_km = 1000.0;
    uniform.gradient_shading = false;
    uniform.map_floor = true;
    let empty = vec![0u8; (cells[0] * cells[1] * cells[2]) as usize];
    let lut = opaque_white_lut();
    let pixels = raymarch_once_with_floor(
        device, queue, pipelines, cells, &empty, &lut, &uniform, size, &floor,
    );
    let mut reading = channels(centre(&pixels, size));
    reading.push(painted_fraction(&pixels));
    reading
}

/// How wide one voxel paints, at the raw field and at the cloud rung.
///
/// The reconstruction knob is the difference between the two numbers; the
/// march's step length is most of the first.
fn probe_lone_voxel_footprint(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    pipelines: &VolumePipelines,
) -> Vec<f64> {
    let size = [128u32, 128];
    let cells = [16u32, 16, 16];
    let mut indices = vec![0u8; (cells[0] * cells[1] * cells[2]) as usize];
    indices[((8 * cells[1] + 8) * cells[0] + 8) as usize] = 255;
    let lut = opaque_white_lut();

    let mut uniform = VolumeUniform::new(CUBE_KM, cells);
    uniform.box_from_clip = box_from_clip_down(2);
    uniform.eye_in_box = eye_outside(2);
    uniform.extinction_per_km = 1000.0;
    uniform.gradient_shading = false;

    let raw = painted_fraction(&raymarch_once(
        device, queue, pipelines, cells, &indices, &lut, &uniform, size,
    ));
    uniform.reconstruction_lod = rustdar_frontend::volume::bridge::CLOUD_RECONSTRUCTION_LOD;
    uniform.step_cells = rustdar_frontend::volume::bridge::CLOUD_STEP_CELLS;
    let cloud = painted_fraction(&raymarch_once(
        device, queue, pipelines, cells, &indices, &lut, &uniform, size,
    ));
    vec![raw, cloud]
}

/// A two-cell slab in a 64-cube, seen edge-on from above.
///
/// The march's step *ceiling* is nearly invisible on every other fixture, and
/// correctly so: when a span outruns the ceiling the `dt` floor stretches the
/// steps to cover it rather than truncating the volume, and for a constant
/// field the discretisation then telescopes out of the alpha exactly. What a
/// collapsed ceiling actually destroys is **resolution** — a feature thinner
/// than the stretched step is missed by whatever fraction of the jittered comb
/// steps over it. So the fixture is a thin feature on a fine grid: 64 cells
/// along the ray at one cell per step, against a slab two cells thick.
fn probe_thin_slab_resolution(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    pipelines: &VolumePipelines,
) -> Vec<f64> {
    let size = [64u32, 64];
    let cells = [64u32, 64, 64];
    let mut indices = vec![0u8; (cells[0] * cells[1] * cells[2]) as usize];
    for z in 31..33u32 {
        for y in 0..cells[1] {
            for x in 0..cells[0] {
                indices[((z * cells[1] + y) * cells[0] + x) as usize] = 255;
            }
        }
    }
    let mut uniform = VolumeUniform::new(CUBE_KM, cells);
    uniform.box_from_clip = box_from_clip_down(2);
    uniform.eye_in_box = eye_outside(2);
    uniform.extinction_per_km = 1000.0;
    uniform.gradient_shading = false;
    let lut = opaque_white_lut();
    let pixels = raymarch_once(
        device, queue, pipelines, cells, &indices, &lut, &uniform, size,
    );
    vec![painted_fraction(&pixels)]
}

/// How far an isosurface reaches into the reconstruction tent, and how the
/// surface it finds is lit there.
///
/// A solid block in air, rendered as an isosurface under ordinary ambient
/// light. Two things live on this picture and nowhere else:
///
/// * `COVERAGE_FLOOR` is where the surface stops. It is the *half level set* of
///   the trilinear coverage field on purpose — the decision boundary that gives
///   a smooth surface the reach of an honest nearest march — and moving it
///   shrinks or inflates the block by a fraction of a cell on every face, which
///   only a silhouette area can read.
/// * The isosurface's shading normal is the gradient of `iso_field x coverage`.
///   Inside the data coverage is 1 and the weight is invisible; at the boundary
///   — which is the entire surface — it is what makes the gradient point out of
///   the data at all. Read as colour, which means the light cannot be ambient
///   only: every other isosurface fixture here sets `ambient = 1` to turn grey
///   into an index readout, and that collapses the wrap term to exactly 1. It
///   also means the product must be **diverging**, with its centre above the
///   data — the ρHV shape the shader names. On a sequential ramp `iso_field`
///   falls outward on its own and an unweighted gradient happens to point the
///   right way; with the centre above the block, `iso_field` of air is the
///   largest value the function takes, so dropping the weight turns the normal
///   round and lights the surface from inside.
fn probe_isosurface_reach(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    pipelines: &VolumePipelines,
) -> Vec<f64> {
    let size = [128u32, 128];
    let cells = [32u32, 32, 32];
    let mut indices = vec![0u8; (cells[0] * cells[1] * cells[2]) as usize];
    for z in 10..22u32 {
        for y in 10..22u32 {
            for x in 10..22u32 {
                indices[((z * cells[1] + y) * cells[0] + x) as usize] = 60;
            }
        }
    }
    let mut uniform = VolumeUniform::new(CUBE_KM, cells);
    uniform.box_from_clip = box_from_clip_down(2);
    uniform.eye_in_box = eye_outside(2);
    // Not `iso_uniform`: its ambient of 1 is what makes a grey level readable
    // as an index, and it is also what would hide every lighting term here.
    // Diverging, centre above the block: |60 - 200| clears the threshold, and
    // so — by more — does |0 - 200|, which is the air the coverage weight has
    // to exclude from the normal.
    uniform.iso_centre = 200.0 / 255.0;
    uniform.iso_threshold = 100.0 / 255.0;
    let lut = opaque_white_lut();
    let pixels = raymarch_once(
        device, queue, pipelines, cells, &indices, &lut, &uniform, size,
    );
    vec![
        painted_fraction(&pixels),
        channel_mean(&pixels, 0),
        f64::from(centre(&pixels, size)[0]) / 255.0,
    ]
}

/// The KLOT NROT green-arc fixture, and the echo edge it lives on.
///
/// A block whose only data index is blue, in air, over a table whose low band
/// is opaque green — the stand-in for NROT's anticyclonic run, which really
/// does sit under its cyclonic data on the one index ramp. The whole claim of
/// the coverage-premultiplied reconstruction is that a fetch beside empty air
/// returns a coverage-weighted mean of the **covered** texels alone, so the
/// block may only ever paint its own blue: any green is a band the data never
/// occupied.
///
/// The reading carries the edge as well as the census, because that is where
/// every coverage term lives — inside the data coverage is exactly 1 and the
/// premultiplied channel, the divisor and the optical-depth weight are all
/// identities.
fn probe_coverage_boundary(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    pipelines: &VolumePipelines,
) -> Vec<f64> {
    const DATA: u8 = 147;
    let size = [128u32, 128];
    let cells = [8u32, 8, 8];
    // A 2x2x2 block in the middle of empty air, every face a no-data boundary.
    let mut indices = vec![0u8; (cells[0] * cells[1] * cells[2]) as usize];
    for z in 3..5u32 {
        for y in 3..5u32 {
            for x in 3..5u32 {
                indices[((z * cells[1] + y) * cells[0] + x) as usize] = DATA;
            }
        }
    }
    let mut lut = vec![0u8; VOLUME_LUT_BYTES];
    for entry in 1..=120usize {
        lut[entry * 4..entry * 4 + 4].copy_from_slice(&[0, 255, 0, 255]);
    }
    let at = usize::from(DATA) * 4;
    lut[at..at + 4].copy_from_slice(&[0, 0, 255, 255]);

    let mut uniform = VolumeUniform::new(CUBE_KM, cells);
    uniform.box_from_clip = box_from_clip_down(2);
    uniform.eye_in_box = eye_outside(2);
    // Not saturating: the edge's alpha has to stay readable, and it is the
    // coverage weight on the optical depth that puts it where it is.
    uniform.extinction_per_km = 0.3;
    uniform.gradient_shading = false;

    let pixels = raymarch_once(
        device, queue, pipelines, cells, &indices, &lut, &uniform, size,
    );
    let green = pixels
        .iter()
        .filter(|px| px[3] > 0 && px[1] > px[0] && px[1] > px[2])
        .count() as f64
        / pixels.len() as f64;
    let blue = pixels
        .iter()
        .filter(|px| px[3] > 0 && px[2] > px[0] && px[2] > px[1])
        .count() as f64
        / pixels.len() as f64;
    vec![
        green,
        blue,
        painted_fraction(&pixels),
        channel_mean(&pixels, 3),
    ]
}

/// A planted slab under a **real perspective camera**, as a silhouette.
///
/// Every other fixture in this directory drives an orthographic-by-hand
/// `box_from_clip` whose `w` row is the identity, so the perspective divide in
/// `unproject` is the identity too and cannot be seen. This one goes through
/// the application's own `view_for`, which is the only way a divide, a depth
/// row or a matrix convention can show up.
fn probe_perspective_silhouette(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    pipelines: &VolumePipelines,
) -> Vec<f64> {
    let size = [96u32, 96];
    let cells = [32u32, 32, 32];
    let box_km = [240.0f32, 240.0, 60.0];
    // Off-axis in yaw and pitch, so no symmetry can hide a moved ray.
    let camera = OrbitCamera::restore(37.0, 24.0, 2.2, [0.0; 3], 1.0).expect("a finite camera");
    let view = view_for(
        camera,
        box_km,
        f32::from(size[0] as u16) / f32::from(size[1] as u16),
    )
    .expect("the battery's camera must be viewable");

    // A slab in one corner of the footprint, full height: an off-centre,
    // asymmetric shape whose silhouette centroid is a strong position read.
    let mut indices = vec![0u8; (cells[0] * cells[1] * cells[2]) as usize];
    for z in 0..cells[2] {
        for y in 4..12u32 {
            for x in 18..28u32 {
                indices[((z * cells[1] + y) * cells[0] + x) as usize] = 255;
            }
        }
    }
    let lut = opaque_white_lut();

    let mut uniform = VolumeUniform::new(box_km, cells);
    uniform.box_from_clip = view.box_from_clip;
    uniform.eye_in_box = view.eye_in_box;
    uniform.extinction_per_km = 1000.0;
    uniform.gradient_shading = false;
    let pixels = raymarch_once(
        device, queue, pipelines, cells, &indices, &lut, &uniform, size,
    );
    let (cx, cy) = centroid_fraction(&pixels, size, |px| px[3] > 128);
    vec![painted_fraction(&pixels), cx, cy]
}

// Tolerances: 0.012 is three 8-bit levels; 0.008 of an image edge is about a
// pixel at these sizes. Renders are deterministic, so these are "materially
// different" bars rather than noise floors — see [`Probe::tolerance`].
static TRANSLUCENT_VOLUME: Probe = Probe {
    name: "a half-transparent uniform volume's centre pixel",
    tolerance: 0.012,
    run: probe_translucent_volume,
};
static OPACITY_RAMP: Probe = Probe {
    name: "a volume halfway up a real opacity ramp",
    tolerance: 0.012,
    run: probe_opacity_ramp,
};
static LIT_GRADIENT: Probe = Probe {
    name: "a diagonally graded field on a pancake box, under gradient shading",
    tolerance: 0.012,
    run: probe_lit_gradient,
};
static EYE_INSIDE_THE_BOX: Probe = Probe {
    name: "an eye at the box's centre looking down through empty air",
    tolerance: 0.012,
    run: probe_eye_inside_the_box,
};
static ISOSURFACE_LEVEL: Probe = Probe {
    name: "where the sequential isosurface landed, and its spread",
    tolerance: 0.012,
    run: probe_isosurface_level,
};
static DIVERGING_ISOSURFACE: Probe = Probe {
    name: "where the diverging isosurface landed",
    tolerance: 0.012,
    run: probe_diverging_isosurface,
};
static FLOOR_ORIENTATION: Probe = Probe {
    name: "a red-north/blue-south floor, top and bottom of the image",
    tolerance: 0.012,
    run: probe_floor_orientation,
};
static FLOOR_REGISTRATION: Probe = Probe {
    name: "the screen centroid of one planted floor patch",
    tolerance: 0.008,
    run: probe_floor_registration,
};
static GRAZING_RAY: Probe = Probe {
    name: "a shallow ray whose plane crossing is outside the footprint",
    tolerance: 0.012,
    run: probe_grazing_ray_past_the_footprint,
};
static FLOOR_FROM_BELOW: Probe = Probe {
    name: "an empty box seen from under its bottom plane",
    tolerance: 0.012,
    run: probe_floor_from_below,
};
static LONE_VOXEL: Probe = Probe {
    name: "one voxel's painted footprint, raw and at the cloud rung",
    tolerance: 0.004,
    run: probe_lone_voxel_footprint,
};
static ISOSURFACE_REACH: Probe = Probe {
    name: "how far an isosurface reaches into the tent, and how it is lit there",
    tolerance: 0.004,
    run: probe_isosurface_reach,
};
static COVERAGE_BOUNDARY: Probe = Probe {
    name: "the NROT green-arc census, and the echo edge it stands on",
    tolerance: 0.004,
    run: probe_coverage_boundary,
};
static THIN_SLAB_RESOLUTION: Probe = Probe {
    name: "how much of a two-cell slab in a 64-cube the march still resolves",
    tolerance: 0.004,
    run: probe_thin_slab_resolution,
};
static PERSPECTIVE_SILHOUETTE: Probe = Probe {
    name: "an off-centre slab's silhouette under a real perspective camera",
    tolerance: 0.008,
    run: probe_perspective_silhouette,
};

// ---------------------------------------------------------------------------
// The battery
// ---------------------------------------------------------------------------

/// Every mutation, and the probe that must see it.
///
/// One row per shader property. The classes are the ones the campaign has
/// actually watched regress in silence.
static MUTANTS: &[Mutant] = &[
    // --- reconstruction and sampling -------------------------------------
    Mutant {
        name: "lut_coord drops the table's divisor",
        class: "reconstruction",
        pattern: "(index * (LUT_ENTRIES - 1.0) + 0.5) / LUT_ENTRIES",
        replacement: "(index * (LUT_ENTRIES - 1.0) + 0.5)",
        occurrences: 1,
        probe: &TRANSLUCENT_VOLUME,
    },
    Mutant {
        name: "the march ignores the reconstruction level and always samples LOD 0",
        class: "reconstruction",
        pattern: "let texel = textureSampleLevel(grid_texture, grid_sampler, p, volume.flags.y).rg;",
        replacement: "let texel = textureSampleLevel(grid_texture, grid_sampler, p, 0.0).rg;",
        occurrences: 1,
        probe: &LONE_VOXEL,
    },
    Mutant {
        name: "the reconstruction drops the coverage divisor and returns the premultiplied index",
        class: "reconstruction",
        pattern: "return vec2<f32>(texel.r / max(texel.g, COVERAGE_EPSILON), texel.g);",
        replacement: "return vec2<f32>(texel.r, texel.g);",
        occurrences: 1,
        probe: &COVERAGE_BOUNDARY,
    },
    Mutant {
        name: "the reconstruction reports full coverage everywhere",
        class: "reconstruction",
        pattern: "return vec2<f32>(texel.r / max(texel.g, COVERAGE_EPSILON), texel.g);",
        replacement: "return vec2<f32>(texel.r / max(texel.g, COVERAGE_EPSILON), 1.0);",
        occurrences: 1,
        probe: &COVERAGE_BOUNDARY,
    },
    Mutant {
        name: "the lit volume's optical depth drops its coverage weight",
        class: "reconstruction",
        pattern: "1.0 - exp(-entry.a * opacity_ramp * coverage * volume.transfer.x * segment_km);",
        replacement: "1.0 - exp(-entry.a * opacity_ramp * volume.transfer.x * segment_km);",
        occurrences: 1,
        probe: &COVERAGE_BOUNDARY,
    },
    // The lit volume's coverage skip, raised to the isosurface's floor rather
    // than deleted. Deleting it outright is the more obvious mutation and is
    // **not portable**: what it admits is the reconstruction tent's outermost
    // tail, below one stored quantum of coverage, and whether that tail is a
    // small positive number or exactly zero is a property of the adapter's
    // filtering precision — it moved this probe by 0.028 on an RTX 3090 and by
    // 0.000 on lavapipe, which CI runs. Raising the constant to COVERAGE_FLOOR
    // is the confusion the shader's own comment warns against (the two numbers
    // mean opposite things: one is a fill-rate floor, the other a decision
    // boundary) and it erases whole features, which every adapter agrees about.
    Mutant {
        name: "the lit volume's coverage skip is raised to the isosurface's decision boundary",
        class: "reconstruction",
        pattern: "} else if coverage >= COVERAGE_SKIP && index > volume.transfer.y {",
        replacement: "} else if coverage >= 0.5 && index > volume.transfer.y {",
        occurrences: 1,
        probe: &LONE_VOXEL,
    },
    // --- the isosurface ---------------------------------------------------
    Mutant {
        name: "refine_iso_hit's bisection is deleted and it returns the far bound",
        class: "isosurface",
        pattern: "let mid = 0.5 * (lo + hi);",
        replacement: "let mid = hi;",
        occurrences: 1,
        probe: &ISOSURFACE_LEVEL,
    },
    Mutant {
        name: "iso_field's diverging fold is a no-op and returns the index",
        class: "isosurface",
        pattern: "return select(index, abs(index - volume.grid_dims.w), volume.grid_dims.w >= 0.0);",
        replacement: "return index;",
        occurrences: 1,
        probe: &DIVERGING_ISOSURFACE,
    },
    Mutant {
        name: "the isosurface threshold moves to half its value",
        class: "isosurface",
        pattern: "iso_field(sample.x) >= volume.eye_in_box.w",
        replacement: "iso_field(sample.x) >= volume.eye_in_box.w * 0.5",
        occurrences: 1,
        probe: &ISOSURFACE_LEVEL,
    },
    Mutant {
        name: "iso_hit_test drops the coverage floor, so unmeasured air crosses",
        class: "isosurface",
        pattern: "return sample.y >= COVERAGE_FLOOR && iso_field(sample.x)",
        replacement: "return iso_field(sample.x)",
        occurrences: 1,
        probe: &DIVERGING_ISOSURFACE,
    },
    Mutant {
        name: "the isosurface's coverage floor moves from the half level set to 0.9",
        class: "isosurface",
        pattern: "const COVERAGE_FLOOR: f32 = 0.5;",
        replacement: "const COVERAGE_FLOOR: f32 = 0.9;",
        occurrences: 1,
        probe: &ISOSURFACE_REACH,
    },
    Mutant {
        name: "the isosurface's shading normal drops its coverage weight",
        class: "isosurface",
        pattern: "return iso_field(sample.x) * sample.y;",
        replacement: "return iso_field(sample.x);",
        occurrences: 1,
        probe: &ISOSURFACE_REACH,
    },
    // --- the map floor ----------------------------------------------------
    Mutant {
        name: "the floor's off-edge guard clamps to the edge texel instead of missing",
        class: "floor",
        pattern: "if hit.x < 0.0 || hit.x > 1.0 || hit.y < 0.0 || hit.y > 1.0 {\n        return -1.0;\n    }\n",
        replacement: "",
        occurrences: 1,
        probe: &GRAZING_RAY,
    },
    Mutant {
        name: "the floor's v axis is not flipped, so the map is upside down",
        class: "floor",
        // Re-anchored when the floor became a Web Mercator reprojection of the
        // pane mirror: there is no longer a `(hit.x, 1 - hit.y)` lookup to
        // negate. The v flip now lives in the *sign* of the Mercator lane —
        // `floor_uv.w` is negative because v grows down the mirror while
        // Mercator y grows north — so flipping that sign mirrors the map about
        // the site's own row, which is the same upside-down picture the
        // original mutation produced.
        pattern: "volume.floor_uv.y + d_merc * volume.floor_uv.w,",
        replacement: "volume.floor_uv.y - d_merc * volume.floor_uv.w,",
        occurrences: 1,
        probe: &FLOOR_ORIENTATION,
    },
    Mutant {
        name: "the floor's footprint is scaled by 1.65% — the shipped misregistration",
        class: "floor",
        // Re-anchored with the row above. The scale is still taken on the uv
        // handed to the sampler, about the uv origin, exactly as it was when
        // that uv was built inline from `hit` — only the expression that
        // computes it has moved.
        pattern: "let sample = textureSampleLevel(floor_texture, floor_sampler, uv, 0.0);",
        replacement: "let sample = textureSampleLevel(floor_texture, floor_sampler, uv * 1.0165, 0.0);",
        occurrences: 1,
        probe: &FLOOR_REGISTRATION,
    },
    Mutant {
        name: "the below-floor fade is deleted and the ground is always at full coverage",
        class: "floor",
        pattern: "let floor_fade = clamp(1.0 + eye.z / FLOOR_BELOW_FADE, 0.0, 1.0);",
        replacement: "let floor_fade = 1.0;",
        occurrences: 1,
        probe: &FLOOR_FROM_BELOW,
    },
    // --- colour -----------------------------------------------------------
    Mutant {
        name: "the sRGB decode's upper segment is the identity",
        class: "colour",
        pattern: "let higher = pow((srgb + vec3<f32>(0.055)) / vec3<f32>(1.055), vec3<f32>(2.4));",
        replacement: "let higher = srgb;",
        occurrences: 1,
        probe: &TRANSLUCENT_VOLUME,
    },
    Mutant {
        name: "the sRGB encode's upper segment is the identity",
        class: "colour",
        pattern: "let higher = vec3<f32>(1.055) * pow(rgb, vec3<f32>(1.0 / 2.4)) - vec3<f32>(0.055);",
        replacement: "let higher = rgb;",
        occurrences: 1,
        probe: &TRANSLUCENT_VOLUME,
    },
    Mutant {
        name: "the final un-premultiply is dropped",
        class: "colour",
        pattern: "let straight_linear = accumulated / alpha;",
        replacement: "let straight_linear = accumulated;",
        occurrences: 1,
        probe: &TRANSLUCENT_VOLUME,
    },
    Mutant {
        name: "the final re-premultiply is dropped",
        class: "colour",
        pattern: "return vec4<f32>(gamma_from_linear_rgb(straight_linear) * alpha, alpha);",
        replacement: "return vec4<f32>(gamma_from_linear_rgb(straight_linear), alpha);",
        occurrences: 1,
        probe: &TRANSLUCENT_VOLUME,
    },
    // --- optical depth and shading ---------------------------------------
    Mutant {
        name: "step_length_km gives every direction the box's diagonal",
        class: "optical depth",
        pattern: "return length(rd * dt * volume.box_size_km.xyz);",
        replacement: "return dt * length(volume.box_size_km.xyz);",
        occurrences: 1,
        probe: &TRANSLUCENT_VOLUME,
    },
    Mutant {
        name: "the opacity ramp is always fully open",
        class: "optical depth",
        pattern: "let opacity_ramp = rise * rise * (3.0 - 2.0 * rise);",
        replacement: "let opacity_ramp = 1.0;",
        occurrences: 1,
        probe: &OPACITY_RAMP,
    },
    // NOT here, and deliberately: deleting the lit volume's empty-index skip
    // (`} else if index > volume.transfer.y {` -> `index >= 0.0`) renders a
    // bit-identical picture at every fixture tried, because the opacity ramp
    // immediately below it already evaluates to exactly 0 at and under
    // `transfer.y` — `rise` clamps to 0 there whatever `transfer.w` is. The
    // skip is a fill-rate optimisation, not a visual contract, and a mutant
    // that can only ever survive would make this file permanently red. What is
    // testable is where the ramp's own foot sits, which is the row below.
    Mutant {
        name: "the opacity ramp's foot sits at zero rather than at the skip threshold",
        class: "optical depth",
        pattern: "let rise = clamp((index - volume.transfer.y) / max(volume.transfer.w, 1e-6), 0.0, 1.0);",
        replacement: "let rise = clamp(index / max(volume.transfer.w, 1e-6), 0.0, 1.0);",
        occurrences: 1,
        probe: &OPACITY_RAMP,
    },
    Mutant {
        name: "the shading gradient is taken of the reconstructed index, not the premultiplied channel",
        class: "shading",
        pattern: "    return textureSampleLevel(grid_texture, grid_sampler, p, volume.flags.y).r;",
        replacement: "    return field_at(p).x;",
        occurrences: 1,
        probe: &LIT_GRADIENT,
    },
    Mutant {
        name: "the shading normal is not negated, so surfaces are lit from inside",
        class: "shading",
        pattern: "let normal = -gradient / magnitude;",
        replacement: "let normal = gradient / magnitude;",
        occurrences: 2,
        probe: &LIT_GRADIENT,
    },
    Mutant {
        name: "the gradient is taken in box units rather than displayed kilometres",
        class: "shading",
        pattern: "    ) / cell_km;",
        replacement: "    );",
        occurrences: 2,
        probe: &LIT_GRADIENT,
    },
    // --- geometry ---------------------------------------------------------
    Mutant {
        name: "unproject drops the perspective divide",
        class: "geometry",
        pattern: "return homogeneous.xyz / homogeneous.w;",
        replacement: "return homogeneous.xyz;",
        occurrences: 1,
        probe: &PERSPECTIVE_SILHOUETTE,
    },
    // Also not here: replacing `unproject`'s far-plane depth with the near
    // plane (`vec4<f32>(ndc, depth, 1.0)` -> `vec4<f32>(ndc, 0.0, 1.0)`)
    // renders bit-identically under every camera, and correctly so — near and
    // far unproject to two points on the same eye ray, and the march only uses
    // their difference's direction. That is a property of the derivation, not
    // a hole. What a probe can see is the transform's *inputs*:
    Mutant {
        name: "the box transform's ndc axes are swapped",
        class: "geometry",
        pattern: "volume.box_from_clip * vec4<f32>(ndc, depth, 1.0)",
        replacement: "volume.box_from_clip * vec4<f32>(ndc.yx, depth, 1.0)",
        occurrences: 1,
        probe: &PERSPECTIVE_SILHOUETTE,
    },
    Mutant {
        name: "the slab entry is not clamped to zero, so an eye inside marches from behind itself",
        class: "geometry",
        pattern: "let entry = max(max(near.x, near.y), max(near.z, 0.0));",
        replacement: "let entry = max(max(near.x, near.y), near.z);",
        occurrences: 1,
        probe: &EYE_INSIDE_THE_BOX,
    },
    Mutant {
        name: "the march step is four times as long",
        class: "geometry",
        pattern: "let dt = max(volume.flags.z / cells_per_t,",
        replacement: "let dt = max(4.0 * volume.flags.z / cells_per_t,",
        occurrences: 1,
        probe: &LONE_VOXEL,
    },
    Mutant {
        name: "the march's step ceiling collapses from 1024 to 8",
        class: "geometry",
        pattern: "const RAYMARCH_STEP_CEILING: i32 = 1024;",
        replacement: "const RAYMARCH_STEP_CEILING: i32 = 8;",
        occurrences: 1,
        probe: &THIN_SLAB_RESOLUTION,
    },
];

/// Every pattern in [`MUTANTS`] still names something in the shader, exactly as
/// often as declared, and every replacement really changes the source.
///
/// **Not `#[ignore]`d**, and that is the point: this is the check that stops
/// the battery going vacuously green, and it needs no adapter. A `volume.wgsl`
/// edit that moves an anchor fails here in the ordinary `cargo test`, on any
/// machine, before anybody looks at a GPU job.
///
/// ```text
/// cargo test -p rustdar-frontend --test volume_shader_mutants \
///     every_mutant_still_matches_the_shader_it_mutates -- --exact
/// ```
#[test]
fn every_mutant_still_matches_the_shader_it_mutates() {
    let mut broken = Vec::new();
    for mutant in MUTANTS {
        if let Err(why) = mutate(VOLUME_SHADER_WGSL, mutant) {
            broken.push(why);
        }
    }
    assert!(
        broken.is_empty(),
        "{} of {} mutation patterns no longer match volume.wgsl. Until they \
         do, the mutation battery is testing nothing:\n\n{}",
        broken.len(),
        MUTANTS.len(),
        broken.join("\n\n"),
    );
}

/// Two names are never the same, so a failure names one row.
#[test]
fn every_mutant_has_its_own_name() {
    let mut names: Vec<&str> = MUTANTS.iter().map(|m| m.name).collect();
    names.sort_unstable();
    let before = names.len();
    names.dedup();
    assert_eq!(
        before,
        names.len(),
        "two mutants share a name, so a failure would not say which row it was",
    );
}

/// The substitution machinery refuses a mutation that changes nothing.
///
/// The guard rail under everything else here: without it a pattern equal to its
/// replacement would render an identical picture, the probe would not move, and
/// the battery would report a live shader property as dead.
#[test]
fn a_substitution_that_changes_nothing_is_refused() {
    let no_op = Mutant {
        name: "a deliberate no-op",
        class: "self-test",
        pattern: "const ISO_REFINE_STEPS: i32 = 8;",
        replacement: "const ISO_REFINE_STEPS: i32 = 8;",
        occurrences: 1,
        probe: &ISOSURFACE_LEVEL,
    };
    let why = mutate(VOLUME_SHADER_WGSL, &no_op).expect_err("a no-op substitution must be refused");
    assert!(why.contains("changed nothing"), "unexpected reason: {why}");

    let absent = Mutant {
        name: "a pattern that is not there",
        class: "self-test",
        pattern: "let this_expression_is_not_in_the_shader = 1.0;",
        replacement: "let it_never_will_be = 1.0;",
        occurrences: 1,
        probe: &ISOSURFACE_LEVEL,
    };
    let why =
        mutate(VOLUME_SHADER_WGSL, &absent).expect_err("an unmatched pattern must be refused");
    assert!(why.contains("found 0 time(s)"), "unexpected reason: {why}");
}

/// Build the real pipelines from `wgsl`, failing loudly if the driver refuses
/// it.
///
/// `create_render_pipeline` returns no `Result`, so a mutation that produces
/// invalid WGSL would otherwise arrive as an unrelated garbage rendering — the
/// error scope turns it into a message naming the mutant.
fn pipelines_from(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    wgsl: &str,
    what: &str,
) -> VolumePipelines {
    let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
    let pipelines = VolumePipelines::from_shader_source(device, attachments(SURFACE), wgsl);
    pipelines.upload_quad(queue);
    let error = pollster::block_on(scope.pop());
    assert!(
        error.is_none(),
        "the shader for `{what}` did not compile, so this mutant tests nothing: {}",
        error.map(|e| e.to_string()).unwrap_or_default(),
    );
    pipelines
}

/// Every mutation of `volume.wgsl` changes what a probe measures.
///
/// The whole battery in one test, because the expensive part is the device and
/// the baselines, and because the useful output is the *table*: every row's
/// divergence is printed whether it passed or not, and a run that fails names
/// every dead mutant at once rather than the first.
///
/// A surviving mutant is not a bug in this file. It is a shader property no
/// probe here can observe — a live, untested expression — and the fix is a new
/// probe, not a deleted row.
///
/// ```text
/// cargo test -p rustdar-frontend --test volume_shader_mutants -- --ignored --nocapture
/// ```
#[test]
#[ignore = "needs a real wgpu adapter; see the doc comment for the invocation"]
fn every_mutant_moves_the_probe_that_should_see_it() {
    let _serialised = gpu_lock();
    let started = std::time::Instant::now();
    let (device, queue) = device();

    // The unmutated source through the same seam the mutants use, so a
    // difference below is the mutation and never the construction path.
    let reference = pipelines_from(&device, &queue, VOLUME_SHADER_WGSL, "the unmutated shader");

    let mut baselines: Vec<(&'static str, Vec<f64>)> = Vec::new();
    for mutant in MUTANTS {
        if !baselines.iter().any(|(name, _)| *name == mutant.probe.name) {
            let reading = (mutant.probe.run)(&device, &queue, &reference);
            baselines.push((mutant.probe.name, reading));
        }
    }
    let baseline_of = |probe: &Probe| -> Vec<f64> {
        baselines
            .iter()
            .find(|(name, _)| *name == probe.name)
            .map(|(_, reading)| reading.clone())
            .expect("every probe used by a mutant was measured above")
    };
    drop(reference);

    let mut survivors = Vec::new();
    println!(
        "\n{:<14}  {:<74}  {:>9}  {:>9}",
        "class", "mutant", "delta", "bar"
    );
    for mutant in MUTANTS {
        let mutated = match mutate(VOLUME_SHADER_WGSL, mutant) {
            Ok(source) => source,
            Err(why) => {
                survivors.push(why);
                continue;
            }
        };
        let pipelines = pipelines_from(&device, &queue, &mutated, mutant.name);
        let baseline = baseline_of(mutant.probe);
        let reading = (mutant.probe.run)(&device, &queue, &pipelines);
        let delta = divergence(&baseline, &reading);
        println!(
            "{:<14}  {:<74}  {delta:>9.5}  {:>9.5}{}",
            mutant.class,
            mutant.name,
            mutant.probe.tolerance,
            if delta > mutant.probe.tolerance {
                ""
            } else {
                "   <-- SURVIVED"
            },
        );
        if delta <= mutant.probe.tolerance {
            survivors.push(format!(
                "{}\n    probe:    {}\n    baseline: {baseline:?}\n    mutated:  {reading:?}\n    \
                 the worst element moved by {delta:.6}, under the probe's bar of {:.6}. \
                 Either this shader property is genuinely unobservable — a live, untested \
                 expression, which is a finding and not a reason to delete the row — or the \
                 probe's fixture does not reach it.",
                mutant.name, mutant.probe.name, mutant.probe.tolerance,
            ));
        }
    }

    println!(
        "\n{} mutants over {} probes in {:.2} s\n",
        MUTANTS.len(),
        baselines.len(),
        started.elapsed().as_secs_f64(),
    );
    assert!(
        survivors.is_empty(),
        "{} of {} shader mutations changed nothing any probe here can see:\n\n{}",
        survivors.len(),
        MUTANTS.len(),
        survivors.join("\n\n"),
    );
}
