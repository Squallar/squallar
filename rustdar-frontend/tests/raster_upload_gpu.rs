//! A banded upload puts the same bytes in the same places a single
//! `write_texture` would have.
//!
//! `texture_upload`'s own tests are arithmetic over [`BandPlan`], which is where
//! the interesting decisions are. Two things they cannot reach, and both of them
//! fail silently:
//!
//! * **The stride.** `copy_buffer_to_texture` is held to
//!   `COPY_BYTES_PER_ROW_ALIGNMENT` where `write_texture` repacks internally, so
//!   the DMA route pads every row and the fallback does not. Get that wrong and
//!   the picture shears progressively across each band — no panic, no validation
//!   message, just a raster that looks like a torn page. A 7362 px surveillance
//!   cut is 29448 bytes a row against a 29696-byte stride, so this is the
//!   shipped shape rather than an edge case.
//! * **The origin.** Each band lands at `y = rows already moved`, and an
//!   off-by-one there stacks bands on top of each other or leaves gaps.
//!
//! So the raster here is deliberately **odd**: 3000 px is 12000 bytes a row
//! against a 12032-byte stride, and 36 MB is more than one frame's budget on
//! *either* route — five [`UPLOAD_BAND_BYTES`] bands, the last of them short.
//! That is the smallest shape that exercises row padding, several bands, a
//! short final band and more than one frame on both routes at once. Every texel
//! is a function of its own coordinates, so a band landing at the wrong row or
//! a row read at the wrong stride produces different bytes rather than the same
//! byte.
//!
//! `#[ignore]`d, both of them: they need a real adapter, and CI has none.
//!
//! [`BandPlan`]: rustdar_frontend::egui_renderer::texture_upload
//! [`UPLOAD_BAND_BYTES`]: rustdar_frontend::egui_renderer::texture_upload::UPLOAD_BAND_BYTES

#![cfg(not(target_arch = "wasm32"))]

use egui_wgpu::wgpu;
use rustdar_frontend::egui_renderer::texture_upload::TextureUploads;
use rustdar_frontend::staging_ring::STAGING_RING_FEATURE;

/// The odd, multi-band shape. See the module note.
const SIDE: usize = 3000;

/// A device, with the staging ring feature or deliberately without it.
///
/// `None` when there is no adapter at all, which is the arm CI takes and the
/// reason both tests are `#[ignore]`d rather than skipped silently.
fn device(with_ring: bool) -> Option<(wgpu::Device, wgpu::Queue, bool)> {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        compatible_surface: None,
        force_fallback_adapter: false,
    }))
    .ok()?;
    let features = if with_ring {
        adapter.features() & STAGING_RING_FEATURE
    } else {
        wgpu::Features::empty()
    };
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("raster-upload"),
        required_features: features,
        required_limits: adapter.limits(),
        memory_hints: wgpu::MemoryHints::default(),
        experimental_features: wgpu::ExperimentalFeatures::default(),
        trace: wgpu::Trace::Off,
    }))
    .ok()?;
    let has_ring = features.contains(STAGING_RING_FEATURE);
    Some((device, queue, has_ring))
}

/// A texel that is a function of where it is, so a misplaced band shows up.
fn texel(x: usize, y: usize) -> [u8; 4] {
    [
        (x % 251) as u8,
        (y % 241) as u8,
        ((x * 7 + y * 13) % 239) as u8,
        // Alpha stays opaque: `ColorImage::from_rgba_premultiplied` keeps the
        // bytes as given, and a varying alpha would only test epaint.
        255,
    ]
}

fn source() -> egui::ColorImage {
    let mut rgba = vec![0u8; SIDE * SIDE * 4];
    for y in 0..SIDE {
        for x in 0..SIDE {
            rgba[(y * SIDE + x) * 4..(y * SIDE + x) * 4 + 4].copy_from_slice(&texel(x, y));
        }
    }
    egui::ColorImage::from_rgba_premultiplied([SIDE, SIDE], &rgba)
}

/// Read mip 0 of `texture` back as tightly packed RGBA.
fn read_back(device: &wgpu::Device, queue: &wgpu::Queue, texture: &wgpu::Texture) -> Vec<u8> {
    let row = SIDE * 4;
    let padded = row.next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT as usize);
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("raster-readback"),
        size: (padded * SIDE) as u64,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
    encoder.copy_texture_to_buffer(
        texture.as_image_copy(),
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(padded as u32),
                rows_per_image: Some(SIDE as u32),
            },
        },
        wgpu::Extent3d {
            width: SIDE as u32,
            height: SIDE as u32,
            depth_or_array_layers: 1,
        },
    );
    queue.submit(Some(encoder.finish()));
    buffer.slice(..).map_async(wgpu::MapMode::Read, |_| {});
    device
        .poll(wgpu::PollType::wait_indefinitely())
        .expect("the readback drains");
    let view = buffer.slice(..).get_mapped_range();
    let mut out = Vec::with_capacity(SIDE * row);
    for y in 0..SIDE {
        out.extend_from_slice(&view[y * padded..y * padded + row]);
    }
    drop(view);
    buffer.unmap();
    out
}

