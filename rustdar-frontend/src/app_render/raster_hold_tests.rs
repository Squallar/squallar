//! A pane keeps the picture it has until the next one is whole.
//!
//! `overlay_cache::hold_tests` covers the four ways a hold ends against the slot
//! itself. These cover the wiring around it: which raster the app decides to
//! hold, what a pane shows before its first one, what the swap costs in resident
//! textures, and the two positional facts nothing has a type to carry — that the
//! promotion runs before the frame is laid out, and that a hold keeps the event
//! loop awake.
//!
//! The rasters are small for the reason `radar_texture_sharing_tests` gives:
//! nothing here is timing anything. **How many frames a raster takes to cross is
//! `texture_upload`'s question and `tests/raster_upload_gpu.rs` answers it on a
//! real adapter.** What is being counted here is textures and swaps, and a swap
//! of a small picture is a swap.

use super::*;
use crate::app::tests::n_pane_app;
use rustdar_overlays::render::overlay_state::OverlayKind;
use rustdar_radar::types::RadarProduct;

const SITE: &str = "KTLX";
const TILT: f32 = 0.5;
const SIDE: usize = 4;

/// Pixels whose bytes depend on `seed`, so two rasters are never one buffer.
fn raster(seed: u8) -> Arc<egui::ColorImage> {
    let rgba: Vec<u8> = (0..(SIDE * SIDE) as u8)
        .flat_map(|i| [seed, i.wrapping_mul(17), seed ^ i, 255])
        .collect();
    Arc::new(egui::ColorImage::from_rgba_unmultiplied(
        [SIDE, SIDE],
        &rgba,
    ))
}

/// Aim `pane_idx` at [`SITE`] with a volume that offers [`TILT`].
fn point_at(app: &mut crate::app::App, pane_idx: usize) {
    let radar = rustdar_radar::sites::get_radar_site(SITE)
        .expect("KTLX is a real radar")
        .clone();
    let mut product_elevations = std::collections::HashMap::new();
    product_elevations.insert(RadarProduct::Reflectivity, vec![TILT]);
    let pane = app.gui.pane_mut(pane_idx).expect("pane exists");
    pane.site = SITE.to_string();
    pane.selected_product = RadarProduct::Reflectivity;
    pane.selected_elevation = TILT;
    app.gui.set_scan_info_for_pane(
        pane_idx,
        rustdar_radar::types::ScanInfo {
            site: radar,
            site_source: rustdar_radar::site_position::SitePositionSource::Table,
            site_position: None,
            timestamp: chrono::NaiveDate::from_ymd_opt(2026, 8, 13)
                .unwrap()
                .and_hms_opt(1, 48, 0)
                .unwrap(),
            vcp_number: 212,
            available_products: vec![RadarProduct::Reflectivity],
            product_elevations,
            status: String::new(),
        },
    );
}

/// An app with `n` panes, all on [`SITE`] and all ready to be handed a render.
fn app_with_panes(n: usize) -> crate::app::App {
    let mut app = n_pane_app(n, SITE);
    for idx in 0..n {
        point_at(&mut app, idx);
    }
    app
}

/// A finished render of `image`.
fn render_of(image: Arc<egui::ColorImage>) -> crate::render_dispatch::CachedPaneRender {
    crate::render_dispatch::CachedPaneRender {
        image,
        max_range_km: 230.0,
        hover: Arc::new(rustdar_radar::hover::HoverSource::empty()),
        product: RadarProduct::Reflectivity,
        elevation: TILT,
        nyquist_ms: None,
        melting_layer_source: None,
        storm_motion_source: None,
    }
}

/// Hand pane `idx` a finished render of `image`.
///
/// The memo is created and dropped around the one call, exactly as
/// `poll_render_results` scopes it to a drain. It holds a clone of every handle
/// it minted, so one kept alive across two placements would keep the replaced
/// raster resident and make the residency assertions below unfalsifiable.
fn place(app: &mut crate::app::App, ctx: &egui::Context, idx: usize, image: Arc<egui::ColorImage>) {
    let mut uploads = PlanViewUploads::default();
    app.apply_render_to_pane(ctx, idx, &render_of(image), &mut uploads);
}

/// Whether pane `idx` is waiting on a raster.
fn holding(app: &crate::app::App, idx: usize) -> bool {
    app.gui.pane(idx).expect("pane exists").is_holding_raster()
}

/// The id of the texture pane `idx` is drawing, if it is drawing one.
fn on_screen(app: &crate::app::App, idx: usize) -> Option<egui::TextureId> {
    Some(
        app.gui
            .pane(idx)?
            .overlay_cache(OverlayKind::Radar)?
            .current()?
            .texture
            .id(),
    )
}

