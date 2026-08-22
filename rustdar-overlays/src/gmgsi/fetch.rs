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
async fn list_hour(
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
        let url = sources.s3_object_url(&sources.gmgsi_bucket, &key);
        let resp = client.get(&url).send().await.map_err(|e| {
            FetchError::from_transport(&e, format!("GMGSI granule request failed: {e}"))
        })?;
        if !resp.status().is_success() {
            // The key came off a listing, so a 404 here is a race with a
            // lifecycle rule rather than "not up yet": try the next hour.
            last = Some(FetchError::from_status(
                resp.status(),
                NotFound::IsBroken,
                format!("GMGSI granule {key} returned HTTP {}", resp.status()),
            ));
            continue;
        }
        let bytes = resp.bytes().await.map_err(|e| {
            FetchError::from_transport(&e, format!("GMGSI granule body read failed: {e}"))
        })?;
        return super::decode::decode(bytes.into(), channel).map_err(FetchError::transient);
    }
    Err(last.unwrap_or_else(|| {
        FetchError::transient(format!(
            "no GMGSI {} granule in the last {LOOKBACK_HOURS} hours",
            channel.display_name()
        ))
    }))
}

#[cfg(test)]
mod tests;
