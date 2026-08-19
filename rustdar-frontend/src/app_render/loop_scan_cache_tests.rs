//! What bounds `LoopDownloadManager`'s two data caches.
//!
//! The volume cache holds one whole `Arc<Scan>` — 47–69 MiB — per
//! `(site, timestamp)`, and until `App::evict_unneeded_loop_scans` nothing ever
//! removed one: a frame eviction retires a pane's *frame*, the site switch's
//! wholesale clear was the only other remover and has since gone, and the loop
//! pool's byte budget counts texture bytes rather than these CPU-side volumes.
//! A pane parked on a live radar accumulated one per polled scan for the life
//! of the process.
//!
//! `l3_cache` is the same defect on the other datasource, and the tests for it
//! live here rather than beside the Level III dispatch because it is the *same*
//! sweep and the *same* predicate that bounds them: one `Level3Product` per
//! `(site, AWIPS code, volume start)`, carrying the decoded message and the
//! bytes it was decoded from, written for every frame of every Level III loop
//! and removed by nothing but that same site switch.
//!
//! Every test here drives the real writers — `append_scan_to_active_loops` for
//! the poll path, `accept_scan_listing` for the listing, `cache_l3_product` for
//! a landed pairing — so a change to what any of them writes reaches these
//! assertions rather than going around them.

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
fn volume() -> rustdar_radar::loop_downloads::CachedVolume {
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
    assert!(
        pane.loop_state.listing_since.is_some(),
        "precondition: entering the fetching phase starts the clock on the \
         grace exemption",
    );
}

