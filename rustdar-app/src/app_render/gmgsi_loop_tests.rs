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
//! declines a staged one is pinned in `rustdar-overlays` by
//! `a_frame_fetch_is_declined_where_there_is_no_key_and_where_the_granule_is_held`.
//! Everything between the listing and the picture is the app's own code.
//!
//! **The arrival order is the design, not a convenience.** GMGSI serialises
//! its frame fetches (one 60 MB decode at a time — thirteen at once is 780 MB)
//! and stages one granule, so the production sequence really is arrive, draw,
//! arrive, draw. The loop below is that sequence.

use rustdar_geo::GeoBounds;
use rustdar_source::handler::SourceEvent;
use rustdar_source::id::known;
use rustdar_source::job::DescribedJob;
use rustdar_source::time::{FrameListing, FrameStamp};
use std::sync::{Arc, Mutex};

use rustdar_overlays::gmgsi::decode::GmgsiGrid;
use rustdar_overlays::gmgsi::{GmgsiChannel, GmgsiFrameFetch, GmgsiListing};
use rustdar_overlays::hrrr::GridCoords;
use rustdar_overlays::render::gridded::ResidentGrid;

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
    let spec = rustdar_overlays::gmgsi::fields::spec(CHANNEL);
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

impl rustdar_worker::offload::JobSink for DescribedJobs {
    fn send(
        &self,
        _id: u64,
        request: rustdar_worker::offload::JobRequest,
    ) -> Result<(), rustdar_worker::offload::JobRequest> {
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
        texture: rustdar_egui::overlay_cache::OverlayTexturePlan {
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
fn draw(app: &mut crate::app::App, id: &rustdar_source::id::LayerId, on: bool) {
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
    *pane.time_state_mut(&known::GMGSI) = rustdar_egui::pane::LayerTimeState::begin(
        (window().1 - window().0).num_seconds() as u64,
        rustdar_radar::types::RenderView::PlanView,
        Box::new(()),
    );

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
                .and_then(rustdar_egui::pane::LoopFrameImage::overlay)
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
/// `rustdar-egui`, which reads the window off the action the real infinity
/// button emitted. Residency is likewise not floored here — see the comment on
/// the arrival loop for the denominator that makes it impossible.
#[test]
fn a_satellite_pane_loops_thirteen_hours_each_with_its_own_picture_end_to_end() {
    let ctx = egui::Context::default();
    let jobs: Arc<Mutex<Vec<DescribedJob>>> = Default::default();
    let _guard = rustdar_worker::offload::install_test_worker(Box::new(DescribedJobs {
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
    // `rustdar-overlays`, at a one-grid budget with thirteen frames listed.
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
