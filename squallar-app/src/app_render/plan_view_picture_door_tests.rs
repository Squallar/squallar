//! **The batch of first paints a resume asks for, against the pipe that
//! carries it.**
//!
//! `squallar_gpu`'s band queue holds every whole picture it is filed until
//! that picture's last band crosses, and it works through them strictly
//! oldest-first, one at a time. Nothing counted them: `render_slot_free`
//! bounds the renders *in flight*, and a picture that has arrived and is
//! sitting in the queue is not one. So the outstanding total was
//! `concurrent_renders + (pictures the queue has not finished)`, whose second
//! term had no ceiling — and on a resume that is the whole batch, "six panes
//! at a time" of a raster the desktop bracket takes at 8192 px.
//!
//! What is asserted here is the door's own contract, one conjunct at a time:
//! the batch is bounded, a refusal writes nothing and is re-asked, each of the
//! two terms closes the door **on its own**, and the three things the door
//! deliberately does not charge stay uncharged.

use super::*;
use crate::app::tests::n_pane_app;
use squallar_device_profile::constants::MAX_PLAN_VIEW_PICTURES_OUTSTANDING as CEILING;
use squallar_radar::types::RadarProduct;
use squallar_source::id::known;

use super::one_render_per_sweep_tests::{asked_for, recorder, sample_scan};

const TILT: f32 = 0.5;
const PRODUCT: RadarProduct = RadarProduct::Reflectivity;

/// Distinct real sites, so every pane asks for a picture of its own and the
/// shared-cache and sibling-in-flight arms above the door never fire. More of
/// them than the ceiling, or the assertion below is about a batch the door
/// could not have shortened.
const SITES: [&str; 6] = ["KTLX", "KPBZ", "KINX", "KDDC", "KABR", "KMPX"];

fn volume_time() -> chrono::NaiveDateTime {
    chrono::NaiveDate::from_ymd_opt(2026, 9, 8)
        .unwrap()
        .and_hms_opt(11, 4, 0)
        .unwrap()
}

/// Aim pane `idx` at `site`, and put a volume under it so the dispatch reaches
/// a real `spawn_level2_render` rather than falling out for want of data.
fn point_at(app: &mut crate::app::App, idx: usize, site: &str) {
    let radar = squallar_radar::sites::get_radar_site(site)
        .unwrap_or_else(|| panic!("{site} is in the site table"))
        .clone();
    let mut product_elevations = std::collections::HashMap::new();
    product_elevations.insert(PRODUCT, vec![TILT]);
    {
        let pane = app.gui.pane_mut(idx).expect("pane exists");
        pane.set_site(site.to_string());
        pane.set_selected_product(squallar_radar::fields::spec(PRODUCT).id.clone());
        pane.set_selected_elevation(TILT);
    }
    app.gui
        .apply(squallar_egui::shell_api::GuiEvent::ScanInfoForPane {
            pane_idx: idx,
            info: squallar_radar::types::ScanInfo {
                site: radar,
                site_source: squallar_radar::site_position::SitePositionSource::Table,
                site_position: None,
                timestamp: volume_time(),
                vcp_number: 212,
                available_products: vec![PRODUCT],
                product_elevations,
                status: String::new(),
            },
        });
    drop(app.volumes.install_still(
        site.to_string(),
        volume_time(),
        (
            sample_scan(),
            Arc::new(squallar_radar::nyquist::DeclaredNyquist::empty()),
        ),
    ));
}

/// `n` panes, each on a site of its own — the resume this door exists for.
fn app_on_distinct_sites(n: usize) -> crate::app::App {
    assert!(n <= SITES.len(), "the fixture has {} sites", SITES.len());
    let mut app = n_pane_app(n, SITES[0]);
    for (idx, site) in SITES.iter().enumerate().take(n) {
        point_at(&mut app, idx, site);
    }
    app
}

/// A picture, small: what is being counted is one picture per outstanding
/// raster whatever its size, so the fixture pays for none of the real one.
fn raster() -> Arc<egui::ColorImage> {
    Arc::new(egui::ColorImage::from_rgba_unmultiplied(
        [4, 4],
        &[7u8, 9, 11, 255].repeat(16),
    ))
}

