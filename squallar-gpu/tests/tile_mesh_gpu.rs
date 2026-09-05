//! A tile's fills and strokes drawn through the GPU path put the **same bytes
//! on the screen** as the CPU placement path they replace.
//!
//! This is the gate the whole mechanism turns on. The two paths reach the same
//! render pass through two different pipelines: egui's, which samples the font
//! atlas at `WHITE_UV` and applies one of two gamma conventions chosen off the
//! target's sRGB-ness, and `squallar_gpu::tile_mesh`'s, which multiplies by a
//! constant one and mirrors the same choice. Everything either of them could
//! get wrong — the entry point, the dither, the blend state, the vertex
//! colour unpack, the clip-space map — shows up as different pixels, so the
//! comparison is a byte compare of two readbacks and not a tolerance.
//!
//! # Why the placement is a power of two
//!
//! `ShapeOrText::placed` computes `scaling * p + translation` in `f32` on the
//! CPU; the shader computes the same expression on the GPU, where a driver may
//! contract the multiply and the add into one FMA and round once instead of
//! twice. At a tile side of 256 points the scale is `256/4096 = 1/16` — an
//! exact power of two, so `scaling * p` is exact whatever the rounding mode
//! and both spellings agree bit for bit. That is also the shipping-typical
//! case (a whole zoom step at tile zoom bias 0), so the gate is not measuring
//! an artificial arrangement; it is measuring the one where a difference can
//! only be the shader's.
//!
//! # Both gamma conventions
//!
//! Every case below runs twice, on an sRGB target and a non-sRGB one, because
//! that bit is what picks egui's fragment entry point and therefore what this
//! shader has to mirror. The two readbacks are asserted to **differ from each
//! other**, which is the interleaved control: it proves the comparison can see
//! a gamma difference at all, so a pass on either arm is a real agreement
//! rather than a byte compare of two identically-wrong pictures.

#![cfg(not(target_arch = "wasm32"))]

use egui_wgpu::wgpu;
use squallar_egui::tile_mesh::{self, TileMeshPainter};
use squallar_gpu::egui_renderer::{AttachmentConfig, EGUI_DITHERING};
use squallar_gpu::tile_mesh::{TileMeshBridge, TileMeshStore};

/// The canvas, in points and (at one point per pixel) in texels.
const SIDE: u32 = 256;

/// The MVT extent every styled tile's geometry is in.
const EXTENT: f32 = 4096.0;

/// The tile's piece on screen: origin at zero, 256 points across, so the
/// placement is `1/16 * p + 0` — see the module doc.
fn piece() -> egui::Rect {
    egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(SIDE as f32, SIDE as f32))
}

/// The tile's fills: overlapping translucent quads in several colours, so the
/// blend state is exercised rather than only the shader's arithmetic, and so a
/// dither difference has gradients to show up in.
fn fills() -> egui::epaint::Mesh {
    let mut mesh = egui::epaint::Mesh::default();
    for (i, colour) in [
        egui::Color32::from_rgba_premultiplied(200, 30, 40, 255),
        egui::Color32::from_rgba_premultiplied(20, 120, 60, 160),
        egui::Color32::from_rgba_premultiplied(70, 70, 200, 90),
        egui::Color32::from_rgba_premultiplied(11, 13, 17, 200),
    ]
    .into_iter()
    .enumerate()
    {
        let at = i as f32 * 400.0;
        mesh.add_rect_with_uv(
            egui::Rect::from_min_size(
                egui::pos2(at, at * 0.5),
                egui::vec2(EXTENT * 0.6, EXTENT * 0.4),
            ),
            egui::Rect::from_min_max(egui::epaint::WHITE_UV, egui::epaint::WHITE_UV),
            colour,
        );
    }
    mesh
}

/// The fixture's fills, through the map's own flattener.
fn flat() -> std::sync::Arc<tile_mesh::TileMeshes> {
    std::sync::Arc::new(tile_mesh::flatten_meshes(std::iter::once((0, &fills()))))
}

/// The feathering the stroke fixture is flattened and drawn at.
///
/// egui's default `feathering_size_in_pixels` over the `pixels_per_point` the
/// frames below use, which is 1: the canvas is [`SIDE`] points and [`SIDE`]
/// texels.
const FEATHERING: f32 = 1.0;

