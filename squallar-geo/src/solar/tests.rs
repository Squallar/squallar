//! The sun, checked against a published almanac rather than against itself.
//!
//! Every reference number in this file was pulled from the **US Naval
//! Observatory** Astronomical Applications API and is quoted here verbatim.
//! Two endpoints, because they pin different things:
//!
//! * `api/celnav` — the sun's apparent declination, Greenwich hour angle,
//!   computed altitude `hc` and true azimuth `zn` at a *named instant*. `hc` is
//!   the geometric altitude of the sun's centre: in celestial navigation the
//!   refraction, semidiameter and parallax corrections are published beside it
//!   as separate terms to apply to a sextant reading, never folded into it. It
//!   is therefore exactly the quantity [`solar_position`] returns.
//! * `api/rstt/oneday` — sunrise, sunset and civil twilight to the minute,
//!   which pin the **below-horizon** branch that `celnav` cannot: it omits the
//!   sun from its list entirely once the sun has set.
//!
//! **Site diversity is a rule here, not a courtesy.** [`ALMANAC`] is 15 rows
//! across 14 distinct places (Tromsø appears twice, at noon and at midnight),
//! spanning 69.6 °N to 77.8 °S, both hemispheres, the equator, both solstices,
//! both equinoxes, longitudes from 166.7 °E to 105.0 °W, and **150 years**,
//! from 1900 to 2050. Singapore is held out of every other test in this file;
//! Reykjavík is held out of [`ALMANAC`] and appears only in [`HORIZON`].
//! Nothing in this module has a fitted parameter — the arithmetic is published
//! theory — so a holdout guards against a *test* tuned to a site, not a model
//! tuned to one.

use super::*;

/// Absolute angular tolerance on every almanac comparison, in degrees.
///
/// **The denominator matters and an earlier version of this file did not name
/// one.** It quoted "worst residual 0.0040°" and set the pin at 0.01°, calling
/// that 2.5× headroom. Both figures were true only of the eleven rows that
/// happened to be in the table. Over the domain the module actually serves they
/// are wrong: **51.7 % of instants exceed 0.0040°**, and 0.01° is exceeded
/// 8.6 % of the time, so the old pin was 0.76× the real error and stayed green
/// only because no row in the table sat near the envelope. Two of the rows below
/// — `worst instant in the swept domain` and `2026-10-28` — redden it.
///
/// **The envelope, with its denominator.** Against a JPL DE421 apparent place,
/// over **947,376 site-instants** — 10-minute steps through all of 2000 and all
/// of 2026, nine sites including both poles, the equator and the antimeridian —
/// the worst disagreement is **0.01323° of Greenwich hour angle** and
/// **0.01298° of arc on the sky**, with a mean of 0.0048°. Nothing in that sweep
/// exceeds 0.02°. Rows at 1900 and 2050 in the table below were then checked
/// against live USNO and land at 0.0119° and 0.0041°, so the envelope is not an
/// artefact of the two years swept.
///
/// So the pin is the measured envelope with ~1.5× headroom, not a number chosen
/// to fit a sample. Two irreducible parts of it are worth naming: this module
/// treats UTC as UT1, which is worth up to 0.0037° on its own, and the abridged
/// theory's own truncation carries the rest.
///
/// **Why that is fit for purpose.** 0.0133° is a fortieth of the sun's disc.
/// Through a Lambert cosine it is ~0.02 of an 8-bit level on flat ground and
/// ~0.5 of a level on the steepest segment of [`SKY_STOPS`], and it moves a
/// terminator by a few seconds. This pin is about the test being honest, not
/// about the picture being at risk.
///
/// `a_ten_second_stale_clock_breaks_every_almanac_row` is what stops this number
/// being decorative, and `the_stale_clock_floor_sits_close_above_the_pin` is
/// what stops the *floor* being decorative in turn.
const ALMANAC_TOL_DEG: f64 = 0.02;

/// One published almanac observation of the sun.
struct AlmanacRow {
    site: &'static str,
    lat: f64,
    /// East-positive.
    lon: f64,
    /// `(year, month, day, hour, minute)` UTC.
    utc: (i64, u32, u32, u32, u32),
    /// USNO `dec`: apparent declination, degrees.
    dec_deg: f64,
    /// USNO `gha`: Greenwich hour angle, degrees west of Greenwich, `0..360`.
    gha_deg: f64,
    /// USNO `hc`: geometric altitude of the sun's centre, degrees.
    altitude_deg: f64,
    /// USNO `zn`: true azimuth, degrees clockwise from north.
    azimuth_deg: f64,
}

