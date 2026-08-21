//! The pane grid as a user meets it: the width deciding the split at runtime,
//! the toggle that overrides the decision.

use super::*;
use crate::pane::SplitOrientation;

fn desktop() -> InputHarness {
    let h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    assert_eq!(
        h.width_class(),
        crate::ui_layout::WidthClass::Expanded,
        "precondition: a desktop window"
    );
    h
}

fn narrow() -> InputHarness {
    let h = InputHarness::with_screen(egui::vec2(420.0, 1400.0));
    assert_eq!(
        h.width_class(),
        crate::ui_layout::WidthClass::Compact,
        "precondition: a phone-shaped window"
    );
    h
}

// ---------------------------------------------------------------- W8

/// **The same running binary restacks when the window narrows.** No `cfg` can
/// do this: one process, one build, two width classes reached by a resize.
#[test]
fn narrowing_the_live_window_restacks_a_pair_of_panes() {
    let mut h = desktop();
    h.set_pane_count(2);
    assert_eq!(h.pane_grid(), vec![2], "wide: side by side");
    let wide_rects = h.pane_rects();
    assert!(
        (wide_rects[0].top() - wide_rects[1].top()).abs() < 1.0,
        "wide: the two panes share a row: {wide_rects:?}"
    );

    h.set_screen(egui::vec2(420.0, 1400.0));
    h.warm_up();
    assert_eq!(
        h.width_class(),
        crate::ui_layout::WidthClass::Compact,
        "the resize must have crossed the breakpoint"
    );
    assert_eq!(h.pane_grid(), vec![1, 1], "narrow: stacked");
    let narrow_rects = h.pane_rects();
    assert!(
        narrow_rects[1].top() >= narrow_rects[0].bottom() - 1.0,
        "narrow: the second pane sits below the first: {narrow_rects:?}"
    );

    h.set_screen(egui::vec2(1400.0, 900.0));
    h.warm_up();
    assert_eq!(h.pane_grid(), vec![2], "widening again must put them back");
}

// ---------------------------------------------------------------- W9

/// The toggle is drawn only when there is a split to orient — one pane has no
/// divider, so three buttons would be an option the window cannot express.
#[test]
fn the_split_toggle_appears_only_with_more_than_one_pane() {
    let mut h = desktop();
    assert_eq!(h.pane_count(), 1);
    assert!(
        h.split_options().is_empty(),
        "one pane must draw no orientation buttons"
    );

    h.set_pane_count(2);
    let drawn: Vec<SplitOrientation> = h
        .split_options()
        .iter()
        .map(|probe| probe.orientation)
        .collect();
    assert_eq!(
        drawn,
        vec![
            SplitOrientation::Auto,
            SplitOrientation::Rows,
            SplitOrientation::Columns
        ],
        "every option must be expressed, including the default"
    );
    assert_eq!(
        h.split_options()
            .iter()
            .filter(|probe| probe.selected)
            .map(|probe| probe.orientation)
            .collect::<Vec<_>>(),
        vec![SplitOrientation::Auto],
        "and exactly one of them must read as chosen"
    );
}

/// **The toggle flips a side-by-side pair to stacked and back, from the top
/// bar, at a wide width** — where `Auto` would never have stacked them.
#[test]
fn the_toggle_flips_a_wide_pair_to_stacked_and_back() {
    let mut h = desktop();
    h.set_pane_count(2);
    assert_eq!(h.pane_grid(), vec![2]);

    let rows = h.split_option(SplitOrientation::Rows).expect("Rows drawn");
    h.mouse_click(rows.rect.center());
    h.warm_up();
    assert_eq!(h.pane_grid(), vec![1, 1], "Rows must stack them");
    let stacked = h.pane_rects();
    assert!(
        stacked[1].top() >= stacked[0].bottom() - 1.0,
        "and the rects must really be stacked: {stacked:?}"
    );

    let cols = h
        .split_option(SplitOrientation::Columns)
        .expect("Columns drawn");
    h.mouse_click(cols.rect.center());
    h.warm_up();
    assert_eq!(h.pane_grid(), vec![2], "Columns must put them back");

    let auto = h.split_option(SplitOrientation::Auto).expect("Auto drawn");
    h.mouse_click(auto.rect.center());
    h.warm_up();
    assert_eq!(
        h.split_options()
            .iter()
            .filter(|probe| probe.selected)
            .map(|probe| probe.orientation)
            .collect::<Vec<_>>(),
        vec![SplitOrientation::Auto],
        "Auto must be reachable again, or the default becomes a one-way door"
    );
    assert_eq!(h.pane_grid(), vec![2], "and Auto is columns at this width");
}

