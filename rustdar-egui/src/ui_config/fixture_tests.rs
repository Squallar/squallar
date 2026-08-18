//! Whole-file fixtures: real configs from real eras of this app, loaded
//! byte-for-byte as a user's disk would supply them.
//!
//! The unit tests beside these build their inputs by saving and mutating; a
//! fixture instead freezes what an *old build actually wrote*, so a change
//! that quietly stops reading some era of file fails here even when every
//! synthetic round trip still passes. The files live under `fixtures/` and
//! are compiled in with `include_str!` — a fixture that could drift from the
//! test reading it would pin nothing.

use crate::Gui;
use crate::config_store::{ConfigStore, MemoryConfigStore, UI_CONFIG_KEY};
use rustdar_overlays::render::overlay_state::OverlayKind;

/// A store holding exactly one era's file, as the disk would.
fn store_with(fixture: &str) -> MemoryConfigStore {
    let store = MemoryConfigStore::default();
    store
        .store(UI_CONFIG_KEY, fixture)
        .expect("the memory store accepts a write");
    store
}

/// The config a pre-M11, pre-`overlay_states` build wrote: sync was two
/// globals, the Radar toggle lived in a per-pane `layers` map, and no
/// `config_version` key existed because no version field did.
///
/// This file must keep loading forever. It pins three migrations at once:
/// the M11 fold (`viewport_sync`/`sync_layers` off seeds every restored
/// pane's links off), the legacy Radar-toggle capture (the first pane's
/// `layers["Radar"]` drives the global handler when no `overlay_states`
/// map exists), and the absence of a version key reading as the oldest
/// version rather than as an error.
#[test]
fn a_legacy_v0_config_loads_with_links_folded_off_and_its_radar_toggle_migrated() {
    let store = store_with(include_str!("fixtures/legacy_v0.json"));

    let mut gui = Gui::new();
    assert!(
        gui.overlays.is_enabled(OverlayKind::Radar),
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
        !gui.overlays.is_enabled(OverlayKind::Radar),
        "the legacy per-pane Radar toggle was not migrated to the handler",
    );

    // The rest of the file arrived: a failed load could not have applied
    // these, so they double as proof the `true` above was honest.
    assert_eq!(pane.site, "KMPX");
    assert_eq!(gui.loop_lookback_secs, 7200);
}
