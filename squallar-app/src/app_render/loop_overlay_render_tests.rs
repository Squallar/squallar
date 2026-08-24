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
//! `squallar-overlays` by
//! `a_named_frame_is_rasterized_from_that_frames_grid_and_not_the_panes`.
//!
//! Each claim names the mutation that turns it red; all five were applied and
//! observed.

use super::*;
use squallar_geo::GeoBounds;
use squallar_source::handler::{FetchPayload, PaneRef};
use squallar_source::id::{LayerId, known};
use squallar_source::job::DescribedJob;
use squallar_source::time::{FrameListing, FrameSource, FrameStamp};
use std::sync::{Arc, Mutex};

use squallar_overlays::render::overlay_state::{
    FetchConfig, FetchTask, OverlayHandler, OverlayRegistry, RenderMode, SourceEvent, Surface,
};

/// The run every stamp in this suite belongs to. Named, because carrying it is
/// half of what the dispatch is for: two model runs both publish a frame valid
/// at the same instant, and a `LoopFrame` holds only the instant.
pub(super) fn run() -> chrono::NaiveDateTime {
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
pub(super) struct Asked {
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

impl FrameSource for FrameLayer {
    /// The newest listed stamp at or before `t`, carrying the run every stamp
    /// this double names — the half WI-5's `fetch_frame` assertion is about.
    fn latest_at(&self, _pane: &PaneRef<'_>, t: chrono::NaiveDateTime) -> Option<FrameStamp> {
        let mut stamps: Vec<FrameStamp> = self
            .listed
            .iter()
            .map(|valid| FrameStamp {
                valid: *valid,
                run: Some(run()),
            })
            .collect();
        stamps.sort_by_key(|stamp| stamp.valid);
        squallar_source::time::newest_at_or_before(&stamps, t)
    }

    /// Residency here is "everything it listed", arranged rather than driven,
    /// so nothing may evict it and no arrival may add to it.
    fn retain_frames(&mut self, _pane: &PaneRef<'_>, _keep: &[FrameStamp]) {}

    fn apply_frame_listing(
        &mut self,
        _listing: FrameListing,
        _scope: FetchPayload,
        _pane: &PaneRef<'_>,
    ) {
    }

    fn apply_frame(&mut self, _stamp: FrameStamp, _data: FetchPayload, _pane: &PaneRef<'_>) {}

    /// **No listing round trip.** This suite arranges its frames directly; a
    /// task here would be a second, competing source of them.
    fn create_frame_list_task(
        &self,
        _ctx: &FetchConfig,
        _pane: &PaneRef<'_>,
        _range: (chrono::NaiveDateTime, chrono::NaiveDateTime),
    ) -> Option<FetchTask> {
        None
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
        _selections: &mut Vec<Arc<dyn squallar_overlays::render::overlay_state::OverlayItem>>,
        _pane: &PaneRef<'_>,
    ) {
    }

    fn time_axis(&self) -> squallar_source::time::TimeAxis {
        squallar_source::time::TimeAxis::FrameSeries {
            typical_step: std::time::Duration::from_secs(3600),
            extends_future: true,
        }
    }
    /// This layer comes in stamped frames, and answers every one of
    /// [`FrameSource`]'s methods below.
    fn frames(&self) -> Option<&dyn FrameSource> {
        Some(self)
    }

    fn frames_mut(&mut self) -> Option<&mut dyn FrameSource> {
        Some(self)
    }

    /// **The instrument.** The dispatch's whole claim is that a loop frame's
    /// raster is described for *that frame*, so the double records the frame it
    /// was described for and hands back a job whose contents nothing reads.
    fn prepare_job(
        &self,
        ctx: &squallar_overlays::render::overlay_state::RasterizeContext,
        _pane: &PaneRef<'_>,
    ) -> Option<DescribedJob> {
        self.asked
            .lock()
            .expect("no poisoned lock")
            .prepared
            .push(ctx.frame);
        Some(DescribedJob::new(
            squallar_overlays::render::rasterize::SitesInput {
                sites: Vec::new(),
                zoom: ctx.zoom,
                is_dark: ctx.is_dark,
                device_scale: ctx.device_scale,
            },
        ))
    }

    fn job_codec(&self) -> Option<&'static squallar_source::job::JobCodec> {
        squallar_overlays::render::jobs::JOB_CODECS
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

impl squallar_worker::offload::JobSink for TakeAll {
    fn send(
        &self,
        _id: u64,
        _request: squallar_worker::offload::JobRequest,
    ) -> Result<(), squallar_worker::offload::JobRequest> {
        *self.taken.lock().expect("no poisoned lock") += 1;
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
        data_generation: 5,
        zoom: 32,
    }
}

/// A one-pane app whose model slot is the double, with `listed` frames on
/// offer.
pub(super) fn app_with_frames(
    listed: Vec<chrono::NaiveDateTime>,
) -> (crate::app::App, Arc<Mutex<Asked>>) {
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
pub(super) fn build_loop(
    app: &mut crate::app::App,
    range: (chrono::NaiveDateTime, chrono::NaiveDateTime),
) {
    let pane = app.gui.pane_mut(0).expect("the fixture built a pane");
    pane.set_transport_layer(known::MODEL_DATA);
    *pane.time_state_mut(&known::MODEL_DATA) = squallar_egui::pane::LayerTimeState::begin(
        (range.1 - range.0).num_seconds() as u64,
        squallar_radar::types::RenderView::PlanView,
        Box::new(()),
    );
    // What the production dispatch records beside the phase: the window the
    // ask covered, which is what the arrival is matched on.
    pane.time_state_mut(&known::MODEL_DATA).asked_range = Some(range);
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
pub(super) fn frame_stamps(app: &crate::app::App) -> Vec<chrono::NaiveDateTime> {
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
                .and_then(squallar_egui::pane::LoopFrameImage::overlay)
                .map(|o| o.texture.id())
        })
        .collect()
}

/// The site radar loops on when this suite makes a pane animate two layers.
fn radar_site() -> squallar_radar::sites::RadarSite {
    squallar_radar::sites::RadarSite {
        name: "KTLX",
        network: squallar_radar::sites::RadarNetwork::of_id("KTLX"),
        lat: 35.33,
        lon: -97.27,
        heights: None,
    }
}

/// Deliver the pane's **live** raster — the one `overlay_frame_bytes` measures
/// — at `side`×`side` texels, so what one loop frame of this layer costs on
/// this pane is a number the test chose rather than one it inherited.
fn deliver_live_raster(app: &mut crate::app::App, ctx: &egui::Context, side: usize) {
    let image = Arc::new(egui::ColorImage::from_rgba_unmultiplied(
        [side, side],
        &vec![7u8; side * side * 4],
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
            frame: None,
        })
        .expect("the receiver lives on the App");
    app.poll_overlay_render_results(ctx);
}

/// Deliver one finished raster for `stamp`, with a picture nothing else shares.
pub(super) fn deliver_raster(
    app: &mut crate::app::App,
    ctx: &egui::Context,
    stamp: FrameStamp,
    shade: u8,
) {
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
    let _guard = squallar_worker::offload::install_test_worker(Box::new(TakeAll {
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
/// WI-5 sampled the frame *list* to `layer_share`'s answer when the listing
/// landed. That figure is not stable: the pane can be resized (an overlay
/// frame's texture is planned off the pane rect, so its cost grows with it),
/// `LoopPool` re-plans the allocation at runtime, and a second layer can start
/// animating and halve the share. So the dispatch re-derives it and evicts
/// what the share no longer covers **before** asking for more.
///
/// The denominators, stated: `overlay_frame_bytes` reads the pane's own live
/// raster and falls back to `LoopFrameModel::overlay` — the class's own
/// overlay frame, 2880x1620x4 = **18.66 MB** on this arm — before one exists.
/// `layer_share` divides the pane's `share_bytes` by that and floors at
/// `MIN_LOOP_FRAMES_PER_PANE`. On a real 1280x960-**point** pane at 1x the
/// figure is 1920x1440x4 = **11.06 MB per frame**, so the 56 MiB wasm pool
/// floor buys **5** such frames for the whole application and two animating
/// layers buy **2** each — the floor, which `layer_share` documents itself as
/// exceeding the byte bound to honour.
///
/// **The arrangement is the one that makes the bound non-vacuous.** WI-5 sizes
/// the frame list from whatever the price is when the listing lands. The moment
/// a real overlay raster lands, `overlay_frame_bytes` reads it instead, and on
/// a large pane it is bigger. The list is then longer than the share pays for,
/// and nothing but this dispatch gives the difference back.
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
    let _guard = squallar_worker::offload::install_test_worker(Box::new(TakeAll {
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
    // `overlay_frame_bytes` falls back to the model's own `overlay` arm.
    let fallback_bytes = LoopFrameModel::from_budgets(&budgets).overlay;
    let held_at_build = layer_share(allocation, None, fallback_bytes, animating);
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
        squallar_device_profile::constants::MAX_OVERLAY_LOOP_RENDERS_PER_PASS,
        "one pass put {built} frames' worth of rasterizes on the funnel at          once, ahead of whatever the user is waiting on",
    );
    assert_eq!(
        *taken.lock().expect("no poisoned lock"),
        squallar_device_profile::constants::MAX_OVERLAY_LOOP_RENDERS_PER_PASS,
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
        .time_state_mut(&known::RADAR) = squallar_egui::pane::LayerTimeState::begin(
        3600,
        squallar_radar::types::RenderView::PlanView,
        Box::new(()),
    );
    let animating_now = app.gui.pane(0).expect("pane 0").animating_layers().count();
    assert_eq!(animating_now, 2, "premise: two layers are animating now");

    let frame_bytes = overlay_frame_bytes(
        app.gui.pane(0).expect("pane 0"),
        &known::MODEL_DATA,
        &budgets,
    );
    let held_now = layer_share(allocation, None, frame_bytes, animating_now);
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
         divided {animating_now} ways — at {frame_bytes} B a frame.",
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

/// **WB-7: a pane animating a 16 MiB radar frame and a 4 MiB overlay frame
/// holds them 1:4, and the two together fit the one share.**
///
/// This is the wiring proof for the byte division, driven through the two real
/// listing arrivals on one real pane rather than through the divider. Before
/// WB-7 there was no arrangement in which the two answers could disagree:
/// `layer_share` divided radar's *count* and a separate `overlay_frames_held`
/// divided the overlay's *bytes*, two authorities that never met, and
/// `LoopFrameModel` had no price for an overlay frame at all.
///
/// **Every figure with its denominator**, desktop, pool at its floor:
///
/// * the pane's share is the whole pool, **576 MiB**, because one pane is
///   looping (`LoopDemand::default()` on a fresh `App`);
/// * a radar plan-view loop frame is `LOOP_IMAGE_SIZE`² × 4 = **16 MiB**;
/// * this pane's model raster is arranged at 1024×1024 texels = **4 MiB**,
///   read back off the pane's own live texture by `overlay_frame_bytes` — a
///   quarter of a radar frame, exactly;
/// * two animating layers, so each takes **288 MiB**;
/// * radar holds **18** frames (288 MiB / 16 MiB, under its 60-frame list cap)
///   and the model holds **72** (288 MiB / 4 MiB);
/// * **72 = 18 × 4**, and 18 × 16 MiB + 72 × 4 MiB = **576 MiB**, the pool
///   floor exactly.
///
/// **Floor — `divide_the_count`: restore `(whole_pane / animating).max(2)`.**
/// Radar's list comes out at 30 (60 / 2) and the model's at 72 (144 / 2), so
/// the 4:1 assertion reds at 2.4:1 — and the two together charge 768 MiB
/// against a 576 MiB floor, so the byte assertion reds too. Applied and
/// observed.
///
/// **Floor — `price_the_overlay_as_radar`: make `overlay_frame_bytes` ignore
/// the pane's measured texture and answer `LoopFrameModel::plan_view`.** The
/// model's list comes out at 18 rather than 72 and both the ratio and the
/// counts-differ assertion red. Applied and observed.
///
/// The counts-differ assertion is there on its own account: it is what an
/// equal split cannot satisfy, so the ratio cannot be met by two numbers that
/// are the same.
#[test]
fn a_pane_animating_two_layers_divides_its_bytes_and_not_its_frame_count() {
    let ctx = egui::Context::default();
    let taken = Arc::new(Mutex::new(0usize));
    let _guard = squallar_worker::offload::install_test_worker(Box::new(TakeAll {
        taken: Arc::clone(&taken),
    }));

    // Hourly, and far more than either share buys.
    let listed: Vec<chrono::NaiveDateTime> = (0..120).map(ts).collect();
    let (mut app, _asked) = app_with_frames(listed.clone());

    // Radar starts animating on the same pane, so both layers are competing
    // for one share when either listing lands.
    *app.gui
        .pane_mut(0)
        .expect("the fixture built a pane")
        .time_state_mut(&known::RADAR) = squallar_egui::radar_layer::begin_loop(
        24 * 3600,
        &radar_site(),
        squallar_radar::types::RenderView::PlanView,
    );

    // The pane's own live model raster, at a size chosen to be exactly a
    // quarter of a radar loop frame. `overlay_frame_bytes` measures this
    // rather than assuming anything, and the pane has to be drawing the layer
    // for a live raster to reach its cache at all.
    app.gui
        .pane_mut(0)
        .expect("pane 0")
        .set_overlay_enabled(known::MODEL_DATA, true);
    app.spawn_overlay_render(vec![0], known::MODEL_DATA, a_render_request(), None);
    deliver_live_raster(&mut app, &ctx, 1024);

    let budgets = app.budgets;
    let allocation = app.loop_allocation();
    let radar_bytes = LoopFrameModel::from_budgets(&budgets).plan_view;
    let overlay_bytes = overlay_frame_bytes(
        app.gui.pane(0).expect("pane 0"),
        &known::MODEL_DATA,
        &budgets,
    );
    assert_eq!(
        overlay_bytes * 4,
        radar_bytes,
        "premise: the pane's measured model frame ({overlay_bytes} B) must be \
         exactly a quarter of a radar loop frame ({radar_bytes} B), or the \
         ratio below is not the one this test claims to read",
    );
    // The model's frame list, through the production arrival. `build_loop`
    // arms the model timeline before it sends the listing, so both layers are
    // animating by the time the arrival divides the share.
    build_loop(&mut app, (listed[0], listed[listed.len() - 1]));
    let overlay_held = frame_stamps(&app).len();
    assert_eq!(
        app.gui.pane(0).expect("pane 0").animating_layers().count(),
        2,
        "premise: two animating layers, or there was no division to read",
    );

    // Radar's, through `accept_scan_listing` — the same function the
    // production caller runs, under its test-facing name.
    let scans: Vec<chrono::NaiveDateTime> = (0..200)
        .map(|i| ts(0) + chrono::Duration::minutes(i * 4))
        .collect();
    let animating = app.gui.pane(0).expect("pane 0").animating_layers().count();
    let pane = app.gui.pane_mut(0).expect("pane 0");
    crate::app::render::accept_scan_listing_for_test(
        allocation,
        &budgets,
        pane.time_state_mut(&known::RADAR),
        "KTLX",
        scans.clone(),
        animating,
    );
    let radar_held = pane.time_state(&known::RADAR).frames.len();

    assert!(
        radar_held < scans.len() && overlay_held < listed.len(),
        "premise: both listings must be longer than the share buys, or \
         neither list is telling us what the divider said — radar \
         {radar_held} of {} and model {overlay_held} of {}",
        scans.len(),
        listed.len(),
    );
    assert_ne!(
        radar_held, overlay_held,
        "the two layers came out at the same frame count, so the pane split \
         its allowance by count and not by bytes — {radar_held} frames at \
         {radar_bytes} B beside {overlay_held} at {overlay_bytes} B",
    );
    assert_eq!(
        overlay_held,
        radar_held * 4,
        "a layer whose frames cost a quarter as much must hold four times as \
         many: radar {radar_held} × {radar_bytes} B against model \
         {overlay_held} × {overlay_bytes} B, out of {} B of share divided \
         {animating} ways",
        allocation.share_bytes,
    );
    let spent = radar_held * radar_bytes + overlay_held * overlay_bytes;
    assert!(
        spent <= squallar_device_profile::constants::LOOP_POOL_FLOOR_BYTES,
        "the pane's two loops charge {spent} B against a pool floor of {} B — \
         the whole point of dividing is that the two together fit",
        squallar_device_profile::constants::LOOP_POOL_FLOOR_BYTES,
    );
    assert!(
        radar_held >= squallar_device_profile::constants::MIN_LOOP_FRAMES_PER_PANE
            && overlay_held >= squallar_device_profile::constants::MIN_LOOP_FRAMES_PER_PANE,
        "the byte bound starved a layer below the floor, and a layer that \
         cannot hold two frames cannot animate at all",
    );
}

/// **WB-7: a pane looping a layer that is not radar is a share of the pool.**
///
/// `loop_demand` reads each pane's *radar* timeline, so before WB-7 a radar-off
/// pane running a forecast or a satellite loop counted for nothing — and
/// `LoopPool::plan` divides by `shares().max(1)`, so a pool nobody claimed was
/// handed **whole** to every pane that had not claimed it. Two such panes each
/// believed they had the entire application's loop bytes.
///
/// **Floor — `forget_the_overlay_pane`: delete the `add_overlay_pane` branch.**
/// `shares()` goes to 0 and both assertions red. Applied and observed.
#[test]
fn a_radar_off_pane_looping_a_model_layer_is_a_share_of_the_pool() {
    let taken = Arc::new(Mutex::new(0usize));
    let _guard = squallar_worker::offload::install_test_worker(Box::new(TakeAll {
        taken: Arc::clone(&taken),
    }));

    let (mut app, _asked) = app_with_frames(vec![ts(0), ts(1), ts(2)]);
    build_loop(&mut app, (ts(0), ts(2)));

    let pane = app.gui.pane(0).expect("pane 0");
    assert!(
        !pane.time_state(&known::RADAR).is_active(),
        "premise: radar is NOT looping on this pane — that is the whole case",
    );
    assert_eq!(
        pane.animating_layers().count(),
        1,
        "premise: exactly one layer, and it is not radar's",
    );

    let demand = app.loop_demand();
    assert_eq!(
        demand.overlay_loops, 1,
        "the pane's model loop did not reach the pool's demand at all",
    );
    assert_eq!(
        demand.shares(),
        1,
        "a pane looping a model field is one way the pool is split; at 0 the \
         `shares().max(1)` in `LoopPool::plan` hands it the whole pool and a \
         second such pane gets the whole pool again",
    );

    // The byte consequence, on the pool's own arithmetic: a second such pane
    // halves what each of them may hold.
    let budgets = app.budgets;
    let model = LoopFrameModel::from_budgets(&budgets);
    let pool = crate::loop_pool::LoopPool::new(
        budgets.loop_pool_floor_bytes,
        crate::loop_pool::LoopPoolLimits::from_budgets(&budgets),
    );
    let two = crate::loop_pool::LoopDemand {
        overlay_loops: 2,
        ..Default::default()
    };
    assert_eq!(
        pool.plan(model, two).share_bytes * 2,
        pool.plan(model, demand).share_bytes,
        "two panes looping a model field must divide the pool between them",
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
    let _guard = squallar_worker::offload::install_test_worker(Box::new(TakeAll {
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
            .renders
            .holds(squallar_egui::overlay_cache::RenderSlot::WHOLE),
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
        ls.phase = squallar_egui::pane::LoopPhase::Inactive;
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
