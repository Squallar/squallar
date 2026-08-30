//! The ladder's own tests: totality, every ceiling, the aspect that is not
//! square, and the two halves of the frame-thread split.
//!
//! **Every criterion here that has a "the plan is right" half also has a half
//! that fails against a plausible wrong plan.** B1 shipped three vacuity holes,
//! each in a test written to close the previous one, and B3's `ground_box` lane
//! shipped with no coverage at all because every fixture sat on the identity.
//! So a fixture that cannot tell the two answers apart is not a fixture here.

use super::*;

/// The Colorado front range, where the archive's two bracket tiles were
/// measured and where the relief is real.
const SITE: (f64, f64) = (39.0, -106.0);

/// The shipped 920 km reflectivity reach, square.
fn box_920() -> Footprint {
    Footprint {
        x_km: (-460.0, 460.0),
        y_km: (-460.0, 460.0),
    }
}

/// `HalfExtentKm::clamped` floors each axis at 10 km and then bounds the
/// corner, so this rectangle — **1329 km by 20 km, 66.45:1** — is a box a user
/// can actually reach. Every aspect criterion is written against it.
fn box_66_to_1() -> Footprint {
    Footprint {
        x_km: (-664.5, 664.5),
        y_km: (-10.0, 10.0),
    }
}

/// A desktop adapter's ceilings against the plan's 2 MiB height texture.
fn desktop() -> HeightCeilings {
    HeightCeilings {
        texture_bytes: 2 * 1024 * 1024,
        max_tiles: DEFAULT_MAX_TILES,
        max_texture_dimension: 32_768,
        max_zoom: 11,
        tile_px: 256,
    }
}

/// The WebGL2 downlevel default: 2048, and the reason the texture ceiling is a
/// runtime figure rather than a `cfg`.
fn webgl2_downlevel() -> HeightCeilings {
    HeightCeilings {
        max_texture_dimension: 2048,
        ..desktop()
    }
}

/// The 3D pane's own field of view (`squallar_egui::volume_view::FOV_Y_DEG`),
/// passed in rather than restated in the crate under test.
const FOV_Y_DEG: f64 = 40.0;

/// `OrbitCamera::eye_distance` is in framing radii, and a framing radius is
/// half the diagonal of the square the box's NORTH extent stands on —
/// `north / sqrt(2)` (`volume_view::framing_radius_km`).
fn framing_radius_km(footprint: &Footprint) -> f64 {
    footprint.extent_km().1 / std::f64::consts::SQRT_2
}

fn camera_at(standoff: f64, pitch_deg: f64, yaw_deg: f64, drawn: Footprint) -> CameraFootprint {
    CameraFootprint {
        drawn,
        distance_km: standoff * framing_radius_km(&drawn),
        fov_y_deg: FOV_Y_DEG,
        aspect: 16.0 / 10.0,
        pitch_deg,
        yaw_deg,
        pivot: [0.0, 0.0],
    }
}

/// `eye_distance_for_plan_scale()`: sqrt(2) / (2 tan(fov/2)).
fn default_standoff() -> f64 {
    std::f64::consts::SQRT_2 / (2.0 * (0.5 * FOV_Y_DEG.to_radians()).tan())
}

/// `MIN_EYE_DISTANCE`, the zoom stop.
const MIN_EYE_DISTANCE: f64 = 0.05;

// ---------------------------------------------------------------- footprint

/// **At the default standoff the camera sees the whole box, so the field is the
/// box and `ground_box` is the identity.**
///
/// That is not a coincidence to be tolerated, it is what the default standoff
/// is *derived* to be: `eye_distance_for_plan_scale` solves for the distance at
/// which the pane shows exactly the box's north extent. So the settled
/// whole-box frame — the one every other suite in the tree renders — must come
/// out of this module untouched.
#[test]
fn the_default_standoff_sees_the_whole_box_and_places_the_identity() {
    for yaw in [0.0, 45.0, 137.0, 225.0, 310.0] {
        for pitch in [8.0, 25.0, 60.0, 85.0] {
            let camera = camera_at(default_standoff(), pitch, yaw, box_920());
            let visible = camera.visible();
            assert_eq!(
                visible,
                box_920(),
                "at the default standoff, yaw {yaw} pitch {pitch} did not see the whole box",
            );
            assert_eq!(
                visible.ground_box_in(&box_920()),
                Some([1.0, 1.0, 0.0, 0.0]),
                "the whole-box footprint is not the identity placement",
            );
        }
    }
}

/// The non-triviality half of the test above: the zoom stop must NOT see the
/// whole box, or "the footprint is the box" would be a function that ignores
/// its camera.
///
/// The figures are the plan's own: the stop is about 39 times closer than the
/// default, and a 512-post whole-box grid would put about thirteen posts across
/// what is on screen.
#[test]
fn the_zoom_stop_sees_a_fraction_of_the_box_and_places_a_sub_rectangle() {
    let camera = camera_at(MIN_EYE_DISTANCE, 25.0, 225.0, box_920());
    let visible = camera.visible();
    let (ex, _) = visible.extent_km();
    let (bx, _) = box_920().extent_km();
    assert!(
        ex < bx / 10.0,
        "the zoom stop saw {ex:.1} km of a {bx:.0} km box, which is not a re-fit worth doing",
    );
    let placed = visible.ground_box_in(&box_920()).expect("a placement");
    assert!(
        placed[0] < 0.1 && placed[1] < 0.1,
        "the zoom stop's placement {placed:?} is not a sub-rectangle",
    );
    // **The plan's own arithmetic, and it is the OVERHEAD camera's.** "A
    // 512-post grid gives about thirteen posts across the view" is the
    // un-turned, un-inclined rectangle: `2 d tan(fov/2)` over the box. An
    // oblique camera legitimately sees more ground than that, because the
    // ground is inclined to it and the viewport is turned on it, so the figure
    // is asserted where it is actually the answer.
    let overhead = camera_at(MIN_EYE_DISTANCE, 89.0, 0.0, box_920()).visible();
    let posts_across_the_view = 512.0 * overhead.extent_km().1 / bx;
    assert!(
        (12.0..15.0).contains(&posts_across_the_view),
        "a whole-box 512-post grid gives {posts_across_the_view:.1} posts across \
         the overhead view, not the ~13 the plan is built on",
    );
    // And the oblique camera is not somehow seeing less than the overhead one.
    assert!(
        512.0 * ex / bx > posts_across_the_view,
        "an oblique camera saw less ground than an overhead one at the same standoff",
    );
}

/// The standoff is what drives the footprint, monotonically, and the ratio at
/// the ends is the ~39x the plan names.
#[test]
fn the_footprint_shrinks_with_the_standoff() {
    let extent = |standoff: f64| {
        camera_at(standoff, 89.0, 0.0, box_920())
            .visible()
            .extent_km()
            .0
    };
    let mut previous = f64::MAX;
    for standoff in [8.0, 4.0, 2.0, 1.0, 0.5, 0.2, 0.1, MIN_EYE_DISTANCE] {
        let now = extent(standoff);
        assert!(
            now <= previous,
            "standoff {standoff} widened the footprint from {previous:.2} to {now:.2} km",
        );
        previous = now;
    }
    let ratio = default_standoff() / MIN_EYE_DISTANCE;
    assert!(
        (38.0..40.0).contains(&ratio),
        "the zoom stop is {ratio:.1}x the default standoff, not the ~39x the plan is built on",
    );
}

/// A shallower pitch sees more ground, because the ground is more nearly
/// edge-on. The `1 / sin(pitch)` term, held as a property rather than as its
/// own arithmetic.
#[test]
fn a_shallower_pitch_sees_more_ground() {
    let extent = |pitch: f64| {
        camera_at(0.3, pitch, 0.0, box_920())
            .visible()
            .extent_km()
            .1
    };
    let mut previous = 0.0;
    for pitch in [89.0, 60.0, 40.0, 25.0, 12.0] {
        let now = extent(pitch);
        assert!(
            now >= previous,
            "pitch {pitch} saw less ground ({now:.1} km) than the steeper one ({previous:.1} km)",
        );
        previous = now;
    }
    // And the horizon is the box, not an infinity: a level camera clips.
    let level = camera_at(0.3, 0.0, 0.0, box_920()).visible();
    assert_eq!(
        level,
        box_920(),
        "a level camera did not clip to the box, so the 1/sin(pitch) term escaped",
    );
}

