//! **Are the ground and the volume lit by the same light?**
//!
//! C2's central criterion, and the shape of failure it is aimed at is not a
//! wrong number: it is a warm sunset ground standing under a neutral-white
//! storm, which reads as two pictures composited rather than as one scene.
//! Every pixel in that frame is plausible on its own, so nothing that looks at
//! one surface can see it.
//!
//! ```text
//! cargo test -p squallar-gpu --test volume_light -- --ignored --nocapture
//! ```
//!
//! `#[ignore]`d (CI's `gpu` job opts in on lavapipe), and the tests hold the
//! shared process-wide GPU lock, like every other suite in this directory.
//! [`the_criterion_rejects_a_build_where_only_one_surface_takes_the_tint`] and
//! [`the_identity_criterion_rejects_a_light_that_does_nothing`] are `#[ignore]`d
//! for the same reason.
//!
//! # What is measured, and what its denominator is
//!
//! Two readings a camera, and **they never share a denominator**:
//!
//! * the **ground** reading is the mean linear RGB of the *ground attachment*
//!   over the pixels the mesh drew — the lit drape alone, with no volume in
//!   front of it and no lid behind it;
//! * the **volume** reading is the mean RGB of the *composited offscreen* over
//!   the painted pixels of a frame rendered with **no ground pass at all** — the
//!   accumulation alone.
//!
//! Two renders, because one frame cannot separate them: the mesh's apron
//! covers the whole drawn box, so there is no pixel in a ground frame carrying
//! volume and nothing else.
//!
//! # The non-triviality half
//!
//! "Both surfaces changed" passes trivially against a shader that lights only
//! one of them if the other happens to move for some unrelated reason. So the
//! honest control is the build that *has* the defect:
//! [`the_criterion_rejects_a_build_where_only_one_surface_takes_the_tint`]
//! constructs both halves of it — the march bypassing `lit`, and the ground
//! fragment bypassing it — and requires the criterion to go red on each.
//!
//! And [`the_headlight_light_collapses_to_the_identity`] would pass against a
//! renderer whose light did nothing at all, so
//! [`the_identity_criterion_rejects_a_light_that_does_nothing`] requires the
//! same two builds to *differ* under the sun.
#![cfg(not(target_arch = "wasm32"))]

use egui_wgpu::wgpu;
use squallar_device_profile::quality::{GroundPass, ResolutionRung};
use squallar_egui::pane::OrbitCamera;
use squallar_egui::volume_view::{VolumeView, view_for};
use squallar_volumetric::raymarch::staging::VolumeStaging;
use squallar_volumetric::raymarch::{
    GroundHeights, OffscreenPlan, PaneMirror, VOLUME_SHADER_WGSL, VolumePipelines,
    post_center_fraction,
};
use squallar_volumetric::uniform::{HEADLIGHT, SurfaceLight, VolumeUniform};

mod gpu_harness;
use gpu_harness::{
    MIRROR_FORMAT, ORBIT_CAMERAS, OrbitFixture, attachments, device, gpu_lock, mercator_y,
    planted_mirror, read_back,
};

/// The offscreen every frame here is rendered at. Smaller than
/// `volume_drape.rs` needs: nothing here identifies a pixel with a place, so
/// the resolution only has to be enough to average over.
const SIZE: [u32; 2] = [320, 200];

/// The acceptance site the plan names for "it works": the Colorado front
/// range, where the relief is real.
const SITE: (f64, f64) = (39.0, -106.0);

/// The shipped 920 km reflectivity reach, square, 20 km tall.
const BOX_KM: [f32; 3] = [920.0, 920.0, 20.0];

/// Mirror texels a side.
const MIRROR_EDGE: u32 = 256;

/// The height field's posts a side.
const POSTS: u32 = 128;

/// The ridge's crest, in box `z`. The same fixture shape `volume_drape.rs`
/// uses, and for the same reason: a mesh flat at `z = 0` is indistinguishable
/// from the lid, and a flat mesh has one normal so nothing here could see a
/// slope.
const RIDGE: f32 = 0.35;

/// Width of the ridge, as a fraction of the box's east-west extent.
const RIDGE_SIGMA: f32 = 0.18;

/// The height encoding: box `z` is the raw sample over the `u16` ceiling.
const HEIGHT_SCALE: f32 = 1.0 / 65_535.0;

/// The **west-facing ramp**'s foot and its total rise, in box `z`.
///
/// A second fixture beside the ridge, and it exists because the ridge cannot
/// answer the question [`the_mesh_is_lit_by_the_shape_it_is_drawn_as`] asks
/// (`#[ignore]`d, like everything here). A
/// symmetric ridge has one flank facing each way, so *every* statistic over it
/// is symmetric under swapping the light east for west — which is exactly the
/// swap an inverted normal performs. A ground that rises steadily west to east
/// faces west at every post, so the mean over the whole mesh has a side.
const RAMP_BASE: f32 = 0.1;
const RAMP_RISE: f32 = 0.6;

// ---------------------------------------------------------------------------
// The lights
// ---------------------------------------------------------------------------

/// Solar elevations the ramp is exercised at, and what each one is.
///
/// Named rather than swept: the ramp's knots are the interesting places, and a
/// sweep would report a curve where what is wanted is "this is what a sunset
/// does to this basemap".
const SUN_ELEVATIONS: [(f32, &str); 5] = [
    (60.0, "high sun"),
    (10.0, "afternoon"),
    (2.0, "sunset"),
    (-3.0, "civil twilight"),
    (-20.0, "night"),
];

/// The day every instant here is searched inside: the June solstice, 2026,
/// as Unix seconds at 00:00 UTC.
///
/// The solstice because it is the day on which every elevation in
/// [`SUN_ELEVATIONS`] is reachable from [`SITE`]: upper culmination is
/// `90 - 39 + 23.4 = 74.4` degrees and lower is `39 + 23.4 - 90 = -27.6`, so
/// 60 above and 20 below both happen. [`instant_at`] asserts what it found
/// rather than trusting that.
const SOLSTICE_UTC: f64 = 1_782_000_000.0;

/// **The instant the sun really is `target_deg` above [`SITE`]**, on the
/// morning or the evening side of local noon.
///
/// A search over a real day rather than a hand-built `SunLight`, so every
/// light this file renders under has come through the whole shipped path —
/// `solar_position`, the two ramps, the white balance, the east-north-up to
/// box mapping and `surface_light_for`. A fixture assembled here would be a
/// second spelling of that path, written to agree with it.
fn instant_at(target_deg: f64, morning: bool) -> f64 {
    let elevation = |minute: i64| {
        squallar_geo::solar::solar_position(SITE.0, SITE.1, SOLSTICE_UTC + minute as f64 * 60.0)
            .expect("the acceptance site and a 2026 instant are not refused")
            .elevation_deg
    };
    // Local noon by measurement, not by arithmetic on the longitude: it is
    // what splits the day into the rising half and the setting half.
    let noon = (0..1_440)
        .max_by(|a, b| elevation(*a).total_cmp(&elevation(*b)))
        .expect("a day has minutes");
    // Twelve hours either side of local noon, which is NOT the UTC day: at
    // this site solar noon is 19:04 UTC, so a search bounded by the UTC day
    // would offer the evening only five hours and never reach a sunset.
    let half: Vec<i64> = if morning {
        (noon - 720..=noon).collect()
    } else {
        (noon..=noon + 720).collect()
    };
    let best = half
        .into_iter()
        .min_by(|a, b| {
            (elevation(*a) - target_deg)
                .abs()
                .total_cmp(&(elevation(*b) - target_deg).abs())
        })
        .expect("a half-day has minutes");
    let found = elevation(best);
    assert!(
        (found - target_deg).abs() < 0.2,
        "the closest the sun comes to {target_deg} degrees over the site on the \
         {} of the solstice is {found}, so this file would be measuring a \
         different sun from the one it names",
        if morning { "morning" } else { "evening" },
    );
    SOLSTICE_UTC + best as f64 * 60.0
}

/// The light at a solar elevation, through the whole production path.
fn sun_at(elevation_deg: f32) -> SurfaceLight {
    sun_on_side(elevation_deg, false)
}

/// [`sun_at`] on a named side of local noon, so two lights of the same height
/// can have mirrored azimuths.
fn sun_on_side(elevation_deg: f32, morning: bool) -> SurfaceLight {
    let light = squallar_egui::volume_view::volume_light(
        true,
        Some(squallar_geo::GeoPoint {
            lat: SITE.0,
            lon: SITE.1,
        }),
        instant_at(f64::from(elevation_deg), morning),
    );
    assert!(
        light.is_sun(),
        "the acceptance site at a 2026 instant was refused, so this file has \
         nothing to render under",
    );
    squallar_volumetric::bridge::surface_light_for(light)
}

