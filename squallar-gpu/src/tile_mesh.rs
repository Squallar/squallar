//! The renderer half of [`squallar_egui::tile_mesh`]: a vector tile's fills
//! and strokes, uploaded once per tile lifetime and drawn with its placement
//! as a uniform.
//!
//! The two differ in one thing each: the vertex layout the pipeline reads
//! ([`stroke::StrokeVertex`] against `TileVertex`) and the index width. A
//! stroke run's `u16` indices are rebased onto the run's own first vertex, so
//! the run binds the vertex buffer at that offset rather than passing a base
//! vertex — WebGL2 has no draw call that takes one.
//!
//! [`TileMeshStore`] lives in `egui_wgpu`'s [`CallbackResources`], which is
//! keyed by type and therefore one slot for the whole application — so the
//! per-tile map is inside it, exactly as the volume's per-pane map is inside
//! `VolumeResources`.
//!
//! # Residency, and what releases it
//!
//! An entry is keyed by [`TileMeshes::id`] and holds a [`Weak`] handle to the
//! flattened buffers the tile cache owns. **That handle is the eviction
//! rule**: the styled tile is dropped when it leaves the tile LRU or a restyle
//! replaces it, the weak handle goes dead, and the next frame's sweep gives
//! the GPU buffers back. Nothing here has a budget of its own to disagree
//! with the tile cache's, and nothing survives a tile that is gone.
//!
//! The sweep runs once per egui pass, not once per callback: `prepare` is
//! called for every one of the frame's ground draws, and a per-callback sweep
//! over a few hundred entries would cost more than the placement this replaces.
//! [`GroundDraw::pass_nr`] is what makes "once per pass" a fact.
//!
//! # The uniform ring
//!
//! Every draw needs its own placement, and `paint` takes `&self` — so the
//! slot cannot be handed over through the store. It is written in `prepare`,
//! which runs for **every** callback of a frame before **any** of them paints
//! (`Renderer::update_buffers` dispatches all the prepares; `Renderer::render`
//! runs afterwards), and remembered on the callback in an [`AtomicU32`]. The
//! ring is monotonic and wraps, so no frame boundary has to be found: a slot
//! is written and read inside one frame, and [`RING_SLOTS`] is the bound on
//! how many ground draws one frame may make before slots would alias.

use std::any::Any;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Weak};

use egui_wgpu::wgpu;
use squallar_egui::tile_mesh::{
    GroundDraw, Placement, RunKind, TILE_VERTEX_BYTES, TileMeshPainter, TileMeshes, ledger, stroke,
};

/// Placements one frame may carry before the ring wraps onto a slot the same
/// frame is still going to read.
///
/// **Derived from three bounds, not chosen.** A pane spans at most 84 tiles
/// (the 3440x1440 figure the campaign's scene A is measured at); the desktop
/// layout caps at six panes; and only the basemap layer carries runs — the
/// terrain hillshade is raster, and a raster tile flattens to no runs at all.
///
/// Runs per tile are what the style's interleaving of fill layers, line
/// layers and anything else that draws leaves behind: adjacent fills coalesce
/// (`mvt::coalesce_adjacent_meshes`) and adjacent strokes span. **Measured on
/// the committed dark style over the Monaco fixture's z14 tile: two fill runs
/// and two stroke runs, four in all** (`squallar-egui`'s
/// `tile_mesh::fixture_tests::the_stroke_buffers_are_smaller_than_epaints_own_format`
/// prints it). At sixteen runs per tile — four times that — the worst case is
/// 6 x 84 x 16 = 8,064, inside this.
///
/// That product is itself an over-count in one more way worth naming: six
/// panes **share** one screen, so six times a full-screen tile span is
/// several times the tiles that can actually be on the glass at once. The
/// margin is therefore larger than the 1.6% the arithmetic above shows.
///
/// 8,192 slots is 2 MiB of uniform buffer at the 256-byte offset alignment a
/// desktop adapter reports, allocated once.
const RING_SLOTS: u32 = 8192;

