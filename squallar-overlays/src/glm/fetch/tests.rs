use super::*;
use chrono::Utc;

/// Only groups and flashes carry area coverage in the L2 LCFA product.
#[test]
fn only_group_and_flash_levels_declare_an_area_variable() {
    assert_eq!(FLASH_VARS.area, Some("flash_area"));
    assert_eq!(GROUP_VARS.area, Some("group_area"));
    assert_eq!(
        EVENT_VARS.area, None,
        "the GLM L2 LCFA product has no `event_area` variable"
    );
}

const FLASH_LEVEL_VARS: [&str; 5] = [
    "flash_lat",
    "flash_lon",
    "flash_energy",
    "flash_area",
    "flash_time_offset_of_first_event",
];

fn absent_variable_error(name: &str) -> String {
    format!("GLM file has no '{name}' variable (product schema change?)")
}

fn length_mismatch_error(name: &str, len: usize, reference: &str, count: usize) -> String {
    format!("GLM variable length mismatch: '{name}' has {len} values but '{reference}' has {count}")
}

#[derive(Default)]
struct Fixture<'a> {
    omit: &'a [&'a str],
    short: Option<&'a str>,
    flash_lats: Option<&'a [f32]>,
    flash_lat_fill: Option<f32>,
}

fn synthetic_glm_file(spec: Fixture<'_>) -> Vec<u8> {
    let mut file = hdf5_pure::FileBuilder::new();
    file.set_attr(
        "time_coverage_start",
        hdf5_pure::AttrValue::String("2026-07-24T12:00:00.0Z".into()),
    );

    {
        let mut put = |name: &str, values: &[f32]| {
            if spec.omit.contains(&name) {
                return;
            }
            let values = if spec.short == Some(name) {
                &values[..1]
            } else {
                values
            };
            let var = file.create_dataset(name);
            var.with_f32_data(values);
            let units = match name {
                n if n.ends_with("_area") => Some("km2"),
                n if n.ends_with("_energy") => Some("J"),
                _ => None,
            };
            if let Some(u) = units {
                var.set_attr("units", hdf5_pure::AttrValue::String(u.into()));
            }
            if name == "flash_lat"
                && let Some(fill) = spec.flash_lat_fill
            {
                var.set_attr("_FillValue", hdf5_pure::AttrValue::F64(fill as f64));
            }
        };

        put("flash_lat", spec.flash_lats.unwrap_or(&[35.0, 36.0]));
        put("flash_lon", &[-97.0, -98.0]);
        put("flash_energy", &[1.0e-14, 2.0e-14]);
        put("flash_area", &[128.0, 256.0]);
        put("flash_time_offset_of_first_event", &[1.0, 2.0]);

        put("event_lat", &[35.5, 36.5]);
        put("event_lon", &[-97.5, -98.5]);
        put("event_energy", &[3.0e-15, 4.0e-15]);
        put("event_time_offset", &[3.0, 4.0]);
    }

    file.finish().expect("write fixture")
}

fn parse_records(
    bytes: &[u8],
    satellite: GlmSatellite,
    levels: &[GlmDataLevel],
) -> Result<Vec<GlmFlash>, String> {
    parse_glm_netcdf(bytes, satellite, levels).map(|p| p.records)
}

fn parse_flashes(bytes: &[u8]) -> Result<Vec<GlmFlash>, String> {
    parse_glm_netcdf(bytes, GlmSatellite::GoesEast, &[GlmDataLevel::Flash]).map(|p| p.records)
}

#[test]
fn flash_level_reports_area_and_event_level_reports_none() {
    let bytes = synthetic_glm_file(Fixture::default());

    let flashes = parse_flashes(&bytes).expect("parse flash level");
    assert_eq!(flashes.len(), 2);
    let areas: Vec<f32> = flashes
        .iter()
        .map(|f| f.area.expect("flash area"))
        .collect();
    assert!(
        (areas[1] / areas[0] - 2.0).abs() < 1e-3,
        "area column should track the file's values, got {areas:?}"
    );
    assert!(areas.iter().all(|a| *a > 1.0), "got {areas:?}");

    let events = parse_records(&bytes, GlmSatellite::GoesEast, &[GlmDataLevel::Event])
        .expect("parse event level");
    assert_eq!(events.len(), 2);
    assert!(
        events.iter().all(|e| e.area.is_none()),
        "events must not report a fabricated area"
    );
    assert!((events[0].lat - 35.5).abs() < 1e-4);
    assert!((events[0].lon - (-97.5)).abs() < 1e-4);
}

#[test]
fn missing_required_variable_is_an_error_not_a_silent_default() {
    for missing in [
        "flash_lat",
        "flash_lon",
        "flash_energy",
        "flash_time_offset_of_first_event",
    ] {
        let bytes = synthetic_glm_file(Fixture {
            omit: &[missing],
            ..Default::default()
        });
        let err = parse_flashes(&bytes).expect_err(
            "a missing required variable must surface, not read back as an empty column",
        );
        assert_eq!(err, absent_variable_error(missing));
    }
}

#[test]
fn a_whole_level_vanishing_is_an_error_not_zero_records() {
    let bytes = synthetic_glm_file(Fixture {
        omit: &FLASH_LEVEL_VARS,
        ..Default::default()
    });
    let err = parse_flashes(&bytes)
        .expect_err("an entirely absent level must not read as 'no lightning'");
    assert_eq!(err, absent_variable_error("flash_lat"));
}

#[test]
fn a_short_required_column_is_rejected() {
    for short in [
        "flash_lon",
        "flash_energy",
        "flash_time_offset_of_first_event",
    ] {
        let bytes = synthetic_glm_file(Fixture {
            short: Some(short),
            ..Default::default()
        });
        let err =
            parse_flashes(&bytes).expect_err("a short column must be rejected, not indexed past");
        assert_eq!(err, length_mismatch_error(short, 1, "flash_lat", 2));
    }
}

#[test]
fn a_dropped_record_reaches_the_caller_and_says_which_kind() {
    let filled = synthetic_glm_file(Fixture {
        flash_lats: Some(&[35.0, -999.0]),
        flash_lat_fill: Some(-999.0),
        ..Default::default()
    });
    let parsed = parse_glm_netcdf(&filled, GlmSatellite::GoesEast, &[GlmDataLevel::Flash])
        .expect("one bad record must not fail the granule");
    assert_eq!(parsed.records.len(), 1, "the good record still draws");
    assert_eq!(
        (
            parsed.drops.considered,
            parsed.drops.fill_values,
            parsed.drops.off_globe
        ),
        (2, 1, 0),
        "a value the file marked missing is a fill-value drop, not a coordinate one",
    );

    let off_globe = synthetic_glm_file(Fixture {
        flash_lats: Some(&[35.0, -999.0]),
        ..Default::default()
    });
    let parsed = parse_glm_netcdf(&off_globe, GlmSatellite::GoesEast, &[GlmDataLevel::Flash])
        .expect("one bad record must not fail the granule");
    assert_eq!(parsed.records.len(), 1);
    assert_eq!(
        (
            parsed.drops.considered,
            parsed.drops.fill_values,
            parsed.drops.off_globe
        ),
        (2, 0, 1),
        "an unmarked coordinate off the globe is this reader and the product \
         disagreeing, and is counted as such",
    );
}

#[test]
fn a_granule_that_keeps_every_record_reports_no_drops() {
    let bytes = synthetic_glm_file(Fixture::default());
    let parsed = parse_glm_netcdf(
        &bytes,
        GlmSatellite::GoesEast,
        &[GlmDataLevel::Flash, GlmDataLevel::Event],
    )
    .expect("the fixture parses");
    assert_eq!(parsed.drops.dropped(), 0);
    assert_eq!(
        parsed.drops.considered, 4,
        "two flashes and two events were looked at, and the denominator counts \
         every level that parsed rather than only the first",
    );
}

#[test]
fn a_level_that_failed_contributes_no_denominator() {
    let bytes = synthetic_glm_file(Fixture {
        omit: &["event_lat"],
        ..Default::default()
    });
    let parsed = parse_glm_netcdf(
        &bytes,
        GlmSatellite::GoesEast,
        &[GlmDataLevel::Flash, GlmDataLevel::Event],
    )
    .expect("one level failing must not fail the granule");
    assert_eq!(parsed.level_failures.len(), 1, "the event level failed");
    assert_eq!(
        parsed.drops.considered, 2,
        "only the flash level was examined, so only its two records count",
    );
}

#[test]
fn batch_partition_keeps_every_error_and_separates_the_kinds() {
    let outcome = BatchOutcome::from_results(vec![
        Ok((
            "a.nc".into(),
            GranuleParse {
                records: Vec::new(),
                level_failures: Vec::new(),
                drops: RecordDrops::default(),
            },
        )),
        Err(FileError::Parse("b.nc: bad variable".into())),
        Err(FileError::Transport("c.nc: HTTP status error: 503".into())),
        Err(FileError::Parse("d.nc: bad variable".into())),
    ]);
    assert_eq!(outcome.entries.len(), 1);
    assert_eq!(
        outcome.parse_errors,
        vec!["b.nc: bad variable", "d.nc: bad variable"]
    );
    assert_eq!(
        outcome.transport_errors,
        vec!["c.nc: HTTP status error: 503"],
        "a 503 is a network problem and must never be counted as a parse failure"
    );
}

#[test]
fn batch_partition_dedups_level_failures_per_level_not_per_file() {
    let both_broken = || {
        vec![
            level_failure(GlmSatellite::GoesEast, GlmDataLevel::Flash),
            level_failure(GlmSatellite::GoesEast, GlmDataLevel::Group),
        ]
    };
    let outcome = BatchOutcome::from_results(vec![
        Ok((
            "a.nc".into(),
            GranuleParse {
                records: Vec::new(),
                level_failures: both_broken(),
                drops: RecordDrops::default(),
            },
        )),
        Ok((
            "b.nc".into(),
            GranuleParse {
                records: Vec::new(),
                level_failures: both_broken(),
                drops: RecordDrops::default(),
            },
        )),
        Ok((
            "c.nc".into(),
            GranuleParse {
                records: Vec::new(),
                level_failures: both_broken(),
                drops: RecordDrops::default(),
            },
        )),
    ]);

    assert_eq!(
        outcome.level_failures.len(),
        2,
        "three files reporting the same two broken levels is two reports, got {:?}",
        outcome.level_failures,
    );
    for level in [GlmDataLevel::Flash, GlmDataLevel::Group] {
        assert!(
            outcome.level_failures.iter().any(|f| f.level == level),
            "{level:?} must survive dedup, got {:?}",
            outcome.level_failures,
        );
    }
}

#[test]
fn batch_partition_sums_record_drops_rather_than_deduping_them() {
    let with_drops = |considered, fill_values, off_globe| {
        Ok((
            "x.nc".to_string(),
            GranuleParse {
                records: Vec::new(),
                level_failures: Vec::new(),
                drops: RecordDrops {
                    considered,
                    fill_values,
                    off_globe,
                },
            },
        ))
    };
    let outcome = BatchOutcome::from_results(vec![
        with_drops(100, 3, 1),
        with_drops(100, 3, 1),
        with_drops(50, 0, 2),
    ]);

    assert_eq!(
        (
            outcome.drops.considered,
            outcome.drops.fill_values,
            outcome.drops.off_globe
        ),
        (250, 6, 4),
        "three granules' drops add up; identical counts are not one report",
    );
}

#[test]
fn a_granule_that_failed_contributes_no_drops_and_no_denominator() {
    let outcome = BatchOutcome::from_results(vec![
        Err(FileError::Transport("a.nc: HTTP status error: 503".into())),
        Err(FileError::Parse("b.nc: bad variable".into())),
    ]);
    assert_eq!(outcome.drops, RecordDrops::default());
}

#[test]
fn the_accumulator_forwards_every_bucket() {
    let mut acc = PollAccumulator::default();
    acc.absorb(
        GlmSatellite::GoesWest,
        &[GlmDataLevel::Group, GlmDataLevel::Flash],
        BatchOutcome {
            entries: vec![("a.nc".into(), Vec::new())],
            parse_errors: vec!["p".into()],
            transport_errors: vec!["t".into()],
            level_failures: vec![level_failure(GlmSatellite::GoesWest, GlmDataLevel::Flash)],
            drops: RecordDrops {
                considered: 40,
                fill_values: 2,
                off_globe: 1,
            },
        },
    );

    assert_eq!(acc.entries.len(), 1);
    assert_eq!(
        (
            acc.drops.considered,
            acc.drops.fill_values,
            acc.drops.off_globe
        ),
        (40, 2, 1),
        "the record bucket must not be dropped either - it is the one that \
         reached only a log line",
    );
    assert_eq!(acc.parse_errors, vec!["p"]);
    assert_eq!(acc.transport_errors, vec!["t"]);
    assert_eq!(
        acc.level_failures.len(),
        1,
        "the level bucket must not be dropped"
    );
    assert_eq!(
        acc.evaluated_levels,
        vec![
            (GlmSatellite::GoesWest, GlmDataLevel::Group),
            (GlmSatellite::GoesWest, GlmDataLevel::Flash),
        ],
        "a granule that parsed is evidence about every level it was asked for"
    );
}

