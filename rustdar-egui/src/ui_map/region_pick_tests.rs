//! The armed 3D region pick, driven through the real UI.
//!
//! `ui_region::tests` pins [`RegionDrag`](crate::ui_region::RegionDrag)'s
//! arithmetic as a function. This file pins the things that are only true of
//! the *gesture*: which pane a press belongs to, which pane the finished box
//! lands on, that the box is drawn back where it came from, that the two armed
//! modes cannot both own one press, and that the mode can be got out of.
//!
//! Two of these are load-bearing beyond the feature:
//!
//! * [`the_captions_box_figure_is_identical_before_and_after_a_zoom_for_a_dragged_region`]
//!   is the user's own acceptance test, applied to a region they picked rather
//!   than one a fixture wrote. The stored region exists because a zoom used to
//!   re-cut the box, and a selector is the one thing that could put a *second*
//!   writer on that path.
//! * [`the_floor_is_framed_on_a_dragged_region_rather_than_on_the_site`] is the
//!   defect that killed this feature the first time. The box and the floor
//!   under it were sized by two independent things, so a floor smaller than the
//!   box left the volume standing on transparency. It is answered now by the
//!   floor being framed *on the region* — and a selector is exactly what makes
//!   a region stop being centred on the site, which is the case that would
//!   expose a framing that had quietly gone back to reading the site.

use super::*;
use crate::input_harness::InputHarness;
use crate::volume_view::StubVolumePainter;
use std::sync::Arc;

const FRAME_DT: f64 = 1.0 / 60.0;

/// A harness with two map panes, both plan views, a scan loaded and a painting
/// volume painter installed.
///
/// Both panes start as plan views on purpose: the drag happens on one, and
/// where the box goes is the target rule's answer rather than the fixture's.
fn pick_harness() -> (InputHarness, Arc<StubVolumePainter>) {
    let painter = Arc::new(StubVolumePainter::painting());
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    h.set_pane_count(2);
    h.load_scan("KTLX");
    h.gui_mut()
        .apply(crate::shell_api::GuiEvent::VolumePainter(Some(
            painter.clone(),
        )));
    h.frames_for(2, FRAME_DT);
    (h, painter)
}

/// Two points on pane `idx` that a drag between will make a box comfortably
/// clear of the resampler's minimum: the pane's centre, and a corner a quarter
/// of the pane away.
///
/// Read off the live pane rather than stated in degrees, so the fixture means
/// the same thing at whatever zoom the pane opens at.
fn centre_and_corner(h: &InputHarness, idx: usize) -> (egui::Pos2, egui::Pos2) {
    let rect = h.pane_rects()[idx];
    (
        rect.center(),
        rect.center() + egui::vec2(rect.width() * 0.25, 0.0),
    )
}

/// Pose pane `idx`'s camera at `(yaw, pitch, standoff)`.
///
/// Through [`OrbitCamera::restore`](crate::pane::OrbitCamera::restore), which is
/// the only public writer of the angles — there are no per-angle setters, and a
/// test that reached into the fields would be able to build a camera `nudge`
/// could never produce. It clamps rather than refuses an out-of-range pitch, so
/// 89 below is the real stop rather than a number that happens to be inside it.
fn pose(h: &mut InputHarness, idx: usize, yaw_deg: f32, pitch_deg: f32, standoff: f32) {
    let volume = h
        .gui_mut()
        .pane_mut(idx)
        .expect("a pane")
        .volume_mut()
        .expect("a 3D pane");
    volume.camera = crate::pane::OrbitCamera::restore(
        yaw_deg,
        pitch_deg,
        standoff,
        volume.camera.pivot(),
        volume.camera.vertical_exaggeration(),
    )
    .expect("finite angles");
}

/// Pane `idx`'s caption line naming the box, as painted.
///
/// Off the screen rather than off the field behind it: a caption that disagreed
/// with the box would be its own defect, and it is the one the user would see.
fn box_line(h: &InputHarness, idx: usize) -> String {
    h.painted_text_strings_in(h.pane_rects()[idx])
        .iter()
        .flat_map(|block| block.lines())
        .find(|line| line.contains("km box"))
        .map(str::to_owned)
        .expect("a painting 3D pane must caption the box it drew")
}