/// The tile's strokes: five polylines with corners of every kind — a gentle
/// bend, a right angle, and one sharper than a right angle, which is the
/// branch that splits a path point in two — in translucent colours so the
/// feathered edges have to blend the same way on both paths.
///
/// **The last one is a hairline**, thinner than [`FEATHERING`], so it takes
/// epaint's three-edge ridge branch rather than the thick one. Both branches
/// are in one fixture deliberately: they draw through the same pipeline out of
/// the same buffer, so a gate over only one of them would leave the other with
/// no picture ever compared. `the_stroke_callback_path_...` asserts both are
/// present rather than trusting these numbers to stay on the right sides of
/// the threshold.
///
/// Coordinates are integers, as MVT geometry is, so the `i16` position the
/// packed vertex carries is exact.
fn strokes() -> Vec<egui::epaint::PathShape> {
    /// One fixture line: its centreline in extent units, its colour and its
    /// width in screen points.
    struct Line {
        points: &'static [(f32, f32)],
        colour: egui::Color32,
        width: f32,
    }
    let lines: [Line; 5] = [
        Line {
            points: &[(200.0, 200.0), (3800.0, 600.0), (3600.0, 3600.0)],
            colour: egui::Color32::from_rgba_premultiplied(220, 40, 40, 255),
            width: 9.0,
        },
        Line {
            // A right angle, and then one much sharper than a right angle.
            points: &[
                (400.0, 3600.0),
                (2000.0, 3600.0),
                (2000.0, 1200.0),
                (1700.0, 3400.0),
            ],
            colour: egui::Color32::from_rgba_premultiplied(30, 140, 70, 190),
            width: 5.0,
        },
        Line {
            points: &[(100.0, 2048.0), (3900.0, 2048.0)],
            colour: egui::Color32::from_rgba_premultiplied(60, 60, 210, 120),
            width: 13.0,
        },
        Line {
            points: &[(3900.0, 100.0), (100.0, 3900.0)],
            colour: egui::Color32::from_rgba_premultiplied(200, 200, 40, 80),
            width: 2.0,
        },
        // A hairline: 0.5 <= 0.9 * FEATHERING, so epaint paints it as a ridge
        // two feather-widths wide with the thinness in the opacity.
        Line {
            points: &[(300.0, 3000.0), (2400.0, 900.0), (3800.0, 2600.0)],
            colour: egui::Color32::from_rgba_premultiplied(240, 120, 200, 255),
            width: 0.5,
        },
    ];
    lines
        .into_iter()
        .map(|line| {
            egui::epaint::PathShape::line(
                line.points.iter().map(|&(x, y)| egui::pos2(x, y)).collect(),
                egui::Stroke::new(line.width, line.colour),
            )
        })
        .collect()
}

/// The fixture's strokes, through the map's own flattener.
fn flat_strokes(paths: &[egui::epaint::PathShape]) -> std::sync::Arc<tile_mesh::TileMeshes> {
    std::sync::Arc::new(tile_mesh::flatten_paths(
        paths.iter().enumerate().map(|(i, p)| (i as u32, p)),
        FEATHERING,
    ))
}

/// The CPU path's shapes for the strokes: the paths placed by
/// `scale * p + translation`, which is exactly what `ShapeOrText::placed`'s
/// path arm produces and what `paint_vector_tile` pushes today.
///
/// **This arm is egui's own tessellator**, not a re-derivation: the whole
/// question is whether the pre-computed offsets put the same triangles on
/// screen as epaint would, so epaint has to be the one drawing the control.
fn cpu_stroke_shapes(paths: &[egui::epaint::PathShape]) -> Vec<egui::Shape> {
    let place = tile_mesh::Placement::of(piece());
    paths
        .iter()
        .map(|path| {
            let mut placed = path.clone();
            for point in &mut placed.points {
                *point = egui::pos2(
                    place.scale * point.x + place.translation[0],
                    place.scale * point.y + place.translation[1],
                );
            }
            egui::Shape::Path(placed)
        })
        .collect()
}

