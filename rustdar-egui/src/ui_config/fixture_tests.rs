//! Whole-file fixtures: real configs from real eras of this app, loaded
//! byte-for-byte as a user's disk would supply them.

use crate::Gui;
use crate::UI_CONFIG_KEY;
use rustdar_kv::{KvStore, MemoryKvStore};
use rustdar_radar::types::RadarProduct;
use rustdar_source::handler::PaneRef;
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
#[test]
fn a_legacy_v0_config_loads_with_links_folded_off_and_its_radar_toggle_migrated() {
    let store = store_with(include_str!("fixtures/legacy_v0.json"));

    let mut gui = Gui::new();
    assert!(
        gui.overlays.is_enabled(&known::RADAR, &PaneRef::bare(0)),
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
        !gui.overlays.is_enabled(&known::RADAR, &PaneRef::bare(0)),
        "the legacy per-pane Radar toggle was not migrated to the handler",
    );
    // And **in the pane**, which is where WO-M10b moved the answer: the
    // assertion above is about the registry's own copy, and a converted
    // handler could satisfy it while the pane came up drawing radar anyway.
    assert!(
        !pane.is_overlay_enabled(&known::RADAR),
        "pane 0 came back with radar on — the migrated toggle did not reach \
         the slot the pane actually draws from",
    );
    assert!(
        !gui.overlays
            .is_enabled(&known::RADAR, &pane.layer_ref(0, &known::RADAR)),
        "the handler answers pane 0 with radar on",
    );

    // The rest of the file arrived: a failed load could not have applied
    // these, so they double as proof the `true` above was honest.
    assert_eq!(pane.site(), "KMPX");
    assert_eq!(gui.loop_lookback_secs, 7200);
}

/// The full shape this build writes, loaded and saved twice: **save₁ must
/// equal save₂**. This is the reopen-1:1 rule as a test — a load followed by
/// a save is a fixpoint, so reopening the app cannot drift the file, and the
/// autosave's has-anything-changed string comparison cannot oscillate.
#[test]
fn a_current_config_reaches_its_save_fixpoint_in_one_round_trip() {
    let store = store_with(include_str!("fixtures/current_full.json"));
    let mut gui = Gui::new();
    assert!(gui.load_ui_config(&store));

    // Spot checks that the file was applied, not defaults: every value here
    // differs from what a fresh `Gui` starts with.
    assert_eq!(gui.pane(0).expect("pane 0").site(), "KTLX");
    let pane1 = gui.pane(1).expect("pane 1");
    assert_eq!(pane1.site(), "KOUN");
    assert_eq!(pane1.selected_product(), RadarProduct::Velocity);
    assert!(!pane1.time_link, "pane 1 saved its time link off");
    assert_eq!(gui.presets.len(), 1, "the user preset arrived");

    let save1 = gui.ui_config_json().expect("a loaded Gui serializes");
    let store2 = store_with(&save1);
    let mut gui2 = Gui::new();
    assert!(gui2.load_ui_config(&store2));
    let save2 = gui2.ui_config_json().expect("a reloaded Gui serializes");

    // The divergence itself, named on the RELOADED panes rather than left
    // implied by the fixpoint below. This file is the corpus's multi-pane
    // divergent-site/product fixture, and WO-E6b's shape transform builds one
    // radar slot per pane out of exactly these three values: a migration that
    // collapsed them onto the top-level `site`, or onto pane 0's, would still
    // reach a fixpoint while showing both panes the same picture.
    let selection = |g: &Gui, i| {
        let p = g.pane(i).expect("the restored layout has both panes");
        (
            p.site().to_string(),
            p.selected_product(),
            p.selected_elevation(),
        )
    };
    assert_ne!(
        selection(&gui, 0),
        selection(&gui, 1),
        "premise: this file's two panes differ in all three values",
    );
    assert_eq!(
        selection(&gui2, 0),
        selection(&gui, 0),
        "pane 0's site, product and tilt did not survive the reload",
    );
    assert_eq!(
        selection(&gui2, 1),
        selection(&gui, 1),
        "pane 1's site, product and tilt did not survive the reload — a \
         migration that collapsed the panes onto one selection reaches the \
         fixpoint below while showing both panes the same picture",
    );

    let v1: serde_json::Value = serde_json::from_str(&save1).expect("save1 is JSON");
    let v2: serde_json::Value = serde_json::from_str(&save2).expect("save2 is JSON");
    assert_eq!(
        v1, v2,
        "save-load-save moved the file: reopening the app would not be 1:1"
    );
}

