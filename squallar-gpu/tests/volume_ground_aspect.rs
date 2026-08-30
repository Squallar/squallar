//! **Does an extreme aspect draw, with nothing edited?**
//!
//! B3 made the ground grid read its dimensions from
//! `textureDimensions(height_texture)` and deleted the `GROUND_POSTS` constant,
//! so a field of any size is supposed to draw with no source change at all.
//! B4 is what makes that reachable: `HalfExtentKm::clamped` floors each axis at
//! 10 km and then bounds the corner, so **1329 km x 20 km — 66.45:1 — is a box
//! a user can get to**, and `HeightPlan::fit` puts the rung's posts on the long
//! axis and derives the short one from the aspect. What comes out is a field
//! like 1024 x 15, which is a shape nothing in this directory had ever drawn.
//!
//! ```text
//! cargo test -p squallar-gpu --test volume_ground_aspect -- --ignored --nocapture
//! ```
//!
//! `#[ignore]`d (CI's `gpu` job opts in on lavapipe), and the tests hold the
//! shared process-wide GPU lock, like every other suite in this directory.
//!
//! # Why this is not the drape suite over again
//!
//! `volume_drape.rs` asks whether the map lands in the right place, and every
//! field it renders is square. What is asked here is narrower and different:
//! that the **grid** — vertex count, apron ring, per-axis layout — is right
//! when the two axes are two orders of magnitude apart. The failure this
//! guards against is an axis-blind arithmetic that is invisible on a square
//! fixture: `posts.x` used where `posts.y` was meant reads as correct at every
//! square field in the tree and draws a torn mesh here.
//!
//! # The oracle
//!
//! The surface is reconstructed the way `volume_drape.rs` reconstructs it —
//! from the occluder's own packed ray parameter along the ray through the
//! pixel's own centre — and required to lie on the analytic surface the host
//! built the field from. That surface is **deliberately different on each
//! axis**: a ramp east and a sawtooth north, so a stage that read one axis's
//! post count for the other lands somewhere the host does not predict rather
//! than somewhere that happens to agree.
#![cfg(not(target_arch = "wasm32"))]

use egui_wgpu::wgpu;
use squallar_device_profile::quality::{GroundPass, ResolutionRung};
use squallar_egui::pane::OrbitCamera;
use squallar_egui::volume_view::{VolumeView, view_for};
use squallar_volumetric::raymarch::staging::VolumeStaging;
use squallar_volumetric::raymarch::{
    GroundHeights, OffscreenPlan, VolumePipelines, ground_vertex_count, post_center_fraction,
};
use squallar_volumetric::uniform::VolumeUniform;

mod gpu_harness;
use gpu_harness::{
    MIRROR_FORMAT, ORBIT_CAMERAS, OrbitFixture, attachments, device, equatorial_floor_lanes_of,
    gpu_lock, opaque_white_lut, planted_mirror, read_back,
};

const SIZE: [u32; 2] = [640, 400];

/// **1329 km by 20 km, 66.45:1** — the extreme `HalfExtentKm::clamped` can
/// reach, and the one the plan names. 20 km tall, so the vertical is not what
/// makes it degenerate.
const BOX_KM: [f32; 3] = [1329.0, 20.0, 20.0];

/// **The two fields `HeightPlan::fit` actually answers over a box this shape**,
/// one per adapter class: a browser on the WebGL2 downlevel guarantee, where
/// the adapter's own 2048-post ceiling binds, and a desktop, where the tile
/// ceiling does one zoom level down.
///
/// **Measured, not derived here.** An earlier version of this file used
/// `[1024, 15]` and called it the planner's answer; it was not — the fit
/// produced 126 distinct shapes over 350 cameras on that box and never that
/// one, so the criteria below were running at a quarter of production's mesh.
/// `squallar-gpu` cannot call `HeightPlan::fit` (the resample runs inside the
/// offload worker, which links neither egui nor wgpu, and `squallar-elevation`
/// is not a dependency here), so these are constants anchored by
/// `squallar_elevation::plan`'s own
/// `the_sixty_six_to_one_box_is_fitted_to_these_two_shapes`. **If that test
/// moves, these are stale.**
const POSTS: [[u32; 2]; 2] = [[2048, 31], [5599, 84]];

