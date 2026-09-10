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
    fills_of([
        egui::Color32::from_rgba_premultiplied(200, 30, 40, 255),
        egui::Color32::from_rgba_premultiplied(20, 120, 60, 160),
        egui::Color32::from_rgba_premultiplied(70, 70, 200, 90),
        egui::Color32::from_rgba_premultiplied(11, 13, 17, 200),
    ])
}

/// [`fills`] with **every channel even**, for the opacity gate: at one half,
/// an even byte halves exactly on both arms, where an odd one lands on .5 and
/// the CPU's `(c * f + 0.5) as u8` rounds it while the GPU carries the float
/// to the framebuffer. The same quads at the same places; only the bytes are
/// nudged, so what is compared is the arithmetic and not a rounding rule.
fn even_fills() -> egui::epaint::Mesh {
    fills_of([
        egui::Color32::from_rgba_premultiplied(200, 30, 40, 254),
        egui::Color32::from_rgba_premultiplied(20, 120, 60, 160),
        egui::Color32::from_rgba_premultiplied(70, 70, 200, 90),
        egui::Color32::from_rgba_premultiplied(10, 12, 16, 200),
    ])
}

/// Four overlapping quads in the given colours, one fixture for both of the
/// above.
fn fills_of(colours: [egui::Color32; 4]) -> egui::epaint::Mesh {
    let mut mesh = egui::epaint::Mesh::default();
    for (i, colour) in colours.into_iter().enumerate() {
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
    flat_of(&fills())
}

/// One mesh through the map's own flattener.
fn flat_of(mesh: &egui::epaint::Mesh) -> std::sync::Arc<tile_mesh::TileMeshes> {
    std::sync::Arc::new(tile_mesh::flatten_meshes(std::iter::once((0, mesh))))
}

/// The feathering the stroke fixture is flattened and drawn at.
///
/// egui's default `feathering_size_in_pixels` over the `pixels_per_point` the
/// frames below use, which is 1: the canvas is [`SIDE`] points and [`SIDE`]
/// texels.
const FEATHERING: f32 = 1.0;

/// **The most a texel may differ between two arms that agree geometrically.**
///
/// Zero texels is what the two comparisons in this file expect and what they
/// got for months. It is not, however, what either of them can *guarantee*,
/// and the difference cost `main` five red runs.
///
/// Both comparisons put two differently-built pictures side by side. The
/// stroke pair agrees to one ulp of the placed coordinate rather than exactly
/// — measured, not argued, by
/// `tile_mesh::fixture_tests::the_offsets_reproduce_epaints_own_tessellation`
/// over 400+ paths on both of epaint's branches. The background pair agrees to
/// a pixel rounding: a feathered `Shape::Rect` under a scissor on one side, a
/// hard `add_colored_rect` mesh on the other. Neither difference is visible at
/// the rasteriser's sub-pixel step — until a texel's coverage lands *exactly*
/// on a rounding boundary, where the two sides are free to round opposite ways
/// and the readback comes back one least-significant bit apart.
///
/// That is not a hypothetical either. Between 2026-09-07 and 2026-09-08 both
/// comparisons reddened `main` on five commits whose `squallar-gpu` tree,
/// `Cargo.lock` and workflow were byte-identical to the greens on either side
/// of them, always with the same reading: `1 of 65536 texels differ, worst
/// channel delta 1` on `Rgba8UnormSrgb`, at `(128, 222)` — a texel on the
/// grid's vertical seam, which [`GRID_ORIGIN`] deliberately places on an exact
/// half-pixel. Bit-identical every time, so not a race; over an unchanged tree,
/// so not a regression. A property of the host the software rasteriser was
/// JIT-compiled for.
///
/// So the bound is one bit, and one bit is the whole of the slack. **A
/// misplacement does not move a colour by one.** It hands a *whole edge* to
/// the wrong side of a pixel: [`GRID_ORIGIN`]'s own doc records that dropping
/// the pixel rounding `background_within` performs reads 600 texels off, three
/// whole columns, in the saturated per-tile colours this fixture is painted in.
/// The delta conjunct below is what separates a rounding boundary from a
/// geometry error, and it is tight: it admits the last bit and nothing else.
const LAST_BIT: u8 = 1;

/// **How many texels of the stroke picture may sit on a coverage tie.**
///
/// The observed figure on CI is one, of 65,536, over ~11,000 painted. This is
/// sixteen: enough headroom that a second tie on another host is not a red,
/// and far too little to hide a moved edge. The fixture's shortest stroke
/// spans most of the canvas, so a stroke placed one pixel out redisplays its
/// whole outline — hundreds of texels, and in saturated colours, so the
/// [`LAST_BIT`] conjunct above catches it before this one is consulted.
const STROKE_TIE_BUDGET: usize = SIDE as usize / 16;

/// **How many texels of the grid picture may sit on a coverage tie.**
///
/// One eighth of a tile's edge. The defect this comparison exists to catch is
/// recorded in [`GRID_ORIGIN`]'s own doc: an edge handed to the wrong side of
/// a half-pixel reads *three whole columns*, 600 texels, off. One column is
/// [`GRID_SIDE`] = 100. Twelve is an order of magnitude under a single
/// column, so no edge can move without this reddening, and an order of
/// magnitude over the one tie CI actually shows.
const SEAM_TIE_BUDGET: usize = GRID_SIDE as usize / 8;

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

/// The whole canvas, which is also the clip every root painter carries.
fn canvas() -> egui::Rect {
    egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(SIDE as f32, SIDE as f32))
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
    frame_clipped(device, queue, renderer, format, vec![(piece(), shapes)]).0
}

/// [`frame`] with the painter's opacity set before the shapes are added --
/// what the layer walk does around a layer's arm. egui tints every shape it
/// can on the way in; a callback goes through untouched and carries its own
/// factor in the uniform.
fn frame_at(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    renderer: &mut egui_wgpu::Renderer,
    format: wgpu::TextureFormat,
    opacity: f32,
    shapes: Vec<egui::Shape>,
) -> Vec<u8> {
    frame_groups(
        device,
        queue,
        renderer,
        format,
        opacity,
        vec![(piece(), shapes)],
    )
    .0
}