/// A file written by a **newer build** — greater version, a layer, a product,
/// a slot member, pane fields and top-level fields this build has never heard
/// of, and even a slot list entry that is not a slot — loads what it can and,
/// on save, hands back every unknown byte.
///
/// The file speaks the **slot shape**, because that is the shape a newer
/// build writes; its `config_version` is far enough ahead that no migration
/// step touches it, so what is read here is read straight.
#[test]
fn a_future_builds_config_survives_a_session_with_every_unknown_intact() {
    let store = store_with(include_str!("fixtures/future_build.json"));
    let mut gui = Gui::new();
    assert!(
        gui.load_ui_config(&store),
        "a greater config_version is not an error — the tolerant load proceeds",
    );

    // The known half applied — out of the radar slot, which is where a pane's
    // selection lives. The file's top-level site is a DIFFERENT one on
    // purpose: reading "KTLX" here can only have come from the slot.
    let pane = gui.pane(0).expect("pane 0");
    assert_eq!(gui.radar.config.site, "KGRR", "premise: the globals differ");
    assert_eq!(pane.site(), "KTLX", "the radar slot's site is the pane's");
    assert_eq!(
        pane.selected_product(),
        RadarProduct::Reflectivity,
        "the unknown product falls back to the default, as ever",
    );
    assert_eq!(pane.selected_elevation(), 0.5, "the slot's tilt applied");
    assert_eq!(
        pane.slot(&known::RADAR).map(|slot| slot.enabled),
        Some(true),
        "the radar slot's own enabled flag applied",
    );
    let mystery = rustdar_source::id::LayerId::new("FutureSatellite");
    assert_eq!(
        pane.slot(&mystery).map(|slot| slot.enabled),
        Some(true),
        "the unknown layer kept its slot and its flag",
    );
    let order = pane.draw_order_vec();
    let radar = order.iter().position(|k| *k == known::RADAR);
    let alerts = order.iter().position(|k| *k == known::NWS_ALERTS);
    assert!(
        radar < alerts,
        "the known slot ids keep their saved relative order",
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
    let slots = v["panes"][0]["layer_slots"]
        .as_array()
        .expect("layer_slots is a list");
    let slot = |id: &str| {
        slots
            .iter()
            .find(|entry| entry.get("id").and_then(|v| v.as_str()) == Some(id))
            .unwrap_or_else(|| panic!("{id} missing from the saved slot list"))
    };
    assert_eq!(
        slot("FutureSatellite")["enabled"],
        serde_json::json!(true),
        "an unknown layer survives with its flag",
    );
    assert_eq!(
        slot("FutureSatellite")["config"],
        serde_json::json!({ "band": "infrared" }),
        "an unknown layer survives with its config",
    );
    assert_eq!(
        slot("Radar")["config"]["beam_hologram"],
        serde_json::json!(true),
        "a member of the radar slot this build cannot name survives beside \
         the four it can — the slot's config is carried, not rebuilt",
    );
    assert!(
        slots
            .iter()
            .any(|entry| entry == &serde_json::json!("a slot shape this build cannot read")),
        "a slot-list entry that is not a slot at all survives verbatim: {slots:?}",
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
    assert_eq!(p0.site(), "KTLX");
    assert_eq!(p0.time.step.as_secs(), 300, "pane 0 arrived intact");

    let p1 = gui.pane(1).expect("pane 1");
    assert_eq!(
        p1.time.step.as_secs(),
        600,
        "the corrupt pane is at defaults, not at its unreadable values",
    );
    assert_eq!(
        p1.site(),
        "KTLX",
        "the corrupt pane's site is the global fallback, as a default pane's is",
    );

    let p2 = gui.pane(2).expect("pane 2");
    assert_eq!(
        p2.site(),
        "KDMX",
        "the pane AFTER the corrupt one kept its own position — salvage is \
         defaults-in-place, never removal",
    );
    assert_eq!(p2.selected_product(), RadarProduct::Velocity);
    assert_eq!(p2.time.step.as_secs(), 900);
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
    assert_eq!(gui.pane(0).expect("pane 0").site(), "KDMX");
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

/// The v1 → v2 `gps_config` split, proven on a file a v1 build
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

/// **The M8b unknown-id pin, both directions, now through the shape
/// migration.** A v2 `draw_order` naming a layer no handler serves —
/// "MysteryLayer" — survives load→migrate→save→reload **in place**, and is
/// skipped at draw rather than resolved.
#[test]
fn an_unknown_draw_order_id_survives_in_place_and_is_skipped_at_draw() {
    let store = store_with(
        r#"{"config_version":2,"pane_count":1,"site":"KTLX",
            "panes":[{"site":"KTLX",
                      "draw_order":["Radar","MysteryLayer","NwsAlerts"]}]}"#,
    );
    let mut gui = Gui::new();
    assert!(gui.load_ui_config(&store), "the fixture must load");

    let mystery = rustdar_source::id::LayerId::new("MysteryLayer");

    // Direction 1 (load): retained in the live stack, in place — after Radar,
    // before NwsAlerts, exactly where the file put it.
    let order = gui.pane(0).expect("pane 0").draw_order_vec();
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

    // Direction 2 (save): written back in place, as a slot.
    let saved = gui.ui_config_json().expect("serializable");
    let v: serde_json::Value = serde_json::from_str(&saved).expect("valid JSON");
    let written: Vec<&str> = v["panes"][0]["layer_slots"]
        .as_array()
        .expect("layer_slots is a list")
        .iter()
        .map(|e| e["id"].as_str().expect("every slot names an id"))
        .collect();
    let wpos = |name: &str| {
        written
            .iter()
            .position(|e| *e == name)
            .unwrap_or_else(|| panic!("{name} missing from the saved slot list"))
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
        second.pane(0).expect("pane 0").draw_order_vec(),
        gui.pane(0).expect("pane 0").draw_order_vec(),
        "the second session's live order must equal the first's — the \
         unknown id neither moves nor multiplies across sessions",
    );
    let resaved = second.ui_config_json().expect("serializable");
    let rv: serde_json::Value = serde_json::from_str(&resaved).expect("valid JSON");
    assert_eq!(
        rv["panes"][0]["layer_slots"], v["panes"][0]["layer_slots"],
        "save→reload→save is a fixpoint for a stack carrying an unknown id",
    );
}

/// The swap half of the unknown-id doctrine: an unregistered id's saved
/// enabled flag and config survive the registry-state overwrite every layer
/// toggle performs (`PaneState::adopt_handler_state`) — and survive the shape
/// migration that folded the three v2 maps into one slot.
#[test]
fn an_unknown_ids_saved_state_survives_a_layer_toggle() {
    let store = store_with(
        r#"{"config_version":2,"pane_count":1,"site":"KTLX",
            "panes":[{"site":"KTLX",
                      "draw_order":["Radar","MysteryLayer"],
                      "enabled_overlays":{"Radar":true,"MysteryLayer":true},
                      "overlay_configs":{"MysteryLayer":{"band":"infrared"}}}]}"#,
    );
    let mut gui = Gui::new();
    assert!(gui.load_ui_config(&store), "the fixture must load");
    let mystery = rustdar_source::id::LayerId::new("MysteryLayer");
    let slot = |g: &Gui, i| {
        g.pane(i)
            .expect("pane 0")
            .slot(&mystery)
            .cloned()
            .map(|slot| (slot.enabled, slot.config))
    };
    assert_eq!(
        slot(&gui, 0),
        Some((true, serde_json::json!({"band": "infrared"}))),
        "premise: the three v2 entries folded into one slot for the unknown id",
    );

    // The toggle every eye click routes through — the swap overwrite.
    gui.set_overlay_on_pane_for_test(0, &known::CITY_LABELS, true);

    assert_eq!(
        slot(&gui, 0),
        Some((true, serde_json::json!({"band": "infrared"}))),
        "a layer toggle's registry-state overwrite dropped an unknown id's \
         saved slot",
    );
    let saved = gui.ui_config_json().expect("serializable");
    let v: serde_json::Value = serde_json::from_str(&saved).expect("valid JSON");
    let written = v["panes"][0]["layer_slots"]
        .as_array()
        .expect("layer_slots is a list")
        .iter()
        .find(|entry| entry["id"] == "MysteryLayer")
        .cloned()
        .expect("the unknown id keeps a slot in the save");
    assert_eq!(
        written["config"],
        serde_json::json!({"band": "infrared"}),
        "the save after the toggle must still carry the unknown id's config",
    );
    assert_eq!(
        written["enabled"],
        serde_json::json!(true),
        "the save after the toggle must still carry the unknown id's flag",
    );

    // The reload leg (WO-E6a). The pin above stopped at the save, so the
    // corpus proved the unknown id's *position* survives a round trip (the
    // draw-order test) and its *state* survives a save — but never the whole
    // triple through a second session. WO-E6b folded all three into one
    // `LayerSlot`, and that transform can lose the enabled flag or the config
    // while leaving the order intact, so the reload is the direction that had
    // to be nailed down before the shape moved.
    let mut second = Gui::new();
    assert!(
        second.load_ui_config(&store_with(&saved)),
        "the save must reload",
    );
    assert_eq!(
        slot(&second, 0),
        Some((true, serde_json::json!({"band": "infrared"}))),
        "the second session lost the unknown id's slot",
    );
    assert_eq!(
        second.pane(0).expect("pane 0").draw_order_vec(),
        gui.pane(0).expect("pane 0").draw_order_vec(),
        "the second session's draw order must equal the first's",
    );
    let resaved = second.ui_config_json().expect("serializable");
    let rv: serde_json::Value = serde_json::from_str(&resaved).expect("valid JSON");
    assert_eq!(
        rv["panes"][0], v["panes"][0],
        "save→reload→save is not a fixpoint for a pane carrying an unknown \
         id's order, enabled flag and config together",
    );
}

/// A file whose **global** `live_chunks` is `false`, over two panes.
///
/// The setting round-trips today through `set_live_chunks` and a direct
/// `UiConfig` parse (`live_chunks_config_tests`), but nothing in the corpus
/// drove the *whole-file* path — `load_ui_config` → `Gui` → `ui_config_json`
/// — with the value **off**. That is the leg that matters: every other file
/// here carries the default `true`, so an assertion on it could not tell the
/// file's value from the value a fresh `Gui` starts with.
///
/// WO-E6b turns this global into a per-pane radar-slot member and fans it out
/// to every pane, so this fixture is deliberately two-pane and deliberately
/// `false`: after that land, one pane taking the fan-out and the other
/// silently keeping the default is a difference this test can see, and the
/// save-fixpoint below is what says the value survives the shape transform in
/// both directions.
#[test]
fn a_global_live_chunks_off_reaches_the_gui_and_returns_on_the_save() {
    let store = store_with(include_str!("fixtures/live_chunks_off.json"));

    let mut gui = Gui::new();
    assert!(
        gui.live_chunks_enabled(),
        "precondition: a fresh Gui starts with live chunks ON, or the \
         assertion below could pass without the file being read at all",
    );
    assert!(gui.load_ui_config(&store), "the fixture must load");
    assert!(
        !gui.live_chunks_enabled(),
        "the file's live_chunks=false did not reach the Gui",
    );

    // The rest of the file arrived, so the `false` above came from a load
    // that worked rather than from one that failed part-way.
    assert_eq!(gui.pane(0).expect("pane 0").site(), "KDMX");
    assert_eq!(gui.pane(1).expect("pane 1").site(), "KFTG");
    assert_ne!(
        gui.pane(1).expect("pane 1").selected_product(),
        gui.pane(0).expect("pane 0").selected_product(),
        "the file gives the two panes different products — equal here means \
         one of them fell back to the default",
    );

    // Direction 2, and the reopen-1:1 rule: the save says `false`, a second
    // session reads it back as `false`, and save₁ == save₂.
    let save1 = gui.ui_config_json().expect("a loaded Gui serializes");
    let v1: serde_json::Value = serde_json::from_str(&save1).expect("save1 is JSON");
    assert_eq!(
        v1["live_chunks"],
        serde_json::json!(false),
        "the save reinstated the default instead of writing the loaded value",
    );

    let mut gui2 = Gui::new();
    assert!(
        gui2.load_ui_config(&store_with(&save1)),
        "the save must reload"
    );
    assert!(
        !gui2.live_chunks_enabled(),
        "the second session lost the setting",
    );
    let save2 = gui2.ui_config_json().expect("a reloaded Gui serializes");
    let v2: serde_json::Value = serde_json::from_str(&save2).expect("save2 is JSON");
    assert_eq!(
        v1, v2,
        "save-load-save moved the file: reopening the app would not be 1:1"
    );
}

/// **The v2 → v3 shape migration, at `Value` level** — the campaign's one
/// structural config migration, proven where it happens rather than through
/// the typed load that would hide it.
///
/// Three parallel per-pane maps become one ordered list of slots; the pane's
/// flat radar selection moves into the radar slot's config; the one global
/// `live_chunks` fans out to every pane while staying at the root for the
/// settings UI and for an older build; and everything the step does not
/// consume is left exactly where it was.
#[test]
fn the_shape_migration_folds_three_maps_into_slots_and_fans_the_global_out() {
    let mut tree: serde_json::Value = serde_json::from_str(
        r#"{"config_version":2,"live_chunks":false,"site":"KTLX",
            "panes":[{"site":"KOUN","selected_product":"Velocity",
                      "selected_elevation":0.9,"time_step_secs":300,
                      "draw_order":["Radar","MysteryLayer",42,"NwsAlerts"],
                      "enabled_overlays":{"Radar":true,"NwsAlerts":false,
                                          "ZebraLayer":true},
                      "overlay_configs":{"MysteryLayer":{"band":"infrared"},
                                         "AardvarkLayer":{"n":1}}},
                     {"site":"KDMX"}]}"#,
    )
    .expect("the fixture is JSON");
    super::migrate::migrate_to_current(&mut tree);

    let pane = &tree["panes"][0];
    for gone in [
        "draw_order",
        "enabled_overlays",
        "overlay_configs",
        "site",
        "selected_product",
        "selected_elevation",
    ] {
        assert!(
            pane.get(gone).is_none(),
            "{gone} was consumed by the transform and must not survive beside \
             the slot it moved into",
        );
    }
    assert_eq!(
        pane["time_step_secs"], 300,
        "a pane field the step does not consume is left exactly alone",
    );

    let slots = pane["layer_slots"]
        .as_array()
        .expect("the pane carries a slot list");
    let ids: Vec<&serde_json::Value> = slots.iter().map(|s| &s["id"]).collect();
    assert_eq!(
        ids,
        vec![
            &serde_json::json!("Radar"),
            &serde_json::json!("MysteryLayer"),
            &serde_json::json!("NwsAlerts"),
            &serde_json::json!("AardvarkLayer"),
            &serde_json::json!("ZebraLayer"),
            &serde_json::json!(null),
        ],
        "the list order is the draw order the file gave, then the ids only \
         the maps named, sorted so the same file always migrates the same \
         way — and the non-string element rides along at the end: {slots:?}",
    );
    let slot = |id: &str| {
        slots
            .iter()
            .find(|s| s["id"] == id)
            .unwrap_or_else(|| panic!("{id} has no slot"))
    };
    assert_eq!(slot("Radar")["enabled"], serde_json::json!(true));
    assert_eq!(slot("NwsAlerts")["enabled"], serde_json::json!(false));
    assert!(
        slot("MysteryLayer").get("enabled").is_none(),
        "a layer the maps said nothing about says nothing here either — the \
         load asks the handler, exactly as a missing map entry always did",
    );
    assert_eq!(
        slot("MysteryLayer")["config"],
        serde_json::json!({"band": "infrared"}),
        "an unknown id's config rides the shape transform verbatim",
    );
    assert_eq!(
        slot("ZebraLayer")["enabled"],
        serde_json::json!(true),
        "an id only the enabled map named still gets its slot",
    );
    assert!(
        slots.iter().any(|s| s == &serde_json::json!(42)),
        "a draw_order element that is not a string at all survives verbatim",
    );

    // The radar slot: the pane's selection, moved, plus the fan-out.
    assert_eq!(
        slot("Radar")["config"],
        serde_json::json!({
            "site": "KOUN",
            "product": "Velocity",
            "elevation": 0.9,
            "live_chunks": false
        }),
        "the radar slot is the pane's own selection plus the global's value",
    );
    assert_eq!(
        tree["panes"][1]["layer_slots"][0],
        serde_json::json!({
            "id": "Radar",
            "config": { "site": "KDMX", "live_chunks": false }
        }),
        "a pane that listed no layers at all still gets its radar slot, and \
         it too takes the fan-out — one pane taking it and the other keeping \
         a default is the divergence this step must not introduce",
    );
    assert_eq!(
        tree["live_chunks"],
        serde_json::json!(false),
        "the global stays at the root: the settings UI still writes it and an \
         older build still reads it",
    );
    // The walk does not stamp `config_version` — the save does, from
    // [`CONFIG_VERSION`] — so this step has to be safe to run twice on its
    // own output, and the pane it already rewrote is how it knows.
    let once = tree.clone();
    super::migrate::migrate_to_current(&mut tree);
    assert_eq!(
        tree, once,
        "the shape step is not idempotent, and the tree it produces still \
         reads as the version it came from",
    );
}

