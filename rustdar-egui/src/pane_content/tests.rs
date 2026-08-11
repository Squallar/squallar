use super::*;

fn point(lat: f64, lon: f64) -> GeoPoint {
    GeoPoint { lat, lon }
}

/// The kind is derived from the content, so the two cannot disagree — which
/// is the entire reason it is a method.
#[test]
fn every_content_variant_reports_its_own_kind() {
    assert_eq!(PaneContent::Map.kind(), PaneKind::Map);
    for kind in [PaneKind::Map, PaneKind::CrossSection, PaneKind::Volume] {
        assert_eq!(PaneContent::for_kind(kind).kind(), kind);
    }
}

/// `Default` is `Map` — a choice, not something the types force: both other
/// variants derive `Default` too, so only `derive(Default)`'s `#[default]`
/// attribute picks this one, and a hand-written impl yielding a section pane
/// would compile.
///
/// Pinned because of what the value is *for*. Six `mem::take` sites leave it
/// in `Gui::panes[idx]` for the rest of the UI pass, and the all-panes
/// filters that key off `PaneState::is_map` read that slot — so a default
/// section pane would make every one of them silently skip whichever pane is
/// being drawn, with no error to say why.
#[test]
fn the_default_content_is_a_map() {
    assert_eq!(PaneContent::default().kind(), PaneKind::Map);
    assert_eq!(PaneKind::default(), PaneKind::Map);
}

/// The sourceless default box covers the whole scan.
///
/// A 3D pane with no picked region is showing "the site's volume", and the
/// plan view beside it draws echo out to `MAX_RANGE_KM` — so a default
/// half-width under that range crops the scan: echo past the box's edge
/// simply vanishes from the 3D picture, which reads as a resample gone
/// wrong rather than as a choice. Two facts keep the default honest, and
/// both are pinned: it reaches the raster's edge, and the resampler passes
/// it through un-clamped, so the box the caption and the camera arithmetic
/// describe is the box that is actually built.
#[test]
#[allow(clippy::assertions_on_constants)] // the covering bound IS a constant pin
fn the_sourceless_default_box_covers_the_whole_scan() {
    assert!(
        DEFAULT_HALF_WIDTH_KM >= rustdar_radar::types::MAX_RANGE_KM,
        "the default box must reach the scan's edge: {DEFAULT_HALF_WIDTH_KM} km \
             of half-width against a {} km surveillance range",
        rustdar_radar::types::MAX_RANGE_KM,
    );
    let region = VolumeRegion::new(point(35.3, -97.3), DEFAULT_HALF_WIDTH_KM)
        .expect("the default half-width must be a region the resampler takes");
    assert_eq!(
        region.half_width_km(),
        DEFAULT_HALF_WIDTH_KM,
        "the resampler must honour the default un-clamped, or the pane's own \
             camera arithmetic describes a different box than the one built",
    );
}

/// A plan view reads one sweep; the other two read the whole ladder, and
/// giving either of them a volume with cuts deliberately skipped fabricates
/// layers rather than failing.
#[test]
fn only_a_map_pane_is_content_with_one_tilt() {
    assert!(!PaneKind::Map.consumes_whole_volume());
    assert!(PaneKind::CrossSection.consumes_whole_volume());
    assert!(PaneKind::Volume.consumes_whole_volume());
    // And the predicate is the view's, not a second copy of it: every kind
    // agrees with the view it maps to, so the two names cannot come to
    // give different answers.
    for kind in [PaneKind::Map, PaneKind::CrossSection, PaneKind::Volume] {
        assert_eq!(
            kind.consumes_whole_volume(),
            kind.render_view().reads_whole_volume(),
            "{kind:?} answers the whole-volume question twice, differently",
        );
    }
}

/// A line that cannot be cut is not representable. Every refusal matters:
/// a NaN endpoint would make [`SectionTarget`] never equal itself and
/// re-render the pane on every frame forever, a finite-but-absurd one would
/// render as empty coverage that looks like an out-of-range line, and a
/// zero-length line has no bearing to walk along.
#[test]
fn a_section_line_refuses_endpoints_it_cannot_be_cut_along() {
    assert!(SectionLine::new(point(35.3, -97.3), point(35.6, -97.0)).is_some());

    // Non-finite, and finite-but-nowhere. The second group is the one a
    // bare `is_finite` guard let through.
    for bad_lat in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, 1e9, 90.001] {
        assert!(
            SectionLine::new(point(bad_lat, -97.3), point(35.6, -97.0)).is_none(),
            "{bad_lat} latitude accepted"
        );
    }
    for bad_lon in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -1e9, 180.001] {
        assert!(
            SectionLine::new(point(35.3, -97.3), point(35.6, bad_lon)).is_none(),
            "{bad_lon} longitude accepted"
        );
    }

    // The bounds are inclusive: a pole and the antimeridian are places.
    assert!(SectionLine::new(point(90.0, 180.0), point(-90.0, -180.0)).is_some());

    assert!(
        SectionLine::new(point(35.3, -97.3), point(35.3, -97.3)).is_none(),
        "a zero-length line has no bearing: every column would sample one point"
    );
}

