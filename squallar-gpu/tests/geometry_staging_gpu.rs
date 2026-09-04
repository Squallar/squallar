//! Geometry that reaches the GPU through cached host memory reaches it
//! unchanged, and really does go that way.
//!
//! `egui_wgpu::Renderer::update_buffers` stages the frame's index and vertex
//! arrays through `Queue::write_buffer_with`, whose mapping `wgpu-hal` places
//! in the card's host-visible BAR window;
//! `squallar_gpu::egui_renderer::geometry_staging` redirects them through a
//! [`squallar_gpu::staging_ring`] slot and a copy-engine transfer instead.
//! Three things have to hold for that to be a fix rather than a change:
//!
//! * **The picture is byte-identical.** Same scene, two renderers, one readback
//!   each, compared texel for texel.
//! * **The same bytes move.** A route that is quicker because it stages less is
//!   not quicker, so the ring's own byte total is checked against the
//!   arithmetic over the primitive list.
//! * **The route is really taken.** Every assertion above passes vacuously on a
//!   renderer whose stager never engages, so the counters are read as well.
//!
//! The fourth test is not a gate. It is the bandwidth reading these figures
//! come from, in the style of [`squallar_gpu::staging_ring`]'s own: measured
//! here, RTX 3090 / Vulkan / debug, 1 MiB to 30 MiB in 1 to 5000 chunks,
//! minimum of five runs —
//!
//! | bytes | chunks | BAR mapping | ring slot |
//! | --- | --- | --- | --- |
//! | 1 MiB | 1 | 2.15 GB/s | 26.9 GB/s |
//! | 1 MiB | 100 | 2.16 GB/s | 55.2 GB/s |
//! | 1 MiB | 5000 | 1.90 GB/s | 7.71 GB/s |
//! | 8 MiB | 1 | 2.15 GB/s | 51.2 GB/s |
//! | 8 MiB | 100 | 2.14 GB/s | 27.2 GB/s |
//! | 8 MiB | 5000 | 1.89 GB/s | 13.4 GB/s |
//! | 30 MiB | 1 | 2.15 GB/s | 23.8 GB/s |
//! | 30 MiB | 100 | 2.15 GB/s | 22.2 GB/s |
//! | 30 MiB | 5000 | 2.07 GB/s | 17.9 GB/s |
//!
//! **Read the two columns differently.** The BAR column is stable to the second
//! decimal across every run taken: 2.15 GB/s, whatever the size and whatever
//! the chunking. The ring column is not — it ranged 7.7 to 65.5 GB/s over three
//! runs on a box with other work on it, because it is a cached-RAM memcpy and
//! is competing for the same cache and the same memory controller. What the
//! table says is an order of magnitude, not a number; the figure to quote is
//! the one below, which is over the function itself.
//!
//! And that, over the function itself rather than one staging: 60 consecutive
//! `update_buffers` calls at 13.75 MB a frame, three runs at different box
//! loads — **BAR mapping 7 657 / 7 766 / 7 800 us a frame, ring 945 / 1 626 /
//! 1 816 us**, 60 staged and 0 declined every time. Read those two spreads the
//! way the table above is read: the BAR side varies by 1.9% and the ring side
//! by 1.9x, and the whole of the ring's spread is the box, because a cached-RAM
//! memcpy competes for cache and a BAR write does not. **The cut is 4.2x at
//! worst and 8.1x at best**; the worst is the honest one to plan with and the
//! best is what a quiet machine actually gets
//! (`what_a_run_of_frames_costs_through_each_route`, debug profile with this
//! crate and `egui-wgpu` at `opt-level = 2`). Both readings are `#[ignore]`d
//! like every other suite in this directory; run them with
//! `cargo test -p squallar-gpu --test geometry_staging_gpu -- --ignored`.

#![cfg(not(target_arch = "wasm32"))]

use std::num::NonZeroU64;

