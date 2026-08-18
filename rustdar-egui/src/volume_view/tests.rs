use super::*;

const BOX_KM: [f32; 3] = [240.0, 240.0, 18.0];

/// A camera aimed at the box's centre with no vertical stretch — true
/// proportions, which is what every matrix test below is written against.
///
/// 1× rather than the shipped default of 3×, deliberately: these tests assert
/// geometry in kilometres, and a default that stretched the box would make
/// every expected value a function of a constant that is allowed to change.
/// The exaggeration has its own tests, which vary it on purpose.
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
///
/// The single strongest end-to-end check available without a GPU: it
/// exercises all three factors and their multiplication order at once. A
/// transposed factor, a swapped multiplication order or a sign error in the
/// basis all move this ray off the centre, and none of them can be seen by
/// reading the code.
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
///
/// This is the geometry half of the #6 zoom: `MIN_EYE_DISTANCE` is 0.05
/// framing radii, which is inside the box from every default angle, and
/// nothing in `build_view` assumes the eye is outside — the derivation is a
/// point and a direction, not a framing. The GPU half (the raymarch's slab
/// entry clamped to zero so an inside eye marches forward from itself)
/// lives in `rustdar-frontend`'s silhouette harness, where the shader runs.
/// Checked at 1x and 12x. The stop no longer moves with the knob — the
/// framing radius reads the true box — but the *box* still grows around
/// the eye, so "inside" is a claim worth re-making at both ends.
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
///
/// Pins the convention rather than the arithmetic: reversing the sine and
/// cosine, or negating one of them, still produces a working orbit that
/// simply spins the wrong way — a defect nobody notices until they compare
/// against a map.
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
///
/// Measured through the geometry rather than asserted about the matrix, so
/// it fails if anyone "fixes" the pancake by normalising the axes. Looking
/// straight down the y axis from level, the box's horizontal half-extent
/// subtends a much larger angle than its vertical one, in the ratio of the
/// physical extents.
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
///
/// They look load-bearing and are not — the shader unprojects only at
/// `depth = 1.0`, where the analytic inverse gives exactly the far plane, and
/// the normalisation divides the distance out. Pinned because the tempting
/// "fix" for a rendering problem is to tune them, and this says in advance
/// that it will do nothing.
///
/// **The depth range is not free of consequences, only of geometry.** The
/// homogeneous `w` at `depth = 1.0` is `1/far`, and it is reached as
/// `(1/far − 1/near) + 1/near` — a subtraction of two nearly equal numbers
/// whenever `far ≫ near`, which cancels most of an `f32`'s digits away
/// before the divide. That is why this asserts over sane ranges and why the
/// production values are a couple of hundred apart rather than a million.
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
///
/// Each of these reaches a division. `f32::clamp` propagates `NaN`, so a
/// clamp here would hand back a matrix of `NaN` that the GPU accepts and
/// draws as an empty pane — a failure with no error anywhere.
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
///
/// Written against a hand-computed product rather than against another
/// call to `multiply`, which is the version that cannot see a transpose.
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

/// The stub painter is not a substitute for the frontend's downcast test.
///
/// This is the test that says so out loud. It asserts the stub's payload is
/// exactly what `egui_wgpu` would silently discard, so nobody can read the
/// stub-based suite as evidence that a real pane draws.
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
            product: rustdar_radar::types::RadarProduct::Reflectivity,
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
    };
    let VolumePaint::Callback { payload, .. } = painter.paint(&frame) else {
        panic!("the painting stub must paint");
    };
    assert!(
        payload.downcast_ref::<StubPayload>().is_some(),
        "the stub's payload is its own type, which nothing in egui_wgpu can draw — \
             the real payload's downcast is pinned in rustdar-frontend by \
             `the_payload_the_painter_hands_over_is_one_egui_wgpu_can_draw`",
    );
    assert_eq!(painter.seen.lock().unwrap().len(), 1);
}

// --- Vertical exaggeration ---------------------------------------------