/// **The happy path**: arming the mode and dragging a square on a map pane
/// gives some pane a 3D view of exactly that ground.
///
/// The baseline the rest of this file is measured against — every other test
/// here asserts that some condition changes this outcome, and would pass
/// vacuously if the gesture never worked at all.
///
/// The layout **grows**: there is room, and a 3D view beside the map it was
/// picked from is the picture the feature is for. Converting the map under the
/// box the user just drew is the one answer that is certainly wrong, and it is
/// what the target rule's second step exists to avoid.
#[test]
fn a_drag_on_an_armed_map_pane_gives_a_pane_a_3d_view_of_that_ground() {
    let (mut h, _painter) = pick_harness();
    h.set_region_pick_armed(true);
    h.warm_up();

    let (centre, corner) = centre_and_corner(&h, 0);
    let want = h.ground_at(0, centre);
    h.drag_region(centre, corner);

    let target = (0..h.pane_rects().len())
        .find(|&idx| h.volume_region(idx).is_some())
        .expect(
            "a dragged region must land on some pane - a gesture that completes and \
                 produces no visible change is indistinguishable from one the app dropped",
        );
    assert_ne!(
        target, 0,
        "the region was applied to the map it was drawn on while another pane was \
         available - taking the map out from under the box the user just drew is the \
         one conversion that is certainly wrong",
    );

    let region = h.volume_region(target).expect("just found");
    assert!(
        (region.centre().lat - want.y()).abs() < 1e-3
            && (region.centre().lon - want.x()).abs() < 1e-3,
        "the box is centred on {:?}, not on the ground the press was over ({want:?}) \
         - the anchor was taken as a pixel rather than as ground",
        region.centre(),
    );
    assert_eq!(
        h.volume_source_pane(target),
        Some(0),
        "the 3D pane does not record the map it was aimed from, so a second drag on \
         that map cannot re-aim it and its box cannot be drawn back on it",
    );
    assert!(
        !h.region_pick_armed(),
        "the mode stayed armed after producing a box, so the user's next pan is a \
         second box",
    );
}

/// **The user's own acceptance test, on a region they dragged out.**
///
/// The defect was reported with two screenshots of one session, and what made
/// them a bug report rather than an impression was the caption: `802 x 490 km
/// box` as opened, `668 x 408 km box` after a zoom. So the caption is what this
/// reads — through the painter and the real text the pane puts on screen, not
/// the field behind it.
///
/// The sibling test in `volume_arm_tests` asserts this for a region a fixture
/// wrote directly into the pane. This one asserts it for a region that arrived
/// the way a user's does, through the arm, the press, the drag and the release
/// — which is the path a selector adds and therefore the path on which a second
/// writer of the region could hide.
///
/// The box figure must be **character-for-character identical** before and
/// after. Anything else is the reported bug, whatever the numbers happen to be.
#[test]
fn the_captions_box_figure_is_identical_before_and_after_a_zoom_for_a_dragged_region() {
    let (mut h, _painter) = pick_harness();
    h.set_region_pick_armed(true);
    h.warm_up();

    let (centre, corner) = centre_and_corner(&h, 0);
    h.drag_region(centre, corner);
    let target = (0..h.pane_rects().len())
        .find(|&idx| h.volume_region(idx).is_some())
        .expect("the drag must have landed somewhere");
    h.frames_for(4, FRAME_DT);

    let picked = h.volume_region(target).expect("a picked region");
    let before = box_line(&h, target);
    assert!(
        before.contains("km box"),
        "precondition: the pane must be captioning a box: {before}",
    );

    let rect = h.pane_rects()[target];
    for _ in 0..6 {
        h.scroll_at(rect.center(), egui::vec2(0.0, 200.0));
        h.frames_for(2, FRAME_DT);
    }

    assert_eq!(
        h.gui_mut()
            .pane(target)
            .expect("a pane")
            .volume()
            .expect("a 3D pane")
            .camera
            .eye_distance(),
        crate::pane::MIN_EYE_DISTANCE,
        "precondition: six notches must have driven the eye all the way to its stop, \
         or an identical caption could mean the gesture does nothing",
    );
    assert_eq!(
        h.volume_region(target),
        Some(picked),
        "zooming re-cut the box the user dragged out - this is the reported defect, \
         reached through the selector instead of through the derivation",
    );
    assert_eq!(
        before,
        box_line(&h, target),
        "the caption's box figure changed across a zoom - this is the defect exactly \
         as reported, and the two strings are the two screenshots",
    );
}

