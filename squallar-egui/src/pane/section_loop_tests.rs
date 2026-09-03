//! The section loop's identity, and the collision it exists to stop.

use super::*;
use squallar_geo::GeoPoint;
use squallar_radar::fields as radar_fields;
use squallar_radar::sites::RadarSite;
use squallar_radar::types::RenderView;

const SITE: &str = "KTLX";
const PRODUCT: FieldId = radar_fields::known::REFLECTIVITY;
const TILT: f32 = 0.5;

/// The row every loop below is keyed to, built here rather than read out of
/// the process-wide table.
fn site() -> RadarSite {
    RadarSite {
        name: SITE,
        network: squallar_radar::sites::RadarNetwork::of_id(SITE),
        lat: 35.33306,
        lon: -97.2775,
        heights: None,
    }
}

fn ts(minute: u32) -> NaiveDateTime {
    chrono::NaiveDate::from_ymd_opt(2026, 8, 10)
        .unwrap()
        .and_hms_opt(18, minute, 0)
        .unwrap()
}

/// The one target both loops in every test below are keyed to. Built once so a
/// test cannot accidentally prove its point by disagreeing about the site.
fn shared_target() -> RenderTarget {
    RenderTarget::new(SITE, &PRODUCT, TILT)
}

fn line() -> SectionLine {
    SectionLine::new(
        GeoPoint {
            lat: 35.0,
            lon: -98.0,
        },
        GeoPoint {
            lat: 36.0,
            lon: -97.0,
        },
    )
    .expect("two distinct points on Earth")
}

fn key() -> SectionLoopKey {
    SectionLoopKey::new(line(), None, squallar_radar::srv::SrvFallback::default())
}

/// A loop of `count` blank frames in the given view, already retargeted so
/// `rendered_for` (and `section_key`, for a section) is set.
fn loop_in(view: RenderView, count: u32) -> LayerTimeState {
    let mut ls = crate::radar_layer::begin_loop(3600, &site(), view);
    ls.phase = LoopPhase::Rendering;
    ls.frames = (0..count)
        .map(|i| LoopFrame {
            timestamp: ts(i),
            image: None,
            render_in_flight: false,
            render_failed: false,
        })
        .collect();
    let section = (view == RenderView::CrossSection).then(key);
    ls.retarget_renders_for(&PRODUCT, TILT, section);
    ls
}

fn plan_view_picture(ctx: &egui::Context) -> LoopFrameImage {
    let image = egui::ColorImage::from_rgba_unmultiplied([1, 1], &[255, 255, 255, 255]);
    LoopFrameImage::PlanView(RadarImageData {
        texture: ctx.load_texture("plan", image, egui::TextureOptions::NEAREST),
        lat: 35.33,
        lon: -97.28,
        max_range_km: 230.0,
        placed: squallar_radar::types::ImageBounds::from_radar_site(35.33, -97.28, 230.0).into(),
        nyquist_ms: None,
        melting_layer_source: None,
        storm_motion: None,
        hover: Arc::new(squallar_radar::hover::HoverSource::empty()),
    })
}

fn section_picture(ctx: &egui::Context, ladder: u64) -> LoopFrameImage {
    let image = egui::ColorImage::from_rgba_unmultiplied([1, 1], &[255, 255, 255, 255]);
    LoopFrameImage::Section(SectionImageData {
        texture: ctx.load_texture("section", image, egui::TextureOptions::NEAREST),
        axes: axes(),
        tilt_elevations_deg: vec![0.5],
        tilt_collected_ms: vec![0],
        ladder,
    })
}

/// Axes with one rung, which is all `SectionAxes` needs to be for a placement
/// test — the arithmetic in it is `squallar_radar::xsect`'s business.
fn axes() -> squallar_radar::xsect::SectionAxes {
    squallar_radar::xsect::SectionAxes {
        length_km: 100.0,
        base_km_msl: 0.0,
        top_km_msl: 20.0,
        near_ground_range_km: 0.0,
        far_ground_range_km: 100.0,
        coverage_ground_range_km: 100.0,
        cone_of_silence_km: 0.0,
        tilt_count: 1,
        widest_tilt_gap_deg: 0.0,
        top_tilt_deg: 0.5,
        top_declared_cut_deg: 0.5,
    }
}