use egui_wgpu::wgpu;
use squallar_gpu::egui_renderer::geometry_staging::{GeometryStaging, GeometryStagingLedger};
use squallar_gpu::staging_ring::STAGING_RING_FEATURE;

/// The offscreen target's side, in texels. Small: the picture is compared, not
/// looked at, and every mesh in [`scene`] is placed inside it.
const SIDE: u32 = 512;

/// A device with the staging-ring feature if the adapter has it.
fn device() -> Option<(wgpu::Device, wgpu::Queue, wgpu::AdapterInfo)> {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        compatible_surface: None,
        force_fallback_adapter: false,
    }))
    .ok()?;
    let info = adapter.get_info();
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("geometry-staging"),
        required_features: adapter.features() & STAGING_RING_FEATURE,
        required_limits: adapter.limits(),
        memory_hints: wgpu::MemoryHints::default(),
        experimental_features: wgpu::ExperimentalFeatures::default(),
        trace: wgpu::Trace::Off,
    }))
    .ok()?;
    Some((device, queue, info))
}

/// A picture with many small meshes and a lot of them — the shape a UI frame
/// actually has. Text is what makes the mesh count high (four vertices a glyph
/// quad, one mesh a galley); the rects and circles are what make any single
/// mesh big enough that a dropped one would show.
fn scene(ctx: &egui::Context) -> (Vec<egui::ClippedPrimitive>, egui::TexturesDelta) {
    let canvas = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(SIDE as f32, SIDE as f32));
    ctx.begin_pass(egui::RawInput {
        screen_rect: Some(canvas),
        ..Default::default()
    });
    for row in 0..24 {
        for column in 0..12 {
            let at = egui::pos2(column as f32 * 42.0 + 2.0, row as f32 * 21.0 + 2.0);
            // A clip rect per cell, so the tessellator emits one primitive per
            // cell rather than folding the whole scene into a single mesh. A
            // one-mesh picture cannot tell a staging route that loses a mesh
            // from one that does not.
            let painter = ctx
                .layer_painter(egui::LayerId::background())
                .with_clip_rect(egui::Rect::from_min_size(at, egui::vec2(40.0, 20.0)));
            painter.rect_filled(
                egui::Rect::from_min_size(at, egui::vec2(38.0, 9.0)),
                2.0,
                egui::Color32::from_rgb(
                    (row * 9) as u8,
                    (column * 17) as u8,
                    ((row + column) * 5) as u8,
                ),
            );
            painter.circle_filled(
                at + egui::vec2(20.0, 15.0),
                4.0,
                egui::Color32::from_rgb(200, (row * 7) as u8, (column * 11) as u8),
            );
            painter.text(
                at + egui::vec2(0.0, 10.0),
                egui::Align2::LEFT_TOP,
                format!("{row}.{column}"),
                egui::FontId::proportional(9.0),
                egui::Color32::WHITE,
            );
        }
    }
    let output = ctx.end_pass();
    let tris = ctx.tessellate(output.shapes, 1.0);
    (tris, output.textures_delta)
}

/// Mesh vertices, mesh indices and the bytes they occupy — the same
/// arithmetic `update_buffers` sizes its staging with, and the same
/// `squallar_gpu::egui_renderer::pass_costs::StagedGeometry` records.
fn geometry(tris: &[egui::ClippedPrimitive]) -> (u64, u64, u64) {
    let (vertices, indices) =
        tris.iter()
            .fold((0u64, 0u64), |acc, clipped| match &clipped.primitive {
                egui::epaint::Primitive::Mesh(mesh) => (
                    acc.0 + mesh.vertices.len() as u64,
                    acc.1 + mesh.indices.len() as u64,
                ),
                egui::epaint::Primitive::Callback(_) => acc,
            });
    (
        vertices,
        indices,
        vertices * size_of::<egui::epaint::Vertex>() as u64 + indices * size_of::<u32>() as u64,
    )
}

