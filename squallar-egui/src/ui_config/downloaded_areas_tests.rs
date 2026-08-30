//! The persisted downloaded-area list: it round-trips whole, it is absent from
//! every config written before it, and nothing in it claims the bytes are
//! still there.

use super::*;
use crate::Gui;
use crate::basemap_download::{AreaSpec, AreaStatus, DownloadedArea, TerrainHold};
use squallar_kv::MemoryKvStore;
use squallar_units::DataSize;

/// A record exercising every field, with values none of which is a default.
fn oklahoma(area_id: &str) -> DownloadedArea {
    DownloadedArea {
        spec: AreaSpec {
            area_id: area_id.to_owned(),
            west: -98.25,
            south: 34.75,
            east: -96.50,
            north: 36.25,
            max_zoom: 12,
        },
        segments_expected: 7,
        bytes: DataSize::from_bytes(112_345_678),
        generation: "basemap_2Fomt-20260828.pmtiles".to_owned(),
        terrain: Some(TerrainHold {
            segments_expected: 2,
            bytes: DataSize::from_bytes(21_000_111),
            generation: "terrain_2F4ca64469750e-20260829".to_owned(),
        }),
    }
}

/// Reopen is exactly 1:1: every field of every area comes back, and a second
/// save is byte-identical to the first.
#[test]
fn downloaded_areas_round_trip_whole_and_reach_a_byte_identical_save() {
    let store = MemoryKvStore::default();
    let mut gui = Gui::new();
    gui.record_downloaded_area(oklahoma("ok-central"));
    // The second record holds no terrain, so the file carries one area of each
    // shape and the round trip proves both — an area with a hillshade and one
    // without, which is what a device that downloaded before terrain existed
    // has.
    gui.record_downloaded_area(DownloadedArea {
        segments_expected: 1,
        bytes: DataSize::from_bytes(3_900_000),
        generation: String::new(),
        terrain: None,
        ..oklahoma("norman")
    });
    gui.save_ui_config(&store);
    let save1 = gui.ui_config_json().expect("a Gui serializes");

    let mut restored = Gui::new();
    assert!(restored.load_ui_config(&store));
    assert_eq!(
        restored.downloaded_areas(),
        gui.downloaded_areas(),
        "an area came back changed - every field of the record persists",
    );
    // The order is the order they finished, not the store's or a sort's.
    let ids: Vec<&str> = restored
        .downloaded_areas()
        .iter()
        .map(|area| area.spec.area_id.as_str())
        .collect();
    assert_eq!(ids, ["ok-central", "norman"]);

    let save2 = restored
        .ui_config_json()
        .expect("a reloaded Gui serializes");
    assert_eq!(
        save1, save2,
        "the save is not a fixpoint over the area list - reopening drifts the \
         file",
    );

    // The bytes are in the file as an integer, not routed through a float on
    // the way: 112,345,678 is past 2^24, where an f32 would quantise it.
    let written: serde_json::Value = serde_json::from_str(&save1).expect("valid JSON");
    assert_eq!(
        written["downloaded_areas"][0]["bytes"], 112_345_678u64,
        "the exact byte count did not survive the file",
    );
    assert_eq!(
        written["downloaded_areas"][0]["generation"], "basemap_2Fomt-20260828.pmtiles",
        "the generation the area was cut from must be carried from the first \
         write, so the generation step needs no migration of its own",
    );
    assert!(
        written["downloaded_areas"][0]
            .as_object()
            .expect("an area is an object")
            .keys()
            .all(|key| !key.contains("complete")),
        "a completeness flag reached the file - completeness is recomputed \
         from the store, never persisted",
    );
}

