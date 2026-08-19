use super::*;

fn item(level: GlmDataLevel, energy: Option<f32>, area: Option<f32>) -> GlmFlashItem {
    GlmFlashItem {
        flash: GlmFlash {
            lat: 35.0,
            lon: -97.0,
            energy,
            area,
            time: chrono::NaiveDate::from_ymd_opt(2026, 7, 24)
                .unwrap()
                .and_hms_opt(12, 0, 0)
                .unwrap(),
            satellite: GlmSatellite::GoesEast,
            level,
        },
        index: 0,
    }
}

/// The popup's key/value rows, in order.
fn rows(item: &GlmFlashItem) -> Vec<(String, String)> {
    let prefs = UserPreferences {
        timezone: rustdar_units::TimezonePreference::Utc,
        ..UserPreferences::default()
    };
    item.popup_content(&prefs)
        .sections
        .into_iter()
        .filter_map(|s| match s {
            PopupSection::KeyValueGrid(rows) => Some(rows),
            _ => None,
        })
        .flatten()
        .collect()
}

fn grid_keys(item: &GlmFlashItem) -> Vec<String> {
    rows(item).into_iter().map(|(k, _)| k).collect()
}

#[test]
fn event_popup_omits_area_row() {
    let keys = grid_keys(&item(GlmDataLevel::Event, Some(1.0e-14), None));
    assert!(
        !keys.iter().any(|k| k == "Area"),
        "events have no area in the L2 LCFA product, got rows {keys:?}"
    );
    assert!(keys.iter().any(|k| k == "Energy"));
}

#[test]
fn flash_and_group_popups_keep_area_row() {
    for level in [GlmDataLevel::Flash, GlmDataLevel::Group] {
        let keys = grid_keys(&item(level, Some(1.0e-14), Some(128.0)));
        assert!(
            keys.iter().any(|k| k == "Area"),
            "{level:?} must still display area, got rows {keys:?}"
        );
    }
}

/// An unreported energy omits the row rather than printing a number: an
/// `unwrap_or(0.0)` renders "0.00e0 J", claiming a measurement GLM cannot
/// express — every energy variable's `add_offset` alone is 2.85e-16.
#[test]
fn popup_omits_the_energy_row_when_energy_is_unknown() {
    let with = grid_keys(&item(GlmDataLevel::Flash, Some(1.0e-14), Some(278.65)));
    assert!(with.contains(&"Energy".to_string()), "rows: {with:?}");

    let without = grid_keys(&item(GlmDataLevel::Flash, None, Some(278.65)));
    assert!(
        !without.contains(&"Energy".to_string()),
        "an unknown energy must not produce a row: {without:?}"
    );
}

/// Locating fields survive whatever the descriptive fields do.
#[test]
fn popup_always_reports_position_even_with_both_fields_unknown() {
    let ks = grid_keys(&item(GlmDataLevel::Event, None, None));
    assert_eq!(ks, vec!["Type", "Latitude", "Longitude"], "rows: {ks:?}");
}

/// Values render in the units the fields document — joules and km², not
/// raw packed counts or square metres.
#[test]
fn popup_renders_reported_values_in_the_documented_units() {
    let r = rows(&item(GlmDataLevel::Flash, Some(7.5e-14), Some(300.0)));
    let get = |k: &str| {
        r.iter()
            .find(|(key, _)| key == k)
            .map(|(_, v)| v.clone())
            .unwrap_or_default()
    };
    assert_eq!(get("Energy"), "7.50e-14 J");
    assert_eq!(get("Area"), "300.0 km²");
}

fn dead_east() -> DeadFeed {
    DeadFeed {
        satellite: GlmSatellite::GoesEast,
        bucket: "noaa-goes16".into(),
        prefixes: vec!["GLM-L2-LCFA/2026/206/02/".into()],
    }
}

fn info_texts(handler: &GlmHandler) -> Vec<String> {
    let ctx = PaneControlContext {
        pane_idx: 0,
        pane_state: None,
    };
    handler
        .controls(&ctx)
        .into_iter()
        .filter_map(|i| match i {
            ControlItem::InfoText { text } => Some(text),
            _ => None,
        })
        .collect()
}

const BOTH: [GlmSatellite; 2] = [GlmSatellite::GoesEast, GlmSatellite::GoesWest];
const WEST_ONLY: [GlmSatellite; 1] = [GlmSatellite::GoesWest];

/// A dead feed must be visible without reading logs.
#[test]
fn dead_feed_is_surfaced_in_the_control_panel() {
    let mut handler = GlmHandler::new();
    handler.enabled = true;
    handler.report_feed_changes(&BOTH, vec![dead_east()]);

    let texts = info_texts(&handler);
    assert!(
        texts
            .iter()
            .any(|t| t.contains("noaa-goes16") && t.contains("GOES-19 (East)")),
        "control panel should name the empty bucket, got {texts:?}"
    );
}

/// A window gap must be visible without reading logs, and must not borrow the
/// dead feed's words: "bucket is empty" would send the reader to check the one
/// thing that is demonstrably fine.
#[test]
fn a_window_gap_is_surfaced_in_the_control_panel_in_its_own_words() {
    let mut handler = GlmHandler::new();
    handler.enabled = true;
    handler.report_window_gaps(
        &BOTH,
        vec![WindowGap {
            satellite: GlmSatellite::GoesEast,
            objects_seen: 180,
        }],
    );

    let texts = info_texts(&handler);
    assert!(
        texts
            .iter()
            .any(|t| t.contains("GOES-19 (East)") && t.contains("listing is healthy")),
        "the panel must name the satellite and clear the bucket, got {texts:?}",
    );
    assert!(
        !texts.iter().any(|t| t.contains("is empty")),
        "an empty bucket is the opposite condition, got {texts:?}",
    );
}

/// A gap that closes takes its notice with it, on the same `queried` rule the
/// dead-feed recovery uses: a satellite nobody asked about has not recovered.
#[test]
fn a_closed_window_gap_clears_its_notice_but_only_if_it_was_queried() {
    let gap = || {
        vec![WindowGap {
            satellite: GlmSatellite::GoesWest,
            objects_seen: 180,
        }]
    };
    let mut handler = GlmHandler::new();
    handler.enabled = true;
    handler.report_window_gaps(&BOTH, gap());
    handler.report_window_gaps(&[GlmSatellite::GoesEast], Vec::new());
    assert_eq!(
        handler.window_gaps.len(),
        1,
        "GOES-West was not queried this poll, so nothing was learned about it",
    );

    handler.report_window_gaps(&BOTH, Vec::new());
    assert!(
        handler.window_gaps.is_empty(),
        "queried and absent from the gaps is the recovery",
    );
}

