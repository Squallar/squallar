//! The 33-level roster, the stack's congruence rules, and the occupancy count.
//!
//! ## What is pinned offline and what is not
//!
//! Two real granules are committed, **byte for byte as the bucket serves them —
//! do not re-encode or re-compress either**: they are here to prove that a
//! directory named for a height carries that height in its own section 4, and a
//! re-encode is free to move exactly the octets that claim would rest on.
//!
//! * `MRMS_MergedReflectivityQC_00.50_20260830-090042.grib2.gz` (234 095 B) —
//!   the bottom of the roster, and the level most easily confused with the 2D
//!   `MergedReflectivityQCComposite_00.50` this crate already ships;
//! * `MRMS_MergedReflectivityQC_19.00_20260830-090042.grib2.gz` (61 680 B) —
//!   the top.
//!
//! **The two are the roster's endpoints, and endpoints are not the roster.**
//! The middle band's 0.5 km spacing and the count of 33 are held by the live
//! listing in `the_bucket_publishes_exactly_the_thirty_three_levels_declared`,
//! which is `#[ignore]`d because it is network. Committing a middle-band
//! granule too would cost 837 KB against a 2.9 MB testdata directory for a
//! check the live test already makes; what the fixtures buy is that the
//! *mechanism* — surface type, scale factor, scaled value — is proven without a
//! network, on the two levels where a mistake would be quietest.

use super::*;
use crate::hrrr::GridCoords;

const LEVEL_0_GZ: &[u8] =
    include_bytes!("../../../testdata/MRMS_MergedReflectivityQC_00.50_20260830-090042.grib2.gz");
const LEVEL_32_GZ: &[u8] =
    include_bytes!("../../../testdata/MRMS_MergedReflectivityQC_19.00_20260830-090042.grib2.gz");

fn decoded(gz: &[u8]) -> RawGrid {
    let grib = super::super::decode::gunzip(gz).expect("the committed granule is a gzip member");
    super::super::decode::parse_grib2_raw(&grib, &MISSING_CODES).expect("it decodes")
}

// ── The roster ──────────────────────────────────────────────────────────────

/// **The 33 directory names, spelled out.**
///
/// Read off `noaa-mrms-pds` on 2026-08-30 with
/// `?list-type=2&delimiter=/&prefix=CONUS/`, which answered `IsTruncated=false`
/// — so this is the whole set the bucket carries, not a page of it. Written as
/// literals rather than derived from [`LEVELS_KM_MSL`], because a table
/// compared against a formatting of itself is a check that cannot fail.
const BUCKET_DIRECTORIES: [&str; LEVEL_COUNT] = [
    "MergedReflectivityQC_00.50",
    "MergedReflectivityQC_00.75",
    "MergedReflectivityQC_01.00",
    "MergedReflectivityQC_01.25",
    "MergedReflectivityQC_01.50",
    "MergedReflectivityQC_01.75",
    "MergedReflectivityQC_02.00",
    "MergedReflectivityQC_02.25",
    "MergedReflectivityQC_02.50",
    "MergedReflectivityQC_02.75",
    "MergedReflectivityQC_03.00",
    "MergedReflectivityQC_03.50",
    "MergedReflectivityQC_04.00",
    "MergedReflectivityQC_04.50",
    "MergedReflectivityQC_05.00",
    "MergedReflectivityQC_05.50",
    "MergedReflectivityQC_06.00",
    "MergedReflectivityQC_06.50",
    "MergedReflectivityQC_07.00",
    "MergedReflectivityQC_07.50",
    "MergedReflectivityQC_08.00",
    "MergedReflectivityQC_08.50",
    "MergedReflectivityQC_09.00",
    "MergedReflectivityQC_10.00",
    "MergedReflectivityQC_11.00",
    "MergedReflectivityQC_12.00",
    "MergedReflectivityQC_13.00",
    "MergedReflectivityQC_14.00",
    "MergedReflectivityQC_15.00",
    "MergedReflectivityQC_16.00",
    "MergedReflectivityQC_17.00",
    "MergedReflectivityQC_18.00",
    "MergedReflectivityQC_19.00",
];