/// [`frame`] over several groups of shapes, each painted under its own clip
/// rect in the order given -- the shape of a ground walk over more than one
/// tile -- returning the readback and what egui's tessellator made of the
/// shapes, so a case can count primitives beside comparing pixels.
fn frame_clipped(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    renderer: &mut egui_wgpu::Renderer,
    format: wgpu::TextureFormat,
    groups: Vec<(egui::Rect, Vec<egui::Shape>)>,
) -> (Vec<u8>, Vec<egui::ClippedPrimitive>) {
    frame_groups(device, queue, renderer, format, 1.0, groups)
}

/// [`frame_clipped`] at a painter opacity. Every group's painter is set to
/// `opacity` before its shapes are added, which at 1.0 is what a fresh
/// painter already carries.
fn frame_groups(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    renderer: &mut egui_wgpu::Renderer,
    format: wgpu::TextureFormat,
    opacity: f32,
    groups: Vec<(egui::Rect, Vec<egui::Shape>)>,
) -> (Vec<u8>, Vec<egui::ClippedPrimitive>) {
    let ctx = egui::Context::default();
    let canvas = canvas();
    ctx.begin_pass(egui::RawInput {
        screen_rect: Some(canvas),
        ..Default::default()
    });
    for (clip, shapes) in groups {
        let mut painter = ctx
            .layer_painter(egui::LayerId::background())
            .with_clip_rect(clip);
        painter.set_opacity(opacity);
        painter.extend(shapes);
    }
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

    (read_back(device, queue, &texture), tris)
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

/// [`cpu_shape`] with every vertex colour put through
/// `Color32::gamma_multiply(opacity)` by hand -- the operation
/// `Painter::add` applies to a mesh under `set_opacity`, spelled out so the
/// opacity gate can show that the painter's tint *is* that and nothing more.
fn cpu_shape_tinted(meshes: &tile_mesh::TileMeshes, opacity: f32) -> Vec<egui::Shape> {
    cpu_shape(meshes)
        .into_iter()
        .map(|shape| match shape {
            egui::Shape::Mesh(mesh) => {
                let mut mesh = std::sync::Arc::unwrap_or_clone(mesh);
                for vertex in &mut mesh.vertices {
                    vertex.color = vertex.color.gamma_multiply(opacity);
                }
                egui::Shape::Mesh(mesh.into())
            }
            other => other,
        })
        .collect()
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
    callback_shapes_at(meshes, pass_nr, 1.0)
}

/// [`callback_shapes`] carrying a layer opacity in its uniform -- what
/// `draw_tile_layer` hands across from `ui.painter().opacity()`.
fn callback_shapes_at(
    meshes: &std::sync::Arc<tile_mesh::TileMeshes>,
    pass_nr: u64,
    opacity: f32,
) -> Vec<egui::Shape> {
    let bridge = TileMeshBridge;
    vec![egui::Shape::Callback(egui::epaint::PaintCallback {
        rect: piece(),
        callback: bridge
            .payload(tile_mesh::GroundBatch {
                draws: &[tile_mesh::GroundDraw {
                    meshes,
                    first_run: 0,
                    run_count: meshes.runs().len(),
                    place: tile_mesh::Placement::of(piece()),
                    opacity,
                    clip: piece(),
                }],
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
                    .payload(tile_mesh::GroundBatch {
                        draws: &[tile_mesh::GroundDraw {
                            meshes,
                            first_run: run,
                            run_count: 1,
                            place: tile_mesh::Placement::of(piece()),
                            opacity: 1.0,
                            clip: piece(),
                        }],
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
        "non-triviality: the control drew {} texels, so a match below would \
         be a compare of two empty pictures",
        painted(&cpu),
    );
    assert!(
        reversed != cpu,
        "the control is blind: drawing the four runs back to front produced \
         the same {} painted texels as drawing them front to back, so these \
         quads do not overlap and the agreements below prove no ordering",
        painted(&cpu),
    );
    assert!(
        batched == cpu,
        "one callback over four runs did not draw what placing the same four \
         runs on the CPU draws: the batch has changed what covers what"
    );
    assert!(
        per_run == cpu,
        "four callbacks of one run each did not match the CPU path either, so \
         the disagreement is not the batching"
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

/// A tile's host fill bytes are drawn **once, by one store**, and a case that
/// draws it twice needs two tiles.
///
/// `TileMeshStore::ensure` takes those bytes one way, on the first `prepare`
/// that sees the tile: `cpu_shape` then finds nothing to read, and a *second*
/// store finds nothing to upload and draws an empty picture. Both are the
/// shipped contract (`TileMeshes::fills_epoch`, and the frame-side decline in
/// `ui_map_overlays::run_is_drawable` that goes with it) rather than anything
/// this file can arrange around — so a case that sweeps two target formats
/// builds a **fresh tile per format**, and takes its CPU arm off that tile
/// before the format's first frame.
///
/// Three of the cases here did neither and sat red behind `#[ignore]`.
///
/// `flat` and `flat_of` mint a new `TileMeshes` on every call, so this is a
/// note about *where they are called from* and not a new mechanism.
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
    let mut readings = Vec::new();
    for format in [
        wgpu::TextureFormat::Rgba8UnormSrgb,
        wgpu::TextureFormat::Rgba8Unorm,
    ] {
        // A fresh tile, and its CPU arm read off it before the first frame of
        // this format draws anything. See the note above `cpu_shape`.
        let meshes = flat();
        assert_eq!(meshes.runs().len(), 1, "the fixture is one coalesced run");
        let cpu_arm = cpu_shape(&meshes);

        let mut renderer = renderer_for(&device, format);
        let cpu = frame(&device, &queue, &mut renderer, format, cpu_arm.clone());
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

/// **A callback at half opacity puts the same bytes on screen as the painter's
/// own tint of the mesh it replaces.**
///
/// A layer's opacity is a painter tint: `Painter::add` puts every colour of
/// every shape it can reach through `Color32::gamma_multiply`, and a
/// `Shape::Callback` is the one shape it cannot reach, so the tile-mesh path
/// carries the factor in its uniform instead. This is the gate that the two
/// spellings are one operation. Three readbacks per gamma convention:
///
/// * **the reference** -- the flattened fills as a CPU mesh, through a painter
///   at 0.5, which is egui tinting it;
/// * **the hand-tinted control** -- the same mesh with every vertex put
///   through `gamma_multiply(0.5)` by this file, through a painter at 1.0;
///   equal to the reference, which pins what the painter's tint *is*;
/// * **the case** -- the callback through the same 0.5 painter, carrying 0.5
///   in its uniform.
///
/// The untinted picture is read too and shown to differ from the reference,
/// so the compares are known to be able to see a tint; and the tinted
/// picture covers the same texels, so a dim is not a clip. The fixture is
/// [`even_fills`], which is why the compare is a byte compare and not a
/// budget: see its doc.
#[test]
#[ignore = "needs a real wgpu adapter"]
fn a_callback_at_half_opacity_puts_the_same_bytes_on_screen_as_the_painters_tint() {
    const HALF: f32 = 0.5;
    let _serialised = gpu_lock();
    let Some((device, queue)) = device() else {
        eprintln!("SKIPPED: no wgpu adapter");
        return;
    };
    for format in [
        wgpu::TextureFormat::Rgba8UnormSrgb,
        wgpu::TextureFormat::Rgba8Unorm,
    ] {
        // A fresh tile per format, arms read off it first. See the note above
        // `cpu_shape`.
        let meshes = flat_of(&even_fills());
        assert_eq!(meshes.runs().len(), 1, "the fixture is one coalesced run");
        let cpu_arm = cpu_shape(&meshes);
        let tinted_arm = cpu_shape_tinted(&meshes, HALF);

        let mut renderer = renderer_for(&device, format);
        let full = frame(&device, &queue, &mut renderer, format, cpu_arm.clone());
        let reference = frame_at(
            &device,
            &queue,
            &mut renderer,
            format,
            HALF,
            cpu_arm.clone(),
        );
        let by_hand = frame(&device, &queue, &mut renderer, format, tinted_arm.clone());
        let gpu = frame_at(
            &device,
            &queue,
            &mut renderer,
            format,
            HALF,
            callback_shapes_at(&meshes, 1, HALF),
        );

        let drew = painted(&full);
        assert!(
            drew > (SIDE * SIDE / 4) as usize,
            "{format:?}: the CPU path painted only {drew} texels, so a match \
             would be two nearly-empty pictures agreeing"
        );
        assert_ne!(
            full, reference,
            "{format:?}: a painter at 0.5 drew the same bytes as one at 1.0, \
             so this suite cannot see a tint and every compare below is vacuous"
        );
        assert_eq!(
            painted(&reference),
            drew,
            "{format:?}: the tint changed which texels were covered"
        );
        assert_eq!(
            painted(&gpu),
            drew,
            "{format:?}: the callback at 0.5 covered a different area"
        );

        let differing = |a: &[u8], b: &[u8]| {
            a.chunks_exact(4)
                .zip(b.chunks_exact(4))
                .filter(|(a, b)| a != b)
                .count()
        };
        assert_eq!(
            differing(&reference, &by_hand),
            0,
            "{format:?}: the painter's tint of a mesh is not `gamma_multiply` \
             on every vertex, so the shader is mirroring the wrong operation"
        );
        let off = differing(&reference, &gpu);
        assert_eq!(
            off,
            0,
            "{format:?}: {off} of {} texels differ between the callback at \
             opacity 0.5 and the painter's own tint of the same mesh -- the \
             uniform is not reaching both vertex stages, or not every channel",
            SIDE * SIDE
        );
    }
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
        // **The two budgets this test's own doc comment promises.** They were
        // prose over an `assert_eq!(differing, 0)` until 2026-09-08; see
        // [`LAST_BIT`] for what that cost and why zero was never the thing
        // this pair can guarantee.
        assert!(
            worst <= LAST_BIT,
            "{format:?}: a channel differs by {worst} between egui's own \
             tessellation of the placed path and the pre-computed offsets, \
             over {differing} of {} texels. One bit is a coverage tie; \
             {worst} is geometry. The offsets are supposed to reproduce \
             epaint's tessellation to one ulp of the placed coordinate — if \
             `tile_mesh::fixture_tests::the_offsets_reproduce_epaints_own_\
             tessellation` is still green then the vertices agree and the \
             difference entered after them, in the shader, the blend or the \
             gamma",
            SIDE * SIDE
        );
        assert!(
            differing <= STROKE_TIE_BUDGET,
            "{format:?}: {differing} of {} texels differ (worst channel delta \
             {worst}) between egui's own tessellation of the placed path and \
             the pre-computed offsets, over {drew} painted. Every one of them \
             is within a bit, so this is not a misplacement — but {} texels \
             is past the {STROKE_TIE_BUDGET} a coverage tie can reach on this \
             fixture, and a whole edge's worth of last-bit disagreement is a \
             rounding rule that has changed, not a tie",
            SIDE * SIDE,
            differing
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

    // The CPU arm first: the frame below takes the tile's host bytes. See the
    // note above `cpu_shape`.
    let cpu_arm: Vec<egui::Shape> = (0..DRAWS).flat_map(|_| cpu_shape(&meshes)).collect();

    let gpu = frame(&device, &queue, &mut renderer, format, shapes);
    let cpu = frame(&device, &queue, &mut renderer, format, cpu_arm);
    assert!(
        painted(&gpu) > (SIDE * SIDE / 4) as usize,
        "the batched frame painted too little for a match to mean anything"
    );
    assert_eq!(
        gpu, cpu,
        "the batched placements do not draw the picture of the same draws \
         placed on the CPU"
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

// ---------------------------------------------------------------------------
// The hoisted background rectangles.
//
// `draw_tile_layer` draws every vector tile's background rectangle ahead of
// every tile's geometry, under the pane's clip, cut to its piece by
// `tile_mesh::background_within` instead of clipped to it by the tile's own
// painter -- so epaint tessellates the lot into one primitive where the clip
// opened one per tile. Whether the cut is the clip, pixel for pixel, is the
// question below; `background_within`'s docs carry the argument and this is
// the picture that holds it.
// ---------------------------------------------------------------------------

/// The grid's north-west corner, **off the pixel grid on purpose, and
/// differently on the two axes.** Every x edge lands on an exact half-pixel,
/// where a hard edge and `round()` disagree: the rasterizer's top-left rule
/// covers a pixel centre sitting on a left edge and not one on a right edge,
/// while `round()` pushes both edges up. So the pixel-rounding
/// `background_within` spells out is load-bearing on that axis, and gated
/// here -- without it the case below reads three whole columns off, 600
/// texels, one column per half-pixel x edge handed to the wrong side. Every y
/// edge lands elsewhere between pixels, where a hard edge rounds itself to
/// the nearest pixel centre and the two roundings have merely to agree.
const GRID_ORIGIN: egui::Pos2 = egui::pos2(28.5, 27.6);

/// A tile's side, in points and texels. Small enough that the 2x2 grid plus
/// the stretched ancestor's overreach fit the canvas with margin.
const GRID_SIDE: f32 = 100.0;

/// One tile of the fixture grid.
struct GridTile {
    /// The rect the tile occupies on screen.
    piece: egui::Rect,
    /// The rect the whole tile is placed against: the piece, or twice it for
    /// the stretched ancestor.
    full: egui::Rect,
    /// Its background colour. Opaque and distinct per tile, so a background
    /// reaching over a neighbour is a visible change.
    background: egui::Color32,
    /// Its one fill quad's colour. Opaque, so a rectangle drawn over it
    /// instead of under it is a visible change.
    quad: egui::Color32,
}

/// A 2x2 grid: three tiles answered by themselves and, south-east, one
/// answered by a **stretched ancestor** whose south-east quarter is the piece
/// -- the whole tile placed against that window is 200 points across and
/// covers all four pieces. That tile is the case the per-tile clip existed
/// for, and the case the cut has to reproduce.
fn grid_tiles() -> Vec<GridTile> {
    let colours = [
        (
            egui::Color32::from_rgb(0x10, 0x20, 0x30),
            egui::Color32::from_rgb(200, 30, 40),
        ),
        (
            egui::Color32::from_rgb(0x30, 0x20, 0x10),
            egui::Color32::from_rgb(20, 160, 60),
        ),
        (
            egui::Color32::from_rgb(0x20, 0x30, 0x10),
            egui::Color32::from_rgb(40, 60, 220),
        ),
        (
            egui::Color32::from_rgb(0x70, 0x10, 0x10),
            egui::Color32::from_rgb(230, 200, 20),
        ),
    ];
    colours
        .into_iter()
        .enumerate()
        .map(|(i, (background, quad))| {
            let column = (i % 2) as f32;
            let row = (i / 2) as f32;
            let piece = egui::Rect::from_min_size(
                GRID_ORIGIN + egui::vec2(column * GRID_SIDE, row * GRID_SIDE),
                egui::vec2(GRID_SIDE, GRID_SIDE),
            );
            let full = if i == 3 {
                egui::Rect::from_min_max(
                    piece.max - egui::vec2(2.0 * GRID_SIDE, 2.0 * GRID_SIDE),
                    piece.max,
                )
            } else {
                piece
            };
            GridTile {
                piece,
                full,
                background,
                quad,
            }
        })
        .collect()
}

/// Each tile's quad, flattened through the map's own flattener: the middle
/// half of the extent, which on the stretched ancestor places partly outside
/// its piece and so exercises the scissor on geometry in every arm.
fn grid_meshes(tiles: &[GridTile]) -> Vec<std::sync::Arc<tile_mesh::TileMeshes>> {
    tiles
        .iter()
        .map(|tile| {
            let mut mesh = egui::epaint::Mesh::default();
            mesh.add_rect_with_uv(
                egui::Rect::from_min_max(
                    egui::pos2(EXTENT * 0.25, EXTENT * 0.25),
                    egui::pos2(EXTENT * 0.75, EXTENT * 0.75),
                ),
                egui::Rect::from_min_max(egui::epaint::WHITE_UV, egui::epaint::WHITE_UV),
                tile.quad,
            );
            std::sync::Arc::new(tile_mesh::flatten_meshes(std::iter::once((0, &mesh))))
        })
        .collect()
}

/// The tile's background as the tile's own walk places it: the whole extent
/// onto the whole tile.
fn placed_background(tile: &GridTile) -> egui::epaint::RectShape {
    egui::epaint::RectShape::filled(tile.full, 0.0, tile.background)
}

/// One callback drawing the tile's quad at the whole tile's placement, under
/// the piece -- what `paint_vector_tile` emits for the tile's one run.
fn grid_callback(
    tile: &GridTile,
    meshes: &std::sync::Arc<tile_mesh::TileMeshes>,
    pass_nr: u64,
) -> egui::Shape {
    egui::Shape::Callback(egui::epaint::PaintCallback {
        rect: tile.piece,
        callback: TileMeshBridge
            .payload(tile_mesh::GroundBatch {
                draws: &[tile_mesh::GroundDraw {
                    meshes,
                    first_run: 0,
                    run_count: 1,
                    place: tile_mesh::Placement::of(tile.full),
                    opacity: 1.0,
                    clip: tile.piece,
                }],
                pass_nr,
            })
            .expect("the bridge always answers for a run it was given"),
    })
}

/// **Every tile's runs in one batched callback**, each span carrying the piece
/// its own callback was clipped to.
///
/// `clip_of` says what clip each span gets, so a case can hand the batch the
/// pieces (what the walk does) or the whole canvas (the control that shows the
/// per-span scissor is load-bearing).
fn grid_batch(
    tiles: &[&GridTile],
    meshes: &[&std::sync::Arc<tile_mesh::TileMeshes>],
    pass_nr: u64,
    clip_of: impl Fn(&GridTile) -> egui::Rect,
) -> egui::Shape {
    let draws: Vec<tile_mesh::GroundDraw<'_>> = tiles
        .iter()
        .zip(meshes)
        .map(|(tile, meshes)| tile_mesh::GroundDraw {
            meshes,
            first_run: 0,
            run_count: 1,
            place: tile_mesh::Placement::of(tile.full),
            opacity: 1.0,
            // Intersected with the canvas, because that is what
            // `Painter::with_clip_rect` does to a tile's piece and so what
            // egui put on the per-tile primitive.
            clip: clip_of(tile).intersect(canvas()),
        })
        .collect();
    let rect = draws
        .iter()
        .map(|draw| draw.clip)
        .filter(|clip| clip.is_positive())
        .reduce(|a, b| a.union(b))
        .unwrap_or_else(canvas);
    egui::Shape::Callback(egui::epaint::PaintCallback {
        rect,
        callback: TileMeshBridge
            .payload(tile_mesh::GroundBatch {
                draws: &draws,
                pass_nr,
            })
            .expect("the bridge always answers for spans it was given"),
    })
}

/// The tiles' geometry, one clipped group per tile, in walk order.
fn grid_geometry(
    tiles: &[GridTile],
    meshes: &[std::sync::Arc<tile_mesh::TileMeshes>],
    pass_nr: u64,
) -> Vec<(egui::Rect, Vec<egui::Shape>)> {
    tiles
        .iter()
        .zip(meshes)
        .map(|(tile, meshes)| (tile.piece, vec![grid_callback(tile, meshes, pass_nr)]))
        .collect()
}

/// **The gate for the batch.** Every tile's runs in one callback put the same
/// bytes on the screen as one callback per tile, and the two things that could
/// make them differ are shown to be visible to the compare.
///
/// The arrangement the batch replaces is one `Shape::Callback` per tile, each
/// added under its own piece's clip, which is what egui turned into the
/// scissor. The batch is one callback under the whole canvas, and the scissor
/// each span used to get from egui it now sets itself — so what is asserted
/// here is that those are the same rectangle, applied the same way, in the
/// same order.
///
/// Four readings:
///
/// * **per tile** — the arrangement being replaced: four tiles, four clipped
///   groups, four callbacks, four primitives;
/// * **batched** — one group, one callback, **one** primitive, every span
///   carrying its own piece. Must match the first byte for byte;
/// * **batched, unclipped** — the same batch with every span clipped to the
///   canvas instead of its piece. Must *differ*: the fixture's fourth tile is
///   a stretched ancestor whose quad reaches over all four pieces, so without
///   the per-span scissor it paints its neighbours. This is what shows the
///   scissor is load-bearing and that the compare can see one missing.
/// * **batched, unclipped, reversed** — the unclipped spans in the opposite
///   order. Must differ from the unclipped reading: with the scissors off the
///   quads overlap, so this is what shows the compare can see draw order at
///   all, and therefore that the agreement above is a real agreement about
///   order rather than a compare that could not tell.
///
/// A **fifth tile, wholly off the canvas, sits in the MIDDLE of the batch** —
/// not at its end, because a span that drew where it should not has to have
/// spans after it for the compare to see it corrupt something. Its presence
/// must change no pixel and no primitive.
///
/// **What this does not gate**, said plainly: the order the spans draw in.
/// With every span carrying its own scissor these four tiles are disjoint, so
/// reversing them changes nothing and this case would not notice — which is a
/// true property of a tile grid, not a hole to paper over. Order is gated by
/// `spans_of_one_batch_draw_in_the_order_they_were_held`, on spans that share
/// a clip and overlap — `#[ignore]`d like this one, and run with
/// `cargo test -p squallar-gpu --test tile_mesh_gpu -- --ignored`.
#[test]
#[ignore = "needs a real wgpu adapter"]
fn one_batched_callback_puts_the_same_bytes_on_screen_as_one_callback_per_tile() {
    let _serialised = gpu_lock();
    let Some((device, queue)) = device() else {
        eprintln!("SKIPPED: no wgpu adapter");
        return;
    };
    let tiles = grid_tiles();
    let meshes = grid_meshes(&tiles);

    // The off-canvas tile: a whole grid cell placed a canvas-width east, so
    // every texel of it is outside the target and its scissor rounds to zero
    // width. Its own colours are distinct from every other tile's, so if it
    // drew anywhere at all the compare would see it.
    let away = GridTile {
        piece: egui::Rect::from_min_size(
            egui::pos2(SIDE as f32 + 40.0, GRID_ORIGIN.y),
            egui::vec2(GRID_SIDE, GRID_SIDE),
        ),
        full: egui::Rect::from_min_size(
            egui::pos2(SIDE as f32 + 40.0, GRID_ORIGIN.y),
            egui::vec2(GRID_SIDE, GRID_SIDE),
        ),
        background: egui::Color32::from_rgb(0x01, 0x7f, 0x7f),
        quad: egui::Color32::from_rgb(255, 0, 255),
    };
    let away_meshes = grid_meshes(std::slice::from_ref(&away)).remove(0);

    // Non-triviality of the fixture, asserted rather than assumed:
    let ancestor = &tiles[3];
    for other in &tiles[..3] {
        assert!(
            ancestor.full.expand(0.01).contains_rect(other.piece),
            "fixture: the ancestor's whole tile {:?} does not cover {:?}, so \
             an absent per-span scissor would paint nothing extra and the \
             control below would be vacuous",
            ancestor.full,
            other.piece
        );
    }
    assert_eq!(
        egui_wgpu::scissor_rect_in_pixels(&away.piece, 1.0, [SIDE, SIDE])[2],
        0,
        "fixture: the off-canvas tile's scissor is not zero-width, so the \
         per-span skip it exists to exercise is not exercised"
    );

    let format = wgpu::TextureFormat::Rgba8UnormSrgb;
    let mut renderer = renderer_for(&device, format);

    // The order every arm draws in: the grid's four tiles with the off-canvas
    // one third, so a span that failed to skip has two spans after it to
    // corrupt.
    let order: Vec<&GridTile> = vec![&tiles[0], &tiles[1], &away, &tiles[2], &tiles[3]];
    let order_meshes: Vec<&std::sync::Arc<tile_mesh::TileMeshes>> =
        vec![&meshes[0], &meshes[1], &away_meshes, &meshes[2], &meshes[3]];

    // Arm one: one callback per tile, each under its own piece.
    let per_tile: Vec<(egui::Rect, Vec<egui::Shape>)> = order
        .iter()
        .zip(&order_meshes)
        .map(|(tile, meshes)| (tile.piece, vec![grid_callback(tile, meshes, 1)]))
        .collect();
    let (per_tile_pixels, per_tile_prims) =
        frame_clipped(&device, &queue, &mut renderer, format, per_tile);

    // Arm two: one callback for the lot, under the canvas.
    let (batched_pixels, batched_prims) = frame_clipped(
        &device,
        &queue,
        &mut renderer,
        format,
        vec![(
            canvas(),
            vec![grid_batch(&order, &order_meshes, 2, |tile| tile.piece)],
        )],
    );

    // Each drawn quad is the middle half of its tile, so four of them are
    // about `4 * (GRID_SIDE / 2)^2` texels; three of them is the floor a
    // meaningful picture has to clear.
    let floor = 3 * (GRID_SIDE as usize / 2).pow(2);
    assert!(
        painted(&per_tile_pixels) > floor,
        "the per-tile arm painted only {} texels against a floor of {floor}, \
         so a match would be two nearly-empty pictures agreeing",
        painted(&per_tile_pixels)
    );
    assert_eq!(
        per_tile_pixels, batched_pixels,
        "one batched callback does not draw the picture of one callback per \
         tile: the per-span scissor, the placement or the order is not what \
         egui gave each primitive"
    );
    // **Four, not five.** The off-canvas tile's piece intersects the canvas in
    // nothing, so epaint drops its `ClippedShape` before the primitive list —
    // which is exactly the "draws nothing" the batch has to reproduce, and it
    // reproduces it one layer lower, in `paint`'s per-span skip. The batch is
    // handed that span on purpose here; the shipped walk drops it at the same
    // place epaint does (`ui_map_overlays::hold_run_batch`), so the renderer's
    // skip is defence in depth and this is where it is exercised.
    assert_eq!(
        per_tile_prims.len(),
        4,
        "the per-tile arm is not one primitive per on-canvas tile, so the \
         count below is not measuring the cut"
    );
    assert_eq!(
        batched_prims.len(),
        1,
        "the batch is not one primitive, which is the whole reason it exists"
    );

    // Control one: the same batch with no per-span scissor. The ancestor
    // paints its neighbours, so this must differ.
    let (unclipped, _) = frame_clipped(
        &device,
        &queue,
        &mut renderer,
        format,
        vec![(
            canvas(),
            vec![grid_batch(&order, &order_meshes, 3, |_| canvas())],
        )],
    );
    assert_ne!(
        unclipped, batched_pixels,
        "dropping every span's scissor changed no pixel, so this suite cannot \
         see the clip at all and the agreement above is vacuous"
    );

    // Control two: the unclipped spans in the other order. With the scissors
    // off the quads overlap, so this must differ from control one.
    let mut reversed: Vec<&GridTile> = order.clone();
    reversed.reverse();
    let mut reversed_meshes: Vec<&std::sync::Arc<tile_mesh::TileMeshes>> = order_meshes.clone();
    reversed_meshes.reverse();
    let (unclipped_reversed, _) = frame_clipped(
        &device,
        &queue,
        &mut renderer,
        format,
        vec![(
            canvas(),
            vec![grid_batch(&reversed, &reversed_meshes, 4, |_| canvas())],
        )],
    );
    assert_ne!(
        unclipped_reversed, unclipped,
        "reversing the spans changed no pixel, so this suite cannot see draw \
         order and the agreement above says nothing about it"
    );

    // And the off-canvas span really is inert: the same batch without it.
    let kept: Vec<&GridTile> = vec![&tiles[0], &tiles[1], &tiles[2], &tiles[3]];
    let kept_meshes: Vec<&std::sync::Arc<tile_mesh::TileMeshes>> =
        vec![&meshes[0], &meshes[1], &meshes[2], &meshes[3]];
    let (without, without_prims) = frame_clipped(
        &device,
        &queue,
        &mut renderer,
        format,
        vec![(
            canvas(),
            vec![grid_batch(&kept, &kept_meshes, 5, |tile| tile.piece)],
        )],
    );
    assert_eq!(
        without, batched_pixels,
        "the off-canvas span in the middle of the batch changed the picture, \
         so `paint`'s per-span skip is not `egui_wgpu`'s"
    );
    assert_eq!(
        without_prims.len(),
        1,
        "the batch without the off-canvas span is not one primitive"
    );
}

/// **The order gate for the batch.** Spans that share a clip and overlap draw
/// in the order they were held, and the compare is shown to be able to see it.
///
/// A batch holds more than one span of one tile whenever a run between two
/// others is declined — the fill runs before it and after it are two spans at
/// the same placement under the same clip, and what covers what is the whole
/// question. `one_batched_callback_puts_the_same_bytes_on_screen_as_one_callback_per_tile`
/// cannot answer it: its spans are a tile grid, so their scissors make them
/// disjoint and any order draws the same picture. (That case is `#[ignore]`d
/// as this one is; both run with
/// `cargo test -p squallar-gpu --test tile_mesh_gpu -- --ignored`.)
///
/// Three readings of four **opaque, overlapping** quads (`layered_fills`),
/// each its own run, held as four separate spans at one placement:
///
/// * **per span** — four callbacks under one clip, which is what the walk
///   emitted before;
/// * **batched** — one callback holding the four spans in that order. Must
///   match byte for byte;
/// * **batched, reversed** — the same four spans in the opposite order. Must
///   *differ*, or the quads do not overlap enough for this suite to see an
///   order at all and the agreement above is vacuous.
#[test]
#[ignore = "needs a real wgpu adapter"]
fn spans_of_one_batch_draw_in_the_order_they_were_held() {
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
        "the fixture is four runs, or there are no spans to order"
    );

    // One span per run, so the order under test is the order of the SPANS and
    // not the order of the runs inside one span.
    let spans = |order: Vec<usize>| -> egui::Shape {
        let draws: Vec<tile_mesh::GroundDraw<'_>> = order
            .into_iter()
            .map(|run| tile_mesh::GroundDraw {
                meshes: &meshes,
                first_run: run,
                run_count: 1,
                place: tile_mesh::Placement::of(piece()),
                opacity: 1.0,
                clip: piece(),
            })
            .collect();
        egui::Shape::Callback(egui::epaint::PaintCallback {
            rect: piece(),
            callback: TileMeshBridge
                .payload(tile_mesh::GroundBatch {
                    draws: &draws,
                    pass_nr: 1,
                })
                .expect("the bridge always answers for spans it was given"),
        })
    };

    let format = wgpu::TextureFormat::Rgba8UnormSrgb;
    let mut renderer = renderer_for(&device, format);

    let per_span = frame(
        &device,
        &queue,
        &mut renderer,
        format,
        callback_shapes_per_run(&meshes, 0..4, 2),
    );
    let batched = frame(
        &device,
        &queue,
        &mut renderer,
        format,
        vec![spans(vec![0, 1, 2, 3])],
    );
    let reversed = frame(
        &device,
        &queue,
        &mut renderer,
        format,
        vec![spans(vec![3, 2, 1, 0])],
    );

    assert!(
        painted(&per_span) > (SIDE * SIDE / 4) as usize,
        "the per-span arm painted only {} texels, so a match would be two \
         nearly-empty pictures agreeing",
        painted(&per_span)
    );
    assert_eq!(
        per_span, batched,
        "four spans in one callback do not draw the picture of four callbacks \
         in the same order: the batch has changed what covers what"
    );
    assert_ne!(
        reversed, batched,
        "reversing the spans changed no pixel, so this suite cannot see the \
         order of a batch's spans and the agreement above is vacuous"
    );
}

/// **The gate for the hoist.** Four tiles drawn four ways, and the readbacks
/// compared byte for byte:
///
/// * **per tile, clipped** -- the arrangement the hoist replaces: each tile's
///   background placed on the whole tile and clipped to the piece by the
///   tile's own painter, then its geometry, tile after tile;
/// * **hoisted** -- what `draw_tile_layer` emits now: every background cut to
///   its piece by `tile_mesh::background_within` -- the hard mesh at the
///   pixel-rounded intersection -- under the canvas's clip, ahead of every
///   tile's geometry. Must match the first byte for byte, or the cut is not
///   the clip. (A *feathered* rectangle here is not: its alpha-zero outer
///   band still blends and dithers one pixel into each neighbour, 59 texels
///   on this canvas, which is what the scissor used to discard.);
/// * **hoisted, uncut** -- the same hoist without the intersection. The
///   stretched ancestor's background then covers all four pieces, so this
///   *must differ*: it is what shows the cut is load-bearing and the compare
///   can see a background where it does not belong;
/// * **hoisted, after the geometry** -- the backgrounds drawn last. Opaque
///   backgrounds over opaque quads, so this too *must differ*: it shows the
///   compare can see draw order, so the first agreement is evidence that the
///   hoist preserved it rather than of an insensitive compare.
///
/// The primitive count is asserted beside the pixels: the four clipped
/// backgrounds are four primitives and the four hoisted ones are one, which
/// is the whole reason the hoist exists.
#[test]
#[ignore = "needs a real wgpu adapter"]
fn the_hoisted_background_rectangles_put_the_same_bytes_on_screen_as_per_tile_clipping() {
    let _serialised = gpu_lock();
    let Some((device, queue)) = device() else {
        eprintln!("SKIPPED: no wgpu adapter");
        return;
    };
    let tiles = grid_tiles();
    let meshes = grid_meshes(&tiles);

    // Non-triviality of the fixture itself: the ancestor's placed background
    // really does reach over the other three pieces, so uncut it would paint
    // them; and every tile edge is off the pixel grid.
    // (To within an ulp of `f32` grid arithmetic: what matters is that the
    // uncut rectangle paints the neighbours' interiors, not a shared edge.)
    let ancestor = &tiles[3];
    for other in &tiles[..3] {
        assert!(
            ancestor.full.expand(0.01).contains_rect(other.piece),
            "fixture: the ancestor's whole tile {:?} does not cover {:?}",
            ancestor.full,
            other.piece
        );
    }
    for tile in &tiles {
        for edge in [tile.piece.min.x, tile.piece.max.x] {
            assert!(
                (edge.fract() - 0.5).abs() < 1e-4,
                "fixture: the x edge {edge} is not on a half-pixel, so the tie the \
                 rounding decides is untested"
            );
        }
        for edge in [tile.piece.min.y, tile.piece.max.y] {
            assert!(
                (edge - edge.round()).abs() > 0.05 && (edge.fract() - 0.5).abs() > 0.05,
                "fixture: the y edge {edge} sits on the pixel grid or on a half-pixel"
            );
        }
    }

    let format = wgpu::TextureFormat::Rgba8UnormSrgb;
    let mut renderer = renderer_for(&device, format);

    // The arrangement the hoist replaces.
    let per_tile: Vec<(egui::Rect, Vec<egui::Shape>)> = tiles
        .iter()
        .zip(&meshes)
        .map(|(tile, meshes)| {
            (
                tile.piece,
                vec![
                    egui::Shape::Rect(placed_background(tile)),
                    grid_callback(tile, meshes, 1),
                ],
            )
        })
        .collect();
    let (clipped, clipped_prims) = frame_clipped(&device, &queue, &mut renderer, format, per_tile);

    // What the walk emits now.
    let hoisted_rects: Vec<egui::Shape> = tiles
        .iter()
        .map(|tile| {
            tile_mesh::background_within(&placed_background(tile), tile.piece, 1.0)
                .expect("every background overlaps its piece")
        })
        .collect();
    let mut hoisted = vec![(canvas(), hoisted_rects.clone())];
    hoisted.extend(grid_geometry(&tiles, &meshes, 2));
    let (hoisted, hoisted_prims) = frame_clipped(&device, &queue, &mut renderer, format, hoisted);

    // Control one: hoisted without the cut.
    let mut uncut = vec![(
        canvas(),
        tiles
            .iter()
            .map(|tile| egui::Shape::Rect(placed_background(tile)))
            .collect(),
    )];
    uncut.extend(grid_geometry(&tiles, &meshes, 3));
    let (uncut, _) = frame_clipped(&device, &queue, &mut renderer, format, uncut);

    // Control two: hoisted after the geometry instead of ahead of it.
    let mut last = grid_geometry(&tiles, &meshes, 4);
    last.push((canvas(), hoisted_rects));
    let (rects_last, _) = frame_clipped(&device, &queue, &mut renderer, format, last);

    let drew = painted(&clipped);
    assert!(
        drew >= (4.0 * GRID_SIDE * GRID_SIDE * 0.95) as usize,
        "non-triviality: the clipped arrangement painted {drew} texels of the \
         {} four opaque pieces cover",
        (4.0 * GRID_SIDE * GRID_SIDE) as usize
    );
    assert_ne!(
        uncut, clipped,
        "the control is blind: the ancestor's background drawn uncut over its \
         three neighbours produced the same picture as the clipped arrangement, \
         so the cut is not load-bearing here and the agreement below proves nothing"
    );
    assert_ne!(
        rects_last, clipped,
        "the control is blind: drawing the backgrounds over the quads produced \
         the same picture as drawing them under, so the compare cannot see draw order"
    );

    let differing: Vec<String> = clipped
        .chunks_exact(4)
        .zip(hoisted.chunks_exact(4))
        .enumerate()
        .filter(|(_, (a, b))| a != b)
        .map(|(i, (a, b))| {
            format!(
                "({}, {}): clipped {a:?} hoisted {b:?}",
                i % SIDE as usize,
                i / SIDE as usize
            )
        })
        .collect();
    let worst = clipped
        .iter()
        .zip(hoisted.iter())
        .map(|(a, b)| a.abs_diff(*b))
        .max()
        .unwrap_or(0);
    // Reported whether or not it gates, so a run that passes still says how
    // close to the budget it came.
    println!(
        "{format:?}: {} of {} texels differ between the clipped and hoisted backgrounds, worst channel delta {worst}, over {drew} painted",
        differing.len(),
        SIDE * SIDE
    );
    // **A coverage tie is allowed here; an edge is not.** See [`LAST_BIT`].
    // [`GRID_ORIGIN`] puts every x edge on an exact half-pixel deliberately,
    // which is the position at which the feathered-and-rounded rectangle's
    // inner feather boundary lands exactly on a pixel centre — so whether that
    // centre reads the fill or a hair under it is a float tie the rasteriser
    // is free to break either way, and lavapipe breaks it differently
    // depending on the host it JITs for.
    assert!(
        worst <= LAST_BIT,
        "a channel differs by {worst} between the per-tile clipped \
         backgrounds and the hoisted cut ones, over {} texels -- the cut is \
         not the clip. One bit is a coverage tie at a half-pixel seam; \
         {worst} is an edge on the wrong side of a pixel, which is what \
         dropping `background_within`'s rounding does. The first of them:\n{}",
        differing.len(),
        differing
            .iter()
            .take(12)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    );
    assert!(
        differing.len() <= SEAM_TIE_BUDGET,
        "{} of {} texels differ between the per-tile clipped backgrounds and \
         the hoisted cut ones. Every one is within a bit, so no edge has \
         moved -- but {SEAM_TIE_BUDGET} is the budget, and a whole column of \
         this grid is {} texels. A disagreement that reaches a column is a \
         rounding rule that has changed, not a tie. The first of them:\n{}",
        differing.len(),
        SIDE * SIDE,
        GRID_SIDE as usize,
        differing
            .iter()
            .take(12)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    );

    // The count the hoist exists for: four clipped backgrounds were four
    // primitives; hoisted they are one mesh of four hard rectangles.
    let meshes_in = |prims: &[egui::ClippedPrimitive]| {
        prims
            .iter()
            .filter(|p| matches!(p.primitive, egui::epaint::Primitive::Mesh(_)))
            .count()
    };
    assert_eq!(
        (meshes_in(&clipped_prims), clipped_prims.len()),
        (4, 8),
        "the clipped arrangement is four background primitives and four callbacks"
    );
    assert_eq!(
        (meshes_in(&hoisted_prims), hoisted_prims.len()),
        (1, 5),
        "the hoisted arrangement is one background primitive and four callbacks"
    );
    let egui::epaint::Primitive::Mesh(first) = &hoisted_prims[0].primitive else {
        panic!("the hoisted backgrounds do not lead the primitive list")
    };
    assert_eq!(
        first.vertices.len(),
        4 * 4,
        "the one background mesh holds {} vertices, not four hard rectangles' 16",
        first.vertices.len()
    );
}
