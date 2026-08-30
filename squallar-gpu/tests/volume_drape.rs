//! **Is the map on the terrain in the right place?**
//!
//! B3's central criterion. The ground mesh takes its colour from the pane
//! mirror, reprojected at the mesh's own surface point; if that reprojection
//! disagrees with the one the map floor uses — or with the one `build_voxels`
//! made the box with — the basemap slides across the terrain and nothing else
//! in the suite notices, because every pixel still carries a plausible colour.
//!
//! ```text
//! cargo test -p squallar-gpu --test volume_drape -- --ignored --nocapture
//! ```
//!
//! `#[ignore]`d (CI's `gpu` job opts in on lavapipe), and the tests hold the
//! shared process-wide GPU lock, like every other suite in this directory.
//!
//! # The oracle
//!
//! The mirror is painted as a **checkerboard on lines of latitude and
//! longitude** — black and white, a quarter of a degree a cell. For a surface
//! point of the mesh the host then does the forward projection the box was made
//! with, `squallar_geo::great_circle_destination`, works out which checker cell
//! that lands in, and requires the ground attachment under that point to carry
//! that cell's colour.
//!
//! Two things make it an oracle rather than a coincidence detector. The colours
//! are the extremes of the channel, so the answer is a **discrete** cell
//! identity and not a number with a tolerance. And the pixel is *identified*
//! through the occluder's own packed ray parameter: before any colour is
//! compared, the decoded `t` under that pixel must equal `|p - eye|` for the
//! surface point the host projected there. A pixel showing a different part of
//! the mesh is skipped rather than compared, so the criterion can never be
//! satisfied by reading the wrong pixel.
//!
//! # The non-triviality half
//!
//! A checkerboard test passes trivially if the two projections it is meant to
//! distinguish agree. So each sample also computes the **scale-and-translate
//! approximation the shader exists to reject** — latitude as `y_km` over
//! kilometres per degree, longitude as `x_km` over kilometres per degree at the
//! site's own latitude — and
//! [`the_approximation_the_shader_rejects_lands_in_the_wrong_cell`] requires it
//! to name a *different* cell at the box's corners. Without that, this file
//! would be green against a shader that had thrown the spherical solution away.
#![cfg(not(target_arch = "wasm32"))]

use egui_wgpu::wgpu;
use squallar_device_profile::quality::{GroundPass, ResolutionRung};
use squallar_egui::pane::OrbitCamera;
use squallar_egui::volume_view::{VolumeView, view_for};
use squallar_volumetric::raymarch::staging::VolumeStaging;
use squallar_volumetric::raymarch::{
    GroundHeights, OffscreenPlan, VolumePipelines, post_center_fraction,
};
use squallar_volumetric::uniform::VolumeUniform;

mod gpu_harness;
use gpu_harness::{
    MIRROR_FORMAT, ORBIT_CAMERAS, OrbitFixture, attachments, device, gpu_lock, mercator_y,
    opaque_white_lut, planted_mirror, read_back,
};

/// The offscreen every frame here is rendered at.
///
/// **Larger than the other suites' 256 x 160, and that is the oracle's own
/// resolution.** The host projects a surface point to a pixel and then reads
/// that pixel's colour, so the comparison carries the ground distance between
/// the point and the pixel's own centre. At 640 across, a 920 km box seen from
/// the default standoff is about 4.6 km a pixel, so half a pixel is under a
/// tenth of a checker cell — comfortably inside [`CENTRE_CLEARANCE`]. At 320 it
/// was a fifth of a cell and one sample of nineteen landed on a blend.
const SIZE: [u32; 2] = [640, 400];

/// How far from a checker boundary a sample must sit to be compared, as a
/// fraction of a cell.
///
/// It fences two blurs at once: the mirror's `Linear` filter, and the ground
/// distance between the host's surface point and the pixel centre it was
/// projected onto. **Computed from the TRUE position alone**, never from what
/// was observed — a skip rule that read the pixel would be selecting away the
/// failures it exists to find.
const CENTRE_CLEARANCE: f64 = 0.38;

/// The acceptance site the plan names for "it works": the Colorado front
/// range, where the relief is real and where the two projections separate
/// furthest.
const SITE: (f64, f64) = (39.0, -106.0);

/// The box the scene is built in: the shipped 920 km reflectivity reach,
/// square, 20 km tall.
///
/// **The size is load-bearing for the non-triviality half.** The separation
/// between the spherical solution and the scale-and-translate approximation
/// grows with the corner's distance from the site; at the shipped default it is
/// tens of kilometres, and at a 50 km box it would be metres and this file
/// would prove nothing.
const BOX_KM: [f32; 3] = [920.0, 920.0, 20.0];

/// The checkerboard's period, in degrees of latitude and longitude.
///
/// A quarter degree is about 28 km north-south. The two projections separate by
/// about 31 km at this box's corners — measured, not assumed, by
/// [`the_approximation_the_shader_rejects_lands_in_the_wrong_cell`] — so one
/// cell is the scale at which the disagreement is visible as an identity rather
/// than as a number. A whole degree would be too coarse to cross a boundary and
/// this file would pass against the approximation.
const CHECKER_DEG: f64 = 0.25;

/// Mirror texels a side.
const MIRROR_EDGE: u32 = 1024;

/// The height field's posts a side.
const POSTS: u32 = 256;

/// The ridge's crest, in box `z`.
///
/// **Not flat, deliberately.** A flat mesh sits on `z = 0`, which is exactly
/// where the map floor's own ray hit lands — so a shader that draped the mesh
/// at the ray's floor crossing instead of at the mesh's own surface point would
/// be indistinguishable from a correct one. With a crest at 0.35 of the box's
/// height the two are 7 km of ground apart at an oblique camera, which is a
/// quarter of a checker cell.
const RIDGE: f32 = 0.35;

/// Width of the ridge, as a fraction of the box's east-west extent.
const RIDGE_SIGMA: f32 = 0.18;

/// The fixture height field in box `z`, clamped into the unit cube for the
/// reason `VolumeUniform::t_scale_for` gives.
fn ridge_height(uv: [f32; 2]) -> f32 {
    let d = (uv[0] - 0.5) / RIDGE_SIGMA;
    (RIDGE * (-0.5 * d * d).exp()).clamp(0.0, 1.0)
}

/// The height encoding, chosen so box `z` is the raw sample over the `u16`
/// ceiling: this file is about *where* the drape lands, not about the
/// elevation encoding, which `the_height_affine_turns_a_raw_sample_into_the_
/// box_it_stands_in` covers.
const HEIGHT_SCALE: f32 = 1.0 / 65_535.0;

fn view_at((yaw, pitch, distance, exaggeration): OrbitFixture) -> VolumeView {
    let camera =
        OrbitCamera::restore(yaw, pitch, distance, [0.0; 3], exaggeration).expect("finite camera");
    view_for(camera, BOX_KM, SIZE[0] as f32 / SIZE[1] as f32).expect("a view")
}

/// A box point's kilometres east and north of the site, the two lines
/// `volume.wgsl`'s `box_x_km` and `box_y_km` are.
fn km_at(uv: [f32; 2]) -> (f64, f64) {
    let west = -f64::from(BOX_KM[0]) / 2.0;
    let south = -f64::from(BOX_KM[1]) / 2.0;
    (
        west + f64::from(uv[0]) * f64::from(BOX_KM[0]),
        south + f64::from(uv[1]) * f64::from(BOX_KM[1]),
    )
}