/// USNO `api/celnav`. Do not re-record a row to make a changed implementation
/// pass — a moved value here is a defect in the arithmetic, not a stale pin.
const ALMANAC: [AlmanacRow; 15] = [
    // Above the Arctic Circle at the June solstice: the midnight sun, and the
    // one case where a sign error in the hour angle is invisible at noon but
    // catastrophic at midnight. Both are pinned.
    AlmanacRow {
        site: "Tromsø, Norway — June solstice, local noon",
        lat: 69.6496,
        lon: 18.9560,
        utc: (2026, 6, 21, 12, 0),
        dec_deg: 23.437851,
        gha_deg: 359.545561,
        altitude_deg: 42.493312,
        azimuth_deg: 203.256883,
    },
    AlmanacRow {
        site: "Tromsø, Norway — June solstice, local midnight (sun still up)",
        lat: 69.6496,
        lon: 18.9560,
        utc: (2026, 6, 21, 0, 0),
        dec_deg: 23.437521,
        gha_deg: 179.572878,
        altitude_deg: 4.036601,
        azimuth_deg: 16.995034,
    },
    // The equator at the March equinox: declination through zero, and an
    // altitude of 84.7° where azimuth is badly conditioned — which is why the
    // assertion below is on separation, not on azimuth.
    AlmanacRow {
        site: "Quito, Ecuador — March equinox",
        lat: -0.1807,
        lon: -78.4678,
        utc: (2026, 3, 20, 17, 0),
        dec_deg: 0.036933,
        gha_deg: 73.156129,
        altitude_deg: 84.683879,
        azimuth_deg: 87.658764,
    },
    // Southern hemisphere at the December solstice: the sun transits to the
    // *north* and the azimuth wraps through 360 seven minutes after transit.
    AlmanacRow {
        site: "Sydney, Australia — December solstice",
        lat: -33.8688,
        lon: 151.2093,
        utc: (2026, 12, 21, 2, 0),
        dec_deg: -23.435012,
        gha_deg: 210.535291,
        altitude_deg: 79.455075,
        azimuth_deg: 351.220440,
    },
    AlmanacRow {
        site: "Ushuaia, Argentina — September equinox",
        lat: -54.8019,
        lon: -68.3030,
        utc: (2026, 9, 22, 15, 0),
        dec_deg: 0.147237,
        gha_deg: 46.829728,
        altitude_deg: 32.296068,
        azimuth_deg: 25.661986,
    },
    AlmanacRow {
        site: "Denver, Colorado — June solstice",
        lat: 39.7400,
        lon: -104.9800,
        utc: (2026, 6, 21, 19, 0),
        dec_deg: 23.437247,
        gha_deg: 104.529653,
        altitude_deg: 73.692799,
        azimuth_deg: 178.528306,
    },
    // KTLX, the acceptance site the plan names for "flat ground still reads as
    // correct", at the shallowest sun of its year.
    AlmanacRow {
        site: "KTLX Norman, Oklahoma — December solstice",
        lat: 35.3331,
        lon: -97.2778,
        utc: (2026, 12, 21, 18, 30),
        dec_deg: -23.437379,
        gha_deg: 97.950177,
        altitude_deg: 31.226068,
        azimuth_deg: 180.721418,
    },
    AlmanacRow {
        site: "Cape Town, South Africa — March equinox, low morning sun",
        lat: -33.9249,
        lon: 18.4241,
        utc: (2026, 3, 20, 6, 0),
        dec_deg: -0.144306,
        gha_deg: 268.122407,
        altitude_deg: 13.751892,
        azimuth_deg: 80.706981,
    },
    AlmanacRow {
        site: "Greenwich — 2000-01-01",
        lat: 51.4779,
        lon: -0.0015,
        utc: (2000, 1, 1, 12, 0),
        dec_deg: -23.032432,
        gha_deg: 359.178715,
        altitude_deg: 15.486154,
        azimuth_deg: 179.214281,
    },
    AlmanacRow {
        site: "McMurdo, Antarctica — December solstice, high southern latitude",
        lat: -77.8463,
        lon: 166.6683,
        utc: (2026, 12, 21, 0, 0),
        dec_deg: -23.434474,
        gha_deg: 180.545602,
        altitude_deg: 35.251400,
        azimuth_deg: 14.397833,
    },
    // ── The four rows below exist because the sample above was comfortable. ──
    // The first two are the worst instants in a 947,376-point DE421 sweep, and
    // both exceed the 0.01° pin this file used to carry. Adding them is the
    // point: a table that cannot redden its own tolerance is not a check.
    AlmanacRow {
        site: "20 °N 72.5 °E — the worst instant in the swept domain",
        lat: 20.0,
        lon: 72.5,
        utc: (2026, 2, 10, 7, 10),
        dec_deg: -14.321022,
        gha_deg: 283.958089,
        altitude_deg: 55.502644,
        azimuth_deg: 173.933137,
    },
    AlmanacRow {
        site: "40 °N 0 °E — the opposite-signed lobe of the same error",
        lat: 40.0,
        lon: 0.0,
        utc: (2026, 10, 28, 12, 0),
        dec_deg: -13.214583,
        gha_deg: 4.053619,
        altitude_deg: 36.652064,
        azimuth_deg: 184.920807,
    },
    // The time lever. A 26-year span was described in an earlier version of this
    // file as testing the Julian-century terms; it does not, and neither does
    // 150 years — see `the_almanac_table_is_diverse_enough_to_arbitrate` for the
    // measured reason. What these two *do* pin is a gross scale error in the
    // century length, and they extend the era span from 26 years to 150.
    AlmanacRow {
        site: "20 °S 60 °W — 1900, the early end of the 150-year lever",
        lat: -20.0,
        lon: -60.0,
        utc: (1900, 9, 17, 15, 0),
        dec_deg: 2.289307,
        gha_deg: 46.374602,
        altitude_deg: 64.008039,
        azimuth_deg: 32.486985,
    },
    AlmanacRow {
        site: "45 °N 10 °E — 2050, the late end of the 150-year lever",
        lat: 45.0,
        lon: 10.0,
        utc: (2050, 6, 15, 11, 0),
        dec_deg: 23.319412,
        gha_deg: 344.852120,
        altitude_deg: 67.916756,
        azimuth_deg: 167.339811,
    },
    // HOLDOUT: near-equatorial, far east, and on no solstice or equinox — an
    // ordinary day, deliberately unlike every row above it, and named in no
    // other test.
    AlmanacRow {
        site: "Singapore — HOLDOUT, an ordinary day",
        lat: 1.3521,
        lon: 103.8198,
        utc: (2026, 8, 30, 6, 0),
        dec_deg: 8.977514,
        gha_deg: 269.821528,
        altitude_deg: 74.429573,
        azimuth_deg: 299.788794,
    },
];

/// One published rise/set/twilight time. `target_deg` is the geometric centre
/// altitude the phenomenon is *defined* at, which is why no refraction model
/// appears anywhere in this crate: −0.8333° already contains the 34′ of
/// standard refraction and the 16′ semidiameter, and civil twilight is 6.0°
/// flat by definition.
struct HorizonRow {
    site: &'static str,
    lat: f64,
    lon: f64,
    /// `(year, month, day, hour, minute)` UTC, as USNO prints it.
    utc: (i64, u32, u32, u32, u32),
    phenomenon: &'static str,
    target_deg: f64,
}

const SUNRISE: f64 = -0.8333;
const CIVIL: f64 = -6.0;