#[test]
fn every_level_spells_the_directory_the_bucket_carries() {
    assert_eq!(LEVELS_KM_MSL.len(), LEVEL_COUNT);
    for (level, expected) in BUCKET_DIRECTORIES.iter().enumerate() {
        assert_eq!(&level_prefix_name(level), expected);
    }
}

/// **The spacing is three bands, not a smooth widening.**
///
/// The plan this work came from described "0.25 km low, widening to 1 km
/// aloft", which reads as two bands and skips the 12 levels that step by
/// exactly 0.5 km. Pinned so the middle band is not lost again.
#[test]
fn the_roster_is_three_spacings_and_is_strictly_ascending() {
    for pair in LEVELS_KM_MSL.windows(2) {
        assert!(pair[1] > pair[0], "{pair:?} does not ascend");
    }
    let steps: Vec<f64> = LEVELS_KM_MSL
        .windows(2)
        .map(|w| ((w[1] - w[0]) * 100.0).round() / 100.0)
        .collect();
    assert_eq!(steps.iter().filter(|&&s| s == 0.25).count(), 10);
    assert_eq!(steps.iter().filter(|&&s| s == 0.50).count(), 12);
    assert_eq!(steps.iter().filter(|&&s| s == 1.00).count(), 10);
    assert_eq!(steps.len(), LEVEL_COUNT - 1);

    assert_eq!(LEVELS_KM_MSL[0], 0.50);
    assert_eq!(LEVELS_KM_MSL[10], 3.00);
    assert_eq!(LEVELS_KM_MSL[11], 3.50);
    assert_eq!(LEVELS_KM_MSL[22], 9.00);
    assert_eq!(LEVELS_KM_MSL[23], 10.00);
    assert_eq!(LEVELS_KM_MSL[LEVEL_COUNT - 1], 19.00);
}

/// The 3D bottom level and the 2D composite share the string `00.50` and are
/// different products in different directories. Nothing may confuse them.
#[test]
fn the_bottom_level_is_not_the_composite() {
    assert_ne!(
        level_prefix_name(0),
        super::super::MrmsProduct::ReflectivityComposite.prefix_name(),
    );
    assert!(!level_prefix_name(0).contains("Composite"));
}

#[test]
fn a_level_key_is_the_stamp_under_its_own_level() {
    let stamp = chrono::NaiveDate::from_ymd_opt(2026, 8, 30)
        .unwrap()
        .and_hms_opt(9, 0, 42)
        .unwrap();
    assert_eq!(
        level_key(12, &stamp),
        "CONUS/MergedReflectivityQC_04.00/20260830/\
         MRMS_MergedReflectivityQC_04.00_20260830-090042.grib2.gz",
    );
    // Every level files one timestep under the same stamp, which is what makes
    // 33 keys a pure function of one listing.
    for level in 0..LEVEL_COUNT {
        assert!(level_key(level, &stamp).ends_with("_20260830-090042.grib2.gz"));
    }
}

#[test]
fn the_stack_bytes_are_stated_rather_than_derived_in_a_hurry() {
    assert_eq!(CONUS_STACK_BYTES, 3_234_000_000);
    assert_eq!(CONUS_STACK_BYTES, 33 * super::super::CONUS_GRID_BYTES);
}

// ── The granules say their own height ───────────────────────────────────────