/// Held for the length of a test, so only one talks to the GPU at a time —
/// the convention `volume_silhouette.rs` and `volume_shader_mutants.rs`
/// already carry, and this suite needs it for the same reason.
///
/// **Not tidiness: without it this suite hangs, and the rate was measured.**
/// Each test builds its own `Instance`, `Device` and `Queue`, and every
/// readback ends in `Device::poll(wait_indefinitely)`. Three of those alive on
/// three threads against one adapter deadlock: on the RTX 3090, with the box
/// otherwise idle, **5 of 12 runs failed to finish inside 45 s without this
/// lock and 0 of 12 with it** (the passing run takes 0.65 s). A suite that
/// hangs is worse than one that fails — the derived `gpu` job in `test.yaml`
/// would wait out its timeout with nothing to read — and it hangs *sometimes*,
/// which is worse again: the first three runs of this file all passed.
static ONE_AT_A_TIME: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Take the GPU lock, ignoring poisoning — an earlier failure reports itself.
fn gpu_lock() -> std::sync::MutexGuard<'static, ()> {
    ONE_AT_A_TIME
        .lock()
        .unwrap_or_else(|held| held.into_inner())
}

fn device() -> Option<(wgpu::Device, wgpu::Queue)> {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        compatible_surface: None,
        force_fallback_adapter: false,
    }))
    .ok()?;
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("tile-mesh"),
        required_features: wgpu::Features::empty(),
        required_limits: adapter.limits(),
        memory_hints: wgpu::MemoryHints::default(),
        experimental_features: wgpu::ExperimentalFeatures::default(),
        trace: wgpu::Trace::Off,
    }))
    .ok()?;
    Some((device, queue))
}

fn target(device: &wgpu::Device, format: wgpu::TextureFormat) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some("tile-mesh target"),
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
    })
}

fn read_back(device: &wgpu::Device, queue: &wgpu::Queue, texture: &wgpu::Texture) -> Vec<u8> {
    let row = SIDE as usize * 4;
    let padded = row.next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT as usize);
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("tile-mesh readback"),
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

/// One frame: `shapes` painted into `piece()`'s clip, tessellated by egui,
/// drawn by egui's renderer into a fresh target, read back.
///
/// Both paths go through this, so the pass, the clear, the descriptor and the
/// tessellator are shared and the only difference is what is in `shapes`.
fn frame(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    renderer: &mut egui_wgpu::Renderer,
    format: wgpu::TextureFormat,
    shapes: Vec<egui::Shape>,
) -> Vec<u8> {
    let ctx = egui::Context::default();
    let canvas = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(SIDE as f32, SIDE as f32));
    ctx.begin_pass(egui::RawInput {
        screen_rect: Some(canvas),
        ..Default::default()
    });
    ctx.layer_painter(egui::LayerId::background())
        .with_clip_rect(piece())
        .extend(shapes);
    let output = ctx.end_pass();
    let tris = ctx.tessellate(output.shapes, 1.0);

    // egui's own mesh arm looks its texture up by id and silently draws
    // nothing without it, so the atlas has to be uploaded or the CPU arm of
    // the comparison would be an empty picture agreeing with nothing.
    for (id, delta) in &output.textures_delta.set {
        renderer.update_texture(device, queue, *id, delta);
    }

    let descriptor = egui_wgpu::ScreenDescriptor {
        size_in_pixels: [SIDE, SIDE],
        pixels_per_point: 1.0,
    };
    let texture = target(device, format);
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
    let user = renderer.update_buffers(device, queue, &mut encoder, &tris, &descriptor);
    {
        let pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("tile-mesh pass"),
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
        renderer.render(&mut pass.forget_lifetime(), &tris, &descriptor);
    }
    let mut buffers = user;
    buffers.push(encoder.finish());
    queue.submit(buffers);

    read_back(device, queue, &texture)
}

