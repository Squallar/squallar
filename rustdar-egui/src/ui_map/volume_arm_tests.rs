use super::*;
use crate::input_harness::InputHarness;
use crate::volume_view::{StubVolumePainter, VolumeFrameState};
use std::sync::Arc;

const FRAME_DT: f64 = 1.0 / 60.0;

/// A harness with one map pane and one 3D pane, a scan loaded, and the given
/// painter installed. Returns the painter so a test can read back what it
/// was asked.
fn volume_harness(painter: StubVolumePainter) -> (InputHarness, Arc<StubVolumePainter>) {
    let painter = Arc::new(painter);
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    h.set_pane_count(2);
    h.make_pane_volume(1);
    h.load_scan("KTLX");
    h.gui_mut().set_volume_painter(Some(painter.clone()));
    h.frames_for(2, FRAME_DT);
    (h, painter)
}

/// The last frame the painter was asked about.
fn last_seen(painter: &StubVolumePainter) -> VolumeFrameState {
    painter
        .seen
        .lock()
        .expect("stub painter mutex")
        .last()
        .cloned()
        .expect("the painter was never asked to paint")
}

fn camera_of(h: &mut InputHarness, idx: usize) -> crate::pane::OrbitCamera {
    h.gui_mut()
        .pane_mut(idx)
        .expect("a pane")
        .volume()
        .expect("a 3D pane")
        .camera
}

/// A 3D pane with a painter and a volume pushes a callback rather than an
/// empty state.
///
/// The baseline the rest of this suite is measured against: every other test
/// here asserts that some condition *stops* this happening, and would pass
/// vacuously if the happy path never worked.
#[test]
fn a_volume_pane_with_a_painter_pushes_a_callback() {
    let (h, _painter) = volume_harness(StubVolumePainter::painting());
    assert_eq!(
        h.volume_arms(),
        vec![VolumeArmProbe {
            pane_idx: 1,
            outcome: None,
        }],
        "the 3D arm should have painted, not explained itself",
    );
}

/// Every headless machine, every suspend and every surface loss lands here,
/// so it is the ordinary state rather than the exceptional one.
#[test]
fn a_volume_pane_with_no_painter_says_it_is_unavailable() {
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    h.set_pane_count(2);
    h.make_pane_volume(1);
    h.load_scan("KTLX");
    h.frames_for(2, FRAME_DT);
    assert_eq!(
        h.volume_arms(),
        vec![VolumeArmProbe {
            pane_idx: 1,
            outcome: Some(VOLUME_EMPTY_STATE.to_owned()),
        }],
    );
}

/// `clear_graphics_state` is the suspend and surface-loss path, and it must
/// take the painter with it: every wgpu handle the painter can reach was
/// made by the device that is going away.
#[test]
fn losing_the_graphics_state_stops_the_pane_drawing() {
    let (mut h, _painter) = volume_harness(StubVolumePainter::painting());
    assert_eq!(
        h.volume_arms()[0].outcome,
        None,
        "precondition: it was drawing",
    );

    h.gui_mut().clear_graphics_state();
    h.frames_for(2, FRAME_DT);
    assert_eq!(
        h.volume_arms()[0].outcome.as_deref(),
        Some(VOLUME_EMPTY_STATE),
        "a painter holding handles from a dead device must not be asked again",
    );
}

/// A pane on a site with no volume at all says the first download is in
/// flight, naming the site.
///
/// This is the cold-start state — a site switch fires the archive fetch
/// immediately, so "downloading" is the truth — and the only state left in
/// which a 3D pane waits at all.
#[test]
fn a_volume_pane_with_no_scan_names_the_site_it_is_waiting_for() {
    let painter = Arc::new(StubVolumePainter::painting());
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    h.set_pane_count(2);
    h.make_pane_volume(1);
    h.gui_mut().set_volume_painter(Some(painter.clone()));
    h.frames_for(2, FRAME_DT);

    let outcome = h.volume_arms()[0].outcome.clone().expect("an empty state");
    assert!(
        outcome.contains("Downloading the first") && outcome.contains("volume"),
        "expected the cold-start download message, got {outcome:?}",
    );
    assert!(
        painter.seen.lock().unwrap().is_empty(),
        "the painter must not be asked for a volume that has not arrived",
    );
}

/// **The pane builds only from the published stamp, never from the plan
/// view's `scan_info`.**
///
/// The pane has a `scan_info` — the plan view beside it is drawing a
/// perfectly good volume — and no published current-volume stamp. The pane
/// must wait rather than build, because the stamp is the App's statement
/// that it holds a volume worth building and the App has made none.
///
/// The mutation this closes is the obvious simplification: keying the
/// target off `pane.scan_info`, which is what the code did long ago and
/// which makes every other volume test pass.
#[test]
fn a_pane_with_no_published_stamp_does_not_build_from_the_plan_views_scan() {
    let painter = Arc::new(StubVolumePainter::painting());
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    h.set_pane_count(2);
    h.make_pane_volume(1);
    h.gui_mut().set_volume_painter(Some(painter.clone()));
    h.load_scan("KTLX");
    // The plan view's volume is on screen; the App has published no stamp
    // for this site. `load_scan` fills both halves — it stands in for a
    // volume arrival — so this is what takes them apart again.
    h.set_current_volume("KTLX", None);
    // Everything the painter saw belongs to the stamp `load_scan`
    // published. The assertion below is about what happens *after* it is
    // withdrawn, so the record starts here.
    painter.seen.lock().unwrap().clear();
    h.frames_for(2, FRAME_DT);

    assert!(
        h.gui_mut().pane(1).expect("pane 1").scan_info.is_some(),
        "precondition: the plan view has a volume",
    );
    let outcome = h.volume_arms()[0].outcome.clone().expect("an empty state");
    assert!(
        outcome.contains("Downloading the first"),
        "a pane with a plan-view volume and no published stamp must wait, got {outcome:?}",
    );
    assert!(
        painter.seen.lock().unwrap().is_empty(),
        "no grid may be asked for on the strength of the plan view's scan",
    );
    assert!(
        !h.last_actions()
            .iter()
            .any(|a| matches!(a, GuiAction::PrepareVolume { .. })),
        "no build may be triggered by the plan view's scan arriving",
    );
}

/// **While it follows live**, the pane names the published stamp, not the
/// plan view's own time.
///
/// The two differ constantly: `scan_info.timestamp` is the volume's start
/// and freezes for the whole flight, while the stamp advances on every
/// sealed sweep. A target built from the wrong one would ask the host for
/// a volume it does not have.
///
/// The pane here is live, which is the whole scope of this rule — the
/// other half is [`a_pane_taken_off_live_names_the_volume_it_is_showing`],
/// and reading the stamp unconditionally is what made the timeline inert
/// over a 3D pane.
#[test]
fn the_target_names_the_published_stamp_rather_than_the_displayed_time() {
    let painter = Arc::new(StubVolumePainter::painting());
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    h.set_pane_count(2);
    h.make_pane_volume(1);
    h.gui_mut().set_volume_painter(Some(painter.clone()));
    h.load_scan("KTLX");
    let shown = h
        .gui_mut()
        .pane(1)
        .expect("pane 1")
        .scan_info
        .as_ref()
        .expect("a scan")
        .timestamp;
    // The stamp leads the volume-start time by construction: it is the
    // newest sealed sweep's own collection time.
    let stamp = shown + chrono::Duration::minutes(4);
    h.set_current_volume("KTLX", Some(stamp));
    h.frames_for(2, FRAME_DT);

    let seen = painter.seen.lock().unwrap();
    let frame = seen.last().expect("the painter was asked");
    assert_eq!(
        frame.target.volume.collected, stamp,
        "the grid must be asked for against the published stamp, not the displayed time",
    );
    assert_eq!(frame.target.volume.site, "KTLX");
}

