//! Level III products from the public `unidata-nexrad-level3` S3 bucket.
//!
//! This replaces NWS TGFTP (`tgftp.nws.noaa.gov/SL.us008001/DF.of/DC.radar/
//! DS.{dir}/SI.{site}/sn.last`), which sends no `Access-Control-Allow-Origin`
//! and answers `403` outright to a request carrying an `Origin:` header. It is
//! unreachable from a browser, and rustdar's web build must not depend on a
//! proxy to reach it.
//!
//! # What changes
//!
//! TGFTP addressed a product by *directory* and always served the newest one
//! from a fixed filename:
//!
//! ```text
//! DS.56rm0/SI.ktlx/sn.last
//! ```
//!
//! The bucket has neither. Keys are **flat**, with the timestamp in the name
//! and no `sn.last` alias:
//!
//! ```text
//! TLX_N0S_2026_07_25_17_30_24
//! ```
//!
//! So "the latest product" becomes: list the prefix for the UTC day, take the
//! last key. Zero-padding makes the bucket's own key order (UTF-8 binary)
//! identical to chronological order, so the last key *is* the newest — no
//! timestamp parsing is needed to pick it, only to report it.
//!
//! Two further differences bite:
//!
//! * **Site codes are three letters.** The bucket keys on `TLX`, not `KTLX`.
//!   See [`site_code`].
//! * **Products are named, not numbered.** `DS.56rm0` becomes `N0S`,
//!   `DS.176pr` becomes `DPR`, and so on — see
//!   [`crate::types::RadarProduct::level3_products`].
//!
//! The bytes themselves are unchanged: objects carry the same
//! `SDUS54 KOUN 251723\r\r\nN0STLX\r\r\n` WMO envelope `sn.last` did, and
//! [`nexrad_level3::decode::decode_product`] already strips it.
//!
//! # The storm-relative velocity gap
//!
//! rustdar used to request four SRM tilts (`56rm0`–`56rm3`). Only the lowest
//! survives. `N1S`, `N2S` and `N3S` are in the bucket historically — the last
//! keys are from 2020 — but **nothing has been written to them since**, because
//! NWS dropped the higher SRM tilts from the NOAAPort broadcast (SCN 22-96).
//! TGFTP still carried them because it is fed from a different path, so the
//! move to any NOAAPort-derived source loses them.
//!
//! Measured against the live bucket on 2026-07-25, for `TLX`:
//!
//! ```text
//! N0S  342 keys for the UTC day     N1S  0 keys, last object 2020-03-30
//! N0K  342                          N2S  0 keys, last object 2020-03-31
//! EET  342                          N3S  0 keys, last object 2020-04-01
//! DVL  342
//! HHC  342
//! DPR  342
//! ```
//!
//! This module does not paper over that. [`RadarProduct::level3_products`]
//! returns the six codes that exist, the three that do not are simply not
//! requested, and no substitute is silently swapped in — an SRM tilt derived
//! from a *different* elevation would be wrong in a way the UI could not show.

use chrono::{Duration, NaiveDate, NaiveDateTime};
use nexrad_level3::model::Level3Message;

use crate::archive::{self, ArchiveError};
use crate::sources::DataSources;

