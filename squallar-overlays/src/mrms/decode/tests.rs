//! Decode, over **real granules — one per shipped product**.
//!
//! `testdata/MRMS_MergedReflectivityQCComposite_00.50_20260821-000039.grib2.gz`
//! (1 321 750 B gzipped, 1 369 957 B of GRIB2) and
//! `testdata/MRMS_PrecipRate_00.00_20260822-032400.grib2.gz` (475 207 B) are the
//! objects those keys name in `noaa-mrms-pds`, byte for byte. Whole CONUS
//! mosaics, not cuts of one, because every claim below is about the products
//! this layer actually ships.
//!
//! **Both are committed rather than one**, and that is not symmetry for its own
//! sake: the two products use *different* reserved codes for missing data, and a
//! suite built on the composite alone declared the rate correct while a third of
//! it reported −3 mm/h as a measurement. See
//! [`MrmsProduct::missing_codes`].
//!
//! Each granule is decoded once for the whole file: 98 MB of values apiece, and
//! decoding per test would multiply that by the harness's thread count.

use super::*;
use std::sync::LazyLock;

/// The committed granules, gzipped exactly as the bucket serves them.
const COMPOSITE_GZ: &[u8] = include_bytes!(
    "../../../testdata/MRMS_MergedReflectivityQCComposite_00.50_20260821-000039.grib2.gz"
);
const RATE_GZ: &[u8] =
    include_bytes!("../../../testdata/MRMS_PrecipRate_00.00_20260822-032400.grib2.gz");

fn gz_for(product: MrmsProduct) -> &'static [u8] {
    match product {
        MrmsProduct::ReflectivityComposite => COMPOSITE_GZ,
        MrmsProduct::PrecipRate => RATE_GZ,
    }
}

/// **The streaming row walk decodes exactly what `grib`'s whole-image decode
/// decodes**, on every committed granule.
///
/// The shipped path stopped calling `Grib2SubmessageDecoder::dispatch()` to
/// avoid its 49,000,000 B image buffer (measured, per granule) and reads
/// section 7 with the `png` crate a row at a time instead. That is only ever
/// allowed to be an *allocation* change: the values must be the same values.
///
/// **The two sides are genuinely two decoders.** [`RAW`] is `grib`'s own
/// `dispatch()`, materialised buffer and all, and it is the only thing in this
/// file that still runs one; [`fixture`] is `parse_grib2`, which is the
/// shipped row walk. The first version of this test compared `RAW` against a
/// second `dispatch()` and would have passed with the row walk deleted.
///
/// **Bit patterns, not approximate equality** — `to_bits`, so a `NaN` that
/// arrived by a different route is still a difference, and so is `-0.0`
/// against `0.0`. Both products, because they carry different reserved codes
/// and different packing parameters.
///
/// Non-triviality: the full 24.5 M points on both sides, and at least a
/// million finite readings — two decoders that agreed because both returned
/// nothing would pass everything.
#[test]
fn the_row_walk_decodes_what_grib_decodes() {
    for &product in MrmsProduct::all() {
        let missing = product.missing_codes();
        let oracle = raw_for(product);
        let shipped = fixture(product).grid.values.to_f32();

        assert_eq!(
            oracle.len(),
            7000 * 3500,
            "{}: grib decoded {} values, not the whole mosaic",
            product.as_str(),
            oracle.len(),
        );
        assert_eq!(
            shipped.len(),
            oracle.len(),
            "{}: the row walk produced {} values against grib's {}",
            product.as_str(),
            shipped.len(),
            oracle.len(),
        );
        assert!(
            oracle.iter().filter(|v| v.is_finite()).count() > 1_000_000,
            "{}: grib found almost no finite readings, so agreeing with it \
             proves nothing",
            product.as_str(),
        );

        // `reading` is applied to grib's side here rather than compared
        // around, because it is the one step both paths share and the claim
        // under test is about the values reaching it.
        let differing = oracle
            .iter()
            .map(|&raw| reading(missing, raw))
            .zip(shipped.iter())
            .enumerate()
            .find(|(_, (a, b))| a.to_bits() != b.to_bits());
        assert!(
            differing.is_none(),
            "{}: the row walk and grib's decode differ at {:?}",
            product.as_str(),
            differing.map(|(i, (a, b))| (i, a, *b)),
        );
    }
}

