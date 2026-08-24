use crate::UI_CONFIG_KEY;
use squallar_kv::{KvStore, MemoryKvStore};
use squallar_radar::fields as radar_fields;
use squallar_source::id::known;

/// Settings the user changed must come back after a save/load cycle.
#[test]
fn changed_settings_survive_a_save_and_load() {
    use crate::pane::{OrbitDelta, PaneKind};

    let store = MemoryKvStore::default();

    let baseline = crate::Gui::new();
    assert_ne!(baseline.loop_lookback_secs, 7200);
    assert_ne!(baseline.loop_speed_fps, 12.5);
    assert!(
        baseline.pane(0).unwrap().viewport_link && baseline.pane(0).unwrap().layer_link,
        "default is linked; test flips both off"
    );
    assert_eq!(
        baseline.pane(0).unwrap().kind(),
        PaneKind::Map,
        "default is a map; test converts it"
    );

    let mut gui = crate::Gui::new();
    gui.loop_lookback_secs = 7200;
    gui.loop_speed_fps = 12.5;
    gui.pane_mut(0).unwrap().viewport_link = false;
    gui.pane_mut(0).unwrap().layer_link = false;
    gui.pane_mut(0)
        .unwrap()
        .set_view(squallar_radar::types::RenderView::Volume);
    let nudged = {
        let volume = gui.pane_mut(0).unwrap().volume_mut().expect("converted");
        volume.camera.nudge(OrbitDelta {
            yaw_deg: -47.5,
            pitch_deg: 12.25,
            zoom_factor: 1.5,
            pan: [0.2, -0.35, 0.1],
        });
        volume.camera.set_vertical_exaggeration(5.5);
        volume.camera
    };
    assert_ne!(
        nudged,
        crate::pane::OrbitCamera::default(),
        "precondition: the camera must differ from the default"
    );
    gui.save_ui_config(&store);

    let mut restored = crate::Gui::new();
    restored.load_ui_config(&store);

    assert_eq!(restored.loop_lookback_secs, 7200);
    assert_eq!(restored.loop_speed_fps, 12.5);
    assert!(
        !restored.pane(0).unwrap().viewport_link && !restored.pane(0).unwrap().layer_link,
        "the per-pane links must survive the round trip"
    );
    assert_eq!(
        restored.pane(0).unwrap().render_view(),
        squallar_radar::types::RenderView::Volume
    );
    assert_eq!(
        restored.pane(0).unwrap().volume().map(|v| v.camera),
        Some(nudged),
        "the pane came back as a 3D view aimed somewhere else"
    );
}

/// M11-3. **An old config's `viewport_sync: false` loads as every restored pane
/// viewport-unlinked, and `sync_layers: false` as every pane layer- and
/// time-unlinked — the retired globals fold into the per-pane links once, on
/// load.**
#[test]
fn a_legacy_global_off_seeds_every_restored_panes_links_off() {
    let store = MemoryKvStore::default();
    store
        .store(
            UI_CONFIG_KEY,
            r#"{"pane_count":2,"site":"KMPX","viewport_sync":false,
                    "panes":[{"site":"KMPX"},{"site":"KOUN","time_link":true}]}"#,
        )
        .unwrap();
    let mut restored = crate::Gui::new();
    assert!(restored.load_ui_config(&store));
    for idx in 0..2 {
        let pane = restored.pane(idx).unwrap();
        assert!(
            !pane.viewport_link,
            "pane {idx}: the legacy viewport_sync=false must seed the link off"
        );
        assert!(
            pane.layer_link && pane.time_link,
            "pane {idx}: the other dimensions' links are not viewport_sync's \
                 to seed"
        );
    }

    let store = MemoryKvStore::default();
    store
        .store(
            UI_CONFIG_KEY,
            r#"{"pane_count":2,"site":"KMPX","sync_layers":false,
                    "panes":[{"site":"KMPX","time_link":true}]}"#,
        )
        .unwrap();
    let mut restored = crate::Gui::new();
    assert!(restored.load_ui_config(&store));
    for idx in 0..2 {
        let pane = restored.pane(idx).unwrap();
        assert!(
            !pane.layer_link && !pane.time_link,
            "pane {idx}: the legacy sync_layers=false must seed the layer \
                 and time links off"
        );
        assert!(
            pane.viewport_link,
            "pane {idx}: the viewport link is not sync_layers' to seed"
        );
    }
}

/// M11-4. **A config with no legacy globals — one this build wrote, or an old one
/// that simply never mentioned them — loads with every pane linked, and the legacy
/// fields are never written again.**
///
/// Extended at WO-SYNCGROUP with the group half of the same claim: a file
/// written before groups existed names no group, and every one of its panes
/// must come back **in group A**, not out of every group. That is the whole
/// migration — one group holding everybody is exactly what three per-pane
/// booleans with no group described — and it is why there is no `migrate.rs`
/// step and no `CONFIG_VERSION` bump for the field.
#[test]
fn absent_legacy_globals_mean_linked_and_are_never_rewritten() {
    use crate::pane::GroupId;

    let store = MemoryKvStore::default();
    store
        .store(
            UI_CONFIG_KEY,
            r#"{"pane_count":2,"site":"KMPX",
                    "panes":[{"site":"KMPX"},{"site":"KOUN"}]}"#,
        )
        .unwrap();
    let mut restored = crate::Gui::new();
    assert!(restored.load_ui_config(&store));
    for idx in 0..2 {
        let pane = restored.pane(idx).unwrap();
        assert!(
            pane.viewport_link && pane.layer_link && pane.time_link,
            "pane {idx}: absent legacy fields must load as all-linked"
        );
        assert_eq!(
            pane.group,
            Some(GroupId::FIRST),
            "pane {idx}: a file that names no group must load as one group \
             holding every pane - anything else changes what an old config \
             means"
        );
    }
    // Behaviour, not just the field: the two panes reach each other.
    assert!(
        restored.panes_layer_linked(0, 1) && restored.panes_time_linked(0, 1),
        "the migrated panes must actually sync with one another, which is \
         what the flags alone used to say"
    );

    let json = restored.ui_config_json().expect("serializable");
    assert!(
        !json.contains("\"viewport_sync\"") && !json.contains("\"sync_layers\""),
        "the retired globals must never be written again"
    );
    assert!(
        json.contains("\"viewport_link\"") && json.contains("\"layer_link\""),
        "the per-pane links are the persisted state now"
    );
    assert!(
        json.contains("\"group\""),
        "and the group is persisted beside them, or the next reopen loses \
         which panes were together"
    );
}

/// **A pane's group survives a save and a load, including the pane that is in
/// no group at all** — the state `#[serde(default)]` alone would silently
/// convert back into group A on every restart.
#[test]
fn the_group_round_trips_including_the_pane_that_is_in_none() {
    use crate::pane::GroupId;

    let second = GroupId::from_index(1).expect("a layout can hold six groups");
    let store = MemoryKvStore::default();
    let mut gui = crate::Gui::new();
    gui.set_pane_count_for_test(3);
    gui.pane_mut(1).expect("pane 1").group = Some(second);
    gui.pane_mut(2).expect("pane 2").group = None;
    gui.save_ui_config(&store);

    let mut restored = crate::Gui::new();
    restored.set_pane_count_for_test(3);
    assert!(restored.load_ui_config(&store));
    assert_eq!(
        restored.pane(0).expect("pane 0").group,
        Some(GroupId::FIRST),
    );
    assert_eq!(restored.pane(1).expect("pane 1").group, Some(second));
    assert_eq!(
        restored.pane(2).expect("pane 2").group,
        None,
        "the pane in no group came back in one - a reopen that re-links what \
         the user unlinked is not 1:1"
    );
    assert!(
        !restored.panes_share_group(0, 1) && !restored.panes_share_group(0, 2),
        "and the restored panes must not reach each other"
    );
}

