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
use crate::hrrr::GridCoords::Regular;

const LEVEL_0_GZ: &[u8] =
    include_bytes!("../../../testdata/MRMS_MergedReflectivityQC_00.50_20260830-090042.grib2.gz");
const LEVEL_32_GZ: &[u8] =
    include_bytes!("../../../testdata/MRMS_MergedReflectivityQC_19.00_20260830-090042.grib2.gz");
/// The 2D column-max composite this crate already ships — **the substitution
/// the level check exists to refuse**, and the one it could not see before the
/// parameter category was carried.
const COMPOSITE_GZ: &[u8] = include_bytes!(
    "../../../testdata/MRMS_MergedReflectivityQCComposite_00.50_20260821-000039.grib2.gz"
);

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
        assert_eq!(raw.parameter, Some(PARAMETER));
        check_granule_is_this_level(level, &raw).expect("the table agrees with the granule");
        assert_eq!((raw.ni, raw.nj), (7000, 3500));
        assert!(matches!(raw.coords, GridCoords::Regular { .. }));
    }
}

/// The non-triviality half: the same check *refuses* a granule stacked at the
/// wrong height. Without this the test above passes for a
/// `check_granule_is_this_level` that returns `Ok(())` unconditionally.
#[test]
fn the_height_check_refuses_a_granule_at_another_level() {
    let raw = decoded(LEVEL_0_GZ);
    for level in 1..LEVEL_COUNT {
        let err = check_granule_is_this_level(level, &raw)
            .expect_err("the 0.50 km granule is not any other level");
        assert!(err.contains("declares"), "{err}");
    }
}

/// **The substitution a height check cannot see.**
///
/// The 2D composite and the 0.50 km level declare the *same* first fixed
/// surface — `(102, 500 m)` — the same 7000 × 3500 grid, the same packing and
/// the same reserved codes. An earlier version of this suite checked only the
/// height, and swapping the composite granule into level 0's slot passed all 14
/// of its tests. The parameter category is what tells them apart, and this
/// asserts the premise as well as the refusal: if the two ever declared the
/// same category, the refusal below would be resting on nothing.
#[test]
fn the_level_check_refuses_the_two_dimensional_composite() {
    let level = decoded(LEVEL_0_GZ);
    let composite = decoded(COMPOSITE_GZ);

    // The premise: everything except the category agrees.
    assert_eq!(level.first_fixed_surface, composite.first_fixed_surface);
    assert_eq!((level.ni, level.nj), (composite.ni, composite.nj));
    for (a, b) in [
        (level.bounds.min_lat, composite.bounds.min_lat),
        (level.bounds.max_lat, composite.bounds.max_lat),
        (level.bounds.min_lon, composite.bounds.min_lon),
        (level.bounds.max_lon, composite.bounds.max_lon),
    ] {
        assert!((a - b).abs() < 1e-5, "{a} vs {b}");
    }
    // The two grids agree to within 3e-10 degrees — the same corners, the same
    // shape, a step that differs only in the last bits of the section-3
    // encoding. **That near-miss is not a discriminator and must not be used as
    // one**: it is 3e-8 of a 0.01 deg cell, it is a property of how two
    // granules happened to be written rather than of what they are, and a
    // check resting on it would pass or fail by rounding.
    let (
        Regular {
            lat0: la,
            lon0: lo,
            dlat: da,
            dlon: do_,
            ..
        },
        Regular {
            lat0: lb,
            lon0: lo2,
            dlat: db,
            dlon: do2,
            ..
        },
    ) = (&level.coords, &composite.coords)
    else {
        panic!("both MRMS grids are the regular arm")
    };
    assert_eq!((la, lo), (lb, lo2));
    assert!((da - db).abs() < 1e-8 && (do_ - do2).abs() < 1e-8);

    // The discriminator, and it is the only one.
    assert_eq!(level.parameter, Some(PARAMETER));
    assert_eq!(composite.parameter, Some(COMPOSITE_PARAMETER));
    assert_ne!(PARAMETER, COMPOSITE_PARAMETER);

    let err = check_granule_is_this_level(0, &composite)
        .expect_err("the composite is not the 0.50 km level, however alike they look");
    assert!(err.contains("column-max composite"), "{err}");
    // And it is refused at every level, not only the one it shares a height
    // with.
    for l in 0..LEVEL_COUNT {
        assert!(check_granule_is_this_level(l, &composite).is_err());
    }
}

