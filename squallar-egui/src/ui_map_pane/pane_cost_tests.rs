//! **The pane's own cost line, both arms.**
//!
//! The user's ruling is that per-pane cost is an overlay inside the frame, and
//! the two things it has to do pull against each other: a pane over the room it
//! has must say so and name what is holding the pool down, and a pane
//! comfortably under must show the same quantity and *not* nag about it. A test
//! that only drove the second arm would pass over a readout that never says
//! anything; one that only drove the first would pass over a readout that
//! always does.
//!
//! The composition is pure — [`super::pane_cost_line`] and
//! [`super::pane_cost_binder_line`] are functions of a `PaneCost` — so both
//! arms are driven here directly, and one harness case at the end shows the
//! line really reaches the glass rather than only the formatter.

use super::{PaneCost, pane_cost_binder_line, pane_cost_line};
use crate::shell_api::{BudgetReadout, Charge, PaneBudget, PoolReadout};
use squallar_device_profile::admit::Pool;
use squallar_device_profile::scene::PoolBinder;

/// A pane charged `cost` of a pool that has `allowed` of room for it.
fn cost(pool: Pool, cost_bytes: u64, allowed: u64, binder: PoolBinder) -> PaneCost {
    PaneCost {
        charge: Charge {
            pool,
            cost_bytes,
            allowed_bytes: Some(allowed),
        },
        binder,
        recovering: false,
        requested_percent: Some(50),
    }
}

const MB: u64 = 1_000_000;

/// **Comfortably under: the figure, and nothing else.**
///
/// The arithmetic: 412 MB charged against 2.7 GB of room, so `over()` is false
/// and the second line is absent. The figure is still there — a pane that is
/// fine is not a pane with nothing to say about what it costs.
#[test]
fn a_pane_under_its_room_shows_the_figure_and_does_not_nag() {
    let under = cost(Pool::Gpu, 412 * MB, 2_700 * MB, PoolBinder::UserPercent);

    assert_eq!(
        pane_cost_line(under),
        "GPU memory: 412 MB of 2.7 GB",
        "the quantity and its denominator are the whole of a healthy pane's line",
    );
    assert_eq!(
        pane_cost_binder_line(under),
        None,
        "a pane with room to spare must not be told whose setting it is under",
    );
}

/// **Over, under the user's own share: the figure, the binder, and the control
/// that moves it.**
///
/// 2.9 GB charged against 2.7 GB of room. The share is the one term of the
/// three that has a control, so it is the one arm that names where the control
/// is and what it is set to now.
#[test]
fn a_pane_over_its_room_names_the_binder_and_the_control_that_moves_it() {
    let over = cost(Pool::Gpu, 2_900 * MB, 2_700 * MB, PoolBinder::UserPercent);

    assert_eq!(
        pane_cost_line(over),
        "GPU memory: 2.9 GB of 2.7 GB",
        "the figure is the same shape over as under - only the second line differs",
    );
    assert_eq!(
        pane_cost_binder_line(over).as_deref(),
        Some("over - raise it in Settings > Memory (now 50 %)"),
        "a refusal the reader can act on has to say where the control is",
    );
}

/// **Over, on a machine that is simply small.** There is no share to raise, so
/// nothing is offered that would not work — but the cause is still named, and
/// with it the one thing the reader *can* do. A line that stopped at "not
/// enough memory" would be the apology this workspace forbids.
#[test]
fn a_hardware_bound_pane_names_the_machine_and_an_act_the_reader_still_has() {
    let over = cost(
        Pool::Host,
        6 * 1_000 * MB,
        4 * 1_000 * MB,
        PoolBinder::Hardware,
    );

    assert_eq!(pane_cost_line(over), "system memory: 6.0 GB of 4.0 GB");
    let line = pane_cost_binder_line(over).expect("an over pane names its binder");
    assert!(
        !line.contains("Settings"),
        "a control that cannot lift this pool must not be offered: {line}",
    );
    assert!(
        line.contains("close a pane"),
        "the reader is left with something to do, not an apology: {line}",
    );
}

