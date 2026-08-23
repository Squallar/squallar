//! Fetching a GMGSI granule.
//!
//! # Why the key is always listed and never constructed
//!
//! Every object name ends in the moment the blend was *created*:
//! `GLOBCOMPLIR_v3r0_blend_s202506011200000_e202506011209599_c202506011234579.nc`.
//! The `_s` and `_e` stamps are predictable — the top of the hour and nine
//! minutes 59.9 seconds later — but `_c` is not: across the four channels of one
//! observed hour it read `1234579`, `1237208`, `1242310` and `1239397`, i.e. it
//! trails the observation by 34 to 42 minutes and by a different amount per
//! channel. No clock can produce it, so there is nothing to GET without first
//! listing.
//!
//! The listing is cheap in a way MRMS's is not: an hour prefix holds exactly
//! **one** object, so one request returns one key and there is no pagination to
//! walk. That is why this module has no continuation-token loop.
//!
//! # Latency
//!
//! The creation lag above means the granule for hour H is not in the bucket
//! until roughly H+40 minutes. [`listing_attempts`] therefore starts two hours
//! back rather than at the current hour, which would 404 for most of every
//! hour.

use chrono::{NaiveDateTime, Timelike};
use rustdar_source::fetch_policy::{FetchError, NotFound};
use rustdar_source::origins::DataSources;

use super::GmgsiChannel;
use super::decode::GmgsiGrid;

/// How many hours back to try before giving up.
///
/// Four: the blend lands ~40 minutes after the hour it covers, so the current
/// hour is usually absent and the previous one is usually present. Two more
/// cover a stalled feed without turning a dead source into a long request
/// storm.
const LOOKBACK_HOURS: i64 = 4;

/// The hours to try, newest first, for "the newest granule as of `now`".
pub(crate) fn listing_attempts(now: NaiveDateTime) -> Vec<NaiveDateTime> {
    let top = now
        .with_minute(0)
        .and_then(|t| t.with_second(0))
        .and_then(|t| t.with_nanosecond(0))
        .unwrap_or(now);
    (0..LOOKBACK_HOURS)
        .map(|h| top - chrono::Duration::hours(h))
        .collect()
}

/// The single object key under one channel's hour prefix, if the hour has
/// landed.
///
/// `Ok(None)` for an hour with no object — an ordinary state for the current
/// hour, not an error. Objects that are not the `v3r0_blend` product are
/// skipped rather than taken: the retired legacy granule shared these prefixes
/// until mid-2025 and a historical query can still meet one.
pub(crate) async fn list_hour(
    client: &reqwest::Client,
    sources: &DataSources,
    channel: GmgsiChannel,
    hour: NaiveDateTime,
) -> Result<Option<String>, FetchError> {
    let prefix = DataSources::gmgsi_hour_prefix(channel.prefix(), &hour);
    let bucket = sources.s3_bucket_url(&sources.gmgsi_bucket);
    let url = format!("{bucket}/?list-type=2&prefix={prefix}");

    let resp =
        client.get(&url).send().await.map_err(|e| {
            FetchError::from_transport(&e, format!("GMGSI list request failed: {e}"))
        })?;
    if !resp.status().is_success() {
        // `IsBroken` for the same reason MRMS's listing says so: a bucket
        // listing is not published on a schedule, so a non-200 means the bucket
        // is gone or renamed. An absent hour is an empty listing, not a 404.
        return Err(FetchError::from_status(
            resp.status(),
            NotFound::IsBroken,
            format!("GMGSI listing returned HTTP {}", resp.status()),
        ));
    }
    let body = resp.text().await.map_err(|e| {
        FetchError::from_transport(&e, format!("GMGSI listing body read failed: {e}"))
    })?;
    Ok(newest_blend_key(&body, channel))
}

