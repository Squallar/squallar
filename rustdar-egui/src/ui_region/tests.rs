//! The floor is framed on the box, and the zoom gesture moves the eye and
//! nothing else.
//!
//! The eleven tests this file used to hold were all about deriving a box from a
//! viewport — containment on four sides, the quantum that stopped the pane
//! rebuilding for ever, the ceiling and floor the derivation was clamped to.
//! None of those things exists: a 3D pane's region is stored rather than
//! measured, so there is no measurement to contain, no per-frame key to keep
//! still and no derived extent to clamp. They are named in the change's test
//! accounting rather than reconstructed here — a test for a concept that has
//! been deleted cannot fail, and a suite full of tests that cannot fail is the
//! defect this codebase keeps having to fix.
//!
//! What is pinned instead is the implication running the *other* way — that a
//! strip framed on a stored box covers the whole of it, tightly, at every
//! latitude and for every shape of pane — plus the arithmetic between a wheel
//! notch and a standoff and the refusals that keep a bad frame's number out of
//! the camera. The *gate* — which pane a gesture belongs to — is pinned through
//! the real UI by `ui_map::volume_arm_tests`, because it is a question about
//! layers and hover that no unit fixture can ask honestly.

use super::{
    COVERAGE_MARGIN, MAX_FRAMING_PASSES, dolly_for_step, ground_half_extent, viewport_for_region,
    zoom_step,
};

/// An input frame carrying `scroll` points of wheel and nothing else, at
/// egui's own default 60 Hz timing.
///
/// `zoom_factor_delta` is private and defaults to exactly 1.0, which is the
/// "no pinch this frame" value [`zoom_step`] tests for — so a default frame
/// with a scroll written into it drives the wheel branch, which is the one a
/// desktop user reaches.
fn scroll_frame(scroll: f32) -> egui::InputState {
    let mut input = egui::InputState::default();
    input.smooth_scroll_delta.y = scroll;
    input
}

/// **The property the user asked for, in the one place it is arithmetic**: a
/// zoom level in is a halving of the eye's standoff, so the ground the pane
/// looks at halves while the box under it does not move at all.
///
/// One Web Mercator zoom level is a factor of two of ground per point by the
/// projection's definition, and a perspective camera sees `2 · d · tan(fov/2)`
/// of ground at its pivot plane — linear in `d`. So the conversion is `2^step`
/// exactly, and this is what says the exponent has not been dropped, doubled or
/// inverted.
#[test]
fn one_zoom_level_is_one_halving_of_the_standoff() {
    assert_eq!(dolly_for_step(1.0), 2.0, "a level in halves the standoff");
    assert_eq!(dolly_for_step(-1.0), 0.5, "a level out doubles it");
    assert_eq!(dolly_for_step(2.0), 4.0, "two levels in is two halvings");
}

/// A frame with no gesture leaves the eye exactly where it is.
///
/// The neutral value is 1.0 and not 0.0 because this is a *divisor*, and the
/// two are one character apart at the call site. A 0.0 would divide the
/// standoff by zero and put an infinity in the camera, whose staleness key
/// would then never equal itself — a permanently rebuilding pane whose only
/// symptom is a fan.
#[test]
fn no_gesture_leaves_the_eye_where_it_is() {
    assert_eq!(dolly_for_step(0.0), 1.0);
}

/// Zooming out undoes zooming in, exactly, at every magnitude a gesture
/// produces.
///
/// `2^s · 2^-s = 1` is arithmetic, but it is arithmetic in `f64` that is then
/// rounded to `f32` twice, and the thing being pinned is that a user who
/// scrolls one notch each way finds the pane where they left it rather than
/// drifting a little further out every round trip.
#[test]
fn zooming_out_undoes_zooming_in() {
    for step in [0.05_f64, 0.25, 0.5, 1.0, 3.0, 7.0] {
        let round_trip = f64::from(dolly_for_step(step)) * f64::from(dolly_for_step(-step));
        assert!(
            (round_trip - 1.0).abs() < 1e-6,
            "a {step}-level round trip scaled the standoff by {round_trip}",
        );
    }
}

/// A gesture frame that produced a `NaN` or an infinity does not reach the
/// camera.
///
/// [`crate::pane::OrbitCamera::nudge`] would refuse such a factor itself — but
/// it refuses the **whole delta**, which throws away the same frame's orbit and
/// pan. Answering the identity here keeps those two verbs working through a
/// frame whose scroll arrived unusable, which is what a user dragging and
/// scrolling at once actually does.
#[test]
fn a_non_finite_gesture_does_not_reach_the_camera() {
    for step in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        assert_eq!(dolly_for_step(step), 1.0, "step {step} was not refused");
    }
}

/// A finite step whose exponential is not finite is refused too.
///
/// The one arithmetic step between [`zoom_step`]'s own finiteness check and the
/// camera's: `2^129` is past `f32::MAX` and `2^-150` is under `f32::MIN_POSITIVE`
/// even as a subnormal, so a finite input can still produce an infinity or a
/// zero. Neither may be divided into a standoff, and testing the *output* rather
/// than bounding the input is what keeps this true if the exponent's base ever
/// changes.
#[test]
fn a_step_that_overflows_the_factor_is_refused() {
    assert_eq!(dolly_for_step(1000.0), 1.0, "an overflow to infinity");
    assert_eq!(dolly_for_step(-1000.0), 1.0, "an underflow to zero");
}

