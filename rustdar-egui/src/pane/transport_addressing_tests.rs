//! **Which layer the loop transport addresses.**
//!
//! `time_state(&known::RADAR)` is radar's own timeline and goes on being
//! addressed by name — the arrival path and the render dispatcher carry
//! radar's payloads and mean exactly that slot. (Until WO-T3.7 the same read
//! was spelled `loop_state()`, a name that claimed to be "this pane's loop"
//! and stopped being true the moment a pane held one timeline per animating
//! layer.) What moves is the *transport*: the ∞ toggle, the play/step/seek
//! buttons and the scrubber, which read [`PaneState::transport_state`] and can
//! therefore land on a model layer.
//!
//! The trap these tests are shaped around: [`PaneState::time_state`] answers a
//! missing slot with the pane's orphan state, so a pane with no model slot
//! would make "radar's timeline" and "the model's timeline" the same object
//! and pass a difference assertion vacuously. Every case below puts a REAL
//! model slot, with a real phase, on the pane first.

use super::*;
use crate::Gui;
use rustdar_kv::MemoryKvStore;

/// Toggle a layer on pane 0 through the **production** door — the same call
/// `Gui::set_active_pane_overlay` makes when a layer row is ticked, taken
/// apart the same way so the registry and the pane can be borrowed at once.
fn toggle(gui: &mut Gui, id: &LayerId, on: bool) {
    let mut pane = std::mem::take(gui.pane_mut(0).expect("pane 0"));
    Gui::write_pane_overlay(&mut gui.overlays, 0, &mut pane, id, on);
    *gui.pane_mut(0).expect("pane 0") = pane;
}

/// Give `id`'s slot on pane 0 a timeline that is genuinely running, so the
/// state it answers with is distinguishable from any other layer's.
fn animate(gui: &mut Gui, id: &LayerId, phase: LoopPhase, span_secs: u64) {
    let state = gui.pane_mut(0).expect("pane 0").time_state_mut(id);
    state.phase = phase;
    state.span_secs = span_secs;
}

/// A pane drawing a model field and no radar has no radar loop to address, so
/// its transport addresses the model — the whole point of the split.
#[test]
fn a_model_only_pane_addresses_the_model_layer() {
    let mut gui = Gui::new();
    toggle(&mut gui, &known::RADAR, false);
    toggle(&mut gui, &known::MODEL_DATA, true);
    animate(&mut gui, &known::MODEL_DATA, LoopPhase::Ready, 64_800);

    assert_eq!(
        gui.pane(0).expect("pane 0").transport_layer(),
        &known::MODEL_DATA,
    );
}

/// With both drawn, the transport addresses radar: the slot list runs bottom
/// to top and radar's `draw_order_weight` is 30 against the model's 10, so
/// radar is the topmost frame-series layer. This is what keeps every existing
/// pane's transport exactly where it was.
#[test]
fn a_pane_drawing_both_addresses_radar_as_the_topmost_frame_series_layer() {
    let mut gui = Gui::new();
    toggle(&mut gui, &known::RADAR, true);
    toggle(&mut gui, &known::MODEL_DATA, true);
    animate(&mut gui, &known::MODEL_DATA, LoopPhase::Ready, 64_800);
    animate(&mut gui, &known::RADAR, LoopPhase::Playing, 3_600);

    assert_eq!(
        gui.pane(0).expect("pane 0").transport_layer(),
        &known::RADAR
    );
}

/// The two accessors answer two different objects on a model-only pane —
/// asserted on the *contents*, because the addresses would also differ for
/// the vacuous reason that one of them is the orphan state.
#[test]
fn the_transport_state_and_the_radar_loop_state_are_different_timelines() {
    let mut gui = Gui::new();
    toggle(&mut gui, &known::RADAR, false);
    toggle(&mut gui, &known::MODEL_DATA, true);
    animate(&mut gui, &known::MODEL_DATA, LoopPhase::Ready, 64_800);
    animate(&mut gui, &known::RADAR, LoopPhase::Inactive, 3_600);

    let pane = gui.pane(0).expect("pane 0");
    assert!(
        pane.slot(&known::RADAR).is_some(),
        "precondition: radar has a REAL slot, so the radar read below is not \
         the orphan state and the difference is not vacuous"
    );
    assert!(
        pane.slot(&known::MODEL_DATA).is_some(),
        "precondition: the model has a REAL slot"
    );

    let transport = pane.transport_state();
    let radar = pane.time_state(&known::RADAR);
    assert_eq!(transport.phase, LoopPhase::Ready);
    assert_eq!(radar.phase, LoopPhase::Inactive);
    assert_eq!(transport.span_secs, 64_800);
    assert_eq!(radar.span_secs, 3_600);
    assert!(
        !std::ptr::eq(transport, radar),
        "two layers, two timelines — not one state answered twice"
    );
}

