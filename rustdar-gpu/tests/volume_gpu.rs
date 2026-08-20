//! What only a real GPU can say about the volume raymarch.
//!
//! Everything here is `#[ignore]`d so a checkout on a box with no working
//! Vulkan loader still gives a green `cargo test`; CI's `gpu` job opts back in
//! on Mesa's lavapipe. The adapter is named once per process on stderr, which
//! is what `--nocapture` is for — an adapter that turned out to be a real GPU
//! would leave the job green having tested something else.
//!
//! ```text
//! cargo test -p rustdar-gpu --test volume_gpu -- --ignored --nocapture
//! ```
//!
//! **These tests hold a process-wide lock and run one at a time**, whatever
//! `--test-threads` says: four devices on one adapter each blocking in
//! `poll(wait_indefinitely)` deadlocked reproducibly. Serialised rather than
//! sharing one device because error scopes are a per-device stack.
//!
//! The adapter, the readback and the planted fixtures live in `gpu_harness`,
//! shared with `volume_shader_mutants.rs` — the battery that proves these tests
//! can fail at all, over these same fixtures.
#![cfg(not(target_arch = "wasm32"))]

use egui_wgpu::wgpu;
use rustdar_device_profile::constants::VOLUME_LUT_BYTES;
use rustdar_volumetric::raymarch::{
    CoarseLevel, ENTRY_FS_BLIT_GAMMA, ENTRY_FS_BLIT_LINEAR, GRID_BYTES_PER_CELL, GRID_MIP_LEVELS,
    OffscreenTarget, VolumePipelines, mirror_is_gamma_encoded,
};
use rustdar_volumetric::uniform::{ISO_OFF, VolumeUniform};

mod gpu_harness;
use gpu_harness::{
    MIRROR_FORMAT, attachments, box_from_clip_down, centre, device, equatorial_box_km,
    equatorial_floor_lanes, eye_outside, gpu_lock, grey_ramp_lut, iso_uniform, mercator_y,
    opaque_white_lut, palette, planted_mirror, raymarch_once, raymarch_once_at,
    raymarch_once_with_floor, read_back, render_target, slab_ramp,
};

/// Open a pass that clears to opaque black, which is what `EguiRenderer::draw`
/// does.
macro_rules! clearing_pass {
    ($encoder:expr, $view:expr) => {
        $encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("rustdar.volume.test.pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: $view,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                    store: wgpu::StoreOp::Store,
                },
                depth_slice: None,
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        })
    };
}

/// Both pipelines build, on both surface colour spaces, with no device error.
#[test]
#[ignore = "needs a real wgpu adapter; see the doc comment for the invocation"]
fn the_pipelines_build_on_a_real_device() {
    let _serialised = gpu_lock();
    let (device, queue) = device();

    for format in [
        wgpu::TextureFormat::Bgra8Unorm,
        wgpu::TextureFormat::Rgba8UnormSrgb,
    ] {
        let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
        let pipelines = VolumePipelines::new(&device, attachments(format));
        pipelines.upload_quad(&queue);
        let error = pollster::block_on(scope.pop());
        assert!(
            error.is_none(),
            "building the volume pipelines for a {format:?} surface failed: {}",
            error.map(|e| e.to_string()).unwrap_or_default()
        );

        let expected = if format.is_srgb() {
            ENTRY_FS_BLIT_LINEAR
        } else {
            ENTRY_FS_BLIT_GAMMA
        };
        assert_eq!(pipelines.blit_entry_point(), expected);
    }
}

/// An offscreen is reused at the same size and rebuilt at a new one.
#[test]
#[ignore = "needs a real wgpu adapter; see the doc comment for the invocation"]
fn an_offscreen_is_reused_at_one_size_and_rebuilt_at_another() {
    let _serialised = gpu_lock();
    let (device, queue) = device();
    let pipelines = VolumePipelines::new(&device, attachments(wgpu::TextureFormat::Bgra8Unorm));
    pipelines.upload_quad(&queue);

    let mut held: Option<OffscreenTarget> = None;
    assert!(
        pipelines.ensure_offscreen(&device, &mut held, [1440, 900]),
        "nothing held must be built"
    );
    assert_eq!(held.as_ref().map(OffscreenTarget::size), Some([1440, 900]));

    assert!(
        !pipelines.ensure_offscreen(&device, &mut held, [1440, 900]),
        "an offscreen of exactly the right size was thrown away and rebuilt, \
         which is a pane-sized allocation on every frame"
    );
    assert_eq!(held.as_ref().map(OffscreenTarget::size), Some([1440, 900]));

    assert!(
        pipelines.ensure_offscreen(&device, &mut held, [720, 450]),
        "a resized pane reused its old offscreen, so the blit would upscale \
         the wrong texture"
    );
    assert_eq!(held.as_ref().map(OffscreenTarget::size), Some([720, 450]));

    // A pane dragged to nothing: the clamp is what stops `create_texture`
    // refusing a zero extent, from a call with no `Result`.
    assert!(pipelines.ensure_offscreen(&device, &mut held, [0, 0]));
    assert_eq!(held.as_ref().map(OffscreenTarget::size), Some([1, 1]));
}

/// A grid of one palette index paints that entry's own colour back out.
#[test]
#[ignore = "needs a real wgpu adapter; see the doc comment for the invocation"]
fn a_uniform_grid_paints_its_palette_colour() {
    let _serialised = gpu_lock();
    const INDEX: u8 = 200;
    const COLOUR: [u8; 4] = [200, 60, 30, 255];
    let size = [64, 64];
    let cells = [8u32, 8, 8];

    let (device, queue) = device();
    let pipelines = VolumePipelines::new(&device, attachments(wgpu::TextureFormat::Bgra8Unorm));
    pipelines.upload_quad(&queue);

    let mut uniform = VolumeUniform::new([10.0, 10.0, 10.0], cells);
    uniform.box_from_clip = box_from_clip_down(2);
    uniform.eye_in_box = eye_outside(2);
    // Enough extinction that a 10 km path is opaque, so the colour is the
    // table's own rather than a blend with the transparent background.
    uniform.extinction_per_km = 1.0;

    let filled = vec![INDEX; (cells[0] * cells[1] * cells[2]) as usize];
    let lut = palette(INDEX, COLOUR);

    for gradient_shading in [false, true] {
        uniform.gradient_shading = gradient_shading;
        let pixels = raymarch_once(
            &device, &queue, &pipelines, cells, &filled, &lut, &uniform, size,
        );
        let painted = centre(&pixels, size);

        assert!(
            painted[3] >= 253,
            "a 10 km path through a fully opaque palette entry came back at \
             alpha {} with gradient_shading={gradient_shading}",
            painted[3]
        );
        for channel in 0..3 {
            let delta = i32::from(painted[channel]) - i32::from(COLOUR[channel]);
            assert!(
                delta.abs() <= 2,
                "channel {channel} came back {} against the table's {} \
                 (gradient_shading={gradient_shading}); the decode/encode round \
                 trip is not a round trip",
                painted[channel],
                COLOUR[channel]
            );
        }
    }

    // The other half: nothing at all, rather than black.
    uniform.gradient_shading = false;
    let empty = vec![0u8; (cells[0] * cells[1] * cells[2]) as usize];
    let pixels = raymarch_once(
        &device, &queue, &pipelines, cells, &empty, &lut, &uniform, size,
    );
    assert_eq!(
        centre(&pixels, size),
        [0, 0, 0, 0],
        "an all-index-0 grid painted something. Index 0 is the bottom of the \
         ramp and the no-data value, so it must contribute nothing — an opaque \
         black box would hide every pane behind it."
    );
}

/// The isosurface mode paints one opaque, lit surface at the threshold, and
/// it reads the DATA, not the table's alpha.
#[test]
#[ignore = "needs a real wgpu adapter; see the doc comment for the invocation"]
fn an_isosurface_paints_an_opaque_lit_surface_from_the_data_alone() {
    let _serialised = gpu_lock();
    const INDEX: u8 = 200;
    // Zero alpha on purpose — see the doc comment.
    const COLOUR: [u8; 4] = [200, 60, 30, 0];
    let size = [64, 64];
    let cells = [8u32, 8, 8];

    let (device, queue) = device();
    let pipelines = VolumePipelines::new(&device, attachments(wgpu::TextureFormat::Bgra8Unorm));
    pipelines.upload_quad(&queue);

    let mut uniform = VolumeUniform::new([10.0, 10.0, 10.0], cells);
    uniform.box_from_clip = box_from_clip_down(2);
    uniform.eye_in_box = eye_outside(2);
    uniform.extinction_per_km = 1.0;

    let filled = vec![INDEX; (cells[0] * cells[1] * cells[2]) as usize];
    let lut = palette(INDEX, COLOUR);

    // Lit volume over a zero-alpha entry: nothing at all.
    uniform.iso_threshold = rustdar_volumetric::uniform::ISO_OFF;
    let pixels = raymarch_once(
        &device, &queue, &pipelines, cells, &filled, &lut, &uniform, size,
    );
    assert_eq!(
        centre(&pixels, size),
        [0, 0, 0, 0],
        "the lit volume painted a zero-alpha entry; the discriminator is dead",
    );

    // Isosurface at a threshold under the filled index: an opaque, lit
    // surface, whatever the table's alpha says.
    uniform.iso_threshold = 150.0 / 255.0;
    uniform.iso_centre = -1.0;
    let pixels = raymarch_once(
        &device, &queue, &pipelines, cells, &filled, &lut, &uniform, size,
    );
    let painted = centre(&pixels, size);
    assert_eq!(
        painted[3], 255,
        "an isosurface hit must be fully opaque, got alpha {}",
        painted[3],
    );
    for channel in 0..3 {
        let full = f64::from(COLOUR[channel]);
        let got = f64::from(painted[channel]);
        // Gamma-space bound of the linear [0.35, 1] lighting window, with a
        // couple of counts of slack for the 8-bit round trips.
        let floor = 255.0
            * (full / 255.0f64)
                .powf(2.2)
                .mul_add(0.33, 0.0)
                .powf(1.0 / 2.2)
            - 3.0;
        assert!(
            got >= floor.max(0.0) && got <= full + 3.0,
            "channel {channel} came back {got} against the entry's {full}: \
             outside the lit window [{floor:.0}, {full}]",
        );
    }

    // And a threshold above the filled index finds no surface at all.
    uniform.iso_threshold = 220.0 / 255.0;
    let pixels = raymarch_once(
        &device, &queue, &pipelines, cells, &filled, &lut, &uniform, size,
    );
    assert_eq!(
        centre(&pixels, size),
        [0, 0, 0, 0],
        "a threshold above every value in the grid still painted a surface",
    );
}

/// Every grey level in the middle of the image, as `(min, max)`.
fn grey_span(pixels: &[[u8; 4]], size: [u32; 2]) -> (u8, u8) {
    let (mut lo, mut hi) = (u8::MAX, u8::MIN);
    for y in size[1] / 4..size[1] * 3 / 4 {
        for x in size[0] / 4..size[0] * 3 / 4 {
            let p = pixels[(y * size[0] + x) as usize];
            assert_eq!(p[3], 255, "the isosurface must be opaque at ({x}, {y})");
            lo = lo.min(p[0]);
            hi = hi.max(p[0]);
        }
    }
    (lo, hi)
}

