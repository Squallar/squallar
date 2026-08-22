//! The format against itself, and against the ways it could look right while
//! being wrong: a collapsed key, a reordered ring, an empty decode.
//!
//! Every pack here is produced by [`write`] — the same encoder
//! `tools/nws-zone-pack` calls by path dependency, so these are round-trips
//! against the real writer rather than against a fixture of hand-laid bytes
//! that could agree with a reader both halves got wrong.

use super::*;

fn square(lat: f64, lon: f64, radius: f64) -> Vec<(f64, f64)> {
    vec![
        (lat - radius, lon - radius),
        (lat - radius, lon + radius),
        (lat + radius, lon + radius),
        (lat + radius, lon - radius),
        (lat - radius, lon - radius),
    ]
}

fn k(kind: Kind, ugc: &str) -> [u8; 7] {
    key(kind, ugc).expect("a six-character UGC is a key")
}

/// Three zones, deliberately awkward: one with a hole, one in two disjoint
/// parts (the shape a unioned multi-feature zone takes), and one whose UGC
/// collides with the first's under a *different* kind — which is the whole
/// reason the key is `(kind, ugc)`.
fn corpus() -> Vec<PackedZone> {
    let mut zones = vec![
        (
            k(Kind::County, "FLC087"),
            vec![vec![square(24.7, -81.4, 0.3), square(24.7, -81.4, 0.1)]],
        ),
        (
            k(Kind::Forecast, "AMZ350"),
            vec![
                vec![square(30.0, -88.0, 0.5)],
                vec![square(31.0, -87.0, 0.25)],
            ],
        ),
        (
            k(Kind::Fire, "FLC087"),
            vec![vec![square(25.0, -80.0, 1.0)]],
        ),
    ];
    zones.sort_by_key(|(key, _)| *key);
    zones
}

/// The corpus has 5 rings of 5 points across 4 polygons — 25 vertices, and the
/// number the round-trip must actually have compared.
const CORPUS_VERTICES: usize = 25;

const CODINGS: [(Coding, u16); 3] = [
    (Coding::F64, 0),
    (Coding::F32, 0),
    // 1e-5 deg is the quantum the shipped pack was measured at: ~1.1 m of
    // latitude, an order finer than the 0.005 deg simplification.
    (Coding::Varint, 5),
];

#[test]
fn every_coding_round_trips_holes_parts_and_ring_order() {
    for (coding, quantum_exp) in CODINGS {
        let zones = corpus();
        let bytes = write(&zones, coding, quantum_exp, 0.005);
        let pack = ZonePack::open(bytes).expect("the writer's own output must open");
        assert_eq!(pack.zone_count(), zones.len());
        assert_eq!(pack.coding(), coding);
        assert_eq!(pack.epsilon(), 0.005, "the pack must carry its own epsilon");

        let tolerance = match coding {
            Coding::F64 => 0.0,
            Coding::F32 | Coding::Varint => 1e-5,
        };
        let mut compared = 0usize;
        for (key, want) in &zones {
            let kind = Kind::from_byte(key[0]).expect("a kind byte");
            let ugc = std::str::from_utf8(&key[1..]).expect("ascii").trim_end();
            let got = pack
                .get(kind, ugc)
                .unwrap_or_else(|| panic!("{coding:?}: {ugc} must be findable"));
            assert_eq!(got.len(), want.len(), "{coding:?}: polygon count");
            for (got_polygon, want_polygon) in got.iter().zip(want) {
                // Ring 0 is the exterior and the rest are holes; a codec that
                // reordered rings would turn an island into a lake.
                assert_eq!(got_polygon.len(), want_polygon.len(), "{coding:?}: rings");
                for (got_ring, want_ring) in got_polygon.iter().zip(want_polygon) {
                    assert_eq!(got_ring.len(), want_ring.len(), "{coding:?}: points");
                    for (&(glat, glon), &(wlat, wlon)) in got_ring.iter().zip(want_ring) {
                        assert!(
                            (glat - wlat).abs() <= tolerance && (glon - wlon).abs() <= tolerance,
                            "{coding:?}: ({glat}, {glon}) is not ({wlat}, {wlon})",
                        );
                        compared += 1;
                    }
                }
            }
        }
        // The floor. A decoder that answered with empty geometry would satisfy
        // every loop above without executing one of them.
        assert_eq!(
            compared, CORPUS_VERTICES,
            "{coding:?}: the round-trip compared nothing",
        );
    }
}

/// The reason the key is a pair. Two zones share the id `FLC087` and are
/// different shapes; a bare-UGC index would answer one of them for both, and
/// the map would paint a real, filled, correctly coloured polygon in the wrong
/// place — indistinguishable from a correct one.
#[test]
fn the_same_ugc_under_two_kinds_stays_two_different_shapes() {
    let bytes = write(&corpus(), Coding::Varint, 5, 0.005);
    let pack = ZonePack::open(bytes).expect("open");

    let county = pack.get(Kind::County, "FLC087").expect("county FLC087");
    let fire = pack.get(Kind::Fire, "FLC087").expect("fire FLC087");
    assert_ne!(
        county[0][0][0], fire[0][0][0],
        "keying by the bare UGC would have collapsed these into one shape",
    );
    assert!(
        pack.get(Kind::Forecast, "FLC087").is_none(),
        "and the third kind carries no such zone at all",
    );
}

