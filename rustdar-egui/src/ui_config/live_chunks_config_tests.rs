use super::*;
use crate::Gui;

/// The setting survives a save/load cycle in both positions.
#[test]
fn the_live_chunks_setting_round_trips() {
    for enabled in [true, false] {
        let mut gui = Gui::new();
        gui.set_live_chunks(enabled);
        let json = gui.ui_config_json().expect("serialises");
        let parsed: UiConfig = serde_json::from_str(&json).expect("parses");
        assert_eq!(parsed.live_chunks, enabled);
    }
}

/// A config written before the field existed takes the default rather than
/// failing to parse — the mechanism `#[serde(default)]` on the container
/// provides, and the one `auto_poll` already relies on.
#[test]
fn a_config_written_before_this_field_defaults_to_chunks() {
    let old = r#"{"pane_count":1,"active_pane":0,"auto_poll":true,"site":"KTLX"}"#;
    let parsed: UiConfig = serde_json::from_str(old).expect("an older config still parses");
    assert!(
        parsed.live_chunks,
        "an existing install would silently lose the low-latency feed"
    );
}
