//! **A data arrival is a reason to rasterize** (WO-M13a).
//!
//! The draw loop's `needs_rerender` pass is unchanged and still runs every
//! frame — it stays the only discoverer for a first render, a resize, a pan and
//! a zoom settle. What is new is that a pane which already has a raster of a
//! layer out does not wait for a frame to notice its data moved.
//!
//! **Why the alerts layer and not the reports layer for most of this.** A
//! handler that does not override `content_signature` takes the default, which
//! is `data_generation()` — a counter that moves on *every* apply, whatever
//! arrived. Under such a layer "the arrival did not move the token" is not a
//! reachable state, so the fixture could not tell a real comparison from a
//! dispatch-always. `NwsAlertHandler` folds its signature out of the alerts it
//! would draw, so both arms exist and both are pinned below. The theme term is
//! the one thing alerts cannot show — they are not theme-sensitive — so it gets
//! its own case on storm reports, which are.

use squallar_egui::overlay_cache::OverlayTexturePlan;
use squallar_geo::GeoBounds;
use squallar_overlays::render::overlay_state::{OverlayFetchResult, SourceEvent};
use squallar_overlays::types::{HatchPattern, OverlayFeature};
use squallar_source::id::{LayerId, known};
use std::sync::{Arc, Mutex};

/// A sink that refuses every job, so the funnel runs it here and lands a real
/// response on the channel — which is how a dispatch's **token, zoom, bounds
/// and target panes** are read back without trusting the record this land
/// writes.
struct RefusingPort;

impl squallar_worker::offload::JobSink for RefusingPort {
    fn send(
        &self,
        _id: u64,
        request: squallar_worker::offload::JobRequest,
    ) -> Result<(), squallar_worker::offload::JobRequest> {
        Err(request)
    }
}

/// A sink that takes every job and counts it — for the cases whose whole
/// assertion is "nothing was dispatched".
struct CountingPort {
    taken: Arc<Mutex<usize>>,
}

impl squallar_worker::offload::JobSink for CountingPort {
    fn send(
        &self,
        _id: u64,
        _request: squallar_worker::offload::JobRequest,
    ) -> Result<(), squallar_worker::offload::JobRequest> {
        *self.taken.lock().unwrap() += 1;
        Ok(())
    }
}

/// **The geometry the record carries, and every field of it is off its
/// default.** A lat span that is not the lon span, a width that is not the
/// height, an overdraw that is not zero, a density that is not one and a zoom
/// that is not a round number: an arrival that quietly rebuilt its own
/// geometry instead of re-using the record would agree with a fixture whose
/// numbers were symmetric, and it cannot agree with this one.
const RECORDED_BOUNDS: GeoBounds = GeoBounds {
    min_lat: 33.25,
    max_lat: 36.75,
    min_lon: -99.5,
    max_lon: -95.25,
};
const RECORDED_PLAN: OverlayTexturePlan = OverlayTexturePlan {
    width: 96,
    height: 48,
    overdraw: 0.125,
    pixels_per_point: 2.0,
    pane_px: [0, 0],
};
const RECORDED_ZOOM: i32 = 37;

fn a_recorded_request(token: u64) -> super::OverlayRenderRequest {
    super::OverlayRenderRequest {
        geo_bounds: RECORDED_BOUNDS,
        texture: RECORDED_PLAN,
        data_generation: token,
        zoom: RECORDED_ZOOM,
    }
}

/// Turn `id` on or off in pane `idx`'s own state — the door a layer toggle
/// takes. Not `overlays.set_enabled`: a converted handler keeps "on" in the
/// pane, and a write to the registry alone is one `adopt_handler_state` away
/// from being undone.
fn set_enabled(app: &mut crate::app::App, idx: usize, id: &LayerId, on: bool) {
    let mut registry = std::mem::take(&mut app.gui.overlays);
    if let Some(pane) = app.gui.pane_mut(idx) {
        pane.hydrate_layer_states(&registry, idx);
        pane.set_layer_enabled(&mut registry, idx, id, on);
    }
    app.gui.overlays = registry;
}

fn a_feature(tag: &str) -> OverlayFeature {
    OverlayFeature::new(
        vec![vec![vec![
            (34.2, -98.8),
            (34.2, -97.2),
            (35.8, -97.2),
            (35.8, -98.8),
        ]]],
        [255, 0, 0, 128],
        [0, 0, 0, 0],
        tag.into(),
        String::new(),
        HatchPattern::None,
    )
}

/// One round of NWS alerts, `n` of them, each with its own id — so the
/// handler's id-folded signature is a function of `n`.
fn an_alert_round(n: usize) -> squallar_overlays::render::overlay_state::FetchPayload {
    let alerts = (0..n)
        .map(|i| squallar_overlays::nws::alert::NwsAlert {
            id: format!("urn:test:{i}"),
            event: "Tornado Warning".into(),
            category: squallar_overlays::nws::alert::AlertCategory::Warning,
            severity: "Severe".parse().unwrap(),
            urgency: "Immediate".parse().unwrap(),
            certainty: "Observed".parse().unwrap(),
            headline: None,
            description: String::new(),
            instruction: None,
            area_desc: String::new(),
            sender_name: String::new(),
            effective: String::new(),
            expires: String::new(),
            onset: None,
            ends: None,
            valid_from: None,
            valid_until: None,
            affected_zones: Vec::new(),
            features: Arc::new(vec![a_feature("T")]),
        })
        .collect();
    squallar_overlays::render::overlay_state::OverlayRegistry::nws_alerts_payload(alerts)
}

