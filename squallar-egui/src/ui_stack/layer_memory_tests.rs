//! **The layers menu's per-layer memory line.**
//!
//! The user's ruling puts per-layer budgets here, in the words `shared N +
//! own M`: what the application pays once however many panes show the layer,
//! and what this pane pays on top. The allowance beside them is the room the
//! rest of the scene leaves the layer in the pool that binds its pane.
//!
//! The panel is 240 pt wide and already carries a name, an eye, a grip, a can
//! and the handler's own status line, so **the second half of this suite is
//! about the layout**: the line has to fit inside the row at every width class,
//! and a row that has one has to grow by exactly the line it gained.

use super::layer_memory_line;
use crate::input_harness::InputHarness;
use crate::shell_api::{BudgetReadout, Charge, LayerBudget, PaneBudget};
use squallar_device_profile::admit::Pool;
use squallar_source::id::known;

const MB: u64 = 1_000_000;

/// One priced layer row.
fn budget(layer: squallar_source::id::LayerId, shared: u64, own: u64, allowed: u64) -> LayerBudget {
    LayerBudget {
        layer,
        shared_bytes: shared,
        own_bytes: own,
        charge: Charge {
            pool: Pool::Gpu,
            cost_bytes: shared + own,
            allowed_bytes: Some(allowed),
        },
    }
}

/// A readout whose one pane carries `rows`.
fn readout(generation: u64, rows: Vec<LayerBudget>) -> BudgetReadout {
    BudgetReadout {
        generation,
        panes: vec![PaneBudget::default()],
        pane_layers: vec![rows],
        ..BudgetReadout::default()
    }
}

/// **A harness with the memory figures switched on**, which is what every
/// test below that expects a line on the glass needs: the switch is off in a
/// fresh `Gui` and off for every install that never touched it, so the
/// unadorned harness is the *hidden* arm. See
/// `the_rows_are_bare_until_the_switch_is_on` for that arm driven directly.
fn showing_figures(size: egui::Vec2) -> InputHarness {
    let mut h = InputHarness::with_screen(size);
    h.gui_mut().memory_figures = true;
    h
}

/// **The line the ruling asks for, verbatim**, with the layer's own allowance
/// beside it.
#[test]
fn a_priced_layer_reads_shared_plus_own_against_its_own_room() {
    assert_eq!(
        layer_memory_line(&budget(known::MRMS, 49 * MB, 18 * MB, 3_100 * MB)),
        "GPU: shared 49 MB + own 18 MB of 3.1 GB",
    );
}

/// **A pool the session has no figure for states the absence**: the pair is
/// still true and is still shown, and no denominator is invented for it.
#[test]
fn a_layer_in_an_unknown_pool_shows_the_pair_and_no_allowance() {
    let mut row = budget(known::MRMS, 49 * MB, 18 * MB, 0);
    row.charge.allowed_bytes = None;
    assert_eq!(layer_memory_line(&row), "GPU: shared 49 MB + own 18 MB");
}

/// **`shared` is displayed, never attributed** — a layer whose whole cost is
/// the application's still says so, rather than folding it into this pane's
/// own figure or dropping it. And the pair is never added: the line carries
/// two numbers because they answer two different questions.
#[test]
fn a_wholly_shared_layer_still_names_the_share() {
    let mut row = budget(known::BASEMAP_TILES, 96 * MB, 0, 3_100 * MB);
    row.charge.pool = Pool::Host;
    let line = layer_memory_line(&row);
    assert_eq!(line, "system: shared 96 MB + own 0.0 MB of 3.1 GB");
    assert!(
        line.contains("shared 96 MB"),
        "the shared half is the half a reader would otherwise go looking for",
    );
}

/// **The row draws it**, and a layer the App priced nothing for draws nothing.
///
/// The second half is what stops the first from being satisfied by a row of
/// zeroes on every layer in the stack: absence is absence.
#[test]
fn the_priced_row_carries_the_line_and_an_unpriced_row_carries_none() {
    let mut h = showing_figures(egui::vec2(1400.0, 900.0));
    h.set_budget_readout(readout(
        1,
        vec![budget(known::RADAR, 49 * MB, 18 * MB, 3_100 * MB)],
    ));
    h.open_layers();

    let radar = h.stack_row(&known::RADAR).expect("the Radar row is drawn");
    assert_eq!(
        radar.memory_line.as_deref(),
        Some("GPU: shared 49 MB + own 18 MB of 3.1 GB"),
        "the priced layer's row must carry its figure",
    );
    assert!(
        h.text_painted_in(h.screen_rect(), "GPU: shared 49 MB + own 18 MB of 3.1 GB"),
        "and the figure must reach the glass, not only the probe",
    );

    let unpriced = h
        .stack()
        .rows
        .into_iter()
        .find(|row| row.kind != known::RADAR)
        .expect("the stack draws more than the one row");
    assert_eq!(
        unpriced.memory_line, None,
        "{:?} was priced nothing and must show no figure",
        unpriced.kind,
    );
}