/// **The load-bearing check, offline**: the directory name and the granule's
/// own section 4 agree, and the surface is altitude above *mean sea level*.
///
/// A height above ground would be a different vertical axis entirely, and the
/// stack would sit on the terrain rather than on the geoid without anything
/// looking wrong.
#[test]
fn a_committed_granule_declares_the_height_its_directory_names() {
    for (level, gz) in [(0usize, LEVEL_0_GZ), (LEVEL_COUNT - 1, LEVEL_32_GZ)] {
        let raw = decoded(gz);
        let (surface_type, value_m) = raw
            .first_fixed_surface
            .expect("MRMS states a first fixed surface");
        assert_eq!(
            surface_type,
            SURFACE_TYPE_ALTITUDE_MSL,
            "{} is not an MSL altitude",
            level_prefix_name(level),
        );
        assert_eq!(value_m, LEVELS_KM_MSL[level] * 1000.0);
        check_declared_level(level, &raw).expect("the table agrees with the granule");
        assert_eq!((raw.ni, raw.nj), (7000, 3500));
        assert!(matches!(raw.coords, GridCoords::Regular { .. }));
    }
}

/// The non-triviality half: the same check *refuses* a granule stacked at the
/// wrong height. Without this the test above passes for a `check_declared_level`
/// that returns `Ok(())` unconditionally.
#[test]
fn the_height_check_refuses_a_granule_at_another_level() {
    let raw = decoded(LEVEL_0_GZ);
    for level in 1..LEVEL_COUNT {
        let err = check_declared_level(level, &raw)
            .expect_err("the 0.50 km granule is not any other level");
        assert!(err.contains("declares"), "{err}");
    }
}

/// The fixtures really carry the codes [`MISSING_CODES`] declares, and no
/// undeclared reserved code occurs at coverage-mask scale — the same
/// non-vacuity floor `decode::tests` holds the 2D products to.
#[test]
fn the_fixtures_carry_the_codes_this_product_declares() {
    for gz in [LEVEL_0_GZ, LEVEL_32_GZ] {
        let grib = super::super::decode::gunzip(gz).expect("gzip member");
        let ctx = grib::from_reader(std::io::Cursor::new(&grib)).expect("GRIB2");
        let (_i, submessage) = ctx.iter().next().expect("one submessage");
        let decoder = grib::Grib2SubmessageDecoder::from(submessage).expect("decoder");
        let raw: Vec<f32> = decoder.dispatch().expect("dispatch").collect();
        assert_eq!(raw.len(), 7000 * 3500);

        for &code in &MISSING_CODES {
            let n = raw.iter().filter(|v| (**v - code).abs() < 0.05).count();
            assert!(n > 0, "the fixture carries no {code}, so nothing tests it");
        }
        // -3 is the precipitation rate's code and must not be a coverage mask
        // here; genuine -3.0 dBZ returns are fine and are why the bar is
        // "coverage-mask scale" rather than zero.
        let threes = raw.iter().filter(|v| (**v + 3.0).abs() < 0.05).count();
        assert!(
            threes * 100 < raw.len(),
            "-3 occurs {threes} times of {}, which is coverage-mask scale for a \
             code this product does not declare",
            raw.len(),
        );
    }
}

// ── The assembler ───────────────────────────────────────────────────────────

fn stamp(second: u32) -> chrono::NaiveDateTime {
    chrono::NaiveDate::from_ymd_opt(2026, 8, 30)
        .unwrap()
        .and_hms_opt(9, 0, second)
        .unwrap()
}

fn coords(ni: usize, nj: usize) -> GridCoords {
    GridCoords::Regular {
        lat0: 55.0,
        lon0: -130.0,
        dlat: -0.01,
        dlon: 0.01,
        ni,
        nj,
        scan_mode: 0,
    }
}

/// A synthetic level, small enough that 33 of them cost nothing.
fn level(l: usize, ni: usize, nj: usize, valid: chrono::NaiveDateTime, fill: f32) -> LevelFetch {
    LevelFetch {
        level: l,
        compressed_bytes: 10,
        grib_bytes: 20,
        grid: RawGrid {
            ni,
            nj,
            coords: coords(ni, nj),
            bounds: squallar_geo::GeoBounds {
                min_lat: 20.0,
                max_lat: 55.0,
                min_lon: -130.0,
                max_lon: -60.0,
            },
            valid,
            first_fixed_surface: Some((SURFACE_TYPE_ALTITUDE_MSL, LEVELS_KM_MSL[l] * 1000.0)),
            values: vec![fill; ni * nj],
        },
    }
}