/// `release_textures` is total over the kinds, and callable on each.
///
/// Every arm is empty today; the point is that the call site in
/// `Gui::clear_graphics_state` is already wired, so the field that needs
/// releasing lands inside a function that is already called on every
/// suspend and every surface loss. A `match` with no wildcard is what makes
/// a fourth kind stop the build rather than leak quietly.
#[test]
fn releasing_textures_is_total_over_the_kinds() {
    for kind in [PaneKind::Map, PaneKind::CrossSection, PaneKind::Volume] {
        let mut content = PaneContent::for_kind(kind);
        content.release_textures();
        assert_eq!(
            content.kind(),
            kind,
            "releasing a pane's textures must not change what kind it is"
        );
    }
}

/// A section pane really gives up its texture — and really keeps everything
/// else, **including its staleness key**.
///
/// Three decisions, and the third is the one with a mutant of its own. The
/// handle has to go, because a texture outliving its context is a leak
/// nothing reports: not a panic, not a blank pane, just memory that never
/// comes back across a suspend/resume cycle. The `CrossSection` behind it
/// has to *stay*, because it is plain memory rather than a GPU handle, it is
/// what a hover reads, and re-cutting it on resume needs the volume — which
/// may well have been evicted by then.
///
/// And `rendered_for` has to stay too, which is the half that looks
/// optional. Clearing it here is the one-line way to make the pane recover:
/// the dispatcher would see no key, ask again, and the picture would come
/// back. It is also a 15.6 MB volume walk plus an 8–13 ms raster on the
/// resume frame, for a picture already in memory, and it fails outright when
/// the volume is gone. Keeping the key is what makes the recovery a
/// re-upload — `App::restore_section_textures` — instead of a re-cut, so
/// this asserts the key survives rather than merely that the pane recovers.
/// Both would pass the second claim; only this one fails the mutant.
///
/// The empty-arm version of this test passed for a build that released
/// nothing at all; found by mutation.
#[test]
fn a_section_pane_drops_its_texture_and_keeps_its_cut() {
    let ctx = egui::Context::default();
    let texture = ctx.load_texture(
        "section-fixture",
        egui::ColorImage::filled([1, 1], egui::Color32::WHITE),
        egui::TextureOptions::NEAREST,
    );
    let line = SectionLine::new(point(35.3, -97.3), point(35.6, -97.0)).expect("valid line");
    // The cut itself is in the fixture, and it is the half the test is
    // *named* for. Without it, `section` is `None` before and after and the
    // assertion below holds for a `release_textures` that drops it — which
    // is the exact mutant that survived: the resume path would re-cut from a
    // volume that may have been evicted instead of re-uploading a raster it
    // still had.
    // Likewise the key: with `rendered_for: None` in the fixture it is
    // `None` before and after, and an arm that cleared it would pass.
    let target = SectionTarget {
        volume: VolumeStamp {
            site: "KTLX".to_owned(),
            collected: chrono::NaiveDate::from_ymd_opt(2026, 7, 30)
                .expect("a real date")
                .and_hms_opt(18, 42, 0)
                .expect("a real time"),
        },
        product: RadarProduct::Reflectivity,
        line,
        ladder: 11,
    };
    let mut content = PaneContent::CrossSection(Box::new(CrossSectionPane {
        line: Some(line),
        source_pane: Some(0),
        section: Some(std::sync::Arc::new(blank_section())),
        texture: Some(texture),
        unavailable: Some(SectionUnavailable::RenderFailed),
        rendered_for: Some(target.clone()),
        detail_open: false,
    }));

    content.release_textures();

    let PaneContent::CrossSection(section) = &content else {
        panic!("releasing a texture changed the pane's kind");
    };
    assert!(
        section.texture.is_none(),
        "the handle outlived the context that owns it"
    );
    assert!(
        section.section.is_some(),
        "the cut went with the texture, so a resume has to re-cut from a \
             volume that may no longer be in memory"
    );
    assert_eq!(
        section.rendered_for,
        Some(target),
        "the staleness key went with the texture, so the resume re-cuts a \
             15.6 MB volume instead of re-uploading the raster it still holds — \
             and fails outright if that volume has been evicted"
    );
    assert_eq!(
        section.line,
        Some(line),
        "the line went with the texture, so the pane forgot what it was aimed at"
    );
    assert_eq!(section.source_pane, Some(0));
    assert_eq!(section.unavailable, Some(SectionUnavailable::RenderFailed));
}

