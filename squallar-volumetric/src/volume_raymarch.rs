//! The offscreen raymarch pipeline, and the quad that composites it into egui.

use egui_wgpu::wgpu;

use crate::VOLUME_TEXTURE_FORMAT;
use crate::blue_noise::{BLUE_NOISE_EDGE, blue_noise_tile};
use crate::uniform::{VOLUME_UNIFORM_BYTES, VolumeUniform};
use squallar_device_profile::constants::{VOLUME_LUT_BYTES, VOLUME_TEXTURE_BUDGET_BYTES};
use squallar_device_profile::quality::{FittedOffscreen, GroundPass, ResolutionRung};
use squallar_gpu::egui_renderer::AttachmentConfig;
use staging::VolumeStaging;

/// The WGSL every volume pipeline is built from.
pub const VOLUME_SHADER_WGSL: &str = include_str!("volume.wgsl");

/// Label prefix every wgpu resource here must carry.
pub const LABEL_PREFIX: &str = "squallar.volume";

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
/// Vertex entry point of the ground pass: the procedural grid.
pub const ENTRY_VS_GROUND: &str = "vs_ground";
/// Fragment entry point of the ground pass: the occluder and the drape.
pub const ENTRY_FS_GROUND: &str = "fs_ground";
/// Vertex entry point of the building prisms: the mesh, lifted onto the ground.
pub const ENTRY_VS_BUILDING: &str = "vs_building";
/// Fragment entry point of the building prisms: the same occluder, one albedo.
pub const ENTRY_FS_BUILDING: &str = "fs_building";
/// Vertex entry point of the compositing quad.
pub const ENTRY_VS_BLIT: &str = "vs_blit";
/// Fragment entry point of the quad on a **non-sRGB** target: pass-through.
pub const ENTRY_FS_BLIT_GAMMA: &str = "fs_blit_gamma_framebuffer";
/// Fragment entry point of the quad on an **sRGB** target: decode to linear.
pub const ENTRY_FS_BLIT_LINEAR: &str = "fs_blit_linear_framebuffer";

