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

// ── Index parsing ─────────────────────────────────────────────────────

/// Expected values are read off the fixture by eye, not from the parser.
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
    // A well-formed line survives alongside a broken one.
    let mixed = format!("nonsense\n{}", SAMPLE_IDX.lines().next().unwrap());
    assert_eq!(parse_idx(&mixed).len(), 1);
}

// ── Byte ranges ───────────────────────────────────────────────────────

/// A record runs from its own offset to one byte before the next.
///
/// The expected end is hand-computed from the fixture: CIN starts at
/// 63,976,324 and PWAT at 64,861,905, so CIN ends at 64,861,904.
#[test]
fn a_byte_range_ends_one_byte_before_the_next_record() {
    let (start, end) = byte_range(&records(), "CIN", "surface").unwrap();
    assert_eq!(start, 63_976_324);
    assert_eq!(end, Some(64_861_904));
    // The length is the gap between the two offsets.
    assert_eq!(end.unwrap() - start + 1, 64_861_905 - 63_976_324);
}

/// An off-by-one here delivers a second record's first byte, which
/// `parse_grib2` now refuses rather than silently decoding the wrong
/// field. Pinning the arithmetic separately says which of the two broke.
#[test]
fn a_byte_range_does_not_overlap_the_following_record() {
    let r = records();
    for pair in r.windows(2) {
        let (_, end) = byte_range(&r, &pair[0].var, &pair[0].level).unwrap();
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

/// The last record has no successor, so its end is open.
#[test]
fn the_final_records_range_is_open_ended() {
    let (start, end) = byte_range(&records(), "CIN", "180-0 mb above ground").unwrap();
    assert_eq!(start, 99_500_000);
    assert_eq!(end, None, "nothing in the index bounds the last record");
}

/// Matching on the variable alone is ambiguous: the fixture has two `CIN`
/// and two `CAPE` records at different levels, and two `HLCY`s. Selecting
/// by variable only would return surface CIN for mixed-layer CIN — a
/// plausible-looking, entirely wrong field.
#[test]
fn a_record_is_selected_by_variable_and_level_together() {
    let r = records();
    assert_eq!(byte_range(&r, "CIN", "surface").unwrap().0, 63_976_324);
    assert_eq!(
        byte_range(&r, "CIN", "180-0 mb above ground").unwrap().0,
        99_500_000,
    );
    assert_eq!(byte_range(&r, "CAPE", "surface").unwrap().0, 63_110_198);
    assert_eq!(
        byte_range(&r, "CAPE", "180-0 mb above ground").unwrap().0,
        99_000_000,
    );
    // ...and the two SRH layers, which differ only in level.
    assert_eq!(
        byte_range(&r, "HLCY", "3000-0 m above ground").unwrap().0,
        94_635_452,
    );
    assert_eq!(
        byte_range(&r, "HLCY", "1000-0 m above ground").unwrap().0,
        95_300_000,
    );
}

/// An unmatched spelling must fail loudly rather than fall back to a near
/// miss — the failure mode the ascending `2000-5000` spellings had.
#[test]
fn an_unmatched_variable_or_level_yields_no_range() {
    let r = records();
    assert_eq!(byte_range(&r, "CIN", "2000-5000 m above ground"), None);
    assert_eq!(byte_range(&r, "NOSUCH", "surface"), None);
    assert_eq!(byte_range(&r, "CIN", "Surface"), None, "matching is exact");
}

// ── Parameter → index record ──────────────────────────────────────────

/// Transcribed **verbatim** from a real `hrrr.t14z.wrfsfcf00.grib2.idx` (UH
/// from the `f01` index, which spells its levels identically).
///
/// There is no rule to infer: HRRR orders layer bounds inconsistently
/// between fields — `HLCY:3000-0` and `MXUPHL:5000-2000` put the top first,
/// `VUCSH:0-6000` and `CAPE:0-3000 m` the bottom — and matching is literal
/// with no near-miss handling.
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

/// Pins every non-composite parameter to the index record it selects.
#[test]
fn every_parameter_selects_a_real_index_record() {
    for &(param, var, level) in IDX_RECORDS {
        assert_eq!(param.grib_var(), var, "{}", param.display_name());
        assert_eq!(param.grib_level(), level, "{}", param.display_name());
    }
}

/// The table above is only a guard if it covers everything.
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

/// Composite components select real index records too.
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

/// No two parameters may select the same record — that would mean one of
/// them is displaying the other's field.
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

// ── Forecast hour ─────────────────────────────────────────────────────

/// f00 `MXUPHL` is a `0-0 day max fcst` — a maximum over a zero-length
/// window, which is identically 0.0 everywhere.
#[test]
fn uh_requests_a_forecast_hour_with_a_nonzero_window() {
    for param in [ModelParameter::MaxUH2to5km, ModelParameter::MaxUH0to2km] {
        assert!(
            param.forecast_hour() > 0,
            "{} must not come from f00: its accumulation window there has \
                 zero length and the field is constant 0.0",
            param.display_name(),
        );
        assert!(param.is_windowed());
    }
}

/// Everything else is instantaneous, so f00 is both valid and freshest.
#[test]
fn non_windowed_parameters_still_come_from_the_analysis() {
    for param in ModelParameter::all() {
        if param.is_windowed() {
            continue;
        }
        assert_eq!(
            param.forecast_hour(),
            0,
            "{} is instantaneous and should come from f00",
            param.display_name(),
        );
    }
}

/// Relaxing `count != 1` to `count < 1` lets two concatenated records decode
/// as a sequence with the first silently winning — a correct-looking grid
/// for the wrong field. Only an `#[ignore]`d live test used to catch that.
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

    // The failure mode a `< 1` guard would let through.
    assert!(exactly_one_submessage(3).is_err());
}

/// The count guard is only reachable through a real submessage, so it is
/// pinned here rather than only by the live S3 tests. HRRR's 1799x1059 is
/// the case that must pass; either direction of mismatch must not.
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

// ── A synthetic GRIB2 message ─────────────────────────────────────────

/// 3 x 2 = 6 points, which keeps the whole message to 188 bytes — a real
/// HRRR record is ~1 MB, far too large to commit.
const SYNTHETIC_NI: u32 = 3;
const SYNTHETIC_NJ: u32 = 2;
const SYNTHETIC_POINTS: u32 = SYNTHETIC_NI * SYNTHETIC_NJ;

/// Byte offset of section 3's `numberOfDataPoints`: section 0 (16) +
/// section 1 (21) + section 3's 5-byte header + its 1-byte source-of-grid.
/// Checked against the built bytes below rather than trusted.
const SECT3_NUM_POINTS_OFFSET: usize = 16 + 21 + 5 + 1;

/// `length | section number | body`, with the length computed.
fn grib_section(number: u8, body: &[u8]) -> Vec<u8> {
    let mut out = ((body.len() + 5) as u32).to_be_bytes().to_vec();
    out.push(number);
    out.extend_from_slice(body);
    out
}

/// A whole GRIB2 message: a 3 x 2 Lambert-conformal grid (template 3.30)
/// carrying a constant field (DRT 5.0 with `nbits = 0`, so section 7 holds
/// no data at all).
///
/// `declared_points` is section 3's `numberOfDataPoints`, which grib reads
/// verbatim — `GridDefinition::num_points()` is a `read_as!(u32, .., 1)`,
/// not `ni * nj` — so any value but [`SYNTHETIC_POINTS`] gives a message
/// well-formed everywhere except the count [`check_point_count`] guards.
fn synthetic_lambert_grib2(declared_points: u32) -> Vec<u8> {
    // HRRR's own central meridian, 262.5 = -97.5.
    synthetic_lambert_grib2_at(declared_points, 262_500_000)
}

/// [`synthetic_lambert_grib2`] with the domain moved.
///
/// `lon0` is written to **both** `Lo1` and `LoV`, in microdegrees, so the grid
/// always sits on its own central meridian. Moving only the first point would
/// put the domain outside the cone's principal sector and the message would be
/// refused for that instead — a different fault wearing the same message.
fn synthetic_lambert_grib2_at(declared_points: u32, lon0: u32) -> Vec<u8> {
    // Section 1 — identification.
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

    // Section 3 — grid definition.
    let mut sect3 = Vec::new();
    sect3.push(0); // source of grid definition: the template below
    sect3.extend_from_slice(&declared_points.to_be_bytes()); // ← perturbed
    sect3.push(0); // no optional list of numbers of points
    sect3.push(0); // ...so nothing to interpret
    sect3.extend_from_slice(&30u16.to_be_bytes()); // template 3.30
    // Template 3.30 body, in grib's `Template3_30` field order.
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

    // Section 4 — product definition, template 4.0.
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

    // Section 5 — data representation, template 5.0. Its own point count
    // stays at Ni x Nj in both fixtures, so section 3's is the only
    // difference between them.
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

    // Section 0. The total length is computed; grib walks the message by
    // decrementing it, so a hand-counted value desynchronises the parse.
    let mut message = b"GRIB".to_vec();
    message.extend_from_slice(&[0, 0]); // reserved
    message.push(0); // discipline: meteorological products
    message.push(2); // GRIB edition 2
    let total = (message.len() + 8 + body.len()) as u64;
    message.extend_from_slice(&total.to_be_bytes());
    message.extend(body);
    message
}

/// [`grid_coords`] on the first submessage of `bytes`, i.e. the real call
/// site, reached the way `parse_grib2` reaches it.
fn grid_coords_of(bytes: &[u8]) -> Result<GridCoords, String> {
    let grib2 = grib::from_reader(std::io::Cursor::new(bytes))
        .map_err(|e| format!("GRIB2 parse error: {e}"))?;
    let (_index, submessage) = grib2
        .iter()
        .next()
        .ok_or_else(|| "no submessages in the synthetic message".to_string())?;
    grid_coords(&submessage)
}

/// The control the mismatch test rests on: this message is well-formed in
/// every other respect, so the `Err` below cannot be blamed on anything
/// else. Without it the negative test would pass on a message grib rejects
/// for an unrelated reason, and the guard would go untested.
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

    let grid = parse_grib2(&bytes, ModelParameter::SurfaceBasedCin)
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

/// The `?` on `check_point_count` inside [`grid_coords`]. Dropping it lets
/// a grid whose Ni x Nj disagrees with section 3 through, and the values
/// are then laid out over the wrong coordinates — weather in the wrong
/// place. [`check_point_count`]'s own body is pinned above; this pins the
/// propagation at the only call site.
///
/// The two fixtures differ in exactly the four bytes of section 3's
/// `numberOfDataPoints`, so nothing else can be what rejects the second.
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

    // Control: the same call, on the same message, with the count agreeing.
    assert!(
        matches!(grid_coords_of(&good), Ok(GridCoords::Lambert(_))),
        "the control fixture must reach the Lambert branch: {:?}",
        grid_coords_of(&good).err(),
    );

    let err = grid_coords_of(&bad).expect_err("the mismatch must be refused");
    assert!(err.contains("Lambert grid point count mismatch"), "{err}");
    assert!(err.contains("7 declared"), "{err}");
    assert!(err.contains("6 computed"), "{err}");

    // ...and it must reach the caller rather than being swallowed there.
    let err = parse_grib2(&bad, ModelParameter::SurfaceBasedCin)
        .expect_err("parse_grib2 must refuse it too");
    assert!(err.contains("Lambert grid point count mismatch"), "{err}");
}

