//! Binding egui's index and vertex buffers once per reset, and drawing every
//! mesh by `first_index` into them, puts the same bytes on screen as binding
//! them at every mesh's own offset did.
//!
//! The vendored `egui_wgpu::Renderer::render` (see `vendor/egui-wgpu/VENDORED.md`)
//! binds both buffers whole where it re-establishes egui's pipeline and draws
//! each mesh as `draw_indexed(first_index..first_index + n, 0, 0..1)`; for
//! that to address the right vertices, `update_buffers` writes each mesh's
//! indices **rebased by the mesh's vertex base** — the start of its vertex
//! slice, in vertices. Two things can be wrong in that and neither shows as a
//! validation error: a base off by a mesh draws the wrong vertices, and a
//! `first_index` off by a mesh draws the wrong indices. Both draw *something*,
//! so the gate is pixels.
//!
//! **The reference.** A picture tessellated into many meshes — one clip rect
//! per cell, so every mesh has a different, non-zero vertex base — is drawn
//! and read back; the same shapes, tessellated under one clip rect, come out
//! as **one** mesh whose base is zero and whose `first_index` is zero, which
//! is exactly the draw upstream made. Inside that one mesh the tessellator
//! already did the rebasing itself (`Mesh::append` adds the vertex offset), so
//! the two arms are two independent rebases of the same geometry, and the
//! readbacks must not differ by a byte.
//!
//! **The control.** Every parity assertion passes on a renderer that draws the
//! wrong vertices *consistently*, so a third arm stages the many-mesh picture
//! with one mesh's indices rebased by a base one mesh too far — the exact
//! failure the cut can produce — and the readback **must differ**. The
//! staging seam (`egui_wgpu::GeometryStager`) is how the tamper gets in, and
//! the stager is otherwise an ordinary mapped-buffer copy, so this file needs
//! no adapter feature: it runs on llvmpipe and SwiftShader as it does on a
//! discrete card. Run it with
//! `cargo test -p squallar-gpu --test egui_bind_once_gpu -- --ignored`; it
//! names the adapter it got, and whether that was hardware or software.

#![cfg(not(target_arch = "wasm32"))]

use std::ops::Range;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use egui_wgpu::wgpu;
use squallar_gpu::egui_renderer::command_stream::census;

/// The offscreen target's side, in texels. Every cell of [`scene`] lies inside
/// it with room to spare.
const SIDE: u32 = 512;

/// Cells across and down. 12 x 24 = 288 meshes on the many-mesh arm, each
/// with a distinct vertex base.
const COLUMNS: u32 = 12;
const ROWS: u32 = 24;
const CELL: egui::Vec2 = egui::vec2(42.0, 21.0);

/// A device on whatever adapter answers; no feature is asked for.
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
        label: Some("egui-bind-once"),
        required_features: wgpu::Features::empty(),
        required_limits: adapter.limits(),
        memory_hints: wgpu::MemoryHints::default(),
        experimental_features: wgpu::ExperimentalFeatures::default(),
        trace: wgpu::Trace::Off,
    }))
    .ok()?;
    Some((device, queue, info))
}

