//! **WI-6b: a non-radar loop's frames get their pictures.**
//!
//! WI-6 landed the consumer — a `LoopFrame` can hold a
//! [`LoopFrameImage::Overlay`] and `overlay_texture_on_screen` paints the one
//! under the playhead. Nothing built one. `spawn_overlay_render` rasterized
//! whichever grid the pane's own selection named, and `OverlayRenderResponse`
//! carried no stamp to file a result under, so a forecast loop sat in
//! `Rendering` for ever and the map went empty of that layer.
//!
//! This is one end-to-end test through the production path, plus the two
//! properties that need their own arrangement. The end-to-end one drives the
//! real arrival (`SourceEvent::Frames` -> `poll_overlay_fetch_results` ->
//! `accept_loop_scan_listings`), the real dispatch
//! (`dispatch_overlay_loop_renders` -> `spawn_overlay_render` -> the job
//! funnel) and the real arrival back (`poll_overlay_render_results`). Nothing
//! is hand-armed except the layer itself.
//!
//! **The layer under test answers to [`known::MODEL_DATA`] and is a double.**
//! Not a made-up id: `spawn_overlay_render` routes by an explicit match on ten
//! known ids and sends everything else to a `log::warn!` and a cleared mark, so
//! a fake id would test the fallback arm rather than the dispatch. The
//! *behaviour* under test is the app's; the model layer's own half — that a
//! named frame is rasterized from that frame's grid — is pinned in
//! `rustdar-overlays` by
//! `a_named_frame_is_rasterized_from_that_frames_grid_and_not_the_panes`.
//!
//! Each claim names the mutation that turns it red; all five were applied and
//! observed.

use super::*;
use rustdar_geo::GeoBounds;
use rustdar_source::handler::{FetchPayload, PaneRef};
use rustdar_source::id::{LayerId, known};
use rustdar_source::job::DescribedJob;
use rustdar_source::time::{FrameListing, FrameStamp};
use std::sync::{Arc, Mutex};

use rustdar_overlays::render::overlay_state::{
    FetchConfig, FetchTask, OverlayHandler, OverlayRegistry, RenderMode, SourceEvent, Surface,
};

/// The run every stamp in this suite belongs to. Named, because carrying it is
/// half of what the dispatch is for: two model runs both publish a frame valid
/// at the same instant, and a `LoopFrame` holds only the instant.
fn run() -> chrono::NaiveDateTime {
    chrono::NaiveDate::from_ymd_opt(2026, 7, 25)
        .unwrap()
        .and_hms_opt(3, 0, 0)
        .unwrap()
}

/// The frame valid `hour` hours into the run.
fn ts(hour: i64) -> chrono::NaiveDateTime {
    run() + chrono::Duration::hours(hour)
}

fn stamp(hour: i64) -> FrameStamp {
    FrameStamp {
        valid: ts(hour),
        run: Some(run()),
    }
}

fn bounds() -> GeoBounds {
    GeoBounds {
        min_lat: 34.0,
        max_lat: 36.0,
        min_lon: -99.0,
        max_lon: -97.0,
    }
}

/// What the layer double was asked, in call order.
#[derive(Default)]
struct Asked {
    /// One entry per `prepare_job`, holding that dispatch's `ctx.frame`.
    /// `None` is a live raster — the pane's own picture.
    prepared: Vec<Option<FrameStamp>>,
    /// One entry per `fetch_frame`, whole. The `run` half is what WI-5's
    /// dispatch dropped: it built `FrameStamp { valid, run: None }` from the
    /// frame list, and the model layer's `frame_target` — the resolver behind
    /// `fetch_frame` — answers `None` without a run, so every model frame
    /// fetch was declined before it was built.
    fetched: Vec<FrameStamp>,
}

/// A `FrameSeries` texture layer that lists frames, holds all of them, and
/// writes down every stamp it is asked to prepare or fetch.
struct FrameLayer {
    listed: Vec<chrono::NaiveDateTime>,
    asked: Arc<Mutex<Asked>>,
}

