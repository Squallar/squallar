//! **What a wait looks like on the glass, and what it may ask of the frame
//! loop — which is nothing.**
//!
//! egui's `Spinner` asks for a repaint on every frame it is drawn, from inside
//! its own paint (`paint_at`: "`ui.request_repaint(); // because it is
//! animated`"). Drawn beside a download that is waiting on the network, that is
//! a frame at the display's rate for the whole wait — 250 a second on the
//! fastest arm — showing nothing but an arc a few degrees on from where it was,
//! and every one of those frames raises no cause in [`crate::frame_need`] and
//! is charged to `egui` as a frame a widget bought for itself. Four data waits
//! in this crate drew one: the status bar's archive fetch, the transport's
//! listing wait and its render count, and the offline download's planning
//! phase. While any of them was on screen the application never went idle.
//!
//! Two things replace it here, and neither asks for a frame:
//!
//! * [`mark`] — the widget's footprint, stroke and colour, frozen at three
//!   quarters of a turn. It says *waiting*. It does not claim to know whether
//!   the wait is progressing, which a turning arc claimed on every frame
//!   whether or not a byte had moved; the words beside it carry the quantity
//!   that does move — a byte count, a frame count, a second — and those are
//!   drawn on the frame the arrival buys, because every arrival path already
//!   wakes one (`squallar_app`'s channel drains raise
//!   [`crate::frame_need::NeedCause::Arrival`] on the take, and its workers
//!   post a redraw when they send).
//! * [`seconds_tick`] — for the one part of a wait that genuinely restates the
//!   clock, an elapsed-seconds counter: the sleep that lands the next frame
//!   exactly where the number changes, and never sooner. The site that prints
//!   the counter then compares the words it drew with the words it drew last
//!   and raises [`crate::frame_need::NeedCause::Clock`] only when they differ
//!   ([`crate::frame_need::note_if_changed`]), so a frame that lands the same
//!   number stays counted as waste — the status bar chip's method, applied
//!   where the wait's counter is.
//!
//! The rule the tests hold: no source in this crate draws the widget. A wait
//! that needs to say "alive" says so with a number that moves.

/// Three quarters of a turn, in degrees.
const MARK_SWEEP_DEGREES: f32 = 270.0;

/// The stroke the widget drew its arc with, kept so the mark reads as the same
/// thing standing still.
const MARK_STROKE: f32 = 3.0;

/// The inset the widget kept between its arc and the square it allocated.
const MARK_INSET: f32 = 2.0;

/// The wait mark: a still three-quarter arc in the square the widget took, in
/// the strong text colour. Allocates the same rect and reports the same
/// accessibility kind, so nothing around it moves; asks for nothing.
pub(crate) fn mark(ui: &mut egui::Ui) -> egui::Response {
    let size = ui.style().spacing.interact_size.y;
    let (rect, response) = ui.allocate_exact_size(egui::vec2(size, size), egui::Sense::hover());
    response.widget_info(|| egui::WidgetInfo::new(egui::WidgetType::ProgressIndicator));
    if ui.is_rect_visible(rect) {
        let color = ui.visuals().strong_text_color();
        let radius = rect.height().min(rect.width()) / 2.0 - MARK_INSET;
        // One vertex per point of radius, as the widget did: smooth at any
        // size the style hands out, never a polygon at the small ones.
        let n_points = (radius.round() as u32).clamp(8, 128);
        // From twelve o'clock, clockwise (screen y grows downward).
        let start = -std::f32::consts::FRAC_PI_2;
        let end = start + MARK_SWEEP_DEGREES.to_radians();
        let points: Vec<egui::Pos2> = (0..=n_points)
            .map(|i| {
                let angle = egui::lerp(start..=end, i as f32 / n_points as f32);
                let (sin, cos) = angle.sin_cos();
                rect.center() + radius * egui::vec2(cos, sin)
            })
            .collect();
        ui.painter().add(egui::Shape::line(
            points,
            egui::Stroke::new(MARK_STROKE, color),
        ));
    }
    response
}

/// One second, for [`seconds_tick`]'s remainder.
const NANOS_PER_SEC: u32 = 1_000_000_000;

/// How long until a counter printing whole seconds of `elapsed` prints a
/// different number.
///
/// The rest of the current second — strictly positive and at most one second
/// by construction, since `subsec_nanos` is below [`NANOS_PER_SEC`]. Anything
/// faster redraws the same number; anything slower skips one. A zero is
/// impossible here, and would be the spin this module exists to remove.
pub(crate) fn seconds_tick(elapsed: std::time::Duration) -> std::time::Duration {
    std::time::Duration::from_nanos(u64::from(NANOS_PER_SEC - elapsed.subsec_nanos()))
}

#[cfg(test)]
#[path = "ui_wait/tests.rs"]
mod tests;