/// The crest of the fixture surface, in box `z`.
const CREST: f32 = 0.40;

/// The fixture's own relief has to be worth the tolerance the surface criterion
/// compares against, or "within 6e-3 box units" would pass against a flat
/// sheet. The east ramp spans `CREST * 0.45` of box height, which is twenty
/// times the tolerance — held at compile time, the way
/// `squallar_elevation::jobs`'s own fixture bound is.
const _: () = assert!(CREST * 0.45 > 6e-3 * 20.0);

/// The height encoding: box `z` is the raw sample over the `u16` ceiling. This
/// file is about the grid, not the elevation encoding.
const HEIGHT_SCALE: f32 = 1.0 / 65_535.0;

/// Mirror texels a side. Small on purpose — the drape is `volume_drape.rs`'s
/// question, and all that is needed here is a mirror for the ground pass to
/// have something to sample.
const MIRROR_EDGE: u32 = 64;

/// **The analytic surface, and it is a different function on each axis.**
///
/// East: a monotone ramp, so a stage that transposed the axes would read the
/// sawtooth where the ramp should be. North: four teeth over fifteen posts, so
/// the function varies on the scale of a *single post* on the short axis —
/// which is where an off-by-one in the apron ring or in `post_of_column` shows
/// up as a whole tooth's worth of error rather than as a rounding difference.
fn surface(uv: [f32; 2]) -> f32 {
    let ramp = uv[0];
    let teeth = (uv[1] * 4.0).fract();
    (CREST * (0.35 + 0.45 * ramp + 0.20 * teeth)).clamp(0.0, 1.0)
}

/// The field's samples for one shape, one `u16` a post, at the post centres the
/// resampler measures at.
fn samples(posts: [u32; 2]) -> Vec<u16> {
    let mut out = Vec::with_capacity((posts[0] * posts[1]) as usize);
    for j in 0..posts[1] {
        for i in 0..posts[0] {
            let z = surface([
                post_center_fraction(i, posts[0]),
                post_center_fraction(j, posts[1]),
            ]);
            out.push((z / HEIGHT_SCALE).round().clamp(0.0, 65_535.0) as u16);
        }
    }
    out
}

/// The mesh's surface at a box-space point, apron ring included: the field's
/// own coordinate clamped to the outermost post centres on **each axis
/// separately**, which is what a duplicated rim post is.
///
/// Bilinear between posts, because that is what the rasteriser interpolates
/// across a cell — and on the short axis, where a cell is a fifteenth of the
/// box, the difference between "the nearest post's height" and the
/// interpolation is most of the tooth.
fn expected_height(posts: [u32; 2], p: [f32; 2]) -> f32 {
    let axis = |v: f32, posts: u32| {
        let lo = post_center_fraction(0, posts);
        let hi = post_center_fraction(posts - 1, posts);
        let clamped = v.clamp(lo, hi);
        // Back to a post index, and the fraction between it and the next.
        let at = clamped * posts as f32 - 0.5;
        let i = at.floor().clamp(0.0, (posts - 1) as f32);
        (i as u32, (at - i).clamp(0.0, 1.0))
    };
    let (i, fx) = axis(p[0], posts[0]);
    let (j, fy) = axis(p[1], posts[1]);
    let at = |di: u32, dj: u32| {
        surface([
            post_center_fraction((i + di).min(posts[0] - 1), posts[0]),
            post_center_fraction((j + dj).min(posts[1] - 1), posts[1]),
        ])
    };
    let south = at(0, 0) * (1.0 - fx) + at(1, 0) * fx;
    let north = at(0, 1) * (1.0 - fx) + at(1, 1) * fx;
    south * (1.0 - fy) + north * fy
}

