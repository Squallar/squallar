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
//! `squallar-radar` has a paginating listing walker. It stays there: the
//! overlays→radar edge is cut and pinned by
//! `squallar-source/tests/charter.rs::the_overlays_to_radar_edge_stays_cut`. This
//! follows `glm::fetch`'s in-crate `roxmltree` shape instead, which is the same
//! decision taken for the same reason.

use chrono::{Duration, NaiveDate, NaiveDateTime};
use squallar_source::origins::DataSources;

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
    at: NaiveDateTime,
) -> Result<String, FetchError> {
    let mut last_error: Option<FetchError> = None;
    for (date, start_after) in listing_attempts(at) {
        match list_day(client, sources, product, date, start_after).await {
            // `max()`, not `last()`: S3 answers in lexicographic key order
            // today, and these keys sort by their own timestamp, but the
            // ordering of a listing is the server's promise rather than ours.
            //
            // FILTERED TO `at` FIRST, and the filter is load-bearing whenever
            // `at` is not now. `listing_attempts` bounds where the listing
            // STARTS, never where it ends: asked about 15:00Z it still lists
            // that whole UTC day, so the unfiltered `max()` would answer with
            // the newest granule of the day — this evening's mosaic drawn over
            // a mid-afternoon instant. A key with no decodable stamp is
            // dropped rather than kept, because an undatable key cannot be
            // shown to be at or before anything.
            Ok(keys) => {
                let newest = keys
                    .into_iter()
                    .filter(|key| key_valid_time(key).is_some_and(|valid| valid <= at))
                    .max();
                if let Some(newest) = newest {
                    return Ok(newest);
                }
            }
            Err(e) => last_error = Some(e),
        }
    }
    Err(last_error.unwrap_or_else(|| {
        // `absent`, not `transient`: an empty listing across the day of `at`
        // and the one before it is "the feed published nothing for over a day
        // up to that instant", which is a real answer and must not read as a
        // broken request.
        FetchError::absent(format!(
            "MRMS: no {} granule in the two UTC days up to {at}",
            product.prefix_name(),
        ))
    }))
}

/// Download and decode the newest granule for `product` at or before `at`.
///
/// `at` is the instant the pane depicts, which on a live pane is the wall clock
/// — so a live pane's request is byte-for-byte the one this made before it took
/// the argument.
pub async fn fetch_latest(
    client: &reqwest::Client,
    sources: &DataSources,
    product: MrmsProduct,
    at: NaiveDateTime,
) -> MrmsFetchResult {
    MrmsFetchResult(fetch_latest_inner(client, sources, product, at).await)
}