/// **The panel is tight, and the line has to live inside it** — at every width
/// class, because the stack is the desktop sidebar, the drawer and the phone
/// sheet's body and the three are the same rows in three hosts.
///
/// Two properties, and they are different failures. A row whose *text block*
/// leaves the row is a line drawn over its neighbour; a row that is not tall
/// enough for the lines it was given is a line drawn over the name above it.
/// The row still has to clear the M8 hit-target floor with both lines on it.
///
/// **Driven at the widest line the format can produce**, not at a typical one:
/// the longest pool tag, three-digit megabytes on both halves and a three-digit
/// gigabyte allowance. A test at `49 MB` would pass on a line 40 pt narrower
/// than anything a real machine can print.
#[test]
fn the_memory_line_fits_inside_the_row_at_every_width() {
    for (size, class) in [
        (
            egui::vec2(420.0, 1400.0),
            crate::ui_layout::WidthClass::Compact,
        ),
        (
            egui::vec2(800.0, 1200.0),
            crate::ui_layout::WidthClass::Medium,
        ),
        (
            egui::vec2(1400.0, 900.0),
            crate::ui_layout::WidthClass::Expanded,
        ),
    ] {
        let mut h = showing_figures(size);
        assert_eq!(
            h.width_class(),
            class,
            "precondition: a {size:?} screen must land in {class:?}",
        );
        let mut widest = budget(known::RADAR, 999 * MB, 999 * MB, 999_000 * MB);
        widest.charge.pool = Pool::Host;
        assert_eq!(
            layer_memory_line(&widest),
            "system: shared 999 MB + own 999 MB of 999 GB",
            "precondition: this is the widest line the format can print",
        );
        h.set_budget_readout(readout(1, vec![widest]));
        h.open_layers();

        let radar = h
            .stack_row(&known::RADAR)
            .unwrap_or_else(|| panic!("the Radar row is drawn on {class:?}"));
        assert!(
            radar.memory_line.is_some(),
            "precondition: the row under test carries a memory line on {class:?}",
        );
        // The text block is laid out with `truncate()`, so a line too long for
        // the panel is clipped rather than allowed to widen it. This is what
        // asserts that: the block never leaves the row it was allocated.
        assert!(
            radar.rect.contains_rect(radar.name.shrink(0.5)),
            "on {class:?} the row's text block {:?} left its row {:?}",
            radar.name,
            radar.rect,
        );
        assert!(
            radar.rect.height() >= 27.5,
            "on {class:?} a two-line row fell under the M8 hit-target floor at \
             {}pt",
            radar.rect.height(),
        );
        for row in &h.stack().rows {
            assert!(
                row.rect.width() <= h.stack().rect.width() + 0.5,
                "on {class:?} {:?}'s row is wider than the panel",
                row.kind,
            );
        }
    }
}

/// **A row with a memory line is taller than the same row without one, by one
/// small line and not by more.**
///
/// The row height is computed twice — once to allocate the click target and
/// once to centre the text in it — and the pair having drifted is exactly how
/// a status line comes to be drawn over the name above it. One test, both
/// spellings, because they can only be wrong together in a way that reads
/// right.
#[test]
fn a_priced_row_grows_by_exactly_the_line_it_gained() {
    let mut bare = showing_figures(egui::vec2(1400.0, 900.0));
    bare.open_layers();
    let without = bare
        .stack_row(&known::RADAR)
        .expect("the Radar row is drawn")
        .rect
        .height();

    let mut priced = showing_figures(egui::vec2(1400.0, 900.0));
    priced.set_budget_readout(readout(
        1,
        vec![budget(known::RADAR, 49 * MB, 18 * MB, 3_100 * MB)],
    ));
    priced.open_layers();
    let with = priced
        .stack_row(&known::RADAR)
        .expect("the Radar row is drawn")
        .rect
        .height();

    assert!(
        with > without,
        "the priced row did not grow at all: {with}pt against {without}pt - \
         the line is being drawn inside the height the row already had",
    );
    // A `Small` line is ~11 pt in the stock theme; anything past a comfortable
    // ceiling means the row grew by more than the one line it gained.
    assert!(
        with - without < 20.0,
        "the priced row grew by {}pt for one small line",
        with - without,
    );
}

/// **Both arms of the switch, on the surface the user pointed at.**
///
/// The same priced scene is in front of the same menu in both halves, so this
/// is not `a session that priced nothing shows nothing` wearing a new name:
/// the readout is published either way and the only difference is the switch.
///
/// The off arm asserts the probe **and** the glass. They can disagree — the
/// probe reports the line the row was given, the painter reports what was
/// drawn — and a figure that reached the painter without the probe would be
/// exactly as visible to the user and exactly as wrong.
#[test]
fn the_rows_are_bare_until_the_switch_is_on() {
    const LINE: &str = "GPU: shared 49 MB + own 18 MB of 3.1 GB";
    let priced = || readout(1, vec![budget(known::RADAR, 49 * MB, 18 * MB, 3_100 * MB)]);

    let mut off = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    assert!(
        !off.gui().memory_figures,
        "premise: a fresh session has the figures off, which is what every \
         install that never touched the switch is",
    );
    off.set_budget_readout(priced());
    off.open_layers();
    let row = off
        .stack_row(&known::RADAR)
        .expect("the Radar row is drawn whether or not it carries a figure");
    assert_eq!(
        row.memory_line, None,
        "the row was handed a figure the user never asked to see",
    );
    assert!(
        !off.text_painted_in(off.screen_rect(), LINE),
        "the figure was painted with the switch off",
    );

    let mut on = showing_figures(egui::vec2(1400.0, 900.0));
    on.set_budget_readout(priced());
    on.open_layers();
    let shown = on.stack_row(&known::RADAR).expect("the Radar row is drawn");
    assert_eq!(
        shown.memory_line.as_deref(),
        Some(LINE),
        "with the switch on the same scene must show the same figure it \
         always did - this is a visibility change, not a removal",
    );
    assert!(
        on.text_painted_in(on.screen_rect(), LINE),
        "and it must reach the glass, not only the probe",
    );
}