/// A plan-view result must not be placed into a section loop, however exactly
/// the two targets agree.
#[test]
fn a_plan_view_result_finds_no_frame_in_a_section_loop_with_the_same_target() {
    let mut section = loop_in(RenderView::CrossSection, 3);
    section.frames[1].render_in_flight = true;
    let target = shared_target();

    assert!(
        section
            .rendered_for
            .as_ref()
            .expect("the loop was retargeted")
            .matches(&target, RenderView::CrossSection),
        "precondition: the two loops agree on the whole RenderTarget, so the \
         refusal below can only come from the view"
    );
    assert_eq!(
        section.frame_awaiting_render_result(ts(1), &target),
        None,
        "a section loop accepted a plan-view raster into a frame it would then \
         animate inside a vertical slice's axes"
    );

    let mut plan = loop_in(RenderView::PlanView, 3);
    plan.frames[1].render_in_flight = true;
    assert_eq!(plan.frame_awaiting_render_result(ts(1), &target), Some(1));
}

/// And the reverse: a finished cut must not be placed into a plan-view loop.
#[test]
fn a_section_result_finds_no_frame_in_a_plan_view_loop_with_the_same_target() {
    let mut plan = loop_in(RenderView::PlanView, 3);
    plan.frames[1].render_in_flight = true;

    assert_eq!(
        plan.frame_awaiting_section_result(ts(1), &shared_target(), &key()),
        None,
        "a plan-view loop accepted a cross-section raster, which it would then \
         stretch across the map pane's geographic bounds"
    );

    let mut section = loop_in(RenderView::CrossSection, 3);
    section.frames[1].render_in_flight = true;
    assert_eq!(
        section.frame_awaiting_section_result(ts(1), &shared_target(), &key()),
        Some(1),
    );
}

/// The sibling broadcast is the other half, and it is the one that reaches
/// panes nobody dispatched anything for.
#[test]
fn a_plan_view_broadcast_is_refused_by_a_section_loop_with_the_same_target() {
    let section = loop_in(RenderView::CrossSection, 3);
    let target = shared_target();
    let sweep = BroadcastSweep {
        rendered: TILT,
        own: Some(TILT),
    };

    assert!(
        section.is_rendered_for(&target),
        "precondition: the cheap refusal in the broadcast loop lets this \
         sibling through, so the authority below is what has to stop it"
    );
    assert!(sweep.agrees(), "precondition: the sweeps agree too");
    assert_eq!(
        section.frame_accepting_broadcast(ts(1), &target, sweep),
        None,
        "a section loop took a plan-view raster off a sibling map pane"
    );

    let plan = loop_in(RenderView::PlanView, 3);
    assert_eq!(
        plan.frame_accepting_broadcast(ts(1), &target, sweep),
        Some(1),
    );
}

/// Redrawing the line makes every frame a picture of somewhere else, and the
/// same call that notices a product change has to notice it.
#[test]
fn moving_the_line_discards_every_frame() {
    let ctx = egui::Context::default();
    let mut ls = loop_in(RenderView::CrossSection, 3);
    for frame in &mut ls.frames {
        frame.image = Some(section_picture(&ctx, 1));
    }

    let elsewhere = SectionLine::new(
        GeoPoint {
            lat: 30.0,
            lon: -99.0,
        },
        GeoPoint {
            lat: 31.0,
            lon: -98.0,
        },
    )
    .expect("two distinct points on Earth");
    assert!(
        ls.retarget_renders_for(
            &PRODUCT,
            TILT,
            Some(SectionLoopKey::new(
                elsewhere,
                None,
                squallar_radar::srv::SrvFallback::default()
            ))
        ),
        "a redrawn line did not invalidate the loop, so every frame would go \
         on animating a slice of the ground the user moved away from"
    );
    assert!(ls.frames.iter().all(|f| f.image.is_none()));
    assert_eq!(ls.section_key().map(|k| k.line), Some(elsewhere));
}