/// Every entry point in [`VOLUME_SHADER_WGSL`], with the stage it belongs to.
///
/// This list is what pulls a stage into the GLSL ES 300 gate: an entry point it
/// omits is never translated by `volume_shader.rs` and reaches a WebGL2 browser
/// having been checked by nothing.
pub const ENTRY_POINTS: [(&str, ShaderStage); 9] = [
    (ENTRY_VS_RAYMARCH, ShaderStage::Vertex),
    (ENTRY_FS_RAYMARCH, ShaderStage::Fragment),
    (ENTRY_VS_GROUND, ShaderStage::Vertex),
    (ENTRY_FS_GROUND, ShaderStage::Fragment),
    (ENTRY_VS_BUILDING, ShaderStage::Vertex),
    (ENTRY_FS_BUILDING, ShaderStage::Fragment),
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

/// The ground pass's outputs, read back by the raymarch in **group 2**.
///
/// Group 2 and not group 0: group 0 is per-*grid* and these two are
/// per-*offscreen*, recreated with it. Binding them at group 0 would tie a
/// per-target texture's lifetime to a per-grid bind group and desynchronise
/// the two. Group 2 also means the existing binding-map slot assertions do not
/// move — the occluder takes texture slot 4, after the floor's 3.
pub const BINDING_OCCLUDER_TEXTURE: u32 = 0;
/// See [`BINDING_OCCLUDER_TEXTURE`].
pub const BINDING_GROUND_TEXTURE: u32 = 1;

/// The format the raymarch renders into.
pub const OFFSCREEN_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

/// The format the occluder is written and read in: the packed ray parameter
/// across RGB, the hit flag in A.
///
/// **Not a depth texture, and that is the constraint this whole design is shaped
/// by.** naga hard-errors on `textureLoad` from depth; `textureSampleLevel` on
/// one emits a `sampler2DShadow` `textureLod` overload that does not exist in
/// GLSL ES 3.00 and fails silently at driver compile; `READ_ONLY_DEPTH_STENCIL`
/// is never set by the GLES adapter; and depth-to-buffer copies are
/// unsupported. `no_entry_point_emits_a_shadow_sampler` is the tripwire that
/// stops the depth route being reintroduced.
pub const OCCLUDER_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

/// The format the ground pass's colour target is written and read in.
///
/// A second colour attachment is not optional: the raymarch pass *clears* the
/// offscreen and its pipeline declares `blend: None`, so anything the ground
/// wrote there would be destroyed before the march ran. MRT is available at the
/// floor — `max_color_attachments: 4` and
/// `max_color_attachment_bytes_per_sample: 32` in `downlevel_defaults()`,
/// inherited by `downlevel_webgl2_defaults()`; two `Rgba8Unorm` targets is 8
/// bytes. `INDEPENDENT_BLEND` is conditional on GLES so both share one blend
/// state, which costs nothing here because the ground pass does not blend.
pub const GROUND_COLOUR_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

/// The ground pass's own depth attachment. It never leaves this crate:
/// `AttachmentConfig::depth_format` stays `None` and egui's own pass carries no
/// depth, which two pin tests scrape for.
pub const GROUND_DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;

/// The height field's binding, in **group 3** of the ground pipeline.
///
/// Group 3 and not group 1: the mirror at group 1 is frame-wide and shared by
/// every pane, while a height field belongs to one pane's own drawn box, so
/// binding them together would tie a per-pane texture's lifetime to a
/// frame-wide bind group. Group 2 is impossible — the ground pass *writes*
/// those two as attachments. Four groups is exactly what
/// `downlevel_webgl2_defaults()` guarantees.
pub const BINDING_HEIGHT_TEXTURE: u32 = 0;

/// The format a height field is uploaded as: one `u16` per post, decoded by
/// `z = raw * height_scale + height_offset`.
///
/// **`R16Uint` is `textureLoad`-only by construction** — WGSL has no sampler
/// for an integer texture — which is exactly what is wanted. One texel is one
/// post, the **1:1 post-to-texel invariant** cannot be lost to a filter, and
/// the WebGL2 float-filterability question never arises. Two bytes a post is
/// also the field's own transport encoding
/// (`squallar_elevation::HeightField::samples`), so nothing is re-quantised
/// between the resampler and the GPU.
pub const HEIGHT_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::R16Uint;

/// The most posts an axis of a height field may carry.
///
/// **Derived from the draw, not chosen.** The vertex count is
/// `6 * (px + 1) * (py + 1)` and `RenderPass::draw` takes a `u32`, so a square
/// field past 26754 posts a side cannot be drawn at all. 8192 is the same
/// ceiling `squallar_elevation::jobs` refuses a request at — a post every 112 m
/// over a 920 km box — and it leaves the vertex count at 402 million, three
/// times inside the `u32`. B3 shipped this check missing: `upload_heights`
/// admitted anything under `max_texture_dimension_2d`, which is 32768 on a real
/// driver, and `6 * 32769^2` overflows.
pub const MAX_POSTS_PER_AXIS: u32 = 8192;

/// Vertices the ground draw issues for a field of `posts` posts a side: two
/// triangles per cell of a `(px + 1) x (py + 1)` grid, from
/// `@builtin(vertex_index)` alone.
///
/// **One cell more than there are posts on each axis**, because the grid
/// carries an apron ring: the rim posts duplicated at the drawn box's own edge.
/// See the shader's `box_axis` for why that is a nearest extrapolation rather
/// than a stretch, and for why the mesh has to cover the whole box at all.
///
/// **Derived from the field's own dimensions, never from a constant.** B1 had a
/// `GROUND_POSTS` here and a twin in the shader; the pair is gone because the
/// shader lays its grid out from `textureDimensions(height_texture)`, so the one
/// number both read is the texture's size.
///
/// `None` for a field whose grid cannot be expressed in a `u32` draw — see
/// [`MAX_POSTS_PER_AXIS`].
pub fn ground_vertex_count(posts: [u32; 2]) -> Option<u32> {
    let cells_x = u64::from(posts[0]) + 1;
    let cells_y = u64::from(posts[1]) + 1;
    u32::try_from(6 * cells_x * cells_y).ok()
}

/// Which post grid column `column` reads: the rim columns repeat their
/// neighbour's. The Rust mirror of the shader's `post_of_column`.
pub fn post_of_column(column: u32, posts: u32) -> u32 {
    column.clamp(1, posts.max(1)) - 1
}

/// The post-grid coordinate a box-unit position sits at along one axis: the
/// Rust mirror of the shader's `ground_post_coord`.
///
/// See that function for why the clamp is exact at the apron rather than
/// approximate.
pub fn ground_post_coord(u: f32, posts: u32, scale: f32, offset: f32) -> f32 {
    let denom = if scale.abs() < 1e-6 { 1.0 } else { scale };
    let s = ((u - offset) / denom) * posts as f32 - 0.5;
    s.clamp(0.0, (posts as f32) - 1.0)
}

/// **The ground mesh's own surface height at a point of the drawn box's unit
/// square** — the Rust mirror of the shader's `ground_surface_at`, and the
/// oracle a building's base is judged against.
///
/// `height_at` answers one post's height in box `z`, already decoded through
/// the uniform's affine and clamped into the unit cube the way the shader's
/// `ground_height` does; this function's whole job is *which* posts and *what
/// weights*, which is the part the two implementations could disagree about.
///
/// **Piecewise planar, never bilinear.** `vs_ground` splits each cell along
/// the anti-diagonal into (0,0)-(1,0)-(0,1) and (0,1)-(1,0)-(1,1), so the
/// drawn surface is two planes per cell and a bilinear read of the same four
/// posts differs from it by the cell's twist everywhere off the diagonals.
pub fn ground_surface_at(
    uv: [f32; 2],
    posts: [u32; 2],
    ground_box: [f32; 4],
    height_at: impl Fn(u32, u32) -> f32,
) -> f32 {
    let s = [
        ground_post_coord(uv[0], posts[0], ground_box[0], ground_box[2]),
        ground_post_coord(uv[1], posts[1], ground_box[1], ground_box[3]),
    ];
    let last = [posts[0].saturating_sub(2), posts[1].saturating_sub(2)];
    let base = [
        (s[0].floor() as u32).min(last[0]),
        (s[1].floor() as u32).min(last[1]),
    ];
    let f = [s[0] - base[0] as f32, s[1] - base[1] as f32];
    let h00 = height_at(base[0], base[1]);
    let h10 = height_at(base[0] + 1, base[1]);
    let h01 = height_at(base[0], base[1] + 1);
    let h11 = height_at(base[0] + 1, base[1] + 1);
    if f[0] + f[1] <= 1.0 {
        h00 + f[0] * (h10 - h00) + f[1] * (h01 - h00)
    } else {
        h11 + (1.0 - f[0]) * (h01 - h11) + (1.0 - f[1]) * (h10 - h11)
    }
}

/// Where grid column `column` sits along its axis, in the **drawn box's** unit
/// square, for a field of `posts` posts placed at `(scale, offset)` — the Rust
/// mirror of the shader's `box_axis`.
///
/// Post centres for the interior columns, and the rim posts duplicated at the
/// box's own edge for the two outer ones. See the shader's copy for why the rim
/// is duplicated rather than pulled out, and for what the apron is load-bearing
/// for.
pub fn box_axis(column: u32, posts: u32, scale: f32, offset: f32) -> f32 {
    if column == 0 {
        return 0.0;
    }
    // `> posts`, where the shader spells the same test `>= posts + 1u`: WGSL
    // has no `u32` overflow to worry about at `posts == u32::MAX` and Rust
    // does, and clippy names the difference. The two are the same predicate.
    if column > posts {
        return 1.0;
    }
    scale * ((column - 1) as f32 + 0.5) / posts as f32 + offset
}

/// Where post `index` of `posts` was measured, as a 0-1 fraction of the
/// **field's own** footprint: `(i + 0.5) / posts`.
///
/// The convention `squallar_elevation::resample::post_center_km` samples at,
/// and the one [`box_axis`]'s interior columns place. Spelled once here so a
/// fixture that synthesises a field and the grid that draws it cannot disagree
/// about it, and pinned across the crate boundary by
/// `the_post_centre_convention_is_the_resamplers_own`.
pub fn post_center_fraction(index: u32, posts: u32) -> f32 {
    if posts == 0 {
        return 0.5;
    }
    (index as f32 + 0.5) / posts as f32
}

/// A 0-1 value across 24 bits of an `Rgba8Unorm`'s RGB, most significant first
/// — the Rust mirror of the shader's `pack24`.
///
/// Floor-based, so every digit is an integer over 255 and the format stores it
/// without rounding. A rounding pack would come back a whole digit out at the
/// carries.
pub fn pack24(v: f32) -> [f32; 3] {
    let x = v.clamp(0.0, 1.0) * 16_777_215.0;
    let hi = (x * (1.0 / 65536.0)).floor();
    let mid = ((x - hi * 65536.0) * (1.0 / 256.0)).floor();
    let lo = (x - hi * 65536.0 - mid * 256.0).floor();
    [hi / 255.0, mid / 255.0, lo / 255.0]
}

/// The Rust mirror of the shader's `unpack24`.
pub fn unpack24(c: [f32; 3]) -> f32 {
    let digit = |v: f32| (v * 255.0).round();
    (digit(c[0]) * 65536.0 + digit(c[1]) * 256.0 + digit(c[2])) * (1.0 / 16_777_215.0)
}

/// [`unpack24`] over the bytes a readback actually yields.
pub fn unpack24_bytes(rgb: [u8; 3]) -> f32 {
    (f32::from(rgb[0]) * 65536.0 + f32::from(rgb[1]) * 256.0 + f32::from(rgb[2]))
        * (1.0 / 16_777_215.0)
}

/// Codes the 24-bit packing has: the quantum every round-trip is judged
/// against.
pub const PACK24_CODES: u32 = 16_777_215;

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
    ground: wgpu::RenderPipeline,
    /// `None` only for a set built from a module that has no prism stages —
    /// see [`VolumePipelines::from_shader_source_without_prisms`]. Production
    /// always has one.
    building: Option<wgpu::RenderPipeline>,
    blit: wgpu::RenderPipeline,
    volume_layout: wgpu::BindGroupLayout,
    floor_layout: wgpu::BindGroupLayout,
    occluder_layout: wgpu::BindGroupLayout,
    height_layout: wgpu::BindGroupLayout,
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
    /// What binds at group 2 when this pane's offscreen carries no ground
    /// attachments: one texel of each, zero-initialised, so the alpha the march
    /// tests reads as "no ground here". The layout stays total either way.
    empty_occluder: wgpu::BindGroup,
    blit_entry_point: &'static str,
}

