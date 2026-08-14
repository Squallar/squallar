//! Level III products from the public `unidata-nexrad-level3` S3 bucket.
//!
//! Not TGFTP (`.../DS.{dir}/SI.{site}/sn.last`): it sends no
//! `Access-Control-Allow-Origin` and answers `403` to any request carrying an
//! `Origin:` header, so a browser cannot reach it.
//!
//! Bucket keys are **flat**, timestamp in the name, and there is no `sn.last`
//! alias:
//!
//! ```text
//! TLX_N0S_2026_07_25_17_30_24
//! ```
//!
//! So "the latest product" is: list the UTC day's prefix, take the last key.
//! Zero-padding makes the bucket's key order (UTF-8 binary) identical to
//! chronological order, so no timestamp parsing is needed to pick it.
//!
//! Site codes are three letters — the bucket keys on `TLX`, not `KTLX`; see
//! [`site_code`]. Products are named, not numbered: `DS.56rm0` is `N0S`,
//! `DS.176pr` is `DPR` (see [`crate::types::RadarProduct::level3_products`]).
//! The bytes are unchanged — objects carry the same
//! `SDUS54 KOUN 251723\r\r\nN0STLX\r\r\n` WMO envelope `sn.last` did, and
//! [`nexrad_level3::decode::decode_product`] strips it.
//!
//! Storm-relative velocity fetches nothing here any more. It once cost five
//! objects a volume — `N0S` for the vector in its PDB plus the dealiased
//! `N0G`/`N1G`/`N2U`/`N3U` as tilts, itself a workaround for `N1S`/`N2S`/
//! `N3S` having had nothing written since 2020 (NWS SCN 22-96) — and is now
//! derived from the Level II volume already in hand, every velocity tilt,
//! with a locally derived vector as the stand-in when none arrives. See
//! [`crate::srv`] for the derivation and [`crate::srm`] for the Level III
//! pipeline it replaced and the live harness that still measures it.

use chrono::{Duration, NaiveDate, NaiveDateTime};
use nexrad_level3::model::{Level3Message, ProductDescriptionBlock};

use crate::archive::{self, ArchiveError};
use crate::sources::DataSources;

/// Failures fetching a Level III product.
#[derive(Debug, thiserror::Error)]
pub enum Level3Error {
    #[error(transparent)]
    Bucket(#[from] ArchiveError),
    /// No matching product. Ordinary for a site that is down or does not
    /// generate one — not a failure to reach the bucket.
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
///
/// The rule is "drop the leading letter", *not* "strip a leading `K`" —
/// stripping only `K` would miss every non-CONUS radar (`PA*`, `PH*`, `TJUA`,
/// `PGUA`). Idempotent: a code already three characters is returned unchanged.
///
/// # Why the ASCII test
///
/// `id.len()` counts *bytes*. Four bytes is four characters only for an ASCII
/// identifier, and `&id[1..]` on four bytes whose first character is multi-byte
/// — `"éab"`, `"Ω12"` — lands inside a UTF-8 sequence and panics. `id` is not
/// always a value this build chose: the site is a field of the user's
/// `ui.json`, hand-editable, and the config load is the boundary that now
/// refuses one that is not [`crate::sites::is_ascii_site_id`]. This is the
/// same rule stated where the byte range is actually taken, because this is a
/// public function over a bare `&str` and callers outside that one path exist.
///
/// An identifier that fails the test is returned unchanged rather than
/// rejected, which is what every other length already got. It is not accepted
/// silently: it goes on to a bucket prefix that matches no object, and comes
/// back as [`Level3Error::NoProduct`] naming the value that was asked for.
pub fn site_code(id: &str) -> &str {
    if id.len() == 4 && crate::sites::is_ascii_site_id(id) {
        &id[1..]
    } else {
        id
    }
}

/// The latest key under one day's prefix, or `None` if the day has no objects.
///
/// Takes the maximum explicitly rather than the last element: S3 returns keys
/// in UTF-8 binary order, but a listing assembled from several pages is only
/// sorted if every page was.
fn newest(keys: Vec<String>) -> Option<String> {
    keys.into_iter().max()
}

/// Which object a Level III product came from, and when it was written.
///
/// The message alone cannot answer "how old is this?", and [`latest_key`] falls
/// back to the previous UTC day, so a site down since yesterday can paint a
/// field up to ~48 h old over a live basemap.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProductStamp {
    /// The bucket key, e.g. `TLX_N0S_2026_07_25_17_30_24`.
    pub key: String,
    /// From [`key_time`]. `None` only for a key whose tail does not parse,
    /// which no key the bucket currently serves produces; such a product is
    /// still worth drawing, just with an unknown age.
    pub time: Option<NaiveDateTime>,
}

impl ProductStamp {
    pub fn from_key(key: impl Into<String>) -> Self {
        let key = key.into();
        let time = key_time(&key);
        Self { key, time }
    }