/// Four verbatim lines from
/// `hrrr.20260725/conus/hrrr.t14z.wrfsfcf01.grib2.idx` where `(var, level)`
/// repeats. Neither pair is a field rustdar requests, but taking record 8
/// where a caller wanted record 44 swaps an instantaneous field for a
/// windowed maximum with no error, so the tie-break is pinned.
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

    // The forecast field distinguishes them and is parsed, so a future
    // caller has a disambiguator.
    assert_eq!(records[0].forecast, "1 hour fcst");
    assert_eq!(records[1].forecast, "0-1 hour max fcst");

    let (start, _) = byte_range(&records, "REFD", "263 K level").unwrap();
    assert_eq!(start, 2_668_643, "the first match must win, i.e. record 8");
    let (start, _) = byte_range(&records, "WEASD", "surface").unwrap();
    assert_eq!(
        start, 42_378_051,
        "the first match must win, i.e. record 68"
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
    // The first day of a month, where the date arithmetic is not just -1.
    let first = NaiveDate::from_ymd_opt(2026, 8, 1).unwrap();
    assert_eq!(
        previous_run(first, 0),
        (NaiveDate::from_ymd_opt(2026, 7, 31).unwrap(), 23)
    );
}

/// The forecast hour must reach the object key; if it does not, the UH fix
/// silently reverts to the constant-zero f00 record.
#[test]
fn the_object_key_carries_the_parameters_forecast_hour() {
    let date = NaiveDate::from_ymd_opt(2026, 7, 25).unwrap();
    let key = |p: ModelParameter| DataSources::hrrr_key(&date, 3, p.forecast_hour());
    assert!(key(ModelParameter::MaxUH2to5km).contains("wrfsfcf01.grib2"));
    assert!(key(ModelParameter::MaxUH0to2km).contains("wrfsfcf01.grib2"));
    assert!(key(ModelParameter::SurfaceBasedCin).contains("wrfsfcf00.grib2"));
}