/// A second picture, a different allocation from [`raster`], so a pane placed
/// both of them is REPLACING rather than being re-described: the `Weak` in
/// `PaneRenderState::uploaded_from` compares allocations, not pixels.
fn other_raster() -> Arc<egui::ColorImage> {
    Arc::new(egui::ColorImage::from_rgba_unmultiplied(
        [4, 4],
        &[13u8, 17, 19, 255].repeat(16),
    ))
}

/// The picture wrapped as the apply path takes it.
fn finished(image: Arc<egui::ColorImage>) -> crate::render_dispatch::CachedPaneRender {
    crate::render_dispatch::CachedPaneRender {
        surface: crate::channels::StillSurface::Raster(image),
        max_range_km: 230.0,
        hover: Arc::new(squallar_radar::hover::HoverSource::empty()),
        product: PRODUCT,
        elevation: TILT,
        nyquist_ms: None,
        melting_layer_source: None,
        storm_motion: None,
    }
}

/// Answer pane `idx`'s render on the channel the worker's reply arrives on,
/// so the picture crosses into the pane's hold exactly as production's does.
fn answer(app: &mut crate::app::App, ctx: &egui::Context, idx: usize) {
    app.channels
        .render_sender
        .send(crate::channels::RenderResponse {
            rendered: Some(crate::channels::RenderedImage {
                surface: crate::channels::StillSurface::Raster(raster()),
                max_range_km: 230.0,
                hover: Arc::new(squallar_radar::hover::HoverSource::empty()),
                nyquist_ms: None,
                melting_layer_source: None,
                storm_motion: None,
            }),
            product: PRODUCT,
            elevation: TILT,
            generation: app.render.render_generation,
            pane_idx: idx,
            speculative_for: None,
        })
        .expect("the receiver lives on the App");
    app.poll_render_results(ctx);
}

/// **The batch is bounded, and the batch is what a resume asks for.**
///
/// Six panes, six sites, one frame. Without the door every one of them posts a
/// whole picture and the queue holds all six; with it the frame asks for what
/// the pipe can carry and the rest are asked for again on later frames.
#[test]
fn a_resume_asks_for_no_more_pictures_than_the_pipe_can_hold() {
    let (recorded, _worker) = recorder();
    let ctx = egui::Context::default();
    let mut app = app_on_distinct_sites(SITES.len());

    app.dispatch_pane_renders(&ctx);

    let posted = asked_for(&recorded);
    assert!(
        SITES.len() > CEILING,
        "the fixture must ask for more than the ceiling or this proves nothing",
    );
    assert!(
        posted.len() >= 2,
        "the frame admitted {} whole picture(s). The band queue drains one \
         picture at a time and strictly oldest-first, so a door that admits one \
         leaves nothing queued behind the picture being moved: the drain idles \
         at every handover and every pane past the first paints LATER than it \
         does with no door at all. Bounding the burst may not cost a paint.",
        posted.len(),
    );
    assert_eq!(
        posted.len(),
        CEILING,
        "{} panes on {} distinct sites posted {} whole-picture renders in one \
         frame against a ceiling of {CEILING}; every one past the ceiling is a \
         whole raster the band queue cannot start any sooner and holds resident \
         until it does",
        SITES.len(),
        SITES.len(),
        posted.len(),
    );
}

/// **A refusal writes nothing**, so the pane asks again — and is served.
///
/// The retry is the whole of what makes the door safe: nothing latches, no
/// raster is discarded, and `needs_render` is still true because
/// `last_rendered` was never set. Draining one picture out of the pipe is what
/// lets the next in.
#[test]
fn a_refused_pane_asks_again_on_a_later_frame_and_is_served() {
    let (recorded, _worker) = recorder();
    let ctx = egui::Context::default();
    let mut app = app_on_distinct_sites(SITES.len());

    app.dispatch_pane_renders(&ctx);
    let first = asked_for(&recorded).len();

    for idx in 0..SITES.len() {
        let asked = app.render.pane_render[idx].holds_plan_view_render();
        assert_eq!(
            app.render.pane_render[idx].last_rendered, None,
            "pane {idx} was recorded as rendered before any raster arrived",
        );
        if !asked {
            assert!(
                app.gui.pane(idx).expect("pane exists").scan_info.is_some(),
                "pane {idx} was refused and also lost the scan it was aimed at; \
                 a refusal must write nothing",
            );
        }
    }

    // One picture leaves the pipe: its render answers, and its pane shows it
    // rather than holding it, because the fixture has no renderer to band it.
    answer(&mut app, &ctx, 0);
    app.gui.promote_held_rasters(|_| true);
    assert!(
        !app.gui
            .pane(0)
            .expect("pane exists")
            .is_holding_plan_view_raster(),
        "precondition: pane 0's picture must have left the pipe, or the door \
         below is being asked the same question as before",
    );

    app.dispatch_pane_renders(&ctx);

    assert!(
        asked_for(&recorded).len() > first,
        "the frame after a picture left the pipe asked for nothing new: {first} \
         renders posted before and {} after, so a refused pane is never re-asked \
         and its pane never paints",
        asked_for(&recorded).len(),
    );
}