/// **The defect that killed this feature the first time, and why it stays
/// killed.**
///
/// The region drag was removed in part because the box and the floor under it
/// were sized independently: `floor_hit` clips the floor to the box's bottom
/// face and `floor_colour` clips it to the mirror's `0..1` — transparent
/// outside, not clamped — so a strip that fell short of the box left the volume
/// standing on nothing along that side.
///
/// It is answered by framing the strip **on the region**, which makes the two
/// rectangles the same one by construction. That is easy to believe for the
/// default box, which is centred on the site like everything else in the pane.
/// A picked region is the case that would catch a framing that had gone back to
/// reading the site: it is centred on ground the user chose, which is
/// deliberately *not* where the radar is.
///
/// So both halves are asserted: the strip is centred on the region's own
/// centre, and it covers the region's own extent on both axes — measured with
/// `ground_half_extent`, the same instrument `ui_region::tests`' coverage sweep
/// uses, so the two cannot pass while disagreeing about what "covers" means.
#[test]
fn the_floor_is_framed_on_a_dragged_region_rather_than_on_the_site() {
    let (mut h, _painter) = pick_harness();
    h.set_region_pick_armed(true);
    h.warm_up();

    let (centre, corner) = centre_and_corner(&h, 0);
    // Off the pane's centre, so the box's centre is somewhere the site is not.
    // A box centred on the site would pass this test through the fallback.
    let centre = centre + egui::vec2(-90.0, -60.0);
    let corner = corner + egui::vec2(-90.0, -60.0);
    h.drag_region(centre, corner);
    let target = (0..h.pane_rects().len())
        .find(|&idx| h.volume_region(idx).is_some())
        .expect("the drag must have landed somewhere");
    h.frames_for(4, FRAME_DT);

    let region = h.volume_region(target).expect("a picked region");
    // Deliberately nowhere near the picked box. `floor_frame_for` takes the
    // site for the *unpicked* case, where the box is the volume's reach centred
    // on the radar; a picked region states its own centre and the site must not
    // reach the answer at all. Handing it the Gulf of Guinea is what makes a
    // framing that read the site fail loudly here instead of passing because
    // the fixture put the radar near the box.
    let site = walkers::lat_lon(0.0, 0.0);

    let strip = h.pane_rects()[target];
    let memory = h.gui_mut().pane(target).expect("a pane").map_memory.clone();
    let frame = floor_frame_for(
        h.gui_mut().pane(target).expect("a pane"),
        target,
        None,
        site,
        Some(strip),
        &memory,
    );

    assert!(
        (frame.centre.y() - region.centre().lat).abs() < 1e-6
            && (frame.centre.x() - region.centre().lon).abs() < 1e-6,
        "the floor strip is centred on {:?}, not on the picked box's centre {:?} - \
         the mirror is sampled in box space, so an off-centre strip is a volume \
         standing on the wrong ground",
        frame.centre,
        region.centre(),
    );

    let covered = crate::ui_region::ground_half_extent(strip, &frame.memory, frame.centre)
        .expect("a framed viewport must be measurable");
    let want = region.half_extent_km();
    assert!(
        covered.east_km >= want.east_km && covered.north_km >= want.north_km,
        "the strip framed on a picked {want:?} box covers only {covered:?} - the \
         volume stands on transparency out there, which is the defect that removed \
         this feature the first time",
    );
}

