use super::*;
use crate::app::tests::{empty_scan, headless, two_pane_app};
use crate::platform_double::TestBridge;
use squallar_egui::pane::{LayerTimeState, LoopFrame, LoopPhase};
use squallar_radar::loop_downloads::LoopDownloadManager;
use squallar_radar::sites::RadarSite;
use squallar_radar::types::RadarProduct;
use squallar_source::id::known;

const SITE: &str = "KTLX";
const PRODUCT: RadarProduct = RadarProduct::Reflectivity;
/// The same field named the way a pane and a render key name it.
const PRODUCT_ID: squallar_source::product::FieldId = squallar_radar::fields::known::REFLECTIVITY;
const TILT: f32 = 0.5;

fn volume_time() -> chrono::NaiveDateTime {
    chrono::NaiveDate::from_ymd_opt(2026, 7, 29)
        .unwrap()
        .and_hms_opt(18, 30, 0)
        .unwrap()
}

fn app_on_site() -> crate::app::App {
    let mut app = headless(TestBridge::desktop());
    point_at_site(&mut app, 0);
    app.render.ensure_pane_count(1);
    app
}

fn point_at_site(app: &mut crate::app::App, pane_idx: usize) {
    let site = squallar_radar::sites::get_radar_site(SITE)
        .expect("KTLX is a real radar")
        .clone();
    let mut product_elevations = std::collections::HashMap::new();
    product_elevations.insert(PRODUCT, vec![TILT]);
    let pane = app.gui.pane_mut(pane_idx).expect("pane exists");
    pane.set_site(SITE.to_string());
    pane.set_selected_product(PRODUCT_ID);
    pane.set_selected_elevation(TILT);
    app.gui
        .apply(squallar_egui::shell_api::GuiEvent::ScanInfoForPane {
            pane_idx,
            info: squallar_radar::types::ScanInfo {
                site,
                site_source: squallar_radar::site_position::SitePositionSource::Table,
                site_position: None,
                timestamp: volume_time(),
                vcp_number: 212,
                available_products: vec![PRODUCT],
                product_elevations,
                status: String::new(),
            },
        });
}

fn section_line() -> squallar_egui::pane::SectionLine {
    squallar_egui::pane::SectionLine::new(
        squallar_geo::GeoPoint {
            lat: 35.0,
            lon: -98.0,
        },
        squallar_geo::GeoPoint {
            lat: 36.0,
            lon: -97.0,
        },
    )
    .expect("two distinct points on Earth")
}

fn finished_pixels() -> Arc<egui::ColorImage> {
    let side = squallar_radar::types::IMAGE_SIZE;
    Arc::new(egui::ColorImage::from_rgba_unmultiplied(
        [side, side],
        &vec![0u8; side * side * 4],
    ))
}

fn loop_frame_pixels() -> egui::ColorImage {
    let side = squallar_device_profile::constants::LOOP_IMAGE_SIZE;
    egui::ColorImage::from_rgba_unmultiplied([side, side], &vec![0u8; side * side * 4])
}

fn cached_output() -> crate::render_dispatch::CachedRenderOutput {
    crate::render_dispatch::CachedRenderOutput {
        image: finished_pixels(),
        max_range_km: 230.0,
        hover: Arc::new(squallar_radar::hover::HoverSource::empty()),
        nyquist_ms: None,
        melting_layer_source: None,
        storm_motion: None,
    }
}

fn holds_radar_texture(app: &mut crate::app::App, pane_idx: usize) -> bool {
    app.gui
        .pane_mut(pane_idx)
        .expect("pane exists")
        .overlay_cache_mut(&known::RADAR)
        .current()
        .is_some()
}

fn deliver(app: &mut crate::app::App, pane_idx: usize) {
    app.channels
        .render_sender
        .send(crate::channels::RenderResponse {
            rendered: Some(crate::channels::RenderedImage {
                image: finished_pixels(),
                max_range_km: 230.0,
                hover: Arc::new(squallar_radar::hover::HoverSource::empty()),
                nyquist_ms: None,
                melting_layer_source: None,
                storm_motion: None,
            }),
            product: PRODUCT,
            elevation: TILT,
            generation: app.render.render_generation,
            pane_idx,
            speculative_for: None,
        })
        .expect("the receiver lives on the App");
    app.poll_render_results(&egui::Context::default());
}