impl VolumePipelines {
    /// Build every pipeline for the pass egui draws into.
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
        Self::build(device, egui_attachments, wgsl, true)
    }

    /// [`Self::from_shader_source`] for a module that **predates the prism
    /// stages**, leaving the building pipeline unbuilt.
    ///
    /// It exists for one caller and the reason is worth stating rather than
    /// leaving as an option nobody explains. `volume_light.rs` pins a copy of
    /// this shader as it stood before C2 and renders through it, so that "the
    /// readable light draws the picture this renderer always drew" is measured
    /// against the picture this renderer actually drew rather than against one
    /// manufactured out of the shader under test. That pin is a **historical
    /// artifact and must stay byte-identical** — its own doc forbids
    /// re-recording it — and it has no `vs_building`, no `fs_building` and no
    /// `fn lit` for one to call. Without this constructor D2 would have retired
    /// that pin, which would have been retiring a light measurement because a
    /// building landed.
    ///
    /// Nothing is lost from what the pin measures: no prism mesh is handed to
    /// `encode_ground` in either criterion that reads it, so the pipeline this
    /// skips would never have been bound.
    pub fn from_shader_source_without_prisms(
        device: &wgpu::Device,
        egui_attachments: AttachmentConfig,
        wgsl: &str,
    ) -> Self {
        Self::build(device, egui_attachments, wgsl, false)
    }

    fn build(
        device: &wgpu::Device,
        egui_attachments: AttachmentConfig,
        wgsl: &str,
        prisms: bool,
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
                    // The ground pass's VERTEX stage reads this same block —
                    // the camera the mesh is drawn through and the camera the
                    // march unprojects through are one buffer, deliberately.
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
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

        // Two textures, no samplers: both are reached with `textureLoad`, at a
        // 1:1 texel-to-pixel invariant, so no filterability question arises.
        let occluder_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some(&label("occluder.layout")),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: BINDING_OCCLUDER_TEXTURE,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: BINDING_GROUND_TEXTURE,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
            ],
        });

        // One integer texture, no sampler and none possible: WGSL has no
        // sampler for `texture_2d<u32>`, which is what holds the 1:1
        // post-to-texel invariant by construction. Visible to the VERTEX stage
        // — this is the only texture in this crate that is.
        let height_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some(&label("height.layout")),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: BINDING_HEIGHT_TEXTURE,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Uint,
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            }],
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
                bind_group_layouts: &[
                    Some(&volume_layout),
                    Some(&floor_layout),
                    Some(&occluder_layout),
                ],
                immediate_size: 0,
            });
        // Groups 0, 1 and 3, with a deliberate hole at 2. The ground stages
        // read the camera out of the shared uniform (0), drape themselves from
        // the same pane mirror the lid uses (1), and stand on the height field
        // (3). Group 2 is skipped rather than renumbered because it is where
        // the march reads this pass's own two attachments back, and one WGSL
        // module cannot spell the same `@group @binding` pair twice.
        let ground_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some(&label("ground.pipeline_layout")),
                bind_group_layouts: &[
                    Some(&volume_layout),
                    Some(&floor_layout),
                    None,
                    Some(&height_layout),
                ],
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

        let ground = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some(&label("ground")),
            layout: Some(&ground_pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some(ENTRY_VS_GROUND),
                compilation_options: Default::default(),
                // **A genuinely empty slice, never a zero-attribute
                // `VertexBufferLayout`.** A layout with no attributes still
                // pushes a vertex step and would then demand a bound buffer at
                // draw time. Empty: pipeline creation loops zero times, the
                // draw check is `0 < 0`, the GLES backend leaves
                // `dirty_vbuf_mask` at zero and skips attribute setup, and naga
                // emits `uint(gl_VertexID)` ungated.
                buffers: &[],
            },
            // The same rasterisation as the quad, `cull_mode: None` included:
            // the grid's winding flips with the camera's side, and a culled
            // underside is a hole rather than a saving.
            primitive: QUAD_PRIMITIVE,
            depth_stencil: Some(wgpu::DepthStencilState {
                format: GROUND_DEPTH_FORMAT,
                depth_write_enabled: Some(true),
                depth_compare: Some(wgpu::CompareFunction::LessEqual),
                stencil: wgpu::StencilState::default(),
                bias: wgpu::DepthBiasState::default(),
            }),
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some(ENTRY_FS_GROUND),
                compilation_options: Default::default(),
                targets: &[
                    Some(wgpu::ColorTargetState {
                        format: OCCLUDER_FORMAT,
                        blend: None,
                        write_mask: wgpu::ColorWrites::ALL,
                    }),
                    Some(wgpu::ColorTargetState {
                        format: GROUND_COLOUR_FORMAT,
                        // `INDEPENDENT_BLEND` is conditional on GLES, so both
                        // targets share one state; `None` on both is what makes
                        // that free rather than a compromise.
                        blend: None,
                        write_mask: wgpu::ColorWrites::ALL,
                    }),
                ],
            }),
            multiview_mask: None,
            cache: None,
        });

        let building = prisms.then(|| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(&label("building")),
                // **The GROUND pass's own layout, shared rather than narrowed**, and
                // it is a correctness requirement rather than a saving.
                //
                // The prisms read only the uniform (0) and the height field (3);
                // the obvious layout therefore leaves the mirror's group 1 empty.
                // That build compiles, validates, draws — and reads the height
                // texture as **zeros**, so every building in the scene stands at
                // box `z = 0` on terrain the terrain pass drew correctly beside it.
                // WebGPU inherits a bind group across a pipeline change only where
                // the two layouts agree on every group up to and including it, so
                // a layout that differs at group 1 unbinds group 3 — and re-binding
                // it in the pass does not bring the texture back. Measured on
                // Vulkan/RTX 3090: with a narrower layout the whole city extrudes
                // from the box floor and
                // `the_prisms_stand_on_the_terrain_and_not_on_the_box_floor` reads
                // the flat-ground twin as the match. That one is `#[ignore]`d for
                // needing an adapter; run it with
                // `cargo test -p squallar-gpu --test volume_buildings -- --ignored`.
                //
                // One pass, one pipeline layout, two pipelines. The prisms declare
                // a mirror they never sample, which costs one bind-group set per
                // frame and buys an invariant that cannot be broken by editing one
                // of the two lists.
                layout: Some(&ground_pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some(ENTRY_VS_BUILDING),
                    compilation_options: Default::default(),
                    // **The one draw in this crate with a real vertex buffer**, and
                    // the deliberate exception the buildings crate's own module doc
                    // argues: the ground is a regular grid derivable from
                    // `@builtin(vertex_index)`, and a city is irregular polygons
                    // whose count is bounded by what is in the footprint. There is
                    // no topology to derive.
                    buffers: &[BUILDING_VERTEX_LAYOUT],
                },
                // **`cull_mode: None`, like every other pipeline here.** A prism is
                // a closed solid wound counter-clockwise seen from outside, so back
                // faces could be dropped — but a footprint whose winding the tile
                // got wrong would then vanish entirely rather than shade oddly, and
                // the depth test already picks the nearest face. A hole through a
                // tower costs more than the fill it would save.
                primitive: QUAD_PRIMITIVE,
                // The ground pass's own depth buffer, shared: prisms and terrain
                // resolve against each other in one pass with no second mechanism,
                // which is what makes a building behind a ridge disappear behind it
                // for free.
                depth_stencil: Some(wgpu::DepthStencilState {
                    format: GROUND_DEPTH_FORMAT,
                    depth_write_enabled: Some(true),
                    depth_compare: Some(wgpu::CompareFunction::LessEqual),
                    stencil: wgpu::StencilState::default(),
                    bias: wgpu::DepthBiasState::default(),
                }),
                multisample: wgpu::MultisampleState::default(),
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some(ENTRY_FS_BUILDING),
                    compilation_options: Default::default(),
                    // The same two targets in the same order as the ground's, so
                    // one pass description serves both draws.
                    targets: &[
                        Some(wgpu::ColorTargetState {
                            format: OCCLUDER_FORMAT,
                            blend: None,
                            write_mask: wgpu::ColorWrites::ALL,
                        }),
                        Some(wgpu::ColorTargetState {
                            format: GROUND_COLOUR_FORMAT,
                            blend: None,
                            write_mask: wgpu::ColorWrites::ALL,
                        }),
                    ],
                }),
                multiview_mask: None,
                cache: None,
            })
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
        let empty_occluder = create_empty_occluder(device, &occluder_layout);

        Self {
            raymarch,
            ground,
            building,
            blit,
            volume_layout,
            floor_layout,
            occluder_layout,
            height_layout,
            blit_layout,
            quad,
            grid_sampler,
            lut_sampler,
            floor_sampler,
            blit_sampler,
            empty_floor,
            empty_occluder,
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

    /// A target for `plan`, with the bind group the blit reads it through and,
    /// when the plan asks for it, the ground pass's three attachments.
    pub fn create_offscreen(&self, device: &wgpu::Device, plan: OffscreenPlan) -> OffscreenTarget {
        let size = plan.size;
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
        let plan = OffscreenPlan {
            size: offscreen_extent(size),
            ..plan
        };
        let ground = match plan.ground {
            GroundPass::Off => None,
            GroundPass::On => Some(create_ground_targets(
                device,
                &self.occluder_layout,
                plan.size,
            )),
        };
        OffscreenTarget {
            plan,
            texture,
            view,
            bind_group,
            ground,
        }
    }

    /// Replace `target` only when what it was built for has changed.
    pub fn ensure_offscreen(
        &self,
        device: &wgpu::Device,
        target: &mut Option<OffscreenTarget>,
        plan: OffscreenPlan,
    ) -> bool {
        let wanted = OffscreenPlan {
            size: offscreen_extent(plan.size),
            ..plan
        };
        if !offscreen_needs_rebuild(target.as_ref().map(OffscreenTarget::plan), wanted) {
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

    /// Record the ground pass into `target`, or do nothing when this target
    /// carries no ground attachments.
    ///
    /// **Recorded into the caller's existing encoder, immediately before
    /// [`Self::encode_raymarch_with_floor`]** — no second encoder and no second
    /// submit, because wgpu inserts the attachment-to-sampled barrier between
    /// two passes in one encoder.
    #[allow(clippy::too_many_arguments)]
    pub fn encode_ground(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        target: &OffscreenTarget,
        volume: &VolumeTextures,
        floor: Option<&PaneMirror>,
        heights: Option<&GroundHeights>,
        buildings: Option<&BuildingPrisms>,
    ) {
        self.encode_ground_with_timestamps(encoder, target, volume, floor, heights, buildings, None)
    }

    /// [`Self::encode_ground`], with timestamp queries bracketing the pass.
    ///
    /// The pass is what the cost harness measures, and it is one pass for both
    /// draws — so a figure taken here is the terrain and the prisms together
    /// and cannot be split between them by subtraction. `volume_building_cost`
    /// separates them by encoding the pass twice, with and without the mesh.
    #[allow(clippy::too_many_arguments)]
    pub fn encode_ground_with_timestamps(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        target: &OffscreenTarget,
        volume: &VolumeTextures,
        floor: Option<&PaneMirror>,
        heights: Option<&GroundHeights>,
        buildings: Option<&BuildingPrisms>,
        timestamp_writes: Option<wgpu::RenderPassTimestampWrites>,
    ) {
        let Some(ground) = target.ground.as_ref() else {
            return;
        };
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some(&label("ground.pass")),
            color_attachments: &[
                Some(wgpu::RenderPassColorAttachment {
                    view: &ground.occluder_view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        // Saturated, NOT transparent: an undrawn texel then
                        // decodes to `t_scale`, which is past everything, so the
                        // march's `min` is a no-op there even if the alpha test
                        // were ever dropped.
                        load: wgpu::LoadOp::Clear(OCCLUDER_CLEAR),
                        store: wgpu::StoreOp::Store,
                    },
                    depth_slice: None,
                }),
                Some(wgpu::RenderPassColorAttachment {
                    view: &ground.colour_view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                    depth_slice: None,
                }),
            ],
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: &ground.depth_view,
                depth_ops: Some(wgpu::Operations {
                    load: wgpu::LoadOp::Clear(1.0),
                    store: wgpu::StoreOp::Discard,
                }),
                stencil_ops: None,
            }),
            timestamp_writes,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        // **No field, no mesh — but the pass is still opened.** The clears
        // above are the point: a target carrying ground attachments that were
        // never written holds whatever was in them, and the march reads the
        // alpha channel of one of them on every pixel. Returning before
        // `begin_render_pass` would make "the upload failed" and "the terrain
        // is transparent here" indistinguishable at the only place that can
        // tell them apart.
        let Some(heights) = heights else {
            return;
        };
        pass.set_pipeline(&self.ground);
        pass.set_bind_group(0, &volume.bind_group, &[]);
        // The same mirror the march's own lid arm samples, bound to the same
        // group, so the drape and the lid cannot be reading two pictures.
        pass.set_bind_group(1, &floor.unwrap_or(&self.empty_floor).bind_group, &[]);
        pass.set_bind_group(3, &heights.bind_group, &[]);
        // No vertex buffer, and none is bound: every position comes from
        // `@builtin(vertex_index)`.
        // Answered rather than unwrapped: this runs on the frame thread, where
        // on wasm a panic aborts the application. `upload_heights` already
        // refused a field whose count does not fit, so a `None` here would be
        // one that reached the GPU another way.
        let Some(vertices) = ground_vertex_count(heights.posts) else {
            return;
        };
        pass.draw(0..vertices, 0..1);

        // **The prisms, into the same pass and after the terrain.** Same
        // attachments, same depth buffer, same packed `t` — so a building
        // occludes the volume and is occluded by a ridge with no mechanism
        // beyond the depth test this pass already had. Drawing them second is
        // not an ordering requirement (the depth test settles it either way);
        // it is so that a target with no mesh in it is byte-identical to the
        // frame B3 shipped.
        let (Some(buildings), Some(building_pipeline)) = (buildings, self.building.as_ref()) else {
            return;
        };
        pass.set_pipeline(building_pipeline);
        // **Every group re-set, including the mirror the prisms never sample.**
        // The two pipelines share one layout precisely so that nothing here can
        // be dropped, and re-setting all three is what keeps that true if a
        // future pipeline stops sharing it: a group left to inheritance across
        // a pipeline change is a texture that reads as zeros rather than an
        // error, which for group 3 puts the whole city on the box floor.
        pass.set_bind_group(0, &volume.bind_group, &[]);
        pass.set_bind_group(1, &floor.unwrap_or(&self.empty_floor).bind_group, &[]);
        // The same field the terrain just drew from, read out of the one
        // texture. There is no second height source for a building to disagree
        // with the ground under it about.
        pass.set_bind_group(3, &heights.bind_group, &[]);
        pass.set_vertex_buffer(0, buildings.vertices.slice(..));
        pass.set_index_buffer(buildings.indices.slice(..), wgpu::IndexFormat::Uint32);
        pass.draw_indexed(0..buildings.index_count, 0, 0..1);
    }

    /// Upload a height field as a [`HEIGHT_FORMAT`] texture, one texel a post.
    ///
    /// `None` for a field whose `samples` length is not `posts.x * posts.y`,
    /// whose posts pass [`MAX_POSTS_PER_AXIS`] or the adapter's own
    /// `max_texture_dimension_2d`, or whose grid would not fit a `u32` draw —
    /// all answered rather than panicked because this runs on the frame thread,
    /// where on wasm a panic aborts the application.
    pub fn upload_heights(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        posts: [u32; 2],
        samples: &[u16],
    ) -> Option<GroundHeights> {
        let expected = (posts[0] as usize).checked_mul(posts[1] as usize)?;
        if posts[0] < 2 || posts[1] < 2 || expected != samples.len() {
            log::error!(
                "3D volume view: refusing a {posts:?} height field carrying {} samples",
                samples.len(),
            );
            return None;
        }
        let limit = device
            .limits()
            .max_texture_dimension_2d
            .min(MAX_POSTS_PER_AXIS);
        if posts[0] > limit || posts[1] > limit {
            log::error!(
                "3D volume view: refusing a {posts:?} height field; the ceiling here is {limit}"
            );
            return None;
        }
        // **Unreachable while `MAX_POSTS_PER_AXIS` holds, and kept anyway.**
        // `a_grid_too_large_for_a_u32_draw_is_refused` asserts that the ceiling
        // is itself drawable, so this `?` cannot fire today — deleting it kills
        // no test, and that is stated rather than left to be discovered. It is
        // here because the ceiling and the draw's width are two different
        // facts: raise the one and this refuses, where without it the count
        // would wrap and the draw would silently render a fraction of the mesh.
        ground_vertex_count(posts)?;
        let held = create_height_texture(device, &self.height_layout, posts);
        // `u16` to bytes with an explicit endianness rather than a transmute of
        // the slice: the wire encoding this came off is big-endian, the texture
        // is native-endian, and letting a cast decide which one this is would
        // be right on x86 and wrong on nothing anybody would notice until it
        // was not.
        let mut bytes = Vec::with_capacity(samples.len() * 2);
        for sample in samples {
            bytes.extend_from_slice(&sample.to_ne_bytes());
        }
        queue.write_texture(
            held.texture.as_image_copy(),
            &bytes,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(posts[0] * 2),
                rows_per_image: Some(posts[1]),
            },
            wgpu::Extent3d {
                width: posts[0],
                height: posts[1],
                depth_or_array_layers: 1,
            },
        );
        Some(held)
    }

    /// Upload a building mesh as the interleaved vertex buffer and index buffer
    /// the prism draw reads.
    ///
    /// **Slices and not `squallar_buildings::BuildingMesh`**, deliberately:
    /// `tests/charter.rs` pins this crate's normal dependencies by name and the
    /// buildings crate is not among them — it links neither wgpu nor egui and
    /// exists to run inside the worker. Positions, normals and indices are the
    /// whole of what a renderer needs, so the type stays on the other side of
    /// the seam and only the numbers cross.
    ///
    /// `None` — never a panic, because this runs on the frame thread where a
    /// wasm panic aborts the tab — for a mesh whose normals do not pair with
    /// its positions, whose index count is not a whole number of triangles,
    /// whose indices do not all address its own vertices, or whose buffers
    /// would pass this adapter's largest single allocation. The coherence check
    /// is the one that is not merely defensive: an index off the end of the
    /// vertex buffer is an out-of-bounds fetch on the GPU, which is a driver's
    /// business rather than something this side of the boundary can catch.
    /// `squallar_buildings::BuildingMesh::is_coherent` makes the same check at
    /// the wire seam; this is a second boundary and it holds it for itself,
    /// because the caller here need not have come off a wire at all.
    ///
    /// An empty mesh answers `None` too. There is nothing to draw, and a zero
    /// length buffer is not a legal wgpu allocation.
    pub fn upload_buildings(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        positions: &[[f32; 3]],
        normals: &[[f32; 3]],
        indices: &[u32],
    ) -> Option<BuildingPrisms> {
        if positions.is_empty() || indices.is_empty() {
            return None;
        }
        if positions.len() != normals.len() || !indices.len().is_multiple_of(3) {
            log::error!(
                "3D volume view: refusing a prism mesh of {} positions, {} normals and {} \
                 indices",
                positions.len(),
                normals.len(),
                indices.len(),
            );
            return None;
        }
        let vertex_count = u32::try_from(positions.len()).ok()?;
        let index_count = u32::try_from(indices.len()).ok()?;
        // One pass, and it rides the serialisation below rather than being a
        // second walk of the same buffer.
        if indices.iter().copied().max()? >= vertex_count {
            log::error!(
                "3D volume view: refusing a prism mesh whose indices reach past its {vertex_count} \
                 vertices",
            );
            return None;
        }

        let vertex_bytes = u64::from(vertex_count) * BUILDING_VERTEX_BYTES;
        let index_bytes = u64::from(index_count) * BUILDING_INDEX_BYTES;
        let ceiling = device.limits().max_buffer_size;
        if vertex_bytes > ceiling || index_bytes > ceiling {
            log::error!(
                "3D volume view: refusing a prism mesh of {vertex_bytes} vertex bytes and \
                 {index_bytes} index bytes; this adapter's largest buffer is {ceiling}",
            );
            return None;
        }

        // Interleaved position-then-normal, which is the layout the pipeline
        // declares and the stride the buildings crate's budget prices. Built
        // here rather than shipped that way because the worker's reply
        // nominates the two as separate tails, and concatenating a 9 MB mesh
        // at each end to save this walk would cost more than it saved.
        let mut vertices = Vec::with_capacity(vertex_bytes as usize);
        for (position, normal) in positions.iter().zip(normals) {
            for axis in position.iter().chain(normal) {
                vertices.extend_from_slice(&axis.to_le_bytes());
            }
        }
        let mut index_bytes_out = Vec::with_capacity(index_bytes as usize);
        for index in indices {
            index_bytes_out.extend_from_slice(&index.to_le_bytes());
        }

        let vertices_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(&label("building.vertices")),
            size: vertex_bytes,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let indices_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(&label("building.indices")),
            size: index_bytes,
            usage: wgpu::BufferUsages::INDEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        queue.write_buffer(&vertices_buffer, 0, &vertices);
        queue.write_buffer(&indices_buffer, 0, &index_bytes_out);

        Some(BuildingPrisms {
            vertices: vertices_buffer,
            indices: indices_buffer,
            index_count,
            vertex_count,
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
        pass.set_bind_group(
            2,
            target
                .ground
                .as_ref()
                .map_or(&self.empty_occluder, |g| &g.bind_group),
            &[],
        );
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

/// What an offscreen is built for.
///
/// The **rung** rides here beside the size because a governor may move a rung
/// mid-session, and a target compared on size alone would then be kept while
/// describing a quality it no longer has. The **ground pass** rides here because
/// it decides whether the three extra attachments exist, and a target built
/// without them cannot grow them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OffscreenPlan {
    /// Texels along each axis.
    pub size: [u32; 2],
    /// The resolution rung this size came off.
    pub rung: ResolutionRung,
    /// Whether the ground pass's attachments are carried.
    pub ground: GroundPass,
}

impl OffscreenPlan {
    /// A plan for a target with no ground pass, at the finest rung — what
    /// every caller wanting today's picture at an explicit size asks for.
    pub fn native(size: [u32; 2]) -> Self {
        Self {
            size,
            rung: ResolutionRung::Native,
            ground: GroundPass::Off,
        }
    }

    /// The plan a fit produced.
    pub fn of(fitted: FittedOffscreen) -> Self {
        Self {
            size: fitted.size,
            rung: fitted.quality.resolution,
            ground: fitted.ground,
        }
    }
}

/// The pane-sized target the raymarch renders into.
pub struct OffscreenTarget {
    plan: OffscreenPlan,
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    bind_group: wgpu::BindGroup,
    /// The ground pass's attachments, and the group-2 bind group the march
    /// reads them back through. Recreated in lockstep with the colour target
    /// above, which is the whole reason they live here rather than beside the
    /// pipelines.
    ground: Option<GroundTargets>,
}

impl OffscreenTarget {
    /// Texels along each axis.
    pub fn size(&self) -> [u32; 2] {
        self.plan.size
    }

    /// Everything this target was built for.
    pub fn plan(&self) -> OffscreenPlan {
        self.plan
    }

    /// Whether this target carries the ground pass's attachments.
    pub fn ground_pass(&self) -> GroundPass {
        self.plan.ground
    }

    /// The texture itself, for a readback in a test.
    pub fn texture(&self) -> &wgpu::Texture {
        &self.texture
    }

    /// The occluder attachment, for a readback in a test. `None` when this
    /// target carries no ground pass.
    pub fn occluder_texture(&self) -> Option<&wgpu::Texture> {
        self.ground.as_ref().map(|g| &g.occluder)
    }

    /// The ground colour attachment, for a readback in a test.
    pub fn ground_texture(&self) -> Option<&wgpu::Texture> {
        self.ground.as_ref().map(|g| &g.colour)
    }
}

/// The three attachments a ground-drawing pane's offscreen carries.
struct GroundTargets {
    occluder: wgpu::Texture,
    occluder_view: wgpu::TextureView,
    colour: wgpu::Texture,
    colour_view: wgpu::TextureView,
    depth_view: wgpu::TextureView,
    bind_group: wgpu::BindGroup,
}

/// What the occluder is cleared to: every digit saturated, so an undrawn texel
/// decodes to exactly 1.0 and therefore to `t_scale` — past the far side of the
/// box, where the march's `min` against the box exit is a no-op.
const OCCLUDER_CLEAR: wgpu::Color = wgpu::Color {
    r: 1.0,
    g: 1.0,
    b: 1.0,
    a: 0.0,
};

/// The ground pass's attachments at `size`, and the bind group the march reads
/// the two colour ones back through.
fn create_ground_targets(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    size: [u32; 2],
) -> GroundTargets {
    let extent = wgpu::Extent3d {
        width: size[0].max(1),
        height: size[1].max(1),
        depth_or_array_layers: 1,
    };
    let colour_usage = wgpu::TextureUsages::RENDER_ATTACHMENT
        | wgpu::TextureUsages::TEXTURE_BINDING
        // The one word that lets a test decode what the pass actually wrote.
        | wgpu::TextureUsages::COPY_SRC;
    let occluder = device.create_texture(&wgpu::TextureDescriptor {
        label: Some(&label("occluder")),
        size: extent,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: OCCLUDER_FORMAT,
        usage: colour_usage,
        view_formats: &[],
    });
    let colour = device.create_texture(&wgpu::TextureDescriptor {
        label: Some(&label("ground_colour")),
        size: extent,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: GROUND_COLOUR_FORMAT,
        usage: colour_usage,
        view_formats: &[],
    });
    let depth = device.create_texture(&wgpu::TextureDescriptor {
        label: Some(&label("ground_depth")),
        size: extent,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: GROUND_DEPTH_FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    });
    let occluder_view = occluder.create_view(&wgpu::TextureViewDescriptor::default());
    let colour_view = colour.create_view(&wgpu::TextureViewDescriptor::default());
    let depth_view = depth.create_view(&wgpu::TextureViewDescriptor::default());
    let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some(&label("occluder.bind_group")),
        layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: BINDING_OCCLUDER_TEXTURE,
                resource: wgpu::BindingResource::TextureView(&occluder_view),
            },
            wgpu::BindGroupEntry {
                binding: BINDING_GROUND_TEXTURE,
                resource: wgpu::BindingResource::TextureView(&colour_view),
            },
        ],
    });
    GroundTargets {
        occluder,
        occluder_view,
        colour,
        colour_view,
        depth_view,
        bind_group,
    }
}