/// Failures fetching a Level III product.
#[derive(Debug, thiserror::Error)]
pub enum Level3Error {
    /// Listing or downloading from the bucket failed.
    #[error(transparent)]
    Bucket(#[from] ArchiveError),
    /// The bucket holds no product matching the request.
    ///
    /// An ordinary outcome for a site that is down or a product a site does
    /// not generate — not a failure to reach the bucket.
    #[error("no {product} product for {site}")]
    NoProduct {
        /// Three-letter site code.
        site: String,
        /// AWIPS product ID.
        product: String,
    },
    /// The object downloaded but did not decode.
    #[error("decode error: {0}")]
    Decode(#[from] nexrad_level3::result::Error),
}

/// Convenience alias for this module's operations.
pub type Result<T> = std::result::Result<T, Level3Error>;

/// The three-letter site code the Level III bucket keys on.
///
/// NEXRAD sites are named with a four-letter ICAO identifier (`KTLX`, `PHKI`,
/// `TJUA`), but Level III product keys use the last three (`TLX`, `HKI`,
/// `JUA`). Dropping the leading letter is the actual rule — it is not
/// "strip a leading `K`": Alaskan (`PA*`), Pacific (`PH*`), Puerto Rican
/// (`TJUA`) and Guam (`PGUA`) sites keep their trailing three just the same,
/// and stripping only `K` would leave `HKI` as `PHKI` and miss every non-CONUS
/// radar.
///
/// A code that is already three characters is returned unchanged, so this is
/// idempotent and safe to apply twice.
pub fn site_code(id: &str) -> &str {
    if id.len() == 4 { &id[1..] } else { id }
}

/// The latest key under one day's prefix, or `None` if the day has no objects.
///
/// Separated from the request so the selection rule is testable without a
/// socket. Keys are returned by S3 in UTF-8 binary order and the timestamp in
/// the name is fully zero-padded, so the maximum is the last element; this
/// takes the maximum explicitly rather than relying on that ordering, because
/// a listing assembled from several pages is only sorted if every page was.
fn newest(keys: Vec<String>) -> Option<String> {
    keys.into_iter().max()
}

/// Timestamp encoded in a Level III key, if it parses.
///
/// `TLX_N0S_2026_07_25_17_30_24` → 2026-07-25 17:30:24 UTC. Used for reporting
/// and for the live tests' freshness assertions, never for choosing a key.
pub fn key_time(key: &str) -> Option<NaiveDateTime> {
    // The last six underscore-separated fields are Y M D H M S. Counting from
    // the end rather than the start keeps this correct for site or product
    // codes that themselves contain an underscore.
    let parts: Vec<&str> = key.split('_').collect();
    if parts.len() < 8 {
        return None;
    }
    let stamp = parts[parts.len() - 6..].join("_");
    NaiveDateTime::parse_from_str(&stamp, "%Y_%m_%d_%H_%M_%S").ok()
}

/// List the keys for one site/product/UTC day.
async fn list_day(
    sources: &DataSources,
    site3: &str,
    product: &str,
    date: &NaiveDate,
) -> Result<Vec<String>> {
    let client = archive::shared_client();
    let prefix = DataSources::level3_day_prefix(site3, product, date);
    log::debug!("Listing Level III objects for prefix {prefix:?}");
    // Paged, not single-shot: a busy site writes ~342 objects per product per
    // day, which fits one 1000-key page today, but the page size is S3's
    // choice and a truncated listing would silently drop the newest keys —
    // which are exactly the ones this function exists to find.
    let keys = archive::collect_keys(&sources.level3_bucket, &prefix, None, |url| {
        archive::get_text(client, url)
    })
    .await?;
    Ok(keys)
}

/// Find the newest key for a product, falling back to the previous UTC day.
///
/// The fallback is what keeps the overlay alive across 00Z: for the first few
/// minutes of a UTC day the day's prefix is empty or nearly so, and without a
/// fallback every Level III layer would blank out once a night.
///
/// Note the asymmetry — the previous day is consulted only when today's
/// listing is *empty*. A non-empty listing for today always wins, because its
/// last key is by construction newer than anything from yesterday.
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
    log::info!(
        "No {product} for {site3} on {today}, falling back to {yesterday}"
    );
    Ok(newest(list_day(sources, site3, product, &yesterday).await?))
}

/// Fetch and decode the latest Level III product for a site.
///
/// `site` may be the four-letter ICAO code the rest of the application uses;
/// it is reduced to three by [`site_code`]. `product` is an AWIPS ID such as
/// `"N0S"`.
pub async fn fetch_latest_product(
    sources: &DataSources,
    site: &str,
    product: &str,
    now: NaiveDateTime,
) -> Result<Level3Message> {
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

    // The object carries the same WMO/AWIPS envelope `sn.last` did; the
    // decoder strips it, so this is byte-identical handling to the TGFTP path.
    Ok(nexrad_level3::decode::decode_product(&bytes)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::RadarProduct;

    /// The AWIPS product ID and Level III message code for every product
    /// rustdar renders from Level III data.
    ///
    /// Transcribed from NWS 2620001 (ICD for the RPG to Class 1 User), product
    /// list. **Nothing here is derived from
    /// [`RadarProduct::level3_products`]** — that is the whole point of the
    /// table. It is keyed by `RadarProduct` rather than by AWIPS ID so it can
    /// contradict the mapping: keyed by ID it could only ever agree, because
    /// `DVL` decodes as message code 134 no matter which product asked for it.
    ///
    /// | product | AWIPS | message code | field |
    /// |---|---|---|---|
    /// | Storm Relative Velocity | `N0S` | 56 | Storm Relative Mean Radial Velocity |
    /// | Specific Differential Phase | `N0K` | 163 | Specific Differential Phase |
    /// | Echo Tops | `EET` | 135 | Enhanced Echo Tops |
    /// | Vertically Integrated Liquid | `DVL` | 134 | Digital Vertically Integrated Liquid |
    /// | Hydrometeor Classification | `HHC` | 177 | Hybrid Hydrometeor Classification |
    /// | Precipitation Rate | `DPR` | 176 | Digital Instantaneous Precipitation Rate |
    const ICD: &[(RadarProduct, &str, i16)] = &[
        (RadarProduct::StormRelativeVelocity, "N0S", 56),
        (RadarProduct::SpecificDifferentialPhase, "N0K", 163),
        (RadarProduct::EchoTops, "EET", 135),
        (RadarProduct::VerticallyIntegratedLiquid, "DVL", 134),
        (RadarProduct::HydrometeorClassification, "HHC", 177),
        (RadarProduct::PrecipitationRate, "DPR", 176),
    ];

    /// The ICD row for a product, or `None` if it has none.
    fn icd_row(product: &RadarProduct) -> Option<&'static (RadarProduct, &'static str, i16)> {
        ICD.iter().find(|(p, ..)| p == product)
    }

    /// Every Level III product must request the AWIPS ID the ICD gives for the
    /// field rustdar renders it as.
    ///
    /// `every_level3_product_has_at_least_one_awips_code` checks only the
    /// *shape* of the string — three characters, uppercase or digit — so it is
    /// just as happy with `DVL` under Echo Tops as with `EET`. That swap does
    /// not crash and does not even fail to decode: the Echo Tops pane fetches
    /// Digital VIL, [`crate::render`]'s VIL look-up table keys on the *decoded*
    /// product code 134 and so helps the wrong bytes decode cleanly in kg/m²,
    /// and the Echo Tops palette then paints a 62 kg/m² VIL core and reports
    /// "Echo Tops: 62.0 kft". Plausible numbers, plausible colours, wrong field,
    /// no error anywhere.
    ///
    /// So the mapping is pinned per product against [`ICD`], transcribed from
    /// the ICD rather than read back out of the code under test.
    #[test]
    fn each_level3_product_requests_the_awips_id_the_icd_gives_it() {
        for product in RadarProduct::all() {
            let Some(&(_, want, _)) = icd_row(product) else {
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
            assert_eq!(
                codes,
                [want],
                "{} requests {codes:?}; the ICD gives {want} for that field",
                product.name(),
            );
        }
    }

    /// The table must cover every Level III product and nothing else, so the
    /// loop above cannot pass by skipping a product it forgot to list.
    #[test]
    fn the_icd_table_covers_exactly_the_level3_products() {
        let level3: Vec<_> = RadarProduct::all().iter().filter(|p| p.is_level3()).collect();
        assert_eq!(
            ICD.len(),
            level3.len(),
            "the ICD table has {} rows for {} Level III products",
            ICD.len(),
            level3.len(),
        );
        // Distinct AWIPS IDs: a table that named one ID twice would let two
        // products agree with it while pointing at the same field.
        for (i, (_, code, _)) in ICD.iter().enumerate() {
            assert!(
                !ICD[..i].iter().any(|(_, other, _)| other == code),
                "{code} appears twice in the ICD table",
            );
        }
    }

    /// The rule is "drop the leading letter", not "strip a leading K".
    ///
    /// Every non-CONUS site would break under the latter, and they are exactly
    /// the ones no CONUS-only test would notice.
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

    /// The newest key is the maximum, and it is chosen by *value* rather than
    /// by position, so a listing that arrived out of order still resolves.
    ///
    /// The fixture is deliberately shuffled: a version of `newest` that
    /// returned `keys.last()` passes on a sorted fixture and fails here.
    #[test]
    fn the_newest_key_is_the_maximum_not_merely_the_last_returned() {
        let keys = vec![
            "TLX_N0S_2026_07_25_17_30_24".to_string(),
            "TLX_N0S_2026_07_25_00_02_19".to_string(),
            "TLX_N0S_2026_07_25_09_13_03".to_string(),
        ];
        assert_eq!(
            newest(keys).as_deref(),
            Some("TLX_N0S_2026_07_25_17_30_24"),
        );
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

    /// The timestamp comes off the tail of the key, so a product code
    /// containing digits or a site code of a different length cannot shift it.
    ///
    /// Expected values are read off the key by hand, not produced by the
    /// parser under test.
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

    /// Every product code the type layer asks for must be one this module can
    /// actually name, and every Level III product must have at least one code.
    ///
    /// Catches a product being added to `RadarProduct::is_level3` without a
    /// code, which would silently render an always-empty layer.
    #[test]
    fn every_level3_product_has_at_least_one_awips_code() {
        for product in RadarProduct::all() {
            let codes = product.level3_products();
            if product.is_level3() {
                let codes = codes.unwrap_or_else(|| {
                    panic!("{} is Level III but names no product code", product.name())
                });
                assert!(!codes.is_empty(), "{} names an empty code list", product.name());
                for code in codes {
                    assert_eq!(code.len(), 3, "{code} is not a 3-character AWIPS ID");
                    assert!(
                        code.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit()),
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

    /// The three dead SRM tilts must not be requested.
    ///
    /// Asserted by name rather than by count: a future edit that adds "N1S"
    /// back — the obvious thing to try when someone notices only one SRM tilt
    /// renders — reintroduces three fetches that can only ever fail.
    #[test]
    fn the_discontinued_srm_tilts_are_not_requested() {
        let codes = RadarProduct::StormRelativeVelocity
            .level3_products()
            .expect("SRM is Level III");
        assert_eq!(codes, ["N0S"]);
        for dead in ["N1S", "N2S", "N3S"] {
            assert!(
                !codes.contains(&dead),
                "{dead} has had no data written since 2020 (NWS SCN 22-96)",
            );
        }
    }

    // ── Live checks ───────────────────────────────────────────────────────
    //
    // Run with:
    //   cargo test -p rustdar-radar --lib -- --ignored --nocapture level3

    /// Every product rustdar asks for genuinely fetches and decodes.
    ///
    /// This is the check that the migration works end to end: for each Level
    /// III product it takes the AWIPS ID out of
    /// [`RadarProduct::level3_products`] — the mapping production fetches with,
    /// not a literal repeated here — then lists the live bucket, downloads the
    /// newest object and decodes it. Nothing is stubbed.
    ///
    /// The decoded message code is checked against [`ICD`] keyed by **product**.
    /// That is what makes a wrong product→ID mapping fail here: keyed by AWIPS
    /// ID instead, the table could only ever agree with the fetch, because
    /// `DVL` decodes as 134 whichever product asked for it. Swap `EET` and
    /// `DVL` in `level3_products` and this test downloads Digital VIL for Echo
    /// Tops and sees 134 where the ICD says 135.
    #[ignore = "hits the live unidata-nexrad-level3 S3 bucket"]
    #[tokio::test]
    async fn live_every_requested_product_fetches_and_decodes() {
        let sources = DataSources::production();
        let now = chrono::Utc::now().naive_utc();

        for product in RadarProduct::all().iter().filter(|p| p.is_level3()) {
            let &(_, _, want_message_code) = icd_row(product)
                .unwrap_or_else(|| panic!("{} has no ICD row", product.name()));
            let codes = product
                .level3_products()
                .unwrap_or_else(|| panic!("{} names no product code", product.name()));

            for &code in codes {
                let msg = fetch_latest_product(&sources, "KTLX", code, now)
                    .await
                    .unwrap_or_else(|e| panic!("{} fetch of {code} failed: {e}", product.name()));
                let got = msg.header.message_code;
                println!(
                    "{} -> {code}: message_code={got}, product_code={}, symbology={}",
                    product.name(),
                    msg.pdb.product_code,
                    msg.symbology.is_some(),
                );
                assert_eq!(
                    got, want_message_code,
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

    /// The 4-letter site code the application uses must reach the 3-letter
    /// bucket keys.
    ///
    /// Distinct from the offline `site_code` test: that one pins the string
    /// transform, this one proves the transform is the one the *bucket* wants.
    /// Passing "KTLX" straight through would 404 every listing, and an empty
    /// listing is indistinguishable from "site is down" without this.
    #[ignore = "hits the live unidata-nexrad-level3 S3 bucket"]
    #[tokio::test]
    async fn live_a_four_letter_icao_site_resolves_to_bucket_keys() {
        let sources = DataSources::production();
        let today = chrono::Utc::now().naive_utc().date();

        let three = latest_key(&sources, "TLX", "N0S", &today)
            .await
            .expect("listing must succeed");
        assert!(three.is_some(), "TLX must have N0S objects today");

        // The un-shortened form must find nothing — which is what makes the
        // shortening load-bearing rather than cosmetic.
        let four = latest_key(&sources, "KTLX", "N0S", &today)
            .await
            .expect("listing must succeed");
        assert_eq!(four, None, "the bucket does not key on 4-letter codes");
    }

    /// The three SRM tilts rustdar dropped really are gone.
    ///
    /// This is the evidence for the gap, kept executable so that if NWS ever
    /// restores them the test starts failing and someone re-adds the tilts.
    /// It asserts on the *live bucket*, not on rustdar's own table, so it
    /// cannot pass by agreeing with the decision it is checking.
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

    /// The newest object is actually recent.
    ///
    /// Pins the "last key wins" rule against the live bucket: a listing that
    /// returned keys in some other order, or a selection that took the first
    /// key, would yield something hours old rather than minutes.
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
