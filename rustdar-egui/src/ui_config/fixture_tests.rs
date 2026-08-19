//! Whole-file fixtures: real configs from real eras of this app, loaded
//! byte-for-byte as a user's disk would supply them.
//!
//! The unit tests beside these build their inputs by saving and mutating; a
//! fixture instead freezes what an *old build actually wrote*, so a change
//! that quietly stops reading some era of file fails here even when every
//! synthetic round trip still passes. The files live under `fixtures/` and
//! are compiled in with `include_str!` — a fixture that could drift from the
//! test reading it would pin nothing.

use crate::Gui;
use crate::UI_CONFIG_KEY;
use rustdar_kv::{KvStore, MemoryKvStore};
use rustdar_radar::types::RadarProduct;
use rustdar_source::id::known;

/// A store holding exactly one era's file, as the disk would.
fn store_with(fixture: &str) -> MemoryKvStore {
    let store = MemoryKvStore::default();
    store
        .store(UI_CONFIG_KEY, fixture)
        .expect("the memory store accepts a write");
    store
}

/// The config a pre-M11, pre-`overlay_states` build wrote: sync was two
/// globals, the Radar toggle lived in a per-pane `layers` map, and no
/// `config_version` key existed because no version field did.
///
/// This file must keep loading forever. It pins three migrations at once:
/// the M11 fold (`viewport_sync`/`sync_layers` off seeds every restored
/// pane's links off), the legacy Radar-toggle capture (the first pane's
/// `layers["Radar"]` drives the global handler when no `overlay_states`
/// map exists), and the absence of a version key reading as the oldest
/// version rather than as an error.
#[test]
fn a_legacy_v0_config_loads_with_links_folded_off_and_its_radar_toggle_migrated() {
    let store = store_with(include_str!("fixtures/legacy_v0.json"));

    let mut gui = Gui::new();
    assert!(
        gui.overlays.is_enabled(&known::RADAR),
        "precondition: a fresh Gui has the radar layer on, or the migration \
         assertion below could pass without the migration running",
    );
    assert!(
        gui.load_ui_config(&store),
        "the legacy file no longer loads — every install from that era \
         loses its whole config on first launch of this build",
    );

    // The M11 fold: both legacy globals are `false`, so every restored
    // pane's per-pane links come back off — `sync_layers` gates the
    // shared-time fan-out under the old model, so it seeds `time_link` too.
    let pane = gui.pane(0).expect("the restored layout has its one pane");
    assert!(!pane.viewport_link, "viewport_sync=false must fold in");
    assert!(!pane.layer_link, "sync_layers=false must fold in");
    assert!(!pane.time_link, "sync_layers=false gated shared time too");

    // The legacy Radar migration: no `overlay_states` in the file, so the
    // first pane's `layers["Radar"] = false` drives the global handler.
    assert!(
        !gui.overlays.is_enabled(&known::RADAR),
        "the legacy per-pane Radar toggle was not migrated to the handler",
    );

    // The rest of the file arrived: a failed load could not have applied
    // these, so they double as proof the `true` above was honest.
    assert_eq!(pane.site, "KMPX");
    assert_eq!(gui.loop_lookback_secs, 7200);
}