#[test]
fn the_dispatcher_skips_a_pane_with_no_plan_view() {
    for kind in [
        squallar_radar::types::RenderView::CrossSection,
        squallar_radar::types::RenderView::Volume,
    ] {
        let mut app = app_on_site();
        app.render.cache_render(
            SITE,
            PRODUCT,
            squallar_radar::types::RenderView::PlanView,
            TILT,
            cached_output(),
        );

        app.dispatch_pane_renders(&egui::Context::default());
        assert!(
            holds_radar_texture(&mut app, 0),
            "precondition: a map pane must take the cached render, or the \
                 assertion below is about a path nothing reaches"
        );
        assert_eq!(
            app.render.pane_render[0].last_rendered,
            Some((PRODUCT, TILT)),
            "precondition: the map pane's dispatch must have been recorded"
        );

        let mut app = app_on_site();
        app.render.cache_render(
            SITE,
            PRODUCT,
            squallar_radar::types::RenderView::PlanView,
            TILT,
            cached_output(),
        );
        app.gui.pane_mut(0).unwrap().set_view(kind);

        app.dispatch_pane_renders(&egui::Context::default());

        assert!(
            !holds_radar_texture(&mut app, 0),
            "{kind:?}: a full-size plan-view image was uploaded to a pane \
                 that draws none"
        );
        assert_eq!(
            app.render.pane_render[0].last_rendered, None,
            "{kind:?}: the dispatcher recorded a render for a pane it must \
                 not have served"
        );
    }
}

#[test]
fn the_sibling_broadcast_skips_a_pane_with_no_plan_view() {
    for kind in [
        squallar_radar::types::RenderView::CrossSection,
        squallar_radar::types::RenderView::Volume,
    ] {
        let mut app = two_pane_app(SITE, SITE);
        point_at_site(&mut app, 0);
        point_at_site(&mut app, 1);

        deliver(&mut app, 0);
        assert!(
            holds_radar_texture(&mut app, 1),
            "precondition: a map sibling on the same site, product and tilt \
                 must take the broadcast, or nothing below is being filtered"
        );

        let mut app = two_pane_app(SITE, SITE);
        point_at_site(&mut app, 0);
        point_at_site(&mut app, 1);
        app.gui.pane_mut(1).unwrap().set_view(kind);

        deliver(&mut app, 0);

        assert!(
            holds_radar_texture(&mut app, 0),
            "{kind:?}: precondition: the origin pane is still a map and must \
                 have been served"
        );
        assert!(
            !holds_radar_texture(&mut app, 1),
            "{kind:?}: the broadcast handed a plan-view raster to a pane that \
                 draws none"
        );
    }
}

#[test]
fn a_render_in_flight_across_a_conversion_is_not_placed() {
    let mut app = app_on_site();
    app.render.pane_render[0].render_started(None);
    app.gui
        .pane_mut(0)
        .unwrap()
        .set_view(squallar_radar::types::RenderView::Volume);

    deliver(&mut app, 0);

    assert!(!holds_radar_texture(&mut app, 0));
    assert!(
        !app.render.pane_render[0].render_in_flight(),
        "the in-flight flag was not cleared, so this pane could never ask \
             for another render as long as it lived"
    );
    assert_eq!(app.render.pane_render[0].last_rendered, None);
}

fn active_loop(timestamps: &[chrono::NaiveDateTime]) -> LayerTimeState {
    let mut ls = squallar_egui::radar_layer::begin_loop(
        3600,
        &RadarSite {
            name: SITE,
            network: squallar_radar::sites::RadarNetwork::of_id(SITE),
            lat: 35.33,
            lon: -97.27,
            heights: None,
        },
        squallar_radar::types::RenderView::PlanView,
    );
    ls.phase = LoopPhase::Rendering;
    ls.frames = timestamps
        .iter()
        .map(|&timestamp| LoopFrame {
            timestamp,
            image: None,
            render_in_flight: false,
            render_failed: false,
        })
        .collect();
    ls.retarget_renders(&PRODUCT_ID, TILT);
    assert!(
        ls.rendered_for.is_some(),
        "precondition: a fresh loop must take its first target"
    );
    ls
}