/// A group index no layout can reach — a file from a build with more panes,
/// or a hand-edited one — loads as no group rather than panicking on the
/// palette's bounds.
#[test]
fn a_group_index_this_build_has_no_letter_for_loads_as_no_group() {
    let store = MemoryKvStore::default();
    store
        .store(
            UI_CONFIG_KEY,
            r#"{"pane_count":1,"panes":[{"site":"KMPX","group":250}]}"#,
        )
        .unwrap();
    let mut restored = crate::Gui::new();
    assert!(restored.load_ui_config(&store));
    assert_eq!(
        restored.pane(0).expect("pane 0").group,
        None,
        "an unreachable index must degrade to no group"
    );
}

/// A drawn Volume Alpha curve survives the round trip, per product, and an
/// untouched product comes back untouched.
#[test]
fn volume_alpha_curves_survive_a_save_and_load() {
    use crate::volume_alpha::{AlphaCurve, CURVE_LEN};

    let store = MemoryKvStore::default();
    let mut gui = crate::Gui::new();
    let mut alphas = [0u8; CURVE_LEN];
    for (i, slot) in alphas.iter_mut().enumerate() {
        *slot = (i / 2) as u8; // a curve no default produces
    }
    let curve = AlphaCurve::from_alphas(alphas);
    gui.volume_alpha
        .set(&radar_fields::known::REFLECTIVITY, curve.clone());
    gui.save_ui_config(&store);

    let mut restored = crate::Gui::new();
    assert!(restored.load_ui_config(&store));
    assert_eq!(
        restored
            .volume_alpha
            .get(&radar_fields::known::REFLECTIVITY),
        Some(curve),
        "the drawn curve must come back exactly",
    );
    assert_eq!(
        restored.volume_alpha.get(&radar_fields::known::VELOCITY),
        None,
        "a product the user never edited must come back with no curve at all",
    );
}

/// A config written before Volume Alpha existed loads with every editor untouched —
/// the field defaults to empty, and empty means bit-exact.
#[test]
fn an_old_config_without_volume_alpha_loads_with_every_editor_untouched() {
    let store = MemoryKvStore::default();
    store
        .store(UI_CONFIG_KEY, "{}")
        .expect("the memory store accepts a write");

    let mut gui = crate::Gui::new();
    assert!(gui.load_ui_config(&store), "an old config still loads");
    assert!(
        !gui.volume_alpha
            .is_edited(&radar_fields::known::REFLECTIVITY),
        "an old config must not conjure a curve for any product",
    );
}

/// A hand-edited or version-skewed curve cannot poison the load: a wrong length is
/// dropped by name, and a curve claiming a visible no-data index is re-clamped on
/// the way in.
#[test]
fn a_hostile_volume_alpha_entry_is_dropped_or_reclamped_never_trusted() {
    let store = MemoryKvStore::default();
    let mut full: Vec<String> = vec!["255".to_owned(); 256];
    full[1] = "9".to_owned();
    let json = format!(
        r#"{{"volume_alpha":[
                {{"product":"Reflectivity","alpha":[1,2,3]}},
                {{"product":"Velocity","alpha":[{}]}}
            ]}}"#,
        full.join(","),
    );
    store
        .store(UI_CONFIG_KEY, &json)
        .expect("the memory store accepts a write");

    let mut gui = crate::Gui::new();
    assert!(gui.load_ui_config(&store), "the rest of the config loads");
    assert_eq!(
        gui.volume_alpha.get(&radar_fields::known::REFLECTIVITY),
        None,
        "a wrong-length curve must be dropped, not padded or truncated",
    );
    let velocity = gui
        .volume_alpha
        .get(&radar_fields::known::VELOCITY)
        .expect("a well-sized curve loads");
    assert_eq!(
        velocity.alphas()[0],
        0,
        "entry 0 is the no-data index and must be re-clamped on load",
    );
    assert_eq!(
        velocity.alphas()[1],
        9,
        "the rest of the curve is kept as saved"
    );
    assert_eq!(velocity.alphas()[255], 255);
}

/// A 3D pane's view mode and the per-product isosurface thresholds survive the
/// round trip; an untouched product comes back untouched.
#[test]
fn the_isosurface_mode_and_thresholds_survive_a_save_and_load() {
    use crate::pane::VolumeViewMode;

    let store = MemoryKvStore::default();
    let mut gui = crate::Gui::new();
    gui.pane_mut(0)
        .unwrap()
        .set_view(squallar_radar::types::RenderView::Volume);
    gui.pane_mut(0).unwrap().volume_mut().unwrap().view_mode = VolumeViewMode::Isosurface;
    gui.volume_iso.set(&radar_fields::known::VELOCITY, 35.0);
    assert_ne!(
        squallar_radar::voxel::default_iso_threshold(
            squallar_radar::fields::product_for(&radar_fields::known::VELOCITY)
                .expect("a registered field"),
        ),
        35.0,
        "precondition: the saved threshold must differ from the default",
    );
    gui.save_ui_config(&store);

    let mut restored = crate::Gui::new();
    assert!(restored.load_ui_config(&store));
    assert_eq!(
        restored.pane(0).unwrap().volume().unwrap().view_mode,
        VolumeViewMode::Isosurface,
        "a pane set to isosurface must come back one",
    );
    assert_eq!(
        restored.volume_iso.get(&radar_fields::known::VELOCITY),
        35.0
    );
    assert!(
        !restored
            .volume_iso
            .is_edited(&radar_fields::known::REFLECTIVITY),
        "an untouched product must come back at the argued default",
    );
}

/// A 3D pane that turned its map floor off comes back with it off.
#[test]
fn a_hidden_map_floor_survives_a_save_and_load() {
    let store = MemoryKvStore::default();
    let mut gui = crate::Gui::new();
    gui.set_pane_count_for_test(2);
    gui.pane_mut(0)
        .unwrap()
        .set_view(squallar_radar::types::RenderView::Volume);
    gui.pane_mut(1)
        .unwrap()
        .set_view(squallar_radar::types::RenderView::Volume);
    assert!(
        !gui.pane(0).unwrap().volume().unwrap().hide_floor,
        "precondition: a fresh 3D pane shows its floor",
    );
    gui.pane_mut(0).unwrap().volume_mut().unwrap().hide_floor = true;
    gui.save_ui_config(&store);

    let mut restored = crate::Gui::new();
    assert!(restored.load_ui_config(&store));
    assert!(
        restored.pane(0).unwrap().volume().unwrap().hide_floor,
        "a pane that turned the floor off must come back with it off",
    );
    assert!(
        !restored.pane(1).unwrap().volume().unwrap().hide_floor,
        "and the toggle is per pane: its neighbour keeps its floor",
    );
}

/// The derived-rung choice survives a save and load, both directions.
#[test]
fn the_storm_motion_fallback_survives_a_save_and_load() {
    use squallar_radar::srv::SrvFallback;

    let store = MemoryKvStore::default();
    let mut gui = crate::Gui::new();
    assert_eq!(
        gui.srv_fallback,
        SrvFallback::MeanWind,
        "precondition: a fresh session falls to the mean wind",
    );
    gui.srv_fallback = SrvFallback::BunkersRightMover;
    gui.save_ui_config(&store);

    let mut restored = crate::Gui::new();
    assert!(restored.load_ui_config(&store));
    assert_eq!(
        restored.srv_fallback,
        SrvFallback::BunkersRightMover,
        "a reader who asked for the right-mover must come back to it",
    );

    restored.srv_fallback = SrvFallback::MeanWind;
    restored.save_ui_config(&store);
    let mut again = crate::Gui::new();
    assert!(again.load_ui_config(&store));
    assert_eq!(again.srv_fallback, SrvFallback::MeanWind);
}

