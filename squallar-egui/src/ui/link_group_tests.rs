//! **Link groups**: the fan-outs that stop at a group's edge (W23), the time
//! half that no longer rides inside the layer guard (W24), the border that
//! paints the group (W25) and the sync section that names and changes it
//! (W26).
//!
//! Its own file rather than a wing of `input_harness/tests.rs`: the claims
//! here are about one model, and they read as one list.

use super::*;
use crate::input_harness::InputHarness;
use crate::pane::GroupId;
use crate::ui::PillKind;

/// Group B, spelled once.
fn b() -> GroupId {
    GroupId::from_index(1).expect("a layout can hold six groups")
}

/// A `Gui` with `count` panes, every one of them in group A, which is what a
/// fresh layout and every migrated config both are.
fn grid(count: usize) -> Gui {
    let mut gui = Gui::new();
    gui.set_pane_count_for_test(count);
    for idx in 0..count {
        assert_eq!(
            gui.pane(idx).expect("a fresh pane").group,
            Some(GroupId::FIRST),
            "precondition: every fresh pane starts in group A"
        );
    }
    gui
}

/// A two-pane harness with the layers panel out of the way.
fn pill_grid() -> InputHarness {
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    h.set_pane_count(2);
    h.close_layers();
    h
}

// ─── W23: the fan-out stops at the group's edge ─────────────────────────────

/// **Shared time reaches the active pane's group and nothing else.** Every
/// link flag stays on throughout, so the only thing that can narrow the target
/// set is the group — which is the whole claim.
#[test]
fn shared_time_stops_at_the_groups_edge() {
    let mut gui = grid(4);
    assert_eq!(
        gui.time_sync_targets(),
        vec![0, 1, 2, 3],
        "precondition: one group of four fans out over all four"
    );

    gui.pane_mut(2).expect("pane 2").group = Some(b());
    gui.pane_mut(3).expect("pane 3").group = Some(b());
    for idx in 0..4 {
        assert!(
            gui.pane(idx).expect("pane").time_link,
            "precondition: pane {idx}'s time link is still on, so the group \
             is the only thing that can exclude it"
        );
    }

    gui.set_active_pane_for_test(0);
    assert_eq!(gui.time_sync_targets(), vec![0, 1], "group A only");
    gui.set_active_pane_for_test(2);
    assert_eq!(gui.time_sync_targets(), vec![2, 3], "group B only");
}

/// **A pane in no group syncs with nobody, and nobody syncs with it** — the
/// state the three booleans could never express, because "off" was always
/// per-dimension.
#[test]
fn a_pane_in_no_group_is_alone_in_every_dimension() {
    let mut gui = grid(3);
    gui.pane_mut(1).expect("pane 1").group = None;

    gui.set_active_pane_for_test(0);
    assert_eq!(
        gui.time_sync_targets(),
        vec![0, 2],
        "the solo pane is not a target"
    );
    assert_eq!(gui.layer_sync_targets(0), vec![0, 2]);

    gui.set_active_pane_for_test(1);
    assert_eq!(
        gui.time_sync_targets(),
        vec![1],
        "and it drives nobody either"
    );
    assert_eq!(gui.layer_sync_targets(1), vec![1]);
    assert!(!gui.panes_layer_linked(0, 1) && !gui.panes_time_linked(0, 1));
    // But it still matches itself: the app-side dedup filters ask this of
    // their own pane's queued work, and a pane that could not match itself
    // would stop deduplicating its own renders on leaving every group.
    assert!(
        gui.panes_share_group(1, 1) && gui.panes_layer_linked(1, 1),
        "a pane must answer true about itself whether or not it is in a group"
    );
}

/// **Two groups load and converge independently.** Pane 0 drives group A onto
/// its site; group B keeps its own.
#[test]
fn a_site_converges_inside_one_group_and_not_the_other() {
    let mut gui = grid(4);
    gui.pane_mut(2).expect("pane 2").group = Some(b());
    gui.pane_mut(3).expect("pane 3").group = Some(b());
    for idx in 0..4 {
        gui.pane_mut(idx)
            .expect("pane")
            .set_site(format!("SITE{idx}"));
    }
    gui.set_active_pane_for_test(0);
    gui.propagate_pane_sync();

    assert_eq!(
        gui.pane(1).expect("pane 1").site(),
        "SITE0",
        "group A followed"
    );
    assert_eq!(
        gui.pane(2).expect("pane 2").site(),
        "SITE2",
        "group B must not have been dragged onto group A's site"
    );
    assert_eq!(gui.pane(3).expect("pane 3").site(), "SITE3");
}

