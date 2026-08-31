//! **The WO-8 cancel seam.** A dispatch that supersedes a destination's
//! outstanding raster withdraws the superseded job at the offload registry, so
//! a raster nobody can use any more is dropped before it runs instead of
//! executing on the pool and being discarded at retire.
//!
//! The sink here is a recording double, so the "worker" runs nothing until the
//! test says so — which is what makes the supersede-versus-reply order
//! deterministic. The pool-side half of the seam (a withdrawn job is skipped
//! before `execute`) is pinned in `squallar_worker::offload`'s own tests; this
//! file owns the app-side halves: which dispatch withdraws which job, what a
//! grouped dispatch may not withdraw, and what the ledger counts.
//!
//! **The `cancelled` counter is process-global**, so the delta assertions
//! below are only about their own test's dispatches while *no other test in
//! the binary* is superseding concurrently. That is arranged by
//! [`crate::app::fetch::overlay_ledger_lock`], held for each test's whole
//! body. It used to be a file-private mutex here, which excluded the two
//! tests already inside the bracket and none of the 47 withdrawals coming
//! from elsewhere in the binary — the lock's own doc carries that census.

use squallar_egui::overlay_cache::OverlayTexturePlan;
use squallar_geo::GeoBounds;
use squallar_source::id::{LayerId, known};
use std::sync::{Arc, Mutex};

/// **The layer superseded, and the reason it is this one.**
///
/// This file is not about any layer; it is about `spawn_overlay_render`'s
/// supersede seam, and the layer is only a vehicle for producing a described
/// texture job. What the vehicle has to be is a **texture layer that answers
/// `prepare_job` with no feed, no clock and no fetch double** — otherwise a test
/// about withdrawal is also a test about the weather.
///
/// It was [`known::RADAR_SITES`] until the site layer split. That layer is
/// `PerFrameDirect` now: its marker, its station names and the selected
/// station's ring are all lengths in points, it answers `prepare_job` with
/// `None`, and `spawn_overlay_render`'s described-kinds arm no longer names it —
/// so a dispatch of it produces no job and the first assertion below would fail
/// on an empty worker.
///
/// [`known::RADAR_COVERAGE`] took the ground half of that layer and with it the
/// property this file needs. Its data is the site table compiled into
/// `squallar-radar`, and `Gui::new` pushes that table through the ordinary
/// arrival door to **both** halves of the split at construction — so
/// `crate::app::tests::n_pane_app` on its own is enough for `prepare_job` to
/// answer, with no feed, no clock and no fetch double. Every other texture layer
/// wants one of those.
///
/// Both halves of that were measured rather than argued: with `KIND` put back to
/// `RADAR_SITES` all three tests below fail on an empty worker (`left: 0,
/// right: 1`), and an explicit `publish_radar_sites` in the fixture changes
/// nothing, because `Gui::new` has already done it.
const KIND: LayerId = known::RADAR_COVERAGE;

/// A sink that records what the funnel hands it and takes every job — each
/// test file owns its double; see `sites_wire_tests`.
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

fn a_render_request(generation: u64) -> super::OverlayRenderRequest {
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
        data_generation: generation,
        zoom: 32,
    }
}

/// **A superseding dispatch withdraws the job it replaced.** Two dispatches
/// for one `(pane, layer)` destination leave the worker owing exactly one
/// answer — the newer job's — with the withdrawn job's `deliver` already run
/// as "nothing", and a reply the worker still produces for it refused.
///
/// Shown RED on the unmodified baseline: both jobs stayed owed
/// (`jobs_in_worker() == 2`), so the superseded raster executed on the pool
/// and was discarded only at retire.
#[test]
fn a_superseding_dispatch_withdraws_the_job_it_replaced() {
    let _ledger = crate::app::fetch::overlay_ledger_lock();
    let taken = Arc::new(Mutex::new(Vec::new()));
    let _guard = squallar_worker::offload::install_test_worker(Box::new(RecordingPort {
        taken: Arc::clone(&taken),
    }));
    let mut app = crate::app::tests::n_pane_app(1, "KTLX");

    let cancelled_before = squallar_egui::overlay_cache::ledger::totals().cancelled;

    app.spawn_overlay_render(vec![0], KIND, a_render_request(5), None);
    assert_eq!(
        squallar_worker::offload::jobs_in_worker(),
        1,
        "fixture: the first dispatch must be owed an answer",
    );

    // The map moved on: the same destination is dispatched again before the
    // first raster ran. The first job is now unusable — its arrival would be
    // refused at retire — so the dispatch withdraws it.
    app.spawn_overlay_render(vec![0], KIND, a_render_request(6), None);
    assert_eq!(
        squallar_worker::offload::jobs_in_worker(),
        1,
        "the superseded job is still owed an answer: it will execute on the \
         worker for a destination that has already moved past it, and its \
         raster will be discarded at retire",
    );
    assert_eq!(
        squallar_egui::overlay_cache::ledger::totals().cancelled - cancelled_before,
        1,
        "the withdrawal must be counted, or the storm this seam kills is \
         invisible in every telemetry line",
    );

    // The withdrawn job's deliver ran as "nothing to draw": one response, for
    // the old generation, carrying no picture.
    let resp = app
        .channels
        .overlay_render_receiver
        .try_recv()
        .expect("withdrawing a job must still deliver, or its marks leak");
    assert_eq!(resp.generation, 5, "the withdrawn dispatch answers first");
    assert!(
        resp.image.is_none(),
        "a withdrawn job must answer nothing — a picture here means it ran",
    );

    // The worker answers the withdrawn job anyway (it had already been
    // posted): the late reply is refused, not delivered twice.
    let first_id = taken.lock().unwrap()[0].0;
    squallar_worker::offload::deliver_job_reply(first_id, None);
    assert!(
        app.channels.overlay_render_receiver.try_recv().is_err(),
        "a late reply for a withdrawn job must not deliver a second response",
    );

    // The newer job is untouched: still owed, and its reply still lands.
    let second_id = taken.lock().unwrap()[1].0;
    squallar_worker::offload::deliver_job_reply(second_id, None);
    let resp = app
        .channels
        .overlay_render_receiver
        .try_recv()
        .expect("the superseding job's own reply must still deliver");
    assert_eq!(resp.generation, 6);
}

