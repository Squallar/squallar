//! **What one layer-stack row asks egui to remember**, counted off the
//! interaction registry rather than the paint list.
//!
//! `egui::WidgetRects` is the per-frame table every hover, click, focus and
//! accessibility read is served from, and a nested [`egui::Ui`] is in it: its
//! `new_child` inserts the scope at `Rect::NOTHING` and the
//! `remember_min_rect` its `Drop` runs updates the same id in place. Each of
//! those is a `Context::create_widget`, so a scope costs two of them a frame
//! whatever it contains — measured at ~776 Ir a call, ~3.4 k Ir a scope, on
//! the callgrind arm this pin was written from.
//!
//! That is the half of a frame no paint memo can take away: replaying a
//! surface's shapes still leaves every widget in it to register. So the thing
//! worth pinning is the scope count, and the row is where it multiplies —
//! whatever this number is, the panel pays it once per layer, every frame.
use crate::input_harness::InputHarness;

/// What a row with no status line registers: the whole-row click target, the
/// row body's own child `Ui`, the drag grip, the eye, the right-to-left
/// scope the trailing controls lay out in, the remove button, the top-down
/// scope the name block stacks in, and the name label.
///
/// **Two of the eight are `Ui` scopes rather than widgets**, and that is the
/// number this pin is really about: the row body and the id salt used to be
/// separate scopes — a `push_id` wrapping a `new_child` — and merging them
/// took a whole `Ui` off every row of every frame.
const WIDGETS_PER_ROW: usize = 8;

/// A row whose handler offers a status line adds the one label that carries
/// it. Nothing else about the row changes, so the status is the only term.
const WIDGETS_PER_STATUS_LINE: usize = 1;

#[test]
fn a_stack_row_registers_eight_widgets_plus_its_status_label() {
    let mut h = InputHarness::new();
    h.warm_up();
    h.frame();

    let stack = h.gui().stack_for_test().clone();
    assert!(
        stack.open,
        "the layer panel is shut on this harness, so the rows below were \
         never registered and the counts prove nothing",
    );
    assert!(
        stack.rows.len() >= 5,
        "a fixture with {} row(s) cannot show a per-row cost multiplying, \
         which is the only reason this pin exists",
        stack.rows.len(),
    );
    let with_status = stack
        .rows
        .iter()
        .filter(|row| row.status_line.is_some())
        .count();
    assert!(
        with_status > 0 && with_status < stack.rows.len(),
        "every row in this fixture carries the same status posture ({} of {} \
         with a line), so the status term below is never exercised in both \
         directions",
        with_status,
        stack.rows.len(),
    );

    for row in &stack.rows {
        let want =
            WIDGETS_PER_ROW + usize::from(row.status_line.is_some()) * WIDGETS_PER_STATUS_LINE;
        assert_eq!(
            h.widgets_within(row.rect),
            want,
            "the {} row registered a different number of widgets than the {} \
             it is built from. A row that grew one is usually a nested \
             `egui::Ui` that did not have to be there — `Ui::horizontal` \
             round a `with_layout`, a `push_id` round a scope that could \
             carry the salt itself — and it costs two `create_widget` calls \
             a frame per layer in the stack. If the row genuinely gained a \
             control, move the constant.",
            row.kind.as_str(),
            want,
        );
    }
}
