//! **What a GLM loop puts on the glass, frame by frame** — the user's third
//! report of the same defect, at the layer the two previous rounds did not
//! reach.
//!
//! *"still broken! GLM STILL only works on the first frame of a loop"* —
//! earlier phrased *"just a single frame at the beginning (or end, hard to
//! tell) has them, the rest are blank."*
//!
//! # Why this suite exists beside the fetch suite's own sweep
//!
//! `squallar-overlays`' `a_poll_landing_on_the_loops_oldest_frame_still_fills_the_whole_sweep`
//! is green and stays green. Read its constants: `SWEEP_SPAN_SECS = 3600`,
//! thirteen frames 300 s apart. That is a **radar-shaped** loop — one where
//! the Lookback slider's own default happens to equal the window the loop is
//! listed over. The user's is not. A satellite loop declares
//! `min_loop_frames() = 13` at an hourly step, so `Gui::loop_span_secs_for`
//! raises the window it is listed over to **twelve hours** while the slider
//! still reads one, and `SourceHandler::depicted_span_secs` — the only thing
//! the poll was told — carried the slider. One frame of thirteen was inside
//! it. Every other frame drew nothing.
//!
//! # What is driven and what is arranged
//!
//! The transport is the **real** `GmgsiHandler`, its listing and granules
//! handed in on the two production arrival paths, exactly as
//! `gmgsi_loop_tests` does and for the same reason (the bytes are 7.5 MB S3
//! objects). Playback is the real `advance_loop_playback`. The lightning
//! raster is described by the real `GlmHandler::prepare_job` through the real
//! `spawn_overlay_render`, drawn by the real `rasterize_glm_strikes`, filed by
//! the real `poll_overlay_render_results`, and read back through the real
//! `PaneState::overlay_texture_on_screen` — the draw fork itself.
//!
//! **One thing is modelled**: the S3 round. [`retained_by`] keeps the flashes
//! the ask the app builds says the pane depicts — `FetchConfig::depicted_frames`
//! where the pane names any, `depicted_span_secs` where it does not, which is
//! that field's documented contract. It is not the *implementation* of
//! `fetch_glm_flashes`; that round is pinned against a real S3 archive in
//! `squallar-overlays`. What this suite adds is the half no fetch test can see:
//! the ask the app makes, and the picture that reaches the glass.

use squallar_geo::GeoBounds;
use squallar_source::handler::SourceEvent;
use squallar_source::id::known;
use squallar_source::time::{FrameListing, FrameStamp};
use std::sync::{Arc, Mutex};

use squallar_overlays::glm::{
    GlmDataLevel, GlmFetchOutcome, GlmFetchResult, GlmFlash, GlmSatellite, RecordDrops,
};
use squallar_overlays::gmgsi::decode::GmgsiGrid;
use squallar_overlays::gmgsi::{GmgsiChannel, GmgsiFrameFetch, GmgsiListing};
use squallar_overlays::hrrr::GridCoords;
use squallar_overlays::render::gridded::ResidentGrid;

const CHANNEL: GmgsiChannel = GmgsiChannel::LongwaveIr;

/// Thirteen hourly mosaics over twelve hours — what `Gmgsi::min_loop_frames`
/// buys against a Lookback slider whose own default is one hour, and the whole
/// reason the two numbers in this suite differ.
const FRAMES: i64 = 13;

/// `GlmPaneState::new`'s own default, which is what the pane below is drawing
/// with: a flash is on screen for five minutes after it happens.
const GLM_WINDOW_SECS: i64 = 300;

fn hour(k: i64) -> chrono::NaiveDateTime {
    chrono::NaiveDate::from_ymd_opt(2025, 6, 1)
        .unwrap()
        .and_hms_opt(0, 0, 0)
        .unwrap()
        + chrono::Duration::hours(k)
}

fn stamp(k: i64) -> FrameStamp {
    FrameStamp {
        valid: hour(k),
        run: None,
    }
}

/// The window the loop is armed over: `min_loop_span_secs`, to the second.
fn window() -> (chrono::NaiveDateTime, chrono::NaiveDateTime) {
    (hour(0), hour(FRAMES - 1))
}