#[test]
fn a_batch_that_parsed_nothing_is_not_evidence() {
    let mut acc = PollAccumulator::default();
    acc.absorb(
        GlmSatellite::GoesEast,
        &[GlmDataLevel::Flash],
        BatchOutcome {
            entries: Vec::new(),
            parse_errors: vec!["every file failed".into()],
            transport_errors: Vec::new(),
            level_failures: Vec::new(),
            drops: RecordDrops::default(),
        },
    );
    assert!(
        acc.evaluated_levels.is_empty(),
        "a batch with no successful parse cannot vouch for any level"
    );
}

#[test]
fn poll_plan_separates_window_size_from_work_to_do() {
    let keys: Vec<String> = (0..12).map(|i| format!("k{i}.nc")).collect();

    let mut cache = GlmCache::default();
    for key in keys.iter().take(9) {
        cache.insert(key.clone(), t0(), Vec::new());
    }

    let mut tally = PollTally::default();
    let new_keys = plan_downloads(&keys, &cache, &live_round(), &mut tally);
    assert_eq!(
        tally.in_window, 12,
        "the window still contains every listed file, cached or not"
    );
    assert_eq!(new_keys.len(), 3, "only the uncached ones need downloading");

    let other: Vec<String> = (0..4).map(|i| format!("w{i}.nc")).collect();
    plan_downloads(&other, &GlmCache::default(), &live_round(), &mut tally);
    assert_eq!(tally.in_window, 16);

    let mut cache = GlmCache::default();
    for key in keys.iter().take(11) {
        cache.insert(key.clone(), t0(), Vec::new());
    }
    let mut tally = PollTally::default();
    let new_keys = plan_downloads(&keys, &cache, &live_round(), &mut tally);
    assert_eq!(new_keys.len(), 1);
    let report =
        summarize_failures(tally.in_window, vec!["k11.nc: boom".into()]).expect("one failure");
    assert!(
        !report.is_total(),
        "one straggler failing must never read as a total outage, got {report:?}"
    );
}

fn t0() -> NaiveDateTime {
    chrono::NaiveDate::from_ymd_opt(2026, 7, 24)
        .unwrap()
        .and_hms_opt(12, 0, 0)
        .unwrap()
}

fn wall_clock_unlike_keys() -> NaiveDateTime {
    t0() + TimeDelta::hours(3) + TimeDelta::minutes(7)
}

fn flash_at(time: NaiveDateTime) -> GlmFlash {
    GlmFlash {
        lat: 38.967,
        lon: -82.1,
        energy: Some(1.0e-14),
        area: Some(278.65),
        time,
        satellite: GlmSatellite::GoesEast,
        level: GlmDataLevel::Flash,
    }
}

fn cached_keys(cache: &GlmCache) -> Vec<String> {
    let mut keys: Vec<String> = cache.entries.keys().cloned().collect();
    keys.sort();
    keys
}

/// Cache a granule that parsed to at least one record, dating it the way S3
/// does: a granule is keyed by the *start* of the ~20 s span it covers, so
/// its records land at or after that instant.
fn cache_granule(cache: &mut GlmCache, key: &str, flashes: Vec<GlmFlash>) {
    let start = flashes
        .iter()
        .map(|f| f.time)
        .min()
        .expect("use GlmCache::insert directly for a granule that parsed to nothing");
    cache.insert(key.to_string(), start, flashes);
}

/// `cutoff` is *inclusive*: a granule whose newest flash lands exactly on
/// the edge would otherwise be evicted and immediately re-downloaded.
///
/// The tick either side is one millisecond because GLM times unpack through
/// a `0.0003814756 s` scale factor — sub-second is the real resolution here,
/// not a contrived epsilon.
#[test]
fn evict_before_keeps_a_granule_sitting_exactly_on_the_cutoff() {
    let cutoff = t0();
    let mut cache = GlmCache::default();
    cache_granule(&mut cache, "exactly_at.nc", vec![flash_at(cutoff)]);
    cache_granule(
        &mut cache,
        "one_tick_before.nc",
        vec![flash_at(cutoff - TimeDelta::milliseconds(1))],
    );
    cache_granule(
        &mut cache,
        "one_tick_after.nc",
        vec![flash_at(cutoff + TimeDelta::milliseconds(1))],
    );

    cache.evict_before(cutoff);

    assert_eq!(
        cached_keys(&cache),
        vec!["exactly_at.nc".to_string(), "one_tick_after.nc".to_string()],
        "the cutoff is inclusive: only the granule strictly before it goes"
    );
}

#[test]
fn evict_before_keeps_a_granule_that_straddles_the_cutoff() {
    let cutoff = t0();
    let stale = cutoff - TimeDelta::seconds(10);
    let fresh = cutoff + TimeDelta::seconds(10);

    let mut cache = GlmCache::default();
    cache_granule(
        &mut cache,
        "straddles.nc",
        vec![flash_at(stale), flash_at(fresh)],
    );

    cache.evict_before(cutoff);

    assert_eq!(cached_keys(&cache), vec!["straddles.nc".to_string()]);
    let times: Vec<NaiveDateTime> = cache.all_flashes().map(|f| f.time).collect();
    assert_eq!(
        times.len(),
        2,
        "the granule is kept intact; trimming individual flashes here would \
             mean re-downloading the file to get them back, and is not this \
             stage's job"
    );
    assert!(times.contains(&stale) && times.contains(&fresh));
}

#[test]
fn evict_before_handles_an_empty_cache_and_both_extremes() {
    let mut empty = GlmCache::default();
    empty.evict_before(t0());
    assert_eq!(empty.all_flashes().count(), 0);
    assert!(cached_keys(&empty).is_empty());

    let mut cache = GlmCache::default();
    for i in 1..=3 {
        cache_granule(
            &mut cache,
            &format!("old{i}.nc"),
            vec![flash_at(t0() - TimeDelta::minutes(i))],
        );
    }
    cache.evict_before(t0());
    assert!(
        cached_keys(&cache).is_empty(),
        "an eviction that keeps stale granules is a cache that grows forever"
    );

    let mut cache = GlmCache::default();
    for i in 1..=3 {
        cache_granule(
            &mut cache,
            &format!("new{i}.nc"),
            vec![flash_at(t0() + TimeDelta::minutes(i))],
        );
    }
    cache.evict_before(t0());
    assert_eq!(
        cached_keys(&cache).len(),
        3,
        "an eviction that clears live granules re-downloads the whole window \
             every poll"
    );
}

#[test]
fn evict_before_ages_an_empty_granule_by_its_own_start_time() {
    let start = t0();
    let mut cache = GlmCache::default();
    cache.insert("quiet.nc".into(), start, Vec::new());

    cache.evict_before(start - TimeDelta::days(365));
    assert!(
        cache.contains_key("quiet.nc"),
        "an empty parse is a successful download; evicting it here is what \
             re-fetched the whole listing window every poll"
    );

    cache.evict_before(start + TimeDelta::milliseconds(1));
    assert!(
        !cache.contains_key("quiet.nc"),
        "an empty granule that never expires is the opposite bug: a cache \
             that grows without bound"
    );
}

#[test]
fn an_empty_granule_ages_out_on_the_same_schedule_as_a_populated_one() {
    let start = t0();
    let tick = TimeDelta::milliseconds(1);

    for cutoff in [start - tick, start, start + tick] {
        let mut cache = GlmCache::default();
        cache.insert("quiet.nc".into(), start, Vec::new());
        cache_granule(&mut cache, "busy.nc", vec![flash_at(start)]);

        cache.evict_before(cutoff);

        assert_eq!(
            cache.contains_key("quiet.nc"),
            cache.contains_key("busy.nc"),
            "at cutoff {cutoff} the empty granule and the populated one that \
                 covers the same instant disagreed: quiet={}, busy={}",
            cache.contains_key("quiet.nc"),
            cache.contains_key("busy.nc"),
        );
    }
}

#[test]
fn a_quiet_granule_is_downloaded_once_not_once_per_poll() {
    let key = "GLM-L2-LCFA/2026/205/12/\
                   OR_GLM-L2-LCFA_G19_s20262051200000_e20262051200200_c20262051200214.nc";
    let start = parse_filename_start_time(key).expect("fixture key must be datable");
    let listing = vec![key.to_string()];

    let mut cache = GlmCache::default();
    let mut tally = PollTally::default();
    assert_eq!(
        plan_downloads(&listing, &cache, &round_covering(start), &mut tally).len(),
        1,
        "an uncached granule must be downloaded once"
    );
    cache.insert(
        key.to_string(),
        granule_start_of(key, wall_clock_unlike_keys()),
        Vec::new(),
    );

    for poll in 1..=5 {
        let cutoff = start - TimeDelta::minutes(30) + TimeDelta::seconds(20 * poll);
        cache.evict_before(cutoff);
        let mut tally = PollTally::default();
        assert!(
            plan_downloads(&listing, &cache, &round_covering(start), &mut tally).is_empty(),
            "poll {poll}: a granule already downloaded and found empty was \
                 re-queued — this is the every-poll re-fetch, back"
        );
        assert_eq!(
            tally.in_window, 1,
            "it is still in the window, just not new work"
        );
    }

    cache.evict_before(start + TimeDelta::milliseconds(1));
    assert!(
        !cache.contains_key(key),
        "a stale empty granule must be evicted, or the cache never shrinks"
    );
}

#[test]
fn cache_granules_keeps_the_empty_ones_too() {
    let busy = "GLM-L2-LCFA/2026/205/12/\
                    OR_GLM-L2-LCFA_G19_s20262051200000_e20262051200200_c20262051200214.nc";
    let quiet = "GLM-L2-LCFA/2026/205/12/\
                     OR_GLM-L2-LCFA_G19_s20262051200200_e20262051200400_c20262051200414.nc";
    let now = wall_clock_unlike_keys();

    let mut cache = GlmCache::default();
    cache_granules(
        &mut cache,
        vec![
            (busy.to_string(), vec![flash_at(t0())]),
            (quiet.to_string(), Vec::new()),
        ],
        now,
    );

    assert!(cache.contains_key(busy));
    assert!(
        cache.contains_key(quiet),
        "a granule that downloaded and parsed to nothing is still downloaded"
    );

    cache.evict_before(
        parse_filename_start_time(quiet).expect("fixture key") + TimeDelta::milliseconds(1),
    );
    assert!(
        !cache.contains_key(quiet),
        "the empty granule ages by its own start time"
    );
}

#[test]
fn granule_start_comes_from_the_key_and_falls_back_to_now() {
    let now = wall_clock_unlike_keys();
    let key = "GLM-L2-LCFA/2026/205/12/\
                   OR_GLM-L2-LCFA_G19_s20262051200000_e20262051200200_c20262051200214.nc";
    assert_eq!(
        granule_start_of(key, now),
        chrono::NaiveDate::from_yo_opt(2026, 205)
            .unwrap()
            .and_hms_opt(12, 0, 0)
            .unwrap(),
        "a listed key carries its own start time; `now` must not be reached"
    );

    assert_eq!(
        granule_start_of("not-a-glm-key.nc", now),
        now,
        "an undatable granule expires one window from now — never instantly \
             (re-fetched every poll) and never not at all (unbounded cache)"
    );
}

#[test]
fn the_window_filter_includes_both_bounds() {
    let cutoff = t0();
    let now = cutoff + TimeDelta::minutes(5);
    let tick = TimeDelta::milliseconds(1);

    let mut cache = GlmCache::default();
    cache_granule(
        &mut cache,
        "spread.nc",
        vec![
            flash_at(cutoff - tick),
            flash_at(cutoff),
            flash_at(cutoff + TimeDelta::minutes(2)),
            flash_at(now),
            flash_at(now + tick),
        ],
    );

    let mut got: Vec<NaiveDateTime> =
        flashes_in_window(&cache, &[GlmSatellite::GoesEast], cutoff, now)
            .into_iter()
            .map(|f| f.time)
            .collect();
    got.sort();

    assert_eq!(
        got,
        vec![cutoff, cutoff + TimeDelta::minutes(2), now],
        "both ends are inclusive and nothing outside them survives"
    );
}

fn west_flash_at(time: NaiveDateTime) -> GlmFlash {
    GlmFlash {
        satellite: GlmSatellite::GoesWest,
        ..flash_at(time)
    }
}

#[test]
fn deselecting_a_satellite_hides_its_cached_flashes_without_evicting_them() {
    let cutoff = t0();
    let now = cutoff + TimeDelta::minutes(5);
    let t = cutoff + TimeDelta::minutes(2);

    let mut cache = GlmCache::default();
    cache_granule(&mut cache, "east.nc", vec![flash_at(t)]);
    cache_granule(&mut cache, "west.nc", vec![west_flash_at(t)]);

    let both = [GlmSatellite::GoesEast, GlmSatellite::GoesWest];
    assert_eq!(flashes_in_window(&cache, &both, cutoff, now).len(), 2);

    let east_only = flashes_in_window(&cache, &[GlmSatellite::GoesEast], cutoff, now);
    assert!(
        east_only
            .iter()
            .all(|f| f.satellite == GlmSatellite::GoesEast),
        "a deselected bird's cached flashes must not render"
    );
    assert_eq!(east_only.len(), 1, "the East flash still renders");

    assert!(
        cache.contains_key("west.nc"),
        "deselection filters the poll output; evicting here would make \
             re-selection re-download the whole window"
    );
    assert_eq!(
        flashes_in_window(&cache, &both, cutoff, now).len(),
        2,
        "re-selecting the bird restores its flashes straight from cache"
    );
}

