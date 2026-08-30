//! Do the prisms stand on the terrain, occlude the volume, and get occluded?
//!
//! Driven through the **production** `encode_ground` + `encode_raymarch_with_floor`
//! pair recorded into one encoder in that order — the same two calls
//! `volume_bridge::prepare` makes, with a real building mesh handed to the first.
//!
//! ```text
//! cargo test -p squallar-gpu --test volume_buildings -- --ignored --nocapture
//! ```
//!
//! `#[ignore]`d (CI's `gpu` job opts in on lavapipe), and the tests hold the
//! shared process-wide GPU lock.
//!
//! # The city is real, and that is not decoration
//!
//! Every frame here extrudes `squallar-buildings`' committed Monaco tile
//! through `read_footprints` + `extrude` — the same two calls the worker's job
//! row makes. A synthetic block of boxes would have been easier and would have
//! measured something this app never draws: the archive's real footprints carry
//! **175.6 ring vertices each**, not the four a rectangle has, and that ratio is
//! what made an earlier capacity claim in `squallar_buildings::budget` wrong by
//! 8.7x.
//!
//! # The trap these fixtures are built against
//!
//! **A building on flat ground cannot distinguish "sits on the terrain surface"
//! from "extruded from z = 0".** That is this unit's central claim and a flat
//! fixture proves nothing about it, in the same way B1's single camera, B3's
//! identity `ground_box` and C2's identity normal factor each hid a lane that a
//! mutant could delete entire. So every frame below stands the city on a ridge
//! whose height varies by a quarter of the box across the footprint, and every
//! criterion that reads a position carries a **flat-ground twin** that is
//! asserted to MISS — [`the_prisms_stand_on_the_terrain_and_not_on_the_box_floor`]
//! measures both halves and prints them.
//!
//! # What is asserted where
//!
//! * [`the_prisms_stand_on_the_terrain_and_not_on_the_box_floor`] — the
//!   registration oracle: a host ray-cast against the very triangles the GPU
//!   drew, against the decoded occluder `t`, over [`CAMERAS`].
//! * [`a_prism_shortens_the_march_it_stands_in_front_of`] — occlusion of the
//!   volume, as the mechanism (`t` shrinks) and as the picture (the composite
//!   moves).
//! * [`a_prism_behind_the_ridge_is_hidden_by_it`] — occlusion BY terrain, with
//!   its near-side twin as the non-triviality half.
//! * [`a_footprint_the_parse_dropped_reaches_no_pixel`] — what `hide_3d` costs
//!   at the glass, and exactly what this file leans on `squallar-buildings` for.
//! * [`the_vertex_stride_is_what_the_budget_prices`] — the two crates' idea of
//!   what a vertex costs is one number.
#![cfg(not(target_arch = "wasm32"))]

use egui_wgpu::wgpu;
use squallar_buildings::footprint::Ring;
use squallar_buildings::{
    BoxFrame, BuildingFootprint, BuildingMesh, PrismBudget, PrismCeilings, TileId, extrude,
    read_footprints,
};
use squallar_device_profile::quality::{GroundPass, ResolutionRung};
use squallar_egui::pane::OrbitCamera;
use squallar_egui::volume_view::{VolumeView, view_for};
use squallar_volumetric::raymarch::staging::VolumeStaging;
use squallar_volumetric::raymarch::{
    BuildingPrisms, GroundHeights, OffscreenPlan, PaneMirror, VolumePipelines, ground_surface_at,
    post_center_fraction, unpack24_bytes,
};
use squallar_volumetric::uniform::{IDENTITY_GROUND_BOX, VolumeUniform};

mod gpu_harness;
use gpu_harness::{
    MIRROR_FORMAT, attachments, device, gpu_lock, opaque_white_lut, planted_mirror, read_back,
};

// ---------------------------------------------------------------------------
// The scene.
// ---------------------------------------------------------------------------

/// The box the city is drawn in: **2 km across and 400 m tall**.
///
/// **Small on purpose, and it is the honest scale rather than a convenience.**
/// A 43 m building on a shipped 240 km radar box is 0.02% of the width and
/// lands on no pixel at any offscreen this suite could afford, so a fixture at
/// that scale would assert things about geometry it could not see. 2 km is the
/// order a dollied camera actually looks at a city through — the plan's own
/// arithmetic for `squallar-buildings` is a ~24 km patch at a 39x dolly — and
/// the Monaco tile this file extrudes is 1.77 km across, so the box holds most
/// of one real tile.
const BOX_KM: [f32; 3] = [2.0, 2.0, 0.4];

/// The offscreen every frame here is rendered at.
const SIZE: [u32; 2] = [320, 200];

/// Posts a side in the fixture height field. Not a production constant — the
/// grid is laid out from `textureDimensions(height_texture)`.
const POSTS: u32 = 256;

/// Width of the fixture ridge, as a fraction of the box's east-west extent.
const RIDGE_SIGMA: f32 = 0.18;

/// The ridge's height in box `z`. At [`BOX_KM`] that is **100 m of relief over
/// 2 km**, which is Monaco's own hillside to within a factor of 1.5.
///
/// The number that matters is not the metres, it is that the terrain under one
/// building differs from the terrain under its neighbour by far more than any
/// tolerance in this file — which is what makes "stands on the terrain" and
/// "extruded from z = 0" different pictures.
const RIDGE_AMPLITUDE: f32 = 0.25;

/// The fixture height field, in box `z`: one ridge running north across the
/// box, peaked at its middle. A Gaussian rather than a cone, so the surface has
/// no crease for a `t` to interpolate across discontinuously.
fn ridge_height(uv: [f32; 2]) -> f32 {
    let d = (uv[0] - 0.5) / RIDGE_SIGMA;
    (RIDGE_AMPLITUDE * (-0.5 * d * d).exp()).clamp(0.0, 1.0)
}

/// The height encoding these fixtures use: box `z` maps to a raw sample
/// linearly with no offset.
const HEIGHT_SCALE: f32 = 1.0 / 65_535.0;

/// One post's height as the GPU decodes it, which is the quantised ridge and
/// not the analytic one. The oracle reads this, so the encoding's own quantum
/// never enters a tolerance.
fn post_height(i: u32, j: u32) -> f32 {
    let uv = [
        post_center_fraction(i, POSTS),
        post_center_fraction(j, POSTS),
    ];
    let raw = (ridge_height(uv) / HEIGHT_SCALE)
        .round()
        .clamp(0.0, 65_535.0) as u16;
    f32::from(raw) * HEIGHT_SCALE
}

/// The fixture field's samples, one `u16` a post.
fn ridge_samples() -> Vec<u16> {
    let mut samples = Vec::with_capacity((POSTS * POSTS) as usize);
    for j in 0..POSTS {
        for i in 0..POSTS {
            let uv = [
                post_center_fraction(i, POSTS),
                post_center_fraction(j, POSTS),
            ];
            samples.push(
                (ridge_height(uv) / HEIGHT_SCALE)
                    .round()
                    .clamp(0.0, 65_535.0) as u16,
            );
        }
    }
    samples
}

fn heights(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    pipelines: &VolumePipelines,
) -> GroundHeights {
    pipelines
        .upload_heights(device, queue, [POSTS, POSTS], &ridge_samples())
        .expect("the fixture height field was refused")
}

/// The mirror the terrain drapes itself with: **saturated red, and every other
/// channel zero**.
///
/// Not a grey, and the reason is that every criterion here needs to tell a
/// terrain pixel from a building pixel as a *discrete identity* rather than by
/// a threshold. `fs_building` paints `BUILDING_ALBEDO`, which is near-neutral,
/// so a red drape makes the two unmistakable in one comparison
/// ([`is_prism_colour`]) instead of a tolerance nobody can defend.
const MIRROR_RGBA: [u8; 4] = [255, 0, 0, 255];