/// A file that **already carries slots** keeps them. This is the shape an
/// older build hands back after a session: it cannot name `layer_slots`, so
/// it carries the list through untouched while writing its own version and
/// its own empty flat fields. Rebuilding the stack from those would throw the
/// user's real layout away on the way back up.
#[test]
fn a_pane_that_already_carries_slots_keeps_them_and_sheds_the_flat_fields() {
    let mut tree: serde_json::Value = serde_json::from_str(
        r#"{"config_version":2,"live_chunks":true,
            "panes":[{"site":"","selected_product":"Reflectivity",
                      "draw_order":[],"enabled_overlays":{},
                      "layer_slots":[{"id":"Radar","enabled":true,
                                      "config":{"site":"KOUN",
                                                "product":"Velocity",
                                                "elevation":0.9,
                                                "live_chunks":false}}]}]}"#,
    )
    .expect("the fixture is JSON");
    super::migrate::migrate_to_current(&mut tree);
    assert_eq!(
        tree["panes"][0]["layer_slots"],
        serde_json::json!([{
            "id": "Radar", "enabled": true,
            "config": {"site": "KOUN", "product": "Velocity",
                       "elevation": 0.9, "live_chunks": false}
        }]),
        "the slots the file already carried are the truth; the older build's \
         empty flat fields are its stale copy",
    );
    assert!(
        tree["panes"][0].get("draw_order").is_none() && tree["panes"][0].get("site").is_none(),
        "the stale copy is shed either way, or the next round trip would \
         resurrect it",
    );
}

