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

/// The march's per-ray sample ceiling, restated for hosts that mirror the
/// shader's arithmetic (the silhouette harness casts the same rays in Rust).
///
/// The WGSL constant is the source of truth; this copy is pinned to the
/// literal in the shader text by `the_step_count_is_a_constant_the_loop_bound_names`,
/// so the two cannot drift silently. 1024 because the cloud rung's half-cell
/// step must cover the desktop grid's 384-cell diagonal — 768 steps — without
/// falling to the stretched-dt fallback.
pub const RAYMARCH_STEP_CEILING: i32 = 1024;

/// Cells one march step advances along the ray, in the grid's own cell
/// metric, **at the instrument default**: the value `VolumeUniform::new`
/// writes into the step lane, which is what the silhouette harness's mirror
/// marches at. Production may hand the shader a different step per frame
/// (`volume::bridge::CLOUD_STEP_CELLS` halves it for the cloud rung); the
/// uniform's default and this constant are pinned to each other by
/// `the_step_count_is_a_constant_the_loop_bound_names`.
pub const RAYMARCH_STEP_CELLS: f32 = 1.0;

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

/// The map floor's bindings, in **group 1** of the raymarch pipeline.
///
/// A group of their own because their lifetime is their own: group 0 is
/// rebuilt with every grid upload, the mirror once per frame, and when no
/// mirror is in hand the pipelines' one-texel transparent placeholder binds
/// here — so `encode_raymarch` always has a complete layout and the shader's
/// floor arm is dead code until `flags.w` says otherwise.
pub const BINDING_FLOOR_TEXTURE: u32 = 0;
/// See [`BINDING_FLOOR_TEXTURE`].
pub const BINDING_FLOOR_SAMPLER: u32 = 1;