/// A config written before the choice existed comes back on the mean wind, and one
/// naming a rung this build does not have does **not** cost the whole file.
#[test]
fn a_config_from_another_build_still_loads_its_storm_motion_fallback() {
    use squallar_radar::srv::SrvFallback;

    for (json, why) in [
        (
            r#"{"site": "KDMX"}"#,
            "an absent key must mean the shipped default",
        ),
        (
            r#"{"site": "KDMX", "srv_fallback": "LeftMover"}"#,
            "a rung from a newer build must not cost the file",
        ),
    ] {
        let store = MemoryKvStore::default();
        store
            .store(UI_CONFIG_KEY, json)
            .expect("the memory store accepts a write");
        let mut gui = crate::Gui::new();
        assert!(gui.load_ui_config(&store), "{why}");
        assert_eq!(gui.srv_fallback, SrvFallback::MeanWind, "{why}");
        assert_eq!(
            gui.pane(0).map(|p| p.site().to_string()),
            Some("KDMX".to_owned()),
            "{why}: the rest of the config was lost",
        );
    }
}

/// A config written before `hide_floor` existed comes back with the floor
/// **showing**.
#[test]
fn a_config_written_before_the_floor_toggle_keeps_its_floor() {
    let store = MemoryKvStore::default();
    store
        .store(
            UI_CONFIG_KEY,
            r#"{
                    "site": "KDMX",
                    "panes": [{"kind": "Volume", "site": "KDMX", "volume": {}}]
                }"#,
        )
        .expect("the memory store accepts a write");

    let mut gui = crate::Gui::new();
    assert!(gui.load_ui_config(&store));
    assert!(
        !gui.pane(0).unwrap().volume().unwrap().hide_floor,
        "an absent key must mean the shipped default: the floor shows",
    );
}

/// A config from a build in which 3D was a **pane kind** comes back as a map pane
/// in the 3D render mode — the same picture, with its camera.
#[test]
fn a_config_naming_the_old_3d_pane_kind_comes_back_as_a_3d_render_mode() {
    use crate::pane::{MapRender, OrbitCamera, PaneKind};

    let store = MemoryKvStore::default();
    store
        .store(
            UI_CONFIG_KEY,
            r#"{
                "site": "KDMX",
                "pane_count": 2,
                "panes": [
                    {
                        "kind": "Volume",
                        "site": "KDMX",
                        "volume": {
                            "yaw_deg": 300.0,
                            "pitch_deg": 40.0,
                            "eye_distance": 1.75,
                            "pivot": [0.25, -0.5, 0.125],
                            "vertical_exaggeration": 6.5,
                            "region": {
                                "centre_lat": 41.73,
                                "centre_lon": -93.72,
                                "half_width_km": 25.0
                            },
                            "source_pane": 0,
                            "hide_floor": true
                        }
                    },
                    {"kind": "Map", "site": "KTLX"}
                ]
            }"#,
        )
        .expect("the memory store accepts a write");

    let mut gui = crate::Gui::new();
    assert!(
        gui.load_ui_config(&store),
        "a config naming the removed 3D pane kind must still load — the whole \
         file, not just the pane"
    );

    let pane = gui.pane(0).expect("pane 0");
    assert_eq!(
        pane.kind(),
        PaneKind::Map,
        "a 3D view is a map pane now: it was always looking at a patch of ground",
    );
    assert_eq!(
        pane.map_render(),
        Some(MapRender::Volume),
        "the pane must come back drawing its volume, not silently flattened to \
         the plan view",
    );
    assert_eq!(
        pane.render_view(),
        squallar_radar::types::RenderView::Volume,
        "and the render dispatched for it must be the raymarch",
    );

    let volume = pane.volume().expect("a pane in the 3D render mode");
    let expected = OrbitCamera::restore(300.0, 40.0, 1.75, [0.25, -0.5, 0.125], 6.5)
        .expect("the fixture's camera is finite and in range");
    assert_eq!(
        volume.camera, expected,
        "the camera the user aimed must survive the migration",
    );
    assert!(
        volume.hide_floor,
        "and so must the floor they turned off — through the inverted key, \
         unchanged on the wire",
    );
    assert_eq!(
        volume.region, None,
        "the square-drag region block was resurrected as a two-axis pick - the \
         extent keys it does not have were laundered through the extent clamp",
    );

    let sibling = gui.pane(1).expect("pane 1");
    assert_eq!(sibling.kind(), PaneKind::Map);
    assert_eq!(sibling.map_render(), Some(MapRender::Plan));
}

/// Saving after that migration writes the **new** vocabulary, so the legacy name is
/// read once and never again.
#[test]
fn a_migrated_3d_pane_is_saved_in_the_new_vocabulary() {
    use crate::pane::MapRender;

    let store = MemoryKvStore::default();
    store
        .store(
            UI_CONFIG_KEY,
            r#"{"site": "KDMX", "panes": [{"kind": "Volume", "site": "KDMX", "volume": {}}]}"#,
        )
        .expect("the memory store accepts a write");
    let mut gui = crate::Gui::new();
    assert!(gui.load_ui_config(&store));

    let json = gui.ui_config_json().expect("the config must be writable");
    let written: serde_json::Value = serde_json::from_str(&json).expect("valid JSON");
    assert_eq!(
        written["panes"][0]["kind"], "Map",
        "the pane must be saved as the kind it is",
    );
    assert_eq!(
        written["panes"][0]["render"], "Volume",
        "with the render mode carrying what the kind used to",
    );

    let again = MemoryKvStore::default();
    again.store(UI_CONFIG_KEY, &json).expect("storable");
    let mut restored = crate::Gui::new();
    assert!(restored.load_ui_config(&again));
    assert_eq!(
        restored.pane(0).expect("pane 0").map_render(),
        Some(MapRender::Volume),
    );
}

/// A view mode from a future build loads as the lit volume, and a threshold for an
/// unknown product is dropped — the same forward tolerance the product enum has,
/// pinned for the two new fields.
#[test]
fn an_unknown_view_mode_or_iso_product_does_not_poison_the_load() {
    use crate::pane::VolumeViewMode;

    let store = MemoryKvStore::default();
    store
        .store(
            UI_CONFIG_KEY,
            r#"{
                    "site": "KDMX",
                    "panes": [{
                        "kind": "Volume",
                        "site": "KDMX",
                        "volume": {"view_mode": "HolographicSlices"}
                    }],
                    "volume_iso": [
                        {"product": "TornadoProbability", "threshold": 5.0},
                        {"product": "Velocity", "threshold": 30.0}
                    ]
                }"#,
        )
        .expect("the memory store accepts a write");

    let mut gui = crate::Gui::new();
    assert!(
        gui.load_ui_config(&store),
        "an unknown view mode or product name must not fail the load",
    );
    assert_eq!(
        gui.pane(0).unwrap().volume().unwrap().view_mode,
        VolumeViewMode::LitVolume,
        "an unknown mode falls back to the lit volume",
    );
    assert_eq!(
        gui.volume_iso.get(&radar_fields::known::VELOCITY),
        30.0,
        "the entry beside the unknown one still loads",
    );
    // The guarantee that mattered, unchanged: never reassigned.
    for product in radar_fields::known::ALL.iter() {
        if *product == radar_fields::known::VELOCITY {
            continue;
        }
        assert!(
            !gui.volume_iso.is_edited(product),
            "the unknown threshold was reassigned to {product:?}",
        );
    }
    // And the open-id doctrine in place of the drop: preserved inert.
    assert_eq!(
        gui.volume_iso.entries().count(),
        2,
        "the unknown field's threshold is preserved inert, not dropped",
    );
    assert_eq!(
        gui.volume_iso.get(&squallar_source::product::FieldId::new(
            "TornadoProbability"
        )),
        5.0,
    );
}

