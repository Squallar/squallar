//! The grid a pane count gets: width-aware under `Auto`, and the user's
//! override at every width.

use super::*;
use crate::ui_layout::WidthClass;

const WIDE: WidthClass = WidthClass::Expanded;
const NARROW: WidthClass = WidthClass::Compact;

fn grid(count: usize, width: WidthClass, orientation: SplitOrientation) -> Vec<usize> {
    PaneLayout::for_count(count, width, orientation)
        .grid()
        .to_vec()
}

/// **Two panes stack on a narrow window and sit side by side on a wide one**,
/// from the same function with no `cfg` between them — the point of the item.
/// Two columns of a sub-600pt window are two slivers.
#[test]
fn two_panes_stack_when_narrow_and_sit_side_by_side_when_wide() {
    assert_eq!(grid(2, WIDE, SplitOrientation::Auto), vec![2]);
    assert_eq!(grid(2, WidthClass::Medium, SplitOrientation::Auto), vec![2]);
    assert_eq!(grid(2, NARROW, SplitOrientation::Auto), vec![1, 1]);
}

/// The two rows a narrow pair gets are one column each, and the rects they
/// produce really are stacked rather than merely differently indexed.
#[test]
fn the_narrow_pair_produces_two_full_width_rects_one_above_the_other() {
    let layout = PaneLayout::for_count(2, NARROW, SplitOrientation::Auto);
    let panel = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(390.0, 600.0));
    let top = layout.pane_rect(0, panel);
    let bottom = layout.pane_rect(1, panel);
    assert!(
        (top.width() - panel.width()).abs() < 0.5 && (bottom.width() - panel.width()).abs() < 0.5,
        "a stacked pane is the full width of the panel: {top:?} / {bottom:?}"
    );
    assert!(
        bottom.top() >= top.bottom() - 0.5,
        "the second pane must be below the first, not beside it: {top:?} / {bottom:?}"
    );
}

/// **Three panes keep `[2, 1]` at every width, and this is the deliberate
/// choice.** The alternative on a phone is `[1, 1, 1]`, three strips whose
/// binding dimension is height — the same axis the pill row and the bottom
/// margin come out of. `[2, 1]` puts the chrome on the axis each pane has to
/// spare and hands the third pane the whole width.
///
/// Asserted through the geometry rather than through the table: what is being
/// pinned is that no pane in the narrow three-up is squeezed below what the
/// stacked alternative would have given it once the chrome is paid.
#[test]
fn three_panes_do_not_become_three_strips_on_a_phone() {
    assert_eq!(grid(3, NARROW, SplitOrientation::Auto), vec![2, 1]);

    // A phone-shaped map panel, and what the chrome takes off a pane's
    // height: the pill row plus the bottom margin it sits in.
    let panel = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(390.0, 620.0));
    const CHROME_HEIGHT: f32 = 40.0;

    let chosen = PaneLayout::for_count(3, NARROW, SplitOrientation::Auto);
    let stacked = PaneLayout::for_count(3, NARROW, SplitOrientation::Rows);

    // What a plan view can draw is set by the pane's *minor* axis, after the
    // chrome has come off the height.
    let usable = |layout: &PaneLayout, idx: usize| {
        let rect = layout.pane_rect(idx, panel);
        rect.width().min(rect.height() - CHROME_HEIGHT)
    };
    let chosen_usable: Vec<f32> = (0..3).map(|idx| usable(&chosen, idx)).collect();
    let stacked_usable: Vec<f32> = (0..3).map(|idx| usable(&stacked, idx)).collect();

    let chosen_total: f32 = chosen_usable.iter().sum();
    let stacked_total: f32 = stacked_usable.iter().sum();
    assert!(
        chosen_total > stacked_total,
        "[2, 1] must beat three strips on total usable circle: \
         {chosen_usable:?} against {stacked_usable:?}"
    );
    assert!(
        chosen_usable
            .iter()
            .zip(&stacked_usable)
            .filter(|(a, b)| a > b)
            .count()
            >= 2,
        "and it must beat it for most of the panes individually, not just on \
         the total: {chosen_usable:?} against {stacked_usable:?}"
    );
}

