use super::*;
use crate::input_harness::InputHarness;
use crate::volume_view::{Showing, StubVolumePainter, VolumeFrameState};
use std::sync::Arc;

const FRAME_DT: f64 = 1.0 / 60.0;

/// The grid on screen is the one the pane asked for — what every caption test
/// that is not *about* the stand-in wants to be told.
const SETTLED: Showing = Showing::SETTLED;

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

/// The painter's report reaches the caption the user reads.
///
/// The two halves of the stand-in are pinned apart — the renderer decides
/// what to draw (`rustdar-frontend`'s `volume_stand_in` suite) and
/// `volume_caption` decides what to write — and this is the seam between
/// them. It is exactly the sort of wiring that can be dropped without a
/// compiler error: `render_volume_pane` could ignore the `showing` field and
/// hand `Showing::SETTLED` to the caption, and every test on either side
/// would stay green while the pane silently went back to claiming a sharpness
/// it does not have.
#[test]
fn the_pane_captions_the_picture_the_painter_says_it_drew() {
    let (h, _painter) = volume_harness(StubVolumePainter::standing_in(Showing {
        cell_km: Some((1.8, 1.8)),
        stale: true,
        partial: false,
    }));
    let pane_rect = h.pane_rects()[1];
    let texts = h.painted_text_strings_in(pane_rect);
    assert!(
        texts.iter().any(|t| t.contains("sharpening")),
        "a pane whose painter says it is standing in must say so in its own \
             caption; painted texts were {texts:?}",
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

/// The zoom of a pane's own viewport, whichever way the pane is drawn.
fn ground_zoom(h: &mut InputHarness, idx: usize) -> f64 {
    h.gui_mut().pane(idx).expect("a pane").map_memory.zoom()
}

/// The half-extent of the box a 3D pane published this frame, kilometres on
/// each horizontal axis.
fn box_extent(h: &mut InputHarness, idx: usize) -> rustdar_radar::voxel::HalfExtentKm {
    h.gui_mut()
        .pane(idx)
        .expect("a pane")
        .volume()
        .expect("a pane in the 3D render mode")
        .viewport_box
        .expect("the arm must publish the box it measured")
        .half_extent_km()
}

/// Scrolling over the 3D pane zooms it; scrolling over another pane does
/// not.
///
/// `Input::zoom_delta` and the scroll delta are **global** — they report the
/// frame's gesture wherever on screen it happened — so the
/// `hovered() || dragged()` gate is correctness rather than politeness.
/// Without it a wheel over a map pane would zoom every 3D pane on screen.
///
/// What "zooms it" means is the pane's **viewport**, which is the whole
/// change: scroll and pinch aim the geography in both render modes, and the
/// box follows because the box *is* the viewport.
#[test]
fn only_a_gesture_over_the_pane_zooms_it() {
    let (mut h, _painter) = volume_harness(StubVolumePainter::painting());
    let rects = h.pane_rects();

    let before = ground_zoom(&mut h, 1);
    h.scroll_at(rects[0].center(), egui::vec2(0.0, 200.0));
    h.frames_for(2, FRAME_DT);
    assert_eq!(
        ground_zoom(&mut h, 1),
        before,
        "a scroll over the map pane must not zoom the 3D pane's ground",
    );

    h.scroll_at(rects[1].center(), egui::vec2(0.0, 200.0));
    h.frames_for(2, FRAME_DT);
    let after = ground_zoom(&mut h, 1);
    assert!(
        after > before,
        "scrolling up over the 3D pane should zoom its geography in: {before} -> {after}",
    );
}

/// A **pinch** outside a 3D pane leaves it alone, exactly as a wheel does.
///
/// The wheel half above rides on `smooth_scroll_delta`; this one rides on
/// `zoom_delta`, and they are separate branches of `ui_region::zoom_step`
/// with separate opportunities to lose the gate. Driven through the web
/// backend's two-device event shape, because that is what a browser sends
/// and what `normalize_touch_devices` exists to fold together — so this is
/// the touch and web arm of the same rule.
#[test]
fn a_pinch_outside_a_3d_pane_does_not_zoom_it() {
    let (mut h, _painter) = volume_harness(StubVolumePainter::painting());
    let rects = h.pane_rects();

    let before = ground_zoom(&mut h, 1);
    h.web_pinch(rects[0].center(), 80.0, 400.0, 6);
    h.frames_for(2, FRAME_DT);
    assert_eq!(
        ground_zoom(&mut h, 1),
        before,
        "a pinch over the map pane zoomed the 3D pane beside it - the \
         `hovered() || dragged()` gate on a global `zoom_delta` is gone",
    );

    // The control, so a pass cannot come from pinch being broken outright.
    let (mut h, _painter) = volume_harness(StubVolumePainter::painting());
    let rects = h.pane_rects();
    let before = ground_zoom(&mut h, 1);
    h.web_pinch(rects[1].center(), 80.0, 400.0, 6);
    h.frames_for(2, FRAME_DT);
    assert!(
        ground_zoom(&mut h, 1) > before,
        "control: a pinch on the 3D pane itself must zoom its geography",
    );
}

/// **The camera does not move when the box changes.**
///
/// The hard requirement of the whole change, and the one that is free:
/// `eye_distance` is in multiples of the box's half-diagonal, so a box the
/// zoom tightened carries the eye in with it and the stored *ratio* never
/// has to be touched. Nothing may reframe, ease or fit — the user is the
/// only thing that moves this camera.
///
/// Asserted on the whole camera rather than on `eye_distance` alone: a
/// "helpful" reframe would plausibly nudge the pitch or re-centre the pivot
/// instead, and an assertion aimed at one field would not see it. The camera
/// is deliberately set away from its default first, because a reframe *to*
/// the default passes against a default-valued camera.
#[test]
fn zooming_the_box_leaves_the_camera_exactly_where_the_user_left_it() {
    let (mut h, _painter) = volume_harness(StubVolumePainter::painting());
    let rect = h.pane_rects()[1];
    let placed = crate::pane::OrbitCamera::restore(137.0, -23.0, 1.75, [0.4, -0.2, 0.6], 4.5)
        .expect("finite");
    h.gui_mut()
        .pane_mut(1)
        .expect("pane 1")
        .volume_mut()
        .expect("a 3D pane")
        .camera = placed;
    h.frames_for(2, FRAME_DT);

    let before_box = box_extent(&mut h, 1).corner_km();
    h.scroll_at(rect.center(), egui::vec2(0.0, 200.0));
    h.frames_for(4, FRAME_DT);

    let after_box = box_extent(&mut h, 1).corner_km();
    assert!(
        after_box < before_box,
        "precondition: the scroll must have re-cut the box: {before_box} -> {after_box}",
    );
    assert_eq!(
        camera_of(&mut h, 1),
        placed,
        "the box changed under the camera and something moved the camera with it",
    );
}

/// A drag orbits a 3D pane and pans a plan view — the same gesture, the two
/// meanings the two pictures have for it.
///
/// Both halves in one test on one harness, because the claim is about the
/// *pair*: a drag that orbited both, or panned both, would satisfy either
/// half read alone. Each half also asserts the other pane's verb did **not**
/// happen — a drag that orbited *and* slid the ground under it is the shape
/// the two-finger cancel in `volume_pane_outcome` exists to prevent.
#[test]
fn a_drag_orbits_in_3d_and_pans_in_2d() {
    let (mut h, _painter) = volume_harness(StubVolumePainter::painting());
    let rects = h.pane_rects();

    // The 3D pane: the camera turns and the ground stays put.
    let before_camera = camera_of(&mut h, 1);
    let before_ground = h.pane_center(1);
    h.mouse_press(rects[1].center());
    h.frames_for(1, FRAME_DT);
    h.mouse_move(rects[1].center() + egui::vec2(110.0, 0.0));
    h.frames_for(1, FRAME_DT);
    h.mouse_release(rects[1].center() + egui::vec2(110.0, 0.0));
    h.frames_for(1, FRAME_DT);

    assert_ne!(
        camera_of(&mut h, 1).yaw_deg(),
        before_camera.yaw_deg(),
        "a drag on a 3D pane must orbit it",
    );
    assert_eq!(
        h.pane_center(1).map(|p| (p.x(), p.y())),
        before_ground.map(|p| (p.x(), p.y())),
        "a drag on a 3D pane must not also slide the ground under the box",
    );

    // The plan view: the ground slides and no camera turns.
    let before_camera = camera_of(&mut h, 1);
    let before_ground = h.pane_center(0);
    h.mouse_press(rects[0].center());
    h.frames_for(1, FRAME_DT);
    h.mouse_move(rects[0].center() + egui::vec2(110.0, 0.0));
    h.frames_for(2, FRAME_DT);
    h.mouse_release(rects[0].center() + egui::vec2(110.0, 0.0));
    h.frames_for(1, FRAME_DT);

    assert_ne!(
        h.pane_center(0).map(|p| (p.x(), p.y())),
        before_ground.map(|p| (p.x(), p.y())),
        "a drag on a plan view must pan it",
    );
    assert_eq!(
        camera_of(&mut h, 1),
        before_camera,
        "a drag on the plan view moved the 3D pane's camera",
    );
}

/// A two-finger drag pans a 3D pane, and the spread in the same gesture
/// zooms its geography.
///
/// The touch spelling of "right-drag pans", and the one that has to carry
/// both verbs at once: `MultiTouchInfo` reports the translation and the
/// pinch from one gesture, and a user who slides two fingers apart while
/// moving them expects both to happen. This is also the pin that one finger
/// still orbits — `normalize_touch_devices` synthesises a *primary* drag
/// from a touch, so a two-finger gesture would be read as an orbit too if
/// the cancel in `volume_pane_outcome` were dropped.
#[test]
fn a_two_finger_drag_pans_a_3d_pane_and_its_spread_zooms_the_ground() {
    let (mut h, _painter) = volume_harness(StubVolumePainter::painting());
    let rect = h.pane_rects()[1];
    let before_camera = camera_of(&mut h, 1);
    let before_ground = ground_zoom(&mut h, 1);

    // Two fingers, spreading *and* travelling: one gesture, both verbs.
    let a = rect.center() - egui::vec2(60.0, 0.0);
    let b = rect.center() + egui::vec2(60.0, 0.0);
    h.web_first_finger_down(a);
    h.web_second_finger_down(b);
    h.frames_for(1, FRAME_DT);
    for step in 1..=6 {
        let spread = 60.0 + 30.0 * step as f32;
        let travel = egui::vec2(9.0 * step as f32, 0.0);
        h.web_pinch_move(
            rect.center() - egui::vec2(spread, 0.0) + travel,
            rect.center() + egui::vec2(spread, 0.0) + travel,
        );
        h.frames_for(1, FRAME_DT);
    }
    h.web_second_finger_up(rect.center() + egui::vec2(300.0, 0.0));
    h.web_first_finger_up(rect.center() - egui::vec2(120.0, 0.0));
    h.frames_for(1, FRAME_DT);

    let after = camera_of(&mut h, 1);
    assert_ne!(
        after.pivot(),
        before_camera.pivot(),
        "a two-finger drag must pan the box",
    );
    assert_eq!(
        (after.yaw_deg(), after.pitch_deg()),
        (before_camera.yaw_deg(), before_camera.pitch_deg()),
        "a two-finger drag must not also orbit - the box would spin while it slid",
    );
    assert!(
        ground_zoom(&mut h, 1) > before_ground,
        "the spread in the same gesture must zoom the geography",
    );
}

/// A scroll moves a 3D pane's viewport by the same amount it moves a plan
/// view's.
///
/// "The same gesture means the same thing" is the whole brief, and it is not
/// something the code can be read for: `ui_region::zoom_step` is a restatement
/// of `walkers::Map::zoom_delta`, which a 3D pane cannot reach because its map
/// is drawn off screen. So the two arms are driven through the real UI on one
/// screen and their answers compared — a restatement that drifts from walkers
/// fails here, and comparing this function against a copy of itself never
/// would.
/// **One harness each, deliberately.** `smooth_scroll_delta` decays over
/// several frames, so a second notch on the same harness lands on top of the
/// first one's tail and reads as the two arms disagreeing by two thirds. The
/// first version of this test did exactly that and blamed the production code.
#[test]
fn a_scroll_moves_a_3d_pane_the_same_distance_it_moves_a_plan_view() {
    // One notch over pane `idx`, from a harness with identical geometry, zoom
    // and frame history — so the only difference between the two answers is
    // which arm read the wheel.
    let notch = |idx: usize| {
        let (mut h, _painter) = volume_harness(StubVolumePainter::painting());
        let rects = h.pane_rects();
        // Both panes start from one zoom, so the comparison is of two deltas
        // rather than of two ratios taken at different scales.
        for pane in [0, 1] {
            h.gui_mut()
                .pane_mut(pane)
                .expect("a pane")
                .map_memory
                .set_zoom(9.0)
                .expect("9 is inside walkers' range");
        }
        h.warm_up();
        let before = ground_zoom(&mut h, idx);
        h.scroll_at(rects[idx].center(), egui::vec2(0.0, 120.0));
        h.frames_for(1, FRAME_DT);
        ground_zoom(&mut h, idx) - before
    };

    let flat_step = notch(0);
    let solid_step = notch(1);
    assert!(
        flat_step > 0.0,
        "precondition: the plan view must have zoomed at all",
    );
    assert!(
        (flat_step - solid_step).abs() < 1e-9,
        "one wheel notch moved the plan view {flat_step} zoom levels and the 3D \
         pane {solid_step} - the same gesture has stopped meaning the same thing",
    );
}

/// Zooming in stops at the ground the radar covers rather than falling
/// through it.
///
/// The bound the user approved, and the latent bug it closes: below
/// `MIN_HALF_WIDTH_KM` the measurement is *refused*, and the caller's
/// fallback for a refusal is `DEFAULT_HALF_WIDTH_KM` — so a viewport zoomed
/// one notch too far would have taken the box from 10 km straight to 230,
/// which is a pop, a full resample, and a floor that covers a twenty-third
/// of what it stands under. The gesture stops instead.
#[test]
fn zooming_in_stops_at_the_resamplers_floor_rather_than_popping_to_the_ceiling() {
    let (mut h, _painter) = volume_harness(StubVolumePainter::painting());
    let rect = h.pane_rects()[1];

    // Forty notches: far past the floor at any starting zoom, and each one
    // is a separate opportunity for the bound to be applied once and then
    // forgotten.
    for _ in 0..40 {
        h.scroll_at(rect.center(), egui::vec2(0.0, 300.0));
        h.frames_for(1, FRAME_DT);
        let km = box_extent(&mut h, 1);
        assert!(
            km.east_km.min(km.north_km) >= rustdar_radar::voxel::MIN_HALF_WIDTH_KM,
            "the box fell through the resampler's floor to {km:?} km",
        );
        assert!(
            km.corner_km() <= rustdar_radar::voxel::MAX_HALF_DIAGONAL_KM,
            "the box popped to the fallback at {km:?} km - the measurement was \
             refused and the caller used the whole-scan default",
        );
    }
    assert!(
        box_extent(&mut h, 1).corner_km()
            < 2.0 * std::f64::consts::SQRT_2 * rustdar_radar::voxel::MIN_HALF_WIDTH_KM,
        "precondition: the zoom must actually have reached the floor, or the \
         assertions above passed without ever being tested",
    );
}

/// A continuous scroll re-cuts the box while it runs and **stops** when it
/// does.
///
/// The churn question the 1 km quantisation exists to answer. The region is
/// part of `VolumeTarget`, so every distinct value is a fresh multi-MiB
/// resample; what must never happen is a *steady state* that keeps producing
/// new ones, because that is a permanently hot CPU whose only symptom is a
/// fan. Transient churn during the gesture is the honest cost of the box
/// being the viewport — the user asked for a different box and got one.
///
/// So this pins the settle, not the transient: once the wheel stops, the
/// region must be bit-identical frame after frame, including through the
/// frames where `smooth_scroll_delta` is still decaying to zero.
#[test]
fn a_scroll_that_stops_stops_re_cutting_the_box() {
    let (mut h, _painter) = volume_harness(StubVolumePainter::painting());
    let rect = h.pane_rects()[1];

    for _ in 0..6 {
        h.scroll_at(rect.center(), egui::vec2(0.0, 150.0));
        h.frames_for(1, FRAME_DT);
    }
    // The decay: `smooth_scroll_delta` does not go to zero the frame the
    // wheel stops, and a settle measured before it has is a settle that was
    // never tested.
    h.frames_for(20, FRAME_DT);

    let settled = h
        .gui_mut()
        .pane(1)
        .expect("pane 1")
        .volume()
        .expect("a 3D pane")
        .viewport_box;
    for frame in 0..30 {
        h.frames_for(1, FRAME_DT);
        let now = h
            .gui_mut()
            .pane(1)
            .expect("pane 1")
            .volume()
            .expect("a 3D pane")
            .viewport_box;
        assert_eq!(
            now, settled,
            "frame {frame} after the scroll stopped re-keyed the region, so the \
             pane resamples for ever with a still viewport",
        );
    }
}

/// A 3D pane's viewport is its own: the link does not carry it either way.
///
/// The **stated decision**, pinned so that it is a decision rather than an
/// accident of `PaneState::is_map` answering `false` for a 3D pane.
/// `sync_viewports` is defined over plan views — panes with a raster to
/// dispatch, donate and synchronise — and a 3D pane is neither a source nor a
/// target of it.
///
/// The cost of the other choice is what settles it: a 3D pane as a sync
/// *target* would resample its whole box, on the frame thread, every time a
/// neighbour's wheel turned — and a plan view zoomed to the street would drive
/// the box straight through the floor `zoom_viewport`'s own bound refuses to
/// cross. Changing it is a one-word change (`is_map` to a predicate about
/// having a viewport at all) and wants its own review, not this one.
#[test]
fn a_3d_panes_viewport_is_not_carried_by_the_link() {
    let (mut h, _painter) = volume_harness(StubVolumePainter::painting());
    let rects = h.pane_rects();

    // The 3D pane's own zoom does not reach the plan view beside it.
    let flat_before = ground_zoom(&mut h, 0);
    h.scroll_at(rects[1].center(), egui::vec2(0.0, 200.0));
    h.frames_for(4, FRAME_DT);
    assert!(
        ground_zoom(&mut h, 1) != flat_before,
        "precondition: the 3D pane must have zoomed",
    );
    assert_eq!(
        ground_zoom(&mut h, 0),
        flat_before,
        "a 3D pane drove the linked plan view's viewport",
    );

    // And the plan view's does not reach the 3D pane.
    let solid_before = ground_zoom(&mut h, 1);
    h.scroll_at(rects[0].center(), egui::vec2(0.0, 200.0));
    h.frames_for(4, FRAME_DT);
    assert!(
        ground_zoom(&mut h, 0) != flat_before,
        "precondition: the plan view must have zoomed",
    );
    assert_eq!(
        ground_zoom(&mut h, 1),
        solid_before,
        "the link carried a plan view's zoom into a 3D pane's box",
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
        let lines = volume_caption(
            "KTLX",
            at(33),
            None,
            square(crate::pane::BASE_HALF_WIDTH_KM),
            camera,
            SETTLED,
        );
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
    let lines = volume_caption(
        "KTLX",
        at(39),
        Some(at(33)),
        square(crate::pane::BASE_HALF_WIDTH_KM),
        Default::default(),
        SETTLED,
    );
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
    let merged = volume_caption(
        "KTLX",
        at(39),
        Some(at(33)),
        square(crate::pane::BASE_HALF_WIDTH_KM),
        Default::default(),
        SETTLED,
    );
    let base = merged
        .iter()
        .find(|l| l.contains("base volume"))
        .unwrap_or_else(|| panic!("the base volume must be named: {merged:?}"));
    assert!(
        base.contains("22:33") && !base.contains("22:39"),
        "the base line must carry the base volume's own time: {base}",
    );

    let filling = volume_caption(
        "KTLX",
        at(39),
        None,
        square(crate::pane::BASE_HALF_WIDTH_KM),
        Default::default(),
        SETTLED,
    );
    assert!(
        filling.iter().any(|l| l.contains("no complete volume yet")),
        "a first volume still filling must be said out loud: {filling:?}",
    );
}

/// The caption reports the resolution the box buys, and it moves with the
/// box.
///
/// The grid's cell count is fixed, so a tighter box spends the same cells
/// over less ground — 2.54 km per cell over a WSR-88D's whole reflectivity
/// volume against 0.16 at 20 km. That is the main reason to pick a region,
/// and it is invisible unless it is written down.
///
/// The sourceless figures are pinned as literals — the 651 km box a 460.125
/// km reflectivity reach earns and the 2.54 km cells it costs — rather than
/// derived from the function the caption itself reads, so a policy that
/// drifted fails here by name instead of being restated as correct.
#[test]
fn the_caption_reports_the_resolution_the_box_buys() {
    let whole_volume = rustdar_radar::voxel::box_half_width_km(460.125);
    let wide = volume_caption(
        "KTLX",
        at(33),
        None,
        square(whole_volume),
        Default::default(),
        SETTLED,
    );
    assert!(
        wide.iter()
            .any(|l| l.contains("651 km box") && l.contains("2.54 km/cell")),
        "a whole WSR-88D reflectivity volume must report the box its reach \
         earns and the cost of it: {wide:?}",
    );

    let tight = crate::pane::VolumeRegion::new(
        crate::pane::GeoPoint {
            lat: 35.3,
            lon: -97.3,
        },
        rustdar_radar::voxel::HalfExtentKm::square(20.0),
    )
    .expect("a valid region");
    let tight_lines = volume_caption(
        "KTLX",
        at(33),
        None,
        tight.half_extent_km(),
        Default::default(),
        SETTLED,
    );
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

/// While the grid on screen is not the one this box asked for, the caption
/// reports the picture — not the request.
///
/// This is the honest half of the zoom's progressive refinement, and it is
/// what licenses the picture half. A pane that has just been zoomed goes on
/// drawing the grid it already has, put into the new box, so the "N km box"
/// figure stays true throughout; the cell size does not, and a caption that
/// went on dividing this box by the cell count would claim a sharpness nobody
/// can see. Either the line says what resolution is really there, or the pane
/// must blank — and blanking is the defect this replaced.
///
/// The two arms differ in what the zoom *did*. Inwards, the held grid covers
/// the whole new box: the picture is complete and merely soft, so the line
/// promises the sharper one. Outwards it does not: the picture is real data in
/// the middle and nothing outside it, and a volume that simply stops is read
/// as weather that stops, so that has to be said rather than promised away.
#[test]
fn a_caption_over_a_stand_in_reports_the_picture_and_not_the_request() {
    let cells = rustdar_radar::voxel::default_shape().nx as f64;
    let asked_for = format!("{:.2} km/cell", 40.0 / cells);
    let line_of = |showing| {
        volume_caption(
            "KTLX",
            at(33),
            None,
            square(20.0),
            Default::default(),
            showing,
        )
        .into_iter()
        .find(|l| l.contains("km box"))
        .expect("a box line")
    };

    // Zoomed in: soft now, sharp shortly.
    let softened = line_of(Showing {
        cell_km: Some((1.8, 1.8)),
        stale: true,
        partial: false,
    });
    assert!(
        softened.contains("40 km box"),
        "the box is the one being drawn, and that is the requested one even \
         while the grid inside it is older: {softened:?}",
    );
    assert!(
        softened.contains("1.80 km/cell") && softened.contains("sharpening"),
        "the line must report the grid actually on screen and say a sharper \
         one is coming: {softened:?}",
    );
    assert!(
        !softened.starts_with(&format!("40 km box - {asked_for}")),
        "this box's own cell size must not be the headline figure while a \
         coarser grid is on screen: {softened:?}",
    );

    // Zoomed out: the middle is real and the rest is not there yet.
    let hollow = line_of(Showing {
        cell_km: Some((1.8, 1.8)),
        stale: true,
        partial: true,
    });
    assert!(
        hollow.contains("over the middle") && hollow.contains("filling in"),
        "a picture that does not reach the box's edges must say so, or the \
         edge of the grid is read as the edge of the weather: {hollow:?}",
    );

    // And once the build lands the line goes back to the plain statement —
    // no permanent "sharpening" on a picture that is already sharp.
    let settled = line_of(Showing {
        cell_km: Some((1.8, 1.8)),
        stale: false,
        partial: false,
    });
    assert_eq!(settled, format!("40 km box - {asked_for}"));
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
    // The third verb the same rect carries. A pan that also zoomed would be
    // the box sliding and re-cutting at once, and `zoom_viewport` reads its
    // gesture off this very response.
    let before_ground = ground_zoom(&mut h, 1);

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
    assert_eq!(
        ground_zoom(&mut h, 1),
        before_ground,
        "a secondary drag must not zoom the ground as well as pan the box",
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

/// A square half-extent, for the caption tests whose subject is something
/// other than the box's shape.
fn square(half_km: f64) -> rustdar_radar::voxel::HalfExtentKm {
    rustdar_radar::voxel::HalfExtentKm::square(half_km)
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

/// Nothing prints through the Volume Alpha button — and the colour scale is
/// what used to.
///
/// The pane's legend is painted onto the glass rather than allocated, so it
/// senses nothing, takes no part in layout and cannot be pushed aside. The
/// button, planted in `pane_rect`'s top-right corner, stood in exactly the
/// corner the vertical scale's unit title stands in: `dBZ` printed straight
/// through `Volume alpha` at every pane width, which is what
/// `legend-on-the-glass/floor-after.png` shows. So the *button* moves — off
/// `color_scale_free_rect` instead of the pane — because the legend's
/// placement is shared with the plan view and forking it for one pane kind
/// would trade a visible overlap for an invisible misalignment across a split.
///
/// # Why it is written against the marks and not against the arithmetic
///
/// The claim is "no text the pane paints lands inside the button, except the
/// button's own label", which is what a user sees. A test comparing the
/// button's rect to a second copy of the legend's geometry would agree with
/// itself while both were wrong, and would say nothing about the value labels
/// or a second stacked bar.
///
/// The last block is what stops it being vacuous: the *old* rect is
/// reconstructed from `pane_rect` and required to collide, so a legend that
/// stopped being drawn — or a title that moved off the corner by itself —
/// fails here rather than passing an assertion about an empty set.
#[test]
fn the_colour_scale_does_not_print_through_the_volume_alpha_button() {
    let (mut h, _painter) = ground_witness_harness();
    h.make_pane_volume(0);
    h.frames_for(2, FRAME_DT);

    let pane_rect = h.pane_rects()[0];
    assert!(
        !h.gui_mut().mirror_source_rects().is_empty(),
        "precondition: the 3D arm never got as far as drawing a pane",
    );
    let (_, button) = *h
        .alpha_buttons()
        .iter()
        .find(|(idx, _)| *idx == 0)
        .expect("the 3D pane drew no Volume Alpha button");
    assert!(
        pane_rect.contains_rect(button),
        "the button {button:?} left the pane {pane_rect:?} entirely",
    );

    // The pane is landscape, so the panel-wide orientation puts the bars on the
    // right edge — the same edge the button hangs off.
    let title: Vec<egui::Rect> = h
        .painted_text_rects()
        .into_iter()
        .filter(|(rect, text)| text == LEGEND_WITNESS && pane_rect.contains_rect(*rect))
        .map(|(rect, _)| rect)
        .collect();
    // More than one run: `draw_shadowed_text` lays the title down twice, the
    // outline and then the text. Both are on screen, so both count.
    assert!(
        !title.is_empty(),
        "precondition: the pane drew no {LEGEND_WITNESS} at all, so the \
         collision below would be about an empty set; it painted {:?}",
        h.painted_text_strings_in(pane_rect),
    );

    let through: Vec<(egui::Rect, String)> = h
        .painted_text_rects()
        .into_iter()
        .filter(|(rect, text)| {
            pane_rect.contains_rect(*rect)
                && rect.intersects(button)
                && text != volume_alpha_editor::ALPHA_BUTTON_LABEL
        })
        .collect();
    assert!(
        through.is_empty(),
        "text is printing through the Volume Alpha button at {button:?}: \
         {through:?}",
    );

    // …and the corner the button used to stand in really is the one the title
    // is in, so the guard above has teeth.
    let old_button = egui::Rect::from_min_size(
        pane_rect.right_top() + egui::vec2(-(88.0 + 8.0), 8.0),
        egui::vec2(88.0, 20.0),
    );
    assert!(
        title.iter().any(|rect| old_button.intersects(*rect)),
        "the legend's title at {title:?} no longer reaches the pane's top-right \
         corner {old_button:?}, so this test would pass with the button put \
         back there",
    );
}

/// Every product's unit title fits the overhang the gutter reserves for it.
///
/// [`SCALE_TITLE_OVERHANG`] is the one guessed number in
/// `color_scale_gutter`: the title is centred on a [`SCALE_BAR_WIDTH`] bar and
/// is wider than it, so the block reaches that much further in than the bar
/// does. Guessed once and then measured here, against every product and every
/// unit preference that changes a suffix — a title wider than the reserve is a
/// title the Volume Alpha button would print through again, and the only sign
/// would be a screenshot.
#[test]
fn every_unit_titles_overhang_fits_the_gutter() {
    use rustdar_units::{HailSizeUnit, HeightUnit, PrecipRateUnit, SpeedUnit, UserPreferences};

    let ctx = egui::Context::default();
    // One frame, so the fonts exist and the layout below can measure.
    let _ = ctx.run_ui(egui::RawInput::default(), |_| {});
    let measure = ctx.debug_painter();
    let font = egui::FontId::proportional(pane_render::SCALE_TITLE_FONT_SIZE);

    // Every unit setting that can change a colour-scale title, crossed. Off
    // each enum's own `ALL`, so a unit added later is measured without anyone
    // remembering to add it here.
    let mut widest = (0.0_f32, "");
    for &speed in SpeedUnit::ALL {
        for &height in HeightUnit::ALL {
            for &hail_size in HailSizeUnit::ALL {
                for &precip_rate in PrecipRateUnit::ALL {
                    let prefs = UserPreferences {
                        speed,
                        height,
                        hail_size,
                        precip_rate,
                        ..UserPreferences::default()
                    };
                    for &product in RadarProduct::all() {
                        let unit = product.unit_label(&prefs);
                        let width = measure
                            .layout_no_wrap(unit.to_owned(), font.clone(), egui::Color32::WHITE)
                            .rect
                            .width();
                        if width > widest.0 {
                            widest = (width, unit);
                        }
                    }
                }
            }
        }
    }

    assert!(
        widest.0 > 0.0,
        "nothing was measured, so the bound below holds vacuously",
    );
    let overhang = (widest.0 - pane_render::SCALE_BAR_WIDTH) / 2.0;
    assert!(
        overhang <= pane_render::SCALE_TITLE_OVERHANG,
        "{:?} lays out {} points wide, which hangs {overhang} points past its \
         bar — more than the {} the gutter reserves, so it reaches back under \
         the pane's floating chrome",
        widest.1,
        widest.0,
        pane_render::SCALE_TITLE_OVERHANG,
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

    let box_at = |h: &mut InputHarness| box_extent(h, 0);

    // Wide open: the viewport shows more ground than the resampler will
    // honour, so the box stops at its ceiling — a **corner** on
    // `MAX_HALF_DIAGONAL_KM`, which for the square this measures today is a
    // half-width of `MAX_EXTENT_KM / √2`. That is past even a 460 km
    // surveillance cut's own 325.4, so it crops nothing a radar can produce,
    // and it is what a pane nobody has aimed should show.
    let wide = box_at(&mut h);
    assert!(
        (wide.corner_km() - rustdar_radar::voxel::MAX_HALF_DIAGONAL_KM).abs() < 1e-9,
        "a pane at the default zoom sees past the resampler's ceiling, so its \
         box's corner must sit on it: {:?} is a {} km corner against {}",
        wide,
        wide.corner_km(),
        rustdar_radar::voxel::MAX_HALF_DIAGONAL_KM,
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
        tight.corner_km() < wide.corner_km(),
        "zooming the pane in must tighten its box: {tight:?} against {wide:?}. \
         A box that ignores the viewport is one the pane's own floor cannot cover.",
    );
    // Not merely smaller — small enough that the grid's fixed cell count buys
    // real detail, which is the entire reason a region control ever existed.
    assert!(
        tight.corner_km() < 0.25 * wide.corner_km(),
        "four zoom steps bought only {tight:?} against {wide:?}; the box is \
         not tracking the viewport, it is being nudged by something else",
    );
}

/// The Map floor checkbox says so when it cannot draw a floor.
///
/// The floor is not a layer drawn beside the volume — it is drawn *by* the
/// raymarch, inside the paint callback a 3D pane pushes only when it has a
/// picture. So in every state where the arm explains itself instead of drawing
/// (no painter, no published volume, a product with no vertical structure, a
/// grid still building) the checkbox is a control that produces nothing, and
/// until now it produced nothing in silence. It stays tickable — the preference
/// is durable and takes effect the moment a picture arrives — and it now says
/// why, quoting the pane's own reason rather than a second wording of it.
///
/// **Both halves, and both are what make this able to fail.** The note must be
/// absent while the pane is drawing, or a note nailed permanently under the row
/// would pass; and it must be present when the pane is not, or deleting the
/// note entirely would pass. The `Map floor` anchor is checked in both states so
/// neither half can pass by the whole block being off screen.
#[test]
fn the_map_floor_checkbox_says_when_there_is_no_floor_to_draw() {
    let (mut h, _painter) = volume_harness(StubVolumePainter::painting());

    // The 3D pane is the one whose properties the sidebar is about. Activated
    // by a click on the pane itself, which is the user's own route — and which
    // must not fade the chrome away underneath the panel we are about to read.
    h.mouse_click(h.pane_rects()[1].center());
    h.warm_up();
    assert_eq!(
        h.active_pane_index(),
        1,
        "the 3D pane must be the active one"
    );
    h.open_pane_props();

    let inspector = |h: &InputHarness| h.inspector_rect().expect("the inspector is open");
    let row_drawn = |h: &InputHarness| h.text_painted_in(inspector(h), MAP_FLOOR_LABEL);
    let note_drawn = |h: &InputHarness| h.text_painted_in(inspector(h), MAP_FLOOR_INERT_NOTE);

    assert_eq!(
        h.volume_arms()[0].outcome,
        None,
        "precondition: the pane is drawing a volume",
    );
    assert!(
        row_drawn(&h),
        "precondition: the Map floor row is on screen"
    );
    assert!(
        !note_drawn(&h),
        "a drawing pane must not be told its floor is going nowhere",
    );

    // Take the picture away, through the path every suspend and surface loss
    // takes. The checkbox is unchanged; what it can produce is not.
    h.gui_mut().clear_graphics_state();
    h.frames_for(3, FRAME_DT);
    assert_eq!(
        h.volume_arms()[0].outcome.as_deref(),
        Some(VOLUME_EMPTY_STATE),
        "precondition: the pane is now explaining itself instead of drawing",
    );
    assert!(row_drawn(&h), "the Map floor row is still on screen");
    assert!(
        note_drawn(&h),
        "the Map floor checkbox draws no floor and says nothing about it; the \
         sidebar painted {:?}",
        h.painted_text_strings_in(inspector(&h)),
    );
    assert!(
        h.text_painted_in(inspector(&h), VOLUME_EMPTY_STATE),
        "the note must quote the pane's own reason, not a second wording of it",
    );
}

/// The other interactive surface a 3D pane carries: the Volume Alpha editor's
/// own window. A drag inside it belongs to the editor and must not also fly the
/// orbit camera under it.
///
/// The glass-layer audit's second half. Everything else drawn over a 3D pane —
/// the colour-scale legends, the stale-image notice, the caption, the empty
/// state, the pane border — goes through a bare `egui::Painter` and allocates
/// no widget at all, so none of it can either take a click or block one. The
/// corner button and this window are the whole interactive inventory, and both
/// have to win the pointer against a pane-wide `Sense::click_and_drag`.
///
/// **Both halves.** The camera must not move while the window is open (the
/// window keeps its own drag) *and* it must move on the identical drag once the
/// window is gone (that position really is over the pane, so the first half is
/// a window that shields rather than a dead zone in the orbit). Half one alone
/// passes if the orbit never worked; half two alone passes if the window is not
/// there at all.
#[test]
fn the_open_alpha_editor_keeps_its_drag_off_the_orbit_camera() {
    let mut h = volume_pane_harness();

    /// One primary drag, pressed at `from` and released 90 points to the right —
    /// far enough that the orbit's `ORBIT_YAW_DEG_PER_POINT` is unmistakable.
    fn drag(h: &mut InputHarness, from: egui::Pos2) {
        let to = from + egui::vec2(90.0, 0.0);
        h.mouse_press(from);
        h.frames_for(1, FRAME_DT);
        h.mouse_move(to);
        h.frames_for(1, FRAME_DT);
        h.mouse_release(to);
        h.frames_for(1, FRAME_DT);
    }
    let editor_open = |h: &mut InputHarness| {
        h.gui_mut()
            .pane(1)
            .expect("pane 1 exists")
            .volume()
            .expect("pane 1 is a 3D pane")
            .alpha_editor_open
    };

    // Opened through the corner button, which is the user's only door to it.
    let button = h
        .alpha_buttons()
        .into_iter()
        .find(|&(idx, _)| idx == 1)
        .expect("the 3D pane draws its Volume alpha corner button")
        .1;
    h.mouse_click(button.center());
    h.warm_up();
    assert!(editor_open(&mut h), "precondition: the editor opened");

    let window = h
        .area_rect(egui::Id::new(("volume_alpha_editor", 1)))
        .expect("the editor window is laid out");
    let spot = window.center();
    assert!(
        h.pane_rects()[1].contains(spot),
        "precondition: the window sits over the 3D pane, so the orbit is what \
         it has to be shielding the drag from",
    );

    let before = camera_of(&mut h, 1);
    drag(&mut h, spot);
    let shielded = camera_of(&mut h, 1);
    assert_eq!(
        (shielded.yaw_deg(), shielded.pitch_deg()),
        (before.yaw_deg(), before.pitch_deg()),
        "a drag inside the Volume Alpha window orbited the pane underneath it",
    );

    // Shut it, and the very same drag reaches the pane it always could.
    h.mouse_click(button.center());
    h.warm_up();
    assert!(!editor_open(&mut h), "precondition: the editor closed");

    let before = camera_of(&mut h, 1);
    drag(&mut h, spot);
    let orbited = camera_of(&mut h, 1);
    assert_ne!(
        orbited.yaw_deg(),
        before.yaw_deg(),
        "the pane does not orbit at that position at all, so the half above \
         proved nothing",
    );
}

/// A touch **double-tap-drag** zooms a 3D pane's geography and does not also
/// spin its box.
///
/// The one gesture in the shipped inventory that already reached a 3D pane's
/// viewport before any of this, and the one that collided with the orbit.
/// `DoubleTapDragDetector` writes `MapMemory` directly for whichever pane is
/// active, whatever its render mode — so on a phone this was always the way to
/// re-cut a 3D box. But it spells the zoom with a held finger travelling up the
/// screen, and `normalize_touch_devices` turns a finger into a synthesised
/// *primary* drag: ungated, the ground zoomed and the camera turned at once,
/// from one gesture.
///
/// `suppress_drag` is the fix and it is not a new concept — it is the same
/// `MapPointerFrame::suppress_pan` a plan view already hands to
/// `Map::drag_pan_buttons`, meaning "another gesture owns this pointer". The
/// zoom half is asserted too, so a pass cannot come from the gesture being
/// dead altogether.
#[test]
fn a_double_tap_drag_zooms_a_3d_panes_ground_without_spinning_its_box() {
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    h.load_scan("KTLX");
    h.make_pane_volume(0);
    h.gui_mut()
        .set_volume_painter(Some(Arc::new(StubVolumePainter::painting())));
    h.warm_up();

    let start = h.pane_rects()[0].center();
    let before_camera = camera_of(&mut h, 0);
    let before_ground = ground_zoom(&mut h, 0);

    // Tap, then press again inside the double-tap window and drag down.
    h.touch_tap(start);
    h.touch_start(start);
    h.frame_after(0.05);
    for step in 1..=3 {
        h.touch_move(start + egui::vec2(0.0, 50.0 * step as f32));
        h.frame_after(FRAME_DT);
    }
    h.touch_end(start + egui::vec2(0.0, 150.0));
    h.frames_for(2, FRAME_DT);

    assert!(
        ground_zoom(&mut h, 0) > before_ground,
        "a double-tap-drag down must zoom the 3D pane's geography in",
    );
    assert_eq!(
        camera_of(&mut h, 0),
        before_camera,
        "the double-tap zoom also orbited the box - one gesture, two verbs",
    );
}

/// A pane nobody is touching asks for the **same box** on every frame, bit for
/// bit — so the volume it rebuilds every sealed sweep lands on the same
/// lattice, and the picture's only change is the new data.
///
/// # Why this is worth a test of its own
///
/// The region is derived, every frame, by unprojecting the pane rect's centre
/// and four edge midpoints through a fresh `walkers::Projector`. That is `f32`
/// rect arithmetic feeding `f64` geodesy, and it is *the same* arithmetic
/// `HALF_WIDTH_STEP_KM` exists to quantise — the half-width would otherwise
/// wander by metres between frames and make a new resample key out of nothing.
/// The **centre** carries no such quantiser, so if it wandered at all, every
/// frame would name a new `VolumeTarget`, every sweep would resample onto a
/// lattice a fraction of a cell from the last, and a pane left open would
/// re-shuffle its bands for ever while its owner sat and watched.
///
/// It does not wander, and that is a property of stable inputs rather than of
/// luck or of rounding: the rect and the map memory do not move when nobody
/// moves them, and the projection is a pure function of those. This test is
/// what says so, because the alternative is invisible in every other suite —
/// a wandering centre costs a rebuild, not a wrong picture, and the only
/// symptom is a warm CPU.
///
/// It is also the measurement that decided **not** to anchor the resample
/// lattice on the radar (see `rustdar_radar::voxel::horizontal_ranges_km`).
/// Anchoring's one regime with real value would be a pitch ratio of 1, where
/// snapping two boxes to a shared lattice makes the resampled field identical
/// rather than merely better aligned. That is exactly the live-rebuild steady
/// state — and this test shows the steady state already *has* an identical box,
/// so there is nothing there for anchoring to win.
#[test]
fn a_settled_pane_asks_for_one_box_for_ever() {
    let (mut h, painter) = volume_harness(StubVolumePainter::painting());
    // Past whatever the map's own entry animation does.
    h.frames_for(30, FRAME_DT);
    painter.seen.lock().expect("stub painter mutex").clear();

    h.frames_for(120, FRAME_DT);

    let seen = painter.seen.lock().expect("stub painter mutex");
    let boxes: Vec<_> = seen.iter().map(|frame| frame.target.region).collect();
    assert!(
        boxes.len() >= 100,
        "precondition: the pane must actually have been asked to paint on \
         these frames, or this test proves nothing; it was asked {} times",
        boxes.len(),
    );
    let first = boxes[0];
    assert!(
        first.is_some(),
        "precondition: the pane must have a measurable viewport, or every \
         frame agrees on `None` and the comparison below is vacuous",
    );
    let wandered = boxes.iter().filter(|b| **b != first).count();
    assert_eq!(
        wandered,
        0,
        "{wandered} of {} frames named a different box than the first with no \
         input at all. Every one of those is a fresh resample key: a new build \
         per frame, and a volume that lands on a slightly different lattice \
         every sealed sweep. The half-width is quantised against exactly this; \
         the centre is not, and relies on the projection being a pure function \
         of a rect and a map memory that nobody moved.",
        boxes.len(),
    );
    drop(seen);

    // The control, and the reason the assertion above is not vacuous: the same
    // pane, the same readback, one wheel notch. A box that never changes for
    // any reason would satisfy the stability check and mean nothing.
    painter.seen.lock().expect("stub painter mutex").clear();
    let pane_rect = h.pane_rects()[1];
    h.scroll_at(pane_rect.center(), egui::vec2(0.0, 8.0));
    h.frames_for(20, FRAME_DT);
    let after: Vec<_> = painter
        .seen
        .lock()
        .expect("stub painter mutex")
        .iter()
        .map(|frame| frame.target.region)
        .collect();
    assert!(
        after.iter().any(|b| *b != first),
        "a wheel notch on the pane left the box exactly where it was, so the \
         stability asserted above is the readback being blind rather than the \
         viewport being still",
    );
}