/// A config written before this field loads as "no downloaded areas", and the
/// field's arrival cost no version rung and no migration step.
#[test]
fn an_older_config_has_no_areas_and_needed_no_version_bump() {
    let old = r#"{"pane_count":1,"config_version":5,"site":"KTLX"}"#;
    let parsed: UiConfig = serde_json::from_str(old).expect("an older config still parses");
    assert!(
        parsed.downloaded_areas.is_empty(),
        "absence must load as no downloaded areas",
    );

    // No bump: a file already at the current version, written before this
    // field existed, is still at the current version. If the field had needed
    // a rung, `CONFIG_VERSION` would have moved past the 5 that file names and
    // this would fail.
    assert_eq!(
        migrate::CONFIG_VERSION,
        5,
        "the downloaded-area list is additive on `favorite_sites`' terms - a \
         moved CONFIG_VERSION means it was not",
    );
    let store = MemoryKvStore::default();
    squallar_kv::KvStore::store(&store, crate::UI_CONFIG_KEY, old).expect("the store accepts it");
    let mut gui = Gui::new();
    assert!(
        gui.load_ui_config(&store),
        "a pre-field config at the current version must load untouched",
    );
    assert!(gui.downloaded_areas().is_empty());

    // And the whole committed corpus predates the field, which is the same
    // statement about every era rather than about one synthetic string.
    for fixture in [
        include_str!("fixtures/current_full.json"),
        include_str!("fixtures/legacy_v0.json"),
        include_str!("fixtures/root_site_v4.json"),
    ] {
        let value: serde_json::Value = serde_json::from_str(fixture).expect("valid JSON");
        assert!(
            value.get("downloaded_areas").is_none(),
            "precondition: the fixture predates the field, or the assertion \
             below is not about absence",
        );
        let store = MemoryKvStore::default();
        squallar_kv::KvStore::store(&store, crate::UI_CONFIG_KEY, fixture)
            .expect("the store accepts it");
        let mut gui = Gui::new();
        assert!(gui.load_ui_config(&store), "the fixture must still load");
        assert!(gui.downloaded_areas().is_empty());
    }
}

/// Lookup finds by id, removal drops exactly one record, and re-recording an
/// id replaces it in place rather than listing the area twice.
#[test]
fn areas_are_found_removed_and_replaced_by_id() {
    let mut gui = Gui::new();
    gui.record_downloaded_area(oklahoma("ok-central"));
    gui.record_downloaded_area(oklahoma("norman"));

    assert_eq!(
        gui.downloaded_area("norman").map(|area| area.spec.max_zoom),
        Some(12),
    );
    assert!(gui.downloaded_area("tulsa").is_none());

    // A re-download at a newer generation is the same area, not a second one.
    gui.record_downloaded_area(DownloadedArea {
        generation: "basemap_2Fomt-20260901.pmtiles".to_owned(),
        ..oklahoma("ok-central")
    });
    assert_eq!(gui.downloaded_areas().len(), 2, "the id was listed twice");
    assert_eq!(
        gui.downloaded_area("ok-central")
            .expect("still held")
            .generation,
        "basemap_2Fomt-20260901.pmtiles",
    );
    assert_eq!(
        gui.downloaded_areas()[0].spec.area_id,
        "ok-central",
        "the replacement moved the area to the end of the list",
    );

    assert!(gui.forget_downloaded_area("ok-central"));
    assert!(
        !gui.forget_downloaded_area("ok-central"),
        "forgetting an area this device does not hold must answer false",
    );
    let ids: Vec<&str> = gui
        .downloaded_areas()
        .iter()
        .map(|area| area.spec.area_id.as_str())
        .collect();
    assert_eq!(ids, ["norman"], "removal took more than its own record");
}

