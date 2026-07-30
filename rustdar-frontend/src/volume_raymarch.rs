//! The offscreen raymarch pipeline, and the quad that composites it into egui.
//!
//! # Why offscreen
//!
//! The raymarch renders into an `Rgba8Unorm` target of its own and `paint` then
//! draws one textured quad. That costs a pane-sized texture — budgeted at
//! [`crate::constants::VOLUME_OFFSCREEN_BUDGET_BYTES`] — and buys two things a
//! callback rendering inside egui's own pass cannot have:
//!
//! 1. **Resolution independent of pane size.** Fill rate, not shader
//!    translation, is the top risk here, and a callback in someone else's pass
//!    has no way to drop quality for a frame. Spike 0a measured 1.776 ms at
//!    2560 x 1440 against 0.229 at 720 x 450, so the lever demonstrably works.
//!    See `volume::quality`.
//! 2. **A colour space of its own.** egui blends premultiplied alpha in *gamma*
//!    space; a raymarch accumulates in linear space. Offscreen, the volume owns
//!    its own regime and only the final quad has to match egui's convention —
//!    one conversion, in one place, testable against egui's own output.
//!
//! # The two colour-space rules, both of them counter-intuitive
//!
//! **The offscreen holds sRGB-encoded premultiplied colour.** The raymarch
//! un-premultiplies before encoding and re-premultiplies after, because
//! encoding an already-premultiplied value is wrong at every alpha but 1.
//!
//! **The blit on an sRGB target decodes the premultiplied value directly.**
//! That is not what colour theory says. `egui_wgpu`'s own
//! `fs_main_linear_framebuffer` calls `linear_from_gamma_rgb` on colours it has
//! already premultiplied in gamma space, i.e. it composites `linear(C*A)`
//! rather than `linear(C)*A`. The principled version — un-premultiply, decode,
//! re-premultiply — measured **60/255 off** against egui's own `rect_filled`;
//! decoding the premultiplied value took the delta to **0**. Matching egui is
//! the requirement; being right in the abstract is not. Both formats are
//! reachable: `select_surface_format`'s non-sRGB preference is
//! `cfg(wasm32)`-only, so a native swapchain can and does land on sRGB.
//!
//! # naga constraints the shader is written around
//!
//! Every one of these is a real failure rather than a style choice, and they
//! are restated in `volume.wgsl` next to the code that obeys them:
//!
//! * `textureSampleLevel` everywhere. Implicit-LOD sampling under a
//!   data-dependent break is `FunctionError::NonUniformControlFlow`, a hard
//!   validator failure on every target rather than a driver quirk.
//! * One sampler per texture per pipeline: `Error::ImageMultipleSamplers`.
//! * Never `textureNumLevels`: it is gated on GLSL core 130 with no ES version
//!   at all, so it is unreachable on WebGL2 forever.
//! * A vertex buffer rather than `@builtin(vertex_index)` arithmetic.
//!
//! # What is NOT proven
//!
//! `tests/volume_shader.rs` translates every entry point to GLSL ES 300 under
//! the options wgpu-hal actually uses, and asserts the output carries no
//! `layout(binding` — which WebGL2 forbids — and is byte-identical for
//! `is_webgl` true and false. That establishes the generated GLSL is *legal*
//! ES 300.
//!
//! **Nothing here establishes that it links in a real browser.** Spike 0a could
//! not test that: the machine it ran on has no display, and a
//! software-rasteriser number would have been meaningless. A driver may still
//! refuse a program naga emitted correctly, which is precisely why
//! `volume::install_error_latch` and `volume::degrade` exist.

use egui_wgpu::wgpu;

use crate::constants::{VOLUME_LUT_BYTES, VOLUME_TEXTURE_BUDGET_BYTES};
use crate::egui_renderer::AttachmentConfig;
use crate::volume::VOLUME_TEXTURE_FORMAT;
use crate::volume::uniform::{VOLUME_UNIFORM_BYTES, VolumeUniform};

/// The WGSL every volume pipeline is built from.
///
/// `include_str!` rather than a runtime asset: a `.wgsl` shipped as a file would
/// need adding to five separate asset allowlists, and `check-relative-paths.py`
/// does not even read the extension. Embedding it also means a missing shader
/// is a build failure rather than a blank pane on one platform.
pub const VOLUME_SHADER_WGSL: &str = include_str!("volume.wgsl");

/// Label prefix every wgpu resource here must carry.
///
/// Not decoration. `volume::install_error_latch` decides whether an uncaptured
/// device error belongs to the volume view by looking for this prefix, and
/// re-panics on anything without it under `debug_assertions`. A resource
/// created without a matching label turns a survivable shader rejection into an
/// abort.
pub const LABEL_PREFIX: &str = "rustdar.volume";

/// Vertex entry point of the raymarch.
pub const ENTRY_VS_RAYMARCH: &str = "vs_raymarch";
/// Fragment entry point of the raymarch.
pub const ENTRY_FS_RAYMARCH: &str = "fs_raymarch";
/// Vertex entry point of the compositing quad.
pub const ENTRY_VS_BLIT: &str = "vs_blit";
/// Fragment entry point of the quad on a **non-sRGB** target: pass-through.
pub const ENTRY_FS_BLIT_GAMMA: &str = "fs_blit_gamma_framebuffer";
/// Fragment entry point of the quad on an **sRGB** target: decode to linear.
pub const ENTRY_FS_BLIT_LINEAR: &str = "fs_blit_linear_framebuffer";

/// Every entry point in [`VOLUME_SHADER_WGSL`], with the stage it belongs to.
///
/// Public because `tests/volume_shader.rs` translates exactly this list: an
/// entry point added to the WGSL and forgotten here would be shipped to a
/// browser without ever having been translated to GLSL.
pub const ENTRY_POINTS: [(&str, ShaderStage); 5] = [
    (ENTRY_VS_RAYMARCH, ShaderStage::Vertex),
    (ENTRY_FS_RAYMARCH, ShaderStage::Fragment),
    (ENTRY_VS_BLIT, ShaderStage::Vertex),
    (ENTRY_FS_BLIT_GAMMA, ShaderStage::Fragment),
    (ENTRY_FS_BLIT_LINEAR, ShaderStage::Fragment),
];

/// Which half of the pipeline an entry point belongs to.
///
/// A tiny local enum rather than `naga::ShaderStage`: naga is a **dev**
/// dependency here, so a shipped type cannot name it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShaderStage {
    /// A `@vertex` entry point.
    Vertex,
    /// A `@fragment` entry point.
    Fragment,
}

/// Bindings the raymarch pipeline declares, in group 0.
pub const BINDING_UNIFORM: u32 = 0;
/// See [`BINDING_UNIFORM`].
pub const BINDING_GRID_TEXTURE: u32 = 1;
/// See [`BINDING_UNIFORM`].
pub const BINDING_GRID_SAMPLER: u32 = 2;
/// See [`BINDING_UNIFORM`].
pub const BINDING_LUT_TEXTURE: u32 = 3;
/// See [`BINDING_UNIFORM`].
pub const BINDING_LUT_SAMPLER: u32 = 4;