/// A pane's first raster goes up as it arrives; every one after it waits.
///
/// The whole rule, in the order a session meets it. The exception is not a
/// special case bolted on: the hold exists to keep a *complete* picture from
/// being replaced by a partial one, and a pane with no picture has nothing to
/// keep. Holding there would trade the whole upload — 117 ms for the widest cut,
/// 1.4 s for the six panes of a resume — for an empty map, which is the one
/// thing worse than a picture arriving in strips.
#[test]
fn the_first_raster_arrives_and_the_second_waits() {
    let ctx = egui::Context::default();
    let mut app = app_with_panes(1);

    place(&mut app, &ctx, 0, raster(1));
    let first = on_screen(&app, 0).expect("a pane with no picture shows the one arriving");
    assert!(
        !holding(&app, 0),
        "the first raster has nothing to hold for"
    );

    place(&mut app, &ctx, 0, raster(2));
    assert_eq!(
        on_screen(&app, 0),
        Some(first),
        "the pane swapped onto a raster whose pixels had not all arrived",
    );
    assert!(holding(&app, 0));

    app.deliver_held_rasters();
    assert_ne!(
        on_screen(&app, 0),
        Some(first),
        "the pane never swapped onto the raster it was holding",
    );
    assert!(!holding(&app, 0));
}

/// Handing a pane the raster it is already holding does not start a second one.
///
/// The tilt-independent products draw one picture at every tilt, so a tilt click
/// on one of them is a cache hit that re-describes rather than re-uploads. Doing
/// that *during* a hold has to reuse the handle already crossing — a second
/// `load_texture` would be a second copy of the same pixels, and the completion
/// the pane then waited on would be for an id the first hold had abandoned.
#[test]
fn re_describing_a_raster_that_is_still_arriving_reuses_it() {
    let ctx = egui::Context::default();
    let mut app = app_with_panes(1);

    place(&mut app, &ctx, 0, raster(1));
    let arriving = raster(2);
    place(&mut app, &ctx, 0, Arc::clone(&arriving));
    let live = ctx.tex_manager().read().num_allocated();

    // The same buffer again, as a cache hit hands it back.
    place(&mut app, &ctx, 0, Arc::clone(&arriving));
    assert_eq!(
        ctx.tex_manager().read().num_allocated(),
        live,
        "a re-description of the raster already crossing minted a second copy",
    );
    assert!(holding(&app, 0), "and it is still held rather than shown");

    app.deliver_held_rasters();
    assert!(!holding(&app, 0));
}

/// Handing a pane the raster it is already showing swaps nothing and waits for
/// nothing.
///
/// The other half of the buffer-identity path, and the one that would deadlock:
/// the handle is already whole, so nothing is going to deliver it a second time.
/// A hold here would stand until some other render replaced it.
#[test]
fn re_describing_the_raster_on_screen_does_not_start_a_hold() {
    let ctx = egui::Context::default();
    let mut app = app_with_panes(1);
    let shown = raster(1);

    place(&mut app, &ctx, 0, Arc::clone(&shown));
    let first = on_screen(&app, 0).expect("placed");

    place(&mut app, &ctx, 0, Arc::clone(&shown));
    assert!(
        !holding(&app, 0),
        "a pane began waiting on pixels that were already on the GPU, and \
         nothing was ever going to tell it they had arrived",
    );
    assert_eq!(on_screen(&app, 0), Some(first), "and kept the same texture");
}

/// A hold is a second resident raster, and the swap gives it back.
///
/// The price, asserted rather than reasoned about. It is a *duration* rather
/// than a peak: replacing a texture has always meant both generations existing
/// at once for the frame between `load_texture` and the `free_texture` after
/// `queue.submit()` — see the note in `App::apply_render_to_pane` on why a
/// replaced handle can simply be dropped. The hold stretches that window from
/// one frame to the frames the bands take, and what must not change is what is
/// left standing afterwards: one raster per pane, as before.
#[test]
fn a_hold_costs_a_second_raster_and_gives_it_back_on_the_swap() {
    let ctx = egui::Context::default();
    let mut app = app_with_panes(1);
    let live = || ctx.tex_manager().read().num_allocated();

    place(&mut app, &ctx, 0, raster(1));
    // A pass first, so the font atlas is allocated and the count below moves
    // only with the rasters. Dropping a handle also only *queues* a free —
    // egui retires the id at `end_pass`, which is where
    // `PreparedFrame::textures_to_free` comes from — so every reading here is
    // taken with a pass boundary behind it.
    let _ = ctx.end_pass();
    let one = live();

    place(&mut app, &ctx, 0, raster(2));
    let _ = ctx.end_pass();
    assert_eq!(
        live(),
        one + 1,
        "a hold is a second full-size raster, and this is where the budget \
         question is asked",
    );

    app.deliver_held_rasters();
    let _ = ctx.end_pass();
    assert_eq!(
        live(),
        one,
        "the picture the swap replaced is still allocated, so residency grows \
         by a raster per swap for the life of the session",
    );
}

