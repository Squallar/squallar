//! The adapter, the readback and the planted fixtures every GPU test file
//! shares.
//!
//! Extracted from `volume_gpu.rs` when `volume_shader_mutants.rs` was written,
//! for the reason that file exists at all: the mutation battery has to drive
//! *the same* fixtures the shipped tests drive, or it would prove that some
//! other rendering can detect a broken shader. An integration test is its own
//! crate, so sharing means a module both files declare rather than an import.
//!
//! Every function here is used by at least one of them and `dead_code` is
//! allowed once, at the top: each test binary compiles its own copy of this
//! module and only calls the part it needs, so an unused-function warning here
//! would be a warning about the *other* file's usage.
#![allow(dead_code)]

use egui_wgpu::wgpu;
use rustdar_frontend::constants::VOLUME_LUT_BYTES;
use rustdar_frontend::egui_renderer::AttachmentConfig;
use rustdar_frontend::volume::raymarch::{FLOOR_FORMAT, PaneMirror, VolumePipelines};
use rustdar_frontend::volume::uniform::VolumeUniform;

/// Held for the length of a test, so only one talks to the GPU at a time.
///
/// Four concurrent devices each blocking in `poll(wait_indefinitely)` deadlock
/// reproducibly on this hardware, so serialising is a fix rather than a
/// workaround — and it costs nothing, because the whole file runs in about a
/// second.
static ONE_AT_A_TIME: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Take the GPU lock, ignoring poisoning.
///
/// Poisoning here means an earlier test already failed and unwound. That test
/// will report its own failure; refusing to run the rest would replace four
/// useful results with one and three panics about the mutex.
pub fn gpu_lock() -> std::sync::MutexGuard<'static, ()> {
    ONE_AT_A_TIME
        .lock()
        .unwrap_or_else(|held| held.into_inner())
}

/// Name the adapter these tests actually got, once per process.
///
/// Not decoration. CI runs these files against Mesa's lavapipe, and the failure
/// mode that would make that worthless is a silent fall back to whatever real
/// GPU the runner turns out to have: the suite would pass and prove nothing
/// about the software path. Under `--nocapture` this line is the receipt. It
/// goes to stderr so it survives a passing run's captured stdout.
pub fn announce(adapter: &wgpu::Adapter) {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let info = adapter.get_info();
        eprintln!(
            "wgpu adapter: {:?} {:?} \"{}\" (driver: {} {})",
            info.backend, info.device_type, info.name, info.driver, info.driver_info
        );
    });
}

/// A device on whatever adapter is to be had.
///
/// Same constructor the application uses, so `WGPU_BACKEND` selects the backend
/// here too.
pub fn device() -> (wgpu::Device, wgpu::Queue) {
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
        label: Some("rustdar.volume.test.device"),
        required_features: wgpu::Features::empty(),
        // Deliberately the adapter's own, not the WebGL2 floor: what is being
        // checked here is that the shader works, and holding a desktop GPU to
        // the browser's limits would only test the limits.
        required_limits: adapter.limits(),
        memory_hints: Default::default(),
        experimental_features: Default::default(),
        trace: Default::default(),
    }))
    .expect("could not create a device on an adapter that was found")
}

/// The egui pass a blit would be composited into, at one colour format.
pub fn attachments(color_format: wgpu::TextureFormat) -> AttachmentConfig {
    AttachmentConfig {
        color_format,
        depth_format: None,
        msaa_samples: 1,
    }
}

/// A texture that can be rendered into and read back.
pub fn render_target(
    device: &wgpu::Device,
    format: wgpu::TextureFormat,
    size: [u32; 2],
) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some("rustdar.volume.test.target"),
        size: wgpu::Extent3d {
            width: size[0],
            height: size[1],
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    })
}

