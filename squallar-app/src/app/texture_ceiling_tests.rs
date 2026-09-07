//! **The user's texture ceiling reaches the budgets in force, and the default
//! reaches nothing.**
//!
//! The type's own arithmetic is pinned next to it, in
//! `squallar_device_profile::budget`. What is checked here is the wiring: that
//! `adopt_budgets` — the one place `App::budgets` is written — actually applies
//! the setting, and that a session which never touches the control adopts the
//! identical struct it adopted before the control existed.
//!
//! Both arms, because only one of them protects anybody. A cap that binds is
//! the feature; a default that changes nothing byte-for-byte is what every
//! existing user gets, and a regression there would be silent on every machine
//! at once.

use squallar_device_profile::budget::{BudgetLimits, Promotion, TextureCeiling, at_class_rung};

use super::tests::{headless, n_pane_app};
use crate::platform_double::TestBridge;

/// The desktop class rung, which is the widest set of sides any bracket
/// resolves and so the one a ceiling has the most to bind.
fn desktop_rung() -> squallar_device_profile::budget::Budgets {
    at_class_rung(&BudgetLimits::DESKTOP, Promotion::Ceiling)
}

/// **The neutral arm.** A session at the default adopts what it was handed,
/// field for field — `Budgets` is `PartialEq` over all of them, so this is the
/// byte-for-byte claim and not a spot check of the four sides.
#[test]
fn the_default_ceiling_adopts_the_budgets_unchanged() {
    let mut app = headless(TestBridge::desktop());
    let wanted = desktop_rung();

    assert_eq!(
        app.texture_ceiling,
        TextureCeiling::NONE,
        "a fresh app does not start at the neutral posture",
    );

    app.adopt_budgets(wanted);
    assert_eq!(
        app.budgets, wanted,
        "the default ceiling moved a budget on its way into the app",
    );
}

/// **The arm that binds.** The setting reaches `App::budgets` through
/// `adopt_budgets`, and lowers every raster side and nothing else.
#[test]
fn a_lowered_ceiling_reaches_the_budgets_in_force() {
    let mut app = headless(TestBridge::desktop());
    let wanted = desktop_rung();
    let ceiling_px = 1024usize;

    app.texture_ceiling = TextureCeiling::clamped(ceiling_px as u32);
    app.adopt_budgets(wanted);

    assert_ne!(
        app.budgets, wanted,
        "the ceiling was set and the adopted budgets are unchanged",
    );
    for (name, got) in [
        ("image_side_px", app.budgets.image_side_px),
        (
            "long_range_image_side_px",
            app.budgets.long_range_image_side_px,
        ),
        ("loop_image_side_px", app.budgets.loop_image_side_px),
        ("raster_side_ceiling_px", app.budgets.raster_side_ceiling_px),
    ] {
        assert_eq!(
            got, ceiling_px,
            "{name} is {got} px under a {ceiling_px} px user ceiling",
        );
    }

    // Restore the four sides and the struct is the one that went in: the
    // ceiling is a cap on rasters, not a rung of the ladder.
    let mut restored = app.budgets;
    restored.image_side_px = wanted.image_side_px;
    restored.long_range_image_side_px = wanted.long_range_image_side_px;
    restored.loop_image_side_px = wanted.loop_image_side_px;
    restored.raster_side_ceiling_px = wanted.raster_side_ceiling_px;
    assert_eq!(
        restored, wanted,
        "the ceiling moved a budget that is not a raster side",
    );
}

/// **The figure the loop dispatch and its validator both read is the one the
/// ceiling moved.**
///
/// `spawn_loop_render` reads `budgets.loop_image_side_px` once and carries it
/// into the reply's length check; before 2026-09-06 both halves read the
/// compile-time `LOOP_IMAGE_SIZE` and could not disagree. This pins the
/// property that made threading it necessary: lowering the ceiling moves the
/// dispatched side, so a validator still reading the constant would refuse
/// every frame the app asks for.
#[test]
fn the_ceiling_moves_the_side_a_loop_frame_is_dispatched_at() {
    let mut app = headless(TestBridge::desktop());
    app.adopt_budgets(desktop_rung());
    let uncapped = app.budgets.loop_image_side_px;

    app.texture_ceiling = TextureCeiling::clamped(TextureCeiling::FLOOR_PX);
    app.adopt_budgets(desktop_rung());

    assert_eq!(
        app.budgets.loop_image_side_px,
        TextureCeiling::FLOOR_PX as usize,
    );
    assert_ne!(
        app.budgets.loop_image_side_px, uncapped,
        "the dispatched loop side did not move, so nothing needed threading",
    );
    assert_ne!(
        app.budgets.loop_image_side_px,
        squallar_device_profile::constants::LOOP_IMAGE_SIZE,
        "the dispatched side still equals the constant the validator used to \
         read, so this test cannot tell the two apart",
    );
}

/// Raising the ceiling back gives the sides back: the setting is a live cap
/// on whatever was resolved, not a latch that a session has to restart to
/// escape.
#[test]
fn raising_the_ceiling_gives_the_sides_back() {
    let mut app = headless(TestBridge::desktop());
    let wanted = desktop_rung();

    app.texture_ceiling = TextureCeiling::clamped(TextureCeiling::FLOOR_PX);
    app.adopt_budgets(wanted);
    assert_ne!(app.budgets, wanted);

    app.texture_ceiling = TextureCeiling::NONE;
    app.adopt_budgets(wanted);
    assert_eq!(
        app.budgets, wanted,
        "clearing the ceiling did not restore the budgets",
    );
}

/// **The App half of effective-beside-requested**: the readout the settings
/// screen draws its line from carries the ceiling the budgets were composed
/// under, and states the device's own figure as *absent* where no device has
/// answered.
///
/// The caption in `squallar_egui::ui_settings` is a pure function of this
/// pair and is tested there on both arms. What can only be checked here is
/// that the pair is composed at all, that it follows the setting, and that a
/// session with no adapter is not handed an invented side — the failure this
/// one would have is a line that reads confidently and describes nothing.
#[test]
fn the_readout_carries_the_ceiling_it_was_composed_under() {
    let mut app = n_pane_app(1, "KTLX");
    let asked = TextureCeiling::clamped(1024);
    assert_ne!(
        asked,
        TextureCeiling::NONE,
        "precondition: the two postures this test tells apart are one value, \
         so both halves below would pass on any behaviour at all",
    );

    app.set_texture_ceiling(asked);
    tick(&mut app);
    assert!(
        app.budget_readout.generation > 0,
        "no readout was composed, so every figure read off it below is the \
         default rather than an answer",
    );
    assert_eq!(
        app.budget_readout.raster.requested, asked,
        "the readout does not carry the ceiling in force, so the settings \
         line would report a figure the user never chose",
    );
    assert_eq!(
        app.budget_readout.raster.effective_side_px, None,
        "a session with no adapter was given a size in force; absence has to \
         reach the caption as absence, or it prints a confident invention",
    );

    // The known-unequal control: clearing the setting moves the readout with
    // it, so the equality above is following the ceiling rather than sitting
    // on a value that never changes.
    app.set_texture_ceiling(TextureCeiling::NONE);
    tick(&mut app);
    assert_eq!(
        app.budget_readout.raster.requested,
        TextureCeiling::NONE,
        "the readout kept a ceiling the session no longer has",
    );
}

/// One telemetry tick, asked for rather than waited on — the cadence
/// `app_render/budget_readout_cadence_tests.rs` pins, driven the same way.
fn tick(app: &mut crate::app::App) {
    app.frame_telemetry_said = None;
    app.report_frame_telemetry();
}