/// **Every shipped granule really takes the streaming arm.**
///
/// The fallback to `grib`'s `dispatch()` is correct and must stay, but a
/// granule that quietly fell into it would allocate 49 MB again with every
/// other test in this file still green — the identity check above passes
/// either way, by construction. This is the check that cannot.
#[test]
fn every_shipped_granule_is_streamable() {
    for &product in MrmsProduct::all() {
        let bytes = gunzip(gz_for(product)).expect("gzip member");
        let grib2 = grib::from_reader(std::io::Cursor::new(&bytes)).expect("GRIB2 parses");
        let (_index, submessage) = grib2.iter().next().expect("one submessage");
        let plan = png_stream_plan(&bytes, &submessage, 7000 * 3500);
        let plan = plan.unwrap_or_else(|| {
            panic!(
                "{}: this granule falls back to grib's whole-image decode",
                product.as_str(),
            )
        });
        assert_eq!(
            plan.sample_bytes,
            2,
            "{}: MRMS packs 16 bits a sample",
            product.as_str(),
        );
        assert!(
            !plan.payload.is_empty(),
            "{}: an empty section 7 payload would decode to nothing",
            product.as_str(),
        );
    }
}

/// The decoded composite, once for the whole file. Most tests below are about
/// the grid rather than the values, and the two granules share every grid fact.
static FIXTURE: LazyLock<MrmsGrid> = LazyLock::new(|| decoded(MrmsProduct::ReflectivityComposite));

static RATE_FIXTURE: LazyLock<MrmsGrid> = LazyLock::new(|| decoded(MrmsProduct::PrecipRate));

fn decoded(product: MrmsProduct) -> MrmsGrid {
    let grib = gunzip(gz_for(product)).expect("the committed granule is a gzip member");
    parse_grib2(&grib, product).expect("the committed granule decodes")
}

fn fixture(product: MrmsProduct) -> &'static MrmsGrid {
    match product {
        MrmsProduct::ReflectivityComposite => &FIXTURE,
        MrmsProduct::PrecipRate => &RATE_FIXTURE,
    }
}

/// The **raw** values, sentinels and all — what `dispatch()` hands back before
/// [`to_reading`] touches them. Decoded separately from the fixtures on purpose:
/// a non-vacuity floor read off the mapped values could not tell "the granule
/// has no −999 in it" from "the mapping works".
static RAW: LazyLock<Vec<f32>> = LazyLock::new(|| raw_values(MrmsProduct::ReflectivityComposite));
static RATE_RAW: LazyLock<Vec<f32>> = LazyLock::new(|| raw_values(MrmsProduct::PrecipRate));

fn raw_values(product: MrmsProduct) -> Vec<f32> {
    let grib = gunzip(gz_for(product)).expect("gzip member");
    let ctx = grib::from_reader(std::io::Cursor::new(&grib)).expect("GRIB2");
    let (_i, submessage) = ctx.iter().next().expect("one submessage");
    let decoder = Grib2SubmessageDecoder::from(submessage).expect("decoder");
    decoder.dispatch().expect("dispatch").collect()
}

fn raw_for(product: MrmsProduct) -> &'static [f32] {
    match product {
        MrmsProduct::ReflectivityComposite => &RAW,
        MrmsProduct::PrecipRate => &RATE_RAW,
    }
}

fn count_near(values: &[f32], sentinel: f32) -> usize {
    values
        .iter()
        .filter(|v| (**v - sentinel).abs() < SENTINEL_EPSILON)
        .count()
}

/// How many points of `values` are one of `product`'s reserved codes.
fn count_missing(values: &[f32], product: MrmsProduct) -> usize {
    values
        .iter()
        .filter(|v| {
            product
                .missing_codes()
                .iter()
                .any(|c| (**v - c).abs() < SENTINEL_EPSILON)
        })
        .count()
}

// ── The non-vacuity floor ───────────────────────────────────────────────────

