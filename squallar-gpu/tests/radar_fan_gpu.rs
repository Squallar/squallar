//! **The fan draws the gate the hover pick would read, in the colour the table
//! holds for its code.**
//!
//! That sentence is the whole of what the polar representation buys and the
//! whole of what it can get wrong. The plan-view raster it replaces resolves
//! colour on the CPU, per gate, into an RGBA image; this path uploads the
//! **codes the wire carried** and resolves colour per fragment out of a
//! `256 x 1` table. Between the two sit a projection evaluated per vertex, an
//! inverse-Mercator solve evaluated per fragment, and a gate index derived from
//! it — three places where a picture can look entirely plausible and be reading
//! the wrong gate.
//!
//! So the criterion here is not "something was painted". Every covered pixel's
//! `(radial, gate)` is derived independently in `f64`, out of
//! `squallar_geo::site_bearing_range_km` and
//! `squallar_radar::beam::slant_range_for_ground_km` — the functions the hover
//! pick itself uses — and the pixel is required to hold the table entry for the
//! code planted at that cell. Not near it: the entry.
//!
//! # What is excluded, and why that does not hollow it out
//!
//! A pixel whose ground range lands within 2% of a gate boundary, or whose
//! bearing lands within 0.02 degrees of a wedge edge, is skipped: the shader
//! works in `f32` and the mirror in `f64`, and at a boundary the two may
//! legitimately round to different sides of it. Every such test asserts a floor
//! on how many pixels it did compare and prints both counts, because a
//! comparison that quietly excluded everything is the failure mode this
//! exclusion could produce.
//!
//! # Both gamma conventions
//!
//! egui picks its fragment entry point off the target's sRGB-ness and this pass
//! draws into the same render pass, so it has to pick the same one. Both are
//! drawn, over an **opaque** clear so that the blend happens in a different
//! space on each arm, and the two readbacks are asserted to differ — the
//! interleaved control that says the comparison can see a gamma difference at
//! all.
//!
//! # Nothing here installs the painter
//!
//! Every callback below is constructed by this file and handed to an
//! `egui_wgpu::Renderer` this file owns. No painter seam publishes one, no pane
//! issues one, and nothing selects the polar price.
#![cfg(not(target_arch = "wasm32"))]

use std::sync::Arc;

use egui_wgpu::wgpu;
use naga::back::glsl;
use naga::proc::{BoundsCheckPolicies, BoundsCheckPolicy};
use naga::valid::{Capabilities, ValidationFlags, Validator};
use squallar_device_profile::constants::{
    MAX_POLAR_GATES, MAX_POLAR_RADIALS, POLAR_LUT_BYTES, POLAR_LUT_ENTRIES,
};
use squallar_egui::radar_fan::{FanGeometry, FanSweep};
use squallar_gpu::egui_renderer::AttachmentConfig;
use squallar_gpu::radar_fan::{
    EDGE_VEC4S, FanRefusal, FanView, MESH_INDICES, MESH_VERTICES, RADAR_FAN_WGSL, RINGS,
    RadarFanCallback, RadarFanStore, SECTORS, SWEEP_UNIFORM_BYTES, admit, chain_bytes, pack_vertex,
};

/// The canvas, in pixels — which is also the callback's viewport and the whole
/// frame, so nothing here depends on egui's rect rounding.
const SIDE: u32 = 256;

/// The site the fixtures are flown from. Nothing depends on the particular
/// pair; a mid-latitude site is chosen so `cos(lat)` is neither 1 nor near 0
/// and a longitude error cannot hide inside it.
const SITE_LAT: f64 = 35.0;
const SITE_LON: f64 = -97.0;

/// Pixels the world spans at the zoom the fixtures are drawn at. At `SITE_LAT`
/// this puts the 200 km fixture disc a shade under 100 px out, so the disc sits
/// inside a 256 px canvas with a margin all round.
const WORLD_PX: f32 = 16384.0;

/// Radials in the fixture sweep, at one degree each.
const RADIALS: usize = 360;
/// Gates in the fixture sweep.
const GATES: usize = 200;
/// One gate's depth along the beam, km.
const GATE_KM: f32 = 1.0;
/// Gate 0's centre along the beam, km.
const FIRST_GATE_KM: f32 = 0.5;

/// Ground kilometres a pixel covers at [`WORLD_PX`]. Only the mip selection
/// reads it, and one gate is [`GATE_KM`], so at this value the LOD is level 0.
const KM_PER_PX: f32 = 2.0;

/// **How far off a gate boundary a pixel must sit to be compared.** In gates:
/// 2% of one is 20 m at the fixture's spacing, and the two arms' arithmetic
/// disagrees by nanometres, so this is slack by three orders of magnitude
/// rather than a fitted tolerance.
const GATE_MARGIN: f64 = 0.02;

/// The same, for a wedge edge, in degrees.
const AZIMUTH_MARGIN: f64 = 0.02;

/// Gates at each end of a radial that are not compared at all: the mesh's
/// outer boundary is a chord through the arc and its innermost sectors are
/// thinner than a pixel, so coverage there is a property of the rasteriser
/// rather than of the gate solve.
const EDGE_GATES: usize = 6;

/// The clear the gamma-convention arms are drawn over: opaque mid grey, so the
/// blend has something to blend WITH. Over a transparent clear, premultiplied
/// `src + dst*(1-a)` is `src` whatever the space, and the two conventions would
/// be indistinguishable by construction.
const CLEAR_GAMMA: f64 = 128.0 / 255.0;

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

/// The code planted at one cell. A function of both axes, and coprime strides,
/// so that a picture reading the wrong radial, the wrong gate, or the two
/// transposed produces a different colour at almost every pixel rather than at
/// a few.
fn code_at(radial: usize, gate: usize) -> u8 {
    2 + ((radial * 13 + gate * 7) % (POLAR_LUT_ENTRIES - 2)) as u8
}

/// An opaque table where every code has its own colour.
fn opaque_lut() -> Vec<u8> {
    let mut lut = vec![0u8; POLAR_LUT_BYTES];
    for (code, entry) in lut.chunks_exact_mut(4).enumerate() {
        let c = code as u8;
        entry.copy_from_slice(&[c, 255 - c, c.wrapping_mul(37), 255]);
    }
    // Code 0 is below-threshold and unpainted; the fixture never plants it, and
    // leaving it opaque would hide a fan that read zeroes off an empty texture.
    lut[0..4].copy_from_slice(&[0, 0, 0, 0]);
    lut
}

/// [`opaque_lut`] with the alphas spread over the range, for the gamma arms.
fn translucent_lut() -> Vec<u8> {
    let mut lut = opaque_lut();
    for (code, entry) in lut.chunks_exact_mut(4).enumerate().skip(1) {
        entry[3] = 40 + ((code * 5) % 200) as u8;
    }
    lut
}

/// One level-0 plane of [`code_at`], with no chain above it.
fn plane_codes() -> Vec<u8> {
    let mut codes = Vec::with_capacity(RADIALS * GATES);
    for radial in 0..RADIALS {
        for gate in 0..GATES {
            codes.push(code_at(radial, gate));
        }
    }
    codes
}

/// One degree per radial, edge to edge, so the drawn wedges tile the circle
/// exactly and `floor(bearing)` is the radial.
fn one_degree_edges() -> Vec<[f32; 2]> {
    (0..RADIALS)
        .map(|i| [i as f32, i as f32 + 1.0])
        .collect::<Vec<_>>()
}

/// The geometry for a sweep at `elevation_deg`, or one whose ranges are already
/// ground ranges.
///
/// The two GROUND radii are the ones the producer fills in
/// (`squallar_app::render_dispatch`'s `fan_sweep`): gate 0's near edge and the
/// last reached gate's far edge, taken through the same beam bend the fragment
/// inverts. The store reads one gate's ground depth off their difference, so
/// nothing here states that number twice.
fn geometry(elevation_deg: Option<f32>) -> FanGeometry {
    let near_slant = f64::from(FIRST_GATE_KM - 0.5 * GATE_KM);
    let far_slant = f64::from(FIRST_GATE_KM + (GATES as f32 - 0.5) * GATE_KM);
    let ground = |slant: f64| match elevation_deg {
        Some(e) => squallar_radar::beam::ground_range_km(slant, f64::from(e)),
        None => slant,
    };
    FanGeometry {
        site_lat: SITE_LAT,
        site_lon: SITE_LON,
        first_gate_slant_km: f64::from(FIRST_GATE_KM),
        gate_interval_slant_km: f64::from(GATE_KM),
        elevation_deg: elevation_deg.map(f64::from),
        reach_gates: GATES as u32,
        reach_km: ground(far_slant),
        first_gate_km: ground(near_slant.max(0.0)),
        earth_radius_km: squallar_geo::EARTH_RADIUS_KM,
        effective_radius_km: squallar_radar::beam::RE_EFF_KM,
    }
}

/// One payload's level offsets, for a chain laid out level 0 first with no gap.
///
/// `max(1, extent >> level)`, which is the texture's own mip extent and
/// therefore the producer's: see `the_chain_arithmetic_is_the_producers`.
fn level_offsets(radials: usize, gates: usize, levels: usize) -> Vec<u32> {
    let mut at = 0u32;
    let mut out = Vec::with_capacity(levels);
    for level in 0..levels {
        out.push(at);
        at += (level_extent(radials, level) * level_extent(gates, level)) as u32;
    }
    out
}

/// One axis at one level, the way a texture sizes it.
fn level_extent(extent: usize, level: usize) -> usize {
    let shift = u32::try_from(level).unwrap_or(u32::MAX);
    extent.checked_shr(shift).unwrap_or(0).max(1)
}

