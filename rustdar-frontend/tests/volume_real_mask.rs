//! A measurement harness: one real Level II volume, rendered offscreen through
//! the production raymarch at a camera measured somewhere else.
//!
//! This file adds **no** assertion about how rustdar should look. It exists so
//! that a projection matrix lifted from another application can be pointed at
//! rustdar's own pipeline and the two silhouettes compared pixel for pixel. The
//! test is `#[ignore]`d and reads every parameter from the environment, because
//! its inputs are a file on someone's disk and a camera nothing in this repo
//! can derive.
//!
//! ```text
//! VOL=/path/to/KDMX20250314_175512_V06 \
//! CENTRE_LAT=41.0 CENTRE_LON=-93.4 HALF_KM=75 THRESH=20 \
//! OUT=/tmp/rd_kdmx \
//! cargo test -p rustdar-frontend --test volume_real_mask -- --ignored --nocapture
//! ```
//!
//! # The environment contract
//!
//! | variable | required | default | meaning |
//! |---|---|---|---|
//! | `VOL` | yes | — | Path to an uncompressed NEXRAD Level II archive file (`AR2V…`). |
//! | `SITE` | no | first four characters of `VOL`'s file name | ICAO of the radar, looked up in `rustdar_radar::sites`. |
//! | `CENTRE_LAT` | yes | — | Region centre latitude, degrees. |
//! | `CENTRE_LON` | yes | — | Region centre longitude, degrees. |
//! | `HALF_KM` | no | `80.0` | Region half-width, km. `build_voxels` clamps to [10, 230]. |
//! | `BASE_KM` | no | `0.0` | Box base, km MSL. |
//! | `TOP_KM` | no | `18.0` | Box top, km MSL. |
//! | `PRODUCT` | no | `BR` | Moment: `BR`/`REF`, `BV`/`VEL`, `SW`, `ZDR`, `PHI`, `RHO`/`CC`. |
//! | `THRESH` | yes | — | The mask's cut, in the moment's own units (dBZ for `BR`). |
//! | `CAM` | no | — | Path to a text file of 19 whitespace-separated `f32`s. |
//! | `YAW` | no | `225.0` | Fallback camera yaw, degrees — used only without `CAM`. |
//! | `PITCH` | no | `25.0` | Fallback camera pitch, degrees. |
//! | `DIST` | no | `2.5` | Fallback eye distance, in box half-diagonals. |
//! | `EXAG` | no | `3.0` | Fallback vertical exaggeration. |
//! | `SIZE` | no | `1200x900` | Output size, `WxH` pixels. |
//! | `EXTINCTION` | no | `800.0` | Extinction per km for the **mask** render. |
//! | `OUT` | yes | — | Output path prefix; the three files below are written under it. |
//!
//! `.gz` volumes are **not** supported and are refused with a message rather
//! than mis-decoded: `rustdar-radar` reaches Level II through
//! `nexrad_data::volume::File`, which understands the bzip2-per-LDM-record
//! framing and nothing else, and no whole-file gunzip exists anywhere in this
//! crate's dependency set. `gunzip` the file first.
//!
//! # `CAM`, and its exact layout
//!
//! Nineteen `f32`s, whitespace-separated, in one file:
//!
//! * the first **sixteen** are `box_from_clip` in **column-major** order —
//!   `m[0][0] m[0][1] m[0][2] m[0][3] m[1][0] … m[3][3]`, column 0 first. That
//!   is `VolumeUniform::box_from_clip`'s own `[[f32; 4]; 4]` layout (`m[column][row]`)
//!   and WGSL's `mat4x4`, so the numbers go in with no transpose;
//! * the next **three** are `eye_in_box`, the perspective eye in the box's own
//!   `0..1` coordinates.
//!
//! Box space is the unit cube: `(0,0,0)` is the box's west/south/bottom corner
//! and `(1,1,1)` its east/north/top one, with the axes in the grid's own order
//! (x east, y north, z up). `box_size_km` is passed to the uniform separately
//! and is the **true, unexaggerated** extent, exactly as
//! `volume_bridge::box_size_km` computes it — so a caller that wants a
//! vertically exaggerated picture must bake the stretch into its own matrix,
//! not into this number.
//!
//! Without `CAM` the camera is built by `rustdar_egui::volume_view::view_for`
//! from `YAW`/`PITCH`/`DIST`/`EXAG`, which is what the application itself does,
//! so the harness is usable with no external measurement at all.
//!
//! # What is written
//!
//! * `<OUT>_mask.pgm` — binary P5, the alpha channel of the **hard-LUT** render.
//! * `<OUT>_colour.ppm` — binary P6, the **production-LUT** render
//!   un-premultiplied and composited over **black**.
//! * `<OUT>_meta.txt` — the numbers behind both, also printed to stdout.
//!
//! # Why the hard LUT makes the alpha channel a mask
//!
//! The palette handed to the mask render is built from the grid's own
//! `index_to_value`: every index whose decoded value is below `THRESH` gets
//! alpha 0, every index at or above it gets opaque white. The shader's
//! `absorbed = 1 - exp(-entry.a · extinction · segment_km)` is therefore
//! **exactly zero** below the cut, and at `EXTINCTION` = 800/km a single
//! kilometre above it saturates — so the returned alpha is coverage, not
//! opacity, and the early-out on transmittance stops the march at the first
//! hit. Gradient shading is off for the same render: it multiplies colour, not
//! alpha, but it costs six extra fetches to change nothing.
//!
//! Two things keep this from being the mathematically exact projection of
//! `{value >= THRESH}`, and both are the production pipeline's own behaviour
//! rather than this harness's:
//!
//! * the grid texture's sampler is `Linear`, so a fetch straddling an echo edge
//!   returns an interpolated **index**, which the (`Nearest`-sampled) hard LUT
//!   then reads as below-cut. The silhouette is eroded by up to half a voxel at
//!   every boundary;
//! * the march steps `RAYMARCH_STEP_CELLS` cells along the ray with a
//!   deterministic per-pixel jitter of the comb's phase, so a chord shorter
//!   than one step — a silhouette tangent — is hit or missed by the pixel's
//!   own hash. That is a one-pixel ring of noise on the mask's boundary, not
//!   the whole-feature loss the fixed 96-step march it replaced could show.
//!
//! Neither is corrected here. A harness that silently rendered something other
//! than what ships would measure the wrong thing.
//!
//! # Why this drives `VolumePipelines` directly
//!
//! `BridgeVolumePainter` refuses a palette whose `fade_band()` is short and
//! refuses a single-tilt volume, and a hard LUT has a fade band of zero by
//! construction — the whole point of it is that there is no ramp. Going through
//! the bridge would return `VolumePaint::Empty` and measure nothing, so this
//! goes to `VolumePipelines` the way `tests/volume_gpu.rs` does.
#![cfg(not(target_arch = "wasm32"))]

