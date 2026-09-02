//! The GPU's own memory figure reaches the profile and, today, nothing else.

use crate::platform::GpuCapacitySource;
use crate::platform_double::TestBridge;
use squallar_device_profile::budget::resolve;

use super::tests::headless;

/// **What this test does not do**, said first: it does not run
/// `App::update_device_profile`, which needs a live wgpu device, and no test
/// in this crate has one. The reading is handed to the fold that method runs,
/// and the bridge carries the same reading so the shape a driver-backed bridge
/// produces is the shape folded. The scrape below holds the two together.
#[test]
fn an_injected_gpu_capacity_reaches_the_profile_and_moves_no_budget() {
    let reading: (u64, GpuCapacitySource) = (24 << 30, GpuCapacitySource::Measured);
    let mut app = headless(TestBridge::desktop().with_gpu_capacity(reading.0, reading.1));
    let budgets_before = app.budgets;
    assert_eq!(
        app.device_profile.vram_bytes, None,
        "unread before any adapter has answered"
    );

    app.adopt_gpu_capacity(Some(reading));

    assert_eq!(app.device_profile.vram_bytes, Some(24 << 30));
    assert_eq!(
        resolve(&app.device_profile),
        budgets_before,
        "a GPU capacity figure moved a budget; nothing spends it yet, and the \
         field that does lands with its own proof",
    );
    assert_eq!(app.budgets, budgets_before);

    app.adopt_gpu_capacity(None);
    assert_eq!(
        app.device_profile.vram_bytes, None,
        "a reader that stops answering leaves the field unread, not stale"
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
}