/// A granule that states no parameter at all is refused rather than admitted on
/// its height — the `None` arm the two fixtures cannot reach.
#[test]
fn a_granule_with_no_parameter_is_refused() {
    let mut raw = decoded(LEVEL_0_GZ);
    raw.parameter = None;
    let err = check_granule_is_this_level(0, &raw).expect_err("no category, no claim");
    assert!(err.contains("no parameter category"), "{err}");

    // A neighbouring category is refused without the composite's explanation.
    let mut raw = decoded(LEVEL_0_GZ);
    raw.parameter = Some((9, 1));
    let err = check_granule_is_this_level(0, &raw).expect_err("a different parameter number");
    assert!(err.contains("not (9, 0)"), "{err}");
    assert!(!err.contains("column-max composite"), "{err}");
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
            parameter: Some(PARAMETER),
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

/// **The hole is `NaN`, never `0.0`** — the second line of defence behind
/// `finish`'s refusal of a partial stack.
///
/// 0 dBZ is a *reading*: a level that never arrived would read downstream as a
/// real, weak echo at that height rather than as an absence, and `Occupancy`
/// would count it at the 0 dBZ bar. Unobservable through `finish` today, which
/// is precisely why it needs a test of its own — an unobservable defence is one
/// nothing holds.
#[test]
fn an_unfilled_level_is_nan_and_never_zero() {
    let mut a = VolumeAssembler::new();
    a.push(level(7, 4, 3, stamp(42), 3.0)).expect("push");
    let filled = a.partial_values();
    assert_eq!(filled.len(), LEVEL_COUNT * 12);
    for (i, &v) in filled.iter().enumerate() {
        let level_of = i / 12;
        if level_of == 7 {
            assert_eq!(v, 3.0);
        } else {
            assert!(
                v.is_nan(),
                "level {level_of} cell {i} filled with {v}, not NaN"
            );
        }
    }
    // And the count agrees: only the one pushed level is a reading.
    assert_eq!(Occupancy::of(filled).readings, 12);
}

#[test]
fn the_assembler_refuses_every_way_two_levels_can_disagree() {
    // A valid time past the tolerance: a stack of two timesteps is not a
    // timestep. The neighbouring scan is ~120 s away, so this is the case that
    // matters and it is refused by a wide margin.
    let mut a = VolumeAssembler::new();
    a.push(level(0, 4, 3, stamp(42), 0.0)).expect("push");
    let err = a
        .push(level(
            1,
            4,
            3,
            stamp(42) + chrono::Duration::seconds(120),
            0.0,
        ))
        .expect_err("two timesteps");
    assert!(err.contains("more than the"), "{err}");

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

    // A different envelope at the same shape and the same coordinates. Only
    // reachable by construction — the decoder derives bounds from the
    // coordinates — but the branch exists, the test's name claims every way,
    // and an unreachable-by-decode branch is exactly the one a future decoder
    // change would reach first.
    let mut a = VolumeAssembler::new();
    a.push(level(0, 4, 3, stamp(42), 0.0)).expect("push");
    let mut moved = level(1, 4, 3, stamp(42), 0.0);
    moved.grid.bounds = squallar_geo::GeoBounds {
        min_lat: 21.0,
        max_lat: 55.0,
        min_lon: -130.0,
        max_lon: -60.0,
    };
    let err = a.push(moved).expect_err("two envelopes");
    assert!(err.contains("envelope"), "{err}");

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

// ── Which stamps are one timestep ───────────────────────────────────────────
//
// `timesteps` and `stamps_at_or_before` are pure, and everything below runs on
// the default `cargo test` row. That is deliberate: a review found that
// replacing the whole cross-level match with "take the first level's stamps"
// passed every test in this file, because the only callers were `#[ignore]`d.
// The claim in this module's own commit message — "the stamp is the
// intersection and never the newest of any one level" — was defended by
// nothing. These are what defend it.

/// A whole day of aligned stamps at the real 120 s cadence, `count` of them.
fn aligned_day(count: usize) -> Vec<Vec<chrono::NaiveDateTime>> {
    let base = stamp(0);
    let series: Vec<chrono::NaiveDateTime> = (0..count)
        .map(|k| base + chrono::Duration::seconds(120 * k as i64))
        .collect();
    vec![series; LEVEL_COUNT]
}

#[test]
fn aligned_levels_give_one_timestep_per_scan() {
    let found = timesteps(&aligned_day(5));
    assert_eq!(found.len(), 5);
    for (k, t) in found.iter().enumerate() {
        assert!(t.is_aligned());
        assert_eq!(t.span_seconds(), 0);
        assert_eq!(
            t.valid(),
            stamp(0) + chrono::Duration::seconds(120 * k as i64)
        );
        assert!(t.stamps.iter().all(|s| *s == t.valid()));
    }
}

/// **The mutant this exists to kill.** Level 0 carries a stamp no other level
/// published — exactly the `003242`/`003243` partition, in the direction where
/// taking the first level's list would invent a timestep 32 levels cannot serve.
#[test]
fn a_stamp_only_the_first_level_published_is_not_a_timestep() {
    let mut per_level = aligned_day(3);
    let orphan = stamp(0) + chrono::Duration::seconds(3600);
    per_level[0].push(orphan);
    per_level[0].sort_unstable();

    let found = timesteps(&per_level);
    assert_eq!(found.len(), 3, "the orphan stamp became a timestep");
    assert!(
        found.iter().all(|t| t.valid() != orphan),
        "a stamp only one level published was admitted",
    );

    // And in the other direction: a stamp every level *but* the first has is
    // not a timestep either, which "take the last level's list" would also get
    // wrong.
    let mut per_level = aligned_day(3);
    let without_first = stamp(0) + chrono::Duration::seconds(7200);
    for stamps in per_level.iter_mut().skip(1) {
        stamps.push(without_first);
        stamps.sort_unstable();
    }
    assert_eq!(timesteps(&per_level).len(), 3);
}

/// **The F2 recovery, in the shape the bucket actually publishes it**: the low
/// six levels one second behind the other 27, zero overlap. An exact-match
/// intersection loses the whole timestep; the tolerance keeps it, and keeps
/// each level's own stamp so the keys resolve.
#[test]
fn a_one_second_partition_across_the_levels_is_still_one_timestep() {
    let mut per_level = aligned_day(1);
    let early = stamp(0);
    let late = early + chrono::Duration::seconds(1);
    for (l, stamps) in per_level.iter_mut().enumerate() {
        stamps[0] = if l < 6 { early } else { late };
    }

    // The premise: no stamp is shared by all 33, so an exact intersection is
    // empty. Stated rather than assumed, or the recovery below proves nothing.
    let shared: Vec<_> = per_level[0]
        .iter()
        .filter(|s| per_level.iter().all(|l| l.contains(s)))
        .collect();
    assert!(
        shared.is_empty(),
        "the partition is not a partition: {shared:?}"
    );

    let found = timesteps(&per_level);
    assert_eq!(found.len(), 1, "the partitioned timestep was lost");
    let t = found[0];
    assert!(!t.is_aligned());
    assert_eq!(t.span_seconds(), 1);
    assert_eq!(t.valid(), late, "a timestep is named by its latest level");
    // Each level keeps its OWN stamp, or 27 of the 33 keys would 404.
    for l in 0..LEVEL_COUNT {
        assert_eq!(t.stamps[l], if l < 6 { early } else { late });
    }
}

/// The tolerance has an edge, and past it the levels are two scans.
#[test]
fn a_split_wider_than_the_tolerance_is_not_one_timestep() {
    for offset in [STAMP_TOLERANCE_SECONDS, STAMP_TOLERANCE_SECONDS + 1] {
        let mut per_level = aligned_day(1);
        for stamps in per_level.iter_mut().skip(6) {
            stamps[0] += chrono::Duration::seconds(offset);
        }
        let found = timesteps(&per_level);
        if offset <= STAMP_TOLERANCE_SECONDS {
            assert_eq!(found.len(), 1, "a {offset} s split is inside the tolerance");
            assert_eq!(found[0].span_seconds(), offset);
        } else {
            assert!(found.is_empty(), "a {offset} s split is two scans, not one");
        }
    }
}

/// Two adjacent scans at the real cadence are never merged — the tolerance is
/// 2.5 % of 120 s, and this is what says so in code rather than in prose.
#[test]
fn adjacent_scans_are_never_merged_into_one_timestep() {
    let found = timesteps(&aligned_day(2));
    assert_eq!(found.len(), 2);
    assert_eq!(
        (found[1].valid() - found[0].valid()).num_seconds(),
        120,
        "two scans collapsed into one",
    );
}

/// A level that published nothing takes every timestep with it, and a wrongly
/// sized input answers empty rather than guessing.
#[test]
fn a_level_with_no_stamps_leaves_no_timestep() {
    let mut per_level = aligned_day(4);
    per_level[19].clear();
    assert!(timesteps(&per_level).is_empty());

    assert!(timesteps(&[]).is_empty());
    assert!(timesteps(&aligned_day(4)[..LEVEL_COUNT - 1]).is_empty());
}

/// **The F5 mutant**: without the `at` filter this returns the whole day, and a
/// "newest at or before 15:00Z" question is answered with the evening's scan.
#[test]
fn stamps_are_filtered_to_the_instant_that_was_asked_about() {
    let day = chrono::NaiveDate::from_ymd_opt(2026, 8, 30).unwrap();
    let keys: Vec<String> = [0u32, 6, 12, 18]
        .iter()
        .map(|h| DataSources::mrms_key(&level_prefix_name(0), &day.and_hms_opt(*h, 0, 42).unwrap()))
        .collect();

    let at = day.and_hms_opt(12, 30, 0).unwrap();
    let got = stamps_at_or_before(&keys, at);
    assert_eq!(got.len(), 3, "an unfiltered listing answers the whole day");
    assert_eq!(
        got.last().copied(),
        Some(day.and_hms_opt(12, 0, 42).unwrap())
    );
    assert!(got.iter().all(|s| *s <= at));

    // The boundary is inclusive: a stamp exactly at `at` is at or before it.
    let exact = day.and_hms_opt(12, 0, 42).unwrap();
    assert_eq!(stamps_at_or_before(&keys, exact).len(), 3);
    assert_eq!(
        stamps_at_or_before(&keys, exact - chrono::Duration::seconds(1)).len(),
        2,
    );

    // Ascending and deduped, and a key with no decodable stamp is dropped
    // rather than kept: an undatable key cannot be shown to be at or before
    // anything.
    let mut noisy = keys.clone();
    noisy.push(keys[0].clone());
    noisy.push("CONUS/MergedReflectivityQC_00.50/20260830/".to_string());
    noisy.reverse();
    let got = stamps_at_or_before(&noisy, day.and_hms_opt(23, 59, 59).unwrap());
    assert_eq!(got.len(), 4);
    assert!(got.windows(2).all(|w| w[0] < w[1]));
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
    // **Not** re-folding `per_level` here: `occupancy()` *is* that fold, so the
    // comparison would be `x == x`. Sum the fields independently instead, which
    // is the claim the fold is supposed to deliver.
    assert_eq!(
        whole.readings,
        per_level.iter().map(|o| o.readings).sum::<usize>(),
    );
    for (i, threshold) in OCCUPANCY_THRESHOLDS_DBZ.iter().enumerate() {
        assert_eq!(
            whole.at_or_above[i],
            per_level.iter().map(|o| o.at_or_above[i]).sum::<usize>(),
            "the {threshold} dBZ counts do not add up",
        );
    }
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
/// denominator and **nothing is averaged across timesteps**: a broad winter
/// system and a quiet summer night differ by orders of magnitude, and one mean
/// describes neither.
///
/// Which timesteps it measures:
///
/// * unset — the newest timestep all 33 levels have published;
/// * `SQUALLAR_MRMS_STACK_AT=2026-01-14T09:00,2026-08-29T21:00` — for each
///   instant, the newest complete timestep at or before it. Stamps are not
///   clock-aligned, so an instant names a *neighbourhood*, and the timestep
///   actually measured is printed.
///
/// **Sample across the retention, not across a week.** The bucket keeps at
/// least 20 months, so a week of August says nothing about February; the module
/// doc's table is 24 draws across 12 months for exactly this reason.
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

    let mut wanted: Vec<StackStamps> = Vec::new();
    for at in instants {
        let listing = std::time::Instant::now();
        let timestep = latest_timestep(&client, &sources, at)
            .await
            .unwrap_or_else(|e| panic!("no complete timestep at or before {at}Z: {e}"));
        println!(
            "asked for {at}Z, newest complete timestep at or before it is {}Z (stamp span {} s; {LEVEL_COUNT} bounded listings in {:.2} s)",
            timestep.valid(),
            timestep.span_seconds(),
            listing.elapsed().as_secs_f64(),
        );
        wanted.push(timestep);
    }

    for timestep in wanted {
        let stamp = timestep.valid();
        let started = std::time::Instant::now();
        let volume = fetch_stack(&client, &sources, &timestep)
            .await
            .expect("33 levels stack");
        let elapsed = started.elapsed();

        assert_eq!((volume.ni, volume.nj), (7000, 3500));
        assert_eq!(volume.cells(), 808_500_000);
        assert_eq!(volume.resident_bytes(), CONUS_STACK_BYTES);
        // The granules' own section 1 times agree with the keys they were
        // fetched under, split and all.
        assert_eq!(volume.valid, stamp);
        assert_eq!(volume.valid_span_seconds, timestep.span_seconds());

        let o = volume.occupancy();
        println!(
            "\nMRMS 3D stack {stamp}Z\n\
               levels                {LEVEL_COUNT} (stamp span {} s)\n\
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
            volume.valid_span_seconds,
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
        // 24.5 M points, and this sweep has measured anywhere from 5 231 to
        // 240 594 across a stack, both far under the bar.
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

/// **How often the 33 levels do not share a stamp, and what the tolerance
/// recovers** — the measurement behind [`STAMP_TOLERANCE_SECONDS`].
///
/// Lists a whole UTC day for all 33 levels and reports, with denominators: each
/// level's granule count, the union of stamps, the **exact** intersection (what
/// a single-stamp design could address), the tolerant timestep count, how many
/// of those are split across levels, and the largest hole in each series
/// against the ~120 s cadence.
///
/// `#[ignore]`d because it is network — ~3 MB of listings per day.
///
/// `cargo test -p squallar-overlays -- --ignored --nocapture the_levels_do_not`
#[cfg(not(target_arch = "wasm32"))]
#[tokio::test]
#[ignore = "hits the live noaa-mrms-pds S3 bucket for 33 whole-day listings"]
async fn the_levels_do_not_always_share_a_stamp_and_the_tolerance_recovers_them() {
    let client = squallar_source::tls::client(
        squallar_source::tls::USER_AGENT,
        std::time::Duration::from_secs(120),
    )
    .build()
    .expect("client");
    let sources = DataSources::production();

    let days: Vec<chrono::NaiveDate> = std::env::var("SQUALLAR_MRMS_DAY")
        .unwrap_or_else(|_| "20260829,20260315".to_string())
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| {
            chrono::NaiveDate::parse_from_str(s, "%Y%m%d")
                .unwrap_or_else(|e| panic!("{s:?} is not a YYYYMMDD day: {e}"))
        })
        .collect();

    for day in days {
        let end = day.and_hms_opt(23, 59, 59).unwrap();
        let mut per_level: Vec<Vec<chrono::NaiveDateTime>> = Vec::with_capacity(LEVEL_COUNT);
        for level in 0..LEVEL_COUNT {
            let keys = super::super::fetch::list_day(
                &client,
                &sources,
                &level_prefix_name(level),
                day,
                None,
            )
            .await
            .expect("the day lists");
            per_level.push(stamps_at_or_before(&keys, end));
        }

        let counts: Vec<usize> = per_level.iter().map(Vec::len).collect();
        let mut union: Vec<chrono::NaiveDateTime> = per_level.iter().flatten().copied().collect();
        union.sort_unstable();
        union.dedup();
        let exact: Vec<chrono::NaiveDateTime> = union
            .iter()
            .copied()
            .filter(|s| per_level.iter().all(|l| l.binary_search(s).is_ok()))
            .collect();
        let tolerant = timesteps(&per_level);
        let split = tolerant.iter().filter(|t| !t.is_aligned()).count();
        let tolerant_valids: Vec<chrono::NaiveDateTime> =
            tolerant.iter().map(StackStamps::valid).collect();

        let largest_gap = |series: &[chrono::NaiveDateTime]| -> i64 {
            series
                .windows(2)
                .map(|w| (w[1] - w[0]).num_seconds())
                .max()
                .unwrap_or(0)
        };
        let per_level_max = *counts.iter().max().unwrap();

        println!(
            "\n{day}\n\
               per-level granules     {}..{}\n\
               union of stamps        {}\n\
               exact intersection     {} — {} of {} unaddressable by one stamp ({:.1} %)\n\
               tolerant timesteps     {} ({} split across levels)\n\
               largest hole, exact    {} s\n\
               largest hole, tolerant {} s   (cadence ~120 s)",
            counts.iter().min().unwrap(),
            per_level_max,
            union.len(),
            exact.len(),
            per_level_max.saturating_sub(exact.len()),
            per_level_max,
            100.0 * (per_level_max.saturating_sub(exact.len())) as f64 / per_level_max as f64,
            tolerant.len(),
            split,
            largest_gap(&exact),
            largest_gap(&tolerant_valids),
        );

        // **The tolerance curve**: what each extra second of slack buys, and
        // where it stops buying anything. This is what picks
        // `STAMP_TOLERANCE_SECONDS` from data instead of from a guess.
        println!("  tolerance   timesteps   split   largest hole");
        for tolerance in [0i64, 1, 2, 3, 4, 5, 6, 8, 10, 15, 30] {
            let found = timesteps_within(&per_level, tolerance);
            let valids: Vec<chrono::NaiveDateTime> = found.iter().map(StackStamps::valid).collect();
            println!(
                "  {tolerance:>9}   {:>9}   {:>5}   {:>9} s",
                found.len(),
                found.iter().filter(|t| !t.is_aligned()).count(),
                largest_gap(&valids),
            );
        }

        // Shown rather than asserted: on a day where every level happened to
        // align there is nothing to show, and a hard assertion here would be a
        // claim about NOAA's scheduler rather than about this code.
        if let Some(t) = tolerant.iter().find(|t| !t.is_aligned()) {
            let early = t.stamps.iter().min().unwrap();
            let late = t.stamps.iter().max().unwrap();
            let n_early = t.stamps.iter().filter(|s| *s == early).count();
            println!(
                "  example split: {n_early} levels at {early}, {} at {late}",
                LEVEL_COUNT - n_early,
            );
        }

        // What *is* asserted holds on any day: the tolerance never loses a
        // timestep an exact match would have found, never invents one the union
        // does not contain, and never merges two scans.
        assert!(
            tolerant.len() >= exact.len(),
            "the tolerance found fewer timesteps than an exact match",
        );
        assert!(tolerant.len() <= union.len());
        assert!(
            tolerant_valids
                .windows(2)
                .all(|w| (w[1] - w[0]).num_seconds() > 60),
            "two scans were merged into one timestep",
        );
    }
}
