use super::*;
use crate::app::tests::{empty_scan, headless, two_pane_app};
use crate::loop_downloads::LoopDownloadManager;
use crate::platform_double::TestBridge;
use rustdar_egui::pane::{LoopFrame, LoopPhase, LoopPlaybackState, PaneKind};
use rustdar_overlays::render::overlay_state::OverlayKind;
use rustdar_radar::sites::RadarSite;
use rustdar_radar::types::RadarProduct;

const SITE: &str = "KTLX";
const PRODUCT: RadarProduct = RadarProduct::Reflectivity;
const TILT: f32 = 0.5;

fn volume_time() -> chrono::NaiveDateTime {
    chrono::NaiveDate::from_ymd_opt(2026, 7, 29)
        .unwrap()
        .and_hms_opt(18, 30, 0)
        .unwrap()
}

/// A one-pane app on [`SITE`] with scan info, which is what
/// `apply_render_to_pane` reads the site coordinates out of before it will
/// place anything at all.
fn app_on_site() -> crate::app::App {
    let mut app = headless(TestBridge::desktop());
    point_at_site(&mut app, 0);
    app.render.ensure_pane_count(1);
    app
}

fn point_at_site(app: &mut crate::app::App, pane_idx: usize) {
    let site = rustdar_radar::sites::get_radar_site(SITE)
        .expect("KTLX is a real radar")
        .clone();
    let mut product_elevations = std::collections::HashMap::new();
    product_elevations.insert(PRODUCT, vec![TILT]);
    let pane = app.gui.pane_mut(pane_idx).expect("pane exists");
    pane.site = SITE.to_string();
    pane.selected_product = PRODUCT;
    pane.selected_elevation = TILT;
    app.gui.set_scan_info_for_pane(
        pane_idx,
        rustdar_radar::types::ScanInfo {
            site,
            timestamp: volume_time(),
            vcp_number: 212,
            available_products: vec![PRODUCT],
            product_elevations,
            status: String::new(),
        },
    );
}

/// Finished pixels, full size: `ColorImage::from_rgba_unmultiplied` checks
/// the buffer against the dimensions it is handed, in a bare `assert_eq!`
/// that is live in release and on the main thread.
fn finished_pixels() -> Arc<Vec<u8>> {
    Arc::new(vec![0u8; IMAGE_SIZE * IMAGE_SIZE * 4])
}

fn cached_output() -> crate::render_dispatch::CachedRenderOutput {
    crate::render_dispatch::CachedRenderOutput {
        image_data: finished_pixels(),
        max_range_km: 230.0,
        value_data: Arc::new(Vec::new()),
    }
}

/// Whether pane `pane_idx` is holding a radar texture.
///
/// The observable throughout this module: it is what `apply_render_to_pane`
/// exists to produce, and the only thing that tells a pane which was served
/// from one which was skipped.
fn holds_radar_texture(app: &mut crate::app::App, pane_idx: usize) -> bool {
    app.gui
        .pane_mut(pane_idx)
        .expect("pane exists")
        .overlay_cache_mut(OverlayKind::Radar)
        .current
        .is_some()
}

/// A finished render landing on the channel, as a render thread posts one,
/// and then drained by the poller.
///
/// The bare `egui::Context` is the whole renderer these paths need —
/// `Context::load_texture` wants no device, no surface and no window — which
/// is what `stamping_tests` already relies on and why the frame's context is
/// a parameter of the poller rather than something it reaches through
/// `self.state` for.
fn deliver(app: &mut crate::app::App, pane_idx: usize) {
    app.channels
        .render_sender
        .send(crate::channels::RenderResponse {
            rendered: Some(crate::channels::RenderedImage {
                image_data: finished_pixels(),
                max_range_km: 230.0,
                value_data: Arc::new(Vec::new()),
            }),
            product: PRODUCT,
            elevation: TILT,
            generation: app.render.render_generation,
            pane_idx,
        })
        .expect("the receiver lives on the App");
    app.poll_render_results(&egui::Context::default());
}