// ── Live checks ───────────────────────────────────────────────────────

/// The invariant the whole selection rests on, and a property of NCEP's
/// index rather than of rustdar's code — an upstream change can break it
/// with no commit here. [`byte_range`] takes the first `(var, level)` hit,
/// which is only safe while no requested pair repeats.
///
/// `cargo test -p rustdar-overlays -- --ignored --nocapture live_every_parameter`
#[tokio::test]
#[ignore = "hits the live noaa-hrrr-bdp-pds S3 bucket"]
async fn live_every_parameter_selects_exactly_one_record() {
    let client = hrrr_client().expect("client");
    let sources = DataSources::production();
    let (date, hour) = latest_available_run();

    // The two indexes do not carry the same record set: windowed parameters
    // live in f01, the rest in f00.
    for forecast_hour in [0u8, 1] {
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

        // Control: the ambiguity this is guarding against is real in this
        // very index, so a "no pair ever repeats" reading of a pass below
        // would be wrong.
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
            if param.forecast_hour() != forecast_hour {
                continue;
            }
            let pairs = param
                .composite_parts()
                .unwrap_or_else(|| vec![(param.grib_var(), param.grib_level())]);
            for (var, level) in pairs {
                let matches = records
                    .iter()
                    .filter(|r| r.var == var && r.level == level)
                    .collect::<Vec<_>>();
                assert_eq!(
                    matches.len(),
                    1,
                    "{} selects {} record(s) for {var}:{level} in f{forecast_hour:02} — \
                         byte_range takes the first, so this is a silent wrong-field read; \
                         match on IdxRecord::forecast as well. Candidates: {:?}",
                    param.display_name(),
                    matches.len(),
                    matches
                        .iter()
                        .map(|r| (r.number, &r.forecast))
                        .collect::<Vec<_>>(),
                );
            }
        }
    }
}

