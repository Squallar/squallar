//! The GPU's own memory figure reaches the profile, and from there the fit —
//! where the class and the platform make it a measurement, and nowhere else.
//! And the browser probe's figure reaches the session's capacity — where the
//! platform is a browser and the class unnamed, and nowhere else.

use crate::loop_pool::GRID_BYTES;
use crate::platform::{GpuCapacitySource, GpuProbeReport, ProbedCapacity};
use crate::platform_double::TestBridge;
use crate::pressure::Pressure;
use egui_wgpu::wgpu;
use squallar_device_profile::budget::{BudgetLimits, Platform, resolve};
use squallar_device_profile::fit::fit;
use squallar_device_profile::quality::DeviceClass;
use squallar_device_profile::scene::{Capacity, CapacitySource, Scene};

use super::tests::headless;
use super::{App, capacity_with_probe, gpu_probe_line};

/// The `budget state:` line the app would print now.
fn budget_line(app: &App) -> String {
    crate::budget_telemetry::budget_state_line(
        &app.budgets,
        &app.device_profile,
        None,
        app.loop_pool.bytes(),
        app.loop_pool_state.allocation().balloon_bytes(),
        &app.capacity(),
        app.gpu_probe,
        crate::pressure::LinearMemoryWatch::default(),
        &app.budget_readout,
    )
}

/// Six 1080p plan-view panes, each looping two hours.
fn six_two_hour_loops() -> Scene {
    Scene {
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
                overlay_pictures: 0,
                picture_px: [0, 0],
                loop_scans_shared: false,
                loop_scans_resident_bytes: 0,
                loop_scans_resident_frames: 0,
                loop_scans_needed: true,
            };
            6
        ],
        tile_sources: Vec::new(),
        mirror_px: [0, 0],
        overlay_grids: Vec::new(),
    }
}

/// What a headless application looks like in a browser: the web bracket, the
/// web platform, an adapter the driver would not class. The host build's
/// `for_target` says native, so the two profile fields are set by hand and
/// the budgets re-resolved to the web floor, as `App::new` would have.
fn web_app(bridge: TestBridge) -> App {
    let mut app = headless(bridge);
    app.device_profile.platform = Platform::Web;
    app.device_profile.limits = BudgetLimits::WASM;
    let floor = resolve(&app.device_profile);
    app.adopt_budgets(floor);
    app
}

/// What the web bridge hands over for a probe that held 4032 MiB and was
/// refused at 8128.
fn a_probe_of(bytes: u64) -> ProbedCapacity {
    ProbedCapacity {
        bytes,
        failed_at: Some(bytes.saturating_mul(2) + (64 << 20)),
        steps: 7,
        elapsed_ms: 812,
        capped: false,
    }
}