fn flat_mirror(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    pipelines: &VolumePipelines,
) -> PaneMirror {
    const EDGE: u32 = 32;
    let rgba: Vec<u8> = std::iter::repeat_n(MIRROR_RGBA, (EDGE * EDGE) as usize)
        .flatten()
        .collect();
    planted_mirror(device, queue, pipelines, [EDGE, EDGE], &rgba)
}

/// Whether a texel of the ground pass's colour attachment came from a prism
/// rather than from the drape.
///
/// The drape is [`MIRROR_RGBA`] through `lit`, which scales all three channels
/// by one number — so green and blue stay exactly zero however the light falls.
/// A prism's albedo is near-neutral, so its green is a real fraction of its
/// red. The test is therefore "did any light reach the green channel", which no
/// drape pixel can pass and no lit prism pixel can fail.
fn is_prism_colour(texel: [u8; 4]) -> bool {
    texel[3] > 0 && texel[1] > 8
}

/// The floor lanes this file's box is drawn with: the site at the Monaco
/// tile's own centre, with the mirror's texture coordinates running slowly
/// enough that the whole footprint lands well inside it.
fn floor_lanes() -> ([f32; 4], [f32; 4]) {
    const FOOTPRINT: f64 = 0.8;
    let half_deg = f64::from(BOX_KM[0] / 2.0) / f64::from(gpu_harness::DEGREE_BOX_KM);
    let half_north_deg = f64::from(BOX_KM[1] / 2.0) / f64::from(gpu_harness::DEGREE_BOX_KM);
    let u_per_degree = FOOTPRINT / (2.0 * half_deg);
    let v_per_mercator_y = -FOOTPRINT
        / (gpu_harness::mercator_y(SITE.0 + half_north_deg)
            - gpu_harness::mercator_y(SITE.0 - half_north_deg));
    (
        [0.5, 0.5, u_per_degree as f32, v_per_mercator_y as f32],
        [
            SITE.0 as f32,
            -BOX_KM[0] / 2.0,
            -BOX_KM[1] / 2.0,
            if MIRROR_FORMAT.is_srgb() { 0.0 } else { 1.0 },
        ],
    )
}

// ---------------------------------------------------------------------------
// The city.
// ---------------------------------------------------------------------------

/// The real `building` layer this suite extrudes, byte for byte the one
/// `squallar-buildings`' own tests read.
const REAL_BUILDING_TILE: &[u8] =
    include_bytes!("../../squallar-buildings/testdata/monaco-building-z14-8529-5974.mvt");

/// The address it came from.
const REAL_TILE: TileId = TileId {
    z: 14,
    x: 8529,
    y: 5974,
};

/// The site the box's kilometres are measured from: that tile's own centre.
const SITE: (f64, f64) = (43.731_414_013_768_99, 7.415_771_484_375);

/// The box the footprints are projected into — [`BOX_KM`] about [`SITE`].
fn frame() -> BoxFrame {
    BoxFrame {
        site: SITE,
        x_km: (f64::from(-BOX_KM[0] / 2.0), f64::from(BOX_KM[0] / 2.0)),
        y_km: (f64::from(-BOX_KM[1] / 2.0), f64::from(BOX_KM[1] / 2.0)),
    }
}

/// Every extrudable footprint of the real tile that lands inside [`frame`].
fn real_footprints() -> Vec<BuildingFootprint> {
    read_footprints(REAL_TILE, REAL_BUILDING_TILE, &frame())
        .expect("the committed Monaco tile no longer parses")
}

/// The whole city, through the shipped budget.
fn city() -> BuildingMesh {
    extrude(
        &real_footprints(),
        &PrismBudget::fit(PrismCeilings::DEFAULT),
    )
}