#[test]
fn the_first_loop_dispatch_pass_skips_only_the_panes_that_cannot_loop() {
    let moved_to = RadarProduct::Velocity;
    assert!(
        !moved_to.is_level3() && !PRODUCT.is_level3(),
        "precondition: both products must be Level II, or the replan the \
             retarget triggers starts a download this test does not serve"
    );

    for (label, kind, aimed, expected) in [
        (
            "map",
            squallar_radar::types::RenderView::PlanView,
            false,
            Some((moved_to, 0.0)),
        ),
        (
            "aimed section",
            squallar_radar::types::RenderView::CrossSection,
            true,
            Some((moved_to, 0.0)),
        ),
        (
            "unaimed section",
            squallar_radar::types::RenderView::CrossSection,
            false,
            Some((PRODUCT, TILT)),
        ),
        (
            "volume",
            squallar_radar::types::RenderView::Volume,
            false,
            Some((moved_to, 0.0)),
        ),
    ] {
        let mut app = app_on_site();
        {
            let pane = app.gui.pane_mut(0).unwrap();
            pane.set_view(kind);
            if aimed {
                pane.cross_section_mut()
                    .expect("only a section pane is aimed")
                    .line = Some(section_line());
            }
            *pane.time_state_mut(&known::RADAR) = active_loop(&[volume_time()]);
            pane.time_state_mut(&known::RADAR).view = kind;
            pane.set_selected_product(squallar_radar::fields::spec(moved_to).id.clone());
            pane.set_selected_elevation(0.0);
        }

        app.dispatch_loop_renders();

        let keyed = app
            .gui
            .pane(0)
            .unwrap()
            .time_state(&known::RADAR)
            .rendered_for
            .as_ref()
            .and_then(|target| {
                Some((
                    squallar_radar::fields::product_for(&target.product)?,
                    target.elevation,
                ))
            });
        assert_eq!(
            keyed, expected,
            "{label}: the loop's render target moved for a pane whose frames \
                 nobody draws — or failed to move for one whose frames are drawn"
        );
    }
}

#[test]
fn the_second_loop_dispatch_pass_judges_every_pane_that_can_loop() {
    for (label, kind, aimed, expected_failed) in [
        (
            "map",
            squallar_radar::types::RenderView::PlanView,
            false,
            true,
        ),
        (
            "aimed section",
            squallar_radar::types::RenderView::CrossSection,
            true,
            true,
        ),
        (
            "unaimed section",
            squallar_radar::types::RenderView::CrossSection,
            false,
            false,
        ),
        (
            "volume",
            squallar_radar::types::RenderView::Volume,
            false,
            true,
        ),
    ] {
        let mut app = app_on_site();
        app.loop_mgr = LoopDownloadManager::new();
        app.loop_mgr.cache_scan(
            SITE,
            volume_time(),
            (Arc::new(empty_scan()), Default::default()),
        );
        {
            let pane = app.gui.pane_mut(0).unwrap();
            pane.set_view(kind);
            if aimed {
                pane.cross_section_mut()
                    .expect("only a section pane is aimed")
                    .line = Some(section_line());
            }
            *pane.time_state_mut(&known::RADAR) = active_loop(&[volume_time()]);
            pane.time_state_mut(&known::RADAR).view = kind;
        }

        app.dispatch_loop_renders();

        assert_eq!(
            app.gui.pane(0).unwrap().time_state(&known::RADAR).frames[0].render_failed,
            expected_failed,
            "{label}: the second dispatch pass judged a frame belonging to a \
                 pane it must not have looked at — or skipped one it must have"
        );
    }
}

