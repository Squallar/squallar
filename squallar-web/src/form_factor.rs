//! What a browser can say about the shape of the device it runs on, from the
//! pointer media queries and the touch-point count. Pure, so the truth table
//! runs on the host; the reads live in `bridge`.
//!
//! The rule: **handheld is a coarse primary pointer with no fine pointer
//! anywhere; a fine pointer anywhere is a desktop**, and only when neither
//! query decided does `maxTouchPoints` break the tie. Screen area is never a
//! signal. The residual the rule accepts is a phone with a mouse plugged in,
//! which classifies as a desktop: it has a fine pointer, and what a desktop
//! browser may earn is bounded by the bracket, never by this reading.

pub use squallar_device_profile::budget::FormFactor;

/// `coarse` is `matchMedia("(pointer: coarse)")`, `any_fine` is
/// `matchMedia("(any-pointer: fine)")` and `max_touch_points` is
/// `navigator.maxTouchPoints`, each `None` where the browser would not say.
pub fn classify(
    coarse: Option<bool>,
    any_fine: Option<bool>,
    max_touch_points: Option<u32>,
) -> Option<FormFactor> {
    match (coarse, any_fine) {
        (_, Some(true)) => Some(FormFactor::Desktop),
        (Some(true), Some(false)) => Some(FormFactor::Handheld),
        _ => max_touch_points.map(|points| {
            if points > 0 {
                FormFactor::Handheld
            } else {
                FormFactor::Desktop
            }
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::{FormFactor, classify};

    /// One row per device the rule was written against, the residual and
    /// the failure arms included.
    #[test]
    fn every_device_row_classifies_as_the_rule_says() {
        type Row = (
            &'static str,
            Option<bool>,
            Option<bool>,
            Option<u32>,
            Option<FormFactor>,
        );
        const ROWS: [Row; 9] = [
            (
                "phone",
                Some(true),
                Some(false),
                Some(5),
                Some(FormFactor::Handheld),
            ),
            // The tiebreak is not consulted once the queries decided.
            (
                "phone reporting no touch points",
                Some(true),
                Some(false),
                Some(0),
                Some(FormFactor::Handheld),
            ),
            (
                "touch laptop",
                Some(false),
                Some(true),
                Some(10),
                Some(FormFactor::Desktop),
            ),
            (
                "desktop",
                Some(false),
                Some(true),
                Some(0),
                Some(FormFactor::Desktop),
            ),
            // The residual: a fine pointer anywhere is a desktop, and a phone
            // with a mouse plugged in has one.
            (
                "mouse on a phone",
                Some(true),
                Some(true),
                Some(5),
                Some(FormFactor::Desktop),
            ),
            (
                "queries failed, touch reported",
                None,
                None,
                Some(5),
                Some(FormFactor::Handheld),
            ),
            (
                "queries failed, no touch",
                None,
                None,
                Some(0),
                Some(FormFactor::Desktop),
            ),
            (
                "no pointer at all, touch reported",
                Some(false),
                Some(false),
                Some(1),
                Some(FormFactor::Handheld),
            ),
            ("everything failed", None, None, None, None),
        ];
        for (device, coarse, any_fine, touch, want) in ROWS {
            assert_eq!(classify(coarse, any_fine, touch), want, "{device}");
        }
    }
}
