//! What one raymarched frame costs on this GPU, measured rather than modelled.
//!
//! `volume_quality.rs` carries Spike 0a's cost table (0.774 ms at 1440 x 900 on
//! an RTX 3090, 96 steps, shaded) and extrapolates every other device from it.
//! This harness is how such a number is measured again after the march changes:
//! it renders **a real Level II volume** through the production pipeline with
//! GPU timestamp queries around the raymarch pass, at the two offscreen sizes
//! the quality ladder actually uses, with shading both on and off.
//!
//! It is `#[ignore]`d because CI has no GPU, and env-driven because its input
//! is a file on someone's disk:
//!
//! ```text
//! VOL=/path/to/KDMX20250314_175512_V06 \
//! CENTRE_LAT=41.0 CENTRE_LON=-93.4 HALF_KM=75 \
//! cargo test -p rustdar-frontend --test volume_march_cost -- --ignored --nocapture
//! ```
//!
//! | variable | required | default | meaning |
//! |---|---|---|---|
//! | `VOL` | yes | — | Uncompressed NEXRAD Level II archive file. |
//! | `SITE` | no | file name's first four chars | Radar ICAO. |
//! | `CENTRE_LAT` / `CENTRE_LON` | yes | — | Region centre, degrees. |
//! | `HALF_KM` | no | `75` | Region half-width, km. |
//! | `YAW`/`PITCH`/`DIST`/`EXAG` | no | `225/25/2.5/3` | Orbit camera. |
//! | `SWEEP_OUT` | no | — | If set: also write a 60-frame yaw pan (0.05°/frame) as PPMs under this prefix, for the screen-locked-banding diagnosis. |
//!
//! The pan sweep exists because the banding artifact this file was written
//! against is only visible **in motion**: iso-`t` step shells are locked to the
//! eye, so under a pan the shell contours stay put in screen space while the
//! volume slides beneath them. Phase-correlating two sweep frames is the
//! diagnostic; the sweep writes the frames.
#![cfg(not(target_arch = "wasm32"))]

use egui_wgpu::wgpu;
use rustdar_egui::pane::OrbitCamera;
use rustdar_egui::volume_view::view_for;
use rustdar_frontend::egui_renderer::AttachmentConfig;
use rustdar_frontend::volume::raymarch::VolumePipelines;
use rustdar_frontend::volume::uniform::VolumeUniform;
use rustdar_radar::types::RadarProduct;
use rustdar_radar::voxel::{DESKTOP_SHAPE, VoxelGrid, VoxelRequest, build_voxels};

/// Timed render passes per configuration. The reported figure is the mean;
/// the minimum is printed beside it as the "nothing else was scheduled" bound.
const TIMED_FRAMES: usize = 30;

/// Warm-up passes before timing, so pipeline and cache compilation is not
/// billed to frame one.
const WARMUP_FRAMES: usize = 5;