/// A config naming a product this build does not know still loads.
#[test]
fn a_config_naming_a_product_from_the_future_still_loads() {
    let store = MemoryKvStore::default();
    store
        .store(
            UI_CONFIG_KEY,
            r#"{
                    "site": "KDMX",
                    "loop_lookback_secs": 7200,
                    "panes": [{"selected_product": "TornadoProbability", "site": "KDMX"}]
                }"#,
        )
        .expect("the memory store accepts a write");

    let mut gui = crate::Gui::new();
    assert!(
        gui.load_ui_config(&store),
        "one unknown product name must not fail the whole config load",
    );
    assert_eq!(
        gui.pane(0).unwrap().selected_product(),
        radar_fields::known::REFLECTIVITY,
        "the unknown product falls back to the default product",
    );
    assert_eq!(
        gui.loop_lookback_secs, 7200,
        "the rest of the file must survive the unknown product",
    );
}

/// A config naming a **pane kind** this build does not know still loads.
#[test]
fn a_config_naming_a_pane_kind_from_the_future_still_loads() {
    let store = MemoryKvStore::default();
    store
        .store(
            UI_CONFIG_KEY,
            r#"{
                    "site": "KDMX",
                    "loop_lookback_secs": 7200,
                    "pane_count": 2,
                    "panes": [
                        {"site": "KDMX", "kind": "Hologram"},
                        {"site": "KDMX", "kind": "Volume", "volume": {"hide_floor": true}}
                    ]
                }"#,
        )
        .expect("the memory store accepts a write");

    let mut gui = crate::Gui::new();
    assert!(
        gui.load_ui_config(&store),
        "one unknown pane kind must not fail the whole config load",
    );
    assert_eq!(
        gui.pane(0).map(crate::pane::PaneState::kind),
        Some(crate::pane::PaneKind::Map),
        "the unknown kind falls back to a map pane",
    );
    assert_eq!(
        gui.pane(1).map(crate::pane::PaneState::render_view),
        Some(squallar_radar::types::RenderView::Volume),
        "the pane beside the unknown one keeps its own view",
    );
    assert_eq!(
        gui.loop_lookback_secs, 7200,
        "the rest of the file must survive the unknown pane kind",
    );
    assert_eq!(
        gui.pane(0).map(|pane| pane.site()),
        Some("KDMX"),
        "the unreadable pane keeps every field that was readable",
    );
}

/// A hand-edited `site` made of bytes no identifier contains is refused here, at
/// the file, and never reaches the byte range that used to split it.
#[test]
fn a_config_naming_a_site_no_radar_could_have_is_refused_not_sliced() {
    let store = MemoryKvStore::default();
    store
        .store(
            UI_CONFIG_KEY,
            r#"{
                    "site": "éab",
                    "loop_lookback_secs": 7200,
                    "pane_count": 2,
                    "panes": [
                        {"site": "Ω12"},
                        {"site": "KDMX"}
                    ]
                }"#,
        )
        .expect("the memory store accepts a write");

    let mut gui = crate::Gui::new();
    let default_site = crate::Gui::new()
        .pane(0)
        .map(|pane| pane.site().to_string());
    assert!(
        gui.load_ui_config(&store),
        "one impossible site must not fail the whole config load",
    );

    let restored = gui
        .pane(0)
        .map(|pane| pane.site().to_string())
        .expect("a pane");

    assert!(
        squallar_radar::level3::site_code(&restored).is_ascii(),
        "the surviving identifier must reduce cleanly",
    );
    assert!(
        restored.is_ascii(),
        "no non-ASCII identifier may survive the load: {restored:?}",
    );
    assert_eq!(
        Some(&restored),
        default_site.as_ref(),
        "a refused site leaves the pane on whatever startup picked",
    );

    assert_eq!(
        gui.pane(1).map(|pane| pane.site()),
        Some("KDMX"),
        "the pane beside the refused one keeps its own site",
    );
    assert_eq!(
        gui.loop_lookback_secs, 7200,
        "the rest of the file must survive the refused site",
    );
}

/// An alpha curve saved for an unknown product is dropped, never reassigned to a
/// product this build knows.
#[test]
fn an_alpha_curve_for_an_unknown_product_is_dropped_not_reassigned() {
    let store = MemoryKvStore::default();
    let full: Vec<String> = vec!["128".to_owned(); 256];
    let json = format!(
        r#"{{"volume_alpha":[
                {{"product":"TornadoProbability","alpha":[{alphas}]}},
                {{"product":"Velocity","alpha":[{alphas}]}}
            ]}}"#,
        alphas = full.join(","),
    );
    store
        .store(UI_CONFIG_KEY, &json)
        .expect("the memory store accepts a write");

    let mut gui = crate::Gui::new();
    assert!(gui.load_ui_config(&store), "the rest of the config loads");
    assert!(
        gui.volume_alpha
            .get(&radar_fields::known::VELOCITY)
            .is_some(),
        "the entry beside the unknown one still loads",
    );

    // **The guarantee, unchanged**: a curve saved under a name this build does
    // not know is never applied to a field this build DOES know.
    for product in radar_fields::known::ALL.iter() {
        if *product == radar_fields::known::VELOCITY {
            continue;
        }
        assert!(
            !gui.volume_alpha.is_edited(product),
            "the unknown entry was remapped onto {product:?}",
        );
    }

    // **What changed, and it is the open-id doctrine**: the entry is no longer
    // DROPPED. It is kept inert -- applying to nothing, because no pane can
    // select a field the registry does not offer -- and written back verbatim,
    // so a curve drawn on a newer build survives a session under this one.
    assert_eq!(
        gui.volume_alpha.entries().count(),
        2,
        "the unknown entry must be preserved beside the known one, not dropped",
    );
    assert!(
        gui.volume_alpha
            .is_edited(&squallar_source::product::FieldId::new(
                "TornadoProbability"
            )),
        "the unknown entry is kept under its own id",
    );
    let saved = gui.ui_config_json().expect("serializable");
    let value: serde_json::Value = serde_json::from_str(&saved).expect("valid JSON");
    let names: Vec<&str> = value["volume_alpha"]
        .as_array()
        .expect("an array")
        .iter()
        .map(|e| e["product"].as_str().expect("a bare string"))
        .collect();
    assert_eq!(
        names,
        ["Velocity", "TornadoProbability"],
        "both entries are written back, the known one under its code order and \
         the unknown one after it",
    );
}

/// A cross-section pane's line and source survive the round trip.
#[test]
fn a_drawn_section_line_survives_a_save_and_load() {
    use crate::pane::{PaneKind, SectionLine};
    use squallar_geo::GeoPoint;

    let store = MemoryKvStore::default();
    let a = GeoPoint {
        lat: 35.0,
        lon: -97.8,
    };
    let b = GeoPoint {
        lat: 35.6,
        lon: -96.9,
    };

    let mut gui = crate::Gui::new();
    gui.set_pane_count_for_test(2);
    gui.pane_mut(1).unwrap().set_kind(PaneKind::CrossSection);
    {
        let section = gui
            .pane_mut(1)
            .unwrap()
            .cross_section_mut()
            .expect("converted");
        section.line = SectionLine::new(a, b);
        section.source_pane = Some(0);
    }
    assert_eq!(
        gui.pane(0).unwrap().kind(),
        PaneKind::Map,
        "precondition: the other pane stays a map, so the kind is per pane"
    );
    gui.save_ui_config(&store);

    let mut restored = crate::Gui::new();
    restored.load_ui_config(&store);

    assert_eq!(restored.pane(0).unwrap().kind(), PaneKind::Map);
    let section = restored
        .pane(1)
        .unwrap()
        .cross_section()
        .expect("pane 1 came back as something other than a section");
    assert_eq!(
        section.line.map(|line| (line.a(), line.b())),
        Some((a, b)),
        "the line came back somewhere else"
    );
    assert_eq!(section.source_pane, Some(0));
    assert_eq!(
        section.rendered_for, None,
        "the staleness key must not be persisted: it names a volume that is \
             not loaded, so a restored pane would think its image was current"
    );
}