/// **The fixture actually carries the thing the next test says is handled.**
///
/// Without this, `mrms_no_coverage_paints_nothing` passes on a granule with no
/// −999 in it at all, which is the shape of vacuous verification: a check whose
/// subject is absent.
#[test]
fn every_fixture_really_contains_the_codes_it_is_here_to_prove() {
    for &product in MrmsProduct::all() {
        let raw = raw_for(product);
        let name = product.as_str();
        assert_eq!(
            raw.len(),
            7000 * 3500,
            "{name}: the fixture is the whole CONUS grid, or the shares below \
             mean nothing",
        );

        // **Every code the product declares must actually occur.** A code
        // nothing in the fixture uses is a code nothing tests — which is
        // exactly how the rate's −3 went unnoticed while the composite's −99
        // was checked.
        for &code in product.missing_codes() {
            let n = count_near(raw, code);
            assert!(
                n > 1_000_000,
                "{name}: only {n} points carry the reserved code {code}. \
                 Either the fixture does not exercise it or the product does \
                 not use it; both make the mapping untested for that code.",
            );
        }

        // And it is not ALL sentinel: there are real readings to leave alone.
        let missing = count_missing(raw, product);
        let real = raw.len() - missing;
        assert!(
            real > 10_000,
            "{name}: only {real} of {} points carry a reading; a fixture with \
             no weather cannot show that the mapping leaves readings alone",
            raw.len(),
        );

        // Nothing in the raw stream is NaN on its own — the fact the whole
        // module rests on. Section 6 carries `bitmap_indicator = 255`, so no
        // point is ever marked missing.
        assert_eq!(
            raw.iter().filter(|v| v.is_nan()).count(),
            0,
            "{name}: a raw value was already NaN, so the mapping is not the \
             only thing standing between a reserved code and the colour ramp \
             — re-read the module docs before trusting either",
        );
    }
}

/// **The two products' reserved sets are different, and both fixtures prove
/// it.** This is the test that would have caught the rate's −3 on the day the
/// layer was written.
#[test]
fn the_two_products_reserve_different_codes_and_each_fixture_shows_its_own() {
    // The rate reserves −3 and uses it for a third of the grid; the composite
    // does NOT, and carries genuine −3.0 dBZ returns instead.
    assert!(MrmsProduct::PrecipRate.missing_codes().contains(&-3.0));
    assert!(
        !MrmsProduct::ReflectivityComposite
            .missing_codes()
            .contains(&-3.0)
    );

    let rate_at_minus_three = count_near(&RATE_RAW, -3.0);
    assert!(
        rate_at_minus_three > 8_000_000,
        "the rate fixture holds {rate_at_minus_three} points at −3; it is the \
         product's no-coverage code and should be a third of the grid",
    );
    assert_eq!(
        count_near(&RATE_RAW, -99.0),
        0,
        "the rate uses no −99 at all — taking the composite's set for both is \
         what this asymmetry punishes",
    );
    assert_eq!(
        count_near(&RATE_RAW, -999.0),
        0,
        "the rate carries no −999 either, though −999 is the packing's own \
         reference value; declaring a code nothing uses is a claim nothing \
         checks",
    );

    // **The other direction, which is why this is a table and not a sign
    // test.** A blanket "negative means missing" rule would be right for a rain
    // rate and would erase real reflectivity.
    let composite_at_minus_three = count_near(&RAW, -3.0);
    assert!(
        composite_at_minus_three > 0,
        "the composite fixture carries no −3.0 dBZ returns, so this direction \
         is untested",
    );
    let composite_negative_readings = FIXTURE
        .grid
        .values
        .iter()
        .filter(|v| v.is_finite() && *v < 0.0)
        .count();
    assert!(
        composite_negative_readings > 1_000,
        "only {composite_negative_readings} negative readings survived the \
         composite's mapping; weak returns below 0 dBZ are real data and a \
         sign test would have taken all of them",
    );
    assert_eq!(
        RATE_FIXTURE
            .grid
            .values
            .iter()
            .filter(|v| v.is_finite() && *v < 0.0)
            .count(),
        0,
        "a negative precipitation rate survived the rate's mapping",
    );
}

// ── The sentinels ───────────────────────────────────────────────────────────

/// Every −999 and every −99 in the real granule became `NaN`, and every reading
/// survived untouched.
#[test]
fn the_reserved_codes_became_nan_and_nothing_else_moved() {
    for &product in MrmsProduct::all() {
        let raw = raw_for(product);
        let mapped = fixture(product).grid.values.to_f32();
        let name = product.as_str();
        assert_eq!(raw.len(), mapped.len());

        let expected_nan = count_missing(raw, product);
        let actual_nan = mapped.iter().filter(|v| v.is_nan()).count();
        assert_eq!(
            actual_nan, expected_nan,
            "{name}: the mapped grid has {actual_nan} missing points against \
             {expected_nan} reserved codes in the raw stream",
        );

        // Point for point, not just by count: a mapping that NaN'd the wrong
        // points in the right number would pass a count check.
        for (i, (&r, &m)) in raw.iter().zip(mapped.iter()).enumerate() {
            let reserved = product
                .missing_codes()
                .iter()
                .any(|c| (r - c).abs() < SENTINEL_EPSILON);
            if reserved {
                assert!(m.is_nan(), "{name}: point {i} raw {r} survived as {m}");
            } else {
                assert_eq!(m, r, "{name}: point {i} was a reading of {r}, now {m}");
            }
        }
    }
}

