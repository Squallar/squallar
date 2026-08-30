//! The detail vocabulary and the generation fact.
//!
//! [`AreaMaintenance`](super::AreaMaintenance) itself is exercised through the
//! screen it serves (`ui_offline_areas/tests.rs`), against a real
//! [`FsSegmentStore`](crate::basemap_download::FsSegmentStore) — a worker
//! tested against a double would be tested against the belief it was written
//! from.

use super::*;

/// The generation the published archive currently carries, as
/// `generation_for_url` encodes it. Written out rather than derived, so a
/// change to either the URL or the encoding shows up here as a diff rather
/// than as two sides moving together.
const AUGUST_2026: &str = "basemap_2Fomt-20260828.pmtiles";
const SEPTEMBER_2026: &str = "basemap_2Fomt-20260904.pmtiles";

/// The levels are named by what you can make out, and a depth between two
/// named ones gets the shallower claim rather than the flattering one.
#[test]
fn a_detail_label_never_promises_more_than_the_depth_holds() {
    assert_eq!(detail_label(10), "Cities and highways");
    assert_eq!(detail_label(12), "Towns and main roads");
    assert_eq!(detail_label(14), "Every street");
    // Between levels: z11 holds less than z12, so it reads as the level it
    // actually reaches.
    assert_eq!(detail_label(11), "Towns and main roads");
    assert_eq!(detail_label(0), "Cities and highways");
    // Past the deepest published zoom there is no more data to describe, so
    // the deepest label stands rather than a fourth one being invented.
    assert_eq!(detail_label(20), "Every street");

    for &(_, label) in DETAIL_LEVELS {
        assert!(
            !label.chars().any(|c| c.is_ascii_digit()),
            "a level label may not carry a number: {label:?} - the vocabulary \
             is what you can make out, never a zoom",
        );
    }
}

/// The generation the app reads is derived from the shipped archive URL, not
/// pasted here — this is the one place the two must agree, and it is what
/// makes the constants above meaningful.
#[test]
fn the_shipped_archive_dates_to_the_generation_these_tests_use() {
    let live =
        crate::basemap_archive::block_cache::generation_for_url(crate::tiles::BASEMAP_ARCHIVE_URL);
    assert!(
        generation_month(&live).is_some(),
        "the shipped archive URL yields {live:?}, which carries no date - the \
         manage screen can state no vintage for anything downloaded from it",
    );
    assert_eq!(
        generation_month(&live),
        generation_month(AUGUST_2026),
        "the shipped archive moved off the generation these tests pin"
    );
}

/// An area cut from the archive the app reads states its vintage and nothing
/// more: there is no update to offer.
#[test]
fn a_current_area_states_its_vintage_and_offers_no_update() {
    let note = generation_note(AUGUST_2026, AUGUST_2026).expect("a dated generation");
    assert_eq!(note.vintage, "August 2026");
    assert!(!note.update_available);
    assert_eq!(note.line(), "Map data August 2026");
}

/// The step-11 case: an older cut stays usable and says so as a fact beside
/// an offer, never as a warning.
#[test]
fn an_older_area_states_its_vintage_and_that_an_update_exists() {
    let note = generation_note(AUGUST_2026, SEPTEMBER_2026).expect("a dated generation");
    assert_eq!(note.vintage, "August 2026");
    assert!(note.update_available);
    assert_eq!(note.line(), "Map data August 2026 \u{b7} update available");

    let line = note.line().to_ascii_lowercase();
    for scold in [
        "expired",
        "out of date",
        "outdated",
        "stale",
        "invalid",
        "no longer",
    ] {
        assert!(
            !line.contains(scold),
            "the generation line reads {:?}, which calls a usable download {scold} - \
             a sub-archive carries its own directories and an older cut stays valid",
            note.line(),
        );
    }
}

/// A difference this cannot order is not an update. The live archive only ever
/// moves forward in practice, but "update available" beside a *newer* stored
/// area would be plainly false, so the claim is made only on a strict
/// ordering.
#[test]
fn a_newer_stored_area_is_not_told_an_update_is_available() {
    let note = generation_note(SEPTEMBER_2026, AUGUST_2026).expect("a dated generation");
    assert_eq!(note.vintage, "September 2026");
    assert!(!note.update_available);
}

/// A record written before generations were stored carries an empty string,
/// and nothing is claimed about it — an unsourced vintage is not a fact, and a
/// line the reader cannot trust is worse than no line.
#[test]
fn an_undated_generation_states_nothing() {
    assert_eq!(generation_note("", AUGUST_2026), None);
    assert_eq!(
        generation_note("basemap_2Fplanet.pmtiles", AUGUST_2026),
        None
    );
    // A build hash whose digits do not form a plausible date is not read as
    // one: `4ca64469750e` holds eight consecutive digits.
    assert_eq!(generation_month("terrain_2F4ca64469750e"), None);
    // And where a hash and a real date sit in one path, the date is found.
    assert_eq!(
        generation_month("terrain_2F4ca64469750e-20260829"),
        Some((2026, 8))
    );
}
