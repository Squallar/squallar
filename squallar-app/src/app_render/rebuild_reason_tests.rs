//! **Why a whole-picture overlay raster was spent, counted — and the view
//! half told apart from the data half.**
//!
//! Overlay pictures are rasterized oversized so that a pan can move across the
//! picture's expanded ground without asking for a new one. At the shipped rung
//! that margin is 2.25x the pane's area, and what it buys is exactly the
//! `RerenderReason::PanCoverage` dispatches it prevents. Until this file there
//! was nothing to price it against: `OverlayTextureCache::needs_rerender`
//! collapsed nine branches into a bare `bool` and `ledger::note_dispatched`
//! took no argument, so the pan share of rebuilds was not merely unmeasured —
//! it was unknowable.
//!
//! **The vacuity this file is built against.** A reason counter that always
//! answered the same variant passes every naive check: the ledger runs, the
//! figures are readable, the sum balances. So the assertion is not "a reason
//! was recorded" but a **two-sided discrimination** over one scene, in two
//! phases that differ in exactly one thing:
//!
//! * the **pan** phase moves the map and delivers no data;
//! * the **data** phase delivers data and does not move the map.
//!
//! and each phase must show its own reasons and **not** the other's. A counter
//! stuck on one variant fails one of the two directions whichever variant it
//! is stuck on, and a counter wired to the wrong flag fails the phase it
//! misreads.
//!
//! Every door below is a production door, and the frame is
//! `idle_raster_tests`': `Gui::ui` decides, `App::process_gui_actions`
//! dispatches, a refusing sink makes the funnel run the real rasterizer on
//! this box, `App::poll_overlay_render_results` is the arrival and
//! `App::deliver_held_rasters` the promote. The layers are not that file's —
//! see `seeded_layers` for the one it drops and why. The view is moved by a
//! **real drag** — pointer events on the frame's own `RawInput`, through the
//! map's own drag handler — and never by writing the pane's centre, so what
//! the pan phase exercises is the door a user's finger goes through.
//!
//! In a test build the ledger is one set of counters per thread, so each
//! phase below reads this test's own dispatches and no sibling's — see
//! `squallar_egui::overlay_cache::ledger::sink` for the mechanism and the
//! premise it rests on, and
//! `the_isolation_these_readings_depend_on_is_on_in_this_binary` for the
//! proof that the switch reached this binary. What that replaced was a
//! crate-wide lock only readers took. The coverage raster this file's data
//! phase kept reading was **not** a sibling's, though: with the test alone in
//! its process the charge still landed, on this thread, and the reasons were
//! the scene's own — see `quiesce` for the coast and `reaches_the_network`
//! for the download.

use squallar_egui::overlay_cache::{RerenderReason, ledger};
use squallar_overlays::render::overlay_state::{OverlayFetchResult, SourceEvent};
use squallar_overlays::types::{HatchPattern, OverlayFeature};
use squallar_source::id::{LayerId, known};
use std::sync::Arc;

/// A sink that refuses every job, so the funnel runs it on this box and lands
/// real pixels at the plan's own size on the app's own channel. A fabricated
/// `ColorImage` is how a mismatch between what was asked for and what came
/// back would go unnoticed by exactly this test.
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

const SCREEN: egui::Vec2 = egui::vec2(1600.0, 900.0);

/// What a desktop adapter reports. Named rather than defaulted: egui's default
/// of 2048 clamps the overdraw away, and the overdraw band *is* the margin
/// this file measures the cost of.
const MAX_TEXTURE_SIDE: usize = 16384;

/// Frames spent letting the pipeline fill before either phase is measured.
const WARMUP_FRAMES: usize = 60;

/// Frames run after each view move, enough for the dispatch to rasterize,
/// arrive and promote before the next move — so the fling brake
/// (`hold_superseded && held.is_some()`) is never the thing under test.
const FRAMES_PER_STEP: usize = 12;

/// How many times the pan phase moves the map.
const PAN_STEPS: usize = 6;

/// How far each step drags, in screen pixels — a quarter of the pane's width.
///
/// The trigger sits at roughly 6% of a viewport of pan, wherever the map is
/// zoomed to: the overdraw band is `OVERDRAW_FRACTION / 2` of a viewport on
/// each side and `PAN_REBUILD_THRESHOLD` of that band is what trips the
/// coverage arm. A quarter viewport is four times that, so this leaves the
/// picture's expanded ground at every zoom rather than at a chosen one.
const PAN_STEP_PX: f32 = SCREEN.x * 0.25;

