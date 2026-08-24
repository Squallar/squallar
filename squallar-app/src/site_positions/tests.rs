use super::*;
use squallar_kv::MemoryKvStore;

fn position(lat_udeg: i32, lon_udeg: i32) -> SitePosition {
    SitePosition {
        lat_udeg,
        lon_udeg,
        site_height_m: 370,
        tower_height_m: 20,
    }
}

#[test]
fn a_learned_position_is_written_at_once_and_read_back_next_run() {
    let store = MemoryKvStore::default();
    let ktlx = position(35_333_060, -97_277_500);

    let mut learning = SitePositions::default();
    assert!(learning.learn(Some(&store), "KTLX", ktlx));

    assert!(
        store.load(SITE_POSITIONS_KEY).is_some(),
        "the write must be synchronous, not deferred to an autosave tick",
    );

    let next_run = SitePositions::load(Some(&store));
    assert_eq!(next_run.get("KTLX"), Some(ktlx));
    assert_eq!(next_run, learning);
    assert_eq!(next_run.len(), 1);
}

#[test]
fn the_persisted_blob_is_integers_all_the_way_down() {
    let store = MemoryKvStore::default();
    let mut learning = SitePositions::default();
    learning.learn(Some(&store), "KTLX", position(35_333_060, -97_277_500));
    learning.learn(Some(&store), "KABR", position(45_455_830, -98_413_330));

    let raw = store.load(SITE_POSITIONS_KEY).expect("written");
    assert!(!raw.contains('.'), "{raw}");
    assert!(!raw.contains("null"), "{raw}");
    assert!(!raw.contains("NaN"), "{raw}");

    let mut other = SitePositions::default();
    other.learn(None, "KABR", position(45_455_830, -98_413_330));
    other.learn(None, "KTLX", position(35_333_060, -97_277_500));
    let other_store = MemoryKvStore::default();
    other.persist(Some(&other_store));
    assert_eq!(other_store.load(SITE_POSITIONS_KEY), Some(raw));
}

#[test]
fn a_fresh_volume_wins_and_a_repeat_writes_nothing() {
    let store = MemoryKvStore::default();
    let mut learning = SitePositions::default();

    let before = position(35_333_060, -97_277_500);
    let after = position(35_333_450, -97_277_500);

    assert!(learning.learn(Some(&store), "KTLX", before));
    assert!(
        !learning.learn(Some(&store), "KTLX", before),
        "an unchanged position must not be rewritten",
    );
    assert!(learning.learn(Some(&store), "KTLX", after));
    assert_eq!(learning.get("KTLX"), Some(after));
    assert_eq!(
        SitePositions::load(Some(&store)).get("KTLX"),
        Some(after),
        "the newer reading is what survives the restart",
    );
}

#[test]
fn an_unreadable_blob_degrades_to_the_table_rather_than_failing() {
    let store = MemoryKvStore::default();
    store.store(SITE_POSITIONS_KEY, "not json at all").unwrap();
    assert!(SitePositions::load(Some(&store)).is_empty());

    store
        .store(
            SITE_POSITIONS_KEY,
            r#"{"KTLX":{"lat_udeg":null,"lon_udeg":-97277500,"site_height_m":370,"tower_height_m":20}}"#,
        )
        .unwrap();
    assert!(SitePositions::load(Some(&store)).is_empty());

    let mut nowhere = SitePositions::default();
    assert!(nowhere.learn(None, "KTLX", position(35_333_060, -97_277_500)));
    assert_eq!(nowhere.get("KTLX"), Some(position(35_333_060, -97_277_500)));
    assert!(SitePositions::load(None).is_empty());
}

#[test]
fn the_number_of_remembered_sites_is_bounded() {
    let mut learning = SitePositions::default();
    for i in 0..MAX_REMEMBERED_SITES {
        assert!(learning.learn(None, &format!("K{i:03}"), position(35_000_000, -97_000_000)));
    }
    assert_eq!(learning.len(), MAX_REMEMBERED_SITES);

    assert!(!learning.learn(None, "KNEW", position(36_000_000, -97_000_000)));
    assert_eq!(learning.get("KNEW"), None);
    assert_eq!(learning.len(), MAX_REMEMBERED_SITES);

    assert!(learning.learn(None, "K000", position(36_000_000, -97_000_000)));
    assert_eq!(
        learning.get("K000"),
        Some(position(36_000_000, -97_000_000))
    );
}