/// **A poll that parsed nothing must not clear the drop notice.**
///
/// The 20 s poll interval races a ~20 s granule cadence, so a poll routinely
/// downloads nothing new, parses nothing, and knows nothing about records. Its
/// tally is `considered: 0` — indistinguishable, by count alone, from "looked
/// at some and dropped none". Taking it at face value would blank a standing
/// notice on roughly every other poll, which is worse than not reporting: a
/// notice that flickers reads as noise.
#[test]
fn a_poll_that_parsed_no_granule_does_not_clear_the_drop_notice() {
    let mut handler = GlmHandler::new();
    handler.enabled = true;
    handler.report_record_drops(RecordDrops {
        considered: 900,
        fill_values: 0,
        off_globe: 12,
    });
    assert!(
        info_texts(&handler).iter().any(|t| t.contains("12")),
        "the drop is standing",
    );

    handler.report_record_drops(RecordDrops::default());
    let texts = info_texts(&handler);
    assert!(
        texts.iter().any(|t| t.contains("12 of 900")),
        "a poll that examined no record cannot vouch for any, got {texts:?}",
    );

    // ...but a poll that *did* look and found nothing wrong is a real recovery.
    handler.report_record_drops(RecordDrops {
        considered: 900,
        fill_values: 0,
        off_globe: 0,
    });
    assert!(
        !info_texts(&handler).iter().any(|t| t.contains("dropped")),
        "a poll that examined records and dropped none clears the notice",
    );
}

#[test]
fn recovered_feed_clears_the_control_panel_notice() {
    let mut handler = GlmHandler::new();
    handler.enabled = true;
    handler.report_feed_changes(&BOTH, vec![dead_east()]);
    handler.report_feed_changes(&BOTH, Vec::new());

    let texts = info_texts(&handler);
    assert!(
        !texts.iter().any(|t| t.contains("noaa-goes16")),
        "notice should clear once the feed returns, got {texts:?}"
    );
}

/// Repeated polls in the same state must not accumulate, and re-entering
/// the dead state must be reportable again.
#[test]
fn repeated_polls_do_not_accumulate_feed_state() {
    let mut handler = GlmHandler::new();
    handler.enabled = true;

    for _ in 0..5 {
        handler.report_feed_changes(&BOTH, vec![dead_east()]);
    }
    assert_eq!(handler.dead_feeds.len(), 1);
    assert_eq!(
        info_texts(&handler)
            .iter()
            .filter(|t| t.contains("noaa-goes16"))
            .count(),
        1
    );

    handler.report_feed_changes(&BOTH, Vec::new());
    assert!(handler.dead_feeds.is_empty());

    handler.report_feed_changes(&BOTH, vec![dead_east()]);
    assert_eq!(handler.dead_feeds.len(), 1);
}

/// A failed fetch tells us nothing about liveness, so the previous verdict
/// must stand.
#[test]
fn failed_fetch_leaves_feed_verdict_untouched() {
    let mut handler = GlmHandler::new();
    handler.enabled = true;
    handler.report_feed_changes(&BOTH, vec![dead_east()]);

    handler.apply_fetch_result(Box::new(GlmFetchResult(Err(
        crate::fetch_policy::FetchError::transient("network down"),
    ))));

    assert_eq!(handler.dead_feeds, vec![dead_east()]);
}

/// Deselecting a dead satellite must not read as recovery — switching to
/// West to work around a dead East is the likely reaction to the notice.
#[test]
fn deselecting_a_dead_satellite_is_not_a_recovery() {
    let mut handler = GlmHandler::new();
    handler.enabled = true;
    handler.satellite = SatelliteSelection::Both;
    handler.report_feed_changes(&BOTH, vec![dead_east()]);

    // User switches to West only; East is no longer queried.
    handler.satellite = SatelliteSelection::West;
    handler.report_feed_changes(&WEST_ONLY, Vec::new());

    assert_eq!(
        handler.dead_feeds,
        vec![dead_east()],
        "an unqueried satellite's verdict must be carried forward, not cleared"
    );
    // ...and the stale notice is not shown while East is deselected.
    assert!(
        !info_texts(&handler)
            .iter()
            .any(|t| t.contains("noaa-goes16")),
        "a deselected satellite should not occupy the panel"
    );
}

/// Switching back does not re-fire the "is dead" warning — no alternating
/// dead/recovered pairs driven purely by dropdown clicks.
#[test]
fn reselecting_a_still_dead_satellite_does_not_re_report_it() {
    let mut handler = GlmHandler::new();
    handler.enabled = true;
    handler.report_feed_changes(&BOTH, vec![dead_east()]);
    handler.report_feed_changes(&WEST_ONLY, Vec::new());
    handler.report_feed_changes(&BOTH, vec![dead_east()]);

    assert_eq!(handler.dead_feeds, vec![dead_east()]);
    assert_eq!(
        info_texts(&handler)
            .iter()
            .filter(|t| t.contains("noaa-goes16"))
            .count(),
        1,
        "the notice should appear once, not duplicate per selection change"
    );
}

/// Recovery is still reported for a satellite that *was* queried.
#[test]
fn recovery_is_still_reported_for_a_queried_satellite() {
    let mut handler = GlmHandler::new();
    handler.enabled = true;
    handler.report_feed_changes(&BOTH, vec![dead_east()]);
    handler.report_feed_changes(&BOTH, Vec::new());

    assert!(handler.dead_feeds.is_empty());
}

fn total_failure() -> FetchFailures {
    FetchFailures {
        in_window: 12,
        failed: 12,
        sample_error: "GLM file has no 'flash_lat' variable (product schema change?)".into(),
    }
}

fn partial_failure(failed: usize) -> FetchFailures {
    FetchFailures {
        in_window: 12,
        failed,
        sample_error: "boom".into(),
    }
}

/// A healthy S3 listing with every granule failing to parse must not read
/// as "Updated 0s ago".
#[test]
fn total_parse_failure_is_surfaced_in_the_control_panel() {
    let mut handler = GlmHandler::new();
    handler.enabled = true;
    handler.report_failures(FailureKind::Parse, Some(total_failure()));

    let texts = info_texts(&handler);
    assert!(
        texts.iter().any(|t| t.contains("failed to parse")),
        "a blank map from parse failures must be explained on screen, got {texts:?}"
    );
    assert!(
        handler.dead_feeds.is_empty(),
        "this failure mode has a healthy listing; it must not be reported as a dead feed"
    );
}

#[test]
fn partial_parse_failure_is_distinguished_from_total() {
    let mut handler = GlmHandler::new();
    handler.enabled = true;
    handler.report_failures(FailureKind::Parse, Some(partial_failure(3)));

    let texts = info_texts(&handler);
    assert!(
        texts.iter().any(|t| t.contains("3/12")),
        "partial failures should report counts, got {texts:?}"
    );
    assert!(
        !texts.iter().any(|t| t.contains("All ")),
        "partial failure must not claim everything failed, got {texts:?}"
    );
}

#[test]
fn parse_failure_notice_clears_when_files_parse_again() {
    let mut handler = GlmHandler::new();
    handler.enabled = true;
    handler.report_failures(FailureKind::Parse, Some(total_failure()));
    handler.report_failures(FailureKind::Parse, None);

    assert!(
        !info_texts(&handler)
            .iter()
            .any(|t| t.contains("failed to parse")),
        "notice should clear once parsing recovers"
    );
}

/// Health is a category, so a fluctuating failure count does not
/// re-announce itself every poll.
#[test]
fn parse_health_does_not_flap_on_changing_counts() {
    let mut handler = GlmHandler::new();
    handler.enabled = true;

    for failed in [3usize, 7, 4, 9] {
        handler.report_failures(FailureKind::Parse, Some(partial_failure(failed)));
        assert_eq!(handler.parse.health, FailureHealth::Partial);
    }

    // Escalation to total is a real change.
    handler.report_failures(FailureKind::Parse, Some(total_failure()));
    assert_eq!(handler.parse.health, FailureHealth::Total);
}