/// Over the real ramp, not over a belief about it: a missing point paints
/// nothing, for **every** MRMS product.
#[test]
fn mrms_no_coverage_paints_nothing() {
    for &product in MrmsProduct::all() {
        let paint = crate::render::gridded::field_paint(&crate::mrms::fields::spec(product).id)
            .expect("every registered product registers a paint");
        for &code in product.missing_codes() {
            let reading = to_reading(product, code);
            assert!(reading.is_nan(), "{code} decoded as {reading}");
            assert_eq!(
                paint.color_for_value(reading),
                [0, 0, 0, 0],
                "{}: a no-coverage point painted a visible colour",
                product.as_str(),
            );
            assert!(!paint.paints(reading));
        }
        // Non-triviality: the ramp is not transparent everywhere.
        let top = paint.scale.thresholds.last().expect("a bar has stops").0;
        assert_ne!(paint.color_for_value(top + 1.0), [0, 0, 0, 0]);
        assert_ne!(paint.color_for_value(top), [0, 0, 0, 0]);
    }
}

/// **What deleting the sentinel mapping would and would not change**, stated as
/// a test because the obvious answer is wrong.
///
/// The expected failure — an unmapped −999 clamping to the top of the bar and
/// painting the ocean solid — is what
/// `ModelParameter::color_for_value`'s unguarded `else` would do. It is **not**
/// what [`crate::render::gridded::color_for`] does: that ramp is transparent
/// below its first stop, and both MRMS bars start well above −99. So the picture
/// is unchanged and the damage lands on the *readings*.
///
/// Written down here rather than in prose alone, because a comment claiming a
/// failure the code cannot produce is how a reader learns to distrust the rest.
#[test]
fn an_unmapped_sentinel_would_change_the_reading_and_not_the_picture() {
    for &product in MrmsProduct::all() {
        let paint =
            crate::render::gridded::field_paint(&crate::mrms::fields::spec(product).id).unwrap();
        for &code in product.missing_codes() {
            assert_eq!(
                paint.color_for_value(code),
                paint.color_for_value(to_reading(product, code)),
                "{}: the mapping changes what {code} PAINTS, which contradicts \
                 this test's premise — re-read `to_reading`'s doc and correct \
                 whichever is now wrong",
                product.as_str(),
            );
            // The reading is the half that does move.
            assert_eq!(product.format_value(to_reading(product, code)), "");
            assert!(
                !product.format_value(code).is_empty(),
                "{}: an unmapped {code} really would be reported as a value",
                product.as_str(),
            );
        }
    }
}

/// The same, on the whole granule: the summary the status line and the blank
/// notice are built from must not carry −999 as the mosaic's minimum.
#[test]
fn the_summary_of_every_real_granule_reports_no_reserved_code_as_a_reading() {
    for &product in MrmsProduct::all() {
        let grid = fixture(product);
        let name = product.as_str();
        let (lo, hi) = grid.value_range.expect("a granule with readings in it");
        for &code in product.missing_codes() {
            assert!(
                (lo - code).abs() >= SENTINEL_EPSILON,
                "{name}: the reported minimum is {lo}, which is the reserved \
                 code {code} and not a measurement",
            );
        }
        assert!(
            hi > 0.0,
            "{name}: non-triviality — there is weather in {lo}..{hi}"
        );
        assert!(grid.blank_notice().is_none(), "{name}: this granule draws");
    }
}

/// A rain rate is never negative, so the rate's summary must not report one.
/// This is the assertion the live fetch failed before the codes became
/// per-product.
#[test]
fn the_rate_mosaic_reports_no_negative_rain() {
    let (lo, hi) = RATE_FIXTURE.value_range.expect("readings");
    assert!(
        lo >= 0.0,
        "the rate mosaic reports {lo} mm/h as its minimum"
    );
    assert!(
        hi > 1.0,
        "non-triviality: it is raining somewhere ({lo}..{hi})"
    );
}