/// **The projection the box was made with**, and the one the shader solves:
/// the direct spherical problem from the site.
fn true_geo(uv: [f32; 2]) -> (f64, f64) {
    let (x_km, y_km) = km_at(uv);
    let range_km = x_km.hypot(y_km);
    let azimuth_deg = x_km.atan2(y_km).to_degrees();
    squallar_geo::great_circle_destination(SITE.0, SITE.1, azimuth_deg, range_km)
}

/// **The scale and translate the shader rejects**, spelled out so it can be
/// measured rather than argued about: latitude as kilometres north over
/// kilometres per degree, longitude the same with the site's own latitude's
/// convergence applied once.
fn approximate_geo(uv: [f32; 2]) -> (f64, f64) {
    let (x_km, y_km) = km_at(uv);
    let km_per_degree = f64::from(gpu_harness::DEGREE_BOX_KM);
    (
        SITE.0 + y_km / km_per_degree,
        SITE.1 + x_km / (km_per_degree * SITE.0.to_radians().cos()),
    )
}

/// Which checker cell a latitude and longitude falls in: `true` for the white
/// squares.
fn checker_is_white(lat: f64, lon: f64) -> bool {
    let cell = |v: f64| (v / CHECKER_DEG).floor() as i64;
    (cell(lat) + cell(lon)).rem_euclid(2) == 0
}

/// How far a latitude and longitude sit from the nearest checker boundary, as a
/// fraction of a cell — 0 on a line, 0.5 dead centre.
///
/// Samples near a boundary are skipped: the mirror is sampled with a `Linear`
/// filter, so a texel within a texel or two of an edge is a blend of two cells
/// and carries neither colour.
fn clearance(lat: f64, lon: f64) -> f64 {
    let of = |v: f64| {
        let f = (v / CHECKER_DEG).rem_euclid(1.0);
        f.min(1.0 - f)
    };
    of(lat).min(of(lon))
}

/// The mirror's geographic extent: the box's own footprint, with a margin, as
/// `(lon range, mercator-y range)`.
///
/// Walked round the box's rim rather than taken from its corners: the footprint
/// of a square box under this projection is not a rectangle in longitude, and
/// its widest point is on an edge rather than at a corner in the hemisphere
/// away from the pole.
fn mirror_extent() -> ((f64, f64), (f64, f64)) {
    let mut lon = (f64::MAX, f64::MIN);
    let mut merc = (f64::MAX, f64::MIN);
    for step in 0..=64u32 {
        let f = step as f32 / 64.0;
        for uv in [[f, 0.0], [f, 1.0], [0.0, f], [1.0, f]] {
            let (lat, lon_deg) = true_geo(uv);
            lon = (lon.0.min(lon_deg), lon.1.max(lon_deg));
            let y = mercator_y(lat);
            merc = (merc.0.min(y), merc.1.max(y));
        }
    }
    // A tenth of the span each way, so no surface point of the mesh can land
    // off the mirror — where `map_colour_at_km` answers "no ground here" and
    // this file would be measuring absence rather than registration.
    let pad = |(lo, hi): (f64, f64)| {
        let margin = (hi - lo) * 0.1;
        (lo - margin, hi + margin)
    };
    (pad(lon), pad(merc))
}

/// The uniform's two floor lanes for [`mirror_extent`].
fn floor_lanes() -> ([f32; 4], [f32; 4]) {
    let (lon, merc) = mirror_extent();
    let u_per_degree = 1.0 / (lon.1 - lon.0);
    // v grows downward through the mirror and Mercator y grows north.
    let v_per_mercator_y = -1.0 / (merc.1 - merc.0);
    let site_merc = mercator_y(SITE.0);
    (
        [
            ((SITE.1 - lon.0) * u_per_degree) as f32,
            ((merc.1 - site_merc) / (merc.1 - merc.0)) as f32,
            u_per_degree as f32,
            v_per_mercator_y as f32,
        ],
        [
            SITE.0 as f32,
            -BOX_KM[0] / 2.0,
            -BOX_KM[1] / 2.0,
            // `Rgba8Unorm` is not sRGB, so the mirror holds gamma-encoded
            // texels.
            if MIRROR_FORMAT.is_srgb() { 0.0 } else { 1.0 },
        ],
    )
}

/// The checkerboard, painted through the exact inverse of the lanes above so
/// that the mirror and the shader agree about which texel is which place.
fn checker_rgba() -> Vec<u8> {
    let (lon, merc) = mirror_extent();
    let mut rgba = Vec::with_capacity((MIRROR_EDGE * MIRROR_EDGE * 4) as usize);
    for row in 0..MIRROR_EDGE {
        let v = (f64::from(row) + 0.5) / f64::from(MIRROR_EDGE);
        // v runs from the north edge downward.
        let y = merc.1 - v * (merc.1 - merc.0);
        // The inverse of `ln(tan(pi/4 + phi/2))`.
        let lat = (2.0 * y.exp().atan() - std::f64::consts::FRAC_PI_2).to_degrees();
        for column in 0..MIRROR_EDGE {
            let u = (f64::from(column) + 0.5) / f64::from(MIRROR_EDGE);
            let lon_deg = lon.0 + u * (lon.1 - lon.0);
            let value = if checker_is_white(lat, lon_deg) {
                255
            } else {
                0
            };
            rgba.extend_from_slice(&[value, value, value, 255]);
        }
    }
    rgba
}

/// A uniform aimed at `view`, with the mirror's lanes and a ground pass.
fn uniform(cells: [u32; 3], view: &VolumeView) -> VolumeUniform {
    let mut uniform = VolumeUniform::new(BOX_KM, cells);
    uniform.box_from_clip = view.box_from_clip;
    uniform.clip_from_box = view.clip_from_box;
    uniform.eye_in_box = view.eye_in_box;
    uniform.ambient = 1.0;
    uniform.gradient_shading = false;
    let (uv, geo) = floor_lanes();
    uniform.floor_uv = uv;
    uniform.floor_geo = geo;
    uniform.aim_occluder(RIDGE, HEIGHT_SCALE, 0.0);
    uniform
}

/// The fixture field's samples, one `u16` a post.
fn ridge_samples() -> Vec<u16> {
    let mut samples = Vec::with_capacity((POSTS * POSTS) as usize);
    for j in 0..POSTS {
        for i in 0..POSTS {
            let z = ridge_height([
                post_center_fraction(i, POSTS),
                post_center_fraction(j, POSTS),
            ]);
            samples.push((z / HEIGHT_SCALE).round().clamp(0.0, 65_535.0) as u16);
        }
    }
    samples
}

/// One frame's readable attachments, plus the composited offscreen.
struct Frame {
    occluder: Vec<[u8; 4]>,
    ground: Vec<[u8; 4]>,
    /// The composited result, `gamma(C) * A` premultiplied. Read only by the
    /// placement criterion, which needs the picture the LID painted - and the
    /// lid never reaches the ground attachment at all.
    offscreen: Vec<[u8; 4]>,
}

fn render(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    pipelines: &VolumePipelines,
    uniform: &VolumeUniform,
    heights: &GroundHeights,
    mirror: &squallar_volumetric::raymarch::PaneMirror,
) -> Frame {
    render_at(device, queue, pipelines, uniform, Some(heights), mirror)
}