#[test]
fn a_backwards_clock_hides_flashes_without_losing_them() {
    let window = TimeDelta::minutes(5);
    let ahead = t0() + TimeDelta::minutes(3);

    let mut cache = GlmCache::default();
    cache_granule(
        &mut cache,
        "granule.nc",
        vec![flash_at(t0()), flash_at(ahead)],
    );

    let now = t0() + TimeDelta::minutes(1);
    let cutoff = now - window;
    cache.evict_before(cutoff);

    assert!(
        cache.contains_key("granule.nc"),
        "a backwards clock must not evict data it has not caught up to yet"
    );
    let during: Vec<NaiveDateTime> =
        flashes_in_window(&cache, &[GlmSatellite::GoesEast], cutoff, now)
            .into_iter()
            .map(|f| f.time)
            .collect();
    assert_eq!(
        during,
        vec![t0()],
        "the flash stamped after `now` is withheld, not published"
    );

    let now = ahead + TimeDelta::minutes(1);
    let cutoff = now - window;
    cache.evict_before(cutoff);
    let mut after: Vec<NaiveDateTime> =
        flashes_in_window(&cache, &[GlmSatellite::GoesEast], cutoff, now)
            .into_iter()
            .map(|f| f.time)
            .collect();
    after.sort();
    assert_eq!(
        after,
        vec![t0(), ahead],
        "both flashes were held in the cache the whole time"
    );
}

fn level_failure(satellite: GlmSatellite, level: GlmDataLevel) -> LevelFailure {
    LevelFailure {
        satellite,
        level,
        sample_error: format!("{level:?} broke"),
    }
}

#[test]
fn build_outcome_binds_each_bucket_to_its_own_field() {
    let tally = PollTally { in_window: 12 };
    let acc = PollAccumulator {
        parse_errors: vec!["a.nc: GLM file has no 'flash_lat' variable".into()],
        transport_errors: vec!["b.nc: HTTP status error: 503".into()],
        level_failures: vec![level_failure(GlmSatellite::GoesWest, GlmDataLevel::Flash)],
        evaluated_levels: vec![
            (GlmSatellite::GoesWest, GlmDataLevel::Flash),
            (GlmSatellite::GoesWest, GlmDataLevel::Group),
        ],
        ..Default::default()
    };
    let outcome = build_outcome(
        Vec::new(),
        Vec::new(),
        Vec::new(),
        vec![GlmSatellite::GoesEast],
        Vec::new(),
        &tally,
        acc,
    );

    assert_eq!(
        outcome.parse_failures.expect("parse failures").sample_error,
        "a.nc: GLM file has no 'flash_lat' variable",
    );
    assert_eq!(
        outcome
            .transport_failures
            .expect("transport failures")
            .sample_error,
        "b.nc: HTTP status error: 503",
    );
    assert_eq!(outcome.queried, vec![GlmSatellite::GoesEast]);

    assert_eq!(
        outcome.level_failures,
        vec![level_failure(GlmSatellite::GoesWest, GlmDataLevel::Flash)],
    );
    assert_eq!(
        outcome.evaluated_levels,
        vec![
            (GlmSatellite::GoesWest, GlmDataLevel::Flash),
            (GlmSatellite::GoesWest, GlmDataLevel::Group),
        ],
        "the evidence set must survive independently of the failures, or \
             every quiet poll reads as a recovery"
    );
}

#[test]
fn build_outcome_keeps_level_failures_out_of_the_file_counts() {
    let tally = PollTally { in_window: 9 };
    let acc = PollAccumulator {
        level_failures: vec![level_failure(GlmSatellite::GoesEast, GlmDataLevel::Group)],
        ..Default::default()
    };
    let outcome = build_outcome(
        Vec::new(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        &tally,
        acc,
    );

    assert!(outcome.parse_failures.is_none(), "no *file* failed");
    assert!(outcome.transport_failures.is_none());
    assert_eq!(outcome.level_failures.len(), 1);
}

#[test]
fn build_outcome_leaves_an_empty_bucket_unreported() {
    let tally = PollTally { in_window: 14 };
    let acc = PollAccumulator {
        parse_errors: vec!["a.nc: boom".into()],
        ..Default::default()
    };
    let outcome = build_outcome(
        Vec::new(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        &tally,
        acc,
    );

    assert_eq!(
        outcome.parse_failures.expect("parse failures").in_window,
        14
    );
    assert!(
        outcome.transport_failures.is_none(),
        "nothing failed to download, so there is nothing to report"
    );
}

#[test]
fn garbage_bytes_are_a_parse_failure_not_a_transport_failure() {
    let err = parse_downloaded_file(
        b"this is not a netcdf file",
        GlmSatellite::GoesEast,
        &[GlmDataLevel::Flash],
    )
    .expect_err("garbage must not parse");

    assert!(
        matches!(err, FileError::Parse(_)),
        "expected a parse failure, got {err:?}"
    );
}

#[test]
fn a_good_granule_parses_through_the_classified_stage() {
    let bytes = synthetic_glm_file(Fixture::default());
    let flashes = parse_downloaded_file(&bytes, GlmSatellite::GoesEast, &[GlmDataLevel::Flash])
        .expect("fixture should parse");
    assert_eq!(flashes.records.len(), 2);
    assert!(flashes.level_failures.is_empty());
}

/// Hermetic: loopback port 1 (`tcpmux`) is not listening, so the connection
/// is refused immediately.
#[test]
fn an_unreachable_host_is_a_transport_failure() {
    // `reqwest` is pinned to `rustls-no-provider`, so `build()` panics with
    // "No provider set" unless a crypto provider is installed first.
    // `tls::client` is not used because it sets `https_only`, and the
    // cleartext loopback URL below is the point of the test.
    squallar_source::tls::init();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("test runtime");
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .expect("client");

    let err = runtime
        .block_on(download_and_parse_one(
            &client,
            "http://127.0.0.1:1/nonexistent.nc",
            GlmSatellite::GoesEast,
            &[GlmDataLevel::Flash],
        ))
        .expect_err("nothing listens on loopback port 1");

    assert!(
        matches!(err, FileError::Transport(_)),
        "a refused connection must not be reported as a product problem, got {err:?}"
    );
}

#[test]
fn summarize_failures_reports_none_when_everything_worked() {
    assert!(summarize_failures(12, Vec::new()).is_none());
}

#[test]
fn summarize_failures_distinguishes_total_from_partial() {
    let partial = summarize_failures(12, vec!["a".into(), "b".into()]).expect("failures present");
    assert_eq!((partial.failed, partial.in_window), (2, 12));
    assert!(!partial.is_total());
    assert_eq!(
        partial.sample_error, "a",
        "should keep the first error as the sample"
    );

    let total =
        summarize_failures(3, vec!["a".into(), "b".into(), "c".into()]).expect("failures present");
    assert!(total.is_total(), "every file in the window failed");
}

fn all_failed(in_window: usize) -> FetchFailures {
    let errors: Vec<String> = (0..in_window).map(|i| format!("f{i}: boom")).collect();
    summarize_failures(in_window, errors).expect("failures present")
}

#[test]
fn total_verdict_needs_more_than_one_file() {
    assert!(
        !all_failed(1).is_total(),
        "a single-file window is too small a sample to call systematic"
    );
    assert!(
        all_failed(2).is_total(),
        "two files is the smallest honest verdict, and is what a 60 s window holds"
    );
    assert!(all_failed(3).is_total());
    assert!(all_failed(14).is_total(), "the default 300 s window");
}

#[test]
fn one_bad_granule_is_never_a_total_failure() {
    for in_window in [2usize, 5, 14, 89] {
        let report =
            summarize_failures(in_window, vec!["f0: boom".into()]).expect("failures present");
        assert!(
            !report.is_total(),
            "1 of {in_window} failing is a bad granule, not a product change"
        );
    }
}

#[test]
fn a_short_area_column_degrades_to_no_area() {
    let bytes = synthetic_glm_file(Fixture {
        short: Some("flash_area"),
        ..Default::default()
    });
    let flashes = parse_flashes(&bytes).expect("a short area must not fail the file");
    assert_eq!(flashes.len(), 2);
    assert!(flashes.iter().all(|f| f.area.is_none()));
}

#[test]
fn missing_optional_area_degrades_without_failing_the_file() {
    let bytes = synthetic_glm_file(Fixture {
        omit: &["flash_area"],
        ..Default::default()
    });
    let flashes = parse_flashes(&bytes).expect("a missing area must not blank the whole overlay");
    assert_eq!(flashes.len(), 2);
    assert!(flashes.iter().all(|f| f.area.is_none()));
    assert!((flashes[0].lat - 35.0).abs() < 1e-4);
    assert!(flashes[0].energy.is_some_and(|e| e > 0.0));
}

#[test]
fn a_listing_round_is_refused_only_when_every_satellite_was() {
    use crate::fetch_policy::{FetchError, FetchFailure};

    let context = "no GLM satellite could be listed (2 failed)";
    let cases: [(Vec<FetchError>, FetchFailure); 3] = [
        (
            vec![
                FetchError::permanent("S3 returned HTTP 400"),
                FetchError::permanent("S3 returned HTTP 400"),
            ],
            FetchFailure::Permanent,
        ),
        (
            vec![
                FetchError::permanent("S3 returned HTTP 400"),
                FetchError::transient("S3 list request failed: timed out"),
            ],
            FetchFailure::Transient,
        ),
        (
            vec![FetchError::transient("S3 list request failed: timed out")],
            FetchFailure::Transient,
        ),
    ];
    for (verdicts, expected) in cases {
        let round = FetchError::of_round(&verdicts, context);
        assert_eq!(round.failure, expected);
        assert!(
            round.message.contains("S3"),
            "the round must keep the origins\' own words: {}",
            round.message,
        );
    }
}

fn listing_xml(key: &str) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <ListBucketResult xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\">\
         <Name>bucket</Name><IsTruncated>false</IsTruncated>\
         <Contents><Key>{key}</Key><Size>1</Size></Contents>\
         </ListBucketResult>"
    )
}

fn http_response(status_line: &str, body: &str) -> String {
    format!(
        "HTTP/1.1 {status_line}\r\nContent-Type: application/xml\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len(),
    )
}

const STALE_GRANULE: &str =
    "GLM-L2-LCFA/2020/001/00/OR_GLM-L2-LCFA_G18_s20200010000000_e20200010000200_c20200010000210.nc";

/// Every request line the mock bucket was sent, in arrival order.
type RecordedRequests = std::sync::Arc<std::sync::Mutex<Vec<String>>>;

fn s3_serving(responses: Vec<(&'static str, String)>) -> DataSources {
    s3_recording(responses).0
}

/// [`s3_serving`], plus the request lines - so a test can assert **what was
/// asked for**, not only what came back.
fn s3_recording(responses: Vec<(&'static str, String)>) -> (DataSources, RecordedRequests) {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let port = listener.local_addr().expect("local addr").port();
    let seen: RecordedRequests = Default::default();
    let recorder = std::sync::Arc::clone(&seen);
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { break };
            let mut scratch = [0u8; 4096];
            let read = stream.read(&mut scratch).unwrap_or(0);
            let request = String::from_utf8_lossy(&scratch[..read]).to_string();
            if let Some(line) = request.lines().next() {
                recorder
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .push(line.to_string());
            }
            let response = responses
                .iter()
                .find(|(bucket, _)| request.contains(&format!("/{bucket}/")))
                .map(|(_, response)| response.clone())
                .unwrap_or_else(|| http_response("404 Not Found", "<Error/>"));
            let _ = stream.write_all(response.as_bytes());
            let _ = stream.flush();
        }
    });
    let sources = DataSources {
        goes_east_bucket: "east".into(),
        goes_west_bucket: "west".into(),
        s3_base: format!("http://127.0.0.1:{port}/{{bucket}}").into(),
        ..DataSources::production()
    };
    (sources, seen)
}

/// A cleartext-capable client: `tls::client` sets `https_only`, which a
/// loopback URL cannot satisfy, and `tls::init` is still required because
/// `reqwest` is pinned to `rustls-no-provider`.
fn loopback_client() -> reqwest::Client {
    squallar_source::tls::init();
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .expect("client")
}

fn cached_flash(cache: &mut GlmCache, key: &str, satellite: GlmSatellite) {
    let time = Utc::now().naive_utc() - TimeDelta::seconds(10);
    cache.insert(
        key.to_string(),
        time,
        vec![GlmFlash {
            lat: 35.0,
            lon: -97.0,
            energy: Some(1.0),
            area: Some(1.0),
            time,
            satellite,
            level: GlmDataLevel::Flash,
        }],
    );
}

/// A live pane's poll: the depicted instant is the wall clock.
fn poll(sources: &DataSources, cache: &mut GlmCache) -> Result<GlmFetchOutcome, FetchError> {
    poll_as_of(sources, cache, Utc::now().naive_utc())
}

fn poll_as_of(
    sources: &DataSources,
    cache: &mut GlmCache,
    as_of: NaiveDateTime,
) -> Result<GlmFetchOutcome, FetchError> {
    poll_spanned(sources, cache, as_of, None)
}

/// [`poll_as_of`] under a depicted span — what a poll dispatched from a
/// looping (or parked) pane carries.
fn poll_spanned(
    sources: &DataSources,
    cache: &mut GlmCache,
    as_of: NaiveDateTime,
    depicted_span_secs: Option<u64>,
) -> Result<GlmFetchOutcome, FetchError> {
    let client = loopback_client();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("test runtime");
    runtime.block_on(fetch_glm_flashes(
        &client,
        sources,
        &[GlmSatellite::GoesEast, GlmSatellite::GoesWest],
        &[GlmDataLevel::Flash],
        cache,
        as_of,
        span_residency(as_of, depicted_span_secs, GLM_MIN_TIME_WINDOW_SECS),
    ))
}

