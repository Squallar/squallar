use super::*;
use rustdar_egui::pane::{LoopPlaybackState, PaneState};
use rustdar_radar::archive::Identifier;
use rustdar_radar::loop_downloads::LoopDownloadManager;
use rustdar_radar::sites::RadarSite;
use rustdar_radar::types::{RadarProduct, ScanInfo};

fn ts(minute: u32) -> NaiveDateTime {
    chrono::NaiveDate::from_ymd_opt(2024, 1, 1)
        .unwrap()
        .and_hms_opt(0, 0, 0)
        .unwrap()
        + chrono::Duration::minutes(i64::from(minute))
}

fn identifier(name: &str) -> Identifier {
    Identifier::new(name.to_string())
}

fn site(name: &'static str, lat: f64, lon: f64) -> RadarSite {
    RadarSite {
        name,
        lat,
        lon,
        heights: None,
    }
}

const SWITCHED_TO: &str = "KFWS";

fn pane_showing(site: RadarSite, timestamp: NaiveDateTime) -> PaneState {
    assert_ne!(
        site.name, SWITCHED_TO,
        "the fixture's divergence must be real"
    );
    let mut pane = PaneState::with_site(SWITCHED_TO.to_string());
    pane.scan_info = Some(ScanInfo {
        site,
        site_source: rustdar_radar::site_position::SitePositionSource::Table,
        site_position: None,
        timestamp,
        vcp_number: 212,
        available_products: vec![RadarProduct::Reflectivity],
        product_elevations: std::collections::HashMap::new(),
        status: String::new(),
    });
    pane
}

fn pane_looping_on(site: RadarSite, lookback_secs: u64, frames: &[u32]) -> PaneState {
    let mut pane = PaneState::with_site(site.name.to_string());
    pane.loop_state = LoopPlaybackState::new_for_loop(
        lookback_secs,
        &site,
        rustdar_radar::types::RenderView::PlanView,
    );
    for &minute in frames {
        let held = crate::app::render::loop_frames_held(
            crate::app::render::test_loop_allocation(),
            &pane.loop_state,
            &crate::app::render::test_budgets(),
        );
        append_polled_frame(&mut pane.loop_state, site.name, ts(minute), held);
    }
    pane
}

fn allocation() -> crate::loop_pool::LoopAllocation {
    crate::app::render::test_loop_allocation()
}

fn budgets() -> rustdar_device_profile::budget::Budgets {
    crate::app::render::test_budgets()
}

fn held_for(pane: &PaneState) -> usize {
    crate::app::render::loop_frames_held(allocation(), &pane.loop_state, &budgets())
}

fn frame_times(pane: &PaneState) -> Vec<NaiveDateTime> {
    pane.loop_state.frames.iter().map(|f| f.timestamp).collect()
}

#[test]
fn a_loop_is_built_from_its_own_panes_scan_not_the_active_panes() {
    let mut panes = [
        pane_showing(site("KTLX", 35.33, -97.27), ts(10)),
        pane_showing(site("KOUN", 35.23, -97.46), ts(25)),
    ];
    let mut mgr = LoopDownloadManager::new();

    assert_eq!(
        panes[1].site(),
        SWITCHED_TO,
        "precondition: pane 1's live site has already moved"
    );

    let req = begin_loop_for_pane(&mut panes, &mut mgr, 1, 600).expect("pane 1 has a scan");

    assert_eq!(
        req.site, "KOUN",
        "the listing must be requested for pane 1's loaded scan's site"
    );
    assert_eq!(
        req.end,
        ts(25),
        "and end at pane 1's own scan time, not the active pane's"
    );
    assert_eq!(req.start, ts(15), "walked back by the lookback");

    let ls = &panes[1].loop_state;
    assert_eq!(ls.site, "KOUN");
    assert_eq!(ls.site_lat, 35.23);
    assert_eq!(ls.site_lon, -97.46);
    assert!(ls.is_fetching(), "and it is waiting for that listing");

    assert!(!panes[0].loop_state.is_active());

    let req = begin_loop_for_pane(&mut panes, &mut mgr, 0, 600).expect("pane 0 has a scan");
    assert_eq!(req.site, "KTLX");
    assert_eq!(req.end, ts(10));
}

#[test]
fn a_pane_with_no_scan_yields_no_loop() {
    let mut panes = [
        pane_showing(site("KTLX", 35.33, -97.27), ts(10)),
        pane_showing(site("KOUN", 35.23, -97.46), ts(25)),
    ];
    panes[1].scan_info = None;
    let mut mgr = LoopDownloadManager::new();

    assert!(begin_loop_for_pane(&mut panes, &mut mgr, 1, 600).is_none());
    assert!(!panes[1].loop_state.is_active(), "no loop was started");
    assert!(
        begin_loop_for_pane(&mut panes, &mut mgr, 7, 600).is_none(),
        "and neither does a pane that does not exist"
    );
}

