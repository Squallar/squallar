//! A TDWR volume states where its radar is, in whichever of the two scales its
//! producer used.
//!
//! # What went wrong
//!
//! The Volume Data Block's `Lat`/`Long` are `Real*4` degrees — ICD 2620002AA
//! Table XVII-B, *Data Block #1 (Volume Data)*. Older TDWR Level II volumes
//! carry the **Level III** radar position in those fields instead: ICD
//! 2620001AD's Product Description Block halfwords 11-12 and 13-14, an `INT*4`
//! count of thousandths of a degree, widened into the `Real*4` without being
//! divided. `TORD20200810_000135_V08` states `41797.0, -87858.0`.
//!
//! "Older", and not "written before 2021-09-15", which is what this said until
//! the switch was measured at more than one radar. It is a **per-site
//! rollout**: five of the terminal radars below were already writing degrees on
//! 2021-09-14 while `TORD` was not, and `TMIA` was still writing thousandths on
//! 2021-09-16. `nexrad_decode`'s `VolumeDataBlock::latitude_raw` carries the
//! table. The 2020 and 2026 fixtures here sit either side of every site's own
//! switch whenever it happened, so the pairing is unaffected.
//!
//! Read as degrees, thousandths are not a place on Earth, and
//! [`SitePosition::from_volume`](rustdar_radar::site_position::SitePosition::from_volume)
//! refused them — correctly, on the evidence it had. Every TDWR volume in the
//! archive older than its site's switch therefore placed no radar at all, and
//! the whole terminal network was untestable through any path that needs a
//! position.
//!
//! The reading is in `nexrad-decode`'s `VolumeDataBlock`, which is the one seam
//! all four consumers of those fields pass through. This file is the check that
//! it reaches a real volume: bytes off the archive, through
//! [`decode_chunk`](rustdar_radar::chunks::decode_chunk), to the position the
//! renderer would place the radar at.
//!
//! # The fixtures
//!
//! `testdata/` holds twelve files, each a real archive cut down to its first
//! Message 31: the volume's own 24-byte header verbatim, then that one message
//! verbatim, re-framed as a single LDM record. Nothing in the message is
//! synthesised — the Volume Data Block under test is the radar's own bytes —
//! and the position is restated identically on every radial, so the first one
//! is the whole of what a position test needs. The largest is 2.6 KB.
//!
//! Five TDWR sites, each with one volume from either side of the producer's
//! correction, and two WSR-88D volumes as the control. The five span 23° of
//! latitude and 31° of longitude and include the one US site that is not in
//! CONUS, because a scale rule arbitrated on one radar is a scale rule fitted
//! to one radar. `TMIA` and `TPHX` were the holdout, checked the same way and
//! not committed.
//!
//! # Where "true coordinates" come from
//!
//! Not from another Level II volume, which would make the check circular, and
//! not from a table in this repository, which was itself read out of volumes.
//! From the **Level III** product stream for the same radars: a separate
//! product, separately generated, whose radar position is a documented
//! thousandths field nobody has to guess at. Read off the `NCR` products of
//! 2026-08-12 with `nexrad_level3`'s own header decoder.
//!
//! That the two agree at all is the finding — the pre-correction Level II
//! producer was writing the Level III field, so recovering the Level III number
//! from it is exactly the claim being made.

use rustdar_radar::site_position::SitePosition;
use rustdar_radar::sites::distance_km;

/// A committed fixture, and the position the site's Level III products state.
struct Fixture {
    /// The archive key the fixture was cut from.
    name: &'static str,
    /// The trimmed archive.
    bytes: &'static [u8],
    /// What the radar writes in its Message 31 headers.
    icao: &'static str,
    /// Degrees north, from the site's Level III Product Description Block.
    truth_lat: f64,
    /// Degrees east, from the same.
    truth_lon: f64,
}