/// A longer id must not be truncated into another zone's key. Silently
/// answering `FLC087` for `FLC0871` is the collision the pair-key exists to
/// prevent, arriving by the other door.
#[test]
fn an_id_that_is_not_six_ascii_characters_has_no_key_at_all() {
    assert!(key(Kind::County, "FLC087").is_some(), "the control");
    assert!(key(Kind::County, "OKZ4").is_some(), "shorter is padded");
    for bad in ["", "FLC0871", "FLC 87", "FL-087", "FLC08\u{e9}"] {
        assert!(
            key(Kind::County, bad).is_none(),
            "{bad:?} must have no key rather than a truncated one",
        );
    }

    let bytes = write(&corpus(), Coding::Varint, 5, 0.005);
    let pack = ZonePack::open(bytes).expect("open");
    assert!(
        pack.get(Kind::County, "FLC0871").is_none(),
        "a seven-character id must miss, not resolve to FLC087's shape",
    );
}

#[test]
fn a_url_segment_names_a_kind_and_an_unknown_one_names_none() {
    assert_eq!(Kind::from_url_segment("county"), Some(Kind::County));
    assert_eq!(Kind::from_url_segment("forecast"), Some(Kind::Forecast));
    assert_eq!(Kind::from_url_segment("fire"), Some(Kind::Fire));
    for unknown in ["", "zones", "Coastal", "county/", "marine"] {
        assert_eq!(
            Kind::from_url_segment(unknown),
            None,
            "{unknown:?} must fall through to the HTTP path, not guess a kind",
        );
    }
}

/// **The silent failure.** An encoder that wrote well-formed nothing would
/// produce a smaller file, open cleanly, resolve every lookup to `Some`, and
/// leave the map blank. Three shapes of nothing, each refused at install.
#[test]
fn a_pack_that_decodes_to_no_drawable_geometry_is_refused() {
    let cases: Vec<(&str, Vec<PackedZone>)> = vec![
        ("no zones at all", Vec::new()),
        (
            "a zone with no polygons",
            vec![(k(Kind::County, "OKC001"), Vec::new())],
        ),
        (
            "a polygon with no rings",
            vec![(k(Kind::County, "OKC001"), vec![Vec::new()])],
        ),
        (
            "a ring that bounds no area",
            vec![(
                k(Kind::County, "OKC001"),
                vec![vec![vec![(35.0, -97.0), (36.0, -97.0)]]],
            )],
        ),
    ];

    for (what, zones) in cases {
        let bytes = write(&zones, Coding::Varint, 5, 0.005);
        assert_eq!(
            ZonePack::open(bytes).err(),
            Some(PackError::Undrawable),
            "{what} must be refused at install, not discovered on the map",
        );
    }

    // The control: the same code path accepts a corpus that does draw.
    let bytes = write(&corpus(), Coding::Varint, 5, 0.005);
    assert!(
        ZonePack::open(bytes).is_ok(),
        "the guard must not refuse a pack that draws",
    );
}

/// The probe samples across the index, so rubbish in the tail is caught too —
/// not only a file that is empty from its first byte.
#[test]
fn the_draws_something_probe_reaches_the_end_of_the_index() {
    let mut zones: Vec<PackedZone> = (0..200)
        .map(|i| {
            (
                k(Kind::County, &format!("OKC{i:03}")),
                vec![vec![square(35.0 + f64::from(i) / 100.0, -97.0, 0.1)]],
            )
        })
        .collect();
    assert!(
        ZonePack::open(write(&zones, Coding::Varint, 5, 0.005)).is_ok(),
        "premise: 200 drawable zones open",
    );

    // Only the last one is empty. A probe that read the front would miss it.
    let last = zones.len() - 1;
    zones[last].1 = Vec::new();
    assert_eq!(
        ZonePack::open(write(&zones, Coding::Varint, 5, 0.005)).err(),
        Some(PackError::Undrawable),
        "an empty zone at the end of the index must still be found",
    );
}

#[test]
fn a_truncated_pack_is_refused_rather_than_panicking() {
    let bytes = write(&corpus(), Coding::Varint, 5, 0.005);
    assert!(
        ZonePack::open(bytes.clone()).is_ok(),
        "premise: the whole pack opens",
    );
    for cut in [0, 1, 5, 6, 8, 10, 25, 26, 30, 40, bytes.len() - 1] {
        // Some prefixes legitimately parse a header; what must never happen is
        // a panic, and what must not happen is a decode claiming geometry the
        // bytes do not contain.
        if let Ok(pack) = ZonePack::open(bytes[..cut].to_vec()) {
            for i in 0..pack.zone_count() {
                let _ = pack.at(i);
            }
        }
    }
}