/// A network failure must never be dressed up as a product schema change.
#[test]
fn transport_failure_is_not_reported_as_a_product_change() {
    let mut handler = GlmHandler::new();
    handler.enabled = true;
    handler.report_failures(
        FailureKind::Transport,
        Some(FetchFailures {
            in_window: 12,
            failed: 12,
            sample_error: "a.nc: HTTP error: error sending request".into(),
        }),
    );

    let texts = info_texts(&handler);
    assert!(
        texts
            .iter()
            .any(|t| t.contains("could not be downloaded") && t.contains("network down?")),
        "a transport failure should point at the network, got {texts:?}"
    );
    assert!(
        !texts.iter().any(|t| t.contains("product change?")),
        "S3 throttling must not be announced as a GLM product change, got {texts:?}"
    );
}

/// A network blip must not clear or mask a live parse problem.
#[test]
fn parse_and_transport_failures_are_tracked_independently() {
    let mut handler = GlmHandler::new();
    handler.enabled = true;
    handler.report_failures(FailureKind::Parse, Some(total_failure()));
    handler.report_failures(FailureKind::Transport, Some(partial_failure(2)));

    let texts = info_texts(&handler);
    assert!(texts.iter().any(|t| t.contains("failed to parse")));
    assert!(texts.iter().any(|t| t.contains("could not be downloaded")));

    // Transport recovers; the parse problem stays on screen.
    handler.report_failures(FailureKind::Transport, None);
    let texts = info_texts(&handler);
    assert!(texts.iter().any(|t| t.contains("failed to parse")));
    assert!(!texts.iter().any(|t| t.contains("could not be downloaded")));
}

// ---- Log-output tests -------------------------------------------------
//
// Edge-triggering is only observable in the log; without capturing it, both
// edge-trigger guards can be deleted with the suite still green.

/// Captures records into `LOG_RECORDS` for assertion.
struct CaptureLogger;

static LOG_RECORDS: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());
/// Serializes the log-observing tests, which necessarily share one global
/// logger.
static LOG_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
/// Only records from this thread are captured: other tests run in parallel
/// and log, which would make the counts below nondeterministic.
static CAPTURE_THREAD: std::sync::Mutex<Option<std::thread::ThreadId>> =
    std::sync::Mutex::new(None);

impl log::Log for CaptureLogger {
    fn enabled(&self, _: &log::Metadata<'_>) -> bool {
        true
    }
    fn log(&self, record: &log::Record<'_>) {
        let capturing = *CAPTURE_THREAD.lock().unwrap_or_else(|e| e.into_inner());
        if capturing != Some(std::thread::current().id()) {
            return;
        }
        LOG_RECORDS
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(format!("{} {}", record.level(), record.args()));
    }
    fn flush(&self) {}
}

/// Run `f` and return everything it logged on this thread.
fn captured_logs(f: impl FnOnce()) -> Vec<String> {
    static INIT: std::sync::Once = std::sync::Once::new();
    let _serial = LOG_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    INIT.call_once(|| {
        // If another logger is already installed the capture stays empty
        // and the assertions below fail loudly.
        let _ = log::set_logger(&CaptureLogger);
        log::set_max_level(log::LevelFilter::Trace);
    });

    LOG_RECORDS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clear();
    *CAPTURE_THREAD.lock().unwrap_or_else(|e| e.into_inner()) = Some(std::thread::current().id());
    f();
    *CAPTURE_THREAD.lock().unwrap_or_else(|e| e.into_inner()) = None;
    LOG_RECORDS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
}

fn count_containing(logs: &[String], needle: &str) -> usize {
    logs.iter().filter(|l| l.contains(needle)).count()
}

/// `warn_once` must actually consult the registry.
///
/// Uses a key nothing else can touch: the registry is process-global and
/// other tests parse granules in parallel. The key *shapes* the real paths
/// build are pinned separately.
#[test]
fn warn_once_reports_a_condition_exactly_once() {
    let key = format!("handler-dedup-probe:{:?}", std::thread::current().id());
    let logs = captured_logs(|| {
        for _ in 0..4 {
            crate::glm::fetch::warn_once(key.clone(), "probe condition");
        }
    });
    assert_eq!(
        count_containing(&logs, "probe condition"),
        1,
        "four identical conditions must produce one line, got {logs:?}"
    );
}

/// Evidence that both satellites' flash layers were actually looked at.
const FLASH_EVALUATED: [(GlmSatellite, GlmDataLevel); 2] = [
    (GlmSatellite::GoesEast, GlmDataLevel::Flash),
    (GlmSatellite::GoesWest, GlmDataLevel::Flash),
];

fn flash_level_gone() -> LevelFailure {
    LevelFailure {
        satellite: GlmSatellite::GoesEast,
        level: GlmDataLevel::Flash,
        sample_error: "GLM file has no 'flash_lat' variable (product schema change?)".into(),
    }
}

/// A layer that stops parsing while the granules stay healthy must show up
/// on screen. Nothing else catches it: the file parsed, so it is not a
/// parse failure, and the listing is fine, so it is not a dead feed.
#[test]
fn a_failed_level_is_surfaced_in_the_control_panel() {
    let mut handler = GlmHandler::new();
    handler.enabled = true;
    handler.report_level_failures(&FLASH_EVALUATED, vec![flash_level_gone()]);

    let texts = info_texts(&handler);
    assert!(
        texts
            .iter()
            .any(|t| t.contains("Flashes") && t.contains("GOES-19 (East)")),
        "the panel must name the missing layer, got {texts:?}"
    );
}

#[test]
fn a_recovered_level_clears_the_control_panel_notice() {
    let mut handler = GlmHandler::new();
    handler.enabled = true;
    handler.report_level_failures(&FLASH_EVALUATED, vec![flash_level_gone()]);
    handler.report_level_failures(&FLASH_EVALUATED, Vec::new());

    assert!(
        !info_texts(&handler).iter().any(|t| t.contains("Flashes")),
        "the notice must clear once the layer parses again"
    );
}

/// Same edge-triggering as dead feeds: an unconditional warning emits ~180
/// identical lines an hour and buries itself.
#[test]
fn a_failed_level_warns_once_then_recovers_once() {
    let logs = captured_logs(|| {
        let mut handler = GlmHandler::new();
        for _ in 0..5 {
            handler.report_level_failures(&FLASH_EVALUATED, vec![flash_level_gone()]);
        }
        handler.report_level_failures(&FLASH_EVALUATED, Vec::new());
    });

    assert_eq!(
        count_containing(&logs, "stopped parsing"),
        1,
        "five identical polls must warn once, got {logs:?}"
    );
    assert_eq!(count_containing(&logs, "parsing again"), 1, "{logs:?}");
}

