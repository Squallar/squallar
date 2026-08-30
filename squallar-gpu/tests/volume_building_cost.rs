//! What a frame of buildings costs on this GPU, measured rather than modelled.
//!
//! ```text
//! cargo test -p squallar-gpu --test volume_building_cost -- --ignored --nocapture
//! ```
//!
//! `#[ignore]`d, and it needs **no file on anyone's disk**: the city is
//! `squallar-buildings`' committed Monaco tile, extruded through the same
//! `read_footprints` + `extrude` the worker's job row runs, and replicated on a
//! grid to reach the vertex counts the rung ladder actually offers. That is
//! what makes this reproducible on the CI `gpu` job rather than a number one
//! machine once produced.
//!
//! # The denominators, because there are three and they are not the same
//!
//! * **`ground pass`** is the whole pass: the terrain grid AND the prisms, in
//!   one `encode_ground`. It is what a frame pays.
//! * **`terrain alone`** is the same pass with no mesh handed to it — B3's
//!   frame, unchanged.
//! * **`the prisms`** is the difference between the two, at the same camera and
//!   the same offscreen. It is a difference of two measurements and not a
//!   measurement of its own, which matters because the two draws share a depth
//!   buffer: prisms that cover terrain make the terrain's own fragments cheaper,
//!   so the difference is the *marginal* cost of adding the city and is not the
//!   cost of drawing the city on its own.
//!
//! The raymarch pass is not in any of them. It is measured by
//! `volume_march_cost.rs`, against a Level II volume, and the two figures have
//! different denominators and are never added.
#![cfg(not(target_arch = "wasm32"))]

use egui_wgpu::wgpu;
use squallar_buildings::{
    BoxFrame, BuildingFootprint, BuildingMesh, PrismBudget, PrismCeilings, TileId, extrude,
    read_footprints,
};
use squallar_device_profile::quality::{GroundPass, ResolutionRung};
use squallar_egui::pane::OrbitCamera;
use squallar_egui::volume_view::view_for;
use squallar_volumetric::raymarch::staging::{STAGING_RING_FEATURE, VolumeStaging};
use squallar_volumetric::raymarch::{BuildingPrisms, OffscreenPlan, VolumePipelines};
use squallar_volumetric::uniform::VolumeUniform;

mod gpu_harness;
use gpu_harness::{MIRROR_FORMAT, attachments, gpu_lock, opaque_white_lut, planted_mirror};

/// Timed passes per configuration; the reported figure is the mean and the min.
const TIMED_FRAMES: usize = 30;

/// Warm-up passes, so shader compilation is not billed to frame one.
const WARMUP_FRAMES: usize = 5;

/// The box the city is drawn in, and the camera it is drawn from — the same
/// scene `volume_buildings.rs` asserts against, so a cost and a correctness
/// figure describe one picture.
const BOX_KM: [f32; 3] = [2.0, 2.0, 0.4];
const SITE: (f64, f64) = (43.731_414_013_768_99, 7.415_771_484_375);
const POSTS: u32 = 256;
const RIDGE_SIGMA: f32 = 0.18;
const RIDGE_AMPLITUDE: f32 = 0.25;
const HEIGHT_SCALE: f32 = 1.0 / 65_535.0;

const REAL_BUILDING_TILE: &[u8] =
    include_bytes!("../../squallar-buildings/testdata/monaco-building-z14-8529-5974.mvt");
const REAL_TILE: TileId = TileId {
    z: 14,
    x: 8529,
    y: 5974,
};

/// The camera every figure below is taken at: the shipped default look, dollied
/// in far enough that the city fills the pane.
///
/// **One camera, and the figure says so.** A cost swept over eleven cameras
/// would be a distribution reported as a number; what a reader needs from this
/// file is a cost at a stated viewpoint, and the correctness sweep is next door.
const CAMERA: (f32, f32, f32, f32) = (140.0, 35.0, 0.9, 3.0);