/// **One group's active pane does not hold the other group's panes.** The
/// source scan used to break on the first moved pane in index order and then
/// hold everything from the active pane; with two groups that made group A's
/// active pane the authority over group B's viewport.
#[test]
fn each_group_resolves_its_own_viewport_source() {
    let mut gui = grid(4);
    gui.pane_mut(2).expect("pane 2").group = Some(b());
    gui.pane_mut(3).expect("pane 3").group = Some(b());
    gui.set_active_pane_for_test(0);

    for (idx, zoom) in [(0, 5.0), (1, 5.0), (2, 9.0), (3, 9.0)] {
        gui.pane_mut(idx)
            .expect("pane")
            .map_memory
            .set_zoom(zoom)
            .expect("in walkers' range");
    }
    let pre_zooms: Vec<f64> = (0..4)
        .map(|idx| gui.pane(idx).expect("pane").map_memory.zoom())
        .collect();
    let pre_positions: Vec<Option<walkers::Position>> = (0..4)
        .map(|idx| gui.pane(idx).expect("pane").map_memory.detached())
        .collect();

    // Pane 2 — group B, not the active pane — is the one that moved.
    gui.pane_mut(2)
        .expect("pane 2")
        .map_memory
        .set_zoom(11.0)
        .expect("in range");
    gui.sync_viewports(&pre_zooms, &pre_positions);

    assert_eq!(
        gui.pane(3).expect("pane 3").map_memory.zoom(),
        11.0,
        "group B's own move must reach group B"
    );
    assert_eq!(
        gui.pane(1).expect("pane 1").map_memory.zoom(),
        5.0,
        "and must not reach group A, which was never asked to move"
    );
    assert_eq!(
        gui.pane(0).expect("pane 0").map_memory.zoom(),
        5.0,
        "the active pane held its own group where it was"
    );
}

// ─── W24: the clock is not the layer link's to switch off ───────────────────

/// **Turning "Sync layers" off leaves time sync whole.** `viewing_live` and
/// the step size used to be written *inside* the layer guard, so a pane that
/// had opted out of layers silently stopped following the group's clock too —
/// while the layer toggle's own hover promised it kept only "site, product,
/// tilt and layers" to itself.
#[test]
fn unlinking_layers_leaves_the_clock_alone() {
    use crate::pane::TimeStep;

    let mut gui = grid(2);
    gui.set_active_pane_for_test(0);
    {
        let src = gui.pane_mut(0).expect("pane 0");
        src.viewing_live = false;
        src.time.step = TimeStep::from_secs(1800);
    }
    {
        let other = gui.pane_mut(1).expect("pane 1");
        other.viewing_live = true;
        other.time.step = TimeStep::from_secs(600);
    }

    // The active pane opts out of layers, and only of layers.
    gui.pane_mut(0).expect("pane 0").layer_link = false;
    assert!(
        gui.pane(0).expect("pane 0").time_link && gui.pane(1).expect("pane 1").time_link,
        "precondition: both time links are on"
    );

    gui.propagate_pane_sync();

    assert!(
        !gui.pane(1).expect("pane 1").viewing_live,
        "the clock must still fan out with the layer link off"
    );
    assert_eq!(
        gui.pane(1).expect("pane 1").time.step,
        TimeStep::from_secs(1800),
        "and so must the step size"
    );
}

/// The hover text stopped over-promising with it: the layers note now says
/// the clock is not its business, and names the toggle that is.
#[test]
fn the_layers_note_hands_the_clock_to_the_time_toggle() {
    let mut h = pill_grid();
    let (_, pill) = h.pill(0, PillKind::Link).expect("pane 0's Sync pill");
    h.mouse_click(pill.center());
    h.frame();
    let popover = h.pill_popover().expect("the popover opened");
    let row = popover
        .rows
        .iter()
        .find(|(label, _, _)| label == "Sync layers")
        .expect("the layers row")
        .1;
    h.mouse_move(row.center());
    h.warm_up();
    let painted = h.painted_text_strings().join(" ");
    assert!(
        painted.contains("Sync time"),
        "the layers hover must name the toggle the clock actually belongs to; \
         drew {painted:?}"
    );
}

// ─── W25: the border paints the group ───────────────────────────────────────