/// Every shape a config can describe that the in-memory representation cannot, and
/// each one falls back to a map rather than failing the load.
#[test]
fn a_pane_config_that_cannot_be_a_pane_loads_as_a_map() {
    use crate::pane::PaneKind;

    for (name, pane_json) in [
        (
            "a section with no section state at all",
            r#"{"kind":"CrossSection"}"#,
        ),
        ("a 3D view with no camera", r#"{"kind":"Volume"}"#),
        (
            "a section line off the earth, which walks a well-defined great \
                 circle over nowhere and renders as empty coverage",
            r#"{"kind":"CrossSection","cross_section":{"line":
                   {"a_lat":1e9,"a_lon":-97.8,"b_lat":35.6,"b_lon":-96.9}}}"#,
        ),
        (
            "a zero-length section line, which has no bearing to walk along",
            r#"{"kind":"CrossSection","cross_section":{"line":
                   {"a_lat":35.0,"a_lon":-97.8,"b_lat":35.0,"b_lon":-97.8}}}"#,
        ),
    ] {
        let store = MemoryKvStore::default();
        store
            .store(
                UI_CONFIG_KEY,
                &format!(r#"{{"pane_count":1,"site":"KTLX","panes":[{pane_json}]}}"#),
            )
            .unwrap();

        let mut restored = crate::Gui::new();
        assert!(
            restored.load_ui_config(&store),
            "{name}: the config must still load — falling back is per pane, \
                 not a refusal of the file"
        );
        assert_eq!(
            restored.pane(0).unwrap().kind(),
            PaneKind::Map,
            "{name}: loaded as a pane whose kind and state disagree"
        );
        assert_eq!(
            restored.pane(0).unwrap().site(),
            "KTLX",
            "{name}: the rest of the pane was lost with its kind"
        );
    }
}

/// A section pane converted but not yet aimed is an ordinary state, not a corrupt
/// one.
#[test]
fn a_section_pane_with_no_line_yet_comes_back_as_a_section() {
    use crate::pane::PaneKind;

    let store = MemoryKvStore::default();
    store
        .store(
            UI_CONFIG_KEY,
            r#"{"pane_count":1,"site":"KTLX",
                    "panes":[{"kind":"CrossSection","cross_section":{}}]}"#,
        )
        .unwrap();

    let mut restored = crate::Gui::new();
    restored.load_ui_config(&store);

    let section = restored
        .pane(0)
        .unwrap()
        .cross_section()
        .expect("an unaimed section is a section");
    assert!(section.line.is_none());
    assert_eq!(restored.pane(0).unwrap().kind(), PaneKind::CrossSection);
}

/// A source-pane index the restored layout does not have is forgotten, and the pane
/// stays a section.
#[test]
fn a_section_sourced_from_a_pane_that_is_gone_forgets_where_it_came_from() {
    use crate::pane::PaneKind;

    let store = MemoryKvStore::default();
    store
        .store(
            UI_CONFIG_KEY,
            r#"{"pane_count":1,"site":"KTLX","panes":[
                    {"kind":"CrossSection","cross_section":{
                        "line":{"a_lat":35.0,"a_lon":-97.8,"b_lat":35.6,"b_lon":-96.9},
                        "source_pane":5}}]}"#,
        )
        .unwrap();

    let mut restored = crate::Gui::new();
    restored.load_ui_config(&store);

    assert_eq!(
        restored.pane_count(),
        1,
        "precondition: one pane, so 5 is out"
    );
    let section = restored
        .pane(0)
        .unwrap()
        .cross_section()
        .expect("the kind survives a stale source index");
    assert_eq!(section.source_pane, None);
    assert!(
        section.line.is_some(),
        "the line is still a line; only where it was drawn was lost"
    );
    assert_eq!(restored.pane(0).unwrap().kind(), PaneKind::CrossSection);
}

/// A config written before pane kinds existed loads as a screen full of maps.
#[test]
fn a_config_predating_pane_kinds_loads_as_maps() {
    use crate::pane::PaneKind;

    let store = MemoryKvStore::default();
    store
        .store(
            UI_CONFIG_KEY,
            r#"{"pane_count":2,"site":"KMPX",
                    "panes":[{"site":"KMPX","zoom":7.0},{"site":"KOUN"}]}"#,
        )
        .unwrap();

    let mut restored = crate::Gui::new();
    assert!(restored.load_ui_config(&store));

    assert_eq!(
        (0..2)
            .map(|i| restored.pane(i).unwrap().kind())
            .collect::<Vec<_>>(),
        vec![PaneKind::Map, PaneKind::Map],
    );
    assert_eq!(restored.pane(0).unwrap().map_memory.zoom(), 7.0);
    assert_eq!(restored.pane(1).unwrap().site(), "KOUN");
}

/// A restored non-map pane arrives with the same invariants as a converted one: no
/// running loop.
#[test]
fn a_restored_non_map_pane_has_no_running_loop() {
    use crate::pane::LoopPhase;

    let store = MemoryKvStore::default();
    store
        .store(
            UI_CONFIG_KEY,
            r#"{"pane_count":1,"site":"KTLX","panes":[
                    {"kind":"Volume","volume":
                        {"yaw_deg":225.0,"pitch_deg":25.0,"eye_distance":2.5}}]}"#,
        )
        .unwrap();

    let mut restored = crate::Gui::new();
    restored
        .pane_mut(0)
        .unwrap()
        .time_state_mut(&known::RADAR)
        .phase = LoopPhase::Playing;
    assert!(
        restored
            .pane(0)
            .unwrap()
            .time_state(&known::RADAR)
            .is_active(),
        "precondition: the loop must be running before the load"
    );

    restored.load_ui_config(&store);

    assert_eq!(
        restored.pane(0).unwrap().render_view(),
        squallar_radar::types::RenderView::Volume
    );
    assert!(
        !restored
            .pane(0)
            .unwrap()
            .time_state(&known::RADAR)
            .is_active(),
        "a restored 3D pane came back with a loop nothing will ever render \
             frames for, which holds every other pane's loop back too"
    );
}

/// A finite camera outside the documented range is clamped, not discarded.
#[test]
fn a_saved_camera_out_of_range_is_clamped_rather_than_dropped() {
    let store = MemoryKvStore::default();
    store
        .store(
            UI_CONFIG_KEY,
            r#"{"pane_count":1,"site":"KTLX","panes":[
                    {"kind":"Volume","volume":
                        {"yaw_deg":-30.0,"pitch_deg":1000.0,"eye_distance":0.001}}]}"#,
        )
        .unwrap();

    let mut restored = crate::Gui::new();
    restored.load_ui_config(&store);

    assert_eq!(
        restored.pane(0).unwrap().render_view(),
        squallar_radar::types::RenderView::Volume
    );
    let camera = restored
        .pane(0)
        .unwrap()
        .volume()
        .expect("a 3D pane")
        .camera;
    assert_eq!(camera.yaw_deg(), 330.0, "yaw wraps rather than clamping");
    assert!(
        camera.pitch_deg().abs() < 90.0,
        "pitch {}",
        camera.pitch_deg()
    );
    assert_eq!(
        camera.eye_distance(),
        0.05,
        "an under-range saved distance must clamp to the zoom's near stop \
             (0.05 framing radii — inside the box is a supported camera), not \
             be discarded",
    );
}

/// A picked region survives a save and a load, both axes and both coordinates.
#[test]
fn a_picked_region_survives_a_save_and_load() {
    let picked = crate::pane::VolumeRegion::new(
        squallar_geo::GeoPoint {
            lat: 35.33,
            lon: -97.28,
        },
        squallar_radar::voxel::HalfExtentKm {
            east_km: 137.0,
            north_km: 84.0,
        },
    )
    .expect("a region on Earth with a finite extent");

    let mut gui = crate::Gui::new();
    gui.set_pane_count(1);
    gui.pane_mut(0)
        .expect("pane 0")
        .set_view(squallar_radar::types::RenderView::Volume);
    gui.pane_mut(0)
        .expect("pane 0")
        .volume_mut()
        .expect("a 3D pane")
        .region = Some(picked);

    let store = MemoryKvStore::default();
    gui.save_ui_config(&store);
    let mut restored = crate::Gui::new();
    restored.load_ui_config(&store);

    assert_eq!(
        restored
            .pane(0)
            .expect("pane 0")
            .volume()
            .expect("a 3D pane")
            .region,
        Some(picked),
        "the region the user picked did not survive the round trip",
    );
}

