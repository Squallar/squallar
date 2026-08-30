//! Does the ground pass occlude the march, and does its colour reach the screen?
//!
//! Driven through the **production** `encode_ground` + `encode_raymarch_with_floor`
//! pair recorded into one encoder in that order — the same two calls
//! `volume_bridge::prepare` makes.
//!
//! ```text
//! cargo test -p squallar-gpu --test volume_occluder -- --ignored --nocapture
//! ```
//!
//! `#[ignore]`d (CI's `gpu` job opts in on lavapipe), and the tests hold the
//! shared process-wide GPU lock: several devices on one adapter each blocking in
//! `poll(wait_indefinitely)` deadlock on this hardware.
//!
//! **What these criteria do and do not certify.** Everything below runs over
//! [`CAMERAS`], a set of eleven spanning the whole drag range: above the ground's
//! crest, between the crest and the box floor, and under the box floor. B1's
//! set was six and stopped at the crest, because below it the composite's arm
//! was written for a flat lid and the mesh's colour was discarded entirely. B1
//! pinned that hole and B2 generalised the arm, so the pin is now
//! [`the_ground_is_opaque_from_below_the_box_floor`] — the same three cameras,
//! asserting the opposite of what its predecessor asserted.
//!
//! A single camera would have certified "terrain draws from above" while
//! reading as "terrain draws"; six certified "terrain draws from above the
//! crest" while reading the same way.
//!
//! **A note on the control, because the plan this implements specified it for a
//! design that changed underneath it.** An earlier draft gave the ground pass a
//! single attachment and no path for its colour to reach the screen, and wrote
//! the control as "occluder on versus off; every differing pixel got strictly
//! less opaque". With the second colour attachment that same plan mandates, that
//! assertion is false of a *correct* build: the ground's own coverage pins alpha
//! at 1 across the whole footprint in both frames, so no pixel can differ in
//! opacity at all. The direction the assertion existed to check is checked here
//! on the quantity that actually moves, with the ground's colour contribution
//! held constant across the pair instead of alpha.
#![cfg(not(target_arch = "wasm32"))]

use egui_wgpu::wgpu;
use squallar_device_profile::quality::{GroundPass, ResolutionRung};
use squallar_egui::pane::OrbitCamera;
use squallar_egui::volume_view::{VolumeView, view_for};
use squallar_volumetric::raymarch::staging::VolumeStaging;
use squallar_volumetric::raymarch::{
    GroundHeights, OffscreenPlan, PaneMirror, VOLUME_SHADER_WGSL, VolumePipelines,
    post_center_fraction, unpack24_bytes,
};
use squallar_volumetric::uniform::VolumeUniform;

mod gpu_harness;
use gpu_harness::{
    MIRROR_FORMAT, attachments, device, gpu_lock, opaque_white_lut, planted_mirror, read_back,
};

/// Posts a side in the fixture height field.
///
/// **Not a production constant.** B3 removed the shader's post-count constant
/// and its Rust twin: the grid is laid out from
/// `textureDimensions(height_texture)`, so this is only the size *this file's*
/// fixture happens to be. 512 is what B1 drew, kept so the discretisation of
/// the analytic ridge below stays far under every tolerance here — linear
/// interpolation of a Gaussian of amplitude 0.25 and sigma 0.12 over a post
/// spacing of 1/512 is under 1e-5 in box z.
const POSTS: u32 = 512;

/// Width of the fixture ridge, as a fraction of the box's east-west extent.
///
/// A Gaussian rather than a cone so the surface has no crease for the
/// occluder's `t` to interpolate across discontinuously.
const RIDGE_SIGMA: f32 = 0.12;

/// The fixture height field, in box `z`: one ridge running north across the
/// box, peaked at its middle. **Clamped into the unit cube**, because
/// `VolumeUniform::t_scale_for`'s bound is the farthest cube CORNER and is only
/// an upper bound on a post while every post is inside.
fn ridge_height(uv: [f32; 2], amplitude: f32) -> f32 {
    let d = (uv[0] - 0.5) / RIDGE_SIGMA;
    (amplitude * (-0.5 * d * d).exp()).clamp(0.0, 1.0)
}

/// The height encoding this file's fixtures use, chosen so that box `z` maps to
/// a raw sample linearly with no offset: `z = raw / (POSTS_MAX)`.
///
/// Real fields arrive at `squallar_elevation`'s `HEIGHT_BASE_M` / 0.25 m
/// encoding and `VolumeUniform::height_affine` composes the lanes from it; that
/// composition has its own test. What this file needs is a field whose decoded
/// box `z` is exactly the analytic ridge to within the encoding's own quantum,
/// so the host oracle predicts the surface the GPU actually draws.
const HEIGHT_SCALE: f32 = 1.0 / 65_535.0;

/// The fixture field's samples for a ridge of `amplitude`, one `u16` a post.
fn ridge_samples(amplitude: f32) -> Vec<u16> {
    let mut samples = Vec::with_capacity((POSTS * POSTS) as usize);
    for j in 0..POSTS {
        for i in 0..POSTS {
            let uv = [
                post_center_fraction(i, POSTS),
                post_center_fraction(j, POSTS),
            ];
            let z = ridge_height(uv, amplitude);
            samples.push((z / HEIGHT_SCALE).round().clamp(0.0, 65_535.0) as u16);
        }
    }
    samples
}

/// The fixture field on the GPU.
fn heights(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    pipelines: &VolumePipelines,
    amplitude: f32,
) -> GroundHeights {
    pipelines
        .upload_heights(device, queue, [POSTS, POSTS], &ridge_samples(amplitude))
        .expect("the fixture height field was refused")
}

/// The mirror byte every texel of this file's drape carries.
///
/// **B3 replaced the ground's flat stand-in colour with the map drape**, so
/// "the mesh's colour" is now whatever the pane mirror holds under the mesh's
/// own surface point. Every criterion here that reads a colour wants that
/// colour to be one thing across the whole footprint, so the mirror is painted
/// flat: what these tests are about is *where* the mesh is and *whether* its
/// colour survives the march, and `volume_drape.rs` is where the registration
/// of a varying drape is measured.
const MIRROR_BYTE: u8 = 180;

/// A mirror painted flat at [`MIRROR_BYTE`], opaque.
fn flat_mirror(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    pipelines: &VolumePipelines,
) -> PaneMirror {
    const EDGE: u32 = 64;
    let rgba: Vec<u8> = std::iter::repeat_n(
        [MIRROR_BYTE, MIRROR_BYTE, MIRROR_BYTE, 255],
        (EDGE * EDGE) as usize,
    )
    .flatten()
    .collect();
    planted_mirror(device, queue, pipelines, [EDGE, EDGE], &rgba)
}

/// The floor lanes this file's box is drawn with: a site at the equator on the
/// prime meridian, with the mirror's texture coordinates running slowly enough
/// that the whole [`BOX_KM`] footprint lands well inside it.
///
/// The box is 240 km a side, which is +-1.079 degrees at the equator; at
/// `0.37` of a mirror per degree that is `u` in roughly 0.1 to 0.9. Nothing
/// here samples the mirror's border, so the flat paint above is the answer at
/// every surface point and "off the mirror" — which reads as no ground at all —
/// cannot be reached by accident.
fn floor_lanes() -> ([f32; 4], [f32; 4]) {
    // The box's own half-extents in degrees at the equator, then the rate that
    // puts its full span across `FOOTPRINT` of the mirror. Derived rather than
    // written down, so a reader can see where the numbers came from and a
    // change to `BOX_KM` carries them.
    const FOOTPRINT: f64 = 0.8;
    let half_deg = f64::from(BOX_KM[0] / 2.0) / f64::from(gpu_harness::DEGREE_BOX_KM);
    let half_north_deg = f64::from(BOX_KM[1] / 2.0) / f64::from(gpu_harness::DEGREE_BOX_KM);
    let u_per_degree = FOOTPRINT / (2.0 * half_deg);
    // v grows downward through the mirror and Mercator y grows north, so the
    // rate is negative.
    let v_per_mercator_y = -FOOTPRINT
        / (gpu_harness::mercator_y(half_north_deg) - gpu_harness::mercator_y(-half_north_deg));
    (
        [0.5, 0.5, u_per_degree as f32, v_per_mercator_y as f32],
        [
            0.0,
            -BOX_KM[0] / 2.0,
            -BOX_KM[1] / 2.0,
            // `MIRROR_FORMAT` is `Rgba8Unorm`, which is not sRGB, so the mirror
            // holds gamma-encoded texels — the lane the shader un-premultiplies
            // through.
            if MIRROR_FORMAT.is_srgb() { 0.0 } else { 1.0 },
        ],
    )
}

/// The offscreen every frame here is rendered at. Small enough that the host
/// oracle's per-pixel ray cast is cheap, large enough that "under one pixel"
/// means something.
const SIZE: [u32; 2] = [256, 160];

/// The box the scene is built in. Flat-ish, like every shipped one.
const BOX_KM: [f32; 3] = [240.0, 240.0, 20.0];