/// Yaw turns the footprint on the ground, and the axis-aligned bound of a
/// turned rectangle is widest on the diagonal.
#[test]
fn yaw_turns_the_footprint_and_the_bound_is_widest_on_the_diagonal() {
    let at = |yaw: f64| camera_at(0.2, 89.0, yaw, box_920()).visible().extent_km();
    let (north_x, north_y) = at(0.0);
    let (diagonal_x, _) = at(45.0);
    let (east_x, east_y) = at(90.0);
    // A near-overhead camera at yaw 0 has its wide axis east, at yaw 90 north.
    assert!(
        north_x > north_y * 1.2,
        "yaw 0 gave {north_x:.1} x {north_y:.1}, which is not a landscape viewport",
    );
    assert!(
        east_y > east_x * 1.2,
        "yaw 90 did not turn the viewport: {east_x:.1} x {east_y:.1}",
    );
    assert!(
        diagonal_x > north_x,
        "the diagonal bound {diagonal_x:.1} is not wider than the axis-aligned {north_x:.1}",
    );
}

/// The pivot moves the footprint with it, and never off the box.
#[test]
fn the_pivot_carries_the_footprint_and_the_clip_holds_it_in_the_box() {
    let mut camera = camera_at(0.2, 89.0, 0.0, box_920());
    camera.pivot = [0.0, 0.0];
    let centred = camera.visible().centre_km();
    camera.pivot = [1.0, -1.0];
    let corner = camera.visible();
    assert!(
        corner.centre_km().0 > centred.0 && corner.centre_km().1 < centred.1,
        "the pivot did not carry the footprint: {:?} against {centred:?}",
        corner.centre_km(),
    );
    let b = box_920();
    assert!(
        corner.x_km.0 >= b.x_km.0
            && corner.x_km.1 <= b.x_km.1
            && corner.y_km.0 >= b.y_km.0
            && corner.y_km.1 <= b.y_km.1,
        "a pivot at the box's own corner put the footprint outside it: {corner:?}",
    );
}

/// Every non-finite way in answers the drawn box rather than a rectangle of
/// `NaN`s. Total, at the boundary where the camera's numbers come from a
/// division by a viewport a frame wide.
#[test]
fn an_unusable_camera_answers_the_whole_box() {
    let base = camera_at(0.2, 25.0, 0.0, box_920());
    let broken = [
        CameraFootprint {
            distance_km: f64::NAN,
            ..base
        },
        CameraFootprint {
            distance_km: 0.0,
            ..base
        },
        CameraFootprint {
            distance_km: -5.0,
            ..base
        },
        CameraFootprint {
            aspect: f64::INFINITY,
            ..base
        },
        CameraFootprint {
            aspect: 0.0,
            ..base
        },
        CameraFootprint {
            fov_y_deg: 0.0,
            ..base
        },
        CameraFootprint {
            fov_y_deg: 180.0,
            ..base
        },
        CameraFootprint {
            pitch_deg: f64::NAN,
            ..base
        },
        CameraFootprint {
            yaw_deg: f64::NAN,
            ..base
        },
        CameraFootprint {
            pivot: [f64::NAN, 0.0],
            ..base
        },
    ];
    for camera in broken {
        assert_eq!(
            camera.visible(),
            box_920(),
            "an unusable camera did not fall back to the drawn box: {camera:?}",
        );
    }
}

// --------------------------------------------------------------- the ladder

/// The ladder answers for every rung, every ceiling and every reachable
/// footprint. Total by construction, held as a sweep.
#[test]
fn the_ladder_always_answers() {
    let mut limits = std::collections::BTreeSet::new();
    for drawn in [box_920(), box_66_to_1()] {
        for standoff in [8.0, default_standoff(), 1.0, 0.4, 0.1, MIN_EYE_DISTANCE] {
            for pitch in [89.0, 60.0, 25.0, 8.0] {
                for yaw in [0.0, 45.0, 225.0] {
                    let visible = camera_at(standoff, pitch, yaw, drawn).visible();
                    for ceilings in [desktop(), webgl2_downlevel()] {
                        let plan = HeightPlan::fit(SITE, visible, ceilings)
                            .expect("a Colorado footprint has tiles");
                        limits.insert(plan.limit);
                        // **A literal two, not `MIN_POSTS_PER_AXIS`.** F8:
                        // using the code's own floor as the test's threshold
                        // makes the pair move together, and `2 -> 1` survives.
                        // Two is what
                        // `squallar_volumetric::raymarch::upload_heights`
                        // refuses below, and that is the number this has to be
                        // compared against.
                        assert!(
                            plan.posts[0] >= 2 && plan.posts[1] >= 2,
                            "{plan:?} has an axis `upload_heights` would refuse",
                        );
                        assert!(
                            plan.posts[0] <= ceilings.post_ceiling()
                                && plan.posts[1] <= ceilings.post_ceiling(),
                            "{plan:?} passed the adapter's ceiling",
                        );
                        assert!(!plan.cover.is_empty(), "{plan:?} names no tiles");
                    }
                }
            }
        }
    }
    // Non-triviality: a sweep that only ever lands on one arm would pass this
    // whatever the other arms did.
    assert!(
        limits.len() >= 3,
        "the whole sweep only reached {limits:?}, so most of the ladder is untested here",
    );
}

/// **Where the implied zoom would pass the archive, the posts come down.**
///
/// The plan's own rule, and it is the arm the zoom stop lands on: a 24 km
/// footprint at rung 2048 wants a post every 12 m, which is z14, and the
/// archive stops at z11. The plan must answer z11 and fewer posts, never z11
/// with the posts it wanted.
#[test]
fn a_footprint_finer_than_the_archive_loses_posts_and_not_zoom() {
    let visible = camera_at(MIN_EYE_DISTANCE, 89.0, 0.0, box_920()).visible();
    let plan = HeightPlan::fit(SITE, visible, desktop()).expect("a plan");
    assert_eq!(
        plan.limit,
        PlanLimit::Archive,
        "the zoom stop was not archive-bound: {plan:?}",
    );
    assert_eq!(plan.cover.zoom, desktop().max_zoom);
    assert!(
        plan.posts[0] < posts_at_ceiling(visible, desktop().post_ceiling())[0],
        "the archive ceiling did not cost any posts: {plan:?}",
    );
    // And the posts it kept are the ones z11 actually resolves, to within a
    // post: spacing is the tile pixel, not a fraction of it.
    let pixel_km = tile_pixel_km(desktop().max_zoom, SITE.0, desktop().tile_px);
    let (spacing_x, _) = plan.post_spacing_km();
    assert!(
        (spacing_x / pixel_km - 1.0).abs() < 0.05,
        "post spacing {spacing_x:.4} km is not the z11 pixel {pixel_km:.4} km: {plan:?}",
    );
    // The non-triviality half: raising the archive ceiling MUST buy posts. If
    // it does not, `Archive` is a label on an arm that is not doing anything.
    let deeper = HeightCeilings {
        max_zoom: 14,
        texture_bytes: 64 * 1024 * 1024,
        max_tiles: 4096,
        ..desktop()
    };
    let richer = HeightPlan::fit(SITE, visible, deeper).expect("a plan");
    // Three zoom levels deeper is eight times the posts on each axis, because
    // a zoom level is twice the ground per tile pixel. Measured: [638, 399] at
    // z11 against [5108, 3193] at z14.
    assert_eq!(
        richer.limit,
        PlanLimit::Archive,
        "with 64 MiB and 4096 tiles the archive is still what binds: {richer:?}",
    );
    assert!(
        richer.posts[0] >= plan.posts[0] * 7 && richer.posts[0] <= plan.posts[0] * 9,
        "z14 bought {:?} posts against z11's {:?}; three zoom levels is eight \
         times, not this",
        richer.posts,
        plan.posts,
    );
}