/// Bindings the blit pipeline declares, also in group 0.
///
/// Deliberately numbered past the raymarch's rather than restarting at 0. One
/// WGSL module may not declare two resources with the same group and binding
/// pair, and both pipelines are built from one module — so the alternative is
/// two modules, which would double the naga test's surface for no gain.
pub const BINDING_BLIT_TEXTURE: u32 = 5;
/// See [`BINDING_BLIT_TEXTURE`].
pub const BINDING_BLIT_SAMPLER: u32 = 6;

/// The format the raymarch renders into.
///
/// **Not** `Rgba8UnormSrgb`. The raymarch writes bytes that are already
/// sRGB-encoded and premultiplied, exactly as egui's vertex colours are; an
/// sRGB view would make the hardware decode them on the way out and undo the
/// encode the fragment shader just performed.
pub const OFFSCREEN_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

/// The format the colour table is uploaded as.
///
/// Plain `Rgba8Unorm`, and the shader decodes it. Letting the hardware do it
/// with an `Rgba8UnormSrgb` view would work, but it would make the volume
/// depend on a second format's feature set that `volume::probe` does not check,
/// and the decode is two lines the fragment shader was already carrying for
/// egui's sake.
pub const LUT_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

/// egui's blend state, which the compositing quad has to match exactly.
///
/// Copied from `egui-wgpu-0.35.0/src/renderer.rs:414-425`. Premultiplied source
/// over destination for colour; `OneMinusDstAlpha`/`One` for alpha, which keeps
/// the destination's alpha meaningful when egui draws onto a transparent
/// window. Writing `OneMinusSrcAlpha` for the alpha component instead is the
/// plausible mistake, and on an opaque swapchain it is invisible.
pub const EGUI_BLEND: wgpu::BlendState = wgpu::BlendState {
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
};

/// Vertices in the fullscreen quad: two triangles.
pub const QUAD_VERTEX_COUNT: u32 = 6;

/// Bytes in the quad's vertex buffer: six `vec2<f32>`.
///
/// A vertex buffer rather than `@builtin(vertex_index)` arithmetic. 48 bytes is
/// nothing, and the arithmetic version is one more thing that has to survive
/// translation to GLSL ES 300 on a driver nobody has tested.
pub const QUAD_BYTES: usize = QUAD_VERTEX_COUNT as usize * 2 * 4;

/// Clip-space corners of the fullscreen quad, in draw order.
///
/// Counter-clockwise when read in wgpu's y-up clip space, matching
/// `FrontFace::Ccw` — though culling is off, so this is documentation rather
/// than a requirement.
const QUAD_CORNERS: [[f32; 2]; QUAD_VERTEX_COUNT as usize] = [
    [-1.0, -1.0],
    [1.0, -1.0],
    [-1.0, 1.0],
    [-1.0, 1.0],
    [1.0, -1.0],
    [1.0, 1.0],
];

/// The quad as the bytes the GPU reads. Hand-packed, like the uniform block.
pub fn quad_bytes() -> [u8; QUAD_BYTES] {
    let mut out = [0u8; QUAD_BYTES];
    for (vertex, corner) in QUAD_CORNERS.iter().enumerate() {
        for (axis, value) in corner.iter().enumerate() {
            let at = (vertex * 2 + axis) * 4;
            out[at..at + 4].copy_from_slice(&value.to_le_bytes());
        }
    }
    out
}

/// A label under [`LABEL_PREFIX`].
fn label(what: &str) -> String {
    format!("{LABEL_PREFIX}.{what}")
}

/// Everything a volume draw needs that does not depend on the data or the pane.
///
/// Built once per device. Two 3D panes at different sizes share this and hold
/// their own [`OffscreenTarget`]; the per-pane split matters because
/// `egui_wgpu::CallbackResources` is a `TypeMap` keyed by **type**, so one
/// inserted type is one slot for the whole application.
pub struct VolumePipelines {
    raymarch: wgpu::RenderPipeline,
    blit: wgpu::RenderPipeline,
    volume_layout: wgpu::BindGroupLayout,
    blit_layout: wgpu::BindGroupLayout,
    quad: wgpu::Buffer,
    grid_sampler: wgpu::Sampler,
    lut_sampler: wgpu::Sampler,
    blit_sampler: wgpu::Sampler,
    blit_entry_point: &'static str,
}

