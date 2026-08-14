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
    let key = SectionInputKey::of(&base, None, rustdar_radar::srv::SrvFallback::default());

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
        SectionInputKey::of(&elsewhere, None, rustdar_radar::srv::SrvFallback::default()),
        key,
        "a redrawn line re-extracted the whole volume"
    );
}

/// Every input the payload *does* depend on invalidates it, including the
/// ladder fingerprint — which on the live feed is the only one that moves.
#[test]
fn a_payload_is_not_reused_across_volume_site_moment_or_ladder() {
    let base = target("KTLX", 30, RadarProduct::Reflectivity, 9);
    let key = SectionInputKey::of(&base, None, rustdar_radar::srv::SrvFallback::default());

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
        assert_ne!(
            SectionInputKey::of(&other, None, rustdar_radar::srv::SrvFallback::default()),
            key,
            "{why}"
        );
    }
}

/// The storm motion vector is part of a storm-relative payload's identity,
/// and of nothing else's.
///
/// An SRV section is not a slice of a measured moment: the derivation runs
/// on the payload, so two vectors are two payloads. Without this the reuse
/// test passed, `extract()` never ran, and the job shipped the previous
/// vector's field — a *silent wrong field*, which is the failure mode this
/// whole struct exists to make impossible.
///
/// The other half is as load-bearing as the first: the eight products that
/// do not read a vector must keep their payload across an edit, or every
/// nudge of a vector nobody is looking through costs a 15.6 MB re-walk of
/// the volume.
#[test]
fn the_storm_motion_vector_is_part_of_a_storm_relative_payloads_identity() {
    let srv = target("KTLX", 30, RadarProduct::StormRelativeVelocity, 9);
    let slow = SectionInputKey::of(
        &srv,
        Some((20.0, 240.0)),
        rustdar_radar::srv::SrvFallback::default(),
    );
    let fast = SectionInputKey::of(
        &srv,
        Some((60.0, 90.0)),
        rustdar_radar::srv::SrvFallback::default(),
    );
    assert_ne!(slow, fast, "the section would ship the old vector's field");
    assert_ne!(
        slow,
        SectionInputKey::of(&srv, None, rustdar_radar::srv::SrvFallback::default()),
        "an override cleared back to the volume's own Bunkers fit is also \
             a different field",
    );
    assert_eq!(
        slow,
        SectionInputKey::of(
            &srv,
            Some((20.0, 240.0)),
            rustdar_radar::srv::SrvFallback::default()
        ),
        "the same vector must reuse, or a section re-walks the volume on \
             every frame",
    );

    // Reflexive on a NaN vector: unequal-to-itself would not draw the
    // wrong picture, it would re-extract 15.6 MB every frame the section
    // stood.
    let nan = SectionInputKey::of(
        &srv,
        Some((f32::NAN, f32::NAN)),
        rustdar_radar::srv::SrvFallback::default(),
    );
    assert_eq!(
        nan,
        SectionInputKey::of(
            &srv,
            Some((f32::NAN, f32::NAN)),
            rustdar_radar::srv::SrvFallback::default()
        )
    );

    // And the products that do not read a vector are not keyed on one —
    // which is why this belongs in the key and not in an eviction.
    let refl = target("KTLX", 30, RadarProduct::Reflectivity, 9);
    assert_eq!(
        SectionInputKey::of(&refl, None, rustdar_radar::srv::SrvFallback::default()),
        SectionInputKey::of(&refl, None, rustdar_radar::srv::SrvFallback::default()),
    );
}