/// **The holding term closes the door on its own.**
///
/// Asserted against what the term itself needs, not against the pair: with
/// **zero** renders in flight, `CEILING` pictures sitting in the pipe must be
/// enough to refuse. A door that only ever closed on the renders would be
/// green on the batch above and blind to a queue that has stopped draining
/// while every render has already answered.
///
/// The panes are given a picture each *first* and it is then **settled**,
/// because a pane's first picture is SHOWN and not held —
/// `PaneState::place_radar_raster` takes the `show` arm when `current()` is
/// none, so there is nothing on the glass to keep. That picture is charged
/// too (see `a_first_picture_shown_rather_than_held_closes_the_door`), so
/// settling it is what leaves the holds as the whole of the total and makes
/// this test about the term it names. A hold is the queue's own window:
/// placed by `apply_render_to_pane`, taken by `promote_held_raster` on the
/// frame the renderer says every band has landed.
#[test]
fn pictures_the_pipe_is_still_holding_close_the_door_with_no_render_in_flight() {
    let (recorded, _worker) = recorder();
    let ctx = egui::Context::default();
    let mut app = app_on_distinct_sites(SITES.len());

    // A first picture on each of `CEILING` panes, shown and then delivered;
    // then a second, held. Nothing is dispatched, so the renders-in-flight
    // term stays zero and the holding term is the whole of the total.
    for idx in 0..CEILING {
        let mut uploads = PlanViewUploads::default();
        let _ = app.apply_render_to_pane(&ctx, idx, &finished(raster()), &mut uploads);
    }
    app.gui.promote_held_rasters(|_| true);
    assert_eq!(
        app.gui.plan_view_pictures_outstanding(),
        0,
        "precondition: the shown first pictures must have left the pipe, or \
         the total below is not the holds",
    );
    for idx in 0..CEILING {
        let mut uploads = PlanViewUploads::default();
        let _ = app.apply_render_to_pane(&ctx, idx, &finished(other_raster()), &mut uploads);
        assert!(
            app.gui
                .pane(idx)
                .expect("pane exists")
                .is_holding_plan_view_raster(),
            "precondition: pane {idx}'s second picture must be HELD, or this \
             test is reading the shown arm it claims to have settled",
        );
    }
    assert_eq!(
        app.render.plan_view_renders_in_flight(),
        0,
        "precondition: no render may be in flight, or this test is reading the \
         term it claims to have removed",
    );
    assert_eq!(
        app.gui.plan_view_pictures_outstanding(),
        CEILING,
        "precondition: the pipe must be holding exactly the ceiling",
    );
    let before = asked_for(&recorded).len();

    app.dispatch_pane_renders(&ctx);

    assert_eq!(
        asked_for(&recorded).len(),
        before,
        "a frame with {CEILING} pictures in the pipe and no render in flight \
         asked for {} more; the holding term does not close the door by itself, \
         so a queue that has stopped draining is unbounded",
        asked_for(&recorded).len() - before,
    );
}