/// [`render`] with no height field at all: the pane draws the flat map lid,
/// which is the picture every pane ships today and the denominator the
/// placement criterion measures its coverage against.
fn render_without_heights(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    pipelines: &VolumePipelines,
    uniform: &VolumeUniform,
    mirror: &squallar_volumetric::raymarch::PaneMirror,
) -> Frame {
    render_at(device, queue, pipelines, uniform, None, mirror)
}

fn render_at(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    pipelines: &VolumePipelines,
    uniform: &VolumeUniform,
    heights: Option<&GroundHeights>,
    mirror: &squallar_volumetric::raymarch::PaneMirror,
) -> Frame {
    // One cell of air: the volume is not what this file is about, and an empty
    // grid keeps the ground attachment the only thing carrying colour.
    let cells = [8u32, 8, 8];
    let indices = vec![0u8; 8 * 8 * 8];
    let volume = pipelines
        .upload_volume(
            device,
            queue,
            cells,
            &indices,
            &opaque_white_lut(),
            &mut VolumeStaging::new(device),
        )
        .expect("the grid and palette were refused");
    volume.write_uniform(queue, uniform);

    let target = pipelines.create_offscreen(
        device,
        OffscreenPlan {
            size: SIZE,
            rung: ResolutionRung::Native,
            ground: GroundPass::On,
        },
    );
    let mut encoder = device.create_command_encoder(&Default::default());
    pipelines.encode_ground(&mut encoder, &target, &volume, Some(mirror), heights);
    pipelines.encode_raymarch_with_floor(&mut encoder, &target, &volume, Some(mirror));
    queue.submit(Some(encoder.finish()));

    Frame {
        offscreen: read_back(device, queue, target.texture(), SIZE),
        occluder: read_back(
            device,
            queue,
            target.occluder_texture().expect("an occluder attachment"),
            SIZE,
        ),
        ground: read_back(
            device,
            queue,
            target.ground_texture().expect("a ground attachment"),
            SIZE,
        ),
    }
}

/// `volume.wgsl`'s `unproject`: a clip-space point back into box space.
fn unproject(m: [[f32; 4]; 4], ndc: [f32; 2], depth: f32) -> [f32; 3] {
    let v = [ndc[0], ndc[1], depth, 1.0];
    let mut out = [0.0f32; 4];
    for (row, slot) in out.iter_mut().enumerate() {
        *slot = (0..4).map(|column| m[column][row] * v[column]).sum();
    }
    [out[0] / out[3], out[1] / out[3], out[2] / out[3]]
}

/// The unit ray through a pixel's own centre — the position the rasteriser
/// hands `@builtin(position)`.
fn ray_through(view: &VolumeView, column: u32, row: u32) -> [f32; 3] {
    let ndc = [
        (column as f32 + 0.5) / SIZE[0] as f32 * 2.0 - 1.0,
        1.0 - (row as f32 + 0.5) / SIZE[1] as f32 * 2.0,
    ];
    let far = unproject(view.box_from_clip, ndc, 1.0);
    let d = [
        far[0] - view.eye_in_box[0],
        far[1] - view.eye_in_box[1],
        far[2] - view.eye_in_box[2],
    ];
    let length = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
    [d[0] / length, d[1] / length, d[2] / length]
}

/// What one camera's comparison found: `(lat, lon, expected white, observed)`
/// for every point that read the wrong cell, beside the two counts that say
/// whether the run measured anything.
struct Comparison {
    compared: usize,
    wrong: Vec<(f64, f64, bool, u8)>,
    off_surface: usize,
}

/// Compare every third pixel the mesh drew against the checker cell the
/// forward projection names, and answer `(compared, wrong, off surface)`.
///
/// Factored out so the mutant below is measured by the *same* comparison the
/// criterion above is, rather than by a second one written to agree with it.
fn compare(view: &VolumeView, t_scale: f32, frame: &Frame) -> Comparison {
    compare_where(view, t_scale, frame, ridge_height, |_| true)
}

/// [`compare`] over a placed field, and over a chosen part of the box.
///
/// `surface` is the analytic surface the mesh is meant to describe — the
/// identity field's own for the criterion above, the placed one with its apron
/// for the patch-edge criterion. `select` is the region of the **drawn box**
/// the caller is asking about, so "the apron drapes correctly" can be asserted
/// without the interior's points diluting it.
///
/// Factored this way rather than copied: the whole value of the checkerboard
/// oracle is that one comparison is what every criterion in this file goes
/// through, and a second spelling of it is a second thing that can be wrong in
/// a way that agrees.
fn compare_where(
    view: &VolumeView,
    t_scale: f32,
    frame: &Frame,
    surface: impl Fn([f32; 2]) -> f32,
    select: impl Fn([f32; 2]) -> bool,
) -> Comparison {
    let mut compared = 0usize;
    let mut wrong: Vec<(f64, f64, bool, u8)> = Vec::new();
    let mut off_surface = 0usize;
    // **The surface point is reconstructed from the PIXEL, not projected
    // to it.** An earlier version of this file picked a point on the mesh,
    // projected it to a pixel and compared there; at the grazing camera the
    // half pixel between the point and that pixel's own centre is nine
    // kilometres of ground, and seven samples of 114 read a neighbouring
    // checker cell — a failure of the oracle, not of the shader. The
    // fragment's `box_p` lies on the ray through its own pixel centre and
    // `fs_ground` writes `t = length(box_p - eye)`, so the eye, that ray
    // and the decoded `t` put the point back exactly where the shader had
    // it.
    for row in (0..SIZE[1]).step_by(3) {
        for column in (0..SIZE[0]).step_by(3) {
            let at = (row * SIZE[0] + column) as usize;
            if frame.ground[at][3] != 255 {
                continue;
            }
            let t = squallar_volumetric::raymarch::unpack24_bytes([
                frame.occluder[at][0],
                frame.occluder[at][1],
                frame.occluder[at][2],
            ]) * t_scale;
            let direction = ray_through(view, column, row);
            let p = [
                view.eye_in_box[0] + direction[0] * t,
                view.eye_in_box[1] + direction[1] * t,
                view.eye_in_box[2] + direction[2] * t,
            ];
            // **The reconstruction really is a point of the mesh**, which
            // is what keeps the identification honest rather than
            // circular: the occluder is a channel the shader wrote, and if
            // it did not describe the analytic surface this file drew, the
            // colour comparison below would be about nowhere.
            if (p[2] - surface([p[0], p[1]])).abs() > 2e-3 {
                off_surface += 1;
                continue;
            }
            if !select([p[0], p[1]]) {
                continue;
            }
            let (lat, lon) = true_geo([p[0], p[1]]);
            // Away from a checker boundary, where a `Linear` filter blends
            // two cells and neither colour is the answer.
            if clearance(lat, lon) < CENTRE_CLEARANCE {
                continue;
            }

            compared += 1;
            let white = checker_is_white(lat, lon);
            let observed = frame.ground[at][0];
            let matches = if white { observed > 247 } else { observed < 8 };
            if !matches {
                wrong.push((lat, lon, white, observed));
            }
        }
    }
    Comparison {
        compared,
        wrong,
        off_surface,
    }
}

// ---------------------------------------------------------------------------
// The oracle.
// ---------------------------------------------------------------------------

