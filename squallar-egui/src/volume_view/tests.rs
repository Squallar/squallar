use super::*;
use squallar_radar::fields as radar_fields;

const BOX_KM: [f32; 3] = [240.0, 240.0, 18.0];

/// A camera aimed at the box's centre with no vertical stretch — true
/// proportions, which is what every matrix test below is written against.
fn camera(yaw: f32, pitch: f32, distance: f32) -> OrbitCamera {
    OrbitCamera::restore(yaw, pitch, distance, [0.0; 3], 1.0).expect("finite camera")
}

/// Apply a column-major matrix to a homogeneous point and divide through,
/// exactly as `unproject` in the shader does.
fn unproject(m: Mat4, ndc: [f32; 3]) -> [f32; 3] {
    let p = [ndc[0], ndc[1], ndc[2], 1.0];
    let mut out = [0.0f32; 4];
    for (r, slot) in out.iter_mut().enumerate() {
        *slot = (0..4).map(|k| m[k][r] * p[k]).sum();
    }
    [out[0] / out[3], out[1] / out[3], out[2] / out[3]]
}

fn direction(view: &VolumeView, ndc: [f32; 2]) -> [f32; 3] {
    let far = unproject(view.box_from_clip, [ndc[0], ndc[1], 1.0]);
    normalize([
        far[0] - view.eye_in_box[0],
        far[1] - view.eye_in_box[1],
        far[2] - view.eye_in_box[2],
    ])
    .expect("a ray with a direction")
}

/// The centre of the screen looks at the centre of the box.
#[test]
fn the_centre_of_the_screen_looks_at_the_centre_of_the_box() {
    for (yaw, pitch) in [(0.0, 0.0), (225.0, 25.0), (37.0, -80.0), (359.0, 89.0)] {
        let view = view_for(camera(yaw, pitch, 2.5), BOX_KM, 1.6).expect("a view");
        let ray = direction(&view, [0.0, 0.0]);
        // The centre of box space is (0.5, 0.5, 0.5); the eye is somewhere
        // outside. The ray from eye to centre is the one the middle pixel
        // must cast.
        let wanted = normalize([
            0.5 - view.eye_in_box[0],
            0.5 - view.eye_in_box[1],
            0.5 - view.eye_in_box[2],
        ])
        .expect("a direction to the centre");
        for axis in 0..3 {
            assert!(
                (ray[axis] - wanted[axis]).abs() < 1e-4,
                "yaw {yaw} pitch {pitch}: centre ray {ray:?} does not point at the box \
                     centre ({wanted:?})",
            );
        }
    }
}

/// A camera zoomed all the way in stands *inside* the box and still gets a
/// view: finite matrices, an eye in the unit cube, and the centre ray on
/// the pivot.
#[test]
fn a_camera_at_the_zoom_stop_is_inside_the_box_and_still_has_a_view() {
    for exaggeration in [1.0, 12.0] {
        let mut camera =
            OrbitCamera::restore(225.0, 25.0, 2.5, [0.0; 3], exaggeration).expect("finite");
        camera.nudge(crate::pane::OrbitDelta {
            zoom_factor: 1e6,
            ..Default::default()
        });
        let view = view_for(camera, BOX_KM, 1.6)
            .expect("the zoom's near stop must still be a viewable camera");
        assert!(
            view.eye_in_box.iter().all(|c| (0.0..=1.0).contains(c)),
            "at {exaggeration}x the fully-zoomed eye should be inside the \
                 box, got {:?}",
            view.eye_in_box,
        );
        assert!(
            view.box_from_clip
                .iter()
                .flatten()
                .all(|value| value.is_finite()),
            "at {exaggeration}x the inside-the-box view built a non-finite \
                 matrix",
        );
        // The orbit still aims at the pivot from inside: the centre ray
        // reaches the box's centre, exactly as it does from outside.
        let ray = direction(&view, [0.0, 0.0]);
        let wanted = normalize([
            0.5 - view.eye_in_box[0],
            0.5 - view.eye_in_box[1],
            0.5 - view.eye_in_box[2],
        ])
        .expect("a direction to the centre");
        for axis in 0..3 {
            assert!(
                (ray[axis] - wanted[axis]).abs() < 1e-3,
                "at {exaggeration}x the inside centre ray {ray:?} is off the \
                     pivot ({wanted:?})",
            );
        }
    }
}

/// Yaw is a compass bearing of the *eye*, so the default camera is to the
/// south-west of the box exactly as [`OrbitCamera::default`] promises.
#[test]
fn yaw_is_the_compass_bearing_of_the_eye_from_the_box() {
    let view = view_for(OrbitCamera::default(), BOX_KM, 1.0).expect("a view");
    assert!(
        view.eye_km[0] < 0.0 && view.eye_km[1] < 0.0,
        "the default camera should sit south-west of the box, not at {:?}",
        view.eye_km,
    );
    assert!(view.eye_km[2] > 0.0, "a positive pitch is above the box");

    for (yaw, axis, sign) in [
        (0.0, 1, 1.0),
        (90.0, 0, 1.0),
        (180.0, 1, -1.0),
        (270.0, 0, -1.0),
    ] {
        let view = view_for(camera(yaw, 0.0, 2.0), BOX_KM, 1.0).expect("a view");
        assert!(
            view.eye_km[axis] * sign > 0.0,
            "yaw {yaw} should put the eye on axis {axis} sign {sign}, got {:?}",
            view.eye_km,
        );
    }
}

/// The box is *not* stretched to a cube: a 240 x 240 x 18 km box keeps its
/// proportions.
#[test]
fn the_box_keeps_its_true_proportions() {
    let view = view_for(camera(180.0, 0.0, 2.0), BOX_KM, 1.0).expect("a view");
    // Box space is the unit cube whatever the physical extent, so the proof
    // has to be in world kilometres: the eye distance is set from the
    // *physical* box's north extent, which a normalised cube would not have.
    let distance = (view.eye_km[0] * view.eye_km[0]
        + view.eye_km[1] * view.eye_km[1]
        + view.eye_km[2] * view.eye_km[2])
        .sqrt();
    let radius = 240.0f32 / std::f32::consts::SQRT_2;
    assert!(
        (distance - 2.0 * radius).abs() < 1e-2,
        "eye at {distance} km is not 2.0 framing radii ({radius} km) out",
    );
    // And the eye in box space is *not* on a sphere: the z axis is 13x
    // shorter, so two framing radii of z is far more of the box's height
    // than of its width.
    let dz = (view.eye_in_box[2] - 0.5).abs();
    let dy = (view.eye_in_box[1] - 0.5).abs();
    assert!(
        dy > dz,
        "a level camera should be displaced in y, not z: {:?}",
        view.eye_in_box,
    );
}

