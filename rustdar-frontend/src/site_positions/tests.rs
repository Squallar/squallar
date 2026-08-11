use super::*;
use rustdar_egui::config_store::MemoryConfigStore;

fn position(lat_udeg: i32, lon_udeg: i32) -> SitePosition {
    SitePosition {
        lat_udeg,
        lon_udeg,
        site_height_m: 370,
        tower_height_m: 20,
    }
}

/// The round trip, through a real store rather than through the struct's own
/// fields.
///
/// The write is the interesting half: it must have happened by the time
/// `learn` returns, not on some later tick, because the session that matters
/// most is the one that ends immediately afterwards.
#[test]
fn a_learned_position_is_written_at_once_and_read_back_next_run() {
    let store = MemoryConfigStore::default();
    let ktlx = position(35_333_060, -97_277_500);

    let mut learning = SitePositions::default();
    assert!(learning.learn(Some(&store), "KTLX", ktlx));

    // Already durable, with nothing else having run.
    assert!(
        store.load(SITE_POSITIONS_KEY).is_some(),
        "the write must be synchronous, not deferred to an autosave tick",
    );

    let next_run = SitePositions::load(Some(&store));
    assert_eq!(next_run.get("KTLX"), Some(ktlx));
    assert_eq!(next_run, learning);
    assert_eq!(next_run.len(), 1);
}

/// Nothing floating-point ever reaches the blob, so nothing in it can be
/// `null` on the way back in.
///
/// This is the failure this whole encoding exists to close: `serde_json`
/// writes a non-finite float as `null` and then refuses to read `null` back,
/// so one bad value would cost every *other* site's position on the next load
/// — a run later, with nothing pointing at the cause.
#[test]
fn the_persisted_blob_is_integers_all_the_way_down() {
    let store = MemoryConfigStore::default();
    let mut learning = SitePositions::default();
    learning.learn(Some(&store), "KTLX", position(35_333_060, -97_277_500));
    learning.learn(Some(&store), "KABR", position(45_455_830, -98_413_330));

    let raw = store.load(SITE_POSITIONS_KEY).expect("written");
    assert!(!raw.contains('.'), "{raw}");
    assert!(!raw.contains("null"), "{raw}");
    assert!(!raw.contains("NaN"), "{raw}");

    // And the serialized form is stable, so two runs that learn the same
    // things write the same bytes and the store is not rewritten for nothing.
    let mut other = SitePositions::default();
    other.learn(None, "KABR", position(45_455_830, -98_413_330));
    other.learn(None, "KTLX", position(35_333_060, -97_277_500));
    let other_store = MemoryConfigStore::default();
    other.persist(Some(&other_store));
    assert_eq!(other_store.load(SITE_POSITIONS_KEY), Some(raw));
}

/// A fresh volume wins outright, and a repeat of the same position writes
/// nothing.
///
/// Both halves matter. The first is the conflict policy — a disagreement means
/// a re-survey happened, not that one of the two readings is noise. The second
/// is what stops a `localStorage` write every five minutes per pane for a
/// value that has not moved.
#[test]
fn a_fresh_volume_wins_and_a_repeat_writes_nothing() {
    let store = MemoryConfigStore::default();
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

/// A blob that cannot be read costs this feature and nothing else.
///
/// The whole point of a separate key: the app degrades to the compiled-in
/// table, and every other setting the user has is untouched because it is not
/// in this blob.
#[test]
fn an_unreadable_blob_degrades_to_the_table_rather_than_failing() {
    let store = MemoryConfigStore::default();
    store.store(SITE_POSITIONS_KEY, "not json at all").unwrap();
    assert!(SitePositions::load(Some(&store)).is_empty());

    // Including the shape a float field would have produced.
    store
        .store(
            SITE_POSITIONS_KEY,
            r#"{"KTLX":{"lat_udeg":null,"lon_udeg":-97277500,"site_height_m":370,"tower_height_m":20}}"#,
        )
        .unwrap();
    assert!(SitePositions::load(Some(&store)).is_empty());

    // And a run with no store at all works, it just remembers nothing.
    let mut nowhere = SitePositions::default();
    assert!(nowhere.learn(None, "KTLX", position(35_333_060, -97_277_500)));
    assert_eq!(nowhere.get("KTLX"), Some(position(35_333_060, -97_277_500)));
    assert!(SitePositions::load(None).is_empty());
}

/// The map cannot grow without bound in a browser's `localStorage`.
///
/// Reaching the cap means something is wrong — it is four times the size of
/// the real network — so the response is to stop taking new sites and say so,
/// while every site already remembered keeps working and keeps updating.
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

    // A site already remembered is still corrected, which is the half that
    // must not be lost to the bound.
    assert!(learning.learn(None, "K000", position(36_000_000, -97_000_000)));
    assert_eq!(
        learning.get("K000"),
        Some(position(36_000_000, -97_000_000))
    );
}
