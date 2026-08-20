use super::super::App;
use super::super::tests::headless;
use crate::platform_double::TestBridge;
use rustdar_radar::chunks::CutSelection;
use rustdar_radar::types::{RadarProduct, ScanInfo};

pub(super) fn show(app: &mut App, product: RadarProduct, selected: f32, available: &[f32]) {
    show_on(app, 0, product, selected, available);
}

pub(super) fn show_on(
    app: &mut App,
    idx: usize,
    product: RadarProduct,
    selected: f32,
    available: &[f32],
) {
    let pane = app.gui.pane_mut(idx).unwrap();
    pane.set_site("KTLX".to_string());
    pane.viewing_live = true;
    pane.set_selected_product(product);
    pane.set_selected_elevation(selected);
    let mut product_elevations = std::collections::HashMap::new();
    product_elevations.insert(product, available.to_vec());
    pane.scan_info = Some(ScanInfo {
        site_source: rustdar_radar::site_position::SitePositionSource::Table,
        site_position: None,
        site: rustdar_radar::sites::RadarSite {
            name: "KTLX",
            lat: 35.3,
            lon: -97.3,
            heights: None,
        },
        timestamp: chrono::NaiveDate::from_ymd_opt(2026, 7, 28)
            .unwrap()
            .and_hms_opt(0, 0, 0)
            .unwrap(),
        vcp_number: 212,
        available_products: vec![product],
        product_elevations,
        status: String::new(),
    });
}

#[test]
fn every_pane_shape_takes_the_whole_feed() {
    let mut app = headless(TestBridge::desktop());
    for &product in RadarProduct::all() {
        show(&mut app, product, 0.5, &[0.5, 1.5, 4.0]);
        assert_eq!(
            app.cut_selection_for("KTLX"),
            CutSelection::All,
            "{product:?}: a live site's feed was narrowed; the merge base \
                 can no longer roll forward and an opened section waits on cuts \
                 the feed skipped",
        );
    }

    for kind in [
        rustdar_radar::types::RenderView::CrossSection,
        rustdar_radar::types::RenderView::Volume,
    ] {
        show(&mut app, RadarProduct::Reflectivity, 0.5, &[0.5, 1.5, 4.0]);
        app.gui.pane_mut(0).unwrap().set_view(kind);
        assert_eq!(app.cut_selection_for("KTLX"), CutSelection::All, "{kind:?}");
    }

    assert_eq!(app.cut_selection_for("KOUN"), CutSelection::All);
}