/// One round of storm reports, `n` of them — the theme-sensitive layer.
fn a_reports_round(n: usize) -> squallar_overlays::render::overlay_state::FetchPayload {
    use squallar_overlays::render::handlers::reports::StormReportsFetchResult;
    use squallar_overlays::spc::reports::{StormReport, StormReportKind, StormReportRound};
    let reports = (0..n)
        .map(|i| StormReport {
            kind: StormReportKind::Tornado,
            time: format!("20{i:02}"),
            valid: None,
            magnitude: None,
            location: "NORMAN".into(),
            county: "CLEVELAND".into(),
            state: "OK".into(),
            lat: 34.0 + i as f64 * 0.1,
            lon: -98.2 + i as f64 * 0.1,
            comments: String::new(),
        })
        .collect();
    Box::new(StormReportsFetchResult(Ok(StormReportRound {
        reports,
        failed_kinds: Vec::new(),
    })))
}

/// Push one `Data` arrival for `id` down the real channel and drain it through
/// **the frame pump's `Ingest` phase**, which is where the arrival dispatch
/// lives.
///
/// Not `poll_overlay_fetch_results` called by hand: every case here would then
/// pass with the pump row's dispatch deleted, which is the silent-partial-
/// success shape. Going in through `poll_data_channels` is what makes deleting
/// it visible.
fn arrive(
    app: &mut crate::app::App,
    id: &LayerId,
    data: squallar_overlays::render::overlay_state::FetchPayload,
) {
    app.channels
        .overlay_fetch_sender
        .send(SourceEvent::Data(OverlayFetchResult {
            kind: id.clone(),
            data,
        }))
        .expect("the receiver is alive");
    app.poll_data_channels();
}

/// What the **draw loop's own token function** says this pane's raster should
/// be keyed at right now.
///
/// What this proves and what it does not: it proves the arrival path went
/// through `overlay_cache_token` rather than reading `content_signature` raw
/// or carrying the recorded number forward. It does not independently
/// re-derive the token — it is the same function, which is the point.
fn draw_loop_token(app: &mut crate::app::App, idx: usize, id: &LayerId) -> u64 {
    let is_dark = app.cached_dark_theme.unwrap_or(false);
    let (panes, overlays) = app.gui.panes_and_overlays_mut();
    panes[idx].hydrate_layer_states(overlays, idx);
    squallar_egui::overlay_cache_token(overlays, idx, &panes[idx], id, is_dark)
}

fn in_flight(app: &mut crate::app::App, idx: usize, id: &LayerId) -> bool {
    app.gui
        .pane_mut(idx)
        .expect("the fixture's pane")
        .overlay_cache_mut(id)
        .renders
        .holds(squallar_egui::overlay_cache::RenderSlot::WHOLE)
}

/// Put a raster in flight for `idx`'s whole picture, or take the mark away.
///
/// The `true` arm records a ticket for the fixture's own recorded geometry: a
/// mark names a dispatch now, and what these tests ask of it is only whether
/// *something* is out, which is what gates the next ask.
fn set_in_flight(app: &mut crate::app::App, idx: usize, id: &LayerId, v: bool) {
    let renders = &mut app
        .gui
        .pane_mut(idx)
        .expect("the fixture's pane")
        .overlay_cache_mut(id)
        .renders;
    if v {
        renders.record(squallar_egui::overlay_cache::RenderTicket::whole(
            A_STALE_TOKEN,
            RECORDED_PLAN.coverage(&RECORDED_BOUNDS),
        ));
    } else {
        renders.abandon_all();
    }
}

/// A dark two-pane app on KTLX with `id` on in pane 0 and one round of data
/// installed.
///
/// **Dark, deliberately.** The token mixes a theme term for a theme-sensitive
/// layer, and a fixture in the default theme cannot tell an arrival that reads
/// the theme from one that assumes light.
fn seeded(
    id: &LayerId,
    round: squallar_overlays::render::overlay_state::FetchPayload,
) -> crate::app::App {
    let mut app = crate::app::tests::n_pane_app(2, "KTLX");
    app.cached_dark_theme = Some(true);
    set_enabled(&mut app, 0, id, true);
    arrive(&mut app, id, round);
    app
}

/// Put a record under `(idx, id)` by making the dispatch that writes one, then
/// let the mark go so the arrival path is not refused for being busy.
///
/// This is the production door — `spawn_overlay_render` — so the record under
/// test is the one the action path writes, not one this file fabricated.
///
/// For the counting-sink cases only: that sink takes the job and answers
/// nothing, so there is no reply to wait for.
fn record_a_dispatch(app: &mut crate::app::App, idx: usize, id: &LayerId, token: u64) {
    app.spawn_overlay_render(vec![idx], id.clone(), a_recorded_request(token), None);
    set_in_flight(app, idx, id, false);
}

