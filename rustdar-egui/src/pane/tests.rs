use super::*;
use rustdar_device_profile::budget::MAX_PANES_DESKTOP;
use rustdar_radar::fields as radar_fields;
use rustdar_radar::sites::RadarSite;
use rustdar_source::id::known;
use std::collections::HashSet;

/// A panel `w` by `h` logical pixels.
fn panel(w: f32, h: f32) -> egui::Rect {
    egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(w, h))
}

/// The orientation follows the panel's shape: landscape windows get the vertical
/// (right-edge) bar, portrait ones the horizontal (bottom) bar.
#[test]
fn color_scale_orientation_follows_the_panel_shape() {
    for (w, h) in [
        (1920.0, 1080.0),
        (1920.0, 1200.0),
        (1280.0, 1024.0),
        (2340.0, 1080.0),
    ] {
        assert!(
            !ColorScaleOrientation::default().resolve(panel(w, h)),
            "{w}x{h} is landscape: the bar belongs on the right edge"
        );
    }
    for (w, h) in [(1080.0, 2340.0), (1200.0, 1920.0), (1200.0, 1600.0)] {
        assert!(
            ColorScaleOrientation::default().resolve(panel(w, h)),
            "{w}x{h} is portrait: the bar belongs along the bottom"
        );
    }
}

/// The decision is sticky inside the band, which is what makes it hysteresis rather
/// than a threshold: a panel resized back and forth across the middle of the band
/// never flips.
#[test]
fn color_scale_orientation_is_sticky_inside_the_band() {
    let mut from_landscape = ColorScaleOrientation::default();
    assert!(!from_landscape.resolve(panel(1920.0, 1080.0)));
    assert!(
        !from_landscape.resolve(panel(960.0, 1200.0)),
        "1.25 is inside the band"
    );
    assert!(
        !from_landscape.resolve(panel(1000.0, 1200.0)),
        "1.20, exactly the old threshold"
    );
    assert!(
        !from_landscape.resolve(panel(1000.0, 1100.0)),
        "1.10, still inside"
    );

    let mut from_portrait = ColorScaleOrientation::default();
    assert!(from_portrait.resolve(panel(1080.0, 2340.0)));
    assert!(from_portrait.resolve(panel(960.0, 1200.0)));
    assert!(from_portrait.resolve(panel(1000.0, 1200.0)));
    assert!(from_portrait.resolve(panel(1000.0, 1100.0)));

    assert!(
        from_landscape.resolve(panel(1000.0, 1400.0)),
        "1.40 is clearly portrait"
    );
    assert!(
        !from_portrait.resolve(panel(1000.0, 1000.0)),
        "1.00 is clearly not portrait"
    );

    assert!(
        from_landscape.resolve(panel(1000.0, 1200.0)),
        "having flipped to horizontal, 1.20 must now keep it"
    );
    assert!(
        !from_portrait.resolve(panel(1000.0, 1200.0)),
        "having flipped to vertical, the same 1.20 must keep that instead"
    );
}

/// The seed ratio sits in the middle of the band, and both of its edges matter: a
/// first panel at 1.12 (a 16:9 laptop's two-pane split) is vertical, one at 1.25
/// (16:10) is horizontal. Seeding at either band edge instead would move one of
/// them.
#[test]
fn the_first_panel_is_seeded_from_the_middle_of_the_band() {
    assert!(
        !ColorScaleOrientation::default().resolve(panel(1000.0, 1120.0)),
        "1.12 is below the seed ratio"
    );
    assert!(
        ColorScaleOrientation::default().resolve(panel(1000.0, 1250.0)),
        "1.25 is above it"
    );
}

/// A panel that has not been laid out yet must not seed the memory.
#[test]
fn color_scale_orientation_ignores_a_degenerate_panel() {
    for degenerate in [egui::Rect::ZERO, egui::Rect::NOTHING] {
        let mut orientation = ColorScaleOrientation::default();
        assert!(!orientation.resolve(degenerate));
        assert!(
            orientation.resolve(panel(960.0, 1200.0)),
            "the first real panel must still be free to seed, even at 1.25 \
                 where only the seed ratio (not the band edge) says portrait"
        );

        assert!(
            orientation.resolve(degenerate),
            "a degenerate panel must report the remembered orientation"
        );
        assert!(
            orientation.resolve(panel(960.0, 1200.0)),
            "and not have disturbed it"
        );
    }
}

/// A pane count past the grid table is clamped, not flattened.
#[test]
fn a_pane_count_past_the_grid_table_is_clamped_rather_than_flattened() {
    let screen = panel(1600.0, 900.0);
    for count in [MAX_PANES_DESKTOP + 1, 12, usize::MAX] {
        let layout = PaneLayout::for_count(
            count,
            crate::ui_layout::WidthClass::Expanded,
            SplitOrientation::Auto,
        );
        assert_eq!(
            layout.pane_count, MAX_PANES_DESKTOP,
            "{count} panes must land on the largest layout that has a grid"
        );

        let rects: Vec<egui::Rect> = (0..layout.pane_count)
            .map(|idx| layout.pane_rect(idx, screen))
            .collect();
        for (idx, rect) in rects.iter().enumerate() {
            assert!(
                *rect != screen,
                "pane {idx} was handed the whole panel: every pane draws \
                     over every other one"
            );
            let containing = rects.iter().filter(|r| r.contains(rect.center())).count();
            assert_eq!(
                containing, 1,
                "pane {idx}'s own centre lands inside {containing} pane \
                     rects, so a click there names an arbitrary pane"
            );
        }
    }

    assert_eq!(
        PaneLayout::for_count(
            0,
            crate::ui_layout::WidthClass::Expanded,
            SplitOrientation::Auto
        )
        .pane_count,
        1
    );

    for count in 1..=MAX_PANES_DESKTOP {
        let layout = PaneLayout::for_count(
            count,
            crate::ui_layout::WidthClass::Expanded,
            SplitOrientation::Auto,
        );
        assert_eq!(
            layout.grid().iter().sum::<usize>(),
            count,
            "the {count}-pane grid does not have {count} cells"
        );
    }
}

fn ts(minute: u32) -> NaiveDateTime {
    chrono::NaiveDate::from_ymd_opt(2024, 1, 1)
        .unwrap()
        .and_hms_opt(0, minute, 0)
        .unwrap()
}

/// A 1x1 texture handle. `egui::Context` allocates textures through its own texture
/// manager, so this needs no window, GPU, or renderer.
fn dummy_texture(ctx: &egui::Context) -> LoopFrameImage {
    LoopFrameImage::PlanView(dummy_plan_view(ctx))
}

/// The plan-view picture inside [`dummy_texture`], for the tests that read its
/// fields rather than only whether a frame has one.
fn dummy_plan_view(ctx: &egui::Context) -> RadarImageData {
    let image = egui::ColorImage::from_rgba_unmultiplied([1, 1], &[255, 255, 255, 255]);
    RadarImageData {
        texture: ctx.load_texture("test", image, egui::TextureOptions::NEAREST),
        lat: 0.0,
        lon: 0.0,
        max_range_km: 100.0,
        placed: rustdar_radar::types::ImageBounds::from_radar_site(0.0, 0.0, 100.0).into(),
        nyquist_ms: None,
        melting_layer_source: None,
        storm_motion: None,
        hover: Arc::new(rustdar_radar::hover::HoverSource::empty()),
    }
}

/// The site every test loop is built for, unless it is explicitly given another.
const SITE: &str = "KTLX";

/// A site value with the code and coordinates agreeing, as the real table has it.
fn site(name: &'static str, lat: f64, lon: f64) -> RadarSite {
    RadarSite {
        name,
        network: rustdar_radar::sites::RadarNetwork::of_id(name),
        lat,
        lon,
        heights: None,
    }
}

fn loop_with_frames(count: usize, current_frame: usize) -> LayerTimeState {
    loop_for_site(&site(SITE, 35.0, -97.0), count, current_frame)
}

fn loop_for_site(site: &RadarSite, count: usize, current_frame: usize) -> LayerTimeState {
    let mut state = crate::radar_layer::begin_loop(3600, site, RenderView::PlanView);
    state.phase = LoopPhase::Rendering;
    state.frames = (0..count)
        .map(|i| LoopFrame {
            timestamp: ts(i as u32),
            image: None,
            render_in_flight: false,
            render_failed: false,
        })
        .collect();
    state.current_frame = current_frame;
    state
}

/// Every frame's scan has downloaded.
fn all_scans_available(_: &LoopFrame) -> bool {
    true
}

/// The target a render result carries, as stamped by `spawn_loop_frame_render`.
fn target(site: &str, product: &FieldId, elevation: f32) -> RenderTarget {
    RenderTarget::new(site, product, elevation)
}

/// The sweep pair a broadcast normally arrives with: the receiver's own scan
/// snapped the selection to the same angle the image was rendered at.
fn same_sweep() -> BroadcastSweep {
    BroadcastSweep {
        rendered: 0.48,
        own: Some(0.48),
    }
}

#[test]
fn render_set_walks_outward_from_playhead() {
    let state = loop_with_frames(8, 0);
    assert_eq!(state.render_set_indices(5), vec![0, 1, 7, 2, 6]);
}

#[test]
fn render_set_is_capped_and_deduplicated() {
    let state = loop_with_frames(4, 2);
    let indices = state.render_set_indices(12);
    assert_eq!(indices.len(), 4, "cannot exceed the frame count");
    assert_eq!(
        indices.iter().copied().collect::<HashSet<_>>(),
        (0..4).collect::<HashSet<_>>(),
        "every frame covered exactly once"
    );

    assert!(state.render_set_indices(0).is_empty());
    assert!(loop_with_frames(0, 0).render_set_indices(6).is_empty());
}