#[test]
fn each_site_is_polled_against_its_own_current_scan() {
    let panes = [
        pane_showing(site("KTLX", 35.33, -97.27), ts(25)),
        pane_showing(site("KOUN", 35.23, -97.46), ts(10)),
    ];

    assert_eq!(
        latest_scan_time_for_site(&panes, "KOUN"),
        Some(ts(10)),
        "KOUN is polled against KOUN's scan, not the active pane's",
    );
    assert_eq!(latest_scan_time_for_site(&panes, "KTLX"), Some(ts(25)));
    assert_eq!(
        latest_scan_time_for_site(&panes, "KFWS"),
        None,
        "a site nothing is showing has no current scan, so its latest is fetched",
    );
}

#[test]
fn a_scans_own_site_decides_which_poll_it_answers() {
    let panes = [pane_showing(site("KTLX", 35.33, -97.27), ts(10))];
    assert_eq!(
        panes[0].site(),
        SWITCHED_TO,
        "precondition: the pane's live site has already moved"
    );

    assert_eq!(latest_scan_time_for_site(&panes, "KTLX"), Some(ts(10)));
    assert_eq!(
        latest_scan_time_for_site(&panes, SWITCHED_TO),
        None,
        "the pane holds no scan of the site it has switched to"
    );
}

#[test]
fn one_sites_current_scan_is_the_newest_pane_showing_it() {
    let ktlx = || site("KTLX", 35.33, -97.27);
    let panes = [pane_showing(ktlx(), ts(10)), pane_showing(ktlx(), ts(25))];
    assert_eq!(latest_scan_time_for_site(&panes, "KTLX"), Some(ts(25)));
}

#[test]
fn beginning_a_loop_clears_the_panes_pending_downloads() {
    let mut panes = [pane_showing(site("KTLX", 35.33, -97.27), ts(10))];
    let mut mgr = LoopDownloadManager::new();
    mgr.insert_pending(
        0,
        rustdar_radar::loop_downloads::PendingDownloads {
            site: "KOUN".to_string(),
            queue: [(ts(5), identifier("KOUN20240101_000500_V06"))]
                .into_iter()
                .collect(),
        },
    );
    assert!(!mgr.is_pane_done(0), "precondition: pane 0 has work queued");

    begin_loop_for_pane(&mut panes, &mut mgr, 0, 600).expect("pane 0 has a scan");

    assert!(
        mgr.is_pane_done(0),
        "the previous loop's downloads are gone"
    );
}

#[test]
fn a_polled_scan_only_reaches_loops_on_its_own_site() {
    let ktlx = site("KTLX", 35.33, -97.27);
    let koun = site("KOUN", 35.23, -97.46);
    let mut panes = [
        pane_looping_on(ktlx, 3600, &[0, 5]),
        pane_looping_on(koun, 3600, &[0, 5]),
    ];

    append_polled_frame_to_loops(&mut panes, "KTLX", ts(10), allocation(), &budgets());

    assert_eq!(frame_times(&panes[0]), vec![ts(0), ts(5), ts(10)]);
    assert_eq!(
        frame_times(&panes[1]),
        vec![ts(0), ts(5)],
        "a KOUN loop must not take a frame for a KTLX scan"
    );
}

#[test]
fn the_loops_site_decides_not_the_panes_live_site() {
    let koun = site("KOUN", 35.23, -97.46);
    let mut panes = [pane_looping_on(koun, 3600, &[0])];
    panes[0].set_site("KTLX".to_string());

    append_polled_frame_to_loops(&mut panes, "KTLX", ts(10), allocation(), &budgets());
    assert_eq!(
        frame_times(&panes[0]),
        vec![ts(0)],
        "the loop is still a KOUN loop"
    );

    append_polled_frame_to_loops(&mut panes, "KOUN", ts(10), allocation(), &budgets());
    assert_eq!(frame_times(&panes[0]), vec![ts(0), ts(10)]);
}

#[test]
fn an_inactive_loop_takes_no_frames() {
    let mut panes = [PaneState::with_site("KTLX".to_string())];
    assert_eq!(
        panes[0].loop_state.site, "",
        "precondition: placeholder site"
    );

    append_polled_frame_to_loops(&mut panes, "KTLX", ts(10), allocation(), &budgets());
    append_polled_frame_to_loops(&mut panes, "", ts(11), allocation(), &budgets());

    assert!(panes[0].loop_state.frames.is_empty());
}

