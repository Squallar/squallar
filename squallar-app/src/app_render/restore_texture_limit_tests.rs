//! **What the restore may hand egui, and when it may hand it over.**
//!
//! The restore used to put every pane's plan view back from a CPU copy the
//! pane had kept since its last render, and the copy is what this module was
//! written around: a 4096 px raster against a context that had not yet been
//! told what this device can hold trips `Context::load_texture`'s
//! `debug_assert!` and takes the winit loop, and with it the Activity, down
//! (measured 3 of 3 on the API-34 x86_64 emulator, 2026-08-21, on two bases).
//!
//! The copy is gone (2026-09-07) and with it the deferral that guarded it: a
//! plan view now comes back through `App::dispatch_pane_renders`, from the
//! render cache or from a fresh render, and the only raster this path still
//! uploads is a section pane's cut. What is left here is the pair of
//! properties that keeps that safe — the section can never be the raster that
//! does not fit, and the restore still runs from inside the frame.

/// The restore is an upload, and it is put back before the paint list is
/// built. Both halves matter and they pull in opposite directions: egui only
/// learns this device's texture limit from the `RawInput` `begin_frame` hands
/// it, so the restore cannot run at the moment the rendering state is built;
/// and a picture restored after the layout is a pane that flashes empty for a
/// frame.
#[test]
fn the_restore_runs_from_inside_the_frame_and_not_from_the_state_that_built_it() {
    let app_rs = include_str!("../app.rs");

    assert_eq!(
        app_rs.matches("fn ensure_rendering_state(").count(),
        2,
        "control: this test reads both cfg arms of `ensure_rendering_state`, \
         and it no longer found two",
    );
    assert_eq!(
        app_rs.matches("self.restore_pending = true;").count(),
        2,
        "control: each `ensure_rendering_state` arm must arm the restore, so \
         a zero below cannot be a restore that simply stopped happening",
    );
    assert_eq!(
        app_rs.matches("self.restore_cached_render(").count(),
        0,
        "the restore is called from `app.rs` again. Every call there is \
         outside egui's pass, where the context still reports the 2048 \
         `InputState::default` carries rather than what the adapter said",
    );

    let body = {
        let (_, rest) = include_str!("../app_render.rs")
            .split_once("fn setup_egui_frame(")
            .expect("setup_egui_frame is no longer a method here");
        rest.split_once("\n    }")
            .map(|(body, _)| body)
            .expect("setup_egui_frame has no recognisable body")
    };
    let at = |needle: &str| {
        body.find(needle)
            .unwrap_or_else(|| panic!("{needle} is no longer in setup_egui_frame"))
    };
    // The receiver sits on its own line since WO-4 grew the call a third
    // argument past rustfmt's chain width; the method name alone is still
    // unique in this body.
    let opened = at(".begin_frame(");
    let restore = at("self.restore_cached_render(");
    // Split the way `arch_ratchets.rs` and `gui_seam_ratchet_tests.rs` split
    // theirs, and for their reason: spelled whole, the needle is itself one
    // more App-pokes-Gui occurrence in `squallar-app`, and it would spend a
    // slot of a permanent ceiling that has none. Prose counts too, so this
    // note does not spell it either.
    let laid_out = at(concat!("self.", "gui.", "ui_phased("));
    assert!(
        opened < restore,
        "the restore uploads before `begin_frame` has told egui what this \
         device's textures may be",
    );
    assert!(
        restore < laid_out,
        "the restored picture is put back after the paint list is built, so \
         a resume shows an empty pane for one frame",
    );
}

/// **Why the restore needs no texture-limit guard**, now that a section cut is
/// the only raster it uploads.
///
/// This test used to explain why `widest_raster_to_restore` weighed plan views
/// only. It now holds down something stronger and load-bearing: with the plan
/// views out of that path, nothing the restore hands egui can exceed the
/// smallest limit any context reports, so the deferral was removed rather than
/// kept for a case that can no longer arise. If a section ever grows past that
/// floor this goes red, and the guard has to come back with it.
#[test]
fn a_cross_section_can_never_be_the_raster_that_does_not_fit() {
    let floor = egui::Context::default().input(|i| i.max_texture_side);
    let section = squallar_radar::xsect::SECTION_WIDTH.max(squallar_radar::xsect::SECTION_HEIGHT);
    assert!(
        section <= floor,
        "a cross-section is now {section} px against the {floor} px an \
         egui context reports before it has run a pass. `restore_cached_render` \
         hands it over with no limit check at all, which was safe only while \
         this held — put the deferral back",
    );
    // The other half of the same sentence, so a floor that rose could not make
    // this pass while the restore had grown a raster of its own again.
    let restore = {
        let (_, rest) = include_str!("../app_render.rs")
            .split_once("pub(super) fn restore_cached_render(")
            .expect("restore_cached_render is no longer a method here");
        rest.split_once("\n    }")
            .map(|(body, _)| body)
            .expect("restore_cached_render has no recognisable body")
    };
    assert!(
        restore.contains("restore_section_textures("),
        "control: the restore no longer uploads the section rasters this test \
         is about",
    );
    assert!(
        !restore.contains("load_texture("),
        "the restore mints a texture of its own again. Every raster it uploads \
         has to be one this test's floor covers, and a `load_texture` here is \
         one it does not",
    );
}