/// The newest `v3r0_blend` key in an S3 `ListObjectsV2` response body.
pub(crate) fn newest_blend_key(body: &str, channel: GmgsiChannel) -> Option<String> {
    let doc = roxmltree::Document::parse(body).ok()?;
    let stem = channel.object_stem();
    let mut keys: Vec<&str> = doc
        .descendants()
        .filter(|n| n.tag_name().name() == "Key")
        .filter_map(|n| n.text())
        .filter(|k| {
            k.ends_with(".nc")
                && k.contains("_v3r0_blend_")
                && k.rsplit('/').next().is_some_and(|f| f.starts_with(stem))
        })
        .collect();
    // Lexicographic order is chronological here: every stamp in the name is
    // zero-padded and fixed width.
    keys.sort_unstable();
    keys.last().map(|k| (*k).to_string())
}

/// The newest available granule for `channel`, decoded.
pub async fn fetch_latest(
    client: &reqwest::Client,
    sources: &DataSources,
    channel: GmgsiChannel,
    now: NaiveDateTime,
) -> Result<GmgsiGrid, FetchError> {
    let mut last: Option<FetchError> = None;
    for hour in listing_attempts(now) {
        let key = match list_hour(client, sources, channel, hour).await {
            Ok(Some(key)) => key,
            Ok(None) => continue,
            Err(e) => {
                last = Some(e);
                continue;
            }
        };
        match fetch_key(client, sources, channel, &key).await {
            Ok(grid) => return Ok(grid),
            // The key came off a listing, so a failure here is a race with a
            // lifecycle rule rather than "not up yet": try the next hour.
            Err(e) => {
                last = Some(e);
                continue;
            }
        }
    }
    Err(last.unwrap_or_else(|| {
        FetchError::transient(format!(
            "no GMGSI {} granule in the last {LOOKBACK_HOURS} hours",
            channel.display_name()
        ))
    }))
}

/// **One granule, by the key a listing already found**, decoded.
///
/// The GET half of [`fetch_latest`], named so the frame path can reuse it: a
/// loop frame's key came off [`list_frame_keys`] and must not be re-listed to
/// be fetched.
pub async fn fetch_key(
    client: &reqwest::Client,
    sources: &DataSources,
    channel: GmgsiChannel,
    key: &str,
) -> Result<GmgsiGrid, FetchError> {
    let url = sources.s3_object_url(&sources.gmgsi_bucket, key);
    let resp = client.get(&url).send().await.map_err(|e| {
        FetchError::from_transport(&e, format!("GMGSI granule request failed: {e}"))
    })?;
    if !resp.status().is_success() {
        return Err(FetchError::from_status(
            resp.status(),
            NotFound::IsBroken,
            format!("GMGSI granule {key} returned HTTP {}", resp.status()),
        ));
    }
    let bytes = resp.bytes().await.map_err(|e| {
        FetchError::from_transport(&e, format!("GMGSI granule body read failed: {e}"))
    })?;
    super::decode::decode(bytes.into(), channel).map_err(FetchError::transient)
}

/// **The most hour prefixes one frame listing will ever request.**
///
/// A ceiling on request *count*, not on the window: past it the hours are
/// evenly sampled with both ends kept, so a wider window still spans the same
/// ground with fewer frames inside it, and the listing says `complete: false`.
///
/// Twenty-six is one more than the widest window the Lookback slider can name
/// — `ui_timeline` runs it to 1440 minutes, i.e. 24 hours, which touches 25
/// hour prefixes — so it does not bind on any window a user can ask for today.
/// It exists because a listing costs **1 LIST per hour** and nothing else in
/// the pipeline bounds that: [`DataSources::gmgsi_hour_prefix`] has no
/// `gmgsi_key` counterpart, so a key cannot be constructed and every frame's
/// object has to be found.
pub(crate) const MAX_FRAME_LIST_REQUESTS: usize = 26;

/// Hour prefixes listed at once. Each response is one XML document naming one
/// object, so this bounds sockets rather than bytes.
const FRAME_LIST_CONCURRENCY: usize = 6;

