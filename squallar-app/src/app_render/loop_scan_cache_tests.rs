use super::*;
use crate::app::tests::{empty_scan, headless};
use crate::platform_double::TestBridge;
use crate::test_keys;
use squallar_egui::pane::LoopPhase;
use squallar_radar::types::{RadarProduct, RenderView};
use squallar_source::id::known;

const SITE: &str = "KTLX";

fn at(minute: u32) -> chrono::NaiveDateTime {
    chrono::NaiveDate::from_ymd_opt(2024, 1, 1)
        .unwrap()
        .and_hms_opt(0, 0, 0)
        .unwrap()
        + chrono::Duration::minutes(minute as i64)
}

fn volume() -> squallar_radar::loop_downloads::CachedVolume {
    (Arc::new(empty_scan()), Default::default())
}

fn app_on_site() -> crate::app::App {
    let mut app = headless(TestBridge::desktop());
    app.gui
        .pane_mut(0)
        .expect("a fresh Gui has one pane")
        .set_site(SITE.to_string());
    app
}

fn begin_loop(app: &mut crate::app::App, lookback_secs: u64) {
    let site = squallar_radar::sites::get_radar_site(SITE)
        .expect("KTLX is in the resolved site table")
        .clone();
    let pane = app.gui.pane_mut(0).expect("a fresh Gui has one pane");
    *pane.time_state_mut(&known::RADAR) =
        squallar_egui::radar_layer::begin_loop(lookback_secs, &site, RenderView::PlanView);
    assert_eq!(
        pane.time_state(&known::RADAR).phase,
        LoopPhase::FetchingScanList,
        "precondition: a freshly built loop is waiting on its listing",
    );
    assert!(
        pane.time_state(&known::RADAR).listing_since.is_some(),
        "precondition: entering the fetching phase starts the clock on the \
         grace exemption",
    );
}

fn age_listing(app: &mut crate::app::App, secs: u64) {
    let pane = app.gui.pane_mut(0).expect("a fresh Gui has one pane");
    let since = pane
        .time_state(&known::RADAR)
        .listing_since
        .expect("the loop is fetching, so it has a clock");
    pane.time_state_mut(&known::RADAR).listing_since = Some(
        since
            .checked_sub(std::time::Duration::from_secs(secs))
            .expect("the monotonic clock's origin is younger than the grace bound, which needs a host booted seconds ago"),
    );
}

fn install_listing(app: &mut crate::app::App, minutes: &[u32]) {
    let allocation = test_loop_allocation();
    let budgets = test_budgets();
    let scans: Vec<_> = minutes.iter().map(|&minute| at(minute)).collect();
    let pane = app.gui.pane_mut(0).expect("a fresh Gui has one pane");
    accept_scan_listing_for_test(
        &allocation,
        &budgets,
        pane.time_state_mut(&known::RADAR),
        SITE,
        scans,
        1,
    )
    .expect("a non-empty listing for this loop's own site is accepted");
    assert_eq!(
        pane.time_state(&known::RADAR).frames.len(),
        minutes.len(),
        "precondition: the listing became frames without being sampled",
    );
}

fn poll_scan(app: &mut crate::app::App, minute: u32) {
    let (scan, declared) = volume();
    app.append_scan_to_active_loops(SITE, at(minute), scan, declared);
}

fn object() -> Arc<squallar_radar::level3::Level3Product> {
    Arc::new(squallar_radar::level3::Level3Product {
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
        stamp: squallar_radar::level3::ProductStamp::from_key("TLX_EET_2024_01_01_00_01_30"),
        bytes: Arc::new(Vec::new()),
    })
}

fn pair(app: &mut crate::app::App, code: &str, minute: u32) {
    app.loop_mgr
        .cache_l3_product(SITE, code, at(minute), Some(object()));
}