/// Scrolling up brings the eye in and scrolling down takes it out.
///
/// Convention rather than arithmetic — a sign error here dollies perfectly well
/// and merely feels backwards, which is the kind of defect that survives review
/// — and it is walkers' own convention, so that a wheel means the same thing
/// over a 3D pane as over the plan view beside it.
#[test]
fn scrolling_up_brings_the_eye_in() {
    assert!(
        dolly_for_step(zoom_step(&scroll_frame(50.0))) > 1.0,
        "a scroll up must divide the standoff by more than one",
    );
    assert!(
        dolly_for_step(zoom_step(&scroll_frame(-50.0))) < 1.0,
        "a scroll down must divide it by less than one",
    );
    assert_eq!(
        dolly_for_step(zoom_step(&scroll_frame(0.0))),
        1.0,
        "a frame with no wheel is not a gesture",
    );
}

/// The range the gesture can actually reach, measured off the camera's own
/// constants rather than restated.
///
/// The module doc claims 160× and 7.32 zoom levels as the bound the box's
/// bounds were replaced by. Both are read back here, so a change to either
/// constant fails this instead of leaving the prose describing a range that no
/// longer exists.
#[test]
fn the_gesture_runs_from_inside_the_box_to_well_outside_it() {
    let span = f64::from(crate::pane::MAX_EYE_DISTANCE / crate::pane::MIN_EYE_DISTANCE);
    assert!(
        (span - 160.0).abs() < 0.5,
        "the standoff range is {span}x, not the 160x the module doc states",
    );
    let levels = span.log2();
    assert!(
        (levels - 7.32).abs() < 0.01,
        "the standoff range is {levels} zoom levels, not the 7.32 the module doc states",
    );
}

/// A box of `half_east_km` × `half_north_km` about a point.
fn half(east_km: f64, north_km: f64) -> rustdar_radar::voxel::HalfExtentKm {
    rustdar_radar::voxel::HalfExtentKm { east_km, north_km }
}

/// **The property the floor exists for**: the strip covers the whole box, on
/// every axis, for every shape of box and pane the application can produce.
///
/// `floor_colour` clips the floor to the mirror's `0..1` and answers
/// *transparent* outside it — off the mirror is ground the strip is not showing
/// and has no colour to report, and clamping would smear the strip's border
/// across the rest of the box as if it were map. So a strip that falls short on
/// any side is a volume standing on nothing along that side, which is the exact
/// symptom the stored region was going to reintroduce and this closes.
///
/// Swept rather than sampled once: the binding axis changes with the pane's
/// aspect and the box's, and a framing that solved for the wrong one would pass
/// on a square fixture. The latitudes matter for the same reason — Mercator's
/// north lane is the one the single-logarithm solve is only approximate on.
#[test]
fn the_framed_strip_covers_the_whole_box() {
    for (w, h) in [
        (700.0, 450.0),
        (450.0, 700.0),
        (900.0, 200.0),
        (400.0, 400.0),
    ] {
        let rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(w, h));
        for lat in [0.0, 25.0, 35.33, 49.0, 64.8] {
            let centre = walkers::lat_lon(lat, -97.28);
            for box_half in [
                half(460.125, 460.125),
                half(300.0, 300.0),
                half(120.0, 75.0),
                half(75.0, 120.0),
                half(10.0, 10.0),
            ] {
                let memory = viewport_for_region(rect, centre, box_half)
                    .expect("a finite box in a pane with area must be framable");
                let covered = ground_half_extent(rect, &memory, centre)
                    .expect("a framed viewport must be measurable");
                assert!(
                    covered.east_km >= box_half.east_km && covered.north_km >= box_half.north_km,
                    "a {w}x{h} strip at {lat}N framed on {box_half:?} covers only \
                     {covered:?} - the volume stands on transparency out there",
                );
            }
        }
    }
}

/// The framing is **tight**, not merely sufficient.
///
/// The mirror is a fixed number of pixels, so every kilometre of ground outside
/// the box is floor resolution spent on ground `floor_hit` will clip away. A
/// solve that zoomed out "to be safe" would pass the coverage test above and
/// cost the floor detail on every 3D pane in the application.
///
/// The binding axis is asserted to land inside [`COVERAGE_MARGIN`] and a little
/// over — the margin is what the solve converges *through*, so landing a
/// fraction of it wide is the intended outcome rather than slack. The other axis
/// is free, because a square box in a 16:9 strip must overhang east–west and
/// there is no framing that avoids it.
#[test]
fn the_framing_spends_no_more_of_the_mirror_than_the_box_needs() {
    let rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(700.0, 450.0));
    let centre = walkers::lat_lon(35.33, -97.28);
    for box_half in [half(460.125, 460.125), half(120.0, 75.0), half(10.0, 10.0)] {
        let memory = viewport_for_region(rect, centre, box_half).expect("framable");
        let covered = ground_half_extent(rect, &memory, centre).expect("measurable");
        let slack = (covered.east_km / box_half.east_km).min(covered.north_km / box_half.north_km);
        assert!(
            slack <= 1.0 + 2.0 * COVERAGE_MARGIN,
            "framing {box_half:?} left the binding axis {slack:.6}x wider than \
             the box; every bit past the margin is mirror resolution the box \
             clips away",
        );
    }
}