/// The format the **placeholder** mirror is created with, and nothing else.
///
/// A real pane mirror takes the swapchain's format instead — see
/// [`VolumePipelines::ensure_mirror`] — because that is what decides which
/// fragment entry point `egui_wgpu` uses and hence what encoding lands in it.
/// The placeholder is one transparent texel that is never sampled (the shader's
/// floor arm is dead while `flags.w` is 0), so its format only has to exist.
pub const FLOOR_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

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
    floor_layout: wgpu::BindGroupLayout,
    blit_layout: wgpu::BindGroupLayout,
    quad: wgpu::Buffer,
    grid_sampler: wgpu::Sampler,
    lut_sampler: wgpu::Sampler,
    floor_sampler: wgpu::Sampler,
    blit_sampler: wgpu::Sampler,
    /// What binds at group 1 when no floor is in hand: one transparent texel.
    /// The raymarch's layout is total either way, and the shader's floor arm
    /// stays dead until `flags.w` turns it on.
    empty_floor: PaneMirror,
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
                        // Filterable is the stated reason `Rg16Float` was
                        // chosen: index-to-dBZ is affine, so hardware filtering
                        // within data is exactly linear dBZ interpolation — and
                        // the coverage-premultiplied reconstruction needs the
                        // hardware to take both channels' means under one set
                        // of weights.
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

        let floor_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some(&label("floor.layout")),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: BINDING_FLOOR_TEXTURE,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: BINDING_FLOOR_SAMPLER,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
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
            // `Linear` between levels is what makes the reconstruction LOD a
            // continuous knob: the shader samples at `flags.y`, and at exactly
            // 0 the level-1 weight is exactly zero, so the instrument
            // configuration stays the bit-exact raw field.
            mipmap_filter: wgpu::MipmapFilterMode::Linear,
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
        // `Linear` because the floor is a map being looked at obliquely, and
        // `ClampToEdge` so the last row of ground does not bleed round to the
        // opposite edge of the box.
        let floor_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some(&label("floor.sampler")),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
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
                bind_group_layouts: &[Some(&volume_layout), Some(&floor_layout)],
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

        // Created and never written: WebGPU zero-initialises textures, so the
        // placeholder is one transparent texel with no upload — and the queue
        // this constructor deliberately does not take is not needed for it.
        let empty_floor =
            create_pane_mirror(device, &floor_layout, &floor_sampler, [1, 1], FLOOR_FORMAT);

        Self {
            raymarch,
            blit,
            volume_layout,
            floor_layout,
            blit_layout,
            quad,
            grid_sampler,
            lut_sampler,
            floor_sampler,
            blit_sampler,
            empty_floor,
            blit_entry_point,
        }
    }

    /// A pane mirror sized for this frame, creating or resizing it as needed.
    ///
    /// `format` must have the **same sRGB-ness as the swapchain**, because
    /// `egui_wgpu` picks its fragment entry point from the swapchain's format
    /// once, at `Renderer::new`, and that one pipeline is what draws into this
    /// target. An sRGB swapchain means egui emits linear values and expects
    /// the target to encode them; a non-sRGB one means egui emits gamma values
    /// directly. Handing this a format that disagrees produces a floor that is
    /// merely a little too dark or too light — no validation error, nothing
    /// that fails a test that is not looking for it.
    ///
    /// Returns `true` when the texture was (re)created, which is the caller's
    /// cue that the previous contents are gone and the mirror must be redrawn
    /// before anything samples it.
    pub fn ensure_mirror(
        &self,
        device: &wgpu::Device,
        mirror: &mut Option<PaneMirror>,
        size: [u32; 2],
        format: wgpu::TextureFormat,
    ) -> bool {
        let size = [size[0].max(1), size[1].max(1)];
        if mirror
            .as_ref()
            .is_some_and(|m| m.size == size && m.format == format)
        {
            return false;
        }
        *mirror = Some(create_pane_mirror(
            device,
            &self.floor_layout,
            &self.floor_sampler,
            size,
            format,
        ));
        true
    }

    /// Plant `rgba` in a mirror, straight from the CPU.
    ///
    /// **Nothing in the frame path calls this.** Production *draws* into the
    /// mirror — that is the entire point of the design — and this exists so the
    /// GPU tests can bind a mirror of known colours without standing up an egui
    /// frame, against the very same texture and bind group production uses.
    ///
    /// `rgba` is `size[0] * size[1] * 4` bytes in the mirror's own encoding,
    /// premultiplied, row 0 at the top. Refuses a mismatch rather than letting
    /// wgpu's own validation decide, so the message names the two numbers.
    pub fn write_mirror(&self, queue: &wgpu::Queue, mirror: &PaneMirror, rgba: &[u8]) -> bool {
        let size = mirror.size;
        let expected = (size[0] as usize)
            .checked_mul(size[1] as usize)
            .and_then(|texels| texels.checked_mul(4));
        if expected != Some(rgba.len()) {
            log::error!(
                "3D volume view: refusing to plant a {size:?} mirror from {} bytes",
                rgba.len(),
            );
            return false;
        }
        queue.write_texture(
            mirror.texture.as_image_copy(),
            rgba,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(size[0] * 4),
                rows_per_image: Some(size[1]),
            },
            wgpu::Extent3d {
                width: size[0],
                height: size[1],
                depth_or_array_layers: 1,
            },
        );
        true
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
    /// `indices` is one byte per cell in x-fastest, then y, then z order — the
    /// grid's own plane, with 0 meaning no data. It is widened here into the
    /// [`VOLUME_TEXTURE_FORMAT`] two-channel plane by
    /// [`coverage_premultiplied`]; the host grid stays one byte per cell,
    /// because coverage is exactly `index != 0` and storing it twice would
    /// double the worker payload and the host residency to carry no
    /// information.
    ///
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
            // Two levels: the raw grid, and the hand-built two-cell mean the
            // reconstruction LOD blends towards. wgpu generates no mips; the
            // level is computed on the CPU below, which for the desktop
            // shape's 16 MiB premultiplied plane is a single pass over the
            // bytes at upload time. A grid too
            // small to halve (a 1x1x1 box, which no shape produces but the
            // upload accepts) keeps one level — `create_texture` would refuse
            // two, from a call with no `Result`, and the sampler clamps an
            // out-of-range LOD to the levels that exist.
            mip_level_count: grid_mip_levels(cells),
            sample_count: 1,
            dimension: wgpu::TextureDimension::D3,
            format: VOLUME_TEXTURE_FORMAT,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let premultiplied = coverage_premultiplied(indices);
        queue.write_texture(
            grid.as_image_copy(),
            &premultiplied,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                // No 256-byte row padding: `write_texture` repacks internally to
                // the backend's `buffer_copy_pitch`, which is 4 on GLES. But
                // `rows_per_image` MUST be `Some` when depth exceeds 1, or every
                // slice after the first is copied from the wrong offset.
                bytes_per_row: Some(cells[0] * GRID_BYTES_PER_CELL),
                rows_per_image: Some(cells[1]),
            },
            wgpu::Extent3d {
                width: cells[0],
                height: cells[1],
                depth_or_array_layers: cells[2],
            },
        );
        if grid_mip_levels(cells) > 1 {
            upload_coarse_level(queue, &grid, cells, &premultiplied);
        }

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
            lut_texture,
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
        self.encode_raymarch_with_floor(encoder, target, volume, None);
    }

    /// [`Self::encode_raymarch`], with a floor to stand the volume on.
    ///
    /// `None` binds the one-texel transparent placeholder — the layout is
    /// total either way, and whether the shader *reads* the floor is the
    /// uniform's `flags.w`, written by the bridge only when it also had a
    /// floor to bind.
    pub fn encode_raymarch_with_floor(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        target: &OffscreenTarget,
        volume: &VolumeTextures,
        floor: Option<&PaneMirror>,
    ) {
        self.encode_raymarch_with_timestamps(encoder, target, volume, floor, None);
    }

    /// [`Self::encode_raymarch`], with timestamp queries bracketing the pass.
    ///
    /// The seam `tests/volume_march_cost.rs` measures through. Passing the
    /// writes into the one place the pass is described keeps the measured pass
    /// and the shipped pass the same pass — a bench that re-recorded its own
    /// copy of this descriptor would silently drift from what it claims to
    /// time. Production always hands `None`; `RenderPassTimestampWrites` needs
    /// `Features::TIMESTAMP_QUERY`, which the app never requests.
    pub fn encode_raymarch_with_timestamps(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        target: &OffscreenTarget,
        volume: &VolumeTextures,
        floor: Option<&PaneMirror>,
        timestamp_writes: Option<wgpu::RenderPassTimestampWrites>,
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
            timestamp_writes,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(&self.raymarch);
        pass.set_bind_group(0, &volume.bind_group, &[]);
        pass.set_bind_group(1, &floor.unwrap_or(&self.empty_floor).bind_group, &[]);
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

/// The pane mirror on the GPU: a frame-sized copy of the 2D pane's own render,
/// plus the bind group the raymarch reads it through at group 1.
///
/// One mirror serves every 3D pane. It covers the whole frame rather than any
/// one box footprint, so two 3D panes sourced from two different maps each find
/// their own ground in it simply by sampling a different region — which is why
/// there is no per-pane keying here, and why nothing has to be invalidated when
/// a pane is re-aimed.
pub struct PaneMirror {
    texture: wgpu::Texture,
    /// The colour attachment the mirror pass draws into.
    view: wgpu::TextureView,
    bind_group: wgpu::BindGroup,
    size: [u32; 2],
    format: wgpu::TextureFormat,
}

impl PaneMirror {
    /// The attachment the mirror pass draws into.
    pub fn view(&self) -> &wgpu::TextureView {
        &self.view
    }

    /// The mirror's size in texels.
    pub fn size(&self) -> [u32; 2] {
        self.size
    }

    /// Whether the mirror holds gamma-encoded texels — the value
    /// `VolumeUniform::floor_geo`'s `w` lane carries to the shader.
    pub fn is_gamma_encoded(&self) -> bool {
        mirror_is_gamma_encoded(self.format)
    }

    /// The format this mirror was created with.
    pub fn format(&self) -> wgpu::TextureFormat {
        self.format
    }

    /// The texture itself, for tests that read it back.
    pub fn texture(&self) -> &wgpu::Texture {
        &self.texture
    }
}

/// Whether a mirror in `format` holds **gamma-encoded** texels.
///
/// The inverse of the format's sRGB-ness, and the reasoning is one step
/// removed from anything this module can see: `egui_wgpu` picks its fragment
/// entry point once, at `Renderer::new`, from the **swapchain's** format —
/// `fs_main_gamma_framebuffer` when it is not sRGB, `fs_main_linear_framebuffer`
/// when it is. That one pipeline is what draws the mirror. So an sRGB target
/// receives linear values (the hardware encodes them on write) and a non-sRGB
/// target receives values egui has already gamma-encoded itself.
///
/// A free function rather than a method because both arms have to be pinned and
/// a `PaneMirror` needs a `wgpu::Device` to exist, which CI rows do not have.
///
/// Both arms are live, but they are not equally common.
/// `app_state::preferred_surface_format` prefers a non-sRGB format on wasm, and
/// natively prefers `Bgra8Unorm` — also non-sRGB — falling back to
/// `capabilities.formats[0]` only on an adapter that does not offer it. So the
/// gamma-encoded arm is the ordinary one on both platforms, and the sRGB arm is
/// the rare one, reached on adapters lacking `Bgra8Unorm` (Android/Vulkan
/// notably). Rare is not unreachable, which is why both are pinned — and it is
/// the rare arm that would otherwise ship broken, because nobody sees it.
pub fn mirror_is_gamma_encoded(format: wgpu::TextureFormat) -> bool {
    !format.is_srgb()
}

/// A mirror of `size` texels and its bind group. No upload: WebGPU
/// zero-initialises, so an undrawn mirror is transparent — which reads as "no
/// ground here", exactly what a floor with no pane behind it should be.
fn create_pane_mirror(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    sampler: &wgpu::Sampler,
    size: [u32; 2],
    format: wgpu::TextureFormat,
) -> PaneMirror {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some(&label("pane_mirror")),
        size: wgpu::Extent3d {
            width: size[0].max(1),
            height: size[1].max(1),
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        // `COPY_DST` is not used in production — the frame path *draws* into
        // this target, it never writes bytes to it. It is here so a GPU test
        // can plant a mirror of known colours through
        // [`VolumePipelines::write_mirror`] without standing up a whole egui
        // frame, and so that the texture those tests bind is the same texture
        // production binds rather than a second kind that could drift from it.
        // The flag costs nothing on any backend.
        usage: wgpu::TextureUsages::TEXTURE_BINDING
            | wgpu::TextureUsages::RENDER_ATTACHMENT
            | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some(&label("pane_mirror.bind_group")),
        layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: BINDING_FLOOR_TEXTURE,
                resource: wgpu::BindingResource::TextureView(&view),
            },
            wgpu::BindGroupEntry {
                binding: BINDING_FLOOR_SAMPLER,
                resource: wgpu::BindingResource::Sampler(sampler),
            },
        ],
    });
    PaneMirror {
        texture,
        view,
        bind_group,
        size: [size[0].max(1), size[1].max(1)],
        format,
    }
}