fn bounds() -> GeoBounds {
    GeoBounds {
        min_lat: 34.0,
        max_lat: 36.0,
        min_lon: -99.0,
        max_lon: -97.0,
    }
}

/// Frame `k`'s own flash: its own instant, 100 s inside that frame's 300 s
/// window, and its own place — a latitude no other frame's probe box contains.
fn flash(k: i64) -> GlmFlash {
    GlmFlash {
        lat: 34.1 + 0.15 * k as f64,
        lon: -98.5,
        energy: Some(1.0e-14),
        area: None,
        time: hour(k) - chrono::Duration::seconds(100),
        satellite: GlmSatellite::GoesEast,
        level: GlmDataLevel::Flash,
    }
}

/// A magnifying window around frame `k`'s flash — the "did **this** frame's
/// strike reach the glass" probe. A box, not the viewport, so a lit picture is
/// that frame's own strike and never a neighbour's.
fn probe(k: i64) -> GeoBounds {
    let f = flash(k);
    GeoBounds {
        min_lat: f.lat - 0.05,
        max_lat: f.lat + 0.05,
        min_lon: f.lon - 0.05,
        max_lon: f.lon + 0.05,
    }
}

fn granule(k: i64) -> GmgsiGrid {
    let spec = squallar_overlays::gmgsi::fields::spec(CHANNEL);
    GmgsiGrid {
        channel: CHANNEL,
        grid: ResidentGrid {
            field: spec.id.clone(),
            ni: 4,
            nj: 1,
            coords: GridCoords::Separable {
                lat_axis: vec![35.0],
                lon_axis: (0..4).map(|i| -99.0 + f64::from(i) * 0.072).collect(),
            },
            values: vec![k as f32; 4],
        },
        bounds: bounds(),
        valid_time: hour(k),
    }
}

/// Every described job the funnel was handed, newest last.
#[derive(Default)]
struct Jobs {
    seen: Arc<Mutex<Vec<squallar_source::job::DescribedJob>>>,
}

impl squallar_worker::offload::JobSink for Jobs {
    fn send(
        &self,
        _id: u64,
        request: squallar_worker::offload::JobRequest,
    ) -> Result<(), squallar_worker::offload::JobRequest> {
        self.seen
            .lock()
            .expect("no poisoned lock")
            .push(request.job.clone());
        Ok(())
    }
}

fn plan() -> squallar_egui::overlay_cache::OverlayTexturePlan {
    squallar_egui::overlay_cache::OverlayTexturePlan {
        width: 64,
        height: 64,
        overdraw: 0.0,
        pixels_per_point: 1.0,
    }
}

/// Draw `id` on pane 0, or stop drawing it — the same four calls
/// `Gui::write_pane_overlay` makes, in the same order.
fn draw(app: &mut crate::app::App, id: &squallar_source::id::LayerId, on: bool) {
    let (panes, overlays) = app.gui.panes_and_overlays_mut();
    panes[0].hydrate_layer_states(overlays, 0);
    panes[0].set_layer_enabled(overlays, 0, id, on);
    panes[0].adopt_handler_state(overlays);
    panes[0].refresh_transport(overlays);
}