/// **A pane taken off live names the volume it is showing.**
///
/// The report this was written from: "changing time in the time bar does
/// not change the 3d viewer render at all". The published current-volume
/// stamp is per **site** and describes what the App holds *now*, so it
/// cannot express "this pane is looking at 18:05"; a 3D pane that read it
/// unconditionally went on naming the live volume through every scrub,
/// never changed its target, and so never asked for a rebuild. The plan
/// view and the cross-section beside it both moved, because both are keyed
/// on the pane's own `scan_info.timestamp`.
///
/// Staged through the two setters the host really uses — `handle_navigate_time`
/// calls `set_viewing_live_for_pane(idx, false)` and the scan drain calls
/// `set_scan_info_for_site` with the volume that came back. The published
/// stamp is deliberately left where it was: on a chunk-fed site the feed
/// goes on sealing sweeps and the stamp never moves backwards at all,
/// which is the state the report came from.
#[test]
fn a_pane_taken_off_live_names_the_volume_it_is_showing() {
    let painter = Arc::new(StubVolumePainter::painting());
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    h.set_pane_count(2);
    h.make_pane_volume(1);
    h.gui_mut().set_volume_painter(Some(painter.clone()));
    h.load_scan("KTLX");
    let live = h
        .gui_mut()
        .pane(1)
        .expect("pane 1")
        .scan_info
        .as_ref()
        .expect("a scan")
        .timestamp;
    h.set_current_volume("KTLX", Some(live));
    h.frames_for(2, FRAME_DT);
    assert_eq!(
        last_seen(&painter).target.volume.collected,
        live,
        "precondition: the live pane names the live volume, or the scrub \
         below has nothing to move",
    );

    // The scrub. Half an hour back, which is several volumes.
    //
    // Every pane on the site, because that is what a navigation does: the
    // scan drain's `set_scan_info_for_site` writes the site's panes and
    // `propagate_layer_sync` converges `viewing_live` across the time-linked
    // group. Writing pane 1 alone would be undone by that pass before the
    // frame drew, which is not a state production can be in.
    let scrubbed = live - chrono::Duration::minutes(30);
    for idx in 0..2 {
        h.gui_mut().set_viewing_live_for_pane(idx, false);
        h.gui_mut()
            .pane_mut(idx)
            .expect("a pane")
            .scan_info
            .as_mut()
            .expect("a scan")
            .timestamp = scrubbed;
    }
    painter.seen.lock().unwrap().clear();
    h.frames_for(2, FRAME_DT);

    assert_eq!(
        last_seen(&painter).target.volume.collected,
        scrubbed,
        "the 3D pane is still aimed at the live volume after a scrub, so the \
         picture cannot change however far the timeline is dragged",
    );
    let asked: Vec<chrono::NaiveDateTime> = h
        .last_actions()
        .iter()
        .filter_map(|a| match a {
            GuiAction::PrepareVolume { target, .. } => Some(target.volume.collected),
            _ => None,
        })
        .collect();
    assert!(
        asked.contains(&scrubbed),
        "no build was asked for against the scrubbed time — the pane's target \
         did not move, so the level-triggered ask never fired. It asked for \
         {asked:?}",
    );
}

/// **The Volume Alpha curve rides the frame, and only when one exists.**
///
/// Both halves are load-bearing. An untouched editor must send `None` —
/// that is the painter's licence to upload the grid's own LUT bit-exactly,
/// and a frame that carried a synthesised default curve instead would take
/// that licence away for every user who never opened the editor. An edited
/// product must send exactly the stored curve, keyed by the *pane's*
/// product — the storm answering the drag is this one field arriving.
#[test]
fn the_alpha_curve_rides_the_frame_only_when_one_is_stored() {
    use crate::volume_alpha::{AlphaCurve, CURVE_LEN};

    let (mut h, painter) = volume_harness(StubVolumePainter::painting());
    assert_eq!(
        last_seen(&painter).alpha,
        None,
        "an untouched editor must hand the painter no curve at all",
    );

    let mut alphas = [0u8; CURVE_LEN];
    alphas[128..].fill(255);
    let curve = AlphaCurve::from_alphas(alphas);
    let product = h.gui_mut().pane(1).expect("pane 1").selected_product;
    h.gui_mut().volume_alpha.set(product, curve.clone());
    h.frames_for(1, FRAME_DT);
    assert_eq!(
        last_seen(&painter).alpha,
        Some(curve),
        "the stored curve for the pane's product must ride the frame",
    );

    h.gui_mut().volume_alpha.reset(product);
    h.frames_for(1, FRAME_DT);
    assert_eq!(
        last_seen(&painter).alpha,
        None,
        "a reset must restore the bit-exact no-curve state, not a copy of the default",
    );
}

/// The Volume Alpha button is on the 3D pane — the editor's only door.
///
/// Asserted through the painted text because that is what a user can see:
/// a button constructed but clipped, layered under the raymarch, or
/// simply never reached by `render_volume_pane` all fail here identically.
#[test]
fn the_volume_alpha_button_is_painted_on_a_3d_pane() {
    let (h, _painter) = volume_harness(StubVolumePainter::painting());
    let pane_rect = h.pane_rects()[1];
    let texts = h.painted_text_strings_in(pane_rect);
    assert!(
        texts
            .iter()
            .any(|t| t.contains(crate::ui::map::volume_alpha_editor::ALPHA_BUTTON_LABEL)),
        "the Volume alpha button must be painted inside the 3D pane; painted \
             texts were {texts:?}",
    );
}

/// A moment the radar does not measure directly is refused by name, before
/// anything asks for a grid `build_voxels` would decline to build.
#[test]
fn a_product_with_no_vertical_structure_is_refused_by_name() {
    let (mut h, painter) = volume_harness(StubVolumePainter::painting());
    // On every pane, not just the 3D one: the layer links default on and
    // `propagate_layer_sync` copies the *active* pane's product to the
    // rest, so writing it to pane 1 alone is undone on the next frame by
    // pane 0.
    for pane in h.gui_mut().panes_mut() {
        pane.selected_product = rustdar_radar::types::RadarProduct::EchoTops;
    }
    let before = painter.seen.lock().unwrap().len();
    h.frames_for(2, FRAME_DT);

    let outcome = h.volume_arms()[0].outcome.clone().expect("an empty state");
    assert!(
        outcome.contains("no vertical structure"),
        "expected the refusal to say why, got {outcome:?}",
    );
    assert_eq!(
        painter.seen.lock().unwrap().len(),
        before,
        "the painter must not be asked about a moment that cannot be sampled",
    );
}