#[test]
fn a_polled_frame_is_inserted_in_time_order_and_never_twice() {
    let ktlx = site("KTLX", 35.33, -97.27);
    let mut panes = [pane_looping_on(ktlx, 3600, &[0, 10])];

    append_polled_frame_to_loops(&mut panes, "KTLX", ts(5), allocation(), &budgets());
    assert_eq!(frame_times(&panes[0]), vec![ts(0), ts(5), ts(10)]);

    append_polled_frame_to_loops(&mut panes, "KTLX", ts(5), allocation(), &budgets());
    assert_eq!(
        frame_times(&panes[0]),
        vec![ts(0), ts(5), ts(10)],
        "no duplicate frame"
    );
}

#[test]
fn appending_evicts_past_the_lookback_window() {
    let ktlx = site("KTLX", 35.33, -97.27);
    let mut panes = [pane_looping_on(ktlx, 600, &[0, 5, 10])];

    append_polled_frame_to_loops(&mut panes, "KTLX", ts(15), allocation(), &budgets());

    assert_eq!(
        frame_times(&panes[0]),
        vec![ts(5), ts(10), ts(15)],
        "the frame older than the window is evicted"
    );
}

#[test]
fn eviction_pulls_the_playhead_back_inside_the_list() {
    let ktlx = site("KTLX", 35.33, -97.27);
    let mut panes = [pane_looping_on(ktlx, 600, &[0, 5, 10])];
    panes[0].loop_state.current_frame = 2;

    append_polled_frame_to_loops(&mut panes, "KTLX", ts(25), allocation(), &budgets());

    assert_eq!(
        frame_times(&panes[0]),
        vec![ts(25)],
        "precondition: only the new frame survives"
    );
    assert_eq!(
        panes[0].loop_state.current_frame, 0,
        "the playhead must land on a frame that exists"
    );
    assert!(
        panes[0]
            .loop_state
            .frames
            .get(panes[0].loop_state.current_frame)
            .is_some(),
        "and resolve to one, which is what the pane renders through"
    );
}

#[test]
fn live_appends_do_not_take_a_loop_past_its_frame_cap() {
    let ktlx = site("KTLX", 35.33, -97.27);
    let held = held_for(&pane_looping_on(ktlx.clone(), 72 * 3600, &[]));
    let sampled: Vec<u32> = (0..held as u32).map(|i| i * 26).collect();
    let mut panes = [pane_looping_on(ktlx, 72 * 3600, &sampled)];
    panes[0].loop_state.listing_sampled = Some(true);
    assert_eq!(
        panes[0].loop_state.frames.len(),
        held,
        "precondition: the loop starts full",
    );

    let newest = *sampled.last().expect("the cap is not zero");
    for i in 1..=held as u32 {
        append_polled_frame_to_loops(
            &mut panes,
            "KTLX",
            ts(newest + i * 4),
            allocation(),
            &budgets(),
        );
    }

    assert_eq!(
        panes[0].loop_state.frames.len(),
        held,
        "{held} appends took the loop to {} frames against a cap of {held}",
        panes[0].loop_state.frames.len(),
    );
    assert!(
        panes[0].loop_state.current_frame < panes[0].loop_state.frames.len(),
        "the playhead must land on a frame that exists",
    );
}

#[test]
fn capping_an_appended_loop_keeps_its_whole_window() {
    let ktlx = site("KTLX", 35.33, -97.27);
    let held = held_for(&pane_looping_on(ktlx.clone(), 72 * 3600, &[]));
    let sampled: Vec<u32> = (0..held as u32).map(|i| i * 26).collect();
    let mut panes = [pane_looping_on(ktlx, 72 * 3600, &sampled)];
    panes[0].loop_state.listing_sampled = Some(true);
    let oldest = ts(sampled[0]);

    let newest = *sampled.last().expect("the cap is not zero");
    let appended = ts(newest + 4);
    append_polled_frame_to_loops(&mut panes, "KTLX", appended, allocation(), &budgets());

    let times = frame_times(&panes[0]);
    assert_eq!(times.len(), held, "back inside the cap");
    assert_eq!(
        times[0], oldest,
        "the oldest frame was dropped, so the loop no longer covers the \
         lookback the user asked for",
    );
    assert_eq!(
        *times.last().expect("frames"),
        appended,
        "the scan that was just polled is not in the loop, so the loop is not \
         showing the present",
    );
}

#[test]
fn a_loop_holding_every_scan_re_measures_the_cadence_as_it_follows_the_site() {
    let ktlx = site("KTLX", 35.33, -97.27);
    let mut panes = [pane_looping_on(ktlx, 72 * 3600, &[0, 9, 18, 27])];

    for (sampled, expected) in [(Some(false), Some(540)), (Some(true), Some(259))] {
        panes[0].loop_state.listing_sampled = sampled;
        panes[0].loop_state.scan_step_secs = Some(259);
        append_polled_frame_to_loops(&mut panes, "KTLX", ts(36), allocation(), &budgets());

        assert_eq!(
            panes[0].loop_state.scan_step_secs, expected,
            "listing_sampled = {sampled:?}",
        );
        panes[0].loop_state.frames.pop();
    }
}
