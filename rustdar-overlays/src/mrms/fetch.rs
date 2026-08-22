//! Finding and downloading the newest MRMS granule.
//!
//! ## Why the key is always listed and never constructed
//!
//! MRMS publishes about every two minutes, and **the timestamps are not
//! clock-aligned**: one observed hour of the composite ran `000039`, `000242`,
//! `000442`, `000641`. Rounding a wall clock to the nearest two minutes and
//! building a key produces a 404 essentially always. Every fetch therefore
//! lists a day prefix and takes the **last** key, which is what "newest" means
//! in a bucket whose keys sort by their own timestamp.
//!
//! The `start-after` bound is the one place a key is *constructed*, and it is
//! constructed as a **lower bound for a listing**, never as an object to GET: an
//! ordinary day holds ~720 keys, so an unbounded listing is one request of ~90 KB
//! every two minutes, against ~1 KB for the bounded one.
//!
//! ## The S3 XML is parsed here rather than reused
//!
//! `rustdar-radar` has a paginating listing walker. It stays there: the
//! overlays→radar edge is cut and pinned by
//! `rustdar-source/tests/charter.rs::the_overlays_to_radar_edge_stays_cut`. This
//! follows `glm::fetch`'s in-crate `roxmltree` shape instead, which is the same
//! decision taken for the same reason.

use chrono::{Duration, NaiveDate, NaiveDateTime, Utc};
use rustdar_source::origins::DataSources;

use super::{MrmsFetchResult, MrmsGrid, MrmsProduct};
use crate::fetch_policy::{FetchError, NotFound};

/// How far back the cheap first listing reaches.
///
/// Comfortably past the ~2-minute cadence, so an ordinary poll finds ~15 keys;
/// short enough that the page is a kilobyte. A feed stalled longer than this
/// falls through to the unbounded listing rather than reporting nothing.
const RECENT_LOOKBACK_MINUTES: i64 = 30;

/// Pages to follow before giving up, matching the shape `glm::fetch` uses. A day
/// prefix is ~720 keys and S3 pages at 1000, so a complete day is one page and
/// this is slack for a product that publishes faster.
const MAX_LIST_PAGES: usize = 8;

/// The prefixes to try, in order, for "the newest granule as of `now`".
///
/// Pure, so the ordering is testable without a wall clock — see
/// "assert the property, not the clock".
///
/// 1. today's prefix, bounded to the last [`RECENT_LOOKBACK_MINUTES`] — the
///    common case, one small page;
/// 2. today's prefix unbounded — a feed stalled longer than the lookback, and
///    the first minutes after UTC midnight;
/// 3. yesterday's prefix — the first granule of a day has not landed yet, or
///    the feed is down and the last thing published was yesterday.
///
/// The bounded attempt is skipped when the lookback crosses midnight, because
/// its key would name yesterday's directory and bound nothing.
pub(crate) fn listing_attempts(now: NaiveDateTime) -> Vec<(NaiveDate, Option<NaiveDateTime>)> {
    let back = now - Duration::minutes(RECENT_LOOKBACK_MINUTES);
    let mut attempts = Vec::with_capacity(3);
    if back.date() == now.date() {
        attempts.push((now.date(), Some(back)));
    }
    attempts.push((now.date(), None));
    attempts.push(((now - Duration::days(1)).date(), None));
    attempts
}

