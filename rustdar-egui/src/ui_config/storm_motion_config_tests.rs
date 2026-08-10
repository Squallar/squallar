use super::*;
use crate::Gui;
use crate::config_store::{ConfigStore, MemoryConfigStore};

/// The storm-motion override survives a save/load cycle — the audit's known
/// persistence gap, closed in M4: the state existed and the settings pane
/// edited it, but every restart silently reset it.
#[test]
fn the_storm_motion_override_round_trips() {
    let store = MemoryConfigStore::default();
    let mut gui = Gui::new();
    gui.storm_motion_override.enabled = true;
    gui.storm_motion_override.speed_kt = 42.0;
    gui.storm_motion_override.direction_deg = 215.0;
    gui.save_ui_config(&store);

    let mut restored = Gui::new();
    restored.load_ui_config(&store);
    assert_eq!(
        restored.storm_motion_override, gui.storm_motion_override,
        "the override must come back exactly as it was set"
    );
}

/// A config from before the field loads with the override off and the
/// default vector — which is how those sessions actually ran.
#[test]
fn an_older_config_defaults_the_override_off() {
    let old = r#"{"pane_count":1,"auto_poll":true,"site":"KTLX"}"#;
    let parsed: UiConfig = serde_json::from_str(old).expect("an older config still parses");
    assert_eq!(
        parsed.storm_motion_override,
        crate::StormMotionOverride::default()
    );
    assert!(!parsed.storm_motion_override.enabled);
}

/// A non-finite number never reaches the file. `DragValue` parses `"nan"`,
/// and `serde_json` writes a non-finite float as `null` — which fails the
/// *next* load and costs the user the whole config, permanently, because the
/// autosave then rewrites it from defaults.
#[test]
fn a_non_finite_override_is_written_as_the_default() {
    let mut gui = Gui::new();
    gui.storm_motion_override.enabled = true;
    gui.storm_motion_override.speed_kt = f32::NAN;
    let json = gui.ui_config_json().expect("serialises");
    let parsed: UiConfig = serde_json::from_str(&json).expect("the file must stay loadable");
    assert!(
        parsed.storm_motion_override.enabled,
        "the guard is per float: the toggle must not be laundered with it"
    );
    assert_eq!(
        parsed.storm_motion_override.speed_kt,
        crate::StormMotionOverride::default().speed_kt,
        "the NaN must be replaced by the default, not written as null"
    );
}

/// A pane's time-link survives the round trip, and a config from before the
/// field loads every pane linked — which is how every pane behaved.
#[test]
fn the_time_link_round_trips_and_defaults_on() {
    let store = MemoryConfigStore::default();
    let mut gui = Gui::new();
    gui.set_pane_count_for_test(2);
    gui.pane_mut(1).expect("pane 1").time_link = false;
    gui.save_ui_config(&store);

    let mut restored = Gui::new();
    restored.load_ui_config(&store);
    assert!(restored.pane(0).expect("pane 0").time_link);
    assert!(
        !restored.pane(1).expect("pane 1").time_link,
        "the unlinked pane must come back unlinked"
    );

    // Strip the field, as an older writer would have.
    let json = store
        .load(crate::config_store::UI_CONFIG_KEY)
        .expect("just saved");
    let mut value: serde_json::Value = serde_json::from_str(&json).expect("valid json");
    for pane in value["panes"].as_array_mut().expect("a pane list") {
        pane.as_object_mut()
            .expect("a pane object")
            .remove("time_link");
    }
    let older_store = MemoryConfigStore::default();
    older_store
        .store(
            crate::config_store::UI_CONFIG_KEY,
            &serde_json::to_string(&value).expect("serializable"),
        )
        .expect("storable");
    let mut restored = Gui::new();
    restored.load_ui_config(&older_store);
    assert!(
        restored.pane(1).expect("pane 1").time_link,
        "a config from before the field must load linked — all-linked is \
         exactly the old behaviour"
    );
}