/// A one-pane app looping the satellite layer with lightning drawn over it —
/// the user's pane. Its HTTP client cannot reach a network.
fn satellite_loop_with_lightning() -> crate::app::App {
    let mut app = crate::app::tests::headless(crate::platform_double::TestBridge::desktop());
    app.http_client = crate::app::tests::unreachable_http_client();

    draw(&mut app, &known::GMGSI, true);
    draw(&mut app, &known::LIGHTNING, true);
    draw(&mut app, &known::RADAR, false);

    let pane = app.gui.pane_mut(0).expect("the fixture built a pane");
    pane.set_transport_layer(known::GMGSI);
    *pane.time_state_mut(&known::GMGSI) = squallar_egui::pane::LayerTimeState::begin(
        (window().1 - window().0).num_seconds() as u64,
        squallar_radar::types::RenderView::PlanView,
        Box::new(()),
    );
    pane.time_state_mut(&known::GMGSI).asked_range = Some(window());

    let keys: Vec<(chrono::NaiveDateTime, String)> = (0..FRAMES)
        .map(|k| {
            (
                hour(k),
                format!(
                    "GMGSI_LW/{}/GLOBCOMPLIR_v3r0_blend_s{}00000_e{}09599_c{}34579.nc",
                    hour(k).format("%Y/%m/%d/%H"),
                    hour(k).format("%Y%m%d%H"),
                    hour(k).format("%Y%m%d%H"),
                    hour(k).format("%Y%m%d%H"),
                ),
            )
        })
        .collect();
    app.channels
        .overlay_fetch_sender
        .send(SourceEvent::Frames {
            id: known::GMGSI,
            listing: FrameListing {
                range: window(),
                frames: keys
                    .iter()
                    .map(|(v, _)| FrameStamp {
                        valid: *v,
                        run: None,
                    })
                    .collect(),
                complete: true,
            },
            scope: Box::new(GmgsiListing {
                channel: CHANNEL,
                range: window(),
                keys,
                complete: true,
            }),
        })
        .expect("the receiver is alive");
    app.poll_overlay_fetch_results();
    app.accept_loop_scan_listings();
    app
}

/// Give every satellite frame a granule and a picture, so the transport is a
/// loop playback can actually walk.
fn fill_transport(app: &mut crate::app::App, ctx: &egui::Context) {
    for k in 0..FRAMES {
        app.channels
            .overlay_fetch_sender
            .send(SourceEvent::FrameReady {
                id: known::GMGSI,
                stamp: stamp(k),
                data: Box::new(GmgsiFrameFetch {
                    channel: CHANNEL,
                    valid: hour(k),
                    grid: Some(granule(k)),
                }),
            })
            .expect("the receiver is alive");
        app.poll_overlay_fetch_results();
        app.channels
            .overlay_render_sender
            .send(crate::channels::OverlayRenderResponse {
                ink: true,
                image: Some(Arc::new(egui::ColorImage::from_rgba_unmultiplied(
                    [1, 1],
                    &[10, 10, 10, 255],
                ))),
                geo_bounds: bounds(),
                overlay_kind: known::GMGSI,
                generation: 0,
                pane_indices: vec![0],
                zoom: 32,
                hit_map: None,
                frame: Some(stamp(k)),
            })
            .expect("the receiver lives on the App");
        app.poll_overlay_render_results(ctx);
    }
}

/// **The flashes the ask the app builds says this pane depicts.**
///
/// The model of the S3 round, and the only modelled step in this suite. It
/// reads `FetchConfig`'s two depicted fields at their documented meaning —
/// the named instants where there are any, the span around the sample where
/// there are not — and keeps the flashes inside one of those windows.
///
/// It is deliberately a function of what the **app** asked for and of nothing
/// else: the defect this suite exists for is entirely in that ask.
fn retained_by(
    config: &squallar_overlays::render::overlay_state::FetchConfig,
    all: &[GlmFlash],
) -> Vec<GlmFlash> {
    let window = chrono::Duration::seconds(GLM_WINDOW_SECS);
    let kept = |time: chrono::NaiveDateTime| {
        if config.depicted_frames.is_empty() {
            let span = chrono::Duration::seconds(config.depicted_span_secs.unwrap_or(0) as i64);
            return time >= config.as_of - window - span && time <= config.as_of + span;
        }
        std::iter::once(config.as_of)
            .chain(config.depicted_frames.iter().copied())
            .any(|at| time >= at - window && time <= at)
    };
    all.iter().filter(|f| kept(f.time)).cloned().collect()
}