/// The texture budget is real: halving it must cost posts, and the plan must
/// say so.
#[test]
fn the_texture_budget_costs_posts_and_names_itself() {
    let visible = camera_at(0.6, 89.0, 0.0, box_920()).visible();
    let generous = HeightCeilings {
        texture_bytes: 32 * 1024 * 1024,
        max_tiles: 4096,
        max_zoom: 15,
        ..desktop()
    };
    let plan = HeightPlan::fit(SITE, visible, generous).expect("a plan");
    let mean = HeightCeilings {
        texture_bytes: 64 * 1024,
        ..generous
    };
    let cut = HeightPlan::fit(SITE, visible, mean).expect("a plan");
    assert!(
        cut.texture_bytes() <= mean.texture_bytes,
        "the fit did not come inside its own budget: {cut:?}",
    );
    // **The byte ceiling is SOLVED, not compared**, so the `>` against `>=`
    // question the tile ceiling has does not arise here — there is no
    // comparison to get wrong. What replaced it is an invariant worth pinning
    // instead: the solved grid never exceeds the budget and does not leave much
    // of it, at every budget across four orders of magnitude. A `floor` on each
    // axis is the only slack, so the shortfall is at most a post a side.
    for texture_bytes in [
        8_192usize,
        65_536,
        524_288,
        2 * 1024 * 1024,
        32 * 1024 * 1024,
    ] {
        let plan = HeightPlan::fit(
            SITE,
            visible,
            HeightCeilings {
                texture_bytes,
                ..generous
            },
        )
        .expect("a plan");
        assert!(
            plan.texture_bytes() <= texture_bytes,
            "a {texture_bytes}-byte budget bought {} bytes: {plan:?}",
            plan.texture_bytes(),
        );
        if plan.limit == PlanLimit::TextureBytes {
            assert!(
                plan.texture_bytes() * 100 >= texture_bytes * 95,
                "a {texture_bytes}-byte budget was cut to {} bytes and still \
                 called itself budget-limited: {plan:?}",
                plan.texture_bytes(),
            );
        }
    }
    assert!(
        cut.posts[0] < plan.posts[0],
        "a 32x smaller budget cost no posts: {cut:?} against {plan:?}",
    );
    assert_eq!(cut.limit, PlanLimit::TextureBytes, "{cut:?}");
}

/// The tile ceiling is real, and it binds before the byte budget does when the
/// footprint is wide.
#[test]
fn the_tile_ceiling_costs_posts_and_names_itself() {
    let visible = box_920();
    let roomy = HeightCeilings {
        texture_bytes: 64 * 1024 * 1024,
        max_tiles: 4096,
        max_zoom: 15,
        ..desktop()
    };
    let plan = HeightPlan::fit(SITE, visible, roomy).expect("a plan");
    let pinched = HeightCeilings {
        max_tiles: 9,
        ..roomy
    };
    let cut = HeightPlan::fit(SITE, visible, pinched).expect("a plan");
    assert!(
        cut.cover.len() <= pinched.max_tiles,
        "the fit named {} tiles against a ceiling of 9: {cut:?}",
        cut.cover.len(),
    );
    // **The ceilings are INCLUSIVE, and that is pinned rather than left to a
    // comparison operator.** Turning either `<=` into `<` survived the whole
    // suite, which made the effective budgets 63 tiles and one byte under —
    // small, but the kind of thing that makes a measured figure disagree with
    // the constant beside it. A cover of exactly the ceiling is allowed.
    let exact = HeightCeilings {
        max_tiles: cut.cover.len(),
        ..pinched
    };
    let at_the_line = HeightPlan::fit(SITE, visible, exact).expect("a plan");
    assert_eq!(
        at_the_line.cover.len(),
        cut.cover.len(),
        "a tile count exactly at the ceiling was refused, so the ceiling is \
         really one tile lower than it says",
    );
    assert!(
        cut.posts[0] < plan.posts[0],
        "the tile ceiling cost no posts: {cut:?} against {plan:?}",
    );
    assert_eq!(cut.limit, PlanLimit::TileCount, "{cut:?}");
}

/// **`max_texture_dimension_2d` is read at run time, and the two figures that
/// matter give two different plans.**
///
/// `downlevel_webgl2_defaults()` guarantees 2048; `device_limits` lifts the
/// trio to the adapter's own figure through `using_resolution`, and Firefox
/// reports 32768 on a real driver. A `cfg` cannot tell those apart — the same
/// wasm build runs on both — so the ladder has to.
#[test]
fn the_adapter_texture_ceiling_is_a_runtime_figure_and_changes_the_answer() {
    // A footprint wide enough that the finest rung is what a big adapter would
    // give, with every other ceiling opened out so this one is the only one
    // that can bind.
    let open = HeightCeilings {
        texture_bytes: 512 * 1024 * 1024,
        max_tiles: 65_536,
        max_zoom: 16,
        ..desktop()
    };
    let visible = camera_at(0.5, 89.0, 0.0, box_920()).visible();
    let big = HeightPlan::fit(
        SITE,
        visible,
        HeightCeilings {
            max_texture_dimension: 32_768,
            ..open
        },
    )
    .expect("a plan");
    let small = HeightPlan::fit(
        SITE,
        visible,
        HeightCeilings {
            max_texture_dimension: 512,
            ..open
        },
    )
    .expect("a plan");
    assert!(
        big.posts.iter().max() > small.posts.iter().max(),
        "the adapter's ceiling did not change the plan: {big:?} against {small:?}",
    );
    assert!(
        small.posts.iter().all(|p| *p <= 512),
        "a 512-limit adapter was handed {small:?}",
    );
    // **The WebGL2 downlevel guarantee of 2048 does NOT bind on a square box at
    // the shipped budget, and saying it did was false.** A 2 MiB texture buys
    // 1024 posts a side on a square footprint, so the byte budget is always the
    // tighter of the two and a 2048-post adapter and a 32768-post one answer
    // the same field. The review measured that over 2450 plans: zero
    // differences. Where it *does* bind is an extreme aspect, which is
    // `the_webgl2_downlevel_ceiling_binds_where_the_aspect_is_extreme`.
    let downlevel = HeightPlan::fit(SITE, visible, webgl2_downlevel()).expect("a plan");
    assert_eq!(
        downlevel.posts,
        HeightPlan::fit(SITE, visible, desktop_open())
            .expect("a plan")
            .posts,
        "a square footprint at the shipped budget was changed by the adapter \
         ceiling after all; the paragraph above needs re-deriving, not deleting",
    );
}

/// Ceilings identical to [`webgl2_downlevel`] but with a desktop adapter, so
/// the two differ in exactly one field.
fn desktop_open() -> HeightCeilings {
    HeightCeilings {
        max_texture_dimension: 32_768,
        ..webgl2_downlevel()
    }
}

/// **Where the WebGL2 downlevel ceiling actually binds: an extreme aspect.**
///
/// F5. The first version of this file asserted that 2048 was "a real constraint
/// on the plan's own sweep", and it was not: on a square footprint a 2 MiB
/// budget allows 1024 posts a side, so the budget is always tighter and a
/// browser and a desktop get identical fields. The tests that *did* show a
/// ceiling biting used 512 and 64 — both below the downlevel guarantee, so
/// neither said anything about a real browser.
///
/// A 66:1 footprint is different: the same 2 MiB spread over a long thin
/// rectangle buys thousands of posts on the long axis, and there the adapter's
/// own figure is what stops it. This is the case that makes
/// [`PlanLimit::TextureDimension`] reachable at a number a real device reports.
#[test]
fn the_webgl2_downlevel_ceiling_binds_where_the_aspect_is_extreme() {
    let footprint = box_66_to_1();
    let browser = HeightPlan::fit(SITE, footprint, webgl2_downlevel()).expect("a plan");
    let desktop = HeightPlan::fit(SITE, footprint, desktop_open()).expect("a plan");
    assert!(
        desktop.posts[0] > browser.posts[0],
        "a 32768-post adapter answered no more than a 2048-post one over a 66:1 \
         box: {desktop:?} against {browser:?}",
    );
    assert!(
        browser.posts[0] <= 2048,
        "the downlevel guarantee was exceeded: {browser:?}",
    );
    // And the plan says so, rather than the caller having to infer it.
    assert_eq!(browser.limit, PlanLimit::TextureDimension, "{browser:?}");
    // Non-triviality: the two runs differ in exactly ONE field, so nothing but
    // the adapter's own figure can explain the difference between them.
    assert_eq!(
        HeightCeilings {
            max_texture_dimension: HeightCeilings::WEBGL2_DOWNLEVEL_DIMENSION,
            ..desktop_open()
        },
        webgl2_downlevel(),
        "the two ceiling sets differ in more than the adapter figure, so the          comparison above is not attributable to it",
    );
    assert_eq!(
        webgl2_downlevel().max_texture_dimension,
        HeightCeilings::WEBGL2_DOWNLEVEL_DIMENSION,
    );
}