fn plan_for(minutes: &[u32]) -> squallar_radar::loop_downloads::FramePlan {
    squallar_radar::loop_downloads::FramePlan::new(
        SITE.to_string(),
        minutes.iter().map(|&minute| at(minute)).collect(),
    )
}

fn frames(app: &crate::app::App) -> Vec<chrono::NaiveDateTime> {
    app.gui
        .pane(0)
        .expect("a fresh Gui has one pane")
        .time_state(&known::RADAR)
        .frames
        .iter()
        .map(|frame| frame.timestamp)
        .collect()
}

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
            .time_state(&known::RADAR)
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

#[test]
fn a_live_loops_frames_keep_their_volumes() {
    let mut app = app_on_site();
    begin_loop(&mut app, 3600);
    install_listing(&mut app, &[0, 4, 8]);
    for minute in [0, 4, 8] {
        app.loop_mgr.cache_scan(SITE, at(minute), volume());
    }
    app.loop_mgr.cache_scan(SITE, at(99), volume());
    assert_eq!(app.loop_mgr.cached_scan_count(SITE), 4);

    app.evict_unshown_scans();

    let target = test_keys::key(SITE, &squallar_radar::fields::known::REFLECTIVITY, 0.5);
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

#[test]
fn a_window_that_moved_sheds_the_volumes_its_old_frames_named() {
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

#[test]
fn the_volume_a_pane_is_viewing_survives_with_no_loop_naming_it() {
    let mut app = app_on_site();
    let info = squallar_radar::types::ScanInfo::from_scan(&empty_scan(), SITE, at(3), None);
    app.gui
        .apply(squallar_egui::shell_api::GuiEvent::ScanInfoForPane { pane_idx: 0, info });
    app.loop_mgr.cache_scan(SITE, at(3), volume());
    app.loop_mgr.cache_scan(SITE, at(9), volume());
    assert!(
        !app.gui
            .pane(0)
            .expect("a fresh Gui has one pane")
            .time_state(&known::RADAR)
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

#[test]
fn a_listing_that_never_returns_stops_exempting_its_site() {
    let mut app = app_on_site();
    begin_loop(&mut app, 3600);
    for minute in [0, 4, 8] {
        app.loop_mgr.cache_scan(SITE, at(minute), volume());
    }

    age_listing(
        &mut app,
        squallar_device_profile::constants::LOOP_LISTING_GRACE.as_secs() / 2,
    );
    app.evict_unshown_scans();
    assert_eq!(
        app.loop_mgr.cached_scan_count(SITE),
        3,
        "a listing well inside the grace window lost its site's window anyway, \
         so an ordinary product switch re-downloads what it already had",
    );

    age_listing(
        &mut app,
        squallar_device_profile::constants::LOOP_LISTING_GRACE.as_secs() + 1,
    );
    app.evict_unshown_scans();
    assert!(
        app.gui
            .pane(0)
            .expect("a fresh Gui has one pane")
            .time_state(&known::RADAR)
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

#[test]
fn a_retired_frame_is_not_re_queued_after_the_window_moves() {
    let mut app = app_on_site();
    begin_loop(&mut app, 600);
    install_listing(&mut app, &[0, 2, 4]);
    app.loop_mgr.set_plan(0, plan_for(&[0, 2, 4]));
    assert_eq!(
        app.loop_mgr.plan_frame_count(0),
        3,
        "precondition: the plan names every frame of the original listing",
    );

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
            .time_state(&known::RADAR)
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

#[test]
fn a_departed_sites_day_listings_go_and_a_shown_sites_stay() {
    let mut app = app_on_site();
    let info = squallar_radar::types::ScanInfo::from_scan(&empty_scan(), SITE, at(0), None);
    app.gui
        .apply(squallar_egui::shell_api::GuiEvent::ScanInfoForPane { pane_idx: 0, info });
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

#[test]
fn a_live_level3_loops_frames_keep_their_objects() {
    let mut app = app_on_site();
    begin_loop(&mut app, 3600);
    install_listing(&mut app, &[0, 4, 8]);
    for minute in [0, 4, 8] {
        pair(&mut app, "EET", minute);
    }
    pair(&mut app, "EET", 99);
    assert_eq!(app.loop_mgr.cached_l3_count(SITE), 4);

    app.evict_unshown_scans();

    let target = test_keys::key(SITE, &squallar_radar::fields::known::ECHO_TOPS, 0.5);
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
        .set_selected_product(squallar_radar::fields::known::SPECIFIC_DIFFERENTIAL_PHASE);

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

/// Run the sweep over a pane whose radar loop holds minutes 0/4/8, with the
/// transport optionally moved off radar first, and answer which of those
/// minutes still has its decoded volume afterwards.
///
/// `driven_by` is the layer the pane's transport addresses. `None` leaves it
/// where a fresh pane puts it, which is radar.
fn volumes_surviving_the_sweep(driven_by: Option<squallar_source::id::LayerId>) -> Vec<u32> {
    const HELD: [u32; 3] = [0, 4, 8];

    let mut app = app_on_site();
    begin_loop(&mut app, 3600);
    install_listing(&mut app, &HELD);
    for minute in HELD {
        app.loop_mgr.cache_scan(SITE, at(minute), volume());
    }

    if let Some(id) = driven_by {
        let pane = app.gui.pane_mut(0).expect("a fresh Gui has one pane");
        // A *running* second timeline, because that is the reachable state:
        // `PaneState::refresh_transport` returns early while the transport's
        // own loop is active, so a pane that armed a satellite loop and then
        // enabled radar keeps the controls on the satellite while radar
        // animates underneath.
        pane.set_transport_layer(id.clone());
        let other = pane.time_state_mut(&id);
        other.phase = LoopPhase::Playing;
        other.span_secs = 43_200;
        // A stamp of its own, nowhere near radar's, so a keep-set built from
        // this timeline cannot accidentally name radar's volumes.
        other.frames = vec![squallar_egui::pane::LoopFrame {
            timestamp: at(500),
            image: None,
            render_in_flight: false,
            render_failed: false,
        }];
        assert!(
            !std::ptr::eq(pane.transport_state(), pane.time_state(&known::RADAR)),
            "precondition: the transport really addresses another timeline, or \
             the two reads are one object and the case is vacuous",
        );
        assert!(
            pane.time_state(&known::RADAR).is_active(),
            "precondition: radar is still animating — the whole point is that \
             its volumes are needed while something else holds the controls",
        );
    }

    app.evict_unshown_scans();

    HELD.into_iter()
        .filter(|minute| app.loop_mgr.get_cached(SITE, &at(*minute)).is_some())
        .collect()
}

/// **The eviction keep-set is keyed off radar's own timeline, never off the
/// transport's.**
///
/// `App::evict_unneeded_loop_scans` retains four radar-only holders —
/// `retain_plan_frames`, `retain_scans`, `retain_l3` and `retain_l3_keys` —
/// and every one of them is keyed by NEXRAD site. The site comes from
/// `radar_layer::site(ls)`, which reads the geometry anchor **only a radar
/// timeline carries**: a satellite, MRMS or model timeline's anchor is
/// `Box::new(())` and answers `""`.
///
/// So a transport-addressed read here does not merely retain the wrong set —
/// it files the whole keep-set under the empty site, `keep(SITE, ts)` answers
/// false for every entry, and **the volumes the pane is actually playing are
/// evicted on the next sweep** and re-downloaded on the pass after it.
///
/// **Reachable, not contrived** — see `volumes_surviving_the_sweep`.
///
/// **The floor is the first case**, and it is the common configuration: with
/// radar driving, the two reads are the same object and the keep-set is
/// identical either way. Without it, a test that only ran the moved-transport
/// case could pass because the fixture retained nothing in either arm.
///
/// WO-T3.7 wrote the reason for this keep into `app.rs` as a comment.
/// Retargeting the read at the transport passed all 668 tests in the crate, so
/// the comment was prose with no gate under it; this is the gate.
#[test]
fn the_eviction_keep_set_is_keyed_off_radars_timeline_not_the_transports() {
    let radar_driving = volumes_surviving_the_sweep(None);
    assert_eq!(
        radar_driving,
        vec![0, 4, 8],
        "floor: with radar driving the transport, every minute the loop names \
         keeps its volume — if this arm retains nothing the comparison below \
         is satisfied by two empty sets",
    );

    let satellite_driving = volumes_surviving_the_sweep(Some(known::GMGSI));
    assert_eq!(
        satellite_driving, radar_driving,
        "a pane whose transport sits on a satellite loop while radar animates \
         underneath lost the volumes of the frames radar is playing. The \
         keep-set is built from `radar_layer::site(ls)`, which answers \"\" for \
         any timeline but radar's, so reading anything other than \
         `time_state(&known::RADAR)` here files every frame under the empty \
         site and evicts the real one's volumes — which the loop then \
         re-downloads on every pass",
    );
}

// ----------------------------------------------------------------------
// Conditional retention: a frame's decoded volume is dead weight where no
// loop on its site derives anything from it.
// ----------------------------------------------------------------------

/// A decoded volume that actually carries gates, so it prices at something.
///
/// The shared [`volume`] fixture declares no sweeps, and a volume of no
/// sweeps correctly prices at zero — which is exactly the value a byte
/// assertion could not tell from a broken total. Eight radials of 400 gates
/// is the same shape `loop_downloads`' own byte suite uses.
///
/// The scan alone, not the cache's pair: naming that alias here would raise a
/// coupling ceiling (`arch_ratchets::the_loop_frame_arms_stay_radars_own_vocabulary`),
/// and the declarations half is `Default::default()` at every call anyway.
fn priced_scan() -> Arc<nexrad_model::data::Scan> {
    use nexrad_model::data::{
        MomentData, PulseWidth, Radial, RadialStatus, Scan, Sweep, VolumeCoveragePattern,
    };

    let radials = (0..8)
        .map(|i| {
            Radial::new(
                1_700_000_000_000,
                i,
                f32::from(i),
                0.5,
                RadialStatus::IntermediateRadialData,
                1,
                0.5,
                Some(MomentData::from_fixed_point(
                    400,
                    2125,
                    250,
                    8,
                    2.0,
                    66.0,
                    vec![3u8; 400],
                )),
                None,
                None,
                None,
                None,
                None,
                None,
            )
        })
        .collect();
    Arc::new(Scan::new(
        VolumeCoveragePattern::new(
            212,
            0,
            0.5,
            PulseWidth::Short,
            false,
            0,
            false,
            0,
            false,
            false,
            0,
            false,
            false,
            Vec::new(),
        ),
        vec![Sweep::new(1, radials)],
    ))
}

/// Say what this pane's loop renders, which is what `loop_product` reads and
/// therefore what `site_needs_decoded_source` is asked about.
fn set_loop_product(app: &mut crate::app::App, field: &squallar_source::product::FieldId) {
    let target = test_keys::key(SITE, field, 0.5);
    app.gui
        .pane_mut(0)
        .expect("a fresh Gui has one pane")
        .time_state_mut(&known::RADAR)
        .rendered_for = Some(target);
}

/// **The other in-application holders of a decoded `Arc<Scan>`**, summed the
/// way `App::publish_heap_census` publishes them, so this figure and the
/// census families agree by construction.
///
/// The census's radar families OVERLAP: the loop download cache, the still
/// inventory and the stored loop frames' hover sources hold `Arc`s of the same
/// `Scan`s, and each reports what it alone would free. So a loop-cache figure
/// that falls proves nothing on its own — the bytes may simply have changed
/// family. This is the other side of that sum, and the assertions below pin it
/// at zero on both sides of the sweep.
fn other_decoded_volume_holders(app: &crate::app::App) -> u64 {
    app.volumes.resident_scan_bytes() as u64 + app.loop_frames.pinned_volume_bytes()
}

/// File `MINUTES`' volumes into the loop cache and hand back a clone of each
/// scan, so the caller can prove who else is holding it.
fn fill_priced_volumes(
    app: &mut crate::app::App,
    minutes: &[u32],
) -> Vec<Arc<nexrad_model::data::Scan>> {
    minutes
        .iter()
        .map(|&minute| {
            let scan = priced_scan();
            app.loop_mgr
                .cache_scan(SITE, at(minute), (Arc::clone(&scan), Default::default()));
            scan
        })
        .collect()
}

const LOOPED: [u32; 6] = [0, 4, 8, 12, 16, 20];

/// **A loop whose product reads Level III objects drops the decoded Level II
/// volumes nothing on its site derives from.**
///
/// This is the largest single family in the heap census — ~150-200 MB in the
/// browser legs, ~650-700 MiB at desktop rungs, a measured 47.99 MiB median
/// per volume over a corpus of 108 real archive volumes. A pane that retargets
/// from Reflectivity to a Level III product keeps its frame list, and until
/// this sweep asked `site_needs_decoded_source` it kept every one of those
/// frames' decoded volumes too: `plan_downloads_for` had already stopped
/// downloading them, but nothing ever dropped the ones already held.
///
/// **The frame set is untouched.** Loop lookback and frame density are both
/// tier 1: this buys fewer bytes per frame, never fewer frames.
///
/// **And the bytes are freed, not moved.** The census's decoded-volume
/// families overlap by design, so a falling loop-cache figure is consistent
/// with the same `Arc`s simply being held somewhere else. The strong-count
/// precondition is what rules that out: at 2, the only holders are this test
/// and the loop cache, so the cache letting go is the last reference in the
/// application going.
#[test]
fn a_level3_loop_drops_the_decoded_volumes_nothing_derives_from() {
    let mut app = app_on_site();
    begin_loop(&mut app, 3600);
    install_listing(&mut app, &LOOPED);
    set_loop_product(&mut app, &squallar_radar::fields::known::PRECIPITATION_RATE);
    let held = fill_priced_volumes(&mut app, &LOOPED);

    let before = app.loop_mgr.cached_scan_bytes();
    let one = before / LOOPED.len();
    assert!(
        one > 0 && before == one * LOOPED.len(),
        "precondition: every frame's volume priced at something and the total \
         is their sum — a fixture that prices at zero makes every assertion \
         below vacuous ({before} bytes over {} volumes)",
        LOOPED.len(),
    );
    for scan in &held {
        assert_eq!(
            Arc::strong_count(scan),
            2,
            "precondition: something other than this test and the loop cache \
             is holding this volume, so dropping the cache entry would move \
             the bytes between census families instead of freeing them",
        );
    }
    assert_eq!(
        other_decoded_volume_holders(&app),
        0,
        "precondition: the still inventory or a stored loop frame is already \
         holding decoded volumes, so the sum below cannot separate a free from \
         a transfer",
    );

    app.evict_unshown_scans();

    assert_eq!(
        app.loop_mgr.cached_scan_bytes(),
        0,
        "a Level III loop is still holding {before} bytes of decoded Level II \
         volumes its frames derive nothing from — at the measured 47.99 MiB \
         median that is 672 MiB over a 14-frame loop, held for a retarget that \
         may never come",
    );
    assert_eq!(
        other_decoded_volume_holders(&app),
        0,
        "the decoded volumes left the loop cache and landed in another census \
         family, so the census reads better and the heap has not moved",
    );
    assert_eq!(
        frames(&app).len(),
        LOOPED.len(),
        "the sweep shortened the loop. Lookback and frame density are tier 1: \
         this pass buys fewer BYTES per frame, never fewer frames",
    );
    assert_eq!(
        app.loop_mgr.plan_frame_count(0),
        0,
        "sanity: this fixture never set a plan, so nothing here can claim the \
         plan was pruned",
    );
}

/// **The opposite direction: a site a Level II loop is playing keeps every
/// frame's volume.**
///
/// The failure this guards is the expensive one. Over-firing costs a
/// re-download and re-decode of the whole window on every sweep — a black
/// loop that never finishes settling — where under-firing costs only the
/// bytes this pass exists to save. A gate verified on the dropping arm alone
/// would not see it.
#[test]
fn a_level2_loop_keeps_every_frames_decoded_volume() {
    let mut app = app_on_site();
    begin_loop(&mut app, 3600);
    install_listing(&mut app, &LOOPED);
    set_loop_product(&mut app, &squallar_radar::fields::known::REFLECTIVITY);
    fill_priced_volumes(&mut app, &LOOPED);

    let before = app.loop_mgr.cached_scan_bytes();

    app.evict_unshown_scans();

    assert_eq!(
        app.loop_mgr.cached_scan_bytes(),
        before,
        "a playing Level II loop lost bytes to the conditional retention it is \
         explicitly exempt from",
    );
    for minute in LOOPED {
        assert!(
            app.loop_mgr.get_cached(SITE, &at(minute)).is_some(),
            "minute {minute}: the volume a playing Level II frame renders from \
             was dropped, so every pass re-downloads and re-decodes it",
        );
    }
}

/// **A loop that has not dispatched yet keeps its window.**
///
/// `loop_product` answers `None` before the first dispatch, and a loop that
/// has not said what it renders cannot be shown to need nothing. The safe
/// direction is the one that holds bytes.
#[test]
fn a_loop_that_has_not_said_what_it_renders_keeps_its_volumes() {
    let mut app = app_on_site();
    begin_loop(&mut app, 3600);
    install_listing(&mut app, &LOOPED);
    fill_priced_volumes(&mut app, &LOOPED);
    assert!(
        app.gui
            .pane(0)
            .expect("a fresh Gui has one pane")
            .time_state(&known::RADAR)
            .rendered_for
            .is_none(),
        "precondition: nothing has dispatched, so the product is unknown",
    );

    let before = app.loop_mgr.cached_scan_bytes();

    app.evict_unshown_scans();

    assert_eq!(
        app.loop_mgr.cached_scan_bytes(),
        before,
        "a loop before its first dispatch was read as deriving nothing, so its \
         window is evicted one frame before it is asked for",
    );
}

/// **The one volume a pane is parked at survives on a site nothing loops
/// from** — the peer's "textures plus one scan" rung, kept rather than
/// swept with the rest.
#[test]
fn a_level3_loop_keeps_the_one_volume_its_pane_is_parked_at() {
    const PARKED: u32 = 8;

    let mut app = app_on_site();
    begin_loop(&mut app, 3600);
    install_listing(&mut app, &LOOPED);
    set_loop_product(&mut app, &squallar_radar::fields::known::PRECIPITATION_RATE);
    fill_priced_volumes(&mut app, &LOOPED);
    let info = squallar_radar::types::ScanInfo::from_scan(&empty_scan(), SITE, at(PARKED), None);
    app.gui
        .apply(squallar_egui::shell_api::GuiEvent::ScanInfoForPane { pane_idx: 0, info });

    app.evict_unshown_scans();

    assert!(
        app.loop_mgr.get_cached(SITE, &at(PARKED)).is_some(),
        "the volume this pane is actually parked at was swept out from under \
         it; a 3D pane served by `prepare_volume`'s loop-cache arm then answers \
         `Waiting` for the rest of the session",
    );
    assert_eq!(
        app.loop_mgr.cached_scan_count(SITE),
        1,
        "the parked entry is a one-volume exception, not a licence for the \
         whole site's window",
    );
}