/// Poll lightning the way the app does: build the ask through the production
/// `fetch_config_for_layer`, then deliver what that ask retains on the
/// production arrival path.
fn poll_lightning(app: &mut crate::app::App) -> usize {
    let all: Vec<GlmFlash> = (0..FRAMES).map(flash).collect();
    let config = crate::app::fetch::fetch_config_for_layer(
        &app.gui,
        0,
        &known::LIGHTNING,
        app.fetch_config(),
    );
    let flashes = retained_by(&config, &all);
    let delivered = flashes.len();
    app.channels
        .overlay_fetch_sender
        .send(SourceEvent::Data(
            squallar_source::handler::OverlayFetchResult {
                kind: known::LIGHTNING,
                data: Box::new(GlmFetchResult(Ok(GlmFetchOutcome {
                    flashes,
                    window_gaps: Vec::new(),
                    record_drops: RecordDrops {
                        considered: all.len(),
                        fill_values: 0,
                        off_globe: 0,
                    },
                    dead_feeds: Vec::new(),
                    queried: vec![GlmSatellite::GoesEast],
                    parse_failures: None,
                    transport_failures: None,
                    level_failures: Vec::new(),
                    evaluated_levels: vec![(GlmSatellite::GoesEast, GlmDataLevel::Flash)],
                    listing_failures: Vec::new(),
                }))),
            },
        ))
        .expect("the receiver is alive");
    app.poll_overlay_fetch_results();
    delivered
}

/// Which frames' strikes a described lightning raster drew, by probing the
/// production rasterizer at each frame's own box.
fn strikes_in(job: &squallar_source::job::DescribedJob) -> Option<Vec<i64>> {
    let input = job.downcast_ref::<squallar_overlays::render::rasterize::GlmStrikesInput>()?;
    Some(
        (0..FRAMES)
            .filter(|&k| {
                let out = squallar_overlays::render::rasterize::rasterize_glm_strikes(
                    input,
                    &probe(k),
                    64,
                    64,
                );
                out.rgba.iter().any(|&b| b != 0)
            })
            .collect(),
    )
}

/// One tick of the pane's overlay pass for lightning: notice the raster is
/// stale, dispatch it, run the job the funnel was handed, file the answer.
///
/// Every step is the production one — `overlay_cache_token` is the function
/// the draw loop itself calls, `needs_rerender` is the cache's own trigger,
/// `spawn_overlay_render` is the one dispatch door, and
/// `poll_overlay_render_results` is the arrival. What is not here is egui's
/// layout: the viewport and texture plan are handed in rather than measured
/// off a pane rect.
fn overlay_pass(
    app: &mut crate::app::App,
    ctx: &egui::Context,
    seen: &Arc<Mutex<Vec<squallar_source::job::DescribedJob>>>,
) -> Option<(u64, Vec<i64>)> {
    let token = {
        let pane = app.gui.pane(0).expect("pane 0");
        squallar_egui::overlay_cache_token(&app.gui.overlays, 0, pane, &known::LIGHTNING, false)
    };
    let stale = {
        let pane = app.gui.pane_mut(0).expect("pane 0");
        let cache = pane.overlay_cache_mut(&known::LIGHTNING);
        cache.needs_rerender(
            token,
            5.0,
            squallar_egui::overlay_cache::ZoomDrive::AT_REST,
            &bounds(),
            &plan(),
        ) && cache.renders.is_empty()
    };
    if !stale {
        return None;
    }
    seen.lock().expect("no poisoned lock").clear();
    app.spawn_overlay_render(
        vec![0],
        known::LIGHTNING,
        crate::app::fetch::OverlayRenderRequest {
            geo_bounds: bounds(),
            texture: plan(),
            data_generation: token,
            zoom: 160,
        },
        None,
    );
    let job = seen.lock().expect("no poisoned lock").last().cloned();
    let drew = strikes_in(job.as_ref()?)?;
    app.channels
        .overlay_render_sender
        .send(crate::channels::OverlayRenderResponse {
            ink: true,
            image: Some(Arc::new(egui::ColorImage::from_rgba_unmultiplied(
                [1, 1],
                &[255, 255, 255, 255],
            ))),
            geo_bounds: bounds(),
            overlay_kind: known::LIGHTNING,
            generation: token,
            pane_indices: vec![0],
            zoom: 160,
            hit_map: None,
            frame: None,
        })
        .expect("the receiver lives on the App");
    app.poll_overlay_render_results(ctx);
    app.deliver_held_rasters();
    Some((token, drew))
}