/// The 3440x1440 tile span one pane can hold. See [`RING_SLOTS`].
const TILES_PER_PANE: u32 = 84;
/// The desktop layout's pane cap. See [`RING_SLOTS`].
const PANES: u32 = 6;
/// Four times the four runs per tile measured on the committed dark style
/// over the Monaco fixture's z14 tile. See [`RING_SLOTS`].
const RUNS_PER_TILE: u32 = 16;

// **A build failure, not a test.** A ring that wraps inside one frame
// overwrites a slot that frame is still going to read, and draws one tile at
// another tile's placement — a wrong picture, silently. The bound moved once
// already, when strokes joined the fills on this path and two runs per tile
// became four; whatever moves it next has to come past this line.
const _: () = assert!(
    PANES * TILES_PER_PANE * RUNS_PER_TILE <= RING_SLOTS,
    "a frame's worst case of ground draws no longer fits the uniform ring"
);

/// Bytes one [`Locals`] block occupies: `vec2 + vec2 + f32 + u32 + vec2`.
const LOCALS_BYTES: u64 = 32;

/// One draw's placement, in the byte layout the WGSL `Locals` block declares.
///
/// Assembled field by field rather than cast from a `repr(C)` struct: this
/// crate forbids `unsafe`, and thirty-two bytes written once per draw is not
/// where a frame is spent.
fn locals_bytes(
    screen_size: [f32; 2],
    place: Placement,
    dithering: bool,
) -> [u8; LOCALS_BYTES as usize] {
    let mut out = [0u8; LOCALS_BYTES as usize];
    let lanes: [[u8; 4]; 6] = [
        screen_size[0].to_ne_bytes(),
        screen_size[1].to_ne_bytes(),
        place.translation[0].to_ne_bytes(),
        place.translation[1].to_ne_bytes(),
        place.scale.to_ne_bytes(),
        u32::from(dithering).to_ne_bytes(),
    ];
    for (lane, bytes) in lanes.iter().enumerate() {
        out[lane * 4..lane * 4 + 4].copy_from_slice(bytes);
    }
    out
}

/// One tile's fills, resident on the GPU.
struct Resident {
    /// The fill buffer pair and the stroke buffer pair — each absent for a
    /// tile whose style produced no run of that kind at this zoom.
    ///
    /// **`Option` and not four buffers**, because a zero-length buffer is a
    /// wgpu validation error rather than an empty draw, and a tile of nothing
    /// but roads is an ordinary thing for a style at one zoom to produce.
    fills: Option<(wgpu::Buffer, wgpu::Buffer)>,
    strokes: Option<(wgpu::Buffer, wgpu::Buffer)>,
    bytes: u64,
    /// The tile cache's ownership, seen from here. Dead means the tile is
    /// gone and so are these buffers, next sweep.
    alive: Weak<TileMeshes>,
}

/// What the ground draws need across frames: the pipeline, the uniform ring,
/// and the tiles that are resident.
pub struct TileMeshStore {
    pipeline: wgpu::RenderPipeline,
    /// The same shader and the same blend, over
    /// [`stroke::StrokeVertex`]'s layout and a `u16` index buffer.
    stroke_pipeline: wgpu::RenderPipeline,
    bind_group: wgpu::BindGroup,
    ring: wgpu::Buffer,
    /// Bytes between two ring slots — the adapter's uniform offset alignment,
    /// never smaller than one [`Locals`].
    stride: u32,
    cursor: u32,
    dithering: bool,
    resident: HashMap<u64, Resident>,
    resident_bytes: u64,
    /// This store's own upload tally, beside the process-wide ledger's. The
    /// ledger is what the product line reports; these are what a test can
    /// assert on without another test in the same process moving them.
    uploads: u64,
    upload_bytes: u64,
    /// The pass the last sweep ran for, so the sweep is once per frame rather
    /// than once per draw.
    swept_pass: Option<u64>,
}