/// The CPU path's shape: the flattened fills placed by
/// `scale * p + translation`, which is `ShapeOrText::placed`'s mesh arm.
///
/// **Built from the same flat buffers the callback path draws**, so the two
/// arms cannot be comparing different geometry. That this arithmetic really is
/// `placed`'s is pinned in `squallar-egui`, by
/// `tile_mesh::tests::the_flat_buffers_placed_by_hand_are_what_placed_answers`
/// — this crate must not depend on `walkers`, and the equivalence is that
/// test's to hold rather than this one's to assume.
fn cpu_shape(meshes: &tile_mesh::TileMeshes) -> Vec<egui::Shape> {
    let place = tile_mesh::Placement::of(piece());
    let mut mesh = egui::epaint::Mesh::default();
    for i in 0..meshes.vertex_count() as usize {
        let vertex = meshes.vertex(i).expect("the vertex is in range");
        mesh.vertices.push(egui::epaint::Vertex {
            pos: egui::pos2(
                place.scale * vertex.pos[0] + place.translation[0],
                place.scale * vertex.pos[1] + place.translation[1],
            ),
            uv: egui::epaint::WHITE_UV,
            color: egui::Color32::from_rgba_premultiplied(
                vertex.color.to_ne_bytes()[0],
                vertex.color.to_ne_bytes()[1],
                vertex.color.to_ne_bytes()[2],
                vertex.color.to_ne_bytes()[3],
            ),
        });
    }
    for i in 0..meshes.index_count() as usize {
        mesh.indices
            .push(meshes.index(i).expect("the index is in range"));
    }
    vec![egui::Shape::Mesh(mesh.into())]
}

/// The callback path's shapes: **one** paint callback covering every run of
/// the tile, at the one placement they share, exactly as `paint_vector_tile`
/// emits them when nothing the ground phase draws sits between the runs.
///
/// The batch is what the parity comparison below is taken against, so "the
/// runs of one callback draw the same pixels as the same runs drawn one
/// callback each, and as the CPU path" is settled by the image rather than by
/// an argument about draw order.
fn callback_shapes(
    meshes: &std::sync::Arc<tile_mesh::TileMeshes>,
    pass_nr: u64,
) -> Vec<egui::Shape> {
    let bridge = TileMeshBridge;
    vec![egui::Shape::Callback(egui::epaint::PaintCallback {
        rect: piece(),
        callback: bridge
            .payload(tile_mesh::GroundDraw {
                meshes,
                first_run: 0,
                run_count: meshes.runs().len(),
                place: tile_mesh::Placement::of(piece()),
                pass_nr,
            })
            .expect("the bridge always answers for a span it was given"),
    })]
}

/// One paint callback per run, in the order given.
///
/// The arrangement the batch replaces — and, given a reversed order, the
/// control that shows the byte compare can see a draw-order difference at all.
fn callback_shapes_per_run(
    meshes: &std::sync::Arc<tile_mesh::TileMeshes>,
    order: impl Iterator<Item = usize>,
    pass_nr: u64,
) -> Vec<egui::Shape> {
    let bridge = TileMeshBridge;
    order
        .map(|run| {
            egui::Shape::Callback(egui::epaint::PaintCallback {
                rect: piece(),
                callback: bridge
                    .payload(tile_mesh::GroundDraw {
                        meshes,
                        first_run: run,
                        run_count: 1,
                        place: tile_mesh::Placement::of(piece()),
                        pass_nr,
                    })
                    .expect("the bridge always answers for a run it was given"),
            })
        })
        .collect()
}

/// Four **opaque** overlapping quads, each its own mesh, so the flatten makes
/// four runs and each one hides part of the one before it.
///
/// Opaque and overlapping is the whole design: with translucent quads the
/// blend is very nearly commutative and a reordered draw would produce a
/// picture too close to the right one to separate, which would make the
/// order-sensitivity control below vacuous. These are `a` over `b` with no
/// alpha, so painting them in any other order is a visibly different image.
fn layered_fills() -> Vec<egui::epaint::Mesh> {
    [
        egui::Color32::from_rgb(200, 30, 40),
        egui::Color32::from_rgb(20, 160, 60),
        egui::Color32::from_rgb(40, 60, 220),
        egui::Color32::from_rgb(230, 200, 20),
    ]
    .into_iter()
    .enumerate()
    .map(|(i, colour)| {
        let at = i as f32 * 512.0;
        let mut mesh = egui::epaint::Mesh::default();
        mesh.add_rect_with_uv(
            egui::Rect::from_min_size(egui::pos2(at, at), egui::vec2(2048.0, 2048.0)),
            egui::Rect::from_min_max(egui::epaint::WHITE_UV, egui::epaint::WHITE_UV),
            colour,
        );
        mesh
    })
    .collect()
}