use egui_wgpu::wgpu;
use rustdar_egui::pane::OrbitCamera;
use rustdar_egui::volume_view::view_for;
use rustdar_frontend::constants::VOLUME_LUT_BYTES;
use rustdar_frontend::egui_renderer::AttachmentConfig;
use rustdar_frontend::volume::raymarch::VolumePipelines;
use rustdar_frontend::volume::uniform::VolumeUniform;
use rustdar_radar::types::RadarProduct;
use rustdar_radar::voxel::{DESKTOP_SHAPE, VoxelGrid, VoxelRequest, build_voxels};

/// Build a real volume, render it twice, and write a mask, a picture and the
/// numbers behind both.
///
/// See the module doc for the invocation and for every environment variable.
#[test]
#[ignore = "needs a real wgpu adapter and a Level II file on disk; see the module doc"]
fn render_a_real_volume_mask() {
    let out_prefix = required("OUT");
    if let Some(parent) = std::path::Path::new(&out_prefix).parent() {
        std::fs::create_dir_all(parent)
            .unwrap_or_else(|e| panic!("creating {} for OUT: {e}", parent.display()));
    }

    let volume_path = std::path::PathBuf::from(required("VOL"));
    let scan = scan_from_archive(&volume_path);
    let (site_name, site_lat, site_lon) = site_of(&volume_path);

    let product = product_from_env();
    let request = VoxelRequest {
        centre: (parsed("CENTRE_LAT"), parsed("CENTRE_LON")),
        half_width_km: parsed_or("HALF_KM", 80.0),
        base_km_msl: parsed_or("BASE_KM", 0.0),
        top_km_msl: parsed_or("TOP_KM", 18.0),
        product,
        // The desktop shape unconditionally: this harness is measuring the
        // renderer, and the runtime ladder that can step it down is a property
        // of the device the application happens to be on.
        shape: DESKTOP_SHAPE,
        // The indices are all the raymarch reads, and the value plane is 32 MiB
        // at this shape.
        values_wanted: false,
    };
    let grid = build_voxels(&scan, &request, site_lat, site_lon).unwrap_or_else(|| {
        panic!(
            "build_voxels refused: product {} at {:?}, base {} km, top {} km",
            product.code(),
            request.centre,
            request.base_km_msl,
            request.top_km_msl,
        )
    });

    let size = size_from_env();
    let box_size_km = box_size_km(&grid);
    let shape = grid.shape();
    let grid_dims = [shape.nx as u32, shape.ny as u32, shape.nz as u32];

    let (camera_source, box_from_clip, eye_in_box, exaggeration) = camera(box_size_km, size);

    let threshold: f32 = parsed("THRESH");
    let (hard_lut, cut_index) = hard_lut(&grid, threshold);
    let extinction = parsed_or("EXTINCTION", 800.0f32);

    let (device, queue) = device();
    let pipelines = VolumePipelines::new(&device, attachments());
    pipelines.upload_quad(&queue);

    // The mask. Opaque-or-nothing palette, extinction high enough that one
    // kilometre saturates, and no shading — see the module doc.
    let mut uniform = VolumeUniform::new(box_size_km, grid_dims);
    uniform.box_from_clip = box_from_clip;
    uniform.eye_in_box = eye_in_box;
    uniform.extinction_per_km = extinction;
    uniform.gradient_shading = false;
    let mask_pixels = raymarch_once(
        &device,
        &queue,
        &pipelines,
        grid_dims,
        grid.indices(),
        &hard_lut,
        &uniform,
        size,
    );

    // The picture, at the production transfer function and the grid's own
    // palette. The transfer fields are set exactly as `volume::bridge` sets
    // them — the fade-anchored skip threshold and the soft edge included,
    // imported from the bridge rather than restated, so an anchor change
    // there cannot leave this harness rendering a different threshold —
    // because "production" is the bridge's configuration, and a harness that
    // rendered the hard-threshold instrument configuration in colour would
    // show an edge the application does not draw.
    uniform.extinction_per_km = rustdar_frontend::volume::uniform::DEFAULT_EXTINCTION_PER_KM;
    uniform.gradient_shading = true;
    uniform.reconstruction_lod = rustdar_frontend::volume::bridge::CLOUD_RECONSTRUCTION_LOD;
    uniform.step_cells = rustdar_frontend::volume::bridge::CLOUD_STEP_CELLS;
    uniform.vertical_exaggeration = exaggeration;
    uniform.empty_index_threshold =
        rustdar_frontend::volume::bridge::empty_index_threshold_for(grid.fade_band());
    uniform.edge_soft_width = rustdar_frontend::volume::bridge::EDGE_SOFT_WIDTH;
    let colour_pixels = raymarch_once(
        &device,
        &queue,
        &pipelines,
        grid_dims,
        grid.indices(),
        grid.lut(),
        &uniform,
        size,
    );

    // The production floor, end to end: the 2D rasterizer at the lowest
    // reflectivity tilt, `resample_floor` onto this very grid's footprint,
    // and the raymarch compositing the volume over it — written as a third
    // frame beside the mask and the colour render.
    let floor_pixels = rustdar_radar::render::find_closest_elevation(
        &scan,
        rustdar_radar::types::RadarProduct::Reflectivity,
        0.0,
    )
    .and_then(|elevation| {
        rustdar_radar::render::render_radar_to_image(
            &scan,
            elevation,
            rustdar_radar::types::RadarProduct::Reflectivity,
            site_lat,
            site_lon,
        )
    })
    .and_then(|(image, max_range_km, _)| {
        rustdar_frontend::volume::floor::resample_floor(
            &image,
            max_range_km,
            site_lat,
            grid.x_range_km(),
            grid.y_range_km(),
        )
    })
    .and_then(|floor| {
        let texture = pipelines.upload_floor(&device, &queue, floor.size, &floor.rgba)?;
        uniform.map_floor = true;
        let volume = pipelines
            .upload_volume(&device, &queue, grid_dims, grid.indices(), grid.lut())
            .expect("the grid uploads twice as readily as once");
        volume.write_uniform(&queue, &uniform);
        let target = pipelines.create_offscreen(&device, size);
        let mut encoder = device.create_command_encoder(&Default::default());
        pipelines.encode_raymarch_with_floor(&mut encoder, &target, &volume, Some(&texture));
        queue.submit(Some(encoder.finish()));
        Some(read_back(&device, &queue, target.texture(), size))
    });

    let mask: Vec<u8> = mask_pixels.iter().map(|px| px[3]).collect();
    let masked = mask.iter().filter(|&&a| a > 0).count();
    let masked_solid = mask.iter().filter(|&&a| a >= 128).count();
    let occupied = grid.indices().iter().filter(|&&i| i != 0).count();
    let above_cut = grid
        .indices()
        .iter()
        .filter(|&&i| i != 0 && i >= cut_index)
        .count();

    write_pgm(&format!("{out_prefix}_mask.pgm"), size, &mask);
    write_ppm(&format!("{out_prefix}_colour.ppm"), size, &colour_pixels);
    if let Some(floor_pixels) = &floor_pixels {
        write_ppm(&format!("{out_prefix}_floor.ppm"), size, floor_pixels);
    }

    let (x0, x1) = grid.x_range_km();
    let (y0, y1) = grid.y_range_km();
    let (z0, z1) = grid.z_range_km_msl();
    let (value_lo, value_hi) = grid.value_range();
    let (grid_lat, grid_lon) = grid.site();
    let meta = format!(
        "volume            {}\n\
         site              {site_name} at {grid_lat:.5}, {grid_lon:.5}\n\
         product           {} ({})\n\
         centre            {:.5}, {:.5}\n\
         half_width_km     {:.3} (requested {:.3}, clamped by build_voxels)\n\
         grid              nx {} ny {} nz {}  ({} cells)\n\
         x_range_km        {x0:.3} .. {x1:.3}  (east of site)\n\
         y_range_km        {y0:.3} .. {y1:.3}  (north of site)\n\
         z_range_km_msl    {z0:.3} .. {z1:.3}\n\
         box_size_km       [{:.3}, {:.3}, {:.3}]  (true extent, as passed to the uniform)\n\
         tilt_count        {}\n\
         widest_tilt_gap   {:.3} deg\n\
         value_range       {value_lo:.4} .. {value_hi:.4} ({})\n\
         fade_band         {}\n\
         cells_nonzero     {occupied} of {} ({:.3} %)\n\
         threshold         {threshold} -> palette index {cut_index} \
         (index_to_value = {:.4})\n\
         cells_at_or_above {above_cut} ({:.3} %)\n\
         camera            {camera_source}\n\
         box_from_clip     {box_from_clip:?}\n\
         eye_in_box        {eye_in_box:?}\n\
         size_px           {} x {}\n\
         extinction_mask   {extinction} per km\n\
         extinction_colour {} per km (production default)\n\
         mask_pixels       {masked} of {} ({:.4} %)\n\
         mask_pixels_a128  {masked_solid}\n\
         colour_background black\n",
        volume_path.display(),
        product.code(),
        product.name(),
        request.centre.0,
        request.centre.1,
        (x1 - x0) / 2.0,
        request.half_width_km,
        shape.nx,
        shape.ny,
        shape.nz,
        shape.cells(),
        box_size_km[0],
        box_size_km[1],
        box_size_km[2],
        grid.tilt_count(),
        grid.widest_tilt_gap_deg(),
        product.name(),
        grid.fade_band(),
        shape.cells(),
        100.0 * occupied as f64 / shape.cells() as f64,
        grid.index_to_value(cut_index),
        100.0 * above_cut as f64 / shape.cells() as f64,
        size[0],
        size[1],
        rustdar_frontend::volume::uniform::DEFAULT_EXTINCTION_PER_KM,
        mask.len(),
        100.0 * masked as f64 / mask.len() as f64,
    );
    let meta_path = format!("{out_prefix}_meta.txt");
    std::fs::write(&meta_path, &meta).unwrap_or_else(|e| panic!("writing {meta_path}: {e}"));
    println!("{meta}");
    println!(
        "wrote {out_prefix}_mask.pgm, {out_prefix}_colour.ppm{}, {meta_path}",
        if floor_pixels.is_some() {
            format!(", {out_prefix}_floor.ppm")
        } else {
            String::new()
        },
    );
}