/// [`record_a_dispatch`] for the **refusing** sink, which really runs the job.
///
/// The seed dispatch's own reply is *waited for* and consumed, not drained
/// with `try_recv`: the refused job runs on its own thread, so a
/// non-blocking drain races it and leaves the seed's reply sitting in the
/// channel — where the case below reads it as if it were the arrival's, and
/// then agrees with itself about a token that never moved.
fn record_a_dispatch_and_take_its_reply(
    app: &mut crate::app::App,
    idx: usize,
    id: &LayerId,
    token: u64,
) {
    app.spawn_overlay_render(vec![idx], id.clone(), a_recorded_request(token), None);
    let seed = app
        .channels
        .overlay_render_receiver
        .recv_timeout(std::time::Duration::from_secs(10))
        .expect("the seed dispatch's own reply");
    assert_eq!(
        seed.generation, token,
        "precondition: the reply drained here must be the seed dispatch's",
    );
    while app.channels.overlay_render_receiver.try_recv().is_ok() {}
    set_in_flight(app, idx, id, false);
}

/// The token the record was written at in the cases that do not care what it
/// is, only that it is not the fresh one.
const A_STALE_TOKEN: u64 = 0x5EED_5EED_5EED_5EED;

// --------------------------------------------------------------- the trigger

#[test]
fn a_pane_holding_a_record_rasterizes_when_its_data_arrives() {
    let _guard = squallar_worker::offload::install_test_worker(Box::new(RefusingPort));
    let id = known::NWS_ALERTS;
    let mut app = seeded(&id, an_alert_round(1));
    record_a_dispatch_and_take_its_reply(&mut app, 0, &id, A_STALE_TOKEN);

    arrive(&mut app, &id, an_alert_round(3));

    let resp = app
        .channels
        .overlay_render_receiver
        .recv_timeout(std::time::Duration::from_secs(10))
        .expect(
            "the arrival did not dispatch a raster: a pane already showing this \
             layer had to wait for the draw loop to notice its data moved, which \
             is the frame WO-M13a removes",
        );
    assert_eq!(resp.overlay_kind, id, "some other layer was rasterized");
    assert_eq!(
        resp.pane_indices,
        vec![0],
        "the arrival dispatch must target exactly the pane holding the record",
    );
    assert_ne!(
        resp.generation, A_STALE_TOKEN,
        "the re-dispatch carried the RECORDED token. That token is stale by \
         definition — it is what the picture on the glass was keyed at — so \
         carrying it forward keys the new raster to the old picture and the \
         cache never adopts it.",
    );
    assert_eq!(
        resp.generation,
        draw_loop_token(&mut app, 0, &id),
        "the re-dispatch was keyed at something other than what the draw \
         loop's own `overlay_cache_token` says. The two paths must agree about \
         what \"the picture would be different\" means, or each will keep \
         re-dispatching what the other just drew.",
    );
    assert!(
        in_flight(&mut app, 0, &id),
        "the arrival dispatch left no `render_in_flight` mark, so it did not go \
         through `spawn_overlay_render`. An unmarked dispatch is dispatched \
         again by the draw loop on the very next frame.",
    );
}

#[test]
fn the_re_dispatch_carries_the_recorded_geometry_unchanged() {
    let _guard = squallar_worker::offload::install_test_worker(Box::new(RefusingPort));
    let id = known::NWS_ALERTS;
    let mut app = seeded(&id, an_alert_round(1));
    record_a_dispatch_and_take_its_reply(&mut app, 0, &id, A_STALE_TOKEN);

    arrive(&mut app, &id, an_alert_round(3));

    let resp = app
        .channels
        .overlay_render_receiver
        .recv_timeout(std::time::Duration::from_secs(10))
        .expect("the arrival dispatched a raster");
    assert_eq!(
        resp.zoom, RECORDED_ZOOM,
        "the arrival invented a zoom. It runs in `Ingest`, before the frame is \
         laid out — the only zoom it can know is the one the draw loop last \
         agreed, and a zoom-driven rebuild stays the draw loop's to discover.",
    );
    assert_eq!(
        resp.geo_bounds,
        RECORDED_PLAN.coverage(&RECORDED_BOUNDS),
        "the arrival rasterized ground other than the record's viewport plus \
         its overdraw — so either the bounds or the texture plan was rebuilt \
         rather than re-used, and the pane would cache a texture placed \
         somewhere it is not looking.",
    );
}

