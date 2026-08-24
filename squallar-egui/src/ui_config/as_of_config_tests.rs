use super::*;
use crate::Gui;
use squallar_kv::{KvStore, MemoryKvStore};

/// The instant a pane is parked on survives a save/load cycle.
///
/// It did not before: `PaneTimePosture` was runtime state, so a pane scrubbed
/// back to a storm came back on live data with nothing said about it. That is
/// the one piece of pane state a scrub exists to produce, and losing it is
/// exactly what "reopen is exactly 1:1" forbids.
#[test]
fn a_parked_pane_round_trips_its_instant() {
    let at = chrono::NaiveDate::from_ymd_opt(2013, 5, 20)
        .unwrap()
        .and_hms_opt(20, 0, 0)
        .unwrap();

    let store = MemoryKvStore::default();
    let mut gui = Gui::new();
    gui.panes[0].time.mode = crate::pane::TimeMode::AsOf(at);
    gui.save_ui_config(&store);

    let mut restored = Gui::new();
    restored.load_ui_config(&store);
    assert_eq!(
        restored.panes[0].time.mode,
        crate::pane::TimeMode::AsOf(at),
        "the parked instant must come back exactly, to the second"
    );
}

/// A live pane stays live, and writes no key at all.
///
/// The absent-key half matters: `Live` is overwhelmingly the common case, and a
/// null written into every pane of every config would be noise in a file people
/// read and diff.
#[test]
fn a_live_pane_writes_nothing_and_comes_back_live() {
    let store = MemoryKvStore::default();
    let mut gui = Gui::new();
    gui.panes[0].time.mode = crate::pane::TimeMode::Live;
    gui.save_ui_config(&store);

    let json = gui.ui_config_json().expect("serialises");
    assert!(
        !json.contains("as_of"),
        "a live pane must not write the key at all, got: {json}"
    );

    let mut restored = Gui::new();
    restored.load_ui_config(&store);
    assert_eq!(restored.panes[0].time.mode, crate::pane::TimeMode::Live);
}

/// A config written before the field loads live, which is how those sessions
/// actually ran.
#[test]
fn an_older_config_loads_live() {
    let old = r#"{"pane_count":1,"auto_poll":true,"site":"KTLX"}"#;
    let parsed: UiConfig = serde_json::from_str(old).expect("an older config still parses");
    assert!(
        parsed.panes.first().is_none_or(|p| p.as_of.is_none()),
        "no pane in a pre-field config may claim to be parked"
    );
}

/// An unreadable instant reads as live rather than failing the load.
///
/// The whole file is at stake, not the field: a hard parse error on one pane's
/// timestamp costs the user every pane, every layer and every preference in it,
/// and the autosave then rewrites the wreckage from defaults. Every other
/// tolerant read in this file exists for the same reason.
#[test]
fn a_malformed_instant_reads_as_live_and_keeps_the_file() {
    for bad in [
        "\"not a time\"",
        "\"2013-05-20\"",           // date only, no clock
        "\"2013-05-20T20:00:00Z\"", // trailing zone this build does not spell
        "\"2013-13-45T99:99:99\"",  // well-formed shape, impossible values
        "17",                       // not even a string
    ] {
        let json =
            format!(r#"{{"pane_count":1,"panes":[{{"as_of":{bad},"time_step_secs":600}}]}}"#);
        let parsed: UiConfig = match serde_json::from_str(&json) {
            Ok(parsed) => parsed,
            // A non-string is allowed to fail the field's own deserialize; what
            // it must never do is take the rest of the config with it, which is
            // what the assertion below the loop covers.
            Err(_) => continue,
        };
        assert!(
            parsed
                .panes
                .first()
                .and_then(|p| p.as_of.as_deref())
                .and_then(parse_as_of)
                .is_none(),
            "{bad} must not resolve to an instant"
        );
    }
}

/// The format is the one a hand-written scene can type, and it round-trips
/// through itself. The marketing rig seeds this field by hand, so a format only
/// the writer can produce would be a field only the writer can use.
#[test]
fn the_written_spelling_parses_back() {
    let at = chrono::NaiveDate::from_ymd_opt(2011, 4, 27)
        .unwrap()
        .and_hms_opt(22, 15, 30)
        .unwrap();
    let text = at.format(AS_OF_FORMAT).to_string();
    assert_eq!(text, "2011-04-27T22:15:30");
    assert_eq!(parse_as_of(&text), Some(at));
}

/// **`viewing_live` survives the round trip, and is not derived from the clock.**
///
/// It gates the archive auto-poll. A pane restored parked but still flagged live
/// had the poll fetch the current volume and install it over the archived one the
/// pane had just asked for — a screenshot pinned to Hurricane Ian came back
/// showing that afternoon's Florida convection, with the correct volume visible
/// in the log directly above the one that replaced it.
///
/// **Non-vacuity floor**: the live pane in the same table must come back live, so
/// "always false" does not pass; and the fourth row pins the case that forbids
/// deriving this from `as_of` — a pane playing a loop depicts an older instant
/// while still following the live site.
#[test]
fn viewing_live_round_trips_independently_of_the_clock() {
    let at = chrono::NaiveDate::from_ymd_opt(2022, 9, 28)
        .unwrap()
        .and_hms_opt(19, 30, 0)
        .unwrap();

    for (mode, live, why) in [
        (
            crate::pane::TimeMode::AsOf(at),
            false,
            "scrubbed to an instant",
        ),
        (crate::pane::TimeMode::Live, true, "following live data"),
        (
            crate::pane::TimeMode::AsOf(at),
            true,
            "looping while still live",
        ),
        (
            crate::pane::TimeMode::Live,
            false,
            "live clock, selection detached",
        ),
    ] {
        let store = MemoryKvStore::default();
        let mut gui = Gui::new();
        {
            let pane = gui.pane_mut(0).expect("pane 0");
            pane.time.mode = mode;
            pane.viewing_live = live;
        }
        gui.save_ui_config(&store);

        let mut restored = Gui::new();
        restored.load_ui_config(&store);
        let pane = restored.pane(0).expect("pane 0");
        assert_eq!(pane.viewing_live, live, "viewing_live must survive: {why}");
        assert_eq!(pane.time.mode, mode, "the clock must survive too: {why}");
    }
}