/// Play one whole loop at `fps` and report, per frame, which strikes the
/// picture on the glass drew. The pipeline is the synchronous one
/// [`overlay_pass`] drives — the regime the content-arm brake must leave
/// exactly alone, at every speed.
fn lit_at_speed(fps: f32) -> (Vec<i64>, Vec<(i64, Vec<i64>)>) {
    let ctx = egui::Context::default();
    let jobs = Jobs::default();
    let seen = Arc::clone(&jobs.seen);
    let _guard = squallar_worker::offload::install_test_worker(Box::new(jobs));

    let mut app = satellite_loop_with_lightning();
    fill_transport(&mut app, &ctx);
    {
        let pane = app.gui.pane_mut(0).expect("pane 0");
        pane.transport_state_mut().phase = squallar_egui::pane::LoopPhase::Playing;
        pane.time.speed_fps = fps;
        pane.set_time_mode(squallar_egui::pane::TimeMode::AsOf(hour(0)));
    }
    let _ = poll_lightning(&mut app);

    let mut lit = Vec::new();
    let mut drawn = Vec::new();
    let mut by_token: std::collections::HashMap<u64, Vec<i64>> = std::collections::HashMap::new();
    for k in 0..FRAMES {
        if let Some((token, drew)) = overlay_pass(&mut app, &ctx, &seen) {
            by_token.insert(token, drew);
        }
        let pane = app.gui.pane(0).expect("pane 0");
        let on_screen = pane
            .overlay_texture_on_screen(&known::LIGHTNING)
            .map(|tex| tex.data_generation)
            .and_then(|token| by_token.get(&token).cloned())
            .unwrap_or_default();
        if on_screen.contains(&k) {
            lit.push(k);
        }
        drawn.push((k, on_screen));
        app.gui
            .pane_mut(0)
            .expect("pane 0")
            .transport_state_mut()
            .last_advance = None;
        app.advance_loop_playback();
    }
    (lit, drawn)
}

/// **The per-frame contract at all three speeds the Speed slider offers.**
///
/// The slider is 1.0–30.0 fps (`ui_timeline.rs`), and those ends are where a
/// brake keyed on delivery could behave differently: at 1 fps a tick is a
/// second and any pipeline lands inside it, at 30 fps a tick is 33 ms and a
/// slow one cannot. The content arm's brake is keyed on *delivery* and never
/// on rate precisely so that the answer is the same at all three, and this is
/// the fixture that says so.
///
/// **Non-vacuity is the `return false` tamper**: making the brake
/// unconditional reds this at every speed, which is the direction the previous
/// attempt on this card failed in. The `return true` tamper leaves it green,
/// and correctly — a synchronous pipeline is never braked either way, and
/// claiming otherwise would be this fixture measuring itself.
#[test]
fn the_per_frame_contract_holds_at_every_speed_the_slider_offers() {
    // This test supersedes repeatedly, so it moves the process-global
    // `cancelled` counter `overlay_cancel_tests` asserts a delta on -- 35 and
    // 12 withdrawals measured across the two cases here. See
    // `overlay_ledger_lock`.
    let _ledger = crate::app::fetch::overlay_ledger_lock();
    for fps in [1.0f32, 10.0, 30.0] {
        let (lit, drawn) = lit_at_speed(fps);
        assert_eq!(
            lit.len() as i64,
            FRAMES,
            "at {fps} fps only {} of {FRAMES} frames drew their own strikes. \
             The brake is keyed on delivery, so a pipeline that answers inside \
             a tick must be untouched at every speed. Lit {lit:?}; each \
             frame's picture drew {drawn:?}",
            lit.len(),
        );
        // Floor A, re-asserted per speed: widening what is HELD may never
        // widen what a frame DRAWS.
        for (k, drew) in &drawn {
            assert!(
                !(*k == FRAMES - 1 && drew.contains(&0)),
                "at {fps} fps the newest frame's picture drew the oldest \
                 frame's strike, twelve hours outside its {GLM_WINDOW_SECS}s \
                 window: {drew:?}",
            );
        }
    }
}