/// Regression: the render budget is shared with static pane renders, so a loop
/// batch can be starved — only some frames spawn, they finish, and for a moment
/// nothing is in flight while most of the set is still blank.
#[test]
fn starved_frames_block_readiness() {
    let ctx = egui::Context::default();
    let mut state = loop_with_frames(4, 0);
    state.frames[0].image = Some(dummy_texture(&ctx));

    assert!(
        !state.frames.iter().any(|f| f.render_in_flight),
        "precondition: the old 'nothing in flight' predicate would pass here"
    );
    assert!(
        !state.render_set_settled(12, all_scans_available),
        "frames that are pending but not yet spawned must block readiness"
    );
}

#[test]
fn fully_rendered_batch_is_settled() {
    let ctx = egui::Context::default();
    let mut state = loop_with_frames(4, 0);
    for frame in &mut state.frames {
        frame.image = Some(dummy_texture(&ctx));
    }
    assert!(state.render_set_settled(12, all_scans_available));
}

#[test]
fn in_flight_frames_block_readiness() {
    let ctx = egui::Context::default();
    let mut state = loop_with_frames(3, 0);
    state.frames[0].image = Some(dummy_texture(&ctx));
    state.frames[1].image = Some(dummy_texture(&ctx));
    state.frames[2].render_in_flight = true;
    assert!(!state.render_set_settled(12, all_scans_available));
}

/// A frame whose scan has not downloaded cannot be rendered yet, so it must not
/// block readiness — download progress is gated separately by the pending queue.
#[test]
fn undownloaded_frames_do_not_block_readiness() {
    let ctx = egui::Context::default();
    let mut state = loop_with_frames(3, 0);
    state.frames[0].image = Some(dummy_texture(&ctx));
    let downloaded = state.frames[0].timestamp;
    assert!(state.render_set_settled(12, |f| f.timestamp == downloaded));
}

/// A frame that has been ruled out (render attempted and produced nothing) must not
/// block readiness forever, or the loop would wedge in `Rendering`.
#[test]
fn failed_frames_do_not_block_readiness() {
    let ctx = egui::Context::default();
    let mut state = loop_with_frames(3, 0);
    state.frames[0].image = Some(dummy_texture(&ctx));
    state.frames[1].render_failed = true;
    state.frames[2].render_failed = true;
    assert!(state.render_set_settled(12, all_scans_available));
}

/// Nothing has been rendered before the first dispatch, so adopting a target is not
/// an invalidation.
#[test]
fn retarget_is_a_noop_before_the_first_dispatch() {
    let mut state = loop_with_frames(3, 0);
    assert!(state.rendered_for.is_none());
    assert!(!state.retarget_renders(&radar_fields::known::REFLECTIVITY, 0.5));
    let adopted = state.rendered_for.as_ref().expect("target adopted");
    assert!(adopted.matches(
        &target(SITE, &radar_fields::known::REFLECTIVITY, 0.5),
        RenderView::PlanView
    ));
}

#[test]
fn retarget_keeps_frames_when_the_selection_is_unchanged() {
    let ctx = egui::Context::default();
    let mut state = loop_with_frames(3, 0);
    state.retarget_renders(&radar_fields::known::REFLECTIVITY, 0.5);
    state.frames[0].image = Some(dummy_texture(&ctx));

    assert!(!state.retarget_renders(&radar_fields::known::REFLECTIVITY, 0.5));
    assert!(state.frames[0].image.is_some());
    assert!(!state.retarget_renders(&radar_fields::known::REFLECTIVITY, 0.505));
    assert!(state.frames[0].image.is_some());
}

/// `texture` and `render_failed` are both judgements about one product at one
/// elevation, and the pane's combo boxes can change that at any time.
#[test]
fn retarget_discards_frame_state_that_judged_the_old_product() {
    let ctx = egui::Context::default();
    let mut state = loop_with_frames(4, 0);
    state.retarget_renders(&radar_fields::known::VELOCITY, 0.5);
    state.frames[0].image = Some(dummy_texture(&ctx));
    state.frames[1].render_failed = true;
    state.frames[2].render_failed = true;
    state.frames[3].render_in_flight = true;

    assert!(state.retarget_renders(&radar_fields::known::REFLECTIVITY, 0.5));
    assert!(state.frames.iter().all(|f| f.image.is_none()));
    assert!(state.frames.iter().all(|f| !f.render_failed));
    assert!(state.frames.iter().all(|f| !f.render_in_flight));

    assert!(!state.render_set_settled(12, all_scans_available));
}

#[test]
fn retarget_reacts_to_an_elevation_change() {
    let ctx = egui::Context::default();
    let mut state = loop_with_frames(3, 0);
    state.retarget_renders(&radar_fields::known::REFLECTIVITY, 0.5);
    state.frames[0].image = Some(dummy_texture(&ctx));

    assert!(state.retarget_renders(&radar_fields::known::REFLECTIVITY, 1.5));
    assert!(state.frames[0].image.is_none());
    let retargeted = state.rendered_for.as_ref().expect("target adopted");
    assert!(retargeted.matches(
        &target(SITE, &radar_fields::known::REFLECTIVITY, 1.5),
        RenderView::PlanView
    ));
}

/// The four products whose plan view is the same picture at every tilt, named here
/// rather than derived: naming them is what makes this a test of the predicate
/// rather than a restatement of it.
const TILT_INDEPENDENT: [FieldId; 4] = [
    radar_fields::known::ECHO_TOPS_INTERPOLATED,
    radar_fields::known::PROBABILITY_OF_SEVERE_HAIL,
    radar_fields::known::MAX_EXPECTED_HAIL_SIZE,
    radar_fields::known::HYDROMETEOR_CLASSIFICATION,
];

/// A plan-view loop of a product the tilt cannot move must keep its frames when
/// only the tilt moves.
#[test]
fn a_tilt_change_keeps_a_tilt_independent_plan_view_loops_frames() {
    let ctx = egui::Context::default();
    for product in TILT_INDEPENDENT {
        let mut state = loop_with_frames(3, 0);
        state.retarget_renders(&product, 0.5);
        state.frames[0].image = Some(dummy_texture(&ctx));

        assert!(
            !state.retarget_renders(&product, 19.5),
            "{product:?} re-rendered every loop frame for a byte-identical picture",
        );
        assert!(
            state.frames[0].image.is_some(),
            "{product:?} dropped a texture the new tilt cannot change",
        );
    }
}

/// The other half: a product whose pixels really do come from the sweep
/// `find_sweep` picks must still discard, or a tilt click would leave the loop
/// animating the tilt before it with nothing saying so.
#[test]
fn a_tilt_change_still_discards_a_tilt_dependent_plan_view_loops_frames() {
    let ctx = egui::Context::default();
    for product in radar_fields::known::ALL.iter() {
        if TILT_INDEPENDENT.contains(product) {
            continue;
        }
        let mut state = loop_with_frames(3, 0);
        state.retarget_renders(product, 0.5);
        state.frames[0].image = Some(dummy_texture(&ctx));

        assert!(
            state.retarget_renders(product, 19.5),
            "{product:?} kept frames drawn from another tilt's sweep",
        );
        assert!(
            state.frames[0].image.is_none(),
            "{product:?} left a texture of the previous tilt on screen",
        );
    }
}

/// The render target is the *whole* key a frame's image is determined by, and the
/// site is half the geometry: `render_radar_to_image` projects around the site's
/// coordinates, so the same scan at the same product and elevation is a different
/// image per site.
#[test]
fn a_result_rendered_for_another_site_is_rejected() {
    let mut state = loop_with_frames(3, 0);
    state.retarget_renders(&radar_fields::known::REFLECTIVITY, 0.5);
    let frame_ts = state.frames[0].timestamp;
    state.frames[0].render_in_flight = true;

    assert_eq!(
        state.frame_awaiting_render_result(
            frame_ts,
            &target(SITE, &radar_fields::known::REFLECTIVITY, 0.5)
        ),
        Some(0),
        "the loop's own site is accepted"
    );
    assert_eq!(
        state.frame_awaiting_render_result(
            frame_ts,
            &target("KOUN", &radar_fields::known::REFLECTIVITY, 0.5)
        ),
        None,
        "an image projected around another site's coordinates must be rejected"
    );
}

/// The site-change path. Switching site tears the loop down and builds a new one
/// (`LayerTimeState::new()` then `new_for_loop`), which is what closes this
/// today — but only incidentally: once the new loop has listed its scans, adopted
/// the same product/elevation and re-marked a frame in flight, an old render still
/// running for the previous site would be accepted on nothing but a timestamp
/// match.
#[test]
fn a_rebuilt_loop_rejects_the_previous_sites_in_flight_result() {
    let mut old = loop_with_frames(3, 0);
    old.retarget_renders(&radar_fields::known::REFLECTIVITY, 0.5);
    let frame_ts = old.frames[0].timestamp;
    old.frames[0].render_in_flight = true;
    let in_flight_target = old.rendered_for.clone().expect("dispatched target");

    let mut rebuilt = loop_for_site(&site("KOUN", 35.2, -97.5), 3, 0);
    rebuilt.retarget_renders(&radar_fields::known::REFLECTIVITY, 0.5);
    rebuilt.frames[0].render_in_flight = true;

    assert_eq!(
        rebuilt.frames[0].timestamp, frame_ts,
        "precondition: the rebuilt loop lists a frame at the same timestamp"
    );
    assert_eq!(
        rebuilt.frame_awaiting_render_result(frame_ts, &in_flight_target),
        None,
        "the old site's render must not be painted onto the new site's frame"
    );
    assert_eq!(
        rebuilt.frame_awaiting_render_result(
            frame_ts,
            &target("KOUN", &radar_fields::known::REFLECTIVITY, 0.5)
        ),
        Some(0),
        "the new site's own render is still accepted"
    );
}

/// The sibling broadcast hands one pane's finished texture to every other pane
/// keyed to the same target, positioning it with the *receiving* pane's
/// `site_lat`/`site_lon`.
#[test]
fn a_sibling_on_another_site_does_not_accept_the_broadcast() {
    let mut sibling = loop_for_site(&site("KOUN", 35.2, -97.5), 3, 0);
    sibling.retarget_renders(&radar_fields::known::REFLECTIVITY, 0.5);

    assert!(
        !sibling.is_rendered_for(&target(SITE, &radar_fields::known::REFLECTIVITY, 0.5)),
        "same product and elevation, different geometry"
    );
    assert!(sibling.is_rendered_for(&target("KOUN", &radar_fields::known::REFLECTIVITY, 0.5)));
}