#[test]
fn an_arrival_that_does_not_move_the_token_dispatches_nothing() {
    let taken = Arc::new(Mutex::new(0usize));
    let _guard = squallar_worker::offload::install_test_worker(Box::new(CountingPort {
        taken: Arc::clone(&taken),
    }));
    let id = known::NWS_ALERTS;
    let mut app = seeded(&id, an_alert_round(2));
    // The record is written at the token that is fresh *now*.
    let settled = draw_loop_token(&mut app, 0, &id);
    record_a_dispatch(&mut app, 0, &id, settled);
    let before = *taken.lock().unwrap();

    // The same alerts arrive again — a real poll answering what it answered
    // last time. The handler folds its signature out of what it would draw, so
    // the token does not move.
    arrive(&mut app, &id, an_alert_round(2));

    assert_eq!(
        draw_loop_token(&mut app, 0, &id),
        settled,
        "precondition: this fixture's re-arrival must leave the token where it \
         was, or the case below cannot tell a comparison from a \
         dispatch-always",
    );
    assert_eq!(
        *taken.lock().unwrap(),
        before,
        "an arrival that changed nothing about the picture still dispatched a \
         raster. Every poll of every layer would then cost a rasterization on \
         every pane showing it.",
    );
    assert!(
        !in_flight(&mut app, 0, &id),
        "a pane was marked in flight for a raster nothing asked for",
    );
}

// ------------------------------------------------- the draw loop's own cases

#[test]
fn a_pane_with_no_record_gets_nothing_from_an_arrival() {
    let taken = Arc::new(Mutex::new(0usize));
    let _guard = squallar_worker::offload::install_test_worker(Box::new(CountingPort {
        taken: Arc::clone(&taken),
    }));
    let id = known::NWS_ALERTS;
    let mut app = seeded(&id, an_alert_round(1));

    arrive(&mut app, &id, an_alert_round(3));

    assert_eq!(
        *taken.lock().unwrap(),
        0,
        "an arrival rasterized for a pane that has never dispatched this layer. \
         A first render has no agreed geometry to re-use — it belongs to the \
         draw loop, which is the only thing that has laid the pane out.",
    );
}

#[test]
fn an_arrival_is_refused_by_a_pane_with_the_layer_switched_off() {
    let taken = Arc::new(Mutex::new(0usize));
    let _guard = squallar_worker::offload::install_test_worker(Box::new(CountingPort {
        taken: Arc::clone(&taken),
    }));
    let id = known::NWS_ALERTS;
    let mut app = seeded(&id, an_alert_round(1));
    record_a_dispatch(&mut app, 0, &id, A_STALE_TOKEN);
    set_enabled(&mut app, 0, &id, false);
    let before = *taken.lock().unwrap();

    arrive(&mut app, &id, an_alert_round(3));

    assert_eq!(
        *taken.lock().unwrap(),
        before,
        "an arrival rasterized a layer the pane has switched off. The record \
         outlives the toggle — it is never collected — so the eligibility \
         check is the only thing making it inert.",
    );
}

#[test]
fn an_arrival_is_refused_while_a_raster_is_already_in_flight() {
    let taken = Arc::new(Mutex::new(0usize));
    let _guard = squallar_worker::offload::install_test_worker(Box::new(CountingPort {
        taken: Arc::clone(&taken),
    }));
    let id = known::NWS_ALERTS;
    let mut app = seeded(&id, an_alert_round(1));
    record_a_dispatch(&mut app, 0, &id, A_STALE_TOKEN);
    set_in_flight(&mut app, 0, &id, true);
    let before = *taken.lock().unwrap();

    arrive(&mut app, &id, an_alert_round(3));

    assert_eq!(
        *taken.lock().unwrap(),
        before,
        "an arrival dispatched a second raster under an outstanding one. It \
         would take a render slot to draw a picture the first dispatch is \
         already drawing, and the draw loop declines on this same flag.",
    );
}

/// **The second door obeys the same rule as the first: no raster is spent into
/// a pane that is still landing one.**
///
/// The arrival path does not go through `needs_rerender`, so neither of that
/// gate's brakes has ever reached it. The in-flight mark above is not the same
/// question — it is cleared the moment a raster comes *back*, and the upload it
/// started runs for frames after that. A dispatch made in that window comes
/// back to a `hold` that replaces rather than queues, so the picture the pane
/// already paid for is discarded before a band of it is drawn and the viewer
/// sees neither.
///
/// **RED on the unmodified baseline**: the arrival dispatched, `taken` moved by
/// one, and the upload in flight was the thing that raster came back to
/// replace.
#[test]
fn an_arrival_is_refused_while_a_picture_is_still_landing() {
    // Withdraws a job, so it moves the process-global `cancelled` counter that
    // `overlay_cancel_tests` asserts a delta on. See `overlay_ledger_lock`.
    let _ledger = crate::app::fetch::overlay_ledger_lock();
    let taken = Arc::new(Mutex::new(0usize));
    let _guard = squallar_worker::offload::install_test_worker(Box::new(CountingPort {
        taken: Arc::clone(&taken),
    }));
    let id = known::NWS_ALERTS;
    let mut app = seeded(&id, an_alert_round(1));
    record_a_dispatch(&mut app, 0, &id, A_STALE_TOKEN);

    // A picture on its way to the GPU, with no raster outstanding behind it —
    // exactly the window the mark above cannot see.
    let cache = app.gui.pane_mut(0).expect("pane 0").overlay_cache_mut(&id);
    cache.hold(a_landing_picture(), None);
    assert!(
        cache.renders.is_empty(),
        "fixture: nothing may be in flight, or the mark above is what refuses \
         and this test measures it a second time",
    );
    let before = *taken.lock().unwrap();

    arrive(&mut app, &id, an_alert_round(3));

    assert_eq!(
        *taken.lock().unwrap(),
        before,
        "an arrival spent a raster into a pane whose picture was still \
         crossing to the GPU. `hold` replaces rather than queues, so that \
         upload is thrown away when this one lands — and the draw loop would \
         have asked for the very same picture one frame after it finished.",
    );

    // Non-triviality: with the hold landed, the same arrival does dispatch.
    // Without this the assertion above passes on an arrival path that refuses
    // everything.
    let cache = app.gui.pane_mut(0).expect("pane 0").overlay_cache_mut(&id);
    let landed = cache
        .take_held_if_delivered(|_| true)
        .expect("the hold is delivered");
    cache.show(landed.data);
    arrive(&mut app, &id, an_alert_round(5));
    assert_eq!(
        *taken.lock().unwrap(),
        before + 1,
        "the refusal outlived the upload that caused it, so an arrival would \
         never rasterize this pane again",
    );
}

