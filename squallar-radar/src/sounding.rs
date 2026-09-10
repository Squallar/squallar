//! Environmental sounding heights per radar site: where the 0 °C and −20 °C
//! surfaces sit, from Open-Meteo's forecast API. The hail products need both,
//! and the hybrid hydrometeor classification stands them in for its wet-bulb
//! operator values ([`crate::hca::HsdaHeights::from_env_heights`]).
//! [`crate::types::RadarProduct::reads_env_heights`] is the whole set.
//!
//! Both heights are **km above mean sea level**, not above the radar. The
//! 0 °C height is Open-Meteo's `freezing_level_height` taken as-is. The −20 °C
//! height is interpolated here from the temperature/geopotential-height pairs
//! at 600/500/400/300 hPa — a span whose endpoints average ~−13 °C and
//! ~−45 °C; the out-of-span arms in [`height_at_minus20_m`] cover the rest.
//!
//! Fetching and parsing are split so the parser is testable offline:
//! [`parse_env_heights`] is pure and runs against
//! `testdata/openmeteo_koax.json`, captured on 2026-07-28 (KOAX: 41.320,
//! −96.367).
//!
//! **Calibrated against nothing.** Nobody publishes a "−20 °C height for a
//! radar site" that this could be differenced against, so the fixed level set,
//! the claim that this span brackets −20 °C, and both out-of-span arms are
//! assertions rather than measurements. These two heights feed POSH, MEHS,
//! every HSDA size class and every HCA and HHC class, so a 500 m error moves
//! all of them at once and leaves every one looking plausible.

use chrono::{DateTime, TimeZone, Timelike, Utc};
use serde::Deserialize;

use crate::sources::DataSources;

/// How long a fetched [`EnvHeights`] stays fresh.
pub const ENV_HEIGHTS_TTL: std::time::Duration = std::time::Duration::from_secs(60 * 60);

/// The response is under a kilobyte; this is connection-setup allowance for a
/// bad link, not transfer time.
const SOUNDING_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// Environmental freezing-level heights over one point **for one hour**.
///
/// `valid_at` is the hour the two heights describe and `fetched_at` is when the
/// request that produced them completed. They are the same hour for a live
/// sounding and far apart for a historical one, and [`Self::is_stale`] is the
/// difference: a sounding for a past hour is already final, so no elapsed
/// wall-clock time can make it wrong.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EnvHeights {
    /// Height of the 0 °C surface, km above mean sea level.
    pub h0c_km_msl: f64,
    /// Height of the −20 °C surface, km above mean sea level.
    pub hm20c_km_msl: f64,
    /// The UTC hour these heights describe.
    pub valid_at: DateTime<Utc>,
    /// When the fetch completed (UTC).
    pub fetched_at: DateTime<Utc>,
}

/// The hour a sounding sample describes, as a UTC instant.
///
/// **The bucket the cache keys on.** Open-Meteo answers in whole hours, so this
/// is the finest instant the source can distinguish — two requests inside one
/// hour read the same model row and return byte-identical values. Measured at
/// KOAX over 72 consecutive rows, `|Δh0|` across one hour is 50 m at the median
/// and 250 m at the worst, against the 500 m this module's header names as the
/// error that moves every hail and HCA class at once; across six hours it is
/// 150 m and 450 m, which is why one pair cannot serve a whole loop.
pub fn hour_bucket(t: DateTime<Utc>) -> DateTime<Utc> {
    t.with_minute(0)
        .and_then(|t| t.with_second(0))
        .and_then(|t| t.with_nanosecond(0))
        .unwrap_or(t)
}

fn ttl() -> chrono::Duration {
    chrono::Duration::from_std(ENV_HEIGHTS_TTL).expect("ENV_HEIGHTS_TTL fits in a chrono::Duration")
}

impl EnvHeights {
    /// Whether this sample describes an hour already closed when it was fetched.
    ///
    /// Such a sample is a reading of the past: the model row behind it will not
    /// be revised by waiting, so it never expires.
    pub fn is_historical(&self) -> bool {
        self.fetched_at.signed_duration_since(self.valid_at) >= ttl()
    }

    /// Whether this value has outlived [`ENV_HEIGHTS_TTL`].
    ///
    /// Only a sounding for the live hour can: see [`Self::is_historical`].
    pub fn is_stale(&self, now: DateTime<Utc>) -> bool {
        !self.is_historical() && now.signed_duration_since(self.fetched_at) >= ttl()
    }
}