/// **The bytes before the shape moved are kept, once.** The v2 → v3 rewrite
/// is not reversible by a downgrade, so the file as it stood is copied aside
/// before the first v3 write and never written again.
#[test]
fn the_first_v3_write_keeps_the_v2_bytes_and_never_writes_them_twice() {
    let original = include_str!("fixtures/current_full.json");
    let store = store_with(original);
    assert!(
        store.load(crate::UI_CONFIG_BACKUP_KEY).is_none(),
        "premise: nothing has been kept aside yet",
    );

    let mut gui = Gui::new();
    assert!(gui.load_ui_config(&store), "the v2 file loads");
    assert_eq!(
        store.load(crate::UI_CONFIG_BACKUP_KEY).as_deref(),
        Some(original),
        "the pre-slot bytes were not kept, byte for byte",
    );

    // A second session, and a save: the copy must not be overwritten with
    // something this build has already rewritten.
    gui.save_ui_config(&store);
    assert_ne!(
        store.load(crate::UI_CONFIG_KEY).as_deref(),
        Some(original),
        "premise: the save really did rewrite the live file",
    );
    assert_eq!(
        store.load(crate::UI_CONFIG_BACKUP_KEY).as_deref(),
        Some(original),
        "the copy is one-time — a second write would replace the only copy \
         of the shape that cannot be rebuilt",
    );

    // **The leg the guard actually exists for**, and the only one that can
    // fail: a second pre-slot file turning up at the live key later. That is
    // the downgrade path — an older build rewrites the file in its own shape,
    // carrying the slot list it cannot name as baggage — and the version
    // check alone does NOT stop it, because that file reads as v2 too.
    // Without the one-time guard the user's real layout is replaced by the
    // downgraded build's flattened copy of it, and the original is gone.
    let downgraded = r#"{"config_version":2,"pane_count":1,"site":"KDMX",
                         "panes":[{"site":"KDMX","layer_slots":[]}]}"#;
    store
        .store(crate::UI_CONFIG_KEY, downgraded)
        .expect("the memory store accepts a write");
    crate::back_up_pre_slot_config(&store);
    assert_eq!(
        store.load(crate::UI_CONFIG_BACKUP_KEY).as_deref(),
        Some(original),
        "a later pre-slot file overwrote the copy — the only bytes that could \
         not be rebuilt were replaced by a build that had already lost them",
    );

    // A store that never held a pre-slot file gains no copy at all.
    let v3_only = store_with(&Gui::new().ui_config_json().expect("a fresh Gui serializes"));
    crate::back_up_pre_slot_config(&v3_only);
    assert!(
        v3_only.load(crate::UI_CONFIG_BACKUP_KEY).is_none(),
        "there is nothing to preserve about a file already in the new shape",
    );
}