fn renderer(device: &wgpu::Device, format: wgpu::TextureFormat) -> egui_wgpu::Renderer {
    egui_wgpu::Renderer::new(
        device,
        format,
        egui_wgpu::RendererOptions {
            depth_stencil_format: None,
            msaa_samples: 1,
            dithering: squallar_gpu::egui_renderer::EGUI_DITHERING,
            ..Default::default()
        },
    )
}

/// Draw `tris` into a fresh target and read it back, tightly packed RGBA.
fn draw(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    renderer: &mut egui_wgpu::Renderer,
    format: wgpu::TextureFormat,
    tris: &[egui::ClippedPrimitive],
    deltas: &egui::TexturesDelta,
) -> Vec<u8> {
    // egui's mesh arm looks its texture up by id and silently draws nothing
    // without it, so an unuploaded atlas would make both arms agree on an
    // empty picture.
    for (id, delta) in &deltas.set {
        renderer.update_texture(device, queue, *id, delta);
    }

    let descriptor = egui_wgpu::ScreenDescriptor {
        size_in_pixels: [SIDE, SIDE],
        pixels_per_point: 1.0,
    };
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("geometry-staging target"),
        size: wgpu::Extent3d {
            width: SIDE,
            height: SIDE,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
    let user = renderer.update_buffers(device, queue, &mut encoder, tris, &descriptor);
    {
        let pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("geometry-staging pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &view,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                    store: wgpu::StoreOp::Store,
                },
                depth_slice: None,
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        renderer.render(&mut pass.forget_lifetime(), tris, &descriptor);
    }
    queue.submit(user.into_iter().chain([encoder.finish()]));
    read_back(device, queue, &texture)
}

fn read_back(device: &wgpu::Device, queue: &wgpu::Queue, texture: &wgpu::Texture) -> Vec<u8> {
    let row = SIDE as usize * 4;
    let padded = row.next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT as usize);
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("geometry-staging readback"),
        size: (padded * SIDE as usize) as u64,
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
                rows_per_image: Some(SIDE),
            },
        },
        wgpu::Extent3d {
            width: SIDE,
            height: SIDE,
            depth_or_array_layers: 1,
        },
    );
    queue.submit(Some(encoder.finish()));
    buffer.slice(..).map_async(wgpu::MapMode::Read, |_| {});
    device
        .poll(wgpu::PollType::wait_indefinitely())
        .expect("the readback drains");
    let view = buffer.slice(..).get_mapped_range();
    let mut out = Vec::with_capacity(row * SIDE as usize);
    for y in 0..SIDE as usize {
        out.extend_from_slice(&view[y * padded..y * padded + row]);
    }
    drop(view);
    buffer.unmap();
    out
}

