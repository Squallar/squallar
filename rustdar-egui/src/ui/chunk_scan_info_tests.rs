use super::*;
use rustdar_radar::sites::RadarSite;
use rustdar_radar::types::RadarProduct;
use std::collections::HashMap;

fn site() -> RadarSite {
    RadarSite {
        name: "KTLX",
        lat: 35.3,
        lon: -97.3,
        heights: None,
    }
}

fn at(minute: u32) -> chrono::NaiveDateTime {
    chrono::NaiveDate::from_ymd_opt(2026, 7, 28)
        .unwrap()
        .and_hms_opt(18, minute, 0)
        .unwrap()
}

fn info(minute: u32, products: &[(RadarProduct, &[f32])]) -> ScanInfo {
    let mut product_elevations = HashMap::new();
    for (product, angles) in products {
        product_elevations.insert(*product, angles.to_vec());
    }
    ScanInfo {
        site: site(),
        timestamp: at(minute),
        vcp_number: 212,
        available_products: products.iter().map(|(p, _)| *p).collect(),
        product_elevations,
        status: format!("minute {minute}"),
    }
}

fn gui_with(existing: ScanInfo) -> Gui {
    let mut gui = Gui::new();
    let pane = gui.pane_mut(0).expect("a fresh Gui has one pane");
    pane.site = "KTLX".to_string();
    pane.scan_info = Some(existing);
    gui
}

/// The mutation this kills: replacing `product_elevations` wholesale. A
/// volume still being assembled knows only the cuts that have completed, so
/// a replace would shrink the picker to one entry every few seconds and let
/// it regrow — and `get_rendering_params` snaps to the nearest *listed*
/// angle, so every pane would walk up the VCP once per volume.
#[test]
fn a_partial_volume_does_not_shrink_the_tilt_list() {
    let full = info(
        0,
        &[(RadarProduct::Reflectivity, &[0.5, 1.5, 2.4, 3.4, 4.3])],
    );
    let mut gui = gui_with(full);

    // The next volume has only completed its lowest cut.
    gui.apply_chunk_scan_info("KTLX", info(5, &[(RadarProduct::Reflectivity, &[0.5])]));

    let merged = gui.pane(0).unwrap().scan_info.clone().unwrap();
    assert_eq!(
        merged.product_elevations[&RadarProduct::Reflectivity],
        vec![0.5, 1.5, 2.4, 3.4, 4.3],
        "the tilt list shrank to the cuts assembled so far"
    );
    assert_eq!(
        merged.timestamp,
        at(5),
        "but the timestamp is the new volume's"
    );
    assert_eq!(merged.status, "minute 5");
}

/// Level III products and their elevations are accumulated into `ScanInfo`
/// *in place* by `poll_level3_results`, and the chunk feed only refetches
/// them when a volume closes. Replacing would freeze every L3 pane —
/// `get_rendering_params` returns `None` with no elevations — for the rest
/// of the volume.
#[test]
fn a_partial_volume_keeps_the_level3_products_already_registered() {
    let existing = info(
        0,
        &[
            (RadarProduct::Reflectivity, &[0.5, 1.5]),
            (RadarProduct::StormRelativeVelocity, &[0.5]),
        ],
    );
    let mut gui = gui_with(existing);

    gui.apply_chunk_scan_info("KTLX", info(5, &[(RadarProduct::Reflectivity, &[0.5])]));

    let merged = gui.pane(0).unwrap().scan_info.clone().unwrap();
    assert!(
        merged
            .available_products
            .contains(&RadarProduct::StormRelativeVelocity),
        "the Level III product was dropped by a Level II cut completing"
    );
    assert_eq!(
        merged.product_elevations[&RadarProduct::StormRelativeVelocity],
        vec![0.5],
        "and its tilt list with it"
    );
}

/// The counterweight: a tilt the assembling volume reveals for the first
/// time still has to appear, or a new cut in a changed VCP would never be
/// selectable.
#[test]
fn a_newly_seen_tilt_is_added_to_the_list() {
    let mut gui = gui_with(info(0, &[(RadarProduct::Reflectivity, &[0.5])]));
    gui.apply_chunk_scan_info(
        "KTLX",
        info(5, &[(RadarProduct::Reflectivity, &[0.5, 6.4])]),
    );
    assert_eq!(
        gui.pane(0)
            .unwrap()
            .scan_info
            .as_ref()
            .unwrap()
            .product_elevations[&RadarProduct::Reflectivity],
        vec![0.5, 6.4]
    );
}

/// A chunk round happens on its own every few seconds. Taking the spinner
/// down would cancel a manual Refresh still in flight and unblock the
/// auto-poll queued behind it; resetting the backoff would undo exactly the
/// retreat the archive fallback depends on.
#[test]
fn a_chunk_update_leaves_the_fetch_spinner_and_the_backoff_alone() {
    let mut gui = gui_with(info(0, &[(RadarProduct::Reflectivity, &[0.5])]));
    gui.radar.fetching = true;
    gui.auto_poll.on_error();
    let backed_off = gui.auto_poll.interval_secs;
    assert!(backed_off > 60, "the fixture must actually be backed off");

    gui.apply_chunk_scan_info("KTLX", info(5, &[(RadarProduct::Reflectivity, &[0.5])]));

    assert!(
        gui.radar.fetching,
        "a chunk update cancelled a manual fetch's spinner"
    );
    assert_eq!(
        gui.auto_poll.interval_secs, backed_off,
        "a chunk update reset the archive poll's backoff"
    );
}

/// The one behaviour it does share with `set_scan_info_for_site`: with
/// chunks feeding live mode, the first data of a session arrives here.
#[test]
fn the_first_chunk_volume_of_a_session_still_claims_the_initial_zoom() {
    let mut gui = gui_with(info(0, &[(RadarProduct::Reflectivity, &[0.5])]));
    gui.initial_zoom_set = false;
    gui.apply_chunk_scan_info("KTLX", info(5, &[(RadarProduct::Reflectivity, &[0.5])]));
    assert!(gui.initial_zoom_set);
}

/// A pane on another site is not touched.
#[test]
fn a_chunk_update_only_reaches_its_own_site() {
    let mut gui = gui_with(info(0, &[(RadarProduct::Reflectivity, &[0.5])]));
    gui.pane_mut(0).unwrap().site = "KOUN".to_string();
    gui.apply_chunk_scan_info("KTLX", info(5, &[(RadarProduct::Reflectivity, &[0.5])]));
    assert_eq!(
        gui.pane(0).unwrap().scan_info.as_ref().unwrap().timestamp,
        at(0)
    );
}

/// Only panes viewing live are fed, and each site is asked for once.
#[test]
fn live_sites_are_distinct_and_exclude_historic_panes() {
    let mut gui = Gui::new();
    gui.set_pane_count_for_test(3);
    for (idx, site) in ["KTLX", "KTLX", "KOUN"].iter().enumerate() {
        let pane = gui.pane_mut(idx).unwrap();
        pane.site = (*site).to_string();
        pane.viewing_live = true;
    }
    assert_eq!(gui.live_sites(), vec!["KTLX", "KOUN"]);

    gui.pane_mut(2).unwrap().viewing_live = false;
    assert_eq!(gui.live_sites(), vec!["KTLX"]);
}