/// A notice about a layer the user switched off is stale, but the verdict
/// is still kept — deselecting must not read as recovery. Nothing else
/// would take the notice down: `clear_cache()` on a level toggle means a
/// deselected layer is never re-evaluated.
#[test]
fn a_failed_level_the_user_switched_off_is_not_shown_but_is_remembered() {
    let mut handler = GlmHandler::new();
    handler.enabled = true;
    handler.report_level_failures(&FLASH_EVALUATED, vec![flash_level_gone()]);
    assert!(info_texts(&handler).iter().any(|t| t.contains("Flashes")));

    handler.show_flashes = false;
    assert!(
        !info_texts(&handler).iter().any(|t| t.contains("Flashes")),
        "a deselected layer must not keep warning"
    );
    assert_eq!(
        handler.level_failures.len(),
        1,
        "...but the verdict is kept, so re-selecting does not re-warn"
    );

    handler.show_flashes = true;
    assert!(
        info_texts(&handler).iter().any(|t| t.contains("Flashes")),
        "and it returns when the layer is selected again"
    );
}

/// A level failure on a deselected satellite is stale and not shown.
#[test]
fn a_failed_level_on_a_deselected_satellite_is_not_shown() {
    let mut handler = GlmHandler::new();
    handler.enabled = true;
    handler.satellite = SatelliteSelection::West;
    handler.report_level_failures(&FLASH_EVALUATED, vec![flash_level_gone()]);

    assert!(
        !info_texts(&handler).iter().any(|t| t.contains("Flashes")),
        "an East-only failure must not show while West is selected"
    );
}

/// A poll that downloads no new granules evaluates nothing, so it must not
/// read as a recovery. Needs no user action to hit: with a 20 s poll
/// interval against a ~20 s granule cadence, polls that find nothing new
/// are routine.
#[test]
fn a_poll_with_no_new_granules_is_not_a_recovery() {
    let logs = captured_logs(|| {
        let mut handler = GlmHandler::new();
        handler.enabled = true;
        handler.report_level_failures(&FLASH_EVALUATED, vec![flash_level_gone()]);
        // Nothing new downloaded: no evidence about any level.
        handler.report_level_failures(&[], Vec::new());

        assert_eq!(
            handler.level_failures.len(),
            1,
            "the verdict must stand when nothing was looked at"
        );
        assert!(
            info_texts(&handler).iter().any(|t| t.contains("Flashes")),
            "the panel notice must not blink off"
        );
    });
    assert_eq!(
        count_containing(&logs, "parsing again"),
        0,
        "a poll that looked at nothing cannot report a recovery: {logs:?}"
    );
}

/// ...and re-selecting must not re-fire the warning, because the verdict was
/// carried rather than cleared.
#[test]
fn carrying_a_verdict_forward_does_not_re_warn() {
    let logs = captured_logs(|| {
        let mut handler = GlmHandler::new();
        handler.report_level_failures(&FLASH_EVALUATED, vec![flash_level_gone()]);
        handler.report_level_failures(&[], Vec::new());
        handler.report_level_failures(&FLASH_EVALUATED, vec![flash_level_gone()]);
    });
    assert_eq!(
        count_containing(&logs, "stopped parsing"),
        1,
        "edge-triggering must survive a poll with no evidence: {logs:?}"
    );
}

/// Deselecting the satellite whose layer is broken must not read as that
/// layer healing.
#[test]
fn deselecting_a_satellite_is_not_a_level_recovery() {
    let west_only: [(GlmSatellite, GlmDataLevel); 1] =
        [(GlmSatellite::GoesWest, GlmDataLevel::Flash)];
    let east_failure = LevelFailure {
        satellite: GlmSatellite::GoesEast,
        level: GlmDataLevel::Flash,
        sample_error: "flash_lat gone".into(),
    };

    let logs = captured_logs(|| {
        let mut handler = GlmHandler::new();
        handler.report_level_failures(&FLASH_EVALUATED, vec![east_failure.clone()]);
        // Dropdown switches to West-only: East is never queried again.
        handler.report_level_failures(&west_only, Vec::new());
        assert_eq!(handler.level_failures, vec![east_failure]);
    });
    assert_eq!(
        count_containing(&logs, "parsing again"),
        0,
        "a satellite we stopped asking about cannot have recovered: {logs:?}"
    );
}

/// Deselecting the *level* is the same claim on the other axis.
#[test]
fn deselecting_a_level_is_not_a_recovery() {
    let groups_only: [(GlmSatellite, GlmDataLevel); 1] =
        [(GlmSatellite::GoesEast, GlmDataLevel::Group)];
    let logs = captured_logs(|| {
        let mut handler = GlmHandler::new();
        handler.report_level_failures(&FLASH_EVALUATED, vec![flash_level_gone()]);
        // User unticks "Flashes": nothing asks about that layer any more.
        handler.report_level_failures(&groups_only, Vec::new());
        assert_eq!(handler.level_failures.len(), 1);
    });
    assert_eq!(count_containing(&logs, "parsing again"), 0, "{logs:?}");
}

/// Two layers can break on the same satellite, so every predicate must
/// compare the level as well as the bird. `known` on the satellite alone
/// swallows the second layer's warning; `still_failing` on the satellite
/// alone silently deletes a carried Flash verdict when Group fails.
#[test]
fn two_levels_failing_on_one_satellite_are_tracked_separately() {
    let east_flash = flash_level_gone();
    let east_group = LevelFailure {
        satellite: GlmSatellite::GoesEast,
        level: GlmDataLevel::Group,
        sample_error: "group_lat gone".into(),
    };
    let east_both: [(GlmSatellite, GlmDataLevel); 2] = [
        (GlmSatellite::GoesEast, GlmDataLevel::Flash),
        (GlmSatellite::GoesEast, GlmDataLevel::Group),
    ];

    let logs = captured_logs(|| {
        let mut handler = GlmHandler::new();
        handler.enabled = true;

        handler.report_level_failures(&east_both, vec![east_flash.clone()]);
        // Group breaks too, while Flash is still broken.
        handler.report_level_failures(&east_both, vec![east_flash.clone(), east_group.clone()]);

        assert_eq!(
            handler.level_failures.len(),
            2,
            "both layers must be tracked"
        );
        let texts = info_texts(&handler);
        assert!(texts.iter().any(|t| t.contains("Flashes")), "{texts:?}");
        assert!(texts.iter().any(|t| t.contains("Groups")), "{texts:?}");
    });

    // Each layer earns its own warning.
    assert_eq!(count_containing(&logs, "the Flashes layer"), 1, "{logs:?}");
    assert_eq!(count_containing(&logs, "the Groups layer"), 1, "{logs:?}");
}

/// The mirror of the above: the *same* layer on *both* birds. `known` on
/// the level alone masks the second satellite's warning; `still_failing` on
/// the level alone deletes the first satellite's carried verdict.
#[test]
fn one_level_failing_on_both_satellites_is_tracked_separately() {
    let east = flash_level_gone();
    let west = LevelFailure {
        satellite: GlmSatellite::GoesWest,
        level: GlmDataLevel::Flash,
        sample_error: "flash_lat gone on west".into(),
    };
    let west_only: [(GlmSatellite, GlmDataLevel); 1] =
        [(GlmSatellite::GoesWest, GlmDataLevel::Flash)];

    let logs = captured_logs(|| {
        let mut handler = GlmHandler::new();
        handler.enabled = true;

        handler.report_level_failures(&FLASH_EVALUATED, vec![east.clone()]);
        handler.report_level_failures(&FLASH_EVALUATED, vec![east.clone(), west.clone()]);
        assert_eq!(
            handler.level_failures.len(),
            2,
            "both birds must be tracked"
        );

        // Only West is evaluated and only West fails, so East's verdict
        // must be carried rather than dropped.
        handler.report_level_failures(&west_only, vec![west.clone()]);
        assert!(
            handler.level_failures.contains(&east),
            "an unexamined East verdict must survive a West failure, got {:?}",
            handler.level_failures
        );
        assert_eq!(handler.level_failures.len(), 2);
    });

    assert_eq!(
        count_containing(&logs, "from GOES-19 (East)"),
        1,
        "{logs:?}"
    );
    assert_eq!(
        count_containing(&logs, "from GOES-18 (West)"),
        1,
        "{logs:?}"
    );
    assert_eq!(count_containing(&logs, "parsing again"), 0, "{logs:?}");
}