/// The full S3 path end to end: real index, real byte range, real `Range`
/// request, decoding the operational DRT 5.3 bytes S3 serves. NOMADS
/// re-encoded to 5.0, so 5.3 was never exercised before the migration.
///
/// `cargo test -p rustdar-overlays -- --ignored --nocapture live_hrrr`
#[tokio::test]
#[ignore = "hits the live noaa-hrrr-bdp-pds S3 bucket"]
async fn live_hrrr_fetches_and_decodes_from_s3() {
    let client = hrrr_client().expect("client");
    let sources = DataSources::production();

    // One surface field, one layer field, one whose level carries spaces
    // and parentheses, and one windowed field from f01.
    for param in [
        ModelParameter::SurfaceBasedCape,
        ModelParameter::MixedLayerCin,
        ModelParameter::PrecipitableWater,
        ModelParameter::MaxUH2to5km,
    ] {
        let grid = match fetch_hrrr_data(&client, &sources, &param).await.0 {
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

        // 1799 x 1059 is HRRR's published operational grid; both NOMADS and
        // S3 return all of it.
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

        // Deliberately NOT `assert!(grid.blank_notice().is_none())`: that
        // asserts on the weather. On a quiet day Max UH 2-5 km peaks below
        // `uh_color`'s lowest threshold of 25 m²/s² (CONUS max was 22.1 on
        // 2026-07-25) and this went red with nothing broken. The other two
        // `blank_notice` cases are already covered above — no usable values
        // by `.expect("finite values")`, and the constant-zero f00 UH record
        // by `lo < hi`.
        if let Some(notice) = grid.blank_notice() {
            println!("  {notice}");
        }

        // Published HRRR domain: SW corner 21.14 N, 237.28 E.
        assert!(
            grid.bounds.min_lat < 25.0 && grid.bounds.max_lat > 47.0,
            "{} bounds {:?} do not span CONUS",
            param.display_name(),
            grid.bounds,
        );
    }
}

/// The composite path also works over byte ranges — two records from the
/// same index, merged.
#[tokio::test]
#[ignore = "hits the live noaa-hrrr-bdp-pds S3 bucket"]
async fn live_hrrr_composite_merges_two_ranged_records() {
    let client = hrrr_client().expect("client");
    let sources = DataSources::production();
    let param = ModelParameter::BulkShear6km;

    let grid = match fetch_composite_hrrr_data(&client, &sources, &param).await.0 {
        Ok(g) => g,
        Err(e) => panic!("bulk shear fetch failed: {e}"),
    };
    let (lo, hi) = grid.value_range.expect("finite values");
    println!("bulk shear: {} pts, range {lo}..{hi}", grid.values.len());
    assert_eq!(grid.values.len(), 1_905_141);
    // Negatives would mean the merge returned one component, not a
    // magnitude.
    assert!(lo >= 0.0, "a vector magnitude cannot be negative, got {lo}");
    assert!(hi > 0.0);
}

/// The guard that makes a byte-range arithmetic bug loud. Built from a live
/// record rather than a committed fixture (one HRRR record is ~1 MB). The
/// single-record case is asserted first, so a `parse_grib2` that rejected
/// everything would fail rather than pass.
#[tokio::test]
#[ignore = "hits the live noaa-hrrr-bdp-pds S3 bucket"]
async fn live_parse_grib2_refuses_more_than_one_submessage() {
    let client = hrrr_client().expect("client");
    let sources = DataSources::production();
    let (date, hour) = latest_available_run();

    let one = match fetch_record(&client, &sources, date, hour, 0, "CIN", "surface").await {
        Ok(b) => b,
        Err(_) => {
            // `previous_run`, not `hour - 1`, which panics for the whole
            // 02:00-02:59 UTC hour.
            let (prev_date, prev_hour) = previous_run(date, hour);
            fetch_record(&client, &sources, prev_date, prev_hour, 0, "CIN", "surface")
                .await
                .expect("CIN fetch")
        }
    };

    // Control: one record must parse.
    let single = parse_grib2(&one, ModelParameter::SurfaceBasedCin);
    assert!(single.is_ok(), "a single record must decode: {single:?}");

    // Two concatenated records must not.
    let mut two = one.clone();
    two.extend_from_slice(&one);
    let err = parse_grib2(&two, ModelParameter::SurfaceBasedCin)
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

    // `match`, not `.or(..await)`: `Result::or` takes its argument by value,
    // so the fallback future was awaited unconditionally — a second ~1 MB
    // range request on every run.
    let bytes = match fetch_record(&client, &sources, date, hour, 0, "CIN", "surface").await {
        Ok(b) => b,
        Err(_) => {
            let (prev_date, prev_hour) = previous_run(date, hour);
            fetch_record(&client, &sources, prev_date, prev_hour, 0, "CIN", "surface")
                .await
                .expect("CIN fetch")
        }
    };

    println!("surface CIN record: {} bytes", bytes.len());
    // Operational record ~1.03 MB (NOMADS returned 2.27 MB for the same
    // field). Bounded clear of a whole file (~130 MB) and of an empty one.
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

/// **The permanently-moved-product test.** A single run 404ing is routine and
/// must stay routine — it is why the previous-hour fallback exists. Both runs
/// 404ing is not: the bucket carries a rolling window and should always have
/// one of the last two hours.
///
/// Left `Absent`, that state is invisible for ever: `Absent` resets the ladder,
/// stamps the clock and reports no fault, so a moved HRRR would poll hourly and
/// say nothing. Escalated to `Transient` it costs the same one poll an hour and
/// shows up in the layer's own panel as "not loading".
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

/// The merge itself is unchanged for every other combination: a round is
/// refused only when both runs were, and one retryable run keeps the round
/// retryable. The fallback is the *older* run and so the one less likely to be
/// missing, which is why "every part" is the right rule here.
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

// ── The domain envelope guard ────────────────────────────────────────────
//
// `check_domain_longitude` exists so that "the renderer assumes a
// non-straddling domain" stops being a fact only a campaign record knows.
// These pin both halves of it: that today's domain passes, and that the
// shapes which would mis-render are refused with the decision attached.

/// The operational domain passes, and passes with room to spare.
///
/// Without this the guard could be tightened to nothing and no test would
/// notice — a refusal that refuses everything is not a guard, it is an outage.
#[test]
fn the_conus_domain_the_app_actually_fetches_is_accepted() {
    // Corner-verified HRRR CONUS, from `hrrr::lambert::tests`.
    let conus = GeoBounds {
        min_lat: 21.1,
        max_lat: 52.7,
        min_lon: -134.0955,
        max_lon: -60.9172,
    };
    assert!(
        check_domain_longitude(&conus).is_ok(),
        "the only domain the app fetches must not be refused"
    );
    // And the margin is real, not incidental: CONUS clears the near edge of
    // the envelope by ~6 deg and the antimeridian by ~46 deg.
    assert!(conus.min_lon - *VALIDATED_DOMAIN_LON.start() > 5.0);
    assert!(180.0 - conus.min_lon.abs() > 40.0);
}

/// A domain east of the antimeridian is refused — the Guam/western-Aleutians
/// shape, where every longitude is positive and a turn from the viewport.
#[test]
fn an_east_hemisphere_domain_is_refused() {
    let seam_parked = GeoBounds {
        min_lat: 30.0,
        max_lat: 33.0,
        min_lon: 173.4773,
        max_lon: 176.7655,
    };
    let err = check_domain_longitude(&seam_parked).expect_err("must refuse");
    assert!(
        err.contains("173.4773"),
        "the message must name the extent it saw"
    );
}

/// A domain straddling the antimeridian is refused. Folded, its extreme
/// longitudes sit at both ends of the range, so this is also the shape for
/// which the rigid whole-grid shift would be the *wrong* repair.
#[test]
fn a_straddling_domain_is_refused() {
    let straddling = GeoBounds {
        min_lat: 50.0,
        max_lat: 55.0,
        min_lon: -179.8,
        max_lon: 179.6,
    };
    assert!(check_domain_longitude(&straddling).is_err());
}

/// The refusal has to carry the decision, not just the fact. This is the
/// whole point of putting the guard at the fetch and not at the render: the
/// reader is mid-change and can act, so the message owes them the options and
/// the evidence.
#[test]
fn the_refusal_names_the_decision_and_where_the_evidence_is() {
    let err = check_domain_longitude(&GeoBounds {
        min_lat: 50.0,
        max_lat: 55.0,
        min_lon: 170.0,
        max_lon: 178.0,
    })
    .expect_err("must refuse");

    for owed in [
        // that it is not a decode bug
        "not a decode failure",
        // the measurement, so the cost is a number and not an adjective
        "3294",
        // each of the three candidate repairs
        "rigid whole-grid shift",
        "per-point shift",
        "wraps_longitude",
        // why the per-point one is not free
        "neighbours' pixel spacing",
        // where the evidence lives
        "campaigns/overlays/t17/",
        // and what not to do on the way past
        "VALIDATED_DOMAIN_LON",
    ] {
        assert!(
            err.contains(owed),
            "the refusal must mention {owed:?}; it said:\n{err}"
        );
    }
}

/// A grid whose coordinates could not be walked reports *that*, rather than
/// being dressed up as a domain problem it is not.
///
/// The sentinel is the one `parse_grib2`'s own bounds walk leaves behind, and
/// it is the interesting case precisely because `f64::MAX` is finite: an
/// `is_finite` test alone passes it straight through to the domain message,
/// which then quotes a 309-digit longitude and blames the wrong thing. This
/// test caught exactly that.
#[test]
fn an_unwalkable_extent_is_refused_as_itself() {
    let err = check_domain_longitude(&GeoBounds {
        min_lat: 0.0,
        max_lat: 0.0,
        min_lon: f64::MAX,
        max_lon: f64::MIN,
    })
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

/// The guard is actually *wired into* `parse_grib2`, not merely present.
///
/// The tests above call `check_domain_longitude` directly, so deleting its
/// one call site would leave every one of them green while the guard stopped
/// running. This drives a whole synthetic GRIB2 message — identical to the
/// one `a_synthetic_lambert_message_decodes_through_the_real_parse_path`
/// decodes successfully, moved to 179 E — through the real parse path, and
/// so fails if the call site goes.
///
/// That paired construction is the point: the CONUS spelling of this message
/// is *known* to decode, so the `Err` here can only be the domain check.
#[test]
fn parse_grib2_refuses_a_domain_outside_the_validated_envelope() {
    // The control: same builder, HRRR's own meridian, decodes fine.
    let conus = synthetic_lambert_grib2(SYNTHETIC_POINTS);
    assert!(
        parse_grib2(&conus, ModelParameter::SurfaceBasedCin).is_ok(),
        "the CONUS spelling must still decode, or this test proves nothing"
    );

    // The same message parked at 179 E, a hair east of the antimeridian.
    let pacific = synthetic_lambert_grib2_at(SYNTHETIC_POINTS, 179_000_000);
    let err = parse_grib2(&pacific, ModelParameter::SurfaceBasedCin)
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

/// A domain refusal is `Permanent`; every other parse failure stays
/// `Transient`.
///
/// The retry ladder's default for a parse error is `Transient`, which is right
/// for a truncated or mis-ranged record and wrong for this: a domain is exactly
/// as unplaceable on the next attempt, so retrying it burns the backoff ladder
/// and presents a configuration mistake as though the network were at fault.
///
/// This also pins the `DOMAIN_REFUSAL_MARK` coupling from both ends — the
/// message that carries it and the classifier that reads it — so neither can
/// be reworded alone.
#[test]
fn the_domain_refusal_is_classified_permanent() {
    let refusal = check_domain_longitude(&GeoBounds {
        min_lat: 50.0,
        max_lat: 55.0,
        min_lon: 170.0,
        max_lon: 178.0,
    })
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

    // The control: an ordinary decode failure must stay retryable, or this
    // classifier would have turned every transient fault permanent.
    assert_eq!(
        classify_parse_error("Lambert grid point count mismatch: 7 declared".to_string()).failure,
        FetchFailure::Transient,
    );
    // ...including the other refusal `check_domain_longitude` can produce,
    // which is a decode problem and not a domain one.
    let unwalkable = check_domain_longitude(&GeoBounds {
        min_lat: 0.0,
        max_lat: 0.0,
        min_lon: f64::MAX,
        max_lon: f64::MIN,
    })
    .expect_err("must refuse");
    assert_eq!(
        classify_parse_error(unwalkable).failure,
        FetchFailure::Transient,
    );
}