/// The whole-granule statement of the same thing: not one drawn point of the
/// mosaic sits where the raw stream said "no coverage".
#[test]
fn no_drawn_point_of_the_real_mosaic_was_a_sentinel() {
    let paint = crate::render::gridded::field_paint(&FIXTURE.grid.field).expect("registered");
    let drawn = FIXTURE
        .grid
        .values
        .iter()
        .filter(|v| paint.paints(*v))
        .count();
    assert_eq!(
        drawn, FIXTURE.visible_points,
        "the summary and the ramp disagree about what draws",
    );
    assert!(
        drawn > 0,
        "the fixture draws nothing, so 'the sentinels do not draw' is vacuous",
    );
    // 24.5 M points and a national mosaic on a quiet night: a fraction of the
    // grid draws, never most of it. If the sentinel mapping were dropped this
    // would be ~100%.
    let share = drawn as f64 / FIXTURE.grid.values.len() as f64;
    assert!(
        share < 0.25,
        "{:.1}% of the mosaic draws — that is the solid-continent failure, \
         not weather",
        share * 100.0,
    );
}

/// A tolerance, not `==`: a sentinel that missed by one ULP is a solid
/// continent, and the packing's scale factor is upstream of us.
#[test]
fn a_reserved_code_a_hair_off_is_still_reserved() {
    use MrmsProduct::{PrecipRate, ReflectivityComposite};
    assert!(to_reading(ReflectivityComposite, -999.0).is_nan());
    assert!(to_reading(ReflectivityComposite, -999.02).is_nan());
    assert!(to_reading(ReflectivityComposite, -98.98).is_nan());
    assert!(to_reading(PrecipRate, -2.98).is_nan());

    // The window is tight enough that the reading next door survives. This is
    // the pair that matters: the composite's real −3.0 dBZ returns are 0.1
    // from −2.9 and −3.1, and the packing quantises to 0.1.
    assert_eq!(to_reading(ReflectivityComposite, -3.0), -3.0);
    assert_eq!(to_reading(ReflectivityComposite, -2.9), -2.9);
    assert!(!to_reading(PrecipRate, -3.1).is_nan());
    assert_eq!(to_reading(ReflectivityComposite, -95.0), -95.0);
    assert_eq!(to_reading(ReflectivityComposite, -998.0), -998.0);
    assert_eq!(to_reading(ReflectivityComposite, 0.0), 0.0);
    assert_eq!(to_reading(PrecipRate, 0.0), 0.0);
    assert_eq!(to_reading(ReflectivityComposite, 72.5), 72.5);

    // Non-finite in is missing out, so nothing downstream has to re-check.
    for &product in MrmsProduct::all() {
        assert!(to_reading(product, f32::INFINITY).is_nan());
        assert!(to_reading(product, f32::NAN).is_nan());
    }
}

// ── The grid ────────────────────────────────────────────────────────────────

/// **A reserved code a product does not declare must not occur at
/// coverage-mask scale.**
///
/// The floor above proves every *declared* code is exercised. This is the other
/// direction, and it is the one that would catch NOAA switching a product onto
/// a neighbour's code: a value from
/// [`MrmsProduct::known_reserved_codes`] filling a third of a grid is a mask,
/// whatever the table says.
///
/// The threshold is **0.1 % of the grid**, which leaves room for the real
/// readings that happen to land on a reserved number — the composite's 347
/// points at exactly −3.0 dBZ, 0.0014 % — while a coverage mask is 34 %. There
/// is no plausible product for which the two are confusable.
#[test]
fn no_undeclared_reserved_code_hides_in_a_fixture() {
    const MASK_SCALE: f64 = 0.001;
    for &product in MrmsProduct::all() {
        let raw = raw_for(product);
        let name = product.as_str();
        for &code in MrmsProduct::known_reserved_codes() {
            if product.missing_codes().contains(&code) {
                continue;
            }
            let n = count_near(raw, code);
            let share = n as f64 / raw.len() as f64;
            assert!(
                share < MASK_SCALE,
                "{name}: {n} points ({:.2} %) sit on {code}, which this product \
                 does not declare as reserved. At that scale it is a coverage \
                 mask, not a coincidence of readings — add it to \
                 `missing_codes` rather than shipping a third of a mosaic as \
                 measurements.",
                share * 100.0,
            );
        }
    }
}

