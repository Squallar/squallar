//! **What the app does when the platform says its graphics context came back.**
//!
//! The bridge's side of this — the two DOM listeners on the canvas, the
//! `preventDefault()` that is what makes a restore happen at all, and the
//! once-only delivery — is `squallar_web::context_loss`, tested on the host
//! beside it. What is asserted here is the half in this crate: the poll is
//! asked on every frame's platform pass, a restore runs the same teardown a
//! native `SurfaceStatus::Lost` runs, and a page that has lost nothing is left
//! alone.

use crate::app::App;
use crate::app::tests::n_pane_app;
use crate::platform::PlatformBridge;
use crate::platform_double::TestBridge;

const SITE: &str = "KTLX";

/// A mirror plan the app is claiming to have applied — one of the seven things
/// a dead device invalidates.
fn applied_plan() -> squallar_gpu::egui_renderer::MirrorPlan {
    squallar_gpu::egui_renderer::MirrorPlan {
        size_in_pixels: [512, 512],
        pixels_per_point: 1.0,
        size_in_points: [512.0, 512.0],
        applied_scale: 1.0,
        wanted_scale: 1.0,
    }
}

/// Seed what a pane holds that a dead device owned, and answer the render
/// generation the teardown is expected to bump.
fn seed_graphics_state(app: &mut App) -> u64 {
    app.render.pane_render[0].last_rendered =
        Some((squallar_radar::types::RadarProduct::Reflectivity, 0.5));
    app.mirror_plan_applied = Some(applied_plan());
    app.gui
        .pane(0)
        .expect("the fixture built a pane")
        .radar_sites_render_gen
}

/// The `n_pane_app` fixture with a bridge whose restore gauge this test still
/// holds — the only way to fire an event that arrives *between* two frames,
/// the way the canvas's own listener fires one.
fn app_on_a_bridge_that_can_lose_its_context(app: &mut App) -> std::rc::Rc<std::cell::Cell<bool>> {
    let bridge = TestBridge::web();
    let gauge = bridge.graphics_restore_gauge();
    app.platform = Box::new(bridge);
    gauge
}

/// **A restore runs the whole teardown**, read through the three things a
/// caller can see from here: the render dedupe is cleared so the next frame
/// rebuilds rather than skipping against a picture the dead device drew, the
/// Gui released its own handles, and the rendering state is gone so the next
/// redraw recreates the surface instead of drawing through a dead one.
#[test]
fn a_restored_context_tears_the_graphics_state_down_and_the_next_frame_rebuilds() {
    let mut app = n_pane_app(1, SITE);
    let restore = app_on_a_bridge_that_can_lose_its_context(&mut app);
    let sites_gen = seed_graphics_state(&mut app);

    restore.set(true);
    app.poll_platform_state();

    assert!(
        app.render.pane_render[0].last_rendered.is_none(),
        "the next frame would dedupe against a picture the lost device drew",
    );
    assert_eq!(
        app.gui
            .pane(0)
            .expect("the fixture built a pane")
            .radar_sites_render_gen,
        sites_gen.wrapping_add(1),
        "Gui::clear_graphics_state did not run, so every pane still holds \
         texture handles the dead device owned",
    );
    assert!(
        app.state.is_none(),
        "the rendering state survived the restore, so the next redraw would \
         draw through the dead device instead of recreating the surface",
    );
    assert!(
        app.mirror_plan_applied.is_none(),
        "the applied mirror plan still describes a texture that died with the \
         device",
    );
}

/// **The poll is consuming, and one restore is one teardown.** A later frame
/// with nothing new to report must leave the app alone: re-running the
/// teardown every frame would drop every texture the page had just rebuilt,
/// which is a page that never paints again rather than one that recovers.
#[test]
fn a_restore_is_acted_on_once_and_the_next_frame_is_left_alone() {
    let mut app = n_pane_app(1, SITE);
    let restore = app_on_a_bridge_that_can_lose_its_context(&mut app);

    restore.set(true);
    app.poll_platform_state();

    // Re-seeded after the first teardown, so what the second pass is asked
    // about is state the rebuild would have put back.
    let after_first = seed_graphics_state(&mut app);
    app.poll_platform_state();

    assert!(
        app.render.pane_render[0].last_rendered.is_some(),
        "the teardown ran a second time on a frame the platform reported \
         nothing on; the page would shed every texture it had just rebuilt",
    );
    assert_eq!(
        app.gui
            .pane(0)
            .expect("the fixture built a pane")
            .radar_sites_render_gen,
        after_first,
        "Gui::clear_graphics_state ran again with no restore to run it",
    );
    assert!(app.mirror_plan_applied.is_some());
}

/// **A page that has lost nothing is untouched** — the over-firing arm, and
/// the one that matters more: this poll runs on every frame of every platform,
/// so a path that fired without an event would tear the graphics state down at
/// the frame rate.
#[test]
fn a_platform_reporting_no_restore_changes_nothing() {
    let mut app = n_pane_app(1, SITE);
    let _restore = app_on_a_bridge_that_can_lose_its_context(&mut app);
    let sites_gen = seed_graphics_state(&mut app);

    for _ in 0..3 {
        app.poll_platform_state();
    }

    assert!(
        app.render.pane_render[0].last_rendered.is_some(),
        "three frames with no context event cleared the render dedupe",
    );
    assert_eq!(
        app.gui
            .pane(0)
            .expect("the fixture built a pane")
            .radar_sites_render_gen,
        sites_gen,
        "three frames with no context event released the Gui's textures",
    );
    assert!(app.mirror_plan_applied.is_some());
}

/// **Every native bridge answers `false`, from the trait's own default**, and
/// a bridge that started answering otherwise would be running a teardown its
/// losses already reach through `SurfaceStatus::Lost` — the same work twice,
/// once too early.
#[test]
fn the_native_bridges_report_no_restore_ever() {
    for (name, mut bridge) in [
        ("desktop", TestBridge::desktop()),
        ("android", TestBridge::android()),
        ("ios", TestBridge::ios()),
    ] {
        assert!(
            !bridge.poll_graphics_restore(),
            "{name} reports a graphics restore",
        );
    }

    // Control: the double CAN answer `true`, so the three readings above are
    // not three readings of a method with no other arm.
    let mut web = TestBridge::web();
    web.graphics_restore_gauge().set(true);
    assert!(web.poll_graphics_restore());
    assert!(
        !web.poll_graphics_restore(),
        "the double's poll is not consuming, so it does not stand in for the \
         bridge it doubles",
    );
}
