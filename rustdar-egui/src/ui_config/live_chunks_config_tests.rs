use super::*;
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

/// Write the live-chunk switch the one way the app writes it now — a layer id
/// and a control update, through the generic door. There is no `Gui` setter
/// for it any more, and that is the point of WO-E8b.
fn set_live_chunks(gui: &mut Gui, on: bool) {
    gui.apply_layer_control(
        &crate::radar_layer::POLL_LAYER,
        &crate::radar_layer::live_chunks_update(on),
    );
}

/// The setting survives a save/load cycle in both positions.
///
/// **WO-E8b moved where it is written**: the value used to be the root key
/// `live_chunks` on `UiConfig` and is now a member of the radar layer's own
/// state blob, so the assertion reads that blob instead of that field. The
/// property is unchanged and so is the coverage — both positions, one cycle.
#[test]
fn the_live_chunks_setting_round_trips() {
    for enabled in [true, false] {
        let mut gui = Gui::new();
        set_live_chunks(&mut gui, enabled);
        let json = gui.ui_config_json().expect("serialises");
        let parsed: serde_json::Value = serde_json::from_str(&json).expect("parses");
        assert_eq!(
            parsed["overlay_states"]["Radar"]["live_chunks"],
            serde_json::json!(enabled),
            "the switch did not reach the radar layer's saved state",
        );
    }
}

/// A config written before the field existed takes the default rather than
/// failing to parse.
///
/// **WO-E8b moved the mechanism as well as the key**: the tolerance used to be
/// `#[serde(default)]` on the container and is now the handler's own
/// `deserialize_state`, which takes a member only if the blob names it. So the
/// assertion drives the whole load rather than a bare `serde` parse — that is
/// the only place the new mechanism is reachable from.
#[test]
fn a_config_written_before_this_field_defaults_to_chunks() {
    let old = r#"{"pane_count":1,"active_pane":0,"auto_poll":true,"site":"KTLX"}"#;
    let mut gui = Gui::new();
    assert!(
        gui.load_ui_config(&store_with(old)),
        "an older config still loads"
    );
    assert!(
        crate::radar_layer::live_chunks_enabled(&gui),
        "an existing install would silently lose the low-latency feed"
    );
}

/// **The v3 → v4 key move, at `Value` level, in both directions.**
///
/// Up: a v3 file's four root keys arrive inside `overlay_states["Radar"]`,
/// each value carried verbatim, and the root no longer names them. Down: the
/// file the migrated `Gui` writes is loaded by a second session, which reads
/// the same four answers back — the round trip the moved keys have to survive
/// for a reopen to be 1:1.
///
/// The four values are deliberately **all off the defaults** (`auto_poll`
/// false, `live_chunks` false, `chunk_notifications` false, a custom
/// endpoint), so an assertion here cannot pass on a build that dropped the
/// keys entirely and answered from its own defaults.
#[test]
fn the_v3_migration_moves_all_four_root_keys_into_the_radar_layer() {
    let v3 = r#"{"config_version":3,"pane_count":1,"active_pane":0,
                 "auto_poll":false,"live_chunks":false,"chunk_notifications":false,
                 "notifier_endpoint":"wss://example.test/notify","site":"KTLX",
                 "loop_lookback_secs":3600,"loop_speed_fps":5.0,"time_step_secs":600,
                 "viewport_sync":true,"panes":[]}"#;

    // Direction 1 — the step itself, where it happens.
    let mut tree: serde_json::Value = serde_json::from_str(v3).expect("valid JSON");
    migrate::migrate_to_current(&mut tree);
    for key in [
        "auto_poll",
        "live_chunks",
        "chunk_notifications",
        "notifier_endpoint",
    ] {
        assert!(
            tree.get(key).is_none(),
            "{key} was copied rather than moved — two homes is the drift the \
             move exists to end",
        );
    }
    let radar = &tree["overlay_states"]["Radar"];
    assert_eq!(radar["auto_poll"], serde_json::json!(false));
    assert_eq!(radar["live_chunks"], serde_json::json!(false));
    assert_eq!(radar["chunk_notifications"], serde_json::json!(false));
    assert_eq!(
        radar["notifier_endpoint"],
        serde_json::json!("wss://example.test/notify"),
        "the endpoint's value did not survive the move verbatim",
    );

    // Direction 2 — the whole load, and then the save it writes.
    let mut gui = Gui::new();
    assert!(
        crate::radar_layer::live_chunks_enabled(&gui)
            && crate::radar_layer::chunk_notifications_enabled(&gui)
            && crate::radar_layer::auto_poll_enabled(&gui.overlays),
        "precondition: a fresh Gui starts with all three switches ON, or the \
         assertions below could pass without the file being read at all",
    );
    assert!(gui.load_ui_config(&store_with(v3)), "the v3 file must load");
    assert!(!crate::radar_layer::live_chunks_enabled(&gui));
    assert!(!crate::radar_layer::chunk_notifications_enabled(&gui));
    assert!(!crate::radar_layer::auto_poll_enabled(&gui.overlays));
    assert_eq!(
        crate::radar_layer::notifier_endpoint(&gui),
        "wss://example.test/notify",
    );

    let save = gui.ui_config_json().expect("a loaded Gui serializes");
    let mut second = Gui::new();
    assert!(
        second.load_ui_config(&store_with(&save)),
        "the save must reload"
    );
    assert!(!crate::radar_layer::live_chunks_enabled(&second));
    assert!(!crate::radar_layer::chunk_notifications_enabled(&second));
    assert!(!crate::radar_layer::auto_poll_enabled(&second.overlays));
    assert_eq!(
        crate::radar_layer::notifier_endpoint(&second),
        "wss://example.test/notify",
        "the second session lost the endpoint",
    );
}

/// **A v4 file already carrying the blob is not overwritten by a stale root.**
///
/// A file holding both is one a newer build wrote and an older build handed
/// back, with the root keys the older build's copy. The blob is the newer half
/// and wins; the root keys are still consumed, so no second home survives.
#[test]
fn a_blob_that_already_answers_beats_the_root_keys_it_replaced() {
    let both = r#"{"config_version":3,"pane_count":1,"active_pane":0,
                   "live_chunks":true,"notifier_endpoint":"wss://stale.test/",
                   "overlay_states":{"Radar":{"live_chunks":false,
                                              "notifier_endpoint":"wss://fresh.test/"}},
                   "site":"KTLX","panes":[]}"#;
    let mut tree: serde_json::Value = serde_json::from_str(both).expect("valid JSON");
    migrate::migrate_to_current(&mut tree);
    assert_eq!(
        tree["overlay_states"]["Radar"]["live_chunks"],
        serde_json::json!(false),
        "the root's stale copy overwrote the blob's fresher answer",
    );
    assert_eq!(
        tree["overlay_states"]["Radar"]["notifier_endpoint"],
        serde_json::json!("wss://fresh.test/"),
    );
    assert!(tree.get("live_chunks").is_none());
    assert!(tree.get("notifier_endpoint").is_none());
}
