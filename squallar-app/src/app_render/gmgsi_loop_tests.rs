//! **WB-11: the satellite layer loops** — the user's originating request, end
//! to end, against the **real** `GmgsiHandler`.
//!
//! Not a double. `loop_overlay_render_tests` had to stand one in the model
//! layer's place because every frame payload in this tree was private, and a
//! double cannot catch a layer that files its frames wrongly — it agrees with
//! whatever the app assumes. `GmgsiListing` and `GmgsiFrameFetch` are public
//! for exactly this, so what runs below is the shipping registry: the real
//! `apply_frame_listing`, the real `list_frames`, the real `fetch_frame`, the
//! real frame-addressed `prepare_job`.
//!
//! **What is arranged and what is driven.** The listing and the granules are
//! handed in on the two production arrival paths (`SourceEvent::Frames` and
//! `SourceEvent::FrameReady`), because the bytes behind them are 7.5 MB
//! objects in an S3 bucket and no test in this tree reaches a network. The
//! HTTP client is replaced with one that cannot connect, so the frame fetches
//! the real dispatch puts on the wire fail immediately and deliver nothing;
//! that the fetch half asks for the right key, declines an unlisted hour and
//! declines a staged one is pinned in `squallar-overlays` by
//! `a_frame_fetch_is_declined_where_there_is_no_key_and_where_the_granule_is_held`.
//! Everything between the listing and the picture is the app's own code.
//!
//! **The arrival order is the design, not a convenience.** GMGSI serialises
//! its frame fetches (one 60 MB decode at a time — thirteen at once is 780 MB)
//! and stages one granule, so the production sequence really is arrive, draw,
//! arrive, draw. The loop below is that sequence.

use squallar_geo::GeoBounds;
use squallar_source::handler::SourceEvent;
use squallar_source::id::known;
use squallar_source::job::DescribedJob;
use squallar_source::time::{FrameListing, FrameStamp};
use std::sync::{Arc, Mutex};

use squallar_overlays::gmgsi::decode::GmgsiGrid;
use squallar_overlays::gmgsi::{GmgsiChannel, GmgsiFrameFetch, GmgsiListing};
use squallar_overlays::hrrr::GridCoords;
use squallar_overlays::render::gridded::ResidentGrid;

/// The channel the layer opens on, and the one the pane below is drawing.
const CHANNEL: GmgsiChannel = GmgsiChannel::LongwaveIr;

/// **Thirteen hourly mosaics over twelve hours**, which is what
/// `Gmgsi::min_loop_frames` buys against a Lookback slider whose own default
/// is one hour.
const FRAMES: i64 = 13;

/// Midnight of the day the committed granule was taken from.
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

/// A granule for `hour(k)` whose **values name the hour**. Small: the real
/// mosaic is 3000x5000 and what is under test is which grid reaches which
/// frame, not how big it is.
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

/// A sink that takes every job and keeps what it was **described with**, so
/// two frames' rasters can be told apart without rasterizing either.
///
/// `DescribedJob`'s equality is the input's own, and GMGSI's input is a
/// `GriddedInput::Resident` holding the granule — so two jobs are equal
/// exactly when they carry the same picture. That is the assertion a texture
/// handle cannot make: the textures below are delivered by hand and would
/// differ however the jobs were described.
#[derive(Default)]
struct DescribedJobs {
    jobs: Arc<Mutex<Vec<DescribedJob>>>,
}

impl squallar_worker::offload::JobSink for DescribedJobs {
    fn send(
        &self,
        _id: u64,
        request: squallar_worker::offload::JobRequest,
    ) -> Result<(), squallar_worker::offload::JobRequest> {
        self.jobs
            .lock()
            .expect("no poisoned lock")
            .push(request.job.clone());
        Ok(())
    }
}