/// The `still_failing` half: `previous` is East/Flash with no evidence,
/// `current` is East/Group, and comparing only the satellite drops
/// East/Flash on the floor.
#[test]
fn an_unrelated_level_failing_does_not_delete_a_carried_verdict() {
    let east_flash = flash_level_gone();
    let east_group = LevelFailure {
        satellite: GlmSatellite::GoesEast,
        level: GlmDataLevel::Group,
        sample_error: "group_lat gone".into(),
    };
    let group_only: [(GlmSatellite, GlmDataLevel); 1] =
        [(GlmSatellite::GoesEast, GlmDataLevel::Group)];

    let logs = captured_logs(|| {
        let mut handler = GlmHandler::new();
        handler.report_level_failures(&FLASH_EVALUATED, vec![east_flash.clone()]);
        // Only Group was evaluated and only Group failed, so Flash's
        // verdict must be carried.
        handler.report_level_failures(&group_only, vec![east_group]);

        assert!(
            handler.level_failures.contains(&east_flash),
            "an unexamined Flash verdict must survive a Group failure, got {:?}",
            handler.level_failures
        );
        assert_eq!(handler.level_failures.len(), 2);
    });
    assert_eq!(
        count_containing(&logs, "parsing again"),
        0,
        "nothing recovered here: {logs:?}"
    );
}

/// The guard must not swallow a *genuine* recovery: evidence present and no
/// failure reported.
#[test]
fn evidence_without_failure_is_a_real_recovery() {
    let logs = captured_logs(|| {
        let mut handler = GlmHandler::new();
        handler.report_level_failures(&FLASH_EVALUATED, vec![flash_level_gone()]);
        handler.report_level_failures(&FLASH_EVALUATED, Vec::new());
        assert!(
            handler.level_failures.is_empty(),
            "a looked-at healthy layer must clear"
        );
    });
    assert_eq!(count_containing(&logs, "parsing again"), 1, "{logs:?}");
}

/// A level failure leaves the *file* count clean — the file did parse.
#[test]
fn a_level_failure_is_not_counted_as_a_failed_file() {
    let mut handler = GlmHandler::new();
    handler.enabled = true;
    handler.report_level_failures(&FLASH_EVALUATED, vec![flash_level_gone()]);

    let texts = info_texts(&handler);
    assert!(
        !texts.iter().any(|t| t.contains("files")),
        "a level failure must not claim any file failed, got {texts:?}"
    );
}

/// A dead feed warns once, not once per poll (~180 lines an hour at a 20 s
/// interval).
#[test]
fn dead_feed_warns_once_across_many_polls() {
    let logs = captured_logs(|| {
        let mut handler = GlmHandler::new();
        handler.enabled = true;
        for _ in 0..10 {
            handler.report_feed_changes(&BOTH, vec![dead_east()]);
        }
    });

    assert_eq!(
        count_containing(&logs, "feed is dead"),
        1,
        "expected exactly one dead-feed warning across 10 polls, got: {logs:?}"
    );
    assert!(
        logs.iter()
            .any(|l| l.starts_with("WARN") && l.contains("noaa-goes16")),
        "the warning should name the bucket, got: {logs:?}"
    );
}

#[test]
fn feed_recovery_logs_once_and_re_arms() {
    let logs = captured_logs(|| {
        let mut handler = GlmHandler::new();
        handler.enabled = true;
        handler.report_feed_changes(&BOTH, vec![dead_east()]);
        for _ in 0..5 {
            handler.report_feed_changes(&BOTH, Vec::new());
        }
        // Going dark again is a fresh edge and must warn again.
        handler.report_feed_changes(&BOTH, vec![dead_east()]);
    });

    assert_eq!(count_containing(&logs, "feed recovered"), 1, "{logs:?}");
    assert_eq!(count_containing(&logs, "feed is dead"), 2, "{logs:?}");
}

/// Deselecting a dead satellite must produce no log output at all — not a
/// recovery, not a repeat warning.
#[test]
fn deselecting_a_dead_satellite_logs_nothing() {
    let logs = captured_logs(|| {
        let mut handler = GlmHandler::new();
        handler.enabled = true;
        handler.report_feed_changes(&BOTH, vec![dead_east()]);

        LOG_RECORDS
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        // Both -> West -> Both, with East still dark the whole time.
        handler.report_feed_changes(&WEST_ONLY, Vec::new());
        handler.report_feed_changes(&BOTH, vec![dead_east()]);
    });

    assert!(
        logs.is_empty(),
        "selection changes alone must not generate feed chatter, got: {logs:?}"
    );
}

/// Without the category guard these ten polls emit ten warnings.
#[test]
fn fluctuating_failure_counts_warn_once() {
    let logs = captured_logs(|| {
        let mut handler = GlmHandler::new();
        handler.enabled = true;
        for failed in [1usize, 5, 2, 7, 3, 6, 4, 8, 2, 9] {
            handler.report_failures(FailureKind::Parse, Some(partial_failure(failed)));
        }
    });

    assert_eq!(
        count_containing(&logs, "files in the window failed to parse"),
        1,
        "expected one warning across ten fluctuating polls, got: {logs:?}"
    );
}

#[test]
fn escalation_and_recovery_each_log_once() {
    let logs = captured_logs(|| {
        let mut handler = GlmHandler::new();
        handler.enabled = true;
        handler.report_failures(FailureKind::Parse, Some(partial_failure(3)));
        handler.report_failures(FailureKind::Parse, Some(total_failure()));
        handler.report_failures(FailureKind::Parse, Some(total_failure()));
        handler.report_failures(FailureKind::Parse, None);
        handler.report_failures(FailureKind::Parse, None);
    });

    assert_eq!(count_containing(&logs, "3/12"), 1, "{logs:?}");
    assert_eq!(count_containing(&logs, "all 12 files"), 1, "{logs:?}");
    assert_eq!(count_containing(&logs, "recovered"), 1, "{logs:?}");
}

// ---- Seam tests -------------------------------------------------------
//
// Everything above drives the private reporting methods directly. These go
// through apply_fetch_result, the only path production takes.

fn outcome(
    queried: Vec<GlmSatellite>,
    dead_feeds: Vec<DeadFeed>,
    parse_failures: Option<FetchFailures>,
    transport_failures: Option<FetchFailures>,
) -> FetchPayload {
    Box::new(GlmFetchResult(Ok(crate::glm::GlmFetchOutcome {
        flashes: Vec::new(),
        dead_feeds,
        queried,
        parse_failures,
        transport_failures,
        level_failures: Vec::new(),
        evaluated_levels: Vec::new(),
        listing_failures: Vec::new(),
        window_gaps: Vec::new(),
        record_drops: crate::glm::RecordDrops::default(),
    })))
}

