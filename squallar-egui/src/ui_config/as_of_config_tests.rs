use super::*;
use crate::Gui;
use squallar_kv::MemoryKvStore;

/// The instant a pane is parked on survives a save/load cycle.
///
/// It did not before: `PaneTimePosture` was runtime state, so a pane scrubbed
/// back to a storm came back on live data with nothing said about it. That is
/// the one piece of pane state a scrub exists to produce, and losing it is
/// exactly what "reopen is exactly 1:1" forbids.
///
/// **The park clears `viewing_live`**, as every scrub, step and Set Time does.
/// This fixture used to leave the pane flagged live over the parked clock,
/// which is not a park: it is the stale-clock state that showed discussions and
/// alerts from hours before under a Live button painted live (report,
/// 2026-09-12), and `PaneState::time_mode` now depicts it as now.
#[test]
fn a_parked_pane_round_trips_its_instant() {
    let at = chrono::NaiveDate::from_ymd_opt(2013, 5, 20)
        .unwrap()
        .and_hms_opt(20, 0, 0)
        .unwrap();

    let store = MemoryKvStore::default();
    let mut gui = Gui::new();
    gui.panes[0].set_viewing_live(false);
    gui.panes[0].set_time_mode(crate::pane::TimeMode::AsOf(at));
    gui.save_ui_config(&store);

    let mut restored = Gui::new();
    restored.load_ui_config(&store);
    assert_eq!(
        restored.panes[0].time_mode(),
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
    gui.panes[0].set_time_mode(crate::pane::TimeMode::Live);
    gui.save_ui_config(&store);

    let json = gui.ui_config_json().expect("serialises");
    assert!(
        !json.contains("as_of"),
        "a live pane must not write the key at all, got: {json}"
    );

    let mut restored = Gui::new();
    restored.load_ui_config(&store);
    assert_eq!(restored.panes[0].time_mode(), crate::pane::TimeMode::Live);
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
/// "always false" does not pass; and
/// [`a_file_with_a_live_flag_and_a_parked_clock_loads_live_and_depicts_now`]
/// pins the case that forbids deriving this from `as_of`, against a file that
/// really carries one.
///
/// **The third row used to expect the parked clock back, and that pinned the
/// defect.** It was named "looping while still live" but armed no loop: a live
/// flag over a clock nothing is moving is exactly the state that reopened with
/// discussions and alerts from hours before (report, 2026-09-12). A live pane
/// depicts a stored instant only while it is its loop's playhead — a loop
/// running, or one restored paused and waiting to re-arm, which reopens on the
/// frame the user chose (`PaneState::restore_time_mode`) — and this row
/// restores no loop. It keeps the flag and expects the pane to depict now.
#[test]
fn viewing_live_round_trips_independently_of_the_clock() {
    let at = chrono::NaiveDate::from_ymd_opt(2022, 9, 28)
        .unwrap()
        .and_hms_opt(19, 30, 0)
        .unwrap();

    for (mode, live, depicted, why) in [
        (
            crate::pane::TimeMode::AsOf(at),
            false,
            crate::pane::TimeMode::AsOf(at),
            "scrubbed to an instant",
        ),
        (
            crate::pane::TimeMode::Live,
            true,
            crate::pane::TimeMode::Live,
            "following live data",
        ),
        (
            crate::pane::TimeMode::AsOf(at),
            true,
            crate::pane::TimeMode::Live,
            "a live flag over a clock no running loop wrote",
        ),
        (
            crate::pane::TimeMode::Live,
            false,
            crate::pane::TimeMode::Live,
            "live clock, selection detached",
        ),
    ] {
        let store = MemoryKvStore::default();
        let mut gui = Gui::new();
        {
            let pane = gui.pane_mut(0).expect("pane 0");
            // The flag first, as every park does: a clock written under a live
            // flag with no loop is one the pane does not depict.
            pane.set_viewing_live(live);
            pane.set_time_mode(mode);
        }
        gui.save_ui_config(&store);

        let mut restored = Gui::new();
        restored.load_ui_config(&store);
        let pane = restored.pane(0).expect("pane 0");
        assert_eq!(
            pane.viewing_live(),
            live,
            "viewing_live must survive: {why}"
        );
        assert_eq!(
            pane.time_mode(),
            depicted,
            "the pane must depict {depicted:?}: {why}"
        );
    }
}

/// **A file with a live flag and a parked clock loads live, and depicts now.**
///
/// The file the deployed build writes for a live pane whose loop was switched
/// off before closing: `as_of` present, `viewing_live` absent. Two claims, one
/// file:
///
/// - `viewing_live` is **not derived from `as_of`** — deriving it would read
///   this pane as parked and stop its chunk feed. It must come back `true`.
/// - the pane **depicts now** — the parked clock is left over from a loop that
///   no longer exists, and depicting it is what showed discussions and alerts
///   from hours before (report, 2026-09-12).
#[test]
fn a_file_with_a_live_flag_and_a_parked_clock_loads_live_and_depicts_now() {
    let json = r#"{"pane_count":1,"panes":[{"as_of":"2022-09-28T19:30:00","time_step_secs":600}]}"#;
    let store = MemoryKvStore::default();
    squallar_kv::KvStore::store(&store, crate::UI_CONFIG_KEY, json).expect("stores");

    let mut restored = Gui::new();
    assert!(restored.load_ui_config(&store), "premise: the file loads");
    let pane = restored.pane(0).expect("pane 0");
    assert!(
        pane.viewing_live(),
        "viewing_live must not be derived from the parked clock"
    );
    assert_eq!(
        pane.time_mode(),
        crate::pane::TimeMode::Live,
        "a live flag over a clock no running loop wrote must depict now"
    );
}