impl TileMeshStore {
    /// Build the pipeline for a pass with these attachments.
    ///
    /// `dithering` must be what `RendererOptions::dithering` gave egui's own
    /// renderer — see [`crate::egui_renderer::EGUI_DITHERING`]. The two
    /// shaders draw side by side into one pass, and a tile whose fills were
    /// not dithered beside egui's dithered gradients is a visible seam.
    pub fn new(
        device: &wgpu::Device,
        attachments: crate::egui_renderer::AttachmentConfig,
        dithering: bool,
    ) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("tile mesh"),
            source: wgpu::ShaderSource::Wgsl(include_str!("tile_mesh.wgsl").into()),
        });

        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("tile mesh locals"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: true,
                    min_binding_size: wgpu::BufferSize::new(LOCALS_BYTES),
                },
                count: None,
            }],
        });

        let stride = align_up(
            LOCALS_BYTES as u32,
            device.limits().min_uniform_buffer_offset_alignment,
        );
        let ring = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("tile mesh locals ring"),
            size: u64::from(stride) * u64::from(RING_SLOTS),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("tile mesh locals"),
            layout: &layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                    buffer: &ring,
                    offset: 0,
                    size: wgpu::BufferSize::new(LOCALS_BYTES),
                }),
            }],
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("tile mesh"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });

        let fill_pipeline = build_pipeline(
            device,
            &shader,
            &pipeline_layout,
            attachments,
            "tile mesh",
            "vs_main",
            &[wgpu::VertexBufferLayout {
                array_stride: TILE_VERTEX_BYTES,
                step_mode: wgpu::VertexStepMode::Vertex,
                attributes: &[
                    wgpu::VertexAttribute {
                        format: wgpu::VertexFormat::Float32x2,
                        offset: 0,
                        shader_location: 0,
                    },
                    wgpu::VertexAttribute {
                        format: wgpu::VertexFormat::Uint32,
                        offset: 8,
                        shader_location: 1,
                    },
                ],
            }],
        );
        let stroke_pipeline = build_pipeline(
            device,
            &shader,
            &pipeline_layout,
            attachments,
            "tile stroke",
            "vs_stroke",
            &[wgpu::VertexBufferLayout {
                array_stride: stroke::STROKE_VERTEX_BYTES,
                step_mode: wgpu::VertexStepMode::Vertex,
                attributes: &[
                    // The centreline point, in extent units. `Sint16x2` and
                    // not a float pair: MVT coordinates are integers and
                    // `tile_mesh::stroke` refuses any that is not one in
                    // range, so this is exact. The GLES backend maps it to
                    // `glVertexAttribIPointer(SHORT)`, which WebGL2 has.
                    wgpu::VertexAttribute {
                        format: wgpu::VertexFormat::Sint16x2,
                        offset: 0,
                        shader_location: 0,
                    },
                    // What epaint's tessellator put between the point and
                    // this vertex, in screen points. Added after the
                    // placement, never scaled by it.
                    wgpu::VertexAttribute {
                        format: wgpu::VertexFormat::Float32x2,
                        offset: 4,
                        shader_location: 1,
                    },
                    wgpu::VertexAttribute {
                        format: wgpu::VertexFormat::Uint32,
                        offset: 12,
                        shader_location: 2,
                    },
                ],
            }],
        );

        Self {
            pipeline: fill_pipeline,
            stroke_pipeline,
            bind_group,
            ring,
            stride,
            cursor: 0,
            dithering,
            resident: HashMap::new(),
            resident_bytes: 0,
            uploads: 0,
            upload_bytes: 0,
            swept_pass: None,
        }
    }

    /// Give back every tile the cache has let go of. Once per pass; see the
    /// module doc.
    fn sweep(&mut self, pass_nr: u64) {
        if self.swept_pass == Some(pass_nr) {
            return;
        }
        self.swept_pass = Some(pass_nr);
        let mut freed = 0u64;
        let mut bytes = 0u64;
        self.resident.retain(|_, entry| {
            if entry.alive.strong_count() > 0 {
                return true;
            }
            freed += 1;
            bytes += entry.bytes;
            false
        });
        if freed > 0 {
            self.resident_bytes -= bytes;
            ledger::note_mesh_eviction(freed);
        }
        ledger::set_mesh_resident_bytes(self.resident_bytes);
    }

    /// Make one tile resident, uploading it if this is the first frame it has
    /// been drawn on. Answers `false` when there is nothing to draw.
    fn ensure(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, meshes: &Arc<TileMeshes>) {
        if self.resident.contains_key(&meshes.id()) {
            return;
        }
        // The one write per tile lifetime this whole mechanism is for.
        let fills = upload_pair(
            device,
            queue,
            "tile mesh",
            meshes.vertex_bytes(),
            meshes.index_bytes(),
        );
        let strokes = upload_pair(
            device,
            queue,
            "tile stroke",
            meshes.stroke_vertex_bytes(),
            meshes.stroke_index_bytes(),
        );

        let bytes = meshes.bytes();
        self.resident_bytes += bytes;
        self.uploads += 1;
        self.upload_bytes += bytes;
        self.resident.insert(
            meshes.id(),
            Resident {
                fills,
                strokes,
                bytes,
                alive: Arc::downgrade(meshes),
            },
        );
        ledger::note_mesh_upload(bytes);
        ledger::set_mesh_resident_bytes(self.resident_bytes);
    }

    /// Write one draw's placement into the ring and answer which slot it went
    /// in.
    fn slot(&mut self, queue: &wgpu::Queue, screen_size: [f32; 2], place: Placement) -> u32 {
        let slot = self.cursor;
        self.cursor = (self.cursor + 1) % RING_SLOTS;
        queue.write_buffer(
            &self.ring,
            u64::from(slot) * u64::from(self.stride),
            &locals_bytes(screen_size, place, self.dithering),
        );
        slot
    }

    /// Bytes this store is holding for tiles. The always-on figure the
    /// eviction claim is read off; see [`ledger`].
    pub fn resident_bytes(&self) -> u64 {
        self.resident_bytes
    }

    /// Tiles this store is holding.
    pub fn resident_tiles(&self) -> usize {
        self.resident.len()
    }

    /// Buffer writes this store has made, and their bytes — one pair per tile
    /// lifetime, never per frame. The per-store half of [`ledger`]'s
    /// process-wide totals.
    pub fn uploads(&self) -> (u64, u64) {
        (self.uploads, self.upload_bytes)
    }
}