/// Backdate pane 0's listing clock by `secs`, the way a listing that has been in
/// flight that long would leave it.
///
/// Subtracting from the stamp rather than sleeping: the bound is a minute, and a
/// test that waited for it would be a minute long and still be timing-dependent.
///
/// `checked_sub` with a named failure, not a bare `-`: `web_time::Instant`'s
/// `Sub` panics on underflow, and this clock's origin is the host's boot. A
/// machine less than a couple of minutes old is the only way to reach it, and an
/// explained refusal is worth more there than a bare arithmetic panic — a panic
/// is also the one way a negative control can "fail" for the wrong reason.
fn age_listing(app: &mut crate::app::App, secs: u64) {
    let pane = app.gui.pane_mut(0).expect("a fresh Gui has one pane");
    let since = pane
        .loop_state
        .listing_since
        .expect("the loop is fetching, so it has a clock");
    pane.loop_state.listing_since = Some(
        since
            .checked_sub(std::time::Duration::from_secs(secs))
            .expect("the monotonic clock's origin is younger than the grace bound, which needs a host booted seconds ago"),
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

/// A paired Level III object as the cache holds one. Nothing here decodes or
/// renders it — every assertion below is about *which* keys are present — so an
/// object carrying no symbology and no bytes is the honest fixture.
fn object() -> Arc<rustdar_radar::level3::Level3Product> {
    Arc::new(rustdar_radar::level3::Level3Product {
        message: nexrad_level3::model::Level3Message {
            header: nexrad_level3::model::MessageHeader {
                message_code: 135,
                date_of_message: 19723,
                time_of_message: 90,
                message_length: 0,
                source_id: 0,
                destination_id: 0,
                number_of_blocks: 3,
            },
            pdb: nexrad_level3::model::ProductDescriptionBlock {
                block_divider: -1,
                latitude: 35.333,
                longitude: -97.278,
                height: 1200,
                product_code: 135,
                operational_mode: 2,
                vcp: 212,
                sequence_number: 0,
                volume_scan_number: 1,
                volume_scan_date: 19723,
                volume_scan_time: 0,
                generation_date: 19723,
                generation_time: 90,
                product_specific_1: 0,
                product_specific_2: 0,
                elevation_number: 1,
                product_specific_3: 0,
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
        stamp: rustdar_radar::level3::ProductStamp::from_key("TLX_EET_2024_01_01_00_01_30"),
        bytes: Arc::new(Vec::new()),
    })
}

/// Land a pairing for `code` against the volume at `minute`, exactly as
/// `poll_loop_l3_fetch_results` files one when its response arrives.
fn pair(app: &mut crate::app::App, code: &str, minute: u32) {
    app.loop_mgr
        .cache_l3_product(SITE, code, at(minute), Some(object()));
}

/// The plan `poll_loop_scan_list_results` files for a listing naming `minutes`.
fn plan_for(minutes: &[u32]) -> rustdar_radar::loop_downloads::FramePlan {
    rustdar_radar::loop_downloads::FramePlan::new(
        SITE.to_string(),
        minutes
            .iter()
            .map(|&minute| {
                (
                    at(minute),
                    Identifier::new(format!("KTLX2024010100{minute:02}00_V06")),
                )
            })
            .collect(),
    )
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

/// **A 3D pane's own volume is kept, even with no loop naming it.**
///
/// `prepare_volume` has three arms, and the third — `App::extract_loop_volume` —
/// serves a volume pane whose target is neither the published live stamp nor the
/// base. That arm is reachable rather than vestigial: the scan drain's
/// `feed_is_ahead` branch *moves* an archive volume into the loop cache and
/// writes neither `scan_data` nor `base_scans`. Swept from under such a target,
/// `prepare_volume` answers `Waiting` — and it is level-triggered, so the pane
/// re-asks and is refused every frame for the rest of the session, showing a 3D
/// view that never builds.
///
/// So the retention set carries each pane's own `(scan_info.site,
/// scan_info.timestamp)` alongside the loop frames. Asserted with **no loop at
/// all**, because a loop frame naming the same volume would make it pass for the
/// wrong reason.
#[test]
fn the_volume_a_pane_is_viewing_survives_with_no_loop_naming_it() {
    let mut app = app_on_site();
    let info = rustdar_radar::types::ScanInfo::from_scan(&empty_scan(), SITE, at(3), None);
    app.gui
        .apply(rustdar_egui::shell_api::GuiEvent::ScanInfoForPane { pane_idx: 0, info });
    app.loop_mgr.cache_scan(SITE, at(3), volume());
    app.loop_mgr.cache_scan(SITE, at(9), volume());
    assert!(
        !app.gui
            .pane(0)
            .expect("a fresh Gui has one pane")
            .loop_state
            .is_active(),
        "precondition: no loop names anything, so only the pane's own view can \
         keep an entry",
    );

    app.evict_unshown_scans();

    assert!(
        app.loop_mgr.get_cached(SITE, &at(3)).is_some(),
        "the volume this pane is viewing was swept out from under it; a 3D pane \
         served by `prepare_volume`'s loop-cache arm then answers `Waiting` \
         every frame for the rest of the session",
    );
    assert!(
        app.loop_mgr.get_cached(SITE, &at(9)).is_none(),
        "the pane's own entry is a two-entry exception, not a licence for the \
         whole site",
    );
}

/// **The clock on the grace rule.** A listing that never returns stops exempting
/// its site.
///
/// This is not a tidiness bound. On wasm32 nothing else ever ends the wait:
/// `rustdar_radar::tls::client` accepts and ignores its timeout because
/// reqwest's wasm `ClientBuilder` has none and a browser `fetch()` has no
/// default, so a black-holed connection leaves the future pending for the life
/// of the tab. `settle_loop_phase` returns early on an empty frame list and
/// `accept_scan_listing` never runs, so the phase never moves while the poll and
/// chunk-feed paths go on writing a volume per seal — the full-rate leak, inside
/// the address space the sweep exists to protect. Natively the wait ends, but at
/// `ARCHIVE_TIMEOUT` = 300 s *per request* and one listing per UTC day, which is
/// minutes rather than a round-trip.
///
/// Both sides of the bound are asserted from one fixture, so a rule that simply
/// stopped exempting anything would fail the first half rather than pass this.
#[test]
fn a_listing_that_never_returns_stops_exempting_its_site() {
    let mut app = app_on_site();
    begin_loop(&mut app, 3600);
    for minute in [0, 4, 8] {
        app.loop_mgr.cache_scan(SITE, at(minute), volume());
    }

    // Just inside the bound: still exempt.
    age_listing(
        &mut app,
        rustdar_device_profile::constants::LOOP_LISTING_GRACE.as_secs() / 2,
    );
    app.evict_unshown_scans();
    assert_eq!(
        app.loop_mgr.cached_scan_count(SITE),
        3,
        "a listing well inside the grace window lost its site's window anyway, \
         so an ordinary product switch re-downloads what it already had",
    );

    // Past it: the exemption is gone, and the loop still names no frame.
    age_listing(
        &mut app,
        rustdar_device_profile::constants::LOOP_LISTING_GRACE.as_secs() + 1,
    );
    app.evict_unshown_scans();
    assert!(
        app.gui
            .pane(0)
            .expect("a fresh Gui has one pane")
            .loop_state
            .is_fetching(),
        "precondition: nothing moved the phase — this is the stuck listing, not \
         a loop that quietly finished",
    );
    assert_eq!(
        app.loop_mgr.cached_scan_count(SITE),
        0,
        "a listing that never returns exempts its site for ever; on wasm32 \
         nothing else ends that wait, so the leak resumes at full rate",
    );
}

/// **The frame plan is swept by the same predicate as the cache.**
///
/// `FramePlan::frames` is the original listing and `append_polled_frame` never
/// prunes it as the window walks forward. While the cache was unbounded that did
/// not show: a retired frame's volume stayed resident, so
/// `dispatch_pending_loop_downloads`' `is_cached` filter dropped its queue entry
/// and nothing re-downloaded it. With the cache swept, a re-plan on the next
/// product switch would re-queue every retired frame — up to `MAX_LOOP_FRAMES`
/// ~10 MB volumes, downloaded, cached, and evicted by the very next sweep, while
/// holding the shared download slots the live frames are waiting on.
///
/// The queue is asserted through the real re-derivation
/// (`plan_downloads_for` under a changed product), not by reading the plan back,
/// because the queue is what actually spends the network.
#[test]
fn a_retired_frame_is_not_re_queued_after_the_window_moves() {
    let mut app = app_on_site();
    begin_loop(&mut app, 600);
    install_listing(&mut app, &[0, 2, 4]);
    // The plan the listing produced, as `poll_loop_scan_list_results` files it.
    app.loop_mgr.set_plan(0, plan_for(&[0, 2, 4]));
    assert_eq!(
        app.loop_mgr.plan_frame_count(0),
        3,
        "precondition: the plan names every frame of the original listing",
    );

    // The window walks forward until the first two frames are retired.
    poll_scan(&mut app, 12);
    poll_scan(&mut app, 14);
    assert_eq!(frames(&app), vec![at(4), at(12), at(14)]);
    assert_eq!(
        app.loop_mgr.plan_frame_count(0),
        3,
        "precondition: the append path prunes the loop's frames and never the \
         plan — the divergence this sweep has to close",
    );

    app.evict_unshown_scans();

    assert_eq!(
        app.loop_mgr.plan_frame_count(0),
        1,
        "the plan still names frames the window retired, so the next re-plan \
         re-downloads a whole retired window",
    );

    // A product switch, which is what re-derives the queue from the plan.
    assert!(
        app.loop_mgr.plan_downloads_for(0, RadarProduct::Velocity),
        "precondition: a product the plan was not derived for really does \
         re-derive the queue",
    );
    assert_eq!(
        app.loop_mgr.pending_queue_count(0),
        1,
        "the re-derived queue would fetch volumes for frames that no longer \
         exist, spending the shared download slots the live frames need",
    );
}

/// **The sibling leak.** Paired Level III objects nothing names accumulated
/// exactly as the volumes did.
///
/// `cache_l3_product` writes one entry per frame per AWIPS code — the gaps
/// deliberately included, so a frame is retired once instead of re-paired every
/// pass — and until this sweep only the site switch's wholesale clear ever took
/// one out, and never otherwise. Every re-listing leaves the
/// previous window behind: a product switch, a time navigation,
/// `reinit_active_loops`.
///
/// The count is asserted **before** the sweep as well as after, so this cannot
/// pass against an empty cache.
#[test]
fn paired_objects_no_live_frame_names_are_not_kept() {
    const PAIRED: u32 = 6;

    let mut app = app_on_site();
    for minute in 0..PAIRED {
        pair(&mut app, "EET", minute);
    }

    assert_eq!(
        app.loop_mgr.cached_l3_count(SITE),
        PAIRED as usize,
        "precondition: the pairing path really did cache every object, so the \
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
        app.loop_mgr.cached_l3_count(SITE),
        0,
        "the objects of a window nothing is playing are still resident; \
         nothing else in this crate ever removes one",
    );
}

/// **The day listings are swept by site**, which is the resolution their key
/// has.
///
/// `l3_keys` holds the bucket keys a site's objects are ranked against —
/// `(site, AWIPS code)`, no volume in it — so the sweep can only ask whether
/// anything still needs the site. Nothing removed one before: the site switch's
/// wholesale clear did it, and that clear had to go because it took every other
/// site's state with it.
///
/// Worse than the bytes if it is missed: `claim_l3_listing` refuses to re-list a
/// `(site, code)` this map already holds, and the days a listing covers come
/// from the frames that asked for it — so a listing kept past its loop is re-used
/// for a window it does not cover and every frame outside it reads as a gap.
///
/// Asserted with a **shown** site alongside the departed one, so it cannot pass
/// against a sweep that simply empties the map.
#[test]
fn a_departed_sites_day_listings_go_and_a_shown_sites_stay() {
    let mut app = app_on_site();
    let info = rustdar_radar::types::ScanInfo::from_scan(&empty_scan(), SITE, at(0), None);
    app.gui
        .apply(rustdar_egui::shell_api::GuiEvent::ScanInfoForPane { pane_idx: 0, info });
    app.loop_mgr
        .cache_l3_keys(SITE, "EET", vec!["TLX_EET_2024_01_01_00_00_30".to_string()]);
    app.loop_mgr.cache_l3_keys(
        "KOUN",
        "EET",
        vec!["OUN_EET_2024_01_01_00_00_30".to_string()],
    );
    assert!(
        app.loop_mgr.l3_keys(SITE, "EET").is_some()
            && app.loop_mgr.l3_keys("KOUN", "EET").is_some(),
        "precondition: both listings are in hand, so the sweep has something to \
         remove and something to keep",
    );

    app.evict_unshown_scans();

    assert!(
        app.loop_mgr.l3_keys(SITE, "EET").is_some(),
        "the listing for the site this pane is showing went, so every pairing \
         it makes re-lists the days first",
    );
    assert!(
        app.loop_mgr.l3_keys("KOUN", "EET").is_none(),
        "a site no pane names kept its day listing; nothing else removes one, \
         and `claim_l3_listing` will refuse to re-make it for a window whose \
         days it does not cover",
    );
}

/// **The keep, on the Level III side.** Every object a live loop frame names
/// survives, and the lookup the renderer makes still resolves.
#[test]
fn a_live_level3_loops_frames_keep_their_objects() {
    let mut app = app_on_site();
    begin_loop(&mut app, 3600);
    install_listing(&mut app, &[0, 4, 8]);
    for minute in [0, 4, 8] {
        pair(&mut app, "EET", minute);
    }
    // An object no frame names, from a window this loop has moved past.
    pair(&mut app, "EET", 99);
    assert_eq!(app.loop_mgr.cached_l3_count(SITE), 4);

    app.evict_unshown_scans();

    let target = RenderTarget::new(SITE, RadarProduct::EchoTops, 0.5);
    for minute in [0, 4, 8] {
        assert!(
            matches!(
                frame_data(&app.loop_mgr, &target, at(minute)),
                Some(LoopFrameData::Products(_)),
            ),
            "minute {minute}: a frame the loop is playing lost its object, so \
             its dispatch pairs the volume again on every pass",
        );
    }
    assert!(
        !app.loop_mgr.l3_is_resolved(SITE, "EET", &at(99)),
        "an object no frame names survived, which is the leak this sweep \
         exists to close",
    );
}

/// **A product switch does not shed the window.**
///
/// The reason the retention rule ignores the AWIPS code. The frames do not move
/// when the product does — both frame lists come from a Level II archive
/// listing and `retarget_renders_keyed` re-renders without re-listing — so a
/// rule that asked which codes the pane's *current* product reads would evict
/// the objects of frames still in the window, and the switch back would re-pair
/// every one of them at up to `PAIRING_CANDIDATES` object fetches apiece.
///
/// The pane is moved to a product reading **neither** cached code, which is the
/// case a code-aware rule gets wrong most completely.
#[test]
fn switching_product_keeps_the_objects_of_frames_still_in_the_window() {
    let mut app = app_on_site();
    begin_loop(&mut app, 3600);
    install_listing(&mut app, &[0, 4]);
    for code in ["DVL", "EET"] {
        for minute in [0, 4] {
            pair(&mut app, code, minute);
        }
    }
    assert_eq!(
        app.loop_mgr.cached_l3_count(SITE),
        4,
        "precondition: two codes over the two volumes the window names",
    );

    app.gui
        .pane_mut(0)
        .expect("a fresh Gui has one pane")
        .selected_product = RadarProduct::SpecificDifferentialPhase;

    app.evict_unshown_scans();

    for code in ["DVL", "EET"] {
        for minute in [0, 4] {
            assert!(
                app.loop_mgr.l3_is_resolved(SITE, code, &at(minute)),
                "{code} at minute {minute}: an object of a frame still in the \
                 window went when the pane changed product, so switching back \
                 re-pairs a volume the loop never stopped naming",
            );
        }
    }
}

/// **The grace rule reaches the Level III cache too**, and expires there too.
///
/// One predicate governs both caches, so a loop whose listing is in flight —
/// naming no frame at all — keeps its objects for exactly as long as it keeps
/// its volumes, and loses them on the same terms.
#[test]
fn a_loop_still_fetching_its_listing_keeps_its_objects() {
    let mut app = app_on_site();
    begin_loop(&mut app, 3600);
    for minute in [0, 4, 8] {
        pair(&mut app, "EET", minute);
    }
    assert!(
        frames(&app).is_empty(),
        "precondition: a loop fetching its listing names no frame at all",
    );

    app.evict_unshown_scans();

    assert_eq!(
        app.loop_mgr.cached_l3_count(SITE),
        3,
        "a loop's objects went in the gap before its listing landed, so every \
         product switch and every re-init re-pairs the whole window",
    );

    // The listing lands and names two of the three.
    install_listing(&mut app, &[0, 4]);
    app.evict_unshown_scans();

    assert_eq!(
        app.loop_mgr.cached_l3_count(SITE),
        2,
        "the grace rule outlived the listing it was granted for",
    );
    assert!(
        !app.loop_mgr.l3_is_resolved(SITE, "EET", &at(8)),
        "the entry the new listing does not name survived the sweep that \
         followed it",
    );
}

/// **A retired frame is not re-paired after the window moves.**
///
/// The Level III half of `a_retired_frame_is_not_re_queued_after_the_window_moves`,
/// and it needs its own sweep: `retain_plan_frames` deliberately leaves the
/// pairing queue alone, because a volume-cache answer cannot judge pairings.
/// `dispatch_pending_loop_l3_pairings` drops a queue entry that `l3_is_resolved`
/// calls answered, so once the cache is swept an unswept queue re-pairs every
/// retired frame — holding the shared `concurrent_loop_downloads` slots the live
/// frames are waiting on.
#[test]
fn a_retired_frame_is_not_re_paired_after_the_window_moves() {
    let mut app = app_on_site();
    begin_loop(&mut app, 600);
    install_listing(&mut app, &[0, 2, 4]);
    app.loop_mgr.set_plan(0, plan_for(&[0, 2, 4]));
    assert!(
        app.loop_mgr.plan_downloads_for(0, RadarProduct::EchoTops),
        "precondition: a Level III product queues pairings rather than volumes",
    );
    assert_eq!(
        app.loop_mgr.pending_l3_queue_count(0),
        3,
        "precondition: one pairing per frame of the original listing",
    );

    // The window walks forward until the first two frames are retired.
    poll_scan(&mut app, 12);
    poll_scan(&mut app, 14);
    assert_eq!(frames(&app), vec![at(4), at(12), at(14)]);
    assert_eq!(
        app.loop_mgr.pending_l3_queue_count(0),
        3,
        "precondition: the append path prunes the loop's frames and never the \
         pairing queue — the divergence this sweep has to close",
    );

    app.evict_unshown_scans();

    assert_eq!(
        app.loop_mgr.pending_l3_queue_count(0),
        1,
        "the queue still owes pairings for frames the window retired, so each \
         is fetched again and evicted by the very next sweep",
    );
}