#[test]
fn a_pane_that_cannot_loop_cannot_hold_another_panes_loop_back() {
    use squallar_egui::pane::LoopPhase;

    let mut app = two_pane_app(SITE, SITE);
    point_at_site(&mut app, 0);
    point_at_site(&mut app, 1);
    assert!(
        app.gui.pane_time_linked(0) && app.gui.pane_time_linked(1),
        "precondition: both panes must be time-linked — it is the per-pane \
             default, and it is what makes one pane able to hold another back"
    );

    {
        let ls = app.gui.pane_mut(0).unwrap().time_state_mut(&known::RADAR);
        *ls = active_loop(&[volume_time()]);
        ls.phase = LoopPhase::Ready;
    }
    assert!(
        app.gui
            .pane(0)
            .unwrap()
            .time_state(&known::RADAR)
            .is_render_ready(),
        "precondition: the map pane's loop must be ready, or nothing can be \
             observed being held back"
    );

    {
        let pane = app.gui.pane_mut(1).unwrap();
        pane.set_view(squallar_radar::types::RenderView::CrossSection);
        *pane.time_state_mut(&known::RADAR) = active_loop(&[volume_time()]);
    }
    assert!(
        !app.gui.pane(1).unwrap().can_loop(),
        "precondition: the second pane must be one nothing renders frames              for, or there is no hazard to observe"
    );
    assert!(
        !app.gui
            .pane(1)
            .unwrap()
            .time_state(&known::RADAR)
            .is_render_ready(),
        "precondition: the converted pane must be un-ready, which is the \
             whole hazard"
    );

    app.sync_loop_playback_start();

    assert_eq!(
        app.gui.pane(0).unwrap().time_state(&known::RADAR).phase,
        LoopPhase::Playing,
        "the map pane's loop never started: a pane nothing renders frames for \
             was counted as a looping pane that had not caught up yet, so with \
             sync on every loop on screen waits for ever"
    );
}

#[test]
fn the_loop_frame_broadcast_skips_a_pane_with_no_plan_view() {
    let textured = |app: &mut crate::app::App, idx: usize| {
        app.gui.pane(idx).unwrap().time_state(&known::RADAR).frames[0]
            .image
            .is_some()
    };

    for kind in [
        None,
        Some(squallar_radar::types::RenderView::CrossSection),
        Some(squallar_radar::types::RenderView::Volume),
    ] {
        let mut app = two_pane_app(SITE, SITE);
        point_at_site(&mut app, 0);
        point_at_site(&mut app, 1);
        assert!(
            app.gui.pane_layer_linked(0) && app.gui.pane_layer_linked(1),
            "precondition: both panes are layer-linked by default"
        );
        app.loop_mgr = LoopDownloadManager::new();
        app.loop_mgr.cache_scan(
            SITE,
            volume_time(),
            super::loop_dispatch_tests::volume_with_sweeps(&[TILT]),
        );
        for idx in 0..2 {
            let ls = app.gui.pane_mut(idx).unwrap().time_state_mut(&known::RADAR);
            *ls = active_loop(&[volume_time()]);
            ls.frames[0].render_in_flight = true;
        }
        if let Some(kind) = kind {
            let pane = app.gui.pane_mut(1).unwrap();
            pane.set_view(kind);
            *pane.time_state_mut(&known::RADAR) = active_loop(&[volume_time()]);
            pane.time_state_mut(&known::RADAR).frames[0].render_in_flight = true;
        }

        let target = app
            .gui
            .pane(0)
            .unwrap()
            .time_state(&known::RADAR)
            .rendered_for
            .clone()
            .expect("the fixture loop is keyed");
        app.channels
            .loop_render_sender
            .send(crate::channels::LoopRenderResponse {
                pane_idx: 0,
                timestamp: volume_time(),
                target,
                snapped: TILT,
                site_lat: 35.33,
                site_lon: -97.27,
                image: Some(loop_frame_pixels()),
                max_range_km: 230.0,
                nyquist_ms: None,
                melting_layer_source: None,
                storm_motion: None,
                polar: Default::default(),
            })
            .expect("the receiver lives on the App");
        app.poll_loop_render_results(&egui::Context::default());

        assert!(
            textured(&mut app, 0),
            "{kind:?}: precondition: the originating pane must take its own frame"
        );
        match kind {
            None => assert!(
                textured(&mut app, 1),
                "precondition: a map sibling keyed to the same target must take \
                     the broadcast, or nothing below is being filtered"
            ),
            Some(kind) => assert!(
                !textured(&mut app, 1),
                "{kind:?}: a loop frame was uploaded to a pane that draws none"
            ),
        }
    }
}