/// **The adapter's ceiling is one of the three closed forms the base is solved
/// from, not something the ladder steps down for.**
///
/// It used to be both: the fit also carried a `too_wide` arm that stepped the
/// rung down for an over-wide candidate, and a mutation run replacing it with
/// `false` killed no test — the clamp was already doing the work. The rewrite
/// that fixed F1 removed the arm entirely rather than keeping a guard nothing
/// could reach, because the base is now *solved* from all three ceilings at
/// once. Stepping the rung for a small adapter would have answered the same
/// clamped post count at every rung anyway, several boundary walks later.
///
/// So what is pinned here is the ceiling doing its own work: the posts come out
/// under it, and the rung stays where the tile count left it.
#[test]
fn the_adapter_ceiling_clamps_the_posts_rather_than_dropping_the_rung() {
    let tiny = HeightCeilings {
        max_texture_dimension: 64,
        texture_bytes: 512 * 1024 * 1024,
        max_tiles: 65_536,
        max_zoom: 16,
        ..desktop()
    };
    let plan = HeightPlan::fit(SITE, box_920(), tiny).expect("a plan");
    assert_eq!(plan.posts, [64, 64], "{plan:?}");
    assert_eq!(
        plan.rung,
        PostRung::FINEST,
        "the ladder stepped down for an adapter ceiling the clamp had already \
         handled, which is the same field several boundary walks later: {plan:?}",
    );
    // The clamp bounds the derived axis too, not only the rung's own.
    let wide = HeightPlan::fit(SITE, box_66_to_1(), tiny).expect("a plan");
    assert!(
        wide.posts.iter().all(|p| *p <= 64 && *p >= 2),
        "a 66:1 footprint escaped a 64-post adapter ceiling: {wide:?}",
    );
    // Non-triviality: without a ceiling this low the same footprint is much
    // finer, so the clamp is what produced the numbers above.
    let open = HeightPlan::fit(
        SITE,
        box_920(),
        HeightCeilings {
            max_texture_dimension: 32_768,
            ..tiny
        },
    )
    .expect("a plan");
    assert!(open.posts[0] > 64, "{open:?}");
    // And `post_ceiling` never answers below the floor a field can be drawn at,
    // however small the adapter claims to be.
    assert_eq!(
        HeightCeilings {
            max_texture_dimension: 0,
            ..tiny
        }
        .post_ceiling(),
        2,
        "an adapter reporting nothing must still leave a field `upload_heights` \
         would accept",
    );
}

// ----------------------------------------------------------------- 66:1

/// **A 66:1 box is covered, on both axes, at every camera.**
///
/// The plan's Done-when. Posts are per-axis and the rung names the long one, so
/// what has to hold is that the short axis never collapses and the spacing
/// comes out very nearly the same on both — a field that is 2048 by 2 has a
/// technically legal texture and 300 km between posts on one axis.
#[test]
fn a_sixty_six_to_one_box_gets_isotropic_posts_and_no_degenerate_axis() {
    let mut worst_anisotropy = 1.0f64;
    let mut seen = 0usize;
    for standoff in [8.0, default_standoff(), 1.0, 0.3, MIN_EYE_DISTANCE] {
        for pitch in [89.0, 45.0, 8.0] {
            for yaw in [0.0, 90.0, 225.0] {
                let visible = camera_at(standoff, pitch, yaw, box_66_to_1()).visible();
                for ceilings in [desktop(), webgl2_downlevel()] {
                    let plan = HeightPlan::fit(SITE, visible, ceilings).expect("a plan");
                    seen += 1;
                    // A literal two: see F8 above. `upload_heights` refuses
                    // an axis under two, and that is the threshold — not
                    // whatever this crate happens to call its own floor, which
                    // would move with it.
                    assert!(
                        plan.posts[0] >= 2 && plan.posts[1] >= 2,
                        "a 66:1 box produced a texture `upload_heights` would \
                         refuse {:?}: {plan:?}",
                        plan.posts,
                    );
                    let (sx, sy) = plan.post_spacing_km();
                    let anisotropy = (sx / sy).max(sy / sx);
                    worst_anisotropy = worst_anisotropy.max(anisotropy);
                }
            }
        }
    }
    assert!(seen >= 30, "only {seen} plans were fitted");
    // Rounding to whole posts is the only thing that can make the two spacings
    // differ, and it is worst where an axis has fewest posts. 1.6 is generous
    // for the rounding and far under the 66 an axis-blind rung would give.
    assert!(
        worst_anisotropy < 1.6,
        "post spacing on a 66:1 box came out {worst_anisotropy:.2}:1 anisotropic",
    );
}

/// The non-triviality half of the test above: a rung applied to **both** axes
/// blindly — the obvious wrong implementation — is refused or wasteful, so the
/// criterion above is not something any plan would pass.
#[test]
fn a_square_grid_over_a_sixty_six_to_one_box_would_be_the_wrong_answer() {
    let footprint = box_66_to_1();
    let (ex, ey) = footprint.extent_km();
    let square = 512u32;
    let (sx, sy) = (ex / f64::from(square), ey / f64::from(square));
    assert!(
        (sx / sy) > 60.0,
        "the fixture is not actually anisotropic: {sx:.3} against {sy:.3} km",
    );
    // And the plan's own answer over the same footprint is not that.
    let plan = HeightPlan::fit(SITE, footprint, desktop()).expect("a plan");
    assert_ne!(
        plan.posts[0], plan.posts[1],
        "the fit gave a square grid over a 66:1 footprint: {plan:?}",
    );
    assert!(
        plan.posts[0] > plan.posts[1] * 10,
        "the fit did not put its posts on the long axis: {plan:?}",
    );
}

/// The grid `squallar_volumetric::raymarch::ground_vertex_count` draws is
/// `6 * (px + 1) * (py + 1)`, and it must fit a `u32` for every plan this
/// module can produce — including the extreme aspects, where one axis is at the
/// adapter's own ceiling.
#[test]
fn every_plan_this_module_makes_is_drawable() {
    let open = HeightCeilings {
        texture_bytes: usize::MAX / 4,
        max_tiles: usize::MAX / 4,
        max_zoom: 16,
        max_texture_dimension: u32::MAX,
        tile_px: 256,
    };
    for footprint in [box_920(), box_66_to_1()] {
        let plan = HeightPlan::fit(SITE, footprint, open).expect("a plan");
        let vertices = 6u64 * (u64::from(plan.posts[0]) + 1) * (u64::from(plan.posts[1]) + 1);
        assert!(
            u32::try_from(vertices).is_ok(),
            "{plan:?} would need {vertices} vertices, which is not a u32 draw",
        );
        assert!(
            plan.posts.iter().all(|p| *p <= MAX_POSTS_PER_AXIS),
            "{plan:?} passed the crate's own post ceiling",
        );
    }
}

// ------------------------------------------------------- geography refusals

/// A footprint with no tiles at any zoom is a refusal and not a coarser plan:
/// no rung would have helped, so the ladder must not walk the whole way down
/// answering the same error nine times.
#[test]
fn a_footprint_with_no_tiles_is_refused_rather_than_coarsened() {
    // The antimeridian: `great_circle_destination` does not wrap, so a box
    // straddling it comes back as longitudes past 180.
    let err = HeightPlan::fit((0.0, 179.9), box_920(), desktop()).expect_err("no tiles here");
    assert!(
        matches!(err, ElevationError::CrossesAntimeridian),
        "{err:?}"
    );

    // Past Web Mercator's limit. A 920 km box up here would cross the pole and
    // be refused for the antimeridian first — the longitudes come back
    // unwrapped — so the fixture is a small box just under the limit whose
    // north edge is just over it: 84N + 150 km is 85.35, against the limit's
    // 85.05.
    let polar = Footprint {
        x_km: (-150.0, 150.0),
        y_km: (-150.0, 150.0),
    };
    let err = HeightPlan::fit((84.0, 0.0), polar, desktop()).expect_err("no tiles here");
    assert!(
        matches!(err, ElevationError::PastMercatorLimit { .. }),
        "{err:?}"
    );
    // Non-triviality: the same box a degree further south is fitted, so the
    // refusal above is the limit and not the fixture.
    assert!(HeightPlan::fit((82.0, 0.0), polar, desktop()).is_ok());

    // And an unusable footprint or tile size.
    for (footprint, ceilings) in [
        (
            Footprint {
                x_km: (0.0, 0.0),
                y_km: (-1.0, 1.0),
            },
            desktop(),
        ),
        (
            Footprint {
                x_km: (1.0, -1.0),
                y_km: (-1.0, 1.0),
            },
            desktop(),
        ),
        (
            Footprint {
                x_km: (f64::NAN, 1.0),
                y_km: (-1.0, 1.0),
            },
            desktop(),
        ),
        (
            box_920(),
            HeightCeilings {
                tile_px: 0,
                ..desktop()
            },
        ),
    ] {
        assert!(
            HeightPlan::fit(SITE, footprint, ceilings).is_err(),
            "{footprint:?} was fitted rather than refused",
        );
    }
}