/// The same seam, carrying the level-failure fields — which `outcome`
/// hardcodes to empty, leaving the only production call of the
/// level-failure feature deletable with the suite green.
fn level_outcome(
    level_failures: Vec<LevelFailure>,
    evaluated_levels: Vec<(GlmSatellite, GlmDataLevel)>,
) -> FetchPayload {
    Box::new(GlmFetchResult(Ok(crate::glm::GlmFetchOutcome {
        flashes: Vec::new(),
        dead_feeds: Vec::new(),
        queried: vec![GlmSatellite::GoesEast, GlmSatellite::GoesWest],
        parse_failures: None,
        transport_failures: None,
        level_failures,
        evaluated_levels,
        listing_failures: Vec::new(),
        window_gaps: Vec::new(),
        record_drops: crate::glm::RecordDrops::default(),
    })))
}

/// The Ok arm must forward *both* level fields, separately. Three ways to
/// sever it: `&[]` for the evidence (a failure can then never clear, so one
/// transient pins "⚠ Flashes unavailable" for the process lifetime),
/// `Vec::new()` for the failures (the notice never appears), and deleting
/// the call outright.
#[test]
fn apply_fetch_result_forwards_both_level_fields() {
    let mut handler = GlmHandler::new();
    handler.enabled = true;

    // 1. A failure arrives through the real seam and reaches the panel.
    handler.apply_fetch_result(level_outcome(
        vec![flash_level_gone()],
        FLASH_EVALUATED.to_vec(),
    ));
    assert_eq!(handler.level_failures, vec![flash_level_gone()]);
    assert!(
        info_texts(&handler).iter().any(|t| t.contains("Flashes")),
        "the failure must survive the seam to the panel"
    );

    // 2. A poll with no evidence must not clear it. If the seam drops
    //    `evaluated_levels`, this still passes — so 3 is the real guard.
    handler.apply_fetch_result(level_outcome(Vec::new(), Vec::new()));
    assert_eq!(
        handler.level_failures,
        vec![flash_level_gone()],
        "a poll that evaluated nothing must not clear the verdict"
    );

    // 3. Evidence with no failure clears it. `&[]` makes `looked`
    //    permanently false and the notice unclearable.
    handler.apply_fetch_result(level_outcome(Vec::new(), FLASH_EVALUATED.to_vec()));
    assert!(
        handler.level_failures.is_empty(),
        "an evaluated, healthy layer must clear through the seam"
    );
    assert!(
        !info_texts(&handler).iter().any(|t| t.contains("Flashes")),
        "and the panel notice must go with it"
    );
}

/// The Ok arm must actually forward the queried set and both failure
/// reports. Passing `&[]` or `None` here silently severs the fixes above.
#[test]
fn apply_fetch_result_forwards_queried_set_and_failures() {
    let mut handler = GlmHandler::new();
    handler.enabled = true;

    // East goes dark through the real seam.
    handler.apply_fetch_result(outcome(
        vec![GlmSatellite::GoesEast, GlmSatellite::GoesWest],
        vec![dead_east()],
        None,
        None,
    ));
    assert_eq!(handler.dead_feeds, vec![dead_east()]);

    // Both failure kinds arrive through the seam and reach the panel.
    handler.apply_fetch_result(outcome(
        vec![GlmSatellite::GoesEast, GlmSatellite::GoesWest],
        vec![dead_east()],
        Some(total_failure()),
        Some(partial_failure(2)),
    ));
    let texts = info_texts(&handler);
    assert!(
        texts.iter().any(|t| t.contains("failed to parse")),
        "parse failures must survive the seam, got {texts:?}"
    );
    assert!(
        texts.iter().any(|t| t.contains("could not be downloaded")),
        "transport failures must survive the seam, got {texts:?}"
    );

    // East recovers, which is only correct because `queried` says we asked.
    handler.apply_fetch_result(outcome(
        vec![GlmSatellite::GoesEast, GlmSatellite::GoesWest],
        Vec::new(),
        None,
        None,
    ));
    assert!(
        handler.dead_feeds.is_empty(),
        "a queried satellite that stops being dead must clear through the seam"
    );
}

// ── The half-delivered round ──────────────────────────────────────────
//
// GLM's round is one S3 listing per selected satellite, and a dead one does
// not fail the round: the survivor's flashes are real and cover most of
// CONUS. That is the right call and it was silent. The round returned Ok,
// `set_data` stamped a fresh clock and cleared the ladder, and GOES-East's
// contribution drained out of the cache window over the next half hour with
// nothing anywhere saying so.

/// A round where one satellite's listing never answered — the shape
/// `outcome` cannot express, since it hardcodes `listing_failures` empty.
fn half_listed_round() -> FetchPayload {
    Box::new(GlmFetchResult(Ok(crate::glm::GlmFetchOutcome {
        flashes: vec![crate::glm::GlmFlash {
            lat: 35.0,
            lon: -97.0,
            energy: None,
            area: None,
            time: chrono::NaiveDate::from_ymd_opt(2026, 7, 24)
                .unwrap()
                .and_hms_opt(12, 0, 0)
                .unwrap(),
            satellite: GlmSatellite::GoesWest,
            level: GlmDataLevel::Flash,
        }],
        dead_feeds: Vec::new(),
        queried: vec![GlmSatellite::GoesWest],
        parse_failures: None,
        transport_failures: None,
        level_failures: Vec::new(),
        evaluated_levels: Vec::new(),
        listing_failures: vec![(
            GlmSatellite::GoesEast,
            crate::fetch_policy::FetchError::transient("GLM listing HTTP 503"),
        )],
        window_gaps: Vec::new(),
        record_drops: crate::glm::RecordDrops::default(),
    })))
}