/// The render target is compared on the site *code* while frames are projected with
/// the site *coordinates*, so the two must come from one site value.
#[test]
fn a_loop_takes_its_code_and_its_coordinates_from_one_site() {
    let koun = site("KOUN", 35.23, -97.46);
    let state = crate::radar_layer::begin_loop(3600, &koun, RenderView::PlanView);

    let geo = crate::radar_layer::geometry(&state).expect("the loop carries its geometry");
    assert_eq!(geo.site, koun.name);
    assert_eq!(geo.lat, koun.lat);
    assert_eq!(geo.lon, koun.lon);
}

/// The dispatcher's donor search is a second, independent way one pane's image
/// reaches another — it runs *before* rendering and suppresses the receiving pane's
/// own render.
#[test]
fn a_donor_on_another_site_is_not_offered() {
    let ctx = egui::Context::default();
    let mut donor = loop_with_frames(3, 0);
    donor.retarget_renders(&radar_fields::known::REFLECTIVITY, 0.5);
    donor.frames[0].image = Some(dummy_texture(&ctx));
    let frame_ts = donor.frames[0].timestamp;

    assert_eq!(
        donor.frame_donatable_to(
            frame_ts,
            &target(SITE, &radar_fields::known::REFLECTIVITY, 0.5)
        ),
        Some(0),
        "a pane on the same target may take this texture"
    );
    assert_eq!(
        donor.frame_donatable_to(
            frame_ts,
            &target("KOUN", &radar_fields::known::REFLECTIVITY, 0.5)
        ),
        None,
        "a pane whose loop is on another site must render its own"
    );
}

/// The dispatcher suppresses a pane's own render on the promise that the queued
/// render's result will be broadcast to it.
#[test]
fn donor_and_broadcast_agree_on_who_may_serve_a_frame() {
    let ctx = egui::Context::default();
    let mut donor = loop_with_frames(3, 0);
    donor.retarget_renders(&radar_fields::known::REFLECTIVITY, 0.5);
    donor.frames[1].image = Some(dummy_texture(&ctx));
    let frame_ts = donor.frames[1].timestamp;

    let same_site = loop_with_frames(3, 0);
    let mut same_site = same_site;
    same_site.retarget_renders(&radar_fields::known::REFLECTIVITY, 0.5);

    let mut other_site = loop_for_site(&site("KOUN", 35.2, -97.5), 3, 0);
    other_site.retarget_renders(&radar_fields::known::REFLECTIVITY, 0.5);

    for (label, receiver) in [("same site", &same_site), ("other site", &other_site)] {
        let offered = donor
            .frame_donatable_to(frame_ts, receiver.rendered_for.as_ref().unwrap())
            .is_some();
        let accepted = receiver
            .frame_accepting_broadcast(frame_ts, donor.rendered_for.as_ref().unwrap(), same_sweep())
            .is_some();
        assert_eq!(
            offered, accepted,
            "{label}: donor offered={offered} but broadcast accepted={accepted}"
        );
    }

    assert!(
            same_site
                .frame_accepting_broadcast(
                    frame_ts,
                    donor.rendered_for.as_ref().unwrap(),
                    same_sweep(),
                )
                .is_some()
        );
}

/// The donor mirror of `a_textured_frame_does_not_accept_a_broadcast`, and the
/// guard is load-bearing in a way that does not announce itself: offering an
/// untextured frame makes the dispatcher queue a clone and skip its own render, the
/// clone then finds no texture to copy, and the frame ends up untextured, not in
/// flight and not failed — which `render_set_settled` scores as unsettled, so the
/// loop never reaches `Ready`.
#[test]
fn an_untextured_frame_is_not_donatable() {
    let ctx = egui::Context::default();
    let mut donor = loop_with_frames(3, 0);
    donor.retarget_renders(&radar_fields::known::REFLECTIVITY, 0.5);
    let current = target(SITE, &radar_fields::known::REFLECTIVITY, 0.5);
    let frame_ts = donor.frames[0].timestamp;

    assert_eq!(
        donor.frame_donatable_to(frame_ts, &current),
        None,
        "a blank frame has nothing to give"
    );
    donor.frames[0].render_in_flight = true;
    assert_eq!(donor.frame_donatable_to(frame_ts, &current), None);

    donor.frames[0].render_in_flight = false;
    donor.frames[0].image = Some(dummy_texture(&ctx));
    assert_eq!(donor.frame_donatable_to(frame_ts, &current), Some(0));
}

/// A frame that already has an image gains nothing from an identical one, and
/// overwriting it churns texture handles.
#[test]
fn a_textured_frame_does_not_accept_a_broadcast() {
    let ctx = egui::Context::default();
    let mut state = loop_with_frames(3, 0);
    state.retarget_renders(&radar_fields::known::REFLECTIVITY, 0.5);
    let current = target(SITE, &radar_fields::known::REFLECTIVITY, 0.5);
    let frame_ts = state.frames[0].timestamp;

    assert_eq!(
        state.frame_accepting_broadcast(frame_ts, &current, same_sweep()),
        Some(0)
    );
    state.frames[0].image = Some(dummy_texture(&ctx));
    assert_eq!(
        state.frame_accepting_broadcast(frame_ts, &current, same_sweep()),
        None
    );
}

/// The coupled defect. The dispatcher suppresses a duplicate render only when the
/// *snapped* sweeps match (`render_already_queued`), so acceptance has to weigh the
/// same thing — otherwise a pane that was not suppressed, and has its own render
/// running, is handed an image of a different tilt and has that render dropped as
/// redundant.
#[test]
fn a_broadcast_of_a_different_sweep_is_refused() {
    let mut state = loop_with_frames(3, 0);
    state.retarget_renders(&radar_fields::known::REFLECTIVITY, 0.5);
    let current = target(SITE, &radar_fields::known::REFLECTIVITY, 0.5);
    let frame_ts = state.frames[0].timestamp;

    assert!(
        state.is_rendered_for(&current),
        "precondition: a target-only test accepts"
    );

    assert_eq!(
        state.frame_accepting_broadcast(
            frame_ts,
            &current,
            BroadcastSweep {
                rendered: 1.4,
                own: Some(0.48)
            },
        ),
        None,
        "an image of the 1.4° sweep must not fill a frame whose scan snaps to 0.48°"
    );
    assert_eq!(
        state.frame_accepting_broadcast(
            frame_ts,
            &current,
            BroadcastSweep {
                rendered: 0.48,
                own: Some(0.48)
            },
        ),
        Some(0),
        "the same sweep is still handed over — the point of the broadcast"
    );
    assert_eq!(
        state.frame_accepting_broadcast(
            frame_ts,
            &current,
            BroadcastSweep {
                rendered: 0.48,
                own: Some(0.485)
            },
        ),
        Some(0),
        "jitter below the tolerance is the same sweep"
    );
}

/// A receiver that cannot say what its own scan snaps to cannot check the image.
#[test]
fn a_broadcast_is_refused_when_the_receiver_has_no_sweep_of_its_own() {
    let mut state = loop_with_frames(3, 0);
    state.retarget_renders(&radar_fields::known::REFLECTIVITY, 0.5);
    let current = target(SITE, &radar_fields::known::REFLECTIVITY, 0.5);
    let frame_ts = state.frames[0].timestamp;

    assert_eq!(
        state.frame_accepting_broadcast(
            frame_ts,
            &current,
            BroadcastSweep {
                rendered: 0.48,
                own: None
            },
        ),
        None
    );
}

/// The `&mut` form gates on the sweep too — it is the one the response path calls,
/// and it is the path that drops the receiver's in-flight render.
#[test]
fn the_mutable_broadcast_accessor_applies_the_sweep_test() {
    let mut state = loop_with_frames(3, 0);
    state.retarget_renders(&radar_fields::known::REFLECTIVITY, 0.5);
    let current = target(SITE, &radar_fields::known::REFLECTIVITY, 0.5);
    let frame_ts = state.frames[0].timestamp;

    assert!(
        state
            .frame_accepting_broadcast_mut(
                frame_ts,
                &current,
                BroadcastSweep {
                    rendered: 1.4,
                    own: Some(0.48)
                },
            )
            .is_none(),
        "no frame is handed back for an image of the wrong sweep"
    );
    assert!(
        state
            .frame_accepting_broadcast_mut(frame_ts, &current, same_sweep())
            .is_some()
    );
}

/// Single-frame mode keeps a `LayerTimeState` around with stale placeholder site
/// fields.
#[test]
fn an_inactive_loop_takes_nothing_from_any_path() {
    let ctx = egui::Context::default();
    let mut state = loop_with_frames(3, 0);
    state.retarget_renders(&radar_fields::known::REFLECTIVITY, 0.5);
    let current = target(SITE, &radar_fields::known::REFLECTIVITY, 0.5);
    let frame_ts = state.frames[0].timestamp;
    state.frames[0].render_in_flight = true;
    state.frames[1].image = Some(dummy_texture(&ctx));
    let textured_ts = state.frames[1].timestamp;

    assert!(
        state
            .frame_awaiting_render_result(frame_ts, &current)
            .is_some()
    );
    assert!(state.frame_donatable_to(textured_ts, &current).is_some());

    state.phase = LoopPhase::Inactive;

    assert_eq!(state.frame_awaiting_render_result(frame_ts, &current), None);
    assert_eq!(
        state.frame_accepting_broadcast(frame_ts, &current, same_sweep()),
        None
    );
    assert_eq!(state.frame_donatable_to(textured_ts, &current), None);
}