// ------------------------------------------------------ off the frame thread

/// **The frame thread does not fit, and that is a count rather than a
/// sentence.**
///
/// `observe` is `O(1)` by inspection, which is exactly the kind of claim this
/// repository has watched turn out to be false. So the fit keeps an always-on
/// counter and this asserts the difference: ten thousand observations move it
/// by nothing, and one `resolve` moves it by one.
#[test]
fn observing_never_fits_and_resolving_always_does() {
    let mut planner = HeightPlanner::new();
    // **The per-thread counter, not the process total.** Other tests fit on
    // other threads while this one runs, so the global figure moves for
    // reasons that have nothing to do with `observe`; the property is about
    // this thread, and so is the instrument.
    let before = ledger::fits_here();
    let mut request = None;
    for step in 0..10_000u32 {
        // A camera that settles, moves, settles again — the whole behaviour,
        // ten thousand times over.
        let standoff = if (step / 64) % 2 == 0 { 0.4 } else { 0.9 };
        let visible = camera_at(standoff, 40.0, 225.0, box_920()).visible();
        if let Some(out) = planner.observe(SITE, visible, desktop()) {
            request = request.or(Some(out));
            // Left in flight deliberately for most of the run; landed
            // occasionally so the latch is not what is doing the work.
            if step % 3 == 0 {
                planner.landed(out.footprint);
            }
        }
    }
    assert_eq!(
        ledger::fits_here(),
        before,
        "observing the camera ten thousand times ran a fit. The frame thread \
         does the debounce and nothing else; a fit walks the whole boundary of \
         the post grid",
    );
    // The non-triviality half: the counter is not simply stuck.
    let request = request.expect("ten thousand observations produced no request at all");
    let _ = request.resolve().expect("a plan");
    assert_eq!(
        ledger::fits_here(),
        before + 1,
        "resolving a request did not move the fit counter, so the assertion \
         above is measuring an instrument that never fires",
    );
    // And the reportable total moved with it, so `fits()` is not a counter
    // nothing increments.
    assert!(ledger::fits() >= ledger::fits_here());
}

/// What `observe` hands off has to be able to leave the thread it was made on.
#[test]
fn a_fit_request_can_cross_to_the_worker() {
    fn assert_send_sync_static<T: Send + Sync + 'static>() {}
    assert_send_sync_static::<FitRequest>();
    assert_send_sync_static::<HeightPlan>();
    assert_send_sync_static::<Footprint>();
    assert_send_sync_static::<HeightCeilings>();
}

/// **The debounce is counted, not timed**, so this asserts the count: a request
/// arrives on the [`QUIET_OBSERVATIONS`]th consecutive observation and not
/// before.
#[test]
fn the_debounce_fires_on_the_nth_observation_and_not_the_nth_minus_one() {
    let mut planner = HeightPlanner::new();
    planner.landed(box_920());
    let want = camera_at(0.2, 40.0, 225.0, box_920()).visible();
    // **The first observation of a new footprint establishes it and is not
    // counted**, so the request arrives on observation number
    // `QUIET_OBSERVATIONS + 1` — the eighth *repeat*. Asserted as an equality
    // and not a range: an off-by-one here is a whole frame of extra latency or
    // a fetch one frame into a gesture, and both are behaviours somebody would
    // otherwise have to notice by feel.
    let mut fired_at = None;
    for step in 0..=(QUIET_OBSERVATIONS + 4) {
        if planner.observe(SITE, want, desktop()).is_some() {
            fired_at = Some(step);
            break;
        }
    }
    assert_eq!(
        fired_at,
        Some(QUIET_OBSERVATIONS),
        "the debounce did not fire on the observation the constant names",
    );
    assert_eq!(planner.quiet_observations(), QUIET_OBSERVATIONS);
}

/// A camera that never stops moving never fits — the property the debounce
/// exists for, and the one a wall-clock threshold would get wrong under load.
#[test]
fn a_camera_still_moving_never_hands_off_and_one_that_stops_does() {
    let mut moving = HeightPlanner::new();
    moving.landed(box_920());
    let mut standoff = 2.0;
    for _ in 0..1_000 {
        // Each step past the hysteresis, so no two observations agree.
        standoff /= RESCALE_TOLERANCE * 1.05;
        if standoff < MIN_EYE_DISTANCE {
            standoff = 2.0;
        }
        let want = camera_at(standoff, 40.0, 225.0, box_920()).visible();
        assert!(
            moving.observe(SITE, want, desktop()).is_none(),
            "a camera in continuous motion handed off a fit",
        );
    }
    // The non-triviality half, on the same planner: it fires the moment the
    // camera stops. Without this the test above would pass against a planner
    // that never fires at all.
    let settled = camera_at(0.35, 40.0, 225.0, box_920()).visible();
    let mut fired = false;
    for _ in 0..=(QUIET_OBSERVATIONS + 1) {
        fired |= moving.observe(SITE, settled, desktop()).is_some();
    }
    assert!(fired, "the camera stopped and no fit was ever handed off");
}

/// A camera resting where it already is asks for nothing, however long it
/// rests. The hysteresis half.
#[test]
fn a_camera_resting_on_the_settled_field_asks_for_nothing() {
    let settled = camera_at(0.4, 40.0, 225.0, box_920()).visible();
    let mut planner = HeightPlanner::new();
    planner.landed(settled);
    // Nudges inside the tolerance: a fraction of a percent of scale, and a
    // shift well under a quarter of the half-extent.
    let (ex, _) = settled.extent_km();
    for step in 0..1_000 {
        let jitter = (step as f64 % 7.0 - 3.0) * ex * 0.01;
        let want = Footprint {
            x_km: (settled.x_km.0 + jitter, settled.x_km.1 + jitter),
            y_km: settled.y_km,
        };
        assert!(
            planner.observe(SITE, want, desktop()).is_none(),
            "a resting camera asked for a re-fit at step {step}",
        );
    }
    // Non-triviality: a real move past the tolerance does ask.
    let moved = camera_at(0.1, 40.0, 225.0, box_920()).visible();
    let mut fired = false;
    for _ in 0..=(QUIET_OBSERVATIONS + 1) {
        fired |= planner.observe(SITE, moved, desktop()).is_some();
    }
    assert!(fired, "a real move past the hysteresis asked for nothing");
}

/// **The hysteresis is per-axis, and a 66:1 footprint is what proves it.**
///
/// F7 — the identity-fixture defect, reopened in a new lane. Every other
/// planner test here used the square `box_920()`, and on a square footprint the
/// two half-extents are equal, so `is_materially` reading the *x* half-extent
/// for both axes is invisible. It is not invisible on a pancake: a 66:1
/// footprint 20 km tall would let its north centre drift **166 km — eight times
/// the footprint's own height** — before the planner counted it as somewhere
/// new, and the field on screen would be of ground the camera had left
/// entirely.
///
/// Asserted from both directions, because "it re-fits" and "it does not re-fit
/// too eagerly" are different failures and a test with only one half passes
/// against a planner that always fires.
#[test]
fn the_hysteresis_measures_each_axis_against_its_own_extent() {
    let settled = box_66_to_1();
    let (ex, ey) = settled.extent_km();
    assert!(ex > ey * 60.0, "the fixture is not a pancake: {settled:?}");

    // A north shift of a third of the SHORT axis is a different footprint --
    // it is past `SHIFT_TOLERANCE` of that axis's own half-extent. A planner
    // measuring both axes against `ex` would need 166 km to notice this.
    let shifted_north = Footprint {
        x_km: settled.x_km,
        y_km: (settled.y_km.0 + ey / 3.0, settled.y_km.1 + ey / 3.0),
    };
    let mut planner = HeightPlanner::new();
    planner.landed(settled);
    let mut fired = false;
    for _ in 0..=(QUIET_OBSERVATIONS + 1) {
        fired |= planner.observe(SITE, shifted_north, desktop()).is_some();
    }
    assert!(
        fired,
        "a north shift of {:.1} km on a footprint {:.1} km tall was not a new \
         footprint. The north axis is being measured against the EAST extent",
        ey / 3.0,
        ey,
    );

    // The other half: the same shift as a fraction of the LONG axis is well
    // inside tolerance on both, and must not re-fit.
    let nudged = Footprint {
        x_km: settled.x_km,
        y_km: (settled.y_km.0 + ey * 0.05, settled.y_km.1 + ey * 0.05),
    };
    let mut resting = HeightPlanner::new();
    resting.landed(settled);
    for step in 0..200 {
        assert!(
            resting.observe(SITE, nudged, desktop()).is_none(),
            "a 5% nudge on the short axis asked for a re-fit at step {step}",
        );
    }

    // And the same, on the east axis, so neither axis is the one being
    // measured for both.
    let shifted_east = Footprint {
        x_km: (settled.x_km.0 + ex / 3.0, settled.x_km.1 + ex / 3.0),
        y_km: settled.y_km,
    };
    let mut east = HeightPlanner::new();
    east.landed(settled);
    let mut fired_east = false;
    for _ in 0..=(QUIET_OBSERVATIONS + 1) {
        fired_east |= east.observe(SITE, shifted_east, desktop()).is_some();
    }
    assert!(
        fired_east,
        "an east shift of a third of the long axis was ignored"
    );
}