fn view_at((yaw, pitch, distance, exaggeration): OrbitFixture) -> VolumeView {
    let camera =
        OrbitCamera::restore(yaw, pitch, distance, [0.0; 3], exaggeration).expect("finite camera");
    view_for(camera, BOX_KM, SIZE[0] as f32 / SIZE[1] as f32).expect("a view")
}

struct Frame {
    occluder: Vec<[u8; 4]>,
    ground: Vec<[u8; 4]>,
}

fn render(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    pipelines: &VolumePipelines,
    uniform: &VolumeUniform,
    heights: &GroundHeights,
    mirror: &squallar_volumetric::raymarch::PaneMirror,
) -> Frame {
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
    pipelines.encode_ground(
        &mut encoder,
        &target,
        &volume,
        Some(mirror),
        Some(heights),
        None,
    );
    pipelines.encode_raymarch_with_floor(&mut encoder, &target, &volume, Some(mirror));
    queue.submit(Some(encoder.finish()));

    Frame {
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

/// The unit ray through a pixel's own centre.
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

/// `(worst residual, points checked)` against [`expected_height`].
fn surface_error(posts: [u32; 2], view: &VolumeView, t_scale: f32, frame: &Frame) -> (f32, usize) {
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
            worst = worst.max((p[2] - expected_height(posts, [p[0], p[1]])).abs());
        }
    }
    (worst, checked)
}

fn uniform(view: &VolumeView) -> VolumeUniform {
    let mut uniform = VolumeUniform::new(BOX_KM, [8, 8, 8]);
    uniform.box_from_clip = view.box_from_clip;
    uniform.clip_from_box = view.clip_from_box;
    uniform.eye_in_box = view.eye_in_box;
    uniform.ambient = 1.0;
    uniform.gradient_shading = false;
    // The box is 1329 km by 20 km, so the mirror's lanes are the equatorial
    // helper's at the same two spans in degrees.
    let (uv, geo) = equatorial_floor_lanes_of(
        f64::from(BOX_KM[0]) / f64::from(gpu_harness::DEGREE_BOX_KM),
        f64::from(BOX_KM[1]) / f64::from(gpu_harness::DEGREE_BOX_KM),
        !MIRROR_FORMAT.is_srgb(),
    );
    uniform.floor_uv = uv;
    uniform.floor_geo = geo;
    uniform.aim_occluder(CREST, HEIGHT_SCALE, 0.0);
    uniform
}