/// The group accent's band: the strip just inside a pane's top edge that
/// [`crate::ui::map::draw_pane_border`] paints its bar and tab into.
fn accent_band(pane: egui::Rect) -> egui::Rect {
    egui::Rect::from_min_max(pane.left_top(), egui::pos2(pane.right(), pane.top() + 22.0))
}

/// **Group membership is answerable from the border alone**, without opening
/// anything: the pane carries its group's accent, and the letter that names
/// it, inside its own border.
#[test]
fn the_border_carries_the_group_and_its_letter() {
    let mut h = pill_grid();
    h.gui_mut().pane_mut(1).expect("pane 1").group = Some(b());
    h.warm_up();

    let borders = h.pane_borders();
    assert_eq!(borders.len(), 2, "precondition: both panes drew a border");
    assert_eq!(borders[0].2.group, Some(GroupId::FIRST));
    assert_eq!(borders[1].2.group, Some(b()));

    let panes = h.pane_rects();
    for (idx, group) in [(0, GroupId::FIRST), (1, b())] {
        let fills = h.painted_fills_within(accent_band(panes[idx]), 2.0);
        assert!(
            fills.contains(&group.accent()),
            "pane {idx} painted no group-{} accent in its border band: {fills:?}",
            group.letter(),
        );
        assert!(
            h.painted_text_strings_in(accent_band(panes[idx]))
                .iter()
                .any(|t| t == &group.letter().to_string()),
            "pane {idx} painted the accent but never named the group, so a \
             reader who cannot tell the two hues apart learns nothing"
        );
    }
    assert_ne!(
        GroupId::FIRST.accent(),
        b().accent(),
        "and the two groups' accents must differ, or the border says one group"
    );
}

/// **A partial member reads differently from a full one**, and by a channel a
/// theme cannot collapse: the accent bar breaks into dashes. A full member's
/// bar is one rect; a partial member's is several.
#[test]
fn a_partial_member_breaks_its_accent_into_dashes() {
    let mut h = pill_grid();
    h.warm_up();
    let panes = h.pane_rects();

    let accent_rects = |h: &InputHarness, pane: egui::Rect| -> usize {
        let band = accent_band(pane);
        h.painted_rects()
            .iter()
            .zip(h.painted_fills())
            .filter(|(r, fill)| {
                band.expand(2.0).contains_rect(**r)
                    && **fill == GroupId::FIRST.accent()
                    && r.height() < 6.0
            })
            .count()
    };

    let whole = accent_rects(&h, panes[1]);
    assert_eq!(
        whole, 1,
        "precondition: a full member's accent bar is one unbroken run"
    );
    assert!(
        !h.gui_mut().pane(1).expect("pane 1").partial_member(),
        "precondition: pane 1 is a full member"
    );

    h.gui_mut().pane_mut(1).expect("pane 1").time_link = false;
    h.warm_up();
    assert!(
        h.gui_mut().pane(1).expect("pane 1").partial_member(),
        "opting out of one dimension while staying in the group is partial \
         membership"
    );
    let borders = h.pane_borders();
    assert!(
        borders[1].2.partial && !borders[0].2.partial,
        "the border must be told which pane is the partial member"
    );
    assert!(
        accent_rects(&h, h.pane_rects()[1]) > whole,
        "a partial member's bar must be broken, not merely recoloured — a \
         hue is exactly what a theme, a projector or a colour-blind reader \
         can collapse"
    );
}