/// One fit is out at a time, and a failed one does not cost another eight
/// observations to retry.
#[test]
fn the_in_flight_latch_holds_one_fit_and_reopens_on_both_outcomes() {
    let mut planner = HeightPlanner::new();
    planner.landed(box_920());
    let want = camera_at(0.2, 40.0, 225.0, box_920()).visible();
    let mut fired = 0usize;
    for _ in 0..200 {
        if planner.observe(SITE, want, desktop()).is_some() {
            fired += 1;
        }
    }
    assert_eq!(fired, 1, "the latch let {fired} fits out for one footprint");
    assert!(planner.is_fitting());

    // A failed round retries immediately: the camera has not moved, so the
    // quiet count is already past the threshold.
    planner.abandoned();
    assert!(
        planner.observe(SITE, want, desktop()).is_some(),
        "a failed fit was not retried on the next observation",
    );

    // A landed one settles instead, and asks for nothing more.
    planner.landed(want);
    assert!(!planner.is_fitting());
    for _ in 0..200 {
        assert!(planner.observe(SITE, want, desktop()).is_none());
    }
    assert_eq!(planner.settled_footprint(), Some(want));
}

// ------------------------------------------------------------- the arithmetic

/// The zoom a spacing implies, against hand-computed figures rather than
/// against the function's own arithmetic.
///
/// On `EARTH_RADIUS_KM = 6371`, a z11 tile pixel at 39°N is
/// `2 pi 6371 cos(39) / (256 * 2048)` km = 59.24 m.
#[test]
fn the_zoom_a_spacing_implies_is_the_archives_own_pixel() {
    let pixel_km = tile_pixel_km(11, 39.0, 256);
    assert!(
        (pixel_km - 0.059_24).abs() < 1e-4,
        "a z11 pixel at 39N came out {pixel_km:.6} km",
    );
    // A spacing exactly at that pixel wants exactly that zoom.
    assert_eq!(zoom_for_spacing(pixel_km, 39.0, 256), 11);
    // A hair finer wants one deeper: `ceil`, never `round`, so no post is
    // interpolated up from a coarser pixel.
    assert_eq!(zoom_for_spacing(pixel_km * 0.99, 39.0, 256), 12);
    // Twice as coarse wants one shallower.
    assert_eq!(zoom_for_spacing(pixel_km * 2.0, 39.0, 256), 10);
    // Total at both ends.
    assert_eq!(zoom_for_spacing(f64::NAN, 39.0, 256), u8::MAX);
    assert_eq!(zoom_for_spacing(0.0, 39.0, 256), u8::MAX);
    assert_eq!(zoom_for_spacing(1e12, 39.0, 256), 0);
    assert_eq!(zoom_for_spacing(1.0, 39.0, 0), u8::MAX);
}

/// **The withdrawn clause, and the arithmetic that retired it.**
///
/// "Post spacing finer than a pixel at full zoom" does not hold from a z11
/// archive, and the answer is **not** to fund z13. Every figure here reproduces
/// independently; what they add up to is in [`HeightPlan::post_spacing_km`],
/// and the short of it is that z12's 29.67 m *is* the source DEM's own
/// resolution (Copernicus GLO-30) while z13's 14.83 m is the same ground
/// interpolated twice as finely at four times the storage.
///
/// Kept as a test rather than as a paragraph because every number in the
/// recommendation is one this file can check, and a recommendation whose
/// arithmetic silently moves is worse than none.
#[test]
fn the_withdrawn_pixel_clause_wanted_z13_and_z13_would_have_bought_nothing() {
    const PANE_POINTS: f64 = 1080.0;
    // **89 degrees is `MAX_PITCH_DEG`, the most favourable pitch there is**,
    // and the clause was only ever measured here. That is stated in the fixture
    // rather than left in the choice of number.
    let visible = camera_at(MIN_EYE_DISTANCE, 89.0, 0.0, box_920()).visible();
    let plan = HeightPlan::fit(SITE, visible, desktop()).expect("a plan");
    let (_, spacing_y_km) = plan.post_spacing_km();
    let km_per_point = visible.extent_km().1 / PANE_POINTS;
    // The pane's own share of the ground: 23.68 km over 1080 points.
    assert!(
        (visible.extent_km().1 - 23.68).abs() < 0.05,
        "the zoom stop shows {:.2} km down the pane, not 23.68",
        visible.extent_km().1,
    );
    assert!(
        (km_per_point * 1000.0 - 21.92).abs() < 0.05,
        "{:.2} m to the point, not 21.92",
        km_per_point * 1000.0,
    );
    assert!(
        spacing_y_km > km_per_point,
        "a z11 archive reached a post per point after all — {:.1} m posts \
         against {:.1} m a point. Re-derive the clause rather than deleting \
         this test",
        spacing_y_km * 1000.0,
        km_per_point * 1000.0,
    );
    assert_eq!(plan.limit, PlanLimit::Archive);

    // **z12 is the source DEM's own resolution and z13 is an oversample.**
    // Copernicus GLO-30 posts at about 30 m; z12 at this latitude is 29.67 m.
    let pixel = |z: u8| tile_pixel_km(z, SITE.0, desktop().tile_px) * 1000.0;
    assert!((pixel(11) - 59.34).abs() < 0.05, "{}", pixel(11));
    assert!((pixel(12) - 29.67).abs() < 0.05, "{}", pixel(12));
    assert!((pixel(13) - 14.83).abs() < 0.05, "{}", pixel(13));
    const GLO30_POSTING_M: f64 = 30.0;
    assert!(
        (pixel(12) - GLO30_POSTING_M).abs() < 1.0,
        "z12 is {:.2} m, which is no longer the source DEM's own posting; the \
         recommendation to fund z12 rests on those two being the same number",
        pixel(12),
    );
    assert!(
        pixel(13) < GLO30_POSTING_M / 1.9,
        "z13 is {:.2} m against a {GLO30_POSTING_M} m source, so it is not the \
         2x oversample the recommendation calls it",
        pixel(13),
    );

    // **And the clause is unreachable at an ORDINARY pitch, at any depth and
    // any budget.** At 25 degrees the footprint opens out past what
    // `MAX_POSTS_PER_AXIS` itself allows.
    let ordinary = camera_at(MIN_EYE_DISTANCE, 25.0, 225.0, box_920()).visible();
    // A point on the ground is the UN-inclined figure -- the pane's share of
    // the ground at the pivot's own depth. The footprint is the inclined one,
    // because the ground runs away from the camera. So the posts wanted are the
    // opened-out footprint over the un-inclined point.
    let posts_wanted = ordinary.extent_km().1 / km_per_point;
    // Measured: 66.40 km of ground, wanting 3028 posts.
    assert!(
        posts_wanted > 3000.0,
        "at 25 degrees the clause wants {posts_wanted:.0} posts over \
         {:.2} km of ground",
        ordinary.extent_km().1,
    );

    // **At the SHIPPED budget, z13 does not clear the clause**, which is the
    // half of the reachability argument that survives.
    let shipped_z13 = HeightPlan::fit(
        SITE,
        ordinary,
        HeightCeilings {
            max_zoom: 13,
            ..desktop()
        },
    )
    .expect("a plan");
    assert!(
        shipped_z13.post_spacing_km().1 > km_per_point,
        "z13 at the shipped 2 MiB / 64-tile ceilings reached a post per point \
         after all: {shipped_z13:?}",
    );

    // **And the half that does NOT survive, recorded because it was the
    // review's own argument and it was true of the older ladder.** The review
    // held that the clause is unreachable at *any* budget, because 3028 posts
    // is past a `PostRung::FINEST` of 2048. That ceiling was an artefact of the
    // absolute-post-count ladder F1 replaced: the rung is now a zoom step over
    // a base that starts at the adapter's own figure, so given every byte and
    // every tile, z13 *does* clear the clause here. What decides against z13 is
    // therefore the oversample above and not reachability — a weaker argument
    // than the review's, and the stronger one is the one that was never in
    // doubt.
    let unlimited = HeightCeilings {
        texture_bytes: usize::MAX / 4,
        max_tiles: usize::MAX / 4,
        max_zoom: 13,
        max_texture_dimension: 32_768,
        tile_px: 256,
    };
    let best = HeightPlan::fit(SITE, ordinary, unlimited).expect("a plan");
    assert!(
        best.post_spacing_km().1 <= km_per_point,
        "z13 with an unlimited budget no longer clears the clause at an \
         ordinary pitch. If that is deliberate, the paragraph above is the one \
         to re-derive: {best:?}",
    );
    // And the depth it *would* take, so the report has a number.
    // The exact depth, not a floor: `>= 13` would have passed silently if the
    // answer were z14, and the number is quoted in the report.
    let wanted = zoom_for_spacing(km_per_point, SITE.0, desktop().tile_px);
    assert_eq!(
        wanted, 13,
        "a post per POINT at the zoom stop wants z{wanted}",
    );
    // **And the clause says "pixel", not "point".** On a 2x display a pixel is
    // half a point, so the requirement is 10.96 m and the depth is one deeper
    // again. Both figures are recorded because the clause is being amended and
    // whoever reads it next needs to know which one it was ever measured
    // against.
    assert_eq!(
        zoom_for_spacing(km_per_point / 2.0, SITE.0, desktop().tile_px),
        14,
    );
}

