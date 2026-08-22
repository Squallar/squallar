use super::*;

/// Twelve verbatim lines from a live `hrrr.t14z.wrfsfcf00.grib2.idx`,
/// including the two shapes that break a naive parser: a level containing
/// spaces and parentheses (`PWAT`), and a variable whose name is itself a
/// colon-free description (`var discipline=0 ...`).
const SAMPLE_IDX: &str = "\
1:0:d=2026072514:REFC:entire atmosphere:anl:
2:300130:d=2026072514:RETOP:cloud top:anl:
3:499431:d=2026072514:var discipline=0 center=7 local_table=1 parmcat=16 parm=201:entire atmosphere:anl:
4:812221:d=2026072514:VIL:entire atmosphere:anl:
5:1064231:d=2026072514:VIS:surface:anl:
105:63110198:d=2026072514:CAPE:surface:anl:
106:63976324:d=2026072514:CIN:surface:anl:
107:64861905:d=2026072514:PWAT:entire atmosphere (considered as a single layer):anl:
131:94635452:d=2026072514:HLCY:3000-0 m above ground:anl:
132:95300000:d=2026072514:HLCY:1000-0 m above ground:anl:
145:99000000:d=2026072514:CAPE:180-0 mb above ground:anl:
146:99500000:d=2026072514:CIN:180-0 mb above ground:anl:
";

fn records() -> Vec<IdxRecord> {
    parse_idx(SAMPLE_IDX)
}

/// A whole `.idx` sidecar as S3 serves it —
/// `hrrr.20260820/conus/hrrr.t00z.wrfsfcf01.grib2.idx`, HTTP 200, 10 171 bytes,
/// fetched 2026-08-21. Committed rather than fetched at test time because the
/// bucket holds a rolling window and this file will be gone; the properties it
/// pins (which `(var, level)` pairs repeat, and what NCEP writes in the
/// forecast field at f01) are the ones the selection depends on.
const F01_IDX: &str = include_str!("../../../testdata/hrrr.20260820.t00z.wrfsfcf01.grib2.idx");

#[test]
fn an_idx_line_splits_into_number_offset_var_and_level() {
    let r = records();
    assert_eq!(r.len(), 12, "every fixture line must parse");

    assert_eq!(r[0].number, 1);
    assert_eq!(r[0].offset, 0);
    assert_eq!(r[0].var, "REFC");
    assert_eq!(r[0].level, "entire atmosphere");
    assert_eq!(r[0].forecast, "anl");

    let pwat = r.iter().find(|r| r.var == "PWAT").unwrap();
    assert_eq!(pwat.offset, 64_861_905);
    assert_eq!(
        pwat.level, "entire atmosphere (considered as a single layer)",
        "a level with spaces and parentheses must survive intact",
    );
}

#[test]
fn a_blank_or_malformed_idx_line_is_skipped_not_fatal() {
    assert!(parse_idx("").is_empty());
    assert!(parse_idx("\n\n").is_empty());
    assert!(parse_idx("garbage\n").is_empty());
    let mixed = format!("nonsense\n{}", SAMPLE_IDX.lines().next().unwrap());
    assert_eq!(parse_idx(&mixed).len(), 1);
}

#[test]
fn a_byte_range_ends_one_byte_before_the_next_record() {
    let (start, end) = byte_range(&records(), "CIN", "surface", None).unwrap();
    assert_eq!(start, 63_976_324);
    assert_eq!(end, Some(64_861_904));
    assert_eq!(end.unwrap() - start + 1, 64_861_905 - 63_976_324);
}

#[test]
fn a_byte_range_does_not_overlap_the_following_record() {
    let r = records();
    for pair in r.windows(2) {
        let (_, end) = byte_range(&r, &pair[0].var, &pair[0].level, None).unwrap();
        assert_eq!(
            end,
            Some(pair[1].offset - 1),
            "{}:{} must stop before {}:{}",
            pair[0].var,
            pair[0].level,
            pair[1].var,
            pair[1].level,
        );
    }
}

#[test]
fn the_final_records_range_is_open_ended() {
    let (start, end) = byte_range(&records(), "CIN", "180-0 mb above ground", None).unwrap();
    assert_eq!(start, 99_500_000);
    assert_eq!(end, None, "nothing in the index bounds the last record");
}

/// Matching on the variable alone is ambiguous: the fixture has two `CIN`,
/// two `CAPE` and two `HLCY` records at different levels.
#[test]
fn a_record_is_selected_by_variable_and_level_together() {
    let r = records();
    assert_eq!(
        byte_range(&r, "CIN", "surface", None).unwrap().0,
        63_976_324
    );
    assert_eq!(
        byte_range(&r, "CIN", "180-0 mb above ground", None)
            .unwrap()
            .0,
        99_500_000,
    );
    assert_eq!(
        byte_range(&r, "CAPE", "surface", None).unwrap().0,
        63_110_198
    );
    assert_eq!(
        byte_range(&r, "CAPE", "180-0 mb above ground", None)
            .unwrap()
            .0,
        99_000_000,
    );
    assert_eq!(
        byte_range(&r, "HLCY", "3000-0 m above ground", None)
            .unwrap()
            .0,
        94_635_452,
    );
    assert_eq!(
        byte_range(&r, "HLCY", "1000-0 m above ground", None)
            .unwrap()
            .0,
        95_300_000,
    );
}

#[test]
fn an_unmatched_variable_or_level_yields_no_range() {
    let r = records();
    assert_eq!(
        byte_range(&r, "CIN", "2000-5000 m above ground", None),
        None
    );
    assert_eq!(byte_range(&r, "NOSUCH", "surface", None), None);
    assert_eq!(
        byte_range(&r, "CIN", "Surface", None),
        None,
        "matching is exact"
    );
}

/// There is no rule to infer: HRRR orders layer bounds inconsistently between
/// fields — `HLCY:3000-0` and `MXUPHL:5000-2000` put the top first,
/// `VUCSH:0-6000` and `CAPE:0-3000 m` the bottom — and matching is literal.
const IDX_RECORDS: &[(ModelParameter, &str, &str)] = &[
    (ModelParameter::SurfaceBasedCin, "CIN", "surface"),
    (
        ModelParameter::MixedLayerCin,
        "CIN",
        "180-0 mb above ground",
    ),
    (ModelParameter::SurfaceBasedCape, "CAPE", "surface"),
    (
        ModelParameter::MixedLayerCape,
        "CAPE",
        "180-0 mb above ground",
    ),
    (
        ModelParameter::MostUnstableCape,
        "CAPE",
        "255-0 mb above ground",
    ),
    (ModelParameter::LiftedIndex, "LFTX", "500-1000 mb"),
    (ModelParameter::Srh1km, "HLCY", "1000-0 m above ground"),
    (ModelParameter::Srh3km, "HLCY", "3000-0 m above ground"),
    (
        ModelParameter::MaxUH2to5km,
        "MXUPHL",
        "5000-2000 m above ground",
    ),
    (
        ModelParameter::MaxUH0to2km,
        "MXUPHL",
        "2000-0 m above ground",
    ),
    (ModelParameter::SurfaceWindGust, "GUST", "surface"),
    (
        ModelParameter::PrecipitableWater,
        "PWAT",
        "entire atmosphere (considered as a single layer)",
    ),
    (ModelParameter::Temperature2m, "TMP", "2 m above ground"),
    (ModelParameter::Dewpoint2m, "DPT", "2 m above ground"),
    (ModelParameter::Visibility, "VIS", "surface"),
];

#[test]
fn every_parameter_selects_a_real_index_record() {
    for &(param, var, level) in IDX_RECORDS {
        assert_eq!(param.grib_var(), var, "{}", param.display_name());
        assert_eq!(param.grib_level(), level, "{}", param.display_name());
    }
}

#[test]
fn the_index_table_covers_every_non_composite_parameter() {
    for param in ModelParameter::all() {
        if param.is_composite() {
            continue;
        }
        assert!(
            IDX_RECORDS.iter().any(|&(p, _, _)| p == *param),
            "{} is not pinned to an index record",
            param.display_name(),
        );
    }
}