/// The isosurface sits where the value crosses the threshold, not where the
/// sample comb happened to notice — which is what `refine_iso_hit`'s bisection
/// is for, and what the one shipped isosurface test could not see.
#[test]
#[ignore = "needs a real wgpu adapter; see the doc comment for the invocation"]
fn an_isosurface_sits_where_the_value_crosses_not_where_the_comb_noticed() {
    let _serialised = gpu_lock();
    let size = [64, 64];
    // Slab 3 is met first (index 40) and slab 0 last (index 208): 56 index
    // units per slab, which is the amplitude of the speckle an unrefined hit
    // would produce.
    let (cells, indices) = slab_ramp(&[208, 152, 96, 40]);
    let lut = grey_ramp_lut();

    let (device, queue) = device();
    let pipelines = VolumePipelines::new(&device, attachments(wgpu::TextureFormat::Bgra8Unorm));
    pipelines.upload_quad(&queue);

    let mut uniform = iso_uniform(cells);
    uniform.iso_centre = ISO_OFF;
    // A threshold three tenths of the way from slab 2's 96 to slab 1's 152, so
    // the crossing sits well inside a step and cannot coincide with the comb.
    const THRESHOLD: u8 = 113;
    uniform.iso_threshold = f32::from(THRESHOLD) / 255.0;

    let pixels = raymarch_once(
        &device, &queue, &pipelines, cells, &indices, &lut, &uniform, size,
    );
    let (lo, hi) = grey_span(&pixels, size);
    assert!(
        u32::from(hi) - u32::from(lo) <= 4,
        "the surface came back as speckle spanning [{lo}, {hi}] of grey: the \
         hit was taken at whatever jittered sample noticed the crossing, not \
         at the crossing",
    );
    let level = i32::from(lo) + (i32::from(hi) - i32::from(lo)) / 2;
    assert!(
        (level - i32::from(THRESHOLD)).abs() <= 4,
        "the surface is drawn at index {level}, where the field crosses the \
         threshold at {THRESHOLD}",
    );
}

/// A diverging isosurface is the level set of the **deviation** from its
/// centre, so it draws both lobes — which is what `iso_field`'s fold is for,
/// and what nothing measured.
#[test]
#[ignore = "needs a real wgpu adapter; see the doc comment for the invocation"]
fn a_diverging_isosurface_draws_both_lobes_of_its_own_field() {
    let _serialised = gpu_lock();
    let size = [64, 64];
    const CENTRE: u8 = 128;
    const DEVIATION: u8 = 34;
    let lut = grey_ramp_lut();

    let (device, queue) = device();
    let pipelines = VolumePipelines::new(&device, attachments(wgpu::TextureFormat::Bgra8Unorm));
    pipelines.upload_quad(&queue);

    // Slab 3 is met first and holds the centre exactly, so neither ramp can
    // cross at the box's entry face — the degenerate hit the shipped test
    // takes.
    for (levels, lobe, expected) in [
        ([68u8, 88, 108, CENTRE], "the low lobe", CENTRE - DEVIATION),
        ([188, 168, 148, CENTRE], "the high lobe", CENTRE + DEVIATION),
    ] {
        let (cells, indices) = slab_ramp(&levels);
        let mut uniform = iso_uniform(cells);
        uniform.iso_centre = f32::from(CENTRE) / 255.0;
        uniform.iso_threshold = f32::from(DEVIATION) / 255.0;

        let pixels = raymarch_once(
            &device, &queue, &pipelines, cells, &indices, &lut, &uniform, size,
        );
        let (lo, hi) = grey_span(&pixels, size);
        let level = i32::from(lo) + (i32::from(hi) - i32::from(lo)) / 2;
        assert!(
            (level - i32::from(expected)).abs() <= 5,
            "{lobe}: the surface is drawn at index {level} (span [{lo}, {hi}]), \
             where |value \u{2212} {CENTRE}| reaches {DEVIATION} at {expected}. \
             An index read straight through would put it at {CENTRE}.",
        );
        assert!(
            (level - i32::from(CENTRE)).abs() > 10,
            "{lobe}: the surface sits on the centre index itself, which is \
             where a threshold read against the raw index rather than against \
             the deviation would put it",
        );
    }
}

/// The isosurface excludes unmeasured air — the one contract
/// `COVERAGE_FLOOR` exists for, and the one nothing measured.
#[test]
#[ignore = "needs a real wgpu adapter; see the doc comment for the invocation"]
fn an_isosurface_excludes_unmeasured_air() {
    let _serialised = gpu_lock();
    let size = [64, 64];
    let lut = grey_ramp_lut();

    let (device, queue) = device();
    let pipelines = VolumePipelines::new(&device, attachments(wgpu::TextureFormat::Bgra8Unorm));
    pipelines.upload_quad(&queue);

    // `slab_ramp`'s slab 0 is met LAST, so the two zeros at the end of each
    // level list are the air the ray enters through. Slab 3 holds the centre
    // exactly, so no ramp can cross at the air/data interface itself.
    for (levels, shape, expected) in [
        (
            // A velocity-like couplet: centre mid-ramp, data falling away from
            // it. |0 - 128| = 128 is nearly four times the threshold.
            [68u8, 88, 108, 128, 0, 0],
            "a diverging centre mid-ramp",
            94i32,
        ),
        (
            // ρHV's shape: the centre at the top of the ramp, so air is the
            // most extreme reading the fold can return — |0 - 250| = 250.
            [160u8, 200, 230, 250, 0, 0],
            "a centre at the top of its ramp",
            190,
        ),
    ] {
        let centre = levels[3];
        let deviation = centre - u8::try_from(expected).expect("in range");
        let (cells, indices) = slab_ramp(&levels);
        let mut uniform = iso_uniform(cells);
        uniform.iso_centre = f32::from(centre) / 255.0;
        uniform.iso_threshold = f32::from(deviation) / 255.0;

        let pixels = raymarch_once(
            &device, &queue, &pipelines, cells, &indices, &lut, &uniform, size,
        );
        // `grey_span` asserts opacity, which is the first half of the claim:
        let (lo, hi) = grey_span(&pixels, size);
        let level = i32::from(lo) + (i32::from(hi) - i32::from(lo)) / 2;
        println!(
            "{shape}: surface at index {level} (span [{lo}, {hi}]); \
             air would read {}",
            0,
        );
        assert!(
            (level - expected).abs() <= 5,
            "{shape}: the surface is drawn at index {level} (span [{lo}, \
             {hi}]), where |value \u{2212} {centre}| reaches {deviation} at \
             {expected}",
        );
        assert!(
            level > 20,
            "{shape}: the surface is drawn at index {level}, which is the \
             no-data index the two air slabs in front of the data hold. \
             `iso_hit_test` is taking its hit in unmeasured air: either the \
             coverage term is gone or COVERAGE_FLOOR has stopped excluding \
             air, and every diverging product's surface has collapsed onto \
             the coverage cone",
        );
    }
}

/// The isosurface keeps features narrower than the smoothing kernel — at the
/// rung the region boxes actually ship.
#[test]
#[ignore = "needs a real wgpu adapter; see the doc comment for the invocation"]
fn an_isosurface_at_the_shipped_rung_keeps_its_sub_kernel_features() {
    let _serialised = gpu_lock();
    let size = [128u32, 128];
    let cells = [16u32, 16, 16];
    let lut = grey_ramp_lut();

    let (device, queue) = device();
    let pipelines = VolumePipelines::new(&device, attachments(wgpu::TextureFormat::Bgra8Unorm));
    pipelines.upload_quad(&queue);

    let empty = vec![0u8; (cells[0] * cells[1] * cells[2]) as usize];
    // One measured cell in the middle of empty air.
    let mut lone_voxel = empty.clone();
    lone_voxel[((8 * cells[1] + 8) * cells[0] + 8) as usize] = 255;
    // One measured slab, one cell thick, spanning the whole horizontal extent
    // — the bright band's shape, and the fill the eye of `eye_outside(2)` sees
    // across the entire frame.
    let mut sheet = empty;
    let plane = (cells[0] * cells[1]) as usize;
    sheet[8 * plane..9 * plane].fill(255);

    // The shipped isosurface configuration: the cloud rung's step density,
    // which the bridge does send, and the raw reconstruction, which is the
    // exemption under test.
    let mut uniform = iso_uniform(cells);
    uniform.iso_centre = ISO_OFF;
    uniform.iso_threshold = 100.0 / 255.0;
    uniform.step_cells = rustdar_volumetric::bridge::CLOUD_STEP_CELLS;

    let painted = |indices: &[u8], uniform: &VolumeUniform| {
        raymarch_once(
            &device, &queue, &pipelines, cells, indices, &lut, uniform, size,
        )
        .iter()
        .filter(|px| px[3] > 0)
        .count()
    };

    let raw = (painted(&lone_voxel, &uniform), painted(&sheet, &uniform));
    uniform.reconstruction_lod = rustdar_volumetric::bridge::CLOUD_RECONSTRUCTION_LOD;
    let smoothed = (painted(&lone_voxel, &uniform), painted(&sheet, &uniform));
    println!(
        "isosurface at threshold 100/255, {}x{} px:\n  \
         lone voxel:   LOD 0 {} px, LOD {} {} px\n  \
         1-cell sheet: LOD 0 {} px, LOD {} {} px",
        size[0],
        size[1],
        raw.0,
        rustdar_volumetric::bridge::CLOUD_RECONSTRUCTION_LOD,
        smoothed.0,
        raw.1,
        rustdar_volumetric::bridge::CLOUD_RECONSTRUCTION_LOD,
        smoothed.1,
    );

    assert!(
        raw.0 > 0,
        "the shipped isosurface configuration paints nothing for a lone \
         measured voxel: a narrow hail core or updraft tip is absent from the \
         3D surface while the 2D pane shows it",
    );
    assert!(
        raw.1 > 0,
        "the shipped isosurface configuration paints nothing for a one-cell \
         sheet: a bright band or TDS shell is absent from the 3D surface",
    );
    assert!(
        raw.1 > raw.0 * 4,
        "the one-cell sheet ({} px) is not substantially larger than the lone \
         voxel ({} px), so the sheet fixture is not spanning the frame and \
         the erasure measurement below has nothing to bite on",
        raw.1,
        raw.0,
    );
    // The erasure the exemption exists for, measured rather than argued.
    assert_eq!(
        smoothed.0, 0,
        "at the region rungs' reconstruction level a lone measured voxel now \
         survives the {} coverage cut ({} px). That is the premise \
         `volume::bridge`'s isosurface exemption rests on, so if it has \
         changed the exemption's reasoning must be rewritten — not the \
         assertion relaxed",
        0.5, smoothed.0,
    );
    assert!(
        smoothed.1 * 4 < raw.1,
        "at the region rungs' reconstruction level the one-cell sheet keeps \
         {} px of its {} — the 0.502 coverage of a half-filled coarse texel \
         is no longer being cut by the 0.5 floor, so `volume::bridge`'s \
         isosurface exemption is resting on a premise that has changed",
        smoothed.1,
        raw.1,
    );
}

/// Opacity is per kilometre travelled, not per box diagonal.
#[test]
#[ignore = "needs a real wgpu adapter; see the doc comment for the invocation"]
fn opacity_accumulates_per_kilometre_not_per_box_diagonal() {
    let _serialised = gpu_lock();
    const INDEX: u8 = 200;
    const EXTINCTION_PER_KM: f32 = 0.01;
    let box_size_km = [240.0f32, 240.0, 20.0];
    let size = [64, 64];
    let cells = [8u32, 8, 8];

    let (device, queue) = device();
    let pipelines = VolumePipelines::new(&device, attachments(wgpu::TextureFormat::Bgra8Unorm));
    pipelines.upload_quad(&queue);

    let filled = vec![INDEX; (cells[0] * cells[1] * cells[2]) as usize];
    let lut = palette(INDEX, [255, 255, 255, 255]);

    let mut alphas = [0.0f64; 3];
    for axis in 0..3 {
        let mut uniform = VolumeUniform::new(box_size_km, cells);
        uniform.box_from_clip = box_from_clip_down(axis);
        uniform.eye_in_box = eye_outside(axis);
        uniform.extinction_per_km = EXTINCTION_PER_KM;
        // Shading would multiply the colour, not the alpha, but leave it off so
        // the only thing under test is path length.
        uniform.gradient_shading = false;

        let pixels = raymarch_once(
            &device, &queue, &pipelines, cells, &filled, &lut, &uniform, size,
        );
        let measured = f64::from(centre(&pixels, size)[3]) / 255.0;
        let expected = 1.0 - (-f64::from(EXTINCTION_PER_KM) * f64::from(box_size_km[axis])).exp();
        assert!(
            (measured - expected).abs() < 0.01,
            "a ray down axis {axis} crosses {} km and should reach alpha \
             {expected:.4}; it reached {measured:.4}. `dt * length(box_size_km)` \
             would give every axis 0.9666.",
            box_size_km[axis]
        );
        alphas[axis] = measured;
    }

    // And the relative distortion, stated as the thing that reads as haze: the
    // ratio of optical depths must be the box's aspect ratio, 12.
    let optical_depth = |alpha: f64| -(1.0 - alpha).ln();
    let anisotropy = optical_depth(alphas[0]) / optical_depth(alphas[2]);
    assert!(
        (11.0..13.0).contains(&anisotropy),
        "a horizontal ray is {anisotropy:.1}x deeper than a vertical one; the \
         box is 12x wider than it is deep, so that is the figure"
    );
}