/// Four panes are a 2x2 at every width: splitting both axes once is the best a
/// squarish window can do for four circles.
#[test]
fn four_panes_are_a_two_by_two_at_every_width() {
    for width in [NARROW, WidthClass::Medium, WIDE] {
        assert_eq!(
            grid(4, width, SplitOrientation::Auto),
            vec![2, 2],
            "{width:?}"
        );
    }
}

/// Every other count answers the same at every width — the compact table has
/// exactly one row of its own, and this says which.
#[test]
fn only_the_two_pane_row_differs_between_the_width_classes() {
    let differing: Vec<usize> = (1..=rustdar_device_profile::budget::MAX_PANES_DESKTOP)
        .filter(|&count| {
            grid(count, NARROW, SplitOrientation::Auto) != grid(count, WIDE, SplitOrientation::Auto)
        })
        .collect();
    assert_eq!(
        differing,
        vec![2],
        "the compact table differs from the wide one at these counts"
    );
}

/// **The preference overrides at every width, in both directions.** The
/// default must not be a rule: a user who wants columns on a phone gets them,
/// and a user who wants rows on a 4K desktop gets those.
#[test]
fn the_preference_overrides_the_width_class_in_both_directions() {
    for width in [NARROW, WidthClass::Medium, WIDE] {
        for count in 1..=rustdar_device_profile::budget::MAX_PANES_DESKTOP {
            assert_eq!(
                grid(count, width, SplitOrientation::Rows),
                vec![1; count],
                "{count} panes as rows at {width:?}"
            );
            assert_eq!(
                grid(count, width, SplitOrientation::Columns),
                vec![count],
                "{count} panes as columns at {width:?}"
            );
        }
    }
}

/// Whatever the width and the preference, the grid holds exactly the panes it
/// was asked for — no cell without a pane, no pane without a cell.
#[test]
fn every_grid_has_exactly_one_cell_per_pane() {
    for width in [NARROW, WidthClass::Medium, WIDE] {
        for orientation in [
            SplitOrientation::Auto,
            SplitOrientation::Rows,
            SplitOrientation::Columns,
        ] {
            for count in 1..=rustdar_device_profile::budget::MAX_PANES_DESKTOP {
                let layout = PaneLayout::for_count(count, width, orientation);
                assert_eq!(
                    layout.grid().iter().sum::<usize>(),
                    count,
                    "{count} panes at {width:?} as {orientation:?}"
                );
                let (rows, cols) = layout.ratios();
                assert_eq!(rows.len(), layout.grid().len());
                assert_eq!(cols.len(), layout.grid().len());
            }
        }
    }
}

/// A width change that does not move the grid leaves the dragged dividers
/// alone: the ratios are the user's, and only a different arity invalidates
/// them.
#[test]
fn a_width_change_that_keeps_the_grid_keeps_the_dragged_dividers() {
    let mut layout = PaneLayout::for_count(4, WIDE, SplitOrientation::Auto);
    assert!(layout.adopt_ratios(&[0.3, 0.7], &[vec![0.25, 0.75], vec![0.6, 0.4]]));

    assert!(
        !layout.reflow(NARROW, SplitOrientation::Auto),
        "4 panes are a 2x2 at both widths, so nothing should have moved"
    );
    let (rows, cols) = layout.ratios();
    assert_eq!(rows, [0.3, 0.7]);
    assert_eq!(cols, [vec![0.25, 0.75], vec![0.6, 0.4]]);
}

/// A width change that *does* move the grid rebuilds the ratios: the old ones
/// describe a grid that no longer exists, and keeping them would be a
/// zero-height pane or a panic.
#[test]
fn a_width_change_that_moves_the_grid_rebuilds_the_dividers() {
    let mut layout = PaneLayout::for_count(2, WIDE, SplitOrientation::Auto);
    assert!(layout.adopt_ratios(&[1.0], &[vec![0.8, 0.2]]));

    assert!(layout.reflow(NARROW, SplitOrientation::Auto));
    assert_eq!(layout.grid(), [1, 1]);
    let (rows, cols) = layout.ratios();
    assert_eq!(rows, [0.5, 0.5]);
    assert_eq!(cols, [vec![1.0], vec![1.0]]);
}

