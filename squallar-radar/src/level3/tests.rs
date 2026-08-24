use super::*;
use crate::types::RadarProduct;

const ICD: &[(RadarProduct, &[(&str, i16)])] = &[
    (RadarProduct::SpecificDifferentialPhase, &[("N0K", 163)]),
    (RadarProduct::EchoTops, &[("EET", 135)]),
    (RadarProduct::VerticallyIntegratedLiquid, &[("DVL", 134)]),
    (RadarProduct::VilDensity, &[("DVL", 134), ("EET", 135)]),
    (RadarProduct::PrecipitationRate, &[("DPR", 176)]),
];

const DERIVED_FROM_OTHER_ROWS: &[RadarProduct] = &[RadarProduct::VilDensity];

fn icd_row(product: &RadarProduct) -> Option<&'static [(&'static str, i16)]> {
    ICD.iter().find(|(p, _)| p == product).map(|(_, ids)| *ids)
}

fn is_derived(product: &RadarProduct) -> bool {
    DERIVED_FROM_OTHER_ROWS.contains(product)
}

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

#[test]
fn a_product_stamp_reports_its_age_from_its_key() {
    let stamp = ProductStamp::from_key("TLX_N0S_2026_07_25_17_30_24");
    assert_eq!(stamp.age(at(17, 45, 24)).map(|a| a.num_minutes()), Some(15));
    assert_eq!(stamp.age(at(17, 30, 24)).map(|a| a.num_seconds()), Some(0));
    assert_eq!(stamp.age(at(17, 29, 24)).map(|a| a.num_minutes()), Some(-1));
}

#[test]
fn a_stamp_from_the_previous_day_is_stale() {
    let now = at(0, 5, 0);
    let overnight = ProductStamp::from_key("TLX_N0S_2026_07_24_23_58_48");
    assert_eq!(overnight.age(now).map(|a| a.num_minutes()), Some(6));
    assert!(
        !overnight.is_stale(now, Duration::minutes(30)),
        "the ordinary 00Z rollover is not staleness",
    );

    let dead = ProductStamp::from_key("TLX_N0S_2026_07_24_11_00_00");
    assert_eq!(dead.age(now).map(|a| a.num_hours()), Some(13));
    assert!(dead.is_stale(now, Duration::minutes(30)));
    assert!(dead.is_stale(now, Duration::hours(12)));
    assert!(!dead.is_stale(now, Duration::hours(14)));
}

#[test]
fn a_stamp_with_no_readable_time_reports_neither_age_nor_staleness() {
    let stamp = ProductStamp::from_key("garbage");
    assert_eq!(stamp.time, None);
    assert_eq!(stamp.age(at(12, 0, 0)), None);
    assert!(!stamp.is_stale(at(12, 0, 0), Duration::zero()));
    assert_eq!(stamp.key, "garbage");
}

#[test]
fn a_site_code_loses_its_leading_letter_not_a_literal_k() {
    assert_eq!(site_code("KTLX"), "TLX");
    assert_eq!(site_code("KFWS"), "FWS");
    assert_eq!(site_code("PAHG"), "AHG");
    assert_eq!(site_code("PHKI"), "HKI");
    assert_eq!(site_code("TJUA"), "JUA");
    assert_eq!(site_code("PGUA"), "GUA");
}

#[test]
fn shortening_a_site_code_is_idempotent() {
    assert_eq!(site_code(site_code("KTLX")), "TLX");
    assert_eq!(site_code("TLX"), "TLX");
}

#[test]
fn a_lowercase_identifier_still_loses_its_leading_letter() {
    assert_eq!(site_code("ktlx"), "tlx");
    assert_eq!(site_code("pHkI"), "HkI");
}

#[test]
fn a_four_byte_identifier_with_a_multibyte_head_is_not_sliced() {
    for id in ["éab", "Ω12", "日a", "🌀"] {
        assert_eq!(id.len(), 4, "{id:?} must be four bytes to exercise this");
        assert!(!id.is_char_boundary(1), "{id:?} must straddle byte 1");
        assert_eq!(site_code(id), id, "{id:?} must pass through untouched");
    }
}

#[test]
fn a_four_byte_identifier_that_slices_legally_is_still_not_a_site() {
    assert_eq!("aéb".len(), 4);
    assert!(
        "aéb".is_char_boundary(1),
        "byte 1 is legal here; that is the point"
    );
    assert_eq!(site_code("aéb"), "aéb");
}

#[test]
fn an_identifier_shorter_than_a_site_code_is_returned_whole() {
    for id in ["", "K", "KT", "KTL", "é", "🌀🌀"] {
        assert_eq!(site_code(id), id, "{id:?} must pass through untouched");
    }
}