/// **The stale-vector bug, one frame at a time.**
#[test]
fn editing_the_storm_motion_vector_discards_every_frame() {
    let ctx = egui::Context::default();
    let mut ls = crate::radar_layer::begin_loop(3600, &site(), RenderView::CrossSection);
    ls.phase = LoopPhase::Rendering;
    ls.frames = (0..3)
        .map(|i| LoopFrame {
            timestamp: ts(i),
            image: None,
            render_in_flight: false,
            render_failed: false,
        })
        .collect();
    let srv = radar_fields::known::STORM_RELATIVE_VELOCITY;
    ls.retarget_renders_for(
        &srv,
        TILT,
        Some(SectionLoopKey::new(
            line(),
            Some((30.0, 240.0)),
            squallar_radar::srv::SrvFallback::default(),
        )),
    );
    for frame in &mut ls.frames {
        frame.image = Some(section_picture(&ctx, 1));
    }

    assert!(
        ls.retarget_renders_for(
            &srv,
            TILT,
            Some(SectionLoopKey::new(
                line(),
                Some((35.0, 240.0)),
                squallar_radar::srv::SrvFallback::default()
            ))
        ),
        "the storm motion vector moved and the loop kept every frame, so the \
         whole animation goes on showing the old vector's field with nothing \
         saying so"
    );
    assert!(ls.frames.iter().all(|f| f.image.is_none()));
}

/// The products a section can actually be cut of: the ones
/// `derive::volume_slot` admits, which is the gate `dispatch_section_renders`
/// refuses on. Derived rather than listed because *which* products are
/// sectionable is not what these two tests are about — the view is — and a
/// stale hand-list would quietly stop covering a newly sectionable product.
fn sectionable_products() -> Vec<FieldId> {
    radar_fields::known::ALL
        .iter()
        .filter(|p| {
            squallar_radar::fields::product_for(p)
                .is_some_and(|p| squallar_radar::derive::volume_slot(p).is_some())
        })
        .cloned()
        .collect()
}

/// **A tilt click must not re-cut a section loop, and it must still re-render
/// the same product's plan-view loop.** One test, two loops, because the claim
/// is that the answer belongs to the *view*: a fix that simply stopped asking
/// would pass half of this and fail the other half.
#[test]
fn a_tilt_change_re_cuts_no_section_but_still_re_renders_the_plan_view() {
    let ctx = egui::Context::default();
    let products = sectionable_products();
    for pair in [
        radar_fields::known::NORMALIZED_ROTATION,
        radar_fields::known::STORM_RELATIVE_VELOCITY,
    ] {
        assert!(
            products.contains(&pair),
            "fixture: {pair:?} must be sectionable, or the trap this test exists \
             for is not covered",
        );
    }

    for product in products {
        let mut section = crate::radar_layer::begin_loop(3600, &site(), RenderView::CrossSection);
        section.phase = LoopPhase::Rendering;
        section.frames = (0..3)
            .map(|i| LoopFrame {
                timestamp: ts(i),
                image: None,
                render_in_flight: false,
                render_failed: false,
            })
            .collect();
        section.retarget_renders_for(&product, TILT, Some(key()));
        for frame in &mut section.frames {
            frame.image = Some(section_picture(&ctx, 1));
        }

        assert!(
            !section.retarget_renders_for(&product, 19.5, Some(key())),
            "{product:?}: a tilt click re-cut a section the tilt cannot move",
        );
        assert!(
            section.frames.iter().all(|f| f.image.is_some()),
            "{product:?}: a tilt click dropped a cut that would come back \
             byte-identical",
        );

        let mut plan = loop_in(RenderView::PlanView, 3);
        plan.retarget_renders(&product, TILT);
        for frame in &mut plan.frames {
            frame.image = Some(plan_view_picture(&ctx));
        }
        assert!(
            plan.retarget_renders(&product, 19.5),
            "{product:?}: a plan-view loop kept frames drawn from another tilt's \
             sweep",
        );
        assert!(
            plan.frames.iter().all(|f| f.image.is_none()),
            "{product:?}: a plan-view loop left the previous tilt on screen",
        );
    }
}