/// The sunset the "both surfaces move" criterion is measured at. Two degrees:
/// the beam is at its most saturated there and still at full strength, which
/// is the largest colour move the ramp can make.
fn sunset() -> SurfaceLight {
    sun_at(2.0)
}

// ---------------------------------------------------------------------------
// The scene
// ---------------------------------------------------------------------------

fn view_at((yaw, pitch, distance, exaggeration): OrbitFixture) -> VolumeView {
    let camera =
        OrbitCamera::restore(yaw, pitch, distance, [0.0; 3], exaggeration).expect("finite camera");
    view_for(camera, BOX_KM, SIZE[0] as f32 / SIZE[1] as f32).expect("a view")
}

/// The fixture height field in box `z`, a north-south ridge.
fn ridge_height(uv: [f32; 2]) -> f32 {
    let d = (uv[0] - 0.5) / RIDGE_SIGMA;
    (RIDGE * (-0.5 * d * d).exp()).clamp(0.0, 1.0)
}

/// Ground rising steadily from the box's west edge to its east, so every post
/// of it faces west.
fn ramp_height(uv: [f32; 2]) -> f32 {
    (RAMP_BASE + RAMP_RISE * uv[0]).clamp(0.0, 1.0)
}

fn samples_of(height: impl Fn([f32; 2]) -> f32) -> Vec<u16> {
    let mut samples = Vec::with_capacity((POSTS * POSTS) as usize);
    for j in 0..POSTS {
        for i in 0..POSTS {
            let z = height([
                post_center_fraction(i, POSTS),
                post_center_fraction(j, POSTS),
            ]);
            samples.push((z / HEIGHT_SCALE).round().clamp(0.0, 65_535.0) as u16);
        }
    }
    samples
}

/// The mirror's own two lanes, for a mirror covering the box's footprint with
/// a margin. The same derivation `volume_drape.rs` makes, without the
/// checkerboard: nothing here identifies a pixel with a place, so the extent
/// only has to be wide enough that no surface point falls off it.
fn floor_lanes() -> ([f32; 4], [f32; 4]) {
    // Half the box's diagonal in degrees, doubled — comfortably past any
    // corner of a 920 km box, and this file never reads a specific texel.
    let span_deg = 12.0;
    let (lon_lo, lon_hi) = (SITE.1 - span_deg, SITE.1 + span_deg);
    let (merc_lo, merc_hi) = (mercator_y(SITE.0 - span_deg), mercator_y(SITE.0 + span_deg));
    let u_per_degree = 1.0 / (lon_hi - lon_lo);
    let v_per_mercator_y = -1.0 / (merc_hi - merc_lo);
    let site_merc = mercator_y(SITE.0);
    (
        [
            ((SITE.1 - lon_lo) * u_per_degree) as f32,
            ((merc_hi - site_merc) / (merc_hi - merc_lo)) as f32,
            u_per_degree as f32,
            v_per_mercator_y as f32,
        ],
        [
            SITE.0 as f32,
            -BOX_KM[0] / 2.0,
            -BOX_KM[1] / 2.0,
            if MIRROR_FORMAT.is_srgb() { 0.0 } else { 1.0 },
        ],
    )
}

/// The uniform every frame here is built from, under `light`.
/// Everything a criterion here varies about a frame, named rather than passed
/// as a row of bare booleans.
#[derive(Clone, Copy)]
struct Fixture {
    /// The mesh's own ceiling, or `None` for a frame with no ground pass.
    ground_max_z: Option<f32>,
    /// Whether the flat map lid is drawn. **`false` is what makes a volume
    /// reading actually ground-free**: with the lid on, it composites behind
    /// the accumulation and leaks into the mean — measured at up to 0.0048,
    /// which is 31% of [`MIN_SHIFT`], and a denominator that is not what its
    /// name says.
    lid: bool,
    /// Whether the march takes its central-difference gradient. **Off in most
    /// of this file for a reason and on in one criterion for a bigger one.**
    ///
    /// Off, `shading` is exactly 1 and `lit` is the whole of what the volume
    /// does with the light — the arm a frame takes on every quality rung below
    /// the top. But off in *every* criterion is how C2 shipped a build in
    /// which the march's directional response was untested and a storm could
    /// be lit from the antipode of the ground under it:
    /// [`both_surfaces_are_lit_from_the_same_side`] is the criterion that
    /// turns it on, `#[ignore]`d like everything in this file.
    gradient_shading: bool,
    /// The camera's vertical stretch, which the shading's normals are taken
    /// against. Defaults to the camera fixture's own, so a frame here is one
    /// the app could produce; [`the_mesh_is_lit_by_the_shape_it_is_drawn_as`]
    /// — `#[ignore]`d, like everything here — overrides it deliberately,
    /// because the lane is its subject.
    exaggeration: f32,
    /// Where the height field's footprint sits in the drawn box. The identity
    /// is a field built for the box being drawn; anything else is a field
    /// standing in over a sub-rectangle, which is the state B3 built the lane
    /// for and which every fixture in this plan had left untested.
    ground_box: [f32; 4],
}

impl Fixture {
    /// A frame with the mesh, the lid, and this camera's own stretch.
    fn ground(camera: OrbitFixture) -> Self {
        Self {
            ground_max_z: Some(RIDGE),
            lid: true,
            gradient_shading: false,
            exaggeration: camera.3,
            ground_box: squallar_volumetric::uniform::IDENTITY_GROUND_BOX,
        }
    }

    /// A frame carrying the accumulation and nothing else: no mesh, no lid.
    fn volume_only(camera: OrbitFixture) -> Self {
        Self {
            ground_max_z: None,
            lid: false,
            ..Self::ground(camera)
        }
    }

    /// A frame carrying the flat lid and nothing else.
    fn lid_only(camera: OrbitFixture) -> Self {
        Self {
            ground_max_z: None,
            ..Self::ground(camera)
        }
    }
}

fn uniform(
    cells: [u32; 3],
    view: &VolumeView,
    light: SurfaceLight,
    fixture: Fixture,
) -> VolumeUniform {
    let mut uniform = VolumeUniform::new(BOX_KM, cells);
    uniform.box_from_clip = view.box_from_clip;
    uniform.clip_from_box = view.clip_from_box;
    uniform.eye_in_box = view.eye_in_box;
    uniform.vertical_exaggeration = fixture.exaggeration;
    uniform.gradient_shading = fixture.gradient_shading;
    let (uv, geo) = floor_lanes();
    uniform.floor_uv = uv;
    uniform.floor_geo = geo;
    uniform.map_floor = fixture.lid;
    if let Some(max_z) = fixture.ground_max_z {
        // Clears `map_floor` on its own: the mesh IS the ground, so a frame
        // that has one has no lid.
        uniform.aim_occluder(max_z, HEIGHT_SCALE, 0.0);
        uniform.ground_box = fixture.ground_box;
    }
    uniform.set_light(light);
    uniform
}

/// A mirror painted with **the shipped light style's own land colours**, in
/// horizontal bands.
///
/// Not a grey card: the ramp multiplies the drape, and the question the plan
/// asks is what it does to the colours the style actually authored — which are
/// a near-white, almost unsaturated palette (`www/styles/light.json`:
/// background `#fafaf8`, landcover `rgba(234, 241, 233, 0.5)` over it, water
/// `#d4dadc`), plus the hillshade's own two greys, which
/// `squallar_egui::terrain` pins at 169 and 192.
const LAND_BANDS: [([u8; 3], &str); 5] = [
    ([250, 250, 248], "background #fafaf8"),
    ([242, 246, 241], "landcover over background"),
    ([212, 218, 220], "water #d4dadc"),
    ([192, 192, 192], "hillshade lit grey"),
    ([169, 169, 169], "hillshade dark grey"),
];

fn land_rgba() -> Vec<u8> {
    let mut rgba = Vec::with_capacity((MIRROR_EDGE * MIRROR_EDGE * 4) as usize);
    for row in 0..MIRROR_EDGE {
        let band =
            LAND_BANDS[(row * LAND_BANDS.len() as u32 / MIRROR_EDGE) as usize % LAND_BANDS.len()].0;
        for _ in 0..MIRROR_EDGE {
            rgba.extend_from_slice(&[band[0], band[1], band[2], 255]);
        }
    }
    rgba
}

