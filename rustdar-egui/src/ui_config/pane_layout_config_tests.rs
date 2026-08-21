//! The pane grid's persistence: the split preference and the dragged divider
//! positions, and what a file that lies about them gets.

use super::*;
use crate::Gui;
use crate::pane::SplitOrientation;
use rustdar_kv::{KvStore, MemoryKvStore};

/// A `Gui` at the width class a saved config was written on. `load_ui_config`
/// runs before any frame, so the width it sees is whatever the `Gui` was last
/// laid out at; `Gui::settle_pane_layout` is the frame-time half and the
/// harness tests exercise that.
fn gui_at(width: crate::ui_layout::WidthClass, panes: usize) -> Gui {
    let mut gui = Gui::new();
    gui.set_width_class_for_test(width);
    gui.set_pane_count_for_test(panes);
    gui
}

/// The dividers the user dragged and the split they chose are where they left
/// them after a restart — the 1:1 reopen rule, which the link flags already
/// honoured and the layout did not.
#[test]
fn the_dragged_dividers_and_the_chosen_split_survive_a_restart() {
    let store = MemoryKvStore::default();
    let mut gui = gui_at(crate::ui_layout::WidthClass::Expanded, 4);
    gui.set_split_orientation(SplitOrientation::Columns);
    assert_eq!(gui.pane_layout_for_test().grid(), [4]);
    assert!(
        gui.pane_layout_mut_for_test()
            .adopt_ratios(&[1.0], &[vec![0.4, 0.2, 0.2, 0.2]]),
        "precondition: the dragged run describes this grid"
    );
    gui.save_ui_config(&store);

    let mut restored = gui_at(crate::ui_layout::WidthClass::Expanded, 1);
    assert!(restored.load_ui_config(&store));
    assert_eq!(
        restored.split_orientation_for_test(),
        SplitOrientation::Columns
    );
    let (rows, cols) = restored.pane_layout_for_test().ratios();
    assert_eq!(rows, [1.0]);
    assert_eq!(cols, [vec![0.4, 0.2, 0.2, 0.2]]);
}

/// **A config from before the fields loads as it always ran**: `Auto`, and the
/// `for_count` defaults. Additive, no `CONFIG_VERSION` bump — the absence is
/// not a gap to migrate, it is the answer.
#[test]
fn an_older_config_without_the_fields_loads_as_auto_with_default_dividers() {
    let old = r#"{"pane_count":2,"active_pane":0,"site":"KTLX"}"#;
    let parsed: UiConfig = serde_json::from_str(old).expect("an older config still parses");
    assert_eq!(parsed.split_orientation, SplitOrientation::Auto);
    assert!(parsed.row_ratios.is_empty());
    assert!(parsed.col_ratios.is_empty());

    let store = MemoryKvStore::default();
    store.store_now(crate::UI_CONFIG_KEY, old).expect("stored");
    let mut gui = gui_at(crate::ui_layout::WidthClass::Expanded, 1);
    assert!(gui.load_ui_config(&store));
    assert_eq!(gui.split_orientation_for_test(), SplitOrientation::Auto);
    assert_eq!(gui.pane_layout_for_test().grid(), [2]);
    let (rows, cols) = gui.pane_layout_for_test().ratios();
    assert_eq!(rows, [1.0]);
    assert_eq!(cols, [vec![0.5, 0.5]]);
}

/// The fields are declared on the struct, so the `#[serde(flatten)] unknown`
/// passthrough does not swallow them into opaque baggage nothing can read.
#[test]
fn the_new_keys_are_declared_rather_than_swallowed_by_the_unknown_map() {
    let mut gui = gui_at(crate::ui_layout::WidthClass::Expanded, 2);
    gui.set_split_orientation(SplitOrientation::Rows);
    let json = gui.ui_config_json().expect("serialises");
    let parsed: UiConfig = serde_json::from_str(&json).expect("round-trips");
    assert_eq!(parsed.split_orientation, SplitOrientation::Rows);
    for key in ["split_orientation", "row_ratios", "col_ratios"] {
        assert!(
            !parsed.unknown.contains_key(key),
            "{key} landed in the unknown passthrough, so nothing reads it"
        );
        assert!(
            json.contains(key),
            "{key} was not written to the file at all"
        );
    }
}

