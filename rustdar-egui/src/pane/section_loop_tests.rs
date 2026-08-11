//! The section loop's identity, and the collision it exists to stop.
//!
//! # The defect these are written against
//!
//! [`RenderTarget`] is `(site, product, elevation)` with **no view term**. That
//! was complete while a loop frame could only ever be a plan-view tilt. It is
//! not complete now: a map pane and a cross-section pane on the same site,
//! product and elevation produce two targets that [`RenderTarget::matches`]
//! calls equal — so every predicate that decides "may this finished picture go
//! into that frame?" would say yes across the two kinds.
//!
//! The visible failure is a 2048² plan-view raster animating inside a section
//! pane's axes, captioned with a height scale and a tilt ladder describing a
//! vertical slice that is not there — and the reverse, a 2048×1024 section
//! stretched across a map pane's geographic bounds. It is the loop path's form
//! of the collision `RenderCacheKey`'s view axis closes on the static path,
//! where the same mistake is a wrong-shaped buffer reaching
//! `ColorImage::from_rgba_unmultiplied`'s `assert_eq!` on the main thread.
//!
//! [`LoopPlaybackState::view`] is the fix, and every test below fails without
//! it: each one builds two loops that agree on the whole `RenderTarget` and
//! differ only in what kind of picture they hold.

use super::*;
use rustdar_radar::types::RenderView;

const SITE: &str = "KTLX";
const PRODUCT: RadarProduct = RadarProduct::Reflectivity;
const TILT: f32 = 0.5;

fn site() -> RadarSite {
    rustdar_radar::sites::get_radar_site(SITE)
        .expect("KTLX is a real radar")
        .clone()
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
    RenderTarget::new(SITE, PRODUCT, TILT)
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
    SectionLoopKey::new(line(), None)
}

/// A loop of `count` blank frames in the given view, already retargeted so
/// `rendered_for` (and `section_key`, for a section) is set.
fn loop_in(view: RenderView, count: u32) -> LoopPlaybackState {
    let mut ls = LoopPlaybackState::new_for_loop(3600, &site(), view);
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
    ls.retarget_renders_for(PRODUCT, TILT, section);
    ls
}

fn plan_view_picture(ctx: &egui::Context) -> LoopFrameImage {
    let image = egui::ColorImage::from_rgba_unmultiplied([1, 1], &[255, 255, 255, 255]);
    LoopFrameImage::PlanView(RadarImageData {
        texture: ctx.load_texture("plan", image, egui::TextureOptions::NEAREST),
        lat: 35.33,
        lon: -97.28,
        max_range_km: 230.0,
        value_data: Arc::new(Vec::new()),
    })
}

fn section_picture(ctx: &egui::Context, ladder: u64) -> LoopFrameImage {
    let image = egui::ColorImage::from_rgba_unmultiplied([1, 1], &[255, 255, 255, 255]);
    LoopFrameImage::Section(SectionImageData {
        texture: ctx.load_texture("section", image, egui::TextureOptions::NEAREST),
        axes: axes(),
        tilt_elevations_deg: vec![0.5],
        ladder,
    })
}

