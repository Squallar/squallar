//! **Idle means idle: a settled overlay layer asks for nothing.**
//!
//! The whole-picture overlay pipeline is a loop — the draw loop notices a
//! stale raster, the funnel rasterizes it, the arrival holds it, the next
//! frame promotes it, and the frame after that asks the same question again.
//! Nothing in the tree closed that loop: every existing check either asks
//! `needs_rerender` directly with hand-built inputs
//! (`overlay_cache::settle_tests`, `coverage_dispatch_tests`) or stands a
//! cache up as a landed render would leave it and asks for one more frame
//! (`input_harness::tests::an_unchanged_warning_set_does_not_re_rasterize_the_alert_overlay`,
//! which also never advances the frame clock, so its `settled` is false
//! throughout and the settle arm is never the arm it exercises).
//!
//! What is missing from all of them is the **round trip**: the picture that
//! comes back has to satisfy the very question that asked for it, on the frame
//! after it reaches the glass, with every input recomputed from the live app
//! rather than restated. A pipeline that completes successfully and then
//! immediately decides it needs to do it again reads green on every one of the
//! checks above.
//!
//! So this drives the real thing. Every step below is a production door:
//! `Gui::ui` is the draw loop that decides, `App::process_gui_actions` is the
//! grouping and dispatch, the refusing sink makes `offload_job` run the real
//! rasterizer and land a real response on the real channel,
//! `App::poll_overlay_render_results` is the arrival, and
//! `App::deliver_held_rasters` is the promote. Nothing here fabricates a
//! texture, a token, a viewport or a plan.
//!
//! **The property is counted, never timed.** The quantity is the number of
//! `GuiAction::RenderOverlay` the draw loop emits over a window of frames, and
//! the frame clock is a number this file advances. Read off the actions rather
//! than off `overlay_cache::ledger`, which is process-global: this binary runs
//! its tests in parallel and a delta over a `static` would be another test's
//! rasters as much as this one's.

use squallar_overlays::render::overlay_state::{OverlayFetchResult, SourceEvent};
use squallar_overlays::types::{HatchPattern, OverlayFeature};
use squallar_source::id::{LayerId, known};
use std::sync::Arc;

/// A sink that refuses every job, so the funnel runs it on this box and lands
/// a real response — real pixels, at the plan's own size — on the app's own
/// channel. The alternative is a fabricated `ColorImage`, and a fabricated one
/// is how a size mismatch between what was asked for and what came back would
/// go unnoticed by exactly this test.
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

/// The viewport every frame below is drawn at. Landscape, and big enough that
/// the plan is a real desktop plan rather than one clamped by
/// `max_texture_side`.
const SCREEN: egui::Vec2 = egui::vec2(1600.0, 900.0);

/// What a desktop adapter reports. Named rather than defaulted: egui's own
/// default is 2048, which clamps the overdraw away and would quietly make this
/// a test about the zero-overdraw path.
const MAX_TEXTURE_SIDE: usize = 16384;

/// Frames spent letting the pipeline fill: dispatch, rasterize, arrive,
/// promote, several times over. Generous, because what is asserted is what
/// happens *after* it.
const WARMUP_FRAMES: usize = 60;

/// Frames the pane is then left alone for. Nothing moves the map, the clock's
/// layers, the theme or the data across any of them.
const IDLE_FRAMES: usize = 60;

fn a_polygon(lat: f64, lon: f64) -> OverlayFeature {
    OverlayFeature::new(
        vec![vec![vec![
            (lat - 0.25, lon - 0.25),
            (lat - 0.25, lon + 0.25),
            (lat + 0.25, lon + 0.25),
            (lat + 0.25, lon - 0.25),
            (lat - 0.25, lon - 0.25),
        ]]],
        [255, 0, 0, 128],
        [0, 0, 0, 255],
        "T".into(),
        String::new(),
        HatchPattern::None,
    )
}