/// The 1x1 pair bound at group 2 when a pane draws no ground. Created and never
/// written: WebGPU zero-initialises, so the alpha the march tests reads zero,
/// which is "no ground here".
fn create_empty_occluder(device: &wgpu::Device, layout: &wgpu::BindGroupLayout) -> wgpu::BindGroup {
    let one = |what: &str, format| {
        device
            .create_texture(&wgpu::TextureDescriptor {
                label: Some(&label(what)),
                size: wgpu::Extent3d {
                    width: 1,
                    height: 1,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage: wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            })
            .create_view(&wgpu::TextureViewDescriptor::default())
    };
    let occluder = one("occluder.placeholder", OCCLUDER_FORMAT);
    let colour = one("ground_colour.placeholder", GROUND_COLOUR_FORMAT);
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some(&label("occluder.placeholder.bind_group")),
        layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: BINDING_OCCLUDER_TEXTURE,
                resource: wgpu::BindingResource::TextureView(&occluder),
            },
            wgpu::BindGroupEntry {
                binding: BINDING_GROUND_TEXTURE,
                resource: wgpu::BindingResource::TextureView(&colour),
            },
        ],
    })
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

/// A building mesh on the GPU: one interleaved vertex buffer and one index
/// buffer.
///
/// **The only vertex and index buffers this crate allocates**, which is why
/// they get a size of their own to report: the terrain's grid is procedural
/// and really does cost zero here.
pub struct BuildingPrisms {
    vertices: wgpu::Buffer,
    indices: wgpu::Buffer,
    index_count: u32,
    vertex_count: u32,
}

