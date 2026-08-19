//! The three polygon overlay dispatches are **described jobs**, end to end.
//!
//! `spawn_overlay_render`'s handler arm used to close over a rasterize
//! closure and hand it to the opaque funnel, whose wasm arm ran it inline on
//! the browser's one thread — 224 ms of measured gesture-end stall for the
//! layer set these kinds make up (measured at main@ebe0ad3b, 2026-08-12
//! web-baseline campaign; instrumentation 3673d316). These pin the
//! replacement, per kind: the
//! dispatch asks the handler for its described input (`prepare_job`), builds
//! a `JobRequest::Overlay`, and hands it to the funnel — so a browser with a
//! worker attached posts it across the port instead of paying the raster on
//! the frame. And a job that is never answered still un-wedges the pane.
//!
//! Reverting the dispatch to a closure fails
//! [`each_polygon_kind_dispatches_as_a_described_job_of_its_own_input`] by
//! shape — the port records nothing, because a closure is executed rather
//! than posted — and fails the un-wedge half by silence, since no failure
//! response can exist for a job that was never described. The hit-map kinds
//! took the same route a slice later (`hitmap_wire_tests`), the model grid
//! last (`model_wire_tests`), and the described set as a whole is pinned
//! from the other side by `rustdar-overlays`'
//! `every_texture_kind_rasterizes_as_a_described_job`.

use rustdar_egui::overlay_cache::OverlayTexturePlan;
use rustdar_geo::GeoBounds;
use rustdar_overlays::render::overlay_state::{OverlayFetchResult, OverlayKind};
use rustdar_overlays::types::{HatchPattern, OverlayFeature};
use std::sync::{Arc, Mutex};

/// A sink that records what the funnel hands it and takes every job —
/// `sites_wire_tests`' port, restated here because each test file owns its
/// double. The round trip is asserted on the recording, so the codec is on
/// this path exactly as it is on the browser's.
struct RecordingPort {
    taken: Arc<Mutex<Vec<(u64, rustdar_worker::offload::JobRequest)>>>,
}

impl rustdar_worker::offload::JobSink for RecordingPort {
    fn send(
        &self,
        id: u64,
        request: rustdar_worker::offload::JobRequest,
    ) -> Result<(), rustdar_worker::offload::JobRequest> {
        self.taken.lock().unwrap().push((id, request));
        Ok(())
    }
}

fn a_render_request() -> super::OverlayRenderRequest {
    super::OverlayRenderRequest {
        geo_bounds: GeoBounds {
            min_lat: 33.0,
            max_lat: 37.0,
            min_lon: -99.0,
            max_lon: -96.0,
        },
        texture: OverlayTexturePlan {
            width: 64,
            height: 48,
            overdraw: 0.0,
            pixels_per_point: 1.0,
        },
        data_generation: 5,
        zoom: 32,
    }
}

/// The codec-row label whose input type `job` carries — the naming half the
/// deleted `OverlayJobInput` match used to provide, restated over
/// `downcast_ref` so a dispatch that described another kind's input is still
/// caught by name. All seven texture inputs are listed; an input no row
/// claims panics.
fn described_label(job: &rustdar_source::job::DescribedJob) -> &'static str {
    use rustdar_overlays::render::rasterize as rz;
    if job.downcast_ref::<rz::AlertsInput>().is_some() {
        "overlay/alerts"
    } else if job.downcast_ref::<rz::OutlooksInput>().is_some() {
        "overlay/outlooks"
    } else if job.downcast_ref::<rz::DiscussionsInput>().is_some() {
        "overlay/discussions"
    } else if job.downcast_ref::<rz::SitesInput>().is_some() {
        "overlay/sites"
    } else if job.downcast_ref::<rz::ReportsInput>().is_some() {
        "overlay/reports"
    } else if job.downcast_ref::<rz::GlmStrikesInput>().is_some() {
        "overlay/glm"
    } else if job.downcast_ref::<rz::ModelDataInput>().is_some() {
        "overlay/model"
    } else {
        panic!("the dispatch described an input no codec row claims: {job:?}")
    }
}