/// A frame-step or a scrub moves the pane's clock onto the **transport
/// layer's** frame stamp. The two timelines below carry deliberately
/// different stamps, so a park that reached radar's list would land on the
/// wrong instant rather than merely on the same one by accident.
#[test]
fn parking_on_a_transport_frame_takes_the_transport_layers_stamp() {
    fn frames(base_hour: u32) -> Vec<LoopFrame> {
        (0..3)
            .map(|i| LoopFrame {
                timestamp: chrono::NaiveDate::from_ymd_opt(2026, 8, 22)
                    .unwrap()
                    .and_hms_opt(base_hour + i, 0, 0)
                    .unwrap(),
                image: None,
                render_in_flight: false,
                render_failed: false,
            })
            .collect()
    }

    let mut gui = Gui::new();
    toggle(&mut gui, &known::RADAR, false);
    toggle(&mut gui, &known::MODEL_DATA, true);
    animate(&mut gui, &known::MODEL_DATA, LoopPhase::Ready, 64_800);
    animate(&mut gui, &known::RADAR, LoopPhase::Ready, 3_600);
    {
        let pane = gui.pane_mut(0).expect("pane 0");
        pane.time_state_mut(&known::MODEL_DATA).frames = frames(12);
        pane.time_state_mut(&known::RADAR).frames = frames(3);
    }

    let pane = gui.pane_mut(0).expect("pane 0");
    assert!(pane.park_on_transport_frame(1));

    assert_eq!(
        pane.time.mode,
        TimeMode::AsOf(
            chrono::NaiveDate::from_ymd_opt(2026, 8, 22)
                .unwrap()
                .and_hms_opt(13, 0, 0)
                .unwrap()
        ),
        "the model's frame 1 (13:00), not radar's (04:00)"
    );
}

/// A loop already running keeps the controls. Ticking another layer on is not
/// a request to hand the ∞ button to a timeline that is doing nothing, and
/// before this the toggle would have re-derived the transport out from under
/// a radar loop the moment the user enabled a model field beside it.
#[test]
fn a_running_loop_keeps_the_transport_when_another_layer_is_toggled() {
    let mut gui = Gui::new();
    toggle(&mut gui, &known::RADAR, true);
    animate(&mut gui, &known::RADAR, LoopPhase::Playing, 3_600);
    // Radar off, so the topmost enabled frame-series layer becomes the model
    // — the re-derivation this test asserts does NOT happen.
    toggle(&mut gui, &known::RADAR, false);
    toggle(&mut gui, &known::MODEL_DATA, true);

    let pane = gui.pane(0).expect("pane 0");
    assert_eq!(
        pane.topmost_frame_series_layer(&gui.overlays),
        Some(&known::MODEL_DATA),
        "precondition: the re-derivation would have moved the transport",
    );
    assert_eq!(
        gui.pane(0).expect("pane 0").transport_layer(),
        &known::RADAR,
        "the playing loop kept the controls",
    );
}

/// The non-triviality floor under every assertion above: a pane nobody has
/// touched addresses radar, so "always answer the model" fails too.
#[test]
fn a_fresh_pane_addresses_radar() {
    let gui = Gui::new();
    assert_eq!(
        gui.pane(0).expect("pane 0").transport_layer(),
        &known::RADAR
    );
}

/// Reopen is 1:1: a pane whose transport had moved to the model comes back
/// addressing the model.
#[test]
fn the_transport_layer_survives_a_restart() {
    let store = MemoryKvStore::default();
    let mut gui = Gui::new();
    toggle(&mut gui, &known::RADAR, false);
    toggle(&mut gui, &known::MODEL_DATA, true);
    assert_eq!(
        gui.pane(0).expect("pane 0").transport_layer(),
        &known::MODEL_DATA,
        "precondition: the transport moved before the save"
    );
    gui.save_ui_config(&store);

    let mut restored = Gui::new();
    assert!(restored.load_ui_config(&store));
    assert_eq!(
        restored.pane(0).expect("pane 0").transport_layer(),
        &known::MODEL_DATA,
    );
}

