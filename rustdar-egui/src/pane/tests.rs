use super::*;
use std::collections::HashSet;

/// A panel `w` by `h` logical pixels.
fn panel(w: f32, h: f32) -> egui::Rect {
    egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(w, h))
}

/// The orientation follows the panel's shape: landscape windows get the
/// vertical (right-edge) bar, portrait ones the horizontal (bottom) bar.
#[test]
fn color_scale_orientation_follows_the_panel_shape() {
    // Every landscape desktop/laptop aspect, and a landscape phone.
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
    // Phone and tablet portrait.
    for (w, h) in [(1080.0, 2340.0), (1200.0, 1920.0), (1200.0, 1600.0)] {
        assert!(
            ColorScaleOrientation::default().resolve(panel(w, h)),
            "{w}x{h} is portrait: the bar belongs along the bottom"
        );
    }
}

/// The decision is sticky inside the band, which is what makes it
/// hysteresis rather than a threshold: a panel resized back and forth
/// across the middle of the band never flips.
#[test]
fn color_scale_orientation_is_sticky_inside_the_band() {
    // Seeded landscape, then resized to well inside the band (h/w = 1.25,
    // the ratio a 16:10 laptop's two-pane split used to sit at).
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

    // Seeded portrait, walked through the identical ratios: it keeps the
    // *other* answer. Same input, different history — that is hysteresis.
    let mut from_portrait = ColorScaleOrientation::default();
    assert!(from_portrait.resolve(panel(1080.0, 2340.0)));
    assert!(from_portrait.resolve(panel(960.0, 1200.0)));
    assert!(from_portrait.resolve(panel(1000.0, 1200.0)));
    assert!(from_portrait.resolve(panel(1000.0, 1100.0)));

    // Only leaving the band flips it, in either direction.
    assert!(
        from_landscape.resolve(panel(1000.0, 1400.0)),
        "1.40 is clearly portrait"
    );
    assert!(
        !from_portrait.resolve(panel(1000.0, 1000.0)),
        "1.00 is clearly not portrait"
    );

    // …and the flip is *recorded*, not just returned. If the memory froze
    // at the seed, the band would be one-sided: the same in-band ratio
    // would keep answering with the original orientation, and the bars
    // would snap back the moment the resize came home.
    assert!(
        from_landscape.resolve(panel(1000.0, 1200.0)),
        "having flipped to horizontal, 1.20 must now keep it"
    );
    assert!(
        !from_portrait.resolve(panel(1000.0, 1200.0)),
        "having flipped to vertical, the same 1.20 must keep that instead"
    );
}