/// One frame's two readable attachments.
struct Frame {
    ground: Vec<[u8; 4]>,
    offscreen: Vec<[u8; 4]>,
}

/// A grid that is entirely full at a mid palette index, so the volume reading
/// is a real accumulation rather than a handful of edge pixels.
const FULL: u8 = 200;

/// A grid of nothing but air, so the offscreen carries the map lid alone.
const AIR: u8 = 0;

/// Cells a side in every grid this file marches.
const CELLS: u32 = 8;

/// A grid whose index **rises steadily west to east**, so the march's
/// central-difference gradient points east at every cell and the outward
/// normal it derives faces west — the same way the ramp ground faces.
///
/// `shading` negates the gradient to get its normal, because the field climbs
/// *into* the data, so a density rising eastward is a surface facing west.
/// That is what lets one criterion require the ground and the volume to
/// brighten under the same sun.
fn westward_field() -> Vec<u8> {
    let mut indices = Vec::with_capacity((CELLS * CELLS * CELLS) as usize);
    for _k in 0..CELLS {
        for _j in 0..CELLS {
            for i in 0..CELLS {
                // Well clear of the empty-index threshold at either end, so
                // every cell is marched and none of them is air.
                indices.push(80 + (i * 20) as u8);
            }
        }
    }
    indices
}

fn render(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    pipelines: &VolumePipelines,
    uniform: &VolumeUniform,
    heights: Option<&GroundHeights>,
    mirror: &PaneMirror,
) -> Frame {
    render_grid(device, queue, pipelines, uniform, heights, mirror, FULL)
}

#[allow(clippy::too_many_arguments)]
fn render_grid(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    pipelines: &VolumePipelines,
    uniform: &VolumeUniform,
    heights: Option<&GroundHeights>,
    mirror: &PaneMirror,
    fill: u8,
) -> Frame {
    let n = (CELLS * CELLS * CELLS) as usize;
    render_field(
        device,
        queue,
        pipelines,
        uniform,
        heights,
        mirror,
        &vec![fill; n],
    )
}

#[allow(clippy::too_many_arguments)]
fn render_field(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    pipelines: &VolumePipelines,
    uniform: &VolumeUniform,
    heights: Option<&GroundHeights>,
    mirror: &PaneMirror,
    indices: &[u8],
) -> Frame {
    let cells = [CELLS; 3];
    let volume = pipelines
        .upload_volume(
            device,
            queue,
            cells,
            indices,
            &gpu_harness::grey_ramp_lut(),
            &mut VolumeStaging::new(device),
        )
        .expect("the grid and palette were refused");
    volume.write_uniform(queue, uniform);

    let target = pipelines.create_offscreen(
        device,
        OffscreenPlan {
            size: SIZE,
            rung: ResolutionRung::Native,
            ground: if heights.is_some() {
                GroundPass::On
            } else {
                GroundPass::Off
            },
        },
    );
    let mut encoder = device.create_command_encoder(&Default::default());
    if heights.is_some() {
        pipelines.encode_ground(&mut encoder, &target, &volume, Some(mirror), heights, None);
    }
    pipelines.encode_raymarch_with_floor(&mut encoder, &target, &volume, Some(mirror));
    queue.submit(Some(encoder.finish()));

    Frame {
        offscreen: read_back(device, queue, target.texture(), SIZE),
        ground: target
            .ground_texture()
            .map(|texture| read_back(device, queue, texture, SIZE))
            .unwrap_or_default(),
    }
}

/// The mean of a set of pixels' three colour channels, 0-1, over the pixels
/// `keep` accepts — and the count, because a mean over nothing is not a
/// reading and this file must never quote one.
fn mean_rgb(pixels: &[[u8; 4]], keep: impl Fn([u8; 4]) -> bool) -> ([f64; 3], usize) {
    let mut sum = [0.0f64; 3];
    let mut n = 0usize;
    for px in pixels {
        if keep(*px) {
            n += 1;
            for (slot, channel) in sum.iter_mut().zip(px) {
                *slot += f64::from(*channel);
            }
        }
    }
    if n == 0 {
        return ([0.0; 3], 0);
    }
    (sum.map(|s| s / n as f64 / 255.0), n)
}

/// The lit drape alone: the ground attachment over the pixels the mesh drew.
fn ground_reading(frame: &Frame) -> ([f64; 3], usize) {
    mean_rgb(&frame.ground, |px| px[3] == 255)
}

/// The accumulation alone: the offscreen of a frame with no ground pass.
fn volume_reading(frame: &Frame) -> ([f64; 3], usize) {
    mean_rgb(&frame.offscreen, |px| px[3] > 0)
}

/// **The lit map lid alone**: an air-only grid, no mesh, so the offscreen
/// carries the flat drape and nothing else.
///
/// The lid is where "what does the ramp do to the style's land colours" is
/// asked, and the reason is that a level surface's `ground_response` is
/// exactly one whatever the azimuth. The mesh's is not: on a real day the sun
/// swings west as it drops, so a mesh reading mixes the ramp's own colour with
/// which flank of the ridge the sun happens to be on — measured, at camera
/// `(35, 12, 1.0, 1.0)`, as a drape that came out *less* warm at 2 degrees
/// than at 10. That is a fact about a ridge, not about a ramp.
/// `None` when this camera does not see enough of the lid to average over,
/// which is a **geometric** fact and not a light one: the lid's coverage is
/// the mirror's own alpha times a fade off the eye's height, and neither reads
/// the light. Two of the eleven cameras sit within a whisker of the box floor
/// looking almost level, so their rays leave the box without ever crossing
/// `z = 0` inside it. [`LID_CAMERAS`] is what stops that skip quietly eating
/// the whole camera set.
fn lid_reading(
    scene: &Scene,
    camera: OrbitFixture,
    light: SurfaceLight,
) -> Option<([f64; 3], usize)> {
    let view = view_at(camera);
    let frame = render_grid(
        &scene.device,
        &scene.queue,
        &scene.pipelines,
        &uniform([8, 8, 8], &view, light, Fixture::lid_only(camera)),
        None,
        &scene.mirror,
        AIR,
    );
    let (reading, count) = mean_rgb(&frame.offscreen, |px| px[3] > 0);
    (count >= MIN_PIXELS).then_some((reading, count))
}

/// How many of [`ORBIT_CAMERAS`] see enough of the flat lid for the ramp
/// criterion to read it: **the six above the ground's crest**, measured.
///
/// The other five are the two between the crest and the box floor and the
/// three under it, and the lid genuinely is not in their frames — a flat plane
/// at `z = 0` is edge-on to a level eye and B1's `FLOOR_BELOW_FADE` dissolves
/// it outright from below. That is shipped behaviour rather than a limit of
/// this file, which is why this is the honest denominator for a criterion
/// about the lid and why [`night_is_dim_but_never_black_on_the_terrain`] takes
/// the whole eleven on the mesh, which every camera can see. (Both are
/// `#[ignore]`d, like everything in this file.)
///
/// A floor rather than a pin: it may only go up.
const LID_CAMERAS: usize = 6;

/// The largest per-channel difference between two readings.
fn shift(a: [f64; 3], b: [f64; 3]) -> f64 {
    a.iter()
        .zip(&b)
        .map(|(x, y)| (x - y).abs())
        .fold(0.0f64, f64::max)
}

/// How far a reading has to move for the light to count as having reached that
/// surface.
///
/// Four 8-bit levels. Far above the quantisation floor, and far below what the
/// sunset ramp actually does — the criterion's own printout carries the
/// measured shifts, so a reader can see how much headroom this has rather than
/// taking the number on trust.
const MIN_SHIFT: f64 = 4.0 / 255.0;

/// The fewest pixels a reading may be a mean over.
///
/// A mean over three pixels is not a reading, and a camera that framed the box
/// off-screen would otherwise report "unchanged" as a pass.
const MIN_PIXELS: usize = 200;

// ---------------------------------------------------------------------------
// The scene, assembled
// ---------------------------------------------------------------------------

struct Scene {
    device: wgpu::Device,
    queue: wgpu::Queue,
    pipelines: VolumePipelines,
    heights: GroundHeights,
    ramp: GroundHeights,
    mirror: PaneMirror,
}

