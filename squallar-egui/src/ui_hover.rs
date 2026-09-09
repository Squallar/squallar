//! Tooltips, asked for only by a widget the pointer is actually on.
//!
//! `Response::on_hover_text` and its two siblings do not first check whether
//! the pointer is anywhere near the widget: each one builds an
//! `egui::containers::tooltip::Tooltip` — a `Popup` carrying the response's
//! layer, id, anchor, gap and width — and then runs
//! `Tooltip::should_show_tooltip`, which takes the context's memory lock, its
//! previous-pass state twice, the global style and five separate reads off
//! the input state before it reaches the one line that matters on nearly every
//! call: for an enabled widget, `!response.hovered()`; for a disabled one,
//! `!ctx.rect_contains_pointer(..)`. A frame asks that question once per
//! tooltip on the glass and answers "no" to all but the one under the pointer.
//!
//! So the gate is moved in front: `contains_pointer()` is a flag already on
//! the `Response` (`Flags::CONTAINS_POINTER`), and reading it is free.
//!
//! # Why the second term is there, and why it is not `hovered()`
//!
//! `should_show_tooltip` has exactly three ways to answer yes with the pointer
//! outside the widget's rect, and all three are guarded by
//! `is_our_tooltip_open`: an interactive tooltip the pointer is on or moving
//! toward, and a tooltip large enough to cover the widget it belongs to (there
//! `contains_pointer` is false precisely *because* the tooltip's own layer is
//! on top). `is_tooltip_open()` is that guard, so the disjunction admits every
//! frame egui would have shown something and nothing else. It is deliberately
//! **not** `hovered()`: a disabled widget is never hovered, and the disabled
//! arm's own test is a rect-contains — which is what `contains_pointer` is.
//!
//! The one behaviour this does drop is `Memory::everything_is_visible`, egui's
//! debug switch that opens every popup at once. Nothing in this workspace sets
//! it.

/// [`egui::Response`]'s three tooltip entry points, each behind the gate the
/// module note describes. Named apart from the inherent methods on purpose:
/// an inherent method wins over a trait one, so a call that kept egui's
/// spelling would silently keep egui's cost.
pub(crate) trait HoverTip {
    /// [`egui::Response::on_hover_text`].
    fn hover_text(self, text: impl Into<egui::WidgetText>) -> Self;

    /// [`Self::hover_text`] with the text composed only on the frame the
    /// tooltip is drawn — for a caller whose text is a `format!` rather than
    /// a literal.
    fn hover_text_lazy(self, text: impl FnOnce() -> String) -> Self;

    /// [`egui::Response::on_hover_ui`].
    fn hover_ui(self, add_contents: impl FnOnce(&mut egui::Ui)) -> Self;

    /// [`egui::Response::on_disabled_hover_ui`].
    fn disabled_hover_ui(self, add_contents: impl FnOnce(&mut egui::Ui)) -> Self;
}

/// Whether egui could draw a tooltip for this response this frame — see the
/// module note. `contains_pointer` first: it is a flag read, and it is the
/// term that is true on the frame a tooltip opens.
fn could_show(response: &egui::Response) -> bool {
    let could = response.contains_pointer() || response.is_tooltip_open();
    #[cfg(test)]
    if could {
        admitted::note();
    }
    could
}

impl HoverTip for egui::Response {
    fn hover_text(self, text: impl Into<egui::WidgetText>) -> Self {
        if could_show(&self) {
            return self.on_hover_text(text);
        }
        self
    }

    fn hover_text_lazy(self, text: impl FnOnce() -> String) -> Self {
        if could_show(&self) {
            // `on_hover_text`'s own body (egui 0.35) with the argument moved
            // inside the closure, so what is drawn is the same widget in the
            // same box.
            return self.on_hover_ui(|ui| {
                ui.set_max_width(ui.spacing().tooltip_width);
                ui.add(egui::Label::new(text()));
            });
        }
        self
    }

    fn hover_ui(self, add_contents: impl FnOnce(&mut egui::Ui)) -> Self {
        if could_show(&self) {
            return self.on_hover_ui(add_contents);
        }
        self
    }

    fn disabled_hover_ui(self, add_contents: impl FnOnce(&mut egui::Ui)) -> Self {
        if could_show(&self) {
            return self.on_disabled_hover_ui(add_contents);
        }
        self
    }
}

/// How many tooltip asks this thread has let through the gate — the counter
/// behind `tooltip_gate_tests`.
///
/// Counted at the gate rather than at the call site, so what it reports is
/// the number of times egui's `Tooltip` machinery was entered: one `Popup`
/// built out of the response, and `should_show_tooltip`'s memory lock, two
/// previous-pass reads, style read and five input reads. Test-only, and a
/// thread-local because the gate is a free function.
#[cfg(test)]
pub(crate) mod admitted {
    use std::cell::Cell;

    thread_local! {
        static ADMITTED: Cell<u64> = const { Cell::new(0) };
    }

    pub(crate) fn note() {
        ADMITTED.with(|c| c.set(c.get().wrapping_add(1)));
    }

    /// Asks admitted on this thread since the last [`reset`].
    pub(crate) fn read() -> u64 {
        ADMITTED.with(Cell::get)
    }

    pub(crate) fn reset() {
        ADMITTED.with(|c| c.set(0));
    }
}