/// Fetch the 0 °C and −20 °C heights above `(lat, lon)` **for the hour `as_of`
/// falls in**.
///
/// `as_of` is the instant the pane is showing, not the wall clock: a pane
/// scrubbed to a past hour and a loop frame from a past hour both resolve the
/// sounding that was over the site then. Pass `Utc::now()` for a live pane.
///
/// `None` when that hour cannot be answered. Open-Meteo keeps the pressure
/// levels the −20 °C height is interpolated from for about three weeks
/// (measured 2026-09-10: present 22 days back, null at 24), so an instant older
/// than that returns nothing rather than a wrong answer.
pub async fn fetch_env_heights(
    sources: &DataSources,
    lat: f64,
    lon: f64,
    as_of: DateTime<Utc>,
) -> Option<EnvHeights> {
    let mut hours = fetch_env_heights_range(sources, lat, lon, as_of, as_of).await;
    hours.sort_by_key(|h| {
        h.valid_at
            .signed_duration_since(as_of)
            .num_seconds()
            .unsigned_abs()
    });
    hours.into_iter().next()
}

/// Fetch **every hourly sounding from `from` to `to`** above `(lat, lon)`.
///
/// One request: Open-Meteo returns the whole range as hourly rows, so a loop's
/// entire span of soundings costs one round trip rather than one per frame.
/// Hours the model cannot answer are absent from the result rather than
/// substituted — see [`parse_env_heights_series`].
pub async fn fetch_env_heights_range(
    sources: &DataSources,
    lat: f64,
    lon: f64,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
) -> Vec<EnvHeights> {
    crate::tls::init();
    let url = sources.sounding_url(lat, lon, from, to);
    let Ok(client) = sources.sounding_client(SOUNDING_TIMEOUT) else {
        return Vec::new();
    };
    let Ok(response) = client.get(&url).send().await else {
        return Vec::new();
    };
    if !response.status().is_success() {
        log::warn!("Sounding fetch: HTTP {} from {url}", response.status());
        return Vec::new();
    }
    let Ok(body) = squallar_source::http::body_to_string(response).await else {
        return Vec::new();
    };
    let fetched_at = Utc::now();
    parse_env_heights_series(&body)
        .into_iter()
        .map(|(valid_at, h0c_km_msl, hm20c_km_msl)| EnvHeights {
            h0c_km_msl,
            hm20c_km_msl,
            valid_at,
            fetched_at,
        })
        .collect()
}

/// **The most hourly soundings one site keeps.**
///
/// The lookback slider tops out at 1440 minutes (`squallar_egui::ui_timeline`),
/// so the widest loop a pane can ask for spans 24 hours and touches 25 hourly
/// buckets counting both ends. The rest is slack for a pane parked outside its
/// own loop's span and for a second pane on the same site at another instant.
///
/// **This is what stops a scrubber drag being a leak.** Dragging mints instants
/// without limit, but they land in hour buckets, and past this cap the sample
/// furthest in time from the newest one is dropped.
pub const MAX_HOURS_PER_SITE: usize = 32;

/// The hourly soundings one site has in hand, bounded by [`MAX_HOURS_PER_SITE`].
///
/// A sample answers for **its own hour only**. Substituting a neighbouring hour
/// is the defect this type exists to remove: an instant answered silently with
/// another instant's data is exactly what a scrubbed pane showing today's
/// freezing level was doing.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct EnvHeightsStore {
    by_hour: std::collections::BTreeMap<DateTime<Utc>, EnvHeights>,
}

impl EnvHeightsStore {
    /// File a sample under the hour it describes, evicting to the cap.
    ///
    /// Returns whether the pair it holds for that hour actually moved — the
    /// caller drops renders on that answer, and a refetch that lands the same
    /// two numbers must not invalidate anything.
    pub fn insert(&mut self, heights: EnvHeights) -> bool {
        let hour = hour_bucket(heights.valid_at);
        let moved = self.by_hour.get(&hour).is_none_or(|old| {
            old.h0c_km_msl != heights.h0c_km_msl || old.hm20c_km_msl != heights.hm20c_km_msl
        });
        self.by_hour.insert(hour, heights);
        while self.by_hour.len() > MAX_HOURS_PER_SITE {
            // Furthest in time from the newest arrival: a loop walking forward
            // sheds the hours behind it, and one walking back sheds the hours
            // ahead. Both ends are reachable, so neither `first` nor `last` is
            // the right one to drop.
            let Some(&newest) = self.by_hour.keys().next_back() else {
                break;
            };
            let Some(&furthest) = self
                .by_hour
                .keys()
                .max_by_key(|k| k.signed_duration_since(newest).num_seconds().abs())
            else {
                break;
            };
            self.by_hour.remove(&furthest);
        }
        moved
    }

