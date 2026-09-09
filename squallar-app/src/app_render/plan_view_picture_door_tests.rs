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
/// The panes are given a picture each *first*, because a pane's first picture
/// is SHOWN and not held — `PaneState::place_radar_raster` takes the `show`
/// arm when `current()` is none, so there is nothing on the glass to keep. It
/// is the second picture that is held, and a hold is the queue's own window:
/// placed by `apply_render_to_pane`, taken by `promote_held_raster` on the
/// frame the renderer says every band has landed.
#[test]
fn pictures_the_pipe_is_still_holding_close_the_door_with_no_render_in_flight() {
    let (recorded, _worker) = recorder();
    let ctx = egui::Context::default();
    let mut app = app_on_distinct_sites(SITES.len());

    // A first picture on each of `CEILING` panes, shown; then a second, held.
    // Nothing is dispatched, so the renders-in-flight term stays zero and the
    // holding term is the whole of the total.
    for idx in 0..CEILING {
        let mut uploads = PlanViewUploads::default();
        app.apply_render_to_pane(&ctx, idx, &finished(raster()), &mut uploads);
        let mut uploads = PlanViewUploads::default();
        app.apply_render_to_pane(&ctx, idx, &finished(other_raster()), &mut uploads);
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
        app.apply_render_to_pane(&ctx, idx, &finished(raster()), &mut uploads);
        let mut uploads = PlanViewUploads::default();
        app.apply_render_to_pane(&ctx, idx, &finished(other_raster()), &mut uploads);
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
/// this door — the mistake `MAX_OVERLAY_PICTURES_OUTSTANDING` records for the
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