/// A picture for [`an_arrival_is_refused_while_a_picture_is_still_landing`] to
/// hold. Only its existence matters — the hold is a slot, and what is in it is
/// never read by the path under test.
fn a_landing_picture() -> squallar_egui::overlay_cache::OverlayTextureData {
    let ctx = egui::Context::default();
    squallar_egui::overlay_cache::OverlayTextureData {
        texture: ctx.load_texture(
            "landing",
            egui::ColorImage::filled([1, 1], egui::Color32::RED),
            egui::TextureOptions::NEAREST,
        ),
        placed: squallar_geo::PlacedRaster::of(RECORDED_BOUNDS),
        data_generation: A_STALE_TOKEN,
        render_zoom: 0,
        width: 1,
        height: 1,
        radar_meta: None,
        hit_map: None,
    }
}

#[test]
fn an_arrival_is_refused_by_a_pane_the_layout_is_not_showing() {
    let taken = Arc::new(Mutex::new(0usize));
    let _guard = squallar_worker::offload::install_test_worker(Box::new(CountingPort {
        taken: Arc::clone(&taken),
    }));
    let id = known::NWS_ALERTS;
    let mut app = seeded(&id, an_alert_round(1));
    // Pane 1 is showing and holds a record; then the layout shrinks to one
    // pane and pane 1 is no longer painted by anybody. Neither `set_pane_count`
    // nor a config load ever shortens the vector, which is what puts pane 1
    // outside every visible walk and inside this one.
    set_enabled(&mut app, 1, &id, true);
    record_a_dispatch(&mut app, 1, &id, A_STALE_TOKEN);
    {
        use squallar_egui::UI_CONFIG_KEY;
        use squallar_kv::KvStore;
        let store = squallar_kv::MemoryKvStore::default();
        store
            .store(UI_CONFIG_KEY, r#"{"pane_count":1,"site":"KTLX"}"#)
            .expect("the memory store always accepts a write");
        assert!(
            app.gui.load_ui_config(&store),
            "the one-pane layout did not parse"
        );
    }
    // **Re-enabled after the shrink, on purpose.** A config load rebuilds the
    // panes' layer state, and a pane that came back switched off would be
    // refused by the enablement check — the case would then pass without the
    // visible-count check existing at all.
    set_enabled(&mut app, 1, &id, true);
    assert_eq!(
        app.gui.panes().len(),
        1,
        "precondition: the layout must really have stopped showing pane 1",
    );
    assert!(
        app.gui
            .pane_mut(1)
            .expect("precondition: pane 1 is still in the vector")
            .is_overlay_enabled(&id),
        "precondition: pane 1 must still have the layer on, or this case is \
         about the enablement check instead",
    );
    assert!(
        !in_flight(&mut app, 1, &id),
        "precondition: pane 1 must not be busy, or this case is about the \
         in-flight check instead",
    );
    assert!(
        app.render
            .overlay_record_holders(&id)
            .iter()
            .any(|(idx, _)| *idx == 1),
        "precondition: pane 1 must still hold its record",
    );
    let before = *taken.lock().unwrap();

    arrive(&mut app, &id, an_alert_round(3));

    assert_eq!(
        *taken.lock().unwrap(),
        before,
        "an arrival rasterized for a pane the layout is not showing. Nothing \
         will paint that texture; the record simply outlived its pane going \
         hidden.",
    );
}

/// `an_alert_round` whose windows closed past the retention margin (WB-4):
/// the next poll that no longer carries these EVICTS them rather than
/// retaining them for backward scrub — which is what it now takes for an
/// empty feed to really empty the layer.
fn an_evictable_alert_round(n: usize) -> squallar_overlays::render::overlay_state::FetchPayload {
    let now = chrono::Utc::now().naive_utc();
    let alerts = (0..n)
        .map(|i| squallar_overlays::nws::alert::NwsAlert {
            id: format!("urn:test:{i}"),
            event: "Tornado Warning".into(),
            category: squallar_overlays::nws::alert::AlertCategory::Warning,
            severity: "Severe".parse().unwrap(),
            urgency: "Immediate".parse().unwrap(),
            certainty: "Observed".parse().unwrap(),
            headline: None,
            description: String::new(),
            instruction: None,
            area_desc: String::new(),
            sender_name: String::new(),
            effective: String::new(),
            expires: String::new(),
            onset: None,
            ends: None,
            valid_from: Some(now - chrono::Duration::hours(30)),
            valid_until: Some(now - chrono::Duration::hours(25)),
            affected_zones: Vec::new(),
            features: Arc::new(vec![a_feature("T")]),
        })
        .collect();
    squallar_overlays::render::overlay_state::OverlayRegistry::nws_alerts_payload(alerts)
}

#[test]
fn an_arrival_that_empties_the_layer_dispatches_nothing() {
    let taken = Arc::new(Mutex::new(0usize));
    let _guard = squallar_worker::offload::install_test_worker(Box::new(CountingPort {
        taken: Arc::clone(&taken),
    }));
    let id = known::NWS_ALERTS;
    // Long-expired windows, deliberately: under WB-4 retention an alert that
    // merely LEFT the feed is kept for backward scrub and the layer still
    // has data — the genuinely-emptied layer this test is about now requires
    // the departing alerts to be past eviction.
    let mut app = seeded(&id, an_evictable_alert_round(2));
    record_a_dispatch(&mut app, 0, &id, A_STALE_TOKEN);
    let before = *taken.lock().unwrap();

    arrive(&mut app, &id, an_alert_round(0));

    let (panes, overlays) = app.gui.panes_and_overlays_mut();
    assert!(
        !overlays.has_data(&id, &panes[0].layer_ref(0, &id)),
        "precondition: the empty round must really leave the layer with no data",
    );
    // **The record, not the sink.** An emptied layer whose dispatch is NOT
    // refused here is refused one layer down, where `prepare_job` answers
    // nothing and the marks are cleared again — so the funnel count cannot
    // tell the two apart and this case passed with the check deleted. What
    // does differ is the record: a dispatch that got as far as
    // `spawn_overlay_render` re-recorded itself at the fresh token before
    // abandoning, and this one must not have.
    let held = app.render.overlay_record_holders(&id);
    assert_eq!(held.len(), 1, "precondition: the record is still there");
    assert_eq!(
        held[0].1.data_generation, A_STALE_TOKEN,
        "an arrival that emptied the layer was dispatched anyway. It reaches \
         `spawn_overlay_render`, re-records itself, resolves the job, finds \
         nothing to draw and abandons — work done once per poll per pane for \
         a picture that was never going to exist.",
    );
    assert_eq!(
        *taken.lock().unwrap(),
        before,
        "an arrival that emptied the layer still dispatched a raster of it. \
         The draw loop refuses on `has_data` for the same reason: there is \
         nothing to draw and the dispatch would abandon itself downstream.",
    );
}

// ----------------------------------------------------------- radar exclusion

#[test]
fn an_arrival_for_radar_never_reaches_the_overlay_record() {
    let taken = Arc::new(Mutex::new(0usize));
    let _guard = squallar_worker::offload::install_test_worker(Box::new(CountingPort {
        taken: Arc::clone(&taken),
    }));
    let mut app = crate::app::tests::n_pane_app(2, "KTLX");
    app.cached_dark_theme = Some(true);
    set_enabled(&mut app, 0, &known::RADAR, true);
    // A record under radar's id — the state the refusal has to survive. It
    // cannot arise through `spawn_overlay_render` today, which is exactly why
    // the refusal is stated rather than left to be a coincidence.
    app.render
        .record_overlay_dispatch(0, &known::RADAR, a_recorded_request(A_STALE_TOKEN));

    // **Every generic gate is open on radar, and that is asserted rather than
    // hoped for.** Radar declares `RenderMode::Texture` and answers `has_data`
    // with an unconditional `true`, so nothing but the named refusal stands
    // between a radar arrival and an overlay dispatch.
    {
        let (panes, overlays) = app.gui.panes_and_overlays_mut();
        panes[0].hydrate_layer_states(overlays, 0);
        assert!(
            panes[0].is_overlay_enabled(&known::RADAR),
            "precondition: radar must be on, or the enablement gate is what \
             refuses and this case is about nothing",
        );
        assert_eq!(
            overlays.render_mode(&known::RADAR),
            Some(squallar_overlays::render::overlay_state::RenderMode::Texture),
            "precondition: radar must declare a texture render mode, or the \
             render-mode gate is what refuses",
        );
        assert!(
            overlays.has_data(&known::RADAR, &panes[0].layer_ref(0, &known::RADAR)),
            "precondition: radar must answer `has_data`, or that gate is what \
             refuses",
        );
    }
    assert_ne!(
        draw_loop_token(&mut app, 0, &known::RADAR),
        A_STALE_TOKEN,
        "precondition: the recorded token must be stale for radar too, or the \
         comparison is what refuses",
    );

    arrive(&mut app, &known::RADAR, an_alert_round(3));

    // **The record, not the funnel.** A radar dispatch that is not refused here
    // is refused inside `spawn_overlay_render`, whose match has no arm for
    // radar and clears the marks again — so the funnel count cannot tell the
    // two apart. The record is written before that match, so it can.
    let held = app.render.overlay_record_holders(&known::RADAR);
    assert_eq!(held.len(), 1, "precondition: the record is still there");
    assert_eq!(
        held[0].1.data_generation, A_STALE_TOKEN,
        "a radar arrival was routed through the overlay record. Radar's own \
         arrival path is the volume stamp (WO-M14c); the draw loop excludes \
         radar from `needs_rerender` for the same reason, and a radar id \
         reaching `spawn_overlay_render` lands in the arm that logs \
         \"cannot rasterize\" once per poll.",
    );
    assert_eq!(
        *taken.lock().unwrap(),
        0,
        "a radar arrival reached the rasterization funnel",
    );
}

// --------------------------------------------------------------- the theme

#[test]
fn the_fresh_token_carries_the_theme_the_raster_will_be_drawn_in() {
    let _guard = squallar_worker::offload::install_test_worker(Box::new(RefusingPort));
    // Storm reports, because alerts are not theme-sensitive and cannot show
    // this at all.
    let id = known::STORM_REPORTS;
    let mut app = seeded(&id, a_reports_round(2));
    assert!(
        app.gui.overlays.theme_sensitive(&id),
        "precondition: this case needs a theme-sensitive layer or it asserts \
         nothing",
    );
    record_a_dispatch_and_take_its_reply(&mut app, 0, &id, A_STALE_TOKEN);

    arrive(&mut app, &id, a_reports_round(5));

    let resp = app
        .channels
        .overlay_render_receiver
        .recv_timeout(std::time::Duration::from_secs(10))
        .expect("the arrival dispatched a raster");
    let dark = draw_loop_token(&mut app, 0, &id);
    app.cached_dark_theme = Some(false);
    let light = draw_loop_token(&mut app, 0, &id);
    app.cached_dark_theme = Some(true);
    assert_ne!(
        dark, light,
        "precondition: the theme term must really move this layer's token, or \
         the assertion below passes in either theme",
    );
    assert_eq!(
        resp.generation, dark,
        "the arrival keyed its raster at the wrong theme's token. The dispatch \
         rasterizes in `cached_dark_theme`; a token computed in the other one \
         keys the picture to a cache entry the pane will never look up.",
    );
}

// ------------------------------------------------- one entry, both paths

#[test]
fn both_paths_write_the_record_and_reach_the_dispatch_through_one_entry() {
    // Withdraws a job, so it moves the process-global `cancelled` counter that
    // `overlay_cancel_tests` asserts a delta on. See `overlay_ledger_lock`.
    let _ledger = crate::app::fetch::overlay_ledger_lock();
    let taken = Arc::new(Mutex::new(0usize));
    let _guard = squallar_worker::offload::install_test_worker(Box::new(CountingPort {
        taken: Arc::clone(&taken),
    }));
    let id = known::NWS_ALERTS;
    let mut app = seeded(&id, an_alert_round(1));
    assert!(
        app.render.overlay_record_holders(&id).is_empty(),
        "precondition: nothing has dispatched this layer yet",
    );

    // The action path: what the draw loop's `needs_rerender` pass emits.
    app.process_gui_actions(vec![squallar_egui::actions::GuiAction::RenderOverlay {
        pane_idx: 0,
        overlay_kind: id.clone(),
        geo_bounds: RECORDED_BOUNDS,
        texture: RECORDED_PLAN,
        data_generation: A_STALE_TOKEN,
        zoom: RECORDED_ZOOM,
    }]);
    let after_action = app.render.overlay_record_holders(&id);
    assert_eq!(
        after_action.len(),
        1,
        "the action path dispatched without writing a record. The arrival path \
         has nothing to fire from, so the draw loop is still the only \
         discoverer.",
    );
    assert_eq!(after_action[0].0, 0, "the record is the dispatching pane's");
    assert_eq!(
        after_action[0].1.data_generation, A_STALE_TOKEN,
        "the record does not carry the token the dispatch was made at",
    );
    assert_eq!(
        *taken.lock().unwrap(),
        1,
        "the action path did not reach the funnel exactly once",
    );
    set_in_flight(&mut app, 0, &id, false);

    // The arrival path, through the same two functions.
    arrive(&mut app, &id, an_alert_round(4));
    let after_arrival = app.render.overlay_record_holders(&id);
    assert_eq!(
        *taken.lock().unwrap(),
        2,
        "the arrival path did not reach the funnel — the two paths do not share \
         one entry",
    );
    assert_eq!(
        after_arrival.len(),
        1,
        "the arrival path wrote a second record for the same (pane, layer) \
         instead of overwriting the first. Records are never collected; \
         overwriting is what keeps the map bounded.",
    );
    assert_ne!(
        after_arrival[0].1.data_generation, A_STALE_TOKEN,
        "the arrival path dispatched without re-recording, so the next arrival \
         would compare against a token two generations old",
    );
    assert_eq!(
        (
            after_arrival[0].1.geo_bounds,
            after_arrival[0].1.texture.width,
            after_arrival[0].1.zoom
        ),
        (RECORDED_BOUNDS, RECORDED_PLAN.width, RECORDED_ZOOM),
        "the record's geometry moved across an arrival re-dispatch",
    );
}

/// **The arrival path owns no dispatch of its own.**
///
/// Stated as a property of the source because the behaviour above cannot see
/// it: a bare `offload_job` beside `spawn_overlay_render` would still post a
/// job and still deliver a response, and only the missing `render_in_flight`
/// mark — one frame later — would show it.
#[test]
fn the_arrival_collection_names_no_dispatch_of_its_own() {
    const APP_FETCH: &str = include_str!("../app_fetch.rs");
    const APP: &str = include_str!("../app.rs");

    /// The body of the method `signature` opens, to its closing brace at
    /// method indentation.
    fn body_of<'a>(source: &'a str, signature: &str) -> &'a str {
        let (_, rest) = source
            .split_once(signature)
            .unwrap_or_else(|| panic!("`{signature}` is no longer written here"));
        rest.split_once("\n    }")
            .map(|(body, _)| body)
            .unwrap_or_else(|| panic!("`{signature}` has no recognisable body"))
    }

    let body = body_of(APP_FETCH, "pub(super) fn arrived_overlay_asks(");
    assert!(
        body.contains("overlay_record_holders"),
        "control: `arrived_overlay_asks` no longer reads the record, so the \
         absence checks below are reading the wrong function",
    );
    for door in ["offload_job(", "offload::offload(", "spawn_overlay_render("] {
        assert!(
            !body.contains(door),
            "`arrived_overlay_asks` names `{door}`. It collects asks; it does \
             not dispatch them. Every ask goes through \
             `dispatch_overlay_renders` so that the dedupe, the grouping and \
             the `render_in_flight` marks are the action path's own.",
        );
    }

    let entry = body_of(APP, "fn dispatch_overlay_renders(");
    assert!(
        entry.contains("deduplicate_overlay_renders(") && entry.contains("spawn_overlay_render("),
        "the shared entry no longer both dedupes and dispatches, so the two \
         paths are no longer getting the same treatment",
    );
    assert_eq!(
        APP.matches("spawn_overlay_render(").count(),
        1,
        "app.rs calls `spawn_overlay_render` from somewhere other than the one \
         shared entry",
    );

    // **One token function, not two.** The comparison the arrival path makes is
    // only meaningful because the number it computes is the number the draw
    // loop compares against — a second definition, however faithful the day it
    // was written, is two definitions to keep in step.
    const UI_MAP_PANE: &str = include_str!("../../../squallar-egui/src/ui_map_pane.rs");
    assert_eq!(
        UI_MAP_PANE.matches("fn overlay_cache_token(").count(),
        1,
        "`overlay_cache_token` is not declared exactly once where the draw loop \
         reads it",
    );
    assert!(
        APP_FETCH.contains("squallar_egui::overlay_cache_token("),
        "the arrival path no longer calls the draw loop's own token function. \
         Whatever it computes instead, the two paths will disagree about which \
         raster is stale and each will re-dispatch what the other just drew.",
    );
}