fn full_stack() -> MrmsVolume {
    let mut a = VolumeAssembler::new();
    for l in 0..LEVEL_COUNT {
        a.push(level(l, 4, 3, stamp(42), l as f32)).expect("push");
    }
    a.finish().expect("all 33 arrived")
}

#[test]
fn a_complete_stack_holds_every_level_at_its_own_index() {
    let v = full_stack();
    assert_eq!((v.ni, v.nj), (4, 3));
    assert_eq!(v.points_per_level(), 12);
    assert_eq!(v.cells(), LEVEL_COUNT * 12);
    assert_eq!(v.values.len(), v.cells());
    assert_eq!(v.resident_bytes(), v.cells() * 4);
    assert_eq!(v.valid, stamp(42));
    assert_eq!(v.compressed_bytes(), LEVEL_COUNT * 10);
    assert_eq!(v.grib_bytes(), LEVEL_COUNT * 20);
    assert_eq!(v.compressed_bytes_by_level, [10; LEVEL_COUNT]);
    for l in 0..LEVEL_COUNT {
        assert!(
            v.level_values(l).iter().all(|&x| x == l as f32),
            "level {l} is not where it was pushed",
        );
    }
}

/// **A partial stack is refused, not returned short.** A hole at one height
/// reads downstream as clear air at that height — the "silent partial success"
/// shape, where a round answers `Ok` while the picture under-draws.
#[test]
fn a_stack_missing_one_level_is_refused() {
    let mut a = VolumeAssembler::new();
    for l in 0..LEVEL_COUNT {
        if l == 17 {
            continue;
        }
        a.push(level(l, 4, 3, stamp(42), 0.0)).expect("push");
    }
    assert_eq!(a.missing_levels(), vec![17]);
    let err = a.finish().expect_err("32 of 33 is not a timestep");
    assert!(err.contains("MergedReflectivityQC_06.50"), "{err}");
}

#[test]
fn the_assembler_refuses_every_way_two_levels_can_disagree() {
    // A different valid time: a stack of two timesteps is not a timestep.
    let mut a = VolumeAssembler::new();
    a.push(level(0, 4, 3, stamp(42), 0.0)).expect("push");
    let err = a
        .push(level(1, 4, 3, stamp(44), 0.0))
        .expect_err("two timesteps");
    assert!(err.contains("valid"), "{err}");

    // A different shape.
    let mut a = VolumeAssembler::new();
    a.push(level(0, 4, 3, stamp(42), 0.0)).expect("push");
    let err = a
        .push(level(1, 5, 3, stamp(42), 0.0))
        .expect_err("two shapes");
    assert!(err.contains("where the stack is"), "{err}");

    // A different horizontal grid at the same shape: the points would not
    // stand above one another, and nothing about the shape would say so.
    let mut a = VolumeAssembler::new();
    a.push(level(0, 4, 3, stamp(42), 0.0)).expect("push");
    let mut shifted = level(1, 4, 3, stamp(42), 0.0);
    shifted.grid.coords = GridCoords::Regular {
        lat0: 55.0,
        lon0: -129.0,
        dlat: -0.01,
        dlon: 0.01,
        ni: 4,
        nj: 3,
        scan_mode: 0,
    };
    let err = a.push(shifted).expect_err("two grids");
    assert!(err.contains("horizontal grid"), "{err}");

    // The same level twice.
    let mut a = VolumeAssembler::new();
    a.push(level(0, 4, 3, stamp(42), 0.0)).expect("push");
    let err = a
        .push(level(0, 4, 3, stamp(42), 0.0))
        .expect_err("one level twice");
    assert!(err.contains("arrived twice"), "{err}");

    // A granule whose section 4 disagrees with the index it is pushed at.
    let mut a = VolumeAssembler::new();
    let mut mislevelled = level(5, 4, 3, stamp(42), 0.0);
    mislevelled.grid.first_fixed_surface = Some((SURFACE_TYPE_ALTITUDE_MSL, 12_000.0));
    let err = a.push(mislevelled).expect_err("wrong height");
    assert!(err.contains("declares"), "{err}");

    // A height above ground rather than above mean sea level.
    let mut a = VolumeAssembler::new();
    let mut agl = level(5, 4, 3, stamp(42), 0.0);
    agl.grid.first_fixed_surface = Some((103, LEVELS_KM_MSL[5] * 1000.0));
    let err = a.push(agl).expect_err("above ground is another axis");
    assert!(err.contains("mean sea level"), "{err}");
}

