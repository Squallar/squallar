//! **A layer holding data somewhere is not a layer that paints here.**
//!
//! Both overlay dispatch doors — the draw loop's `needs_rerender` pass
//! (`ui_map_pane`) and `App::arrived_overlay_asks` — gate on
//! `SourceHandler::has_data`, which takes no bounds. Every implementor in the
//! tree answers it extent-blind; five of them are literally
//! `!self.state.data.is_empty()`. So one active alert anywhere in the country
//! made a pane over Oklahoma spend a full-size raster — the pixmap allocated,
//! painted and scanned in the worker, and on the web crossed to it — to be told
//! its own extent was empty. `RasterizeOutput::settle_blank` then threw the
//! buffer away and the pane was handed a blank.
//!
//! `SourceHandler::paints_in` asks that same question one field earlier, where
//! it can still refuse the work, and `App::spawn_overlay_render` consults it
//! before it builds a paint input.
//!
//! # The three cases, and why none of them subsumes another
//!
//! * `an_extent_with_no_feature_in_it_dispatches_no_raster_job` is the saving,
//!   read off the **sink** rather than off any counter this land wrote: a job
//!   the funnel never received is the only unambiguous statement that no
//!   picture was rasterized.
//! * `an_empty_extent_delivers_a_blank_rather_than_no_response` is the trap.
//!   The refusal must not be spelled as `prepare_job -> None`: that exit takes
//!   `App::clear_overlay_render_marks` and sends nothing at all, so the pane
//!   keeps whatever ink it was drawing. A layer that renders nothing while its
//!   popup still answers is a shipped symptom of exactly that shape, in a
//!   different crate, found the same night. A blank is a **clear**.
//! * `an_extent_holding_a_feature_still_dispatches_its_raster` is the
//!   anti-vacuity floor. Without it a `paints_in` stuck at `false` satisfies
//!   both cases above and turns every overlay on the map into a clear — which
//!   would be diagnosed as a broken rasterizer rather than as a broken
//!   predicate.
//!
//! Nothing here states the answer it asserts. The predicate under test is the
//! real `NwsAlertHandler::paints_in` over data delivered through the real
//! arrival channel, and what it is compared against is where the fixture put
//! one polygon.

use squallar_egui::overlay_cache::OverlayTexturePlan;
use squallar_geo::GeoBounds;
use squallar_overlays::render::overlay_state::{OverlayFetchResult, SourceEvent};
use squallar_overlays::types::{HatchPattern, OverlayFeature};
use squallar_source::id::{LayerId, known};
use std::sync::{Arc, Mutex};

/// A sink that takes every job and counts it. The whole assertion of two cases
/// below is "nothing was dispatched", and this is what can say so.
struct CountingPort {
    taken: Arc<Mutex<usize>>,
}

impl squallar_worker::offload::JobSink for CountingPort {
    fn send(
        &self,
        _id: u64,
        _request: squallar_worker::offload::JobRequest,
    ) -> Result<(), squallar_worker::offload::JobRequest> {
        *self.taken.lock().expect("no poisoned lock") += 1;
        Ok(())
    }
}

/// The pane's viewport for every case here — Oklahoma, around KTLX.
///
/// Asymmetric on both axes on purpose: a dispatch that rebuilt its own bounds
/// instead of using the request's would agree with a square fixture.
const VIEWPORT: GeoBounds = GeoBounds {
    min_lat: 33.25,
    max_lat: 36.75,
    min_lon: -99.5,
    max_lon: -95.25,
};

/// `overdraw` is not zero, so the ground a raster covers is wider than
/// [`VIEWPORT`] — `OverlayTexturePlan::coverage` expands it by an eighth of
/// each span per side, to roughly 32.81..37.19 by -100.03..-94.72. The
/// out-of-extent fixture below is in Maine, which is outside that by degrees
/// rather than by a rounding.
const PLAN: OverlayTexturePlan = OverlayTexturePlan {
    width: 96,
    height: 48,
    overdraw: 0.125,
    pixels_per_point: 2.0,
    pane_px: [0, 0],
};

const ZOOM: i32 = 37;
const TOKEN: u64 = 4242;

