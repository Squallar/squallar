use super::super::App;
use super::super::tests::headless;
use crate::platform_double::TestBridge;
use rustdar_radar::chunks::CutSelection;
use rustdar_radar::types::{RadarProduct, ScanInfo};

/// Re-point the pane an existing app already has, so a per-product sweep
/// does not stand a `wgpu` instance up once per variant.
pub(super) fn show(app: &mut App, product: RadarProduct, selected: f32, available: &[f32]) {
    show_on(app, 0, product, selected, available);
}

/// [`show`] against a chosen pane, for the multi-pane cases.
pub(super) fn show_on(
    app: &mut App,
    idx: usize,
    product: RadarProduct,
    selected: f32,
    available: &[f32],
) {
    let pane = app.gui.pane_mut(idx).unwrap();
    pane.site = "KTLX".to_string();
    pane.viewing_live = true;
    pane.selected_product = product;
    pane.selected_elevation = selected;
    let mut product_elevations = std::collections::HashMap::new();
    product_elevations.insert(product, available.to_vec());
    pane.scan_info = Some(ScanInfo {
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

/// **Every pane shape takes the whole feed.** The narrowing this module
/// used to pin — tilt lists for single-sweep panes, `All` forced by
/// whole-volume products, section/3D pane kinds and active loops — is
/// superseded by the current merged volume: the substrate's premise is
/// that a live site always holds a full, current copy of its data, and a
/// feed that skips cuts breaks it twice over (the overlay misses rungs,
/// and no closed volume is ever whole, so the base never rolls forward).
///
/// The sweep runs the very configurations that used to narrow — most
/// pointedly the single-tilt Reflectivity pane, the case the traffic
/// saving existed for — so reintroducing any narrowing arm fails here on
/// the exact shape it would narrow.
#[test]
fn every_pane_shape_takes_the_whole_feed() {
    use rustdar_egui::pane::PaneKind;

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

    for kind in [PaneKind::CrossSection, PaneKind::Volume] {
        show(&mut app, RadarProduct::Reflectivity, 0.5, &[0.5, 1.5, 4.0]);
        app.gui.pane_mut(0).unwrap().set_kind(kind);
        assert_eq!(app.cut_selection_for("KTLX"), CutSelection::All, "{kind:?}");
    }

    // A site with no pane at all still answers `All`: the answer is a
    // property of the substrate now, not of what is on screen.
    assert_eq!(app.cut_selection_for("KOUN"), CutSelection::All);
}
