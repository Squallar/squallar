use super::*;
use crate::Gui;
use crate::ui::catalog::PresetPane;
use rustdar_kv::MemoryKvStore;

/// A user preset that exercises every field.
fn preset() -> super::super::PresetConfig {
    super::super::PresetConfig {
        name: "Chase day".into(),
        pane_count: 2,
        panes: vec![
            PresetPane {
                product: rustdar_source::product::FieldId::from_static("Velocity"),
                elevation: 0.5,
            },
            PresetPane {
                product: rustdar_source::product::FieldId::from_static("Reflectivity"),
                elevation: 1.5,
            },
        ],
        overlays: vec![
            rustdar_source::id::known::RADAR,
            rustdar_source::id::known::STORM_REPORTS,
        ]
        .into(),
    }
}

/// A saved preset comes back whole; the built-ins are compiled in, not persisted.
#[test]
fn user_presets_round_trip_and_an_older_config_has_none() {
    let store = MemoryKvStore::default();
    let mut gui = Gui::new();
    gui.presets.push(preset());
    gui.save_ui_config(&store);

    let mut restored = Gui::new();
    restored.load_ui_config(&store);
    assert_eq!(
        restored.presets,
        vec![preset()],
        "the preset must come back exactly as saved"
    );

    let old = r#"{"pane_count":1,"auto_poll":true,"site":"KTLX"}"#;
    let parsed: UiConfig = serde_json::from_str(old).expect("an older config still parses");
    assert!(parsed.presets.is_empty());
}

/// A preset naming a field this build does not register **keeps that name**,
/// and costs neither the pane nor the file.
///
/// **This replaces a substitution with a preservation, deliberately** (WO-E9d).
/// The guarantee the old pin bought — one unknown name must not fail the whole
/// config — is unchanged and asserted below. What changed is what happens to the
/// name itself: it used to be rewritten to Reflectivity on load, which silently
/// destroyed a preset authored on a newer build the first time an older build
/// saved. Under the open-id doctrine the entry is carried through untouched and
/// written back verbatim, so the round trip is lossless in both directions.
#[test]
fn an_unknown_preset_field_is_preserved_and_costs_neither_pane_nor_file() {
    let store = MemoryKvStore::default();
    let mut gui = Gui::new();
    gui.presets.push(preset());
    gui.save_ui_config(&store);

    let saved = store.load(crate::UI_CONFIG_KEY).expect("just saved");
    let mut value: serde_json::Value = serde_json::from_str(&saved).expect("valid json");
    value["presets"][0]["panes"][0]["product"] = serde_json::json!("FutureProduct");
    let newer_store = MemoryKvStore::default();
    newer_store
        .store(
            crate::UI_CONFIG_KEY,
            &serde_json::to_string(&value).expect("serializable"),
        )
        .expect("storable");

    let mut restored = Gui::new();
    assert!(
        restored.load_ui_config(&newer_store),
        "one unregistered field name must not fail the whole config"
    );
    assert_eq!(
        restored.presets[0].panes[0].product.as_str(),
        "FutureProduct",
        "an id this build does not register is preserved, never substituted — \
         substituting it would destroy a newer build's preset on the first save",
    );
    assert_eq!(
        restored.presets[0].panes[1].product.as_str(),
        "Reflectivity",
        "the rest of the preset survives",
    );
    assert!(
        (restored.presets[0].panes[0].elevation - 0.5).abs() < f32::EPSILON,
        "the unregistered pane keeps its other members too",
    );

    // The other direction: saving from this build writes the unknown id back
    // byte-for-byte rather than dropping or replacing it.
    let round = MemoryKvStore::default();
    restored.save_ui_config(&round);
    let written: serde_json::Value =
        serde_json::from_str(&round.load(crate::UI_CONFIG_KEY).expect("just saved"))
            .expect("valid json");
    assert_eq!(
        written["presets"][0]["panes"][0]["product"],
        serde_json::json!("FutureProduct"),
        "the unregistered id must be written back verbatim, which is what makes \
         the downgrade lossless",
    );
}

/// Every field the compiled-in presets name is a field this build registers.
///
/// The presets hold **`FieldId` literals** — open strings, which no compiler
/// checks — so this is the check. A typo, or a registration renamed without its
/// preset, would otherwise show up as a preset tile that silently does nothing
/// to the pane it claims to configure.
#[test]
fn the_builtin_presets_name_registered_fields() {
    let gui = Gui::new();
    let presets = crate::ui::catalog::builtin_presets();
    assert!(!presets.is_empty(), "there are presets to check");
    let mut checked = 0;
    for preset in &presets {
        assert!(
            !preset.panes.is_empty(),
            "preset {:?} configures no panes, so it checks nothing",
            preset.name,
        );
        for pane in &preset.panes {
            assert!(
                gui.overlays.field(&pane.product).is_some(),
                "built-in preset {:?} names the field {:?}, which no registered \
                 source offers — its tile would build a pane that ignores it",
                preset.name,
                pane.product.as_str(),
            );
            checked += 1;
        }
    }
    // Non-triviality floor: the loop above actually ran over every pane.
    assert_eq!(
        checked,
        presets.iter().map(|p| p.panes.len()).sum::<usize>(),
        "the walk skipped panes",
    );
    assert!(
        checked >= 9,
        "expected at least the nine shipped panes, saw {checked}"
    );
}
