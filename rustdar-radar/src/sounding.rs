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

use chrono::{DateTime, Utc};
use serde::Deserialize;

use crate::sources::DataSources;

/// How long a fetched [`EnvHeights`] stays fresh.
pub const ENV_HEIGHTS_TTL: std::time::Duration = std::time::Duration::from_secs(60 * 60);

/// The response is under a kilobyte; this is connection-setup allowance for a
/// bad link, not transfer time.
const SOUNDING_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// Environmental freezing-level heights over one point, with the fetch time
/// that [`Self::is_stale`] measures the TTL from.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EnvHeights {
    /// Height of the 0 °C surface, km above mean sea level.
    pub h0c_km_msl: f64,
    /// Height of the −20 °C surface, km above mean sea level.
    pub hm20c_km_msl: f64,
    /// When the fetch completed (UTC).
    pub fetched_at: DateTime<Utc>,
}

impl EnvHeights {
    /// Whether this value has outlived [`ENV_HEIGHTS_TTL`].
    pub fn is_stale(&self, now: DateTime<Utc>) -> bool {
        let ttl = chrono::Duration::from_std(ENV_HEIGHTS_TTL)
            .expect("ENV_HEIGHTS_TTL fits in a chrono::Duration");
        now.signed_duration_since(self.fetched_at) >= ttl
    }
}

/// Fetch the current 0 °C and −20 °C heights above `(lat, lon)`.
pub async fn fetch_env_heights(sources: &DataSources, lat: f64, lon: f64) -> Option<EnvHeights> {
    crate::tls::init();
    let url = sources.sounding_url(lat, lon);
    let client = sources.sounding_client(SOUNDING_TIMEOUT).build().ok()?;
    let response = client.get(&url).send().await.ok()?;
    if !response.status().is_success() {
        log::warn!("Sounding fetch: HTTP {} from {url}", response.status());
        return None;
    }
    let body = response.text().await.ok()?;
    let (h0c_km_msl, hm20c_km_msl) = parse_env_heights(&body)?;
    Some(EnvHeights {
        h0c_km_msl,
        hm20c_km_msl,
        fetched_at: Utc::now(),
    })
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

/// Parse an Open-Meteo response into `(h0c_km_msl, hm20c_km_msl)`.
pub fn parse_env_heights(json: &str) -> Option<(f64, f64)> {
    let response: SoundingResponse = serde_json::from_str(json).ok()?;
    let hours = response.hourly.freezing_level_height.len();
    (0..hours).find_map(|i| {
        let (freezing_m, levels) = response.hourly.row(i)?;
        if !freezing_m.is_finite() {
            return None;
        }
        let hm20_m = height_at_minus20_m(&levels)?;
        Some((freezing_m / 1000.0, hm20_m / 1000.0))
    })
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

    // ── TTL ───────────────────────────────────────────────────────────────

    fn heights_at(fetched_at: DateTime<Utc>) -> EnvHeights {
        EnvHeights {
            h0c_km_msl: 4.2,
            hm20c_km_msl: 7.5,
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

    #[test]
    fn a_clock_stepped_backwards_reads_as_fresh() {
        let fetched = chrono::DateTime::parse_from_rfc3339("2026-07-28T18:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        assert!(!heights_at(fetched).is_stale(fetched - chrono::Duration::hours(5)));
    }

    // ── Live ──────────────────────────────────────────────────────────────

    /// `cargo test -p rustdar-radar --lib -- --ignored --nocapture live_koax`
    #[cfg(not(target_arch = "wasm32"))]
    #[ignore = "hits the live API"]
    #[tokio::test]
    async fn live_koax_sounding_is_physically_plausible() {
        let sources = DataSources::production();
        let heights = fetch_env_heights(&sources, 41.320, -96.367)
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