#[test]
fn one_dead_satellite_does_not_blank_the_others_flashes() {
    let sources = s3_serving(vec![
        (
            "east",
            http_response("500 Internal Server Error", "<Error/>"),
        ),
        ("west", http_response("200 OK", &listing_xml(STALE_GRANULE))),
    ]);
    let mut cache = GlmCache::default();
    cached_flash(&mut cache, "east/granule.nc", GlmSatellite::GoesEast);
    cached_flash(&mut cache, "west/granule.nc", GlmSatellite::GoesWest);

    let outcome = poll(&sources, &mut cache).expect("one satellite answered; the round stands");

    assert_eq!(
        outcome.queried,
        vec![GlmSatellite::GoesWest],
        "a satellite whose listing never answered must not be counted as queried \
         — absence from `dead_feeds` reads as recovery for anything in there",
    );
    let (dead, why) = outcome
        .listing_failures
        .first()
        .expect("the failed listing must be reported, not swallowed");
    assert_eq!(*dead, GlmSatellite::GoesEast);
    assert!(why.message.contains("500"), "{}", why.message);
    assert!(
        outcome.dead_feeds.is_empty(),
        "the satellite that answered had objects; only a zero-object listing is \
         a dead feed",
    );

    let mut satellites: Vec<GlmSatellite> = outcome.flashes.iter().map(|f| f.satellite).collect();
    satellites.sort_by_key(|s| format!("{s:?}"));
    assert_eq!(
        satellites,
        vec![GlmSatellite::GoesEast, GlmSatellite::GoesWest],
        "the survivor's flashes were dropped with the failed satellite's — and \
         so were the failed satellite's own earlier granules, which are still \
         real flashes inside the window",
    );
}

fn fresh_granule_key(now: NaiveDateTime) -> String {
    let start = now - TimeDelta::seconds(20);
    format!(
        "GLM-L2-LCFA/{}/{}/OR_GLM-L2-LCFA_G18_s{}0_e1_c2.nc",
        start.format("%Y/%j"),
        start.format("%H"),
        start.format("%Y%j%H%M%S"),
    )
}

#[test]
fn a_listing_with_no_in_window_granule_is_a_window_gap_not_a_dead_feed() {
    let sources = s3_serving(vec![
        ("east", http_response("200 OK", &listing_xml(STALE_GRANULE))),
        ("west", http_response("200 OK", &listing_xml(STALE_GRANULE))),
    ]);
    let mut cache = GlmCache::default();

    let outcome = poll(&sources, &mut cache).expect("both listings answered");

    let gapped: Vec<GlmSatellite> = outcome.window_gaps.iter().map(|g| g.satellite).collect();
    assert_eq!(
        gapped,
        vec![GlmSatellite::GoesEast, GlmSatellite::GoesWest],
        "a listing that placed no granule in the window must say so; this is the \
         round that returned Ok with an empty layer and a fresh clock",
    );
    assert!(
        outcome.window_gaps.iter().all(|g| g.objects_seen > 0),
        "the count is what separates this from a dead feed, and the report is \
         useless without it",
    );
    assert!(
        outcome.dead_feeds.is_empty(),
        "the bucket is not empty, and telling the operator it is sends them to \
         check the one thing that is fine",
    );
}

#[test]
fn an_empty_bucket_is_a_dead_feed_and_not_also_a_window_gap() {
    let empty = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
                 <ListBucketResult xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\">\
                 <Name>bucket</Name><IsTruncated>false</IsTruncated>\
                 </ListBucketResult>";
    let sources = s3_serving(vec![
        ("east", http_response("200 OK", empty)),
        ("west", http_response("200 OK", empty)),
    ]);
    let mut cache = GlmCache::default();

    let outcome = poll(&sources, &mut cache).expect("both listings answered");

    assert_eq!(outcome.dead_feeds.len(), 2);
    assert!(
        outcome.window_gaps.is_empty(),
        "an empty bucket is already reported as dead; saying its listing was \
         healthy in the same breath is a contradiction",
    );
}

#[test]
fn a_granule_in_the_window_is_never_a_window_gap() {
    let key = fresh_granule_key(Utc::now().naive_utc());
    let sources = s3_serving(vec![
        ("east", http_response("200 OK", &listing_xml(&key))),
        ("west", http_response("200 OK", &listing_xml(&key))),
    ]);
    let mut cache = GlmCache::default();

    let outcome = poll(&sources, &mut cache).expect("both listings answered");

    assert!(
        outcome.window_gaps.is_empty(),
        "the window holds a granule, so the feed is publishing; that the granule \
         then failed is a different report and has one",
    );
    assert!(outcome.dead_feeds.is_empty());
    assert!(
        outcome.flashes.is_empty(),
        "the mock serves XML for the object body, so nothing parses - the point \
         is that an empty layer is still not a window gap",
    );
}

#[test]
fn a_round_fails_only_when_no_satellite_could_be_listed() {
    let sources = s3_serving(vec![
        ("east", http_response("400 Bad Request", "<Error/>")),
        ("west", http_response("503 Service Unavailable", "<Error/>")),
    ]);
    let mut cache = GlmCache::default();
    cached_flash(&mut cache, "west/granule.nc", GlmSatellite::GoesWest);

    let Err(err) = poll(&sources, &mut cache) else {
        panic!("no satellite could be listed, so the round has nothing to stand on");
    };

    assert_eq!(
        err.failure,
        crate::fetch_policy::FetchFailure::Transient,
        "one bucket refusing while the other is unavailable is not the product \
         refusing us: {}",
        err.message,
    );
    assert!(
        err.message
            .contains("no GLM satellite could be listed (2 failed)"),
        "{}",
        err.message,
    );
    assert!(
        err.message.contains("400") && err.message.contains("503"),
        "the round must keep both origins' own words: {}",
        err.message,
    );
}

#[test]
fn a_multibyte_key_is_undatable_rather_than_fatal() {
    for key in [
        "GLM-L2-LCFA/2026/205/12/OR_GLM-L2-LCFA_G19_sé.nc",
        "OR_GLM-L2-LCFA_G19_séééééé.nc",
        "OR_GLM-L2-LCFA_G19_s2026205120000é_e1_c2.nc",
        "OR_GLM-L2-LCFA_G19_s20é62051200000_e1_c2.nc",
    ] {
        assert_eq!(
            parse_filename_start_time(key),
            None,
            "{key:?} names no time, and must not panic on the way to saying so",
        );
    }
}

#[test]
fn the_ascii_ranges_a_real_key_carries_still_parse() {
    let key = "GLM-L2-LCFA/2026/205/12/\
               OR_GLM-L2-LCFA_G19_s20262051200000_e20262051200200_c20262051200214.nc";
    let start = parse_filename_start_time(key).expect("a real key must still date");
    assert_eq!(
        start.format("%Y-%m-%d %H:%M:%S").to_string(),
        "2026-07-24 12:00:00"
    );
}

// ---------------------------------------------------------------------------
// The depicted instant picks the archive.
//
// GLM is a `TimeAxis::EventLifetime` layer: its picture is "the strikes of the
// last N seconds **as of T**". `list_glm_files` has always been addressed by
// `{year}/{doy}/{hour}`, so once the poll takes T rather than the wall clock,
// the whole published archive is reachable. These are end-to-end over the
// loopback bucket: they assert the request that went out and the flashes that
// came back, not an intermediate figure.

/// 2020-06-15 07:30:00 UTC. Day 167 of a leap year; the prefix below is spelled
/// out rather than derived, so a wrong `%j` cannot agree with itself.
fn archive_instant() -> NaiveDateTime {
    chrono::NaiveDate::from_ymd_opt(2020, 6, 15)
        .unwrap()
        .and_hms_opt(7, 30, 0)
        .unwrap()
}

const ARCHIVE_PREFIX: &str = "GLM-L2-LCFA/2020/167/07/";

/// A granule starting 07:29:30 - inside the 60 s window ending at
/// [`archive_instant`], and under [`ARCHIVE_PREFIX`].
const ARCHIVE_GRANULE: &str = "GLM-L2-LCFA/2020/167/07/\
     OR_GLM-L2-LCFA_G19_s20201670729300_e20201670729400_c20201670729410.nc";

fn seed_flash(cache: &mut GlmCache, key: &str, time: NaiveDateTime) {
    cache.insert(
        key.to_string(),
        time,
        vec![GlmFlash {
            lat: 35.0,
            lon: -97.0,
            energy: Some(1.0),
            area: Some(1.0),
            time,
            satellite: GlmSatellite::GoesEast,
            level: GlmDataLevel::Flash,
        }],
    );
}

/// **The residency a round with no depicted frames produces** — the span
/// posture, whose stops are the whole interval `as_of ± span` sampled at the
/// layer's own window, so the per-stop asks touch and `Residency::over`
/// coalesces them into the one unbroken range the span really is.
///
/// This is `GlmHandler::depicted_stops` fed through
/// `SourceHandler::residency_for`, stated as its value because that method is
/// the handler's own. The equality is pinned end to end by
/// [`a_poll_landing_on_the_loops_oldest_frame_still_fills_the_whole_sweep`],
/// which dispatches through `create_fetch_tasks` and counts the archive hours
/// the real derivation reaches.
fn span_residency(as_of: NaiveDateTime, span_secs: Option<u64>, window_secs: f64) -> Residency {
    let span = TimeDelta::seconds(span_secs.unwrap_or(0) as i64);
    let window = TimeDelta::milliseconds((window_secs * 1000.0) as i64);
    Residency::over([(as_of - span - window, as_of + span)])
}

/// The listed ranges of a live round — a pane depicting one moving instant.
/// Its `as_of` is deliberately nowhere near the fixture keys, which is fine
/// for keys that carry no readable stamp: an undatable granule always passes
/// the download filter.
fn live_round() -> Vec<(NaiveDateTime, NaiveDateTime)> {
    listed_ranges(&span_residency(
        wall_clock_unlike_keys(),
        None,
        GLM_MIN_TIME_WINDOW_SECS,
    ))
}

/// [`live_round`] sampled just after `granule` — a round whose own listing
/// would have returned that granule.
///
/// `list_glm_files` clips its keys to the range it was asked over, so this is
/// the only kind of round `plan_downloads` is ever handed a **datable** key
/// under. Before WO-T3.11 a round naming no frames matched every key
/// unconditionally, so a fixture could hand it a key from another era; the
/// answer is identical in production either way, and stating the round the key
/// belongs to is what keeps that true here.
fn round_covering(granule: NaiveDateTime) -> Vec<(NaiveDateTime, NaiveDateTime)> {
    listed_ranges(&span_residency(
        granule + TimeDelta::seconds(1),
        None,
        GLM_MIN_TIME_WINDOW_SECS,
    ))
}