/// **The batch draws its runs in the order the style asked for.**
///
/// `the_callback_path_puts_the_same_bytes_on_screen_as_cpu_placement` settles
/// the shader against the CPU on a tile of **one** run, so it says nothing
/// about a callback that draws several. (It is `#[ignore]`d like everything
/// here; run the file with `cargo test -p squallar-gpu --test tile_mesh_gpu --
/// --ignored`.) This is that case: four opaque overlapping fill runs, drawn
/// three ways, and the three readbacks compared.
///
/// * **one batched callback** — what `paint_vector_tile` emits today;
/// * **one callback per run** — what it emitted before, byte-identical or the
///   batch has changed what covers what;
/// * **one callback per run, reversed** — the interleaved control. It must
///   *differ*, or these quads do not overlap enough for the compare to see an
///   order at all and the two agreements above would prove nothing.
///
/// The CPU arm is the fourth reading and the anchor: `cpu_shape` walks the
/// flat index buffer in order, so it is the order the runs were flattened in
/// by construction rather than by a second statement of it here.
#[test]
#[ignore = "needs a real wgpu adapter"]
fn a_batched_callback_draws_its_runs_in_run_order() {
    let _serialised = gpu_lock();
    let Some((device, queue)) = device() else {
        eprintln!("SKIPPED: no wgpu adapter");
        return;
    };
    let layers = layered_fills();
    let meshes = std::sync::Arc::new(tile_mesh::flatten_meshes(
        layers.iter().enumerate().map(|(i, m)| (i as u32, m)),
    ));
    assert_eq!(
        meshes.runs().len(),
        4,
        "the fixture is four runs, or the batch under test is not a batch"
    );

    let format = wgpu::TextureFormat::Rgba8UnormSrgb;
    let mut renderer = renderer_for(&device, format);
    let cpu = frame(&device, &queue, &mut renderer, format, cpu_shape(&meshes));
    let batched = frame(
        &device,
        &queue,
        &mut renderer,
        format,
        callback_shapes(&meshes, 1),
    );
    let per_run = frame(
        &device,
        &queue,
        &mut renderer,
        format,
        callback_shapes_per_run(&meshes, 0..4, 2),
    );
    let reversed = frame(
        &device,
        &queue,
        &mut renderer,
        format,
        callback_shapes_per_run(&meshes, (0..4).rev(), 3),
    );

    assert!(
        painted(&cpu) > 1000,
        "non-triviality: the control drew {} texels, so a match below would          be a compare of two empty pictures",
        painted(&cpu),
    );
    assert!(
        reversed != cpu,
        "the control is blind: drawing the four runs back to front produced          the same {} painted texels as drawing them front to back, so these          quads do not overlap and the agreements below prove no ordering",
        painted(&cpu),
    );
    assert!(
        batched == cpu,
        "one callback over four runs did not draw what placing the same four          runs on the CPU draws: the batch has changed what covers what"
    );
    assert!(
        per_run == cpu,
        "four callbacks of one run each did not match the CPU path either, so          the disagreement is not the batching"
    );
}

fn renderer_for(device: &wgpu::Device, format: wgpu::TextureFormat) -> egui_wgpu::Renderer {
    let mut renderer = egui_wgpu::Renderer::new(
        device,
        format,
        egui_wgpu::RendererOptions {
            depth_stencil_format: None,
            msaa_samples: 1,
            dithering: EGUI_DITHERING,
            ..Default::default()
        },
    );
    renderer.callback_resources.insert(TileMeshStore::new(
        device,
        AttachmentConfig {
            color_format: format,
            depth_format: None,
            msaa_samples: 1,
        },
        EGUI_DITHERING,
    ));
    renderer
}

/// How many texels are not the transparent clear — the floor under every
/// comparison below. A pair of empty pictures matches perfectly and proves
/// nothing.
fn painted(pixels: &[u8]) -> usize {
    pixels
        .chunks_exact(4)
        .filter(|p| p != &[0, 0, 0, 0])
        .count()
}