/// Axes with one rung, which is all `SectionAxes` needs to be for a placement
/// test — the arithmetic in it is `rustdar_radar::xsect`'s business.
fn axes() -> rustdar_radar::xsect::SectionAxes {
    rustdar_radar::xsect::SectionAxes {
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
///
/// This is the headline collision. `frame_awaiting_render_result` is what
/// `accept_render_result` calls to find the frame a finished plan-view raster
/// belongs in; without the view test it finds one here, because the section
/// loop's `rendered_for` is the same site, product and elevation and the frame
/// carries the same timestamp and is in flight.
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
            .matches(&target),
        "precondition: the two loops agree on the whole RenderTarget, so the \
         refusal below can only come from the view"
    );
    assert_eq!(
        section.frame_awaiting_render_result(ts(1), &target),
        None,
        "a section loop accepted a plan-view raster into a frame it would then \
         animate inside a vertical slice's axes"
    );

    // The same call against a plan-view loop in the same state does find it, so
    // the assertion above is about the view and not about the frame's state.
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
///
/// `poll_loop_render_results` offers a finished plan-view texture to every
/// sibling loop that `is_rendered_for` the result's target. A section loop
/// answers *yes* to that question — it is the same target — so the authority
/// under it has to say no.
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

/// Donation is the *before* half of the same pair — the dispatcher suppresses a
/// render on the promise of it — so it has to refuse the same crossings.
#[test]
fn neither_kind_of_loop_donates_a_frame_to_the_other() {
    let ctx = egui::Context::default();
    let target = shared_target();

    let mut section = loop_in(RenderView::CrossSection, 3);
    section.frames[1].image = Some(section_picture(&ctx, 77));
    assert_eq!(
        section.frame_donatable_to(ts(1), &target),
        None,
        "a section loop offered its raster to a map pane's loop, which would \
         have suppressed that pane's own render and left the frame served by \
         a picture of the wrong thing"
    );

    let mut plan = loop_in(RenderView::PlanView, 3);
    plan.frames[1].image = Some(plan_view_picture(&ctx));
    assert_eq!(
        plan.section_frame_donatable_to(ts(1), &target, &key(), 77),
        None,
        "a map pane's loop offered a plan-view raster to a section loop"
    );

    // Each still donates to its own kind, so the refusals above are about the
    // view rather than about the frames being empty.
    assert_eq!(plan.frame_donatable_to(ts(1), &target), Some(1));
    assert_eq!(
        section.section_frame_donatable_to(ts(1), &target, &key(), 77),
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
        ls.retarget_renders_for(PRODUCT, TILT, Some(SectionLoopKey::new(elsewhere, None))),
        "a redrawn line did not invalidate the loop, so every frame would go \
         on animating a slice of the ground the user moved away from"
    );
    assert!(ls.frames.iter().all(|f| f.image.is_none()));
    assert_eq!(ls.section_key().map(|k| k.line), Some(elsewhere));
}

/// **The stale-vector bug, one frame at a time.**
///
/// A storm-relative section is not a slice of a measured moment: the derivation
/// runs on the way out of the volume, so the picture *is* a function of the
/// vector. That cost `SectionInputKey` a review cycle on the live pane, where
/// the symptom was one visibly wrong redraw. A loop keyed on time alone would
/// reproduce it once per frame — and unlike the live pane, the wrong picture
/// would then sit in a list and animate.
#[test]
fn editing_the_storm_motion_vector_discards_every_frame() {
    let ctx = egui::Context::default();
    let mut ls = LoopPlaybackState::new_for_loop(3600, &site(), RenderView::CrossSection);
    ls.phase = LoopPhase::Rendering;
    ls.frames = (0..3)
        .map(|i| LoopFrame {
            timestamp: ts(i),
            image: None,
            render_in_flight: false,
            render_failed: false,
        })
        .collect();
    let srv = RadarProduct::StormRelativeVelocity;
    ls.retarget_renders_for(
        srv,
        TILT,
        Some(SectionLoopKey::new(line(), Some((30.0, 240.0)))),
    );
    for frame in &mut ls.frames {
        frame.image = Some(section_picture(&ctx, 1));
    }

    assert!(
        ls.retarget_renders_for(
            srv,
            TILT,
            Some(SectionLoopKey::new(line(), Some((35.0, 240.0))))
        ),
        "the storm motion vector moved and the loop kept every frame, so the \
         whole animation goes on showing the old vector's field with nothing \
         saying so"
    );
    assert!(ls.frames.iter().all(|f| f.image.is_none()));
}

/// The vector is stored as raw bits so the comparison is reflexive: rewriting
/// the same key must not invalidate anything, or a section loop would re-cut
/// every frame on every dispatch pass for ever.
#[test]
fn rewriting_the_same_storm_motion_vector_invalidates_nothing() {
    let mut ls = LoopPlaybackState::new_for_loop(3600, &site(), RenderView::CrossSection);
    let srv = RadarProduct::StormRelativeVelocity;
    let motion = Some((30.0, 240.0));
    ls.retarget_renders_for(srv, TILT, Some(SectionLoopKey::new(line(), motion)));
    assert!(
        !ls.retarget_renders_for(srv, TILT, Some(SectionLoopKey::new(line(), motion))),
        "an unchanged vector counted as a change, so every frame is re-cut on \
         every dispatch pass with a hot CPU as the only symptom"
    );
}

/// A raster cut from a different tilt ladder must not be handed across.
///
/// The newest frame's volume is re-cached under the same `(site, timestamp)`
/// key as more of it seals, so one loop can hold a cut from a two-rung ladder
/// while a sibling is about to cut the fourteen-rung one. This is the section's
/// stand-in for the snapped-sweep comparison a plan-view broadcast makes, and
/// it reuses `sampler::ladder_fingerprint` rather than inventing a second
/// notion of section staleness.
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
    );
    assert_eq!(
        ls.frame_accepting_section_broadcast(ts(1), &shared_target(), &elsewhere, 7, Some(7)),
        None,
        "a cut along a different line was accepted into this loop"
    );
}