/// The `&mut` forms are what the response path uses; they must resolve to the same
/// frame the index forms name.
#[test]
fn the_mutable_accessors_hand_back_the_frame_that_was_chosen() {
    let mut state = loop_with_frames(3, 0);
    state.retarget_renders(&radar_fields::known::REFLECTIVITY, 0.5);
    let current = target(SITE, &radar_fields::known::REFLECTIVITY, 0.5);

    let shared = state.frames[0].timestamp;
    state.frames[2].timestamp = shared;
    state.frames[2].render_in_flight = true;

    let expected = state.frame_awaiting_render_result(shared, &current);
    assert_eq!(expected, Some(2));

    let frame = state
        .frame_awaiting_render_result_mut(shared, &current)
        .expect("frame handed back");
    frame.render_in_flight = false;
    assert!(!state.frames[2].render_in_flight);
    assert_eq!(state.frame_awaiting_render_result(shared, &current), None);
}

/// The broadcast half of the same property.
#[test]
fn the_broadcast_accessor_hands_back_the_frame_that_was_chosen() {
    let ctx = egui::Context::default();
    let mut state = loop_with_frames(3, 0);
    state.retarget_renders(&radar_fields::known::REFLECTIVITY, 0.5);
    let current = target(SITE, &radar_fields::known::REFLECTIVITY, 0.5);

    let shared = state.frames[0].timestamp;
    state.frames[2].timestamp = shared;
    state.frames[0].image = Some(dummy_texture(&ctx));

    assert_eq!(
        state.frames.iter().position(|f| f.timestamp == shared),
        Some(0),
        "precondition: a timestamp-only lookup lands on the textured frame"
    );
    assert_eq!(
        state.frame_accepting_broadcast(shared, &current, same_sweep()),
        Some(2)
    );

    let frame = state
        .frame_accepting_broadcast_mut(shared, &current, same_sweep())
        .expect("frame handed back");
    frame.image = Some(dummy_texture(&ctx));
    assert!(
        state.frames[2].image.is_some(),
        "frame 2 received the texture"
    );
    assert_eq!(
        state.frame_accepting_broadcast(shared, &current, same_sweep()),
        None,
        "and nothing at this timestamp wants another"
    );
}

/// Elevation is still absorbed at the jitter scale — now by sharing a tenths
/// bucket rather than by a tolerance — and the site is still exact.
#[test]
fn target_matching_tolerates_elevation_jitter_only() {
    let (refl, vel) = (
        &radar_fields::known::REFLECTIVITY,
        &radar_fields::known::VELOCITY,
    );
    let base = target(SITE, refl, 0.5);
    let view = RenderView::PlanView;
    assert!(base.matches(&target(SITE, refl, 0.505), view));
    assert!(!base.matches(&target(SITE, refl, 1.5), view));
    assert!(!base.matches(&target(SITE, vel, 0.5), view));
    assert!(!base.matches(&target("KOUN", refl, 0.5), view));
}

/// **The tilt is part of the acceptance comparison exactly when it selects the
/// picture** — the same question `retarget_renders_keyed` asks, now asked in one
/// place. Walks every `(view, product)` pair and compares against
/// `elevation_selects_picture` rather than against a second list, so the two
/// cannot drift; the tilt-independent products are named at `TILT_INDEPENDENT`
/// above, which is what keeps this from restating the predicate.
#[test]
fn the_acceptance_comparison_asks_the_tilt_only_when_the_tilt_selects_the_picture() {
    let mut dropped = 0;
    let mut kept = 0;
    for view in [
        RenderView::PlanView,
        RenderView::CrossSection,
        RenderView::Volume,
    ] {
        for product in radar_fields::known::ALL.iter() {
            let base = target(SITE, product, 0.5);
            // Far enough apart that no bucket and no tolerance could join them.
            let moved = target(SITE, product, 19.5);
            let same_picture = base.matches(&moved, view);
            assert_eq!(
                same_picture,
                !crate::field_facts::elevation_selects_picture(view, product),
                "{view:?}/{product:?}: the acceptance comparison and \
                 `elevation_selects_picture` disagree about whether the tilt \
                 names a different picture",
            );
            if same_picture {
                dropped += 1
            } else {
                kept += 1
            }
        }
    }
    // Non-triviality floor: both answers must actually occur, or a predicate
    // stuck at one of them would pass the walk above.
    assert!(
        dropped > 0 && kept > 0,
        "the walk saw only one answer ({dropped} dropped, {kept} kept) and so \
         could not have caught a comparison stuck at either",
    );
}

/// **The two boundaries where a bucket and a tolerance disagree, pinned by the
/// exact angles the campaign named.** A bucket is transitive and a tolerance is
/// not, and this is what that difference looks like from the outside: the
/// change is stated here rather than smoothed away.
#[test]
fn the_bucket_and_the_tolerance_part_company_at_two_named_boundaries() {
    let view = RenderView::PlanView;
    let product = &radar_fields::known::REFLECTIVITY;

    // 0.002 apart — inside ELEVATION_TOLERANCE, so this used to be an
    // acceptance; buckets 5 and 6, so it is now an INVALIDATION and the render
    // is dispatched again.
    assert!(
        (0.549_f32 - 0.551_f32).abs() <= ELEVATION_TOLERANCE,
        "precondition: the old rule called these the same angle",
    );
    assert!(
        !target(SITE, product, 0.549).matches(&target(SITE, product, 0.551), view),
        "0.549 and 0.551 straddle a tenths boundary and must no longer match",
    );

    // 0.03 apart — outside ELEVATION_TOLERANCE, so this used to be a refusal;
    // both in bucket 5, so it is now an EARLY-OUT and the render is skipped.
    assert!(
        (0.50_f32 - 0.53_f32).abs() > ELEVATION_TOLERANCE,
        "precondition: the old rule called these different angles",
    );
    assert!(
        target(SITE, product, 0.50).matches(&target(SITE, product, 0.53), view),
        "0.50 and 0.53 share a tenths bucket and must now match",
    );
}

/// Item 2: the accept check and the write must resolve to the same frame.
#[test]
fn the_accepted_frame_is_the_one_that_is_in_flight() {
    let mut state = loop_with_frames(3, 0);
    state.retarget_renders(&radar_fields::known::REFLECTIVITY, 0.5);

    let shared = state.frames[0].timestamp;
    state.frames[2].timestamp = shared;
    state.frames[2].render_in_flight = true;

    assert_eq!(
        state.frames.iter().position(|f| f.timestamp == shared),
        Some(0),
        "precondition: a timestamp-only lookup lands on the wrong frame"
    );
    assert_eq!(
        state.frame_awaiting_render_result(
            shared,
            &target(SITE, &radar_fields::known::REFLECTIVITY, 0.5)
        ),
        Some(2),
        "the result must be written to the frame that was actually dispatched"
    );
}

/// Eviction must keep exactly the render set.
#[test]
fn eviction_keeps_exactly_the_render_set() {
    let ctx = egui::Context::default();
    let mut state = loop_with_frames(10, 4);
    for frame in &mut state.frames {
        frame.image = Some(dummy_texture(&ctx));
    }

    state.evict_textures_outside_render_set(3);

    let textured: HashSet<usize> = state
        .frames
        .iter()
        .enumerate()
        .filter(|(_, f)| f.image.is_some())
        .map(|(i, _)| i)
        .collect();
    assert_eq!(
        textured,
        state
            .render_set_indices(3)
            .into_iter()
            .collect::<HashSet<_>>()
    );
    assert!(state.render_set_settled(3, all_scans_available));
}

/// The defect the in-flight mark alone cannot catch.
#[test]
fn stale_result_is_rejected_after_the_frame_is_respawned() {
    let mut state = loop_with_frames(3, 0);
    state.retarget_renders(&radar_fields::known::VELOCITY, 0.5);
    let frame_ts = state.frames[0].timestamp;
    state.frames[0].render_in_flight = true; // render dispatched for Velocity

    assert!(state.retarget_renders(&radar_fields::known::REFLECTIVITY, 0.5));
    state.frames[0].render_in_flight = true;

    assert!(
        state.frames[0].render_in_flight,
        "precondition: an in-flight-only guard would accept the stale result here"
    );
    assert_eq!(
        state.frame_awaiting_render_result(
            frame_ts,
            &target(SITE, &radar_fields::known::VELOCITY, 0.5)
        ),
        None,
        "a result for the abandoned target must be rejected"
    );
    assert_eq!(
        state.frame_awaiting_render_result(
            frame_ts,
            &target(SITE, &radar_fields::known::REFLECTIVITY, 0.5)
        ),
        Some(0),
        "the re-dispatched render for the current target is still accepted"
    );
}

#[test]
fn results_for_frames_not_awaiting_one_are_rejected() {
    let ctx = egui::Context::default();
    let mut state = loop_with_frames(3, 0);
    state.retarget_renders(&radar_fields::known::REFLECTIVITY, 0.5);
    let frame_ts = state.frames[0].timestamp;

    let current = target(SITE, &radar_fields::known::REFLECTIVITY, 0.5);

    assert_eq!(state.frame_awaiting_render_result(frame_ts, &current), None);
    state.frames[0].image = Some(dummy_texture(&ctx));
    assert_eq!(state.frame_awaiting_render_result(frame_ts, &current), None);

    state.frames[1].render_in_flight = true;
    assert_eq!(state.frame_awaiting_render_result(ts(59), &current), None);
}

/// Eviction now keeps only render-set members, where the previous rule kept the
/// `budget` closest *textured* frames regardless of membership.
#[test]
fn eviction_drops_textured_frames_outside_the_render_set() {
    let ctx = egui::Context::default();
    let mut state = loop_with_frames(10, 0);
    for idx in [2, 3, 4, 5] {
        state.frames[idx].image = Some(dummy_texture(&ctx));
    }
    assert_eq!(state.render_set_indices(3), vec![0, 1, 9]);

    state.evict_textures_outside_render_set(3);

    assert!(
        state.frames.iter().all(|f| f.image.is_none()),
        "none of the textured frames were in the render set"
    );
}