impl VolumePipelines {
    /// Build both pipelines for the pass egui draws into.
    ///
    /// `egui_attachments` is what `EguiRenderer::attachment_config()` reports.
    /// Only the **blit** needs it — the raymarch targets its own offscreen and
    /// is bound by [`OFFSCREEN_FORMAT`] instead. A pipeline built for a pass
    /// with a different colour format, sample count or depth attachment is a
    /// validation error at draw time, and `create_render_pipeline` returns no
    /// `Result` to notice it in.
    pub fn new(device: &wgpu::Device, egui_attachments: AttachmentConfig) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some(&label("shader")),
            source: wgpu::ShaderSource::Wgsl(VOLUME_SHADER_WGSL.into()),
        });

        let volume_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some(&label("raymarch.layout")),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: BINDING_UNIFORM,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        // Declared rather than left `None` so a buffer too small
                        // for the block is refused at bind-group creation
                        // instead of read past at draw time.
                        min_binding_size: wgpu::BufferSize::new(VOLUME_UNIFORM_BYTES as u64),
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: BINDING_GRID_TEXTURE,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        // Filterable is the stated reason `R8Unorm` was chosen:
                        // index-to-dBZ is affine, so hardware filtering within
                        // data is exactly linear dBZ interpolation.
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D3,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: BINDING_GRID_SAMPLER,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: BINDING_LUT_TEXTURE,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        // Non-filterable on purpose. The table is indexed, not
                        // interpolated: blending two palette entries would mix
                        // the colours of two unrelated dBZ levels.
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: BINDING_LUT_SAMPLER,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::NonFiltering),
                    count: None,
                },
            ],
        });

        let blit_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some(&label("blit.layout")),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: BINDING_BLIT_TEXTURE,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: BINDING_BLIT_SAMPLER,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });

        let quad = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(&label("quad")),
            size: QUAD_BYTES as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        // `Linear` on the grid for the reason the format was chosen; `Nearest`
        // on the table because an interpolated palette index is a colour from
        // between two dBZ levels. One sampler per texture, which is also a naga
        // requirement rather than only good sense.
        let grid_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some(&label("grid.sampler")),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            // The third axis matters here and nowhere else in this crate: the
            // gradient's central difference reaches one voxel outside the box
            // at every face, and a repeating address mode would wrap the top of
            // a storm round to the ground.
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Nearest,
            ..Default::default()
        });
        let lut_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some(&label("lut.sampler")),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            mipmap_filter: wgpu::MipmapFilterMode::Nearest,
            ..Default::default()
        });
        // `Linear` here is what makes the resolution rung usable at all: it is
        // the filter that turns a 720 x 450 offscreen back into a 1440 x 900
        // pane without it reading as a mosaic.
        let blit_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some(&label("blit.sampler")),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Nearest,
            ..Default::default()
        });

        let blit_entry_point = blit_entry_point_for(egui_attachments.color_format);

        let raymarch_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some(&label("raymarch.pipeline_layout")),
                bind_group_layouts: &[Some(&volume_layout)],
                immediate_size: 0,
            });
        let blit_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some(&label("blit.pipeline_layout")),
            bind_group_layouts: &[Some(&blit_layout)],
            immediate_size: 0,
        });

        let raymarch = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some(&label("raymarch")),
            layout: Some(&raymarch_pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some(ENTRY_VS_RAYMARCH),
                compilation_options: Default::default(),
                buffers: &[QUAD_VERTEX_LAYOUT],
            },
            primitive: QUAD_PRIMITIVE,
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some(ENTRY_FS_RAYMARCH),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: OFFSCREEN_FORMAT,
                    // No blending: the pass clears the target and the quad
                    // covers every texel exactly once, so each fragment is the
                    // final value rather than something to composite.
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });

        let blit = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some(&label("blit")),
            layout: Some(&blit_pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some(ENTRY_VS_BLIT),
                compilation_options: Default::default(),
                buffers: &[QUAD_VERTEX_LAYOUT],
            },
            primitive: QUAD_PRIMITIVE,
            depth_stencil: egui_attachments.depth_format.map(depth_state_for),
            multisample: wgpu::MultisampleState {
                count: egui_attachments.msaa_samples,
                ..Default::default()
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some(blit_entry_point),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: egui_attachments.color_format,
                    blend: Some(EGUI_BLEND),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });

        Self {
            raymarch,
            blit,
            volume_layout,
            blit_layout,
            quad,
            grid_sampler,
            lut_sampler,
            blit_sampler,
            blit_entry_point,
        }
    }

    /// Upload the quad. Separate from `new` because it needs a queue.
    pub fn upload_quad(&self, queue: &wgpu::Queue) {
        queue.write_buffer(&self.quad, 0, &quad_bytes());
    }

    /// Which blit fragment entry point this instance was built with.
    pub fn blit_entry_point(&self) -> &'static str {
        self.blit_entry_point
    }

    /// A target of `size` texels, with the bind group the blit reads it through.
    pub fn create_offscreen(&self, device: &wgpu::Device, size: [u32; 2]) -> OffscreenTarget {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some(&label("offscreen")),
            size: wgpu::Extent3d {
                width: offscreen_extent(size)[0],
                height: offscreen_extent(size)[1],
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: OFFSCREEN_FORMAT,
            // `COPY_SRC` and `COPY_DST` are for the tests, and worth the two
            // words: they are what lets `tests/volume_gpu.rs` read a rendered
            // frame back and seed a known premultiplied value without a
            // raymarch in the way. The second is what makes the blit's
            // zero-delta comparison against egui's own `rect_filled` possible
            // at all, and that comparison is the only evidence for the
            // counter-intuitive sRGB rule.
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_SRC
                | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some(&label("blit.bind_group")),
            layout: &self.blit_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: BINDING_BLIT_TEXTURE,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: BINDING_BLIT_SAMPLER,
                    resource: wgpu::BindingResource::Sampler(&self.blit_sampler),
                },
            ],
        });
        OffscreenTarget {
            size: offscreen_extent(size),
            texture,
            view,
            bind_group,
        }
    }

    /// Replace `target` only when the size it was built for has changed.
    ///
    /// Returns whether it reallocated. Reallocating every frame would be a
    /// pane-sized texture churned at the frame rate, which is the kind of thing
    /// that looks like a driver problem rather than an application one.
    pub fn ensure_offscreen(
        &self,
        device: &wgpu::Device,
        target: &mut Option<OffscreenTarget>,
        size: [u32; 2],
    ) -> bool {
        let wanted = offscreen_extent(size);
        if !offscreen_needs_rebuild(target.as_ref().map(OffscreenTarget::size), wanted) {
            return false;
        }
        *target = Some(self.create_offscreen(device, wanted));
        true
    }

    /// Upload a voxel grid and its colour table, and make the buffer the
    /// raymarch reads its camera from.
    ///
    /// `indices` is one byte per cell in x-fastest, then y, then z order.
    /// `lut` is [`VOLUME_LUT_BYTES`] of straight (non-premultiplied),
    /// gamma-encoded RGBA — what `get_color_for_value` produces.
    pub fn upload_volume(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        cells: [u32; 3],
        indices: &[u8],
        lut: &[u8],
    ) -> Option<VolumeTextures> {
        if let Some(why) = upload_refusal(cells, indices.len(), lut.len()) {
            log::error!("3D volume view: {why}");
            return None;
        }

        let grid = device.create_texture(&wgpu::TextureDescriptor {
            label: Some(&label("grid")),
            size: wgpu::Extent3d {
                width: cells[0],
                height: cells[1],
                depth_or_array_layers: cells[2],
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D3,
            format: VOLUME_TEXTURE_FORMAT,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        queue.write_texture(
            grid.as_image_copy(),
            indices,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                // No 256-byte row padding: `write_texture` repacks internally to
                // the backend's `buffer_copy_pitch`, which is 4 on GLES. But
                // `rows_per_image` MUST be `Some` when depth exceeds 1, or every
                // slice after the first is copied from the wrong offset.
                bytes_per_row: Some(cells[0]),
                rows_per_image: Some(cells[1]),
            },
            wgpu::Extent3d {
                width: cells[0],
                height: cells[1],
                depth_or_array_layers: cells[2],
            },
        );

        let lut_texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some(&label("lut")),
            size: wgpu::Extent3d {
                width: lut_texel_count(),
                height: 1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: LUT_FORMAT,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        queue.write_texture(
            lut_texture.as_image_copy(),
            lut,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(VOLUME_LUT_BYTES as u32),
                rows_per_image: Some(1),
            },
            wgpu::Extent3d {
                width: lut_texel_count(),
                height: 1,
                depth_or_array_layers: 1,
            },
        );

        let uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(&label("uniform")),
            size: VOLUME_UNIFORM_BYTES as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let grid_view = grid.create_view(&wgpu::TextureViewDescriptor::default());
        let lut_view = lut_texture.create_view(&wgpu::TextureViewDescriptor::default());
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some(&label("raymarch.bind_group")),
            layout: &self.volume_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: BINDING_UNIFORM,
                    resource: uniform.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: BINDING_GRID_TEXTURE,
                    resource: wgpu::BindingResource::TextureView(&grid_view),
                },
                wgpu::BindGroupEntry {
                    binding: BINDING_GRID_SAMPLER,
                    resource: wgpu::BindingResource::Sampler(&self.grid_sampler),
                },
                wgpu::BindGroupEntry {
                    binding: BINDING_LUT_TEXTURE,
                    resource: wgpu::BindingResource::TextureView(&lut_view),
                },
                wgpu::BindGroupEntry {
                    binding: BINDING_LUT_SAMPLER,
                    resource: wgpu::BindingResource::Sampler(&self.lut_sampler),
                },
            ],
        });

        Some(VolumeTextures {
            cells,
            uniform,
            bind_group,
        })
    }

    /// Record the raymarch into `target`.
    ///
    /// Its own render pass on the caller's encoder — for a paint callback that
    /// is `egui_encoder`, which egui submits *before* its own commands, so the
    /// offscreen is written before the blit reads it. Getting that order wrong
    /// paints last frame's volume, which looks like input lag rather than like
    /// a bug.
    pub fn encode_raymarch(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        target: &OffscreenTarget,
        volume: &VolumeTextures,
    ) {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some(&label("raymarch.pass")),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &target.view,
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
        pass.set_pipeline(&self.raymarch);
        pass.set_bind_group(0, &volume.bind_group, &[]);
        pass.set_vertex_buffer(0, self.quad.slice(..));
        pass.draw(0..QUAD_VERTEX_COUNT, 0..1);
    }

    /// Draw the offscreen into a pass the caller already opened.
    ///
    /// The caller is responsible for the viewport: the quad covers all of clip
    /// space, so `set_viewport` on the pane's rectangle is what places it. That
    /// is deliberate — it needs no second uniform and no per-frame vertex
    /// upload, and egui re-binds pipeline, scissor and viewport after every
    /// callback anyway.
    pub fn paint_blit(&self, pass: &mut wgpu::RenderPass<'static>, target: &OffscreenTarget) {
        pass.set_pipeline(&self.blit);
        pass.set_bind_group(0, &target.bind_group, &[]);
        pass.set_vertex_buffer(0, self.quad.slice(..));
        pass.draw(0..QUAD_VERTEX_COUNT, 0..1);
    }
}

