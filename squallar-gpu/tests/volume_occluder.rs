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
//! **What these criteria do and do not certify.** Everything below except
//! [`the_ground_is_invisible_from_below_the_box_floor`] runs over
//! [`ABOVE_THE_GROUND_CAMERAS`], a set of six, and holds **only** while the eye
//! is above the ground's own crest. It is not a stylistic precondition: below
//! `z = 0` the composite takes neither arm and the mesh's colour is discarded
//! entirely, which the pinned test at the bottom of this file measures and hands
//! to B2 in writing. A single camera would have certified "terrain draws from
//! above" while reading as "terrain draws".
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
    GROUND_POSTS, GROUND_STAND_IN_COLOUR, OffscreenPlan, VolumePipelines, ground_height,
    unpack24_bytes,
};
use squallar_volumetric::uniform::VolumeUniform;

mod gpu_harness;
use gpu_harness::{attachments, device, gpu_lock, opaque_white_lut, read_back};

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

/// **Every camera the criteria below are asserted over, and they are all above
/// the ground.**
///
/// Six, spread over yaw, pitch, standoff and exaggeration — the repo's rule is
/// to arbitrate across four or five diverse sites rather than tune to one, and
/// the reason it applies here is measured rather than assumed: at `distance
/// 2.2` the eye crosses `z = 0` at about pitch −0.9°, so a single positive-pitch
/// fixture certifies one half of one axis.
///
/// The set is deliberately *not* a sweep of the whole drag range. Below the
/// crest the shipped composite is wrong, and that is B2's arm rule to fix; what
/// belongs here is the region B1 claims, asserted widely inside it, with the
/// region it does not claim pinned separately.
const ABOVE_THE_GROUND_CAMERAS: [Camera; 6] = [
    // The original fixture: obliquely down from the south-west.
    (215.0, 28.0, 2.2, 1.0),
    // The other side, closer, low enough that rays cross the ridge at a slant.
    (35.0, 12.0, 1.0, 1.0),
    // Steep and vertically exaggerated — the shipped default look.
    (140.0, 60.0, 0.8, 3.0),
    // Grazing, and the nearest to the plane this set goes: eye z ~ 3.1.
    (300.0, 8.0, 2.2, 1.0),
    // Near-overhead, where the ridge's silhouette is at its smallest.
    (0.0, 85.0, 1.5, 1.0),
    // Inside the box at the zoom stop, eye z ~ 0.70 — above the crest, but only
    // just, and with the near plane much closer than anywhere else here.
    (75.0, 28.0, 0.05, 1.0),
];

fn view_at((yaw, pitch, distance, exaggeration): Camera) -> VolumeView {
    let camera =
        OrbitCamera::restore(yaw, pitch, distance, [0.0; 3], exaggeration).expect("finite camera");
    view_for(camera, BOX_KM, SIZE[0] as f32 / SIZE[1] as f32).expect("a view")
}

