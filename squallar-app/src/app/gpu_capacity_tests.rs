//! The GPU's own memory figure reaches the profile, and from there the fit —
//! where the class and the platform make it a measurement, and nowhere else.

use crate::loop_pool::GRID_BYTES;
use crate::platform::GpuCapacitySource;
use crate::platform_double::TestBridge;
use squallar_device_profile::budget::resolve;
use squallar_device_profile::fit::fit;
use squallar_device_profile::quality::DeviceClass;
use squallar_device_profile::scene::{Capacity, CapacitySource};

use super::App;
use super::tests::headless;

/// **What this test does not do**, said first: it does not run
/// `App::update_device_profile`, which needs a live wgpu device, and no test
/// in this crate has one. The reading is handed to the fold that method runs,
/// and the bridge carries the same reading so the shape a driver-backed bridge
/// produces is the shape folded. The scrape below holds the two together.
///
/// **What it holds**: the class rung never moves on a reading — `resolve`
/// reads none of the profile's memory fields, on either arm — and on this
/// profile the reading is no measurement at all: the headless application has
/// met no adapter, so its class is `Unknown` at the WebGL2 guarantee, which
/// the floor crate's policy keeps on the presumed arm whatever it read. The
/// arm where the reading *is* spent is
/// [`a_measured_capacity_reaches_the_fit_and_a_presumed_one_does_not_pretend_to`].
#[test]
fn an_injected_gpu_capacity_reaches_the_profile_and_moves_no_budget() {
    let reading: (u64, GpuCapacitySource) = (24 << 30, GpuCapacitySource::Measured);
    let mut app = headless(TestBridge::desktop().with_gpu_capacity(reading.0, reading.1));
    let budgets_before = app.budgets;
    assert_eq!(
        app.device_profile.vram_bytes, None,
        "unread before any adapter has answered"
    );
    assert_eq!(app.device_profile.class, DeviceClass::Unknown);

    app.adopt_gpu_capacity(Some(reading));

    assert_eq!(app.device_profile.vram_bytes, Some(24 << 30));
    assert_eq!(
        resolve(&app.device_profile),
        budgets_before,
        "a GPU capacity figure moved the class rung; `resolve` reads no memory field",
    );
    assert_eq!(app.budgets, budgets_before);
    assert_eq!(
        app.capacity().source,
        CapacitySource::Presumed,
        "an adapter the driver would not class, reporting the WebGL2 guarantee, is \
         not a measurement whatever it read",
    );

    app.adopt_gpu_capacity(None);
    assert_eq!(
        app.device_profile.vram_bytes, None,
        "a reader that stops answering leaves the field unread, not stale"
    );
}

/// **A measured capacity reaches the fit, and a presumed one does not pretend
/// to.** The same headless application on the class a discrete card earns:
/// with a 24 GiB reading its capacity is measured, the `budget state:` line
/// says `cap 24576 1`, and six two-hour loops fit at the full 36-frame render
/// budget — where the 3840 MiB presumption, which is what the same profile
/// holds before the reading arrives, prints `cap 3840 0` and halves the
/// history to 18. The default headless application, having met no adapter,
/// prints `cap 3840 0` too.
#[test]
fn a_measured_capacity_reaches_the_fit_and_a_presumed_one_does_not_pretend_to() {
    let line = |app: &App| {
        crate::budget_telemetry::budget_state_line(
            &app.budgets,
            &app.device_profile,
            None,
            app.loop_pool.bytes(),
            &app.capacity(),
        )
    };
    let six = squallar_device_profile::scene::Scene {
        panes: vec![
            squallar_device_profile::scene::PaneNeed {
                px: [1920, 1080],
                view: squallar_radar::types::RenderView::PlanView,
                looping: true,
                loop_span_secs: 2 * 60 * 60,
                cadence_secs: None,
                overlay_frame_bytes: 0,
                volume_grids: 0,
                ground: squallar_device_profile::quality::GroundPass::Off,
                buildings: false,
            };
            6
        ],
        tile_sources: Vec::new(),
        mirror_px: [0, 0],
    };

    let plain = headless(TestBridge::desktop());
    assert!(line(&plain).ends_with(", cap 3840 0"), "{}", line(&plain));
    assert_eq!(
        plain.capacity(),
        Capacity::presumed(&plain.device_profile.limits)
    );

    let reading = (24u64 << 30, GpuCapacitySource::Measured);
    let mut app = headless(TestBridge::desktop().with_gpu_capacity(reading.0, reading.1));
    // What `update_device_profile` does with a live discrete adapter, minus
    // the adapter: the class, then the reading through the tested fold.
    app.device_profile.class = DeviceClass::Discrete;
    assert!(
        line(&app).ends_with(", cap 3840 0"),
        "a discrete class with no reading yet is still the presumption: {}",
        line(&app),
    );
    let presumed = fit(&six, &app.device_profile, &app.capacity(), GRID_BYTES);
    assert_eq!(presumed.loop_render_budget, 18);

    app.adopt_gpu_capacity(Some(reading));

    assert_eq!(app.capacity(), Capacity::measured(24 << 30, None));
    assert!(line(&app).ends_with(", cap 24576 1"), "{}", line(&app));
    let measured = fit(&six, &app.device_profile, &app.capacity(), GRID_BYTES);
    assert_eq!(
        measured.loop_render_budget, 36,
        "six two-hour loops fit an 18 GiB allowance with every frame",
    );
    assert_eq!(measured, resolve(&app.device_profile));
    assert_eq!(
        app.fit_scene(&six),
        measured,
        "the application's own fit is the floor crate's, against the same capacity",
    );
    // The pool follows: 3456 MiB of loops, past the bracket's 3072 MiB ceiling.
    app.adopt_budgets(measured);
    assert_eq!(app.pool_for_scene(&six).bytes(), 3456 << 20);

    // A reader that stops answering puts the same profile back on the
    // presumed arm: nothing is remembered.
    app.adopt_gpu_capacity(None);
    assert!(line(&app).ends_with(", cap 3840 0"), "{}", line(&app));
}

/// The fold the test above runs is the one production runs: the method asks
/// the bridge about the live adapter and device, and hands that answer to the
/// same fold.
#[test]
fn the_profile_update_folds_the_bridges_reading_through_the_tested_seam() {
    let body = include_str!("../app.rs")
        .split_once("fn update_device_profile(&mut self")
        .and_then(|(_, rest)| rest.split_once("\n    }"))
        .map(|(body, _)| body)
        .expect("`update_device_profile` is still a method on `App`");
    assert!(
        body.contains("self.platform.gpu_capacity(&state.adapter, &state.device)"),
        "`update_device_profile` no longer asks the bridge about the live adapter",
    );
    assert!(
        body.contains("self.adopt_gpu_capacity(capacity)"),
        "`update_device_profile` no longer folds the reading through `adopt_gpu_capacity`",
    );
    assert!(
        body.contains("self.fit_scene(&scene)"),
        "`update_device_profile` no longer fits the scene through the checked seam",
    );
}
