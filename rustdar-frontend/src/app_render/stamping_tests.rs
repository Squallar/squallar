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
/// blank but full size: `ColorImage::from_rgba_unmultiplied` checks the
/// buffer against the dimensions it is given.
fn finished(product: RadarProduct, elevation: f32) -> CachedPaneRender {
    CachedPaneRender {
        image_data: Arc::new(vec![0u8; IMAGE_SIZE * IMAGE_SIZE * 4]),
        max_range_km: 230.0,
        value_data: Arc::new(Vec::new()),
        product,
        elevation,
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

    app.apply_render_to_pane(&ctx, 0, &finished(PRODUCT, 0.5));

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

    app.apply_render_to_pane(&ctx, 0, &finished(PRODUCT, 0.5));
    assert_eq!(
        app.gui.pane(0).unwrap().data_time,
        Some(object_time()),
        "precondition: dated from the bucket object",
    );

    app.apply_render_to_pane(&ctx, 0, &finished(RadarProduct::Reflectivity, 0.5));

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
    app.apply_render_to_pane(&ctx, 0, &finished(PRODUCT, 0.5));
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
    app.apply_render_to_pane(&ctx, 0, &finished(RadarProduct::Reflectivity, 0.5));
    assert_eq!(
        app.gui.pane(0).unwrap().stale_image_on_screen(),
        Some((RadarProduct::Reflectivity, 0.5)),
    );
}