/// **The border says the same thing in both OS themes.** There is no in-app
/// theme override, so both are reachable and neither is opt-in; the accent is
/// drawn over its own dark backing rather than in a theme colour, so the
/// pixels must be identical either way.
#[test]
fn the_group_accent_is_the_same_in_both_os_themes() {
    // Only the accent's own colours, so a neighbour's theme-tinted chrome
    // drifting into the band is not read as this border changing. A border
    // that DID take a theme colour still fails: its rects would stop matching
    // any accent, the list would empty on one arm, and the precondition below
    // is what makes an empty list a failure rather than a pass.
    let accents: Vec<egui::Color32> = GroupId::all().map(GroupId::accent).collect();
    let read = |dark: bool| {
        let mut h = pill_grid();
        h.set_os_theme(dark);
        h.gui_mut().pane_mut(1).expect("pane 1").group = Some(b());
        h.gui_mut().pane_mut(1).expect("pane 1").layer_link = false;
        h.warm_up();
        assert_eq!(
            h.ctx().global_style().visuals.dark_mode,
            dark,
            "precondition: the context really took the theme it was handed",
        );
        let panes = h.pane_rects();
        let marks: Vec<Vec<(egui::Rect, egui::Color32)>> = panes
            .iter()
            .map(|pane| {
                let band = accent_band(*pane).expand(2.0);
                h.painted_rects()
                    .iter()
                    .zip(h.painted_fills())
                    .filter(|(r, fill)| band.contains_rect(**r) && accents.contains(fill))
                    .map(|(r, fill)| (*r, *fill))
                    .collect()
            })
            .collect();
        let letters: Vec<Vec<String>> = panes
            .iter()
            .map(|pane| h.painted_text_strings_in(accent_band(*pane)))
            .collect();
        (marks, letters)
    };

    let (dark_marks, dark_letters) = read(true);
    let (light_marks, light_letters) = read(false);
    assert!(
        dark_marks.iter().all(|pane| !pane.is_empty()),
        "precondition: every pane painted an accent on the dark arm, or the \
         equality below is between two empty lists"
    );
    assert_eq!(
        dark_marks, light_marks,
        "the group accent moved or changed colour with the OS theme — the \
         border must read the same in both, because neither is opt-in"
    );
    assert!(
        dark_letters
            .iter()
            .all(|pane| pane.contains(&"A".to_owned()) || pane.contains(&"B".to_owned())),
        "precondition: the letters were painted at all"
    );
    assert_eq!(
        dark_letters, light_letters,
        "and so must the letters that name the groups"
    );
}

// ─── W26: the sync section shows and changes the group ──────────────────────

/// **The pill names the group on its own face.** It used to collapse three
/// booleans into one bit and keep the detail in a hover, which a touch device
/// never shows.
#[test]
fn the_pill_names_the_group_without_a_hover() {
    let mut h = pill_grid();
    let (label, _) = h.pill(0, PillKind::Link).expect("pane 0's Sync pill");
    assert_eq!(label, "Sync A");

    h.gui_mut().pane_mut(0).expect("pane 0").group = Some(b());
    h.warm_up();
    let (label, _) = h.pill(0, PillKind::Link).expect("still drawn");
    assert_eq!(label, "Sync B", "the pill follows the pane into its group");

    h.gui_mut().pane_mut(0).expect("pane 0").group = None;
    h.warm_up();
    let (label, _) = h.pill(0, PillKind::Link).expect("still drawn");
    assert_eq!(
        label, "\u{2297} Solo",
        "a pane in no group must say so, not read as a linked pane"
    );
}

/// **Looking at a pane's links does not make it the active pane.** Every other
/// pill activates on click; this one did too, so merely opening pane 2's sync
/// popover moved the source of every fan-out in the app onto pane 2.
#[test]
fn opening_the_sync_popover_does_not_activate_the_pane() {
    let mut h = pill_grid();
    assert_eq!(
        h.active_pane_index(),
        0,
        "precondition: pane 0 is the active one"
    );

    let (_, pill) = h.pill(1, PillKind::Link).expect("pane 1's Sync pill");
    h.mouse_click(pill.center());
    h.frame();
    assert!(h.pill_popover().is_some(), "the popover must have opened");
    assert_eq!(
        h.active_pane_index(),
        0,
        "inspecting pane 1's links is not selecting pane 1"
    );

    // The control that *is* a selection still is one, and says so on its face.
    let popover = h.pill_popover().expect("still up");
    let relink = popover
        .rows
        .iter()
        .find(|(label, _, _)| label == "Re-link this group here")
        .expect("the re-link row")
        .1;
    h.mouse_click(relink.center());
    h.warm_up();
    assert_eq!(
        h.active_pane_index(),
        1,
        "re-link names this pane the group's reference — that one is a choice"
    );
}

/// **The group row moves a pane between groups, and the popover stays up.**
/// The sync popover is the one that deliberately survives clicks inside it,
/// because setting a pane's links is several gestures in a row.
#[test]
fn the_group_row_moves_the_pane_and_keeps_the_popover_up() {
    let mut h = pill_grid();
    let (_, pill) = h.pill(0, PillKind::Link).expect("pane 0's Sync pill");
    h.mouse_click(pill.center());
    h.frame();
    let popover = h.pill_popover().expect("the popover opened");
    let group_row = popover
        .rows
        .iter()
        .find(|(label, _, _)| label == "Group")
        .expect("the group row is drawn")
        .1;

    // The row is a run of letters; B is the second button on it.
    let letters = h.painted_text_strings_in(group_row);
    assert!(
        letters.iter().any(|t| t == "B"),
        "the group row must offer a group to move to; drew {letters:?}"
    );
    let b_button = h
        .text_rect_in(group_row, "B")
        .expect("the B button's own rect");
    h.mouse_click(b_button.center());
    h.frame();

    assert_eq!(
        h.gui_mut().pane(0).expect("pane 0").group,
        Some(b()),
        "clicking a group letter must move the pane into it"
    );
    assert!(
        h.pill_popover().is_some(),
        "and the popover must stay up, like every other row in this section"
    );
    assert!(
        !h.gui_mut().panes_share_group(0, 1),
        "the two panes are in different groups now"
    );
}

