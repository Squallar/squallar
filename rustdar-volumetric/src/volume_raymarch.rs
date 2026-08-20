//! The offscreen raymarch pipeline, and the quad that composites it into egui.

use egui_wgpu::wgpu;

use crate::VOLUME_TEXTURE_FORMAT;
use crate::blue_noise::{BLUE_NOISE_EDGE, blue_noise_tile};
use crate::uniform::{VOLUME_UNIFORM_BYTES, VolumeUniform};
use rustdar_device_profile::constants::{VOLUME_LUT_BYTES, VOLUME_TEXTURE_BUDGET_BYTES};
use rustdar_gpu::egui_renderer::AttachmentConfig;
use staging::VolumeStaging;

/// The WGSL every volume pipeline is built from.
pub const VOLUME_SHADER_WGSL: &str = include_str!("volume.wgsl");

/// Label prefix every wgpu resource here must carry.
pub const LABEL_PREFIX: &str = "rustdar.volume";

/// The march's per-ray sample ceiling, restated for hosts that mirror the
/// shader's arithmetic (the silhouette harness casts the same rays in Rust).
pub const RAYMARCH_STEP_CEILING: i32 = 1024;

/// Cells one march step advances along the ray, in the grid's own cell metric,
/// **at the instrument default**: the value `VolumeUniform::new` writes into
/// the step lane, which is what the silhouette harness's mirror marches at.
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
pub const ENTRY_POINTS: [(&str, ShaderStage); 5] = [
    (ENTRY_VS_RAYMARCH, ShaderStage::Vertex),
    (ENTRY_FS_RAYMARCH, ShaderStage::Fragment),
    (ENTRY_VS_BLIT, ShaderStage::Vertex),
    (ENTRY_FS_BLIT_GAMMA, ShaderStage::Fragment),
    (ENTRY_FS_BLIT_LINEAR, ShaderStage::Fragment),
];

/// Which half of the pipeline an entry point belongs to.
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
pub const BINDING_BLIT_TEXTURE: u32 = 5;
/// See [`BINDING_BLIT_TEXTURE`].
pub const BINDING_BLIT_SAMPLER: u32 = 6;

/// The march's blue noise tile, back in the **raymarch's** group 0.
pub const BINDING_JITTER_TEXTURE: u32 = 7;

/// The format the blue noise tile is uploaded as: one byte a texel.
pub const JITTER_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::R8Unorm;

/// The map floor's bindings, in **group 1** of the raymarch pipeline.
pub const BINDING_FLOOR_TEXTURE: u32 = 0;
/// See [`BINDING_FLOOR_TEXTURE`].
pub const BINDING_FLOOR_SAMPLER: u32 = 1;

/// The format the **placeholder** mirror is created with, and nothing else.
pub const FLOOR_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

/// The format the raymarch renders into.
pub const OFFSCREEN_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

/// The format the colour table is uploaded as.
pub const LUT_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

/// egui's blend state, which the compositing quad has to match exactly.
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
pub const QUAD_BYTES: usize = QUAD_VERTEX_COUNT as usize * 2 * 4;

