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

/// How far a 3D pane's eye stands off its box, in framing radii — what the
/// zoom gesture now moves, and the only thing it moves.
fn standoff(h: &mut InputHarness, idx: usize) -> f32 {
    camera_of(h, idx).eye_distance()
}

/// The region a 3D pane has **stored**, or `None` for "the volume's own reach".
///
/// Read rather than measured, which is the whole of the change these tests are
/// about: there is no per-frame derivation left to interrogate, so what a test
/// asks for is what the pane is holding.
fn stored_region(h: &mut InputHarness, idx: usize) -> Option<crate::pane::VolumeRegion> {
    h.gui_mut()
        .pane(idx)
        .expect("a pane")
        .volume()
        .expect("a pane in the 3D render mode")
        .region
}

/// A region of a stated size about KTLX, for tests that need the stored region
/// to be a *value* rather than the `None` two panes would agree on vacuously.
fn picked_region(half_east_km: f64, half_north_km: f64) -> crate::pane::VolumeRegion {
    crate::pane::VolumeRegion::new(
        crate::pane::GeoPoint {
            lat: 35.33,
            lon: -97.28,
        },
        rustdar_radar::voxel::HalfExtentKm {
            east_km: half_east_km,
            north_km: half_north_km,
        },
    )
    .expect("a region centred on a point on Earth with a finite extent")
}

/// Give pane `idx` a picked region, as the selector will.
fn pick_region(h: &mut InputHarness, idx: usize, region: crate::pane::VolumeRegion) {
    h.gui_mut()
        .pane_mut(idx)
        .expect("a pane")
        .volume_mut()
        .expect("a pane in the 3D render mode")
        .region = Some(region);
}

