//! What only a real GPU can say about the volume raymarch.
//!
//! Everything here is `#[ignore]`d and every test carries its own invocation.
//! The attribute is about *this machine*, not about CI: a checkout on a box
//! with no working Vulkan loader must still give a green `cargo test`, so the
//! whole file is opt-in and nobody has to own a GPU to contribute.
//!
//! **CI opts in.** The `gpu` job in `test.yaml` installs `mesa-vulkan-drivers`,
//! points the Vulkan loader at Mesa's lavapipe and runs this file with
//! `-- --ignored`, so every test below is executed on every PR against a
//! software rasteriser. Nothing here is any longer protected by hand alone.
//!
//! That the tests pass on llvmpipe is not incidental — it is checked, and it is
//! the reason the job can exist on a runner with no graphics hardware. The
//! adapter is named once per process on stderr, which is what `--nocapture` in
//! that job is for: an adapter that quietly turned out to be a real GPU would
//! leave the job green having tested something else entirely.
//!
//! Run the lot with:
//!
//! ```text
//! cargo test -p rustdar-frontend --test volume_gpu -- --ignored --nocapture
//! ```
//!
//! **These tests hold a process-wide lock and therefore run one at a time**,
//! whatever `--test-threads` says. Four of them creating four devices on one
//! adapter and each blocking in `poll(wait_indefinitely)` deadlocked
//! reproducibly on this box; serialising them is a fix rather than a
//! workaround, and it costs nothing because the whole file runs in about a
//! second.
//!
//! Serialised rather than sharing one device, because
//! `the_pipelines_build_on_a_real_device` pushes an error scope, and error
//! scopes are a per-device stack — a concurrent test's error would land inside
//! it and be reported against the wrong thing.
//!
//! Four things are checked, and each is here because no host test can reach it:
//!
//! 1. **The pipelines build.** `create_render_pipeline` returns no `Result`, so
//!    a shader a driver refuses surfaces asynchronously — which is why the
//!    error scope, not the absence of a panic, is what is asserted.
//! 2. **The march composites what the palette says.** A uniform grid must paint
//!    its palette entry's own colour back out, which is the end-to-end check
//!    that the decode/accumulate/encode round trip is a round trip.
//! 3. **Opacity is per kilometre, not per box diagonal.** Spike 0a's first bug,
//!    as a property rather than a source scan.
//! 4. **The blit matches egui exactly, on both surface colour spaces.** Spike
//!    0a's second bug. This is the measurement the counter-intuitive sRGB rule
//!    rests on, and it is the only thing that can distinguish the rule from the
//!    colour-theoretically correct version that measured 60/255 away from it.
#![cfg(not(target_arch = "wasm32"))]

use egui_wgpu::wgpu;
use rustdar_frontend::constants::VOLUME_LUT_BYTES;
use rustdar_frontend::egui_renderer::AttachmentConfig;
use rustdar_frontend::volume::raymarch::{
    ENTRY_FS_BLIT_GAMMA, ENTRY_FS_BLIT_LINEAR, OffscreenTarget, VolumePipelines,
};
use rustdar_frontend::volume::uniform::{ISO_OFF, VolumeUniform};

/// Open a pass that clears to opaque black, which is what `EguiRenderer::draw`
/// does.
///
/// A macro rather than a function because `RenderPassDescriptor`'s
/// `color_attachments` borrows a slice, and a function returning the descriptor
/// would be returning a reference to its own temporary.
macro_rules! clearing_pass {
    ($encoder:expr, $view:expr) => {
        $encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("rustdar.volume.test.pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: $view,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                    store: wgpu::StoreOp::Store,
                },
                depth_slice: None,
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        })
    };
}

/// Held for the length of a test, so only one talks to the GPU at a time.
///
/// See the module doc: four concurrent devices each blocking in
/// `poll(wait_indefinitely)` deadlock on this hardware.
static ONE_AT_A_TIME: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Take the GPU lock, ignoring poisoning.
///
/// Poisoning here means an earlier test already failed and unwound. That test
/// will report its own failure; refusing to run the rest would replace four
/// useful results with one and three panics about the mutex.
fn gpu_lock() -> std::sync::MutexGuard<'static, ()> {
    ONE_AT_A_TIME
        .lock()
        .unwrap_or_else(|held| held.into_inner())
}

/// Name the adapter these tests actually got, once per process.
///
/// Not decoration. CI runs this file against Mesa's lavapipe, and the failure
/// mode that would make that worthless is a silent fall back to whatever real
/// GPU the runner turns out to have: the suite would pass and prove nothing
/// about the software path. Under `--nocapture` this line is the receipt. It
/// goes to stderr so it survives a passing run's captured stdout.
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

/// A device, or `None` when there is no adapter to be had.
///
/// Same constructor the application uses, so `WGPU_BACKEND` selects the backend
/// here too.
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
fn attachments(color_format: wgpu::TextureFormat) -> AttachmentConfig {
    AttachmentConfig {
        color_format,
        depth_format: None,
        msaa_samples: 1,
    }
}