/// Six panes on one sweep hold one texture between them, and one answer swaps
/// all six.
///
/// `PlanViewUploads` already made the *upload* one per raster rather than one
/// per pane; the hold must not undo that by minting a handle each. On the
/// desktop arm that is the difference between one 206.75 MiB second copy and
/// six of them — 207 MiB against 1241 MiB.
#[test]
fn panes_sharing_a_sweep_hold_one_texture_between_them() {
    let ctx = egui::Context::default();
    let mut app = app_with_panes(6);

    let first = raster(1);
    {
        let mut uploads = PlanViewUploads::default();
        for idx in 0..6 {
            app.apply_render_to_pane(&ctx, idx, &render_of(Arc::clone(&first)), &mut uploads);
        }
    }
    app.deliver_held_rasters();
    let _ = ctx.end_pass();
    let before = ctx.tex_manager().read().num_allocated();

    let second = raster(2);
    {
        let mut uploads = PlanViewUploads::default();
        for idx in 0..6 {
            app.apply_render_to_pane(&ctx, idx, &render_of(Arc::clone(&second)), &mut uploads);
        }
    }
    assert_eq!(
        ctx.tex_manager().read().num_allocated(),
        before + 1,
        "six panes holding one sweep minted more than one texture for it",
    );
    assert!((0..6).all(|idx| holding(&app, idx)));

    app.deliver_held_rasters();
    assert!(
        (0..6).all(|idx| !holding(&app, idx)),
        "one delivery did not swap every pane served from that raster",
    );
}

/// A renderer rebuild lets go of every hold rather than waiting on it.
///
/// The one way a hold could last for ever: the ids belong to an `egui::Context`
/// that no longer exists, so `TextureUploads::is_delivered` answers `false`
/// about them and will never answer anything else. A hold left standing would
/// keep `any_raster_held` true, and the event loop would run at refresh rate for
/// the rest of the session asking a question whose answer cannot change.
#[test]
fn a_renderer_rebuild_releases_every_hold() {
    let ctx = egui::Context::default();
    let mut app = app_with_panes(1);

    place(&mut app, &ctx, 0, raster(1));
    place(&mut app, &ctx, 0, raster(2));
    assert!(
        app.gui.any_raster_held(),
        "precondition: a hold is standing"
    );

    // What `ensure_rendering_state` reaches once a new `AppState` exists, with
    // a fresh context standing in for the one it just built.
    app.restore_cached_render(&egui::Context::default());
    assert!(
        !app.gui.any_raster_held(),
        "a hold survived the renderer that was going to deliver it, so nothing \
         will ever end it and the loop never parks again",
    );
}

/// The swap happens before the frame is laid out, and after nothing.
///
/// Positional, and nothing has a type that could carry it. Promoting after
/// `Gui::ui` would build this frame's paint list from the previous picture and
/// show the new one on the next frame instead — one extra frame of latency on
/// every raster, for nothing. Promoting after the pollers would ask about a hold
/// on the frame it was staged, when the answer can only be no.
#[test]
fn a_delivered_raster_is_promoted_before_the_frame_is_laid_out() {
    let (_, body) = include_str!("../app_render.rs")
        .split_once("fn setup_egui_frame(")
        .expect("setup_egui_frame is no longer a method here");
    let promote = body
        .find("self.promote_uploaded_rasters(")
        .expect("setup_egui_frame no longer promotes delivered rasters");
    let poll = body
        .find("self.poll_render_results(")
        .expect("setup_egui_frame no longer polls renders");
    let laid_out = body
        .find("self.gui.ui(")
        .expect("setup_egui_frame no longer lays out a frame");
    assert!(
        promote < laid_out,
        "a raster delivered on the previous frame is promoted after this \
         frame's paint list is built, so every swap costs an extra frame",
    );
    assert!(
        promote < poll,
        "the promotion runs after the poller that stages holds, so a hold is \
         asked about on the frame it was staged, when the answer can only be no",
    );
}

/// A held raster keeps the event loop awake until it is shown.
///
/// The other positional fact, and the one whose absence is silent. The app runs
/// on `ControlFlow::Wait`. `end_pass_and_upload` asks for a zero `repaint_delay`
/// while bands are still pending, which covers the upload but **not** the swap —
/// on the frame the last band lands nothing is pending any more. Without this
/// term a raster would finish crossing and then sit unshown behind the previous
/// sweep until some unrelated input woke the loop.
#[test]
fn a_held_raster_keeps_the_frame_loop_awake() {
    let source = include_str!("../app.rs");
    let redraw = source
        .find("fn handle_redraw(")
        .map(|at| &source[at..])
        .expect("handle_redraw is gone from app.rs");
    let first_rearm = redraw
        .find("notify_redraw(&self.window)")
        .expect("handle_redraw no longer re-arms");
    assert!(
        redraw
            .find("self.gui.any_raster_held()")
            .is_some_and(|at| at < first_rearm),
        "the re-arm has no term for a pane holding a picture, so a raster that \
         finishes uploading with nothing else in flight is not shown until an \
         unrelated input repaints",
    );
}