/// The near and far planes do not move a ray.
#[test]
fn the_frustum_depth_range_does_not_move_a_ray() {
    let camera = camera(225.0, 25.0, 2.5);
    let shallow = build_view(camera, BOX_KM, 1.6, 1.0, 3_000.0).expect("a view");
    let deep = build_view(camera, BOX_KM, 1.6, 20.0, 60_000.0).expect("a view");
    assert_ne!(
        shallow.box_from_clip, deep.box_from_clip,
        "precondition: the two frustums must actually differ",
    );
    for ndc in [[0.0, 0.0], [-1.0, -1.0], [0.9, -0.3]] {
        let want = direction(&shallow, ndc);
        let got = direction(&deep, ndc);
        for axis in 0..3 {
            assert!(
                (got[axis] - want[axis]).abs() < 1e-3,
                "ndc {ndc:?}: a 20x deeper frustum moved the ray from {want:?} to {got:?}",
            );
        }
    }
}

/// A wider viewport spreads the rays horizontally and leaves the vertical
/// field of view alone. That is what `aspect` means, and dividing by it
/// instead of multiplying is the mistake that squashes a 3D pane in a split
/// layout while looking perfect in a square one.
#[test]
fn aspect_widens_the_horizontal_field_of_view_only() {
    let camera = camera(0.0, 0.0, 3.0);
    let square = view_for(camera, BOX_KM, 1.0).expect("a view");
    let wide = view_for(camera, BOX_KM, 2.0).expect("a view");

    let horizontal = |v: &VolumeView| {
        let centre = direction(v, [0.0, 0.0]);
        let edge = direction(v, [1.0, 0.0]);
        dot(centre, edge)
    };
    let vertical = |v: &VolumeView| {
        let centre = direction(v, [0.0, 0.0]);
        let edge = direction(v, [0.0, 1.0]);
        dot(centre, edge)
    };

    assert!(
        horizontal(&wide) < horizontal(&square),
        "doubling the aspect should widen the horizontal field of view",
    );
    assert!(
        (vertical(&wide) - vertical(&square)).abs() < 1e-6,
        "the vertical field of view must not depend on the aspect",
    );
}

fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

/// Every degenerate input is refused, not clamped.
#[test]
fn a_box_or_a_viewport_that_cannot_be_looked_at_is_refused() {
    let camera = OrbitCamera::default();
    for bad in [
        [0.0, 240.0, 18.0],
        [240.0, 0.0, 18.0],
        [240.0, 240.0, 0.0],
        [-240.0, 240.0, 18.0],
        [f32::NAN, 240.0, 18.0],
        [f32::INFINITY, 240.0, 18.0],
    ] {
        assert!(
            view_for(camera, bad, 1.0).is_none(),
            "box {bad:?} should have no view",
        );
    }
    for bad in [0.0, -1.0, f32::NAN, f32::INFINITY] {
        assert!(
            view_for(camera, BOX_KM, bad).is_none(),
            "aspect {bad} should have no view",
        );
    }
}

/// The multiplication is column-major and in that order.
#[test]
fn the_matrix_product_is_column_major() {
    // A pure translate by (1,2,3) and a pure scale by 2.
    let translate: Mat4 = [
        [1.0, 0.0, 0.0, 0.0],
        [0.0, 1.0, 0.0, 0.0],
        [0.0, 0.0, 1.0, 0.0],
        [1.0, 2.0, 3.0, 1.0],
    ];
    let scale: Mat4 = [
        [2.0, 0.0, 0.0, 0.0],
        [0.0, 2.0, 0.0, 0.0],
        [0.0, 0.0, 2.0, 0.0],
        [0.0, 0.0, 0.0, 1.0],
    ];
    // scale · translate scales the translation; translate · scale does not.
    assert_eq!(multiply(scale, translate)[3], [2.0, 4.0, 6.0, 1.0]);
    assert_eq!(multiply(translate, scale)[3], [1.0, 2.0, 3.0, 1.0]);
}