async fn fetch_latest_inner(
    client: &reqwest::Client,
    sources: &DataSources,
    product: MrmsProduct,
    at: NaiveDateTime,
) -> Result<MrmsGrid, FetchError> {
    let key = latest_key(client, sources, product, at).await?;
    log::info!("Fetching MRMS {} key {key}", product.as_str());
    fetch_key(client, sources, product, &key).await
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

/// **The most day prefixes one frame listing will ever request.**
///
/// A ceiling on request *count*, not on the window: past it the days are
/// evenly sampled with both ends kept, so a wider window still spans the same
/// ground with fewer days listed inside it, and the listing says
/// `complete: false`.
///
/// Four is double the widest window the Lookback slider can name — 1440
/// minutes touches at most **two** UTC day prefixes — so it does not bind on
/// any window a user can ask for today. Where GMGSI's counterpart is 26, this
/// is 4, because the unit of listing is different: an MRMS day prefix answers
/// every stamp of its ~720-granule day in **one** request (S3 pages at 1000),
/// where a GMGSI hour prefix answers one.
pub(crate) const MAX_FRAME_LIST_REQUESTS: usize = 4;

/// The UTC days whose prefixes cover `range`, at most
/// [`MAX_FRAME_LIST_REQUESTS`] of them, **endpoint-anchored** when there are
/// more days than that.
pub(crate) fn days_in_range(range: (NaiveDateTime, NaiveDateTime)) -> Vec<NaiveDate> {
    let (first, last) = (range.0.date(), range.1.date());
    if last < first {
        return Vec::new();
    }
    let n = (last - first).num_days() as usize + 1;
    if n <= MAX_FRAME_LIST_REQUESTS {
        return (0..n).map(|k| first + Duration::days(k as i64)).collect();
    }
    // Both ends exact: index 0 and index `n - 1` are always produced, so the
    // ground the window covers is unchanged and only its resolution falls.
    let m = MAX_FRAME_LIST_REQUESTS;
    (0..m)
        .map(|k| first + Duration::days((k * (n - 1) / (m - 1)) as i64))
        .collect()
}

/// The valid time an MRMS object key carries in its own name —
/// `..._YYYYMMDD-HHMMSS.grib2.gz` — or `None` for a key that is not one.
///
/// Read off the key rather than off GRIB2 section 1 because at listing time
/// there are no GRIB2 bytes: the stamp is what a frame is *addressed* by, and
/// the granule's own section-1 time is what the live path reports once the
/// bytes arrive.
pub(crate) fn key_valid_time(key: &str) -> Option<NaiveDateTime> {
    let name = key.rsplit('/').next()?;
    let stem = name.strip_suffix(".grib2.gz")?;
    let (_, stamp) = stem.rsplit_once('_')?;
    NaiveDateTime::parse_from_str(stamp, "%Y%m%d-%H%M%S").ok()
}

/// **Every granule of `product` inside `range`, as `(valid stamp, object
/// key)`, with whether that is known to be all of them.**
///
/// The cost, with its denominator: **1 LIST per UTC day the window touches**
/// — at most two for any window the Lookback slider can name, ~90 KB of XML
/// each — and **1 GET per frame later**, ~1.3 MB gzipped apiece. Per frame
/// that is *cheaper* than GMGSI, whose unpredictable creation stamps force 1
/// LIST per hour; MRMS keys are pure functions of their own timestamp, so one
/// day listing names the whole day's ~720 stamps at once. At the slider's
/// default hour a loop is one LIST and ~30 GETs (~40 MB), serialised by the
/// handler's frame gate.
///
/// The key is carried rather than the stamp alone because **timestamps are
/// not clock-aligned** (`000039`, `000242`, `000442`): a stamp cannot be
/// rounded back into a key, so a frame fetched later would otherwise have to
/// re-list its day.
///
/// A day with no keys is an ordinary absence and does not make the answer
/// incomplete. A listing that *errored* does, and so does a range wider than
/// [`MAX_FRAME_LIST_REQUESTS`] days.
pub async fn list_frame_keys(
    client: &reqwest::Client,
    sources: &DataSources,
    product: MrmsProduct,
    range: (NaiveDateTime, NaiveDateTime),
) -> (Vec<(NaiveDateTime, String)>, bool) {
    let days = days_in_range(range);
    let sampled = match (days.first(), days.last()) {
        (Some(first), Some(last)) => (*last - *first).num_days() as usize + 1 != days.len(),
        _ => false,
    };
    let mut keys: Vec<(NaiveDateTime, String)> = Vec::new();
    let mut every_day_answered = true;
    for day in days {
        // The first day's listing is bounded to the window's own start — the
        // same `start-after` economy the live poll uses, ~1 KB against ~90 KB.
        let start_after =
            (day == range.0.date() && range.0.time() != chrono::NaiveTime::MIN).then_some(range.0);
        match list_day(client, sources, product, day, start_after).await {
            Ok(listed) => {
                keys.extend(listed.into_iter().filter_map(|key| {
                    let valid = key_valid_time(&key)?;
                    (range.0 <= valid && valid <= range.1).then_some((valid, key))
                }));
            }
            Err(e) => {
                every_day_answered = false;
                log::warn!("MRMS frame listing of {day} failed: {e:?}");
            }
        }
    }
    keys.sort_by_key(|(valid, _)| *valid);
    keys.dedup_by(|a, b| a.0 == b.0);
    log::info!(
        "MRMS {}: listed {} frames in the window",
        product.as_str(),
        keys.len(),
    );
    (keys, every_day_answered && !sampled)
}

/// **One granule, by the key a listing already found**, decoded.
///
/// The GET half of [`fetch_latest`], for the frame path: a loop frame's key
/// came off [`list_frame_keys`] and must not be re-listed to be fetched.
pub async fn fetch_key(
    client: &reqwest::Client,
    sources: &DataSources,
    product: MrmsProduct,
    key: &str,
) -> Result<MrmsGrid, FetchError> {
    let url = sources.s3_object_url(&sources.mrms_bucket, key);
    let resp = client
        .get(&url)
        .send()
        .await
        .map_err(|e| FetchError::from_transport(&e, format!("MRMS request failed: {e}")))?;
    if !resp.status().is_success() {
        // `IsRoutine`: the key came out of a listing, so a 404 here means the
        // object was expired or replaced between the two requests.
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

#[cfg(test)]
mod tests {
    use super::*;
    // Only the live-network check reads the wall clock now: the fetch path
    // takes the instant it is asked about, and that check is native-only.
    #[cfg(not(target_arch = "wasm32"))]
    use chrono::Utc;

    fn instant(h: u32, m: u32) -> NaiveDateTime {
        chrono::NaiveDate::from_ymd_opt(2026, 8, 24)
            .expect("a real date")
            .and_hms_opt(h, m, 0)
            .expect("a real time")
    }

    fn key_at(h: u32, m: u32) -> String {
        format!(
            "CONUS/MergedReflectivityQCComposite_00.50/20260824/\
             MRMS_MergedReflectivityQCComposite_00.50_20260824-{h:02}{m:02}40.grib2.gz"
        )
        .replace(' ', "")
    }

    /// **The granule chosen for a past instant is the newest at or before it**,
    /// not the newest that exists.
    ///
    /// This is the whole of the depicted-instant contract for this layer, and
    /// the filter it tests is the one `listing_attempts` cannot supply: that
    /// function bounds where a listing STARTS, and asked about 15:00Z it still
    /// lists the rest of the UTC day. Before the filter, a pane parked at 15:00Z
    /// was handed the 21:56Z mosaic — measured, and drawn over an afternoon scan.
    #[test]
    fn a_past_instant_takes_the_newest_granule_at_or_before_it() {
        let keys = [
            key_at(14, 50),
            key_at(14, 58),
            key_at(15, 4),
            key_at(21, 56),
        ];

        let chosen = |t: NaiveDateTime| {
            keys.iter()
                .filter(|k| key_valid_time(k).is_some_and(|v| v <= t))
                .max()
                .cloned()
        };

        assert_eq!(
            chosen(instant(15, 0)),
            Some(key_at(14, 58)),
            "the newest granule at or before 15:00Z is the 14:58Z one",
        );
        assert_eq!(
            chosen(instant(23, 0)),
            Some(key_at(21, 56)),
            "asked about now, the answer is still the newest — a live pane's \
             request must not move because this filter exists",
        );
        assert_eq!(
            chosen(instant(14, 0)),
            None,
            "an instant before every granule takes none of them rather than \
             the oldest",
        );
    }

    /// A key whose stamp does not decode is dropped, never kept.
    ///
    /// An undatable key cannot be shown to be at or before anything, and
    /// keeping it would let one malformed object win a `max()` over correctly
    /// stamped ones — it sorts by bytes, not by time.
    #[test]
    fn an_undatable_key_is_not_eligible() {
        assert_eq!(key_valid_time("CONUS/x/20260824/nonsense.grib2.gz"), None);
        let keys = [
            "CONUS/x/20260824/zzz_nonsense.grib2.gz".to_string(),
            key_at(14, 58),
        ];
        let chosen = keys
            .iter()
            .filter(|k| key_valid_time(k).is_some_and(|v| v <= instant(15, 0)))
            .max()
            .cloned();
        assert_eq!(chosen, Some(key_at(14, 58)));
    }

    /// The listing walks back from the instant it was asked about, not from now.
    #[test]
    fn the_listing_walks_back_from_the_instant_it_was_asked_about() {
        let attempts = listing_attempts(instant(15, 0));
        for (date, _) in &attempts {
            assert!(
                *date <= instant(15, 0).date(),
                "{date} is after the instant asked about",
            );
        }
        assert!(
            attempts.iter().any(|(d, _)| *d == instant(15, 0).date()),
            "the day of the instant must be listed: {attempts:?}",
        );
    }

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

    /// **A frame listing costs 1 LIST per UTC day the window touches, and it
    /// is bounded.** The widest window the Lookback slider can name is 1440
    /// minutes, which touches at most two day prefixes — asserted as a const
    /// so a bound that started binding on a real window is a build failure.
    #[test]
    fn a_frame_listing_costs_one_request_per_day_and_is_bounded() {
        let day = |d: u32| NaiveDate::from_ymd_opt(2026, 8, d).unwrap();
        let mid = |d: u32| day(d).and_hms_opt(12, 0, 0).unwrap();

        // The slider's own ceiling: 24 h crossing midnight is two days.
        assert_eq!(days_in_range((mid(20), mid(21))), vec![day(20), day(21)]);
        const _: () = assert!(2 <= MAX_FRAME_LIST_REQUESTS);

        // A window inside one day is one request.
        assert_eq!(
            days_in_range((mid(20), day(20).and_hms_opt(18, 0, 0).unwrap())),
            vec![day(20)],
        );

        // A window nothing can ask for today: bounded, both ends kept, in
        // order, no duplicates.
        let wide = days_in_range((mid(1), mid(29)));
        assert_eq!(
            wide.len(),
            MAX_FRAME_LIST_REQUESTS,
            "a 29-day window would be 29 LIST requests unbounded"
        );
        assert_eq!(wide.first(), Some(&day(1)));
        assert_eq!(wide.last(), Some(&day(29)));
        assert!(wide.windows(2).all(|w| w[0] < w[1]));

        assert!(days_in_range((mid(21), mid(20))).is_empty());
    }

    /// The stamp a key carries in its own name round-trips through the same
    /// helper production keys are built with — including the non-clock-aligned
    /// seconds that make the stamp unreconstructable from a wall clock.
    #[test]
    fn a_keys_own_stamp_is_what_the_listing_files_it_under() {
        let stamp = at(2026, 8, 21, 0, 2) + Duration::seconds(42); // 000242
        let key = DataSources::mrms_key("MergedReflectivityQCComposite_00.50", &stamp);
        assert_eq!(key_valid_time(&key), Some(stamp));

        // Not a granule key: no stamp to file under, so it is skipped rather
        // than misfiled.
        assert_eq!(key_valid_time("CONUS/PrecipRate_00.00/20260821/"), None);
        assert_eq!(key_valid_time("MRMS_notastamp.grib2.gz"), None);
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
    /// `cargo test -p squallar-overlays -- --ignored --nocapture live_mrms`
    #[cfg(not(target_arch = "wasm32"))]
    #[tokio::test]
    #[ignore = "hits the live noaa-mrms-pds S3 bucket"]
    async fn live_mrms_fetches_and_decodes_every_shipped_product() {
        // Through `squallar_source::tls`, like every other client in the tree:
        // the workspace pins `rustls-no-provider`, so a bare
        // `reqwest::Client::builder()` panics for want of a crypto provider.
        let client = squallar_source::tls::client(
            squallar_source::tls::USER_AGENT,
            std::time::Duration::from_secs(120),
        )
        .build()
        .expect("client");
        let sources = DataSources::production();

        for &product in MrmsProduct::all() {
            let key = latest_key(&client, &sources, product, Utc::now().naive_utc())
                .await
                .unwrap_or_else(|e| panic!("{}: no key: {e}", product.as_str()));
            assert!(
                key.starts_with(&format!("CONUS/{}/", product.prefix_name())),
                "{key} is not under the product's own prefix",
            );
            assert!(!key.contains("5KM"), "{key} addresses the dead prefix");

            let grid = match fetch_latest(&client, &sources, product, Utc::now().naive_utc())
                .await
                .0
            {
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