/// A cut of the right shape and no content, so a fixture can hold a picture
/// for a release or a retarget to act on.
///
/// Full size — `from_parts` refuses anything else, because a mis-shaped
/// section reaches `ColorImage::from_rgba_unmultiplied`'s `assert_eq!` on
/// the main thread.
fn blank_section() -> CrossSection {
    use rustdar_radar::sampler::SampleStatus;
    use rustdar_radar::xsect::{SECTION_HEIGHT, SECTION_WIDTH, SectionAxes};
    let pixels = SECTION_WIDTH * SECTION_HEIGHT;
    CrossSection::from_parts(
        vec![0u8; pixels * 4],
        vec![f32::NAN; pixels],
        vec![SampleStatus::NoCoverage.wire_code(); pixels],
        SectionAxes {
            length_km: 100.0,
            base_km_msl: 0.4,
            top_km_msl: 20.4,
            near_ground_range_km: 10.0,
            far_ground_range_km: 110.0,
            coverage_ground_range_km: 0.0,
            cone_of_silence_km: 0.0,
            tilt_count: 1,
            widest_tilt_gap_deg: 0.0,
            top_tilt_deg: 0.5,
            top_declared_cut_deg: 19.5,
        },
        vec![0.5],
        vec![0],
    )
    .expect("a full-size, all-NoCoverage section is well formed")
}

/// The staleness key notices a new volume with no help from any reset path,
/// because the volume time is in the key.
#[test]
fn a_section_target_goes_stale_when_the_volume_does() {
    let line = SectionLine::new(point(35.3, -97.3), point(35.6, -97.0)).expect("valid line");
    let at = |minute: u32| {
        chrono::NaiveDate::from_ymd_opt(2026, 7, 29)
            .unwrap()
            .and_hms_opt(18, minute, 0)
            .unwrap()
    };
    let target = |site: &str, minute: u32, ladder: u64| SectionTarget {
        volume: VolumeStamp {
            site: site.to_owned(),
            collected: at(minute),
        },
        product: RadarProduct::Reflectivity,
        line,
        ladder,
    };

    assert_eq!(target("KTLX", 30, 9), target("KTLX", 30, 9));
    assert_ne!(
        target("KTLX", 30, 9),
        target("KTLX", 36, 9),
        "a new volume for the site makes the section on screen stale"
    );
    assert_ne!(
        target("KTLX", 30, 9),
        target("KOUN", 30, 9),
        "the same volume time at another site is a different picture"
    );
    // The live-feed arm, and the reason `ladder` is in the key at all.
    // The volume time here is *identical* — it is the first sweep's, and
    // on the feed that is frozen for five to six minutes while the merged
    // ladder refreshes underneath it. Without the ladder fingerprint
    // these two keys are equal and the section cut from the first chunk
    // stands for the whole volume.
    assert_ne!(
        target("KTLX", 30, 1),
        target("KTLX", 30, 9),
        "the same volume under a changed ladder is a different section"
    );
}

/// The camera's one writer refuses a non-finite delta rather than clamping
/// it. Clamping would carry the NaN through (`f32::clamp` propagates it),
/// and a NaN camera makes the re-render comparison fire on every frame for
/// the life of the pane, silently.
#[test]
fn a_non_finite_nudge_leaves_the_camera_exactly_where_it_was() {
    // The premise, stated so nobody "simplifies" the guard back into a clamp.
    assert!(f32::NAN.clamp(-89.0, 89.0).is_nan());

    let start = OrbitCamera::default();
    for bad in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
        for delta in [
            OrbitDelta {
                yaw_deg: bad,
                ..Default::default()
            },
            OrbitDelta {
                pitch_deg: bad,
                ..Default::default()
            },
            OrbitDelta {
                zoom_factor: bad,
                ..Default::default()
            },
        ] {
            let mut camera = start;
            camera.nudge(delta);
            assert_eq!(camera, start, "{delta:?} moved the camera");
            assert!(camera.yaw_deg().is_finite());
            assert!(camera.pitch_deg().is_finite());
            assert!(camera.eye_distance().is_finite());
        }
    }

    // A zero or negative zoom factor is refused for the same reason: it is
    // a ratio, and a degenerate gesture span produces one.
    for factor in [0.0, -1.0] {
        let mut camera = start;
        camera.nudge(OrbitDelta {
            zoom_factor: factor,
            ..Default::default()
        });
        assert_eq!(camera, start, "zoom factor {factor} moved the camera");
    }
}

