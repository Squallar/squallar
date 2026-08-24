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
use squallar_kv::MemoryKvStore;

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
        placed: squallar_radar::types::ImageBounds::from_radar_site(35.33, -97.28, 230.0).into(),
        nyquist_ms: Some(nyquist_ms),
        melting_layer_source: None,
        storm_motion: None,
        hover: std::sync::Arc::new(squallar_radar::hover::HoverSource::empty()),
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

// ---------------------------------------------------------------------------
// **The static half of the fork: a satellite loop running over a radar image
// that is NOT looping.** (WO-T3.8)
//
// The four reads below — `stale_image_on_screen` and the three legend
// annotations under it — all ask `time_state(&known::RADAR).is_active()` to
// decide between *radar's playing frame* and *radar's last static render*.
// Retargeting any of them at the transport answers "yes, a loop is running"
// about a timeline that is not radar's, takes the loop branch, and finds
// nothing there: `active_image` is radar-addressed, radar is not looping, so
// it answers `None` and the annotation is simply lost.
//
// **Reachable by exactly the door the two WO-T3.7 pins use.**
// `PaneState::refresh_transport` returns early while the transport's own loop
// is active, so a pane that armed a GMGSI loop and *then* enabled radar keeps
// the controls on the satellite. Radar not looping is the ordinary case on
// such a pane: it draws its live scan out of the overlay cache.

/// A pane drawing a **static** radar render — the picture the overlay cache
/// holds after a plain (non-looping) scan — described by the facts a render
/// carries about itself and nothing else can recompute.
///
/// `meta_product` is what the *pixels* are; `selected` is what the pane is
/// asking for. Equal means the image on the glass matches the selection;
/// different is what [`PaneState::stale_image_on_screen`] exists to report.
///
/// Both slots are opened through the **production door** — the same
/// `Gui::write_pane_overlay` a layer row's tick makes — so neither read below
/// can be the pane's orphan state.
fn pane_showing_a_static_radar_render(
    ctx: &egui::Context,
    selected: &FieldId,
    meta_product: &FieldId,
    nyquist_ms: Option<f64>,
    melting_layer_source: Option<squallar_radar::hca::MeltingLayerSource>,
    storm_motion: Option<squallar_radar::srv::SrvMotion>,
) -> PaneState {
    use crate::overlay_cache::{OverlayTextureData, RadarTextureMeta};

    let image = egui::ColorImage::from_rgba_unmultiplied([1, 1], &[255, 255, 255, 255]);
    let mut gui = Gui::new();
    toggle(&mut gui, &known::RADAR, true);
    toggle(&mut gui, &known::GMGSI, true);
    let mut pane = std::mem::take(gui.pane_mut(0).expect("pane 0"));
    pane.set_selected_product(selected.clone());
    pane.overlay_cache_mut(&known::RADAR)
        .show(OverlayTextureData {
            texture: ctx.load_texture("static-scan", image, egui::TextureOptions::NEAREST),
            placed: squallar_geo::PlacedRaster::of(squallar_geo::GeoBounds {
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
                hover: std::sync::Arc::new(squallar_radar::hover::HoverSource::empty()),
                lat: 35.0,
                lon: -97.0,
                max_range_km: 100.0,
                nyquist_ms,
                melting_layer_source,
                storm_motion,
                product: meta_product.clone(),
                elevation: 0.5,
            }),
            hit_map: None,
        });
    pane
}