/// The full shape this build writes, loaded and saved twice: **save₁ must
/// equal save₂**. This is the reopen-1:1 rule as a test — a load followed by
/// a save is a fixpoint, so reopening the app cannot drift the file, and the
/// autosave's has-anything-changed string comparison cannot oscillate.
///
/// Compared as parsed JSON values, which is exactly the equality the rule
/// needs: key order is a serializer artifact, content is the contract.
#[test]
fn a_current_config_reaches_its_save_fixpoint_in_one_round_trip() {
    let store = store_with(include_str!("fixtures/current_full.json"));
    let mut gui = Gui::new();
    assert!(gui.load_ui_config(&store));

    // Spot checks that the file was applied, not defaults: every value here
    // differs from what a fresh `Gui` starts with.
    assert_eq!(gui.pane(0).expect("pane 0").site, "KTLX");
    let pane1 = gui.pane(1).expect("pane 1");
    assert_eq!(pane1.site, "KOUN");
    assert_eq!(pane1.selected_product, RadarProduct::Velocity);
    assert!(!pane1.time_link, "pane 1 saved its time link off");
    assert_eq!(gui.presets.len(), 1, "the user preset arrived");

    let save1 = gui.ui_config_json().expect("a loaded Gui serializes");
    let store2 = store_with(&save1);
    let mut gui2 = Gui::new();
    assert!(gui2.load_ui_config(&store2));
    let save2 = gui2.ui_config_json().expect("a reloaded Gui serializes");

    let v1: serde_json::Value = serde_json::from_str(&save1).expect("save1 is JSON");
    let v2: serde_json::Value = serde_json::from_str(&save2).expect("save2 is JSON");
    assert_eq!(
        v1, v2,
        "save-load-save moved the file: reopening the app would not be 1:1"
    );
}

/// A file written by a **newer build** — greater version, an overlay kind,
/// a product, pane fields and top-level fields this build has never heard
/// of — loads what it can and, on save, hands back every unknown byte.
///
/// The assertions are on the **saved JSON**, because that is where the
/// guarantee lives: preserving unknowns in memory and dropping them on the
/// way out would still strand the user's newer config after one session
/// under this build.
#[test]
fn a_future_builds_config_survives_a_session_with_every_unknown_intact() {
    let store = store_with(include_str!("fixtures/future_build.json"));
    let mut gui = Gui::new();
    assert!(
        gui.load_ui_config(&store),
        "a greater config_version is not an error — the tolerant load proceeds",
    );

    // The known half applied.
    let pane = gui.pane(0).expect("pane 0");
    assert_eq!(pane.site, "KTLX");
    assert_eq!(
        pane.selected_product,
        RadarProduct::Reflectivity,
        "the unknown product falls back to the default, as ever",
    );
    assert_eq!(
        pane.enabled_overlays.get(&known::RADAR),
        Some(&true),
        "the known overlay keys beside the unknown one still applied",
    );
    let order = &pane.draw_order;
    let radar = order.iter().position(|k| *k == known::RADAR);
    let alerts = order.iter().position(|k| *k == known::NWS_ALERTS);
    assert!(
        radar < alerts,
        "the known draw-order names keep their saved relative order",
    );

    // The downgrade-safe round trip: the save carries every unknown.
    let saved = gui.ui_config_json().expect("serializable");
    let v: serde_json::Value = serde_json::from_str(&saved).expect("valid JSON");
    assert_eq!(
        v["config_version"],
        u64::from(super::migrate::CONFIG_VERSION),
        "the save describes itself as this build's format",
    );
    assert_eq!(
        v["hologram_mode"],
        serde_json::json!({ "depth": 3 }),
        "an unknown top-level field survives to the file",
    );
    assert_eq!(
        v["panes"][0]["particle_engine"], "on",
        "an unknown pane-level field survives to the file",
    );
    assert!(
        v["panes"][0]["draw_order"]
            .as_array()
            .expect("draw_order is a list")
            .iter()
            .any(|e| e == "FutureSatellite"),
        "an unknown overlay kind survives in the draw order",
    );
    assert_eq!(
        v["panes"][0]["enabled_overlays"]["FutureSatellite"], true,
        "an unknown overlay kind survives in the enabled map",
    );
    assert_eq!(
        v["panes"][0]["overlay_configs"]["FutureSatellite"],
        serde_json::json!({ "band": "infrared" }),
        "an unknown overlay kind survives in the config map",
    );
    assert_eq!(
        v["overlay_states"]["FutureSatellite"],
        serde_json::json!({ "opacity": 0.5 }),
        "an unknown handler's saved state survives in overlay_states",
    );
    assert!(
        v["presets"][0]["overlays"]
            .as_array()
            .expect("preset overlays is a list")
            .iter()
            .any(|e| e == "FutureSatellite"),
        "an unknown overlay kind survives in a preset",
    );
}