/// Clip-space corners of the fullscreen quad, in draw order.
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
    pub fn new(device: &wgpu::Device, egui_attachments: AttachmentConfig) -> Self {
        Self::from_shader_source(device, egui_attachments, VOLUME_SHADER_WGSL)
    }

    /// [`VolumePipelines::new`], over WGSL handed in rather than
    /// [`VOLUME_SHADER_WGSL`].
    pub fn from_shader_source(
        device: &wgpu::Device,
        egui_attachments: AttachmentConfig,
        wgsl: &str,
    ) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some(&label("shader")),
            source: wgpu::ShaderSource::Wgsl(wgsl.into()),
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
                wgpu::BindGroupLayoutEntry {
                    binding: BINDING_JITTER_TEXTURE,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        // Non-filterable, and no sampler entry follows: the
                        // shader reaches this one with `textureLoad`.
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
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
        // between two dBZ levels.
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
            // raymarch in the way.
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
    pub fn upload_volume(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        cells: [u32; 3],
        indices: &[u8],
        lut: &[u8],
        staging: &mut VolumeStaging,
    ) -> Option<VolumeTextures> {
        self.upload_volume_at(
            device,
            queue,
            cells,
            indices,
            lut,
            CoarseLevel::Built,
            staging,
        )
    }

    /// [`Self::upload_volume`], told whether this device will ever sample the
    /// coarse level.
    #[allow(clippy::too_many_arguments)]
    pub fn upload_volume_at(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        cells: [u32; 3],
        indices: &[u8],
        lut: &[u8],
        coarse: CoarseLevel,
        staging: &mut VolumeStaging,
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
            // Two levels when this device will read the second one: the raw
            // grid, and the hand-built two-cell mean the reconstruction LOD
            // blends towards.
            mip_level_count: grid_mip_levels(cells, coarse),
            sample_count: 1,
            dimension: wgpu::TextureDimension::D3,
            format: VOLUME_TEXTURE_FORMAT,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        // The ring first, and the call below only when it declined.
        if !staging.write_plane(device, queue, &grid, cells, indices) {
            let premultiplied = coverage_premultiplied_into(staging.widening(), indices);
            queue.write_texture(
                grid.as_image_copy(),
                premultiplied,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    // No 256-byte row padding: `write_texture` repacks
                    // internally to the backend's `buffer_copy_pitch`, which is
                    // 4 on GLES.
                    bytes_per_row: Some(cells[0] * GRID_BYTES_PER_CELL),
                    rows_per_image: Some(cells[1]),
                },
                wgpu::Extent3d {
                    width: cells[0],
                    height: cells[1],
                    depth_or_array_layers: cells[2],
                },
            );
        }
        if grid_mip_levels(cells, coarse) > 1 {
            upload_coarse_level(queue, &grid, cells, indices);
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

        // The march's stratification tile.
        let jitter_texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some(&label("jitter")),
            size: wgpu::Extent3d {
                width: BLUE_NOISE_EDGE,
                height: BLUE_NOISE_EDGE,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: JITTER_FORMAT,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        queue.write_texture(
            jitter_texture.as_image_copy(),
            blue_noise_tile(),
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(BLUE_NOISE_EDGE),
                rows_per_image: Some(BLUE_NOISE_EDGE),
            },
            wgpu::Extent3d {
                width: BLUE_NOISE_EDGE,
                height: BLUE_NOISE_EDGE,
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
        let jitter_view = jitter_texture.create_view(&wgpu::TextureViewDescriptor::default());
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
                wgpu::BindGroupEntry {
                    binding: BINDING_JITTER_TEXTURE,
                    resource: wgpu::BindingResource::TextureView(&jitter_view),
                },
            ],
        });

        Some(VolumeTextures {
            cells,
            // The levels this descriptor actually asked for, not the levels a
            // grid of this shape may have — see `grid_bytes_at`.
            bytes: resident_grid_bytes_at(cells, coarse).unwrap_or(0),
            uniform,
            bind_group,
            lut_texture,
        })
    }

    /// Record the raymarch into `target`.
    pub fn encode_raymarch(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        target: &OffscreenTarget,
        volume: &VolumeTextures,
    ) {
        self.encode_raymarch_with_floor(encoder, target, volume, None);
    }

    /// [`Self::encode_raymarch`], with a floor to stand the volume on.
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
        // this target, it never writes bytes to it.
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
    /// GPU bytes the two textures below occupy, recorded at upload because
    /// this is the one moment the coarse decision is in hand: `CoarseLevel` is
    /// consumed by the descriptor and nothing on the handle can be asked
    /// afterwards whether the level was allocated. See [`Self::texture_bytes`].
    bytes: usize,
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

    /// GPU texture bytes this upload occupies: the grid as it was laid out,
    /// plus its colour table and its jitter tile.
    pub fn texture_bytes(&self) -> usize {
        self.bytes
    }

    /// Point the raymarch's camera somewhere.
    pub fn write_uniform(&self, queue: &wgpu::Queue, uniform: &VolumeUniform) {
        queue.write_buffer(&self.uniform, 0, &uniform.to_bytes());
    }

    /// Replace the colour table in place — the Volume Alpha path, called only
    /// when the effective table actually changed, never per frame.
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
/// packed, or `None` if it overflows.
pub fn grid_bytes(cells: [u32; 3]) -> Option<usize> {
    cell_count(cells)?.checked_mul(GRID_BYTES_PER_CELL as usize)
}