/// Frames one drag stroke is spread across, so the handler sees a per-frame
/// pointer delta rather than one teleport.
const DRAG_FRAMES: usize = 8;

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

fn an_alert_round(n: usize) -> squallar_overlays::render::overlay_state::FetchPayload {
    let now = chrono::Utc::now().naive_utc();
    let alerts = (0..n)
        .map(|i| squallar_overlays::nws::alert::NwsAlert {
            id: format!("urn:reason:{i}"),
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

fn a_reports_round(n: usize) -> squallar_overlays::render::overlay_state::FetchPayload {
    use squallar_overlays::render::handlers::reports::StormReportsFetchResult;
    use squallar_overlays::spc::reports::{StormReport, StormReportKind, StormReportRound};
    let reports = (0..n)
        .map(|i| StormReport {
            kind: StormReportKind::Tornado,
            time: format!("21{i:02}"),
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

fn enable(app: &mut crate::app::App, idx: usize, id: &LayerId) {
    let mut registry = std::mem::take(&mut app.gui.overlays);
    if let Some(pane) = app.gui.pane_mut(idx) {
        pane.hydrate_layer_states(&registry, idx);
        pane.set_layer_enabled(&mut registry, idx, id, true);
    }
    app.gui.overlays = registry;
}

fn a_discussion_round(n: usize) -> squallar_overlays::render::overlay_state::FetchPayload {
    use squallar_overlays::spc::discussion::{MdType, SpcDiscussion};
    let discussions = (0..n)
        .map(|i| {
            let (lat, lon) = (34.5 + i as f64 * 0.3, -98.0);
            SpcDiscussion {
                number: i as u32 + 1,
                title: format!("Mesoscale Discussion #{:04}", i + 1),
                text: String::new(),
                link: String::new(),
                md_type: MdType::Convective,
                polygon: vec![vec![
                    (lat - 0.25, lon - 0.25),
                    (lat - 0.25, lon + 0.25),
                    (lat + 0.25, lon + 0.25),
                    (lat + 0.25, lon - 0.25),
                    (lat - 0.25, lon - 0.25),
                ]],
                feature: a_polygon(lat, lon),
                concerning: None,
                valid_from: None,
                valid_until: None,
            }
        })
        .collect();
    squallar_overlays::render::overlay_state::OverlayRegistry::spc_discussions_payload(discussions)
}

/// Three texture layers whose data **this test alone delivers**.
///
/// Not `idle_raster_tests`' three: that scene seeds `RadarCoverage`, whose
/// content is the radar site table — a process-global `static`
/// (`squallar_radar::sites::table_generation`) that every other test in this
/// binary resolving a site moves, and which `Gui::republish_radar_sites_if_the_table_moved`
/// re-delivers into the pane on the next frame. `idle_raster_tests` reads the
/// generation on both sides of its window and excuses that layer's rasters
/// when it moved; a reason bracket cannot, because the ledger does not say
/// which layer a raster was for. Measured 2026-09-06 under the whole suite at
/// eight threads: the pan phase, which delivers no data, read `1 content, 2
/// sweep` beside its `27 pan`, all on this test's own thread, and direction 1
/// failed on the content raster. Which token moved was not read directly —
/// the site table is one candidate, the pane clock moving under a scan-info
/// arrival (see `reaches_the_network`) the other — and both are out of this
/// scene now: every layer here has a token only this file's `arrive` can
/// move, and the frame opens no socket.
fn seeded_layers() -> [LayerId; 3] {
    [
        known::NWS_ALERTS,
        known::STORM_REPORTS,
        known::SPC_DISCUSSIONS,
    ]
}

fn a_pane_with_three_texture_layers() -> crate::app::App {
    let mut app = crate::app::tests::n_pane_app(1, "KTLX");
    app.cached_dark_theme = Some(false);
    for id in seeded_layers() {
        enable(&mut app, 0, &id);
    }
    arrive(&mut app, &known::NWS_ALERTS, an_alert_round(3));
    arrive(&mut app, &known::STORM_REPORTS, a_reports_round(4));
    arrive(&mut app, &known::SPC_DISCUSSIONS, a_discussion_round(2));
    app
}

/// One frame of the real app in the real order: promote what landed, drain
/// what arrived, build the paint list, process what the draw loop asked for.
///
/// `events` ride in on the frame's own `RawInput`, which is the only door a
/// pan has — the map's centre is moved by the drag handler reading these, not
/// by this file writing `map_memory`. `squallar-app` does not depend on
/// `walkers` and this test does not make it: the events are plain
/// `egui::Event` values, shaped as `egui-winit` reports a mouse drag (a
/// `PointerMoved` in the same batch as each `PointerButton`; see
/// `squallar_egui::input_fidelity`, which is `pub(crate)` there).
fn one_frame(app: &mut crate::app::App, ctx: &egui::Context, time: f64, events: Vec<egui::Event>) {
    app.deliver_held_rasters();
    app.poll_overlay_render_results(ctx);
    ctx.begin_pass(egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, SCREEN)),
        time: Some(time),
        max_texture_side: Some(MAX_TEXTURE_SIDE),
        events,
        ..Default::default()
    });
    let actions = app.gui.ui(ctx);
    let _ = ctx.end_pass();
    // The network-reaching actions this frame emits are declined by the app
    // itself now — see `crate::app::offline`, which is where this scene's
    // local filter went and what the measurement below belongs to.
    app.process_gui_actions(actions);
    // The refused job runs on a thread of its own; give it the wall clock it
    // needs to land on the channel the next frame drains.
    std::thread::sleep(std::time::Duration::from_millis(2));
}

/// `n` quiet frames — no input at all, which is what makes the settle
/// countdown run and the pipeline finish.
fn frames(app: &mut crate::app::App, ctx: &egui::Context, clock: &mut f64, n: usize) {
    for _ in 0..n {
        *clock += 1.0 / 60.0;
        one_frame(app, ctx, *clock, Vec::new());
    }
}

/// Whether pane 0 is still doing anything a raster could be owed to: a
/// raster dispatched and not yet retired, a picture held short of the glass,
/// a settle countdown still running — or **the view still moving**. Read from
/// the pane itself: the [`RendersInFlight`] mark is set at the dispatch and
/// cleared at the arrival, so it is true for exactly the span a raster is in
/// the air, wherever the thread rasterizing it is scheduled; and walkers'
/// `MapMemory::animating` is true for exactly the span a released drag is
/// still coasting.
///
/// [`RendersInFlight`]: squallar_egui::overlay_cache::RendersInFlight
fn scene_in_motion(app: &crate::app::App) -> bool {
    let pane = app.gui.pane(0).expect("the fixture's pane");
    let pipeline_busy = pane.overlay_textures.values().any(|cache| {
        !cache.renders.is_empty() || cache.is_holding() || cache.settle_is_counting_down()
    });
    pipeline_busy || pane.map_memory.animating() || pane.map_memory.dragging()
}

/// Run quiet frames until the scene is at rest and the ledger has stopped
/// moving, then a few more.
///
/// **The phase boundary, and it has to be a real one.** Whatever the pan
/// phase set in motion and had not finished by the boundary is charged to the
/// data phase, and the data phase's premise — the map does not move — is then
/// false by the test's own doing.
///
/// **Rest is the scene's own state, not a still ledger.** This used to return
/// after the ledger had read the same for `QUIET_FRAMES` frames, and that is
/// a wall-clock proxy wearing a counter's clothes. What it missed, read off a
/// per-frame trace of the draw pass on 2026-09-06: **a released drag coasts.**
/// walkers' `Center::Inertia` carries the release velocity forward with a 0.2 s
/// time constant — about 0.92 per frame at this file's 60 Hz clock — and a
/// quarter-viewport stroke over eight frames coasts for tens of frames after
/// the release. Across the ten "quiet" frames the viewport's longitude read
/// 232.65, 233.12, 233.56, 233.96, 234.33, 234.68, 234.99, 235.28, 235.54,
/// 235.79 … still creeping, with nothing in flight and the ledger still. The
/// coverage margin gives out wherever that creep crosses it, which is set by
/// where the last picture happened to be placed — arrival timing — so 1 run
/// in 10 crossed it *after* the reset and read `1 pan` or `2 pan` in a data
/// phase that had moved nothing itself. The pan phase's own count wandered
/// (39, 41, 43 …) for the same reason.
///
/// So the boundary is now drawn on [`scene_in_motion`]: nothing in flight,
/// nothing held, no settle owed, and the map neither dragging nor coasting.
/// The quiet frames are the settle on top of that, not the criterion.
///
/// A scene that never comes to rest still fails here rather than hanging: the
/// frame bound is a hang guard well past the coast (tens of frames) plus the
/// settle, not a budget.
fn quiesce(app: &mut crate::app::App, ctx: &egui::Context, clock: &mut f64) {
    const QUIET_FRAMES: usize = 10;
    const MAX_FRAMES: usize = 2000;
    let mut last = ledger::totals();
    let mut still = 0usize;
    for _ in 0..MAX_FRAMES {
        frames(app, ctx, clock, 1);
        let now = ledger::totals();
        let at_rest = now == last && !scene_in_motion(app);
        still = if at_rest { still + 1 } else { 0 };
        last = now;
        if still >= QUIET_FRAMES {
            return;
        }
    }
    panic!(
        "the scene never came to rest across {MAX_FRAMES} quiet frames, so no \
         phase boundary here is clean: {}",
        breakdown(&last)
    );
}

fn moved(pos: egui::Pos2) -> egui::Event {
    egui::Event::PointerMoved(pos)
}

fn button(pos: egui::Pos2, pressed: bool) -> egui::Event {
    egui::Event::PointerButton {
        pos,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: egui::Modifiers::default(),
    }
}

/// Drag the map `dx` pixels leftwards across `DRAG_FRAMES` frames, which moves
/// the *view* east. A press, a run of moves and a release — one frame each, so
/// the drag handler sees a real pointer delta per frame exactly as it does
/// under a mouse.
fn drag_view_east(app: &mut crate::app::App, ctx: &egui::Context, clock: &mut f64, dx: f32) {
    let start = egui::pos2(SCREEN.x * 0.5, SCREEN.y * 0.5);
    *clock += 1.0 / 60.0;
    one_frame(app, ctx, *clock, vec![moved(start), button(start, true)]);
    for step in 1..=DRAG_FRAMES {
        let at = start - egui::vec2(dx * step as f32 / DRAG_FRAMES as f32, 0.0);
        *clock += 1.0 / 60.0;
        one_frame(app, ctx, *clock, vec![moved(at)]);
    }
    let end = start - egui::vec2(dx, 0.0);
    *clock += 1.0 / 60.0;
    one_frame(app, ctx, *clock, vec![moved(end), button(end, false)]);
}

/// A reading of every reason, as `(name, count)`, for a failure message that
/// says what was actually seen rather than only what was missing.
fn breakdown(t: &ledger::Totals) -> String {
    RerenderReason::ALL
        .iter()
        .map(|r| format!("{} {}", t.reason(*r), r.name()))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Rasters this reading charges to data rather than to the view: the arrival
/// door, plus the content arm's one-shot. See `RerenderReason`.
fn data_driven(t: &ledger::Totals) -> u64 {
    t.reason(RerenderReason::ArrivalDoor) + t.reason(RerenderReason::ContentOneShot)
}

/// **The reason is recorded, and it tells a view-driven rebuild from a
/// data-driven one.**
///
/// Two phases over one scene, differing in exactly one thing each — and each
/// asserted in both directions, because a counter stuck on any single variant
/// passes one direction and fails the other.
#[test]
fn the_dispatch_reason_separates_a_pan_from_a_data_arrival() {
    let _worker = squallar_worker::offload::install_test_worker(Box::new(RefusingPort));

    let ctx = egui::Context::default();
    let mut app = a_pane_with_three_texture_layers();
    let mut clock = 0.0;

    frames(&mut app, &ctx, &mut clock, WARMUP_FRAMES);

    // ── Phase A: the map moves, no data arrives ─────────────────────────
    quiesce(&mut app, &ctx, &mut clock);
    ledger::reset_for_test();
    for _ in 0..PAN_STEPS {
        drag_view_east(&mut app, &ctx, &mut clock, PAN_STEP_PX);
        frames(&mut app, &ctx, &mut clock, FRAMES_PER_STEP);
    }
    quiesce(&mut app, &ctx, &mut clock);
    let pan_phase = ledger::totals();

    // ── Phase B: data arrives, the map does not move ────────────────────
    ledger::reset_for_test();
    for round in 0..PAN_STEPS {
        arrive(&mut app, &known::NWS_ALERTS, an_alert_round(3 + round));
        arrive(&mut app, &known::STORM_REPORTS, a_reports_round(4 + round));
        frames(&mut app, &ctx, &mut clock, FRAMES_PER_STEP);
    }
    quiesce(&mut app, &ctx, &mut clock);
    let data_phase = ledger::totals();

    // ── The floor: both phases actually ran the dispatch ────────────────
    assert!(
        pan_phase.ran(),
        "the pan phase dispatched no raster at all, so every figure below is \
         trivially zero and this test proves nothing: {}",
        breakdown(&pan_phase)
    );
    assert!(
        data_phase.ran(),
        "the data phase dispatched no raster at all: {}",
        breakdown(&data_phase)
    );

    // ── The identity: every dispatch carries exactly one reason ─────────
    for (name, t) in [("pan", &pan_phase), ("data", &data_phase)] {
        assert!(
            t.reasons_balance(),
            "the {name} phase's reasons do not sum to its dispatches \
             ({} dispatched, reasons {}). A `RendersInFlight::record` reached \
             the ledger without passing through `note_dispatched`.",
            t.dispatched,
            breakdown(t)
        );
        assert_eq!(
            t.reason(RerenderReason::Unattributed),
            0,
            "the {name} phase recorded a raster with nothing armed, which is a \
             hole in the arming wiring rather than a kind of rebuild: {}",
            breakdown(t)
        );
    }

    // ── The anti-vacuity conjunct ───────────────────────────────────────
    //
    // A counter that always answered the same variant satisfies everything
    // above. This is the check it cannot pass: one scene, two reasons.
    let seen: std::collections::BTreeSet<&'static str> = RerenderReason::ALL
        .iter()
        .filter(|r| pan_phase.reason(**r) + data_phase.reason(**r) > 0)
        .map(|r| r.name())
        .collect();
    assert!(
        seen.len() >= 2,
        "only one distinct reason was ever observed ({seen:?}) across a scene \
         that both moved the view and delivered data. A reason counter with one \
         answer passes every other check in this file and measures nothing. \
         pan phase: {}; data phase: {}",
        breakdown(&pan_phase),
        breakdown(&data_phase)
    );

    // ── Direction 1: the pan phase is pan, and is NOT data ──────────────
    assert!(
        pan_phase.margin_avoidable() > 0,
        "moving the map across {PAN_STEPS} drags of {PAN_STEP_PX} px charged no \
         raster to the coverage arm, so the figure that prices the oversampling \
         margin cannot be read: {}",
        breakdown(&pan_phase)
    );
    assert_eq!(
        data_driven(&pan_phase),
        0,
        "no data arrived across the pan phase, yet {} of its {} dispatches were \
         charged to a data reason: {}",
        data_driven(&pan_phase),
        pan_phase.dispatched,
        breakdown(&pan_phase)
    );

    // ── Direction 2: the data phase is data, and is NOT pan ─────────────
    //
    // **The two directions together are the discrimination**, and either one
    // alone is passable by a broken counter: one stuck on `PanCoverage` passes
    // direction 1, one stuck on `ContentOneShot` passes direction 2, and
    // neither passes both.
    //
    // The pinned quantity is `PanCoverage` on each side rather than the wider
    // `view_driven`, because `PanCoverage` is the exact quantity the
    // oversampling margin buys off, so it is the one worth a contract. (This
    // note used to excuse a `ZoomSettled` in the data phase as "the data
    // phase's own arrivals moving the zoom 4 -> 7". Nothing in this scene
    // moves the zoom; that was the pane's NEXRAD download landing mid-scene
    // — see `reaches_the_network`.)
    assert!(
        data_driven(&data_phase) > 0,
        "delivering data charged no raster to a data reason: {}",
        breakdown(&data_phase)
    );
    assert_eq!(
        data_phase.margin_avoidable(),
        0,
        "the map did not move across the data phase, yet {} of its {} \
         dispatches were charged to the coverage arm. Either the reason is \
         wired to the wrong branch or the view moved when nothing asked it to: \
         {}",
        data_phase.margin_avoidable(),
        data_phase.dispatched,
        breakdown(&data_phase)
    );
}

/// The reading this lane exists to take, printed rather than asserted.
///
/// **An instrument, not a gate**, and `#[ignore]` for that reason: what it
/// reports is a property of the scene it drives, and a scene is not a
/// contract. Run it by name to re-take the measurement:
///
/// ```text
/// cargo test -p squallar-app --lib the_reason_breakdown -- --ignored --nocapture
/// ```
///
/// Every phase is quiesced before and after, so each block is the reasons that
/// phase's own stimulus caused and not the previous one's tail.
#[test]
#[ignore = "instrument: prints the reason breakdown, asserts nothing about it"]
fn the_reason_breakdown_over_a_pan_and_load_scene() {
    let _worker = squallar_worker::offload::install_test_worker(Box::new(RefusingPort));

    let ctx = egui::Context::default();
    let mut app = a_pane_with_three_texture_layers();
    let mut clock = 0.0;

    let say_zoom = |app: &mut crate::app::App, label: &str| {
        let z = app.gui.pane(0).expect("pane 0").map_memory.zoom();
        println!("  [{label}] pane zoom = {z}");
    };
    let say = |label: &str, t: &ledger::Totals| {
        println!(
            "{label:<22} {:>4} dispatched | {} | view-driven {} | margin-avoidable {}",
            t.dispatched,
            breakdown(t),
            t.view_driven(),
            t.margin_avoidable(),
        );
    };

    // Cold start: three texture layers with data, nothing having happened yet.
    ledger::reset_for_test();
    frames(&mut app, &ctx, &mut clock, WARMUP_FRAMES);
    quiesce(&mut app, &ctx, &mut clock);
    say_zoom(&mut app, "cold start");
    say("cold start", &ledger::totals());

    // Idle: the same pane, left alone.
    ledger::reset_for_test();
    frames(&mut app, &ctx, &mut clock, WARMUP_FRAMES);
    quiesce(&mut app, &ctx, &mut clock);
    say_zoom(&mut app, "idle");
    say("idle", &ledger::totals());

    // Pan only.
    ledger::reset_for_test();
    for _ in 0..PAN_STEPS {
        drag_view_east(&mut app, &ctx, &mut clock, PAN_STEP_PX);
        frames(&mut app, &ctx, &mut clock, FRAMES_PER_STEP);
    }
    quiesce(&mut app, &ctx, &mut clock);
    say_zoom(&mut app, "pan only");
    say("pan only", &ledger::totals());

    // Data only.
    ledger::reset_for_test();
    for round in 0..PAN_STEPS {
        arrive(&mut app, &known::NWS_ALERTS, an_alert_round(3 + round));
        arrive(&mut app, &known::STORM_REPORTS, a_reports_round(4 + round));
        frames(&mut app, &ctx, &mut clock, FRAMES_PER_STEP);
    }
    quiesce(&mut app, &ctx, &mut clock);
    say_zoom(&mut app, "data only");
    say("data only", &ledger::totals());

    // Both at once, which is what a live map under a moving hand looks like.
    ledger::reset_for_test();
    for round in 0..PAN_STEPS {
        arrive(&mut app, &known::NWS_ALERTS, an_alert_round(9 + round));
        drag_view_east(&mut app, &ctx, &mut clock, PAN_STEP_PX);
        frames(&mut app, &ctx, &mut clock, FRAMES_PER_STEP);
    }
    quiesce(&mut app, &ctx, &mut clock);
    say_zoom(&mut app, "pan + data");
    say("pan + data", &ledger::totals());
}

/// **What the oversampling margin actually buys, priced at every rung.**
///
/// The margin's justification is that a pan moves across the picture's
/// expanded ground without asking for a new picture. That is a trade with two
/// sides and the tree only ever stated one of them:
///
/// * a **wider** margin means fewer `PanCoverage` rebuilds per viewport
///   panned;
/// * and every picture — including the ones the margin did not prevent — is
///   bigger, by the square of the rung.
///
/// So the figure that decides it is neither the rebuild count nor the picture
/// size but their **product**: bytes rasterized per unit of pan, beside the
/// resident cost of one picture. `squallar_device_profile::fit::picture_bytes`
/// scales both sides by the rung, so 150 is 2.25x the pane's area, 125 is
/// 1.5625x and 100 is 1.0x — and `overdraw_for_oversample` turns the same rung
/// into the band the coverage arm measures against
/// (`(percent - 100) / 200`, so 0.25, 0.125 and 0.0).
///
/// Each rung gets a **fresh app**, warmed and quiesced before its counters are
/// zeroed, and then the identical drag script. An instrument, not a gate.
#[test]
#[ignore = "instrument: prices the oversampling margin at each rung"]
fn what_the_oversample_margin_buys_at_each_rung() {
    let _worker = squallar_worker::offload::install_test_worker(Box::new(RefusingPort));

    println!(
        "{:>5}  {:>9}  {:>6}  {:>4}  {:>12}  {:>10}  {:>12}",
        "rung", "1 picture", "disp", "pan", "bytes rast", "MB/drag", "resident MB"
    );
    for percent in squallar_device_profile::constants::OVERLAY_OVERSAMPLE_PERCENTS {
        let ctx = egui::Context::default();
        let mut app = a_pane_with_three_texture_layers();
        app.budgets.overlay_oversample_percent = percent;
        let mut clock = 0.0;

        frames(&mut app, &ctx, &mut clock, WARMUP_FRAMES);
        quiesce(&mut app, &ctx, &mut clock);
        ledger::reset_for_test();

        for _ in 0..PAN_STEPS {
            drag_view_east(&mut app, &ctx, &mut clock, PAN_STEP_PX);
            frames(&mut app, &ctx, &mut clock, FRAMES_PER_STEP);
        }
        quiesce(&mut app, &ctx, &mut clock);
        let t = ledger::totals();

        // One picture at this rung for this pane, from the planner's own
        // arithmetic rather than from a figure this file invents.
        let one = squallar_device_profile::fit::picture_bytes(
            [SCREEN.x as u32, SCREEN.y as u32],
            percent,
        );
        let mb = |b: u64| b as f64 / (1024.0 * 1024.0);
        // The drag script is the same at every rung, so "per drag" is a
        // constant denominator across the rows and the columns are comparable.
        println!(
            "{percent:>5}  {:>8.2}M  {:>6}  {:>4}  {:>11.1}M  {:>9.1}  {:>11.2}",
            mb(one),
            t.dispatched,
            t.margin_avoidable(),
            mb(t.picture_bytes),
            mb(t.picture_bytes) / PAN_STEPS as f64,
            mb(one) * 3.0,
        );
    }
    println!(
        "denominator: {PAN_STEPS} drags of {PAN_STEP_PX} px on a {}x{} pane, \
         3 texture layers; `resident MB` is one picture per layer.",
        SCREEN.x as u32, SCREEN.y as u32
    );
}

/// **The isolation every reading in this file depends on is switched on in
/// *this* binary.**
///
/// `squallar-egui`'s `a_sibling_threads_writes_stay_out_of_this_threads_figures`
/// proves the mechanism where the counters live. This one proves it reached
/// here, which is a separate fact: the counters are one set per thread in that
/// crate's own test build through `cfg(test)`, and in this one only through
/// the `test-support` feature on this crate's dev-dependency edge. Drop that
/// feature and every figure below silently goes back to being its neighbours'
/// dispatches as much as its own — with nothing else in the workspace to say
/// so.
#[test]
fn the_isolation_these_readings_depend_on_is_on_in_this_binary() {
    const SIBLING_DISPATCHES: u64 = 64;

    ledger::reset_for_test();
    let before = ledger::totals();

    let sibling = std::thread::spawn(|| {
        ledger::reset_for_test();
        for _ in 0..SIBLING_DISPATCHES {
            ledger::note_dispatched(RerenderReason::PanCoverage);
        }
        ledger::totals()
    })
    .join()
    .expect("the sibling thread ran to completion");

    assert_eq!(
        sibling.dispatched, SIBLING_DISPATCHES,
        "the sibling's own dispatches did not reach its own figures, so what \
         is asserted below is isolation from a thread that counted nothing",
    );
    assert_eq!(
        ledger::totals(),
        before,
        "{SIBLING_DISPATCHES} dispatches on another thread moved this \
         thread's figures, so this binary is back on one shared set of \
         counters and the pan/data discrimination above reads its siblings' \
         rasters as its own",
    );

    ledger::note_dispatched(RerenderReason::PanCoverage);
    assert_eq!(
        ledger::totals().dispatched,
        before.dispatched + 1,
        "a dispatch on this thread did not move this thread's own count, so \
         the equality above says 'nothing counts anywhere' rather than \
         'a sibling cannot reach me'",
    );
}