#[test]
fn composite_components_select_real_index_records() {
    let parts = ModelParameter::BulkShear6km.composite_parts().unwrap();
    let expected = [
        ("VUCSH", "0-6000 m above ground"),
        ("VVCSH", "0-6000 m above ground"),
    ];
    assert_eq!(parts.len(), expected.len());
    for (&(got_var, got_lev), (var, level)) in parts.iter().zip(expected) {
        assert_eq!(got_var, var);
        assert_eq!(got_lev, level);
    }
}

#[test]
fn no_two_parameters_select_the_same_index_record() {
    let mut seen = std::collections::HashSet::new();
    for &(param, var, level) in IDX_RECORDS {
        assert!(
            seen.insert((var, level)),
            "{} selects `{var}:{level}`, which another parameter already claims",
            param.display_name(),
        );
    }
}

/// f00 `MXUPHL` is a `0-0 day max fcst` — a maximum over a zero-length
/// window, which is identically 0.0 everywhere.
#[test]
fn uh_requests_a_forecast_hour_with_a_nonzero_window() {
    for param in [ModelParameter::MaxUH2to5km, ModelParameter::MaxUH0to2km] {
        assert!(
            param.min_forecast_hour() > 0,
            "{} must not come from f00: its accumulation window there has \
                 zero length and the field is constant 0.0",
            param.display_name(),
        );
        assert!(param.is_windowed());
    }
}

/// The floor's whole job: whatever hour a caller asks for, a windowed
/// parameter never reaches the network at f00, where its window has zero length
/// and the field is a constant 0.0 that draws as an empty map with no error.
#[test]
fn a_windowed_parameter_never_requests_f00() {
    let date = NaiveDate::from_ymd_opt(2026, 8, 20).unwrap();

    // Non-vacuity floor: some parameter must actually have a non-zero floor, or
    // "never f00" is satisfied by an empty set of windowed parameters.
    assert!(
        ModelParameter::all().iter().any(|p| p.is_windowed()),
        "there must be a windowed parameter for this to be about",
    );

    for param in ModelParameter::all() {
        for requested in 0..=48u8 {
            let effective = effective_forecast_hour(param, requested);
            assert!(
                effective >= requested,
                "{} at f{requested:02}: the floor may only raise, never lower",
                param.display_name(),
            );
            assert!(effective >= param.min_forecast_hour());
            if param.is_windowed() {
                assert_ne!(
                    effective,
                    0,
                    "{} must never be requested at f00",
                    param.display_name(),
                );
            }
        }
        // It is a floor, not a constant: every hour above it passes through
        // untouched, which is what makes a forecast scrub possible at all.
        for requested in 1..=48u8 {
            assert_eq!(
                effective_forecast_hour(param, requested),
                requested,
                "{} at f{requested:02} must not be moved",
                param.display_name(),
            );
        }
    }

    // And the raise reaches both things that name the hour on the wire: the
    // object key, and the record qualifier inside it.
    let uh = ModelParameter::MaxUH2to5km;
    let raised = effective_forecast_hour(&uh, 0);
    assert_eq!(raised, 1);
    assert!(DataSources::hrrr_key(&date, 0, raised).contains("wrfsfcf01.grib2"));
    assert_eq!(record_forecast(&uh, raised), "0-1 hour max fcst");
    assert_eq!(
        record_forecast(&uh, 0),
        "0-0 day max fcst",
        "the record the floor exists to avoid must still be nameable, or this \
         test would be asserting that f00 does not exist",
    );
}

#[test]
fn non_windowed_parameters_still_come_from_the_analysis() {
    for param in ModelParameter::all() {
        if param.is_windowed() {
            continue;
        }
        assert_eq!(
            param.min_forecast_hour(),
            0,
            "{} is instantaneous and should come from f00",
            param.display_name(),
        );
    }
}

/// Relaxing `count != 1` to `count < 1` lets two concatenated records decode as
/// a sequence with the first silently winning.
#[test]
fn only_a_single_submessage_is_accepted() {
    assert!(
        exactly_one_submessage(1).is_ok(),
        "one record must be accepted"
    );

    let none = exactly_one_submessage(0).expect_err("zero records must be refused");
    assert!(none.contains("found 0"), "{none}");

    let two = exactly_one_submessage(2).expect_err("two records must be refused");
    assert!(two.contains("found 2"), "{two}");
    assert!(two.contains("exactly one GRIB2 submessage"), "{two}");

    assert!(exactly_one_submessage(3).is_err());
}

#[test]
fn a_grid_point_count_must_match_what_section_three_declares() {
    assert!(check_point_count(1_905_141, 1_905_141).is_ok());

    let short = check_point_count(1_905_140, 1_905_141)
        .expect_err("a grid one point short must be refused");
    assert!(short.contains("1905141 declared"), "{short}");
    assert!(short.contains("1905140 computed"), "{short}");

    assert!(
        check_point_count(1_905_142, 1_905_141).is_err(),
        "a grid one point long must be refused too",
    );
}

/// 3 x 2 = 6 points, which keeps the whole message to 188 bytes — a real
/// HRRR record is ~1 MB, far too large to commit.
const SYNTHETIC_NI: u32 = 3;
const SYNTHETIC_NJ: u32 = 2;
const SYNTHETIC_POINTS: u32 = SYNTHETIC_NI * SYNTHETIC_NJ;

/// Byte offset of section 3's `numberOfDataPoints`: section 0 (16) + section 1
/// (21) + section 3's 5-byte header + its 1-byte source-of-grid.
const SECT3_NUM_POINTS_OFFSET: usize = 16 + 21 + 5 + 1;

fn grib_section(number: u8, body: &[u8]) -> Vec<u8> {
    let mut out = ((body.len() + 5) as u32).to_be_bytes().to_vec();
    out.push(number);
    out.extend_from_slice(body);
    out
}

/// A whole GRIB2 message: a 3 x 2 Lambert-conformal grid (template 3.30)
/// carrying a constant field (DRT 5.0, `nbits = 0`).
///
/// `declared_points` is section 3's `numberOfDataPoints`, which grib reads
/// verbatim rather than as `ni * nj`.
fn synthetic_lambert_grib2(declared_points: u32) -> Vec<u8> {
    synthetic_lambert_grib2_at(declared_points, 262_500_000)
}