/// The pane-sized target the raymarch renders into.
pub struct OffscreenTarget {
    size: [u32; 2],
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    bind_group: wgpu::BindGroup,
}

impl OffscreenTarget {
    /// Texels along each axis.
    pub fn size(&self) -> [u32; 2] {
        self.size
    }

    /// The texture itself, for a readback in a test.
    pub fn texture(&self) -> &wgpu::Texture {
        &self.texture
    }
}

/// A voxel grid and its palette, uploaded, plus the camera buffer.
pub struct VolumeTextures {
    cells: [u32; 3],
    uniform: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
}

impl VolumeTextures {
    /// Cells along each axis.
    pub fn cells(&self) -> [u32; 3] {
        self.cells
    }

    /// Point the raymarch's camera somewhere.
    pub fn write_uniform(&self, queue: &wgpu::Queue, uniform: &VolumeUniform) {
        queue.write_buffer(&self.uniform, 0, &uniform.to_bytes());
    }
}

/// Bytes an `R8Unorm` grid of this shape occupies, or `None` if it overflows.
pub fn grid_bytes(cells: [u32; 3]) -> Option<usize> {
    cells
        .iter()
        .try_fold(1usize, |acc, &n| acc.checked_mul(n as usize))
}

/// Entries in the colour table, which is also its texture's width.
///
/// Derived from the byte budget the table travels in rather than written as
/// 256, so the shader's `LUT_ENTRIES`, the upload's texture width and
/// `VOLUME_LUT_BYTES` cannot drift apart. A pure function rather than an
/// expression inlined into the texture descriptor because the descriptor needs
/// a device to reach, and this is arithmetic that can be wrong.
pub fn lut_texel_count() -> u32 {
    (VOLUME_LUT_BYTES / 4) as u32
}

/// The extent an offscreen is really created at.
///
/// Never zero on either axis. `wgpu` refuses a zero extent, and it refuses it
/// from `create_texture`, which returns no `Result` — so a pane dragged to
/// nothing would surface asynchronously through the uncaptured-error sink
/// rather than as a value anyone could check.
pub fn offscreen_extent(size: [u32; 2]) -> [u32; 2] {
    [size[0].max(1), size[1].max(1)]
}

/// Whether a held offscreen has to be thrown away for a new size.
///
/// Split out from [`VolumePipelines::ensure_offscreen`] because that function
/// needs a device and this decision does not. Getting it backwards reallocates
/// a pane-sized texture on every frame, which reads as a driver problem rather
/// than an application one.
fn offscreen_needs_rebuild(held: Option<[u32; 2]>, wanted: [u32; 2]) -> bool {
    held != Some(wanted)
}

/// Why an upload must be refused, or `None` when the shapes agree.
///
/// Pure, so the refusal can be tested without a GPU. Both halves matter and
/// neither implies the other: `write_texture` with too few bytes is a
/// validation error, and with too many it silently ignores the tail — so an
/// off-by-one grid would upload a plausible volume shifted by a slice.
fn upload_refusal(cells: [u32; 3], indices_len: usize, lut_len: usize) -> Option<String> {
    // `?` here would be exactly backwards: a cell count that overflows `usize`
    // is the strongest reason to refuse, and returning `None` for it would
    // report the grid as acceptable.
    let Some(expected) = grid_bytes(cells) else {
        return Some(format!(
            "refusing a {cells:?} grid: its cell count overflows"
        ));
    };
    if indices_len == expected && lut_len == VOLUME_LUT_BYTES {
        return None;
    }
    Some(format!(
        "refusing a {cells:?} grid with {indices_len} index bytes (expected \
         {expected}) and a {lut_len}-byte colour table (expected \
         {VOLUME_LUT_BYTES})"
    ))
}

/// Which blit fragment entry point a surface format needs.
///
/// Keyed on `is_srgb` rather than on the target: `select_surface_format` only
/// prefers a non-sRGB format under `cfg(target_arch = "wasm32")`, and natively
/// falls back to `capabilities.formats[0]` — which is an sRGB format on plenty
/// of drivers. Assuming either way is how a native build ends up with a volume
/// that is visibly darker than everything egui drew next to it.
pub fn blit_entry_point_for(format: wgpu::TextureFormat) -> &'static str {
    if format.is_srgb() {
        ENTRY_FS_BLIT_LINEAR
    } else {
        ENTRY_FS_BLIT_GAMMA
    }
}

/// The vertex layout both pipelines read the quad through.
const QUAD_VERTEX_LAYOUT: wgpu::VertexBufferLayout<'static> = wgpu::VertexBufferLayout {
    array_stride: 8,
    step_mode: wgpu::VertexStepMode::Vertex,
    attributes: &[wgpu::VertexAttribute {
        format: wgpu::VertexFormat::Float32x2,
        offset: 0,
        shader_location: 0,
    }],
};

/// Both pipelines rasterise the same way: a list of two triangles, no culling.
///
/// Culling is off rather than `Back` because the quad's winding is then one
/// transcription error away from drawing nothing at all, with no diagnostic —
/// and there is no depth or overdraw to save.
const QUAD_PRIMITIVE: wgpu::PrimitiveState = wgpu::PrimitiveState {
    topology: wgpu::PrimitiveTopology::TriangleList,
    strip_index_format: None,
    front_face: wgpu::FrontFace::Ccw,
    cull_mode: None,
    unclipped_depth: false,
    polygon_mode: wgpu::PolygonMode::Fill,
    conservative: false,
};

/// A depth state that matches a pass carrying a depth attachment without
/// reading or writing it.
///
/// `EguiRenderer::draw` attaches no depth buffer today, so this is unreachable
/// — but `AttachmentConfig` can carry one, and a pipeline that ignores a depth
/// format the pass has is a validation error at draw time. Writing the arm is
/// cheaper than discovering it.
fn depth_state_for(format: wgpu::TextureFormat) -> wgpu::DepthStencilState {
    wgpu::DepthStencilState {
        format,
        depth_write_enabled: Some(false),
        depth_compare: Some(wgpu::CompareFunction::Always),
        stencil: wgpu::StencilState::default(),
        bias: wgpu::DepthBiasState::default(),
    }
}