/// **The map lands on the terrain where the forward projection says it does.**
#[test]
#[ignore = "needs a real wgpu adapter; see the doc comment for the invocation"]
fn the_drape_lands_in_the_cell_the_forward_projection_names() {
    let _held = gpu_lock();
    let (device, queue) = device();
    let pipelines = VolumePipelines::new(&device, attachments(wgpu::TextureFormat::Rgba8Unorm));
    pipelines.upload_quad(&queue);
    let mirror = planted_mirror(
        &device,
        &queue,
        &pipelines,
        [MIRROR_EDGE, MIRROR_EDGE],
        &checker_rgba(),
    );
    let heights = pipelines
        .upload_heights(&device, &queue, [POSTS, POSTS], &ridge_samples())
        .expect("the fixture height field was refused");

    let mut compared_total = 0usize;
    let mut off_surface_total = 0usize;
    for camera in ORBIT_CAMERAS {
        let view = view_at(camera);
        let uniform = uniform([8, 8, 8], &view);
        let t_scale = uniform.occluder_t_scale;
        let frame = render(&device, &queue, &pipelines, &uniform, &heights, &mirror);

        let Comparison {
            compared,
            wrong,
            off_surface,
        } = compare(&view, t_scale, &frame);
        off_surface_total += off_surface;
        assert!(
            compared >= 30,
            "{camera:?}: only {compared} surface points could be identified on \
             the offscreen, so this criterion is asserting about almost \
             nothing. Either the camera frames the box out of view or the \
             occluder no longer decodes to the ray parameter the host solves",
        );
        assert!(
            wrong.is_empty(),
            "{camera:?}: {} of {compared} surface points carry the wrong \
             checker cell (first: {:?}). The drape is registered to something \
             other than the mesh's own surface point — a basemap sliding \
             across the terrain, which every other criterion in this directory \
             passes through",
            wrong.len(),
            &wrong[..wrong.len().min(4)],
        );
        compared_total += compared;
    }
    assert!(
        compared_total >= 3000,
        "only {compared_total} points were compared across all eleven cameras",
    );
    // A few rim pixels genuinely reconstruct off the analytic surface — the
    // rasteriser's coverage rule reaches half a pixel past the mesh's own edge
    // — but a build where that were the common case would be one where the
    // occluder had stopped describing the geometry, and the criterion above
    // would be quietly measuring a handful of pixels instead of thousands.
    assert!(
        off_surface_total * 5 < compared_total,
        "{off_surface_total} reconstructed points of {compared_total} compared \
         do not lie on the mesh at all",
    );
}

/// **The oracle's own non-triviality half, on the GPU: the same comparison
/// against a shader that HAS the approximation, which must fail it.**
///
/// The arithmetic half below proves the two projections name different checker
/// cells. This proves the criterion above can *see* that difference — a
/// separate claim, and the one that would be missing if the oracle were reading
/// the wrong pixel, comparing a blend, or skipping everything.
///
/// The mutant is exactly the projection `volume.wgsl`'s own comment says it
/// rejects: longitude as kilometres east over kilometres per degree at the
/// site's latitude, latitude as kilometres north over kilometres per degree.
///
/// Measured on this hardware: noticed at **10 of the 11 cameras, on 1242 of
/// 10683 compared points**. The thresholds below are four cameras and a hundred
/// points, which is margin rather than a tuned figure — the one camera that
/// does not notice is the near-overhead one, which frames the middle of the box
/// where the two projections genuinely agree, and it is *right* not to notice.
#[test]
#[ignore = "needs a real wgpu adapter; see the doc comment for the invocation"]
fn the_oracle_notices_the_approximation_in_the_shader() {
    let _held = gpu_lock();
    let (device, queue) = device();
    let attachments = attachments(wgpu::TextureFormat::Rgba8Unorm);

    // The spherical solution's two lines, and the scale-and-translate that
    // replaces them.
    const TRUE_LON: &str =
        "    let d_lon_deg = degrees(atan2(sin_az * sd * cos_phi0, cd - sin_phi0 * sin_lat));";
    const TRUE_MERC: &str =
        "    let d_merc = mercator_y_from_sin(sin_lat) - mercator_y(site_lat_rad);";
    const APPROX_LON: &str = "    let d_lon_deg = x_km / (KM_PER_DEGREE_LAT * cos_phi0);";
    const APPROX_MERC: &str = "    let d_merc = mercator_y(site_lat_rad + radians(y_km / KM_PER_DEGREE_LAT)) - mercator_y(site_lat_rad);";

    let source = squallar_volumetric::raymarch::VOLUME_SHADER_WGSL;
    for anchor in [TRUE_LON, TRUE_MERC] {
        assert_eq!(
            source.matches(anchor).count(),
            1,
            "the reprojection has moved; re-anchor this mutant rather than \
             deleting it. Anchor was:\n{anchor}",
        );
    }
    let mutated = source
        .replacen(TRUE_LON, APPROX_LON, 1)
        .replacen(TRUE_MERC, APPROX_MERC, 1);

    let pipelines = VolumePipelines::from_shader_source(&device, attachments, &mutated);
    pipelines.upload_quad(&queue);
    let mirror = planted_mirror(
        &device,
        &queue,
        &pipelines,
        [MIRROR_EDGE, MIRROR_EDGE],
        &checker_rgba(),
    );
    let heights = pipelines
        .upload_heights(&device, &queue, [POSTS, POSTS], &ridge_samples())
        .expect("the fixture height field was refused");

    let mut compared_total = 0usize;
    let mut wrong_total = 0usize;
    let mut cameras_that_noticed = 0usize;
    for camera in ORBIT_CAMERAS {
        let view = view_at(camera);
        let uniform = uniform([8, 8, 8], &view);
        let frame = render(&device, &queue, &pipelines, &uniform, &heights, &mirror);
        let seen = compare(&view, uniform.occluder_t_scale, &frame);
        compared_total += seen.compared;
        wrong_total += seen.wrong.len();
        if !seen.wrong.is_empty() {
            cameras_that_noticed += 1;
        }
    }

    assert!(
        compared_total >= 3000,
        "only {compared_total} points were compared, so this is not the same \
         measurement the criterion above makes",
    );
    // Not "every camera": one that frames only the middle of the box sees the
    // region where the two projections nearly agree, and it *should* pass.
    // What must not happen is the mutant surviving the whole set.
    assert!(
        cameras_that_noticed >= 4 && wrong_total >= 100,
        "the scale-and-translate approximation was noticed at only \
         {cameras_that_noticed} of eleven cameras, on {wrong_total} of \
         {compared_total} compared points. The oracle above is then green \
         against a shader that threw the spherical solution away, which is the \
         one thing it exists to refuse",
    );
}

/// **The non-triviality half: the approximation the shader rejects names a
/// different cell.**
///
/// Needs no GPU — it is arithmetic over the two projections — so it runs on the
/// default row and reddens on a machine with no Vulkan loader. That is
/// deliberate: the oracle above is `#[ignore]`d, and a non-triviality half that
/// only runs where the thing it qualifies runs is half a gate.
#[test]
fn the_approximation_the_shader_rejects_lands_in_the_wrong_cell() {
    let mut disagreements = 0usize;
    let mut worst_km = 0.0f64;
    // The corners, where the two separate furthest, plus the edge midpoints.
    for uv in [
        [0.0f32, 0.0],
        [1.0, 0.0],
        [0.0, 1.0],
        [1.0, 1.0],
        [0.5, 0.0],
        [0.0, 0.5],
        [1.0, 0.5],
        [0.5, 1.0],
    ] {
        let (true_lat, true_lon) = true_geo(uv);
        let (approx_lat, approx_lon) = approximate_geo(uv);
        let km_per_degree = f64::from(gpu_harness::DEGREE_BOX_KM);
        let north_km = (true_lat - approx_lat) * km_per_degree;
        let east_km = (true_lon - approx_lon) * km_per_degree * true_lat.to_radians().cos();
        worst_km = worst_km.max(north_km.hypot(east_km));
        if checker_is_white(true_lat, true_lon) != checker_is_white(approx_lat, approx_lon) {
            disagreements += 1;
        }
    }

    assert!(
        worst_km > 20.0,
        "the two projections separate by at most {worst_km:.1} km over this \
         box, which is under a checker cell. The oracle above would then pass \
         against a shader that had thrown the spherical solution away, and the \
         separation is what makes it an oracle rather than a coincidence \
         detector",
    );
    assert!(
        disagreements >= 4,
        "the scale-and-translate approximation names the same checker cell as \
         the forward projection at all but {disagreements} of the box's eight \
         rim points, so `the_drape_lands_in_the_cell_the_forward_projection_\
         names` is not distinguishing the two. Separation was {worst_km:.1} km \
         against a {CHECKER_DEG}-degree cell",
    );
}