fn scene(wgsl: &str) -> Scene {
    let (device, queue) = device();
    // **Without the prism pipeline**, because one caller hands in
    // [`PRE_C2_SHADER`] — a byte-identical copy of this shader as it stood
    // before C2, which has no `vs_building`, no `fs_building` and no `fn lit`
    // for one to call. Building it would fail pipeline creation and retire a
    // light pin because a building landed. Nothing here draws a prism, so the
    // pipeline this skips would never have been bound.
    let pipelines =
        VolumePipelines::from_shader_source_without_prisms(&device, attachments(SURFACE), wgsl);
    // **The march draws through a vertex buffer and the ground pass does not.**
    // Without this the ground attachment fills and the offscreen comes back
    // empty at every camera, which reads as "the light changed nothing".
    pipelines.upload_quad(&queue);
    let heights = pipelines
        .upload_heights(&device, &queue, [POSTS, POSTS], &samples_of(ridge_height))
        .expect("the ridge field was refused");
    let ramp = pipelines
        .upload_heights(&device, &queue, [POSTS, POSTS], &samples_of(ramp_height))
        .expect("the ramp field was refused");
    let mirror = planted_mirror(
        &device,
        &queue,
        &pipelines,
        [MIRROR_EDGE, MIRROR_EDGE],
        &land_rgba(),
    );
    Scene {
        device,
        queue,
        pipelines,
        heights,
        ramp,
        mirror,
    }
}

/// The offscreen is format-independent; the blit is not, and the blit is not
/// under test here.
const SURFACE: wgpu::TextureFormat = wgpu::TextureFormat::Bgra8Unorm;

/// Both readings at one camera under one light: `(ground, volume)`.
fn readings(scene: &Scene, camera: OrbitFixture, light: SurfaceLight) -> ([f64; 3], [f64; 3]) {
    let view = view_at(camera);
    let cells = [8u32, 8, 8];

    let with_ground = render(
        &scene.device,
        &scene.queue,
        &scene.pipelines,
        &uniform(cells, &view, light, Fixture::ground(camera)),
        Some(&scene.heights),
        &scene.mirror,
    );
    let (ground, ground_px) = ground_reading(&with_ground);
    assert!(
        ground_px >= MIN_PIXELS,
        "{camera:?}: the mesh drew {ground_px} pixels, which is not a scene to \
         average over. A criterion measured over nothing reads as 'unchanged' \
         and passes",
    );

    let without_ground = render(
        &scene.device,
        &scene.queue,
        &scene.pipelines,
        &uniform(cells, &view, light, Fixture::volume_only(camera)),
        None,
        &scene.mirror,
    );
    let (volume, volume_px) = volume_reading(&without_ground);
    assert!(
        volume_px >= MIN_PIXELS,
        "{camera:?}: the march painted {volume_px} pixels, which is not a scene \
         to average over",
    );

    (ground, volume)
}

/// How far one surface moved between two lights, in **both** the senses that
/// matter.
#[derive(Clone, Copy)]
struct Move {
    /// The largest per-channel move. Says the surface took *a* light.
    shift: f64,
    /// How far red-minus-blue moved. Says it took *this* light.
    warming: f64,
}

impl Move {
    /// Whether this surface really went from the studio lamp to a sunset.
    ///
    /// **Both halves, and the second one is not decoration.** A magnitude test
    /// alone passes on a build where `set_light` never writes the beam at all:
    /// the beam lane keeps `HEADLIGHT`'s white, the surface is then lit by a
    /// full-strength white sun under a sunset sky, and the sky term on its own
    /// clears a 4/255 magnitude floor easily. That is a wholly wrong picture —
    /// a permanent noon beam — and C2's first-pass flagship could not see it.
    /// The colour is what a sunset *is*, so the colour is what has to move.
    fn is_a_sunset(&self) -> bool {
        self.shift >= MIN_SHIFT && self.warming >= MIN_WARMING
    }
}

/// How far red-minus-blue must travel for a surface to have taken the *sun's
/// colour* rather than merely some light.
///
/// A twentieth, against a beam whose own red-minus-blue at 2 degrees is 0.74
/// of full scale before any albedo. Measured over the eleven cameras the
/// ground warms by 0.086 to 0.249 and the volume by 0.242, so the margin runs
/// from 1.7x at the tightest camera to about 5x — and a build whose beam never
/// reaches the GPU warms by the sky's own difference alone, which is under a
/// hundredth.
const MIN_WARMING: f64 = 0.05;

/// How far red sits above blue in a reading.
fn warmth(reading: [f64; 3]) -> f64 {
    reading[0] - reading[2]
}

/// Whether **both** surfaces went from the readable light to a sunset at this
/// camera, and by how much.
///
/// One function, so the criterion below and the controls that must fail are
/// measuring the same thing rather than two things written to agree.
fn both_moved(scene: &Scene, camera: OrbitFixture) -> (Move, Move) {
    let (ground_head, volume_head) = readings(scene, camera, HEADLIGHT);
    let (ground_sun, volume_sun) = readings(scene, camera, sunset());
    (
        Move {
            shift: shift(ground_head, ground_sun),
            warming: warmth(ground_sun) - warmth(ground_head),
        },
        Move {
            shift: shift(volume_head, volume_sun),
            warming: warmth(volume_sun) - warmth(volume_head),
        },
    )
}

// ---------------------------------------------------------------------------
// The criteria
// ---------------------------------------------------------------------------

/// **C2's done-when, first half: both surfaces change together.**
///
/// At every one of the eleven cameras, swapping the readable light for a
/// sunset must move the ground's colour *and* the volume's. One surface moving
/// is the two-composited-pictures failure, and it is what
/// [`the_criterion_rejects_a_build_where_only_one_surface_takes_the_tint`]
/// constructs — `#[ignore]`d beside this one, and run by the same invocation
/// at the top of the file.
#[test]
#[ignore = "needs a GPU"]
fn both_surfaces_change_together_under_the_sun() {
    let _lock = gpu_lock();
    let scene = scene(VOLUME_SHADER_WGSL);
    for camera in ORBIT_CAMERAS {
        let (ground, volume) = both_moved(&scene, camera);
        println!(
            "{camera:?}: ground moved {:.4} and warmed {:+.4}, volume moved \
             {:.4} and warmed {:+.4} (floors {MIN_SHIFT:.4} / {MIN_WARMING:.4})",
            ground.shift, ground.warming, volume.shift, volume.warming,
        );
        assert!(
            ground.is_a_sunset() && volume.is_a_sunset(),
            "{camera:?}: the sunset moved the ground by {:.4} and warmed it by \
             {:+.4}, and moved the volume by {:.4} and warmed it by {:+.4}. Both \
             have to do both. A scene in which only one surface takes the tint \
             is a warm ground under a neutral-white storm - two pictures \
             composited, not one scene - and a scene where both merely CHANGE \
             is one a permanently white beam would also draw",
            ground.shift,
            ground.warming,
            volume.shift,
            volume.warming,
        );
    }
}

/// **The non-triviality half.** Two builds, each with exactly the defect the
/// criterion above exists to catch, and each must fail it.
///
/// The mutations are the smallest ones that produce the defect: the surface
/// keeps its geometry, its drape and its accumulation, and loses only its
/// reach into the light. A criterion that stayed green through either of these
/// would be green through the whole point of C2.
#[test]
#[ignore = "needs a GPU"]
fn the_criterion_rejects_a_build_where_only_one_surface_takes_the_tint() {
    let _lock = gpu_lock();
    // (what is unlit, the pattern, its replacement, whose shift must vanish)
    let mutants: [(&str, &str, &str, bool); 2] = [
        (
            "the march bypasses `lit`, so the storm keeps the studio light \
             while the ground goes to sunset",
            "            colour = lit(colour, response);",
            "            colour = colour * response;",
            false,
        ),
        (
            "the ground fragment bypasses `lit`, so the terrain stays neutral \
             under a sunset-tinted storm",
            "    out.colour = vec4<f32>(\n        lit(ground.rgb, ground_response(normalize(in.normal))),\n        ground.a,\n    );",
            "    out.colour = ground;",
            true,
        ),
    ];

    for (name, pattern, replacement, ground_should_move) in mutants {
        assert_eq!(
            VOLUME_SHADER_WGSL.matches(pattern).count(),
            1,
            "{name}: the anchor is gone, so this control is not being applied \
             to anything - re-anchor it rather than deleting it. Pattern:\n{pattern}",
        );
        let mutated = VOLUME_SHADER_WGSL.replacen(pattern, replacement, 1);
        assert_ne!(
            mutated, VOLUME_SHADER_WGSL,
            "{name}: the substitution changed nothing",
        );
        let scene = scene(&mutated);
        let mut failed_somewhere = false;
        for camera in ORBIT_CAMERAS {
            let (ground, volume) = both_moved(&scene, camera);
            println!(
                "CONTROL {name}\n  {camera:?}: ground {:.4}/{:+.4}, volume {:.4}/{:+.4}",
                ground.shift, ground.warming, volume.shift, volume.warming,
            );
            // The surface that kept its light must still take a sunset, or the
            // control proves nothing but that the shader stopped working.
            let kept_its_light = if ground_should_move {
                volume.is_a_sunset()
            } else {
                ground.is_a_sunset()
            };
            assert!(
                kept_its_light,
                "{name}: {camera:?} broke BOTH surfaces, so this control is not \
                 the one-sided build it claims to be and its red says nothing",
            );
            failed_somewhere |= !(ground.is_a_sunset() && volume.is_a_sunset());
        }
        assert!(
            failed_somewhere,
            "{name}: the criterion stayed green against a build in which one \
             surface does not take the light at all. It cannot see the defect \
             it exists for",
        );
    }
}

