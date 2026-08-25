//! **The theme choice survives a restart**, and an absent one means system.

use crate::Gui;
use crate::pane::ThemeChoice;
use squallar_kv::MemoryKvStore;

/// System is the default, and it writes no key.
///
/// The byte-preservation rule: a config that expresses no opinion about the
/// theme must come back out of a save byte-for-byte as it went in.
#[test]
fn following_the_system_writes_no_key() {
    let gui = Gui::new();
    assert_eq!(gui.theme, ThemeChoice::System, "system is the default");
    let json = gui.ui_config_json().expect("a config to write");
    assert!(!json.contains("\"theme\""), "system wrote a key: {json}");
}

/// An explicit choice round-trips.
#[test]
fn an_explicit_choice_round_trips() {
    for choice in [ThemeChoice::Light, ThemeChoice::Dark] {
        let mut gui = Gui::new();
        gui.theme = choice;
        let store = MemoryKvStore::default();
        gui.save_ui_config(&store);

        let mut reopened = Gui::new();
        assert!(reopened.load_ui_config(&store), "the config must load");
        assert_eq!(
            reopened.theme, choice,
            "{choice:?} did not survive a restart"
        );
    }
}

/// An unknown spelling follows the system rather than refusing to open.
///
/// Read tolerantly, like every other field in this file: a config hand-edited
/// or written by a later build must still open.
#[test]
fn an_unknown_spelling_follows_the_system() {
    assert_eq!(ThemeChoice::parse("aubergine"), ThemeChoice::System);
    assert_eq!(ThemeChoice::parse(""), ThemeChoice::System);
    // Non-triviality floor: the spellings it DOES know must still parse.
    assert_eq!(ThemeChoice::parse("light"), ThemeChoice::Light);
    assert_eq!(ThemeChoice::parse("dark"), ThemeChoice::Dark);
}

/// **`System` is the arm with no opinion**, which is what lets the platform
/// answer; the other two are answers in themselves.
///
/// This is the distinction `App::resolve_theme` branches on, so it is stated
/// here rather than left implicit in a `match` in another crate.
#[test]
fn only_system_defers_to_the_platform() {
    assert_eq!(ThemeChoice::System.is_dark(), None);
    assert_eq!(ThemeChoice::Light.is_dark(), Some(false));
    assert_eq!(ThemeChoice::Dark.is_dark(), Some(true));
}

/// The spelling written is the spelling read.
#[test]
fn the_wire_spelling_round_trips_through_itself() {
    for choice in [ThemeChoice::System, ThemeChoice::Light, ThemeChoice::Dark] {
        assert_eq!(ThemeChoice::parse(choice.as_str()), choice);
    }
}