/// The radar slot's config is the **pane's**, and a layer toggle must not
/// touch it. Every other slot's config is overwritten from the registry on
/// every toggle (`adopt_handler_state`), and the radar slot would lose this
/// pane's site, product, tilt and live-chunk switch with it.
#[test]
fn a_layer_toggle_leaves_the_radar_slots_own_members_alone() {
    let store = store_with(include_str!("fixtures/current_full.json"));
    let mut gui = Gui::new();
    assert!(gui.load_ui_config(&store), "the fixture must load");

    let before = gui
        .pane(1)
        .expect("pane 1")
        .slot(&known::RADAR)
        .expect("every pane has a radar slot")
        .config
        .clone();
    assert_eq!(
        before,
        serde_json::json!({
            "site": "KOUN", "product": "Velocity",
            "elevation": 0.9, "live_chunks": true
        }),
        "premise: the slot carries all four pane-owned members",
    );

    gui.set_overlay_on_pane_for_test(1, &known::CITY_LABELS, true);

    let after = gui
        .pane(1)
        .expect("pane 1")
        .slot(&known::RADAR)
        .expect("the radar slot survived the toggle")
        .config
        .clone();
    for key in crate::pane::RADAR_SLOT_PANE_KEYS {
        assert_eq!(
            after.get(key),
            before.get(key),
            "a layer toggle overwrote the radar slot's {key:?} with a handler \
             state, and this pane's selection went with it: {after}",
        );
    }
    assert!(
        gui.pane(1)
            .expect("pane 1")
            .is_overlay_enabled(&known::CITY_LABELS),
        "premise: the toggle really did run",
    );
}

