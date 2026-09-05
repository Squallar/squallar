use super::*;
use crate::render::overlay_state::PaneRef;

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

fn rows(item: &GlmFlashItem) -> Vec<(String, String)> {
    let prefs = UserPreferences {
        timezone: squallar_units::TimezonePreference::Utc,
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

#[test]
fn popup_always_reports_position_even_with_both_fields_unknown() {
    let ks = grid_keys(&item(GlmDataLevel::Event, None, None));
    assert_eq!(ks, vec!["Type", "Latitude", "Longitude"], "rows: {ks:?}");
}

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
    let ctx = PaneRef::bare(0);
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

#[test]
fn dead_feed_is_surfaced_in_the_control_panel() {
    let mut handler = GlmHandler::new();
    handler.defaults.enabled = true;
    handler.report_feed_changes(&BOTH, vec![dead_east()]);

    let texts = info_texts(&handler);
    assert!(
        texts
            .iter()
            .any(|t| t.contains("noaa-goes16") && t.contains("GOES-19 (East)")),
        "control panel should name the empty bucket, got {texts:?}"
    );
}

#[test]
fn a_window_gap_is_surfaced_in_the_control_panel_in_its_own_words() {
    let mut handler = GlmHandler::new();
    handler.defaults.enabled = true;
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

#[test]
fn a_closed_window_gap_clears_its_notice_but_only_if_it_was_queried() {
    let gap = || {
        vec![WindowGap {
            satellite: GlmSatellite::GoesWest,
            objects_seen: 180,
        }]
    };
    let mut handler = GlmHandler::new();
    handler.defaults.enabled = true;
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

#[test]
fn a_poll_that_parsed_no_granule_does_not_clear_the_drop_notice() {
    let mut handler = GlmHandler::new();
    handler.defaults.enabled = true;
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
    handler.defaults.enabled = true;
    handler.report_feed_changes(&BOTH, vec![dead_east()]);
    handler.report_feed_changes(&BOTH, Vec::new());

    let texts = info_texts(&handler);
    assert!(
        !texts.iter().any(|t| t.contains("noaa-goes16")),
        "notice should clear once the feed returns, got {texts:?}"
    );
}

#[test]
fn repeated_polls_do_not_accumulate_feed_state() {
    let mut handler = GlmHandler::new();
    handler.defaults.enabled = true;

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

#[test]
fn failed_fetch_leaves_feed_verdict_untouched() {
    let mut handler = GlmHandler::new();
    handler.defaults.enabled = true;
    handler.report_feed_changes(&BOTH, vec![dead_east()]);

    handler.apply_fetch_result(
        Box::new(GlmFetchResult(Err(
            crate::fetch_policy::FetchError::transient("network down"),
        ))),
        &PaneRef::across(&[]),
    );

    assert_eq!(handler.dead_feeds, vec![dead_east()]);
}

#[test]
fn deselecting_a_dead_satellite_is_not_a_recovery() {
    let mut handler = GlmHandler::new();
    handler.defaults.enabled = true;
    handler.defaults.satellite = SatelliteSelection::Both;
    handler.report_feed_changes(&BOTH, vec![dead_east()]);

    handler.defaults.satellite = SatelliteSelection::West;
    handler.report_feed_changes(&WEST_ONLY, Vec::new());

    assert_eq!(
        handler.dead_feeds,
        vec![dead_east()],
        "an unqueried satellite's verdict must be carried forward, not cleared"
    );
    assert!(
        !info_texts(&handler)
            .iter()
            .any(|t| t.contains("noaa-goes16")),
        "a deselected satellite should not occupy the panel"
    );
}

#[test]
fn reselecting_a_still_dead_satellite_does_not_re_report_it() {
    let mut handler = GlmHandler::new();
    handler.defaults.enabled = true;
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

#[test]
fn recovery_is_still_reported_for_a_queried_satellite() {
    let mut handler = GlmHandler::new();
    handler.defaults.enabled = true;
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

#[test]
fn total_parse_failure_is_surfaced_in_the_control_panel() {
    let mut handler = GlmHandler::new();
    handler.defaults.enabled = true;
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
    handler.defaults.enabled = true;
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
    handler.defaults.enabled = true;
    handler.report_failures(FailureKind::Parse, Some(total_failure()));
    handler.report_failures(FailureKind::Parse, None);

    assert!(
        !info_texts(&handler)
            .iter()
            .any(|t| t.contains("failed to parse")),
        "notice should clear once parsing recovers"
    );
}

#[test]
fn parse_health_does_not_flap_on_changing_counts() {
    let mut handler = GlmHandler::new();
    handler.defaults.enabled = true;

    for failed in [3usize, 7, 4, 9] {
        handler.report_failures(FailureKind::Parse, Some(partial_failure(failed)));
        assert_eq!(handler.parse.health, FailureHealth::Partial);
    }

    handler.report_failures(FailureKind::Parse, Some(total_failure()));
    assert_eq!(handler.parse.health, FailureHealth::Total);
}

#[test]
fn transport_failure_is_not_reported_as_a_product_change() {
    let mut handler = GlmHandler::new();
    handler.defaults.enabled = true;
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

#[test]
fn parse_and_transport_failures_are_tracked_independently() {
    let mut handler = GlmHandler::new();
    handler.defaults.enabled = true;
    handler.report_failures(FailureKind::Parse, Some(total_failure()));
    handler.report_failures(FailureKind::Transport, Some(partial_failure(2)));

    let texts = info_texts(&handler);
    assert!(texts.iter().any(|t| t.contains("failed to parse")));
    assert!(texts.iter().any(|t| t.contains("could not be downloaded")));

    handler.report_failures(FailureKind::Transport, None);
    let texts = info_texts(&handler);
    assert!(texts.iter().any(|t| t.contains("failed to parse")));
    assert!(!texts.iter().any(|t| t.contains("could not be downloaded")));
}

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

#[test]
fn a_failed_level_is_surfaced_in_the_control_panel() {
    let mut handler = GlmHandler::new();
    handler.defaults.enabled = true;
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
    handler.defaults.enabled = true;
    handler.report_level_failures(&FLASH_EVALUATED, vec![flash_level_gone()]);
    handler.report_level_failures(&FLASH_EVALUATED, Vec::new());

    assert!(
        !info_texts(&handler).iter().any(|t| t.contains("Flashes")),
        "the notice must clear once the layer parses again"
    );
}

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

#[test]
fn a_failed_level_the_user_switched_off_is_not_shown_but_is_remembered() {
    let mut handler = GlmHandler::new();
    handler.defaults.enabled = true;
    handler.report_level_failures(&FLASH_EVALUATED, vec![flash_level_gone()]);
    assert!(info_texts(&handler).iter().any(|t| t.contains("Flashes")));

    handler.defaults.show_flashes = false;
    assert!(
        !info_texts(&handler).iter().any(|t| t.contains("Flashes")),
        "a deselected layer must not keep warning"
    );
    assert_eq!(
        handler.level_failures.len(),
        1,
        "...but the verdict is kept, so re-selecting does not re-warn"
    );

    handler.defaults.show_flashes = true;
    assert!(
        info_texts(&handler).iter().any(|t| t.contains("Flashes")),
        "and it returns when the layer is selected again"
    );
}

#[test]
fn a_failed_level_on_a_deselected_satellite_is_not_shown() {
    let mut handler = GlmHandler::new();
    handler.defaults.enabled = true;
    handler.defaults.satellite = SatelliteSelection::West;
    handler.report_level_failures(&FLASH_EVALUATED, vec![flash_level_gone()]);

    assert!(
        !info_texts(&handler).iter().any(|t| t.contains("Flashes")),
        "an East-only failure must not show while West is selected"
    );
}

#[test]
fn a_poll_with_no_new_granules_is_not_a_recovery() {
    let logs = captured_logs(|| {
        let mut handler = GlmHandler::new();
        handler.defaults.enabled = true;
        handler.report_level_failures(&FLASH_EVALUATED, vec![flash_level_gone()]);
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
        handler.report_level_failures(&west_only, Vec::new());
        assert_eq!(handler.level_failures, vec![east_failure]);
    });
    assert_eq!(
        count_containing(&logs, "parsing again"),
        0,
        "a satellite we stopped asking about cannot have recovered: {logs:?}"
    );
}

#[test]
fn deselecting_a_level_is_not_a_recovery() {
    let groups_only: [(GlmSatellite, GlmDataLevel); 1] =
        [(GlmSatellite::GoesEast, GlmDataLevel::Group)];
    let logs = captured_logs(|| {
        let mut handler = GlmHandler::new();
        handler.report_level_failures(&FLASH_EVALUATED, vec![flash_level_gone()]);
        handler.report_level_failures(&groups_only, Vec::new());
        assert_eq!(handler.level_failures.len(), 1);
    });
    assert_eq!(count_containing(&logs, "parsing again"), 0, "{logs:?}");
}

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
        handler.defaults.enabled = true;

        handler.report_level_failures(&east_both, vec![east_flash.clone()]);
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

    assert_eq!(count_containing(&logs, "the Flashes layer"), 1, "{logs:?}");
    assert_eq!(count_containing(&logs, "the Groups layer"), 1, "{logs:?}");
}

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
        handler.defaults.enabled = true;

        handler.report_level_failures(&FLASH_EVALUATED, vec![east.clone()]);
        handler.report_level_failures(&FLASH_EVALUATED, vec![east.clone(), west.clone()]);
        assert_eq!(
            handler.level_failures.len(),
            2,
            "both birds must be tracked"
        );

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

#[test]
fn a_level_failure_is_not_counted_as_a_failed_file() {
    let mut handler = GlmHandler::new();
    handler.defaults.enabled = true;
    handler.report_level_failures(&FLASH_EVALUATED, vec![flash_level_gone()]);

    let texts = info_texts(&handler);
    assert!(
        !texts.iter().any(|t| t.contains("files")),
        "a level failure must not claim any file failed, got {texts:?}"
    );
}

#[test]
fn dead_feed_warns_once_across_many_polls() {
    let logs = captured_logs(|| {
        let mut handler = GlmHandler::new();
        handler.defaults.enabled = true;
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
        handler.defaults.enabled = true;
        handler.report_feed_changes(&BOTH, vec![dead_east()]);
        for _ in 0..5 {
            handler.report_feed_changes(&BOTH, Vec::new());
        }
        handler.report_feed_changes(&BOTH, vec![dead_east()]);
    });

    assert_eq!(count_containing(&logs, "feed recovered"), 1, "{logs:?}");
    assert_eq!(count_containing(&logs, "feed is dead"), 2, "{logs:?}");
}

#[test]
fn deselecting_a_dead_satellite_logs_nothing() {
    let logs = captured_logs(|| {
        let mut handler = GlmHandler::new();
        handler.defaults.enabled = true;
        handler.report_feed_changes(&BOTH, vec![dead_east()]);

        LOG_RECORDS
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        handler.report_feed_changes(&WEST_ONLY, Vec::new());
        handler.report_feed_changes(&BOTH, vec![dead_east()]);
    });

    assert!(
        logs.is_empty(),
        "selection changes alone must not generate feed chatter, got: {logs:?}"
    );
}

#[test]
fn fluctuating_failure_counts_warn_once() {
    let logs = captured_logs(|| {
        let mut handler = GlmHandler::new();
        handler.defaults.enabled = true;
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
        handler.defaults.enabled = true;
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

#[test]
fn apply_fetch_result_forwards_both_level_fields() {
    let mut handler = GlmHandler::new();
    handler.defaults.enabled = true;

    handler.apply_fetch_result(
        level_outcome(vec![flash_level_gone()], FLASH_EVALUATED.to_vec()),
        &PaneRef::across(&[]),
    );
    assert_eq!(handler.level_failures, vec![flash_level_gone()]);
    assert!(
        info_texts(&handler).iter().any(|t| t.contains("Flashes")),
        "the failure must survive the seam to the panel"
    );

    handler.apply_fetch_result(level_outcome(Vec::new(), Vec::new()), &PaneRef::across(&[]));
    assert_eq!(
        handler.level_failures,
        vec![flash_level_gone()],
        "a poll that evaluated nothing must not clear the verdict"
    );

    handler.apply_fetch_result(
        level_outcome(Vec::new(), FLASH_EVALUATED.to_vec()),
        &PaneRef::across(&[]),
    );
    assert!(
        handler.level_failures.is_empty(),
        "an evaluated, healthy layer must clear through the seam"
    );
    assert!(
        !info_texts(&handler).iter().any(|t| t.contains("Flashes")),
        "and the panel notice must go with it"
    );
}

#[test]
fn apply_fetch_result_forwards_queried_set_and_failures() {
    let mut handler = GlmHandler::new();
    handler.defaults.enabled = true;

    handler.apply_fetch_result(
        outcome(
            vec![GlmSatellite::GoesEast, GlmSatellite::GoesWest],
            vec![dead_east()],
            None,
            None,
        ),
        &PaneRef::across(&[]),
    );
    assert_eq!(handler.dead_feeds, vec![dead_east()]);

    handler.apply_fetch_result(
        outcome(
            vec![GlmSatellite::GoesEast, GlmSatellite::GoesWest],
            vec![dead_east()],
            Some(total_failure()),
            Some(partial_failure(2)),
        ),
        &PaneRef::across(&[]),
    );
    let texts = info_texts(&handler);
    assert!(
        texts.iter().any(|t| t.contains("failed to parse")),
        "parse failures must survive the seam, got {texts:?}"
    );
    assert!(
        texts.iter().any(|t| t.contains("could not be downloaded")),
        "transport failures must survive the seam, got {texts:?}"
    );

    handler.apply_fetch_result(
        outcome(
            vec![GlmSatellite::GoesEast, GlmSatellite::GoesWest],
            Vec::new(),
            None,
            None,
        ),
        &PaneRef::across(&[]),
    );
    assert!(
        handler.dead_feeds.is_empty(),
        "a queried satellite that stops being dead must clear through the seam"
    );
}

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

#[test]
fn a_satellite_whose_listing_failed_marks_the_layer_and_names_it() {
    use crate::render::overlay_state::{OverlayFetchResult, OverlayRegistry};

    let ctx = PaneRef::bare(0);
    let kind = known::LIGHTNING;
    let mut registry = OverlayRegistry::default();
    registry.set_enabled(&kind, true, &mut PaneMut::bare(0));

    registry.apply_fetch_result(
        OverlayFetchResult {
            kind: kind.clone(),
            data: half_listed_round(),
        },
        &PaneRef::bare(0),
    );

    let line = registry
        .status_line(&kind, &PaneRef::bare(0))
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

    assert_eq!(
        registry.fetch_health(&kind),
        Some(&crate::fetch_policy::FetchHealth::Ok),
    );
    assert!(
        !line.contains("not updating"),
        "a half round is not stale: {line}"
    );

    registry.apply_fetch_result(
        OverlayFetchResult {
            kind: kind.clone(),
            data: outcome(
                vec![GlmSatellite::GoesEast, GlmSatellite::GoesWest],
                Vec::new(),
                None,
                None,
            ),
        },
        &PaneRef::bare(0),
    );
    assert!(
        !registry
            .status_line(&kind, &PaneRef::bare(0))
            .is_some_and(|l| l.contains("incomplete")),
        "the mark outlived the round it was about",
    );
}

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

fn marks(payload: FetchPayload) -> (String, Option<String>) {
    use crate::render::overlay_state::{OverlayFetchResult, OverlayRegistry};

    let kind = known::LIGHTNING;
    let mut registry = OverlayRegistry::default();
    registry.set_enabled(&kind, true, &mut PaneMut::bare(0));
    registry.apply_fetch_result(
        OverlayFetchResult {
            kind: kind.clone(),
            data: payload,
        },
        &PaneRef::bare(0),
    );
    let ctx = PaneRef::bare(0);
    let note = registry
        .controls(&kind, &ctx)
        .into_iter()
        .find_map(|item| match item {
            ControlItem::InfoText { text } if text.starts_with("Incomplete") => Some(text),
            _ => None,
        });
    (
        registry
            .status_line(&kind, &PaneRef::bare(0))
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

#[test]
fn a_whole_round_carries_no_mark() {
    let (line, note) = marks(round(Vec::new(), None, None, Vec::new()));
    assert!(
        !line.starts_with("!"),
        "nothing failed and the layer claims a fault: {line}",
    );
    assert_eq!(note, None, "nothing failed and the options say otherwise");
}

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

/// **Two panes hold different GLM selections at the same time, and neither
/// edit reaches the registry.** The toggle-only layers prove a flag can
/// diverge; this proves a whole selection can — a satellite and a time window,
/// which is what the config swap was faking by re-installing one handler's
/// fields before every read.
///
/// Non-triviality floor: the two states are asserted **equal** first, so the
/// divergence below cannot be one `create_pane_state` handed out.
#[test]
fn two_panes_hold_different_glm_selections_and_the_registry_keeps_none_of_them() {
    use squallar_source::handler::PaneMut;

    let mut handler = GlmHandler::new();
    let mut a = handler
        .create_pane_state(true)
        .expect("lightning keeps per-pane state");
    let mut b = handler
        .create_pane_state(true)
        .expect("lightning keeps per-pane state");
    assert_eq!(
        handler.serialize_pane_state(&*a),
        handler.serialize_pane_state(&*b),
        "premise: two fresh panes start identical",
    );

    handler.apply_control(
        &ControlUpdate {
            id: "time_window",
            value: ControlValue::Float(20.0),
        },
        &mut PaneMut {
            pane_idx: 0,
            state: Some(&mut *a),
            peers: &[],
        },
    );
    handler.apply_control(
        &ControlUpdate {
            id: "satellite",
            value: ControlValue::String("west".into()),
        },
        &mut PaneMut {
            pane_idx: 1,
            state: Some(&mut *b),
            peers: &[],
        },
    );

    let pane_a = PaneRef {
        state: Some(&*a),
        ..PaneRef::bare(0)
    };
    let pane_b = PaneRef {
        state: Some(&*b),
        ..PaneRef::bare(1)
    };

    // The cache token is what the render dispatch groups panes by: an equal
    // token here is one pane drawing the other pane's lightning.
    assert_ne!(
        handler.content_signature(&pane_a),
        handler.content_signature(&pane_b),
        "two panes on different satellites and windows shared one cache token",
    );

    // Read back through the trait, not through the state: this is the path the
    // draw loop and the layer stack take.
    assert_eq!(
        handler.serialize_pane_state(&*a)["time_window_secs"],
        serde_json::json!(1200.0),
        "pane 0's window",
    );
    assert_eq!(
        handler.serialize_pane_state(&*b)["time_window_secs"],
        serde_json::json!(300.0),
        "pane 1 took pane 0's window",
    );
    assert_eq!(
        handler.serialize_pane_state(&*b)["satellite"],
        serde_json::json!("west"),
        "pane 1's satellite",
    );
    assert_eq!(
        handler.serialize_pane_state(&*a)["satellite"],
        serde_json::json!("both"),
        "pane 0 took pane 1's satellite",
    );

    // `status_line` reads the window, so it is per-pane too.
    // No data needed: the line reports a count and the window, and the count
    // is honestly zero here.
    assert!(
        handler
            .status_line(&pane_a)
            .expect("enabled")
            .contains("20 min"),
        "pane 0's status line: {:?}",
        handler.status_line(&pane_a),
    );
    assert!(
        handler
            .status_line(&pane_b)
            .expect("enabled")
            .contains("5 min"),
        "pane 1's status line: {:?}",
        handler.status_line(&pane_b),
    );

    // And the registry's own copy moved for NEITHER edit — the assertion that
    // fails the moment a handler writes a per-pane value to `&mut self`.
    assert_eq!(handler.defaults.time_window_secs, 300.0);
    assert_eq!(handler.defaults.satellite, SatelliteSelection::Both);
}

/// **A flash's age is measured from the instant the picture DEPICTS, not from
/// the wall clock.** The wire field is still called `now` and its bytes are
/// unmoved — it has always meant "what these ages are measured from", which is
/// exactly `as_of`.
///
/// The two clocks are set an hour apart, so a body still reading `ctx.now`
/// fails by 3600 s rather than by rounding. A live pane sets them equal, which
/// is why this is a dark land.
#[test]
fn the_glm_job_ages_flashes_from_the_depicted_instant_not_the_wall_clock() {
    use crate::render::overlay_state::RasterizeContext;

    let mut handler = GlmHandler::new();
    handler.defaults.enabled = true;
    handler.apply_fetch_result(
        Box::new(GlmFetchResult(Ok(crate::glm::GlmFetchOutcome {
            flashes: vec![item(GlmDataLevel::Flash, Some(1e-14), None).flash],
            dead_feeds: Vec::new(),
            queried: vec![GlmSatellite::GoesEast],
            parse_failures: None,
            transport_failures: None,
            level_failures: Vec::new(),
            evaluated_levels: vec![(GlmSatellite::GoesEast, GlmDataLevel::Flash)],
            listing_failures: Vec::new(),
            window_gaps: Vec::new(),
            record_drops: Default::default(),
        }))),
        &PaneRef::across(&[]),
    );

    let wall = chrono::NaiveDate::from_ymd_opt(2026, 7, 24)
        .unwrap()
        .and_hms_opt(12, 0, 0)
        .unwrap();
    let depicted = wall - chrono::Duration::hours(1);
    let job = handler
        .prepare_job(
            &RasterizeContext {
                is_dark: false,
                zoom: 7.0,
                device_scale: 1.0,
                now: wall,
                as_of: depicted,
                frame: None,
            },
            &PaneRef::bare(0),
        )
        .expect("a flash is resident, so the layer describes a job");
    assert_eq!(
        job.downcast_ref::<crate::render::rasterize::GlmStrikesInput>()
            .expect("the GLM row")
            .now,
        depicted,
        "the flash-age reference is the wall clock, so a scrubbed pane would \
         fade every flash by the distance between the two",
    );
}

/// **The layer this whole contract was written for, answering for itself.**
///
/// Thirteen hourly stops — a twelve-hour satellite loop's frames — against
/// the default five-minute window. The ask is **65 minutes**: thirteen
/// windows, one behind each stop. The extent they are scattered across is
/// twelve hours and five minutes, and it is that extent the poll was
/// reconstructing when GLM lit two frames of thirteen.
///
/// The figure is `DepictedWindow`'s own (`4dc162f7`), computed here by the
/// layer instead of by hand inside the fetch.
#[test]
fn a_lightning_loop_asks_for_thirteen_windows_not_twelve_hours() {
    let handler = GlmHandler::new();
    let pane = PaneRef::bare(0);
    assert_eq!(
        handler.defaults.time_window_secs, 300.0,
        "premise: the 65 minutes below is thirteen of THIS window, so a moved \
         default moves the figure",
    );

    let stops: Vec<chrono::NaiveDateTime> = (0..13).map(loop_hour).collect();
    let residency = handler.residency_for(&pane, &stops);

    assert_eq!(
        residency.total(),
        chrono::Duration::minutes(65),
        "thirteen five-minute windows are 65 minutes of archive: {:?}",
        residency.ranges(),
    );
    assert_eq!(
        residency.ranges().len(),
        13,
        "the stops are an hour apart and the windows five minutes wide, so \
         nothing merges",
    );

    let (from, to) = residency.extent().expect("thirteen ranges have an extent");
    assert_eq!(
        to - from,
        chrono::Duration::hours(12) + chrono::Duration::minutes(5),
        "the extent is the quantity a caller reading the loop's span asked \
         the archive for — 12 h 05 min, against 65 min of it depicted",
    );

    // Every window opens exactly one `time_window_secs` behind its own stop,
    // and closes ON it. The stop being inside its own window is the law the
    // GLM bug becomes.
    for (k, stop) in stops.iter().enumerate() {
        assert!(
            residency.covers(*stop),
            "stop {k} at {stop} is not inside what the layer asked to hold",
        );
        assert!(
            residency.covers(*stop - chrono::Duration::seconds(300)),
            "and neither is the far edge of its window",
        );
        assert!(
            !residency.covers(*stop - chrono::Duration::seconds(301)),
            "one second past the window is archive this layer draws nothing \
             from",
        );
    }
}

/// The window follows the **control**, not a constant: widen
/// `time_window_secs` and the ask widens with it.
///
/// The floor for the acceptance above — without it, a `residency_for` that
/// hardcoded 300 s would pass every assertion there.
#[test]
fn the_lightning_window_is_the_one_the_pane_is_set_to() {
    let mut handler = GlmHandler::new();
    handler.defaults.time_window_secs = 1800.0;
    let pane = PaneRef::bare(0);

    let residency = handler.residency_for(&pane, &[loop_hour(0), loop_hour(1)]);
    assert_eq!(
        residency.total(),
        chrono::Duration::minutes(60),
        "two half-hour windows, because the control says half an hour",
    );
    assert_eq!(residency.ranges().len(), 2);
}

/// **A parked scrub asks for one window, and a loop for one per stop.** The
/// coalescing is doing the work rather than the fixture being degenerate.
#[test]
fn one_stop_asks_for_one_lightning_window() {
    let handler = GlmHandler::new();
    let pane = PaneRef::bare(0);

    let one = handler.residency_for(&pane, &[loop_hour(6)]);
    assert_eq!(one.ranges().len(), 1);
    assert_eq!(one.total(), chrono::Duration::minutes(5));

    // Stops closer together than the window are ONE range, not two: a
    // scrubbed pane sampling every minute does not ask for a range a minute.
    let dense: Vec<chrono::NaiveDateTime> = (0..6)
        .map(|k| loop_hour(6) + chrono::Duration::minutes(k))
        .collect();
    let merged = handler.residency_for(&pane, &dense);
    assert_eq!(
        merged.ranges().len(),
        1,
        "six overlapping windows are one stretch: {:?}",
        merged.ranges(),
    );
    assert_eq!(
        merged.total(),
        chrono::Duration::minutes(10),
        "5 min behind the first stop plus the 5 min the stops span, counted \
         once",
    );
}

fn loop_hour(k: i64) -> chrono::NaiveDateTime {
    chrono::NaiveDate::from_ymd_opt(2026, 8, 22)
        .expect("a real date")
        .and_hms_opt(0, 0, 0)
        .expect("a real time")
        + chrono::Duration::hours(k)
}

// ── The built paint rows are memoised (WO: GLM paint memo) ────────────

/// A handler holding one granule of `n` flashes, each at its own position so
/// a row set that drifted would be visible in the values and not only in the
/// pointer.
fn a_handler_holding(n: usize) -> GlmHandler {
    let mut handler = GlmHandler::new();
    handler.defaults.enabled = true;
    let base = item(GlmDataLevel::Flash, Some(1e-14), None).flash;
    let flashes: Vec<GlmFlash> = (0..n)
        .map(|i| GlmFlash {
            lat: 33.0 + i as f64 * 0.01,
            lon: -99.0 + i as f64 * 0.01,
            ..base
        })
        .collect();
    handler.apply_fetch_result(
        Box::new(GlmFetchResult(Ok(crate::glm::GlmFetchOutcome {
            flashes,
            dead_feeds: Vec::new(),
            queried: vec![GlmSatellite::GoesEast],
            parse_failures: None,
            transport_failures: None,
            level_failures: Vec::new(),
            evaluated_levels: vec![(GlmSatellite::GoesEast, GlmDataLevel::Flash)],
            listing_failures: Vec::new(),
            window_gaps: Vec::new(),
            record_drops: Default::default(),
        }))),
        &PaneRef::across(&[]),
    );
    handler
}

fn memo_clock() -> chrono::NaiveDateTime {
    chrono::NaiveDate::from_ymd_opt(2026, 7, 24)
        .unwrap()
        .and_hms_opt(12, 0, 0)
        .unwrap()
}

fn memo_ctx(
    zoom: f64,
    is_dark: bool,
    as_of: chrono::NaiveDateTime,
) -> crate::render::overlay_state::RasterizeContext {
    crate::render::overlay_state::RasterizeContext {
        is_dark,
        zoom,
        device_scale: 1.0,
        now: memo_clock(),
        as_of,
        frame: None,
    }
}

fn flashes_of(job: &DescribedJob) -> &Arc<Vec<rasterize::FlashPaint>> {
    &job.downcast_ref::<rasterize::GlmStrikesInput>()
        .expect("a GLM job")
        .flashes
}

/// **The hit.** Two dispatches a zoom quantum apart describe two inputs — the
/// zoom differs — that share ONE row allocation, and the per-flash copy ran
/// once.
///
/// This is the direction a correctness-only test cannot see: a memo whose key
/// moved every frame would satisfy every assertion about *what* the rows say
/// while rebuilding all of them every dispatch.
#[test]
fn dispatches_at_two_zooms_share_one_built_flash_row_set() {
    let handler = a_handler_holding(50);
    let pane = PaneRef::bare(0);
    let near = handler
        .prepare_job(&memo_ctx(7.0, false, memo_clock()), &pane)
        .unwrap();
    let far = handler
        .prepare_job(&memo_ctx(7.5, false, memo_clock()), &pane)
        .unwrap();
    assert_ne!(near, far, "the zoom is in the input and moved");
    assert!(
        Arc::ptr_eq(flashes_of(&near), flashes_of(&far)),
        "the flash rows must be one shared allocation, not a second copy",
    );
    assert_eq!(flashes_of(&near).len(), 50);
    assert_eq!(
        handler.flash_memo.builds.get(),
        1,
        "fifty flashes were copied once, not once per dispatch",
    );
}

/// **The clock is not in the key, and the hit is not stale.** `ctx.as_of`
/// reaches the input as `now` and this layer quantises it at one second, so a
/// key carrying it would miss on every dispatch of a scrubbed pane while the
/// memo still read as a working one. The rows are shared across the scrub;
/// the clock beside them is the dispatch's own.
#[test]
fn a_scrub_reuses_the_rows_and_still_carries_its_own_clock() {
    let handler = a_handler_holding(20);
    let pane = PaneRef::bare(0);
    let live = handler
        .prepare_job(&memo_ctx(7.0, false, memo_clock()), &pane)
        .unwrap();
    let scrubbed_at = memo_clock() - chrono::Duration::hours(1);
    let scrubbed = handler
        .prepare_job(&memo_ctx(7.0, false, scrubbed_at), &pane)
        .unwrap();
    assert!(
        Arc::ptr_eq(flashes_of(&live), flashes_of(&scrubbed)),
        "a second of scrub must not re-copy every flash: the rows carry no \
         clock term",
    );
    assert_eq!(
        handler.flash_memo.builds.get(),
        1,
        "the depicted instant must not be a key term",
    );
    assert_eq!(
        scrubbed
            .downcast_ref::<rasterize::GlmStrikesInput>()
            .unwrap()
            .now,
        scrubbed_at,
        "the shared rows must not drag the previous dispatch's clock with \
         them — the ages the rasterizer computes come from this field",
    );
    assert_eq!(
        live.downcast_ref::<rasterize::GlmStrikesInput>()
            .unwrap()
            .now,
        memo_clock(),
    );
}

/// The theme is the other per-dispatch scalar that decides bytes, and it is
/// beside the rows rather than in them.
#[test]
fn a_theme_flip_reuses_the_rows_and_still_carries_its_own_theme() {
    let handler = a_handler_holding(20);
    let pane = PaneRef::bare(0);
    let light = handler
        .prepare_job(&memo_ctx(7.0, false, memo_clock()), &pane)
        .unwrap();
    let dark = handler
        .prepare_job(&memo_ctx(7.0, true, memo_clock()), &pane)
        .unwrap();
    assert!(Arc::ptr_eq(flashes_of(&light), flashes_of(&dark)));
    assert!(
        dark.downcast_ref::<rasterize::GlmStrikesInput>()
            .unwrap()
            .is_dark
    );
    assert!(
        !light
            .downcast_ref::<rasterize::GlmStrikesInput>()
            .unwrap()
            .is_dark
    );
    assert_eq!(handler.flash_memo.builds.get(), 1);
}

/// **The miss.** A poll moves the generation, the rows rebuild — once — with
/// the new granule's values, and the old rows are parked rather than freed on
/// the frame thread.
#[test]
fn a_poll_rebuilds_the_flash_rows_once_and_parks_the_old_ones() {
    let mut handler = a_handler_holding(4);
    let pane = PaneRef::bare(0);
    let before = handler
        .prepare_job(&memo_ctx(7.0, false, memo_clock()), &pane)
        .unwrap();
    assert_eq!(flashes_of(&before).len(), 4);

    let base = item(GlmDataLevel::Flash, Some(1e-14), None).flash;
    handler.apply_fetch_result(
        Box::new(GlmFetchResult(Ok(crate::glm::GlmFetchOutcome {
            flashes: (0..7)
                .map(|i| GlmFlash {
                    lat: 40.0 + i as f64 * 0.01,
                    ..base
                })
                .collect(),
            dead_feeds: Vec::new(),
            queried: vec![GlmSatellite::GoesEast],
            parse_failures: None,
            transport_failures: None,
            level_failures: Vec::new(),
            evaluated_levels: vec![(GlmSatellite::GoesEast, GlmDataLevel::Flash)],
            listing_failures: Vec::new(),
            window_gaps: Vec::new(),
            record_drops: Default::default(),
        }))),
        &PaneRef::across(&[]),
    );

    let after = handler
        .prepare_job(&memo_ctx(7.0, false, memo_clock()), &pane)
        .unwrap();
    assert!(
        !Arc::ptr_eq(flashes_of(&before), flashes_of(&after)),
        "a poll must rebuild: holding the previous granule's rows would draw \
         the previous granule",
    );
    assert_eq!(flashes_of(&after).len(), 7, "the new granule's rows");
    assert_eq!(flashes_of(&after)[0].lat, 40.0);
    assert_eq!(
        handler.flash_memo.builds.get(),
        2,
        "one build per granule, not one per dispatch",
    );
    assert_eq!(
        handler.flash_memo.take_retired().len(),
        1,
        "the retired generation's rows were handed back, not freed inline",
    );
}

/// **Why the view half of the key is zero, derived rather than copied.**
///
/// Every pane-selected term that can change which flashes exist — the
/// satellite, the hierarchy levels, the window the fetch asks for — bumps
/// `data_generation` in `apply_control`, so it reaches the memo through the
/// generation half and never needs a view fold. A change here that stopped
/// bumping would serve one pane's satellite to another, so this pins the
/// premise rather than the consequence.
#[test]
fn every_selection_the_rows_could_depend_on_moves_the_generation() {
    for update in [
        ControlUpdate {
            id: "satellite",
            value: ControlValue::String("west".to_string()),
        },
        ControlUpdate {
            id: "time_window",
            value: ControlValue::Float(9.0),
        },
        ControlUpdate {
            id: "show_events",
            value: ControlValue::Bool(true),
        },
        ControlUpdate {
            id: "show_groups",
            value: ControlValue::Bool(true),
        },
        ControlUpdate {
            id: "show_flashes",
            value: ControlValue::Bool(false),
        },
    ] {
        let mut handler = a_handler_holding(3);
        let before = handler.state.data_generation;
        handler.apply_control(&update, &mut PaneMut::bare(0));
        assert_ne!(
            handler.state.data_generation, before,
            "control {:?} left the generation still — the flash memo keys its \
             view half on zero because every such control moves it",
            update.id,
        );
    }
}

/// An empty slab describes no job and copies nothing, and does not park an
/// empty answer against the generation that later has rows.
#[test]
fn an_empty_slab_describes_no_job_and_builds_nothing() {
    let handler = GlmHandler::new();
    assert!(
        handler
            .prepare_job(&memo_ctx(7.0, false, memo_clock()), &PaneRef::bare(0))
            .is_none()
    );
    assert_eq!(handler.flash_memo.builds.get(), 0);
}