/// A product the radar *derives* tilt by tilt is not refused by name — it
/// is asked for.
///
/// The mirror of the test above, and the second of the three UI-facing
/// gates that admit SRV, NROT and KDP to the vertical views. Until now
/// none of the three had a test: all could be reverted to
/// `sampler::samplable` — the exact pre-admission code — with every test
/// in the workspace green, and every derived pane would refuse by name
/// with the volume behind it perfectly able to render.
#[test]
fn a_derived_product_is_asked_for_rather_than_refused_by_name() {
    use rustdar_radar::types::RadarProduct;
    for product in [
        RadarProduct::StormRelativeVelocity,
        RadarProduct::NormalizedRotation,
        RadarProduct::SpecificDifferentialPhase,
    ] {
        assert!(
            rustdar_radar::sampler::samplable(product).is_none(),
            "precondition: {} has no native moment, so this is about the \
                 `volume_slot` gate and not about `samplable`",
            product.name(),
        );
        let (mut h, _painter) = volume_harness(StubVolumePainter::painting());
        // Every pane: the linked sync pass propagates the active pane's
        // product.
        for pane in h.gui_mut().panes_mut() {
            pane.selected_product = product;
        }
        h.frames_for(2, FRAME_DT);

        let outcome = h.volume_arms()[0].outcome.clone();
        assert!(
            !outcome
                .as_deref()
                .is_some_and(|o| o.contains("no vertical structure")),
            "{} is derived tilt by tilt, but the 3D pane refused it: {outcome:?}",
            product.name(),
        );
        assert!(
            h.last_actions()
                .iter()
                .any(|a| matches!(a, GuiAction::PrepareVolume { .. })),
            "{} never got a grid request, so the pane refused it silently",
            product.name(),
        );
    }
}