/// One unreadable pane costs exactly that pane, restored to defaults **in
/// its own position** — never dropped, because a pane is a position in a
/// layout and dropping one renumbers every pane after it.
#[test]
fn a_corrupt_pane_costs_that_pane_its_settings_and_nothing_else() {
    let store = store_with(include_str!("fixtures/corrupt_pane.json"));
    let mut gui = Gui::new();
    assert!(
        gui.load_ui_config(&store),
        "one corrupt pane must not fail the whole config",
    );

    let p0 = gui.pane(0).expect("pane 0");
    assert_eq!(p0.site, "KTLX");
    assert_eq!(p0.time_step_secs, 300, "pane 0 arrived intact");

    let p1 = gui.pane(1).expect("pane 1");
    assert_eq!(
        p1.time_step_secs, 600,
        "the corrupt pane is at defaults, not at its unreadable values",
    );
    assert_eq!(
        p1.site, "KTLX",
        "the corrupt pane's site is the global fallback, as a default pane's is",
    );

    let p2 = gui.pane(2).expect("pane 2");
    assert_eq!(
        p2.site, "KDMX",
        "the pane AFTER the corrupt one kept its own position — salvage is \
         defaults-in-place, never removal",
    );
    assert_eq!(p2.selected_product, RadarProduct::Velocity);
    assert_eq!(p2.time_step_secs, 900);
}

/// A corrupt top-level container costs its own settings and nothing else:
/// `preferences: 5` resets the preferences, while the site, the panes and
/// the lookback all arrive.
#[test]
fn a_corrupt_top_level_field_resets_to_its_default_and_the_rest_loads() {
    let store = store_with(include_str!("fixtures/corrupt_top_field.json"));
    let mut gui = Gui::new();
    assert!(
        gui.load_ui_config(&store),
        "one corrupt field must not fail the whole config",
    );
    assert_eq!(
        serde_json::to_value(&gui.preferences).expect("serializable"),
        serde_json::to_value(rustdar_units::UserPreferences::default()).expect("serializable"),
        "the unreadable preferences reset to defaults",
    );
    assert_eq!(gui.pane(0).expect("pane 0").site, "KDMX");
    assert_eq!(gui.loop_lookback_secs, 7200, "the rest of the file arrived");
}

/// A file that is not JSON at all is the one whole-file refusal left: there
/// are no units inside it to salvage, and the honest answer is that nothing
/// was applied — the caller keeps its defaults and reports a first run.
#[test]
fn a_truncated_file_is_the_one_remaining_whole_file_refusal() {
    let store = store_with(include_str!("fixtures/truncated.json"));
    let mut gui = Gui::new();
    assert!(
        !gui.load_ui_config(&store),
        "a file that does not parse as JSON has nothing to salvage",
    );
    assert_eq!(
        gui.loop_lookback_secs, 3600,
        "the refused load left the defaults untouched",
    );
}

