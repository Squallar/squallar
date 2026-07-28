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
//! # The storm-relative velocity gap
//!
//! Only the lowest SRM product survives. `N1S`/`N2S`/`N3S` exist in the bucket
//! historically but have had nothing written to them since 2020, because NWS
//! dropped the higher tilts from the NOAAPort broadcast (SCN 22-96); every
//! CORS-clean source is NOAAPort-derived. Measured on 2026-07-25 for `TLX`:
//! `N0S`/`N0K`/`EET`/`DVL`/`HHC`/`DPR` 342 keys each for the UTC day, `N1S`,
//! `N2S`, `N3S` zero (last objects 2020-03-30, 03-31, 04-01).
//!
//! All four tilts are **derived** instead, from the dealiased velocity products
//! the same bucket does carry — `N0G` and `N1G` (code 154), `N2U` and `N3U`
//! (code 99), 294 objects a day each, the same as `N0S`. `N0S` is still fetched
//! but is no longer drawn: it supplies the storm motion vector, which no
//! velocity product carries. Deriving 0.5° too is what makes all four panes
//! 0.25 km fields at 254 levels that a storm motion override reaches; rendering
//! `N0S` left the lowest one coarser than its neighbours and deaf to the
//! override. See [`crate::srm`].
//!
//! No substitute is swapped in for a *missing* tilt: an SRM field from a
//! different elevation would be wrong in a way the UI could not show, so the
//! elevation always comes from the fetched product's own Product Description
//! Block.

use chrono::{Duration, NaiveDate, NaiveDateTime};
use nexrad_level3::model::Level3Message;

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
    Ok(Level3Product { message, stamp })
}

/// Fetch the latest Level III product for a site as raw bytes, WMO/AWIPS
/// envelope included. For products whose payload rustdar reads as text — the
/// NVW VAD Wind Profile's tabular block via
/// [`crate::nrot::parse_nvw_wind_levels`] — rather than decoding.
pub async fn fetch_latest_raw(
    sources: &DataSources,
    site: &str,
    product: &str,
    now: NaiveDateTime,
) -> Result<Vec<u8>> {
    let site3 = site_code(site).to_uppercase();
    let date = now.date();
    let Some(key) = latest_key(sources, &site3, product, &date).await? else {
        return Err(Level3Error::NoProduct {
            site: site3,
            product: product.to_string(),
        });
    };
    let url = sources.level3_object_url(&key);
    log::info!("Fetching Level III (raw) {key}");
    let client = archive::shared_client();
    Ok(archive::get_bytes(client, url).await?.to_vec())
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
    /// | Storm Relative Velocity | `N0S` | 56 | Storm Relative Mean Radial Velocity (vector only, not rendered) |
    /// | ″ | `N0G` | 154 | Super-Res Digital Base Velocity (derived to SRM) |
    /// | ″ | `N1G` | 154 | Super-Res Digital Base Velocity (derived to SRM) |
    /// | ″ | `N2U` | 99 | Digital Base Velocity (derived to SRM) |
    /// | ″ | `N3U` | 99 | Digital Base Velocity (derived to SRM) |
    /// | Specific Differential Phase | `N0K` | 163 | Specific Differential Phase |
    /// | Echo Tops | `EET` | 135 | Enhanced Echo Tops |
    /// | Vertically Integrated Liquid | `DVL` | 134 | Digital Vertically Integrated Liquid |
    /// | Hydrometeor Classification | `HHC` | 177 | Hybrid Hydrometeor Classification |
    /// | Precipitation Rate | `DPR` | 176 | Digital Instantaneous Precipitation Rate |
    ///
    /// The AWIPS IDs are listed **in request order**, so the table pins which
    /// tilt each one is, not merely that the set is right.
    const ICD: &[(RadarProduct, &[(&str, i16)])] = &[
        (
            RadarProduct::StormRelativeVelocity,
            &[
                ("N0S", 56),
                ("N0G", 154),
                ("N1G", 154),
                ("N2U", 99),
                ("N3U", 99),
            ],
        ),
        (RadarProduct::SpecificDifferentialPhase, &[("N0K", 163)]),
        (RadarProduct::EchoTops, &[("EET", 135)]),
        (RadarProduct::VerticallyIntegratedLiquid, &[("DVL", 134)]),
        (RadarProduct::HydrometeorClassification, &[("HHC", 177)]),
        (RadarProduct::PrecipitationRate, &[("DPR", 176)]),
    ];

    fn icd_row(product: &RadarProduct) -> Option<&'static [(&'static str, i16)]> {
        ICD.iter().find(|(p, _)| p == product).map(|(_, ids)| *ids)
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
        // pointing at the same field.
        let all: Vec<&str> = ICD
            .iter()
            .flat_map(|(_, ids)| ids.iter().map(|(id, _)| *id))
            .collect();
        for (i, code) in all.iter().enumerate() {
            assert!(
                !all[..i].contains(code),
                "{code} appears twice in the ICD table",
            );
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

    /// The three dead SRM tilts must not be requested. Asserted by name, not
    /// count: adding "N1S" back is the obvious thing to try when someone
    /// notices the tilts are computed rather than fetched, and it can only ever
    /// fail.
    #[test]
    fn the_discontinued_srm_tilts_are_not_requested() {
        let codes = RadarProduct::StormRelativeVelocity
            .level3_products()
            .expect("SRM is Level III");
        assert_eq!(codes, ["N0S", "N0G", "N1G", "N2U", "N3U"]);
        for dead in ["N1S", "N2S", "N3S"] {
            assert!(
                !codes.contains(&dead),
                "{dead} has had no data written since 2020 (NWS SCN 22-96)",
            );
        }
    }

    /// Every SRM tilt must come from a product the derivation recognises as
    /// dealiased velocity, or it would render as base velocity under a
    /// storm-relative label — the storm motion silently never applied.
    ///
    /// `N0S` is the one key fetched that is not a tilt. It is product 56,
    /// already storm-relative, so `srm::derive` refuses it; it is fetched for
    /// the storm motion vector in its Product Description Block alone.
    #[test]
    fn every_srm_tilt_requests_a_dealiased_velocity_product() {
        let codes = RadarProduct::StormRelativeVelocity
            .level3_products()
            .expect("SRM is Level III");
        let row = icd_row(&RadarProduct::StormRelativeVelocity).expect("SRM has an ICD row");
        assert_eq!(
            codes[0],
            crate::srm::STORM_MOTION_PRODUCT,
            "the first key is the vector source, and only that",
        );
        assert_eq!(
            codes[1..],
            crate::srm::SRM_TILT_PRODUCTS,
            "the rest are the tilts"
        );
        for (id, message_code) in &row[1..] {
            assert!(
                crate::srm::VELOCITY_PRODUCT_CODES.contains(message_code),
                "{id} decodes as {message_code}, which srm::derive would refuse",
            );
        }
        // Product 56 must NOT be in that set, or a tilt would have the
        // correction applied to a field that already has it.
        assert!(!crate::srm::VELOCITY_PRODUCT_CODES.contains(&56));
        assert!(
            !crate::srm::SRM_TILT_PRODUCTS.contains(&crate::srm::STORM_MOTION_PRODUCT),
            "the vector source is back as a tilt: it is 1 km at the RPG's 16 display \
             levels where every other tilt is 0.25 km at 254, and its gate values \
             already carry the RPG's vector, so the storm motion override cannot \
             reach it",
        );
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
