//! A banded upload puts the same bytes in the same places a single
//! `write_texture` would have.

#![cfg(not(target_arch = "wasm32"))]

use egui_wgpu::wgpu;
use rustdar_gpu::egui_renderer::texture_upload::{TextureUploads, UPLOAD_BAND_BYTES};
use rustdar_gpu::staging_ring::STAGING_RING_FEATURE;

/// The odd, multi-band shape. See the module note.
const SIDE: usize = 3000;

/// A device, with the staging ring feature or deliberately without it.
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
        // Alpha stays opaque: a varying alpha would only test epaint.
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
fn run_to_completion(
    uploads: &mut TextureUploads,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    renderer: &mut egui_wgpu::Renderer,
    set: &[(egui::TextureId, egui::epaint::ImageDelta)],
    watch: Option<egui::TextureId>,
) -> u32 {
    let mut frames = 0;
    let mut pending = uploads.apply(device, queue, renderer, set);
    while pending {
        // While a band is still to move, `is_delivered` has to answer *no*, or
        // a pane swaps onto a half-filled picture.
        if let Some(id) = watch {
            assert!(
                !uploads.is_delivered(id),
                "the raster reported delivered after {frames} frames with bands \
                 still pending, so a pane would swap onto a half-filled picture",
            );
        }
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

/// **Every byte of the raster is counted once, on the route that carried it.**
///
/// This is the exact half of the raster telemetry. `UploadTotals` lives on the
/// renderer rather than in a `static`, so nothing else in the process can move
/// it and the figures below can be `==` rather than `>=` — which is what the
/// process-global overlay ledger cannot do, and why
/// `every_arrival_is_either_a_picture_or_a_drop` asserts an identity and a
/// direction instead.
///
/// Four things, and the first is the non-vacuity floor:
///
/// * `deltas` is positive. Without it `staged_bytes == 0` on the no-ring arm
///   would be satisfied by an upload path that had done nothing at all, which
///   is the shape of every vacuous check this campaign has caught.
/// * the banded total is the raster's own size, **exactly once** — a band
///   counted twice or a partial band counted whole both fail here;
/// * more than one band moved, or the byte figure is not a banded one;
/// * every byte took the route this arm asked for, so the two arms of
///   [`every_texel_lands`] must disagree about the split while agreeing about
///   the total. A counter that were a constant, or that ignored `staged`,
///   would pass one arm and fail the other.
fn the_upload_ledger_counts_every_byte_of_a_banded_raster_once(
    uploads: &TextureUploads,
    deltas: usize,
    with_ring: bool,
) {
    let totals = uploads.totals();
    assert!(
        totals.deltas > 0,
        "the ledger saw no delta at all, so every byte figure below is zero for \
         a reason that has nothing to do with what this test is checking",
    );
    assert_eq!(
        totals.deltas, deltas as u64,
        "egui handed over {deltas} deltas and the ledger counted {}",
        totals.deltas,
    );
    assert_eq!(
        totals.banded_bytes(),
        (SIDE * SIDE * 4) as u64,
        "a {SIDE}px RGBA raster is {} B and the ledger says {} B crossed as \
         bands (staged {} + blocking {})",
        SIDE * SIDE * 4,
        totals.banded_bytes(),
        totals.staged_bytes,
        totals.blocking_bytes,
    );
    assert!(
        totals.bands > 1,
        "{} band(s) moved a raster of {} B against a {UPLOAD_BAND_BYTES} B band \
         budget, so nothing here was banded",
        totals.bands,
        SIDE * SIDE * 4,
    );
    if with_ring {
        assert_eq!(
            totals.blocking_bytes, 0,
            "{} B went through `write_texture` on a device with a ring; that is \
             frame thread, and it is what the ring exists to avoid",
            totals.blocking_bytes,
        );
        assert!(totals.staged_bytes > 0);
    } else {
        assert_eq!(
            totals.staged_bytes, 0,
            "{} B were reported staged on a device with no ring",
            totals.staged_bytes,
        );
        assert_eq!(totals.blocking_bytes, totals.banded_bytes());
    }
}

/// Both routes put every texel exactly where a single `write_texture` would.
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
    // one has been run, and `update_texture` refuses that size.
    let ctx = egui::Context::default();
    // The adapter's limit, as `EguiRenderer::new` hands it to
    // `egui_winit::State`: egui's own default is the WebGL2 floor of 2048 and
    // it *panics* on a larger `load_texture`.
    ctx.begin_pass(egui::RawInput {
        max_texture_side: Some(device.limits().max_texture_dimension_2d as usize),
        ..Default::default()
    });
    let handle = ctx.load_texture("raster", source(), egui::TextureOptions::NEAREST);
    let delta = ctx.end_pass().textures_delta;

    let mut uploads = TextureUploads::new(&device);
    assert_eq!(uploads.has_ring(), with_ring);
    // Before anything is filed the answer is no: the delta is still in egui's
    // `TextureManager`.
    assert!(
        !uploads.is_delivered(handle.id()),
        "an id this module has never been shown reported delivered",
    );
    let frames = run_to_completion(
        &mut uploads,
        &device,
        &queue,
        &mut renderer,
        &delta.set,
        Some(handle.id()),
    );
    assert!(
        uploads.is_delivered(handle.id()),
        "the last band landed and the raster still does not report delivered, so \
         the pane holding it would hold forever",
    );

    // More than one, or the band budget is doing nothing.
    assert!(
        frames > 1,
        "a {SIDE}px raster finished in one frame, so the {} bytes it carries did \
         not exceed a frame's budget and nothing here was exercised",
        SIDE * SIDE * 4,
    );

    the_upload_ledger_counts_every_byte_of_a_banded_raster_once(
        &uploads,
        delta.set.len(),
        with_ring,
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