// ── Occupancy ───────────────────────────────────────────────────────────────

#[test]
fn occupancy_counts_readings_and_excludes_every_missing_code() {
    let values = [
        f32::NAN, // a mapped -999 or -99
        -30.0,
        -0.1,
        0.0,
        4.9,
        5.0,
        19.9,
        20.0,
        39.9,
        40.0,
        70.0,
    ];
    let o = Occupancy::of(&values);
    assert_eq!(o.cells, 11);
    assert_eq!(o.readings, 10);
    // >= 0, >= 5, >= 20, >= 40
    assert_eq!(o.at_or_above, [8, 6, 4, 2]);
    assert!((o.fraction(o.readings) - 10.0 / 11.0).abs() < 1e-12);
    assert_eq!(Occupancy::default().fraction(1), 0.0);
}

/// The counts are counts, so two scopes add — which is what lets a per-level
/// figure and a whole-stack figure be the same measurement stated twice.
#[test]
fn a_stacks_occupancy_is_the_sum_of_its_levels() {
    let v = full_stack();
    let whole = v.occupancy();
    let per_level = v.per_level_occupancy();
    assert_eq!(per_level.len(), LEVEL_COUNT);
    assert_eq!(whole.cells, v.cells());
    assert_eq!(
        whole,
        per_level
            .into_iter()
            .fold(Occupancy::default(), Occupancy::merged),
    );
    // The synthetic stack fills level `l` with `l` dBZ, so every level from 5
    // up clears the 5 dBZ bar and no cell is missing.
    assert_eq!(whole.readings, whole.cells);
    assert_eq!(whole.at_or_above[1], (LEVEL_COUNT - 5) * 12);
}

/// The thresholds are a ladder, so their counts can only fall as it climbs. A
/// pair that inverted would mean the count loop had lost its ordering.
#[test]
fn the_occupancy_thresholds_are_a_ladder() {
    for pair in OCCUPANCY_THRESHOLDS_DBZ.windows(2) {
        assert!(pair[1] > pair[0], "{pair:?}");
    }
    let raw = decoded(LEVEL_0_GZ);
    let o = Occupancy::of(&raw.values);
    assert_eq!(o.cells, 7000 * 3500);
    for pair in o.at_or_above.windows(2) {
        assert!(pair[1] <= pair[0], "{:?} is not a ladder", o.at_or_above);
    }
    assert!(o.readings <= o.cells);
    assert!(o.at_or_above[0] <= o.readings);
    // Not vacuous: the real granule has something in it at the lowest bar.
    assert!(o.at_or_above[1] > 0);
}

// ── The live bucket (network; `#[ignore]`d exactly as `live_mrms` is) ────────