#[test]
#[ignore = "needs a real wgpu adapter and a Level II file on disk; see the module doc"]
fn measure_the_raymarch_cost_on_a_real_volume() {
    let volume_path = std::path::PathBuf::from(required("VOL"));
    let scan = scan_from_archive(&volume_path);
    let (_site, site_lat, site_lon) = site_of(&volume_path);

    let request = VoxelRequest {
        centre: (parsed("CENTRE_LAT"), parsed("CENTRE_LON")),
        half_width_km: parsed_or("HALF_KM", 75.0),
        base_km_msl: 0.0,
        top_km_msl: 18.0,
        product: RadarProduct::Reflectivity,
        shape: DESKTOP_SHAPE,
        values_wanted: false,
    };
    let grid = build_voxels(&scan, &request, site_lat, site_lon)
        .expect("build_voxels refused the reflectivity volume");
    let shape = grid.shape();
    let grid_dims = [shape.nx as u32, shape.ny as u32, shape.nz as u32];
    let box_size_km = box_size_km(&grid);

    let (device, queue) = device_with_timestamps();
    let pipelines = VolumePipelines::new(&device, attachments());
    pipelines.upload_quad(&queue);
    let volume = pipelines
        .upload_volume(&device, &queue, grid_dims, grid.indices(), grid.lut())
        .expect("the grid and palette were refused");

    let yaw = parsed_or("YAW", 225.0f32);
    let pitch = parsed_or("PITCH", 25.0f32);
    let distance = parsed_or("DIST", 2.5f32);
    let exaggeration = parsed_or("EXAG", 3.0f32);

    let occupied = grid.indices().iter().filter(|&&i| i != 0).count();
    println!(
        "volume {} at {grid_dims:?}, {:.2}% of cells occupied, box {box_size_km:?} km, \
         camera yaw {yaw} pitch {pitch} dist {distance} exag {exaggeration}",
        volume_path.display(),
        100.0 * occupied as f64 / (grid_dims[0] * grid_dims[1] * grid_dims[2]) as f64,
    );

    for size in [[1440u32, 900], [720, 450]] {
        for shading in [true, false] {
            let camera = OrbitCamera::restore(yaw, pitch, distance, [0.0; 3], exaggeration)
                .expect("a finite camera");
            let view = view_for(camera, box_size_km, size[0] as f32 / size[1] as f32)
                .expect("the camera must be viewable");
            let mut uniform = VolumeUniform::new(box_size_km, grid_dims);
            uniform.box_from_clip = view.box_from_clip;
            uniform.eye_in_box = view.eye_in_box;
            uniform.gradient_shading = shading;
            // The bridge's own transfer edge, imported rather than restated,
            // so the timed march is the shipped march — the fade-anchored
            // skip changes what the empty shell costs, and that saving is
            // production behaviour. An anchor change in the bridge cannot
            // leave this instrument measuring a different threshold.
            uniform.empty_index_threshold =
                rustdar_frontend::volume::bridge::empty_index_threshold_for(grid.fade_band());
            uniform.edge_soft_width = rustdar_frontend::volume::bridge::EDGE_SOFT_WIDTH;
            volume.write_uniform(&queue, &uniform);

            let target = pipelines.create_offscreen(&device, size);
            let (mean_ms, min_ms) = timed_passes(&device, &queue, &pipelines, &target, &volume);
            println!(
                "{}x{} shading {}: mean {mean_ms:.3} ms, min {min_ms:.3} ms over \
                 {TIMED_FRAMES} frames",
                size[0],
                size[1],
                if shading { "on " } else { "off" },
            );
        }
    }

    // The pan sweep, at the shipped quality (native size, shading on).
    if let Ok(prefix) = std::env::var("SWEEP_OUT") {
        if let Some(parent) = std::path::Path::new(&prefix).parent() {
            std::fs::create_dir_all(parent).expect("creating SWEEP_OUT's directory");
        }
        let size = [1440u32, 900];
        let target = pipelines.create_offscreen(&device, size);
        for frame in 0..60u32 {
            let camera = OrbitCamera::restore(
                yaw + 0.05 * frame as f32,
                pitch,
                distance,
                [0.0; 3],
                exaggeration,
            )
            .expect("a finite camera");
            let view = view_for(camera, box_size_km, size[0] as f32 / size[1] as f32)
                .expect("the camera must be viewable");
            let mut uniform = VolumeUniform::new(box_size_km, grid_dims);
            uniform.box_from_clip = view.box_from_clip;
            uniform.eye_in_box = view.eye_in_box;
            uniform.gradient_shading = true;
            uniform.empty_index_threshold =
                rustdar_frontend::volume::bridge::empty_index_threshold_for(grid.fade_band());
            uniform.edge_soft_width = rustdar_frontend::volume::bridge::EDGE_SOFT_WIDTH;
            volume.write_uniform(&queue, &uniform);

            let mut encoder = device.create_command_encoder(&Default::default());
            pipelines.encode_raymarch(&mut encoder, &target, &volume);
            queue.submit(Some(encoder.finish()));
            let pixels = read_back(&device, &queue, target.texture(), size);
            write_ppm(&format!("{prefix}_{frame:03}.ppm"), size, &pixels);
        }
        println!("wrote 60 pan frames under {prefix}_NNN.ppm (0.05 deg of yaw per frame)");
    }
}

