//! Is the raymarch's silhouette the one the geometry says it should be?
//!
//! Oracle-free: a palette transparent at index 0 and opaque elsewhere, with an
//! extinction large enough that one sample saturates, turns the render into a
//! binary mask; the same `box_from_clip` and `eye_in_box` the shader was handed
//! then cast one host ray per pixel and intersect the planted geometry
//! analytically. `#[ignore]`d (CI's `gpu` job opts in on lavapipe), and the
//! tests hold a process-wide lock: several devices on one adapter each blocking
//! in `poll(wait_indefinitely)` deadlock on this hardware.
//!
//! ```text
//! cargo test -p rustdar-gpu --test volume_silhouette -- --ignored --nocapture
//! ```
//!
//! Metrics: IoU of the two masks; symmetric difference; **max boundary
//! displacement**, the largest Chebyshev distance from a disagreeing pixel to
//! the analytic mask's boundary (insensitive to edge length); **centroid
//! offset**, the position check an area-only test cannot make.
//!
//! Two systematic residuals, measured rather than corrected:
//!
//! 1. **Linear-filter bleed.** The grid is `Rg16Float` sampled `Linear` and
//!    coverage-premultiplied, so the reach is set by interpolated *coverage*,
//!    not index: a cell contributes where coverage reaches `COVERAGE_SKIP` =
//!    1/255, and that tent runs from 1 at the outermost filled cell's centre to
//!    0 at its empty neighbour's, so the field extends nearly a full cell past
//!    that centre. Reach no longer depends on the stored value — an index-1 and
//!    an index-255 sphere paint bit-identically. Every comparison is reported
//!    twice: against the planted surface, and against it dilated one cell.
//! 2. **The jittered voxel-locked march.** `dt` is [`RAYMARCH_STEP_CELLS`] cells
//!    (floored so [`RAYMARCH_STEP_CEILING`] steps span the box) and the comb
//!    starts a per-pixel hash fraction past the entry, so a chord shorter than
//!    one step is hit or missed by that pixel's own jitter — at a tangent, a
//!    one-pixel ring of coin flips, measured at 0.19% of a coarse slab's mask.
#![cfg(not(target_arch = "wasm32"))]

use egui_wgpu::wgpu;
use rustdar_device_profile::constants::VOLUME_LUT_BYTES;
use rustdar_egui::pane::OrbitCamera;
use rustdar_egui::volume_view::view_for;
use rustdar_gpu::egui_renderer::AttachmentConfig;
use rustdar_volumetric::raymarch::staging::{STAGING_RING_FEATURE, VolumeStaging};
use rustdar_volumetric::raymarch::{RAYMARCH_STEP_CEILING, RAYMARCH_STEP_CELLS, VolumePipelines};
use rustdar_volumetric::uniform::VolumeUniform;

// ---------------------------------------------------------------------------
// GPU plumbing, lifted from tests/volume_gpu.rs
// ---------------------------------------------------------------------------

/// Held for the length of a test, so only one talks to the GPU at a time.
static ONE_AT_A_TIME: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Take the GPU lock, ignoring poisoning — an earlier failure reports itself.
fn gpu_lock() -> std::sync::MutexGuard<'static, ()> {
    ONE_AT_A_TIME
        .lock()
        .unwrap_or_else(|held| held.into_inner())
}

/// Name the adapter these tests actually got, once per process.
fn announce(adapter: &wgpu::Adapter) {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let info = adapter.get_info();
        eprintln!(
            "wgpu adapter: {:?} {:?} \"{}\" (driver: {} {})",
            info.backend, info.device_type, info.name, info.driver, info.driver_info
        );
    });
}

/// A device on whatever adapter `WGPU_BACKEND` selects.
fn device() -> (wgpu::Device, wgpu::Queue) {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::default(),
        force_fallback_adapter: false,
        compatible_surface: None,
    }))
    .expect("no wgpu adapter; these tests are ignored by default for that reason");
    announce(&adapter);
    pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("rustdar.volume.silhouette.device"),
        // The one feature production asks for, on the same terms — see
        // `gpu_harness::device`.
        required_features: adapter.features() & STAGING_RING_FEATURE,
        required_limits: adapter.limits(),
        memory_hints: Default::default(),
        experimental_features: Default::default(),
        trace: Default::default(),
    }))
    .expect("could not create a device on an adapter that was found")
}

/// The egui pass a blit would be composited into. Only the raymarch is used
/// here, and it renders into `OFFSCREEN_FORMAT` regardless, but the pipelines
/// need one.
fn attachments() -> AttachmentConfig {
    AttachmentConfig {
        color_format: wgpu::TextureFormat::Bgra8Unorm,
        depth_format: None,
        msaa_samples: 1,
    }
}

/// Read an RGBA8 texture back as one `[u8; 4]` per texel, row-major, row 0 at
/// the top of the render target.
fn read_back(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    texture: &wgpu::Texture,
    size: [u32; 2],
) -> Vec<[u8; 4]> {
    let unpadded = size[0] * 4;
    let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let padded = unpadded.div_ceil(align) * align;

    let staging = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("rustdar.volume.silhouette.readback"),
        size: u64::from(padded) * u64::from(size[1]),
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });

    let mut encoder = device.create_command_encoder(&Default::default());
    encoder.copy_texture_to_buffer(
        texture.as_image_copy(),
        wgpu::TexelCopyBufferInfo {
            buffer: &staging,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(padded),
                rows_per_image: Some(size[1]),
            },
        },
        wgpu::Extent3d {
            width: size[0],
            height: size[1],
            depth_or_array_layers: 1,
        },
    );
    queue.submit(Some(encoder.finish()));

    staging.slice(..).map_async(wgpu::MapMode::Read, |result| {
        result.expect("mapping the readback buffer failed");
    });
    device
        .poll(wgpu::PollType::wait_indefinitely())
        .expect("polling the device failed");

    let mapped = staging.slice(..).get_mapped_range();
    let mut pixels = Vec::with_capacity((size[0] * size[1]) as usize);
    for row in 0..size[1] as usize {
        let start = row * padded as usize;
        for column in 0..size[0] as usize {
            let at = start + column * 4;
            pixels.push(<[u8; 4]>::try_from(&mapped[at..at + 4]).expect("four bytes per texel"));
        }
    }
    pixels
}

/// Render one raymarched frame and read it back.
#[allow(clippy::too_many_arguments)]
fn raymarch_once(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    pipelines: &VolumePipelines,
    cells: [u32; 3],
    indices: &[u8],
    lut: &[u8],
    uniform: &VolumeUniform,
    size: [u32; 2],
) -> Vec<[u8; 4]> {
    let volume = pipelines
        .upload_volume(
            device,
            queue,
            cells,
            indices,
            lut,
            &mut VolumeStaging::new(device),
        )
        .expect("the grid and palette were refused");
    volume.write_uniform(queue, uniform);
    let target = pipelines.create_offscreen(device, size);
    let mut encoder = device.create_command_encoder(&Default::default());
    pipelines.encode_raymarch(&mut encoder, &target, &volume);
    queue.submit(Some(encoder.finish()));
    read_back(device, queue, target.texture(), size)
}

/// A `box_from_clip` that unprojects the far plane onto the far face of the
/// box, looking down one axis. From `tests/volume_gpu.rs`.
fn box_from_clip_down(axis: usize) -> [[f32; 4]; 4] {
    let screen: [usize; 2] = match axis {
        0 => [1, 2],
        1 => [0, 2],
        _ => [0, 1],
    };
    let mut matrix = [[0.0f32; 4]; 4];
    matrix[0][screen[0]] = 0.5;
    matrix[1][screen[1]] = 0.5;
    matrix[3][screen[0]] = 0.5;
    matrix[3][screen[1]] = 0.5;
    matrix[2][axis] = -2.5;
    matrix[3][axis] = 1.5;
    matrix[3][3] = 1.0;
    matrix
}

/// The eye that goes with [`box_from_clip_down`]: outside the near face.
fn eye_outside(axis: usize) -> [f32; 3] {
    let mut eye = [0.5f32; 3];
    eye[axis] = 3.0;
    eye
}

// ---------------------------------------------------------------------------
// Palettes and grids
// ---------------------------------------------------------------------------

/// Transparent at index 0, fully opaque white at every other index.
fn hard_mask_lut() -> Vec<u8> {
    let mut lut = vec![0u8; VOLUME_LUT_BYTES];
    for entry in 1..VOLUME_LUT_BYTES / 4 {
        lut[entry * 4..entry * 4 + 4].copy_from_slice(&[255, 255, 255, 255]);
    }
    lut
}

/// A table whose every non-zero index is white at a chosen straight alpha, for
/// the optical-depth measurements where a saturated mask would say nothing.
fn translucent_lut(alpha: u8) -> Vec<u8> {
    let mut lut = vec![0u8; VOLUME_LUT_BYTES];
    for entry in 1..VOLUME_LUT_BYTES / 4 {
        lut[entry * 4..entry * 4 + 4].copy_from_slice(&[255, 255, 255, alpha]);
    }
    lut
}