/// `lon0` is written to **both** `Lo1` and `LoV`, so the grid always sits on its
/// own central meridian; moving only the first point would be refused for
/// leaving the cone's principal sector instead.
fn synthetic_lambert_grib2_at(declared_points: u32, lon0: u32) -> Vec<u8> {
    let mut sect1 = Vec::new();
    sect1.extend_from_slice(&7u16.to_be_bytes()); // centre: NCEP
    sect1.extend_from_slice(&0u16.to_be_bytes()); // subcentre
    sect1.push(2); // master table version
    sect1.push(1); // local table version
    sect1.push(0); // significance of reference time: analysis
    sect1.extend_from_slice(&2026u16.to_be_bytes());
    sect1.extend_from_slice(&[7, 25, 14, 0, 0]); // month, day, hour, min, sec
    sect1.push(0); // production status: operational
    sect1.push(1); // type of data: forecast

    let mut sect3 = Vec::new();
    sect3.push(0); // source of grid definition: the template below
    sect3.extend_from_slice(&declared_points.to_be_bytes()); // ← perturbed
    sect3.push(0); // no optional list of numbers of points
    sect3.push(0); // ...so nothing to interpret
    sect3.extend_from_slice(&30u16.to_be_bytes()); // template 3.30
    sect3.push(6); // Code Table 3.2 value 6: sphere, radius 6371229 m
    sect3.push(0);
    sect3.extend_from_slice(&0u32.to_be_bytes()); // spherical radius
    sect3.push(0);
    sect3.extend_from_slice(&0u32.to_be_bytes()); // major axis
    sect3.push(0);
    sect3.extend_from_slice(&0u32.to_be_bytes()); // minor axis
    sect3.extend_from_slice(&SYNTHETIC_NI.to_be_bytes());
    sect3.extend_from_slice(&SYNTHETIC_NJ.to_be_bytes());
    sect3.extend_from_slice(&38_500_000i32.to_be_bytes()); // La1
    sect3.extend_from_slice(&lon0.to_be_bytes()); // Lo1
    sect3.push(0b0000_1000); // resolution and component flags
    sect3.extend_from_slice(&38_500_000i32.to_be_bytes()); // LaD
    sect3.extend_from_slice(&lon0.to_be_bytes()); // LoV
    sect3.extend_from_slice(&3_000_000u32.to_be_bytes()); // Dx, mm
    sect3.extend_from_slice(&3_000_000u32.to_be_bytes()); // Dy, mm
    sect3.push(0); // projection centre: north pole, one cone
    sect3.push(0b0100_0000); // +i, +j, i-consecutive, no alternating rows
    sect3.extend_from_slice(&38_500_000i32.to_be_bytes()); // Latin1
    sect3.extend_from_slice(&38_500_000i32.to_be_bytes()); // Latin2
    sect3.extend_from_slice(&0i32.to_be_bytes()); // southern pole lat
    sect3.extend_from_slice(&0u32.to_be_bytes()); // southern pole lon

    let mut sect4 = Vec::new();
    sect4.extend_from_slice(&0u16.to_be_bytes()); // no coordinate values
    sect4.extend_from_slice(&0u16.to_be_bytes()); // template 4.0
    sect4.push(7); // parameter category: thermodynamic stability indices
    sect4.push(7); // parameter number: CIN
    sect4.push(2); // type of generating process: forecast
    sect4.push(0); // background process
    sect4.push(83); // generating process identifier
    sect4.extend_from_slice(&0u16.to_be_bytes()); // hours after cutoff
    sect4.push(0); // minutes after cutoff
    sect4.push(1); // indicator of unit of time range: hour
    sect4.extend_from_slice(&0u32.to_be_bytes()); // forecast time
    sect4.push(1); // first fixed surface: ground or water surface
    sect4.push(0); // scale factor
    sect4.extend_from_slice(&0u32.to_be_bytes()); // scaled value
    sect4.push(255); // no second fixed surface
    sect4.push(0);
    sect4.extend_from_slice(&0u32.to_be_bytes());

    let mut sect5 = Vec::new();
    sect5.extend_from_slice(&SYNTHETIC_POINTS.to_be_bytes());
    sect5.extend_from_slice(&0u16.to_be_bytes()); // template 5.0
    sect5.extend_from_slice(&(-75.0f32).to_be_bytes()); // reference value
    sect5.extend_from_slice(&0i16.to_be_bytes()); // binary scale factor
    sect5.extend_from_slice(&0i16.to_be_bytes()); // decimal scale factor
    sect5.push(0); // 0 bits/value: every point is the reference value
    sect5.push(0); // original field values are floating point

    let mut body = grib_section(1, &sect1);
    body.extend(grib_section(3, &sect3));
    body.extend(grib_section(4, &sect4));
    body.extend(grib_section(5, &sect5));
    body.extend(grib_section(6, &[255])); // no bitmap
    body.extend(grib_section(7, &[])); // no data, nbits is 0
    body.extend_from_slice(b"7777"); // section 8

    let mut message = b"GRIB".to_vec();
    message.extend_from_slice(&[0, 0]); // reserved
    message.push(0); // discipline: meteorological products
    message.push(2); // GRIB edition 2
    let total = (message.len() + 8 + body.len()) as u64;
    message.extend_from_slice(&total.to_be_bytes());
    message.extend(body);
    message
}

fn grid_coords_of(bytes: &[u8]) -> Result<GridCoords, String> {
    let grib2 = grib::from_reader(std::io::Cursor::new(bytes))
        .map_err(|e| format!("GRIB2 parse error: {e}"))?;
    let (_index, submessage) = grib2
        .iter()
        .next()
        .ok_or_else(|| "no submessages in the synthetic message".to_string())?;
    grid_coords(&submessage)
}

#[test]
fn a_synthetic_lambert_message_decodes_through_the_real_parse_path() {
    let bytes = synthetic_lambert_grib2(SYNTHETIC_POINTS);
    assert_eq!(bytes.len(), 188, "the fixture should be 188 bytes");
    assert_eq!(&bytes[..4], b"GRIB");
    assert_eq!(&bytes[bytes.len() - 4..], b"7777");
    assert_eq!(
        u32::from_be_bytes(
            bytes[SECT3_NUM_POINTS_OFFSET..SECT3_NUM_POINTS_OFFSET + 4]
                .try_into()
                .unwrap(),
        ),
        SYNTHETIC_POINTS,
        "SECT3_NUM_POINTS_OFFSET must land on section 3's declared count",
    );

    let grid = parse_grib2(&bytes, ModelParameter::SurfaceBasedCin, 0)
        .expect("the synthetic message must decode end to end");
    assert_eq!((grid.ni, grid.nj), (3, 2));
    assert_eq!(grid.values.len(), SYNTHETIC_POINTS as usize);
    assert_eq!(grid.coords.len(), SYNTHETIC_POINTS as usize);
    assert!(
        matches!(grid.coords, GridCoords::Lambert(_)),
        "template 3.30 must take the lambert branch of grid_coords",
    );
    assert!(
        grid.values.iter().all(|&v| v == -75.0),
        "nbits = 0 is a constant field at the reference value: {:?}",
        grid.values,
    );
    assert_eq!(grid.ref_time.to_string(), "2026-07-25 14:00:00");
}

/// The `?` on `check_point_count` inside [`grid_coords`]. Dropping it lays the
/// values out over the wrong coordinates. The two fixtures differ in exactly the
/// four bytes of section 3's `numberOfDataPoints`.
#[test]
fn a_declared_point_count_that_disagrees_with_the_grid_is_refused() {
    let good = synthetic_lambert_grib2(SYNTHETIC_POINTS);
    let bad = synthetic_lambert_grib2(SYNTHETIC_POINTS + 1);
    assert_eq!(good.len(), bad.len());

    let differing: Vec<usize> = (0..good.len()).filter(|&i| good[i] != bad[i]).collect();
    let field = SECT3_NUM_POINTS_OFFSET..SECT3_NUM_POINTS_OFFSET + 4;
    assert!(
        !differing.is_empty() && differing.iter().all(|i| field.contains(i)),
        "the fixtures must differ only inside {field:?}, but differ at {differing:?}",
    );

    assert!(
        matches!(grid_coords_of(&good), Ok(GridCoords::Lambert(_))),
        "the control fixture must reach the Lambert branch: {:?}",
        grid_coords_of(&good).err(),
    );

    let err = grid_coords_of(&bad).expect_err("the mismatch must be refused");
    assert!(err.contains("Lambert grid point count mismatch"), "{err}");
    assert!(err.contains("7 declared"), "{err}");
    assert!(err.contains("6 computed"), "{err}");

    let err = parse_grib2(&bad, ModelParameter::SurfaceBasedCin, 0)
        .expect_err("parse_grib2 must refuse it too");
    assert!(err.contains("Lambert grid point count mismatch"), "{err}");
}

/// Four verbatim lines from `hrrr.t14z.wrfsfcf01.grib2.idx` where `(var, level)`
/// repeats: taking record 8 where a caller wanted record 44 swaps an
/// instantaneous field for a windowed maximum with no error.
///
/// **This documents the unqualified path, which is now a deliberate opt-out
/// rather than the only behaviour available.** The two offsets below are the
/// ones this test has always asserted and they have not moved — `None` still
/// resolves to the lowest-numbered `(var, level)` hit, still silently. What
/// changed is that production no longer takes it: see
/// [`a_forecast_qualifier_selects_between_duplicate_var_level_pairs`] for the
/// path it takes instead, over the same two repeated pairs in a real index.
#[test]
fn a_repeated_var_and_level_resolves_to_the_lowest_numbered_record() {
    const AMBIGUOUS: &str = "\
8:2668643:d=2026072514:REFD:263 K level:1 hour fcst:
44:27615521:d=2026072514:REFD:263 K level:0-1 hour max fcst:
68:42378051:d=2026072514:WEASD:surface:1 hour fcst:
85:58942796:d=2026072514:WEASD:surface:0-1 hour acc fcst:
";
    let records = parse_idx(AMBIGUOUS);
    assert_eq!(records.len(), 4);

    assert_eq!(records[0].forecast, "1 hour fcst");
    assert_eq!(records[1].forecast, "0-1 hour max fcst");

    let (start, _) = byte_range(&records, "REFD", "263 K level", None).unwrap();
    assert_eq!(start, 2_668_643, "the first match must win, i.e. record 8");
    let (start, _) = byte_range(&records, "WEASD", "surface", None).unwrap();
    assert_eq!(
        start, 42_378_051,
        "the first match must win, i.e. record 68"
    );

    // The half that is new: the same two pairs, qualified, reach the record the
    // positional tie-break skipped. Without this the fix would be untested by
    // the very fixture that motivated it.
    let (start, _) =
        byte_range(&records, "REFD", "263 K level", Some("0-1 hour max fcst")).unwrap();
    assert_eq!(start, 27_615_521, "record 44, the one position 0 hid");
    let (start, _) = byte_range(&records, "WEASD", "surface", Some("0-1 hour acc fcst")).unwrap();
    assert_eq!(start, 58_942_796, "record 85, the one position 0 hid");
}