/// The map a region was picked on survives the round trip beside the region.
#[test]
fn the_map_a_region_was_picked_on_survives_a_save_and_load() {
    let picked = crate::pane::VolumeRegion::new(
        squallar_geo::GeoPoint {
            lat: 35.33,
            lon: -97.28,
        },
        squallar_radar::voxel::HalfExtentKm::square(115.0),
    )
    .expect("a region on Earth with a finite extent");

    let mut gui = crate::Gui::new();
    gui.set_pane_count(2);
    gui.pane_mut(1)
        .expect("pane 1")
        .set_view(squallar_radar::types::RenderView::Volume);
    {
        let volume = gui
            .pane_mut(1)
            .expect("pane 1")
            .volume_mut()
            .expect("a 3D pane");
        volume.region = Some(picked);
        volume.source_pane = Some(0);
    }

    let store = MemoryKvStore::default();
    gui.save_ui_config(&store);
    let mut restored = crate::Gui::new();
    restored.load_ui_config(&store);

    let volume = restored
        .pane(1)
        .expect("pane 1")
        .volume()
        .expect("a 3D pane")
        .clone();
    assert_eq!(
        volume.region,
        Some(picked),
        "the region did not survive beside its source",
    );
    assert_eq!(
        volume.source_pane,
        Some(0),
        "the pane forgot which map aimed it, so the next drag on pane 0 opens a \
         second 3D view instead of adjusting this one",
    );
}

/// A source index the restored layout does not have is **forgotten, and the region
/// is kept**.
#[test]
fn a_dangling_source_pane_is_forgotten_and_the_region_kept() {
    let picked = crate::pane::VolumeRegion::new(
        squallar_geo::GeoPoint {
            lat: 35.33,
            lon: -97.28,
        },
        squallar_radar::voxel::HalfExtentKm::square(115.0),
    )
    .expect("a region on Earth with a finite extent");

    let mut wide = crate::Gui::new();
    wide.set_pane_count(4);
    wide.pane_mut(1)
        .expect("pane 1")
        .set_view(squallar_radar::types::RenderView::Volume);
    {
        let volume = wide
            .pane_mut(1)
            .expect("pane 1")
            .volume_mut()
            .expect("a 3D pane");
        volume.region = Some(picked);
        volume.source_pane = Some(3);
    }

    let store = MemoryKvStore::default();
    wide.save_ui_config(&store);

    let mut narrow = crate::Gui::new();
    narrow.load_ui_config(&store);
    narrow.set_pane_count(2);

    let volume = narrow
        .pane(1)
        .expect("pane 1")
        .volume()
        .expect("a 3D pane")
        .clone();
    assert_eq!(
        volume.region,
        Some(picked),
        "the region was dropped along with its dangling source - the ground the user \
         picked does not stop existing because the map it was drawn on did",
    );
}

/// What the write-side finiteness filter actually prevents — and it is worse than
/// "the config fails to serialize".
#[test]
fn a_non_finite_float_would_poison_the_config_file_permanently() {
    assert_eq!(
        serde_json::to_string(&f32::NAN).expect("serde_json writes it happily"),
        "null",
        "if this ever starts erroring instead, these guards become about a \
             failed save rather than about a file that can never be read again"
    );
    assert!(
        serde_json::from_str::<f32>("null").is_err(),
        "and this is the half that makes it permanent"
    );

    let mut gui = crate::Gui::new();
    let pane = gui.pane_mut(0).unwrap();
    pane.set_view(squallar_radar::types::RenderView::Volume);
    pane.map_mut()
        .expect("a map pane")
        .volume
        .camera
        .nudge(crate::pane::OrbitDelta {
            yaw_deg: 18.0,
            pitch_deg: 4.0,
            zoom_factor: 1.25,
            ..Default::default()
        });
    let json = gui
        .ui_config_json()
        .expect("a 3D pane stopped the config from being written at all");
    let written: serde_json::Value = serde_json::from_str(&json).expect("valid JSON");
    for field in ["yaw_deg", "pitch_deg", "eye_distance"] {
        let value = &written["panes"][0]["volume"][field];
        assert!(
            value.is_f64(),
            "{field} was written as {value}, which will fail every future load"
        );
    }

    let store = MemoryKvStore::default();
    store.store(UI_CONFIG_KEY, &json).unwrap();
    let mut restored = crate::Gui::new();
    assert!(restored.load_ui_config(&store));
    assert_eq!(
        restored.pane(0).unwrap().render_view(),
        squallar_radar::types::RenderView::Volume
    );
}

/// Zoom and pan are what "come back to where I left off" actually means, and
/// neither was persisted before.
#[test]
fn a_panned_and_zoomed_map_comes_back_where_it_was_left() {
    let store = MemoryKvStore::default();

    let baseline = crate::Gui::new();
    let default_zoom = baseline.pane(0).unwrap().map_memory.zoom();
    assert_ne!(
        default_zoom, 9.0,
        "the test zoom must differ from the default"
    );
    assert!(
        baseline.pane(0).unwrap().map_memory.detached().is_none(),
        "a fresh pane follows its site; the test then pans it away"
    );

    let mut gui = crate::Gui::new();
    {
        let pane = gui.pane_mut(0).unwrap();
        pane.map_memory.set_zoom(9.0).unwrap();
        pane.map_memory
            .center_at(walkers::lat_lon(44.9778, -93.2650));
    }
    gui.save_ui_config(&store);

    let mut restored = crate::Gui::new();
    restored.load_ui_config(&store);

    let pane = restored.pane(0).unwrap();
    assert_eq!(pane.map_memory.zoom(), 9.0);
    let center = pane.map_memory.detached().expect("the pan was persisted");
    assert!((center.y() - 44.9778).abs() < 1e-9, "lat {}", center.y());
    assert!((center.x() + 93.2650).abs() < 1e-9, "lon {}", center.x());
}

/// Following the site and being centred on the site's coordinates look the same
/// until the pane changes site, at which point one moves and the other does not.
#[test]
fn a_map_following_its_site_does_not_come_back_pinned() {
    let store = MemoryKvStore::default();

    let mut gui = crate::Gui::new();
    gui.pane_mut(0).unwrap().map_memory.set_zoom(7.0).unwrap();
    assert!(gui.pane(0).unwrap().map_memory.detached().is_none());
    gui.save_ui_config(&store);

    let mut restored = crate::Gui::new();
    restored.load_ui_config(&store);

    assert_eq!(restored.pane(0).unwrap().map_memory.zoom(), 7.0);
    assert!(
        restored.pane(0).unwrap().map_memory.detached().is_none(),
        "an un-panned map was restored as pinned to a fixed centre"
    );
}

/// Configs written before the viewport was persisted must keep the built-in default
/// zoom rather than being read as "saved zoom 0".
#[test]
fn a_config_predating_viewport_persistence_keeps_the_default_zoom() {
    let store = MemoryKvStore::default();
    let default_zoom = crate::Gui::new().pane(0).unwrap().map_memory.zoom();

    store
        .store(
            UI_CONFIG_KEY,
            r#"{"pane_count":1,"site":"KMPX","panes":[{"site":"KMPX"}]}"#,
        )
        .unwrap();

    let mut restored = crate::Gui::new();
    restored.load_ui_config(&store);

    assert_eq!(restored.pane(0).unwrap().site(), "KMPX");
    assert_eq!(
        restored.pane(0).unwrap().map_memory.zoom(),
        default_zoom,
        "an absent zoom was treated as a saved value"
    );
    assert!(restored.pane(0).unwrap().map_memory.detached().is_none());
}