fn request_paths(seen: &RecordedRequests) -> Vec<String> {
    seen.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

/// A scrubbed pane asks the archive for the hour it depicts, and keeps the
/// flashes it is showing.
///
/// Three things at once, because they are one behaviour and a hand-armed test
/// of any one of them passes while the other two are broken:
///
/// 1. the **listing prefix** is the depicted hour, not the current one;
/// 2. the listing's `start`/`end` bounds moved with it - a granule inside the
///    depicted window is accepted, so the round reports no window gap;
/// 3. **retention** is anchored on the depicted instant, so the granule the
///    pane is displaying survives eviction and comes back in `flashes`.
#[test]
fn a_scrubbed_poll_reaches_the_archive_hour_it_depicts_and_keeps_it() {
    let (sources, seen) = s3_recording(vec![
        (
            "east",
            http_response("200 OK", &listing_xml(ARCHIVE_GRANULE)),
        ),
        (
            "west",
            http_response("200 OK", &listing_xml(ARCHIVE_GRANULE)),
        ),
    ]);

    // Already held, so the round downloads nothing: what is under test is
    // which hour was asked for and what survived, not the parse path.
    let mut cache = GlmCache::default();
    seed_flash(
        &mut cache,
        ARCHIVE_GRANULE,
        archive_instant() - TimeDelta::seconds(25),
    );

    let outcome =
        poll_as_of(&sources, &mut cache, archive_instant()).expect("both listings answered");

    let paths = request_paths(&seen);
    assert_eq!(
        paths.len(),
        2,
        "one listing per satellite and no downloads: {paths:?}",
    );
    assert!(
        paths.iter().all(|p| p.contains(ARCHIVE_PREFIX)),
        "the poll depicts 2020-06-15 07:30 UTC, so it must list \
         {ARCHIVE_PREFIX}; it asked for {paths:?}",
    );
    let this_year = Utc::now().naive_utc().format("/%Y/").to_string();
    assert!(
        !paths.iter().any(|p| p.contains(&this_year)),
        "the wall clock leaked into a scrubbed poll: {paths:?}",
    );

    assert!(
        outcome.window_gaps.is_empty(),
        "the archive granule sits inside the depicted window, so the listing \
         bounds moved with the prefix; a gap here means only the prefix did",
    );
    assert!(outcome.dead_feeds.is_empty());

    let times: Vec<NaiveDateTime> = outcome.flashes.iter().map(|f| f.time).collect();
    assert_eq!(
        times,
        vec![archive_instant() - TimeDelta::seconds(25)],
        "the scrubbed pane's own flash was evicted or filtered away; this is \
         the round that shows an empty sky over a storm five years ago",
    );
}

/// The other half of the same behaviour, and the reason "cull everything" or
/// "fetch nothing" cannot pass: a **live** poll is what it always was. It asks
/// for the current hour, it keeps the flashes inside its own window, and it
/// drops the five-year-old granule sitting beside them.
///
/// The clock is sampled **once** and handed to the poll, rather than sampled
/// here and again inside it: a live pane's `as_of` *is* one `Utc::now()`, and
/// two samples straddle the second (and the hour) that the prefixes below are
/// derived from. The prefix assertion is over the hours the live window
/// `[now - GLM_MIN_TIME_WINDOW_SECS, now]` actually covers, which is **two**
/// for the first 60 s of every hour — asserting the single current hour read
/// green for 98.3% of the wall clock and red for the rest, and did fail here
/// once, at 22:00Z on 2026-08-22.
#[test]
fn a_live_poll_still_asks_for_the_current_hour_and_evicts_the_archive() {
    let now = Utc::now().naive_utc();
    let live_key = fresh_granule_key(now);
    let (sources, seen) = s3_recording(vec![
        ("east", http_response("200 OK", &listing_xml(&live_key))),
        ("west", http_response("200 OK", &listing_xml(&live_key))),
    ]);

    let mut cache = GlmCache::default();
    seed_flash(&mut cache, &live_key, now - TimeDelta::seconds(10));
    seed_flash(
        &mut cache,
        ARCHIVE_GRANULE,
        archive_instant() - TimeDelta::seconds(25),
    );

    let outcome = poll_as_of(&sources, &mut cache, now).expect("both listings answered");

    let paths = request_paths(&seen);
    let current = now.format("GLM-L2-LCFA/%Y/%j/%H/").to_string();
    // **`- GRANULE_SPAN`, and leaving it out is a wall-clock flake.** The
    // listing does not open at the residency's start: `listed_ranges` widens
    // every range back by one [`GRANULE_SPAN`], because a granule whose key
    // names 40 s before the window still carries content inside it. This line
    // used to restate the window as `now - GLM_MIN_TIME_WINDOW_SECS` alone,
    // which is a DIFFERENT hour from production's for the 40 s after
    // `hh:01:00` — the poll asks for the previous hour, the test says its
    // window does not cover it, and the assertion fails on a correct build.
    // Measured 2026-09-01: 3 failures in 34 runs of the whole crate binary,
    // all three inside 01:01:0x, none of them load-related. Taken off the
    // production constant rather than restated, so the two cannot drift again.
    let window_start =
        now - TimeDelta::seconds(GLM_MIN_TIME_WINDOW_SECS as i64) - super::GRANULE_SPAN;
    let opened_in = window_start.format("GLM-L2-LCFA/%Y/%j/%H/").to_string();
    assert!(
        paths.iter().any(|p| p.contains(&current)),
        "a live pane must still list {current}; it asked for {paths:?}",
    );
    assert!(
        paths
            .iter()
            .all(|p| p.contains(&current) || p.contains(&opened_in)),
        "the live poll listed an hour its own window does not cover — it \
         covers {opened_in} and {current}; it asked for {paths:?}",
    );
    assert!(
        !paths.iter().any(|p| p.contains(ARCHIVE_PREFIX)),
        "a live pane reached into the archive: {paths:?}",
    );

    let times: Vec<NaiveDateTime> = outcome.flashes.iter().map(|f| f.time).collect();
    assert_eq!(
        times,
        vec![now - TimeDelta::seconds(10)],
        "a live pane's window is unchanged: the 10 s flash is in it and the \
         2020 granule is not",
    );
    assert!(
        !cache.contains_key(ARCHIVE_GRANULE),
        "a live poll must still evict what has aged out, or the cache grows \
         without bound",
    );
    assert!(outcome.window_gaps.is_empty());
    assert!(outcome.dead_feeds.is_empty());
}

/// The handler seam, end to end: what a pane depicts reaches the bucket.
///
/// `create_fetch_tasks` is where the depicted instant crosses from the render
/// context into the poll, and the task it returns is an opaque future - so this
/// runs the future against the loopback bucket and reads the prefix off the
/// wire rather than off an intermediate value.
#[test]
fn the_handlers_fetch_task_carries_the_depicted_instant_to_the_bucket() {
    use crate::render::overlay_state::{FetchConfig, OverlayHandler, PaneRef};

    let (sources, seen) = s3_recording(vec![
        (
            "east",
            http_response("200 OK", &listing_xml(ARCHIVE_GRANULE)),
        ),
        (
            "west",
            http_response("200 OK", &listing_xml(ARCHIVE_GRANULE)),
        ),
    ]);
    let handler = crate::render::handlers::glm::GlmHandler::new();
    let config = FetchConfig {
        client: loopback_client(),
        zone_cache_dir: None,
        sources,
        viewport: None,
        as_of: archive_instant(),
        depicted_span_secs: None,
        depicted_frames: Vec::new(),
    };

    let mut tasks = handler.create_fetch_tasks(&config, &PaneRef::bare(0));
    assert_eq!(tasks.len(), 1, "GLM builds exactly one poll task");
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("test runtime");
    runtime.block_on(tasks.remove(0).future);

    let paths = request_paths(&seen);
    assert!(
        !paths.is_empty(),
        "the task reached no bucket at all, so nothing below is a measurement",
    );
    assert!(
        paths.iter().all(|p| p.contains(ARCHIVE_PREFIX)),
        "the handler was given a render context depicting 2020-06-15 07:30 UTC \
         and must list {ARCHIVE_PREFIX}; it asked for {paths:?}",
    );
}

// ── The loop hazard: one data slot swept by a moving clock ──────────────────

/// A granule key under the hour prefix of `start`, whose `_s` field is `start`.
fn granule_key(start: NaiveDateTime) -> String {
    format!(
        "GLM-L2-LCFA/{}/OR_GLM-L2-LCFA_G19_s{}0_e{}0_c{}0.nc",
        start.format("%Y/%j/%H"),
        start.format("%Y%j%H%M%S"),
        (start + TimeDelta::seconds(20)).format("%Y%j%H%M%S"),
        (start + TimeDelta::seconds(21)).format("%Y%j%H%M%S"),
    )
}

fn loop_flash_at(lat: f64, lon: f64, time: NaiveDateTime) -> GlmFlash {
    GlmFlash {
        lat,
        lon,
        energy: Some(1.0e-14),
        area: Some(128.0),
        time,
        satellite: GlmSatellite::GoesEast,
        level: GlmDataLevel::Flash,
    }
}

/// Whether the handler's raster at the depicted instant `as_of` lights any
/// pixel inside `bounds` — the whole paint path: `prepare_job` describes the
/// job exactly as the dispatch would, and `rasterize_glm_strikes` is the
/// worker's own body.
fn strikes_visible(
    handler: &crate::render::handlers::glm::GlmHandler,
    as_of: NaiveDateTime,
    bounds: &squallar_geo::GeoBounds,
) -> bool {
    use crate::render::overlay_state::{OverlayHandler, PaneRef, RasterizeContext};
    let job = handler
        .prepare_job(
            &RasterizeContext {
                is_dark: false,
                zoom: 6.0,
                device_scale: 1.0,
                // The wall clock is deliberately NOT the depicted instant:
                // the ages must be measured from `as_of`.
                now: as_of + TimeDelta::hours(30),
                as_of,
                frame: None,
            },
            &PaneRef::bare(0),
        )
        .expect("flashes are resident, so the layer describes a job");
    let input = job
        .downcast_ref::<crate::render::rasterize::GlmStrikesInput>()
        .expect("the GLM row");
    let out = crate::render::rasterize::rasterize_glm_strikes(input, bounds, 64, 64);
    out.rgba.iter().any(|&b| b != 0)
}

/// **The visual E2E's bug: a pane looping frames across more than one hour
/// with GLM on lit strikes on a single frame, mismatched from the frame
/// under it.**
///
/// The loop's clock sweeps `TimeMode::AsOf(frame.valid)`; the poll samples
/// that clock every 20 s; and the fetch anchored BOTH its granule listing and
/// its cache eviction on the one sampled instant. So whichever hour the last
/// poll happened to sample is the only hour whose strikes are resident, and
/// every other frame of the loop draws an empty sky.
///
/// The loop here is two hours of 2-min frames; the poll is taken while the
/// playhead is on the last frame (07:40:30). Each frame must draw the flashes
/// of ITS OWN window: the 06:10:30 frame its 06:10:10 flash, the 07:40:30
/// frame its 07:40:10 flash — and neither frame the other's.
#[test]
fn a_loop_sweeping_two_hours_draws_each_frames_own_strikes_not_the_polls() {
    use crate::render::overlay_state::{FetchConfig, OverlayHandler, PaneRef};

    let day = chrono::NaiveDate::from_ymd_opt(2020, 6, 15).unwrap();
    let frame_early = day.and_hms_opt(6, 10, 30).unwrap();
    let frame_late = day.and_hms_opt(7, 40, 30).unwrap();
    let g_early = granule_key(day.and_hms_opt(6, 10, 0).unwrap());
    let g_late = granule_key(day.and_hms_opt(7, 40, 0).unwrap());
    let flash_early = loop_flash_at(35.0, -97.0, day.and_hms_opt(6, 10, 10).unwrap());
    let flash_late = loop_flash_at(30.0, -85.0, day.and_hms_opt(7, 40, 10).unwrap());

    // Both hours' granules are already parsed and resident, exactly as they
    // are after the loop's earlier polls have seen both hours: what is under
    // test is what a poll KEEPS and what the listing reaches, not the parser.
    let listing = http_response(
        "200 OK",
        &format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\
             <ListBucketResult xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\">\
             <Name>bucket</Name><IsTruncated>false</IsTruncated>\
             <Contents><Key>{g_early}</Key><Size>1</Size></Contents>\
             <Contents><Key>{g_late}</Key><Size>1</Size></Contents>\
             </ListBucketResult>"
        ),
    );
    let (sources, seen) = s3_recording(vec![("east", listing.clone()), ("west", listing)]);

    let mut handler = crate::render::handlers::glm::GlmHandler::new();
    handler.defaults.enabled = true;
    {
        let mut cache = handler.cache.lock().unwrap_or_else(|e| e.into_inner());
        cache.insert(
            g_early.clone(),
            day.and_hms_opt(6, 10, 0).unwrap(),
            vec![flash_early.clone()],
        );
        cache.insert(
            g_late.clone(),
            day.and_hms_opt(7, 40, 0).unwrap(),
            vec![flash_late.clone()],
        );
    }

    // The poll the loop's 20 s cadence lands while the playhead is on the
    // LAST frame — `fetch_config_for_layer` hands it that frame's instant.
    let config = FetchConfig {
        client: loopback_client(),
        zone_cache_dir: None,
        sources,
        viewport: None,
        as_of: frame_late,
        // What `fetch_config_for_layer` hands a pane whose Lookback is two
        // hours: the loop's clock sweeps that span between polls.
        depicted_span_secs: Some(7200),
        depicted_frames: Vec::new(),
    };
    let mut tasks = handler.create_fetch_tasks(&config, &PaneRef::bare(0));
    assert_eq!(tasks.len(), 1, "GLM builds exactly one poll task");
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("test runtime");
    let payload = runtime.block_on(tasks.remove(0).future);
    handler.apply_fetch_result(payload, &PaneRef::bare(0));

    let around_early = squallar_geo::GeoBounds {
        min_lat: 34.0,
        max_lat: 36.0,
        min_lon: -98.0,
        max_lon: -96.0,
    };
    let around_late = squallar_geo::GeoBounds {
        min_lat: 29.0,
        max_lat: 31.0,
        min_lon: -86.0,
        max_lon: -84.0,
    };

    assert!(
        strikes_visible(&handler, frame_early, &around_early),
        "the frame at 06:10:30 draws no strikes although a flash struck at \
         06:10:10, 20 s inside its 300 s window: the poll at 07:40:30 evicted \
         every granule outside its own window, so of the whole two-hour loop \
         only the frames near 07:40:30 light up",
    );
    assert!(
        !strikes_visible(&handler, frame_early, &around_late),
        "the frame at 06:10:30 draws the 07:40:10 strike, which has not \
         happened yet at that instant — retention must widen what is HELD, \
         never what a frame DRAWS",
    );
    assert!(
        strikes_visible(&handler, frame_late, &around_late),
        "the frame at 07:40:30 draws no strikes although a flash struck at \
         07:40:10, 20 s inside its 300 s window",
    );
    assert!(
        !strikes_visible(&handler, frame_late, &around_early),
        "the frame at 07:40:30 draws the 06:10:10 strike, 5420 s outside its \
         300 s window — the fade ramp must stay relative to the depicted \
         instant",
    );

    // The listing must have reached the EARLY frame's hour, not only the
    // poll's own: the loop's span is two hours, and a listing pinned to the
    // sampled instant is what leaves every other hour's sky empty.
    let paths = request_paths(&seen);
    assert!(
        paths.iter().any(|p| p.contains("GLM-L2-LCFA/2020/167/06/")),
        "the poll at 07:40:30 never listed the 06Z hour the loop's early \
         frames depict — the fetch follows one sampled instant, so the loop \
         lights up only around the frame the last poll happened to land on; \
         it asked for {paths:?}",
    );

    // ── The other direction: the poll lands while the playhead is EARLY ──
    //
    // Every remaining frame of the loop is then *ahead* of the sampled
    // instant, and a delivered set bounded above by that instant leaves all
    // of them dark until the playhead catches up — the same single-frame
    // symptom, mirrored. `horizon` is what carries them, and the frame's own
    // cull is still what decides that the early frame does not draw the late
    // strike.
    let config_early = FetchConfig {
        as_of: frame_early,
        ..config
    };
    let mut tasks = handler.create_fetch_tasks(&config_early, &PaneRef::bare(0));
    assert_eq!(tasks.len(), 1, "GLM builds exactly one poll task");
    let payload = runtime.block_on(tasks.remove(0).future);
    handler.apply_fetch_result(payload, &PaneRef::bare(0));

    assert!(
        strikes_visible(&handler, frame_late, &around_late),
        "the poll sampled 06:10:30 and the 07:40:30 frame went dark: flashes \
         later than the sampled instant are still inside the loop, and a \
         delivery cut off at the sample blanks every frame ahead of the \
         playhead until it arrives there",
    );
    assert!(
        strikes_visible(&handler, frame_early, &around_early),
        "the sampled frame lost its own strike",
    );
    assert!(
        !strikes_visible(&handler, frame_early, &around_late),
        "widening what is HELD must never widen what a frame DRAWS: the \
         06:10:30 frame drew a strike from 07:40:10",
    );
}

