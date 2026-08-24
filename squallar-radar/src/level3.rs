//! Level III products from the public `unidata-nexrad-level3` S3 bucket.
//!
//! Not TGFTP (`.../DS.{dir}/SI.{site}/sn.last`): it sends no
//! `Access-Control-Allow-Origin` and answers `403` to any request carrying an
//! `Origin:` header, so a browser cannot reach it.
//!
//! Bucket keys are flat with the timestamp in the name
//! (`TLX_N0S_2026_07_25_17_30_24`) and there is no `sn.last` alias, so "the
//! latest product" is: list the UTC day's prefix, take the last key.
//! Zero-padding makes the bucket's key order identical to chronological order.
//!
//! Site codes are three letters — the bucket keys on `TLX`, not `KTLX`; see
//! [`site_code`]. Products are named, not numbered: `DS.56rm0` is `N0S`,
//! `DS.176pr` is `DPR`. Objects carry a
//! `SDUS54 KOUN 251723\r\r\nN0STLX\r\r\n` WMO envelope, which
//! [`nexrad_level3::decode::decode_product`] strips.

use chrono::{Duration, NaiveDate, NaiveDateTime};
use nexrad_level3::model::{Level3Message, ProductDescriptionBlock};

use crate::archive::{self, ArchiveError};
use crate::sources::DataSources;

/// Failures fetching a Level III product.
#[derive(Debug, thiserror::Error)]
pub enum Level3Error {
    #[error(transparent)]
    Bucket(#[from] ArchiveError),
    /// No matching product.
    #[error("no {product} product for {site}")]
    NoProduct {
        /// Three-letter site code.
        site: String,
        /// AWIPS product ID.
        product: String,
    },
    #[error("decode error: {0}")]
    Decode(#[from] nexrad_level3::result::Error),
}

pub type Result<T> = std::result::Result<T, Level3Error>;

/// The three-letter site code the Level III bucket keys on: the last three of
/// the four-letter ICAO identifier (`KTLX` → `TLX`, `PHKI` → `HKI`).
pub fn site_code(id: &str) -> &str {
    if id.len() == 4 && crate::sites::is_ascii_site_id(id) {
        &id[1..]
    } else {
        id
    }
}

/// The latest key under one day's prefix, or `None` if the day has no objects.
fn newest(keys: Vec<String>) -> Option<String> {
    keys.into_iter().max()
}

/// Which object a Level III product came from, and when it was written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProductStamp {
    /// The bucket key, e.g. `TLX_N0S_2026_07_25_17_30_24`.
    pub key: String,
    /// From [`key_time`].
    pub time: Option<NaiveDateTime>,
}

impl ProductStamp {
    pub fn from_key(key: impl Into<String>) -> Self {
        let key = key.into();
        let time = key_time(&key);
        Self { key, time }
    }

    /// Age as of `now`, or `None` for an unreadable key.
    pub fn age(&self, now: NaiveDateTime) -> Option<Duration> {
        self.time.map(|t| now - t)
    }