/// Both products decode to the same grid, so every grid fact below reads off
/// one fixture and holds for the other.
#[test]
fn every_shipped_product_decodes_to_the_same_grid() {
    assert_eq!(FIXTURE.grid.coords, RATE_FIXTURE.grid.coords);
    assert_eq!(
        (FIXTURE.grid.ni, FIXTURE.grid.nj),
        (RATE_FIXTURE.grid.ni, RATE_FIXTURE.grid.nj),
    );
    assert_eq!(FIXTURE.bounds, RATE_FIXTURE.bounds);
}

/// Section 3, read as the closed form rather than as 392 MB of coordinates.
#[test]
fn the_grid_is_the_regular_arm_built_from_section_three() {
    assert_eq!((FIXTURE.grid.ni, FIXTURE.grid.nj), (7000, 3500));
    let crate::hrrr::GridCoords::Regular {
        lat0,
        lon0,
        dlat,
        dlon,
        ni,
        nj,
        scan_mode,
    } = FIXTURE.grid.coords
    else {
        panic!(
            "MRMS decoded to {:?}, not the regular arm — the explicit arm is \
             392 MB of coordinates AND turns windowing off",
            FIXTURE.grid.coords,
        );
    };
    assert_eq!((ni, nj), (7000, 3500));
    // The measured corner pair, wrapped into [-180, 180).
    assert!((lat0 - 54.995).abs() < 1e-6, "lat0 = {lat0}");
    assert!((lon0 - -129.995).abs() < 1e-6, "lon0 = {lon0}");
    // 0.01°, and the mosaic scans **south**: row 0 is the north edge.
    assert!((dlat - -0.01).abs() < 1e-9, "dlat = {dlat}");
    assert!((dlon - 0.01).abs() < 1e-9, "dlon = {dlon}");
    // i-consecutive, non-alternating: the two bits `index_bounds` refuses on.
    assert_eq!(scan_mode, 0, "scan mode {scan_mode:#010b}");
}

/// The one property the whole memory posture rests on: a regular grid answers
/// `index_bounds`, so `projection_window` cuts the grid instead of walking all
/// 24.5 M points per render.
#[test]
fn the_grid_can_be_windowed_which_the_explicit_arm_could_not() {
    let viewport = squallar_geo::GeoBounds {
        min_lat: 35.0,
        max_lat: 36.0,
        min_lon: -98.0,
        max_lon: -97.0,
    };
    let bounds = FIXTURE
        .grid
        .coords
        .index_bounds(&viewport, FIXTURE.grid.ni, FIXTURE.grid.nj)
        .expect("a regular grid answers index_bounds; the explicit arm returns None");
    let (i0, i1, j0, j1) = bounds;
    assert!(i1 > i0 && j1 > j0, "{bounds:?}");
    // A 1° viewport over a 70° × 35° grid: a hundredth of the columns.
    assert!(
        (i1 - i0) < 200.0 && (j1 - j0) < 200.0,
        "a 1° window took {} × {} cells",
        i1 - i0,
        j1 - j0,
    );
    assert!(FIXTURE.grid.coords.cell_span_degrees(35.0).is_some());
    assert!(!FIXTURE.grid.coords.wraps_longitude());
}

/// Round-trip: the closed form and the scan order agree, so a value read at an
/// index lands where the coordinates say it does.
#[test]
fn nearest_is_the_inverse_of_at_on_the_real_grid() {
    let coords = &FIXTURE.grid.coords;
    for index in [0usize, 1, 7000, 12_345_678, 24_499_999] {
        let (lat, lon) = coords.at(index).expect("in range");
        assert_eq!(coords.nearest(lat, lon), Some(index), "index {index}");
    }
    assert_eq!(coords.len(), 24_500_000);
    assert_eq!(coords.at(24_500_000), None);
}

/// The envelope the layer declares really does bracket the granule, and the
/// domain check the fetch path runs really does pass on it.
#[test]
fn the_real_granule_sits_inside_the_declared_envelope() {
    let b = FIXTURE.bounds;
    assert!((b.min_lon - -129.995).abs() < 1e-4, "{b:?}");
    assert!((b.max_lon - -60.005).abs() < 1e-4, "{b:?}");
    assert!((b.min_lat - 20.005).abs() < 1e-4, "{b:?}");
    assert!((b.max_lat - 54.995).abs() < 1e-4, "{b:?}");
    crate::hrrr::fetch::check_domain_longitude(&b, &crate::mrms::MRMS_DOMAIN_LON, "MRMS")
        .expect("the shipped envelope must accept the shipped product");
}