/// `dispatch_pane_renders` skips a pane with no plan view, and skips it
/// *before* the rendering-params branch.
///
/// Driven through the render cache rather than through a spawned render, so
/// neither a thread nor a decoded volume is needed: a cache hit is one of the
/// two ways the `if` arm places an image, and reaching it at all proves the
/// pane got past the guard. The map case is asserted in the same run, so this
/// cannot be satisfied by a dispatcher that skips every pane.
#[test]
fn the_dispatcher_skips_a_pane_with_no_plan_view() {
    for kind in [PaneKind::CrossSection, PaneKind::Volume] {
        let mut app = app_on_site();
        app.render.cache_render(
            SITE,
            PRODUCT,
            rustdar_radar::types::RenderView::PlanView,
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
            rustdar_radar::types::RenderView::PlanView,
            TILT,
            cached_output(),
        );
        app.gui.pane_mut(0).unwrap().set_kind(kind);

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

/// The sibling broadcast skips a pane with no plan view.
///
/// It accepts on site + product + elevation with **no view term**, and all
/// three match for a section pane sitting beside the map it was cut from —
/// which is the ordinary arrangement rather than a corner case. Unfiltered,
/// the section pane is handed the map's raster on the first render either of
/// them triggers.
///
/// Pane 1 is asserted to take the broadcast while it is still a map, so what
/// is observed below is the filter and not a sibling that never qualified.
#[test]
fn the_sibling_broadcast_skips_a_pane_with_no_plan_view() {
    for kind in [PaneKind::CrossSection, PaneKind::Volume] {
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
        app.gui.pane_mut(1).unwrap().set_kind(kind);

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

/// A render already in flight when its pane is converted is not placed on it.
///
/// `dispatch_pane_renders` no longer starts one, but conversion happens on a
/// frame and a render takes many, so the window is real rather than
/// theoretical. The result still clears `render_in_flight` — that is its
/// other job, and dropping it would wedge the pane forever — and
/// `last_rendered` stays unset, so converting back to a map re-dispatches
/// rather than showing nothing.
#[test]
fn a_render_in_flight_across_a_conversion_is_not_placed() {
    let mut app = app_on_site();
    app.render.pane_render[0].render_in_flight = true;
    app.gui.pane_mut(0).unwrap().set_kind(PaneKind::Volume);

    deliver(&mut app, 0);

    assert!(!holds_radar_texture(&mut app, 0));
    assert!(
        !app.render.pane_render[0].render_in_flight,
        "the in-flight flag was not cleared, so this pane could never ask \
             for another render as long as it lived"
    );
    assert_eq!(app.render.pane_render[0].last_rendered, None);
}

/// A loop on [`SITE`] with one frame per timestamp, keyed to
/// [`PRODUCT`] at [`TILT`].
fn active_loop(timestamps: &[chrono::NaiveDateTime]) -> LoopPlaybackState {
    let mut ls = LoopPlaybackState::new_for_loop(
        3600,
        &RadarSite {
            name: SITE,
            lat: 35.33,
            lon: -97.27,
            heights: None,
        },
    );
    ls.phase = LoopPhase::Rendering;
    ls.frames = timestamps
        .iter()
        .map(|&timestamp| LoopFrame {
            timestamp,
            texture: None,
            render_in_flight: false,
            render_failed: false,
        })
        .collect();
    // Takes the target and reports `false`: there was nothing to discard, so
    // there is nothing for the caller to react to. What matters here is that
    // `rendered_for` is now set, which is what the dispatcher reads.
    ls.retarget_renders(PRODUCT, TILT);
    assert!(
        ls.rendered_for.is_some(),
        "precondition: a fresh loop must take its first target"
    );
    ls
}

/// `dispatch_loop_renders`' **first** pass skips a pane with no plan view.
///
/// That pass's job is to notice the pane's product moving and re-key the whole
/// frame list to it, which also queues a fresh download plan — for a pane
/// nobody draws, a download queue serving nobody. So the observable is
/// `rendered_for`: it must move for a map pane and must not move for a
/// non-map one.
#[test]
fn the_first_loop_dispatch_pass_skips_a_pane_with_no_plan_view() {
    let moved_to = RadarProduct::Velocity;
    assert!(
        !moved_to.is_level3() && !PRODUCT.is_level3(),
        "precondition: both products must be Level II, or the replan the \
             retarget triggers starts a download this test does not serve"
    );

    for (kind, expected) in [
        (PaneKind::Map, Some((moved_to, 0.0))),
        (PaneKind::CrossSection, Some((PRODUCT, TILT))),
        (PaneKind::Volume, Some((PRODUCT, TILT))),
    ] {
        let mut app = app_on_site();
        {
            let pane = app.gui.pane_mut(0).unwrap();
            // Converted *first*, because `set_kind` tears a loop down — the
            // root fix for the stuck-loop family. Planting the loop afterwards
            // is what leaves the state this filter is about, and it is
            // reachable: `loop_state` is a public field, and the setter is
            // not the only route to a non-map pane.
            pane.set_kind(kind);
            pane.loop_state = active_loop(&[volume_time()]);
            pane.selected_product = moved_to;
            pane.selected_elevation = 0.0;
        }

        app.dispatch_loop_renders();

        let keyed = app
            .gui
            .pane(0)
            .unwrap()
            .loop_state
            .rendered_for
            .as_ref()
            .map(|target| (target.product, target.elevation));
        assert_eq!(
            keyed, expected,
            "{kind:?}: the loop's render target moved for a pane whose frames \
                 nobody draws — or failed to move for one whose frames are drawn"
        );
    }
}

/// `dispatch_loop_renders`' **second** pass skips a pane with no plan view.
///
/// That pass is the one which plans renders and clones siblings' textures.
/// The observable is `render_failed`, which it sets on a frame whose own
/// volume carries no sweep for the selected product: a scan with no sweeps at
/// all makes `find_closest_elevation` answer `None`, so a map pane's frame is
/// retired and a non-map pane's frame is never examined. No render thread and
/// no real volume are involved.
#[test]
fn the_second_loop_dispatch_pass_skips_a_pane_with_no_plan_view() {
    for (kind, expected_failed) in [
        (PaneKind::Map, true),
        (PaneKind::CrossSection, false),
        (PaneKind::Volume, false),
    ] {
        let mut app = app_on_site();
        app.loop_mgr = LoopDownloadManager::new();
        // A volume that is present, so the frame is not `Pending`, and
        // carries nothing for the product, so it is `Unrenderable`.
        app.loop_mgr
            .cache_scan(SITE, volume_time(), Arc::new(empty_scan()));
        {
            let pane = app.gui.pane_mut(0).unwrap();
            // Converted first; see the note in the test above.
            pane.set_kind(kind);
            pane.loop_state = active_loop(&[volume_time()]);
        }

        app.dispatch_loop_renders();

        assert_eq!(
            app.gui.pane(0).unwrap().loop_state.frames[0].render_failed,
            expected_failed,
            "{kind:?}: the second dispatch pass judged a frame belonging to a \
                 pane it must not have looked at — or skipped one it must have"
        );
    }
}

/// A pane with no plan view cannot hold another pane's loop back.
///
/// The worst of these, because the symptom is in the *other* panes and the
/// cause is the filter that protects the render path.
/// `sync_loop_playback_start`'s rule is "hold every looping pane until all of
/// them are ready", and a pane whose frames nothing renders can never become
/// ready — `dispatch_loop_renders` neither fills its frames nor marks them
/// failed. So one such pane, with Sync Layers on, stops every map pane's loop
/// from ever starting: a deadlock, silently, in panes the user did not touch.
///
/// The blocked pane is given a real textured frame so it *is* render-ready and
/// would start on its own; the only thing that can stop it is the sync rule.
#[test]
fn a_pane_with_no_plan_view_cannot_hold_another_panes_loop_back() {
    use rustdar_egui::pane::LoopPhase;

    let mut app = two_pane_app(SITE, SITE);
    point_at_site(&mut app, 0);
    point_at_site(&mut app, 1);
    assert!(
        app.gui.is_sync_layers(),
        "precondition: sync must be on — it is the config default, and it is              what makes one pane able to hold another back"
    );

    // Pane 0: a map pane whose loop is ready to play.
    {
        let ls = &mut app.gui.pane_mut(0).unwrap().loop_state;
        *ls = active_loop(&[volume_time()]);
        ls.phase = LoopPhase::Ready;
    }
    assert!(
        app.gui.pane(0).unwrap().loop_state.is_render_ready(),
        "precondition: the map pane's loop must be ready, or nothing can be \
             observed being held back"
    );

    // Pane 1: converted, and then given an active loop whose frames nothing
    // will ever render — the state `set_kind` clears but a public field can
    // still reach.
    {
        let pane = app.gui.pane_mut(1).unwrap();
        pane.set_kind(PaneKind::Volume);
        pane.loop_state = active_loop(&[volume_time()]);
    }
    assert!(
        !app.gui.pane(1).unwrap().loop_state.is_render_ready(),
        "precondition: the converted pane must be un-ready, which is the \
             whole hazard"
    );

    app.sync_loop_playback_start();

    assert_eq!(
        app.gui.pane(0).unwrap().loop_state.phase,
        LoopPhase::Playing,
        "the map pane's loop never started: a pane nothing renders frames for \
             was counted as a looping pane that had not caught up yet, so with \
             sync on every loop on screen waits for ever"
    );
}

/// The loop-frame broadcast skips a pane with no plan view.
///
/// The fifth of these broadcasts and the direct sibling of the static one:
/// a loop frame is a plan-view raster, so handing one to a pane that draws
/// none buys a GPU texture per frame for nothing.
///
/// Driven by planting the same target on both panes and delivering one
/// finished frame, with the map case asserted in the same run so the filter is
/// what is observed rather than a sibling that never qualified.
#[test]
fn the_loop_frame_broadcast_skips_a_pane_with_no_plan_view() {
    let textured = |app: &mut crate::app::App, idx: usize| {
        app.gui.pane(idx).unwrap().loop_state.frames[0]
            .texture
            .is_some()
    };

    for kind in [None, Some(PaneKind::CrossSection), Some(PaneKind::Volume)] {
        let mut app = two_pane_app(SITE, SITE);
        point_at_site(&mut app, 0);
        point_at_site(&mut app, 1);
        assert!(
            app.gui.is_sync_layers(),
            "precondition: sync is on by default"
        );
        app.loop_mgr = LoopDownloadManager::new();
        // A volume that really carries the tilt, reusing the fixture the loop
        // dispatch tests already build: `broadcast_sweep` resolves the
        // *sibling's* own scan and refuses an image whose angle its data does
        // not have, so an empty volume would refuse the broadcast for a reason
        // that has nothing to do with pane kinds.
        app.loop_mgr.cache_scan(
            SITE,
            volume_time(),
            super::loop_dispatch_tests::scan_with_sweeps(&[TILT]),
        );
        for idx in 0..2 {
            let ls = &mut app.gui.pane_mut(idx).unwrap().loop_state;
            *ls = active_loop(&[volume_time()]);
            // A result is only accepted for a frame that is *awaiting* one —
            // see `frame_awaiting_render_result_mut` — which is the state
            // `dispatch_loop_renders` leaves behind when it spawns.
            ls.frames[0].render_in_flight = true;
        }
        if let Some(kind) = kind {
            // Converted, then re-given the loop: `set_kind` tears one down.
            let pane = app.gui.pane_mut(1).unwrap();
            pane.set_kind(kind);
            pane.loop_state = active_loop(&[volume_time()]);
            pane.loop_state.frames[0].render_in_flight = true;
        }

        let target = app
            .gui
            .pane(0)
            .unwrap()
            .loop_state
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
                image: Some(egui::ColorImage::from_rgba_unmultiplied(
                    [IMAGE_SIZE, IMAGE_SIZE],
                    &finished_pixels(),
                )),
                max_range_km: 230.0,
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

/// `restore_cached_render` skips a pane with no plan view.
///
/// `dispatch_pane_renders` deliberately *keeps* `cached_render` on a converted
/// pane so that converting back to a map is instant, which makes this the one
/// place the kept copy could still be uploaded — on every suspend, resume and
/// surface loss, a full-size RGBA texture into the Radar overlay cache of a
/// pane that draws no map.
#[test]
fn the_cached_render_restore_skips_a_pane_with_no_plan_view() {
    for kind in [PaneKind::CrossSection, PaneKind::Volume] {
        let mut app = app_on_site();
        app.render.cache_render(
            SITE,
            PRODUCT,
            rustdar_radar::types::RenderView::PlanView,
            TILT,
            cached_output(),
        );
        app.dispatch_pane_renders(&egui::Context::default());
        assert!(
            app.render.pane_render[0].cached_render.is_some(),
            "precondition: the pane must be holding a cached render to restore"
        );

        // The state a conversion leaves: the cached pixels are kept on purpose.
        app.gui.pane_mut(0).unwrap().set_kind(kind);
        app.gui
            .pane_mut(0)
            .unwrap()
            .overlay_cache_mut(OverlayKind::Radar)
            .current = None;

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

/// Converting a pane tears its loop down, on both sides of the seam.
///
/// The root fix for the stuck-loop family, which was eight consumers with one
/// cause: a loop left running on a pane nothing renders frames for holds
/// `loop_mgr` state, keeps the event loop waking at loop frame rate, reads
/// "Rendering n/m" for ever with no transport drawn to cancel it, and goes on
/// spending the *shared* download budget on volumes nobody will draw.
///
/// `PaneState::set_kind` does the pane-local half. The other half — this
/// pane's queue inside `LoopDownloadManager`, which is keyed by index and
/// which a `PaneState` cannot reach — is done by `dispatch_loop_renders`, so
/// that it also covers a pane that reached a non-map kind by a route that
/// never called the setter.
#[test]
fn converting_a_pane_tears_its_loop_down_on_both_sides() {
    for kind in [PaneKind::CrossSection, PaneKind::Volume] {
        let mut app = app_on_site();
        app.gui.pane_mut(0).unwrap().loop_state = active_loop(&[volume_time()]);
        app.loop_mgr = LoopDownloadManager::new();
        app.loop_mgr.set_plan(
            0,
            crate::loop_downloads::FramePlan::new(
                SITE.to_string(),
                vec![(
                    volume_time(),
                    rustdar_radar::archive::Identifier::new("a-volume".to_string()),
                )],
            ),
        );
        app.loop_mgr.plan_downloads_for(0, PRODUCT);
        assert!(
            app.loop_mgr.pending_pane_indices().contains(&0),
            "precondition: the pane must own a download queue to be relieved of"
        );
        assert!(app.gui.pane(0).unwrap().loop_state.is_active());

        app.gui.pane_mut(0).unwrap().set_kind(kind);

        assert!(
            !app.gui.pane(0).unwrap().loop_state.is_active(),
            "{kind:?}: the loop survived the conversion, so it will read \
                 \"Rendering\" for ever with no transport drawn to cancel it"
        );
        // The host-side half, applied by the frame pass rather than by the
        // setter, because a `PaneState` cannot see `loop_mgr`.
        app.dispatch_loop_renders();
        assert!(
            !app.loop_mgr.pending_pane_indices().contains(&0),
            "{kind:?}: the download queue outlived the loop, so it goes on \
                 spending the shared budget on volumes nobody will draw"
        );
    }
}

/// `App::evict_unshown_scans` needs **no** kind filter, and this is the pin
/// on that.
///
/// It is the one all-panes loop where excluding a non-map pane would be the
/// bug. It retains a decoded volume if any pane names its site, through
/// `pane.site` and `pane.scan_info.site` — both flat fields on every pane
/// whatever its kind. A section pane samples the whole volume, so it needs it
/// alive *more* than a map pane does, and dropping it under one is a
/// use-after-evict-shaped fault in the pass whose entire job is knowing what
/// is on screen. This is why `PaneContent` is one field on a flat
/// `PaneState` rather than an `enum PaneState`.
#[test]
fn a_whole_volume_pane_keeps_the_volume_it_is_sampling() {
    for kind in [PaneKind::CrossSection, PaneKind::Volume] {
        let mut app = app_on_site();
        app.gui.pane_mut(0).unwrap().set_kind(kind);
        app.scan_data
            .insert(SITE.to_string(), Arc::new(empty_scan()));
        app.scan_data
            .insert("KOUN".to_string(), Arc::new(empty_scan()));

        app.evict_unshown_scans();

        assert!(
            app.scan_data.contains_key(SITE),
            "{kind:?}: the volume this pane is cutting from was evicted"
        );
        assert!(
            !app.scan_data.contains_key("KOUN"),
            "precondition: eviction must still be happening at all, or the \
                 assertion above holds for a pass that dropped nothing"
        );
    }
}