#[test]
fn eviction_is_a_noop_within_budget() {
    let ctx = egui::Context::default();
    let mut state = loop_with_frames(10, 0);
    state.frames[5].image = Some(dummy_texture(&ctx));
    state.frames[6].image = Some(dummy_texture(&ctx));

    state.evict_textures_outside_render_set(3);

    assert!(state.frames[5].image.is_some());
    assert!(state.frames[6].image.is_some());
}

/// Frames outside the budgeted window around the playhead are never rendered, so
/// they must not hold up readiness either.
#[test]
fn frames_outside_the_render_set_do_not_block_readiness() {
    let ctx = egui::Context::default();
    let mut state = loop_with_frames(10, 0);
    for &idx in &state.render_set_indices(3) {
        state.frames[idx].image = Some(dummy_texture(&ctx));
    }
    assert!(state.render_set_settled(3, all_scans_available));
    assert!(
        !state.render_set_settled(10, all_scans_available),
        "widening the budget pulls blank frames back into the set"
    );
}

/// A pane showing a finished velocity render whose cut declared `nyquist_ms`.
fn velocity_pane(ctx: &egui::Context, nyquist_ms: Option<f64>) -> PaneState {
    pane_showing_render(ctx, &radar_fields::known::VELOCITY, nyquist_ms, None, None)
}

/// A pane showing a finished classification render that stood on `source`.
fn classification_pane(
    ctx: &egui::Context,
    source: Option<rustdar_radar::hca::MeltingLayerSource>,
) -> PaneState {
    pane_showing_render(
        ctx,
        &radar_fields::known::HYDROMETEOR_CLASSIFICATION,
        None,
        source,
        None,
    )
}

/// A pane showing a finished storm-relative render shifted by `source`.
fn storm_relative_pane(
    ctx: &egui::Context,
    source: Option<rustdar_radar::srv::StormMotionSource>,
) -> PaneState {
    pane_showing_render(
        ctx,
        &radar_fields::known::STORM_RELATIVE_VELOCITY,
        None,
        None,
        source.map(srm_vector),
    )
}

/// A storm motion vector on `source`'s rung, with the speed and direction the
/// legend would draw.
fn srm_vector(source: rustdar_radar::srv::StormMotionSource) -> rustdar_radar::srv::SrvMotion {
    use rustdar_radar::srv::StormMotionSource as S;
    let (speed_kt, direction_deg) = match source {
        S::UserOverride => (45.0, 210.0),
        S::RpgScitAverage => (31.0, 246.0),
        S::MeanWind => (26.2, 209.5),
        S::BunkersRightMover => (38.2, 224.6),
    };
    rustdar_radar::srv::SrvMotion {
        speed_kt,
        direction_deg,
        source,
    }
}

/// A pane showing a finished render of `product`, described by the facts a render
/// carries about itself that nothing else can recompute.
fn pane_showing_render(
    ctx: &egui::Context,
    product: &FieldId,
    nyquist_ms: Option<f64>,
    melting_layer_source: Option<rustdar_radar::hca::MeltingLayerSource>,
    storm_motion: Option<rustdar_radar::srv::SrvMotion>,
) -> PaneState {
    use crate::overlay_cache::{OverlayTextureData, RadarTextureMeta};

    let image = egui::ColorImage::from_rgba_unmultiplied([1, 1], &[255, 255, 255, 255]);
    let mut pane = PaneState::new();
    pane.set_selected_product(product.clone());
    pane.overlay_cache_mut(&known::RADAR)
        .show(OverlayTextureData {
            texture: ctx.load_texture("fold", image, egui::TextureOptions::NEAREST),
            placed: rustdar_geo::PlacedRaster::of(rustdar_geo::GeoBounds {
                min_lat: 34.0,
                max_lat: 36.0,
                min_lon: -98.0,
                max_lon: -96.0,
            }),
            data_generation: 0,
            render_zoom: 0,
            width: 1,
            height: 1,
            radar_meta: Some(RadarTextureMeta {
                hover: Arc::new(rustdar_radar::hover::HoverSource::empty()),
                lat: 35.0,
                lon: -97.0,
                max_range_km: 100.0,
                nyquist_ms,
                melting_layer_source,
                storm_motion,
                product: product.clone(),
                elevation: 0.5,
            }),
            hit_map: None,
        });
    pane
}

/// A loop frame carrying its own fold limit.
fn plan_view_folding_at(ctx: &egui::Context, nyquist_ms: Option<f64>) -> LoopFrameImage {
    LoopFrameImage::PlanView(RadarImageData {
        nyquist_ms,
        ..dummy_plan_view(ctx)
    })
}

/// While a loop runs, the number on the legend is the *playing frame's*.
#[test]
fn a_looping_pane_reports_the_playing_frames_fold_limit() {
    let ctx = egui::Context::default();
    let mut pane = velocity_pane(&ctx, Some(26.42));
    assert_eq!(
        pane.displayed_nyquist_ms(),
        Some(26.42),
        "precondition: a still pane says what its own static render declared",
    );

    *pane.loop_state_mut() = loop_with_frames(3, 1);
    pane.loop_state_mut().frames[0].image = Some(plan_view_folding_at(&ctx, Some(31.0)));
    pane.loop_state_mut().frames[1].image = Some(plan_view_folding_at(&ctx, Some(22.14)));
    assert_eq!(
        pane.displayed_nyquist_ms(),
        Some(22.14),
        "the pane annotated the replaced static render rather than the frame \
         on the glass",
    );

    pane.loop_state_mut().current_frame = 0;
    assert_eq!(pane.displayed_nyquist_ms(), Some(31.0));

    pane.loop_state_mut().frames[0].image = Some(plan_view_folding_at(&ctx, None));
    assert_eq!(pane.displayed_nyquist_ms(), None);

    pane.loop_state_mut().current_frame = 2;
    assert_eq!(pane.displayed_nyquist_ms(), None);
}

/// Only a plan view of base velocity answers at all.
#[test]
fn only_a_plan_view_of_base_velocity_carries_a_fold_limit() {
    let ctx = egui::Context::default();
    let mut pane = velocity_pane(&ctx, Some(22.14));
    assert_eq!(pane.displayed_nyquist_ms(), Some(22.14));

    for product in &[
        radar_fields::known::STORM_RELATIVE_VELOCITY,
        radar_fields::known::SPECTRUM_WIDTH,
        radar_fields::known::REFLECTIVITY,
    ] {
        pane.set_selected_product(product.clone());
        assert_eq!(
            pane.displayed_nyquist_ms(),
            None,
            "{product:?} was annotated with a velocity fold limit",
        );
    }

    pane.set_selected_product(radar_fields::known::VELOCITY);
    pane.set_map_render(MapRender::Volume);
    assert_eq!(
        pane.displayed_nyquist_ms(),
        None,
        "a 3D pane raymarches a ladder of cuts and cannot fold at one speed",
    );

    pane.set_map_render(MapRender::Plan);
    assert_eq!(pane.displayed_nyquist_ms(), Some(22.14));
    pane.set_kind(PaneKind::CrossSection);
    assert_eq!(
        pane.displayed_nyquist_ms(),
        None,
        "a section cuts through every rung of the ladder at once",
    );
}

/// A classification pane reports the melting layer **its own pixels** stood on.
#[test]
fn a_classification_pane_reports_the_layer_its_pixels_stood_on() {
    use rustdar_radar::hca::MeltingLayerSource;

    let ctx = egui::Context::default();

    let measured = classification_pane(&ctx, Some(MeltingLayerSource::Rpg));
    assert_eq!(
        measured.displayed_melting_layer_source(),
        Some(MeltingLayerSource::Rpg),
    );

    let guessed = classification_pane(&ctx, Some(MeltingLayerSource::FleetDefault));
    assert_eq!(
        guessed.displayed_melting_layer_source(),
        Some(MeltingLayerSource::FleetDefault),
    );

    let mut other = classification_pane(&ctx, Some(MeltingLayerSource::FleetDefault));
    for product in &[
        radar_fields::known::REFLECTIVITY,
        radar_fields::known::VELOCITY,
        radar_fields::known::CORRELATION_COEFFICIENT,
    ] {
        other.set_selected_product(product.clone());
        assert_eq!(
            other.displayed_melting_layer_source(),
            None,
            "{product:?} was described as standing on a melting layer",
        );
    }

    let mut ladder = classification_pane(&ctx, Some(MeltingLayerSource::FleetDefault));
    ladder.set_map_render(MapRender::Volume);
    assert_eq!(ladder.displayed_melting_layer_source(), None);
    ladder.set_map_render(MapRender::Plan);
    ladder.set_kind(PaneKind::CrossSection);
    assert_eq!(ladder.displayed_melting_layer_source(), None);
}

/// While a loop runs, the provenance is the *playing frame's*.
#[test]
fn a_looping_classification_pane_reports_the_playing_frames_layer() {
    use rustdar_radar::hca::MeltingLayerSource;

    let ctx = egui::Context::default();
    let mut pane = classification_pane(&ctx, Some(MeltingLayerSource::Rpg));
    *pane.loop_state_mut() = loop_with_frames(3, 1);
    pane.loop_state_mut().frames[1].image = Some(LoopFrameImage::PlanView(RadarImageData {
        melting_layer_source: Some(MeltingLayerSource::FleetDefault),
        ..dummy_plan_view(&ctx)
    }));
    assert_eq!(
        pane.displayed_melting_layer_source(),
        Some(MeltingLayerSource::FleetDefault),
        "the loop reported the static render's layer, not the frame on the glass",
    );

    pane.loop_state_mut().current_frame = 2;
    assert_eq!(pane.displayed_melting_layer_source(), None);
}