/// Inside [`VIEWPORT`]: a box over central Oklahoma.
const IN_VIEW: (f64, f64, f64, f64) = (34.2, 35.8, -98.8, -97.2);

/// Outside the ground [`PLAN`] covers: a box over Maine.
const OUT_OF_VIEW: (f64, f64, f64, f64) = (44.0, 45.0, -70.0, -69.0);

fn a_feature_over(corners: (f64, f64, f64, f64)) -> OverlayFeature {
    let (min_lat, max_lat, min_lon, max_lon) = corners;
    OverlayFeature::new(
        vec![vec![vec![
            (min_lat, min_lon),
            (min_lat, max_lon),
            (max_lat, max_lon),
            (max_lat, min_lon),
        ]]],
        [255, 0, 0, 128],
        [0, 0, 0, 0],
        "T".into(),
        String::new(),
        HatchPattern::None,
    )
}

/// One alert, in force now, whose only polygon is over `corners`.
///
/// In force rather than expired because `NwsAlertHandler::paints_in` filters on
/// `admitted` before it culls: an alert outside the depicted instant is refused
/// for a reason that has nothing to do with the extent, and either case would
/// then pass with the cull deleted.
fn an_alert_over(
    corners: (f64, f64, f64, f64),
) -> squallar_overlays::render::overlay_state::FetchPayload {
    let now = chrono::Utc::now().naive_utc();
    let alerts = vec![squallar_overlays::nws::alert::NwsAlert {
        id: "urn:test:extent".into(),
        event: "Tornado Warning".into(),
        category: squallar_overlays::nws::alert::AlertCategory::Warning,
        severity: "Severe".parse().expect("a known severity"),
        urgency: "Immediate".parse().expect("a known urgency"),
        certainty: "Observed".parse().expect("a known certainty"),
        headline: None,
        description: String::new(),
        instruction: None,
        area_desc: String::new(),
        sender_name: String::new(),
        effective: String::new(),
        expires: String::new(),
        onset: None,
        ends: None,
        valid_from: Some(now - chrono::Duration::hours(1)),
        valid_until: Some(now + chrono::Duration::hours(1)),
        affected_zones: Vec::new(),
        features: Arc::new(vec![a_feature_over(corners)]),
    }];
    squallar_overlays::render::overlay_state::OverlayRegistry::nws_alerts_payload(alerts)
}

/// Turn `id` on in pane `idx`'s own state — the door a layer toggle takes, and
/// not `overlays.set_enabled`, which one `adopt_handler_state` would undo.
fn enable(app: &mut crate::app::App, idx: usize, id: &LayerId) {
    let mut registry = std::mem::take(&mut app.gui.overlays);
    if let Some(pane) = app.gui.pane_mut(idx) {
        pane.hydrate_layer_states(&registry, idx);
        pane.set_layer_enabled(&mut registry, idx, id, true);
    }
    app.gui.overlays = registry;
}

/// Deliver one alert round through the real channel and the real `Ingest`
/// drain, so the handler's state is written the way production writes it.
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

/// A one-pane app with the alerts layer on and one alert over `corners`.
///
/// **The precondition every case shares** is asserted here rather than in each
/// of them: `has_data` must be `true` in *both* fixtures. It is the extent-blind
/// answer these tests exist to stop being the last word, so a fixture where it
/// were false would prove nothing about the cull — the dispatch would already
/// have been refused a door earlier.
fn an_app_with_one_alert_over(corners: (f64, f64, f64, f64)) -> crate::app::App {
    let id = known::NWS_ALERTS;
    let mut app = crate::app::tests::n_pane_app(1, "KTLX");
    enable(&mut app, 0, &id);
    arrive(&mut app, &id, an_alert_over(corners));
    let (panes, overlays) = app.gui.panes_and_overlays_mut();
    assert!(
        overlays.has_data(&id, &panes[0].layer_ref(0, &id)),
        "precondition: the layer must hold data, or the dispatch is refused \
         for a reason that is not the extent",
    );
    app
}

fn a_request() -> super::OverlayRenderRequest {
    super::OverlayRenderRequest {
        geo_bounds: VIEWPORT,
        texture: PLAN,
        data_generation: TOKEN,
        zoom: ZOOM,
    }
}