/// A voxel grid and its palette, uploaded, plus the camera buffer.
pub struct VolumeTextures {
    cells: [u32; 3],
    uniform: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
    /// The palette's own texture, kept so the table can be rewritten in place
    /// — the Volume Alpha editor changes 1 KiB of alpha without touching the
    /// 16 MiB grid beside it, and the bind group keeps pointing at this same
    /// texture across the write.
    lut_texture: wgpu::Texture,
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

    /// Replace the colour table in place — the Volume Alpha path, called only
    /// when the effective table actually changed, never per frame.
    ///
    /// The same validation as the upload's: a table that is not exactly
    /// [`VOLUME_LUT_BYTES`] is refused with a log line rather than handed to
    /// `write_texture`, whose size mismatch would be a validation error
    /// raised on a queue with no `Result` to return it through.
    pub fn write_lut(&self, queue: &wgpu::Queue, lut: &[u8]) {
        if lut.len() != VOLUME_LUT_BYTES {
            log::error!(
                "3D volume view: refusing a {}-byte colour table rewrite (expected {})",
                lut.len(),
                VOLUME_LUT_BYTES,
            );
            return;
        }
        queue.write_texture(
            self.lut_texture.as_image_copy(),
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
    }
}

/// Bytes one cell of [`VOLUME_TEXTURE_FORMAT`] occupies: the premultiplied
/// index and the coverage beside it, a half float each.
///
/// Two bytes a channel rather than one is not headroom. See
/// [`VOLUME_TEXTURE_FORMAT`] for why an eight-bit channel makes `R̄ / Ḡ`
/// wrong at an echo edge on any sampler that filters `unorm` in fixed point.
pub const GRID_BYTES_PER_CELL: u32 = 4;

/// Bytes one channel of [`VOLUME_TEXTURE_FORMAT`] occupies.
const GRID_BYTES_PER_CHANNEL: usize = 2;

/// Cells a grid of this shape holds, or `None` if the product overflows —
/// which is also the length of the one-byte-per-cell index plane a caller
/// hands [`VolumePipelines::upload_volume`].
pub fn cell_count(cells: [u32; 3]) -> Option<usize> {
    cells
        .iter()
        .try_fold(1usize, |acc, &n| acc.checked_mul(n as usize))
}

/// Bytes a [`VOLUME_TEXTURE_FORMAT`] grid of this shape occupies at **mip 0**,
/// or `None` if it overflows. See [`grid_bytes_with_mips`] for what the
/// allocation actually costs.
pub fn grid_bytes(cells: [u32; 3]) -> Option<usize> {
    cell_count(cells)?.checked_mul(GRID_BYTES_PER_CELL as usize)
}

/// Bytes the grid texture really costs: every level [`grid_mip_levels`] gives
/// it, at [`GRID_BYTES_PER_CELL`] a cell.
///
/// Separate from [`grid_bytes`] because the two answer different questions —
/// `grid_bytes` sizes the upload buffer for one level, this sizes the
/// allocation — and because the memory budget in `constants` is a claim about
/// the allocation. Before the coverage channel landed the budget quietly
/// counted mip 0 alone and the coarse level rode in the headroom; now both are
/// named.
pub fn grid_bytes_with_mips(cells: [u32; 3]) -> Option<usize> {
    let mut total = grid_bytes(cells)?;
    if grid_mip_levels(cells) > 1 {
        total = total.checked_add(grid_bytes(coarse_cells(cells))?)?;
    }
    Some(total)
}

/// Mip levels the grid texture carries: the raw field, and one hand-built
/// two-cell mean below it for the reconstruction LOD to blend towards.
pub const GRID_MIP_LEVELS: u32 = 2;

/// Mip levels a grid of this shape can actually carry: [`GRID_MIP_LEVELS`]
/// unless the grid is too small to halve on every axis at once — a 1x1x1
/// grid, which no shape rung produces but the upload accepts, and for which
/// `create_texture` would refuse a second level from a call with no `Result`.
fn grid_mip_levels(cells: [u32; 3]) -> u32 {
    if cells.iter().copied().max().unwrap_or(0) >= 2 {
        GRID_MIP_LEVELS
    } else {
        1
    }
}

/// wgpu's own mip arithmetic: `max(n / 2, 1)` per axis.
fn coarse_cells(cells: [u32; 3]) -> [u32; 3] {
    cells.map(|n| (n / 2).max(1))
}

/// The grid's own index plane widened into [`VOLUME_TEXTURE_FORMAT`]:
/// `R = coverage × index`, `G = coverage`, coverage being 1 exactly where the
/// index is not `rustdar_radar::voxel::NO_DATA_INDEX`.
///
/// Premultiplication is trivial here because coverage is binary and index 0 is
/// unreachable for a measurement (`ramp_index` clamps every finite value to
/// `1..=255`), so `coverage × index` **is** the index and the whole function is
/// "the byte, then 255 or 0". Written out anyway rather than folded into the
/// caller: it is the one place the texture's contract is expressed, the mip
/// below reads the same layout, and `the_premultiplied_plane_is_index_and_a_
/// binary_coverage` pins it.
///
/// One pass over the plane, at grid-upload time — once per built volume, not
/// per frame. On the desktop shape that is 8 MiB read and 32 MiB written in the
/// same `prepare` that already walks the same bytes to build the coarse level.
fn coverage_premultiplied(indices: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(indices.len() * GRID_BYTES_PER_CELL as usize);
    for &index in indices {
        let covered = index != rustdar_radar::voxel::NO_DATA_INDEX;
        // `index` is already `coverage x index` in byte units: coverage is
        // binary and the only index it zeroes is 0 itself.
        push_channel(&mut out, f32::from(index) / 255.0);
        push_channel(&mut out, if covered { 1.0 } else { 0.0 });
    }
    out
}

/// Append one [`VOLUME_TEXTURE_FORMAT`] channel to a texel plane.
///
/// Little endian, which is what `write_texture` wants on every target this
/// builds for — WebGPU's texel byte order is the format's own, and no
/// big-endian target is in the matrix.
fn push_channel(out: &mut Vec<u8>, value: f32) {
    out.extend_from_slice(&half::f16::from_f32(value).to_le_bytes());
}

/// Read one [`VOLUME_TEXTURE_FORMAT`] channel back out of a texel plane.
fn read_channel(plane: &[u8], at: usize) -> f32 {
    let bytes = [plane[at], plane[at + 1]];
    half::f16::from_le_bytes(bytes).to_f32()
}

/// Write the hand-built coarse level into the grid texture's mip 1.
///
/// `premultiplied` is the level-0 plane [`coverage_premultiplied`] produced,
/// not the grid's own indices.
fn upload_coarse_level(
    queue: &wgpu::Queue,
    grid: &wgpu::Texture,
    cells: [u32; 3],
    premultiplied: &[u8],
) {
    let (coarse_cells, coarse) = downsampled_grid(cells, premultiplied);
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: grid,
            mip_level: 1,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        &coarse,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(coarse_cells[0] * GRID_BYTES_PER_CELL),
            rows_per_image: Some(coarse_cells[1]),
        },
        wgpu::Extent3d {
            width: coarse_cells[0],
            height: coarse_cells[1],
            depth_or_array_layers: coarse_cells[2],
        },
    );
}