/// **A dead satellite reaches the row and the options, through the real
/// ingest path.**
///
/// This is what `listing_failures` is carried for: without the verdict
/// reaching the UI, the field is bookkeeping nobody reads. Driven through
/// `OverlayRegistry::apply_fetch_result` rather than the handler alone,
/// because the rendering is the registry's and a handler-level assertion
/// would pass with the mark never drawn.
#[test]
fn a_satellite_whose_listing_failed_marks_the_layer_and_names_it() {
    use crate::render::controls::PaneControlContext;
    use crate::render::overlay_state::{OverlayFetchResult, OverlayRegistry};

    let ctx = PaneControlContext {
        pane_idx: 0,
        pane_state: None,
    };
    let kind = known::LIGHTNING;
    let mut registry = OverlayRegistry::default();
    registry.set_enabled(&kind, true);

    registry.apply_fetch_result(OverlayFetchResult {
        kind: kind.clone(),
        data: half_listed_round(),
    });

    let line = registry
        .status_line(&kind)
        .expect("an enabled lightning layer states its own line");
    assert!(
        line.starts_with("! incomplete"),
        "half the sky stopped arriving and the row says nothing: {line}",
    );
    assert!(
        line.contains("1 flashes"),
        "the layer's own line must survive the mark: {line}",
    );

    let note = registry
        .controls(&kind, &ctx)
        .into_iter()
        .find_map(|item| match item {
            ControlItem::InfoText { text } if text.starts_with("Incomplete") => Some(text),
            _ => None,
        })
        .expect("the options must say what the row is marking");
    assert!(
        note.contains("missing 1 of 2 satellite feeds"),
        "the note must count the feeds, not the flashes: {note}",
    );
    assert!(
        note.contains(GlmSatellite::GoesEast.display_name()) && note.contains("HTTP 503"),
        "the note must name which satellite and why: {note}",
    );

    // The survivor's flashes are real data on a fresh clock. A half round is
    // not a failed one and must not be filed as one.
    assert_eq!(
        registry.fetch_health(&kind),
        Some(&crate::fetch_policy::FetchHealth::Ok),
    );
    assert!(
        !line.contains("not updating"),
        "a half round is not stale: {line}"
    );

    // Both listings answer next poll: the mark clears itself.
    registry.apply_fetch_result(OverlayFetchResult {
        kind: kind.clone(),
        data: outcome(
            vec![GlmSatellite::GoesEast, GlmSatellite::GoesWest],
            Vec::new(),
            None,
            None,
        ),
    });
    assert!(
        !registry
            .status_line(&kind)
            .is_some_and(|l| l.contains("incomplete")),
        "the mark outlived the round it was about",
    );
}

// ── The other three ways this round under-delivers ────────────────────
//
// A listing is one of four. A dead feed lists `200` with no objects; listed
// granules can refuse to download or to parse; and one hierarchy level can
// stop parsing inside granules that otherwise did. All four end a poll with
// flashes off the map, and the first draft of the coverage report counted
// only the listing — so the other three stayed exactly as silent as the
// listing had been, under a mechanism built to end that silence. The
// transport case had a log line that said so in as many words: "the map is
// blank despite a healthy S3 listing".

/// A round with every listing healthy, parameterised by what went wrong
/// after it.
fn round(
    dead_feeds: Vec<DeadFeed>,
    transport_failures: Option<FetchFailures>,
    parse_failures: Option<FetchFailures>,
    level_failures: Vec<LevelFailure>,
) -> FetchPayload {
    Box::new(GlmFetchResult(Ok(crate::glm::GlmFetchOutcome {
        flashes: Vec::new(),
        dead_feeds,
        queried: vec![GlmSatellite::GoesEast, GlmSatellite::GoesWest],
        parse_failures,
        transport_failures,
        level_failures,
        evaluated_levels: Vec::new(),
        listing_failures: Vec::new(),
        window_gaps: Vec::new(),
        record_drops: crate::glm::RecordDrops::default(),
    })))
}

/// The row and the note an enabled lightning layer shows for `payload`.
fn marks(payload: FetchPayload) -> (String, Option<String>) {
    use crate::render::controls::PaneControlContext;
    use crate::render::overlay_state::{OverlayFetchResult, OverlayRegistry};

    let kind = known::LIGHTNING;
    let mut registry = OverlayRegistry::default();
    registry.set_enabled(&kind, true);
    registry.apply_fetch_result(OverlayFetchResult {
        kind: kind.clone(),
        data: payload,
    });
    let ctx = PaneControlContext {
        pane_idx: 0,
        pane_state: None,
    };
    let note = registry
        .controls(&kind, &ctx)
        .into_iter()
        .find_map(|item| match item {
            ControlItem::InfoText { text } if text.starts_with("Incomplete") => Some(text),
            _ => None,
        });
    (
        registry
            .status_line(&kind)
            .expect("an enabled lightning layer states its own line"),
        note,
    )
}

fn failures(in_window: usize, failed: usize) -> Option<FetchFailures> {
    Some(FetchFailures {
        in_window,
        failed,
        sample_error: "granule.nc: HTTP status error (503 Service Unavailable)".into(),
    })
}

/// **Every granule refused, both listings healthy: a blank map.**
///
/// Measured over a socket before the fix — two feeds queried, zero listing
/// failures, `transport_failures` 2 of 2, zero flashes, and
/// `DataCompleteness::is_complete()` returning `true`. The layer's own
/// warning for this state already named the cost; nothing carried it to the
/// user.
#[test]
fn granules_that_will_not_download_mark_the_layer() {
    let (line, note) = marks(round(Vec::new(), failures(20, 20), None, Vec::new()));
    assert!(
        line.starts_with("! incomplete"),
        "the map is blank and the row says nothing: {line}",
    );
    let note = note.expect("the options must say what the row is marking");
    assert!(
        note.contains("missing 2 of 2 satellite feeds"),
        "no granule of either feed reached the map: {note}",
    );
    assert!(
        note.contains("0 of 20 granules resolved"),
        "the note must count the granules it could not obtain: {note}",
    );
    assert!(
        note.contains("503"),
        "the note must keep the origin's own words: {note}",
    );
}

/// Some granules refused: the feeds are drawing part of their window, which
/// is a different sentence from drawing none of it.
#[test]
fn some_granules_refused_leaves_the_feeds_part_drawn() {
    let (line, note) = marks(round(Vec::new(), failures(20, 7), None, Vec::new()));
    assert!(line.starts_with("! incomplete"), "{line}");
    let note = note.expect("note");
    assert!(
        note.contains("2 of 2 satellite feeds drawing only part of their area"),
        "part of a window is not none of it: {note}",
    );
    assert!(note.contains("13 of 20 granules resolved"), "{note}");
}

/// A listing that answers `200` with no objects at all. GLM publishes a
/// granule every twenty seconds, so this is the feed being gone — the
/// handler's own log line says "not a quiet sky". It is in `queried`, which
/// is exactly why counting listing failures alone read it as delivering.
#[test]
fn a_dead_feed_is_missing_even_though_its_listing_answered() {
    let dead = DeadFeed {
        satellite: GlmSatellite::GoesEast,
        bucket: "noaa-goes16".into(),
        prefixes: vec!["GLM-L2-LCFA/2026/225/06".into()],
    };
    let (line, note) = marks(round(vec![dead], None, None, Vec::new()));
    assert!(
        line.starts_with("! incomplete"),
        "a feed returning no objects at all is not a whole round: {line}",
    );
    let note = note.expect("note");
    assert!(note.contains("missing 1 of 2 satellite feeds"), "{note}",);
    assert!(
        note.contains(GlmSatellite::GoesEast.display_name()) && note.contains("no objects"),
        "the note must name which feed and why: {note}",
    );
}

/// Granules download and parse, one hierarchy level inside them does not:
/// that layer alone is empty on the map and the others are unaffected, which
/// is part-drawn rather than missing.
#[test]
fn a_level_that_stopped_parsing_marks_the_feeds_part_drawn() {
    let level = LevelFailure {
        satellite: GlmSatellite::GoesEast,
        level: GlmDataLevel::Group,
        sample_error: "GLM file has no 'group_area' variable".into(),
    };
    let (line, note) = marks(round(Vec::new(), None, None, vec![level]));
    assert!(
        line.starts_with("! incomplete"),
        "a layer that went empty is not a whole round: {line}",
    );
    let note = note.expect("note");
    assert!(
        note.contains("drawing only part of their area"),
        "the other levels still drew: {note}",
    );
    assert!(
        note.contains("stopped parsing"),
        "the note must say which level: {note}",
    );
}