/// An SRV pane reports the vector **its own pixels** were shifted by.
#[test]
fn a_storm_relative_pane_reports_the_vector_its_pixels_were_shifted_by() {
    use rustdar_radar::srv::StormMotionSource;

    let ctx = egui::Context::default();

    let rpg = storm_relative_pane(&ctx, Some(StormMotionSource::RpgScitAverage));
    assert_eq!(
        rpg.displayed_storm_motion(),
        Some(srm_vector(StormMotionSource::RpgScitAverage)),
    );

    for rung in [
        StormMotionSource::UserOverride,
        StormMotionSource::BunkersRightMover,
        StormMotionSource::MeanWind,
    ] {
        let pane = storm_relative_pane(&ctx, Some(rung));
        assert_eq!(
            pane.displayed_storm_motion(),
            Some(srm_vector(rung)),
            "{rung:?} was not reported by the pane it shifted",
        );
    }

    let mut other = storm_relative_pane(&ctx, Some(StormMotionSource::BunkersRightMover));
    for product in &[
        radar_fields::known::REFLECTIVITY,
        radar_fields::known::VELOCITY,
        radar_fields::known::HYDROMETEOR_CLASSIFICATION,
    ] {
        other.set_selected_product(product.clone());
        assert_eq!(
            other.displayed_storm_motion(),
            None,
            "{product:?} was described as having been shifted by a storm motion",
        );
    }

    let mut ladder = storm_relative_pane(&ctx, Some(StormMotionSource::BunkersRightMover));
    ladder.set_map_render(MapRender::Volume);
    assert_eq!(ladder.displayed_storm_motion(), None);
    ladder.set_map_render(MapRender::Plan);
    ladder.set_kind(PaneKind::CrossSection);
    assert_eq!(ladder.displayed_storm_motion(), None);

    assert_eq!(
        storm_relative_pane(&ctx, Some(StormMotionSource::MeanWind))
            .displayed_melting_layer_source(),
        None,
    );
    assert_eq!(
        classification_pane(
            &ctx,
            Some(rustdar_radar::hca::MeltingLayerSource::FleetDefault)
        )
        .displayed_storm_motion(),
        None,
    );
}

/// While a loop runs, the vector is the *playing frame's*.
#[test]
fn a_looping_storm_relative_pane_reports_the_playing_frames_vector() {
    use rustdar_radar::srv::StormMotionSource;

    let ctx = egui::Context::default();
    let mut pane = storm_relative_pane(&ctx, Some(StormMotionSource::RpgScitAverage));
    *pane.loop_state_mut() = loop_with_frames(3, 1);
    pane.loop_state_mut().frames[1].image = Some(LoopFrameImage::PlanView(RadarImageData {
        storm_motion: Some(srm_vector(StormMotionSource::BunkersRightMover)),
        ..dummy_plan_view(&ctx)
    }));
    assert_eq!(
        pane.displayed_storm_motion(),
        Some(srm_vector(StormMotionSource::BunkersRightMover)),
        "the loop reported the static render's vector, not the frame on the glass",
    );

    pane.loop_state_mut().current_frame = 2;
    assert_eq!(pane.displayed_storm_motion(), None);
}

/// **`OneFrame` is the `0` the config file has always written for it**, and
/// every other option is the number it always was. WO-E7a gave the step a type
/// and deliberately did not give it a new serde form: a file written by this
/// build and a file written by the last one say the same thing.
#[test]
fn one_scan_is_the_zero_the_config_file_has_always_written() {
    assert_eq!(TimeStep::OneFrame.as_secs(), 0);
    assert_eq!(TimeStep::from_secs(0), TimeStep::OneFrame);
    for secs in [600, 1800, 3600, 7200, 21600, 43200] {
        assert_eq!(
            TimeStep::from_secs(secs),
            TimeStep::Secs(secs),
            "{secs} is a duration, not the sentinel",
        );
        assert_eq!(
            TimeStep::from_secs(secs).as_secs(),
            secs,
            "{secs} round-trips through the config file's own spelling",
        );
    }
}

/// **Two layers in one pane keep two timelines.** The whole point of moving
/// the loop state onto the slot: a pane animating radar and a pane animating
/// something else are one pane, and neither reads the other's playhead.
#[test]
fn two_layers_in_one_pane_keep_two_timelines() {
    let mut pane = PaneState::with_site(SITE.to_string());
    pane.loop_state_mut().current_frame = 3;
    pane.time_state_mut(&known::MODEL_DATA).current_frame = 7;

    assert_eq!(
        pane.loop_state().current_frame,
        3,
        "the radar layer kept its own playhead",
    );
    assert_eq!(
        pane.time_state(&known::MODEL_DATA).current_frame,
        7,
        "and the model layer kept its own",
    );
    assert_eq!(
        pane.time_state(&known::NWS_ALERTS).current_frame,
        0,
        "a layer nothing has written for is at the start, not at somebody else's frame",
    );
}

/// **A pane with no slot for a layer has no timeline for it either**, and
/// gains one only when something writes. A fresh `PaneState` starts with an
/// empty stack (WO-E6b), so every read of a loop that has not begun has to
/// answer "inactive" without inventing a slot.
#[test]
fn a_pane_with_no_slot_for_a_layer_answers_with_an_empty_timeline() {
    let mut pane = PaneState::with_site(SITE.to_string());
    assert!(
        pane.layers.is_empty(),
        "precondition: a fresh pane carries no slots at all",
    );
    assert!(!pane.loop_state().is_active());
    assert!(pane.loop_state().frames.is_empty());
    assert!(
        pane.slot(&known::RADAR).is_none(),
        "and reading did not create one",
    );

    pane.loop_state_mut().phase = LoopPhase::Playing;

    assert!(
        pane.slot(&known::RADAR).is_some(),
        "writing the timeline gave the layer a slot to keep it on",
    );
    assert!(
        pane.loop_state().is_active(),
        "and the write landed where the read looks",
    );
}

// ── WO-E7b: the pane's clock decides which frame is shown ─────────────────

/// A timeline holding frames at whole minutes `0..count`, active and unparked.
fn timeline_at_minutes(count: u32) -> LayerTimeState {
    let mut state =
        crate::radar_layer::begin_loop(3600, &site(SITE, 35.0, -97.0), RenderView::PlanView);
    state.phase = LoopPhase::Rendering;
    state.frames = (0..count)
        .map(|i| LoopFrame {
            timestamp: ts(i),
            image: None,
            render_in_flight: false,
            render_failed: false,
        })
        .collect();
    state
}

/// **The derivation itself**: the frame a layer shows at instant `T` is the
/// latest one stamped at or before `T`, and under `Live` it is the newest
/// there is. Walked over a clock that lands between frames, exactly on one,
/// before all of them and after all of them — four different questions, and a
/// floor that refuses an answer which is the same for all of them.
#[test]
fn the_frame_shown_is_the_latest_one_at_or_before_the_panes_clock() {
    let timeline = timeline_at_minutes(4);

    let cases: [(TimeMode, usize, &str); 6] = [
        (TimeMode::Live, 3, "Live is the newest frame there is"),
        (
            TimeMode::AsOf(ts(3)),
            3,
            "a clock past every frame rests on the last",
        ),
        (
            TimeMode::AsOf(ts(2)),
            2,
            "a clock exactly on a stamp shows that frame",
        ),
        (
            TimeMode::AsOf(ts(2) + chrono::Duration::seconds(30)),
            2,
            "a clock between two frames shows the earlier one, never the later",
        ),
        (TimeMode::AsOf(ts(1)), 1, "and again one frame back"),
        (
            TimeMode::AsOf(ts(0) - chrono::Duration::seconds(1)),
            0,
            "a clock before every frame gives the render set a centre to grow \
             from - `frame_at` is residency, and 0 is the nearest frame",
        ),
    ];

    let answers: HashSet<usize> = cases
        .iter()
        .map(|(mode, _, _)| timeline.frame_at(*mode))
        .collect();
    assert!(
        answers.len() >= 3,
        "non-triviality floor: the derivation gave {} distinct answers across \
         six clocks spanning the whole timeline, so a constant would pass",
        answers.len(),
    );

    for (mode, want, why) in cases {
        assert_eq!(timeline.frame_at(mode), want, "{why}");
    }

    // And the presentational half of the same six clocks: identical to
    // `frame_at` everywhere a frame qualifies, and `None` where none does.
    for (mode, want, why) in cases.iter().take(5) {
        assert_eq!(
            timeline.qualifying_frame_at(*mode),
            Some(*want),
            "a qualifying clock must present the frame `frame_at` names: {why}",
        );
    }
    assert_eq!(
        timeline.qualifying_frame_at(TimeMode::AsOf(ts(0) - chrono::Duration::seconds(1))),
        None,
        "a clock before every frame presents NOTHING - `TimeAxis::FrameSeries` \
         draws nothing when no frame qualifies, and frame 0 is valid AFTER the \
         instant asked about",
    );
}

/// A timeline holding nothing answers `0` rather than panicking or naming a
/// frame — `frame_at` is read on every pane every frame, including the ones
/// whose listing has not landed.
#[test]
fn an_empty_timeline_names_no_frame_and_does_not_panic() {
    let empty = LayerTimeState::new();
    assert_eq!(empty.frame_at(TimeMode::Live), 0);
    assert_eq!(empty.frame_at(TimeMode::AsOf(ts(5))), 0);
    assert_eq!(empty.playhead_stamp(), None, "and names no instant either");
}

