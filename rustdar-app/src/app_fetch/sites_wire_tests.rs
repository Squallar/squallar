//! The sites overlay dispatch is a **described job**, end to end.

use rustdar_egui::overlay_cache::OverlayTexturePlan;
use rustdar_geo::GeoBounds;
use rustdar_source::id::known;
use std::sync::{Arc, Mutex};

/// A sink that records what the funnel hands it and takes every job, standing where
/// `rustdar-web`'s `Port` stands.
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

fn in_flight(app: &mut crate::app::App) -> bool {
    app.gui
        .pane_mut(0)
        .expect("pane 0")
        .overlay_cache_mut(&known::RADAR_SITES)
        .render_in_flight
}

/// The dispatch posts a described overlay job to the installed sink — it does not run a
/// closure around it — and what it posts survives its own wire round trip, names this
/// pane's site as current, and marks the pane in flight.
#[test]
fn the_sites_dispatch_is_a_described_job_and_a_dead_worker_unwedges_it() {
    let taken = Arc::new(Mutex::new(Vec::new()));
    let _guard = rustdar_worker::offload::install_test_worker(Box::new(RecordingPort {
        taken: Arc::clone(&taken),
    }));

    let mut app = crate::app::tests::n_pane_app(1, "KTLX");
    app.spawn_overlay_render(vec![0], known::RADAR_SITES, a_render_request());

    {
        let posted = taken.lock().unwrap();
        assert_eq!(
            posted.len(),
            1,
            "the sites dispatch did not hand the funnel exactly one described \
             job — the closure path is back, and on wasm that is the inline \
             rasterization this slice removed",
        );
        let (_, request) = &posted[0];
        // The envelope destructure is irrefutable since WO-M7.2; the typed downcast below
        // is what proves the dispatch posted this kind.
        let rustdar_worker::offload::JobRequest { geometry, job } = request;
        assert_eq!(
            (geometry.width, geometry.height),
            (64, 48),
            "the plan's own dimensions"
        );
        let Some(sites) = job.downcast_ref::<rustdar_overlays::render::rasterize::SitesInput>()
        else {
            panic!("the sites dispatch posted an overlay job of another kind");
        };
        assert!(
            !sites.sites.is_empty(),
            "the described job carries no sites at all, so the worker would \
             rasterize an empty layer",
        );
        assert!(
            sites.sites.iter().any(|s| s.name == "KTLX" && s.is_current),
            "the pane's own site is not marked current in the described input",
        );
        // The browser's sink serialises exactly this value on its way out; the round trip
        // here keeps the codec on the recorded path.
        assert_eq!(
            rustdar_worker::offload::JobRequest::from_bytes(&request.to_bytes()).as_ref(),
            Some(request),
            "the posted job does not survive its own wire form",
        );
    }
    assert!(
        in_flight(&mut app),
        "premise for the un-wedge below: the dispatch must have marked the \
         pane in flight",
    );

    // The worker dies with the job outstanding.
    rustdar_worker::offload::abandon_worker("test: the worker died");
    let resp = app.channels.overlay_render_receiver.try_recv().expect(
        "a job the worker never answered sent nothing on the overlay channel: \
         the pane stays marked in flight forever and the site-marker layer \
         can never be asked for again",
    );
    assert!(resp.image.is_none(), "a failed job answered with a picture",);
    assert_eq!(resp.overlay_kind, known::RADAR_SITES);
    assert_eq!(
        resp.pane_indices,
        vec![0],
        "the failure response must name every pane the dispatch marked, or \
         the poller cannot clear their marks",
    );
}