/// Drive frames until the upload says it is done, and say how many it took.
///
/// The first frame carries the delta; the rest carry nothing, which is exactly
/// what `end_pass_and_upload` does on a frame egui produced no new texture on.
/// The bound is the liveness claim: a raster that never finished would hang the
/// suite rather than fail it, so it fails it.
fn run_to_completion(
    uploads: &mut TextureUploads,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    renderer: &mut egui_wgpu::Renderer,
    set: &[(egui::TextureId, egui::epaint::ImageDelta)],
) -> u32 {
    let mut frames = 0;
    let mut pending = uploads.apply(device, queue, renderer, set);
    while pending {
        frames += 1;
        assert!(
            frames < 1000,
            "the upload was still not finished after {frames} frames — a band is \
             not making progress and the pane would never draw",
        );
        // A declined ring hands the band back for the *next* frame, so a frame
        // that moved nothing has to be given the chance the app would give it.
        let _ = device.poll(wgpu::PollType::wait_indefinitely());
        pending = uploads.apply(device, queue, renderer, &[]);
    }
    frames + 1
}

/// Both routes put every texel exactly where a single `write_texture` would.
///
/// Parameterised over the ring rather than written twice, because the property
/// is that the two routes **agree**: the DMA path pads each row to the copy
/// alignment and the fallback does not, and the whole risk is that only one of
/// them is right.
fn every_texel_lands(with_ring: bool) {
    let Some((device, queue, has_ring)) = device(with_ring) else {
        eprintln!("no adapter; nothing to check");
        return;
    };
    assert_eq!(
        has_ring, with_ring,
        "this run wanted a ring={with_ring} device and the adapter gave {has_ring}, \
         so it would have measured the other route",
    );

    let mut renderer = egui_wgpu::Renderer::new(
        &device,
        wgpu::TextureFormat::Bgra8Unorm,
        egui_wgpu::RendererOptions::default(),
    );
    // A real pass, not a bare `Context`: egui's font atlas is a 0x0 delta until
    // one has been run, and `Renderer::update_texture` refuses to create a
    // texture of that size. Production has always had a pass here — this is
    // `end_pass_and_upload`'s own input, which is the point.
    let ctx = egui::Context::default();
    // The adapter's limit, the way `EguiRenderer::new` hands it to
    // `egui_winit::State`. egui's own default is the WebGL2 floor of 2048 and
    // it *panics* on a larger `load_texture`, so a bare `RawInput` here would
    // refuse the shape this test exists to check.
    ctx.begin_pass(egui::RawInput {
        max_texture_side: Some(device.limits().max_texture_dimension_2d as usize),
        ..Default::default()
    });
    let handle = ctx.load_texture("raster", source(), egui::TextureOptions::NEAREST);
    let delta = ctx.end_pass().textures_delta;

    let mut uploads = TextureUploads::new(&device);
    assert_eq!(uploads.has_ring(), with_ring);
    let frames = run_to_completion(&mut uploads, &device, &queue, &mut renderer, &delta.set);

    // More than one, or the band budget is not doing anything and this test is
    // checking a single `write_texture` under another name.
    assert!(
        frames > 1,
        "a {SIDE}px raster finished in one frame, so the {} bytes it carries did \
         not exceed a frame's budget and nothing here was exercised",
        SIDE * SIDE * 4,
    );

    let texture = uploads
        .texture(handle.id())
        .expect("a raster over a band is owned by the upload path");
    let got = read_back(&device, &queue, texture);

    let mut wrong = 0usize;
    let mut first = None;
    for y in 0..SIDE {
        for x in 0..SIDE {
            let at = (y * SIDE + x) * 4;
            if got[at..at + 4] != texel(x, y) {
                wrong += 1;
                first.get_or_insert((x, y, [got[at], got[at + 1], got[at + 2], got[at + 3]]));
            }
        }
    }
    assert_eq!(
        wrong,
        0,
        "{wrong} of {} texels came back wrong over {frames} frames (ring={with_ring}); \
         the first is at {:?}, which should have been {:?}",
        SIDE * SIDE,
        first,
        first.map(|(x, y, _)| texel(x, y)),
    );
}

/// The DMA route: bands staged through host memory and pulled across by the copy
/// engine, with padded rows.
#[test]
#[ignore = "needs a real GPU adapter with MAPPABLE_PRIMARY_BUFFERS"]
fn a_banded_dma_upload_lands_every_texel_where_it_belongs() {
    every_texel_lands(true);
}

/// The fallback route: `write_texture` per band, packed rows, which is what
/// WebGL2 and GLES take for every band of every raster.
#[test]
#[ignore = "needs a real GPU adapter"]
fn a_banded_write_texture_upload_lands_every_texel_where_it_belongs() {
    every_texel_lands(false);
}
