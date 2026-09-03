//! `scene_of` prices each 3D pane at what the volume painter last fitted its
//! offscreen from — the pane's own size in physical pixels and the ground pass
//! it decided — read off the painter the app owns. The window's size stands in
//! only until the painter has fitted one; a 2D pane carries no size at all.

use crate::app::App;
use crate::app::tests::n_pane_app;
use crate::loop_pool::GRID_BYTES;
use squallar_device_profile::fit::need_terms;
use squallar_device_profile::quality::GroundPass;
use squallar_radar::types::RenderView;
use squallar_volumetric::VolumeSupport;
use squallar_volumetric::bridge::BridgeVolumePainter;
use std::sync::Arc;

const SITE: &str = "KTLX";
const MIB: usize = 1024 * 1024;
/// A 1920 x 1080 window split six ways: each pane's own rect.
const SIXTH: [u32; 2] = [640, 540];
const HD: [u32; 2] = [1920, 1080];

/// `n` 3D panes and the painter that fits their offscreens, exactly as
/// `install_volume_bridge` builds it, but with nothing painted yet.
fn app_with_volume_panes(n: usize) -> App {
    let mut app = n_pane_app(n, SITE);
    for idx in 0..n {
        app.gui
            .pane_mut(idx)
            .expect("the fixture has this pane")
            .set_view(RenderView::Volume);
    }
    assert!(
        app.state.is_none(),
        "precondition: a headless app has no surface, so the window figure is [0, 0]"
    );
    assert_eq!(
        app.budgets.offscreen_bytes,
        20 * MIB,
        "precondition: the desktop class rung"
    );
    app.volume_painter = Some(Arc::new(BridgeVolumePainter::new(
        app.volume_store.clone(),
        app.budgets.quality_ceiling,
        app.budgets.offscreen_bytes,
        VolumeSupport::Supported,
    )));
    app
}

fn painter(app: &App) -> &BridgeVolumePainter {
    app.volume_painter
        .as_deref()
        .expect("the fixture installed a painter")
}

fn offscreens(app: &App) -> u64 {
    need_terms(&app.scene_of(), &app.budgets, GRID_BYTES).offscreens
}

/// Six 3D panes on a 1920 x 1080 window price six 640 x 540 offscreens —
/// 8,294,400 bytes, what ONE window-sized offscreen costs — where the window
/// figure priced six of the window, 49,766,400. The difference is 41,472,000
/// bytes of need that no pane ever held.
#[test]
fn six_volume_panes_price_six_pane_sized_offscreens() {
    let app = app_with_volume_panes(6);
    for idx in 0..6 {
        painter(&app).note_pane_picture(idx, SIXTH, GroundPass::Off);
    }
    let scene = app.scene_of();
    assert_eq!(scene.panes.len(), 6);
    for pane in &scene.panes {
        assert_eq!(pane.view, RenderView::Volume);
        assert_eq!(pane.px, SIXTH, "each pane at its own rect");
        assert_eq!(pane.ground, GroundPass::Off);
    }
    assert_eq!(offscreens(&app), 6 * 640 * 540 * 4);
    assert_eq!(offscreens(&app), 8_294_400);

    let window_priced = 6 * app
        .budgets
        .quality_ceiling
        .fit(HD, app.budgets.offscreen_bytes, GroundPass::Off)
        .bytes() as u64;
    assert_eq!(window_priced, 49_766_400, "what the window figure priced");
    assert_eq!(window_priced - offscreens(&app), 41_472_000);
}

/// One 3D pane: its rect is the window less the bars, which the painter is
/// told and the app is not. Priced at 1920 x 1000 the pane costs 7,680,000
/// bytes where the window figure, 1920 x 1080, cost 8,294,400.
#[test]
fn a_lone_pane_is_priced_at_its_own_rect_not_the_window() {
    let app = app_with_volume_panes(1);
    painter(&app).note_pane_picture(0, [1920, 1000], GroundPass::Off);
    let scene = app.scene_of();
    assert_eq!(scene.panes.len(), 1);
    assert_eq!(scene.panes[0].px, [1920, 1000]);
    assert_eq!(offscreens(&app), 1920 * 1000 * 4);
    assert_eq!(offscreens(&app), 7_680_000);
    assert_eq!(
        app.budgets
            .quality_ceiling
            .fit(HD, app.budgets.offscreen_bytes, GroundPass::Off)
            .bytes(),
        8_294_400,
        "what the window figure priced",
    );
}

/// The ground pass is the painter's decision, carried as decided: a pane
/// whose fit priced ground is priced at 16 bytes a pixel here too.
#[test]
fn the_ground_pass_the_painter_decided_is_propagated() {
    let app = app_with_volume_panes(2);
    painter(&app).note_pane_picture(0, SIXTH, GroundPass::On);
    painter(&app).note_pane_picture(1, SIXTH, GroundPass::Off);
    let scene = app.scene_of();
    assert_eq!(scene.panes[0].ground, GroundPass::On);
    assert_eq!(scene.panes[1].ground, GroundPass::Off);
    assert_eq!(offscreens(&app), 640 * 540 * 16 + 640 * 540 * 4);
}

/// Until the painter has fitted an offscreen for a 3D pane, the window's size
/// stands in with no ground — the conservative figure, over-pricing by at
/// most the offscreen budget and never under-pricing. Headless, the window is
/// `[0, 0]`, which the precondition in the fixture pins.
#[test]
fn a_volume_pane_the_painter_has_not_fitted_is_priced_at_the_window() {
    let app = app_with_volume_panes(2);
    painter(&app).note_pane_picture(1, SIXTH, GroundPass::On);
    let scene = app.scene_of();
    assert_eq!(scene.panes[0].px, [0, 0], "the window figure, unpainted");
    assert_eq!(scene.panes[0].ground, GroundPass::Off);
    assert_eq!(scene.panes[1].px, SIXTH, "the painted pane is its own");
    assert_eq!(scene.panes[1].ground, GroundPass::On);
}

/// A pane that gave its offscreen back is not still priced at its old
/// picture: releasing the volume forgets the record, and the pane falls back
/// to the window stand-in until it is painted again.
#[test]
fn a_released_pane_is_no_longer_priced_at_its_old_picture() {
    let mut app = app_with_volume_panes(1);
    painter(&app).note_pane_picture(0, SIXTH, GroundPass::On);
    assert_eq!(app.scene_of().panes[0].px, SIXTH);
    app.handle_release_volume(0);
    assert_eq!(painter(&app).pane_picture(0), None);
    assert_eq!(app.scene_of().panes[0].px, [0, 0]);
    assert_eq!(app.scene_of().panes[0].ground, GroundPass::Off);
}

/// Nothing is sized from a 2D pane's figure, and it carries none — not the
/// window's, whatever the painter holds under the same index.
#[test]
fn a_plan_view_pane_carries_no_size() {
    let mut app = app_with_volume_panes(1);
    painter(&app).note_pane_picture(0, SIXTH, GroundPass::On);
    app.gui
        .pane_mut(0)
        .expect("pane 0 exists")
        .set_view(RenderView::PlanView);
    let scene = app.scene_of();
    assert_eq!(scene.panes[0].view, RenderView::PlanView);
    assert_eq!(scene.panes[0].px, [0, 0]);
    assert_eq!(scene.panes[0].ground, GroundPass::Off);
    assert_eq!(offscreens(&app), 0);
}