/// **The dispatch decomposition starts from zero on every call, and the clear
/// happens before any work it would otherwise inherit.**
///
/// `dispatch_overlay_renders` is reached twice per frame from different
/// segments: the arrival path calls it in `Ingest`, upstream of the paint
/// list, and the action path calls it in `handle_redraw`'s tail. Both fill
/// the same accumulator on the ledger. Only the second one's total is the
/// `post` segment's `dispatch` cut, and `frame dispatch (*)` telescopes to
/// that cut — so an entry that did not clear would file the arrival path's
/// microseconds inside a family whose denominator excludes them, and the
/// seven would stop summing to their parent.
///
/// Stated as a property of the source because no behavioural test can see it:
/// an accumulator that kept a stale total still reports seven figures, still
/// telescopes on any frame where the arrival path happened not to run, and is
/// wrong only on the frames where it did.
#[test]
fn the_dispatch_entry_clears_its_accumulator_before_it_dedupes() {
    const APP: &str = include_str!("../app.rs");
    let signature = "fn dispatch_overlay_renders(";
    let entry = APP
        .split_once(signature)
        .map(|(_, rest)| rest)
        .and_then(|rest| rest.split_once("\n    }"))
        .map(|(body, _)| body)
        .unwrap_or_else(|| panic!("`{signature}` has no recognisable body"));

    let clear = entry.find("take_dispatch_cuts()").unwrap_or_else(|| {
        panic!(
            "the dispatch entry never clears its accumulator, so an \
             arrival-path dispatch earlier in the frame is counted inside the \
             `post` tail's `dispatch` cut",
        )
    });
    let dedupe = entry
        .find("deduplicate_overlay_renders(")
        .expect("control: the entry no longer dedupes, so this reads the wrong function");
    assert!(
        clear < dedupe,
        "the dispatch entry clears its accumulator only after it has begun \
         work, so whatever the dedupe costs is added to a total the clear \
         then throws away",
    );
}
