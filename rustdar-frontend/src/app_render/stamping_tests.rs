use super::*;
use crate::platform_double::TestBridge;
use nexrad_level3::model::{Level3Message, MessageHeader, ProductDescriptionBlock};
use rustdar_radar::level3::{Level3Product, ProductStamp};
use rustdar_radar::types::{RadarProduct, ScanInfo};

/// A radar whose Level III objects the pane below is showing.
pub(super) const SITE: &str = "KMPX";
/// The product carried through — any Level III product will do, and
/// storm-relative velocity no longer is one.
const PRODUCT: RadarProduct = RadarProduct::EchoTops;

/// The smallest Level III object `nearest_tilt` will consider: it reads
/// the elevation off the PDB and nothing else.
pub(super) fn tilt(elevation_tenths: i16, key: &str) -> Level3Product {
    Level3Product {
        message: Level3Message {
            header: MessageHeader {
                message_code: 135,
                date_of_message: 20661,
                time_of_message: 7108,
                message_length: 0,
                source_id: 0,
                destination_id: 0,
                number_of_blocks: 3,
            },
            pdb: ProductDescriptionBlock {
                block_divider: -1,
                latitude: 44.849,
                longitude: -93.565,
                height: 1000,
                product_code: 135,
                operational_mode: 2,
                vcp: 212,
                sequence_number: 0,
                volume_scan_number: 39,
                volume_scan_date: 20661,
                volume_scan_time: 7108,
                generation_date: 20661,
                generation_time: 7108,
                product_specific_1: 0,
                product_specific_2: 0,
                elevation_number: 1,
                product_specific_3: elevation_tenths,
                thresholds: [0u16; 16],
                product_specific_47_53: [0i16; 7],
                version: 0,
                spot_blank: 0,
                symbology_offset: 60,
                graphic_offset: 0,
                tabular_offset: 0,
            },
            symbology: None,
        },
        stamp: ProductStamp::from_key(key),
        // No render in these tests, so nothing decodes them.
        bytes: std::sync::Arc::new(Vec::new()),
    }
}

/// A finished render, as `poll_render_results` builds one. The pixels are
/// blank but full size, and already converted: the unmultiply moved to the
/// render thread, so what reaches a pane is a `ColorImage`.
fn finished(product: RadarProduct, elevation: f32) -> CachedPaneRender {
    let side = rustdar_radar::types::IMAGE_SIZE;
    CachedPaneRender {
        image: Arc::new(egui::ColorImage::from_rgba_unmultiplied(
            [side, side],
            &vec![0u8; side * side * 4],
        )),
        max_range_km: 230.0,
        value_data: Arc::new(Vec::new()),
        product,
        elevation,
        nyquist_ms: None,
        melting_layer_source: None,
    }
}

/// The volume the fixture pane has loaded, deliberately **not** the time in
/// the Level III key below: a pane stamped with the wrong one of the two is
/// then a wrong value rather than a coincidence.
fn volume_time() -> chrono::NaiveDateTime {
    chrono::NaiveDate::from_ymd_opt(2026, 7, 26)
        .unwrap()
        .and_hms_opt(1, 48, 0)
        .unwrap()
}

/// The time `MPX_EET_2026_07_26_01_55_52` carries — seven minutes after the
/// volume, which is what a bucket object that lagged a volume looks like.
fn object_time() -> chrono::NaiveDateTime {
    chrono::NaiveDate::from_ymd_opt(2026, 7, 26)
        .unwrap()
        .and_hms_opt(1, 55, 52)
        .unwrap()
}

/// An `App` with one pane on [`SITE`], far enough along that
/// `apply_render_to_pane` will not bail out of it: the pane needs scan info
/// for the site coordinates and the dispatcher needs a slot for the pane.
pub(super) fn app_showing_site() -> crate::app::App {
    let mut app = crate::app::tests::headless(TestBridge::desktop());
    let site = rustdar_radar::sites::get_radar_site(SITE)
        .expect("KMPX is a real radar")
        .clone();
    app.gui.pane_mut(0).unwrap().site = SITE.to_string();
    app.gui.set_scan_info_for_pane(
        0,
        ScanInfo {
            site,
            site_source: rustdar_radar::site_position::SitePositionSource::Table,
            site_position: None,
            timestamp: volume_time(),
            vcp_number: 212,
            available_products: vec![PRODUCT],
            product_elevations: std::collections::HashMap::new(),
            status: String::new(),
        },
    );
    app.render.ensure_pane_count(1);
    app
}