/// One round of NWS alerts with real validity windows around the wall clock —
/// the shape a live feed delivers, so the as-of filter in
/// `NwsAlertHandler::paint_input` has something to do.
fn an_alert_round(n: usize) -> squallar_overlays::render::overlay_state::FetchPayload {
    let now = chrono::Utc::now().naive_utc();
    let alerts = (0..n)
        .map(|i| squallar_overlays::nws::alert::NwsAlert {
            id: format!("urn:idle:{i}"),
            event: "Tornado Warning".into(),
            category: squallar_overlays::nws::alert::AlertCategory::Warning,
            severity: "Severe".parse().expect("a CAP severity"),
            urgency: "Immediate".parse().expect("a CAP urgency"),
            certainty: "Observed".parse().expect("a CAP certainty"),
            headline: None,
            description: String::new(),
            instruction: None,
            area_desc: String::new(),
            sender_name: String::new(),
            effective: String::new(),
            expires: String::new(),
            onset: None,
            ends: None,
            valid_from: Some(now - chrono::Duration::minutes(10)),
            valid_until: Some(now + chrono::Duration::hours(6)),
            affected_zones: Vec::new(),
            features: Arc::new(vec![a_polygon(35.33 + i as f64 * 0.2, -97.27)]),
        })
        .collect();
    squallar_overlays::render::overlay_state::OverlayRegistry::nws_alerts_payload(alerts)
}

/// One round of storm reports — a layer that takes the **default**
/// `content_signature`, which is `data_generation()` and moves on every apply.
/// Alerts fold theirs out of the warning set, and the site table keys on a
/// per-pane counter; three different token shapes, one question.
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
            lat: 35.0 + i as f64 * 0.1,
            lon: -97.5 + i as f64 * 0.1,
            comments: String::new(),
        })
        .collect();
    Box::new(StormReportsFetchResult(Ok(StormReportRound {
        reports,
        failed_kinds: Vec::new(),
    })))
}

/// Push one arrival down the real channel and drain it through the frame
/// pump's `Ingest` phase — the door `App::arrived_overlay_asks` lives behind,
/// so the arrival half of the dispatch is exercised too.
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

/// Turn `id` on in pane `idx` through the pane's own state, which is where
/// "on" lives for every handler.
fn enable(app: &mut crate::app::App, idx: usize, id: &LayerId) {
    let mut registry = std::mem::take(&mut app.gui.overlays);
    if let Some(pane) = app.gui.pane_mut(idx) {
        pane.hydrate_layer_states(&registry, idx);
        pane.set_layer_enabled(&mut registry, idx, id, true);
    }
    app.gui.overlays = registry;
}

/// The layers this fixture puts a picture on the glass for.
fn seeded_layers() -> [LayerId; 3] {
    [
        known::NWS_ALERTS,
        known::STORM_REPORTS,
        known::RADAR_COVERAGE,
    ]
}

/// One pane on KTLX with three texture layers on and data behind each.
///
/// `RadarCoverage` is in the set because it is the one texture layer that needs
/// no network: the site table is compiled in and `publish_radar_sites` pushes it
/// through the ordinary arrival door. It carries the raster half of what used to
/// be `RadarSites`, which is a per-frame layer now and rasterizes nothing.
fn a_pane_with_three_texture_layers() -> crate::app::App {
    let mut app = crate::app::tests::n_pane_app(1, "KTLX");
    app.cached_dark_theme = Some(false);
    for id in seeded_layers() {
        enable(&mut app, 0, &id);
    }
    arrive(&mut app, &known::NWS_ALERTS, an_alert_round(3));
    arrive(&mut app, &known::STORM_REPORTS, a_reports_round(4));
    app.gui.publish_radar_sites();
    app
}

/// One frame of the real app, in the real order: promote what landed, drain
/// what arrived, build the paint list, then process what the draw loop asked
/// for. Answers what it asked for, by layer.
fn one_frame(app: &mut crate::app::App, ctx: &egui::Context, time: f64) -> Vec<LayerId> {
    app.deliver_held_rasters();
    app.poll_overlay_render_results(ctx);

    ctx.begin_pass(egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, SCREEN)),
        time: Some(time),
        max_texture_side: Some(MAX_TEXTURE_SIDE),
        ..Default::default()
    });
    let actions = app.gui.ui(ctx);
    let _ = ctx.end_pass();

    let asked = actions
        .iter()
        .filter_map(|a| match a {
            squallar_egui::actions::GuiAction::RenderOverlay { overlay_kind, .. } => {
                Some(overlay_kind.clone())
            }
            _ => None,
        })
        .collect();
    app.process_gui_actions(actions);
    asked
}