/// The five TDWR sites as they read **before their own switch** — thousandths
/// of a degree in a field declared in degrees. All five are 2020 volumes, which
/// is a year clear of the earliest switch any of them made.
const THOUSANDTHS: [Fixture; 5] = [
    Fixture {
        name: "TORD20200810_000135_V08",
        bytes: include_bytes!("../testdata/TORD20200810_000135_V08.first-message"),
        icao: "TORD",
        truth_lat: 41.797,
        truth_lon: -87.858,
    },
    Fixture {
        name: "TOKC20200810_000457_V08",
        bytes: include_bytes!("../testdata/TOKC20200810_000457_V08.first-message"),
        icao: "TOKC",
        truth_lat: 35.276,
        truth_lon: -97.510,
    },
    Fixture {
        name: "TDAL20200810_000145_V08",
        bytes: include_bytes!("../testdata/TDAL20200810_000145_V08.first-message"),
        icao: "TDAL",
        truth_lat: 32.926,
        truth_lon: -96.968,
    },
    Fixture {
        name: "TPIT20200810_000554_V08",
        bytes: include_bytes!("../testdata/TPIT20200810_000554_V08.first-message"),
        icao: "TPIT",
        truth_lat: 40.501,
        truth_lon: -80.486,
    },
    Fixture {
        name: "TSJU20200810_140750_V08",
        bytes: include_bytes!("../testdata/TSJU20200810_140750_V08.first-message"),
        icao: "TSJU",
        truth_lat: 18.474,
        truth_lon: -66.180,
    },
];

/// The same five sites **after** the correction, writing the degrees the ICD
/// asks for.
const DEGREES: [Fixture; 5] = [
    Fixture {
        name: "TORD20260812_000527_V08",
        bytes: include_bytes!("../testdata/TORD20260812_000527_V08.first-message"),
        icao: "TORD",
        truth_lat: 41.797,
        truth_lon: -87.858,
    },
    Fixture {
        name: "TOKC20260812_000531_V08",
        bytes: include_bytes!("../testdata/TOKC20260812_000531_V08.first-message"),
        icao: "TOKC",
        truth_lat: 35.276,
        truth_lon: -97.510,
    },
    Fixture {
        name: "TDAL20260812_000346_V08",
        bytes: include_bytes!("../testdata/TDAL20260812_000346_V08.first-message"),
        icao: "TDAL",
        truth_lat: 32.926,
        truth_lon: -96.968,
    },
    Fixture {
        name: "TPIT20260812_000525_V08",
        bytes: include_bytes!("../testdata/TPIT20260812_000525_V08.first-message"),
        icao: "TPIT",
        truth_lat: 40.501,
        truth_lon: -80.486,
    },
    Fixture {
        name: "TSJU20260812_000041_V08",
        bytes: include_bytes!("../testdata/TSJU20260812_000041_V08.first-message"),
        icao: "TSJU",
        truth_lat: 18.474,
        truth_lon: -66.180,
    },
];

/// The control: two WSR-88D volumes, one from either end of the same span, in
/// both Volume Data Block formats — `KAMX`'s 2020 volume is the 40-byte block
/// and `KTLX`'s is the 48-byte one.
const WSR88D: [Fixture; 2] = [
    Fixture {
        name: "KAMX20200810_000424_V06",
        bytes: include_bytes!("../testdata/KAMX20200810_000424_V06.first-message"),
        icao: "KAMX",
        truth_lat: 25.611_08,
        truth_lon: -80.412_67,
    },
    Fixture {
        name: "KTLX20260811_000049_V06",
        bytes: include_bytes!("../testdata/KTLX20260811_000049_V06.first-message"),
        icao: "KTLX",
        truth_lat: 35.333_36,
        truth_lon: -97.277_76,
    },
];

/// One thousandth of a degree of latitude, in kilometres — the finest the
/// pre-correction producer's field could express, and so the closest two
/// producers can be asked to agree.
const ONE_THOUSANDTH_KM: f64 = 0.112;

/// Decode a fixture to the position it states.
fn position_of(fixture: &Fixture) -> SitePosition {
    let contents = rustdar_radar::chunks::decode_chunk(fixture.name, fixture.bytes)
        .unwrap_or_else(|e| panic!("decoding {}: {e}", fixture.name));
    let site = contents
        .site
        .unwrap_or_else(|| panic!("{} carries no Volume Data Block", fixture.name));
    assert_eq!(
        site.identifier_string().trim(),
        fixture.icao,
        "{} names a different radar than the fixture claims",
        fixture.name
    );
    SitePosition::from_volume(&site).unwrap_or_else(|| {
        panic!(
            "{} states ({}, {}), which from_volume refuses as a place no radar is",
            fixture.name,
            site.latitude(),
            site.longitude(),
        )
    })
}