/// **The browser probe's figure reaches the fit, and only where it belongs.**
/// A web profile the driver would not class — every browser — holds the
/// 288 MiB presumption until the probe reports and prints `cap 288 0`; the
/// fold then prints `cap 4032 2`, the capacity is `Capacity::probed`, and
/// the scene is re-fitted once. Six two-hour loops that the presumption had
/// to shorten keep every frame under a 3024 MiB allowance; the page heap the
/// probe does not speak for still cannot hold their decoded volumes, and
/// only the host rungs move for that.
#[test]
fn a_probed_capacity_reaches_the_fit_on_a_web_profile_and_prints_cap_2() {
    let probe = a_probe_of(4032 << 20);
    let mut app = web_app(TestBridge::web().with_probed_capacity(probe.bytes));
    assert_eq!(app.device_profile.class, DeviceClass::Unknown);
    assert!(
        budget_line(&app).contains(", cap 288 0, probe 0, balloon "),
        "{}",
        budget_line(&app)
    );
    assert_eq!(
        app.capacity(),
        Capacity::presumed(&BudgetLimits::WASM),
        "before the probe reports, the web bracket's constant is the capacity",
    );
    let six = six_two_hour_loops();
    let presumed = fit(&six, &app.device_profile, &app.capacity(), GRID_BYTES);
    let class_rung = resolve(&app.device_profile);
    assert!(
        presumed.steps_back > 0,
        "six two-hour loops do not fit a 288 MiB presumption at the class rung: {presumed:?}",
    );

    // What the tick does with a live WebGPU adapter, minus the adapter: the
    // bridge's answer for that backend, through the fold.
    assert_eq!(
        app.platform.gpu_probe_report(wgpu::Backend::Gl),
        GpuProbeReport::Skipped,
        "asked about a WebGL2 page, the bridge has no figure to fold",
    );
    let GpuProbeReport::Found(reading) =
        app.platform.gpu_probe_report(wgpu::Backend::BrowserWebGpu)
    else {
        panic!("the double answers for a WebGPU backend");
    };
    assert_eq!(reading.bytes, probe.bytes);
    app.adopt_probed_capacity(reading, wgpu::Backend::BrowserWebGpu);

    assert_eq!(app.gpu_probe.bytes(), Some(4032 << 20));
    // The probe answered for the GPU; the page heap's declared ceiling
    // rides along from the bracket's presumption.
    assert_eq!(
        app.capacity(),
        Capacity {
            host_bytes: Some(1 << 30),
            ..Capacity::probed(4032 << 20)
        }
    );
    assert_eq!(
        app.capacity().allowance(),
        3024 << 20,
        "three quarters of a probed figure"
    );
    assert!(
        budget_line(&app).contains(", cap 4032 2, probe 4, balloon "),
        "{}",
        budget_line(&app)
    );
    assert_eq!(
        resolve(&app.device_profile),
        class_rung,
        "the probe moved no profile field; `resolve` reads none of it",
    );
    // **The probe answered for the GPU, and on the GPU the six loops fit at
    // the class rung**: the history the 288 MiB presumption halved is back at
    // its fourteen frames, and no GPU rung is taken. The page heap is still
    // the bracket's 1 GiB — the probe says nothing about it — and six loops'
    // decoded volumes, 6 x 14 x 64 MiB = 5376 MiB of `loop_scans_host`, do
    // not fit its 768 MiB allowance at any host rung, so the three host rungs
    // (the margin twice, the tiles) are taken and the scene holds there. The
    // two axes are two walls, and this is the probe moving exactly one.
    let probed = fit(&six, &app.device_profile, &app.capacity(), GRID_BYTES);
    assert_eq!(
        probed.loop_render_budget, class_rung.loop_render_budget,
        "six two-hour loops fit a 3024 MiB allowance with every frame",
    );
    assert!(
        presumed.loop_render_budget < class_rung.loop_render_budget,
        "the 288 MiB presumption had halved the history: {presumed:?}",
    );
    assert_eq!(probed.steps_back, 3, "the three host rungs, no GPU rung");
    assert_eq!(
        squallar_device_profile::budget::Budgets {
            steps_back: 0,
            tile_whole_zoom: false,
            overlay_oversample_percent: 150,
            ..probed
        },
        class_rung,
        "a host wall moved something other than the host rungs",
    );
    assert_eq!(
        squallar_device_profile::fit::over(&six, &probed, &app.capacity(), GRID_BYTES),
        (false, true),
        "the GPU fits; the page heap cannot hold six loops' volumes at any rung",
    );
    assert!(squallar_device_profile::fit::every_host_rung_at_its_stop(
        &probed,
        &app.device_profile.limits
    ));
    assert_eq!(app.fit_scene(&six), probed);

    // Once: a second report, larger, changes nothing.
    app.adopt_probed_capacity(a_probe_of(8 << 30), wgpu::Backend::BrowserWebGpu);
    assert_eq!(app.gpu_probe.bytes(), Some(4032 << 20));
    assert!(
        budget_line(&app).contains(", cap 4032 2, probe 4, balloon "),
        "{}",
        budget_line(&app)
    );
}

/// A figure the probe reached at its own bound prints `probe 5` beside the
/// same `cap N 2`: the reader can tell a floor from a refusal off the level
/// line alone, without the once-only `, capped`.
#[test]
fn a_capped_probe_prints_five_beside_its_figure() {
    let mut app = web_app(TestBridge::web());
    app.adopt_probed_capacity(
        ProbedCapacity {
            bytes: 8 << 30,
            failed_at: None,
            steps: 8,
            elapsed_ms: 1900,
            capped: true,
        },
        wgpu::Backend::BrowserWebGpu,
    );
    assert!(
        budget_line(&app).contains(", cap 8192 2, probe 5, balloon "),
        "{}",
        budget_line(&app)
    );
}

/// Pressure still lowers a probed capacity: `held_to` applies on this arm as
/// on the other two.
#[test]
fn a_probed_capacity_is_still_held_to_what_pressure_taught() {
    let mut app = web_app(TestBridge::web());
    app.adopt_probed_capacity(a_probe_of(4032 << 20), wgpu::Backend::BrowserWebGpu);
    app.session_capacity = Some(1024 << 20);
    assert_eq!(app.capacity().gpu_bytes, 1024 << 20);
    assert_eq!(app.capacity().source, CapacitySource::Probed);
}