/// **The gate.** Same tile, two paths, byte-identical readback — on both
/// gamma conventions, with the two conventions shown to differ from each
/// other so the compare is known to be sensitive to the thing being tested.
#[test]
#[ignore = "needs a real wgpu adapter"]
fn the_callback_path_puts_the_same_bytes_on_screen_as_cpu_placement() {
    let _serialised = gpu_lock();
    let Some((device, queue)) = device() else {
        eprintln!("SKIPPED: no wgpu adapter");
        return;
    };
    let meshes = flat();
    assert_eq!(meshes.runs().len(), 1, "the fixture is one coalesced run");

    let mut readings = Vec::new();
    for format in [
        wgpu::TextureFormat::Rgba8UnormSrgb,
        wgpu::TextureFormat::Rgba8Unorm,
    ] {
        let mut renderer = renderer_for(&device, format);
        let cpu = frame(&device, &queue, &mut renderer, format, cpu_shape(&meshes));
        let gpu = frame(
            &device,
            &queue,
            &mut renderer,
            format,
            callback_shapes(&meshes, 1),
        );

        let drew = painted(&cpu);
        assert!(
            drew > (SIDE * SIDE / 4) as usize,
            "{format:?}: the CPU path painted only {drew} texels, so a match \
             would be two nearly-empty pictures agreeing"
        );
        assert_eq!(
            painted(&gpu),
            drew,
            "{format:?}: the two paths covered different areas"
        );

        let differing = cpu
            .chunks_exact(4)
            .zip(gpu.chunks_exact(4))
            .filter(|(a, b)| a != b)
            .count();
        assert_eq!(
            differing,
            0,
            "{format:?}: {differing} of {} texels differ between CPU \
             placement and the callback path — the shader's gamma, dither, \
             blend or colour unpack does not match egui's",
            SIDE * SIDE
        );
        readings.push(cpu);
    }

    // The interleaved control: the two target formats really do produce
    // different pictures, so the byte compares above were capable of failing
    // on exactly the difference this gate exists to catch.
    assert_ne!(
        readings[0], readings[1],
        "the sRGB and non-sRGB targets read back identically, so this suite \
         cannot see a gamma convention at all and both passes above are vacuous"
    );
}