/// Percent-encode a continuation token for a query string. S3 tokens are base64
/// and carry `+`, `/` and `=`.
fn urlencoded(value: &str) -> String {
    value
        .bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// Every key under one day prefix, above `start_after` when one is given, in the
/// bucket's own (timestamp) order.
async fn list_day(
    client: &reqwest::Client,
    sources: &DataSources,
    product: MrmsProduct,
    date: NaiveDate,
    start_after: Option<NaiveDateTime>,
) -> Result<Vec<String>, FetchError> {
    let prefix = DataSources::mrms_day_prefix(product.prefix_name(), &date);
    let bucket = sources.s3_bucket_url(&sources.mrms_bucket);
    let mut keys: Vec<String> = Vec::new();
    let mut token: Option<String> = None;

    for _ in 0..MAX_LIST_PAGES {
        let mut url = format!("{bucket}/?list-type=2&prefix={prefix}");
        if let Some(after) = start_after {
            url.push_str("&start-after=");
            url.push_str(&urlencoded(&DataSources::mrms_key(
                product.prefix_name(),
                &after,
            )));
        }
        if let Some(ref t) = token {
            url.push_str("&continuation-token=");
            url.push_str(&urlencoded(t));
        }

        let resp = client.get(&url).send().await.map_err(|e| {
            FetchError::from_transport(&e, format!("MRMS list request failed: {e}"))
        })?;
        if !resp.status().is_success() {
            // `IsBroken`: a bucket listing is not published on a schedule, so a
            // 404 on `?list-type=2` means the bucket is gone or renamed rather
            // than "not up yet". An absent *day* is an empty listing, not a 404.
            return Err(FetchError::from_status(
                resp.status(),
                NotFound::IsBroken,
                format!("MRMS listing returned HTTP {}", resp.status()),
            ));
        }
        let body = resp.text().await.map_err(|e| {
            FetchError::from_transport(&e, format!("MRMS listing body read failed: {e}"))
        })?;

        let doc = roxmltree::Document::parse(&body)
            .map_err(|e| FetchError::transient(format!("MRMS listing is not XML: {e}")))?;
        for node in doc.descendants() {
            if node.tag_name().name() == "Key"
                && let Some(key) = node.text()
                && key.ends_with(".grib2.gz")
            {
                keys.push(key.to_string());
            }
        }

        let truncated = doc
            .descendants()
            .find(|n| n.tag_name().name() == "IsTruncated")
            .and_then(|n| n.text())
            .is_some_and(|t| t == "true");
        if !truncated {
            return Ok(keys);
        }

        // A truncated page with no usable token would re-issue the identical
        // request for ever; so would a token the server repeated.
        let next = doc
            .descendants()
            .find(|n| n.tag_name().name() == "NextContinuationToken")
            .and_then(|n| n.text())
            .filter(|t| !t.is_empty())
            .map(|s| s.to_string());
        let Some(next) = next else {
            log::warn!(
                "MRMS: listing for {prefix:?} is truncated but carries no \
                 continuation token; using the {} keys already read",
                keys.len(),
            );
            return Ok(keys);
        };
        if token.as_deref() == Some(next.as_str()) {
            log::warn!("MRMS: S3 repeated a continuation token for {prefix:?}; stopping");
            return Ok(keys);
        }
        token = Some(next);
    }

    // Not an error: the keys already read are real and the newest of them is a
    // real granule, just possibly not the newest one. Saying so beats refusing
    // to draw anything.
    log::warn!("MRMS: listing for {prefix:?} did not finish within {MAX_LIST_PAGES} pages");
    Ok(keys)
}

/// The newest granule's key for `product`.
///
/// The wall clock is read in exactly one place — here — and everything that
/// *depends* on it is [`listing_attempts`], which is pure and is what the tests
/// drive.
pub async fn latest_key(
    client: &reqwest::Client,
    sources: &DataSources,
    product: MrmsProduct,
) -> Result<String, FetchError> {
    let mut last_error: Option<FetchError> = None;
    for (date, start_after) in listing_attempts(Utc::now().naive_utc()) {
        match list_day(client, sources, product, date, start_after).await {
            // `max()`, not `last()`: S3 answers in lexicographic key order
            // today, and these keys sort by their own timestamp, but the
            // ordering of a listing is the server's promise rather than ours.
            Ok(keys) => {
                if let Some(newest) = keys.into_iter().max() {
                    return Ok(newest);
                }
            }
            Err(e) => last_error = Some(e),
        }
    }
    Err(last_error.unwrap_or_else(|| {
        // `absent`, not `transient`: an empty listing across today and
        // yesterday is "the feed has published nothing for over a day", which
        // is a real answer and must not read as a broken request.
        FetchError::absent(format!(
            "MRMS: no {} granule in the last two UTC days",
            product.prefix_name(),
        ))
    }))
}

/// Download and decode the newest granule for `product`.
pub async fn fetch_latest(
    client: &reqwest::Client,
    sources: &DataSources,
    product: MrmsProduct,
) -> MrmsFetchResult {
    MrmsFetchResult(fetch_latest_inner(client, sources, product).await)
}

async fn fetch_latest_inner(
    client: &reqwest::Client,
    sources: &DataSources,
    product: MrmsProduct,
) -> Result<MrmsGrid, FetchError> {
    let key = latest_key(client, sources, product).await?;
    let url = sources.s3_object_url(&sources.mrms_bucket, &key);
    log::info!("Fetching MRMS {} from {url}", product.as_str());

    let resp = client
        .get(&url)
        .send()
        .await
        .map_err(|e| FetchError::from_transport(&e, format!("MRMS request failed: {e}")))?;
    if !resp.status().is_success() {
        // `IsRoutine`: the key came out of a listing, so a 404 here means the
        // object was expired or replaced between the two requests — normal, and
        // the next poll two minutes later will name a different key.
        return Err(FetchError::from_status(
            resp.status(),
            NotFound::IsRoutine,
            format!("MRMS {key}: HTTP {}", resp.status()),
        ));
    }
    let body = resp
        .bytes()
        .await
        .map_err(|e| FetchError::from_transport(&e, format!("MRMS body read failed: {e}")))?;

    decode_body(&body, product)
}

/// gunzip and decode, with the domain refusal kept **permanent** and everything
/// else transient — the same split `hrrr::fetch::classify_parse_error` makes,
/// and for the same reason: a grid outside the envelope will be outside it on
/// every retry, so the retry ladder must not keep asking.
pub(crate) fn decode_body(body: &[u8], product: MrmsProduct) -> Result<MrmsGrid, FetchError> {
    let grib = super::decode::gunzip(body).map_err(FetchError::transient)?;
    super::decode::parse_grib2(&grib, product).map_err(|message| {
        if message.contains("unsupported model domain") {
            FetchError::permanent(message)
        } else {
            FetchError::transient(message)
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(y: i32, m: u32, d: u32, h: u32, min: u32) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(y, m, d)
            .unwrap()
            .and_hms_opt(h, min, 0)
            .unwrap()
    }

    /// Mid-day: the cheap bounded listing first, then the two unbounded
    /// fallbacks. No wall clock in the assertion.
    #[test]
    fn a_midday_fetch_tries_the_bounded_listing_first() {
        let attempts = listing_attempts(at(2026, 8, 21, 18, 12));
        assert_eq!(attempts.len(), 3);
        assert_eq!(
            attempts[0],
            (
                NaiveDate::from_ymd_opt(2026, 8, 21).unwrap(),
                Some(at(2026, 8, 21, 17, 42)),
            ),
        );
        assert_eq!(
            attempts[1],
            (NaiveDate::from_ymd_opt(2026, 8, 21).unwrap(), None),
        );
        assert_eq!(
            attempts[2],
            (NaiveDate::from_ymd_opt(2026, 8, 20).unwrap(), None),
        );
    }

    /// Just after UTC midnight the lookback lands in yesterday, where a
    /// `start-after` key would bound nothing — so the bounded attempt is
    /// dropped rather than issued and wasted.
    #[test]
    fn a_fetch_just_after_midnight_skips_the_bounded_listing() {
        let attempts = listing_attempts(at(2026, 8, 21, 0, 4));
        assert_eq!(attempts.len(), 2);
        assert!(
            attempts.iter().all(|(_, after)| after.is_none()),
            "a bounded listing across the day boundary would name yesterday's \
             directory: {attempts:?}",
        );
        assert_eq!(attempts[0].0, NaiveDate::from_ymd_opt(2026, 8, 21).unwrap());
        assert_eq!(attempts[1].0, NaiveDate::from_ymd_opt(2026, 8, 20).unwrap());
    }

    /// The month and year roll back with the day, which a bare `day - 1` would
    /// not do.
    #[test]
    fn the_fallback_prefix_rolls_back_over_a_month_boundary() {
        let attempts = listing_attempts(at(2026, 1, 1, 0, 10));
        assert_eq!(
            attempts.last().unwrap().0,
            NaiveDate::from_ymd_opt(2025, 12, 31).unwrap(),
        );
    }

    /// The `start-after` bound is a key under the same prefix the listing walks,
    /// or S3 returns the whole day and the optimisation is a lie.
    #[test]
    fn the_bounded_listing_names_a_key_inside_its_own_prefix() {
        let now = at(2026, 8, 21, 18, 12);
        let (date, after) = listing_attempts(now)[0];
        let after = after.expect("the midday attempt is the bounded one");
        let prefix = DataSources::mrms_day_prefix("MergedReflectivityQCComposite_00.50", &date);
        let key = DataSources::mrms_key("MergedReflectivityQCComposite_00.50", &after);
        assert!(key.starts_with(&prefix), "{key} is not under {prefix}");
    }

    #[test]
    fn a_continuation_token_survives_the_query_string() {
        let token = "14rsJHBpQGY/M4yVmYVY23Q7XBUolMyy6Iyh9aw1ZAfovwfu9oAeTB0Rv7EQHn2cpJrQ16sY6QTgFT5WXiy1v5iMplKfKTxg33G341P64zrA=";
        let encoded = urlencoded(token);
        assert!(!encoded.contains('/'), "{encoded}");
        assert!(!encoded.contains('='), "{encoded}");
        assert!(!encoded.contains('+'), "{encoded}");
        assert!(encoded.contains("%2F") && encoded.contains("%3D"));
    }

    /// Garbage in must not decode to a plausible mosaic.
    #[test]
    fn a_body_that_is_not_a_gzip_member_is_refused() {
        let err = decode_body(b"not gzip at all", MrmsProduct::ReflectivityComposite)
            .expect_err("plain bytes are not a granule");
        assert!(err.message.contains("gunzip"), "{}", err.message);
    }

    /// **The whole path against the real bucket**: list, take the newest key,
    /// download, gunzip, decode, window.
    ///
    /// Everything above this test is either pure or a fixture, so nothing else
    /// in the suite can catch a listing URL S3 rejects or a product directory
    /// NOAA renamed. `#[ignore]`d because it is network, exactly as the HRRR
    /// live tests are.
    ///
    /// `cargo test -p rustdar-overlays -- --ignored --nocapture live_mrms`
    #[cfg(not(target_arch = "wasm32"))]
    #[tokio::test]
    #[ignore = "hits the live noaa-mrms-pds S3 bucket"]
    async fn live_mrms_fetches_and_decodes_every_shipped_product() {
        // Through `rustdar_source::tls`, like every other client in the tree:
        // the workspace pins `rustls-no-provider`, so a bare
        // `reqwest::Client::builder()` panics for want of a crypto provider.
        let client = rustdar_source::tls::client(
            rustdar_source::tls::USER_AGENT,
            std::time::Duration::from_secs(120),
        )
        .build()
        .expect("client");
        let sources = DataSources::production();

        for &product in MrmsProduct::all() {
            let key = latest_key(&client, &sources, product)
                .await
                .unwrap_or_else(|e| panic!("{}: no key: {e}", product.as_str()));
            assert!(
                key.starts_with(&format!("CONUS/{}/", product.prefix_name())),
                "{key} is not under the product's own prefix",
            );
            assert!(!key.contains("5KM"), "{key} addresses the dead prefix");

            let grid = match fetch_latest(&client, &sources, product).await.0 {
                Ok(grid) => grid,
                Err(e) => panic!("{}: {e}", product.as_str()),
            };
            assert_eq!((grid.grid.ni, grid.grid.nj), (7000, 3500));
            assert!(
                matches!(grid.grid.coords, crate::hrrr::GridCoords::Regular { .. }),
                "the live product must decode to the windowable arm",
            );
            // Freshly published, so the granule is minutes old at most. A day
            // is generous slack for a stalled feed and still catches a fetch
            // that silently landed on last year's key.
            let age = Utc::now().naive_utc() - grid.valid;
            assert!(
                age < Duration::days(1) && age > Duration::hours(-1),
                "{}: valid {} is {age} from now",
                product.as_str(),
                grid.valid,
            );
            let (lo, hi) = grid.value_range.expect("a live granule has readings");
            // **Against the live feed, not the fixture**: this is the one check
            // that would notice NOAA moving a product onto a different reserved
            // code, which is exactly how the rate's −3 was found.
            for &code in super::MrmsProduct::known_reserved_codes() {
                assert!(
                    (lo - code).abs() > 0.05,
                    "{}: the live decode reported the reserved code {code} as \
                     its minimum reading, so a sentinel reached the summary",
                    product.as_str(),
                );
            }
            if product == MrmsProduct::PrecipRate {
                assert!(lo >= 0.0, "a rain rate of {lo} mm/h is not a measurement",);
            }
            println!(
                "{}: {key}\n  valid {} | {} drawable of {} | range {lo}..{hi} {}",
                product.as_str(),
                grid.valid,
                grid.visible_points,
                grid.grid.values.len(),
                product.unit_label(),
            );
        }
    }
}
