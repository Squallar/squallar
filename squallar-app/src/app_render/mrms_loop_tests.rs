//! **WB-10: the national mosaic loops** — the last layer conversion of the
//! loop campaign, end to end, against the **real** `MrmsHandler`.
//!
//! Not a double, for the reason `gmgsi_loop_tests` states: a double cannot
//! catch a layer that files its frames wrongly — it agrees with whatever the
//! app assumes. `MrmsListing` and `MrmsFrameFetch` are public for exactly
//! this, so what runs below is the shipping registry: the real
//! `apply_frame_listing`, the real `list_frames`, the real `fetch_frame`, the
//! real frame-addressed `prepare_job`.
//!
//! **What is arranged and what is driven.** The listing and the granules are
//! handed in on the two production arrival paths (`SourceEvent::Frames` and
//! `SourceEvent::FrameReady`), because the bytes behind them are ~1.3 MB
//! objects in an S3 bucket and no test in this tree reaches a network. The
//! HTTP client is replaced with one that cannot connect. Everything between
//! the listing and the picture is the app's own code.
//!
//! **The stamps are not clock-aligned**, here as in the bucket (`000039`,
//! `000242`): the fixture strides 121 s from a :39 start, so any code that
//! rounds a stamp to the nominal 2-minute grid files it under an instant
//! nothing listed, and these tests go red. The stamp is carried whole or the
//! frame is lost.

use squallar_geo::GeoBounds;
use squallar_source::handler::SourceEvent;
use squallar_source::id::known;
use squallar_source::job::DescribedJob;
use squallar_source::time::{FrameListing, FrameStamp};
use std::sync::{Arc, Mutex};

use squallar_overlays::hrrr::GridCoords;
use squallar_overlays::mrms::{MrmsFrameFetch, MrmsGrid, MrmsListing, MrmsProduct};
use squallar_overlays::render::gridded::ResidentGrid;

/// The product the layer opens on, and the one the pane below is drawing.
const PRODUCT: MrmsProduct = MrmsProduct::ReflectivityComposite;

/// Thirteen ~2-minute mosaics — what a Lookback slider set to the window
/// below buys at this cadence, with **no** `min_loop_frames` floor anywhere
/// in the arithmetic: MRMS declares none, because the slider's own spans
/// already yield real loops at 120 s a frame.
const FRAMES: i64 = 13;

/// Stamp `k` of the fixture timeline — non-clock-aligned seconds, uneven
/// against the nominal step, exactly like the bucket's own.
fn t(k: i64) -> chrono::NaiveDateTime {
    chrono::NaiveDate::from_ymd_opt(2026, 8, 21)
        .unwrap()
        .and_hms_opt(0, 0, 39)
        .unwrap()
        + chrono::Duration::seconds(121 * k)
}

fn stamp(k: i64) -> FrameStamp {
    FrameStamp {
        valid: t(k),
        run: None,
    }
}

/// The window the loop is armed over.
fn window() -> (chrono::NaiveDateTime, chrono::NaiveDateTime) {
    (t(0), t(FRAMES - 1))
}

fn bounds() -> GeoBounds {
    GeoBounds {
        min_lat: 34.0,
        max_lat: 36.0,
        min_lon: -99.0,
        max_lon: -97.0,
    }
}

/// A granule for `t(k)` whose **values name the stamp**. Small: the real
/// mosaic is 7000x3500 and what is under test is which grid reaches which
/// frame, not how big it is.
fn granule(k: i64) -> MrmsGrid {
    let spec = squallar_overlays::mrms::fields::spec(PRODUCT);
    MrmsGrid {
        product: PRODUCT,
        grid: Arc::new(ResidentGrid {
            field: spec.id.clone(),
            ni: 4,
            nj: 1,
            coords: GridCoords::Regular {
                lat0: bounds().max_lat,
                lon0: bounds().min_lon,
                dlat: -0.01,
                dlon: (bounds().max_lon - bounds().min_lon) / 3.0,
                ni: 4,
                nj: 1,
                scan_mode: 0,
            },
            values: vec![k as f32; 4],
        }),
        bounds: bounds(),
        valid: t(k),
        visible_points: 4,
        value_range: Some((k as f32, k as f32)),
    }
}

/// A sink that takes every job and keeps what it was **described with**, so
/// two frames' rasters can be told apart without rasterizing either.
/// `DescribedJob`'s equality is the input's own, and MRMS's input is a
/// `GriddedInput::Resident` holding the granule — so two jobs are equal
/// exactly when they carry the same picture.
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
            pane_px: [0, 0],
        },
        data_generation: 0,
        zoom: 32,
    }
}

