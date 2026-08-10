use super::*;
use crate::input_harness::InputHarness;
use crate::pane::PaneKind;
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

/// The pane names the **published stamp**, not the plan view's own time.
///
/// The two differ constantly: `scan_info.timestamp` is the volume's start
/// and freezes for the whole flight, while the stamp advances on every
/// sealed sweep. A target built from the wrong one would ask the host for
/// a volume it does not have.
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
    // On every pane, not just the 3D one: `sync_layers` defaults on and
    // propagates the *active* pane's product to the rest, so writing it to
    // pane 1 alone is undone on the next frame by pane 0.
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
        // Every pane: `sync_layers` propagates the active pane's product.
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
    h.gui_mut().request_pane_kind(1, PaneKind::Map);
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
    h.gui_mut().request_pane_kind(0, PaneKind::CrossSection);
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

/// The reset returns the pivot and the region, not only the angles.
///
/// Through `reset_volume_view`, which is what the button calls — a test that
/// restated the assignments would pass whatever the button actually did.
///
/// Leaving the pivot out is the easy mistake and the one that matters: a
/// pane panned to its clamp and then reset would visibly change angle and
/// still be looking at the corner of the box, which reads as a reset that
/// half-worked.
#[test]
fn the_reset_returns_the_pivot_and_the_region_as_well_as_the_angles() {
    let mut volume = crate::pane::VolumePane::default();
    volume.camera.nudge(crate::pane::OrbitDelta {
        yaw_deg: 40.0,
        pitch_deg: -15.0,
        zoom_factor: 1.4,
        pan: [0.6, -0.4, 0.3],
    });
    volume.camera.set_vertical_exaggeration(9.0);
    volume.region = crate::pane::VolumeRegion::new(
        crate::pane::GeoPoint {
            lat: 35.3,
            lon: -97.3,
        },
        25.0,
    );
    volume.source_pane = Some(0);
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
    assert_eq!(volume.region, None, "the region must come back too");
    assert_eq!(
        volume.source_pane, None,
        "and its provenance, or the next drag on that map re-aims this pane",
    );
}

/// A region change invalidates the grid; a camera change does not.
///
/// This is the line between the two halves of the feature — the region
/// changes what is *sampled*, the camera only how it is *drawn* — and it is
/// the one that costs 155 ms on the frame thread when it is drawn in the
/// wrong place. Orbiting, panning or exaggerating must not rebuild.
#[test]
fn a_region_change_rebuilds_the_grid_and_a_camera_change_does_not() {
    let mut h = volume_pane_harness();
    h.frames_for(2, FRAME_DT);
    // Settle: the pane has asked for its grid and been told it has one.
    let target = h
        .last_actions()
        .iter()
        .find_map(|a| match a {
            GuiAction::PrepareVolume { target, .. } => Some(target.clone()),
            _ => None,
        })
        .expect("a build was asked for");
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
        "precondition: a settled pane asks for nothing",
    );

    // Moving the camera every way there is.
    {
        let volume = h
            .gui_mut()
            .pane_mut(1)
            .expect("pane 1")
            .volume_mut()
            .expect("a 3D pane");
        volume.camera.nudge(crate::pane::OrbitDelta {
            yaw_deg: 30.0,
            pitch_deg: 10.0,
            zoom_factor: 1.5,
            pan: [0.5, 0.5, 0.5],
        });
        volume.camera.set_vertical_exaggeration(11.0);
    }
    h.frames_for(2, FRAME_DT);
    assert!(
        !h.last_actions()
            .iter()
            .any(|a| matches!(a, GuiAction::PrepareVolume { .. })),
        "orbiting, panning and exaggerating must all redraw from the grid in hand",
    );

    // Changing the region.
    h.gui_mut()
        .pane_mut(1)
        .expect("pane 1")
        .volume_mut()
        .expect("a 3D pane")
        .region = crate::pane::VolumeRegion::new(
        crate::pane::GeoPoint {
            lat: 35.3,
            lon: -97.3,
        },
        25.0,
    );
    h.frames_for(2, FRAME_DT);
    let asked = h
        .last_actions()
        .iter()
        .find_map(|a| match a {
            GuiAction::PrepareVolume { target, .. } => Some(target.clone()),
            _ => None,
        })
        .expect("a new region must trigger a rebuild");
    assert_eq!(
        asked.region.map(|r| r.half_width_km()),
        Some(25.0),
        "the rebuild must be for the region that was picked",
    );
}

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