fn ridge_samples() -> Vec<u16> {
    let mut samples = Vec::with_capacity((POSTS * POSTS) as usize);
    for _j in 0..POSTS {
        for i in 0..POSTS {
            let u = (i as f32 + 0.5) / POSTS as f32;
            let d = (u - 0.5) / RIDGE_SIGMA;
            let z = (RIDGE_AMPLITUDE * (-0.5 * d * d).exp()).clamp(0.0, 1.0);
            samples.push((z / HEIGHT_SCALE).round().clamp(0.0, 65_535.0) as u16);
        }
    }
    samples
}

fn frame_of(x_km: (f64, f64), y_km: (f64, f64)) -> BoxFrame {
    BoxFrame {
        site: SITE,
        x_km,
        y_km,
    }
}

/// The real tile's footprints, translated onto a `copies x copies` grid.
///
/// **Replication and not invention.** Every vertex count below is a whole
/// number of real Monaco tiles, so the ring-vertex distribution, the height
/// distribution and the tessellator's work per building are the archive's at
/// every rung. A synthetic block of boxes would have moved every one of them,
/// and `squallar_buildings::budget` records an 8.7x capacity error that came
/// from exactly that substitution.
fn replicated_city(copies: u32) -> BuildingMesh {
    // Read once over a frame wide enough to keep the whole tile, then translate.
    let source = read_footprints(
        REAL_TILE,
        REAL_BUILDING_TILE,
        &frame_of((-10.0, 10.0), (-10.0, 10.0)),
    )
    .expect("the committed Monaco tile no longer parses");
    let mut all: Vec<BuildingFootprint> = Vec::new();
    let step = f64::from(BOX_KM[0]) / f64::from(copies.max(1));
    for j in 0..copies {
        for i in 0..copies {
            let dx = -f64::from(BOX_KM[0]) / 2.0 + (f64::from(i) + 0.5) * step;
            let dy = -f64::from(BOX_KM[1]) / 2.0 + (f64::from(j) + 0.5) * step;
            for footprint in &source {
                let mut moved = footprint.clone();
                for ring in &mut moved.rings {
                    for point in &mut ring.points {
                        point[0] = point[0] / f64::from(copies.max(1)) + dx;
                        point[1] = point[1] / f64::from(copies.max(1)) + dy;
                    }
                }
                moved.bbox = [
                    moved.bbox[0] / f64::from(copies.max(1)) + dx,
                    moved.bbox[1] / f64::from(copies.max(1)) + dy,
                    moved.bbox[2] / f64::from(copies.max(1)) + dx,
                    moved.bbox[3] / f64::from(copies.max(1)) + dy,
                ];
                all.push(moved);
            }
        }
    }
    // The finest rung, so the ladder never sheds inside a cost measurement —
    // a figure taken from a mesh the budget truncated would be a figure for a
    // smaller city reported as one for a larger.
    extrude(
        &all,
        &PrismBudget::fit(PrismCeilings {
            vram_bytes: u64::MAX,
            max_buffer_bytes: u64::MAX,
        }),
    )
}