/// **Premise census (temporary instrument).** How many rasters does a playing
/// loop really spend on a `TimeAxis::EventLifetime` layer when the pipeline
/// answers after `latency` ticks rather than instantly? Runs BOTH halves of
/// the production dispatch predicate — `needs_rerender` and
/// `RendersInFlight::admits` — which is the pair `ui_map_pane` evaluates.
fn treadmill_census(latency: usize, cycles: i64) -> String {
    let ctx = egui::Context::default();
    let jobs = Jobs::default();
    let _seen = Arc::clone(&jobs.seen);
    let _guard = squallar_worker::offload::install_test_worker(Box::new(jobs));

    let mut app = satellite_loop_with_lightning();
    fill_transport(&mut app, &ctx);
    {
        let pane = app.gui.pane_mut(0).expect("pane 0");
        pane.transport_state_mut().phase = squallar_egui::pane::LoopPhase::Playing;
        pane.set_time_mode(squallar_egui::pane::TimeMode::AsOf(hour(0)));
    }
    let _ = poll_lightning(&mut app);
    squallar_egui::overlay_cache::ledger::reset_for_test();

    let limit = squallar_device_profile::constants::MAX_CONCURRENT_RENDERS;
    let ticks = (FRAMES * cycles) as usize;
    let mut dispatches = 0usize;
    let mut tokens_dispatched: std::collections::HashSet<u64> = Default::default();
    let mut tokens_seen: std::collections::HashSet<u64> = Default::default();
    let mut inflight: Vec<(usize, u64)> = Vec::new();

    let land = |app: &mut crate::app::App, token: u64| {
        app.channels
            .overlay_render_sender
            .send(crate::channels::OverlayRenderResponse {
                ink: true,
                image: Some(Arc::new(egui::ColorImage::from_rgba_unmultiplied(
                    [1, 1],
                    &[255, 255, 255, 255],
                ))),
                geo_bounds: bounds(),
                overlay_kind: known::LIGHTNING,
                generation: token,
                pane_indices: vec![0],
                zoom: 160,
                hit_map: None,
                frame: None,
            })
            .expect("the receiver lives on the App");
        app.poll_overlay_render_results(&ctx);
        app.deliver_held_rasters();
    };

    for t in 0..ticks {
        let due: Vec<u64> = inflight
            .iter()
            .filter(|(at, _)| *at <= t)
            .map(|(_, tok)| *tok)
            .collect();
        inflight.retain(|(at, _)| *at > t);
        for token in due {
            land(&mut app, token);
        }

        let token = {
            let pane = app.gui.pane(0).expect("pane 0");
            squallar_egui::overlay_cache_token(&app.gui.overlays, 0, pane, &known::LIGHTNING, false)
        };
        tokens_seen.insert(token);

        let go = {
            let pane = app.gui.pane_mut(0).expect("pane 0");
            let cache = pane.overlay_cache_mut(&known::LIGHTNING);
            let stale = cache.needs_rerender(
                token,
                5.0,
                squallar_egui::overlay_cache::ZoomDrive::AT_REST,
                &bounds(),
                &plan(),
            );
            stale
                && cache
                    .renders
                    .admits(squallar_egui::overlay_cache::RenderSlot::WHOLE, limit)
        };
        if go {
            dispatches += 1;
            tokens_dispatched.insert(token);
            app.spawn_overlay_render(
                vec![0],
                known::LIGHTNING,
                crate::app::fetch::OverlayRenderRequest {
                    geo_bounds: bounds(),
                    texture: plan(),
                    data_generation: token,
                    zoom: 160,
                },
                None,
            );
            if latency == 0 {
                land(&mut app, token);
            } else {
                inflight.push((t + latency, token));
            }
        }

        app.gui
            .pane_mut(0)
            .expect("pane 0")
            .transport_state_mut()
            .last_advance = None;
        app.advance_loop_playback();
    }
    let led = squallar_egui::overlay_cache::ledger::totals();
    format!(
        "latency={latency} ticks={ticks}: dispatches={dispatches} distinct_dispatched={} \
         distinct_seen={} frames={FRAMES} | ledger dispatched={} arrived={} pictures={} \
         MB={:.1} on_screen={} superseded={} dropped={} cancelled={}",
        tokens_dispatched.len(),
        tokens_seen.len(),
        led.dispatched,
        led.arrived,
        led.pictures,
        led.picture_bytes as f64 / (1024.0 * 1024.0),
        led.on_screen(),
        led.superseded,
        led.dropped,
        led.cancelled,
    )
}