// --- The region drag, end to end ---------------------------------------

/// Arming the mode and dragging on a map opens a 3D pane aimed at the ground
/// that was dragged.
///
/// The whole gesture through the shipped path: menu state, press, drag,
/// release, deferred apply. Everything below picks at one part of it; this is
/// the one that proves the parts are joined up.
#[test]
fn dragging_on_an_armed_map_aims_a_3d_pane_at_that_ground() {
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    h.load_scan("KTLX");
    h.gui_mut().set_region_arm_for_test(true);
    h.frames_for(1, FRAME_DT);

    let rect = h.pane_rects()[0];
    drag_region(
        &mut h,
        rect.center(),
        rect.center() + egui::vec2(120.0, 0.0),
    );

    assert_eq!(
        h.pane_kinds().len(),
        2,
        "a drag with room in the layout must open a pane beside the map",
    );
    assert_eq!(h.pane_kinds()[1], PaneKind::Volume);
    let volume = h
        .gui_mut()
        .pane(1)
        .expect("the new pane")
        .volume()
        .expect("a 3D pane")
        .clone();
    let region = volume.region.expect("aimed at a region");
    assert!(
        region.half_width_km() >= rustdar_radar::voxel::MIN_HALF_WIDTH_KM,
        "the committed region must be one the resampler will honour: {}",
        region.half_width_km(),
    );
    assert_eq!(
        volume.source_pane,
        Some(0),
        "the pane must remember which map it was aimed from",
    );
}

/// A map pane with the region mode already armed.
fn armed_map() -> InputHarness {
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    h.load_scan("KTLX");
    h.gui_mut().set_region_arm_for_test(true);
    h.frames_for(1, FRAME_DT);
    h
}

/// The region the 3D pane a drag opened is aimed at.
fn aimed_region(h: &mut InputHarness) -> crate::pane::VolumeRegion {
    h.gui_mut()
        .pane(1)
        .expect("the new pane")
        .volume()
        .expect("a 3D pane")
        .region
        .expect("aimed at a region")
}

/// **The committed region is centred on the ground the press landed on.**
///
/// Every other test of this gesture presses at `rect.center()` — which is
/// also the one point at which reading the pane's centre instead of the
/// pointer cannot be told apart from reading the pointer. A regression there
/// would centre every user-dragged region on the middle of the map with the
/// whole suite green, and the symptom is a box near where it was drawn rather
/// than nowhere, which is exactly the kind of wrongness that gets lived with.
///
/// Pinned against the gesture's own ruler rather than against a projection
/// this crate would have to re-derive: a drag from P to Q and a drag from Q to
/// P commit two boxes of the same size, and their centres are exactly that
/// size apart. Anchoring both presses at the pane's centre makes the
/// separation zero.
///
/// Two harnesses rather than two drags on one, because the first commit grows
/// the layout and moves the map's rect out from under the second press.
#[test]
fn the_committed_region_is_centred_on_the_ground_the_press_landed_on() {
    let mut first = armed_map();
    let rect = first.pane_rects()[0];
    // Neither end is the pane's centre, and neither shares a coordinate with
    // it: a substitution has to be wrong in both latitude and longitude.
    let p = rect.center() + egui::vec2(-40.0, -25.0);
    let q = rect.center() + egui::vec2(20.0, 18.0);

    drag_region(&mut first, p, q);
    let from_p = aimed_region(&mut first);

    let mut second = armed_map();
    drag_region(&mut second, q, p);
    let from_q = aimed_region(&mut second);

    assert!(
        from_p.half_width_km() > rustdar_radar::voxel::MIN_HALF_WIDTH_KM
            && from_p.half_width_km() < rustdar_radar::voxel::MAX_HALF_WIDTH_KM,
        "precondition: the box must be strictly inside the resampler's clamp, \
             or its size is not a ruler: {} km",
        from_p.half_width_km(),
    );

    // Screen `y` runs down and `x` runs east, so the press up and to the left
    // is the one further north and further west.
    assert!(
        from_p.centre().lat > from_q.centre().lat,
        "the press higher up the pane must aim further north: {:?} vs {:?}",
        from_p.centre(),
        from_q.centre(),
    );
    assert!(
        from_p.centre().lon < from_q.centre().lon,
        "the press further left must aim further west: {:?} vs {:?}",
        from_p.centre(),
        from_q.centre(),
    );

    // And by exactly the ground the drag measured for itself.
    let mut apart = crate::ui_region::RegionDrag::begin(0, from_p.centre())
        .expect("a centre the projector placed on Earth");
    apart.extend_to(from_q.centre());
    assert!(
        (apart.half_width_km() - from_p.half_width_km()).abs() < 1e-6,
        "the two centres must be the box's own width apart — {} km against a \
             box of {} km. A press read at the pane's centre puts both boxes in \
             the same place.",
        apart.half_width_km(),
        from_p.half_width_km(),
    );
}