/// A minimal `ScanInfo`. Only its arrival matters here, not its contents; the site
/// the delivery is *addressed to* is the `ScanInfoForSite` event's site field, not
/// this.
fn a_scan() -> squallar_radar::types::ScanInfo {
    squallar_radar::types::ScanInfo {
        site: squallar_radar::sites::RadarSite {
            name: "KTLX",
            network: squallar_radar::sites::RadarNetwork::of_id("KTLX"),
            lat: 35.3,
            lon: -97.3,
            heights: None,
        },
        site_source: squallar_radar::site_position::SitePositionSource::Table,
        site_position: None,
        timestamp: chrono::NaiveDate::from_ymd_opt(2026, 7, 28)
            .unwrap()
            .and_hms_opt(18, 0, 0)
            .unwrap(),
        vcp_number: 212,
        available_products: vec![
            squallar_radar::fields::product_for(&radar_fields::known::REFLECTIVITY)
                .expect("a registered field"),
        ],
        product_elevations: std::collections::HashMap::new(),
        status: "test".to_string(),
    }
}

/// Save at zoom 9, reload, then let the session's first scan arrive.
#[test]
fn a_restored_zoom_survives_the_sessions_first_scan() {
    let store = MemoryKvStore::default();

    let mut gui = crate::Gui::new();
    let site = gui.pane(0).unwrap().site().to_string();
    assert_ne!(
        gui.pane(0).unwrap().map_memory.zoom(),
        9.0,
        "the test zoom must differ from the default"
    );
    gui.pane_mut(0).unwrap().map_memory.set_zoom(9.0).unwrap();
    gui.save_ui_config(&store);

    let mut restored = crate::Gui::new();
    assert!(restored.load_ui_config(&store));
    assert_eq!(
        restored.pane(0).unwrap().map_memory.zoom(),
        9.0,
        "precondition: the load itself must put the zoom back"
    );

    restored.apply(crate::shell_api::GuiEvent::ScanInfoForSite {
        site: site.clone(),
        info: a_scan(),
    });

    assert_eq!(
        restored.pane(0).unwrap().map_memory.zoom(),
        9.0,
        "the session's first scan overwrote the zoom the load had restored"
    );

    let second = MemoryKvStore::default();
    restored.save_ui_config(&second);
    let mut again = crate::Gui::new();
    assert!(again.load_ui_config(&second));
    assert_eq!(
        again.pane(0).unwrap().map_memory.zoom(),
        9.0,
        "the clobbered zoom was written back, so the loss is permanent"
    );
}

/// The chunk-path twin. With live mode fed by the real-time chunk bucket, the first
/// data of a session arrives through `ChunkScanInfo` instead, and it claims the
/// same latch.
#[test]
fn a_restored_zoom_survives_the_sessions_first_chunk_volume() {
    let store = MemoryKvStore::default();

    let mut gui = crate::Gui::new();
    let site = gui.pane(0).unwrap().site().to_string();
    gui.pane_mut(0).unwrap().map_memory.set_zoom(9.0).unwrap();
    gui.save_ui_config(&store);

    let mut restored = crate::Gui::new();
    assert!(restored.load_ui_config(&store));
    assert_eq!(restored.pane(0).unwrap().map_memory.zoom(), 9.0);

    restored.apply(crate::shell_api::GuiEvent::ChunkScanInfo {
        site: site.clone(),
        info: a_scan(),
    });

    assert_eq!(
        restored.pane(0).unwrap().map_memory.zoom(),
        9.0,
        "the session's first chunk volume overwrote the restored zoom"
    );
}

/// Anti-degeneration guard 1: deleting the latch also makes the two tests above
/// pass.
#[test]
fn a_first_run_with_no_config_still_zooms_to_the_radar_on_its_first_scan() {
    let store = MemoryKvStore::default();

    let mut gui = crate::Gui::new();
    assert!(
        !gui.load_ui_config(&store),
        "precondition: an empty store is a first run"
    );
    let site = gui.pane(0).unwrap().site().to_string();
    assert_ne!(
        gui.pane(0).unwrap().map_memory.zoom(),
        crate::ui::DEFAULT_INITIAL_ZOOM,
        "precondition: a fresh pane must not already be at the radar zoom, or \
         this test cannot tell the latch from a default"
    );

    gui.apply(crate::shell_api::GuiEvent::ScanInfoForSite {
        site: site.clone(),
        info: a_scan(),
    });

    assert_eq!(
        gui.pane(0).unwrap().map_memory.zoom(),
        crate::ui::DEFAULT_INITIAL_ZOOM,
        "a first run was left at continental zoom staring at a tiny blob"
    );
}

/// Anti-degeneration guard 2: a config written before viewport persistence
/// (`ee823ca5`) has no `zoom` key at all — every already-installed copy of the app
/// on disk right now.
#[test]
fn a_config_without_a_saved_zoom_still_zooms_to_the_radar_on_its_first_scan() {
    let store = MemoryKvStore::default();
    store
        .store(
            UI_CONFIG_KEY,
            r#"{"pane_count":1,"site":"KMPX","panes":[{"site":"KMPX"}]}"#,
        )
        .unwrap();

    let mut gui = crate::Gui::new();
    assert!(gui.load_ui_config(&store));
    assert_ne!(
        gui.pane(0).unwrap().map_memory.zoom(),
        crate::ui::DEFAULT_INITIAL_ZOOM,
        "precondition: a legacy config restores no zoom"
    );

    gui.apply(crate::shell_api::GuiEvent::ScanInfoForSite {
        site: "KMPX".to_owned(),
        info: a_scan(),
    });

    assert_eq!(
        gui.pane(0).unwrap().map_memory.zoom(),
        crate::ui::DEFAULT_INITIAL_ZOOM,
        "a config predating viewport persistence lost the initial zoom"
    );
}

/// A scan nobody asked for must not spend the one-shot latch, nor move maps.
#[test]
fn a_scan_no_pane_is_watching_neither_moves_a_map_nor_spends_the_latch() {
    let mut gui = crate::Gui::new();
    let site = gui.pane(0).unwrap().site().to_string();
    let before = gui.pane(0).unwrap().map_memory.zoom();
    assert_ne!(site, "KABX", "precondition: the stray site must be a stray");

    gui.apply(crate::shell_api::GuiEvent::ScanInfoForSite {
        site: "KABX".to_owned(),
        info: a_scan(),
    });
    assert_eq!(
        gui.pane(0).unwrap().map_memory.zoom(),
        before,
        "a scan no pane is viewing moved the map anyway"
    );

    gui.apply(crate::shell_api::GuiEvent::ScanInfoForSite {
        site: site.clone(),
        info: a_scan(),
    });
    assert_eq!(
        gui.pane(0).unwrap().map_memory.zoom(),
        crate::ui::DEFAULT_INITIAL_ZOOM,
        "the stray scan spent the latch the real first scan needed"
    );
}

/// A pane layout wider than a phone offers survives the round trip.
#[test]
fn a_pane_layout_wider_than_a_phone_offers_survives_the_round_trip() {
    use crate::ui_layout::WidthClass;
    use squallar_device_profile::budget::{MAX_PANES_DESKTOP, MAX_PANES_MOBILE};

    assert!(
        MAX_PANES_DESKTOP > WidthClass::Compact.max_panes(),
        "precondition: the saved layout must be wider than a compact screen \
             would offer, or the clamp under test is never reached"
    );

    let store = MemoryKvStore::default();
    let mut gui = crate::Gui::new();
    gui.set_pane_count_for_test(MAX_PANES_DESKTOP);
    gui.save_ui_config(&store);

    let mut restored = crate::Gui::new();
    restored.load_ui_config(&store);
    assert_eq!(
        restored.pane_count(),
        MAX_PANES_DESKTOP,
        "the config was clamped to the current device's limit, so the \
             user's layout is gone and the next save writes the truncated one"
    );

    let second = MemoryKvStore::default();
    restored.save_ui_config(&second);
    let mut again = crate::Gui::new();
    again.load_ui_config(&second);
    assert_eq!(again.pane_count(), MAX_PANES_DESKTOP);

    assert_ne!(
        MAX_PANES_DESKTOP, MAX_PANES_MOBILE,
        "precondition: the two limits must differ, or nothing above can \
             tell a correct clamp from the broken one"
    );
}