/// Scrolling over the 3D pane zooms it; scrolling over another pane does
/// not.
///
/// `Input::zoom_delta` and the scroll delta are **global** — they report the
/// frame's gesture wherever on screen it happened — so the
/// `hovered() || dragged()` gate is correctness rather than politeness.
/// Without it a wheel over a map pane would zoom every 3D pane on screen.
///
/// What "zooms it" means is the pane's **eye**: the gesture divides the
/// standoff and leaves the box, the grid inside it and the ground under it
/// exactly where they were.
#[test]
fn only_a_gesture_over_the_pane_zooms_it() {
    let (mut h, _painter) = volume_harness(StubVolumePainter::painting());
    let rects = h.pane_rects();

    let before = standoff(&mut h, 1);
    h.scroll_at(rects[0].center(), egui::vec2(0.0, 200.0));
    h.frames_for(2, FRAME_DT);
    assert_eq!(
        standoff(&mut h, 1),
        before,
        "a scroll over the map pane must not dolly the 3D pane beside it",
    );

    h.scroll_at(rects[1].center(), egui::vec2(0.0, 200.0));
    h.frames_for(2, FRAME_DT);
    let after = standoff(&mut h, 1);
    assert!(
        after < before,
        "scrolling up over the 3D pane should bring its eye in: {before} -> {after}",
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

    let before = standoff(&mut h, 1);
    h.web_pinch(rects[0].center(), 80.0, 400.0, 6);
    h.frames_for(2, FRAME_DT);
    assert_eq!(
        standoff(&mut h, 1),
        before,
        "a pinch over the map pane dollied the 3D pane beside it - the \
         `hovered() || dragged()` gate on a global `zoom_delta` is gone",
    );

    // The control, so a pass cannot come from pinch being broken outright.
    let (mut h, _painter) = volume_harness(StubVolumePainter::painting());
    let rects = h.pane_rects();
    let before = standoff(&mut h, 1);
    h.web_pinch(rects[1].center(), 80.0, 400.0, 6);
    h.frames_for(2, FRAME_DT);
    assert!(
        standoff(&mut h, 1) < before,
        "control: a pinch on the 3D pane itself must bring its eye in",
    );
}

/// **The box does not move when the user zooms.** This is the acceptance test
/// for the defect, reported three times:
///
/// > The 3d viewer's region should CAP at either the size of the data in the
/// > radar scan, or the region selected if the user did that. That region (the
/// > selector OR the radar's ring) must never change. Zooming should keep the
/// > rest of the region around and merely zoom into what's already there.
///
/// Six notches rather than one, because the defect was *cumulative*: each
/// gesture frame re-derived the box from a viewport the previous frame had
/// already tightened, so a single notch understated it and a held scroll took
/// the pane from an 802 × 490 km box to 668 × 408.
///
/// Three things are asserted and each covers a different way to reintroduce it.
/// The **region** is the resample key, so a change there is the box actually
/// moving. The pane's **map memory** is what the box used to be derived from,
/// so a write there is the defect one refactor away from coming back even while
/// the region looks still. And the **standoff** is the precondition: without it
/// a pass could mean the gesture had simply stopped working, which is how this
/// shipped green twice.
///
/// The region is *picked* first rather than left at `None`, because `None ==
/// None` is a comparison that holds however badly the code behaves.
///
/// Only the standoff may move. Yaw, pitch and pivot are checked too — a
/// "helpful" reframe would plausibly nudge the pitch or re-centre the pivot,
/// and an assertion aimed only at the box would not see it.
#[test]
fn zooming_moves_the_eye_and_leaves_the_box_exactly_where_it_was() {
    let (mut h, _painter) = volume_harness(StubVolumePainter::painting());
    let rect = h.pane_rects()[1];
    let picked = picked_region(120.0, 75.0);
    pick_region(&mut h, 1, picked);
    h.frames_for(2, FRAME_DT);

    let before_ground = ground_zoom(&mut h, 1);
    let before_camera = camera_of(&mut h, 1);

    for _ in 0..6 {
        h.scroll_at(rect.center(), egui::vec2(0.0, 200.0));
        h.frames_for(2, FRAME_DT);
    }

    let after_camera = camera_of(&mut h, 1);
    assert!(
        after_camera.eye_distance() < before_camera.eye_distance(),
        "precondition: six notches must have brought the eye in, or the \
         assertions below pass because the gesture does nothing at all: {} -> {}",
        before_camera.eye_distance(),
        after_camera.eye_distance(),
    );
    assert_eq!(
        stored_region(&mut h, 1),
        Some(picked),
        "zooming re-cut the box the user picked - this is the reported defect, \
         and the caption's own figure is what the user watched it happen in",
    );
    assert_eq!(
        ground_zoom(&mut h, 1),
        before_ground,
        "the zoom wrote the pane's map memory, which is what the box used to be \
         derived from and what its plan view is still drawn with",
    );
    assert_eq!(
        (
            after_camera.yaw_deg(),
            after_camera.pitch_deg(),
            after_camera.pivot(),
        ),
        (
            before_camera.yaw_deg(),
            before_camera.pitch_deg(),
            before_camera.pivot(),
        ),
        "a zoom reframed the camera - the standoff is the only thing it may move",
    );
}

/// The other half of the same rule: an **unpicked** pane's box does not move
/// either, and the thing that must not move is the one the resampler keys on.
///
/// `None` is the ordinary state of a 3D pane and it means "the volume's own
/// reach", so this is the case the reporting user was actually in. It is a
/// weaker assertion than the picked one above — `None` cannot be re-cut into a
/// different `None` — which is exactly why the map memory is checked beside it:
/// under the old arm a `None` pane did not exist at all, because the derivation
/// filled the field in on the first drawn frame.
#[test]
fn an_unpicked_panes_box_is_the_volumes_reach_and_a_zoom_does_not_touch_it() {
    let (mut h, _painter) = volume_harness(StubVolumePainter::painting());
    let rect = h.pane_rects()[1];
    h.frames_for(4, FRAME_DT);

    assert_eq!(
        stored_region(&mut h, 1),
        None,
        "a pane nobody has aimed must ask for the volume's own reach, which is \
         the `None` only `build_voxels` can resolve",
    );
    let before_ground = ground_zoom(&mut h, 1);
    let before_standoff = standoff(&mut h, 1);

    for _ in 0..6 {
        h.scroll_at(rect.center(), egui::vec2(0.0, 200.0));
        h.frames_for(2, FRAME_DT);
    }

    assert!(
        standoff(&mut h, 1) < before_standoff,
        "precondition: the gesture must have reached the camera",
    );
    assert_eq!(
        stored_region(&mut h, 1),
        None,
        "a zoom gave an unaimed pane a box, so the pane stopped asking for the \
         whole ring the moment the user touched it",
    );
    assert_eq!(
        ground_zoom(&mut h, 1),
        before_ground,
        "the zoom wrote the pane's map memory",
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
/// dollies its eye.
///
/// The touch spelling of "right-drag pans", and the one that has to carry
/// both verbs at once: `MultiTouchInfo` reports the translation and the
/// pinch from one gesture, and a user who slides two fingers apart while
/// moving them expects both to happen. This is also the pin that one finger
/// still orbits — `normalize_touch_devices` synthesises a *primary* drag
/// from a touch, so a two-finger gesture would be read as an orbit too if
/// the cancel in `volume_pane_outcome` were dropped.
#[test]
fn a_two_finger_drag_pans_a_3d_pane_and_its_spread_dollies_the_eye() {
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
        after.eye_distance() < before_camera.eye_distance(),
        "the spread in the same gesture must bring the eye in",
    );
    assert_eq!(
        ground_zoom(&mut h, 1),
        before_ground,
        "the spread moved the pane's ground, which is the box's old derivation \
         reaching the gesture through the touch arm",
    );
}

/// A scroll covers the same ground on a 3D pane as it does on a plan view.
///
/// "The same gesture means the same thing" is the whole brief, and it is not
/// something the code can be read for: `ui_region::zoom_step` is a restatement
/// of `walkers::Map::zoom_delta`, which a 3D pane cannot reach because its map
/// is drawn off screen. So the two arms are driven through the real UI on one
/// screen and their answers compared — a restatement that drifts from walkers
/// fails here, and comparing this function against a copy of itself never
/// would.
///
/// **The two answers are in different units now, and converting between them is
/// the claim.** A plan view answers in Web Mercator zoom *levels*, where one
/// level is a factor of two of ground per point. A 3D pane answers in
/// `eye_distance`, and a perspective camera sees `2 · d · tan(fov/2)` of ground
/// at its pivot plane — linear in `d`. So the same gesture means the same thing
/// exactly when the standoff ratio is `2^-levels`, which is what
/// `ui_region::zoom_camera` computes and what this checks end to end.
///
/// The tolerance is 1e-5 rather than 1e-9 because `eye_distance` is an `f32`
/// where a zoom level is an `f64`; at the ~1.9 standoff a pane opens at, one
/// `f32` ulp is 1.2e-7 and the log2 of a ratio of two of them is about 1e-7. A
/// drifting restatement moves this by percent, not by ulps.
///
/// **One harness each, deliberately.** `smooth_scroll_delta` decays over
/// several frames, so a second notch on the same harness lands on top of the
/// first one's tail and reads as the two arms disagreeing by two thirds. The
/// first version of this test did exactly that and blamed the production code.
#[test]
fn a_scroll_moves_a_3d_pane_the_same_distance_it_moves_a_plan_view() {
    // One notch over pane `idx`, from a harness with identical geometry, zoom
    // and frame history — so the only difference between the two answers is
    // which arm read the wheel. Answered in zoom levels by both arms, the 3D
    // one through the conversion this test exists to pin.
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
        let before_ground = ground_zoom(&mut h, idx);
        let before_standoff = f64::from(standoff(&mut h, 1));
        h.scroll_at(rects[idx].center(), egui::vec2(0.0, 120.0));
        h.frames_for(1, FRAME_DT);
        if idx == 0 {
            ground_zoom(&mut h, idx) - before_ground
        } else {
            // Levels in, from the ratio the standoff moved by: `d/2` per level.
            -(f64::from(standoff(&mut h, 1)) / before_standoff).log2()
        }
    };

    let flat_step = notch(0);
    let solid_step = notch(1);
    assert!(
        flat_step > 0.0,
        "precondition: the plan view must have zoomed at all",
    );
    assert!(
        (flat_step - solid_step).abs() < 1e-5,
        "one wheel notch moved the plan view {flat_step} zoom levels and the 3D \
         pane {solid_step} - the same gesture has stopped meaning the same thing",
    );
}

/// Zooming stops at the camera's own stops, in both directions, and the box is
/// untouched at both of them.
///
/// The gesture used to be bounded by the *resampler* — below
/// `MIN_HALF_WIDTH_KM` the derived box was refused outright, so a viewport
/// zoomed one notch too far popped from 10 km straight to the 230 km fallback.
/// There is no derived box left to fall through, so the only bound is
/// `MIN_EYE_DISTANCE..=MAX_EYE_DISTANCE`, which is the honest one: what the
/// gesture runs out of is somewhere to put the eye.
///
/// Forty notches each way, far past either stop, because each one is a separate
/// opportunity for a bound to be applied once and then forgotten — and because
/// `nudge` clamps rather than refuses, so a sign error would sail past the stop
/// and come back on the far side.
#[test]
fn zooming_stops_at_the_cameras_own_stops_without_moving_the_box() {
    let (mut h, _painter) = volume_harness(StubVolumePainter::painting());
    let rect = h.pane_rects()[1];
    let picked = picked_region(120.0, 75.0);
    pick_region(&mut h, 1, picked);
    h.frames_for(2, FRAME_DT);

    for (direction, scroll) in [("in", 300.0_f32), ("out", -300.0)] {
        for _ in 0..40 {
            h.scroll_at(rect.center(), egui::vec2(0.0, scroll));
            h.frames_for(1, FRAME_DT);
            let d = standoff(&mut h, 1);
            assert!(
                (crate::pane::MIN_EYE_DISTANCE..=crate::pane::MAX_EYE_DISTANCE).contains(&d),
                "zooming {direction} put the eye at {d}, outside the camera's own range",
            );
            assert_eq!(
                stored_region(&mut h, 1),
                Some(picked),
                "zooming {direction} moved the box",
            );
        }
        let reached = standoff(&mut h, 1);
        let stop = if direction == "in" {
            crate::pane::MIN_EYE_DISTANCE
        } else {
            crate::pane::MAX_EYE_DISTANCE
        };
        assert_eq!(
            reached, stop,
            "precondition: zooming {direction} forty times must actually reach \
             the stop, or the assertions above passed without ever being tested",
        );
    }
}

// `a_scroll_that_stops_stops_re_cutting_the_box` used to stand here. It pinned
// the *settle* of a per-frame derivation — that once the wheel stopped, the box
// stopped being re-cut — which was the churn question the 1 km quantisation
// existed to answer. There is no derivation and no quantum: a scroll never
// re-cuts the box at all, during the gesture or after it, and
// `zooming_moves_the_eye_and_leaves_the_box_exactly_where_it_was` asserts the
// stronger property over the same six notches. Kept as a note rather than as a
// test that cannot fail.

/// A 3D pane's gesture is its own: the link does not carry it either way.
///
/// The **stated decision**, pinned so that it is a decision rather than an
/// accident of `PaneState::is_map` answering `false` for a 3D pane.
/// `sync_viewports` is defined over plan views — panes with a raster to
/// dispatch, donate and synchronise — and a 3D pane is neither a source nor a
/// target of it.
///
/// The cost of the other choice is what settles it: a 3D pane as a sync
/// *target* would take a neighbour's wheel as a camera move, so a plan view
/// zoomed to the street would fly the 3D pane's eye to its stop with nobody
/// having touched it. Changing it is a one-word change (`is_map` to a predicate
/// about having a viewport at all) and wants its own review, not this one.
#[test]
fn a_3d_panes_gesture_is_not_carried_by_the_link() {
    let (mut h, _painter) = volume_harness(StubVolumePainter::painting());
    let rects = h.pane_rects();

    // The 3D pane's own gesture does not reach the plan view beside it.
    let flat_before = ground_zoom(&mut h, 0);
    let solid_before = standoff(&mut h, 1);
    h.scroll_at(rects[1].center(), egui::vec2(0.0, 200.0));
    h.frames_for(4, FRAME_DT);
    assert!(
        standoff(&mut h, 1) != solid_before,
        "precondition: the 3D pane must have taken the gesture",
    );
    assert_eq!(
        ground_zoom(&mut h, 0),
        flat_before,
        "a 3D pane drove the linked plan view's viewport",
    );

    // And the plan view's does not reach the 3D pane.
    let solid_before = standoff(&mut h, 1);
    let solid_region = stored_region(&mut h, 1);
    h.scroll_at(rects[0].center(), egui::vec2(0.0, 200.0));
    h.frames_for(4, FRAME_DT);
    assert!(
        ground_zoom(&mut h, 0) != flat_before,
        "precondition: the plan view must have zoomed",
    );
    assert_eq!(
        standoff(&mut h, 1),
        solid_before,
        "the link carried a plan view's zoom into a 3D pane's camera",
    );
    assert_eq!(
        stored_region(&mut h, 1),
        solid_region,
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
/// km reflectivity reach earns and the 3.59 km cells it costs — rather than
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
            .any(|l| l.contains("920 km box") && l.contains("3.59 km/cell")),
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

/// Every legend a 3D pane can paint, with a preference set that paints it.
///
/// **Every product, not the nine the volume pipeline can sample.** A 3D pane
/// draws its legend and its Volume Alpha button from `Gui::draw_volume_glass`
/// and `volume_alpha_editor::editor_ui`, both of which run whatever the arm
/// decided — so a pane switched to VIL shows `no vertical structure` in the
/// middle and a full VIL colour bar down the right edge, button and all.
///
/// The unit crossing is off each enum's own `ALL`, so a unit added later is
/// exercised without anyone remembering to add it here, and it is collapsed by
/// what each product's legend *says*: 48 preference sets over 17 products are
/// 816 legends, of which 31 differ in a tick or a title. A preference no
/// product's bar reads produces a legend already in the list and costs nothing;
/// one that changes a tick or a title is a new legend and gets its own frame.
fn every_legend_a_volume_pane_can_paint() -> Vec<(RadarProduct, UserPreferences)> {
    use rustdar_units::{HailSizeUnit, HeightUnit, PrecipRateUnit, SpeedUnit};

    let mut said: Vec<(RadarProduct, Vec<String>, &'static str)> = Vec::new();
    let mut legends = Vec::new();
    for &product in RadarProduct::all() {
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
                        // Keyed by the product too, not by the marks alone:
                        // SRV's bar is velocity's, character for character, and
                        // a list that collapsed them would never run the arm
                        // for a product at all.
                        let says = (
                            product,
                            pane_render::legend_ticks(product, &prefs),
                            product.unit_label(&prefs),
                        );
                        if !said.contains(&says) {
                            said.push(says);
                            legends.push((product, prefs));
                        }
                    }
                }
            }
        }
    }
    legends
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
/// The last block of each round is what stops it being vacuous: the *old* rect
/// is reconstructed from `pane_rect` and required to collide, so a legend that
/// stopped being drawn — or a title that moved off the corner by itself —
/// fails here rather than passing an assertion about an empty set.
///
/// # Why it runs every legend and not the one on screen
///
/// It ran reflectivity in the default units, and reflectivity's bar ends at
/// `95`, which lays out 12.4 points wide in the 20 points of clear glass
/// between the top tick and the button's left edge. Correlation coefficient's
/// ends at `0.98` — 21.3 points, and a gradient bar's last stop centres on the
/// very top of the bar, level with the button. `0.98` printed through
/// `Volume alpha` on every 3D pane showing ρHV in every unit preference, the
/// default included, with this test green the whole time. Velocity in km/h and
/// differential phase (`130` and `345`, 18.6) cleared it by 1.4 points.
#[test]
fn the_colour_scale_does_not_print_through_the_volume_alpha_button() {
    let (mut h, _painter) = ground_witness_harness();
    h.make_pane_volume(0);
    h.frames_for(2, FRAME_DT);
    assert!(
        !h.gui_mut().mirror_source_rects().is_empty(),
        "precondition: the 3D arm never got as far as drawing a pane",
    );

    let legends = every_legend_a_volume_pane_can_paint();
    assert!(
        legends.len() > RadarProduct::all().len(),
        "precondition: {} legends for {} products means the unit crossing \
         collapsed to nothing and only the default preferences are being \
         drawn",
        legends.len(),
        RadarProduct::all().len(),
    );

    for (product, prefs) in legends {
        // On every pane, not just the 3D one: the layer links default on and
        // `propagate_layer_sync` copies the *active* pane's product to the
        // rest, so writing it to pane 0 alone is undone on the next frame.
        for pane in h.gui_mut().panes_mut() {
            pane.selected_product = product;
        }
        h.gui_mut().preferences = prefs.clone();
        h.frames_for(2, FRAME_DT);
        // Named by the two things that pick a legend out of the list: what it
        // is a scale of, and what it is labelled in.
        let what = format!("{} as {}", product.name(), product.unit_label(&prefs));

        let pane_rect = h.pane_rects()[0];
        let (_, button) = *h
            .alpha_buttons()
            .iter()
            .find(|(idx, _)| *idx == 0)
            .unwrap_or_else(|| panic!("the 3D pane drew no Volume Alpha button for {what}"));
        assert!(
            pane_rect.contains_rect(button),
            "the button {button:?} left the pane {pane_rect:?} entirely, \
             showing {what}",
        );

        // The pane is landscape, so the panel-wide orientation puts the bars on
        // the right edge — the same edge the button hangs off.
        let witness = product.unit_label(&prefs);
        let title: Vec<egui::Rect> = h
            .painted_text_rects()
            .into_iter()
            .filter(|(rect, text)| text == witness && pane_rect.contains_rect(*rect))
            .map(|(rect, _)| rect)
            .collect();
        // More than one run: `draw_shadowed_text` lays the title down twice,
        // the outline and then the text. Both are on screen, so both count.
        assert!(
            !title.is_empty(),
            "precondition: the pane drew no {witness} at all for {what}, so the \
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
            "text is printing through the Volume Alpha button at {button:?} \
             showing {what}: {through:?}",
        );

        // …and the corner the button used to stand in really is the one the
        // title is in, so the guard above has teeth.
        let old_button = egui::Rect::from_min_size(
            pane_rect.right_top() + egui::vec2(-(88.0 + 8.0), 8.0),
            egui::vec2(88.0, 20.0),
        );
        assert!(
            title.iter().any(|rect| old_button.intersects(*rect)),
            "the legend's title at {title:?} no longer reaches the pane's \
             top-right corner {old_button:?} showing {what}, so this test would \
             pass with the button put back there",
        );
    }
}

/// The Volume Alpha button is inside the pane it belongs to at **every width a
/// pane can reach** — not only on the one full-width pane the collision test
/// above runs.
///
/// # What the collision test could not see
///
/// It asserts `pane_rect.contains_rect(button)` on a single 1400-point pane,
/// where the button's left edge stands over 1200 points inside the pane's and
/// containment was never in question. The button hangs off
/// `color_scale_free_rect`'s top-right corner, so that slack is
///
/// ```text
/// slack(w) = w - gutter - (button width + margin)
/// ```
///
/// and it goes negative on a narrow pane. Drawn there anyway, the half that
/// hung over the pane's left edge was cut by the child ui's clip rect
/// (`ui_map.rs`, `child_ui.set_clip_rect(pane_rect)`) — a sheared control,
/// which reads as a rendering fault rather than as a layout out of room.
///
/// # Why it drags a divider rather than shrinking the window
///
/// Because that is how a pane gets narrow while its legend stays the size it
/// is. The gutter is measured text and does not shrink with the pane, and the
/// bars' orientation is keyed on the whole *panel* with hysteresis
/// (`ColorScaleOrientation`), so a window shrunk far enough to narrow a pane
/// would flip the bars to the bottom edge on the way and stop exercising the
/// gutter at all. A divider drag moves one column and leaves the panel alone,
/// which is exactly the user gesture that produces the widths this is about.
///
/// # Why `needed` is read off the frame and not computed
///
/// The width at which the old placement ran out of pane is `gutter + 96`, and
/// the gutter is what the legend's own text lays out to: 52.4 points for
/// spectrum width, 67.5 for precipitation rate in mm/hr, 60 more per stacked
/// overlay legend, and different again in another font. Restating that here
/// would be a second copy of `color_scale_gutter` that could agree with itself
/// while both were wrong. One reading of the button at the wide end fixes the
/// whole line, because `slack` moves point for point with the pane's width.
///
/// # Both orientations, because the room runs out in two different ways
///
/// Landscape puts the bars on the right edge, where the gutter is what eats the
/// button's room, and a 900-point panel showing reflectivity needs 151.1 points
/// of pane against the 135 `MIN_RATIO = 0.15` lets a column reach. Portrait
/// puts them along the bottom, where the free rect is as wide as the pane and
/// the button needs its own 96 points against the 64.8 the same floor allows.
#[test]
fn the_volume_alpha_button_stays_inside_its_pane_at_every_width() {
    /// One pointer step of the drag, points.
    ///
    /// Small deliberately: `PaneLayout`'s `drag_divider` refuses a step that
    /// would take a column under `MIN_RATIO` *whole* rather than clamping it to
    /// the floor, so the sweep stops within one step of `MIN_RATIO` and a
    /// coarse drag would stop well short of the narrow end this is about.
    const STEP: f32 = 3.0;

    /// Pane `idx`'s Volume Alpha button on the last frame, if it drew one.
    fn button_on(h: &InputHarness, idx: usize) -> Option<egui::Rect> {
        h.alpha_buttons()
            .into_iter()
            .find(|&(i, _)| i == idx)
            .map(|(_, rect)| rect)
    }

    /// Drag the column divider from the even split down to the `MIN_RATIO`
    /// floor, checking every button every pane draws on the way.
    fn sweep(h: &mut InputHarness, what: &str) {
        let wide_pane = h.pane_rects()[0];
        let wide_button = button_on(h, 0).unwrap_or_else(|| {
            panic!(
                "precondition: the {what} sweep's widest pane {wide_pane:?} \
                 drew no Volume Alpha button, so there is no wide end to \
                 measure the narrow one against"
            )
        });
        let needed = wide_pane.width() - (wide_button.left() - wide_pane.left());

        let panel = h.map_panel_rect();
        let y = panel.center().y;
        let mut x = wide_pane.right();
        h.mouse_press(egui::pos2(x, y));
        h.frames_for(1, FRAME_DT);

        let mut narrowest = wide_pane.width();
        let mut narrowest_drawn = wide_pane.width();
        while x - STEP > panel.left() {
            x -= STEP;
            h.mouse_move(egui::pos2(x, y));
            // **Two** frames, and the second one is what makes the two rects
            // below the same frame's. `PaneLayout::handle_dividers` runs after
            // the pane loop — deliberately, so a divider outranks a map pan in
            // the overlap — so the frame that moves a ratio has already drawn
            // its panes on the previous one, while `pane_rects` reads the live
            // layout. Comparing across that seam measures a button against a
            // pane it was never drawn in, in the direction that invents
            // failures on the pane being narrowed.
            h.frames_for(2, FRAME_DT);
            // Every pane, not only the one being narrowed: the other end of
            // the divider is a pane too, and a rule that held for one width
            // and not the other would be caught here.
            for (idx, pane_rect) in h.pane_rects().into_iter().enumerate() {
                let Some(button) = button_on(h, idx) else {
                    continue;
                };
                assert!(
                    pane_rect.contains_rect(button),
                    "on the {what} sweep the Volume Alpha button {button:?} is \
                     not inside pane {idx} {pane_rect:?}, {:.1} points wide: \
                     the pane's child ui clips to its own rect, so what the \
                     user is shown there is half a button",
                    pane_rect.width(),
                );
            }
            let width = h.pane_rects()[0].width();
            narrowest = narrowest.min(width);
            if button_on(h, 0).is_some() {
                narrowest_drawn = narrowest_drawn.min(width);
            }
        }
        h.mouse_release(egui::pos2(x, y));
        h.frames_for(1, FRAME_DT);

        // The device that stops the loop above passing vacuously: it must have
        // visited a width the old placement could not have survived.
        assert!(
            narrowest < needed,
            "the {what} sweep bottomed out at a {narrowest:.1}-point pane and \
             the old placement only left the pane below {needed:.1}, so it \
             never reached a width this test is about",
        );
        // …and the other half of vacuous, which is a button withheld
        // everywhere. It is drawn at every width with the room for it, which
        // the sweep can only see to within one sample either side of the
        // boundary.
        assert!(
            narrowest_drawn < needed + 2.0 * STEP,
            "the narrowest {what} pane that drew a button was \
             {narrowest_drawn:.1} points against the {needed:.1} it has the \
             room at, so the button is being withheld from panes that can hold \
             it",
        );
    }

    // Landscape: 700 points tall against 900 wide is well under
    // `COLOR_SCALE_HORIZONTAL_EXIT`, so the bars stand on the right edge —
    // the same edge the button hangs off, and the case where the gutter is
    // what the button runs out of room against.
    let mut landscape = InputHarness::with_screen(egui::vec2(900.0, 700.0));
    landscape.set_pane_count(2);
    // Explicitly, because the gutter is the whole subject: a pane with the
    // layer off reserves nothing and the free rect is the pane.
    landscape
        .gui_mut()
        .enable_overlay_for_test(OverlayKind::ColorScale);
    landscape.make_pane_volume(0);
    landscape.make_pane_volume(1);
    landscape.frames_for(2, FRAME_DT);
    sweep(&mut landscape, "landscape");

    // Portrait: a 432x936 phone window, the one `clear_of_bottom_chrome` is
    // written against. The bars go along the bottom, so the free rect is as
    // wide as the pane and the button runs out of room against the pane
    // itself.
    let mut portrait = InputHarness::with_screen(egui::vec2(432.0, 936.0));
    portrait.set_pane_count(2);
    portrait
        .gui_mut()
        .enable_overlay_for_test(OverlayKind::ColorScale);
    portrait.make_pane_volume(0);
    portrait.make_pane_volume(1);
    portrait.frames_for(2, FRAME_DT);
    sweep(&mut portrait, "portrait");
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

/// **The box a 3D pane resamples does not follow its viewport**, through the
/// real render arm.
///
/// The inverse of the test that used to stand here, and it is the same
/// end-to-end route: between the pane's map memory and the field the resampler
/// is keyed on sits the whole arm — the floor strip, the `mem::take`n pane, the
/// volume branch — and any one of them writing the region is the reported defect
/// back again.
///
/// The viewport is moved **directly**, by four zoom levels on the pane's own
/// `MapMemory`, rather than by a gesture. That is deliberate: the gesture is
/// pinned elsewhere, and what this asks is the stronger question — if the
/// viewport moves for *any* reason at all, including a window resize or a
/// divider drag that no gesture test can reach, does the box move with it? Four
/// levels is a 16× change in the ground the pane shows, which is far more than
/// the 1.2× that took the reported session from Chattanooga to Dalton.
#[test]
fn a_3d_panes_box_does_not_follow_its_own_viewport() {
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    h.load_scan("KTLX");
    h.make_pane_volume(0);
    h.warm_up();

    // A picked region, so that "the box did not move" is a statement about a
    // value rather than two `None`s agreeing with each other.
    let picked = picked_region(120.0, 75.0);
    pick_region(&mut h, 0, picked);
    h.warm_up();
    let before_camera = camera_of(&mut h, 0);

    // Zoomed in four levels, the way a user frames a storm on the map.
    h.gui_mut()
        .pane_mut(0)
        .expect("pane 0")
        .map_memory
        .set_zoom(11.0)
        .expect("11 is inside walkers' range");
    h.warm_up();

    assert_eq!(
        stored_region(&mut h, 0),
        Some(picked),
        "the pane's viewport moved 16x and took its box with it - the region is \
         being derived from the viewport again",
    );
    assert_eq!(
        camera_of(&mut h, 0),
        before_camera,
        "moving the viewport moved the camera, so a divider drag or a window \
         resize now reframes a 3D pane the user had aimed",
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
    // A picked region, so the frames below are compared against a value: an
    // unpicked pane names `None` on every frame, which every implementation of
    // this agrees on however badly it behaves.
    pick_region(&mut h, 1, picked_region(120.0, 75.0));
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
        "precondition: the pane must be carrying the picked region, or every \
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
    // pane, the same readback, a **different picked region**. A field the arm
    // had stopped reading at all would satisfy the stability check above and
    // mean nothing.
    //
    // The control used to be a wheel notch, back when a notch re-cut the box.
    // It cannot be one now — the whole point is that no gesture moves this — so
    // the control is the one thing that legitimately does: the user picking
    // somewhere else.
    painter.seen.lock().expect("stub painter mutex").clear();
    let repicked = picked_region(45.0, 45.0);
    pick_region(&mut h, 1, repicked);
    h.frames_for(20, FRAME_DT);
    let after: Vec<_> = painter
        .seen
        .lock()
        .expect("stub painter mutex")
        .iter()
        .map(|frame| frame.target.region)
        .collect();
    assert!(
        after.contains(&Some(repicked)),
        "picking a new region never reached the painter, so the stability \
         asserted above is the readback being blind rather than the box being \
         still",
    );
}