/// Texels a 3D texture's **width** is rounded up to.
const TEXTURE_TILE_TEXELS_X: usize = 16;
/// Rows a 3D texture's **height** is rounded up to. See [`TEXTURE_TILE_TEXELS_X`].
const TEXTURE_TILE_ROWS_Y: usize = 8;
/// Layers a 3D texture's **depth** is rounded up to, above the point where the
/// rounding stops being to a power of two. See [`TEXTURE_TILE_TEXELS_X`].
const TEXTURE_TILE_LAYERS_Z: usize = 16;

/// Slack allowed on every texture allocation over the tile arithmetic above.
const TEXTURE_ALLOCATION_SLACK_BYTES: usize = 4096;

/// One axis of one mip level, rounded up to the tile the backend lays it out in.
fn tile_up(n: usize, tile: usize) -> usize {
    n.div_ceil(tile).saturating_mul(tile)
}

/// The depth rule, which is not a plain multiple. See [`TEXTURE_TILE_TEXELS_X`].
fn tile_up_layers(n: usize) -> usize {
    if n <= TEXTURE_TILE_LAYERS_Z {
        n.max(1).next_power_of_two()
    } else {
        tile_up(n, TEXTURE_TILE_LAYERS_Z)
    }
}

/// Bytes one mip level of a texture of this shape reserves, tiles included.
fn level_bytes(cells: [u32; 3], level: u32, bytes_per_texel: usize) -> Option<usize> {
    let extent = |axis: usize| (cells[axis] >> level).max(1) as usize;
    tile_up(extent(0), TEXTURE_TILE_TEXELS_X)
        .checked_mul(tile_up(extent(1), TEXTURE_TILE_ROWS_Y))?
        .checked_mul(tile_up_layers(extent(2)))?
        .checked_mul(bytes_per_texel)
}

/// Mip levels a texture of this shape has all the way down to 1×1×1.
fn full_mip_levels(cells: [u32; 3]) -> u32 {
    u32::BITS
        - cells
            .iter()
            .copied()
            .max()
            .unwrap_or(1)
            .max(1)
            .leading_zeros()
}

/// Bytes a texture of this shape reserves on the device: every level it will be
/// laid out with, each rounded up to the backend's tiles, plus
/// [`TEXTURE_ALLOCATION_SLACK_BYTES`]. `None` if it overflows.
fn texture_allocation_bytes(
    cells: [u32; 3],
    bytes_per_texel: usize,
    asks_for_mips: bool,
) -> Option<usize> {
    let levels = if asks_for_mips {
        full_mip_levels(cells)
    } else {
        1
    };
    (0..levels)
        .try_fold(0usize, |acc, level| {
            acc.checked_add(level_bytes(cells, level, bytes_per_texel)?)
        })?
        .checked_add(TEXTURE_ALLOCATION_SLACK_BYTES)
}

/// Bytes the grid texture costs **at its worst**: laid out with every level a
/// grid of this shape can have, at [`GRID_BYTES_PER_CELL`] a cell.
pub fn grid_bytes_with_mips(cells: [u32; 3]) -> Option<usize> {
    grid_bytes_at(cells, CoarseLevel::Built)
}

/// [`grid_bytes_with_mips`], for an upload that has already made the coarse
/// decision — what a *resident* texture of this shape actually occupies.
pub fn grid_bytes_at(cells: [u32; 3], coarse: CoarseLevel) -> Option<usize> {
    texture_allocation_bytes(
        cells,
        GRID_BYTES_PER_CELL as usize,
        grid_mip_levels(cells, coarse) > 1,
    )
}