/// **The saving, read off the funnel.**
///
/// A job the sink never received is the one statement that no picture was
/// rasterized which does not depend on a counter this land wrote. The alert is
/// real, in force, enabled and holds geometry — it is simply somewhere else.
#[test]
fn an_extent_with_no_feature_in_it_dispatches_no_raster_job() {
    let taken = Arc::new(Mutex::new(0usize));
    let _guard = squallar_worker::offload::install_test_worker(Box::new(CountingPort {
        taken: Arc::clone(&taken),
    }));
    let mut app = an_app_with_one_alert_over(OUT_OF_VIEW);

    app.spawn_overlay_render(vec![0], known::NWS_ALERTS, a_request(), None);

    assert_eq!(
        *taken.lock().expect("no poisoned lock"),
        0,
        "an alert in Maine put a raster job through the funnel for a pane \
         looking at Oklahoma",
    );
}

/// **The trap, and it is the important one.**
///
/// The refusal above must be delivered as a blank and never as "no response".
/// `prepare_job -> None` takes `App::clear_overlay_render_marks` and sends
/// nothing, which leaves the pane drawing whatever it had — the shape of a
/// layer that renders nothing while its popup still answers. A blank clears.
///
/// Read off the response the pane will actually be handed, not off an
/// intention: `OverlayPicture::Blank` is what `poll_overlay_render_results`
/// turns into `OverlayTextureCache::show_blank`, and the ticket terms are what
/// retire this dispatch's in-flight mark rather than some other one's.
#[test]
fn an_empty_extent_delivers_a_blank_rather_than_no_response() {
    let taken = Arc::new(Mutex::new(0usize));
    let _guard = squallar_worker::offload::install_test_worker(Box::new(CountingPort {
        taken: Arc::clone(&taken),
    }));
    let mut app = an_app_with_one_alert_over(OUT_OF_VIEW);

    app.spawn_overlay_render(vec![0], known::NWS_ALERTS, a_request(), None);

    let response = app
        .channels
        .overlay_render_receiver
        .recv_timeout(std::time::Duration::from_secs(10))
        .expect("a refused extent still answers the pane, or its ink stays up");
    // **Where the blank came from, and it matters.** A rasterizer that ran
    // would answer a blank too, and this case would then pass with the whole
    // refusal deleted. The sink took no job, so the only thing that could have
    // built this response is the extent refusal.
    assert_eq!(
        *taken.lock().expect("no poisoned lock"),
        0,
        "a job reached the funnel, so this blank is the rasterizer's answer \
         and says nothing about the refusal",
    );
    assert!(
        matches!(
            response.picture,
            Some(crate::channels::OverlayPicture::Blank { .. })
        ),
        "an empty extent answered {:?}; only a blank clears the pane",
        response.picture.as_ref().map(|p| p.ink()),
    );
    assert_eq!(
        response.generation, TOKEN,
        "the blank must answer THIS dispatch, or it retires another one's mark",
    );
    assert_eq!(
        response.overlay_kind,
        known::NWS_ALERTS,
        "the blank must be filed against the layer that was refused",
    );
    assert_eq!(
        response.geo_bounds,
        PLAN.coverage(&VIEWPORT),
        "the blank must carry the ground the raster would have covered",
    );
    assert_eq!(
        response.pane_indices,
        vec![0],
        "the blank must reach the pane that asked",
    );
}

/// **The anti-vacuity floor.**
///
/// A `paints_in` stuck at `false` — or a cull whose longitude frame is wrong —
/// satisfies both cases above and clears every overlay on the map. The only
/// thing that separates the saving from that is a dispatch which still happens
/// when the feature really is in view.
#[test]
fn an_extent_holding_a_feature_still_dispatches_its_raster() {
    let taken = Arc::new(Mutex::new(0usize));
    let _guard = squallar_worker::offload::install_test_worker(Box::new(CountingPort {
        taken: Arc::clone(&taken),
    }));
    let mut app = an_app_with_one_alert_over(IN_VIEW);

    app.spawn_overlay_render(vec![0], known::NWS_ALERTS, a_request(), None);

    assert_eq!(
        *taken.lock().expect("no poisoned lock"),
        1,
        "an alert inside the pane's own viewport was refused a raster",
    );
}