/// What [`MAX_FRAMING_PASSES`] is actually worth, measured rather than asserted.
///
/// The east–west lane is exact in one pass because walkers' points per degree of
/// longitude is exactly `tile_size · 2^zoom / 360`. The north–south lane is not,
/// because Web Mercator's scale varies with latitude and the latitudes of the
/// rect's own edges move as the zoom does, so each pass measures a projection
/// the last one changed — and it approaches from **above**, which is why
/// [`COVERAGE_MARGIN`] exists rather than an exact target.
///
/// Both facts are asserted here, on the worst shape this application can make.
/// The budget is held to the convergence rate that justifies it, and the
/// approach direction is held because the margin's whole argument rests on it:
/// a solve that crossed below 1.0 would make the margin unnecessary slack, and
/// one that stopped converging would make it insufficient.
#[test]
fn the_framing_budget_is_enough_for_the_worst_shape_the_app_can_make() {
    // Tall, high-latitude, whole-ring: the most Mercator can bend between a
    // rect's centre and its poleward edge in this application.
    let rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(450.0, 900.0));
    let centre = walkers::lat_lon(64.8, -147.5);
    let want = half(460.125, 460.125);

    let mut memory = walkers::MapMemory::default();
    let mut shortfalls = Vec::new();
    for _ in 0..MAX_FRAMING_PASSES {
        let covered = ground_half_extent(rect, &memory, centre).expect("measurable");
        let shortfall = (want.east_km / covered.east_km).max(want.north_km / covered.north_km);
        shortfalls.push(shortfall);
        memory
            .set_zoom(memory.zoom() - shortfall.log2())
            .expect("inside walkers' range");
    }

    // The approach direction the margin is built on: never below 1.0.
    assert!(
        shortfalls.iter().all(|s| *s >= 1.0),
        "the solve crossed below exact coverage, so `COVERAGE_MARGIN` is slack \
         rather than the direction the solve is wrong in: {shortfalls:?}",
    );
    // And the budget: settled inside the margin with passes still in hand.
    let settled = shortfalls
        .iter()
        .position(|s| *s - 1.0 <= COVERAGE_MARGIN)
        .expect("the solve must settle inside the margin at all");
    assert!(
        settled + 1 < MAX_FRAMING_PASSES,
        "the solve took {} of {MAX_FRAMING_PASSES} passes to reach the margin, \
         leaving no room for a latitude or a pane shape nobody has thought of: \
         {shortfalls:?}",
        settled + 1,
    );
}

/// A box or a pane that cannot be framed is refused, so the caller falls back to
/// the pane's own map memory rather than to a viewport built from a `NaN`.
///
/// The extent is checked for being **positive** as well as finite: a zero-width
/// box divides to an infinity in the solve, and an infinite zoom step reaches
/// `set_zoom` as a `NaN` that walkers accepts as a number.
#[test]
fn an_unframable_box_or_pane_is_refused() {
    let centre = walkers::lat_lon(35.33, -97.28);
    let rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(700.0, 450.0));
    let flat = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(700.0, 0.0));

    assert!(
        viewport_for_region(flat, centre, half(120.0, 75.0)).is_none(),
        "a pane collapsed to nothing by a divider drag has no viewport to frame",
    );
    for bad in [
        half(0.0, 75.0),
        half(120.0, 0.0),
        half(-120.0, 75.0),
        half(f64::NAN, 75.0),
        half(120.0, f64::INFINITY),
    ] {
        assert!(
            viewport_for_region(rect, centre, bad).is_none(),
            "{bad:?} was framed rather than refused",
        );
    }
}

/// A framed strip is centred on the box, not on wherever the pane's map was
/// left.
///
/// The centre is the other half of "the two clips are the same rectangle": a
/// strip zoomed out far enough to cover a 920 km box but centred 400 km away
/// still leaves a side of the box transparent. It is asserted through the
/// projector rather than off `MapMemory`, because what the shader ends up
/// sampling is the projection.
#[test]
fn a_framed_strip_is_centred_on_the_box() {
    let rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(700.0, 450.0));
    let centre = walkers::lat_lon(35.33, -97.28);
    let memory = viewport_for_region(rect, centre, half(120.0, 75.0)).expect("framable");
    let projector = walkers::Projector::new(rect, &memory, centre);
    let middle = projector.unproject(rect.center().to_vec2());
    let (_, off_km) =
        rustdar_radar::beam::site_bearing_range_km(centre.y(), centre.x(), middle.y(), middle.x());
    assert!(
        off_km < 0.001,
        "the strip's middle is {off_km} km from the box's centre",
    );
    assert!(
        memory.detached().is_none(),
        "a detached memory would centre the strip on the pane's last pan \
         instead of on the box",
    );
}