/// The stand-in ridge's height, in box `z`. Tall enough to reach through the
/// volume slab below, which is what gives the clip something to remove.
const RIDGE_AMPLITUDE: f32 = 0.25;

/// One camera: `(yaw, pitch, distance, vertical exaggeration)`.
type Camera = (f32, f32, f32, f32);

/// **Every camera the criteria below are asserted over, and they span the drag
/// range rather than the half of it that used to work.**
///
/// Eleven, spread over yaw, pitch, standoff and exaggeration — the repo's rule is
/// to arbitrate across four or five diverse sites rather than tune to one — and
/// across all three regions the composite distinguishes. The reason the last
/// matters is measured rather than assumed: at `distance 2.2` the eye crosses
/// `z = 0` at about pitch −0.9° and the crest at about −1.6°, so a
/// positive-pitch fixture certifies one half of one axis and every camera in
/// B1's set was in the outermost region of three.
///
/// The list itself lives in `gpu_harness` because `volume_drape.rs` reads it
/// too; the region split below is this file's own, because it is a property of
/// this file's box and ridge.
const CAMERAS: [Camera; 11] = gpu_harness::ORBIT_CAMERAS;

/// The three cameras of [`CAMERAS`] that put the eye under the box floor,
/// selected by the property rather than relisted.
fn below_the_box_floor() -> Vec<Camera> {
    CAMERAS
        .into_iter()
        .filter(|camera| view_at(*camera).eye_in_box[2] < 0.0)
        .collect()
}

fn view_at((yaw, pitch, distance, exaggeration): Camera) -> VolumeView {
    let camera =
        OrbitCamera::restore(yaw, pitch, distance, [0.0; 3], exaggeration).expect("finite camera");
    view_for(camera, BOX_KM, SIZE[0] as f32 / SIZE[1] as f32).expect("a view")
}

/// [`CAMERAS`] really does reach all three regions the composite distinguishes,
/// asserted rather than assumed: every one of these fixtures is a yaw, a pitch
/// and a standoff, and where that lands the eye is a whole camera pipeline away
/// from being obvious. A set that drifted into one region would still read as
/// "eleven cameras".
#[test]
fn the_camera_set_reaches_above_the_crest_under_it_and_under_the_floor() {
    let (mut over, mut between, mut under) = (0usize, 0usize, 0usize);
    for camera in CAMERAS {
        let z = view_at(camera).eye_in_box[2];
        if z >= RIDGE_AMPLITUDE {
            over += 1;
        } else if z >= 0.0 {
            between += 1;
        } else {
            under += 1;
        }
    }
    assert_eq!(
        (over, between, under),
        (6, 2, 3),
        "the camera set no longer spans the three regions it is asserted over. \
         Above the crest the ground is behind the march because the eye is; \
         between the crest and the floor it is behind because the march was \
         CLIPPED against it; under the floor it is behind for the same reason \
         and the shipped composite used to paint none of it",
    );
}

/// A precondition for the criteria that genuinely need the eye over the crest,
/// asserted rather than assumed: a fixture that drifted under the ground would
/// make the test measure something other than the property it names.
fn assert_above_the_ground(camera: Camera, view: &VolumeView, ridge: f32) {
    assert!(
        view.eye_in_box[2] > ridge,
        "camera {camera:?} puts the eye at box z {} , which is not above the \
         {ridge} ridge it is meant to be looking down on",
        view.eye_in_box[2],
    );
}

/// A uniform aimed by `view`, with the occluder on or off.
fn uniform(cells: [u32; 3], view: &VolumeView, ridge: f32, occluder: bool) -> VolumeUniform {
    let mut uniform = VolumeUniform::new(BOX_KM, cells);
    uniform.box_from_clip = view.box_from_clip;
    uniform.clip_from_box = view.clip_from_box;
    uniform.eye_in_box = view.eye_in_box;
    // Ambient only, so the march's shading is exactly 1 and the picture is a
    // function of the geometry rather than of a normal — and the same for the
    // ground, which C2 gave a normal of its own. Every criterion in this file
    // reads the mesh's colour as a discrete identity, so the light has to be
    // one under which the mesh's colour is the mirror's own.
    //
    // The two lines survive each other: `set_light` writes the three colour
    // and direction lanes and deliberately not `ambient`, which belongs to the
    // medium rather than to the light.
    uniform.ambient = 1.0;
    uniform.gradient_shading = false;
    uniform.set_light(gpu_harness::UNLIT);
    // **The mirror is bound in every frame here, and `map_floor` is on.**
    //
    // B1 held the mirror out of the picture and hardcoded `map_floor = false`
    // in every test in this file, which is what hid the lid-beside-a-mesh
    // defect from all of them: with both on, the pixels where a ray meets
    // z = 0 but never meets the mesh fell back to the lid, and the composite
    // painted it behind the march at full coverage from underneath. 76, 74 and
    // 33 such pixels at the three below-floor cameras, at alpha above 200.
    //
    // The mesh now takes its colour FROM the mirror — B3's drape — so holding
    // it out is no longer even possible: an unbound mirror is an invisible
    // terrain. Asking for the lid here and letting `aim_occluder` take it away
    // again is what makes this file exercise the defect's own configuration
    // rather than route around it.
    let (uv, geo) = floor_lanes();
    uniform.floor_uv = uv;
    uniform.floor_geo = geo;
    uniform.map_floor = true;
    if occluder {
        // Through the blessed setter, which derives the scale from THIS
        // uniform's eye — the two are independent lanes and a scale from
        // another eye mis-clips every ray — and which puts the lid out,
        // because the mesh IS the ground.
        uniform.aim_occluder(ridge, HEIGHT_SCALE, 0.0);
        assert!(
            !uniform.map_floor,
            "`aim_occluder` left the lid on beside a mesh, which is the pair \
             B1 measured and B3 closed",
        );
    } else {
        // The mesh still DRAWS — that is what makes the control below a
        // control — but the march is told no ground pass ran. The ceiling is
        // zeroed with it to obey the sentinel discipline `ground_max_z`'s own
        // doc states, and **nothing enforces that**: the composite reads
        // `occluder.x`, not the ceiling, so a stale ceiling changes no picture
        // in this tree. An earlier draft of this comment named a guard that
        // refused the incoherent pair at the seam a uniform reaches the GPU
        // through; no such guard has ever existed, and the one that lives there
        // checks the scale against the eye and never looks at the ceiling. If
        // B3 or B4 gives the ceiling a reader, that is the moment to write one.
        //
        // The decode lanes stay set: the mesh still has to stand at the same
        // heights, or the control below would be comparing two shapes rather
        // than two composites. The lid stays on for the same reason it is on
        // above — this is the frame with no ground pass, which is the only
        // frame that may have one.
        uniform.height_scale = HEIGHT_SCALE;
        uniform.height_offset = 0.0;
        uniform.ground_max_z = 0.0;
        uniform.occluder_t_scale = 0.0;
    }
    uniform
}

/// A grid whose bottom `filled` slabs of eight are one opaque index and whose
/// rest is no-data air.
fn floor_slab(filled: usize) -> ([u32; 3], Vec<u8>) {
    const EDGE: u32 = 8;
    let cells = [EDGE, EDGE, EDGE];
    let mut indices = Vec::with_capacity((EDGE * EDGE * EDGE) as usize);
    for slab in 0..EDGE as usize {
        let index = if slab < filled { 200 } else { 0 };
        indices.extend(std::iter::repeat_n(index, (EDGE * EDGE) as usize));
    }
    (cells, indices)
}

/// A grid of nothing but air: the volume contributes no colour at all, so
/// whatever reaches the screen came from the ground.
fn empty_grid() -> ([u32; 3], Vec<u8>) {
    floor_slab(0)
}

/// One frame: the ground pass and then the march, in one encoder, in that
/// order — and the occluder and ground attachments read back beside the
/// offscreen.
struct Frame {
    /// The composited offscreen, `gamma(C) * A` premultiplied.
    offscreen: Vec<[u8; 4]>,
    /// The occluder attachment: packed `t` in RGB, the hit flag in A.
    occluder: Vec<[u8; 4]>,
    /// The ground pass's own colour attachment.
    ground: Vec<[u8; 4]>,
}