/// The floor's framing does not depend on the camera — including at a shallow
/// pitch, where the box's far edge is well inside the frustum and on screen.
///
/// `floor_colour` works in **box space**: it maps the box's own unit square
/// through the mirror and nothing in the uniform says where the camera is. So
/// sizing the strip to "the part on screen" is not expressible, and the strip
/// is framed on the whole box at every pose. This is what says so, rather than
/// leaving it to be believed off the prose — a framing that started reading the
/// camera would break the moment a pane was orbited.
///
/// The shallow end is the interesting one. The frustum overshoots the box at
/// every reachable pitch, and below 20° — half the 40° vertical field of view —
/// the far ray never meets the ground at all, so a shallow pitch is where a
/// camera-aware framing would have the most to "save" and the most to get
/// wrong.
#[test]
fn the_floors_framing_is_the_same_at_every_camera_pose() {
    let (mut h, _painter) = pick_harness();
    h.set_region_pick_armed(true);
    h.warm_up();

    let (centre, corner) = centre_and_corner(&h, 0);
    h.drag_region(centre, corner);
    let target = (0..h.pane_rects().len())
        .find(|&idx| h.volume_region(idx).is_some())
        .expect("the drag must have landed somewhere");
    h.frames_for(4, FRAME_DT);

    // Elsewhere on purpose, for the reason the test above gives.
    let site = walkers::lat_lon(0.0, 0.0);
    let strip = h.pane_rects()[target];

    let framing = |h: &mut InputHarness| {
        let memory = h.gui_mut().pane(target).expect("a pane").map_memory.clone();
        let frame = floor_frame_for(
            h.gui_mut().pane(target).expect("a pane"),
            target,
            None,
            site,
            Some(strip),
            &memory,
        );
        (frame.centre, frame.memory.zoom())
    };

    let before = framing(&mut h);
    for (pitch, yaw, distance) in [
        // Shallower than half the 40 degree vertical field of view, so the far
        // ray never meets the ground and the box's edge is unambiguously on
        // screen - the pose a camera-aware framing would have most to "save".
        (5.0_f32, 137.0_f32, 0.05_f32),
        // The stop, 89 degrees: `restore` clamps to it, so this is the real
        // steepest pose rather than a number chosen to be inside the range.
        (89.0, -40.0, 8.0),
        (25.0, 0.0, 1.9),
    ] {
        pose(&mut h, target, yaw, pitch, distance);
        h.frames_for(2, FRAME_DT);
        assert_eq!(
            framing(&mut h),
            before,
            "the floor's framing moved when the camera did (pitch {pitch}, yaw {yaw}, \
             standoff {distance}) - the mirror is sampled in box space, so a framing \
             that follows the eye is a floor that stops covering the box",
        );
    }
}

/// A second drag on the same map **re-aims the pane that map already feeds**,
/// rather than opening another one.
///
/// The common case after the first drag is adjusting the box, not wanting a
/// second view of it — and three adjustments filling the screen with panes is
/// how a feature becomes something users avoid. This is the target rule's first
/// step, and the whole reason a 3D pane records its source.
#[test]
fn a_second_drag_on_the_same_map_re_aims_the_pane_it_already_feeds() {
    let (mut h, _painter) = pick_harness();
    h.set_region_pick_armed(true);
    h.warm_up();

    let (centre, corner) = centre_and_corner(&h, 0);
    h.drag_region(centre, corner);
    let first = (0..h.pane_rects().len())
        .find(|&idx| h.volume_region(idx).is_some())
        .expect("the first drag must land somewhere");
    let panes_after_first = h.pane_rects().len();
    let first_region = h.volume_region(first).expect("a picked region");

    h.set_region_pick_armed(true);
    h.warm_up();
    let (centre, corner) = centre_and_corner(&h, 0);
    h.drag_region(centre, corner + egui::vec2(-60.0, 0.0));

    assert_eq!(
        h.pane_rects().len(),
        panes_after_first,
        "a second drag grew the layout again - adjusting a box must not cost a pane",
    );
    let second_region = h.volume_region(first).expect("the same pane, re-aimed");
    assert_ne!(
        second_region, first_region,
        "the second drag did not re-aim the pane it was supposed to - either it \
         landed elsewhere or it did nothing",
    );
    assert_eq!(
        (0..h.pane_rects().len())
            .filter(|&idx| h.volume_region(idx).is_some())
            .count(),
        1,
        "a second 3D pane was aimed - the first drag's pane was supposed to be \
         re-aimed in place",
    );
}