/// **The readable light's arithmetic collapses to the identity.**
///
/// Byte-for-byte, at every camera, against the same shader with the light
/// multiplied out — `lit` reduced to `albedo * response` and the ground's
/// response to a flat one.
///
/// **What this can and cannot see, stated because an earlier draft of this
/// comment over-claimed.** Under `HEADLIGHT` the beam is one and the sky zero,
/// so `albedo * (1 * r + 0)` against `albedo * r` is an IEEE identity — which
/// means this criterion tests that `HEADLIGHT`'s constants really do reach the
/// GPU and that `ground_response` of a level normal is exactly one, and
/// **nothing else**. Its reference is manufactured out of the shader under
/// test, so it carries every other change that shader has. The claim that C2
/// left the shipped picture alone belongs to
/// [`the_headlight_draws_the_picture_the_shader_drew_before_c2`] — `#[ignore]`d
/// beside this one — which compares against a pre-C2 shader pinned out of git.
///
/// Rendered with **no height field**, the configuration every pane ships
/// today.
#[test]
#[ignore = "needs a GPU"]
fn the_headlight_light_collapses_to_the_identity() {
    let _lock = gpu_lock();
    let (before, mutated) = pre_c2_arithmetic();
    let now = scene(VOLUME_SHADER_WGSL);
    assert_ne!(mutated, VOLUME_SHADER_WGSL);

    for camera in ORBIT_CAMERAS {
        let (a, b) = meshless_pair(&before, &now, camera, HEADLIGHT);
        let differing = a.iter().zip(&b).filter(|(x, y)| x != y).count();
        assert_eq!(
            differing,
            0,
            "{camera:?}: {differing} of {} pixels differ between the shipped \
             picture and the arithmetic that drew it before C2, under the \
             readable light. The headlight is supposed to be the identity - \
             beam one, sky zero - so this is a picture that moved for users \
             who never asked for the sun",
            a.len(),
        );
    }
}

/// **The identity criterion's own non-triviality half.**
///
/// [`the_headlight_light_collapses_to_the_identity`] — `#[ignore]`d beside
/// this one — would be just as green against a renderer whose light did
/// nothing at all, in either mode. So the same two builds must *differ*, at
/// every camera, under the sun.
#[test]
#[ignore = "needs a GPU"]
fn the_identity_criterion_rejects_a_light_that_does_nothing() {
    let _lock = gpu_lock();
    let (before, _) = pre_c2_arithmetic();
    let now = scene(VOLUME_SHADER_WGSL);

    for camera in ORBIT_CAMERAS {
        let (a, b) = meshless_pair(&before, &now, camera, sunset());
        let differing = a.iter().zip(&b).filter(|(x, y)| x != y).count();
        println!(
            "{camera:?}: {differing} of {} pixels differ under a sunset",
            a.len()
        );
        assert!(
            differing > 0,
            "{camera:?}: the pre-C2 arithmetic draws the SUNSET frame \
             identically too, so the identity criterion is measuring a light \
             that reaches nothing",
        );
    }
}

/// **The shader as it stood the commit before C2**, byte-exact out of git:
///
/// ```text
/// git show 7f16b547:squallar-volumetric/src/volume.wgsl
/// ```
///
/// A historical artifact and **not a copy to keep in step**. It is here so
/// that "the readable light draws the picture this renderer always drew" is
/// measured against the picture this renderer actually drew, rather than
/// against a reference manufactured out of the shader under test — which is
/// what [`the_headlight_light_collapses_to_the_identity`] does, and which
/// cannot see any change the shader carries.
///
/// Both criteria that read it are `#[ignore]`d for a real adapter; run them
/// with `cargo test -p squallar-gpu --test volume_light -- --ignored`.
///
/// **When this goes red for a reason that is not about light**, because
/// someone legitimately changed what the march draws: that is the pin doing
/// its job, and the answer is to retire it deliberately with a measurement of
/// what moved, in a commit that says so. It is not to re-record it — a
/// re-recorded blob is the shader under test again, and this file would be
/// back to asserting `x * 1 + 0 == x`.
const PRE_C2_SHADER: &str = include_str!("volume_light/pre_c2_volume.wgsl");

/// **The readable light draws the picture the shader drew before C2**, at
/// every camera, byte for byte.
///
/// The real form of the claim: not "the light multiplies out" but "this is the
/// same picture", against a shader that predates every line of this unit and
/// carries none of its changes. Rendered in the configuration every pane ships
/// today — the flat map lid and no height field, because `heights` is `None`
/// at the frame build until the archive A2 would fetch from is published.
///
/// So this is what says a user who never asks for the sun sees exactly what
/// they saw before. `#[ignore]`d for a real adapter.
#[test]
#[ignore = "needs a GPU"]
fn the_headlight_draws_the_picture_the_shader_drew_before_c2() {
    let _lock = gpu_lock();
    assert!(
        !PRE_C2_SHADER.contains("fn lit(") && !PRE_C2_SHADER.contains("sun_beam"),
        "the pinned pre-C2 shader has C2 in it, so it is not the thing it \
         claims to be and this criterion compares a shader to itself",
    );
    let before = scene(PRE_C2_SHADER);
    let now = scene(VOLUME_SHADER_WGSL);

    for camera in ORBIT_CAMERAS {
        let (a, b) = meshless_pair(&before, &now, camera, HEADLIGHT);
        let differing = a.iter().zip(&b).filter(|(x, y)| x != y).count();
        assert_eq!(
            differing,
            0,
            "{camera:?}: {differing} of {} pixels differ between the picture \
             this shader drew the commit before C2 and the one it draws now, \
             under the readable light. That light is supposed to be the \
             identity - beam one, sky zero, a level lid's response exactly one \
             - so this is a picture that moved for users who never asked for \
             the sun",
            a.len(),
        );
    }
}

/// The pre-C2 pin's own non-triviality half: the two shaders **do** differ
/// under the sun, so the criterion above is not comparing two identical
/// strings by another name.
///
/// `#[ignore]`d for a real adapter.
#[test]
#[ignore = "needs a GPU"]
fn the_pre_c2_pin_can_tell_the_two_shaders_apart() {
    let _lock = gpu_lock();
    let before = scene(PRE_C2_SHADER);
    let now = scene(VOLUME_SHADER_WGSL);
    for camera in ORBIT_CAMERAS {
        let (a, b) = meshless_pair(&before, &now, camera, sunset());
        let differing = a.iter().zip(&b).filter(|(x, y)| x != y).count();
        println!(
            "{camera:?}: {differing} of {} pixels differ under a sunset",
            a.len()
        );
        assert!(
            differing > 0,
            "{camera:?}: the pre-C2 shader draws the SUNSET frame identically \
             too, so the pin is comparing something to itself",
        );
    }
}

/// The build that carries the arithmetic this renderer used before C2: the
/// light multiplied out of `lit`, and the ground's response flattened.
fn pre_c2_arithmetic() -> (Scene, String) {
    let mut wgsl = VOLUME_SHADER_WGSL.to_owned();
    for (pattern, replacement) in [
        (
            "    return albedo * (volume.sun_beam.xyz * response + volume.sky_ambient.xyz);",
            "    return albedo * response;",
        ),
        (
            "    return clamp(dot(normal, l) / level, 0.0, SLOPE_RESPONSE_CEILING);",
            "    return 1.0;",
        ),
    ] {
        assert_eq!(
            wgsl.matches(pattern).count(),
            1,
            "the pre-C2 arithmetic's anchor is gone - re-anchor it rather than \
             deleting it. Pattern:\n{pattern}",
        );
        wgsl = wgsl.replacen(pattern, replacement, 1);
    }
    (scene(&wgsl), wgsl)
}