#[test]
#[ignore = "needs a real wgpu adapter with timestamp queries; see the module doc"]
fn measure_the_building_pass_cost_on_a_real_city() {
    let _guard = gpu_lock();
    let (device, queue) = device_with_timestamps();
    let pipelines = VolumePipelines::new(&device, attachments(wgpu::TextureFormat::Bgra8Unorm));
    pipelines.upload_quad(&queue);

    let heights = pipelines
        .upload_heights(&device, &queue, [POSTS, POSTS], &ridge_samples())
        .expect("the fixture height field was refused");
    const EDGE: u32 = 32;
    let rgba: Vec<u8> = std::iter::repeat_n([180u8, 180, 180, 255], (EDGE * EDGE) as usize)
        .flatten()
        .collect();
    let mirror = planted_mirror(&device, &queue, &pipelines, [EDGE, EDGE], &rgba);

    let (cells, indices) = ([8u32, 8, 8], vec![0u8; 8 * 8 * 8]);
    let volume = pipelines
        .upload_volume(
            &device,
            &queue,
            cells,
            &indices,
            &opaque_white_lut(),
            &mut VolumeStaging::new(&device),
        )
        .expect("the grid and palette were refused");

    // The cities, in whole tiles: 1, 4, 9 and 16 copies of the real one.
    let cities: Vec<(u32, BuildingMesh)> = [1u32, 2, 3, 4]
        .into_iter()
        .map(|copies| (copies * copies, replicated_city(copies)))
        .collect();

    println!(
        "adapter timestamp period {} ns; box {BOX_KM:?} km, camera {CAMERA:?}, \
         {POSTS}x{POSTS} height posts",
        queue.get_timestamp_period(),
    );
    println!(
        "\nEvery row is the GROUND PASS alone. The raymarch pass is not in any \
         of these figures — see `volume_march_cost.rs`, whose denominator is \
         different and is never added to this one."
    );

    for size in [[1440u32, 900], [720, 450]] {
        let camera = OrbitCamera::restore(CAMERA.0, CAMERA.1, CAMERA.2, [0.0; 3], CAMERA.3)
            .expect("a finite camera");
        let view = view_for(camera, BOX_KM, size[0] as f32 / size[1] as f32)
            .expect("the camera must be viewable");
        let mut uniform = VolumeUniform::new(BOX_KM, cells);
        uniform.box_from_clip = view.box_from_clip;
        uniform.clip_from_box = view.clip_from_box;
        uniform.eye_in_box = view.eye_in_box;
        uniform.vertical_exaggeration = camera.vertical_exaggeration();
        uniform.floor_uv = [0.5, 0.5, 0.37, -0.37];
        uniform.floor_geo = [
            SITE.0 as f32,
            -BOX_KM[0] / 2.0,
            -BOX_KM[1] / 2.0,
            if MIRROR_FORMAT.is_srgb() { 0.0 } else { 1.0 },
        ];
        uniform.aim_occluder(RIDGE_AMPLITUDE, HEIGHT_SCALE, 0.0);
        assert!(uniform.place_buildings(-BOX_KM[0] / 2.0, -BOX_KM[1] / 2.0));
        volume.write_uniform(&queue, &uniform);

        let target = pipelines.create_offscreen(
            &device,
            OffscreenPlan {
                size,
                rung: ResolutionRung::Native,
                ground: GroundPass::On,
            },
        );

        let (bare_mean, bare_min) = timed_ground_passes(
            &device, &queue, &pipelines, &target, &volume, &mirror, &heights, None,
        );
        println!(
            "\n{}x{}  terrain alone                    : mean {bare_mean:.4} ms, \
             min {bare_min:.4} ms",
            size[0], size[1],
        );

        for (tiles, mesh) in &cities {
            let prisms = pipelines
                .upload_buildings(
                    &device,
                    &queue,
                    &mesh.positions,
                    &mesh.normals,
                    &mesh.indices,
                )
                .expect("the city mesh was refused");
            let (mean, min) = timed_ground_passes(
                &device,
                &queue,
                &pipelines,
                &target,
                &volume,
                &mirror,
                &heights,
                Some(&prisms),
            );
            println!(
                "{}x{}  {tiles:>2} Monaco tiles: {:>7} buildings, {:>7} vertices, \
                 {:>7} triangles, {:>8.3} MB of buffers",
                size[0],
                size[1],
                mesh.kept,
                mesh.positions.len(),
                mesh.indices.len() / 3,
                prisms.buffer_bytes() as f64 / 1.0e6,
            );
            println!("        ground pass (terrain + prisms) : mean {mean:.4} ms, min {min:.4} ms",);
            println!(
                "        the prisms (the difference)    : mean {:.4} ms, min {:.4} ms",
                mean - bare_mean,
                min - bare_min,
            );
        }
    }
}