/// A whole real index — 170 records of `hrrr.20260820/conus/hrrr.t00z.
/// wrfsfcf01.grib2.idx`, fetched 2026-08-21 and committed verbatim — rather
/// than four hand-picked lines, so the selection is exercised against NCEP's
/// actual record ordering and its actual forecast vocabulary.
#[test]
fn a_forecast_qualifier_selects_between_duplicate_var_level_pairs() {
    let records = parse_idx(F01_IDX);
    assert_eq!(records.len(), 170, "every fixture line must parse");

    // Non-vacuity floor: the fixture has to *contain* the ambiguity, or every
    // assertion below passes for the wrong reason. These are the only two pairs
    // that repeat in this file, and each repeats exactly twice.
    let repeats: Vec<(&str, &str)> = records
        .iter()
        .filter(|r| {
            records
                .iter()
                .filter(|o| o.var == r.var && o.level == r.level)
                .count()
                > 1
        })
        .map(|r| (r.var.as_str(), r.level.as_str()))
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();
    assert_eq!(
        repeats,
        vec![("REFD", "263 K level"), ("WEASD", "surface")],
        "the fixture must carry the duplicates this function exists to resolve",
    );

    // Every distinct qualifier resolves to a *different* record, and to the one
    // whose number the index actually gives it.
    for (var, level, forecast, number) in [
        ("REFD", "263 K level", "1 hour fcst", 8),
        ("REFD", "263 K level", "0-1 hour max fcst", 44),
        ("WEASD", "surface", "1 hour fcst", 68),
        ("WEASD", "surface", "0-1 hour acc fcst", 85),
    ] {
        let want = records
            .iter()
            .find(|r| r.number == number)
            .unwrap_or_else(|| panic!("record {number} must exist in the fixture"));
        let (start, _) = byte_range(&records, var, level, Some(forecast))
            .unwrap_or_else(|| panic!("{var}:{level}:{forecast} must resolve"));
        assert_eq!(
            start, want.offset,
            "{var}:{level}:{forecast} must land on record {number}",
        );
    }

    // The unqualified call cannot tell those four apart: it answers the same
    // offset for both members of each pair. This is the assertion that makes
    // the four above mean something rather than merely agree with themselves.
    for (var, level) in [("REFD", "263 K level"), ("WEASD", "surface")] {
        let unqualified = byte_range(&records, var, level, None).unwrap().0;
        let qualified: Vec<u64> = records
            .iter()
            .filter(|r| r.var == var && r.level == level)
            .map(|r| {
                byte_range(&records, var, level, Some(&r.forecast))
                    .unwrap()
                    .0
            })
            .collect();
        assert_eq!(qualified.len(), 2);
        assert_ne!(qualified[0], qualified[1], "{var}:{level}");
        assert!(
            qualified.contains(&unqualified),
            "{var}:{level}: the unqualified answer must still be one of them",
        );
    }

    // A qualifier that names no record refuses rather than falling back to the
    // positional hit — a silent fallback would be the original bug wearing the
    // new signature.
    assert_eq!(
        byte_range(&records, "REFD", "263 K level", Some("0-1 hour acc fcst")),
        None,
        "a qualifier that matches nothing must not degrade to the first hit",
    );
}

/// The qualifier production builds, against the real index it must match. Its
/// grammar is NCEP's, not ours, and getting it wrong fails the fetch outright
/// now that the match is exact — so it is pinned against the file.
#[test]
fn the_forecast_qualifier_names_the_record_the_index_actually_carries() {
    let records = parse_idx(F01_IDX);

    for param in ModelParameter::all() {
        let pairs = param
            .composite_parts()
            .unwrap_or_else(|| vec![(param.grib_var(), param.grib_level())]);
        let forecast = record_forecast(param, 1);
        for (var, level) in pairs {
            let hit = records
                .iter()
                .find(|r| r.var == var && r.level == level && r.forecast == forecast);
            assert!(
                hit.is_some(),
                "{} wants `{var}:{level}:{forecast}` at f01, which this index \
                 does not carry; candidates: {:?}",
                param.display_name(),
                records
                    .iter()
                    .filter(|r| r.var == var && r.level == level)
                    .map(|r| &r.forecast)
                    .collect::<Vec<_>>(),
            );
        }
    }

    // The f00 wording is `day`, not `hour`, and is not derivable from the
    // hourly form — the one place this grammar is genuinely irregular.
    assert_eq!(record_forecast(&ModelParameter::SurfaceBasedCin, 0), "anl");
    assert_eq!(
        record_forecast(&ModelParameter::MaxUH2to5km, 0),
        "0-0 day max fcst",
    );
    assert_eq!(
        record_forecast(&ModelParameter::MaxUH2to5km, 18),
        "17-18 hour max fcst",
    );
    assert_eq!(
        record_forecast(&ModelParameter::SurfaceBasedCin, 18),
        "18 hour fcst",
    );
}

/// `hour` is a `u8` and `latest_available_run` returns 0 for the whole
/// 02:00-02:59 UTC hour, so an unguarded `hour - 1` is a daily debug panic
/// and a wrap to run "255z" in release.
#[test]
fn the_previous_run_rolls_back_over_midnight() {
    let day = NaiveDate::from_ymd_opt(2026, 7, 25).unwrap();
    let before = NaiveDate::from_ymd_opt(2026, 7, 24).unwrap();
    assert_eq!(
        previous_run(day, 0),
        (before, 23),
        "00Z must fall back to 23Z yesterday"
    );
    assert_eq!(previous_run(day, 1), (day, 0));
    assert_eq!(previous_run(day, 14), (day, 13));
    let first = NaiveDate::from_ymd_opt(2026, 8, 1).unwrap();
    assert_eq!(
        previous_run(first, 0),
        (NaiveDate::from_ymd_opt(2026, 7, 31).unwrap(), 23)
    );
}