/// **The playhead has one writer.** Moving the pane's clock moves every
/// layer's playhead onto it; `park_on_frame` is the same move said by index,
/// and it takes the frame's own stamp so a second layer lands on the same
/// instant rather than the same index.
#[test]
fn the_clock_moves_the_playhead_and_two_layers_land_on_one_instant() {
    let mut pane = PaneState::new();
    *pane.time_state_mut(&known::RADAR) = timeline_at_minutes(6);
    // A second frame-series layer on a coarser cadence: frames at 0 and 4.
    let mut model = timeline_at_minutes(6);
    model
        .frames
        .retain(|f| f.timestamp == ts(0) || f.timestamp == ts(4));
    *pane.time_state_mut(&known::MODEL_DATA) = model;

    pane.set_time_mode(TimeMode::AsOf(ts(5)));
    assert_eq!(
        pane.time_state(&known::RADAR).current_frame(),
        5,
        "radar has a frame at 5 and shows it",
    );
    assert_eq!(
        pane.time_state(&known::MODEL_DATA).current_frame(),
        1,
        "the coarser layer shows its 4-minute frame — the latest it has at or \
         before the same instant, not the same index",
    );

    // Said by index instead, on radar's frame 2, which is 2 minutes.
    assert!(pane.park_on_loop_frame(2), "frame 2 exists");
    assert_eq!(
        pane.time.mode,
        TimeMode::AsOf(ts(2)),
        "parking on a frame moves the CLOCK to that frame's stamp",
    );
    assert_eq!(pane.time_state(&known::MODEL_DATA).current_frame(), 0);
    assert!(
        !pane.park_on_loop_frame(99),
        "an index naming no frame moves nothing",
    );
    assert_eq!(
        pane.time.mode,
        TimeMode::AsOf(ts(2)),
        "and the clock did not move"
    );

    // **The walk, and its non-vacuity floor.** One clock stepped across the
    // whole window rather than sampled at one instant: at every step each
    // layer must sit on the latest stamp of its OWN list at or before the
    // clock. The property is restated here from the frame lists, never read
    // back off the thing under test.
    //
    // The floor is the point. Two layers that had quietly been put on one
    // cadence would satisfy every equality above and below by showing the same
    // stamp twice, so the walk must find instants where they disagree.
    //
    // Ported from the deleted `fake-source` acceptance suite's alien-cadence
    // criterion, re-pointed at the two layers that really declare
    // `TimeAxis::FrameSeries` — radar on the volume cadence, the model hourly.
    //
    // **The model's grid is re-seeded to start LATE on purpose**, which is the
    // mixed-span case WI-3's contract is about: at the head of the window the
    // clock sits before every stamp the model holds, so it must answer
    // `None` — draw nothing — rather than floor onto its oldest frame. Radar's
    // grid starts at the window's head, so radar always qualifies, and that
    // asymmetry is what makes the walk exercise both halves of the contract.
    let mut late = timeline_at_minutes(6);
    late.frames.retain(|f| f.timestamp >= ts(2));
    *pane.time_state_mut(&known::MODEL_DATA) = late;

    const STEPS: u32 = 6;
    let mut disagreed = 0;
    let mut model_blank = 0;
    for minute in 0..STEPS {
        pane.set_time_mode(TimeMode::AsOf(ts(minute)));
        let mut shown = Vec::new();
        for layer in [&known::RADAR, &known::MODEL_DATA] {
            let state = pane.time_state(layer);
            // The rule restated from the frame list, never read back off the
            // thing under test: the latest stamp at or before the clock, and
            // nothing at all when the layer holds none.
            let want = state
                .frames
                .iter()
                .map(|f| f.timestamp)
                .filter(|t| *t <= ts(minute))
                .max();
            assert_eq!(
                state.playhead_stamp(),
                want,
                "{layer:?} at minute {minute}: a layer shows the latest stamp \
                 of its OWN list at or before the pane's clock, and draws \
                 nothing when it holds none",
            );
            shown.push(want);
        }
        if shown[1].is_none() {
            model_blank += 1;
        }
        if shown[0] != shown[1] {
            disagreed += 1;
        }
    }
    assert!(
        disagreed >= 2,
        "the two layers showed the same instant at every step of the walk \
         ({disagreed} disagreements), so the walk cannot tell one clock over \
         two cadences from one clock over two copies of one cadence",
    );
    // The mixed-span floor, both halves: the late layer really did answer
    // "nothing qualifies" at least once, so the walk reached the case the
    // contract is about — and it did not answer that always, which would be a
    // silently blank layer passing as a fixed contract.
    assert!(
        model_blank > 0,
        "the model qualified at all {STEPS} clock positions, so this walk \
         never reached the empty answer WI-3's contract is about",
    );
    assert!(
        model_blank < STEPS,
        "the model answered `None` at every one of {STEPS} clock positions - a \
         blanket blank satisfies the contract and draws an empty map",
    );
}

/// **The time-primary layer is the topmost animating one**, read off the draw
/// order rather than by knowing which layer is radar. Radar's draw weight (30)
/// sits above the model's (10), so radar wins on any pane that draws both.
#[test]
fn the_clock_follows_the_topmost_animating_layer() {
    let mut pane = PaneState::new();
    assert_eq!(
        pane.clock_layer(),
        None,
        "a pane animating nothing has no clock layer"
    );
    assert!(!pane.playing(), "and is not playing");

    *pane.time_state_mut(&known::MODEL_DATA) = timeline_at_minutes(3);
    assert_eq!(
        pane.clock_layer(),
        Some(&known::MODEL_DATA),
        "the only animating layer is the time-primary one",
    );

    // Radar joins, and the slot list is the draw order bottom-to-top.
    *pane.time_state_mut(&known::RADAR) = timeline_at_minutes(3);
    let order: Vec<&LayerId> = pane.draw_order().collect();
    assert!(
        order.iter().position(|id| **id == known::RADAR)
            > order.iter().position(|id| **id == known::MODEL_DATA),
        "precondition: radar is drawn above the model on this pane",
    );
    assert_eq!(
        pane.clock_layer(),
        Some(&known::RADAR),
        "the topmost animating layer takes the clock",
    );

    pane.time_state_mut(&known::RADAR).phase = LoopPhase::Playing;
    assert!(
        pane.playing(),
        "the pane plays when its time-primary layer does"
    );

    // The arm raised ALONE, because "any layer is playing" passes every other
    // case here: the layer BELOW the time-primary one plays and the
    // time-primary one does not. The pane is paused, and a `playing` that
    // polled the whole stack would say otherwise.
    pane.time_state_mut(&known::MODEL_DATA).phase = LoopPhase::Playing;
    pane.time_state_mut(&known::RADAR).phase = LoopPhase::Paused;
    assert!(
        pane.time_state(&known::MODEL_DATA).is_playing(),
        "precondition: the layer below really is playing",
    );
    assert!(
        !pane.playing(),
        "and a paused time-primary layer is a paused pane, whatever the layer \
         below it is doing",
    );
}

/// **Eviction keeps the pane on the moment, not on the index.** A loop parked
/// at 20 minutes whose older frames are dropped still names 20 minutes; before
/// WO-E7b the index was preserved instead, which silently moved the picture.
#[test]
fn eviction_keeps_the_pane_on_the_moment_it_was_parked_at() {
    let mut pane = PaneState::new();
    *pane.time_state_mut(&known::RADAR) = timeline_at_minutes(5);
    assert!(pane.park_on_loop_frame(2), "parked on the 2-minute frame");

    // The two oldest frames age out of the window.
    pane.time_state_mut(&known::RADAR)
        .frames
        .retain(|f| f.timestamp >= ts(1));
    pane.settle_playheads();

    assert_eq!(
        pane.time_state(&known::RADAR).playhead_stamp(),
        Some(ts(2)),
        "the same instant is on screen, at a different index",
    );
    assert_eq!(
        pane.time_state(&known::RADAR).current_frame(),
        1,
        "which is index 1 now, not the 2 it was",
    );

    // And when the clock falls off the front entirely, the layer stops
    // answering rather than answering with a frame from after the instant
    // asked about. The index still names a frame that exists — the render set
    // needs a centre — but nothing is presented from it.
    pane.time_state_mut(&known::RADAR)
        .frames
        .retain(|f| f.timestamp >= ts(3));
    pane.settle_playheads();
    assert_eq!(
        pane.time_state(&known::RADAR).current_frame(),
        0,
        "residency still has a centre, and the nearest frame is index 0",
    );
    assert_eq!(
        pane.time_state(&known::RADAR).playhead_stamp(),
        None,
        "the pane is parked at 2 minutes and the oldest frame it still holds \
         is 3 - naming ts(3) here would caption a frame valid AFTER the \
         depicted instant as the picture at it",
    );
    assert_eq!(
        pane.time_state(&known::RADAR).qualifying_frame(),
        None,
        "but no frame qualifies at the 2-minute clock, so none is presented",
    );
    assert!(
        pane.active_image().is_none(),
        "and nothing is drawn - this is the mixed-span bug in miniature",
    );
}