/// The placement affine is the identity exactly when the field is the box, and
/// the inverse of it recovers the footprint otherwise.
#[test]
fn the_placement_affine_round_trips_the_footprint() {
    let drawn = box_920();
    assert_eq!(drawn.ground_box_in(&drawn), Some([1.0, 1.0, 0.0, 0.0]));
    let inner = Footprint {
        x_km: (-230.0, 230.0),
        y_km: (0.0, 92.0),
    };
    let placed = inner.ground_box_in(&drawn).expect("a placement");
    assert_eq!(placed, [0.5, 0.1, 0.25, 0.5]);
    // Read back through the affine the shader applies.
    let (bx, by) = drawn.extent_km();
    let back = Footprint {
        x_km: (
            drawn.x_km.0 + f64::from(placed[2]) * bx,
            drawn.x_km.0 + f64::from(placed[2] + placed[0]) * bx,
        ),
        y_km: (
            drawn.y_km.0 + f64::from(placed[3]) * by,
            drawn.y_km.0 + f64::from(placed[3] + placed[1]) * by,
        ),
    };
    // Through `f32`, because that is what the uniform lane is: a metre of
    // slack on a 920 km box is the format's own resolution, not a drift.
    let close = |a: f64, b: f64| (a - b).abs() < 1e-3;
    assert!(
        close(back.x_km.0, inner.x_km.0)
            && close(back.x_km.1, inner.x_km.1)
            && close(back.y_km.0, inner.y_km.0)
            && close(back.y_km.1, inner.y_km.1),
        "{back:?} did not come back as {inner:?}",
    );
    // A box with no extent has no placement rather than an infinity.
    assert_eq!(
        inner.ground_box_in(&Footprint {
            x_km: (0.0, 0.0),
            y_km: (0.0, 1.0)
        }),
        None,
    );
}

/// The request a plan builds names the plan's own footprint, posts and cover —
/// so a caller cannot re-derive the box and hand the resampler a different one
/// than the tiles were fetched for.
#[test]
fn the_request_carries_the_plans_own_box() {
    let plan = HeightPlan::fit(SITE, box_920(), desktop()).expect("a plan");
    let request = plan.request(Vec::new());
    assert_eq!(request.site, plan.site);
    assert_eq!(request.x_km, plan.footprint.x_km);
    assert_eq!(request.y_km, plan.footprint.y_km);
    assert_eq!(request.posts, plan.posts);
    assert_eq!(request.cover, plan.cover);
    // And the cover it carries really is the one the posts need: the job's own
    // run recomputes it and refuses a plane that does not contain it.
    let recomputed = cover_for(
        plan.site,
        plan.footprint.x_km,
        plan.footprint.y_km,
        plan.posts,
        plan.cover.zoom,
        plan.cover.tile_px,
    )
    .expect("a cover");
    assert!(
        plan.cover.covers(&recomputed),
        "the plan's cover {:?} does not contain the one its own posts need {recomputed:?}",
        plan.cover,
    );
}

/// The rung counts zoom levels, and its divisor is the one fact that implies.
#[test]
fn a_rung_is_a_zoom_level_and_its_divisor_says_so() {
    // The finest rung is no step at all, so the base a fit solves for is what
    // the plan answers whenever the tile count does not bind.
    assert_eq!(PostRung::FINEST.linear_divisor(), 1);
    assert_eq!(PostRung::FINEST.zooms_below(), 0);
    // A zoom level is twice the ground per tile pixel, so the k-th rung is 2^k.
    let mut rung = PostRung::FINEST;
    for k in 0..24u8 {
        assert_eq!(rung.zooms_below(), k, "{rung:?}");
        assert_eq!(rung.linear_divisor(), 1u32 << k, "{rung:?}");
        rung = rung.next_coarser().expect("the counter has not run out");
    }
    // Total at the far end rather than a shift that panics in debug and wraps
    // in release. No archive is 32 levels deep; the answer is still an answer.
    assert_eq!(PostRung::from_zooms_below(32).linear_divisor(), u32::MAX);
    assert_eq!(PostRung::from_zooms_below(200).linear_divisor(), u32::MAX);
    assert_eq!(PostRung::from_zooms_below(u8::MAX).next_coarser(), None);
}

/// **F1 regression: the fit is monotone in the archive's depth.**
///
/// A deeper archive can never buy a *coarser* field. It used to: the archive
/// clamp fired only when the finest rung's own implied zoom passed `max_zoom`,
/// so once the ladder stepped down for the byte budget the coarser rung's zoom
/// fell under the ceiling, the clamp stopped running, and the plan took the
/// rung's raw power-of-two posts instead of the finest the budget could
/// actually afford. At the shipped ceilings on the zoom-stop footprint,
/// `max_zoom = 13` answered a field **25% coarser than `max_zoom = 12`, with
/// 38% of the texture budget unspent**.
///
/// Asserted as monotonicity over a sweep rather than as the two figures that
/// exposed it, because the two figures are one instance of a shape.
#[test]
fn a_deeper_archive_never_buys_a_coarser_field() {
    for drawn in [box_920(), box_66_to_1()] {
        for standoff in [8.0, default_standoff(), 1.0, 0.3, MIN_EYE_DISTANCE] {
            for pitch in [89.0, 45.0, 25.0, 8.0] {
                let visible = camera_at(standoff, pitch, 225.0, drawn).visible();
                let mut previous: Option<(u8, [u32; 2], f64)> = None;
                for max_zoom in 8..=18u8 {
                    let plan = HeightPlan::fit(
                        SITE,
                        visible,
                        HeightCeilings {
                            max_zoom,
                            ..desktop()
                        },
                    )
                    .expect("a plan");
                    let (sx, _) = plan.post_spacing_km();
                    if let Some((was_zoom, was_posts, was_spacing)) = previous {
                        assert!(
                            plan.posts[0] >= was_posts[0] && plan.posts[1] >= was_posts[1],
                            "z{max_zoom} answered {:?} where z{was_zoom} answered \
                             {was_posts:?} over the same footprint. A deeper archive \
                             bought a coarser field",
                            plan.posts,
                        );
                        assert!(
                            sx <= was_spacing * 1.000_001,
                            "z{max_zoom} spaced its posts {sx:.5} km apart where \
                             z{was_zoom} managed {was_spacing:.5} km",
                        );
                    }
                    previous = Some((max_zoom, plan.posts, sx));
                }
            }
        }
    }
}