/// Every instant here is fixed. `latest_available_run` used to read `Utc::now()`
/// inside itself, which left a test nothing to check the answer against except
/// the same clock — a comparison that agrees no matter what the offset is.
#[test]
fn run_for_is_two_hours_behind_the_clock() {
    let at = |y, m, d, h, min| {
        NaiveDate::from_ymd_opt(y, m, d)
            .unwrap()
            .and_hms_opt(h, min, 0)
            .unwrap()
    };
    let day = |y, m, d| NaiveDate::from_ymd_opt(y, m, d).unwrap();

    // Mid-day: two whole hours back, minutes discarded rather than rounded.
    assert_eq!(run_for(at(2026, 8, 20, 14, 0)), (day(2026, 8, 20), 12));
    assert_eq!(run_for(at(2026, 8, 20, 14, 59)), (day(2026, 8, 20), 12));
    assert_eq!(run_for(at(2026, 8, 20, 15, 0)), (day(2026, 8, 20), 13));

    // Back over midnight — the case that produces run hour 0, and so the case
    // `previous_run`'s u8 guard exists for.
    assert_eq!(run_for(at(2026, 8, 20, 1, 30)), (day(2026, 8, 19), 23));
    assert_eq!(run_for(at(2026, 8, 20, 0, 0)), (day(2026, 8, 19), 22));
    assert_eq!(run_for(at(2026, 8, 20, 2, 0)), (day(2026, 8, 20), 0));

    // Back over a month boundary, and over a leap day.
    assert_eq!(run_for(at(2026, 9, 1, 0, 15)), (day(2026, 8, 31), 22));
    assert_eq!(run_for(at(2024, 3, 1, 1, 0)), (day(2024, 2, 29), 23));

    // The property, over a full day at minute granularity rather than at the
    // eight points above: the answer is always exactly two hours behind.
    let start = at(2026, 8, 20, 0, 0);
    for minutes in 0..(24 * 60) {
        let now = start + chrono::Duration::minutes(minutes);
        let expected = now - chrono::Duration::hours(2);
        assert_eq!(
            run_for(now),
            (expected.date(), expected.time().hour() as u8),
            "run_for({now}) must be the 2 h-old instant's date and hour",
        );
    }

    // The wrapper must be `run_for` against the clock and nothing else. Bracket
    // the call rather than compare against a second clock read: the two reads
    // straddle an hour boundary once an hour otherwise, which is a flake, not a
    // finding. `before`/`after` are at most microseconds apart, so this admits
    // exactly one or two candidate runs and rejects any other offset.
    let before = Utc::now().naive_utc();
    let live = latest_available_run();
    let after = Utc::now().naive_utc();
    assert!(
        live == run_for(before) || live == run_for(after),
        "latest_available_run gave {live:?}, which is neither run_for({before}) \
         = {:?} nor run_for({after}) = {:?}",
        run_for(before),
        run_for(after),
    );
}

/// The forecast hour must reach the object key; if it does not, the UH fix
/// silently reverts to the constant-zero f00 record.
#[test]
fn the_object_key_carries_the_parameters_forecast_hour() {
    let date = NaiveDate::from_ymd_opt(2026, 7, 25).unwrap();
    let key = |p: ModelParameter| DataSources::hrrr_key(&date, 3, p.min_forecast_hour());
    assert!(key(ModelParameter::MaxUH2to5km).contains("wrfsfcf01.grib2"));
    assert!(key(ModelParameter::MaxUH0to2km).contains("wrfsfcf01.grib2"));
    assert!(key(ModelParameter::SurfaceBasedCin).contains("wrfsfcf00.grib2"));
}

/// The invariant the whole selection rests on, and a property of NCEP's index
/// rather than of rustdar's code: the `(var, level, forecast)` triple
/// [`byte_range`] is given must name exactly one record. One match too few is a
/// loud fetch failure; two is a silent wrong-field read.
///
/// Walks more than the two hours the parameters' floors sit at, because the
/// forecast qualifier's grammar changes with the hour and f01 is the only hour
/// the committed fixture covers.
///
/// `cargo test -p rustdar-overlays -- --ignored --nocapture live_every_parameter`
#[tokio::test]
#[ignore = "hits the live noaa-hrrr-bdp-pds S3 bucket"]
async fn live_every_parameter_selects_exactly_one_record() {
    let client = hrrr_client().expect("client");
    let sources = DataSources::production();
    let (date, hour) = latest_available_run();

    for forecast_hour in [0u8, 1, 2, 6, 18] {
        let url = sources.hrrr_idx_url(&date, hour, forecast_hour);
        let text = match client
            .get(&url)
            .send()
            .await
            .and_then(|r| r.error_for_status())
        {
            Ok(r) => r.text().await.expect("index body"),
            Err(_) => {
                let (prev_date, prev_hour) = previous_run(date, hour);
                client
                    .get(sources.hrrr_idx_url(&prev_date, prev_hour, forecast_hour))
                    .send()
                    .await
                    .expect("index request")
                    .error_for_status()
                    .expect("index status")
                    .text()
                    .await
                    .expect("index body")
            }
        };
        let records = parse_idx(&text);
        assert!(
            records.len() > 100,
            "f{forecast_hour:02} index parsed to {} records",
            records.len()
        );

        let repeated = records
            .iter()
            .filter(|r| {
                records
                    .iter()
                    .filter(|o| o.var == r.var && o.level == r.level)
                    .count()
                    > 1
            })
            .map(|r| format!("{}:{}", r.var, r.level))
            .collect::<std::collections::BTreeSet<_>>();
        println!(
            "f{forecast_hour:02}: {} records, repeated pairs: {repeated:?}",
            records.len()
        );

        for param in ModelParameter::all() {
            // The hour the fetch would really use, so a windowed parameter is
            // checked at f01 rather than at the f00 it can never ask for.
            if effective_forecast_hour(param, forecast_hour) != forecast_hour {
                continue;
            }
            let forecast = record_forecast(param, forecast_hour);
            let pairs = param
                .composite_parts()
                .unwrap_or_else(|| vec![(param.grib_var(), param.grib_level())]);
            for (var, level) in pairs {
                let matches = records
                    .iter()
                    .filter(|r| r.var == var && r.level == level && r.forecast == forecast)
                    .collect::<Vec<_>>();
                assert_eq!(
                    matches.len(),
                    1,
                    "{} selects {} record(s) for {var}:{level}:{forecast} in \
                         f{forecast_hour:02} — 0 is a fetch failure, 2 is a silent \
                         wrong-field read. Same (var, level) at any qualifier: {:?}",
                    param.display_name(),
                    matches.len(),
                    records
                        .iter()
                        .filter(|r| r.var == var && r.level == level)
                        .map(|r| (r.number, &r.forecast))
                        .collect::<Vec<_>>(),
                );
            }
        }
    }
}

/// The full S3 path end to end, decoding the operational DRT 5.3 bytes S3 serves.
///
/// `cargo test -p rustdar-overlays -- --ignored --nocapture live_hrrr`
#[tokio::test]
#[ignore = "hits the live noaa-hrrr-bdp-pds S3 bucket"]
async fn live_hrrr_fetches_and_decodes_from_s3() {
    let client = hrrr_client().expect("client");
    let sources = DataSources::production();
    let run = latest_available_run();

    // f00 for the instantaneous fields; the windowed one is raised to f01 by
    // the floor rather than by this list, which is the point of the floor.
    for param in [
        ModelParameter::SurfaceBasedCape,
        ModelParameter::MixedLayerCin,
        ModelParameter::PrecipitableWater,
        ModelParameter::MaxUH2to5km,
    ] {
        let grid = match fetch_hrrr_data(&client, &sources, &param, run, 0).await.0 {
            Ok(g) => g,
            Err(e) => panic!("{} fetch failed: {e}", param.display_name()),
        };
        let (lo, hi) = grid.value_range.expect("finite values");
        println!(
            "{}: f{:02}, {}x{} = {} pts, range {lo}..{hi}, {} visible, ref {}",
            param.display_name(),
            grid.forecast_hour,
            grid.ni,
            grid.nj,
            grid.values.len(),
            grid.visible_points,
            grid.ref_time,
        );

        assert_eq!(grid.ni, 1799, "{}", param.display_name());
        assert_eq!(grid.nj, 1059, "{}", param.display_name());
        assert_eq!(grid.values.len(), 1_905_141, "{}", param.display_name());
        assert_eq!(grid.coords.len(), grid.values.len());
        assert!(
            matches!(grid.coords, GridCoords::Lambert(_)),
            "{} must take the lazy 3.30 path, not materialise 30 MB",
            param.display_name(),
        );

        assert!(
            lo < hi,
            "{} decoded as a constant field ({lo})",
            param.display_name(),
        );

        // Deliberately NOT `assert!(grid.blank_notice().is_none())`: that asserts
        // on the weather. On a quiet day Max UH 2-5 km peaks below `uh_color`'s
        // lowest threshold of 25 m²/s² and this went red with nothing broken.
        if let Some(notice) = grid.blank_notice() {
            println!("  {notice}");
        }

        assert!(
            grid.bounds.min_lat < 25.0 && grid.bounds.max_lat > 47.0,
            "{} bounds {:?} do not span CONUS",
            param.display_name(),
            grid.bounds,
        );
    }
}

