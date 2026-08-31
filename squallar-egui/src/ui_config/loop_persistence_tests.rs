//! **A loop survives a restart** — the `loop_playback` half of "reopen is
//! exactly 1:1".
//!
//! Everything else a pane holds already persisted; a loop did not. Closing the
//! app on a playing loop reopened on a still picture, with nothing on screen
//! saying anything had been dropped.

use super::*;
use crate::Gui;
use crate::pane::{LoopArm, LoopPhase};
use squallar_kv::MemoryKvStore;

/// A pane with no loop writes no key at all.
///
/// `skip_serializing_if` for the same reason `as_of` and `viewing_live` have
/// one: no loop is the overwhelming default, and writing the key into every
/// pane of every config would move the bytes of files that say nothing about
/// loops — which
/// `a_config_naming_an_unregistered_layer_is_written_back_byte_preserved`
/// exists to forbid.
#[test]
fn a_pane_with_no_loop_writes_no_key() {
    let gui = Gui::new();
    let json = gui.ui_config_json().expect("a config to write");
    assert!(
        !json.contains("loop_playback"),
        "a loopless pane wrote the key: {json}"
    );
}

/// Each phase maps to the state a reopen should land in.
///
/// The transient phases collapse to `paused` deliberately: what listing,
/// rendering and paused have in common is "armed, not advancing", and a config
/// written mid-fetch must not reopen claiming frames it never had.
#[test]
fn every_phase_maps_to_what_a_reopen_should_do() {
    let cases = [
        (LoopPhase::Inactive, None),
        (LoopPhase::Playing, Some("playing")),
        (LoopPhase::Paused, Some("paused")),
        (LoopPhase::Ready, Some("paused")),
        (LoopPhase::Rendering, Some("paused")),
        (LoopPhase::FetchingScanList, Some("paused")),
    ];
    for (phase, want) in cases {
        let mut gui = Gui::new();
        gui.pane_mut(0).expect("pane 0").transport_state_mut().phase = phase;
        let pane = gui.pane(0).expect("pane 0");
        assert_eq!(
            loop_playback_of(pane).as_deref(),
            want,
            "{phase:?} should reopen as {want:?}",
        );
    }
}

/// **The round trip**: a playing loop is written, read back, and asks to be
/// armed and played.
#[test]
fn a_playing_loop_round_trips_as_a_request_to_arm_and_play() {
    let mut gui = Gui::new();
    gui.pane_mut(0).expect("pane 0").transport_state_mut().phase = LoopPhase::Playing;

    let store = MemoryKvStore::default();
    gui.save_ui_config(&store);

    let mut reopened = Gui::new();
    assert!(reopened.load_ui_config(&store), "the config must load");

    assert_eq!(
        reopened.pane(0).expect("pane 0").loop_arm_pending,
        Some(LoopArm { playing: true }),
        "a loop that was playing must come back asking to play",
    );
}

/// And a paused one comes back paused rather than playing.
///
/// The counterweight: without it, a restore that armed everything playing would
/// pass the test above and silently start loops the user had stopped.
#[test]
fn a_paused_loop_round_trips_without_asking_to_play() {
    let mut gui = Gui::new();
    gui.pane_mut(0).expect("pane 0").transport_state_mut().phase = LoopPhase::Paused;

    let store = MemoryKvStore::default();
    gui.save_ui_config(&store);

    let mut reopened = Gui::new();
    assert!(reopened.load_ui_config(&store));
    assert_eq!(
        reopened.pane(0).expect("pane 0").loop_arm_pending,
        Some(LoopArm { playing: false }),
    );
}

/// A pane that had no loop asks for none — so "restore the loop" cannot be
/// reached by arming every pane.
#[test]
fn a_pane_without_a_loop_asks_for_nothing() {
    let gui = Gui::new();
    let store = MemoryKvStore::default();
    gui.save_ui_config(&store);

    let mut reopened = Gui::new();
    assert!(reopened.load_ui_config(&store));
    assert_eq!(reopened.pane(0).expect("pane 0").loop_arm_pending, None);
}

/// **A wish still waiting for its first scan is written back, not dropped.**
///
/// A restored loop cannot arm until the site's first scan lands, so between
/// boot and that arrival the transport's phase is honestly `Inactive` while
/// `loop_arm_pending` still carries the whole request. A save made in that
/// window used to read the phase alone and write no key — closing the app
/// twice in a row before data arrived silently shed the loop.
#[test]
fn a_wish_still_waiting_for_its_scan_survives_a_save() {
    for (playing, want) in [(true, "playing"), (false, "paused")] {
        let mut gui = Gui::new();
        {
            let pane = gui.pane_mut(0).expect("pane 0");
            pane.loop_arm_pending = Some(LoopArm { playing });
            assert_eq!(
                pane.transport_state().phase,
                LoopPhase::Inactive,
                "precondition: the wish is parked, not armed",
            );
        }
        assert_eq!(
            loop_playback_of(gui.pane(0).expect("pane 0")).as_deref(),
            Some(want),
            "a parked wish (playing: {playing}) must be written back",
        );
    }
}

/// An unrecognised spelling reads as no loop rather than as an error.
///
/// Every other field in this file is read tolerantly, and a config that has been
/// hand-edited or written by a later build must still open.
#[test]
fn an_unknown_spelling_reads_as_no_loop() {
    assert_eq!(loop_arm_from_config(None), None);
    assert_eq!(loop_arm_from_config(Some("")), None);
    assert_eq!(loop_arm_from_config(Some("rewinding")), None);
    assert_eq!(
        loop_arm_from_config(Some("playing")),
        Some(LoopArm { playing: true }),
        "non-triviality floor: a spelling this DOES know must still parse",
    );
}

/// **The measurement rig's scene E seeds really ask for a playing loop.**
///
/// The tolerance the test above pins is exactly what makes this one
/// necessary. `loop_arm_from_config` reads anything it does not recognise as
/// *no loop at all* — silently, by design — so a typo in the launcher's seed
/// fails nothing anywhere: it produces a scene E leg that measures scene A
/// and files the row as E. The rig's spelling is therefore checked against
/// this module's own vocabulary rather than restated in the launcher's
/// language, which is the same seam `raster_telemetry_line_tests` holds for
/// the console sentences.
#[test]
fn the_measure_rig_seeds_a_loop_this_build_recognises() {
    const RUN_MEASURE: &str = include_str!("../../../.github/browser-rig/run_measure.sh");

    // The seed is JSON inside a shell single-quoted string inside JSON, so
    // the key arrives backslash-escaped: \"loop_playback\":\"playing\".
    let key = "\\\"loop_playback\\\":\\\"";
    let seeded: Vec<&str> = RUN_MEASURE
        .match_indices(key)
        .map(|(at, _)| {
            let rest = &RUN_MEASURE[at + key.len()..];
            let end = rest.find('\\').expect("the seeded value is closed");
            &rest[..end]
        })
        .collect();
    assert!(
        !seeded.is_empty(),
        "run_measure.sh no longer seeds `loop_playback` anywhere, so every \
         scene E leg measures a still picture and reports it as a loop",
    );
    for value in seeded {
        assert_eq!(
            loop_arm_from_config(Some(value)),
            Some(LoopArm { playing: true }),
            "run_measure.sh seeds loop_playback={value:?}, which this build \
             reads as no armed-and-playing loop — a scene E leg armed with \
             it measures no loop at all",
        );
    }
}
