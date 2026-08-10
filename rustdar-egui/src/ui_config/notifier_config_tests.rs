use super::*;
use crate::Gui;

/// Both notification settings survive a save/load cycle.
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
/// acting as a silent off switch — turning the feature off is what the
/// toggle is for.
#[test]
fn an_empty_endpoint_falls_back_to_the_default() {
    let mut gui = Gui::new();
    gui.set_notifier_endpoint("   ");
    assert_eq!(gui.notifier_endpoint(), crate::DEFAULT_NOTIFIER_ENDPOINT);
    gui.set_notifier_endpoint("wss://example.test/");
    assert_eq!(gui.notifier_endpoint(), "wss://example.test/");
}

/// A 3D pane's **region** and where it was dragged from survive the round
/// trip, and a corrupt one costs the region rather than the pane.
///
/// The region is the one piece of 3D state a user produced by hand, and a
/// carefully aimed 20 km box silently coming back as the 460 km default on
/// restart is a feature that looks broken rather than absent.
///
/// The asymmetry in the second half is deliberate and is the thing worth
/// pinning: a bad *camera* costs the pane its kind, because a 3D pane with no
/// view is nothing; a bad *region* costs only the region, because a 3D pane
/// with no region has a perfectly good default box about its site.
#[test]
fn a_3d_panes_region_survives_the_round_trip_and_a_corrupt_one_costs_only_itself() {
    use crate::pane::{GeoPoint, PaneKind, VolumeRegion};

    let store = crate::config_store::MemoryConfigStore::default();
    let mut gui = crate::Gui::new();
    gui.set_pane_count_for_test(2);
    gui.pane_mut(1).unwrap().set_kind(PaneKind::Volume);
    let region = VolumeRegion::new(
        GeoPoint {
            lat: 35.3,
            lon: -97.3,
        },
        23.5,
    )
    .expect("a valid region");
    {
        let volume = gui.pane_mut(1).unwrap().volume_mut().expect("converted");
        volume.region = Some(region);
        volume.source_pane = Some(0);
    }
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
        Some(region),
        "the picked ground must come back"
    );
    assert_eq!(
        volume.source_pane,
        Some(0),
        "and which map it was dragged on, or the next drag there would open \
             another pane instead of re-aiming this one",
    );

    // A region naming ground that is not on Earth. Through the file, because
    // the in-memory type cannot hold one — which is the whole reason the wire
    // form is flat numbers and `VolumeRegion::new` is the gate.
    let saved = store
        .load(crate::config_store::UI_CONFIG_KEY)
        .expect("just saved");
    // Edited as a tree rather than as a string: the writer pretty-prints, so
    // a `str::replace` on `"centre_lat":35.3` matches nothing and the test
    // passes by asserting about an unmodified file.
    let mut value: serde_json::Value = serde_json::from_str(&saved).expect("valid json");
    value["panes"][1]["volume"]["region"]["centre_lat"] = serde_json::json!(1000.0);
    let corrupt = serde_json::to_string(&value).expect("serializable");
    assert_ne!(corrupt, saved, "the corruption must have landed");
    let corrupt_store = crate::config_store::MemoryConfigStore::default();
    corrupt_store
        .store(crate::config_store::UI_CONFIG_KEY, &corrupt)
        .expect("storable");
    let mut restored = crate::Gui::new();
    restored.load_ui_config(&corrupt_store);
    assert_eq!(
        restored.pane(1).expect("pane 1").kind(),
        PaneKind::Volume,
        "an unusable region must not cost the pane its kind",
    );
    assert_eq!(
        restored
            .pane(1)
            .expect("pane 1")
            .volume()
            .expect("a 3D pane")
            .region,
        None,
        "an unusable region must be dropped for the default box about the site",
    );
}

/// A config written before the pan and the exaggeration existed comes back
/// centred and at the default stretch.
///
/// `#[serde(default)]` on the struct is what does it, and the failure it
/// avoids is not subtle: without it the missing `vertical_exaggeration`
/// deserializes as `0.0`, which collapses the box to a plane and divides by
/// zero in `box_from_world`. Every user with a saved 3D pane would see an
/// empty pane on the first run after the upgrade.
#[test]
fn a_config_from_before_the_new_camera_fields_loads_at_the_defaults() {
    use crate::pane::{OrbitCamera, PaneKind};

    let store = crate::config_store::MemoryConfigStore::default();
    let mut gui = crate::Gui::new();
    gui.pane_mut(0).unwrap().set_kind(PaneKind::Volume);
    gui.save_ui_config(&store);

    // Strip the three fields that did not exist, as an older writer would.
    let json = store
        .load(crate::config_store::UI_CONFIG_KEY)
        .expect("just saved");
    let mut value: serde_json::Value = serde_json::from_str(&json).expect("valid json");
    let volume = value["panes"][0]["volume"]
        .as_object_mut()
        .expect("a 3D pane's config");
    volume.remove("pivot");
    volume.remove("vertical_exaggeration");
    volume.remove("region");
    let older = serde_json::to_string(&value).expect("serializable");
    let older_store = crate::config_store::MemoryConfigStore::default();
    older_store
        .store(crate::config_store::UI_CONFIG_KEY, &older)
        .expect("storable");

    let mut restored = crate::Gui::new();
    restored.load_ui_config(&older_store);
    let camera = restored
        .pane(0)
        .expect("pane 0")
        .volume()
        .expect("a 3D pane")
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