/// A filled square inside [`a_render_request`]'s box, so every seeded kind
/// has geometry that would paint.
fn a_feature() -> OverlayFeature {
    OverlayFeature::new(
        vec![vec![vec![
            (34.2, -98.8),
            (34.2, -97.2),
            (35.8, -97.2),
            (35.8, -98.8),
        ]]],
        [255, 0, 0, 128],
        [0, 0, 0, 0],
        "T".into(),
        String::new(),
        HatchPattern::None,
    )
}

/// Give `app`'s registry the smallest data `kind`'s handler will describe.
///
/// Through `apply_fetch_result` — the same door a live fetch uses — so the
/// handler's own delivery path runs, not some test-only installer.
fn seed(app: &mut crate::app::App, kind: OverlayKind) {
    use rustdar_overlays::render::handlers::{alert, discussion, outlook};
    let data: rustdar_overlays::render::overlay_state::FetchPayload = match kind {
        OverlayKind::NwsAlerts => {
            let alert = rustdar_overlays::nws::alert::NwsAlert {
                id: "urn:test".into(),
                event: "Tornado Warning".into(),
                category: rustdar_overlays::nws::alert::AlertCategory::Warning,
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
                affected_zones: Vec::new(),
                features: Arc::new(vec![a_feature()]),
            };
            Box::new(alert::NwsAlertFetchResult(Ok(
                rustdar_overlays::nws::fetch::ActiveAlerts::whole(vec![alert]),
            )))
        }
        OverlayKind::SpcOutlook => {
            use rustdar_overlays::spc::outlook::{OutlookDay, OutlookProduct, SpcOutlook};
            // The outlook's "enabled" *is* its product set, and its data is
            // keyed by (day, product): the toggle has to precede the payload
            // for the two to meet — the same order `texture_tests::seed`
            // states.
            app.gui.overlays.set_enabled(kind, true);
            Box::new(outlook::SpcOutlookFetchResult {
                day: OutlookDay::Day1,
                product: OutlookProduct::Categorical,
                result: Ok(SpcOutlook {
                    day: OutlookDay::Day1,
                    product: OutlookProduct::Categorical,
                    valid: None,
                    expire: None,
                    features: vec![a_feature()],
                }),
            })
        }
        OverlayKind::SpcDiscussions => {
            let md = rustdar_overlays::spc::discussion::SpcDiscussion {
                number: 1,
                title: "Mesoscale Discussion #0001".into(),
                text: String::new(),
                link: String::new(),
                md_type: rustdar_overlays::spc::discussion::MdType::Convective,
                polygon: vec![vec![
                    (34.2, -98.8),
                    (34.2, -97.2),
                    (35.8, -97.2),
                    (35.8, -98.8),
                ]],
                feature: a_feature(),
                concerning: None,
            };
            Box::new(discussion::SpcDiscussionFetchResult(Ok(vec![md])))
        }
        other => panic!("{other:?} is not a polygon kind this fixture seeds"),
    };
    app.gui
        .overlays
        .apply_fetch_result(OverlayFetchResult { kind, data });
    // Keep the pane's stored per-layer config in step with the handler, the
    // way the UI does after every control change (`Gui::initialize_pane_enabled`
    // seeded pane defaults at startup, and `spawn_overlay_render` reloads
    // them at dispatch): without this, the dispatch would restore the
    // outlook's default empty product set over the toggle this seed just set,
    // and the test would be exercising a stale-config path rather than the
    // routing it is about.
    let configs = app.gui.overlays.save_pane_configs();
    if let Some(pane) = app.gui.pane_mut(0) {
        pane.overlay_configs = configs;
    }
}

fn in_flight(app: &mut crate::app::App, kind: OverlayKind) -> bool {
    app.gui
        .pane_mut(0)
        .expect("pane 0")
        .overlay_cache_mut(kind)
        .render_in_flight
}