/// The two projections really do agree near the site, which is what says the
/// disagreement above is the *projection* separating rather than the
/// approximation being nonsense.
#[test]
fn the_two_projections_agree_at_the_site_itself() {
    let (true_lat, true_lon) = true_geo([0.5, 0.5]);
    let (approx_lat, approx_lon) = approximate_geo([0.5, 0.5]);
    assert!(
        (true_lat - SITE.0).abs() < 1e-9 && (true_lon - SITE.1).abs() < 1e-9,
        "the box's centre is not the site: {true_lat}, {true_lon}",
    );
    assert!(
        (true_lat - approx_lat).abs() < 1e-9 && (true_lon - approx_lon).abs() < 1e-9,
        "the approximation is not even right at the origin, so it is a bug \
         rather than the projection this file means to reject",
    );
}

// ---------------------------------------------------------------------------
// The placement lane, on the GPU.
// ---------------------------------------------------------------------------

/// A field over the middle quarter of the drawn box: what state two looks like
/// after a pane has zoomed out by two while a newer field is in flight.
///
/// `(scale_x, scale_y, offset_x, offset_y)`, the lane's own order.
const PLACED: [f32; 4] = [0.5, 0.5, 0.25, 0.25];

/// The surface the placed mesh describes, in box `z`, at a box-space point.
///
/// Inside the footprint it is the field interpolated; outside it is the apron —
/// the rim post's own height, held flat. Expressed as a **clamp of the field's
/// own coordinate** to the outermost post centres, which is what a duplicated
/// rim post is.
fn placed_height(p: [f32; 2]) -> f32 {
    let lo = post_center_fraction(0, POSTS);
    let hi = post_center_fraction(POSTS - 1, POSTS);
    let axis = |v: f32, scale: f32, offset: f32| ((v - offset) / scale).clamp(lo, hi);
    ridge_height([
        axis(p[0], PLACED[0], PLACED[2]),
        axis(p[1], PLACED[1], PLACED[3]),
    ])
}

/// How far every reconstructed surface point of `frame` sits from `expected`,
/// and how many were checked.
fn surface_error(
    view: &VolumeView,
    t_scale: f32,
    frame: &Frame,
    expected: impl Fn([f32; 2]) -> f32,
) -> (f32, usize) {
    let mut worst = 0.0f32;
    let mut checked = 0usize;
    for row in (0..SIZE[1]).step_by(3) {
        for column in (0..SIZE[0]).step_by(3) {
            let at = (row * SIZE[0] + column) as usize;
            if frame.ground[at][3] != 255 {
                continue;
            }
            let t = squallar_volumetric::raymarch::unpack24_bytes([
                frame.occluder[at][0],
                frame.occluder[at][1],
                frame.occluder[at][2],
            ]) * t_scale;
            let direction = ray_through(view, column, row);
            let p = [
                view.eye_in_box[0] + direction[0] * t,
                view.eye_in_box[1] + direction[1] * t,
                view.eye_in_box[2] + direction[2] * t,
            ];
            checked += 1;
            worst = worst.max((p[2] - expected([p[0], p[1]])).abs());
        }
    }
    (worst, checked)
}

/// Where a ray first enters the box's own xy column, and the height the mesh
/// has there.
///
/// A downward ray that enters the column ALREADY below the terrain sheet can
/// never meet it - the sheet has no thickness and the box's side wall is open -
/// so it reaches `z = 0` inside the unit square having drawn no ground. That is
/// the sliver the flat lid used to fill, and it is a property of the geometry
/// rather than of the placement: it is the same size with the identity
/// placement, measured.
fn enters_below_the_sheet(
    view: &VolumeView,
    column: u32,
    row: u32,
    surface: impl Fn([f32; 2]) -> f32,
) -> bool {
    let eye = view.eye_in_box;
    let d = ray_through(view, column, row);
    // Entry into the infinite column x, y in [0, 1]: the far end of each axis's
    // near intersection.
    let mut t_enter = 0.0f32;
    for axis in 0..2 {
        let (o, dir) = (eye[axis], d[axis]);
        if dir.abs() < 1e-6 {
            continue;
        }
        let (a, b) = ((0.0 - o) / dir, (1.0 - o) / dir);
        t_enter = t_enter.max(a.min(b));
    }
    let at = [
        eye[0] + d[0] * t_enter,
        eye[1] + d[1] * t_enter,
        eye[2] + d[2] * t_enter,
    ];
    at[2] < surface([at[0], at[1]])
}