/// **Over, under the governor**, which is the one binder that can lift on its
/// own — so whether it is climbing back is the news, and the line says it.
#[test]
fn a_governed_pane_says_whether_the_ceiling_is_coming_back_up() {
    let stuck = cost(Pool::Gpu, 3_000 * MB, 2_000 * MB, PoolBinder::Governor);
    let lifting = PaneCost {
        recovering: true,
        ..stuck
    };

    assert_eq!(
        pane_cost_binder_line(stuck).as_deref(),
        Some("over - memory pressure"),
    );
    assert_eq!(
        pane_cost_binder_line(lifting).as_deref(),
        Some("over - memory pressure, recovering"),
        "the recoverable case is the one worth distinguishing, and it is",
    );
}

/// **A unified adapter has one memory, and the line calls it that.** Naming a
/// GPU share on a machine where the two shares are one pool would be a
/// denominator the reader does not have.
#[test]
fn a_unified_pool_is_named_memory_and_not_one_of_its_two_shares() {
    let joint = cost(Pool::Joint, 700 * MB, 8_000 * MB, PoolBinder::Hardware);
    assert_eq!(pane_cost_line(joint), "memory: 700 MB of 8.0 GB");
}

/// **A pool the session has no figure for states the absence.** No
/// denominator is printed, and nothing is ever over — the same answer the
/// admission doors give an unknown pool.
#[test]
fn an_unknown_pool_prints_no_denominator_and_is_never_over() {
    let unknown = PaneCost {
        charge: Charge {
            pool: Pool::Host,
            cost_bytes: 268 * MB,
            allowed_bytes: None,
        },
        binder: PoolBinder::Hardware,
        recovering: false,
        requested_percent: None,
    };

    assert_eq!(pane_cost_line(unknown), "system memory: 268 MB");
    assert_eq!(pane_cost_binder_line(unknown), None);
}

/// A readout of one pane charged `cost_bytes` with `allowed` of room.
///
/// `generation` is the caller's because the `Gui` takes a copy only when it
/// moves — a second fixture at the same generation is silently the first one,
/// which is exactly the contract `shell_api::BudgetReadout::generation` states
/// and exactly how this test first failed.
fn readout(generation: u64, cost_bytes: u64, allowed: u64) -> BudgetReadout {
    BudgetReadout {
        generation,
        panes: vec![PaneBudget {
            charge: Charge {
                pool: Pool::Gpu,
                cost_bytes,
                allowed_bytes: Some(allowed),
            },
            ..PaneBudget::default()
        }],
        gpu: PoolReadout {
            binder: PoolBinder::UserPercent,
            requested_percent: Some(50),
            ..PoolReadout::default()
        },
        ..BudgetReadout::default()
    }
}

/// **The line reaches the glass**, not only the formatter.
///
/// Both arms again, through the real pane pass: the figure is painted either
/// way and the binder line only when the pane is over. Without this the four
/// tests above would be a pure-function suite over a function nothing calls.
#[test]
fn the_cost_line_is_painted_on_the_pane_and_the_nag_only_when_it_is_over() {
    let mut h = crate::input_harness::InputHarness::with_screen(egui::vec2(1400.0, 900.0));

    h.set_budget_readout(readout(1, 412 * MB, 2_700 * MB));
    assert!(
        h.text_painted_in(h.screen_rect(), "GPU memory: 412 MB of 2.7 GB"),
        "a healthy pane still shows what it costs",
    );
    assert!(
        !h.text_painted_in(h.screen_rect(), "Settings > Memory"),
        "and is not told to go and change a setting",
    );

    h.set_budget_readout(readout(2, 2_900 * MB, 2_700 * MB));
    assert!(
        h.text_painted_in(h.screen_rect(), "GPU memory: 2.9 GB of 2.7 GB"),
        "the figure is on the glass in the over arm too",
    );
    assert!(
        h.text_painted_in(
            h.screen_rect(),
            "over - raise it in Settings > Memory (now 50 %)"
        ),
        "and the binder line with the control on it",
    );
}

/// **A session that has priced no scene draws no line at all.** The harness
/// carries no readout until a fixture publishes one, which is every other test
/// in this crate — so this is also what keeps the suite's own panes clean.
#[test]
fn a_pane_with_no_readout_paints_no_cost_line() {
    let mut h = crate::input_harness::InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    h.warm_up();
    assert!(
        !h.text_painted_in(h.screen_rect(), "GPU memory:"),
        "an application that has priced nothing must invent no figure",
    );
}