/// **The retention bound, with its denominator.**
///
/// Span retention is what fixed the single-frame loop, and unbounded it is a
/// new defect: with all three levels on, [`RecordDrops`]'s measurement is
/// 1584507 records over 105 granules — ~15k rows per 20 s granule — so a span
/// alone could hold a day of storm in one `Arc<Mutex>`.
/// [`MAX_RETAINED_FLASHES`] caps the count and
/// [`GlmCache::evict_oldest_over`] enforces it **oldest granule first**, so an
/// overflowing loop keeps its newest hours lit rather than its oldest.
///
/// The bound is a *count*; the byte figure the failure prints is that count
/// times `size_of::<GlmFlash>()`, which is the whole per-flash cost —
/// `GlmFlash` owns nothing on the heap, so there is no second term.
///
/// **Floor — `retain_everything_forever`: delete the `evict_oldest_over` call
/// from [`fetch_glm_flashes`]** (or widen the cap to `usize::MAX`). The count
/// assertion then reads the 320000 seeded flashes against a 250000 cap.
#[test]
fn a_spanned_poll_caps_what_it_retains_and_drops_its_oldest_hours_first() {
    const PER_GRANULE: usize = 80_000;
    let day = chrono::NaiveDate::from_ymd_opt(2020, 6, 15).unwrap();
    let as_of = day.and_hms_opt(8, 0, 0).unwrap();
    let span_secs: u64 = 7200;

    // Four granules inside the span, oldest first. The cap sits between three
    // granules' worth (240000) and four (320000), so exactly one must go and
    // *which* one is the whole claim.
    let starts = [
        day.and_hms_opt(6, 10, 0).unwrap(),
        day.and_hms_opt(6, 40, 0).unwrap(),
        day.and_hms_opt(7, 10, 0).unwrap(),
        day.and_hms_opt(7, 40, 0).unwrap(),
    ];
    let keys: Vec<String> = starts.iter().copied().map(granule_key).collect();

    let mut cache = GlmCache::default();
    for (key, start) in keys.iter().zip(starts) {
        let flashes: Vec<GlmFlash> = (0..PER_GRANULE)
            .map(|i| loop_flash_at(35.0, -97.0, start + TimeDelta::milliseconds(i as i64)))
            .collect();
        cache.insert(key.clone(), start, flashes);
    }

    let seeded = cache.flash_count();
    assert_eq!(
        seeded,
        PER_GRANULE * starts.len(),
        "premise: the seed is what the arithmetic below assumes",
    );
    assert!(
        seeded > MAX_RETAINED_FLASHES,
        "non-triviality floor: a seed at or under the cap makes every \
         assertion below pass without eviction ever running. Seeded {seeded}, \
         cap {MAX_RETAINED_FLASHES}",
    );

    // The listing names a granule the cache already holds, so nothing is
    // downloaded and what is measured is retention alone.
    let listing = http_response("200 OK", &listing_xml(&keys[0]));
    let sources = s3_serving(vec![("east", listing.clone()), ("west", listing)]);

    let outcome =
        poll_spanned(&sources, &mut cache, as_of, Some(span_secs)).expect("both listings answered");

    let kept = cache.flash_count();
    assert!(
        kept <= MAX_RETAINED_FLASHES,
        "a spanned poll retained {kept} flashes against a cap of \
         {MAX_RETAINED_FLASHES} — {} bytes at size_of::<GlmFlash>() = {}, \
         which is the bound this cache is allowed to cost per pane",
        MAX_RETAINED_FLASHES * std::mem::size_of::<GlmFlash>(),
        std::mem::size_of::<GlmFlash>(),
    );
    assert_eq!(
        kept,
        PER_GRANULE * 3,
        "eviction is whole-granule: a half-kept granule still answers \
         `contains_key`, so the download planner would never refetch it",
    );
    assert!(
        !cache.contains_key(&keys[0]),
        "the OLDEST granule survived the cap. Oldest-first is what keeps an \
         overflowing loop's newest hours lit rather than its oldest",
    );
    for key in &keys[1..] {
        assert!(
            cache.contains_key(key),
            "a granule newer than the evicted one was dropped too: {key}",
        );
    }
    assert_eq!(
        outcome.flashes.len(),
        PER_GRANULE * 3,
        "the delivered set is what survived the cap, not what was seeded",
    );
}

// ── The listing's own reach: a poll covers the span in BOTH directions ──────

/// A mock bucket that answers **per prefix and per object**: a `list-type=2`
/// request returns exactly the seeded keys under the prefix it was asked for,
/// and an object request returns that granule's own bytes.
///
/// [`s3_recording`] cannot stand in for this. It answers *every* prefix with
/// one identical canned listing, so a listing that never reached an hour and
/// one that did come back identical, and it serves no objects at all, so
/// nothing downstream of the listing runs. Every test below turns on what a
/// poll actually *downloads*, which needs both.
///
/// One granule set for both buckets: the callers here query one satellite, and
/// which satellite a key belongs to is not what is under test.
fn s3_archive(granules: Vec<(String, Vec<u8>)>) -> (DataSources, RecordedRequests) {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let port = listener.local_addr().expect("local addr").port();
    let seen: RecordedRequests = Default::default();
    let recorder = std::sync::Arc::clone(&seen);
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { break };
            let mut scratch = [0u8; 8192];
            let read = stream.read(&mut scratch).unwrap_or(0);
            let request = String::from_utf8_lossy(&scratch[..read]).to_string();
            let line = request.lines().next().unwrap_or("").to_string();
            recorder
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(line.clone());
            let _ = stream.write_all(&archive_reply(&line, &granules));
            let _ = stream.flush();
        }
    });
    let sources = DataSources {
        goes_east_bucket: "east".into(),
        goes_west_bucket: "west".into(),
        s3_base: format!("http://127.0.0.1:{port}/{{bucket}}").into(),
        ..DataSources::production()
    };
    (sources, seen)
}

/// The one route table [`s3_archive`] serves: a request line carrying
/// `prefix=` is a listing, anything else addresses an object.
fn archive_reply(line: &str, granules: &[(String, Vec<u8>)]) -> Vec<u8> {
    let path = line.split_whitespace().nth(1).unwrap_or("");
    if let Some(rest) = path.split("prefix=").nth(1) {
        let prefix = rest.split('&').next().unwrap_or("");
        let mut body = String::from(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\
             <ListBucketResult xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\">\
             <Name>bucket</Name><IsTruncated>false</IsTruncated>",
        );
        for key in granules
            .iter()
            .map(|(k, _)| k)
            .filter(|k| k.starts_with(prefix))
        {
            body.push_str(&format!(
                "<Contents><Key>{key}</Key><Size>1</Size></Contents>"
            ));
        }
        body.push_str("</ListBucketResult>");
        return http_response("200 OK", &body).into_bytes();
    }
    match granules.iter().find(|(k, _)| path.ends_with(k.as_str())) {
        Some((_, bytes)) => {
            let mut out = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\n\
                 Content-Length: {}\r\nConnection: close\r\n\r\n",
                bytes.len(),
            )
            .into_bytes();
            out.extend_from_slice(bytes);
            out
        }
        None => http_response("404 Not Found", "<Error/>").into_bytes(),
    }
}

/// One flash at `lat`/`lon`, 10 s into the granule starting at `start` — the
/// shape [`synthetic_glm_file`] writes, with the epoch and the position under
/// the caller's control so a bucket can carry a distinguishable granule per
/// frame. No `units` on the time offset, so it unpacks as seconds since
/// `time_coverage_start`.
fn one_flash_granule(start: NaiveDateTime, lat: f32, lon: f32) -> Vec<u8> {
    let mut file = hdf5_pure::FileBuilder::new();
    file.set_attr(
        "time_coverage_start",
        hdf5_pure::AttrValue::String(format!("{}Z", start.format("%Y-%m-%dT%H:%M:%S.0"))),
    );
    {
        let mut put = |name: &str, value: f32, units: Option<&str>| {
            let var = file.create_dataset(name);
            var.with_f32_data(&[value]);
            if let Some(u) = units {
                var.set_attr("units", hdf5_pure::AttrValue::String(u.into()));
            }
        };
        put("flash_lat", lat, None);
        put("flash_lon", lon, None);
        put("flash_energy", 1.0e-14, Some("J"));
        put("flash_area", 128.0, Some("km2"));
        put("flash_time_offset_of_first_event", 10.0, None);
    }
    file.finish().expect("write granule")
}

/// **The loop the user was watching**: the Lookback slider's default span
/// (`PaneTimePosture::default().span_secs`), at the GLM window's own default
/// (`GlmPaneState::new`), stepped so consecutive frames never share a flash.
const SWEEP_SPAN_SECS: u64 = 3600;
const SWEEP_WINDOW_SECS: f64 = 300.0;
const SWEEP_STEP_SECS: i64 = 300;
/// Both endpoints included: `3600 / 300 + 1`.
const SWEEP_FRAMES: usize = 13;

/// The loop's frames, oldest first. `frames[0]` is the oldest the pane can
/// depict and `frames[SWEEP_FRAMES - 1]` the newest; a poll's `as_of` is one
/// sample of a clock sweeping between them.
fn sweep_frames() -> Vec<NaiveDateTime> {
    let newest = chrono::NaiveDate::from_ymd_opt(2020, 6, 15)
        .unwrap()
        .and_hms_opt(8, 0, 0)
        .unwrap();
    (0..SWEEP_FRAMES)
        .map(|i| newest - TimeDelta::seconds(SWEEP_STEP_SECS * (SWEEP_FRAMES - 1 - i) as i64))
        .collect()
}

/// Frame `i`'s own granule starts 30 s before it, so its single flash lands
/// 20 s inside that frame's 300 s window — and 320 s outside the previous
/// frame's, which is what makes "frame `i` is lit" mean "granule `i` arrived".
fn sweep_granule_start(frame: NaiveDateTime) -> NaiveDateTime {
    frame - TimeDelta::seconds(30)
}

/// Frame `i`'s flash sits at a longitude no other frame's box contains, so a
/// lit box is that frame's own strike and never a neighbour's.
fn sweep_lon(i: usize) -> f32 {
    -110.0 + 4.0 * i as f32
}

fn sweep_bounds(i: usize) -> squallar_geo::GeoBounds {
    squallar_geo::GeoBounds {
        min_lat: 34.0,
        max_lat: 36.0,
        min_lon: sweep_lon(i) as f64 - 1.0,
        max_lon: sweep_lon(i) as f64 + 1.0,
    }
}

/// One granule per frame, and nothing else in the bucket.
fn sweep_bucket() -> Vec<(String, Vec<u8>)> {
    sweep_frames()
        .into_iter()
        .enumerate()
        .map(|(i, frame)| {
            let start = sweep_granule_start(frame);
            (
                granule_key(start),
                one_flash_granule(start, 35.0, sweep_lon(i)),
            )
        })
        .collect()
}

/// A handler on one satellite and the flash level alone: two satellites would
/// double every request without changing what is under test, and the group
/// level has no columns in [`one_flash_granule`].
fn sweep_handler() -> crate::render::handlers::glm::GlmHandler {
    let mut handler = crate::render::handlers::glm::GlmHandler::new();
    handler.defaults.enabled = true;
    handler.defaults.satellite = crate::render::handlers::glm::SatelliteSelection::East;
    handler.defaults.show_groups = false;
    handler.defaults.show_flashes = true;
    handler.defaults.time_window_secs = SWEEP_WINDOW_SECS;
    handler
}

/// One poll of the loop, dispatched exactly as `fetch_config_for_layer` would
/// while the pane's clock reads `as_of`.
fn sweep_poll(
    handler: &mut crate::render::handlers::glm::GlmHandler,
    sources: &DataSources,
    as_of: NaiveDateTime,
) {
    use crate::render::overlay_state::{FetchConfig, OverlayHandler, PaneRef};
    let config = FetchConfig {
        client: loopback_client(),
        zone_cache_dir: None,
        sources: sources.clone(),
        viewport: None,
        as_of,
        depicted_span_secs: Some(SWEEP_SPAN_SECS),
        depicted_frames: Vec::new(),
    };
    let mut tasks = handler.create_fetch_tasks(&config, &PaneRef::bare(0));
    assert_eq!(tasks.len(), 1, "GLM builds exactly one poll task");
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("test runtime");
    let payload = runtime.block_on(tasks.remove(0).future);
    handler.apply_fetch_result(payload, &PaneRef::bare(0));
}

