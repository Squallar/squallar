//! The launch that has never seen a radar — and the one that has seen two.
//!
//! With no compiled-in table there is a state the application never used to
//! be able to reach: nothing cached, nothing learned, nothing carried, and so
//! no site list, no marker and nothing for the timezone hint to resolve
//! against. These pin what happens next.
//!
//! # The state that shipped broken
//!
//! The in-session apply used to be gated on `!restored && table.rows()
//! .is_empty()` — "is this install new" — and a returning user fails both
//! halves: they have a stored config, and a position learned off a volume they
//! once opened places a row. The catalogue was filed for the next launch and
//! the session ran on the two radars it could place. Measured in headless
//! Chromium against the built PWA: `site catalogue cached: 203 radars` with no
//! in-session apply, and the full network one reload later.
//!
//! Every browser session that has ever run this app is that state after an
//! upgrade from a build without a catalogue, which is why it was reported from
//! the web and not from the desktop. Nothing about it is wasm-specific: the
//! predicate is in this crate and compiles identically for every target, which
//! is what lets these tests pin it on the host.
//!
//! So the gate is now "can the table answer yet" — [`App::catalogue_pending`],
//! asked of the *catalogue* — and the timezone hint keeps its own separate
//! gate, [`App::site_hint_pending`], asked of whether the user brought a site.

use crate::app::tests::headless;
use crate::platform_double::TestBridge;
use rustdar_egui::config_store::{ConfigStore, MemoryConfigStore};
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
///
/// Written through `save_ui_config` rather than as a literal blob, for the
/// reason `app::tests::seed_config` gives: a hand-written config stops matching
/// the format the moment it changes and then tests nothing. What matters here
/// is only that `load_ui_config` answers `true` — that is the `restored` half
/// of the predicate that used to suppress the catalogue.
fn store_of_a_returning_user() -> Rc<MemoryConfigStore> {
    let store = Rc::new(MemoryConfigStore::default());
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
///
/// The regression. `restored` is true and the process-wide table is not empty,
/// so the old `!restored && table.rows().is_empty()` gate was false twice over
/// and the catalogue went to the cache alone — leaving a site list of whatever
/// the install had decoded, with no way for the user to tell.
///
/// Drives [`crate::app::App::new`]'s own predicate rather than setting the flag
/// by hand, which is exactly what the previous tests here did and exactly why
/// this shipped green: the flag was pinned and the expression that computes it
/// was not.
///
/// Fails on revert: restore either conjunct and `catalogue_pending` comes back
/// false, so `ZZQD` never reaches the table.
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
///
/// The other half of the split, and the reason the catalogue apply and the
/// timezone hint are gated on different questions. A returning user brought a
/// site; a catalogue landing mid-session must add radars around it and must
/// not re-run the hint over the top of it.
#[test]
fn a_returning_users_open_site_survives_the_catalogue_landing() {
    // Not `ZZQE`: `app::tests` reserves that one and asserts nothing has placed
    // it. Every identifier here has to be unused process-wide, because the site
    // table these tests resolve into is shared by the whole binary.
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
    let opened = app.gui.pane(0).expect("a pane exists").site.clone();

    app.channels
        .site_catalogue_sender
        .send(landed(Some(catalogue_naming(RETURNING, -33_000_000))))
        .unwrap();
    app.poll_site_catalogue();

    assert_eq!(
        app.gui.pane(0).expect("a pane exists").site,
        opened,
        "the catalogue may add radars around them; it may not move them",
    );
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
///
/// Offline is not an error state — it is a launch that runs on the cache — and
/// on a fresh install the cache is empty, so the honest outcome is a session
/// with no radars and flags the next launch can still spend. Clearing them here
/// would mean an install that was offline once could never adopt a catalogue
/// in-session again.
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
///
/// The counterweight, and the property the whole next-launch policy exists
/// for: with the network already in hand, a catalogue landing mid-session must
/// change the cache and nothing else, or a marker moves under the user.
#[test]
fn a_launch_that_already_knows_the_network_still_waits_for_the_next_one() {
    const LATER: &str = "ZZQC";
    const CACHED: &str = "ZZQG";
    // A store that already holds a catalogue, written through the same
    // function the app writes one with. This is the launch the next-launch
    // policy is *for*, and it only exists if something was cached.
    let store = Rc::new(MemoryConfigStore::default());
    crate::site_catalogue::store_if_changed(
        Some(store.as_ref() as &dyn ConfigStore),
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
///
/// `store_if_changed` answers `false` for a browser with site data blocked, a
/// sandboxed iframe and a full `localStorage` alike. Taking that as "keep the
/// empty one" discarded a catalogue already in hand and then spent the flag on
/// it, leaving the session with no radars on exactly the platforms least able
/// to spare them. Persistence is how the next launch benefits; this one
/// benefits from the value it is holding.
#[test]
fn a_catalogue_that_cannot_be_persisted_is_still_applied_to_this_session() {
    const UNSTORABLE: &str = "ZZQF";
    let mut app = headless(TestBridge::desktop().without_config_store());
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