/// The two figures F1 was found on, kept as the concrete case behind the sweep
/// above — and as the thing that would have to be re-derived if the shipped
/// ceilings move.
///
/// **The fit spends its budget.** Both depths come out inside 2 MiB and neither
/// leaves a third of it unspent, which is the half of the defect that the
/// monotonicity sweep alone would not have caught: a plan can be monotone and
/// still be wasting the budget at every rung.
#[test]
fn the_shipped_ceilings_spend_their_texture_budget_at_every_archive_depth() {
    let visible = camera_at(MIN_EYE_DISTANCE, 25.0, 225.0, box_920()).visible();
    for max_zoom in [11u8, 12, 13, 14] {
        let plan = HeightPlan::fit(
            SITE,
            visible,
            HeightCeilings {
                max_zoom,
                ..desktop()
            },
        )
        .expect("a plan");
        assert!(
            plan.texture_bytes() <= desktop().texture_bytes,
            "z{max_zoom} overran the budget: {plan:?}",
        );
        // Under budget is fine; leaving a third of it on the table while the
        // archive still has detail to give is not. The exception is a plan the
        // ARCHIVE bound -- there is nothing left to buy with the rest.
        if plan.limit != PlanLimit::Archive && plan.limit != PlanLimit::TileCount {
            assert!(
                plan.texture_bytes() * 4 >= desktop().texture_bytes * 3,
                "z{max_zoom} used {} of {} budget bytes and was limited by \
                 {:?} rather than by the archive: {plan:?}",
                plan.texture_bytes(),
                desktop().texture_bytes,
                plan.limit,
            );
        }
    }
}

/// **The ladder steps the tile count's own quantum, and every rung below the
/// first is independent of the base.**
///
/// The two facts F1's fix rests on, asserted directly rather than inferred from
/// the monotonicity sweep passing.
///
/// The tile count only changes where the *zoom* changes, so a ladder that
/// halved posts could land between two zooms and pay a quadrupling for one
/// extra post — which is how the second non-monotone spelling arose. Stepping
/// zooms cannot: one rung down is exactly one zoom down, one tile-count
/// quantum.
///
/// And because every rung below the first re-solves the posts from that zoom's
/// own pixel rather than dividing the base, two different bases that end up on
/// the same rung answer the same field. That is what turns "monotone" from an
/// observation over a sweep into a property.
#[test]
fn the_ladder_steps_the_tile_counts_own_quantum() {
    // One rung is one zoom, and one zoom is twice the ground per tile pixel.
    let finest = tile_pixel_km(20, SITE.0, desktop().tile_px);
    for k in 0..10u8 {
        let rung = PostRung::from_zooms_below(k);
        let pixel = tile_pixel_km(20 - k, SITE.0, desktop().tile_px);
        assert!(
            (pixel / finest - f64::from(rung.linear_divisor())).abs() < 1e-9,
            "{rung:?} is not {}x the ground per pixel",
            rung.linear_divisor(),
        );
    }

    // **The ladder reaches a zoom that meets the ceiling, however deep it
    // started.** Six fixed rungs did not: from z11 they stopped at z6, and a
    // four-tile ceiling wanted z5, so the fit returned a plan that violated the
    // ceiling it was searching for. Asserted over the whole archive range.
    for max_zoom in 8..=20u8 {
        let plan = HeightPlan::fit(
            SITE,
            box_920(),
            HeightCeilings {
                max_tiles: 4,
                max_zoom,
                ..desktop()
            },
        )
        .expect("a plan");
        assert!(
            plan.cover.len() <= 4,
            "starting at z{max_zoom} the ladder ran out before meeting a \
             four-tile ceiling: {plan:?}",
        );
    }

    // **Every rung below the first is base-independent.** Two runs whose only
    // difference is a ceiling that changes the base must agree once the tile
    // count has pushed them off the top rung.
    let footprint = box_920();
    let tight = HeightCeilings {
        max_tiles: 4,
        max_zoom: 12,
        ..desktop()
    };
    let looser_base = HeightCeilings {
        max_zoom: 16,
        texture_bytes: 256 * 1024 * 1024,
        ..tight
    };
    let a = HeightPlan::fit(SITE, footprint, tight).expect("a plan");
    let b = HeightPlan::fit(SITE, footprint, looser_base).expect("a plan");
    assert_ne!(
        a.rung,
        PostRung::FINEST,
        "the tile ceiling did not push this off the top rung, so the fixture \
         cannot say anything about the rungs below it: {a:?}",
    );
    assert_eq!(
        a.posts, b.posts,
        "two runs that differ only in ceilings which change the BASE answered \
         different fields once the tile count had pushed them below the top \
         rung. Those rungs are supposed to be solved from the zoom alone: \
         {a:?} against {b:?}",
    );
    assert_eq!(a.cover.zoom, b.cover.zoom);

    // Non-triviality: those two ceiling sets really do give different bases, so
    // the agreement above is the ladder's doing and not the ceilings being the
    // same in disguise.
    let open = HeightCeilings {
        max_tiles: usize::MAX / 4,
        ..tight
    };
    let open_looser = HeightCeilings {
        max_tiles: usize::MAX / 4,
        ..looser_base
    };
    assert_ne!(
        HeightPlan::fit(SITE, footprint, open)
            .expect("a plan")
            .posts,
        HeightPlan::fit(SITE, footprint, open_looser)
            .expect("a plan")
            .posts,
        "the two ceiling sets give the same base even with the tile ceiling \
         lifted, so the equality above proves nothing",
    );
}

/// **What the fit actually answers over a 66:1 box, pinned, because two GPU
/// suites quote it and cannot call it.**
///
/// F6. The aspect fixture in `squallar-gpu/tests/volume_ground_aspect.rs` used
/// `[1024, 15]` and the commit message attributed those posts to the planner.
/// They were not the planner's: `round(1024 / 66.45)` took 1024 for the rung
/// when the finest ask is the adapter's own ceiling, and across 350 cameras
/// over that box the fit produced 126 distinct shapes and never that one. The
/// behavioural claim survived — an extreme aspect drew, and the mutant was
/// noticed — but at a quarter of production's mesh, and nothing pinned it.
///
/// `squallar-gpu` cannot declare `squallar-elevation` (the resample runs inside
/// the offload worker, which links neither egui nor wgpu), so the numbers are
/// carried there as constants and anchored here. **If this test moves, that
/// fixture is stale.**
#[test]
fn the_sixty_six_to_one_box_is_fitted_to_these_two_shapes() {
    // A browser on the WebGL2 downlevel guarantee: the adapter's own ceiling is
    // what binds, so the long axis is exactly 2048.
    let browser = HeightPlan::fit(SITE, box_66_to_1(), webgl2_downlevel()).expect("a plan");
    assert_eq!(browser.posts, [2048, 31], "{browser:?}");
    assert_eq!(browser.limit, PlanLimit::TextureDimension);

    // A desktop adapter: the tile ceiling is what binds, one zoom level down.
    let desktop_plan = HeightPlan::fit(SITE, box_66_to_1(), desktop()).expect("a plan");
    assert_eq!(desktop_plan.posts, [5599, 84], "{desktop_plan:?}");
    assert_eq!(desktop_plan.limit, PlanLimit::TileCount);
    assert_eq!(desktop_plan.rung.zooms_below(), 1);

    // Both are shapes `upload_heights` accepts and the draw can issue, and both
    // are far from square — which is the property the GPU fixture is for.
    for plan in [&browser, &desktop_plan] {
        assert!(plan.posts[0] >= 2 && plan.posts[1] >= 2, "{plan:?}");
        assert!(
            plan.posts[0] > plan.posts[1] * 60,
            "{plan:?} is not the extreme aspect the fixture exists to draw",
        );
        let vertices = 6u64 * (u64::from(plan.posts[0]) + 1) * (u64::from(plan.posts[1]) + 1);
        assert!(
            u32::try_from(vertices).is_ok(),
            "{plan:?} is not a u32 draw"
        );
    }
    // And the desktop answer really is the bigger mesh, so rendering only the
    // browser one would be testing the smaller of the two.
    assert!(desktop_plan.posts[0] > browser.posts[0] * 2);
}