/// **A pane that has let its picture go is not charged for it.**
///
/// The falling half of the same term, and the reason the count is a sweep of
/// the panes rather than a level someone remembers to pay: a door whose total
/// only ever rises latches shut and every pane stops re-rendering for the life
/// of the session.
#[test]
fn a_picture_that_left_the_pipe_stops_being_charged() {
    let ctx = egui::Context::default();
    let mut app = app_on_distinct_sites(SITES.len());

    for idx in 0..CEILING {
        let mut uploads = PlanViewUploads::default();
        let _ = app.apply_render_to_pane(&ctx, idx, &finished(raster()), &mut uploads);
    }
    app.gui.promote_held_rasters(|_| true);
    for idx in 0..CEILING {
        let mut uploads = PlanViewUploads::default();
        let _ = app.apply_render_to_pane(&ctx, idx, &finished(other_raster()), &mut uploads);
    }
    assert_eq!(
        app.gui.plan_view_pictures_outstanding(),
        CEILING,
        "precondition: the pipe must be holding exactly the ceiling",
    );
    assert!(
        !app.render
            .plan_view_picture_slot_free(app.gui.plan_view_pictures_outstanding()),
        "precondition: the door must be shut before the release below",
    );

    app.gui.promote_held_rasters(|_| true);

    assert_eq!(
        app.gui.plan_view_pictures_outstanding(),
        0,
        "every band landed and the panes still read as holding: the door's \
         total never falls, so one batch of held pictures shuts the plan-view \
         dispatch for the life of the session",
    );
    assert!(
        app.render
            .plan_view_picture_slot_free(app.gui.plan_view_pictures_outstanding()),
        "the door stayed shut after every picture left the pipe",
    );
}

/// **A cross-section cut is not a plan-view picture**, so it does not close
/// this door — the mistake `MAX_OVERLAY_PICTURE_BYTES_OUTSTANDING` records for the
/// overlay one, where charging a producer the door cannot throttle lets a
/// playing loop close it against everything else.
#[test]
fn a_render_that_is_not_a_plan_view_does_not_close_the_door() {
    let mut app = app_on_distinct_sites(1);
    app.render.pane_render[0].render_started(None);

    assert!(
        app.render.pane_render[0].render_in_flight(),
        "precondition: the pane must have a render in flight, or the \
         distinction below is untested",
    );
    assert_eq!(
        app.render.plan_view_renders_in_flight(),
        0,
        "a section cut was counted against the plan-view door",
    );
    assert!(
        app.render.plan_view_picture_slot_free(0),
        "a section cut spent a plan-view picture slot",
    );
}

/// **A pane served out of the shared `RenderCache` is charged nothing.**
///
/// Its picture is an `Arc` the cache is already holding, so the upload adds a
/// queue entry and no host bytes; refusing it would delay a pane and free
/// nothing. So the arm above the door must still serve with the door shut.
#[test]
fn a_pane_served_from_the_shared_cache_is_not_charged() {
    let (_recorded, _worker) = recorder();
    let ctx = egui::Context::default();
    let mut app = app_on_distinct_sites(SITES.len());

    // Shut the door: `CEILING` renders in flight and nothing answered.
    app.dispatch_pane_renders(&ctx);
    assert_eq!(
        app.render.plan_view_renders_in_flight(),
        CEILING,
        "precondition: the door must be shut for this to be about the door",
    );
    let refused = (0..SITES.len())
        .find(|&idx| !app.render.pane_render[idx].holds_plan_view_render())
        .expect("more panes than the ceiling, so one was refused");

    // And put that pane's own picture in the shared cache, as a sibling's
    // finished render would.
    app.render.cache_render(
        SITES[refused],
        PRODUCT,
        squallar_radar::types::RenderView::PlanView,
        TILT,
        crate::render_dispatch::CachedRenderOutput {
            surface: crate::channels::StillSurface::Raster(raster()),
            max_range_km: 230.0,
            hover: Arc::new(squallar_radar::hover::HoverSource::empty()),
            nyquist_ms: None,
            melting_layer_source: None,
            storm_motion: None,
        },
    );

    app.dispatch_pane_renders(&ctx);

    assert_eq!(
        app.render.pane_render[refused].last_rendered,
        Some((PRODUCT, TILT)),
        "pane {refused} had its picture in the shared cache and the door \
         refused it anyway: that costs the pane a paint and frees nothing, \
         because the `Arc` the cache holds is the one the upload would file",
    );
    assert!(
        app.gui
            .pane(refused)
            .expect("pane exists")
            .overlay_cache(&known::RADAR)
            .is_some_and(|cache| cache.is_holding() || cache.current().is_some()),
        "pane {refused} was recorded as served without a picture reaching it",
    );
}

