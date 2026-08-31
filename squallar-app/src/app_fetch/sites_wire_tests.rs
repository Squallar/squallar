//! The radar-coverage overlay dispatch is a **described job**, end to end.
//!
//! This is the raster half of what used to be one `RadarSites` layer. The
//! markers and labels became a per-frame painter and stopped answering
//! `prepare_job` at all; the ground they carried — the network's 230 km
//! coverage — kept the dispatch under [`known::RADAR_COVERAGE`], so this is
//! where the wire for it is exercised.

use squallar_egui::overlay_cache::OverlayTexturePlan;
use squallar_geo::GeoBounds;
use squallar_source::id::known;
use std::sync::{Arc, Mutex};

/// A sink that records what the funnel hands it and takes every job, standing where
/// `squallar-web`'s `Port` stands.
struct RecordingPort {
    taken: Arc<Mutex<Vec<(u64, squallar_worker::offload::JobRequest)>>>,
}

impl squallar_worker::offload::JobSink for RecordingPort {
    fn send(
        &self,
        id: u64,
        request: squallar_worker::offload::JobRequest,
    ) -> Result<(), squallar_worker::offload::JobRequest> {
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
        .overlay_cache_mut(&known::RADAR_COVERAGE)
        .renders
        .holds(squallar_egui::overlay_cache::RenderSlot::WHOLE)
}

/// The dispatch posts a described overlay job to the installed sink — it does not run a
/// closure around it — and what it posts survives its own wire round trip, carries the
/// whole network's stations at their published positions, and marks the pane in flight.
#[test]
fn the_coverage_dispatch_is_a_described_job_and_a_dead_worker_unwedges_it() {
    let taken = Arc::new(Mutex::new(Vec::new()));
    let _guard = squallar_worker::offload::install_test_worker(Box::new(RecordingPort {
        taken: Arc::clone(&taken),
    }));

    let mut app = crate::app::tests::n_pane_app(1, "KTLX");
    // The coverage layer answers `prepare_job` only once it holds the table, and
    // the table arrives through the ordinary door rather than being read by the
    // handler — `squallar-overlays` may not name `squallar-radar`.
    app.gui.publish_radar_sites();
    app.spawn_overlay_render(vec![0], known::RADAR_COVERAGE, a_render_request(), None);

    {
        let posted = taken.lock().unwrap();
        assert_eq!(
            posted.len(),
            1,
            "the coverage dispatch did not hand the funnel exactly one described \
             job — the closure path is back, and on wasm that is the inline \
             rasterization this slice removed",
        );
        let (_, request) = &posted[0];
        // The envelope destructure is irrefutable since WO-M7.2; the typed downcast below
        // is what proves the dispatch posted this kind.
        let squallar_worker::offload::JobRequest { geometry, job } = request;
        assert_eq!(
            (geometry.width, geometry.height),
            (64, 48),
            "the plan's own dimensions"
        );
        let Some(coverage) =
            job.downcast_ref::<squallar_overlays::render::rasterize::CoverageInput>()
        else {
            panic!("the coverage dispatch posted an overlay job of another kind");
        };
        assert!(
            !coverage.sites.is_empty(),
            "the described job carries no sites at all, so the worker would \
             rasterize an empty layer",
        );
        // **The whole network, not the pane's station.** The wash is the
        // network's coverage, so nothing about which radar this pane is on may
        // reach the input — which is what lets two panes at one viewport share
        // a raster. A count short of the table is a row the wash would not
        // cover.
        assert_eq!(
            coverage.sites.len(),
            squallar_radar::sites::radars().len(),
            "the described job carries {} of the table's {} stations, so the \
             wash has holes in it where the dropped radars stand",
            coverage.sites.len(),
            squallar_radar::sites::radars().len(),
        );
        // The positions travel, rather than a count of zeroed rows: resolved
        // against the table by name so the fixture states a station and not a
        // pair of literals that could be re-typed to match a bug.
        let ktlx = squallar_radar::sites::get_radar_site("KTLX")
            .expect("KTLX is in the compiled site table");
        assert!(
            coverage
                .sites
                .iter()
                .any(|s| s.lat == ktlx.lat && s.lon == ktlx.lon),
            "no station in the described input stands where the table puts \
             KTLX, so the positions did not survive the describe",
        );
        // The browser's sink serialises exactly this value on its way out; the round trip
        // here keeps the codec on the recorded path.
        assert_eq!(
            squallar_worker::offload::JobRequest::from_bytes(&request.to_bytes()).as_ref(),
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
    squallar_worker::offload::abandon_worker("test: the worker died");
    let resp = app.channels.overlay_render_receiver.try_recv().expect(
        "a job the worker never answered sent nothing on the overlay channel: \
         the pane stays marked in flight forever and the coverage layer \
         can never be asked for again",
    );
    assert!(resp.image.is_none(), "a failed job answered with a picture",);
    assert_eq!(resp.overlay_kind, known::RADAR_COVERAGE);
    assert_eq!(
        resp.pane_indices,
        vec![0],
        "the failure response must name every pane the dispatch marked, or \
         the poller cannot clear their marks",
    );
}