/// The whole gate, in one device: the two routes draw the same picture, the
/// ring moves the same bytes the picture has, and the ring was really used.
///
/// One test rather than three because each of the three costs an adapter, a
/// device and a font atlas, and because the third is what keeps the first two
/// from passing vacuously: they are one claim.
#[test]
#[ignore = "needs a real GPU adapter with MAPPABLE_PRIMARY_BUFFERS"]
fn the_ring_route_draws_the_same_picture_out_of_the_same_bytes() {
    let Some((device, queue, info)) = device() else {
        eprintln!("no wgpu adapter: nothing measured, nothing asserted");
        return;
    };
    if !squallar_gpu::egui_renderer::geometry_staging::available(&device) {
        eprintln!(
            "{} / {:?} has no MAPPABLE_PRIMARY_BUFFERS: nothing measured, nothing asserted",
            info.name, info.backend
        );
        return;
    }
    let format = wgpu::TextureFormat::Rgba8Unorm;

    let ctx = egui::Context::default();
    let (tris, deltas) = scene(&ctx);
    let (vertices, indices, bytes) = geometry(&tris);
    eprintln!(
        "scene: {} primitives, {vertices} vertices, {indices} indices, {bytes} B",
        tris.len()
    );
    assert!(
        vertices > 10_000,
        "the scene tessellated to {vertices} vertices in {} primitives, which is \
         too small a picture to tell a staging route from a no-op",
        tris.len(),
    );

    // The queue route: no stager, which is upstream's behaviour and every
    // device without the ring feature.
    let mut queue_route = renderer(&device, format);
    let through_queue = draw(&device, &queue, &mut queue_route, format, &tris, &deltas);

    // The ring route.
    let ledger = GeometryStagingLedger::default();
    let mut ring_route = renderer(&device, format);
    ring_route.set_geometry_stager(Box::new(GeometryStaging::new(&ledger)));
    let through_ring = draw(&device, &queue, &mut ring_route, format, &tris, &deltas);

    let totals = ledger.totals();
    assert_eq!(
        (totals.staged, totals.declined),
        (1, 0),
        "the ring route staged {} time(s) and was declined {} time(s) over one \
         `update_buffers` call. Every other assertion in this test passes \
         unchanged on a renderer that never took the ring, so this is the one \
         that says the picture below was drawn out of ring-staged bytes.",
        totals.staged,
        totals.declined,
    );
    assert_eq!(
        totals.bytes, bytes,
        "the ring moved {} B for a picture of {vertices} vertices and {indices} \
         indices, which is {bytes} B. A staging route that is faster because it \
         stages less is not faster.",
        totals.bytes,
    );

    let differing = through_queue
        .iter()
        .zip(&through_ring)
        .filter(|(a, b)| a != b)
        .count();
    assert_eq!(
        differing,
        0,
        "{differing} of {} bytes differ between the queue-staged and \
         ring-staged pictures of the same {vertices}-vertex scene",
        through_queue.len(),
    );
}