/// The classification itself: which kinds can animate, and what each one's
/// frame *is*.
#[test]
fn every_kind_can_loop_and_each_frame_is_its_own_shape() {
    assert!(PaneKind::Map.can_loop());
    assert!(
        PaneKind::CrossSection.can_loop(),
        "a cross-section is a raster of one line through one volume, which is \
         exactly what a loop frame is"
    );
    assert!(
        PaneKind::Volume.can_loop(),
        "a 3D volume's loop frame is the resident grid rather than a \
         camera-specific raster, which is what makes orbiting a loop free \
         instead of invalidating every frame of it"
    );
    // The classification is not "everything loops" — it is that each kind's
    // frame is a different shape, and the shapes must not be interchangeable.
    // Without this the enum could grow a fourth variant that answered every
    // accessor with `None` and nothing above would notice.
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
        assert_eq!(image.view(), view);
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
///
/// Without it, enabling a loop on an unaimed section pane fills a frame list
/// nothing can cut. The volumes download perfectly well, so
/// `render_set_settled` never calls the batch settled and the loop sits in
/// `Rendering` for the session — a permanent wait, which this codebase calls
/// the worst state a pane can be in.
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
    pane.loop_state = LoopPlaybackState::new_for_loop(3600, &site(), RenderView::PlanView);
    assert!(pane.loop_state.is_active());

    pane.set_kind(PaneKind::CrossSection);
    assert!(
        !pane.loop_state.is_active(),
        "a map pane's plan-view frames survived the conversion to a section \
         pane, which would animate a list nothing can refill while holding \
         MAX_LOOP_RENDER_BUDGET textures alive to do it"
    );
    assert_eq!(pane.loop_state.view, RenderView::PlanView);
}

/// `active_image` and `active_section_image` read the same frame and each
/// answers only for its own shape, so a caller cannot draw one into the other's
/// chrome.
#[test]
fn the_playhead_answers_only_for_the_shape_it_holds() {
    let ctx = egui::Context::default();
    let mut pane = PaneState::new();
    pane.loop_state = loop_in(RenderView::CrossSection, 3);
    pane.loop_state.current_frame = 1;
    pane.loop_state.frames[1].image = Some(section_picture(&ctx, 5));

    assert!(
        pane.active_image().is_none(),
        "the map painter was handed a cross-section raster"
    );
    assert_eq!(
        pane.active_section_image().map(|s| s.ladder),
        Some(5),
        "the section painter was not handed the frame on the playhead"
    );

    pane.loop_state.frames[1].image = Some(plan_view_picture(&ctx));
    assert!(pane.active_image().is_some());
    assert!(
        pane.active_section_image().is_none(),
        "the section painter was handed a plan-view raster, which it would \
         draw under a height scale and a tilt ladder"
    );
}
