use super::*;
use crate::Gui;
use crate::config_store::MemoryConfigStore;
use crate::ui::catalog::PresetPane;

/// A user preset that exercises every field.
fn preset() -> super::super::PresetConfig {
    super::super::PresetConfig {
        name: "Chase day".into(),
        pane_count: 2,
        panes: vec![
            PresetPane {
                product: RadarProduct::Velocity,
                elevation: 0.5,
            },
            PresetPane {
                product: RadarProduct::Reflectivity,
                elevation: 1.5,
            },
        ],
        overlays: vec![OverlayKind::Radar, OverlayKind::StormReports],
    }
}

/// A saved preset comes back whole, and a config from before the field has
/// none — the built-ins are compiled in, not persisted, so "none" is still a
/// populated catalog.
#[test]
fn user_presets_round_trip_and_an_older_config_has_none() {
    let store = MemoryConfigStore::default();
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

/// A preset naming a product this build does not know falls back to the
/// default product rather than failing the whole file — the same forward
/// tolerance the pane configs carry, through the same deserializer.
#[test]
fn an_unknown_preset_product_costs_the_product_not_the_file() {
    let store = MemoryConfigStore::default();
    let mut gui = Gui::new();
    gui.presets.push(preset());
    gui.save_ui_config(&store);

    let saved = store
        .load(crate::config_store::UI_CONFIG_KEY)
        .expect("just saved");
    let mut value: serde_json::Value = serde_json::from_str(&saved).expect("valid json");
    value["presets"][0]["panes"][0]["product"] = serde_json::json!("FutureProduct");
    let newer_store = MemoryConfigStore::default();
    newer_store
        .store(
            crate::config_store::UI_CONFIG_KEY,
            &serde_json::to_string(&value).expect("serializable"),
        )
        .expect("storable");

    let mut restored = Gui::new();
    assert!(
        restored.load_ui_config(&newer_store),
        "one unknown product name must not fail the whole config"
    );
    assert_eq!(
        restored.presets[0].panes[0].product,
        RadarProduct::Reflectivity,
        "the unknown product falls back to the default"
    );
    assert_eq!(
        restored.presets[0].panes[1].product,
        RadarProduct::Reflectivity,
        "the rest of the preset survives"
    );
}