/// The bytes really do land somewhere else.
///
/// The claim the whole change rests on is that a `MAP_READ | COPY_SRC` buffer
/// is allocated out of *cached system RAM* while the `MAP_WRITE | COPY_SRC`
/// buffer `Queue::write_buffer_with` maps is allocated out of the card's
/// host-visible BAR window. That is a property of the driver's memory types
/// and of `gpu-allocator`'s choice among them, and nothing about the
/// `BufferUsages` bits passed in makes it true — so it is read here rather
/// than assumed.
///
/// **What this checks and what it does not.** `gpu-allocator` 0.28 picks the
/// first memory type, in index order, whose properties contain the preferred
/// set for the requested `MemoryLocation` (`vulkan/mod.rs`, `allocate` and
/// `find_memorytype_index`); the two preferred sets are spelled below. That
/// walk is replicated here over the physical device's real memory types. What
/// is *not* read is the `memoryTypeBits` of the actual slot buffer —
/// `wgpu_hal::vulkan::Buffer` keeps its handle private, so no allocation's own
/// memory type is reachable through any public API. The replication is
/// therefore exact whenever a buffer may use every host-visible type, which is
/// the ordinary case and is what both routes' buffers ask for.
///
/// **Which devices assert and which skip.** The property is a discrete-GPU
/// one: it needs a host-visible memory type that is *not* `DEVICE_LOCAL`, so
/// that a ring slot has somewhere to live other than the BAR window. A
/// discrete card (the NVIDIA and AMD desktop parts) exposes such a type and
/// takes the assertion arm. A device with one unified heap — Mesa's llvmpipe,
/// Apple Silicon, most integrated GPUs — marks every host-visible type
/// `DEVICE_LOCAL` as well; there the property cannot hold by construction,
/// whichever type the allocator picks, and the test prints why and asserts
/// nothing. The other skip, a device with no `HOST_CACHED` type at all, is the
/// same shape: no cached heap exists to move into, so there is nothing to
/// check.
#[cfg(any(
    target_os = "windows",
    target_os = "linux",
    target_os = "android",
    target_os = "freebsd"
))]
#[test]
#[ignore = "needs a real GPU adapter"]
fn the_ring_and_the_queue_allocate_out_of_different_heaps() {
    // Vulkan's `VkMemoryPropertyFlagBits`, which are ABI and so may be named by
    // value. Spelled here rather than imported because `ash` is not a
    // dependency of this workspace -- it arrives under `wgpu-hal` -- and
    // naming the type would make it one.
    const DEVICE_LOCAL: u32 = 0x0000_0001;
    const HOST_VISIBLE: u32 = 0x0000_0002;
    const HOST_COHERENT: u32 = 0x0000_0004;
    const HOST_CACHED: u32 = 0x0000_0008;

    let Some((device, _queue, info)) = device() else {
        eprintln!("no wgpu adapter: nothing measured, nothing asserted");
        return;
    };
    if info.backend != wgpu::Backend::Vulkan {
        eprintln!(
            "{} runs on {:?}, not Vulkan: nothing measured, nothing asserted",
            info.name, info.backend
        );
        return;
    }

    // SAFETY: the handles are only read from, and only while `device` -- which
    // owns them -- is alive.
    let flags: Vec<u32> = unsafe {
        let hal = device
            .as_hal::<wgpu::hal::api::Vulkan>()
            .expect("the backend said Vulkan");
        let properties = hal
            .shared_instance()
            .raw_instance()
            .get_physical_device_memory_properties(hal.raw_physical_device());
        properties.memory_types[..properties.memory_type_count as usize]
            .iter()
            .map(|memory_type| memory_type.property_flags.as_raw())
            .collect()
    };

    let first_containing = |wanted: u32| flags.iter().position(|&have| have & wanted == wanted);

    // `MemoryLocation::GpuToCpu`, which is what `MAP_READ | COPY_SRC` maps to
    // and so what a ring slot is allocated out of.
    let ring = first_containing(HOST_VISIBLE | HOST_COHERENT | HOST_CACHED);
    // `MemoryLocation::CpuToGpu`, which is what `MAP_WRITE | COPY_SRC` maps to
    // and so what `Queue::write_buffer_with` hands back.
    let bar = first_containing(HOST_VISIBLE | HOST_COHERENT | DEVICE_LOCAL);

    let Some(ring) = ring else {
        eprintln!(
            "{} offers no HOST_CACHED memory type at all ({flags:x?}): there is no \
             cached heap to move geometry into, and this change buys nothing here. \
             Nothing asserted.",
            info.name,
        );
        return;
    };

    // A unified-memory device: every host-visible type is also DEVICE_LOCAL,
    // so "not in the BAR window" has no type to be true of. The assertion
    // below would then report the allocator's pick as wrong when no pick could
    // be right.
    let host_only = flags
        .iter()
        .any(|&have| have & HOST_VISIBLE != 0 && have & DEVICE_LOCAL == 0);
    if !host_only {
        let listed: Vec<String> = flags.iter().map(|have| format!("{have:#x}")).collect();
        eprintln!(
            "{} exposes a single unified heap: every HOST_VISIBLE memory type is also \
             DEVICE_LOCAL ([{}]). A ring slot cannot leave the BAR window here because \
             there is nowhere else to go; the change is a no-op on this device and the \
             byte figures will say so. Nothing asserted.",
            info.name,
            listed.join(", "),
        );
        return;
    }

    assert_eq!(
        flags[ring] & DEVICE_LOCAL,
        0,
        "the memory type `gpu-allocator` gives a ring slot (index {ring}, flags \
         {:#x}) is DEVICE_LOCAL, i.e. it IS the BAR window. Geometry staged \
         through it crosses PCIe at host-store speed exactly as before; the \
         change is a no-op on this device and the byte figures will say so.",
        flags[ring],
    );

    match bar {
        Some(bar) => assert_ne!(
            bar, ring,
            "`Queue::write_buffer_with`'s memory type and a ring slot's are the \
             same type (index {ring}, flags {:#x}), so the two routes are the \
             same memory and nothing has moved.",
            flags[ring],
        ),
        None => eprintln!(
            "{} offers no HOST_VISIBLE|HOST_COHERENT|DEVICE_LOCAL type, so \
             `write_buffer_with` falls to its required set rather than its \
             preferred one. The ring's heap is still cached and non-local, \
             which is what was asserted.",
            info.name,
        ),
    }

    eprintln!(
        "{} / Vulkan: {} memory types, ring slot -> index {ring} ({:#x}), \
         write_buffer_with -> {bar:?}",
        info.name,
        flags.len(),
        flags[ring],
    );
}

