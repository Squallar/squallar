use super::*;
use squallar_egui::pane::PaneState;
use squallar_radar::loop_downloads::LoopDownloadManager;
use squallar_radar::sites::RadarSite;
use squallar_radar::types::{RadarProduct, ScanInfo};
use squallar_source::id::known;

fn ts(minute: u32) -> NaiveDateTime {
    chrono::NaiveDate::from_ymd_opt(2024, 1, 1)
        .unwrap()
        .and_hms_opt(0, 0, 0)
        .unwrap()
        + chrono::Duration::minutes(i64::from(minute))
}

fn site(name: &'static str, lat: f64, lon: f64) -> RadarSite {
    RadarSite {
        name,
        network: squallar_radar::sites::RadarNetwork::of_id(name),
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
        site_source: squallar_radar::site_position::SitePositionSource::Table,
        site_position: None,
        timestamp,
        vcp_number: 212,
        available_products: vec![RadarProduct::Reflectivity],
        product_elevations: std::collections::HashMap::new(),
        status: String::new(),
    });
    pane
}

/// **The fixture builds its frames through the production walk**, one polled
/// scan at a time — not by writing a frame list into the timeline. A fixture
/// that assembled the list itself could not tell a walk that appends from one
/// that does not.
fn pane_looping_on(site: RadarSite, lookback_secs: u64, frames: &[u32]) -> PaneState {
    let mut panes = [PaneState::with_site(site.name.to_string())];
    *panes[0].time_state_mut(&known::RADAR) = squallar_egui::radar_layer::begin_loop(
        lookback_secs,
        &site,
        squallar_radar::types::RenderView::PlanView,
    );
    for &minute in frames {
        append_polled_frame_to_loops(
            &mut panes,
            &no_handlers(),
            site.name,
            ts(minute),
            allocation(),
            &budgets(),
        );
    }
    let [pane] = panes;
    pane
}

/// **A registry that registers nothing.** Radar's own frame supply answers an
/// empty `frames_resident` by contract — its decoded volumes live above its
/// handler — so an unregistered radar id and the real one give this walk the
/// same answer, and every radar assertion below is about the polled stamp
/// alone. The layer that DOES answer is exercised in
/// `app_fetch/satellite_loop_append_tests.rs`.
fn no_handlers() -> squallar_overlays::render::overlay_state::OverlayRegistry {
    squallar_overlays::render::overlay_state::OverlayRegistry::with_handlers(Vec::new())
}

/// The live registry, so the axis `begin_loop_for_pane` branches on is the one
/// radar really declares (`extends_future: false`) rather than a stand-in.
fn registry() -> squallar_overlays::render::overlay_state::OverlayRegistry {
    squallar_overlays::render::overlay_state::OverlayRegistry::with_handlers(
        squallar_egui::sources::all(),
    )
}

/// A fetch context with a client that goes nowhere: the listing tasks
/// `begin_loop_for_pane` builds are never spawned here, only counted.
fn a_fetch_config() -> squallar_overlays::render::overlay_state::FetchConfig {
    squallar_overlays::render::overlay_state::FetchConfig {
        client: {
            squallar_source::tls::init();
            reqwest::Client::new()
        },
        zone_cache_dir: None,
        sources: squallar_radar::sources::DataSources::production(),
        viewport: None,
        as_of: ts(0),
        depicted_span_secs: None,
        depicted_frames: Vec::new(),
    }
}

fn allocation() -> crate::loop_pool::LoopAllocation {
    crate::app::render::test_loop_allocation()
}

fn budgets() -> squallar_device_profile::budget::Budgets {
    crate::app::render::test_budgets()
}

fn held_for(pane: &PaneState) -> usize {
    crate::app::render::loop_frames_held(allocation(), pane.time_state(&known::RADAR), &budgets())
}

fn frame_times(pane: &PaneState) -> Vec<NaiveDateTime> {
    pane.time_state(&known::RADAR)
        .frames
        .iter()
        .map(|f| f.timestamp)
        .collect()
}