/// Extinction per kilometre that saturates a single sample.
const SATURATING_EXTINCTION: f32 = 1000.0;

/// Fill a grid with index 255 wherever a cell's **centre** is inside the shape.
fn plant<F: Fn([f32; 3]) -> bool>(cells: [u32; 3], inside: F) -> Vec<u8> {
    plant_at(cells, 255, inside)
}

/// [`plant`], at a chosen palette index.
fn plant_at<F: Fn([f32; 3]) -> bool>(cells: [u32; 3], index: u8, inside: F) -> Vec<u8> {
    let mut indices = vec![0u8; (cells[0] * cells[1] * cells[2]) as usize];
    for z in 0..cells[2] {
        for y in 0..cells[1] {
            for x in 0..cells[0] {
                let centre = [
                    (x as f32 + 0.5) / cells[0] as f32,
                    (y as f32 + 0.5) / cells[1] as f32,
                    (z as f32 + 0.5) / cells[2] as f32,
                ];
                if inside(centre) {
                    indices[((z * cells[1] + y) * cells[0] + x) as usize] = index;
                }
            }
        }
    }
    indices
}

/// One cell's size in box units along each axis.
fn cell_size(cells: [u32; 3]) -> [f32; 3] {
    [
        1.0 / cells[0] as f32,
        1.0 / cells[1] as f32,
        1.0 / cells[2] as f32,
    ]
}

// ---------------------------------------------------------------------------
// The host-side ray cast: exactly what the shader does, in f32
// ---------------------------------------------------------------------------

/// Apply a column-major matrix to `(ndc, depth, 1)` and divide through, which
/// is `unproject` in `volume.wgsl` character for character.
fn unproject(m: [[f32; 4]; 4], ndc: [f32; 2], depth: f32) -> [f32; 3] {
    let p = [ndc[0], ndc[1], depth, 1.0];
    let mut out = [0.0f32; 4];
    for (r, slot) in out.iter_mut().enumerate() {
        *slot = (0..4).map(|k| m[k][r] * p[k]).sum();
    }
    [out[0] / out[3], out[1] / out[3], out[2] / out[3]]
}

/// The pixel-centre NDC of pixel `(px, py)` in a `size`-shaped target.
fn ndc_for_pixel(px: u32, py: u32, size: [u32; 2]) -> [f32; 2] {
    [
        (px as f32 + 0.5) / size[0] as f32 * 2.0 - 1.0,
        1.0 - (py as f32 + 0.5) / size[1] as f32 * 2.0,
    ]
}

/// The ray the shader casts for one pixel: origin at the eye, unit direction
/// towards the far plane.
fn ray_for_pixel(
    uniform: &VolumeUniform,
    px: u32,
    py: u32,
    size: [u32; 2],
) -> ([f32; 3], [f32; 3]) {
    let far = unproject(uniform.box_from_clip, ndc_for_pixel(px, py, size), 1.0);
    let eye = uniform.eye_in_box;
    let d = [far[0] - eye[0], far[1] - eye[1], far[2] - eye[2]];
    let length = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
    (eye, [d[0] / length, d[1] / length, d[2] / length])
}

/// The step the shader takes for this ray: [`RAYMARCH_STEP_CELLS`] cells along
/// the ray in the grid's cell metric, floored so [`RAYMARCH_STEP_CEILING`]
/// steps always cover the span. Mirrors `fs_raymarch` line for line.
fn march_dt(direction: [f32; 3], cells: [u32; 3], span: f32) -> f32 {
    let scaled = [
        direction[0] * cells[0] as f32,
        direction[1] * cells[1] as f32,
        direction[2] * cells[2] as f32,
    ];
    let cells_per_t = (scaled[0] * scaled[0] + scaled[1] * scaled[1] + scaled[2] * scaled[2])
        .sqrt()
        .max(1.0);
    (RAYMARCH_STEP_CELLS / cells_per_t).max(span / RAYMARCH_STEP_CEILING as f32)
}

/// The shader's per-pixel jitter, mirrored: Jimenez's interleaved gradient
/// noise over the fragment's framebuffer coordinate (pixel centre, so +0.5).
#[allow(clippy::excessive_precision)]
fn ign(px: u32, py: u32) -> f32 {
    let x = px as f32 + 0.5;
    let y = py as f32 + 0.5;
    (52.9829189f32 * (0.06711056f32 * x + 0.00583715f32 * y).fract()).fract()
}

/// Entry and exit parameters of an axis-aligned box, the shader's `slab_entry_exit`
/// generalised off the unit cube. `exit <= entry` means a miss.
fn slab(origin: [f32; 3], direction: [f32; 3], lo: [f32; 3], hi: [f32; 3]) -> (f32, f32) {
    let mut entry = 0.0f32;
    let mut exit = f32::INFINITY;
    for axis in 0..3 {
        let d = if direction[axis].abs() < 1e-6 {
            1e-6f32.copysign(direction[axis])
        } else {
            direction[axis]
        };
        let a = (lo[axis] - origin[axis]) / d;
        let b = (hi[axis] - origin[axis]) / d;
        entry = entry.max(a.min(b));
        exit = exit.min(a.max(b));
    }
    (entry, exit)
}

/// Does the ray meet the axis-aligned box in front of the eye?
fn hits_box(origin: [f32; 3], direction: [f32; 3], lo: [f32; 3], hi: [f32; 3]) -> bool {
    let (entry, exit) = slab(origin, direction, lo, hi);
    exit > entry
}

/// Does the ray meet the axis-aligned ellipsoid in front of the eye?
fn hits_ellipsoid(
    origin: [f32; 3],
    direction: [f32; 3],
    centre: [f32; 3],
    radii: [f32; 3],
) -> bool {
    let mut o = [0.0f32; 3];
    let mut d = [0.0f32; 3];
    for axis in 0..3 {
        o[axis] = (origin[axis] - centre[axis]) / radii[axis];
        d[axis] = direction[axis] / radii[axis];
    }
    let a = d[0] * d[0] + d[1] * d[1] + d[2] * d[2];
    let b = 2.0 * (o[0] * d[0] + o[1] * d[1] + o[2] * d[2]);
    let c = o[0] * o[0] + o[1] * o[1] + o[2] * o[2] - 1.0;
    let discriminant = b * b - 4.0 * a * c;
    if discriminant <= 0.0 {
        return false;
    }
    // The far root: positive means some part of the ellipsoid is in front.
    (-b + discriminant.sqrt()) / (2.0 * a) > 0.0
}

/// The mask a predicate on rays would paint, one ray per pixel.
fn analytic_mask<F: Fn([f32; 3], [f32; 3]) -> bool>(
    uniform: &VolumeUniform,
    size: [u32; 2],
    hit: F,
) -> Vec<bool> {
    let mut mask = vec![false; (size[0] * size[1]) as usize];
    for py in 0..size[1] {
        for px in 0..size[0] {
            let (origin, direction) = ray_for_pixel(uniform, px, py, size);
            mask[(py * size[0] + px) as usize] = hit(origin, direction);
        }
    }
    mask
}

/// The alpha channel thresholded at half. See the module doc: with
/// [`hard_mask_lut`] and [`SATURATING_EXTINCTION`] there is nothing between 0
/// and 255 to threshold, which is checked rather than assumed.
fn rendered_mask(pixels: &[[u8; 4]]) -> Vec<bool> {
    pixels.iter().map(|p| p[3] >= 128).collect()
}

// ---------------------------------------------------------------------------
// Comparing two masks
// ---------------------------------------------------------------------------

/// Chebyshev distance from every pixel to the nearest pixel on `mask`'s
/// boundary, by the two-pass chamfer.
fn boundary_distance(mask: &[bool], size: [u32; 2]) -> Vec<f32> {
    let w = size[0] as usize;
    let h = size[1] as usize;
    let far = (w + h) as f32;
    let mut d = vec![far; w * h];
    for y in 0..h {
        for x in 0..w {
            let at = y * w + x;
            let me = mask[at];
            let edge = [(-1i32, 0i32), (1, 0), (0, -1), (0, 1)]
                .iter()
                .any(|(dx, dy)| {
                    let nx = x as i32 + dx;
                    let ny = y as i32 + dy;
                    nx >= 0
                        && ny >= 0
                        && (nx as usize) < w
                        && (ny as usize) < h
                        && mask[ny as usize * w + nx as usize] != me
                });
            if edge {
                d[at] = 0.0;
            }
        }
    }
    // Chebyshev: every one of the eight neighbours is one step away.
    for y in 0..h {
        for x in 0..w {
            let at = y * w + x;
            let mut best = d[at];
            for (dx, dy) in [(-1i32, 0i32), (-1, -1), (0, -1), (1, -1)] {
                let nx = x as i32 + dx;
                let ny = y as i32 + dy;
                if nx >= 0 && ny >= 0 && (nx as usize) < w && (ny as usize) < h {
                    best = best.min(d[ny as usize * w + nx as usize] + 1.0);
                }
            }
            d[at] = best;
        }
    }
    for y in (0..h).rev() {
        for x in (0..w).rev() {
            let at = y * w + x;
            let mut best = d[at];
            for (dx, dy) in [(1i32, 0i32), (1, 1), (0, 1), (-1, 1)] {
                let nx = x as i32 + dx;
                let ny = y as i32 + dy;
                if nx >= 0 && ny >= 0 && (nx as usize) < w && (ny as usize) < h {
                    best = best.min(d[ny as usize * w + nx as usize] + 1.0);
                }
            }
            d[at] = best;
        }
    }
    d
}

