use super::*;
use rustdar_radar::fields as radar_fields;
use rustdar_radar::sites::RadarSite;
use rustdar_source::product::FieldId;
use std::collections::HashMap;

/// The radar layer's own field value for an id.
///
/// A [`ScanInfo`](rustdar_radar::types::ScanInfo) is radar's fact about a scan
/// and its tables are keyed by radar's own field, so these fixtures resolve
/// ids through the one door instead of naming the layer's type. A macro rather
/// than a function because the answer's type is the layer's, and a function
/// would have to write it down.
macro_rules! resolve {
    ($id:expr) => {
        rustdar_radar::fields::product_for($id).expect("a registered field")
    };
}

fn site() -> RadarSite {
    RadarSite {
        name: "KTLX",
        network: rustdar_radar::sites::RadarNetwork::of_id("KTLX"),
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

fn info(minute: u32, products: &[(FieldId, &[f32])]) -> ScanInfo {
    let mut product_elevations = HashMap::new();
    for (product, angles) in products {
        product_elevations.insert(resolve!(product), angles.to_vec());
    }
    ScanInfo {
        site: site(),
        site_source: rustdar_radar::site_position::SitePositionSource::Table,
        site_position: None,
        timestamp: at(minute),
        vcp_number: 212,
        available_products: products.iter().map(|(p, _)| resolve!(p)).collect(),
        product_elevations,
        status: format!("minute {minute}"),
    }
}

fn gui_with(existing: ScanInfo) -> Gui {
    let mut gui = Gui::new();
    let pane = gui.pane_mut(0).expect("a fresh Gui has one pane");
    pane.set_site("KTLX".to_string());
    pane.scan_info = Some(existing);
    gui
}

#[test]
fn a_partial_volume_does_not_shrink_the_tilt_list() {
    let full = info(
        0,
        &[(
            radar_fields::known::REFLECTIVITY,
            &[0.5, 1.5, 2.4, 3.4, 4.3],
        )],
    );
    let mut gui = gui_with(full);

    gui.apply(crate::shell_api::GuiEvent::ChunkScanInfo {
        site: "KTLX".to_owned(),
        info: info(5, &[(radar_fields::known::REFLECTIVITY, &[0.5])]),
    });

    let merged = gui.pane(0).unwrap().scan_info.clone().unwrap();
    assert_eq!(
        merged.product_elevations[&resolve!(&radar_fields::known::REFLECTIVITY)],
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

/// Level III products accumulate into `ScanInfo` in place; the chunk feed only
/// refetches them when a volume closes.
#[test]
fn a_partial_volume_keeps_the_level3_products_already_registered() {
    let existing = info(
        0,
        &[
            (radar_fields::known::REFLECTIVITY, &[0.5, 1.5]),
            (radar_fields::known::STORM_RELATIVE_VELOCITY, &[0.5]),
        ],
    );
    let mut gui = gui_with(existing);

    gui.apply(crate::shell_api::GuiEvent::ChunkScanInfo {
        site: "KTLX".to_owned(),
        info: info(5, &[(radar_fields::known::REFLECTIVITY, &[0.5])]),
    });

    let merged = gui.pane(0).unwrap().scan_info.clone().unwrap();
    assert!(
        merged
            .available_products
            .contains(&resolve!(&radar_fields::known::STORM_RELATIVE_VELOCITY)),
        "the Level III product was dropped by a Level II cut completing"
    );
    assert_eq!(
        merged.product_elevations[&resolve!(&radar_fields::known::STORM_RELATIVE_VELOCITY)],
        vec![0.5],
        "and its tilt list with it"
    );
}

/// A tilt the assembling volume reveals for the first time still has to appear.
#[test]
fn a_newly_seen_tilt_is_added_to_the_list() {
    let mut gui = gui_with(info(0, &[(radar_fields::known::REFLECTIVITY, &[0.5])]));
    gui.apply(crate::shell_api::GuiEvent::ChunkScanInfo {
        site: "KTLX".to_owned(),
        info: info(5, &[(radar_fields::known::REFLECTIVITY, &[0.5, 6.4])]),
    });
    assert_eq!(
        gui.pane(0)
            .unwrap()
            .scan_info
            .as_ref()
            .unwrap()
            .product_elevations[&resolve!(&radar_fields::known::REFLECTIVITY)],
        vec![0.5, 6.4]
    );
}

/// How far up the archive poll's failure ladder the radar layer is — the
/// quantity `interval_secs` used to hold, now read off the layer's own retry
/// ledger.
fn archive_failures(gui: &Gui) -> u32 {
    match gui.overlays.fetch_health(&crate::radar_layer::POLL_LAYER) {
        Some(rustdar_source::fetch_policy::FetchHealth::Failing { attempts, .. }) => *attempts,
        _ => 0,
    }
}

/// **A radar error ends the round it belonged to**: the spinner comes down
/// and the failure files against the layer's ladder.
///
/// Both halves are load-bearing and NEITHER was pinned. Since WO-E8d the
/// in-flight flag is the radar layer's own, and `auto_fetch_delay` refuses to
/// schedule a round while one is in flight — so an error that did not end the
/// round would leave the spinner up and wedge the archive poll for the rest
/// of the session. The ladder half was unpinned before this land too: nothing
/// connected a shell-side `GuiEvent::Error` to `FetchRetry`.
///
/// Found by a tamper, not by reading: deleting `end_radar_round` from the
/// `Error` arm was **green** across both packages.
#[test]
fn a_radar_error_ends_the_round_it_belonged_to() {
    // **No product named on purpose.** This test is about a round's end, not
    // about a tilt, and `PRODUCT_IN_EGUI_MAX` is at its floor — a mention
    // added for scenery would have spent the ceiling on nothing.
    let mut gui = gui_with(info(0, &[]));

    gui.apply(crate::shell_api::GuiEvent::Fetching(true));
    assert!(
        gui.fetching(),
        "premise: a round must be in flight to be ended"
    );
    assert_eq!(
        archive_failures(&gui),
        0,
        "premise: the ladder starts clean"
    );

    gui.apply(crate::shell_api::GuiEvent::Error("network down".to_owned()));

    assert!(
        !gui.fetching(),
        "an error left the spinner up, and the archive poll wedged behind it"
    );
    assert!(
        archive_failures(&gui) > 0,
        "an error never reached the layer's retry ladder, so a failing origin \
         is asked again on the next frame instead of being spaced out"
    );
}

/// A chunk round happens on its own every few seconds.
#[test]
fn a_chunk_update_leaves_the_fetch_spinner_and_the_backoff_alone() {
    let mut gui = gui_with(info(0, &[(radar_fields::known::REFLECTIVITY, &[0.5])]));
    gui.end_radar_round(crate::ui::RoundOutcome::Failed("network down"));
    gui.set_radar_round_in_flight(true);
    let backed_off = archive_failures(&gui);
    assert!(backed_off > 0, "the fixture must actually be backed off");

    gui.apply(crate::shell_api::GuiEvent::ChunkScanInfo {
        site: "KTLX".to_owned(),
        info: info(5, &[(radar_fields::known::REFLECTIVITY, &[0.5])]),
    });

    assert!(
        gui.fetching(),
        "a chunk update cancelled a manual fetch's spinner"
    );
    assert_eq!(
        archive_failures(&gui),
        backed_off,
        "a chunk update reset the archive poll's backoff"
    );
}

/// With chunks feeding live mode, the first data of a session arrives here.
#[test]
fn the_first_chunk_volume_of_a_session_still_claims_the_initial_zoom() {
    let mut gui = gui_with(info(0, &[(radar_fields::known::REFLECTIVITY, &[0.5])]));
    gui.initial_zoom_set = false;
    gui.apply(crate::shell_api::GuiEvent::ChunkScanInfo {
        site: "KTLX".to_owned(),
        info: info(5, &[(radar_fields::known::REFLECTIVITY, &[0.5])]),
    });
    assert!(gui.initial_zoom_set);
}

/// ...but only when a pane is actually on that site.
#[test]
fn a_chunk_volume_no_pane_is_watching_does_not_claim_the_initial_zoom() {
    let mut gui = gui_with(info(0, &[(radar_fields::known::REFLECTIVITY, &[0.5])]));
    gui.initial_zoom_set = false;
    gui.apply(crate::shell_api::GuiEvent::ChunkScanInfo {
        site: "KABX".to_owned(),
        info: info(5, &[(radar_fields::known::REFLECTIVITY, &[0.5])]),
    });
    assert!(
        !gui.initial_zoom_set,
        "a volume for a site no pane is on spent the latch"
    );
}

/// A pane on another site is not touched.
#[test]
fn a_chunk_update_only_reaches_its_own_site() {
    let mut gui = gui_with(info(0, &[(radar_fields::known::REFLECTIVITY, &[0.5])]));
    gui.pane_mut(0).unwrap().set_site("KOUN".to_string());
    gui.apply(crate::shell_api::GuiEvent::ChunkScanInfo {
        site: "KTLX".to_owned(),
        info: info(5, &[(radar_fields::known::REFLECTIVITY, &[0.5])]),
    });
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
        pane.set_site((*site).to_string());
        pane.viewing_live = true;
    }
    assert_eq!(gui.live_sites(), vec!["KTLX", "KOUN"]);

    gui.pane_mut(2).unwrap().viewing_live = false;
    assert_eq!(gui.live_sites(), vec!["KTLX"]);
}
