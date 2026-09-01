use super::*;

/// The side a static plan view reaches on this build. On the desktop and
/// mobile classes that is 4096 — the size the emulator's restore was carrying
/// when it tripped egui's assert.
const OVERSIZED: usize = squallar_device_profile::constants::LONG_RANGE_IMAGE_SIZE;

/// What the API-34 x86_64 emulator's adapter reports for
/// `max_texture_dimension_2d`, measured 2026-08-21 — sixteen times the number a
/// context that has not run a pass admits.
const DEVICE_LIMIT: usize = 32768;

fn cached(side: usize) -> crate::render_dispatch::CachedPaneRender {
    crate::render_dispatch::CachedPaneRender {
        image: Arc::new(egui::ColorImage::from_rgba_unmultiplied(
            [side, side],
            &vec![0u8; side * side * 4],
        )),
        max_range_km: 417.0,
        hover: Arc::new(squallar_radar::hover::HoverSource::empty()),
        product: squallar_radar::types::RadarProduct::Reflectivity,
        elevation: 0.5,
        nyquist_ms: None,
        melting_layer_source: None,
        storm_motion: None,
    }
}

fn app_holding(side: usize) -> crate::app::App {
    let mut app = super::stamping_tests::app_showing_site();
    app.render.pane_render[0].cached_render = Some(cached(side));
    app
}

fn placed(app: &mut crate::app::App) -> Option<(u32, u32)> {
    let pane = app.gui.pane_mut(0)?;
    let cache = pane.overlay_cache_mut(&squallar_source::id::known::RADAR);
    cache.current().map(|entry| (entry.width, entry.height))
}

/// The restore is an upload, and egui only learns this device's texture limit
/// from the `RawInput` `begin_frame` hands it. Running the restore at the
/// moment the rendering state is built hands a fresh context a 4096 px picture
/// against the 2048 `InputState::default` carries.
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
         `InputState::default` carries rather than what the adapter said, and \
         a 4096 px plan view put back there trips `Context::load_texture`'s \
         `debug_assert!` and takes the winit loop down with it",
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
         device's textures may be, which is the whole defect",
    );
    assert!(
        restore < laid_out,
        "the restored picture is put back after the paint list is built, so \
         a resume shows an empty pane for one frame",
    );
}

/// Deferring, not clamping: the picture the user had comes back whole or it
/// comes back next frame, and never at half its size.
#[test]
fn a_raster_the_context_cannot_hold_defers_instead_of_uploading() {
    let ctx = egui::Context::default();
    let admitted = ctx.input(|i| i.max_texture_side);
    assert!(
        OVERSIZED > admitted,
        "premise: a context that has run no pass admits {admitted} px, which \
         this build's {OVERSIZED} px plan view no longer exceeds. The \
         ordering in `setup_egui_frame` may now be unnecessary — re-read it \
         rather than deleting this test",
    );

    let mut app = app_holding(OVERSIZED);
    app.restore_pending = false;
    app.restore_cached_render(&ctx);

    assert_eq!(
        placed(&mut app),
        None,
        "a raster wider than the context admits was handed to it anyway",
    );
    assert!(
        app.restore_pending,
        "the restore gave up on the picture instead of leaving itself to be \
         run again, so the pane stays empty until something unrelated \
         repaints it",
    );
}

/// The other half, and the reason the deferral is safe: once the context has
/// been told the real number, the same raster goes back at the size it was
/// rendered at.
#[test]
fn a_raster_the_context_admits_comes_back_at_the_size_it_was_rendered_at() {
    let ctx = egui::Context::default();
    ctx.begin_pass(egui::RawInput {
        max_texture_side: Some(DEVICE_LIMIT),
        ..Default::default()
    });

    let mut app = app_holding(OVERSIZED);
    app.restore_pending = true;
    app.restore_cached_render(&ctx);

    assert_eq!(
        placed(&mut app),
        Some((OVERSIZED as u32, OVERSIZED as u32)),
        "the restore did not put back the picture the pane had, at the size \
         it had it",
    );
    assert!(
        !app.restore_pending,
        "the restore ran and still asks to be run again, so every frame \
         re-uploads the same picture",
    );
}

/// Why [`super::App::widest_raster_to_restore`] weighs plan views only.
#[test]
fn a_cross_section_can_never_be_the_raster_that_does_not_fit() {
    let floor = egui::Context::default().input(|i| i.max_texture_side);
    let section = squallar_radar::xsect::SECTION_WIDTH.max(squallar_radar::xsect::SECTION_HEIGHT);
    assert!(
        section <= floor,
        "a cross-section is now {section} px against the {floor} px an \
         egui context reports before it has run a pass, so \
         `widest_raster_to_restore` has to weigh sections too — it does not, \
         and a resumed section pane would trip the assert this whole module \
         exists for",
    );
}