/// USNO `api/rstt/oneday` with `tz=0`. Six sites, four phenomena each;
/// Reykjavík is a holdout that appears in no other test.
const HORIZON: [HorizonRow; 24] = [
    HorizonRow {
        site: "Reykjavík (HOLDOUT)",
        lat: 64.1466,
        lon: -21.9426,
        utc: (2026, 10, 15, 7, 29),
        phenomenon: "begin civil twilight",
        target_deg: CIVIL,
    },
    HorizonRow {
        site: "Reykjavík (HOLDOUT)",
        lat: 64.1466,
        lon: -21.9426,
        utc: (2026, 10, 15, 8, 18),
        phenomenon: "sunrise",
        target_deg: SUNRISE,
    },
    HorizonRow {
        site: "Reykjavík (HOLDOUT)",
        lat: 64.1466,
        lon: -21.9426,
        utc: (2026, 10, 15, 18, 8),
        phenomenon: "sunset",
        target_deg: SUNRISE,
    },
    HorizonRow {
        site: "Reykjavík (HOLDOUT)",
        lat: 64.1466,
        lon: -21.9426,
        utc: (2026, 10, 15, 18, 56),
        phenomenon: "end civil twilight",
        target_deg: CIVIL,
    },
    HorizonRow {
        site: "KTLX Norman",
        lat: 35.3331,
        lon: -97.2778,
        utc: (2026, 12, 21, 13, 6),
        phenomenon: "begin civil twilight",
        target_deg: CIVIL,
    },
    HorizonRow {
        site: "KTLX Norman",
        lat: 35.3331,
        lon: -97.2778,
        utc: (2026, 12, 21, 13, 34),
        phenomenon: "sunrise",
        target_deg: SUNRISE,
    },
    HorizonRow {
        site: "KTLX Norman",
        lat: 35.3331,
        lon: -97.2778,
        utc: (2026, 12, 21, 23, 20),
        phenomenon: "sunset",
        target_deg: SUNRISE,
    },
    HorizonRow {
        site: "KTLX Norman",
        lat: 35.3331,
        lon: -97.2778,
        utc: (2026, 12, 21, 23, 49),
        phenomenon: "end civil twilight",
        target_deg: CIVIL,
    },
    HorizonRow {
        site: "Sydney",
        lat: -33.8688,
        lon: 151.2093,
        utc: (2026, 12, 21, 9, 5),
        phenomenon: "sunset",
        target_deg: SUNRISE,
    },
    HorizonRow {
        site: "Sydney",
        lat: -33.8688,
        lon: 151.2093,
        utc: (2026, 12, 21, 9, 35),
        phenomenon: "end civil twilight",
        target_deg: CIVIL,
    },
    HorizonRow {
        site: "Sydney",
        lat: -33.8688,
        lon: 151.2093,
        utc: (2026, 12, 21, 18, 12),
        phenomenon: "begin civil twilight",
        target_deg: CIVIL,
    },
    HorizonRow {
        site: "Sydney",
        lat: -33.8688,
        lon: 151.2093,
        utc: (2026, 12, 21, 18, 41),
        phenomenon: "sunrise",
        target_deg: SUNRISE,
    },
    HorizonRow {
        site: "Quito",
        lat: -0.1807,
        lon: -78.4678,
        utc: (2026, 3, 20, 10, 57),
        phenomenon: "begin civil twilight",
        target_deg: CIVIL,
    },
    HorizonRow {
        site: "Quito",
        lat: -0.1807,
        lon: -78.4678,
        utc: (2026, 3, 20, 11, 18),
        phenomenon: "sunrise",
        target_deg: SUNRISE,
    },
    HorizonRow {
        site: "Quito",
        lat: -0.1807,
        lon: -78.4678,
        utc: (2026, 3, 20, 23, 24),
        phenomenon: "sunset",
        target_deg: SUNRISE,
    },
    HorizonRow {
        site: "Quito",
        lat: -0.1807,
        lon: -78.4678,
        utc: (2026, 3, 20, 23, 45),
        phenomenon: "end civil twilight",
        target_deg: CIVIL,
    },
    HorizonRow {
        site: "Ushuaia",
        lat: -54.8019,
        lon: -68.3030,
        utc: (2026, 9, 22, 9, 46),
        phenomenon: "begin civil twilight",
        target_deg: CIVIL,
    },
    HorizonRow {
        site: "Ushuaia",
        lat: -54.8019,
        lon: -68.3030,
        utc: (2026, 9, 22, 10, 21),
        phenomenon: "sunrise",
        target_deg: SUNRISE,
    },
    HorizonRow {
        site: "Ushuaia",
        lat: -54.8019,
        lon: -68.3030,
        utc: (2026, 9, 22, 22, 31),
        phenomenon: "sunset",
        target_deg: SUNRISE,
    },
    HorizonRow {
        site: "Ushuaia",
        lat: -54.8019,
        lon: -68.3030,
        utc: (2026, 9, 22, 23, 7),
        phenomenon: "end civil twilight",
        target_deg: CIVIL,
    },
    HorizonRow {
        site: "Denver",
        lat: 39.7400,
        lon: -104.9800,
        utc: (2026, 6, 21, 2, 31),
        phenomenon: "sunset",
        target_deg: SUNRISE,
    },
    HorizonRow {
        site: "Denver",
        lat: 39.7400,
        lon: -104.9800,
        utc: (2026, 6, 21, 3, 4),
        phenomenon: "end civil twilight",
        target_deg: CIVIL,
    },
    HorizonRow {
        site: "Denver",
        lat: 39.7400,
        lon: -104.9800,
        utc: (2026, 6, 21, 10, 59),
        phenomenon: "begin civil twilight",
        target_deg: CIVIL,
    },
    HorizonRow {
        site: "Denver",
        lat: 39.7400,
        lon: -104.9800,
        utc: (2026, 6, 21, 11, 32),
        phenomenon: "sunrise",
        target_deg: SUNRISE,
    },
];

/// Unix seconds for a UTC calendar instant — Howard Hinnant's `days_from_civil`,
/// which is exact for every proleptic-Gregorian date and needs no dependency.
/// `the_calendar_helper_is_itself_pinned` is the check on it.
fn unix_utc(year: i64, month: u32, day: u32, hour: u32, minute: u32) -> f64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = i64::from(if month > 2 { month - 3 } else { month + 9 });
    let doy = (153 * mp + 2) / 5 + i64::from(day) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    (days * 86_400 + i64::from(hour) * 3_600 + i64::from(minute) * 60) as f64
}

/// `solar_position`, for the rows that are inside the supported window by
/// construction. Panics rather than returning, so a row that drifts out of the
/// window is loud instead of silently skipped.
fn position_at(lat: f64, lon: f64, unix_seconds: f64) -> SolarPosition {
    solar_position(lat, lon, unix_seconds).unwrap_or_else(|| {
        panic!("({lat}, {lon}) at {unix_seconds} is outside the supported window")
    })
}

/// Angular distance between two sky directions, in degrees.
///
/// The almanac comparison is made on **separation**, not on azimuth, and that
/// is the whole reason this helper exists. Azimuth is ill-conditioned near the
/// zenith — an 0.002° position error at Quito's 84.7° altitude becomes 0.023°
/// of azimuth, and a per-component azimuth tolerance would then have to be
/// loosened by an order of magnitude for every row to accommodate one. Angular
/// separation is the physically meaningful quantity, is uniformly conditioned,
/// and catches every error azimuth would have caught.
fn sky_separation_deg(el_a: f64, az_a: f64, el_b: f64, az_b: f64) -> f64 {
    let unit = |el: f64, az: f64| {
        let (sin_el, cos_el) = el.to_radians().sin_cos();
        let (sin_az, cos_az) = az.to_radians().sin_cos();
        [cos_el * sin_az, cos_el * cos_az, sin_el]
    };
    let (a, b) = (unit(el_a, az_a), unit(el_b, az_b));
    (a[0] * b[0] + a[1] * b[1] + a[2] * b[2])
        .clamp(-1.0, 1.0)
        .acos()
        .to_degrees()
}

/// Signed difference of two angles, carried into `-180..180`.
fn angle_delta(a: f64, b: f64) -> f64 {
    (a - b + 180.0).rem_euclid(360.0) - 180.0
}

/// Greenwich hour angle from a local one: LHA is measured west of the *local*
/// meridian, GHA west of Greenwich, and east longitude is what separates them.
fn greenwich_hour_angle_deg(position: &SolarPosition, lon_deg: f64) -> f64 {
    (position.hour_angle_deg - lon_deg).rem_euclid(360.0)
}