/// What the two routes cost `update_buffers` itself, over a run of frames.
///
/// **Not a gate**, for the reason the reading below is not one. It exists
/// because the per-byte table cannot answer the one question the real path
/// raises and a single staging cannot: a ring slot comes back through
/// `map_async`, which resolves only once the copy reading it has drained, and
/// at [`squallar_gpu::staging_ring::STAGING_RING_DEPTH`] slots a run of frames
/// can outpace it. A route that is ten times quicker on the frames it serves
/// and declines every second frame is not ten times quicker. `declined` is
/// printed beside the clock so the two are read together.
#[test]
#[ignore = "needs a real GPU adapter with MAPPABLE_PRIMARY_BUFFERS"]
fn what_a_run_of_frames_costs_through_each_route() {
    /// Frames driven per route. Enough that a ring which can only serve every
    /// other frame shows up in `declined` rather than hiding in a mean.
    const FRAMES: usize = 60;
    /// Copies of the scene per frame, to reach a byte volume in the range the
    /// app actually stages (native scene A's heaviest window: 32.6 MB a call).
    const COPIES: usize = 24;

    let Some((device, queue, info)) = device() else {
        eprintln!("no wgpu adapter: nothing measured, nothing asserted");
        return;
    };
    if !squallar_gpu::egui_renderer::geometry_staging::available(&device) {
        eprintln!(
            "{} has no MAPPABLE_PRIMARY_BUFFERS: nothing measured",
            info.name
        );
        return;
    }
    let format = wgpu::TextureFormat::Rgba8Unorm;
    let ctx = egui::Context::default();
    let (one, deltas) = scene(&ctx);
    let tris: Vec<egui::ClippedPrimitive> = std::iter::repeat_n(one, COPIES).flatten().collect();
    let (_, _, bytes) = geometry(&tris);

    let descriptor = egui_wgpu::ScreenDescriptor {
        size_in_pixels: [SIDE, SIDE],
        pixels_per_point: 1.0,
    };

    let run = |ledger: Option<&GeometryStagingLedger>| -> u128 {
        let mut renderer = renderer(&device, format);
        if let Some(ledger) = ledger {
            renderer.set_geometry_stager(Box::new(GeometryStaging::new(ledger)));
        }
        for (id, delta) in &deltas.set {
            renderer.update_texture(&device, &queue, *id, delta);
        }
        let mut total = 0;
        for _ in 0..FRAMES {
            let mut encoder =
                device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
            let started = std::time::Instant::now();
            let user = renderer.update_buffers(&device, &queue, &mut encoder, &tris, &descriptor);
            total += started.elapsed().as_micros();
            queue.submit(user.into_iter().chain([encoder.finish()]));
            let _ = device.poll(wgpu::PollType::wait_indefinitely());
        }
        total
    };

    let queue_us = run(None);
    let ledger = GeometryStagingLedger::default();
    let ring_us = run(Some(&ledger));
    let totals = ledger.totals();

    assert_eq!(
        totals.staged + totals.declined,
        FRAMES as u64,
        "the ring route saw {} of {FRAMES} frames; the rest staged nothing, so \
         the clocks below are over two different sets of frames",
        totals.staged + totals.declined,
    );

    eprintln!(
        "{} / {:?}: {bytes} B a frame over {FRAMES} frames -- \
         update_buffers through the BAR mapping {:.0} us a frame, through the \
         ring {:.0} us a frame ({} staged, {} declined)",
        info.name,
        info.backend,
        queue_us as f64 / FRAMES as f64,
        ring_us as f64 / FRAMES as f64,
        totals.staged,
        totals.declined,
    );
}