/// **The mesh stands where the placement lane puts it, and covers the whole
/// drawn box while doing it.**
///
/// This is the one GPU frame with a non-identity `ground_box`, and it closes
/// two holes at once.
///
/// **It is the lane's only behavioural gate.** Every other fixture in this
/// directory goes through `aim_occluder` and leaves `IDENTITY_GROUND_BOX`,
/// where the affine is `1 x uv + 0` — so replacing the shader's placed position
/// with the bare grid coordinate, *precisely the wrong picture
/// `GroundPlacement`'s doc says it exists to prevent*, passed
/// `cargo test --workspace` and every suite here. Checker and checked both
/// lived on the identity, which is B1's third vacuity hole in a new place. The
/// mutant control below is what makes that impossible again.
///
/// **And it is F1's own regression control.** A field over a sub-rectangle used
/// to leave the footprint outside it with no mesh, while the lid stayed
/// suppressed frame-uniformly — volume over nothing, measured at 8 of 11
/// cameras. The apron ring is what covers it, and the second half of this test
/// requires the placed frame to paint ground everywhere a no-field frame paints
/// its lid.
#[test]
#[ignore = "needs a real wgpu adapter; see the doc comment for the invocation"]
fn a_placed_field_stands_where_the_lane_puts_it_and_still_covers_the_box() {
    let _held = gpu_lock();
    let (device, queue) = device();
    let attachments = attachments(wgpu::TextureFormat::Rgba8Unorm);
    let pipelines = VolumePipelines::new(&device, attachments);
    pipelines.upload_quad(&queue);
    let mirror = planted_mirror(
        &device,
        &queue,
        &pipelines,
        [MIRROR_EDGE, MIRROR_EDGE],
        &checker_rgba(),
    );
    let heights = pipelines
        .upload_heights(&device, &queue, [POSTS, POSTS], &ridge_samples())
        .expect("the fixture height field was refused");

    // The same scene with no field at all, so "the lid used to be here" has a
    // measured denominator rather than an argued one.
    let bare = |view: &VolumeView| {
        let mut uniform = VolumeUniform::new(BOX_KM, [8, 8, 8]);
        uniform.box_from_clip = view.box_from_clip;
        uniform.clip_from_box = view.clip_from_box;
        uniform.eye_in_box = view.eye_in_box;
        uniform.ambient = 1.0;
        uniform.gradient_shading = false;
        let (uv, geo) = floor_lanes();
        uniform.floor_uv = uv;
        uniform.floor_geo = geo;
        uniform.map_floor = true;
        uniform
    };

    let mut cameras_compared = 0usize;
    let mut checked_total = 0usize;
    for camera in ORBIT_CAMERAS {
        let view = view_at(camera);
        let mut placed = uniform([8, 8, 8], &view);
        placed.ground_box = PLACED;
        let frame = render(&device, &queue, &pipelines, &placed, &heights, &mirror);

        // (a) The surface is the PLACED one.
        let (worst, checked) = surface_error(&view, placed.occluder_t_scale, &frame, placed_height);
        checked_total += checked;
        assert!(
            checked >= 30,
            "{camera:?}: only {checked} surface points were reconstructed",
        );
        assert!(
            worst < 4e-3,
            "{camera:?}: a reconstructed surface point is {worst:.3e} box units \
             off the surface the placement lane describes, over {checked} \
             points. The mesh is not standing where `ground_box` puts it",
        );

        // (b) Nothing the lid used to cover is left with no ground at all.
        let no_field = render_without_heights(&device, &queue, &pipelines, &bare(&view), &mirror);
        let had_ground: Vec<bool> = no_field.offscreen.iter().map(|p| p[3] > 0).collect();
        let denominator = had_ground.iter().filter(|had| **had).count();
        // Below the box floor the lid dissolves to nothing, so there is no
        // denominator to compare against and the criterion is not about that
        // camera. Skipped by the property, never by the outcome.
        if denominator < (SIZE[0] * SIZE[1]) as usize / 20 {
            continue;
        }
        cameras_compared += 1;
        let lost: Vec<usize> = had_ground
            .iter()
            .enumerate()
            .filter(|(at, had)| **had && frame.offscreen[*at][3] == 0)
            .map(|(at, _)| at)
            .collect();
        // **Every lost pixel is one of two things, and neither is an uncovered
        // region.**
        //
        // The rasteriser's coverage rule and the march's analytic
        // `slab_entry_exit` necessarily disagree on the outermost pixel of a
        // silhouette - B1 measured that at one pixel of 4113. And a downward
        // ray that enters the box's xy column ALREADY below the terrain sheet
        // never meets it: the sheet has no thickness and the box's side wall is
        // open, so it reaches `z = 0` inside the square having drawn no ground.
        // That second one is a sliver at the box's rim that the flat lid used
        // to fill, it is the same size under the identity placement (measured),
        // and it is declared here rather than tolerated by a threshold.
        //
        // What must NOT happen is a pixel losing its ground with ground on
        // every side of it and a ray that entered above the sheet - which is
        // what an uncovered REGION looks like, and is the 8827 of 11299 the
        // review measured before the apron ring.
        let boundary = |at: usize| -> bool {
            let (col, row) = (
                (at % SIZE[0] as usize) as i64,
                (at / SIZE[0] as usize) as i64,
            );
            for dr in -1..=1i64 {
                for dc in -1..=1i64 {
                    let (c, r) = (col + dc, row + dr);
                    if c < 0 || r < 0 || c >= i64::from(SIZE[0]) || r >= i64::from(SIZE[1]) {
                        return true;
                    }
                    if !had_ground[(r * i64::from(SIZE[0]) + c) as usize] {
                        return true;
                    }
                }
            }
            false
        };
        let unexplained: Vec<usize> = lost
            .iter()
            .copied()
            .filter(|at| {
                !boundary(*at)
                    && !enters_below_the_sheet(
                        &view,
                        (*at % SIZE[0] as usize) as u32,
                        (*at / SIZE[0] as usize) as u32,
                        placed_height,
                    )
            })
            .collect();
        assert!(
            unexplained.is_empty(),
            "{camera:?}: {} of {denominator} pixels that carried the map floor \
             carry no ground at all once a field is placed over the middle \
             quarter of the box, and {} of those are neither on the \
             silhouette nor under the terrain sheet - so this is an uncovered \
             REGION. The mesh does not cover the footprint the field does not, \
             and the lid is suppressed frame-uniformly, so the pane shows \
             volume over nothing",
            lost.len(),
            unexplained.len(),
        );
        // The two explanations are a characterisation, not an amnesty: the
        // whole loss has to stay a rim effect. A tenth of the lid's own
        // footprint is far larger than either mechanism can produce and far
        // smaller than the 78% the review measured.
        assert!(
            lost.len() * 10 < denominator,
            "{camera:?}: {} of {denominator} pixels lost their ground. Both \
             mechanisms above are one-pixel-deep rim effects; this is a region",
            lost.len(),
        );
    }

    assert!(
        cameras_compared >= 6,
        "only {cameras_compared} of eleven cameras had a lid to compare \
         against, so the coverage half of this test is measuring almost nothing",
    );
    assert!(checked_total >= 3000);
}

/// **The mutant control for the lane: a shader that ignores `ground_box` must
/// fail the criterion above.**
///
/// The substitution is the exact edit that survived everything B3 first
/// shipped — the placed position replaced by the bare grid coordinate, which
/// stretches any field over the whole drawn box.
#[test]
#[ignore = "needs a real wgpu adapter; see the doc comment for the invocation"]
fn the_placement_criterion_notices_a_shader_that_ignores_the_lane() {
    let _held = gpu_lock();
    let (device, queue) = device();
    let attachments = attachments(wgpu::TextureFormat::Rgba8Unorm);

    const PLACED_LINE: &str =
        "    return scale * ((f32(column - 1u) + 0.5) / f32(posts)) + offset;";
    const STRETCHED: &str = "    return (f32(column - 1u) + 0.5) / f32(posts);";
    let source = squallar_volumetric::raymarch::VOLUME_SHADER_WGSL;
    assert_eq!(
        source.matches(PLACED_LINE).count(),
        1,
        "the grid's placed position has moved; re-anchor this mutant rather \
         than deleting it",
    );
    let pipelines = VolumePipelines::from_shader_source(
        &device,
        attachments,
        &source.replacen(PLACED_LINE, STRETCHED, 1),
    );
    pipelines.upload_quad(&queue);
    let mirror = planted_mirror(
        &device,
        &queue,
        &pipelines,
        [MIRROR_EDGE, MIRROR_EDGE],
        &checker_rgba(),
    );
    let heights = pipelines
        .upload_heights(&device, &queue, [POSTS, POSTS], &ridge_samples())
        .expect("the fixture height field was refused");

    let mut noticed = 0usize;
    let mut worst_seen = 0.0f32;
    for camera in ORBIT_CAMERAS {
        let view = view_at(camera);
        let mut placed = uniform([8, 8, 8], &view);
        placed.ground_box = PLACED;
        let frame = render(&device, &queue, &pipelines, &placed, &heights, &mirror);
        let (worst, checked) = surface_error(&view, placed.occluder_t_scale, &frame, placed_height);
        worst_seen = worst_seen.max(worst);
        if checked >= 30 && worst >= 4e-3 {
            noticed += 1;
        }
    }

    assert!(
        noticed >= 6,
        "a shader that ignores `ground_box` entirely was noticed at only \
         {noticed} of eleven cameras (worst surface error {worst_seen:.3e}). \
         The lane the uniform grew from 304 to 320 bytes to carry would then \
         be verified as host arithmetic with nothing measuring the shader that \
         consumes it",
    );
}