/// The other half of the same floor: an untouched config round-trips to
/// radar, so a serializer that always writes the model — or a loader that
/// always reads it — fails here even though the test above passes.
#[test]
fn an_untouched_config_round_trips_to_radar() {
    let store = MemoryKvStore::default();
    let gui = Gui::new();
    gui.save_ui_config(&store);

    let mut restored = Gui::new();
    assert!(restored.load_ui_config(&store));
    assert_eq!(
        restored.pane(0).expect("pane 0").transport_layer(),
        &known::RADAR,
    );
}

/// A plan-view loop frame carrying `nyquist_ms` as its fingerprint, so the
/// assertion below can say *which* timeline was read rather than merely that
/// something was.
fn plan_view_folding_at(ctx: &egui::Context, nyquist_ms: f64) -> LoopFrameImage {
    let image = egui::ColorImage::from_rgba_unmultiplied([1, 1], &[255, 255, 255, 255]);
    LoopFrameImage::PlanView(RadarImageData {
        texture: ctx.load_texture("plan", image, egui::TextureOptions::NEAREST),
        lat: 35.33,
        lon: -97.28,
        max_range_km: 230.0,
        placed: rustdar_radar::types::ImageBounds::from_radar_site(35.33, -97.28, 230.0).into(),
        nyquist_ms: Some(nyquist_ms),
        melting_layer_source: None,
        storm_motion: None,
        hover: std::sync::Arc::new(rustdar_radar::hover::HoverSource::empty()),
    })
}

/// One frame, stamped, holding `image`.
fn one_frame(image: LoopFrameImage) -> Vec<LoopFrame> {
    vec![LoopFrame {
        timestamp: chrono::NaiveDate::from_ymd_opt(2026, 8, 22)
            .unwrap()
            .and_hms_opt(12, 0, 0)
            .unwrap(),
        image: Some(image),
        render_in_flight: false,
        render_failed: false,
    }]
}

/// **Radar's picture is read off radar's own timeline, never off whichever
/// layer holds the transport.**
///
/// The keep WO-T3.7 spelled out at `PaneState::active_loop_image`, held as a
/// test instead of as a doc line — the shed re-spelled ~30 reads, and a doc
/// saying "this one is radar's on purpose" is prose, not evidence. Retargeting
/// that read at the transport passed every suite in the tree before this
/// existed.
///
/// **The state is reachable, not a contrivance:**
/// [`PaneState::refresh_transport`] refuses to move the transport out from
/// under a **running** loop, so a pane that armed a model loop and then started
/// radar keeps the controls on the model while radar animates.
/// `set_transport_layer` is the config loader's own door and is used here to
/// reach that state in one line.
///
/// **The fixture is deliberately more forgiving than production.** A real model
/// frame holds [`LoopFrameImage::Overlay`], and [`LoopFrameImage::plan_view`]
/// answers `None` for that arm — so a transport-addressed read would fail by
/// returning nothing at all. Giving the model timeline a *plan-view* frame
/// instead means the read has to actively prefer radar's number rather than
/// merely trip over the other arm.
#[test]
fn radars_picture_is_read_off_radars_timeline_not_the_transports() {
    let ctx = egui::Context::default();
    let mut gui = Gui::new();
    toggle(&mut gui, &known::RADAR, true);
    toggle(&mut gui, &known::MODEL_DATA, true);
    animate(&mut gui, &known::MODEL_DATA, LoopPhase::Playing, 64_800);
    animate(&mut gui, &known::RADAR, LoopPhase::Playing, 3_600);
    {
        let pane = gui.pane_mut(0).expect("pane 0");
        pane.set_transport_layer(known::MODEL_DATA);
        pane.time_state_mut(&known::MODEL_DATA).frames =
            one_frame(plan_view_folding_at(&ctx, 12.0));
        pane.time_state_mut(&known::RADAR).frames = one_frame(plan_view_folding_at(&ctx, 31.5));
    }

    let pane = gui.pane(0).expect("pane 0");
    assert_eq!(
        pane.transport_layer(),
        &known::MODEL_DATA,
        "precondition: the transport is NOT radar, or the two reads are the \
         same object and the assertion below is vacuous",
    );
    assert!(
        pane.time_state(&known::MODEL_DATA)
            .qualifying_frame()
            .is_some(),
        "precondition: the transport's own timeline really has a frame under \
         its playhead, so a transport-addressed read would find one",
    );
    assert_eq!(
        pane.active_image().and_then(|img| img.nyquist_ms),
        Some(31.5),
        "radar's own frame (31.5), not the transport's (12.0)",
    );
}