/// **A 66:1 field draws, and the mesh it draws is the one the host built.**
///
/// Over all eleven cameras, **at both shapes the fit answers**. What would fail
/// it is any arithmetic in the vertex stage that reads one axis's post count
/// for the other: the vertex-per-cell walk, the apron ring's two outer columns,
/// `post_of_column`'s clamp, or `box_axis`'s division. Every one of those is
/// invisible on a square field.
///
/// Measured on this hardware, per shape: `[2048, 31]` — 10 of 11 cameras
/// compared, 166,489 points, worst residual **1.08e-4**; `[5599, 84]` — 10 of
/// 11, 166,333 points, worst **4.35e-4**. Both against the 6e-3 tolerance
/// below, so the thresholds are margin rather than tuned figures. The residual
/// is tighter than `volume_drape.rs`'s because [`expected_height`] models the
/// rasteriser's bilinear interpolation across a cell rather than the analytic
/// surface the field was sampled from.
#[test]
#[ignore = "needs a real wgpu adapter; see the doc comment for the invocation"]
fn a_sixty_six_to_one_field_draws_the_mesh_the_host_built() {
    let _held = gpu_lock();
    let (device, queue) = device();
    let pipelines = VolumePipelines::new(&device, attachments(wgpu::TextureFormat::Rgba8Unorm));
    pipelines.upload_quad(&queue);
    let mirror = planted_mirror(
        &device,
        &queue,
        &pipelines,
        [MIRROR_EDGE, MIRROR_EDGE],
        // Opaque: `fs_ground` writes the MIRROR's alpha into the ground
        // attachment (off the mirror there is no map to drape with), and the
        // reconstruction below selects on that alpha being 255. A mirror at any
        // other alpha makes every criterion here skip every pixel — measured,
        // as the first spelling of this fixture.
        &[200u8, 200, 200, 255].repeat((MIRROR_EDGE * MIRROR_EDGE) as usize),
    );

    for posts in POSTS {
        let heights = pipelines
            .upload_heights(&device, &queue, posts, &samples(posts))
            .expect("a shape the fit answers was refused; the texture is degenerate");

        let mut checked_total = 0usize;
        let mut worst_seen = 0.0f32;
        let mut cameras_compared = 0usize;
        for camera in ORBIT_CAMERAS {
            let view = view_at(camera);
            let uniform = uniform(&view);
            let frame = render(&device, &queue, &pipelines, &uniform, &heights, &mirror);
            let (worst, checked) = surface_error(posts, &view, uniform.occluder_t_scale, &frame);
            // A 66:1 box is a sliver, and at the standoffs this set was chosen
            // for against a SQUARE box some cameras frame almost none of it.
            // Skipped by the property — how many points the mesh put on the
            // offscreen — never by the outcome.
            if checked < 200 {
                continue;
            }
            cameras_compared += 1;
            checked_total += checked;
            worst_seen = worst_seen.max(worst);
            assert!(
                worst < 6e-3,
                "{posts:?} {camera:?}: a reconstructed surface point is \
                 {worst:.3e} box units off the mesh the host built, over \
                 {checked} points. On a 66:1 field that is an axis-blind \
                 arithmetic in the vertex stage — the kind every square fixture \
                 in this directory reads as correct",
            );
        }
        assert!(
            cameras_compared >= 6,
            "{posts:?}: only {cameras_compared} of eleven cameras framed enough \
             of a 66:1 box to compare, so this criterion is measuring almost \
             nothing",
        );
        assert!(
            checked_total >= 20_000,
            "{posts:?}: only {checked_total} points were reconstructed across \
             the whole set",
        );
    }
}

/// The grid a 66:1 plan asks for is one the draw can actually issue, and the
/// count is per-axis rather than one axis squared.
///
/// Host-side, so it runs in `cargo test --workspace` where the GPU criterion
/// above does not.
#[test]
fn the_sixty_six_to_one_grid_is_a_u32_draw_of_the_right_width() {
    for posts in POSTS {
        let count = ground_vertex_count(posts).expect("the fitted grid is drawable");
        // One cell more than there are posts on each axis: the apron ring.
        assert_eq!(count, 6 * (posts[0] + 1) * (posts[1] + 1));
        // Non-triviality: the two axes really are far apart, so a count derived
        // from either one alone is a different number.
        assert_ne!(count, 6 * (posts[0] + 1) * (posts[0] + 1));
        assert_ne!(count, 6 * (posts[1] + 1) * (posts[1] + 1));
        assert!(posts[0] > posts[1] * 60, "{posts:?} is not 66:1 at all");
        // Both axes drawable: two is what `upload_heights` refuses below.
        assert!(posts.iter().all(|p| *p >= 2));
    }
    // The two shapes are genuinely different meshes, so rendering both is not
    // rendering one twice — and the desktop one is the larger.
    assert_ne!(POSTS[0], POSTS[1]);
    assert!(POSTS[1][0] > POSTS[0][0] * 2);
}

/// The analytic surface really is a different function on each axis, which is
/// what makes the GPU criterion above able to notice a transposed stage.
#[test]
fn the_fixture_surface_separates_the_two_axes() {
    // East is monotone; north is not.
    let east: Vec<f32> = (0..16).map(|i| surface([i as f32 / 15.0, 0.3])).collect();
    assert!(east.windows(2).all(|w| w[1] > w[0]), "{east:?}");
    let north: Vec<f32> = (0..16).map(|j| surface([0.3, j as f32 / 15.0])).collect();
    assert!(
        north.windows(2).any(|w| w[1] < w[0]),
        "the north axis is monotone too, so a transposed stage would land \
         somewhere this fixture predicts: {north:?}",
    );
    // And transposing the arguments is a visible difference nearly everywhere.
    let differing = (0..64)
        .filter(|k| {
            let (u, v) = ((k % 8) as f32 / 7.0, (k / 8) as f32 / 7.0);
            (surface([u, v]) - surface([v, u])).abs() > 6e-3
        })
        .count();
    assert!(
        differing >= 48,
        "only {differing} of 64 sample points can tell the two axes apart",
    );
}