// ---------------------------------------------------------------------------
// B4: the patch edge.
// ---------------------------------------------------------------------------

/// **The placement a re-fit at the zoom stop actually produces.**
///
/// `MIN_EYE_DISTANCE = 0.05` against a default standoff of about 1.94 puts the
/// eye 32.5 km off a 920 km box's pivot, and 40 degrees of vertical field of
/// view at 25 degrees of pitch over a 16:10 viewport subtends about 66 km of
/// ground. 66.4 / 920 is 0.0722, centred.
///
/// **Deliberately not [`PLACED`]'s quarter.** That fixture was chosen to make
/// the placement lane visible; this one is the number
/// `squallar_elevation::plan` answers for the camera the whole unit exists for,
/// and at 0.5% of the box's area the apron is nearly the entire picture —
/// which is exactly the regime the open question is about.
const REFIT: [f32; 4] = [0.0722, 0.0722, 0.4639, 0.4639];

/// The surface a re-fitted mesh describes: the field inside [`REFIT`], and the
/// rim post's own height held flat outside it.
fn refit_height(p: [f32; 2]) -> f32 {
    let lo = post_center_fraction(0, POSTS);
    let hi = post_center_fraction(POSTS - 1, POSTS);
    let axis = |v: f32, scale: f32, offset: f32| ((v - offset) / scale).clamp(lo, hi);
    ridge_height([
        axis(p[0], REFIT[0], REFIT[2]),
        axis(p[1], REFIT[1], REFIT[3]),
    ])
}

/// Whether a box-space point is outside the re-fitted field's own footprint —
/// the apron, which under a zoom-stop re-fit is 99.5% of the drawn box.
fn on_the_apron(p: [f32; 2]) -> bool {
    let outside = |v: f32, scale: f32, offset: f32| {
        let f = (v - offset) / scale;
        !(0.0..=1.0).contains(&f)
    };
    outside(p[0], REFIT[0], REFIT[2]) || outside(p[1], REFIT[1], REFIT[3])
}

/// What the reconstruction found either side of the patch edge:
/// `(interior relief in box z, discriminating apron points)`.
///
/// **The second figure is the non-triviality half of the surface criterion, and
/// it is what stops "the apron is flat" being a sentence nothing can
/// contradict.** A first draft of this measured the apron's residual against
/// the flat model and asserted it small — but the same residual was the filter
/// for "is this point on the mesh at all", so the assertion was capped below
/// its own threshold by construction and could not fail. That is this
/// repository's documented vacuous-verification shape exactly, caught here by
/// writing down what the number was bounded by.
///
/// What is counted instead is apron points where the flat-skirt model and the
/// **stretched** one — the field pulled over the whole drawn box, which is what
/// a shader ignoring the apron would draw — differ by more than the criterion's
/// own tolerance. Every one of those is a point at which
/// [`the_patch_edge_stands_still_under_orbit_and_keeps_its_drape`]'s surface
/// check is discriminating rather than agreeing with both answers at once.
/// That test is `#[ignore]`d and runs under
/// `cargo test -p squallar-gpu --test volume_drape -- --ignored`.
fn patch_edge_evidence(view: &VolumeView, t_scale: f32, frame: &Frame) -> (f32, usize) {
    let mut discriminating = 0usize;
    let mut interior = (f32::MAX, f32::MIN);
    for row in (0..SIZE[1]).step_by(3) {
        for column in (0..SIZE[0]).step_by(3) {
            let at = (row * SIZE[0] + column) as usize;
            if frame.ground[at][3] != 255 {
                continue;
            }
            let t = squallar_volumetric::raymarch::unpack24_bytes([
                frame.occluder[at][0],
                frame.occluder[at][1],
                frame.occluder[at][2],
            ]) * t_scale;
            let direction = ray_through(view, column, row);
            let p = [
                view.eye_in_box[0] + direction[0] * t,
                view.eye_in_box[1] + direction[1] * t,
                view.eye_in_box[2] + direction[2] * t,
            ];
            if on_the_apron([p[0], p[1]]) {
                // The two answers, and whether this point can tell them apart.
                // Deliberately computed from the point's POSITION alone and
                // never from what was observed there: a rule that read the
                // pixel would be selecting away the failures it exists to find.
                let flat = refit_height([p[0], p[1]]);
                let stretched = ridge_height([p[0], p[1]]);
                if (flat - stretched).abs() > 4e-3 {
                    discriminating += 1;
                }
            } else if (p[2] - refit_height([p[0], p[1]])).abs() <= 2e-3 {
                interior = (interior.0.min(p[2]), interior.1.max(p[2]));
            }
        }
    }
    let span = if interior.0 <= interior.1 {
        interior.1 - interior.0
    } else {
        0.0
    };
    (span, discriminating)
}