/// The pin under the two-owner arrangement above: the radar slot's config is
/// split between the pane and the radar handler by **name**, so the two sets
/// of names must stay disjoint. A handler member called `site` would be
/// silently dropped on every swap and every save.
#[test]
fn the_radar_handler_and_the_pane_do_not_claim_the_same_slot_members() {
    use rustdar_source::handler::SourceHandler;
    // **`serialize_pane_state`, not `serialize_state`**: the radar slot's
    // handler half is what the PANE persists, and since WO-M10c that is the
    // only half a handler writes at all — the global `serialize_state` no
    // longer carries a per-pane member for any layer.
    let handler = rustdar_radar::source::RadarSource::new();
    let fresh = handler
        .create_pane_state(false)
        .expect("the radar handler keeps per-pane state");
    let state = handler.serialize_pane_state(&*fresh);
    let members: Vec<&String> = state
        .as_object()
        .expect("the radar handler saves an object")
        .keys()
        .collect();
    assert!(
        !members.is_empty(),
        "premise: the radar handler saves something, or this check cannot fail",
    );
    for key in crate::pane::RADAR_SLOT_PANE_KEYS {
        assert!(
            !state.get(key).is_some(),
            "the radar handler now saves {key:?}, which the PANE owns in that \
             same slot ({members:?}) — one of the two loses its value on every \
             `adopt_handler_state`; give the handler a different name or teach \
             the split about it",
        );
    }
}

/// **The slot list may never be spelled `layers`.** That key is the v0
/// per-pane toggle map, still read by the migration chain — an older build
/// handed a list where it expects a map fails its whole pane and salvages it
/// to defaults, which is the one downgrade outcome this shape move must not
/// cause.
#[test]
fn the_slot_list_does_not_reuse_the_v0_toggle_maps_key() {
    let mut gui = Gui::new();
    assert!(gui.load_ui_config(&store_with(include_str!("fixtures/current_full.json"))));
    let saved = gui.ui_config_json().expect("serializable");
    let v: serde_json::Value = serde_json::from_str(&saved).expect("valid JSON");
    assert!(
        v["panes"][0]["layer_slots"].is_array(),
        "the slot list is written as a list",
    );
    assert!(
        v["panes"][0]["layers"].is_object(),
        "and `layers` is still the object an older build expects: {}",
        v["panes"][0]["layers"],
    );
}

/// The live-chunk switch **fans out to every pane's radar slot**. One switch
/// has always meant every pane, and each pane carrying its own copy is only
/// safe while the setter writes all of them.
#[test]
fn the_live_chunk_switch_reaches_every_panes_radar_slot() {
    let store = store_with(include_str!("fixtures/live_chunks_off.json"));
    let mut gui = Gui::new();
    assert!(gui.load_ui_config(&store), "the two-pane fixture must load");
    for i in 0..2 {
        assert_eq!(
            gui.pane(i).expect("both panes").radar_live_chunks(),
            Some(false),
            "pane {i} did not take the migration's fan-out of the global",
        );
    }

    gui.set_live_chunks(true);
    for i in 0..2 {
        assert_eq!(
            gui.pane(i).expect("both panes").radar_live_chunks(),
            Some(true),
            "pane {i} kept the old value — the setter moved the global only, \
             and one toggle no longer means every pane",
        );
    }
    assert!(gui.live_chunks_enabled(), "the active pane answers");

    let v: serde_json::Value =
        serde_json::from_str(&gui.ui_config_json().expect("serializable")).expect("valid JSON");
    assert_eq!(v["live_chunks"], serde_json::json!(true));
    for i in 0..2 {
        assert_eq!(
            v["panes"][i]["layer_slots"]
                .as_array()
                .expect("a slot list")
                .iter()
                .find(|s| s["id"] == "Radar")
                .expect("a radar slot")["config"]["live_chunks"],
            serde_json::json!(true),
            "pane {i}'s slot did not reach the file",
        );
    }
}