/// Section 1's reference time is the key's own stamp.
#[test]
fn the_valid_time_is_the_one_in_the_key() {
    assert_eq!(
        FIXTURE.valid,
        chrono::NaiveDate::from_ymd_opt(2026, 8, 21)
            .unwrap()
            .and_hms_opt(0, 0, 39)
            .unwrap(),
    );
}

/// The residency figure the cache budget is spent against is the real one.
///
/// **The literal moved 98,000,000 -> 49,000,000 with the store**, and both
/// halves are here on purpose: the first line holds the *real decoded granule*
/// against the constant every budget is stated in, and the second holds that
/// constant against a figure written out by hand, so a store and a budget that
/// drifted together still fail here.
#[test]
fn one_resident_grid_is_the_forty_nine_megabytes_the_budget_assumes() {
    assert_eq!(FIXTURE.resident_bytes(), crate::mrms::CONUS_GRID_BYTES);
    assert_eq!(FIXTURE.resident_bytes(), 49_000_000);
}

// ── Refusals ────────────────────────────────────────────────────────────────

#[test]
fn a_truncated_granule_is_refused_rather_than_half_decoded() {
    let grib = gunzip(COMPOSITE_GZ).unwrap();
    let err = parse_grib2(&grib[..grib.len() / 2], MrmsProduct::ReflectivityComposite)
        .expect_err("half a granule is not a mosaic");
    assert!(err.contains("MRMS"), "{err}");
}

#[test]
fn a_body_that_is_not_gzip_is_refused() {
    assert!(gunzip(b"").is_err());
    assert!(gunzip(b"MRMS but not compressed").is_err());
}

/// A degenerate section 3 must not produce a grid with a zero step: `dlon == 0`
/// makes `index_bounds` divide by zero and `nearest` answer nonsense.
#[test]
fn a_single_column_grid_is_refused_rather_than_given_an_undefined_step() {
    let one_row = param_set::LatLonGrid {
        grid: param_set::Grid {
            ni: 1,
            nj: 1,
            initial_production_domain_basic_angle: 0,
            basic_angle_subdivisions: 0xffff_ffff,
            first_point_lat: 35_000_000,
            first_point_lon: 260_000_000,
            resolution_and_component_flags: param_set::ResolutionAndComponentFlags(0b0011_0000),
            last_point_lat: 35_000_000,
            last_point_lon: 260_000_000,
        },
        i_direction_inc: 0xffff_ffff,
        j_direction_inc: 0xffff_ffff,
        scanning_mode: param_set::ScanningMode(0),
    };
    assert!(regular_grid(&one_row).is_err());
}

/// Longitudes east of 180 in the octets come back wrapped, and the step is not
/// wrapped with them.
#[test]
fn a_westward_domain_wraps_its_origin_and_keeps_its_step() {
    let conus = param_set::LatLonGrid {
        grid: param_set::Grid {
            ni: 7000,
            nj: 3500,
            initial_production_domain_basic_angle: 0,
            basic_angle_subdivisions: 0xffff_ffff,
            first_point_lat: 54_995_000,
            first_point_lon: 230_005_000,
            resolution_and_component_flags: param_set::ResolutionAndComponentFlags(0b0011_0000),
            last_point_lat: 20_005_000,
            last_point_lon: 299_995_000,
        },
        i_direction_inc: 10_000,
        j_direction_inc: 10_000,
        scanning_mode: param_set::ScanningMode(0),
    };
    let crate::hrrr::GridCoords::Regular { lon0, dlon, .. } =
        regular_grid(&conus).expect("the real section 3 builds")
    else {
        panic!("not the regular arm");
    };
    assert!((lon0 - -129.995).abs() < 1e-6, "{lon0}");
    assert!(dlon > 0.0 && (dlon - 0.01).abs() < 1e-9, "{dlon}");
}

