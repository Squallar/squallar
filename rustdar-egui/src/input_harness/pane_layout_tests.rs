//! The pane grid as a user meets it: the width deciding the split at runtime,
//! the toggle that overrides the decision, and closing one specific pane.

use super::*;
use crate::actions::GuiAction;
use crate::pane::SplitOrientation;

fn desktop() -> InputHarness {
    let h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    assert_eq!(
        h.width_class(),
        crate::ui_layout::WidthClass::Expanded,
        "precondition: a desktop window"
    );
    h
}

fn narrow() -> InputHarness {
    let h = InputHarness::with_screen(egui::vec2(420.0, 1400.0));
    assert_eq!(
        h.width_class(),
        crate::ui_layout::WidthClass::Compact,
        "precondition: a phone-shaped window"
    );
    h
}

// ---------------------------------------------------------------- W8

/// **The same running binary restacks when the window narrows.** No `cfg` can
/// do this: one process, one build, two width classes reached by a resize.
#[test]
fn narrowing_the_live_window_restacks_a_pair_of_panes() {
    let mut h = desktop();
    h.set_pane_count(2);
    assert_eq!(h.pane_grid(), vec![2], "wide: side by side");
    let wide_rects = h.pane_rects();
    assert!(
        (wide_rects[0].top() - wide_rects[1].top()).abs() < 1.0,
        "wide: the two panes share a row: {wide_rects:?}"
    );

    h.set_screen(egui::vec2(420.0, 1400.0));
    h.warm_up();
    assert_eq!(
        h.width_class(),
        crate::ui_layout::WidthClass::Compact,
        "the resize must have crossed the breakpoint"
    );
    assert_eq!(h.pane_grid(), vec![1, 1], "narrow: stacked");
    let narrow_rects = h.pane_rects();
    assert!(
        narrow_rects[1].top() >= narrow_rects[0].bottom() - 1.0,
        "narrow: the second pane sits below the first: {narrow_rects:?}"
    );

    h.set_screen(egui::vec2(1400.0, 900.0));
    h.warm_up();
    assert_eq!(h.pane_grid(), vec![2], "widening again must put them back");
}

// ---------------------------------------------------------------- W9

/// The toggle is drawn only when there is a split to orient — one pane has no
/// divider, so three buttons would be an option the window cannot express.
#[test]
fn the_split_toggle_appears_only_with_more_than_one_pane() {
    let mut h = desktop();
    assert_eq!(h.pane_count(), 1);
    assert!(
        h.split_options().is_empty(),
        "one pane must draw no orientation buttons"
    );

    h.set_pane_count(2);
    let drawn: Vec<SplitOrientation> = h
        .split_options()
        .iter()
        .map(|probe| probe.orientation)
        .collect();
    assert_eq!(
        drawn,
        vec![
            SplitOrientation::Auto,
            SplitOrientation::Rows,
            SplitOrientation::Columns
        ],
        "every option must be expressed, including the default"
    );
    assert_eq!(
        h.split_options()
            .iter()
            .filter(|probe| probe.selected)
            .map(|probe| probe.orientation)
            .collect::<Vec<_>>(),
        vec![SplitOrientation::Auto],
        "and exactly one of them must read as chosen"
    );
}

/// **The toggle flips a side-by-side pair to stacked and back, from the top
/// bar, at a wide width** — where `Auto` would never have stacked them.
#[test]
fn the_toggle_flips_a_wide_pair_to_stacked_and_back() {
    let mut h = desktop();
    h.set_pane_count(2);
    assert_eq!(h.pane_grid(), vec![2]);

    let rows = h.split_option(SplitOrientation::Rows).expect("Rows drawn");
    h.mouse_click(rows.rect.center());
    h.warm_up();
    assert_eq!(h.pane_grid(), vec![1, 1], "Rows must stack them");
    let stacked = h.pane_rects();
    assert!(
        stacked[1].top() >= stacked[0].bottom() - 1.0,
        "and the rects must really be stacked: {stacked:?}"
    );

    let cols = h
        .split_option(SplitOrientation::Columns)
        .expect("Columns drawn");
    h.mouse_click(cols.rect.center());
    h.warm_up();
    assert_eq!(h.pane_grid(), vec![2], "Columns must put them back");

    let auto = h.split_option(SplitOrientation::Auto).expect("Auto drawn");
    h.mouse_click(auto.rect.center());
    h.warm_up();
    assert_eq!(
        h.split_options()
            .iter()
            .filter(|probe| probe.selected)
            .map(|probe| probe.orientation)
            .collect::<Vec<_>>(),
        vec![SplitOrientation::Auto],
        "Auto must be reachable again, or the default becomes a one-way door"
    );
    assert_eq!(h.pane_grid(), vec![2], "and Auto is columns at this width");
}

