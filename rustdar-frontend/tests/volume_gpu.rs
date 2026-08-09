//! What only a real GPU can say about the volume raymarch.
//!
//! Everything here is `#[ignore]`d and every test carries its own invocation.
//! CI has no GPU and is not getting one: adding `mesa-vulkan-drivers` to the
//! workflow is a separate, last, revertable commit that nothing before it may
//! depend on.
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
use rustdar_frontend::volume::uniform::VolumeUniform;

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
