//! The launch that has never seen a radar — and the one that has seen two.

use crate::app::tests::headless;
use crate::platform_double::TestBridge;
use rustdar_kv::{KvStore, MemoryKvStore};
use rustdar_radar::catalogue::{CataloguePosition, SiteCatalogue};
use std::collections::BTreeMap;
use std::rc::Rc;

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
            elevation_m: 400,
        },
    );
    SiteCatalogue::union([ARRIVAL.to_string(), UNPLACEABLE.to_string()], &positions)
}

/// A catalogue naming one radar, for the tests that only need "something
/// arrived".
fn catalogue_naming(id: &str, lat_udeg: i32) -> SiteCatalogue {
    let mut positions = BTreeMap::new();
    positions.insert(
        id.to_string(),
        CataloguePosition {
            lat_udeg,
            lon_udeg: -141_000_000,
            elevation_m: 400,
        },
    );
    SiteCatalogue::union([id.to_string()], &positions)
}

fn landed(catalogue: Option<SiteCatalogue>) -> crate::channels::SiteCatalogueResponse {
    crate::channels::SiteCatalogueResponse { catalogue }
}

/// A store holding a UI config the app itself wrote, and no catalogue.
fn store_of_a_returning_user() -> Rc<MemoryKvStore> {
    let store = Rc::new(MemoryKvStore::default());
    let mut gui = rustdar_egui::Gui::new();
    gui.loop_speed_fps = 9.25;
    gui.save_ui_config(store.as_ref());
    assert!(
        store
            .load(crate::site_catalogue::SITE_CATALOGUE_KEY)
            .is_none(),
        "precondition: this user has a config and no catalogue, which is the \
         whole state under test",
    );
    store
}

/// **A returning user with no cached catalogue still gets the network, in
/// this session.**
#[test]
fn a_returning_user_without_a_catalogue_still_gets_the_network_this_session() {
    const RETURNING: &str = "ZZQD";
    let store = store_of_a_returning_user();
    let mut app = headless(TestBridge::desktop().with_store(Rc::clone(&store)));

    assert!(
        app.catalogue_pending,
        "a stored config and a table with rows in it must not be read as \
         'this install already knows the network'",
    );
    assert!(
        !rustdar_radar::sites::knows_site(RETURNING),
        "precondition: the radar may not be known yet, or this proves nothing",
    );

    app.channels
        .site_catalogue_sender
        .send(landed(Some(catalogue_naming(RETURNING, -32_000_000))))
        .unwrap();
    app.poll_site_catalogue();

    assert!(
        rustdar_radar::sites::get_radar_site(RETURNING).is_some(),
        "the catalogue must reach the live table without waiting for a reload",
    );
    assert!(
        !app.catalogue_pending,
        "and spending it is what puts every later catalogue back on the \
         ordinary next-launch path",
    );
}

/// **…and is not moved off the site they came back to.**
#[test]
fn a_returning_users_open_site_survives_the_catalogue_landing() {
    const RETURNING: &str = "ZZQH";
    let store = store_of_a_returning_user();
    let mut app = headless(
        TestBridge::desktop()
            .with_store(Rc::clone(&store))
            .with_timezone("America/Chicago"),
    );
    assert!(
        !app.site_hint_pending,
        "a launch that restored a config brought its own site",
    );
    let opened = app.gui.pane(0).expect("a pane exists").site().to_string();

    app.channels
        .site_catalogue_sender
        .send(landed(Some(catalogue_naming(RETURNING, -33_000_000))))
        .unwrap();
    app.poll_site_catalogue();

    assert_eq!(
        app.gui.pane(0).expect("a pane exists").site(),
        opened,
        "the catalogue may add radars around them; it may not move them",
    );
}

/// **A first launch applies its first catalogue in this session.**
#[test]
fn a_first_launch_adopts_its_first_catalogue_without_waiting_for_the_next_one() {
    let mut app = headless(TestBridge::desktop());
    app.catalogue_pending = true;
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
        !app.catalogue_pending,
        "the first catalogue spends it; every later one takes the ordinary \
         next-launch path",
    );
}

/// A failed fetch leaves both flags armed.
#[test]
fn a_failed_first_fetch_leaves_the_launch_still_waiting() {
    let mut app = headless(TestBridge::desktop());
    app.catalogue_pending = true;
    app.site_hint_pending = true;

    app.channels
        .site_catalogue_sender
        .send(landed(None))
        .unwrap();
    app.poll_site_catalogue();

    assert!(
        app.catalogue_pending,
        "nothing arrived, so nothing is spent"
    );
    assert!(
        app.site_hint_pending,
        "and the hint has nothing to run against"
    );
}

/// A catalogue this install already had does **not** get applied to the live
/// table.
#[test]
fn a_launch_that_already_knows_the_network_still_waits_for_the_next_one() {
    const LATER: &str = "ZZQC";
    const CACHED: &str = "ZZQG";
    let store = Rc::new(MemoryKvStore::default());
    crate::site_catalogue::store_if_changed(
        Some(store.as_ref() as &dyn KvStore),
        &SiteCatalogue::default(),
        &catalogue_naming(CACHED, -35_000_000),
    );
    let mut app = headless(TestBridge::desktop().with_store(Rc::clone(&store)));
    assert!(
        !app.catalogue_pending,
        "precondition: this app read a catalogue at startup, so it is not \
         waiting on a first one",
    );

    app.channels
        .site_catalogue_sender
        .send(landed(Some(catalogue_naming(LATER, -31_000_000))))
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

/// A catalogue that arrives is held even when it cannot be persisted.
#[test]
fn a_catalogue_that_cannot_be_persisted_is_still_applied_to_this_session() {
    const UNSTORABLE: &str = "ZZQF";
    let mut app = headless(TestBridge::desktop().without_kv());
    app.catalogue_pending = true;

    app.channels
        .site_catalogue_sender
        .send(landed(Some(catalogue_naming(UNSTORABLE, -34_000_000))))
        .unwrap();
    app.poll_site_catalogue();

    assert!(
        app.site_catalogue.contains(UNSTORABLE),
        "the fetched catalogue is this session's, whether or not it could be \
         written",
    );
    assert!(
        rustdar_radar::sites::get_radar_site(UNSTORABLE).is_some(),
        "and it reaches the table, which is what the user actually sees",
    );
}