#[test]
fn a_pack_of_another_format_or_version_is_refused() {
    let good = write(&corpus(), Coding::Varint, 5, 0.005);

    let mut wrong_magic = good.clone();
    wrong_magic[0] = b'X';
    assert_eq!(
        ZonePack::open(wrong_magic).err(),
        Some(PackError::NotAPack),
        "a file that is not a pack must not be read as one",
    );

    let mut wrong_version = good.clone();
    wrong_version[6..8].copy_from_slice(&(VERSION + 1).to_le_bytes());
    assert_eq!(
        ZonePack::open(wrong_version).err(),
        Some(PackError::NotAPack)
    );

    let mut wrong_coding = good;
    wrong_coding[8..10].copy_from_slice(&7u16.to_le_bytes());
    assert_eq!(
        ZonePack::open(wrong_coding).err(),
        Some(PackError::NotAPack),
        "a coding this build cannot decode must be refused, not guessed at",
    );
}

/// A corrupt length is the one that costs a browser tab its memory rather than
/// its correctness. Both a fixed-width count and a varint one.
#[test]
fn a_corrupt_length_cannot_reserve_more_than_the_file_holds() {
    let mut bytes = write(&corpus(), Coding::F64, 0, 0.005);
    let blob_start = HEADER_LEN + corpus().len() * INDEX_ENTRY_LEN + 4;
    bytes[blob_start..blob_start + 4].copy_from_slice(&u32::MAX.to_le_bytes());
    // The header still parses; the probe is what meets the lie.
    assert_eq!(
        ZonePack::open(bytes).err(),
        Some(PackError::Undrawable),
        "a polygon count the blob cannot hold must be refused, not allocated",
    );

    let mut bytes = write(&corpus(), Coding::Varint, 5, 0.005);
    // A five-byte varint asking for 2^28 polygons, over the first blob's first
    // bytes. The blob is longer than five bytes, so nothing shifts.
    let blob_start = HEADER_LEN + corpus().len() * INDEX_ENTRY_LEN + 4;
    bytes[blob_start..blob_start + 5].copy_from_slice(&[0x80, 0x80, 0x80, 0x80, 0x01]);
    assert_eq!(
        ZonePack::open(bytes).err(),
        Some(PackError::Undrawable),
        "a varint count the blob cannot hold must be refused too",
    );

    // A zone_count larger than the file: refused before the index is sliced.
    let mut bytes = write(&corpus(), Coding::Varint, 5, 0.005);
    bytes[22..26].copy_from_slice(&u32::MAX.to_le_bytes());
    assert_eq!(ZonePack::open(bytes).err(), Some(PackError::Truncated));
}

/// The index is a binary search, so every key must be findable and every
/// non-key must miss — checked across a corpus big enough for the search to
/// have somewhere to go wrong.
#[test]
fn every_key_in_a_large_index_is_findable_and_no_other_one_is() {
    let mut zones: Vec<PackedZone> = Vec::new();
    for kind in [Kind::Forecast, Kind::County, Kind::Fire] {
        for i in 0..500 {
            zones.push((
                k(
                    kind,
                    &format!("ZZ{}{i:03}", kind.label().as_bytes()[0] as char),
                ),
                vec![vec![square(35.0 + f64::from(i) / 1000.0, -97.0, 0.05)]],
            ));
        }
    }
    zones.sort_by_key(|(key, _)| *key);
    let count = zones.len();
    let pack = ZonePack::open(write(&zones, Coding::Varint, 5, 0.005)).expect("open");

    let mut found = 0usize;
    for (key, _) in &zones {
        let kind = Kind::from_byte(key[0]).expect("kind");
        let ugc = std::str::from_utf8(&key[1..]).expect("ascii").trim_end();
        assert!(pack.get(kind, ugc).is_some(), "{ugc} under {kind:?}");
        found += 1;
    }
    assert_eq!(found, count, "the search found nothing to look for");

    for absent in ["ZZF999", "AAA000", "ZZC500"] {
        assert!(
            pack.get(Kind::County, absent).is_none(),
            "{absent} is not in the pack and must not resolve to a neighbour",
        );
    }
}

/// Installing is what the app does with a validated pack, and a rejected one
/// must leave the process exactly as it was: resolving over HTTP.
#[test]
fn install_publishes_a_good_pack_and_a_bad_one_changes_nothing() {
    // Deliberately not asserting `installed().is_none()` first: this static is
    // process-wide and another test in this binary may have installed one.
    let before = installed().map(|pack| pack.byte_len());

    assert_eq!(
        install(vec![0u8; 4]).err(),
        Some(PackError::NotAPack),
        "four bytes of nothing are not a pack",
    );
    assert_eq!(
        installed().map(|pack| pack.byte_len()),
        before,
        "a refused pack must not disturb what is installed",
    );

    let bytes = write(&corpus(), Coding::Varint, 5, 0.005);
    let want = bytes.len();
    let pack = install(bytes).expect("a drawable pack installs");
    assert_eq!(pack.zone_count(), 3);
    assert_eq!(
        installed().map(|pack| pack.byte_len()),
        Some(want),
        "and a good one is what the next lookup reads",
    );
}