/// **A pane's FIRST picture closes the door**, and it is shown rather than
/// held.
///
/// The arm the door could not see. `PaneState::place_radar_raster` puts an
/// arriving raster straight on the glass when there is nothing on it to
/// protect — the picture fills top-down as its bands land, which is what a
/// first paint IS — and the door's occupancy term read panes that were
/// *holding*. Every pane's first picture takes this arm, so on a resume, the
/// batch this door exists for, the term was structurally zero for the whole
/// burst and the door reduced to a bound on the renders in flight.
///
/// Asserted against what this term needs on its own: **zero** renders in
/// flight, **nothing held**, and `CEILING` first pictures must be enough to
/// refuse.
#[test]
fn a_first_picture_shown_rather_than_held_closes_the_door() {
    let (recorded, _worker) = recorder();
    let ctx = egui::Context::default();
    let mut app = app_on_distinct_sites(SITES.len());

    for idx in 0..CEILING {
        let mut uploads = PlanViewUploads::default();
        let _ = app.apply_render_to_pane(&ctx, idx, &finished(raster()), &mut uploads);
        assert!(
            !app.gui
                .pane(idx)
                .expect("pane exists")
                .is_holding_plan_view_raster(),
            "precondition: pane {idx}'s first picture must be SHOWN and not \
             held, or this test is reading the arm it claims to be about",
        );
        assert!(
            app.gui
                .pane(idx)
                .expect("pane exists")
                .overlay_cache(&known::RADAR)
                .is_some_and(|cache| cache.current().is_some()),
            "precondition: pane {idx} was recorded as served with nothing on \
             its glass",
        );
    }
    assert_eq!(
        app.render.plan_view_renders_in_flight(),
        0,
        "precondition: no render may be in flight, or this test is reading the \
         other term",
    );
    assert_eq!(
        app.gui.plan_view_pictures_outstanding(),
        CEILING,
        "{CEILING} whole pictures are in the band queue, every one of them on a \
         pane's glass and filling top-down, and the door's occupancy term reads \
         {}: a resume is exactly this shape, so the door sees none of it",
        app.gui.plan_view_pictures_outstanding(),
    );
    let before = asked_for(&recorded).len();

    app.dispatch_pane_renders(&ctx);

    assert_eq!(
        asked_for(&recorded).len(),
        before,
        "a frame with {CEILING} first pictures still crossing to the GPU and no \
         render in flight asked for {} more whole rasters",
        asked_for(&recorded).len() - before,
    );
}

/// **A first picture stops being charged once its bands have landed.**
///
/// The falling half of the arm above, and the reason it is a flag cleared by
/// the delivery sweep rather than a level someone pays: a total that only ever
/// rises latches the door shut for the life of the session, which is the exact
/// failure `a_picture_that_left_the_pipe_stops_being_charged` gates for holds.
#[test]
fn a_first_picture_that_landed_stops_being_charged() {
    let ctx = egui::Context::default();
    let mut app = app_on_distinct_sites(SITES.len());

    for idx in 0..CEILING {
        let mut uploads = PlanViewUploads::default();
        let _ = app.apply_render_to_pane(&ctx, idx, &finished(raster()), &mut uploads);
    }
    assert!(
        !app.render
            .plan_view_picture_slot_free(app.gui.plan_view_pictures_outstanding()),
        "precondition: the door must be shut before the delivery below",
    );

    app.gui.promote_held_rasters(|_| true);

    assert_eq!(
        app.gui.plan_view_pictures_outstanding(),
        0,
        "every band landed and the shown pictures are still charged: the \
         door's total never falls, so one batch of first paints shuts the \
         plan-view dispatch for the life of the session",
    );
    assert!(
        app.render
            .plan_view_picture_slot_free(app.gui.plan_view_pictures_outstanding()),
        "the door stayed shut after every first picture left the pipe",
    );
}