/// The pane asks for its grid until it has one, and stops the moment the
/// host records that it does.
///
/// Level-triggered by design — see `GuiAction::PrepareVolume` — so the half
/// worth testing is that it *stops*, which an edge-triggered implementation
/// would get right for free and a broken level-triggered one would not.
#[test]
fn a_volume_pane_asks_for_its_grid_until_the_host_says_it_has_one() {
    let (mut h, _painter) = volume_harness(StubVolumePainter::painting());

    let asked: Vec<_> = h
        .last_actions()
        .iter()
        .filter_map(|a| match a {
            GuiAction::PrepareVolume { pane_idx, target } => Some((*pane_idx, target.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(asked.len(), 1, "the pane should have asked exactly once");
    let (pane_idx, target) = asked.into_iter().next().expect("one request");
    assert_eq!(pane_idx, 1);
    assert_eq!(target.volume.site, "KTLX");

    // What the host does when the build lands.
    h.gui_mut()
        .pane_mut(1)
        .expect("pane 1")
        .volume_mut()
        .expect("a 3D pane")
        .rendered_for = Some(target);
    h.frames_for(2, FRAME_DT);

    assert!(
        !h.last_actions()
            .iter()
            .any(|a| matches!(a, GuiAction::PrepareVolume { .. })),
        "a pane that has its grid must stop asking for it",
    );
}

/// Converting a 3D pane to something else releases its volume.
///
/// The only moment a pane stops needing an 8 MiB grid without anything else
/// noticing: it is still on screen, still on the same site, still live.
#[test]
fn converting_a_volume_pane_away_releases_its_volume() {
    let (mut h, _painter) = volume_harness(StubVolumePainter::painting());
    h.gui_mut()
        .request_pane_view(1, rustdar_radar::types::RenderView::PlanView);
    h.frames_for(1, FRAME_DT);

    assert!(
        h.last_actions()
            .iter()
            .any(|a| matches!(a, GuiAction::ReleaseVolume { pane_idx: 1 })),
        "converting away from a 3D pane must release its volume, got {:?}",
        h.last_actions()
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>(),
    );
}

/// Converting a pane that was never a 3D pane releases nothing.
///
/// The mutation this closes: dropping the `kind() == Volume` half of the
/// guard leaves a `ReleaseVolume` on every conversion — harmless today, and
/// a pane releasing a volume another pane is using the moment the store is
/// keyed any other way.
#[test]
fn converting_a_map_pane_releases_nothing() {
    let (mut h, _painter) = volume_harness(StubVolumePainter::painting());
    h.gui_mut()
        .request_pane_view(0, rustdar_radar::types::RenderView::CrossSection);
    h.frames_for(1, FRAME_DT);
    assert!(
        !h.last_actions()
            .iter()
            .any(|a| matches!(a, GuiAction::ReleaseVolume { .. })),
        "a map pane has no volume to release",
    );
}

/// The painter is asked with the camera **after** this frame's drag.
///
/// The trap this closes is not a wrong picture but a *late* one: building
/// the payload before the UI pass leaves the orbit one frame behind the
/// pointer, which reads as input lag and gets "fixed" by turning the drag
/// sensitivity up.
#[test]
fn the_painter_sees_the_camera_after_this_frames_drag() {
    let (mut h, painter) = volume_harness(StubVolumePainter::painting());
    let rect = h.pane_rects()[1];

    h.mouse_press(rect.center());
    h.frames_for(1, FRAME_DT);
    h.mouse_move(rect.center() + egui::vec2(120.0, 0.0));
    h.frames_for(1, FRAME_DT);

    let moved = camera_of(&mut h, 1);
    assert_ne!(
        moved,
        crate::pane::OrbitCamera::default(),
        "precondition: the drag must have moved the camera at all",
    );
    assert_eq!(
        last_seen(&painter).camera,
        moved,
        "the painter was handed a stale camera, so the volume lags the pointer by a frame",
    );
    h.mouse_release(rect.center() + egui::vec2(120.0, 0.0));
}

/// Dragging turns the box the way the pointer went, in both axes.
///
/// Signs, not arithmetic. A sign error still orbits perfectly smoothly and
/// merely feels inverted, which is the sort of defect that survives review
/// and is reported months later as "the 3D view is backwards".
#[test]
fn dragging_turns_the_box_the_way_the_pointer_went() {
    for drag in [egui::vec2(120.0, 0.0), egui::vec2(0.0, 120.0)] {
        let (mut h, _painter) = volume_harness(StubVolumePainter::painting());
        let rect = h.pane_rects()[1];
        let before = camera_of(&mut h, 1);

        h.mouse_press(rect.center());
        h.frames_for(1, FRAME_DT);
        h.mouse_move(rect.center() + drag);
        h.frames_for(1, FRAME_DT);
        h.mouse_release(rect.center() + drag);
        h.frames_for(1, FRAME_DT);

        let after = camera_of(&mut h, 1);
        if drag.x != 0.0 {
            assert!(
                after.yaw_deg() > before.yaw_deg(),
                "dragging right should raise the eye's bearing: {} -> {}",
                before.yaw_deg(),
                after.yaw_deg(),
            );
            assert_eq!(
                after.pitch_deg(),
                before.pitch_deg(),
                "a horizontal drag must not pitch",
            );
        } else {
            assert!(
                after.pitch_deg() > before.pitch_deg(),
                "dragging down should raise the eye: {} -> {}",
                before.pitch_deg(),
                after.pitch_deg(),
            );
            assert_eq!(
                after.yaw_deg(),
                before.yaw_deg(),
                "a vertical drag must not yaw",
            );
        }
    }
}

/// Scrolling over the 3D pane zooms it; scrolling over another pane does
/// not.
///
/// `Input::zoom_delta` and the scroll delta are **global** — they report the
/// frame's gesture wherever on screen it happened — so the
/// `hovered() || dragged()` gate is correctness rather than politeness.
/// Without it a wheel over a map pane would zoom every 3D pane on screen.
#[test]
fn only_a_gesture_over_the_pane_zooms_it() {
    let (mut h, _painter) = volume_harness(StubVolumePainter::painting());
    let rects = h.pane_rects();

    let before = camera_of(&mut h, 1).eye_distance();
    h.scroll_at(rects[0].center(), egui::vec2(0.0, 200.0));
    h.frames_for(2, FRAME_DT);
    assert_eq!(
        camera_of(&mut h, 1).eye_distance(),
        before,
        "a scroll over the map pane must not move the 3D pane's camera",
    );

    h.scroll_at(rects[1].center(), egui::vec2(0.0, 200.0));
    h.frames_for(2, FRAME_DT);
    let after = camera_of(&mut h, 1).eye_distance();
    assert!(
        after < before,
        "scrolling up over the 3D pane should bring the eye in: {before} -> {after}",
    );
}

/// The painter is told the pane's size in **physical** pixels, not points.
///
/// The offscreen target is allocated from this number, so handing over
/// points on a 2x display would allocate a quarter-sized texture and blit it
/// stretched — which looks like the resolution rung working rather than like
/// a bug.
///
/// **Run at 2x deliberately.** At the harness's default scale points and
/// pixels are the same number, so an assertion that multiplies by
/// `pixels_per_point` passes whether the production code multiplies or not.
/// The first version of this test did exactly that and could not see the
/// mutation it is named for.
#[test]
fn the_painter_is_told_the_pane_size_in_physical_pixels() {
    let (mut h, painter) = volume_harness(StubVolumePainter::painting());
    h.set_pixels_per_point(2.0);
    h.frames_for(2, FRAME_DT);

    assert_eq!(
        h.pixels_per_point(),
        2.0,
        "precondition: points and pixels must differ, or this proves nothing",
    );
    let rect = h.pane_rects()[1];
    let seen = last_seen(&painter);
    assert_eq!(
        seen.size_px,
        [
            (rect.width() * 2.0).round() as u32,
            (rect.height() * 2.0).round() as u32,
        ],
        "the pane is {} x {} points, so at 2x it is twice that in pixels",
        rect.width(),
        rect.height(),
    );
    assert_eq!(seen.pane_idx, 1);
}

/// A long explanation is wrapped inside the pane, not laid out on one line
/// that runs off both edges.
///
/// Found by looking at the app rather than by reasoning: the 3D pane's
/// palette refusal is a paragraph, and `Painter::text` centres a single
/// unwrapped line — so it rendered as a strip of words with the start and
/// end of every line cut away. That reads as a rendering bug, not as an
/// explanation, which makes it worse than the empty box it replaced.
#[test]
fn a_long_empty_state_is_wrapped_inside_the_pane() {
    let long = "Velocity cannot be drawn as a volume yet. Its colour table is opaque at \
                    the bottom of its scale, so every boundary between measured and unmeasured \
                    air paints, and a volume is mostly unmeasured air.";
    let (h, _painter) = volume_harness(StubVolumePainter::empty(long));
    let pane = h.pane_rects()[1];

    let painted: Vec<_> = h
        .painted_text_rects()
        .into_iter()
        .filter(|(_, text)| text.contains("cannot be drawn"))
        .collect();
    assert_eq!(painted.len(), 1, "the refusal should be painted once");
    let (rect, _) = &painted[0];
    assert!(
        rect.width() <= pane.width(),
        "the message is {} wide in a {} pane, so it runs off both edges",
        rect.width(),
        pane.width(),
    );
    assert!(
        pane.contains_rect(*rect),
        "the message at {rect:?} is not inside its pane {pane:?}",
    );
}

/// Whatever the painter says is why the pane is empty is what the pane says.
///
/// The renderer knows things this crate cannot name — a device error latched
/// mid-session, a single-tilt volume, a grid still building — and every one
/// of them is a different thing for the user to do about it.
#[test]
fn the_painters_own_reason_reaches_the_pane() {
    let (h, _painter) = volume_harness(StubVolumePainter::empty("a very specific reason"));
    assert_eq!(
        h.volume_arms()[0].outcome.as_deref(),
        Some("a very specific reason"),
    );
}

// --- The caption: everything the pane claims about the picture ----------

/// **The height the pane reports is real at every exaggeration.**
///
/// This is the counterweight that makes the exaggeration defensible at all.
/// The stretch is a drawing convention; a stretched *number* would be a
/// fabricated measurement, and 0–59 kft MSL is a figure a forecaster would
/// read off the screen and act on.
///
/// The mutation this closes is the tempting one — multiplying the top of the
/// box by the exaggeration so the caption "matches what you see". At 3× that
/// produces "0–177 kft MSL", which is above the Kármán line and still looks
/// like a readout.
#[test]
fn the_height_the_pane_reports_is_real_at_every_exaggeration() {
    let mut seen = Vec::new();
    for ex in [1.0f32, 3.0, 12.0] {
        let mut camera = crate::pane::OrbitCamera::default();
        camera.set_vertical_exaggeration(ex);
        let lines = volume_caption("KTLX", at(33), None, None, camera);
        let height = lines
            .iter()
            .find(|l| l.contains("kft MSL"))
            .unwrap_or_else(|| panic!("no height line at {ex}x in {lines:?}"))
            .clone();
        assert!(
            height.starts_with("0-59 kft MSL"),
            "the height must be the box's true extent, not the drawn one: {height:?}",
        );
        assert!(
            height.contains(&format!("{ex:.1}×")),
            "the exaggeration must be stated beside it: {height:?}",
        );
        seen.push(height);
    }
    assert_eq!(
        seen.iter().filter(|h| h.starts_with("0-59")).count(),
        3,
        "every setting must report the same real height: {seen:?}",
    );
}

/// The caption states the merged volume's freshness truthfully: "newest
/// data" and its time in the first line, never a claim about the whole
/// volume.
///
/// The word "newest" is the load-bearing one. A merged volume's low tilts
/// can be seconds old while its top is minutes older; a first line that
/// said only "volume 22:39Z" would let the whole picture borrow the
/// freshest sweep's currency.
#[test]
fn the_caption_states_the_newest_data_time_not_a_whole_volume_claim() {
    let lines = volume_caption("KTLX", at(39), Some(at(33)), None, Default::default());
    assert!(
        lines[0].contains("KTLX") && lines[0].contains("newest data") && lines[0].contains("22:39"),
        "the first line must name the site and say the time is the newest \
             data's, not the volume's: {lines:?}",
    );
}

/// The caption names the base volume the un-refreshed tilts come from —
/// and while a site's first volume is still filling, says there is no
/// complete volume at all rather than staying quiet.
///
/// Both halves are honesty devices. Without the first, a reader cannot
/// see the merged volume's span — the newest-data line alone reads as
/// "everything is this fresh". Without the second, a ladder still filling
/// reads as a full atmosphere.
#[test]
fn the_caption_names_the_base_volume_or_says_the_first_is_still_filling() {
    let merged = volume_caption("KTLX", at(39), Some(at(33)), None, Default::default());
    let base = merged
        .iter()
        .find(|l| l.contains("base volume"))
        .unwrap_or_else(|| panic!("the base volume must be named: {merged:?}"));
    assert!(
        base.contains("22:33") && !base.contains("22:39"),
        "the base line must carry the base volume's own time: {base}",
    );

    let filling = volume_caption("KTLX", at(39), None, None, Default::default());
    assert!(
        filling.iter().any(|l| l.contains("no complete volume yet")),
        "a first volume still filling must be said out loud: {filling:?}",
    );
}

/// The caption reports the resolution the region buys, and it moves with the
/// region.
///
/// The grid's cell count is fixed, so a tighter box spends the same cells
/// over less ground — 1.80 km per cell at the whole-scan default against
/// 0.16 at 20 km. That is the main reason to pick a region, and it is
/// invisible unless it is written down.
///
/// The default's figures are pinned as literals — the full 460 km scan and
/// the 1.80 km cells it costs — rather than derived from the constant the
/// caption itself reads, so a default that drifted from covering the scan
/// fails here by name instead of being restated as correct.
#[test]
fn the_caption_reports_the_resolution_the_region_buys() {
    let wide = volume_caption("KTLX", at(33), None, None, Default::default());
    assert!(
        wide.iter()
            .any(|l| l.contains("460 km box") && l.contains("1.80 km/cell")),
        "the sourceless default must report the whole scan and its cost: {wide:?}",
    );

    let tight = crate::pane::VolumeRegion::new(
        crate::pane::GeoPoint {
            lat: 35.3,
            lon: -97.3,
        },
        20.0,
    )
    .expect("a valid region");
    let tight_lines = volume_caption("KTLX", at(33), None, Some(tight), Default::default());
    let line = tight_lines
        .iter()
        .find(|l| l.contains("km box"))
        .expect("a box line");
    assert!(
        line.contains("40 km box"),
        "a 20 km half-width is a 40 km box: {line:?}",
    );
    // The whole point of the feature: a quarter of the width is four times
    // the resolution, and both figures are on screen.
    let cells = rustdar_radar::voxel::default_shape().nx as f64;
    assert!(
        line.contains(&format!("{:.2} km/cell", 40.0 / cells)),
        "the tighter box must report its finer cells: {line:?}",
    );
}

// --- The pan gesture ----------------------------------------------------

/// A secondary drag pans and does not orbit; a primary drag orbits and does
/// not pan.
///
/// The two are separate verbs on separate buttons, and a mutation that made
/// either drag do both would still move the picture — plausibly — while
/// making the other gesture impossible to perform cleanly.
#[test]
fn the_secondary_drag_pans_and_the_primary_drag_orbits() {
    let mut h = volume_pane_harness();
    let rect = h.pane_rects()[1];
    let before = camera_of(&mut h, 1);

    h.mouse_press_secondary(rect.center());
    h.frames_for(1, FRAME_DT);
    h.mouse_move(rect.center() + egui::vec2(90.0, 0.0));
    h.frames_for(1, FRAME_DT);
    h.mouse_release_secondary(rect.center() + egui::vec2(90.0, 0.0));
    h.frames_for(1, FRAME_DT);

    let panned = camera_of(&mut h, 1);
    assert_ne!(panned.pivot(), before.pivot(), "a secondary drag must pan");
    assert_eq!(
        (panned.yaw_deg(), panned.pitch_deg()),
        (before.yaw_deg(), before.pitch_deg()),
        "a secondary drag must not orbit",
    );

    let before = panned;
    h.mouse_press(rect.center());
    h.frames_for(1, FRAME_DT);
    h.mouse_move(rect.center() + egui::vec2(90.0, 0.0));
    h.frames_for(1, FRAME_DT);
    h.mouse_release(rect.center() + egui::vec2(90.0, 0.0));
    h.frames_for(1, FRAME_DT);

    let orbited = camera_of(&mut h, 1);
    assert_ne!(
        orbited.yaw_deg(),
        before.yaw_deg(),
        "a primary drag must orbit"
    );
    assert_eq!(
        orbited.pivot(),
        before.pivot(),
        "a primary drag must not pan",
    );
}

/// The box travels the way the pointer went.
///
/// Through the whole shipped path rather than through `pan_for_drag` alone,
/// so a sign inverted between the two — the gesture reading the drag one way
/// and the maths another — cannot hide.
#[test]
fn a_secondary_drag_carries_the_box_the_way_the_pointer_went() {
    let mut h = volume_pane_harness();
    let rect = h.pane_rects()[1];
    // Due south of the box looking north, so screen-right is due east and the
    // axis the pivot moves on is nameable.
    {
        let camera = &mut h
            .gui_mut()
            .pane_mut(1)
            .expect("pane 1")
            .volume_mut()
            .expect("a 3D pane")
            .camera;
        *camera =
            crate::pane::OrbitCamera::restore(180.0, 0.0, 2.5, [0.0; 3], 1.0).expect("finite");
    }
    h.frames_for(1, FRAME_DT);

    h.mouse_press_secondary(rect.center());
    h.frames_for(1, FRAME_DT);
    h.mouse_move(rect.center() + egui::vec2(80.0, 0.0));
    h.frames_for(1, FRAME_DT);

    assert!(
        camera_of(&mut h, 1).pivot()[0] < -1e-4,
        "dragging right must aim west so the box travels east: {:?}",
        camera_of(&mut h, 1).pivot(),
    );
}

/// A pane collapsed to nothing by a divider drag does not put a NaN in the
/// camera.
///
/// The realistic path to a zero viewport height, and the consequence of
/// laundering it rather than refusing is a staleness key that never equals
/// itself — a rebuild every frame, for ever, with a hot CPU as its only
/// symptom.
#[test]
fn a_pane_with_no_height_pans_to_nothing_rather_than_to_nan() {
    let mut h = volume_pane_harness();
    let rect = h.pane_rects()[1];
    // The gesture still runs; only the geometry is degenerate.
    let pan = crate::volume_view::pan_for_drag(
        camera_of(&mut h, 1),
        [160.0, 160.0, 18.0],
        0.0,
        [rect.width(), 0.0],
    );
    assert_eq!(pan, None, "a zero-height pane must produce no pan at all");

    let mut camera = camera_of(&mut h, 1);
    camera.nudge(crate::pane::OrbitDelta {
        pan: [f32::NAN, 0.0, 0.0],
        ..Default::default()
    });
    assert!(
        camera.pivot().iter().all(|p| p.is_finite()),
        "a non-finite pan must be refused whole: {:?}",
        camera.pivot(),
    );
}

// --- Reset --------------------------------------------------------------

/// A 3D pane on a 2-pane harness, with an archive volume and a painter.
fn volume_pane_harness() -> InputHarness {
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    h.set_pane_count(2);
    h.make_pane_volume(1);
    h.gui_mut()
        .set_volume_painter(Some(Arc::new(StubVolumePainter::painting())));
    h.load_scan("KTLX");
    h.frames_for(2, FRAME_DT);
    h
}

fn at(minute: u32) -> chrono::NaiveDateTime {
    chrono::NaiveDate::from_ymd_opt(2026, 7, 30)
        .expect("a real date")
        .and_hms_opt(22, minute, 0)
        .expect("a real time")
}

/// The mirror pass's guest list is this frame's floor strips — and nothing
/// else.
///
/// The baseline for the tests below, and the first coverage
/// `mirror_source_rects` or `map_pane_geo` have ever had. The negative cases
/// would pass vacuously if the positive one did not work, because "no rects" is
/// also what a guest list that never populates says.
///
/// The strip's *position* is the load-bearing half. A guest rect inside the
/// frame would be the pane's own chrome — the volume, drawn over itself — and a
/// guest rect at the pane's own coordinates would be exactly that. Both are
/// checked, because "one rect of the right size" is true of the wrong rect too.
#[test]
fn the_mirror_guest_list_is_this_frames_floor_strips() {
    let (mut h, _painter) = volume_harness(StubVolumePainter::painting());
    let screen = h.screen_rect();
    let pane_rect = h.pane_rects()[1];

    let rects = h.gui_mut().mirror_source_rects();
    assert_eq!(
        rects.len(),
        1,
        "one 3D pane showing a floor should ask for one strip and nothing \
         else, got {rects:?}",
    );
    let strip = rects[0];
    assert!(
        strip.width() > 0.0 && strip.height() > 0.0,
        "the guest list carried a degenerate rect {strip:?}; the mirror pass \
         would clip every primitive away and the floor would be blank",
    );
    assert!(
        strip.min.y >= screen.max.y,
        "the strip {strip:?} is inside the frame (bottom {}); the map would be \
         drawn on the glass, over the volume it is the floor for",
        screen.max.y,
    );
    assert_eq!(
        strip.size(),
        pane_rect.size(),
        "the strip is the pane's own rect moved down, so it is the pane's own \
         size — a strip of another size would sample the wrong ground",
    );

    // A pane whose floor is switched off asks for nothing.
    h.gui_mut()
        .pane_mut(1)
        .expect("a pane")
        .volume_mut()
        .expect("a 3D pane")
        .hide_floor = true;
    h.frames_for(2, FRAME_DT);
    assert!(
        h.gui_mut().mirror_source_rects().is_empty(),
        "a 3D pane with its map floor turned off is still paying for a mirror \
         pass it does not read",
    );

    // And a layout with no 3D pane in it asks for nothing at all, so a frame
    // of plain maps allocates no mirror.
    h.gui_mut()
        .pane_mut(1)
        .expect("a pane")
        .volume_mut()
        .expect("a 3D pane")
        .hide_floor = false;
    h.make_pane_map(1);
    h.frames_for(2, FRAME_DT);
    assert!(
        h.gui_mut().mirror_source_rects().is_empty(),
        "a layout of plain map panes is still asking for a mirror pass",
    );
}

/// **The reported bug.** A 3D view opened from a tab — no split, no region
/// dragged on a neighbouring map — shows its floor.
///
/// This is the one-pane shape, which is what a tab is and what every phone is.
/// Under the borrowed-source arrangement it was the shape with no floor at all:
/// the pane's `source_pane` was either unset or pointed at *itself*, and either
/// way the registration was refused — correctly, because the only thing at that
/// index was the 3D pane's own chrome. The strip is what gives the pane a map
/// of its own to stand on.
///
/// Both halves are asserted. A `source` alone would be satisfied by registering
/// the pane's own rect, which is the failure the old arrangement was protecting
/// against; a strip alone would be satisfied by geometry nothing samples.
#[test]
fn a_3d_pane_with_no_neighbouring_map_still_gets_a_floor() {
    let painter = Arc::new(StubVolumePainter::painting());
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    h.set_pane_count(1);
    h.load_scan("KTLX");
    h.gui_mut().set_volume_painter(Some(painter.clone()));
    h.frames_for(2, FRAME_DT);
    assert_eq!(
        h.pane_kinds(),
        vec![rustdar_radar::types::RenderView::PlanView]
    );

    h.make_pane_volume(0);
    h.frames_for(2, FRAME_DT);

    let seen = last_seen(&painter);
    assert!(
        seen.floor,
        "the pane is not even asking for a floor, so nothing below is about \
         the floor",
    );
    let geo = seen
        .source
        .expect("a 3D pane that is its own map must be registered to it");
    let screen = h.screen_rect();
    assert!(
        geo.rect.min.y >= screen.max.y,
        "the floor is registered to {:?}, which is inside the frame — that is \
         the pane's own chrome, not a map",
        geo.rect,
    );
    assert!(
        geo.points_per_degree_lon > 0.0 && geo.points_per_mercator_y < 0.0,
        "the affine {geo:?} is not a live Mercator projection; the floor would \
         reproject through zeros",
    );
    assert_eq!(
        h.gui_mut().mirror_source_rects(),
        vec![geo.rect],
        "the pane is registered to a strip the mirror pass does not copy",
    );
    assert!(
        seen.mirror_size_points[1] >= geo.rect.max.y,
        "the mirror is {} points tall and the strip reaches {}: the floor \
         would sample past the bottom of the texture",
        seen.mirror_size_points[1],
        geo.rect.max.y,
    );
}

/// Two 3D panes get two strips, and the strips do not overlap.
///
/// The one failure a packed layout would have that a uniform translation cannot:
/// a strip landing on another pane's makes one pane's floor a picture of the
/// other pane's map, which is a plausible-looking picture rather than a blank
/// one. Also the case the mirror's size bound is written against — see
/// `Gui::mirror_size_points`.
#[test]
fn two_3d_panes_get_two_strips_that_cannot_collide() {
    let painter = Arc::new(StubVolumePainter::painting());
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    h.set_pane_count(2);
    h.make_pane_volume(0);
    h.make_pane_volume(1);
    h.load_scan("KTLX");
    h.gui_mut().set_volume_painter(Some(painter.clone()));
    h.frames_for(2, FRAME_DT);

    let screen = h.screen_rect();
    let rects = h.gui_mut().mirror_source_rects();
    assert_eq!(rects.len(), 2, "two 3D panes asked for {rects:?}");
    // Area, not `intersects`: side-by-side panes share an edge and so do
    // their strips, exactly as the panes themselves do on the glass.
    let shared = rects[0].intersect(rects[1]);
    assert!(
        shared.width() <= 0.0 || shared.height() <= 0.0,
        "the strips {rects:?} overlap over {shared:?}: one pane's floor would \
         be the other pane's map",
    );
    for strip in &rects {
        assert!(
            strip.min.y >= screen.max.y,
            "strip {strip:?} is inside the frame",
        );
    }
    // The bound the mirror's memory arithmetic rests on.
    let bottom = rects.iter().map(|r| r.max.y).fold(0.0f32, f32::max);
    assert!(
        bottom <= 2.0 * screen.max.y,
        "the strips reach {bottom} points, past twice the frame's {}: the \
         mirror is no longer bounded at twice the frame",
        screen.max.y,
    );
}

/// A pane that stops showing a floor stops being registered, and its index does
/// not carry its old strip to whatever pane takes it next.
///
/// Indices are reused when the layout sheds panes, so a stale entry does not
/// read as absent — it reads as *some other pane's* map. Two ways in: the pane
/// becomes a map again, and the user hides the floor. Both must give the
/// mirror's texels back rather than go on copying a strip nothing draws into.
#[test]
fn a_pane_that_stops_showing_a_floor_stops_being_registered() {
    let (mut h, painter) = volume_harness(StubVolumePainter::painting());
    assert!(
        last_seen(&painter).source.is_some(),
        "the floor was never registered in the first place, so nothing below \
         means anything",
    );

    h.gui_mut()
        .pane_mut(1)
        .expect("a pane")
        .volume_mut()
        .expect("a 3D pane")
        .hide_floor = true;
    h.frames_for(2, FRAME_DT);
    assert_eq!(
        last_seen(&painter).source,
        None,
        "a pane with the floor hidden is still handing the renderer a \
         registration to draw one through",
    );
    assert!(h.gui_mut().mirror_source_rects().is_empty());

    // Back on, then converted away entirely.
    h.gui_mut()
        .pane_mut(1)
        .expect("a pane")
        .volume_mut()
        .expect("a 3D pane")
        .hide_floor = false;
    h.frames_for(2, FRAME_DT);
    assert!(h.gui_mut().mirror_source_rects().len() == 1);
    h.make_pane_map(1);
    h.frames_for(2, FRAME_DT);
    assert!(
        h.gui_mut().mirror_source_rects().is_empty(),
        "a pane that is a map again is still on the mirror's guest list, so \
         the mirror is copying a strip nothing draws into",
    );
}

/// A radar site whose label is map content on this pane and nothing else.
///
/// Vance AFB, about 110 km north-west of KTLX, so on a pane centred there at
/// [`WITNESS_ZOOM`] it lands well inside — nowhere near the 100-point cull
/// margin `visible_radar_sites` also draws labels into, which reach outside
/// the strip and are only clipped away later.
///
/// Deliberately **not** KTLX: the pane's own site is printed by the pill row
/// too, and a witness two different things draw cannot say which one drew it.
const GROUND_WITNESS_SITE: &str = "KVNX";

/// The unit label at the head of the pane's colour scale, which is the one
/// mark on the pane that only the legend draws.
const LEGEND_WITNESS: &str = "dBZ";

/// Zoom the ground witness needs. `handle_radar_site_interactions` draws site
/// names only from zoom 5 up, and a pane's default is 4.
const WITNESS_ZOOM: f64 = 7.0;

/// One pane, one scan, the radar-site labels switched on and the map zoomed in
/// far enough to draw them.
///
/// Those labels are the only **geography** a headless frame paints: it fetches
/// no tiles and uploads no radar texture, and both are drawn as textures
/// anyway, while the `RADARS` table is compiled in and its names are drawn as
/// per-frame text. Without them a 3D pane's strip is empty — the pane's legend
/// used to be the only thing in it, and the legend is exactly what has just
/// been moved out — so both tests below would compare two empty sets and pass
/// having proved nothing.
fn ground_witness_harness() -> (InputHarness, Arc<StubVolumePainter>) {
    let painter = Arc::new(StubVolumePainter::painting());
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    h.set_pane_count(1);
    h.load_scan("KTLX");
    h.gui_mut().set_volume_painter(Some(painter.clone()));
    h.gui_mut().enable_overlay_for_test(OverlayKind::RadarSites);
    h.gui_mut()
        .pane_mut(0)
        .expect("pane 0")
        .map_memory
        .set_zoom(WITNESS_ZOOM)
        .expect("walkers rejected the witness zoom");
    h.frames_for(2, FRAME_DT);
    (h, painter)
}

/// Where a mark landed inside some rect, rounded to whole points:
/// `(min x, min y, max x, max y)`.
type At = (i32, i32, i32, i32);

/// One mark a frame painted: where it landed, and what it said — empty for a
/// painted rect, which says nothing.
type Mark = (At, String);

/// Every mark the last frame painted inside `region`, in coordinates relative
/// to it: painted rects, and text runs with what they said.
///
/// Text as well as rects because the two halves of a pane's content are drawn
/// with different primitives — the legend is mostly filled rects, the site
/// labels are text — and a probe that saw only one of them would be blind to
/// whichever half it was pointed at.
fn local_marks(h: &InputHarness, region: egui::Rect) -> Vec<Mark> {
    let local = |rect: egui::Rect| {
        let l = rect.translate(-region.min.to_vec2());
        (
            l.min.x.round() as i32,
            l.min.y.round() as i32,
            l.max.x.round() as i32,
            l.max.y.round() as i32,
        )
    };
    let mut marks: Vec<Mark> = h
        .painted_rects()
        .iter()
        .filter(|rect| region.contains_rect(**rect))
        .map(|rect| (local(*rect), String::new()))
        .chain(
            h.painted_text_rects()
                .into_iter()
                .filter(|(rect, _)| region.contains_rect(*rect))
                .map(|(rect, text)| (local(rect), text)),
        )
        .collect();
    marks.sort_unstable();
    marks
}

/// The marks of one named text run, out of a set [`local_marks`] produced.
///
/// A run and not a single mark: every label on the map is painted twice, once
/// as its own shadow (`draw_shadowed_text`), so a witness that took one of them
/// would be picking a half at random.
fn marks_saying(marks: &[Mark], text: &str) -> Vec<At> {
    marks
        .iter()
        .filter(|(_, said)| said == text)
        .map(|(at, _)| *at)
        .collect()
}

/// The pane's **ground** really is drawn — into the strip, exactly as the same
/// pane drew it on the glass, and no longer on the glass.
///
/// Every other test here is about geometry the arm *reported*. This one is
/// about geometry the tessellator was actually handed, which is the difference
/// between a floor and a plan for one: the mirror pass copies primitives, so a
/// strip with no primitives in it is a transparent texture and a blank floor,
/// and nothing above this would notice.
///
/// It is written as a **comparison against the same pane as a map** rather than
/// as a count, because a count cannot tell a map from a stray highlight. The
/// same pane is rendered both ways at the same size and the painted marks are
/// compared in rect-local coordinates, so the claim is that every mark in the
/// strip is a mark the map pane made at the same place within its own rect.
///
/// Containment rather than equality, and the direction is the honest one: the
/// map pane's rect also has the shell's chrome over it — the layers panel, the
/// status bar, the pills — which is drawn above the pane rather than inside
/// `Map::show`, is not the same for a 3D pane as for a map, and must **not** be
/// copied onto a floor. It now also has the pane's own legend, which is chrome
/// too and stays behind for the same reason (see
/// [`the_panes_legend_is_painted_onto_the_glass_and_never_into_the_strip`]). So
/// the strip is a subset, and what stops a subset being vacuous is a named
/// piece of geography — [`GROUND_WITNESS_SITE`] — required *in* the strip and
/// required *absent* from the glass, in both cases at the local position the
/// plan view drew it at.
///
/// Tiles and the radar raster are absent from both sides — a headless frame
/// fetches no tiles and uploads no textures — so what is being compared is the
/// rest of the map content pass. That is the point: whatever the map arm
/// paints, the strip paints, and neither side is written down here to be kept
/// in step by hand.
#[test]
fn the_panes_map_is_painted_into_the_strip_and_not_onto_the_glass() {
    let (mut h, _painter) = ground_witness_harness();

    // The same pane, as a map, on the glass.
    let pane_rect = h.pane_rects()[0];
    let as_a_map = local_marks(&h, pane_rect);
    assert!(
        as_a_map.len() > 1,
        "the map pane painted nothing inside its own rect, so the comparison \
         below would hold between two empty sets",
    );
    let witness = marks_saying(&as_a_map, GROUND_WITNESS_SITE);
    assert!(
        !witness.is_empty(),
        "the map pane never drew {GROUND_WITNESS_SITE}, so requiring it below \
         would prove nothing; it painted {:?}",
        h.painted_text_strings_in(pane_rect),
    );

    h.make_pane_volume(0);
    h.frames_for(2, FRAME_DT);
    let strip = *h
        .gui_mut()
        .mirror_source_rects()
        .first()
        .expect("the 3D pane asked for no strip at all");

    let in_strip = local_marks(&h, strip);
    assert!(
        !in_strip.is_empty(),
        "nothing was painted into the strip {strip:?}: the mirror would copy \
         an empty rect and the floor would be blank",
    );
    let stray: Vec<_> = in_strip
        .iter()
        .filter(|mark| !as_a_map.contains(mark))
        .collect();
    assert!(
        stray.is_empty(),
        "the strip has {} marks the same pane's map does not, at {stray:?}: it \
         is drawing something other than the map it is supposed to be",
        stray.len(),
    );
    for at in &witness {
        assert!(
            in_strip.contains(&(*at, GROUND_WITNESS_SITE.to_owned())),
            "the strip did not get the pane's own map content: \
             {GROUND_WITNESS_SITE} is missing from {at:?}, where the same pane \
             drew it as a map",
        );
    }

    // And none of it is on the glass, under the volume.
    let on_glass = local_marks(&h, pane_rect);
    for at in &witness {
        assert!(
            !on_glass.contains(&(*at, GROUND_WITNESS_SITE.to_owned())),
            "the pane's map is still being drawn at {pane_rect:?}, on the \
             glass, over the volume it is meant to be the ground under: \
             {GROUND_WITNESS_SITE} is at {at:?}",
        );
    }
}

/// The mirror of the test above, for the other half of the pane's content: the
/// colour scale is chrome, so it belongs **on the glass** in ordinary 2D and
/// must never reach the strip.
///
/// A legend in the strip is a legend the raymarcher copies onto the floor,
/// where it is painted flat into the ground in perspective — shrinking with
/// distance, swinging round with the camera and unreadable from most of them.
/// That is what this pins, and it pins it in both directions at once, because
/// only one of the two is a regression anybody would notice: a legend that
/// stopped being drawn at all would pass a "not in the strip" assertion.
///
/// The placement claim is the strong half. The legend's marks are taken from
/// the pane drawn **as a map** and then required at the *same rect-local
/// positions* once the pane is a volume — so "the same widget under the same
/// placement rules" is asserted against the plan view itself rather than
/// against a copy of the geometry written down here. A 3D pane that drew its
/// own legend a few points off, or on the other edge, or at the floor's scale,
/// fails.
#[test]
fn the_panes_legend_is_painted_onto_the_glass_and_never_into_the_strip() {
    let (mut h, _painter) = ground_witness_harness();

    let pane_rect = h.pane_rects()[0];
    let as_a_map = local_marks(&h, pane_rect);
    let legend = marks_saying(&as_a_map, LEGEND_WITNESS);
    assert!(
        !legend.is_empty(),
        "the map pane drew no legend, so requiring one below would prove \
         nothing; it painted {:?}",
        h.painted_text_strings_in(pane_rect),
    );
    // The bar itself, not just its title: `color_scale_strips` counts the 2×20
    // strips `render_color_scale` lays the ramp down as, along each axis. A
    // legend reduced to its unit label would still satisfy the marks above.
    let bar_as_a_map = h.color_scale_strips(pane_rect);
    assert!(
        bar_as_a_map.0 + bar_as_a_map.1 > 0,
        "the map pane painted no colour-scale strips at all, so the counts \
         below would agree at zero",
    );

    h.make_pane_volume(0);
    h.frames_for(2, FRAME_DT);
    let strip = *h
        .gui_mut()
        .mirror_source_rects()
        .first()
        .expect("the 3D pane asked for no strip at all");
    let on_glass = local_marks(&h, pane_rect);
    let in_strip = local_marks(&h, strip);

    // The strip is drawing *something*, so "the legend is not in it" is a
    // statement about the legend rather than about an empty rect.
    assert!(
        !in_strip.is_empty(),
        "nothing reached the strip at all, so its lack of a legend says nothing",
    );

    for at in &legend {
        assert!(
            on_glass.contains(&(*at, LEGEND_WITNESS.to_owned())),
            "the 3D pane's legend is not where the plan view puts it: \
             {LEGEND_WITNESS} is missing from {at:?} on the glass. The pane \
             painted {:?}",
            h.painted_text_strings_in(pane_rect),
        );
        assert!(
            !in_strip.contains(&(*at, LEGEND_WITNESS.to_owned())),
            "the legend reached the strip at {at:?}: the mirror would copy it \
             onto the floor and paint it flat into the ground",
        );
    }

    assert_eq!(
        h.color_scale_strips(pane_rect),
        bar_as_a_map,
        "the 3D pane's colour bar is not the plan view's: the same pane drew \
         {bar_as_a_map:?} strips as a map",
    );
    assert_eq!(
        h.color_scale_strips(strip),
        (0, 0),
        "colour-bar strips were painted into the strip, so the floor is \
         carrying a legend",
    );
}

/// The reset returns the **pivot** as well as the angles, and leaves the view
/// mode alone.
///
/// The pivot is the one that is easy to forget: a reset that restores the
/// angle and the zoom while leaving the eye aimed at a corner of the box is a
/// control that visibly does something and leaves the pane still looking at
/// the wrong place.
///
/// It used to reset the region and its source pane too. Neither exists: the
/// box is the pane's viewport, so the way back to a wider one is to zoom the
/// map out, and this button deliberately does not move the user's map.
#[test]
fn the_reset_returns_the_pivot_as_well_as_the_angles_and_keeps_the_view_mode() {
    let mut volume = crate::pane::VolumePane {
        view_mode: crate::pane::VolumeViewMode::Isosurface,
        ..Default::default()
    };
    volume.camera.nudge(crate::pane::OrbitDelta {
        yaw_deg: 40.0,
        pitch_deg: -15.0,
        zoom_factor: 1.4,
        pan: [0.6, -0.4, 0.3],
    });
    volume.camera.set_vertical_exaggeration(9.0);
    assert_ne!(
        volume.camera.pivot(),
        [0.0; 3],
        "precondition: the view has been panned off centre",
    );

    reset_volume_view(&mut volume);

    assert_eq!(
        volume.camera.pivot(),
        [0.0; 3],
        "the pivot must come back, or the box stays off to one side",
    );
    assert_eq!(volume.camera, crate::pane::OrbitCamera::default());
    assert_eq!(
        volume.view_mode,
        crate::pane::VolumeViewMode::Isosurface,
        "the reset is for a pane that is lost, and a view mode is not a way to \
         be lost — flipping it back would un-choose something the user chose",
    );
}

/// **The box a 3D pane resamples is its own viewport, through the real render
/// arm.**
///
/// The end-to-end half of `ui_region`'s unit tests: those measure the
/// derivation, this measures that the pane *uses* it. Between them sits the
/// whole arm — the floor strip, the `mem::take`n pane, the publish onto
/// `VolumePane::viewport_box` — and the failure this catches is a derivation
/// that is perfectly correct and wired to nothing, which is what a pane looked
/// like for one iteration of this change.
///
/// Zooming is asserted to *move* the box, not merely to produce one. A pane
/// that answered the same 460 km whatever its viewport showed is exactly the
/// state before this change, and it is the state in which a zoomed-in pane
/// stands its volume on a floor that stops a quarter of the way across.
#[test]
fn a_3d_panes_box_follows_its_own_viewport() {
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    h.load_scan("KTLX");
    h.make_pane_volume(0);
    h.warm_up();

    let box_at = |h: &mut InputHarness| {
        h.gui_mut()
            .pane(0)
            .expect("pane 0")
            .volume()
            .expect("a pane in the 3D render mode")
            .viewport_box
            .expect("the arm must publish the box it measured")
            .half_width_km()
    };

    // Wide open: the viewport shows more ground than the resampler will
    // honour, so the box stops at its ceiling — the whole scan, cropping
    // nothing, which is what a pane that has not been aimed should show.
    let wide = box_at(&mut h);
    assert_eq!(
        wide,
        rustdar_radar::voxel::MAX_HALF_WIDTH_KM,
        "a pane at the default zoom sees past the resampler's ceiling, so its \
         box must sit on it",
    );

    // Zoomed in, the way a user frames a storm.
    h.gui_mut()
        .pane_mut(0)
        .expect("pane 0")
        .map_memory
        .set_zoom(11.0)
        .expect("11 is inside walkers' range");
    h.warm_up();

    let tight = box_at(&mut h);
    assert!(
        tight < wide,
        "zooming the pane in must tighten its box: {tight} km against {wide} km. \
         A box that ignores the viewport is one the pane's own floor cannot cover.",
    );
    // Not merely smaller — small enough that the grid's fixed cell count buys
    // real detail, which is the entire reason a region control ever existed.
    assert!(
        tight < 0.25 * wide,
        "four zoom steps bought only {tight} km against {wide} km; the box is \
         not tracking the viewport, it is being nudged by something else",
    );
}