#[tokio::test]
#[ignore = "hits the live noaa-hrrr-bdp-pds S3 bucket"]
async fn live_hrrr_composite_merges_two_ranged_records() {
    let client = hrrr_client().expect("client");
    let sources = DataSources::production();
    let param = ModelParameter::BulkShear6km;

    let grid = match fetch_composite_hrrr_data(&client, &sources, &param, latest_available_run(), 0)
        .await
        .0
    {
        Ok(g) => g,
        Err(e) => panic!("bulk shear fetch failed: {e}"),
    };
    let (lo, hi) = grid.value_range.expect("finite values");
    println!("bulk shear: {} pts, range {lo}..{hi}", grid.values.len());
    assert_eq!(grid.values.len(), 1_905_141);
    assert!(lo >= 0.0, "a vector magnitude cannot be negative, got {lo}");
    assert!(hi > 0.0);
}

/// The guard that makes a byte-range arithmetic bug loud. Built from a live
/// record rather than a committed fixture (one HRRR record is ~1 MB).
#[tokio::test]
#[ignore = "hits the live noaa-hrrr-bdp-pds S3 bucket"]
async fn live_parse_grib2_refuses_more_than_one_submessage() {
    let client = hrrr_client().expect("client");
    let sources = DataSources::production();
    let (date, hour) = latest_available_run();

    let one = match fetch_record(&client, &sources, (date, hour), 0, "CIN", "surface", "anl").await
    {
        Ok(b) => b,
        Err(_) => {
            let (prev_date, prev_hour) = previous_run(date, hour);
            fetch_record(
                &client,
                &sources,
                (prev_date, prev_hour),
                0,
                "CIN",
                "surface",
                "anl",
            )
            .await
            .expect("CIN fetch")
        }
    };

    let single = parse_grib2(&one, ModelParameter::SurfaceBasedCin, 0);
    assert!(single.is_ok(), "a single record must decode: {single:?}");

    let mut two = one.clone();
    two.extend_from_slice(&one);
    let err = parse_grib2(&two, ModelParameter::SurfaceBasedCin, 0)
        .expect_err("two records must be refused, not silently truncated");
    println!("two-record error: {err}");
    assert!(
        err.contains("exactly one GRIB2 submessage"),
        "expected the one-submessage guard to fire, got: {err}",
    );
}

/// Dropping the `Range` header leaves everything above passing while
/// transferring ~130 MB per field, so the size is measured.
#[tokio::test]
#[ignore = "hits the live noaa-hrrr-bdp-pds S3 bucket"]
async fn live_a_ranged_record_is_a_small_fraction_of_the_file() {
    let client = hrrr_client().expect("client");
    let sources = DataSources::production();
    let (date, hour) = latest_available_run();

    // `match`, not `.or(..await)`: `Result::or` takes its argument by value, so
    // the fallback future was awaited unconditionally.
    let bytes =
        match fetch_record(&client, &sources, (date, hour), 0, "CIN", "surface", "anl").await {
            Ok(b) => b,
            Err(_) => {
                let (prev_date, prev_hour) = previous_run(date, hour);
                fetch_record(
                    &client,
                    &sources,
                    (prev_date, prev_hour),
                    0,
                    "CIN",
                    "surface",
                    "anl",
                )
                .await
                .expect("CIN fetch")
            }
        };

    println!("surface CIN record: {} bytes", bytes.len());
    // Operational record ~1.03 MB. Bounded clear of a whole file (~130 MB).
    assert!(
        (100_000..8_000_000).contains(&bytes.len()),
        "{} bytes is not a single GRIB2 record",
        bytes.len(),
    );
    assert_eq!(
        &bytes[..4],
        b"GRIB",
        "range did not start at a record boundary"
    );
}

/// **The permanently-moved-product test.** A single run 404ing is routine — it
/// is why the previous-hour fallback exists. Both runs 404ing is not: the bucket
/// carries a rolling window and should always have one of the last two hours.
/// Left `Absent` that state is invisible for ever; escalated to `Transient` it
/// costs the same one poll an hour and shows up in the layer's own panel.
#[test]
fn both_candidate_runs_missing_is_not_routine() {
    use crate::fetch_policy::{FetchError, FetchFailure, NotFound};

    let absent = || {
        FetchError::from_status(
            reqwest::StatusCode::NOT_FOUND,
            NotFound::IsRoutine,
            "index …wrfsfcf00.grib2.idx: HTTP 404 Not Found",
        )
    };
    assert_eq!(
        absent().failure,
        FetchFailure::Absent,
        "premise: one run's 404 is routine on its own",
    );

    let round = round_verdict([absent(), absent()], "HRRR fetch failed");
    assert_eq!(
        round.failure,
        FetchFailure::Transient,
        "a product missing from both candidate runs polls hourly and reports \
         nothing at all if this stays Absent",
    );
    assert!(
        round.message.contains("404"),
        "the round must keep the origin's own words: {}",
        round.message,
    );
}

#[test]
fn a_two_run_round_is_refused_only_when_both_runs_were() {
    use crate::fetch_policy::{FetchError, FetchFailure};

    let cases = [
        (
            FetchError::permanent("400"),
            FetchError::permanent("400"),
            FetchFailure::Permanent,
        ),
        (
            FetchError::permanent("400"),
            FetchError::transient("500"),
            FetchFailure::Transient,
        ),
        (
            FetchError::transient("500"),
            FetchError::transient("500"),
            FetchFailure::Transient,
        ),
    ];
    for (first, second, expected) in cases {
        assert_eq!(
            round_verdict([first, second], "HRRR fetch failed").failure,
            expected,
        );
    }
}

/// The operational domain passes, and passes with room to spare: a refusal that
/// refuses everything is not a guard, it is an outage.
#[test]
fn the_conus_domain_the_app_actually_fetches_is_accepted() {
    let conus = GeoBounds {
        min_lat: 21.1,
        max_lat: 52.7,
        min_lon: -134.0955,
        max_lon: -60.9172,
    };
    assert!(
        check_domain_longitude(&conus, &HRRR_DOMAIN_LON, "HRRR").is_ok(),
        "the only domain the app fetches must not be refused"
    );
    assert!(conus.min_lon - *HRRR_DOMAIN_LON.start() > 5.0);
    assert!(180.0 - conus.min_lon.abs() > 40.0);
}

/// A domain east of the antimeridian is refused — the Guam shape, where every
/// longitude is positive and a turn from the viewport.
#[test]
fn an_east_hemisphere_domain_is_refused() {
    let seam_parked = GeoBounds {
        min_lat: 30.0,
        max_lat: 33.0,
        min_lon: 173.4773,
        max_lon: 176.7655,
    };
    let err =
        check_domain_longitude(&seam_parked, &HRRR_DOMAIN_LON, "HRRR").expect_err("must refuse");
    assert!(
        err.contains("173.4773"),
        "the message must name the extent it saw"
    );
}

#[test]
fn a_straddling_domain_is_refused() {
    let straddling = GeoBounds {
        min_lat: 50.0,
        max_lat: 55.0,
        min_lon: -179.8,
        max_lon: 179.6,
    };
    assert!(check_domain_longitude(&straddling, &HRRR_DOMAIN_LON, "HRRR").is_err());
}

#[test]
fn the_refusal_names_the_decision_and_where_the_evidence_is() {
    let err = check_domain_longitude(
        &GeoBounds {
            min_lat: 50.0,
            max_lat: 55.0,
            min_lon: 170.0,
            max_lon: 178.0,
        },
        &HRRR_DOMAIN_LON,
        "HRRR",
    )
    .expect_err("must refuse");

    for owed in [
        "not a decode failure",
        "3294",
        "rigid whole-grid shift",
        "per-point shift",
        "wraps_longitude",
        "neighbours' pixel spacing",
        "campaigns/overlays/t17/",
        "HRRR_DOMAIN_LON",
    ] {
        assert!(
            err.contains(owed),
            "the refusal must mention {owed:?}; it said:\n{err}"
        );
    }
}