/// A block naming no real area is dropped, and costs only itself.
#[test]
fn a_nonsense_block_restores_to_nothing_and_costs_only_its_own_area() {
    let good = DownloadedAreaConfig::of(&oklahoma("good"));
    let cases: Vec<(&str, DownloadedAreaConfig)> = vec![
        (
            "an id no store would build a filename from",
            DownloadedAreaConfig {
                area_id: "../../etc".to_owned(),
                ..DownloadedAreaConfig::of(&oklahoma("x"))
            },
        ),
        (
            "an empty id",
            DownloadedAreaConfig {
                area_id: String::new(),
                ..DownloadedAreaConfig::of(&oklahoma("x"))
            },
        ),
        (
            "a bbox whose east is west of its west",
            DownloadedAreaConfig {
                east: -99.0,
                ..DownloadedAreaConfig::of(&oklahoma("x"))
            },
        ),
        (
            "a bbox with no height",
            DownloadedAreaConfig {
                north: 34.75,
                ..DownloadedAreaConfig::of(&oklahoma("x"))
            },
        ),
        (
            "a latitude off the globe",
            DownloadedAreaConfig {
                north: 95.0,
                ..DownloadedAreaConfig::of(&oklahoma("x"))
            },
        ),
        (
            "a NaN edge",
            DownloadedAreaConfig {
                west: f64::NAN,
                ..DownloadedAreaConfig::of(&oklahoma("x"))
            },
        ),
        (
            "a zoom the tile-id space cannot express",
            DownloadedAreaConfig {
                max_zoom: 32,
                ..DownloadedAreaConfig::of(&oklahoma("x"))
            },
        ),
        (
            "a cut of no segments, which nothing could fail to hold all of",
            DownloadedAreaConfig {
                segments: 0,
                ..DownloadedAreaConfig::of(&oklahoma("x"))
            },
        ),
    ];

    assert!(
        good.restore().is_some(),
        "premise: the block these are mutations of restores, or every case \
         below passes for the wrong reason",
    );
    for (what, block) in cases {
        assert!(block.restore().is_none(), "{what} restored to an area");
    }

    // In the file: one bad block among good ones costs itself alone.
    let config = UiConfig {
        downloaded_areas: vec![
            DownloadedAreaConfig::of(&oklahoma("first")),
            DownloadedAreaConfig {
                max_zoom: 32,
                ..DownloadedAreaConfig::of(&oklahoma("broken"))
            },
            DownloadedAreaConfig::of(&oklahoma("last")),
        ],
        ..UiConfig::default()
    };
    let store = MemoryKvStore::default();
    squallar_kv::KvStore::store(
        &store,
        crate::UI_CONFIG_KEY,
        &serde_json::to_string(&config).expect("the config serializes"),
    )
    .expect("the store accepts it");
    let mut gui = Gui::new();
    assert!(
        gui.load_ui_config(&store),
        "one bad block must not fail the file"
    );
    let ids: Vec<&str> = gui
        .downloaded_areas()
        .iter()
        .map(|area| area.spec.area_id.as_str())
        .collect();
    assert_eq!(ids, ["first", "last"]);
}

/// The persisted record round-trips to something the engine can resume from,
/// and the reconciliation the UI asks is over that record's own cut.
#[test]
fn a_restored_record_is_startable_and_reconcilable() {
    let store = MemoryKvStore::default();
    let mut gui = Gui::new();
    gui.record_downloaded_area(oklahoma("ok-central"));
    gui.save_ui_config(&store);

    let mut restored = Gui::new();
    assert!(restored.load_ui_config(&store));
    let area = restored.downloaded_area("ok-central").expect("restored");

    assert_eq!(
        area.spec,
        oklahoma("ok-central").spec,
        "the spec the engine plans and resumes from did not survive the file",
    );
    assert_eq!(
        area.reconcile(&std::collections::BTreeSet::from([0, 1, 2])),
        AreaStatus {
            present: 3,
            expected: 7,
        },
        "a restored record must reconcile against the store like any other",
    );
    assert!(
        !area
            .reconcile(&std::collections::BTreeSet::from([0, 1, 2]))
            .is_complete(),
        "an area three-sevenths on the device read as complete because it was \
         persisted",
    );
}