/// The precondition every criterion below depends on, asserted rather than
/// assumed: a fixture that drifted under the ground would make the test measure
/// the hole instead of the property.
fn assert_above_the_ground(camera: Camera, view: &VolumeView, ridge: f32) {
    assert!(
        view.eye_in_box[2] > ridge,
        "camera {camera:?} puts the eye at box z {} , which is not above the \
         {ridge} ridge it is meant to be looking down on. Below the crest the \
         composite's arm is B2's to fix and this criterion does not hold — see \
         `the_ground_is_invisible_from_below_the_box_floor`",
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
    // function of the geometry rather than of a normal.
    uniform.ambient = 1.0;
    uniform.gradient_shading = false;
    // Held constant across every pair below: with the mirror out of the picture,
    // the only ground in the frame is the mesh's own.
    uniform.map_floor = false;
    if occluder {
        // Through the blessed setter, which derives the scale from THIS
        // uniform's eye — the two are independent lanes and a scale from
        // another eye mis-clips every ray.
        uniform.aim_occluder(ridge, ridge);
    } else {
        uniform.ground_ridge_amplitude = ridge;
        uniform.ground_max_z = ridge;
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

fn render(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    pipelines: &VolumePipelines,
    cells: [u32; 3],
    indices: &[u8],
    uniform: &VolumeUniform,
) -> Frame {
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
    pipelines.encode_ground(&mut encoder, &target, &volume);
    pipelines.encode_raymarch_with_floor(&mut encoder, &target, &volume, None);
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
        p[2] - ground_height([p[0], p[1]], amplitude)
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

/// The ground's straight linear colour as the offscreen must hold it:
/// gamma-encoded and premultiplied by a coverage of exactly 1.
fn expected_ground_pixel() -> [u8; 3] {
    GROUND_STAND_IN_COLOUR.map(|linear| {
        let gamma = if linear <= 0.003_130_8 {
            linear * 12.92
        } else {
            1.055 * linear.powf(1.0 / 2.4) - 0.055
        };
        (gamma * 255.0).round() as u8
    })
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

    for camera in ABOVE_THE_GROUND_CAMERAS {
        let view = view_at(camera);
        assert_above_the_ground(camera, &view, 0.0);
        let (cells, indices) = empty_grid();
        // Flat: amplitude zero, so the mesh and `floor_hit` describe one surface.
        let uniform = uniform(cells, &view, 0.0, true);
        let t_scale = uniform.occluder_t_scale;
        let frame = render(&device, &queue, &pipelines, cells, &indices, &uniform);

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

/// **The mesh clips the march, and over the region B1 claims it only ever
/// removes volume from a ray.**
///
/// Two frames through the production pair, with the ground's colour
/// contribution held constant across them — flat ground against a ridge, both
/// with the occluder on. That is the generalisation of the plan's own rule that
/// `map_floor` must not differ across the pair: if the ground contributes in one
/// frame and not the other, the difference measured is the ground appearing, not
/// the volume being clipped.
///
/// **The direction is not universal and this doc used to say it was.** It holds
/// while the eye is above the ground; from below the plane the composite takes
/// neither arm and the sign inverts on every differing pixel. See
/// [`the_ground_is_invisible_from_below_the_box_floor`], which is `#[ignore]`d
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

    for camera in ABOVE_THE_GROUND_CAMERAS {
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
        );
        let ridged = render(
            &device,
            &queue,
            &pipelines,
            cells,
            &indices,
            &semi_transparent(RIDGE_AMPLITUDE),
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
    for camera in ABOVE_THE_GROUND_CAMERAS {
        let view = view_at(camera);
        let off = |ridge: f32| {
            let mut u = uniform(cells, &view, ridge, false);
            u.extinction_per_km = 0.15;
            u
        };
        let flat = render(&device, &queue, &pipelines, cells, &indices, &off(0.0));
        let ridged = render(
            &device,
            &queue,
            &pipelines,
            cells,
            &indices,
            &off(RIDGE_AMPLITUDE),
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

/// **The ground's colour survives the raymarch pass, at every camera above it.**
///
/// This exists because the control above cannot see a terrain that never draws:
/// a correctly clipped volume over an empty background differs in exactly the
/// direction a control asserts, so the flagship test passes on a build with no
/// visible ground at all. Here the volume is air, so anything on the screen
/// inside the ridge's silhouette came from the mesh.
///
/// It is asserted over six cameras rather than one because at one it certified
/// "terrain draws **from above**" while reading as "terrain draws" — the exact
/// vacuity this criterion was added to close, one level up.
#[test]
#[ignore = "needs a real wgpu adapter; see the doc comment for the invocation"]
fn the_grounds_own_colour_reaches_the_screen() {
    let _held = gpu_lock();
    let (device, queue) = device();
    let pipelines = VolumePipelines::new(&device, attachments(wgpu::TextureFormat::Rgba8Unorm));
    pipelines.upload_quad(&queue);

    let expected = expected_ground_pixel();
    let (cells, indices) = empty_grid();

    for camera in ABOVE_THE_GROUND_CAMERAS {
        let view = view_at(camera);
        assert_above_the_ground(camera, &view, RIDGE_AMPLITUDE);
        let lit = uniform(cells, &view, RIDGE_AMPLITUDE, true);
        let dark = uniform(cells, &view, RIDGE_AMPLITUDE, false);
        let lit = render(&device, &queue, &pipelines, cells, &indices, &lit);
        let dark = render(&device, &queue, &pipelines, cells, &indices, &dark);

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
            "{camera:?}: {opaque} pixels are opaque with the occluder off over \
             an empty grid, so the colour asserted above did not have to come \
             from the mesh"
        );
    }
}

// ---------------------------------------------------------------------------
// The limit of what B1 certifies, measured rather than left implicit.
// ---------------------------------------------------------------------------

/// **From below the box floor the ground is entirely invisible, and B2 owns
/// the fix.**
///
/// `let eye_above_plane = eye.z >= 0.0` is a predicate written for a *flat*
/// ground at `z = 0`, and `floor_fade = clamp(1 + eye.z / FLOOR_BELOW_FADE)`
/// with `FLOOR_BELOW_FADE = 0.08` is zero for any eye more than 0.08 box
/// heights under the plane. Below the plane neither composite arm runs,
/// `surface_colour` is never called, and the mesh's colour is discarded — while
/// the *clip* still applies, so the volume is cut by ground that is not drawn.
///
/// **This is an ordinary camera, not a corner.** `MAX_PITCH_DEG = 89.0` and
/// pitch is clamped to `[-89, +89]`, and at the default standoff the eye
/// crosses `z = 0` at about pitch −0.9° — so essentially the whole
/// negative-pitch half of the drag range is in here. Measured across pitch at
/// `distance 2.2`: −18° gives `eye_in_box.z = -5.27`, −40° gives −11.50, −89°
/// gives −18.16, and `floor_fade` is 0 at every one of them.
///
/// It is pinned rather than fixed because **generalising that predicate is B2's
/// whole work unit**, and B2 as planned reserves only "below the terrain
/// surface but above `z = 0`" — below `z = 0` *with a mesh* is a case neither
/// unit names. When B2 lands, this test fails and must be rewritten to the
/// behaviour that replaces it, not deleted.
#[test]
#[ignore = "needs a real wgpu adapter; see the doc comment for the invocation"]
fn the_ground_is_invisible_from_below_the_box_floor() {
    let _held = gpu_lock();
    let (device, queue) = device();
    let pipelines = VolumePipelines::new(&device, attachments(wgpu::TextureFormat::Rgba8Unorm));
    pipelines.upload_quad(&queue);

    // The reviewer's own camera, and two more spread across the negative half.
    for camera in [
        (215.0f32, -18.0f32, 2.2f32, 1.0f32),
        (35.0, -40.0, 2.2, 1.0),
        (140.0, -89.0, 1.0, 1.0),
    ] {
        let view = view_at(camera);
        assert!(
            view.eye_in_box[2] < 0.0,
            "precondition: {camera:?} was meant to put the eye under the box \
             floor, and it is at z {}",
            view.eye_in_box[2]
        );

        let (cells, indices) = empty_grid();
        let lit = uniform(cells, &view, RIDGE_AMPLITUDE, true);
        let frame = render(&device, &queue, &pipelines, cells, &indices, &lit);

        let drew = (0..(SIZE[0] * SIZE[1]) as usize)
            .filter(|at| frame.ground[*at][3] == 255)
            .count();
        assert!(
            drew > (SIZE[0] * SIZE[1]) as usize / 20,
            "{camera:?}: the mesh drew over only {drew} pixels, so this pin is \
             not measuring the hole it names"
        );
        let invisible = (0..(SIZE[0] * SIZE[1]) as usize)
            .filter(|at| frame.ground[*at][3] == 255 && frame.offscreen[*at][3] == 0)
            .count();
        eprintln!(
            "below-plane hole {camera:?}: eye z {:.3}, mesh drew {drew} px, \
             {invisible} of them composited to alpha 0",
            view.eye_in_box[2]
        );
        assert_eq!(
            invisible,
            drew,
            "{camera:?}: {} of {drew} pixels the mesh drew now reach the \
             screen. If B2's arm rule has landed, this pin has done its job — \
             REWRITE it to the behaviour that replaced it rather than deleting \
             it, and widen `ABOVE_THE_GROUND_CAMERAS` to cover the range that \
             now works",
            drew - invisible,
        );
    }
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
    for camera in ABOVE_THE_GROUND_CAMERAS {
        let view = view_at(camera);
        let uniform = uniform(cells, &view, RIDGE_AMPLITUDE, true);
        let t_scale = uniform.occluder_t_scale;
        let frame = render(&device, &queue, &pipelines, cells, &indices, &uniform);

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
                worst = worst.max((p[2] - ground_height([p[0], p[1]], RIDGE_AMPLITUDE)).abs());
                checked += 1;
            }
        }
        assert!(checked > (SIZE[0] * SIZE[1]) as usize / 20);
        // One cell of the grid is `1 / (GROUND_POSTS - 1)` across, and a chord
        // of a Gaussian across one cell departs from it by at most the
        // curvature over that span — orders under the bound at 512 posts.
        let cell = 1.0 / (GROUND_POSTS - 1) as f32;
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
    // **The yaw is load-bearing.** `ground_height` varies in `u` alone, so a
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
    let frame = render(&device, &queue, &pipelines, cells, &indices, &uniform);

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