/// **A probe on a native profile is ignored.** The readers answer there and
/// the probe never runs; a figure handed to a native profile anyway changes
/// no capacity — presumed stays presumed, and a measured card stays measured.
#[test]
fn a_probe_on_a_native_profile_is_ignored() {
    let mut app = headless(TestBridge::desktop());
    assert_eq!(app.device_profile.platform, Platform::Native);
    let before = app.capacity();
    assert_eq!(before.source, CapacitySource::Presumed);

    app.adopt_probed_capacity(a_probe_of(4032 << 20), wgpu::Backend::Vulkan);

    assert_eq!(
        app.gpu_probe.bytes(),
        Some(4032 << 20),
        "recorded, and spent nowhere"
    );
    assert_eq!(app.capacity(), before);
    // The line says what happened: a probe reported (`probe 4`) and the
    // capacity in force is still the presumption (`cap 3840 0`). No native
    // bridge produces this pair; the double is what makes the arm reachable.
    assert!(
        budget_line(&app).contains(", cap 3840 0, probe 4, balloon "),
        "{}",
        budget_line(&app)
    );

    // Pure, over every arm the guard names.
    let mut native = app.device_profile;
    assert_eq!(
        capacity_with_probe(&native, Some(1 << 30)).source,
        CapacitySource::Presumed
    );
    native.class = DeviceClass::Discrete;
    native.vram_bytes = Some(24 << 30);
    assert_eq!(
        capacity_with_probe(&native, Some(1 << 30)),
        Capacity::measured(24 << 30, None),
        "a measured card outranks any probe",
    );
    let mut web = native;
    web.platform = Platform::Web;
    web.class = DeviceClass::Unknown;
    web.vram_bytes = None;
    web.limits = BudgetLimits::WASM;
    assert_eq!(
        capacity_with_probe(&web, Some(1 << 30)),
        Capacity {
            host_bytes: Some(1 << 30),
            ..Capacity::probed(1 << 30)
        },
        "a web profile the driver would not class is the one arm the probe fills, \
         and the page heap's declared ceiling rides along",
    );
    assert_eq!(
        capacity_with_probe(&web, None),
        Capacity::presumed(&BudgetLimits::WASM),
        "no figure yet is the presumption",
    );
    web.class = DeviceClass::Software;
    assert_eq!(
        capacity_with_probe(&web, Some(1 << 30)),
        Capacity::presumed(&BudgetLimits::WASM),
        "a rasteriser's allowance is not this app's picture",
    );
}

/// The one line the probe prints: integers, ASCII, the backend by name, and
/// the two bounds a reader needs to see whether the figure is the device's or
/// the probe's own.
#[test]
fn the_gpu_probe_line_reads_as_pinned() {
    let probe = ProbedCapacity {
        bytes: 4032 << 20,
        failed_at: Some(8128 << 20),
        steps: 7,
        elapsed_ms: 812,
        capped: false,
    };
    assert_eq!(
        gpu_probe_line(&probe, wgpu::Backend::BrowserWebGpu),
        "gpu probe: 4032 MiB ok, failed at 8128 MiB, 7 steps, 812 ms, backend BrowserWebGpu"
    );
    let capped = ProbedCapacity {
        bytes: 8 << 30,
        failed_at: None,
        steps: 8,
        elapsed_ms: 1900,
        capped: true,
    };
    let line = gpu_probe_line(&capped, wgpu::Backend::BrowserWebGpu);
    assert_eq!(
        line,
        "gpu probe: 8192 MiB ok, failed at none, 8 steps, 1900 ms, backend BrowserWebGpu, capped"
    );
    assert!(line.is_ascii());
}

/// The tick asks the bridge with the live adapter's backend and folds the
/// answer through the tested seam; the fold re-fits through the checked one.
#[test]
fn the_tick_folds_the_probe_through_the_tested_seam() {
    let render = include_str!("../app_render.rs");
    let tick = render
        .split_once("fn report_frame_telemetry(&mut self)")
        .and_then(|(_, rest)| rest.split_once("\n    }"))
        .map(|(body, _)| body)
        .expect("`report_frame_telemetry` is still a method on `App`");
    assert!(
        tick.contains("self.poll_gpu_probe()"),
        "the telemetry tick no longer asks after the probe",
    );

    let app = include_str!("../app.rs");
    let poll = app
        .split_once("fn poll_gpu_probe(&mut self)")
        .and_then(|(_, rest)| rest.split_once("\n    }"))
        .map(|(body, _)| body)
        .expect("`poll_gpu_probe` is still a method on `App`");
    assert!(
        poll.contains("state.adapter.get_info().backend"),
        "`poll_gpu_probe` no longer names the live adapter's backend",
    );
    assert!(
        poll.contains("self.platform.gpu_probe_report(backend)"),
        "`poll_gpu_probe` no longer asks the bridge",
    );
    assert!(
        poll.contains("self.gpu_probe_settled = report.is_settled()"),
        "`poll_gpu_probe` no longer stops asking once the bridge has settled",
    );
    assert!(
        poll.contains("self.adopt_probed_capacity(probe, backend)"),
        "`poll_gpu_probe` no longer folds through `adopt_probed_capacity`",
    );

    let fold = app
        .split_once("fn adopt_probed_capacity(&mut self")
        .and_then(|(_, rest)| rest.split_once("\n    }"))
        .map(|(body, _)| body)
        .expect("`adopt_probed_capacity` is still a method on `App`");
    assert!(
        fold.contains("self.fit_scene(&scene)"),
        "`adopt_probed_capacity` no longer fits the scene through the checked seam",
    );
    assert!(
        fold.contains("self.pool_for_scene(&scene)"),
        "`adopt_probed_capacity` no longer re-sizes the pool",
    );
}