impl BuildingPrisms {
    /// Indices in the mesh — three per triangle.
    pub fn index_count(&self) -> u32 {
        self.index_count
    }

    /// Vertices in the mesh.
    pub fn vertex_count(&self) -> u32 {
        self.vertex_count
    }

    /// GPU bytes the pair occupies, priced the way
    /// `squallar_buildings::budget` prices a rung.
    pub fn buffer_bytes(&self) -> u64 {
        u64::from(self.vertex_count) * BUILDING_VERTEX_BYTES
            + u64::from(self.index_count) * BUILDING_INDEX_BYTES
    }
}

/// A height field on the GPU: one `R16Uint` texel per post, plus the bind group
/// the ground pass's vertex stage reads it through at group 3.
///
/// **The posts are the texture's own dimensions**, kept here only so the draw
/// can compute its vertex count without asking the texture; the shader reads
/// them straight off `textureDimensions`, so the two cannot describe different
/// grids.
pub struct GroundHeights {
    texture: wgpu::Texture,
    bind_group: wgpu::BindGroup,
    posts: [u32; 2],
}

impl GroundHeights {
    /// Posts along each axis.
    pub fn posts(&self) -> [u32; 2] {
        self.posts
    }

    /// GPU bytes this field occupies: two per post.
    pub fn texture_bytes(&self) -> usize {
        (self.posts[0] as usize).saturating_mul(self.posts[1] as usize) * 2
    }