// ── The volume ───────────────────────────────────────────────────────────────

/// Decode a whole Level II archive file into a `Scan`.
///
/// **Not** `nexrad_data::volume::File::scan`, which is what
/// `rustdar-radar/examples/render_product.rs` uses: `nexrad-data` is
/// deliberately not a dependency of `rustdar-frontend` (its manifest says so in
/// as many words), so the only route from bytes to a `Scan` this crate's
/// dependency set offers is `rustdar_radar::chunks::decode_chunk` — which
/// dispatches on the `AR2` magic and walks exactly the same records — plus
/// `nexrad_model`'s own `Sweep::from_radials`. That pair is what `File::scan`
/// does internally, minus the `Site` block, which `build_voxels` does not read.
fn scan_from_archive(path: &std::path::Path) -> nexrad_model::data::Scan {
    let bytes =
        std::fs::read(path).unwrap_or_else(|e| panic!("reading VOL {}: {e}", path.display()));
    assert!(
        !bytes.starts_with(&[0x1f, 0x8b]),
        "{} is gzipped. Level II reaches this crate through nexrad-data's \
         bzip2-per-record framing and nothing in rustdar-frontend's dependency \
         set can gunzip a whole file; run `gunzip` on it first.",
        path.display(),
    );
    let name = path
        .file_name()
        .and_then(std::ffi::OsStr::to_str)
        .unwrap_or("volume");
    let contents = rustdar_radar::chunks::decode_chunk(name, &bytes)
        .unwrap_or_else(|e| panic!("decoding {}: {e}", path.display()));
    let coverage_pattern = contents.coverage_pattern.unwrap_or_else(|| {
        panic!(
            "{} carries no message 5, so there is no tilt ladder and \
             VolumeSampler would refuse it",
            path.display(),
        )
    });
    let sweeps = nexrad_model::data::Sweep::from_radials(contents.radials);
    assert!(
        !sweeps.is_empty(),
        "{} decoded to no sweeps",
        path.display()
    );
    nexrad_model::data::Scan::new(coverage_pattern, sweeps)
}