/// **What a walk files, the rest of that walk is charged for.**
///
/// The occupancy is read ONCE at the head of `App::dispatch_pane_renders` and
/// the walk visits up to six panes under it. The shared-cache arm uploads a
/// whole picture and dispatches no render, so it moved neither term of that
/// reading: a pane served from the cache filed a whole picture into the band
/// queue and every pane the walk visited after it was told the queue was as
/// empty as it had been at the head of the frame.
///
/// The pipe is primed to one short of the ceiling and the cache is given
/// exactly one picture, so the walk's own filing is the unit that decides:
/// with it counted the next pane is refused, without it that pane dispatches
/// a whole new raster on top of a full queue.
#[test]
fn a_picture_the_walk_filed_is_charged_to_the_panes_after_it() {
    let (recorded, _worker) = recorder();
    let ctx = egui::Context::default();
    let mut app = app_on_distinct_sites(SITES.len());

    // The pipe, one short of the ceiling: first pictures on the LAST panes, so
    // the walk reaches the cache arm below before it reaches them.
    let primed = CEILING - 1;
    for offset in 0..primed {
        let idx = SITES.len() - 1 - offset;
        let mut uploads = PlanViewUploads::default();
        let _ = app.apply_render_to_pane(&ctx, idx, &finished(raster()), &mut uploads);
    }
    assert_eq!(
        app.gui.plan_view_pictures_outstanding(),
        primed,
        "precondition: the pipe must start one short of the ceiling, or the \
         walk's own filing is not what decides below",
    );

    // One picture in the shared cache, for the FIRST pane the walk visits.
    app.render.cache_render(
        SITES[0],
        PRODUCT,
        squallar_radar::types::RenderView::PlanView,
        TILT,
        crate::render_dispatch::CachedRenderOutput {
            surface: crate::channels::StillSurface::Raster(raster()),
            max_range_km: 230.0,
            hover: Arc::new(squallar_radar::hover::HoverSource::empty()),
            nyquist_ms: None,
            melting_layer_source: None,
            storm_motion: None,
        },
    );

    app.dispatch_pane_renders(&ctx);

    assert_eq!(
        app.render.pane_render[0].last_rendered,
        Some((PRODUCT, TILT)),
        "precondition: pane 0 must have been served from the cache, or the \
         walk filed nothing and this test is about nothing",
    );
    assert_eq!(
        app.gui.plan_view_pictures_outstanding(),
        CEILING,
        "precondition: the cache hit must have taken the pipe to the ceiling",
    );
    assert_eq!(
        asked_for(&recorded).len(),
        0,
        "the walk filed a whole picture out of the shared cache and then \
         dispatched {} more on top of a full queue: what a walk files is \
         invisible to the panes it visits after it, so the arm that most looks \
         like `six at once` on a resume is the arm the door does not charge",
        asked_for(&recorded).len(),
    );
}

/// **A picture the shared cache answers with is the cache's own allocation**,
/// so the upload files a refcount and no host bytes.
///
/// The half of the exemption that is TRUE, proved rather than asserted — the
/// reason that arm is charged going forward and still never refused. A refusal
/// would cost the pane a paint and free nothing, because the buffer the band
/// queue would hold is the one the cache is holding already: `Band` keeps
/// `Arc::clone(image)` and `TextureUploads::publish_pending_level` prices it
/// at `as_raw().len()` whoever else holds it.
#[test]
fn a_cache_hit_files_the_buffer_the_cache_is_already_holding() {
    let ctx = egui::Context::default();
    let mut app = app_on_distinct_sites(1);
    let buffer = raster();

    app.render.cache_render(
        SITES[0],
        PRODUCT,
        squallar_radar::types::RenderView::PlanView,
        TILT,
        crate::render_dispatch::CachedRenderOutput {
            surface: crate::channels::StillSurface::Raster(Arc::clone(&buffer)),
            max_range_km: 230.0,
            hover: Arc::new(squallar_radar::hover::HoverSource::empty()),
            nyquist_ms: None,
            melting_layer_source: None,
            storm_motion: None,
        },
    );

    app.dispatch_pane_renders(&ctx);

    let served = app
        .render
        .get_cached_render(
            SITES[0],
            PRODUCT,
            squallar_radar::types::RenderView::PlanView,
            TILT,
        )
        .expect("the entry the walk was served from is still resident")
        .surface
        .raster()
        .expect("the entry the walk was served from is a raster")
        .clone();
    assert_eq!(
        app.render.pane_render[0].last_rendered,
        Some((PRODUCT, TILT)),
        "precondition: the pane was not served from the cache at all, so \
         nothing below is about the upload a cache hit files",
    );
    assert!(
        Arc::ptr_eq(&served, &buffer),
        "the cache answered with a different allocation from the one it was \
         given, so a cache hit is a second copy of the picture on the host and \
         admitting one past the door is not free",
    );
    // The conjunct that reaches the upload rather than the cache: the buffer
    // `ctx.load_texture` was handed, which is the buffer the band queue now
    // holds an `Arc` to, is the cache's own allocation and not a copy of it.
    assert!(
        app.render.pane_render[0].shows_buffer(&buffer),
        "the cache hit uploaded a buffer that is not the one the cache holds, \
         so the queue entry it filed is 216,796,176 B of host memory that did \
         not exist before and the arm is not free after all",
    );
}

