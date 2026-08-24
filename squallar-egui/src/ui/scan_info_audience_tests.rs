//! **Two scan-info events, two audiences.**
//!
//! `UNLINK_NOTE` makes two promises about a pane that has left the shared
//! clock: parked in the archive it holds its moment, and still live it still
//! follows new scans. They are two different delivery rules, and this is the
//! seam where the shell's one event became two so that both can be true.

use super::*;
use squallar_radar::sites::RadarSite;

fn site_named(name: &'static str) -> RadarSite {
    RadarSite {
        name,
        network: squallar_radar::sites::RadarNetwork::of_id(name),
        lat: 35.3,
        lon: -97.3,
        heights: None,
    }
}

fn at(minute: u32) -> chrono::NaiveDateTime {
    chrono::NaiveDate::from_ymd_opt(2026, 8, 21)
        .unwrap()
        .and_hms_opt(18, minute, 0)
        .unwrap()
}

fn info(name: &'static str, minute: u32) -> ScanInfo {
    ScanInfo {
        site: site_named(name),
        site_source: squallar_radar::site_position::SitePositionSource::Table,
        site_position: None,
        timestamp: at(minute),
        vcp_number: 212,
        available_products: Vec::new(),
        product_elevations: Default::default(),
        status: String::new(),
    }
}

/// Three panes on `sites`, each already showing that site's `at(0)` volume.
fn gui_on(sites: [&'static str; 3]) -> Gui {
    let mut gui = Gui::new();
    gui.set_pane_count_for_test(3);
    for (idx, name) in sites.into_iter().enumerate() {
        let pane = gui.pane_mut(idx).expect("the fixture built three panes");
        pane.set_site(name.to_string());
        pane.scan_info = Some(info(name, 0));
    }
    gui
}

fn shown_at(gui: &Gui, idx: usize) -> chrono::NaiveDateTime {
    gui.pane(idx)
        .and_then(|p| p.scan_info.as_ref().map(|i| i.timestamp))
        .unwrap_or_else(|| panic!("pane {idx} must still hold scan info"))
}

/// The requester's group takes it; the pane that left the clock does not.
#[test]
fn an_addressed_volume_reaches_the_requesters_time_group_and_stops_there() {
    let mut gui = gui_on(["KTLX", "KTLX", "KTLX"]);
    gui.pane_mut(1).expect("pane 1").time_link = false;

    gui.apply(crate::shell_api::GuiEvent::ScanInfoForTimeGroup {
        site: "KTLX".to_owned(),
        requester: 0,
        info: info("KTLX", 6),
    });

    assert_eq!(
        shown_at(&gui, 0),
        at(6),
        "the requester must take its volume"
    );
    assert_eq!(
        shown_at(&gui, 2),
        at(6),
        "a pane still on the shared clock moves with the requester",
    );
    assert_eq!(
        shown_at(&gui, 1),
        at(0),
        "the unlinked pane holds its moment — the whole point of the event",
    );
}

/// A pane that has left the clock navigates **alone**, and does not drag the
/// group it left with it.
#[test]
fn an_unlinked_requester_moves_only_itself() {
    let mut gui = gui_on(["KTLX", "KTLX", "KTLX"]);
    gui.pane_mut(1).expect("pane 1").time_link = false;

    gui.apply(crate::shell_api::GuiEvent::ScanInfoForTimeGroup {
        site: "KTLX".to_owned(),
        requester: 1,
        info: info("KTLX", 6),
    });

    assert_eq!(
        shown_at(&gui, 1),
        at(6),
        "the requester must take its volume"
    );
    for idx in [0, 2] {
        assert_eq!(
            shown_at(&gui, idx),
            at(0),
            "pane {idx} was moved by a pane that is not on its clock",
        );
    }
}

/// Shared time is not shared data: a linked pane on another site is skipped.
#[test]
fn a_linked_pane_on_another_site_does_not_take_the_volume() {
    let mut gui = gui_on(["KTLX", "KOUN", "KTLX"]);

    gui.apply(crate::shell_api::GuiEvent::ScanInfoForTimeGroup {
        site: "KTLX".to_owned(),
        requester: 0,
        info: info("KTLX", 6),
    });

    assert_eq!(
        shown_at(&gui, 2),
        at(6),
        "the same-site linked pane takes it"
    );
    assert_eq!(
        shown_at(&gui, 1),
        at(0),
        "a KOUN pane took a KTLX volume because it shares a clock with the \
         requester",
    );
    assert_eq!(
        gui.pane(1)
            .expect("pane 1")
            .scan_info
            .as_ref()
            .unwrap()
            .site
            .name,
        "KOUN",
        "and its scan info must still describe its own radar",
    );
}

/// **The overshoot guard at the seam.** The site-wide event is what the live
/// chunk feed publishes, and it still reaches every pane on the site — the
/// time link has no say in it.
#[test]
fn the_site_wide_event_still_reaches_every_pane_on_the_site() {
    let mut gui = gui_on(["KTLX", "KTLX", "KOUN"]);
    gui.pane_mut(1).expect("pane 1").time_link = false;

    gui.apply(crate::shell_api::GuiEvent::ScanInfoForSite {
        site: "KTLX".to_owned(),
        info: info("KTLX", 6),
    });

    for idx in [0, 1] {
        assert_eq!(
            shown_at(&gui, idx),
            at(6),
            "pane {idx} stopped following the live feed",
        );
    }
    assert_eq!(
        shown_at(&gui, 2),
        at(0),
        "site-wide still means this site, not every pane",
    );
}

/// The live feed's own event: every pane on the site **that is following
/// live**, and the time link has no say in it either way.
#[test]
fn the_live_event_reaches_the_following_panes_and_only_them() {
    let mut gui = gui_on(["KTLX", "KTLX", "KTLX"]);
    gui.pane_mut(1).expect("pane 1").time_link = false;
    gui.pane_mut(2).expect("pane 2").viewing_live = false;

    gui.apply(crate::shell_api::GuiEvent::LiveScanInfoForSite {
        site: "KTLX".to_owned(),
        info: info("KTLX", 6),
    });

    assert_eq!(
        shown_at(&gui, 0),
        at(6),
        "the live linked pane must follow the feed",
    );
    assert_eq!(
        shown_at(&gui, 1),
        at(6),
        "and so must a live pane that has left the shared clock — unlinking is \
         about who drives the navigation, not about whether live is live",
    );
    assert_eq!(
        shown_at(&gui, 2),
        at(0),
        "the pane parked in the archive was dragged forward by the live feed",
    );
}

/// The mid-volume merge takes the same audience: it moves `timestamp` too.
#[test]
fn the_mid_volume_merge_skips_a_pane_parked_in_the_archive() {
    let mut gui = gui_on(["KTLX", "KTLX", "KTLX"]);
    gui.pane_mut(2).expect("pane 2").viewing_live = false;

    gui.apply(crate::shell_api::GuiEvent::ChunkScanInfo {
        site: "KTLX".to_owned(),
        info: info("KTLX", 6),
    });

    assert_eq!(shown_at(&gui, 0), at(6), "a live pane takes the merge");
    assert_eq!(
        shown_at(&gui, 2),
        at(0),
        "the merge writes `timestamp`, so an ungated pass moves a parked pane \
         just as the closed-volume path did",
    );
}