/// The grid budget, restated where the upload can see it.
///
/// Not enforced at upload time: the grid's shape is chosen in `rustdar-radar`
/// against `VOLUME_GRID_CELLS`, and refusing a grid here would turn a budget
/// regression into a blank pane rather than a failing test. The constant is
/// named so the two stay linked.
const _: () = assert!(VOLUME_TEXTURE_BUDGET_BYTES > VOLUME_LUT_BYTES);

#[cfg(test)]
mod tests {
    use super::*;

    /// [`VOLUME_SHADER_WGSL`] with its comments removed.
    ///
    /// Every "the shader must NOT contain X" assertion runs against this rather
    /// than the raw source, because the comments in `volume.wgsl` deliberately
    /// name the things the shader must not do — `textureNumLevels`,
    /// `dt * length(box_size_km)` — so that a reader learns why. Scanning the
    /// raw text would make those explanations trip their own guards, and the
    /// fix a hurried reader would reach for is deleting the explanation.
    ///
    /// `//` to end of line is the only comment form `volume.wgsl` uses, and
    /// WGSL has no string literals for a `//` to hide inside.
    fn shader_code() -> String {
        VOLUME_SHADER_WGSL
            .lines()
            .map(|line| line.split("//").next().unwrap_or(""))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// The comment-stripper actually strips something, and keeps the code.
    ///
    /// Without this, a `shader_code` that returned an empty string would make
    /// every absence assertion below pass vacuously — which is the failure mode
    /// of every scan-based test and the reason they need a control.
    #[test]
    fn the_comment_stripper_removes_prose_and_keeps_code() {
        let code = shader_code();
        assert!(
            code.len() < VOLUME_SHADER_WGSL.len() / 2,
            "volume.wgsl is {} bytes and its code is {} — the stripper is not \
             removing the comments",
            VOLUME_SHADER_WGSL.len(),
            code.len()
        );
        assert!(
            code.contains("fn fs_raymarch(") && code.contains("textureSampleLevel("),
            "the comment stripper removed code as well as comments"
        );
        assert!(
            !code.contains("naga"),
            "a word that appears only in this file's prose survived the stripper"
        );
    }

    /// The quad is 48 bytes of `vec2<f32>`, and it covers all of clip space.
    ///
    /// The size is the claim the module doc makes; the coverage is the claim
    /// the blit's viewport trick rests on. A quad that covered only part of
    /// clip space would blit a fraction of the offscreen into the whole pane,
    /// which reads as a zoomed-in volume rather than as a broken quad.
    #[test]
    fn the_quad_is_forty_eight_bytes_covering_all_of_clip_space() {
        assert_eq!(QUAD_BYTES, 48);
        assert_eq!(quad_bytes().len(), QUAD_BYTES);
        assert_eq!(QUAD_VERTEX_COUNT as usize % 3, 0, "not whole triangles");

        let xs: Vec<f32> = QUAD_CORNERS.iter().map(|c| c[0]).collect();
        let ys: Vec<f32> = QUAD_CORNERS.iter().map(|c| c[1]).collect();
        assert_eq!(xs.iter().cloned().fold(f32::INFINITY, f32::min), -1.0);
        assert_eq!(xs.iter().cloned().fold(f32::NEG_INFINITY, f32::max), 1.0);
        assert_eq!(ys.iter().cloned().fold(f32::INFINITY, f32::min), -1.0);
        assert_eq!(ys.iter().cloned().fold(f32::NEG_INFINITY, f32::max), 1.0);

        // All four corners present, so the two triangles really do tile the
        // rectangle rather than covering one half of it twice.
        for corner in [[-1.0, -1.0], [1.0, -1.0], [-1.0, 1.0], [1.0, 1.0]] {
            assert!(
                QUAD_CORNERS.contains(&corner),
                "clip-space corner {corner:?} is not in the quad, so part of \
                 the offscreen is never drawn"
            );
        }
    }

    /// The two triangles [`QUAD_CORNERS`] describes, in draw order.
    ///
    /// A test helper rather than production code: nothing that draws needs the
    /// quad grouped into triangles, but a coverage assertion has to talk about
    /// triangles — a quad that names all four corners can still fail to tile
    /// the rectangle.
    fn quad_triangles() -> [[[f32; 2]; 3]; 2] {
        [
            [QUAD_CORNERS[0], QUAD_CORNERS[1], QUAD_CORNERS[2]],
            [QUAD_CORNERS[3], QUAD_CORNERS[4], QUAD_CORNERS[5]],
        ]
    }

    /// The two triangles tile clip space exactly once, with no gap and no
    /// overlap.
    ///
    /// Added after a mutation survived the test above. `QUAD_CORNERS` has six
    /// negative components; deleting the minus from **four** of them leaves all
    /// four clip-space corners present and the bounding box unchanged, so every
    /// assertion up there still passes — while turning the pair into two
    /// triangles that both cover the upper half and leave a quadrant of the
    /// volume simply not drawn. (The other two are vertex 0's, and the corner
    /// check does catch those, because removing either loses `[-1, -1]`
    /// entirely.) Corner presence is not coverage, so assert coverage: this
    /// test catches all six.
    ///
    /// Sampled at points chosen to miss every edge: the shared diagonal is
    /// `x + y = 0`, and `-1.88 + 0.19 * (i + j)` is zero only at a
    /// non-integer `i + j`.
    #[test]
    fn the_two_triangles_tile_clip_space_exactly_once() {
        /// Which side of the directed line `a -> b` the point falls on.
        fn side(a: [f32; 2], b: [f32; 2], p: [f32; 2]) -> f32 {
            (b[0] - a[0]) * (p[1] - a[1]) - (b[1] - a[1]) * (p[0] - a[0])
        }
        /// Inside, for either winding.
        fn inside(triangle: [[f32; 2]; 3], p: [f32; 2]) -> bool {
            let sides = [
                side(triangle[0], triangle[1], p),
                side(triangle[1], triangle[2], p),
                side(triangle[2], triangle[0], p),
            ];
            sides.iter().all(|&s| s >= 0.0) || sides.iter().all(|&s| s <= 0.0)
        }

        let triangles = quad_triangles();
        for i in 0..10 {
            for j in 0..10 {
                let point = [-0.95 + 0.19 * i as f32, -0.93 + 0.19 * j as f32];
                let covering = triangles.iter().filter(|t| inside(**t, point)).count();
                assert_eq!(
                    covering, 1,
                    "clip-space point {point:?} is covered by {covering} of the \
                     quad's two triangles. Anything but one means the volume is \
                     missing a region of the pane, or drawing one twice."
                );
            }
        }
    }

    /// The quad's bytes are little-endian `f32` pairs in draw order.
    #[test]
    fn the_quad_packs_its_corners_in_draw_order() {
        let packed = quad_bytes();
        for (vertex, corner) in QUAD_CORNERS.iter().enumerate() {
            for (axis, expected) in corner.iter().enumerate() {
                let at = (vertex * 2 + axis) * 4;
                let value = f32::from_le_bytes(
                    <[u8; 4]>::try_from(&packed[at..at + 4]).expect("four bytes"),
                );
                assert_eq!(value, *expected, "vertex {vertex} axis {axis}");
            }
        }
        assert_eq!(
            QUAD_VERTEX_LAYOUT.array_stride as usize * QUAD_VERTEX_COUNT as usize,
            QUAD_BYTES,
            "the vertex stride and the packed bytes disagree, so the second \
             triangle reads from the wrong offset"
        );
    }

    /// sRGB targets get the decoding blit and non-sRGB ones the pass-through.
    ///
    /// This is the whole of bug #2's mitigation on the Rust side, and both arms
    /// are reachable natively — `select_surface_format` only prefers a non-sRGB
    /// format on wasm32.
    #[test]
    fn the_blit_entry_point_follows_the_surfaces_srgb_ness() {
        for format in [
            wgpu::TextureFormat::Rgba8UnormSrgb,
            wgpu::TextureFormat::Bgra8UnormSrgb,
        ] {
            assert_eq!(
                blit_entry_point_for(format),
                ENTRY_FS_BLIT_LINEAR,
                "{format:?} is an sRGB surface and did not get the decoding blit"
            );
        }
        for format in [
            wgpu::TextureFormat::Rgba8Unorm,
            wgpu::TextureFormat::Bgra8Unorm,
        ] {
            assert_eq!(
                blit_entry_point_for(format),
                ENTRY_FS_BLIT_GAMMA,
                "{format:?} is not an sRGB surface and did not get the \
                 pass-through blit"
            );
        }
    }

    /// The offscreen is not itself an sRGB format.
    ///
    /// It holds bytes the raymarch has already encoded. An `Rgba8UnormSrgb`
    /// target would have the hardware decode them on the way out, undoing that
    /// encode — and the result is plausible, merely washed out.
    #[test]
    fn the_offscreen_format_is_not_srgb() {
        assert!(!OFFSCREEN_FORMAT.is_srgb());
        assert!(!LUT_FORMAT.is_srgb());
    }

    /// The blend state is egui's, component for component.
    ///
    /// Written out rather than compared against a copy: `egui_wgpu` does not
    /// export the value, so the only thing that can be pinned locally is the
    /// literal. The measurement that actually proves the match is
    /// `the_blit_matches_egui_exactly_on_both_surface_formats`, which needs a
    /// GPU. The alpha component is the half worth staring at — `OneMinusDstAlpha`
    /// and `One`, not the `OneMinusSrcAlpha` symmetry invites.
    #[test]
    fn the_blend_state_is_the_one_egui_uses() {
        assert_eq!(EGUI_BLEND.color.src_factor, wgpu::BlendFactor::One);
        assert_eq!(
            EGUI_BLEND.color.dst_factor,
            wgpu::BlendFactor::OneMinusSrcAlpha
        );
        assert_eq!(EGUI_BLEND.color.operation, wgpu::BlendOperation::Add);
        assert_eq!(
            EGUI_BLEND.alpha.src_factor,
            wgpu::BlendFactor::OneMinusDstAlpha
        );
        assert_eq!(EGUI_BLEND.alpha.dst_factor, wgpu::BlendFactor::One);
        assert_eq!(EGUI_BLEND.alpha.operation, wgpu::BlendOperation::Add);
    }

    /// Every entry point this file names exists in the WGSL, and vice versa.
    ///
    /// Both directions are load-bearing. A name here that the shader does not
    /// declare is a pipeline that fails to create, from a call with no `Result`.
    /// A name in the shader that is missing from [`ENTRY_POINTS`] is worse: it
    /// is an entry point that ships to a browser having never been translated
    /// to GLSL by `tests/volume_shader.rs`.
    #[test]
    fn the_entry_point_list_is_exactly_what_the_shader_declares() {
        for (name, stage) in ENTRY_POINTS {
            let attribute = match stage {
                ShaderStage::Vertex => "@vertex",
                ShaderStage::Fragment => "@fragment",
            };
            let declaration = format!("fn {name}(");
            let at = VOLUME_SHADER_WGSL
                .find(&declaration)
                .unwrap_or_else(|| panic!("volume.wgsl declares no `{declaration}`"));
            let preceding = VOLUME_SHADER_WGSL[..at]
                .lines()
                .rev()
                .map(str::trim)
                .find(|line| !line.is_empty())
                .expect("nothing precedes the entry point");
            assert_eq!(
                preceding, attribute,
                "`{name}` is listed as a {stage:?} entry point but the shader \
                 declares it under `{preceding}`"
            );
        }

        let code = shader_code();
        let declared = code.matches("@vertex").count() + code.matches("@fragment").count();
        assert_eq!(
            declared,
            ENTRY_POINTS.len(),
            "volume.wgsl declares {declared} entry points but ENTRY_POINTS \
             lists {}. An unlisted entry point is never translated to GLSL by \
             the naga test, so it reaches a browser unchecked.",
            ENTRY_POINTS.len()
        );
    }

    /// The shader binds exactly the group-0 slots this file declares.
    ///
    /// A binding number that drifts between the WGSL and the bind group layout
    /// is a validation error at pipeline creation — from `create_render_pipeline`,
    /// which returns no `Result`, so it arrives asynchronously through the
    /// uncaptured-error sink instead.
    #[test]
    fn the_shaders_bindings_are_the_ones_the_layouts_declare() {
        for (binding, name) in [
            (BINDING_UNIFORM, "volume"),
            (BINDING_GRID_TEXTURE, "grid_texture"),
            (BINDING_GRID_SAMPLER, "grid_sampler"),
            (BINDING_LUT_TEXTURE, "lut_texture"),
            (BINDING_LUT_SAMPLER, "lut_sampler"),
            (BINDING_BLIT_TEXTURE, "blit_texture"),
            (BINDING_BLIT_SAMPLER, "blit_sampler"),
        ] {
            let expected = format!("@group(0) @binding({binding}) var");
            let line = VOLUME_SHADER_WGSL
                .lines()
                .find(|line| line.starts_with(&expected))
                .unwrap_or_else(|| {
                    panic!("volume.wgsl has no `{expected}` declaration for `{name}`")
                });
            assert!(
                line.contains(name),
                "binding {binding} is declared as `{line}`, not as `{name}`"
            );
        }

        let bindings = shader_code().matches("@binding(").count();
        assert_eq!(
            bindings, 7,
            "volume.wgsl declares {bindings} bindings; this file names 7, and a \
             binding the layouts do not declare fails pipeline creation"
        );
    }

    /// One sampler per texture, in each pipeline, as naga requires.
    ///
    /// `Error::ImageMultipleSamplers` is a real naga error, not a convention:
    /// a texture sampled through two samplers in one entry point does not
    /// translate to GLSL at all, because GLSL's `sampler3D` fuses the two.
    #[test]
    fn each_texture_has_exactly_one_sampler() {
        let code = shader_code();
        let textures = code.matches(": texture_").count();
        let samplers = code.matches(": sampler;").count();
        assert_eq!(
            (textures, samplers),
            (3, 3),
            "volume.wgsl declares {textures} textures and {samplers} samplers; \
             naga refuses a texture sampled through two samplers in one entry \
             point"
        );
    }

    /// The shader samples with an explicit level everywhere.
    ///
    /// Implicit-LOD sampling under the march's data-dependent break is
    /// `FunctionError::NonUniformControlFlow` — a hard validator failure on
    /// every target, not a driver quirk. `textureSample` compiles in a shader
    /// with no branching, so this is exactly the edit that would pass review.
    #[test]
    fn every_sample_gives_an_explicit_level() {
        let implicit = shader_code().matches("textureSample(").count();
        assert_eq!(
            implicit, 0,
            "volume.wgsl calls `textureSample` {implicit} time(s); the march \
             breaks on a data-dependent condition, so implicit-LOD sampling is \
             a validation failure on every backend"
        );
        assert!(shader_code().contains("textureSampleLevel("));
    }

    /// `textureNumLevels` appears nowhere.
    ///
    /// naga gates it on GLSL core 130 with no ES version at all, so it is
    /// unreachable on WebGL2 forever — and the failure would be at translation
    /// time on the browser only, i.e. on the target CI covers least.
    #[test]
    fn the_shader_never_asks_how_many_mip_levels_there_are() {
        assert!(
            !shader_code().contains("textureNumLevels"),
            "volume.wgsl calls `textureNumLevels`, which naga gates on GLSL \
             core 130 with no ES version at all"
        );
    }

    /// The step count is a `const`, so it folds to a literal in the loop.
    #[test]
    fn the_step_count_is_a_constant_the_loop_bound_names() {
        assert!(
            shader_code().contains("const RAYMARCH_STEPS: i32 = 96;"),
            "the raymarch's step count is no longer a `const` literal"
        );
        assert!(
            shader_code().contains("i < RAYMARCH_STEPS"),
            "the march's loop bound is no longer the constant"
        );
    }

    /// The step length puts the ray direction inside the `length`.
    ///
    /// This is spike 0a's first bug and it is worth the source scan, because
    /// `dt * length(box_size_km)` compiles, reads plausibly, and on the
    /// 240 x 240 x 20 km box makes a vertical ray roughly twelve times more
    /// opaque per step than a horizontal one — which looks like haze.
    ///
    /// `a_vertical_and_a_horizontal_ray_agree_on_opacity_per_kilometre` is the
    /// property test; this is the one that runs without a GPU.
    #[test]
    fn the_step_length_scales_the_direction_not_just_the_box() {
        assert!(
            shader_code().contains("return length(rd * dt * volume.box_size_km.xyz);"),
            "`step_length_km` no longer multiplies the direction by the box \
             size inside the `length`"
        );
        assert!(
            !shader_code().contains("dt * length(volume.box_size_km"),
            "the shader takes the length of the box size without the ray \
             direction, which makes opacity per step depend on nothing but the \
             box's diagonal"
        );
    }

    /// The anisotropy the guard above exists to prevent, stated as numbers.
    ///
    /// A source scan pins the text; this pins the *reason*, so a future reader
    /// who wants to simplify the shader can see what it costs. Both figures are
    /// worth having: the absolute one says how far off a vertical ray is, and
    /// the relative one is why the result reads as haze rather than as a bug —
    /// the whole image gets denser together, so nothing looks inconsistent.
    ///
    /// The box is the one the volume actually uses: 240 km across, 20 km deep.
    #[test]
    fn the_wrong_step_length_is_seventeen_times_off_and_twelve_times_anisotropic() {
        let box_size_km = [240.0f64, 240.0, 20.0];
        // The wrong formula, `dt * length(box_size_km)`, gives every direction
        // the box's diagonal.
        let wrong = box_size_km.iter().map(|km| km * km).sum::<f64>().sqrt();

        // The right one gives each axis-aligned ray that axis' own extent.
        let vertical = box_size_km[2];
        let horizontal = box_size_km[0];

        let vertical_inflation = wrong / vertical;
        let horizontal_inflation = wrong / horizontal;
        assert!(
            (16.5..17.5).contains(&vertical_inflation),
            "a vertical step would be {vertical_inflation:.1}x too long, not \
             the ~17x the shader's comment claims"
        );
        assert!(
            (1.3..1.5).contains(&horizontal_inflation),
            "a horizontal step would be {horizontal_inflation:.1}x too long, \
             not the ~1.4x the shader's comment claims"
        );

        let anisotropy = vertical_inflation / horizontal_inflation;
        assert!(
            (11.5..12.5).contains(&anisotropy),
            "the bug would leave a vertical ray {anisotropy:.1}x more opaque \
             relative to a horizontal one, not the ~12x claimed"
        );
        assert!(
            (anisotropy - horizontal / vertical).abs() < 1e-9,
            "the relative distortion is exactly the box's aspect ratio, and \
             this arithmetic no longer says so"
        );
    }

    /// The sRGB blit decodes the premultiplied value, without un-premultiplying.
    ///
    /// Spike 0a's second finding, and the counter-intuitive one: the principled
    /// version measured 60/255 off against egui's own `rect_filled`, and
    /// decoding the premultiplied value directly took the delta to 0. A future
    /// reader who "fixes" this is making the output wrong, so pin it.
    #[test]
    fn the_srgb_blit_decodes_the_premultiplied_value_directly() {
        let body = entry_point_body(ENTRY_FS_BLIT_LINEAR);
        assert!(
            body.contains("linear_from_gamma_rgb(premultiplied_gamma.rgb)"),
            "the sRGB blit no longer decodes the premultiplied value the way \
             egui's own fs_main_linear_framebuffer does: {body}"
        );
        assert!(
            !body.contains('/'),
            "the sRGB blit divides — the only division it could want is by \
             alpha, to un-premultiply before decoding. That is the \
             colour-theoretically correct answer and it measured 60/255 away \
             from egui's own output; matching egui is the requirement: {body}"
        );
    }

    /// And the non-sRGB blit does not decode at all.
    #[test]
    fn the_non_srgb_blit_is_a_pass_through() {
        let body = entry_point_body(ENTRY_FS_BLIT_GAMMA);
        assert!(
            !body.contains("linear_from_gamma_rgb") && !body.contains("gamma_from_linear_rgb"),
            "the non-sRGB blit converts colour space. egui writes gamma-encoded \
             premultiplied colour onto that surface and blends it in gamma \
             space, which is exactly what the offscreen already holds: {body}"
        );
        assert!(body.contains("textureSampleLevel("));
    }

    /// The raymarch un-premultiplies before encoding and re-premultiplies after.
    ///
    /// The other half of the colour rule, and the half that *is* principled:
    /// encoding an already-premultiplied value is wrong at every alpha but 1.
    #[test]
    fn the_raymarch_encodes_a_straight_colour_and_premultiplies_after() {
        let body = entry_point_body(ENTRY_FS_RAYMARCH);
        assert!(
            body.contains("let straight_linear = accumulated / alpha;"),
            "the raymarch no longer un-premultiplies before encoding: {body}"
        );
        assert!(
            body.contains("gamma_from_linear_rgb(straight_linear) * alpha"),
            "the raymarch no longer re-premultiplies after encoding, so the \
             offscreen holds a straight colour where egui's convention is \
             premultiplied: {body}"
        );
    }

    /// The transfer functions are egui's, character for character.
    ///
    /// Rewriting either — a different cutoff, a 2.2 exponent instead of 2.4 —
    /// produces output that is wrong by a few counts everywhere, which reads as
    /// a slightly different theme rather than as a bug.
    #[test]
    fn the_transfer_functions_match_eguis_own() {
        for line in [
            "let cutoff = srgb < vec3<f32>(0.04045);",
            "let lower = srgb / vec3<f32>(12.92);",
            "let higher = pow((srgb + vec3<f32>(0.055)) / vec3<f32>(1.055), vec3<f32>(2.4));",
            "let cutoff = rgb < vec3<f32>(0.0031308);",
            "let lower = rgb * vec3<f32>(12.92);",
            "let higher = vec3<f32>(1.055) * pow(rgb, vec3<f32>(1.0 / 2.4)) - vec3<f32>(0.055);",
        ] {
            assert!(
                shader_code().contains(line),
                "volume.wgsl's sRGB transfer functions have diverged from \
                 egui-wgpu's egui.wgsl:44-57; this line is gone: {line}"
            );
        }
    }

    /// Grid byte counts, including the overflow the multiplication can hit.
    #[test]
    fn a_grids_byte_count_is_its_cell_count() {
        assert_eq!(grid_bytes([256, 256, 128]), Some(8 * 1024 * 1024));
        assert_eq!(grid_bytes([128, 128, 64]), Some(1024 * 1024));
        assert_eq!(grid_bytes([1, 1, 1]), Some(1));
        assert_eq!(
            grid_bytes([u32::MAX, u32::MAX, u32::MAX]),
            None,
            "a grid whose cell count overflows `usize` must not wrap to a small \
             number and then be compared against a slice length"
        );
    }

    /// An offscreen never has a zero axis, and a real size passes through.
    ///
    /// Both halves: clamping unconditionally to 1 would be as wrong as not
    /// clamping at all, and `create_texture` — where this lands — returns no
    /// `Result` for either.
    #[test]
    fn an_offscreen_extent_is_clamped_up_from_zero_and_left_alone_otherwise() {
        assert_eq!(offscreen_extent([0, 0]), [1, 1]);
        assert_eq!(offscreen_extent([0, 900]), [1, 900]);
        assert_eq!(offscreen_extent([1440, 0]), [1440, 1]);
        assert_eq!(offscreen_extent([1440, 900]), [1440, 900]);
    }

    /// A held offscreen is rebuilt for a new size and kept for the same one.
    ///
    /// The mistake this catches is the comparison inverted: a pane-sized
    /// texture reallocated on every frame is invisible in a screenshot and
    /// reads as a driver problem rather than as an application one.
    #[test]
    fn an_offscreen_is_rebuilt_only_when_its_size_changed() {
        assert!(
            offscreen_needs_rebuild(None, [1440, 900]),
            "nothing held must always be built"
        );
        assert!(
            !offscreen_needs_rebuild(Some([1440, 900]), [1440, 900]),
            "an offscreen of the right size was thrown away and rebuilt"
        );
        for changed in [[1441, 900], [1440, 901], [900, 1440]] {
            assert!(
                offscreen_needs_rebuild(Some([1440, 900]), changed),
                "a {changed:?} pane reused a 1440x900 offscreen, so it would be \
                 blitted at the wrong scale"
            );
        }
    }

    /// An upload whose shapes disagree is refused, and one that agrees is not.
    ///
    /// The three ways to get this wrong are all here: too few index bytes, too
    /// many, and a colour table of the wrong length. `write_texture` is a
    /// validation error for the first and **silently ignores the tail** for the
    /// second, which uploads a plausible volume shifted by a slice.
    #[test]
    fn an_upload_whose_shapes_disagree_is_refused() {
        let cells = [8u32, 8, 8];
        let cell_count = 8 * 8 * 8;
        assert_eq!(upload_refusal(cells, cell_count, VOLUME_LUT_BYTES), None);

        for (indices, lut, what) in [
            (cell_count - 1, VOLUME_LUT_BYTES, "one index byte short"),
            (cell_count + 1, VOLUME_LUT_BYTES, "one index byte long"),
            (0, VOLUME_LUT_BYTES, "no indices at all"),
            (cell_count, VOLUME_LUT_BYTES - 4, "a table one entry short"),
            (cell_count, 0, "no colour table"),
        ] {
            assert!(
                upload_refusal(cells, indices, lut).is_some(),
                "an upload with {what} was accepted"
            );
        }

        assert!(
            upload_refusal([u32::MAX, u32::MAX, u32::MAX], 0, VOLUME_LUT_BYTES).is_some(),
            "a grid whose cell count overflows `usize` was accepted; that is \
             the strongest reason to refuse, not a reason to say nothing"
        );
    }

    /// The colour table's texture width is its entry count, from the budget.
    #[test]
    fn the_colour_tables_texture_is_as_wide_as_the_budget_pays_for() {
        assert_eq!(lut_texel_count(), 256);
        assert_eq!(lut_texel_count() as usize * 4, VOLUME_LUT_BYTES);
        assert!(
            shader_code().contains(&format!(
                "const LUT_ENTRIES: f32 = {}.0;",
                lut_texel_count()
            )),
            "the shader's palette size and the uploaded texture's width \
             disagree, so every colour is fetched from a fraction of a texel off"
        );
    }

    /// Every wgpu label this module writes is under the latch's prefix.
    ///
    /// `install_error_latch` re-panics on any uncaptured error whose message
    /// does not carry `rustdar.volume`, under `debug_assertions`. So a resource
    /// created here without the prefix converts a survivable driver refusal
    /// into an abort — on the target where an abort is a dead browser tab.
    #[test]
    fn every_label_this_module_writes_carries_the_latch_prefix() {
        let source = include_str!("volume_raymarch.rs");
        let mut labels = 0;
        for fragment in source.split("label(\"").skip(1) {
            let (name, _) = fragment.split_once('"').expect("an unterminated label");
            // Skip the definition of `label` itself and the doc comments.
            if name.contains('{') {
                continue;
            }
            labels += 1;
            assert!(
                label(name).starts_with(LABEL_PREFIX),
                "the label helper produced `{}` for `{name}`, which the \
                 uncaptured-error latch would treat as an unrelated error",
                label(name)
            );
        }
        assert!(
            labels >= 10,
            "only {labels} labels were found; the scan is not looking where it \
             thinks it is"
        );
        assert!(
            !source.contains("label: Some(\""),
            "a wgpu descriptor in this module writes a literal label instead of \
             going through `label()`, so it may not carry the \
             `{LABEL_PREFIX}` prefix the error latch keys on"
        );
    }

    /// The body of one WGSL entry point, from its `{` to the matching `}`.
    fn entry_point_body(name: &str) -> &'static str {
        let at = VOLUME_SHADER_WGSL
            .find(&format!("fn {name}("))
            .unwrap_or_else(|| panic!("volume.wgsl declares no `{name}`"));
        let open = VOLUME_SHADER_WGSL[at..]
            .find('{')
            .expect("an entry point with no body");
        let start = at + open;
        let mut depth = 0usize;
        for (offset, byte) in VOLUME_SHADER_WGSL[start..].bytes().enumerate() {
            match byte {
                b'{' => depth += 1,
                b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        return &VOLUME_SHADER_WGSL[start..=start + offset];
                    }
                }
                _ => {}
            }
        }
        panic!("`{name}`'s body is not brace-balanced")
    }
}
