//! **Whose selection an automatic round refreshes**, with a split open.
//!
//! The poll gate emits one `FetchOverlay` per layer. What it may never do is
//! emit one per *layer* when two panes have asked that layer for two different
//! things — the pane whose selection is left out draws its last answer for the
//! life of the session while every phase, every clock and every status line
//! reads healthy.
//!
//! The other direction is the cost side and is tested here too: two panes
//! asking for the *same* thing must produce ONE round. An over-firing fan-out
//! spends the user's bandwidth and the layer's cache on a duplicate of an
//! answer it already has.

use super::*;
use squallar_source::controls::{ControlUpdate, ControlValue};
use std::collections::BTreeSet;

/// The mosaic layer: it auto-polls (120 s), and its fetch is shaped by a
/// per-pane selection — `MrmsHandler::create_fetch_tasks` reads
/// `self.view(pane).selected_product` and asks the bucket for that product
/// alone. Two panes on two products are two different asks.
const KIND: LayerId = known::MRMS;

/// The two products the layer registers, as they are spelled in a saved slot.
const COMPOSITE: &str = "mrms_reflectivity";
const PRECIP_RATE: &str = "mrms_preciprate";

/// A split with the mosaic on in both panes, and the second pane unlinked —
/// which is what a user does to compare two products side by side, and the
/// only state in which the two panes' selections can differ at all.
fn split_with_mosaic_in_both_panes() -> Gui {
    let mut gui = Gui::new();
    gui.set_pane_count_for_test(2);
    for idx in 0..2 {
        gui.pane_mut(idx)
            .expect("the layout was just given two panes")
            .set_overlay_enabled(KIND, true);
    }
    // Unlinked, so `propagate_layer_state` cannot converge the two panes back
    // onto one product behind the test's back — the scene under test is the
    // one where they genuinely differ.
    gui.pane_mut(1).expect("two panes").layer_link = false;
    gui
}

/// Put `product` in `idx`'s mosaic slot through the real control door, which
/// is what hydrates the slot and writes the choice back into the pane.
fn choose_product(gui: &mut Gui, idx: usize, product: &str) {
    gui.apply_control_on_pane_for_test(
        idx,
        &KIND,
        &ControlUpdate {
            id: "product",
            value: ControlValue::String(product.to_string()),
        },
    );
}

/// The product `idx` is showing, off its own saved slot.
fn product_of(gui: &Gui, idx: usize) -> String {
    gui.pane(idx)
        .expect("pane exists")
        .slot(&KIND)
        .expect("the mosaic is in this pane's stack")
        .config
        .get("product")
        .and_then(|v| v.as_str())
        .expect("the control edit wrote this pane's product back to its slot")
        .to_string()
}

/// One turn of the real auto-poll gate; the mosaic rounds it started, by the
/// pane each was attributed to.
fn mosaic_rounds(gui: &mut Gui) -> Vec<usize> {
    let mut actions = Vec::new();
    gui.check_auto_polls(&mut actions);
    actions
        .into_iter()
        .filter_map(|action| match action {
            GuiAction::FetchOverlay { kind, pane_idx } if kind == KIND => Some(pane_idx),
            _ => None,
        })
        .collect()
}

/// **The defect.** Two panes, two products, one round — and the round is
/// always the first pane's, so the second pane's mosaic is never refetched.
#[test]
fn every_pane_selection_on_screen_gets_a_round() {
    let mut gui = split_with_mosaic_in_both_panes();
    choose_product(&mut gui, 0, COMPOSITE);
    choose_product(&mut gui, 1, PRECIP_RATE);

    let on_screen: BTreeSet<String> = (0..2).map(|idx| product_of(&gui, idx)).collect();
    assert_eq!(
        on_screen,
        BTreeSet::from([COMPOSITE.to_string(), PRECIP_RATE.to_string()]),
        "premise: the split must really show two different mosaics"
    );

    let rounds = mosaic_rounds(&mut gui);
    let refreshed: BTreeSet<String> = rounds.iter().map(|&idx| product_of(&gui, idx)).collect();
    let started = rounds.len();

    assert_eq!(
        refreshed, on_screen,
        "the automatic poll started {started} round(s), for pane(s) {rounds:?}, \
         covering {refreshed:?} — every product in {on_screen:?} missing from that \
         set is drawn by a pane that will never be refetched again this session, \
         with the layer's clock, health and status line all reading fresh",
    );
}

/// **The cost side.** Two panes wanting the same thing is one ask, not two:
/// the fan-out may not turn a split into a second identical request.
///
/// Both panes are edited through the control door, so both carry a hydrated
/// state and a written-back config — the shape in which a dedup keyed on
/// anything but what the round actually asks for would split them.
#[test]
fn two_panes_on_the_same_product_produce_one_round() {
    let mut gui = split_with_mosaic_in_both_panes();
    choose_product(&mut gui, 0, PRECIP_RATE);
    choose_product(&mut gui, 1, PRECIP_RATE);

    assert_eq!(
        product_of(&gui, 0),
        product_of(&gui, 1),
        "premise: both panes must be showing the same mosaic"
    );

    let rounds = mosaic_rounds(&mut gui);
    let started = rounds.len();
    assert_eq!(
        started, 1,
        "two panes showing one product started {started} rounds ({rounds:?}) — the \
         same object fetched twice, decoded twice and cached twice for one picture",
    );
}