/// Arm a **satellite** loop on `pane` and hand it the transport, leaving
/// radar's own timeline idle.
///
/// The satellite frame is stamped hours away from anything radar carries, so
/// a transport-addressed read cannot land on radar's answer by accident.
fn a_satellite_loop_takes_the_transport(pane: &mut PaneState) {
    {
        let sat = pane.time_state_mut(&known::GMGSI);
        sat.phase = LoopPhase::Playing;
        sat.span_secs = 43_200;
        sat.frames = vec![LoopFrame {
            timestamp: chrono::NaiveDate::from_ymd_opt(2026, 8, 22)
                .unwrap()
                .and_hms_opt(6, 0, 0)
                .unwrap(),
            image: None,
            render_in_flight: false,
            render_failed: false,
        }];
    }
    pane.set_transport_layer(known::GMGSI);

    assert!(
        pane.slot(&known::RADAR).is_some(),
        "precondition: radar has a REAL slot, so the radar-addressed reads \
         below are not the pane's orphan state",
    );
    assert!(
        pane.slot(&known::GMGSI).is_some(),
        "precondition: the satellite has a REAL slot of its own",
    );
    assert!(
        !std::ptr::eq(pane.transport_state(), pane.time_state(&known::RADAR)),
        "precondition: the transport really addresses another timeline, or the \
         two reads are one object and every assertion below is vacuous",
    );
    assert!(
        pane.transport_state().is_active(),
        "precondition: the satellite loop is genuinely running, so a \
         transport-addressed `is_active()` reads TRUE",
    );
    assert!(
        !pane.time_state(&known::RADAR).is_active(),
        "precondition: radar is NOT looping — the whole fork is which of the \
         two timelines answers that question",
    );
}

/// **The staleness notice is read off radar's own timeline, never off
/// whichever layer holds the transport.**
///
/// `stale_image_on_screen` answers `None` for a *looping* pane, because a
/// looping pane's frame textures are dropped the moment the selection moves
/// and therefore cannot be stale. Retargeting that gate at the transport lets
/// a running satellite loop answer it: the notice is suppressed and the pane
/// goes on presenting **reflectivity pixels labelled as velocity**, silently
/// — the exact confidently-wrong picture the notice exists to prevent.
///
/// **The floor is the first arm**, asserted against a literal rather than
/// against the other arm, so "both answered `None`" cannot satisfy this.
#[test]
fn radars_staleness_notice_is_read_off_radars_timeline_not_the_transports() {
    let ctx = egui::Context::default();

    let radar_driving = pane_showing_a_static_radar_render(
        &ctx,
        &radar_fields::known::VELOCITY,
        &radar_fields::known::REFLECTIVITY,
        None,
        None,
        None,
    );
    assert_eq!(
        radar_driving.stale_image_on_screen(),
        Some((radar_fields::known::REFLECTIVITY, 0.5)),
        "floor: a pane asking for velocity over reflectivity pixels disowns \
         them — if this arm already answered None the assertion below would be \
         satisfied by two Nones",
    );

    let mut satellite_driving = pane_showing_a_static_radar_render(
        &ctx,
        &radar_fields::known::VELOCITY,
        &radar_fields::known::REFLECTIVITY,
        None,
        None,
        None,
    );
    a_satellite_loop_takes_the_transport(&mut satellite_driving);

    assert_eq!(
        satellite_driving.stale_image_on_screen(),
        Some((radar_fields::known::REFLECTIVITY, 0.5)),
        "a pane whose transport sits on a satellite loop stopped disowning the \
         radar pixels on its own glass: the notice is gone and reflectivity is \
         now presented, unlabelled, as the velocity the pane is asking for",
    );
}