fn a_render_request() -> crate::app::fetch::OverlayRenderRequest {
    crate::app::fetch::OverlayRenderRequest {
        geo_bounds: bounds(),
        texture: squallar_egui::overlay_cache::OverlayTexturePlan {
            width: 8,
            height: 5,
            overdraw: 0.0,
            pixels_per_point: 1.0,
        },
        data_generation: 0,
        zoom: 32,
    }
}

/// A one-pane app drawing GMGSI and nothing else time-shaped, with its HTTP
/// client stubbed so no dispatch below can reach a network.
fn satellite_app() -> crate::app::App {
    let mut app = crate::app::tests::headless(crate::platform_double::TestBridge::desktop());
    // Unroutable and instant: `127.0.0.1:1` refuses, and the timeout is the
    // backstop for a machine that answers on it.
    app.http_client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_millis(1))
        .connect_timeout(std::time::Duration::from_millis(1))
        .build()
        .expect("a client with no connection to make");

    draw(&mut app, &known::GMGSI, true);
    draw(&mut app, &known::RADAR, false);
    app
}

/// Draw `id` on pane 0, or stop drawing it — the same four calls
/// `Gui::write_pane_overlay` makes, in the same order, so the pane below is in
/// a state the real door can produce. `refresh_transport` is the last of them
/// and is what moves a pane's transport when its enabled set changes.
fn draw(app: &mut crate::app::App, id: &squallar_source::id::LayerId, on: bool) {
    let (panes, overlays) = app.gui.panes_and_overlays_mut();
    panes[0].hydrate_layer_states(overlays, 0);
    panes[0].set_layer_enabled(overlays, 0, id, on);
    panes[0].adopt_handler_state(overlays);
    panes[0].refresh_transport(overlays);
}

/// Arm pane 0's satellite loop over [`window`] and answer its listing on the
/// production arrival path, with the keys a real listing would have found.
fn build_loop(app: &mut crate::app::App) {
    let pane = app.gui.pane_mut(0).expect("the fixture built a pane");
    pane.set_transport_layer(known::GMGSI);
    *pane.time_state_mut(&known::GMGSI) = squallar_egui::pane::LayerTimeState::begin(
        (window().1 - window().0).num_seconds() as u64,
        squallar_radar::types::RenderView::PlanView,
        Box::new(()),
    );
    // What the production dispatch records beside the phase: the window the
    // ask covered, which is what the arrival is matched on.
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
                frames: keys.iter().map(|(v, _)| stamp_at(*v)).collect(),
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
}

fn stamp_at(valid: chrono::NaiveDateTime) -> FrameStamp {
    FrameStamp { valid, run: None }
}

/// Deliver one frame's granule on the production arrival path.
fn deliver_granule(app: &mut crate::app::App, k: i64) {
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
}