/// One payload from its parts, laid out level 0 first with no gap.
///
/// The field is [`squallar_radar`]'s reflectivity spelling because a payload
/// carries one; nothing this file drives reads it — the table is baked before
/// a sweep reaches the store, so all that is left to name is which legend the
/// gates belong to.
fn payload(
    radials: usize,
    gates: usize,
    codes: Vec<u8>,
    levels: usize,
    lut: Vec<u8>,
    edges: Vec<[f32; 2]>,
    geometry: FanGeometry,
) -> FanSweep {
    FanSweep {
        field: squallar_radar::fields::known::REFLECTIVITY,
        radials: radials as u32,
        gates: gates as u32,
        codes,
        level_offsets: level_offsets(radials, gates, levels),
        lut_rgba: lut,
        edges,
        geometry,
    }
}

/// The fixture sweep, at one elevation arm and one table.
///
/// **No id.** Residency is the payload's own address, so two fixtures with
/// identical bytes are two sweeps because they are two allocations — which is
/// what a `Weak` handle held beside each makes exact.
fn fixture(elevation_deg: Option<f32>, lut: Vec<u8>) -> Arc<FanSweep> {
    let sweep = Arc::new(payload(
        RADIALS,
        GATES,
        plane_codes(),
        1,
        lut,
        one_degree_edges(),
        geometry(elevation_deg),
    ));
    assert!(sweep.is_well_formed(), "the fixture describes itself");
    admit(&sweep).expect("the fixture sweep is inside every cap");
    sweep
}

/// The view every fixture is drawn under: the site at the canvas centre, the
/// whole canvas as the viewport.
fn view(opacity: f32) -> FanView {
    FanView {
        rect: egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(SIDE as f32, SIDE as f32)),
        site_pt: egui::pos2(SIDE as f32 / 2.0, SIDE as f32 / 2.0),
        world_pt: WORLD_PX,
        site_lat_deg: SITE_LAT,
        km_per_pt: KM_PER_PX,
        earth_radius_km: squallar_geo::EARTH_RADIUS_KM as f32,
        opacity,
    }
}

// ---------------------------------------------------------------------------
// The independent mirror
// ---------------------------------------------------------------------------

/// What the fan should be showing at one pixel, derived from the same functions
/// the hover pick uses and sharing no line with the shader.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Expect {
    /// The cell the pixel sits over.
    Cell { radial: usize, gate: usize },
    /// Outside the sweep's gates: nothing is painted there.
    Unpainted,
    /// Too close to a boundary for the two arithmetics to be required to agree.
    Skip,
}

/// The mirror. `f64` throughout, and every geographic step is a call into the
/// workspace's own geodesy rather than a second spelling of it.
fn expect_at(px: u32, py: u32, elevation_deg: Option<f32>) -> Expect {
    let site = view(1.0);
    // The fragment is evaluated at the pixel centre.
    let rel_x = (f64::from(px) + 0.5 - f64::from(site.site_pt.x)) / f64::from(WORLD_PX);
    let rel_y = (f64::from(py) + 0.5 - f64::from(site.site_pt.y)) / f64::from(WORLD_PX);

    let two_pi = std::f64::consts::TAU;
    let merc_y_site = SITE_LAT.to_radians().sin().atanh();
    let lat = (merc_y_site - two_pi * rel_y).tanh().asin().to_degrees();
    let lon = SITE_LON + (two_pi * rel_x).to_degrees();

    let (bearing_deg, ground_km) =
        squallar_geo::site_bearing_range_km(SITE_LAT, SITE_LON, lat, lon);

    let along_beam_km = match elevation_deg {
        Some(e) => squallar_radar::beam::slant_range_for_ground_km(ground_km, f64::from(e)),
        None => ground_km,
    };
    let g = (along_beam_km - f64::from(FIRST_GATE_KM)) / f64::from(GATE_KM) + 0.5;
    let fractional = g - g.floor();
    if !(GATE_MARGIN..=1.0 - GATE_MARGIN).contains(&fractional) {
        return Expect::Skip;
    }
    let gate = g.floor();
    if gate < 0.0 || gate >= GATES as f64 {
        // Outside the disc entirely is unpainted; the ring just past the last
        // gate is where the mesh's chord boundary lives and is not asserted.
        if gate >= GATES as f64 && gate < (GATES + EDGE_GATES) as f64 {
            return Expect::Skip;
        }
        return Expect::Unpainted;
    }
    let gate = gate as usize;
    if !(EDGE_GATES..GATES - EDGE_GATES).contains(&gate) {
        return Expect::Skip;
    }

    let azimuth_fraction = bearing_deg - bearing_deg.floor();
    if !(AZIMUTH_MARGIN..=1.0 - AZIMUTH_MARGIN).contains(&azimuth_fraction) {
        return Expect::Skip;
    }
    let radial = bearing_deg.floor() as usize % RADIALS;
    Expect::Cell { radial, gate }
}

// ---------------------------------------------------------------------------
// Device, target, readback
// ---------------------------------------------------------------------------

/// Held for the length of a test, so only one talks to the GPU at a time.
static ONE_AT_A_TIME: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn gpu_lock() -> std::sync::MutexGuard<'static, ()> {
    ONE_AT_A_TIME
        .lock()
        .unwrap_or_else(|held| held.into_inner())
}

/// A device on whatever adapter is to be had, naming it once per process.
///
/// **The name matters.** This box carries both a discrete adapter and a
/// software one, and a figure read off the wrong one is a different reading.
fn device() -> Option<(wgpu::Device, wgpu::Queue)> {
    device_with(wgpu::Features::empty())
}

/// [`device`] with the staging ring's feature where the adapter has it.
///
/// The suite's ordinary device asks for nothing, so
/// `radar_fan::chain_staging::available` reads `false` on it and the staged
/// upload route is unreachable — a suite that never drives half the code it
/// covers. Asked for separately rather than added to `device` because every
/// other test here is about a picture and not about a route, and a device
/// carrying a feature they do not use is a difference between what they run on
/// and what CI's other rows run on.
fn device_with_ring() -> Option<(wgpu::Device, wgpu::Queue)> {
    device_with(squallar_gpu::staging_ring::STAGING_RING_FEATURE)
}

fn device_with(extra: wgpu::Features) -> Option<(wgpu::Device, wgpu::Queue)> {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        compatible_surface: None,
        force_fallback_adapter: false,
    }))
    .ok()?;
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let info = adapter.get_info();
        eprintln!(
            "wgpu adapter: {:?} {:?} \"{}\" (driver: {} {})",
            info.backend, info.device_type, info.name, info.driver, info.driver_info
        );
    });
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("radar-fan"),
        required_features: adapter.features() & extra,
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
        label: Some("radar fan target"),
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