/// The centroid of a mask in pixels, or `None` when it is empty.
fn centroid(mask: &[bool], size: [u32; 2]) -> Option<[f64; 2]> {
    let mut sum = [0.0f64; 2];
    let mut count = 0u64;
    for (at, on) in mask.iter().enumerate() {
        if *on {
            sum[0] += (at as u32 % size[0]) as f64;
            sum[1] += (at as u32 / size[0]) as f64;
            count += 1;
        }
    }
    (count > 0).then(|| [sum[0] / count as f64, sum[1] / count as f64])
}

/// Everything one mask comparison says.
#[derive(Clone, Copy, Debug)]
struct Metrics {
    rendered: usize,
    expected: usize,
    intersection: usize,
    symmetric_difference: usize,
    iou: f64,
    centroid_offset_px: [f64; 2],
    centroid_magnitude_px: f64,
    max_boundary_displacement_px: f64,
}

fn compare(rendered: &[bool], expected: &[bool], size: [u32; 2]) -> Metrics {
    let mut intersection = 0usize;
    let mut union = 0usize;
    for (a, b) in rendered.iter().zip(expected) {
        if *a && *b {
            intersection += 1;
        }
        if *a || *b {
            union += 1;
        }
    }
    let rendered_count = rendered.iter().filter(|on| **on).count();
    let expected_count = expected.iter().filter(|on| **on).count();

    let distance = boundary_distance(expected, size);
    let mut worst = 0.0f64;
    for (at, (a, b)) in rendered.iter().zip(expected).enumerate() {
        if a != b {
            worst = worst.max(f64::from(distance[at]));
        }
    }

    let offset = match (centroid(rendered, size), centroid(expected, size)) {
        (Some(r), Some(e)) => [r[0] - e[0], r[1] - e[1]],
        _ => [f64::NAN, f64::NAN],
    };

    Metrics {
        rendered: rendered_count,
        expected: expected_count,
        intersection,
        symmetric_difference: union - intersection,
        iou: if union == 0 {
            1.0
        } else {
            intersection as f64 / union as f64
        },
        centroid_offset_px: offset,
        centroid_magnitude_px: offset[0].hypot(offset[1]),
        max_boundary_displacement_px: worst,
    }
}

impl Metrics {
    fn report(&self, label: &str) {
        println!(
            "  {label:<42} IoU {:.5}  rendered {:>7}  analytic {:>7}  both {:>7}  \
             symdiff {:>6} ({:.3}%)  max-edge {:>4.1} px  centroid ({:+.3}, {:+.3}) px = {:.3}",
            self.iou,
            self.rendered,
            self.expected,
            self.intersection,
            self.symmetric_difference,
            100.0 * self.symmetric_difference as f64 / self.expected.max(1) as f64,
            self.max_boundary_displacement_px,
            self.centroid_offset_px[0],
            self.centroid_offset_px[1],
            self.centroid_magnitude_px,
        );
    }
}

/// The fraction of `expected` that `rendered` paints.
fn covered_fraction(rendered: &[bool], expected: &[bool]) -> f64 {
    let need = expected.iter().filter(|on| **on).count();
    let got = rendered
        .iter()
        .zip(expected)
        .filter(|(r, e)| **r && **e)
        .count();
    if need == 0 {
        1.0
    } else {
        got as f64 / need as f64
    }
}

/// How far inside `expected` the deepest **lost** pixel sits, in Chebyshev
/// pixels from `expected`'s boundary. Zero means every miss is on the boundary
/// itself.
fn max_lost_distance(rendered: &[bool], expected: &[bool], size: [u32; 2]) -> f64 {
    let distance = boundary_distance(expected, size);
    let mut worst = 0.0f64;
    for (at, (r, e)) in rendered.iter().zip(expected).enumerate() {
        if *e && !*r {
            worst = worst.max(f64::from(distance[at]));
        }
    }
    worst
}

/// The fraction of `expected`'s area that `rendered` paints outside `outer`.
fn overflow_fraction(rendered: &[bool], outer: &[bool], expected: &[bool]) -> f64 {
    let out = rendered
        .iter()
        .zip(outer)
        .filter(|(r, o)| **r && !**o)
        .count();
    out as f64 / expected.iter().filter(|on| **on).count().max(1) as f64
}

/// The IoU floor a render can be held to **without tuning a constant**.
fn derived_iou_floor(exact: &[bool], outer: &[bool]) -> f64 {
    exact.iter().filter(|on| **on).count() as f64
        / outer.iter().filter(|on| **on).count().max(1) as f64
}

/// True when any mask pixel sits on the image border, i.e. the silhouette is
/// clipped and every extent measured from it is a measurement of the viewport.
fn touches_border(mask: &[bool], size: [u32; 2]) -> bool {
    let w = size[0] as usize;
    let h = size[1] as usize;
    (0..w).any(|x| mask[x] || mask[(h - 1) * w + x])
        || (0..h).any(|y| mask[y * w] || mask[y * w + w - 1])
}

/// The mask's bounding box as `(min_x, max_x, min_y, max_y)`, or `None` when
/// it is empty.
fn bounds(mask: &[bool], size: [u32; 2]) -> Option<(u32, u32, u32, u32)> {
    let mut found: Option<(u32, u32, u32, u32)> = None;
    for (at, on) in mask.iter().enumerate() {
        if !on {
            continue;
        }
        let x = at as u32 % size[0];
        let y = at as u32 / size[0];
        found = Some(match found {
            None => (x, x, y, y),
            Some((lx, hx, ly, hy)) => (lx.min(x), hx.max(x), ly.min(y), hy.max(y)),
        });
    }
    found
}

// ---------------------------------------------------------------------------
// Scenes
// ---------------------------------------------------------------------------

/// A uniform set up for a hard-mask render through a real orbit camera.
fn masking_uniform(
    camera: OrbitCamera,
    box_size_km: [f32; 3],
    cells: [u32; 3],
    size: [u32; 2],
) -> VolumeUniform {
    let view = view_for(camera, box_size_km, size[0] as f32 / size[1] as f32)
        .expect("the test camera must be viewable");
    let mut uniform = VolumeUniform::new(box_size_km, cells);
    uniform.box_from_clip = view.box_from_clip;
    uniform.eye_in_box = view.eye_in_box;
    uniform.extinction_per_km = SATURATING_EXTINCTION;
    // Shading multiplies colour, never alpha — but it costs six extra fetches
    // per step and this file only ever reads alpha, so it is off throughout.
    uniform.gradient_shading = false;
    uniform
}

fn camera(yaw: f32, pitch: f32, distance: f32, exaggeration: f32) -> OrbitCamera {
    OrbitCamera::restore(yaw, pitch, distance, [0.0; 3], exaggeration).expect("a finite camera")
}

/// The eye distance that frames [`BOX_KM`] the same size at every exaggeration.
fn framed_distance(exaggeration: f32) -> f32 {
    2.5 * stretched_half_diagonal(exaggeration) / (BOX_KM[1] * std::f32::consts::FRAC_1_SQRT_2)
}

/// Half the diagonal of [`BOX_KM`] stretched by `exaggeration`. See
/// [`framed_distance`], which is the only reason this harness still needs it.
fn stretched_half_diagonal(exaggeration: f32) -> f32 {
    0.5 * (BOX_KM[0] * BOX_KM[0]
        + BOX_KM[1] * BOX_KM[1]
        + (BOX_KM[2] * exaggeration) * (BOX_KM[2] * exaggeration))
        .sqrt()
}

/// A box whose horizontal axes are far larger than its vertical one, which is
/// the shape a real volume has.
const BOX_KM: [f32; 3] = [240.0, 240.0, 60.0];

/// The desktop grid, and a coarse one, so every resolution-dependent residual
/// below is measured at both.
const FINE: [u32; 3] = [256, 256, 128];
const COARSE: [u32; 3] = [64, 64, 64];

// ---------------------------------------------------------------------------
// 1. The instrument itself
// ---------------------------------------------------------------------------