/// The v1 → v2 `gps_config` split (WO-RL-1), proven on a file a v1 build
/// actually could have written: port and baud land under `serial_config`,
/// `heading_source` becomes its own top-level key, and a member inside the
/// old container that this build cannot name rides the rename **verbatim** —
/// the step is a pure `Value` edit, and a rewrite that parsed into the new
/// types would shed it (the armor this fixture exists to keep).
#[test]
fn a_v1_gps_config_splits_into_serial_config_and_a_root_heading() {
    let fixture = include_str!("fixtures/gps_split_v1.json");

    // The step itself, at `Value` level: the unknown member must survive the
    // walk, which is only provable before the typed load fields it away.
    let mut tree: serde_json::Value = serde_json::from_str(fixture).expect("the fixture is JSON");
    super::migrate::migrate_to_current(&mut tree);
    assert!(
        tree.get("gps_config").is_none(),
        "the old container must not survive beside the new one",
    );
    assert_eq!(tree["serial_config"]["port_path"], "/dev/ttyUSB0");
    assert_eq!(tree["serial_config"]["baud_rate"], 4800);
    assert_eq!(
        tree["heading_source"], "CompassOnly",
        "heading choice moves to the root — it matters on every platform",
    );
    assert!(
        tree["serial_config"].get("heading_source").is_none(),
        "the moved member must not also stay behind",
    );
    assert_eq!(
        tree["serial_config"]["dgps_beacon_hz"],
        serde_json::json!(310.5),
        "a member this build cannot name rides the rename verbatim — the \
         migration is a pure Value edit, never a parse",
    );

    // The whole path: load, and reach the save fixpoint in one round trip —
    // migrating a v1 file is still reopen-1:1.
    let store = store_with(fixture);
    let mut gui = Gui::new();
    assert!(gui.load_ui_config(&store), "the v1 file loads");
    assert_eq!(gui.serial_config.port_path.as_deref(), Some("/dev/ttyUSB0"));
    assert_eq!(gui.serial_config.baud_rate, 4800);
    assert_eq!(
        gui.heading_source,
        rustdar_location::HeadingSource::CompassOnly,
    );

    let save1 = gui.ui_config_json().expect("a loaded Gui serializes");
    let store2 = store_with(&save1);
    let mut gui2 = Gui::new();
    assert!(gui2.load_ui_config(&store2));
    let save2 = gui2.ui_config_json().expect("a reloaded Gui serializes");
    let v1: serde_json::Value = serde_json::from_str(&save1).expect("save1 is JSON");
    let v2: serde_json::Value = serde_json::from_str(&save2).expect("save2 is JSON");
    assert_eq!(
        v1["serial_config"]["port_path"], "/dev/ttyUSB0",
        "the split half the file keeps must actually reach the save",
    );
    assert_eq!(v1["heading_source"], "CompassOnly");
    assert_eq!(
        v1, v2,
        "save-load-save moved the migrated file: reopening would not be 1:1"
    );
}

/// **The M8b unknown-id pin, both directions.** A `draw_order` naming a layer
/// no handler serves — "MysteryLayer" — survives load→save→reload **in
/// place**, and is skipped at draw rather than resolved.
///
/// This is a DELIBERATE behavior change (M8b b2): the pre-M8b reconcile
/// *dropped* unknown names from the pane's live list and a sidecar re-appended
/// them after the known order on save, so a newer build's layer lost its
/// position through a session under this build. With open [`LayerId`]s the
/// unknown id is an ordinary entry: it keeps its saved position among the
/// names it was saved between, the draw loop's `handler_by_id` gate skips it
/// (retained-in-list, skipped-at-draw), and the second session reads back
/// exactly what the first wrote — the fixpoint half.
#[test]
fn an_unknown_draw_order_id_survives_in_place_and_is_skipped_at_draw() {
    let store = store_with(
        r#"{"config_version":3,"pane_count":1,"site":"KTLX",
            "panes":[{"site":"KTLX",
                      "draw_order":["Radar","MysteryLayer","NwsAlerts"]}]}"#,
    );
    let mut gui = Gui::new();
    assert!(gui.load_ui_config(&store), "the fixture must load");

    let mystery = rustdar_source::id::LayerId::new("MysteryLayer");

    // Direction 1 (load): retained in the live list, in place — after Radar,
    // before NwsAlerts, exactly where the file put it.
    let order = &gui.pane(0).expect("pane 0").draw_order;
    let pos = |id: &rustdar_source::id::LayerId| {
        order
            .iter()
            .position(|k| k == id)
            .unwrap_or_else(|| panic!("{id:?} missing from the live draw order"))
    };
    assert!(
        pos(&known::RADAR) < pos(&mystery) && pos(&mystery) < pos(&known::NWS_ALERTS),
        "the unknown id must keep its saved position IN the list, not ride a \
         sidecar to the end: {order:?}",
    );
    // The skip predicate the draw loop gates each id on: no handler, no arm.
    assert!(
        gui.overlays.handler_by_id(&mystery).is_none(),
        "no handler serves MysteryLayer — the draw loop skips it by this exact \
         predicate",
    );
    // The registered ids the file omitted joined at their weight positions.
    assert_eq!(
        order.len(),
        13,
        "all twelve registered layers plus the unknown id: {order:?}",
    );

    // Direction 2 (save): written back in place.
    let saved = gui.ui_config_json().expect("serializable");
    let v: serde_json::Value = serde_json::from_str(&saved).expect("valid JSON");
    let written: Vec<&str> = v["panes"][0]["draw_order"]
        .as_array()
        .expect("draw_order is a list")
        .iter()
        .map(|e| e.as_str().expect("every entry is a string"))
        .collect();
    let wpos = |name: &str| {
        written
            .iter()
            .position(|e| *e == name)
            .unwrap_or_else(|| panic!("{name} missing from the saved draw order"))
    };
    assert!(
        wpos("Radar") < wpos("MysteryLayer") && wpos("MysteryLayer") < wpos("NwsAlerts"),
        "the save must write the unknown id where it sits, never appended to \
         the tail: {written:?}",
    );

    // The fixpoint: a second session reads back exactly what the first wrote.
    let second_store = store_with(&saved);
    let mut second = Gui::new();
    assert!(second.load_ui_config(&second_store), "the save must reload");
    assert_eq!(
        second.pane(0).expect("pane 0").draw_order,
        gui.pane(0).expect("pane 0").draw_order,
        "the second session's live order must equal the first's — the \
         unknown id neither moves nor multiplies across sessions",
    );
    let resaved = second.ui_config_json().expect("serializable");
    let rv: serde_json::Value = serde_json::from_str(&resaved).expect("valid JSON");
    assert_eq!(
        rv["panes"][0]["draw_order"], v["panes"][0]["draw_order"],
        "save→reload→save is a fixpoint for a list carrying an unknown id",
    );
}