/// **The same gate for strokes**, and the harder half: a stroke's geometry is
/// not carried across, only the *offset* each vertex takes from its point, and
/// the shader adds it after the placement. The control arm is egui's own
/// tessellator over the placed `Shape::Path`, which is literally the path this
/// replaces.
///
/// Two texel budgets rather than one byte compare, and the reason is measured
/// in `squallar-egui`: the two sides compute the normal from differences taken
/// in different spaces, so the vertex positions agree to within one ulp of the
/// placed coordinate rather than exactly
/// (`tile_mesh::fixture_tests::the_offsets_reproduce_epaints_own_tessellation`).
/// One ulp is far under the rasteriser's sub-pixel step, so the expectation is
/// still zero differing texels; the budget is there so that if a driver's
/// coverage rounding does land on a boundary, this reddens with a *number*
/// rather than turning into a flake somebody re-runs.
#[test]
#[ignore = "needs a real wgpu adapter"]
fn the_stroke_callback_path_puts_the_same_bytes_on_screen_as_cpu_placement() {
    let _serialised = gpu_lock();
    let Some((device, queue)) = device() else {
        eprintln!("SKIPPED: no wgpu adapter");
        return;
    };
    let paths = strokes();
    let meshes = flat_strokes(&paths);
    assert_eq!(
        meshes.runs().len(),
        1,
        "the fixture's four consecutive paths are one run"
    );
    assert!(
        meshes.stroke_vertex_count() > 0,
        "the fixture flattened to no stroke vertices, so the GPU arm below \
         would draw nothing and match an empty picture"
    );
    // **And no fills at all**, which makes this the stroke-only tile too — a
    // style at one zoom can produce one, and a residency that allocated a
    // zero-length fill buffer for it would be a wgpu validation failure
    // rather than an empty draw.
    assert_eq!(meshes.vertex_count(), 0, "this fixture is strokes only");

    // **Both of epaint's feathered branches are in the picture.** They share a
    // pipeline and a buffer, so a fixture that drifted onto one side of the
    // hairline threshold would still pass every assertion below while leaving
    // the other branch with no rendered comparison anywhere.
    let (thick, hairline): (Vec<_>, Vec<_>) = paths
        .iter()
        .partition(|path| path.stroke.width > 0.9 * FEATHERING);
    assert!(
        !thick.is_empty() && !hairline.is_empty(),
        "the fixture has {} thick and {} hairline strokes at feathering \
         {FEATHERING}; both branches must be drawn here",
        thick.len(),
        hairline.len()
    );

    let mut readings = Vec::new();
    for format in [
        wgpu::TextureFormat::Rgba8UnormSrgb,
        wgpu::TextureFormat::Rgba8Unorm,
    ] {
        let mut renderer = renderer_for(&device, format);
        let cpu = frame(
            &device,
            &queue,
            &mut renderer,
            format,
            cpu_stroke_shapes(&paths),
        );
        let gpu = frame(
            &device,
            &queue,
            &mut renderer,
            format,
            callback_shapes(&meshes, 1),
        );

        let drew = painted(&cpu);
        assert!(
            drew > (SIDE * SIDE / 64) as usize,
            "{format:?}: the CPU path painted only {drew} texels, so a match \
             would be two nearly-empty pictures agreeing"
        );

        let differing = cpu
            .chunks_exact(4)
            .zip(gpu.chunks_exact(4))
            .filter(|(a, b)| a != b)
            .count();
        let worst = cpu
            .iter()
            .zip(gpu.iter())
            .map(|(a, b)| a.abs_diff(*b))
            .max()
            .unwrap_or(0);
        println!(
            "{format:?}: {differing} of {} texels differ, worst channel \
             delta {worst}, over {drew} painted",
            SIDE * SIDE
        );
        assert_eq!(
            differing,
            0,
            "{format:?}: {differing} of {} texels differ between egui's own \
             tessellation of the placed path and the pre-computed offsets \
             (worst channel delta {worst})",
            SIDE * SIDE
        );
        readings.push(cpu);
    }

    assert_ne!(
        readings[0], readings[1],
        "the sRGB and non-sRGB targets read back identically, so this suite \
         cannot see a gamma convention at all and both passes above are vacuous"
    );
}

/// **One buffer write per tile lifetime, not one per frame.**
///
/// Baseline behaviour is the thing this replaces: the CPU path re-places,
/// re-tessellates and re-stages every vertex on every frame, so the honest
/// control here is the draw count — `N` frames really did draw the tile `N`
/// times while the upload happened once.
#[test]
#[ignore = "needs a real wgpu adapter"]
fn a_static_viewport_uploads_each_tile_once_however_many_frames_it_draws() {
    let _serialised = gpu_lock();
    let Some((device, queue)) = device() else {
        eprintln!("SKIPPED: no wgpu adapter");
        return;
    };
    const FRAMES: u64 = 12;
    let format = wgpu::TextureFormat::Rgba8UnormSrgb;
    let mut renderer = renderer_for(&device, format);
    let meshes = flat();

    for pass_nr in 0..FRAMES {
        let _ = frame(
            &device,
            &queue,
            &mut renderer,
            format,
            callback_shapes(&meshes, pass_nr),
        );
    }

    let store = renderer
        .callback_resources
        .get::<TileMeshStore>()
        .expect("the store is installed");
    assert_eq!(
        store.resident_tiles(),
        1,
        "one tile drawn {FRAMES} times is resident more than once"
    );
    assert_eq!(
        store.uploads(),
        (1, meshes.bytes()),
        "one tile drawn {FRAMES} times did not upload exactly once, for \
         exactly its own buffers"
    );
    assert_eq!(
        store.resident_bytes(),
        meshes.bytes(),
        "the store's byte account does not equal what it is holding"
    );
}