/// A persisted camera comes back exactly, and a corrupt one comes back as
/// nothing.
///
/// `restore` is the only way a camera can be built from numbers off disk, so
/// it is where a hand-edited or version-skewed config is stopped. The refusal
/// half matters for the same reason [`OrbitCamera::nudge`]'s does: a NaN
/// camera makes the re-render comparison fire every frame for ever, silently.
#[test]
fn a_restored_camera_is_the_one_that_was_saved_or_none_at_all() {
    let start = OrbitCamera::default();
    let round_tripped = OrbitCamera::restore(
        start.yaw_deg(),
        start.pitch_deg(),
        start.eye_distance(),
        start.pivot(),
        start.vertical_exaggeration(),
    )
    .expect("a camera's own values must restore");
    assert_eq!(round_tripped, start);

    // A camera that had been moved, so the round trip is not just the default
    // agreeing with itself.
    let mut moved = start;
    moved.nudge(OrbitDelta {
        yaw_deg: -47.5,
        pitch_deg: 12.25,
        zoom_factor: 1.5,
        // Panned as well, so the round trip covers the pivot rather than
        // agreeing with itself about a zero.
        pan: [0.2, -0.35, 0.1],
    });
    moved.set_vertical_exaggeration(5.5);
    assert_ne!(moved, start, "precondition: the nudge must have moved it");
    assert_eq!(
        OrbitCamera::restore(
            moved.yaw_deg(),
            moved.pitch_deg(),
            moved.eye_distance(),
            moved.pivot(),
            moved.vertical_exaggeration(),
        ),
        Some(moved)
    );

    let ok_pivot = [0.0; 3];
    for bad in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
        assert_eq!(
            OrbitCamera::restore(bad, 25.0, 2.5, ok_pivot, 3.0),
            None,
            "yaw {bad}"
        );
        assert_eq!(
            OrbitCamera::restore(225.0, bad, 2.5, ok_pivot, 3.0),
            None,
            "pitch {bad}"
        );
        assert_eq!(
            OrbitCamera::restore(225.0, 25.0, bad, ok_pivot, 3.0),
            None,
            "distance {bad}"
        );
        assert_eq!(
            OrbitCamera::restore(225.0, 25.0, 2.5, ok_pivot, bad),
            None,
            "exaggeration {bad}"
        );
        for axis in 0..3 {
            let mut pivot = ok_pivot;
            pivot[axis] = bad;
            assert_eq!(
                OrbitCamera::restore(225.0, 25.0, 2.5, pivot, 3.0),
                None,
                "pivot axis {axis} = {bad}"
            );
        }
    }

    // Finite but out of range: wrapped and clamped rather than refused, and
    // through the same expressions `nudge` uses — so a restored camera cannot
    // hold a value `nudge` would never produce.
    let stretched = OrbitCamera::restore(-30.0, 1_000.0, 0.001, [9.0, -9.0, 9.0], 1_000.0)
        .expect("finite, so restorable");
    assert_eq!(stretched.yaw_deg(), 330.0);
    assert_eq!(stretched.pitch_deg(), MAX_PITCH_DEG);
    assert_eq!(stretched.eye_distance(), MIN_EYE_DISTANCE);
    assert_eq!(
        stretched.pivot(),
        [MAX_PIVOT_FRACTION, -MAX_PIVOT_FRACTION, MAX_PIVOT_FRACTION],
        "an out-of-range pivot must be clamped onto the box, not refused",
    );
    assert_eq!(stretched.vertical_exaggeration(), MAX_VERTICAL_EXAGGERATION);
}

