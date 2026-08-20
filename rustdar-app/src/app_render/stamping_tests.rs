use super::*;
use crate::platform_double::TestBridge;
use nexrad_level3::model::{Level3Message, MessageHeader, ProductDescriptionBlock};
use rustdar_radar::level3::{Level3Product, ProductStamp};
use rustdar_radar::types::{RadarProduct, ScanInfo};

pub(super) const SITE: &str = "KMPX";
const PRODUCT: RadarProduct = RadarProduct::EchoTops;

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
        bytes: std::sync::Arc::new(Vec::new()),
    }
}

fn finished(product: RadarProduct, elevation: f32) -> CachedPaneRender {
    let side = rustdar_radar::types::IMAGE_SIZE;
    CachedPaneRender {
        image: Arc::new(egui::ColorImage::from_rgba_unmultiplied(
            [side, side],
            &vec![0u8; side * side * 4],
        )),
        max_range_km: 230.0,
        hover: Arc::new(rustdar_radar::hover::HoverSource::empty()),
        product,
        elevation,
        nyquist_ms: None,
        melting_layer_source: None,
        storm_motion: None,
    }
}

fn volume_time() -> chrono::NaiveDateTime {
    chrono::NaiveDate::from_ymd_opt(2026, 7, 26)
        .unwrap()
        .and_hms_opt(1, 48, 0)
        .unwrap()
}

fn object_time() -> chrono::NaiveDateTime {
    chrono::NaiveDate::from_ymd_opt(2026, 7, 26)
        .unwrap()
        .and_hms_opt(1, 55, 52)
        .unwrap()
}

pub(super) fn app_showing_site() -> crate::app::App {
    let mut app = crate::app::tests::headless(TestBridge::desktop());
    let site = rustdar_radar::sites::get_radar_site(SITE)
        .expect("KMPX is a real radar")
        .clone();
    app.gui.pane_mut(0).unwrap().site = SITE.to_string();
    app.gui
        .apply(rustdar_egui::shell_api::GuiEvent::ScanInfoForPane {
            pane_idx: 0,
            info: ScanInfo {
                site,
                site_source: rustdar_radar::site_position::SitePositionSource::Table,
                site_position: None,
                timestamp: volume_time(),
                vcp_number: 212,
                available_products: vec![PRODUCT],
                product_elevations: std::collections::HashMap::new(),
                status: String::new(),
            },
        });
    app.render.ensure_pane_count(1);
    app
}

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

    let pane = app.gui.pane_mut(0).unwrap();
    assert!(
        pane.overlay_cache_mut(&rustdar_source::id::known::RADAR)
            .current()
            .is_some(),
        "precondition: no texture was placed at all",
    );
}

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
        "precondition: dated from the bucket object — the pane's first raster, \
             which has no predecessor to keep on screen and so goes up at once",
    );

    app.apply_render_to_pane(
        &ctx,
        0,
        &finished(RadarProduct::Reflectivity, 0.5),
        &mut PlanViewUploads::default(),
    );
    assert_eq!(
        app.gui.pane(0).unwrap().data_time,
        Some(object_time()),
        "the pane was redated while it was still showing the previous picture",
    );

    app.deliver_held_rasters();
    assert_eq!(
        app.gui.pane(0).unwrap().data_time,
        Some(volume_time()),
        "a volume-derived product reports the volume's time — the same line, \
             filled in the same way",
    );
}

#[test]
fn a_placed_render_describes_what_it_depicts() {
    let ctx = egui::Context::default();
    let mut app = app_showing_site();
    assert!(
        PRODUCT.is_level3() && !RadarProduct::Reflectivity.is_level3(),
        "one product from each datasource",
    );

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

    app.gui.pane_mut(0).unwrap().selected_product = PRODUCT;
    assert_eq!(
        app.gui.pane(0).unwrap().stale_image_on_screen(),
        None,
        "the image is the selection now",
    );

    app.apply_render_to_pane(
        &ctx,
        0,
        &finished(RadarProduct::Reflectivity, 0.5),
        &mut PlanViewUploads::default(),
    );
    assert_eq!(
        app.gui.pane(0).unwrap().stale_image_on_screen(),
        None,
        "the pane disowned the picture it was still showing, on the strength of \
             one that had not arrived",
    );

    app.deliver_held_rasters();
    assert_eq!(
        app.gui.pane(0).unwrap().stale_image_on_screen(),
        Some((RadarProduct::Reflectivity, 0.5)),
    );
}

#[test]
fn a_long_range_render_is_placed_at_the_size_it_was_rendered_at() {
    let side = rustdar_device_profile::constants::LONG_RANGE_IMAGE_SIZE;
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
        max_range_km: 417.0,
        hover: std::sync::Arc::new(rustdar_radar::hover::HoverSource::empty()),
        product: PRODUCT,
        elevation: 0.5,
        nyquist_ms: Some(23.84),
        melting_layer_source: None,
        storm_motion: None,
    };
    app.apply_render_to_pane(&ctx, 0, &render, &mut PlanViewUploads::default());

    let pane = app.gui.pane_mut(0).unwrap();
    let cache = pane.overlay_cache_mut(&rustdar_source::id::known::RADAR);
    let placed = cache
        .current()
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
    assert_eq!(
        app.render.pane_render[0]
            .cached_render
            .as_ref()
            .map(|c| c.image.size),
        Some([side, side]),
    );
}

#[test]
fn a_resume_puts_back_the_fold_limit_it_took_down() {
    let ctx = egui::Context::default();
    let mut app = app_showing_site();
    let render = CachedPaneRender {
        nyquist_ms: Some(26.42),
        melting_layer_source: None,
        storm_motion: None,
        ..finished(PRODUCT, 0.5)
    };
    app.apply_render_to_pane(&ctx, 0, &render, &mut PlanViewUploads::default());

    {
        let pane = app.gui.pane_mut(0).unwrap();
        let cache = pane.overlay_cache_mut(&rustdar_source::id::known::RADAR);
        cache.clear();
    }
    app.restore_cached_render(&ctx);

    let pane = app.gui.pane_mut(0).unwrap();
    let cache = pane.overlay_cache_mut(&rustdar_source::id::known::RADAR);
    let placed = cache
        .current()
        .expect("the kept copy must have been re-uploaded");
    assert_eq!(
        placed.radar_meta.as_ref().and_then(|m| m.nyquist_ms),
        Some(26.42),
        "the restored image describes the same cut the render did",
    );
}

#[test]
fn a_resumed_velocity_pane_annotates_the_fold_again() {
    let ctx = egui::Context::default();
    let mut app = app_showing_site();
    app.gui.pane_mut(0).unwrap().selected_product = RadarProduct::Velocity;

    let render = CachedPaneRender {
        nyquist_ms: Some(26.42),
        melting_layer_source: None,
        storm_motion: None,
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
        let cache = pane.overlay_cache_mut(&rustdar_source::id::known::RADAR);
        cache.clear();
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