/// Whether the handler's raster at `as_of` lights any pixel inside `bounds` —
/// [`strikes_visible`]'s body, minus its `expect`: a round that delivered
/// nothing describes no job, and "nothing to draw" is the blank frame this
/// suite counts, not a panic.
fn frame_lit(
    handler: &crate::render::handlers::glm::GlmHandler,
    as_of: NaiveDateTime,
    bounds: &squallar_geo::GeoBounds,
) -> bool {
    use crate::render::overlay_state::{OverlayHandler, PaneRef, RasterizeContext};
    let Some(job) = handler.prepare_job(
        &RasterizeContext {
            is_dark: false,
            zoom: 6.0,
            device_scale: 1.0,
            now: as_of + TimeDelta::hours(30),
            as_of,
            frame: None,
        },
        &PaneRef::bare(0),
    ) else {
        return false;
    };
    let input = job
        .downcast_ref::<crate::render::rasterize::GlmStrikesInput>()
        .expect("the GLM row");
    let out = crate::render::rasterize::rasterize_glm_strikes(input, bounds, 64, 64);
    out.rgba.iter().any(|&b| b != 0)
}

/// Which frames of the sweep drew their own strike — the count the user's
/// report is about, one row per frame the loop asks for.
fn lit_frames(handler: &crate::render::handlers::glm::GlmHandler) -> Vec<usize> {
    sweep_frames()
        .into_iter()
        .enumerate()
        .filter(|&(i, frame)| frame_lit(handler, frame, &sweep_bounds(i)))
        .map(|(i, _)| i)
        .collect()
}

/// The listing requests of a recorded round, in arrival order.
fn list_paths(seen: &RecordedRequests) -> Vec<String> {
    request_paths(seen)
        .into_iter()
        .filter(|p| p.contains("list-type=2"))
        .collect()
}

/// **The user's second report, as a test.** *"GLM flashes still don't show up
/// properly across a loop — just a single frame at the beginning (or end, hard
/// to tell) has them, the rest are blank."*
///
/// The span-aware poll widened `start` and the retention cutoff, but the
/// listing still ENDED at the sampled instant. So a poll landing while the
/// playhead sits on the loop's oldest frame lists
/// `[oldest - window - span, oldest]` — a range entirely *behind* the loop —
/// and downloads exactly the one granule the sample itself sits in. Retention
/// cannot rescue that: there is nothing held to retain.
///
/// Nor does the loop heal itself between polls. `GlmHandler::auto_poll_interval`
/// is 20 s; a loop at the default 5 fps sweeps these 13 frames in 2.6 s, so the
/// playhead crosses the whole span seven times per poll and the sky it shows is
/// whichever single instant the last poll happened to sample.
///
/// **Floor — `list_to_the_sample`: put `as_of` back as `list_glm_files`'s
/// `end` argument in [`fetch_glm_flashes`].** The count assertion then reads
/// 1 of 13.
#[test]
fn a_poll_landing_on_the_loops_oldest_frame_still_fills_the_whole_sweep() {
    let (sources, seen) = s3_archive(sweep_bucket());
    let mut handler = sweep_handler();
    let frames = sweep_frames();

    sweep_poll(&mut handler, &sources, frames[0]);

    let lit = lit_frames(&handler);
    assert_eq!(
        lit.len(),
        SWEEP_FRAMES,
        "GLM flashes still don't show up properly across a loop — just a \
         single frame at the beginning (or end, hard to tell) has them, the \
         rest are blank: {} of {SWEEP_FRAMES} frames drew strikes. Lit frames \
         were {lit:?} of 0..{SWEEP_FRAMES}; the poll sampled the loop's \
         OLDEST frame ({}), and a listing bounded above by the sample reaches \
         no granule any later frame depicts.",
        lit.len(),
        frames[0],
    );

    // Non-triviality: "fetch the whole span" must not become "every frame
    // draws every strike". The newest frame's flash is 3600 s after the
    // oldest frame's 300 s window closes.
    assert!(
        !frame_lit(&handler, frames[0], &sweep_bounds(SWEEP_FRAMES - 1)),
        "the oldest frame drew the newest frame's strike, which has not \
         happened yet at that instant — widening what is HELD must never \
         widen what a frame DRAWS",
    );
    assert!(
        !frame_lit(&handler, frames[SWEEP_FRAMES - 1], &sweep_bounds(0)),
        "the newest frame drew the oldest frame's strike, 3600 s outside its \
         300 s window",
    );

    // The hours the sweep needs, and no more: `[oldest - 300 - 3600,
    // oldest + 3600]` is 05:55–08:00Z on 2020-06-15, four UTC hours.
    let paths = list_paths(&seen);
    assert_eq!(
        paths.len(),
        4,
        "a poll at {} over a {SWEEP_SPAN_SECS}s span and a \
         {SWEEP_WINDOW_SECS}s window covers 05:55–08:00Z = 4 UTC hours, so 4 \
         LIST requests per satellite; it made {}: {paths:?}",
        frames[0],
        paths.len(),
    );
    for hour in [
        "/2020/167/05/",
        "/2020/167/06/",
        "/2020/167/07/",
        "/2020/167/08/",
    ] {
        assert!(
            paths.iter().any(|p| p.contains(hour)),
            "the listing never reached {hour}; it asked for {paths:?}",
        );
    }
}

/// The mirror, and the direction that already worked: the poll lands while the
/// playhead is on the loop's NEWEST frame. Every other frame is then *behind*
/// the sample, which `start = as_of - window - span` already reached — so this
/// is a pin on what must not regress while the listing's upper bound moves,
/// not a second reading of the same defect.
///
/// **Floor — `list_from_the_sample`: drop the `- span` term from `start` in
/// [`fetch_glm_flashes`].** The count assertion then reads 1 of 13.
#[test]
fn a_poll_landing_on_the_loops_newest_frame_still_fills_the_whole_sweep() {
    let (sources, seen) = s3_archive(sweep_bucket());
    let mut handler = sweep_handler();
    let frames = sweep_frames();

    sweep_poll(&mut handler, &sources, frames[SWEEP_FRAMES - 1]);

    let lit = lit_frames(&handler);
    assert_eq!(
        lit.len(),
        SWEEP_FRAMES,
        "the poll sampled the loop's NEWEST frame ({}) and only {} of \
         {SWEEP_FRAMES} frames drew strikes — lit frames {lit:?} of \
         0..{SWEEP_FRAMES}. Reaching BACK over the span is the half that \
         already worked; a loop lit at one end is the user's report mirrored.",
        frames[SWEEP_FRAMES - 1],
        lit.len(),
    );

    // `[newest - 300 - 3600, newest + 3600]` is 06:55–09:00Z: four UTC hours
    // again, shifted by one. The count is a property of the span, not of
    // where inside it the poll landed.
    let paths = list_paths(&seen);
    assert_eq!(
        paths.len(),
        4,
        "06:55–09:00Z is 4 UTC hours, so 4 LIST requests per satellite; it \
         made {}: {paths:?}",
        paths.len(),
    );
}

// ── A loop wider than the slider that named it ───────────────────────────

/// **The satellite-shaped loop**, and the shape the sweep above is not: a
/// layer declaring `min_loop_frames() = 13` at an hourly step is listed over
/// **twelve hours** while the Lookback slider still reads one, so a poll told
/// only [`SWEEP_SPAN_SECS`] reaches one frame of the thirteen.
const WIDE_STEP_SECS: i64 = 3600;

/// The thirteen instants such a loop can stop on, oldest first.
fn wide_frames() -> Vec<NaiveDateTime> {
    let newest = chrono::NaiveDate::from_ymd_opt(2020, 6, 15)
        .unwrap()
        .and_hms_opt(12, 0, 0)
        .unwrap();
    (0..SWEEP_FRAMES)
        .map(|i| newest - TimeDelta::seconds(WIDE_STEP_SECS * (SWEEP_FRAMES - 1 - i) as i64))
        .collect()
}

/// Frame `i`'s own granule, plus a **decoy** half an hour later — inside the
/// loop's twelve-hour extent and inside no frame's 300 s window. The decoy is
/// what tells "asked for the loop's windows" apart from "asked for the loop's
/// extent, object by object": both light every frame, and only one of them is
/// affordable.
fn wide_bucket() -> Vec<(String, Vec<u8>)> {
    let mut objects = Vec::new();
    for (i, frame) in wide_frames().into_iter().enumerate() {
        let start = sweep_granule_start(frame);
        objects.push((
            granule_key(start),
            one_flash_granule(start, 35.0, sweep_lon(i)),
        ));
        let decoy = frame + TimeDelta::seconds(WIDE_STEP_SECS / 2);
        objects.push((
            granule_key(decoy),
            one_flash_granule(decoy, 35.0, sweep_lon(i)),
        ));
    }
    objects
}

/// Every decoy key in [`wide_bucket`] — the granules a round that asked for
/// the extent would have downloaded and a round that asked for the windows
/// must not.
fn wide_decoy_keys() -> Vec<String> {
    wide_frames()
        .into_iter()
        .map(|frame| granule_key(frame + TimeDelta::seconds(WIDE_STEP_SECS / 2)))
        .collect()
}

/// One poll of such a loop: the Lookback slider's span, and the frames the
/// pane can actually stop on beside it.
fn wide_poll(
    handler: &mut crate::render::handlers::glm::GlmHandler,
    sources: &DataSources,
    as_of: NaiveDateTime,
) {
    use crate::render::overlay_state::{FetchConfig, OverlayHandler, PaneRef};
    let config = FetchConfig {
        client: loopback_client(),
        zone_cache_dir: None,
        sources: sources.clone(),
        viewport: None,
        as_of,
        depicted_span_secs: Some(SWEEP_SPAN_SECS),
        depicted_frames: wide_frames(),
    };
    let mut tasks = handler.create_fetch_tasks(&config, &PaneRef::bare(0));
    assert_eq!(tasks.len(), 1, "GLM builds exactly one poll task");
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("test runtime");
    let payload = runtime.block_on(tasks.remove(0).future);
    handler.apply_fetch_result(payload, &PaneRef::bare(0));
}

/// **A loop wider than the span it was told is lit by its frames, and costs
/// its frames.**
///
/// The sweep above holds where the loop's window and the Lookback setting are
/// the same number. A satellite loop's are not, and the span alone then
/// reaches one frame of thirteen — the user's *"only works on the first frame
/// of a loop"*. Naming the depicted instants is what closes it without asking
/// the archive for the whole twelve hours: **thirteen 300 s windows, not 24
/// hours of 20-second granules.**
///
/// **Floor — `windows_are_the_extent`: make `DepictedWindow::covers` return
/// `true` unconditionally.** Every decoy is downloaded and the second
/// assertion reads 13.
#[test]
fn a_loop_wider_than_its_span_is_lit_by_the_frames_it_names() {
    let (sources, seen) = s3_archive(wide_bucket());
    let mut handler = sweep_handler();
    let frames = wide_frames();

    wide_poll(&mut handler, &sources, frames[0]);

    let lit: Vec<usize> = frames
        .iter()
        .enumerate()
        .filter(|&(i, frame)| frame_lit(&handler, *frame, &sweep_bounds(i)))
        .map(|(i, _)| i)
        .collect();
    assert_eq!(
        lit.len(),
        SWEEP_FRAMES,
        "GLM still only works on the first frame of a loop: {} of \
         {SWEEP_FRAMES} frames drew strikes, lit frames {lit:?}. The loop is \
         twelve hours wide and the span it was told is {SWEEP_SPAN_SECS}s, so \
         the instants it names are the only thing that can reach the other \
         frames.",
        lit.len(),
    );

    // The cost claim, and the reason the fix is not "hand it the twelve
    // hours": a granule inside the loop's extent but inside no frame's window
    // is listed and left alone.
    let asked = request_paths(&seen);
    let decoys = wide_decoy_keys();
    let downloaded = decoys
        .iter()
        .filter(|key| asked.iter().any(|line| line.contains(key.as_str())))
        .count();
    assert_eq!(
        downloaded,
        0,
        "{downloaded} of {} granules inside the loop's extent but inside no \
         frame's window were downloaded. A round that asks for the extent \
         object by object asks for 24 hours of 20-second granules.",
        decoys.len(),
    );

    // Non-triviality: the decoys have to be reachable, or "none downloaded" is
    // a statement about an empty bucket.
    assert_eq!(
        decoys.len(),
        SWEEP_FRAMES,
        "fixture: one decoy per frame, or the count above proves nothing",
    );
    assert!(
        !frame_lit(
            &handler,
            frames[0] + TimeDelta::seconds(WIDE_STEP_SECS / 2),
            &sweep_bounds(0)
        ),
        "fixture: the decoy's own instant draws nothing, so it really was \
         never downloaded rather than downloaded and culled",
    );
}