/// Which of `seeded_layers` has a picture on the glass in pane 0.
fn layers_on_the_glass(app: &crate::app::App) -> Vec<LayerId> {
    let pane = app.gui.pane(0).expect("the fixture's pane");
    seeded_layers()
        .into_iter()
        .filter(|id| {
            pane.overlay_cache(id)
                .is_some_and(|cache| cache.current().is_some())
        })
        .collect()
}

/// **The acceptance.** With no input, no map movement and no new data, a pane
/// whose texture layers each hold a picture asks for **no** further rasters.
///
/// Two non-vacuity floors, and each fails on its own. Without the first this
/// passes on a fixture that never rasterized anything at all — which is what a
/// pane with no enabled texture layer looks like, and is the same hole
/// `ledger::Totals::ran` exists to close on the rig side. Without the second
/// it passes on a pipeline whose answers never reach the screen, where a
/// settled `needs_rerender` would mean the opposite of what it says.
#[test]
fn an_idle_pane_asks_for_no_further_rasters_once_its_layers_hold_a_picture() {
    let _guard = squallar_worker::offload::install_test_worker(Box::new(RefusingPort));
    let ctx = egui::Context::default();
    let mut app = a_pane_with_three_texture_layers();

    let mut time = 100.0_f64;
    let mut warmed: Vec<LayerId> = Vec::new();
    for _ in 0..WARMUP_FRAMES {
        time += 1.0 / 60.0;
        for id in one_frame(&mut app, &ctx, time) {
            if !warmed.contains(&id) {
                warmed.push(id);
            }
        }
        // The refused job runs on its own thread; give it the frame's worth of
        // wall time it would have had in the app before the next drain.
        std::thread::sleep(std::time::Duration::from_millis(2));
    }

    assert!(
        !warmed.is_empty(),
        "floor: the fixture never asked for a single overlay raster, so the \
         zero asserted below is what a pane with no enabled texture layer \
         reports and says nothing about settling",
    );
    let on_glass = layers_on_the_glass(&app);
    assert_eq!(
        on_glass.len(),
        seeded_layers().len(),
        "floor: {} layer(s) asked for a raster and only {on_glass:?} ended up \
         with a picture on the glass, so a settled gate below would mean the \
         pipeline stopped rather than finished",
        warmed.len(),
    );

    // **The one thing "idle" cannot promise in this binary**, read on both
    // sides of the window rather than assumed. `RadarCoverage` holds a copy of
    // the site table, and `Gui::republish_radar_sites_if_the_table_moved`
    // re-delivers it whenever `squallar_radar::sites::table_generation` moves —
    // which bumps the handler's data generation and so its cache token. That
    // table is a process-global `static` that any other test in this binary can
    // resolve into while this one runs. A `RadarCoverage` raster asked for
    // across such a move is the layer doing exactly what it should. Measured:
    // with the whole `squallar-app` suite running in parallel this fires, and
    // with this test filtered to itself it does not.
    //
    // Read BEFORE the window and compared after, so the generation cannot be
    // read as unmoved because it moved while the window ran.
    let table_before = squallar_radar::sites::table_generation();
    let mut idle_asks: Vec<(usize, LayerId)> = Vec::new();
    for frame in 0..IDLE_FRAMES {
        time += 1.0 / 60.0;
        for id in one_frame(&mut app, &ctx, time) {
            idle_asks.push((frame, id));
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    let table_moved = squallar_radar::sites::table_generation() != table_before;

    let unexplained: Vec<&(usize, LayerId)> = idle_asks
        .iter()
        .filter(|(_, id)| !(table_moved && *id == known::RADAR_COVERAGE))
        .collect();
    assert!(
        unexplained.is_empty(),
        "an idle pane asked for {} more raster(s) over {IDLE_FRAMES} frames \
         with nothing moving: {unexplained:?}. Layers that had settled after \
         the warm-up: {on_glass:?}; the site table {} under this run. A \
         whole-picture overlay raster is tens of megabytes rasterized, \
         uploaded and thrown away, once per round trip, for ever.",
        unexplained.len(),
        if table_moved { "moved" } else { "held still" },
    );
}