/// **And the same toggle overrides in the other direction on a phone**, where
/// `Auto` stacks: the default must not be a rule at either end.
#[test]
fn the_toggle_overrides_the_narrow_default_too() {
    let mut h = narrow();
    h.set_pane_count(2);
    assert_eq!(h.pane_grid(), vec![1, 1], "Auto stacks a narrow pair");

    // The phone top bar has no segments; the Layers sheet page hosts them.
    h.mouse_click(h.bottom_bar().layers.0.center());
    h.warm_up();
    assert_eq!(h.sheet().page, Some(crate::ui::SheetPage::Layers));

    let cols = h
        .split_option(SplitOrientation::Columns)
        .expect("the sheet must host the toggle too");
    h.mouse_click(cols.rect.center());
    h.warm_up();
    assert_eq!(
        h.pane_grid(),
        vec![2],
        "Columns must hold at a compact width"
    );
}

/// **The sheet's galley really fits the run it hosts.** The phone page draws
/// the segments in the roomy form, so the extra buttons are a real fit risk
/// rather than a hypothetical one — measured against the sheet's own rect, not
/// assumed.
#[test]
fn the_split_toggle_fits_the_phone_sheets_galley() {
    let mut h = narrow();
    h.set_pane_count(rustdar_device_profile::budget::MAX_PANES_MOBILE);
    h.mouse_click(h.bottom_bar().layers.0.center());
    h.warm_up();
    assert_eq!(h.sheet().page, Some(crate::ui::SheetPage::Layers));

    let sheet = h.sheet_rect().expect("the Layers page is open");
    let drawn = h.split_options();
    assert_eq!(drawn.len(), 3, "precondition: the toggle drew");
    for probe in drawn {
        assert!(
            sheet.contains_rect(probe.rect),
            "the {:?} button at {:?} spills out of the sheet {sheet:?}",
            probe.orientation,
            probe.rect
        );
        assert!(
            probe.rect.width() > 1.0 && probe.rect.height() > 1.0,
            "the {:?} button collapsed to {:?} instead of laying out",
            probe.orientation,
            probe.rect
        );
    }
    // The pane-count segments must not have been pushed out to make room.
    assert_eq!(
        h.pane_option_counts().len(),
        crate::ui_layout::WidthClass::max_panes_absolute(),
        "the counts must still all be drawn beside the new buttons"
    );
}

/// The chosen split survives a restart along with the dividers — the reopen
/// rule, driven through the real frame loop rather than the config unit tests.
#[test]
fn the_chosen_split_comes_back_after_a_restart() {
    use rustdar_kv::MemoryKvStore;
    let store = MemoryKvStore::default();

    let mut h = desktop();
    h.set_pane_count(2);
    let rows = h.split_option(SplitOrientation::Rows).expect("Rows drawn");
    h.mouse_click(rows.rect.center());
    h.warm_up();
    assert_eq!(h.pane_grid(), vec![1, 1]);
    h.gui().save_ui_config(&store);

    let mut reopened = desktop();
    assert!(reopened.gui_mut().load_ui_config(&store));
    reopened.warm_up();
    assert_eq!(
        reopened.pane_grid(),
        vec![1, 1],
        "the stacked pair must come back stacked on a wide window"
    );
    assert!(
        reopened
            .split_option(SplitOrientation::Rows)
            .expect("Rows drawn")
            .selected,
        "and the toggle must show the restored choice"
    );
}
