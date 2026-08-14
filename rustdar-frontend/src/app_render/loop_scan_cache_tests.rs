//! What bounds `LoopDownloadManager`'s decoded volumes.
//!
//! The cache holds one whole `Arc<Scan>` — 47–69 MiB — per `(site, timestamp)`,
//! and until `App::evict_unneeded_loop_scans` nothing ever removed one: a frame
//! eviction retires a pane's *frame*, `clear_all` fires only when a pane leaves
//! a radar, and the loop pool's byte budget counts texture bytes rather than
//! these CPU-side volumes. A pane parked on a live radar accumulated one per
//! polled scan for the life of the process.
//!
//! Every test here drives the real writers — `append_scan_to_active_loops` for
//! the poll path, `accept_scan_listing` for the listing — so a change to what
//! either of them writes reaches these assertions rather than going around them.

use super::*;
use crate::app::tests::{empty_scan, headless};
use crate::platform_double::TestBridge;
use rustdar_egui::pane::{LoopPhase, LoopPlaybackState};
use rustdar_radar::archive::Identifier;
use rustdar_radar::types::{RadarProduct, RenderView};

/// The radar every loop below is on.
const SITE: &str = "KTLX";

fn at(minute: u32) -> chrono::NaiveDateTime {
    chrono::NaiveDate::from_ymd_opt(2024, 1, 1)
        .unwrap()
        .and_hms_opt(0, 0, 0)
        .unwrap()
        + chrono::Duration::minutes(minute as i64)
}

/// A volume as the cache holds one. Nothing here reads a moment or a fold
/// limit — every assertion is about *which* keys are present — so the cheap
/// empty volume is the honest fixture.
fn volume() -> crate::loop_downloads::CachedVolume {
    (Arc::new(empty_scan()), Default::default())
}

/// A headless app whose one pane is on [`SITE`], with no loop.
fn app_on_site() -> crate::app::App {
    let mut app = headless(TestBridge::desktop());
    app.gui.pane_mut(0).expect("a fresh Gui has one pane").site = SITE.to_string();
    app
}

/// Put pane 0's loop where `handle_enable_loop` leaves it: active, on [`SITE`],
/// and `FetchingScanList` with no frames at all.
fn begin_loop(app: &mut crate::app::App, lookback_secs: u64) {
    let site = rustdar_radar::sites::get_radar_site(SITE)
        .expect("KTLX is in the resolved site table")
        .clone();
    let pane = app.gui.pane_mut(0).expect("a fresh Gui has one pane");
    pane.loop_state = LoopPlaybackState::new_for_loop(lookback_secs, &site, RenderView::PlanView);
    assert_eq!(
        pane.loop_state.phase,
        LoopPhase::FetchingScanList,
        "precondition: a freshly built loop is waiting on its listing",
    );
}

/// Install a listing naming `minutes`, through the function the real listing
/// response goes through — which is what moves the loop out of
/// `FetchingScanList` and fills `frames`.
fn install_listing(app: &mut crate::app::App, minutes: &[u32]) {
    let allocation = test_loop_allocation();
    let budgets = test_budgets();
    let scans: Vec<_> = minutes
        .iter()
        .map(|&minute| {
            (
                at(minute),
                Identifier::new(format!("KTLX2024010100{minute:02}00_V06")),
            )
        })
        .collect();
    let pane = app.gui.pane_mut(0).expect("a fresh Gui has one pane");
    accept_scan_listing(allocation, &budgets, &mut pane.loop_state, SITE, scans)
        .expect("a non-empty listing for this loop's own site is accepted");
    assert_eq!(
        pane.loop_state.frames.len(),
        minutes.len(),
        "precondition: the listing became frames without being sampled",
    );
}

/// Feed the poll path a volume, the way a completed auto-poll does.
fn poll_scan(app: &mut crate::app::App, minute: u32) {
    let (scan, declared) = volume();
    app.append_scan_to_active_loops(SITE, at(minute), scan, declared);
}

fn frames(app: &crate::app::App) -> Vec<chrono::NaiveDateTime> {
    app.gui
        .pane(0)
        .expect("a fresh Gui has one pane")
        .loop_state
        .frames
        .iter()
        .map(|frame| frame.timestamp)
        .collect()
}

/// **The leak.** A live site with no loop at all accumulated every volume its
/// polls delivered.
///
/// `append_scan_to_active_loops` caches unconditionally and *then* offers the
/// frame to whatever loops are on the site — so a pane watching a radar without
/// looping it wrote an entry every scan and no path removed one. At a WSR-88D's
/// precip cadence that is roughly 0.4–1 GB an hour, held for the life of the
/// process, outside every byte budget in the workspace.
///
/// The count is asserted **before** the sweep as well as after, so the test
/// cannot pass against an empty cache — which is how a cache test comes to
/// prove nothing.
#[test]
fn polled_volumes_no_loop_asked_for_are_not_kept() {
    const POLLED: u32 = 6;

    let mut app = app_on_site();
    for minute in 0..POLLED {
        poll_scan(&mut app, minute);
    }

    assert_eq!(
        app.loop_mgr.cached_scan_count(SITE),
        POLLED as usize,
        "precondition: the poll path really did cache every volume, so the \
         sweep below has something to remove",
    );
    assert!(
        !app.gui
            .pane(0)
            .expect("a fresh Gui has one pane")
            .loop_state
            .is_active(),
        "precondition: no loop names any of these volumes",
    );

    app.evict_unshown_scans();

    assert_eq!(
        app.loop_mgr.cached_scan_count(SITE),
        0,
        "a pane parked on a live radar is still holding one decoded volume per \
         polled scan; nothing else in this crate ever removes one",
    );
    assert!(
        !app.loop_mgr.has_cached_site(SITE),
        "the emptied site's inner map was left behind, so \"holds nothing\" and \
         \"is not in the map\" have come apart",
    );
}