/// **A layer with nothing valid yet shows nothing, and says nothing.**
///
/// `TimeAxis::FrameSeries` is explicit that nothing is drawn when no frame
/// qualifies, and this is the case a pane of mixed spans produces the moment
/// it exists: scrub to a clock inside the long layer's window but before the
/// short layer's, and the short layer has no frame valid at that instant. The
/// answer is an empty picture, not the oldest frame it happens to hold — that
/// frame is valid *after* the depicted instant, and presenting it dates the
/// map wrong with nothing on screen to say so.
///
/// Three floors, because "draw nothing" is easy to get trivially right:
///
/// 1. a second layer that *does* have a qualifying frame still resolves — a
///    blanket blank would replace one wrong answer with an empty map;
/// 2. texture residency is unaffected — the render set still centres on the
///    nearest frame, because a scrub forward needs exactly those;
/// 3. a settled render set does not manufacture a picture. If the presented
///    frame were `frame_at(..).unwrap_or(0)` in disguise, `active_image`
///    would be `Some` here.
#[test]
fn a_layer_whose_frames_all_postdate_the_clock_draws_nothing() {
    let ctx = egui::Context::default();
    let mut pane = PaneState::new();

    // The short-span layer: four textured frames, all at or after 5 minutes.
    let mut radar = loop_with_frames(0, 0);
    radar.frames = (5..9)
        .map(|i| LoopFrame {
            timestamp: ts(i),
            image: Some(dummy_texture(&ctx)),
            render_in_flight: false,
            render_failed: false,
        })
        .collect();
    *pane.time_state_mut(&known::RADAR) = radar;

    // The long-span layer, holding a frame at the clock itself.
    let mut model = loop_with_frames(0, 0);
    model.frames = [ts(0), ts(4)]
        .into_iter()
        .map(|timestamp| LoopFrame {
            timestamp,
            image: None,
            render_in_flight: false,
            render_failed: false,
        })
        .collect();
    *pane.time_state_mut(&known::MODEL_DATA) = model;

    // The pane's clock sits before every frame radar holds, inside model's.
    pane.set_time_mode(TimeMode::AsOf(ts(0)));

    // ── The behaviour under test ──────────────────────────────────────────
    // The stamp first, deliberately: when this floor fires, the failure names
    // the instant that was fabricated (`Some(00:05:00)` against a clock at
    // 00:00), which no unrelated breakage produces.
    assert_eq!(
        pane.time_state(&known::RADAR).playhead_stamp(),
        None,
        "the clock is at ts(0) and the oldest radar frame is ts(5); naming a \
         stamp here presents a frame valid AFTER the depicted instant",
    );
    assert_eq!(
        pane.time_state(&known::RADAR).qualifying_frame(),
        None,
        "no radar frame is valid at the depicted instant, so none is named",
    );
    assert!(
        pane.active_image().is_none(),
        "and nothing is drawn, though every frame carries a texture",
    );
    assert_eq!(
        pane.data_time_on_screen(),
        None,
        "the caption says nothing rather than dating the map at ts(5)",
    );

    // ── Floor 1: a qualifying layer still resolves ────────────────────────
    assert_eq!(
        pane.time_state(&known::MODEL_DATA).qualifying_frame(),
        Some(0),
        "the layer that DOES have a frame at the clock still names it - a \
         blanket 'draw nothing' would pass every assertion above and leave \
         the whole map blank",
    );
    assert_eq!(
        pane.time_state(&known::MODEL_DATA).playhead_stamp(),
        Some(ts(0)),
        "and names its real stamp",
    );

    // ── Floor 2: residency is untouched, and centred on the NEAREST frame ─
    let radar = pane.time_state(&known::RADAR);
    assert_eq!(
        radar.current_frame(),
        0,
        "the render set keeps a centre even though nothing is presented",
    );
    assert_eq!(
        radar.render_set_indices(2),
        vec![0, 1],
        "and it is the two NEAREST frames - ts(5) and ts(6) are what a scrub \
         forward reaches first",
    );

    // ── Floor 3: settled textures do not manufacture a picture ────────────
    assert!(
        radar.render_set_settled(2, all_scans_available),
        "precondition: the render set really is fully textured",
    );
    assert!(
        pane.active_image().is_none(),
        "a settled render set is a statement about TEXTURES, not about what \
         the clock names: readiness must not put a frame on screen that the \
         depicted instant does not select",
    );

    // Eviction keeps the nearest frames and drops the far ones - it must not
    // read the absent playhead as 'keep nothing' or 'keep everything'.
    pane.time_state_mut(&known::RADAR)
        .evict_textures_outside_render_set(2);
    let kept: Vec<usize> = pane
        .time_state(&known::RADAR)
        .frames
        .iter()
        .enumerate()
        .filter(|(_, f)| f.image.is_some())
        .map(|(i, _)| i)
        .collect();
    assert_eq!(
        kept,
        vec![0, 1],
        "the two nearest frames kept their textures and the far two lost \
         theirs; a non-qualifying playhead must not evict the wrong set",
    );
    assert!(pane.active_image().is_none(), "and still nothing is drawn",);
}

/// **WO-E7d: the loop caption describes the layer the clock walks.** The
/// caption's four inputs are read off `clock_layer`'s timeline, so a pane
/// animating something other than radar describes *that* rather than
/// describing radar's empty timeline. On every pane in this build the answer
/// is radar, which is why nothing the caption says today moves.
#[test]
fn the_caption_reads_the_timeline_of_the_layer_the_clock_walks() {
    let mut pane = PaneState::new();

    // Only the model animates: it is the clock layer, so its frames are what
    // a caption would describe.
    let mut model = timeline_at_minutes(3);
    model.cadence_secs = Some(3600);
    model.sampled = Some(false);
    *pane.time_state_mut(&known::MODEL_DATA) = model;
    let id = pane.clock_layer().cloned().expect("the model animates");
    assert_eq!(id, known::MODEL_DATA);
    assert_eq!(
        pane.time_state(&id).cadence_secs,
        Some(3600),
        "the caption's cadence comes from the clock layer, not from radar's \
         empty timeline",
    );
    assert_eq!(pane.time_state(&id).frames.len(), 3);

    // Radar joins and takes the clock back, being drawn above the model.
    let mut radar = timeline_at_minutes(9);
    radar.cadence_secs = Some(259);
    radar.sampled = Some(true);
    *pane.time_state_mut(&known::RADAR) = radar;
    let id = pane.clock_layer().cloned().expect("radar animates");
    assert_eq!(id, known::RADAR, "radar is drawn above the model");
    assert_eq!(
        (
            pane.time_state(&id).frames.len(),
            pane.time_state(&id).cadence_secs,
            pane.time_state(&id).sampled,
        ),
        (9, Some(259), Some(true)),
        "and all four of the caption's inputs move with it",
    );

    // A pane animating nothing has no clock layer, so there is no caption —
    // rather than a caption describing an empty radar timeline.
    let bare = PaneState::new();
    assert_eq!(bare.clock_layer(), None);
}

/// **The model loop cap covers a forecast horizon by sampling, and the radar
/// texture cap it must not borrow really is 14.**
///
/// The overlays-side sibling
/// `the_model_loop_cap_leaves_a_grid_for_every_other_pane` pins what the byte
/// budget allows — exactly 8, 23 and 65 grids for a looping pane. It cannot ask
/// the next two questions: `rustdar-overlays` may not see
/// `rustdar-device-profile` (that crate declares `rustdar-radar`, and the
/// overlays → radar edge is charter-cut) and does not own the sampler. This
/// crate sees both, so they are asked here.
///
/// **One** — the 14 that sibling spells by hand, because the charter denies it
/// the import, is the real `WASM_MAX_LOOP_FRAMES`. Without this its 6.0 %
/// overrun could go stale with nothing going red.
///
/// **Two** — the frame counts the model doc's table states are what
/// [`listing_sample_indices`] really returns. It is **not** a fixed stride:
/// handed more hours than the cap it returns *exactly* the cap, anchored on
/// both the first forecast hour and the last.
///
/// The 8/23/65 below are the sibling's figures, restated because the import is
/// forbidden; they are pinned exactly there, so a budget change reddens that
/// test rather than passing quietly here.
#[test]
fn the_model_loop_cap_covers_a_forecast_horizon_by_sampling() {
    use rustdar_device_profile::constants::{
        DESKTOP_MAX_LOOP_FRAMES, MOBILE_MAX_LOOP_FRAMES, WASM_MAX_LOOP_FRAMES,
    };

    // One. The collision, checked against the definition rather than against a
    // number retyped into a doc comment.
    assert_eq!(
        WASM_MAX_LOOP_FRAMES, 14,
        "`model.rs` spells this 14 by hand, because the charter denies it the \
         import, and computes a 6.0 % budget overrun from it. It has moved, so \
         that arithmetic is now stale.",
    );

    // The byte-budget caps, clamped by the device's own frame cap. Both bind
    // somewhere, which is why both are here: the budget binds on wasm, the
    // frame cap on mobile and desktop.
    for (name, budget_cap, frame_cap, expect) in [
        ("wasm32", 8usize, WASM_MAX_LOOP_FRAMES, 8usize),
        ("mobile", 23, MOBILE_MAX_LOOP_FRAMES, 20),
        ("desktop", 65, DESKTOP_MAX_LOOP_FRAMES, 60),
    ] {
        assert_eq!(
            budget_cap.min(frame_cap),
            expect,
            "{name}: min(byte-budget cap {budget_cap}, device frame cap \
             {frame_cap}) is {}, not the {expect} the model doc's sampling \
             table is computed from",
            budget_cap.min(frame_cap),
        );
    }

    // Two. What each clamped cap yields over the two horizons the plan names.
    // A horizon of H hours is H + 1 forecast hours, hour 0 included.
    for (name, cap, hours, frames) in [
        ("wasm", 8usize, 19usize, 8usize),
        ("wasm", 8, 49, 8),
        ("mobile", 20, 19, 19),
        ("mobile", 20, 49, 20),
        ("desktop", 60, 19, 19),
        ("desktop", 60, 49, 49),
    ] {
        let horizon = hours - 1;
        let picked: Vec<usize> =
            listing_sample_indices(hours, cap).unwrap_or_else(|| (0..hours).collect());
        assert_eq!(
            picked.len(),
            frames,
            "{name} at a cap of {cap} draws {} of the {hours} forecast hours \
             across a {horizon} h horizon, not the {frames} the model doc \
             states",
            picked.len(),
        );
        assert!(
            picked.len() <= cap,
            "{name}: {} frames over a cap of {cap}. Every frame is a resident \
             CONUS grid, so this is an overrun of the model byte budget.",
            picked.len(),
        );
        assert_eq!(
            (picked[0], picked[picked.len() - 1]),
            (0, horizon),
            "{name} at a cap of {cap} must span the whole {horizon} h horizon: \
             a loop that stops short presents a partial forecast as the answer",
        );
        // The step is the two integers around the ideal spacing and nothing
        // wider, or the walk reads as a stutter rather than as a loop.
        let steps: Vec<usize> = picked.windows(2).map(|w| w[1] - w[0]).collect();
        let (lo, hi) = (
            steps.iter().copied().min().unwrap_or(1),
            steps.iter().copied().max().unwrap_or(1),
        );
        assert!(
            hi - lo <= 1,
            "{name} at a cap of {cap} over {hours} hours steps between {lo} and \
             {hi} hours",
        );
    }

    // Non-triviality: the sampler decimates on the rows that are supposed to
    // decimate and declines on the rows that are not. Without this the whole
    // table could be satisfied by listings that happened to fit.
    assert!(
        listing_sample_indices(19, 8).is_some() && listing_sample_indices(49, 8).is_some(),
        "the wasm rows are supposed to exercise decimation; if the sampler \
         declines them, they prove nothing",
    );
    assert!(
        listing_sample_indices(49, 60).is_none() && listing_sample_indices(19, 20).is_none(),
        "the desktop rows and mobile's 18 h row are supposed to be \
         undecimated, so a decimating sampler would be checked nowhere",
    );
}