/// The mask really is a mask, and image rows really do run top-down.
#[test]
#[ignore = "needs a real wgpu adapter; see the doc comment for the invocation"]
fn a_hard_palette_makes_the_render_a_binary_mask_and_the_rows_run_top_down() {
    let _serialised = gpu_lock();
    let (device, queue) = device();
    let pipelines = VolumePipelines::new(&device, attachments());
    pipelines.upload_quad(&queue);
    let lut = hard_mask_lut();
    let size = [256u32, 256];

    // --- row order, through an axis-aligned camera with no camera maths in it
    let cells = COARSE;
    let low = plant(cells, |c| c[2] < 0.4);
    let mut uniform = VolumeUniform::new(BOX_KM, cells);
    // Looking down the y axis: ndc.x spans box x, ndc.y spans box z.
    uniform.box_from_clip = box_from_clip_down(1);
    uniform.eye_in_box = eye_outside(1);
    uniform.extinction_per_km = SATURATING_EXTINCTION;
    uniform.gradient_shading = false;

    let pixels = raymarch_once(
        &device, &queue, &pipelines, cells, &low, &lut, &uniform, size,
    );
    let mask = rendered_mask(&pixels);
    let (_, _, min_y, max_y) = bounds(&mask, size).expect("a slab in the bottom of the box paints");
    println!(
        "row order: a z<0.4 slab occupies image rows {min_y}..={max_y} of {}",
        size[1]
    );
    assert!(
        min_y > size[1] / 2,
        "a slab in the bottom of the box painted rows {min_y}..={max_y}; row 0 is not \
         the top of the render target, so every NDC this file computes has the wrong \
         vertical sign and every silhouette below is upside down"
    );

    // --- saturation, over a shape with a long boundary
    let sphere = plant(cells, |c| {
        let d = [c[0] - 0.5, c[1] - 0.5, c[2] - 0.5];
        (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt() < 0.3
    });
    let uniform = masking_uniform(camera(225.0, 25.0, 2.5, 1.0), BOX_KM, cells, size);
    let pixels = raymarch_once(
        &device, &queue, &pipelines, cells, &sphere, &lut, &uniform, size,
    );

    let mut zero = 0usize;
    let mut low_grey = 0usize;
    let mut high_grey = 0usize;
    let mut full = 0usize;
    for p in &pixels {
        match p[3] {
            0 => zero += 1,
            1..=127 => low_grey += 1,
            128..=254 => high_grey += 1,
            255 => full += 1,
        }
    }
    let mask = rendered_mask(&pixels);
    let edge = boundary_distance(&mask, size)
        .iter()
        .filter(|d| **d == 0.0)
        .count();
    println!(
        "alpha histogram over {} px: 0 -> {zero}, 1..127 -> {low_grey}, \
         128..254 -> {high_grey}, 255 -> {full}; mask boundary is {edge} px, \
         so the grey band is {:.4} px wide on average",
        pixels.len(),
        (low_grey + high_grey) as f64 / (edge.max(1) as f64 / 2.0),
    );
    assert!(
        low_grey + high_grey == 0,
        "the render is not a binary mask: {} of {} pixels are partially \
         transparent, so `alpha >= 128` is a threshold on a soft edge rather \
         than a silhouette",
        low_grey + high_grey,
        pixels.len(),
    );
    assert!(full > 0 && zero > 0, "the mask must have both phases");

    // --- the default edge width really is hard, on a shape that can see it
    let faint_sphere = plant_at(cells, 1, |c| {
        let d = [c[0] - 0.5, c[1] - 0.5, c[2] - 0.5];
        (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt() < 0.3
    });
    let pixels = raymarch_once(
        &device,
        &queue,
        &pipelines,
        cells,
        &faint_sphere,
        &lut,
        &uniform,
        size,
    );
    let mut faint_grey = 0usize;
    let mut faint_full = 0usize;
    let mut faint_zero = 0usize;
    for p in &pixels {
        match p[3] {
            0 => faint_zero += 1,
            255 => faint_full += 1,
            _ => faint_grey += 1,
        }
    }
    println!(
        "index-1 sphere: alpha histogram over {} px: 0 -> {faint_zero}, \
         partial -> {faint_grey}, 255 -> {faint_full}",
        pixels.len(),
    );
    assert!(
        faint_grey == 0,
        "{faint_grey} of {} pixels of an index-1 shape are partially \
         transparent under a saturating extinction: a soft edge width has \
         leaked into the instrument default. DEFAULT_EDGE_SOFT_WIDTH must \
         stay 0 — the production width belongs to volume::bridge, and a soft \
         default puts a grey band on every instrument's edge, exactly as its \
         doc claims",
        pixels.len(),
    );
    assert!(
        faint_full > 0 && faint_zero > 0,
        "the index-1 mask must have both phases"
    );
    // --- and the two spheres are the SAME mask, which is residual #1's claim
    assert_eq!(
        faint_full, full,
        "an index-1 sphere paints {faint_full} px and an index-255 sphere \
         {full} px of the SAME planted geometry: the silhouette's reach is \
         reading the stored value, which is the `R8Unorm` index gate's \
         behaviour and not the coverage tent's",
    );
}

// ---------------------------------------------------------------------------
// 2. A sphere
// ---------------------------------------------------------------------------

/// A sphere planted in box space projects to the silhouette an exact
/// ray-sphere test predicts.
#[test]
#[ignore = "needs a real wgpu adapter; see the doc comment for the invocation"]
fn a_planted_sphere_projects_to_its_exact_ray_cast_silhouette() {
    let _serialised = gpu_lock();
    let (device, queue) = device();
    let pipelines = VolumePipelines::new(&device, attachments());
    pipelines.upload_quad(&queue);
    let lut = hard_mask_lut();
    let size = [384u32, 384];
    const RADIUS: f32 = 0.3;
    let centre = [0.5f32; 3];

    for cells in [FINE, COARSE] {
        let grid = plant(cells, |c| {
            let d = [c[0] - centre[0], c[1] - centre[1], c[2] - centre[2]];
            (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt() < RADIUS
        });
        let uniform = masking_uniform(camera(225.0, 25.0, 2.5, 1.0), BOX_KM, cells, size);
        let pixels = raymarch_once(
            &device, &queue, &pipelines, cells, &grid, &lut, &uniform, size,
        );
        let rendered = rendered_mask(&pixels);
        assert!(
            !touches_border(&rendered, size),
            "the silhouette is clipped by the viewport, so its extent is the \
             pane's rather than the sphere's"
        );

        let exact = analytic_mask(&uniform, size, |o, d| {
            hits_ellipsoid(o, d, centre, [RADIUS; 3])
        });
        let step = cell_size(cells);
        let grown = |by: f32| {
            let radii = [
                RADIUS + by * step[0],
                RADIUS + by * step[1],
                RADIUS + by * step[2],
            ];
            analytic_mask(&uniform, size, move |o, d| {
                hits_ellipsoid(o, d, centre, radii)
            })
        };
        let dilated = grown(1.0);
        let outer = grown(2.0);

        println!("sphere r={RADIUS} box-units, grid {cells:?}, render {size:?}:");
        let exact_metrics = compare(&rendered, &exact, size);
        exact_metrics.report("vs the exact planted sphere");
        compare(&rendered, &dilated, size).report("vs the sphere dilated by one cell");
        let floor = derived_iou_floor(&exact, &outer);
        println!(
            "    one cell is {step:?} box units; projected radius about {:.1} px; \
             covers {:.4}% of the exact mask, {:.4}% of its area lies outside the \
             two-cell envelope; derived IoU floor {floor:.4}",
            (exact_metrics.expected as f64 / std::f64::consts::PI).sqrt(),
            100.0 * covered_fraction(&rendered, &exact),
            100.0 * overflow_fraction(&rendered, &outer, &exact),
        );

        assert!(
            covered_fraction(&rendered, &exact) > 0.999,
            "the render lost {:.3}% of the analytic silhouette; the filter can \
             only dilate and a sphere of radius {RADIUS} is far thicker than a \
             march step, so nothing here should erode",
            100.0 * (1.0 - covered_fraction(&rendered, &exact)),
        );
        assert!(
            overflow_fraction(&rendered, &outer, &exact) < 0.005,
            "{:.3}% of the analytic area was painted outside the two-cell \
             envelope, which is more than the linear filter can reach",
            100.0 * overflow_fraction(&rendered, &outer, &exact),
        );
        assert!(
            exact_metrics.iou >= floor,
            "a sphere of radius {RADIUS} on a {cells:?} grid rendered at IoU \
             {:.4}, under the {floor:.4} that containment between the exact \
             surface and its two-cell envelope guarantees",
            exact_metrics.iou,
        );
        assert!(
            exact_metrics.centroid_magnitude_px < 1.0,
            "the rendered silhouette's centroid is {:.3} px from the analytic \
             one; a centred sphere must project to a centred silhouette",
            exact_metrics.centroid_magnitude_px,
        );
    }
}

/// An off-centre sphere puts its silhouette exactly where the rays say.
#[test]
#[ignore = "needs a real wgpu adapter; see the doc comment for the invocation"]
fn an_off_centre_sphere_puts_its_silhouette_where_the_rays_say() {
    let _serialised = gpu_lock();
    let (device, queue) = device();
    let pipelines = VolumePipelines::new(&device, attachments());
    pipelines.upload_quad(&queue);
    let lut = hard_mask_lut();
    let size = [384u32, 384];
    let cells = [128u32, 128, 128];
    const RADIUS: f32 = 0.15;

    let uniform = masking_uniform(camera(225.0, 25.0, 2.5, 1.0), BOX_KM, cells, size);
    let mut worst = 0.0f64;
    for offset in [
        [0.0f32, 0.0, 0.0],
        [0.25, 0.0, 0.0],
        [-0.25, 0.0, 0.0],
        [0.0, 0.25, 0.0],
        [0.0, 0.0, 0.28],
        [0.2, -0.2, -0.2],
    ] {
        let centre = [0.5 + offset[0], 0.5 + offset[1], 0.5 + offset[2]];
        let grid = plant(cells, |c| {
            let d = [c[0] - centre[0], c[1] - centre[1], c[2] - centre[2]];
            (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt() < RADIUS
        });
        let pixels = raymarch_once(
            &device, &queue, &pipelines, cells, &grid, &lut, &uniform, size,
        );
        let rendered = rendered_mask(&pixels);
        let expected = analytic_mask(&uniform, size, |o, d| {
            hits_ellipsoid(o, d, centre, [RADIUS; 3])
        });
        let metrics = compare(&rendered, &expected, size);
        let analytic_centre = centroid(&expected, size).expect("the sphere is in frame");
        println!(
            "sphere at box {centre:?}: analytic centroid ({:.2}, {:.2}) px",
            analytic_centre[0], analytic_centre[1],
        );
        metrics.report("");
        worst = worst.max(metrics.centroid_magnitude_px);
    }
    println!("worst centroid residual over six placements: {worst:.4} px");
    assert!(
        worst < 1.0,
        "a planted sphere's rendered centroid strayed {worst:.3} px from its \
         analytic one; the silhouette is the right size in the wrong place"
    );
}

/// An eye **inside** the box sees what is ahead of it and none of what is
/// behind it.
#[test]
#[ignore = "needs a real wgpu adapter; see the doc comment for the invocation"]
fn an_eye_inside_the_box_sees_ahead_and_none_of_what_is_behind() {
    let _serialised = gpu_lock();
    let (device, queue) = device();
    let pipelines = VolumePipelines::new(&device, attachments());
    pipelines.upload_quad(&queue);
    let lut = hard_mask_lut();
    let size = [384u32, 384];
    let cells = [128u32, 128, 128];
    const RADIUS: f32 = 0.08;
    // Due south of the pivot looking north, level, at the zoom's near stop.
    const AHEAD: [f32; 3] = [0.5, 0.75, 0.5];
    const BEHIND: [f32; 3] = [0.5, 0.15, 0.5];

    for exaggeration in [1.0f32, 12.0] {
        let uniform = masking_uniform(camera(180.0, 0.0, 0.05, exaggeration), BOX_KM, cells, size);
        assert!(
            uniform.eye_in_box.iter().all(|c| (0.0..=1.0).contains(c)),
            "precondition at {exaggeration}x: the fully-zoomed eye must be inside \
             the box, got {:?}",
            uniform.eye_in_box,
        );
        // The behind sphere must be wholly behind the eye, or the render is
        // entitled to paint it and the assertion below tests nothing.
        assert!(
            BEHIND[1] + RADIUS < uniform.eye_in_box[1],
            "precondition at {exaggeration}x: the sphere at {BEHIND:?} reaches \
             past the eye at {:?}",
            uniform.eye_in_box,
        );

        let planted_behind = plant(cells, |c| {
            let d = [c[0] - BEHIND[0], c[1] - BEHIND[1], c[2] - BEHIND[2]];
            (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt() < RADIUS
        });
        let pixels = raymarch_once(
            &device,
            &queue,
            &pipelines,
            cells,
            &planted_behind,
            &lut,
            &uniform,
            size,
        );
        let painted = rendered_mask(&pixels).iter().filter(|on| **on).count();
        assert_eq!(
            painted, 0,
            "at {exaggeration}x a sphere wholly behind an inside eye painted \
             {painted} pixels; the march is starting at the box wall behind the \
             camera instead of at the camera",
        );

        let planted_ahead = plant(cells, |c| {
            let d = [c[0] - AHEAD[0], c[1] - AHEAD[1], c[2] - AHEAD[2]];
            (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt() < RADIUS
        });
        let pixels = raymarch_once(
            &device,
            &queue,
            &pipelines,
            cells,
            &planted_ahead,
            &lut,
            &uniform,
            size,
        );
        let rendered = rendered_mask(&pixels);
        let expected = analytic_mask(&uniform, size, |o, d| {
            hits_ellipsoid(o, d, AHEAD, [RADIUS; 3])
        });
        let metrics = compare(&rendered, &expected, size);
        metrics.report(&format!("inside eye, {exaggeration}x, sphere ahead"));
        let coverage = covered_fraction(&rendered, &expected);
        assert!(
            coverage > 0.99,
            "at {exaggeration}x an inside eye lost {:.3}% of the silhouette \
             ahead of it",
            (1.0 - coverage) * 100.0,
        );
        assert!(
            metrics.centroid_magnitude_px < 1.5,
            "at {exaggeration}x the silhouette ahead of an inside eye strayed \
             {:.3} px from where the rays say it is",
            metrics.centroid_magnitude_px,
        );
    }
}

// ---------------------------------------------------------------------------
// 3. Slabs
// ---------------------------------------------------------------------------

/// An axis-aligned slab projects to the silhouette an exact ray-box test
/// predicts.
#[test]
#[ignore = "needs a real wgpu adapter; see the doc comment for the invocation"]
fn a_planted_slab_projects_to_its_exact_ray_cast_silhouette() {
    let _serialised = gpu_lock();
    let (device, queue) = device();
    let pipelines = VolumePipelines::new(&device, attachments());
    pipelines.upload_quad(&queue);
    let lut = hard_mask_lut();
    let size = [384u32, 384];

    // Every bound is on a cell boundary at both resolutions, so the
    // rasterisation is exact and the only residual is the filter and the march.
    let shapes: [(&str, [f32; 3], [f32; 3]); 2] = [
        (
            "horizontal layer, z in 0.25..0.5",
            [0.0, 0.0, 0.25],
            [1.0, 1.0, 0.5],
        ),
        (
            "wall thin in x, x in 0.375..0.4375",
            [0.375, 0.125, 0.125],
            [0.4375, 0.875, 0.875],
        ),
    ];

    for cells in [FINE, COARSE] {
        for (label, lo, hi) in shapes {
            let grid = plant(cells, |c| {
                (0..3).all(|axis| c[axis] > lo[axis] && c[axis] < hi[axis])
            });
            let uniform = masking_uniform(camera(225.0, 25.0, 2.8, 1.0), BOX_KM, cells, size);
            let pixels = raymarch_once(
                &device, &queue, &pipelines, cells, &grid, &lut, &uniform, size,
            );
            let rendered = rendered_mask(&pixels);
            let expected = analytic_mask(&uniform, size, |o, d| hits_box(o, d, lo, hi));

            let step = cell_size(cells);
            let grown = |by: f32| {
                let g_lo = [
                    lo[0] - by * step[0],
                    lo[1] - by * step[1],
                    lo[2] - by * step[2],
                ];
                let g_hi = [
                    hi[0] + by * step[0],
                    hi[1] + by * step[1],
                    hi[2] + by * step[2],
                ];
                analytic_mask(&uniform, size, move |o, d| hits_box(o, d, g_lo, g_hi))
            };
            let dilated = grown(1.0);
            let outer = grown(2.0);

            println!("{label}, grid {cells:?}:");
            let metrics = compare(&rendered, &expected, size);
            metrics.report("vs the exact planted slab");
            compare(&rendered, &dilated, size).report("vs the slab dilated by one cell");
            let floor = derived_iou_floor(&expected, &outer);
            println!(
                "    covers {:.4}% of the exact mask, {:.4}% of its area outside the \
                 two-cell envelope; derived IoU floor {floor:.4}",
                100.0 * covered_fraction(&rendered, &expected),
                100.0 * overflow_fraction(&rendered, &outer, &expected),
            );

            // Coverage splits at the boundary now. A slab's extreme silhouette
            // rows are rays whose chord through it is shorter than one march
            // step, and whether the jittered comb samples inside that chord is
            // the pixel's own hash — measured: the coarse grid loses 0.19% of
            // this layer's mask, every lost pixel at Chebyshev distance 0 from
            // the analytic boundary, and the fine grid 0.04%. The interior is
            // still the hard claim: no *lost* pixel may sit deeper than the
            // boundary ring. Two different bounds, stated so the 1 px below is
            // not read as the edge's total play: **losses** are the jitter's
            // and are bounded at 1 px (the tangent ring), while **overpaint**
            // is the linear filter's dilation and is bounded only by the
            // two-cell envelope — about 4-5 px at the coarse grid's projected
            // cell size here — which the overflow assertion below holds it to.
            assert!(
                covered_fraction(&rendered, &expected) > 0.995,
                "{label} on {cells:?} lost {:.3}% of its analytic silhouette, \
                 more than the tangent ring can explain",
                100.0 * (1.0 - covered_fraction(&rendered, &expected)),
            );
            assert!(
                max_lost_distance(&rendered, &expected, size) <= 1.0,
                "{label} on {cells:?} lost a pixel {:.1} px inside the analytic \
                 mask; jitter can only reach the tangent ring, so anything \
                 deeper means the geometry moved",
                max_lost_distance(&rendered, &expected, size),
            );
            assert!(
                overflow_fraction(&rendered, &outer, &expected) < 0.005,
                "{label} on {cells:?} painted {:.3}% of its area outside the \
                 two-cell envelope",
                100.0 * overflow_fraction(&rendered, &outer, &expected),
            );
            assert!(
                metrics.iou >= floor,
                "{label} on a {cells:?} grid rendered at IoU {:.4}, under the \
                 {floor:.4} that containment between the exact slab and its \
                 two-cell envelope guarantees",
                metrics.iou,
            );
        }
    }
}

/// A sphere that is a true sphere in **kilometres** is an ellipsoid in box
/// space, and renders as one.
#[test]
#[ignore = "needs a real wgpu adapter; see the doc comment for the invocation"]
fn a_sphere_in_kilometres_is_an_ellipsoid_in_box_space_and_renders_as_one() {
    let _serialised = gpu_lock();
    let (device, queue) = device();
    let pipelines = VolumePipelines::new(&device, attachments());
    pipelines.upload_quad(&queue);
    let lut = hard_mask_lut();
    let size = [384u32, 384];
    let cells = [128u32, 128, 128];
    // A box squat enough that a 25 km sphere is a strongly eccentric ellipsoid
    // in box space (radii 0.208, 0.208, 0.417) without leaving the box.
    let box_km = [120.0f32, 120.0, 60.0];
    const RADIUS_KM: f32 = 25.0;
    let radii = [
        RADIUS_KM / box_km[0],
        RADIUS_KM / box_km[1],
        RADIUS_KM / box_km[2],
    ];
    let centre = [0.5f32; 3];

    let grid = plant(cells, |c| {
        let d = [
            (c[0] - centre[0]) / radii[0],
            (c[1] - centre[1]) / radii[1],
            (c[2] - centre[2]) / radii[2],
        ];
        d[0] * d[0] + d[1] * d[1] + d[2] * d[2] < 1.0
    });
    let uniform = masking_uniform(camera(225.0, 25.0, 2.5, 1.0), box_km, cells, size);
    let pixels = raymarch_once(
        &device, &queue, &pipelines, cells, &grid, &lut, &uniform, size,
    );
    let rendered = rendered_mask(&pixels);
    let expected = analytic_mask(&uniform, size, |o, d| hits_ellipsoid(o, d, centre, radii));
    let step = cell_size(cells);
    let grown = |by: f32| {
        let grown_radii = [
            radii[0] + by * step[0],
            radii[1] + by * step[1],
            radii[2] + by * step[2],
        ];
        analytic_mask(&uniform, size, move |o, d| {
            hits_ellipsoid(o, d, centre, grown_radii)
        })
    };
    let dilated = grown(1.0);
    let outer = grown(2.0);

    println!("a {RADIUS_KM} km sphere in a {box_km:?} km box is box-space radii {radii:?}:");
    let metrics = compare(&rendered, &expected, size);
    metrics.report("vs the exact ellipsoid");
    compare(&rendered, &dilated, size).report("vs the ellipsoid dilated by one cell");
    let floor = derived_iou_floor(&expected, &outer);
    println!(
        "    covers {:.4}% of the exact mask, {:.4}% of its area outside the \
         two-cell envelope; derived IoU floor {floor:.4}",
        100.0 * covered_fraction(&rendered, &expected),
        100.0 * overflow_fraction(&rendered, &outer, &expected),
    );
    assert!(
        covered_fraction(&rendered, &expected) > 0.999,
        "the render lost {:.3}% of the ellipsoid's analytic silhouette",
        100.0 * (1.0 - covered_fraction(&rendered, &expected)),
    );
    assert!(
        metrics.iou >= floor,
        "a true-kilometre sphere rendered at IoU {:.4} against the ellipsoid \
         its own box conversion predicts, under the derived floor {floor:.4}",
        metrics.iou,
    );
}

// ---------------------------------------------------------------------------
// 4. Vertical exaggeration
// ---------------------------------------------------------------------------

/// The silhouette tracks the rays at every vertical exaggeration, and a cube's
/// measured aspect grows exactly linearly with the knob.
#[test]
#[ignore = "needs a real wgpu adapter; see the doc comment for the invocation"]
fn the_silhouette_matches_the_rays_at_every_vertical_exaggeration() {
    let _serialised = gpu_lock();
    let (device, queue) = device();
    let pipelines = VolumePipelines::new(&device, attachments());
    pipelines.upload_quad(&queue);
    let lut = hard_mask_lut();
    let cells = [128u32, 128, 128];
    const EXAGGERATIONS: [f32; 4] = [1.0, 3.0, 6.0, 12.0];

    // --- the primary check: a sphere, at the shipped viewing angle
    let size = [384u32, 384];
    const RADIUS: f32 = 0.3;
    let centre = [0.5f32; 3];
    let sphere = plant(cells, |c| {
        let d = [c[0] - centre[0], c[1] - centre[1], c[2] - centre[2]];
        (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt() < RADIUS
    });
    println!("a box-space sphere against its own ray cast, at each exaggeration:");
    let mut ious = Vec::new();
    for exaggeration in EXAGGERATIONS {
        let uniform = masking_uniform(
            camera(225.0, 25.0, framed_distance(exaggeration), exaggeration),
            BOX_KM,
            cells,
            size,
        );
        let pixels = raymarch_once(
            &device, &queue, &pipelines, cells, &sphere, &lut, &uniform, size,
        );
        let rendered = rendered_mask(&pixels);
        let expected = analytic_mask(&uniform, size, |o, d| {
            hits_ellipsoid(o, d, centre, [RADIUS; 3])
        });
        let metrics = compare(&rendered, &expected, size);
        metrics.report(&format!("{exaggeration:>5}x"));
        assert!(
            covered_fraction(&rendered, &expected) > 0.999,
            "at {exaggeration}x the render lost {:.3}% of the ray-cast \
             silhouette built with its very own box_from_clip",
            100.0 * (1.0 - covered_fraction(&rendered, &expected)),
        );
        ious.push(metrics.iou);
    }
    // The real statement: the residual is the filter's, so it must be the *same*
    // residual at every exaggeration. A knob that reached the geometry rather
    // than the camera would move this.
    let spread = ious.iter().cloned().fold(f64::MIN, f64::max)
        - ious.iter().cloned().fold(f64::MAX, f64::min);
    println!("  IoU spread across the four exaggerations: {spread:.5}");
    assert!(
        spread < 0.01,
        "the agreement with the ray cast varies by {spread:.4} across the \
         exaggeration range; the residual should be the cell-scale filter band \
         and nothing else, so it should not depend on the knob"
    );

    // --- the aspect law
    let size = [512u32, 512];
    const HALF: f32 = 0.35;
    let cube = plant(cells, |c| (0..3).all(|axis| (c[axis] - 0.5).abs() < HALF));
    let f = 1.0 / (0.5f32 * 40.0f32.to_radians()).tan();
    println!(
        "a cube of box half-extent {HALF}, seen level from due south, in a \
         {BOX_KM:?} km box (Sz/Sx = {:.4}):",
        BOX_KM[2] / BOX_KM[0],
    );
    for exaggeration in EXAGGERATIONS {
        let uniform = masking_uniform(
            camera(180.0, 0.0, framed_distance(exaggeration), exaggeration),
            BOX_KM,
            cells,
            size,
        );
        let pixels = raymarch_once(
            &device, &queue, &pipelines, cells, &cube, &lut, &uniform, size,
        );
        let rendered = rendered_mask(&pixels);
        assert!(
            !touches_border(&rendered, size),
            "at {exaggeration}x the cube's silhouette is clipped, so its \
             measured extent is the viewport's"
        );
        let (min_x, max_x, min_y, max_y) = bounds(&rendered, size).expect("the cube paints");
        let width = (max_x - min_x + 1) as f64;
        let height = (max_y - min_y + 1) as f64;
        let measured = height / width;
        let predicted = f64::from(exaggeration) * f64::from(BOX_KM[2] / BOX_KM[0]);

        // The absolute sizes, to show where the framing compensation lives.
        let distance = 2.5 * stretched_half_diagonal(exaggeration);
        let depth = distance - HALF * BOX_KM[1];
        let predicted_width =
            f64::from(2.0 * f * HALF * BOX_KM[0] / depth) * f64::from(size[0]) / 2.0;
        let predicted_height =
            f64::from(2.0 * f * HALF * BOX_KM[2] * exaggeration / depth) * f64::from(size[1]) / 2.0;

        println!(
            "  {exaggeration:>5}x  {width:>5.0} x {height:>5.0} px  (predicted \
             {predicted_width:>6.1} x {predicted_height:>6.1})  ratio {measured:.4} \
             vs predicted {predicted:.4}  ({:+.2}%)",
            100.0 * (measured / predicted - 1.0),
        );
        assert!(
            (measured / predicted - 1.0).abs() < 0.03,
            "at {exaggeration}x the silhouette's height/width is {measured:.4}, \
             not the {predicted:.4} that E·Sz/Sx predicts",
        );
    }
}

/// Optical depth is measured against the **unexaggerated** box, and the raw
/// alpha at the centre is not invariant — the two are different statements.
#[test]
#[ignore = "needs a real wgpu adapter; see the doc comment for the invocation"]
fn optical_depth_is_measured_against_the_unexaggerated_box() {
    let _serialised = gpu_lock();
    let (device, queue) = device();
    let pipelines = VolumePipelines::new(&device, attachments());
    pipelines.upload_quad(&queue);

    const LUT_ALPHA: u8 = 128;
    const EXTINCTION: f32 = 0.01;
    let lut = translucent_lut(LUT_ALPHA);
    let size = [64u32, 64];
    let cells = [64u32, 64, 64];
    // Every cell filled, so the whole span between the box's faces contributes
    // and the path length is the slab crossing itself rather than a rasterised
    // shape's.
    let filled = vec![255u8; (cells[0] * cells[1] * cells[2]) as usize];
    // A steep camera, because that is where a stretched `box_size_km` would
    // show: the z component of the reconstructed step is what it would change.
    let pitch = 80.0;

    for exaggeration in [1.0f32, 12.0] {
        let mut uniform =
            masking_uniform(camera(225.0, pitch, 2.5, exaggeration), BOX_KM, cells, size);
        uniform.extinction_per_km = EXTINCTION;
        // The exaggeration lane rides `box_size_km.w`, and it must be SET for
        // this test to guard anything: `masking_uniform` leaves it at 1.0, and
        // with the lane at 1.0 the exact violation this test exists to catch —
        // `step_length_km` reading the stretched extent — is a multiply by one
        // that no measurement can see. Production writes the camera's own
        // value here (`volume::bridge`), so the instrument does the same, and
        // the shader's contract is that only `shading()` may read it.
        uniform.vertical_exaggeration = exaggeration;
        assert_eq!(
            uniform.box_size_km, BOX_KM,
            "precondition: the uniform must carry the true extent"
        );

        let (origin, direction) = ray_for_pixel(&uniform, size[0] / 2, size[1] / 2, size);
        let (entry, exit) = slab(origin, direction, [0.0; 3], [1.0; 3]);
        let span = exit - entry.max(0.0);
        let km = |box_km: [f32; 3]| {
            let v = [
                direction[0] * span * box_km[0],
                direction[1] * span * box_km[1],
                direction[2] * span * box_km[2],
            ];
            f64::from((v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt())
        };
        let true_km = km(BOX_KM);
        let stretched_km = km([BOX_KM[0], BOX_KM[1], BOX_KM[2] * exaggeration]);
        let alpha_for =
            |path: f64| 1.0 - (-f64::from(LUT_ALPHA) / 255.0 * f64::from(EXTINCTION) * path).exp();

        let pixels = raymarch_once(
            &device, &queue, &pipelines, cells, &filled, &lut, &uniform, size,
        );
        let measured =
            f64::from(pixels[((size[1] / 2) * size[0] + size[0] / 2) as usize][3]) / 255.0;

        println!(
            "{exaggeration:>5}x, pitch {pitch}: centre ray crosses {true_km:.2} km of the \
             true box -> alpha {:.4}; measured {measured:.4}; had box_size_km been \
             stretched it would be {stretched_km:.2} km -> alpha {:.4}",
            alpha_for(true_km),
            alpha_for(stretched_km),
        );
        assert!(
            (measured - alpha_for(true_km)).abs() < 0.01,
            "at {exaggeration}x the centre alpha is {measured:.4}, not the \
             {:.4} the unexaggerated box_size_km predicts; a stretched one \
             would give {:.4}",
            alpha_for(true_km),
            alpha_for(stretched_km),
        );
    }
}

// ---------------------------------------------------------------------------
// 5. The two known residuals, measured
// ---------------------------------------------------------------------------

/// The linear filter bleeds half a cell past a sharp flat top. This measures
/// how much, in pixels and in kilometres.
#[test]
#[ignore = "needs a real wgpu adapter; see the doc comment for the invocation"]
fn the_linear_filter_bleeds_half_a_cell_past_a_sharp_top() {
    let _serialised = gpu_lock();
    let (device, queue) = device();
    let pipelines = VolumePipelines::new(&device, attachments());
    pipelines.upload_quad(&queue);
    let lut = hard_mask_lut();
    let size = [768u32, 768];
    // On a cell boundary at 128 and at 64 cells, so the rasterisation is exact
    // and every pixel of overshoot is the filter's.
    const TOP: f32 = 0.75;

    for box_km in [[40.0f32, 40.0, 20.0], [240.0, 240.0, 120.0]] {
        for cells in [FINE, COARSE] {
            let grid = plant(cells, |c| c[2] < TOP);
            // Close in and level, so the box's height spans most of the pane and
            // a fraction of a cell is a countable number of pixels.
            let uniform = masking_uniform(camera(180.0, 0.0, 1.6, 1.0), box_km, cells, size);
            let pixels = raymarch_once(
                &device, &queue, &pipelines, cells, &grid, &lut, &uniform, size,
            );
            let rendered = rendered_mask(&pixels);
            let expected = analytic_mask(&uniform, size, |o, d| {
                hits_box(o, d, [0.0, 0.0, 0.0], [1.0, 1.0, TOP])
            });

            // The centre column, so the geometry below describes the very ray
            // whose row is being read.
            let column = size[0] / 2;
            let top_row = |mask: &[bool]| {
                (0..size[1])
                    .find(|y| mask[(y * size[0] + column) as usize])
                    .expect("the slab paints in the centre column")
            };
            let rendered_top = top_row(&rendered);
            let analytic_top = top_row(&expected);

            // Out of pixels and into box z. Two places are worth naming:
            let places = |row: u32| {
                let (origin, direction) = ray_for_pixel(&uniform, column, row, size);
                let (entry, exit) = slab(origin, direction, [0.0; 3], [1.0; 3]);
                let entry = entry.max(0.0);
                let dt = march_dt(direction, cells, exit - entry);
                let at = |t: f32| origin[2] + direction[2] * t;
                (at(entry), at(entry + ign(column, row) * dt))
            };
            let (face_z, first_z) = places(rendered_top);
            let at_face = f64::from(face_z - TOP);
            let at_first = f64::from(first_z - TOP);
            let cells_z = f64::from(cells[2]);
            let rendered_count = rendered.iter().filter(|on| **on).count();
            let expected_count = expected.iter().filter(|on| **on).count();

            println!(
                "box {box_km:?} km, grid {cells:?}: centre-column top row rendered \
                 {rendered_top} vs analytic {analytic_top} = {} px of bleed. At the \
                 box's near face that is {at_face:.5} box-z = {:.3} cells = {:.4} km; \
                 at the ray's first sample {at_first:.5} box-z = {:.3} cells = \
                 {:.4} km (the threshold crossing is half a cell above the planted \
                 face). Silhouette grew {} px, {:+.3}% of its area.",
                analytic_top as i64 - rendered_top as i64,
                at_face * cells_z,
                at_face * f64::from(box_km[2]),
                at_first * cells_z,
                at_first * f64::from(box_km[2]),
                rendered_count as i64 - expected_count as i64,
                100.0 * (rendered_count as f64 - expected_count as f64) / expected_count as f64,
            );

            assert!(
                rendered_top <= analytic_top,
                "the render stopped *below* the analytic top edge, which the \
                 filter cannot do"
            );
            assert!(
                at_first > 0.0 && at_first * cells_z < 1.0,
                "the top edge bled {:.3} cells past the planted face, measured \
                 at the first sample; the trilinear threshold crossing sits half \
                 a cell above it and nothing should reach a whole one",
                at_first * cells_z,
            );
        }
    }

    // --- the edge does NOT move with the value at the edge
    let cells = FINE;
    let box_km = [40.0f32, 40.0, 20.0];
    let uniform = masking_uniform(camera(180.0, 0.0, 1.6, 1.0), box_km, cells, size);
    let column = size[0] / 2;
    let mut rows = Vec::new();
    for index in [1u8, 8, 64, 255] {
        let grid = plant_at(cells, index, |c| c[2] < TOP);
        let pixels = raymarch_once(
            &device, &queue, &pipelines, cells, &grid, &lut, &uniform, size,
        );
        let rendered = rendered_mask(&pixels);
        let top = (0..size[1])
            .find(|y| rendered[(y * size[0] + column) as usize])
            .expect("the slab paints");
        let (origin, direction) = ray_for_pixel(&uniform, column, top, size);
        let (entry, exit) = slab(origin, direction, [0.0; 3], [1.0; 3]);
        let entry = entry.max(0.0);
        let t = entry + ign(column, top) * march_dt(direction, cells, exit - entry);
        let overshoot = f64::from(origin[2] + direction[2] * t - TOP);
        println!(
            "edge index {index:>3}: top row {top}, first-sample overshoot {:.3} cells \
             = {:.4} km; the threshold crossing predicts {:.3} cells",
            overshoot * f64::from(cells[2]),
            overshoot * f64::from(box_km[2]),
            1.0 - 0.5 / f64::from(index) - 0.5,
        );
        rows.push(top);
    }
    // How much box z one image row is worth here, so the pixel figure can be
    // stated against the 0.998 − 0.5 = 0.498 of a cell the two crossings differ
    // by rather than left as a bare count.
    let z_at = |row: u32| {
        let (origin, direction) = ray_for_pixel(&uniform, column, row, size);
        let (entry, exit) = slab(origin, direction, [0.0; 3], [1.0; 3]);
        let entry = entry.max(0.0);
        let dt = march_dt(direction, cells, exit - entry);
        origin[2] + direction[2] * (entry + ign(column, row) * dt)
    };
    let per_row = f64::from((z_at(rows[0]) - z_at(rows[0] + 1)).abs());
    let faintest = rows[0];
    let strongest = *rows.last().expect("four rows");
    println!(
        "the silhouette's top edge moves {} px between a faint (index 1) and a \
         saturated (index 255) return on the very same planted surface; the \
         old index gate's 0.498-of-a-cell crossing difference predicted \
         {:.1} px, and coverage predicts 0",
        i64::from(faintest) - i64::from(strongest),
        0.498 / f64::from(cells[2]) / per_row,
    );
    assert!(
        rows.iter().all(|row| *row == rows[0]),
        "the silhouette's top edge sits at rows {rows:?} for indices 1, 8, 64, \
         255 of the same planted surface: the reach is reading the stored \
         value. Under a coverage gate it cannot — coverage is 1 at every \
         filled cell whatever its index — so this is the `R8Unorm` index \
         gate's behaviour back in the shader"
    );
}

/// What the voxel-locked march resolves, as two measurements rather than an
/// argument.
#[test]
#[ignore = "needs a real wgpu adapter; see the doc comment for the invocation"]
fn what_the_voxel_locked_march_resolves_of_a_thin_slab_and_a_fine_grid() {
    let _serialised = gpu_lock();
    let (device, queue) = device();
    let pipelines = VolumePipelines::new(&device, attachments());
    pipelines.upload_quad(&queue);
    let lut = hard_mask_lut();
    let size = [384u32, 384];
    let cells = FINE;
    let step = cell_size(cells);

    for (label, pitch) in [("near-nadir", 85.0f32), ("grazing", 4.0)] {
        for thickness in [1u32, 2] {
            // A horizontal layer `thickness` cells deep, on cell boundaries.
            let lo_z = 0.5f32;
            let hi_z = lo_z + thickness as f32 * step[2];
            let grid = plant(cells, |c| c[2] > lo_z && c[2] < hi_z);
            let uniform = masking_uniform(camera(225.0, pitch, 2.5, 1.0), BOX_KM, cells, size);

            let (origin, direction) = ray_for_pixel(&uniform, size[0] / 2, size[1] / 2, size);
            let (entry, exit) = slab(origin, direction, [0.0; 3], [1.0; 3]);
            let dt = march_dt(direction, cells, exit - entry.max(0.0));
            let chord = thickness as f32 * step[2] / direction[2].abs();

            let pixels = raymarch_once(
                &device, &queue, &pipelines, cells, &grid, &lut, &uniform, size,
            );
            let rendered = rendered_mask(&pixels);
            let exact = analytic_mask(&uniform, size, |o, d| {
                hits_box(o, d, [0.0, 0.0, lo_z], [1.0, 1.0, hi_z])
            });
            let dilated = analytic_mask(&uniform, size, |o, d| {
                hits_box(
                    o,
                    d,
                    [-step[0], -step[1], lo_z - step[2]],
                    [1.0 + step[0], 1.0 + step[1], hi_z + step[2]],
                )
            });
            let covered = |mask: &[bool]| {
                let hit = rendered
                    .iter()
                    .zip(mask)
                    .filter(|(r, m)| **r && **m)
                    .count();
                100.0 * hit as f64 / mask.iter().filter(|on| **on).count().max(1) as f64
            };

            println!(
                "{label} (pitch {pitch}), {thickness}-cell layer on {cells:?}: \
                 centre dt = {:.5} box units = {:.2} cells of z; the layer's chord \
                 is {:.5} = {:.2} steps; coverage {:.2}% of the exact mask, {:.2}% \
                 of the one-cell dilation",
                dt,
                dt / step[2],
                chord,
                chord / dt,
                covered(&exact),
                covered(&dilated),
            );
            assert!(
                covered(&exact) > 1.0,
                "a {thickness}-cell layer vanished entirely at pitch {pitch}"
            );
        }
    }

    // --- the sweep the 96-step march failed
    println!("a one-cell layer at increasing z resolution, seen near-nadir:");
    for depth in [128u32, 256, 512, 1024] {
        let cells = [64u32, 64, depth];
        let step = cell_size(cells);
        let lo_z = 0.5f32;
        let hi_z = lo_z + step[2];
        let grid = plant(cells, |c| c[2] > lo_z && c[2] < hi_z);
        let uniform = masking_uniform(camera(225.0, 85.0, 2.5, 1.0), BOX_KM, cells, size);
        let (origin, direction) = ray_for_pixel(&uniform, size[0] / 2, size[1] / 2, size);
        let (entry, exit) = slab(origin, direction, [0.0; 3], [1.0; 3]);
        let dt = march_dt(direction, cells, exit - entry.max(0.0));
        let pixels = raymarch_once(
            &device, &queue, &pipelines, cells, &grid, &lut, &uniform, size,
        );
        let rendered = rendered_mask(&pixels);
        let exact = analytic_mask(&uniform, size, |o, d| {
            hits_box(o, d, [0.0, 0.0, lo_z], [1.0, 1.0, hi_z])
        });
        let coverage = covered_fraction(&rendered, &exact);
        println!(
            "  {cells:?}: dt = {:.2} cells of z, the filtered layer is about 3 cells \
             = {:.2} steps; coverage {:.2}% of the exact mask",
            dt / step[2],
            3.0 * step[2] / dt,
            100.0 * coverage,
        );
        assert!(
            coverage > 0.99,
            "a one-cell layer at {depth} z-cells lost {:.2}% of its mask; the \
             voxel-locked step (with the ceiling's dt floor past 512) was \
             measured at 99.8%+ on every rung of this ladder",
            100.0 * (1.0 - coverage),
        );
    }

    // --- the same sphere at two resolutions
    const RADIUS: f32 = 0.3;
    let centre = [0.5f32; 3];
    println!("the same sphere, two grids:");
    for grid_cells in [FINE, COARSE] {
        let grid = plant(grid_cells, |c| {
            let d = [c[0] - centre[0], c[1] - centre[1], c[2] - centre[2]];
            (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt() < RADIUS
        });
        let uniform = masking_uniform(camera(225.0, 25.0, 2.5, 1.0), BOX_KM, grid_cells, size);
        let (origin, direction) = ray_for_pixel(&uniform, size[0] / 2, size[1] / 2, size);
        let (entry, exit) = slab(origin, direction, [0.0; 3], [1.0; 3]);
        let dt = march_dt(direction, grid_cells, exit - entry.max(0.0));
        let pixels = raymarch_once(
            &device, &queue, &pipelines, grid_cells, &grid, &lut, &uniform, size,
        );
        let rendered = rendered_mask(&pixels);
        let expected = analytic_mask(&uniform, size, |o, d| {
            hits_ellipsoid(o, d, centre, [RADIUS; 3])
        });
        let metrics = compare(&rendered, &expected, size);
        metrics.report(&format!(
            "{grid_cells:?}, dt = {:.2} x-cells",
            dt * grid_cells[0] as f32
        ));
    }
}