impl OverlayHandler for FrameLayer {
    fn id(&self) -> LayerId {
        known::MODEL_DATA
    }
    fn surface(&self) -> Surface {
        Surface::Ground
    }
    fn draw_order_weight(&self) -> u32 {
        999
    }
    fn display_name(&self) -> &str {
        "Frames"
    }
    fn render_mode(&self) -> RenderMode {
        RenderMode::Texture
    }
    fn data_generation(&self) -> u64 {
        0
    }
    fn has_data(&self, _pane: &PaneRef<'_>) -> bool {
        true
    }
    fn is_fetching(&self) -> bool {
        false
    }
    fn set_fetching(&mut self, _f: bool, _pane: &PaneRef<'_>) {}
    fn fetch_time(&self) -> Option<web_time::Instant> {
        None
    }
    fn apply_fetch_result(&mut self, _result: FetchPayload, _pane: &PaneRef<'_>) {}
    fn retain_selections(
        &self,
        _selections: &mut Vec<Arc<dyn rustdar_overlays::render::overlay_state::OverlayItem>>,
        _pane: &PaneRef<'_>,
    ) {
    }

    fn time_axis(&self) -> rustdar_source::time::TimeAxis {
        rustdar_source::time::TimeAxis::FrameSeries {
            typical_step: std::time::Duration::from_secs(3600),
            extends_future: true,
        }
    }

    fn frame_horizon(&self, _pane: &PaneRef<'_>) -> chrono::Duration {
        chrono::Duration::hours(18)
    }

    fn list_frames(
        &self,
        _ctx: &FetchConfig,
        _pane: &PaneRef<'_>,
        range: (chrono::NaiveDateTime, chrono::NaiveDateTime),
    ) -> FrameListing {
        FrameListing {
            range,
            frames: self
                .listed
                .iter()
                .filter(|valid| range.0 <= **valid && **valid <= range.1)
                .map(|valid| FrameStamp {
                    valid: *valid,
                    run: Some(run()),
                })
                .collect(),
            complete: true,
        }
    }

    /// Everything it listed is held. The suite is about the raster, not about
    /// the fetch, so residency is arranged rather than driven.
    fn frames_resident(&self, _pane: &PaneRef<'_>) -> Vec<FrameStamp> {
        self.listed
            .iter()
            .map(|valid| FrameStamp {
                valid: *valid,
                run: Some(run()),
            })
            .collect()
    }

    fn fetch_frame(
        &self,
        _ctx: &FetchConfig,
        _pane: &PaneRef<'_>,
        stamp: &FrameStamp,
    ) -> Option<FetchTask> {
        self.asked
            .lock()
            .expect("no poisoned lock")
            .fetched
            .push(*stamp);
        Some(FetchTask {
            kind: known::MODEL_DATA,
            future: Box::pin(async move { Box::new(()) as FetchPayload }),
        })
    }

    /// **The instrument.** The dispatch's whole claim is that a loop frame's
    /// raster is described for *that frame*, so the double records the frame it
    /// was described for and hands back a job whose contents nothing reads.
    fn prepare_job(
        &self,
        ctx: &rustdar_overlays::render::overlay_state::RasterizeContext,
        _pane: &PaneRef<'_>,
    ) -> Option<DescribedJob> {
        self.asked
            .lock()
            .expect("no poisoned lock")
            .prepared
            .push(ctx.frame);
        Some(DescribedJob::new(
            rustdar_overlays::render::rasterize::SitesInput {
                sites: Vec::new(),
                zoom: ctx.zoom,
                is_dark: ctx.is_dark,
                device_scale: ctx.device_scale,
            },
        ))
    }

    fn job_codec(&self) -> Option<&'static rustdar_source::job::JobCodec> {
        rustdar_overlays::render::jobs::JOB_CODECS
            .iter()
            .find(|row| row.label == "overlay/sites")
    }
}

/// A sink that takes every job and records nothing but the count — the funnel
/// must not actually rasterize here, and the responses are delivered by hand
/// below so each frame's picture can be told from its neighbour's.
struct TakeAll {
    taken: Arc<Mutex<usize>>,
}

impl rustdar_worker::offload::JobSink for TakeAll {
    fn send(
        &self,
        _id: u64,
        _request: rustdar_worker::offload::JobRequest,
    ) -> Result<(), rustdar_worker::offload::JobRequest> {
        *self.taken.lock().expect("no poisoned lock") += 1;
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
        data_generation: 5,
        zoom: 32,
    }
}

/// A one-pane app whose model slot is the double, with `listed` frames on
/// offer.
fn app_with_frames(listed: Vec<chrono::NaiveDateTime>) -> (crate::app::App, Arc<Mutex<Asked>>) {
    let asked: Arc<Mutex<Asked>> = Default::default();
    let mut app = crate::app::tests::headless(crate::platform_double::TestBridge::desktop());
    app.gui.overlays = OverlayRegistry::with_handlers(vec![Box::new(FrameLayer {
        listed,
        asked: Arc::clone(&asked),
    })]);
    (app, asked)
}

/// Put pane 0's model layer into a loop waiting on a listing over `range`, then
/// answer it on the one production arrival path.
fn build_loop(app: &mut crate::app::App, range: (chrono::NaiveDateTime, chrono::NaiveDateTime)) {
    let pane = app.gui.pane_mut(0).expect("the fixture built a pane");
    pane.set_transport_layer(known::MODEL_DATA);
    *pane.time_state_mut(&known::MODEL_DATA) = rustdar_egui::pane::LayerTimeState::begin(
        (range.1 - range.0).num_seconds() as u64,
        rustdar_radar::types::RenderView::PlanView,
        Box::new(()),
    );
    app.channels
        .overlay_fetch_sender
        .send(SourceEvent::Frames {
            id: known::MODEL_DATA,
            listing: FrameListing {
                range,
                frames: Vec::new(),
                complete: true,
            },
            scope: Box::new(()),
        })
        .expect("the receiver is alive");
    app.poll_overlay_fetch_results();
    app.accept_loop_scan_listings();
}

/// The stamps the frame list ended up holding, oldest first.
fn frame_stamps(app: &crate::app::App) -> Vec<chrono::NaiveDateTime> {
    app.gui
        .pane(0)
        .expect("pane 0")
        .time_state(&known::MODEL_DATA)
        .frames
        .iter()
        .map(|f| f.timestamp)
        .collect()
}

/// The texture id on each frame, `None` where a frame has no picture. Identity
/// and not presence: the defect this suite exists for hands every frame the
/// *same* handle, which reads green against any `is_some()`.
fn frame_textures(app: &crate::app::App) -> Vec<Option<egui::TextureId>> {
    app.gui
        .pane(0)
        .expect("pane 0")
        .time_state(&known::MODEL_DATA)
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

/// Deliver one finished raster for `stamp`, with a picture nothing else shares.
fn deliver_raster(app: &mut crate::app::App, ctx: &egui::Context, stamp: FrameStamp, shade: u8) {
    let image = Arc::new(egui::ColorImage::from_rgba_unmultiplied(
        [1, 1],
        &[shade, shade, shade, 255],
    ));
    app.channels
        .overlay_render_sender
        .send(crate::channels::OverlayRenderResponse {
            image: Some(image),
            geo_bounds: bounds(),
            overlay_kind: known::MODEL_DATA,
            generation: 5,
            pane_indices: vec![0],
            zoom: 32,
            hit_map: None,
            frame: Some(stamp),
        })
        .expect("the receiver lives on the App");
    app.poll_overlay_render_results(ctx);
}

// ── The end-to-end claim ────────────────────────────────────────────────────

/// **The acceptance of WI-6b, through the production path from a listing
/// arriving to a picture sitting on a frame.**
///
/// Four things are asserted in the order the frame does them:
///
/// 1. the frame fetches WI-5 dispatches carry the **run** the listing named;
/// 2. the dispatch asks for one raster **per frame**, each describing **its own
///    stamp**;
/// 3. a result files itself to the frame **that asked**, not to the frame at
///    the index it was dispatched at;
/// 4. two frames end up holding two **different textures**.
///
/// **Floor 1 — `drop_the_run`:** put `FrameStamp { valid, run: None }` back in
/// `accept_loop_scan_listings`. The first block fails; with the real model
/// handler in place this is the difference between a forecast loop that fetches
/// its grids and one that fetches nothing at all.
///
/// **Floor 2 — `ignore_residency`:** make the dispatch build its own stamp from
/// `frame.timestamp` instead of taking the layer's. The same block fails on the
/// run again, from the other end.
///
/// **Floor 3 — `file_to_first_frame`:** file every result to `frames[0]`. Step
/// 3 fails: the picture meant for the *second* frame lands on the first.
#[test]
fn a_forecast_loops_frames_each_get_their_own_picture_end_to_end() {
    let ctx = egui::Context::default();
    let taken = Arc::new(Mutex::new(0usize));
    let _guard = rustdar_worker::offload::install_test_worker(Box::new(TakeAll {
        taken: Arc::clone(&taken),
    }));

    let (mut app, asked) = app_with_frames(vec![ts(0), ts(1), ts(2)]);
    build_loop(&mut app, (ts(0), ts(2)));

    assert_eq!(
        frame_stamps(&app),
        vec![ts(0), ts(1), ts(2)],
        "premise: the listing must have become the layer's frame list, or \
         there is nothing to render",
    );

    // 1. The run survives the frame list.
    let fetched = asked.lock().expect("no poisoned lock").fetched.clone();
    assert_eq!(
        fetched.len(),
        3,
        "one fetch per frame the loop means to hold",
    );
    assert!(
        fetched.iter().all(|f| f.run == Some(run())),
        "a frame fetch was dispatched with no run: {fetched:?}. The model \
         layer's `frame_target` answers `None` without one, so every frame of \
         a real forecast loop would be declined and no grid would ever land.",
    );

    // The pane's live raster, which is what writes the geometry record the
    // loop dispatch reuses — and the control that a dispatch naming no frame
    // still describes the pane's own picture.
    app.spawn_overlay_render(vec![0], known::MODEL_DATA, a_render_request(), None);
    assert_eq!(
        asked.lock().expect("no poisoned lock").prepared,
        vec![None],
        "control: the live dispatch described a frame. Every pane that is not \
         looping would now rasterize something other than its own selection.",
    );

    // 2. One raster per frame, each naming its own stamp. Sorted, because the
    //    render set walks outward from the playhead rather than oldest-first —
    //    which of the three is asked for first is `render_set_indices`'s
    //    business and is pinned there.
    app.dispatch_overlay_loop_renders();
    let prepared = asked.lock().expect("no poisoned lock").prepared.clone();
    assert_eq!(prepared.first(), Some(&None), "the live raster came first");
    let mut loop_asks: Vec<FrameStamp> = prepared[1..]
        .iter()
        .map(|f| f.expect("a loop dispatch that named no frame"))
        .collect();
    loop_asks.sort_by_key(|f| f.valid);
    assert_eq!(
        loop_asks,
        vec![stamp(0), stamp(1), stamp(2)],
        "the loop dispatch did not describe one raster per frame at that \
         frame's own stamp (run included). Anything else means every frame of \
         the loop receives one picture under three captions.",
    );
    assert_eq!(
        *taken.lock().expect("no poisoned lock"),
        4,
        "one job on the funnel per description — the live one plus three \
         frames. Heavy work off the frame thread is the whole reason this goes \
         through `spawn_overlay_render` rather than rasterizing inline.",
    );
    assert!(
        app.gui
            .pane(0)
            .expect("pane 0")
            .time_state(&known::MODEL_DATA)
            .frames
            .iter()
            .all(|f| f.render_in_flight),
        "a dispatched frame was left unmarked, so the next pass would ask for \
         it again and spend the bytes twice",
    );

    // 3. A result lands on the frame that asked. Delivered out of order, so an
    //    arrival filing by position rather than by stamp is caught.
    deliver_raster(&mut app, &ctx, stamp(1), 40);
    let after_one = frame_textures(&app);
    assert!(
        after_one[1].is_some(),
        "the raster for the second frame did not reach it",
    );
    assert_eq!(
        (after_one[0], after_one[2]),
        (None, None),
        "one frame's raster landed on another frame. A loop filing by the \
         index it dispatched at is wrong the moment the list is re-sampled \
         under a render that is still in the air.",
    );

    // 4. Two frames, two textures.
    deliver_raster(&mut app, &ctx, stamp(0), 200);
    let both = frame_textures(&app);
    let (Some(first), Some(second)) = (both[0], both[1]) else {
        panic!("both delivered frames must be holding a picture: {both:?}");
    };
    assert_ne!(
        first, second,
        "two frames at two stamps are holding the SAME texture. This is the \
         assertion a presence check cannot make: the loop would animate, and \
         every frame of it would be the same picture.",
    );
}

// ── The bound ───────────────────────────────────────────────────────────────

/// **The dispatch is bounded by the byte share, re-derived every pass** — and
/// the bound bites when the share moves under a frame list that was built for a
/// larger one.
///
/// WI-5 sampled the frame *list* to `overlay_frames_held` when the listing
/// landed. That figure is not stable: the pane can be resized (an overlay
/// frame's texture is planned off the pane rect, so its cost grows with it),
/// `LoopPool` re-plans the allocation at runtime, and a second layer can start
/// animating and halve the share. So the dispatch re-derives it and evicts
/// what the share no longer covers **before** asking for more.
///
/// The denominators, stated: `overlay_frame_bytes` reads the pane's own live
/// raster — here a 1x1 texture, 4 bytes — and falls back to
/// `Budgets::loop_frame_bytes()` before one exists. `overlay_frames_held`
/// divides the pane's `share_bytes` by that and floors at
/// `MIN_LOOP_FRAMES_PER_PANE`. On a real 1280x960-**point** pane at 1x the
/// figure is 1920x1440x4 = **11.06 MB per frame**, so the 56 MiB wasm pool
/// floor buys **5** such frames for the whole application and two animating
/// layers buy **2** each — the floor, which `overlay_frames_held` documents
/// itself as exceeding the byte bound to honour.
///
/// **The arrangement is the one that makes the bound non-vacuous.** WI-5 sizes
/// the frame list from `Budgets::loop_frame_bytes()` — the radar figure — because
/// before the first raster there is nothing to measure. The moment a real
/// overlay raster lands, `overlay_frame_bytes` reads it instead, and on a large
/// pane it is bigger. The list is then longer than the share pays for, and
/// nothing but this dispatch gives the difference back.
///
/// **Floor 1 — `unbounded_dispatch`:** delete the eviction and take the whole
/// frame list as the budget. Every frame is then kept, and the byte assertion
/// fails naming the count and the bytes.
///
/// **Floor 2 — `no_burst_cap`:** delete the per-pass `break`. The first pass
/// asks for every frame the list holds instead of four.
#[test]
fn the_dispatch_holds_no_more_frames_than_the_pane_s_byte_share_buys() {
    let ctx = egui::Context::default();
    let taken = Arc::new(Mutex::new(0usize));
    let _guard = rustdar_worker::offload::install_test_worker(Box::new(TakeAll {
        taken: Arc::clone(&taken),
    }));

    // Ten-minute stamps, enough of them that the list is capped by the byte
    // share rather than by the listing.
    let listed: Vec<chrono::NaiveDateTime> = (0..80)
        .map(|i| ts(0) + chrono::Duration::minutes(i * 10))
        .collect();
    let (mut app, asked) = app_with_frames(listed.clone());
    build_loop(&mut app, (listed[0], listed[listed.len() - 1]));

    let allocation = app.loop_allocation();
    let budgets = app.budgets;
    let animating = app.gui.pane(0).expect("pane 0").animating_layers().count();
    // The figure WI-5 built the list under: no raster has landed, so
    // `overlay_frame_bytes` falls back to the radar loop frame's own cost.
    let fallback_bytes = budgets.loop_frame_bytes();
    let held_at_build = overlay_frames_held(allocation, fallback_bytes, animating);
    let built = frame_stamps(&app).len();
    assert_eq!(
        built,
        held_at_build,
        "premise: the list must have been capped by the share, not by the \
         listing — {} bytes of share / {fallback_bytes} bytes per frame across \
         {animating} animating layer(s) = {held_at_build}, from {} listed",
        allocation.share_bytes,
        listed.len(),
    );

    // **A layer that has never rasterized on this pane is skipped**: the loop
    // reuses the geometry of the pane's own live raster and will not invent a
    // viewport for one that does not exist.
    app.dispatch_overlay_loop_renders();
    assert_eq!(
        *taken.lock().expect("no poisoned lock"),
        0,
        "the loop dispatch rasterized against a viewport nothing had ever          asked this layer for on this pane",
    );
    app.spawn_overlay_render(vec![0], known::MODEL_DATA, a_render_request(), None);
    asked.lock().expect("no poisoned lock").prepared.clear();
    *taken.lock().expect("no poisoned lock") = 0;

    // **The burst cap.** Every one of these frames is owed a picture, and one
    // pass asks for a bounded few: an overlay raster is offloaded, but it
    // shares the job funnel with the live rasters an interacting user is
    // waiting on, and a measured CONUS HRRR rasterize is 133 ms median. The
    // rest are left alone, not retired — the next pass asks again.
    app.dispatch_overlay_loop_renders();
    assert_eq!(
        asked.lock().expect("no poisoned lock").prepared.len(),
        rustdar_device_profile::constants::MAX_OVERLAY_LOOP_RENDERS_PER_PASS,
        "one pass put {built} frames' worth of rasterizes on the funnel at          once, ahead of whatever the user is waiting on",
    );
    assert_eq!(
        *taken.lock().expect("no poisoned lock"),
        rustdar_device_profile::constants::MAX_OVERLAY_LOOP_RENDERS_PER_PASS,
        "and every one of them reached the funnel",
    );

    // Every frame gets its picture, through the real arrival.
    for (i, valid) in frame_stamps(&app).into_iter().enumerate() {
        deliver_raster(
            &mut app,
            &ctx,
            FrameStamp {
                valid,
                run: Some(run()),
            },
            i as u8,
        );
    }
    assert!(
        frame_textures(&app).iter().all(Option::is_some),
        "premise: every frame must be textured before the share moves, or the \
         eviction has nothing to give back",
    );

    // **A second layer starts animating.** The pane's share divides across
    // every layer that is looping, so the list built for one layer's whole
    // share is now twice what this layer is entitled to. Nothing re-lists and
    // nothing re-samples — this dispatch is the only thing that gives the
    // difference back.
    *app.gui
        .pane_mut(0)
        .expect("pane 0")
        .time_state_mut(&known::RADAR) = rustdar_egui::pane::LayerTimeState::begin(
        3600,
        rustdar_radar::types::RenderView::PlanView,
        Box::new(()),
    );
    let animating_now = app.gui.pane(0).expect("pane 0").animating_layers().count();
    assert_eq!(animating_now, 2, "premise: two layers are animating now");

    let frame_bytes = overlay_frame_bytes(
        app.gui.pane(0).expect("pane 0"),
        &known::MODEL_DATA,
        &budgets,
    );
    let held_now = overlay_frames_held(allocation, frame_bytes, animating_now);
    assert!(
        held_now < built,
        "premise: the second animating layer must have made the built list \
         unaffordable, or there is no bound to observe — {built} frames x \
         {frame_bytes} B = {} B against {} B of share divided {animating_now} \
         ways",
        built * frame_bytes,
        allocation.share_bytes,
    );

    asked.lock().expect("no poisoned lock").prepared.clear();
    *taken.lock().expect("no poisoned lock") = 0;
    app.dispatch_overlay_loop_renders();

    let textured = frame_textures(&app).iter().filter(|t| t.is_some()).count();
    assert_eq!(
        textured,
        held_now,
        "the pane is holding {textured} textures where its share buys \
         {held_now}: {} B held against {} B — the pane's {} B of pool share \
         divided {animating_now} ways — at {frame_bytes} B a frame. None of \
         this is visible to `LoopPool`: `LoopFrameModel` still has no \
         `overlay` arm and `layer_share` divides frame COUNT rather than bytes \
         (WB-7), so nothing else in the build would ever notice.",
        textured * frame_bytes,
        allocation.share_bytes / animating_now,
        allocation.share_bytes,
    );
    assert!(
        asked.lock().expect("no poisoned lock").prepared.is_empty(),
        "the dispatch asked for a raster it had just evicted, which is a loop \
         that spends its whole share re-rendering the frames it keeps throwing \
         away",
    );
    assert_eq!(
        *taken.lock().expect("no poisoned lock"),
        0,
        "and nothing reached the funnel",
    );
}

// ── Non-triviality ──────────────────────────────────────────────────────────

/// **A pane that is not looping this layer is untouched by any of it.**
///
/// The control for the whole item: the loop dispatch must find nothing to do on
/// an ordinary pane, and the live raster path must describe exactly what it
/// described before `RasterizeContext::frame` existed. A producer that fires on
/// a non-looping pane would rasterize the pane's picture into frames it has
/// none of, and — worse — would spend the loop byte allowance on a pane that is
/// not animating at all.
///
/// **Floor 1 — `no_animating_filter`:** walk `pane.layers` instead of
/// `pane.animating_layers()`. The last block fails: a timeline that has been
/// switched off goes on being rendered into and goes on spending the share.
///
/// **Floor 2 — `skip_the_live_mark`:** drop the `None` arm of the in-flight
/// mark in `spawn_overlay_render`. The pane's overlay cache is never marked,
/// so the draw loop re-asks for the same live raster on every frame.
#[test]
fn a_pane_that_is_not_looping_dispatches_nothing_and_describes_its_own_picture() {
    let taken = Arc::new(Mutex::new(0usize));
    let _guard = rustdar_worker::offload::install_test_worker(Box::new(TakeAll {
        taken: Arc::clone(&taken),
    }));

    let (mut app, asked) = app_with_frames(vec![ts(0), ts(1)]);
    // No loop: the pane is showing this layer live, as every pane does today.

    app.dispatch_overlay_loop_renders();
    assert!(
        asked.lock().expect("no poisoned lock").prepared.is_empty(),
        "the loop dispatch described a raster for a pane with no loop at all",
    );
    assert_eq!(
        *taken.lock().expect("no poisoned lock"),
        0,
        "the loop dispatch put a job on the funnel for a pane with no loop",
    );

    app.spawn_overlay_render(vec![0], known::MODEL_DATA, a_render_request(), None);
    assert_eq!(
        asked.lock().expect("no poisoned lock").prepared,
        vec![None],
        "a live raster is described for a named frame. Every non-looping pane \
         in the build would rasterize something other than its own selection.",
    );
    assert!(
        app.gui
            .pane_mut(0)
            .expect("pane 0")
            .overlay_cache_mut(&known::MODEL_DATA)
            .render_in_flight,
        "the live dispatch stopped marking the pane's overlay cache in flight, \
         so the draw loop would ask for the same raster on every frame",
    );

    // **A timeline that has been switched off is not rendered into, even while
    // its frames are still standing.** The dispatch gates on the layer's own
    // `is_active()` rather than on the presence of a list, so a loop that stops
    // stops spending the byte share on the same frame it stops.
    build_loop(&mut app, (ts(0), ts(1)));
    {
        let ls = app
            .gui
            .pane_mut(0)
            .expect("pane 0")
            .time_state_mut(&known::MODEL_DATA);
        ls.phase = rustdar_egui::pane::LoopPhase::Inactive;
        assert!(
            !ls.frames.is_empty(),
            "premise: the frames must still be standing, or an inactive \
             timeline and an empty one are indistinguishable here",
        );
    }
    asked.lock().expect("no poisoned lock").prepared.clear();
    app.dispatch_overlay_loop_renders();
    assert!(
        asked.lock().expect("no poisoned lock").prepared.is_empty(),
        "a switched-off timeline was rendered into. Its frames still hold the \
         bytes; asking for more is a loop nobody is watching taking the share \
         from one somebody is.",
    );
}