/// A texture that can be rendered into and read back.
fn render_target(
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
fn box_from_clip_down(axis: usize) -> [[f32; 4]; 4] {
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
fn eye_outside(axis: usize) -> [f32; 3] {
    let mut eye = [0.5f32; 3];
    eye[axis] = 3.0;
    eye
}

/// A palette where one entry is `colour` and everything else is transparent.
fn palette(index: u8, colour: [u8; 4]) -> Vec<u8> {
    let mut lut = vec![0u8; VOLUME_LUT_BYTES];
    let at = index as usize * 4;
    lut[at..at + 4].copy_from_slice(&colour);
    lut
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

/// [`raymarch_once`], with a floor bound at group 1.
#[allow(clippy::too_many_arguments)]
fn raymarch_once_with_floor(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    pipelines: &VolumePipelines,
    cells: [u32; 3],
    indices: &[u8],
    lut: &[u8],
    uniform: &VolumeUniform,
    size: [u32; 2],
    floor: &rustdar_frontend::volume::raymarch::FloorTexture,
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
fn centre(pixels: &[[u8; 4]], size: [u32; 2]) -> [u8; 4] {
    pixels[((size[1] / 2) * size[0] + size[0] / 2) as usize]
}

/// Both pipelines build, on both surface colour spaces, with no device error.
///
/// The assertion is on the error scope rather than on the absence of a panic:
/// `create_render_pipeline` returns no `Result`, and its errors arrive through
/// the uncaptured sink, which in a plain test binary would be a panic on some
/// other thread or nothing at all.
///
/// ```text
/// cargo test -p rustdar-frontend --test volume_gpu \
///     the_pipelines_build_on_a_real_device -- --ignored --exact --nocapture
/// ```
#[test]
#[ignore = "needs a real wgpu adapter; see the doc comment for the invocation"]
fn the_pipelines_build_on_a_real_device() {
    let _serialised = gpu_lock();
    let (device, queue) = device();

    for format in [
        wgpu::TextureFormat::Bgra8Unorm,
        wgpu::TextureFormat::Rgba8UnormSrgb,
    ] {
        let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
        let pipelines = VolumePipelines::new(&device, attachments(format));
        pipelines.upload_quad(&queue);
        let error = pollster::block_on(scope.pop());
        assert!(
            error.is_none(),
            "building the volume pipelines for a {format:?} surface failed: {}",
            error.map(|e| e.to_string()).unwrap_or_default()
        );

        let expected = if format.is_srgb() {
            ENTRY_FS_BLIT_LINEAR
        } else {
            ENTRY_FS_BLIT_GAMMA
        };
        assert_eq!(pipelines.blit_entry_point(), expected);
    }
}

/// An offscreen is reused at the same size and rebuilt at a new one.
///
/// `ensure_offscreen` needs a device, so no host test can reach it — and its
/// two failure modes are both quiet. Always rebuilding churns a pane-sized
/// texture at the frame rate, which looks like a driver problem rather than an
/// application one. Never rebuilding blits a stale texture at the wrong scale
/// after a resize, which looks like a camera bug.
///
/// ```text
/// cargo test -p rustdar-frontend --test volume_gpu \
///     an_offscreen_is_reused_at_one_size_and_rebuilt_at_another \
///     -- --ignored --exact --nocapture
/// ```
#[test]
#[ignore = "needs a real wgpu adapter; see the doc comment for the invocation"]
fn an_offscreen_is_reused_at_one_size_and_rebuilt_at_another() {
    let _serialised = gpu_lock();
    let (device, queue) = device();
    let pipelines = VolumePipelines::new(&device, attachments(wgpu::TextureFormat::Bgra8Unorm));
    pipelines.upload_quad(&queue);

    let mut held: Option<OffscreenTarget> = None;
    assert!(
        pipelines.ensure_offscreen(&device, &mut held, [1440, 900]),
        "nothing held must be built"
    );
    assert_eq!(held.as_ref().map(OffscreenTarget::size), Some([1440, 900]));

    assert!(
        !pipelines.ensure_offscreen(&device, &mut held, [1440, 900]),
        "an offscreen of exactly the right size was thrown away and rebuilt, \
         which is a pane-sized allocation on every frame"
    );
    assert_eq!(held.as_ref().map(OffscreenTarget::size), Some([1440, 900]));

    assert!(
        pipelines.ensure_offscreen(&device, &mut held, [720, 450]),
        "a resized pane reused its old offscreen, so the blit would upscale \
         the wrong texture"
    );
    assert_eq!(held.as_ref().map(OffscreenTarget::size), Some([720, 450]));

    // A pane dragged to nothing: the clamp is what stops `create_texture`
    // refusing a zero extent, from a call with no `Result`.
    assert!(pipelines.ensure_offscreen(&device, &mut held, [0, 0]));
    assert_eq!(held.as_ref().map(OffscreenTarget::size), Some([1, 1]));
}

/// A grid of one palette index paints that entry's own colour back out.
///
/// The end-to-end check on the colour round trip: the shader decodes the
/// table's gamma-encoded entry to linear, accumulates, un-premultiplies,
/// re-encodes and re-premultiplies. For a constant colour every one of those
/// steps has to cancel exactly, so anything but the original bytes back is a
/// broken conversion — and a broken conversion is a volume that is merely a bit
/// dark, which nobody would report as a bug.
///
/// Also checks the empty-cell skip in the same shape: an all-zero grid must
/// come back fully transparent rather than fully black.
///
/// ```text
/// cargo test -p rustdar-frontend --test volume_gpu \
///     a_uniform_grid_paints_its_palette_colour -- --ignored --exact --nocapture
/// ```
#[test]
#[ignore = "needs a real wgpu adapter; see the doc comment for the invocation"]
fn a_uniform_grid_paints_its_palette_colour() {
    let _serialised = gpu_lock();
    const INDEX: u8 = 200;
    const COLOUR: [u8; 4] = [200, 60, 30, 255];
    let size = [64, 64];
    let cells = [8u32, 8, 8];

    let (device, queue) = device();
    let pipelines = VolumePipelines::new(&device, attachments(wgpu::TextureFormat::Bgra8Unorm));
    pipelines.upload_quad(&queue);

    let mut uniform = VolumeUniform::new([10.0, 10.0, 10.0], cells);
    uniform.box_from_clip = box_from_clip_down(2);
    uniform.eye_in_box = eye_outside(2);
    // Enough extinction that a 10 km path is opaque, so the colour is the
    // table's own rather than a blend with the transparent background.
    uniform.extinction_per_km = 1.0;

    let filled = vec![INDEX; (cells[0] * cells[1] * cells[2]) as usize];
    let lut = palette(INDEX, COLOUR);

    for gradient_shading in [false, true] {
        uniform.gradient_shading = gradient_shading;
        let pixels = raymarch_once(
            &device, &queue, &pipelines, cells, &filled, &lut, &uniform, size,
        );
        let painted = centre(&pixels, size);

        assert!(
            painted[3] >= 253,
            "a 10 km path through a fully opaque palette entry came back at \
             alpha {} with gradient_shading={gradient_shading}",
            painted[3]
        );
        for channel in 0..3 {
            let delta = i32::from(painted[channel]) - i32::from(COLOUR[channel]);
            assert!(
                delta.abs() <= 2,
                "channel {channel} came back {} against the table's {} \
                 (gradient_shading={gradient_shading}); the decode/encode round \
                 trip is not a round trip",
                painted[channel],
                COLOUR[channel]
            );
        }
    }

    // The other half: nothing at all, rather than black.
    uniform.gradient_shading = false;
    let empty = vec![0u8; (cells[0] * cells[1] * cells[2]) as usize];
    let pixels = raymarch_once(
        &device, &queue, &pipelines, cells, &empty, &lut, &uniform, size,
    );
    assert_eq!(
        centre(&pixels, size),
        [0, 0, 0, 0],
        "an all-index-0 grid painted something. Index 0 is the bottom of the \
         ramp and the no-data value, so it must contribute nothing — an opaque \
         black box would hide every pane behind it."
    );
}

/// The isosurface mode paints one opaque, lit surface at the threshold, and
/// it reads the DATA, not the table's alpha.
///
/// The discriminating fixture: the filled grid's palette entry has **zero
/// alpha**. The lit volume renders it as nothing at all — every absorbed
/// contribution is scaled by the entry's alpha — while the isosurface must
/// still paint an opaque, lit version of the entry's colour, because its
/// threshold reads the interpolated index and its surface is opaque by
/// construction. That is the "threshold reads the data, not the curve"
/// doctrine, run on the GPU: a Volume Alpha curve (which rewrites exactly
/// this alpha channel) can strip the lit volume to nothing and the
/// isosurface must not move.
///
/// Lighting is asserted as a bound, not a value: the surface colour is the
/// entry's times the half-Lambert wrap, which lives in
/// `[ambient, 1] = [0.35, 1]` of the decoded colour.
///
/// ```text
/// cargo test -p rustdar-frontend --test volume_gpu \
///     an_isosurface_paints_an_opaque_lit_surface_from_the_data_alone \
///     -- --ignored --exact --nocapture
/// ```
#[test]
#[ignore = "needs a real wgpu adapter; see the doc comment for the invocation"]
fn an_isosurface_paints_an_opaque_lit_surface_from_the_data_alone() {
    let _serialised = gpu_lock();
    const INDEX: u8 = 200;
    // Zero alpha on purpose — see the doc comment.
    const COLOUR: [u8; 4] = [200, 60, 30, 0];
    let size = [64, 64];
    let cells = [8u32, 8, 8];

    let (device, queue) = device();
    let pipelines = VolumePipelines::new(&device, attachments(wgpu::TextureFormat::Bgra8Unorm));
    pipelines.upload_quad(&queue);

    let mut uniform = VolumeUniform::new([10.0, 10.0, 10.0], cells);
    uniform.box_from_clip = box_from_clip_down(2);
    uniform.eye_in_box = eye_outside(2);
    uniform.extinction_per_km = 1.0;

    let filled = vec![INDEX; (cells[0] * cells[1] * cells[2]) as usize];
    let lut = palette(INDEX, COLOUR);

    // Lit volume over a zero-alpha entry: nothing at all.
    uniform.iso_threshold = rustdar_frontend::volume::uniform::ISO_OFF;
    let pixels = raymarch_once(
        &device, &queue, &pipelines, cells, &filled, &lut, &uniform, size,
    );
    assert_eq!(
        centre(&pixels, size),
        [0, 0, 0, 0],
        "the lit volume painted a zero-alpha entry; the discriminator is dead",
    );

    // Isosurface at a threshold under the filled index: an opaque, lit
    // surface, whatever the table's alpha says.
    uniform.iso_threshold = 150.0 / 255.0;
    uniform.iso_centre = -1.0;
    let pixels = raymarch_once(
        &device, &queue, &pipelines, cells, &filled, &lut, &uniform, size,
    );
    let painted = centre(&pixels, size);
    assert_eq!(
        painted[3], 255,
        "an isosurface hit must be fully opaque, got alpha {}",
        painted[3],
    );
    for channel in 0..3 {
        let full = f64::from(COLOUR[channel]);
        let got = f64::from(painted[channel]);
        // Gamma-space bound of the linear [0.35, 1] lighting window, with a
        // couple of counts of slack for the 8-bit round trips.
        let floor = 255.0
            * (full / 255.0f64)
                .powf(2.2)
                .mul_add(0.33, 0.0)
                .powf(1.0 / 2.2)
            - 3.0;
        assert!(
            got >= floor.max(0.0) && got <= full + 3.0,
            "channel {channel} came back {got} against the entry's {full}: \
             outside the lit window [{floor:.0}, {full}]",
        );
    }

    // And a threshold above the filled index finds no surface at all.
    uniform.iso_threshold = 220.0 / 255.0;
    let pixels = raymarch_once(
        &device, &queue, &pipelines, cells, &filled, &lut, &uniform, size,
    );
    assert_eq!(
        centre(&pixels, size),
        [0, 0, 0, 0],
        "a threshold above every value in the grid still painted a surface",
    );
}

/// A grey ramp table: entry `i` is the colour `(i, i, i)`, opaque.
///
/// With [`VolumeUniform::ambient`] at 1 the isosurface's half-Lambert wrap
/// collapses to exactly 1, so a pixel's grey level *is* the palette index the
/// surface was found at, to within the 8-bit round trip. That is what turns
/// "where did the surface land" into something a test can read off a pixel.
fn grey_ramp_lut() -> Vec<u8> {
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
fn slab_ramp(levels: &[u8]) -> ([u32; 3], Vec<u8>) {
    let cells = [8u32, 8, levels.len() as u32];
    let mut indices = Vec::with_capacity((cells[0] * cells[1] * cells[2]) as usize);
    for level in levels {
        indices.extend(std::iter::repeat_n(*level, (cells[0] * cells[1]) as usize));
    }
    (cells, indices)
}

/// A uniform ready for an isosurface measurement: ambient light only, so
/// shading is exactly 1, and no index band skipped.
fn iso_uniform(cells: [u32; 3]) -> VolumeUniform {
    let mut uniform = VolumeUniform::new([10.0, 10.0, 10.0], cells);
    uniform.box_from_clip = box_from_clip_down(2);
    uniform.eye_in_box = eye_outside(2);
    uniform.ambient = 1.0;
    uniform.empty_index_threshold = 0.5 / 255.0;
    uniform
}

/// Every grey level in the middle of the image, as `(min, max)`.
fn grey_span(pixels: &[[u8; 4]], size: [u32; 2]) -> (u8, u8) {
    let (mut lo, mut hi) = (u8::MAX, u8::MIN);
    for y in size[1] / 4..size[1] * 3 / 4 {
        for x in size[0] / 4..size[0] * 3 / 4 {
            let p = pixels[(y * size[0] + x) as usize];
            assert_eq!(p[3], 255, "the isosurface must be opaque at ({x}, {y})");
            lo = lo.min(p[0]);
            hi = hi.max(p[0]);
        }
    }
    (lo, hi)
}

/// The isosurface sits where the value crosses the threshold, not where the
/// sample comb happened to notice — which is what `refine_iso_hit`'s bisection
/// is for, and what the one shipped isosurface test could not see.
///
/// That test fills the box uniformly, so its only crossing is the box's own
/// entry face and `refine_iso_hit` is handed a degenerate interval: replacing
/// its whole body with `return t_hi_in` passed 149/149 host, 11/11 GPU and
/// 10/10 silhouette tests.
///
/// The fixture here is a graded field — four slabs whose index rises along the
/// ray — read through a grey ramp table under ambient-only light, so a pixel's
/// grey level *is* the index the surface was found at. Two things then follow,
/// and each is a different half of the bug:
///
/// * the level equals the threshold, because that is where the field crosses
///   it; and
/// * every pixel agrees, because the sample comb is **jittered per pixel**
///   (`interleaved_gradient_noise`), so an unrefined hit lands wherever that
///   pixel's stratum fell and the surface comes back as speckle spanning a
///   whole slab of index.
///
/// ```text
/// cargo test -p rustdar-frontend --test volume_gpu \
///     an_isosurface_sits_where_the_value_crosses_not_where_the_comb_noticed \
///     -- --ignored --exact --nocapture
/// ```
#[test]
#[ignore = "needs a real wgpu adapter; see the doc comment for the invocation"]
fn an_isosurface_sits_where_the_value_crosses_not_where_the_comb_noticed() {
    let _serialised = gpu_lock();
    let size = [64, 64];
    // Slab 3 is met first (index 40) and slab 0 last (index 208): 56 index
    // units per slab, which is the amplitude of the speckle an unrefined hit
    // would produce.
    let (cells, indices) = slab_ramp(&[208, 152, 96, 40]);
    let lut = grey_ramp_lut();

    let (device, queue) = device();
    let pipelines = VolumePipelines::new(&device, attachments(wgpu::TextureFormat::Bgra8Unorm));
    pipelines.upload_quad(&queue);

    let mut uniform = iso_uniform(cells);
    uniform.iso_centre = ISO_OFF;
    // A threshold three tenths of the way from slab 2's 96 to slab 1's 152, so
    // the crossing sits well inside a step and cannot coincide with the comb.
    const THRESHOLD: u8 = 113;
    uniform.iso_threshold = f32::from(THRESHOLD) / 255.0;

    let pixels = raymarch_once(
        &device, &queue, &pipelines, cells, &indices, &lut, &uniform, size,
    );
    let (lo, hi) = grey_span(&pixels, size);
    assert!(
        u32::from(hi) - u32::from(lo) <= 4,
        "the surface came back as speckle spanning [{lo}, {hi}] of grey: the \
         hit was taken at whatever jittered sample noticed the crossing, not \
         at the crossing",
    );
    let level = i32::from(lo) + (i32::from(hi) - i32::from(lo)) / 2;
    assert!(
        (level - i32::from(THRESHOLD)).abs() <= 4,
        "the surface is drawn at index {level}, where the field crosses the \
         threshold at {THRESHOLD}",
    );
}

/// A diverging isosurface is the level set of the **deviation** from its
/// centre, so it draws both lobes — which is what `iso_field`'s fold is for,
/// and what nothing measured.
///
/// Deleting that fold (`return index`) turned every diverging surface into a
/// sequential one and passed the whole suite: the only isosurface test filled
/// its box uniformly and set `iso_centre` to the sequential sentinel, so the
/// fold was never taken.
///
/// Both fixtures start at the centre index at the box's near face and ramp
/// *away* from it, one downward and one upward. Under the fold each finds its
/// own lobe, at its own index, on its own side. Without it both collapse to
/// "the first index over the threshold", which on either ramp is the very
/// first sample — the centre value itself.
///
/// ```text
/// cargo test -p rustdar-frontend --test volume_gpu \
///     a_diverging_isosurface_draws_both_lobes_of_its_own_field \
///     -- --ignored --exact --nocapture
/// ```
#[test]
#[ignore = "needs a real wgpu adapter; see the doc comment for the invocation"]
fn a_diverging_isosurface_draws_both_lobes_of_its_own_field() {
    let _serialised = gpu_lock();
    let size = [64, 64];
    const CENTRE: u8 = 128;
    const DEVIATION: u8 = 34;
    let lut = grey_ramp_lut();

    let (device, queue) = device();
    let pipelines = VolumePipelines::new(&device, attachments(wgpu::TextureFormat::Bgra8Unorm));
    pipelines.upload_quad(&queue);

    // Slab 3 is met first and holds the centre exactly, so neither ramp can
    // cross at the box's entry face — the degenerate hit the shipped test
    // takes.
    for (levels, lobe, expected) in [
        ([68u8, 88, 108, CENTRE], "the low lobe", CENTRE - DEVIATION),
        ([188, 168, 148, CENTRE], "the high lobe", CENTRE + DEVIATION),
    ] {
        let (cells, indices) = slab_ramp(&levels);
        let mut uniform = iso_uniform(cells);
        uniform.iso_centre = f32::from(CENTRE) / 255.0;
        uniform.iso_threshold = f32::from(DEVIATION) / 255.0;

        let pixels = raymarch_once(
            &device, &queue, &pipelines, cells, &indices, &lut, &uniform, size,
        );
        let (lo, hi) = grey_span(&pixels, size);
        let level = i32::from(lo) + (i32::from(hi) - i32::from(lo)) / 2;
        assert!(
            (level - i32::from(expected)).abs() <= 5,
            "{lobe}: the surface is drawn at index {level} (span [{lo}, {hi}]), \
             where |value \u{2212} {CENTRE}| reaches {DEVIATION} at {expected}. \
             An index read straight through would put it at {CENTRE}.",
        );
        assert!(
            (level - i32::from(CENTRE)).abs() > 10,
            "{lobe}: the surface sits on the centre index itself, which is \
             where a threshold read against the raw index rather than against \
             the deviation would put it",
        );
    }
}

/// The isosurface excludes unmeasured air — the one contract
/// `COVERAGE_FLOOR` exists for, and the one nothing measured.
///
/// `iso_hit_test`'s coverage term had no test at all: deleting it outright, or
/// dropping `COVERAGE_FLOOR` from 0.5 to 0.0, passed the entire suite — 13/13
/// here, 10/10 silhouette, 151/151 lib. The reason is that every isosurface
/// fixture in this file is *fully covered* ([`slab_ramp`] fills every cell), so
/// no iso test contained a single no-data cell and the term was never reached.
///
/// What the term prevents is stated in the shader and is worst for exactly the
/// products that most need an isosurface. `field_at` of an all-air fetch is
/// index 0 by the floored divisor, and `iso_field` folds that against the
/// diverging centre, so:
///
/// * for a velocity-like product whose centre sits mid-ramp, air reads as a
///   deviation of the whole half-ramp — a strong inbound crossing at the very
///   first sample, which shrink-wraps the surface onto the *coverage cone* and
///   hides the couplet inside it; and
/// * for ρHV, whose centre sits at the **top** of its ramp, index 0 is the
///   single most extreme value the field can produce — the largest possible
///   hit, from the absence of data.
///
/// Both directions are rendered here. Each fixture puts two slabs of no-data
/// air between the eye and the data, so a march without the coverage term takes
/// its hit in the air at index 0 and the grey ramp reads back 0, while the
/// honest surface reads back the index where the deviation actually reaches the
/// threshold. The two are ~90 and ~190 grey levels apart, so neither mutation
/// can survive as a tolerance argument.
///
/// ```text
/// cargo test -p rustdar-frontend --test volume_gpu \
///     an_isosurface_excludes_unmeasured_air \
///     -- --ignored --exact --nocapture
/// ```
#[test]
#[ignore = "needs a real wgpu adapter; see the doc comment for the invocation"]
fn an_isosurface_excludes_unmeasured_air() {
    let _serialised = gpu_lock();
    let size = [64, 64];
    let lut = grey_ramp_lut();

    let (device, queue) = device();
    let pipelines = VolumePipelines::new(&device, attachments(wgpu::TextureFormat::Bgra8Unorm));
    pipelines.upload_quad(&queue);

    // `slab_ramp`'s slab 0 is met LAST, so the two zeros at the end of each
    // level list are the air the ray enters through. Slab 3 holds the centre
    // exactly, so no ramp can cross at the air/data interface itself.
    for (levels, shape, expected) in [
        (
            // A velocity-like couplet: centre mid-ramp, data falling away from
            // it. |0 - 128| = 128 is nearly four times the threshold.
            [68u8, 88, 108, 128, 0, 0],
            "a diverging centre mid-ramp",
            94i32,
        ),
        (
            // ρHV's shape: the centre at the top of the ramp, so air is the
            // most extreme reading the fold can return — |0 - 250| = 250.
            [160u8, 200, 230, 250, 0, 0],
            "a centre at the top of its ramp",
            190,
        ),
    ] {
        let centre = levels[3];
        let deviation = centre - u8::try_from(expected).expect("in range");
        let (cells, indices) = slab_ramp(&levels);
        let mut uniform = iso_uniform(cells);
        uniform.iso_centre = f32::from(centre) / 255.0;
        uniform.iso_threshold = f32::from(deviation) / 255.0;

        let pixels = raymarch_once(
            &device, &queue, &pipelines, cells, &indices, &lut, &uniform, size,
        );
        // `grey_span` asserts opacity, which is the first half of the claim:
        // an air test that rejected too much would paint nothing at all.
        let (lo, hi) = grey_span(&pixels, size);
        let level = i32::from(lo) + (i32::from(hi) - i32::from(lo)) / 2;
        println!(
            "{shape}: surface at index {level} (span [{lo}, {hi}]); \
             air would read {}",
            0,
        );
        assert!(
            (level - expected).abs() <= 5,
            "{shape}: the surface is drawn at index {level} (span [{lo}, \
             {hi}]), where |value \u{2212} {centre}| reaches {deviation} at \
             {expected}",
        );
        assert!(
            level > 20,
            "{shape}: the surface is drawn at index {level}, which is the \
             no-data index the two air slabs in front of the data hold. \
             `iso_hit_test` is taking its hit in unmeasured air: either the \
             coverage term is gone or COVERAGE_FLOOR has stopped excluding \
             air, and every diverging product's surface has collapsed onto \
             the coverage cone",
        );
    }
}

/// The isosurface keeps features narrower than the smoothing kernel — at the
/// rung the region boxes actually ship.
///
/// This is the measurement behind `volume::bridge`'s isosurface exemption, and
/// the reason the exemption is a line of code rather than a comment.
/// `cloud_reconstruction_lod_for` returns the full `CLOUD_RECONSTRUCTION_LOD`
/// for every cell size at or under 0.65 km, which is **both** shipped region
/// rungs (a 60 km box is 0.23 km/cell, a 160 km one 0.625). At that level a
/// lone measured voxel is an eighth of its coarse texel — coverage 32/255 =
/// 0.125 — and a one-cell sheet is half of one, coverage 128/255 = 0.502. The
/// shader's `COVERAGE_FLOOR` cut of 0.5 deletes the first outright and all but
/// deletes the second.
///
/// Both fixtures are shapes a forecaster looks for: the lone voxel is a narrow
/// hail core or an updraft tip, the one-cell sheet a bright band or a TDS
/// shell. Losing them from the 3D surface while the 2D pane and the lit volume
/// both still show them is the "3D erased a core the 2D pane shows" failure the
/// occupancy mip and the LOD taper were both added to close.
///
/// So the isosurface marches the raw field, and this test pins both halves:
/// the shipped configuration keeps the features, and the smoothed level is
/// measured erasing them rather than left as an assertion in a comment. If the
/// second assertion ever fails, the erasure has stopped being real and
/// `volume::bridge`'s reasoning needs rewriting rather than relaxing.
///
/// ```text
/// cargo test -p rustdar-frontend --test volume_gpu \
///     an_isosurface_at_the_shipped_rung_keeps_its_sub_kernel_features \
///     -- --ignored --exact --nocapture
/// ```
#[test]
#[ignore = "needs a real wgpu adapter; see the doc comment for the invocation"]
fn an_isosurface_at_the_shipped_rung_keeps_its_sub_kernel_features() {
    let _serialised = gpu_lock();
    let size = [128u32, 128];
    let cells = [16u32, 16, 16];
    let lut = grey_ramp_lut();

    let (device, queue) = device();
    let pipelines = VolumePipelines::new(&device, attachments(wgpu::TextureFormat::Bgra8Unorm));
    pipelines.upload_quad(&queue);

    let empty = vec![0u8; (cells[0] * cells[1] * cells[2]) as usize];
    // One measured cell in the middle of empty air.
    let mut lone_voxel = empty.clone();
    lone_voxel[((8 * cells[1] + 8) * cells[0] + 8) as usize] = 255;
    // One measured slab, one cell thick, spanning the whole horizontal extent
    // — the bright band's shape, and the fill the eye of `eye_outside(2)` sees
    // across the entire frame.
    let mut sheet = empty;
    let plane = (cells[0] * cells[1]) as usize;
    sheet[8 * plane..9 * plane].fill(255);

    // The shipped isosurface configuration: the cloud rung's step density,
    // which the bridge does send, and the raw reconstruction, which is the
    // exemption under test.
    let mut uniform = iso_uniform(cells);
    uniform.iso_centre = ISO_OFF;
    uniform.iso_threshold = 100.0 / 255.0;
    uniform.step_cells = rustdar_frontend::volume::bridge::CLOUD_STEP_CELLS;

    let painted = |indices: &[u8], uniform: &VolumeUniform| {
        raymarch_once(
            &device, &queue, &pipelines, cells, indices, &lut, uniform, size,
        )
        .iter()
        .filter(|px| px[3] > 0)
        .count()
    };

    let raw = (painted(&lone_voxel, &uniform), painted(&sheet, &uniform));
    uniform.reconstruction_lod = rustdar_frontend::volume::bridge::CLOUD_RECONSTRUCTION_LOD;
    let smoothed = (painted(&lone_voxel, &uniform), painted(&sheet, &uniform));
    println!(
        "isosurface at threshold 100/255, {}x{} px:\n  \
         lone voxel:   LOD 0 {} px, LOD {} {} px\n  \
         1-cell sheet: LOD 0 {} px, LOD {} {} px",
        size[0],
        size[1],
        raw.0,
        rustdar_frontend::volume::bridge::CLOUD_RECONSTRUCTION_LOD,
        smoothed.0,
        raw.1,
        rustdar_frontend::volume::bridge::CLOUD_RECONSTRUCTION_LOD,
        smoothed.1,
    );

    assert!(
        raw.0 > 0,
        "the shipped isosurface configuration paints nothing for a lone \
         measured voxel: a narrow hail core or updraft tip is absent from the \
         3D surface while the 2D pane shows it",
    );
    assert!(
        raw.1 > 0,
        "the shipped isosurface configuration paints nothing for a one-cell \
         sheet: a bright band or TDS shell is absent from the 3D surface",
    );
    assert!(
        raw.1 > raw.0 * 4,
        "the one-cell sheet ({} px) is not substantially larger than the lone \
         voxel ({} px), so the sheet fixture is not spanning the frame and \
         the erasure measurement below has nothing to bite on",
        raw.1,
        raw.0,
    );
    // The erasure the exemption exists for, measured rather than argued.
    assert_eq!(
        smoothed.0, 0,
        "at the region rungs' reconstruction level a lone measured voxel now \
         survives the {} coverage cut ({} px). That is the premise \
         `volume::bridge`'s isosurface exemption rests on, so if it has \
         changed the exemption's reasoning must be rewritten — not the \
         assertion relaxed",
        0.5, smoothed.0,
    );
    assert!(
        smoothed.1 * 4 < raw.1,
        "at the region rungs' reconstruction level the one-cell sheet keeps \
         {} px of its {} — the 0.502 coverage of a half-filled coarse texel \
         is no longer being cut by the 0.5 floor, so `volume::bridge`'s \
         isosurface exemption is resting on a premise that has changed",
        smoothed.1,
        raw.1,
    );
}

/// Opacity is per kilometre travelled, not per box diagonal.
///
/// Spike 0a's first bug, as the property it actually breaks. On a
/// 240 x 240 x 20 km box a vertical ray crosses 20 km and a horizontal one 240,
/// so at 0.01 per km their alphas must be `1 - exp(-0.2)` and `1 - exp(-2.4)`.
/// The 96-step discretisation drops out exactly — `(exp(-s*L/96))^96` is
/// `exp(-s*L)` — so these are analytic values, not tolerances hiding a fudge.
///
/// With `dt * length(box_size_km)` instead, both rays would get the box's
/// 340 km diagonal and both would read `1 - exp(-3.4) = 0.967`. The vertical
/// one is the tell: 0.18 against 0.97 is not a subtle difference, which is
/// precisely why it is worth having a test that can see it — on screen the
/// whole volume simply looks denser.
///
/// ```text
/// cargo test -p rustdar-frontend --test volume_gpu \
///     opacity_accumulates_per_kilometre_not_per_box_diagonal \
///     -- --ignored --exact --nocapture
/// ```
#[test]
#[ignore = "needs a real wgpu adapter; see the doc comment for the invocation"]
fn opacity_accumulates_per_kilometre_not_per_box_diagonal() {
    let _serialised = gpu_lock();
    const INDEX: u8 = 200;
    const EXTINCTION_PER_KM: f32 = 0.01;
    let box_size_km = [240.0f32, 240.0, 20.0];
    let size = [64, 64];
    let cells = [8u32, 8, 8];

    let (device, queue) = device();
    let pipelines = VolumePipelines::new(&device, attachments(wgpu::TextureFormat::Bgra8Unorm));
    pipelines.upload_quad(&queue);

    let filled = vec![INDEX; (cells[0] * cells[1] * cells[2]) as usize];
    let lut = palette(INDEX, [255, 255, 255, 255]);

    let mut alphas = [0.0f64; 3];
    for axis in 0..3 {
        let mut uniform = VolumeUniform::new(box_size_km, cells);
        uniform.box_from_clip = box_from_clip_down(axis);
        uniform.eye_in_box = eye_outside(axis);
        uniform.extinction_per_km = EXTINCTION_PER_KM;
        // Shading would multiply the colour, not the alpha, but leave it off so
        // the only thing under test is path length.
        uniform.gradient_shading = false;

        let pixels = raymarch_once(
            &device, &queue, &pipelines, cells, &filled, &lut, &uniform, size,
        );
        let measured = f64::from(centre(&pixels, size)[3]) / 255.0;
        let expected = 1.0 - (-f64::from(EXTINCTION_PER_KM) * f64::from(box_size_km[axis])).exp();
        assert!(
            (measured - expected).abs() < 0.01,
            "a ray down axis {axis} crosses {} km and should reach alpha \
             {expected:.4}; it reached {measured:.4}. `dt * length(box_size_km)` \
             would give every axis 0.9666.",
            box_size_km[axis]
        );
        alphas[axis] = measured;
    }

    // And the relative distortion, stated as the thing that reads as haze: the
    // ratio of optical depths must be the box's aspect ratio, 12.
    let optical_depth = |alpha: f64| -(1.0 - alpha).ln();
    let anisotropy = optical_depth(alphas[0]) / optical_depth(alphas[2]);
    assert!(
        (11.0..13.0).contains(&anisotropy),
        "a horizontal ray is {anisotropy:.1}x deeper than a vertical one; the \
         box is 12x wider than it is deep, so that is the figure"
    );
}

/// The blit composites exactly what egui would, on both surface colour spaces.
///
/// The measurement the whole colour design rests on. egui is driven for real —
/// a `rect_filled` of a known `Color32`, tessellated and rendered by
/// `egui_wgpu::Renderer` itself — and the blit is given the same premultiplied
/// gamma bytes in its offscreen. Both composite over the same cleared target
/// with the same blend state, so any difference in the two fragment shaders'
/// conventions shows up as a per-channel delta.
///
/// **Zero is the bar**, not "close". The colour-theoretically correct sRGB blit
/// — un-premultiply, decode, re-premultiply — measured 60/255 away here, which
/// is why decoding the premultiplied value directly is what shipped.
///
/// Dithering is switched off on egui's side. It is *on* in production
/// (`EguiRenderer::new` takes `RendererOptions`' default), and it adds
/// sub-eight-bit noise to egui's own geometry — the blit does not dither and
/// does not need to, because it is sampling an eight-bit texture rather than
/// quantising a float. Leaving it on here would compare the blit against noise.
///
/// The comparison is on the rectangle's interior: `rect_filled` is feathered by
/// about a pixel at its edges and the viewport is not, so the boundary is two
/// different things by design.
///
/// The map floor stands under the volume — drawn only when the flag says,
/// the right way up, and behind the volume's own opacity.
///
/// Four renders through one down-looking camera, each closing a mutation:
///
/// 1. Floor bound but `map_floor` off: nothing paints. Removing the shader's
///    `flags.w` gate fails here — and this is the instrument contract, since
///    every mask harness renders with a floor-capable pipeline now.
/// 2. Flag on over an empty grid: the whole footprint is the floor, opaque.
///    Deleting the after-march composite fails here.
/// 3. The floor's orientation: a red-north/blue-south floor renders red at
///    the top of the image. Dropping the `1 - hit.y` flip renders it blue.
/// 4. A saturating slab over the west half occludes the floor there and
///    leaves it visible to the east: the floor is behind the volume, not
///    over it.
///
/// ```text
/// cargo test -p rustdar-frontend --test volume_gpu \
///     the_map_floor_stands_under_the_volume_and_only_when_asked \
///     -- --ignored --exact --nocapture
/// ```
#[test]
#[ignore = "needs a real wgpu adapter; see the doc comment for the invocation"]
fn the_map_floor_stands_under_the_volume_and_only_when_asked() {
    let _serialised = gpu_lock();
    let size = [128u32, 128];
    let cells = [16u32, 16, 16];

    let (device, queue) = device();
    let pipelines = VolumePipelines::new(&device, attachments(wgpu::TextureFormat::Bgra8Unorm));
    pipelines.upload_quad(&queue);

    // Red top half (the box's north), blue bottom half. Opaque.
    let floor_side = 8usize;
    let mut floor_rgba = Vec::with_capacity(floor_side * floor_side * 4);
    for row in 0..floor_side {
        for _col in 0..floor_side {
            if row < floor_side / 2 {
                floor_rgba.extend_from_slice(&[255, 0, 0, 255]);
            } else {
                floor_rgba.extend_from_slice(&[0, 0, 255, 255]);
            }
        }
    }
    let floor = pipelines
        .upload_floor(
            &device,
            &queue,
            [floor_side as u32, floor_side as u32],
            &floor_rgba,
        )
        .expect("a well-shaped floor uploads");

    let empty = vec![0u8; (cells[0] * cells[1] * cells[2]) as usize];
    let mut lut = vec![0u8; VOLUME_LUT_BYTES];
    for entry in 1..lut.len() / 4 {
        lut[entry * 4..entry * 4 + 4].copy_from_slice(&[255, 255, 255, 255]);
    }

    // Looking down the z axis: image rows run from the box's north (top) to
    // south, columns west to east.
    let mut uniform = VolumeUniform::new([10.0, 10.0, 10.0], cells);
    uniform.box_from_clip = box_from_clip_down(2);
    uniform.eye_in_box = eye_outside(2);
    uniform.extinction_per_km = 1000.0;
    uniform.gradient_shading = false;

    // 1. Bound but not asked for: the flag is the gate, not the binding.
    let pixels = raymarch_once_with_floor(
        &device, &queue, &pipelines, cells, &empty, &lut, &uniform, size, &floor,
    );
    assert!(
        pixels.iter().all(|px| *px == [0, 0, 0, 0]),
        "a floor bound at group 1 painted with map_floor off; the shader has \
         lost its flags.w gate and every mask instrument now stands on ground",
    );

    // 2 + 3. Asked for, over an empty grid: the footprint is ground, opaque,
    // and the right way up.
    uniform.map_floor = true;
    let pixels = raymarch_once_with_floor(
        &device, &queue, &pipelines, cells, &empty, &lut, &uniform, size, &floor,
    );
    let top = pixels[(size[1] / 4 * size[0] + size[0] / 2) as usize];
    let bottom = pixels[(3 * size[1] / 4 * size[0] + size[0] / 2) as usize];
    assert_eq!(top[3], 255, "the floor must be opaque ground");
    assert!(
        top[0] > 200 && top[2] < 50,
        "the box's north edge must show the floor image's row 0 (red), got \
         {top:?}; a flipped v axis puts the map upside down",
    );
    assert!(
        bottom[2] > 200 && bottom[0] < 50,
        "the box's south edge must show the floor image's bottom rows (blue), \
         got {bottom:?}",
    );

    // 4. A saturating slab over the west half: the volume composites over
    // the floor where it stands, and the floor shows to the east.
    let mut west_slab = vec![0u8; (cells[0] * cells[1] * cells[2]) as usize];
    for z in 0..cells[2] {
        for y in 0..cells[1] {
            for x in 0..cells[0] / 2 {
                west_slab[((z * cells[1] + y) * cells[0] + x) as usize] = 255;
            }
        }
    }
    let pixels = raymarch_once_with_floor(
        &device, &queue, &pipelines, cells, &west_slab, &lut, &uniform, size, &floor,
    );
    let west = pixels[(size[1] / 4 * size[0] + size[0] / 4) as usize];
    let east = pixels[(size[1] / 4 * size[0] + 3 * size[0] / 4) as usize];
    assert!(
        west.iter().take(3).all(|c| *c > 200),
        "over the slab the volume (saturated white) must hide the floor, got \
         {west:?}; the floor is compositing in front of the march",
    );
    assert!(
        east[0] > 200 && east[2] < 50,
        "east of the slab the floor (red at this row) must show, got {east:?}",
    );
}

/// The floor and the volume agree, to the pixel, about where the weather
/// stands.
///
/// The orientation case above is qualitative — red north, blue south. This is
/// the quantitative seam: one voxel column and one floor patch are planted at
/// the **same box footprint cell**, each is rendered alone through the same
/// down-looking camera, and their screen centroids must coincide within a
/// pixel bound. Any offset, flip or scale disagreement between the volume's
/// texture mapping and the floor's `(hit.x, 1 - hit.y)` sampling moves one
/// centroid and not the other: dropping the v flip alone moves the floor
/// patch 36 px here.
///
/// ```text
/// cargo test -p rustdar-frontend --test volume_gpu \
///     the_floor_and_the_volume_put_the_same_weather_in_the_same_place \
///     -- --ignored --exact --nocapture
/// ```
#[test]
#[ignore = "needs a real wgpu adapter; see the doc comment for the invocation"]
fn the_floor_and_the_volume_put_the_same_weather_in_the_same_place() {
    let _serialised = gpu_lock();
    let size = [128u32, 128];
    let cells = [32u32, 32, 32];
    let (device, queue) = device();
    let pipelines = VolumePipelines::new(&device, attachments(wgpu::TextureFormat::Bgra8Unorm));
    pipelines.upload_quad(&queue);

    // The planted cell: (24, 20) of 32 — off-centre on both axes and off the
    // diagonal, so every flip and every axis swap moves it.
    let (col_cell, row_cell) = (24u32, 20u32);

    // A full-height voxel column at that cell.
    let mut column = vec![0u8; (cells[0] * cells[1] * cells[2]) as usize];
    for z in 0..cells[2] {
        column[((z * cells[1] + row_cell) * cells[0] + col_cell) as usize] = 255;
    }
    // Green at every data index, not just 255: the grid is sampled `Linear`,
    // so rays off the column's exact centre read interpolated indices, and a
    // single-entry palette would paint only the centre line. The half-cell
    // bleed this admits is symmetric about the column, which is what a
    // centroid instrument needs.
    let mut lut = vec![0u8; VOLUME_LUT_BYTES];
    for entry in 1..lut.len() / 4 {
        lut[entry * 4..entry * 4 + 4].copy_from_slice(&[0, 255, 0, 255]);
    }

    // A floor patch over the same footprint: floor row 0 is the box's NORTH
    // edge, so box y in [20/32, 21/32] is floor rows [1 - 21/32, 1 - 20/32).
    let floor_side = 64usize;
    let mut floor_rgba = vec![0u8; floor_side * floor_side * 4];
    for px in floor_rgba.chunks_exact_mut(4) {
        px[3] = 255;
    }
    let scale = floor_side as u32 / cells[0];
    for row in (floor_side as u32 - (row_cell + 1) * scale)..(floor_side as u32 - row_cell * scale)
    {
        for col in (col_cell * scale)..((col_cell + 1) * scale) {
            let at = ((row * floor_side as u32 + col) * 4) as usize;
            floor_rgba[at..at + 4].copy_from_slice(&[255, 0, 0, 255]);
        }
    }
    let floor = pipelines
        .upload_floor(
            &device,
            &queue,
            [floor_side as u32, floor_side as u32],
            &floor_rgba,
        )
        .expect("a well-shaped floor uploads");

    let mut uniform = VolumeUniform::new([10.0, 10.0, 10.0], cells);
    uniform.box_from_clip = box_from_clip_down(2);
    // Not `eye_outside(2)`: an eye 2.5 box-heights up gives every ray a real
    // lateral slope, and a full-height column smears across the screen by
    // parallax — a position instrument needs parallel rays. An eye 200 boxes
    // up through the same far plane is orthographic to under a tenth of a
    // pixel at this size.
    uniform.eye_in_box = [0.5, 0.5, 200.0];
    uniform.extinction_per_km = 1000.0;
    uniform.gradient_shading = false;

    // Screen centroid of the pixels `select` keeps.
    let centroid = |pixels: &[[u8; 4]], select: &dyn Fn([u8; 4]) -> bool| -> (f64, f64) {
        let mut n = 0usize;
        let (mut sx, mut sy) = (0.0, 0.0);
        for (i, px) in pixels.iter().enumerate() {
            if select(*px) {
                n += 1;
                sx += (i % size[0] as usize) as f64;
                sy += (i / size[0] as usize) as f64;
            }
        }
        assert!(n > 0, "nothing painted; a broken fixture");
        (sx / n as f64, sy / n as f64)
    };

    // The volume alone.
    let pixels = raymarch_once_with_floor(
        &device, &queue, &pipelines, cells, &column, &lut, &uniform, size, &floor,
    );
    let volume_at = centroid(&pixels, &|px| px[1] > 100 && px[0] < 100);

    // The floor alone, under an empty grid.
    uniform.map_floor = true;
    let empty = vec![0u8; (cells[0] * cells[1] * cells[2]) as usize];
    let pixels = raymarch_once_with_floor(
        &device, &queue, &pipelines, cells, &empty, &lut, &uniform, size, &floor,
    );
    let floor_at = centroid(&pixels, &|px| px[0] > 100 && px[2] < 100);

    // Where the geometry says both stand: the cell centre, through the
    // down-camera's screen mapping (col = x·W, row = (1 − y)·H).
    let want = (
        (f64::from(col_cell) + 0.5) / f64::from(cells[0]) * f64::from(size[0]),
        (1.0 - (f64::from(row_cell) + 0.5) / f64::from(cells[1])) * f64::from(size[1]),
    );
    for (name, (cx, cy)) in [("volume", volume_at), ("floor", floor_at)] {
        assert!(
            (cx - want.0).abs() < 3.0 && (cy - want.1).abs() < 3.0,
            "the {name} put the planted cell at ({cx:.1}, {cy:.1}), the geometry \
             says ({:.1}, {:.1})",
            want.0,
            want.1,
        );
    }
    let (dx, dy) = (floor_at.0 - volume_at.0, floor_at.1 - volume_at.1);
    assert!(
        dx.abs() < 2.0 && dy.abs() < 2.0,
        "floor and volume disagree by ({dx:.2}, {dy:.2}) px about where the same \
         cell stands — the registration seam has moved",
    );
}

/// From under the box, the floor does not wall the volume off.
///
/// The composite draws the floor **in front** of the whole volume for an eye
/// under the bottom plane — geometrically it is in front — but the user asked
/// for the ground to become transparent from below, and the shader fades its
/// coverage out over `FLOOR_BELOW_FADE` of eye depth. Two renders through an
/// up-looking camera, one closing each half:
///
/// 1. A saturating slab in the box, floor on, eye well under the fade band:
///    the slab's colour reaches the pixel — deleting the fade (coverage 1
///    from below) walls it off with ground.
/// 2. An empty grid, floor on, same eye: the pixel is fully transparent —
///    compositing any residual ground from below fails here.
///
/// The eye-above cases are pinned by
/// [`the_map_floor_stands_under_the_volume_and_only_when_asked`], which this
/// change must leave bit-identical: above the plane the fade is exactly 1.
///
/// ```text
/// cargo test -p rustdar-frontend --test volume_gpu \
///     the_floor_is_transparent_from_below \
///     -- --ignored --exact --nocapture
/// ```
#[test]
#[ignore = "needs a real wgpu adapter; see the doc comment for the invocation"]
fn the_floor_is_transparent_from_below() {
    let _serialised = gpu_lock();
    let size = [96u32, 96];
    let cells = [16u32, 16, 16];
    let (device, queue) = device();
    let pipelines = VolumePipelines::new(&device, attachments(wgpu::TextureFormat::Bgra8Unorm));
    pipelines.upload_quad(&queue);

    // An opaque red floor: the wall the fade must dissolve.
    let floor_rgba: Vec<u8> = std::iter::repeat_n([255u8, 0, 0, 255], 64)
        .flatten()
        .collect();
    let floor = pipelines
        .upload_floor(&device, &queue, [8, 8], &floor_rgba)
        .expect("a well-shaped floor uploads");

    // Looking UP the z axis from under the box: the mirror of
    // `box_from_clip_down(2)` — depth 1 unprojects one box beyond the top
    // face, the eye sits one box under the bottom, well below the fade band.
    let mut up = [[0.0f32; 4]; 4];
    up[0][0] = 0.5;
    up[1][1] = 0.5;
    up[3][0] = 0.5;
    up[3][1] = 0.5;
    up[2][2] = 2.5;
    up[3][2] = -0.5;
    up[3][3] = 1.0;
    let mut uniform = VolumeUniform::new([10.0, 10.0, 10.0], cells);
    uniform.box_from_clip = up;
    uniform.eye_in_box = [0.5, 0.5, -1.0];
    uniform.extinction_per_km = 1000.0;
    uniform.gradient_shading = false;
    uniform.map_floor = true;

    // 1. A saturating white slab fills the box's top half.
    let mut slab = vec![0u8; (cells[0] * cells[1] * cells[2]) as usize];
    for z in cells[2] / 2..cells[2] {
        for y in 0..cells[1] {
            for x in 0..cells[0] {
                slab[((z * cells[1] + y) * cells[0] + x) as usize] = 255;
            }
        }
    }
    let mut lut = vec![0u8; VOLUME_LUT_BYTES];
    for entry in 1..lut.len() / 4 {
        lut[entry * 4..entry * 4 + 4].copy_from_slice(&[255, 255, 255, 255]);
    }
    let pixels = raymarch_once_with_floor(
        &device, &queue, &pipelines, cells, &slab, &lut, &uniform, size, &floor,
    );
    let seen = centre(&pixels, size);
    assert!(
        seen.iter().take(3).all(|c| *c > 200) && seen[3] == 255,
        "from below, the volume (saturated white) must show through the floor, \
         got {seen:?}; an opaque ground from underneath is the wall the user \
         reported",
    );

    // 2. Nothing in the box: nothing may paint — a residual ground fragment
    // from below is the same wall at partial opacity.
    let empty = vec![0u8; (cells[0] * cells[1] * cells[2]) as usize];
    let pixels = raymarch_once_with_floor(
        &device, &queue, &pipelines, cells, &empty, &lut, &uniform, size, &floor,
    );
    let seen = centre(&pixels, size);
    assert_eq!(
        seen,
        [0, 0, 0, 0],
        "an empty box viewed from below must be fully transparent with the \
         floor toggle on",
    );
}

/// The smoothed reconstruction really reaches the coarse level: a lone voxel
/// paints a **wider** footprint through the cloud rung than through the raw
/// field.
///
/// Two mutations this can see, and one it deliberately cannot:
///
/// * Deleting the mip-1 upload in `upload_volume` leaves level 1 zeroed
///   (WebGPU zero-initialises textures), the LOD-1 render paints nothing,
///   and the width assertion fails on an empty mask.
/// * Writing the wrong bytes into the level — a stride or dimension error —
///   moves or smears the footprint, which the width ratio bounds.
/// * It cannot see the *default* leaking soft: that contract belongs to the
///   silhouette harness's index-1 sphere, which this test leaves untouched.
///
/// ```text
/// cargo test -p rustdar-frontend --test volume_gpu \
///     the_smoothed_reconstruction_spreads_a_lone_voxel \
///     -- --ignored --exact --nocapture
/// ```
#[test]
#[ignore = "needs a real wgpu adapter; see the doc comment for the invocation"]
fn the_smoothed_reconstruction_spreads_a_lone_voxel() {
    let _serialised = gpu_lock();
    let size = [128u32, 128];
    let cells = [16u32, 16, 16];

    let (device, queue) = device();
    let pipelines = VolumePipelines::new(&device, attachments(wgpu::TextureFormat::Bgra8Unorm));
    pipelines.upload_quad(&queue);

    // One filled cell in the middle of an empty grid — the isolated spike the
    // reconstruction exists to dissolve.
    let mut indices = vec![0u8; (cells[0] * cells[1] * cells[2]) as usize];
    indices[((8 * cells[1] + 8) * cells[0] + 8) as usize] = 255;
    // Opaque at every non-zero index, so interpolated indices between the
    // spike and its empty neighbours stay visible and alpha is a mask.
    let mut lut = vec![0u8; VOLUME_LUT_BYTES];
    for entry in 1..lut.len() / 4 {
        lut[entry * 4..entry * 4 + 4].copy_from_slice(&[255, 255, 255, 255]);
    }

    let mut uniform = VolumeUniform::new([10.0, 10.0, 10.0], cells);
    uniform.box_from_clip = box_from_clip_down(2);
    uniform.eye_in_box = eye_outside(2);
    uniform.extinction_per_km = 1000.0;
    uniform.gradient_shading = false;

    let painted = |uniform: &VolumeUniform| {
        raymarch_once(
            &device, &queue, &pipelines, cells, &indices, &lut, uniform, size,
        )
        .iter()
        .filter(|px| px[3] > 0)
        .count()
    };

    let raw = painted(&uniform);
    uniform.reconstruction_lod = rustdar_frontend::volume::bridge::CLOUD_RECONSTRUCTION_LOD;
    uniform.step_cells = rustdar_frontend::volume::bridge::CLOUD_STEP_CELLS;
    let cloud = painted(&uniform);
    println!("lone voxel: raw field paints {raw} px, smoothed reconstruction {cloud} px");

    assert!(raw > 0, "precondition: the lone voxel must paint at all");
    assert!(
        cloud > raw,
        "the smoothed reconstruction painted {cloud} px against the raw \
         field's {raw}; the coarse level is empty or never sampled, so the \
         cloud rung is silently rendering the raw field",
    );
    assert!(
        cloud < raw * 8,
        "the smoothed reconstruction painted {cloud} px against the raw \
         field's {raw} — more than the two-cell kernel can explain, so the \
         coarse level's bytes are misplaced",
    );
}

/// The coverage-premultiplied reconstruction never paints a palette band the
/// data does not occupy — the boundary-honesty contract behind the KLOT NROT
/// green arcs, now discharged by the texture rather than by a nearest march.
///
/// The fixture is the defect's shape in miniature: a small block whose only
/// data index is 147 (blue), over a LUT whose entries 1..=120 are opaque
/// green — the stand-in for NROT's anticyclonic band, which really does sit
/// under its cyclonic data on the one index ramp. A plain `R8Unorm` tent
/// interpolates *indices*, so every sample in the one-cell shell between the
/// block and empty air reads some index in (0, 147) and paints the green the
/// field never contained; measured on KLOT 2026-08-10, a volume with 0-2
/// honest green voxels rendered broad green arcs exactly this way.
///
/// With `R = coverage x index`, `G = coverage` and `index = R_bar / G_bar`,
/// air contributes 0 to both sums and drops out of the mean, so every sample
/// in that shell reconstructs to 147 exactly. The block may only ever paint
/// its own blue — **at the same LOD 0 trilinear filter the old path painted
/// green at**, which is the difference between this and the nearest march it
/// replaces.
///
/// # The control, and why it is not optional
///
/// A green-free render proves nothing on its own: a camera that missed the
/// boundary, or a march that never sampled the shell, would produce the same
/// zero. So the same fixture is rendered a second time with every air cell
/// replaced by [`CONTROL_AIR`] — a real index, so coverage is 1 everywhere and
/// the tent blends it against 147 straight through the green run. That render
/// **must** be green. It is the old defect reproduced through this very shader
/// with coverage removed as the only variable, so it pins that the geometry,
/// the camera and the march do reach the interpolation shell.
///
/// [`CONTROL_AIR`]'s own LUT entry is **transparent**, and that is the whole
/// design of the control rather than an accident of it. With an opaque entry
/// the outer air layer saturates on the first sample at this fixture's
/// extinction and the render is uniformly green without the march ever
/// reaching the data block — deleting the block from the control fixture
/// changed nothing, so the control was inert, green by construction, and
/// asserting on it pinned nothing. Transparent air makes the *only* possible
/// green the interpolation shell between index 1 and index 147, which is the
/// property the control claims. The opaque green run therefore starts at entry
/// 2.
///
/// ```text
/// cargo test -p rustdar-frontend --test volume_gpu \
///     coverage_reconstruction_never_paints_a_band_the_data_does_not_occupy \
///     -- --ignored --exact --nocapture
/// ```
#[test]
#[ignore = "needs a real wgpu adapter; see the doc comment for the invocation"]
fn coverage_reconstruction_never_paints_a_band_the_data_does_not_occupy() {
    let _serialised = gpu_lock();
    let size = [128u32, 128];
    let cells = [8u32, 8, 8];
    const DATA: u8 = 147;
    /// The air replacement in the control: a real index below the green
    /// band, so coverage is 1 everywhere and the tent has a band to sweep.
    ///
    /// Its own LUT entry is transparent — see the doc comment. An opaque one
    /// saturates the outer layer and the control never reaches the data.
    const CONTROL_AIR: u8 = 1;

    let (device, queue) = device();
    let pipelines = VolumePipelines::new(&device, attachments(wgpu::TextureFormat::Bgra8Unorm));
    pipelines.upload_quad(&queue);

    // A 2x2x2 block in the middle of empty air, every face a no-data boundary.
    let block = |air: u8| {
        let mut indices = vec![air; (cells[0] * cells[1] * cells[2]) as usize];
        for z in 3..5u32 {
            for y in 3..5u32 {
                for x in 3..5u32 {
                    indices[((z * cells[1] + y) * cells[0] + x) as usize] = DATA;
                }
            }
        }
        indices
    };
    // The band under the data: opaque green, like NROT's anticyclonic run.
    // It starts at 2, not at 1: entry `CONTROL_AIR` stays transparent so the
    // control's air layer cannot saturate before the march reaches the shell.
    let mut lut = vec![0u8; VOLUME_LUT_BYTES];
    for entry in usize::from(CONTROL_AIR) + 1..=120usize {
        lut[entry * 4..entry * 4 + 4].copy_from_slice(&[0, 255, 0, 255]);
    }
    let at = usize::from(DATA) * 4;
    lut[at..at + 4].copy_from_slice(&[0, 0, 255, 255]);

    let mut uniform = VolumeUniform::new([10.0, 10.0, 10.0], cells);
    uniform.box_from_clip = box_from_clip_down(2);
    uniform.eye_in_box = eye_outside(2);
    uniform.extinction_per_km = 1000.0;
    uniform.gradient_shading = false;

    let census = |indices: &[u8], uniform: &VolumeUniform| {
        let pixels = raymarch_once(
            &device, &queue, &pipelines, cells, indices, &lut, uniform, size,
        );
        let green = pixels
            .iter()
            .filter(|px| px[3] > 0 && px[1] > px[0] && px[1] > px[2])
            .count();
        let blue = pixels
            .iter()
            .filter(|px| px[3] > 0 && px[2] > px[0] && px[2] > px[1])
            .count();
        (green, blue)
    };

    let (green, blue) = census(&block(0), &uniform);
    let (control_green, control_blue) = census(&block(CONTROL_AIR), &uniform);
    println!(
        "coverage: {green} green px / {blue} blue px; \
         all-covered control: {control_green} green px / {control_blue} blue px"
    );

    assert!(
        control_green > 0,
        "precondition: with coverage 1 everywhere the tent no longer paints \
         the under-band between index {CONTROL_AIR} and index {DATA}, so this \
         fixture has stopped exercising the interpolation shell and the \
         green-free assertion below is vacuous",
    );
    assert!(
        blue > 0,
        "the reconstruction erased the data itself — the block must still \
         paint its own colour",
    );
    assert_eq!(
        green, 0,
        "the march painted {green} green pixels from a volume whose only data \
         index is blue: a filtered sample is being dragged across the no-data \
         boundary again, which is the KLOT NROT green-arc defect",
    );
}

/// ```text
/// cargo test -p rustdar-frontend --test volume_gpu \
///     the_blit_matches_egui_exactly_on_both_surface_formats \
///     -- --ignored --exact --nocapture
/// ```
#[test]
#[ignore = "needs a real wgpu adapter; see the doc comment for the invocation"]
fn the_blit_matches_egui_exactly_on_both_surface_formats() {
    let _serialised = gpu_lock();
    const SIZE: [u32; 2] = [64, 64];
    // Partial alpha on purpose: at alpha 1 the premultiply is the identity and
    // every candidate rule agrees, so a fully opaque colour would prove nothing.
    let colour = egui::Color32::from_rgba_unmultiplied(200, 60, 30, 128);
    let rect = egui::Rect::from_min_max(egui::pos2(16.0, 16.0), egui::pos2(48.0, 48.0));

    let (device, queue) = device();

    for format in [
        wgpu::TextureFormat::Rgba8Unorm,
        wgpu::TextureFormat::Rgba8UnormSrgb,
    ] {
        let theirs = egui_reference(&device, &queue, format, SIZE, rect, colour);
        let ours = blitted(&device, &queue, format, SIZE, rect, colour);

        let mut worst = 0i32;
        let mut worst_at = (0u32, 0u32);
        // Four pixels in from each edge of the rect, clear of the feathering.
        for y in (rect.min.y as u32 + 4)..(rect.max.y as u32 - 4) {
            for x in (rect.min.x as u32 + 4)..(rect.max.x as u32 - 4) {
                let at = (y * SIZE[0] + x) as usize;
                for channel in 0..4 {
                    let delta = i32::from(ours[at][channel]) - i32::from(theirs[at][channel]);
                    if delta.abs() > worst {
                        worst = delta.abs();
                        worst_at = (x, y);
                    }
                }
            }
        }

        let at = (worst_at.1 * SIZE[0] + worst_at.0) as usize;
        assert_eq!(
            worst, 0,
            "on a {format:?} surface the blit is {worst}/255 away from egui's \
             own rect_filled at {worst_at:?}: egui wrote {:?}, the blit wrote \
             {:?}. Matching egui is the requirement — the principled \
             un-premultiply/decode/re-premultiply measured 60/255 off here.",
            theirs[at], ours[at],
        );
    }
}

/// egui's own rendering of one filled rectangle, read back.
fn egui_reference(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    format: wgpu::TextureFormat,
    size: [u32; 2],
    rect: egui::Rect,
    colour: egui::Color32,
) -> Vec<[u8; 4]> {
    let context = egui::Context::default();
    let raw_input = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(size[0] as f32, size[1] as f32),
        )),
        ..Default::default()
    };
    // Painted straight onto a layer rather than through a panel, so the only
    // geometry in the frame is the rectangle.
    let output = context.run_ui(raw_input, |context| {
        context
            .layer_painter(egui::LayerId::background())
            .rect_filled(rect, 0.0, colour);
    });
    let primitives = context.tessellate(output.shapes, 1.0);

    let mut renderer = egui_wgpu::Renderer::new(
        device,
        format,
        egui_wgpu::RendererOptions {
            msaa_samples: 1,
            depth_stencil_format: None,
            dithering: false,
            predictable_texture_filtering: false,
        },
    );
    for (id, delta) in &output.textures_delta.set {
        renderer.update_texture(device, queue, *id, delta);
    }
    let screen_descriptor = egui_wgpu::ScreenDescriptor {
        size_in_pixels: size,
        pixels_per_point: 1.0,
    };

    let target = render_target(device, format, size);
    let view = target.create_view(&Default::default());
    let mut encoder = device.create_command_encoder(&Default::default());
    let user_buffers =
        renderer.update_buffers(device, queue, &mut encoder, &primitives, &screen_descriptor);
    {
        let pass = clearing_pass!(encoder, &view);
        renderer.render(&mut pass.forget_lifetime(), &primitives, &screen_descriptor);
    }
    queue.submit(user_buffers.into_iter().chain([encoder.finish()]));

    read_back(device, queue, &target, size)
}

/// The same colour, put through the offscreen and the compositing quad.
fn blitted(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    format: wgpu::TextureFormat,
    size: [u32; 2],
    rect: egui::Rect,
    colour: egui::Color32,
) -> Vec<[u8; 4]> {
    let pipelines = VolumePipelines::new(device, attachments(format));
    pipelines.upload_quad(queue);

    // The offscreen holds sRGB-encoded PREMULTIPLIED colour, which is exactly
    // what `Color32` already is — egui premultiplies after encoding, so its own
    // four bytes are the convention the raymarch's last line produces.
    let offscreen_size = [(rect.width() as u32).max(1), (rect.height() as u32).max(1)];
    let offscreen = pipelines.create_offscreen(device, offscreen_size);
    let texels: Vec<u8> = std::iter::repeat_n(
        colour.to_array(),
        (offscreen_size[0] * offscreen_size[1]) as usize,
    )
    .flatten()
    .collect();
    queue.write_texture(
        offscreen.texture().as_image_copy(),
        &texels,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(offscreen_size[0] * 4),
            rows_per_image: Some(offscreen_size[1]),
        },
        wgpu::Extent3d {
            width: offscreen_size[0],
            height: offscreen_size[1],
            depth_or_array_layers: 1,
        },
    );

    let target = render_target(device, format, size);
    let view = target.create_view(&Default::default());
    let mut encoder = device.create_command_encoder(&Default::default());
    {
        let mut pass = clearing_pass!(encoder, &view).forget_lifetime();
        // The quad covers all of clip space; the viewport is what places it.
        pass.set_viewport(
            rect.min.x,
            rect.min.y,
            rect.width(),
            rect.height(),
            0.0,
            1.0,
        );
        pipelines.paint_blit(&mut pass, &offscreen);
    }
    queue.submit(Some(encoder.finish()));

    read_back(device, queue, &target, size)
}