/// **The keep.** Every volume a live loop frame names survives the sweep, and
/// the lookup the renderer makes still resolves.
///
/// This is the half a byte-LRU cannot promise: evict an entry a frame still
/// names and that frame's next dispatch re-requests it over the network, every
/// pass, for as long as the loop runs.
#[test]
fn a_live_loops_frames_keep_their_volumes() {
    let mut app = app_on_site();
    begin_loop(&mut app, 3600);
    install_listing(&mut app, &[0, 4, 8]);
    for minute in [0, 4, 8] {
        app.loop_mgr.cache_scan(SITE, at(minute), volume());
    }
    // A volume no frame names, from a window this loop has already moved past.
    app.loop_mgr.cache_scan(SITE, at(99), volume());
    assert_eq!(app.loop_mgr.cached_scan_count(SITE), 4);

    app.evict_unshown_scans();

    let target = RenderTarget::new(SITE, RadarProduct::Reflectivity, 0.5);
    for minute in [0, 4, 8] {
        assert!(
            app.loop_mgr.get_cached(SITE, &at(minute)).is_some(),
            "minute {minute}: a frame the loop is playing lost its volume, so \
             its dispatch re-downloads it on every pass",
        );
        assert!(
            frame_data(&app.loop_mgr, &target, at(minute)).is_some(),
            "minute {minute}: the lookup the renderer actually makes no longer \
             resolves",
        );
    }
    assert!(
        app.loop_mgr.get_cached(SITE, &at(99)).is_none(),
        "a volume no frame names survived, which is the leak this sweep exists \
         to close",
    );
}

/// A loop's window moves with every live append, and the volumes its retired
/// frames named go on the next sweep.
///
/// Two evictions, in order: `append_polled_frame` measures the lookback from
/// the *newest* frame and drops what falls out of it, and the sweep then drops
/// the cache entries those frames were the only namers of. The entry count
/// tracks the frame count, which is the property that makes the cache bounded
/// by the loop rather than by the session.
#[test]
fn a_window_that_moved_sheds_the_volumes_its_old_frames_named() {
    // Ten minutes, so the appends below really do push the oldest frames out.
    let mut app = app_on_site();
    begin_loop(&mut app, 600);
    install_listing(&mut app, &[0, 2, 4]);
    for minute in [0, 2, 4] {
        app.loop_mgr.cache_scan(SITE, at(minute), volume());
    }

    app.evict_unshown_scans();
    assert_eq!(
        app.loop_mgr.cached_scan_count(SITE),
        3,
        "precondition: nothing was evicted while every entry was named",
    );

    // Two live polls. Each caches its own volume and moves the window forward.
    poll_scan(&mut app, 12);
    poll_scan(&mut app, 14);

    assert_eq!(
        frames(&app),
        vec![at(4), at(12), at(14)],
        "precondition: the window moved and dropped its two oldest frames",
    );
    assert_eq!(
        app.loop_mgr.cached_scan_count(SITE),
        5,
        "precondition: the retired frames' volumes are still resident — the \
         frame eviction does not touch the cache",
    );

    app.evict_unshown_scans();

    assert_eq!(
        app.loop_mgr.cached_scan_count(SITE),
        frames(&app).len(),
        "the cache no longer tracks the loop's frame list",
    );
    for minute in [0, 2] {
        assert!(
            app.loop_mgr.get_cached(SITE, &at(minute)).is_none(),
            "minute {minute}: the volume of a frame the window retired is still \
             held, so a loop parked on a live site grows without bound",
        );
    }
    for minute in [4, 12, 14] {
        assert!(
            app.loop_mgr.get_cached(SITE, &at(minute)).is_some(),
            "minute {minute}: a frame still in the window lost its volume",
        );
    }
}

/// **The grace rule.** A loop waiting on its listing has no frames, and its
/// site is skipped whole rather than swept against an empty set.
///
/// Without it every product switch and every loop re-init re-downloads its
/// entire window: `begin_loop_for_pane` empties `frames` and leaves the loop in
/// `FetchingScanList` for as long as the listing round-trip takes, and a sweep
/// during that gap sees a loop that names nothing. That call site states the
/// contract this preserves — "The scan cache is global and deliberately kept."
///
/// The second half is what stops the rule from being a blanket exemption: once
/// the listing installs frames, the entries none of them name go.
#[test]
fn a_loop_still_fetching_its_listing_keeps_its_window() {
    let mut app = app_on_site();
    begin_loop(&mut app, 3600);
    for minute in [0, 4, 8] {
        app.loop_mgr.cache_scan(SITE, at(minute), volume());
    }
    assert!(
        frames(&app).is_empty(),
        "precondition: a loop fetching its listing names no frame at all",
    );

    app.evict_unshown_scans();

    assert_eq!(
        app.loop_mgr.cached_scan_count(SITE),
        3,
        "a loop's whole window was evicted in the gap before its listing \
         landed, so every product switch and every re-init re-downloads it",
    );

    // The listing lands and names two of the three.
    install_listing(&mut app, &[0, 4]);
    app.evict_unshown_scans();

    assert_eq!(
        app.loop_mgr.cached_scan_count(SITE),
        2,
        "the grace rule outlived the listing it was granted for, which makes it \
         a permanent exemption rather than a settling window",
    );
    assert!(
        app.loop_mgr.get_cached(SITE, &at(8)).is_none(),
        "the entry the new listing does not name survived the sweep that \
         followed it",
    );
}
