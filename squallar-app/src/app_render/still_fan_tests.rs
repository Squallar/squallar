//! **A still plan view that came back a plane**: what the pane holds, what it
//! no longer mints, and what it still says about the picture.
//!
//! The premise this seam overturned is worth stating, because the tests below
//! are its consequences. A still pane's dispatch set `values_wanted` and read
//! that as a request for the raster's *representation* — so the one term that
//! actually sets the desktop working-set floor, the `side²` cells-and-image
//! pair the plan-view render allocates, was tied to a pane wanting a number
//! under the pointer. A plane's numbers are its codes; the field beside one
//! carries them a byte a gate now, so the flag asks for information the polar
//! surface has.

use super::*;
use crate::app::tests::{drain_uploads, n_pane_app};
use squallar_radar::types::RadarProduct;
use squallar_source::id::known;

const SITE: &str = "KTLX";
const TILT: f32 = 0.5;

/// A painter that draws nothing — enough to be installed, which is all
/// `App::plan_surface` and the draw fork ask of one.
struct NoOpFanPainter;

impl squallar_egui::radar_fan::RadarFanPainter for NoOpFanPainter {
    fn payload(
        &self,
        _draw: squallar_egui::radar_fan::FanDraw<'_>,
    ) -> Option<std::sync::Arc<dyn std::any::Any + Send + Sync>> {
        None
    }
}

/// One well-formed fan payload.
fn fan() -> Arc<squallar_egui::radar_fan::FanSweep> {
    let sweep = Arc::new(squallar_egui::radar_fan::FanSweep {
        field: squallar_radar::fields::known::REFLECTIVITY,
        radials: 4,
        gates: 8,
        codes: std::sync::Arc::new(vec![0; 4 * 8]),
        value_table: std::sync::Arc::new(vec![0.0; squallar_egui::radar_fan::LUT_ENTRIES]),
        level_offsets: vec![0],
        lut_rgba: vec![0; squallar_egui::radar_fan::LUT_BYTES],
        edges: vec![[0.0, 90.0], [90.0, 180.0], [180.0, 270.0], [270.0, 360.0]],
        geometry: squallar_egui::radar_fan::FanGeometry {
            site_lat: 35.33,
            site_lon: -97.27,
            first_gate_slant_km: 2.125,
            gate_interval_slant_km: 0.25,
            elevation_deg: Some(0.5),
            reach_gates: 8,
            reach_km: 2.5,
            first_gate_km: 2.0,
            earth_radius_km: squallar_geo::EARTH_RADIUS_KM,
            effective_radius_km: squallar_radar::beam::RE_EFF_KM,
        },
    });
    assert!(sweep.is_well_formed(), "the fixture describes itself");
    sweep
}

fn raster() -> Arc<egui::ColorImage> {
    Arc::new(egui::ColorImage::filled([4, 4], egui::Color32::RED))
}

/// Aim pane 0 at a site carrying `product` at [`TILT`], far enough along that
/// `apply_render_to_pane` will not bail out.
fn point_at(app: &mut crate::app::App, product: RadarProduct) {
    let radar = squallar_radar::sites::get_radar_site(SITE)
        .expect("KTLX is a real radar")
        .clone();
    let mut product_elevations = std::collections::HashMap::new();
    product_elevations.insert(product, vec![TILT]);
    let pane = app.gui.pane_mut(0).expect("pane exists");
    pane.set_site(SITE.to_string());
    pane.set_selected_product(squallar_radar::fields::spec(product).id.clone());
    pane.set_selected_elevation(TILT);
    app.gui
        .apply(squallar_egui::shell_api::GuiEvent::ScanInfoForPane {
            pane_idx: 0,
            info: squallar_radar::types::ScanInfo {
                site: radar,
                site_source: squallar_radar::site_position::SitePositionSource::Table,
                site_position: None,
                timestamp: chrono::NaiveDate::from_ymd_opt(2026, 8, 11)
                    .unwrap()
                    .and_hms_opt(18, 30, 0)
                    .unwrap(),
                vcp_number: 212,
                available_products: vec![product],
                product_elevations,
                status: String::new(),
            },
        });
}

/// Deliver a finished still render carrying `surface` to pane 0.
fn deliver(
    app: &mut crate::app::App,
    ctx: &egui::Context,
    product: RadarProduct,
    surface: crate::channels::StillSurface,
) {
    app.channels
        .render_sender
        .send(crate::channels::RenderResponse {
            rendered: Some(crate::channels::RenderedImage {
                surface,
                max_range_km: 230.0,
                hover: Arc::new(squallar_radar::hover::HoverSource::empty()),
                nyquist_ms: Some(26.5),
                melting_layer_source: None,
                storm_motion: None,
            }),
            product,
            elevation: TILT,
            generation: app.render.render_generation,
            pane_idx: 0,
            speculative_for: None,
        })
        .expect("the receiver lives on the App");
    app.poll_render_results(ctx);
}