/// GPU milliseconds of the raymarch pass alone, over [`TIMED_FRAMES`] frames.
fn timed_passes(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    pipelines: &VolumePipelines,
    target: &rustdar_frontend::volume::raymarch::OffscreenTarget,
    volume: &rustdar_frontend::volume::raymarch::VolumeTextures,
) -> (f64, f64) {
    let queries = device.create_query_set(&wgpu::QuerySetDescriptor {
        label: Some("rustdar.volume.cost.queries"),
        ty: wgpu::QueryType::Timestamp,
        count: 2,
    });
    let resolve = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("rustdar.volume.cost.resolve"),
        size: 16,
        usage: wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    });
    let staging = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("rustdar.volume.cost.staging"),
        size: 16,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let period = f64::from(queue.get_timestamp_period());

    let mut samples_ms = Vec::with_capacity(TIMED_FRAMES);
    for frame in 0..WARMUP_FRAMES + TIMED_FRAMES {
        let mut encoder = device.create_command_encoder(&Default::default());
        // The very pass the application records, with timestamps bracketing it
        // through the seam `encode_raymarch_with_timestamps` provides.
        pipelines.encode_raymarch_with_timestamps(
            &mut encoder,
            target,
            volume,
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

// ── The volume, copied from tests/volume_real_mask.rs ───────────────────────

/// Decode a whole Level II archive file into a `Scan`. See
/// `volume_real_mask.rs` for why this goes through `decode_chunk`.
fn scan_from_archive(path: &std::path::Path) -> nexrad_model::data::Scan {
    let bytes =
        std::fs::read(path).unwrap_or_else(|e| panic!("reading VOL {}: {e}", path.display()));
    assert!(
        !bytes.starts_with(&[0x1f, 0x8b]),
        "{} is gzipped; gunzip it first",
        path.display(),
    );
    let name = path
        .file_name()
        .and_then(std::ffi::OsStr::to_str)
        .unwrap_or("volume");
    let contents = rustdar_radar::chunks::decode_chunk(name, &bytes)
        .unwrap_or_else(|e| panic!("decoding {}: {e}", path.display()));
    let coverage_pattern = contents
        .coverage_pattern
        .unwrap_or_else(|| panic!("{} carries no message 5", path.display()));
    let sweeps = nexrad_model::data::Sweep::from_radials(contents.radials);
    assert!(
        !sweeps.is_empty(),
        "{} decoded to no sweeps",
        path.display()
    );
    nexrad_model::data::Scan::new(coverage_pattern, sweeps)
}

/// The radar's ICAO and position: `SITE`, or the file name's first four chars.
fn site_of(path: &std::path::Path) -> (String, f64, f64) {
    let name = std::env::var("SITE").ok().unwrap_or_else(|| {
        path.file_name()
            .and_then(std::ffi::OsStr::to_str)
            .filter(|name| name.len() >= 4)
            .map(|name| name[..4].to_ascii_uppercase())
            .unwrap_or_else(|| panic!("cannot read an ICAO off {}; set SITE", path.display()))
    });
    let site = rustdar_radar::sites::get_radar_site(&name)
        .unwrap_or_else(|| panic!("{name} is not in rustdar_radar::sites; set SITE"));
    (name, site.lat, site.lon)
}

/// The box's true physical extent in kilometres.
fn box_size_km(grid: &VoxelGrid) -> [f32; 3] {
    let (x0, x1) = grid.x_range_km();
    let (y0, y1) = grid.y_range_km();
    let (z0, z1) = grid.z_range_km_msl();
    [(x1 - x0) as f32, (y1 - y0) as f32, (z1 - z0) as f32]
}

// ── GPU plumbing ─────────────────────────────────────────────────────────────

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
    assert!(
        adapter.features().contains(wgpu::Features::TIMESTAMP_QUERY),
        "this adapter cannot answer timestamp queries, so there is nothing to measure with"
    );
    pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("rustdar.volume.cost.device"),
        required_features: wgpu::Features::TIMESTAMP_QUERY,
        required_limits: adapter.limits(),
        memory_hints: Default::default(),
        experimental_features: Default::default(),
        trace: Default::default(),
    }))
    .expect("could not create a device on an adapter that was found")
}

/// The egui pass a blit would be composited into; only the blit reads it.
fn attachments() -> AttachmentConfig {
    AttachmentConfig {
        color_format: wgpu::TextureFormat::Bgra8Unorm,
        depth_format: None,
        msaa_samples: 1,
    }
}

/// Read an RGBA8 texture back as one `[u8; 4]` per texel, row-major.
fn read_back(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    texture: &wgpu::Texture,
    size: [u32; 2],
) -> Vec<[u8; 4]> {
    let unpadded = size[0] * 4;
    let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let padded = unpadded.div_ceil(align) * align;
    let staging = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("rustdar.volume.cost.readback"),
        size: u64::from(padded) * u64::from(size[1]),
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
                rows_per_image: Some(size[1]),
            },
        },
        wgpu::Extent3d {
            width: size[0],
            height: size[1],
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
    let mut pixels = Vec::with_capacity((size[0] * size[1]) as usize);
    for row in 0..size[1] as usize {
        let start = row * padded as usize;
        for column in 0..size[0] as usize {
            let at = start + column * 4;
            pixels.push(<[u8; 4]>::try_from(&mapped[at..at + 4]).expect("four bytes per texel"));
        }
    }
    pixels
}

/// The offscreen's colour over black, binary P6. Premultiplied over black is
/// the premultiplied value itself.
fn write_ppm(path: &str, size: [u32; 2], pixels: &[[u8; 4]]) {
    assert_eq!(pixels.len(), (size[0] * size[1]) as usize);
    let mut out = format!("P6\n{} {}\n255\n", size[0], size[1]).into_bytes();
    for pixel in pixels {
        out.extend_from_slice(&pixel[..3]);
    }
    std::fs::write(path, out).unwrap_or_else(|e| panic!("writing {path}: {e}"));
}

// ── The environment ──────────────────────────────────────────────────────────

fn required(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("{name} is required; see this file's module doc"))
}

fn parsed<T>(name: &str) -> T
where
    T: std::str::FromStr,
    T::Err: std::fmt::Display,
{
    let raw = required(name);
    raw.trim()
        .parse()
        .unwrap_or_else(|e| panic!("{name}={raw:?} does not parse: {e}"))
}

fn parsed_or<T>(name: &str, fallback: T) -> T
where
    T: std::str::FromStr,
    T::Err: std::fmt::Display,
{
    match std::env::var(name) {
        Ok(raw) => raw
            .trim()
            .parse()
            .unwrap_or_else(|e| panic!("{name}={raw:?} does not parse: {e}")),
        Err(_) => fallback,
    }
}