/// Each polygon kind's dispatch posts **one described job** carrying **its
/// own input variant** — not a closure, and not another kind's payload — and
/// what it posts survives its own wire round trip and marks the pane in
/// flight.
///
/// All three kinds in one walk because the routing is one match arm: a kind
/// quietly moved back to the closure path posts nothing here, which is the
/// exact regression this test exists to name.
#[test]
fn each_polygon_kind_dispatches_as_a_described_job_of_its_own_input() {
    let taken = Arc::new(Mutex::new(Vec::new()));
    let _guard = rustdar_worker::offload::install_test_worker(Box::new(RecordingPort {
        taken: Arc::clone(&taken),
    }));

    let mut app = crate::app::tests::n_pane_app(1, "KTLX");
    for kind in [
        OverlayKind::NwsAlerts,
        OverlayKind::SpcOutlook,
        OverlayKind::SpcDiscussions,
    ] {
        seed(&mut app, kind);
        let before = taken.lock().unwrap().len();
        app.spawn_overlay_render(vec![0], kind, a_render_request());

        let posted = taken.lock().unwrap();
        assert_eq!(
            posted.len(),
            before + 1,
            "the {kind:?} dispatch did not hand the funnel exactly one \
             described job — the closure path is back, and on wasm that is \
             the inline gesture-end rasterization this slice removed",
        );
        let (_, request) = &posted[before];
        // The envelope destructure is irrefutable since WO-M7.2; the typed
        // downcast below is what proves the dispatch posted this kind.
        let rustdar_worker::offload::JobRequest { geometry, job } = request;
        assert_eq!(
            (geometry.width, geometry.height),
            (64, 48),
            "the plan's own dimensions"
        );
        let own_label = app
            .gui
            .overlays
            .job_codec(kind)
            .expect("every texture kind owns a codec row")
            .label;
        assert_eq!(
            described_label(job),
            own_label,
            "the {kind:?} dispatch described some other kind's input — the \
             worker would rasterize the wrong layer under this pane's marks",
        );
        // The browser's sink serialises exactly this value on its way out;
        // the round trip here keeps the codec on the recorded path.
        assert_eq!(
            rustdar_worker::offload::JobRequest::from_bytes(&request.to_bytes()).as_ref(),
            Some(request),
            "the posted {kind:?} job does not survive its own wire form",
        );
        drop(posted);
        assert!(
            in_flight(&mut app, kind),
            "the {kind:?} dispatch must have marked the pane in flight",
        );
    }
}

/// The failure path closes the loop for the polygon kinds the way it does
/// for sites: the worker dies with the job outstanding, the funnel fails it,
/// and the image-less response — the only thing that clears the named panes'
/// in-flight marks — reaches the channel. One kind stands for the three
/// because the deliver is one shared function (`overlay_job_deliver`, pinned
/// by `frame_thread_conversion_tests`); what is per-kind is the dispatch,
/// which the walk above covers.
#[test]
fn a_dead_worker_unwedges_an_alert_pane() {
    let taken = Arc::new(Mutex::new(Vec::new()));
    let _guard = rustdar_worker::offload::install_test_worker(Box::new(RecordingPort {
        taken: Arc::clone(&taken),
    }));

    let mut app = crate::app::tests::n_pane_app(1, "KTLX");
    seed(&mut app, OverlayKind::NwsAlerts);
    app.spawn_overlay_render(vec![0], OverlayKind::NwsAlerts, a_render_request());
    assert_eq!(
        taken.lock().unwrap().len(),
        1,
        "premise: the alert dispatch must have posted a described job",
    );
    assert!(
        in_flight(&mut app, OverlayKind::NwsAlerts),
        "premise for the un-wedge below: the dispatch must have marked the \
         pane in flight",
    );

    rustdar_worker::offload::abandon_worker("test: the worker died");
    let resp = app.channels.overlay_render_receiver.try_recv().expect(
        "a job the worker never answered sent nothing on the overlay \
         channel: the pane stays marked in flight forever and the alert \
         layer can never be asked for again",
    );
    assert!(resp.image.is_none(), "a failed job answered with a picture");
    assert_eq!(resp.overlay_kind, OverlayKind::NwsAlerts);
    assert_eq!(
        resp.pane_indices,
        vec![0],
        "the failure response must name every pane the dispatch marked, or \
         the poller cannot clear their marks",
    );
}