/// Placing an image also dates it, with the time of the data *behind that
/// image*.
///
/// `latest_key` falls back to the previous UTC day, so a site that went down
/// yesterday serves an object most of a day old while the Level II scan line
/// beside it looks current. The data line is the only thing that says so, and
/// nothing between the render arriving and the pane being drawn would notice
/// this call going missing — the pane would simply keep the time it last had.
#[test]
fn a_placed_render_dates_the_pane_it_lands_on() {
    let ctx = egui::Context::default();
    let mut app = app_showing_site();
    app.render.cache_level3(
        "EET".to_string(),
        SITE.to_string(),
        tilt(5, "MPX_EET_2026_07_26_01_55_52"),
    );

    app.apply_render_to_pane(
        &ctx,
        0,
        &finished(PRODUCT, 0.5),
        &mut PlanViewUploads::default(),
    );

    assert_eq!(
        app.gui.pane(0).unwrap().data_time,
        Some(object_time()),
        "a Level III pane must report its own object's time, not the volume's",
    );

    // …and the image really did land, so the assertion above is about a
    // frame the user would be looking at rather than an early return.
    let pane = app.gui.pane_mut(0).unwrap();
    assert!(
        pane.overlay_cache_mut(rustdar_overlays::render::overlay_state::OverlayKind::Radar)
            .current
            .is_some(),
        "precondition: no texture was placed at all",
    );
}

/// Switching datasource replaces the time rather than leaving the old one.
///
/// The assignment is unconditional for this reason: leaving the Level III
/// object's time in place would caption a field derived from the volume with
/// the age of one it has nothing to do with. And the replacement is the
/// volume's own time, not nothing — a product whose age line disappears is a
/// product the user can identify as coming from somewhere else, which is the
/// asymmetry this line no longer has.
#[test]
fn switching_datasource_redates_the_pane_rather_than_undating_it() {
    let ctx = egui::Context::default();
    let mut app = app_showing_site();
    app.render.cache_level3(
        "EET".to_string(),
        SITE.to_string(),
        tilt(5, "MPX_EET_2026_07_26_01_55_52"),
    );

    app.apply_render_to_pane(
        &ctx,
        0,
        &finished(PRODUCT, 0.5),
        &mut PlanViewUploads::default(),
    );
    assert_eq!(
        app.gui.pane(0).unwrap().data_time,
        Some(object_time()),
        "precondition: dated from the bucket object",
    );

    app.apply_render_to_pane(
        &ctx,
        0,
        &finished(RadarProduct::Reflectivity, 0.5),
        &mut PlanViewUploads::default(),
    );

    assert_eq!(
        app.gui.pane(0).unwrap().data_time,
        Some(volume_time()),
        "a volume-derived product reports the volume's time — the same line, \
             filled in the same way",
    );
}

