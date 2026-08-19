//! The model-grid overlay dispatch is a **described job**, end to end — the
//! last kind through the wire, and the one that used to be the documented
//! reason `spawn_overlay_render` kept an opaque closure arm at all.
//!
//! These pin the replacement from the dispatch side: the handler answers its
//! `prepare_job` (the grid by `Arc`, so describing costs a refcount), the
//! dispatch builds a `JobRequest::Overlay` carrying the described
//! `ModelDataInput`, and the funnel posts it — so a browser with
//! a worker attached rasterizes the grid across the port instead of paying
//! the raster inline on a gesture-settle frame. What the sink serialises is
//! the projection-window **cut** of that grid, and executing the recorded
//! request directly and through its own bytes must paint identical pixels;
//! the codec-level twin of that claim (with its floors and controls) is
//! `offload::tests::the_model_render_is_byte_identical_direct_and_via_the_wire`.
//!
//! Reverting the dispatch to a closure fails
//! [`the_model_dispatch_is_a_described_job_of_the_whole_grid`] by shape —
//! the port records nothing, because a closure is executed rather than
//! posted — and the un-wedge half by silence, since no failure response can
//! exist for a job that was never described. The described set as a whole is
//! pinned from the other side by `rustdar-overlays`'
//! `every_texture_kind_rasterizes_as_a_described_job`.

use rustdar_egui::overlay_cache::OverlayTexturePlan;
use rustdar_geo::GeoBounds;
use rustdar_overlays::render::overlay_state::{OverlayFetchResult, OverlayKind};
use std::sync::{Arc, Mutex};

/// A sink that records what the funnel hands it and takes every job —
/// `polygon_wire_tests`' port, restated because each test file owns its
/// double.
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

/// A 6×5 CIN grid over the viewport — the handler's default parameter, so no
/// control has to be driven — with values alternating between a painting
/// −300 J/kg and a sub-threshold 0, so the raster genuinely paints and the
/// parity below is not over a blank buffer.
fn a_seedable_grid() -> rustdar_overlays::hrrr::HrrrGridData {
    use rustdar_overlays::hrrr::{GridCoords, HrrrGridData, ModelParameter};
    let parameter = ModelParameter::SurfaceBasedCin;
    let (ni, nj) = (6usize, 5usize);
    let values: Vec<f32> = (0..ni * nj)
        .map(|k| if k % 2 == 0 { -300.0 } else { 0.0 })
        .collect();
    let mut lats = Vec::with_capacity(ni * nj);
    let mut lons = Vec::with_capacity(ni * nj);
    for j in 0..nj {
        for i in 0..ni {
            lats.push(36.6 - 3.2 * (j as f64 / (nj - 1) as f64));
            lons.push(-98.6 + 2.2 * (i as f64 / (ni - 1) as f64));
        }
    }
    let (visible_points, value_range) =
        rustdar_overlays::hrrr::summarize_values(&values, parameter);
    HrrrGridData {
        parameter,
        values,
        coords: GridCoords::Explicit { lats, lons },
        ni,
        nj,
        bounds: GeoBounds {
            min_lat: 33.4,
            max_lat: 36.6,
            min_lon: -98.6,
            max_lon: -96.4,
        },
        ref_time: chrono::NaiveDate::from_ymd_opt(2026, 8, 14)
            .unwrap()
            .and_hms_opt(12, 0, 0)
            .unwrap(),
        forecast_hour: parameter.forecast_hour(),
        visible_points,
        value_range,
    }
}

/// Seed the registry through `apply_fetch_result` — the same door a live
/// fetch uses — and keep the pane's stored per-layer config in step, the way
/// `polygon_wire_tests::seed` explains.
fn seed(app: &mut crate::app::App) {
    let data: rustdar_overlays::render::overlay_state::FetchPayload = Box::new(
        rustdar_overlays::hrrr::HrrrFetchResult(Ok(a_seedable_grid())),
    );
    app.gui.overlays.apply_fetch_result(OverlayFetchResult {
        kind: OverlayKind::ModelData.id(),
        data,
    });
    let configs = app.gui.overlays.save_pane_configs();
    if let Some(pane) = app.gui.pane_mut(0) {
        pane.overlay_configs = configs;
    }
}

fn in_flight(app: &mut crate::app::App) -> bool {
    app.gui
        .pane_mut(0)
        .expect("pane 0")
        .overlay_cache_mut(&OverlayKind::ModelData.id())
        .render_in_flight
}