/// **Two panes hold different answers for the same layer, and both survive the
/// reopen.** The load-bearing test of WO-M10b: before it, one registry field
/// answered for every pane, and a per-pane toggle only looked per-pane because
/// the config swap re-installed the right value before each read.
///
/// It has a **non-triviality floor**: the two panes are asserted to start in
/// agreement, so the divergence below cannot be one the fixture already had.
#[test]
fn two_panes_hold_different_answers_for_one_layer_and_both_survive_a_reopen() {
    let kind = known::CITY_LABELS;
    let store = store_with(include_str!("fixtures/live_chunks_off.json"));
    let mut gui = Gui::new();
    assert!(gui.load_ui_config(&store), "the two-pane fixture must load");

    let before: Vec<bool> = (0..2)
        .map(|i| gui.pane(i).expect("both panes").is_overlay_enabled(&kind))
        .collect();
    assert_eq!(
        before[0], before[1],
        "premise: the fixture's two panes agree about {kind:?}, or the \
         divergence asserted below was already in the file",
    );

    gui.set_overlay_on_pane_for_test(0, &kind, true);
    gui.set_overlay_on_pane_for_test(1, &kind, false);

    // Both halves: what the pane draws, and what the HANDLER answers when
    // asked about that pane. The second is the one that used to be a global.
    for (idx, want) in [(0usize, true), (1usize, false)] {
        let pane = gui.pane(idx).expect("both panes");
        assert_eq!(
            pane.is_overlay_enabled(&kind),
            want,
            "pane {idx}'s slot flag",
        );
        assert_eq!(
            gui.overlays.is_enabled(&kind, &pane.layer_ref(idx, &kind)),
            want,
            "the handler answered pane {idx} from some other pane's state — \
             this is exactly the bug the config swap was hiding",
        );
    }

    // **Put the registry's own copy at odds with the panes**, then re-run the
    // write-back every toggle and control edit ends with. Without this the
    // check below could not fail while the config swap was alive: the registry
    // happened to be holding the value the last pane wrote, so bytes taken
    // from the registry and bytes taken from the pane were the same bytes.
    // The swap died at WO-M10c and `self.enabled` is now only the layer's
    // default, but the disagreement is still what tells the two sources
    // apart, so it stays.
    gui.overlays
        .set_enabled(&kind, true, &mut rustdar_source::handler::PaneMut::bare(0));
    gui.readopt_panes_for_test();
    assert!(
        !gui.pane(1).expect("both panes").is_overlay_enabled(&kind),
        "pane 1 took the registry's copy on the write-back — its own state is \
         not what its slot is written from",
    );

    // The saved bytes carry both, in the shape they have always had.
    let json = gui.ui_config_json().expect("serializable");
    let v: serde_json::Value = serde_json::from_str(&json).expect("valid JSON");
    for (idx, want) in [(0usize, true), (1usize, false)] {
        assert_eq!(
            v["panes"][idx]["layer_slots"]
                .as_array()
                .expect("a slot list")
                .iter()
                .find(|s| s["id"] == kind.as_str())
                .expect("a slot for the layer")["config"]["enabled"],
            serde_json::json!(want),
            "pane {idx}'s config did not reach the file",
        );
    }

    // Reopen: a fresh build, the same file.
    let reopened = MemoryKvStore::default();
    reopened
        .store(UI_CONFIG_KEY, &json)
        .expect("the memory store accepts a write");
    let mut again = Gui::new();
    assert!(
        again.load_ui_config(&reopened),
        "the written file must reload"
    );
    for (idx, want) in [(0usize, true), (1usize, false)] {
        let pane = again.pane(idx).expect("both panes");
        assert_eq!(
            pane.is_overlay_enabled(&kind),
            want,
            "pane {idx} did not come back as it was left",
        );
        assert_eq!(
            again
                .overlays
                .is_enabled(&kind, &pane.layer_ref(idx, &kind)),
            want,
            "pane {idx}'s handler state did not survive the reopen",
        );
    }
}