/// Bytes the colour table's own texture reserves beside the grid.
pub fn lut_allocation_bytes() -> usize {
    texture_allocation_bytes([lut_texel_count(), 1, 1], LUT_BYTES_PER_TEXEL, false)
        .unwrap_or(VOLUME_LUT_BYTES)
}

/// Bytes the march's stratification tile reserves.
pub fn jitter_allocation_bytes() -> usize {
    texture_allocation_bytes(
        [BLUE_NOISE_EDGE, BLUE_NOISE_EDGE, 1],
        JITTER_BYTES_PER_TEXEL,
        false,
    )
    .unwrap_or(0)
}

/// Bytes one texel of [`LUT_FORMAT`] occupies.
const LUT_BYTES_PER_TEXEL: usize = 4;
/// Bytes one texel of [`JITTER_FORMAT`] occupies.
const JITTER_BYTES_PER_TEXEL: usize = 1;

/// **Everything one resident grid costs the device**: the grid texture as it is
/// laid out, its colour table's texture, and the jitter tile created beside it.
pub fn resident_grid_bytes(cells: [u32; 3]) -> Option<usize> {
    resident_grid_bytes_at(cells, CoarseLevel::Built)
}

/// [`resident_grid_bytes`], for an upload whose coarse decision is already
/// made. See [`grid_bytes_at`] for which question is which.
pub fn resident_grid_bytes_at(cells: [u32; 3], coarse: CoarseLevel) -> Option<usize> {
    grid_bytes_at(cells, coarse)?
        .checked_add(lut_allocation_bytes())?
        .checked_add(jitter_allocation_bytes())
}

/// Whether an upload gives the grid texture its coarse mip level at all.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CoarseLevel {
    /// Build it and upload it.
    Built,
    /// Leave it out of the descriptor. Not merely unwritten — unallocated,
    /// which is the whole point.
    Omitted,
}

/// Mip levels the grid texture carries: the raw field, and one hand-built
/// two-cell mean below it for the reconstruction LOD to blend towards.
pub const GRID_MIP_LEVELS: u32 = 2;

/// Mip levels a grid of this shape actually gets: [`GRID_MIP_LEVELS`] unless
/// the caller has said nothing will sample the second one, or the grid is too
/// small to halve on every axis at once — a 1x1x1 grid, which no shape rung
/// produces but the upload accepts, and for which `create_texture` would refuse
/// a second level from a call with no `Result`.
fn grid_mip_levels(cells: [u32; 3], coarse: CoarseLevel) -> u32 {
    if coarse == CoarseLevel::Built && cells.iter().copied().max().unwrap_or(0) >= 2 {
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
fn coverage_premultiplied_into<'a>(out: &'a mut Vec<u8>, indices: &[u8]) -> &'a [u8] {
    let texels = coverage_texels();
    let stride = GRID_BYTES_PER_CELL as usize;
    let plane_bytes = indices.len() * stride;
    // Grow only. Shrinking would give the pages back and buy them again on the
    // next larger grid, which is the whole cost being removed here.
    if out.len() < plane_bytes {
        out.resize(plane_bytes, 0);
    }
    for (texel, &index) in out[..plane_bytes].chunks_exact_mut(stride).zip(indices) {
        texel.copy_from_slice(&texels[index as usize]);
    }
    &out[..plane_bytes]
}

/// Every [`VOLUME_TEXTURE_FORMAT`] texel a grid byte can widen into, indexed by
/// that byte: `R = coverage × index`, `G = coverage`, little endian, exactly as
/// [`channel_bytes`] writes them.
fn coverage_texels() -> &'static [[u8; GRID_BYTES_PER_CELL as usize]; 256] {
    static TEXELS: std::sync::LazyLock<[[u8; GRID_BYTES_PER_CELL as usize]; 256]> =
        std::sync::LazyLock::new(|| {
            std::array::from_fn(|index| {
                let index = index as u8;
                let covered = index != rustdar_radar::voxel::NO_DATA_INDEX;
                // `index` is already `coverage x index` in byte units: coverage
                // is binary and the only index it zeroes is 0 itself.
                let red = channel_bytes(f32::from(index) / 255.0);
                let green = channel_bytes(if covered { 1.0 } else { 0.0 });
                [red[0], red[1], green[0], green[1]]
            })
        });
    &TEXELS
}

