//! **The two memory-share controls survive a restart**, and an absent pair
//! means the whole of both pools.
//!
//! The absence half is the one that protects existing users: every config
//! written before the Memory section existed says nothing about either pool,
//! and the only reading of that silence which cannot quietly take memory away
//! from somebody is "all of it".

use crate::Gui;
use squallar_device_profile::scene::PoolPercents;
use squallar_kv::{KvStore, MemoryKvStore};

/// The whole of both pools is the default, and it writes no key.
///
/// The byte-preservation rule: a config that expresses no opinion about
/// memory must come back out of a save byte-for-byte as it went in, which is
/// what `a_config_naming_an_unregistered_layer_is_written_back_byte_preserved`
/// asserts for the corpus and this asserts for a fresh `Gui`.
#[test]
fn the_whole_of_both_pools_writes_no_key() {
    let gui = Gui::new();
    assert_eq!(
        gui.memory_percents(),
        PoolPercents::FULL,
        "a fresh Gui must start at neutrality",
    );
    let json = gui.ui_config_json().expect("a config to write");
    assert!(
        !json.contains("memory_percent"),
        "the neutral pair wrote a key: {json}",
    );
}

/// A lowered share round-trips, on each pool independently.
#[test]
fn a_lowered_share_survives_a_restart() {
    for percents in [
        PoolPercents { gpu: 40, host: 100 },
        PoolPercents { gpu: 100, host: 25 },
        PoolPercents { gpu: 55, host: 70 },
        PoolPercents {
            gpu: PoolPercents::FLOOR,
            host: PoolPercents::FLOOR,
        },
    ] {
        let mut gui = Gui::new();
        gui.memory_percents = percents;
        let store = MemoryKvStore::default();
        gui.save_ui_config(&store);

        let mut reopened = Gui::new();
        assert!(reopened.load_ui_config(&store), "the config must load");
        assert_eq!(
            reopened.memory_percents(),
            percents,
            "{percents:?} did not survive a restart",
        );
    }
}

/// **A config written before the Memory section existed opens at the whole of
/// both pools.** Neutrality, not policy: any other default would make an
/// upgrade silently hold back memory the user never asked to give up.
#[test]
fn a_config_written_before_the_memory_section_opens_at_the_whole_pool() {
    let store = MemoryKvStore::default();
    store
        .store(
            crate::UI_CONFIG_KEY,
            r#"{"config_version":5,"pane_count":1,"active_pane":0,
                "loop_lookback_secs":3600,"loop_speed_fps":4.0,
                "time_step_secs":600,"panes":[{}],"preferences":{}}"#,
        )
        .expect("the memory store accepts a write");

    let mut gui = Gui::new();
    assert!(gui.load_ui_config(&store), "the pre-field config must load");
    assert_eq!(
        gui.memory_percents(),
        PoolPercents::FULL,
        "an upgrade quietly took memory away from every existing install",
    );
}

/// A hand-edited or out-of-range value costs a sensible setting, never the
/// file. Both ends, and the neutral pair is left exactly where it was.
#[test]
fn a_share_outside_the_offered_range_is_held_inside_it() {
    for (written, expected) in [
        ("0", PoolPercents::FLOOR),
        ("250", 100),
        ("100", 100),
        ("10", PoolPercents::FLOOR),
    ] {
        let store = MemoryKvStore::default();
        store
            .store(
                crate::UI_CONFIG_KEY,
                &format!(
                    r#"{{"config_version":5,"pane_count":1,"active_pane":0,
                        "loop_lookback_secs":3600,"loop_speed_fps":4.0,
                        "time_step_secs":600,"panes":[{{}}],"preferences":{{}},
                        "gpu_memory_percent":{written},
                        "system_memory_percent":{written}}}"#
                ),
            )
            .expect("the memory store accepts a write");

        let mut gui = Gui::new();
        assert!(gui.load_ui_config(&store), "a hand-edited config must load");
        assert_eq!(
            gui.memory_percents(),
            PoolPercents {
                gpu: expected,
                host: expected
            },
            "a file naming {written} % opened at {:?}",
            gui.memory_percents(),
        );
    }
}

/// **The two keys are independent on the wire**: a file that lowers only one
/// pool writes only that key, and reads back with the other untouched.
#[test]
fn lowering_one_pool_writes_one_key() {
    let mut gui = Gui::new();
    gui.memory_percents = PoolPercents { gpu: 40, host: 100 };
    let json = gui.ui_config_json().expect("a config to write");
    assert!(
        json.contains("\"gpu_memory_percent\": 40"),
        "the lowered pool wrote no key: {json}",
    );
    assert!(
        !json.contains("system_memory_percent"),
        "the untouched pool wrote a key: {json}",
    );
}