/// The vector is stored as raw bits so the comparison is reflexive: rewriting
/// the same key must not invalidate anything, or a section loop would re-cut
/// every frame on every dispatch pass for ever.
#[test]
fn rewriting_the_same_storm_motion_vector_invalidates_nothing() {
    let mut ls = crate::radar_layer::begin_loop(3600, &site(), RenderView::CrossSection);
    let srv = radar_fields::known::STORM_RELATIVE_VELOCITY;
    let motion = Some((30.0, 240.0));
    ls.retarget_renders_for(
        &srv,
        TILT,
        Some(SectionLoopKey::new(
            line(),
            motion,
            squallar_radar::srv::SrvFallback::default(),
        )),
    );
    assert!(
        !ls.retarget_renders_for(
            &srv,
            TILT,
            Some(SectionLoopKey::new(
                line(),
                motion,
                squallar_radar::srv::SrvFallback::default()
            ))
        ),
        "an unchanged vector counted as a change, so every frame is re-cut on \
         every dispatch pass with a hot CPU as the only symptom"
    );
}

/// A raster cut from a different tilt ladder must not be handed across.
#[test]
fn a_broadcast_cut_from_another_ladder_is_refused() {
    let ls = loop_in(RenderView::CrossSection, 3);
    let target = shared_target();

    assert_eq!(
        ls.frame_accepting_section_broadcast(ts(1), &target, &key(), 7, Some(7)),
        Some(1),
        "precondition: an agreeing ladder is accepted"
    );
    assert_eq!(
        ls.frame_accepting_section_broadcast(ts(1), &target, &key(), 7, Some(8)),
        None,
        "a raster cut from a ladder this loop's own volume no longer resolves \
         was accepted, so the frame shows a partial volume for ever"
    );
    assert_eq!(
        ls.frame_accepting_section_broadcast(ts(1), &target, &key(), 7, None),
        None,
        "an unverifiable hand-off was accepted; a local cut will follow, and \
         is better than a guess"
    );
}

/// Both halves of the key, always. A sibling cut along another line is not a
/// picture of this loop's slice however well the target agrees.
#[test]
fn a_broadcast_cut_along_another_line_is_refused() {
    let ls = loop_in(RenderView::CrossSection, 3);
    let elsewhere = SectionLoopKey::new(
        SectionLine::new(
            GeoPoint {
                lat: 30.0,
                lon: -99.0,
            },
            GeoPoint {
                lat: 31.0,
                lon: -98.0,
            },
        )
        .expect("two distinct points on Earth"),
        None,
        squallar_radar::srv::SrvFallback::default(),
    );
    assert_eq!(
        ls.frame_accepting_section_broadcast(ts(1), &shared_target(), &elsewhere, 7, Some(7)),
        None,
        "a cut along a different line was accepted into this loop"
    );
}