/// Read an RGBA8 texture back as one `[u8; 4]` per texel, row-major.
///
/// `copy_texture_to_buffer` wants rows padded to
/// `COPY_BYTES_PER_ROW_ALIGNMENT`, so the padding is added on the way out and
/// stripped on the way back — getting that wrong shears the image, which is
/// exactly the kind of thing that looks like a shader bug.
pub fn read_back(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    texture: &wgpu::Texture,
    size: [u32; 2],
) -> Vec<[u8; 4]> {
    let unpadded = size[0] * 4;
    let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let padded = unpadded.div_ceil(align) * align;

    let staging = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("rustdar.volume.test.readback"),
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

/// A `box_from_clip` that unprojects the far plane onto the far face of the
/// box, looking down one axis.
///
/// `axis` is which box axis the ray travels along, and the camera sits on its
/// positive side. Column-major, because that is what `VolumeUniform` packs and
/// what WGSL's `mat4x4` is.
pub fn box_from_clip_down(axis: usize) -> [[f32; 4]; 4] {
    // The two axes the screen spans, in order.
    let screen: [usize; 2] = match axis {
        0 => [1, 2],
        1 => [0, 2],
        _ => [0, 1],
    };
    let mut matrix = [[0.0f32; 4]; 4];
    // ndc.x and ndc.y map [-1, 1] onto [0, 1] of the two screen axes.
    matrix[0][screen[0]] = 0.5;
    matrix[1][screen[1]] = 0.5;
    matrix[3][screen[0]] = 0.5;
    matrix[3][screen[1]] = 0.5;
    // Depth 1 (the far plane) lands one box beyond the far face, so a ray from
    // an eye outside the near face crosses the whole box.
    matrix[2][axis] = -2.5;
    matrix[3][axis] = 1.5;
    matrix[3][3] = 1.0;
    matrix
}

/// The eye that goes with [`box_from_clip_down`]: outside the near face.
pub fn eye_outside(axis: usize) -> [f32; 3] {
    let mut eye = [0.5f32; 3];
    eye[axis] = 3.0;
    eye
}

/// A palette where one entry is `colour` and everything else is transparent.
pub fn palette(index: u8, colour: [u8; 4]) -> Vec<u8> {
    let mut lut = vec![0u8; VOLUME_LUT_BYTES];
    let at = index as usize * 4;
    lut[at..at + 4].copy_from_slice(&colour);
    lut
}

/// A palette that is opaque white at every index but the no-data 0.
pub fn opaque_white_lut() -> Vec<u8> {
    let mut lut = vec![0u8; VOLUME_LUT_BYTES];
    for entry in 1..lut.len() / 4 {
        lut[entry * 4..entry * 4 + 4].copy_from_slice(&[255, 255, 255, 255]);
    }
    lut
}

/// A grey ramp table: entry `i` is the colour `(i, i, i)`, opaque.
///
/// With [`VolumeUniform::ambient`] at 1 the isosurface's half-Lambert wrap
/// collapses to exactly 1, so a pixel's grey level *is* the palette index the
/// surface was found at, to within the 8-bit round trip. That is what turns
/// "where did the surface land" into something a test can read off a pixel.
pub fn grey_ramp_lut() -> Vec<u8> {
    let mut lut = vec![0u8; VOLUME_LUT_BYTES];
    for (i, entry) in lut.chunks_exact_mut(4).enumerate() {
        let level = i as u8;
        entry.copy_from_slice(&[level, level, level, 255]);
    }
    // Entry 0 is the no-data index and is transparent in every real table.
    lut[0..4].copy_from_slice(&[0, 0, 0, 0]);
    lut
}

/// An `8 x 8 x nz` grid whose index depends only on the slab: `levels[k]` is
/// the value of every cell in slab `k`.
///
/// The eye of [`eye_outside`]`(2)` is above the box, so a ray descends through
/// slab `nz-1` first and slab 0 last.
pub fn slab_ramp(levels: &[u8]) -> ([u32; 3], Vec<u8>) {
    let cells = [8u32, 8, levels.len() as u32];
    let mut indices = Vec::with_capacity((cells[0] * cells[1] * cells[2]) as usize);
    for level in levels {
        indices.extend(std::iter::repeat_n(*level, (cells[0] * cells[1]) as usize));
    }
    (cells, indices)
}

/// A uniform ready for an isosurface measurement: ambient light only, so
/// shading is exactly 1, and no index band skipped.
pub fn iso_uniform(cells: [u32; 3]) -> VolumeUniform {
    let mut uniform = VolumeUniform::new([10.0, 10.0, 10.0], cells);
    uniform.box_from_clip = box_from_clip_down(2);
    uniform.eye_in_box = eye_outside(2);
    uniform.ambient = 1.0;
    uniform.empty_index_threshold = 0.5 / 255.0;
    uniform
}

/// Render one raymarched frame and read it back.
#[allow(clippy::too_many_arguments)]
pub fn raymarch_once(
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
        .upload_volume(device, queue, cells, indices, lut)
        .expect("the grid and palette were refused");
    assert_eq!(
        volume.cells(),
        cells,
        "the uploaded grid does not report the shape it was given, so the \
         uniform block's grid_dims would describe a different texture"
    );
    volume.write_uniform(queue, uniform);
    let target = pipelines.create_offscreen(device, size);
    assert_eq!(
        target.size(),
        size,
        "the offscreen does not report the size it was created at"
    );

    let mut encoder = device.create_command_encoder(&Default::default());
    pipelines.encode_raymarch(&mut encoder, &target, &volume);
    queue.submit(Some(encoder.finish()));

    read_back(device, queue, target.texture(), size)
}

/// The texel format every mirror built here is created in.
///
/// [`FLOOR_FORMAT`] is what production's own placeholder mirror uses, and being
/// `Rgba8Unorm` it takes the bytes `write_mirror` is handed in the order they
/// were written. That is worth stating rather than assuming: these tests build
/// their *pipelines* for a `Bgra8Unorm` swapchain, and a mirror in that format
/// would read the very same fixture bytes as BGRA and turn every red patch
/// blue, with no validation error anywhere to say so.
pub const MIRROR_FORMAT: wgpu::TextureFormat = FLOOR_FORMAT;

/// A mirror of `size` texels holding `rgba`, through the very same
/// `ensure_mirror` texture and bind group the frame path draws into.
///
/// `rgba` is premultiplied, row 0 at the top, in the mirror's own encoding —
/// which is a change from the floor image this replaced, whose bytes were
/// straight. Every fixture in these suites is fully opaque, and a fully opaque
/// colour is the same four bytes premultiplied or straight, so the fixtures
/// read as plain colours and the distinction costs nothing here. It would not
/// be free for a translucent one.
pub fn planted_mirror(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    pipelines: &VolumePipelines,
    size: [u32; 2],
    rgba: &[u8],
) -> PaneMirror {
    let mut mirror = None;
    assert!(
        pipelines.ensure_mirror(device, &mut mirror, size, MIRROR_FORMAT),
        "ensure_mirror declined to create a mirror where there was none",
    );
    let mirror = mirror.expect("ensure_mirror reported a creation and left nothing behind");
    assert_eq!(
        mirror.size(),
        size,
        "the mirror is not the size it was asked for"
    );
    assert!(
        pipelines.write_mirror(queue, &mirror, rgba),
        "write_mirror refused a fixture of {} bytes for a {size:?} mirror",
        rgba.len(),
    );
    mirror
}

/// [`raymarch_once`], with a pane mirror bound at group 1 for the floor.
#[allow(clippy::too_many_arguments)]
pub fn raymarch_once_with_floor(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    pipelines: &VolumePipelines,
    cells: [u32; 3],
    indices: &[u8],
    lut: &[u8],
    uniform: &VolumeUniform,
    size: [u32; 2],
    floor: &PaneMirror,
) -> Vec<[u8; 4]> {
    let volume = pipelines
        .upload_volume(device, queue, cells, indices, lut)
        .expect("the grid and palette were refused");
    volume.write_uniform(queue, uniform);
    let target = pipelines.create_offscreen(device, size);
    let mut encoder = device.create_command_encoder(&Default::default());
    pipelines.encode_raymarch_with_floor(&mut encoder, &target, &volume, Some(floor));
    queue.submit(Some(encoder.finish()));
    read_back(device, queue, target.texture(), size)
}

/// The pixel at the centre of a `size`-shaped image.
pub fn centre(pixels: &[[u8; 4]], size: [u32; 2]) -> [u8; 4] {
    pixels[((size[1] / 2) * size[0] + size[0] / 2) as usize]
}

/// The box side that spans exactly one degree of latitude, in kilometres —
/// `KM_PER_DEGREE_LAT`, as `volume.wgsl` and `ImageBounds` both spell it.
pub const DEGREE_BOX_KM: f32 = 111.32;

/// Web Mercator's y at a latitude in degrees: `ln(tan(pi/4 + phi/2))`.
///
/// The projection's own definition, and line for line what the shader's
/// `mercator_y` evaluates. Restated here rather than tabulated as a magic
/// number so [`equatorial_floor_lanes`] is derived from the same closed form
/// the thing under test uses — a constant copied out of a calculator would
/// keep agreeing with a shader that had changed.
pub fn mercator_y(lat_deg: f64) -> f64 {
    (std::f64::consts::FRAC_PI_4 + lat_deg.to_radians() / 2.0)
        .tan()
        .ln()
}

/// The uniform's two floor lanes for a `DEGREE_BOX_KM`-square box whose site is
/// at its centre **on the equator**, arranged so the box's footprint covers
/// exactly the whole mirror with the mirror's row 0 along the box's north edge.
///
/// # Why this is arranged rather than assumed
///
/// The mirror is a Web Mercator picture of the whole frame, not a picture of
/// the box, so `floor_colour` reprojects into it per pixel — out to geography
/// in kilometres east and north of the site, and back through longitude and
/// Mercator y, taking `cos φ` at *this pixel's* latitude. There is no longer
/// any `(hit.x, 1 - hit.y)` texture lookup for a fixture to lean on. The
/// orientation and registration cases below still want that simple
/// correspondence to hold, so it is *established by the lanes* instead of
/// assumed by the shader: on the equator `cos φ` is 1 and Mercator y is very
/// nearly linear in latitude, and a box one degree on a side then maps onto the
/// unit square of the mirror to within a rounding error. What those tests
/// assert therefore becomes a check on the reprojection rather than on a
/// texture lookup, which is strictly more than the old fixtures could say.
///
/// # The residual, and why it is legitimate rather than a fudge
///
/// `cos φ` runs from 1 at the site to 0.999962 at ±0.5°, so `u` departs from
/// `hit.x` by at most 1.9e-5 — 1.5e-4 of a texel on an eight-texel mirror and
/// 1.2e-3 of a texel on a sixty-four-texel one. Mercator's y is odd and cubic
/// in latitude about the equator, so `v` departs from `1 - hit.y` by at most
/// 2.4e-6, an order of magnitude smaller again. Both are far under the
/// sub-texel wobble the floor's `Linear` sampler already has, and neither can
/// move a centroid by a hundredth of the pixel bounds asserted below. The
/// trapezoid the shader exists to correct is real on the shipped 460 km box at
/// 41.7°N — 8.5 texels of 512 — and is deliberately absent here.
///
/// Returns `(floor_uv, floor_geo)`. `gamma_encoded` is the mirror's own
/// [`PaneMirror::is_gamma_encoded`], which is what the shader is being told
/// about the texels it is sampling.
pub fn equatorial_floor_lanes(gamma_encoded: bool) -> ([f32; 4], [f32; 4]) {
    // v grows downward through the mirror and Mercator y grows north, so the
    // rate is negative; its magnitude is one whole mirror over the Mercator
    // span of the box's one degree of latitude. Derived from `mercator_y`
    // rather than written down: it comes out at -57.29505, and a reader who
    // wants to know why *that* number should be able to see the two calls it
    // came from.
    let v_per_mercator_y = -1.0 / (mercator_y(0.5) - mercator_y(-0.5));
    (
        // u at the site, v at the site, u per degree of longitude east, v per
        // unit of Mercator y. The site is the mirror's centre, and one degree
        // of longitude — the box's full width at the equator — is one whole
        // mirror across.
        [0.5, 0.5, 1.0, v_per_mercator_y as f32],
        // Site latitude, then the box's west and south edges as kilometres
        // east and north of it: the site is the box's centre, so both are half
        // a side to the negative.
        [
            0.0,
            -DEGREE_BOX_KM / 2.0,
            -DEGREE_BOX_KM / 2.0,
            if gamma_encoded { 1.0 } else { 0.0 },
        ],
    )
}

/// The box extent [`equatorial_floor_lanes`] is written for.
///
/// Only the two horizontal axes take part in the reprojection. The vertical is
/// left at the 10 km these tests always used, so the march's optical depth
/// through a down- or up-looking camera — which is what every opacity assertion
/// here turns on — is exactly what it was before the floor grew a projection.
pub const fn equatorial_box_km() -> [f32; 3] {
    [DEGREE_BOX_KM, DEGREE_BOX_KM, 10.0]
}
