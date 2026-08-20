use super::*;
use rustdar_radar::fields as radar_fields;

fn point(lat: f64, lon: f64) -> GeoPoint {
    GeoPoint { lat, lon }
}

/// The kind is derived from the content, so the two cannot disagree — which
/// is the entire reason it is a method.
#[test]
fn every_content_variant_reports_its_own_kind() {
    for kind in [PaneKind::Map, PaneKind::CrossSection] {
        assert_eq!(PaneContent::for_kind(kind).kind(), kind);
    }
}

/// A map pane's *view* depends on its render mode; its *kind* does not.
#[test]
fn a_map_panes_view_follows_its_render_mode_and_its_kind_does_not() {
    for (render, view) in [
        (MapRender::Plan, RenderView::PlanView),
        (MapRender::Volume, RenderView::Volume),
    ] {
        let content = PaneContent::Map(Box::new(MapPane {
            render,
            volume: VolumePane::default(),
        }));
        assert_eq!(
            content.kind(),
            PaneKind::Map,
            "{render:?} is still a map pane: a 3D view is how a pane draws, not what it is",
        );
        assert_eq!(
            content.render_view(),
            view,
            "{render:?} must dispatch a {view:?} render",
        );
    }
    assert_eq!(
        PaneContent::for_kind(PaneKind::CrossSection).render_view(),
        RenderView::CrossSection,
        "a section's view does not depend on any mode",
    );
}

/// Switching a map pane's render mode keeps the other mode's state.
#[test]
fn leaving_the_volume_mode_and_returning_keeps_the_camera() {
    let mut camera = OrbitCamera::default();
    camera.nudge(OrbitDelta {
        yaw_deg: -42.0,
        pitch_deg: 11.0,
        zoom_factor: 1.7,
        pan: [0.1, -0.2, 0.05],
    });
    let mut map = MapPane {
        render: MapRender::Volume,
        volume: VolumePane {
            camera,
            hide_floor: true,
            view_mode: VolumeViewMode::Isosurface,
            ..Default::default()
        },
    };
    let aimed = map.volume.clone();

    map.render = MapRender::Plan;
    map.render = MapRender::Volume;

    assert_eq!(
        map.volume, aimed,
        "a round trip through the plan view lost the 3D state, so the two modes are          two panes after all",
    );
}

/// `Default` is `Map` — a choice, not something the types force: both other
/// variants derive `Default` too, so only `derive(Default)`'s `#[default]`
/// attribute picks this one, and a hand-written impl yielding a section pane
/// would compile.
#[test]
fn the_default_content_is_a_plan_view_map() {
    assert_eq!(PaneContent::default().kind(), PaneKind::Map);
    assert_eq!(PaneKind::default(), PaneKind::Map);
    assert_eq!(PaneContent::default().render_view(), RenderView::PlanView);
    assert_eq!(MapRender::default(), MapRender::Plan);
}

/// The stand-in box is the resampler's own fallback, it survives un-clamped,
/// and it still covers the whole scan.
#[test]
#[allow(clippy::assertions_on_constants)] // the covering bound IS a constant pin
fn the_stand_in_box_is_the_resamplers_own_fallback_and_survives_it_unclamped() {
    assert_eq!(
        BASE_HALF_WIDTH_KM,
        rustdar_radar::voxel::box_half_width_km(f64::NAN),
        "the pane and the resampler must fall back to one box, or a pane with \
             no grid poses its camera against a width nothing will resample",
    );
    assert!(
        BASE_HALF_WIDTH_KM >= rustdar_radar::types::BASE_EXTENT_KM,
        "the stand-in box must reach the raster's floor: {BASE_HALF_WIDTH_KM} km \
             of half-width against a {} km frame",
        rustdar_radar::types::BASE_EXTENT_KM,
    );
    let stand_in = rustdar_radar::voxel::HalfExtentKm::square(BASE_HALF_WIDTH_KM);
    let region = VolumeRegion::new(point(35.3, -97.3), stand_in)
        .expect("the stand-in half-width must be a region the resampler takes");
    assert_eq!(
        region.half_extent_km(),
        stand_in,
        "the resampler must honour the stand-in un-clamped, or the pane's own \
             camera arithmetic describes a different box than the one built",
    );
}