/// The grid's mip level 1: **the plain box mean of both channels**, over all
/// eight fine cells under each coarse one, no special case anywhere.
///
/// # Why the special case is gone, rather than merely moved
///
/// This used to exclude the no-data index from the mean by hand, and it had to
/// — a naive mean folded "not measured" in as the bottom of the dBZ ramp, and
/// on the KCRP 2017-08-26 (Harvey) volume at the default 460 km box (1.8 km
/// cells) that erased the eyewall: the ≥50 dBZ classes lost 41% of their pixels
/// and the ≥30 dBZ classes 81%, with the 2D pane showing a red core the 3D pane
/// had painted away.
///
/// Coverage premultiplication makes the occupancy weighting **fall out of the
/// arithmetic**. Write the fine cells as `(c_i · x_i, c_i)`. The box mean of
/// the two channels is `(Σ c_i x_i / 8, Σ c_i / 8)`, and the shader's
/// reconstruction divides one by the other: `R̄ / Ḡ = Σ c_i x_i / Σ c_i` — the
/// occupancy mean the hand-written version computed, with no branch — and the
/// coarse texel *additionally* carries `Σ c_i / 8`, the block's real occupancy,
/// which the old one-channel level had no room for and therefore threw away.
/// So the coarse level is now strictly more informative than the level it
/// replaces, and the code is a mean.
///
/// The mean is taken in `f32` and stored back as the format's half float.
/// Averaging indices is exact averaging of the physical value because
/// index↔value is affine — the same fact that justified the format's linear
/// filtering in the first place.
///
/// # The identity is exact in ℝ and quantised in binary16
///
/// It is not *exactly* the occupancy mean once stored, because both channels
/// round to the texel format before the shader divides. What the shader
/// reconstructs is `half(Σ c x / 8) / half(Σ c / 8)`, not `Σ c x / Σ c`.
///
/// Both roundings are **relative** — half an ulp of the value itself, i.e.
/// 2⁻¹¹ — so the quotient's error is bounded by 2⁻¹⁰ of full scale whatever
/// the block's occupancy, which is **a quarter of one index unit**. The
/// convex-hull invariant `field_at` states therefore survives this level to
/// that tolerance, and the sparse blocks are no worse than the dense ones.
///
/// This is the same property that made [`VOLUME_TEXTURE_FORMAT`] a float
/// format, arriving here for the same reason. Under the `Rg8Unorm` this
/// replaced the divisor was not `n` but `round₈(255 n)`, stepping in units of
/// 255/8 ≈ 31.9, and the bound was **4 index units** — worst at `n = 1,
/// x = 4`, which stored `(1, 32)` and reconstructed to 7.97 — with the hull
/// broken outright on sparse blocks: a single measured cell at index 1, 2 or 3
/// rounded `Σ x` to `R̄ = 0` while `Ḡ = 32`, so the block reconstructed to
/// index **0**, outside the hull of the one value it held, at a coverage the
/// lit volume does sample.
/// `the_grid_mip_is_the_mean_of_each_coarse_blocks_measured_cells` pins the
/// bound.
///
/// The stated semantics, precisely: a fetch at an LOD between the levels
/// interpolates the raw field with this one, so the reconstruction is the
/// affine mean of the cells that were measured — to the tolerance above —
/// dilated by at most the two-cell kernel into cells that were not, with the
/// dilation's alpha now scaled by the coarse occupancy rather than left to the
/// palette. Presentation over untouched data, like every knob on this rung:
/// level 0 is bit-exact, and LOD 0 — the instrument default — never reads this
/// level at all.
///
/// Odd extents follow wgpu's own mip arithmetic ([`coarse_cells`]), and the
/// fine cells under a coarse one are whatever the halved coordinate maps back
/// onto, clamped to the fine extent, so no fine cell is read out of bounds and
/// every coarse cell averages only cells that exist. A clamped extent means the
/// same fine cell is counted twice, in both channels alike, which leaves the
/// ratio — and so the reconstructed index — unchanged.
fn downsampled_grid(cells: [u32; 3], premultiplied: &[u8]) -> ([u32; 3], Vec<u8>) {
    let coarse = coarse_cells(cells);
    let fine = cells.map(|n| n as usize);
    let stride = GRID_BYTES_PER_CELL as usize;
    let mut out = Vec::with_capacity((coarse[0] * coarse[1] * coarse[2]) as usize * stride);
    for cz in 0..coarse[2] as usize {
        for cy in 0..coarse[1] as usize {
            for cx in 0..coarse[0] as usize {
                let mut sum = [0.0f32; 2];
                for dz in 0..2 {
                    for dy in 0..2 {
                        for dx in 0..2 {
                            let fx = (cx * 2 + dx).min(fine[0] - 1);
                            let fy = (cy * 2 + dy).min(fine[1] - 1);
                            let fz = (cz * 2 + dz).min(fine[2] - 1);
                            let at = ((fz * fine[1] + fy) * fine[0] + fx) * stride;
                            for (channel, total) in sum.iter_mut().enumerate() {
                                *total += read_channel(
                                    premultiplied,
                                    at + channel * GRID_BYTES_PER_CHANNEL,
                                );
                            }
                        }
                    }
                }
                for total in sum {
                    push_channel(&mut out, total / 8.0);
                }
            }
        }
    }
    (coarse, out)
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
    // Against the **cell count**, not [`grid_bytes`]: the caller hands over the
    // grid's own one-byte-per-cell index plane, and the second channel is
    // synthesised here. Comparing against the texture's two-byte figure would
    // refuse every correct grid.
    //
    // `?` here would be exactly backwards: a cell count that overflows `usize`
    // is the strongest reason to refuse, and returning `None` for it would
    // report the grid as acceptable.
    let Some(expected) = cell_count(cells) else {
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

#[path = "volume_raymarch/tests.rs"]
#[cfg(test)]
mod tests;