/// The **answer to B4's open sub-question: what the patch edge does under
/// orbit.**
///
/// It does three things, and this measures all three at all eleven cameras:
///
/// 1. **It stands still.** The mesh is authored in box space and `ground_box`
///    is a box-space affine, so the surface — crease included — is a function
///    of `(x, y)` with no camera in it. The criterion is that one host model of
///    that function predicts every reconstructed surface point from every
///    camera. A crease that swam with the eye would fail it at the cameras it
///    swam to.
/// 2. **It is a terrace lip, not a hole and not a cliff.** The apron is flat at
///    the rim post's own height and joins the field's rim exactly — `box_axis`
///    duplicates the rim post rather than pulling it out — so the surface is
///    continuous across the edge and its *slope* is what jumps. No gap, no
///    z-fight, and no ground missing: what the eye sees is a lip.
/// 3. **The map keeps its registration across it.** `fs_ground` drapes at the
///    fragment's own surface point, and the apron's fragments have real box
///    coordinates even though their height is the rim's — so the basemap runs
///    on across the lip undisturbed. That is the half nothing measured before:
///    every checkerboard sample in this file until now was taken over a field
///    on the identity placement, where there is no apron to be wrong about.
///
/// The third is the one with a plausible wrong answer, and
/// [`the_patch_edge_criterion_notices_a_drape_clamped_to_the_field`] is the
/// mutant for it — clamping the *colour* to the field's footprint the way the
/// *height* is clamped is the natural mistake, and it smears the rim's map
/// across the entire apron. That mutant is `#[ignore]`d like everything else
/// in this file; run it with
/// `cargo test -p squallar-gpu --test volume_drape -- --ignored`. Measured on
/// this hardware: it is noticed at **11 of 11 cameras, on 4539 of 9215
/// compared apron points**.
#[test]
#[ignore = "needs a real wgpu adapter; see the doc comment for the invocation"]
fn the_patch_edge_stands_still_under_orbit_and_keeps_its_drape() {
    let _held = gpu_lock();
    let (device, queue) = device();
    let pipelines = VolumePipelines::new(&device, attachments(wgpu::TextureFormat::Rgba8Unorm));
    pipelines.upload_quad(&queue);
    let mirror = planted_mirror(
        &device,
        &queue,
        &pipelines,
        [MIRROR_EDGE, MIRROR_EDGE],
        &checker_rgba(),
    );
    let heights = pipelines
        .upload_heights(&device, &queue, [POSTS, POSTS], &ridge_samples())
        .expect("the fixture height field was refused");

    let mut apron_compared_total = 0usize;
    let mut cameras_with_relief = 0usize;
    let mut discriminating_total = 0usize;
    let mut worst_surface_error = 0.0f32;
    for camera in ORBIT_CAMERAS {
        let view = view_at(camera);
        let mut placed = uniform([8, 8, 8], &view);
        placed.ground_box = REFIT;
        let frame = render(&device, &queue, &pipelines, &placed, &heights, &mirror);

        // (1) It stands still: one box-space model predicts every camera.
        let (worst, checked) = surface_error(&view, placed.occluder_t_scale, &frame, refit_height);
        worst_surface_error = worst_surface_error.max(worst);
        assert!(
            checked >= 30,
            "{camera:?}: only {checked} surface points were reconstructed",
        );
        assert!(
            worst < 4e-3,
            "{camera:?}: a reconstructed surface point is {worst:.3e} box units \
             off the box-space surface the re-fit describes, over {checked} \
             points. The patch edge is somewhere this camera put it rather than \
             where the placement did — it swims",
        );

        // (2) It is a lip: the check above is discriminating out on the apron
        // and not only over the patch, and the patch really has a field in it.
        let (interior_relief, discriminating) =
            patch_edge_evidence(&view, placed.occluder_t_scale, &frame);
        discriminating_total += discriminating;
        if interior_relief > RIDGE * 0.3 {
            cameras_with_relief += 1;
        }

        // (3) The drape runs on across it: the checkerboard oracle, restricted
        // to apron points.
        let Comparison {
            compared,
            wrong,
            off_surface: _,
        } = compare_where(
            &view,
            placed.occluder_t_scale,
            &frame,
            refit_height,
            on_the_apron,
        );
        apron_compared_total += compared;
        assert!(
            wrong.is_empty(),
            "{camera:?}: {} of {compared} APRON surface points carry the wrong \
             checker cell (first: {:?}). The map does not run on across the \
             patch edge — the flat skirt outside the re-fitted field is drawn \
             with the wrong part of the basemap on it",
            wrong.len(),
            &wrong[..wrong.len().min(4)],
        );
    }

    // The apron is where nearly every pixel is at this placement, so a run that
    // compared few of them would be a run that had measured the interior and
    // called it the edge. Measured on this hardware: 9215 apron points
    // compared across the eleven cameras, worst surface residual 4.8e-4 against
    // a 4e-3 tolerance.
    assert!(
        apron_compared_total >= 3000,
        "only {apron_compared_total} apron points were compared across all \
         eleven cameras",
    );
    // **The surface criterion is discriminating on the apron.** Measured on
    // this hardware: **158,896** of the reconstructed apron points sit where
    // the flat skirt and the stretched field disagree by more than the
    // tolerance, so the per-camera check above is telling those two answers
    // apart at a hundred thousand places rather than agreeing with both.
    //
    // **Two counts, two denominators, never added.** This one is every
    // reconstructed apron point; `apron_compared_total` is only those the
    // CHECKERBOARD could also use, which means clear of a cell boundary by
    // `CENTRE_CLEARANCE` on both axes. That filter keeps
    // `(1 - 2 * 0.38)^2 = 0.0576`, and `158896 * 0.0576` is **9152** against
    // the 9215 measured — under a percent apart, which is what the clearance
    // rule alone predicts. (An earlier note rounded 0.0576 to 0.058 and got
    // 9216, one off the measurement; that agreement was a rounding artefact
    // and is not evidence of anything.) A thousand is margin, not a tuned
    // figure.
    assert!(
        discriminating_total >= 1000,
        "only {discriminating_total} apron points can tell the flat skirt from \
         a stretched field, so 'the surface is the placed one' is not being \
         asserted about the patch edge at all",
    );
    // And the fixture really does have a field inside the patch: without this
    // the criterion would pass against a field with no relief anywhere, where
    // there is no edge to have a question about. Measured: 10 of 11.
    assert!(
        cameras_with_relief >= 3,
        "only {cameras_with_relief} of eleven cameras reconstructed any relief \
         INSIDE the re-fitted patch, so 'the apron is flat and the interior is \
         not' is measuring one half of a comparison",
    );
    assert!(worst_surface_error < 4e-3);
}

/// **The mutant control for the patch edge's drape: a shader that clamps the
/// map to the field's own footprint the way it clamps the height.**
///
/// This is the natural mistake, not a contrived one. The apron *is* the rim
/// extended — `post_of_column` clamps the height sample to the rim post — and
/// applying the same clamp to `map_colour_at_km`'s arguments reads as finishing
/// the job. What it actually does is smear the basemap's rim pixel across
/// 99.5% of the box at a zoom-stop re-fit, and no criterion in this directory
/// noticed before this one, because every other fixture sits on the identity
/// placement where the clamp is a no-op.
#[test]
#[ignore = "needs a real wgpu adapter; see the doc comment for the invocation"]
fn the_patch_edge_criterion_notices_a_drape_clamped_to_the_field() {
    let _held = gpu_lock();
    let (device, queue) = device();
    let attachments = attachments(wgpu::TextureFormat::Rgba8Unorm);

    const TRUE_DRAPE: &str =
        "    let ground = map_colour_at_km(box_x_km(in.box_p.x), box_y_km(in.box_p.y));";
    const CLAMPED_DRAPE: &str = "    let ground = map_colour_at_km(box_x_km(clamp(in.box_p.x, volume.ground_box.z, volume.ground_box.z + volume.ground_box.x)), box_y_km(clamp(in.box_p.y, volume.ground_box.w, volume.ground_box.w + volume.ground_box.y)));";

    let source = squallar_volumetric::raymarch::VOLUME_SHADER_WGSL;
    assert_eq!(
        source.matches(TRUE_DRAPE).count(),
        1,
        "the ground fragment's drape has moved; re-anchor this mutant rather \
         than deleting it. Anchor was:\n{TRUE_DRAPE}",
    );
    let pipelines = VolumePipelines::from_shader_source(
        &device,
        attachments,
        &source.replacen(TRUE_DRAPE, CLAMPED_DRAPE, 1),
    );
    pipelines.upload_quad(&queue);
    let mirror = planted_mirror(
        &device,
        &queue,
        &pipelines,
        [MIRROR_EDGE, MIRROR_EDGE],
        &checker_rgba(),
    );
    let heights = pipelines
        .upload_heights(&device, &queue, [POSTS, POSTS], &ridge_samples())
        .expect("the fixture height field was refused");

    let mut noticed = 0usize;
    let mut wrong_total = 0usize;
    let mut compared_total = 0usize;
    for camera in ORBIT_CAMERAS {
        let view = view_at(camera);
        let mut placed = uniform([8, 8, 8], &view);
        placed.ground_box = REFIT;
        let frame = render(&device, &queue, &pipelines, &placed, &heights, &mirror);
        let Comparison {
            compared, wrong, ..
        } = compare_where(
            &view,
            placed.occluder_t_scale,
            &frame,
            refit_height,
            on_the_apron,
        );
        compared_total += compared;
        wrong_total += wrong.len();
        if compared >= 30 && !wrong.is_empty() {
            noticed += 1;
        }
    }

    assert!(
        noticed >= 6,
        "a shader that drapes the apron with the field's rim colour was \
         noticed at only {noticed} of eleven cameras ({wrong_total} wrong of \
         {compared_total} compared). The apron would then be an untested lane \
         covering nearly the whole box whenever the camera is dollied in",
    );
}