    /// An unreadable timestamp is **not** stale — it is unknown.
    pub fn is_stale(&self, now: NaiveDateTime, max: Duration) -> bool {
        self.age(now).is_some_and(|age| age > max)
    }
}

/// A decoded Level III product, with the identity of the object it came from.
#[derive(Debug, Clone)]
pub struct Level3Product {
    pub message: Level3Message,
    pub stamp: ProductStamp,
    /// The object's bytes, WMO/AWIPS envelope included.
    pub bytes: std::sync::Arc<Vec<u8>>,
}

impl Level3Product {
    pub fn age(&self, now: NaiveDateTime) -> Option<Duration> {
        self.stamp.age(now)
    }
}

/// Timestamp encoded in a Level III key: `TLX_N0S_2026_07_25_17_30_24` →
/// 2026-07-25 17:30:24 UTC. For reporting only, never for choosing a key.
pub fn key_time(key: &str) -> Option<NaiveDateTime> {
    // The last six underscore-separated fields are Y M D H M S.
    let parts: Vec<&str> = key.split('_').collect();
    if parts.len() < 8 {
        return None;
    }
    let stamp = parts[parts.len() - 6..].join("_");
    NaiveDateTime::parse_from_str(&stamp, "%Y_%m_%d_%H_%M_%S").ok()
}

/// List the keys for one site/product/UTC day.
pub(crate) async fn list_day(
    sources: &DataSources,
    site3: &str,
    product: &str,
    date: &NaiveDate,
) -> Result<Vec<String>> {
    let client = archive::shared_client();
    let prefix = DataSources::level3_day_prefix(site3, product, date);
    log::debug!("Listing Level III objects for prefix {prefix:?}");
    // Paged, not single-shot: a truncated listing would drop the newest keys.
    let keys = archive::collect_keys(
        &sources.s3_bucket_url(&sources.level3_bucket),
        &prefix,
        None,
        |url| archive::get_text(client, url),
    )
    .await?;
    Ok(keys)
}

/// Find the newest key for a product, falling back to the previous UTC day.
pub async fn latest_key(
    sources: &DataSources,
    site3: &str,
    product: &str,
    today: &NaiveDate,
) -> Result<Option<String>> {
    if let Some(key) = newest(list_day(sources, site3, product, today).await?) {
        return Ok(Some(key));
    }
    let yesterday = *today - Duration::days(1);
    log::info!("No {product} for {site3} on {today}, falling back to {yesterday}");
    Ok(newest(list_day(sources, site3, product, &yesterday).await?))
}

/// Fetch and decode the latest Level III product for a site.
pub async fn fetch_latest_product(
    sources: &DataSources,
    site: &str,
    product: &str,
    now: NaiveDateTime,
) -> Result<Level3Product> {
    let site3 = site_code(site).to_uppercase();
    let date = now.date();

    let Some(key) = latest_key(sources, &site3, product, &date).await? else {
        return Err(Level3Error::NoProduct {
            site: site3,
            product: product.to_string(),
        });
    };

    let url = sources.level3_object_url(&key);
    log::info!("Fetching Level III {key}");
    let client = archive::shared_client();
    let bytes = archive::get_bytes(client, url).await?;

    // The object carries a WMO/AWIPS envelope; the decoder strips it.
    let message = nexrad_level3::decode::decode_product(&bytes)?;
    let stamp = ProductStamp::from_key(key);
    if let Some(age) = stamp.age(now) {
        log::info!(
            "Level III {} is {} minutes old",
            stamp.key,
            age.num_minutes()
        );
    }
    Ok(Level3Product {
        message,
        stamp,
        bytes: std::sync::Arc::new(bytes),
    })
}

// ── Pairing an object to a Level II volume ────────────────────────────────
//
// Everything below pairs by volume identity, never by key recency.

/// How many bucket objects to open looking for a particular volume, and how
/// far from the volume start to look.
pub const PAIRING_CANDIDATES: usize = 10;
/// How far from the volume start a candidate key may be stamped.
pub const PAIRING_WINDOW_MINUTES: i64 = 20;
/// How far a decoded PDB's volume start may sit from the Level II volume start and
/// still be the same volume.
pub const VOLUME_MATCH_TOLERANCE_SECS: i64 = 60;

/// The PDB's volume scan start as a timestamp.
pub fn volume_scan_started(pdb: &ProductDescriptionBlock) -> Option<NaiveDateTime> {
    let days = u64::from(pdb.volume_scan_date).checked_sub(1)?;
    NaiveDate::from_ymd_opt(1970, 1, 1)?
        .checked_add_days(chrono::Days::new(days))?
        .and_hms_opt(0, 0, 0)?
        .checked_add_signed(Duration::seconds(i64::from(pdb.volume_scan_time)))
}

/// Whether a decoded product was generated from the volume that started at
/// `l2_volume_start`, within [`VOLUME_MATCH_TOLERANCE_SECS`].
pub fn names_volume(pdb: &ProductDescriptionBlock, l2_volume_start: NaiveDateTime) -> bool {
    volume_scan_started(pdb).is_some_and(|started| {
        (started - l2_volume_start).num_seconds().abs() <= VOLUME_MATCH_TOLERANCE_SECS
    })
}

/// Which object of a paired volume a product wants.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VolumePick {
    /// The candidate nearest the volume start that names the volume, and — when
    /// `cut` is given — that elevation number.
    Nearest {
        /// PDB `elevation_number` to require, for the per-tilt products.
        cut: Option<u8>,
    },
    /// The highest-keyed object naming the volume: the end-of-volume
    /// composite for the QPE family.
    Latest,
}