/// **N sibling panes on one picture are ONE charge**, because they are one
/// entry in the band queue.
///
/// The other half of the exemption, and the half that was over-charging rather
/// than under-charging: the door counted PANES HOLDING, and the queue prices
/// distinct images. `poll_render_results` hands every sibling the same
/// `egui::TextureHandle` out of one `PlanViewUploads`, so six panes on one
/// site were six charges against a door bounding a queue that held one
/// 216,796,176 B buffer — shutting the dispatch against traffic that was never
/// in the pipe.
#[test]
fn sibling_panes_sharing_one_picture_are_one_charge() {
    let ctx = egui::Context::default();
    let mut app = n_pane_app(2, SITES[0]);
    point_at(&mut app, 0, SITES[0]);
    point_at(&mut app, 1, SITES[0]);

    answer(&mut app, &ctx, 0);

    let ids: Vec<_> = (0..2)
        .map(|idx| {
            app.gui
                .pane(idx)
                .expect("pane exists")
                .plan_view_pictures_in_pipe()
        })
        .collect();
    assert!(
        ids[0][1].is_some() && ids[0][1] == ids[1][1],
        "precondition: both panes must be showing the SAME arriving texture, \
         or there is no shared picture to charge once ({ids:?})",
    );
    assert_eq!(
        app.gui.plan_view_pictures_outstanding(),
        1,
        "two panes showing one picture were charged {} against a queue holding \
         one buffer",
        app.gui.plan_view_pictures_outstanding(),
    );
}

/// **ONE picture landing reopens the door**, and the queue does not idle
/// waiting for the batch.
///
/// The first-paint half of the whole door, asserted on the arm this cut adds.
/// `MAX_PLAN_VIEW_PICTURES_OUTSTANDING`'s const assert floors the ceiling at
/// two so the queue always has a picture behind the one it is draining; that
/// floor is worth nothing if the total only falls when the LAST picture in the
/// pipe lands, because the drain would then empty and every pane past the
/// first would paint later than it does with no door at all.
///
/// So: fill the pipe with first pictures, deliver exactly one of them, and the
/// next pane must be served on that frame.
#[test]
fn one_picture_landing_reopens_the_door_for_the_next_pane() {
    let (recorded, _worker) = recorder();
    let ctx = egui::Context::default();
    let mut app = app_on_distinct_sites(SITES.len());

    let mut first_ids = Vec::new();
    for idx in 0..CEILING {
        let mut uploads = PlanViewUploads::default();
        let _ = app.apply_render_to_pane(&ctx, idx, &finished(raster()), &mut uploads);
        first_ids.push(
            app.gui
                .pane(idx)
                .expect("pane exists")
                .plan_view_pictures_in_pipe()[1]
                .expect("the picture is on the glass and still arriving"),
        );
    }
    app.dispatch_pane_renders(&ctx);
    let while_full = asked_for(&recorded).len();
    assert_eq!(
        while_full, 0,
        "precondition: the door must be shut with the pipe full, or its \
         reopening below proves nothing",
    );

    // Exactly one picture's bands land. The other `CEILING - 1` are still in
    // the queue, which is what stops the drain idling at the handover.
    let landed = first_ids[0];
    app.gui.promote_held_rasters(|id| id == landed);

    assert_eq!(
        app.gui.plan_view_pictures_outstanding(),
        CEILING - 1,
        "one picture landed and the pipe reads {}: the total must fall by the \
         picture that left and no more, or the door is either latched or \
         opened for traffic still crossing",
        app.gui.plan_view_pictures_outstanding(),
    );

    app.dispatch_pane_renders(&ctx);

    assert!(
        asked_for(&recorded).len() > while_full,
        "the frame after one picture landed asked for nothing: the door \
         reopens only when the whole batch has drained, so the band queue \
         idles at every handover and every pane past the first paints LATER \
         than it does with no door at all",
    );
}