/// How far a decoded position is from the site's Level III one.
fn error_km(fixture: &Fixture) -> f64 {
    let position = position_of(fixture);
    distance_km(
        position.lat(),
        position.lon(),
        fixture.truth_lat,
        fixture.truth_lon,
    )
}

/// Every fixture places its radar where that radar's Level III products place
/// it.
///
/// The five thousandths rows are what fails without the scale being read: each
/// states a latitude in the tens of thousands of degrees, which `from_volume`
/// refuses outright, so `position_of` panics before it can measure anything.
#[test]
fn every_volume_places_its_radar_where_its_level_three_products_do() {
    for fixture in THOUSANDTHS.iter().chain(&DEGREES).chain(&WSR88D) {
        let error = error_km(fixture);
        assert!(
            error < ONE_THOUSANDTH_KM,
            "{} decoded {error:.4} km from where its Level III products put it",
            fixture.name
        );
    }
}

/// The two producers describe the same radar, and reading each in its own scale
/// lands on the same place.
///
/// This is the check that the scale is being *read* rather than fitted: the
/// two volumes of a pair share no bytes in these fields, and 4 of the 5 pairs
/// come out identical to the micro-degree. `TSJU` is the fifth, and it differs
/// by one thousandth of a degree of longitude — the old producer wrote
/// −66.179 where the current one and the Level III stream both say −66.180,
/// which is 106 m and the finest disagreement its field can express.
#[test]
fn both_producers_place_a_radar_alike() {
    let mut identical = Vec::new();
    for (old, new) in THOUSANDTHS.iter().zip(&DEGREES) {
        assert_eq!(old.icao, new.icao, "the two tables are not paired");
        let (before, after) = (position_of(old), position_of(new));
        let moved = distance_km(before.lat(), before.lon(), after.lat(), after.lon());
        assert!(
            moved <= ONE_THOUSANDTH_KM,
            "{} moved {moved:.4} km between {} and {}",
            old.icao,
            old.name,
            new.name
        );
        if (before.lat_udeg, before.lon_udeg) == (after.lat_udeg, after.lon_udeg) {
            identical.push(old.icao);
        }
    }
    assert_eq!(
        identical,
        ["TORD", "TOKC", "TDAL", "TPIT"],
        "which pairs agree exactly is a measurement, not a tolerance",
    );
}

/// A WSR-88D decodes to the exact integers it decoded to before any of this,
/// in both block formats.
///
/// Spelled as the micro-degrees rather than as a distance, because that is what
/// "unchanged" means here: a tolerance would pass a value that had been divided
/// by a thousand and multiplied back.
#[test]
fn a_wsr88d_position_is_the_integer_it_always_was() {
    let expected = [(25_611_084, -80_412_666), (35_333_363, -97_277_763)];
    for (fixture, (lat_udeg, lon_udeg)) in WSR88D.iter().zip(expected) {
        let position = position_of(fixture);
        assert_eq!(
            (position.lat_udeg, position.lon_udeg),
            (lat_udeg, lon_udeg),
            "{} moved",
            fixture.name
        );
    }
}

/// The heights are not part of this and do not move. A TDWR states one figure
/// twice, which is how [`SitePosition::heights_over`] tells the two instruments
/// apart, and it states the same figure in both producers.
#[test]
fn a_tdwr_states_one_height_twice_in_both_producers() {
    for (old, new) in THOUSANDTHS.iter().zip(&DEGREES) {
        let (before, after) = (position_of(old), position_of(new));
        assert_eq!(
            before.site_height_m, before.tower_height_m,
            "{} is a TDWR and states its height once",
            old.name
        );
        assert_eq!(
            (before.site_height_m, before.tower_height_m),
            (after.site_height_m, after.tower_height_m),
            "{} changed height between producers",
            old.icao
        );
    }

    for fixture in &WSR88D {
        let position = position_of(fixture);
        assert_ne!(
            position.site_height_m, position.tower_height_m,
            "{} is a WSR-88D and states a tower separately",
            fixture.name
        );
    }
}