/// The blit composites exactly what egui would, on both surface colour spaces.
#[test]
#[ignore = "needs a real wgpu adapter; see the doc comment for the invocation"]
fn the_map_floor_stands_under_the_volume_and_only_when_asked() {
    let _serialised = gpu_lock();
    let size = [128u32, 128];
    let cells = [16u32, 16, 16];

    let (device, queue) = device();
    let pipelines = VolumePipelines::new(&device, attachments(wgpu::TextureFormat::Bgra8Unorm));
    pipelines.upload_quad(&queue);

    // Red top half of the mirror, blue bottom half. Opaque, so premultiplied
    // and straight are the same four bytes.
    let mirror_side = 8usize;
    let mut mirror_rgba = Vec::with_capacity(mirror_side * mirror_side * 4);
    for row in 0..mirror_side {
        for _col in 0..mirror_side {
            if row < mirror_side / 2 {
                mirror_rgba.extend_from_slice(&[255, 0, 0, 255]);
            } else {
                mirror_rgba.extend_from_slice(&[0, 0, 255, 255]);
            }
        }
    }
    let floor = planted_mirror(
        &device,
        &queue,
        &pipelines,
        [mirror_side as u32, mirror_side as u32],
        &mirror_rgba,
    );

    let empty = vec![0u8; (cells[0] * cells[1] * cells[2]) as usize];
    let mut lut = vec![0u8; VOLUME_LUT_BYTES];
    for entry in 1..lut.len() / 4 {
        lut[entry * 4..entry * 4 + 4].copy_from_slice(&[255, 255, 255, 255]);
    }

    // Looking down the z axis: image rows run from the box's north (top) to
    // south, columns west to east.
    let mut uniform = VolumeUniform::new(equatorial_box_km(), cells);
    uniform.box_from_clip = box_from_clip_down(2);
    uniform.eye_in_box = eye_outside(2);
    uniform.extinction_per_km = 1000.0;
    uniform.gradient_shading = false;
    // The footprint over the whole mirror, north edge on row 0 — the
    // correspondence the assertions below are written in terms of, established
    // through the reprojection rather than assumed of it.
    let (floor_uv, floor_geo) = equatorial_floor_lanes(floor.is_gamma_encoded());
    uniform.floor_uv = floor_uv;
    uniform.floor_geo = floor_geo;

    // 1. Bound but not asked for: the flag is the gate, not the binding.
    let pixels = raymarch_once_with_floor(
        &device, &queue, &pipelines, cells, &empty, &lut, &uniform, size, &floor,
    );
    assert!(
        pixels.iter().all(|px| *px == [0, 0, 0, 0]),
        "a mirror bound at group 1 painted with map_floor off; the shader has \
         lost its flags.w gate and every mask instrument now stands on ground",
    );

    // 2 + 3. Asked for, over an empty grid: the footprint is ground, opaque,
    // and the right way up.
    uniform.map_floor = true;
    let pixels = raymarch_once_with_floor(
        &device, &queue, &pipelines, cells, &empty, &lut, &uniform, size, &floor,
    );
    let top = pixels[(size[1] / 4 * size[0] + size[0] / 2) as usize];
    let bottom = pixels[(3 * size[1] / 4 * size[0] + size[0] / 2) as usize];
    assert_eq!(top[3], 255, "the floor must be opaque ground");
    assert!(
        top[0] > 200 && top[2] < 50,
        "the box's north edge must reproject onto the mirror's row 0 (red), got \
         {top:?}; a positive floor_uv.w — v running north with Mercator y — \
         puts the map upside down",
    );
    assert!(
        bottom[2] > 200 && bottom[0] < 50,
        "the box's south edge must reproject onto the mirror's bottom rows \
         (blue), got {bottom:?}",
    );

    // 4. A saturating slab over the west half: the volume composites over
    // the floor where it stands, and the floor shows to the east.
    let mut west_slab = vec![0u8; (cells[0] * cells[1] * cells[2]) as usize];
    for z in 0..cells[2] {
        for y in 0..cells[1] {
            for x in 0..cells[0] / 2 {
                west_slab[((z * cells[1] + y) * cells[0] + x) as usize] = 255;
            }
        }
    }
    let pixels = raymarch_once_with_floor(
        &device, &queue, &pipelines, cells, &west_slab, &lut, &uniform, size, &floor,
    );
    let west = pixels[(size[1] / 4 * size[0] + size[0] / 4) as usize];
    let east = pixels[(size[1] / 4 * size[0] + 3 * size[0] / 4) as usize];
    assert!(
        west.iter().take(3).all(|c| *c > 200),
        "over the slab the volume (saturated white) must hide the floor, got \
         {west:?}; the floor is compositing in front of the march",
    );
    assert!(
        east[0] > 200 && east[2] < 50,
        "east of the slab the floor (red at this row) must show, got {east:?}",
    );
}

/// The floor and the volume agree, to the pixel, about where the weather
/// stands.
#[test]
#[ignore = "needs a real wgpu adapter; see the doc comment for the invocation"]
fn the_floor_and_the_volume_put_the_same_weather_in_the_same_place() {
    let _serialised = gpu_lock();
    let size = [128u32, 128];
    let cells = [32u32, 32, 32];
    let (device, queue) = device();
    let pipelines = VolumePipelines::new(&device, attachments(wgpu::TextureFormat::Bgra8Unorm));
    pipelines.upload_quad(&queue);

    // The planted cell: (24, 20) of 32 — off-centre on both axes and off the
    // diagonal, so every flip and every axis swap moves it.
    let (col_cell, row_cell) = (24u32, 20u32);

    // A full-height voxel column at that cell.
    let mut column = vec![0u8; (cells[0] * cells[1] * cells[2]) as usize];
    for z in 0..cells[2] {
        column[((z * cells[1] + row_cell) * cells[0] + col_cell) as usize] = 255;
    }
    // Green at every data index, not just 255: the grid is sampled `Linear`,
    // so rays off the column's exact centre read interpolated indices, and a
    // single-entry palette would paint only the centre line. The half-cell
    // bleed this admits is symmetric about the column, which is what a
    // centroid instrument needs.
    let mut lut = vec![0u8; VOLUME_LUT_BYTES];
    for entry in 1..lut.len() / 4 {
        lut[entry * 4..entry * 4 + 4].copy_from_slice(&[0, 255, 0, 255]);
    }

    // A mirror patch over the same footprint. Under the lanes below the box
    // spans the whole mirror with row 0 along its NORTH edge, so box y in
    // [20/32, 21/32] is mirror rows [1 - 21/32, 1 - 20/32) — the same
    // arithmetic as before, now a consequence of the reprojection rather than
    // of a texture lookup. Opaque black elsewhere so the patch is the only red.
    let mirror_side = 64usize;
    let mut mirror_rgba = vec![0u8; mirror_side * mirror_side * 4];
    for px in mirror_rgba.chunks_exact_mut(4) {
        px[3] = 255;
    }
    let scale = mirror_side as u32 / cells[0];
    for row in
        (mirror_side as u32 - (row_cell + 1) * scale)..(mirror_side as u32 - row_cell * scale)
    {
        for col in (col_cell * scale)..((col_cell + 1) * scale) {
            let at = ((row * mirror_side as u32 + col) * 4) as usize;
            mirror_rgba[at..at + 4].copy_from_slice(&[255, 0, 0, 255]);
        }
    }
    let floor = planted_mirror(
        &device,
        &queue,
        &pipelines,
        [mirror_side as u32, mirror_side as u32],
        &mirror_rgba,
    );

    let mut uniform = VolumeUniform::new(equatorial_box_km(), cells);
    uniform.box_from_clip = box_from_clip_down(2);
    // Not `eye_outside(2)`: an eye 2.5 box-heights up gives every ray a real
    // lateral slope, and a full-height column smears across the screen by
    // parallax — a position instrument needs parallel rays. An eye 200 boxes
    // up through the same far plane is orthographic to under a tenth of a
    // pixel at this size.
    uniform.eye_in_box = [0.5, 0.5, 200.0];
    uniform.extinction_per_km = 1000.0;
    uniform.gradient_shading = false;
    let (floor_uv, floor_geo) = equatorial_floor_lanes(floor.is_gamma_encoded());
    uniform.floor_uv = floor_uv;
    uniform.floor_geo = floor_geo;

    // Screen centroid of the pixels `select` keeps.
    let centroid = |pixels: &[[u8; 4]], select: &dyn Fn([u8; 4]) -> bool| -> (f64, f64) {
        let mut n = 0usize;
        let (mut sx, mut sy) = (0.0, 0.0);
        for (i, px) in pixels.iter().enumerate() {
            if select(*px) {
                n += 1;
                sx += (i % size[0] as usize) as f64;
                sy += (i / size[0] as usize) as f64;
            }
        }
        assert!(n > 0, "nothing painted; a broken fixture");
        (sx / n as f64, sy / n as f64)
    };

    // The volume alone.
    let pixels = raymarch_once_with_floor(
        &device, &queue, &pipelines, cells, &column, &lut, &uniform, size, &floor,
    );
    let volume_at = centroid(&pixels, &|px| px[1] > 100 && px[0] < 100);

    // The floor alone, under an empty grid.
    uniform.map_floor = true;
    let empty = vec![0u8; (cells[0] * cells[1] * cells[2]) as usize];
    let pixels = raymarch_once_with_floor(
        &device, &queue, &pipelines, cells, &empty, &lut, &uniform, size, &floor,
    );
    let floor_at = centroid(&pixels, &|px| px[0] > 100 && px[2] < 100);

    // Where the geometry says both stand: the cell centre, through the
    // down-camera's screen mapping (col = x·W, row = (1 − y)·H).
    let want = (
        (f64::from(col_cell) + 0.5) / f64::from(cells[0]) * f64::from(size[0]),
        (1.0 - (f64::from(row_cell) + 0.5) / f64::from(cells[1])) * f64::from(size[1]),
    );
    for (name, (cx, cy)) in [("volume", volume_at), ("floor", floor_at)] {
        assert!(
            (cx - want.0).abs() < 3.0 && (cy - want.1).abs() < 3.0,
            "the {name} put the planted cell at ({cx:.1}, {cy:.1}), the geometry \
             says ({:.1}, {:.1})",
            want.0,
            want.1,
        );
    }
    let (dx, dy) = (floor_at.0 - volume_at.0, floor_at.1 - volume_at.1);
    assert!(
        dx.abs() < 2.0 && dy.abs() < 2.0,
        "floor and volume disagree by ({dx:.2}, {dy:.2}) px about where the same \
         cell stands — the registration seam has moved",
    );
}