/// The radar's ICAO and position: `SITE`, or the file name's first four
/// characters.
///
/// `build_voxels`' `lat`/`lon` are the **radar's**, not the region centre's —
/// the whole grid is expressed as kilometres from the site — so getting this
/// from the region centre instead would silently move every voxel.
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

/// The box's true physical extent in kilometres, exactly as
/// `volume_bridge::box_size_km` computes it.
fn box_size_km(grid: &VoxelGrid) -> [f32; 3] {
    let (x0, x1) = grid.x_range_km();
    let (y0, y1) = grid.y_range_km();
    let (z0, z1) = grid.z_range_km_msl();
    [(x1 - x0) as f32, (y1 - y0) as f32, (z1 - z0) as f32]
}

/// A palette that is opaque at or above `threshold` and absent below it, plus
/// the index the cut landed on.
///
/// Index 0 is `[0, 0, 0, 0]` whatever the threshold: it is both the bottom of
/// the affine ramp and the no-data value, so painting it would paint every cell
/// the radar never looked at.
fn hard_lut(grid: &VoxelGrid, threshold: f32) -> (Vec<u8>, u8) {
    let mut lut = vec![0u8; VOLUME_LUT_BYTES];
    let mut cut = None;
    for index in 1..=u8::MAX {
        if grid.index_to_value(index) < threshold {
            continue;
        }
        cut.get_or_insert(index);
        let at = index as usize * 4;
        lut[at..at + 4].copy_from_slice(&[255, 255, 255, 255]);
    }
    let (lo, hi) = grid.value_range();
    let cut = cut.unwrap_or_else(|| {
        panic!("THRESH {threshold} is above every palette entry; the ramp is {lo} .. {hi}")
    });
    (lut, cut)
}