/// A ramp function under the name it is reported by.
type NamedRamp = (&'static str, fn(f64) -> [f64; 3]);

/// Relative luminance of a linear-light RGB triple, Rec. 709 coefficients.
fn luminance(rgb: [f64; 3]) -> f64 {
    0.2126 * rgb[0] + 0.7152 * rgb[1] + 0.0722 * rgb[2]
}

/// How far a row's clock is nudged to prove the almanac pin can see an error.
///
/// **Ten seconds, not sixty.** At sixty the guard was 22× above the pin it
/// protects: `ALMANAC_TOL_DEG` could be raised from 0.01 all the way to 0.22
/// with every test in this file still green, because a minute of Earth rotation
/// is 0.25° and the floor never noticed. Ten seconds is 0.0417° of hour angle,
/// which sits just about 2× over the pin — close enough that loosening the pin
/// materially is caught almost immediately.
/// `the_stale_clock_floor_sits_close_above_the_pin` asserts that ratio rather
/// than leaving it to this comment.
const STALE_CLOCK_SECONDS: f64 = 10.0;

#[test]
fn the_calendar_helper_is_itself_pinned() {
    assert_eq!(unix_utc(1970, 1, 1, 0, 0), 0.0);
    assert_eq!(unix_utc(2000, 1, 1, 12, 0), 946_728_000.0);
    assert_eq!(unix_utc(2026, 8, 30, 0, 0), 1_788_048_000.0);
    // A leap day, and the day after it, one 86 400 s step apart.
    assert_eq!(
        unix_utc(2024, 3, 1, 0, 0) - unix_utc(2024, 2, 29, 0, 0),
        86_400.0
    );
    // 1900 is not a leap year under the Gregorian rule; 2000 is.
    assert_eq!(
        unix_utc(1900, 3, 1, 0, 0) - unix_utc(1900, 2, 28, 0, 0),
        86_400.0
    );
    assert_eq!(
        unix_utc(2000, 3, 1, 0, 0) - unix_utc(2000, 2, 28, 0, 0),
        2.0 * 86_400.0
    );
}

#[test]
fn the_julian_day_of_j2000_is_j2000() {
    assert_eq!(julian_day(unix_utc(2000, 1, 1, 12, 0)), 2_451_545.0);
    assert_eq!(julian_day(0.0), 2_440_587.5);
}

/// The flagship: declination, Greenwich hour angle and sky position against
/// fifteen published USNO observations.
#[test]
fn the_position_matches_the_usno_almanac_at_fifteen_instants() {
    let mut worst = (0.0_f64, 0.0_f64, 0.0_f64);
    for row in &ALMANAC {
        let (y, mo, d, h, mi) = row.utc;
        let at = unix_utc(y, mo, d, h, mi);
        let got = position_at(row.lat, row.lon, at);

        let d_dec = (got.declination_deg - row.dec_deg).abs();
        let d_gha = angle_delta(greenwich_hour_angle_deg(&got, row.lon), row.gha_deg).abs();
        let sep = sky_separation_deg(
            got.elevation_deg,
            got.azimuth_deg,
            row.altitude_deg,
            row.azimuth_deg,
        );

        assert!(
            d_dec <= ALMANAC_TOL_DEG,
            "{}: declination {:.6}° against the almanac's {:.6}° — {d_dec:.6}° out, \
             over the {ALMANAC_TOL_DEG}° pin",
            row.site,
            got.declination_deg,
            row.dec_deg,
        );
        assert!(
            d_gha <= ALMANAC_TOL_DEG,
            "{}: Greenwich hour angle {:.6}° against the almanac's {:.6}° — {d_gha:.6}° out, \
             over the {ALMANAC_TOL_DEG}° pin",
            row.site,
            greenwich_hour_angle_deg(&got, row.lon),
            row.gha_deg,
        );
        assert!(
            sep <= ALMANAC_TOL_DEG,
            "{}: sun at ({:.6}° alt, {:.6}° az) against the almanac's \
             ({:.6}°, {:.6}°) — {sep:.6}° apart on the sky, over the {ALMANAC_TOL_DEG}° pin",
            row.site,
            got.elevation_deg,
            got.azimuth_deg,
            row.altitude_deg,
            row.azimuth_deg,
        );

        worst = (worst.0.max(d_dec), worst.1.max(d_gha), worst.2.max(sep));
    }
    println!(
        "worst residual over {} rows: declination {:.6}°, GHA {:.6}°, separation {:.6}°",
        ALMANAC.len(),
        worst.0,
        worst.1,
        worst.2,
    );

    // The table reaches the envelope. Without this, rows could be quietly
    // replaced by comfortable ones and the pin would go back to being a
    // statement about the sample rather than about the domain.
    assert!(
        worst.1 > 0.010 && worst.2 > 0.010,
        "no row in the table comes within a factor of two of the {ALMANAC_TOL_DEG}° pin \
         (worst GHA {:.6}°, worst separation {:.6}°) — the table has drifted back to a \
         comfortable sample and no longer exercises the tolerance it asserts",
        worst.1,
        worst.2,
    );
}

/// The non-triviality floor under the test above: a clock ten seconds stale has
/// to break **every** row.
#[test]
fn a_ten_second_stale_clock_breaks_every_almanac_row() {
    for row in &ALMANAC {
        let (y, mo, d, h, mi) = row.utc;
        let late = position_at(
            row.lat,
            row.lon,
            unix_utc(y, mo, d, h, mi) + STALE_CLOCK_SECONDS,
        );
        let d_gha = angle_delta(greenwich_hour_angle_deg(&late, row.lon), row.gha_deg).abs();
        let sep = sky_separation_deg(
            late.elevation_deg,
            late.azimuth_deg,
            row.altitude_deg,
            row.azimuth_deg,
        );
        assert!(
            d_gha > ALMANAC_TOL_DEG && sep > ALMANAC_TOL_DEG,
            "{}: a {STALE_CLOCK_SECONDS} s stale clock moved the sun only {d_gha:.6}° in \
             hour angle and {sep:.6}° on the sky — the {ALMANAC_TOL_DEG}° pin cannot see it, \
             so the almanac test above proves nothing",
            row.site,
        );
    }
}

/// And the floor over the floor. An anti-vacuity guard that only fires far above
/// the pin lets the pin be loosened silently; the previous 60 s spelling allowed
/// `ALMANAC_TOL_DEG` to go from 0.01 to 0.22 — 22× — with everything still green.
/// This asserts the headroom is small enough to bind.
#[test]
fn the_stale_clock_floor_sits_close_above_the_pin() {
    let mut smallest = f64::INFINITY;
    for row in &ALMANAC {
        let (y, mo, d, h, mi) = row.utc;
        let late = position_at(
            row.lat,
            row.lon,
            unix_utc(y, mo, d, h, mi) + STALE_CLOCK_SECONDS,
        );
        smallest = smallest.min(sky_separation_deg(
            late.elevation_deg,
            late.azimuth_deg,
            row.altitude_deg,
            row.azimuth_deg,
        ));
    }
    let headroom = smallest / ALMANAC_TOL_DEG;
    println!(
        "stale-clock floor is {headroom:.2}x the pin ({smallest:.6}° against {ALMANAC_TOL_DEG}°)"
    );
    assert!(
        headroom > 1.0,
        "the stale-clock guard fires below the pin ({headroom:.2}x): it would redden on a \
         correct implementation"
    );
    assert!(
        headroom < 4.0,
        "the stale-clock guard sits {headroom:.2}x above the pin, so the pin could be \
         loosened that far before anything noticed. Shorten STALE_CLOCK_SECONDS."
    );
}

/// The almanac table cannot be quietly narrowed to a comfortable subset. The
/// workspace rule is to arbitrate across four or five *diverse* sites plus a
/// holdout, and this is that rule as a test.
///
/// **What the era span does and does not buy.** An earlier version of this file
/// claimed a 26-year span meant "a scale error that is invisible across one
/// season shows here". It did not, and the 150-year span below does not either.
/// Measured by zeroing each secular term in turn and taking the worst sky
/// displacement over 1900–2100 at three sites: the mean-longitude `T²` term is
/// worth 0.000123°, the mean-anomaly `T²` term 0.000005°, the equation-of-centre
/// secular terms 0.001893°, the eccentricity secular terms 0.004469°, and the
/// obliquity secular drift 0.012999°. **Every one is under the
/// {`ALMANAC_TOL_DEG`}° pin**, so no almanac row at any date can pin them — they
/// are smaller than the theory's own truncation error. That is acceptable
/// precisely because it is the same statement as the pin: an error this module
/// cannot detect is also an error the picture cannot show. What the span *does*
/// pin is a gross scale error — a century length of 36524 instead of 36525 is
/// caught — and that is the honest claim.
#[test]
fn the_almanac_table_is_diverse_enough_to_arbitrate() {
    let lats: Vec<f64> = ALMANAC.iter().map(|r| r.lat).collect();
    let lons: Vec<f64> = ALMANAC.iter().map(|r| r.lon).collect();
    let alts: Vec<f64> = ALMANAC.iter().map(|r| r.altitude_deg).collect();
    let decs: Vec<f64> = ALMANAC.iter().map(|r| r.dec_deg).collect();
    let years: Vec<i64> = ALMANAC.iter().map(|r| r.utc.0).collect();

    assert!(
        ALMANAC.len() >= 12,
        "{} rows is not a spread",
        ALMANAC.len()
    );
    assert!(
        lats.iter().any(|&l| l > 60.0) && lats.iter().any(|&l| l < -60.0),
        "no high-latitude pair: {lats:?}"
    );
    assert!(
        lats.iter().any(|&l| l.abs() < 2.0),
        "nothing on the equator, where declination changes sign: {lats:?}"
    );
    assert!(
        lons.iter().any(|&l| l > 100.0) && lons.iter().any(|&l| l < -100.0),
        "longitudes do not straddle the globe: {lons:?}"
    );
    assert!(
        decs.iter().any(|&d| d > 23.0) && decs.iter().any(|&d| d < -23.0),
        "neither solstice is represented: {decs:?}"
    );
    assert!(
        decs.iter().any(|&d| d.abs() < 0.5),
        "no equinox is represented: {decs:?}"
    );
    let (lo, hi) = alts
        .iter()
        .fold((90.0_f64, -90.0_f64), |(lo, hi), &a| (lo.min(a), hi.max(a)));
    assert!(
        hi - lo > 70.0,
        "the sun is never both near the horizon and near the zenith: {lo}°..{hi}°"
    );
    assert!(
        years.iter().max().unwrap() - years.iter().min().unwrap() >= 100,
        "the era span is under a century, so not even a gross century-length error \
         is levered: {years:?}"
    );
}

/// The below-horizon branch, which `celnav` cannot pin because it drops the sun
/// from its output once the sun has set. Sunrise, sunset and civil twilight are
/// *defined* at fixed geometric centre altitudes, so a published time is a
/// published elevation.
///
/// **The tolerance is derived, not chosen.** USNO *rounds* these times to the
/// nearest minute, so the true crossing is up to **half** a minute either side
/// of the printed one — an earlier version of this file said "a minute" and then
/// allowed a whole one, which was 2× its own justification. The elevation error
/// half a minute permits is half the site's own rate of change of elevation,
/// which runs 0.250°/min at the equator and 0.104°/min at Reykjavík in October.
/// So the bound is measured per row as half a minute of that rate plus twice
/// [`ALMANAC_TOL_DEG`] for the arithmetic itself.
///
/// **These rows pin the branch; they cannot arbitrate accuracy, and the worst
/// residual here must not be read as an accuracy figure.** Every residual is
/// inside the print-rounding interval, so the signal is USNO's printing
/// precision and not this module's arithmetic — the assertion at the end of the
/// test states that rather than leaving it to prose.
#[test]
fn sunrise_sunset_and_civil_twilight_land_where_the_almanac_puts_them() {
    let mut worst = 0.0_f64;
    for row in &HORIZON {
        let (y, mo, d, h, mi) = row.utc;
        let at = unix_utc(y, mo, d, h, mi);
        let elevation = position_at(row.lat, row.lon, at).elevation_deg;

        // Degrees of elevation the sun covers in a minute, here and now.
        let rate_per_min = (position_at(row.lat, row.lon, at + 30.0).elevation_deg
            - position_at(row.lat, row.lon, at - 30.0).elevation_deg)
            .abs();
        let tolerance = 0.5 * rate_per_min + 2.0 * ALMANAC_TOL_DEG;
        let err = (elevation - row.target_deg).abs();

        assert!(
            err <= tolerance,
            "{} {} at {h:02}:{mi:02} UTC: the sun is at {elevation:.4}°, and the \
             phenomenon is defined at {:.4}° — {err:.4}° out against a {tolerance:.4}° \
             bound built from this site's own {rate_per_min:.4}°/min",
            row.site,
            row.phenomenon,
            row.target_deg,
        );
        worst = worst.max(err);
    }
    println!(
        "worst rise/set/twilight residual over {} rows: {worst:.4}° \
         (print-rounding dominated; NOT an accuracy figure)",
        HORIZON.len()
    );
    assert!(
        worst > ALMANAC_TOL_DEG,
        "the worst residual here is {worst:.4}°, under the {ALMANAC_TOL_DEG}° arithmetic \
         envelope. If that is ever true, these rows have become an accuracy check and the \
         comment saying they are not needs revisiting."
    );
}

/// The polar cases as *properties* rather than instants: above the Arctic
/// Circle at the June solstice the sun never sets, and at the December solstice
/// it never rises. A sign error in the hour angle or the declination that
/// survived a noon-only check does not survive this.
#[test]
fn the_midnight_sun_and_the_polar_night_both_hold_at_tromso() {
    let (lat, lon) = (69.6496, 18.9560);
    for minute in 0..(24 * 60) {
        let offset = f64::from(minute) * 60.0;
        let summer = position_at(lat, lon, unix_utc(2026, 6, 21, 0, 0) + offset);
        assert!(
            summer.elevation_deg > 0.0,
            "Tromsø, June solstice, minute {minute}: the sun set, at {:.4}°",
            summer.elevation_deg
        );
        let winter = position_at(lat, lon, unix_utc(2026, 12, 21, 0, 0) + offset);
        assert!(
            winter.elevation_deg < 0.0,
            "Tromsø, December solstice, minute {minute}: the sun rose, to {:.4}°",
            winter.elevation_deg
        );
    }
}

/// Every input that is answered is answered completely, and every input that
/// cannot be is refused outright.
///
/// An earlier version of this test swept a finite grid and concluded "nothing
/// this function can be handed produces a `NaN`". That was false: `1e100`
/// seconds overflowed the Julian-century polynomials and returned a `NaN`
/// elevation, azimuth and hour angle beside a *finite* declination — and
/// `sun_light` then handed a consumer `direction_enu = [NaN, NaN, NaN]` next to
/// an ordinary-looking night colour. A latitude of 1000° silently returned
/// −19.07°. Both are now `None`.
#[test]
fn every_answer_is_whole_and_every_refusal_is_outright() {
    let t = unix_utc(2026, 6, 21, 12, 0);

    // Refused: not a number, not a place, not a time this theory covers.
    for (lat, lon, secs, why) in [
        (f64::NAN, 0.0, t, "NaN latitude"),
        (0.0, f64::NAN, t, "NaN longitude"),
        (0.0, 0.0, f64::NAN, "NaN instant"),
        (f64::INFINITY, 0.0, t, "infinite latitude"),
        (0.0, f64::NEG_INFINITY, t, "infinite longitude"),
        (0.0, 0.0, f64::INFINITY, "infinite instant"),
        (0.0, 0.0, f64::NEG_INFINITY, "infinite past"),
        (90.001, 0.0, t, "latitude past the north pole"),
        (-1000.0, 0.0, t, "latitude of −1000°"),
        (0.0, 0.0, 1.0e100, "an instant 3e90 centuries away"),
        (0.0, 0.0, -1.0e18, "an instant long before the theory"),
    ] {
        assert!(
            solar_position(lat, lon, secs).is_none(),
            "{why} was answered instead of refused"
        );
        assert!(
            sun_light(lat, lon, secs).is_none(),
            "{why}: sun_light answered"
        );
    }

    // Accepted, and answered whole: the poles exactly, unwrapped longitudes,
    // and the far corners of the supported window.
    let window = SUPPORTED_JULIAN_CENTURIES * DAYS_PER_JULIAN_CENTURY * 86_400.0;
    let j2000 = unix_utc(2000, 1, 1, 12, 0);
    for lat_step in -18..=18 {
        let lat = f64::from(lat_step) * 5.0;
        for lon in [-540.0, -180.0, -45.0, 0.0, 45.0, 180.0, 540.0] {
            for at in [
                j2000 - window * 0.999,
                j2000 - 3.0e9,
                t,
                j2000 + 3.0e9,
                j2000 + window * 0.999,
            ] {
                let p = solar_position(lat, lon, at)
                    .unwrap_or_else(|| panic!("refused lat {lat}, lon {lon}, at {at}"));
                assert!(
                    p.elevation_deg.is_finite()
                        && p.azimuth_deg.is_finite()
                        && p.declination_deg.is_finite()
                        && p.hour_angle_deg.is_finite(),
                    "non-finite at lat {lat}, lon {lon}, at {at}: {p:?}"
                );
                assert!((-90.0..=90.0).contains(&p.elevation_deg), "{p:?}");
                assert!((0.0..360.0).contains(&p.azimuth_deg), "{p:?}");
                assert!((-24.5..=24.5).contains(&p.declination_deg), "{p:?}");
                assert!((-180.0..180.0).contains(&p.hour_angle_deg), "{p:?}");

                let light = sun_light(lat, lon, at).expect("sun_light agrees with solar_position");
                assert!(
                    light.direction_enu.iter().all(|c| c.is_finite())
                        && light.colour.iter().all(|c| c.is_finite())
                        && light.ambient.iter().all(|c| c.is_finite()),
                    "sun_light produced a non-finite component: {light:?}"
                );
            }
        }
    }
}

// ── The two ramps ─────────────────────────────────────────────────────────

#[test]
fn the_ramp_knots_ascend() {
    for (name, stops) in [("beam", &BEAM_STOPS[..]), ("sky", &SKY_STOPS[..])] {
        for pair in stops.windows(2) {
            assert!(
                pair[1].0 > pair[0].0,
                "the {name} ramp's knots are scanned in order and {:?} does not follow {:?}",
                pair[1],
                pair[0],
            );
        }
        assert_eq!(stops[0].0, -90.0, "{name} ramp does not start at −90°");
        assert_eq!(
            stops[stops.len() - 1].0,
            90.0,
            "{name} ramp does not end at +90°"
        );
    }
}

/// The ramps, pinned at named elevations. Each is a knot, so the value is the
/// authored one rather than an interpolation — a moved number here is a
/// deliberate change to how the scene looks, and should read as one in review.
#[test]
fn the_ramps_are_pinned_at_named_elevations() {
    let beam: [(f64, [f64; 3], &str); 7] = [
        (90.0, [1.000, 1.000, 1.000], "zenith — neutral"),
        (30.0, [1.000, 0.980, 0.950], "high sun — barely warm"),
        (10.0, [1.000, 0.880, 0.720], "afternoon — warm"),
        (2.0, [1.000, 0.620, 0.360], "low sun — amber"),
        (0.0, [0.950, 0.470, 0.260], "sun on the horizon — sunset"),
        (-0.8333, [0.0, 0.0, 0.0], "sunset proper — the beam is gone"),
        (-18.0, [0.0, 0.0, 0.0], "night — still no beam"),
    ];
    for (elevation, expected, what) in beam {
        assert_eq!(
            sun_tint(elevation),
            expected,
            "the direct beam at {elevation}° ({what}) moved",
        );
    }

    let sky: [(f64, [f64; 3], &str); 7] = [
        (90.0, [0.250, 0.290, 0.400], "zenith — deep blue sky"),
        (10.0, [0.270, 0.280, 0.340], "daylight sky"),
        (0.0, [0.235, 0.185, 0.210], "sunset sky — warm"),
        (-1.0, [0.205, 0.170, 0.195], "afterglow"),
        (-3.0, [0.130, 0.125, 0.185], "afterglow gone violet"),
        (
            -6.0,
            [0.100, 0.120, 0.200],
            "end of civil twilight — cool blue",
        ),
        (
            -18.0,
            [0.035, 0.045, 0.075],
            "astronomical night — the floor",
        ),
    ];
    for (elevation, expected, what) in sky {
        assert_eq!(
            sky_ambient(elevation),
            expected,
            "the sky at {elevation}° ({what}) moved",
        );
    }

    // Midpoints interpolate, so a ramp is a ramp and not a staircase.
    // Approximate, and only here: a knot is an authored constant and compares
    // exactly, but a midpoint is arithmetic and lands an ulp off it.
    for (elevation, expected) in [(1.0, [0.975, 0.545, 0.310]), (20.0, [1.0, 0.930, 0.835])] {
        let got = sun_tint(elevation);
        assert!(
            got.iter().zip(expected).all(|(a, b)| (a - b).abs() < 1e-12),
            "the beam midpoint at {elevation}° is {got:?}, not {expected:?}",
        );
    }
}

/// Light never gets brighter as the sun goes down.
///
/// **This is a proof, not a sample, and the proof is at the knots.** Luminance
/// is a linear functional of the channels and each ramp is linear between
/// knots, so monotone at consecutive knots implies monotone everywhere. The
/// dense sweep afterwards checks the *interpolator*, not the claim — and it is
/// worth noting that a sweep alone would not be a proof: the old 0.1° sweep was
/// only exhaustive because every knot happened to sit on an integer degree, and
/// the −0.8333° knot this file now carries would have silently demoted it to a
/// spot check.
#[test]
fn neither_ramp_brightens_as_the_sun_falls() {
    for (name, stops) in [("beam", &BEAM_STOPS[..]), ("sky", &SKY_STOPS[..])] {
        for pair in stops.windows(2) {
            let (lo, hi) = (luminance(pair[0].1), luminance(pair[1].1));
            assert!(
                hi >= lo,
                "the {name} ramp brightens downward between {}° ({lo:.6}) and {}° ({hi:.6})",
                pair[0].0,
                pair[1].0,
            );
        }
    }

    let ramps: [NamedRamp; 2] = [("beam", sun_tint), ("sky", sky_ambient)];
    for (name, at) in ramps {
        let mut previous = luminance(at(90.0));
        let mut step = 9000;
        while step >= -9000 {
            let elevation = f64::from(step) * 0.01;
            let here = luminance(at(elevation));
            assert!(
                here <= previous + 1e-12,
                "the {name} ramp brightened on the way down: {here:.6} at {elevation}° \
                 against {previous:.6} just above it",
            );
            previous = here;
            step -= 1;
        }
    }

    // And they really do dim, so the assertions above are not satisfied by a
    // constant.
    assert!(luminance(sun_tint(90.0)) > 0.9 && luminance(sun_tint(-6.0)) == 0.0);
    assert!(luminance(sky_ambient(90.0)) > 6.0 * luminance(sky_ambient(-18.0)));
}

/// Warm toward the horizon, cool below it. The physical claim is that the direct
/// beam reddens with path length and that what is left after sunset is scattered
/// short-wavelength light, so the red/blue ordering in the **sky** ramp has to
/// invert somewhere just under the horizon — and it does, near −1.3°.
#[test]
fn the_light_warms_toward_the_horizon_and_the_sky_cools_below_it() {
    for elevation in [30.0, 10.0, 2.0, 0.0] {
        let [r, g, b] = sun_tint(elevation);
        assert!(
            r >= g && g >= b,
            "the beam at {elevation}° is not warm: {r:.3}/{g:.3}/{b:.3}"
        );
    }
    for elevation in [0.0, -1.0] {
        let [r, _, b] = sky_ambient(elevation);
        assert!(
            r > b,
            "the sky at {elevation}° is not warm: {r:.3} vs {b:.3}"
        );
    }
    for elevation in [-3.0, -6.0, -12.0, -18.0, -40.0] {
        let [r, _, b] = sky_ambient(elevation);
        assert!(
            b > r,
            "the sky at {elevation}° is not cool: red {r:.3} against blue {b:.3}"
        );
    }

    // The warm-to-cool crossover is below the horizon, not above it: an amber
    // sunset sky must not have turned blue while the sun is still visible. The
    // bracket is tight on purpose — the knots at −3° and −1° already guarantee
    // a crossover somewhere in (−3, −1), so a window of (−3, −1) would assert
    // nothing that the table does not already force.
    let crossover = (-600..=0)
        .map(|s| f64::from(s) * 0.01)
        .find(|&e| {
            let [r, _, b] = sky_ambient(e);
            r >= b
        })
        .expect("red overtakes blue somewhere between −6° and the horizon");
    assert!(
        (-1.5..=-1.1).contains(&crossover),
        "the warm/cool crossover drifted to {crossover}°"
    );
}

/// The night floor is dim, not black. Terrain has to read by silhouette at
/// 2 a.m.; a scene that renders as nothing is the defect, not the caption.
#[test]
fn the_night_floor_is_dim_but_never_black() {
    for elevation in [-18.0, -30.0, -60.0, -90.0, -1000.0] {
        let rgb = sky_ambient(elevation);
        assert_eq!(
            rgb, SKY_STOPS[0].1,
            "the floor is not flat below −18°: {rgb:?} at {elevation}°"
        );
        assert!(
            rgb.iter().all(|&c| c >= 0.03),
            "the floor went black at {elevation}°: {rgb:?}"
        );
    }
    // Dim, though — not a flat grey that erases the difference between noon and
    // midnight.
    assert!(luminance(SKY_STOPS[0].1) < 0.08);
}

#[test]
fn both_ramps_clamp_outside_their_range_and_survive_a_nan() {
    assert_eq!(sun_tint(91.0), [1.0, 1.0, 1.0]);
    assert_eq!(sun_tint(1.0e300), [1.0, 1.0, 1.0]);
    assert_eq!(sun_tint(-91.0), BEAM_STOPS[0].1);
    assert_eq!(sky_ambient(91.0), SKY_STOPS[SKY_STOPS.len() - 1].1);
    assert_eq!(sky_ambient(-91.0), SKY_STOPS[0].1);
    // A `NaN` fails every comparison, finds no segment, and falls out at the
    // darkest knot — the safe way to be wrong about how bright a scene is.
    assert_eq!(sun_tint(f64::NAN), BEAM_STOPS[0].1);
    assert_eq!(sky_ambient(f64::NAN), SKY_STOPS[0].1);
}

// ── Direction and colour, from one call ───────────────────────────────────

/// The reason [`sun_light`] exists: a caller cannot take the direction from one
/// place and the colour from another, because there is only one place.
#[test]
fn the_light_carries_direction_and_colour_from_the_same_instant() {
    for row in &ALMANAC {
        let (y, mo, d, h, mi) = row.utc;
        let at = unix_utc(y, mo, d, h, mi);
        let light = sun_light(row.lat, row.lon, at).expect("an almanac row is inside the window");

        assert_eq!(Some(light.position), solar_position(row.lat, row.lon, at));
        assert_eq!(light.colour, sun_tint(light.position.elevation_deg));
        assert_eq!(light.ambient, sky_ambient(light.position.elevation_deg));

        let [e, n, u] = light.direction_enu;
        let length = (e * e + n * n + u * u).sqrt();
        assert!(
            (length - 1.0).abs() < 1e-12,
            "{}: the direction is {length} long, not 1",
            row.site
        );
        // The vector really is the position, spelled as a vector: recovering
        // the angles from it returns them.
        let recovered_elevation = u.asin().to_degrees();
        let recovered_azimuth = e.atan2(n).to_degrees().rem_euclid(360.0);
        assert!(
            (recovered_elevation - light.position.elevation_deg).abs() < 1e-9,
            "{}: elevation does not survive the round trip",
            row.site
        );
        assert!(
            angle_delta(recovered_azimuth, light.position.azimuth_deg).abs() < 1e-9,
            "{}: azimuth does not survive the round trip",
            row.site
        );
        // Up is the sign of the elevation, at night as much as by day.
        assert_eq!(
            u > 0.0,
            light.position.elevation_deg > 0.0,
            "{}: the up component disagrees with the elevation's sign",
            row.site
        );
    }
}

/// The direction points where the almanac says the sun is, in the frame the
/// consumer will use it in — east/north/up.
#[test]
fn the_direction_points_at_the_sun_in_the_east_north_up_frame() {
    // Denver at the June solstice, two minutes before transit: high, and a
    // degree and a half *east* of due south. The almanac row's own azimuth is
    // 178.53°, which is east of 180 — an earlier version of this comment said
    // west, and the assertion beside it took an absolute value and so could not
    // catch the error. It is signed now.
    let light = sun_light(39.74, -104.98, unix_utc(2026, 6, 21, 19, 0)).expect("in window");
    let [east, north, up] = light.direction_enu;
    assert!(up > 0.95, "the solstice sun over Denver is high: up = {up}");
    assert!(
        north < 0.0,
        "the sun is south of Denver at local noon: north = {north}"
    );
    assert!(
        east > 0.0 && east < 0.02,
        "the sun should be a hair EAST of the meridian two minutes before \
         transit: east = {east}"
    );

    // Tromsø's midnight sun is *north*, low, and the beam is amber — the case
    // that separates a real sun from a headlight.
    let midnight = sun_light(69.6496, 18.9560, unix_utc(2026, 6, 21, 0, 0)).expect("in window");
    assert!(midnight.direction_enu[1] > 0.9, "{midnight:?}");
    assert!(midnight.direction_enu[2] > 0.0, "{midnight:?}");
    let [r, _, b] = midnight.colour;
    assert!(
        r > b,
        "a 4° sun should be warm, not blue: {:?}",
        midnight.colour
    );
}

/// The night this module designs has to reach a pixel, and with the directional
/// term alone it does not.
///
/// This is the test the ambient channel exists for. `colour` is the direct beam
/// and is identically zero once the sun has set, so a consumer computing only
/// `albedo · colour · max(0, N·L)` renders **pure black at night, on every
/// surface, at every orientation** — and the five below-horizon knots that exist
/// so ground reads by silhouette at 2 a.m. would never be seen.
#[test]
fn the_night_reaches_a_pixel_and_the_beam_alone_would_not() {
    // KTLX at half past midnight local, at the December solstice.
    let light = sun_light(35.3331, -97.2778, unix_utc(2026, 12, 22, 6, 30)).expect("in window");
    assert!(
        light.position.elevation_deg < -18.0,
        "expected astronomical night, got {:.3}°",
        light.position.elevation_deg
    );

    // Eight surface orientations: up, the four compass slopes, and three tilts.
    let normals = [
        [0.0, 0.0, 1.0],
        [1.0, 0.0, 0.0],
        [-1.0, 0.0, 0.0],
        [0.0, 1.0, 0.0],
        [0.0, -1.0, 0.0],
        [0.6, 0.0, 0.8],
        [0.0, 0.6, 0.8],
        [-0.6, -0.6, 0.529],
    ];
    let dot = |a: [f64; 3], b: [f64; 3]| a[0] * b[0] + a[1] * b[1] + a[2] * b[2];

    for normal in normals {
        let cosine = dot(normal, light.direction_enu).max(0.0);
        // The directional half really is nothing at all, on every orientation.
        let beam: f64 = light.colour.iter().sum::<f64>() * cosine;
        assert_eq!(
            beam, 0.0,
            "the beam is not zero at night for normal {normal:?} — this test's premise is gone"
        );
        // And the whole light is not.
        let lit = [
            light.colour[0] * cosine + light.ambient[0],
            light.colour[1] * cosine + light.ambient[1],
            light.colour[2] * cosine + light.ambient[2],
        ];
        assert!(
            luminance(lit) > 0.04,
            "night renders as black for normal {normal:?}: {lit:?}"
        );
        assert!(
            lit[2] > lit[0],
            "night should read cool for normal {normal:?}: {lit:?}"
        );
    }

    // At sunset the warm sky reaches ground facing away from the sun, which the
    // beam alone would leave neutral-dark.
    let dusk = sun_light(35.3331, -97.2778, unix_utc(2026, 12, 21, 23, 20)).expect("in window");
    assert!(dusk.position.elevation_deg < 0.0 && dusk.position.elevation_deg > -1.5);
    let [r, _, b] = dusk.ambient;
    assert!(r > b, "the sunset sky should be warm: {:?}", dusk.ambient);
}

/// `colour + ambient` overshoots 1.0 on a surface facing a high sun, by design,
/// and the figure is pinned so it cannot drift into a consumer unnoticed.
/// Exposure belongs to the consumer; what belongs here is stating the number.
#[test]
fn the_lit_sum_peaks_where_it_is_documented_to() {
    let peak = luminance([
        sun_tint(90.0)[0] + sky_ambient(90.0)[0],
        sun_tint(90.0)[1] + sky_ambient(90.0)[1],
        sun_tint(90.0)[2] + sky_ambient(90.0)[2],
    ]);
    assert!(
        (peak - 1.2894).abs() < 0.0005,
        "the documented 1.29 peak of colour + ambient moved to {peak:.4}"
    );

    // The sky is a real fraction of the beam at noon, not a rounding error and
    // not a second sun.
    let share = luminance(sky_ambient(90.0)) / luminance(sun_tint(90.0));
    assert!(
        (0.15..0.40).contains(&share),
        "the sky is {share:.3} of the beam at the zenith, which is not a sky"
    );
}