/// From under the box, the floor does not wall the volume off.
#[test]
#[ignore = "needs a real wgpu adapter; see the doc comment for the invocation"]
fn the_floor_is_transparent_from_below() {
    let _serialised = gpu_lock();
    let size = [96u32, 96];
    let cells = [16u32, 16, 16];
    let (device, queue) = device();
    let pipelines = VolumePipelines::new(&device, attachments(wgpu::TextureFormat::Bgra8Unorm));
    pipelines.upload_quad(&queue);

    // An opaque red mirror: the wall the fade must dissolve. Wall to wall, so
    // this case says nothing about where the reprojection lands and everything
    // about the coverage it is multiplied by — which is the point.
    let mirror_rgba: Vec<u8> = std::iter::repeat_n([255u8, 0, 0, 255], 64)
        .flatten()
        .collect();
    let floor = planted_mirror(&device, &queue, &pipelines, [8, 8], &mirror_rgba);

    // Looking UP the z axis from under the box: the mirror of
    // `box_from_clip_down(2)` — depth 1 unprojects one box beyond the top
    // face, the eye sits one box under the bottom, well below the fade band.
    let mut up = [[0.0f32; 4]; 4];
    up[0][0] = 0.5;
    up[1][1] = 0.5;
    up[3][0] = 0.5;
    up[3][1] = 0.5;
    up[2][2] = 2.5;
    up[3][2] = -0.5;
    up[3][3] = 1.0;
    let mut uniform = VolumeUniform::new(equatorial_box_km(), cells);
    uniform.box_from_clip = up;
    uniform.eye_in_box = [0.5, 0.5, -1.0];
    uniform.extinction_per_km = 1000.0;
    uniform.gradient_shading = false;
    uniform.map_floor = true;
    let (floor_uv, floor_geo) = equatorial_floor_lanes(floor.is_gamma_encoded());
    uniform.floor_uv = floor_uv;
    uniform.floor_geo = floor_geo;

    // 1. A saturating white slab fills the box's top half.
    let mut slab = vec![0u8; (cells[0] * cells[1] * cells[2]) as usize];
    for z in cells[2] / 2..cells[2] {
        for y in 0..cells[1] {
            for x in 0..cells[0] {
                slab[((z * cells[1] + y) * cells[0] + x) as usize] = 255;
            }
        }
    }
    let mut lut = vec![0u8; VOLUME_LUT_BYTES];
    for entry in 1..lut.len() / 4 {
        lut[entry * 4..entry * 4 + 4].copy_from_slice(&[255, 255, 255, 255]);
    }
    let pixels = raymarch_once_with_floor(
        &device, &queue, &pipelines, cells, &slab, &lut, &uniform, size, &floor,
    );
    let seen = centre(&pixels, size);
    assert!(
        seen.iter().take(3).all(|c| *c > 200) && seen[3] == 255,
        "from below, the volume (saturated white) must show through the floor, \
         got {seen:?}; an opaque ground from underneath is the wall the user \
         reported",
    );

    // 2. Nothing in the box: nothing may paint — a residual ground fragment
    // from below is the same wall at partial opacity.
    let empty = vec![0u8; (cells[0] * cells[1] * cells[2]) as usize];
    let pixels = raymarch_once_with_floor(
        &device, &queue, &pipelines, cells, &empty, &lut, &uniform, size, &floor,
    );
    let seen = centre(&pixels, size);
    assert_eq!(
        seen,
        [0, 0, 0, 0],
        "an empty box viewed from below must be fully transparent with the \
         floor toggle on",
    );
}

/// egui's own sRGB transfer functions, in Rust.
fn linear_from_gamma(gamma: f64) -> f64 {
    if gamma < 0.04045 {
        gamma / 12.92
    } else {
        ((gamma + 0.055) / 1.055).powf(2.4)
    }
}

/// The inverse of [`linear_from_gamma`]; see there.
fn gamma_from_linear(linear: f64) -> f64 {
    if linear < 0.0031308 {
        linear * 12.92
    } else {
        1.055 * linear.powf(1.0 / 2.4) - 0.055
    }
}

/// The mirror's encoding is a fact the shader has to be *told*, and
/// `floor_geo.w` is what tells it.
#[test]
#[ignore = "needs a real wgpu adapter; see the doc comment for the invocation"]
fn the_floor_decodes_the_mirror_only_when_the_flag_says_to() {
    let _serialised = gpu_lock();
    let size = [64u32, 64];
    let cells = [8u32, 8, 8];
    let (device, queue) = device();
    let pipelines = VolumePipelines::new(&device, attachments(wgpu::TextureFormat::Bgra8Unorm));
    pipelines.upload_quad(&queue);

    // Mid grey, opaque. The alpha matters twice over: `floor_colour`
    // un-premultiplies before it decodes, so a translucent fixture would be
    // measuring that division as well, and this test is about one thing.
    const GREY: u8 = 128;
    let mirror_rgba: Vec<u8> = std::iter::repeat_n([GREY, GREY, GREY, 255], 64)
        .flatten()
        .collect();
    let floor = planted_mirror(&device, &queue, &pipelines, [8, 8], &mirror_rgba);
    assert!(
        mirror_is_gamma_encoded(MIRROR_FORMAT) && floor.is_gamma_encoded(),
        "this test's fixture assumes a non-sRGB mirror holds gamma-encoded \
         texels; were MIRROR_FORMAT to become sRGB, the honest arm below would \
         be the cleared flag and not the set one",
    );

    // Nothing in the box: the floor is the whole picture, so the byte read back
    // is the floor's own composite and not a blend with anything.
    let empty = vec![0u8; (cells[0] * cells[1] * cells[2]) as usize];
    let lut = vec![0u8; VOLUME_LUT_BYTES];

    let mut uniform = VolumeUniform::new(equatorial_box_km(), cells);
    uniform.box_from_clip = box_from_clip_down(2);
    uniform.eye_in_box = eye_outside(2);
    uniform.gradient_shading = false;
    uniform.map_floor = true;
    let (floor_uv, floor_geo) = equatorial_floor_lanes(floor.is_gamma_encoded());
    uniform.floor_uv = floor_uv;
    uniform.floor_geo = floor_geo;

    let honest = centre(
        &raymarch_once_with_floor(
            &device, &queue, &pipelines, cells, &empty, &lut, &uniform, size, &floor,
        ),
        size,
    );
    // The lie: the same mirror, the flag alone cleared.
    uniform.floor_geo[3] = 0.0;
    let doubly_encoded = centre(
        &raymarch_once_with_floor(
            &device, &queue, &pipelines, cells, &empty, &lut, &uniform, size, &floor,
        ),
        size,
    );

    let want_honest = f64::from(GREY);
    let want_doubly_encoded = gamma_from_linear(f64::from(GREY) / 255.0) * 255.0;
    // The oracle checked before it is used as one: decode-then-encode must be
    // the identity, and encoding an already-encoded value must brighten it a
    // long way. A broken oracle would otherwise pass this test on a broken
    // shader.
    assert!(
        (gamma_from_linear(linear_from_gamma(f64::from(GREY) / 255.0)) * 255.0 - want_honest).abs()
            < 0.5,
        "the transfer functions restated here are not inverses of each other",
    );
    assert!(
        want_doubly_encoded > want_honest + 40.0,
        "mid grey encoded twice must be far brighter than mid grey; the oracle \
         is wrong, not the shader",
    );

    for (name, seen, want) in [
        ("with the flag set", honest, want_honest),
        ("with the flag cleared", doubly_encoded, want_doubly_encoded),
    ] {
        assert_eq!(
            seen[3], 255,
            "an opaque mirror under an empty box must composite opaque ground \
             {name}, got {seen:?}",
        );
        for channel in 0..3 {
            assert!(
                (f64::from(seen[channel]) - want).abs() <= 2.0,
                "{name} the floor composited {seen:?}; channel {channel} should \
                 be {want:.1}. Either floor_geo.w is not reaching the decode, or \
                 the decode is not egui's",
            );
        }
    }
    assert!(
        u16::from(doubly_encoded[0]) > u16::from(honest[0]) + 40,
        "clearing the gamma flag over a gamma-encoded mirror must brighten the \
         floor — {honest:?} against {doubly_encoded:?}. A shader that ignores \
         the lane entirely renders these two identically and every real floor \
         at the wrong brightness",
    );
}

/// A box footprint that runs off the mirror composites nothing there, rather
/// than smearing the mirror's border texel across the ground.
#[test]
#[ignore = "needs a real wgpu adapter; see the doc comment for the invocation"]
fn the_floor_stops_at_the_mirrors_edge_rather_than_smearing_it() {
    let _serialised = gpu_lock();
    let size = [128u32, 128];
    let cells = [8u32, 8, 8];
    let (device, queue) = device();
    let pipelines = VolumePipelines::new(&device, attachments(wgpu::TextureFormat::Bgra8Unorm));
    pipelines.upload_quad(&queue);

    // Wall to wall red: every texel of the mirror, its border included, is the
    // colour a clamp would smear. Nothing in the fixture can make the east side
    // transparent except the shader refusing to sample at all.
    let mirror_rgba: Vec<u8> = std::iter::repeat_n([255u8, 0, 0, 255], 64)
        .flatten()
        .collect();
    let floor = planted_mirror(&device, &queue, &pipelines, [8, 8], &mirror_rgba);

    let empty = vec![0u8; (cells[0] * cells[1] * cells[2]) as usize];
    let lut = vec![0u8; VOLUME_LUT_BYTES];

    let mut uniform = VolumeUniform::new(equatorial_box_km(), cells);
    uniform.box_from_clip = box_from_clip_down(2);
    uniform.eye_in_box = eye_outside(2);
    uniform.gradient_shading = false;
    uniform.map_floor = true;
    let (floor_uv, floor_geo) = equatorial_floor_lanes(floor.is_gamma_encoded());
    uniform.floor_uv = floor_uv;
    uniform.floor_geo = floor_geo;
    // The site a quarter of a mirror east of centre. u is then 0.25 + hit.x to
    // within the residual, so the footprint leaves the mirror at hit.x = 0.75.
    uniform.floor_uv[0] = 0.75;

    let pixels = raymarch_once_with_floor(
        &device, &queue, &pipelines, cells, &empty, &lut, &uniform, size, &floor,
    );
    // Both samples on the middle row, where v is comfortably inside the mirror:
    let row = size[1] / 2;
    let west = pixels[(row * size[0] + size[0] / 4) as usize];
    let east = pixels[(row * size[0] + 7 * size[0] / 8) as usize];
    assert!(
        west[0] > 200 && west[3] == 255,
        "the part of the footprint that lands on the mirror must be ground, got \
         {west:?}; the shifted lanes have moved the whole box off the picture",
    );
    assert_eq!(
        east,
        [0, 0, 0, 0],
        "the part of the footprint that runs off the mirror must paint nothing, \
         got {east:?}; the uv guard has become a clamp and the border texel is \
         being smeared across ground the source pane is not showing",
    );
}

/// Where a ray from the standard down-looking fixture camera meets the floor
/// plane, as a box coordinate.
fn floor_hit_of_pixel(col: u32, row: u32, size: [u32; 2]) -> (f64, f64) {
    let ndc_x = 2.0 * (f64::from(col) + 0.5) / f64::from(size[0]) - 1.0;
    let ndc_y = 1.0 - 2.0 * (f64::from(row) + 0.5) / f64::from(size[1]);
    // eye.z = 3, far.z = -1, so z = 0 at 3/4 of the way; the lateral half-span
    // of 0.5 is scaled by that same 3/4.
    (0.5 + 0.375 * ndc_x, 0.5 + 0.375 * ndc_y)
}