// ── The camera ───────────────────────────────────────────────────────────────

/// `box_from_clip`, `eye_in_box` and the vertical exaggeration, from `CAM` or
/// from the orbit camera.
///
/// The `CAM` path reports exaggeration 1.0: the file holds a finished matrix
/// with the stretch already baked in, and there is no way to recover the knob
/// from it. The shading is then lit against the true geometry, which is what
/// 1.0 means.
fn camera(box_size_km: [f32; 3], size: [u32; 2]) -> (String, [[f32; 4]; 4], [f32; 3], f32) {
    if let Ok(path) = std::env::var("CAM") {
        let text =
            std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("reading CAM {path}: {e}"));
        let numbers: Vec<f32> = text
            .split_whitespace()
            .map(|field| {
                field
                    .parse()
                    .unwrap_or_else(|e| panic!("CAM {path}: {field:?} is not an f32: {e}"))
            })
            .collect();
        assert_eq!(
            numbers.len(),
            19,
            "CAM {path} holds {} numbers; it must hold 19 — 16 column-major \
             box_from_clip then 3 eye_in_box",
            numbers.len(),
        );
        assert!(
            numbers.iter().all(|n| n.is_finite()),
            "CAM {path} holds a non-finite number; the GPU would render an \
             empty pane and report nothing"
        );
        let mut matrix = [[0.0f32; 4]; 4];
        for (lane, &value) in numbers[..16].iter().enumerate() {
            // Column-major: lane 0..4 is column 0, and `box_from_clip` is
            // indexed `m[column][row]`.
            matrix[lane / 4][lane % 4] = value;
        }
        let eye = [numbers[16], numbers[17], numbers[18]];
        return (format!("CAM {path}"), matrix, eye, 1.0);
    }

    let yaw = parsed_or("YAW", 225.0f32);
    let pitch = parsed_or("PITCH", 25.0f32);
    let distance = parsed_or("DIST", 2.5f32);
    let exaggeration = parsed_or("EXAG", 3.0f32);
    let camera = OrbitCamera::restore(yaw, pitch, distance, [0.0; 3], exaggeration)
        .expect("YAW/PITCH/DIST/EXAG must all be finite");
    let aspect = size[0] as f32 / size[1] as f32;
    let view = view_for(camera, box_size_km, aspect)
        .expect("view_for refused the box or the aspect ratio");
    (
        format!(
            "view_for(yaw {}, pitch {}, distance {}, exaggeration {})",
            camera.yaw_deg(),
            camera.pitch_deg(),
            camera.eye_distance(),
            camera.vertical_exaggeration(),
        ),
        view.box_from_clip,
        view.eye_in_box,
        camera.vertical_exaggeration(),
    )
}