/// A reflow that changes nothing reports nothing, however many times it is
/// asked — this runs every frame.
#[test]
fn re_asking_the_same_width_and_preference_is_inert() {
    let mut layout = PaneLayout::for_count(3, WIDE, SplitOrientation::Auto);
    assert!(layout.adopt_ratios(&[0.6, 0.4], &[vec![0.3, 0.7], vec![1.0]]));
    for _ in 0..5 {
        assert!(!layout.reflow(WIDE, SplitOrientation::Auto));
    }
    let (rows, _) = layout.ratios();
    assert_eq!(rows, [0.6, 0.4]);
}

/// **Nothing from outside is trusted.** Every way a persisted run of ratios
/// can be wrong is refused, and refusal leaves the defaults in place rather
/// than a half-applied grid.
#[test]
fn adopt_ratios_refuses_every_shape_of_bad_input() {
    let good_rows = vec![0.4, 0.6];
    let good_cols = vec![vec![0.3, 0.7], vec![1.0]];

    // The control: this layout's own grid, so the good input really is good.
    let mut layout = PaneLayout::for_count(3, WIDE, SplitOrientation::Auto);
    assert_eq!(layout.grid(), [2, 1]);
    assert!(
        layout.adopt_ratios(&good_rows, &good_cols),
        "precondition: a well-formed run for this grid is taken"
    );

    /// One way a persisted run can be wrong: what it is, the rows, the columns.
    type BadRun = (&'static str, Vec<f32>, Vec<Vec<f32>>);

    let bad: Vec<BadRun> = vec![
        ("no rows at all", vec![], good_cols.clone()),
        ("too few rows", vec![1.0], good_cols.clone()),
        ("too many rows", vec![0.3, 0.3, 0.4], good_cols.clone()),
        (
            "a row below MIN_RATIO",
            vec![MIN_RATIO - 0.01, 1.0 - (MIN_RATIO - 0.01)],
            good_cols.clone(),
        ),
        (
            "rows that do not sum to 1",
            vec![0.4, 0.4],
            good_cols.clone(),
        ),
        ("a negative row", vec![-0.4, 1.4], good_cols.clone()),
        ("a NaN row", vec![f32::NAN, 0.5], good_cols.clone()),
        (
            "an infinite row",
            vec![f32::INFINITY, 0.5],
            good_cols.clone(),
        ),
        ("no columns at all", good_rows.clone(), vec![]),
        (
            "the wrong number of column runs",
            good_rows.clone(),
            vec![vec![1.0]],
        ),
        (
            "the wrong arity inside a column run",
            good_rows.clone(),
            vec![vec![0.3, 0.3, 0.4], vec![1.0]],
        ),
        (
            "a column below MIN_RATIO",
            good_rows.clone(),
            vec![vec![MIN_RATIO - 0.01, 1.0 - (MIN_RATIO - 0.01)], vec![1.0]],
        ),
        (
            "columns that do not sum to 1",
            good_rows.clone(),
            vec![vec![0.3, 0.3], vec![1.0]],
        ),
        (
            "a zero column, the zero-width pane",
            good_rows.clone(),
            vec![vec![0.0, 1.0], vec![1.0]],
        ),
    ];

    for (what, rows, cols) in bad {
        let mut layout = PaneLayout::for_count(3, WIDE, SplitOrientation::Auto);
        let defaults = {
            let (r, c) = layout.ratios();
            (r.to_vec(), c.to_vec())
        };
        assert!(
            !layout.adopt_ratios(&rows, &cols),
            "{what} must be refused: {rows:?} / {cols:?}"
        );
        let (r, c) = layout.ratios();
        assert_eq!(
            (r.to_vec(), c.to_vec()),
            defaults,
            "{what} was refused but left the layout changed anyway"
        );
    }
}

/// A refused run still leaves every pane a real rect. This is the failure the
/// validation exists for: a zero ratio is a pane with no pixels, and it must
/// not be reachable from a file.
#[test]
fn a_refused_run_still_leaves_every_pane_a_drawable_rect() {
    let mut layout = PaneLayout::for_count(4, WIDE, SplitOrientation::Auto);
    assert!(!layout.adopt_ratios(&[0.0, 1.0], &[vec![0.0, 1.0], vec![0.5, 0.5]]));
    let panel = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1600.0, 900.0));
    for idx in 0..4 {
        let rect = layout.pane_rect(idx, panel);
        assert!(
            rect.width() > 1.0 && rect.height() > 1.0,
            "pane {idx} came out as {rect:?}"
        );
    }
}