/// A committed box is drawn **on the map it was picked on, and on no other**.
///
/// A 3D pane resamples a patch of ground that is invisible from the map it came
/// from, and the 3D view's own caption gives a size rather than a place — so
/// this outline is the only on-screen answer to "where is that volume from".
/// The "and no other" half matters as much: a box drawn on every map would be a
/// box that says nothing about which one aimed it.
#[test]
fn a_committed_box_is_drawn_on_the_map_it_was_picked_on() {
    let (mut h, _painter) = pick_harness();
    h.set_pane_count(3);
    h.frames_for(2, FRAME_DT);
    h.set_region_pick_armed(true);
    h.warm_up();

    let (centre, corner) = centre_and_corner(&h, 0);
    h.drag_region(centre, corner);
    let target = (0..h.pane_rects().len())
        .find(|&idx| h.volume_region(idx).is_some())
        .expect("the drag must land somewhere");
    h.frames_for(2, FRAME_DT);

    let drawn: Vec<_> = h.gui_mut().region_boxes_for_test().to_vec();
    assert_eq!(
        drawn.iter().map(|(map, _, _)| *map).collect::<Vec<_>>(),
        vec![0],
        "the committed box was drawn on {:?} - it belongs on pane 0, the map it was \
         picked on, and on nothing else",
        drawn.iter().map(|(map, _, _)| *map).collect::<Vec<_>>(),
    );
    assert_eq!(
        drawn[0].1, target,
        "the box drawn on the map names the wrong 3D pane",
    );
    assert!(
        drawn[0].2.width() > 1.0 && drawn[0].2.height() > 1.0,
        "the box was drawn as a degenerate rect {:?}, which is a dot rather than an \
         outline",
        drawn[0].2,
    );
}

/// **One press cannot be two gestures**: arming either modal drag disarms the
/// other, from every route into them.
///
/// They share one detector, so a frame with both armed would hand one press to
/// two interpreters — a line and a box out of the same drag. The exclusion
/// lives in the two setters rather than at the call sites, which is what makes
/// it hold for the top bar, the dropdown and the phone sheet alike.
#[test]
fn arming_either_modal_drag_disarms_the_other() {
    let (mut h, _painter) = pick_harness();

    h.set_section_draw_armed(true);
    h.set_region_pick_armed(true);
    assert!(h.region_pick_armed(), "the region pick did not arm");
    assert!(
        !h.section_draw_armed(),
        "the cross-section draw survived the region pick arming - one press would be \
         both a line and a box",
    );

    h.set_section_draw_armed(true);
    assert!(h.section_draw_armed(), "the section draw did not arm");
    assert!(
        !h.region_pick_armed(),
        "the region pick survived the section draw arming",
    );
}

/// The back-out gesture cancels the armed mode, and drops a half-dragged box
/// with it.
///
/// A mode's classic failure is the user forgetting they are in it and then not
/// working out why the map will not pan. `dismiss_top_layer` is the answer that
/// costs nothing to discover: it is what Escape and Android's back button both
/// resolve to, and they mean "back out" everywhere else in this application.
///
/// Called directly rather than through a synthesised key press, which is this
/// suite's convention for it: the key and the back button are the *frontend's*
/// two routes into this one method, so a test that pressed a key would be
/// testing winit's binding rather than the dismissal order.
#[test]
fn the_back_out_gesture_cancels_the_armed_region_pick() {
    let (mut h, _painter) = pick_harness();
    h.set_region_pick_armed(true);
    h.warm_up();

    let (centre, corner) = centre_and_corner(&h, 0);
    h.mouse_move(centre);
    h.frame();
    h.mouse_press(centre);
    h.frame();
    h.mouse_move(corner);
    h.frame();

    assert!(
        h.gui_mut().dismiss_top_layer(),
        "the armed mode was not the top layer, so a back press would have fallen \
         through it to whatever is under - on Android that is leaving the app",
    );
    h.frames_for(2, FRAME_DT);

    assert!(
        !h.region_pick_armed(),
        "the back-out gesture did not disarm the region pick",
    );
    assert!(
        (0..h.pane_rects().len()).all(|idx| h.volume_region(idx).is_none()),
        "a cancelled drag still committed a box",
    );

    // And the abandoned drag is gone rather than waiting: releasing now must
    // not commit the box the user just cancelled. This is the half a bare
    // `region_pick_armed = false` would fail - the flag would be down and the
    // drag would still be sitting there, ready for the release frame.
    h.mouse_release(corner);
    h.frames_for(2, FRAME_DT);
    assert!(
        (0..h.pane_rects().len()).all(|idx| h.volume_region(idx).is_none()),
        "the release after a back-out committed the cancelled box - a cancel that \
         only un-ticks the checkbox leaves the gesture live",
    );
}