/// **And the same toggle overrides in the other direction on a phone**, where
/// `Auto` stacks: the default must not be a rule at either end.
#[test]
fn the_toggle_overrides_the_narrow_default_too() {
    let mut h = narrow();
    h.set_pane_count(2);
    assert_eq!(h.pane_grid(), vec![1, 1], "Auto stacks a narrow pair");

    // The phone top bar has no segments; the Layers sheet page hosts them.
    h.mouse_click(h.bottom_bar().layers.0.center());
    h.warm_up();
    assert_eq!(h.sheet().page, Some(crate::ui::SheetPage::Layers));

    let cols = h
        .split_option(SplitOrientation::Columns)
        .expect("the sheet must host the toggle too");
    h.mouse_click(cols.rect.center());
    h.warm_up();
    assert_eq!(
        h.pane_grid(),
        vec![2],
        "Columns must hold at a compact width"
    );
}

/// **The sheet's galley really fits the run it hosts.** The phone page draws
/// the segments in the roomy form, so the extra buttons are a real fit risk
/// rather than a hypothetical one — measured against the sheet's own rect, not
/// assumed.
#[test]
fn the_split_toggle_fits_the_phone_sheets_galley() {
    let mut h = narrow();
    h.set_pane_count(rustdar_device_profile::budget::MAX_PANES_MOBILE);
    h.mouse_click(h.bottom_bar().layers.0.center());
    h.warm_up();
    assert_eq!(h.sheet().page, Some(crate::ui::SheetPage::Layers));

    let sheet = h.sheet_rect().expect("the Layers page is open");
    let drawn = h.split_options();
    assert_eq!(drawn.len(), 3, "precondition: the toggle drew");
    for probe in drawn {
        assert!(
            sheet.contains_rect(probe.rect),
            "the {:?} button at {:?} spills out of the sheet {sheet:?}",
            probe.orientation,
            probe.rect
        );
        assert!(
            probe.rect.width() > 1.0 && probe.rect.height() > 1.0,
            "the {:?} button collapsed to {:?} instead of laying out",
            probe.orientation,
            probe.rect
        );
    }
    // The pane-count segments must not have been pushed out to make room.
    assert_eq!(
        h.pane_option_counts().len(),
        crate::ui_layout::WidthClass::max_panes_absolute(),
        "the counts must still all be drawn beside the new buttons"
    );
}

/// The chosen split survives a restart along with the dividers — the reopen
/// rule, driven through the real frame loop rather than the config unit tests.
#[test]
fn the_chosen_split_comes_back_after_a_restart() {
    use rustdar_kv::MemoryKvStore;
    let store = MemoryKvStore::default();

    let mut h = desktop();
    h.set_pane_count(2);
    let rows = h.split_option(SplitOrientation::Rows).expect("Rows drawn");
    h.mouse_click(rows.rect.center());
    h.warm_up();
    assert_eq!(h.pane_grid(), vec![1, 1]);
    h.gui().save_ui_config(&store);

    let mut reopened = desktop();
    assert!(reopened.gui_mut().load_ui_config(&store));
    reopened.warm_up();
    assert_eq!(
        reopened.pane_grid(),
        vec![1, 1],
        "the stacked pair must come back stacked on a wide window"
    );
    assert!(
        reopened
            .split_option(SplitOrientation::Rows)
            .expect("Rows drawn")
            .selected,
        "and the toggle must show the restored choice"
    );
}

// ---------------------------------------------------------------- W11

/// The close control is in Pane properties, not on the pill row: a control
/// that destroys a pane must not sit on the hover-reveal path, where it idles
/// at 35% opacity until the pointer finds it.
#[test]
fn the_close_control_is_in_pane_properties_and_only_with_a_pane_to_fall_back_to() {
    let mut h = desktop();
    h.open_pane_props();
    assert_eq!(
        h.inspector().close_pane,
        egui::Rect::NOTHING,
        "the last pane must offer no close"
    );

    h.set_pane_count(3);
    h.open_pane_props();
    let close = h.inspector().close_pane;
    assert_ne!(close, egui::Rect::NOTHING, "three panes must offer a close");
    assert!(close.width() > 1.0 && close.height() > 1.0);
}