/// One [`VOLUME_TEXTURE_FORMAT`] channel, as the bytes a texel plane holds it
/// in.
fn channel_bytes(value: f32) -> [u8; GRID_BYTES_PER_CHANNEL] {
    half::f16::from_f32(value).to_le_bytes()
}

/// Read one [`VOLUME_TEXTURE_FORMAT`] channel back out of a texel plane.
#[cfg(test)]
fn read_channel(plane: &[u8], at: usize) -> f32 {
    let bytes = [plane[at], plane[at + 1]];
    half::f16::from_le_bytes(bytes).to_f32()
}

/// Write the hand-built coarse level into the grid texture's mip 1.
fn upload_coarse_level(queue: &wgpu::Queue, grid: &wgpu::Texture, cells: [u32; 3], indices: &[u8]) {
    let (coarse_cells, coarse) = downsampled_grid(cells, indices);
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
fn downsampled_grid(cells: [u32; 3], indices: &[u8]) -> ([u32; 3], Vec<u8>) {
    let coarse = coarse_cells(cells);
    let fine = cells.map(|n| n as usize);
    let stride = GRID_BYTES_PER_CELL as usize;
    let mut out = Vec::with_capacity((coarse[0] * coarse[1] * coarse[2]) as usize * stride);
    for cz in 0..coarse[2] as usize {
        for cy in 0..coarse[1] as usize {
            for cx in 0..coarse[0] as usize {
                // `Σ c x` and `Σ c`, both exact: the first is at most 8 x 255
                // and the second at most 8, so nothing has rounded yet when the
                // division below rounds once.
                let mut premultiplied = 0u32;
                let mut covered = 0u32;
                for dz in 0..2 {
                    for dy in 0..2 {
                        for dx in 0..2 {
                            let fx = (cx * 2 + dx).min(fine[0] - 1);
                            let fy = (cy * 2 + dy).min(fine[1] - 1);
                            let fz = (cz * 2 + dz).min(fine[2] - 1);
                            let index = indices[(fz * fine[1] + fy) * fine[0] + fx];
                            // Both channels off the one byte: coverage is
                            // binary and the only index it zeroes is 0 itself,
                            // so `c x` is the index and `c` is "not no-data".
                            premultiplied += u32::from(index);
                            covered += u32::from(index != rustdar_radar::voxel::NO_DATA_INDEX);
                        }
                    }
                }
                // Full scale is index 255, which is what puts the 255 in the
                // divisor: the texel's channels are 0-1, not 0-255.
                out.extend_from_slice(&channel_bytes(premultiplied as f32 / (255.0 * 8.0)));
                out.extend_from_slice(&channel_bytes(covered as f32 / 8.0));
            }
        }
    }
    (coarse, out)
}

/// Entries in the colour table, which is also its texture's width.
pub fn lut_texel_count() -> u32 {
    (VOLUME_LUT_BYTES / 4) as u32
}

/// The extent an offscreen is really created at.
pub fn offscreen_extent(size: [u32; 2]) -> [u32; 2] {
    [size[0].max(1), size[1].max(1)]
}

/// Whether a held offscreen has to be thrown away for a new size.
fn offscreen_needs_rebuild(held: Option<[u32; 2]>, wanted: [u32; 2]) -> bool {
    held != Some(wanted)
}

/// Why an upload must be refused, or `None` when the shapes agree.
fn upload_refusal(cells: [u32; 3], indices_len: usize, lut_len: usize) -> Option<String> {
    // Against the **cell count**, not [`grid_bytes`]: the caller hands over the
    // grid's own one-byte-per-cell index plane, and the second channel is
    // synthesised here.
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
const _: () = assert!(VOLUME_TEXTURE_BUDGET_BYTES > VOLUME_LUT_BYTES);

#[path = "volume_raymarch/staging.rs"]
pub mod staging;

#[path = "volume_raymarch/tests.rs"]
#[cfg(test)]
mod tests;
