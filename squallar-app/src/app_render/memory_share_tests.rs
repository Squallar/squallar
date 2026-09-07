//! **The user's two memory shares, through the whole chain a real session
//! takes**: restored from disk before the first fit, applied to the capacity
//! ahead of every allowance, and reported back beside what the machine
//! actually allowed.
//!
//! The device-profile crate proves the arithmetic on every capacity arm
//! (`squallar_device_profile::scene::percent_tests`). What is proved here is
//! that the arithmetic is *reached* — that a value the user typed into a
//! settings row survives a restart, gets into `App::capacity` before the
//! ladder reads it, and comes back out in the readout the row paints from.

use crate::app::App;
use crate::app::tests::headless;
use crate::platform_double::TestBridge;
use squallar_device_profile::scene::{PoolBinder, PoolPercents};
use squallar_egui::UI_CONFIG_KEY;
use squallar_kv::KvStore;

/// A machine whose OS answers with 16 GiB available. Big enough that halving
/// it is far outside any rounding.
const POOL: u64 = 16 * 1024 * 1024 * 1024;

/// An application opened over a config that names these two shares, on a
/// native bridge whose RAM reader answers and whose GPU reader does not — the
/// shape of every desktop with no readable card, and the arm the RAM control
/// used to do nothing at all on.
fn app_with_shares(percents: PoolPercents) -> App {
    let bridge = TestBridge::desktop().with_available_memory(POOL);
    let store = bridge.store();
    store
        .store(
            UI_CONFIG_KEY,
            &format!(
                r#"{{"pane_count":1,"site":"KTLX","panes":[{{"site":"KTLX"}}],
                    "gpu_memory_percent":{},"system_memory_percent":{}}}"#,
                percents.gpu, percents.host,
            ),
        )
        .expect("the memory store always accepts a write");
    headless(bridge)
}

/// **The setting is in force before the first fit**, not one frame later.
///
/// `App::new` reads it off the restored `Gui` rather than waiting for a
/// `GuiAction`, because the first `fit` runs before any frame does — and the
/// budgets that first round resolves are the ones most likely to be the
/// largest.
#[test]
fn a_restored_share_is_in_force_before_the_first_frame() {
    let app = app_with_shares(PoolPercents { gpu: 50, host: 50 });
    assert_eq!(
        app.memory_percents,
        PoolPercents { gpu: 50, host: 50 },
        "the App opened at the whole pool over a config that asked for half",
    );
}

/// **The RAM share moves `host_allowance()` on a machine with no readable
/// card.** The GPU reader answers nothing there, the capacity falls to the
/// presumed arm carrying the host pool beside it, and this is the arm on
/// which the control has to work or it does nothing at all for a large class
/// of desktops.
#[test]
fn the_ram_share_moves_the_host_allowance_with_no_gpu_reader() {
    let full = app_with_shares(PoolPercents::FULL);
    let cap = full.capacity();
    assert_eq!(
        cap.source,
        squallar_device_profile::scene::CapacitySource::Presumed,
        "precondition: this bridge's GPU reader answered nothing",
    );
    assert_eq!(
        cap.host_bytes,
        Some(POOL),
        "precondition: this bridge's RAM reader did answer",
    );
    let wide = cap
        .host_allowance()
        .expect("a host figure has a host allowance");

    let half = app_with_shares(PoolPercents { gpu: 100, host: 50 });
    let narrow = half
        .capacity()
        .host_allowance()
        .expect("halving keeps the host figure");
    assert_eq!(
        narrow,
        wide / 2,
        "50 % of the host pool left {narrow} B of allowance against {wide} B \
         at the whole pool",
    );
}

/// **100 % is byte-identical to the session every existing user is running**
/// — the whole capacity, not just the figure the slider names. A fresh
/// install, a config written before the setting existed and a reset all land
/// here, so a drift on this arm is a drift for nearly everybody.
#[test]
fn the_default_share_leaves_the_capacity_byte_identical() {
    let defaulted = headless(TestBridge::desktop().with_available_memory(POOL));
    let asked_for_everything = app_with_shares(PoolPercents::FULL);
    assert_eq!(
        defaulted.memory_percents,
        PoolPercents::FULL,
        "an install with nothing written down did not open at neutrality",
    );
    assert_eq!(
        asked_for_everything.capacity(),
        defaulted.capacity(),
        "asking for the whole pool is not the same as never asking",
    );
    // And the two figures the ladder actually reads.
    assert_eq!(
        asked_for_everything.capacity().allowance(),
        defaulted.capacity().allowance(),
    );
    assert_eq!(
        asked_for_everything.capacity().host_allowance(),
        defaulted.capacity().host_allowance(),
    );
}

/// **A change made mid-session reaches the App** through the one wire the
/// design gives it, and is priced from the next capacity read on.
#[test]
fn moving_the_control_mid_session_reaches_the_capacity() {
    let mut app = app_with_shares(PoolPercents::FULL);
    let before = app.capacity();
    app.handle_gui_action(
        squallar_egui::actions::GuiAction::SetMemoryPercents(PoolPercents { gpu: 25, host: 25 }),
        None,
    );
    let after = app.capacity();
    assert_eq!(
        after.host_bytes,
        before.host_bytes.map(|bytes| bytes / 4),
        "the action did not reach the capacity's host figure",
    );
    assert_eq!(
        after.gpu_bytes,
        before.gpu_bytes / 4,
        "the action did not reach the capacity's GPU figure",
    );
}

