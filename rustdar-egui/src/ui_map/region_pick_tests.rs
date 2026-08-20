//! The armed 3D region pick, driven through the real UI.

use super::*;
use crate::input_harness::InputHarness;
use crate::volume_view::StubVolumePainter;
use std::sync::Arc;

const FRAME_DT: f64 = 1.0 / 60.0;

/// A harness with two map panes, both plan views, a scan loaded and a painting
/// volume painter installed.
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
fn centre_and_corner(h: &InputHarness, idx: usize) -> (egui::Pos2, egui::Pos2) {
    let rect = h.pane_rects()[idx];
    (
        rect.center(),
        rect.center() + egui::vec2(rect.width() * 0.25, 0.0),
    )
}

/// Pose pane `idx`'s camera at `(yaw, pitch, standoff)`.
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
#[test]
fn the_floor_is_framed_on_a_dragged_region_rather_than_on_the_site() {
    let (mut h, _painter) = pick_harness();
    h.set_region_pick_armed(true);
    h.warm_up();

    let (centre, corner) = centre_and_corner(&h, 0);
    let centre = centre + egui::vec2(-90.0, -60.0);
    let corner = corner + egui::vec2(-90.0, -60.0);
    h.drag_region(centre, corner);
    let target = (0..h.pane_rects().len())
        .find(|&idx| h.volume_region(idx).is_some())
        .expect("the drag must have landed somewhere");
    h.frames_for(4, FRAME_DT);

    let region = h.volume_region(target).expect("a picked region");
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
        (5.0_f32, 137.0_f32, 0.05_f32),
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

    h.mouse_release(corner);
    h.frames_for(2, FRAME_DT);
    assert!(
        (0..h.pane_rects().len()).all(|idx| h.volume_region(idx).is_none()),
        "the release after a back-out committed the cancelled box - a cancel that \
         only un-ticks the checkbox leaves the gesture live",
    );
}

/// A drag too small to be honoured is **discarded, and the mode stays armed**.
#[test]
fn a_too_small_drag_is_discarded_and_the_mode_stays_armed() {
    let (mut h, _painter) = pick_harness();
    h.set_region_pick_armed(true);
    h.warm_up();

    let centre = h.pane_rects()[0].center();
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

    let min = rustdar_radar::voxel::MIN_HALF_WIDTH_KM;
    assert_eq!(
        region_hint_text(1.0, Some(512)),
        region_hint_text(min, Some(512)),
        "a box under the minimum was described at its drawn size rather than at the \
         size `build_voxels` would widen it to",
    );

    assert_eq!(
        region_hint_text(115.0, None).as_deref(),
        Some("230 km"),
        "the hint invented a cell count before any grid existed to read one off",
    );
}