#[test]
fn a_loop_is_built_from_its_own_panes_scan_not_the_active_panes() {
    let mut panes = [
        pane_showing(site("KTLX", 35.33, -97.27), ts(10)),
        pane_showing(site("KOUN", 35.23, -97.46), ts(25)),
    ];
    let mut reg = registry();

    assert_eq!(
        panes[1].site(),
        SWITCHED_TO,
        "precondition: pane 1's live site has already moved"
    );

    let req = arm_layer_loop(&mut panes, &mut reg, 1, ts(0), 600, &known::RADAR, None)
        .expect("pane 1 has a scan");

    assert_eq!(
        req.layer,
        known::RADAR,
        "the listing must be requested for pane 1's loaded scan's site"
    );
    assert_eq!(
        req.end,
        ts(25),
        "and end at pane 1's own scan time, not the active pane's"
    );
    assert_eq!(req.start, ts(15), "walked back by the lookback");

    let ls = panes[1].time_state(&known::RADAR);
    assert_eq!(squallar_egui::radar_layer::site(ls), "KOUN");
    assert_eq!(squallar_egui::radar_layer::coords(ls).0, 35.23);
    assert_eq!(squallar_egui::radar_layer::coords(ls).1, -97.46);
    assert!(ls.is_fetching(), "and it is waiting for that listing");

    assert!(!panes[0].time_state(&known::RADAR).is_active());

    let req = arm_layer_loop(&mut panes, &mut reg, 0, ts(0), 600, &known::RADAR, None)
        .expect("pane 0 has a scan");
    assert_eq!(req.layer, known::RADAR);
    assert_eq!(req.end, ts(10));
}

#[test]
fn a_pane_with_no_scan_yields_no_loop() {
    let mut panes = [
        pane_showing(site("KTLX", 35.33, -97.27), ts(10)),
        pane_showing(site("KOUN", 35.23, -97.46), ts(25)),
    ];
    panes[1].scan_info = None;
    let mut reg = registry();

    assert!(arm_layer_loop(&mut panes, &mut reg, 1, ts(0), 600, &known::RADAR, None).is_none());
    assert!(
        !panes[1].time_state(&known::RADAR).is_active(),
        "no loop was started"
    );
    assert!(
        arm_layer_loop(&mut panes, &mut reg, 7, ts(0), 600, &known::RADAR, None).is_none(),
        "and neither does a pane that does not exist"
    );
}