/// A pane that cannot draw the layer does not buy it a round either — the
/// fan-out walks the same predicate the poll gate and the data release do,
/// so a 3D pane with its floor hidden is not a second selection to refresh.
#[test]
fn a_pane_with_no_ground_does_not_earn_a_round() {
    let mut gui = split_with_mosaic_in_both_panes();
    choose_product(&mut gui, 0, COMPOSITE);
    choose_product(&mut gui, 1, PRECIP_RATE);
    assert_eq!(
        mosaic_rounds(&mut gui).len(),
        2,
        "premise: two drawable panes on two products are two rounds"
    );

    gui.pane_mut(1)
        .expect("two panes")
        .set_view(squallar_radar::types::RenderView::Volume);
    gui.pane_mut(1)
        .expect("two panes")
        .volume_mut()
        .expect("a 3D pane has volume state")
        .hide_floor = true;

    let rounds = mosaic_rounds(&mut gui);
    assert_eq!(
        rounds,
        vec![0],
        "a pane with no surface to draw the mosaic on was still bought a round"
    );
}

/// **A scrub is only part of the ask where the request carries it.**
///
/// METAR is `TimeAxis::Live`: `as_of_for_layer` leaves the fetch context on the
/// wall clock for it, and its `create_fetch_tasks` reads neither the pane nor
/// `as_of`. So two panes parked hours apart ask the national feed for the very
/// same bytes, and charging that difference a second download is the over-fire
/// this whole fan-out has to avoid.
#[test]
fn a_scrub_buys_no_round_on_a_layer_whose_request_ignores_the_clock() {
    let kind = known::METAR;
    let mut gui = Gui::new();
    gui.set_pane_count_for_test(2);
    for idx in 0..2 {
        gui.pane_mut(idx)
            .expect("two panes")
            .set_overlay_enabled(kind.clone(), true);
    }
    gui.pane_mut(1).expect("two panes").layer_link = false;
    gui.pane_mut(1).expect("two panes").time.mode = crate::pane::TimeMode::AsOf(
        chrono::NaiveDate::from_ymd_opt(2026, 6, 1)
            .and_then(|d| d.and_hms_opt(12, 0, 0))
            .expect("a real instant"),
    );

    let mut actions = Vec::new();
    gui.check_auto_polls(&mut actions);
    let rounds: Vec<usize> = actions
        .iter()
        .filter_map(|a| match a {
            GuiAction::FetchOverlay { kind: k, pane_idx } if *k == kind => Some(*pane_idx),
            _ => None,
        })
        .collect();
    assert_eq!(
        rounds,
        vec![0],
        "a scrubbed pane bought a second round of a feed whose request does not \
         carry the clock — two identical downloads for one answer"
    );
}

/// The other half of the same axis: where the request **does** carry the
/// depicted instant, two panes parked apart are two different asks.
///
/// NWS alerts are `TimeAxis::EventLifetime` and their fetch reads `as_of`, so a
/// pane scrubbed into the past is asking for a set of alerts the live pane's
/// round will never contain. Collapsing the two is the original defect on its
/// other axis.
#[test]
fn a_scrub_earns_a_round_where_the_request_carries_the_clock() {
    let kind = known::NWS_ALERTS;
    let mut gui = Gui::new();
    gui.set_pane_count_for_test(2);
    for idx in 0..2 {
        gui.pane_mut(idx)
            .expect("two panes")
            .set_overlay_enabled(kind.clone(), true);
    }
    gui.pane_mut(1).expect("two panes").layer_link = false;

    let mut before = Vec::new();
    gui.check_auto_polls(&mut before);
    let live_rounds = before
        .iter()
        .filter(|a| matches!(a, GuiAction::FetchOverlay { kind: k, .. } if *k == kind))
        .count();
    assert_eq!(
        live_rounds, 1,
        "premise: two live panes on this layer are one ask"
    );

    gui.pane_mut(1).expect("two panes").time.mode = crate::pane::TimeMode::AsOf(
        chrono::NaiveDate::from_ymd_opt(2026, 6, 1)
            .and_then(|d| d.and_hms_opt(12, 0, 0))
            .expect("a real instant"),
    );
    let mut after = Vec::new();
    gui.check_auto_polls(&mut after);
    let rounds: Vec<usize> = after
        .iter()
        .filter_map(|a| match a {
            GuiAction::FetchOverlay { kind: k, pane_idx } if *k == kind => Some(*pane_idx),
            _ => None,
        })
        .collect();
    assert_eq!(
        rounds,
        vec![0, 1],
        "the pane scrubbed into the past shares the live pane's round, so it \
         draws whichever alerts were in force now rather than then"
    );
}