/// `cos φ` is taken at **the pixel's own latitude**, not at the site's, and
/// this is the one instrument in the tree that can tell the two apart.
#[test]
#[ignore = "needs a real wgpu adapter; see the doc comment for the invocation"]
fn the_floor_takes_cos_at_the_pixels_latitude_not_the_sites() {
    let _serialised = gpu_lock();
    let size = [128u32, 128];
    let cells = [8u32, 8, 8];
    let (device, queue) = device();
    let pipelines = VolumePipelines::new(&device, attachments(wgpu::TextureFormat::Bgra8Unorm));
    pipelines.upload_quad(&queue);

    // The shader's own constant, and `ImageBounds`' — one figure, imported so
    // this fixture cannot be reasoning about a sphere the shader left behind.
    use rustdar_geo::KM_PER_DEGREE_LAT;
    const SITE_LAT: f64 = 45.0;
    // East by north. The west and south edges sit *on* the site, so `x_km` and
    // `y_km` are the box coordinate times these — the simplest possible
    // arithmetic on the side of the fixture, leaving the reprojection as the
    // only interesting step.
    const BOX_EAST_KM: f32 = 100.0;
    const BOX_NORTH_KM: f32 = 400.0;
    // Mirror `u` per degree of longitude. Chosen so the east edge of the
    // footprint lands near `u = 0.94` — as far along the ramp as it can go
    // while staying clear of the border texel, which is what maximises the gap
    // between the two hypotheses (they differ by a *ratio* of `cos`, so the
    // discrimination scales with how far out on the ramp the probe sits).
    const U_PER_DEGREE_LON: f32 = 0.375;

    // A red ramp, exactly linear in `u` over 0.5..1 and clamped to nothing
    // below. Linear filtering between texels that lie on a line reproduces the
    // line, so the sample at any `u` in that half is `(u - 0.5) * 510` to
    // within the 8-bit rounding of the texels either side.
    const MIRROR_W: u32 = 256;
    const MIRROR_H: u32 = 64;
    let ramp = |u: f64| ((u - 0.5) * 510.0).clamp(0.0, 255.0);
    let mut mirror_rgba = Vec::with_capacity((MIRROR_W * MIRROR_H * 4) as usize);
    for _row in 0..MIRROR_H {
        for col in 0..MIRROR_W {
            let u = (f64::from(col) + 0.5) / f64::from(MIRROR_W);
            mirror_rgba.extend_from_slice(&[ramp(u).round() as u8, 0, 0, 255]);
        }
    }
    let floor = planted_mirror(
        &device,
        &queue,
        &pipelines,
        [MIRROR_W, MIRROR_H],
        &mirror_rgba,
    );

    // `v` per unit of Mercator y: one whole mirror over the 40..50 °N span,
    // negative because `v` runs down the picture while Mercator y runs north.
    let v_per_mercator_y = -1.0 / (mercator_y(50.0) - mercator_y(40.0));

    let empty = vec![0u8; (cells[0] * cells[1] * cells[2]) as usize];
    let lut = vec![0u8; VOLUME_LUT_BYTES];
    let mut uniform = VolumeUniform::new([BOX_EAST_KM, BOX_NORTH_KM, 10.0], cells);
    uniform.box_from_clip = box_from_clip_down(2);
    uniform.eye_in_box = eye_outside(2);
    uniform.gradient_shading = false;
    uniform.map_floor = true;
    uniform.floor_uv = [0.5, 0.5, U_PER_DEGREE_LON, v_per_mercator_y as f32];
    uniform.floor_geo = [
        SITE_LAT as f32,
        // West and south edges, as kilometres east and north of the site.
        0.0,
        0.0,
        if floor.is_gamma_encoded() { 1.0 } else { 0.0 },
    ];

    // The far north-east pixel: the corner of the image, which is the corner of
    // the *visible* footprint — see `floor_hit_of_pixel` for why that is 0.872
    // of the box and not 1.0.
    let (probe_col, probe_row) = (size[0] - 1, 0);
    let (hit_x, hit_y) = floor_hit_of_pixel(probe_col, probe_row, size);
    let x_km = hit_x * f64::from(BOX_EAST_KM);
    let y_km = hit_y * f64::from(BOX_NORTH_KM);

    // The two hypotheses, differing in one `cos` and nothing else.
    let lat_at_pixel = SITE_LAT + y_km / KM_PER_DEGREE_LAT;
    let predict = |cos_lat: f64| {
        let d_lon = x_km / (KM_PER_DEGREE_LAT * cos_lat);
        ramp(0.5 + d_lon * f64::from(U_PER_DEGREE_LON))
    };
    let cos_at_pixel = predict(lat_at_pixel.to_radians().cos());
    let cos_at_site = predict(SITE_LAT.to_radians().cos());

    // The probe proves itself before it is trusted: a fixture whose two
    // hypotheses agree would pass whatever the shader did.
    assert!(
        (cos_at_pixel - cos_at_site).abs() > 8.0,
        "this fixture no longer discriminates: cos-at-pixel predicts \
         {cos_at_pixel:.1} and cos-at-site {cos_at_site:.1}, only \
         {:.1} apart. The box, the latitude or the ramp's slope has been \
         changed in a way that collapses the very difference under test.",
        (cos_at_pixel - cos_at_site).abs(),
    );
    // And the footprint must stay on the mirror, or the guard returns
    // transparent and both hypotheses read back as zero.
    assert!(
        cos_at_pixel < 255.0,
        "the ramp saturates at the probe: `u` has run past the mirror's east \
         edge and the measurement is a clamp, not a reprojection",
    );

    let pixels = raymarch_once_with_floor(
        &device, &queue, &pipelines, cells, &empty, &lut, &uniform, size, &floor,
    );
    let probe = pixels[(probe_row * size[0] + probe_col) as usize];
    assert_eq!(
        probe[3], 255,
        "the probe pixel must be opaque ground, got {probe:?}; the footprint \
         has moved off the mirror and nothing below measures anything",
    );
    let read = f64::from(probe[0]);
    assert!(
        (read - cos_at_pixel).abs() <= 3.0,
        "the floor read back red {read} at the box's far north-east corner. \
         Taking `cos φ` at this pixel's own latitude predicts \
         {cos_at_pixel:.1}; taking it at the site's predicts {cos_at_site:.1}. \
         The shader has stopped correcting the footprint's trapezoid, which \
         drifts the ground east-west by up to 7.6 km at the corners of the \
         shipped box — zero at the centre, growing with latitude.",
    );
}

/// A **translucent** mirror composites at its own alpha, in both of the two
/// encodings egui can have written it in.
#[test]
#[ignore = "needs a real wgpu adapter; see the doc comment for the invocation"]
fn a_translucent_mirror_composites_at_its_own_alpha() {
    let _serialised = gpu_lock();
    let size = [64u32, 64];
    let cells = [8u32, 8, 8];
    let (device, queue) = device();
    let pipelines = VolumePipelines::new(&device, attachments(wgpu::TextureFormat::Bgra8Unorm));
    pipelines.upload_quad(&queue);

    // Straight white at half alpha.
    const ALPHA: u8 = 128;
    let alpha = f64::from(ALPHA) / 255.0;
    // `gamma(C) * A` with C white, which is what egui's *gamma* entry point
    // writes and what its linear one encodes.
    let premultiplied_gamma = 1.0 * alpha;
    let gamma_texel = (premultiplied_gamma * 255.0).round() as u8;
    let linear_texel = (linear_from_gamma(premultiplied_gamma) * 255.0).round() as u8;
    // The raymarch's own output convention is egui's: gamma-encoded and
    // premultiplied. Recovering straight white means writing `gamma(1) * A`
    // back out, which is the same byte the gamma arm planted.
    let expected = f64::from(gamma_texel);

    let empty = vec![0u8; (cells[0] * cells[1] * cells[2]) as usize];
    let lut = vec![0u8; VOLUME_LUT_BYTES];

    for (gamma_encoded, texel, arm) in [
        (true, gamma_texel, "gamma (non-sRGB swapchain)"),
        (false, linear_texel, "linear (sRGB swapchain)"),
    ] {
        let mirror_rgba: Vec<u8> = std::iter::repeat_n([texel, texel, texel, ALPHA], 64)
            .flatten()
            .collect();
        let floor = planted_mirror(&device, &queue, &pipelines, [8, 8], &mirror_rgba);

        // Nothing in the box, so the pixel read back is the floor's own
        // composite and not a blend with a march.
        let mut uniform = VolumeUniform::new(equatorial_box_km(), cells);
        uniform.box_from_clip = box_from_clip_down(2);
        uniform.eye_in_box = eye_outside(2);
        uniform.gradient_shading = false;
        uniform.map_floor = true;
        let (floor_uv, mut floor_geo) = equatorial_floor_lanes(gamma_encoded);
        uniform.floor_uv = floor_uv;
        // The lanes take the flag as an argument, but say so here too: this
        // fixture *lies* about the mirror's encoding in one arm, planting the
        // bytes the other entry point would have written into the one texture
        // format a test can make. That is the same device
        // `the_floor_decodes_the_mirror_only_when_the_flag_says_to` uses —
        // both are `#[ignore]`d, so neither runs without `-- --ignored`.
        floor_geo[3] = if gamma_encoded { 1.0 } else { 0.0 };
        uniform.floor_geo = floor_geo;

        let pixels = raymarch_once_with_floor(
            &device, &queue, &pipelines, cells, &empty, &lut, &uniform, size, &floor,
        );
        let px = centre(&pixels, size);
        assert!(
            (f64::from(px[3]) - f64::from(ALPHA)).abs() <= 3.0,
            "the {arm} arm put the floor down at alpha {} where the mirror's \
             own is {ALPHA}; the floor's coverage is no longer the mirror's \
             alpha, got {px:?}",
            px[3],
        );
        for channel in 0..3 {
            assert!(
                (f64::from(px[channel]) - expected).abs() <= 3.0,
                "the {arm} arm read back {px:?} where {expected} was due in \
                 every colour channel. The mirror holds straight white at \
                 alpha {ALPHA}, premultiplied in gamma space; recovering it \
                 means dividing by alpha *in gamma space* and re-premultiplying \
                 on the way out. Too dark means the un-premultiply is missing; \
                 the linear arm reading ~88 means it is being taken in linear \
                 space, which is a different wrong answer.",
            );
        }
    }
}

/// The smoothed reconstruction really reaches the coarse level: a lone voxel
/// paints a **wider** footprint through the cloud rung than through the raw
/// field.
#[test]
#[ignore = "needs a real wgpu adapter; see the doc comment for the invocation"]
fn the_smoothed_reconstruction_spreads_a_lone_voxel() {
    let _serialised = gpu_lock();
    let size = [128u32, 128];
    let cells = [16u32, 16, 16];

    let (device, queue) = device();
    let pipelines = VolumePipelines::new(&device, attachments(wgpu::TextureFormat::Bgra8Unorm));
    pipelines.upload_quad(&queue);

    // One filled cell in the middle of an empty grid — the isolated spike the
    // reconstruction exists to dissolve.
    let mut indices = vec![0u8; (cells[0] * cells[1] * cells[2]) as usize];
    indices[((8 * cells[1] + 8) * cells[0] + 8) as usize] = 255;
    // Opaque at every non-zero index, so interpolated indices between the
    // spike and its empty neighbours stay visible and alpha is a mask.
    let mut lut = vec![0u8; VOLUME_LUT_BYTES];
    for entry in 1..lut.len() / 4 {
        lut[entry * 4..entry * 4 + 4].copy_from_slice(&[255, 255, 255, 255]);
    }

    let mut uniform = VolumeUniform::new([10.0, 10.0, 10.0], cells);
    uniform.box_from_clip = box_from_clip_down(2);
    uniform.eye_in_box = eye_outside(2);
    uniform.extinction_per_km = 1000.0;
    uniform.gradient_shading = false;

    let painted = |uniform: &VolumeUniform| {
        raymarch_once(
            &device, &queue, &pipelines, cells, &indices, &lut, uniform, size,
        )
        .iter()
        .filter(|px| px[3] > 0)
        .count()
    };

    let raw = painted(&uniform);
    uniform.reconstruction_lod = rustdar_volumetric::bridge::CLOUD_RECONSTRUCTION_LOD;
    uniform.step_cells = rustdar_volumetric::bridge::CLOUD_STEP_CELLS;
    let cloud = painted(&uniform);
    println!("lone voxel: raw field paints {raw} px, smoothed reconstruction {cloud} px");

    assert!(raw > 0, "precondition: the lone voxel must paint at all");
    assert!(
        cloud > raw,
        "the smoothed reconstruction painted {cloud} px against the raw \
         field's {raw}; the coarse level is empty or never sampled, so the \
         cloud rung is silently rendering the raw field",
    );
    assert!(
        cloud < raw * 8,
        "the smoothed reconstruction painted {cloud} px against the raw \
         field's {raw} — more than the two-cell kernel can explain, so the \
         coarse level's bytes are misplaced",
    );
}

