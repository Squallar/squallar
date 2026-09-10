//! **What the wide top bar's own row asks egui to remember**, counted off the
//! interaction registry rather than the paint list.
//!
//! `egui::WidgetRects` is the per-frame table every hover, click, focus and
//! accessibility read is served from, and a nested [`egui::Ui`] is in it:
//! `new_child` inserts the scope at `Rect::NOTHING` and the
//! `remember_min_rect` its `Drop` runs updates the same id in place. Each of
//! those is a `Context::create_widget`, so a scope costs two of them a frame
//! whatever it contains.
//!
//! The shape this pin guards is the one that keeps coming back:
//! `ui.horizontal(|ui| ui.with_layout(right_to_left, ..))`.
//! [`egui::Ui::horizontal`] is `allocate_ui_with_layout` at the **ambient**
//! direction, so a row whose content wants the other one used to be spelled as
//! two `egui::Ui`s where the row needs one — and the inner one inherits the
//! outer's rect exactly, nothing having been allocated out of it yet. The
//! one-scope spelling is [`crate::ui_layout::row_with_layout`].
use crate::input_harness::InputHarness;

/// What the wide bar's row registers, scopes and controls together.
///
/// **Five of them are `egui::Ui` scopes rather than controls**, and that is
/// the number this pin is really about: the row itself (`row_with_layout`,
/// right-to-left, so the four trailing toggles own the right edge), the
/// left-to-right scope the rest of the bar lays out in, the horizontal
/// `ScrollArea`'s own pair, and the `horizontal` inside it that the run sits
/// on. It used to be six — an `horizontal` whose entire body was a
/// `with_layout` reversing it.
const REGISTRATIONS_IN_THE_WIDE_BAR: usize = 26;

#[test]
fn the_wide_top_bar_row_registers_twenty_six_things() {
    let mut h = InputHarness::new();
    h.warm_up();
    h.frame();

    let bar = h.top_bar().rect;
    assert!(
        bar.is_finite() && bar.width() > 400.0,
        "the top bar drew at {bar:?}, which is not the wide bar this pin is \
         about, so the count below is of something else",
    );
    let interactive = h
        .widget_senses_within(bar)
        .into_iter()
        .filter(|sense| *sense != egui::Sense::hover())
        .count();
    assert!(
        interactive >= 5,
        "only {interactive} interactive control(s) inside the bar — this \
         fixture is not drawing the wide bar's run at all, so a count over it \
         proves nothing",
    );

    assert_eq!(
        h.widgets_within(bar),
        REGISTRATIONS_IN_THE_WIDE_BAR,
        "the wide top bar registered a different number of things than the \
         {REGISTRATIONS_IN_THE_WIDE_BAR} it is built from. A bar that grew \
         one is usually `Ui::horizontal` wrapping a `with_layout` that \
         reverses it — two `egui::Ui`s for one row, and each costs two \
         `Context::create_widget` calls a frame whatever it holds. If the bar \
         genuinely gained a control or a nesting level, move the constant.",
    );
}