/// **Pricing probe (temporary).** What residency would really cost, from the
/// tree's own constants and its own frame-byte model — not scaled arithmetic.
#[test]
#[ignore = "instrument, not a gate: prints its census by panicking"]
fn residency_pricing_probe() {
    use squallar_device_profile::constants as c;
    let frame_bytes = crate::loop_pool::nominal_overlay_frame_bytes();
    let mut rows = Vec::new();
    for (name, floor, ceiling) in [
        (
            "wasm",
            c::WASM_LOOP_POOL_FLOOR_BYTES,
            c::WASM_LOOP_POOL_CEILING_BYTES,
        ),
        (
            "mobile",
            c::MOBILE_LOOP_POOL_FLOOR_BYTES,
            c::MOBILE_LOOP_POOL_CEILING_BYTES,
        ),
        (
            "desktop",
            c::DESKTOP_LOOP_POOL_FLOOR_BYTES,
            c::DESKTOP_LOOP_POOL_CEILING_BYTES,
        ),
    ] {
        // One pane, radar + one EventLifetime layer animating: `layer_share`
        // divides the pane's whole share by the animating count, in bytes.
        for (which, pool) in [("floor", floor), ("ceiling", ceiling)] {
            for animating in [2usize, 3] {
                let frames = ((pool / animating) / frame_bytes).max(c::MIN_LOOP_FRAMES_PER_PANE);
                rows.push(format!(
                    "{name:8} {which:8} animating={animating} pool={:5} MiB -> {frames:3} frames \
                     resident of a 13-frame loop",
                    pool / (1024 * 1024),
                ));
            }
        }
    }
    panic!(
        "PRICING (frame_bytes={frame_bytes} = {:.2} MB)\n{}",
        frame_bytes as f64 / (1024.0 * 1024.0),
        rows.join("\n"),
    );
}

/// **Disqualifier probe (temporary).** What does a loop frame with no image
/// draw? If `overlay_texture_on_screen` answers `None`, residency that cannot
/// afford every frame reproduces the reported defect on the frames it missed.
#[test]
#[ignore = "instrument, not a gate: prints its census by panicking"]
fn an_imageless_loop_frame_draws_nothing() {
    let ctx = egui::Context::default();
    let jobs = Jobs::default();
    let _guard = squallar_worker::offload::install_test_worker(Box::new(jobs));
    let mut app = satellite_loop_with_lightning();
    fill_transport(&mut app, &ctx);

    let pane = app.gui.pane_mut(0).expect("pane 0");
    // The satellite transport is active and every frame HAS an image.
    let with_image = pane.overlay_texture_on_screen(&known::GMGSI).is_some();
    // Now blank the frame under the playhead, leaving the timeline active.
    let ls = pane.time_state_mut(&known::GMGSI);
    for f in ls.frames.iter_mut() {
        f.image = None;
    }
    let without_image = pane.overlay_texture_on_screen(&known::GMGSI).is_some();
    panic!(
        "FALLBACK PROBE: active timeline with images -> on_screen={with_image}; \
         same timeline with every image cleared -> on_screen={without_image}",
    );
}

#[test]
#[ignore = "instrument, not a gate: prints its census by panicking"]
fn treadmill_premise_census() {
    let mut out = Vec::new();
    for latency in [0usize, 1, 2, 3, 6] {
        out.push(treadmill_census(latency, 3));
    }
    panic!("CENSUS\n{}", out.join("\n"));
}

