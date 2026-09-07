//! Tests for [`super`]: the tick lands where the number moves, the mark asks
//! for nothing where the widget it replaced asked every frame, and no source
//! in this crate draws that widget.

use super::{mark, seconds_tick};
use std::time::Duration;

/// **The tick is the rest of the current second**: adding it crosses exactly
/// one whole-second boundary, and one nanosecond less crosses none. Held at
/// the phases that have bitten sleeps before — zero, one past, one short, and
/// the whole-second case where a remainder spelled as `1s - subsec` is a
/// whole second and a remainder spelled as `subsec` is a zero-length spin.
#[test]
fn the_tick_lands_exactly_where_the_number_moves() {
    for millis in [0u64, 1, 250, 999, 1_000, 1_001, 4_500, 59_999, 3_600_000] {
        let elapsed = Duration::from_millis(millis);
        let tick = seconds_tick(elapsed);
        assert!(
            !tick.is_zero() && tick <= Duration::from_secs(1),
            "at {millis}ms the counter asked for a {tick:?} sleep",
        );
        assert_eq!(
            (elapsed + tick).as_secs(),
            elapsed.as_secs() + 1,
            "at {millis}ms the tick does not land on the next number",
        );
        assert_eq!(
            (elapsed + tick - Duration::from_nanos(1)).as_secs(),
            elapsed.as_secs(),
            "at {millis}ms the tick lands late, past a number nobody saw",
        );
    }
}

/// One pass over `draw`, on a context that has already run one: the soonest
/// repaint the pass asked for, and the stroke widths of the paths it painted.
///
/// The warm-up pass is not politeness. A fresh `egui::Context` is built with
/// one repaint outstanding ("let\u{2019}s run a couple of frames at the start"),
/// so its first pass reports `ZERO` whatever was drawn on it, and a widget
/// measured on a fresh context cannot be told from one that asks every frame.
/// The outstanding count is spent by the empty pass, and what the measured
/// pass reports is what `draw` asked for.
fn one_pass(draw: impl FnOnce(&mut egui::Ui)) -> (Duration, Vec<f32>) {
    let ctx = egui::Context::default();
    let _ = ctx.run_ui(egui::RawInput::default(), |_| {});
    // `run_ui` takes an `FnMut` and runs it once; the `Option` is how a
    // once-only body is handed to it.
    let mut draw = Some(draw);
    let out = ctx.run_ui(egui::RawInput::default(), |ui| {
        if let Some(draw) = draw.take() {
            draw(ui);
        }
    });
    let asked = out
        .viewport_output
        .values()
        .map(|v| v.repaint_delay)
        .min()
        .unwrap_or(Duration::MAX);
    let widths = out
        .shapes
        .iter()
        .filter_map(|clipped| match &clipped.shape {
            egui::Shape::Path(path) => Some(path.stroke.width),
            _ => None,
        })
        .collect();
    (asked, widths)
}

/// **The mark asks for no frame, and paints.** The widget it replaced is the
/// control on both counts: it asks for the next frame now, and it paints an arc
/// of the same stroke — so a context that reported `MAX` for both, or painted
/// nothing for either, is one this test cannot read rather than one where the
/// mark is quiet.
#[test]
fn the_mark_asks_for_no_frame_where_the_widget_it_replaces_asked_every_frame() {
    let (asked, painted) = one_pass(|ui| {
        ui.spinner();
    });
    assert_eq!(
        asked,
        Duration::ZERO,
        "control: the widget no longer asks for a frame on the frame it is \
         drawn, so this test's premise is gone",
    );
    let widget_stroke = painted
        .iter()
        .find(|w| **w == super::MARK_STROKE)
        .copied()
        .expect("control: the widget painted no arc of the mark's stroke");

    let (asked, painted) = one_pass(|ui| {
        mark(ui);
    });
    assert_eq!(
        asked,
        Duration::MAX,
        "the wait mark asked for a frame; a mark is drawn on the frames \
         other causes buy and never buys one",
    );
    assert!(
        painted.contains(&widget_stroke),
        "the mark painted no arc of the widget's stroke ({painted:?}); a \
         quiet mark that draws nothing is a blank, not a mark",
    );
}

/// **No source in this crate draws the widget.** Every one of its former sites
/// draws [`mark`], and the walk that proves the first half is shown to see the
/// second — a scan over a tree that had moved would otherwise pass over
/// nothing. Test files are skipped by name, as `ui_glyphs`' walk skips them:
/// the control above needs to spell the call.
#[test]
fn no_wait_in_this_crate_draws_the_widget_that_asks_for_a_frame_every_frame() {
    let needles = [
        concat!(".spin", "ner("),
        concat!("Spin", "ner::new("),
        concat!("Spin", "ner::default("),
        concat!("egui::Spin", "ner"),
        concat!("add(Spin", "ner"),
    ];
    let mut roots = vec![std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src")];
    let mut widget_sites = Vec::new();
    let mut mark_sites = Vec::new();
    let mut scanned = 0usize;
    let mut saw_status_bar = false;
    while let Some(dir) = roots.pop() {
        for entry in std::fs::read_dir(&dir).expect("source dir must be readable") {
            let path = entry.expect("dir entry").path();
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if path.is_dir() {
                roots.push(path);
            } else if name.ends_with(".rs") && !name.contains("test") {
                scanned += 1;
                saw_status_bar |= name == "ui_statusbar.rs";
                let src = std::fs::read_to_string(&path).expect("source must be readable");
                let hits: usize = needles.iter().map(|n| src.matches(n).count()).sum();
                if hits > 0 {
                    widget_sites.push((path.display().to_string(), hits));
                }
                let marks = src.matches("wait::mark(").count();
                if marks > 0 {
                    mark_sites.push((path.display().to_string(), marks));
                }
            }
        }
    }
    assert!(
        scanned > 40 && saw_status_bar,
        "the walk read {scanned} sources and {} the status bar — the walk is \
         broken, not the tree",
        if saw_status_bar { "saw" } else { "never saw" },
    );
    assert!(
        widget_sites.is_empty(),
        "egui's repaint-every-frame widget is drawn from {widget_sites:?}; a \
         wait draws `ui::wait::mark` and lets the arrival buy the frame",
    );
    let marks: usize = mark_sites.iter().map(|(_, n)| n).sum();
    assert!(
        marks >= 4,
        "only {marks} wait marks in the tree ({mark_sites:?}); the four sites \
         the widget was taken out of draw one each, so fewer means a wait \
         lost its mark rather than that the scan is clean",
    );
}