/// The bandwidth reading in this file's module note. **Not a gate**: it prints
/// what the two routes cost per byte on the machine it runs on and asserts
/// nothing about the clock, because a wall-clock threshold on a shared box
/// reddens correctness rows for load rather than for regressions.
///
/// What it *does* assert is that both routes moved the bytes at all, which is
/// what keeps a printed zero readable.
#[test]
#[ignore = "needs a real GPU adapter with MAPPABLE_PRIMARY_BUFFERS"]
fn what_each_staging_route_costs_per_byte() {
    let Some((device, queue, info)) = device() else {
        eprintln!("no wgpu adapter: nothing measured, nothing asserted");
        return;
    };
    if !squallar_gpu::egui_renderer::geometry_staging::available(&device) {
        eprintln!(
            "{} has no MAPPABLE_PRIMARY_BUFFERS: nothing measured",
            info.name
        );
        return;
    }
    eprintln!("{} / {:?}", info.name, info.backend);

    for &bytes in &[1usize << 20, 8 << 20, 30 << 20] {
        for &chunks in &[1usize, 100, 5000] {
            let source = vec![0xABu8; bytes];
            let chunk = bytes / chunks;
            let destination = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("bandwidth destination"),
                size: bytes as u64,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });

            let mut window_us = u128::MAX;
            for _ in 0..5 {
                let started = std::time::Instant::now();
                {
                    let Some(mut view) = queue.write_buffer_with(
                        &destination,
                        0,
                        NonZeroU64::new(bytes as u64).expect("a non-zero size"),
                    ) else {
                        panic!("the queue refused a {bytes} B staging buffer");
                    };
                    let mut at = 0;
                    for _ in 0..chunks {
                        view.slice(at..at + chunk)
                            .copy_from_slice(&source[at..at + chunk]);
                        at += chunk;
                    }
                }
                window_us = window_us.min(started.elapsed().as_micros());
                queue.submit([]);
                let _ = device.poll(wgpu::PollType::wait_indefinitely());
            }

            let slot = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("bandwidth staging"),
                // The ring's pair, and only this pair. See
                // `squallar_gpu::staging_ring`.
                usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_SRC,
                size: bytes as u64,
                mapped_at_creation: false,
            });
            let mut ring_us = u128::MAX;
            for _ in 0..5 {
                slot.slice(..).map_async(wgpu::MapMode::Read, |result| {
                    assert!(result.is_ok(), "the staging slot maps");
                });
                let _ = device.poll(wgpu::PollType::wait_indefinitely());
                let started = std::time::Instant::now();
                {
                    let mut view = slot.get_mapped_range_mut(..bytes as u64);
                    let mut at = 0;
                    for _ in 0..chunks {
                        view.slice(at..at + chunk)
                            .copy_from_slice(&source[at..at + chunk]);
                        at += chunk;
                    }
                }
                ring_us = ring_us.min(started.elapsed().as_micros());
                slot.unmap();
                let mut encoder =
                    device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
                encoder.copy_buffer_to_buffer(&slot, 0, &destination, 0, bytes as u64);
                queue.submit(Some(encoder.finish()));
                let _ = device.poll(wgpu::PollType::wait_indefinitely());
            }

            assert!(
                window_us > 0 && ring_us > 0,
                "one of the two routes moved {bytes} B in under a microsecond, \
                 which is a clock that stopped rather than a copy that happened",
            );
            let rate = |us: u128| bytes as f64 / (us as f64 * 1e-6) / 1e9;
            eprintln!(
                "{bytes:>9} B in {chunks:>5} chunks | BAR mapping {window_us:>7} us \
                 ({:>5.2} GB/s) | ring slot {ring_us:>7} us ({:>5.2} GB/s)",
                rate(window_us),
                rate(ring_us),
            );
        }
    }
}