/// **A frame of many ground draws writes the uniform ring once.**
///
/// The per-draw `queue.write_buffer` this replaced was half of
/// `update_buffers` on the scene-D profile (see `PlacementBatch`). Sixty-two
/// callbacks — that scene's per-pass draw count — are placed in one frame; the
/// store must have laid sixty-two placements and made exactly one ring write,
/// and the picture must still be that of the same draws placed on the CPU.
/// The picture is the control for the count: the store is fresh, so a slot
/// the batch failed to write reads as zeros — a scale of zero, a draw
/// collapsed to a point — and the compare reddens.
#[test]
#[ignore = "needs a real wgpu adapter"]
fn a_frame_of_many_ground_draws_writes_the_ring_once() {
    let _serialised = gpu_lock();
    let Some((device, queue)) = device() else {
        eprintln!("SKIPPED: no wgpu adapter");
        return;
    };
    const DRAWS: u64 = 62;
    let format = wgpu::TextureFormat::Rgba8UnormSrgb;
    let mut renderer = renderer_for(&device, format);
    let meshes = flat();
    let shapes: Vec<egui::Shape> = (0..DRAWS)
        .flat_map(|_| callback_shapes(&meshes, 0))
        .collect();
    assert_eq!(
        shapes.len() as u64,
        DRAWS,
        "the fixture is one run per callback"
    );

    let gpu = frame(&device, &queue, &mut renderer, format, shapes);
    let cpu = frame(
        &device,
        &queue,
        &mut renderer,
        format,
        (0..DRAWS).flat_map(|_| cpu_shape(&meshes)).collect(),
    );
    assert!(
        painted(&gpu) > (SIDE * SIDE / 4) as usize,
        "the batched frame painted too little for a match to mean anything"
    );
    assert_eq!(
        gpu, cpu,
        "the batched placements do not draw the picture of the same draws placed          on the CPU"
    );

    let store = renderer
        .callback_resources
        .get::<TileMeshStore>()
        .expect("the store is installed");
    assert_eq!(
        store.placement_writes(),
        (DRAWS, 1),
        "{DRAWS} ground draws in one pass were not one ring write"
    );
}

/// **Residency ends with the tile, and the bytes come back.**
///
/// The tile cache owns the flattened buffers; the store holds a weak handle
/// and nothing else. Dropping the `Arc` is what a tile leaving the LRU (or a
/// restyle replacing it) does, and the next frame's sweep must give the GPU
/// buffers back rather than accumulate them across a zoom sweep.
#[test]
#[ignore = "needs a real wgpu adapter"]
fn a_tile_the_cache_let_go_of_stops_being_resident_and_its_bytes_come_back() {
    let _serialised = gpu_lock();
    let Some((device, queue)) = device() else {
        eprintln!("SKIPPED: no wgpu adapter");
        return;
    };
    let format = wgpu::TextureFormat::Rgba8UnormSrgb;
    let mut renderer = renderer_for(&device, format);

    // A zoom sweep in miniature: twenty tiles, each drawn once and then let
    // go of, one after another. Without the sweep the store would hold all
    // twenty; with it, it holds what the cache still owns.
    const TILES: usize = 20;
    let mut peak_tiles = 0;
    let mut peak_bytes = 0;
    let mut one_tile_bytes = 0;
    for pass_nr in 0..TILES {
        let meshes = flat();
        one_tile_bytes = meshes.bytes();
        let _ = frame(
            &device,
            &queue,
            &mut renderer,
            format,
            // A frame of its own, which is what makes the store sweep: the
            // sweep is once per egui pass, not once per callback.
            callback_shapes(&meshes, pass_nr as u64),
        );
        let store = renderer
            .callback_resources
            .get::<TileMeshStore>()
            .expect("the store is installed");
        peak_tiles = peak_tiles.max(store.resident_tiles());
        peak_bytes = peak_bytes.max(store.resident_bytes());
        // The tile cache lets go. The previous frame's callback still holds a
        // clone until its `tris` are dropped, which is why the sweep is a
        // frame behind and the peak below is two rather than one.
        drop(meshes);
    }

    assert!(
        peak_tiles <= 2,
        "{TILES} tiles drawn one at a time left {peak_tiles} resident: the \
         store is accumulating instead of sweeping"
    );
    assert!(
        peak_bytes <= 2 * one_tile_bytes,
        "the byte account peaked at {peak_bytes} for a working set of one \
         tile ({one_tile_bytes} B)"
    );

    // Non-triviality: the store really was holding something, so the bound
    // above is not "nothing was ever uploaded".
    assert!(
        peak_tiles >= 1 && peak_bytes >= one_tile_bytes,
        "nothing was ever resident, so the eviction bound is vacuous"
    );
}