/// The two builds' offscreens for one camera under one light, with **no ground
/// mesh** — the shipped configuration, where the only ground a pane draws is
/// the flat lid.
fn meshless_pair(
    before: &Scene,
    now: &Scene,
    camera: OrbitFixture,
    light: SurfaceLight,
) -> (Vec<[u8; 4]>, Vec<[u8; 4]>) {
    let view = view_at(camera);
    let cells = [8u32, 8, 8];
    let uniform = uniform(cells, &view, light, Fixture::lid_only(camera));
    let one = render(
        &before.device,
        &before.queue,
        &before.pipelines,
        &uniform,
        None,
        &before.mirror,
    );
    let two = render(
        &now.device,
        &now.queue,
        &now.pipelines,
        &uniform,
        None,
        &now.mirror,
    );
    let (painted, _) = mean_rgb(&two.offscreen, |px| px[3] > 0);
    assert!(
        painted.iter().any(|c| *c > 0.0),
        "{camera:?}: the frame is empty, so comparing it to another empty frame \
         proves nothing",
    );
    (one.offscreen, two.offscreen)
}

/// **What the ramp does to the style's own land colours**, measured on the
/// flat map lid rather than assumed to compose.
///
/// Two claims, and both are about the ramp rather than about a threshold:
///
/// * the drape's red-minus-blue **rises all the way down** — 60 degrees, then
///   10, then 2 — because the beam reddens as the air mass grows and there is
///   nowhere else for warmth in a near-white palette to come from;
/// * a **high sun leaves the style alone**, within five 8-bit levels, which is
///   what the white balance in `sun_over` buys and what an unbalanced daylight
///   does not.
///
/// The printout is as much the deliverable as the pass: "does this ramp
/// compose with a basemap authored under neutral light" is answered by the
/// numbers.
#[test]
#[ignore = "needs a GPU"]
fn the_ramp_reddens_the_basemap_as_the_sun_drops() {
    let _lock = gpu_lock();
    let scene = scene(VOLUME_SHADER_WGSL);
    let lights: Vec<(f32, &str, SurfaceLight)> = SUN_ELEVATIONS
        .iter()
        .map(|(elevation, what)| (*elevation, *what, sun_at(*elevation)))
        .collect();
    let mut measured = 0usize;
    for camera in ORBIT_CAMERAS {
        let Some(_) = lid_reading(&scene, camera, HEADLIGHT) else {
            println!("{camera:?}: no lid in frame, skipped");
            continue;
        };
        measured += 1;
        let mut warmth_by_elevation: Vec<(f32, f64)> = Vec::new();
        for (elevation, what, light) in &lights {
            let (ground, _) = lid_reading(&scene, camera, *light).expect(
                "the lid's coverage does not read the light, so a camera that \
                 saw it under the headlight must see it under every light",
            );
            let warmth = ground[0] - ground[2];
            println!(
                "{camera:?} {what} ({elevation} deg): drape mean \
                 [{:.3}, {:.3}, {:.3}], red-minus-blue {warmth:+.3}",
                ground[0], ground[1], ground[2],
            );
            warmth_by_elevation.push((*elevation, warmth));
        }
        // **The lower the sun, the warmer the map.** Stated as an ORDER over
        // the four above-horizon knots rather than as four thresholds: an
        // order has no tolerance to tune, and it is the property a reader of
        // the picture would name. Below the horizon there is no beam at all
        // and the scattered light is blue, so the two twilight knots are
        // asserted the other way.
        let warmth_at = |elevation: f32| {
            warmth_by_elevation
                .iter()
                .find(|(e, _)| *e == elevation)
                .expect("a knot that was rendered")
                .1
        };
        let (noon, afternoon, sunset_warmth) = (warmth_at(60.0), warmth_at(10.0), warmth_at(2.0));
        assert!(
            sunset_warmth > afternoon && afternoon > noon,
            "{camera:?}: red-minus-blue on the drape runs {noon:+.3} at 60 \
             degrees, {afternoon:+.3} at 10 and {sunset_warmth:+.3} at 2, and \
             it has to rise all the way down. The style's land colours are \
             near-white and nearly unsaturated, so every bit of warmth the \
             picture has comes from the beam",
        );
        // **And a high sun leaves the style alone.** Five 8-bit levels either
        // way, which is the whole point of the white balance in `sun_over`:
        // unbalanced, `colour + ambient` at the zenith is `[1.25, 1.29, 1.40]`
        // and this same figure was -0.098 — a visibly blue basemap at noon,
        // with the background and landcover both clipped to one white.
        //
        // Not zero, and the residue is real rather than slack: a high sun's
        // beam still reaches a slope turned away from it at a reduced
        // `ground_response`, and what fills in there is the sky, which is
        // blue. Averaged over a ridge it comes out a whisker cool.
        assert!(
            noon.abs() < 0.02,
            "{camera:?}: the drape is {noon:+.3} off neutral under a 60-degree \
             sun. The style's colours were authored under neutral light, and a \
             basemap that is visibly tinted at noon is the ramp failing to \
             compose with them rather than the sun being interesting",
        );
        for elevation in [-3.0f32, -20.0] {
            let cool = warmth_at(elevation);
            assert!(
                cool < 0.0,
                "{camera:?}: the drape is {cool:+.3} red-minus-blue at \
                 {elevation} degrees, where the beam is identically zero and \
                 the only light left is scattered. Twilight that is not blue \
                 means the sky term is not the one reaching the pixel",
            );
        }
    }
    assert!(
        measured >= LID_CAMERAS,
        "only {measured} of {} cameras had a lid to read, and the floor is \
         {LID_CAMERAS}. A criterion that skipped its way to a pass is not one",
        ORBIT_CAMERAS.len(),
    );
}

/// **Night is dim, and never black**, on the terrain, at every one of the
/// eleven cameras.
///
/// This is C1's own review finding rendered: the sky is applied with no cosine
/// on it precisely because `beam * max(0, N.L)` is identically zero everywhere
/// the sun is down, and a floor folded inside that cosine would make every
/// twilight and night colour in the ramp unreachable. A pane at 2 a.m. that
/// came back pure black would be that defect coming back through this
/// renderer.
///
/// Measured on the **mesh**, not the lid, and that is what lets it take the
/// whole camera set: the mesh covers the drawn box, so every camera that can
/// see the box can see it — including the five that have no lid in frame at
/// all.
#[test]
#[ignore = "needs a GPU"]
fn night_is_dim_but_never_black_on_the_terrain() {
    let _lock = gpu_lock();
    let scene = scene(VOLUME_SHADER_WGSL);
    let night = sun_at(-20.0);
    let twilight = sun_at(-3.0);
    for camera in ORBIT_CAMERAS {
        for (light, what) in [(night, "night"), (twilight, "civil twilight")] {
            let (ground, _) = readings(&scene, camera, light);
            println!(
                "{camera:?} {what}: terrain mean [{:.4}, {:.4}, {:.4}]",
                ground[0], ground[1], ground[2],
            );
            assert!(
                ground.iter().all(|c| *c > 0.0),
                "{camera:?}: the terrain is exactly black at {what}. The sky \
                 term carries no cosine precisely so that the night floor \
                 reaches a pixel and ground still reads by silhouette",
            );
            assert!(
                ground.iter().all(|c| *c < 0.35),
                "{camera:?}: the terrain reads {ground:?} at {what}, which is \
                 not a night",
            );
            assert!(
                ground[2] > ground[0],
                "{camera:?}: the terrain is not blue at {what}, where the beam \
                 is identically zero and every photon left is scattered",
            );
        }
    }
}

