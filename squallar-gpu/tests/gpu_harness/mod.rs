//! The adapter, the readback and the planted fixtures every GPU test file
//! shares.
#![allow(dead_code)]

use egui_wgpu::wgpu;
use squallar_device_profile::constants::VOLUME_LUT_BYTES;
use squallar_gpu::egui_renderer::AttachmentConfig;
use squallar_volumetric::raymarch::staging::{STAGING_RING_FEATURE, VolumeStaging};
use squallar_volumetric::raymarch::{
    CoarseLevel, FLOOR_FORMAT, OffscreenPlan, PaneMirror, VolumePipelines,
};
use squallar_volumetric::uniform::VolumeUniform;

/// Held for the length of a test, so only one talks to the GPU at a time.
static ONE_AT_A_TIME: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Take the GPU lock, ignoring poisoning.
pub fn gpu_lock() -> std::sync::MutexGuard<'static, ()> {
    ONE_AT_A_TIME
        .lock()
        .unwrap_or_else(|held| held.into_inner())
}

/// Name the adapter these tests actually got, once per process.
pub fn announce(adapter: &wgpu::Adapter) {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let info = adapter.get_info();
        eprintln!(
            "wgpu adapter: {:?} {:?} \"{}\" (driver: {} {})",
            info.backend, info.device_type, info.name, info.driver, info.driver_info
        );
    });
}

/// A device on whatever adapter is to be had.
pub fn device() -> (wgpu::Device, wgpu::Queue) {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::default(),
        force_fallback_adapter: false,
        compatible_surface: None,
    }))
    .expect("no wgpu adapter; these tests are ignored by default for that reason");
    announce(&adapter);
    pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("squallar.volume.test.device"),
        // The one feature production asks for, on the same terms: only where
        // the adapter already has it. Without this the device would refuse to
        // create a `MAP_READ | COPY_SRC` buffer and every suite sharing this
        // harness would test the fallback while production shipped the ring.
        required_features: adapter.features() & STAGING_RING_FEATURE,
        // Deliberately the adapter's own, not the WebGL2 floor: what is being
        // checked here is that the shader works, and holding a desktop GPU to
        // the browser's limits would only test the limits.
        required_limits: adapter.limits(),
        memory_hints: Default::default(),
        experimental_features: Default::default(),
        trace: Default::default(),
    }))
    .expect("could not create a device on an adapter that was found")
}

/// The egui pass a blit would be composited into, at one colour format.
pub fn attachments(color_format: wgpu::TextureFormat) -> AttachmentConfig {
    AttachmentConfig {
        color_format,
        depth_format: None,
        msaa_samples: 1,
    }
}

