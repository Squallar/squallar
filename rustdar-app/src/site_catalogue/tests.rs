use super::*;
use rustdar_kv::MemoryKvStore;
use rustdar_radar::catalogue::CataloguePosition;
use std::collections::BTreeMap;

fn placed(lat_udeg: i32) -> CataloguePosition {
    CataloguePosition {
        lat_udeg,
        lon_udeg: -97_000_000,
        elevation_m: 400,
    }
}

/// A catalogue with one placed member and one the NWS could not place — the
/// two states the union produces, both of which have to survive the cache.
fn catalogue() -> SiteCatalogue {
    let positions = BTreeMap::from([("KTLX".to_owned(), placed(35_333_340))]);
    SiteCatalogue::union(["KTLX".to_owned(), "TPBI".to_owned()], &positions)
}

/// The round trip, through a real store rather than through the struct's own
/// fields.
///
/// The write is the interesting half: it must have happened by the time
/// [`store_if_changed`] returns, not on some later tick. The session that
/// matters most is the one that ends immediately afterwards, and the whole
/// point of the cache is that the *next* launch has it.
#[test]
fn a_fetched_catalogue_is_written_at_once_and_read_back_next_run() {
    let store = MemoryKvStore::default();
    let fetched = catalogue();

    assert!(store_if_changed(
        Some(&store),
        &SiteCatalogue::default(),
        &fetched
    ));
    assert!(
        store.load(SITE_CATALOGUE_KEY).is_some(),
        "the write must be synchronous, not deferred to an autosave tick",
    );

    let next_run = load(Some(&store));
    assert_eq!(next_run, fetched);
    assert_eq!(next_run.position("KTLX"), Some(placed(35_333_340)));
    assert!(
        next_run.contains("TPBI") && next_run.position("TPBI").is_none(),
        "the unplaced member has to survive: dropping it turns the union \
         into an intersection and takes TPBI and KCRI off the map",
    );
}

/// Nothing floating-point ever reaches the blob, so nothing in it can be
/// `null` on the way back in — except the one `null` that means "unplaced",
/// which is a value and not a failure.
///
/// A non-finite `f64` is written by `serde_json` as `null` and then fails to
/// deserialize on the *next* load, which costs the whole catalogue a run after
/// the bug that produced it.
#[test]
fn the_persisted_form_is_integers() {
    let store = MemoryKvStore::default();
    store_if_changed(Some(&store), &SiteCatalogue::default(), &catalogue());

    let raw = store.load(SITE_CATALOGUE_KEY).expect("just written");
    assert!(
        raw.contains("35333340"),
        "micro-degrees, not degrees: {raw}"
    );
    assert!(!raw.contains('.'), "no decimal point anywhere: {raw}");
    assert!(
        raw.contains(r#""TPBI":null"#),
        "and `null` is the unplaced member, deliberately: {raw}",
    );
}

/// Rewriting an unchanged catalogue is skipped.
///
/// The ordinary case is a launch whose fetch confirms what is already cached,
/// and ~15 KB into `localStorage` per launch for a value that has not moved is
/// a cost with no benefit.
#[test]
fn an_unchanged_catalogue_is_not_rewritten() {
    let store = MemoryKvStore::default();
    let cached = catalogue();
    assert!(store_if_changed(
        Some(&store),
        &SiteCatalogue::default(),
        &cached
    ));
    assert!(
        !store_if_changed(Some(&store), &cached, &catalogue()),
        "the same catalogue twice must not write twice",
    );
}

/// **A failed fetch degrades to the cache**, silently.
///
/// This is the offline path and the one that must not be allowed to rot: the
/// fetch answers `None`, nothing is written, and the next `load` still returns
/// everything the last successful fetch left behind. The alternative — writing
/// an empty catalogue over a good one — would cost every position on a launch
/// that happened to start in a tunnel.
#[test]
fn a_failed_fetch_leaves_the_cache_intact() {
    let store = MemoryKvStore::default();
    let cached = catalogue();
    store_if_changed(Some(&store), &SiteCatalogue::default(), &cached);

    // What `poll_site_catalogue` does with `SiteCatalogueResponse { catalogue:
    // None }`: nothing at all.
    let failed: Option<SiteCatalogue> = None;
    if let Some(fetched) = failed {
        store_if_changed(Some(&store), &cached, &fetched);
    }

    assert_eq!(load(Some(&store)), cached);
    assert!(load(Some(&store)).contains("KTLX"));
}

/// An unreadable blob degrades to nothing rather than propagating.
///
/// The cost is one session on the compiled-in seed, which is where the app was
/// before any of this existed, and the launch's fetch replaces it.
#[test]
fn an_unreadable_blob_is_dropped() {
    for raw in ["", "not json", "[1,2,3]", r#"{"KTLX":{"lat_udeg":1}}"#] {
        let store = MemoryKvStore::default();
        store
            .store(SITE_CATALOGUE_KEY, raw)
            .expect("the double's store cannot fail");
        assert!(load(Some(&store)).is_empty(), "{raw:?}");
    }
}

/// An implausibly large blob is refused rather than loaded.
///
/// `localStorage` is a few megabytes for the whole origin, and a catalogue four
/// times the size of the real network is junk rather than growth.
#[test]
fn an_absurdly_large_catalogue_is_refused() {
    let store = MemoryKvStore::default();
    let positions = BTreeMap::new();
    let huge = SiteCatalogue::union(
        (0..MAX_CATALOGUE_SITES + 1).map(|n| format!("Z{n:03}")),
        &positions,
    );
    store
        .store(
            SITE_CATALOGUE_KEY,
            &serde_json::to_string(&huge).expect("serializes"),
        )
        .expect("the double's store cannot fail");

    assert!(load(Some(&store)).is_empty());
}

/// No store — the web build before `localStorage` is reachable, and Android
/// before `set_config_dir` — is an empty catalogue and a dropped write, not a
/// panic.
#[test]
fn no_store_is_not_an_error() {
    assert!(load(None).is_empty());
    assert!(!store_if_changed(
        None,
        &SiteCatalogue::default(),
        &catalogue()
    ));
}