/// A plan view reads one sweep; the other two read the whole ladder, and
/// giving either of them a volume with cuts deliberately skipped fabricates
/// layers rather than failing.
#[test]
fn only_a_plan_view_is_content_with_one_tilt() {
    assert!(!RenderView::PlanView.reads_whole_volume());
    assert!(RenderView::CrossSection.reads_whole_volume());
    assert!(RenderView::Volume.reads_whole_volume());
    for (render, whole) in [(MapRender::Plan, false), (MapRender::Volume, true)] {
        assert_eq!(
            render.render_view().reads_whole_volume(),
            whole,
            "{render:?} must ask for {} of the ladder",
            if whole { "all" } else { "one tilt" },
        );
    }
    assert_eq!(
        PaneKind::CrossSection.render_view(),
        Some(RenderView::CrossSection),
    );
    assert_eq!(
        PaneKind::Map.render_view(),
        None,
        "a map pane's view is not knowable from its kind, and saying otherwise \
         would pick one of its two pictures at random",
    );
}

/// A line that cannot be cut is not representable.
#[test]
fn a_section_line_refuses_endpoints_it_cannot_be_cut_along() {
    assert!(SectionLine::new(point(35.3, -97.3), point(35.6, -97.0)).is_some());

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

    assert!(SectionLine::new(point(90.0, 180.0), point(-90.0, -180.0)).is_some());

    assert!(
        SectionLine::new(point(35.3, -97.3), point(35.3, -97.3)).is_none(),
        "a zero-length line has no bearing: every column would sample one point"
    );
}

/// `release_textures` is total over the kinds, and callable on each.
#[test]
fn releasing_textures_is_total_over_the_kinds() {
    for kind in [PaneKind::Map, PaneKind::CrossSection] {
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
#[test]
fn a_section_pane_drops_its_texture_and_keeps_its_cut() {
    let ctx = egui::Context::default();
    let texture = ctx.load_texture(
        "section-fixture",
        egui::ColorImage::filled([1, 1], egui::Color32::WHITE),
        egui::TextureOptions::NEAREST,
    );
    let line = SectionLine::new(point(35.3, -97.3), point(35.6, -97.0)).expect("valid line");
    let target = SectionTarget {
        volume: VolumeStamp {
            site: "KTLX".to_owned(),
            collected: chrono::NaiveDate::from_ymd_opt(2026, 7, 30)
                .expect("a real date")
                .and_hms_opt(18, 42, 0)
                .expect("a real time"),
        },
        product: radar_fields::known::REFLECTIVITY,
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
        product: radar_fields::known::REFLECTIVITY,
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

    let mut moved = start;
    moved.nudge(OrbitDelta {
        yaw_deg: -47.5,
        pitch_deg: 12.25,
        zoom_factor: 1.5,
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

    camera.nudge(OrbitDelta {
        yaw_deg: 200.0,
        ..Default::default()
    });
    assert_eq!(camera.yaw_deg(), 95.0);

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

/// The zoom's near stop is 0.05 framing radii — inside the box, by the
/// literal.
#[test]
fn the_zoom_stops_at_a_twentieth_of_a_framing_radius() {
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
            "at {exaggeration}x the zoom's near stop moved; 0.05 framing radii \
                 is the inside-the-box zoom #6 asked for, and anything at or above \
                 1.0 locks the eye out of the box again",
        );
    }
    let restored = OrbitCamera::restore(225.0, 25.0, 0.001, [0.0; 3], 3.0).expect("finite");
    assert_eq!(restored.eye_distance(), 0.05);
}

/// The exaggeration knob clamps a finite value and refuses a non-finite one.
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