/// Make one kind of run's buffer pair resident, or answer `None` when there is
/// nothing of that kind on this tile.
///
/// **The `None` is not an error path.** A style at one zoom can produce a tile
/// of nothing but roads, or nothing but water, and a zero-length wgpu buffer
/// is a validation failure rather than an empty draw. The run list is what
/// decides whether either half is ever asked for; this is what keeps the
/// unasked-for half from being allocated at all.
fn upload_pair(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    label: &str,
    vertices: &[u8],
    indices: &[u8],
) -> Option<(wgpu::Buffer, wgpu::Buffer)> {
    if vertices.is_empty() || indices.is_empty() {
        return None;
    }
    let vertex_buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some(label),
        size: vertices.len() as u64,
        usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let index_buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some(label),
        size: indices.len() as u64,
        usage: wgpu::BufferUsages::INDEX | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    queue.write_buffer(&vertex_buffer, 0, vertices);
    queue.write_buffer(&index_buffer, 0, indices);
    Some((vertex_buffer, index_buffer))
}

/// One of the two pipelines, which differ only in the vertex entry point and
/// the vertex buffer layout.
///
/// Everything else — the blend state, the depth state, the multisample count
/// and which fragment entry point the target's sRGB-ness selects — is egui's
/// own and is shared, because the fills, the strokes and egui's own primitives
/// draw into one pass and a difference is a seam in the map.
fn build_pipeline(
    device: &wgpu::Device,
    shader: &wgpu::ShaderModule,
    layout: &wgpu::PipelineLayout,
    attachments: crate::egui_renderer::AttachmentConfig,
    label: &str,
    vertex_entry: &str,
    buffers: &[wgpu::VertexBufferLayout<'_>],
) -> wgpu::RenderPipeline {
    // egui picks its fragment entry point off the target's sRGB-ness and this
    // must pick the same one, or these draws are gamma-shifted against every
    // other primitive in the pass. Derived here rather than passed in, so the
    // two pipelines cannot be given different answers to one question.
    let fragment_entry = if attachments.color_format.is_srgb() {
        "fs_main_linear_framebuffer"
    } else {
        "fs_main_gamma_framebuffer"
    };
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some(label),
        layout: Some(layout),
        vertex: wgpu::VertexState {
            module: shader,
            entry_point: Some(vertex_entry),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            buffers,
        },
        primitive: wgpu::PrimitiveState::default(),
        depth_stencil: attachments
            .depth_format
            .map(|format| wgpu::DepthStencilState {
                format,
                // egui's own pipeline writes no depth and compares Always;
                // these draw in the same pass and must not start.
                depth_write_enabled: Some(false),
                depth_compare: Some(wgpu::CompareFunction::Always),
                stencil: wgpu::StencilState::default(),
                bias: wgpu::DepthBiasState::default(),
            }),
        multisample: wgpu::MultisampleState {
            count: attachments.msaa_samples.max(1),
            mask: !0,
            alpha_to_coverage_enabled: false,
        },
        fragment: Some(wgpu::FragmentState {
            module: shader,
            entry_point: Some(fragment_entry),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            // egui's blend state, copied rather than chosen: premultiplied
            // over for colour, and the alpha lane that keeps a
            // transparent-target composite right.
            targets: &[Some(wgpu::ColorTargetState {
                format: attachments.color_format,
                blend: Some(wgpu::BlendState {
                    color: wgpu::BlendComponent {
                        src_factor: wgpu::BlendFactor::One,
                        dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                        operation: wgpu::BlendOperation::Add,
                    },
                    alpha: wgpu::BlendComponent {
                        src_factor: wgpu::BlendFactor::OneMinusDstAlpha,
                        dst_factor: wgpu::BlendFactor::One,
                        operation: wgpu::BlendOperation::Add,
                    },
                }),
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        multiview_mask: None,
        cache: None,
    })
}

/// Round `value` up to a multiple of `alignment`.
fn align_up(value: u32, alignment: u32) -> u32 {
    let alignment = alignment.max(1);
    value.div_ceil(alignment) * alignment
}

/// One tile's fill run, on one frame.
struct TileMeshCallback {
    /// Keeps the flattened buffers alive until `prepare` has read them, and is
    /// what the store's weak handle is taken from.
    meshes: Arc<TileMeshes>,
    kind: RunKind,
    /// A stroke run's first vertex, which the vertex buffer is bound at so the
    /// run's `u16` indices address it from zero. WebGL2 has no base-vertex
    /// draw call, which is why this is a binding offset rather than an
    /// argument to `draw_indexed`.
    first_vertex: u32,
    first_index: u32,
    index_count: u32,
    place: Placement,
    pass_nr: u64,
    /// Written by `prepare`, read by `paint`. See the module doc: every
    /// prepare of a frame runs before any paint of it.
    slot: AtomicU32,
}

impl egui_wgpu::CallbackTrait for TileMeshCallback {
    fn prepare(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        screen_descriptor: &egui_wgpu::ScreenDescriptor,
        _egui_encoder: &mut wgpu::CommandEncoder,
        callback_resources: &mut egui_wgpu::CallbackResources,
    ) -> Vec<wgpu::CommandBuffer> {
        let Some(store) = callback_resources.get_mut::<TileMeshStore>() else {
            // Counted rather than silent, and counted here rather than said:
            // this crate declares no `log` dependency (see
            // `texture_upload::TextureUploads::totals_if_moved` for the same
            // split), and a store-less callback is the one wiring mistake
            // that produces an ordinary-looking map with no fills in it.
            ledger::note_mesh_store_missing();
            return Vec::new();
        };
        store.sweep(self.pass_nr);
        store.ensure(device, queue, &self.meshes);

        // `ScreenDescriptor::screen_size_in_points` is private to egui-wgpu;
        // this is its body, and it must stay its body — egui's own uniform
        // carries the same number and the two shaders map to clip space with
        // it.
        let points = [
            screen_descriptor.size_in_pixels[0] as f32 / screen_descriptor.pixels_per_point,
            screen_descriptor.size_in_pixels[1] as f32 / screen_descriptor.pixels_per_point,
        ];
        let slot = store.slot(queue, points, self.place);
        self.slot.store(slot, Ordering::Relaxed);
        Vec::new()
    }

    fn paint(
        &self,
        info: egui::PaintCallbackInfo,
        render_pass: &mut wgpu::RenderPass<'static>,
        callback_resources: &egui_wgpu::CallbackResources,
    ) {
        let Some(store) = callback_resources.get::<TileMeshStore>() else {
            return;
        };
        let Some(resident) = store.resident.get(&self.meshes.id()) else {
            return;
        };
        // egui set a viewport from the callback's rect as a courtesy. The
        // geometry here is already in screen points — the uniform placed it —
        // so the viewport has to be the whole frame or the tile would be
        // squeezed into its own rect a second time. The scissor egui set from
        // the clip rect is what keeps a stretched ancestor inside its piece,
        // and is deliberately left alone.
        render_pass.set_viewport(
            0.0,
            0.0,
            info.screen_size_px[0] as f32,
            info.screen_size_px[1] as f32,
            0.0,
            1.0,
        );
        match self.kind {
            RunKind::Fill => {
                let Some((vertices, indices)) = &resident.fills else {
                    return;
                };
                render_pass.set_pipeline(&store.pipeline);
                render_pass.set_vertex_buffer(0, vertices.slice(..));
                render_pass.set_index_buffer(indices.slice(..), wgpu::IndexFormat::Uint32);
            }
            RunKind::Stroke => {
                let Some((vertices, indices)) = &resident.strokes else {
                    return;
                };
                render_pass.set_pipeline(&store.stroke_pipeline);
                render_pass.set_vertex_buffer(
                    0,
                    vertices.slice(u64::from(self.first_vertex) * stroke::STROKE_VERTEX_BYTES..),
                );
                render_pass.set_index_buffer(indices.slice(..), wgpu::IndexFormat::Uint16);
            }
        }
        render_pass.set_bind_group(
            0,
            &store.bind_group,
            &[self.slot.load(Ordering::Relaxed) * store.stride],
        );
        render_pass.draw_indexed(
            self.first_index..self.first_index + self.index_count,
            0,
            0..1,
        );
    }
}

/// The seam's renderer side: turns one ground draw into the payload
/// `egui_wgpu` downcasts.
///
/// Holds nothing. The store is in the callback resources and the buffers are
/// in the store, so this is installed once and never has to be replaced when
/// a tile arrives or goes.
#[derive(Default)]
pub struct TileMeshBridge;

impl TileMeshPainter for TileMeshBridge {
    fn payload(&self, draw: GroundDraw<'_>) -> Option<Arc<dyn Any + Send + Sync>> {
        let run = *draw.meshes.runs().get(draw.run)?;
        Some(
            egui_wgpu::Callback::new_paint_callback(
                egui::Rect::ZERO,
                TileMeshCallback {
                    meshes: Arc::clone(draw.meshes),
                    kind: run.kind,
                    first_vertex: run.first_vertex,
                    first_index: run.first_index,
                    index_count: run.index_count,
                    place: draw.place,
                    pass_nr: draw.pass_nr,
                    slot: AtomicU32::new(0),
                },
            )
            .callback,
        )
    }
}

#[cfg(test)]
#[path = "tile_mesh/tests.rs"]
mod tests;