/// A drag too small to be honoured is **discarded, and the mode stays armed**.
///
/// A stray tap is the likeliest thing to happen right after arming — it is how
/// a user checks which pane they are on — and both wrong answers cost the user
/// something they just said. Committing it would resample a box `build_voxels`
/// widens behind their back; disarming would throw away an intent expressed one
/// press ago and leave them to work out from nothing that the toggle had
/// un-ticked itself.
#[test]
fn a_too_small_drag_is_discarded_and_the_mode_stays_armed() {
    let (mut h, _painter) = pick_harness();
    h.set_region_pick_armed(true);
    h.warm_up();

    let centre = h.pane_rects()[0].center();
    // Two points apart, so this is a *drag* rather than a click, and still far
    // under the resampler's 10 km of half-width at the pane's opening zoom.
    h.drag_region(centre, centre + egui::vec2(2.0, 0.0));

    assert!(
        (0..h.pane_rects().len()).all(|idx| h.volume_region(idx).is_none()),
        "a box under the resampler's minimum committed - `build_voxels` would have \
         widened it and the pane's own resolution readout would describe the wrong \
         picture",
    );
    assert!(
        h.region_pick_armed(),
        "a discarded mis-drag disarmed the mode, throwing away an intent the user \
         expressed one press ago",
    );
}

/// A drag on a pane that is **already showing its volume** picks nothing.
///
/// The box is aimed with a projector, and the only projector a 3D pane has is
/// the off-screen floor strip — which is not where the user's finger is. So the
/// armed resolver never runs for one, which also means its pan is not
/// suppressed and its click is not eaten for a gesture that could never be
/// made. `PaneState::is_map` is the question that answers this, and it is not
/// the pane's *kind*: a 3D pane's kind is `Map`.
#[test]
fn a_drag_on_a_pane_drawing_its_volume_picks_nothing() {
    let (mut h, _painter) = pick_harness();
    h.make_pane_volume(1);
    h.frames_for(2, FRAME_DT);
    h.set_region_pick_armed(true);
    h.warm_up();

    let before = h.volume_region(1);
    let (centre, corner) = centre_and_corner(&h, 1);
    h.drag_region(centre, corner);

    assert_eq!(
        h.volume_region(1),
        before,
        "a drag on a pane drawing its volume picked a region - there is no projector \
         under the pointer there to have aimed it with",
    );
}

/// **The way out of a picked region**, and it leaves the camera alone.
///
/// A picked region is deliberately immovable — no zoom, pan, divider drag or
/// resize touches it — so nothing else in the pane can widen the box again.
/// Without a clear, picking one would be a one-way door. `None` rather than a
/// wide number, because `None` is a different *rule*: the volume's own reach,
/// resolved by the resampler from the scan in hand.
///
/// The camera surviving is the point of having this beside Reset view rather
/// than only inside it: a user who tightened onto a storm that turned out to be
/// nothing wants the ring back and wants to keep the angle they spent a minute
/// finding.
///
/// Driven through the **real button** rather than by writing the fields, which
/// is what makes it a test of the affordance rather than of the assignment. A
/// clear that works and has no control is not a way out of anything, and the
/// row is only drawn when there is a region to drop — so its presence is
/// asserted first, or a missing button would pass this by the click landing on
/// nothing.
#[test]
fn clearing_a_region_returns_the_whole_ring_and_keeps_the_camera() {
    let (mut h, _painter) = pick_harness();
    h.set_region_pick_armed(true);
    h.warm_up();
    let (centre, corner) = centre_and_corner(&h, 0);
    h.drag_region(centre, corner);
    let target = (0..h.pane_rects().len())
        .find(|&idx| h.volume_region(idx).is_some())
        .expect("the drag must land somewhere");

    pose(&mut h, target, 115.0, 52.0, 2.5);
    let posed = h
        .gui_mut()
        .pane(target)
        .expect("a pane")
        .volume()
        .expect("a 3D pane")
        .camera;

    // The sidebar is about the active pane, and the 3D pane is the one whose
    // region this drops. Activated by a click on the pane itself, the user's
    // own route.
    h.mouse_click(h.pane_rects()[target].center());
    h.warm_up();
    h.open_pane_props();

    let button = h
        .painted_text_rects()
        .into_iter()
        .find(|(_, text)| text == WHOLE_RING_LABEL)
        .map(|(rect, _)| rect)
        .expect(
            "a pane holding a picked region must offer a way back to the whole ring - \
             a selection that cannot be undone is a trap",
        );
    h.mouse_click(button.center());
    h.frames_for(2, FRAME_DT);

    assert_eq!(
        h.volume_region(target),
        None,
        "the cleared pane still holds a box - `None` is the volume's own reach, and \
         it is the only way back to the whole ring",
    );
    assert!(
        !h.painted_text_rects()
            .iter()
            .any(|(_, text)| text == WHOLE_RING_LABEL),
        "the Whole ring row is still drawn for a pane that has no region to drop",
    );
    assert_eq!(
        h.gui_mut()
            .pane(target)
            .expect("a pane")
            .volume()
            .expect("a 3D pane")
            .camera,
        posed,
        "clearing the region moved the camera - the whole reason this is not Reset \
         view is that it must not",
    );
    assert!(
        h.gui_mut().region_boxes_for_test().is_empty(),
        "a box is still drawn on the source map for a region the pane no longer holds",
    );
}

