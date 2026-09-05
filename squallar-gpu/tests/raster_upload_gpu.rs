//! A banded upload puts the same bytes in the same places a single
//! `write_texture` would have.

#![cfg(not(target_arch = "wasm32"))]

use egui_wgpu::wgpu;
use squallar_gpu::egui_renderer::texture_upload::{TextureUploads, UPLOAD_BAND_BYTES};
use squallar_gpu::staging_ring::STAGING_RING_FEATURE;

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
         bands (staged {} B; blocking {} B, {} B of it whole)",
        SIDE * SIDE * 4,
        totals.banded_bytes(),
        totals.staged_bytes,
        totals.blocking_bytes,
        totals.whole_bytes,
    );
    assert!(
        totals.bands > 1,
        "{} band(s) moved a raster of {} B against a {UPLOAD_BAND_BYTES} B band \
         budget, so nothing here was banded",
        totals.bands,
        SIDE * SIDE * 4,
    );
    if with_ring {
        // The whole-route bytes (the font atlas here) are blocking on every
        // device -- `update_texture` is `write_texture` on the frame's own
        // queue. What the ring must keep off the frame thread is every BAND.
        assert_eq!(
            totals.blocking_bytes,
            totals.whole_bytes,
            "{} B of bands went through `write_texture` on a device with a \
             ring; that is frame thread, and it is what the ring exists to \
             avoid",
            totals.blocking_bytes - totals.whole_bytes,
        );
        assert!(totals.staged_bytes > 0);
    } else {
        assert_eq!(
            totals.staged_bytes, 0,
            "{} B were reported staged on a device with no ring",
            totals.staged_bytes,
        );
        assert_eq!(
            totals.blocking_bytes,
            totals.bytes(),
            "a ringless device moves every byte through blocking \
             `write_texture` on the frame thread",
        );
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

/// **One picture either side of the band budget, on a device with no ring:
/// the ledger calls every byte of both blocking.**
///
/// Spike B's pair, measured 2026-08-30 on identical web traffic: Firefox's
/// ~8.51 MB pictures banded and were counted ~13 GB blocking; Chromium's
/// ~7.57 MB pictures went whole and were counted ~0.1 GB — opposite ledger
/// classifications flipped by 32 px of canvas width. Both move every byte
/// through a blocking `write_texture` on the frame thread (a whole delta goes
/// through `Renderer::update_texture`, which is `write_texture` on the frame's
/// own queue), so "which bytes were frame time" must not depend on whether a
/// picture straddles [`UPLOAD_BAND_BYTES`].
#[test]
#[ignore = "needs a real GPU adapter"]
fn a_ringless_byte_is_called_blocking_on_both_sides_of_the_band_straddle() {
    let Some((device, queue, has_ring)) = device(false) else {
        eprintln!("no adapter; nothing to check");
        return;
    };
    assert!(!has_ring, "this run needs the ringless arm");

    let mut renderer = egui_wgpu::Renderer::new(
        &device,
        wgpu::TextureFormat::Bgra8Unorm,
        egui_wgpu::RendererOptions::default(),
    );
    let ctx = egui::Context::default();
    ctx.begin_pass(egui::RawInput {
        max_texture_side: Some(device.limits().max_texture_dimension_2d as usize),
        ..Default::default()
    });
    // 1400 px RGBA is 7 840 000 B — under the 8 MiB band, the Chromium side.
    // 1500 px RGBA is 9 000 000 B — over it, the Firefox side.
    let under = ctx.load_texture(
        "under-the-band",
        egui::ColorImage::filled([1400, 1400], egui::Color32::from_rgb(10, 20, 30)),
        egui::TextureOptions::NEAREST,
    );
    let over = ctx.load_texture(
        "over-the-band",
        egui::ColorImage::filled([1500, 1500], egui::Color32::from_rgb(40, 50, 60)),
        egui::TextureOptions::NEAREST,
    );
    let delta = ctx.end_pass().textures_delta;

    let mut uploads = TextureUploads::new(&device);
    assert!(!uploads.has_ring());
    run_to_completion(
        &mut uploads,
        &device,
        &queue,
        &mut renderer,
        &delta.set,
        None,
    );

    let totals = uploads.totals();
    let traffic = (1400u64 * 1400 + 1500 * 1500) * 4;
    assert!(
        totals.deltas >= 2,
        "the two rasters never reached the ledger (deltas={})",
        totals.deltas,
    );
    assert!(
        totals.bytes() >= traffic,
        "the ledger counted {} B against at least {traffic} B of rasters",
        totals.bytes(),
    );
    assert_eq!(
        totals.blocking_bytes,
        totals.bytes(),
        "a device with no ring moves every byte through a blocking \
         `write_texture` on the frame thread, whichever side of the \
         {UPLOAD_BAND_BYTES}-byte band its picture fell on; the ledger called \
         {} of {} B blocking",
        totals.blocking_bytes,
        totals.bytes(),
    );
    drop((under, over));
}

/// The fallback route: `write_texture` per band, packed rows, which is what
/// WebGL2 and GLES take for every band of every raster.
#[test]
#[ignore = "needs a real GPU adapter"]
fn a_banded_write_texture_upload_lands_every_texel_where_it_belongs() {
    every_texel_lands(false);
}

/// An opaque image of `size`, for a resident figure that only cares how many
/// texels there are.
fn filled(size: [usize; 2]) -> egui::ColorImage {
    egui::ColorImage::from_rgba_premultiplied(size, &vec![255u8; size[0] * size[1] * 4])
}

/// Run one egui pass and take every delta pending for `watch`.
///
/// **One `Context` for a whole test.** A `TextureHandle` holds an `Arc` of the
/// `TextureManager` it was minted from, so a `set` or a `set_partial` reaches
/// that context whichever one a later pass is opened on — and a second context
/// would end its pass holding nothing, which reads as a green "no bytes moved"
/// for a delta that really was filed.
///
/// `max_texture_side` is the adapter's, as `EguiRenderer::new` hands it to
/// `egui_winit::State`: egui's own default is the WebGL2 floor of 2048 and it
/// *panics* on a larger `load_texture`. It is carried on every pass because
/// `Context::load_texture` reads the last input's, and a texture is minted
/// between passes here.
///
/// Filtered to `watch` so nothing egui does on its own account — a font atlas,
/// a stand-in — enters the figures the caller asserts on.
fn take_deltas(
    ctx: &egui::Context,
    device: &wgpu::Device,
    watch: egui::TextureId,
) -> Vec<(egui::TextureId, egui::epaint::ImageDelta)> {
    ctx.begin_pass(egui::RawInput {
        max_texture_side: Some(device.limits().max_texture_dimension_2d as usize),
        ..Default::default()
    });
    ctx.end_pass()
        .textures_delta
        .set
        .into_iter()
        .filter(|(at, _)| *at == watch)
        .collect()
}

/// A context whose `max_texture_side` is this device's, ready to mint from.
fn warm_context(device: &wgpu::Device) -> egui::Context {
    let ctx = egui::Context::default();
    let _ = take_deltas(&ctx, device, egui::TextureId::Managed(u64::MAX));
    ctx
}

/// **The resident texture level rises on an upload and FALLS on a free**,
/// driven through the real `apply` and `free` on a real device.
///
/// The falling half is the one that matters. `UploadTotals` is cumulative flow
/// and only ever climbs — one Tier-2 leg moved 21.7 GB of uploads against a
/// device holding a few hundred MB — so a "resident" figure that rose and never
/// fell would be that counter under a new name. Three things here that a
/// cumulative counter cannot do: a free gives bytes back, a replace costs the
/// new size and not both, and a raster banded over many frames is charged once
/// rather than once per band.
#[test]
#[ignore = "needs a real GPU adapter"]
fn the_resident_texture_level_rises_on_upload_and_falls_on_free() {
    let Some((device, queue, _)) = device(true) else {
        eprintln!("no adapter; nothing to check");
        return;
    };
    let mut renderer = egui_wgpu::Renderer::new(
        &device,
        wgpu::TextureFormat::Bgra8Unorm,
        egui_wgpu::RendererOptions::default(),
    );
    let mut uploads = TextureUploads::new(&device);
    assert_eq!(
        uploads.resident_texture_bytes(),
        0,
        "a renderer that has been shown nothing is holding bytes",
    );
    let ctx = warm_context(&device);
    const SMALL: usize = 64;

    // A raster over the band budget: this module allocates it, and egui keeps
    // the 1x1 stand-in `seed` put under the id.
    let big = ctx.load_texture("big", source(), egui::TextureOptions::NEAREST);
    let set = take_deltas(&ctx, &device, big.id());
    assert_eq!(set.len(), 1, "the raster delta did not reach the renderer");
    let frames = run_to_completion(&mut uploads, &device, &queue, &mut renderer, &set, None);
    assert!(
        frames > 1,
        "a {SIDE}px raster finished in one frame, so the banded path — the one \
         that must charge a raster once and not once per band — never ran",
    );
    let raster = (SIDE * SIDE * 4) as u64;
    assert_eq!(
        uploads.resident_texture_bytes(),
        raster + 4,
        "a {SIDE}px raster charged over {frames} frames of bands, plus the 1x1 \
         stand-in egui still holds under its id",
    );
    let after_big = uploads.totals().bytes();
    assert!(after_big >= raster, "the cumulative total lost the raster");

    // A small texture goes whole through egui's own path and is charged there.
    let small = ctx.load_texture(
        "small",
        filled([SMALL, SMALL]),
        egui::TextureOptions::NEAREST,
    );
    let set = take_deltas(&ctx, &device, small.id());
    assert_eq!(set.len(), 1, "the small delta did not reach the renderer");
    uploads.apply(&device, &queue, &mut renderer, &set);
    let small_bytes = (SMALL * SMALL * 4) as u64;
    assert_eq!(
        uploads.resident_texture_bytes(),
        raster + 4 + small_bytes,
        "the whole-delta route allocated a texture the level did not see",
    );

    // A second raster, then that same id replaced at a smaller size.
    let mut second = ctx.load_texture(
        "second",
        filled([2048, 2048]),
        egui::TextureOptions::NEAREST,
    );
    let set = take_deltas(&ctx, &device, second.id());
    run_to_completion(&mut uploads, &device, &queue, &mut renderer, &set, None);
    let wide = (2048 * 2048 * 4) as u64;
    assert_eq!(
        uploads.resident_texture_bytes(),
        raster + wide + 8 + small_bytes,
        "two rasters, two stand-ins and the small texture",
    );

    // The replace. The old texture is superseded at file time and the new one
    // arrives when the drain allocates it, so the level must end at the NEW
    // size and not at both.
    second.set(filled([1024, 1024]), egui::TextureOptions::NEAREST);
    let set = take_deltas(&ctx, &device, second.id());
    assert_eq!(
        set.len(),
        1,
        "the replacing delta did not reach the renderer"
    );
    assert!(set[0].1.pos.is_none(), "the replace was filed as a partial");
    run_to_completion(&mut uploads, &device, &queue, &mut renderer, &set, None);
    let narrow = (1024 * 1024 * 4) as u64;
    assert_eq!(
        uploads.resident_texture_bytes(),
        raster + narrow + 8 + small_bytes,
        "a replaced raster left its old {wide} B texture on the level, so the \
         figure climbs with the session the way a running total does",
    );

    // And the fall. `EguiRenderer::free_textures` frees on both sides in one
    // breath; this is the same pair.
    for id in [big.id(), small.id(), second.id()] {
        renderer.free_texture(&id);
        uploads.free(&[id]);
    }
    assert_eq!(
        uploads.resident_texture_bytes(),
        0,
        "every texture was retired and the device level did not come back to \
         zero — the falling half is the whole difference between this figure \
         and the cumulative upload total, which is {} B",
        uploads.totals().bytes(),
    );
    assert!(
        uploads.totals().bytes() > after_big,
        "the cumulative total did not climb across the run, so the level's \
         return to zero is not being compared against anything",
    );
}

/// **A raster-atlas page is ONE resident charge of its full size, and the
/// tiles written into it are free.**
///
/// `squallar_egui::raster_atlas` (landed `dfd5daab`) creates a page as one
/// `load_texture` of a 1806x1806 transparent image — 7x7 slots at a 258 pitch
/// inside the 2048 ceiling — and then writes each 256x256 hillshade tile into
/// it as a `set_partial` of a 258x258 gutter-padded patch. The gutter grows a
/// tile's *upload* bytes by 1.57 %, and the cumulative upload total is exactly
/// the instrument that shows that climb for as long as the map scrolls. The
/// resident level must not: the page is allocated once, every tile after it
/// writes into a texture that already exists, and a level that charged the
/// partials would read a scrolling basemap as a leak.
#[test]
#[ignore = "needs a real GPU adapter"]
fn an_atlas_page_is_one_resident_charge_and_its_tiles_are_free() {
    const PAGE: usize = 1806;
    const SLOT: usize = 258;

    let Some((device, queue, _)) = device(true) else {
        eprintln!("no adapter; nothing to check");
        return;
    };
    if (device.limits().max_texture_dimension_2d as usize) < PAGE {
        eprintln!("this adapter cannot hold a {PAGE}px page; nothing to check");
        return;
    }
    let mut renderer = egui_wgpu::Renderer::new(
        &device,
        wgpu::TextureFormat::Bgra8Unorm,
        egui_wgpu::RendererOptions::default(),
    );
    let mut uploads = TextureUploads::new(&device);
    let ctx = warm_context(&device);

    // The page, exactly as `raster_atlas::place` creates it.
    let mut page = ctx.load_texture(
        "atlas-page",
        egui::ColorImage::filled([PAGE, PAGE], egui::Color32::TRANSPARENT),
        Default::default(),
    );
    let set = take_deltas(&ctx, &device, page.id());
    assert_eq!(set.len(), 1, "the page delta did not reach the renderer");
    run_to_completion(&mut uploads, &device, &queue, &mut renderer, &set, None);

    let page_bytes = (PAGE * PAGE * 4) as u64;
    assert_eq!(page_bytes, 13_046_544);
    let after_page = uploads.resident_texture_bytes();
    assert_eq!(
        after_page,
        page_bytes + 4,
        "the page and its stand-in; a page must be one resident allocation of \
         its full size from creation, not the sum of what is written into it",
    );
    let uploaded_after_page = uploads.totals().bytes();

    // Every slot of the page filled, the way the atlas fills them.
    let patch = filled([SLOT, SLOT]);
    let slots = PAGE / SLOT;
    assert_eq!(slots * slots, 49);
    for row in 0..slots {
        for col in 0..slots {
            page.set_partial([col * SLOT, row * SLOT], patch.clone(), Default::default());
            let set = take_deltas(&ctx, &device, page.id());
            assert_eq!(set.len(), 1, "the partial did not reach the renderer");
            assert!(
                set[0].1.pos.is_some(),
                "a tile reached the renderer as a FULL delta, which really \
                 would allocate a texture",
            );
            run_to_completion(&mut uploads, &device, &queue, &mut renderer, &set, None);
        }
    }
    assert_eq!(
        uploads.resident_texture_bytes(),
        after_page,
        "49 tiles written into one page moved the resident level; a scrolling \
         basemap would read as a leak",
    );
    // And the counter that DOES climb, so this is not a pair of zeros: the
    // gutter's 1.57 % is on that figure and on no other.
    let tiles = (slots * slots * SLOT * SLOT * 4) as u64;
    assert!(
        uploads.totals().bytes() >= uploaded_after_page + tiles,
        "the cumulative upload total did not carry the {tiles} B of tile \
         patches, so the resident level's flatness above proves nothing",
    );

    renderer.free_texture(&page.id());
    uploads.free(&[page.id()]);
    assert_eq!(uploads.resident_texture_bytes(), 0);
}