    /// The texture itself, for tests that read it back.
    pub fn texture(&self) -> &wgpu::Texture {
        &self.texture
    }
}

/// A [`HEIGHT_FORMAT`] texture of `posts` texels and its bind group, with
/// nothing written into it. WebGPU zero-initialises, so an un-uploaded field
/// reads as the encoding's own floor everywhere — which is why the ground pass
/// is not encoded at all without a real one rather than drawn from this.
fn create_height_texture(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    posts: [u32; 2],
) -> GroundHeights {
    let posts = [posts[0].max(1), posts[1].max(1)];
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some(&label("height")),
        size: wgpu::Extent3d {
            width: posts[0],
            height: posts[1],
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: HEIGHT_FORMAT,
        usage: wgpu::TextureUsages::TEXTURE_BINDING
            | wgpu::TextureUsages::COPY_DST
            | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some(&label("height.bind_group")),
        layout,
        entries: &[wgpu::BindGroupEntry {
            binding: BINDING_HEIGHT_TEXTURE,
            resource: wgpu::BindingResource::TextureView(&view),
        }],
    });
    GroundHeights {
        texture,
        bind_group,
        posts,
    }
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
    ///
    /// The one seam a uniform reaches the GPU through, and therefore where the
    /// occluder's two coupled lanes are checked: `occluder_t_scale` is derived
    /// from `eye_in_box`, both are public, and a scale computed against another
    /// eye mis-clips every ray in the frame while the picture still looks
    /// plausible. `debug_assert`, so the frame path pays nothing in release.
    pub fn write_uniform(&self, queue: &wgpu::Queue, uniform: &VolumeUniform) {
        debug_assert!(
            uniform.occluder_is_aimed_at_its_own_eye(),
            "occluder_t_scale is {} but this uniform's eye at {:?} wants {}; \
             set it through `VolumeUniform::aim_occluder`, which derives one \
             from the other",
            uniform.occluder_t_scale,
            uniform.eye_in_box,
            VolumeUniform::t_scale_for(uniform.eye_in_box),
        );
        // The second coupled pair, at the same seam - and a `log::error!`
        // rather than a `debug_assert!`, deliberately. The shader refuses the
        // two grounds on its own (`map_t`'s guard), so this is not what makes
        // the picture right; it is what TELLS a caller that built the pair. A
        // debug assertion would say nothing in release and nothing at all on
        // the shipped web build, which is where a wiring mistake would
        // actually reach a user - and it would abort
        // `the_lid_is_never_painted_where_the_mesh_did_not_draw`, which builds
        // this exact pair on purpose to prove the shader survives it. That one
        // is `#[ignore]`d (it needs a real adapter); run it with
        // `cargo test -p squallar-gpu --test volume_occluder -- --ignored`.
        // The default-row gate on the same guard is
        // `the_lid_is_never_drawn_beside_a_mesh` in `volume_shader.rs`.
        if !uniform.ground_is_one_surface() {
            log::error!(
                "3D volume view: this uniform asks for the flat map lid and a ground pass at \
                 once. The mesh IS the ground, so a frame with one has no lid; aim the ground \
                 pass through `VolumeUniform::aim_occluder`, which puts the lid out as it aims. \
                 The shader draws the mesh alone regardless",
            );
        }
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
/// index is not `squallar_radar::voxel::NO_DATA_INDEX`.
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
                let covered = index != squallar_radar::voxel::NO_DATA_INDEX;
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
                            covered += u32::from(index != squallar_radar::voxel::NO_DATA_INDEX);
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

/// Whether a held offscreen has to be thrown away.
///
/// The whole plan, not the size alone: two of a plan's three fields can move
/// without the size moving with them, and a target kept across either is
/// describing something it is not.
fn offscreen_needs_rebuild(held: Option<OffscreenPlan>, wanted: OffscreenPlan) -> bool {
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

/// One prism vertex as the GPU reads it: three `f32` of position then three of
/// normal, interleaved.
///
/// The same 24 bytes `squallar_buildings::PRISM_VERTEX_BYTES` prices a vertex
/// at, which is the number that crate's whole rung ladder is built on;
/// `the_vertex_stride_is_what_the_budget_prices` holds the pair.
pub const BUILDING_VERTEX_BYTES: u64 = 24;

/// Bytes one prism index occupies. `u32`, matching
/// `squallar_buildings::PRISM_INDEX_BYTES`, because that crate's finest rung is
/// sixteen times past what a `u16` addresses.
pub const BUILDING_INDEX_BYTES: u64 = 4;

/// The vertex layout the building pipeline reads its mesh through.
const BUILDING_VERTEX_LAYOUT: wgpu::VertexBufferLayout<'static> = wgpu::VertexBufferLayout {
    array_stride: BUILDING_VERTEX_BYTES,
    step_mode: wgpu::VertexStepMode::Vertex,
    attributes: &[
        wgpu::VertexAttribute {
            format: wgpu::VertexFormat::Float32x3,
            offset: 0,
            shader_location: 0,
        },
        wgpu::VertexAttribute {
            format: wgpu::VertexFormat::Float32x3,
            offset: 12,
            shader_location: 1,
        },
    ],
};

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