/// **The two-panes-diverge test of WO-M10c: two panes, two HRRR parameters.**
///
/// The order's named subject, and the case the config swap could only fake.
/// Every read below goes through the pane — the controls model, the legend,
/// the cache token and the described job — and the swap symbols are absent
/// from the build (`arch_ratchets::the_config_swap_stays_deleted`), so nothing
/// is re-installing one pane's selection before each read.
///
/// **Non-triviality floor**: the two panes are asserted to agree before they
/// are diverged, and the parameter is set through `apply_control` on the real
/// slot state rather than by writing a field.
#[test]
fn two_panes_hold_different_hrrr_parameters_through_every_read_and_a_reopen() {
    use rustdar_overlays::hrrr::{GridCoords, HrrrFetchResult, HrrrGridData, ModelParameter};
    use rustdar_overlays::render::controls::{ControlItem, ControlUpdate, ControlValue};
    use rustdar_overlays::render::overlay_state::{OverlayFetchResult, RasterizeContext};

    let kind = known::MODEL_DATA;
    let left = ModelParameter::all()[0];
    let right = ModelParameter::all()[1];
    assert_ne!(left, right, "premise: two distinct parameters");

    fn grid(parameter: ModelParameter) -> HrrrGridData {
        let (ni, nj) = (4usize, 3usize);
        let values: Vec<f32> = (0..ni * nj).map(|k| 10.0 + k as f32).collect();
        let mut lats = Vec::new();
        let mut lons = Vec::new();
        for j in 0..nj {
            for i in 0..ni {
                lats.push(36.0 - 2.0 * (j as f64 / (nj - 1) as f64));
                lons.push(-98.0 + 2.0 * (i as f64 / (ni - 1) as f64));
            }
        }
        let (visible_points, value_range) =
            rustdar_overlays::hrrr::summarize_values(&values, parameter);
        HrrrGridData {
            parameter,
            values,
            coords: GridCoords::Explicit { lats, lons },
            ni,
            nj,
            bounds: rustdar_geo::GeoBounds {
                min_lat: 34.0,
                max_lat: 36.0,
                min_lon: -98.0,
                max_lon: -96.0,
            },
            ref_time: chrono::NaiveDate::from_ymd_opt(2026, 8, 20)
                .unwrap()
                .and_hms_opt(12, 0, 0)
                .unwrap(),
            forecast_hour: parameter.forecast_hour(),
            visible_points,
            value_range,
        }
    }

    let store = store_with(include_str!("fixtures/live_chunks_off.json"));
    let mut gui = Gui::new();
    assert!(gui.load_ui_config(&store), "the two-pane fixture must load");

    // Both grids resident, so a difference below is a difference in the
    // SELECTION and not in what happens to be cached.
    for parameter in [left, right] {
        gui.deliver_overlay_fetch(OverlayFetchResult {
            kind: kind.clone(),
            data: Box::new(HrrrFetchResult(Ok(grid(parameter)))),
        });
    }
    for idx in 0..2 {
        gui.set_overlay_on_pane_for_test(idx, &kind, true);
    }

    let selected = |gui: &Gui, idx: usize| -> String {
        let pane = gui.pane(idx).expect("both panes");
        let items = gui.overlays.controls(&kind, &pane.layer_ref(idx, &kind));
        fn find(items: &[ControlItem]) -> Option<String> {
            for item in items {
                match item {
                    ControlItem::Dropdown { id, selected, .. } if *id == "parameter" => {
                        return Some(selected.clone());
                    }
                    ControlItem::Section { items, .. } => {
                        if let Some(found) = find(items) {
                            return Some(found);
                        }
                    }
                    _ => {}
                }
            }
            None
        }
        find(&items).expect("the model layer offers a parameter dropdown")
    };

    assert_eq!(
        selected(&gui, 0),
        selected(&gui, 1),
        "premise: the two panes start on the same parameter, or the \
         divergence asserted below was already there",
    );

    // Through the same construction the inspector uses — the REAL slot state.
    for (idx, parameter) in [(0usize, left), (1usize, right)] {
        gui.apply_control_on_pane_for_test(
            idx,
            &kind,
            &ControlUpdate {
                id: "parameter",
                value: ControlValue::String(parameter.as_str().to_owned()),
            },
        );
    }

    let clock = chrono::NaiveDate::from_ymd_opt(2026, 8, 20)
        .unwrap()
        .and_hms_opt(12, 0, 0)
        .unwrap();
    let ctx = RasterizeContext {
        is_dark: false,
        zoom: 7.0,
        device_scale: 1.0,
        now: clock,
        as_of: clock,
    };
    let mut tokens = Vec::new();
    for (idx, parameter) in [(0usize, left), (1usize, right)] {
        let pane = gui.pane(idx).expect("both panes");
        let view = pane.layer_ref(idx, &kind);
        assert_eq!(
            selected(&gui, idx),
            parameter.as_str(),
            "pane {idx}'s controls offer another pane's parameter",
        );
        assert_eq!(
            gui.overlays.status_line(&kind, &view).as_deref(),
            Some(parameter.display_name()),
            "pane {idx}'s status line",
        );
        assert_eq!(
            gui.overlays.legend(&kind, &view).map(|l| l.signature),
            Some(parameter as u64 + 1),
            "pane {idx}'s legend signature",
        );
        assert!(
            gui.overlays.prepare_job(&kind, &ctx, &view).is_some(),
            "pane {idx} would be skipped by the render dispatch",
        );
        tokens.push(gui.overlays.content_signature(&kind, &view));
    }
    assert_ne!(
        tokens[0], tokens[1],
        "the two panes share one cache token, so the dispatch would group \
         them and hand both the same raster",
    );

    // The saved bytes carry both, and a reopen brings them back.
    let json = gui.ui_config_json().expect("serializable");
    let v: serde_json::Value = serde_json::from_str(&json).expect("valid JSON");
    for (idx, parameter) in [(0usize, left), (1usize, right)] {
        assert_eq!(
            v["panes"][idx]["layer_slots"]
                .as_array()
                .expect("a slot list")
                .iter()
                .find(|s| s["id"] == kind.as_str())
                .expect("a model slot")["config"]["parameter"],
            serde_json::json!(parameter.as_str()),
            "pane {idx}'s parameter did not reach the file",
        );
    }

    let reopened = MemoryKvStore::default();
    reopened
        .store(UI_CONFIG_KEY, &json)
        .expect("the memory store accepts a write");
    let mut again = Gui::new();
    assert!(
        again.load_ui_config(&reopened),
        "the written file must reload"
    );
    for (idx, parameter) in [(0usize, left), (1usize, right)] {
        assert_eq!(
            selected(&again, idx),
            parameter.as_str(),
            "pane {idx} did not come back on the parameter it was left on",
        );
    }
}