/// **The three legend annotations are read off radar's own timeline, never off
/// whichever layer holds the transport.**
///
/// `displayed_nyquist_ms`, `displayed_melting_layer_source` and
/// `displayed_storm_motion` each fork on the same question, and each takes
/// `active_image()` — radar's playing frame — when the answer is yes.
/// Retargeting the gate at a running satellite loop takes that branch over a
/// radar timeline holding no frames at all, so `active_image()` answers `None`
/// and **the annotation vanishes from the legend**: the velocity ramp stops
/// saying where it folds, the classification stops saying what melting layer
/// it stood on, and the storm-relative field stops saying what vector it was
/// shifted by.
///
/// Cosmetic but real — the fold limit is what tells a reader whether the
/// couplet in front of them is aliased.
///
/// **Each arm's floor is a literal**, taken on a radar-driven pane first.
#[test]
fn the_legend_annotations_are_read_off_radars_timeline_not_the_transports() {
    use squallar_radar::hca::MeltingLayerSource;

    let ctx = egui::Context::default();

    // -- Nyquist -----------------------------------------------------------
    let radar_driving = pane_showing_a_static_radar_render(
        &ctx,
        &radar_fields::known::VELOCITY,
        &radar_fields::known::VELOCITY,
        Some(26.42),
        None,
        None,
    );
    assert_eq!(
        radar_driving.displayed_nyquist_ms(),
        Some(26.42),
        "floor: a still velocity pane reports the fold limit its own cut \
         declared",
    );
    let mut satellite_driving = pane_showing_a_static_radar_render(
        &ctx,
        &radar_fields::known::VELOCITY,
        &radar_fields::known::VELOCITY,
        Some(26.42),
        None,
        None,
    );
    a_satellite_loop_takes_the_transport(&mut satellite_driving);
    assert_eq!(
        satellite_driving.displayed_nyquist_ms(),
        Some(26.42),
        "the velocity ramp lost its fold limit because a satellite loop \
         happened to be playing: the legend no longer says past what speed the \
         sign wraps, so an aliased couplet reads as a real one",
    );

    // -- Melting layer -----------------------------------------------------
    let radar_driving = pane_showing_a_static_radar_render(
        &ctx,
        &radar_fields::known::HYDROMETEOR_CLASSIFICATION,
        &radar_fields::known::HYDROMETEOR_CLASSIFICATION,
        None,
        Some(MeltingLayerSource::FleetDefault),
        None,
    );
    assert_eq!(
        radar_driving.displayed_melting_layer_source(),
        Some(MeltingLayerSource::FleetDefault),
        "floor: a still classification pane reports the melting layer its own \
         pixels stood on",
    );
    let mut satellite_driving = pane_showing_a_static_radar_render(
        &ctx,
        &radar_fields::known::HYDROMETEOR_CLASSIFICATION,
        &radar_fields::known::HYDROMETEOR_CLASSIFICATION,
        None,
        Some(MeltingLayerSource::FleetDefault),
        None,
    );
    a_satellite_loop_takes_the_transport(&mut satellite_driving);
    assert_eq!(
        satellite_driving.displayed_melting_layer_source(),
        Some(MeltingLayerSource::FleetDefault),
        "the classification lost the caveat that nobody measured its melting \
         layer, because a satellite loop happened to be playing — an \
         unqualified guess is now drawn as a measurement",
    );

    // -- Storm motion ------------------------------------------------------
    let motion = squallar_radar::srv::SrvMotion {
        speed_kt: 38.2,
        direction_deg: 224.6,
        source: squallar_radar::srv::StormMotionSource::BunkersRightMover,
    };
    let radar_driving = pane_showing_a_static_radar_render(
        &ctx,
        &radar_fields::known::STORM_RELATIVE_VELOCITY,
        &radar_fields::known::STORM_RELATIVE_VELOCITY,
        None,
        None,
        Some(motion),
    );
    assert_eq!(
        radar_driving.displayed_storm_motion().map(|m| m.speed_kt),
        Some(38.2),
        "floor: a still storm-relative pane reports the vector its own pixels \
         were shifted by",
    );
    let mut satellite_driving = pane_showing_a_static_radar_render(
        &ctx,
        &radar_fields::known::STORM_RELATIVE_VELOCITY,
        &radar_fields::known::STORM_RELATIVE_VELOCITY,
        None,
        None,
        Some(motion),
    );
    a_satellite_loop_takes_the_transport(&mut satellite_driving);
    assert_eq!(
        satellite_driving
            .displayed_storm_motion()
            .map(|m| m.speed_kt),
        Some(38.2),
        "the storm-relative field lost the vector it was shifted by, because a \
         satellite loop happened to be playing: the legend no longer says what \
         motion was subtracted, and every velocity on the glass is \
         unattributable",
    );
}