/// **An allocation the browser refuses while the probe holds its textures is
/// the probe's, not a wall of this session's.** Without the guard the path is
/// reachable: `on_pressure(OutOfMemory)` lowers the session presumption to
/// nine tenths of the capacity in force — the 288 MiB presumption while the
/// report is pending — and `held_to` then holds the probed 4032 MiB down to
/// 259 MiB for the whole session. With it: economy is evicted, the rung and
/// the presumption stand, and the figure that lands afterwards is spent in
/// full. A surface loss in the same window is still the application's own,
/// and an out-of-memory after the report is too.
#[test]
fn an_out_of_memory_during_the_probe_window_holds_the_presumption() {
    use squallar_device_profile::constants::ECONOMY_FRACTION;
    let presumed = Capacity::presumed(&BudgetLimits::WASM).gpu_bytes;
    let lowered = |bytes: u64| bytes / ECONOMY_FRACTION.1 * ECONOMY_FRACTION.0;

    let mut app = web_app(TestBridge::web());
    app.gpu_probe = GpuProbeReport::Pending;
    app.on_pressure(Pressure::OutOfMemory);
    assert_eq!(
        app.session_capacity, None,
        "an OOM in the probe window lowered the presumption"
    );
    app.adopt_probed_capacity(a_probe_of(4032 << 20), wgpu::Backend::BrowserWebGpu);
    assert_eq!(
        app.capacity(),
        Capacity {
            host_bytes: Some(1 << 30),
            ..Capacity::probed(4032 << 20)
        },
        "the probed figure is spent in full, not held to a wall the probe built"
    );

    // Any other cause in the same window is the session's own.
    let mut app = web_app(TestBridge::web());
    app.gpu_probe = GpuProbeReport::Pending;
    app.on_pressure(Pressure::SurfaceLost);
    assert_eq!(app.session_capacity, Some(lowered(presumed)));

    // And an OOM once the probe has reported lowers the probed figure, as
    // pressure lowers any capacity in force.
    let mut app = web_app(TestBridge::web());
    app.adopt_probed_capacity(a_probe_of(4032 << 20), wgpu::Backend::BrowserWebGpu);
    app.on_pressure(Pressure::OutOfMemory);
    assert_eq!(app.session_capacity, Some(lowered(4032 << 20)));
    assert_eq!(app.capacity().gpu_bytes, lowered(4032 << 20));
    assert_eq!(app.capacity().source, CapacitySource::Probed);
}

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
            app.loop_pool_state.allocation().balloon_bytes(),
            &app.capacity(),
            app.gpu_probe,
            crate::pressure::LinearMemoryWatch::default(),
            &app.budget_readout,
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
                overlay_pictures: 0,
                picture_px: [0, 0],
                loop_scans_shared: false,
                loop_scans_resident_bytes: 0,
                loop_scans_resident_frames: 0,
                loop_scans_needed: true,
            };
            6
        ],
        tile_sources: Vec::new(),
        mirror_px: [0, 0],
        overlay_grids: Vec::new(),
    };

    let plain = headless(TestBridge::desktop());
    assert!(
        line(&plain).contains(", cap 3840 0, probe 0, balloon "),
        "{}",
        line(&plain)
    );
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
        line(&app).contains(", cap 3840 0, probe 0, balloon "),
        "a discrete class with no reading yet is still the presumption: {}",
        line(&app),
    );
    let presumed = fit(&six, &app.device_profile, &app.capacity(), GRID_BYTES);
    assert_eq!(presumed.loop_render_budget, 18);

    app.adopt_gpu_capacity(Some(reading));

    assert_eq!(app.capacity(), Capacity::measured(24 << 30, None));
    assert!(
        line(&app).contains(", cap 24576 1, probe 0, balloon "),
        "{}",
        line(&app)
    );
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
    assert!(
        line(&app).contains(", cap 3840 0, probe 0, balloon "),
        "{}",
        line(&app)
    );
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