/// **The registration guarantee itself.** `clip_from_box` is built forward
/// and `box_from_clip` backward, from the same camera; their product being the
/// identity is what says the ground mesh and the march that occludes against
/// it are looking through one camera rather than two that agree by inspection.
#[test]
fn the_forward_and_backward_cameras_are_exact_inverses() {
    // Every camera the other matrix tests use, plus the zoom stop, plus an
    // exaggerated box — the residual an inversion carries is largest where the
    // matrix is worst conditioned, which is the flattest box and the nearest eye.
    let cameras = [
        (0.0f32, 0.0f32, 2.5f32, 1.0f32, 1.6f32),
        (225.0, 25.0, 2.5, 1.0, 1.6),
        (37.0, -80.0, 1.2, 3.0, 0.4),
        (359.0, 89.0, 0.05, 3.0, 3.2),
    ];
    let mut worst = 0.0f32;
    let mut worst_round_trip = 0.0f32;
    for (yaw, pitch, distance, exaggeration, aspect) in cameras {
        let camera = OrbitCamera::restore(yaw, pitch, distance, [0.0; 3], exaggeration)
            .expect("finite camera");
        let view = view_for(camera, BOX_KM, aspect).expect("a view");
        let product = multiply(view.clip_from_box, view.box_from_clip);
        for (column, values) in product.iter().enumerate() {
            for (row, value) in values.iter().enumerate() {
                let expected = if column == row { 1.0 } else { 0.0 };
                worst = worst.max((value - expected).abs());
            }
        }

        // And the property the two matrices are actually used for: a point in
        // the box, projected forward by the ground pass's matrix and
        // unprojected back by the march's, returns to itself.
        for corner in 0..8u32 {
            for offset in [0.0f32, 0.37] {
                let p = [
                    (corner & 1) as f32 * (1.0 - 2.0 * offset) + offset,
                    ((corner >> 1) & 1) as f32 * (1.0 - 2.0 * offset) + offset,
                    ((corner >> 2) & 1) as f32 * (1.0 - 2.0 * offset) + offset,
                ];
                let mut clip = [0.0f32; 4];
                for (r, slot) in clip.iter_mut().enumerate() {
                    *slot = (0..3).map(|k| view.clip_from_box[k][r] * p[k]).sum::<f32>()
                        + view.clip_from_box[3][r];
                }
                // Behind the eye: no pixel, so nothing to register.
                if clip[3] <= 1e-4 {
                    continue;
                }
                let ndc = [clip[0] / clip[3], clip[1] / clip[3], clip[2] / clip[3]];
                let back = unproject(view.box_from_clip, ndc);
                for axis in 0..3 {
                    worst_round_trip = worst_round_trip.max((back[axis] - p[axis]).abs());
                }
            }
        }
    }

    eprintln!(
        "camera inverse: worst entry {worst:e}, worst point round trip \
         {worst_round_trip:e} box units"
    );

    // **The bound is what `f32` allows, not what the derivation deserves.**
    // The two matrices are each a product of three factors whose entries run to
    // the box's own kilometres and the focal length — order 10^2 to 10^3 — so
    // the identity's off-diagonal zeros are differences of numbers that size,
    // and one ulp of those is ~1e-5 absolute. What this rules out is a matrix
    // that is *structurally* wrong — transposed, wrong-signed, built at another
    // aspect — and those miss by order 1, five orders past this. The control
    // below is what shows the bound is still narrow enough to catch one.
    assert!(
        worst < 1e-4,
        "clip_from_box · box_from_clip is off the identity by {worst}; the two \
         cameras have stopped being one derivation, and occlusion registers a \
         pixel out"
    );
    // And the property the ground pass actually depends on: a point in the box
    // projected forward and unprojected back returns to itself. In box units
    // against the box's own extent — at the 256-pixel offscreen these suites
    // render, the box is about 940 m to the pixel and this bound is under a
    // tenth of one.
    assert!(
        worst_round_trip < 1e-3,
        "a box point came back {worst_round_trip} box units away from itself \
         through the two cameras — the mesh and the march would draw the same \
         surface in two places"
    );
}

/// And the identity is not vacuous: a matrix that is *not* the inverse fails it.
#[test]
fn the_inverse_check_would_notice_a_camera_that_did_not_match() {
    let camera = OrbitCamera::restore(225.0, 25.0, 2.5, [0.0; 3], 1.0).expect("finite camera");
    let view = view_for(camera, BOX_KM, 1.6).expect("a view");
    // The same camera at a different aspect — the kind of near-miss an
    // independently-derived forward matrix would produce.
    let other = view_for(camera, BOX_KM, 1.61).expect("a view");
    let product = multiply(other.clip_from_box, view.box_from_clip);
    let worst = (0..4)
        .flat_map(|c| (0..4).map(move |r| (c, r)))
        .map(|(c, r)| (product[c][r] - if c == r { 1.0 } else { 0.0 }).abs())
        .fold(0.0f32, f32::max);
    assert!(
        worst > 1e-3,
        "a camera built at a different aspect still read as the inverse (worst \
         {worst}), so the identity above proves nothing"
    );
}

/// The stub painter is not a substitute for the frontend's downcast test.
#[test]
fn the_stub_payload_is_the_kind_egui_wgpu_discards_in_silence() {
    let painter = StubVolumePainter::painting();
    let frame = VolumeFrameState {
        pane_idx: 0,
        target: VolumeTarget {
            region: None,
            volume: crate::pane::VolumeStamp {
                site: "KTLX".to_owned(),
                collected: chrono::NaiveDate::from_ymd_opt(2024, 5, 6)
                    .unwrap()
                    .and_hms_opt(22, 0, 0)
                    .unwrap(),
            },
            product: radar_fields::known::REFLECTIVITY,
        },
        camera: OrbitCamera::default(),
        size_px: [800, 600],
        pixels_per_point: 1.0,
        floor: true,
        source: None,
        mirror_size_points: [800.0, 600.0],
        alpha: None,
        view_mode: crate::pane::VolumeViewMode::LitVolume,
        iso_threshold: 18.0,
        light: crate::volume_view::VolumeLight::Headlight,
        heights: None,
    };
    let VolumePaint::Callback { payload, .. } = painter.paint(&frame) else {
        panic!("the painting stub must paint");
    };
    assert!(
        payload.downcast_ref::<StubPayload>().is_some(),
        "the stub's payload is its own type, which nothing in egui_wgpu can draw — \
             the real payload's downcast is pinned in squallar-volumetric by \
             `the_payload_the_painter_hands_over_is_one_egui_wgpu_can_draw`",
    );
    assert_eq!(painter.seen.lock().unwrap().len(), 1);
}

// --- Vertical exaggeration ---------------------------------------------

/// The exaggeration stretches the box's geometry and moves no cell within
/// it.
#[test]
fn exaggeration_stretches_the_world_and_moves_no_cell_in_the_box() {
    for ex in [1.0, 3.0, 12.0] {
        let camera = OrbitCamera::restore(225.0, 25.0, 2.5, [0.0; 3], ex).expect("finite");
        let view = view_for(camera, BOX_KM, 1.6).expect("a viewable box");
        // The ray through the middle of the pane is aimed at the pivot, which
        // is the box's centre — box space (0.5, 0.5, 0.5) — whatever the
        // stretch.
        let eye = view.eye_in_box;
        let dir = direction(&view, [0.0, 0.0]);
        let t = (0.5 - eye[2]) / dir[2];
        let hit = [eye[0] + dir[0] * t, eye[1] + dir[1] * t, 0.5];
        assert!(
            (hit[0] - 0.5).abs() < 1e-3 && (hit[1] - 0.5).abs() < 1e-3,
            "at {ex}x the centre ray must still reach the box's centre, got {hit:?}",
        );
    }
}

