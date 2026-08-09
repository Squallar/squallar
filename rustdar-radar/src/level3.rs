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
//! with a locally computed Bunkers right-mover as the default vector. See
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
pub fn site_code(id: &str) -> &str {
    if id.len() == 4 { &id[1..] } else { id }
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
    let keys = archive::collect_keys(&sources.level3_bucket, &prefix, None, |url| {
        archive::get_text(client, url)
    })
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
mod tests {
    use super::*;
    use crate::types::RadarProduct;

    /// AWIPS product ID and Level III message code per product, transcribed
    /// from NWS 2620001 (ICD for the RPG to Class 1 User).
    ///
    /// Keyed by `RadarProduct`, not by AWIPS ID, so it can *contradict*
    /// [`RadarProduct::level3_products`]: keyed by ID it could only ever agree,
    /// because `DVL` decodes as 134 no matter which product asked for it.
    ///
    /// | product | AWIPS | message code | field |
    /// |---|---|---|---|
    /// | Specific Differential Phase | `N0K` | 163 | Specific Differential Phase |
    /// | Echo Tops | `EET` | 135 | Enhanced Echo Tops |
    /// | Vertically Integrated Liquid | `DVL` | 134 | Digital Vertically Integrated Liquid |
    /// | VIL Density | `DVL`, `EET` | 134, 135 | the two above, divided |
    /// | Precipitation Rate | `DPR` | 176 | Digital Instantaneous Precipitation Rate |
    ///
    /// Hydrometeor Classification's row (`HHC` 177) left with the fetch —
    /// the product composites locally from Level II now ([`crate::hhc`]).
    ///
    /// The AWIPS IDs are listed **in request order**, so the table pins which
    /// tilt each one is, not merely that the set is right. Storm-relative
    /// velocity's five-entry row (`N0S` 56 vector-only, `N0G`/`N1G` 154,
    /// `N2U`/`N3U` 99) left with the fetch — the product derives from
    /// Level II now ([`crate::srv`]).
    ///
    /// VIL density is the one **derived** row: its two IDs are not two tilts of
    /// one field but the numerator and the denominator of
    /// `1000 · DVL / ((EET + 0.5) · 304.8)` ([`crate::vild`]), in that order —
    /// so the order pins which is which, and swapping them would divide
    /// kilofeet by kilograms. It is therefore also the one row whose IDs
    /// legitimately appear elsewhere in the table; see
    /// [`DERIVED_FROM_OTHER_ROWS`].
    const ICD: &[(RadarProduct, &[(&str, i16)])] = &[
        (RadarProduct::SpecificDifferentialPhase, &[("N0K", 163)]),
        (RadarProduct::EchoTops, &[("EET", 135)]),
        (RadarProduct::VerticallyIntegratedLiquid, &[("DVL", 134)]),
        (RadarProduct::VilDensity, &[("DVL", 134), ("EET", 135)]),
        (RadarProduct::PrecipitationRate, &[("DPR", 176)]),
    ];

    /// The products whose row is a **computation over other products' objects**
    /// rather than the tilts of a field of their own.
    ///
    /// This exists so the duplicate-ID check below stays as strict as it was
    /// for everything else. Its point was never "no ID may appear twice" for
    /// its own sake — it was that two products must not each claim to *be* the
    /// same field, because `DVL` under Echo Tops decodes cleanly and paints a
    /// 62 kg/m² VIL core labelled "Echo Tops: 62.0 kft". A derived product
    /// reusing its inputs' objects is a different thing, and admitting it by
    /// name — rather than by relaxing the check for everyone — keeps the
    /// original bug catchable.
    ///
    /// The obligation an entry takes on: its row must name **more than one** ID
    /// (a single-ID row that reuses another product's ID is exactly the
    /// copy-paste bug), and every ID it names must belong to some
    /// non-derived row, so a derived product cannot invent an input nothing
    /// fetches.
    const DERIVED_FROM_OTHER_ROWS: &[RadarProduct] = &[RadarProduct::VilDensity];

    fn icd_row(product: &RadarProduct) -> Option<&'static [(&'static str, i16)]> {
        ICD.iter().find(|(p, _)| p == product).map(|(_, ids)| *ids)
    }

    fn is_derived(product: &RadarProduct) -> bool {
        DERIVED_FROM_OTHER_ROWS.contains(product)
    }

    /// Every Level III product must request the AWIPS IDs the ICD gives for the
    /// field rustdar renders it as, in the order it gives them.
    ///
    /// Swapping two IDs neither crashes nor fails to decode: `DVL` under Echo
    /// Tops decodes cleanly in kg/m² and the Echo Tops palette then paints a
    /// 62 kg/m² VIL core labelled "Echo Tops: 62.0 kft". Shape-only checks
    /// (`every_level3_product_has_at_least_one_awips_code`) miss that.
    #[test]
    fn each_level3_product_requests_the_awips_id_the_icd_gives_it() {
        for product in RadarProduct::all() {
            let Some(row) = icd_row(product) else {
                assert!(
                    !product.is_level3(),
                    "{} is Level III but has no row in the ICD table; add one \
                     rather than leaving the mapping unpinned",
                    product.name(),
                );
                continue;
            };
            assert!(
                product.is_level3(),
                "{} has an ICD row but is not reported as Level III",
                product.name(),
            );
            let codes = product
                .level3_products()
                .unwrap_or_else(|| panic!("{} names no product code", product.name()));
            let want: Vec<&str> = row.iter().map(|(id, _)| *id).collect();
            assert_eq!(
                codes,
                want,
                "{} requests {codes:?}; the ICD gives {want:?} for that field",
                product.name(),
            );
        }
    }

    /// The table must cover every Level III product and nothing else, or the
    /// test above passes by skipping a product the table forgot.
    #[test]
    fn the_icd_table_covers_exactly_the_level3_products() {
        let level3: Vec<_> = RadarProduct::all()
            .iter()
            .filter(|p| p.is_level3())
            .collect();
        assert_eq!(
            ICD.len(),
            level3.len(),
            "the ICD table has {} rows for {} Level III products",
            ICD.len(),
            level3.len(),
        );
        // A duplicated ID would let two products agree with the table while
        // pointing at the same field — so among the rows that *are* a field,
        // every ID is still unique. The derived rows are checked separately,
        // and more strictly, below.
        let primary: Vec<&str> = ICD
            .iter()
            .filter(|(p, _)| !is_derived(p))
            .flat_map(|(_, ids)| ids.iter().map(|(id, _)| *id))
            .collect();
        for (i, code) in primary.iter().enumerate() {
            assert!(
                !primary[..i].contains(code),
                "{code} appears twice among the ICD table's own fields",
            );
        }

        // A derived row reuses its inputs' objects rather than naming a field
        // of its own. It owes two things: more than one input (a single-ID row
        // reusing another product's ID is the copy-paste bug the check above
        // exists for), and every input has to be an object some non-derived row
        // fetches, so nothing can invent an input.
        for product in DERIVED_FROM_OTHER_ROWS {
            let row = icd_row(product)
                .unwrap_or_else(|| panic!("{} is derived but has no row", product.name()));
            assert!(
                row.len() > 1,
                "{} is listed as derived from other rows but names only {row:?}",
                product.name(),
            );
            let mut seen: Vec<&str> = Vec::new();
            for (id, code) in row {
                assert!(
                    !seen.contains(id),
                    "{} names {id} twice — one object cannot be two inputs",
                    product.name(),
                );
                seen.push(id);
                assert!(
                    ICD.iter()
                        .any(|(p, ids)| !is_derived(p)
                            && ids.iter().any(|(i, c)| i == id && c == code)),
                    "{} takes {id} ({code}) as an input, but no product fetches that \
                     object as a field of its own",
                    product.name(),
                );
            }
        }
    }

    fn at(h: u32, m: u32, s: u32) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(2026, 7, 25)
            .unwrap()
            .and_hms_opt(h, m, s)
            .unwrap()
    }

    /// A fetched product must say how old it is: nothing downstream could
    /// otherwise tell a product two minutes old from one the previous-day
    /// fallback dug out of yesterday. Expected ages are subtracted by hand, not
    /// recomputed with `key_time`.
    #[test]
    fn a_product_stamp_reports_its_age_from_its_key() {
        let stamp = ProductStamp::from_key("TLX_N0S_2026_07_25_17_30_24");
        // 17:45:24 - 17:30:24
        assert_eq!(stamp.age(at(17, 45, 24)).map(|a| a.num_minutes()), Some(15));
        // 17:30:24 - 17:30:24
        assert_eq!(stamp.age(at(17, 30, 24)).map(|a| a.num_seconds()), Some(0));
        // Ahead of `now` reads negative rather than clamping to zero.
        assert_eq!(stamp.age(at(17, 29, 24)).map(|a| a.num_minutes()), Some(-1));
    }

    /// The previous-day fallback is the case this exists for: yesterday's key
    /// must read as old, not as fresh.
    #[test]
    fn a_stamp_from_the_previous_day_is_stale() {
        let now = at(0, 5, 0);
        // 00:05:00 today minus 23:58:48 yesterday = 6 minutes 12 seconds.
        let overnight = ProductStamp::from_key("TLX_N0S_2026_07_24_23_58_48");
        assert_eq!(overnight.age(now).map(|a| a.num_minutes()), Some(6));
        assert!(
            !overnight.is_stale(now, Duration::minutes(30)),
            "the ordinary 00Z rollover is not staleness",
        );

        // A site down since the previous morning: the fallback still finds a
        // key, and it is nearly a day and a half old.
        let dead = ProductStamp::from_key("TLX_N0S_2026_07_24_11_00_00");
        assert_eq!(dead.age(now).map(|a| a.num_hours()), Some(13));
        assert!(dead.is_stale(now, Duration::minutes(30)));
        assert!(dead.is_stale(now, Duration::hours(12)));
        assert!(!dead.is_stale(now, Duration::hours(14)));
    }

    /// An unreadable key has an *unknown* age, which is not the same as stale:
    /// `age` is `None` and `is_stale` is false.
    #[test]
    fn a_stamp_with_no_readable_time_reports_neither_age_nor_staleness() {
        let stamp = ProductStamp::from_key("garbage");
        assert_eq!(stamp.time, None);
        assert_eq!(stamp.age(at(12, 0, 0)), None);
        assert!(!stamp.is_stale(at(12, 0, 0), Duration::zero()));
        // The key survives, so the UI can still say *what* it drew.
        assert_eq!(stamp.key, "garbage");
    }

    /// The rule is "drop the leading letter", not "strip a leading K": every
    /// non-CONUS site breaks under the latter.
    #[test]
    fn a_site_code_loses_its_leading_letter_not_a_literal_k() {
        assert_eq!(site_code("KTLX"), "TLX");
        assert_eq!(site_code("KFWS"), "FWS");
        // Alaska, Hawaii, Puerto Rico, Guam — all `P*`/`T*`, none start with K.
        assert_eq!(site_code("PAHG"), "AHG");
        assert_eq!(site_code("PHKI"), "HKI");
        assert_eq!(site_code("TJUA"), "JUA");
        assert_eq!(site_code("PGUA"), "GUA");
    }

    /// Applying it twice must not eat a second character.
    #[test]
    fn shortening_a_site_code_is_idempotent() {
        assert_eq!(site_code(site_code("KTLX")), "TLX");
        assert_eq!(site_code("TLX"), "TLX");
    }

    /// The newest key is chosen by *value*, not position. The fixture is
    /// shuffled: a `newest` returning `keys.last()` fails here.
    #[test]
    fn the_newest_key_is_the_maximum_not_merely_the_last_returned() {
        let keys = vec![
            "TLX_N0S_2026_07_25_17_30_24".to_string(),
            "TLX_N0S_2026_07_25_00_02_19".to_string(),
            "TLX_N0S_2026_07_25_09_13_03".to_string(),
        ];
        assert_eq!(newest(keys).as_deref(), Some("TLX_N0S_2026_07_25_17_30_24"),);
    }

    /// Zero padding is what makes binary order equal chronological order.
    /// An hour of `9` rather than `09` would sort after `17`.
    #[test]
    fn zero_padded_hours_keep_binary_order_equal_to_time_order() {
        let mut keys = [
            "TLX_N0S_2026_07_25_09_13_03".to_string(),
            "TLX_N0S_2026_07_25_17_30_24".to_string(),
        ];
        keys.sort();
        assert_eq!(keys.last().unwrap(), "TLX_N0S_2026_07_25_17_30_24");
        assert!(key_time(&keys[0]).unwrap() < key_time(&keys[1]).unwrap());
    }

    #[test]
    fn an_empty_listing_has_no_newest_key() {
        assert_eq!(newest(Vec::new()), None);
    }

    /// The timestamp comes off the tail of the key, so a product code with
    /// digits or a site code of another length cannot shift it.
    #[test]
    fn a_key_timestamp_is_read_from_the_last_six_fields() {
        assert_eq!(
            key_time("TLX_N0S_2026_07_25_17_30_24"),
            Some(
                NaiveDate::from_ymd_opt(2026, 7, 25)
                    .unwrap()
                    .and_hms_opt(17, 30, 24)
                    .unwrap()
            ),
        );
        assert_eq!(
            key_time("FWS_DPR_2026_01_05_00_02_19"),
            Some(
                NaiveDate::from_ymd_opt(2026, 1, 5)
                    .unwrap()
                    .and_hms_opt(0, 2, 19)
                    .unwrap()
            ),
        );
        assert_eq!(key_time("TLX_N0S_2026_07_25"), None, "no time fields");
        assert_eq!(key_time("garbage"), None);
    }

    /// The prefix must pin the day, or the listing returns the whole archive
    /// for that site/product and "newest" becomes "newest ever recorded".
    #[test]
    fn the_day_prefix_constrains_the_listing_to_one_utc_day() {
        let d = NaiveDate::from_ymd_opt(2026, 7, 25).unwrap();
        let prefix = DataSources::level3_day_prefix("TLX", "N0S", &d);
        assert_eq!(prefix, "TLX_N0S_2026_07_25");
        // A key from a different day must not match the prefix.
        assert!(!"TLX_N0S_2026_07_24_23_58_48".starts_with(&prefix));
        assert!("TLX_N0S_2026_07_25_17_30_24".starts_with(&prefix));
    }

    /// Catches a product added to `RadarProduct::is_level3` without a code,
    /// which would silently render an always-empty layer.
    #[test]
    fn every_level3_product_has_at_least_one_awips_code() {
        for product in RadarProduct::all() {
            let codes = product.level3_products();
            if product.is_level3() {
                let codes = codes.unwrap_or_else(|| {
                    panic!("{} is Level III but names no product code", product.name())
                });
                assert!(
                    !codes.is_empty(),
                    "{} names an empty code list",
                    product.name()
                );
                for code in codes {
                    assert_eq!(code.len(), 3, "{code} is not a 3-character AWIPS ID");
                    assert!(
                        code.chars()
                            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit()),
                        "{code} is not an AWIPS ID",
                    );
                }
            } else {
                assert!(
                    codes.is_none(),
                    "{} is a Level II moment but names Level III codes",
                    product.name(),
                );
            }
        }
    }

    /// Storm-relative velocity fetches nothing from Level III at all: it is
    /// derived from the Level II volume ([`crate::srv`]). Asserted directly,
    /// because putting the five-object fetch back — or just "N1S", dead
    /// since 2020 (NWS SCN 22-96) — is the obvious regression when someone
    /// notices the tilts are computed rather than fetched.
    #[test]
    fn storm_relative_velocity_requests_no_level3_product() {
        assert!(!RadarProduct::StormRelativeVelocity.is_level3());
        assert_eq!(RadarProduct::StormRelativeVelocity.level3_products(), None);
        assert_eq!(
            RadarProduct::StormRelativeVelocity.moment_slot(),
            Some(crate::types::MomentSlot::Velocity),
            "every velocity tilt lists, where the fetch offered four",
        );
        assert!(
            icd_row(&RadarProduct::StormRelativeVelocity).is_none(),
            "no ICD row either — the table covers exactly what is fetched",
        );
    }

    // ── Pairing ───────────────────────────────────────────────────────────

    /// A PDB carrying only the fields the pairing reads.
    fn pdb_for_volume(date: u16, time: u32, elevation_number: u16) -> ProductDescriptionBlock {
        ProductDescriptionBlock {
            block_divider: -1,
            latitude: 41.320,
            longitude: -96.367,
            height: 1148,
            product_code: 135,
            operational_mode: 2,
            vcp: 212,
            sequence_number: 0,
            volume_scan_number: 1,
            volume_scan_date: date,
            volume_scan_time: time,
            generation_date: date,
            generation_time: time + 90,
            product_specific_1: 0,
            product_specific_2: 0,
            elevation_number,
            product_specific_3: 0,
            thresholds: [0; 16],
            product_specific_47_53: [0; 7],
            version: 0,
            spot_blank: 0,
            symbology_offset: 60,
            graphic_offset: 0,
            tabular_offset: 0,
        }
    }

    /// The volume stamp conversion: day 1 is 1970-01-01, so MJD 20661 at 7108 s
    /// is 2026-07-26 01:58:28 — checked against a calendar, not against the
    /// function.
    #[test]
    fn the_volume_stamp_reads_day_one_as_the_epoch() {
        let t = volume_scan_started(&pdb_for_volume(20661, 7108, 0)).expect("a valid stamp");
        assert_eq!(t.to_string(), "2026-07-26 01:58:28");
        assert_eq!(
            volume_scan_started(&pdb_for_volume(1, 0, 0)).map(|t| t.to_string()),
            Some("1970-01-01 00:00:00".to_string()),
            "day 1 is the epoch itself",
        );
        // Day 0 cannot precede the epoch: it is None, not 1969-12-31.
        assert!(volume_scan_started(&pdb_for_volume(0, 0, 0)).is_none());
    }

    /// The volume test is a tolerance, not a search: a minute either way is the
    /// same volume, more is not — and a PDB with no readable stamp names no
    /// volume at all rather than passing on a plausible default.
    #[test]
    fn a_pdb_names_the_volume_it_started_within_a_minute_of() {
        // MJD 20661 @ 7108 s = 2026-07-26 01:58:28.
        let started = volume_scan_started(&pdb_for_volume(20661, 7108, 0)).expect("valid");
        let pdb = pdb_for_volume(20661, 7108, 0);

        assert!(names_volume(&pdb, started));
        assert!(names_volume(&pdb, started + Duration::seconds(60)));
        assert!(names_volume(&pdb, started - Duration::seconds(60)));
        assert!(
            !names_volume(&pdb, started + Duration::seconds(61)),
            "past the tolerance is another volume",
        );
        assert!(!names_volume(&pdb, started - Duration::seconds(61)));
        // A four-minute volume spacing is comfortably outside it, which is what
        // makes the tolerance safe against pairing the neighbour.
        assert!(!names_volume(&pdb, started + Duration::minutes(4)));

        assert!(
            !names_volume(&pdb_for_volume(0, 0, 0), started),
            "an unreadable volume stamp names nothing",
        );
    }

    fn key_at(product: &str, hms: (u32, u32, u32)) -> String {
        format!(
            "TLX_{product}_2026_07_25_{:02}_{:02}_{:02}",
            hms.0, hms.1, hms.2
        )
    }

    fn want_at(h: u32, m: u32, s: u32) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(2026, 7, 25)
            .unwrap()
            .and_hms_opt(h, m, s)
            .unwrap()
    }

    /// The rule the whole pairing rests on: candidates are ranked by distance
    /// from the *volume start*, never by recency. The fixture puts the newest
    /// key furthest away, so a selection that took `keys.max()` — which is what
    /// the display path's `latest_key` does — would rank it first.
    #[test]
    fn candidates_are_ranked_by_distance_from_the_volume_not_by_recency() {
        let keys = vec![
            key_at("EET", (17, 45, 10)), // newest, 15 min out
            key_at("EET", (17, 31, 30)), // 90 s out
            key_at("EET", (17, 28, 40)), // 80 s out — nearest
            key_at("EET", (17, 36, 0)),  // 6 min out
        ];
        let ranked = candidates_near(keys, want_at(17, 30, 0));
        assert_eq!(
            ranked,
            vec![
                key_at("EET", (17, 28, 40)),
                key_at("EET", (17, 31, 30)),
                key_at("EET", (17, 36, 0)),
                key_at("EET", (17, 45, 10)),
            ],
            "the nearest object is the one written just after the volume; the \
             newest key is a later volume's",
        );
    }

    /// Keys outside the window are not candidates at all, and an unparseable
    /// key cannot be ranked so it is dropped rather than sorted to the front.
    #[test]
    fn the_pairing_window_and_unreadable_keys_bound_the_candidate_set() {
        let inside = key_at("EET", (17, 49, 0)); // 19 min out
        let outside = key_at("EET", (17, 51, 0)); // 21 min out
        let ranked = candidates_near(
            vec![
                "garbage".to_string(),
                outside.clone(),
                inside.clone(),
                "TLX_EET_2026_07_25".to_string(),
            ],
            want_at(17, 30, 0),
        );
        assert_eq!(ranked, vec![inside], "only the in-window readable key");
        assert!(!ranked.contains(&outside));
    }

    /// A busy product-day serves hundreds of objects; the candidate list is
    /// capped so a pairing opens a bounded number of objects, and the cap keeps
    /// the *nearest* ones.
    #[test]
    fn the_candidate_list_is_capped_at_the_nearest_objects() {
        // One key every 10 s for 5 minutes either side of the volume: 60 keys,
        // all inside the window.
        let mut keys = Vec::new();
        for offset in -30i64..=30 {
            let t = want_at(17, 30, 0) + Duration::seconds(offset * 10);
            keys.push(format!("TLX_EET_{}", t.format("%Y_%m_%d_%H_%M_%S")));
        }
        let ranked = candidates_near(keys, want_at(17, 30, 0));
        assert_eq!(ranked.len(), PAIRING_CANDIDATES);
        // The nearest is the exact hit; nothing further than 50 s survives the
        // cap (the exact hit plus five pairs either side).
        assert_eq!(ranked[0], key_at("EET", (17, 30, 0)));
        for key in &ranked {
            let delta = (key_time(key).expect("built from a timestamp") - want_at(17, 30, 0))
                .num_seconds()
                .abs();
            assert!(delta <= 50, "{key} is {delta} s out but survived the cap");
        }
    }

    /// Equidistant candidates resolve the same way every call: the tie breaks on
    /// the key, so a listing assembled in another order pairs identically.
    #[test]
    fn equidistant_candidates_break_their_tie_deterministically() {
        let before = key_at("EET", (17, 29, 0));
        let after = key_at("EET", (17, 31, 0));
        let want = want_at(17, 30, 0);
        assert_eq!(
            candidates_near(vec![after.clone(), before.clone()], want),
            candidates_near(vec![before.clone(), after.clone()], want),
        );
        assert_eq!(
            candidates_near(vec![after, before.clone()], want)[0],
            before,
            "the earlier key wins, since the tie breaks on the key itself",
        );
    }

    /// Midnight: a volume just after 00Z is paired against objects that can
    /// still be under yesterday's prefix, so both days are listed.
    #[test]
    fn the_pairing_days_cover_the_previous_utc_day() {
        let just_after_midnight = NaiveDate::from_ymd_opt(2026, 7, 25)
            .unwrap()
            .and_hms_opt(0, 3, 0)
            .unwrap();
        assert_eq!(
            pairing_days(just_after_midnight),
            [
                NaiveDate::from_ymd_opt(2026, 7, 25).unwrap(),
                NaiveDate::from_ymd_opt(2026, 7, 24).unwrap(),
            ],
        );
        // And an in-window key from yesterday really does rank.
        let overnight = "TLX_EET_2026_07_24_23_59_10".to_string();
        assert_eq!(
            candidates_near(vec![overnight.clone()], just_after_midnight),
            vec![overnight],
        );
    }

    /// The per-product pick, which is what keeps a QPE loop from animating
    /// partial accumulations. Asserted for every product so a new Level III
    /// product cannot arrive without a decision.
    #[test]
    fn only_the_qpe_product_takes_the_volumes_last_object() {
        use crate::types::RadarProduct;
        assert_eq!(
            RadarProduct::PrecipitationRate.level3_volume_pick(),
            Some(VolumePick::Latest),
            "DPR emits a partial intermediate per SAILS cut; the composite is last",
        );
        for product in RadarProduct::all() {
            match product.level3_volume_pick() {
                None => assert!(
                    !product.is_level3(),
                    "{} is Level III but names no volume pick",
                    product.name(),
                ),
                Some(pick) => {
                    assert!(product.is_level3());
                    if *product != RadarProduct::PrecipitationRate {
                        assert_eq!(
                            pick,
                            VolumePick::NEAREST,
                            "{} publishes once per volume",
                            product.name(),
                        );
                    }
                }
            }
        }
    }

    /// VIL density is a Level III product with **two** inputs, in numerator
    /// then denominator order, and no Level II moment behind it.
    ///
    /// Asserted directly because every part of it is a plausible regression:
    /// the product used to be `compute_vil / compute_eet` off the Level II
    /// volume and listed on the reflectivity moment, and it was retired for
    /// measured muteness at the thresholds it is read for (see
    /// [`crate::vil`]'s validation section). A `moment_slot` here again would
    /// put the local quotient back on screen; a one-ID row would render a
    /// kg/m² field through the g/m³ palette.
    #[test]
    fn vil_density_requests_the_two_products_it_divides() {
        assert!(RadarProduct::VilDensity.is_level3());
        assert_eq!(
            RadarProduct::VilDensity.level3_products(),
            Some(&["DVL", "EET"][..]),
            "DVL is the numerator and EET the denominator — the order is the ratio",
        );
        assert_eq!(
            RadarProduct::VilDensity.moment_slot(),
            None,
            "no Level II moment stands behind it any more",
        );
        assert_eq!(
            icd_row(&RadarProduct::VilDensity),
            Some(&[("DVL", 134i16), ("EET", 135)][..]),
        );
        assert!(is_derived(&RadarProduct::VilDensity));

        // Its inputs are the objects the two single-field products fetch, so
        // nothing new is downloaded on its account.
        assert_eq!(
            RadarProduct::VerticallyIntegratedLiquid.level3_products(),
            Some(&["DVL"][..]),
        );
        assert_eq!(RadarProduct::EchoTops.level3_products(), Some(&["EET"][..]));
    }

    /// One poll fetches one object per **code**, not one per (product, code).
    ///
    /// The three products that read `DVL` and `EET` between them name those two
    /// codes four times over, and before this the fetch loop walked the table
    /// product by product and asked the bucket for each of the four — two extra
    /// ~100 KB GETs per site per poll, for bytes already in hand.
    #[test]
    fn one_poll_asks_for_each_object_once() {
        assert_eq!(
            RadarProduct::level3_codes_for(&[
                RadarProduct::VerticallyIntegratedLiquid,
                RadarProduct::EchoTops,
                RadarProduct::VilDensity,
            ]),
            ["DVL", "EET"],
            "VIL, echo tops and VIL density need two objects between them, not four",
        );

        // The whole roster, which is what a real poll asks for: one entry per
        // distinct code, and every code some product names is in it.
        let codes = RadarProduct::level3_codes_for(RadarProduct::all());
        assert_eq!(codes, ["DPR", "DVL", "EET", "N0K"]);
        for product in RadarProduct::all() {
            for code in product.level3_products().unwrap_or(&[]) {
                assert!(
                    codes.contains(code),
                    "{} names {code}, which no poll would fetch",
                    product.name(),
                );
            }
        }
        assert!(
            RadarProduct::level3_codes_for(&[RadarProduct::Reflectivity]).is_empty(),
            "a Level II product needs no bucket object",
        );
    }

    /// `level3_readers` is the exact inverse of `level3_products`.
    ///
    /// It is what a landed object is dispatched by — which panes to redraw, which
    /// entries the product picker gains — so a code whose readers it under-reports
    /// is a product that silently never notices its data arriving.
    #[test]
    fn every_code_reports_exactly_the_products_that_read_it() {
        assert_eq!(
            RadarProduct::level3_readers("DVL"),
            [
                RadarProduct::VerticallyIntegratedLiquid,
                RadarProduct::VilDensity
            ],
            "DVL is VIL's whole field and VIL density's numerator",
        );
        assert_eq!(
            RadarProduct::level3_readers("EET"),
            [RadarProduct::EchoTops, RadarProduct::VilDensity],
            "EET is echo tops' field and VIL density's denominator",
        );
        assert_eq!(
            RadarProduct::level3_readers("DPR"),
            [RadarProduct::PrecipitationRate],
        );
        assert!(
            RadarProduct::level3_readers("N0G").is_empty(),
            "a code nothing names has no readers — SRM's old tilts are gone",
        );

        // Round-trip: every product is a reader of every code it names, and of
        // nothing else.
        for product in RadarProduct::all() {
            for code in RadarProduct::level3_codes_for(RadarProduct::all()) {
                let names_it = product
                    .level3_products()
                    .is_some_and(|codes| codes.contains(&code));
                assert_eq!(
                    RadarProduct::level3_readers(code).contains(product),
                    names_it,
                    "{} and {code} disagree about whether it is a reader",
                    product.name(),
                );
            }
        }
    }

    /// Products sharing an AWIPS code must agree on which object of a volume to
    /// take.
    ///
    /// One object is cached per `(code, site)` and handed to every product that
    /// reads it, so a `Latest` reader and a `Nearest` reader of one code would
    /// take turns replacing the entry with the other's choice — and each would
    /// draw the other's object roughly half the time, with nothing in the image
    /// to say so. Today `DPR` (the only `Latest`) is named by one product and the
    /// shared codes `DVL`/`EET` are `Nearest` throughout; this fails the moment
    /// that stops being true, which is the point at which the loop's per-code
    /// cache needs a pick in its key.
    #[test]
    fn every_shared_level3_code_agrees_on_its_volume_pick() {
        for code in RadarProduct::level3_codes_for(RadarProduct::all()) {
            let readers = RadarProduct::level3_readers(code);
            let picks: Vec<_> = readers
                .iter()
                .map(|p| {
                    (
                        p.name(),
                        p.level3_volume_pick().unwrap_or_else(|| {
                            panic!("{} reads {code} but is not Level III", p.name())
                        }),
                    )
                })
                .collect();
            let (_, first) = picks[0];
            assert!(
                picks.iter().all(|(_, pick)| *pick == first),
                "{code} is read with conflicting volume picks: {picks:?}; the \
                 per-code object cache can only hold one of them",
            );
        }
    }

    // ── Live checks ───────────────────────────────────────────────────────
    //
    // Run with:
    //   cargo test -p rustdar-radar --lib -- --ignored --nocapture level3

    /// Every product rustdar asks for genuinely fetches and decodes, against
    /// the live bucket and the production mapping.
    ///
    /// The decoded message code is checked against [`ICD`] keyed by **product**,
    /// which is what makes a wrong product→ID mapping fail: swap `EET` and
    /// `DVL` in `level3_products` and this downloads Digital VIL for Echo Tops
    /// and sees 134 where the ICD says 135.
    #[ignore = "hits the live unidata-nexrad-level3 S3 bucket"]
    #[tokio::test]
    async fn live_every_requested_product_fetches_and_decodes() {
        let sources = DataSources::production();
        let now = chrono::Utc::now().naive_utc();

        for product in RadarProduct::all().iter().filter(|p| p.is_level3()) {
            let row =
                icd_row(product).unwrap_or_else(|| panic!("{} has no ICD row", product.name()));

            for &(code, want_message_code) in row {
                let fetched = fetch_latest_product(&sources, "KTLX", code, now)
                    .await
                    .unwrap_or_else(|e| panic!("{} fetch of {code} failed: {e}", product.name()));
                let msg = &fetched.message;
                let got = msg.header.message_code;
                println!(
                    "{} -> {code}: message_code={got}, product_code={}, symbology={}, \
                     key={}, age={:?} min",
                    product.name(),
                    msg.pdb.product_code,
                    msg.symbology.is_some(),
                    fetched.stamp.key,
                    fetched.age(now).map(|a| a.num_minutes()),
                );
                // The timestamp must survive the fetch, not just the key
                // parser: it is what marks a product from yesterday's fallback.
                assert!(
                    fetched.stamp.time.is_some(),
                    "{code} arrived as {} with no readable timestamp",
                    fetched.stamp.key,
                );
                assert_eq!(
                    got,
                    want_message_code,
                    "{} fetches {code}, which decoded as message code {got}; \
                     the ICD gives {want_message_code} for {}",
                    product.name(),
                    product.name(),
                );
                assert!(
                    msg.symbology.is_some(),
                    "{code} decoded with no symbology block — the object arrived \
                     but carries no display data",
                );
            }
        }
    }

    /// Proves the shortening is the one the *bucket* wants, not just that the
    /// string transform works: "KTLX" straight through lists nothing, which is
    /// indistinguishable from "site is down" without this.
    #[ignore = "hits the live unidata-nexrad-level3 S3 bucket"]
    #[tokio::test]
    async fn live_a_four_letter_icao_site_resolves_to_bucket_keys() {
        let sources = DataSources::production();
        let today = chrono::Utc::now().naive_utc().date();

        let three = latest_key(&sources, "TLX", "N0S", &today)
            .await
            .expect("listing must succeed");
        assert!(three.is_some(), "TLX must have N0S objects today");

        // The un-shortened form must find nothing.
        let four = latest_key(&sources, "KTLX", "N0S", &today)
            .await
            .expect("listing must succeed");
        assert_eq!(four, None, "the bucket does not key on 4-letter codes");
    }

    /// The three dropped SRM tilts really are gone, asserted against the live
    /// bucket rather than rustdar's own table. Starts failing if NWS restores
    /// them.
    #[ignore = "hits the live unidata-nexrad-level3 S3 bucket"]
    #[tokio::test]
    async fn live_the_dropped_srm_tilts_have_no_current_data() {
        let sources = DataSources::production();
        let today = chrono::Utc::now().naive_utc().date();

        // Control: the tilt that survives must be present, so an outage or a
        // wrong prefix cannot make the assertion below pass vacuously.
        let n0s = latest_key(&sources, "TLX", "N0S", &today)
            .await
            .expect("listing must succeed");
        assert!(
            n0s.is_some(),
            "N0S is missing too — the bucket or the prefix is wrong, so this \
             test cannot say anything about N1S/N2S/N3S",
        );

        for dead in ["N1S", "N2S", "N3S"] {
            let key = latest_key(&sources, "TLX", dead, &today)
                .await
                .expect("listing must succeed");
            assert_eq!(
                key, None,
                "{dead} has data again — NWS may have restored the higher SRM \
                 tilts; re-add it to RadarProduct::level3_products",
            );
        }
    }

    /// A real Level III loop, end to end: take the last hour of a site's Level II
    /// volumes — which is exactly the frame timeline the frontend's loop builds —
    /// list the bucket once, and pair every frame.
    ///
    /// What it proves that the unit tests cannot: N frames yield N *decoded* objects
    /// from N *distinct* volumes, each naming its own frame's volume start. A
    /// pairing that fell back to the newest key would return the same object for
    /// every frame and still "succeed" — the distinctness assertions are what catch
    /// that, and they are the reason SAILS republication makes recency the wrong
    /// rule.
    ///
    /// The listing is done once for the whole loop, as the frontend does it, so this
    /// also measures the real request cost: one listing plus one object per frame.
    ///
    /// Tries several sites: any one of them can be down or between volumes.
    ///
    /// ```text
    /// cargo test -p rustdar-radar --release -- --ignored --nocapture live_a_loops_frames
    /// ```
    #[ignore = "hits the live unidata-nexrad-level3 S3 bucket and the Level II archive"]
    #[tokio::test]
    async fn live_a_loops_frames_each_pair_with_their_own_volume() {
        crate::tls::init();
        let sources = DataSources::production();
        let now = chrono::Utc::now().naive_utc();
        // Long enough for several volumes at any VCP, short enough to stay inside
        // one product-day listing most of the time.
        let start = now - Duration::hours(1);

        // Echo tops: one AWIPS code, published once per volume.
        let product = RadarProduct::EchoTops;
        let code = product.level3_products().expect("EET")[0];
        let pick = product
            .level3_volume_pick()
            .expect("a Level III product names a pick");

        for site in ["KTLX", "KOUN", "KFWS", "KMPX", "KOAX", "KLZK"] {
            let Ok(volumes) = crate::scan::list_scans_for_range(site, start, now).await else {
                continue;
            };
            // The frontend caps a loop's frames; five is enough to make
            // distinctness mean something without a long test.
            let frames: Vec<NaiveDateTime> = volumes
                .iter()
                .rev()
                .take(5)
                .map(|(t, _)| *t)
                .rev()
                .collect();
            if frames.len() < 3 {
                println!("{site}: only {} volumes in the window", frames.len());
                continue;
            }

            // One listing for the whole loop, exactly as the loop does it.
            let days: Vec<NaiveDate> = {
                let mut days = Vec::new();
                for f in &frames {
                    for d in pairing_days(*f) {
                        if !days.contains(&d) {
                            days.push(d);
                        }
                    }
                }
                days
            };
            let keys = list_days(&sources, site, code, &days).await;
            println!(
                "{site}: {} frames, {} {code} keys across {} day(s)",
                frames.len(),
                keys.len(),
                days.len(),
            );
            if keys.is_empty() {
                continue;
            }

            let mut paired = Vec::new();
            for frame in &frames {
                let candidates = candidates_near(keys.iter().cloned(), *frame);
                let Some(object) =
                    product_from_candidates(&sources, candidates, *frame, pick).await
                else {
                    println!("  {frame}: gap — no {code} for that volume");
                    continue;
                };
                let pdb_start =
                    volume_scan_started(&object.message.pdb).expect("a decoded PDB is stamped");
                println!(
                    "  {frame}: {} (volume {pdb_start}, elevation {:.1}\u{b0})",
                    object.stamp.key,
                    object.message.pdb.elevation_angle(),
                );
                assert!(
                    names_volume(&object.message.pdb, *frame),
                    "{site}: {} was paired to frame {frame} but its PDB names {pdb_start}",
                    object.stamp.key,
                );
                assert!(
                    object.message.symbology.is_some(),
                    "{site}: {} decoded with no symbology, so the frame would draw nothing",
                    object.stamp.key,
                );
                paired.push((*frame, object));
            }

            // A gap or two is ordinary; every frame resolving to nothing is not.
            assert!(
                paired.len() >= 3,
                "{site}: only {} of {} frames paired — the loop would be mostly gaps",
                paired.len(),
                frames.len(),
            );
            // The assertion the pairing exists for. Taking the newest key would
            // give one object repeated, which every check above still passes for
            // the one frame nearest it.
            for (i, (_, a)) in paired.iter().enumerate() {
                for (_, b) in &paired[..i] {
                    assert_ne!(
                        a.stamp.key, b.stamp.key,
                        "{site}: two frames paired to the same object, so the loop \
                         would animate one image",
                    );
                    assert_ne!(
                        volume_scan_started(&a.message.pdb),
                        volume_scan_started(&b.message.pdb),
                        "{site}: two frames paired to objects of the same volume",
                    );
                }
            }
            println!(
                "{site}: {} frames -> {} distinct volumes",
                frames.len(),
                paired.len()
            );
            return;
        }
        panic!("no site served an hour of volumes with EET objects");
    }

    /// Pins "last key wins" against the live bucket: keys returned in another
    /// order, or a selection taking the first key, would yield something hours
    /// old rather than minutes.
    #[ignore = "hits the live unidata-nexrad-level3 S3 bucket"]
    #[tokio::test]
    async fn live_the_selected_key_is_the_freshest_one() {
        let sources = DataSources::production();
        let now = chrono::Utc::now().naive_utc();

        let key = latest_key(&sources, "TLX", "N0S", &now.date())
            .await
            .expect("listing must succeed")
            .expect("TLX must have N0S objects today");
        let stamp = key_time(&key).expect("key carries a timestamp");
        let age = now - stamp;
        println!("newest N0S key {key} is {} minutes old", age.num_minutes());

        assert!(
            age >= Duration::zero(),
            "{key} is stamped in the future ({stamp} vs {now})",
        );
        // A volume scan is 4-10 minutes; 90 gives room for a site in
        // maintenance without admitting "the first key of the day".
        assert!(
            age < Duration::minutes(90),
            "newest N0S key {key} is {} minutes old — the selection is not \
             picking the freshest object",
            age.num_minutes(),
        );
    }
}