/// **The acceptance.** Play the loop once and read, for every frame, what
/// `overlay_texture_on_screen` hands the painter and which strikes that
/// picture drew.
///
/// A frame counts as lit only when the picture on the glass drew **that
/// frame's own** strike. A picture is not enough and a strike is not enough:
/// the defect is a frame drawing nothing while its neighbour draws, and a
/// picture left over from another instant is the failure this fork was built
/// to remove.
///
/// **Floor A — the widening must not become "everything, always".** The last
/// frame's raster must NOT draw the first frame's strike: it is twelve hours
/// outside its 300 s window. Widening what is HELD may never widen what a
/// frame DRAWS.
///
/// **Floor B — `frames_for_nobody`: make `depicted_frames_for_layer` return
/// `Vec::new()`**, which is exactly what the tree did before this land. The
/// ask falls back to the Lookback slider's 3600 s against a 43200 s loop and
/// the count assertion reads **1 of 13**.
#[test]
fn every_frame_of_a_satellite_loop_draws_its_own_lightning() {
    // This test supersedes repeatedly, so it moves the process-global
    // `cancelled` counter `overlay_cancel_tests` asserts a delta on -- 35 and
    // 12 withdrawals measured across the two cases here. See
    // `overlay_ledger_lock`.
    let _ledger = crate::app::fetch::overlay_ledger_lock();
    let ctx = egui::Context::default();
    let jobs = Jobs::default();
    let seen = Arc::clone(&jobs.seen);
    let _guard = squallar_worker::offload::install_test_worker(Box::new(jobs));

    let mut app = satellite_loop_with_lightning();
    fill_transport(&mut app, &ctx);

    // Premise: the loop really is wider than the slider that named it, or the
    // two numbers this suite is about are the same number and it proves
    // nothing.
    let (span_setting, listed) = {
        let pane = app.gui.pane(0).expect("pane 0");
        (pane.time.span_secs, pane.transport_state().span_secs)
    };
    assert!(
        listed > span_setting,
        "premise: a satellite loop is listed over its own floor ({listed}s) \
         while the Lookback slider still reads {span_setting}s. Equal numbers \
         mean this fixture is a radar-shaped loop and the defect cannot show.",
    );

    // Start the loop where playback starts it, then let the poll land — the
    // production order, and the one that made the bug reachable: the pane is
    // already on a loop clock when lightning is asked for.
    {
        let pane = app.gui.pane_mut(0).expect("pane 0");
        pane.transport_state_mut().phase = squallar_egui::pane::LoopPhase::Playing;
        pane.set_time_mode(squallar_egui::pane::TimeMode::AsOf(hour(0)));
    }
    let delivered = poll_lightning(&mut app);

    let mut lit: Vec<i64> = Vec::new();
    let mut drawn: Vec<(i64, Vec<i64>)> = Vec::new();
    let mut by_token: std::collections::HashMap<u64, Vec<i64>> = std::collections::HashMap::new();

    for k in 0..FRAMES {
        if let Some((token, drew)) = overlay_pass(&mut app, &ctx, &seen) {
            by_token.insert(token, drew);
        }
        let pane = app.gui.pane(0).expect("pane 0");
        let on_screen = pane
            .overlay_texture_on_screen(&known::LIGHTNING)
            .map(|tex| tex.data_generation)
            .and_then(|token| by_token.get(&token).cloned())
            .unwrap_or_default();
        if on_screen.contains(&k) {
            lit.push(k);
        }
        drawn.push((k, on_screen));

        // One frame on, the way playback moves: results applied, then advance.
        app.gui
            .pane_mut(0)
            .expect("pane 0")
            .transport_state_mut()
            .last_advance = None;
        app.advance_loop_playback();
    }

    assert_eq!(
        lit.len() as i64,
        FRAMES,
        "still broken! GLM _STILL_ only works on the first frame of a loop — \
         just a single frame at the beginning (or end, hard to tell) has \
         them, the rest are blank: {} of {FRAMES} frames drew strikes. Lit \
         frames were {lit:?} of 0..{FRAMES}; what each frame's picture drew \
         was {drawn:?}, and the poll delivered {delivered} of {FRAMES} \
         flashes. The loop is listed over {listed}s and the pane's Lookback \
         setting is {span_setting}s.",
        lit.len(),
    );

    // Floor A: what is HELD widened; what a frame DRAWS must not.
    let newest = by_token
        .values()
        .find(|drew| drew.contains(&(FRAMES - 1)))
        .expect("the newest frame's raster was described");
    assert!(
        !newest.contains(&0),
        "the newest frame's picture drew the oldest frame's strike, twelve \
         hours outside its {GLM_WINDOW_SECS}s window: {newest:?}. Widening \
         what is HELD must never widen what a frame DRAWS.",
    );
    assert!(
        delivered < (FRAMES * 4) as usize,
        "non-triviality: the poll must retain the loop's windows, not every \
         flash there has ever been — {delivered} rows",
    );
}
