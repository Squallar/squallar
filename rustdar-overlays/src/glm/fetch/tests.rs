use super::*;

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

/// Every flash-level variable, so tests can omit "all of them".
const FLASH_LEVEL_VARS: [&str; 5] = [
    "flash_lat",
    "flash_lon",
    "flash_energy",
    "flash_area",
    "flash_time_offset_of_first_event",
];

/// The error a missing variable must produce, verbatim.
fn absent_variable_error(name: &str) -> String {
    format!("GLM file has no '{name}' variable (product schema change?)")
}

/// The error a short column must produce, verbatim.
fn length_mismatch_error(name: &str, len: usize, reference: &str, count: usize) -> String {
    format!("GLM variable length mismatch: '{name}' has {len} values but '{reference}' has {count}")
}

#[derive(Default)]
struct Fixture<'a> {
    /// Variables to leave out entirely, simulating a schema change.
    omit: &'a [&'a str],
    /// Variable to write against a deliberately shorter dimension,
    /// simulating a corrupt or restructured file.
    short: Option<&'a str>,
}

/// Build a minimal in-memory GLM-shaped NetCDF4 file: flashes carry an
/// area variable, events deliberately do not (mirroring the real product).
///
/// Writes plain unpacked `f32` in canonical units: the subject here is
/// *column presence*, not CF packing, which `glm::tests` covers against
/// packed shorts. `units` is still declared — an undeclared unit is
/// reported as unknown.
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
            // A "short" column is one element instead of two: the length
            // *is* the data here, so there are no dimensions to mismatch.
            let values = if spec.short == Some(name) {
                &values[..1]
            } else {
                values
            };
            let var = file.create_dataset(name);
            var.with_f32_data(values);
            // Only the two unit-converted fields need a `units` attribute.
            let units = match name {
                n if n.ends_with("_area") => Some("km2"),
                n if n.ends_with("_energy") => Some("J"),
                _ => None,
            };
            if let Some(u) = units {
                var.set_attr("units", hdf5_pure::AttrValue::String(u.into()));
            }
        };

        put("flash_lat", &[35.0, 36.0]);
        put("flash_lon", &[-97.0, -98.0]);
        put("flash_energy", &[1.0e-14, 2.0e-14]);
        put("flash_area", &[128.0, 256.0]);
        put("flash_time_offset_of_first_event", &[1.0, 2.0]);

        put("event_lat", &[35.5, 36.5]);
        put("event_lon", &[-97.5, -98.5]);
        put("event_energy", &[3.0e-15, 4.0e-15]);
        put("event_time_offset", &[3.0, 4.0]);
        // Note: no `event_area` — that is the point of the fixture.
    }

    file.finish().expect("write fixture")
}

/// Records only, for tests that do not care about level failures.
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
    // Pins that the right column was read, in order, without pinning the
    // fixture's own scale: area = [128, 256] has a ratio of exactly 2,
    // which lat [35, 36] and lon [-97, -98] do not, and which a pure
    // scaling preserves (`flash_area` has add_offset = 0 in the product).
    // The `> 1.0` floor excludes the ~1e-14 energy column.
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
    // Fails if events fall back to Some(0.0), rendered as "Area: 0.0 km²".
    assert!(
        events.iter().all(|e| e.area.is_none()),
        "events must not report a fabricated area"
    );
    // The rest of the event record must still parse.
    assert!((events[0].lat - 35.5).abs() < 1e-4);
    assert!((events[0].lon - (-97.5)).abs() < 1e-4);
}

/// A required variable disappearing from the product must fail the parse,
/// not quietly yield zeros or an empty result set. Each required variable is
/// omitted alone in turn — energy included, whose zero-default would render
/// as a minimum-size bolt — so the other columns stay equally sized and no
/// length mismatch can mask which gate fired.
///
/// Verbatim, not `contains`: with the required-variable gate removed the
/// downstream length check also errors, and its message interpolates both
/// the offending name and `vars.lat`, so the two gates shadow each other.
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

/// The case only the required-variable gate can catch: with every column
/// equally absent there is no length mismatch to trip the downstream check.
/// Fails if an absent variable reads back as an empty column, which parses
/// cleanly into zero records — a blank map reported as success.
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