/// A grid uploaded **without** its coarse level marches the raw field at the
/// cloud rung — the same image, pixel for pixel, as asking for level 0.
#[test]
#[ignore = "needs a real wgpu adapter; see the doc comment for the invocation"]
fn an_omitted_coarse_level_marches_the_raw_field_at_the_cloud_rung() {
    let _serialised = gpu_lock();
    let size = [64u32, 64];
    let cells = [16u32, 16, 16];
    /// The rung a desktop asks for when the taper has not closed it.
    const CLOUD_LOD: f32 = rustdar_volumetric::bridge::CLOUD_RECONSTRUCTION_LOD;

    let (device, queue) = device();
    let pipelines = VolumePipelines::new(&device, attachments(wgpu::TextureFormat::Bgra8Unorm));
    pipelines.upload_quad(&queue);

    // The isolated spike the reconstruction is most visible on — one filled
    // cell in empty air — so a level that is present and empty, and a level
    // that is absent, cannot paint the same thing.
    let mut indices = vec![0u8; (cells[0] * cells[1] * cells[2]) as usize];
    indices[((8 * cells[1] + 8) * cells[0] + 8) as usize] = 255;
    let lut = opaque_white_lut();

    let mut uniform = VolumeUniform::new([10.0, 10.0, 10.0], cells);
    uniform.box_from_clip = box_from_clip_down(2);
    uniform.eye_in_box = eye_outside(2);
    uniform.extinction_per_km = 1000.0;
    uniform.gradient_shading = false;
    // The cloud rung's own step, held for every render below: the LOD is the
    // one variable, so an exact comparison is a comparison of the texture.
    uniform.step_cells = rustdar_volumetric::bridge::CLOUD_STEP_CELLS;

    let render = |coarse: CoarseLevel, lod: f32| {
        let mut uniform = uniform;
        uniform.reconstruction_lod = lod;
        let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
        let pixels = raymarch_once_at(
            &device, &queue, &pipelines, cells, &indices, &lut, &uniform, size, coarse,
        );
        let error = pollster::block_on(scope.pop());
        assert!(
            error.is_none(),
            "uploading a {coarse:?} grid and marching it at LOD {lod} raised \
             a validation error: {}. In production this lands inside \
             `prepare`, which has no `Result` — writing mip 1 of a texture \
             that has only mip 0 is silent there.",
            error.map(|e| e.to_string()).unwrap_or_default(),
        );
        pixels
    };

    let raw = render(CoarseLevel::Omitted, 0.0);
    let clamped = render(CoarseLevel::Omitted, CLOUD_LOD);
    let built = render(CoarseLevel::Built, CLOUD_LOD);

    let painted = |pixels: &[[u8; 4]]| pixels.iter().filter(|px| px[3] > 0).count();
    let (raw_px, clamped_px, built_px) = (painted(&raw), painted(&clamped), painted(&built));
    println!(
        "omitted coarse level: level 0 paints {raw_px} px, the cloud rung \
         {clamped_px} px, and the built level at that rung {built_px} px, of \
         {} pixels",
        size[0] * size[1],
    );

    assert!(
        raw_px > 0,
        "precondition: the fixture must paint at level 0, or every comparison \
         below is between two empty images",
    );
    // The first pixel that differs rather than `assert_eq!` on the images:
    let parted = raw
        .iter()
        .zip(&clamped)
        .position(|(level_0, rung)| level_0 != rung);
    let at = parted.unwrap_or_default();
    assert!(
        parted.is_none(),
        "an omitted coarse level marched at LOD {CLOUD_LOD} is not level 0's \
         own image: it paints {clamped_px} px where level 0 paints {raw_px}, \
         and the images first part at pixel {at} ({:?} against {:?}). The \
         sampler clamps an out-of-range level to the levels that exist, so a \
         grid with one of them must render the raw field here; a descriptor \
         that allocated a second level nothing writes gives a zeroed one to \
         sample instead",
        raw[at],
        clamped[at],
    );
    assert!(
        built != raw,
        "control: with the coarse level built, the same fixture at the same \
         rung rendered level 0's own image ({built_px} px against {raw_px}). \
         `reconstruction_lod` is reaching nothing, so the equality above holds \
         for a reason that has nothing to do with the omission and would go on \
         holding over a texture that had lost its second level entirely",
    );
}

/// The coverage-premultiplied reconstruction never paints a palette band the
/// data does not occupy — the boundary-honesty contract behind the KLOT NROT
/// green arcs, now discharged by the texture rather than by a nearest march.
#[test]
#[ignore = "needs a real wgpu adapter; see the doc comment for the invocation"]
fn coverage_reconstruction_never_paints_a_band_the_data_does_not_occupy() {
    let _serialised = gpu_lock();
    let size = [128u32, 128];
    let cells = [8u32, 8, 8];
    const DATA: u8 = 147;
    /// The air replacement in the control: a real index below the green
    /// band, so coverage is 1 everywhere and the tent has a band to sweep.
    const CONTROL_AIR: u8 = 1;

    let (device, queue) = device();
    let pipelines = VolumePipelines::new(&device, attachments(wgpu::TextureFormat::Bgra8Unorm));
    pipelines.upload_quad(&queue);

    // A 2x2x2 block in the middle of empty air, every face a no-data boundary.
    let block = |air: u8| {
        let mut indices = vec![air; (cells[0] * cells[1] * cells[2]) as usize];
        for z in 3..5u32 {
            for y in 3..5u32 {
                for x in 3..5u32 {
                    indices[((z * cells[1] + y) * cells[0] + x) as usize] = DATA;
                }
            }
        }
        indices
    };
    // The band under the data: opaque green, like NROT's anticyclonic run.
    let mut lut = vec![0u8; VOLUME_LUT_BYTES];
    for entry in usize::from(CONTROL_AIR) + 1..=120usize {
        lut[entry * 4..entry * 4 + 4].copy_from_slice(&[0, 255, 0, 255]);
    }
    let at = usize::from(DATA) * 4;
    lut[at..at + 4].copy_from_slice(&[0, 0, 255, 255]);

    let mut uniform = VolumeUniform::new([10.0, 10.0, 10.0], cells);
    uniform.box_from_clip = box_from_clip_down(2);
    uniform.eye_in_box = eye_outside(2);
    uniform.extinction_per_km = 1000.0;
    uniform.gradient_shading = false;

    let census = |indices: &[u8], uniform: &VolumeUniform| {
        let pixels = raymarch_once(
            &device, &queue, &pipelines, cells, indices, &lut, uniform, size,
        );
        let green = pixels
            .iter()
            .filter(|px| px[3] > 0 && px[1] > px[0] && px[1] > px[2])
            .count();
        let blue = pixels
            .iter()
            .filter(|px| px[3] > 0 && px[2] > px[0] && px[2] > px[1])
            .count();
        (green, blue)
    };

    let (green, blue) = census(&block(0), &uniform);
    let (control_green, control_blue) = census(&block(CONTROL_AIR), &uniform);
    println!(
        "coverage: {green} green px / {blue} blue px; \
         all-covered control: {control_green} green px / {control_blue} blue px"
    );

    assert!(
        control_green > 0,
        "precondition: with coverage 1 everywhere the tent no longer paints \
         the under-band between index {CONTROL_AIR} and index {DATA}, so this \
         fixture has stopped exercising the interpolation shell and the \
         green-free assertion below is vacuous",
    );
    assert!(
        blue > 0,
        "the reconstruction erased the data itself — the block must still \
         paint its own colour",
    );
    assert_eq!(
        green, 0,
        "the march painted {green} green pixels from a volume whose only data \
         index is blue: a filtered sample is being dragged across the no-data \
         boundary again, which is the KLOT NROT green-arc defect",
    );
}

/// ```text
/// cargo test -p rustdar-gpu --test volume_gpu \
///     the_blit_matches_egui_exactly_on_both_surface_formats \
///     -- --ignored --exact --nocapture
/// ```
#[test]
#[ignore = "needs a real wgpu adapter; see the doc comment for the invocation"]
fn the_blit_matches_egui_exactly_on_both_surface_formats() {
    let _serialised = gpu_lock();
    const SIZE: [u32; 2] = [64, 64];
    // Partial alpha on purpose: at alpha 1 the premultiply is the identity and
    // every candidate rule agrees, so a fully opaque colour would prove nothing.
    let colour = egui::Color32::from_rgba_unmultiplied(200, 60, 30, 128);
    let rect = egui::Rect::from_min_max(egui::pos2(16.0, 16.0), egui::pos2(48.0, 48.0));

    let (device, queue) = device();

    for format in [
        wgpu::TextureFormat::Rgba8Unorm,
        wgpu::TextureFormat::Rgba8UnormSrgb,
    ] {
        let theirs = egui_reference(&device, &queue, format, SIZE, rect, colour);
        let ours = blitted(&device, &queue, format, SIZE, rect, colour);

        let mut worst = 0i32;
        let mut worst_at = (0u32, 0u32);
        // Four pixels in from each edge of the rect, clear of the feathering.
        for y in (rect.min.y as u32 + 4)..(rect.max.y as u32 - 4) {
            for x in (rect.min.x as u32 + 4)..(rect.max.x as u32 - 4) {
                let at = (y * SIZE[0] + x) as usize;
                for channel in 0..4 {
                    let delta = i32::from(ours[at][channel]) - i32::from(theirs[at][channel]);
                    if delta.abs() > worst {
                        worst = delta.abs();
                        worst_at = (x, y);
                    }
                }
            }
        }

        let at = (worst_at.1 * SIZE[0] + worst_at.0) as usize;
        assert_eq!(
            worst, 0,
            "on a {format:?} surface the blit is {worst}/255 away from egui's \
             own rect_filled at {worst_at:?}: egui wrote {:?}, the blit wrote \
             {:?}. Matching egui is the requirement — the principled \
             un-premultiply/decode/re-premultiply measured 60/255 off here.",
            theirs[at], ours[at],
        );
    }
}

/// egui's own rendering of one filled rectangle, read back.
fn egui_reference(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    format: wgpu::TextureFormat,
    size: [u32; 2],
    rect: egui::Rect,
    colour: egui::Color32,
) -> Vec<[u8; 4]> {
    let context = egui::Context::default();
    let raw_input = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(size[0] as f32, size[1] as f32),
        )),
        ..Default::default()
    };
    // Painted straight onto a layer rather than through a panel, so the only
    // geometry in the frame is the rectangle.
    let output = context.run_ui(raw_input, |context| {
        context
            .layer_painter(egui::LayerId::background())
            .rect_filled(rect, 0.0, colour);
    });
    let primitives = context.tessellate(output.shapes, 1.0);

    let mut renderer = egui_wgpu::Renderer::new(
        device,
        format,
        egui_wgpu::RendererOptions {
            msaa_samples: 1,
            depth_stencil_format: None,
            dithering: false,
            predictable_texture_filtering: false,
        },
    );
    for (id, delta) in &output.textures_delta.set {
        renderer.update_texture(device, queue, *id, delta);
    }
    let screen_descriptor = egui_wgpu::ScreenDescriptor {
        size_in_pixels: size,
        pixels_per_point: 1.0,
    };

    let target = render_target(device, format, size);
    let view = target.create_view(&Default::default());
    let mut encoder = device.create_command_encoder(&Default::default());
    let user_buffers =
        renderer.update_buffers(device, queue, &mut encoder, &primitives, &screen_descriptor);
    {
        let pass = clearing_pass!(encoder, &view);
        renderer.render(&mut pass.forget_lifetime(), &primitives, &screen_descriptor);
    }
    queue.submit(user_buffers.into_iter().chain([encoder.finish()]));

    read_back(device, queue, &target, size)
}

/// The same colour, put through the offscreen and the compositing quad.
fn blitted(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    format: wgpu::TextureFormat,
    size: [u32; 2],
    rect: egui::Rect,
    colour: egui::Color32,
) -> Vec<[u8; 4]> {
    let pipelines = VolumePipelines::new(device, attachments(format));
    pipelines.upload_quad(queue);

    // The offscreen holds sRGB-encoded PREMULTIPLIED colour, which is exactly
    // what `Color32` already is — egui premultiplies after encoding, so its own
    // four bytes are the convention the raymarch's last line produces.
    let offscreen_size = [(rect.width() as u32).max(1), (rect.height() as u32).max(1)];
    let offscreen = pipelines.create_offscreen(device, offscreen_size);
    let texels: Vec<u8> = std::iter::repeat_n(
        colour.to_array(),
        (offscreen_size[0] * offscreen_size[1]) as usize,
    )
    .flatten()
    .collect();
    queue.write_texture(
        offscreen.texture().as_image_copy(),
        &texels,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(offscreen_size[0] * 4),
            rows_per_image: Some(offscreen_size[1]),
        },
        wgpu::Extent3d {
            width: offscreen_size[0],
            height: offscreen_size[1],
            depth_or_array_layers: 1,
        },
    );

    let target = render_target(device, format, size);
    let view = target.create_view(&Default::default());
    let mut encoder = device.create_command_encoder(&Default::default());
    {
        let mut pass = clearing_pass!(encoder, &view).forget_lifetime();
        // The quad covers all of clip space; the viewport is what places it.
        pass.set_viewport(
            rect.min.x,
            rect.min.y,
            rect.width(),
            rect.height(),
            0.0,
            1.0,
        );
        pipelines.paint_blit(&mut pass, &offscreen);
    }
    queue.submit(Some(encoder.finish()));

    read_back(device, queue, &target, size)
}