/// A one-pane app drawing MRMS and nothing else time-shaped, with its HTTP
/// client stubbed so no dispatch below can reach a network.
fn mosaic_app() -> crate::app::App {
    let mut app = crate::app::tests::headless(crate::platform_double::TestBridge::desktop());
    app.http_client = crate::app::tests::unreachable_http_client();

    draw(&mut app, &known::MRMS, true);
    draw(&mut app, &known::RADAR, false);
    app
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

/// Arm pane 0's mosaic loop over [`window`] and answer its listing on the
/// production arrival path, with the keys a real listing would have found —
/// **one day-prefix LIST buys all of them**, which is the cost shape that
/// makes an MRMS frame cheaper to list than a GMGSI one.
fn build_loop(app: &mut crate::app::App) {
    let pane = app.gui.pane_mut(0).expect("the fixture built a pane");
    pane.set_transport_layer(known::MRMS);
    *pane.time_state_mut(&known::MRMS) = squallar_egui::pane::LayerTimeState::begin(
        (window().1 - window().0).num_seconds() as u64,
        squallar_radar::types::RenderView::PlanView,
        Box::new(()),
    );
    pane.time_state_mut(&known::MRMS).asked_range = Some(window());

    let keys: Vec<(chrono::NaiveDateTime, String)> = (0..FRAMES)
        .map(|k| {
            (
                t(k),
                squallar_source::origins::DataSources::mrms_key(PRODUCT.prefix_name(), &t(k)),
            )
        })
        .collect();
    app.channels
        .overlay_fetch_sender
        .send(SourceEvent::Frames {
            id: known::MRMS,
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
            scope: Box::new(MrmsListing {
                product: PRODUCT,
                range: window(),
                keys,
                complete: true,
            }),
        })
        .expect("the receiver is alive");
    app.poll_overlay_fetch_results();
    app.accept_loop_scan_listings();
}

/// Deliver one frame's granule on the production arrival path.
fn deliver_granule(app: &mut crate::app::App, k: i64) {
    app.channels
        .overlay_fetch_sender
        .send(SourceEvent::FrameReady {
            id: known::MRMS,
            stamp: stamp(k),
            data: Box::new(MrmsFrameFetch {
                product: PRODUCT,
                valid: t(k),
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
            ink: true,
            image: Some(Arc::new(egui::ColorImage::from_rgba_unmultiplied(
                [1, 1],
                &[shade, shade, shade, 255],
            ))),
            geo_bounds: bounds(),
            overlay_kind: known::MRMS,
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
        .time_state(&known::MRMS)
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
        .time_state(&known::MRMS)
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

/// **The acceptance of WB-10.** An MRMS pane with a loop enabled ends with
/// thirteen frames over the window, each carrying its **own** picture, each
/// under its **own** non-clock-aligned stamp.
///
/// Five things, in the order the app does them:
///
/// 1. the layer's own listing became the loop's frame list, stamps carried
///    whole — seconds and all;
/// 2. one granule is staged at a time (pinned at the layer, where the budget
///    can be injected — see `squallar-overlays`);
/// 3. every frame's raster is described from **that frame's** granule — the
///    thirteen jobs are pairwise different;
/// 4. each result files itself to the frame that asked;
/// 5. thirteen frames hold thirteen **different** textures.
///
/// Claim 3 is the `ctx.frame` gap the audit named: a `FrameSeries` layer that
/// ignores `ctx.frame` hands every frame the same picture, and no assertion
/// over texture *presence* can see it. Claims 4 and 5 are also where the
/// audit's `content_signature` worry lands: frame identity travels on the
/// response's own `frame` stamp and each frame keeps its own texture, so two
/// frames cannot share one — asserted on identity, not presence.
///
/// **Floor A — `ignore_the_frame`:** drop the `ctx.frame` arm of MRMS's
/// `prepare_job` so it reads the live cache.
///
/// **Floor B — `forget_the_keys`:** return from `apply_frame_listing` before
/// it files anything — the seam a layer-only suite cannot see.
#[test]
fn a_mosaic_pane_loops_thirteen_frames_each_with_its_own_picture_end_to_end() {
    let ctx = egui::Context::default();
    let jobs: Arc<Mutex<Vec<DescribedJob>>> = Default::default();
    let _guard = squallar_worker::offload::install_test_worker(Box::new(DescribedJobs {
        jobs: Arc::clone(&jobs),
    }));

    let mut app = mosaic_app();
    assert_eq!(
        app.gui.pane(0).expect("pane 0").transport_layer(),
        &known::MRMS,
        "premise: MRMS is a FrameSeries layer since WB-10, so a pane drawing \
         it and no radar addresses it as its transport. Before WB-10 it was \
         `Live` and this pane had no transport at all",
    );

    build_loop(&mut app);

    // 1. Thirteen frames, stamps carried whole.
    assert_eq!(
        frame_stamps(&app),
        (0..FRAMES).map(t).collect::<Vec<_>>(),
        "the layer's listing must have become the loop's frame list, with \
         the bucket's non-clock-aligned seconds intact — a rounded stamp is \
         an instant nothing listed",
    );

    // The geometry record a loop frame's raster reuses is written by the
    // pane's own live dispatch, exactly as it is in production.
    app.spawn_overlay_render(vec![0], known::MRMS, a_render_request(), None);
    assert!(
        jobs.lock().expect("no poisoned lock").is_empty(),
        "control: with no live granule the live dispatch describes nothing, \
         so every job counted below is a loop frame's and none is the pane's \
         own picture",
    );

    // 2, 3, 4. Arrive, draw, arrive, draw — the order the serialised fetch
    // and the one-granule staging area impose in production. Residency is NOT
    // asserted here: the shipped staging budget is one 98 MB mosaic and these
    // granules are sixteen bytes, so an eviction assertion over them could
    // not fail. It is pinned where the budget can be injected —
    // `the_layer_stages_one_granule_however_many_frames_the_loop_holds` in
    // `squallar-overlays`.
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
                 Every frame of the loop would be one instant's mosaic under \
                 thirteen captions, and nothing else in this build detects it",
            );
        }
    }

    // 5. Thirteen frames, thirteen textures — identity, not presence.
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

/// **The WB-10 clock ruling, read at the pane** — and the neighbours
/// untouched, one assertion each.
///
/// The transport is the topmost enabled `FrameSeries` layer, zero special
/// cases. MRMS's 15 sits above the model's 10 and GMGSI's 5 and below
/// radar's 30, so:
///
/// * radar on: radar keeps the clock, exactly as before this item;
/// * radar off, model + MRMS: **MRMS takes the clock** — the ruling. MRMS is
///   observed radar on the finest non-radar cadence, and a user who wants
///   the model's clock disables MRMS on that pane;
/// * radar off, satellite + MRMS: MRMS again — GMGSI keeps taking the clock
///   from nothing, exactly as WB-11 ruled.
///
/// **Floor — `mosaic_below_the_model`:** `Mrms::draw_order_weight` 15 -> 8.
/// The model+MRMS arm goes red here, and the ordering pin in
/// `squallar-egui`'s `radar_takes_the_clock_wherever_it_is_drawn` goes red
/// with it.
#[test]
fn the_mosaic_takes_the_clock_of_a_radar_off_pane_and_of_nothing_else() {
    let mut app = crate::app::tests::headless(crate::platform_double::TestBridge::desktop());

    // Radar out of the box, with the mosaic drawn on top of it.
    draw(&mut app, &known::MRMS, true);
    assert_eq!(
        app.gui.pane(0).expect("pane 0").transport_layer(),
        &known::RADAR,
        "a pane drawing radar and the mosaic still walks radar's clock — \
         radar is byte-for-byte the transport it was before WB-10",
    );

    // The model beside the mosaic, radar off: the ruling.
    draw(&mut app, &known::MODEL_DATA, true);
    draw(&mut app, &known::RADAR, false);
    assert_eq!(
        app.gui.pane(0).expect("pane 0").transport_layer(),
        &known::MRMS,
        "the WB-10 ruling: on a radar-off pane the national mosaic outranks \
         the model (15 over 10) — it is observed radar, on a 2-minute scrub \
         grain against the model's hour, and the topmost-wins rule keeps \
         zero special cases",
    );

    // The satellite beside the mosaic, model off: GMGSI still yields.
    draw(&mut app, &known::MODEL_DATA, false);
    draw(&mut app, &known::GMGSI, true);
    assert_eq!(
        app.gui.pane(0).expect("pane 0").transport_layer(),
        &known::MRMS,
        "GMGSI takes the clock from nothing (WB-11), the mosaic included",
    );

    // And with the mosaic off, the model's clock is exactly what it was.
    draw(&mut app, &known::GMGSI, false);
    draw(&mut app, &known::MODEL_DATA, true);
    draw(&mut app, &known::MRMS, false);
    assert_eq!(
        app.gui.pane(0).expect("pane 0").transport_layer(),
        &known::MODEL_DATA,
        "disabling MRMS is the gesture that hands a mixed pane back to the \
         model's clock, and the model's own transport is untouched by WB-10",
    );
}