/// **The producer asks for the surface the draw can show, and for nothing
/// else** — one field, one answer, read by the still dispatch, the
/// adjacent-tilt pre-render and every loop frame.
///
/// A polar surface has no fallback: the raster it would fall back to is the
/// allocation the representation exists not to make, so a plane built where
/// nothing can draw one is a pane with no radar on it rather than a slow one.
///
/// TAMPER: return `Fan` unconditionally and the first arm goes red.
#[test]
fn the_surface_asked_for_follows_the_renderer_that_is_installed() {
    let ctx = egui::Context::default();
    let mut app = n_pane_app(1, SITE);
    let _ = &ctx;
    assert_eq!(
        app.plan_surface(),
        squallar_radar::jobs::PlanSurface::Raster,
        "a build with no fan renderer must ask for the raster",
    );
    app.radar_fan_painter = Some(Arc::new(NoOpFanPainter));
    assert_eq!(
        app.plan_surface(),
        squallar_radar::jobs::PlanSurface::Fan,
        "a build that can draw a fan must be allowed to ask for one",
    );
}

/// **A still reply that came back a plane mints no texture**, and that absence
/// is the whole of the saving rather than a detail of it: a pane holding both
/// representations of one picture costs strictly more than the raster it
/// replaced.
///
/// The raster arm of the same delivery is the control immediately below.
/// Without it, "no texture" would also be true of a seam that had stopped
/// accepting still results at all.
///
/// TAMPER: fall through to the upload after placing the fan and the first
/// `is_none` goes red.
#[test]
fn a_still_fan_is_placed_and_no_texture_is_minted() {
    let ctx = egui::Context::default();
    let mut app = n_pane_app(1, SITE);
    point_at(&mut app, RadarProduct::Reflectivity);
    let sweep = fan();

    let _ = drain_uploads(&ctx);
    deliver(
        &mut app,
        &ctx,
        RadarProduct::Reflectivity,
        crate::channels::StillSurface::Fan(Arc::clone(&sweep)),
    );
    let uploads = drain_uploads(&ctx);
    assert!(
        uploads.is_empty(),
        "a polar still pane uploaded {} texture(s); it is holding both \
         representations of one picture and costs more than the raster did",
        uploads.len(),
    );

    let pane = app.gui.pane(0).expect("pane exists");
    let placed = pane
        .still_radar_fan()
        .expect("a plane reply puts a fan on the pane");
    assert!(
        Arc::ptr_eq(&placed.sweeps[0], &sweep),
        "the payload was rebuilt on the frame thread rather than carried",
    );
    assert!(
        pane.overlay_cache(&known::RADAR)
            .and_then(|c| c.current())
            .is_none(),
        "a polar still pane also minted a texture; it is holding both surfaces",
    );
}

/// The control for the test above: the same delivery carrying pixels does put
/// a texture on the pane and no fan.
#[test]
fn a_still_raster_is_uploaded_and_leaves_no_fan() {
    let ctx = egui::Context::default();
    let mut app = n_pane_app(1, SITE);
    point_at(&mut app, RadarProduct::Reflectivity);

    let _ = drain_uploads(&ctx);
    deliver(
        &mut app,
        &ctx,
        RadarProduct::Reflectivity,
        crate::channels::StillSurface::Raster(raster()),
    );
    assert_eq!(
        drain_uploads(&ctx).len(),
        1,
        "control: a raster reply must still upload exactly one texture, or the \
         zero the polar arm asserts is a zero about a seam that stopped working",
    );

    let pane = app.gui.pane(0).expect("pane exists");
    assert!(
        pane.overlay_cache(&known::RADAR)
            .and_then(|c| c.current())
            .is_some(),
        "control: a raster reply must still put a texture on the pane",
    );
    assert!(pane.still_radar_fan().is_none());
}

/// **One layer, one picture**: a pane handed each surface in turn holds only
/// the newer one.
///
/// Left standing, the retired surface draws under the fresh one and holds its
/// buffer for as long as it does so — a texture on the way in, host bytes on
/// the way back.
#[test]
fn the_two_surfaces_retire_each_other() {
    let ctx = egui::Context::default();
    let mut app = n_pane_app(1, SITE);
    point_at(&mut app, RadarProduct::Reflectivity);

    deliver(
        &mut app,
        &ctx,
        RadarProduct::Reflectivity,
        crate::channels::StillSurface::Raster(raster()),
    );
    let _ = drain_uploads(&ctx);
    deliver(
        &mut app,
        &ctx,
        RadarProduct::Reflectivity,
        crate::channels::StillSurface::Fan(fan()),
    );
    let _ = drain_uploads(&ctx);
    {
        let pane = app.gui.pane(0).expect("pane exists");
        assert!(pane.still_radar_fan().is_some());
        assert!(
            pane.overlay_cache(&known::RADAR)
                .and_then(|c| c.current())
                .is_none(),
            "the raster the fan replaced is still on the pane",
        );
    }

    deliver(
        &mut app,
        &ctx,
        RadarProduct::Reflectivity,
        crate::channels::StillSurface::Raster(raster()),
    );
    let _ = drain_uploads(&ctx);
    let pane = app.gui.pane(0).expect("pane exists");
    assert!(
        pane.still_radar_fan().is_none(),
        "the fan the raster replaced is still on the pane",
    );
    assert!(
        pane.overlay_cache(&known::RADAR)
            .and_then(|c| c.current())
            .is_some(),
    );
}