impl VolumePick {
    /// The whole-volume, no-cut-filter case.
    pub const NEAREST: Self = Self::Nearest { cut: None };
}

/// The keys of `keys` stamped within [`PAIRING_WINDOW_MINUTES`] of `want`,
/// **nearest first**.
pub fn candidates_near(keys: impl IntoIterator<Item = String>, want: NaiveDateTime) -> Vec<String> {
    let mut candidates: Vec<(i64, String)> = keys
        .into_iter()
        .filter_map(|k| {
            let t = key_time(&k)?;
            let delta = (t - want).num_seconds().abs();
            (delta <= PAIRING_WINDOW_MINUTES * 60).then_some((delta, k))
        })
        .collect();
    // Sorted by `(delta, key)`, so equidistant keys resolve the same way every
    // call rather than by listing order.
    candidates.sort();
    candidates
        .into_iter()
        .take(PAIRING_CANDIDATES)
        .map(|(_, k)| k)
        .collect()
}

/// Every key for one site/product across `days`, in one flat list.
pub async fn list_days(
    sources: &DataSources,
    site: &str,
    product: &str,
    days: &[NaiveDate],
) -> Vec<String> {
    let site3 = site_code(site).to_uppercase();
    let mut keys = Vec::new();
    for day in days {
        match list_day(sources, &site3, product, day).await {
            Ok(k) => keys.extend(k),
            Err(e) => log::warn!("Listing {product} for {site3} on {day} failed: {e}"),
        }
    }
    keys
}

/// The UTC days a pairing window around `want` can touch: the day itself and
/// the one before, for windows spanning midnight.
pub fn pairing_days(want: NaiveDateTime) -> [NaiveDate; 2] {
    [want.date(), want.date() - Duration::days(1)]
}

/// The keys for `site`/`product` within the pairing window of `want`, nearest
/// first — [`list_days`] over [`pairing_days`] then [`candidates_near`].
pub async fn candidate_keys(
    sources: &DataSources,
    site: &str,
    product: &str,
    want: NaiveDateTime,
) -> Vec<String> {
    let days = pairing_days(want);
    candidates_near(list_days(sources, site, product, &days).await, want)
}

/// Open `candidates` in order and return the object generated from the volume
/// that started at `l2_volume_start`, or `None` if none of them was.
pub async fn product_from_candidates(
    sources: &DataSources,
    candidates: Vec<String>,
    l2_volume_start: NaiveDateTime,
    pick: VolumePick,
) -> Option<Level3Product> {
    let mut best: Option<Level3Product> = None;
    for key in candidates {
        let url = sources.level3_object_url(&key);
        let Ok(bytes) = archive::get_bytes(archive::shared_client(), url).await else {
            continue;
        };
        let Ok(message) = nexrad_level3::decode::decode_product(&bytes) else {
            continue;
        };
        if !names_volume(&message.pdb, l2_volume_start) {
            continue;
        }
        if let VolumePick::Nearest { cut: Some(cut) } = pick
            && message.pdb.elevation_number != u16::from(cut)
        {
            continue;
        }
        let product = Level3Product {
            message,
            stamp: ProductStamp::from_key(key),
            bytes: std::sync::Arc::new(bytes),
        };
        match pick {
            // The candidates are already nearest-first, so the first match is
            // the answer and the remaining objects need not be downloaded.
            VolumePick::Nearest { .. } => return Some(product),
            VolumePick::Latest => {
                if best
                    .as_ref()
                    .is_none_or(|b| product.stamp.key > b.stamp.key)
                {
                    best = Some(product);
                }
            }
        }
    }
    best
}

/// The Level III object generated **from** a given Level II volume: list the
/// pairing window's days, rank the candidates nearest the volume start, and
/// take the first (or highest-keyed, per `pick`) whose PDB names that volume.
pub async fn fetch_product_for_volume(
    sources: &DataSources,
    site: &str,
    product: &str,
    l2_volume_start: NaiveDateTime,
    pick: VolumePick,
) -> Option<Level3Product> {
    let candidates = candidate_keys(sources, site, product, l2_volume_start).await;
    product_from_candidates(sources, candidates, l2_volume_start, pick).await
}

// Native-only: the live checks at the tail are `#[tokio::test]`, and that
// dev-dependency is target-gated off wasm32.
#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests;