/// **`release_pane` gives real GPU memory back, and only the pane's own.**
#[test]
#[ignore = "needs a real wgpu adapter; see the doc comment for the invocation"]
fn release_pane_frees_that_panes_offscreen_and_the_uploads_the_store_let_go_of() {
    use rustdar_device_profile::quality::offscreen_bytes;
    use rustdar_volumetric::bridge::VolumeResources;
    use rustdar_volumetric::raymarch::resident_grid_bytes_at;

    let _serialised = gpu_lock();
    let (device, queue) = device();
    let mut resources = VolumeResources::new(
        &device,
        attachments(wgpu::TextureFormat::Bgra8Unorm),
        &queue,
    );
    assert_eq!(
        resources.resident_bytes(),
        0,
        "a fresh renderer holds no pane resources",
    );

    // Every fixture is a different size from its sibling, so one byte total
    // says *which* offscreen and *which* upload survived rather than merely
    // how many did — an inverted retain frees exactly as many bytes as the
    // right one, and only the sizes tell the two apart.
    const KEPT_PANE_PX: [u32; 2] = [96, 64];
    const GONE_PANE_PX: [u32; 2] = [48, 32];
    let (kept_cells, kept_indices) = slab_ramp(&[10, 20, 30, 40]);
    let (gone_cells, gone_indices) = slab_ramp(&[10, 20]);
    let lut = grey_ramp_lut();
    const KEPT_ID: u64 = 7;
    const GONE_ID: u64 = 9;

    assert!(resources.ensure_pane_offscreen(&device, 0, KEPT_PANE_PX));
    assert!(resources.ensure_pane_offscreen(&device, 1, GONE_PANE_PX));
    assert!(resources.ensure_upload(
        &device,
        &queue,
        KEPT_ID,
        kept_cells,
        &kept_indices,
        &lut,
        None,
        CoarseLevel::Omitted,
    ));
    assert!(resources.ensure_upload(
        &device,
        &queue,
        GONE_ID,
        gone_cells,
        &gone_indices,
        &lut,
        None,
        CoarseLevel::Omitted,
    ));

    let kept_pane = offscreen_bytes(KEPT_PANE_PX);
    let gone_pane = offscreen_bytes(GONE_PANE_PX);
    let kept_grid =
        resident_grid_bytes_at(kept_cells, CoarseLevel::Omitted).expect("a tiny grid fits");
    let gone_grid =
        resident_grid_bytes_at(gone_cells, CoarseLevel::Omitted).expect("a tiny grid fits");
    assert!(
        gone_pane > 0 && kept_pane > gone_pane && kept_grid > gone_grid,
        "precondition: the fixtures cost something and each differs from its \
         sibling, or the byte total below cannot say which one survived",
    );
    assert_eq!(
        resources.resident_bytes(),
        kept_pane + gone_pane + kept_grid + gone_grid,
        "precondition: two offscreens and two uploads are actually resident — \
         if this is 0 the release below frees nothing and passes vacuously",
    );

    // Pane 1 goes, and the store has let go of `GONE_ID` with it.
    resources.release_pane(1, &[KEPT_ID]);

    assert_eq!(
        resources.resident_bytes(),
        kept_pane + kept_grid,
        "the release did not give back exactly pane 1's offscreen and the one \
         upload the store stopped naming — and, since every fixture is a \
         different size, this is also what says pane 0's attachment and the \
         still-named grid are the ones left",
    );

    // The last pane going takes everything with it — the case that has no
    // `prepare` after it to prune anything.
    resources.release_pane(0, &[]);
    assert_eq!(
        resources.resident_bytes(),
        0,
        "closing the last 3D pane left GPU memory behind for the session",
    );
}

/// `upload_volume_at` paints the same frame whichever route its plane took.
#[test]
#[ignore = "needs a real wgpu adapter; see the doc comment for the invocation"]
fn both_upload_routes_paint_the_same_frame() {
    use rustdar_volumetric::raymarch::staging::{STAGING_RING_FEATURE, VolumeStaging};

    let _serialised = gpu_lock();
    let (device, queue) = device();
    let pipelines = VolumePipelines::new(&device, attachments(wgpu::TextureFormat::Bgra8Unorm));
    pipelines.upload_quad(&queue);

    let (cells, indices) = slab_ramp(&[0, 40, 90, 140, 190, 240, 0, 0]);
    let lut = grey_ramp_lut();
    let uniform = iso_uniform(cells);
    const SIZE: [u32; 2] = [64, 64];

    let mut ring = VolumeStaging::new(&device);
    assert_eq!(
        ring.has_ring(),
        device.features().contains(STAGING_RING_FEATURE),
        "`VolumeStaging::new` disagrees with the device it was built from about \
         whether a staging ring is available, so the capability check the whole \
         design rests on is reading the wrong thing",
    );
    // Host-only, whatever this device can do: the arm a browser takes.
    let mut host_only = VolumeStaging::default();
    assert!(!host_only.has_ring());

    let mut painted = Vec::new();
    for staging in [&mut ring, &mut host_only] {
        let volume = pipelines
            .upload_volume_at(
                &device,
                &queue,
                cells,
                &indices,
                &lut,
                CoarseLevel::Omitted,
                staging,
            )
            .expect("the grid and palette were refused");
        volume.write_uniform(&queue, &uniform);
        let target = pipelines.create_offscreen(&device, SIZE);
        let mut encoder = device.create_command_encoder(&Default::default());
        pipelines.encode_raymarch(&mut encoder, &target, &volume);
        queue.submit(Some(encoder.finish()));
        painted.push(read_back(&device, &queue, target.texture(), target.size()));
    }

    // The ring's own residency, and the fallback's — which is what says the two
    // arms above really were two arms and not the same one twice.
    eprintln!(
        "staging ring: {} ({} host bytes); fallback: {} host bytes",
        ring.has_ring(),
        ring.host_bytes(),
        host_only.host_bytes(),
    );
    assert!(
        host_only.host_bytes() > 0,
        "the fallback widened nothing, so its frame below was painted by \
         something other than the `write_texture` path this test exists to keep \
         working",
    );

    let through_ring = &painted[0];
    let through_write_texture = &painted[1];
    let lit = through_write_texture
        .iter()
        .filter(|pixel| pixel[3] > 0)
        .count();
    assert!(
        lit * 4 > through_write_texture.len(),
        "precondition: only {lit} of {} pixels have any coverage, so two blank \
         frames would compare equal and this test would prove nothing",
        through_write_texture.len(),
    );
    assert_eq!(
        through_ring, through_write_texture,
        "the volume painted from a staging-ring upload differs from the same \
         volume painted from a `write_texture` upload",
    );
}

/// The `grid_from_box` affine really crops, and the bounds flag really stops
/// the sampler smearing the grid's rim across ground the radar never reported.
#[test]
#[ignore = "needs a real wgpu adapter; see the doc comment for the invocation"]
fn the_crop_magnifies_a_sub_box_and_answers_air_outside_the_grid() {
    let _serialised = gpu_lock();
    let size = [128u32, 128];
    let cells = [16u32, 16, 16];
    let n = (cells[0] * cells[1] * cells[2]) as usize;

    let (device, queue) = device();
    let pipelines = VolumePipelines::new(&device, attachments(wgpu::TextureFormat::Bgra8Unorm));
    pipelines.upload_quad(&queue);

    // Opaque at every non-zero index, so alpha is a mask of what was fetched.
    let mut lut = vec![0u8; VOLUME_LUT_BYTES];
    for entry in 1..lut.len() / 4 {
        lut[entry * 4..entry * 4 + 4].copy_from_slice(&[255, 255, 255, 255]);
    }

    // Looking down the vertical, so the painted count is the horizontal
    // footprint the crop is about and the vertical axis stays out of it.
    let mut uniform = VolumeUniform::new([10.0, 10.0, 10.0], cells);
    uniform.box_from_clip = box_from_clip_down(2);
    uniform.eye_in_box = [0.5, 0.5, 100.0];
    uniform.extinction_per_km = 1000.0;
    uniform.gradient_shading = false;

    let painted = |indices: &[u8], uniform: &VolumeUniform| {
        raymarch_once(
            &device, &queue, &pipelines, cells, indices, &lut, uniform, size,
        )
        .iter()
        .filter(|px| px[3] > 0)
        .count()
    };

    // --- Zooming in: the drawn box is the middle half of the grid ---------
    let mut middle = vec![0u8; n];
    for z in 0..cells[2] {
        for y in 4..12 {
            for x in 4..12 {
                middle[((z * cells[1] + y) * cells[0] + x) as usize] = 255;
            }
        }
    }
    let whole = painted(&middle, &uniform);

    let mut cropped = uniform;
    cropped.grid_from_box_scale = [0.5, 0.5, 1.0];
    cropped.grid_from_box_offset = [0.25, 0.25, 0.0];
    let magnified = painted(&middle, &cropped);
    println!(
        "crop: the middle half of the grid paints {whole} px drawn whole, \
         {magnified} px drawn as the box"
    );

    assert!(whole > 0, "precondition: the block paints at all");
    assert!(
        magnified > whole * 2,
        "the middle-half block paints {magnified} px through an affine that \
         makes it the whole box, against {whole} px drawn whole — it should be \
         about four times the area. The fetch is not going through \
         `grid_coord`, so a zoomed pane draws its held grid stretched across \
         the requested box instead of cropped to it",
    );

    // --- Zooming out: the drawn box reaches past the grid ------------------
    let full = vec![255u8; n];
    let identity = painted(&full, &uniform);

    let mut outward = uniform;
    outward.grid_from_box_scale = [2.0, 2.0, 1.0];
    outward.grid_from_box_offset = [-0.5, -0.5, 0.0];
    outward.grid_bounded = true;
    let honest = painted(&full, &outward);

    outward.grid_bounded = false;
    let smeared = painted(&full, &outward);
    println!(
        "bounds: a full grid drawn in a box twice its width paints {honest} px \
         bounded, {smeared} px clamped, against {identity} px at the identity"
    );

    assert!(
        honest < identity / 2,
        "a full grid drawn into a box twice its width paints {honest} px \
         against {identity} at the identity; it covers a quarter of that box \
         and should paint about a quarter. The affine is not reaching the \
         fetch",
    );
    assert!(
        smeared > honest,
        "clamped and bounded paint the same {honest} px, so the bounds test is \
         not doing anything — either the flag is ignored or the sampler is not \
         clamping. Its whole job is that the ground outside a held grid reads \
         as air and not as that grid's rim, which would be the picture claiming \
         weather nobody measured",
    );
}

