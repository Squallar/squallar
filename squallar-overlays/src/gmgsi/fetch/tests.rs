//! Key selection, against a verbatim S3 listing body.

use super::*;

/// A real `ListObjectsV2` response for `GMGSI_LW/2025/06/01/00/`, trimmed to
/// its `<Contents>` and keeping **both** product generations, because that hour
/// really did carry both.
const LISTING_WITH_BOTH_GENERATIONS: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<ListBucketResult xmlns="http://s3.amazonaws.com/doc/2006-03-01/">
<Name>noaa-gmgsi-pds</Name><KeyCount>2</KeyCount><IsTruncated>false</IsTruncated>
<Contents><Key>GMGSI_LW/2025/06/01/00/GLOBCOMPLIR_nc.2025060100</Key><Size>7300498</Size></Contents>
<Contents><Key>GMGSI_LW/2025/06/01/00/GLOBCOMPLIR_v3r0_blend_s202506010000000_e202506010009599_c202506010036335.nc</Key><Size>7473829</Size></Contents>
</ListBucketResult>"#;

#[test]
fn the_retired_legacy_granule_is_skipped_for_the_blend() {
    let key = newest_blend_key(LISTING_WITH_BOTH_GENERATIONS, GmgsiChannel::LongwaveIr)
        .expect("the hour holds a blend granule");
    assert!(key.ends_with("_c202506010036335.nc"), "chose {key}");
    assert!(
        !key.contains("GLOBCOMPLIR_nc."),
        "the legacy McIDAS granule was chosen: {key}"
    );
}

/// The four channels share a key *shape* but not a stem, and the stems do not
/// match the bucket prefixes: shortwave is `GMGSI_SW`/`GLOBCOMPSIR`.
#[test]
fn a_channels_listing_does_not_answer_for_another_channel() {
    assert!(newest_blend_key(LISTING_WITH_BOTH_GENERATIONS, GmgsiChannel::Visible).is_none());
    assert!(newest_blend_key(LISTING_WITH_BOTH_GENERATIONS, GmgsiChannel::WaterVapor).is_none());
    // Longwave is `LIR` and shortwave is `SIR`; neither is a prefix of the
    // other, so the LW listing must not answer for SW.
    assert!(newest_blend_key(LISTING_WITH_BOTH_GENERATIONS, GmgsiChannel::ShortwaveIr).is_none());
}

#[test]
fn the_newest_of_several_blends_wins() {
    let body = r#"<?xml version="1.0"?><ListBucketResult>
<Contents><Key>GMGSI_WV/2025/06/01/12/GLOBCOMPWV_v3r0_blend_s202506011200000_e202506011209599_c202506011239397.nc</Key></Contents>
<Contents><Key>GMGSI_WV/2025/06/01/12/GLOBCOMPWV_v3r0_blend_s202506011200000_e202506011209599_c202506011111111.nc</Key></Contents>
</ListBucketResult>"#;
    let key = newest_blend_key(body, GmgsiChannel::WaterVapor).unwrap();
    assert!(key.ends_with("_c202506011239397.nc"), "chose {key}");
}

#[test]
fn an_empty_hour_is_no_key_rather_than_an_error() {
    let body =
        r#"<?xml version="1.0"?><ListBucketResult><KeyCount>0</KeyCount></ListBucketResult>"#;
    assert!(newest_blend_key(body, GmgsiChannel::LongwaveIr).is_none());
}

#[test]
fn a_body_that_is_not_xml_yields_no_key() {
    assert!(newest_blend_key("<<<not xml", GmgsiChannel::LongwaveIr).is_none());
}

/// The blend lands ~40 minutes after the hour it covers, so the ladder must
/// reach past the current hour or it would 404 for most of every hour.
#[test]
fn the_listing_ladder_walks_back_from_the_current_hour() {
    let now = chrono::NaiveDate::from_ymd_opt(2025, 6, 1)
        .unwrap()
        .and_hms_opt(12, 5, 30)
        .unwrap();
    let attempts = listing_attempts(now);
    assert_eq!(attempts.len(), LOOKBACK_HOURS as usize);
    // Newest first, on the hour, strictly decreasing.
    assert_eq!(attempts[0].hour(), 12);
    assert_eq!(attempts[0].minute(), 0);
    assert_eq!(attempts[0].second(), 0);
    assert_eq!(attempts[1].hour(), 11);
    assert_eq!(attempts[LOOKBACK_HOURS as usize - 1].hour(), 9);
    assert!(attempts.windows(2).all(|w| w[0] > w[1]));
}

#[test]
fn the_hour_prefix_is_the_directory_the_object_lives_in() {
    let stamp = chrono::NaiveDate::from_ymd_opt(2025, 6, 1)
        .unwrap()
        .and_hms_opt(12, 0, 0)
        .unwrap();
    assert_eq!(
        DataSources::gmgsi_hour_prefix(GmgsiChannel::LongwaveIr.prefix(), &stamp),
        "GMGSI_LW/2025/06/01/12/"
    );
    // Zero-padded, so a single-digit month and hour still address the bucket.
    let january = chrono::NaiveDate::from_ymd_opt(2026, 1, 2)
        .unwrap()
        .and_hms_opt(3, 0, 0)
        .unwrap();
    assert_eq!(
        DataSources::gmgsi_hour_prefix(GmgsiChannel::Visible.prefix(), &january),
        "GMGSI_VIS/2026/01/02/03/"
    );
}

/// `GMGSI_SSR` was discontinued 2025-06-03. The prefix still lists objects for
/// older dates, so "the enum has no variant" is the only thing stopping a
/// re-add, and this states why.
#[test]
fn the_discontinued_ssr_product_is_not_a_channel() {
    assert_eq!(GmgsiChannel::all().len(), 4);
    assert!(
        !GmgsiChannel::all()
            .iter()
            .any(|c| c.prefix().contains("SSR"))
    );
}
