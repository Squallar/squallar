//! The two hit-map overlay dispatches — storm reports and GLM lightning —
//! are **described jobs**, end to end, with the half of a hit map that cannot
//! cross a message port captured at the dispatch and zipped at delivery.
//!
//! `spawn_overlay_render` used to route these kinds to `offload`'s opaque
//! closure arm precisely because their `HitMap` owned an
//! `Arc<dyn OverlayItem>` id map. The split — portable `HitCells` on the
//! reply, id_map captured page-side — is what let them leave it, and these
//! pin the replacement: the dispatch describes the kind's own input, captures
//! `hit_items` beside it, and the shared deliver zips the reply's cells with
//! exactly that capture — refusing, as a whole failed render, any reply whose
//! cells do not fit the dispatch's own grid and items, because a hit map
//! zipped across a mismatch is a hover that names the wrong report.
//!
//! The order-stability half of the zip contract — cells record row positions,
//! so the id_map must be the same enumeration — is pinned with a shuffled-map
//! negative control in `offload::tests`, beside the byte-parity gates.

use rustdar_egui::overlay_cache::OverlayTexturePlan;
use rustdar_overlays::render::overlay_state::{OverlayFetchResult, OverlayKind};
use rustdar_overlays::render::rasterize::HitCells;
use rustdar_overlays::types::GeoBounds;
use std::sync::{Arc, Mutex};

/// A sink that records what the funnel hands it and takes every job — each
/// test file owns its double; see `sites_wire_tests`.
struct RecordingPort {
    taken: Arc<Mutex<Vec<(u64, crate::offload::JobRequest)>>>,
}

impl crate::offload::JobSink for RecordingPort {
    fn send(
        &self,
        id: u64,
        request: crate::offload::JobRequest,
    ) -> Result<(), crate::offload::JobRequest> {
        self.taken.lock().unwrap().push((id, request));
        Ok(())
    }
}

/// A sink that refuses every job, so the funnel runs it here — which drives
/// the **whole** described path, dispatch through `execute` through the
/// shared deliver, on this thread, and lands the response on the channel.
struct RefusingPort;

impl crate::offload::JobSink for RefusingPort {
    fn send(
        &self,
        _id: u64,
        request: crate::offload::JobRequest,
    ) -> Result<(), crate::offload::JobRequest> {
        Err(request)
    }
}

/// The id_map argument's shape, named once for the deliver-level cases below.
type IdMap = Option<Vec<Arc<dyn rustdar_overlays::render::overlay_state::OverlayItem>>>;

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