/// The control: nothing failed anywhere, so nothing is marked. Without this,
/// every assertion above would pass on a `round_coverage` that simply always
/// reported incomplete.
#[test]
fn a_whole_round_carries_no_mark() {
    let (line, note) = marks(round(Vec::new(), None, None, Vec::new()));
    assert!(
        !line.starts_with("!"),
        "nothing failed and the layer claims a fault: {line}",
    );
    assert_eq!(note, None, "nothing failed and the options say otherwise");
}

/// A round whose listings were healthy but placed no granule in the window,
/// for `gapped` satellites out of the two queried.
fn gapped_round(gapped: Vec<GlmSatellite>) -> FetchPayload {
    Box::new(GlmFetchResult(Ok(crate::glm::GlmFetchOutcome {
        flashes: Vec::new(),
        dead_feeds: Vec::new(),
        queried: vec![GlmSatellite::GoesEast, GlmSatellite::GoesWest],
        parse_failures: None,
        transport_failures: None,
        level_failures: Vec::new(),
        evaluated_levels: Vec::new(),
        listing_failures: Vec::new(),
        window_gaps: gapped
            .into_iter()
            .map(|satellite| crate::glm::WindowGap {
                satellite,
                objects_seen: 180,
            })
            .collect(),
        record_drops: crate::glm::RecordDrops::default(),
    })))
}

/// A round that parsed granules and threw records away inside them.
fn dropping_round(considered: usize, fill_values: usize, off_globe: usize) -> FetchPayload {
    Box::new(GlmFetchResult(Ok(crate::glm::GlmFetchOutcome {
        flashes: Vec::new(),
        dead_feeds: Vec::new(),
        queried: vec![GlmSatellite::GoesEast, GlmSatellite::GoesWest],
        parse_failures: None,
        transport_failures: None,
        level_failures: Vec::new(),
        evaluated_levels: Vec::new(),
        listing_failures: Vec::new(),
        window_gaps: Vec::new(),
        record_drops: crate::glm::RecordDrops {
            considered,
            fill_values,
            off_globe,
        },
    })))
}

/// **A window gap reaches the row and the options.**
///
/// The listing answered `200` with objects and none of them covers the window,
/// so this feed delivered nothing while every existing check stayed quiet: the
/// dead-feed check needs zero objects, the granule checks need a granule to
/// have failed, and there was none to fail. The round returned `Ok` on a fresh
/// clock and `is_complete()` was `true`.
#[test]
fn a_window_gap_marks_the_layer_and_says_the_listing_was_healthy() {
    let (line, note) = marks(gapped_round(vec![GlmSatellite::GoesEast]));
    assert!(
        line.starts_with("! incomplete"),
        "a feed delivered no granule and the row says nothing: {line}",
    );
    let note = note.expect("the options must say what the row is marking");
    assert!(
        note.contains("missing 1 of 2 satellite feeds"),
        "one feed contributed nothing, the other is fine: {note}",
    );
    assert!(
        note.contains("listing healthy"),
        "the operator's next move depends on knowing the bucket is not the \
         problem, which is the whole reason this is not a dead feed: {note}",
    );
    assert!(
        note.contains("180"),
        "the object count is the evidence for that claim: {note}",
    );
}

/// A window gap is not charged again for granules it never listed. It put
/// nothing in `in_window`, so there is nothing of its own to have failed, and
/// counting it among the live feeds would report three faults on two feeds.
#[test]
fn a_window_gap_is_not_charged_again_for_the_other_feeds_granules() {
    let payload = Box::new(GlmFetchResult(Ok(crate::glm::GlmFetchOutcome {
        flashes: Vec::new(),
        dead_feeds: Vec::new(),
        queried: vec![GlmSatellite::GoesEast, GlmSatellite::GoesWest],
        parse_failures: None,
        transport_failures: failures(20, 20),
        level_failures: Vec::new(),
        evaluated_levels: Vec::new(),
        listing_failures: Vec::new(),
        window_gaps: vec![crate::glm::WindowGap {
            satellite: GlmSatellite::GoesEast,
            objects_seen: 180,
        }],
        record_drops: crate::glm::RecordDrops::default(),
    }))) as FetchPayload;
    let (_, note) = marks(payload);
    let note = note.expect("note");
    assert!(
        note.contains("missing 2 of 2 satellite feeds"),
        "one feed listed no granule and one lost every granule it listed - two, \
         not three: {note}",
    );
}

/// **Dropped records reach the row and the options, with their own
/// denominator.**
///
/// The count existed and went to a log line. Fails if it stops reaching the
/// panel, and - because the denominator is asserted inside the same string -
/// if the record count is ever pooled into the granule figures standing beside
/// it, where `1200` records would read as `1200` granules.
#[test]
fn dropped_records_mark_the_layer_and_carry_their_own_denominator() {
    let (line, note) = marks(dropping_round(1200, 4, 11));
    assert!(
        line.starts_with("! incomplete"),
        "records were thrown away and the row says nothing: {line}",
    );
    let note = note.expect("the options must say what the row is marking");
    assert!(
        note.contains("2 of 2 satellite feeds drawing only part of their area"),
        "a hole in a layer that is otherwise drawing is partial, not missing: {note}",
    );
    assert!(
        note.contains("4 of 1200 records dropped for fill values"),
        "the record denominator must travel with the record count: {note}",
    );
    assert!(
        note.contains("11 of 1200 records dropped for coordinates off the globe"),
        "the two causes indict different things and are never merged: {note}",
    );
}

/// **The quiet-sky control, at the coverage layer.** A round that listed its
/// granules, parsed them and kept every record carries no mark, even though it
/// drew no flashes at all - which is exactly what a calm night looks like.
///
/// Without this every assertion above would pass on a `round_coverage` that
/// reported incomplete whenever the layer was empty, and that is the false
/// alarm that would make the mark worthless: GLM is genuinely quiet most
/// nights, and a permanent "incomplete" is one people learn to scroll past.
#[test]
fn a_quiet_sky_with_no_flashes_at_all_carries_no_mark() {
    let (line, note) = marks(dropping_round(1200, 0, 0));
    assert!(
        !line.starts_with("!"),
        "every granule listed, downloaded, parsed and kept every record, and \
         nothing flashed - that is a correct empty result: {line}",
    );
    assert_eq!(
        note, None,
        "an empty map is not a fault unless something went missing on the way",
    );
}

/// A dead feed and a granule failure at once must not charge the dead feed
/// twice: it listed no objects, so none of the granules in the window were
/// ever its to deliver.
#[test]
fn a_dead_feed_is_not_charged_again_for_the_other_feeds_granules() {
    let dead = DeadFeed {
        satellite: GlmSatellite::GoesEast,
        bucket: "noaa-goes16".into(),
        prefixes: vec!["GLM-L2-LCFA/2026/225/06".into()],
    };
    let (_, note) = marks(round(vec![dead], failures(20, 20), None, Vec::new()));
    let note = note.expect("note");
    assert!(
        note.contains("missing 2 of 2 satellite feeds"),
        "one feed is dead and one delivered no granules — two, not three: {note}",
    );
}