/// One frame, with a height field of `amplitude` under the mesh and the flat
/// mirror bound for its drape.
///
/// The field and the mirror are built per call rather than hoisted: a 512-post
/// field is half a megabyte and these suites render a few dozen frames, and
/// building them here is what lets every call site name the amplitude it means
/// beside the uniform that describes it.
fn render(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    pipelines: &VolumePipelines,
    cells: [u32; 3],
    indices: &[u8],
    uniform: &VolumeUniform,
    amplitude: f32,
) -> Frame {
    let mirror = flat_mirror(device, queue, pipelines);
    let field = heights(device, queue, pipelines, amplitude);
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
    assert_eq!(
        target.ground_pass(),
        GroundPass::On,
        "the target did not carry the ground attachments it was planned with"
    );

    let mut encoder = device.create_command_encoder(&Default::default());
    pipelines.encode_ground(
        &mut encoder,
        &target,
        &volume,
        Some(&mirror),
        Some(&field),
        None,
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
// The host oracle: the same rays, cast in Rust.
// ---------------------------------------------------------------------------

/// `volume.wgsl`'s `slab_direction`, to the bit: each component's magnitude
/// floored, sign kept. `>= 0.0` takes the positive arm, as `select` does.
fn slab_direction(rd: [f32; 3]) -> [f32; 3] {
    rd.map(|c| {
        let magnitude = c.abs().max(1e-6);
        if c < 0.0 { -magnitude } else { magnitude }
    })
}

/// `volume.wgsl`'s `floor_hit`: where this ray meets `z = 0` inside the unit
/// square, or negative for none.
fn floor_hit(eye: [f32; 3], direction: [f32; 3]) -> f32 {
    if direction[2].abs() < 1e-6 {
        return -1.0;
    }
    let t = (0.0 - eye[2]) * (1.0 / slab_direction(direction)[2]);
    if t <= 0.0 {
        return -1.0;
    }
    let hit = [
        eye[0] + direction[0] * t,
        eye[1] + direction[1] * t,
        eye[2] + direction[2] * t,
    ];
    if !(0.0..=1.0).contains(&hit[0]) || !(0.0..=1.0).contains(&hit[1]) {
        return -1.0;
    }
    t
}

/// A column-major matrix applied to a homogeneous point, divided through.
fn unproject(m: [[f32; 4]; 4], ndc: [f32; 2], depth: f32) -> [f32; 3] {
    let p = [ndc[0], ndc[1], depth, 1.0];
    let mut out = [0.0f32; 4];
    for (r, slot) in out.iter_mut().enumerate() {
        *slot = (0..4).map(|k| m[k][r] * p[k]).sum();
    }
    [out[0] / out[3], out[1] / out[3], out[2] / out[3]]
}

/// The march's own ray for the pixel at `(column, row)`, cast from the pixel
/// **centre** — which is where `clip_position.xy` lands and therefore which
/// occluder texel the march reads.
fn ray(view: &VolumeView, column: u32, row: u32) -> [f32; 3] {
    let ndc = [
        2.0 * (column as f32 + 0.5) / SIZE[0] as f32 - 1.0,
        1.0 - 2.0 * (row as f32 + 0.5) / SIZE[1] as f32,
    ];
    let far = unproject(view.box_from_clip, ndc, 1.0);
    let eye = view.eye_in_box;
    let d = [far[0] - eye[0], far[1] - eye[1], far[2] - eye[2]];
    let length = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
    [d[0] / length, d[1] / length, d[2] / length]
}

fn along(eye: [f32; 3], direction: [f32; 3], t: f32) -> [f32; 3] {
    [
        eye[0] + direction[0] * t,
        eye[1] + direction[1] * t,
        eye[2] + direction[2] * t,
    ]
}

/// Every crossing of the analytic surface along this ray inside the box, in
/// order. Marched finely and refined by bisection — a mesh's own tessellation
/// is not what decides which surface is nearer.
fn surface_crossings(eye: [f32; 3], direction: [f32; 3], amplitude: f32) -> Vec<f32> {
    let height_above = |t: f32| {
        let p = along(eye, direction, t);
        p[2] - ridge_height([p[0], p[1]], amplitude)
    };
    let inside = |t: f32| {
        let p = along(eye, direction, t);
        (0.0..=1.0).contains(&p[0]) && (0.0..=1.0).contains(&p[1])
    };
    const STEPS: usize = 4000;
    // The box is the unit cube and the direction is normalised in box space, so
    // a ray from any camera in this file reaches the far side well inside this.
    // 4000 steps over it is ~60 samples across the ridge's own sigma.
    const FAR: f32 = 8.0;
    let mut out = Vec::new();
    let mut previous: Option<(f32, f32)> = None;
    for step in 0..=STEPS {
        let t = FAR * step as f32 / STEPS as f32;
        if !inside(t) {
            previous = None;
            continue;
        }
        let f = height_above(t);
        if let Some((t_before, f_before)) = previous
            && f_before > 0.0
            && f <= 0.0
        {
            let (mut lo, mut hi) = (t_before, t);
            for _ in 0..40 {
                let mid = 0.5 * (lo + hi);
                if height_above(mid) > 0.0 {
                    lo = mid;
                } else {
                    hi = mid;
                }
            }
            out.push(0.5 * (lo + hi));
        }
        previous = Some((t, f));
    }
    out
}

/// Pixels that are *not* within `margin` of a disagreement between two masks —
/// the rasteriser's coverage rule and an exact ray test necessarily differ on
/// the silhouette, and nowhere else.
fn interior(mask: &[bool], margin: i32) -> Vec<bool> {
    let at = |c: i32, r: i32| -> bool {
        if c < 0 || r < 0 || c >= SIZE[0] as i32 || r >= SIZE[1] as i32 {
            return false;
        }
        mask[(r as u32 * SIZE[0] + c as u32) as usize]
    };
    let mut out = vec![false; mask.len()];
    for row in 0..SIZE[1] as i32 {
        for column in 0..SIZE[0] as i32 {
            let mut all = true;
            for dr in -margin..=margin {
                for dc in -margin..=margin {
                    all &= at(column + dc, row + dr);
                }
            }
            out[(row as u32 * SIZE[0] + column as u32) as usize] = all;
        }
    }
    out
}

/// Premultiplied luminance, the scalar the control's direction is measured on.
fn luminance(pixel: [u8; 4]) -> f32 {
    0.2126 * f32::from(pixel[0]) + 0.7152 * f32::from(pixel[1]) + 0.0722 * f32::from(pixel[2])
}

/// The drape's colour as the offscreen must hold it: gamma-encoded and
/// premultiplied by a coverage of exactly 1.
///
/// The mirror is painted flat at [`MIRROR_BYTE`] and opaque, so the shader
/// un-premultiplies by 1, decodes to linear, the composite re-encodes to
/// gamma and multiplies by a coverage of 1 — which is the mirror's own byte
/// back again, to within the two 8-bit round trips between. That identity is
/// asserted rather than assumed by
/// `the_grounds_own_colour_reaches_the_screen`'s tolerance — which is
/// `#[ignore]`d like everything else here; run it with
/// `cargo test -p squallar-gpu --test volume_occluder -- --ignored`.
fn expected_ground_pixel() -> [u8; 3] {
    [MIRROR_BYTE; 3]
}

// ---------------------------------------------------------------------------
// (a) Registration.
// ---------------------------------------------------------------------------

/// **The occluder is registered with the march's own rays, at every camera.**
///
/// With a flat analytic source the mesh *is* the `z = 0` plane, and the march
/// already solves that plane analytically in `floor_hit`. Decoding the packed
/// `t` and comparing the two proves the camera, the space, the packing and the
/// decode together — one derivation checked against another that shares nothing
/// but the uniform block.
#[test]
#[ignore = "needs a real wgpu adapter; see the doc comment for the invocation"]
fn the_occluder_decodes_to_the_ray_parameter_the_march_would_solve() {
    let _held = gpu_lock();
    let (device, queue) = device();
    let pipelines = VolumePipelines::new(&device, attachments(wgpu::TextureFormat::Rgba8Unorm));
    pipelines.upload_quad(&queue);

    // No precondition on the eye's height: this criterion reads the occluder
    // ATTACHMENT rather than the composite, and `floor_hit` solves the same
    // plane for a ray travelling up as for one travelling down.
    for camera in CAMERAS {
        let view = view_at(camera);
        let (cells, indices) = empty_grid();
        // Flat: amplitude zero, so the mesh and `floor_hit` describe one surface.
        let uniform = uniform(cells, &view, 0.0, true);
        let t_scale = uniform.occluder_t_scale;
        let frame = render(&device, &queue, &pipelines, cells, &indices, &uniform, 0.0);

        let mut gpu_mask = vec![false; (SIZE[0] * SIZE[1]) as usize];
        let mut host_mask = vec![false; (SIZE[0] * SIZE[1]) as usize];
        let mut host_t = vec![-1.0f32; (SIZE[0] * SIZE[1]) as usize];
        for row in 0..SIZE[1] {
            for column in 0..SIZE[0] {
                let at = (row * SIZE[0] + column) as usize;
                gpu_mask[at] = frame.occluder[at][3] > 127;
                let t = floor_hit(view.eye_in_box, ray(&view, column, row));
                host_t[at] = t;
                host_mask[at] = t >= 0.0;
            }
        }

        let covered = gpu_mask.iter().filter(|c| **c).count();
        assert!(
            covered > (SIZE[0] * SIZE[1]) as usize / 20,
            "{camera:?}: the ground mesh covered {covered} of {} pixels, which \
             is too few for anything below to be measuring the picture",
            SIZE[0] * SIZE[1]
        );

        // Outside the footprint the pass wrote nothing, so the alpha the march
        // tests must read zero — and the RGB must decode past the box.
        for (at, (gpu, host)) in gpu_mask.iter().zip(&host_mask).enumerate() {
            if *gpu || *host {
                continue;
            }
            let texel = frame.occluder[at];
            assert_eq!(
                texel[3], 0,
                "{camera:?}: an unhit texel carries alpha {}, so the march \
                 would clip a ray against ground that is not there",
                texel[3]
            );
            let decoded = unpack24_bytes([texel[0], texel[1], texel[2]]) * t_scale;
            assert!(
                decoded >= t_scale * 0.999,
                "{camera:?}: an undrawn texel decodes to t = {decoded} against \
                 a scale of {t_scale}; the clear must decode PAST the box, so \
                 that a dropped alpha test would still be a no-op rather than a \
                 wall"
            );
        }

        // The silhouette is where a coverage rule and an exact ray test are
        // allowed to disagree; two pixels in from either mask's edge, they may
        // not.
        let mut agree = vec![false; gpu_mask.len()];
        for (at, slot) in agree.iter_mut().enumerate() {
            *slot = gpu_mask[at] == host_mask[at];
        }
        let inside = interior(&agree, 2);
        let mut compared = 0usize;
        let mut worst = 0.0f32;
        let mut low_digits = [false; 256];
        for (at, ok) in inside.iter().enumerate() {
            if !*ok || !gpu_mask[at] {
                continue;
            }
            let texel = frame.occluder[at];
            let decoded = unpack24_bytes([texel[0], texel[1], texel[2]]) * t_scale;
            worst = worst.max((decoded - host_t[at]).abs());
            low_digits[texel[2] as usize] = true;
            compared += 1;
        }
        let distinct_lows = low_digits.iter().filter(|seen| **seen).count();
        assert!(
            compared > covered / 2,
            "{camera:?}: only {compared} of {covered} covered pixels were away \
             from a mask disagreement; the two masks are not describing the \
             same surface"
        );

        // The bound, in box units, is what one offscreen pixel is worth at this
        // camera: the largest step in the host's own `t` between neighbouring
        // pixels. Derived from the picture rather than chosen, and it is
        // exactly the error a half-pixel registration slip would produce.
        let mut one_pixel = 0.0f32;
        for row in 0..SIZE[1] {
            for column in 1..SIZE[0] {
                let at = (row * SIZE[0] + column) as usize;
                if host_t[at] < 0.0 || host_t[at - 1] < 0.0 {
                    continue;
                }
                one_pixel = one_pixel.max((host_t[at] - host_t[at - 1]).abs());
            }
        }
        let code = t_scale / 16_777_215.0;
        eprintln!(
            "occluder registration {camera:?}: {compared} px, worst |dt| = \
             {worst:.3e} box units = {:.1} codes; one pixel = {one_pixel:.3e}, \
             t_scale = {t_scale:.3}, {distinct_lows} distinct low digits",
            worst / code,
        );
        assert!(
            worst < one_pixel,
            "{camera:?}: the decoded occluder disagrees with the march's own \
             `floor_hit` by {worst} box units, which is more than the \
             {one_pixel} one pixel is worth at this camera — the mesh and the \
             march are not registered"
        );

        // **The low digit has to carry information, and a tolerance cannot
        // check that.** A pack whose low term overflows what an `Rgba8Unorm`
        // holds clamps to 255 on every drawn texel — 8 of the 24 bits gone —
        // and the bound above cannot see it: 255 codes of `t_scale` is 1.5e-4
        // box units at one of these cameras and 4.2e-5 at another, the same
        // size as the interpolation disagreement being measured. So this is a
        // measurement of the shipped bytes instead: across a footprint this
        // size the blue channel must take most of its values.
        assert!(
            distinct_lows > 64,
            "{camera:?}: the occluder's low digit takes only {distinct_lows} \
             distinct values over {compared} texels. It should take nearly all \
             256 — one value means the digit is being clamped away and the \
             packing has 16 bits, not 24, whatever the registration bound says"
        );
    }
}

// ---------------------------------------------------------------------------
// (b) The control.
// ---------------------------------------------------------------------------

/// **The mesh clips the march, and it only ever removes volume from a ray.**
///
/// Two frames through the production pair, with the ground's colour
/// contribution held constant across them — flat ground against a ridge, both
/// with the occluder on. That is the generalisation of the plan's own rule that
/// `map_floor` must not differ across the pair: if the ground contributes in one
/// frame and not the other, the difference measured is the ground appearing, not
/// the volume being clipped.
///
/// **This is the one criterion in this file that stays regional after B2, and
/// the reason is geometric rather than a limit on the composite.** B1 narrowed
/// it from "universal" to "while the eye is above the ground" because below the
/// plane the shipped composite took neither arm and the sign inverted. That
/// reason is gone — the ground is opaque from underneath now — but the
/// direction still does not hold there, for a different reason that B2
/// measured: **from below the box floor a taller ridge clips LESS, not more.**
/// The ray enters the box through its bottom face travelling up, so raising
/// `h(x, y)` pointwise moves the first upward crossing later and the marchable
/// span between the two grows.
///
/// Measured at `(215, -18, 2.2, 1)`, and the denominators are named because an
/// earlier draft of this doc got them wrong by 2.5x: of 7053 pixels with ground
/// in **both** frames, **2843 differ (40.3%) and every one of those got
/// brighter**; the other 4210 are unchanged. Not "all 7053 got brighter", and
/// the per-pixel deltas are not summarised here at all — the first four the
/// assertion prints share one value and that value is not an aggregate.
///
/// So this criterion runs over the six cameras above the crest, and every other
/// criterion in this file runs over all eleven. That split is asserted below,
/// not left to the reader: a set that quietly shrank would still read as "the
/// cameras above the crest". See
/// [`the_ground_is_opaque_from_below_the_box_floor`], which is `#[ignore]`d
/// like this one; run it with
/// `cargo test -p squallar-gpu --test volume_occluder -- --ignored`.
#[test]
#[ignore = "needs a real wgpu adapter; see the doc comment for the invocation"]
fn a_ridge_removes_volume_from_every_ray_it_changes_and_adds_none() {
    let _held = gpu_lock();
    let (device, queue) = device();
    let pipelines = VolumePipelines::new(&device, attachments(wgpu::TextureFormat::Rgba8Unorm));
    pipelines.upload_quad(&queue);

    // The bottom quarter of the box, semi-transparent: saturated white would
    // make the clip invisible, since a saturated ray is white however far it
    // marched.
    let (cells, indices) = floor_slab(2);

    let above: Vec<Camera> = CAMERAS
        .into_iter()
        .filter(|camera| view_at(*camera).eye_in_box[2] > RIDGE_AMPLITUDE)
        .collect();
    assert_eq!(
        above.len(),
        6,
        "the six cameras above the crest are the ones this direction is defined \
         over, and there are now {}: {above:?}",
        above.len(),
    );

    for camera in above {
        let view = view_at(camera);
        assert_above_the_ground(camera, &view, RIDGE_AMPLITUDE);
        let semi_transparent = |ridge: f32| {
            let mut u = uniform(cells, &view, ridge, true);
            u.extinction_per_km = 0.15;
            u
        };
        let flat = render(
            &device,
            &queue,
            &pipelines,
            cells,
            &indices,
            &semi_transparent(0.0),
            0.0,
        );
        let ridged = render(
            &device,
            &queue,
            &pipelines,
            cells,
            &indices,
            &semi_transparent(RIDGE_AMPLITUDE),
            RIDGE_AMPLITUDE,
        );

        let mut common = 0usize;
        let mut differing = 0usize;
        let mut brighter = Vec::new();
        for at in 0..(SIZE[0] * SIZE[1]) as usize {
            // Both frames' ground covered this pixel: outside that set the
            // ridge's own silhouette is what moved, which is a different claim.
            if flat.ground[at][3] < 255 || ridged.ground[at][3] < 255 {
                continue;
            }
            common += 1;
            let before = luminance(flat.offscreen[at]);
            let after = luminance(ridged.offscreen[at]);
            // One code of round-off in the 8-bit target is not a difference.
            if (after - before).abs() <= 1.0 {
                continue;
            }
            differing += 1;
            if after > before {
                brighter.push((at, before, after));
            }
        }

        assert!(
            common > (SIZE[0] * SIZE[1]) as usize / 20,
            "{camera:?}: only {common} pixels had ground under them in both \
             frames"
        );
        let fraction = differing as f32 / common as f32;
        eprintln!(
            "ridge control {camera:?}: {differing} of {common} pixels with \
             ground in BOTH frames differ ({:.1}%)",
            fraction * 100.0
        );
        assert!(
            fraction > 0.05,
            "{camera:?}: a {RIDGE_AMPLITUDE} ridge changed only {:.2}% of the \
             pixels it stands under. The mesh is not clipping the march — and a \
             percentage floor is the only thing that notices a clamp that \
             compiles and does nothing",
            fraction * 100.0
        );
        assert!(
            brighter.is_empty(),
            "{camera:?}: {} pixels got BRIGHTER with a ridge in front of the \
             volume, first {:?}. A ray stopped early can only carry less of the \
             volume it was marching, never more; a difference in both \
             directions is what a registration bug looks like and is exactly \
             what a bare percentage floor would have passed",
            brighter.len(),
            &brighter[..brighter.len().min(4)],
        );
    }
}

/// And the occluder is what does it: with `occluder.x` at zero the very same
/// ridge changes nothing at all.
#[test]
#[ignore = "needs a real wgpu adapter; see the doc comment for the invocation"]
fn with_the_occluder_off_the_same_ridge_changes_nothing() {
    let _held = gpu_lock();
    let (device, queue) = device();
    let pipelines = VolumePipelines::new(&device, attachments(wgpu::TextureFormat::Rgba8Unorm));
    pipelines.upload_quad(&queue);

    let (cells, indices) = floor_slab(2);
    for camera in CAMERAS {
        let view = view_at(camera);
        let off = |ridge: f32| {
            let mut u = uniform(cells, &view, ridge, false);
            u.extinction_per_km = 0.15;
            u
        };
        let flat = render(&device, &queue, &pipelines, cells, &indices, &off(0.0), 0.0);
        let ridged = render(
            &device,
            &queue,
            &pipelines,
            cells,
            &indices,
            &off(RIDGE_AMPLITUDE),
            RIDGE_AMPLITUDE,
        );
        assert_eq!(
            flat.offscreen, ridged.offscreen,
            "{camera:?}: the ridge changed the picture with the occluder \
             switched off, so the control above is not measuring the occluder"
        );
    }
}

// ---------------------------------------------------------------------------
// (e) The non-invisibility criterion.
// ---------------------------------------------------------------------------

/// **The ground's colour survives the raymarch pass, at every camera.**
///
/// This exists because the control above cannot see a terrain that never draws:
/// a correctly clipped volume over an empty background differs in exactly the
/// direction a control asserts, so the flagship test passes on a build with no
/// visible ground at all. Here the volume is air, so anything on the screen
/// inside the ridge's silhouette came from the mesh.
///
/// It is asserted over eleven cameras rather than one because at one it
/// certified "terrain draws **from above**" while reading as "terrain draws" —
/// the exact vacuity this criterion was added to close, one level up. At six it
/// then certified "terrain draws **from above the crest**" and read the same
/// way, which is the same vacuity a region narrower; the fixture is now the
/// whole drag range, and `the_camera_set_reaches_above_the_crest_under_it_and_
/// under_the_floor` is what keeps it there.
#[test]
#[ignore = "needs a real wgpu adapter; see the doc comment for the invocation"]
fn the_grounds_own_colour_reaches_the_screen() {
    let _held = gpu_lock();
    let (device, queue) = device();
    let pipelines = VolumePipelines::new(&device, attachments(wgpu::TextureFormat::Rgba8Unorm));
    pipelines.upload_quad(&queue);

    let expected = expected_ground_pixel();
    let (cells, indices) = empty_grid();

    for camera in CAMERAS {
        let view = view_at(camera);
        let lit = uniform(cells, &view, RIDGE_AMPLITUDE, true);
        let mut dark = uniform(cells, &view, RIDGE_AMPLITUDE, false);
        // **The control is a frame with NO ground in it at all**, not merely
        // one whose occluder is off. B3 made the mesh's colour the map drape,
        // and `uniform` therefore binds the mirror and asks for the lid in
        // every frame; `aim_occluder` takes the lid away again wherever a mesh
        // draws. So an occluder-off frame still paints ground — the lid — and
        // 11299 of its pixels are opaque, measured. Turning the lid off here
        // as well is what puts the control back on the question it asks:
        // whether the colour above had to come from the mesh.
        dark.map_floor = false;
        let lit = render(
            &device,
            &queue,
            &pipelines,
            cells,
            &indices,
            &lit,
            RIDGE_AMPLITUDE,
        );
        let dark = render(
            &device,
            &queue,
            &pipelines,
            cells,
            &indices,
            &dark,
            RIDGE_AMPLITUDE,
        );

        // **Inside the silhouette**, which is what the criterion says. The
        // rasteriser's coverage rule and the march's own `slab_entry_exit`
        // necessarily disagree on the rim: at the grazing camera in this set,
        // one pixel of 4113 is covered by the mesh while the march's ray
        // misses the box and returns transparent. One pixel in from the edge
        // there is no such thing.
        let drew: Vec<bool> = (0..(SIZE[0] * SIZE[1]) as usize)
            .map(|at| lit.ground[at][3] == 255)
            .collect();
        let well_inside = interior(&drew, 1);

        let mut painted = 0usize;
        let mut wrong = Vec::new();
        for (at, inside) in well_inside.iter().enumerate() {
            if !inside {
                continue;
            }
            painted += 1;
            let pixel = lit.offscreen[at];
            let off_by = |lane: usize| i32::from(pixel[lane]).abs_diff(i32::from(expected[lane]));
            if pixel[3] != 255 || (0..3).any(|lane| off_by(lane) > 2) {
                wrong.push((at, pixel));
            }
        }

        assert!(
            painted > (SIZE[0] * SIZE[1]) as usize / 20,
            "{camera:?}: the mesh covered only {painted} pixels, so this test \
             is asserting about almost nothing"
        );
        assert!(
            wrong.is_empty(),
            "{camera:?}: {} of {painted} pixels the ground drew over do not \
             carry the ground's colour {expected:?} (first {:?}). The mesh's \
             colour is not reaching the screen: the raymarch pass clears the \
             offscreen, so a ground that wrote its colour there rather than \
             into its own attachment is erased before anything reads it — and \
             the clip alone would still pass the control above",
            wrong.len(),
            &wrong[..wrong.len().min(4)],
        );

        // And the contrast: with the ground pass's output ignored, the same air
        // volume paints nothing at all. Without this the assertion above could
        // be satisfied by a background that happened to be that colour.
        let opaque = dark.offscreen.iter().filter(|p| p[3] > 0).count();
        assert_eq!(
            opaque, 0,
            "{camera:?}: {opaque} pixels are opaque with no ground at all over \
             an empty grid, so the colour asserted above did not have to come \
             from the mesh"
        );
    }
}

/// **A frame holds ONE ground: the lid is never painted where the mesh did not
/// draw.**
///
/// B1 handed this forward in writing, in `volume.wgsl`'s own `floor_fade`
/// comment. `floor_t` fell back to the flat lid at every pixel where a ray
/// crossed `z = 0` inside the unit square but left the box without meeting the
/// mesh — the silhouette's outside edge — and in a frame that HAD a ground pass
/// those pixels took `floor_fade = 1.0` and an arm of `true`, so the lid
/// composited behind the march at full opacity while the eye was under it.
/// B1 probed it at the three below-floor cameras and measured **76, 74 and 33
/// pixels at alpha above 200**.
///
/// **The uniform here is deliberately the pair `aim_occluder` refuses.** The
/// type will not build it — `aim_occluder` clears `map_floor`, and
/// `aiming_the_occluder_puts_the_map_lid_out` pins that — so this test puts the
/// lid back on afterwards, which is the one order that reaches the defect. That
/// is the point: the shader must be right even when the uniform lies, because
/// "the caller is careful" is not a property anything can measure.
///
/// The non-triviality half is not an argument, it is a second render: the same
/// scene through B1's own guard, substituted into the WGSL and built through
/// `from_shader_source`, which must paint lid pixels where the shipped one
/// paints none.
#[test]
#[ignore = "needs a real wgpu adapter; see the doc comment for the invocation"]
fn the_lid_is_never_painted_where_the_mesh_did_not_draw() {
    let _held = gpu_lock();
    let (device, queue) = device();
    let attachments = attachments(wgpu::TextureFormat::Rgba8Unorm);
    let shipped = VolumePipelines::new(&device, attachments);
    shipped.upload_quad(&queue);

    /// The shipped guard, and B1's, which is the shipped one with the
    /// ground-pass sentinel deleted.
    const SHIPPED: &str = "        volume.flags.w > 0.5 && volume.occluder.x <= 0.0,";
    const B1: &str = "        volume.flags.w > 0.5,";
    assert_eq!(
        VOLUME_SHADER_WGSL.matches(SHIPPED).count(),
        1,
        "the lid's guard has moved; re-anchor this mutant rather than deleting it",
    );
    let regressed = VolumePipelines::from_shader_source(
        &device,
        attachments,
        &VOLUME_SHADER_WGSL.replacen(SHIPPED, B1, 1),
    );
    regressed.upload_quad(&queue);

    let (cells, indices) = empty_grid();
    let mut regressions = 0usize;
    for camera in CAMERAS {
        let view = view_at(camera);
        let mut lying = uniform(cells, &view, RIDGE_AMPLITUDE, true);
        assert!(
            !lying.map_floor,
            "`aim_occluder` no longer clears the lid, so this fixture is not \
             restoring the pair — it is being handed it",
        );
        lying.map_floor = true;

        // Pixels the mesh never covered, in the frame's own occluder alpha —
        // the same channel `ground_covered` reads, so this is the very set the
        // composite falls back to the lid on.
        let count_lid = |frame: &Frame| -> usize {
            (0..(SIZE[0] * SIZE[1]) as usize)
                .filter(|&at| frame.ground[at][3] == 0 && frame.offscreen[at][3] > 200)
                .count()
        };

        let held = render(
            &device,
            &queue,
            &shipped,
            cells,
            &indices,
            &lying,
            RIDGE_AMPLITUDE,
        );
        assert_eq!(
            count_lid(&held),
            0,
            "{camera:?}: the flat lid painted where the mesh never drew. A \
             frame holds one ground; the lid composited there is behind the \
             march at full opacity with the eye under it, which is the defect \
             B2 removed for the mesh surviving on the other surface",
        );

        regressions += count_lid(&render(
            &device,
            &queue,
            &regressed,
            cells,
            &indices,
            &lying,
            RIDGE_AMPLITUDE,
        ));
    }

    // **The non-triviality half.** B1's guard has to paint some of those
    // pixels somewhere in the set, or the assertion above is true of a build
    // that cannot fail it — and this whole criterion would be a check that
    // cannot fail.
    assert!(
        regressions > 0,
        "B1's own guard painted no lid pixels at any of the eleven cameras, so \
         the assertion above is not measuring anything. Either the fixture has \
         drifted off the configuration that reaches the defect — the lid on \
         beside an aimed occluder, seen from under the box floor — or the \
         mutant is no longer the guard it replaces",
    );
}

// ---------------------------------------------------------------------------
// Under the box floor: B1's pinned hole, rewritten to the behaviour that
// replaced it.
// ---------------------------------------------------------------------------

/// **From below the box floor the ground is OPAQUE, and every pixel it drew
/// reaches the screen.**
///
/// This test used to assert the exact opposite, at these exact three cameras,
/// and B1 left it standing with instructions to rewrite rather than delete it.
/// What it measured: `eye_above_plane = eye.z >= 0.0` was a predicate written
/// for a flat lid, and `floor_fade = clamp(1 + eye.z / FLOOR_BELOW_FADE)` with
/// `FLOOR_BELOW_FADE = 0.08` is zero for any eye more than 0.08 box heights
/// under the plane — so below the plane neither arm ran, `surface_colour` was
/// never called, and the mesh's colour was discarded while the *clip* still
/// applied. 7198/7198, 14664/14664 and 40960/40960 pixels the mesh drew
/// composited to alpha 0.
///
/// **It was never a corner.** `MAX_PITCH_DEG = 89.0`, pitch is clamped to
/// `[-89, +89]`, and at the default standoff the eye crosses `z = 0` at about
/// pitch −0.9°, so essentially the whole negative-pitch half of an ordinary
/// drag was in it.
///
/// **The decision B2 made, and it is the better of two bad states rather than
/// a good one.** See
/// [`the_mesh_walls_the_volume_off_from_below_and_the_lid_does_not`], which
/// measures what opacity costs and hands the real fix forward. It is
/// `#[ignore]`d like this one; run both with
/// `cargo test -p squallar-gpu --test volume_occluder -- --ignored`.
///
/// An earlier draft of this comment argued that `FLOOR_BELOW_FADE` exists
/// because a featureless infinite lid walls the pane off, and that standing
/// ground is different because "it has a silhouette, a horizon and an edge to
/// see past". **That argument is withdrawn: it is false at the camera in this
/// very fixture.** At `(140, -89, 1.0, 1.0)` the mesh covers 40960 of 40960
/// pixels. Wall to wall. No silhouette, no edge.
///
/// What survives is narrower and is about what B2 can reach. The march is
/// CLIPPED against the mesh, and that clip is source-order-pinned with its own
/// mutant battery — it is not B2's to move. With it in place there are exactly
/// two states: opaque terrain, or the hole B1 measured, where the clip cuts the
/// volume and a faded mesh paints nothing over the cut, so the user sees
/// neither weather nor ground. A fade cannot un-do a binary clip, and there is
/// no coverage between 0 and 1 at which the two agree. Opaque strictly
/// dominates the hole, needs no third arm and no new number, and is what this
/// lands. It does **not** answer the user's report; that is recorded as an open
/// item on the track that owns the clip.
///
/// A second cost, recorded rather than glossed: from below, B3's drape is the
/// top-down map raster painted on the underside of the terrain. That is a
/// wrong-side texture and it wants a distinct underside shade, which is a
/// D-track change to what the ground pass writes.
///
/// **What this pin uniquely certifies is narrower than its name.** The ridge
/// half of it is now also covered by `the_grounds_own_colour_reaches_the_screen`
/// (`#[ignore]`d, same invocation as above),
/// which runs over all eleven cameras and so includes these three; what only
/// this test carries is the relief-0 corner below, and that corner is the sole
/// killer of the empty-span mutant. The name is kept because the behaviour it
/// names is the one B1 handed forward, but a reader should not take it for more
/// coverage than that.
///
/// **A ridge and a FLAT mesh, because the flat one is its own corner.** A mesh
/// with no relief stands exactly on the box's bottom face, so from underneath
/// its `ground_t` **is** the ray's box entry and `span.y = min(span.y,
/// ground_t)` empties the span. The march's `span.y <= span.x` early-out then
/// returned transparent over ground the mesh had drawn — the same defect this
/// test is named for, in the one configuration the ridge cannot reach. It is a
/// reason to march nothing, not a reason to draw nothing.
#[test]
#[ignore = "needs a real wgpu adapter; see the doc comment for the invocation"]
fn the_ground_is_opaque_from_below_the_box_floor() {
    let _held = gpu_lock();
    let (device, queue) = device();
    let pipelines = VolumePipelines::new(&device, attachments(wgpu::TextureFormat::Rgba8Unorm));
    pipelines.upload_quad(&queue);

    let below = below_the_box_floor();
    assert_eq!(
        below.len(),
        3,
        "the three cameras this pin was written against are no longer in \
         `CAMERAS`, so it is measuring something else: {below:?}",
    );

    let expected = expected_ground_pixel();
    for (camera, relief) in below
        .into_iter()
        .flat_map(|camera| [(camera, RIDGE_AMPLITUDE), (camera, 0.0)])
    {
        let view = view_at(camera);

        let (cells, indices) = empty_grid();
        let lit = uniform(cells, &view, relief, true);
        let frame = render(&device, &queue, &pipelines, cells, &indices, &lit, relief);

        let drew = (0..(SIZE[0] * SIZE[1]) as usize)
            .filter(|at| frame.ground[*at][3] == 255)
            .count();
        assert!(
            drew > (SIZE[0] * SIZE[1]) as usize / 20,
            "{camera:?} at relief {relief}: the mesh drew over only {drew} \
             pixels, so this pin is not measuring the region it names"
        );
        // One pixel in from the silhouette, for the same reason the criterion
        // above is: the rasteriser's coverage rule and the march's own
        // `slab_entry_exit` necessarily disagree on the rim and nowhere else.
        let ground_mask: Vec<bool> = (0..(SIZE[0] * SIZE[1]) as usize)
            .map(|at| frame.ground[at][3] == 255)
            .collect();
        let well_inside = interior(&ground_mask, 1);

        let mut painted = 0usize;
        let mut invisible = 0usize;
        let mut wrong = Vec::new();
        for (at, inside) in well_inside.iter().enumerate() {
            if !inside {
                continue;
            }
            painted += 1;
            let pixel = frame.offscreen[at];
            if pixel[3] == 0 {
                invisible += 1;
            }
            let off_by = |lane: usize| i32::from(pixel[lane]).abs_diff(i32::from(expected[lane]));
            if pixel[3] != 255 || (0..3).any(|lane| off_by(lane) > 2) {
                wrong.push((at, pixel));
            }
        }
        eprintln!(
            "below-floor opacity {camera:?} relief {relief}: eye z {:.3}, mesh \
             drew {drew} px, {painted} well inside, {invisible} composited to \
             alpha 0",
            view.eye_in_box[2]
        );
        assert!(
            wrong.is_empty(),
            "{camera:?} at relief {relief}: {} of {painted} pixels the mesh \
             drew do not carry its colour {expected:?} from underneath (first \
             {:?}, {invisible} of them fully transparent). An eye under the box \
             floor is under the terrain, not under a lid: the march is CLIPPED \
             against the mesh, so fading the mesh out leaves the hole the clip \
             cut with nothing in it — which is what 40960 of 40960 transparent \
             pixels at pitch −89° used to be. At relief 0 the mesh stands on \
             the box's bottom face and the clip empties the span outright, \
             which the march must answer by drawing the surface rather than by \
             returning transparent",
            wrong.len(),
            &wrong[..wrong.len().min(4)],
        );
    }
}

// ---------------------------------------------------------------------------
// What opacity from below costs, measured rather than argued.
// ---------------------------------------------------------------------------

/// **The mesh walls the volume off from below. The lid does not, and a user
/// reported the lid.**
///
/// `volume_gpu::the_floor_is_transparent_from_below` — `#[ignore]`d like this
/// one, run with `cargo test -p squallar-gpu --test volume_gpu -- --ignored` —
/// asserts that a saturated slab in the box's top half shows through the ground
/// from underneath, and its message names the reason: *"an opaque ground from
/// underneath is the wall the user reported"*. It stays green because it runs
/// with **no ground pass**, and the symptom it names comes back through the
/// mesh. This is that test's missing sibling: same question, ground pass on.
///
/// **The answer today is the wall, and this pins it rather than hiding it.**
/// The occlusion is B1's clip, not B2's arm — `span.y = min(span.y, ground_t)`
/// removes the volume above the terrain whatever the composite then paints, and
/// that clip is source-order-pinned with its own mutant battery. B2 only chose
/// what fills the cut: opaque terrain, or the hole B1 measured where nothing is
/// painted at all and the user sees neither weather nor ground. Opacity
/// dominates the hole. Neither answers the report.
///
/// **The fix, for whoever owns the clip.** The clip and the coverage have to be
/// one decision. Below the plane that means not clipping against a surface the
/// composite is going to fade, and compositing it OVER the march with
/// `ground.a * floor_fade` — the "floor in front" arm, which already exists and
/// is currently unreachable with a ground pass on. That is a change to a pinned
/// invariant and to `floor_fade`'s domain, and it is deliberately not made
/// here.
///
/// When it is made, **this test fails and must be rewritten to the behaviour
/// that replaced it, not deleted** — the same instruction B1 left on the pin
/// this file's other criteria grew out of.
#[test]
#[ignore = "needs a real wgpu adapter; see the doc comment for the invocation"]
fn the_mesh_walls_the_volume_off_from_below_and_the_lid_does_not() {
    let _held = gpu_lock();
    let (device, queue) = device();
    let pipelines = VolumePipelines::new(&device, attachments(wgpu::TextureFormat::Rgba8Unorm));
    pipelines.upload_quad(&queue);

    // Straight up from under the box floor, where the mesh fills the frame:
    // the camera that makes the "it has an edge to see past" argument false.
    let camera = (140.0f32, -89.0f32, 1.0f32, 1.0f32);
    let view = view_at(camera);
    assert!(
        view.eye_in_box[2] < 0.0,
        "precondition: {camera:?} is meant to put the eye under the box floor, \
         and it is at z {}",
        view.eye_in_box[2],
    );

    // The volume in the box's TOP quarter — above the terrain, which is what
    // makes this about occlusion rather than about the underground sliver the
    // control above measures.
    const EDGE: u32 = 8;
    let cells = [EDGE, EDGE, EDGE];
    let mut indices = vec![0u8; (EDGE * EDGE * EDGE) as usize];
    for slab in (EDGE as usize) - 2..EDGE as usize {
        let base = slab * (EDGE * EDGE) as usize;
        indices[base..base + (EDGE * EDGE) as usize].fill(200);
    }

    let painted_with_volume = |occluder: bool| {
        let mut aimed = uniform(cells, &view, RIDGE_AMPLITUDE, occluder);
        aimed.extinction_per_km = 0.15;
        let frame = render(
            &device,
            &queue,
            &pipelines,
            cells,
            &indices,
            &aimed,
            RIDGE_AMPLITUDE,
        );
        let ground_only = expected_ground_pixel();
        let (mut painted, mut carrying) = (0usize, 0usize);
        for pixel in &frame.offscreen {
            if pixel[3] == 0 {
                continue;
            }
            painted += 1;
            if (0..3).any(|lane| i32::from(pixel[lane]).abs_diff(i32::from(ground_only[lane])) > 2)
            {
                carrying += 1;
            }
        }
        (painted, carrying)
    };

    let (lid_painted, lid_carrying) = painted_with_volume(false);
    let (mesh_painted, mesh_carrying) = painted_with_volume(true);
    eprintln!(
        "wall from below {camera:?}: with the lid alone {lid_carrying} of \
         {lid_painted} painted pixels carry volume; with a ground pass \
         {mesh_carrying} of {mesh_painted} do"
    );

    // The lid half: this is `the_floor_is_transparent_from_below`'s claim —
    // `#[ignore]`d, `cargo test -p squallar-gpu --test volume_gpu -- --ignored`
    // — re-measured here so the comparison has a control on this hardware
    // rather than a citation.
    assert!(
        lid_carrying * 2 > lid_painted,
        "with no ground pass only {lid_carrying} of {lid_painted} painted \
         pixels carry volume from below, so the lid is already walling the \
         volume off and this test's comparison has no baseline — that is a \
         regression in `FLOOR_BELOW_FADE`, not in the mesh"
    );
    // And the mesh half, which is the open item.
    assert_eq!(
        mesh_carrying, 0,
        "{mesh_carrying} of {mesh_painted} pixels carry volume through the mesh \
         from below. If the clip has been taught to fade with the coverage, \
         this pin has done its job — REWRITE it to the behaviour that replaced \
         it rather than deleting it, and revisit \
         `the_ground_is_opaque_from_below_the_box_floor`, whose opacity \
         argument rests on this being the only alternative"
    );
}

// ---------------------------------------------------------------------------
// Between the crest and the box floor: the region the plan named, and the
// predicate it named for it.
// ---------------------------------------------------------------------------

/// **Terrain the eye is below still composites BEHIND volume standing in front
/// of it — and the plan's own predicate for this region is what breaks that.**
///
/// The plan asked B2 to generalise the arm to *"the eye is above the ground's
/// maximum height"*, `occluder.y`, on the reasoning that a camera at box
/// z = 0.05 with a ridge at 0.15 in front of it "composites terrain under an
/// accumulation it is in front of". **That reasoning does not survive B1's own
/// clip**, and this test is the measurement rather than the argument.
///
/// `span.y = min(span.y, ground_t)` runs before `jitter` and `dt` and is
/// source-order-pinned there. The mesh is authored inside the unit cube and
/// `slab_entry_exit` floors its entry at 0, so a ray that meets the mesh met
/// the box first and the clipped span never ends short of where it began.
/// Every accumulated sample therefore lies *in front of* the surface, at every
/// pixel, whatever the eye's height — so the ground is behind the accumulation
/// and the "floor behind" arm is the correct one here. Sending this region to
/// the "floor in front" arm instead paints opaque terrain over volume the
/// clip already proved is nearer than it.
///
/// The forced build below is exactly the plan's predicate, and the assertion is
/// two-sided: the shipped arm must keep the volume, and the crest arm must lose
/// it. Without the second half this test would pass on a scene with no volume
/// standing in front of the ridge at all.
#[test]
#[ignore = "needs a real wgpu adapter; see the doc comment for the invocation"]
fn the_terrain_composites_behind_volume_standing_in_front_of_it() {
    use squallar_volumetric::raymarch::VOLUME_SHADER_WGSL;

    /// The shipped arm, asserted to match so a moved anchor cannot make this
    /// force something that does not exist.
    const ARM: &str = "let ground_behind_the_march = eye.z >= 0.0 || volume.occluder.x > 0.0;";
    /// The plan's own generalisation: above the ground's greatest height.
    const CREST: &str = "let ground_behind_the_march = eye.z >= volume.occluder.y;";

    let _held = gpu_lock();
    let (device, queue) = device();
    assert_eq!(
        VOLUME_SHADER_WGSL.matches(ARM).count(),
        1,
        "the composite's arm is no longer `{ARM}`, so this test is forcing \
         something that does not exist — re-anchor it rather than deleting it",
    );

    let format = wgpu::TextureFormat::Rgba8Unorm;
    let shipped = VolumePipelines::new(&device, attachments(format));
    let crest = VolumePipelines::from_shader_source(
        &device,
        attachments(format),
        &VOLUME_SHADER_WGSL.replace(ARM, CREST),
    );
    for pipelines in [&shipped, &crest] {
        pipelines.upload_quad(&queue);
    }

    // Semi-transparent, so a pixel carrying volume AND ground is distinguishable
    // from one carrying ground alone — a saturated ray is white however far it
    // marched, and would hide the very difference this measures.
    let (cells, indices) = floor_slab(2);
    let ground_only = expected_ground_pixel();

    // **Every camera the eye is below the crest at, which is both regions and
    // not just the band above the plane.** Under the box floor the crest
    // predicate is false too, and so is the flat lid's `eye.z >= 0.0` — the arm
    // this replaced. Restricting this to the band would have left the shipped
    // arm's advantage unmeasured at exactly the cameras B1 pinned as a hole.
    let below_the_crest: Vec<Camera> = CAMERAS
        .into_iter()
        .filter(|camera| view_at(*camera).eye_in_box[2] < RIDGE_AMPLITUDE)
        .collect();
    assert_eq!(
        below_the_crest.len(),
        5,
        "the cameras below the crest are what this criterion is defined over, \
         and there are now {}: {below_the_crest:?}",
        below_the_crest.len(),
    );

    let mut total_darkened = 0usize;
    for camera in below_the_crest {
        let view = view_at(camera);
        let z = view.eye_in_box[2];
        let mut aimed = uniform(cells, &view, RIDGE_AMPLITUDE, true);
        aimed.extinction_per_km = 0.15;
        let with_shipped = render(
            &device,
            &queue,
            &shipped,
            cells,
            &indices,
            &aimed,
            RIDGE_AMPLITUDE,
        );
        let with_crest = render(
            &device,
            &queue,
            &crest,
            cells,
            &indices,
            &aimed,
            RIDGE_AMPLITUDE,
        );

        let ground_mask: Vec<bool> = (0..(SIZE[0] * SIZE[1]) as usize)
            .map(|at| with_shipped.ground[at][3] == 255)
            .collect();
        let well_inside = interior(&ground_mask, 1);

        // A pixel carries volume in front of the ground when the shipped
        // composite is not the bare ground colour there. Two codes of slack,
        // the same tolerance the non-invisibility criterion uses.
        let carries_volume = |pixel: [u8; 4]| {
            (0..3).any(|lane| i32::from(pixel[lane]).abs_diff(i32::from(ground_only[lane])) > 2)
        };

        let (mut with_volume, mut darkened, mut kept) = (0usize, 0usize, 0usize);
        for (at, inside) in well_inside.iter().enumerate() {
            if !inside || !carries_volume(with_shipped.offscreen[at]) {
                continue;
            }
            with_volume += 1;
            if carries_volume(with_crest.offscreen[at]) {
                kept += 1;
            } else {
                darkened += 1;
            }
        }
        total_darkened += darkened;

        eprintln!(
            "below-crest arm {camera:?}: eye z {z:.3} against a \
             {RIDGE_AMPLITUDE} crest, {with_volume} px carry volume in front \
             of the ground; the crest predicate loses {darkened} of them and \
             keeps {kept}"
        );
        assert!(
            with_volume > 500,
            "{camera:?}: only {with_volume} pixels carry volume in front of the \
             ground, so this fixture cannot tell the two arms apart and the \
             assertion below would be vacuous"
        );
        // `kept == 0`, which is what the doc claims and what all five cameras
        // measure. A 25% floor stood here first and was 4x looser than the
        // measurement: a criterion whose threshold is nowhere near its own
        // number is not pinning the number.
        assert_eq!(
            kept, 0,
            "{camera:?}: the crest predicate kept {kept} of {with_volume} \
             pixels carrying volume in front of the ground, and the shipped arm \
             keeps all of them. Every camera measured so far loses every one — \
             a survivor means the two arms are not the different composites \
             this test claims, and the shipped arm's advantage here is unproven"
        );
    }
    assert!(
        total_darkened > 0,
        "no camera in `CAMERAS` puts the eye below the crest, so this criterion \
         ran over nothing"
    );
}

// ---------------------------------------------------------------------------
// The mesh itself.
// ---------------------------------------------------------------------------

/// The mesh really is drawn from `@builtin(vertex_index)` alone, at the post
/// count both sides name — checked by the one thing a host can see of it: the
/// occluder's decoded points lie on the analytic surface.
#[test]
#[ignore = "needs a real wgpu adapter; see the doc comment for the invocation"]
fn the_mesh_stands_at_the_height_the_analytic_field_gives_it() {
    let _held = gpu_lock();
    let (device, queue) = device();
    let pipelines = VolumePipelines::new(&device, attachments(wgpu::TextureFormat::Rgba8Unorm));
    pipelines.upload_quad(&queue);

    let (cells, indices) = empty_grid();
    for camera in CAMERAS {
        let view = view_at(camera);
        let uniform = uniform(cells, &view, RIDGE_AMPLITUDE, true);
        let t_scale = uniform.occluder_t_scale;
        let frame = render(
            &device,
            &queue,
            &pipelines,
            cells,
            &indices,
            &uniform,
            RIDGE_AMPLITUDE,
        );

        let mut checked = 0usize;
        let mut worst = 0.0f32;
        for row in 0..SIZE[1] {
            for column in 0..SIZE[0] {
                let at = (row * SIZE[0] + column) as usize;
                if frame.occluder[at][3] < 255 {
                    continue;
                }
                let texel = frame.occluder[at];
                let t = unpack24_bytes([texel[0], texel[1], texel[2]]) * t_scale;
                let p = along(view.eye_in_box, ray(&view, column, row), t);
                worst = worst.max((p[2] - ridge_height([p[0], p[1]], RIDGE_AMPLITUDE)).abs());
                checked += 1;
            }
        }
        assert!(checked > (SIZE[0] * SIZE[1]) as usize / 20);
        // One cell of the grid is `1 / (POSTS - 1)` across, and a chord
        // of a Gaussian across one cell departs from it by at most the
        // curvature over that span — orders under the bound at 512 posts.
        let cell = 1.0 / (POSTS - 1) as f32;
        eprintln!(
            "ground silhouette {camera:?}: {checked} texels, worst height error \
             {worst:.3e} box units against a {cell:.3e} cell"
        );
        assert!(
            worst < cell,
            "{camera:?}: a decoded occluder point sits {worst} box units off \
             the analytic surface, which is more than the {cell} one grid cell \
             spans — the mesh is not standing where the height field puts it"
        );
    }
}

/// **The ground pass's depth attachment resolves a real hidden surface.**
///
/// Every camera above looks down a slope shallow enough that no part of the
/// mesh hides another, so `depth_compare` never decides anything and the
/// `Depth32Float` — 4 of the 16 B/px it charges the budget, and the thing B3's
/// drape and D2's buildings both rest on — is carried untested. Here the
/// vertical exaggeration and a low pitch put the ridge's near flank in front of
/// its far one, and the occluder must carry the NEAR crossing.
#[test]
#[ignore = "needs a real wgpu adapter; see the doc comment for the invocation"]
fn the_ground_pass_keeps_the_nearest_surface_where_one_hides_another() {
    let _held = gpu_lock();
    let (device, queue) = device();
    let pipelines = VolumePipelines::new(&device, attachments(wgpu::TextureFormat::Rgba8Unorm));
    pipelines.upload_quad(&queue);

    // Steep in world terms — the box's 20 km stretched by 3 — and looked at
    // from just above the crest, which is what makes one flank hide the other.
    //
    // **The yaw is load-bearing.** `ridge_height` varies in `u` alone, so a
    // ray travelling along `v` crosses a constant height and never meets the
    // surface twice however steep it is. Yaw 90 puts the eye due east and the
    // rays across the ridge's profile, which is the only direction in which
    // this mesh can hide anything from itself. Measured: yaw 180 gives ZERO
    // pixels with two crossings.
    const AMPLITUDE: f32 = 0.4;
    let camera = (90.0f32, 6.0f32, 1.6f32, 3.0f32);
    let view = view_at(camera);
    assert_above_the_ground(camera, &view, AMPLITUDE);

    let (cells, indices) = empty_grid();
    let uniform = uniform(cells, &view, AMPLITUDE, true);
    let t_scale = uniform.occluder_t_scale;
    let frame = render(
        &device, &queue, &pipelines, cells, &indices, &uniform, AMPLITUDE,
    );

    let mut hidden = 0usize;
    let mut wrong = Vec::new();
    for row in 0..SIZE[1] {
        for column in 0..SIZE[0] {
            let at = (row * SIZE[0] + column) as usize;
            if frame.occluder[at][3] < 255 {
                continue;
            }
            let direction = ray(&view, column, row);
            let crossings = surface_crossings(view.eye_in_box, direction, AMPLITUDE);
            if crossings.len() < 2 {
                continue;
            }
            hidden += 1;
            let texel = frame.occluder[at];
            let t = unpack24_bytes([texel[0], texel[1], texel[2]]) * t_scale;
            // The midpoint between the nearest and the next crossing: a
            // fragment that won on `Always` rather than on depth lands past it.
            // Robust to the mesh's own tessellation, which the near/far gap is
            // orders larger than.
            let midpoint = 0.5 * (crossings[0] + crossings[1]);
            if t > midpoint {
                wrong.push((column, row, t, crossings[0], crossings[1]));
            }
        }
    }

    eprintln!(
        "depth resolution {camera:?}: {hidden} pixels have a hidden surface \
         behind the one they show"
    );
    assert!(
        hidden > 200,
        "only {hidden} pixels have two surface crossings, so this scene has \
         almost no hidden surface and the depth test still decides nothing — \
         the fixture, not the pipeline, is what failed"
    );
    assert!(
        wrong.is_empty(),
        "{} of {hidden} pixels carry a `t` past the midpoint between the near \
         and far surfaces, first {:?}. The ground pass is keeping whichever \
         triangle rasterised last rather than whichever is nearest — its depth \
         attachment is not doing its job",
        wrong.len(),
        &wrong[..wrong.len().min(4)],
    );
}
