use rustdar_egui::overlay_cache::OverlayTexturePlan;
use rustdar_geo::GeoBounds;
use rustdar_overlays::render::overlay_state::OverlayFetchResult;
use rustdar_overlays::types::{HatchPattern, OverlayFeature};
use rustdar_source::handler::PaneRef;
use rustdar_source::id::{LayerId, known};
use std::sync::{Arc, Mutex};

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
    } else if job.downcast_ref::<rz::GriddedInput>().is_some() {
        "overlay/model"
    } else {
        panic!("the dispatch described an input no codec row claims: {job:?}")
    }
}

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

/// Turn `id` on **in pane 0's own state**, the door a layer toggle takes.
///
/// Not `overlays.set_enabled(.., PaneMut::bare(0))`: a converted handler keeps
/// "on" in the pane, and a write to the registry alone is one
/// `adopt_handler_state` away from being undone — which is exactly what
/// happened here when the outlook took its pane in WO-M10c.
fn enable_on_pane0(app: &mut crate::app::App, id: &LayerId) {
    let mut registry = std::mem::take(&mut app.gui.overlays);
    if let Some(pane) = app.gui.pane_mut(0) {
        pane.hydrate_layer_states(&registry, 0);
        pane.set_layer_enabled(&mut registry, 0, id, true);
    }
    app.gui.overlays = registry;
}

fn seed(app: &mut crate::app::App, id: &LayerId) {
    use rustdar_overlays::render::handlers::{alert, discussion, outlook};
    let data: rustdar_overlays::render::overlay_state::FetchPayload = match id {
        id if *id == known::NWS_ALERTS => {
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
                valid_from: None,
                valid_until: None,
                affected_zones: Vec::new(),
                features: Arc::new(vec![a_feature()]),
            };
            Box::new(alert::NwsAlertFetchResult(Ok(
                rustdar_overlays::nws::fetch::ActiveAlerts::whole(vec![alert]),
            )))
        }
        id if *id == known::SPC_OUTLOOK => {
            use rustdar_overlays::spc::outlook::{OutlookDay, OutlookProduct, SpcOutlook};
            enable_on_pane0(app, id);
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
        id if *id == known::SPC_DISCUSSIONS => {
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
        other => panic!(
            "{} is not a polygon layer this fixture seeds",
            other.as_str()
        ),
    };
    app.gui.overlays.apply_fetch_result(
        OverlayFetchResult {
            kind: id.clone(),
            data,
        },
        &PaneRef::bare(0),
    );
    let registry_snapshot = std::mem::take(&mut app.gui.overlays);
    if let Some(pane) = app.gui.pane_mut(0) {
        pane.adopt_handler_state(&registry_snapshot);
    }
    app.gui.overlays = registry_snapshot;
}

fn in_flight(app: &mut crate::app::App, id: &LayerId) -> bool {
    app.gui
        .pane_mut(0)
        .expect("pane 0")
        .overlay_cache_mut(id)
        .render_in_flight
}

#[test]
fn each_polygon_kind_dispatches_as_a_described_job_of_its_own_input() {
    let taken = Arc::new(Mutex::new(Vec::new()));
    let _guard = rustdar_worker::offload::install_test_worker(Box::new(RecordingPort {
        taken: Arc::clone(&taken),
    }));

    let mut app = crate::app::tests::n_pane_app(1, "KTLX");
    for kind in [
        known::NWS_ALERTS,
        known::SPC_OUTLOOK,
        known::SPC_DISCUSSIONS,
    ] {
        seed(&mut app, &kind);
        let before = taken.lock().unwrap().len();
        app.spawn_overlay_render(vec![0], kind.clone(), a_render_request(), None);

        let posted = taken.lock().unwrap();
        assert_eq!(
            posted.len(),
            before + 1,
            "the {kind:?} dispatch did not hand the funnel exactly one \
             described job — the closure path is back, and on wasm that is \
             the inline gesture-end rasterization this slice removed",
        );
        let (_, request) = &posted[before];
        let rustdar_worker::offload::JobRequest { geometry, job } = request;
        assert_eq!(
            (geometry.width, geometry.height),
            (64, 48),
            "the plan's own dimensions"
        );
        let own_label = app
            .gui
            .overlays
            .job_codec(&kind)
            .expect("every texture kind owns a codec row")
            .label;
        assert_eq!(
            described_label(job),
            own_label,
            "the {kind:?} dispatch described some other kind's input — the \
             worker would rasterize the wrong layer under this pane's marks",
        );
        assert_eq!(
            rustdar_worker::offload::JobRequest::from_bytes(&request.to_bytes()).as_ref(),
            Some(request),
            "the posted {kind:?} job does not survive its own wire form",
        );
        drop(posted);
        assert!(
            in_flight(&mut app, &kind),
            "the {kind:?} dispatch must have marked the pane in flight",
        );
    }
}

#[test]
fn a_dead_worker_unwedges_an_alert_pane() {
    let taken = Arc::new(Mutex::new(Vec::new()));
    let _guard = rustdar_worker::offload::install_test_worker(Box::new(RecordingPort {
        taken: Arc::clone(&taken),
    }));

    let mut app = crate::app::tests::n_pane_app(1, "KTLX");
    seed(&mut app, &known::NWS_ALERTS);
    app.spawn_overlay_render(vec![0], known::NWS_ALERTS, a_render_request(), None);
    assert_eq!(
        taken.lock().unwrap().len(),
        1,
        "premise: the alert dispatch must have posted a described job",
    );
    assert!(
        in_flight(&mut app, &known::NWS_ALERTS),
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
    assert_eq!(resp.overlay_kind, known::NWS_ALERTS);
    assert_eq!(
        resp.pane_indices,
        vec![0],
        "the failure response must name every pane the dispatch marked, or \
         the poller cannot clear their marks",
    );
}