/// **A layer handed a window takes it, whatever its own arm would have
/// reached for.**
///
/// The contract that makes a pane's several timelines one loop: the transport
/// derives the window and every layer after it is listed over that same one,
/// because the pane's clock can only ever name instants the transport holds
/// frames for. Radar is the subject because its own arm is the one that would
/// disagree loudest — it ends at the scan the pane is showing (`ts(10)` here),
/// not at the handed range.
///
/// **Floor — `own_window`:** drop the `over.unwrap_or(...)` from the backward
/// arm and the two assertions read `ts(4)`/`ts(10)`, the range radar computed
/// for itself.
#[test]
fn a_layer_handed_a_window_is_listed_over_that_window() {
    let mut panes = [pane_showing(site("KTLX", 35.33, -97.27), ts(10))];
    let mut reg = registry();

    let own = arm_layer_loop(&mut panes, &mut reg, 0, ts(0), 600, &known::RADAR, None)
        .expect("pane 0 has a scan");
    assert_eq!(
        (own.start, own.end),
        (ts(0), ts(10)),
        "premise: left to itself, radar ends its range at the pane's own scan",
    );

    let handed = arm_layer_loop(
        &mut panes,
        &mut reg,
        0,
        ts(0),
        600,
        &known::RADAR,
        Some((ts(100), ts(160))),
    )
    .expect("pane 0 has a scan");
    assert_eq!(
        (handed.start, handed.end),
        (ts(100), ts(160)),
        "the handed window must win: a layer listed over its own range instead \
         holds frames at instants the pane's clock can never stop on",
    );
    assert_eq!(
        panes[0].time_state(&known::RADAR).span_secs,
        (ts(160) - ts(100)).num_seconds() as u64,
        "and the span it records is the handed window's, which is what the \
         arrival path matches a landing listing against",
    );
    assert_eq!(
        panes[0].time_state(&known::RADAR).asked_range,
        Some((ts(100), ts(160))),
        "as is the ask it recorded",
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
    let mut reg = registry();
    mgr.insert_pending(
        0,
        squallar_radar::loop_downloads::PendingDownloads {
            site: "KOUN".to_string(),
            queue: [ts(5)].into_iter().collect(),
        },
    );
    assert!(!mgr.is_pane_done(0), "precondition: pane 0 has work queued");

    assert!(
        matches!(
            begin_loop_for_pane(
                &mut panes,
                &mut reg,
                &mut mgr,
                &a_fetch_config(),
                0,
                ts(0),
                600,
            ),
            LoopScanDispatch::Armed(_),
        ),
        "pane 0 has a scan",
    );

    assert!(
        mgr.is_pane_done(0),
        "the previous loop's downloads are gone"
    );
}

/// **"No scan yet" is a deferral, not a refusal.** A radar transport with no
/// scan to anchor on answers `TransportNotReady`, and the answer is a pure
/// "not yet": nothing armed, nothing already queued torn down. The refusal
/// twin below is what holds `TransportUnlistable` in place beside it.
#[test]
fn a_scanless_radar_transport_defers_rather_than_refuses() {
    let mut panes = [PaneState::with_site("KTLX".to_string())];
    assert!(
        panes[0].scan_info.is_none(),
        "precondition: no scan has landed on this pane",
    );
    let mut mgr = LoopDownloadManager::new();
    // Work queued for a loop that already exists must survive a deferral —
    // the not-ready return sits BEFORE the pending-download clear.
    mgr.insert_pending(
        0,
        squallar_radar::loop_downloads::PendingDownloads {
            site: "KOUN".to_string(),
            queue: [ts(5)].into_iter().collect(),
        },
    );
    let mut reg = registry();

    assert!(
        matches!(
            begin_loop_for_pane(
                &mut panes,
                &mut reg,
                &mut mgr,
                &a_fetch_config(),
                0,
                ts(0),
                600,
            ),
            LoopScanDispatch::TransportNotReady,
        ),
        "a scanless radar transport is a listing that cannot be built YET, \
         not one that cannot exist",
    );
    assert!(
        !panes[0].time_state(&known::RADAR).is_active(),
        "nothing was armed",
    );
    assert!(!mgr.is_pane_done(0), "and nothing was cleared");
}

/// The refusal, untouched: a transport whose handler cannot build a listing
/// task — which no arriving scan can fix — still answers
/// `TransportUnlistable`, with the half-built arm put back.
#[test]
fn a_transport_that_cannot_list_is_still_refused_not_deferred() {
    let mut panes = [pane_showing(site("KTLX", 35.33, -97.27), ts(10))];
    let mut mgr = LoopDownloadManager::new();
    let mut reg = no_handlers();

    assert!(
        matches!(
            begin_loop_for_pane(
                &mut panes,
                &mut reg,
                &mut mgr,
                &a_fetch_config(),
                0,
                ts(0),
                600,
            ),
            LoopScanDispatch::TransportUnlistable,
        ),
        "the scan is here; what fails is the listing itself",
    );
    assert!(
        !panes[0].time_state(&known::RADAR).is_active(),
        "the half-built arm was put back, not left in FetchingScanList",
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

    append_polled_frame_to_loops(
        &mut panes,
        &no_handlers(),
        "KTLX",
        ts(10),
        allocation(),
        &budgets(),
    );

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

    append_polled_frame_to_loops(
        &mut panes,
        &no_handlers(),
        "KTLX",
        ts(10),
        allocation(),
        &budgets(),
    );
    assert_eq!(
        frame_times(&panes[0]),
        vec![ts(0)],
        "the loop is still a KOUN loop"
    );

    append_polled_frame_to_loops(
        &mut panes,
        &no_handlers(),
        "KOUN",
        ts(10),
        allocation(),
        &budgets(),
    );
    assert_eq!(frame_times(&panes[0]), vec![ts(0), ts(10)]);
}

#[test]
fn an_inactive_loop_takes_no_frames() {
    let mut panes = [PaneState::with_site("KTLX".to_string())];
    assert_eq!(
        squallar_egui::radar_layer::site(panes[0].time_state(&known::RADAR)),
        "",
        "precondition: placeholder site"
    );

    append_polled_frame_to_loops(
        &mut panes,
        &no_handlers(),
        "KTLX",
        ts(10),
        allocation(),
        &budgets(),
    );
    append_polled_frame_to_loops(
        &mut panes,
        &no_handlers(),
        "",
        ts(11),
        allocation(),
        &budgets(),
    );

    assert!(panes[0].time_state(&known::RADAR).frames.is_empty());
}

#[test]
fn a_polled_frame_is_inserted_in_time_order_and_never_twice() {
    let ktlx = site("KTLX", 35.33, -97.27);
    let mut panes = [pane_looping_on(ktlx, 3600, &[0, 10])];

    append_polled_frame_to_loops(
        &mut panes,
        &no_handlers(),
        "KTLX",
        ts(5),
        allocation(),
        &budgets(),
    );
    assert_eq!(frame_times(&panes[0]), vec![ts(0), ts(5), ts(10)]);

    append_polled_frame_to_loops(
        &mut panes,
        &no_handlers(),
        "KTLX",
        ts(5),
        allocation(),
        &budgets(),
    );
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

    append_polled_frame_to_loops(
        &mut panes,
        &no_handlers(),
        "KTLX",
        ts(15),
        allocation(),
        &budgets(),
    );

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
    // A LIVE pane whose playhead is on the newest frame it holds. Parking is
    // the only writer of the playhead, so the clock is put back to `Live`
    // straight after — and after WO-M12f that posture is what decides the
    // window: a live pane's is anchored on its newest frame, exactly as every
    // pane's was before. The scrubbed pane's is pinned next door.
    panes[0].park_on_frame(&known::RADAR, 2);
    panes[0].set_time_mode(squallar_egui::pane::TimeMode::Live);
    assert_eq!(
        panes[0].time_state(&known::RADAR).current_frame(),
        2,
        "precondition: the playhead is past where the window will leave it"
    );

    append_polled_frame_to_loops(
        &mut panes,
        &no_handlers(),
        "KTLX",
        ts(25),
        allocation(),
        &budgets(),
    );

    assert_eq!(
        frame_times(&panes[0]),
        vec![ts(25)],
        "precondition: only the new frame survives"
    );
    assert_eq!(
        panes[0].time_state(&known::RADAR).current_frame(),
        0,
        "the playhead must land on a frame that exists"
    );
    assert!(
        panes[0]
            .time_state(&known::RADAR)
            .frames
            .get(panes[0].time_state(&known::RADAR).current_frame())
            .is_some(),
        "and resolve to one, which is what the pane renders through"
    );
}

/// **A scrubbed pane's frame-list window follows its clock, not the newest
/// frame** (WO-M12f).
///
/// OLD behaviour, and the value this replaces: the cutoff was
/// `newest_frame - span` for every posture, so this same scenario answered
/// `vec![ts(25)]` — one arriving live frame evicted `ts(0)`, `ts(5)` and
/// `ts(10)`, which are the frames the pane is parked on and rendering. NEW:
/// the cutoff is `clock - span`, so the window sits where the pane is
/// looking and the arriving frame joins it.
#[test]
fn a_scrubbed_panes_window_follows_its_clock_not_the_newest_frame() {
    let ktlx = site("KTLX", 35.33, -97.27);
    let mut panes = [pane_looping_on(ktlx, 600, &[0, 5, 10])];
    panes[0].park_on_frame(&known::RADAR, 1);
    assert_eq!(
        panes[0].time_state(&known::RADAR).playhead_stamp(),
        Some(ts(5)),
        "precondition: the pane is parked five minutes back"
    );

    append_polled_frame_to_loops(
        &mut panes,
        &no_handlers(),
        "KTLX",
        ts(25),
        allocation(),
        &budgets(),
    );

    assert_eq!(
        frame_times(&panes[0]),
        vec![ts(0), ts(5), ts(10), ts(25)],
        "the frames the scrubbed pane is looking at were evicted by a live \
         arrival — the window followed the newest frame instead of the clock"
    );
    assert_eq!(
        panes[0].time_state(&known::RADAR).playhead_stamp(),
        Some(ts(5)),
        "and the playhead still names the instant it was parked on"
    );

    // And it FOLLOWS the clock rather than merely being wider than it: move
    // the clock onto the new frame and the same window now bites, dropping
    // everything more than the lookback behind it. Without this the test
    // above could not tell a clock-anchored cutoff from no cutoff at all.
    panes[0].park_on_frame(&known::RADAR, 3);
    assert_eq!(
        panes[0].time_state(&known::RADAR).playhead_stamp(),
        Some(ts(25)),
        "precondition: the clock moved forward onto the arrival"
    );
    append_polled_frame_to_loops(
        &mut panes,
        &no_handlers(),
        "KTLX",
        ts(30),
        allocation(),
        &budgets(),
    );
    assert_eq!(
        frame_times(&panes[0]),
        vec![ts(25), ts(30)],
        "the window did not move up with the clock"
    );
}

/// **A live pane keeps the window it always had**, stated beside the change
/// so the two postures are visibly one decision: `Live` resolves to the
/// newest frame on a frame series, which is the anchor the cutoff used
/// unconditionally before WO-M12f.
#[test]
fn a_live_panes_window_is_still_anchored_on_its_newest_frame() {
    let ktlx = site("KTLX", 35.33, -97.27);
    let mut panes = [pane_looping_on(ktlx, 600, &[0, 5, 10])];
    assert!(
        matches!(panes[0].time.mode, squallar_egui::pane::TimeMode::Live),
        "precondition: a pane is live until something parks it"
    );

    append_polled_frame_to_loops(
        &mut panes,
        &no_handlers(),
        "KTLX",
        ts(25),
        allocation(),
        &budgets(),
    );

    assert_eq!(
        frame_times(&panes[0]),
        vec![ts(25)],
        "a live pane stopped evicting past its lookback"
    );
}

#[test]
fn live_appends_do_not_take_a_loop_past_its_frame_cap() {
    let ktlx = site("KTLX", 35.33, -97.27);
    let held = held_for(&pane_looping_on(ktlx.clone(), 72 * 3600, &[]));
    let sampled: Vec<u32> = (0..held as u32).map(|i| i * 26).collect();
    let mut panes = [pane_looping_on(ktlx, 72 * 3600, &sampled)];
    panes[0].time_state_mut(&known::RADAR).sampled = Some(true);
    assert_eq!(
        panes[0].time_state(&known::RADAR).frames.len(),
        held,
        "precondition: the loop starts full",
    );

    let newest = *sampled.last().expect("the cap is not zero");
    for i in 1..=held as u32 {
        append_polled_frame_to_loops(
            &mut panes,
            &no_handlers(),
            "KTLX",
            ts(newest + i * 4),
            allocation(),
            &budgets(),
        );
    }

    assert_eq!(
        panes[0].time_state(&known::RADAR).frames.len(),
        held,
        "{held} appends took the loop to {} frames against a cap of {held}",
        panes[0].time_state(&known::RADAR).frames.len(),
    );
    assert!(
        panes[0].time_state(&known::RADAR).current_frame()
            < panes[0].time_state(&known::RADAR).frames.len(),
        "the playhead must land on a frame that exists",
    );
}

#[test]
fn capping_an_appended_loop_keeps_its_whole_window() {
    let ktlx = site("KTLX", 35.33, -97.27);
    let held = held_for(&pane_looping_on(ktlx.clone(), 72 * 3600, &[]));
    let sampled: Vec<u32> = (0..held as u32).map(|i| i * 26).collect();
    let mut panes = [pane_looping_on(ktlx, 72 * 3600, &sampled)];
    panes[0].time_state_mut(&known::RADAR).sampled = Some(true);
    let oldest = ts(sampled[0]);

    let newest = *sampled.last().expect("the cap is not zero");
    let appended = ts(newest + 4);
    append_polled_frame_to_loops(
        &mut panes,
        &no_handlers(),
        "KTLX",
        appended,
        allocation(),
        &budgets(),
    );

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
        panes[0].time_state_mut(&known::RADAR).sampled = sampled;
        panes[0].time_state_mut(&known::RADAR).cadence_secs = Some(259);
        append_polled_frame_to_loops(
            &mut panes,
            &no_handlers(),
            "KTLX",
            ts(36),
            allocation(),
            &budgets(),
        );

        assert_eq!(
            panes[0].time_state(&known::RADAR).cadence_secs,
            expected,
            "listing_sampled = {sampled:?}",
        );
        panes[0].time_state_mut(&known::RADAR).frames.pop();
    }
}