/// A texture that can be rendered into and read back.
pub fn render_target(
    device: &wgpu::Device,
    format: wgpu::TextureFormat,
    size: [u32; 2],
) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some("squallar.volume.test.target"),
        size: wgpu::Extent3d {
            width: size[0],
            height: size[1],
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

/// Read an RGBA8 texture back as one `[u8; 4]` per texel, row-major.
pub fn read_back(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    texture: &wgpu::Texture,
    size: [u32; 2],
) -> Vec<[u8; 4]> {
    let unpadded = size[0] * 4;
    let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let padded = unpadded.div_ceil(align) * align;

    let staging = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("squallar.volume.test.readback"),
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

/// A `box_from_clip` that unprojects the far plane onto the far face of the
/// box, looking down one axis.
pub fn box_from_clip_down(axis: usize) -> [[f32; 4]; 4] {
    // The two axes the screen spans, in order.
    let screen: [usize; 2] = match axis {
        0 => [1, 2],
        1 => [0, 2],
        _ => [0, 1],
    };
    let mut matrix = [[0.0f32; 4]; 4];
    // ndc.x and ndc.y map [-1, 1] onto [0, 1] of the two screen axes.
    matrix[0][screen[0]] = 0.5;
    matrix[1][screen[1]] = 0.5;
    matrix[3][screen[0]] = 0.5;
    matrix[3][screen[1]] = 0.5;
    // Depth 1 (the far plane) lands one box beyond the far face, so a ray from
    // an eye outside the near face crosses the whole box.
    matrix[2][axis] = -2.5;
    matrix[3][axis] = 1.5;
    matrix[3][3] = 1.0;
    matrix
}

/// The eye that goes with [`box_from_clip_down`]: outside the near face.
pub fn eye_outside(axis: usize) -> [f32; 3] {
    let mut eye = [0.5f32; 3];
    eye[axis] = 3.0;
    eye
}

/// A palette where one entry is `colour` and everything else is transparent.
pub fn palette(index: u8, colour: [u8; 4]) -> Vec<u8> {
    let mut lut = vec![0u8; VOLUME_LUT_BYTES];
    let at = index as usize * 4;
    lut[at..at + 4].copy_from_slice(&colour);
    lut
}

/// A palette that is opaque white at every index but the no-data 0.
pub fn opaque_white_lut() -> Vec<u8> {
    let mut lut = vec![0u8; VOLUME_LUT_BYTES];
    for entry in 1..lut.len() / 4 {
        lut[entry * 4..entry * 4 + 4].copy_from_slice(&[255, 255, 255, 255]);
    }
    lut
}

/// A grey ramp table: entry `i` is the colour `(i, i, i)`, opaque.
pub fn grey_ramp_lut() -> Vec<u8> {
    let mut lut = vec![0u8; VOLUME_LUT_BYTES];
    for (i, entry) in lut.chunks_exact_mut(4).enumerate() {
        let level = i as u8;
        entry.copy_from_slice(&[level, level, level, 255]);
    }
    // Entry 0 is the no-data index and is transparent in every real table.
    lut[0..4].copy_from_slice(&[0, 0, 0, 0]);
    lut
}

/// An `8 x 8 x nz` grid whose index depends only on the slab: `levels[k]` is
/// the value of every cell in slab `k`.
pub fn slab_ramp(levels: &[u8]) -> ([u32; 3], Vec<u8>) {
    let cells = [8u32, 8, levels.len() as u32];
    let mut indices = Vec::with_capacity((cells[0] * cells[1] * cells[2]) as usize);
    for level in levels {
        indices.extend(std::iter::repeat_n(*level, (cells[0] * cells[1]) as usize));
    }
    (cells, indices)
}

/// A uniform ready for an isosurface measurement: ambient light only, so
/// shading is exactly 1, and no index band skipped.
pub fn iso_uniform(cells: [u32; 3]) -> VolumeUniform {
    let mut uniform = VolumeUniform::new([10.0, 10.0, 10.0], cells);
    uniform.box_from_clip = box_from_clip_down(2);
    uniform.eye_in_box = eye_outside(2);
    uniform.ambient = 1.0;
    uniform.empty_index_threshold = 0.5 / 255.0;
    uniform
}

/// Render one raymarched frame and read it back, with the grid's coarse level
/// built — [`raymarch_once_at`] at [`CoarseLevel::Built`].
#[allow(clippy::too_many_arguments)]
pub fn raymarch_once(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    pipelines: &VolumePipelines,
    cells: [u32; 3],
    indices: &[u8],
    lut: &[u8],
    uniform: &VolumeUniform,
    size: [u32; 2],
) -> Vec<[u8; 4]> {
    raymarch_once_at(
        device,
        queue,
        pipelines,
        cells,
        indices,
        lut,
        uniform,
        size,
        CoarseLevel::Built,
    )
}

/// [`raymarch_once`], told whether this grid is uploaded with a coarse mip
/// level at all.
#[allow(clippy::too_many_arguments)]
pub fn raymarch_once_at(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    pipelines: &VolumePipelines,
    cells: [u32; 3],
    indices: &[u8],
    lut: &[u8],
    uniform: &VolumeUniform,
    size: [u32; 2],
    coarse: CoarseLevel,
) -> Vec<[u8; 4]> {
    let volume = pipelines
        .upload_volume_at(
            device,
            queue,
            cells,
            indices,
            lut,
            coarse,
            // Staging for the device the caller actually has, exactly as
            // production builds it — so on an adapter with
            // `STAGING_RING_FEATURE` every render in these suites arrives
            // through the staging ring, and on one without it through
            // `write_texture`. Handing in `VolumeStaging::default()` here would
            // be cheaper and would quietly take the fallback on every machine,
            // leaving the route production uses covered by nothing that draws.
            &mut VolumeStaging::new(device),
        )
        .expect("the grid and palette were refused");
    assert_eq!(
        volume.cells(),
        cells,
        "the uploaded grid does not report the shape it was given, so the \
         uniform block's grid_dims would describe a different texture"
    );
    volume.write_uniform(queue, uniform);
    let target = pipelines.create_offscreen(device, OffscreenPlan::native(size));
    assert_eq!(
        target.size(),
        size,
        "the offscreen does not report the size it was created at"
    );

    let mut encoder = device.create_command_encoder(&Default::default());
    pipelines.encode_raymarch(&mut encoder, &target, &volume);
    queue.submit(Some(encoder.finish()));

    read_back(device, queue, target.texture(), size)
}

/// The texel format every mirror built here is created in.
pub const MIRROR_FORMAT: wgpu::TextureFormat = FLOOR_FORMAT;

/// A mirror of `size` texels holding `rgba`, through the very same
/// `ensure_mirror` texture and bind group the frame path draws into.
pub fn planted_mirror(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    pipelines: &VolumePipelines,
    size: [u32; 2],
    rgba: &[u8],
) -> PaneMirror {
    let mut mirror = None;
    assert!(
        pipelines.ensure_mirror(device, &mut mirror, size, MIRROR_FORMAT),
        "ensure_mirror declined to create a mirror where there was none",
    );
    let mirror = mirror.expect("ensure_mirror reported a creation and left nothing behind");
    assert_eq!(
        mirror.size(),
        size,
        "the mirror is not the size it was asked for"
    );
    assert!(
        pipelines.write_mirror(queue, &mirror, rgba),
        "write_mirror refused a fixture of {} bytes for a {size:?} mirror",
        rgba.len(),
    );
    mirror
}

/// [`raymarch_once`], with a pane mirror bound at group 1 for the floor.
#[allow(clippy::too_many_arguments)]
pub fn raymarch_once_with_floor(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    pipelines: &VolumePipelines,
    cells: [u32; 3],
    indices: &[u8],
    lut: &[u8],
    uniform: &VolumeUniform,
    size: [u32; 2],
    floor: &PaneMirror,
) -> Vec<[u8; 4]> {
    let volume = pipelines
        .upload_volume(
            device,
            queue,
            cells,
            indices,
            lut,
            &mut VolumeStaging::new(device),
        )
        .expect("the grid and palette were refused");
    volume.write_uniform(queue, uniform);
    let target = pipelines.create_offscreen(device, OffscreenPlan::native(size));
    let mut encoder = device.create_command_encoder(&Default::default());
    pipelines.encode_raymarch_with_floor(&mut encoder, &target, &volume, Some(floor));
    queue.submit(Some(encoder.finish()));
    read_back(device, queue, target.texture(), size)
}

/// The pixel at the centre of a `size`-shaped image.
pub fn centre(pixels: &[[u8; 4]], size: [u32; 2]) -> [u8; 4] {
    pixels[((size[1] / 2) * size[0] + size[0] / 2) as usize]
}

/// One orbit camera: `(yaw degrees, pitch degrees, standoff, vertical
/// exaggeration)`.
pub type OrbitFixture = (f32, f32, f32, f32);

/// **The eleven cameras every ground-pass criterion is asserted over.**
///
/// Spread over yaw, pitch, standoff and exaggeration, and — against the box
/// `volume_occluder.rs` uses — across all three regions the composite
/// distinguishes: above the ground's crest, between the crest and the box
/// floor, and under the box floor. Which region a fixture lands in is a whole
/// camera pipeline away from being obvious, so the file that depends on the
/// split asserts it rather than assuming it
/// (`the_camera_set_reaches_above_the_crest_under_it_and_under_the_floor`).
///
/// It lives here rather than in one suite because two suites read it, and two
/// lists that have to agree is what this repository keeps removing.
pub const ORBIT_CAMERAS: [OrbitFixture; 11] = [
    // -- Above the crest. B1's six, unchanged. --
    // The original fixture: obliquely down from the south-west.
    (215.0, 28.0, 2.2, 1.0),
    // The other side, closer, low enough that rays cross the ridge at a slant.
    (35.0, 12.0, 1.0, 1.0),
    // Steep and vertically exaggerated — the shipped default look.
    (140.0, 60.0, 0.8, 3.0),
    // Grazing: eye z ~ 3.1.
    (300.0, 8.0, 2.2, 1.0),
    // Near-overhead, where the ridge's silhouette is at its smallest.
    (0.0, 85.0, 1.5, 1.0),
    // Inside the box at the zoom stop, eye z ~ 0.70 — above the crest, but only
    // just, and with the near plane much closer than anywhere else here.
    (75.0, 28.0, 0.05, 1.0),
    // -- Between the crest and the box floor. Both are close standoffs
    // deliberately: the band is only about 1.6 degrees of pitch wide at
    // standoff 2.2, and a level camera that far out sees the box edge-on. --
    //
    // Eye z 0.056 against a 0.25 crest.
    (300.0, -5.0, 0.6, 1.0),
    // Eye z 0.204, exaggerated 3x and further off.
    (35.0, -6.0, 1.0, 3.0),
    // -- Under the box floor. B1's three pinned-hole cameras, promoted whole. --
    //
    // The reviewer's own camera: eye z -5.27.
    (215.0, -18.0, 2.2, 1.0),
    // Deeper and from the other side: eye z -11.50.
    (35.0, -40.0, 2.2, 1.0),
    // Straight up at the clamp stop, `MAX_PITCH_DEG` all the way over.
    (140.0, -89.0, 1.0, 1.0),
];

/// The box side that spans exactly one degree of latitude, in kilometres.
pub const DEGREE_BOX_KM: f32 = squallar_geo::KM_PER_DEGREE_LAT as f32;

/// Web Mercator's y at a latitude in degrees: `ln(tan(pi/4 + phi/2))`.
pub fn mercator_y(lat_deg: f64) -> f64 {
    (std::f64::consts::FRAC_PI_4 + lat_deg.to_radians() / 2.0)
        .tan()
        .ln()
}

/// The uniform's two floor lanes for a `DEGREE_BOX_KM`-square box whose site is
/// at its centre **on the equator**, arranged so the box's footprint covers
/// exactly the whole mirror with the mirror's row 0 along the box's north edge.
pub fn equatorial_floor_lanes(gamma_encoded: bool) -> ([f32; 4], [f32; 4]) {
    equatorial_floor_lanes_of(1.0, 1.0, gamma_encoded)
}

/// [`equatorial_floor_lanes`] for a box `east_degrees` of longitude wide and
/// `north_degrees` of latitude tall.
pub fn equatorial_floor_lanes_of(
    east_degrees: f64,
    north_degrees: f64,
    gamma_encoded: bool,
) -> ([f32; 4], [f32; 4]) {
    // v grows downward through the mirror and Mercator y grows north, so the
    // rate is negative; its magnitude is one whole mirror over the Mercator
    // span of the box's latitude. Derived from `mercator_y` rather than
    // written down: at one degree it comes out at -57.29505, and a reader who
    // wants to know why *that* number should be able to see the two calls it
    // came from.
    let half_north = north_degrees / 2.0;
    let v_per_mercator_y = -1.0 / (mercator_y(half_north) - mercator_y(-half_north));
    (
        // u at the site, v at the site, u per degree of longitude east, v per
        // unit of Mercator y. The site is the mirror's centre, and the box's
        // full width in longitude — `east_degrees` at the equator — is one
        // whole mirror across.
        [
            0.5,
            0.5,
            (1.0 / east_degrees) as f32,
            v_per_mercator_y as f32,
        ],
        // Site latitude, then the box's west and south edges as kilometres
        // east and north of it: the site is the box's centre, so both are half
        // a side to the negative.
        [
            0.0,
            (-east_degrees / 2.0 * f64::from(DEGREE_BOX_KM)) as f32,
            (-north_degrees / 2.0 * f64::from(DEGREE_BOX_KM)) as f32,
            if gamma_encoded { 1.0 } else { 0.0 },
        ],
    )
}

/// The box extent [`equatorial_floor_lanes`] is written for.
pub const fn equatorial_box_km() -> [f32; 3] {
    [DEGREE_BOX_KM, DEGREE_BOX_KM, 10.0]
}

/// The box extent [`equatorial_floor_lanes_of`] is written for, in the same
/// two spans. `equatorial_box_km_of(1.0, 1.0)` is [`equatorial_box_km`], and
/// `the_wide_lanes_are_the_square_ones_at_one_degree` holds them to it.
pub fn equatorial_box_km_of(east_degrees: f64, north_degrees: f64) -> [f32; 3] {
    [
        (east_degrees * f64::from(DEGREE_BOX_KM)) as f32,
        (north_degrees * f64::from(DEGREE_BOX_KM)) as f32,
        10.0,
    ]
}