/// Give `app`'s registry the smallest data `kind`'s handler will describe:
/// two rows at well-separated in-box positions, so both markers draw and the
/// delivered hit map has two distinct identities to answer with. Through
/// `apply_fetch_result` — the same door a live fetch uses.
fn seed(app: &mut crate::app::App, kind: OverlayKind) {
    use rustdar_overlays::render::handlers::reports::StormReportsFetchResult;
    let data: rustdar_overlays::render::overlay_state::FetchPayload = match kind {
        OverlayKind::StormReports => {
            use rustdar_overlays::spc::reports::{StormReport, StormReportKind, StormReportRound};
            let report = |report_kind, lat, lon| StormReport {
                kind: report_kind,
                time: "2015".into(),
                magnitude: None,
                location: "NORMAN".into(),
                county: "CLEVELAND".into(),
                state: "OK".into(),
                lat,
                lon,
                comments: String::new(),
            };
            app.gui.overlays.set_enabled(kind, true);
            Box::new(StormReportsFetchResult(Ok(StormReportRound {
                reports: vec![
                    report(StormReportKind::Tornado, 34.0, -98.2),
                    report(StormReportKind::Hail, 36.2, -96.8),
                ],
                failed_kinds: Vec::new(),
            })))
        }
        OverlayKind::Lightning => {
            use rustdar_overlays::glm::{
                GlmDataLevel, GlmFetchOutcome, GlmFetchResult, GlmFlash, GlmSatellite, RecordDrops,
            };
            // Real clock reads, because the dispatch captures its own
            // `Utc::now()` into the context: seconds-old flashes sit far
            // inside the handler's 300 s default window on any run.
            let now = chrono::Utc::now().naive_utc();
            let flash = |age_secs: i64, lat: f64, lon: f64| GlmFlash {
                lat,
                lon,
                energy: Some(1e-14),
                area: None,
                time: now - chrono::Duration::seconds(age_secs),
                satellite: GlmSatellite::GoesEast,
                level: GlmDataLevel::Flash,
            };
            app.gui.overlays.set_enabled(kind, true);
            Box::new(GlmFetchResult(Ok(GlmFetchOutcome {
                flashes: vec![flash(10, 34.0, -98.2), flash(20, 36.2, -96.8)],
                dead_feeds: Vec::new(),
                queried: Vec::new(),
                parse_failures: None,
                transport_failures: None,
                level_failures: Vec::new(),
                evaluated_levels: Vec::new(),
                listing_failures: Vec::new(),
                window_gaps: Vec::new(),
                record_drops: RecordDrops::default(),
            })))
        }
        other => panic!("{other:?} is not a hit-map kind this fixture seeds"),
    };
    app.gui
        .overlays
        .apply_fetch_result(OverlayFetchResult { kind, data });
    // Keep the pane's stored per-layer config in step with the handler —
    // `polygon_wire_tests::seed` says why.
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

/// Each hit-map kind's dispatch posts **one described job** carrying **its
/// own input variant**, what it posts survives its own wire round trip, and
/// the pane is marked in flight — the walk `polygon_wire_tests` makes, on the
/// two kinds that used to be the documented reason this path could not exist.
#[test]
fn each_hit_map_kind_dispatches_as_a_described_job_of_its_own_input() {
    let taken = Arc::new(Mutex::new(Vec::new()));
    let _guard = crate::offload::install_test_worker(Box::new(RecordingPort {
        taken: Arc::clone(&taken),
    }));

    let mut app = crate::app::tests::n_pane_app(1, "KTLX");
    for kind in [OverlayKind::StormReports, OverlayKind::Lightning] {
        seed(&mut app, kind);
        let before = taken.lock().unwrap().len();
        app.spawn_overlay_render(vec![0], kind, a_render_request());

        let posted = taken.lock().unwrap();
        assert_eq!(
            posted.len(),
            before + 1,
            "the {kind:?} dispatch did not hand the funnel exactly one \
             described job — the closure path is back, and on wasm that is \
             an inline gesture-end rasterization",
        );
        let (_, request) = &posted[before];
        let crate::offload::JobRequest::Overlay {
            width,
            height,
            input,
            ..
        } = request
        else {
            panic!("the {kind:?} dispatch posted a job of another kind, not JobRequest::Overlay");
        };
        assert_eq!((*width, *height), (64, 48), "the plan's own dimensions");
        let named = match input {
            crate::offload::OverlayJobInput::Reports(_) => OverlayKind::StormReports,
            crate::offload::OverlayJobInput::Glm(_) => OverlayKind::Lightning,
            crate::offload::OverlayJobInput::Alerts(_) => OverlayKind::NwsAlerts,
            crate::offload::OverlayJobInput::Outlooks(_) => OverlayKind::SpcOutlook,
            crate::offload::OverlayJobInput::Discussions(_) => OverlayKind::SpcDiscussions,
            crate::offload::OverlayJobInput::Sites(_) => OverlayKind::RadarSites,
        };
        assert_eq!(
            named, kind,
            "the {kind:?} dispatch described some other kind's input — the \
             worker would rasterize the wrong layer under this pane's marks",
        );
        assert_eq!(
            crate::offload::JobRequest::from_bytes(&request.to_bytes()).as_ref(),
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

/// The whole described path, end to end, for a hit-map kind: dispatch,
/// `execute`, the shared deliver's zip — and a response whose hit map
/// resolves probes to **the items this dispatch captured**, with both seeded
/// rows reachable. Driven by a refusing sink, so the funnel runs the job
/// through the same `execute` a worker would call.
#[test]
fn a_delivered_hit_map_resolves_clicks_to_the_dispatched_items() {
    let _guard = crate::offload::install_test_worker(Box::new(RefusingPort));

    for kind in [OverlayKind::StormReports, OverlayKind::Lightning] {
        let mut app = crate::app::tests::n_pane_app(1, "KTLX");
        seed(&mut app, kind);
        let items = app
            .gui
            .overlays
            .hit_items(kind)
            .expect("a seeded hit-map kind captures items");
        assert_eq!(items.len(), 2, "premise: two rows seeded");

        app.spawn_overlay_render(vec![0], kind, a_render_request());
        let resp = app
            .channels
            .overlay_render_receiver
            .recv_timeout(std::time::Duration::from_secs(10))
            .expect("the refused job ran here and delivered");
        assert_eq!(resp.overlay_kind, kind);
        assert!(
            resp.image.is_some(),
            "{kind:?}: the described render must answer a picture",
        );
        let hit_map = resp
            .hit_map
            .as_ref()
            .unwrap_or_else(|| panic!("{kind:?}: the response carries no hit map — clicks died"));

        // Probe every quarter-res cell centre of the 64×48 plan (16×12).
        let mut seen = [false, false];
        let mut probes_that_hit = 0;
        for qy in 0..12u32 {
            for qx in 0..16u32 {
                let u = (qx as f32 + 0.5) / 16.0;
                let v = (qy as f32 + 0.5) / 12.0;
                for hit in hit_map.hit_test(u, v) {
                    probes_that_hit += 1;
                    let index = items
                        .iter()
                        .position(|item| item.matches(hit.as_ref()))
                        .unwrap_or_else(|| {
                            panic!(
                                "{kind:?}: a hit resolved to an item this \
                                 dispatch never captured",
                            )
                        });
                    seen[index] = true;
                }
            }
        }
        assert!(
            probes_that_hit > 0,
            "{kind:?}: no probe hit anything, so the identity claims above \
             are vacuous",
        );
        assert_eq!(
            seen,
            [true, true],
            "{kind:?}: both seeded rows must be reachable through the \
             delivered hit map — a truncated id space loses whichever items \
             sit past the cut",
        );
    }
}

/// The deliver's mismatch guards, each against the shape the reply would
/// have if the two ends stopped being one build — and each answered as a
/// **whole failed render**, never as a picture with a wrong or missing hit
/// map, with the positive control first so every refusal below is the
/// mutation's doing.
#[test]
fn a_mismatched_hit_reply_is_a_failed_render_not_a_wrong_hit_map() {
    let mut app = crate::app::tests::n_pane_app(1, "KTLX");
    seed(&mut app, OverlayKind::StormReports);
    let items = app
        .gui
        .overlays
        .hit_items(OverlayKind::StormReports)
        .expect("items");
    let (width, height) = (64u32, 48u32);
    let rgba = vec![0u8; (width * height * 4) as usize];
    // 64/4 × 48/4, with one occupied cell naming item 1: the shape `execute`
    // really answers for this plan.
    let cells = |idx: u32, id: u32| {
        let mut occupied = std::collections::HashMap::new();
        occupied.insert(idx, vec![id]);
        HitCells {
            width: 16,
            height: 12,
            cells: occupied,
        }
    };

    let deliver = |id_map, output| {
        let response = super::OverlayRenderResponse {
            image: None,
            geo_bounds: a_render_request().geo_bounds,
            overlay_kind: OverlayKind::StormReports,
            generation: 5,
            pane_indices: vec![0],
            zoom: 32,
            hit_map: None,
        };
        crate::app::App::overlay_job_deliver(
            "test-deliver",
            width,
            height,
            id_map,
            response,
            app.channels.overlay_render_sender.clone(),
            None,
        )(Some(output));
        app.channels
            .overlay_render_receiver
            .recv_timeout(std::time::Duration::from_secs(10))
            .expect("every deliver answers, failure included")
    };

    // The positive control: a reply of this dispatch's own shape zips, and
    // the zipped map answers the item the cells name — at cell 33 = (1, 2).
    let ok = deliver(
        Some(items.clone()),
        crate::offload::JobOutput::OverlayRaster {
            rgba: rgba.clone(),
            hit_cells: Some(cells(33, 1)),
        },
    );
    assert!(ok.image.is_some(), "the well-shaped reply must deliver");
    let hit = ok
        .hit_map
        .expect("the well-shaped reply must zip")
        .hit_test((1.0 + 0.5) / 16.0, (2.0 + 0.5) / 12.0);
    assert_eq!(hit.len(), 1);
    assert!(
        hit[0].matches(items[1].as_ref()),
        "the zipped map must answer the item the cells name",
    );

    // Every mismatch: a grid that is not this dispatch's, an id past the
    // captured items, cells without items, items without cells, and a
    // buffer of the wrong length beside well-formed cells.
    let mismatches: Vec<(&str, IdMap, crate::offload::JobOutput)> = vec![
        (
            "a wrong-grid reply",
            Some(items.clone()),
            crate::offload::JobOutput::OverlayRaster {
                rgba: rgba.clone(),
                hit_cells: Some(HitCells {
                    width: 17,
                    height: 12,
                    cells: std::collections::HashMap::from([(33u32, vec![1u32])]),
                }),
            },
        ),
        (
            "an id past the captured items",
            Some(items.clone()),
            crate::offload::JobOutput::OverlayRaster {
                rgba: rgba.clone(),
                hit_cells: Some(cells(33, 2)),
            },
        ),
        (
            "cells with no items captured",
            None,
            crate::offload::JobOutput::OverlayRaster {
                rgba: rgba.clone(),
                hit_cells: Some(cells(33, 0)),
            },
        ),
        (
            "no cells where items were captured",
            Some(items.clone()),
            crate::offload::JobOutput::OverlayRaster {
                rgba: rgba.clone(),
                hit_cells: None,
            },
        ),
        (
            "a short buffer beside well-formed cells",
            Some(items.clone()),
            crate::offload::JobOutput::OverlayRaster {
                rgba: vec![0u8; 16],
                hit_cells: Some(cells(33, 1)),
            },
        ),
    ];
    for (what, id_map, output) in mismatches {
        let resp = deliver(id_map, output);
        assert!(
            resp.image.is_none(),
            "{what} delivered a picture; a mismatched reply is another \
             build's, and its hit map zipped anyway would name wrong items",
        );
        assert!(
            resp.hit_map.is_none(),
            "{what} delivered a hit map beside a failed render",
        );
    }
}

/// The failure path closes the loop for the hit-map kinds the way it does
/// for the polygon kinds: the worker dies with the job outstanding, and the
/// image-less response — the only thing that clears the named panes'
/// in-flight marks — reaches the channel.
#[test]
fn a_dead_worker_unwedges_a_reports_pane() {
    let taken = Arc::new(Mutex::new(Vec::new()));
    let _guard = crate::offload::install_test_worker(Box::new(RecordingPort {
        taken: Arc::clone(&taken),
    }));

    let mut app = crate::app::tests::n_pane_app(1, "KTLX");
    seed(&mut app, OverlayKind::StormReports);
    app.spawn_overlay_render(vec![0], OverlayKind::StormReports, a_render_request());
    assert_eq!(
        taken.lock().unwrap().len(),
        1,
        "premise: the reports dispatch must have posted a described job",
    );
    assert!(
        in_flight(&mut app, OverlayKind::StormReports),
        "premise for the un-wedge below",
    );

    crate::offload::abandon_worker("test: the worker died");
    let resp = app.channels.overlay_render_receiver.try_recv().expect(
        "a job the worker never answered sent nothing on the overlay \
         channel: the pane stays marked in flight forever",
    );
    assert!(resp.image.is_none(), "a failed job answered with a picture");
    assert!(
        resp.hit_map.is_none(),
        "a failed job answered with a hit map"
    );
    assert_eq!(resp.overlay_kind, OverlayKind::StormReports);
    assert_eq!(
        resp.pane_indices,
        vec![0],
        "the failure response must name every pane the dispatch marked",
    );
}