/// The top-of-hour instants `range` **reaches into**, at most
/// [`MAX_FRAME_LIST_REQUESTS`] of them, **endpoint-anchored** when there are
/// more hours than that.
///
/// **The two edges round the same way — down — and for opposite reasons.**
///
/// An hour `H`'s granule depicts `H`, and nothing depicts the minutes after
/// it: every instant in `H..H+1h` is drawn by carrying `H`'s granule forward.
/// So the hour a window *starts inside* is the newest granule at or before
/// `range.0` — the only picture the window's first partial hour can be drawn
/// from, and exactly what "the latest data at `range.0`" means. Rounding the
/// leading edge **up** left that hour unnamed, and a loop enabled at `HH:MM`
/// had `60 - MM` minutes of rail with no satellite granule behind it at all.
///
/// The trailing edge rounds **down** because the hour after `range.1` depicts
/// an instant later than anything in the window: no clock inside `range` can
/// ever stop on it, so listing it would buy a granule nothing can draw.
pub(crate) fn hours_in_range(range: (NaiveDateTime, NaiveDateTime)) -> Vec<NaiveDateTime> {
    let top = |t: NaiveDateTime| {
        t.with_minute(0)
            .and_then(|t| t.with_second(0))
            .and_then(|t| t.with_nanosecond(0))
            .unwrap_or(t)
    };
    let last = top(range.1);
    let first = top(range.0);
    if last < first {
        return Vec::new();
    }
    let n = (last - first).num_hours() as usize + 1;
    if n <= MAX_FRAME_LIST_REQUESTS {
        return (0..n)
            .map(|k| first + chrono::Duration::hours(k as i64))
            .collect();
    }
    // Both ends exact: index 0 and index `n - 1` are always produced, so the
    // ground the window covers is unchanged and only its resolution falls.
    let m = MAX_FRAME_LIST_REQUESTS;
    (0..m)
        .map(|k| first + chrono::Duration::hours((k * (n - 1) / (m - 1)) as i64))
        .collect()
}

/// **Every granule of `channel` inside `range`, as `(valid hour, object key)`,
/// with whether that is known to be all of them.**
///
/// The key is carried rather than the hour alone because there is nothing to
/// carry it back from: the object name ends in an unpredictable creation
/// stamp, so a frame fetched later would otherwise have to list its hour a
/// second time. **1 LIST per hour here, 1 GET per frame later.**
///
/// An hour with no object is an ordinary absence — the current hour has not
/// landed yet, or the feed stalled — and does not make the answer incomplete.
/// A listing that *errored* does, and so does a range wider than
/// [`MAX_FRAME_LIST_REQUESTS`] hours.
pub async fn list_frame_keys(
    client: &reqwest::Client,
    sources: &DataSources,
    channel: GmgsiChannel,
    range: (NaiveDateTime, NaiveDateTime),
) -> (Vec<(NaiveDateTime, String)>, bool) {
    use futures::StreamExt;

    let hours = hours_in_range(range);
    let asked = hours.len();
    let sampled = match (hours.first(), hours.last()) {
        (Some(first), Some(last)) => (*last - *first).num_hours() as usize + 1 != asked,
        _ => false,
    };
    let answers = futures::stream::iter(
        hours
            .into_iter()
            .map(|hour| async move { (hour, list_hour(client, sources, channel, hour).await) }),
    )
    .buffered(FRAME_LIST_CONCURRENCY)
    .collect::<Vec<_>>()
    .await;

    let mut keys: Vec<(NaiveDateTime, String)> = Vec::new();
    let mut every_hour_answered = true;
    for (hour, answer) in answers {
        match answer {
            Ok(Some(key)) => keys.push((hour, key)),
            Ok(None) => {}
            Err(e) => {
                every_hour_answered = false;
                log::warn!("GMGSI listing of {hour} failed: {e:?}");
            }
        }
    }
    keys.sort_by_key(|a| a.0);
    log::info!(
        "GMGSI {}: listed {asked} hours, found {} granules",
        channel.display_name(),
        keys.len(),
    );
    (keys, every_hour_answered && !sampled)
}

#[cfg(test)]
mod tests;