/// Read an RGBA8 target back, row-major, four bytes a texel.
fn read_back(device: &wgpu::Device, queue: &wgpu::Queue, texture: &wgpu::Texture) -> Vec<u8> {
    let unpadded = SIDE * 4;
    let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let padded = unpadded.div_ceil(align) * align;
    let staging = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("radar fan readback"),
        size: u64::from(padded) * u64::from(SIDE),
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let mut encoder = device.create_command_encoder(&Default::default());
    encoder.copy_texture_to_buffer(
        texture.as_image_copy(),
        wgpu::TexelCopyBufferInfo {
            buffer: &staging,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(padded),
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
    staging.slice(..).map_async(wgpu::MapMode::Read, |result| {
        result.expect("mapping the readback buffer failed");
    });
    device
        .poll(wgpu::PollType::wait_indefinitely())
        .expect("polling the device failed");
    let mapped = staging.slice(..).get_mapped_range();
    let mut out = Vec::with_capacity((SIDE * SIDE * 4) as usize);
    for row in 0..SIDE as usize {
        let start = row * padded as usize;
        out.extend_from_slice(&mapped[start..start + unpadded as usize]);
    }
    out
}

/// One frame: the callbacks issued into the whole canvas, drawn by egui's own
/// renderer through the store, read back.
///
/// The whole path is egui's — the tessellator, `update_buffers`, the pass, the
/// clip — so what is measured is the callback as `egui_wgpu` will actually run
/// it and not a render pass this file assembled.
fn frame(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    store: Option<RadarFanStore>,
    format: wgpu::TextureFormat,
    clear: wgpu::Color,
    callbacks: Vec<RadarFanCallback>,
) -> (Vec<u8>, egui_wgpu::Renderer) {
    let mut renderer = egui_wgpu::Renderer::new(
        device,
        format,
        egui_wgpu::RendererOptions {
            depth_stencil_format: None,
            msaa_samples: 1,
            dithering: squallar_gpu::egui_renderer::EGUI_DITHERING,
            ..Default::default()
        },
    );
    if let Some(store) = store {
        renderer.callback_resources.insert(store);
    }

    let canvas = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(SIDE as f32, SIDE as f32));
    let ctx = egui::Context::default();
    ctx.begin_pass(egui::RawInput {
        screen_rect: Some(canvas),
        ..Default::default()
    });
    {
        let painter = ctx
            .layer_painter(egui::LayerId::background())
            .with_clip_rect(canvas);
        for callback in callbacks {
            painter.add(egui::Shape::Callback(egui::epaint::PaintCallback {
                rect: canvas,
                callback: callback.payload(),
            }));
        }
    }
    let output = ctx.end_pass();
    let tris = ctx.tessellate(output.shapes, 1.0);
    for (id, delta) in &output.textures_delta.set {
        renderer.update_texture(device, queue, *id, delta);
    }

    let descriptor = egui_wgpu::ScreenDescriptor {
        size_in_pixels: [SIDE, SIDE],
        pixels_per_point: 1.0,
    };
    let texture = target(device, format);
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    let mut encoder = device.create_command_encoder(&Default::default());
    let user = renderer.update_buffers(device, queue, &mut encoder, &tris, &descriptor);
    {
        let pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("radar fan"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(clear),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        renderer.render(&mut pass.forget_lifetime(), &tris, &descriptor);
    }
    queue.submit(user.into_iter().chain(Some(encoder.finish())));
    (read_back(device, queue, &texture), renderer)
}

/// The store back out of a renderer's callback resources.
fn store_of(renderer: &egui_wgpu::Renderer) -> &RadarFanStore {
    renderer
        .callback_resources
        .get::<RadarFanStore>()
        .expect("the store this frame was drawn with")
}

fn attachments(format: wgpu::TextureFormat) -> AttachmentConfig {
    AttachmentConfig {
        color_format: format,
        depth_format: None,
        msaa_samples: 1,
    }
}

/// Texels that are not the clear.
fn painted(pixels: &[u8], clear: [u8; 4]) -> usize {
    pixels.chunks_exact(4).filter(|p| *p != clear).count()
}

fn texel(pixels: &[u8], px: u32, py: u32) -> [u8; 4] {
    let at = ((py * SIDE + px) * 4) as usize;
    [pixels[at], pixels[at + 1], pixels[at + 2], pixels[at + 3]]
}

// ---------------------------------------------------------------------------
// The shader, without an adapter
// ---------------------------------------------------------------------------

/// Parse and validate the shader once, with naga's own diagnostic on failure.
fn validated() -> (naga::Module, naga::valid::ModuleInfo) {
    let module = naga::front::wgsl::parse_str(RADAR_FAN_WGSL).unwrap_or_else(|error| {
        panic!(
            "src/radar_fan.wgsl is not valid WGSL:\n{}",
            error.emit_to_string(RADAR_FAN_WGSL)
        )
    });
    let info = Validator::new(ValidationFlags::all(), Capabilities::empty())
        .validate(&module)
        .unwrap_or_else(|error| {
            panic!(
                "src/radar_fan.wgsl does not pass naga's validator:\n{}",
                error.emit_to_string(RADAR_FAN_WGSL)
            )
        });
    (module, info)
}

/// The shader's three entry points, with the stage each is compiled at.
const ENTRY_POINTS: [(&str, naga::ShaderStage); 3] = [
    ("vs_main", naga::ShaderStage::Vertex),
    ("fs_main_linear_framebuffer", naga::ShaderStage::Fragment),
    ("fs_main_gamma_framebuffer", naga::ShaderStage::Fragment),
];

/// The binding map wgpu-hal's GLES device builds for this pipeline layout: one
/// counter per binding type, running across the whole layout in group then
/// binding order.
fn binding_map() -> glsl::BindingMap {
    let mut map = glsl::BindingMap::default();
    map.insert(
        naga::ResourceBinding {
            group: 0,
            binding: 0,
        },
        0,
    );
    map.insert(
        naga::ResourceBinding {
            group: 1,
            binding: 0,
        },
        0,
    );
    map.insert(
        naga::ResourceBinding {
            group: 1,
            binding: 1,
        },
        1,
    );
    map.insert(
        naga::ResourceBinding {
            group: 1,
            binding: 2,
        },
        1,
    );
    map
}

/// **The shader is valid WGSL and survives translation to GLSL ES 300.**
///
/// The web arm has no runtime gate in this file — nothing here boots a browser
/// — so this is what says a WebGL2 target is handed something it can compile.
/// It is the check that would have caught the two constructs the design flagged
/// as unexercised in this tree: an `array<vec4<f32>, N>` read from a uniform
/// block with a dynamic index, and an R8Uint texture read by `textureLoad` at
/// an explicit level.
#[test]
fn the_shader_is_valid_wgsl_and_translates_to_webgl2_glsl() {
    let (module, info) = validated();
    let mut found: Vec<(String, naga::ShaderStage)> = module
        .entry_points
        .iter()
        .map(|entry| (entry.name.clone(), entry.stage))
        .collect();
    found.sort();
    let mut expected: Vec<(String, naga::ShaderStage)> = ENTRY_POINTS
        .iter()
        .map(|&(name, stage)| (name.to_owned(), stage))
        .collect();
    expected.sort();
    assert_eq!(
        found, expected,
        "the shader's entry points are not the three the pipeline builds from"
    );

    for (name, stage) in ENTRY_POINTS {
        for is_webgl in [true, false] {
            let options = glsl::Options {
                version: glsl::Version::Embedded {
                    version: 300,
                    is_webgl,
                },
                writer_flags: glsl::WriterFlags::ADJUST_COORDINATE_SPACE
                    | glsl::WriterFlags::FORCE_POINT_SIZE,
                binding_map: binding_map(),
                zero_initialize_workgroup_memory: true,
            };
            let policies = BoundsCheckPolicies {
                index: BoundsCheckPolicy::Unchecked,
                buffer: BoundsCheckPolicy::Unchecked,
                image_load: BoundsCheckPolicy::Unchecked,
                binding_array: BoundsCheckPolicy::Unchecked,
            };
            let pipeline_options = glsl::PipelineOptions {
                shader_stage: stage,
                entry_point: name.to_owned(),
                multiview: None,
            };
            let mut source = String::new();
            let mut writer = glsl::Writer::new(
                &mut source,
                &module,
                &info,
                &options,
                &pipeline_options,
                policies,
            )
            .unwrap_or_else(|e| {
                panic!("`{name}` cannot be set up for GLSL ES 300 (webgl={is_webgl}): {e}")
            });
            writer
                .write()
                .unwrap_or_else(|e| panic!("`{name}` does not translate (webgl={is_webgl}): {e}"));
            drop(writer);
            assert!(
                source.contains("void main()"),
                "`{name}` translated to something with no entry point in it"
            );
        }
    }
}

/// **The per-sweep uniform fits the block size WebGL2 guarantees.**
///
/// The drawn-edge table is a uniform array and not a texture, deliberately: a
/// vertex-stage texture fetch is legal in ES 3.0 and is exercised nowhere in
/// this tree. That choice is only safe while the block fits the floor, and the
/// table is sized by [`MAX_POLAR_RADIALS`] — so a wider cap would silently push
/// it past.
#[test]
fn the_sweep_uniform_fits_the_webgl2_guaranteed_block() {
    let floor = wgpu::Limits::downlevel_webgl2_defaults().max_uniform_buffer_binding_size;
    assert!(
        SWEEP_UNIFORM_BYTES <= floor,
        "one sweep's uniform is {SWEEP_UNIFORM_BYTES} B against WebGL2's \
         guaranteed {floor} B block. The drawn-edge table is a uniform array \
         because a vertex-stage texture fetch is exercised nowhere in this \
         tree; past this floor that trade stops being available."
    );
    assert_eq!(
        EDGE_VEC4S * 2,
        MAX_POLAR_RADIALS,
        "the edge table no longer carries one (lo, hi) pair per radial the \
         producer may declare, so the widest admissible sweep would read its \
         last radials' azimuths off the scalars past the end of the array"
    );
    assert_eq!(SECTORS, MAX_POLAR_RADIALS);
}

/// **The chain arithmetic this module slices by is the producer's own — and
/// both are the TEXTURE's.**
///
/// The store cuts one uploaded buffer into mip levels with [`chain_bytes`];
/// `squallar_radar`'s `CodePlane` fills that buffer with its own. Two spellings
/// of one sum is how a level gets uploaded from the wrong offset — a picture
/// that is plausible at every zoom and wrong at all but one.
///
/// **The third party is wgpu**, and it is the one that was missing until
/// 2026-09-08. A code plane is uploaded as a texture's own mip chain, so the
/// level count and every level's extent are the texture's to decide:
/// `Extent3d::max_mips` and `Extent3d::mip_level_size`, asked here as pure
/// functions so this needs no adapter. The producer ceil-halved instead, and a
/// 720 × 1832 sweep — the commonest shape there is — asked for twelve levels
/// of a texture that admits eleven. `create_texture` refused it, the refusal
/// made the bind group invalid, and every frame afterwards logged
/// *"BindGroup with 'radar fan sweep' label is invalid"* against a pane with
/// no radar on it.
///
/// TAMPER: ceil-halve either `full_mip_levels` or `FanSweep::level_shape` and
/// the wgpu rows below go red at 720 × 1832 and at 17 × 5.
#[test]
fn the_chain_arithmetic_is_the_producers() {
    use squallar_radar::render::codes::{CodePlane, LutKey, full_mip_levels};
    use squallar_radar::types::RadarProduct;

    for (radials, gates) in [(720usize, 1832usize), (720, 1192), (360, 920), (17, 5)] {
        let plane = CodePlane::build(
            radials,
            gates,
            vec![7u8; radials * gates],
            LutKey::identity(RadarProduct::Reflectivity),
            8,
        )
        .expect("reflectivity at a real shape is admissible");
        let levels = full_mip_levels(radials, gates);
        assert_eq!(plane.levels(), levels);
        assert_eq!(
            chain_bytes(radials, gates, levels),
            plane.resident_bytes(),
            "the store's chain arithmetic disagrees with the producer's at \
             {radials}x{gates}"
        );
        // **The texture's own answer.** `RadarFanStore::ensure` creates the
        // plane at `width = gates, height = radials`, so this is that
        // descriptor and not a restatement of the shape.
        let extent = wgpu::Extent3d {
            width: gates as u32,
            height: radials as u32,
            depth_or_array_layers: 1,
        };
        assert_eq!(
            levels as u32,
            extent.max_mips(wgpu::TextureDimension::D2),
            "a {radials}x{gates} plane declares {levels} levels and a texture \
             of that size admits {}; `create_texture` refuses the descriptor \
             and every later `set_bind_group` names an invalid bind group",
            extent.max_mips(wgpu::TextureDimension::D2),
        );
        // And level by level, which is what the upload actually slices.
        let mut geom = geometry(Some(0.5));
        geom.reach_gates = gates as u32;
        let sweep = payload(
            radials,
            gates,
            {
                let mut all = Vec::new();
                for level in 0..levels {
                    all.extend_from_slice(plane.level(level).expect("a declared level").0);
                }
                all
            },
            levels,
            opaque_lut(),
            vec![[0.0, 1.0]; radials],
            geom,
        );
        admit(&sweep).expect("a plane the producer built is one this can upload");
        for level in 0..levels {
            let mine = sweep.level(level).expect("a declared level");
            let (r, g) = sweep.level_shape(level).expect("a declared level");
            let (theirs, pr, pg) = plane.level(level).expect("a declared level");
            assert_eq!(
                (r as usize, g as usize),
                (pr, pg),
                "level {level} at {radials}x{gates}"
            );
            assert_eq!(mine, theirs, "level {level} at {radials}x{gates}");
            // And the extent the texture will hold that level at, which is
            // what `queue.write_texture` copies into. A level one texel wider
            // than its own mip overruns it — the second half of the same
            // defect, and the half a level-count clamp alone would have left.
            let gpu = extent.mip_level_size(level as u32, wgpu::TextureDimension::D2);
            assert_eq!(
                (r, g),
                (gpu.height, gpu.width),
                "level {level} at {radials}x{gates} is {r}x{g} and its mip is \
                 {}x{}",
                gpu.height,
                gpu.width,
            );
        }
    }
}

/// **A hostile or malformed payload is refused, never truncated.**
///
/// A plane that silently dropped radials or gates would draw a sweep nobody
/// measured; one whose buffer was short of its shape would read past the end of
/// a level. Each conjunct is driven on its own.
///
/// **These are the upload's questions and not the payload's self-description.**
/// Whether a payload's own numbers agree with each other is
/// `FanSweep::is_well_formed`'s, asked at the draw fork in `squallar_egui` and
/// counted there; what [`admit`] adds is the resolution caps, a usable gate
/// depth, and the chain length the texture will actually be sliced by.
#[test]
fn a_malformed_payload_is_refused_rather_than_truncated() {
    let geom = |gates: usize| {
        let mut g = geometry(None);
        g.reach_gates = gates as u32;
        g
    };
    let shape = |r: usize, g: usize| {
        admit(&payload(
            r,
            g,
            vec![0; r.saturating_mul(g)],
            1,
            opaque_lut(),
            vec![[0.0, 1.0]; r],
            geom(g.max(1)),
        ))
    };
    assert_eq!(
        shape(4_000, 60),
        Err(FanRefusal::Shape {
            radials: 4_000,
            gates: 60
        }),
        "4,000 radials is past the cap and was not refused"
    );
    assert_eq!(
        shape(360, 60_000),
        Err(FanRefusal::Shape {
            radials: 360,
            gates: 60_000
        }),
        "60,000 gates is past the cap and was not refused"
    );
    assert_eq!(
        shape(0, 60),
        Err(FanRefusal::Shape {
            radials: 0,
            gates: 60
        })
    );
    // The widest admissible shape is admitted, so the refusals above are the
    // cap firing and not the door refusing everything.
    assert!(shape(MAX_POLAR_RADIALS, MAX_POLAR_GATES).is_ok());
    // And the shape one gate inside each cap, so neither bound is off by one
    // in the permissive direction.
    assert!(shape(MAX_POLAR_RADIALS - 1, MAX_POLAR_GATES - 1).is_ok());

    let four_by_five = |codes: usize, lut: Vec<u8>, edges: usize| {
        admit(&payload(
            4,
            5,
            vec![0; codes],
            1,
            lut,
            vec![[0.0, 1.0]; edges],
            geom(5),
        ))
    };
    assert_eq!(
        four_by_five(19, opaque_lut(), 4),
        Err(FanRefusal::CodeBytes { got: 19, want: 20 })
    );
    assert_eq!(
        four_by_five(20, vec![0; 12], 4),
        Err(FanRefusal::LutBytes {
            got: 12,
            want: POLAR_LUT_BYTES
        })
    );
    assert_eq!(
        four_by_five(20, opaque_lut(), 3),
        Err(FanRefusal::EdgeCount { got: 3, want: 4 })
    );

    // A zero slant depth, which is the gate index's divisor.
    let mut zero_slant = geom(5);
    zero_slant.gate_interval_slant_km = 0.0;
    assert_eq!(
        admit(&payload(
            4,
            5,
            vec![0; 20],
            1,
            opaque_lut(),
            vec![[0.0, 1.0]; 4],
            zero_slant
        )),
        Err(FanRefusal::GateInterval(0.0))
    );
    // And a zero GROUND depth, which is the level selection's — a different
    // lane, refused on its own terms. The disc's two ground radii are equal
    // here, so the difference the store reads one gate's depth off is zero.
    let mut flat_disc = geom(5);
    flat_disc.first_gate_km = flat_disc.reach_km;
    assert_eq!(
        admit(&payload(
            4,
            5,
            vec![0; 20],
            1,
            opaque_lut(),
            vec![[0.0, 1.0]; 4],
            flat_disc
        )),
        Err(FanRefusal::GateInterval(0.0))
    );

    // Two sweeps of one callback must name one site: `Locals` holds one.
    let here = fixture(None, opaque_lut());
    let mut elsewhere_geom = geometry(None);
    elsewhere_geom.site_lon = SITE_LON + 1.0;
    let elsewhere = Arc::new(payload(
        RADIALS,
        GATES,
        plane_codes(),
        1,
        opaque_lut(),
        one_degree_edges(),
        elsewhere_geom,
    ));
    let sweeps: Arc<[Arc<FanSweep>]> = Arc::from(vec![Arc::clone(&here), elsewhere]);
    let painter = squallar_gpu::radar_fan::RadarFanBridge;
    let draw = |sweeps: &Arc<[Arc<FanSweep>]>| {
        use squallar_egui::radar_fan::RadarFanPainter as _;
        let v = view(1.0);
        painter
            .payload(squallar_egui::radar_fan::FanDraw {
                sweeps,
                view: squallar_egui::radar_fan::FanView {
                    rect: v.rect,
                    site_px: v.site_pt,
                    world_px: f64::from(v.world_pt),
                    km_per_px: f64::from(v.km_per_pt),
                    pixels_per_point: 1.0,
                },
                opacity: 1.0,
                pass_nr: 1,
            })
            .is_some()
    };
    assert!(
        !draw(&sweeps),
        "two sweeps at two sites became one callback, which would draw the \
         second sweep's gates at the first sweep's radar"
    );
    // The same span with one site is issued, so the refusal above is the site
    // comparison firing and not the bridge declining every span.
    let agreeing: Arc<[Arc<FanSweep>]> = Arc::from(vec![here, fixture(None, opaque_lut())]);
    assert!(draw(&agreeing));
}

/// **The canonical mesh names every `(sector, ring, side)` exactly once, and
/// every index it emits is in the vertex buffer.**
///
/// The mesh is one `u32` a vertex and is never rebuilt, so a packing that
/// aliased two sectors would put one sweep's radial at another's azimuth
/// forever, in a picture that looks like a radar image.
#[test]
fn the_mesh_covers_every_sector_ring_and_side_once() {
    let sweep = fixture(None, opaque_lut());
    let _ = sweep;
    let mut seen = std::collections::HashSet::new();
    for sector in 0..SECTORS as u32 {
        for ring in 0..=RINGS as u32 {
            for side in 0..2u32 {
                assert!(
                    seen.insert(pack_vertex(sector, ring, side)),
                    "the packing aliases ({sector}, {ring}, {side}) onto an \
                     earlier vertex"
                );
            }
        }
    }
    assert_eq!(seen.len(), MESH_VERTICES);
    assert_eq!(MESH_INDICES, SECTORS * RINGS * 6);
}

// ---------------------------------------------------------------------------
// The gate, on the GPU
// ---------------------------------------------------------------------------

/// One arm of the gate criterion: draw the fixture at `elevation_deg` and hold
/// every compared pixel to the table entry for the code planted at the cell the
/// mirror says it sits over.
///
/// Answers `(compared, skipped, unpainted)` so the caller can floor them.
fn assert_every_pixel_reads_its_own_gate(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    elevation_deg: Option<f32>,
) -> (usize, usize, usize) {
    // The gamma framebuffer: the fragment's premultiplied gamma colour reaches
    // the texel unconverted, so over a transparent clear an opaque entry lands
    // in the readback byte for byte.
    let format = wgpu::TextureFormat::Rgba8Unorm;
    let lut = opaque_lut();
    let sweep = fixture(elevation_deg, lut.clone());
    let store = RadarFanStore::new(device, attachments(format));
    let (pixels, renderer) = frame(
        device,
        queue,
        Some(store),
        format,
        wgpu::Color::TRANSPARENT,
        vec![RadarFanCallback::new(vec![Arc::clone(&sweep)], view(1.0), 1).expect("one sweep")],
    );
    assert_eq!(store_of(&renderer).resident_sweeps(), 1);

    let (mut compared, mut skipped, mut unpainted) = (0usize, 0usize, 0usize);
    let mut wrong: Vec<String> = Vec::new();
    for py in 0..SIDE {
        for px in 0..SIDE {
            let got = texel(&pixels, px, py);
            match expect_at(px, py, elevation_deg) {
                Expect::Skip => skipped += 1,
                Expect::Unpainted => {
                    unpainted += 1;
                    if got != [0, 0, 0, 0] && wrong.len() < 8 {
                        wrong.push(format!(
                            "({px},{py}) is outside every gate and is painted {got:?}"
                        ));
                    }
                }
                Expect::Cell { radial, gate } => {
                    compared += 1;
                    let code = code_at(radial, gate);
                    let at = usize::from(code) * 4;
                    let want = [lut[at], lut[at + 1], lut[at + 2], lut[at + 3]];
                    if got != want && wrong.len() < 8 {
                        wrong.push(format!(
                            "({px},{py}) sits over radial {radial} gate {gate}, whose \
                             code {code} is {want:?} in the table, and reads {got:?}"
                        ));
                    }
                }
            }
        }
    }
    assert!(
        wrong.is_empty(),
        "the fan is not drawing the gate the pick would read (elevation \
         {elevation_deg:?}); {compared} pixels compared, {skipped} skipped near \
         a boundary:\n{}",
        wrong.join("\n")
    );
    (compared, skipped, unpainted)
}

/// **THE GATE.** Every pixel of the fan holds the table entry for the code at
/// the cell an independent `f64` solve says it sits over — on a sweep whose
/// ranges are ground ranges, and on one flown at an elevation, where the
/// fragment has to invert the spherical 4/3 beam.
#[test]
#[ignore = "needs a real wgpu adapter"]
fn the_fan_paints_the_gate_the_pick_would_read() {
    let _serialised = gpu_lock();
    let Some((device, queue)) = device() else {
        eprintln!("SKIPPED: no wgpu adapter");
        return;
    };

    for elevation_deg in [None, Some(0.5f32), Some(4.0f32)] {
        let (compared, skipped, unpainted) =
            assert_every_pixel_reads_its_own_gate(&device, &queue, elevation_deg);
        eprintln!(
            "elevation {elevation_deg:?}: {compared} compared, {skipped} skipped, \
             {unpainted} asserted unpainted"
        );
        // The floors. Without them an `expect_at` that answered `Skip`
        // everywhere would satisfy every assertion above by having nothing to
        // check, which is exactly the shape of a vacuous pass.
        assert!(
            compared > 15_000,
            "only {compared} pixels were compared out of {}; the mirror is \
             excluding almost everything and the criterion above is close to \
             vacuous",
            SIDE * SIDE
        );
        assert!(
            unpainted > 10_000,
            "only {unpainted} pixels were asserted to be OUTSIDE the disc. The \
             fixture disc covers well under half the canvas, so a smaller \
             figure means the mirror is not placing it where the shader does"
        );
    }
}

/// **The mirror is a real discriminator**: the same comparison run against a
/// picture built from the wrong cell fails.
///
/// Without this, `the_fan_paints_the_gate_the_pick_would_read` establishes only
/// that two things agree, with no evidence that the comparison could have seen
/// them disagree. This transposes radial and gate in the expectation — the
/// single likeliest way to get the texture axes wrong — and requires the check
/// to reject it.
///
/// That test is `#[ignore]`d and the default row skips it; run it with
/// `cargo test -p squallar-gpu --test radar_fan_gpu -- --ignored`. This one is
/// not, deliberately: it needs no adapter, so the discriminator behind the
/// criterion runs on every board even where the criterion itself cannot.
#[test]
fn the_gate_criterion_rejects_a_transposed_lookup() {
    let lut = opaque_lut();
    let mut differed = 0usize;
    let mut cells = 0usize;
    for py in 0..SIDE {
        for px in 0..SIDE {
            if let Expect::Cell { radial, gate } = expect_at(px, py, None) {
                // The transposition is only defined where the two indices are
                // interchangeable — the fixture is 360 x 200, so the radials
                // past 200 have no gate to swap with and are not part of the
                // denominator either.
                if gate >= RADIALS || radial >= GATES {
                    continue;
                }
                cells += 1;
                let right = usize::from(code_at(radial, gate)) * 4;
                let wrong = usize::from(code_at(gate, radial)) * 4;
                if lut[right..right + 4] != lut[wrong..wrong + 4] {
                    differed += 1;
                }
            }
        }
    }
    assert!(
        cells > 10_000,
        "the mirror found only {cells} cells where the two axes are          interchangeable, so this discriminator is measuring almost nothing"
    );
    assert!(
        differed * 100 > cells * 95,
        "transposing the two axes changes the expected colour at only \
         {differed} of {cells} cells, so the criterion could pass on a shader \
         that read the code plane transposed"
    );
}

// ---------------------------------------------------------------------------
// The two gamma conventions
// ---------------------------------------------------------------------------

/// sRGB gamma 0-1 to linear, `egui.wgsl`'s own piecewise curve.
fn linear_from_gamma(c: f64) -> f64 {
    if c < 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

/// linear 0-1 to sRGB gamma — what the hardware does on a write to an sRGB
/// target, and the inverse of the curve above.
fn gamma_from_linear(c: f64) -> f64 {
    if c <= 0.003_130_8 {
        c * 12.92
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    }
}

/// **Both gamma conventions are drawn, each correctly, and they differ.**
///
/// egui picks its fragment entry point off the target's sRGB-ness; this pass
/// draws into the same render pass and must pick the same one, or every radar
/// pixel is gamma-shifted against the map beneath it. The two are asserted to
/// differ from each other as the interleaved control — over an opaque clear, so
/// the blend really does happen in two different spaces and a comparison that
/// could not see the difference would fail here rather than pass silently.
#[test]
#[ignore = "needs a real wgpu adapter"]
fn both_gamma_conventions_draw_and_they_differ() {
    let _serialised = gpu_lock();
    let Some((device, queue)) = device() else {
        eprintln!("SKIPPED: no wgpu adapter");
        return;
    };

    let lut = translucent_lut();
    let mut readings = Vec::new();
    for format in [
        wgpu::TextureFormat::Rgba8Unorm,
        wgpu::TextureFormat::Rgba8UnormSrgb,
    ] {
        // The clear is the SAME GREY on both arms: a clear colour is given in
        // linear, and an sRGB target encodes it on the way in. Handing both the
        // same number would have put two different greys under the fan and made
        // the "they differ" control pass for a reason unrelated to the shader.
        let clear = if format.is_srgb() {
            linear_from_gamma(CLEAR_GAMMA)
        } else {
            CLEAR_GAMMA
        };
        let sweep = fixture(Some(0.5), lut.clone());
        let store = RadarFanStore::new(&device, attachments(format));
        let (pixels, _) = frame(
            &device,
            &queue,
            Some(store),
            format,
            wgpu::Color {
                r: clear,
                g: clear,
                b: clear,
                a: 1.0,
            },
            vec![RadarFanCallback::new(vec![sweep], view(1.0), 1).expect("one sweep")],
        );
        let clear_byte = [128u8, 128, 128, 255];
        let drew = painted(&pixels, clear_byte);
        assert!(
            drew > 5_000,
            "the {format:?} arm painted only {drew} texels over its clear"
        );
        readings.push(pixels);
    }
    assert!(
        readings[0] != readings[1],
        "the two gamma conventions produced identical readbacks over an opaque \
         clear. The blend is meant to happen in gamma space on one arm and in \
         linear space on the other, so a byte-identical pair means one of the \
         two entry points is not being selected and this comparison cannot see \
         a gamma difference at all"
    );

    // And each arm is right, not merely different, at every pixel the mirror
    // places inside a cell.
    let mut compared = 0usize;
    let mut wrong: Vec<String> = Vec::new();
    for py in 0..SIDE {
        for px in 0..SIDE {
            let Expect::Cell { radial, gate } = expect_at(px, py, Some(0.5)) else {
                continue;
            };
            compared += 1;
            let at = usize::from(code_at(radial, gate)) * 4;
            let alpha = f64::from(lut[at + 3]) / 255.0;
            for channel in 0..3 {
                let src_gamma = f64::from(lut[at + channel]) / 255.0;
                // Premultiplied, in gamma space, exactly as `shade` builds it.
                let src_premultiplied = src_gamma * alpha;
                let gamma_arm = src_premultiplied + CLEAR_GAMMA * (1.0 - alpha);
                let srgb_arm = gamma_from_linear(
                    linear_from_gamma(src_premultiplied)
                        + linear_from_gamma(CLEAR_GAMMA) * (1.0 - alpha),
                );
                for (arm, want, pixels) in [
                    ("gamma", gamma_arm, &readings[0]),
                    ("srgb", srgb_arm, &readings[1]),
                ] {
                    let got = f64::from(texel(pixels, px, py)[channel]);
                    let expected = (want * 255.0).round();
                    if (got - expected).abs() > 2.0 && wrong.len() < 8 {
                        wrong.push(format!(
                            "{arm} arm, ({px},{py}) channel {channel}: expected \
                             {expected} got {got}"
                        ));
                    }
                }
            }
        }
    }
    assert!(
        wrong.is_empty(),
        "{compared} compared:\n{}",
        wrong.join("\n")
    );
    assert!(compared > 15_000, "only {compared} pixels were compared");
}

// ---------------------------------------------------------------------------
// The rest of the store's contract
// ---------------------------------------------------------------------------

/// **An alpha-zero table entry paints nothing, with no discard and no branch.**
///
/// The below-threshold code is unpainted rather than black, and the fragment
/// has no arm for it: an entry at alpha zero premultiplies to `(0,0,0,0)` and
/// contributes exactly nothing under egui's blend. This drives that on a table
/// where every code is transparent, and requires the picture to be the clear.
#[test]
#[ignore = "needs a real wgpu adapter"]
fn an_alpha_zero_table_paints_nothing() {
    let _serialised = gpu_lock();
    let Some((device, queue)) = device() else {
        eprintln!("SKIPPED: no wgpu adapter");
        return;
    };
    let format = wgpu::TextureFormat::Rgba8Unorm;
    let clear = wgpu::Color {
        r: CLEAR_GAMMA,
        g: CLEAR_GAMMA,
        b: CLEAR_GAMMA,
        a: 1.0,
    };
    let clear_byte = [128u8, 128, 128, 255];

    // The control first: the same fixture with an opaque table paints, so the
    // null below is a property of the alphas and not of a fan that never drew.
    let opaque = fixture(None, opaque_lut());
    let store = RadarFanStore::new(&device, attachments(format));
    let (visible, _) = frame(
        &device,
        &queue,
        Some(store),
        format,
        clear,
        vec![RadarFanCallback::new(vec![opaque], view(1.0), 1).expect("one sweep")],
    );
    assert!(
        painted(&visible, clear_byte) > 5_000,
        "the opaque control painted nothing, so the null below means nothing"
    );

    let mut invisible_lut = vec![0u8; POLAR_LUT_BYTES];
    for (code, entry) in invisible_lut.chunks_exact_mut(4).enumerate() {
        entry.copy_from_slice(&[code as u8, 200, 40, 0]);
    }
    let hidden = fixture(None, invisible_lut);
    let store = RadarFanStore::new(&device, attachments(format));
    let (pixels, _) = frame(
        &device,
        &queue,
        Some(store),
        format,
        clear,
        vec![RadarFanCallback::new(vec![hidden], view(1.0), 1).expect("one sweep")],
    );
    assert_eq!(
        painted(&pixels, clear_byte),
        0,
        "a table whose every entry is alpha zero still changed texels, so the \
         premultiply is not being applied and a below-threshold gate would \
         paint black over the map"
    );
}

/// **The opacity uniform scales the whole fan**, which is how a layer's
/// paint-time opacity reaches a callback at all: `Painter::add` cannot tint a
/// `Shape::Callback`.
#[test]
#[ignore = "needs a real wgpu adapter"]
fn the_opacity_uniform_scales_the_fan() {
    let _serialised = gpu_lock();
    let Some((device, queue)) = device() else {
        eprintln!("SKIPPED: no wgpu adapter");
        return;
    };
    let format = wgpu::TextureFormat::Rgba8Unorm;
    let lut = opaque_lut();
    let mut readings = Vec::new();
    for opacity in [1.0f32, 0.5] {
        let sweep = fixture(None, lut.clone());
        let store = RadarFanStore::new(&device, attachments(format));
        let (pixels, _) = frame(
            &device,
            &queue,
            Some(store),
            format,
            wgpu::Color::TRANSPARENT,
            vec![RadarFanCallback::new(vec![sweep], view(opacity), 1).expect("one sweep")],
        );
        readings.push(pixels);
    }
    let mut compared = 0usize;
    for py in 0..SIDE {
        for px in 0..SIDE {
            let Expect::Cell { .. } = expect_at(px, py, None) else {
                continue;
            };
            compared += 1;
            let full = texel(&readings[0], px, py);
            let half = texel(&readings[1], px, py);
            // Premultiplied by half: every channel and the alpha, within the
            // one least significant bit an 8-bit target rounds to.
            for channel in 0..4 {
                let want = f64::from(full[channel]) * 0.5;
                let got = f64::from(half[channel]);
                assert!(
                    (got - want).abs() <= 1.0,
                    "at ({px},{py}) channel {channel}: full {} halves to {want}, \
                     the half-opacity arm reads {got}",
                    full[channel]
                );
            }
        }
    }
    assert!(compared > 15_000, "only {compared} pixels were compared");
}

/// **A sector past the sweep's own radial count draws nothing** — the one
/// mechanism that lets a single canonical mesh serve every sweep shape, and the
/// one that keeps the fragment's `textureLoad` inside the plane's rows.
///
/// A surplus sector's drawn edges are equal, so its two triangles are
/// degenerate and no fragment of one is ever raised. If instead the vertex
/// stage derived an azimuth from the sector index — the obvious alternative,
/// and the one a canonical mesh invites — all 1,440 sectors would draw and an
/// eight-radial sweep would paint a full disc.
///
/// **The table is opaque at code 0 for this fixture alone.** Every other test
/// here leaves code 0 unpainted, which is what a below-threshold gate means;
/// but a surplus sector reads a row past the end of an eight-row plane, and a
/// bounds policy that answers zero there would make the defect invisible
/// through a table whose code 0 is transparent. Painting it is what turns "a
/// sector that should not exist drew" into a visible texel.
#[test]
#[ignore = "needs a real wgpu adapter"]
fn sectors_past_the_sweeps_radials_draw_nothing() {
    let _serialised = gpu_lock();
    let Some((device, queue)) = device() else {
        eprintln!("SKIPPED: no wgpu adapter");
        return;
    };
    let format = wgpu::TextureFormat::Rgba8Unorm;
    // Eight radials of 45 degrees each covers the whole circle, and eight of
    // one degree each covers a fortieth of it. The second is the fixture: what
    // is being checked is that the 1,432 unused sectors are silent.
    let mut lut = opaque_lut();
    lut[0..4].copy_from_slice(&[255, 0, 255, 255]);
    let narrow = Arc::new(payload(
        8,
        GATES,
        vec![64u8; 8 * GATES],
        1,
        lut,
        (0..8).map(|i| [i as f32, i as f32 + 1.0]).collect(),
        geometry(None),
    ));
    admit(&narrow).expect("eight radials is inside every cap");
    let store = RadarFanStore::new(&device, attachments(format));
    let (pixels, _) = frame(
        &device,
        &queue,
        Some(store),
        format,
        wgpu::Color::TRANSPARENT,
        vec![RadarFanCallback::new(vec![narrow], view(1.0), 1).expect("one sweep")],
    );
    let drew = painted(&pixels, [0, 0, 0, 0]);
    assert!(
        drew > 200,
        "the eight-radial fixture painted only {drew} texels, so the null below \
         would hold for a fan that drew nothing at all"
    );
    // Eight one-degree wedges is 8/360 of a disc of radius ~100 px, which is
    // about 700 texels. A tenth of the canvas would be the surplus sectors
    // drawing.
    assert!(
        drew < (SIDE * SIDE / 10) as usize,
        "{drew} texels are painted for a sweep of eight one-degree radials. \
         The canonical mesh carries {SECTORS} sectors and the surplus ones are \
         meant to be degenerate; this many texels means they are not"
    );
}

/// **Six render-pass calls for one sweep, and two more for each extra one —
/// and each draw submits the sweep's own sectors, not the whole mesh.**
///
/// The call count is the reason this path exists rather than a callback per
/// radial. The index count is the other half of a fan's per-frame price and
/// **the picture cannot report it at all**: the canonical mesh carries a sector
/// per radial `MAX_POLAR_RADIALS` admits (1440) and a real sweep declares half
/// that, the surplus sectors' drawn edges are equal, and degenerate triangles
/// paint the same nothing whether they are submitted or not. So a draw that
/// went back to `0..MESH_INDICES` would render an identical frame and every
/// readback in this file would still pass. Both are counted by the store
/// itself, always on, because a claim about a per-frame cost that nothing
/// counts is prose.
///
/// The fixture declares `RADIALS` of `SECTORS`, asserted below: at a sweep
/// filling the mesh the fitted range IS the whole mesh and the assertion could
/// not fail.
///
/// TAMPER: draw `0..MESH_INDICES` and the index rows go red while every pixel
/// assertion in this file stays green.
#[test]
#[ignore = "needs a real wgpu adapter"]
fn the_callback_records_six_calls_and_two_more_per_extra_sweep() {
    let _serialised = gpu_lock();
    let Some((device, queue)) = device() else {
        eprintln!("SKIPPED: no wgpu adapter");
        return;
    };
    let format = wgpu::TextureFormat::Rgba8Unorm;
    for sweeps in 1..=3usize {
        let carried: Vec<_> = (0..sweeps).map(|_| fixture(None, opaque_lut())).collect();
        let store = RadarFanStore::new(&device, attachments(format));
        let (_, renderer) = frame(
            &device,
            &queue,
            Some(store),
            format,
            wgpu::Color::TRANSPARENT,
            vec![RadarFanCallback::new(carried, view(1.0), 1).expect("at least one sweep")],
        );
        let store = store_of(&renderer);
        let (recorded, paints) = store.recorded_calls();
        assert_eq!(paints, 1, "one callback should paint once");
        assert_eq!(
            recorded,
            4 + 2 * sweeps as u64,
            "{sweeps} sweeps recorded {recorded} render-pass calls. A pipeline, \
             a frame bind group, a vertex buffer and an index buffer are the \
             four the span shares; a loop step is meant to cost a bind group \
             and a draw and nothing else"
        );
        assert_eq!(store.resident_sweeps(), sweeps);
        // One ring write for the pass, not one per draw.
        assert_eq!(store.view_writes(), (1, 1));

        // A build failure rather than a row, because it is a statement about
        // two constants: a fixture filling the mesh would make the fitted
        // range the whole mesh and the row below could not fail.
        const { assert!(RADIALS < SECTORS) };
        assert_eq!(
            store.indices_drawn(),
            sweeps as u64 * squallar_gpu::radar_fan::sector_indices(RADIALS as u32) as u64,
            "{sweeps} sweeps of {RADIALS} radials submitted {} indices against              a {MESH_INDICES}-index mesh; a fan draws the sectors its sweep              has radials for and the rest are degenerate by construction",
            store.indices_drawn()
        );
        assert!(
            store.indices_drawn() < sweeps as u64 * MESH_INDICES as u64,
            "the fitted range is the whole mesh, so this row cannot fail"
        );
    }
}

/// **The owner's handle is the whole eviction rule.**
///
/// A sweep dropped by whatever owns it is given back on the next pass, and one
/// still held is not. Nothing here has a budget of its own to disagree with the
/// owner's.
#[test]
#[ignore = "needs a real wgpu adapter"]
fn a_dropped_sweep_is_given_back_on_the_next_pass() {
    let _serialised = gpu_lock();
    let Some((device, queue)) = device() else {
        eprintln!("SKIPPED: no wgpu adapter");
        return;
    };
    let format = wgpu::TextureFormat::Rgba8Unorm;
    let kept = fixture(None, opaque_lut());
    let dropped = fixture(None, opaque_lut());
    let store = RadarFanStore::new(&device, attachments(format));
    let (_, mut renderer) = frame(
        &device,
        &queue,
        Some(store),
        format,
        wgpu::Color::TRANSPARENT,
        vec![
            RadarFanCallback::new(vec![Arc::clone(&kept), Arc::clone(&dropped)], view(1.0), 1)
                .expect("two sweeps"),
        ],
    );
    let before = store_of(&renderer).resident_bytes();
    assert_eq!(store_of(&renderer).resident_sweeps(), 2);
    assert_eq!(
        before,
        (kept.resident_bytes() + dropped.resident_bytes()) as u64
    );

    // The owner lets one go, and a later pass sweeps it.
    let store = renderer
        .callback_resources
        .remove::<RadarFanStore>()
        .expect("the store this frame was drawn with");
    drop(dropped);
    let (_, renderer) = frame(
        &device,
        &queue,
        Some(store),
        format,
        wgpu::Color::TRANSPARENT,
        vec![RadarFanCallback::new(vec![Arc::clone(&kept)], view(1.0), 2).expect("one sweep")],
    );
    assert_eq!(
        store_of(&renderer).resident_sweeps(),
        1,
        "the sweep the owner dropped is still resident, so this store is \
         keeping GPU memory alive past the thing that owns it"
    );
    assert_eq!(
        store_of(&renderer).resident_bytes(),
        kept.resident_bytes() as u64
    );
    // And no second upload for the sweep that stayed.
    assert_eq!(store_of(&renderer).uploads().0, 2);
}

/// **A callback that finds no store draws nothing and says so.**
///
/// The store is keyed by type in `egui_wgpu`'s callback resources, so an
/// install-order slip produces callbacks that decline in silence and a picture
/// nobody can tell from "no data yet". This crate declares no `log` dependency,
/// so the evidence is a counter.
#[test]
#[ignore = "needs a real wgpu adapter"]
fn a_callback_with_no_store_draws_nothing_and_is_counted() {
    let _serialised = gpu_lock();
    let Some((device, queue)) = device() else {
        eprintln!("SKIPPED: no wgpu adapter");
        return;
    };
    let format = wgpu::TextureFormat::Rgba8Unorm;
    let before = squallar_gpu::radar_fan::storeless_callbacks();
    let sweep = fixture(None, opaque_lut());
    // One callback with no store reaches TWO sites that count it: `prepare`,
    // which finds nothing to upload into, and `paint`, which finds nothing to
    // draw out of. Asserted as an exact delta rather than as "it moved",
    // because "it moved" is satisfied by either site alone — one pin covering
    // two routes, and the route that stopped counting would never be noticed.
    let (pixels, _) = frame(
        &device,
        &queue,
        None,
        format,
        wgpu::Color::TRANSPARENT,
        vec![RadarFanCallback::new(vec![sweep], view(1.0), 1).expect("one sweep")],
    );
    assert_eq!(painted(&pixels, [0, 0, 0, 0]), 0);
    assert_eq!(
        squallar_gpu::radar_fan::storeless_callbacks() - before,
        2,
        "one store-less callback should be counted once where it could not \
         upload and once where it could not draw. A different figure means one \
         of the two sites has stopped counting — and with the other still \
         counting, nothing but this exact delta would notice"
    );
}

/// **The mip level is chosen by how much ground a pixel covers — over the
/// WHOLE chain, and the device raises no error uploading it.**
///
/// A zoomed-out fragment must read the strongest echo in its footprint rather
/// than whichever radial happened to be written last, and the mechanism is an
/// explicit level chosen from `km_per_px` against the gate's own ground depth.
/// Every level is planted with a constant code of its own, so the level being
/// read names itself in the colour.
///
/// **The chain is the full one, and that is what this test was missing.** It
/// drove four levels of a 360 × 200 plane until 2026-09-08 — 360, 180, 90, 45
/// and 200, 100, 50, 25, every one of them an exact halving — so it never
/// reached a level where an extent goes odd and never asked a texture for more
/// levels than it admits. A real 720 × 1832 sweep does both at once: it asked
/// for twelve levels of a texture that takes eleven, `create_texture` refused
/// the descriptor, and the invalid bind group that left behind was recorded
/// into the encoder on every frame for the rest of the session. Fifteen GPU
/// suites were green through all of it.
///
/// From level 4 the fixture's own extents go odd (45 → 22, 25 → 12), which is
/// where the fragment's index needs its clamp: gate 199 shifts to 12 where
/// level 4 holds 0..11. Without the clamp `textureLoad` answers zero out of
/// bounds and the outer gates read unpainted, which the constant code makes
/// visible.
///
/// TAMPER: drop the fragment's `min` against `textureDimensions`, or
/// ceil-halve either side of the chain arithmetic, and the deep arms go red.
#[test]
#[ignore = "needs a real wgpu adapter"]
fn the_mip_level_is_chosen_by_the_pixel_footprint() {
    let _serialised = gpu_lock();
    let Some((device, queue)) = device() else {
        eprintln!("SKIPPED: no wgpu adapter");
        return;
    };
    // **Every validation error the device raises, kept.** The symptom this
    // test now covers is a device error and not a wrong pixel: a refused
    // `create_texture` leaves an invalid bind group, and what a running app
    // shows for it is an unbounded log flood beside a pane with no radar.
    let errors: Arc<std::sync::Mutex<Vec<String>>> = Arc::new(std::sync::Mutex::new(Vec::new()));
    let sink = Arc::clone(&errors);
    device.on_uncaptured_error(Arc::new(move |e: wgpu::Error| {
        sink.lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(format!("{e}"));
    }));
    let format = wgpu::TextureFormat::Rgba8Unorm;
    let lut = opaque_lut();

    // The whole chain, each level a constant code of its own.
    let levels = squallar_radar::render::codes::full_mip_levels(RADIALS, GATES);
    assert_eq!(levels, 9, "the fixture shape's full chain");
    let mut codes = Vec::new();
    for level in 0..levels {
        let cells = level_extent(RADIALS, level) * level_extent(GATES, level);
        codes.extend(std::iter::repeat_n(10u8 + level as u8, cells));
    }
    let sweep = Arc::new(payload(
        RADIALS,
        GATES,
        codes,
        levels,
        lut.clone(),
        one_degree_edges(),
        geometry(None),
    ));
    assert!(
        sweep.is_well_formed(),
        "the full-chain fixture describes itself"
    );
    admit(&sweep).expect("the whole chain of the fixture shape");

    // `gate_interval_km` is 1 km here, so `km_per_px` names the level directly:
    // floor(log2(km_per_px)).
    for (km_per_px, level) in [
        (1.0f32, 0usize),
        (2.0, 1),
        (4.0, 2),
        (8.0, 3),
        (16.0, 4),
        (32.0, 5),
        (64.0, 6),
        (128.0, 7),
        (256.0, 8),
    ] {
        let mut at_zoom = view(1.0);
        at_zoom.km_per_pt = km_per_px;
        let store = RadarFanStore::new(&device, attachments(format));
        let (pixels, _) = frame(
            &device,
            &queue,
            Some(store),
            format,
            wgpu::Color::TRANSPARENT,
            vec![RadarFanCallback::new(vec![Arc::clone(&sweep)], at_zoom, 1).expect("one sweep")],
        );
        let code = 10u8 + level as u8;
        let at = usize::from(code) * 4;
        let want = [lut[at], lut[at + 1], lut[at + 2], lut[at + 3]];
        let mut compared = 0usize;
        for py in 0..SIDE {
            for px in 0..SIDE {
                if !matches!(expect_at(px, py, None), Expect::Cell { .. }) {
                    continue;
                }
                compared += 1;
                assert_eq!(
                    texel(&pixels, px, py),
                    want,
                    "at km_per_px {km_per_px} the fan should be reading level \
                     {level}, whose code is {code}"
                );
            }
        }
        assert!(compared > 15_000, "only {compared} pixels were compared");
    }

    // And the clamp: a plane of one level stays on level 0 however far out the
    // view is, with no branch and no cfg. That is what a categorical plane
    // needs.
    let flat = fixture(None, lut.clone());
    let mut far = view(1.0);
    far.km_per_pt = 512.0;
    let store = RadarFanStore::new(&device, attachments(format));
    let (pixels, _) = frame(
        &device,
        &queue,
        Some(store),
        format,
        wgpu::Color::TRANSPARENT,
        vec![RadarFanCallback::new(vec![flat], far, 1).expect("one sweep")],
    );
    let mut compared = 0usize;
    for py in 0..SIDE {
        for px in 0..SIDE {
            let Expect::Cell { radial, gate } = expect_at(px, py, None) else {
                continue;
            };
            compared += 1;
            let at = usize::from(code_at(radial, gate)) * 4;
            assert_eq!(
                texel(&pixels, px, py),
                [lut[at], lut[at + 1], lut[at + 2], lut[at + 3]],
                "a one-level plane read something other than level 0 at \
                 km_per_px 512"
            );
        }
    }
    assert!(compared > 15_000, "only {compared} pixels were compared");

    let raised = errors
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    assert!(
        raised.is_empty(),
        "the device raised {} validation errors uploading and drawing this \
         chain; the first is what the rest are downstream of:\n{}",
        raised.len(),
        raised.join("\n"),
    );
}

/// **The staged chain upload draws the picture the window upload draws, at
/// every level of the chain and for every sweep of a pass.**
///
/// `RadarFanStore::ensure` used to write the whole mip chain with
/// `queue.write_texture` inside `egui_wgpu`'s `prepare`, which runs on the
/// frame thread: a blocking host store per byte through the card's BAR window,
/// once per sweep that becomes resident, and a frame may carry six.
/// `radar_fan::chain_staging` files the sweeps instead and moves the pass out
/// of one cached-memory staging slot with the copy engine, which is what
/// `egui_renderer::texture_upload` does for egui's own rasters.
///
/// A buffer-to-texture copy reads at a stride held to
/// `COPY_BYTES_PER_ROW_ALIGNMENT` where `write_texture` repacks internally, so
/// the staged route pads every row and every level begins at its own offset in
/// the slot. **That padding is the whole of what can be wrong here**, and it is
/// wrong per level and per sweep: a level read at another level's stride is a
/// radar picture whose every radial is sheared along the beam, and a second
/// sweep laid at the first's offset is one pane drawing another pane's codes.
///
/// # What an input needs for this to reach that
///
/// Three properties, asserted below rather than assumed:
///
/// * **more than one mip level** — the suite's long-standing fixture is one
///   level, where there is no second offset to get wrong at all;
/// * **a level-0 row that is not already a multiple of the alignment** — where
///   it is, padding is the identity and the staged route and the window route
///   agree for free;
/// * **more than one sweep in the pass** — one sweep's regions all begin at
///   zero however the accumulation is spelled.
///
/// The under sweep is a constant code at every level, so a top sweep read out
/// of the wrong region of the slot reads *that* rather than something merely
/// different; and it is drawn first, so its own size is what places the sweep
/// every assertion below is about.
///
/// TAMPER: hand `copy_buffer_to_texture` `row_bytes` instead of `padded_row`,
/// or lay every sweep at offset 0, and the level rows go red. Feed both runs
/// the same route and the two-route assertion goes red instead — which is the
/// control that says the comparison is between two routes and not one run
/// against itself.
#[test]
#[ignore = "needs a real wgpu adapter"]
fn the_two_chain_upload_routes_draw_the_same_picture() {
    let _serialised = gpu_lock();
    let Some((device, queue)) = device_with_ring() else {
        eprintln!("SKIPPED: no wgpu adapter");
        return;
    };
    if !squallar_gpu::radar_fan::chain_staging::available(&device) {
        eprintln!("SKIPPED: adapter has no MAPPABLE_PRIMARY_BUFFERS");
        return;
    }
    // Every validation error the device raises, kept: a refused copy is a
    // device error and not a wrong pixel, and what a running app shows for one
    // is a log flood beside a pane with no radar.
    let errors: Arc<std::sync::Mutex<Vec<String>>> = Arc::new(std::sync::Mutex::new(Vec::new()));
    let sink = Arc::clone(&errors);
    device.on_uncaptured_error(Arc::new(move |e: wgpu::Error| {
        sink.lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(format!("{e}"));
    }));
    let format = wgpu::TextureFormat::Rgba8Unorm;
    let lut = opaque_lut();

    let levels = squallar_radar::render::codes::full_mip_levels(RADIALS, GATES);
    assert!(
        levels > 1,
        "a one-level chain has no second level to place, so this suite would \
         pass whatever the offsets were"
    );
    assert_ne!(
        GATES % wgpu::COPY_BYTES_PER_ROW_ALIGNMENT as usize,
        0,
        "a fixture whose level-0 row is already aligned makes the padding an \
         identity and the two routes agree for free"
    );

    /// The under sweep's code, at every level. Outside [`code_at`]'s range and
    /// outside the per-level constants below, so reading it anywhere is
    /// unambiguous.
    const UNDER: u8 = 1;
    /// Radials the top sweep draws. The rest of the circle is the under
    /// sweep's.
    const HALF: usize = RADIALS / 2;
    let under = Arc::new(payload(
        RADIALS,
        GATES,
        vec![UNDER; chain_bytes(RADIALS, GATES, levels)],
        levels,
        lut.clone(),
        one_degree_edges(),
        geometry(None),
    ));

    // Level 0 varies with both axes, so a sheared row is a different colour at
    // almost every pixel; every deeper level is a constant of its own, so the
    // level the view selects is readable without a second mirror.
    let mut codes = plane_codes();
    for level in 1..levels {
        let cells = level_extent(RADIALS, level) * level_extent(GATES, level);
        codes.extend(std::iter::repeat_n(40u8 + level as u8, cells));
    }
    // **The top sweep draws over half the circle and no more**, so the sweep
    // beneath it is visible rather than merely present. Without that, two
    // sweeps laid at one offset would still draw correctly — the second
    // written wins the bytes and the first is invisible under it — and the
    // whole per-sweep half of this suite would be an identity.
    let half: Vec<[f32; 2]> = one_degree_edges()
        .into_iter()
        .enumerate()
        .map(|(radial, edge)| if radial < HALF { edge } else { [0.0, 0.0] })
        .collect();
    let over = Arc::new(payload(
        RADIALS,
        GATES,
        codes,
        levels,
        lut.clone(),
        half,
        geometry(None),
    ));
    for sweep in [&under, &over] {
        assert!(sweep.is_well_formed(), "the fixture describes itself");
        admit(sweep).expect("the fixture is inside every cap");
    }
    let carried = 2 * (chain_bytes(RADIALS, GATES, levels) + POLAR_LUT_BYTES) as u64;

    // `gate_interval_km` is 1 km here, so `km_per_px` names the level:
    // floor(log2(km_per_px)).
    for (km_per_px, level) in [
        (1.0f32, 0usize),
        (2.0, 1),
        (4.0, 2),
        (8.0, 3),
        (16.0, 4),
        (32.0, 5),
        (64.0, 6),
        (128.0, 7),
        (256.0, 8),
    ] {
        let mut at_zoom = view(1.0);
        at_zoom.km_per_pt = km_per_px;
        let mut shots = Vec::new();
        for staged in [true, false] {
            let store = RadarFanStore::with_staging(&device, attachments(format), staged);
            let (pixels, renderer) = frame(
                &device,
                &queue,
                Some(store),
                format,
                wgpu::Color::TRANSPARENT,
                vec![
                    RadarFanCallback::new(vec![Arc::clone(&under), Arc::clone(&over)], at_zoom, 1)
                        .expect("two sweeps"),
                ],
            );
            let totals = store_of(&renderer).chain_staging();
            // One pass, both sweeps, one route — which is what says the two
            // readbacks below really came from two different routes and not
            // from one run compared with itself.
            assert_eq!(
                (totals.staged, totals.declined),
                if staged { (1, 0) } else { (0, 1) },
                "the store took the other route at km_per_px {km_per_px}, \
                 staged={staged}"
            );
            assert_eq!(
                totals.bytes,
                if staged { carried } else { 0 },
                "the ring carried something other than both chains and both \
                 tables at km_per_px {km_per_px}"
            );
            shots.push(pixels);
        }
        assert_eq!(
            shots[0], shots[1],
            "the staged and window routes drew different pictures at \
             km_per_px {km_per_px}"
        );

        // And the picture is the right one, not merely the same one twice.
        let entry = |code: u8| {
            let at = usize::from(code) * 4;
            [lut[at], lut[at + 1], lut[at + 2], lut[at + 3]]
        };
        let (mut over_px, mut under_px) = (0usize, 0usize);
        for py in 0..SIDE {
            for px in 0..SIDE {
                let Expect::Cell { radial, gate } = expect_at(px, py, None) else {
                    continue;
                };
                let want = if radial < HALF {
                    over_px += 1;
                    if level == 0 {
                        entry(code_at(radial, gate))
                    } else {
                        entry(40u8 + level as u8)
                    }
                } else {
                    // The top sweep draws nothing over this half, so what is
                    // on the glass is the sweep laid FIRST in the pass — and
                    // it is one code at every level.
                    under_px += 1;
                    entry(UNDER)
                };
                assert_eq!(
                    texel(&shots[0], px, py),
                    want,
                    "at km_per_px {km_per_px} the fan should be reading level \
                     {level} of the {} sweep",
                    if radial < HALF { "top" } else { "under" }
                );
            }
        }
        assert!(
            over_px > 7_000 && under_px > 7_000,
            "only {over_px} top and {under_px} under pixels were compared"
        );
    }

    let raised = errors
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    assert!(
        raised.is_empty(),
        "the device raised {} error(s) staging a chain: {}",
        raised.len(),
        raised.join("; ")
    );
}