/// GPU milliseconds of the ground pass alone, over [`TIMED_FRAMES`] frames.
#[allow(clippy::too_many_arguments)]
fn timed_ground_passes(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    pipelines: &VolumePipelines,
    target: &squallar_volumetric::raymarch::OffscreenTarget,
    volume: &squallar_volumetric::raymarch::VolumeTextures,
    mirror: &squallar_volumetric::raymarch::PaneMirror,
    heights: &squallar_volumetric::raymarch::GroundHeights,
    prisms: Option<&BuildingPrisms>,
) -> (f64, f64) {
    let queries = device.create_query_set(&wgpu::QuerySetDescriptor {
        label: Some("squallar.volume.building_cost.queries"),
        ty: wgpu::QueryType::Timestamp,
        count: 2,
    });
    let resolve = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("squallar.volume.building_cost.resolve"),
        size: 16,
        usage: wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    });
    let staging = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("squallar.volume.building_cost.staging"),
        size: 16,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let period = f64::from(queue.get_timestamp_period());

    let mut samples_ms = Vec::with_capacity(TIMED_FRAMES);
    for frame in 0..WARMUP_FRAMES + TIMED_FRAMES {
        let mut encoder = device.create_command_encoder(&Default::default());
        // The very pass the application records, timestamps bracketing it.
        pipelines.encode_ground_with_timestamps(
            &mut encoder,
            target,
            volume,
            Some(mirror),
            Some(heights),
            prisms,
            Some(wgpu::RenderPassTimestampWrites {
                query_set: &queries,
                beginning_of_pass_write_index: Some(0),
                end_of_pass_write_index: Some(1),
            }),
        );
        encoder.resolve_query_set(&queries, 0..2, &resolve, 0);
        encoder.copy_buffer_to_buffer(&resolve, 0, &staging, 0, 16);
        queue.submit(Some(encoder.finish()));

        staging.slice(..).map_async(wgpu::MapMode::Read, |result| {
            result.expect("mapping the timestamp buffer failed");
        });
        device
            .poll(wgpu::PollType::wait_indefinitely())
            .expect("polling the device failed");
        let stamps: Vec<u64> = {
            let mapped = staging.slice(..).get_mapped_range();
            mapped
                .chunks_exact(8)
                .map(|b| u64::from_le_bytes(<[u8; 8]>::try_from(b).expect("eight bytes")))
                .collect()
        };
        staging.unmap();
        if frame >= WARMUP_FRAMES {
            samples_ms.push((stamps[1].wrapping_sub(stamps[0])) as f64 * period / 1.0e6);
        }
    }
    let mean = samples_ms.iter().sum::<f64>() / samples_ms.len() as f64;
    let min = samples_ms.iter().cloned().fold(f64::MAX, f64::min);
    (mean, min)
}

/// A device that can answer timestamp queries, or a panic naming the feature.
fn device_with_timestamps() -> (wgpu::Device, wgpu::Queue) {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::default(),
        force_fallback_adapter: false,
        compatible_surface: None,
    }))
    .expect("no wgpu adapter; this test is ignored by default for that reason");
    gpu_harness::announce(&adapter);
    assert!(
        adapter.features().contains(wgpu::Features::TIMESTAMP_QUERY),
        "this adapter cannot answer timestamp queries, so there is nothing to measure with"
    );
    pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("squallar.volume.building_cost.device"),
        required_features: wgpu::Features::TIMESTAMP_QUERY
            | (adapter.features() & STAGING_RING_FEATURE),
        required_limits: adapter.limits(),
        memory_hints: Default::default(),
        experimental_features: Default::default(),
        trace: Default::default(),
    }))
    .expect("could not create a device on an adapter that was found")
}