// ── The GPU, copied from `tests/volume_gpu.rs` ───────────────────────────────

/// A device on whatever adapter `WGPU_BACKEND` selects — the application's own
/// constructor.
fn device() -> (wgpu::Device, wgpu::Queue) {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::default(),
        force_fallback_adapter: false,
        compatible_surface: None,
    }))
    .expect("no wgpu adapter; this test is ignored by default for that reason");
    pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("rustdar.volume.mask.device"),
        required_features: wgpu::Features::empty(),
        required_limits: adapter.limits(),
        memory_hints: Default::default(),
        experimental_features: Default::default(),
        trace: Default::default(),
    }))
    .expect("could not create a device on an adapter that was found")
}

/// The egui pass a blit would be composited into. Only the blit pipeline reads
/// it; the raymarch targets its own offscreen.
fn attachments() -> AttachmentConfig {
    AttachmentConfig {
        color_format: wgpu::TextureFormat::Bgra8Unorm,
        depth_format: None,
        msaa_samples: 1,
    }
}

/// Render one raymarched frame into a fresh offscreen and read it back.
#[allow(clippy::too_many_arguments)]
fn raymarch_once(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    pipelines: &VolumePipelines,
    cells: [u32; 3],
    indices: &[u8],
    lut: &[u8],
    uniform: &VolumeUniform,
    size: [u32; 2],
) -> Vec<[u8; 4]> {
    let volume = pipelines
        .upload_volume(device, queue, cells, indices, lut)
        .expect("the grid and palette were refused");
    volume.write_uniform(queue, uniform);
    let target = pipelines.create_offscreen(device, size);

    let mut encoder = device.create_command_encoder(&Default::default());
    pipelines.encode_raymarch(&mut encoder, &target, &volume);
    queue.submit(Some(encoder.finish()));

    read_back(device, queue, target.texture(), size)
}