#[test]
fn the_cached_render_restore_skips_a_pane_with_no_plan_view() {
    for kind in [
        squallar_radar::types::RenderView::CrossSection,
        squallar_radar::types::RenderView::Volume,
    ] {
        let mut app = app_on_site();
        app.render.cache_render(
            SITE,
            PRODUCT,
            squallar_radar::types::RenderView::PlanView,
            TILT,
            cached_output(),
        );
        app.dispatch_pane_renders(&egui::Context::default());
        assert!(
            app.render.pane_render[0].cached_render.is_some(),
            "precondition: the pane must be holding a cached render to restore"
        );

        app.gui.pane_mut(0).unwrap().set_view(kind);
        app.gui
            .pane_mut(0)
            .unwrap()
            .overlay_cache_mut(&known::RADAR)
            .clear();

        app.restore_cached_render(&egui::Context::default());

        assert!(
            !holds_radar_texture(&mut app, 0),
            "{kind:?}: a resume re-uploaded a full-size plan-view texture to a \
                 pane that draws none"
        );
        assert!(
            app.render.pane_render[0].cached_render.is_some(),
            "{kind:?}: the cached pixels must survive, or converting back to a \
                 map costs a fresh render rather than an upload"
        );
    }
}

#[test]
fn converting_a_pane_tears_its_loop_down_on_both_sides() {
    for kind in [
        squallar_radar::types::RenderView::CrossSection,
        squallar_radar::types::RenderView::Volume,
    ] {
        let mut app = app_on_site();
        *app.gui.pane_mut(0).unwrap().time_state_mut(&known::RADAR) = active_loop(&[volume_time()]);
        app.loop_mgr = LoopDownloadManager::new();
        app.loop_mgr.set_plan(
            0,
            squallar_radar::loop_downloads::FramePlan::new(SITE.to_string(), vec![volume_time()]),
        );
        app.loop_mgr.plan_downloads_for(0, PRODUCT);
        assert!(
            app.loop_mgr.pending_pane_indices().contains(&0),
            "precondition: the pane must own a download queue to be relieved of"
        );
        assert!(
            app.gui
                .pane(0)
                .unwrap()
                .time_state(&known::RADAR)
                .is_active()
        );

        app.gui.pane_mut(0).unwrap().set_view(kind);

        assert!(
            !app.gui
                .pane(0)
                .unwrap()
                .time_state(&known::RADAR)
                .is_active(),
            "{kind:?}: the loop survived the conversion, so it will read \
                 \"Rendering\" for ever with no transport drawn to cancel it"
        );
        app.dispatch_loop_renders();
        assert!(
            !app.loop_mgr.pending_pane_indices().contains(&0),
            "{kind:?}: the download queue outlived the loop, so it goes on \
                 spending the shared budget on volumes nobody will draw"
        );
    }
}

#[test]
fn a_whole_volume_pane_keeps_the_volume_it_is_sampling() {
    for kind in [
        squallar_radar::types::RenderView::CrossSection,
        squallar_radar::types::RenderView::Volume,
    ] {
        let mut app = app_on_site();
        app.gui.pane_mut(0).unwrap().set_view(kind);
        drop(app.volumes.install_still(
            SITE.to_string(),
            volume_time(),
            (Arc::new(empty_scan()), Default::default()),
        ));
        drop(app.volumes.install_still(
            "KOUN".to_string(),
            volume_time(),
            (Arc::new(empty_scan()), Default::default()),
        ));

        app.evict_unshown_scans();

        assert!(
            app.volumes.holds_still(SITE, volume_time()),
            "{kind:?}: the volume this pane is cutting from was evicted"
        );
        assert!(
            !app.volumes.holds_any_still("KOUN"),
            "precondition: eviction must still be happening at all, or the \
                 assertion above holds for a pass that dropped nothing"
        );
    }
}