    /// The `(0 °C, −20 °C)` pair for the hour `when` falls in, or `None`.
    ///
    /// `None` is a real answer: the caller renders nothing rather than
    /// classifying against some other hour's atmosphere.
    pub fn at(&self, when: DateTime<Utc>) -> Option<(f64, f64)> {
        self.by_hour
            .get(&hour_bucket(when))
            .map(|h| (h.h0c_km_msl, h.hm20c_km_msl))
    }

    /// Whether every hour in `from ..= to` is already held, so no fetch is owed.
    pub fn covers(&self, from: DateTime<Utc>, to: DateTime<Utc>) -> bool {
        let mut hour = hour_bucket(from);
        let last = hour_bucket(to);
        while hour <= last {
            if !self.by_hour.contains_key(&hour) {
                return false;
            }
            hour += chrono::Duration::hours(1);
        }
        true
    }

    /// Drop every sample that has outlived the TTL for the live hour.
    pub fn drop_stale(&mut self, now: DateTime<Utc>) {
        self.by_hour.retain(|_, h| !h.is_stale(now));
    }

    /// How many hourly samples are held. **The quantity the cap bounds.**
    pub fn len(&self) -> usize {
        self.by_hour.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_hour.is_empty()
    }

    /// The hours held, oldest first.
    pub fn hours(&self) -> impl Iterator<Item = DateTime<Utc>> + '_ {
        self.by_hour.keys().copied()
    }
}

/// The slice of an Open-Meteo `/v1/forecast` response this module reads.
#[derive(Deserialize)]
struct SoundingResponse {
    hourly: Hourly,
}

/// Parallel hourly arrays. `Option<f64>` per element because Open-Meteo emits
/// JSON `null` where a model row is missing, and a null hour must not take the
/// whole response down — [`parse_env_heights`] just moves to the next hour.
#[derive(Deserialize)]
struct Hourly {
    /// Row timestamps, `%Y-%m-%dT%H:%M` and UTC because the query names no
    /// timezone. **The only thing that says which hour a row describes**, and
    /// the reason a row is chosen by its own time rather than by its position.
    time: Vec<String>,
    freezing_level_height: Vec<Option<f64>>,
    #[serde(rename = "temperature_600hPa")]
    t_600: Vec<Option<f64>>,
    #[serde(rename = "geopotential_height_600hPa")]
    z_600: Vec<Option<f64>>,
    #[serde(rename = "temperature_500hPa")]
    t_500: Vec<Option<f64>>,
    #[serde(rename = "geopotential_height_500hPa")]
    z_500: Vec<Option<f64>>,
    #[serde(rename = "temperature_400hPa")]
    t_400: Vec<Option<f64>>,
    #[serde(rename = "geopotential_height_400hPa")]
    z_400: Vec<Option<f64>>,
    #[serde(rename = "temperature_300hPa")]
    t_300: Vec<Option<f64>>,
    #[serde(rename = "geopotential_height_300hPa")]
    z_300: Vec<Option<f64>>,
}

impl Hourly {
    /// Everything hour `i` needs, or `None` if any piece of it is null or the
    /// arrays are shorter than `i`: the freezing-level height in meters, and
    /// the four `(height m, temperature °C)` levels ordered bottom-up
    /// (600 → 300 hPa).
    fn row(&self, i: usize) -> Option<(f64, [(f64, f64); 4])> {
        let get = |v: &Vec<Option<f64>>| v.get(i).copied().flatten();
        Some((
            get(&self.freezing_level_height)?,
            [
                (get(&self.z_600)?, get(&self.t_600)?),
                (get(&self.z_500)?, get(&self.t_500)?),
                (get(&self.z_400)?, get(&self.t_400)?),
                (get(&self.z_300)?, get(&self.t_300)?),
            ],
        ))
    }
}

/// Every complete hour in an Open-Meteo response, as
/// `(valid_at, h0c_km_msl, hm20c_km_msl)`, in the order the response lists them.
///
/// Incomplete hours are dropped rather than taking the response down: Open-Meteo
/// emits JSON `null` where a model row is missing, and beyond the pressure
/// levels' retention window every row is null while `freezing_level_height`
/// still reads — which is one height of the two and not enough to render with.
pub fn parse_env_heights_series(json: &str) -> Vec<(DateTime<Utc>, f64, f64)> {
    let Ok(response) = serde_json::from_str::<SoundingResponse>(json) else {
        return Vec::new();
    };
    let hourly = &response.hourly;
    (0..hourly.time.len())
        .filter_map(|i| {
            let valid_at = parse_hour(hourly.time.get(i)?)?;
            let (freezing_m, levels) = hourly.row(i)?;
            if !freezing_m.is_finite() {
                return None;
            }
            let hm20_m = height_at_minus20_m(&levels)?;
            Some((valid_at, freezing_m / 1000.0, hm20_m / 1000.0))
        })
        .collect()
}