/// The sentinel is the one `parse_grib2`'s own bounds walk leaves behind, and it
/// is interesting precisely because `f64::MAX` is finite: an `is_finite` test
/// alone passes it through to a domain message quoting a 309-digit longitude.
#[test]
fn an_unwalkable_extent_is_refused_as_itself() {
    let err = check_domain_longitude(
        &GeoBounds {
            min_lat: 0.0,
            max_lat: 0.0,
            min_lon: f64::MAX,
            max_lon: f64::MIN,
        },
        &HRRR_DOMAIN_LON,
        "HRRR",
    )
    .expect_err("must refuse");
    assert!(
        err.contains("inverted"),
        "a sentinel extent must not be reported as a straddling domain; it said:\n{err}"
    );
    assert!(
        !err.contains("campaigns/overlays/t17/"),
        "a decode failure must not send the reader to the domain decision"
    );
}

/// The tests above call `check_domain_longitude` directly, so deleting its one
/// call site would leave them all green while the guard stopped running. This
/// drives a whole synthetic message through the real parse path, whose CONUS
/// spelling is known to decode.
#[test]
fn parse_grib2_refuses_a_domain_outside_the_validated_envelope() {
    let conus = synthetic_lambert_grib2(SYNTHETIC_POINTS);
    assert!(
        parse_grib2(&conus, ModelParameter::SurfaceBasedCin, 0).is_ok(),
        "the CONUS spelling must still decode, or this test proves nothing"
    );

    let pacific = synthetic_lambert_grib2_at(SYNTHETIC_POINTS, 179_000_000);
    let err = parse_grib2(&pacific, ModelParameter::SurfaceBasedCin, 0)
        .expect_err("a domain at 179 E must be refused by parse_grib2 itself");

    assert!(
        err.contains("outside the -140.0..-50.0 envelope"),
        "the refusal must be the domain check and not some other decode \
         failure; it said:\n{err}"
    );
    assert!(
        err.contains("campaigns/overlays/t17/"),
        "reached through the real parse path, the refusal must still carry \
         the decision; it said:\n{err}"
    );
}

/// A domain refusal is `Permanent`; every other parse failure stays `Transient`.
///
/// The retry ladder's default for a parse error is `Transient`, which is wrong
/// here: a domain is exactly as unplaceable on the next attempt. Also pins the
/// `DOMAIN_REFUSAL_MARK` coupling from both ends.
#[test]
fn the_domain_refusal_is_classified_permanent() {
    let refusal = check_domain_longitude(
        &GeoBounds {
            min_lat: 50.0,
            max_lat: 55.0,
            min_lon: 170.0,
            max_lon: 178.0,
        },
        &HRRR_DOMAIN_LON,
        "HRRR",
    )
    .expect_err("must refuse");
    assert!(
        refusal.contains(DOMAIN_REFUSAL_MARK),
        "the refusal must carry the mark the classifier reads; it said:\n{refusal}"
    );
    assert_eq!(
        classify_parse_error(refusal).failure,
        FetchFailure::Permanent,
        "a domain does not become placeable by being asked for again"
    );

    assert_eq!(
        classify_parse_error("Lambert grid point count mismatch: 7 declared".to_string()).failure,
        FetchFailure::Transient,
    );
    let unwalkable = check_domain_longitude(
        &GeoBounds {
            min_lat: 0.0,
            max_lat: 0.0,
            min_lon: f64::MAX,
            max_lon: f64::MIN,
        },
        &HRRR_DOMAIN_LON,
        "HRRR",
    )
    .expect_err("must refuse");
    assert_eq!(
        classify_parse_error(unwalkable).failure,
        FetchFailure::Transient,
    );
}

/// The envelope is the **source's**, not the module's: a source that declares a
/// global domain is accepted at longitudes HRRR's own envelope refuses.
///
/// This is the half of the T17 decision that was taken. It does not claim the
/// antimeridian *renders* correctly — see the refusal text, which still asks
/// for that repair — only that the envelope stopped being one shared constant
/// whose widening would silently widen HRRR's claim too.
#[test]
fn a_global_domain_is_accepted_when_the_source_declares_one() {
    let global = -180.0..=180.0;
    let pacific = GeoBounds {
        min_lat: 30.0,
        max_lat: 33.0,
        min_lon: 173.4773,
        max_lon: 176.7655,
    };
    assert!(
        check_domain_longitude(&pacific, &HRRR_DOMAIN_LON, "HRRR").is_err(),
        "premise: HRRR's own envelope refuses this extent, so the acceptance \
         below is the declared domain doing the work",
    );
    assert!(
        check_domain_longitude(&pacific, &global, "test/global").is_ok(),
        "a source declaring -180..180 must not be refused inside it",
    );
    assert!(
        check_domain_longitude(
            &GeoBounds {
                min_lat: 0.0,
                max_lat: 1.0,
                min_lon: -180.0,
                max_lon: 180.0,
            },
            &global,
            "test/global",
        )
        .is_ok(),
        "the declared ends are inclusive",
    );
}

/// A declared domain is a claim, not an exemption: outside its own ends the
/// source is refused exactly as HRRR is, and the refusal names *it*.
#[test]
fn a_source_is_still_refused_outside_its_own_declared_domain() {
    let narrow = -100.0..=-90.0;
    let err = check_domain_longitude(
        &GeoBounds {
            min_lat: 30.0,
            max_lat: 40.0,
            min_lon: -110.0,
            max_lon: -95.0,
        },
        &narrow,
        "test/narrow",
    )
    .expect_err("outside its own declared ends, a source is refused");
    assert!(
        err.contains("test/narrow"),
        "the refusal must name the source that declared the envelope; it said:\n{err}"
    );
    assert!(
        err.contains("-100.0..-90.0"),
        "the refusal must quote the declared envelope, not some other one; it said:\n{err}"
    );
    assert!(
        err.contains(DOMAIN_REFUSAL_MARK),
        "a narrower source's refusal is still permanent; it said:\n{err}"
    );
    // Control: inside the same narrow envelope the very same check passes, so
    // the refusal above is the extent and not the envelope's mere presence.
    assert!(
        check_domain_longitude(
            &GeoBounds {
                min_lat: 30.0,
                max_lat: 40.0,
                min_lon: -99.0,
                max_lon: -95.0,
            },
            &narrow,
            "test/narrow",
        )
        .is_ok(),
    );
}

// ---------------------------------------------------------------------------
// The analysis-axis listing (S2 2.6)
// ---------------------------------------------------------------------------

/// **Only an analysis GRIB2 key is a run**, and every other key in the same
/// prefix answers `None`.
///
/// The rejections are the point: `hrrr.YYYYMMDD/conus/` carries the `.idx`
/// sidecar of every hour, all 48 forecast hours, and the sub-hourly `wrfsubh`
/// and `wrfnat` families. A substring match on `wrfsfcf00` would read the
/// sidecar as a run and offer a frame whose fetch downloads 9 KB of text.
#[test]
fn only_an_analysis_grib2_key_names_a_run() {
    assert_eq!(
        run_of_analysis_key("hrrr.20260820/conus/hrrr.t14z.wrfsfcf00.grib2"),
        Some(
            NaiveDate::from_ymd_opt(2026, 8, 20)
                .unwrap()
                .and_hms_opt(14, 0, 0)
                .unwrap()
        ),
    );
    assert_eq!(
        run_of_analysis_key("hrrr.20140730/conus/hrrr.t00z.wrfsfcf00.grib2"),
        Some(
            NaiveDate::from_ymd_opt(2014, 7, 30)
                .unwrap()
                .and_hms_opt(0, 0, 0)
                .unwrap()
        ),
        "the archive begins at hrrr.20140730",
    );

    for key in [
        "hrrr.20260820/conus/hrrr.t14z.wrfsfcf00.grib2.idx",
        "hrrr.20260820/conus/hrrr.t14z.wrfsfcf01.grib2",
        "hrrr.20260820/conus/hrrr.t14z.wrfsfcf48.grib2",
        "hrrr.20260820/conus/hrrr.t14z.wrfsubhf00.grib2",
        "hrrr.20260820/conus/hrrr.t14z.wrfnatf00.grib2",
        "hrrr.20260820/alaska/hrrr.t14z.wrfsfcf00.grib2",
        "hrrr.20260820/hrrr.t14z.wrfsfcf00.grib2",
        "hrrr.2026082/conus/hrrr.t14z.wrfsfcf00.grib2",
        "hrrr.20260820/conus/hrrr.t99z.wrfsfcf00.grib2",
        "",
    ] {
        assert_eq!(
            run_of_analysis_key(key),
            None,
            "`{key}` was read as an analysis run",
        );
    }
}

