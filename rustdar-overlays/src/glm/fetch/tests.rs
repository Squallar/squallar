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
    let new_keys = plan_downloads(&keys, &cache, &mut tally);
    assert_eq!(
        tally.in_window, 12,
        "the window still contains every listed file, cached or not"
    );
    assert_eq!(new_keys.len(), 3, "only the uncached ones need downloading");

    let other: Vec<String> = (0..4).map(|i| format!("w{i}.nc")).collect();
    plan_downloads(&other, &GlmCache::default(), &mut tally);
    assert_eq!(tally.in_window, 16);

    let mut cache = GlmCache::default();
    for key in keys.iter().take(11) {
        cache.insert(key.clone(), t0(), Vec::new());
    }
    let mut tally = PollTally::default();
    let new_keys = plan_downloads(&keys, &cache, &mut tally);
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
        plan_downloads(&listing, &cache, &mut tally).len(),
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
            plan_downloads(&listing, &cache, &mut tally).is_empty(),
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
    rustdar_source::tls::init();
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
    rustdar_source::tls::init();
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
    let client = loopback_client();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("test runtime");
    runtime.block_on(fetch_glm_flashes(
        &client,
        sources,
        &[GlmSatellite::GoesEast, GlmSatellite::GoesWest],
        GLM_MIN_TIME_WINDOW_SECS,
        &[GlmDataLevel::Flash],
        cache,
        as_of,
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

    let outcome = poll(&sources, &mut cache).expect("both listings answered");

    let paths = request_paths(&seen);
    let current = now.format("GLM-L2-LCFA/%Y/%j/%H/").to_string();
    assert!(
        paths.iter().all(|p| p.contains(&current)),
        "a live pane must still list {current}; it asked for {paths:?}",
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