/// **Reset view returns the region as well as the camera**, which is what its
/// own hover text has always promised.
///
/// The button is the pane's one answer for a view that is lost — panned off the
/// box, spun to a strange angle, tightened onto a region that turned out to be
/// empty — and leaving the region out is the easy mistake. The symptom is a
/// reset that visibly does something and leaves the pane still looking at the
/// wrong ground.
#[test]
fn reset_view_returns_the_region_as_well_as_the_camera() {
    let mut volume = crate::pane::VolumePane {
        region: crate::pane::VolumeRegion::new(
            rustdar_geo::GeoPoint {
                lat: 35.33,
                lon: -97.28,
            },
            rustdar_radar::voxel::HalfExtentKm::square(40.0),
        ),
        source_pane: Some(0),
        ..Default::default()
    };
    volume.camera = crate::pane::OrbitCamera::restore(
        115.0,
        70.0,
        2.5,
        volume.camera.pivot(),
        volume.camera.vertical_exaggeration(),
    )
    .expect("finite angles");

    reset_volume_view(&mut volume);

    assert_eq!(
        volume.region, None,
        "Reset view left the picked region in place, so a pane tightened onto empty \
         ground has no button that returns it",
    );
    assert_eq!(
        volume.source_pane, None,
        "Reset view left the source index, so the cleared pane is still the preferred \
         target of the next drag on that map",
    );
    assert_eq!(
        volume.camera,
        crate::pane::OrbitCamera::default(),
        "Reset view did not return the camera",
    );
}

/// The drag's hint states the resolution the box buys, and states it
/// **truthfully**.
///
/// The resolution is the whole reason to pick a region — the grid's cell count
/// is fixed, so a tighter box spends the same cells over less ground — and it
/// is invisible unless it is said. The figures are the shipped grid's own: 512
/// cells across a 230 km box is 0.45 km per cell, and across the 920.25 km ring
/// it is 1.80.
///
/// Through `region_hint_text` rather than through a restatement of the
/// division, because the property is that the hint routes through
/// `VolumeRegion` — which clamps — rather than dividing the raw drag. A hint
/// reading 4 km over a box that will become 10 km is a hint that lies.
#[test]
fn the_drags_hint_states_the_resolution_the_box_buys() {
    assert_eq!(
        region_hint_text(115.0, Some(512)).as_deref(),
        Some("230 km - 0.45 km/cell"),
        "a 230 km box over 512 cells is 0.45 km per cell",
    );
    assert_eq!(
        region_hint_text(50.0, Some(512)).as_deref(),
        Some("100 km - 0.20 km/cell"),
        "a 100 km box over 512 cells is 0.20 km per cell",
    );
    assert_eq!(
        region_hint_text(460.125, Some(512)).as_deref(),
        Some("920 km - 1.80 km/cell"),
        "the whole ring over 512 cells is 1.80 km per cell - the figure a picked \
         region is buying its way down from",
    );

    // Clamped, not divided raw: a box under the resampler's floor is described
    // as the box that will actually be built.
    let min = rustdar_radar::voxel::MIN_HALF_WIDTH_KM;
    assert_eq!(
        region_hint_text(1.0, Some(512)),
        region_hint_text(min, Some(512)),
        "a box under the minimum was described at its drawn size rather than at the \
         size `build_voxels` would widen it to",
    );

    // No grid built yet: the box alone, rather than a confident km-per-cell out
    // of a compile-time shape this device may not have.
    assert_eq!(
        region_hint_text(115.0, None).as_deref(),
        Some("230 km"),
        "the hint invented a cell count before any grid existed to read one off",
    );
}