/// **The roster, against the bucket that publishes it.**
///
/// The count of 33 and the middle band's 0.5 km spacing have no offline proof —
/// the committed fixtures are the roster's two endpoints — so this is what
/// carries them. `#[ignore]`d because it is network.
///
/// `cargo test -p squallar-overlays -- --ignored --nocapture volume::tests`
#[cfg(not(target_arch = "wasm32"))]
#[tokio::test]
#[ignore = "hits the live noaa-mrms-pds S3 bucket"]
async fn the_bucket_publishes_exactly_the_thirty_three_levels_declared() {
    let client = squallar_source::tls::client(
        squallar_source::tls::USER_AGENT,
        std::time::Duration::from_secs(120),
    )
    .build()
    .expect("client");
    let sources = DataSources::production();

    // The delimited listing of `CONUS/`, which is where the roster lives.
    let url = format!(
        "{}/?list-type=2&delimiter=/&prefix=CONUS/&max-keys=1000",
        sources.s3_bucket_url(&sources.mrms_bucket),
    );
    let body = client
        .get(&url)
        .send()
        .await
        .expect("listing")
        .text()
        .await
        .expect("listing body");
    let doc = roxmltree::Document::parse(&body).expect("listing is XML");
    assert!(
        doc.descendants()
            .find(|n| n.tag_name().name() == "IsTruncated")
            .and_then(|n| n.text())
            == Some("false"),
        "the roster listing is truncated, so a missing directory would read as \
         a short roster rather than as a paging failure",
    );
    let mut found: Vec<String> = doc
        .descendants()
        .filter(|n| n.tag_name().name() == "Prefix")
        .filter_map(|n| n.text())
        .filter_map(|p| p.strip_prefix("CONUS/"))
        .filter_map(|p| p.strip_suffix('/'))
        // `_` and not the bare stem: `MergedReflectivityQCComposite_00.50` and
        // `MergedReflectivityQComposite_00.50` both start with a prefix of this
        // name and are not levels.
        .filter(|p| p.starts_with("MergedReflectivityQC_"))
        .map(|p| p.to_string())
        .collect();
    found.sort();
    let mut declared: Vec<String> = (0..LEVEL_COUNT).map(level_prefix_name).collect();
    declared.sort();
    assert_eq!(
        found, declared,
        "the bucket's roster is not the declared one"
    );
    println!("roster: {} level directories, as declared", found.len());
}