/// A separate gate from the required-variable one: a *present but short*
/// variable is corruption, and indexing past it would panic.
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

/// Every per-file error must survive the batch partition, in the right
/// bucket. Fails if errors are discarded into a log line, which is what
/// made a total parse failure read as "Updated 0s ago".
#[test]
fn batch_partition_keeps_every_error_and_separates_the_kinds() {
    let outcome = BatchOutcome::from_results(vec![
        Ok((
            "a.nc".into(),
            GranuleParse {
                records: Vec::new(),
                level_failures: Vec::new(),
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

/// A schema change hits every granule in the window identically, so the
/// same broken level in twenty files is one report — but two *different*
/// broken levels are two, and collapsing them would hide a layer.
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
            },
        )),
        Ok((
            "b.nc".into(),
            GranuleParse {
                records: Vec::new(),
                level_failures: both_broken(),
            },
        )),
        Ok((
            "c.nc".into(),
            GranuleParse {
                records: Vec::new(),
                level_failures: both_broken(),
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

/// Fails if the accumulator drops any bucket — invisible from the async
/// fetch that calls it.
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
        },
    );

    assert_eq!(acc.entries.len(), 1);
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

/// ...but only a granule that actually parsed is evidence: treating a batch
/// where every file failed as evidence announces a recovery on an outage.
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
        },
    );
    assert!(
        acc.evaluated_levels.is_empty(),
        "a batch with no successful parse cannot vouch for any level"
    );
}

/// The failure denominator counts the whole window, not this poll's
/// downloads. Fails if the two are conflated, which makes one corrupt
/// granule read as "1/1 — everything failed" after a few ticks.
#[test]
fn poll_plan_separates_window_size_from_work_to_do() {
    let keys: Vec<String> = (0..12).map(|i| format!("k{i}.nc")).collect();

    // Empty granules: the steady state a quiet sky produces, which must
    // read as "already downloaded".
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

    // The tally accumulates across satellites rather than being overwritten.
    let other: Vec<String> = (0..4).map(|i| format!("w{i}.nc")).collect();
    plan_downloads(&other, &GlmCache::default(), &mut tally);
    assert_eq!(tally.in_window, 16);

    // The pathological steady state: everything cached but one straggler,
    // which is what a 20 s poll against 20 s granules looks like.
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

// ---------------------------------------------------------------------
// Retention: `GlmCache::evict_before` and `flashes_in_window`. Every way
// of getting these wrong renders identically — a quiet sky.
// ---------------------------------------------------------------------

/// An arbitrary but fixed instant to hang the retention tests off, so they
/// never consult the wall clock.
fn t0() -> NaiveDateTime {
    chrono::NaiveDate::from_ymd_opt(2026, 7, 24)
        .unwrap()
        .and_hms_opt(12, 0, 0)
        .unwrap()
}

/// A wall clock that shares no instant with any fixture S3 key.
///
/// `t0()` is 2026-07-24 = day of year 205, the same day the
/// `..._s2026205....nc` fixture keys encode, so passing it as `now` makes
/// "dated from the key" and "dated from the wall clock" indistinguishable.
/// Any fixture feeding a `now` alongside a real key must use this.
fn wall_clock_unlike_keys() -> NaiveDateTime {
    t0() + TimeDelta::hours(3) + TimeDelta::minutes(7)
}

/// A flash whose only interesting property is when it happened.
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

/// Cache keys sorted, so assertions on "what is left" are order-stable.
fn cached_keys(cache: &GlmCache) -> Vec<String> {
    let mut keys: Vec<String> = cache.entries.keys().cloned().collect();
    keys.sort();
    keys
}

/// Cache a granule that parsed to at least one record, dating it the way S3
/// does: a granule is keyed by the *start* of the ~20 s span it covers, so
/// its records land at or after that instant.
///
/// A granule with no records has no start to derive, so those fixtures must
/// state one explicitly through [`GlmCache::insert`].
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

/// Eviction is per *granule*, not per flash. A granule spans ~20 s, so the
/// newest-but-one file straddles the cutoff on essentially every poll;
/// tightening this to "all flashes in window" evicts and re-downloads a
/// live file every tick.
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

/// The two ends of the range, and the degenerate case in between. A no-op
/// eviction grows the cache without bound; a clear-everything eviction
/// re-downloads the whole window every poll.
#[test]
fn evict_before_handles_an_empty_cache_and_both_extremes() {
    // Empty cache: must not panic, must stay empty.
    let mut empty = GlmCache::default();
    empty.evict_before(t0());
    assert_eq!(empty.all_flashes().count(), 0);
    assert!(cached_keys(&empty).is_empty());

    // Everything is stale.
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

    // Nothing is stale.
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

/// A granule that parsed to *zero* records is aged by its own start time.
///
/// Fails if eviction goes back to a predicate over the flashes, which
/// evicts every empty granule immediately: at the 30-minute maximum window
/// that re-fetched roughly 90 granules × ~250 KB ≈ 22 MB every 20 s.
#[test]
fn evict_before_ages_an_empty_granule_by_its_own_start_time() {
    let start = t0();
    let mut cache = GlmCache::default();
    cache.insert("quiet.nc".into(), start, Vec::new());

    // A cutoff far behind the granule: an empty granule inside the window
    // is downloaded data, and must survive exactly like a populated one.
    cache.evict_before(start - TimeDelta::days(365));
    assert!(
        cache.contains_key("quiet.nc"),
        "an empty parse is a successful download; evicting it here is what \
             re-fetched the whole listing window every poll"
    );

    // ...and it is not immortal either: past its own start it goes, on the
    // same schedule a populated granule would.
    cache.evict_before(start + TimeDelta::milliseconds(1));
    assert!(
        !cache.contains_key("quiet.nc"),
        "an empty granule that never expires is the opposite bug: a cache \
             that grows without bound"
    );
}

/// An empty granule ages out on *exactly* the same schedule as a populated
/// one covering the same instant — retention must not be bought by making
/// quiet granules special.
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

/// End to end: cache → evict → plan. Both halves are needed — eviction is
/// what drops the entry and `plan_downloads` is what re-queues it — so
/// pinning either alone leaves the re-fetch loop reachable.
#[test]
fn a_quiet_granule_is_downloaded_once_not_once_per_poll() {
    // A real GLM key, so the granule is dated the way production dates it.
    let key = "GLM-L2-LCFA/2026/205/12/\
                   OR_GLM-L2-LCFA_G19_s20262051200000_e20262051200200_c20262051200214.nc";
    let start = parse_filename_start_time(key).expect("fixture key must be datable");
    let listing = vec![key.to_string()];

    // Poll 1: nothing cached, so it is queued and downloaded. It parses to
    // no records — a quiet 20 s over the ocean.
    let mut cache = GlmCache::default();
    let mut tally = PollTally::default();
    assert_eq!(
        plan_downloads(&listing, &cache, &mut tally).len(),
        1,
        "an uncached granule must be downloaded once"
    );
    // `now` is nowhere near the key's own time — see
    // `wall_clock_unlike_keys`.
    cache.insert(
        key.to_string(),
        granule_start_of(key, wall_clock_unlike_keys()),
        Vec::new(),
    );

    // Polls 2..n, still inside the window: the listing keeps offering it and
    // the cache must keep answering "already have it" (~250 KB per miss).
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

    // Past the cutoff it leaves, like any other granule. It is out of the
    // listing by then too, so nothing re-queues it.
    cache.evict_before(start + TimeDelta::milliseconds(1));
    assert!(
        !cache.contains_key(key),
        "a stale empty granule must be evicted, or the cache never shrinks"
    );
}

/// Every granule a poll parsed reaches the cache, empty ones included:
/// dropping them here makes `plan_downloads` re-queue them every poll.
#[test]
fn cache_granules_keeps_the_empty_ones_too() {
    let busy = "GLM-L2-LCFA/2026/205/12/\
                    OR_GLM-L2-LCFA_G19_s20262051200000_e20262051200200_c20262051200214.nc";
    let quiet = "GLM-L2-LCFA/2026/205/12/\
                     OR_GLM-L2-LCFA_G19_s20262051200200_e20262051200400_c20262051200414.nc";
    // Not `t0()`: see `wall_clock_unlike_keys`.
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

    // And it is dated from its key, not from `now`.
    cache.evict_before(
        parse_filename_start_time(quiet).expect("fixture key") + TimeDelta::milliseconds(1),
    );
    assert!(
        !cache.contains_key(quiet),
        "the empty granule ages by its own start time"
    );
}

/// The fallback in [`granule_start_of`] is unreachable for anything a poll
/// listed, and must stay bounded rather than become either bug it replaced.
#[test]
fn granule_start_comes_from_the_key_and_falls_back_to_now() {
    // Not `t0()`: see `wall_clock_unlike_keys`.
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

/// Both window bounds are inclusive and both are load-bearing: losing the
/// lower one shows hours-old bolts inside a retained granule, losing the
/// upper one publishes flashes stamped after the poll's own instant.
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

/// A flash from the other bird, for the satellite-selection tests.
fn west_flash_at(time: NaiveDateTime) -> GlmFlash {
    GlmFlash {
        satellite: GlmSatellite::GoesWest,
        ..flash_at(time)
    }
}

/// "Both" → "East" must stop GOES-West's cached flashes rendering *now*,
/// not once they age out of the (up to 30-minute) window — and must not
/// cost the cache: re-selecting West restores its flashes instantly, with
/// no re-download.
#[test]
fn deselecting_a_satellite_hides_its_cached_flashes_without_evicting_them() {
    let cutoff = t0();
    let now = cutoff + TimeDelta::minutes(5);
    let t = cutoff + TimeDelta::minutes(2);

    let mut cache = GlmCache::default();
    cache_granule(&mut cache, "east.nc", vec![flash_at(t)]);
    cache_granule(&mut cache, "west.nc", vec![west_flash_at(t)]);

    // Control: with both selected, both birds render.
    let both = [GlmSatellite::GoesEast, GlmSatellite::GoesWest];
    assert_eq!(flashes_in_window(&cache, &both, cutoff, now).len(), 2);

    // "Both" → "East": the in-window West flash disappears from the poll.
    let east_only = flashes_in_window(&cache, &[GlmSatellite::GoesEast], cutoff, now);
    assert!(
        east_only
            .iter()
            .all(|f| f.satellite == GlmSatellite::GoesEast),
        "a deselected bird's cached flashes must not render"
    );
    assert_eq!(east_only.len(), 1, "the East flash still renders");

    // ...but the West granule was hidden, not evicted: re-selection needs
    // nothing from the network.
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

/// An NTP step backwards (or a resume with a stale RTC) puts `now` behind
/// flashes already cached. They must be hidden from the poll and nothing
/// more — still cached, so they reappear without a re-download when the
/// clock recovers.
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

    // The clock steps back: `now` lands between the two cached flashes.
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

    // The clock catches up. Nothing had to be fetched again.
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

/// Every bucket must land in its own field: swapping two makes every 503
/// read as "product change?", dropping the level bucket makes a broken
/// layer silent.
#[test]
fn build_outcome_binds_each_bucket_to_its_own_field() {
    let tally = PollTally { in_window: 12 };
    let acc = PollAccumulator {
        parse_errors: vec!["a.nc: GLM file has no 'flash_lat' variable".into()],
        transport_errors: vec!["b.nc: HTTP status error: 503".into()],
        level_failures: vec![level_failure(GlmSatellite::GoesWest, GlmDataLevel::Flash)],
        // A *superset* of the failures: Group was evaluated and found
        // healthy. Identical sets would let `evaluated_levels` be derived
        // from `level_failures`, giving a layer that can never clear.
        evaluated_levels: vec![
            (GlmSatellite::GoesWest, GlmDataLevel::Flash),
            (GlmSatellite::GoesWest, GlmDataLevel::Group),
        ],
        ..Default::default()
    };
    let outcome = build_outcome(
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

    // Carried through untouched: not summarised into a file count.
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

/// A level failure is not a file failure. Routing it through
/// `summarize_failures` would announce "N/M files failed to parse" while the
/// other layers are still drawing.
#[test]
fn build_outcome_keeps_level_failures_out_of_the_file_counts() {
    let tally = PollTally { in_window: 9 };
    let acc = PollAccumulator {
        level_failures: vec![level_failure(GlmSatellite::GoesEast, GlmDataLevel::Group)],
        ..Default::default()
    };
    let outcome = build_outcome(Vec::new(), Vec::new(), Vec::new(), Vec::new(), &tally, acc);

    assert!(outcome.parse_failures.is_none(), "no *file* failed");
    assert!(outcome.transport_failures.is_none());
    assert_eq!(outcome.level_failures.len(), 1);
}

/// Both kinds share the window as their denominator, and an empty bucket
/// stays `None` rather than reporting a zero-failure failure.
#[test]
fn build_outcome_leaves_an_empty_bucket_unreported() {
    let tally = PollTally { in_window: 14 };
    let acc = PollAccumulator {
        parse_errors: vec!["a.nc: boom".into()],
        ..Default::default()
    };
    let outcome = build_outcome(Vec::new(), Vec::new(), Vec::new(), Vec::new(), &tally, acc);

    assert_eq!(
        outcome.parse_failures.expect("parse failures").in_window,
        14
    );
    assert!(
        outcome.transport_failures.is_none(),
        "nothing failed to download, so there is nothing to report"
    );
}

/// Bytes that arrived but are not the product are a *parse* failure. Tagging
/// them Transport would point the user at their network for a product
/// problem.
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

/// A valid granule still parses through the classified wrapper.
#[test]
fn a_good_granule_parses_through_the_classified_stage() {
    let bytes = synthetic_glm_file(Fixture::default());
    let flashes = parse_downloaded_file(&bytes, GlmSatellite::GoesEast, &[GlmDataLevel::Flash])
        .expect("fixture should parse");
    assert_eq!(flashes.records.len(), 2);
    assert!(flashes.level_failures.is_empty());
}

/// A download that never lands is a *transport* failure, all the way out
/// through `download_and_parse_one`.
///
/// Hermetic: loopback port 1 (`tcpmux`) is not listening, so the connection
/// is refused immediately.
#[test]
fn an_unreachable_host_is_a_transport_failure() {
    // `reqwest` is pinned to `rustls-no-provider`, so `build()` panics with
    // "No provider set" unless a crypto provider is installed first.
    // `tls::client` is not used because it sets `https_only`, and the
    // cleartext loopback URL below is the point of the test.
    rustdar_radar::tls::init();
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

/// The total/partial distinction drives both the log severity and the panel
/// wording.
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

/// The floor, asserted against literals rather than
/// `MIN_FILES_FOR_TOTAL_VERDICT`: a loop over `1..CONST` is empty when the
/// constant is 1 and passes vacuously.
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

/// The floor must not swallow the case it exists for: one bad granule among
/// several is partial, at every window size.
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

/// A short *optional* column degrades instead of failing the file, and must
/// not hand back a half-length area column.
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

/// Losing the one optional column degrades the popup, not the whole overlay.
#[test]
fn missing_optional_area_degrades_without_failing_the_file() {
    let bytes = synthetic_glm_file(Fixture {
        omit: &["flash_area"],
        ..Default::default()
    });
    let flashes = parse_flashes(&bytes).expect("a missing area must not blank the whole overlay");
    assert_eq!(flashes.len(), 2);
    assert!(flashes.iter().all(|f| f.area.is_none()));
    // Position and energy are untouched.
    assert!((flashes[0].lat - 35.0).abs() < 1e-4);
    assert!(flashes[0].energy.is_some_and(|e| e > 0.0));
}

/// A round is refused only when **every** satellite was refused.
///
/// The loop this guards used to be `list_glm_files(...).await?`, so one
/// satellite's failure returned for the whole round and a dead GOES-East
/// silently blanked GOES-West. It collects per satellite now and only builds a
/// round verdict when *none* of them answered — and even then, one bucket
/// answering 400 while the other times out is not the product refusing us.
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

// ── The satellite loop, over a real socket ──────────────────────────────────
//
// The loop these drive — one listing per satellite, keeping whatever answered —
// is the half of `fetch_glm_flashes` that decides whether one dead satellite
// takes the other's flashes off the map. It had no test that ran it, because
// the S3 origin was built literally inside the listing and could not be pointed
// anywhere else; `DataSources::s3_base` is that obstacle removed, and these are
// what it was removed for. The verdict merge alone is pinned above, at
// `a_listing_round_is_refused_only_when_every_satellite_was`, and pinning a
// helper is not pinning the caller: the loop reached `of_round` correctly and
// could still have returned early, dropped the survivor, or counted a failed
// listing as a live feed.

/// An S3 `ListBucketResult` carrying one key, in the shape
/// [`list_glm_files`] parses.
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

/// A granule key whose encoded start time is years outside any live window.
///
/// So the listing is answered and **counted** — `objects_seen` is what tells a
/// dead feed from a quiet sky — while nothing is queued for download, which
/// keeps these tests to the listing round they are about.
const STALE_GRANULE: &str =
    "GLM-L2-LCFA/2020/001/00/OR_GLM-L2-LCFA_G18_s20200010000000_e20200010000200_c20200010000210.nc";

/// Serve canned S3 responses from loopback, picked by which bucket the request
/// names, and return sources that address every bucket there.
///
/// The whole point of [`DataSources::s3_base`]: production reads
/// `https://{bucket}.s3.amazonaws.com`, a test reads `http://127.0.0.1:{port}/{bucket}`,
/// and the fetch under test cannot tell the difference because it never spells
/// either one.
fn s3_serving(responses: Vec<(&'static str, String)>) -> DataSources {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let port = listener.local_addr().expect("local addr").port();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { break };
            let mut scratch = [0u8; 4096];
            let read = stream.read(&mut scratch).unwrap_or(0);
            let request = String::from_utf8_lossy(&scratch[..read]).to_string();
            let response = responses
                .iter()
                .find(|(bucket, _)| request.contains(&format!("/{bucket}/")))
                .map(|(_, response)| response.clone())
                .unwrap_or_else(|| http_response("404 Not Found", "<Error/>"));
            let _ = stream.write_all(response.as_bytes());
            let _ = stream.flush();
        }
    });
    DataSources {
        goes_east_bucket: "east".into(),
        goes_west_bucket: "west".into(),
        s3_base: format!("http://127.0.0.1:{port}/{{bucket}}").into(),
        ..DataSources::production()
    }
}

/// A cleartext-capable client: `tls::client` sets `https_only`, which a
/// loopback URL cannot satisfy, and `tls::init` is still required because
/// `reqwest` is pinned to `rustls-no-provider`.
fn loopback_client() -> reqwest::Client {
    rustdar_radar::tls::init();
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .expect("client")
}

/// One flash already in the cache from an earlier poll.
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

fn poll(sources: &DataSources, cache: &mut GlmCache) -> Result<GlmFetchOutcome, FetchError> {
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
    ))
}

/// **One dead satellite must not blank the other.**
///
/// The loop was `list_glm_files(...).await?`, so the first satellite to fail
/// returned for the whole round: a dead GOES-East silently took GOES-West's
/// flashes with it, on a layer where the survivor still covers most of CONUS.
/// The round is `Ok` now, and says which half of the sky stopped arriving
/// rather than reporting a green poll over a half-empty map.
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

/// A round fails only when **nothing** could be listed — and even then, one
/// bucket refusing while the other times out is not the product refusing us.
///
/// The verdict reaches the layer's ledger, so the difference is a ladder rung
/// against `REFUSALS_BEFORE_BROKEN` versus an ordinary backoff.
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

/// A bucket key whose `_s` field carries a multi-byte character is undatable,
/// not fatal.
///
/// The keys come from the text of `<Key>` nodes in the bucket's own
/// `ListObjectsV2` reply and are filtered on nothing but a `.nc` suffix, so the
/// shape of the `_s` field is the server's word and not this build's. The gate
/// that used to guard the six byte ranges below was `s_field.len() < 14`, in
/// bytes, and a multi-byte character anywhere in the first fourteen put one of
/// them inside a UTF-8 sequence — a panic in the GLM poll task, from a listing.
///
/// The last two cases are the ones a length gate cannot catch by counting: they
/// are long enough, and wrong in the middle.
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

/// The fix must not have made every key undatable: a real one still dates.
///
/// Without this, `parse_filename_start_time` could return `None` unconditionally
/// and the test above would still pass — and every GLM granule would silently
/// re-download on every poll.
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