/// **What this crate charges for a resident grid is never under what the
/// device reserved for it.**
#[test]
#[ignore = "needs a real wgpu adapter; see the doc comment for the invocation"]
fn the_charged_grid_bytes_are_never_under_what_the_device_reserved() {
    use rustdar_volumetric::raymarch::grid_bytes_at;
    use rustdar_volumetric::raymarch::staging::VolumeStaging;

    let _serialised = gpu_lock();
    let (device, queue) = device();
    let Some(_) = device.generate_allocator_report() else {
        eprintln!(
            "this backend reports no allocator; nothing to compare the charge \
             against, so the check is skipped rather than passed"
        );
        return;
    };
    let layout = probe_mip_layout(&device, &queue);
    eprintln!("mip layout on this device: {layout:?}");
    let pipelines = VolumePipelines::new(&device, attachments(wgpu::TextureFormat::Bgra8Unorm));

    let reserved = |device: &wgpu::Device| -> u64 {
        device
            .generate_allocator_report()
            .expect("the backend reported an allocator a moment ago")
            .allocations
            .iter()
            .filter(|a| a.name == "rustdar.volume.grid")
            .map(|a| a.size)
            .sum()
    };

    let mut checked = 0;
    for cells in [
        [256u32, 256, 128],
        [512, 512, 32],
        [320, 320, 32],
        [192, 192, 96],
        [128, 128, 64],
        [64, 64, 34],
    ] {
        for coarse in [CoarseLevel::Built, CoarseLevel::Omitted] {
            let indices = vec![7u8; cells.iter().map(|&n| n as usize).product::<usize>()];
            let before = reserved(&device);
            let volume = pipelines
                .upload_volume_at(
                    &device,
                    &queue,
                    cells,
                    &indices,
                    &opaque_white_lut(),
                    coarse,
                    &mut VolumeStaging::new(&device),
                )
                .expect("a shipped rung is not refused");
            let actual = reserved(&device) - before;
            let charged = grid_bytes_at(cells, coarse).expect("a shipped rung fits") as u64;
            assert!(
                actual > 0,
                "{cells:?} {coarse:?}: the grid texture is not showing up in the \
                 allocator report under its label, so this test is comparing \
                 the charge against nothing",
            );
            assert!(
                charged >= actual,
                "{cells:?} {coarse:?}: charged {charged} B for a grid the device \
                 reserved {actual} B for — {} B short, which is the direction an \
                 eviction figure may never be wrong in",
                actual - charged,
            );
            eprintln!(
                "{cells:?} {coarse:?}: reserved {actual}, charged {charged} \
                 (+{} B, {:.2}%)",
                charged - actual,
                100.0 * (charged - actual) as f64 / actual as f64,
            );
            match layout {
                MipLayout::WholePyramid => assert_eq!(
                    charged - actual,
                    GRID_CHARGE_SURPLUS_BYTES,
                    "{cells:?} {coarse:?}: this device lays the whole pyramid \
                     out, so the tile model and the device are counting the \
                     same levels and the only thing between them is the page \
                     and the per-image constant. The charge ran {} B over \
                     instead — see `TEXTURE_ALLOCATION_SLACK_BYTES` for what \
                     that surplus is made of",
                    charged - actual,
                ),
                MipLayout::NamedLevelsOnly => assert_eq!(
                    actual,
                    named_level_payload(cells, coarse),
                    "{cells:?} {coarse:?}: this device lays out only the levels \
                     the descriptor names, and every such backend measured so \
                     far reserves exactly their payload — no tiles, no \
                     per-image constant. This one reserved {actual} B for a \
                     payload of {} B, which is a third layout and means the \
                     surplus has to be re-measured here. The charge is \
                     {charged} B, still over, so nothing is unsafe",
                    named_level_payload(cells, coarse),
                ),
            }
            checked += 1;

            // The drop is not the free: wgpu holds the texture until a
            // submission is triaged, and a bare `poll` on an idle device has no
            // submission index to advance past. So: submit, then wait.
            drop(volume);
            queue.submit(None);
            let _ = device.poll(wgpu::PollType::wait_indefinitely());
        }
    }
    assert_eq!(checked, 12, "every rung and both coarse arms were measured");
}

/// What a grid's charge runs over the device's own figure on a
/// [`MipLayout::WholePyramid`] backend: the 4,096 B page
/// `volume::raymarch::TEXTURE_ALLOCATION_SLACK_BYTES` adds, less the 512 B the
/// driver adds to every `D3` image and the tile model does not name.
const GRID_CHARGE_SURPLUS_BYTES: u64 = 4096 - 512;

/// How a backend lays a mip-mapped `D3` image out — **two layouts exist in the
/// wild**, and `volume::raymarch::grid_bytes_at` charges for the larger.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MipLayout {
    /// Naming a second level reserves the **whole pyramid** down to 1×1×1,
    /// whether or not anything ever writes to it — so past one level the count
    /// stops mattering. Measured on an NVIDIA RTX 3090, Vulkan, driver
    /// 610.57.04: identical reservations at `mip_level_count` 2 through 9.
    WholePyramid,
    /// Naming a second level reserves **those levels and nothing below them**,
    /// so every further level named costs its own bytes. Measured on Mesa's
    /// lavapipe (Mesa 26.1.6, LLVM 22.1.8) — which is the backend CI's `gpu`
    /// job runs on, so this arm is the one every PR exercises.
    NamedLevelsOnly,
}

/// Read [`MipLayout`] off the device, by naming one more level than a
/// descriptor already names and seeing whether the reservation moves.
fn probe_mip_layout(device: &wgpu::Device, queue: &wgpu::Queue) -> MipLayout {
    use rustdar_volumetric::VOLUME_TEXTURE_FORMAT;

    const PROBE_CELLS: [u32; 3] = [128, 128, 64];
    const PROBE_LABEL: &str = "rustdar.volume.grid.miplayout.probe";

    let reserved_at = |levels: u32| -> u64 {
        let sum = |device: &wgpu::Device| -> u64 {
            device
                .generate_allocator_report()
                .expect("the backend reported an allocator a moment ago")
                .allocations
                .iter()
                .filter(|a| a.name == PROBE_LABEL)
                .map(|a| a.size)
                .sum()
        };
        let before = sum(device);
        let probe = device.create_texture(&wgpu::TextureDescriptor {
            label: Some(PROBE_LABEL),
            size: wgpu::Extent3d {
                width: PROBE_CELLS[0],
                height: PROBE_CELLS[1],
                depth_or_array_layers: PROBE_CELLS[2],
            },
            mip_level_count: levels,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D3,
            format: VOLUME_TEXTURE_FORMAT,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let got = sum(device) - before;
        // Same as the rungs: the drop is not the free until a submission is
        // triaged, and the second reading has to start from an empty report.
        drop(probe);
        queue.submit(None);
        let _ = device.poll(wgpu::PollType::wait_indefinitely());
        got
    };

    let two = reserved_at(2);
    let three = reserved_at(3);
    // The payload of the level the third descriptor named and the second did
    // not. A backend that lays out only what it is told grows by exactly this.
    let level_two_payload: u64 = PROBE_CELLS
        .iter()
        .map(|&n| (n >> 2).max(1) as u64)
        .product::<u64>()
        * u64::from(GRID_BYTES_PER_CELL);
    eprintln!(
        "mip layout probe {PROBE_CELLS:?}: 2 levels reserved {two} B, 3 levels \
         reserved {three} B, level 2 is {level_two_payload} B of payload"
    );
    if three.saturating_sub(two) < level_two_payload / 2 {
        MipLayout::WholePyramid
    } else {
        MipLayout::NamedLevelsOnly
    }
}

/// The payload of the mip levels a grid's descriptor names, packed: no tiles
/// and no per-image constant.
fn named_level_payload(cells: [u32; 3], coarse: CoarseLevel) -> u64 {
    let levels = if coarse == CoarseLevel::Built && cells.iter().copied().max().unwrap_or(0) >= 2 {
        GRID_MIP_LEVELS
    } else {
        1
    };
    (0..levels)
        .map(|level| {
            cells
                .iter()
                .map(|&n| u64::from((n >> level).max(1)))
                .product::<u64>()
                * u64::from(GRID_BYTES_PER_CELL)
        })
        .sum()
}

/// **Every pixel of a frame composites the floor on the same arm.**
#[test]
#[ignore = "needs a real wgpu adapter; see the doc comment for the invocation"]
fn the_floor_composites_on_one_arm_per_frame() {
    use rustdar_volumetric::raymarch::VOLUME_SHADER_WGSL;

    /// The one line the two forced builds replace. Asserted to match, because a
    /// battery whose anchor has moved is a green test that proves nothing.
    const ARM: &str = "let eye_above_plane = eye.z >= 0.0;";

    let _serialised = gpu_lock();
    let (device, queue) = device();
    assert_eq!(
        VOLUME_SHADER_WGSL.matches(ARM).count(),
        1,
        "the composite's arm is no longer decided by `{ARM}`, so this test is \
         forcing something that does not exist",
    );
    let forced =
        |on: bool| VOLUME_SHADER_WGSL.replace(ARM, &format!("let eye_above_plane = {on};"));

    let format = wgpu::TextureFormat::Bgra8Unorm;
    let shipped = VolumePipelines::new(&device, attachments(format));
    let behind = VolumePipelines::from_shader_source(&device, attachments(format), &forced(true));
    let in_front =
        VolumePipelines::from_shader_source(&device, attachments(format), &forced(false));
    for pipelines in [&shipped, &behind, &in_front] {
        pipelines.upload_quad(&queue);
    }

    const SIZE: [u32; 2] = [64, 64];
    const CELLS: [u32; 3] = [8, 8, 8];
    let red: Vec<u8> = std::iter::repeat_n([255u8, 0, 0, 255], 64)
        .flatten()
        .collect();
    let solid = vec![255u8; (CELLS[0] * CELLS[1] * CELLS[2]) as usize];
    let lut = opaque_white_lut();

    // Looking up, for an eye under the bottom plane: the mirror of
    // `box_from_clip_down(2)`, so depth 1 unprojects above the top face.
    let mut looking_up = [[0.0f32; 4]; 4];
    looking_up[0][0] = 0.5;
    looking_up[1][1] = 0.5;
    looking_up[3][0] = 0.5;
    looking_up[3][1] = 0.5;
    looking_up[2][2] = 2.5;
    looking_up[3][2] = -0.5;
    looking_up[3][3] = 1.0;

    let frame = |pipelines: &VolumePipelines, eye_z: f32| {
        let floor = planted_mirror(&device, &queue, pipelines, [8, 8], &red);
        let mut uniform = VolumeUniform::new(equatorial_box_km(), CELLS);
        // The mirror standing exactly over the box's footprint, so every ray
        // that reaches the bottom plane inside the box lands on ground rather
        // than on whatever a default lane clamps to.
        let (floor_uv, floor_geo) = equatorial_floor_lanes(floor.is_gamma_encoded());
        uniform.floor_uv = floor_uv;
        uniform.floor_geo = floor_geo;
        // Down the z axis from above, up it from below — the two ways a ray
        // reaches the bottom plane at all, and the only cameras under which
        // the arm question arises.
        uniform.box_from_clip = if eye_z >= 0.0 {
            box_from_clip_down(2)
        } else {
            looking_up
        };
        uniform.eye_in_box = [0.5, 0.5, eye_z];
        uniform.extinction_per_km = 1000.0;
        uniform.gradient_shading = false;
        uniform.map_floor = true;
        raymarch_once_with_floor(
            &device, &queue, pipelines, CELLS, &solid, &lut, &uniform, SIZE, &floor,
        )
    };

    // The fade band is 0.08 box heights. Below the plane it is walked in 0.002
    // steps, because that is where the two discriminants disagree: an upward
    // ray's box entry IS its floor crossing, so `floor_t > span.x` compares a
    // number against itself and a ULP of difference decides. Above the plane
    // the eye is walked out of the box and away.
    let mut heights: Vec<f32> = (1..40).map(|n| -(n as f32) * 0.002).collect();
    heights.extend((1..30).map(|n| n as f32 * 0.002));
    heights.extend([-1.5, -0.5, -0.09, 0.1, 0.25, 0.5, 0.75, 1.0, 1.5, 3.0]);

    let (mut above, mut below, mut agreed) = (0usize, 0usize, 0usize);
    for eye_z in heights {
        let shipped_px = frame(&shipped, eye_z);
        let behind_px = frame(&behind, eye_z);
        let front_px = frame(&in_front, eye_z);
        let is_behind = shipped_px == behind_px;
        let is_front = shipped_px == front_px;
        assert!(
            is_behind || is_front,
            "eye.z = {eye_z}: the frame matches neither forced arm, so its \
             pixels did not agree on one — {} of {} pixels differ from the \
             behind arm and {} from the in-front arm",
            shipped_px
                .iter()
                .zip(&behind_px)
                .filter(|(a, b)| a != b)
                .count(),
            shipped_px.len(),
            shipped_px
                .iter()
                .zip(&front_px)
                .filter(|(a, b)| a != b)
                .count(),
        );
        match (is_behind, is_front) {
            (true, true) => agreed += 1,
            (true, false) => above += 1,
            (false, true) => below += 1,
            (false, false) => unreachable!("the assertion above already fired"),
        }
    }
    // Both arms are actually resolved, and by a margin. A sweep that only ever
    // reached one of them — or one on which the two arms happened to paint the
    // same picture everywhere — would pass the loop above vacuously.
    assert!(
        above >= 10 && below >= 30,
        "the sweep resolved {above} frames onto the behind arm and {below} onto \
         the in-front arm ({agreed} could not be told apart); it is meant to \
         cross the plane with the two arms visibly different either side",
    );
    eprintln!(
        "arm uniformity: {above} frames behind, {below} in front, {agreed} \
         where the two arms paint the same picture"
    );
}