/// A commit disarms the mode; a discarded mis-drag leaves it armed.
///
/// The same shape the section draw has always had, and for the same two
/// reasons. Once a box is committed the mode's job is done — leaving it on
/// turns the user's next pan into a second box, which is how a pane they
/// never asked for appears. But a mis-click while armed must cost nothing:
/// a stray tap is how a user checks which pane they are on, and disarming
/// there would silently throw away the intent they just expressed.
#[test]
fn a_commit_disarms_the_mode_and_a_mis_drag_leaves_it_armed() {
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    h.load_scan("KTLX");
    h.gui_mut().set_region_arm_for_test(true);
    h.frames_for(1, FRAME_DT);

    let rect = h.pane_rects()[0];
    // A press and release with no movement at all: the mis-click, first,
    // while the mode is still fresh.
    drag_region(&mut h, rect.center(), rect.center());
    assert!(
        h.gui_mut().region_arm_for_test(),
        "a discarded drag must leave the mode armed",
    );

    drag_region(
        &mut h,
        rect.center(),
        rect.center() + egui::vec2(120.0, 0.0),
    );
    assert!(
        !h.gui_mut().region_arm_for_test(),
        "a commit must disarm the mode: its job is done",
    );

    // And the consequence the disarm exists for: the next drag is a pan
    // again, not a second box. The committed pane keeps the region it was
    // aimed at and no further pane appears.
    let kinds = h.pane_kinds();
    let aimed = aimed_region(&mut h);
    drag_region(
        &mut h,
        rect.center(),
        rect.center() + egui::vec2(-90.0, 40.0),
    );
    assert_eq!(
        h.pane_kinds(),
        kinds,
        "a drag after the disarm must not grow or convert anything",
    );
    assert_eq!(
        aimed_region(&mut h),
        aimed,
        "a drag after the disarm must not re-aim the pane",
    );
}

/// A mis-drag commits nothing — it does not open a pane and does not re-aim
/// one.
#[test]
fn a_mis_drag_leaves_the_layout_and_the_region_alone() {
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    h.load_scan("KTLX");
    h.gui_mut().set_region_arm_for_test(true);
    h.frames_for(1, FRAME_DT);

    let rect = h.pane_rects()[0];
    let before = h.pane_kinds();
    // Two points apart, which at any plausible zoom is far under 10 km.
    drag_region(&mut h, rect.center(), rect.center() + egui::vec2(2.0, 0.0));

    assert_eq!(
        h.pane_kinds(),
        before,
        "a drag below the resampler's minimum must change nothing",
    );
}