/// **Every value the mosaic holds is a function of a 16-bit code and three
/// per-granule scalars** — so an `f32` per point is a *widening of the source's
/// own width*, and storing the code beside `(ref_val, exp, dec)` would be a
/// repacking rather than a quantisation.
///
/// The premise a halved [`ResidentGrid`](crate::render::gridded::ResidentGrid)
/// would rest on, checked rather than assumed. What it pins, per shipped
/// product and over the **whole** 24.5 M-point mosaic:
///
/// * **the code is 16 bits.** `num_bits == 16` and `orig_field_type == 0`, so a
///   `u16` holds every code section 7 can carry. Not decoration:
///   [`png_stream_plan`] admits any non-zero multiple of eight, and a granule
///   published at 24 or 32 bits would need a wider code — a scaled-integer
///   store must select its width off this field, never assume it;
/// * **the value is recovered exactly.** `(ref_val + code * 2^exp) * 10^-dec`
///   run over the streamed code reproduces the shipped `f32` **bit for bit**
///   (`to_bits`, so a `NaN` by another route and a `-0.0` against `0.0` are
///   both differences), including the reserved codes
///   [`reading`] turns into `NaN`.
///
/// **The arithmetic on both sides is deliberately the same expression**, and
/// that is the point rather than a weakness: what is being pinned is that the
/// shipped `f32` carries no information the `(code, ref_val, exp, dec)` tuple
/// does not, not that two implementations agree. The claim that could fail here
/// is the *width* one, and it fails loudly.
///
/// Streamed row by row rather than collected: a `Vec<u16>` of the mosaic is
/// 49,000,000 B, which is exactly the buffer this module exists to not
/// allocate.
///
/// Non-vacuity: the full point count on both sides, at least a million finite
/// readings, and at least a hundred distinct codes — a granule of one repeated
/// code would satisfy every equality above.
#[test]
fn every_mosaic_value_is_a_sixteen_bit_code_and_three_scalars() {
    for &product in MrmsProduct::all() {
        let name = product.as_str();
        let missing = product.missing_codes();
        let shipped = &fixture(product).grid.values;
        let points = 7000 * 3500;
        assert_eq!(shipped.len(), points, "{name}: not the whole mosaic");

        let bytes = gunzip(gz_for(product)).expect("gzip member");
        let grib2 = grib::from_reader(std::io::Cursor::new(&bytes)).expect("GRIB2 parses");
        let (_index, submessage) = grib2.iter().next().expect("one submessage");
        let plan = png_stream_plan(&bytes, &submessage, points)
            .unwrap_or_else(|| panic!("{name}: not on the streaming arm"));

        assert_eq!(
            plan.simple.num_bits, 16,
            "{name}: a {} -bit code does not fit a u16",
            plan.simple.num_bits,
        );
        assert_eq!(plan.sample_bytes, 2, "{name}: 16 bits is two bytes");

        let two_pow = 2_f32.powi(i32::from(plan.simple.exp));
        let dig_factor = 10_f32.powi(-i32::from(plan.simple.dec));
        let ref_val = plan.simple.ref_val;

        let decoder = png::Decoder::new(std::io::Cursor::new(plan.payload));
        let mut reader = decoder.read_info().expect("section 7 is a PNG");
        let line = reader
            .output_line_size(reader.info().width)
            .expect("a line size");
        let mut row = vec![0u8; line];
        let mut seen = std::collections::HashSet::new();
        let mut finite = 0usize;
        let mut checked = 0usize;
        while reader.read_row(&mut row).expect("a PNG row").is_some() {
            for sample in row.chunks_exact(2) {
                let code = u16::from_be_bytes([sample[0], sample[1]]);
                let rebuilt = reading(missing, (ref_val + f32::from(code) * two_pow) * dig_factor);
                let held = shipped.get(checked).expect("inside the mosaic");
                assert_eq!(
                    rebuilt.to_bits(),
                    held.to_bits(),
                    "{name}: point {checked} holds {held} and code {code} \
                     rebuilds {rebuilt}",
                );
                if held.is_finite() {
                    finite += 1;
                }
                seen.insert(code);
                checked += 1;
            }
        }

        assert_eq!(
            checked, points,
            "{name}: {checked} codes for {points} points"
        );
        assert!(
            finite > 1_000_000,
            "{name}: only {finite} finite readings; a mosaic of holes would \
             satisfy every equality above",
        );
        println!(
            "{name}: {} distinct codes over {points} points, {finite} finite",
            seen.len(),
        );
        assert!(
            seen.len() > 100,
            "{name}: only {} distinct codes; a granule of one repeated code \
             proves nothing about the map",
            seen.len(),
        );
    }
}