/// **A grouped job is withdrawn only when every destination it serves has
/// moved past it.** One dispatch for two panes, then a supersede on one of
/// them: the job survives — pane 1 is still waiting on it — and only the
/// supersede of the last destination withdraws it.
#[test]
fn a_grouped_job_survives_until_its_last_destination_supersedes() {
    let _ledger = crate::app::fetch::overlay_ledger_lock();
    let taken = Arc::new(Mutex::new(Vec::new()));
    let _guard = squallar_worker::offload::install_test_worker(Box::new(RecordingPort {
        taken: Arc::clone(&taken),
    }));
    let mut app = crate::app::tests::n_pane_app(2, "KTLX");

    app.spawn_overlay_render(vec![0, 1], KIND, a_render_request(5), None);
    assert_eq!(squallar_worker::offload::jobs_in_worker(), 1);

    app.spawn_overlay_render(vec![0], KIND, a_render_request(6), None);
    assert_eq!(
        squallar_worker::offload::jobs_in_worker(),
        2,
        "pane 1 still waits on the grouped job; withdrawing it for pane 0's \
         supersede alone starves pane 1",
    );
    assert!(
        app.channels.overlay_render_receiver.try_recv().is_err(),
        "nothing was withdrawn, so nothing may have delivered",
    );

    app.spawn_overlay_render(vec![1], KIND, a_render_request(7), None);
    assert_eq!(
        squallar_worker::offload::jobs_in_worker(),
        2,
        "the grouped job's last destination superseded: it is withdrawn, and \
         only the two live jobs remain owed",
    );
    let resp = app
        .channels
        .overlay_render_receiver
        .try_recv()
        .expect("the withdrawal delivers the grouped job as nothing");
    assert_eq!(resp.generation, 5);
    assert!(resp.image.is_none());
}

/// **A destination whose job already answered withdraws nothing.** The normal
/// serial cadence — dispatch, deliver, dispatch — must not count a cancel or
/// deliver anything extra: the seam is for jobs still owed, not bookkeeping
/// left behind by finished ones.
#[test]
fn a_new_dispatch_after_a_delivered_answer_withdraws_nothing() {
    let _ledger = crate::app::fetch::overlay_ledger_lock();
    let taken = Arc::new(Mutex::new(Vec::new()));
    let _guard = squallar_worker::offload::install_test_worker(Box::new(RecordingPort {
        taken: Arc::clone(&taken),
    }));
    let mut app = crate::app::tests::n_pane_app(1, "KTLX");

    let cancelled_before = squallar_egui::overlay_cache::ledger::totals().cancelled;

    app.spawn_overlay_render(vec![0], KIND, a_render_request(5), None);
    let first_id = taken.lock().unwrap()[0].0;
    squallar_worker::offload::deliver_job_reply(first_id, None);
    assert_eq!(squallar_worker::offload::jobs_in_worker(), 0);
    let _ = app.channels.overlay_render_receiver.try_recv();

    // The pane's cache retired the ticket with the delivery in production;
    // here the fixture only cares that the *record* of the finished job does
    // not read as something to withdraw.
    app.spawn_overlay_render(vec![0], KIND, a_render_request(6), None);
    assert_eq!(squallar_worker::offload::jobs_in_worker(), 1);
    assert_eq!(
        squallar_egui::overlay_cache::ledger::totals().cancelled - cancelled_before,
        0,
        "a finished job was counted as cancelled; the counter no longer \
         means what the telemetry line says",
    );
    assert!(
        app.channels.overlay_render_receiver.try_recv().is_err(),
        "nothing was withdrawn, so nothing may have delivered",
    );
}