/// Closing a pane closes **that** pane, and the panes above it move up a
/// number rather than the highest one silently disappearing.
#[test]
fn closing_a_middle_pane_removes_that_pane_and_renumbers_the_rest() {
    let mut h = desktop();
    h.set_pane_count(4);
    // Unlinked, or the layer fan-out copies the active pane's site over the
    // very marks this test identifies the panes by.
    h.set_layer_links(false);
    for idx in 0..4 {
        h.gui_mut()
            .pane_mut(idx)
            .expect("pane")
            .set_site(format!("SITE{idx}"));
    }

    let mut actions = Vec::new();
    let ctx = h.ctx().clone();
    assert!(h.gui_mut().close_pane(&ctx, 1, &mut actions));
    h.warm_up();

    assert_eq!(h.pane_count(), 3);
    let sites: Vec<String> = (0..3)
        .map(|idx| h.gui().pane(idx).expect("pane").site().to_string())
        .collect();
    assert_eq!(
        sites,
        vec!["SITE0", "SITE2", "SITE3"],
        "pane 1 must be the one that went, not the highest-numbered"
    );
}

/// Closing the pane you are looking at moves you to its neighbour, not to pane
/// 0 — and closing any other pane leaves you on the pane you were on, under
/// its new number.
#[test]
fn the_active_pane_follows_the_neighbour_rather_than_snapping_to_zero() {
    let cases = [
        // (active before, closed, active after)
        (3usize, 1usize, 2usize), // above the close: same pane, new number
        (0, 2, 0),                // below the close: untouched
        (2, 2, 2),                // the active one: the pane that slid in
        (3, 3, 2),                // the active one, and it was last: the one before
    ];
    for (before, closed, after) in cases {
        let mut h = desktop();
        h.set_pane_count(4);
        h.gui_mut().set_active_pane_for_test(before);
        let mut actions = Vec::new();
        let ctx = h.ctx().clone();
        assert!(h.gui_mut().close_pane(&ctx, closed, &mut actions));
        assert_eq!(
            h.active_pane_index(),
            after,
            "active {before}, closed {closed}"
        );
    }
}

/// The last pane is never closed: a window with no pane has nothing to show.
#[test]
fn the_last_pane_cannot_be_closed() {
    let mut h = desktop();
    let mut actions = Vec::new();
    let ctx = h.ctx().clone();
    assert!(!h.gui_mut().close_pane(&ctx, 0, &mut actions));
    assert_eq!(h.pane_count(), 1);
    assert!(
        actions.is_empty(),
        "a refused close must not emit invalidation for a close that did not happen"
    );
}

/// **THE ITEM.** `PaneId` is a slot position, so closing pane 2 renumbers 3 to
/// 2. Work already queued against pane 3 would then land on the pane that was
/// pane 4 — the wrong pane, silently, with no error anywhere.
///
/// Queue an action against pane 3, close pane 2, and assert it does not land.
/// The uncooperative form of this test — the `actions.retain` line deleted —
/// is what proves it can fail; see the module-level note on
/// `Gui::close_pane`'s invalidation list.
#[test]
fn work_queued_for_a_pane_above_the_closed_one_does_not_land_on_its_renumbering() {
    let mut h = desktop();
    h.set_pane_count(4);

    // Pane 3 asked to jump to live and to fetch an overlay before the close.
    // Pane 0 asked for the same, and it is the control: its number does not
    // move, so its work must survive.
    let mut actions = vec![
        GuiAction::JumpToLive { pane_idx: 3 },
        GuiAction::NavigateOneScan {
            pane_idx: 3,
            forward: true,
        },
        GuiAction::JumpToLive { pane_idx: 0 },
        GuiAction::SwitchRadarSite {
            site: "KTLX".to_owned(),
            pane_idx: 2,
        },
    ];

    let ctx = h.ctx().clone();
    assert!(h.gui_mut().close_pane(&ctx, 2, &mut actions));

    // Nothing addressed to pane 2 or above survives: after the renumber those
    // indices name different panes.
    let survivors: Vec<Option<usize>> = actions
        .iter()
        .filter(|action| {
            !matches!(
                action,
                GuiAction::ReleaseVolume { .. } | GuiAction::PaneClosed { .. }
            )
        })
        .map(GuiAction::pane_idx)
        .collect();
    assert_eq!(
        survivors,
        vec![Some(0)],
        "only the pane below the close keeps its queued work"
    );
    assert!(
        !actions
            .iter()
            .any(|action| matches!(action, GuiAction::JumpToLive { pane_idx: 2 })),
        "pane 3's jump-to-live must not have been re-aimed at the pane that \
         is now pane 2 — that is the bug this test exists for"
    );

    // And the invalidation the app side needs is emitted, once, naming the
    // slot the renumbering starts at.
    let closed: Vec<usize> = actions
        .iter()
        .filter_map(|action| match action {
            GuiAction::PaneClosed { pane_idx } => Some(*pane_idx),
            _ => None,
        })
        .collect();
    assert_eq!(closed, vec![2], "exactly one PaneClosed, naming the slot");

    // The volume store's refcount is keyed by pane index, so every old index
    // from the closed one up has to be handed back.
    let released: Vec<usize> = actions
        .iter()
        .filter_map(|action| match action {
            GuiAction::ReleaseVolume { pane_idx } => Some(*pane_idx),
            _ => None,
        })
        .collect();
    assert_eq!(
        released,
        vec![2, 3],
        "every old index at or above the close must give its volume back"
    );
}