/// Stretching the box does not move the eye, so the ground keeps its scale as
/// the knob turns and only heights grow.
#[test]
fn the_exaggeration_knob_does_not_move_the_eye() {
    let length = |v: [f32; 3]| (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    // Both ends of the knob's travel, on a whole-scan box and on one at the
    // resampler's floor: the old expression was off by 2% at one and by 550%
    // at the other, so a single box size would have missed it.
    for box_km in [[651.0, 651.0, 18.0], [20.0, 20.0, 18.0]] {
        let flat = OrbitCamera::restore(225.0, 25.0, 2.5, [0.0; 3], 1.0).expect("finite");
        let at_1x = view_for(flat, box_km, 1.6).expect("viewable").eye_km;
        for exaggeration in [3.0, 6.0, 12.0] {
            let camera =
                OrbitCamera::restore(225.0, 25.0, 2.5, [0.0; 3], exaggeration).expect("finite");
            let stretched = view_for(camera, box_km, 1.6).expect("viewable").eye_km;
            assert!(
                (length(stretched) - length(at_1x)).abs() < 1e-3,
                "a {box_km:?} box at {exaggeration}x moved the eye from \
                 {} km to {} km, so the knob rescaled the ground",
                length(at_1x),
                length(stretched),
            );
        }
    }
}

/// Widening the box does not push the eye back: a wider pane shows more ground
/// at the same scale, not the same ground smaller.
#[test]
fn widening_the_box_does_not_push_the_eye_back() {
    let length = |v: [f32; 3]| (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    let camera = OrbitCamera::default();
    let square = view_for(camera, [460.0, 460.0, 18.0], 1.0).expect("viewable");
    // Every aspect a column can take, from the 0.15 `MIN_RATIO` clamp on a
    // wide window up past 16:9 — the property has to hold continuously, not
    // only at the one ratio the complaint arrived at.
    for east_km in [70.0f32, 230.0, 460.0, 818.0, 1104.0, 3066.0] {
        let aspect = east_km / 460.0;
        let wide = view_for(camera, [east_km, 460.0, 18.0], aspect).expect("viewable");
        assert!(
            (length(wide.eye_km) - length(square.eye_km)).abs() < 1e-2,
            "a box {east_km} km east moved the eye from {} km to {} km; the \
             pane's width must not set the scale",
            length(square.eye_km),
            length(wide.eye_km),
        );
    }
}

/// Zooming the ground moves the picture by exactly what was zoomed.
#[test]
fn the_zoom_is_proportional_to_the_ground_it_zoomed() {
    let length = |v: [f32; 3]| (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    let camera = OrbitCamera::default();
    let standoff = |half_km: f32| {
        let box_km = [2.0 * half_km, 2.0 * half_km, 18.0];
        length(view_for(camera, box_km, 1.6).expect("viewable").eye_km)
    };
    // The resampler's ceiling for a square ask down to `MIN_HALF_WIDTH_KM`.
    let steps = [325.3f32, 160.0, 80.0, 40.0, 20.0, 10.0];
    for pair in steps.windows(2) {
        let (wide, tight) = (pair[0], pair[1]);
        let ground = wide / tight;
        let picture = standoff(wide) / standoff(tight);
        assert!(
            (picture / ground - 1.0).abs() < 1e-3,
            "zooming {wide} km to {tight} km is {ground}x of ground and came \
             back as {picture}x of picture",
        );
    }
    let ends = standoff(steps[0]) / standoff(steps[steps.len() - 1]);
    assert!(
        (ends / (steps[0] / steps[steps.len() - 1]) - 1.0).abs() < 1e-3,
        "over the resampler's whole range the zoom came back as {ends}x",
    );
}

/// The box occupies the same fraction of its pane whatever shape the pane is.
#[test]
fn the_box_fills_the_same_fraction_of_every_pane_shape() {
    let length = |v: [f32; 3]| (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    let camera = OrbitCamera::default();
    let north_km = 460.0f32;
    for aspect in [0.15f32, 0.5, 1.0, 1.778, 2.4, 7.1] {
        let view = view_for(camera, [aspect * north_km, north_km, 18.0], aspect).expect("viewable");
        // What the pane's height spans in kilometres, over the box's own north
        // extent — the box's share of the pane. 1 is the plan pane's own scale;
        // above it the 3D view is zoomed out against the pane it came from and
        // below it zoomed in.
        let spanned = 2.0 * length(view.eye_km) * (0.5 * FOV_Y_DEG.to_radians()).tan();
        assert!(
            (spanned / north_km - 1.0).abs() < 1e-4,
            "at aspect {aspect} the pane spans {spanned} km over a {north_km} km \
             box, {}x rather than 1x",
            spanned / north_km,
        );
    }
}

/// Converting a plan pane to 3D is exactly 1:1 — the same ground at the same
/// scale, not a change of viewpoint that is also a zoom.
#[test]
fn converting_a_plan_pane_to_3d_keeps_its_ground_scale() {
    // The ground the pane's width covers, in kilometres, read out of the view's
    // own matrix.
    let ground_across_pane = |view: &VolumeView, box_km: [f32; 3]| {
        let hit = |ndc_x: f32| {
            let ray = direction(view, [ndc_x, 0.0]);
            // The pivot is the box's centre here, so its plane is box z = 0.5.
            // Solved in box space, where the ray is: the vertical axis is scaled
            // differently from the other two, but the crossing is the same one.
            let steps = (0.5 - view.eye_in_box[2]) / ray[2];
            [
                (view.eye_in_box[0] + steps * ray[0] - 0.5) * box_km[0],
                (view.eye_in_box[1] + steps * ray[1] - 0.5) * box_km[1],
            ]
        };
        let (left, right) = (hit(-1.0), hit(1.0));
        ((right[0] - left[0]).powi(2) + (right[1] - left[1]).powi(2)).sqrt()
    };

    for (yaw, pitch) in [(225.0, 25.0), (0.0, 25.0), (90.0, 60.0), (315.0, 88.0)] {
        for exaggeration in [1.0f32, 3.0, 12.0] {
            let camera = OrbitCamera::restore(
                yaw,
                pitch,
                OrbitCamera::default().eye_distance(),
                [0.0; 3],
                exaggeration,
            )
            .expect("finite");
            // The resampler's ceiling for a square ask down to its 10 km floor.
            for north_km in [650.6f32, 460.0, 80.0, 20.0] {
                for aspect in [0.15f32, 0.5, 1.0, 1.778, 2.4, 7.1] {
                    // The box a plan pane of this shape cuts out of the ground:
                    // its north extent from the pane's height, its east extent
                    // from the pane's width.
                    let box_km = [aspect * north_km, north_km, 18.0];
                    let view = view_for(camera, box_km, aspect).expect("viewable");
                    // The plan pane draws `north_km` over its height, so it
                    // draws `aspect * north_km` across its width.
                    let plan_km = aspect * north_km;
                    let drawn_km = ground_across_pane(&view, box_km);
                    assert!(
                        (drawn_km / plan_km - 1.0).abs() < 1e-3,
                        "at yaw {yaw} pitch {pitch} {exaggeration}x, a {north_km} km \
                         box in a pane of aspect {aspect} draws {drawn_km} km of ground \
                         across its width where the plan pane it was made from draws \
                         {plan_km} km — {}x, and 1x is what converting a pane means",
                        drawn_km / plan_km,
                    );
                }
            }
        }
    }
}

/// A fresh 3D pane asks the mirror for exactly one texel a pixel.
#[test]
fn a_fresh_pane_asks_the_mirror_for_one_texel_a_pixel() {
    let box_km = [460.0f32, 460.0, 18.0];
    for lat in [25.0f64, 41.7311, 60.0] {
        // A plan pane 900 points tall showing the box's north extent over that
        // height, expressed the way `MapPaneGeo` carries it.
        let points_per_degree_lon =
            (900.0 / 460.0) * squallar_geo::KM_PER_DEGREE_LAT * lat.to_radians().cos();
        let magnification = floor_magnification(
            OrbitCamera::default(),
            box_km,
            900.0,
            points_per_degree_lon,
            lat,
        )
        .expect("a fresh pane on a real affine must produce a demand");
        assert!(
            (magnification - 1.0).abs() < 1e-4,
            "a fresh pane at {lat} N magnifies its own plan pane's floor by \
             {magnification}x; 1x is what the default standoff is derived to give",
        );
    }
}

/// Only the vertical axis is stretched.
#[test]
fn exaggeration_touches_only_the_vertical_axis() {
    let camera = OrbitCamera::restore(225.0, 25.0, 2.5, [0.0; 3], 4.0).expect("finite");
    assert_eq!(
        exaggerated_box_km(camera, BOX_KM),
        [BOX_KM[0], BOX_KM[1], BOX_KM[2] * 4.0],
    );
}

// --- Panning ------------------------------------------------------------

/// The box follows the pointer: dragging right carries it right.
#[test]
fn the_box_follows_the_pointer_when_the_view_is_panned() {
    for exaggeration in [1.0f32, 3.0, 12.0] {
        // Due south of the box looking north, so screen-right is due east and
        // screen-up is due up: the two axes are separable and nameable.
        let start = OrbitCamera::restore(180.0, 0.0, 2.5, [0.0; 3], exaggeration).expect("finite");

        let mut right_drag = start;
        right_drag.nudge(crate::pane::OrbitDelta {
            pan: pan_for_drag(start, BOX_KM, 900.0, [100.0, 0.0]).expect("a pannable view"),
            ..Default::default()
        });
        assert!(
            right_drag.pivot()[0] < -1e-4,
            "at {exaggeration}x, dragging right must aim further west so the box \
                 travels east: {:?}",
            right_drag.pivot(),
        );

        let mut down_drag = start;
        down_drag.nudge(crate::pane::OrbitDelta {
            pan: pan_for_drag(start, BOX_KM, 900.0, [0.0, 100.0]).expect("a pannable view"),
            ..Default::default()
        });
        assert!(
            down_drag.pivot()[2] > 1e-4,
            "at {exaggeration}x, dragging down must aim higher so the box travels \
                 down: {:?}",
            down_drag.pivot(),
        );
    }
}

/// **A drag of N points moves the pivot N points' worth of world.**
#[test]
fn a_drag_moves_the_pivot_by_exactly_the_world_the_pointer_crossed() {
    let height = 900.0f32;
    let aspect = 1.6f32;
    // The box is a 13:1 pancake, so a 60-point drag at the far end of the
    // zoom is 58 km — comfortably inside 120 km of half-width and comfortably
    // *outside* 9 km of true half-height. Vertical drags are therefore run
    // only where the stretch has bought the height room for them: at 12× the
    // half-height is 108 km, and the clamp is nowhere near.
    let horizontal = [60.0f32, 0.0f32];
    let vertical = [0.0f32, 60.0f32];
    let cases = [
        (1.0f32, 1.2f32, horizontal),
        (1.0, 2.5, horizontal),
        (1.0, 7.0, horizontal),
        (3.0, 1.2, horizontal),
        (3.0, 2.5, horizontal),
        (3.0, 7.0, horizontal),
        (12.0, 1.2, horizontal),
        (12.0, 2.5, horizontal),
        (12.0, 7.0, horizontal),
        (12.0, 1.2, vertical),
        (12.0, 2.5, vertical),
        (12.0, 7.0, vertical),
    ];
    for (exaggeration, distance, drag) in cases {
        // Due south of the box looking north, so screen-right is due east and
        // screen-up is due up.
        let camera =
            OrbitCamera::restore(180.0, 0.0, distance, [0.0; 3], exaggeration).expect("finite");
        let mut panned = camera;
        panned.nudge(crate::pane::OrbitDelta {
            pan: pan_for_drag(camera, BOX_KM, height, drag).expect("a pannable view"),
            ..Default::default()
        });

        // Where the new pivot is, in the *old* view.
        let view = view_for(camera, BOX_KM, aspect).expect("viewable");
        let stretched = exaggerated_box_km(panned, BOX_KM);
        let pivot_box = to_box(pivot_km(panned, BOX_KM), stretched);
        let label = format!("{exaggeration}x at distance {distance}, drag {drag:?}");
        assert!(
            pivot_box.iter().all(|c| *c > 0.0 && *c < 1.0),
            "precondition: the drag must not have hit the pivot clamp — {label}: \
                 {pivot_box:?}",
        );

        let ndc_x = -2.0 * drag[0] / (height * aspect);
        let ndc_y = 2.0 * drag[1] / height;
        let dir = direction(&view, [ndc_x, ndc_y]);
        let eye = view.eye_in_box;
        // Along `y`, the axis a north-facing camera is least parallel to.
        let t = (pivot_box[1] - eye[1]) / dir[1];
        let hit = [eye[0] + dir[0] * t, pivot_box[1], eye[2] + dir[2] * t];
        assert!(
            (hit[0] - pivot_box[0]).abs() < 2e-3 && (hit[2] - pivot_box[2]).abs() < 2e-3,
            "the pivot must land under where the pointer went — {label}: \
                 ray {hit:?} vs pivot {pivot_box:?}",
        );
    }
}

/// The pivot cannot be pushed off the box, however long the drag.
#[test]
fn no_amount_of_dragging_pushes_the_box_off_the_pane() {
    let mut camera = OrbitCamera::restore(180.0, 0.0, 2.5, [0.0; 3], 3.0).expect("finite");
    for _ in 0..200 {
        let pan = pan_for_drag(camera, BOX_KM, 900.0, [-400.0, -400.0]).expect("pannable");
        camera.nudge(crate::pane::OrbitDelta {
            pan,
            ..Default::default()
        });
    }
    for axis in camera.pivot() {
        assert!(
            (-1.0..=1.0).contains(&axis),
            "the pivot must stay on the box: {:?}",
            camera.pivot(),
        );
    }
    let view = view_for(camera, BOX_KM, 1.6).expect("viewable");
    let eye = view.eye_in_box;
    let dir = direction(&view, [0.0, 0.0]);
    let inside = (0..4000).any(|step| {
        let t = step as f32 * 0.005;
        let p = [
            eye[0] + dir[0] * t,
            eye[1] + dir[1] * t,
            eye[2] + dir[2] * t,
        ];
        p.iter().all(|c| (0.0..=1.0).contains(c))
    });
    assert!(
        inside,
        "after a pan run all the way to the clamp, the middle of the pane must \
             still be looking at the box",
    );
}

/// A pan is refused rather than laundered when it would divide by zero.
#[test]
fn a_pan_that_would_divide_by_zero_is_refused() {
    let camera = OrbitCamera::default();
    assert_eq!(pan_for_drag(camera, BOX_KM, 0.0, [10.0, 10.0]), None);
    assert_eq!(pan_for_drag(camera, BOX_KM, -5.0, [10.0, 10.0]), None);
    assert_eq!(pan_for_drag(camera, BOX_KM, f32::NAN, [10.0, 10.0]), None);
    assert_eq!(
        pan_for_drag(camera, [240.0, 240.0, 0.0], 900.0, [10.0, 10.0]),
        None,
    );
    assert_eq!(pan_for_drag(camera, BOX_KM, 900.0, [f32::NAN, 0.0]), None);
    assert_eq!(
        pan_for_drag(camera, BOX_KM, 900.0, [0.0, f32::INFINITY]),
        None,
    );
}

/// A panned camera aims at its pivot: the pivot is what lands in the middle
/// of the pane, at every yaw and pitch.
#[test]
fn a_panned_camera_looks_at_its_pivot_from_every_angle() {
    for (yaw, pitch) in [(0.0, 0.0), (225.0, 25.0), (95.0, -40.0), (310.0, 70.0)] {
        let camera = OrbitCamera::restore(yaw, pitch, 2.5, [0.4, -0.3, 0.5], 3.0).expect("finite");
        let view = view_for(camera, BOX_KM, 1.6).expect("viewable");
        let stretched = exaggerated_box_km(camera, BOX_KM);
        let want = to_box(pivot_km(camera, BOX_KM), stretched);

        let eye = view.eye_in_box;
        let dir = direction(&view, [0.0, 0.0]);
        let axis = (0..3)
            .max_by(|a, b| dir[*a].abs().total_cmp(&dir[*b].abs()))
            .expect("three axes");
        let t = (want[axis] - eye[axis]) / dir[axis];
        let hit = [
            eye[0] + dir[0] * t,
            eye[1] + dir[1] * t,
            eye[2] + dir[2] * t,
        ];
        for i in 0..3 {
            assert!(
                (hit[i] - want[i]).abs() < 2e-3,
                "at yaw {yaw} pitch {pitch} the centre ray must reach the pivot: \
                     {hit:?} vs {want:?}",
            );
        }
    }
}

/// A pivot of 1.0 is the **top face of the drawn box**, at every
/// exaggeration.
#[test]
fn a_pivot_of_one_is_the_top_face_of_the_drawn_box() {
    for ex in [1.0f32, 3.0, 12.0] {
        let camera = OrbitCamera::restore(180.0, 0.0, 2.5, [0.0, 0.0, 1.0], ex).expect("finite");
        let stretched = exaggerated_box_km(camera, BOX_KM);
        let in_box = to_box(pivot_km(camera, BOX_KM), stretched);
        assert!(
            (in_box[2] - 1.0).abs() < 1e-5,
            "at {ex}x a pivot of 1.0 must sit on the box's top face, got {in_box:?}",
        );
    }
    // And the bottom face, so a sign error cannot pass by symmetry alone.
    let camera = OrbitCamera::restore(180.0, 0.0, 2.5, [0.0, 0.0, -1.0], 5.0).expect("finite");
    let in_box = to_box(pivot_km(camera, BOX_KM), exaggerated_box_km(camera, BOX_KM));
    assert!((in_box[2]).abs() < 1e-5, "got {in_box:?}");
}

/// A [`MapPaneGeo`] reproduces Web Mercator exactly, not near its anchor.
#[test]
fn a_map_pane_affine_is_web_mercator_and_not_a_linearisation() {
    use crate::volume_view::{MapPaneGeo, mercator_y_of_lat};

    let geo = MapPaneGeo {
        rect: egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(800.0, 600.0)),
        anchor_lat: 41.7,
        anchor_lon: -93.7,
        anchor: egui::pos2(400.0, 300.0),
        points_per_degree_lon: 250.0,
        // Negative: Mercator y increases north, screen y increases down.
        points_per_mercator_y: -14_000.0,
    };

    assert_eq!(geo.project(41.7, -93.7), geo.anchor, "the anchor is fixed");

    // East is right and north is up, both at the declared rate.
    let east = geo.project(41.7, -92.7);
    assert!(
        (east.x - 650.0).abs() < 1e-3,
        "a degree east is 250 points right, got {east:?}"
    );
    assert!(
        (east.y - 300.0).abs() < 1e-3,
        "a pure longitude step must not move y"
    );

    let north = geo.project(42.7, -93.7);
    let want_y = 300.0 + (mercator_y_of_lat(42.7) - mercator_y_of_lat(41.7)) * -14_000.0;
    assert!(
        (f64::from(north.y) - want_y).abs() < 1e-3,
        "got {north:?}, want y {want_y}"
    );
    assert!(
        north.y < 300.0,
        "north must be up the screen, got {north:?}"
    );

    // The non-linearity itself: a degree north of the anchor and a degree south
    // of it are NOT the same number of points, because Mercator stretches
    // poleward. A latitude-linear affine would make these equal, and that is
    // the 3.7 km error this seam exists to avoid on the shipped 460 km box.
    let up = 300.0 - north.y;
    let down = geo.project(40.7, -93.7).y - 300.0;
    assert!(
        (up - down).abs() > 1.0,
        "Mercator's rows are not evenly spaced in latitude, so a degree north \
         ({up} points) and a degree south ({down} points) of 41.7 must differ. \
         Equal means the affine has been rewritten in latitude.",
    );
}

/// The 460 km box the user reported the soft floor on, at a camera close enough
/// to see it, asks for more texels than the mirror is drawn with.
#[test]
fn the_reported_framing_asks_for_more_than_one_mirror_texel_a_pixel() {
    let camera = OrbitCamera::restore(225.0, 20.0, 1.0, [0.0; 3], 3.0)
        .expect("the reported camera is a legal one");
    // A source pane showing the 460 km box across about 900 points, expressed
    // the way `MapPaneGeo` carries it: points per degree of longitude at 34.635.
    let points_per_km = 900.0 / 460.0;
    let points_per_degree_lon =
        points_per_km * squallar_geo::KM_PER_DEGREE_LAT * 34.635_f64.to_radians().cos();

    let magnification = floor_magnification(
        camera,
        [460.0, 460.0, 18.0],
        900.0,
        points_per_degree_lon,
        34.635,
    )
    .expect("a real framing must produce a demand");
    assert!(
        magnification > 1.0,
        "the reported framing magnifies the floor by {magnification}x, so a \
         mirror at the frame's own density has nothing left to give",
    );
    assert!(
        magnification < 4.0,
        "{magnification}x is outside the regime the rungs were sized for; \
         re-derive `MIRROR_SCALE_MAX` before widening this",
    );
}

/// Backing the camera off reduces the demand, and the relationship is the
/// reciprocal one perspective implies.
#[test]
fn the_demand_falls_as_the_reciprocal_of_the_eye_distance() {
    let box_km = [460.0, 460.0, 18.0];
    let near = OrbitCamera::restore(225.0, 20.0, 1.0, [0.0; 3], 3.0).unwrap();
    let far = OrbitCamera::restore(225.0, 20.0, 2.0, [0.0; 3], 3.0).unwrap();

    let near = floor_magnification(near, box_km, 900.0, 4000.0, 35.0).unwrap();
    let far = floor_magnification(far, box_km, 900.0, 4000.0, 35.0).unwrap();
    assert!(near > far, "a closer eye must ask for more texels");
    assert!(
        (near / far - 2.0).abs() < 1e-3,
        "halving the eye distance must double the demand, got {near} and {far}",
    );
}

/// A degenerate pane or a degenerate affine asks for nothing rather than for
/// everything.
#[test]
fn a_degenerate_framing_asks_for_no_texels_at_all() {
    let camera = OrbitCamera::default();
    let box_km = [460.0, 460.0, 18.0];
    assert_eq!(floor_magnification(camera, box_km, 0.0, 4000.0, 35.0), None);
    assert_eq!(
        floor_magnification(camera, box_km, f32::NAN, 4000.0, 35.0),
        None
    );
    assert_eq!(floor_magnification(camera, box_km, 900.0, 0.0, 35.0), None);
    assert_eq!(
        floor_magnification(camera, box_km, 900.0, f64::NAN, 35.0),
        None
    );
    assert_eq!(
        floor_magnification(camera, [0.0, 0.0, 0.0], 900.0, 4000.0, 35.0),
        None
    );
}

// ---------------------------------------------------------------------------
// C2: one light, two modes
// ---------------------------------------------------------------------------

/// The Colorado front range, and an instant in the middle of a 2026 afternoon.
const PLACE: squallar_geo::GeoPoint = squallar_geo::GeoPoint {
    lat: 39.0,
    lon: -106.0,
};
const AFTERNOON: f64 = 1_782_000_000.0 + 20.0 * 3_600.0;

/// **A refused instant takes the readable light**, and nothing about the frame
/// says night.
///
/// This is the whole of C2's answer to "what does a refusal do", and the two
/// alternatives are both worse. `unwrap_or(<night>)` is the silent-night
/// defect the `Option` was introduced to remove — it paints a dark pane that
/// looks like a correct 3 a.m. picture and is a refusal. Declining to draw
/// trades the whole picture for a light, over a timestamp.
///
/// The readable light is a complete, legible, correct picture of the volume,
/// so nothing here is half-done — and the pane's own control reads this same
/// function and reports which light came back, which is what keeps the
/// substitution from being silent.
#[test]
fn a_refused_instant_takes_the_readable_light_and_never_a_silent_night() {
    // Every input `squallar_geo::solar::sun_light` refuses, one per reason.
    for (instant, anchor, why) in [
        (f64::NAN, Some(PLACE), "a non-finite instant"),
        (f64::INFINITY, Some(PLACE), "an infinite instant"),
        (
            // Past +/-5 Julian centuries from J2000, where every polynomial in
            // that module overflows.
            1.0e19,
            Some(PLACE),
            "an instant outside the theory's window",
        ),
        (
            AFTERNOON,
            Some(squallar_geo::GeoPoint {
                lat: 91.0,
                lon: 0.0,
            }),
            "a latitude that is not a place",
        ),
        (AFTERNOON, None, "a site this build cannot place"),
    ] {
        assert_eq!(
            volume_light(true, anchor, instant),
            VolumeLight::Headlight,
            "{why} did not fall back to the readable light",
        );
    }
}

/// The refusal is a refusal of the SUN, not of the mode: the same call with a
/// place and an instant answers with the sun.
///
/// The non-triviality half of the test above, which would be just as green
/// against a build that never returned a sun at all.
#[test]
fn a_placed_site_at_an_ordinary_instant_really_does_get_the_sun() {
    let light = volume_light(true, Some(PLACE), AFTERNOON);
    let VolumeLight::Sun(sun) = light else {
        panic!("the front range on a 2026 afternoon was refused: {light:?}");
    };
    assert!(
        sun.elevation_deg > 0.0,
        "the sun is {} degrees up at 20:00 UTC over 106 W in June, which is \
         about 14:00 local and should be daylight",
        sun.elevation_deg,
    );
    // `direction_enu` is a unit vector and box space is that frame, so this is
    // a unit vector too. A direction that is not normalised would be a
    // brightness the shader's `normalize` silently discards on one surface and
    // not the other.
    let length = sun
        .direction_box
        .iter()
        .map(|c| f64::from(*c) * f64::from(*c))
        .sum::<f64>()
        .sqrt();
    assert!(
        (length - 1.0).abs() < 1e-5,
        "the box-space sun direction is {length} long, not one",
    );
    // Up is the elevation's own sine, which is what makes box space the local
    // east-north-up frame rather than merely resembling it.
    assert!(
        (f64::from(sun.direction_box[2]) - f64::from(sun.elevation_deg).to_radians().sin()).abs()
            < 1e-5,
        "the direction's up component is not the elevation's sine",
    );
}

/// **The readable light is the readable light in both directions**: asking for
/// it never consults the sun, whatever the instant.
#[test]
fn the_readable_mode_never_reaches_for_the_sun() {
    for instant in [AFTERNOON, f64::NAN, 0.0] {
        assert_eq!(
            volume_light(false, Some(PLACE), instant),
            VolumeLight::Headlight
        );
    }
}

/// A volume's collection time really is the Unix instant it names.
///
/// `sun_light` takes seconds because `squallar-geo`'s charter forbids it
/// chrono, so this conversion is the one place the two calendars meet — and a
/// wrong one would place every sun in the app at the same believable wrong
/// hour. Pinned against a date computed by hand rather than by chrono, which
/// is the thing being checked.
#[test]
fn a_collection_time_converts_to_the_instant_it_names() {
    let at = |y, m, d, hh, mm, ss| {
        chrono::NaiveDate::from_ymd_opt(y, m, d)
            .unwrap()
            .and_hms_opt(hh, mm, ss)
            .unwrap()
    };
    assert_eq!(unix_seconds_of(at(1970, 1, 1, 0, 0, 0)), 0.0);
    assert_eq!(unix_seconds_of(at(1970, 1, 2, 0, 0, 0)), 86_400.0);
    // 2000-01-01T00:00:00Z: thirty years, of which 1972, 1976 ... 1996 are the
    // seven leap years plus 2000 is not yet reached, so 30 * 365 + 7 days.
    assert_eq!(
        unix_seconds_of(at(2000, 1, 1, 0, 0, 0)),
        (30.0 * 365.0 + 7.0) * 86_400.0,
    );
    // And before the epoch, which a `u64` spelling would wrap.
    assert_eq!(unix_seconds_of(at(1969, 12, 31, 23, 59, 59)), -1.0);
}

/// **The white balance is a balance**: a zenith sun is exactly neutral and
/// exactly one, which is what lets a basemap authored under neutral light read
/// as itself at noon.
///
/// Measured on the ramps rather than on a picture, so it holds whether or not
/// a GPU is present. `squallar-gpu/tests/volume_light.rs` renders it, and is
/// `#[ignore]`d for a real adapter.
#[test]
fn a_zenith_sun_is_exactly_white() {
    let beam = squallar_geo::solar::sun_tint(90.0);
    let sky = squallar_geo::solar::sky_ambient(90.0);
    let white = super::zenith_white();
    for channel in 0..3 {
        assert!(
            (white[channel] - (beam[channel] + sky[channel])).abs() < 1e-12,
            "the white this renderer balances against is not the light a level \
             surface takes under a zenith sun",
        );
        assert!(
            white[channel] > 1.0,
            "channel {channel} of daylight is {}, so there would be nothing to \
             balance and the criterion below could not fail",
            white[channel],
        );
    }
    // And it is not neutral before the balance, which is the reason the
    // balance exists: the sky is blue and a white basemap took its colour.
    assert!(
        white[2] - white[0] > 0.1,
        "unbalanced daylight is already neutral ({white:?}), so nothing here \
         is being corrected",
    );
}

/// The sun is placed over the ground the box is actually about: the picked
/// region's centre when there is one, the site otherwise.
#[test]
fn the_sun_is_placed_over_the_box_and_not_over_the_site_it_was_dragged_from() {
    let site = squallar_geo::GeoPoint {
        lat: 35.0,
        lon: -97.0,
    };
    assert_eq!(
        crate::pane::volume_box_anchor(None, Some(site)),
        Some(site),
        "a pane with no region is the default box about its site",
    );
    let centre = squallar_geo::GeoPoint {
        lat: 36.0,
        lon: -99.0,
    };
    let region =
        crate::pane::VolumeRegion::new(centre, squallar_radar::voxel::HalfExtentKm::square(100.0))
            .expect("a region on Earth");
    assert_eq!(
        crate::pane::volume_box_anchor(Some(region), Some(site)),
        Some(centre),
        "a picked region moves the box, so it moves the sun with it",
    );
    assert_eq!(
        crate::pane::volume_box_anchor(Some(region), None),
        None,
        "a pane whose site is not placed has no frame to put a box in, and \
         there is no such thing as the sun over nowhere",
    );
}