/// The dispatch posts **one described job** carrying the model input — the
/// grid whole, by `Arc`, so the native transport moves a refcount and the
/// window cut happens only where bytes are built — and what it posts
/// rasterizes byte-identically directly and through its own wire form.
#[test]
fn the_model_dispatch_is_a_described_job_of_the_whole_grid() {
    let taken = Arc::new(Mutex::new(Vec::new()));
    let _guard = rustdar_worker::offload::install_test_worker(Box::new(RecordingPort {
        taken: Arc::clone(&taken),
    }));

    let mut app = crate::app::tests::n_pane_app(1, "KTLX");
    seed(&mut app);
    app.spawn_overlay_render(vec![0], OverlayKind::ModelData, a_render_request());

    let posted = taken.lock().unwrap();
    assert_eq!(
        posted.len(),
        1,
        "the model dispatch did not hand the funnel exactly one described \
         job — the closure path is back, and on wasm that is the inline \
         gesture-end rasterization S5d removed",
    );
    let (_, request) = &posted[0];
    // The envelope destructure is irrefutable since WO-M7.2; the typed
    // downcast below is what proves the dispatch posted this kind.
    let rustdar_worker::offload::JobRequest { geometry, job } = request;
    assert_eq!(
        (geometry.width, geometry.height),
        (64, 48),
        "the plan's own dimensions"
    );
    let Some(model) = job.downcast_ref::<rustdar_overlays::render::rasterize::ModelDataInput>()
    else {
        panic!("the model dispatch described some other kind's input: {job:?}");
    };
    assert!(
        matches!(
            model,
            rustdar_overlays::render::rasterize::ModelDataInput::Whole(_)
        ),
        "the dispatch pre-cut the grid on the frame thread; the `Whole` \
         carry exists so the memcpy happens only where bytes must be built",
    );

    // The browser's sink serialises exactly this value on its way out. The
    // whole-grid carry canonicalises to its window there, so the round trip
    // is asserted on the *pixels*: direct execution of the recorded request
    // and execution of its own bytes must paint the same picture — with a
    // painted floor, or the equality would hold over a blank buffer.
    let direct = rustdar_worker::offload::execute(request)
        .and_then(|out| out.take::<rustdar_overlays::render::rasterize::RasterizeOutput>())
        .expect("the posted model job rasterizes")
        .rgba;
    assert!(
        direct.iter().skip(3).step_by(4).any(|&a| a > 0),
        "the seeded grid painted nothing, so the wire parity below is \
         vacuous",
    );
    let via_wire = rustdar_worker::offload::execute_bytes(&request.to_bytes())
        .and_then(|out| out.take::<rustdar_overlays::render::rasterize::RasterizeOutput>())
        .expect("the posted model job survives its own wire form")
        .rgba;
    assert_eq!(
        via_wire, direct,
        "the posted model job paints differently through its own wire form",
    );

    drop(posted);
    assert!(
        in_flight(&mut app),
        "the model dispatch must have marked the pane in flight",
    );
}

/// The failure path closes the loop: the worker dies with the model job
/// outstanding, the funnel fails it, and the image-less response — the only
/// thing that clears the named panes' in-flight marks — reaches the channel.
#[test]
fn a_dead_worker_unwedges_a_model_pane() {
    let taken = Arc::new(Mutex::new(Vec::new()));
    let _guard = rustdar_worker::offload::install_test_worker(Box::new(RecordingPort {
        taken: Arc::clone(&taken),
    }));

    let mut app = crate::app::tests::n_pane_app(1, "KTLX");
    seed(&mut app);
    app.spawn_overlay_render(vec![0], OverlayKind::ModelData, a_render_request());
    assert_eq!(
        taken.lock().unwrap().len(),
        1,
        "premise: the model dispatch must have posted a described job",
    );
    assert!(
        in_flight(&mut app),
        "premise for the un-wedge below: the dispatch must have marked the \
         pane in flight",
    );

    rustdar_worker::offload::abandon_worker("test: the worker died");
    let resp = app.channels.overlay_render_receiver.try_recv().expect(
        "a job the worker never answered sent nothing on the overlay \
         channel: the pane stays marked in flight forever and the model \
         layer can never be asked for again",
    );
    assert!(resp.image.is_none(), "a failed job answered with a picture");
    assert_eq!(resp.overlay_kind, OverlayKind::ModelData);
    assert_eq!(
        resp.pane_indices,
        vec![0],
        "the failure response must name every pane the dispatch marked, or \
         the poller cannot clear their marks",
    );
}