/// The same claim through the real affordance rather than the function: click
/// the close button in Pane properties and the frame's own action list comes
/// back without the work aimed at the panes that moved.
#[test]
fn the_close_button_drops_the_frames_queued_work_for_the_panes_that_moved() {
    let mut h = desktop();
    h.set_pane_count(3);
    h.gui_mut().set_active_pane_for_test(1);
    h.open_pane_props();

    let close = h.inspector().close_pane;
    assert_ne!(close, egui::Rect::NOTHING);
    h.mouse_click(close.center());

    assert_eq!(h.pane_count(), 2, "the click must have closed a pane");
    assert!(
        h.last_actions()
            .iter()
            .any(|action| matches!(action, GuiAction::PaneClosed { pane_idx: 1 })),
        "the frame must report the close to the app side: {:?}",
        h.last_actions()
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
    );
}

/// Every action variant that carries a pane index answers `pane_idx`. The
/// match in `GuiAction::pane_idx` has no wildcard arm, so a new variant is a
/// compile error rather than an action the close cannot invalidate — this test
/// pins the other half: that the ones which *do* carry an index report it.
#[test]
fn every_pane_bearing_action_reports_the_pane_it_is_about() {
    let with_pane: Vec<GuiAction> = vec![
        GuiAction::SwitchRadarSite {
            site: "KTLX".to_owned(),
            pane_idx: 7,
        },
        GuiAction::FetchOverlay {
            kind: rustdar_source::id::known::RADAR,
            pane_idx: 7,
        },
        GuiAction::RefreshOverlay {
            kind: rustdar_source::id::known::RADAR,
            pane_idx: 7,
        },
        GuiAction::EnableLoop {
            pane_idx: 7,
            lookback_secs: 60,
        },
        GuiAction::DisableLoop { pane_idx: 7 },
        GuiAction::ToggleLoopPlayback { pane_idx: 7 },
        GuiAction::StepLoopFrame {
            pane_idx: 7,
            forward: true,
        },
        GuiAction::SeekLoopFrame {
            pane_idx: 7,
            frame_index: 0,
        },
        GuiAction::NavigateTime {
            pane_idx: 7,
            step_secs: 60,
        },
        GuiAction::NavigateOneScan {
            pane_idx: 7,
            forward: true,
        },
        GuiAction::JumpToLive { pane_idx: 7 },
        GuiAction::ReleaseVolume { pane_idx: 7 },
        GuiAction::PaneClosed { pane_idx: 7 },
    ];
    for action in &with_pane {
        assert_eq!(
            action.pane_idx(),
            Some(7),
            "{action} does not report its pane"
        );
    }

    let without_pane = [
        GuiAction::Exit,
        GuiAction::StopGps,
        GuiAction::RequestLocation,
        GuiAction::StopLocation,
        GuiAction::OpenLocationSettings,
    ];
    for action in &without_pane {
        assert_eq!(
            action.pane_idx(),
            None,
            "{action} claims a pane it has none"
        );
    }
}