/// **One timestep, stacked, with what it cost.**
///
/// The measurement E1 exists to take. Every figure is printed with its own
/// denominator and **nothing is averaged across timesteps**: an active
/// afternoon and a quiet night differ by orders of magnitude, and one mean
/// describes neither.
///
/// Which timesteps it measures:
///
/// * unset — the newest stamp all 33 levels have published;
/// * `SQUALLAR_MRMS_STACK_AT=2026-08-29T21:00,2026-08-30T09:00` — for each
///   instant, the newest complete stamp at or before it. Stamps are not
///   clock-aligned, so an instant names a *neighbourhood*, and the stamp
///   actually measured is printed.
///
/// `#[ignore]`d because it is network, and because each timestep holds 3.2 GB.
/// Run it `--release`: a debug decode is several times slower and the wall
/// clock would describe a profile nothing ships.
///
/// `cargo test -p squallar-overlays --release -- --ignored --nocapture the_live_stack`
#[cfg(not(target_arch = "wasm32"))]
#[tokio::test]
#[ignore = "hits the live noaa-mrms-pds S3 bucket and holds 3.2 GB per timestep"]
async fn the_live_stack_decodes_and_reports_its_own_cost() {
    let client = squallar_source::tls::client(
        squallar_source::tls::USER_AGENT,
        std::time::Duration::from_secs(120),
    )
    .build()
    .expect("client");
    let sources = DataSources::production();

    let requested: Vec<chrono::NaiveDateTime> = std::env::var("SQUALLAR_MRMS_STACK_AT")
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| {
            chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M")
                .unwrap_or_else(|e| panic!("{s:?} is not a YYYY-MM-DDTHH:MM instant: {e}"))
        })
        .collect();

    let instants = if requested.is_empty() {
        vec![chrono::Utc::now().naive_utc()]
    } else {
        requested
    };
    let mut stamps: Vec<chrono::NaiveDateTime> = Vec::new();
    for at in instants {
        let listing = std::time::Instant::now();
        let stamp = latest_complete_stamp(&client, &sources, at)
            .await
            .unwrap_or_else(|e| panic!("no complete stamp at or before {at}Z: {e}"));
        println!(
            "asked for {at}Z, newest complete stamp at or before it is {stamp}Z              ({LEVEL_COUNT} bounded listings in {:.2} s)",
            listing.elapsed().as_secs_f64(),
        );
        stamps.push(stamp);
    }

    for stamp in stamps {
        let started = std::time::Instant::now();
        let volume = fetch_stack(&client, &sources, &stamp)
            .await
            .expect("33 levels stack");
        let elapsed = started.elapsed();

        assert_eq!((volume.ni, volume.nj), (7000, 3500));
        assert_eq!(volume.cells(), 808_500_000);
        assert_eq!(volume.resident_bytes(), CONUS_STACK_BYTES);
        assert_eq!(volume.valid, stamp);

        let o = volume.occupancy();
        println!(
            "\nMRMS 3D stack {stamp}Z\n\
               levels                {LEVEL_COUNT}\n\
               grid                  {}x{} = {} points per level\n\
               cells                 {} (the denominator of every share below)\n\
               download (gzipped)    {} B\n\
               GRIB2 after gunzip    {} B\n\
               resident f32          {} B\n\
               fetch+decode+stack    {:.2} s, {} concurrent GETs\n\
               readings              {} ({:.4} %)\n\
               >= 0 dBZ              {} ({:.4} %)\n\
               >= 5 dBZ              {} ({:.4} %)\n\
               >= 20 dBZ             {} ({:.4} %)\n\
               >= 40 dBZ             {} ({:.4} %)",
            volume.ni,
            volume.nj,
            volume.points_per_level(),
            o.cells,
            volume.compressed_bytes(),
            volume.grib_bytes(),
            volume.resident_bytes(),
            elapsed.as_secs_f64(),
            STACK_FETCH_CONCURRENCY,
            o.readings,
            o.fraction(o.readings) * 100.0,
            o.at_or_above[0],
            o.fraction(o.at_or_above[0]) * 100.0,
            o.at_or_above[1],
            o.fraction(o.at_or_above[1]) * 100.0,
            o.at_or_above[2],
            o.fraction(o.at_or_above[2]) * 100.0,
            o.at_or_above[3],
            o.fraction(o.at_or_above[3]) * 100.0,
        );

        println!("\n  level      km   bytes(gz)    readings     >=5 dBZ    >=40 dBZ   share>=5");
        for (l, lo) in volume.per_level_occupancy().into_iter().enumerate() {
            println!(
                "  {:>5}  {:>6.2}   {:>9}   {:>9}   {:>9}   {:>9}   {:>7.4} %",
                l,
                LEVELS_KM_MSL[l],
                volume.compressed_bytes_by_level[l],
                lo.readings,
                lo.at_or_above[1],
                lo.at_or_above[3],
                lo.fraction(lo.at_or_above[1]) * 100.0,
            );
        }

        // Non-vacuity: a stack that decoded to nothing at all would otherwise
        // report a beautifully small occupancy.
        assert!(o.readings > 0, "the whole stack decoded to missing");
        assert!(o.at_or_above[1] > 0, "no cell anywhere clears 5 dBZ");

        // **The undeclared-code sweep, across all 33 levels rather than the
        // two committed granules.** `-3` is the precipitation rate's
        // no-coverage code and is not in `MISSING_CODES`, so it survives the
        // decode's mapping and would be reported as a reading. Genuine -3.0 dBZ
        // returns are ordinary, which is why the bar is coverage-mask scale
        // rather than zero — the 2D composite fixture carries 347 of them in
        // 24.5 M points.
        let threes = volume
            .values
            .iter()
            .filter(|v| (**v + 3.0).abs() < 0.05)
            .count();
        println!("  -3.0 dBZ (undeclared code) {threes} of {}", o.cells);
        assert!(
            threes * 100 < o.cells,
            "-3 occurs {threes} times of {}, which is coverage-mask scale for a \
             code this product does not declare",
            o.cells,
        );
    }
}