/// The classification itself: which views can animate, and what each one's
/// frame *is*.
#[test]
fn every_view_can_loop_and_each_frame_is_its_own_shape() {
    assert!(RenderView::PlanView.can_loop());
    assert!(
        RenderView::CrossSection.can_loop(),
        "a cross-section is a raster of one line through one volume, which is \
         exactly what a loop frame is"
    );
    assert!(
        RenderView::Volume.can_loop(),
        "a 3D volume's loop frame is the resident grid rather than a \
         camera-specific raster, which is what makes orbiting a loop free \
         instead of invalidating every frame of it"
    );
    // The classification is not "everything loops" — it is that each view's
    // frame is a different shape, and the shapes must not be interchangeable.
    for (image, view) in [
        (
            LoopFrameImage::Volume(VolumeFrameGrid {
                id: 7,
                target: volume_target(),
            }),
            RenderView::Volume,
        ),
        (
            plan_view_picture(&egui::Context::default()),
            RenderView::PlanView,
        ),
    ] {
        assert_eq!(image.view(), Some(view));
        assert_eq!(
            image.volume().is_some(),
            view == RenderView::Volume,
            "{view:?}: a consumer asking for a resident grid was handed \
             another kind's frame, or refused its own",
        );
        assert!(
            image.section().is_none(),
            "{view:?}: a section consumer was handed a frame that is not one",
        );
    }
}

/// A `VolumeTarget` for the fixtures above: this loop's site, the default box
/// about it, at one arbitrary volume time.
fn volume_target() -> crate::pane::VolumeTarget {
    crate::pane::VolumeTarget {
        volume: crate::pane::VolumeStamp {
            site: SITE.to_owned(),
            collected: ts(1),
        },
        product: PRODUCT,
        region: None,
    }
}

/// A section pane cannot loop until it has been aimed, and the refusal is on
/// the pane rather than on the kind.
#[test]
fn a_section_pane_cannot_loop_until_it_has_a_line() {
    let mut pane = PaneState::new();
    assert!(pane.can_loop(), "precondition: a map pane can");

    pane.set_kind(PaneKind::CrossSection);
    assert!(
        !pane.can_loop(),
        "an unaimed section pane offered a loop, which would fill with frames \
         nothing can cut and never settle"
    );

    pane.cross_section_mut().expect("it is a section pane").line = Some(line());
    assert!(
        pane.can_loop(),
        "an aimed section pane was still refused, so sections cannot loop at all"
    );
}

/// Converting a pane between two kinds that both loop still tears the loop
/// down: the frames are pictures of the old shape and the state's `view` now
/// claims the new one.
#[test]
fn converting_between_two_looping_kinds_tears_the_loop_down() {
    let mut pane = PaneState::new();
    *pane.time_state_mut(&known::RADAR) =
        crate::radar_layer::begin_loop(3600, &site(), RenderView::PlanView);
    assert!(pane.time_state(&known::RADAR).is_active());

    pane.set_kind(PaneKind::CrossSection);
    assert!(
        !pane.time_state(&known::RADAR).is_active(),
        "a map pane's plan-view frames survived the conversion to a section \
         pane, which would animate a list nothing can refill while holding \
         MAX_LOOP_RENDER_BUDGET textures alive to do it"
    );
    assert_eq!(pane.time_state(&known::RADAR).view, RenderView::PlanView);
}

/// `active_image` and `active_section_image` read the same frame and each
/// answers only for its own shape, so a caller cannot draw one into the other's
/// chrome.
#[test]
fn the_playhead_answers_only_for_the_shape_it_holds() {
    let ctx = egui::Context::default();
    let mut pane = PaneState::new();
    *pane.time_state_mut(&known::RADAR) = loop_in(RenderView::CrossSection, 3);
    pane.time_state_mut(&known::RADAR).current_frame = 1;
    pane.time_state_mut(&known::RADAR).frames[1].image = Some(section_picture(&ctx, 5));

    assert!(
        pane.active_image().is_none(),
        "the map painter was handed a cross-section raster"
    );
    assert_eq!(
        pane.active_section_image().map(|s| s.ladder),
        Some(5),
        "the section painter was not handed the frame on the playhead"
    );

    pane.time_state_mut(&known::RADAR).frames[1].image = Some(plan_view_picture(&ctx));
    assert!(pane.active_image().is_some());
    assert!(
        pane.active_section_image().is_none(),
        "the section painter was handed a plan-view raster, which it would \
         draw under a height scale and a tilt ladder"
    );
}