/// **Both routes draw the same section.** `parity_walk` pins that every row is
/// *reachable* at each width, one route per width — it does not compare the
/// two routes' contents, so this is the equality it does not assert.
#[test]
fn the_pill_popover_and_the_pane_properties_draw_the_same_sync_rows() {
    let mut h = pill_grid();
    h.gui_mut().pane_mut(0).expect("pane 0").group = Some(b());
    h.gui_mut().pane_mut(0).expect("pane 0").layer_link = false;
    h.warm_up();

    h.open_pane_props();
    let inspector: Vec<(String, bool)> = h
        .inspector()
        .sync_rows
        .iter()
        .map(|(label, _, on)| (label.clone(), *on))
        .collect();
    h.close_inspector();
    h.close_layers();
    h.warm_up();

    let (_, pill) = h.pill(0, PillKind::Link).expect("pane 0's Sync pill");
    h.mouse_click(pill.center());
    h.frame();
    let popover: Vec<(String, bool)> = h
        .pill_popover()
        .expect("the popover opened")
        .rows
        .iter()
        .map(|(label, _, on)| (label.clone(), *on))
        .collect();

    assert!(
        !inspector.is_empty(),
        "precondition: the inspector drew the section at all"
    );
    assert_eq!(
        inspector, popover,
        "the two routes into one body drew different rows — the section is \
         one inventory, or it is two"
    );
    assert!(
        inspector.iter().any(|(label, on)| label == "Group" && *on),
        "and the group is one of the rows both of them draw: {inspector:?}"
    );
}

/// **The bulk unlink the section never had**, and its inverse, both stop at
/// the group's edge.
#[test]
fn relink_and_unlink_reach_the_group_and_stop_there() {
    let mut h = InputHarness::with_screen(egui::vec2(1400.0, 900.0));
    h.set_pane_count(4);
    h.close_layers();
    {
        let gui = h.gui_mut();
        for idx in 2..4 {
            gui.pane_mut(idx).expect("pane").group = Some(b());
            gui.pane_mut(idx).expect("pane").time_link = false;
        }
        for idx in 0..2 {
            gui.pane_mut(idx).expect("pane").time_link = false;
        }
    }
    h.warm_up();

    let click_row = |h: &mut InputHarness, pane: usize, label: &str| {
        let (_, pill) = h.pill(pane, PillKind::Link).expect("a Sync pill");
        h.mouse_click(pill.center());
        h.frame();
        let row = h
            .pill_popover()
            .expect("the popover opened")
            .rows
            .iter()
            .find(|(drawn, _, _)| drawn == label)
            .unwrap_or_else(|| panic!("no {label:?} row"))
            .1;
        h.mouse_click(row.center());
        h.warm_up();
        h.key_press(egui::Key::Escape);
        h.warm_up();
    };

    click_row(&mut h, 0, "Re-link this group here");
    {
        let gui = h.gui_mut();
        assert!(
            gui.pane(0).expect("pane 0").time_link && gui.pane(1).expect("pane 1").time_link,
            "re-link must reach every pane in its own group"
        );
        assert!(
            !gui.pane(2).expect("pane 2").time_link && !gui.pane(3).expect("pane 3").time_link,
            "and must not reach the other group — which is the difference \
             between a group model and three booleans"
        );
    }

    click_row(&mut h, 0, "Unlink this group");
    {
        let gui = h.gui_mut();
        for idx in 0..2 {
            let pane = gui.pane(idx).expect("pane");
            assert!(
                !pane.viewport_link && !pane.layer_link && !pane.time_link,
                "pane {idx}: the bulk unlink must take all three dimensions"
            );
            assert_eq!(
                pane.group,
                Some(GroupId::FIRST),
                "pane {idx}: unlinking is not leaving — the group is still \
                 the group, it just syncs nothing"
            );
        }
    }
}
