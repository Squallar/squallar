use super::*;
use crate::Gui;

#[test]
fn the_notifier_settings_round_trip() {
    let mut gui = Gui::new();
    gui.set_chunk_notifications(false);
    gui.set_notifier_endpoint("wss://example.test");
    let json = gui.ui_config_json().expect("serialises");
    let parsed: UiConfig = serde_json::from_str(&json).expect("parses");
    assert!(!parsed.chunk_notifications);
    assert_eq!(parsed.notifier_endpoint, "wss://example.test");
}

/// A config written before these fields existed keeps the low-latency
/// defaults rather than failing to parse or silently opting out.
#[test]
fn an_older_config_defaults_to_notifications_on() {
    let old = r#"{"pane_count":1,"auto_poll":true,"site":"KTLX"}"#;
    let parsed: UiConfig = serde_json::from_str(old).expect("an older config still parses");
    assert!(parsed.chunk_notifications);
    assert!(parsed.live_chunks);
}

/// A cleared endpoint box falls back to the built-in default rather than
/// acting as a silent off switch.
#[test]
fn an_empty_endpoint_falls_back_to_the_default() {
    let mut gui = Gui::new();
    gui.set_notifier_endpoint("   ");
    assert_eq!(gui.notifier_endpoint(), crate::DEFAULT_NOTIFIER_ENDPOINT);
    gui.set_notifier_endpoint("wss://example.test/");
    assert_eq!(gui.notifier_endpoint(), "wss://example.test/");
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