/// **A record written before an area could hold terrain still loads, and reads
/// as basemap-only** — which is exactly what such an area is.
///
/// Additive on `favorite_sites`' terms: no `CONFIG_VERSION` rung and no
/// migration step, asserted here rather than described.
#[test]
fn a_basemap_only_record_loads_unchanged_and_needed_no_version_bump() {
    let old = r#"{
        "pane_count": 1,
        "config_version": 5,
        "downloaded_areas": [{
            "area_id": "ok-central",
            "west": -98.25, "south": 34.75, "east": -96.5, "north": 36.25,
            "max_zoom": 12,
            "segments": 7,
            "bytes": 112345678,
            "generation": "basemap_2Fomt-20260828.pmtiles"
        }]
    }"#;
    let store = MemoryKvStore::default();
    squallar_kv::KvStore::store(&store, crate::UI_CONFIG_KEY, old).expect("the store accepts it");
    let mut gui = Gui::new();
    assert!(
        gui.load_ui_config(&store),
        "a record predating the terrain half must still load"
    );

    let area = gui
        .downloaded_area("ok-central")
        .expect("the area is held")
        .clone();
    assert_eq!(area.segments_expected, 7, "the basemap half moved");
    assert_eq!(area.bytes.bytes(), 112_345_678, "the byte figure moved");
    assert_eq!(
        area.terrain, None,
        "an area with no terrain block claimed a hillshade it never fetched"
    );

    // No bump. If the terrain half had needed a rung, `CONFIG_VERSION` would
    // have moved past the 5 that file names and this would fail.
    assert_eq!(
        migrate::CONFIG_VERSION,
        5,
        "the terrain half is additive on `favorite_sites`' terms - a moved \
         CONFIG_VERSION means it was not",
    );

    // And it reconciles as an area with one half: the terrain listing is
    // empty, and that must not make it read as short.
    let whole: std::collections::BTreeSet<u32> = (0..7).collect();
    assert!(
        area.reconcile_all(&whole, &std::collections::BTreeSet::new())
            .is_complete(),
        "a basemap-only area read as incomplete because it holds no hillshade"
    );

    // Written back, the record gains no key: a device that never downloaded
    // terrain writes the file its previous build wrote.
    let saved: serde_json::Value =
        serde_json::from_str(&gui.ui_config_json().expect("serializable")).expect("valid JSON");
    assert!(
        saved["downloaded_areas"][0]
            .as_object()
            .expect("an area is an object")
            .get("terrain")
            .is_none(),
        "a basemap-only record grew a terrain key: {}",
        saved["downloaded_areas"][0]
    );
    assert!(
        saved["download_area"]
            .as_object()
            .expect("the selection is an object")
            .get("terrain")
            .is_none(),
        "an untouched checkbox wrote a key: {}",
        saved["download_area"]
    );
}

/// A record that **does** hold terrain says so, with the terrain archive's own
/// generation beside the basemap's — two cadences, two strings.
#[test]
fn a_terrain_holding_record_carries_its_own_cut_and_generation() {
    let store = MemoryKvStore::default();
    let mut gui = Gui::new();
    gui.record_downloaded_area(oklahoma("ok-central"));
    gui.save_ui_config(&store);

    let written: serde_json::Value =
        serde_json::from_str(&gui.ui_config_json().expect("serializable")).expect("valid JSON");
    let terrain = &written["downloaded_areas"][0]["terrain"];
    assert_eq!(terrain["segments"], 2);
    assert_eq!(
        terrain["bytes"], 21_000_111u64,
        "the terrain half's exact byte count did not survive the file",
    );
    assert_eq!(terrain["generation"], "terrain_2F4ca64469750e-20260829");
    assert_ne!(
        terrain["generation"], written["downloaded_areas"][0]["generation"],
        "one generation string was made to date both archives",
    );

    let mut restored = Gui::new();
    assert!(restored.load_ui_config(&store));
    assert_eq!(
        restored.downloaded_area("ok-central"),
        gui.downloaded_area("ok-central"),
        "the terrain half did not come back whole",
    );
}

/// A terrain block naming a cut of nothing costs the **hillshade**, never the
/// rectangle: the area still restores, as basemap-only.
///
/// The same discipline `restore` already applies to the area itself — a
/// nonsense block becomes no block rather than a bad one — one level down.
#[test]
fn a_nonsense_terrain_block_costs_the_hillshade_and_not_the_area() {
    let broken = r#"{
        "pane_count": 1,
        "config_version": 5,
        "downloaded_areas": [{
            "area_id": "norman",
            "west": -98.25, "south": 34.75, "east": -96.5, "north": 36.25,
            "max_zoom": 12,
            "segments": 4,
            "bytes": 9000,
            "generation": "basemap_2Fomt-20260828.pmtiles",
            "terrain": { "segments": 0, "bytes": 5, "generation": "x" }
        }]
    }"#;
    let store = MemoryKvStore::default();
    squallar_kv::KvStore::store(&store, crate::UI_CONFIG_KEY, broken)
        .expect("the store accepts it");
    let mut gui = Gui::new();
    assert!(gui.load_ui_config(&store));

    let area = gui.downloaded_area("norman").expect("the area survives");
    assert_eq!(
        area.segments_expected, 4,
        "the rectangle was lost with the block"
    );
    assert_eq!(
        area.terrain, None,
        "a cut of zero segments would be a half nothing could ever fail to hold all of"
    );
}