/// **A malformed ratio list falls back to the defaults**, and the load still
/// succeeds: a hand-edited or truncated divider run must cost the user their
/// divider positions, not the whole config and not a zero-height pane.
///
/// Every row here is a distinct way the file can be wrong, and the assertion
/// is the same for all of them — the defaults, and four drawable rects.
#[test]
fn a_malformed_ratio_list_falls_back_instead_of_panicking() {
    let cases: Vec<(&str, serde_json::Value, serde_json::Value)> = vec![
        (
            "a row run of the wrong arity",
            serde_json::json!([1.0]),
            serde_json::json!([[0.5, 0.5], [0.5, 0.5]]),
        ),
        (
            "a zero row, the zero-height pane",
            serde_json::json!([0.0, 1.0]),
            serde_json::json!([[0.5, 0.5], [0.5, 0.5]]),
        ),
        (
            "a row below the floor",
            serde_json::json!([0.01, 0.99]),
            serde_json::json!([[0.5, 0.5], [0.5, 0.5]]),
        ),
        (
            "rows that do not add up",
            serde_json::json!([0.2, 0.2]),
            serde_json::json!([[0.5, 0.5], [0.5, 0.5]]),
        ),
        (
            "a negative column",
            serde_json::json!([0.5, 0.5]),
            serde_json::json!([[-0.5, 1.5], [0.5, 0.5]]),
        ),
        (
            "a column run of the wrong arity",
            serde_json::json!([0.5, 0.5]),
            serde_json::json!([[1.0], [0.5, 0.5]]),
        ),
        (
            "no column runs at all",
            serde_json::json!([0.5, 0.5]),
            serde_json::json!([]),
        ),
    ];

    for (what, rows, cols) in cases {
        let store = MemoryKvStore::default();
        let donor = gui_at(crate::ui_layout::WidthClass::Expanded, 4);
        donor.save_ui_config(&store);
        let json = store.load(crate::UI_CONFIG_KEY).expect("just saved");
        let mut value: serde_json::Value = serde_json::from_str(&json).expect("valid json");
        value["row_ratios"] = rows;
        value["col_ratios"] = cols;
        store
            .store_now(crate::UI_CONFIG_KEY, &value.to_string())
            .expect("stored");

        let mut gui = gui_at(crate::ui_layout::WidthClass::Expanded, 1);
        assert!(
            gui.load_ui_config(&store),
            "{what}: the load must still succeed — bad dividers are not a bad config"
        );
        assert_eq!(gui.pane_count(), 4, "{what}");
        let (r, c) = gui.pane_layout_for_test().ratios();
        assert_eq!(r, [0.5, 0.5], "{what}: rows must be the for_count defaults");
        assert_eq!(
            c,
            [vec![0.5, 0.5], vec![0.5, 0.5]],
            "{what}: columns must be the for_count defaults"
        );

        let panel = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1600.0, 900.0));
        for idx in 0..4 {
            let rect = gui.pane_layout_for_test().pane_rect(idx, panel);
            assert!(
                rect.width() > 1.0 && rect.height() > 1.0,
                "{what}: pane {idx} came out as {rect:?}"
            );
        }
    }
}

/// A run that describes a *different* window's grid is refused rather than
/// stretched. Saved on a wide window as two columns, opened on a phone where
/// the pair stacks: the arity no longer matches and the defaults win.
#[test]
fn dividers_saved_for_another_windows_grid_are_refused() {
    let store = MemoryKvStore::default();
    let mut wide = gui_at(crate::ui_layout::WidthClass::Expanded, 2);
    assert_eq!(wide.pane_layout_for_test().grid(), [2]);
    assert!(
        wide.pane_layout_mut_for_test()
            .adopt_ratios(&[1.0], &[vec![0.7, 0.3]])
    );
    wide.save_ui_config(&store);

    let mut narrow = gui_at(crate::ui_layout::WidthClass::Compact, 1);
    assert!(narrow.load_ui_config(&store));
    assert_eq!(
        narrow.pane_layout_for_test().grid(),
        [1, 1],
        "the compact window stacks the pair"
    );
    let (rows, cols) = narrow.pane_layout_for_test().ratios();
    assert_eq!(rows, [0.5, 0.5]);
    assert_eq!(cols, [vec![1.0], vec![1.0]]);
}

/// A save straight after a load writes back what it read — the round trip is a
/// fixed point, so an autosave cannot quietly erode the user's dividers.
#[test]
fn a_save_after_a_load_writes_back_the_same_dividers() {
    let store = MemoryKvStore::default();
    let mut gui = gui_at(crate::ui_layout::WidthClass::Expanded, 4);
    assert!(
        gui.pane_layout_mut_for_test()
            .adopt_ratios(&[0.35, 0.65], &[vec![0.2, 0.8], vec![0.55, 0.45]])
    );
    gui.save_ui_config(&store);
    let first = store.load(crate::UI_CONFIG_KEY).expect("just saved");

    let mut reopened = gui_at(crate::ui_layout::WidthClass::Expanded, 1);
    assert!(reopened.load_ui_config(&store));
    reopened.save_ui_config(&store);
    let second = store.load(crate::UI_CONFIG_KEY).expect("saved again");

    let a: serde_json::Value = serde_json::from_str(&first).expect("valid");
    let b: serde_json::Value = serde_json::from_str(&second).expect("valid");
    for key in ["row_ratios", "col_ratios", "split_orientation"] {
        assert_eq!(a[key], b[key], "{key} moved across a load-then-save");
    }
}
