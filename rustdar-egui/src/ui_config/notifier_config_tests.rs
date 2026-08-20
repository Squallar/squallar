use crate::Gui;
use rustdar_kv::{KvStore, MemoryKvStore};

/// A store holding exactly one file, as the disk would.
fn store_with(file: &str) -> MemoryKvStore {
    let store = MemoryKvStore::default();
    store
        .store(crate::UI_CONFIG_KEY, file)
        .expect("the memory store accepts a write");
    store
}

/// Write one of the notifier settings the one way the app writes them now — a
/// layer id and a control update, through the generic door. There is no `Gui`
/// setter for either any more, and that is the point of WO-E8b.
fn apply(gui: &mut Gui, update: rustdar_source::controls::ControlUpdate) {
    gui.apply_layer_control(&crate::radar_layer::POLL_LAYER, &update);
}

/// **WO-E8b moved where these are written**: the two values used to be root
/// keys on `UiConfig` and are now members of the radar layer's own state blob,
/// so the assertions read that blob instead of those fields. The property —
/// both settings survive the save — is unchanged.
#[test]
fn the_notifier_settings_round_trip() {
    let mut gui = Gui::new();
    apply(
        &mut gui,
        crate::radar_layer::chunk_notifications_update(false),
    );
    apply(
        &mut gui,
        crate::radar_layer::notifier_endpoint_update("wss://example.test"),
    );
    let json = gui.ui_config_json().expect("serialises");
    let parsed: serde_json::Value = serde_json::from_str(&json).expect("parses");
    let radar = &parsed["overlay_states"]["Radar"];
    assert_eq!(radar["chunk_notifications"], serde_json::json!(false));
    assert_eq!(
        radar["notifier_endpoint"],
        serde_json::json!("wss://example.test")
    );
}

/// A config written before these fields existed keeps the low-latency
/// defaults rather than failing to load or silently opting out.
///
/// **WO-E8b moved the mechanism too**: the tolerance was `#[serde(default)]`
/// on the container and is now the handler's own `deserialize_state`, which is
/// only reachable through the whole load.
#[test]
fn an_older_config_defaults_to_notifications_on() {
    let old = r#"{"pane_count":1,"auto_poll":true,"site":"KTLX"}"#;
    let mut gui = Gui::new();
    assert!(
        gui.load_ui_config(&store_with(old)),
        "an older config still loads"
    );
    assert!(crate::radar_layer::chunk_notifications_enabled(&gui));
    assert!(crate::radar_layer::live_chunks_enabled(&gui));
}

/// A cleared endpoint box falls back to the built-in default rather than
/// acting as a silent off switch.
#[test]
fn an_empty_endpoint_falls_back_to_the_default() {
    let mut gui = Gui::new();
    apply(
        &mut gui,
        crate::radar_layer::notifier_endpoint_update("   "),
    );
    assert_eq!(
        crate::radar_layer::notifier_endpoint(&gui),
        rustdar_radar::source::DEFAULT_NOTIFIER_ENDPOINT
    );
    assert_eq!(
        crate::radar_layer::notifier_endpoint_raw(&gui),
        "   ",
        "the box stopped showing what the user typed",
    );
    apply(
        &mut gui,
        crate::radar_layer::notifier_endpoint_update("wss://example.test/"),
    );
    assert_eq!(
        crate::radar_layer::notifier_endpoint(&gui),
        "wss://example.test/"
    );
}

/// A config written before the camera grew fields loads at the defaults
/// rather than at zeros.
#[test]
fn a_config_from_before_the_new_camera_fields_loads_at_the_defaults() {
    use crate::pane::{MapRender, OrbitCamera};

    let store = rustdar_kv::MemoryKvStore::default();
    let mut gui = crate::Gui::new();
    let pane = gui.pane_mut(0).expect("pane 0");
    assert!(pane.set_map_render(MapRender::Volume));
    pane.map_mut()
        .expect("a map pane")
        .volume
        .camera
        .nudge(crate::pane::OrbitDelta {
            yaw_deg: 12.0,
            ..Default::default()
        });
    gui.save_ui_config(&store);

    // Strip the two fields that did not exist, and put back the one that no
    // longer does — an older writer's file exactly.
    let json = store.load(crate::UI_CONFIG_KEY).expect("just saved");
    let mut value: serde_json::Value = serde_json::from_str(&json).expect("valid json");
    let volume = value["panes"][0]["volume"]
        .as_object_mut()
        .expect("a moved camera writes a volume block");
    volume.remove("pivot");
    volume.remove("vertical_exaggeration");
    volume.insert(
        "region".to_owned(),
        serde_json::json!({ "centre_lat": 35.3, "centre_lon": -97.3, "half_width_km": 40.0 }),
    );
    let older = serde_json::to_string(&value).expect("serializable");
    let older_store = rustdar_kv::MemoryKvStore::default();
    older_store
        .store(crate::UI_CONFIG_KEY, &older)
        .expect("storable");

    let mut restored = crate::Gui::new();
    restored.load_ui_config(&older_store);
    let camera = restored
        .pane(0)
        .expect("pane 0")
        .volume()
        .expect("the pane must come back in the 3D mode")
        .camera;
    assert_eq!(
        camera.pivot(),
        [0.0; 3],
        "a missing pivot must load centred"
    );
    assert_eq!(
        camera.vertical_exaggeration(),
        OrbitCamera::default().vertical_exaggeration(),
        "a missing exaggeration must load at the default, never at zero",
    );
}