/// Placing an image also records **what it depicts**, so a pane can tell when
/// its pixels are not the selection its labels describe.
///
/// Written into the texture's own `RadarTextureMeta`, which is what makes
/// `PaneState::stale_image_on_screen` impossible to leave behind: the pair is
/// placed together and dropped together. Nothing between the render arriving
/// and the pane being drawn would notice this assignment going missing — the
/// pane would simply never report a mismatch, and would go on captioning one
/// product's image with another's name, which is the defect.
///
/// Both datasources, in both directions, from the one call: the product on the
/// render is the only thing that differs, so a Level II and a Level III image
/// cannot be described differently. This is also the contract
/// `InputHarness::place_radar_image` imitates.
#[test]
fn a_placed_render_describes_what_it_depicts() {
    let ctx = egui::Context::default();
    let mut app = app_showing_site();
    assert!(
        PRODUCT.is_level3() && !RadarProduct::Reflectivity.is_level3(),
        "one product from each datasource",
    );

    // A Level III image under a Level II selection.
    app.gui.pane_mut(0).unwrap().selected_product = RadarProduct::Reflectivity;
    app.apply_render_to_pane(
        &ctx,
        0,
        &finished(PRODUCT, 0.5),
        &mut PlanViewUploads::default(),
    );
    assert_eq!(
        app.gui.pane(0).unwrap().stale_image_on_screen(),
        Some((PRODUCT, 0.5)),
        "the placed image's own product and sweep, reported so the pane can \
             say the label is ahead of the pixels",
    );

    // The matching render lands: nothing to report.
    app.gui.pane_mut(0).unwrap().selected_product = PRODUCT;
    assert_eq!(
        app.gui.pane(0).unwrap().stale_image_on_screen(),
        None,
        "the image is the selection now",
    );

    // And the other way round — a Level II image under a Level III selection,
    // through the same call.
    app.apply_render_to_pane(
        &ctx,
        0,
        &finished(RadarProduct::Reflectivity, 0.5),
        &mut PlanViewUploads::default(),
    );
    assert_eq!(
        app.gui.pane(0).unwrap().stale_image_on_screen(),
        Some((RadarProduct::Reflectivity, 0.5)),
    );
}

/// A long-range render lands on a pane at its own size, and the overlay entry
/// says so.
///
/// `apply_render_to_pane` used to name `IMAGE_SIZE` three times: once to
/// convert the buffer and twice to describe the texture. The conversion has
/// moved to the render thread, and the two descriptions now come off the
/// picture itself — which is the property worth pinning, because a texture
/// uploaded at 4096 and described as 2048 would be placed on a quarter of the
/// ground its gates were painted onto, with the hover reading the wrong pixel
/// and nothing anywhere to notice.
#[test]
fn a_long_range_render_is_placed_at_the_size_it_was_rendered_at() {
    let side = crate::constants::LONG_RANGE_IMAGE_SIZE;
    // The same limit the device gate is read from, told to egui the way
    // `egui_winit::State::new` tells it: `Context::load_texture` asserts the
    // image against `max_texture_side`, and a bare `Context` defaults it to
    // the 2048 WebGL2 floor. The two are one number in the shipped app —
    // `AppState::long_range_raster_ok` and this both come off
    // `device.limits().max_texture_dimension_2d` — so a fixture that let them
    // disagree would be modelling a state the host cannot reach.
    let ctx = egui::Context::default();
    ctx.begin_pass(egui::RawInput {
        max_texture_side: Some(side),
        ..Default::default()
    });
    let mut app = app_showing_site();

    let render = CachedPaneRender {
        image: std::sync::Arc::new(egui::ColorImage::from_rgba_unmultiplied(
            [side, side],
            &vec![0u8; side * side * 4],
        )),
        // The extent that put it there: a 417 km TDWR long-range cut.
        max_range_km: 417.0,
        value_data: std::sync::Arc::new(vec![f32::NAN; side * side]),
        product: PRODUCT,
        elevation: 0.5,
        // KTLX's 0.5° Doppler cut's own declaration, so the assertion below
        // reads a number the fixture states rather than a default that would
        // pass against a placement that dropped it. A WSR-88D's and not the
        // TDWR's whose range the extent above came from: a TDWR declares
        // `nyquist_velocity = 0` on every cut, which `DeclaredNyquist::declare`
        // refuses, so it arrives here with nothing to stamp.
        nyquist_ms: Some(23.84),
        melting_layer_source: None,
    };
    app.apply_render_to_pane(&ctx, 0, &render, &mut PlanViewUploads::default());

    let pane = app.gui.pane_mut(0).unwrap();
    let cache = pane.overlay_cache_mut(rustdar_overlays::render::overlay_state::OverlayKind::Radar);
    let placed = cache
        .current
        .as_ref()
        .expect("the long-range render must have been placed");
    assert_eq!(
        (placed.width, placed.height),
        (side as u32, side as u32),
        "the overlay entry describes the texture at a size the render did not \
         produce",
    );
    assert_eq!(
        placed.radar_meta.as_ref().map(|m| m.max_range_km),
        Some(417.0),
        "the ground the texture is placed on is the render's own extent",
    );
    assert_eq!(
        placed.radar_meta.as_ref().and_then(|m| m.nyquist_ms),
        Some(23.84),
        "the fold limit of the cut behind these pixels travels with them; \
         without it a velocity pane can say nothing about where its own \
         picture wraps",
    );
    // And the pane kept a copy at the same size, so a resume re-uploads the
    // picture rather than a differently shaped one.
    assert_eq!(
        app.render.pane_render[0]
            .cached_render
            .as_ref()
            .map(|c| c.image.size),
        Some([side, side]),
    );
}