/// The exaggeration stretches the box's geometry and moves no cell within
/// it.
///
/// This is the property the whole design rests on. `box_from_clip` maps clip
/// space to *box* space — the unit cube over the voxel grid — so if the
/// stretch were being applied to the data rather than to the camera's world,
/// the box coordinate a given ray reached would change. It must not: the
/// centre of the box is the centre of the box at every setting.
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
///
/// This replaced the opposite assertion, which measured `eye_distance` against
/// the *stretched* half-diagonal in the name of "the box fills the same
/// fraction of the pane at 1× and at 12×". That promise was not kept and could
/// not be: the stretch changes the box's shape, and one distance cannot hold
/// two changing dimensions still. Measured at 1× against 12×, the box's
/// silhouette still grew 1.36× on a whole-scan box, while on a box zoomed to
/// the resampler's 10 km floor the **ground** shrank to 0.154× — the knob was
/// a 6.5× zoom out, which is precisely what `exaggerated_box_km` says a
/// vertical exaggeration must never be.
///
/// So the property is now the one a vertical exaggeration is defined by, and
/// it holds exactly rather than approximately. The mutation it closes is
/// reaching for `exaggerated_box_km` in `framing_radius_km`, which is the
/// tempting "fix" for a box that stands tall out of the pane at 12×.
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
///
/// The regression this pins arrived with the box that covers its pane's
/// rectangle instead of the square inscribed in it. The standoff was
/// `eye_distance × half_diagonal` over all three axes, so the box getting wider
/// moved the eye out with it — measured on a real KDMX volume at 1600×900,
/// 1.4397× further out for a 460 km square becoming an 818 × 460 km rectangle,
/// which brought a patch of ground back at 0.694× its size at the default
/// camera. Converting a pane to 3D made the storm smaller, which is the
/// complaint the rectangle was built to answer.
///
/// The eye distance is asserted rather than the picture because with a fixed
/// vertical field of view the two are the same statement: the ground a pane
/// spans is `2 · distance · tan(fov/2)`, so an unmoved eye *is* an unchanged
/// scale, at every yaw and pitch at once.
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
///
/// The box's height is fixed — 0 to 18 km MSL, times the knob — while the zoom
/// moves its ground extent over the resampler's whole range, a 651 km whole-scan
/// box down to a 20 km floor. A standoff taken from the three-axis half-diagonal
/// therefore stopped tracking the zoom as the ground shrank past the ~54 km
/// vertical term: measured across that range at the shipped 3× exaggeration, a
/// **32.53× ground zoom read as 15.12× of picture**, so the user lost more than
/// half of what they asked for, worst at the tight end where they were looking
/// hardest.
///
/// Asserted end to end *and* step by step, because a rule can hit the endpoints
/// and sag between them.
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
///
/// The framing is `eye_distance`'s to set and nothing else's. Under the
/// three-axis half-diagonal it was also the pane's: the pane's height spanned
/// 0.926 of the box's north extent at aspect 0.15 and 6.525 at 7.1, a factor of
/// 7.04 from the pane's shape alone, so a user who dragged a divider was
/// silently zoomed and a narrow 3D pane sat at a different scale from the plan
/// pane it was made from. Held at `eye_distance / √2 · 2 · tan(20°) = 1.00000`
/// here, at every aspect.
///
/// Re-baselined from **1.28683**, which is what that expression returned at the
/// 2.5 standoff this default replaced — same six aspect ratios, same 1e-4
/// tolerance, still pinned to an exact number rather than to a range. The
/// baseline moved because the standoff did; the property being asserted, that
/// the number does not depend on the pane's *shape*, did not. 1 is not a
/// coincidence and not a new literal either: it is what
/// [`eye_distance_for_plan_scale`] is defined to produce, at any field of view,
/// so this stays true if [`FOV_Y_DEG`] changes and fails if anyone pins the
/// standoff to a number again.
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
///
/// This is the defect [`eye_distance_for_plan_scale`] closes, stated as the user
/// sees it. At the 2.5 standoff it replaced, a 3D pane opened **1.287×** zoomed
/// out from the pane it was made from, at every aspect ratio and every zoom.
///
/// # Measured out of `box_from_clip`, not off the standoff
///
/// The constant being right and the picture being right are two claims, and only
/// the second one is the user's — so nothing here reads `eye_distance`. Two rays
/// are cast through the pane's left and right edges exactly as the shader casts
/// them, meet the horizontal plane the pivot sits on, and the kilometres between
/// the hits are compared with the kilometres the plan pane of the same shape
/// draws across the same width. An error anywhere between the camera and the
/// matrix — a factor left in `framing_radius_km`, a field of view the standoff
/// stopped agreeing with, a `box_from_world` that scaled the horizontal axes —
/// lands here.
///
/// # Why across the pane and not down it
///
/// A tilted camera foreshortens, so the two directions cannot both be 1:1 and
/// only one of them is the scale. The camera's right vector is level at every
/// pitch, so the across-pane direction is the one perspective leaves alone; the
/// receding direction is compressed by the tilt on purpose, and that compression
/// is what makes the picture read as ground rather than as a plan view.
///
/// Both edge rays meet a horizontal plane at the same depth — the right vector
/// has no vertical component to change it — so the span between them is linear
/// in the screen coordinate. That makes this a *scale* and not an average of
/// one, which is why it can be read from the pane's full width rather than from
/// a derivative at the centre.
///
/// Swept over yaw, pitch, the exaggeration knob's whole travel, the resampler's
/// whole zoom range and every aspect a column can take, because "uniformly" was
/// the reported shape of the defect and a single framing cannot see it.
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
///
/// The same equality [`converting_a_plan_pane_to_3d_keeps_its_ground_scale`]
/// measures, read off the other side and through the production path that sizes
/// the floor mirror: if the 3D pane draws the ground at the plan pane's scale,
/// then the copy of the plan pane it samples is already at the right density and
/// [`floor_magnification`] is 1. It was **1.28683** at the 2.5 standoff — a
/// pane that opened asking for a rung it did not need.
///
/// Worth its own test because the two functions reach the figure by different
/// arithmetic — one through `box_from_clip`, one through the source pane's
/// affine and a latitude — and a change that broke the agreement would leave
/// both self-consistent.
#[test]
fn a_fresh_pane_asks_the_mirror_for_one_texel_a_pixel() {
    let box_km = [460.0f32, 460.0, 18.0];
    for lat in [25.0f64, 41.7311, 60.0] {
        // A plan pane 900 points tall showing the box's north extent over that
        // height, expressed the way `MapPaneGeo` carries it.
        let points_per_degree_lon =
            (900.0 / 460.0) * rustdar_geo::KM_PER_DEGREE_LAT * lat.to_radians().cos();
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
///
/// A *vertical* exaggeration that scaled all three axes would be a zoom, and
/// a zoom is what `eye_distance` already is — so the mutation is invisible in
/// a screenshot and wrong in every measurement.
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
///
/// Both signs are convention rather than arithmetic — an inverted pan pans
/// perfectly well and merely feels wrong, which is the kind of defect that
/// survives review — so both are asserted.
///
/// Run at three exaggerations including the shipped default. 1× is the single
/// value at which `exaggerated_box_km` is the identity, so a fixture pinned
/// there cannot see the box `pan_for_drag` is measured against at all.
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
///
/// This is the scaling that makes panning feel attached to the object rather
/// than to the mouse, and it is the one thing about the gesture that is
/// arithmetic rather than taste. Asserted by casting the ray the pointer
/// ended on and checking the new pivot is on it — which is the user-visible
/// statement of the property, and which a wrong constant cannot satisfy at
/// three different zooms at once.
///
/// # The exaggeration is part of the property
///
/// The world a screen point spans is set by the eye's distance, which is
/// measured in framing radii **of the true box**; the fraction the pivot is
/// stored as is against the *stretched* half-extent. The two boxes are
/// deliberately not the same one, so every case here is a check that
/// `pan_for_drag` reads each of them exactly where `pivot_km` and `view_for`
/// do. Running only at 1× — the single value at which `exaggerated_box_km` is
/// the identity — would make the test blind to the whole of that, which is
/// the defect this file's other sites were fixed for.
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
        //
        // The pivot is what lands in the middle of the pane, so after a drag
        // of N points **right** the new pivot is the object point that was N
        // points **left** before it — which is precisely what "the content
        // followed the pointer" means, and is the whole property under test.
        // The same for a drag **down** and the point that was N points
        // **up**.
        //
        // The viewport is `height · aspect` points wide and NDC spans `-1..1`,
        // so N points is `2N / (height · aspect)` across and `2N / height`
        // down. NDC `y` runs up while screen `y` runs down, which cancels the
        // second inversion and leaves the vertical term positive.
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
///
/// The clamp is what stops the box being pushed entirely off screen: the
/// pivot is what lands in the middle of the pane, so a pivot that is always a
/// point of the box means some of the box is always under the middle of the
/// pane. Both halves are asserted — the bound itself, and the consequence.
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
///
/// A pane one frame tall during a divider drag is the realistic way this
/// arrives, and the consequence of clamping instead would be a NaN pivot —
/// which is not a wrong picture but a staleness key that never equals itself,
/// and therefore a rebuild every frame for the life of the pane.
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
///
/// The mutation this closes is adding the pivot to the eye but leaving
/// `forward` pointing back at the origin — which still pans, still looks
/// plausible, and puts the box's *centre* in the middle of the pane rather
/// than the point the user dragged to.
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
///
/// This is what the unit means, and it is what makes the clamp in
/// `OrbitCamera::nudge` a one-line guarantee: a pivot inside ±1 is a point of
/// the box, so some of the box is always under the middle of the pane.
///
/// The mutation this closes measures the pivot against the *true* box while
/// the geometry is drawn against the stretched one. Every relative test still
/// passes — the two ends of the pan agree with each other — and the meaning
/// quietly changes: at 3× the clamp would stop the pivot a third of the way
/// up the drawn box, so the top of a storm could never be brought to the
/// middle of the pane.
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
///
/// The seam's whole claim is that four numbers replace a `walkers::Projector`
/// without loss: screen `x` is linear in longitude and screen `y` is linear in
/// Mercator `y`, so an affine in those two variables is the projection's closed
/// form rather than a local linearisation. If that were only approximately true,
/// the 3D floor would register at the box's centre and drift at its corners —
/// which is the exact failure the reprojection exists to remove, so it must not
/// be reintroduced one layer up.
///
/// The two directions are asserted separately because a sign error in either is
/// a floor that is mirrored rather than misplaced, and a mirrored floor at a
/// centred anchor looks almost right.
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
///
/// This is the whole premise of adaptive mirror resolution stated as a number.
/// The source pane is showing the box across its width — about two points to
/// the kilometre — while the 3D pane at this distance puts nearly four points on
/// the same kilometre. Every mirror texel is therefore stretched across about
/// two screen pixels, which is what "the basemap, roads and place labels all go
/// soft" is.
///
/// The figures are the user's framing: KFDX at 34.635 N, a 460 km box, 18 km
/// tall at 3x vertical exaggeration, in a 900-point-tall pane.
#[test]
fn the_reported_framing_asks_for_more_than_one_mirror_texel_a_pixel() {
    let camera = OrbitCamera::restore(225.0, 20.0, 1.0, [0.0; 3], 3.0)
        .expect("the reported camera is a legal one");
    // A source pane showing the 460 km box across about 900 points, expressed
    // the way `MapPaneGeo` carries it: points per degree of longitude at 34.635.
    let points_per_km = 900.0 / 460.0;
    let points_per_degree_lon =
        points_per_km * rustdar_geo::KM_PER_DEGREE_LAT * 34.635_f64.to_radians().cos();

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
///
/// Not a tautology worth skipping: the sign is the whole of whether the rung
/// helps or hurts, and a sign error here would spend the largest mirror on the
/// most zoomed-*out* view — where the floor is already minified and nothing
/// could be gained.
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
///
/// The direction matters: an unanswerable question that returned a large number
/// would allocate the largest mirror the target allows, on a frame that has
/// nothing to draw into it.
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