#[test]
fn the_newest_key_is_the_maximum_not_merely_the_last_returned() {
    let keys = vec![
        "TLX_N0S_2026_07_25_17_30_24".to_string(),
        "TLX_N0S_2026_07_25_00_02_19".to_string(),
        "TLX_N0S_2026_07_25_09_13_03".to_string(),
    ];
    assert_eq!(newest(keys).as_deref(), Some("TLX_N0S_2026_07_25_17_30_24"),);
}

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

#[test]
fn the_day_prefix_constrains_the_listing_to_one_utc_day() {
    let d = NaiveDate::from_ymd_opt(2026, 7, 25).unwrap();
    let prefix = DataSources::level3_day_prefix("TLX", "N0S", &d);
    assert_eq!(prefix, "TLX_N0S_2026_07_25");
    assert!(!"TLX_N0S_2026_07_24_23_58_48".starts_with(&prefix));
    assert!("TLX_N0S_2026_07_25_17_30_24".starts_with(&prefix));
}

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

#[test]
fn the_volume_stamp_reads_day_one_as_the_epoch() {
    let t = volume_scan_started(&pdb_for_volume(20661, 7108, 0)).expect("a valid stamp");
    assert_eq!(t.to_string(), "2026-07-26 01:58:28");
    assert_eq!(
        volume_scan_started(&pdb_for_volume(1, 0, 0)).map(|t| t.to_string()),
        Some("1970-01-01 00:00:00".to_string()),
        "day 1 is the epoch itself",
    );
    assert!(volume_scan_started(&pdb_for_volume(0, 0, 0)).is_none());
}

#[test]
fn a_pdb_names_the_volume_it_started_within_a_minute_of() {
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
    let overnight = "TLX_EET_2026_07_24_23_59_10".to_string();
    assert_eq!(
        candidates_near(vec![overnight.clone()], just_after_midnight),
        vec![overnight],
    );
}

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

    assert_eq!(
        RadarProduct::VerticallyIntegratedLiquid.level3_products(),
        Some(&["DVL"][..]),
    );
    assert_eq!(RadarProduct::EchoTops.level3_products(), Some(&["EET"][..]));
}

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

#[ignore = "hits the live unidata-nexrad-level3 S3 bucket"]
#[tokio::test]
async fn live_every_requested_product_fetches_and_decodes() {
    let sources = DataSources::production();
    let now = chrono::Utc::now().naive_utc();

    for product in RadarProduct::all().iter().filter(|p| p.is_level3()) {
        let row = icd_row(product).unwrap_or_else(|| panic!("{} has no ICD row", product.name()));

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

#[ignore = "hits the live unidata-nexrad-level3 S3 bucket"]
#[tokio::test]
async fn live_a_four_letter_icao_site_resolves_to_bucket_keys() {
    let sources = DataSources::production();
    let today = chrono::Utc::now().naive_utc().date();

    let three = latest_key(&sources, "TLX", "N0S", &today)
        .await
        .expect("listing must succeed");
    assert!(three.is_some(), "TLX must have N0S objects today");

    let four = latest_key(&sources, "KTLX", "N0S", &today)
        .await
        .expect("listing must succeed");
    assert_eq!(four, None, "the bucket does not key on 4-letter codes");
}

#[ignore = "hits the live unidata-nexrad-level3 S3 bucket"]
#[tokio::test]
async fn live_the_dropped_srm_tilts_have_no_current_data() {
    let sources = DataSources::production();
    let today = chrono::Utc::now().naive_utc().date();

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
/// cargo test -p squallar-radar --release -- --ignored --nocapture live_a_loops_frames
/// ```
#[ignore = "hits the live unidata-nexrad-level3 S3 bucket and the Level II archive"]
#[tokio::test]
async fn live_a_loops_frames_each_pair_with_their_own_volume() {
    crate::tls::init();
    let sources = DataSources::production();
    let now = chrono::Utc::now().naive_utc();
    let start = now - Duration::hours(1);

    let product = RadarProduct::EchoTops;
    let code = product.level3_products().expect("EET")[0];
    let pick = product
        .level3_volume_pick()
        .expect("a Level III product names a pick");

    for site in ["KTLX", "KOUN", "KFWS", "KMPX", "KOAX", "KLZK"] {
        let Ok(volumes) = crate::scan::list_scans_for_range(site, start, now).await else {
            continue;
        };
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
            let Some(object) = product_from_candidates(&sources, candidates, *frame, pick).await
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

        assert!(
            paired.len() >= 3,
            "{site}: only {} of {} frames paired — the loop would be mostly gaps",
            paired.len(),
            frames.len(),
        );
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
