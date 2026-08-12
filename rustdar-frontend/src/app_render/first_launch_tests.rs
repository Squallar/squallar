//! The launch that has never seen a radar.
//!
//! With no compiled-in table there is a state the application never used to
//! be able to reach: nothing cached, nothing learned, nothing carried, and so
//! no site list, no marker and nothing for the timezone hint to resolve
//! against. These pin what happens next.

use crate::app::tests::headless;
use crate::platform_double::TestBridge;
use rustdar_radar::catalogue::{CataloguePosition, SiteCatalogue};
use std::collections::BTreeMap;

/// An identifier nothing else in this crate places, so a sibling test cannot
/// supply the row these tests are about.
const ARRIVAL: &str = "ZZQA";
/// A second one, listed by the bucket and unplaceable by the NWS — the `TPBI`
/// and `KCRI` shape.
const UNPLACEABLE: &str = "ZZQB";

/// A catalogue naming both, with a position for only one.
fn fetched() -> SiteCatalogue {
    let mut positions = BTreeMap::new();
    positions.insert(
        ARRIVAL.to_string(),
        CataloguePosition {
            lat_udeg: -30_000_000,
            lon_udeg: -140_000_000,
            feedhorn_m: 400,
        },
    );
    SiteCatalogue::union(
        [ARRIVAL.to_string(), UNPLACEABLE.to_string()],
        &positions,
    )
}

fn landed(catalogue: Option<SiteCatalogue>) -> crate::channels::SiteCatalogueResponse {
    crate::channels::SiteCatalogueResponse { catalogue }
}

/// **A first launch applies its first catalogue in this session.**
///
/// The rule everywhere else is that a fetched catalogue is written to the
/// cache and applied on the *next* launch, so a marker cannot move under a
/// user already looking at it. A launch that knows no radars has no marker,
/// no site-list row and no height datum to move, and waiting would mean a
/// first run that looks broken and a second that works.
///
/// Fails on revert in the way that matters: with the in-session apply removed,
/// neither identifier is in the table after the poll, however many frames run.
#[test]
fn a_first_launch_adopts_its_first_catalogue_without_waiting_for_the_next_one() {
    let mut app = headless(TestBridge::desktop());
    // What `App::with_instance` computes on a genuinely fresh install. Set
    // here because the process-wide table is shared with every sibling test,
    // which has certainly placed radars into it by now — the flag is the
    // condition under test, not the emptiness that produces it.
    app.site_hint_pending = true;
    assert!(
        rustdar_radar::sites::get_radar_site(ARRIVAL).is_none()
            && !rustdar_radar::sites::knows_site(UNPLACEABLE),
        "precondition: neither radar may be known yet, or this proves nothing",
    );

    app.channels
        .site_catalogue_sender
        .send(landed(Some(fetched())))
        .unwrap();
    app.poll_site_catalogue();

    let row = rustdar_radar::sites::get_radar_site(ARRIVAL)
        .expect("the catalogue's placed radar must reach the live table now");
    assert_eq!((row.lat, row.lon), (-30.0, -140.0));
    assert!(
        rustdar_radar::sites::unplaced().contains(&UNPLACEABLE),
        "and the one it cannot place must reach the site list all the same",
    );

    assert!(
        !app.site_hint_pending,
        "the first catalogue spends it; every later one takes the ordinary \
         next-launch path",
    );
}

/// A failed fetch leaves the flag armed.
///
/// Offline is not an error state — it is a launch that runs on the cache — and
/// on a fresh install the cache is empty, so the honest outcome is a session
/// with no radars and a flag the next launch can still spend. Clearing it here
/// would mean an install that was offline once could never adopt a catalogue
/// in-session again.
#[test]
fn a_failed_first_fetch_leaves_the_launch_still_waiting() {
    let mut app = headless(TestBridge::desktop());
    app.site_hint_pending = true;

    app.channels.site_catalogue_sender.send(landed(None)).unwrap();
    app.poll_site_catalogue();

    assert!(app.site_hint_pending, "nothing arrived, so nothing is spent");
}

/// An ordinary launch does **not** take a fetched catalogue into the live
/// table.
///
/// The counterweight, and the property the whole next-launch policy exists
/// for: with radars already on screen, a catalogue landing mid-session must
/// change the cache and nothing else, or a marker moves under the user.
#[test]
fn a_launch_that_already_knows_radars_still_waits_for_the_next_one() {
    const LATER: &str = "ZZQC";
    let mut app = headless(TestBridge::desktop());
    assert!(
        !app.site_hint_pending,
        "precondition: this app knows radars already",
    );

    let mut positions = BTreeMap::new();
    positions.insert(
        LATER.to_string(),
        CataloguePosition {
            lat_udeg: -31_000_000,
            lon_udeg: -141_000_000,
            feedhorn_m: 400,
        },
    );
    app.channels
        .site_catalogue_sender
        .send(landed(Some(SiteCatalogue::union(
            [LATER.to_string()],
            &positions,
        ))))
        .unwrap();
    app.poll_site_catalogue();

    assert!(
        !rustdar_radar::sites::knows_site(LATER),
        "a catalogue landing on a running app must not add a marker to it",
    );
    assert!(
        app.site_catalogue.contains(LATER),
        "it is cached for the next launch, which is the whole point",
    );
}