/// Open-Meteo's `%Y-%m-%dT%H:%M` row stamp, read as UTC.
///
/// The query names no timezone, so the response is GMT and these are UTC
/// instants. Seconds are absent from the format.
fn parse_hour(raw: &str) -> Option<DateTime<Utc>> {
    let naive = chrono::NaiveDateTime::parse_from_str(raw, "%Y-%m-%dT%H:%M").ok()?;
    Utc.from_utc_datetime(&naive).into()
}

/// The complete hour **nearest `as_of`**, as `(valid_at, h0c, hm20c)`.
///
/// Nearest rather than first: the response is a window around the requested
/// hour, and taking whichever row happened to come back first is how a pane
/// asking for one instant is answered with another. Ties go to the earlier row.
pub fn parse_env_heights_at(json: &str, as_of: DateTime<Utc>) -> Option<(DateTime<Utc>, f64, f64)> {
    parse_env_heights_series(json)
        .into_iter()
        .min_by_key(|(valid_at, _, _)| {
            valid_at
                .signed_duration_since(as_of)
                .num_seconds()
                .unsigned_abs()
        })
}

/// Parse an Open-Meteo response into `(h0c_km_msl, hm20c_km_msl)`, taking the
/// first complete hour it lists.
pub fn parse_env_heights(json: &str) -> Option<(f64, f64)> {
    parse_env_heights_series(json)
        .first()
        .map(|&(_, h0c, hm20c)| (h0c, hm20c))
}

const TARGET_C: f64 = -20.0;

