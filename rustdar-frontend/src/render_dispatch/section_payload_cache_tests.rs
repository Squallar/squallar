use super::*;
use rustdar_egui::pane::{GeoPoint, SectionLine, SectionTarget, VolumeStamp};

fn at(minute: u32) -> chrono::NaiveDateTime {
    chrono::NaiveDate::from_ymd_opt(2026, 7, 30)
        .unwrap()
        .and_hms_opt(18, minute, 0)
        .unwrap()
}

fn target(site: &str, minute: u32, product: RadarProduct, ladder: u64) -> SectionTarget {
    SectionTarget {
        volume: VolumeStamp {
            site: site.to_owned(),
            collected: at(minute),
        },
        product,
        line: SectionLine::new(
            GeoPoint {
                lat: 35.3,
                lon: -97.3,
            },
            GeoPoint {
                lat: 35.6,
                lon: -97.0,
            },
        )
        .expect("a valid line"),
        ladder,
    }
}

/// The payload cache reuses on the four things the payload actually depends
/// on, and the **line is not one of them**.
///
/// That asymmetry is the cache's whole reason to exist: moving the line is
/// the commonest way to want another cut, and it is the one case that must
/// *not* pay for a 15.6 MB re-extraction.
#[test]
fn a_payload_is_reused_for_another_line_through_the_same_volume() {
    let base = target("KTLX", 30, RadarProduct::Reflectivity, 9);
    let key = SectionInputKey::of(&base);

    let mut elsewhere = base.clone();
    elsewhere.line = SectionLine::new(
        GeoPoint {
            lat: 34.0,
            lon: -99.0,
        },
        GeoPoint {
            lat: 36.9,
            lon: -95.1,
        },
    )
    .expect("a valid line");
    assert_ne!(elsewhere, base, "precondition: the line really moved");
    assert_eq!(
        SectionInputKey::of(&elsewhere),
        key,
        "a redrawn line re-extracted the whole volume"
    );
}

/// Every input the payload *does* depend on invalidates it, including the
/// ladder fingerprint — which on the live feed is the only one that moves.
#[test]
fn a_payload_is_not_reused_across_volume_site_moment_or_ladder() {
    let base = target("KTLX", 30, RadarProduct::Reflectivity, 9);
    let key = SectionInputKey::of(&base);

    for (other, why) in [
        (
            target("KTLX", 36, RadarProduct::Reflectivity, 9),
            "a payload of the previous volume",
        ),
        (
            target("KOUN", 30, RadarProduct::Reflectivity, 9),
            "a payload projected around another site",
        ),
        (
            target("KTLX", 30, RadarProduct::Velocity, 9),
            "extract_volume narrows to one moment, so this is an empty ladder",
        ),
        (
            target("KTLX", 30, RadarProduct::Reflectivity, 1),
            "the live-feed case: the same volume, one rung-choice ago",
        ),
    ] {
        assert_ne!(SectionInputKey::of(&other), key, "{why}");
    }
}