/// Loading from a store with nothing in it must leave the defaults alone rather
/// than zeroing them — this is every first run.
#[test]
fn an_empty_store_leaves_defaults_untouched() {
    let store = MemoryKvStore::default();
    let mut gui = crate::Gui::new();
    let expected = gui.loop_lookback_secs;

    gui.load_ui_config(&store);

    assert_eq!(gui.loop_lookback_secs, expected);
}

/// A corrupt config must not wipe the user's session or panic.
#[test]
fn unparseable_config_is_ignored() {
    let store = MemoryKvStore::default();
    store.store(UI_CONFIG_KEY, "{ not json").unwrap();

    let mut gui = crate::Gui::new();
    let expected = gui.loop_lookback_secs;
    gui.load_ui_config(&store);

    assert_eq!(gui.loop_lookback_secs, expected);
}

/// A fold limit is a fact about a sweep, so it is never written to the config.
#[test]
fn a_reopened_pane_carries_no_fold_limit_until_its_first_render() {
    let store = MemoryKvStore::default();
    let mut gui = crate::Gui::new();
    gui.pane_mut(0)
        .unwrap()
        .set_selected_product(radar_fields::known::VELOCITY);
    gui.save_ui_config(&store);

    let written = store.load(UI_CONFIG_KEY).expect("config should be stored");
    for word in ["nyquist", "fold", "folds"] {
        assert!(
            !written.contains(word),
            "the config schema grew a {word:?} field: {written}",
        );
    }

    let mut restored = crate::Gui::new();
    restored.load_ui_config(&store);
    let pane = restored.pane(0).expect("a restored pane");
    assert_eq!(
        pane.selected_product(),
        radar_fields::known::VELOCITY,
        "precondition: the pane must come back on velocity, or it would \
         answer None for the wrong reason",
    );
    assert_eq!(
        pane.displayed_nyquist_ms(),
        None,
        "a pane with no picture on it claimed to know where that picture folds",
    );
}

/// A pane's field comes back as the registry's own `&'static` spelling, not as
/// the bytes that were on disk.
///
/// **Pointer identity, because that is the property and equality is not.** The
/// two `FieldId`s compare equal either way; what
/// [`crate::ui_config::product_or_default`] promises is that the surviving one
/// borrows the registry's static string, so `PaneState::selected_product`
/// returns a `Cow::Borrowed` and reading a pane's field on the frame path
/// allocates nothing. A load that handed back an owned copy of the same bytes
/// would pass an `assert_eq!` and fail this.
#[test]
fn the_loaded_field_is_the_registrys_own_static_spelling() {
    let store = MemoryKvStore::default();
    let mut gui = crate::Gui::new();
    // Written through a *constructed* id rather than the const, so the bytes
    // that reach the disk cannot already be the static this test is looking for.
    gui.pane_mut(0)
        .unwrap()
        .set_selected_product(squallar_source::product::FieldId::new(String::from(
            "Velocity",
        )));
    gui.save_ui_config(&store);

    let mut restored = crate::Gui::new();
    assert!(restored.load_ui_config(&store));
    let loaded = restored
        .pane(0)
        .expect("a restored pane")
        .selected_product();
    assert_eq!(
        loaded,
        radar_fields::known::VELOCITY,
        "precondition: the pane must come back on velocity",
    );
    let registered = squallar_radar::fields::spec_for(&radar_fields::known::VELOCITY)
        .expect("the radar layer registers Velocity");
    assert!(
        std::ptr::eq(loaded.as_str(), registered.id.as_str()),
        "the pane's field is a copy of the bytes off the disk, not the \
         registry's own static spelling: reading it on the frame path now \
         allocates once per read",
    );
}

/// An unknown field on disk falls back to the default field, and the fallback is
/// itself the registry's static spelling.
#[test]
fn a_pane_whose_saved_field_this_build_does_not_register_falls_back() {
    let store = MemoryKvStore::default();
    let mut gui = crate::Gui::new();
    gui.pane_mut(0)
        .unwrap()
        .set_selected_product(squallar_source::product::FieldId::new(String::from(
            "NoSuchFieldAnywhere",
        )));
    gui.save_ui_config(&store);

    let mut restored = crate::Gui::new();
    assert!(restored.load_ui_config(&store));
    let loaded = restored
        .pane(0)
        .expect("a restored pane")
        .selected_product();
    assert_eq!(
        loaded,
        radar_fields::known::REFLECTIVITY,
        "a pane's selection has to name a field this build can draw; unlike a \
         saved curve there is nothing for it to be preserved inert *on*",
    );
}

/// Saving writes under the shared key, which is what the filesystem backend maps
/// onto `ui.json`.
#[test]
fn save_writes_under_the_ui_key() {
    let store = MemoryKvStore::default();
    assert!(store.load(UI_CONFIG_KEY).is_none());

    crate::Gui::new().save_ui_config(&store);

    let written = store.load(UI_CONFIG_KEY).expect("config should be stored");
    assert!(
        serde_json::from_str::<super::UiConfig>(&written).is_ok(),
        "stored blob should parse back as a UiConfig"
    );
}

/// **The alias mechanism, exercised against a table that is not empty** —
/// because the production table [`super::FIELD_ALIASES`] is, and an
/// identity function proves nothing about the machinery a rename will need.
#[test]
fn a_saved_field_key_is_read_through_the_alias_table() {
    use squallar_source::product::FieldId;

    let table: &[(&str, &str)] = &[("OldSpelling", "Reflectivity"), ("Stale", "Velocity")];
    assert_eq!(
        super::resolve_field_alias(table, FieldId::new("OldSpelling")),
        FieldId::new("Reflectivity"),
        "a renamed field's saved key must read as its current id",
    );
    assert_eq!(
        super::resolve_field_alias(table, FieldId::new("Velocity")),
        FieldId::new("Velocity"),
        "an id with no row is itself",
    );
    assert_eq!(
        super::resolve_field_alias(table, FieldId::new("NeverHeardOfIt")),
        FieldId::new("NeverHeardOfIt"),
        "an id this build does not register is passed through, not dropped — \
         the open-id doctrine survives the alias hop",
    );
    // Single hop: a row whose target is itself another row's source must not
    // chain, or two rows could conspire into a rename nobody wrote down.
    let chain: &[(&str, &str)] = &[("A", "B"), ("B", "C")];
    assert_eq!(
        super::resolve_field_alias(chain, FieldId::new("A")),
        FieldId::new("B"),
        "resolution is single-hop",
    );
}

/// The shipped alias table is empty, and every row it ever gains must name a
/// field this build actually registers on the right-hand side — an alias
/// pointing at nothing would orphan the saved state it was added to rescue.
#[test]
fn the_shipped_alias_table_is_empty_and_every_row_lands_on_a_registered_field() {
    use squallar_source::product::FieldId;

    assert!(
        super::FIELD_ALIASES.is_empty(),
        "no radar field has been renamed; a first row here is a deliberate act \
         and this assertion is where it gets noticed: {:?}",
        super::FIELD_ALIASES,
    );
    for (old, current) in super::FIELD_ALIASES {
        assert!(
            squallar_radar::fields::product_for(&FieldId::new(*current)).is_some(),
            "the alias {old} -> {current} points at a field this build does \
             not register, so it orphans exactly the saved state it exists to \
             rescue",
        );
        assert_ne!(old, current, "an alias to itself is a no-op row");
    }
}