/// Read an RGBA8 texture back as one `[u8; 4]` per texel, row-major.
///
/// `copy_texture_to_buffer` wants rows padded to
/// `COPY_BYTES_PER_ROW_ALIGNMENT`, so the padding is added on the way out and
/// stripped on the way back.
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
        label: Some("rustdar.volume.mask.readback"),
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

// ── Output, by hand ──────────────────────────────────────────────────────────

/// One 8-bit grey plane, binary P5. Row 0 is the top, which is the order
/// `read_back` produces and the order a render target is in.
fn write_pgm(path: &str, size: [u32; 2], grey: &[u8]) {
    assert_eq!(grey.len(), (size[0] * size[1]) as usize);
    let mut out = format!("P5\n{} {}\n255\n", size[0], size[1]).into_bytes();
    out.extend_from_slice(grey);
    std::fs::write(path, out).unwrap_or_else(|e| panic!("writing {path}: {e}"));
}

/// The offscreen's colour, binary P6.
///
/// The shader's output is gamma-encoded and **premultiplied**. Compositing over
/// black is `rgb·a + 0·(1 − a)`, which is the premultiplied value itself — so
/// the bytes go out unchanged and the un-premultiply cancels rather than being
/// skipped. Over any other background it would not, which is why the background
/// is named here and in the meta block.
fn write_ppm(path: &str, size: [u32; 2], pixels: &[[u8; 4]]) {
    assert_eq!(pixels.len(), (size[0] * size[1]) as usize);
    let mut out = format!("P6\n{} {}\n255\n", size[0], size[1]).into_bytes();
    for pixel in pixels {
        out.extend_from_slice(&pixel[..3]);
    }
    std::fs::write(path, out).unwrap_or_else(|e| panic!("writing {path}: {e}"));
}

// ── The environment ──────────────────────────────────────────────────────────

/// A variable that has no sensible default.
fn required(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("{name} is required; see this file's module doc"))
}

/// A required variable, parsed.
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

/// An optional variable, parsed, or `fallback`.
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

/// `PRODUCT`, restricted to what `rustdar_radar::sampler::samplable` accepts —
/// anything else makes `build_voxels` return `None` with no explanation here.
fn product_from_env() -> RadarProduct {
    let raw = std::env::var("PRODUCT").unwrap_or_else(|_| "BR".to_owned());
    match raw.trim().to_ascii_uppercase().as_str() {
        "BR" | "REF" | "REFLECTIVITY" => RadarProduct::Reflectivity,
        "BV" | "VEL" | "VELOCITY" => RadarProduct::Velocity,
        "SW" | "SPECTRUMWIDTH" => RadarProduct::SpectrumWidth,
        "ZDR" => RadarProduct::DifferentialReflectivity,
        "PHI" | "PHIDP" => RadarProduct::DifferentialPhase,
        "RHO" | "CC" => RadarProduct::CorrelationCoefficient,
        other => panic!(
            "PRODUCT={other} is not a moment a volume can be resampled from; \
             use BR, BV, SW, ZDR, PHI or CC"
        ),
    }
}

/// `SIZE`, as `WxH`.
fn size_from_env() -> [u32; 2] {
    let raw = std::env::var("SIZE").unwrap_or_else(|_| "1200x900".to_owned());
    let (width, height) = raw
        .trim()
        .split_once(['x', 'X'])
        .unwrap_or_else(|| panic!("SIZE={raw:?} is not WxH"));
    let parse = |field: &str| -> u32 {
        field
            .parse()
            .unwrap_or_else(|e| panic!("SIZE={raw:?}: {field:?} is not a u32: {e}"))
    };
    let size = [parse(width), parse(height)];
    assert!(
        size[0] > 0 && size[1] > 0,
        "SIZE={raw:?} has a zero axis; there is nothing to render into"
    );
    size
}