/// The seed ratio sits in the middle of the band, and both of its edges
/// matter: a first panel at 1.12 (a 16:9 laptop's two-pane split) is
/// vertical, one at 1.25 (16:10) is horizontal. Seeding at either band edge
/// instead would move one of them.
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
///
/// Both degenerate rects give a NaN ratio, which compares false against
/// everything — so without the guard they quietly record "vertical", and
/// the first *real* panel is then judged against the band's far edge
/// instead of the seed ratio. The panel below is deliberately inside the
/// band, where that difference shows.
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

        // A degenerate rect arriving *later* — a collapsed or hidden panel
        // mid-session — must hand back what is remembered, not a default.
        // Answering `false` there would flip every bar for a frame.
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
///
/// Asserted on the rects rather than on `grid()`: the failure this guards
/// is that `pane_rect` hands every index the whole panel, so what matters
/// is that each pane gets its own cell and that a point inside one cell is
/// inside exactly one. The second claim is `detect_active_pane_click`'s
/// hit-test verbatim — under the old fall-through every rect contained
/// every position, so the active pane flipped 0 → 1 → 0 on successive
/// clicks and panes 2 upward were unreachable.
#[test]
fn a_pane_count_past_the_grid_table_is_clamped_rather_than_flattened() {
    let screen = panel(1600.0, 900.0);
    for count in [MAX_PANES_DESKTOP + 1, 12, usize::MAX] {
        let layout = PaneLayout::for_count(count);
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

    // Zero clamps up for the same reason: `pane_count == 0` with a one-cell
    // grid draws no panes at all while the grid says there is one.
    assert_eq!(PaneLayout::for_count(0).pane_count, 1);

    // …and the table itself still describes exactly as many cells as it
    // claims panes, which is the invariant the clamp exists to preserve.
    for count in 1..=MAX_PANES_DESKTOP {
        let layout = PaneLayout::for_count(count);
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

/// A 1x1 texture handle. `egui::Context` allocates textures through its own
/// texture manager, so this needs no window, GPU, or renderer.
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
        value_data: Arc::new(Vec::new()),
    }
}

/// The site every test loop is built for, unless it is explicitly given another.
const SITE: &str = "KTLX";

/// A site value with the code and coordinates agreeing, as the real table has it.
fn site(name: &'static str, lat: f64, lon: f64) -> RadarSite {
    RadarSite {
        name,
        lat,
        lon,
        heights: None,
    }
}

fn loop_with_frames(count: usize, current_frame: usize) -> LoopPlaybackState {
    loop_for_site(&site(SITE, 35.0, -97.0), count, current_frame)
}

fn loop_for_site(site: &RadarSite, count: usize, current_frame: usize) -> LoopPlaybackState {
    let mut state = LoopPlaybackState::new_for_loop(3600, site, RenderView::PlanView);
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
fn target(site: &str, product: RadarProduct, elevation: f32) -> RenderTarget {
    RenderTarget::new(site, product, elevation)
}

/// The sweep pair a broadcast normally arrives with: the receiver's own scan
/// snapped the selection to the same angle the image was rendered at. Every test
/// that is not *about* the sweep needs this, since a disagreeing pair refuses the
/// frame before anything else is looked at.
fn same_sweep() -> BroadcastSweep {
    BroadcastSweep {
        rendered: 0.48,
        own: Some(0.48),
    }
}

#[test]
fn render_set_walks_outward_from_playhead() {
    let state = loop_with_frames(8, 0);
    // Forward first, then backward (wrapping), alternating.
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
/// nothing is in flight while most of the set is still blank. The old predicate
/// ("no frame is in flight") called that ready and animated blank frames.
#[test]
fn starved_frames_block_readiness() {
    let ctx = egui::Context::default();
    let mut state = loop_with_frames(4, 0);
    // One frame rendered; the rest never got a slot, so nothing is in flight.
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

/// A frame that has been ruled out (render attempted and produced nothing) must
/// not block readiness forever, or the loop would wedge in `Rendering`.
#[test]
fn failed_frames_do_not_block_readiness() {
    let ctx = egui::Context::default();
    let mut state = loop_with_frames(3, 0);
    state.frames[0].image = Some(dummy_texture(&ctx));
    state.frames[1].render_failed = true;
    state.frames[2].render_failed = true;
    assert!(state.render_set_settled(12, all_scans_available));
}

/// Nothing has been rendered before the first dispatch, so adopting a target is
/// not an invalidation.
#[test]
fn retarget_is_a_noop_before_the_first_dispatch() {
    let mut state = loop_with_frames(3, 0);
    assert!(state.rendered_for.is_none());
    assert!(!state.retarget_renders(RadarProduct::Reflectivity, 0.5));
    let adopted = state.rendered_for.as_ref().expect("target adopted");
    assert!(adopted.matches(&target(SITE, RadarProduct::Reflectivity, 0.5)));
}

#[test]
fn retarget_keeps_frames_when_the_selection_is_unchanged() {
    let ctx = egui::Context::default();
    let mut state = loop_with_frames(3, 0);
    state.retarget_renders(RadarProduct::Reflectivity, 0.5);
    state.frames[0].image = Some(dummy_texture(&ctx));

    assert!(!state.retarget_renders(RadarProduct::Reflectivity, 0.5));
    assert!(state.frames[0].image.is_some());
    // Elevation jitter below the tolerance used elsewhere is not a change.
    assert!(!state.retarget_renders(RadarProduct::Reflectivity, 0.505));
    assert!(state.frames[0].image.is_some());
}

/// `texture` and `render_failed` are both judgements about one product at one
/// elevation, and the pane's combo boxes can change that at any time. A frame
/// retired under a product only some scans carry must come back when the user
/// switches to a product every scan carries — otherwise it stays blank forever
/// while readiness counts it as settled, and playback animates with holes.
#[test]
fn retarget_discards_frame_state_that_judged_the_old_product() {
    let ctx = egui::Context::default();
    let mut state = loop_with_frames(4, 0);
    state.retarget_renders(RadarProduct::Velocity, 0.5);
    state.frames[0].image = Some(dummy_texture(&ctx));
    // Retired because their scans carry no Velocity sweep. Readiness counts
    // retired frames as settled (see `failed_frames_do_not_block_readiness`),
    // so left alone these would animate as permanent holes under any product.
    state.frames[1].render_failed = true;
    state.frames[2].render_failed = true;
    // Still rendering Velocity when the user switches away.
    state.frames[3].render_in_flight = true;

    assert!(state.retarget_renders(RadarProduct::Reflectivity, 0.5));
    assert!(state.frames.iter().all(|f| f.image.is_none()));
    assert!(state.frames.iter().all(|f| !f.render_failed));
    // In-flight renders are un-marked so their old-product results are rejected
    // on arrival rather than painted onto a retargeted frame.
    assert!(state.frames.iter().all(|f| !f.render_in_flight));

    // And the loop must render the whole set again before it can be Ready.
    assert!(!state.render_set_settled(12, all_scans_available));
}

#[test]
fn retarget_reacts_to_an_elevation_change() {
    let ctx = egui::Context::default();
    let mut state = loop_with_frames(3, 0);
    state.retarget_renders(RadarProduct::Reflectivity, 0.5);
    state.frames[0].image = Some(dummy_texture(&ctx));

    assert!(state.retarget_renders(RadarProduct::Reflectivity, 1.5));
    assert!(state.frames[0].image.is_none());
    let retargeted = state.rendered_for.as_ref().expect("target adopted");
    assert!(retargeted.matches(&target(SITE, RadarProduct::Reflectivity, 1.5)));
}

/// The render target is the *whole* key a frame's image is determined by, and the
/// site is half the geometry: `render_radar_to_image` projects around the site's
/// coordinates, so the same scan at the same product and elevation is a different
/// image per site. Without the site in the key, "a loop frame's image is fully
/// determined by (timestamp, product, elevation)" is simply false, and the target
/// comparison stops being exact.
#[test]
fn a_result_rendered_for_another_site_is_rejected() {
    let mut state = loop_with_frames(3, 0);
    state.retarget_renders(RadarProduct::Reflectivity, 0.5);
    let frame_ts = state.frames[0].timestamp;
    state.frames[0].render_in_flight = true;

    assert_eq!(
        state
            .frame_awaiting_render_result(frame_ts, &target(SITE, RadarProduct::Reflectivity, 0.5)),
        Some(0),
        "the loop's own site is accepted"
    );
    assert_eq!(
        state.frame_awaiting_render_result(
            frame_ts,
            &target("KOUN", RadarProduct::Reflectivity, 0.5)
        ),
        None,
        "an image projected around another site's coordinates must be rejected"
    );
}

/// The site-change path. Switching site tears the loop down and builds a new one
/// (`LoopPlaybackState::new()` then `new_for_loop`), which is what closes this
/// today — but only incidentally: once the new loop has listed its scans, adopted
/// the same product/elevation and re-marked a frame in flight, an old render still
/// running for the previous site would be accepted on nothing but a timestamp
/// match. Two sites' volume times colliding to the second is unlikely, not
/// impossible, and the frame-list contents are not ours to guarantee.
#[test]
fn a_rebuilt_loop_rejects_the_previous_sites_in_flight_result() {
    let mut old = loop_with_frames(3, 0);
    old.retarget_renders(RadarProduct::Reflectivity, 0.5);
    let frame_ts = old.frames[0].timestamp;
    old.frames[0].render_in_flight = true;
    let in_flight_target = old.rendered_for.clone().expect("dispatched target");

    // User switches site: the loop is rebuilt for the new site and reaches the
    // same state — same timestamp, same selection, frame dispatched again.
    let mut rebuilt = loop_for_site(&site("KOUN", 35.2, -97.5), 3, 0);
    rebuilt.retarget_renders(RadarProduct::Reflectivity, 0.5);
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
            &target("KOUN", RadarProduct::Reflectivity, 0.5)
        ),
        Some(0),
        "the new site's own render is still accepted"
    );
}

/// The sibling broadcast hands one pane's finished texture to every other pane
/// keyed to the same target, positioning it with the *receiving* pane's
/// `site_lat`/`site_lon`. A pane whose loop geometry is another site would draw
/// the image in the wrong place, so the site has to be part of that match too.
#[test]
fn a_sibling_on_another_site_does_not_accept_the_broadcast() {
    let mut sibling = loop_for_site(&site("KOUN", 35.2, -97.5), 3, 0);
    sibling.retarget_renders(RadarProduct::Reflectivity, 0.5);

    assert!(
        !sibling.is_rendered_for(&target(SITE, RadarProduct::Reflectivity, 0.5)),
        "same product and elevation, different geometry"
    );
    assert!(sibling.is_rendered_for(&target("KOUN", RadarProduct::Reflectivity, 0.5)));
}

/// The render target is compared on the site *code* while frames are projected
/// with the site *coordinates*, so the two must come from one site value. If they
/// could disagree every later comparison would be exact and wrong.
#[test]
fn a_loop_takes_its_code_and_its_coordinates_from_one_site() {
    let koun = site("KOUN", 35.23, -97.46);
    let state = LoopPlaybackState::new_for_loop(3600, &koun, RenderView::PlanView);

    assert_eq!(state.site, koun.name);
    assert_eq!(state.site_lat, koun.lat);
    assert_eq!(state.site_lon, koun.lon);
}

/// The dispatcher's donor search is a second, independent way one pane's image
/// reaches another — it runs *before* rendering and suppresses the receiving
/// pane's own render. It has to apply the same site test as the broadcast.
#[test]
fn a_donor_on_another_site_is_not_offered() {
    let ctx = egui::Context::default();
    let mut donor = loop_with_frames(3, 0);
    donor.retarget_renders(RadarProduct::Reflectivity, 0.5);
    donor.frames[0].image = Some(dummy_texture(&ctx));
    let frame_ts = donor.frames[0].timestamp;

    assert_eq!(
        donor.frame_donatable_to(frame_ts, &target(SITE, RadarProduct::Reflectivity, 0.5)),
        Some(0),
        "a pane on the same target may take this texture"
    );
    assert_eq!(
        donor.frame_donatable_to(frame_ts, &target("KOUN", RadarProduct::Reflectivity, 0.5)),
        None,
        "a pane whose loop is on another site must render its own"
    );
}

/// The dispatcher suppresses a pane's own render on the promise that the queued
/// render's result will be broadcast to it. If the donor test and the broadcast
/// test disagree, that promise is broken and the frame is served by neither —
/// blank forever, while readiness waits on it. They must agree frame for frame.
#[test]
fn donor_and_broadcast_agree_on_who_may_serve_a_frame() {
    let ctx = egui::Context::default();
    let mut donor = loop_with_frames(3, 0);
    donor.retarget_renders(RadarProduct::Reflectivity, 0.5);
    donor.frames[1].image = Some(dummy_texture(&ctx));
    let frame_ts = donor.frames[1].timestamp;

    let same_site = loop_with_frames(3, 0);
    let mut same_site = same_site;
    same_site.retarget_renders(RadarProduct::Reflectivity, 0.5);

    let mut other_site = loop_for_site(&site("KOUN", 35.2, -97.5), 3, 0);
    other_site.retarget_renders(RadarProduct::Reflectivity, 0.5);

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

    // And the same-site pair really does transfer, so the agreement is not the
    // trivial "both always refuse".
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
/// untextured frame makes the dispatcher queue a clone and skip its own render,
/// the clone then finds no texture to copy, and the frame ends up untextured, not
/// in flight and not failed — which `render_set_settled` scores as unsettled, so
/// the loop never reaches `Ready`. It cannot self-correct either, because a donor
/// frame outside the donor's own render set is never rendered, so the empty offer
/// repeats every pass. Exactly the "served by neither" failure the paired donor
/// and acceptance tests exist to prevent.
#[test]
fn an_untextured_frame_is_not_donatable() {
    let ctx = egui::Context::default();
    let mut donor = loop_with_frames(3, 0);
    donor.retarget_renders(RadarProduct::Reflectivity, 0.5);
    let current = target(SITE, RadarProduct::Reflectivity, 0.5);
    let frame_ts = donor.frames[0].timestamp;

    assert_eq!(
        donor.frame_donatable_to(frame_ts, &current),
        None,
        "a blank frame has nothing to give"
    );
    // Being mid-render is not having an image either.
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
    state.retarget_renders(RadarProduct::Reflectivity, 0.5);
    let current = target(SITE, RadarProduct::Reflectivity, 0.5);
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
/// redundant. Nothing re-renders the frame afterwards: it is textured, so the
/// dispatcher skips it and readiness counts it settled. The wrong sweep is final.
#[test]
fn a_broadcast_of_a_different_sweep_is_refused() {
    let mut state = loop_with_frames(3, 0);
    state.retarget_renders(RadarProduct::Reflectivity, 0.5);
    let current = target(SITE, RadarProduct::Reflectivity, 0.5);
    let frame_ts = state.frames[0].timestamp;

    // Same site, same product, same *selection* — the target matches exactly.
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
    // Sweep angles round-trip through the scan's own radials, so they are compared
    // with the same tolerance as every other angle here.
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
/// Refusing costs one local render once the scan lands; accepting would paint an
/// unverified tilt that nothing revisits.
#[test]
fn a_broadcast_is_refused_when_the_receiver_has_no_sweep_of_its_own() {
    let mut state = loop_with_frames(3, 0);
    state.retarget_renders(RadarProduct::Reflectivity, 0.5);
    let current = target(SITE, RadarProduct::Reflectivity, 0.5);
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
    state.retarget_renders(RadarProduct::Reflectivity, 0.5);
    let current = target(SITE, RadarProduct::Reflectivity, 0.5);
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

/// Single-frame mode keeps a `LoopPlaybackState` around with stale placeholder
/// site fields. Nothing may be applied to it through any path.
#[test]
fn an_inactive_loop_takes_nothing_from_any_path() {
    let ctx = egui::Context::default();
    let mut state = loop_with_frames(3, 0);
    state.retarget_renders(RadarProduct::Reflectivity, 0.5);
    let current = target(SITE, RadarProduct::Reflectivity, 0.5);
    let frame_ts = state.frames[0].timestamp;
    state.frames[0].render_in_flight = true;
    state.frames[1].image = Some(dummy_texture(&ctx));
    let textured_ts = state.frames[1].timestamp;

    // Precondition: everything is accepted while the loop is active.
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

/// The `&mut` forms are what the response path uses; they must resolve to the
/// same frame the index forms name.
#[test]
fn the_mutable_accessors_hand_back_the_frame_that_was_chosen() {
    let mut state = loop_with_frames(3, 0);
    state.retarget_renders(RadarProduct::Reflectivity, 0.5);
    let current = target(SITE, RadarProduct::Reflectivity, 0.5);

    let shared = state.frames[0].timestamp;
    state.frames[2].timestamp = shared;
    state.frames[2].render_in_flight = true;

    let expected = state.frame_awaiting_render_result(shared, &current);
    assert_eq!(expected, Some(2));

    let frame = state
        .frame_awaiting_render_result_mut(shared, &current)
        .expect("frame handed back");
    frame.render_in_flight = false;
    // The mark was cleared on frame 2, not on the other frame with this timestamp.
    assert!(!state.frames[2].render_in_flight);
    assert_eq!(state.frame_awaiting_render_result(shared, &current), None);
}

/// The broadcast half of the same property. This is the accessor the response path
/// actually calls, and duplicate timestamps are no more structurally prevented for
/// it than for the render-result accessor.
#[test]
fn the_broadcast_accessor_hands_back_the_frame_that_was_chosen() {
    let ctx = egui::Context::default();
    let mut state = loop_with_frames(3, 0);
    state.retarget_renders(RadarProduct::Reflectivity, 0.5);
    let current = target(SITE, RadarProduct::Reflectivity, 0.5);

    // Two frames at one timestamp, the first already textured — so the frame that
    // may take a broadcast is the *second*, not the one a plain lookup would reach.
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

/// Elevation is still compared with tolerance, and the site exactly.
#[test]
fn target_matching_tolerates_elevation_jitter_only() {
    let base = target(SITE, RadarProduct::Reflectivity, 0.5);
    assert!(base.matches(&target(SITE, RadarProduct::Reflectivity, 0.505)));
    assert!(!base.matches(&target(SITE, RadarProduct::Reflectivity, 1.5)));
    assert!(!base.matches(&target(SITE, RadarProduct::Velocity, 0.5)));
    assert!(!base.matches(&target("KOUN", RadarProduct::Reflectivity, 0.5)));
}

/// Item 2: the accept check and the write must resolve to the same frame. The old
/// shape asked "is *some* frame with this timestamp in flight?" and left the caller
/// to fetch "the frame with this timestamp" — two lookups free to disagree, which
/// would clear one frame and leave the dispatched one marked in flight forever.
/// Returning the index makes disagreement unrepresentable.
#[test]
fn the_accepted_frame_is_the_one_that_is_in_flight() {
    let mut state = loop_with_frames(3, 0);
    state.retarget_renders(RadarProduct::Reflectivity, 0.5);

    // Two frames sharing a timestamp. Deduplication upstream makes this
    // unreachable today; nothing in this type enforces it.
    let shared = state.frames[0].timestamp;
    state.frames[2].timestamp = shared;
    state.frames[2].render_in_flight = true;

    assert_eq!(
        state.frames.iter().position(|f| f.timestamp == shared),
        Some(0),
        "precondition: a timestamp-only lookup lands on the wrong frame"
    );
    assert_eq!(
        state.frame_awaiting_render_result(shared, &target(SITE, RadarProduct::Reflectivity, 0.5)),
        Some(2),
        "the result must be written to the frame that was actually dispatched"
    );
}

/// Eviction must keep exactly the render set. A rule that disagreed with the
/// dispatcher would drop textures for frames about to be re-rendered.
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

/// The defect the in-flight mark alone cannot catch. `retarget_renders` un-marks
/// the frame, but the *same* dispatch pass re-spawns it for the new target and
/// marks it again — so when the older render finishes first (it started seconds
/// earlier on the same workload) it arrives at a frame that is genuinely in
/// flight. Only the target stamped on the result identifies it as stale. Left
/// unchecked the frame keeps the previous product's image forever: the dispatcher
/// skips textured frames, readiness counts it settled, and the newer result is
/// then dropped because the frame is no longer marked.
#[test]
fn stale_result_is_rejected_after_the_frame_is_respawned() {
    let mut state = loop_with_frames(3, 0);
    state.retarget_renders(RadarProduct::Velocity, 0.5);
    let frame_ts = state.frames[0].timestamp;
    state.frames[0].render_in_flight = true; // render dispatched for Velocity

    // User switches product; the same dispatch pass re-spawns and re-marks.
    assert!(state.retarget_renders(RadarProduct::Reflectivity, 0.5));
    state.frames[0].render_in_flight = true;

    assert!(
        state.frames[0].render_in_flight,
        "precondition: an in-flight-only guard would accept the stale result here"
    );
    assert_eq!(
        state.frame_awaiting_render_result(frame_ts, &target(SITE, RadarProduct::Velocity, 0.5)),
        None,
        "a result for the abandoned target must be rejected"
    );
    assert_eq!(
        state
            .frame_awaiting_render_result(frame_ts, &target(SITE, RadarProduct::Reflectivity, 0.5)),
        Some(0),
        "the re-dispatched render for the current target is still accepted"
    );
}

#[test]
fn results_for_frames_not_awaiting_one_are_rejected() {
    let ctx = egui::Context::default();
    let mut state = loop_with_frames(3, 0);
    state.retarget_renders(RadarProduct::Reflectivity, 0.5);
    let frame_ts = state.frames[0].timestamp;

    let current = target(SITE, RadarProduct::Reflectivity, 0.5);

    // Never dispatched, or already satisfied by a sibling pane's broadcast.
    assert_eq!(state.frame_awaiting_render_result(frame_ts, &current), None);
    state.frames[0].image = Some(dummy_texture(&ctx));
    assert_eq!(state.frame_awaiting_render_result(frame_ts, &current), None);

    // A timestamp that is not in the frame list at all (list rebuilt since dispatch).
    state.frames[1].render_in_flight = true;
    assert_eq!(state.frame_awaiting_render_result(ts(59), &current), None);
}

/// Eviction now keeps only render-set members, where the previous rule kept the
/// `budget` closest *textured* frames regardless of membership. Out-of-set
/// textures are frames the dispatcher will never refresh, so this is deliberate;
/// the visible effect is that scrubbing back to one blanks until it re-renders.
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
    // Textured, but deliberately far from the playhead and outside the render set.
    state.frames[5].image = Some(dummy_texture(&ctx));
    state.frames[6].image = Some(dummy_texture(&ctx));

    state.evict_textures_outside_render_set(3);

    assert!(state.frames[5].image.is_some());
    assert!(state.frames[6].image.is_some());
}

/// Frames outside the budgeted window around the playhead are never rendered,
/// so they must not hold up readiness either.
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