/// **The readout says what was asked for, what is in force, and which term is
/// holding it there** — the three things a bare percentage cannot say, and
/// the mitigation for a user who lowered this months ago and forgot.
#[test]
fn the_readout_names_the_request_the_figure_in_force_and_the_binding_term() {
    let mut app = app_with_shares(PoolPercents { gpu: 40, host: 40 });
    app.frame_telemetry_said = None;
    app.report_frame_telemetry();

    let gpu = app.budget_readout.gpu;
    assert_eq!(
        gpu.requested_percent,
        Some(40),
        "the request is not reported"
    );
    assert_eq!(
        gpu.effective_percent,
        Some(40),
        "nothing else was binding, so the figure in force is the request",
    );
    assert_eq!(
        gpu.binder,
        PoolBinder::UserPercent,
        "the user's own setting is the term in force and is not named",
    );
    assert!(
        !gpu.recovering,
        "a pool that has never been squeezed claims to be recovering",
    );

    let host = app
        .budget_readout
        .host
        .expect("this bridge has a host pool");
    assert_eq!(host.requested_percent, Some(40));
    assert_eq!(host.effective_percent, Some(40));
    assert_eq!(host.binder, PoolBinder::UserPercent);
}

/// **At the whole pool the hardware is what is binding**, and the readout
/// says so rather than crediting a setting that took nothing.
#[test]
fn a_share_that_takes_nothing_leaves_the_hardware_named() {
    let mut app = app_with_shares(PoolPercents::FULL);
    app.frame_telemetry_said = None;
    app.report_frame_telemetry();

    let gpu = app.budget_readout.gpu;
    assert_eq!(gpu.requested_percent, Some(100));
    assert_eq!(gpu.effective_percent, Some(100));
    assert_eq!(
        gpu.binder,
        PoolBinder::Hardware,
        "a setting that lowered nothing was credited with the limit",
    );
}

/// **A governor under the user's share is named as the governor**, and named
/// as recoverable when its ceiling is actually on its way back.
///
/// The GPU side is the control here: this test squeezes only the host, so the
/// card's governor holds no step and must not borrow the host side's word.
#[test]
fn a_governor_under_the_users_share_is_named_and_its_recovery_is_reported() {
    let mut app = app_with_shares(PoolPercents { gpu: 80, host: 80 });
    // The page heap's governor, stepped down under the share the user allows.
    let ceiling = app.capacity().host_bytes.expect("a host figure") / 2;
    app.capacity_modulation = squallar_device_profile::scene::Modulation {
        gpu_ceiling: None,
        host_ceiling: Some(ceiling),
    };
    app.frame_telemetry_said = None;
    app.report_frame_telemetry();

    let host = app
        .budget_readout
        .host
        .expect("this bridge has a host pool");
    assert_eq!(
        host.binder,
        PoolBinder::Governor,
        "a modulation under the user's share was credited to the user",
    );
    assert_eq!(
        host.requested_percent,
        Some(80),
        "the request is still what the user asked for, not what they got",
    );
    assert_eq!(
        host.effective_percent,
        Some(40),
        "80 % of the pool, halved again by the governor, is 40 % of the pool",
    );
    assert!(
        !host.recovering,
        "nothing has been banked toward a promotion, so nothing is recovering",
    );
    assert_eq!(
        app.budget_readout.gpu.binder,
        PoolBinder::UserPercent,
        "the host pool's governor reached the GPU pool's verdict",
    );
    assert!(
        !app.budget_readout.gpu.recovering,
        "the card's governor holds no step, so nothing of its ceiling is on \
         its way back",
    );
}

/// **A ceiling on its way back up says so**, through the real recovery state
/// rather than a flag set for the test: a ceiling that is climbing reads
/// differently from one that is stuck, and "memory pressure" alone reads like
/// a wall.
///
/// The GPU pool is the control. Its governor is squeezed by GPU pressure and
/// nothing else, and this test squeezes only the page heap, so it must not
/// borrow this word.
#[test]
fn a_ceiling_with_readings_banked_toward_a_promotion_reads_as_recovering() {
    let mut app = app_with_shares(PoolPercents { gpu: 80, host: 80 });
    let host = app.capacity().host_bytes.expect("a host figure");
    // A real squeeze, so the level, the dwell and the bank are the shipped
    // ones — `held` is only ever non-zero on a squeezed session.
    let ceiling = app.host_recovery.squeeze(Some(host), host / 2);
    app.capacity_modulation = squallar_device_profile::scene::Modulation {
        gpu_ceiling: None,
        host_ceiling: ceiling,
    };
    assert_eq!(app.host_recovery.held(), 0, "a squeeze banks nothing");

    // One qualifying reading, short of the dwell: banked, not yet promoted.
    assert!(
        !app.host_recovery.observe(true),
        "precondition: one reading is short of the dwell, so no step is back",
    );
    assert_eq!(app.host_recovery.held(), 1, "the reading was not banked");

    app.frame_telemetry_said = None;
    app.report_frame_telemetry();
    let pool = app
        .budget_readout
        .host
        .expect("this bridge has a host pool");
    assert_eq!(pool.binder, PoolBinder::Governor);
    assert!(
        pool.recovering,
        "a ceiling with a reading banked toward its promotion reads as a wall",
    );
    assert!(
        !app.budget_readout.gpu.recovering,
        "the GPU pool borrowed the host pool's recovery, though its own \
         governor was never squeezed",
    );
}