/// **Ground and volume are lit from the same side**, at every camera — the
/// criterion that makes a second light visible rather than merely unwritable.
///
/// C2 shipped for one commit with the march's `shading` normalising its own
/// copy of the direction lane. Negating that copy lit the raymarched storm
/// from the exact antipode of the ground it stands on, in one frame, and every
/// criterion in this file stayed green — because `gradient_shading` was
/// `false` in **every** fixture, so `shading` never executed at all and the
/// volume's whole directional response was untested by its own suite.
///
/// So this one turns it on. The scene is arranged so both surfaces face the
/// same way: ground that rises steadily west to east faces west, and a density
/// that rises west to east has a gradient pointing east, which `shading`
/// negates into a normal facing west. Light them from the west and from the
/// east; **both** have to be brighter under the western sun. A sign flip in
/// either response breaks the agreement, whichever one it is in.
///
/// `#[ignore]`d for a real adapter, like everything here.
#[test]
#[ignore = "needs a GPU"]
fn both_surfaces_are_lit_from_the_same_side() {
    let _lock = gpu_lock();
    let scene = scene(VOLUME_SHADER_WGSL);
    let (west, east) = grazing_pair(10.0);
    let field = westward_field();

    for camera in ORBIT_CAMERAS {
        let view = view_at(camera);
        let read = |light: SurfaceLight| -> (f64, f64) {
            let lit_ground = render_field(
                &scene.device,
                &scene.queue,
                &scene.pipelines,
                &uniform(
                    [CELLS; 3],
                    &view,
                    light,
                    Fixture {
                        ground_max_z: Some(RAMP_BASE + RAMP_RISE),
                        gradient_shading: true,
                        ..Fixture::ground(camera)
                    },
                ),
                Some(&scene.ramp),
                &scene.mirror,
                &field,
            );
            let lit_volume = render_field(
                &scene.device,
                &scene.queue,
                &scene.pipelines,
                &uniform(
                    [CELLS; 3],
                    &view,
                    light,
                    Fixture {
                        gradient_shading: true,
                        ..Fixture::volume_only(camera)
                    },
                ),
                None,
                &scene.mirror,
                &field,
            );
            let (ground, ground_px) = mean_rgb(&lit_ground.ground, |px| px[3] == 255);
            let (volume, volume_px) = mean_rgb(&lit_volume.offscreen, |px| px[3] > 0);
            assert!(
                ground_px >= MIN_PIXELS && volume_px >= MIN_PIXELS,
                "{camera:?}: {ground_px} ground and {volume_px} volume pixels, \
                 which is not a scene to average over",
            );
            (ground[1], volume[1])
        };

        let (ground_west, volume_west) = read(west);
        let (ground_east, volume_east) = read(east);
        println!(
            "{camera:?}: ground {ground_west:.4}/{ground_east:.4}, volume \
             {volume_west:.4}/{volume_east:.4} (west/east)",
        );
        assert!(
            ground_west > ground_east + MIN_SHIFT && volume_west > volume_east + MIN_SHIFT,
            "{camera:?}: a west-facing ground reads {ground_west:.4} under a \
             western sun and {ground_east:.4} under an eastern one, and a \
             west-facing volume reads {volume_west:.4} and {volume_east:.4}. \
             Both have to prefer the same sun. Two surfaces that disagree about \
             which way the light comes from is the two-composited-pictures \
             failure with the pictures lit from opposite sides, and every pixel \
             of it is plausible on its own",
        );
    }
}

/// **The flat map lid dims toward a grazing sun.**
///
/// `ground_response` is exactly one for the lid at any sun above
/// `LEVEL_COSINE_FLOOR`, which is what keeps the readable light's picture
/// unchanged — and it is why replacing the lid's whole response with a literal
/// `1.0` left all of C2's first-pass criteria green. The lid is the only
/// ground a pane draws today, so its response having no coverage at all was
/// the largest uncovered surface in the change.
///
/// The last five degrees are where the response is not one, and this measures
/// there. **Both lights carry the same beam and sky** and differ only in the
/// direction's elevation, so the beam ramp cannot be what moves the reading —
/// the response is the only thing left. Under a constant response the two are
/// identical.
///
/// `#[ignore]`d for a real adapter.
#[test]
#[ignore = "needs a GPU"]
fn the_flat_lid_dims_toward_a_grazing_sun() {
    let _lock = gpu_lock();
    let scene = scene(VOLUME_SHADER_WGSL);
    // One light's colours, two directions: 30 degrees up, and 2 degrees up.
    // `LEVEL_COSINE_FLOOR` is sin(5 degrees), so the lid's response is 1 at 30
    // and sin(2)/sin(5) = 0.40 at 2.
    let colours = sun_at(10.0);
    let aimed_at = |elevation_deg: f64| {
        let (sin_el, cos_el) = elevation_deg.to_radians().sin_cos();
        SurfaceLight {
            direction: [0.0, -cos_el as f32, sin_el as f32],
            ..colours
        }
    };
    let (high, grazing) = (aimed_at(30.0), aimed_at(2.0));

    let mut measured = 0usize;
    for camera in ORBIT_CAMERAS {
        let Some((bright, _)) = lid_reading(&scene, camera, high) else {
            println!("{camera:?}: no lid in frame, skipped");
            continue;
        };
        measured += 1;
        let (dim, _) = lid_reading(&scene, camera, grazing).expect(
            "the lid's coverage does not read the light, so a camera that saw \
             it under one direction must see it under the other",
        );
        println!(
            "{camera:?}: lid reads {:.4} at 30 degrees and {:.4} at 2, same beam",
            bright[1], dim[1],
        );
        assert!(
            bright[1] > dim[1] + MIN_SHIFT,
            "{camera:?}: the lid reads {:.4} under a 30-degree sun and {:.4} \
             under a 2-degree one carrying the SAME beam and sky, so its \
             response is not a function of where the light is. A flat lid that \
             never dims while the mesh beside it does is two grounds with two \
             responses",
            bright[1],
            dim[1],
        );
    }
    assert!(
        measured >= LID_CAMERAS,
        "only {measured} of {} cameras had a lid to read, and the floor is \
         {LID_CAMERAS}",
        ORBIT_CAMERAS.len(),
    );
}

/// **A field standing in over part of the box is lit at its own slope**, not
/// at the slope it would have if it covered the whole box.
///
/// `ground_normal` divides its rise by the run between two posts, and that run
/// is the **field's** footprint — `box_size_km.xy * ground_box.xy` — which is
/// the drawn box's only while the two are the same box. Dropping the
/// `ground_box` factor left `volume_light`, `volume_drape`, `volume_occluder`,
/// `volume_shader` and `volume_silhouette` all green, because every fixture in
/// this plan leaves the lane at the identity. It is the fourth time an
/// identity fixture has hidden a lane in this plan.
///
/// The state it hides is exactly the one B3 built the lane for: a field built
/// for an older box, standing in over a sub-rectangle so the pane draws
/// something rather than nothing while a newer field is in flight. There the
/// posts are half as far apart on the ground, so the terrain is **twice as
/// steep** — and with the factor gone it would be lit as the gentle thing it
/// is not, while being drawn as the steep thing it is.
///
/// `#[ignore]`d for a real adapter.
#[test]
#[ignore = "needs a GPU"]
fn a_field_standing_in_over_part_of_the_box_is_lit_at_its_own_slope() {
    let _lock = gpu_lock();
    let scene = scene(VOLUME_SHADER_WGSL);
    let (west, _) = grazing_pair(10.0);
    // The same ramp over half the box's width, centred. Same rise, half the
    // run, so twice the slope and a west-facing face turned twice as far
    // toward a western sun.
    let half = [0.5, 1.0, 0.25, 0.0];

    for camera in ORBIT_CAMERAS {
        let view = view_at(camera);
        let read = |ground_box: [f32; 4]| -> f64 {
            let frame = render(
                &scene.device,
                &scene.queue,
                &scene.pipelines,
                &uniform(
                    [CELLS; 3],
                    &view,
                    west,
                    Fixture {
                        ground_max_z: Some(RAMP_BASE + RAMP_RISE),
                        ground_box,
                        ..Fixture::ground(camera)
                    },
                ),
                Some(&scene.ramp),
                &scene.mirror,
            );
            let (reading, count) = mean_rgb(&frame.ground, |px| px[3] == 255);
            assert!(
                count >= MIN_PIXELS,
                "{camera:?}: the mesh drew {count} pixels over {ground_box:?}",
            );
            reading[1]
        };

        let whole = read(squallar_volumetric::uniform::IDENTITY_GROUND_BOX);
        let standing_in = read(half);
        println!(
            "{camera:?}: the ramp reads {whole:.4} over the whole box and \
             {standing_in:.4} over half of it",
        );
        assert!(
            standing_in > whole + MIN_SHIFT,
            "{camera:?}: the same field reads {whole:.4} over the whole box and \
             {standing_in:.4} over half of it, and half the run at the same \
             rise is twice the slope - it has to catch more of a western sun. \
             A normal that divided by the DRAWN box would answer the same both \
             times, and every fixture in this plan but this one leaves \
             `ground_box` at the identity",
        );
    }
}