/// **The mutant control: a vertex stage that reads the wrong axis's post
/// count.**
///
/// The substitution transposes which axis bounds each column's post lookup —
/// `post_of_column(column.x, posts.y)` and the mirror of it. On **every square
/// field in this repository that edit is a no-op**, which is the whole reason
/// this file exists: `volume_drape.rs`, `volume_occluder.rs` and
/// `volume_shader.rs` would all stay green against it, and the first thing to
/// notice would be a user on a pancake box seeing terrain that stops a
/// sixtieth of the way across.
///
/// Measured on this hardware: noticed at **10 of 10 comparable cameras at both
/// shapes**, with a worst residual of **0.175 box units** against the 6e-3
/// tolerance — the mutant is not a marginal one, it draws a different mesh.
#[test]
#[ignore = "needs a real wgpu adapter; see the doc comment for the invocation"]
fn the_aspect_criterion_notices_a_stage_that_transposes_the_axes() {
    let _held = gpu_lock();
    let (device, queue) = device();
    let attachments = attachments(wgpu::TextureFormat::Rgba8Unorm);

    const TRUE_LOOKUP: &str =
        "        post_of_column(column.x, posts.x),\n        post_of_column(column.y, posts.y),";
    const TRANSPOSED: &str =
        "        post_of_column(column.x, posts.y),\n        post_of_column(column.y, posts.x),";

    let source = squallar_volumetric::raymarch::VOLUME_SHADER_WGSL;
    assert_eq!(
        source.matches(TRUE_LOOKUP).count(),
        1,
        "the vertex stage's post lookup has moved; re-anchor this mutant rather \
         than deleting it. Anchor was:\n{TRUE_LOOKUP}",
    );
    let pipelines = VolumePipelines::from_shader_source(
        &device,
        attachments,
        &source.replacen(TRUE_LOOKUP, TRANSPOSED, 1),
    );
    pipelines.upload_quad(&queue);
    let mirror = planted_mirror(
        &device,
        &queue,
        &pipelines,
        [MIRROR_EDGE, MIRROR_EDGE],
        &[200u8, 200, 200, 255].repeat((MIRROR_EDGE * MIRROR_EDGE) as usize),
    );
    for posts in POSTS {
        let heights = pipelines
            .upload_heights(&device, &queue, posts, &samples(posts))
            .expect("the fixture height field was refused");

        let mut noticed = 0usize;
        let mut comparable = 0usize;
        let mut worst_seen = 0.0f32;
        for camera in ORBIT_CAMERAS {
            let view = view_at(camera);
            let uniform = uniform(&view);
            let frame = render(&device, &queue, &pipelines, &uniform, &heights, &mirror);
            let (worst, checked) = surface_error(posts, &view, uniform.occluder_t_scale, &frame);
            if checked < 200 {
                continue;
            }
            comparable += 1;
            worst_seen = worst_seen.max(worst);
            if worst >= 6e-3 {
                noticed += 1;
            }
        }

        assert!(
            comparable >= 6,
            "{posts:?}: only {comparable} cameras framed enough of the box for \
             the mutant to have been noticed at all",
        );
        assert!(
            noticed >= 6,
            "{posts:?}: a vertex stage reading the wrong axis's post count was \
             noticed at only {noticed} of {comparable} comparable cameras \
             (worst residual {worst_seen:.3e}). Every other ground suite here \
             is square and would stay green, so this file would be the only \
             thing standing between that edit and a user",
        );
    }
}