/// The `n` tallest of the real footprints, extruded.
///
/// **A subset, and it is the registration oracle's cost that decides it.** That
/// criterion casts a host ray against every triangle the GPU drew; the whole
/// city is tens of thousands of them and the sweep is over eleven cameras, so
/// the full set would put minutes of debug-build ray-triangle arithmetic in a
/// suite that is already `#[ignore]`d for needing a GPU. The other criteria all
/// run over the whole city, where the cost is the GPU's.
fn tallest(n: usize) -> BuildingMesh {
    let mut footprints = real_footprints();
    footprints.sort_by(|a, b| {
        b.height_m
            .partial_cmp(&a.height_m)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    footprints.truncate(n);
    extrude(&footprints, &PrismBudget::fit(PrismCeilings::DEFAULT))
}

/// A square footprint of `side_km` centred at `(x_km, y_km)`, `height_m` tall.
///
/// The two criteria about occlusion by terrain need a building at a chosen
/// place, which no real tile provides — Monaco's footprints are where Monaco's
/// footprints are. Everything else in this file is the real archive.
fn planted(x_km: f64, y_km: f64, side_km: f64, height_m: f64) -> BuildingFootprint {
    let h = side_km / 2.0;
    // Counter-clockwise in the east/north frame, which is `Ring`'s canonical
    // exterior winding.
    let points = vec![
        [x_km - h, y_km - h],
        [x_km + h, y_km - h],
        [x_km + h, y_km + h],
        [x_km - h, y_km + h],
    ];
    BuildingFootprint {
        rings: vec![Ring {
            points,
            exterior: true,
        }],
        base_m: 0.0,
        height_m,
        bbox: [x_km - h, y_km - h, x_km + h, y_km + h],
    }
}

// ---------------------------------------------------------------------------
// The frame.
// ---------------------------------------------------------------------------

/// One camera: `(yaw, pitch, distance, vertical exaggeration)`.
type Camera = (f32, f32, f32, f32);

/// **Every camera the criteria below are asserted over.**
///
/// `gpu_harness::ORBIT_CAMERAS`, unchanged and unfiltered: eleven, spanning
/// above the crest, between the crest and the box floor, and under the floor.
/// It lives there because three suites read it, and three lists that have to
/// agree is what this repository keeps removing.
const CAMERAS: [Camera; 11] = gpu_harness::ORBIT_CAMERAS;

fn view_at((yaw, pitch, distance, exaggeration): Camera) -> VolumeView {
    let camera =
        OrbitCamera::restore(yaw, pitch, distance, [0.0; 3], exaggeration).expect("finite camera");
    view_for(camera, BOX_KM, SIZE[0] as f32 / SIZE[1] as f32).expect("a view")
}

/// A uniform aimed by `view`, with the ground pass on and the city placed.
fn uniform(cells: [u32; 3], view: &VolumeView) -> VolumeUniform {
    let mut uniform = VolumeUniform::new(BOX_KM, cells);
    uniform.box_from_clip = view.box_from_clip;
    uniform.clip_from_box = view.clip_from_box;
    uniform.eye_in_box = view.eye_in_box;
    // Ambient only for the march, and a light under which every surface takes
    // exactly its own albedo: every criterion here reads a colour as a discrete
    // identity, and a directional term turns each of them into a number with a
    // tolerance. `volume_light.rs` is where the light itself is measured.
    uniform.ambient = 1.0;
    uniform.gradient_shading = false;
    uniform.set_light(gpu_harness::UNLIT);
    let (uv, geo) = floor_lanes();
    uniform.floor_uv = uv;
    uniform.floor_geo = geo;
    uniform.map_floor = true;
    uniform.aim_occluder(RIDGE_AMPLITUDE, HEIGHT_SCALE, 0.0);
    assert!(
        !uniform.map_floor,
        "`aim_occluder` left the lid on beside a mesh"
    );
    assert!(
        uniform.place_buildings(-BOX_KM[0] / 2.0, -BOX_KM[1] / 2.0),
        "`place_buildings` refused this box's own south-west corner"
    );
    uniform
}

/// A grid of nothing but air, so whatever reaches the screen came from a
/// surface.
fn empty_grid() -> ([u32; 3], Vec<u8>) {
    ([8, 8, 8], vec![0u8; 8 * 8 * 8])
}

/// A grid whose every cell carries one opaque index: a fog filling the box, so
/// a shortened march is a visibly different pixel.
fn filled_grid() -> ([u32; 3], Vec<u8>) {
    ([8, 8, 8], vec![200u8; 8 * 8 * 8])
}

/// One frame: the ground pass and then the march, in one encoder, in that
/// order, with the occluder and ground attachments read back beside the
/// offscreen.
struct Frame {
    offscreen: Vec<[u8; 4]>,
    occluder: Vec<[u8; 4]>,
    ground: Vec<[u8; 4]>,
}

impl Frame {
    /// Whether a prism covered this pixel: the ground pass wrote a hit AND the
    /// colour under it is a prism's rather than the drape's.
    fn prism_at(&self, at: usize) -> bool {
        self.occluder[at][3] > 128 && is_prism_colour(self.ground[at])
    }

    /// The decoded ray parameter at a pixel, in box units, or `None` where the
    /// ground pass wrote nothing.
    fn hit_t(&self, at: usize, t_scale: f32) -> Option<f32> {
        let texel = self.occluder[at];
        (texel[3] > 128).then(|| unpack24_bytes([texel[0], texel[1], texel[2]]) * t_scale)
    }
}

/// Whether a prism covered this pixel **and all four of its neighbours**.
///
/// The rim of a prism's footprint is where the rasteriser's coverage rule and
/// the march's own `slab_entry_exit` disagree, and where a wall seen edge-on
/// puts a `t` indistinguishable from the ground's. Every criterion that
/// compares one frame's surface against another's runs over the interior, which
/// removes that class by construction instead of by a tolerance nobody can
/// defend. The counts each criterion prints are what stop the restriction
/// swallowing the criterion.
fn interior_prism(frame: &Frame, at: usize) -> bool {
    let (width, height) = (SIZE[0] as usize, SIZE[1] as usize);
    let (x, y) = (at % width, at / width);
    if x == 0 || y == 0 || x + 1 == width || y + 1 == height {
        return false;
    }
    [at, at - 1, at + 1, at - width, at + width]
        .into_iter()
        .all(|neighbour| frame.prism_at(neighbour))
}

fn upload_prisms(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    pipelines: &VolumePipelines,
    mesh: &BuildingMesh,
) -> BuildingPrisms {
    pipelines
        .upload_buildings(device, queue, &mesh.positions, &mesh.normals, &mesh.indices)
        .expect("the fixture prism mesh was refused")
}

#[allow(clippy::too_many_arguments)]
fn render(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    pipelines: &VolumePipelines,
    cells: [u32; 3],
    indices: &[u8],
    uniform: &VolumeUniform,
    prisms: Option<&BuildingPrisms>,
) -> Frame {
    let mirror = flat_mirror(device, queue, pipelines);
    let field = heights(device, queue, pipelines);
    let volume = pipelines
        .upload_volume(
            device,
            queue,
            cells,
            indices,
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
        Some(&mirror),
        Some(&field),
        prisms,
    );
    pipelines.encode_raymarch_with_floor(&mut encoder, &target, &volume, Some(&mirror));
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

// ---------------------------------------------------------------------------
// The host oracle: the same triangles, in Rust.
// ---------------------------------------------------------------------------

/// One prism vertex's position in box space, as `vs_building` computes it.
///
/// **`standing` is what the flat-ground twin turns off.** With it, the vertex
/// is lifted by the terrain under its own footprint; without it, it is
/// extruded from `z = 0` — which is exactly the mutant this unit exists to
/// distinguish from a correct build, and which no flat fixture could.
fn box_position(km: [f32; 3], placement: [f32; 4], standing: bool) -> [f32; 3] {
    let uv = [
        km[0] * placement[0] + placement[2],
        km[1] * placement[1] + placement[3],
    ];
    let ground = if standing {
        ground_surface_at(
            [uv[0].clamp(0.0, 1.0), uv[1].clamp(0.0, 1.0)],
            [POSTS, POSTS],
            IDENTITY_GROUND_BOX,
            post_height,
        )
    } else {
        0.0
    };
    [uv[0], uv[1], ground + km[2] / BOX_KM[2]]
}

/// Every triangle of `mesh` in box space, through [`box_position`].
fn box_triangles(mesh: &BuildingMesh, placement: [f32; 4], standing: bool) -> Vec<[[f32; 3]; 3]> {
    let positions: Vec<[f32; 3]> = mesh
        .positions
        .iter()
        .map(|km| box_position(*km, placement, standing))
        .collect();
    mesh.indices
        .chunks_exact(3)
        .map(|t| {
            [
                positions[t[0] as usize],
                positions[t[1] as usize],
                positions[t[2] as usize],
            ]
        })
        .collect()
}

/// `volume.wgsl`'s `unproject`, to the lane: `box_from_clip` is column-major.
fn unproject(box_from_clip: [[f32; 4]; 4], ndc: [f32; 2], depth: f32) -> [f32; 3] {
    let mut out = [0.0f32; 4];
    let v = [ndc[0], ndc[1], depth, 1.0];
    for (column, weights) in box_from_clip.iter().enumerate() {
        for (row, slot) in out.iter_mut().enumerate() {
            *slot += weights[row] * v[column];
        }
    }
    [out[0] / out[3], out[1] / out[3], out[2] / out[3]]
}

/// The ray through the centre of pixel `(x, y)`, normalised — the same
/// direction `fs_raymarch` casts.
fn ray_at(view: &VolumeView, x: u32, y: u32) -> [f32; 3] {
    let ndc = [
        2.0 * ((x as f32 + 0.5) / SIZE[0] as f32) - 1.0,
        1.0 - 2.0 * ((y as f32 + 0.5) / SIZE[1] as f32),
    ];
    let far = unproject(view.box_from_clip, ndc, 1.0);
    let eye = view.eye_in_box;
    let d = [far[0] - eye[0], far[1] - eye[1], far[2] - eye[2]];
    let length = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
    [d[0] / length, d[1] / length, d[2] / length]
}

/// Moller-Trumbore. `None` for a miss or a hit behind the eye.
fn ray_triangle(eye: [f32; 3], dir: [f32; 3], tri: [[f32; 3]; 3]) -> Option<f32> {
    let sub = |a: [f32; 3], b: [f32; 3]| [a[0] - b[0], a[1] - b[1], a[2] - b[2]];
    let cross = |a: [f32; 3], b: [f32; 3]| {
        [
            a[1] * b[2] - a[2] * b[1],
            a[2] * b[0] - a[0] * b[2],
            a[0] * b[1] - a[1] * b[0],
        ]
    };
    let dot = |a: [f32; 3], b: [f32; 3]| a[0] * b[0] + a[1] * b[1] + a[2] * b[2];

    let e1 = sub(tri[1], tri[0]);
    let e2 = sub(tri[2], tri[0]);
    let p = cross(dir, e2);
    let det = dot(e1, p);
    if det.abs() < 1e-12 {
        return None;
    }
    let inv = 1.0 / det;
    let s = sub(eye, tri[0]);
    let u = dot(s, p) * inv;
    if !(-1e-6..=1.0 + 1e-6).contains(&u) {
        return None;
    }
    let q = cross(s, e1);
    let v = dot(dir, q) * inv;
    if v < -1e-6 || u + v > 1.0 + 1e-6 {
        return None;
    }
    let t = dot(e2, q) * inv;
    (t > 1e-6).then_some(t)
}

/// The nearest triangle this ray meets, or `None`.
fn nearest_hit(eye: [f32; 3], dir: [f32; 3], triangles: &[[[f32; 3]; 3]]) -> Option<f32> {
    triangles
        .iter()
        .filter_map(|tri| ray_triangle(eye, dir, *tri))
        .fold(None, |best: Option<f32>, t| {
            Some(best.map_or(t, |b| b.min(t)))
        })
}

// ---------------------------------------------------------------------------
// The criteria.
// ---------------------------------------------------------------------------

/// The two crates price a prism vertex at one number.
///
/// `squallar_buildings::budget`'s whole rung ladder — every VRAM figure, every
/// "how many buildings" claim — is arithmetic over `PRISM_VERTEX_BYTES`, and
/// the renderer's vertex layout is what actually decides it. Two constants that
/// have to agree, in two crates that cannot see each other in the normal graph,
/// is exactly the pair this repository keeps removing; it cannot be removed
/// here because the buildings crate may not link wgpu, so it is gated instead.
#[test]
fn the_vertex_stride_is_what_the_budget_prices() {
    assert_eq!(
        squallar_volumetric::raymarch::BUILDING_VERTEX_BYTES,
        squallar_buildings::PRISM_VERTEX_BYTES,
        "the renderer's vertex stride and the budget's price for a vertex have \
         parted company. Every VRAM figure in `squallar_buildings::budget` is \
         arithmetic over the budget's number, and the draw reads the \
         renderer's",
    );
    assert_eq!(
        squallar_volumetric::raymarch::BUILDING_INDEX_BYTES,
        squallar_buildings::PRISM_INDEX_BYTES,
        "the renderer's index width and the budget's price for an index have \
         parted company",
    );
}

/// The fixture city really is a city, and its footprints really do stand on
/// ground that varies under them.
///
/// **The non-triviality floor for every criterion in this file.** A tile that
/// stopped parsing, a box that stopped holding it, or a ridge flat under the
/// whole city would leave every assertion below true and meaningless — the
/// exact shape of the vacuity this plan has now found five times.
#[test]
fn the_fixture_city_is_real_and_the_ground_under_it_is_not_flat() {
    let footprints = real_footprints();
    assert!(
        footprints.len() >= 20,
        "only {} footprints of the committed Monaco tile land in this box; the \
         fixture is no longer a city",
        footprints.len(),
    );
    let ring_vertices: usize = footprints.iter().map(|f| f.ring_vertices()).sum();
    let mesh = city();
    assert!(
        mesh.kept >= 20 && !mesh.is_empty(),
        "the budget kept {} of {} footprints and the mesh has {} indices",
        mesh.kept,
        footprints.len(),
        mesh.indices.len(),
    );

    // The ground under the city, at each footprint's own first ring vertex.
    let placement = placement_lane();
    let (mut low, mut high) = (f32::MAX, f32::MIN);
    for footprint in &footprints {
        let p = footprint.rings[0].points[0];
        let z = box_position([p[0] as f32, p[1] as f32, 0.0], placement, true)[2];
        low = low.min(z);
        high = high.max(z);
    }
    let spread = high - low;
    // The tallest building in this tile, as a fraction of the box.
    let tallest_km = footprints.iter().map(|f| f.height_m).fold(0.0f64, f64::max) / 1000.0;
    let tallest_box_z = tallest_km as f32 / BOX_KM[2];
    println!(
        "fixture city: {} footprints, {ring_vertices} ring vertices, {} mesh vertices, \
         {} triangles; ground under them spans {spread:.4} box z ({:.1} m); tallest \
         building {tallest_box_z:.4} box z ({:.1} m)",
        footprints.len(),
        mesh.positions.len(),
        mesh.indices.len() / 3,
        spread * BOX_KM[2] * 1000.0,
        tallest_km * 1000.0,
    );
    assert!(
        spread > tallest_box_z,
        "the ground under this city spans {spread} of box z and its tallest \
         building is {tallest_box_z}. The relief has to be the LARGER of the \
         two or a prism standing on the terrain and a prism extruded from \
         z = 0 are the same picture to within a building's own height, which \
         is the flat-ground fixture trap this file is built against",
    );
}

/// The placement lane this file's box implies, read off the uniform rather than
/// restated — so the oracle and the shader cannot be given two different ones.
fn placement_lane() -> [f32; 4] {
    let mut uniform = VolumeUniform::new(BOX_KM, [8, 8, 8]);
    assert!(uniform.place_buildings(-BOX_KM[0] / 2.0, -BOX_KM[1] / 2.0));
    uniform.building_box
}

/// **The registration oracle, and the flat-ground twin that must miss.**
///
/// At every camera, at every pixel a prism drew, the decoded occluder `t` is
/// compared against a host ray-cast into the very triangles the GPU was handed
/// — with each vertex lifted by `raymarch::ground_surface_at`, the Rust mirror
/// of the shader's own surface lookup. Agreement inside a fraction of a pixel
/// proves the camera, the placement lane, the terrain lookup, the packing and
/// the decode together.
///
/// **The half that makes it mean anything** is the same cast with the terrain
/// forced to zero. A build that extruded from the box floor would satisfy the
/// first half against a flat fixture and this one against nothing; here it is
/// asserted to miss by orders of magnitude, and the two figures are printed.
#[test]
#[ignore = "needs a real wgpu adapter; see the module doc"]
fn the_prisms_stand_on_the_terrain_and_not_on_the_box_floor() {
    let _guard = gpu_lock();
    let (device, queue) = device();
    let pipelines = VolumePipelines::new(&device, attachments(wgpu::TextureFormat::Bgra8Unorm));
    pipelines.upload_quad(&queue);

    let mesh = tallest(6);
    let prisms = upload_prisms(&device, &queue, &pipelines, &mesh);
    let (cells, indices) = empty_grid();
    let placement = placement_lane();
    let standing = box_triangles(&mesh, placement, true);
    let flat = box_triangles(&mesh, placement, false);

    let mut cameras_with_pixels = 0usize;
    let mut discriminating_total = 0usize;
    for camera in CAMERAS {
        let view = view_at(camera);
        let uniform = uniform(cells, &view);
        let frame = render(
            &device,
            &queue,
            &pipelines,
            cells,
            &indices,
            &uniform,
            Some(&prisms),
        );

        // Every pixel a prism drew, then a bounded sample of them: the cast is
        // linear in the triangle count and this runs in a debug build.
        let drawn: Vec<usize> = (0..(SIZE[0] * SIZE[1]) as usize)
            .filter(|at| frame.prism_at(*at))
            .collect();
        if drawn.len() < MIN_PRISM_PIXELS {
            // Legal: `ORBIT_CAMERAS` includes cameras under the box floor, and
            // the terrain can hide the whole city from one — straight up at the
            // pitch clamp leaves exactly one stray rim pixel of it. The
            // sweep-wide assertions below are what stop every camera answering
            // this way.
            println!(
                "camera {camera:?}: {} prism pixels, too few to measure",
                drawn.len()
            );
            continue;
        }
        cameras_with_pixels += 1;

        let step = (drawn.len() / SAMPLED_PIXELS).max(1);
        let mut standing_worst = 0.0f32;
        // **Agreements, not a worst case.** A flat-ground mesh mostly stands
        // somewhere the ray does not go at all, so its cast MISSES — and a
        // `max` over the hits it did make reads 0.0 when it made none, which is
        // the initial value and passes nothing. What has to be zero is the
        // number of sampled pixels a flat build would have explained.
        let mut flat_agreements = 0usize;
        let mut discriminating = 0usize;
        let mut flat_worst = 0.0f32;
        let mut compared = 0usize;
        for &at in drawn.iter().step_by(step) {
            let (x, y) = ((at as u32) % SIZE[0], (at as u32) / SIZE[0]);
            let direction = ray_at(&view, x, y);
            let Some(gpu_t) = frame.hit_t(at, uniform.occluder_t_scale) else {
                continue;
            };
            let Some(host_t) = nearest_hit(view.eye_in_box, direction, &standing) else {
                // The rasteriser's coverage rule and a host cast through the
                // pixel CENTRE disagree at a silhouette's outermost pixel; that
                // is B1's own finding and it is a rim property, not a
                // registration one. Skipped rather than tolerated, and the
                // count below is what stops the skip swallowing the criterion.
                continue;
            };
            compared += 1;
            standing_worst = standing_worst.max((gpu_t - host_t).abs());
            let Some(flat_t) = nearest_hit(view.eye_in_box, direction, &flat) else {
                // The flat mesh is not even in this ray's way, which is the
                // strongest form of "a build that ignored the terrain would not
                // have drawn this pixel".
                discriminating += 1;
                continue;
            };
            // **A pixel can only discriminate where the two meshes differ.**
            // The fixture ridge is a Gaussian, so near the box edges it is
            // essentially zero and a building standing there is in the same
            // place either way — those pixels are not evidence for the
            // criterion and must not be counted as evidence against it. What
            // the sweep needs is that the pixels which DO separate the two
            // hypotheses all fall on the standing one.
            let separation = (host_t - flat_t).abs();
            if separation <= REGISTRATION_TOLERANCE * 10.0 {
                continue;
            }
            discriminating += 1;
            let miss = (gpu_t - flat_t).abs();
            flat_worst = flat_worst.max(miss);
            if miss <= REGISTRATION_TOLERANCE {
                flat_agreements += 1;
            }
        }
        assert!(
            compared >= 4,
            "camera {camera:?}: only {compared} of {} prism pixels could be \
             cast into the host mesh at all. A criterion that skips its way to \
             green is the vacuity this file is written against",
            drawn.len(),
        );
        discriminating_total += discriminating;
        println!(
            "camera {camera:?}: {} prism pixels, {compared} cast; standing worst \
             {standing_worst:.5} box z. {discriminating} of them separate standing \
             from flat ground at all; a flat mesh missed those by up to \
             {flat_worst:.5} and explained {flat_agreements}",
            drawn.len(),
        );
        assert!(
            standing_worst <= REGISTRATION_TOLERANCE,
            "camera {camera:?}: the GPU's decoded `t` differs from a host cast \
             into the same triangles by {standing_worst} box units, past the \
             {REGISTRATION_TOLERANCE} tolerance. The prisms are not where the \
             shader's own surface lookup puts them",
        );
        assert_eq!(
            flat_agreements, 0,
            "camera {camera:?}: a mesh extruded from the BOX FLOOR explains \
             {flat_agreements} of {compared} sampled pixels to inside this \
             criterion's own tolerance. Either the fixture has drifted onto \
             flat ground, or the build under test is ignoring the terrain and \
             the half above is matching it for the wrong reason",
        );
    }
    assert!(
        cameras_with_pixels >= 6,
        "only {cameras_with_pixels} of {} cameras drew a prism at all",
        CAMERAS.len(),
    );
    // **The non-triviality floor of the whole criterion.** Every assertion
    // above is about pixels that separate a city standing on the terrain from
    // one extruded off the box floor; if the sweep found none of them, the
    // criterion certified nothing and would go on certifying nothing forever.
    assert!(
        discriminating_total >= 40,
        "the whole sweep found only {discriminating_total} pixels at which \
         standing on the terrain and extruding from the box floor are \
         different pictures. The fixture has drifted flat",
    );
    println!(
        "{discriminating_total} pixels across the sweep separate standing on the \
         terrain from extruding off the box floor, and every one of them fell on \
         standing"
    );
}

/// Prism pixels sampled per camera for the host cast.
const SAMPLED_PIXELS: usize = 24;

/// How many prism pixels a camera must show before it is measured at all.
///
/// **Not a way of dropping inconvenient cameras**, and the sweep-wide floors
/// are what keep it honest: at the pitch clamp, straight up from under the box
/// floor, the whole city reduces to a single stray silhouette pixel, and a
/// per-pixel registration figure taken from one rim pixel is noise reported as
/// a measurement.
const MIN_PRISM_PIXELS: usize = 8;

/// How far the GPU's decoded `t` may sit from the host cast, in box units.
///
/// **A fraction of the on-screen pixel and not a chosen small number.** The box
/// is one unit across and the offscreen is [`SIZE`] wide, so one pixel spans
/// about 1/320 = 3.1e-3 box units at the near face; this is a fifth of that.
/// What is left inside it is the packing's own quantum (1/2^24 of `t_scale`,
/// which is under 1e-6 here) and the rasteriser's interpolation against a host
/// cast through the pixel centre.
const REGISTRATION_TOLERANCE: f32 = 6.0e-4;

/// **A prism shortens the march that would have run past it.**
///
/// Two claims, and the file asserts both because either alone reads as the
/// other: the *mechanism*, that the ray parameter the occluder carries moves
/// nearer where a prism drew, and the *picture*, that a frame with a fog in the
/// box composites differently at those pixels.
///
/// The non-triviality half is the direction: a clip can only ever remove
/// volume, so no prism pixel may come back carrying MORE of the fog. Measured
/// on premultiplied luminance with the surface held constant across the pair —
/// which it is not, since a prism paints its own albedo where the drape would
/// have been, so the comparison is drawn against the same pair rendered through
/// an EMPTY grid and the fog's own contribution is what moves.
#[test]
#[ignore = "needs a real wgpu adapter; see the module doc"]
fn a_prism_shortens_the_march_it_stands_in_front_of() {
    let _guard = gpu_lock();
    let (device, queue) = device();
    let pipelines = VolumePipelines::new(&device, attachments(wgpu::TextureFormat::Bgra8Unorm));
    pipelines.upload_quad(&queue);

    let mesh = city();
    let prisms = upload_prisms(&device, &queue, &pipelines, &mesh);
    let (air_cells, air) = empty_grid();
    let (fog_cells, fog) = filled_grid();

    let mut cameras_measured = 0usize;
    for camera in CAMERAS {
        let view = view_at(camera);
        let uniform = uniform(air_cells, &view);

        let with = render(
            &device,
            &queue,
            &pipelines,
            air_cells,
            &air,
            &uniform,
            Some(&prisms),
        );
        let without = render(&device, &queue, &pipelines, air_cells, &air, &uniform, None);
        let fog_with = render(
            &device,
            &queue,
            &pipelines,
            fog_cells,
            &fog,
            &uniform,
            Some(&prisms),
        );
        let fog_without = render(&device, &queue, &pipelines, fog_cells, &fog, &uniform, None);

        let mut nearer = 0usize;
        let mut farther = 0usize;
        let mut brighter_fog = 0usize;
        let mut moved = 0usize;
        for at in 0..(SIZE[0] * SIZE[1]) as usize {
            // **The interior of a prism's footprint, never its rim.** The
            // rasteriser's coverage rule and the march's own `slab_entry_exit`
            // disagree about a silhouette's outermost pixel — B1 measured that
            // as one pixel of 4113 and asserted inside the silhouette for the
            // same reason. Here a rim pixel can carry a wall seen edge-on,
            // whose `t` is the ground's to within the packing, so it reads as
            // "farther" for no reason a mechanism could name. Requiring all
            // four neighbours to be prisms too removes the rim by construction
            // rather than by a tolerance.
            if !interior_prism(&with, at) {
                continue;
            }
            let Some(prism_t) = with.hit_t(at, uniform.occluder_t_scale) else {
                continue;
            };
            match without.hit_t(at, uniform.occluder_t_scale) {
                // The terrain drew here too, so the prism is either in front of
                // it or the pixel is a rim disagreement.
                Some(ground_t) => {
                    if prism_t < ground_t - PACKING_SLACK {
                        nearer += 1;
                    } else if prism_t > ground_t + PACKING_SLACK {
                        farther += 1;
                    }
                }
                // The mesh did not cover this pixel at all — a prism standing
                // where the terrain's silhouette has already ended. Nearer than
                // nothing, and it cannot be farther.
                None => nearer += 1,
            }
            // The fog's own contribution, isolated by subtracting the same
            // frame drawn through an empty grid: that is what holds the SURFACE
            // constant across a pair whose surface genuinely differs.
            let fog_gain_with = luminance(fog_with.offscreen[at]) - luminance(with.offscreen[at]);
            let fog_gain_without =
                luminance(fog_without.offscreen[at]) - luminance(without.offscreen[at]);
            if fog_gain_with > fog_gain_without + FOG_SLACK {
                brighter_fog += 1;
            } else if fog_gain_with < fog_gain_without - FOG_SLACK {
                moved += 1;
            }
        }
        if nearer + farther == 0 {
            println!("camera {camera:?}: no prism pixel to compare");
            continue;
        }
        cameras_measured += 1;
        println!(
            "camera {camera:?}: {nearer} prism pixels nearer than the ground behind them, \
             {farther} farther; the fog fell back at {moved} and rose at {brighter_fog}",
        );
        assert_eq!(
            farther, 0,
            "camera {camera:?}: {farther} prism pixels decode to a `t` FARTHER \
             than the terrain the same pixel showed without them. A prism the \
             ray reaches after the ground is a prism underground, and the \
             march would then be clipped at the wrong surface",
        );
        assert!(
            nearer > 0,
            "camera {camera:?}: no prism pixel is nearer than what was behind \
             it, so nothing here could occlude anything",
        );
        assert_eq!(
            brighter_fog, 0,
            "camera {camera:?}: {brighter_fog} prism pixels carry MORE of the \
             fog with the buildings in than without them. A clip can only ever \
             remove volume",
        );
        assert!(
            moved > 0,
            "camera {camera:?}: {nearer} prism pixels are nearer than the \
             ground behind them and not one of them lost any fog. The occluder \
             is being written and not read, which is a frame where buildings \
             are painted over a volume they do not occlude",
        );
    }
    assert!(
        cameras_measured >= 6,
        "only {cameras_measured} of {} cameras had a prism pixel to compare",
        CAMERAS.len(),
    );
}

/// Two codes of the 24-bit packing, in box units at this file's `t_scale`.
///
/// The comparison above is between two decoded `t`s, so the floor under it is
/// the packing's own quantum rather than anything geometric. `t_scale` here is
/// at most 1.05 times the cube's diagonal, so a code is under 4e-7 box units
/// and this is an order past two of them.
const PACKING_SLACK: f32 = 1.0e-5;

/// How much premultiplied luminance the fog's contribution may move before it
/// counts as having moved. One code of an eight-bit channel.
const FOG_SLACK: f32 = 1.0 / 255.0;

/// Premultiplied luminance of an offscreen texel, 0 to 1.
fn luminance(texel: [u8; 4]) -> f32 {
    (0.2126 * f32::from(texel[0]) + 0.7152 * f32::from(texel[1]) + 0.0722 * f32::from(texel[2]))
        / 255.0
}

/// **A prism behind the ridge is hidden by it, and its twin in front is not.**
///
/// The other direction of the done-when, and the one that no colour comparison
/// can fake: two identical towers, one on each side of the ridge, at a camera
/// low enough that the crest stands between the eye and the far one. The near
/// one must paint pixels and the far one must paint none.
///
/// **The twin IS the non-triviality half.** A build that drew no prisms at all,
/// or clipped them all away, would satisfy "the far one is absent" perfectly.
#[test]
#[ignore = "needs a real wgpu adapter; see the module doc"]
fn a_prism_behind_the_ridge_is_hidden_by_it() {
    let _guard = gpu_lock();
    let (device, queue) = device();
    let pipelines = VolumePipelines::new(&device, attachments(wgpu::TextureFormat::Bgra8Unorm));
    pipelines.upload_quad(&queue);

    // Both towers sit on the flat ground away from the ridge crest, at the same
    // distance from it, and are the same size — so the ONLY thing that differs
    // is which side of the crest they are on.
    const OFFSET_KM: f64 = 0.7;
    const SIDE_KM: f64 = 0.12;
    const TOWER_M: f64 = 30.0;

    // A shallow pitch is what puts the crest between the eye and the far tower:
    // the ridge climbs 100 m over 2 km, which at this camera's 3x exaggeration
    // is a 16.7 degree slope, so anything under that leaves the far side of it
    // hidden. The standoff is what frames both towers at once — at 1.2 the near
    // one falls outside the frustum entirely, which is a fixture that reads as
    // occlusion and is framing.
    let camera: Camera = (270.0, 4.0, 2.4, 3.0);
    let view = view_at(camera);
    // **Which side the eye is on is measured, not assumed.** A yaw is three
    // rotations away from a box-space eye position, and reading it wrong is how
    // this fixture first ran: it named the visible tower "far" and asserted the
    // hidden one was the control. `box_x_km` runs east and the ridge peaks at
    // the box's middle, so the near tower is simply the one on the eye's side
    // of it.
    let east_side = view.eye_in_box[0] > 0.5;
    let near_km = if east_side { OFFSET_KM } else { -OFFSET_KM };
    let far_km = -near_km;

    let budget = PrismBudget::fit(PrismCeilings::DEFAULT);
    let near = extrude(&[planted(near_km, 0.0, SIDE_KM, TOWER_M)], &budget);
    let far = extrude(&[planted(far_km, 0.0, SIDE_KM, TOWER_M)], &budget);
    assert!(
        !near.is_empty() && !far.is_empty(),
        "a planted tower tessellated to nothing"
    );

    let near_prisms = upload_prisms(&device, &queue, &pipelines, &near);
    let far_prisms = upload_prisms(&device, &queue, &pipelines, &far);
    let (cells, indices) = empty_grid();

    let uniform = uniform(cells, &view);
    let near_frame = render(
        &device,
        &queue,
        &pipelines,
        cells,
        &indices,
        &uniform,
        Some(&near_prisms),
    );
    let far_frame = render(
        &device,
        &queue,
        &pipelines,
        cells,
        &indices,
        &uniform,
        Some(&far_prisms),
    );

    let near_pixels = (0..(SIZE[0] * SIZE[1]) as usize)
        .filter(|at| near_frame.prism_at(*at))
        .count();
    let far_pixels = (0..(SIZE[0] * SIZE[1]) as usize)
        .filter(|at| far_frame.prism_at(*at))
        .count();
    println!(
        "camera {camera:?}: eye at box x {:.3}, so the near tower is at \
         {near_km} km and the far one at {far_km}; the near tower paints \
         {near_pixels} pixels, the one behind the ridge paints {far_pixels}",
        view.eye_in_box[0],
    );
    assert!(
        near_pixels > 40,
        "the tower in FRONT of the ridge paints only {near_pixels} pixels, so \
         this camera cannot see a tower at all and the criterion below is \
         vacuous",
    );
    assert_eq!(
        far_pixels, 0,
        "the tower BEHIND the ridge paints {far_pixels} pixels. It is drawn \
         into the same pass and depth buffer as the terrain, so a pixel of it \
         surviving means the depth test is not settling the two",
    );
}

/// **What `hide_3d` costs at the glass**, and exactly what this file leans on
/// `squallar-buildings` for.
///
/// The done-when says `hide_3d` is respected, and there are two halves to that.
/// The parse half — a feature carrying the key never becomes a footprint — is
/// `squallar_buildings::footprint`'s, pinned by
/// `the_hide_3d_key_is_honoured_though_no_shipped_archive_carries_it` against a
/// synthetic tile, because **no archive this workspace ships carries the key**.
/// It is not re-asserted here and could not be: the fixture encoder that builds
/// such a tile is `pub(crate)` in that crate, and a third copy of an MVT writer
/// in this file would be a second belief about the wire, not a second check.
///
/// The half this unit owns is the one below: **the renderer draws exactly the
/// footprints the parse kept, and nothing else.** A footprint dropped before
/// `extrude` reaches no pixel, and the pixels it would have reached go back to
/// showing what is behind it — measured both ways. Compose the two and a hidden
/// feature is absent from the picture; hold them apart and it is honest about
/// which crate proves which.
#[test]
#[ignore = "needs a real wgpu adapter; see the module doc"]
fn a_footprint_the_parse_dropped_reaches_no_pixel() {
    let _guard = gpu_lock();
    let (device, queue) = device();
    let pipelines = VolumePipelines::new(&device, attachments(wgpu::TextureFormat::Bgra8Unorm));
    pipelines.upload_quad(&queue);

    let budget = PrismBudget::fit(PrismCeilings::DEFAULT);
    let mut footprints = real_footprints();
    footprints.sort_by(|a, b| {
        b.height_m
            .partial_cmp(&a.height_m)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    footprints.truncate(6);

    // The one the parse is imagined to have dropped: the tallest, so its
    // absence is the largest thing that can be absent.
    let dropped = footprints[0].clone();
    let kept: Vec<BuildingFootprint> = footprints[1..].to_vec();

    let all = extrude(&footprints, &budget);
    let without = extrude(&kept, &budget);
    assert_eq!(
        all.kept as usize,
        footprints.len(),
        "the budget shed a footprint, so the pair below differs by more than \
         the one this test drops"
    );
    let alone = extrude(&[dropped], &budget);

    let all_prisms = upload_prisms(&device, &queue, &pipelines, &all);
    let without_prisms = upload_prisms(&device, &queue, &pipelines, &without);
    let alone_prisms = upload_prisms(&device, &queue, &pipelines, &alone);
    let (cells, indices) = empty_grid();

    let mut cameras_measured = 0usize;
    for camera in CAMERAS {
        let view = view_at(camera);
        let uniform = uniform(cells, &view);
        let all_frame = render(
            &device,
            &queue,
            &pipelines,
            cells,
            &indices,
            &uniform,
            Some(&all_prisms),
        );
        let without_frame = render(
            &device,
            &queue,
            &pipelines,
            cells,
            &indices,
            &uniform,
            Some(&without_prisms),
        );
        let alone_frame = render(
            &device,
            &queue,
            &pipelines,
            cells,
            &indices,
            &uniform,
            Some(&alone_prisms),
        );

        // Where the dropped footprint would have drawn AND nothing else stands
        // in front of it. Restricting to that set is what makes the assertion
        // about the footprint rather than about the rest of the city.
        //
        // **Judged on `t` and not on colour**, which is the correction to a
        // first version of this criterion that compared offscreen bytes and
        // read 4 of 21 pixels as survivors of a correct build. Every prism in
        // the scene carries the same albedo and this file's light is flat, so a
        // pixel where the dropped building stood in FRONT of another building
        // is byte-identical once it is removed — the picture really is the
        // same there, and the thing that moved is how far away it is. What must
        // be true is that the surface behind is farther, or that there is no
        // surface there at all.
        let mut its_own = 0usize;
        let mut survived = 0usize;
        let mut unchanged_elsewhere = 0usize;
        let mut changed_elsewhere = 0usize;
        for at in 0..(SIZE[0] * SIZE[1]) as usize {
            let alone_here = interior_prism(&alone_frame, at);
            let all_t = all_frame.hit_t(at, uniform.occluder_t_scale);
            let alone_t = alone_frame.hit_t(at, uniform.occluder_t_scale);
            let without_t = without_frame.hit_t(at, uniform.occluder_t_scale);
            // The dropped footprint owns this pixel when it is the nearest
            // thing there in the full frame — which is exactly when the full
            // frame and the footprint-alone frame agree about the distance.
            let its = alone_here
                && matches!((all_t, alone_t), (Some(a), Some(b)) if (a - b).abs() <= PACKING_SLACK);
            if its {
                its_own += 1;
                let behind_is_farther = match (all_t, without_t) {
                    (Some(a), Some(b)) => b > a + PACKING_SLACK,
                    // Nothing at all behind it: the strongest form of absent.
                    (Some(_), None) => true,
                    _ => false,
                };
                if !behind_is_farther {
                    survived += 1;
                }
            } else if !alone_frame.prism_at(at) {
                // **Its own rim is neither**, and that is the point of testing
                // `prism_at` here where `alone_here` above tested the interior:
                // a pixel the dropped footprint half-covered legitimately moves
                // when it goes, and calling that "a pixel it never covered"
                // would make this half of the criterion assert a rim rule
                // instead of a mesh rule.
                if without_frame.offscreen[at] == all_frame.offscreen[at] {
                    unchanged_elsewhere += 1;
                } else {
                    changed_elsewhere += 1;
                }
            }
        }
        if its_own == 0 {
            println!("camera {camera:?}: the dropped footprint is not visible here");
            continue;
        }
        cameras_measured += 1;
        println!(
            "camera {camera:?}: the dropped footprint owns {its_own} pixels, at {survived} of \
             which nothing moved away; elsewhere {unchanged_elsewhere} unchanged, \
             {changed_elsewhere} changed",
        );
        assert_eq!(
            survived, 0,
            "camera {camera:?}: at {survived} of the dropped footprint's own \
             {its_own} pixels the nearest surface did not move away when it was \
             removed, so the renderer is drawing a building the parse did not \
             hand it",
        );
        assert_eq!(
            changed_elsewhere, 0,
            "camera {camera:?}: removing one footprint moved {changed_elsewhere} \
             pixels it never covered. The draw is not a function of the mesh it \
             was given",
        );
    }
    assert!(
        cameras_measured >= 4,
        "only {cameras_measured} of {} cameras could see the dropped footprint \
         at all",
        CAMERAS.len(),
    );
}

/// **A prism's walls are lit by the face they are, and the normals that say so
/// come out of the vertex buffer.**
///
/// Every other criterion in this file renders under [`gpu_harness::UNLIT`] — a
/// unit-white sky and no beam at all — so that a colour can be read as a
/// discrete identity rather than as a number with a tolerance. That choice has
/// a cost, and it was measured rather than assumed: a mutant that pointed the
/// normal attribute at the position's own bytes, so every prism vertex carried
/// its coordinates as its normal, **survived all five of them**. Under a light
/// with no beam, `lit` is `albedo * (0 * response + 1)` and the normal cancels
/// out of the whole file.
///
/// So this one criterion turns the beam on. It asserts nothing about how bright
/// a wall is — that is `volume_light.rs`'s subject — only that the walls facing
/// the light and the walls facing away are **not the same colour**, which is
/// false of a build whose normals are garbage and false of one that dropped the
/// second vertex attribute.
///
/// The non-triviality half is the same scene under [`gpu_harness::UNLIT`],
/// where the spread must collapse: without it, a criterion that "the colours
/// differ" would pass on a drape that happened to vary.
#[test]
#[ignore = "needs a real wgpu adapter; see the module doc"]
fn a_prisms_walls_are_shaded_by_the_faces_they_are() {
    let _guard = gpu_lock();
    let (device, queue) = device();
    let pipelines = VolumePipelines::new(&device, attachments(wgpu::TextureFormat::Bgra8Unorm));
    pipelines.upload_quad(&queue);

    let budget = PrismBudget::fit(PrismCeilings::DEFAULT);
    // One square tower at the box centre, so all four walls are in one scene
    // and at least two of them face the camera at any yaw.
    let tower = extrude(&[planted(0.0, 0.0, 0.4, 120.0)], &budget);
    let prisms = upload_prisms(&device, &queue, &pipelines, &tower);
    let (cells, indices) = empty_grid();

    let camera: Camera = (215.0, 20.0, 1.4, 3.0);
    let view = view_at(camera);

    let mut lit_spread = 0u16;
    let mut flat_spread = 0u16;
    let mut lit_high = 0u16;
    let mut flat_high = 0u16;
    for (name, light) in [
        (
            "the shipped headlight",
            squallar_volumetric::uniform::HEADLIGHT,
        ),
        ("no beam at all", gpu_harness::UNLIT),
    ] {
        let mut uniform = uniform(cells, &view);
        uniform.set_light(light);
        let frame = render(
            &device,
            &queue,
            &pipelines,
            cells,
            &indices,
            &uniform,
            Some(&prisms),
        );
        let (mut low, mut high) = (255u16, 0u16);
        let mut pixels = 0usize;
        for at in 0..(SIZE[0] * SIZE[1]) as usize {
            if !interior_prism(&frame, at) {
                continue;
            }
            pixels += 1;
            let green = u16::from(frame.ground[at][1]);
            low = low.min(green);
            high = high.max(green);
        }
        assert!(
            pixels > 100,
            "under {name} the tower paints only {pixels} interior pixels, so \
             there is no wall here to be shaded"
        );
        let spread = high - low;
        println!(
            "camera {camera:?} under {name}: {pixels} interior prism pixels, \
             green {low}..{high}, spread {spread}",
        );
        if light.beam[0] > 0.0 {
            lit_spread = spread;
            lit_high = high;
        } else {
            flat_spread = spread;
            flat_high = high;
        }
    }

    assert_eq!(
        flat_spread, 0,
        "with no beam at all the prisms still show a spread of {flat_spread} \
         codes across their faces, so something other than the light is \
         varying and the comparison below is not about normals",
    );
    assert!(
        lit_spread > 40,
        "under a real beam the tower's faces span only {lit_spread} codes of \
         green, against 0 with the beam off. A prism's walls face four \
         different ways and must take four different amounts of light",
    );
    // **The roof pins the normals to a value, not just to a spread**, and that
    // is the correction to a first version of this criterion that asserted only
    // that the faces differ. A mutant pointing the normal attribute at the
    // position's own bytes gives every vertex a different garbage normal, so
    // the faces still differ and the spread test passed it.
    //
    // What a correct build has and that one cannot is a surface whose
    // directional response is exactly **one**: `ground_response` of straight up
    // is `L.z / max(L.z, floor)`, which is 1 under any readable light, and the
    // roof cap's normal is exactly `(0, 0, 1)`. Under `UNLIT` every surface
    // takes its own albedo, so the brightest prism texel is the albedo itself —
    // and under the headlight the roof must reach that same value. A build with
    // no `(0, 0, 1)` anywhere in its normals cannot.
    assert_eq!(
        lit_high, flat_high,
        "the brightest prism texel is {lit_high} under the headlight and \
         {flat_high} under a light that cancels the response entirely. The roof \
         cap's normal is exactly straight up, whose response is exactly one, so \
         the two must be the same code — a mesh whose normals are its positions \
         has no such face and reads exactly this way",
    );
}

/// `upload_buildings` refuses a mesh whose indices do not address its own
/// vertices, rather than handing the driver an out-of-bounds fetch.
///
/// **Not merely defensive.** `squallar_buildings::BuildingMesh::is_coherent`
/// makes the same check at the wire seam, but the renderer is a second boundary
/// and its caller need not have come off a wire at all; an index past the end
/// of the vertex buffer is a driver's business rather than something this side
/// can catch. The non-triviality half is the same mesh with the index put back.
#[test]
#[ignore = "needs a real wgpu adapter; see the module doc"]
fn an_incoherent_mesh_is_refused_rather_than_uploaded() {
    let _guard = gpu_lock();
    let (device, queue) = device();
    let pipelines = VolumePipelines::new(&device, attachments(wgpu::TextureFormat::Bgra8Unorm));

    let mesh = extrude(
        &[planted(0.0, 0.0, 0.2, 80.0)],
        &PrismBudget::fit(PrismCeilings::DEFAULT),
    );
    assert!(
        pipelines
            .upload_buildings(
                &device,
                &queue,
                &mesh.positions,
                &mesh.normals,
                &mesh.indices
            )
            .is_some(),
        "the coherent fixture mesh was refused, so the refusal below proves \
         nothing",
    );

    let mut doctored = mesh.indices.clone();
    doctored[0] = mesh.positions.len() as u32;
    assert!(
        pipelines
            .upload_buildings(&device, &queue, &mesh.positions, &mesh.normals, &doctored)
            .is_none(),
        "a mesh whose first index reaches one past its {} vertices was \
         uploaded. That index is an out-of-bounds fetch on the GPU",
        mesh.positions.len(),
    );
    assert!(
        pipelines
            .upload_buildings(
                &device,
                &queue,
                &mesh.positions,
                &mesh.normals[1..],
                &mesh.indices
            )
            .is_none(),
        "a mesh with one fewer normal than positions was uploaded",
    );
}

/// A prism whose footprint lies outside the drawn box writes no fragment, and
/// the same prism inside it does.
///
/// `fs_building`'s clip is not tidiness: `VolumeUniform::t_scale_for` bounds the
/// occluder's packing by the farthest unit-cube CORNER, and a fragment written
/// past the cube would saturate the packing and decode to `t_scale` — past
/// everything — so the march would stop clipping against the very building it
/// was painting.
#[test]
#[ignore = "needs a real wgpu adapter; see the module doc"]
fn a_prism_outside_the_drawn_box_writes_nothing() {
    let _guard = gpu_lock();
    let (device, queue) = device();
    let pipelines = VolumePipelines::new(&device, attachments(wgpu::TextureFormat::Bgra8Unorm));
    pipelines.upload_quad(&queue);

    let budget = PrismBudget::fit(PrismCeilings::DEFAULT);
    // **Just outside the box's east edge, not far outside it**, and that is the
    // correction to a first version of this criterion that planted the tower at
    // 4 km. At that distance the tower is outside the camera's FRUSTUM as well
    // as outside the box, so the criterion passed on a build with the clip
    // deleted entirely: measured, `fs_building` discarding nothing left this
    // test green. A tower half a box outside the edge is on screen, which is
    // what makes the clip the only thing that can remove it.
    let outside = extrude(&[planted(1.3, 0.0, 0.2, 80.0)], &budget);
    let inside = extrude(&[planted(0.0, 0.0, 0.2, 80.0)], &budget);
    let outside_prisms = upload_prisms(&device, &queue, &pipelines, &outside);
    let inside_prisms = upload_prisms(&device, &queue, &pipelines, &inside);
    let (cells, indices) = empty_grid();

    // Overhead, so a tower beyond the east edge is well inside the frame: at a
    // grazing camera the box's own edge is near the horizon and "outside the
    // box" and "off the screen" become the same thing again.
    let camera: Camera = (0.0, 85.0, 1.5, 1.0);
    let view = view_at(camera);
    let uniform = uniform(cells, &view);
    let out_frame = render(
        &device,
        &queue,
        &pipelines,
        cells,
        &indices,
        &uniform,
        Some(&outside_prisms),
    );
    let in_frame = render(
        &device,
        &queue,
        &pipelines,
        cells,
        &indices,
        &uniform,
        Some(&inside_prisms),
    );

    let out_pixels = (0..(SIZE[0] * SIZE[1]) as usize)
        .filter(|at| out_frame.prism_at(*at))
        .count();
    let in_pixels = (0..(SIZE[0] * SIZE[1]) as usize)
        .filter(|at| in_frame.prism_at(*at))
        .count();
    println!(
        "camera {camera:?}: a tower beyond the box's east edge paints \
         {out_pixels} pixels; the same tower inside paints {in_pixels}",
    );
    assert!(
        in_pixels > 40,
        "the tower INSIDE the box paints only {in_pixels} pixels, so the \
         criterion below distinguishes nothing"
    );
    assert_eq!(
        out_pixels, 0,
        "a tower {out_pixels} pixels' worth outside the drawn box wrote \
         fragments. `t_scale` is only an over-estimate while every written \
         fragment is inside the unit cube",
    );
}