/// The listing keeps the runs the bucket really carries, in order, clipped to
/// the window — and **only** those: a run the archive is missing must not
/// appear, which is the whole reason this lists rather than computes.
///
/// Non-vacuity: the served day carries a gap (no 12Z) and a key of every
/// rejected family, so a walk that constructed the hourly cycle or matched on
/// a substring answers a different set.
#[test]
fn the_analysis_listing_keeps_the_runs_the_bucket_carries() {
    let day = NaiveDate::from_ymd_opt(2026, 8, 20).unwrap();
    let mut keys: Vec<String> = Vec::new();
    for hour in [10u32, 11, 13, 14] {
        keys.push(format!(
            "hrrr.20260820/conus/hrrr.t{hour:02}z.wrfsfcf00.grib2"
        ));
        keys.push(format!(
            "hrrr.20260820/conus/hrrr.t{hour:02}z.wrfsfcf00.grib2.idx"
        ));
        keys.push(format!(
            "hrrr.20260820/conus/hrrr.t{hour:02}z.wrfsfcf06.grib2"
        ));
    }
    let body = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\
         <ListBucketResult xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\">\
         <Name>b</Name><IsTruncated>false</IsTruncated>{}</ListBucketResult>",
        keys.iter()
            .map(|k| format!("<Contents><Key>{k}</Key></Contents>"))
            .collect::<String>(),
    );

    let sources = s3_serving(body);
    let range = (
        day.and_hms_opt(11, 0, 0).unwrap(),
        day.and_hms_opt(14, 0, 0).unwrap(),
    );
    let runs = listing_runtime().block_on(list_analysis_runs(&loopback_client(), &sources, range));
    assert_eq!(
        runs.expect("the loopback bucket answers"),
        [11u32, 13, 14]
            .map(|h| day.and_hms_opt(h, 0, 0).unwrap())
            .to_vec(),
        "the listing must be the analysis keys inside the window, ascending, \
         with 12Z absent because the bucket does not carry it",
    );
}

/// A listing S3 refuses is an error, not an empty day: an empty `Ok` would
/// reach `apply_frame_listing` as a covering listing and settle the window on
/// "no runs exist".
#[test]
fn a_refused_listing_is_an_error_and_not_an_empty_day() {
    let sources = s3_serving_status("404 Not Found", "<Error/>");
    let day = NaiveDate::from_ymd_opt(2026, 8, 20).unwrap();
    let range = (
        day.and_hms_opt(0, 0, 0).unwrap(),
        day.and_hms_opt(1, 0, 0).unwrap(),
    );
    let out = listing_runtime().block_on(list_analysis_runs(&loopback_client(), &sources, range));
    let err = out.expect_err("a 404 on the listing is not an empty day");
    assert!(err.message.contains("404"), "{}", err.message);
}

/// A window spanning UTC midnight lists both day prefixes. One LIST per day,
/// not one per frame.
#[test]
fn a_window_across_midnight_lists_both_days() {
    let body = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\
         <ListBucketResult xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\">\
         <Name>b</Name><IsTruncated>false</IsTruncated>\
         <Contents><Key>hrrr.20260820/conus/hrrr.t23z.wrfsfcf00.grib2</Key></Contents>\
         <Contents><Key>hrrr.20260821/conus/hrrr.t00z.wrfsfcf00.grib2</Key></Contents>\
         </ListBucketResult>"
        .to_string();
    let sources = s3_serving(body);
    let range = (
        NaiveDate::from_ymd_opt(2026, 8, 20)
            .unwrap()
            .and_hms_opt(23, 0, 0)
            .unwrap(),
        NaiveDate::from_ymd_opt(2026, 8, 21)
            .unwrap()
            .and_hms_opt(0, 0, 0)
            .unwrap(),
    );
    let runs = listing_runtime()
        .block_on(list_analysis_runs(&loopback_client(), &sources, range))
        .expect("the loopback bucket answers");
    assert_eq!(runs.len(), 2, "{runs:?}");
    assert!(runs[0] < runs[1], "ascending: {runs:?}");
}

/// The day walk is bounded: a window nobody bounded must not become four
/// thousand LISTs against a twelve-year archive.
///
/// Non-vacuity: the loopback server **counts** the requests it answered, so
/// an unbounded walk fails here rather than merely taking 4383 round trips and
/// passing. `MAX_LISTED_DAYS` is a const assert away from the archive's depth,
/// so the comparison itself is not restated as a runtime check clippy can see
/// through.
#[test]
fn an_unbounded_window_stops_at_the_day_cap() {
    let body = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\
         <ListBucketResult xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\">\
         <Name>b</Name><IsTruncated>false</IsTruncated>\
         <Contents><Key>hrrr.20260820/conus/hrrr.t00z.wrfsfcf00.grib2</Key></Contents>\
         <Contents><Key>hrrr.20260821/conus/hrrr.t00z.wrfsfcf00.grib2</Key></Contents>\
         <Contents><Key>hrrr.20260822/conus/hrrr.t00z.wrfsfcf00.grib2</Key></Contents>\
         <Contents><Key>hrrr.20260823/conus/hrrr.t00z.wrfsfcf00.grib2</Key></Contents>\
         <Contents><Key>hrrr.20260824/conus/hrrr.t00z.wrfsfcf00.grib2</Key></Contents>\
         <Contents><Key>hrrr.20260825/conus/hrrr.t00z.wrfsfcf00.grib2</Key></Contents>\
         <Contents><Key>hrrr.20260826/conus/hrrr.t00z.wrfsfcf00.grib2</Key></Contents>\
         <Contents><Key>hrrr.20260827/conus/hrrr.t00z.wrfsfcf00.grib2</Key></Contents>\
         <Contents><Key>hrrr.20260828/conus/hrrr.t00z.wrfsfcf00.grib2</Key></Contents>\
         </ListBucketResult>"
        .to_string();
    let (sources, served) = s3_serving_counted("200 OK", &body);
    let start = NaiveDate::from_ymd_opt(2026, 8, 20)
        .unwrap()
        .and_hms_opt(0, 0, 0)
        .unwrap();
    let runs = listing_runtime()
        .block_on(list_analysis_runs(
            &loopback_client(),
            &sources,
            (start, start + chrono::Duration::days(365 * 12)),
        ))
        .expect("the loopback bucket answers");
    assert_eq!(
        served.load(std::sync::atomic::Ordering::SeqCst),
        MAX_LISTED_DAYS,
        "the walk issued one LIST per day of a twelve-year window",
    );
    // Every prefix serves the same nine keys, so the dedupe leaves nine.
    assert_eq!(runs.len(), 9, "{runs:?}");
}

fn listing_runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("test runtime")
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

fn s3_serving(body: String) -> DataSources {
    s3_serving_counted("200 OK", &body).0
}

fn s3_serving_status(status_line: &'static str, body: &str) -> DataSources {
    s3_serving_counted(status_line, body).0
}

/// A loopback S3 serving one canned response, **and the count of requests it
/// answered** — the only way to assert that a walk was bounded rather than
/// merely slow.
fn s3_serving_counted(
    status_line: &'static str,
    body: &str,
) -> (DataSources, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
    use std::io::{Read, Write};
    let response = format!(
        "HTTP/1.1 {status_line}\r\nContent-Type: application/xml\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len(),
    );
    let served = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counter = served.clone();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let port = listener.local_addr().expect("local addr").port();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { break };
            let mut scratch = [0u8; 4096];
            let _ = stream.read(&mut scratch);
            counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let _ = stream.write_all(response.as_bytes());
            let _ = stream.flush();
        }
    });
    (
        DataSources {
            hrrr_bucket: "hrrr".into(),
            s3_base: format!("http://127.0.0.1:{port}/{{bucket}}").into(),
            ..DataSources::production()
        },
        served,
    )
}