/// **The anchor is the ground, not the pixel.**
///
/// Pan is suppressed while armed but zoom is not, so a wheel notch mid-drag
/// moves every pixel of the map while the ground stays where it is. A pixel
/// anchor would silently re-aim the box to whatever is now under the old
/// coordinate; a geographic one cannot.
#[test]
fn a_mid_drag_zoom_does_not_move_the_region_it_is_anchored_to() {
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    h.load_scan("KTLX");
    h.gui_mut().set_region_arm_for_test(true);
    h.frames_for(1, FRAME_DT);
    let rect = h.pane_rects()[0];

    // A drag with no zoom in it, for the baseline.
    drag_region(
        &mut h,
        rect.center(),
        rect.center() + egui::vec2(120.0, 0.0),
    );
    let plain = h
        .gui_mut()
        .pane(1)
        .expect("the new pane")
        .volume()
        .expect("a 3D pane")
        .region
        .expect("aimed");

    // The same drag, with the map zoomed under it between press and release.
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    h.load_scan("KTLX");
    h.gui_mut().set_region_arm_for_test(true);
    h.frames_for(1, FRAME_DT);
    h.mouse_press(rect.center());
    h.frames_for(1, FRAME_DT);
    h.scroll_at(rect.center(), egui::vec2(0.0, 40.0));
    h.frames_for(2, FRAME_DT);
    h.mouse_move(rect.center() + egui::vec2(120.0, 0.0));
    h.frames_for(1, FRAME_DT);
    h.mouse_release(rect.center() + egui::vec2(120.0, 0.0));
    h.frames_for(2, FRAME_DT);
    let zoomed = h
        .gui_mut()
        .pane(1)
        .expect("the new pane")
        .volume()
        .expect("a 3D pane")
        .region
        .expect("aimed");

    assert!(
        (plain.centre().lat - zoomed.centre().lat).abs() < 1e-9
            && (plain.centre().lon - zoomed.centre().lon).abs() < 1e-9,
        "the centre is the ground the press landed on and a zoom must not move it: \
             {:?} vs {:?}",
        plain.centre(),
        zoomed.centre(),
    );
}

/// While armed, the map does not pan and a click does not switch site.
///
/// Both are unconditional — from the moment the mode is on, not from the
/// moment a drag is recognised — because a press that will become a region
/// drag is indistinguishable from one that will become a pan until the
/// pointer moves, and by then the map has slid under the anchor.
#[test]
fn arming_the_mode_takes_the_drag_and_the_click_away_from_the_map() {
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    h.load_scan("KTLX");
    h.frames_for(1, FRAME_DT);
    assert!(
        !h.frame().resolved.suppress_pan,
        "precondition: an unarmed map pans normally",
    );

    h.gui_mut().set_region_arm_for_test(true);
    h.frames_for(1, FRAME_DT);
    assert!(
        h.frame().resolved.suppress_pan,
        "arming the mode must take the pan away at once, before any drag",
    );

    let rect = h.pane_rects()[0];
    h.mouse_click(rect.center());
    h.frames_for(2, FRAME_DT);
    assert!(
        !h.last_actions()
            .iter()
            .any(|a| matches!(a, GuiAction::SwitchRadarSite { .. })),
        "a press while armed is a region gesture, not a click on the map",
    );
}

/// A committed region is drawn on the map it came from, and only on that
/// one.
///
/// A 3D pane whose box is invisible on the map is one the user cannot tell
/// the provenance of — "where is this volume from" has no answer on screen.
/// Drawing it on *every* map would be worse than not drawing it: two panes on
/// different sites would each claim the other's box.
#[test]
fn a_committed_region_is_drawn_on_the_map_it_came_from() {
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    h.set_pane_count(3);
    h.load_scan("KTLX");
    h.gui_mut().set_region_arm_for_test(true);
    h.frames_for(1, FRAME_DT);

    let source = h.pane_rects()[0];
    drag_region(
        &mut h,
        source.center(),
        source.center() + egui::vec2(120.0, 0.0),
    );
    h.gui_mut().set_region_arm_for_test(false);
    h.frames_for(2, FRAME_DT);

    // A stroked square whose two sides are within a point of each other,
    // sitting inside the source pane. Classified by geometry rather than by
    // colour, the way `color_scale_strips` classifies its bars — and
    // centred clear of the floating chrome, whose square icon buttons and
    // checkboxes would otherwise count as regions on whichever pane they
    // float over. Centre-based, like the pane test itself: a region box
    // that merely runs *under* a panel's edge is still the map's.
    let chrome = [
        h.status_bar().rect,
        h.timeline().rect,
        h.layers_panel_rect().unwrap_or(egui::Rect::NOTHING),
    ];
    let square_in = move |h: &mut InputHarness, pane: egui::Rect| {
        h.painted_rects()
            .iter()
            .filter(|r| {
                pane.contains(r.center())
                    && chrome.iter().all(|c| !c.contains(r.center()))
                    && r.width() > 8.0
                    && (r.width() - r.height()).abs() < 1.0
            })
            .count()
    };
    let others: Vec<egui::Rect> = h.pane_rects()[1..].to_vec();
    assert!(
        square_in(&mut h, source) > 0,
        "the region must be drawn on the map it was dragged on",
    );
    for (idx, rect) in others.iter().enumerate() {
        // Pane 1 became the 3D view; pane 2 is another map and must be clean.
        if h.pane_kinds()[idx + 1] != PaneKind::Map {
            continue;
        }
        assert_eq!(
            square_in(&mut h, *rect),
            0,
            "a map that did not produce the region must not draw it",
        );
    }
}

