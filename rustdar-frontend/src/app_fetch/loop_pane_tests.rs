use super::*;
use crate::loop_downloads::LoopDownloadManager;
use rustdar_egui::pane::{LoopPlaybackState, PaneState};
use rustdar_radar::archive::Identifier;
use rustdar_radar::sites::RadarSite;
use rustdar_radar::types::{RadarProduct, ScanInfo};

fn ts(minute: u32) -> NaiveDateTime {
    chrono::NaiveDate::from_ymd_opt(2024, 1, 1)
        .unwrap()
        .and_hms_opt(0, minute, 0)
        .unwrap()
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

/// The site every `pane_showing` pane has already switched *to*, and whose scan
/// has not landed yet. Deliberately not a site any test here builds a loop from.
const SWITCHED_TO: &str = "KFWS";

/// A pane showing `site`'s scan at `timestamp`, with its live `site` field
/// already moved on to [`SWITCHED_TO`].
///
/// That divergence is the window `begin_loop_for_pane`'s own doc describes: the
/// pane's `site` field changes the instant the user picks a new radar, while
/// `scan_info` still holds the previous site's scan until the new one loads. The
/// loop must be built from the `scan_info` — the one place where the code, the
/// coordinates and the timestamp all come from the same radar.
///
/// Handing the pane a `site` equal to `scan_info.site.name` would make the two
/// interchangeable, and every assertion below would hold just as well for a
/// `begin_loop_for_pane` that read the wrong one.
fn pane_showing(site: RadarSite, timestamp: NaiveDateTime) -> PaneState {
    assert_ne!(
        site.name, SWITCHED_TO,
        "the fixture's divergence must be real"
    );
    let mut pane = PaneState::with_site(SWITCHED_TO.to_string());
    pane.scan_info = Some(ScanInfo {
        site,
        timestamp,
        vcp_number: 212,
        available_products: vec![RadarProduct::Reflectivity],
        product_elevations: std::collections::HashMap::new(),
        status: String::new(),
    });
    pane
}

/// A pane with an active loop on `site`, holding frames at the given minutes.
fn pane_looping_on(site: RadarSite, lookback_secs: u64, frames: &[u32]) -> PaneState {
    let mut pane = PaneState::with_site(site.name.to_string());
    pane.loop_state = LoopPlaybackState::new_for_loop(lookback_secs, &site);
    for &minute in frames {
        append_polled_frame(&mut pane.loop_state, site.name, ts(minute));
    }
    pane
}

fn frame_times(pane: &PaneState) -> Vec<NaiveDateTime> {
    pane.loop_state.frames.iter().map(|f| f.timestamp).collect()
}

/// The defect: `handle_enable_loop` read the *active* pane's scan info and
/// `reinit_active_loops` then applied it to every looping pane, so a pane on
/// another site silently showed the active pane's radar under its own label.
#[test]
fn a_loop_is_built_from_its_own_panes_scan_not_the_active_panes() {
    // Pane 0 is the active one in every real call path that reaches here.
    let mut panes = [
        pane_showing(site("KTLX", 35.33, -97.27), ts(10)),
        pane_showing(site("KOUN", 35.23, -97.46), ts(25)),
    ];
    let mut mgr = LoopDownloadManager::new();

    assert_eq!(
        panes[1].site, SWITCHED_TO,
        "precondition: pane 1's live site has already moved"
    );

    let req = begin_loop_for_pane(&mut panes, &mut mgr, 1, 600).expect("pane 1 has a scan");

    // Pane 1's *scan_info* site, which is neither the active pane's site nor its
    // own live `site` field. Both are in reach at the listing site and both are
    // wrong: the identifiers this listing returns are cached and projected with
    // the coordinates that came out of the same `scan_info`.
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

    // The loop state is built from the same site value the listing names, so the
    // code it is compared on and the coordinates it projects with agree.
    let ls = &panes[1].loop_state;
    assert_eq!(ls.site, "KOUN");
    assert_eq!(ls.site_lat, 35.23);
    assert_eq!(ls.site_lon, -97.46);
    assert!(ls.is_fetching(), "and it is waiting for that listing");

    // The pane that was *not* asked for is untouched, so nothing here is
    // incidentally right because both panes were written.
    assert!(!panes[0].loop_state.is_active());

    // Pane 0 reads as itself when it is the one asked for.
    let req = begin_loop_for_pane(&mut panes, &mut mgr, 0, 600).expect("pane 0 has a scan");
    assert_eq!(req.site, "KTLX");
    assert_eq!(req.end, ts(10));
}

/// A pane with nothing loaded yet has no loop parameters, and must not borrow
/// another pane's — nor leave a loop half-built.
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

/// The defect: auto-poll asked one question per live site but answered every
/// one of them with the *active* pane's scan time. With two sites on screen the
/// site that was not active either never updated (its latest was older than the
/// active pane's, so `check_and_fetch_latest` declined) or was re-downloaded and
/// re-rendered in full every poll interval (the active pane was parked on
/// historic data, so everything looked new).
#[test]
fn each_site_is_polled_against_its_own_current_scan() {
    // Pane 0 is the active one in every path that reaches here, and is the
    // newer of the two.
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

/// The scan's own site decides, not the pane's live `site` field: the two
/// diverge for as long as a switched pane's new scan takes to land, and reading
/// the pane's field would offer the old site's timestamp as the new site's
/// current — suppressing the very fetch the switch is waiting on.
#[test]
fn a_scans_own_site_decides_which_poll_it_answers() {
    let panes = [pane_showing(site("KTLX", 35.33, -97.27), ts(10))];
    assert_eq!(
        panes[0].site, SWITCHED_TO,
        "precondition: the pane's live site has already moved"
    );

    assert_eq!(latest_scan_time_for_site(&panes, "KTLX"), Some(ts(10)));
    assert_eq!(
        latest_scan_time_for_site(&panes, SWITCHED_TO),
        None,
        "the pane holds no scan of the site it has switched to"
    );
}

/// Two panes on one site, one stepped back in time: the poll must compare
/// against the newest of them, or every interval re-downloads a scan the other
/// pane already has.
#[test]
fn one_sites_current_scan_is_the_newest_pane_showing_it() {
    let ktlx = || site("KTLX", 35.33, -97.27);
    let panes = [pane_showing(ktlx(), ts(10)), pane_showing(ktlx(), ts(25))];
    assert_eq!(latest_scan_time_for_site(&panes, "KTLX"), Some(ts(25)));
}

/// Enabling a loop drops the previous listing's undispatched downloads: they
/// were queued for the loop this call is replacing, and on a site switch they
/// are another radar's files.
#[test]
fn beginning_a_loop_clears_the_panes_pending_downloads() {
    let mut panes = [pane_showing(site("KTLX", 35.33, -97.27), ts(10))];
    let mut mgr = LoopDownloadManager::new();
    mgr.insert_pending(
        0,
        crate::loop_downloads::PendingDownloads {
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

/// The defect this half of the site fix exists for. Auto-poll delivers one
/// site's scan; a loop on a different site used to take a frame for it, then
/// render that scan around its own coordinates.
#[test]
fn a_polled_scan_only_reaches_loops_on_its_own_site() {
    let ktlx = site("KTLX", 35.33, -97.27);
    let koun = site("KOUN", 35.23, -97.46);
    let mut panes = [
        pane_looping_on(ktlx, 3600, &[0, 5]),
        pane_looping_on(koun, 3600, &[0, 5]),
    ];

    append_polled_frame_to_loops(&mut panes, "KTLX", ts(10));

    assert_eq!(frame_times(&panes[0]), vec![ts(0), ts(5), ts(10)]);
    assert_eq!(
        frame_times(&panes[1]),
        vec![ts(0), ts(5)],
        "a KOUN loop must not take a frame for a KTLX scan"
    );
}

/// The loop's own site is the geometry site captured when it was built. A pane
/// whose live `site` field has been re-synced without its loop being rebuilt
/// must still be judged on the loop's site, or the frame lands in a loop that
/// projects it somewhere else.
#[test]
fn the_loops_site_decides_not_the_panes_live_site() {
    let koun = site("KOUN", 35.23, -97.46);
    let mut panes = [pane_looping_on(koun, 3600, &[0])];
    // `propagate_layer_sync` converges the pane's site without rebuilding loops.
    panes[0].site = "KTLX".to_string();

    append_polled_frame_to_loops(&mut panes, "KTLX", ts(10));
    assert_eq!(
        frame_times(&panes[0]),
        vec![ts(0)],
        "the loop is still a KOUN loop"
    );

    append_polled_frame_to_loops(&mut panes, "KOUN", ts(10));
    assert_eq!(frame_times(&panes[0]), vec![ts(0), ts(10)]);
}

/// Single-frame mode keeps a `LoopPlaybackState` around whose `site` is an
/// empty placeholder. A poll must not turn that into a frame list.
#[test]
fn an_inactive_loop_takes_no_frames() {
    let mut panes = [PaneState::with_site("KTLX".to_string())];
    assert_eq!(
        panes[0].loop_state.site, "",
        "precondition: placeholder site"
    );

    append_polled_frame_to_loops(&mut panes, "KTLX", ts(10));
    append_polled_frame_to_loops(&mut panes, "", ts(11));

    assert!(panes[0].loop_state.frames.is_empty());
}

#[test]
fn a_polled_frame_is_inserted_in_time_order_and_never_twice() {
    let ktlx = site("KTLX", 35.33, -97.27);
    let mut panes = [pane_looping_on(ktlx, 3600, &[0, 10])];

    // Out-of-order arrival still lands between its neighbours.
    append_polled_frame_to_loops(&mut panes, "KTLX", ts(5));
    assert_eq!(frame_times(&panes[0]), vec![ts(0), ts(5), ts(10)]);

    append_polled_frame_to_loops(&mut panes, "KTLX", ts(5));
    assert_eq!(
        frame_times(&panes[0]),
        vec![ts(0), ts(5), ts(10)],
        "no duplicate frame"
    );
}

/// Frames older than the lookback window are dropped as new ones arrive.
#[test]
fn appending_evicts_past_the_lookback_window() {
    let ktlx = site("KTLX", 35.33, -97.27);
    // 10 minutes of lookback, frames every 5 minutes.
    let mut panes = [pane_looping_on(ktlx, 600, &[0, 5, 10])];

    append_polled_frame_to_loops(&mut panes, "KTLX", ts(15));

    assert_eq!(
        frame_times(&panes[0]),
        vec![ts(5), ts(10), ts(15)],
        "the frame older than the window is evicted"
    );
}

/// The playhead has to come back inside the list when eviction shortens it.
///
/// A poll gap wider than the lookback — the site was down, the app was asleep,
/// the machine was suspended — evicts the whole window at once. Left alone,
/// `current_frame` points past the end, `PaneState::displayed_frame` resolves it
/// with `.get()` and finds nothing, and the pane renders blank. A paused loop
/// never advances, so it stays blank.
#[test]
fn eviction_pulls_the_playhead_back_inside_the_list() {
    let ktlx = site("KTLX", 35.33, -97.27);
    let mut panes = [pane_looping_on(ktlx, 600, &[0, 5, 10])];
    panes[0].loop_state.current_frame = 2;

    // 15 minutes on from the newest frame, with a 10 minute window: everything
    // that was there is now older than the cutoff.
    append_polled_frame_to_loops(&mut panes, "KTLX", ts(25));

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