    /// Age as of `now`, or `None` for an unreadable key. A key stamped in the
    /// future yields a negative duration rather than being clamped, so
    /// "impossible" stays distinguishable from "fresh".
    pub fn age(&self, now: NaiveDateTime) -> Option<Duration> {
        self.time.map(|t| now - t)
    }

    /// An unreadable timestamp is **not** stale — it is unknown. Callers that
    /// must tell the two apart should read [`Self::age`].
    pub fn is_stale(&self, now: NaiveDateTime, max: Duration) -> bool {
        self.age(now).is_some_and(|age| age > max)
    }
}

/// A decoded Level III product, with the identity of the object it came from.
#[derive(Debug, Clone)]
pub struct Level3Product {
    pub message: Level3Message,
    pub stamp: ProductStamp,
    /// The object's bytes, WMO/AWIPS envelope included — exactly what
    /// [`nexrad_level3::decode::decode_product`] was handed to produce
    /// [`message`](Self::message).
    ///
    /// Kept because a `Level3Message` has no wire form: its radial packets are
    /// run-length structures with no serde derives anywhere in the graph. The
    /// browser's rasterization worker is given these bytes and decodes them
    /// itself, which reuses that exact decoder rather than adding a second
    /// description of the product — and moves the decode off the main thread
    /// alongside the render.
    ///
    /// A product is a few hundred kilobytes against the ~10 MB volume beside
    /// it, and `Arc` so a render can borrow them without a copy.
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
    // The last six underscore-separated fields are Y M D H M S. Counted from
    // the end, so a site or product code containing an underscore is safe.
    let parts: Vec<&str> = key.split('_').collect();
    if parts.len() < 8 {
        return None;
    }
    let stamp = parts[parts.len() - 6..].join("_");
    NaiveDateTime::parse_from_str(&stamp, "%Y_%m_%d_%H_%M_%S").ok()
}

/// List the keys for one site/product/UTC day.
///
/// `pub(crate)` for [`crate::srm`]'s validation harness, which has to find the
/// key belonging to a *particular* volume rather than the newest one.
pub(crate) async fn list_day(
    sources: &DataSources,
    site3: &str,
    product: &str,
    date: &NaiveDate,
) -> Result<Vec<String>> {
    let client = archive::shared_client();
    let prefix = DataSources::level3_day_prefix(site3, product, date);
    log::debug!("Listing Level III objects for prefix {prefix:?}");
    // Paged, not single-shot: ~342 objects per product-day fits one 1000-key
    // page today, but the page size is S3's choice and a truncated listing
    // would drop the newest keys — the ones this exists to find.
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
///
/// The fallback keeps the overlay alive across 00Z, when the new day's prefix
/// is empty. It is consulted only when today's listing is *empty*: any key from
/// today is by construction newer than anything from yesterday.
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

/// Fetch and decode the latest Level III product for a site. `site` may be the
/// four-letter ICAO code the rest of the application uses; [`site_code`]
/// reduces it. `product` is an AWIPS ID such as `"N0S"`.
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
// Everything below pairs by **volume identity**, never by key recency. It is
// the one implementation: the validation twins and the SRM harness (branch
// `campaign-harness`) and the frontend's Level III loop all route through it,
// so the rule that makes it correct cannot hold in one copy and not another.

/// How many bucket objects to open looking for a particular volume, and how
/// far from the volume start to look.
///
/// The bucket's *newest* key will not do. SAILS republishes the lowest cut two
/// to four times a volume and the QPE family emits an intermediate per
/// SAILS/MRLE scan, so the newest key for a code is usually a mid-volume
/// repeat of some *other* volume than the one being paired. Taking it skipped
/// the wanted cut at two sites in three on the SRM harness's first run.
pub const PAIRING_CANDIDATES: usize = 10;
/// How far from the volume start a candidate key may be stamped. A product is
/// generated within seconds to a couple of minutes of the volume it describes;
/// twenty minutes is wide enough for a slow RPG and narrow enough that the
/// neighbouring volumes' objects are still ordered behind the right one.
pub const PAIRING_WINDOW_MINUTES: i64 = 20;
/// How far a decoded PDB's volume start may sit from the Level II volume start
/// and still be the same volume. The two are written by different subsystems
/// from the same clock, so this is slack, not a search radius.
pub const VOLUME_MATCH_TOLERANCE_SECS: i64 = 60;

/// The PDB's volume scan start as a timestamp. The modified Julian date's
/// **day 1 is 1970-01-01** — the same convention as the generation stamp.
///
/// This is the field that makes pairing possible at all: it is the Level II
/// volume start, so an object and the volume it was generated from name the
/// same instant however long the RPG took to publish it.
pub fn volume_scan_started(pdb: &ProductDescriptionBlock) -> Option<NaiveDateTime> {
    let days = u64::from(pdb.volume_scan_date).checked_sub(1)?;
    NaiveDate::from_ymd_opt(1970, 1, 1)?
        .checked_add_days(chrono::Days::new(days))?
        .and_hms_opt(0, 0, 0)?
        .checked_add_signed(Duration::seconds(i64::from(pdb.volume_scan_time)))
}

/// Whether a decoded product was generated from the volume that started at
/// `l2_volume_start`, within [`VOLUME_MATCH_TOLERANCE_SECS`].
///
/// A PDB with no readable volume stamp names no volume: `false`, never
/// "close enough".
pub fn names_volume(pdb: &ProductDescriptionBlock, l2_volume_start: NaiveDateTime) -> bool {
    volume_scan_started(pdb).is_some_and(|started| {
        (started - l2_volume_start).num_seconds().abs() <= VOLUME_MATCH_TOLERANCE_SECS
    })
}

/// Which object of a paired volume a product wants.
///
/// The distinction is real and per-product: most products emit once per
/// volume, but the QPE family emits an end-of-volume composite *plus* one
/// partial intermediate per SAILS/MRLE scan, all stamped with the same volume
/// start. Taking the nearest-to-start one there is taking a partial answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VolumePick {
    /// The candidate nearest the volume start that names the volume, and —
    /// when `cut` is given — that elevation number. What a once-per-volume
    /// product wants.
    Nearest {
        /// PDB `elevation_number` to require, for the per-tilt products.
        cut: Option<u8>,
    },
    /// The highest-keyed object naming the volume: the end-of-volume
    /// composite for the QPE family.
    Latest,
}

impl VolumePick {
    /// The whole-volume, no-cut-filter case — what every product rustdar
    /// fetches for display uses unless it is a QPE composite.
    pub const NEAREST: Self = Self::Nearest { cut: None };
}

/// The keys of `keys` stamped within [`PAIRING_WINDOW_MINUTES`] of `want`,
/// **nearest first**.
///
/// Pure, and the ordering is the whole point: the matching object is written
/// within seconds of its siblings, so the first candidate is almost always the
/// answer and [`product_from_candidates`] opens no more objects than it must.
/// Keys whose tail does not parse carry no time and are dropped — a key that
/// cannot be placed in the window cannot be ranked in it either.
pub fn candidates_near(keys: impl IntoIterator<Item = String>, want: NaiveDateTime) -> Vec<String> {
    let mut candidates: Vec<(i64, String)> = keys
        .into_iter()
        .filter_map(|k| {
            let t = key_time(&k)?;
            let delta = (t - want).num_seconds().abs();
            (delta <= PAIRING_WINDOW_MINUTES * 60).then_some((delta, k))
        })
        .collect();
    // Sorted by `(delta, key)`, so equidistant keys — a product published
    // before and after the volume start by the same margin — resolve the same
    // way every call rather than by listing order.
    candidates.sort();
    candidates
        .into_iter()
        .take(PAIRING_CANDIDATES)
        .map(|(_, k)| k)
        .collect()
}

/// Every key for one site/product across `days`, in one flat list.
///
/// Listing a day is a network round-trip per day, and a caller pairing many
/// volumes against one product — a loop — must not repeat it per volume. So
/// the listing and the pairing are separate: list once with this, then rank
/// per volume with [`candidates_near`].
///
/// A day that fails to list contributes nothing rather than failing the whole
/// call: with a two-day span the other day usually still answers, and a
/// product that genuinely cannot be found is reported as "no object for this
/// volume" — which is also what a real gap looks like.
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
///
/// A day *after* `want` is deliberately absent. The window is symmetric, so an
/// object generated just after a volume that started at 23:59 lands on the next
/// day — but a Level III key names its **generation** time and the pairing only
/// ever looks backwards for a volume that has already been published, so the
/// object of the last volume of a day is on that day or, at worst, seconds into
/// the next. Adding a third listing to cover those seconds would cost a
/// round-trip per pairing for every volume of every day.
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
///
/// `None` is ordinary: a site can simply not have generated the product for a
/// volume, which is what a gap in a loop looks like. It is deliberately not an
/// error — there is nothing to retry and nothing to report.
///
/// Never falls back to "the newest key" or "the nearest key regardless": every
/// returned object has had its PDB checked against the volume. See
/// [`PAIRING_CANDIDATES`].
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
///
/// One volume, one product code, one listing. A caller pairing many volumes
/// against one code should list once with [`list_days`] and then call
/// [`product_from_candidates`] per volume instead.
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