/// **What the pane says about the picture does not depend on which surface it
/// is.**
///
/// Every annotation a pane draws around its plan view — the extent the range
/// ring is struck on, the readout under the pointer, the fold limit the
/// velocity ramp is caveated by, which product and sweep the pixels really are
/// — is a property of the render and not of its shape. They are answered
/// through one accessor for that reason; four copies of a cache read would
/// have left a fan pane reporting a retired raster's provenance.
///
/// TAMPER: make `radar_meta_on_screen` skip the fan arm and every assertion
/// here goes red at once.
#[test]
fn a_fan_pane_reports_the_same_provenance_a_raster_pane_would() {
    let ctx = egui::Context::default();
    let mut app = n_pane_app(1, SITE);
    point_at(&mut app, RadarProduct::Velocity);

    deliver(
        &mut app,
        &ctx,
        RadarProduct::Velocity,
        crate::channels::StillSurface::Fan(fan()),
    );
    let _ = drain_uploads(&ctx);

    let pane = app.gui.pane(0).expect("pane exists");
    let meta = pane
        .radar_meta_on_screen()
        .expect("a fan pane answers for its own picture");
    assert_eq!(meta.max_range_km, 230.0);
    assert_eq!(meta.elevation, TILT);
    assert_eq!(meta.product, squallar_radar::fields::known::VELOCITY);
    assert_eq!(
        pane.displayed_nyquist_ms(),
        Some(26.5),
        "the fold limit the ramp is caveated by is lost on a fan pane",
    );
    assert!(
        pane.stale_image_on_screen().is_none(),
        "a fan pane showing exactly its selection reported itself stale",
    );
}

/// **The still dispatch posts the surface the caller asked for, and asks for
/// the numbers on either of them.**
///
/// The defect this seam removed lived here: the job was built with
/// `PlanSurface::Raster` written into it, beside a comment saying a plane
/// could not answer a hover. Both halves are asserted together because the
/// pairing is the claim — a fan request that dropped `values_wanted` would put
/// a pane's readout out rather than free the pair, and a `values_wanted` that
/// still forced the raster is exactly what was there before.
///
/// The job is read back through `to_bytes`/`from_bytes`, so what is asserted
/// is what a real worker boundary would carry.
///
/// TAMPER: write either flag as a literal in `spawn_level2_render` and one of
/// the arms below goes red.
#[test]
fn a_still_dispatch_carries_the_asked_for_surface_and_wants_the_numbers() {
    for asked in [
        squallar_radar::jobs::PlanSurface::Raster,
        squallar_radar::jobs::PlanSurface::Fan,
    ] {
        let (posted, _worker) = super::one_render_per_sweep_tests::recorder();
        let mut app = crate::app::tests::headless(crate::platform_double::TestBridge::desktop());
        let site = squallar_radar::sites::get_radar_site(SITE)
            .expect("KTLX is a real radar")
            .clone();
        app.gui.pane_mut(0).unwrap().set_site(SITE.to_string());
        app.render.ensure_pane_count(1);

        let (sender, _rx) = std::sync::mpsc::channel();
        app.render.spawn_level2_render(
            0,
            &crate::render_dispatch::RenderParams {
                product: RadarProduct::Reflectivity,
                elevation: TILT,
                lat: site.lat,
                lon: site.lon,
            },
            SITE,
            super::one_render_per_sweep_tests::sample_scan(),
            &squallar_radar::nyquist::DeclaredNyquist::empty(),
            chrono::NaiveDateTime::default(),
            asked,
            sender,
            None,
        );

        let bytes = posted.lock().unwrap().clone();
        assert_eq!(bytes.len(), 1, "{asked:?}: one still frame was dispatched");
        let request = squallar_worker::offload::JobRequest::from_bytes(&bytes[0])
            .expect("a job this build posted decodes");
        let plan = request
            .job
            .downcast_ref::<squallar_radar::jobs::RadarPlanJob>()
            .unwrap_or_else(|| panic!("a plan-view dispatch posted {request:?}"));
        assert_eq!(
            plan.surface, asked,
            "{asked:?}: the still dispatch overrode the surface its caller chose",
        );
        assert!(
            plan.values_wanted,
            "{asked:?}: a still pane stopped asking for the numbers its readout reads",
        );
    }
}
