use super::*;
use crate::app::tests::n_pane_app;
use squallar_radar::types::RadarProduct;
use squallar_source::id::known;

const SITE: &str = "KTLX";
const TILT: f32 = 0.5;
const SIDE: usize = 4;

fn raster(seed: u8) -> Arc<egui::ColorImage> {
    let rgba: Vec<u8> = (0..(SIDE * SIDE) as u8)
        .flat_map(|i| [seed, i.wrapping_mul(17), seed ^ i, 255])
        .collect();
    Arc::new(egui::ColorImage::from_rgba_unmultiplied(
        [SIDE, SIDE],
        &rgba,
    ))
}

fn point_at(app: &mut crate::app::App, pane_idx: usize) {
    let radar = squallar_radar::sites::get_radar_site(SITE)
        .expect("KTLX is a real radar")
        .clone();
    let mut product_elevations = std::collections::HashMap::new();
    product_elevations.insert(RadarProduct::Reflectivity, vec![TILT]);
    let pane = app.gui.pane_mut(pane_idx).expect("pane exists");
    pane.set_site(SITE.to_string());
    pane.set_selected_product(squallar_radar::fields::known::REFLECTIVITY);
    pane.set_selected_elevation(TILT);
    app.gui
        .apply(squallar_egui::shell_api::GuiEvent::ScanInfoForPane {
            pane_idx,
            info: squallar_radar::types::ScanInfo {
                site: radar,
                site_source: squallar_radar::site_position::SitePositionSource::Table,
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
        });
}

fn app_with_panes(n: usize) -> crate::app::App {
    let mut app = n_pane_app(n, SITE);
    for idx in 0..n {
        point_at(&mut app, idx);
    }
    app
}

fn render_of(image: Arc<egui::ColorImage>) -> crate::render_dispatch::CachedPaneRender {
    crate::render_dispatch::CachedPaneRender {
        image,
        max_range_km: 230.0,
        hover: Arc::new(squallar_radar::hover::HoverSource::empty()),
        product: squallar_radar::types::RadarProduct::Reflectivity,
        elevation: TILT,
        nyquist_ms: None,
        melting_layer_source: None,
        storm_motion: None,
    }
}

fn place(app: &mut crate::app::App, ctx: &egui::Context, idx: usize, image: Arc<egui::ColorImage>) {
    let mut uploads = PlanViewUploads::default();
    app.apply_render_to_pane(ctx, idx, &render_of(image), &mut uploads);
}

fn holding(app: &crate::app::App, idx: usize) -> bool {
    app.gui.pane(idx).expect("pane exists").is_holding_raster()
}

fn on_screen(app: &crate::app::App, idx: usize) -> Option<egui::TextureId> {
    Some(
        app.gui
            .pane(idx)?
            .overlay_cache(&known::RADAR)?
            .current()?
            .texture
            .id(),
    )
}

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

#[test]
fn re_describing_a_raster_that_is_still_arriving_reuses_it() {
    let ctx = egui::Context::default();
    let mut app = app_with_panes(1);

    place(&mut app, &ctx, 0, raster(1));
    let arriving = raster(2);
    place(&mut app, &ctx, 0, Arc::clone(&arriving));
    let live = ctx.tex_manager().read().num_allocated();

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

#[test]
fn a_hold_costs_a_second_raster_and_gives_it_back_on_the_swap() {
    let ctx = egui::Context::default();
    let mut app = app_with_panes(1);
    let live = || ctx.tex_manager().read().num_allocated();

    place(&mut app, &ctx, 0, raster(1));
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

    app.restore_cached_render(&egui::Context::default());
    assert!(
        !app.gui.any_raster_held(),
        "a hold survived the renderer that was going to deliver it, so nothing \
         will ever end it and the loop never parks again",
    );
}

#[test]
fn a_delivered_raster_is_promoted_before_the_frame_is_laid_out() {
    let (_, body) = include_str!("../app_render.rs")
        .split_once("fn setup_egui_frame(")
        .expect("setup_egui_frame is no longer a method here");
    let promote = body
        .find("self.promote_uploaded_rasters(")
        .expect("setup_egui_frame no longer promotes delivered rasters");
    let poll = body
        .find("self.run_frame_pump(PumpPhase::Apply")
        .expect("setup_egui_frame no longer runs the pump's results-apply phase");
    let laid_out = body
        .find("self.gui.ui_phased(")
        .expect("setup_egui_frame no longer lays out a frame");
    assert!(
        promote < laid_out,
        "a raster delivered on the previous frame is promoted after this \
         frame's paint list is built, so every swap costs an extra frame",
    );
    assert!(
        promote < poll,
        "the promotion runs after the results-apply phase whose poller stages \
         holds, so a hold is asked about on the frame it was staged, when the \
         answer can only be no",
    );
}

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