/// A suspend and resume puts the picture back exactly as it was, fold limit
/// included.
///
/// The restore path rebuilds the overlay entry from the pane's kept copy and
/// from nothing else — the volume behind the pixels may have been evicted by
/// then, and on a device that was backgrounded for an hour it certainly has
/// been. A field that reached the texture on the render path but not on this
/// one gives a pane that answers a question before suspend and stops answering
/// it after, with the same picture on the glass either way.
#[test]
fn a_resume_puts_back_the_fold_limit_it_took_down() {
    let ctx = egui::Context::default();
    let mut app = app_showing_site();
    let render = CachedPaneRender {
        nyquist_ms: Some(26.42),
        melting_layer_source: None,
        ..finished(PRODUCT, 0.5)
    };
    app.apply_render_to_pane(&ctx, 0, &render, &mut PlanViewUploads::default());

    // What a surface loss does: the overlay entry goes, the pane's kept copy
    // stays. `restore_cached_render` is what runs on the way back.
    {
        let pane = app.gui.pane_mut(0).unwrap();
        let cache =
            pane.overlay_cache_mut(rustdar_overlays::render::overlay_state::OverlayKind::Radar);
        cache.current = None;
    }
    app.restore_cached_render(&ctx);

    let pane = app.gui.pane_mut(0).unwrap();
    let cache = pane.overlay_cache_mut(rustdar_overlays::render::overlay_state::OverlayKind::Radar);
    let placed = cache
        .current
        .as_ref()
        .expect("the kept copy must have been re-uploaded");
    assert_eq!(
        placed.radar_meta.as_ref().and_then(|m| m.nyquist_ms),
        Some(26.42),
        "the restored image describes the same cut the render did",
    );
}

/// …and the legend at the far end of that wire says so again.
///
/// The other half of the resume claim, asserted where the user would read it:
/// `PaneState::displayed_nyquist_ms` is what the velocity bar's `folds ±N` line
/// and its ±Vny markers are drawn from, so a restore that put the metadata back
/// without the pane answering would give a picture that came back identical
/// with an annotation that did not. Reopening is 1:1 or it is not a reopen.
///
/// Velocity rather than this module's usual product, because it is the only one
/// that folds — the annotation is deliberately silent on every other bar.
#[test]
fn a_resumed_velocity_pane_annotates_the_fold_again() {
    let ctx = egui::Context::default();
    let mut app = app_showing_site();
    app.gui.pane_mut(0).unwrap().selected_product = RadarProduct::Velocity;

    let render = CachedPaneRender {
        nyquist_ms: Some(26.42),
        melting_layer_source: None,
        ..finished(RadarProduct::Velocity, 0.5)
    };
    app.apply_render_to_pane(&ctx, 0, &render, &mut PlanViewUploads::default());
    assert_eq!(
        app.gui.pane(0).unwrap().displayed_nyquist_ms(),
        Some(26.42),
        "precondition: the pane must be annotating its own render before the \
         suspend, or the assertion after it proves nothing",
    );

    {
        let pane = app.gui.pane_mut(0).unwrap();
        let cache =
            pane.overlay_cache_mut(rustdar_overlays::render::overlay_state::OverlayKind::Radar);
        cache.current = None;
    }
    assert_eq!(
        app.gui.pane(0).unwrap().displayed_nyquist_ms(),
        None,
        "a pane whose picture is gone still claimed to know where it folded",
    );

    app.restore_cached_render(&ctx);
    assert_eq!(
        app.gui.pane(0).unwrap().displayed_nyquist_ms(),
        Some(26.42),
        "the picture came back and the annotation did not",
    );
}