/// A finite nudge does move it, and lands inside the limits — so the test
/// above is about the refusal rather than about a camera that never moves.
#[test]
fn a_finite_nudge_moves_the_camera_and_stays_in_range() {
    let mut camera = OrbitCamera::default();
    camera.nudge(OrbitDelta {
        yaw_deg: 30.0,
        pitch_deg: 10.0,
        zoom_factor: 2.0,
        ..Default::default()
    });
    assert_eq!(camera.yaw_deg(), 255.0);
    assert_eq!(camera.pitch_deg(), 35.0);
    assert!(camera.eye_distance() < OrbitCamera::default().eye_distance());

    // Yaw wraps rather than clamping — a camera that stuck at 360 could not
    // be spun all the way round.
    camera.nudge(OrbitDelta {
        yaw_deg: 200.0,
        ..Default::default()
    });
    assert_eq!(camera.yaw_deg(), 95.0);

    // Pitch and distance clamp, and stop just short of vertical: at exactly
    // ±90 the camera basis is degenerate and the image rolls arbitrarily.
    camera.nudge(OrbitDelta {
        pitch_deg: 1_000.0,
        zoom_factor: 0.000_01,
        ..Default::default()
    });
    assert_eq!(camera.pitch_deg(), MAX_PITCH_DEG);
    assert_eq!(camera.eye_distance(), MAX_EYE_DISTANCE);

    camera.nudge(OrbitDelta {
        pitch_deg: -10_000.0,
        zoom_factor: 100_000.0,
        ..Default::default()
    });
    assert_eq!(camera.pitch_deg(), -MAX_PITCH_DEG);
    assert_eq!(camera.eye_distance(), MIN_EYE_DISTANCE);
}

/// The zoom's near stop is 0.05 half-diagonals — inside the box, by the
/// literal.
///
/// Pinned as a number rather than as `MIN_EYE_DISTANCE`, because the
/// property is the *value*: at 1.05 (the old floor) the eye can never enter
/// the box and "zoom in as far as I want" stops a whole box away from the
/// storm; at 0.05 the eye ends up inside it, which the raymarch supports by
/// clamping its slab entry to zero. A symbolic assertion would follow the
/// constant wherever it went and could not see this regress. Checked at 1x
/// and 12x exaggeration: the limit is in half-diagonals of the *stretched*
/// box, so the stop is the same fraction of the view whatever the stretch.
#[test]
fn the_zoom_stops_at_a_twentieth_of_a_half_diagonal() {
    for exaggeration in [1.0, 12.0] {
        let mut camera =
            OrbitCamera::restore(225.0, 25.0, 2.5, [0.0; 3], exaggeration).expect("finite");
        camera.nudge(OrbitDelta {
            zoom_factor: 1e6,
            ..Default::default()
        });
        assert_eq!(
            camera.eye_distance(),
            0.05,
            "at {exaggeration}x the zoom's near stop moved; 0.05 half-diagonals \
                 is the inside-the-box zoom #6 asked for, and anything at or above \
                 1.0 locks the eye out of the box again",
        );
    }
    // And the same literal from the restore path, so a persisted camera
    // cannot come back with a closer zoom than the wheel can reach.
    let restored = OrbitCamera::restore(225.0, 25.0, 0.001, [0.0; 3], 3.0).expect("finite");
    assert_eq!(restored.eye_distance(), 0.05);
}

/// The exaggeration knob clamps a finite value and refuses a non-finite one.
///
/// The asymmetry is the same one `nudge` draws. A slider that reaches the end
/// of its travel should stop, so an out-of-range number is wound back. A NaN
/// has no nearest legal value, and `f32::clamp` **propagates** it — so a
/// clamp on the way in would launder it into a camera that looks checked, and
/// it would arrive at `box_from_world` as a divide-by-NaN. The GPU accepts
/// that matrix, renders an empty pane, and reports nothing anywhere.
#[test]
fn the_exaggeration_knob_clamps_the_finite_and_refuses_the_rest() {
    let mut camera = OrbitCamera::default();
    camera.set_vertical_exaggeration(100.0);
    assert_eq!(camera.vertical_exaggeration(), MAX_VERTICAL_EXAGGERATION);
    camera.set_vertical_exaggeration(0.0);
    assert_eq!(
        camera.vertical_exaggeration(),
        MIN_VERTICAL_EXAGGERATION,
        "zero would collapse the box to a plane and divide by zero",
    );
    camera.set_vertical_exaggeration(5.5);
    assert_eq!(camera.vertical_exaggeration(), 5.5);

    for bad in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
        camera.set_vertical_exaggeration(bad);
        assert_eq!(
            camera.vertical_exaggeration(),
            5.5,
            "{bad} must leave the knob exactly where it was, not clamp it",
        );
    }
}