/// A light's colours at `elevation_deg`, aimed from due west and from due
/// east: the same beam and sky, mirrored in azimuth, so only the direction
/// moves.
fn grazing_pair(elevation_deg: f64) -> (SurfaceLight, SurfaceLight) {
    let colours = sun_at(elevation_deg as f32);
    let (sin_el, cos_el) = elevation_deg.to_radians().sin_cos();
    (
        SurfaceLight {
            direction: [-cos_el as f32, 0.0, sin_el as f32],
            ..colours
        },
        SurfaceLight {
            direction: [cos_el as f32, 0.0, sin_el as f32],
            ..colours
        },
    )
}

/// **The mesh is lit by the shape it is DRAWN as**: the right flank is bright,
/// and a stretched box is lit as the steep thing it is stretched into.
///
/// Two of C2's own mutation survivors live here, and neither is a rounding —
/// each is a whole class of wrong picture that the criteria above could not
/// see, because every one of them asks "did this change" and neither of these
/// mutations stops anything changing.
///
/// * **`normalize(vec3(-slope.x, -slope.y, 1))` with the signs flipped.** The
///   normal then leans the wrong way and the lit and shaded flanks trade
///   places: the classic inverted hillshade, in which every valley reads as a
///   ridge. It survives any pairwise test over the fixture ridge, because a
///   symmetric ridge is symmetric under exactly the swap the flip performs.
///   That is why this criterion uses a ground that rises steadily west to east
///   — every post of it faces west, so the mean over the whole mesh has a
///   side, and a light from the west has to beat a light from the east.
/// * **`rise_km` without `box_size_km.w`.** The stretch the camera is shown
///   would then not reach the normals, and a box drawn three times as tall
///   would be shaded as the gentle thing it is not. Held here by varying the
///   lane against a fixed camera, which is precisely what the lane means: the
///   mesh's `z` is in box space and the exaggeration is baked into the camera,
///   so this lane is the *only* thing that tells the shading how steep the
///   picture is.
///
/// `#[ignore]`d for a real adapter, like everything in this file.
#[test]
#[ignore = "needs a GPU"]
fn the_mesh_is_lit_by_the_shape_it_is_drawn_as() {
    let _lock = gpu_lock();
    let scene = scene(VOLUME_SHADER_WGSL);
    // Ten degrees, from due west and due east: same height, mirrored azimuth,
    // so the beam and sky colours are identical and only the direction moves.
    let elevation = 10.0f64;
    let (sin_el, cos_el) = elevation.to_radians().sin_cos();
    let colours = sun_at(elevation as f32);
    let from_west = SurfaceLight {
        direction: [-cos_el as f32, 0.0, sin_el as f32],
        ..colours
    };
    let from_east = SurfaceLight {
        direction: [cos_el as f32, 0.0, sin_el as f32],
        ..colours
    };

    let read = |camera: OrbitFixture, light: SurfaceLight, exaggeration: f32| -> f64 {
        let view = view_at(camera);
        let frame = render(
            &scene.device,
            &scene.queue,
            &scene.pipelines,
            &uniform(
                [8, 8, 8],
                &view,
                light,
                Fixture {
                    ground_max_z: Some(RAMP_BASE + RAMP_RISE),
                    exaggeration,
                    ..Fixture::ground(camera)
                },
            ),
            Some(&scene.ramp),
            &scene.mirror,
        );
        let (reading, count) = mean_rgb(&frame.ground, |px| px[3] == 255);
        assert!(
            count >= MIN_PIXELS,
            "{camera:?}: the ramp drew {count} pixels, which is not a surface \
             to average over",
        );
        // The green channel: the two lights are the same colour, so any one
        // channel carries the whole comparison, and green is the one the eye
        // weighs most.
        reading[1]
    };

    for camera in ORBIT_CAMERAS {
        let west = read(camera, from_west, 1.0);
        let east = read(camera, from_east, 1.0);
        println!(
            "{camera:?}: west-facing ground reads {west:.4} lit from the west \
             and {east:.4} lit from the east",
        );
        assert!(
            west > east + MIN_SHIFT,
            "{camera:?}: ground that faces west reads {west:.4} under a western \
             sun and {east:.4} under an eastern one, and the western sun has to \
             win. A normal that leans the wrong way trades the lit flank for \
             the shaded one - every valley reads as a ridge, and the picture is \
             plausible everywhere",
        );

        let flat = read(camera, from_west, 1.0);
        let steep = read(camera, from_west, 3.0);
        println!("{camera:?}: exaggeration 1 reads {flat:.4}, 3 reads {steep:.4}");
        assert!(
            steep > flat + MIN_SHIFT,
            "{camera:?}: stretching the box from 1x to 3x moved the west-facing \
             ground's brightness from {flat:.4} to {steep:.4}, and a slope \
             turned three times as far toward the sun has to catch more of it. \
             The exaggeration is baked into the camera, so this lane is the \
             only thing that tells the shading how steep the picture it is \
             drawing is",
        );
    }
}

/// **The mesh has relief**: turning the light round moves the picture, at
/// every camera.
///
/// The two lights are the same sun height on the two sides of local noon, so
/// they differ only in azimuth: their `l.z` is the same, their beam and sky
/// colours are the same, and `ground_response` is therefore a function of the
/// **normal alone** across the pair. A shader whose response ignored the
/// normal would draw the two frames byte-identically, so "these two frames
/// differ" is exactly the claim "the mesh is shaded by its own shape" — with
/// no threshold standing in for it.
///
/// Measured **per pixel**, not as a mean: the fixture ridge is symmetric, so
/// averaging the whole mesh cancels its two flanks against each other and
/// reports no relief for a shader that has plenty.
///
/// This is the coverage `volume_occluder.rs` gave up when C2 put it under
/// `UNLIT`. Every criterion in that file reads the mesh's colour as a discrete
/// identity, so it cannot also be the file that watches the mesh get shaded.
/// Without this, `ground_response` could return a constant and nothing in the
/// repository would notice.
#[test]
#[ignore = "needs a GPU"]
fn a_slope_toward_a_low_sun_is_brighter_than_one_turned_away() {
    let _lock = gpu_lock();
    let scene = scene(VOLUME_SHADER_WGSL);
    // Ten degrees: high enough that the beam is still strong, low enough that
    // `ground_response`'s divisor is small and the ridge's gentle slopes — a
    // 7 km rise over a 920 km box — separate.
    let morning = sun_on_side(10.0, true);
    let evening = sun_on_side(10.0, false);

    for camera in ORBIT_CAMERAS {
        let view = view_at(camera);
        let cells = [8u32, 8, 8];
        let mut frames = Vec::new();
        for light in [morning, evening] {
            frames.push(render(
                &scene.device,
                &scene.queue,
                &scene.pipelines,
                &uniform(cells, &view, light, Fixture::ground(camera)),
                Some(&scene.heights),
                &scene.mirror,
            ));
        }
        let (a, b) = (&frames[0].ground, &frames[1].ground);
        let (mut drawn, mut moved, mut worst) = (0usize, 0usize, 0u8);
        for (one, two) in a.iter().zip(b) {
            if one[3] != 255 || two[3] != 255 {
                continue;
            }
            drawn += 1;
            let delta = (0..3)
                .map(|c| one[c].abs_diff(two[c]))
                .max()
                .unwrap_or_default();
            worst = worst.max(delta);
            // Two levels: past the 8-bit quantisation floor, so a pixel that
            // "moved" really did.
            if delta >= 2 {
                moved += 1;
            }
        }
        assert!(
            drawn >= MIN_PIXELS,
            "{camera:?}: the mesh drew {drawn} pixels under both lights, which \
             is not a surface to compare",
        );
        let fraction = moved as f64 / drawn as f64;
        println!(
            "{camera:?}: {moved} of {drawn} mesh pixels ({:.1}%) change when the \
             sun moves from morning to evening, worst {worst} levels",
            100.0 * fraction,
        );
        assert!(
            fraction >= 0.05,
            "{camera:?}: only {moved} of {drawn} pixels ({:.1}%) changed when \
             the sun crossed the sky at a fixed height. The two lights differ \
             ONLY in azimuth, so a response that ignored the mesh's normal \
             would draw them identically - this is a flat drape with a \
             silhouette, not terrain",
            100.0 * fraction,
        );
    }
}