/// Press, drag and release on a map pane, then let the deferred apply run.
fn drag_region(h: &mut InputHarness, from: egui::Pos2, to: egui::Pos2) {
    h.mouse_press(from);
    h.frames_for(1, FRAME_DT);
    h.mouse_move(to);
    h.frames_for(1, FRAME_DT);
    h.mouse_release(to);
    // Two frames: the commit is recorded on the release frame and applied
    // after that frame's pane loop, so the pane only reads as changed on the
    // next one.
    h.frames_for(2, FRAME_DT);
}

/// While armed, no click reaches the map's own handlers at all.
///
/// Asserted on `overlay_click_pos` rather than on a downstream action,
/// because that field is the *convention*: every map click handler consumes
/// it, so nulling it is what takes the click away from all of them at once
/// — including the ones added after this feature. A test that only checked
/// that no site was switched would pass with the gate removed, because the
/// radar-sites overlay is off by default.
#[test]
fn while_armed_no_click_reaches_the_maps_own_handlers() {
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    h.load_scan("KTLX");
    let rect = h.pane_rects()[0];

    // Press and release on separate frames, as a pointer really does: egui
    // reports the click on the frame the button comes back up.
    h.mouse_press(rect.center());
    h.frames_for(1, FRAME_DT);
    h.mouse_release(rect.center());
    let unarmed = h.frame();
    assert!(
        unarmed.resolved.overlay_click_pos.is_some(),
        "precondition: an unarmed map delivers its clicks",
    );

    h.gui_mut().set_region_arm_for_test(true);
    h.frames_for(1, FRAME_DT);
    h.mouse_press(rect.center());
    h.frames_for(1, FRAME_DT);
    h.mouse_release(rect.center());
    let armed = h.frame();
    assert_eq!(
        armed.resolved.overlay_click_pos, None,
        "a press while armed is a region gesture and must reach no map handler",
    );
}

/// A discarded drag leaves nothing drawn on the map.
///
/// The mutation this closes is forgetting to clear the in-flight drag on
/// release. Nothing breaks immediately — the next press overwrites it — but
/// the preview box stays painted over the map for as long as the mode is
/// armed, which looks exactly like a committed region that was never
/// committed.
#[test]
fn a_discarded_drag_leaves_no_box_behind_on_the_map() {
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    h.load_scan("KTLX");
    h.gui_mut().set_region_arm_for_test(true);
    h.frames_for(1, FRAME_DT);
    let rect = h.pane_rects()[0];

    // Big enough to be drawn while in flight, small enough to be discarded.
    // 6 points is well under 10 km at this zoom.
    h.mouse_press(rect.center());
    h.frames_for(1, FRAME_DT);
    h.mouse_move(rect.center() + egui::vec2(6.0, 0.0));
    h.frames_for(1, FRAME_DT);
    h.mouse_release(rect.center() + egui::vec2(6.0, 0.0));
    h.frames_for(3, FRAME_DT);

    // Nothing square and region-sized anywhere near where the drag was. The
    // preview is a stroked square centred on the press, so it would sit
    // right here if it had survived.
    let squares = h
        .painted_rects()
        .iter()
        .filter(|r| {
            (r.width() - r.height()).abs() < 1.0
                && r.width() > 2.0
                && r.center().distance(rect.center()) < 40.0
        })
        .count();
    assert_eq!(
        squares, 0,
        "a drag that committed nothing must leave nothing drawn",
    );
}