/// **A live pane is byte-for-byte the pane it always was.** `span_secs: None`
/// makes `span` zero, so `horizon == as_of` and the listing's upper bound is
/// the sampled instant exactly as before this fix.
///
/// The bucket carries a granule in the NEXT hour, 40 minutes after the live
/// pane's instant. A live poll must not list that hour, must not download it,
/// and must not return its flash — the guard against "just widen everything",
/// which is the cheapest wrong fix here.
///
/// **Floor — `every_pane_is_a_span`: make `span` default to the loop span
/// rather than zero (`depicted_span_secs.unwrap_or(3600)`) in
/// [`fetch_glm_flashes`].** Observed: the LIST count reads 3, not 1, and the
/// hours it asked for are 06Z, 07Z and 08Z.
#[test]
fn a_live_polls_listing_range_and_returned_set_are_unchanged() {
    let day = chrono::NaiveDate::from_ymd_opt(2020, 6, 15).unwrap();
    let as_of = day.and_hms_opt(7, 30, 0).unwrap();
    let in_window = day.and_hms_opt(7, 29, 30).unwrap();
    let next_hour = day.and_hms_opt(8, 10, 0).unwrap();

    let (sources, seen) = s3_archive(vec![
        (
            granule_key(in_window),
            one_flash_granule(in_window, 35.0, -97.0),
        ),
        (
            granule_key(next_hour),
            one_flash_granule(next_hour, 30.0, -85.0),
        ),
    ]);

    let mut cache = GlmCache::default();
    let client = loopback_client();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("test runtime");
    let outcome = runtime
        .block_on(fetch_glm_flashes(
            &client,
            &sources,
            &[GlmSatellite::GoesEast],
            &[GlmDataLevel::Flash],
            &mut cache,
            as_of,
            span_residency(as_of, None, SWEEP_WINDOW_SECS),
        ))
        .expect("the listing answered");

    let paths = list_paths(&seen);
    assert_eq!(
        paths.len(),
        1,
        "a live poll's range is `[as_of - window, as_of]` = 07:25–07:30Z, one \
         UTC hour, so one LIST request per satellite; it made {}: {paths:?}",
        paths.len(),
    );
    assert!(
        paths[0].contains("/2020/167/07/"),
        "the live hour is 07Z: {paths:?}",
    );
    assert!(
        !paths.iter().any(|p| p.contains("/2020/167/08/")),
        "a live pane listed an hour AHEAD of its own instant: {paths:?}",
    );

    let times: Vec<NaiveDateTime> = outcome.flashes.iter().map(|f| f.time).collect();
    assert_eq!(
        times,
        vec![in_window + TimeDelta::seconds(10)],
        "a live poll's returned set is the flashes inside `[as_of - window, \
         as_of]` and nothing else",
    );
    let downloads: Vec<String> = request_paths(&seen)
        .into_iter()
        .filter(|p| p.contains(".nc"))
        .collect();
    assert_eq!(
        downloads.len(),
        1,
        "a live poll downloaded a granule outside its own window: \
         {downloads:?}",
    );
}

/// **What one poll costs in LIST requests, with its denominator.**
///
/// The listing is addressed by `{year}/{doy}/{hour}`, so the cost is the count
/// of distinct UTC hours `[as_of - window - span, as_of + span]` touches — a
/// *listing* count, not a download count: [`plan_downloads`] still gates every
/// GET against the cache, so a wider listing re-downloads nothing already
/// held.
///
/// Per satellite, per 20 s poll (`GlmHandler::auto_poll_interval`):
///
/// | posture | span | window | hours | before this fix |
/// |---|---|---|---|---|
/// | default Lookback, mid-hour | 3600 | 300 | 3 | 2 |
/// | default Lookback, just past the hour | 3600 | 300 | 4 | 3 |
/// | widest Lookback (24 h) at the widest window | 86400 | 1800 | 50 | 26 |
/// | live | — | 300 | 1–2 | 1–2 |
///
/// Both satellites doubles every row: 6–8 LIST per poll at the default
/// posture, which at one poll per 20 s is 0.3–0.4 LIST/s.
///
/// **Floor — `horizon_beyond_the_span`: widen the listing's upper bound past
/// the span (`let horizon = as_of + span + span;`) in [`fetch_glm_flashes`].**
/// Observed: every span row reads one hour higher (3→4, 4→5, 50→74) while both
/// live rows hold, which is what makes this table a cost bound rather than a
/// transcript.
#[test]
fn one_polls_listing_cost_is_the_hours_the_span_touches() {
    let day = chrono::NaiveDate::from_ymd_opt(2020, 6, 15).unwrap();
    let mid_hour = day.and_hms_opt(7, 40, 0).unwrap();
    let past_the_hour = day.and_hms_opt(7, 0, 30).unwrap();

    let mut measured: Vec<(&str, usize, usize)> = Vec::new();
    for (label, as_of, span, window, expected) in [
        (
            "default Lookback, mid-hour: 06:35–08:40Z",
            mid_hour,
            Some(SWEEP_SPAN_SECS),
            SWEEP_WINDOW_SECS,
            3usize,
        ),
        (
            "default Lookback, just past the hour: 05:55:30–08:00:30Z",
            past_the_hour,
            Some(SWEEP_SPAN_SECS),
            SWEEP_WINDOW_SECS,
            4,
        ),
        (
            "widest Lookback at the widest window: 06-14 11:30Z – 06-16 12:00Z",
            day.and_hms_opt(12, 0, 0).unwrap(),
            Some(86_400),
            crate::glm::GLM_MAX_TIME_WINDOW_SECS,
            50,
        ),
        ("live, mid-hour", mid_hour, None, SWEEP_WINDOW_SECS, 1),
        (
            "live, just past the hour",
            past_the_hour,
            None,
            SWEEP_WINDOW_SECS,
            2,
        ),
    ] {
        measured.push((label, lists_issued(as_of, span, window), expected));
    }

    // Every row measured before any is asserted: a widening moves the whole
    // table, and a report naming only the first row that moved understates it.
    let table: Vec<String> = measured
        .iter()
        .map(|(label, counted, expected)| format!("  {label}: {counted} (want {expected})"))
        .collect();
    let wrong: Vec<&str> = measured
        .iter()
        .filter(|(_, counted, expected)| counted != expected)
        .map(|(label, _, _)| *label)
        .collect();
    assert!(
        wrong.is_empty(),
        "LIST requests per poll, per ONE satellite — both satellites doubles \
         every row, and the poll repeats every 20 s \
         (`GlmHandler::auto_poll_interval`). {} of {} rows moved \
         ({wrong:?}):\n{}",
        wrong.len(),
        measured.len(),
        table.join("\n"),
    );
}

/// One poll of one satellite against an EMPTY bucket, returning how many LIST
/// requests it issued. Empty because a listing that names no key downloads
/// nothing, so every recorded request is a listing.
fn lists_issued(as_of: NaiveDateTime, span_secs: Option<u64>, window_secs: f64) -> usize {
    let (sources, seen) = s3_archive(Vec::new());
    let mut cache = GlmCache::default();
    let client = loopback_client();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("test runtime");
    runtime
        .block_on(fetch_glm_flashes(
            &client,
            &sources,
            &[GlmSatellite::GoesEast],
            &[GlmDataLevel::Flash],
            &mut cache,
            as_of,
            span_residency(as_of, span_secs, window_secs),
        ))
        .expect("the listing answered");
    list_paths(&seen).len()
}

/// **The forward reach against the real bucket.** Everything above is a mock:
/// nothing else in this suite can catch a prefix the live archive does not
/// publish, or a forward hour S3 answers differently.
///
/// `as_of` is placed five minutes before an hour boundary, so `as_of + span`
/// lands in the NEXT UTC hour — a genuinely different `{year}/{doy}/{hour}`
/// prefix, which only a listing bounded by `horizon` ever asks for. Four hours
/// back, so the archive is settled.
///
/// `cargo test -p squallar-overlays -- --ignored --nocapture live_glm`
#[cfg(not(target_arch = "wasm32"))]
#[tokio::test]
#[ignore = "hits the live noaa-goes19 GLM S3 bucket"]
async fn live_glm_listing_reaches_the_hour_ahead_of_the_sample() {
    use chrono::Timelike;
    const LIVE_SPAN_SECS: u64 = 600;

    let client = squallar_source::tls::client(
        squallar_source::tls::USER_AGENT,
        std::time::Duration::from_secs(120),
    )
    .build()
    .expect("client");
    let sources = DataSources::production();

    let hour = (Utc::now().naive_utc() - TimeDelta::hours(4))
        .with_minute(0)
        .and_then(|t| t.with_second(0))
        .and_then(|t| t.with_nanosecond(0))
        .expect("truncate to the hour");
    let as_of = hour - TimeDelta::seconds(300);
    let horizon = as_of + TimeDelta::seconds(LIVE_SPAN_SECS as i64);
    assert_ne!(
        as_of.hour(),
        horizon.hour(),
        "the premise: {as_of} + {LIVE_SPAN_SECS}s must cross into the next \
         UTC hour ({horizon}), or this proves nothing about the forward \
         prefix",
    );

    let mut cache = GlmCache::default();
    let outcome = fetch_glm_flashes(
        &client,
        &sources,
        &[GlmSatellite::GoesEast],
        &[GlmDataLevel::Flash],
        &mut cache,
        as_of,
        span_residency(as_of, Some(LIVE_SPAN_SECS), GLM_MIN_TIME_WINDOW_SECS),
    )
    .await
    .expect("the live listing answered");

    let before = outcome.flashes.iter().filter(|f| f.time < as_of).count();
    let after = outcome.flashes.iter().filter(|f| f.time > as_of).count();
    let next_hour = outcome.flashes.iter().filter(|f| f.time >= hour).count();
    let seen_span: Option<(NaiveDateTime, NaiveDateTime)> = outcome
        .flashes
        .iter()
        .map(|f| f.time)
        .min()
        .zip(outcome.flashes.iter().map(|f| f.time).max());
    println!(
        "live GLM: as_of {as_of}, horizon {horizon}\n  {} flashes returned: \
         {before} before as_of, {after} after, {next_hour} in the {:02}Z \
         hour\n  observed span {seen_span:?}\n  {} flashes held in cache",
        outcome.flashes.len(),
        hour.hour(),
        cache.flash_count(),
    );

    assert!(
        before > 0,
        "the live poll reached nothing behind the sample, so the window \
         itself is broken and the forward assertion below would prove nothing",
    );
    assert!(
        after > 0,
        "the live poll returned no flash later than the sampled instant, so \
         every frame of a real loop ahead of the playhead is blank",
    );
    assert!(
        next_hour > 0,
        "the live poll never reached the {:02}Z prefix — the hour \
         `as_of + span` lands in. A listing bounded by the sample stops one \
         hour short of the loop's newest frames.",
        hour.hour(),
    );
}

/// **WO-T3.11: the layer's own window is subtracted exactly once** on the way
/// from the stops a pane can make to the objects a poll asks for.
///
/// The defect this closes is a *shape*, not a number. `fetch_glm_flashes` used
/// to be handed `(as_of, span, frames)` and derive `start`/`cutoff`/`horizon`
/// itself, subtracting `time_window_secs` down there — while the app measured
/// the pane's reach up in `depicted_reach_for_layer` and had to use
/// `Residency::extent` rather than `residency_for` **precisely so the window
/// was not subtracted twice**. Two authorities on one loop is what lit a
/// twelve-hour sweep on a single frame, three times.
///
/// There is one subtraction now and it is `residency_for`'s. Everything below
/// is read off that answer:
///
/// * the eviction cutoff is the residency's own oldest instant, carrying **no**
///   [`GRANULE_SPAN`] — residency states which *flashes* must be held;
/// * the listing reaches one `GRANULE_SPAN` further back, because a granule
///   straddling a window's opening carries flashes inside it, and that is a
///   statement about S3 objects which the caller applies itself.
///
/// The `assert_ne!` names the value a second subtraction would produce, so
/// "subtracted once" is a claim this test can fail rather than a restatement
/// of the code.
#[test]
fn the_layers_window_is_subtracted_once_between_the_stops_and_the_listing() {
    use crate::render::overlay_state::PaneRef;
    use squallar_source::handler::SourceHandler;

    let handler = sweep_handler();
    let window = TimeDelta::milliseconds((SWEEP_WINDOW_SECS * 1000.0) as i64);
    let stops = sweep_frames();
    let oldest = stops[0];

    let residency = handler.residency_for(&PaneRef::bare(0), &stops);
    let listed = listed_ranges(&residency);

    assert_eq!(
        residency
            .extent()
            .expect("thirteen stops name a residency")
            .0,
        oldest - window,
        "the residency reaches somewhere other than one window behind its \
         oldest stop, which is the eviction cutoff and the flash filter's own \
         lower bound",
    );
    assert_eq!(
        listed
            .first()
            .expect("a non-empty residency lists ranges")
            .0,
        oldest - window - GRANULE_SPAN,
        "the listing does not reach the granule straddling the oldest \
         window's opening",
    );
    assert_ne!(
        listed
            .first()
            .expect("a non-empty residency lists ranges")
            .0,
        oldest - window - window - GRANULE_SPAN,
        "the window was subtracted twice — once by `residency_for` and once \
         again downstream, which is the two-authorities shape this refactor \
         removed",
    );

    // Non-triviality: the two subtractions have to be distinguishable, so the
    // window must be neither zero nor equal to the granule span.
    assert!(
        window > TimeDelta::zero() && window != GRANULE_SPAN,
        "fixture: a {SWEEP_WINDOW_SECS}s window makes the assertions above \
         indistinguishable from each other",
    );
}