/// The swap half of the unknown-id doctrine: an unregistered id's saved
/// `enabled_overlays`/`overlay_configs` entries survive the registry-state
/// overwrite every layer toggle performs (`PaneState::adopt_handler_state`).
///
/// Before M8b those entries rode a baggage sidecar the swap could not touch;
/// with the sidecar gone the pane maps are the ONLY carrier, so a wholesale
/// `save_pane_configs()` assignment would silently drop a newer build's
/// layer state on the first eye click — and the next autosave would make the
/// loss permanent. This is the test that fails instead.
#[test]
fn an_unknown_ids_saved_state_survives_a_layer_toggle() {
    let store = store_with(
        r#"{"config_version":3,"pane_count":1,"site":"KTLX",
            "panes":[{"site":"KTLX",
                      "draw_order":["Radar","MysteryLayer"],
                      "enabled_overlays":{"Radar":true,"MysteryLayer":true},
                      "overlay_configs":{"MysteryLayer":{"band":"infrared"}}}]}"#,
    );
    let mut gui = Gui::new();
    assert!(gui.load_ui_config(&store), "the fixture must load");
    let mystery = rustdar_source::id::LayerId::new("MysteryLayer");
    assert_eq!(
        gui.pane(0)
            .expect("pane 0")
            .overlay_configs
            .get(&mystery)
            .cloned(),
        Some(serde_json::json!({"band": "infrared"})),
        "premise: the unknown entry landed in the pane map",
    );

    // The toggle every eye click routes through — the swap overwrite.
    gui.set_overlay_on_pane_for_test(0, &known::CITY_LABELS, true);

    let pane = gui.pane(0).expect("pane 0");
    assert_eq!(
        pane.overlay_configs.get(&mystery).cloned(),
        Some(serde_json::json!({"band": "infrared"})),
        "a layer toggle's registry-state overwrite dropped an unknown id's \
         saved config",
    );
    assert_eq!(
        pane.enabled_overlays.get(&mystery).copied(),
        Some(true),
        "a layer toggle's registry-state overwrite dropped an unknown id's \
         enabled flag",
    );
    let saved = gui.ui_config_json().expect("serializable");
    let v: serde_json::Value = serde_json::from_str(&saved).expect("valid JSON");
    assert_eq!(
        v["panes"][0]["overlay_configs"]["MysteryLayer"],
        serde_json::json!({"band": "infrared"}),
        "the save after the toggle must still carry the unknown id's state",
    );
}
