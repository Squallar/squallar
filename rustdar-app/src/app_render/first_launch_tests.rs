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
            network: None,
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
            network: None,
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

// ── The volume decoded before its radar was known ───────────────────

/// The moment a volume in these tests was collected.
fn volume_time() -> chrono::NaiveDateTime {
    chrono::NaiveDate::from_ymd_opt(2026, 8, 20)
        .unwrap()
        .and_hms_opt(19, 41, 36)
        .unwrap()
}

/// A volume that states where its radar is, the way a Message 31 volume does.
fn volume_stating(id: &[u8; 4], lat: f32, lon: f32) -> nexrad_model::data::Scan {
    use nexrad_model::data::{PulseWidth, Scan, VolumeCoveragePattern};
    Scan::with_site(
        nexrad_model::meta::Site::new(*id, lat, lon, 370, 20),
        VolumeCoveragePattern::new(
            212,
            0,
            0.5,
            PulseWidth::Short,
            false,
            0,
            false,
            0,
            false,
            false,
            0,
            false,
            false,
            Vec::new(),
        ),
        Vec::new(),
    )
}

/// **The volume that teaches the table where its radar is can name it too.**
///
/// Measured on a fresh config tree, 2026-08-20: the first volume was fetched
/// and decoded and never rasterised, because the position it states reaches
/// the site table one line *after* the `ScanInfo` that has to name the radar
/// is built — so the info said `UNKNOWN`, and `dispatch_pane_renders` looks
/// the volume up in a still store keyed by the site.
#[test]
fn a_volume_that_places_its_own_radar_names_it_in_the_same_breath() {
    const TAUGHT: &str = "ZZQJ";
    let mut app = headless(TestBridge::desktop());
    assert!(
        !rustdar_radar::sites::knows_site(TAUGHT),
        "precondition: nothing may place this radar yet, or this proves nothing",
    );

    let info = app.scan_info_learning_position(
        &volume_stating(b"ZZQJ", -30.5, -140.5),
        TAUGHT,
        volume_time(),
    );

    assert_eq!(
        info.site_source,
        rustdar_radar::site_position::SitePositionSource::Volume,
        "precondition: the volume has to be the thing that placed it, or this \
         pins some other path",
    );
    assert_eq!(
        info.site.name, TAUGHT,
        "the volume's own position reached the table in this same call, so the \
         info it produced must be able to name the radar; UNKNOWN is not a key \
         the still store holds and the picture is never made",
    );
}

/// **A volume decoded before the catalogue landed is drawn once it lands.**
///
/// The other half of the same first-launch defect: a volume that states no
/// position of its own cannot place its radar, so nothing renders — correctly,
/// because a row at (0, 0) would be worse than no picture. The catalogue's
/// arrival is the moment that can un-skip it, and before this it did not:
/// 0 renders in 83 s on a fresh tree, against 15 within a second on a launch
/// that had a cached catalogue.
#[test]
fn the_catalogue_landing_draws_the_volume_the_launch_could_not_place() {
    const BLIND: &str = "ZZQK";
    let (recorded, _worker) = super::one_render_per_sweep_tests::recorder();
    let ctx = egui::Context::default();
    let mut app = headless(TestBridge::desktop());
    app.catalogue_pending = true;
    app.render.ensure_pane_count(1);
    assert!(
        !rustdar_radar::sites::knows_site(BLIND),
        "precondition: nothing may place this radar yet, or this proves nothing",
    );

    let scan = super::one_render_per_sweep_tests::sample_scan();
    let info = app.scan_info_learning_position(&scan, BLIND, volume_time());
    assert_eq!(
        info.site.name,
        rustdar_radar::sites::UNKNOWN_SITE_NAME,
        "precondition: a volume stating nothing about an unplaced radar is the \
         state under test",
    );
    {
        let pane = app.gui.pane_mut(0).expect("a pane exists");
        pane.set_site(BLIND.to_string());
        pane.set_selected_product(rustdar_radar::fields::known::REFLECTIVITY);
        pane.set_selected_elevation(0.5);
    }
    app.gui
        .apply(rustdar_egui::shell_api::GuiEvent::ScanInfoForSite {
            site: BLIND.to_string(),
            info,
        });
    app.volumes.install_still(
        BLIND.to_string(),
        (
            scan,
            std::sync::Arc::new(rustdar_radar::nyquist::DeclaredNyquist::empty()),
        ),
    );

    app.dispatch_pane_renders(&ctx);
    assert_eq!(
        super::one_render_per_sweep_tests::asked_for(&recorded),
        Vec::new(),
        "precondition: a radar nothing can place has no picture to make",
    );

    app.channels
        .site_catalogue_sender
        .send(landed(Some(catalogue_naming(BLIND, -36_000_000))))
        .unwrap();
    app.poll_site_catalogue();
    app.dispatch_pane_renders(&ctx);

    assert_eq!(
        super::one_render_per_sweep_tests::asked_for(&recorded),
        vec![(rustdar_radar::types::RadarProduct::Reflectivity, 0.5)],
        "the volume was already fetched and decoded; the catalogue's arrival \
         has to un-skip the render it made impossible, or the map stays blank \
         for the whole session",
    );
}

/// The timezone hint has the same shape of defect as the location-fix upgrade: it asks
/// **pane 0** whether the open would be a no-op, then opens the **active** pane. Two panes
/// are enough for those to disagree, and the hint is spent on entry (`site_hint_pending` is
/// cleared before the check) whether or not the pane it names ever moves.
#[test]
fn the_timezone_hint_opens_the_active_pane_even_when_pane_zero_is_already_there() {
    use rustdar_egui::UI_CONFIG_KEY;

    let mut app = headless(TestBridge::desktop().with_timezone("America/Chicago"));
    let hinted = crate::location_hint::site_for_timezone("America/Chicago")
        .expect("America/Chicago names a radar, which is what the hint is for");

    // Pane 0 is already the hint's radar; the active pane is a different one.
    let store = MemoryKvStore::default();
    store
        .store(
            UI_CONFIG_KEY,
            &format!(
                r#"{{"pane_count":2,"active_pane":1,"site":"{hinted}",
                     "panes":[{{"site":"{hinted}"}},{{"site":"KDLH"}}]}}"#
            ),
        )
        .expect("the memory store always accepts a write");
    assert!(
        app.gui.load_ui_config(&store),
        "the two-pane fixture config did not parse"
    );
    app.render.ensure_pane_count(2);
    assert_eq!(
        (
            app.gui.active_pane_idx(),
            app.gui.pane(0).expect("a pane exists").site()
        ),
        (1, hinted),
        "precondition: pane 0 on the hint's radar, and the active pane elsewhere",
    );

    app.site_hint_pending = true;
    app.open_on_the_timezones_radar();

    assert_eq!(
        app.gui.pane(1).expect("the fixture built two panes").site(),
        hinted,
        "the hint opened nothing: the no-op check asked pane 0, which was \
         already there, so the pane the switch names never moved",
    );
}