/// Height (m MSL) where the profile crosses −20 °C.
///
/// `levels` is four `(height m, temperature °C)` pairs ordered bottom-up. The
/// interpolation is linear in temperature between the first bracketing pair.
/// Off the ends of the span: colder than −20 °C at 600 hPa extends downward on
/// the 600→500 hPa lapse rate, clamped at sea level; warmer than −20 °C at
/// 300 hPa extends upward on the 400→300 hPa lapse rate. Either extension
/// needs the segment to cool with height; when it does not, the edge level's
/// own height is the answer. Non-finite inputs are rejected outright.
fn height_at_minus20_m(levels: &[(f64, f64); 4]) -> Option<f64> {
    if levels.iter().any(|(z, t)| !z.is_finite() || !t.is_finite()) {
        return None;
    }

    let (z0, t0) = levels[0];
    if t0 <= TARGET_C {
        let (z1, t1) = levels[1];
        if t1 < t0 {
            let extended = z0 + (TARGET_C - t0) * (z1 - z0) / (t1 - t0);
            return Some(extended.max(0.0));
        }
        return Some(z0.max(0.0));
    }

    // In-span: first pair whose top is at or below −20 °C. `ta > TARGET_C >= tb`
    // here, so the denominator is strictly positive.
    for pair in levels.windows(2) {
        let (za, ta) = pair[0];
        let (zb, tb) = pair[1];
        if tb <= TARGET_C {
            return Some(za + (ta - TARGET_C) / (ta - tb) * (zb - za));
        }
    }

    let (z2, t2) = levels[2];
    let (z3, t3) = levels[3];
    if t3 < t2 {
        return Some(z3 + (t3 - TARGET_C) / (t2 - t3) * (z3 - z2));
    }
    Some(z3)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The response `DataSources::sounding_url(41.320, -96.367)` returned on
    /// 2026-07-28.
    const KOAX: &str = include_str!("../testdata/openmeteo_koax.json");

    fn assert_close(actual: f64, expected: f64, what: &str) {
        assert!(
            (actual - expected).abs() < 1e-9,
            "{what}: expected {expected}, got {actual}",
        );
    }

    fn summer_levels() -> [(f64, f64); 4] {
        [
            (4400.0, 4.0),
            (5900.0, -4.0),
            (7600.0, -14.0),
            (9700.0, -30.0),
        ]
    }

    #[test]
    fn the_koax_fixture_parses_to_the_hand_computed_heights() {
        let (h0c, hm20c) = parse_env_heights(KOAX).expect("fixture should parse");

        assert_close(h0c, 5.190, "0C height km");

        let expected = 7632.10 + (5.7 / 15.2) * (9748.39 - 7632.10);
        assert_close(hm20c, expected / 1000.0, "-20C height km");
        assert!(
            (8.3..8.6).contains(&hm20c),
            "-20C height {hm20c} km is outside the plausible band for this profile",
        );
    }

    #[test]
    fn a_crossing_between_two_levels_interpolates_linearly() {
        let h = height_at_minus20_m(&summer_levels()).unwrap();
        assert_close(h, 7600.0 + (6.0 / 16.0) * 2100.0, "-20C height m");
    }

    #[test]
    fn a_crossing_exactly_at_a_level_returns_that_level_height() {
        let mut levels = summer_levels();
        levels[2].1 = -20.0; // 400 hPa exactly −20 °C
        assert_close(
            height_at_minus20_m(&levels).unwrap(),
            levels[2].0,
            "-20C height m",
        );
    }

    #[test]
    fn a_column_still_warm_at_300_hpa_extends_the_top_lapse_rate_upward() {
        let levels = [
            (4400.0, 20.0),
            (5900.0, 10.0),
            (7600.0, -10.0),
            (9700.0, -18.0),
        ];
        let h = height_at_minus20_m(&levels).unwrap();
        assert_close(h, 9700.0 + (2.0 / 8.0) * 2100.0, "-20C height m");
        assert!(h > 9700.0, "extension must be above the 300 hPa level");
    }

    #[test]
    fn a_column_warm_at_300_hpa_with_an_inverted_top_clamps_to_300_hpa() {
        let levels = [
            (4400.0, 20.0),
            (5900.0, 10.0),
            (7600.0, -16.0),
            (9700.0, -15.0),
        ];
        assert_close(
            height_at_minus20_m(&levels).unwrap(),
            9700.0,
            "-20C height m",
        );
    }

    #[test]
    fn an_arctic_column_extends_the_bottom_lapse_rate_downward() {
        let levels = [
            (4100.0, -24.0),
            (5600.0, -30.0),
            (7300.0, -40.0),
            (9100.0, -55.0),
        ];
        let h = height_at_minus20_m(&levels).unwrap();
        assert_close(h, 4100.0 - (4.0 / 6.0) * 1500.0, "-20C height m");
        assert!(h < 4100.0, "extension must be below the 600 hPa level");
    }

    #[test]
    fn the_downward_extension_clamps_at_sea_level() {
        let levels = [
            (4100.0, -60.0),
            (5600.0, -62.0),
            (7300.0, -65.0),
            (9100.0, -70.0),
        ];
        assert_close(height_at_minus20_m(&levels).unwrap(), 0.0, "-20C height m");
    }

    #[test]
    fn exactly_minus_twenty_at_the_bottom_level_is_that_level() {
        let levels = [
            (4100.0, -20.0),
            (5600.0, -28.0),
            (7300.0, -40.0),
            (9100.0, -55.0),
        ];
        assert_close(
            height_at_minus20_m(&levels).unwrap(),
            4100.0,
            "-20C height m",
        );
    }

    #[test]
    fn an_arctic_column_with_an_inversion_above_600_clamps_to_600_hpa() {
        let levels = [
            (4100.0, -22.0),
            (5600.0, -21.0),
            (7300.0, -30.0),
            (9100.0, -50.0),
        ];
        assert_close(
            height_at_minus20_m(&levels).unwrap(),
            4100.0,
            "-20C height m",
        );
    }

    #[test]
    fn non_finite_inputs_are_rejected_not_propagated() {
        let mut levels = summer_levels();
        levels[1].1 = f64::NAN;
        assert_eq!(height_at_minus20_m(&levels), None);
        let mut levels = summer_levels();
        levels[2].0 = f64::INFINITY;
        assert_eq!(height_at_minus20_m(&levels), None);
    }

    #[test]
    fn a_null_first_hour_falls_through_to_the_second() {
        let json = KOAX.replacen("[5190.00,", "[null,", 1);
        let (h0c, _) = parse_env_heights(&json).expect("hour 1 is complete");
        assert_close(h0c, 5.100, "0C height km");
    }

    #[test]
    fn all_null_hours_parse_to_none() {
        let json = KOAX
            .replace("[5190.00,5100.00]", "[null,null]")
            .replace("[4.1,3.9]", "[null,null]");
        assert_eq!(parse_env_heights(&json), None);
    }

    #[test]
    fn wrong_shapes_parse_to_none_not_a_panic() {
        assert_eq!(parse_env_heights(""), None);
        assert_eq!(parse_env_heights("not json"), None);
        assert_eq!(parse_env_heights("{}"), None);
        assert_eq!(parse_env_heights(r#"{"hourly":{}}"#), None);
        let empty = KOAX
            .replace("[5190.00,5100.00]", "[]")
            .replace("[\"2026-07-28T18:00\",\"2026-07-28T19:00\"]", "[]");
        assert_eq!(parse_env_heights(&empty), None);
    }

    // ── The hour a response is read at ────────────────────────────────────

    fn utc(y: i32, m: u32, d: u32, h: u32, min: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, m, d, h, min, 0).unwrap()
    }

    /// **Defect 1, at the parse seam.** The KOAX fixture holds two hours whose
    /// 0 °C heights differ by 90 m: 18:00 is 5.190 km and 19:00 is 5.100 km.
    /// Asking for one hour must not answer with the other. Taking the first
    /// complete row — which is what this did until the API grew an instant —
    /// answers 5.190 for every question anyone can ask of this response.
    #[test]
    fn the_hour_asked_for_is_the_hour_returned() {
        let (at18, h0_18, _) =
            parse_env_heights_at(KOAX, utc(2026, 7, 28, 18, 0)).expect("18:00 is a complete row");
        assert_eq!(at18, utc(2026, 7, 28, 18, 0));
        assert_close(h0_18, 5.190, "0C height km at 18:00");

        let (at19, h0_19, _) =
            parse_env_heights_at(KOAX, utc(2026, 7, 28, 19, 0)).expect("19:00 is a complete row");
        assert_eq!(at19, utc(2026, 7, 28, 19, 0));
        assert_close(h0_19, 5.100, "0C height km at 19:00");

        assert_ne!(
            h0_18, h0_19,
            "the two hours of this fixture must not resolve to one height",
        );
    }

    /// Every instant inside an hour reads that hour's row: Open-Meteo answers in
    /// whole hours, so a finer question has no finer answer.
    #[test]
    fn an_instant_inside_an_hour_reads_that_hours_row() {
        for minute in [0, 1, 30, 59] {
            let (valid_at, h0, _) = parse_env_heights_at(KOAX, utc(2026, 7, 28, 19, minute))
                .expect("19:00 is a complete row");
            assert_eq!(valid_at, utc(2026, 7, 28, 19, 0), "minute {minute}");
            assert_close(h0, 5.100, "0C height km");
        }
    }

    /// An instant off both ends takes the nearest row it has rather than the
    /// first one listed.
    #[test]
    fn an_instant_outside_the_response_takes_the_nearest_row() {
        let (early, _, _) =
            parse_env_heights_at(KOAX, utc(2026, 7, 28, 4, 0)).expect("a row is still chosen");
        assert_eq!(early, utc(2026, 7, 28, 18, 0), "nearest below the window");
        let (late, _, _) =
            parse_env_heights_at(KOAX, utc(2026, 7, 29, 6, 0)).expect("a row is still chosen");
        assert_eq!(late, utc(2026, 7, 28, 19, 0), "nearest above the window");
    }

    /// The series is every complete hour, in response order.
    #[test]
    fn the_series_lists_every_complete_hour() {
        let series = parse_env_heights_series(KOAX);
        assert_eq!(series.len(), 2, "the fixture has two complete hours");
        assert_eq!(series[0].0, utc(2026, 7, 28, 18, 0));
        assert_eq!(series[1].0, utc(2026, 7, 28, 19, 0));
    }

    /// An hour whose pressure levels are null is dropped, not answered with the
    /// freezing level alone — which is the shape Open-Meteo returns past the
    /// levels' retention window.
    #[test]
    fn an_hour_with_null_pressure_levels_is_not_in_the_series() {
        let json = KOAX.replacen("[4.1,3.9]", "[null,3.9]", 1);
        let series = parse_env_heights_series(&json);
        assert_eq!(series.len(), 1, "only the complete hour survives");
        assert_eq!(series[0].0, utc(2026, 7, 28, 19, 0));
        assert_eq!(
            parse_env_heights_at(&json, utc(2026, 7, 28, 18, 0)).map(|(t, _, _)| t),
            Some(utc(2026, 7, 28, 19, 0)),
            "asking for the null hour falls to the neighbour rather than inventing one",
        );
    }

    /// A response with no rows at all answers nothing.
    #[test]
    fn a_response_with_no_complete_hour_answers_nothing() {
        let json = KOAX
            .replace("[5190.00,5100.00]", "[null,null]")
            .replace("[4.1,3.9]", "[null,null]");
        assert_eq!(parse_env_heights_at(&json, utc(2026, 7, 28, 18, 0)), None);
    }

    // ── The store, and what bounds it ─────────────────────────────────────

    fn sample(valid_at: DateTime<Utc>, h0: f64) -> EnvHeights {
        EnvHeights {
            h0c_km_msl: h0,
            hm20c_km_msl: h0 + 3.0,
            valid_at,
            fetched_at: valid_at + chrono::Duration::days(1),
        }
    }

    /// A sample answers for its own hour and for no other.
    #[test]
    fn the_store_answers_for_the_hour_asked_and_no_other() {
        let mut store = EnvHeightsStore::default();
        store.insert(sample(utc(2026, 7, 28, 18, 0), 5.0));
        assert_eq!(store.at(utc(2026, 7, 28, 18, 0)), Some((5.0, 8.0)));
        assert_eq!(store.at(utc(2026, 7, 28, 18, 59)), Some((5.0, 8.0)));
        assert_eq!(
            store.at(utc(2026, 7, 28, 19, 0)),
            None,
            "the next hour must not be answered with this one's sounding",
        );
    }

    /// **The ceiling, with a reading.** A scrubber drag mints instants without
    /// limit; this drags across a week at one-second steps — 604,800 distinct
    /// instants, every one of them a `set` a naive instant-keyed map would have
    /// kept — and the store must still hold [`MAX_HOURS_PER_SITE`].
    #[test]
    fn an_unbounded_scrubber_drag_cannot_grow_the_store_past_its_cap() {
        let mut store = EnvHeightsStore::default();
        let base = utc(2026, 7, 28, 0, 0);
        let mut inserted = 0u32;
        for step in 0..604_800u32 {
            if step % 97 != 0 {
                continue; // one sample per 97 s of drag, ~6,235 inserts
            }
            store.insert(sample(
                base + chrono::Duration::seconds(i64::from(step)),
                4.0 + f64::from(step % 100) / 100.0,
            ));
            inserted += 1;
            assert!(
                store.len() <= MAX_HOURS_PER_SITE,
                "store reached {} entries after {inserted} inserts",
                store.len(),
            );
        }
        assert!(inserted > 6_000, "the drag must actually be long");
        assert_eq!(
            store.len(),
            MAX_HOURS_PER_SITE,
            "a week-long drag settles exactly at the cap",
        );

        let bytes = store.len() * std::mem::size_of::<EnvHeights>();
        println!(
            "EnvHeightsStore ceiling: {} blocks x {} B = {bytes} B per site              after {inserted} inserts across 604,800 distinct instants",
            store.len(),
            std::mem::size_of::<EnvHeights>(),
        );
        assert!(
            bytes < 2_048,
            "one site's whole sounding cache is {bytes} B",
        );
    }

    /// The cap keeps the hours nearest the newest arrival, in both directions:
    /// a loop walking backwards must not have its own frames evicted.
    #[test]
    fn the_cap_keeps_the_hours_around_the_newest_arrival() {
        let mut store = EnvHeightsStore::default();
        let base = utc(2026, 7, 28, 0, 0);
        // Fill well past the cap walking forward, then land one far in the past.
        for hour in 0..(MAX_HOURS_PER_SITE as i64 + 20) {
            store.insert(sample(base + chrono::Duration::hours(hour), 4.0));
        }
        assert_eq!(store.len(), MAX_HOURS_PER_SITE);
        let newest = base + chrono::Duration::hours(MAX_HOURS_PER_SITE as i64 + 19);
        assert!(store.at(newest).is_some(), "the newest hour survives");
        assert!(
            store.at(base).is_none(),
            "the hour furthest from the newest was shed",
        );
    }

    /// A refetch landing the same two numbers is not a change, so it drops no
    /// renders; a refetch that moves them is.
    #[test]
    fn only_a_moved_pair_reports_a_change() {
        let mut store = EnvHeightsStore::default();
        let hour = utc(2026, 7, 28, 18, 0);
        assert!(store.insert(sample(hour, 5.19)), "the first sample is new");
        assert!(
            !store.insert(sample(hour, 5.19)),
            "the same pair for the same hour did not move",
        );
        assert!(store.insert(sample(hour, 5.10)), "a moved pair is a change");
    }

    /// `covers` is what decides a fetch is owed: it must be false while any
    /// hour of the span is missing and true only once every one is held.
    #[test]
    fn covers_is_false_until_every_hour_of_the_span_is_held() {
        let mut store = EnvHeightsStore::default();
        let from = utc(2026, 7, 28, 12, 0);
        let to = utc(2026, 7, 28, 15, 0);
        assert!(!store.covers(from, to), "an empty store covers nothing");
        for hour in [12, 13, 15] {
            store.insert(sample(utc(2026, 7, 28, hour, 0), 4.0));
        }
        assert!(!store.covers(from, to), "14:00 is still missing");
        store.insert(sample(utc(2026, 7, 28, 14, 0), 4.0));
        assert!(store.covers(from, to), "every hour of the span is held");
        assert!(
            store.covers(from, from),
            "a single-hour span is covered by its own hour",
        );
    }

    /// A whole 24-hour loop — the widest the lookback slider offers — fits under
    /// the cap with room to spare.
    #[test]
    fn the_widest_loop_the_slider_offers_fits_under_the_cap() {
        let mut store = EnvHeightsStore::default();
        let base = utc(2026, 7, 28, 0, 0);
        for hour in 0..=24 {
            store.insert(sample(base + chrono::Duration::hours(hour), 4.0));
        }
        assert_eq!(store.len(), 25, "24 hours of span is 25 hourly buckets");
        assert!(store.len() <= MAX_HOURS_PER_SITE);
        assert!(store.covers(base, base + chrono::Duration::hours(24)));
    }

    // ── TTL ───────────────────────────────────────────────────────────────

    fn heights_at(fetched_at: DateTime<Utc>) -> EnvHeights {
        EnvHeights {
            h0c_km_msl: 4.2,
            hm20c_km_msl: 7.5,
            valid_at: fetched_at,
            fetched_at,
        }
    }

    #[test]
    fn fresh_inside_the_ttl_stale_at_and_past_it() {
        let fetched = chrono::DateTime::parse_from_rfc3339("2026-07-28T18:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let h = heights_at(fetched);

        assert!(!h.is_stale(fetched), "fresh at the instant it was fetched");
        assert!(
            !h.is_stale(fetched + chrono::Duration::minutes(59)),
            "fresh just inside the hour",
        );
        assert!(
            h.is_stale(fetched + chrono::Duration::hours(1)),
            "stale exactly at the TTL",
        );
        assert!(
            h.is_stale(fetched + chrono::Duration::hours(3)),
            "stale well past the TTL",
        );
    }

    /// **A sounding for a past hour never expires.** The values describe an hour
    /// that is already closed, so no amount of elapsed wall-clock time makes
    /// them wrong — and expiring them would refetch the same numbers forever
    /// while a pane sat parked in the past.
    #[test]
    fn a_historical_sounding_never_goes_stale() {
        let valid_at = utc(2013, 5, 20, 20, 0);
        let fetched_at = utc(2026, 7, 28, 18, 0);
        let h = EnvHeights {
            h0c_km_msl: 4.2,
            hm20c_km_msl: 7.5,
            valid_at,
            fetched_at,
        };
        assert!(h.is_historical(), "an hour 13 years before the fetch");
        assert!(!h.is_stale(fetched_at + chrono::Duration::days(365)));
    }

    /// A live sounding still ages out: `valid_at` and `fetched_at` are the same
    /// hour, so nothing about it is final.
    #[test]
    fn a_live_sounding_still_ages_out() {
        let now = utc(2026, 7, 28, 18, 0);
        let h = EnvHeights {
            h0c_km_msl: 4.2,
            hm20c_km_msl: 7.5,
            valid_at: now,
            fetched_at: now,
        };
        assert!(!h.is_historical());
        assert!(h.is_stale(now + chrono::Duration::hours(1)));
    }

    #[test]
    fn a_clock_stepped_backwards_reads_as_fresh() {
        let fetched = chrono::DateTime::parse_from_rfc3339("2026-07-28T18:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        assert!(!heights_at(fetched).is_stale(fetched - chrono::Duration::hours(5)));
    }

    // ── Live ──────────────────────────────────────────────────────────────

    /// `cargo test -p squallar-radar --lib -- --ignored --nocapture live_koax`
    #[cfg(not(target_arch = "wasm32"))]
    #[ignore = "hits the live API"]
    #[tokio::test]
    async fn live_koax_sounding_is_physically_plausible() {
        let sources = DataSources::production();
        let heights = fetch_env_heights(&sources, 41.320, -96.367, Utc::now())
            .await
            .expect("live Open-Meteo fetch + parse should succeed");
        println!(
            "KOAX env heights: 0C {:.3} km MSL, -20C {:.3} km MSL, fetched {}",
            heights.h0c_km_msl, heights.hm20c_km_msl, heights.fetched_at,
        );
        assert!(
            heights.h0c_km_msl > 0.0 && heights.h0c_km_msl < 6.0,
            "0C height {} km is outside (0, 6) km",
            heights.h0c_km_msl,
        );
        assert!(
            heights.hm20c_km_msl > heights.h0c_km_msl,
            "-20C height {} km is not above the 0C height {} km",
            heights.hm20c_km_msl,
            heights.h0c_km_msl,
        );
    }
}