/// Deliver one finished raster for `stamp`, with a shade nothing else shares.
fn deliver_raster(app: &mut crate::app::App, ctx: &egui::Context, k: i64) {
    let shade = 10 + (k as u8) * 15;
    app.channels
        .overlay_render_sender
        .send(crate::channels::OverlayRenderResponse {
            image: Some(Arc::new(egui::ColorImage::from_rgba_unmultiplied(
                [1, 1],
                &[shade, shade, shade, 255],
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

fn frame_stamps(app: &crate::app::App) -> Vec<chrono::NaiveDateTime> {
    app.gui
        .pane(0)
        .expect("pane 0")
        .time_state(&known::GMGSI)
        .frames
        .iter()
        .map(|f| f.timestamp)
        .collect()
}

/// The texture id on each frame, `None` where a frame has no picture.
fn frame_textures(app: &crate::app::App) -> Vec<Option<egui::TextureId>> {
    app.gui
        .pane(0)
        .expect("pane 0")
        .time_state(&known::GMGSI)
        .frames
        .iter()
        .map(|f| {
            f.image
                .as_ref()
                .and_then(squallar_egui::pane::LoopFrameImage::overlay)
                .map(|o| o.texture.id())
        })
        .collect()
}

/// **The acceptance of WB-11.** A satellite pane with a loop enabled ends with
/// thirteen frames over twelve hours, each carrying its own picture.
///
/// Five things, in the order the app does them:
///
/// 1. the layer's own listing became a **thirteen**-frame list spanning twelve
///    hours — the `min_loop_frames` floor, without which an hourly source
///    yields two frames;
/// 2. one granule is staged at a time, however many frames the loop holds;
/// 3. every frame's raster is described from **that frame's** granule — the
///    thirteen jobs are pairwise different;
/// 4. each result files itself to the frame that asked;
/// 5. thirteen frames hold thirteen **different** textures.
///
/// Claim 3 is the one nothing in this build could previously make. WI-6b named
/// it as an uncaught generic gap: a `FrameSeries` layer that ignores
/// `ctx.frame` hands every frame the same picture, and no assertion over
/// texture *presence* can see it. The jobs are compared rather than the
/// textures because the textures here are delivered by hand and would differ
/// whatever the layer described.
///
/// **Floor A — `ignore_the_frame`:** drop the `ctx.frame` arm of GMGSI's
/// `prepare_job` so it reads the live cache. Observed red: it described
/// nothing at all, and claim 3 failed with 0 jobs against 13.
///
/// **Floor B — `forget_the_keys`:** return from `apply_frame_listing` before
/// it files anything. Observed red: claim 1 failed with an empty frame list —
/// the listing arrives, is dropped, and `accept_loop_scan_listings` retires
/// the loop. This is the seam a layer-only suite cannot see.
///
/// **What this test does NOT floor, stated plainly.** The window is armed by
/// hand at 43,200 s, so `Gmgsi::min_loop_frames` -> 0 leaves it **green**. The
/// span floor is derived in `Gui::loop_span_secs_for` and reaches the app only
/// through a `GuiAction::EnableLoop`; it is floored where it is derived, by
/// `the_satellite_layer_loops_over_twelve_hours_and_radar_over_the_sliders` in
/// `squallar-egui`, which reads the window off the action the real infinity
/// button emitted. Residency is likewise not floored here — see the comment on
/// the arrival loop for the denominator that makes it impossible.
#[test]
fn a_satellite_pane_loops_thirteen_hours_each_with_its_own_picture_end_to_end() {
    let ctx = egui::Context::default();
    let jobs: Arc<Mutex<Vec<DescribedJob>>> = Default::default();
    let _guard = squallar_worker::offload::install_test_worker(Box::new(DescribedJobs {
        jobs: Arc::clone(&jobs),
    }));

    let mut app = satellite_app();
    assert_eq!(
        app.gui.pane(0).expect("pane 0").transport_layer(),
        &known::GMGSI,
        "premise: GMGSI is a FrameSeries layer, so a pane drawing it and no \
         radar addresses it as its transport. Before WB-11 it was `Live` and \
         this pane had no transport at all",
    );

    build_loop(&mut app);

    // 1. Thirteen frames over twelve hours.
    assert_eq!(
        frame_stamps(&app),
        (0..FRAMES).map(hour).collect::<Vec<_>>(),
        "the layer's listing must have become the loop's frame list",
    );
    assert_eq!(
        (window().1 - window().0).num_seconds(),
        43_200,
        "twelve hours: thirteen hourly mosaics is twelve steps end to end",
    );

    // The geometry record a loop frame's raster reuses is written by the
    // pane's own live dispatch, exactly as it is in production.
    app.spawn_overlay_render(vec![0], known::GMGSI, a_render_request(), None);
    assert!(
        jobs.lock().expect("no poisoned lock").is_empty(),
        "control: with no live granule the live dispatch describes nothing, so \
         every job counted below is a loop frame's and none is the pane's own \
         picture",
    );

    // 2, 3, 4. Arrive, draw, arrive, draw — the order the serialised fetch and
    // the one-granule staging area impose in production.
    //
    // **Residency is NOT asserted here, and the denominator is why.** The
    // shipped staging budget is one 60 MB mosaic; these granules are four
    // values, sixteen bytes, so nothing this test can afford to allocate would
    // ever overflow it and an eviction assertion over them could not fail.
    // That claim is pinned where the budget can be injected —
    // `the_layer_stages_one_granule_however_many_frames_the_loop_holds` in
    // `squallar-overlays`, at a one-grid budget with thirteen frames listed.
    // What this loop pins instead is that the arrival, the dispatch and the
    // filing work frame by frame in that order.
    for k in 0..FRAMES {
        deliver_granule(&mut app, k);
        app.dispatch_overlay_loop_renders();
        assert_eq!(
            frame_textures(&app).iter().filter(|t| t.is_some()).count(),
            k as usize,
            "before this frame's raster is delivered, exactly the {k} earlier \
             frames are holding pictures",
        );
        deliver_raster(&mut app, &ctx, k);
    }

    // 3. Thirteen jobs, pairwise different.
    let described = jobs.lock().expect("no poisoned lock").clone();
    assert_eq!(
        described.len(),
        FRAMES as usize,
        "one raster per frame; {} were described",
        described.len(),
    );
    for (i, a) in described.iter().enumerate() {
        for (j, b) in described.iter().enumerate().skip(i + 1) {
            assert!(
                a != b,
                "frames {i} and {j} were described with the SAME picture. \
                 Every frame of the loop would be one hour's satellite image \
                 under thirteen captions, and nothing else in this build \
                 detects it",
            );
        }
    }

    // 5. Thirteen frames, thirteen textures.
    let textures = frame_textures(&app);
    let ids: Vec<egui::TextureId> = textures
        .iter()
        .map(|t| t.expect("every frame must be holding a picture"))
        .collect();
    let mut unique = ids.clone();
    unique.sort_by_key(|id| format!("{id:?}"));
    unique.dedup();
    assert_eq!(
        unique.len(),
        FRAMES as usize,
        "two frames are holding the same texture handle: {ids:?}",
    );
}

/// **Non-triviality: radar and the model are untouched.**
///
/// GMGSI's weight is 5, the lowest any layer claims, so it cannot take a pane's
/// clock from either of them — which is the whole of the WB-11 draw-order
/// ruling. A pane drawing radar loops radar, over the slider's own window,
/// exactly as it did before this item.
#[test]
fn a_radar_pane_and_a_model_pane_loop_exactly_as_they_did() {
    let mut app = crate::app::tests::headless(crate::platform_double::TestBridge::desktop());

    // Radar out of the box, with the satellite layer drawn on top of it.
    draw(&mut app, &known::GMGSI, true);
    assert_eq!(
        app.gui.pane(0).expect("pane 0").transport_layer(),
        &known::RADAR,
        "a pane drawing radar and satellite still walks radar's clock",
    );

    // The model on top of satellite, radar off.
    draw(&mut app, &known::MODEL_DATA, true);
    draw(&mut app, &known::RADAR, false);
    assert_eq!(
        app.gui.pane(0).expect("pane 0").transport_layer(),
        &known::MODEL_DATA,
        "a pane drawing the model and satellite still walks the model's clock",
    );
}

/// **Every frame fetch the app has put on the wire, in the order it answered**
/// — the ask, not the answer, and not the dispatcher's own in-flight set,
/// which is the thing under test and would agree with itself.
///
/// Drained until the wire stays quiet. Nothing is put back: the caller decides
/// whether the app takes delivery, and *that* is what clears the in-flight
/// mark. A floor that needs the mark to stay set simply never calls
/// [`deliver_fetch_answers`].
fn fetch_asks(app: &crate::app::App) -> Vec<chrono::NaiveDateTime> {
    let mut seen: Vec<chrono::NaiveDateTime> = Vec::new();
    let mut quiet = 0;
    while quiet < 20 {
        let mut got = false;
        while let Ok(event) = app.channels.overlay_fetch_receiver.try_recv() {
            if let SourceEvent::FrameReady { id, stamp, .. } = &event
                && *id == known::GMGSI
            {
                seen.push(stamp.valid);
                got = true;
            }
        }
        quiet = if got { quiet } else { quiet + 1 };
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    seen
}

/// Let the app take delivery of `stamps`' fetches on the production arrival
/// path, carrying no granule — which is what a failed fetch delivers, and what
/// clears the dispatcher's in-flight mark.
fn deliver_fetch_answers(app: &mut crate::app::App, stamps: &[chrono::NaiveDateTime]) {
    for valid in stamps {
        app.channels
            .overlay_fetch_sender
            .send(SourceEvent::FrameReady {
                id: known::GMGSI,
                stamp: stamp_at(*valid),
                data: Box::new(GmgsiFrameFetch {
                    channel: CHANNEL,
                    valid: *valid,
                    grid: None,
                }),
            })
            .expect("the receiver is alive");
    }
    app.poll_overlay_fetch_results();
}

/// [`fetch_asks`] deduplicated and sorted — the SET of instants asked for,
/// where how often each repeats is not the question.
fn distinct_asks(app: &crate::app::App) -> Vec<chrono::NaiveDateTime> {
    let mut seen = fetch_asks(app);
    seen.sort_unstable();
    seen.dedup();
    seen
}

/// **The re-ask is once per granule, not once per frame of the pump.**
///
/// The other half of the guard on `refetch_owed_loop_frames`. That pass runs
/// in every `Dispatch` phase — sixty times a second on a live window — and the
/// frames it walks stay owed for as long as their granules are travelling. Ask
/// on every pass and a thirteen-frame loop is thirteen 7.4 MB GETs *per frame*
/// against `noaa-gmgsi-pds`, for the whole time it is loading normally. The
/// in-flight mark is what makes it one.
///
/// **The fetch is made not to answer**, deterministically and with no clock in
/// it: this drives `Dispatch` alone and never `Ingest`, so nothing takes
/// delivery and the mark stays set exactly as it does while a granule is on
/// the wire. `PUMPS` passes therefore ask `PUMPS` times or once, and nothing
/// in between.
///
/// **Non-triviality is the second half**: once the answers are delivered and
/// the marks clear, the frames are still owed and the very next pass asks
/// again. A guard that simply never asked would pass the count assertion and
/// fail this one — which is the failure `a_satellite_loop_asks_again...`
/// floors from the opposite side.
///
/// **Floor — `no_in_flight_guard`:** drop the `loop_frame_fetch_in_flight`
/// half of the skip in `refetch_owed_loop_frames`. Observed red: 130 asks
/// across 10 passes where 13 are allowed — one storm per pump frame.
#[test]
fn a_frame_owed_its_granule_is_asked_for_once_however_many_passes_run() {
    let ctx = egui::Context::default();
    let _guard = squallar_worker::offload::install_test_worker(Box::new(DescribedJobs::default()));
    let mut app = satellite_app();
    build_loop(&mut app);

    // The listing's own burst, taken delivery of so every mark it set is
    // cleared and the wire is quiet before the pass under test runs.
    let burst = distinct_asks(&app);
    deliver_fetch_answers(&mut app, &burst);
    assert_eq!(
        burst.len(),
        FRAMES as usize,
        "premise: the listing put one fetch per frame on the wire",
    );

    // The pane's live raster, which is what lets the loop ask at all.
    app.spawn_overlay_render(vec![0], known::GMGSI, a_render_request(), None);

    /// Pump passes to run while every granule is still travelling. Ten, so an
    /// unguarded re-ask is an order of magnitude over the ceiling rather than
    /// a near miss.
    const PUMPS: usize = 10;
    for _ in 0..PUMPS {
        // `Dispatch` alone. Running `Ingest` here would take delivery of the
        // answers and clear the marks, which is the state the second half of
        // this test sets up on purpose.
        app.run_frame_pump(crate::app::frame_pump::PumpPhase::Dispatch, Some(&ctx));
    }

    let asks = fetch_asks(&app);
    assert_eq!(
        asks.len(),
        FRAMES as usize,
        "{PUMPS} pump passes over {FRAMES} frames whose granules are still on \
         the wire asked for {} of them. A frame owed its data is owed it once, \
         however many frames of the pump walk past it; asking again per pass \
         is {FRAMES} GETs of 7.4 MB every frame, for the whole time a loop is \
         loading normally",
        asks.len(),
    );
    let mut once_each = asks.clone();
    once_each.sort_unstable();
    once_each.dedup();
    assert_eq!(
        once_each,
        (0..FRAMES).map(hour).collect::<Vec<_>>(),
        "and the one ask each must be the whole frame list, not one instant \
         asked for {FRAMES} times",
    );

    // ── Non-triviality: the mark is a latch, not a stop ──────────────────
    // Deliver the answers — every one carrying no granule, so the frames are
    // owed exactly as much as before — and the next pass must ask again. A
    // guard that had simply switched the re-ask off would reach here green.
    deliver_fetch_answers(&mut app, &once_each);
    app.run_frame_pump(crate::app::frame_pump::PumpPhase::Dispatch, Some(&ctx));
    assert_eq!(
        distinct_asks(&app),
        (0..FRAMES).map(hour).collect::<Vec<_>>(),
        "once a fetch has answered, its frame is owed its granule again and \
         the next pass must ask; a mark that never cleared would make one \
         failed fetch terminal for that frame",
    );
}

/// **A loop whose granules were asked for before the pane could draw them is
/// not a loop that stays blank.** The regression for the reported "GMGSI
/// doesn't loop".
///
/// A loop frame's raster is sized by the record of the pane's own *live*
/// raster of that layer (`RenderDispatcher::overlay_record`), and a satellite
/// pane has none until its live granule has landed and been rasterized. The
/// frame fetches, though, all went out the instant the listing landed — so on
/// a pane without that record every granule was fetched, staged into a
/// one-granule staging area, evicted by the next arrival and never described
/// into a job. Nothing re-asked for any of them: `fetch_frame` was called once
/// per stamp, from the listing arrival, and never again. The loop sat in
/// `Rendering` for ever with no picture, no error and nothing in flight —
/// which is exactly what the live instrument caught:
/// `phase=Rendering frames=11 pictures=0 failed=0 in_flight=0`, after eleven
/// successful 7.4 MB downloads.
///
/// What is asserted here is the **ask**, read off the production wire — the
/// `SourceEvent::FrameReady` each spawned fetch answers with — and not off the
/// dispatcher's own in-flight set, which is the thing under test and would
/// agree with itself. The fetches fail (the fixture's client cannot connect),
/// which is the point: a granule that never arrives still proves the app went
/// and asked for it.
///
/// **Floor A — `no_refetch`:** delete the `refetch_owed_loop_frames` call from
/// `dispatch_overlay_loop_renders`.
/// **Floor B — `fetch_without_a_record`:** drop the `overlay_record` gate from
/// `dispatch_loop_frame_fetches`, so the frames are spent before the pane can
/// draw them and nothing brings them back.
#[test]
fn a_satellite_loop_asks_again_for_the_granules_it_could_not_yet_draw() {
    let ctx = egui::Context::default();
    let _guard = squallar_worker::offload::install_test_worker(Box::new(DescribedJobs::default()));
    let mut app = satellite_app();
    build_loop(&mut app);
    assert_eq!(
        frame_stamps(&app).len(),
        FRAMES as usize,
        "premise: the listing became a {FRAMES}-frame loop",
    );

    // 1. **The listing's own burst**, spent while the pane has no raster of
    //    this layer to size a frame by. Every one of these fetches is
    //    dispatched, answers, and leaves nothing behind — which is the state
    //    the bug froze in. Drained here so what step 4 reads is a second ask
    //    and not this one still arriving.
    let listing_burst = distinct_asks(&app);
    deliver_fetch_answers(&mut app, &listing_burst);
    assert_eq!(
        listing_burst,
        (0..FRAMES).map(hour).collect::<Vec<_>>(),
        "premise: the listing arrival puts one fetch per frame on the wire",
    );
    assert!(
        frame_textures(&app).iter().all(Option::is_none),
        "premise: none of that burst became a picture — the pane had no \
         raster of its own to size one by",
    );

    // 2. The pane's live raster lands — the production writer of the geometry
    //    record, and the only thing that changes between here and step 1.
    app.spawn_overlay_render(vec![0], known::GMGSI, a_render_request(), None);

    // 3. The every-frame pass, exactly where the pump runs it.
    app.run_frame_pump(crate::app::frame_pump::PumpPhase::Dispatch, Some(&ctx));

    // 4. And now the app asks the layer for every frame it is still owed.
    assert_eq!(
        distinct_asks(&app),
        (0..FRAMES).map(hour).collect::<Vec<_>>(),
        "once the pane can size a raster, every frame owed its granule must be \
         asked for AGAIN; a frame whose granule was spent before the pane \
         could draw it had no other way back, and the loop stayed blank for \
         its whole life",
    );
}

// -- The live bucket, through the real UI (network; `#[ignore]`d) ------------

/// **The user's scenario, every rung real**: a radar-off satellite pane, the
/// real `Gui::ui` pass driving the real `FetchOverlay` and the real
/// `RenderOverlay`, the real `EnableLoop`, the real listing against
/// `noaa-gmgsi-pds`, the real GETs, and the real frame pump — until the loop
/// plays with pictures.
///
/// **Nothing is hand-armed.** `a_satellite_pane_loops_thirteen_hours...` above
/// writes the geometry record with a bare `spawn_overlay_render` call; here
/// the record is written only if the shipped UI really asks for the pane's
/// live raster, which is the rung the user's bug fell through.
///
/// `cargo test -p squallar-app --lib -- --ignored --nocapture live_gmgsi`
#[test]
#[ignore = "hits the live noaa-gmgsi-pds S3 bucket"]
fn live_a_satellite_pane_plays_its_loop_end_to_end() {
    struct PrintLogger;
    impl log::Log for PrintLogger {
        fn enabled(&self, _: &log::Metadata) -> bool {
            true
        }
        fn log(&self, record: &log::Record) {
            if record.level() <= log::Level::Info {
                println!("[{}] {}", record.level(), record.args());
            }
        }
        fn flush(&self) {}
    }
    let _ = log::set_boxed_logger(Box::new(PrintLogger));
    log::set_max_level(log::LevelFilter::Info);

    let ctx = egui::Context::default();
    let mut app = crate::app::tests::headless(crate::platform_double::TestBridge::desktop());
    // The REAL http client the headless app built stays; nothing is doubled.
    draw(&mut app, &known::GMGSI, true);
    draw(&mut app, &known::RADAR, false);
    assert_eq!(
        app.gui.pane(0).expect("pane 0").transport_layer(),
        &known::GMGSI,
        "the radar-off pane's transport must address the satellite layer",
    );

    /// **One frame, in `App::handle_redraw`'s own order**: the `Ingest` pump
    /// where `poll_data_channels` runs it, then `setup_egui_frame`'s three
    /// pumps, `update_loop_readiness` and `push_frame_inputs`, then the
    /// shipped `Gui::ui`, then `process_gui_actions`. Anything reordered here
    /// is a frame this app never runs.
    fn ui_frame(app: &mut crate::app::App, ctx: &egui::Context, time: f64) {
        use crate::app::frame_pump::PumpPhase;
        app.run_frame_pump(PumpPhase::Ingest, None);
        app.run_frame_pump(PumpPhase::Apply, Some(ctx));
        app.run_frame_pump(PumpPhase::Advance, Some(ctx));
        app.run_frame_pump(PumpPhase::Dispatch, Some(ctx));
        app.update_loop_readiness();
        app.push_frame_inputs();
        ctx.begin_pass(egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1024.0, 768.0),
            )),
            time: Some(time),
            max_texture_side: Some(8192),
            ..Default::default()
        });
        let actions = app.gui.ui(ctx);
        let _ = ctx.end_pass();
        app.process_gui_actions(actions);
    }

    // Settle, then wait for the pane's own live satellite picture — the
    // `has_data` the shipped `ui_map_pane` gates its raster ask on.
    let mut time = 100.0f64;
    for step in 0..900usize {
        time += 0.1;
        ui_frame(&mut app, &ctx, time);
        if app.render.overlay_record(0, &known::GMGSI).is_some() {
            println!("live raster record written at step {step}");
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    println!(
        "before EnableLoop: overlay_record={}",
        app.render.overlay_record(0, &known::GMGSI).is_some(),
    );

    // The real door the infinity button knocks on, with the window the UI
    // would floor for this layer (`min_loop_frames` = 13 hourly mosaics).
    app.handle_gui_action(
        squallar_egui::actions::GuiAction::EnableLoop {
            pane_idx: 0,
            lookback_secs: 43_200,
        },
        None,
    );

    let mut last = String::new();
    let mut loaded: Option<usize> = None;
    for step in 0..1800usize {
        time += 0.1;
        ui_frame(&mut app, &ctx, time);

        let staged = {
            let view = app.gui.pane(0).expect("pane 0").view(0);
            app.gui
                .overlays
                .frames_resident(&known::GMGSI, &view.layer(&known::GMGSI))
                .len()
        };
        let has_record = app.render.overlay_record(0, &known::GMGSI).is_some();
        let pane = app.gui.pane(0).expect("pane 0");
        let ls = pane.time_state(&known::GMGSI);
        let pictures = ls.frames.iter().filter(|f| f.image.is_some()).count();
        let line = format!(
            "phase={:?} frames={} pictures={pictures} staged={staged} record={has_record}",
            ls.phase,
            ls.frames.len(),
        );
        if line != last {
            println!("step {step}: {line}");
            last = line;
        }
        if matches!(ls.phase, squallar_egui::pane::LoopPhase::Playing)
            && pictures == ls.frames.len()
        {
            println!("the loop plays with every picture at step {step}");
            loaded = Some(step);
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    let Some(loaded) = loaded else {
        let pane = app.gui.pane(0).expect("pane 0");
        let ls = pane.time_state(&known::GMGSI);
        panic!(
            "the loop never played every picture: {last} (failed={}, in_flight={})",
            ls.frames.iter().filter(|f| f.render_failed).count(),
            ls.frames.iter().filter(|f| f.render_in_flight).count(),
        );
    };

    // **And it must actually move.** A phase reading `Playing` over a frozen
    // playhead is exactly the "nothing plays" the user reported, and every
    // assertion above is blind to it.
    let mut visited: Vec<Option<egui::TextureId>> = Vec::new();
    for _ in 0..200usize {
        time += 0.1;
        ui_frame(&mut app, &ctx, time);
        let pane = app.gui.pane(0).expect("pane 0");
        // **What the map paints**, not what the timeline believes: a
        // `Playing` phase over a frozen picture is the reported bug.
        let at = pane
            .overlay_texture_on_screen(&known::GMGSI)
            .map(|tex| tex.texture.id());
        if !visited.contains(&at) {
            visited.push(at);
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    println!(
        "after loading at step {loaded}, the pane painted {} distinct textures",
        visited.len()
    );
    let pane = app.gui.pane(0).expect("pane 0");
    let ls = pane.time_state(&known::GMGSI);
    assert!(
        visited.len() > 1,
        "the loop reads {:?} but the pane painted one texture for {} frames: \
         nothing plays",
        ls.phase,
        ls.frames.len(),
    );
}