/// The picture: a grid of cells, each a rounded rect, a disc and a label, all
/// on egui's font texture. `split` gives every cell its own clip rect, which
/// is what makes the tessellator open one mesh per cell; without it the whole
/// grid is one mesh. Every shape sits at least two points inside its cell so
/// the cell scissor on the split arm clips nothing — feathering included —
/// and the two arms draw the same pixels by construction.
fn scene(ctx: &egui::Context, split: bool) -> (Vec<egui::ClippedPrimitive>, egui::TexturesDelta) {
    let canvas = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(SIDE as f32, SIDE as f32));
    ctx.begin_pass(egui::RawInput {
        screen_rect: Some(canvas),
        ..Default::default()
    });
    for row in 0..ROWS {
        for column in 0..COLUMNS {
            let at = egui::pos2(column as f32 * CELL.x + 2.0, row as f32 * CELL.y + 2.0);
            let base = ctx.layer_painter(egui::LayerId::background());
            let painter = if split {
                base.with_clip_rect(egui::Rect::from_min_size(at, CELL))
            } else {
                base
            };
            painter.rect_filled(
                egui::Rect::from_min_size(at + egui::vec2(2.0, 2.0), egui::vec2(28.0, 5.0)),
                1.5,
                egui::Color32::from_rgb(
                    (row * 9) as u8,
                    (column * 17) as u8,
                    ((row + column) * 5) as u8,
                ),
            );
            painter.circle_filled(
                at + egui::vec2(35.0, 11.0),
                3.5,
                egui::Color32::from_rgb(200, (row * 7) as u8, (column * 11) as u8),
            );
            painter.text(
                at + egui::vec2(2.0, 8.0),
                egui::Align2::LEFT_TOP,
                format!("{row}.{column}"),
                egui::FontId::proportional(7.0),
                egui::Color32::WHITE,
            );
        }
    }
    let output = ctx.end_pass();
    let tris = ctx.tessellate(output.shapes, 1.0);
    (tris, output.textures_delta)
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
    deltas: &[egui::TexturesDelta],
) -> Vec<u8> {
    // egui's mesh arm looks its texture up by id and silently draws nothing
    // without it, so an unuploaded atlas would make every arm agree on an
    // empty picture. The blank-picture control below is what would catch
    // that; this is what keeps it from firing.
    for delta in deltas {
        for (id, image) in &delta.set {
            renderer.update_texture(device, queue, *id, image);
        }
    }

    let descriptor = egui_wgpu::ScreenDescriptor {
        size_in_pixels: [SIDE, SIDE],
        pixels_per_point: 1.0,
    };
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("egui-bind-once target"),
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
            label: Some("egui-bind-once pass"),
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
        label: Some("egui-bind-once readback"),
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

/// Bytes that differ between two readbacks of the same size.
fn differing(a: &[u8], b: &[u8]) -> usize {
    assert_eq!(a.len(), b.len(), "the two readbacks are not the same size");
    a.iter().zip(b).filter(|(x, y)| x != y).count()
}

/// Bytes that are not the clear colour: the "something was drawn" floor.
fn painted(pixels: &[u8]) -> usize {
    pixels.iter().filter(|&&b| b != 0).count()
}

/// The meshes of a primitive list, in the order `update_buffers` stages them.
fn meshes(tris: &[egui::ClippedPrimitive]) -> impl Iterator<Item = &egui::epaint::Mesh> {
    tris.iter().filter_map(|clipped| match &clipped.primitive {
        egui::epaint::Primitive::Mesh(mesh) => Some(mesh),
        egui::epaint::Primitive::Callback(_) => None,
    })
}

/// A stager that copies exactly what the renderer wrote — through an ordinary
/// `MAP_WRITE | COPY_SRC` buffer, so no adapter feature is involved — and then
/// overwrites one mesh's index words with the same indices rebased by a base
/// **one mesh too far**. The failure the rebase can produce, injected at the
/// only seam that sees the staged bytes.
struct WrongBase {
    /// The victim's indices, rebased by the wrong base, as bytes.
    wrong: Vec<u8>,
    /// Where they go in the index half of the region.
    at: Range<usize>,
    /// Stagings made, so the arm can prove the tamper was applied at all.
    applied: Arc<AtomicUsize>,
}

impl WrongBase {
    /// The tamper for mesh `victim` of `tris`: its index bytes' range and its
    /// indices rebased by the *next* mesh's base rather than its own.
    fn for_mesh(tris: &[egui::ClippedPrimitive], victim: usize) -> (Self, Arc<AtomicUsize>) {
        let mut index_offset = 0usize;
        let mut vertex_base = 0u32;
        for (k, mesh) in meshes(tris).enumerate() {
            let bytes = mesh.indices.len() * size_of::<u32>();
            if k == victim {
                let wrong_base = vertex_base + mesh.vertices.len() as u32;
                let wrong = mesh
                    .indices
                    .iter()
                    .flat_map(|index| (index + wrong_base).to_ne_bytes())
                    .collect();
                let applied = Arc::new(AtomicUsize::new(0));
                return (
                    Self {
                        wrong,
                        at: index_offset..index_offset + bytes,
                        applied: Arc::clone(&applied),
                    },
                    applied,
                );
            }
            index_offset += bytes;
            vertex_base += mesh.vertices.len() as u32;
        }
        panic!("the scene has no mesh {victim} to tamper with");
    }
}

impl egui_wgpu::GeometryStager for WrongBase {
    fn stage(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        index: (&wgpu::Buffer, u64),
        vertex: (&wgpu::Buffer, u64),
        fill: &mut dyn FnMut(&mut wgpu::BufferViewMut),
    ) -> bool {
        let (index_buffer, index_bytes) = index;
        let (vertex_buffer, vertex_bytes) = vertex;
        let total = index_bytes + vertex_bytes;
        let staging = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("egui-bind-once wrong-base staging"),
            size: total,
            usage: wgpu::BufferUsages::MAP_WRITE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: true,
        });
        {
            let mut region = staging.slice(..).get_mapped_range_mut();
            fill(&mut region);
            region.slice(self.at.clone()).copy_from_slice(&self.wrong);
        }
        staging.unmap();
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        if index_bytes > 0 {
            encoder.copy_buffer_to_buffer(&staging, 0, index_buffer, 0, index_bytes);
        }
        if vertex_bytes > 0 {
            encoder.copy_buffer_to_buffer(&staging, index_bytes, vertex_buffer, 0, vertex_bytes);
        }
        queue.submit(Some(encoder.finish()));
        self.applied.fetch_add(1, Ordering::Relaxed);
        true
    }
}

/// The whole gate in one device: many meshes drawn by `first_index` out of
/// buffers bound once match the one-mesh tessellation of the same shapes
/// byte for byte, and a base one mesh too far does not.
///
/// One test rather than three because each arm costs an adapter, a device and
/// a font atlas, and because the third is what keeps the first from passing
/// vacuously: a comparison that cannot fail proves nothing.
#[test]
#[ignore = "needs a real wgpu adapter"]
fn many_meshes_drawn_by_first_index_match_the_one_mesh_tessellation() {
    let Some((device, queue, info)) = device() else {
        eprintln!("no wgpu adapter: nothing measured, nothing asserted");
        return;
    };
    let arm = match info.device_type {
        wgpu::DeviceType::Cpu => "software",
        wgpu::DeviceType::DiscreteGpu
        | wgpu::DeviceType::IntegratedGpu
        | wgpu::DeviceType::VirtualGpu => "hardware",
        wgpu::DeviceType::Other => "unknown",
    };
    eprintln!(
        "adapter: {} / {:?} / {:?} ({arm}); driver {} {}",
        info.name, info.backend, info.device_type, info.driver, info.driver_info
    );
    let format = wgpu::TextureFormat::Rgba8Unorm;
    let surface = [SIDE, SIDE];

    // One context for both tessellations, so the font atlas is built once and
    // laid out identically for both arms. Every delta either pass emitted is
    // uploaded to every renderer, in order.
    let ctx = egui::Context::default();
    let (many, many_delta) = scene(&ctx, true);
    let (one, one_delta) = scene(&ctx, false);
    let deltas = [many_delta, one_delta];

    // Controls on the two tessellations: the many-mesh arm really has many
    // meshes, every one drawn, and the one-mesh arm really has one.
    let many_census = census(&many, 1.0, surface);
    let one_census = census(&one, 1.0, surface);
    let expected = u64::from(COLUMNS * ROWS);
    assert_eq!(
        (many_census.meshes, many_census.draws, many_census.skipped),
        (expected, expected, 0),
        "the split scene tessellated to {} meshes, {} drawn, {} skipped; the \
         arm needs every one of {expected} cells to be its own mesh with its \
         own vertex base, and every one drawn",
        many_census.meshes,
        many_census.draws,
        many_census.skipped,
    );
    assert_eq!(
        (one_census.meshes, one_census.draws),
        (1, 1),
        "the unsplit scene tessellated to {} meshes; the reference arm must be \
         the one mesh whose base and `first_index` are both zero",
        one_census.meshes,
    );
    assert_eq!(
        (many_census.buffer_binds, one_census.buffer_binds),
        (2, 2),
        "{} and {} buffer binds for the two arms; both are one reset, so both \
         bind the buffers exactly once",
        many_census.buffer_binds,
        one_census.buffer_binds,
    );
    let (vertices, indices) = meshes(&many).fold((0usize, 0usize), |acc, mesh| {
        (acc.0 + mesh.vertices.len(), acc.1 + mesh.indices.len())
    });
    eprintln!(
        "scene: {} meshes, {vertices} vertices, {indices} indices on the split arm; \
         {} calls recorded against {} with a bind per mesh",
        many_census.meshes,
        many_census.calls,
        many_census.calls - many_census.buffer_binds + 2 * many_census.draws,
    );

    let mut many_route = renderer(&device, format);
    let through_many = draw(&device, &queue, &mut many_route, format, &many, &deltas);
    let mut one_route = renderer(&device, format);
    let through_one = draw(&device, &queue, &mut one_route, format, &one, &deltas);

    let inked = painted(&through_one);
    assert!(
        inked > 10_000,
        "the reference picture has {inked} non-zero bytes of {}; a picture \
         that blank cannot tell a right draw from a wrong one",
        through_one.len(),
    );

    let parity = differing(&through_many, &through_one);
    assert_eq!(
        parity,
        0,
        "{parity} of {} bytes differ between {} meshes drawn by `first_index` \
         out of buffers bound once and the same shapes drawn as one mesh; a \
         mesh's rebased indices or its `first_index` address the wrong geometry",
        through_one.len(),
        many_census.meshes,
    );

    // The control: the same many-mesh picture with one mesh's indices rebased
    // one mesh too far MUST differ, or the parity above proved nothing.
    let victim = (COLUMNS * ROWS / 2) as usize;
    let (tamper, applied) = WrongBase::for_mesh(&many, victim);
    let wrong_words: Vec<u32> = tamper
        .wrong
        .chunks_exact(4)
        .map(|b| u32::from_ne_bytes([b[0], b[1], b[2], b[3]]))
        .collect();
    assert!(
        wrong_words.iter().all(|&w| (w as usize) < vertices),
        "the tamper's indices reach past the {vertices} staged vertices; an \
         out-of-range index is robustness-clamped by the driver and would test \
         the driver, not the rebase",
    );
    let mut tampered_route = renderer(&device, format);
    tampered_route.set_geometry_stager(Box::new(tamper));
    let through_tamper = draw(&device, &queue, &mut tampered_route, format, &many, &deltas);
    assert_eq!(
        applied.load(Ordering::Relaxed),
        1,
        "the wrong-base stager was asked to stage {} time(s) over one \
         `update_buffers`; a control that never ran is not a control",
        applied.load(Ordering::Relaxed),
    );
    let sensitivity = differing(&through_tamper, &through_many);
    eprintln!("sensitivity: mesh {victim} rebased one mesh too far moved {sensitivity} bytes");
    assert!(
        sensitivity > 0,
        "mesh {victim}'s indices rebased by the next mesh's base drew the same \
         {} bytes as the correct base; the parity assertion above cannot see a \
         wrong base and is vacuous",
        through_many.len(),
    );
}
